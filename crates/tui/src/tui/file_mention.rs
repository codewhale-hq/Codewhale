//! `@`-mention parsing, completion, and expansion for the composer.
//!
//! Two responsibilities live here:
//!
//! 1. **Tab-completion** at the cursor — `try_autocomplete_file_mention` is
//!    called by the composer's Tab handler. Walks the workspace, ranks
//!    candidates by prefix-then-substring match, and either splices the
//!    completion in directly (single match), extends to a shared prefix, or
//!    surfaces options in the status line.
//! 2. **Expansion before send** — when the user hits Enter on a message that
//!    contains `@<path>` references, `user_request_with_file_mentions`
//!    appends a "Local context from @mentions" block with the file contents
//!    (or directory listings, or media-attachment hints) so the model can see
//!    what the user pointed at. Capped per-message and per-file.
//!
//! The module is deliberately self-contained: nothing inside reaches into UI
//! widgets or rendering, so it stays unit-testable from `ui/tests.rs` and
//! from its own module-level tests.
//!
//! Pulled out of `ui.rs` to shrink the 5,500-line monolith and to give the
//! mention logic a single home that future maintainers can find without
//! grepping for `@` across half the codebase.

use std::fmt::Write;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::tui::app::{App, MentionCompletionCache};
use crate::tui::git_mention::{self, GitMentionCache, GitMentionKind};
use crate::tui::mention_completion::{MentionDiscoveryBehavior, MentionDiscoveryKey};
use codewhale_core::{
    ContextReference, ContextReferenceKind, ContextReferenceSource, MediaAttachmentReference,
    media_attachment_references,
};
use codewhale_runtime::working_set::Workspace;

/// Maximum number of `@`-mentions whose contents are inlined into one user
/// message. Beyond this we stop appending blocks but the raw `@token` text
/// remains in the message.
pub const MAX_FILE_MENTIONS_PER_MESSAGE: usize = 8;
/// Per-file byte ceiling when inlining mention contents.
pub const MAX_MENTION_FILE_BYTES: u64 = 128 * 1024;
/// Per-directory entry ceiling when inlining a directory listing.
pub const MAX_DIRECTORY_MENTION_ENTRIES: usize = 80;

/// Maximum file-mention completion candidates to consider per keypress. Caps
/// the cost of walking large workspaces; subsequent keystrokes narrow further.
const FILE_MENTION_COMPLETION_LIMIT: usize = 64;

/// Compact composer preview row for local context. `included=false` also
/// covers lexical `@` mentions whose exact inclusion is resolved on send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMentionPreview {
    pub kind: String,
    pub label: String,
    pub detail: Option<String>,
    pub included: bool,
    pub removable: bool,
}

// ---------------------------------------------------------------------------
//  Tab-completion
// ---------------------------------------------------------------------------

/// If the cursor sits inside a `@<partial>` token in the input, return the
/// byte offset where the `@` starts (so we can splice in a completion) and
/// the partial path the user has typed so far. The token stops at whitespace
/// or the end of input. Returns `None` when the cursor is outside any mention
/// or the token is empty (`@` with nothing after it).
pub fn partial_file_mention_at_cursor(input: &str, cursor_chars: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    if cursor_chars > chars.len() {
        return None;
    }
    // Walk left from the cursor until we find an `@` or a whitespace; if
    // whitespace comes first the cursor isn't inside a mention.
    let mut start_chars = cursor_chars;
    while start_chars > 0 {
        let prev = chars[start_chars - 1];
        if prev == '@' {
            start_chars -= 1;
            break;
        }
        if prev.is_whitespace() {
            return None;
        }
        start_chars -= 1;
    }
    if start_chars == cursor_chars || chars.get(start_chars) != Some(&'@') {
        return None;
    }
    // Confirm the `@` itself is at a valid mention boundary.
    if !is_file_mention_start(&chars, start_chars) {
        return None;
    }
    // Consume from the `@` to the next whitespace (the end of the token).
    let mut end_chars = start_chars + 1;
    while end_chars < chars.len() && !chars[end_chars].is_whitespace() {
        end_chars += 1;
    }
    let partial: String = chars[start_chars + 1..end_chars].iter().collect();
    let byte_start: usize = chars[..start_chars].iter().map(|c| c.len_utf8()).sum();
    Some((byte_start, partial))
}

/// The mention body for `path`: bare when [`extract_file_mentions`] reads
/// it back unchanged, otherwise wrapped in the quoted form it also parses
/// (`@"My Docs/notes.md"`). A bare path with a space would split into a
/// missing-file mention for its first word.
pub(crate) fn file_mention_body(path: &str) -> String {
    let bare_round_trips = !path.is_empty()
        && !path.chars().any(char::is_whitespace)
        && !path.starts_with(['"', '\''])
        && trim_unquoted_mention(path) == path;
    if bare_round_trips {
        path.to_string()
    } else if !path.contains('"') {
        format!("\"{path}\"")
    } else if !path.contains('\'') {
        format!("'{path}'")
    } else {
        path.to_string()
    }
}

/// Cwd-aware completion entry point. Shares its walker with the future
/// Ctrl+P fuzzy picker (#97); see [`Workspace::completions`] for the
/// ranking + display rules.
#[cfg(test)]
pub fn find_file_mention_completions(
    workspace: &Workspace,
    partial: &str,
    limit: usize,
) -> Vec<String> {
    let entries = workspace.completions(partial, limit);
    // #441: re-rank by frecency so files the user mentions a lot float up.
    // Never-mentioned candidates fall back to the workspace ranker's order.
    let entries = super::file_frecency::rerank_by_frecency(entries);
    tracing::debug!(
        target: "codewhale_tui::file_mention",
        partial = %partial,
        workspace = %workspace.root.display(),
        cwd = ?std::env::current_dir().ok(),
        match_count = entries.len(),
        "file mention completion walk",
    );
    entries
}

/// Resolve the `@`-mention completion popup contents for the current
/// composer state. Returns an empty `Vec` when:
///
/// - The popup is suppressed (`app.mention_menu_hidden`).
/// - The cursor is not inside an `@<partial>` token.
/// - The workspace walk produced no candidates.
///
/// Mirrors `visible_slash_menu_entries` so the composer widget can treat
/// both menus identically (one `Vec<String>` of entries, one selected index).
///
/// Once the composer widget is extended to render this as a popup, it will
/// pair with `apply_mention_menu_selection` for the Up/Down/Enter flow.
#[must_use]
pub fn visible_mention_menu_entries(app: &mut App, limit: usize) -> Vec<String> {
    if app.mention_menu_hidden {
        app.composer.mention_discovery.cancel();
        return Vec::new();
    }
    let Some((_byte_start, partial)) =
        partial_file_mention_at_cursor(&app.input, app.cursor_position)
    else {
        app.composer.mention_discovery.cancel();
        return Vec::new();
    };
    if limit == 0 {
        app.composer.mention_discovery.cancel();
        return Vec::new();
    }

    mention_menu_entries(app, &partial, limit).0
}

/// Drain a completed discovery result without waiting. The event loop calls
/// this once per tick so a finished scan repaints the popup even when the user
/// has stopped typing.
pub(crate) fn poll_background_mention_discovery(app: &mut App) -> bool {
    if app.composer.mention_discovery.poll() {
        app.composer.mention_completion_cache = None;
        return true;
    }
    false
}

/// Return `(entries, ready)`. `ready = false` means discovery is still in the
/// background; callers must not misreport that temporary empty result as "no
/// matches".
fn mention_menu_entries(app: &mut App, partial: &str, limit: usize) -> (Vec<String>, bool) {
    if poll_background_mention_discovery(app) {
        app.needs_redraw = true;
    }

    let workspace = app.workspace.clone();
    let cwd = app.composer.mention_cwd.clone();
    let walk_depth = app.mention_walk_depth;
    let behavior = app.mention_menu_behavior.clone();
    let follow_links = app.workspace_follow_symlinks;
    let discovery_key = if behavior == "browser" {
        MentionDiscoveryKey::browser(
            workspace.clone(),
            cwd.clone(),
            walk_depth,
            follow_links,
            partial.to_string(),
        )
    } else {
        MentionDiscoveryKey::fuzzy(workspace.clone(), cwd.clone(), walk_depth, follow_links)
    };
    app.composer
        .mention_discovery
        .ensure_requested(discovery_key.clone());

    if let Some(ref cache) = app.composer.mention_completion_cache
        && cache.workspace == workspace
        && cache.cwd == cwd
        && cache.partial == partial
        && cache.limit == limit
        && cache.walk_depth == walk_depth
        && cache.behavior == behavior
        && cache.follow_links == follow_links
    {
        return (cache.entries.clone(), true);
    }

    let Some(candidates) = app
        .composer
        .mention_discovery
        .cached_entries(&discovery_key)
    else {
        return (Vec::new(), false);
    };
    let entries = match &discovery_key.behavior {
        MentionDiscoveryBehavior::Fuzzy => {
            let ranked = codewhale_runtime::working_set::rank_completion_candidates(
                candidates, partial, limit,
            );
            super::file_frecency::rerank_by_frecency(ranked)
        }
        MentionDiscoveryBehavior::Browser { .. } => {
            candidates.iter().take(limit).cloned().collect()
        }
    };

    let entries = with_git_mention_entries(entries, partial, limit);

    app.composer.mention_completion_cache = Some(MentionCompletionCache {
        workspace,
        cwd,
        partial: partial.to_string(),
        limit,
        walk_depth,
        behavior,
        follow_links,
        entries: entries.clone(),
    });

    (entries, true)
}

/// Prepend the `@git` / `@diff` tokens that prefix-match `partial` to the path
/// completions, so curated git context is discoverable from the same menu as
/// files (#4067). They lead because a two-entry prefix match is what the user
/// meant when they typed `gi` or `di`, and paths still fill the rest.
///
/// A bare `@` is deliberately left alone: that menu is the file picker, and
/// pushing two fixed tokens above every path would cost a slot on every
/// mention the user makes. The tokens appear as soon as a matching character
/// is typed.
fn with_git_mention_entries(entries: Vec<String>, partial: &str, limit: usize) -> Vec<String> {
    // `mention_menu_limit = 0` is a documented way to disable the popup
    // entirely. The git tokens are menu entries like any other and must
    // respect the same cap, or setting 0 would still pop a one-entry menu.
    if limit == 0 {
        return Vec::new();
    }
    let needle = partial.trim().to_lowercase();
    if needle.is_empty() {
        return entries;
    }
    let matching: Vec<String> = GitMentionKind::iter_all()
        .filter(|kind| kind.token().starts_with(&needle))
        .map(|kind| kind.token().to_string())
        .collect();
    if matching.is_empty() {
        return entries;
    }
    let mut combined = matching;
    combined.truncate(limit);
    for entry in entries {
        if combined.len() >= limit {
            break;
        }
        if !combined.contains(&entry) {
            combined.push(entry);
        }
    }
    combined
}

/// Apply the currently selected `@`-mention popup entry to the composer
/// input, splicing it in place of the `@<partial>` token at the cursor.
/// Returns `true` if a substitution occurred.
///
/// Designed to be invoked by the same keybinding that drives
/// `apply_slash_menu_selection` (Enter / Tab); the caller is responsible
/// for choosing which menu is "active" based on cursor context.
pub fn apply_mention_menu_selection(app: &mut App, entries: &[String]) -> bool {
    if entries.is_empty() {
        return false;
    }
    let Some((byte_start, partial)) =
        partial_file_mention_at_cursor(&app.input, app.cursor_position)
    else {
        return false;
    };
    let selected_idx = app
        .mention_menu_selected
        .min(entries.len().saturating_sub(1));
    let replacement = &entries[selected_idx];
    // #441: bump this path's frecency before we splice it in. The store
    // persists asynchronously, so this never blocks input handling.
    super::file_frecency::record_mention(replacement);
    replace_file_mention(app, byte_start, &partial, &file_mention_body(replacement));
    app.mention_menu_hidden = false;
    app.status_message = Some(format!("Attached @{replacement}"));
    true
}

/// Tab-completion handler for `@file` mentions. Mirrors the slash-command
/// flow: a single match is applied directly; multiple matches with a longer
/// shared prefix extend the partial; otherwise the first few candidates are
/// surfaced via the status line. Returns true when the input was modified or
/// a suggestion was offered, so the caller can short-circuit other handlers.
pub fn try_autocomplete_file_mention(app: &mut App) -> bool {
    let Some((byte_start, partial)) =
        partial_file_mention_at_cursor(&app.input, app.cursor_position)
    else {
        return false;
    };
    let (candidates, ready) = mention_menu_entries(app, &partial, FILE_MENTION_COMPLETION_LIMIT);
    if !ready {
        return true;
    }
    if candidates.is_empty() {
        app.status_message = Some(no_file_mention_matches_status(
            &partial,
            app.mention_walk_depth,
        ));
        return true;
    }
    if candidates.len() == 1 {
        // #441: a unique-match completion is also a "mention" for ranking.
        super::file_frecency::record_mention(&candidates[0]);
        replace_file_mention(
            app,
            byte_start,
            &partial,
            &file_mention_body(&candidates[0]),
        );
        app.status_message = Some(format!("Attached @{}", candidates[0]));
        return true;
    }
    let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
    // Extend only up to the first whitespace: the partial token ends there,
    // so `@My Do` would leave a missing `@My` mention and a dead next Tab.
    // Paths with spaces are completed whole (and quoted) by the unique-match
    // branch or the mention menu.
    let shared = longest_common_prefix(&candidate_refs);
    let shared = shared
        .find(char::is_whitespace)
        .map_or(shared, |idx| &shared[..idx]);
    if shared.len() > partial.len() {
        replace_file_mention(app, byte_start, &partial, shared);
        app.status_message = Some(format!("@{shared}…"));
        return true;
    }
    let preview = candidates
        .iter()
        .take(5)
        .map(|c| format!("@{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    app.status_message = Some(format!("Matches: {preview}"));
    true
}

fn no_file_mention_matches_status(partial: &str, walk_depth: usize) -> String {
    if path_partial_reaches_walk_depth(partial, walk_depth) {
        format!(
            "No files match @{partial} (mention_walk_depth={walk_depth}; use /config set mention_walk_depth 0 to search deeper)"
        )
    } else {
        format!("No files match @{partial}")
    }
}

fn path_partial_reaches_walk_depth(partial: &str, walk_depth: usize) -> bool {
    if walk_depth == 0 {
        return false;
    }
    let component_count = partial
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .count();
    component_count >= walk_depth
}

/// Splice a completion into the input, replacing the `@<partial>` token at
/// `byte_start` with `@<replacement>`. Cursor moves to the end of the new
/// token so further keystrokes extend (or escape via space) naturally.
fn replace_file_mention(app: &mut App, byte_start: usize, partial: &str, replacement: &str) {
    let original_token_len = '@'.len_utf8() + partial.len();
    let original_token_end = byte_start + original_token_len;
    let mut new_input =
        String::with_capacity(app.input.len() - original_token_len + 1 + replacement.len());
    new_input.push_str(&app.input[..byte_start]);
    new_input.push('@');
    new_input.push_str(replacement);
    if original_token_end < app.input.len() {
        new_input.push_str(&app.input[original_token_end..]);
    }
    let new_cursor_chars =
        app.input[..byte_start].chars().count() + 1 + replacement.chars().count();
    app.input = new_input;
    app.cursor_position = new_cursor_chars;
}

pub fn longest_common_prefix<'a>(values: &[&'a str]) -> &'a str {
    let Some(first) = values.first().copied() else {
        return "";
    };
    let mut end = first.len();

    for value in values.iter().skip(1) {
        while end > 0 && !value.starts_with(&first[..end]) {
            end -= 1;
            // Ensure we land on a valid UTF-8 char boundary.
            while end > 0 && !first.is_char_boundary(end) {
                end -= 1;
            }
        }
        if end == 0 {
            return "";
        }
    }

    &first[..end]
}

// ---------------------------------------------------------------------------
//  Expansion at send-time
// ---------------------------------------------------------------------------

/// Append a "Local context from @mentions" block to the user's message when
/// any `@path` references are present. Returns the input unchanged when
/// there are none.
///
/// `cwd` carries the user's launch directory and drives the second
/// resolution pass (issue #101): relative `@<path>` mentions resolve under
/// `cwd` when `workspace.join(path)` doesn't exist, so the user's mental
/// anchor (their shell's pwd) wins when it diverges from `--workspace`.
/// Pass `None` to disable the cwd pass entirely (workspace-only).
///
/// Resolution here never walks the tree on submit (#4365). A miss on the
/// exact two-pass lookup falls back to a bounded, unique-match-only search of
/// the composer's already-built background completion index
/// (`completion_index`, when the caller has one cached); the winning
/// candidate must still exist on disk. Ambiguous, stale, or absent matches
/// stay an honest `<missing-file>` that names only what the user typed —
/// never a fabricated workspace-root path.
///
/// Convenience wrapper that allocates a throwaway cache. Test-only: the real
/// send paths share one cache across the references and payload passes.
#[cfg(test)]
pub fn user_request_with_file_mentions(
    input: &str,
    workspace: &Path,
    cwd: Option<PathBuf>,
) -> String {
    user_request_with_file_mentions_cached(
        input,
        workspace,
        cwd,
        &mut GitMentionCache::default(),
        None,
    )
}

pub fn user_request_with_file_mentions_cached(
    input: &str,
    workspace: &Path,
    cwd: Option<PathBuf>,
    git_cache: &mut GitMentionCache,
    completion_index: Option<&[String]>,
) -> String {
    let Some(context) =
        local_context_from_file_mentions(input, workspace, cwd, git_cache, completion_index)
    else {
        return input.to_string();
    };
    format!("{input}\n\n---\n\nLocal context from @mentions:\n{context}")
}

#[must_use]
pub fn pending_context_previews(input: &str) -> Vec<FileMentionPreview> {
    let mut previews = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for mention in extract_file_mentions(input)
        .into_iter()
        .take(MAX_FILE_MENTIONS_PER_MESSAGE)
    {
        if !seen.insert(mention.clone()) {
            continue;
        }
        // Composer previews stay lexical (no git subprocess from the render
        // loop, same rule as #4365 for path stats); the payload is resolved
        // once at submit time.
        if let Some(kind) = git_mention::git_mention_kind(&mention) {
            previews.push(FileMentionPreview {
                kind: "git".to_string(),
                label: kind.label().to_string(),
                detail: Some("resolved on send".to_string()),
                included: false,
                removable: false,
            });
            continue;
        }
        let media = is_media_path(Path::new(&mention));
        previews.push(FileMentionPreview {
            kind: if media { "media" } else { "mention" }.to_string(),
            label: mention,
            detail: Some(if media {
                "use /attach for media bytes".to_string()
            } else {
                "resolved on send".to_string()
            }),
            // Lexical preview deliberately does not stat the path while the
            // user types. Exact inclusion/missing metadata is resolved once,
            // at submit time, rather than from the render loop (#4365).
            included: false,
            removable: false,
        });
    }

    for attachment in extract_media_attachment_references(input) {
        previews.push(FileMentionPreview {
            kind: attachment.kind,
            label: attachment.path,
            detail: Some("attached media".to_string()),
            included: true,
            removable: true,
        });
    }
    previews
}

/// Convenience wrapper that allocates a throwaway cache. Test-only, as above.
#[cfg(test)]
#[must_use]
pub fn context_references_from_input(
    input: &str,
    workspace: &Path,
    cwd: Option<PathBuf>,
) -> Vec<ContextReference> {
    context_references_from_input_cached(
        input,
        workspace,
        cwd,
        &mut GitMentionCache::default(),
        None,
    )
}

#[must_use]
pub fn context_references_from_input_cached(
    input: &str,
    workspace: &Path,
    cwd: Option<PathBuf>,
    git_cache: &mut GitMentionCache,
    completion_index: Option<&[String]>,
) -> Vec<ContextReference> {
    let mut references = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let ws = Workspace::with_cwd(workspace.to_path_buf(), cwd);

    for mention in extract_file_mentions(input)
        .into_iter()
        .take(MAX_FILE_MENTIONS_PER_MESSAGE)
    {
        // Git mentions resolve against the working tree, not the path index,
        // so the inspector reports their real size and budget (#4067).
        if let Some(kind) = git_mention::git_mention_kind(&mention) {
            let payload = git_cache.resolve(kind, workspace).clone();
            let detail = match payload.unavailable_reason.as_deref() {
                Some(reason) => format!("{}, {reason}", kind.label()),
                None if payload.truncated => format!(
                    "{}, {} bytes truncated at {} budget",
                    kind.label(),
                    payload.bytes,
                    kind.byte_budget()
                ),
                None => format!("{}, {} bytes", kind.label(), payload.bytes),
            };
            let reference = ContextReference {
                kind: ContextReferenceKind::GitContext,
                source: ContextReferenceSource::AtMention,
                badge: "git".to_string(),
                label: kind.token().to_string(),
                target: workspace.display().to_string(),
                included: payload.unavailable_reason.is_none(),
                expanded: false,
                detail: Some(detail),
            };
            if seen.insert(format!("git-mention:{}", kind.token())) {
                references.push(reference);
            }
            continue;
        }

        let (path, display_path, exists) =
            resolve_mention_for_send(&ws, &mention, completion_index);
        let reference = context_reference_for_mention(&mention, &path, &display_path, exists);
        if !seen.insert(format!(
            "{:?}:{:?}:{}:{}",
            reference.source, reference.kind, reference.target, reference.label
        )) {
            continue;
        }
        references.push(reference);
    }

    for reference in extract_media_attachment_references(input) {
        let context_reference = ContextReference {
            kind: ContextReferenceKind::MediaAttachment,
            source: ContextReferenceSource::Attachment,
            badge: reference.kind,
            label: reference.path.clone(),
            target: reference.path,
            included: true,
            expanded: false,
            detail: Some("attached media".to_string()),
        };
        if !seen.insert(format!(
            "{:?}:{:?}:{}:{}",
            context_reference.source,
            context_reference.kind,
            context_reference.target,
            context_reference.label
        )) {
            continue;
        }
        references.push(context_reference);
    }

    references
}

fn context_reference_for_mention(
    raw: &str,
    path: &Path,
    display_path: &str,
    exists: bool,
) -> ContextReference {
    if !exists {
        return ContextReference {
            kind: ContextReferenceKind::Missing,
            source: ContextReferenceSource::AtMention,
            badge: "missing".to_string(),
            label: raw.to_string(),
            // No resolved target exists; naming the workspace-root guess here
            // would present a path we already know is wrong.
            target: raw.to_string(),
            included: false,
            expanded: false,
            detail: Some("not found".to_string()),
        };
    }
    if path.is_dir() {
        return ContextReference {
            kind: ContextReferenceKind::Directory,
            source: ContextReferenceSource::AtMention,
            badge: "dir".to_string(),
            label: raw.to_string(),
            target: display_path.to_string(),
            included: true,
            expanded: true,
            detail: Some("directory listing".to_string()),
        };
    }
    if !path.is_file() {
        return ContextReference {
            kind: ContextReferenceKind::Unsupported,
            source: ContextReferenceSource::AtMention,
            badge: "skipped".to_string(),
            label: raw.to_string(),
            target: display_path.to_string(),
            included: false,
            expanded: false,
            detail: Some("unsupported path".to_string()),
        };
    }
    if is_media_path(path) {
        return ContextReference {
            kind: ContextReferenceKind::MediaMention,
            source: ContextReferenceSource::AtMention,
            badge: "media".to_string(),
            label: raw.to_string(),
            target: display_path.to_string(),
            included: false,
            expanded: false,
            detail: Some("use /attach for media bytes".to_string()),
        };
    }

    let detail = match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_MENTION_FILE_BYTES => {
            Some("included truncated".to_string())
        }
        Ok(_) => Some("included".to_string()),
        Err(err) => Some(format!("metadata: {err}")),
    };

    ContextReference {
        kind: ContextReferenceKind::File,
        source: ContextReferenceSource::AtMention,
        badge: "file".to_string(),
        label: raw.to_string(),
        target: display_path.to_string(),
        included: true,
        expanded: true,
        detail: detail.or_else(|| Some(display_path.to_string())),
    }
}

fn extract_media_attachment_references(input: &str) -> Vec<MediaAttachmentReference> {
    media_attachment_references(input)
}

// ---------------------------------------------------------------------------
//  macOS screencapture-temp stabilization
// ---------------------------------------------------------------------------
//
// macOS parks dragged-out screenshots under a per-capture temp directory like
// `/var/folders/…/T/Temporary Items/NSIRD_screencaptureui_XXXX/` (recent
// releases spell it `TemporaryItems`, without the space) and deletes
// it minutes later. Inbound references to such files are copied to a stable
// directory the moment the message is received, so the agent later reads a
// path that still exists.

/// Marker fragments of the macOS screencapture temp directory layout.
const SCREENCAPTURE_TEMP_DIR_MARKERS: [&str; 2] = ["Temporary Items", "screencaptureui"];

/// The `Temporary Items` component as recent macOS releases spell it.
const SCREENCAPTURE_TEMP_DIR_COMPACT: &str = "TemporaryItems";

/// Stable per-session directory for stabilized screencapture files. Follows
/// the same home-first convention as `clipboard.rs`'s clipboard-images dir.
pub(crate) fn screenshot_stabilization_dir(workspace: &Path) -> PathBuf {
    match codewhale_paths::user_home() {
        Some(home) => home.join(".codewhale").join("attachments"),
        None => workspace.join("attachments"),
    }
}

/// Whether a path lives under a macOS screencapture "Temporary Items" dir:
/// it must have a `Temporary Items` component and a `screencaptureui`-named
/// component (e.g. `NSIRD_screencaptureui_XXXX`), so ordinary user paths are
/// never touched even when their names contain either fragment.
fn is_screencapture_temp_path(path: &Path) -> bool {
    let components: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    components
        .iter()
        .any(|c| c == SCREENCAPTURE_TEMP_DIR_MARKERS[0] || c == SCREENCAPTURE_TEMP_DIR_COMPACT)
        && components
            .iter()
            .any(|c| c.contains(SCREENCAPTURE_TEMP_DIR_MARKERS[1]))
}

/// Keep " at " out of a stable copy's name: `[Attached …: <desc> at <path>]`
/// uses it as the separator, and older parsers split at its last occurrence.
fn stable_attachment_name(file_name: &std::ffi::OsStr) -> String {
    file_name.to_string_lossy().replace(" at ", "-")
}

/// Largest screenshot copied to a stable location.
const MAX_STABLE_ATTACHMENT_BYTES: u64 = 64 * 1024 * 1024;

/// Copy a screencapture temp file to `artifact_dir` and return the stable
/// destination. Returns `None` when the path is not a screencapture temp
/// file, not a regular file, or the copy fails — callers keep the original
/// reference then. Idempotent: an existing destination is reused without a
/// second copy.
///
/// `artifact_dir` is `<root>/<name>`: the destination is written through the
/// pinned no-follow writer anchored at `root`, so a linked `artifact_dir`, a
/// linked destination, or a dangling link at the destination name is refused
/// instead of being written through. `root` itself may be a user-selected link
/// (a relocated `~/.codewhale`).
fn stabilize_screencapture_file(path: &Path, artifact_dir: &Path) -> Option<PathBuf> {
    if !is_screencapture_temp_path(path) || !path.is_file() {
        return None;
    }
    let name = stable_attachment_name(path.file_name()?);
    let dest = artifact_dir.join(&name);
    let relative = Path::new(artifact_dir.file_name()?).join(&name);
    let root = artifact_dir.parent()?;
    // `artifact_dir` sits under the user's own state root unless no home was
    // available and it fell back to the workspace
    // (`screenshot_stabilization_dir`). Only the state root may be a link the
    // user made to relocate `~/.codewhale`.
    let state_root = codewhale_paths::user_home()
        .is_some_and(|home| root == home.join(codewhale_paths::CODEWHALE_APP_DIR));
    let open = if state_root {
        codewhale_util::files::WorkspaceFile::open_resolved_root
    } else {
        codewhale_util::files::WorkspaceFile::open
    };
    let target = open(root, &relative, true).ok()?;
    // An existing regular destination is reused; a link there fails to open.
    if target.open_file().is_ok() {
        return Some(dest);
    }
    if std::fs::metadata(path).ok()?.len() > MAX_STABLE_ATTACHMENT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    match target.publish(&bytes) {
        Ok(()) => Some(dest),
        // Lost a race to another writer of the same name: reuse only if what
        // is there is a regular file we may open.
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists && target.open_file().is_ok() =>
        {
            Some(dest)
        }
        Err(_) => None,
    }
}

/// A path reference found in inbound text: its byte span, the path text, and
/// how it was carried (`@`-mention prefix and/or surrounding quotes).
struct ScreencaptureCandidate {
    byte_start: usize,
    byte_end: usize,
    path: String,
    quote: Option<char>,
    mention: bool,
}

/// Reconstruct a path that an unquoted paste split across whitespace tokens
/// (the "Temporary Items" component contains a space). Returns the char span
/// and joined path of the window that names an existing screencapture temp
/// file, bounded to a handful of tokens either side of `seed`.
fn screencapture_window(
    chars: &[char],
    tokens: &[(usize, usize)],
    seed: usize,
) -> Option<(usize, usize, String)> {
    let max_span = tokens.len().min(12);
    for left in 0..=seed.min(max_span) {
        for right in 0..=max_span {
            let end = (seed + right).min(tokens.len() - 1);
            let (mut ws, mut we) = (tokens[seed - left].0, tokens[end].1);
            let raw: String = chars[ws..we].iter().collect();
            // Trim leading delimiters and trailing punctuation against the
            // raw span (advancing both ends), then collapse interior
            // whitespace runs so the probe path matches the file.
            let lead = raw
                .chars()
                .take_while(|&ch| matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | '@'))
                .count();
            let trimmed = trim_unquoted_mention(&raw);
            ws += lead;
            we -= raw.chars().count() - trimmed.chars().count();
            let joined = trimmed
                .chars()
                .skip(lead)
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let path = Path::new(&joined);
            if is_screencapture_temp_path(path) && path.is_file() {
                return Some((ws, we, joined));
            }
        }
    }
    None
}

/// Rewrite every inbound reference to a macOS screencapture temp file to a
/// stable copy under `artifact_dir`. Non-matching references — not a
/// screencapture path, missing, or an unresolvable copy — are left untouched;
/// the function never fails and never changes the message otherwise.
pub(crate) fn stabilize_screenshot_references(input: &str, artifact_dir: &Path) -> String {
    let chars: Vec<char> = input.chars().collect();
    let offsets: Vec<usize> = input.char_indices().map(|(i, _)| i).collect();
    let mut candidates: Vec<ScreencaptureCandidate> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            // `@"quoted path"` / `@bare/path` mentions (mirror `extract_file_mentions`).
            '@' if is_file_mention_start(&chars, i) => {
                let byte_start = offsets[i];
                if let Some(quote @ ('"' | '\'')) = chars.get(i + 1).copied()
                    && let Some(rel) = chars[i + 2..].iter().position(|&ch| ch == quote)
                {
                    let end = i + 2 + rel;
                    let path: String = chars[i + 2..end].iter().collect();
                    if !path.trim().is_empty() {
                        candidates.push(ScreencaptureCandidate {
                            byte_start,
                            byte_end: offsets.get(end + 1).copied().unwrap_or(input.len()),
                            path: path.trim().to_string(),
                            quote: Some(quote),
                            mention: true,
                        });
                    }
                    i = end + 1;
                } else {
                    let mut end = i + 1;
                    while end < chars.len() && !chars[end].is_whitespace() {
                        end += 1;
                    }
                    let raw: String = chars[i + 1..end].iter().collect();
                    let trimmed = trim_unquoted_mention(&raw);
                    if !trimmed.is_empty() {
                        candidates.push(ScreencaptureCandidate {
                            byte_start,
                            byte_end: offsets.get(end).copied().unwrap_or(input.len()),
                            path: trimmed.to_string(),
                            quote: None,
                            mention: true,
                        });
                    }
                    i = end;
                }
            }
            // Quoted strings: terminals quote drag-dropped paths with spaces.
            // A closing quote is only sought on the same line, so a lone
            // apostrophe in prose never swallows the rest of the message.
            quote @ ('"' | '\'') => {
                if let Some(rel) = chars[i + 1..]
                    .iter()
                    .take_while(|&&ch| ch != '\n')
                    .position(|&ch| ch == quote)
                {
                    let end = i + 1 + rel;
                    let path: String = chars[i + 1..end].iter().collect();
                    if !path.trim().is_empty() {
                        candidates.push(ScreencaptureCandidate {
                            byte_start: offsets[i],
                            byte_end: offsets.get(end + 1).copied().unwrap_or(input.len()),
                            path: path.trim().to_string(),
                            quote: Some(quote),
                            mention: false,
                        });
                    }
                    i = end + 1;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    // Bare unquoted pastes: rebuild the path across whitespace tokens.
    let tokens: Vec<(usize, usize)> = {
        let mut tokens = Vec::new();
        let mut start = None;
        for (idx, ch) in chars.iter().enumerate() {
            match (ch.is_whitespace(), start) {
                (true, Some(s)) => {
                    tokens.push((s, idx));
                    start = None;
                }
                (false, None) => start = Some(idx),
                _ => {}
            }
        }
        if let Some(s) = start {
            tokens.push((s, chars.len()));
        }
        tokens
    };
    for (seed, &(token_start, token_end)) in tokens.iter().enumerate() {
        let token: String = chars[token_start..token_end].iter().collect();
        if !(token.contains("screencaptureui")
            || token.contains("Temporary")
            || token.contains("Items"))
        {
            continue;
        }
        let Some((ws, we, path)) = screencapture_window(&chars, &tokens, seed) else {
            continue;
        };
        let byte_end = offsets.get(we).copied().unwrap_or(input.len());
        // Only suppress the window when it overlaps a candidate that is
        // itself a confirmed screencapture temp file (that candidate will
        // rewrite it); prose quoted with `'` never blocks a real reference.
        let confirmed = |c: &ScreencaptureCandidate| {
            is_screencapture_temp_path(Path::new(&c.path)) && Path::new(&c.path).is_file()
        };
        let blocked = candidates
            .iter()
            .any(|c| c.byte_start < byte_end && offsets[ws] < c.byte_end && confirmed(c));
        if !blocked {
            candidates.push(ScreencaptureCandidate {
                byte_start: offsets[ws],
                byte_end,
                path,
                quote: None,
                mention: false,
            });
        }
    }

    // Apply last-to-first so byte spans stay valid.
    candidates.sort_by_key(|c| c.byte_start);
    let mut output = input.to_string();
    for candidate in candidates.iter().rev() {
        let Some(dest) = stabilize_screencapture_file(Path::new(&candidate.path), artifact_dir)
        else {
            continue;
        };
        let stable = dest.to_string_lossy();
        let mut replacement = String::new();
        if candidate.mention {
            replacement.push('@');
        }
        if let Some(quote) = candidate.quote {
            replacement.push(quote);
        }
        replacement.push_str(stable.as_ref());
        if let Some(quote) = candidate.quote {
            replacement.push(quote);
        }
        output.replace_range(candidate.byte_start..candidate.byte_end, &replacement);
    }
    output
}

fn local_context_from_file_mentions(
    input: &str,
    workspace: &Path,
    cwd: Option<PathBuf>,
    git_cache: &mut GitMentionCache,
    completion_index: Option<&[String]>,
) -> Option<String> {
    let mentions = extract_file_mentions(input);
    if mentions.is_empty() {
        return None;
    }

    let mut blocks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let ws = Workspace::with_cwd(workspace.to_path_buf(), cwd);

    for mention in mentions.into_iter().take(MAX_FILE_MENTIONS_PER_MESSAGE) {
        // `@git` / `@diff` resolve to curated git context, not to a path, so
        // they short-circuit before any workspace path resolution (#4067).
        if let Some(kind) = git_mention::git_mention_kind(&mention) {
            if !seen.insert(format!("git-mention:{}", kind.token())) {
                continue;
            }
            blocks.push(git_cache.resolve(kind, workspace).block.clone());
            continue;
        }

        // `@path:START-END` attaches a line range of a file (issue #5550).
        // Exact resolution wins first: a file that literally contains a colon
        // is treated as its full self. Only a genuine miss re-reads the token
        // as `path:START-END`.
        let mut range = None;
        let mut mention_path = mention.as_str();
        // `Workspace::resolve_exact` already returns absolute paths when the root
        // is absolute (TUI always runs from an absolute workspace), so we
        // skip `canonicalize()` here — it's per-mention I/O on the
        // message-send hot path. Accept the rare symlink-aliasing dedup
        // miss as the cost of avoiding a syscall (Gemini code-review).
        let (mut path, mut display_path, mut exists) =
            resolve_mention_for_send(&ws, mention_path, completion_index);
        if !exists && let Some((path_part, parsed)) = split_mention_range(&mention) {
            let (ranged_path, ranged_display, ranged_exists) =
                resolve_mention_for_send(&ws, path_part, completion_index);
            if ranged_exists {
                range = Some(parsed);
                mention_path = path_part;
                (path, display_path, exists) = (ranged_path, ranged_display, ranged_exists);
            }
        }
        tracing::debug!(
            target: "codewhale_tui::file_mention",
            raw_typed = %mention,
            workspace = %workspace.display(),
            cwd = ?std::env::current_dir().ok(),
            resolved = %display_path,
            exists,
            "file mention resolution",
        );

        // Gate every block — including <missing-file> — through the dedup
        // set so a user typing the same non-existent file twice doesn't
        // waste tokens on duplicate missing-file blocks (Devin code-review).
        // Missing mentions dedup on the typed token: there is no resolved
        // path to key on, and the workspace-root guess must not become one.
        let dedup_key = if exists {
            display_path.clone()
        } else {
            format!("missing:{mention}")
        };
        if !seen.insert(dedup_key) {
            continue;
        }

        if exists {
            blocks.push(render_file_mention_context(
                mention_path,
                &path,
                &display_path,
                range,
            ));
        } else {
            // Honest miss: name only what the user typed. Emitting the
            // workspace-root join as `path=` presented a non-existent file
            // as if it were the resolved target.
            blocks.push(format!("<missing-file mention=\"@{mention}\" />"));
        }
    }

    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n\n"))
    }
}

/// Endpoints of a `@path:START-END` mention (1-based, inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileRange {
    pub start: u32,
    pub end: u32,
}

/// Split a trailing `:START-END` range off a mention token.
///
/// Only an exact `digits-digits` suffix after a non-empty path part is
/// treated as a range, so `notes.txt`, `x:1`, `x:a-b`, `:1-2`, and Windows
/// `C:\\dir\\file` (digits requirement) all stay whole. The caller must have
/// already failed an exact path resolution for the full token before calling.
fn split_mention_range(raw: &str) -> Option<(&str, FileRange)> {
    let (path_part, range_part) = raw.rsplit_once(':')?;
    if path_part.is_empty() {
        return None;
    }
    let (start, end) = range_part.split_once('-')?;
    if start.is_empty()
        || end.is_empty()
        || !start.bytes().all(|b| b.is_ascii_digit())
        || !end.bytes().all(|b| b.is_ascii_digit())
        || start.len() > 5
        || end.len() > 5
    {
        return None;
    }
    let start: u32 = start.parse().ok()?;
    let end: u32 = end.parse().ok()?;
    if start == 0 || end < start {
        return None;
    }
    Some((path_part, FileRange { start, end }))
}

/// Send-time mention resolution: exact two-pass lookup first (workspace root,
/// then launch cwd), then a bounded fallback against the composer's cached
/// background completion index. Returns the path, its display form, and
/// whether it exists. On a miss the returned path is the workspace-root guess
/// — callers must not present it as resolved when `exists` is false.
fn resolve_mention_for_send(
    ws: &Workspace,
    mention: &str,
    completion_index: Option<&[String]>,
) -> (PathBuf, String, bool) {
    let guess = match ws.resolve_exact(mention) {
        Ok(path) => {
            let display = path.display().to_string();
            return (path, display, true);
        }
        Err(guess) => guess,
    };
    if let Some(resolved) = completion_index
        .and_then(|candidates| resolve_mention_in_completion_index(mention, candidates, ws))
    {
        let display = resolved.display().to_string();
        return (resolved, display, true);
    }
    let display = guess.display().to_string();
    (guess, display, false)
}

/// Bounded send-time fallback for `@`-mention misses.
///
/// #4365 keeps filesystem walks off the submit path, so instead of walking we
/// match the typed token against the composer's already-built background
/// completion index (workspace- or cwd-relative display strings). A candidate
/// wins only when it is the *unique* path-suffix or basename match and the
/// winning path still resolves on disk — the index may be a few seconds
/// stale. Anything else (no hit, ambiguous hit, stale hit) returns `None` so
/// the caller emits an honest `<missing-file>` instead of attaching an
/// arbitrary same-name file from a nested directory.
fn resolve_mention_in_completion_index(
    mention: &str,
    candidates: &[String],
    ws: &Workspace,
) -> Option<PathBuf> {
    // Absolute and home-anchored mentions name an exact location; a basename
    // lookalike elsewhere in the tree would be a different file, not a fix-up.
    // A leading separator is rooted on every platform, but `Path::is_absolute`
    // is false for `/foo` on Windows (no drive prefix), which would otherwise
    // let a rooted miss fall through to the index and attach an unrelated
    // same-name file. Test the root marker directly so the guard holds there.
    // `\` is a root marker only on Windows; on Unix it is an ordinary
    // (if unusual) leading filename character, so leave that case alone.
    let rooted = mention.starts_with('/') || (cfg!(windows) && mention.starts_with('\\'));
    if mention.starts_with('~') || rooted || Path::new(mention).is_absolute() {
        return None;
    }
    let needle = mention.replace('\\', "/");
    let needle = needle.trim_matches('/');
    if needle.is_empty() {
        return None;
    }
    let needle_lower = needle.to_lowercase();
    let basename_lower = needle_lower.rsplit('/').next()?;

    let normalized = |candidate: &str| candidate.replace('\\', "/");
    let suffix_match = |candidate: &str| {
        let cand = normalized(candidate);
        let cand = cand.trim_end_matches('/').to_lowercase();
        !cand.is_empty() && (cand == needle_lower || cand.ends_with(&format!("/{needle_lower}")))
    };
    let basename_match = |candidate: &str| {
        let cand = normalized(candidate);
        let cand = cand.trim_end_matches('/').to_lowercase();
        !cand.is_empty() && cand.rsplit('/').next() == Some(basename_lower)
    };

    // Prefer path-suffix hits: they carry the user's typed directory context.
    // Fall back to basename hits only when no suffix hit exists. Either way,
    // more than one distinct winner is ambiguous and resolves nothing.
    let predicates: [&dyn Fn(&str) -> bool; 2] = [&suffix_match, &basename_match];
    for predicate in predicates {
        let mut winner: Option<&str> = None;
        for candidate in candidates {
            if !predicate(candidate) {
                continue;
            }
            match winner {
                None => winner = Some(candidate.as_str()),
                Some(existing) if existing == candidate.as_str() => {}
                Some(_) => return None,
            }
        }
        if let Some(winner) = winner {
            // The index can be stale; only a path that still resolves exists.
            // Display strings are workspace- or cwd-relative, which
            // `resolve_exact` re-anchors exactly like a typed path.
            //
            // Rejoin the components with this platform's separator first.
            // Index strings are `/`-separated, and `root.join("ops/f.md")`
            // keeps that slash verbatim on Windows, yielding a mixed
            // `C:\ws\ops/f.md` that we then hand to the model and print in
            // the context inspector. Same path, inconsistent rendering.
            let native: PathBuf = winner.split('/').filter(|part| !part.is_empty()).collect();
            return ws.resolve_exact(&native.to_string_lossy()).ok();
        }
    }
    None
}

fn extract_file_mentions(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut mentions = Vec::new();
    let mut idx = 0;

    while idx < chars.len() {
        if chars[idx] != '@' || !is_file_mention_start(&chars, idx) {
            idx += 1;
            continue;
        }

        let Some(next) = chars.get(idx + 1).copied() else {
            break;
        };
        if next.is_whitespace() {
            idx += 1;
            continue;
        }

        if matches!(next, '"' | '\'') {
            let quote = next;
            let mut end = idx + 2;
            let mut raw = String::new();
            while end < chars.len() && chars[end] != quote {
                raw.push(chars[end]);
                end += 1;
            }
            if !raw.trim().is_empty() {
                mentions.push(raw.trim().to_string());
            }
            idx = end.saturating_add(1);
            continue;
        }

        let mut end = idx + 1;
        let mut raw = String::new();
        while end < chars.len() && !chars[end].is_whitespace() {
            raw.push(chars[end]);
            end += 1;
        }
        let trimmed = trim_unquoted_mention(&raw);
        if !trimmed.is_empty() {
            mentions.push(trimmed.to_string());
        }
        idx = end;
    }

    mentions
}

fn is_file_mention_start(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    chars
        .get(idx.saturating_sub(1))
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '<' | '"' | '\''))
}

fn trim_unquoted_mention(raw: &str) -> &str {
    let mut trimmed = raw.trim();
    while trimmed.chars().count() > 1
        && trimmed
            .chars()
            .last()
            .is_some_and(|ch| matches!(ch, ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'))
    {
        trimmed = &trimmed[..trimmed.len() - trimmed.chars().last().unwrap().len_utf8()];
    }
    trimmed
}

fn render_file_mention_context(
    raw: &str,
    path: &Path,
    display_path: &str,
    range: Option<FileRange>,
) -> String {
    if !path.exists() {
        return format!("<missing-file mention=\"@{raw}\" path=\"{display_path}\" />");
    }
    if path.is_dir() {
        return render_directory_mention_context(raw, path, display_path);
    }
    if !path.is_file() {
        return format!("<unsupported-path mention=\"@{raw}\" path=\"{display_path}\" />");
    }
    if is_media_path(path) {
        return format!(
            "<media-file mention=\"@{raw}\" path=\"{display_path}\">\nUse /attach {raw} when the intent is to attach this image or video to the next message.\n</media-file>"
        );
    }

    let range_attr = match range {
        Some(FileRange { start, end }) => format!(r#" lines="{start}-{end}""#),
        None => String::new(),
    };
    match read_file_content(path, range) {
        Ok((text, truncated, beyond_eof)) => {
            let truncated_attr = if truncated { " truncated=\"true\"" } else { "" };
            let beyond_attr = if beyond_eof {
                " beyond-eof=\"true\""
            } else {
                ""
            };
            format!(
                "<file mention=\"@{raw}\" path=\"{display_path}\"{range_attr}{truncated_attr}{beyond_attr}>\n{text}\n</file>"
            )
        }
        Err(err) => {
            format!(
                "<unreadable-file mention=\"@{raw}\" path=\"{display_path}\">\n{err}\n</unreadable-file>"
            )
        }
    }
}

fn render_directory_mention_context(raw: &str, path: &Path, display_path: &str) -> String {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => {
            return format!(
                "<unreadable-directory mention=\"@{raw}\" path=\"{display_path}\">\n{err}\n</unreadable-directory>"
            );
        }
    };

    // Keep only the first MAX entries in sort order while counting the rest:
    // a max-heap of that size bounds memory to the output instead of
    // materializing and sorting the whole directory (U07-03).
    let mut total = 0usize;
    let mut kept = std::collections::BinaryHeap::with_capacity(MAX_DIRECTORY_MENTION_ENTRIES + 1);
    for entry in entries.filter_map(|entry| entry.ok()) {
        total += 1;
        let marker = entry
            .file_type()
            .ok()
            .filter(|ty| ty.is_dir())
            .map_or("", |_| "/");
        kept.push(format!("{}{}", entry.file_name().to_string_lossy(), marker));
        if kept.len() > MAX_DIRECTORY_MENTION_ENTRIES {
            kept.pop();
        }
    }
    let names = kept.into_sorted_vec();
    let mut body = names.join("\n");
    if total > MAX_DIRECTORY_MENTION_ENTRIES {
        let omitted = total - MAX_DIRECTORY_MENTION_ENTRIES;
        let _ = write!(body, "\n... {omitted} more entries");
    }
    format!("<directory mention=\"@{raw}\" path=\"{display_path}\">\n{body}\n</directory>")
}

/// Bounded read of a mention's file content, optionally sliced to a line
/// range. Returns `(text, truncated, beyond_eof)`: `truncated` mirrors the
/// full-file byte bound, except for a range that starts past that bound,
/// where it says whether the streamed range itself was cut; `beyond_eof` is
/// set only when the requested range starts past the end of the file.
fn read_file_content(
    path: &Path,
    range: Option<FileRange>,
) -> std::io::Result<(String, bool, bool)> {
    let mut file = std::fs::File::open(path)?;
    let (text, truncated) = read_text_prefix(&mut file)?;
    let Some(FileRange { start, end }) = range else {
        return Ok((text, truncated, false));
    };
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().copied() == Some("") {
        lines.pop();
    }
    let start_idx = usize::try_from(start.saturating_sub(1)).unwrap_or(usize::MAX);
    if start_idx >= lines.len() {
        if truncated {
            // The prefix ended, not the file: read the range from the file
            // itself instead of calling it past EOF (U07-04).
            std::io::Seek::rewind(&mut file)?;
            return read_line_range_past_prefix(file, u64::from(start), u64::from(end));
        }
        return Ok((String::new(), truncated, true));
    }
    let end_idx = usize::try_from(end).unwrap_or(usize::MAX).min(lines.len());
    Ok((lines[start_idx..end_idx].join("\n"), truncated, false))
}

/// Stream a 1-based inclusive line range that starts beyond the bounded
/// prefix. Skipped lines are scanned, never stored; the range text itself is
/// held to the same byte budget as a whole-file mention.
fn read_line_range_past_prefix(
    file: impl Read,
    start: u64,
    end: u64,
) -> std::io::Result<(String, bool, bool)> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(file);
    let mut line = 1u64;
    while line < start {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok((String::new(), false, true));
        }
        let consumed = match buf.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                line += 1;
                newline + 1
            }
            None => buf.len(),
        };
        reader.consume(consumed);
    }
    if reader.fill_buf()?.is_empty() {
        return Ok((String::new(), false, true));
    }
    let budget = usize::try_from(MAX_MENTION_FILE_BYTES).unwrap_or(usize::MAX);
    let mut buffer = Vec::new();
    let mut truncated = false;
    while line <= end {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let (take, newline) = match buf.iter().position(|byte| *byte == b'\n') {
            Some(newline) => (newline + 1, true),
            None => (buf.len(), false),
        };
        let room = budget - buffer.len();
        if take > room {
            buffer.extend_from_slice(&buf[..room]);
            truncated = true;
            break;
        }
        buffer.extend_from_slice(&buf[..take]);
        reader.consume(take);
        if newline {
            line += 1;
        }
    }
    let mut text = decode_bounded_text(buffer, truncated)?;
    if text.ends_with('\n') {
        text.pop();
    }
    Ok((text, truncated, false))
}

fn read_text_prefix(file: &mut impl Read) -> std::io::Result<(String, bool)> {
    let mut buffer = Vec::new();
    file.by_ref()
        .take(MAX_MENTION_FILE_BYTES + 1)
        .read_to_end(&mut buffer)?;
    let truncated = buffer.len() as u64 > MAX_MENTION_FILE_BYTES;
    if truncated {
        buffer.truncate(MAX_MENTION_FILE_BYTES as usize);
    }
    Ok((decode_bounded_text(buffer, truncated)?, truncated))
}

/// Decode a budget-bounded read as UTF-8 text, rejecting binary content.
fn decode_bounded_text(mut buffer: Vec<u8>, truncated: bool) -> std::io::Result<String> {
    if truncated {
        // Round down to the nearest valid UTF-8 character boundary so a
        // multi-byte sequence (CJK, emoji, etc.) is never split at the cut point.
        // Only adjust when error_len() is None — that means truncation landed
        // mid-sequence (incomplete tail).  A Some(_) error_len means the file
        // genuinely contains invalid UTF-8 bytes; leave the buffer intact so
        // the from_utf8 call below returns the correct "file is not UTF-8" error.
        if let Err(e) = std::str::from_utf8(&buffer)
            && e.error_len().is_none()
        {
            buffer.truncate(e.valid_up_to());
        }
    }
    if buffer.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file appears to be binary",
        ));
    }
    let text = std::str::from_utf8(&buffer)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "file is not UTF-8"))?
        .to_string();
    Ok(text)
}

fn is_media_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "tif"
            | "tiff"
            | "ppm"
            | "mp4"
            | "mov"
            | "m4v"
            | "webm"
            | "avi"
            | "mkv"
    )
}

// ---------------------------------------------------------------------------
//  #101 regression repros
// ---------------------------------------------------------------------------
//
// The bug being guarded: typing `@<some/file>` resolved under `--workspace`,
// not the user's launch CWD. When the two diverged (the canonical case is
// `--workspace=/repo` with `pwd=/repo/sub`), every relative `@` token routed
// to the wrong root and the prompt got `<missing-file>` blocks.
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn inserted_mention_bodies_parse_back_to_the_whole_path() {
        for path in [
            "src/main.rs",
            "My Docs/notes.md",
            "Screenshot 2026-09-28 at 10.00.00.png",
            "notes)",
            "say \"hi\" now.md",
        ] {
            let input = format!("look at @{} please", file_mention_body(path));
            assert_eq!(
                extract_file_mentions(&input),
                vec![path.to_string()],
                "{input}"
            );
        }
        assert_eq!(file_mention_body("src/main.rs"), "src/main.rs");
    }

    /// #101 regression — workspace-vs-cwd divergence: `@bar.txt` typed from
    /// the cwd `<root>/sub` MUST resolve to `<root>/sub/bar.txt`, never to
    /// `<root>/bar.txt` (which doesn't exist).
    #[test]
    fn cwd_pass_resolves_when_workspace_pass_misses() {
        let tmp = TempDir::new().expect("tempdir");
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir");
        let bar = sub.join("bar.txt");
        std::fs::write(&bar, "hello bar").expect("write bar");

        let content =
            user_request_with_file_mentions("look at @bar.txt", tmp.path(), Some(sub.clone()));

        // The block must reference the cwd-rooted path with the file's body —
        // and crucially it must NOT collapse to <missing-file>.
        assert!(
            content.contains("hello bar"),
            "expected file body to be inlined; got: {content}",
        );
        assert!(
            !content.contains("<missing-file"),
            "must not surface <missing-file> for a path that exists under cwd; got: {content}",
        );
        let bar_disp = bar.display().to_string();
        assert!(
            content.contains(&bar_disp),
            "expected resolved path {bar_disp} in content; got: {content}",
        );
        // Belt-and-suspenders: the workspace-rooted path doesn't exist and
        // must not appear in the rendered <file path="..."> attribute.
        let wrong = tmp.path().join("bar.txt").display().to_string();
        assert!(
            !content.contains(&format!("path=\"{wrong}\"")),
            "should NOT have routed to {wrong}; got: {content}",
        );
    }

    /// #101 regression — nested workspace path: `@nested/deep/file.md` with
    /// the file at workspace root resolves through the workspace pass.
    #[test]
    fn workspace_pass_resolves_nested_path() {
        let tmp = TempDir::new().expect("tempdir");
        let nested = tmp.path().join("nested/deep");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let file_md = nested.join("file.md");
        std::fs::write(&file_md, "# nested deep").expect("write file_md");

        // Cwd is irrelevant; an unrelated tempdir would do. Pass `None` so we
        // are unambiguously testing the workspace-pass path.
        let content = user_request_with_file_mentions("see @nested/deep/file.md", tmp.path(), None);

        assert!(content.contains("# nested deep"), "got: {content}");
        assert!(!content.contains("<missing-file"), "got: {content}");
        // Path-separator-portable check: the resolved path's filename is the
        // most reliable cross-platform anchor (Windows mixes `/` and `\` when
        // join() preserves user-typed separators).
        let basename = file_md
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file_name utf-8");
        assert!(
            content.contains(basename),
            "basename {basename} not in path; got: {content}",
        );
    }

    /// Snapshot-style check: the rendered `<file>` block for a resolvable
    /// mention must include the expected attributes and contents, and must
    /// NOT contain `<missing-file>`.
    #[test]
    fn resolvable_mention_renders_file_block_not_missing_file() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("guide.md"), "# Guide\nUse the fast path.\n")
            .expect("write");

        let content = user_request_with_file_mentions("read @guide.md", tmp.path(), None);

        // Header + tag presence.
        assert!(content.contains("Local context from @mentions:"));
        assert!(content.contains("<file mention=\"@guide.md\""));
        assert!(content.contains("# Guide\nUse the fast path."));
        assert!(content.ends_with("</file>"), "got: {content}");
        // The bug fingerprint MUST be absent.
        assert!(!content.contains("<missing-file"), "got: {content}");
    }

    /// Negative test: a truly missing path still produces `<missing-file>`
    /// so the user gets an explicit signal instead of silent failure.
    #[test]
    fn truly_missing_mention_still_renders_missing_file() {
        let tmp = TempDir::new().expect("tempdir");

        let content = user_request_with_file_mentions(
            "huh @does/not/exist.txt",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
        );

        assert!(
            content.contains("<missing-file mention=\"@does/not/exist.txt\""),
            "got: {content}",
        );
    }

    #[test]
    fn pending_context_preview_is_lexical_and_does_not_probe_paths() {
        let previews = pending_context_previews("read @guide.md and @missing.md");

        assert_eq!(previews.len(), 2);
        assert_eq!(previews[0].kind, "mention");
        assert_eq!(previews[0].label, "guide.md");
        assert!(!previews[0].included);
        assert_eq!(previews[0].detail.as_deref(), Some("resolved on send"));
        assert_eq!(previews[1].kind, "mention");
        assert_eq!(previews[1].label, "missing.md");
        assert!(!previews[1].included);
        assert_eq!(previews[1].detail.as_deref(), Some("resolved on send"));
    }

    #[test]
    fn pending_context_preview_distinguishes_attach_media_from_at_media() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("photo.png"), b"png").expect("write");
        let attached = tmp.path().join("photo.png").display().to_string();
        let input = format!("inspect @photo.png\n[Attached image: {attached}]");

        let previews = pending_context_previews(&input);

        assert!(
            previews
                .iter()
                .any(|item| item.kind == "media" && !item.included),
            "at-mention media should be hint-only: {previews:?}"
        );
        assert!(
            previews
                .iter()
                .any(|item| item.kind == "image" && item.included),
            "/attach media should be included: {previews:?}"
        );
    }

    #[test]
    fn manually_typed_basename_does_not_fuzzy_attach_nested_file() {
        let tmp = TempDir::new().expect("tempdir");
        let nested = tmp.path().join("nested");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("guide.md"), "nested secret").expect("write");

        // With no completion index on hand there is no send-time fallback:
        // the miss stays an explicit <missing-file> rather than attaching an
        // arbitrary same-name file from a nested directory (#4365).
        let content = user_request_with_file_mentions("read @guide.md", tmp.path(), None);

        assert!(
            content.contains("<missing-file mention=\"@guide.md\""),
            "a manually typed basename should remain exact: {content}",
        );
        assert!(
            !content.contains("nested secret"),
            "exact resolution must not silently attach a fuzzy nested match: {content}",
        );
    }

    // ---------------------------------------------------------------------
    //  Send-time completion-index fallback
    // ---------------------------------------------------------------------
    //
    // The dogfood failure this guards: `@FINISH-0.9.4.md` typed at the
    // workspace root resolved "not found" and injected a <missing-file> block
    // carrying the wrong (workspace-root) path, even though the file sat one
    // directory down. Misses now fall back to a bounded unique-match search
    // of the composer's background completion index; unresolvable misses emit
    // an honest block that names only what the user typed.

    fn expand_with_index(
        input: &str,
        workspace: &Path,
        cwd: Option<PathBuf>,
        index: &[String],
    ) -> String {
        user_request_with_file_mentions_cached(
            input,
            workspace,
            cwd,
            &mut GitMentionCache::default(),
            Some(index),
        )
    }

    /// A unique basename hit in the completion index resolves a nested file
    /// and injects its real path.
    #[test]
    fn mention_miss_resolves_via_unique_index_basename() {
        let tmp = TempDir::new().expect("tempdir");
        let nested = tmp.path().join("ops");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("FINISH-0.9.4.md"), "ship list").expect("write");

        let index = vec!["ops/FINISH-0.9.4.md".to_string(), "README.md".to_string()];
        let content = expand_with_index("finish @FINISH-0.9.4.md", tmp.path(), None, &index);

        assert!(content.contains("ship list"), "got: {content}");
        assert!(!content.contains("<missing-file"), "got: {content}");
        let real = nested.join("FINISH-0.9.4.md").display().to_string();
        assert!(
            content.contains(&real),
            "expected resolved path {real} in content; got: {content}",
        );
    }

    /// A typed partial path resolves through a unique path-suffix hit.
    #[test]
    fn mention_miss_resolves_via_unique_index_suffix() {
        let tmp = TempDir::new().expect("tempdir");
        let nested = tmp.path().join("nested/deep");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("file.md"), "deep body").expect("write");
        // A same-basename file elsewhere must not make the suffix hit
        // ambiguous: the typed directory context disambiguates.
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).expect("mkdir");
        std::fs::write(other.join("file.md"), "other body").expect("write");

        let index = vec![
            "nested/deep/file.md".to_string(),
            "other/file.md".to_string(),
        ];
        let content = expand_with_index("see @deep/file.md", tmp.path(), None, &index);

        assert!(content.contains("deep body"), "got: {content}");
        assert!(!content.contains("other body"), "got: {content}");
        assert!(!content.contains("<missing-file"), "got: {content}");
    }

    /// Two same-basename candidates with no typed directory context are
    /// ambiguous: nothing is attached and the miss stays explicit.
    #[test]
    fn ambiguous_index_basename_stays_missing() {
        let tmp = TempDir::new().expect("tempdir");
        for dir in ["a", "b"] {
            std::fs::create_dir_all(tmp.path().join(dir)).expect("mkdir");
            std::fs::write(tmp.path().join(dir).join("guide.md"), format!("body {dir}"))
                .expect("write");
        }

        let index = vec!["a/guide.md".to_string(), "b/guide.md".to_string()];
        let content = expand_with_index("read @guide.md", tmp.path(), None, &index);

        assert!(
            content.contains("<missing-file mention=\"@guide.md\""),
            "an ambiguous basename must not attach an arbitrary winner: {content}",
        );
        assert!(!content.contains("body a"), "got: {content}");
        assert!(!content.contains("body b"), "got: {content}");
    }

    /// A stale index entry (file deleted after the scan) must not attach.
    #[test]
    fn stale_index_entry_stays_missing() {
        let tmp = TempDir::new().expect("tempdir");

        let index = vec!["ghost.md".to_string()];
        let content = expand_with_index("boo @ghost.md", tmp.path(), None, &index);

        assert!(
            content.contains("<missing-file mention=\"@ghost.md\" />"),
            "got: {content}",
        );
    }

    /// Absolute mentions name an exact location; the index must never
    /// substitute a same-basename file from inside the workspace.
    #[test]
    fn absolute_mention_miss_never_uses_index() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("guide.md"), "workspace guide").expect("write");

        let index = vec!["guide.md".to_string()];
        let content = expand_with_index(
            "read @/definitely/absent/guide.md",
            tmp.path(),
            None,
            &index,
        );

        assert!(
            content.contains("<missing-file mention=\"@/definitely/absent/guide.md\" />"),
            "got: {content}",
        );
        assert!(!content.contains("workspace guide"), "got: {content}");
    }

    /// The honest miss format: the block names only the typed mention and
    /// never the non-existent workspace-root join.
    #[test]
    fn missing_file_block_names_only_the_typed_mention() {
        let tmp = TempDir::new().expect("tempdir");

        let content = user_request_with_file_mentions(
            "huh @does/not/exist.txt",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
        );

        assert!(
            content.contains("<missing-file mention=\"@does/not/exist.txt\" />"),
            "got: {content}",
        );
        let wrong = tmp.path().join("does/not/exist.txt").display().to_string();
        assert!(
            !content.contains(&wrong),
            "must not inject the wrong workspace-root path {wrong}; got: {content}",
        );
    }

    /// The context inspector mirrors the payload: index-resolved mentions
    /// report their real path, unresolved ones report the typed token.
    #[test]
    fn context_references_reflect_index_resolution() {
        let tmp = TempDir::new().expect("tempdir");
        let nested = tmp.path().join("ops");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("runbook.md"), "steps").expect("write");

        let index = vec!["ops/runbook.md".to_string()];
        let references = context_references_from_input_cached(
            "read @runbook.md and @absent.md",
            tmp.path(),
            None,
            &mut GitMentionCache::default(),
            Some(&index),
        );

        let resolved = references
            .iter()
            .find(|r| r.label == "runbook.md")
            .expect("runbook reference");
        assert_eq!(resolved.kind, ContextReferenceKind::File);
        assert!(resolved.included);
        let real = nested.join("runbook.md").display().to_string();
        assert_eq!(resolved.target, real, "{resolved:?}");

        let missing = references
            .iter()
            .find(|r| r.label == "absent.md")
            .expect("absent reference");
        assert_eq!(missing.kind, ContextReferenceKind::Missing);
        assert!(!missing.included);
        assert_eq!(
            missing.target, "absent.md",
            "a missing mention must not report the workspace-root guess as its target: {missing:?}",
        );
    }

    #[test]
    fn media_attachment_references_include_removable_line_ranges() {
        let input = "before\n[Attached image: 8x4 PNG at /tmp/pasted.png]\nafter";

        let references = media_attachment_references(input);

        assert_eq!(references.len(), 1);
        let reference = &references[0];
        assert_eq!(reference.kind, "image");
        assert_eq!(reference.path, "/tmp/pasted.png");
        assert_eq!(
            &input[reference.start_byte..reference.end_byte],
            "[Attached image: 8x4 PNG at /tmp/pasted.png]\n"
        );
    }

    #[test]
    fn context_references_preserve_exact_targets_and_roundtrip() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").expect("write");
        let input = "read @src/main.rs";

        let references =
            context_references_from_input(input, tmp.path(), Some(tmp.path().to_path_buf()));

        assert_eq!(references.len(), 1);
        let reference = &references[0];
        assert_eq!(reference.kind, ContextReferenceKind::File);
        assert_eq!(reference.source, ContextReferenceSource::AtMention);
        assert_eq!(reference.label, "src/main.rs");
        assert!(reference.target.ends_with("src/main.rs"));
        assert!(reference.included);
        assert!(reference.expanded);

        let encoded = serde_json::to_string(reference).expect("serialize");
        let decoded: ContextReference = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(&decoded, reference);
    }

    /// Regression test for #1441: truncating at MAX_MENTION_FILE_BYTES must not
    /// split a multi-byte UTF-8 sequence, which previously produced U+FFFD
    /// replacement characters in the TUI output.
    #[test]
    fn read_text_prefix_truncation_respects_utf8_char_boundary() {
        use std::io::Write;

        // Build a file that is MAX_MENTION_FILE_BYTES - 1 ASCII bytes followed
        // by a 3-byte CJK character (U+4E2D, '中'). The naive truncate at
        // MAX_MENTION_FILE_BYTES cuts after the first byte of '中', producing
        // an invalid sequence.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("cjk.txt");
        let mut f = std::fs::File::create(&path).expect("create");
        let padding = vec![b'a'; MAX_MENTION_FILE_BYTES as usize - 1];
        f.write_all(&padding).expect("write padding");
        f.write_all("中".as_bytes()).expect("write CJK");

        let (text, truncated, beyond_eof) = read_file_content(&path, None).expect("should succeed");
        assert!(!beyond_eof);
        assert!(
            truncated,
            "file exceeds limit so should be marked truncated"
        );
        assert!(
            !text.contains('\u{FFFD}'),
            "truncated text must not contain replacement characters; got: {text:?}",
        );
    }

    #[test]
    fn mention_range_splitting_accepts_only_exact_digit_pairs() {
        assert_eq!(
            split_mention_range("src/lib.rs:120-160"),
            Some((
                "src/lib.rs",
                FileRange {
                    start: 120,
                    end: 160
                }
            )),
        );
        assert_eq!(
            split_mention_range("x:1-2"),
            Some(("x", FileRange { start: 1, end: 2 })),
        );
        for whole in [
            "notes.txt",
            "x:1",
            "x:a-b",
            ":1-2",
            "x:1-a",
            "x:0-2",
            "x:2-1",
        ] {
            assert_eq!(split_mention_range(whole), None, "{whole} must stay whole");
        }
    }

    /// U07-04: a range that starts past the bounded prefix is read from the
    /// file, not reported as past EOF; a range past the real end still is.
    #[test]
    fn ranged_file_mention_reads_past_the_bounded_prefix() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("long.txt");
        let body: String = (1..=20_000).map(|n| format!("line {n}\n")).collect();
        assert!(body.len() as u64 > MAX_MENTION_FILE_BYTES);
        std::fs::write(&path, body).expect("write");

        let (text, truncated, beyond_eof) = read_file_content(
            &path,
            Some(FileRange {
                start: 19_998,
                end: 20_000,
            }),
        )
        .expect("range read");
        assert!(!beyond_eof, "the file has these lines");
        assert!(!truncated, "three short lines fit the budget");
        assert_eq!(text, "line 19998\nline 19999\nline 20000");

        let (_, _, beyond_eof) = read_file_content(
            &path,
            Some(FileRange {
                start: 20_001,
                end: 20_002,
            }),
        )
        .expect("range read");
        assert!(beyond_eof, "the file really ends at line 20000");
    }

    #[test]
    fn ranged_file_mention_slices_lines_and_reports_beyond_eof() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("lines.rs");
        std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write");

        let (text, truncated, beyond_eof) =
            read_file_content(&path, Some(FileRange { start: 2, end: 4 })).expect("range read");
        assert!(!truncated);
        assert!(!beyond_eof);
        assert_eq!(text, "two\nthree\nfour");

        // An end past the file clamps to what exists.
        let (text, _, beyond_eof) =
            read_file_content(&path, Some(FileRange { start: 4, end: 99 })).expect("clamped range");
        assert!(!beyond_eof);
        assert_eq!(text, "four\nfive");

        // A start past the end is flagged honestly.
        let (text, _, beyond_eof) =
            read_file_content(&path, Some(FileRange { start: 9, end: 12 })).expect("beyond range");
        assert!(beyond_eof);
        assert!(text.is_empty());
    }

    #[test]
    fn ranged_mention_render_annotates_lines_and_honours_the_byte_bound() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("notes.rs");
        std::fs::write(&path, "a\nb\nc\nd\n").expect("write");
        let rendered = render_file_mention_context(
            "notes.rs:2-3",
            &path,
            "notes.rs",
            Some(FileRange { start: 2, end: 3 }),
        );
        assert!(rendered.contains(r#"lines="2-3""#), "{rendered}");
        assert!(rendered.contains("\nb\nc\n"));

        let beyond = render_file_mention_context(
            "notes.rs:80-90",
            &path,
            "notes.rs",
            Some(FileRange { start: 80, end: 90 }),
        );
        assert!(beyond.contains(r#"beyond-eof="true""#), "{beyond}");
    }
    // ---------------------------------------------------------------------
    //  #4067 — @git / @diff composer mentions
    // ---------------------------------------------------------------------

    fn init_test_repo(dir: &Path) {
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .expect("git available in tests");
            assert!(out.status.success(), "git {args:?} failed");
        }
    }

    fn commit_test_repo(dir: &Path) {
        for args in [vec!["add", "-A"], vec!["commit", "-m", "initial"]] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .expect("git available in tests");
        }
    }

    #[test]
    fn git_and_diff_mentions_inline_curated_context_not_paths() {
        let tmp = TempDir::new().expect("tempdir");
        init_test_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "one\n").expect("write");
        commit_test_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "two\n").expect("write");

        let expanded = user_request_with_file_mentions(
            "look at @git and @diff",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
        );

        assert!(expanded.contains("<git-status"), "{expanded}");
        assert!(expanded.contains("<git-diff"), "{expanded}");
        assert!(expanded.contains("a.txt"), "{expanded}");
        // The tokens are not treated as paths, so no missing-file block.
        assert!(!expanded.contains("<missing-file"), "{expanded}");
    }

    #[test]
    fn git_mentions_outside_a_repository_say_so_explicitly() {
        let tmp = TempDir::new().expect("tempdir");
        let expanded = user_request_with_file_mentions(
            "status? @git",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
        );
        assert!(expanded.contains("<git-unavailable"), "{expanded}");
        assert!(expanded.contains("not a git repository"), "{expanded}");
    }

    #[test]
    fn git_mention_is_deduplicated_within_one_message() {
        let tmp = TempDir::new().expect("tempdir");
        init_test_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "one\n").expect("write");
        commit_test_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "two\n").expect("write");

        let expanded = user_request_with_file_mentions(
            "@diff and again @diff",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
        );
        assert_eq!(expanded.matches("<git-diff").count(), 1, "{expanded}");
    }

    #[test]
    fn paths_that_merely_start_with_git_stay_file_mentions() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("diff.txt"), "plain file").expect("write");

        let expanded = user_request_with_file_mentions(
            "see @diff.txt",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
        );
        assert!(expanded.contains("plain file"), "{expanded}");
        assert!(!expanded.contains("<git-diff"), "{expanded}");
    }

    #[test]
    fn large_diff_is_truncated_and_the_inspector_reports_the_budget() {
        let tmp = TempDir::new().expect("tempdir");
        init_test_repo(tmp.path());
        std::fs::write(tmp.path().join("big.txt"), "seed\n").expect("write");
        commit_test_repo(tmp.path());
        let bulk: String = (0..40_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(tmp.path().join("big.txt"), bulk).expect("write");

        let expanded =
            user_request_with_file_mentions("@diff", tmp.path(), Some(tmp.path().to_path_buf()));
        assert!(
            expanded.contains("truncated=\"true\""),
            "expected truncation marker"
        );

        let references =
            context_references_from_input("@diff", tmp.path(), Some(tmp.path().to_path_buf()));
        let git_ref = references
            .iter()
            .find(|r| r.kind == ContextReferenceKind::GitContext)
            .expect("git reference present in the inspector");
        assert_eq!(git_ref.label, "diff");
        assert!(git_ref.included);
        let detail = git_ref.detail.clone().unwrap_or_default();
        assert!(detail.contains("truncated at"), "{detail}");
        assert!(
            detail.contains(&crate::tui::git_mention::MAX_GIT_DIFF_MENTION_BYTES.to_string()),
            "{detail}"
        );
    }

    #[test]
    fn empty_repository_reference_is_visible_but_not_included() {
        let tmp = TempDir::new().expect("tempdir");
        init_test_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "one\n").expect("write");
        commit_test_repo(tmp.path());

        let references =
            context_references_from_input("@diff", tmp.path(), Some(tmp.path().to_path_buf()));
        let git_ref = references
            .iter()
            .find(|r| r.kind == ContextReferenceKind::GitContext)
            .expect("git reference present even when there is nothing to show");
        assert!(!git_ref.included);
        assert!(
            git_ref
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("no working-tree changes")),
            "{:?}",
            git_ref.detail
        );
    }

    #[test]
    fn composer_preview_lists_git_mentions_without_running_git() {
        let previews = pending_context_previews("@git @diff");
        let kinds: Vec<&str> = previews.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(kinds, vec!["git", "git"]);
        assert!(previews.iter().all(|p| !p.included));
    }

    /// #4067 review follow-up: `mention_menu_limit = 0` is a documented way to
    /// disable the popup. The git tokens are menu entries like any other and
    /// must respect the same cap — otherwise setting 0 still pops a one-entry
    /// menu the moment the user types `@g`.
    #[test]
    fn git_mention_entries_respect_a_zero_menu_limit() {
        let paths = vec!["src/main.rs".to_string()];
        assert!(with_git_mention_entries(paths.clone(), "g", 0).is_empty());
        assert!(with_git_mention_entries(paths.clone(), "d", 0).is_empty());
        assert!(with_git_mention_entries(paths.clone(), "", 0).is_empty());
        assert!(with_git_mention_entries(Vec::new(), "gi", 0).is_empty());
    }

    /// A small non-zero limit must cap the token list this function builds.
    ///
    /// The empty-partial branch is pass-through by design — those entries were
    /// already capped upstream by `rank_completion_candidates`, and shrinking
    /// them here would silently drop paths the caller asked for.
    #[test]
    fn git_mention_entries_never_exceed_the_menu_limit() {
        let paths = vec!["a.rs".to_string(), "b.rs".to_string()];
        for limit in 1..=4 {
            let matched = with_git_mention_entries(paths.clone(), "d", limit);
            assert!(matched.len() <= limit, "limit {limit}: {matched:?}");
            // Both tokens match a bare prefix that hits `git` and `diff`
            // through separate entries; the cap still holds.
            let both = with_git_mention_entries(Vec::new(), "", limit);
            assert!(both.len() <= limit, "limit {limit}: {both:?}");
        }
        // Pass-through: the caller's already-capped paths survive untouched.
        assert_eq!(with_git_mention_entries(paths.clone(), "", 1), paths);
    }

    /// #4067 review follow-up: one submit resolves a git mention once, not
    /// once per surface. `@diff` makes git compute the whole working-tree diff
    /// before the byte budget applies, so a repeat is real wasted work.
    #[test]
    fn a_submit_resolves_each_git_mention_only_once() {
        let tmp = TempDir::new().expect("tempdir");
        init_test_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "one\n").expect("write");
        commit_test_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "two\n").expect("write");

        let mut cache = crate::tui::git_mention::GitMentionCache::default();
        let references = context_references_from_input_cached(
            "@diff",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
            &mut cache,
            None,
        );
        let expanded = user_request_with_file_mentions_cached(
            "@diff",
            tmp.path(),
            Some(tmp.path().to_path_buf()),
            &mut cache,
            None,
        );

        // Both surfaces describe the same resolution.
        let git_ref = references
            .iter()
            .find(|r| r.kind == ContextReferenceKind::GitContext)
            .expect("git reference");
        assert!(git_ref.included);
        assert!(expanded.contains("<git-diff"), "{expanded}");

        // And the shared cache holds exactly one entry for it.
        assert_eq!(cache.len(), 1, "the diff must be resolved once per submit");
    }

    #[test]
    fn completion_offers_git_and_diff_alongside_paths() {
        let paths = vec!["src/main.rs".to_string(), "docs/guide.md".to_string()];
        // A bare `@` stays the file picker.
        assert_eq!(with_git_mention_entries(paths.clone(), "", 8), paths);

        let narrowed = with_git_mention_entries(paths.clone(), "di", 8);
        assert_eq!(narrowed.first().map(String::as_str), Some("diff"));
        assert!(narrowed.contains(&"src/main.rs".to_string()));

        let git_only = with_git_mention_entries(paths.clone(), "g", 8);
        assert_eq!(git_only.first().map(String::as_str), Some("git"));

        // A partial that matches no token leaves path completion untouched.
        assert_eq!(with_git_mention_entries(paths.clone(), "src", 8), paths);
    }

    // ------------------------------------------------------------------
    // macOS screencapture-temp stabilization
    // ------------------------------------------------------------------

    /// A fake macOS screencapture temp tree. Returns the tempdir (alive for
    /// the test's duration), the screenshot source path, and the artifact dir
    /// stabilization should copy into.
    fn screencapture_fixture() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let source_dir = tmp
            .path()
            .join("Temporary Items")
            .join("NSIRD_screencaptureui_7F3A");
        std::fs::create_dir_all(&source_dir).expect("mkdir");
        let source = source_dir.join("Screenshot 2026-08-10 at 01.09.39 截图.png");
        std::fs::write(&source, b"fake screenshot bytes").expect("write");
        let artifact_dir = tmp.path().join("attachments");
        (tmp, source, artifact_dir)
    }

    #[test]
    fn detects_screencapture_temp_paths() {
        assert!(is_screencapture_temp_path(Path::new(
            "/var/folders/x/T/Temporary Items/NSIRD_screencaptureui_ABC/Shot.png"
        )));
        // The spelling in the founder's failing drop (macOS 26).
        assert!(is_screencapture_temp_path(Path::new(
            "/var/folders/gc/x/T/TemporaryItems/NSIRD_screencaptureui_IqPorQ/Screenshot 2026-10-04 at 22.25.47.png"
        )));
        assert!(!is_screencapture_temp_path(Path::new(
            "/tmp/TemporaryItems/Shot.png"
        )));
        let (tmp, source, _) = screencapture_fixture();
        assert!(is_screencapture_temp_path(&source));
        // Only one marker is not a screencapture temp location.
        assert!(!is_screencapture_temp_path(Path::new(
            "/tmp/Temporary Items/Shot.png"
        )));
        assert!(!is_screencapture_temp_path(Path::new("/tmp/Shot.png")));
        assert!(!is_screencapture_temp_path(tmp.path()));
    }

    #[test]
    fn stabilizes_a_quoted_paste_with_spaces_and_unicode() {
        let (tmp, source, artifact_dir) = screencapture_fixture();
        let input = format!("take a look at \"{}\" please", source.display());
        let out = stabilize_screenshot_references(&input, &artifact_dir);

        let stable = artifact_dir.join("Screenshot 2026-08-10-01.09.39 截图.png");
        assert_eq!(
            std::fs::read(&stable).expect("stable copy"),
            b"fake screenshot bytes"
        );
        assert!(out.contains(&stable.display().to_string()), "got: {out}");
        assert!(!out.contains(&source.display().to_string()), "got: {out}");
        assert!(source.is_file(), "source must be left in place");
        // Idempotent: the stable path is not a screencapture reference, and a
        // second pass over the rewritten text changes nothing.
        assert_eq!(stabilize_screenshot_references(&out, &artifact_dir), out);
        let _ = tmp;
    }

    #[test]
    fn stabilizes_mention_attached_and_unquoted_references() {
        let (tmp, source, artifact_dir) = screencapture_fixture();
        let disp = source.display().to_string();
        let input = format!("see @\"{disp}\", also [Attached image: {disp}] and bare {disp} here");
        let out = stabilize_screenshot_references(&input, &artifact_dir);

        let stable = artifact_dir.join("Screenshot 2026-08-10-01.09.39 截图.png");
        let stable_disp = stable.display().to_string();
        // Three different carriers, one stable destination each time, and
        // exactly one physical copy despite the duplicates.
        assert_eq!(out.matches(&stable_disp).count(), 3, "got: {out}");
        assert!(!out.contains(&disp), "got: {out}");
        assert_eq!(
            std::fs::read_dir(&artifact_dir).expect("artifacts").count(),
            1
        );
        let _ = tmp;
    }

    #[test]
    fn leaves_missing_and_non_screencapture_paths_alone() {
        let (tmp, source, artifact_dir) = screencapture_fixture();
        let ghost = source.with_file_name("Screenshot 1999-01-01 at 00.00.00.png");
        let input = format!(
            "missing {} regular /tmp/notes.txt mention @README.md quote \"no file\"",
            ghost.display()
        );
        let out = stabilize_screenshot_references(&input, &artifact_dir);
        assert_eq!(out, input);
        assert!(std::fs::read_dir(&artifact_dir).is_err());
        let _ = tmp;
    }

    #[test]
    fn single_quoted_prose_does_not_block_a_real_reference() {
        let (tmp, source, artifact_dir) = screencapture_fixture();
        let disp = source.display().to_string();
        // The `'` pair encloses the path but is prose, not a quoted path.
        let input = format!("'check {disp} is here' please");
        let out = stabilize_screenshot_references(&input, &artifact_dir);
        let stable = artifact_dir.join("Screenshot 2026-08-10-01.09.39 截图.png");
        assert!(out.contains(&stable.display().to_string()), "got: {out}");
        assert!(!out.contains(&disp), "got: {out}");
        let _ = tmp;
    }

    #[test]
    fn handles_leading_delimiters_on_unquoted_pastes() {
        let (tmp, source, artifact_dir) = screencapture_fixture();
        let disp = source.display().to_string();
        // Paren-wrapped paste: the `(` rides on the first path token, and an
        // unquoted `@`-prefixed paste keeps its `@` in the rewritten text.
        let input = format!("see ({disp}) and also @{disp} thanks");
        let out = stabilize_screenshot_references(&input, &artifact_dir);
        let stable = artifact_dir.join("Screenshot 2026-08-10-01.09.39 截图.png");
        let stable_disp = stable.display().to_string();
        assert_eq!(out.matches(&stable_disp).count(), 2, "got: {out}");
        assert!(out.contains(&format!("({stable_disp})")), "got: {out}");
        assert!(out.contains(&format!("@{stable_disp}")), "got: {out}");
        assert!(!out.contains(&disp), "got: {out}");
        let _ = tmp;
    }

    #[test]
    fn stabilizes_a_dropped_attachment_under_compact_temporary_items() {
        let tmp = TempDir::new().expect("tempdir");
        let source_dir = tmp
            .path()
            .join("TemporaryItems")
            .join("NSIRD_screencaptureui_IqPorQ");
        std::fs::create_dir_all(&source_dir).expect("mkdir");
        let source = source_dir.join("Screenshot 2026-10-04 at 22.25.47.png");
        std::fs::write(&source, b"screenshot").expect("write");
        let artifact_dir = tmp.path().join("attachments");
        let input = format!("what is this?\n[Attached image: {}]\n", source.display());

        let out = stabilize_screenshot_references(&input, &artifact_dir);

        let stable = artifact_dir.join("Screenshot 2026-10-04-22.25.47.png");
        assert!(stable.is_file(), "stable copy must exist");
        assert!(
            out.contains(&format!("[Attached image: {}]", stable.display())),
            "got: {out}"
        );
        let _ = tmp;
    }

    #[test]
    fn handles_a_multibyte_final_filename_char() {
        let tmp = TempDir::new().expect("tempdir");
        let source_dir = tmp
            .path()
            .join("Temporary Items")
            .join("NSIRD_screencaptureui_9B2C");
        std::fs::create_dir_all(&source_dir).expect("mkdir");
        // No ASCII extension: the reference span ends on a CJK code point.
        let source = source_dir.join("截图");
        std::fs::write(&source, b"screenshot").expect("write");
        let artifact_dir = tmp.path().join("attachments");
        let input = format!("here {} it is", source.display());
        let out = stabilize_screenshot_references(&input, &artifact_dir);
        let stable = artifact_dir.join("截图");
        assert!(out.contains(&stable.display().to_string()), "got: {out}");
        assert!(!out.contains(&source.display().to_string()), "got: {out}");
        let _ = tmp;
    }

    /// The stable copy is written through a no-follow writer: a dangling link
    /// at the destination name, a linked destination, and a linked directory
    /// are all refused, and nothing is created behind them.
    #[cfg(unix)]
    #[test]
    fn stable_copies_are_never_written_through_a_link() {
        use std::os::unix::fs::symlink;
        let (tmp, source, artifact_dir) = screencapture_fixture();
        let outside = TempDir::new().expect("outside");
        let input = format!("see \"{}\"", source.display());
        let name = "Screenshot 2026-08-10-01.09.39 截图.png";

        // Dangling link at the destination name.
        std::fs::create_dir_all(&artifact_dir).expect("mkdir");
        let behind = outside.path().join("created-by-the-link");
        symlink(&behind, artifact_dir.join(name)).expect("link");
        assert_eq!(
            stabilize_screenshot_references(&input, &artifact_dir),
            input
        );
        assert!(
            !behind.exists(),
            "a dangling link must not be written through"
        );

        // A linked destination directory.
        let linked_dir = tmp.path().join("linked-attachments");
        symlink(outside.path(), &linked_dir).expect("link");
        assert_eq!(stabilize_screenshot_references(&input, &linked_dir), input);
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        let _ = tmp;
    }

    #[test]
    fn keeps_the_original_reference_when_the_copy_fails() {
        let (tmp, source, _) = screencapture_fixture();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"x").expect("write");
        // create_dir_all under a regular file must fail.
        let bad_artifact_dir = blocker.join("attachments");
        let input = format!("see \"{}\"", source.display());
        let out = stabilize_screenshot_references(&input, &bad_artifact_dir);
        assert_eq!(out, input);
        assert!(source.is_file());
        let _ = tmp;
    }
}
