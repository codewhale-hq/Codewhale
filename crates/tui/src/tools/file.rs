//! File system engines for the lowercase `read`, `write`, and `edit` primitives
//! plus deferred workspace helpers such as `list_dir`. The older `File`,
//! `read_file`, `write_file`, and `edit_file` names remain registered but hidden
//! so saved sessions can replay their original schemas and behavior.
//!
//! These tools provide safe file system operations within the workspace,
//! with path validation to prevent escaping the workspace boundary.

use super::diff_format::make_unified_diff;
use super::rust_format::{NORMALIZED_NOTE, normalize_edit};
use super::spec::{
    ApprovalRequirement, RichToolResult, ToolCapability, ToolContext, ToolError, ToolResult,
    ToolSpec, lsp_diagnostics_for_paths, optional_str, optional_u64, required_str,
};
use super::syntax_check::guard_edit;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

// === Content-hash edit guards (#3979) ===

/// Format a file snapshot's content hash as `sha256:<hex>`.
///
/// The prefixed shape (rather than a bare hex digest) is deliberate: it is
/// self-describing in the transcript, and it makes an accidentally-truncated or
/// hand-invented value fail the equality check instead of matching by luck.
///
/// A file's hash is taken over its raw bytes, always before any windowing,
/// truncation, or rendering, so the value `read` reports is the value `write`,
/// `edit`, and `patch` verify.
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{}", crate::hashing::sha256_hex(bytes))
}

/// Hash a file's bytes without holding the whole file in memory.
///
/// The read path streams a bounded window out of large files on purpose, so it
/// never has the full contents to hash. Digesting through a separate streaming
/// pass keeps that memory bound while still producing a hash over the entire
/// file — the only value an edit guard can verify against.
fn hash_file_streaming(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;

    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!(
        "sha256:{}",
        crate::hashing::hex_bytes(hasher.finalize())
    ))
}

/// The one-line header that reports a snapshot hash to the model.
///
/// This goes in `ToolResult::content`, not in `ToolResult::metadata`, and that
/// placement is the whole point. `metadata` never reaches the model: the wire
/// `ContentBlock::ToolResult` (`crates/core/src/request.rs`) has no field for
/// it, and the turn loop builds the tool message from `output.content` alone
/// (`crates/tui/src/core/engine/turn_loop.rs`). Metadata is for the TUI,
/// telemetry, and the approval/mutation receipts. A hash the model cannot read
/// is a guard the model cannot use, so it is rendered into the content — either
/// as an attribute on the `<file …>` envelope, or as this header line for the
/// unwrapped small-file read.
fn content_hash_header(hash: &str) -> String {
    format!("content_hash=\"{hash}\"\n")
}

/// Reject a mutation whose `expected_hash` does not describe the current file.
///
/// Callers must run this against the exact snapshot the mutation would be
/// applied to, and before anything is written. `None` (parameter absent) keeps
/// the pre-#3979 behavior untouched — the guard is opt-in.
fn verify_expected_hash(
    expected: Option<&str>,
    current_bytes: &[u8],
    action: &str,
    path_str: &str,
) -> Result<(), ToolError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let actual = content_hash(current_bytes);
    if expected == actual {
        return Ok(());
    }
    Err(ToolError::execution_failed(format!(
        "File `{action}` refused: {path_str} changed since it was read. \
         expected_hash was {expected} but the file is now {actual}, so nothing was written. \
         Recovery: call File with action=\"read\" path=\"{path_str}\" to get the current contents \
         and its content_hash, then retry with the new hash."
    )))
}

/// Shared schema text for the optional guard parameter.
///
/// The hidden compatibility schema still has a byte budget, so this is only the
/// instruction the legacy caller needs: what to pass and what happens on a
/// mismatch. The rationale stays in doc comments rather than schema bytes.
pub(super) const EXPECTED_HASH_DESCRIPTION: &str = "The `content_hash` from a prior read; the write is refused and the file left unchanged if it changed since";

// === Cross-harness parameter aliases ===

/// Rewrite well-known parameter spellings from other coding harnesses onto the
/// names this tool actually implements.
///
/// Every mainstream harness names the same three file-edit arguments
/// differently — `old_string`/`new_string`, `old_str`/`new_str`,
/// `oldText`/`newText` — and models carry whichever spelling their training
/// saw most. CodeWhale's canonical `search`/`replace` is the odd one out, so a
/// model reaching for its prior used to burn a full turn on a rejection
/// (#5209) and then guess again. Translating an unambiguous synonym is
/// strictly better than refusing it: the edit the model asked for is the edit
/// that happens, and the schema still advertises exactly one canonical name so
/// there is no new ambiguity to learn.
///
/// This is deliberately *not* a silent-acceptance path. Only exact synonyms
/// are mapped, a synonym that disagrees with an explicitly supplied canonical
/// value is an error rather than a coin flip, and any parameter that is not a
/// known synonym still fails validation. The #5209 guarantee — no fabricated
/// "Replaced 1 occurrence" for an edit that never landed — is unchanged.
pub(super) struct ParamAlias {
    /// Spelling a model might emit.
    alias: &'static str,
    /// Parameter this tool implements.
    canonical: &'static str,
}

const fn alias(alias: &'static str, canonical: &'static str) -> ParamAlias {
    ParamAlias { alias, canonical }
}

/// Path spellings shared by every file action. `path` is CodeWhale's
/// canonical name and the most common one in the field, but `file_path` is
/// widespread enough in training data to be worth accepting everywhere.
pub(super) const PATH_ALIASES: &[ParamAlias] =
    &[alias("file_path", "path"), alias("filePath", "path")];

/// `input` with [`PATH_ALIASES`] folded onto `path`, exactly as the file
/// tools' `execute` does before touching disk.
///
/// Policy gates (typed file rules, the workspace-write carve-out, repo law,
/// Auto-Review) must judge the path the tool will act on. Reading only the
/// raw `path` key misses accepted `file_path`/`filePath` spellings. A
/// conflicting pair, which `execute` refuses, is returned unchanged.
pub(crate) fn with_canonical_path_argument(input: &Value) -> Cow<'_, Value> {
    if !PATH_ALIASES
        .iter()
        .any(|ParamAlias { alias, .. }| input.get(*alias).is_some())
    {
        return Cow::Borrowed(input);
    }
    let mut folded = input.clone();
    match apply_param_aliases(&mut folded, PATH_ALIASES, "path") {
        Ok(()) => Cow::Owned(folded),
        Err(_) => Cow::Borrowed(input),
    }
}

/// `path` and every spelling [`PATH_ALIASES`] folds onto it, for gates that
/// deliberately over-collect candidate targets.
pub(crate) fn path_argument_keys() -> impl Iterator<Item = &'static str> {
    std::iter::once("path").chain(PATH_ALIASES.iter().map(|ParamAlias { alias, .. }| *alias))
}

/// Edit-specific spellings. Ordered most- to least-common.
const EDIT_ALIASES: &[ParamAlias] = &[
    alias("old_string", "search"),
    alias("new_string", "replace"),
    alias("old_str", "search"),
    alias("new_str", "replace"),
    alias("oldText", "search"),
    alias("newText", "replace"),
    alias("old_text", "search"),
    alias("new_text", "replace"),
    alias("replacement", "replace"),
];

/// Read-window spellings. `offset`/`limit` and `line_offset`/`n_lines` both
/// name the same two numbers as CodeWhale's `start_line`/`max_lines` in widely
/// trained-on tool surfaces. A wrong guess here used to be ignored outright,
/// silently returning the head of the file instead of the window the model
/// asked for — a wrong answer shaped like a right one.
const READ_ALIASES: &[ParamAlias] = &[
    alias("offset", "start_line"),
    alias("line_offset", "start_line"),
    alias("limit", "max_lines"),
    alias("n_lines", "max_lines"),
    alias("num_lines", "max_lines"),
];

/// `search_name` spellings. The `File` wrapper advertises `max_results` for
/// both search actions, but only `search_content` implements that name; on
/// `search_name` the same number is spelled `limit`. Folding it here (rather
/// than copying it inside the wrapper) keeps one alias mechanism, so the
/// result-count cap a model asks for is the cap it gets whichever name it
/// reaches for, and a direct `file_search` call behaves the same way.
pub(super) const SEARCH_NAME_ALIASES: &[ParamAlias] = &[alias("max_results", "limit")];

/// `search_content` spellings, mirroring `SEARCH_NAME_ALIASES` in the other
/// direction: the wrapper advertises `query` and `limit` on the name-search
/// side, and a model that carries them across to a content search means
/// `pattern` and `max_results`.
pub(super) const SEARCH_CONTENT_ALIASES: &[ParamAlias] =
    &[alias("query", "pattern"), alias("limit", "max_results")];

/// Apply `aliases` to `input`, in place.
///
/// An alias is consumed only when the canonical key is absent. When both are
/// present and *equal* the alias is dropped as a harmless duplicate; when both
/// are present and disagree the call fails, because guessing which one the
/// model meant is exactly the fabrication this path exists to prevent.
pub(super) fn apply_param_aliases(
    input: &mut Value,
    aliases: &[ParamAlias],
    tool_label: &str,
) -> Result<(), ToolError> {
    let Some(obj) = input.as_object_mut() else {
        return Ok(());
    };

    for ParamAlias { alias, canonical } in aliases {
        let Some(alias_value) = obj.remove(*alias) else {
            continue;
        };
        match obj.get(*canonical) {
            None => {
                obj.insert((*canonical).to_string(), alias_value);
            }
            Some(existing) if existing == &alias_value => {}
            Some(_) => {
                return Err(ToolError::invalid_input(format!(
                    "{tool_label} received both `{canonical}` and its alias `{alias}` with different values, so the intended argument is ambiguous; nothing was changed. Pass only `{canonical}`."
                )));
            }
        }
    }

    Ok(())
}

// === Per-action parameter contracts ===

/// The parameter contract for one `File` action.
///
/// #5209 taught `edit` to refuse a parameter it does not implement instead of
/// dropping it and returning a success-shaped receipt. Only `edit` learned it.
/// Every other action kept silently discarding unknown keys, and for a reader
/// that is the same failure wearing a quieter costume: a misspelled
/// `start_line` on `read` is dropped, the head of the file comes back, and
/// nothing in the response says the requested window was never honored — a
/// wrong answer shaped like a right one.
///
/// One table, one error shape, every action.
pub(super) struct ActionParams {
    /// Action name as the model spells it on `File` (`read`, `write`, …).
    action: &'static str,
    /// Every parameter the action implements, canonical spellings only.
    /// Aliases are folded onto these by [`apply_param_aliases`] before
    /// validation runs, so they must not be listed here.
    allowed: &'static [&'static str],
    /// Parameters the action cannot run without.
    required: &'static [&'static str],
    /// `true` when exactly one of `required` is needed rather than all of
    /// them — `patch` accepts `patch`, `replace`, or `changes`.
    required_is_choice: bool,
}

const fn params(
    action: &'static str,
    allowed: &'static [&'static str],
    required: &'static [&'static str],
) -> ActionParams {
    ActionParams {
        action,
        allowed,
        required,
        required_is_choice: false,
    }
}

pub(super) const READ_PARAMS: ActionParams = params(
    "read",
    &["path", "start_line", "max_lines", "pages"],
    &["path"],
);

pub(super) const WRITE_PARAMS: ActionParams = params(
    "write",
    &["path", "content", "expected_hash"],
    &["path", "content"],
);

pub(super) const EDIT_PARAMS: ActionParams = params(
    "edit",
    &["path", "search", "replace", "expected_hash"],
    &["path", "search", "replace"],
);

pub(super) const LIST_PARAMS: ActionParams = params("list", &["path"], &[]);

pub(super) const SEARCH_NAME_PARAMS: ActionParams = params(
    "search_name",
    &["query", "path", "limit", "extensions", "exclude"],
    &["query"],
);

pub(super) const SEARCH_CONTENT_PARAMS: ActionParams = params(
    "search_content",
    &[
        "pattern",
        "path",
        "include",
        "exclude",
        "context_lines",
        "case_insensitive",
        "max_results",
    ],
    &["pattern"],
);

pub(super) const PATCH_PARAMS: ActionParams = ActionParams {
    action: "patch",
    allowed: &[
        "path",
        "patch",
        "replace",
        "changes",
        "fuzz",
        "create_if_missing",
        "expected_hash",
    ],
    required: &["patch", "replace", "changes"],
    required_is_choice: true,
};

/// Render `names` as a backticked, comma-separated English list.
fn quoted_list(names: &[&str], conjunction: &str) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
    match quoted.as_slice() {
        [] => "none".to_string(),
        [only] => only.clone(),
        [first, second] => format!("{first} {conjunction} {second}"),
        [head @ .., last] => format!("{}, {conjunction} {last}", head.join(", ")),
    }
}

impl ActionParams {
    /// Reject parameter names this action does not implement.
    ///
    /// Must run *after* [`apply_param_aliases`], exactly as the `edit` path
    /// does. The alias lane's reasoning stands: translating an unambiguous
    /// synonym is better than refusing it, so by the time this runs every
    /// spelling with a known meaning has already been folded onto its
    /// canonical name. What is left is a name with no known meaning, where
    /// continuing would mean guessing which argument was intended — so it
    /// hard-errors rather than dropping the argument and reporting success.
    pub(super) fn reject_unknown(&self, input: &Value) -> Result<(), ToolError> {
        let action = self.action;
        let required = if self.required_is_choice {
            format!("one of {}", quoted_list(self.required, "or"))
        } else {
            quoted_list(self.required, "and")
        };

        let Some(obj) = input.as_object() else {
            return Err(ToolError::invalid_input(format!(
                "File {action} input must be an object. Allowed parameters are {}. Required: {required}. The {action} was not performed.",
                quoted_list(self.allowed, "and"),
            )));
        };

        let unexpected: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|key| !self.allowed.contains(key))
            .collect();
        if !unexpected.is_empty() {
            return Err(ToolError::invalid_input(format!(
                "unexpected File {action} parameter(s): {}. Allowed parameters are {}. Required: {required}. The {action} was not performed.",
                unexpected.join(", "),
                quoted_list(self.allowed, "and"),
            )));
        }

        Ok(())
    }

    /// A required parameter that is not also allowed would make the refusal
    /// self-contradicting: it would name an argument the same check rejects.
    #[cfg(test)]
    pub(super) fn assert_required_is_allowed(&self) {
        for name in self.required {
            assert!(
                self.allowed.contains(name),
                "File {} requires `{name}` but does not allow it",
                self.action
            );
        }
    }
}

// === ReadFileTool ===

fn canonical_path_for_credential_guard(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn config_backup_path_for_credential_guard(config_path: &Path) -> PathBuf {
    let mut file_name = config_path
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_else(|| std::ffi::OsString::from(codewhale_config::CONFIG_FILE_NAME));
    file_name.push(".bak");
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(file_name)
}

fn is_config_or_backup(candidate: &Path, config_path: &Path) -> bool {
    let config_path = canonical_path_for_credential_guard(config_path);
    let backup_path =
        canonical_path_for_credential_guard(&config_backup_path_for_credential_guard(&config_path));
    candidate == config_path || candidate == backup_path
}

/// Resolve a model-supplied path for an in-process read, applying every read
/// guard in the one safe order: the deny-list on the caller's raw spelling
/// (so a denial never names a symlink target), then `resolve_path`, then the
/// credential-store check and the deny-list again on the resolved path.
///
/// Every tool that reads a file's content in-process and hands it (or a
/// derivative) to the model goes through this.
pub(crate) fn resolve_guarded_read_path(
    context: &ToolContext,
    raw: &str,
    tool: &str,
) -> Result<PathBuf, ToolError> {
    enforce_read_denylist(Path::new(raw), tool)?;
    let path = context.resolve_path(raw)?;
    if is_codewhale_credential_path(&path) {
        return Err(ToolError::permission_denied(format!(
            "{tool} cannot expose Codewhale configuration or credential-store files; use `codewhale config list` or `codewhale auth status` for safe inspection"
        )));
    }
    enforce_read_denylist(&path, tool)?;
    Ok(path)
}

/// Workspace-escape fallback for the read-only image tools (`read`,
/// `read_media`): admit `raw` only when it is exactly an image the user
/// attached in this session's prompts. See
/// [`crate::image_attach::resolve_user_attached_image`] for the admission
/// rule; the credential and deny-list guards still apply here. `Ok(None)`
/// means "not admitted" and the caller keeps its original refusal.
///
/// Known limit: a path the user mentioned only in prose, never attached, is
/// not admitted; `/trust add` remains the way to open a directory.
pub(crate) async fn user_attached_image_read_path(
    context: &ToolContext,
    raw: &str,
    tool: &str,
) -> Result<Option<PathBuf>, ToolError> {
    let Some(snapshot) = context.session_objects.as_ref() else {
        return Ok(None);
    };
    let references = crate::image_attach::user_attached_image_references(&snapshot.messages);
    if references.is_empty() {
        return Ok(None);
    }
    let raw_owned = raw.to_string();
    let admitted = tokio::task::spawn_blocking(move || {
        crate::image_attach::resolve_user_attached_image(&references, &raw_owned)
    })
    .await
    .map_err(|error| ToolError::execution_failed(format!("Failed to resolve {raw}: {error}")))?;
    let Some(path) = admitted else {
        return Ok(None);
    };
    if is_codewhale_credential_path(&path) {
        return Err(ToolError::permission_denied(format!(
            "{tool} cannot expose Codewhale configuration or credential-store files; use `codewhale config list` or `codewhale auth status` for safe inspection"
        )));
    }
    enforce_read_denylist(&path, tool)?;
    Ok(Some(path))
}

/// Refuse a read the sandbox read deny-list blocks (S1).
///
/// `read_file`, `read`, and `read_media` all run *in-process*: they call
/// `std::fs` inside the harness, so `sandbox-exec` and `bwrap` never see them
/// and the OS-level deny rules do not apply. This is the enforcement point for
/// those tools, and the refusal is always an explicit error — never an empty
/// result, which would read as "the file is empty" and invite the model to
/// probe siblings.
pub(crate) fn enforce_read_denylist(path: &Path, tool: &str) -> Result<(), ToolError> {
    // Expand the user's home before authorization, retaining the spelling they
    // supplied in every denial. This shares the file tools' path resolution;
    // expansion grants no additional access and never exposes a symlink target.
    let home_path = path
        .to_str()
        .map(super::spec::resolve_home_path)
        .transpose()?
        .flatten();
    if home_path
        .as_deref()
        .is_some_and(is_codewhale_credential_path)
    {
        return Err(ToolError::permission_denied(format!(
            "{tool} cannot expose Codewhale configuration or credential-store files; use `codewhale config list` or `codewhale auth status` for safe inspection"
        )));
    }
    match crate::sandbox::read_guard::active().check(home_path.as_deref().unwrap_or(path)) {
        Ok(()) => Ok(()),
        Err(mut denial) => {
            denial.requested = path.to_path_buf();
            let message = denial.message(tool);
            tracing::warn!(
                target: "codewhale::sandbox::read_guard",
                requested = %denial.requested.display(),
                via_symlink = denial.via_symlink,
                tool = tool,
                "sandbox read deny-list refused a read"
            );
            Err(ToolError::permission_denied(message))
        }
    }
}

/// Return whether `read_file` must refuse a CodeWhale-owned credential file.
///
/// This is deliberately scoped to the active config, the two conventional
/// config locations (including one-time backups), and CodeWhale's file-backed
/// secret-store directories. Other dotfiles remain readable. Model-bound
/// redaction is still required because shell tools can read these files and
/// arbitrary commands can print credentials without reading a file at all.
pub(crate) fn is_codewhale_credential_path(path: &Path) -> bool {
    let candidate = canonical_path_for_credential_guard(path);

    let active_config = codewhale_config::resolve_config_path(None).ok();
    if let Some(active_config) = &active_config
        && is_config_or_backup(&candidate, active_config)
    {
        return true;
    }

    // `CODEWHALE_HOME` relocates the *runtime* home; it is not a licence to read
    // the user's real `~/.codewhale/config.toml`. `codewhale_home()` returns the
    // override when one is set, so relying on it alone left the ambient store
    // unguarded whenever that variable pointed elsewhere. Keep the ambient root
    // in the set alongside the override, mirroring the deliberately
    // unconditional `~/.codewhale/secrets` entry in `sandbox::read_guard`
    // (read_guard.rs:481-487). `legacy_deepseek_home()` is already ambient by
    // construction (paths/src/lib.rs:183-185), so it needs no counterpart.
    let mut roots: Vec<PathBuf> = Vec::with_capacity(3);
    roots.extend(codewhale_config::codewhale_home().ok());
    roots.extend(codewhale_config::legacy_deepseek_home().ok());
    // Without `CODEWHALE_HOME`, `codewhale_home()` already returned this exact
    // ambient root, so pushing it again only re-canonicalizes identical
    // config/backup/secrets paths. Keep it only when it adds a distinct root.
    if let Some(ambient) =
        codewhale_paths::user_home().map(|home| home.join(codewhale_config::CODEWHALE_APP_DIR))
        && !roots.contains(&ambient)
    {
        roots.push(ambient);
    }
    for root in roots {
        let config_path = root.join(codewhale_config::CONFIG_FILE_NAME);
        // Only skip the exact config/backup pair already checked above.
        // Distinct spellings still canonicalize; every secrets root is checked.
        if active_config
            .as_ref()
            .is_none_or(|active| active.as_os_str() != config_path.as_os_str())
            && is_config_or_backup(&candidate, &config_path)
        {
            return true;
        }

        let secrets_dir = canonical_path_for_credential_guard(&root.join("secrets"));
        if candidate.starts_with(secrets_dir) {
            return true;
        }
    }

    false
}

// === small-contract-compatible primitive implementation helpers ===

/// Default model-visible byte budget for one `read` call.
///
/// Bytes are the *only* default bound: there is no line cap, so an ordinary
/// source or prose file comes back whole in one call instead of being paged
/// at some arbitrary line count with most of the budget unspent.
const READ_DEFAULT_MAX_BYTES: usize = 100_000;
/// Hard ceiling on a budget the *model* asks for with `max_bytes`. A larger
/// request clamps down to this; it is never an error.
const READ_REQUEST_MAX_BYTES: usize = 500_000;
/// Outer bound on the operator's process-wide `[workshop] read_result_max_bytes`
/// override, and therefore on any read result.
const READ_RESULT_ABSOLUTE_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Whole-source processing bound, independent of the model-visible read budget.
/// The lowercase primitives accept only regular, single-link files. Hidden
/// compatibility readers and PDF extraction keep their existing contracts.
const CONTRACT_FILE_MAX_BYTES: usize = 16 * 1024 * 1024;

fn check_contract_file_size(size: usize) -> Result<(), ToolError> {
    if size > CONTRACT_FILE_MAX_BYTES {
        return Err(ToolError::execution_failed(
            "File content exceeds the supported 16 MiB processing cap. Split or export a smaller file, or use an appropriate separately authorized tool.",
        ));
    }
    Ok(())
}

fn check_contract_cancelled(token: Option<&CancellationToken>) -> Result<(), ToolError> {
    if token.is_some_and(CancellationToken::is_cancelled) {
        return Err(ToolError::cancelled("Operation aborted"));
    }
    Ok(())
}

/// Read at most cap+1 actual bytes, including files that grow after metadata.
/// Cancellation is polled between bounded reads; it cannot interrupt an OS
/// syscall already in progress. This worker never mutates the file.
pub(super) fn read_contract_source(
    reader: &mut impl std::io::Read,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<u8>, ToolError> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        check_contract_cancelled(cancel)?;
        let remaining = CONTRACT_FILE_MAX_BYTES + 1 - bytes.len();
        let read_len = remaining.min(chunk.len());
        let count = match reader.read(&mut chunk[..read_len]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(ToolError::execution_failed(format!(
                    "Failed to read file: {error}"
                )));
            }
        };
        check_contract_cancelled(cancel)?;
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..count]);
        check_contract_file_size(bytes.len())?;
    }
}

async fn load_contract_source(
    path: &Path,
    writable: bool,
    context: &ToolContext,
) -> Result<Option<Vec<u8>>, ToolError> {
    check_file_operation_cancelled(context)?;
    let path = path.to_path_buf();
    let cancel = context.cancel_token.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        check_contract_cancelled(cancel.as_ref())?;
        let open_error = |error| {
            if writable {
                ToolError::execution_failed(format!(
                    "Could not edit file {}: target must be readable and writable ({error})",
                    path.display()
                ))
            } else {
                ToolError::execution_failed(format!("Failed to read {}: {error}", path.display()))
            }
        };
        let Some(mut file) = crate::plugins::registry::open_existing_regular_file(&path, writable)
            .map_err(open_error)?
        else {
            return Ok(None);
        };
        let metadata = file
            .metadata()
            .map_err(|error| open_error(error.to_string()))?;
        if metadata.len() > CONTRACT_FILE_MAX_BYTES as u64 {
            check_contract_file_size(CONTRACT_FILE_MAX_BYTES + 1)?;
        }
        read_contract_source(&mut file, cancel.as_ref()).map(Some)
    });
    let result = if let Some(cancel) = context.cancel_token.as_ref() {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ToolError::cancelled("Operation aborted")),
            result = &mut worker => result,
        }
    } else {
        worker.await
    };
    result.map_err(|error| ToolError::execution_failed(format!("File read task: {error}")))?
}

/// Resolve the byte budget for one `read` call from the three layers that can
/// set it, highest wins:
///
/// 1. **The model's own request** — `max_bytes` on this call, clamped to
///    [`READ_REQUEST_MAX_BYTES`] (500 000).
/// 2. **The operator's process-wide override** — `[workshop]
///    read_result_max_bytes`, clamped into
///    `[READ_DEFAULT_MAX_BYTES, READ_RESULT_ABSOLUTE_MAX_BYTES]` (2 MiB).
/// 3. **The default** — [`READ_DEFAULT_MAX_BYTES`] (100 000).
///
/// The result is `max(1, 2-or-3)`. Both raising layers can only raise: a model
/// request never shrinks a budget the operator widened, and an operator who
/// widened it process-wide keeps that floor when the model asks for less.
fn effective_read_max_bytes(requested: Option<usize>) -> usize {
    let baseline =
        crate::tools::large_output_router::WorkshopConfig::active_read_result_max_bytes()
            .map_or(READ_DEFAULT_MAX_BYTES, |configured| {
                configured.clamp(READ_DEFAULT_MAX_BYTES, READ_RESULT_ABSOLUTE_MAX_BYTES)
            });
    match requested {
        Some(requested) => baseline.max(requested.min(READ_REQUEST_MAX_BYTES)),
        None => baseline,
    }
}

type FileMutationMutex = AsyncMutex<()>;

/// File primitives can also be invoked outside the native engine's global
/// execution lock (for example by an embedded host). Keep writes to one path
/// ordered in those hosts without exposing any locking ceremony in the tool
/// schema or result.
fn file_mutation_lock(path: &Path) -> Result<Arc<FileMutationMutex>, ToolError> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<FileMutationMutex>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().map_err(|_| {
        ToolError::execution_failed(
            "file mutation queue is unavailable because its lock was poisoned",
        )
    })?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    Ok(lock)
}

async fn acquire_file_mutation(
    path: &Path,
    context: &ToolContext,
) -> Result<OwnedMutexGuard<()>, ToolError> {
    let lock = file_mutation_lock(path)?;
    if let Some(cancel) = context.cancel_token.as_ref() {
        tokio::select! {
            guard = lock.lock_owned() => Ok(guard),
            () = cancel.cancelled() => Err(ToolError::cancelled("Operation aborted")),
        }
    } else {
        Ok(lock.lock_owned().await)
    }
}

/// Atomic workspace write on the blocking pool: temp create plus fsync plus
/// rename (and a retry loop on Windows) must not park a Tokio worker
/// (blocking-call convention, #6149). Error shape matches the historical
/// inline call.
async fn run_blocking_write_atomic(path: &Path, contents: Vec<u8>) -> Result<(), ToolError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::utils::write_atomic_workspace(&path, &contents).map_err(|e| {
            ToolError::execution_failed(format!("Failed to write {}: {e}", path.display()))
        })
    })
    .await
    .map_err(|e| ToolError::execution_failed(format!("File write task: {e}")))??;
    Ok(())
}

fn check_file_operation_cancelled(context: &ToolContext) -> Result<(), ToolError> {
    check_contract_cancelled(context.cancel_token.as_ref())
}

/// One `mutation.files[]` receipt entry. `size` and `sha256` describe the
/// exact bytes this call wrote, recorded where they were written so a turn's
/// artifact reference never has to re-read (and race) the disk. `sha256` is
/// plain lowercase hex: the same value `GET /v1/workspace/files/read` reports
/// as `revision`.
pub(crate) fn mutation_file_entry(path: &str, outcome: &str, written: Option<&[u8]>) -> Value {
    let mut entry = json!({ "path": path, "outcome": outcome });
    if let Some(bytes) = written {
        entry["size"] = json!(bytes.len());
        entry["sha256"] = json!(crate::hashing::sha256_hex(bytes));
    }
    entry
}

#[allow(clippy::too_many_arguments)]
async fn contract_mutation_result(
    context: &ToolContext,
    file_path: &Path,
    requested_path: &str,
    before: &str,
    after: &str,
    written: &[u8],
    outcome: &str,
    summary: String,
) -> ToolResult {
    let paths = [file_path.to_path_buf()];
    let diagnostics = lsp_diagnostics_for_paths(context, &paths).await;
    // Echolocation: the model-facing receipt is one line while the diff
    // rides metadata for the TUI, so the echo tells the model what its
    // edit hit (touched symbols) and what hears it (impacted callers).
    // Deterministic, hard-budgeted, None when there is nothing to say.
    let mut summary = summary;
    let workspace = context.workspace.clone();
    let echo_path = file_path.to_path_buf();
    let echo_before = before.to_string();
    let echo_after = after.to_string();
    match tokio::task::spawn_blocking(move || {
        crate::tools::echolocation::sound_edit_echo(
            &workspace,
            &echo_path,
            &echo_before,
            &echo_after,
        )
    })
    .await
    {
        Ok(Some(echo)) => {
            summary.push_str("\n\n");
            summary.push_str(&echo);
        }
        Ok(None) => {}
        Err(error) => summary.push_str(&format!("\n\nEdit echo unavailable: {error}")),
    }
    ToolResult::success(summary).with_metadata(json!({
        "event": "file.mutation",
        "lsp_diagnostics": diagnostics,
        "mutation": {
            "diff": make_unified_diff(requested_path, before, after),
            "files": [mutation_file_entry(requested_path, outcome, Some(written))],
            "renames": []
        }
    }))
}

fn reject_primitive_unknown(input: &Value, tool: &str, allowed: &[&str]) -> Result<(), ToolError> {
    let object = input
        .as_object()
        .ok_or_else(|| ToolError::invalid_input(format!("{tool} input must be an object")))?;
    let unexpected = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if unexpected.is_empty() {
        return Ok(());
    }
    Err(ToolError::invalid_input(format!(
        "unexpected {tool} parameter(s): {}",
        unexpected.join(", ")
    )))
}

fn contract_nonnegative_int(input: &Value, key: &str) -> Result<Option<usize>, ToolError> {
    let Some(value) = input.get(key) else {
        return Ok(None);
    };
    let number = codewhale_tools::json_nonnegative_integer(value)
        .ok_or_else(|| ToolError::invalid_input(format!("{key} must be a non-negative integer")))?;
    usize::try_from(number)
        .map(Some)
        .map_err(|_| ToolError::invalid_input(format!("{key} exceeds platform range")))
}

fn primitive_image_mime(bytes: &[u8]) -> Option<&'static str> {
    crate::image_attach::sniff_media_type(bytes)
        .or_else(|| bytes.starts_with(b"BM").then_some("image/bmp"))
}

fn contract_format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[derive(Debug)]
struct ContractReadWindow {
    content: String,
    shown_lines: usize,
    truncated: bool,
    first_line_too_large: bool,
}

/// Retain only complete lines from the head, stopping at `max_bytes`. A
/// terminal newline is content but does not add a phantom line to the
/// truncation counter.
///
/// The byte budget is the single bound. There is no line cap to fragment a
/// file that fits: every retained line costs at least its own newline, so
/// `max_bytes` already bounds the line count as well.
fn contract_read_window(content: &str, max_bytes: usize) -> ContractReadWindow {
    if content
        .split('\n')
        .next()
        .is_some_and(|line| line.len() > max_bytes)
    {
        return ContractReadWindow {
            content: String::new(),
            shown_lines: 0,
            truncated: true,
            first_line_too_large: true,
        };
    }
    if content.len() <= max_bytes {
        return ContractReadWindow {
            content: content.to_string(),
            shown_lines: content.split_terminator('\n').count(),
            truncated: false,
            first_line_too_large: false,
        };
    }
    let mut shown_lines = 0;
    let mut bytes = 0usize;
    for line in content.split_terminator('\n') {
        let next = line.len() + usize::from(shown_lines > 0);
        if bytes.saturating_add(next) > max_bytes {
            break;
        }
        shown_lines += 1;
        bytes += next;
    }
    ContractReadWindow {
        content: content[..bytes].to_string(),
        shown_lines,
        truncated: true,
        first_line_too_large: false,
    }
}

/// Tool for reading UTF-8 files from the workspace.
pub struct ReadFileTool;

impl ReadFileTool {
    /// Execute the lowercase `read` primitive without leaking the hidden
    /// Codewhale hash/snapshot protocol into its small-contract-shaped model contract.
    pub(super) async fn execute_contract_read(
        input: Value,
        context: &ToolContext,
    ) -> Result<RichToolResult, ToolError> {
        reject_primitive_unknown(
            &input,
            "read",
            &["path", "offset", "limit", "max_bytes", "expected_revision"],
        )?;
        let path_str = required_str(&input, "path")?;
        let expected_revision = optional_str(&input, "expected_revision")?;
        if let Some(revision) = expected_revision
            && !crate::tools::echolocation::valid_source_revision(revision)
        {
            return Err(ToolError::invalid_input(
                "expected_revision must be sha256: followed by 64 lowercase hexadecimal digits",
            ));
        }
        let offset = contract_nonnegative_int(&input, "offset")?;
        let limit = contract_nonnegative_int(&input, "limit")?;
        let max_bytes = effective_read_max_bytes(contract_nonnegative_int(&input, "max_bytes")?);
        // S1/F2: check the caller's own spelling BEFORE `resolve_path`
        // canonicalizes it. A workspace symlink `notes.txt` -> a denied vault
        // file resolves to the secret's absolute location, and a denial raised
        // only on the resolved path would name that location in the error —
        // answering the very question ("where is the secret?") the read was
        // probing for. `read_guard::check` canonicalizes internally, so the
        // raw spelling still matches by its target; the resolved check after
        // `resolve_path` stays as defense in depth for callers whose process
        // cwd is not the workspace.
        let file_path = match resolve_guarded_read_path(context, path_str, "read") {
            Ok(path) => path,
            Err(error @ ToolError::PathEscape { .. }) => {
                user_attached_image_read_path(context, path_str, "read")
                    .await?
                    .ok_or(error)?
            }
            Err(error) => return Err(error),
        };
        check_file_operation_cancelled(context)?;
        let bytes = match load_contract_source(&file_path, false, context).await? {
            Some(bytes) => bytes,
            None => {
                let echo_path = file_path.clone();
                let echo = tokio::task::spawn_blocking(move || {
                    super::echolocation::sound_miss_echo(&echo_path)
                })
                .await
                .map_err(|error| ToolError::execution_failed(format!("Miss echo task: {error}")))?;
                return Err(ToolError::execution_failed(format!(
                    "Failed to read {}: file not found{}",
                    file_path.display(),
                    echo.unwrap_or_default()
                )));
            }
        };
        // #6283: every read response carries the file's byte size, line
        // count, and truncation flag so the caller can page deliberately
        // instead of discovering a huge file one window at a time.
        let size_bytes = bytes.len();
        check_file_operation_cancelled(context)?;
        if let Some(mime_type) = primitive_image_mime(&bytes) {
            if expected_revision.is_some() {
                return Err(ToolError::execution_failed(
                    "Navigation reference is stale: the target is now an image. Read the path again without the old range/reference.",
                ));
            }
            // Decoding and any downscale are CPU-bound; keep them off the
            // runtime worker (#6149).
            let prepared = tokio::task::spawn_blocking(move || {
                crate::image_attach::prepare_tool_image_bytes(&bytes, mime_type)
            })
            .await
            .map_err(|error| {
                ToolError::execution_failed(format!("Failed to prepare image: {error}"))
            })?;
            context.note_file_read(&file_path);
            return Ok(RichToolResult::with_content_blocks(
                ToolResult::success(prepared.note).with_metadata(json!({
                    "evidence_routing": "inline"
                })),
                prepared.block.into_iter().collect(),
            ));
        }

        // The small-contract reader decodes non-image buffers as UTF-8 text with replacement
        // characters instead of refusing the whole read on one invalid byte.
        let workspace = context.workspace.clone();
        let observation_path = file_path.clone();
        let guarded = expected_revision.is_some();
        let (text, source_revision, utf8_valid, cache_identity) =
            tokio::task::spawn_blocking(move || {
                let cache_identity = crate::tools::echolocation::navigation_cache_identity(
                    &workspace,
                    &observation_path,
                );
                let revision = (cache_identity.is_some() || guarded).then(|| content_hash(&bytes));
                let (text, utf8_valid) = match String::from_utf8(bytes) {
                    Ok(text) => (text, true),
                    Err(error) => (
                        String::from_utf8_lossy(error.as_bytes()).into_owned(),
                        false,
                    ),
                };
                (text, revision, utf8_valid, cache_identity)
            })
            .await
            .map_err(|error| {
                ToolError::execution_failed(format!("Failed to decode source: {error}"))
            })?;
        check_file_operation_cancelled(context)?;
        if let Some(expected) = expected_revision
            && source_revision.as_deref() != Some(expected)
        {
            return Err(ToolError::execution_failed(format!(
                "Navigation reference for {path_str} is stale. Read the path without the old range/expected_revision to obtain a new reference; do not reuse its offsets."
            )));
        }
        // Count and select with byte boundaries, without a pointer per newline.
        let line_count = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let requested_offset = offset.unwrap_or(1);
        let start = requested_offset.saturating_sub(1);
        if start >= line_count {
            return Err(ToolError::execution_failed(format!(
                "Offset {requested_offset} is beyond end of file ({line_count} lines total)"
            )));
        }
        let selected_lines = limit.unwrap_or(usize::MAX).min(line_count - start);
        // An empty selection slices `""` whatever the range start is, so it
        // needs a valid boundary, not the walked one: skip the prefix scan.
        let start_byte = if start == 0 || selected_lines == 0 {
            0
        } else {
            text.match_indices('\n')
                .nth(start - 1)
                .expect("line count validated offset")
                .0
                + 1
        };
        let end_byte = if selected_lines == 0 {
            start_byte
        } else {
            text[start_byte..]
                .match_indices('\n')
                .nth(selected_lines - 1)
                .map_or(text.len(), |(index, _)| start_byte + index)
        };
        let selected_content = &text[start_byte..end_byte];
        let window = contract_read_window(selected_content, max_bytes);
        // Truncated means the file holds more than this response shows:
        // either the byte budget cut the window, or a bounded range stopped
        // before EOF. A whole file that fits is never truncated.
        let truncated = window.truncated || limit.is_some() && start + selected_lines < line_count;
        let first_display = start + 1;
        let mut output = if window.first_line_too_large {
            let size = selected_content.split('\n').next().map_or(0, str::len);
            format!(
                "[Line {first_display} is {}, exceeds the {max_bytes}-byte output budget for this call. Use bash: sed -n '{first_display}p' {path_str} | head -c {max_bytes}]",
                contract_format_size(size)
            )
        } else {
            window.content
        };

        if !window.first_line_too_large && window.truncated {
            let last_display = first_display + window.shown_lines.saturating_sub(1);
            let next_offset = last_display + 1;
            // Continuation must be exact: name the next offset, and when the
            // caller asked for a bounded range, the part of that range still
            // unread. `max_bytes` is only offered while it can still go up.
            let mut hint = format!("offset={next_offset}");
            if let Some(limit) = limit {
                let remaining = limit.saturating_sub(window.shown_lines);
                if remaining > 0 {
                    hint.push_str(&format!(" limit={remaining}"));
                }
            }
            let raise = if max_bytes < READ_REQUEST_MAX_BYTES {
                format!(", or max_bytes up to {READ_REQUEST_MAX_BYTES} to read more per call")
            } else {
                String::new()
            };
            output.push_str(&format!(
                "\n\n[Showing lines {first_display}-{last_display} of {} ({} total, {max_bytes}-byte output budget). Use {hint} to continue{raise}.]",
                line_count,
                contract_format_size(size_bytes)
            ));
        } else if limit.is_some() {
            let consumed = selected_lines;
            if start + consumed < line_count {
                let remaining = line_count - (start + consumed);
                let next_offset = start + consumed + 1;
                output.push_str(&format!(
                    "\n\n[{remaining} more lines in file ({} total). Use offset={next_offset} to continue.]",
                    contract_format_size(size_bytes)
                ));
            }
        }

        // Echolocation: stateless kin footer sounded fresh from source
        // (no index, no new deps). Deterministic and hard-budgeted, so a
        // repeated read is byte-identical. Two distinct files read in
        // this pod put the next read there into the terminal buzz
        // (callers included).
        let buzz = file_path.parent().is_some_and(|parent| {
            context.pod_visit_count(parent) >= crate::tools::echolocation::BUZZ_VISIT_THRESHOLD
        });
        let cached_navigation = if let Some((namespace, path)) = &cache_identity {
            context
                .runtime
                .code_observations
                .lock()
                .await
                .get_json_named(namespace, path)
                .cloned()
                .and_then(|value| {
                    serde_json::from_value::<crate::tools::echolocation::SourceNavigation>(value)
                        .ok()
                })
                .filter(|navigation| {
                    navigation.matches(path, source_revision.as_deref().unwrap_or_default())
                })
        } else {
            None
        };
        let workspace = context.workspace.clone();
        let echo_path = file_path.clone();
        let navigation_path = cache_identity.as_ref().map(|(_, path)| path.clone());
        let (echo, navigation) = tokio::task::spawn_blocking(move || {
            let navigation = cached_navigation.or_else(|| {
                navigation_path
                    .zip(source_revision)
                    .map(|(path, revision)| {
                        crate::tools::echolocation::SourceNavigation::from_source(
                            path, revision, &text, utf8_valid,
                        )
                    })
            });
            let echo = crate::tools::echolocation::sound_echolocation(
                &workspace,
                &echo_path,
                &text,
                buzz,
                navigation.as_ref(),
            )
            .render_with_navigation(navigation.as_ref());
            (echo, navigation)
        })
        .await
        .map_err(|error| {
            ToolError::execution_failed(format!("read echo worker failed: {error}"))
        })?;
        check_file_operation_cancelled(context)?;
        output.push_str(&echo);
        if let (Some((namespace, path)), Some(navigation)) = (&cache_identity, &navigation) {
            let _ = context.runtime.code_observations.lock().await.insert_json(
                namespace,
                path,
                serde_json::to_value(navigation)
                    .map_err(|error| ToolError::execution_failed(error.to_string()))?,
            );
        }

        // Keep hidden legacy edit replay working. Ordinary reads remain
        // unguarded; the optional navigation revision pins a selected range,
        // not a read-before-edit permission ceremony.
        context.note_file_read(&file_path);
        let mut metadata = json!({
            "evidence_routing": "inline",
            // The budget this call actually enforced. The context
            // compactor honors it so an already-bounded read is never
            // truncated a second time on its way into the conversation.
            "read_budget_bytes": max_bytes,
            // #6283: paging contract. `size` is the whole file in bytes,
            // `line_count` its total lines, `truncated` whether the file
            // holds more than this response shows.
            "size": size_bytes,
            "truncated": truncated,
            "line_count": line_count
        });
        if let Some(navigation) = navigation {
            metadata["navigation"] = serde_json::to_value(navigation)
                .map_err(|error| ToolError::execution_failed(error.to_string()))?;
        }
        Ok(RichToolResult::plain(
            ToolResult::success(output).with_metadata(metadata),
        ))
    }
}

#[async_trait]
impl ToolSpec for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 file from the workspace. Compared with `cat`, `head`, `tail`, or `sed -n '..p'` in `Bash`, it is faster, sandbox-aware, and needs no approval prompt. Plain text is returned as-is and records the file snapshot required before `edit` will make a narrow in-place edit. Text reads report the whole file's `content_hash=\"sha256:…\"`; pass that value back as `expected_hash` on a later `write`, `edit`, or `patch` to have the write refused if the file changed in between. Codewhale config files and file-backed credential stores cannot be read with this tool; use `codewhale config list` or `codewhale auth status` for safe inspection. PDFs are text-extracted when the optional `pdftotext` executable (Poppler) is installed. Image screenshots are OCR-extracted when local OCR is available. Cannot read other non-PDF binaries.\n\nFor large files, use `start_line` and `max_lines` to read in chunks. By default, returns up to 500 lines or 16KB, whichever comes first. If `truncated=\"true\"` and `next_start_line` is present, continue reading from there; a byte-limited window instead shows head + tail with a `[CONTENT TRUNCATED]` marker and its note says how to narrow the range. For PDFs, use `pages` instead — `start_line`/`max_lines` only apply to text files."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file (relative to workspace, absolute, or ~/ home-relative). Alias: `file_path`"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Starting line (1-based, default 1). Aliases: `offset`, `line_offset`"
                },
                "max_lines": {
                    "type": "integer",
                    "description": "Maximum lines to return (default 500, max 500; a 16KB byte budget applies regardless). Aliases: `limit`, `n_lines`"
                },
                "pages": {
                    "type": "string",
                    "description": "PDF only: page range to extract, e.g. \"1-5\" or \"10\". Ignored for non-PDF files."
                }
            },
            "required": ["path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let mut input = input;
        apply_param_aliases(&mut input, PATH_ALIASES, "File read")?;
        apply_param_aliases(&mut input, READ_ALIASES, "File read")?;
        READ_PARAMS.reject_unknown(&input)?;

        let path_str = required_str(&input, "path")?;
        // S1/F2: raw spelling first, resolved path after — see the matching
        // comment in `execute_contract_read`. Only the raw-spelling denial can
        // promise an error that never names the symlink target's location.
        let file_path = resolve_guarded_read_path(context, path_str, "read_file")?;
        let pages = optional_str(&input, "pages")?;

        if let Some(result) = read_pdf_if_detected(
            &file_path,
            pages,
            super::pdf::PdfTextCommand::system(Some(context)),
        )
        .await?
        {
            return Ok(result);
        }
        if is_image_for_ocr(&file_path) {
            return read_image_via_ocr(&file_path, path_str, context).await;
        }

        // Open before parameter parsing so a missing file keeps the
        // historical "Failed to read …" error shape regardless of the other
        // arguments. The open and size probe run on the blocking pool —
        // tool handlers execute on the Tokio runtime (blocking-call
        // convention, #6149).
        let file_bytes = tokio::task::spawn_blocking({
            let file_path = file_path.clone();
            move || {
                let file = fs::File::open(&file_path).map_err(|e| {
                    ToolError::execution_failed(format!(
                        "Failed to read {}: {}",
                        file_path.display(),
                        e
                    ))
                })?;
                Ok::<_, ToolError>(file.metadata().map(|meta| meta.len()).unwrap_or(u64::MAX))
            }
        })
        .await
        .map_err(|e| ToolError::execution_failed(format!("File open task: {e}")))??;

        let explicit_range = input
            .get("start_line")
            .or_else(|| input.get("max_lines"))
            .is_some();

        // Small-file fast path. Only applies when the caller didn't pass an
        // explicit range — otherwise an explicit `start_line = 5` on a
        // tiny file would silently ignore the request.
        if !explicit_range && file_bytes <= SMALL_FILE_BYTES as u64 {
            let contents = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
                ToolError::execution_failed(format!(
                    "Failed to read {}: {}",
                    file_path.display(),
                    e
                ))
            })?;
            context.note_file_read(&file_path);

            let total_lines = contents.lines().count();
            if total_lines <= SMALL_FILE_LINES {
                // The whole file is in hand, so hash it directly rather than
                // re-reading it. Prefixed as a header line because this branch
                // returns the contents unwrapped — there is no `<file …>` tag
                // to hang the attribute on.
                let hash = content_hash(contents.as_bytes());
                let body = format!("{}{contents}", content_hash_header(&hash));
                return Ok(ToolResult::success(body).with_metadata(json!({
                    "evidence_routing": "inline",
                    "content_hash": hash
                })));
            }

            // Small in bytes but too many lines: render the default window
            // straight from the in-memory contents.
            let hash = content_hash(contents.as_bytes());
            let window: Vec<String> = contents
                .lines()
                .take(DEFAULT_READ_LINES)
                .map(str::to_string)
                .collect();
            return Ok(render_line_window(
                path_str,
                &window,
                total_lines,
                1,
                DEFAULT_READ_LINES,
                Some(hash.as_str()),
            ));
        }

        // Strict types (2026-08-04 review): a `start_line:"1200"` string or a
        // negative/float value used to silently fall back to the defaults —
        // returning the head of the file instead of the window the model
        // asked for, the exact wrong-answer-shaped-like-a-right-one this
        // action's alias/unknown-parameter hardening exists to prevent.
        let start_line = match optional_u64(&input, "start_line", 1)? {
            0 => {
                return Err(ToolError::invalid_input(
                    "start_line must be 1-based and greater than 0".to_string(),
                ));
            }
            v => usize::try_from(v).map_err(|_| {
                ToolError::invalid_input(
                    "start_line exceeds platform addressable range".to_string(),
                )
            })?,
        };

        let max_lines = match optional_u64(&input, "max_lines", DEFAULT_READ_LINES as u64)? {
            0 => {
                return Err(ToolError::invalid_input(
                    "max_lines must be greater than 0".to_string(),
                ));
            }
            v => {
                let converted = usize::try_from(v).map_err(|_| {
                    ToolError::invalid_input(
                        "max_lines exceeds platform addressable range".to_string(),
                    )
                })?;
                std::cmp::min(converted, HARD_MAX_READ_LINES)
            }
        };

        // Bounded read for ranged/large files: skip and take lines through a
        // BufReader instead of materializing the whole file. The stream still
        // runs to EOF so the total line count and whole-file UTF-8 validation
        // match the historical read_to_string behavior. Open, stream, and hash
        // all run on the blocking pool (blocking-call convention, #6149).
        let (window, total_lines, hash) = tokio::task::spawn_blocking({
            let file_path = file_path.clone();
            move || {
                let file = fs::File::open(&file_path).map_err(|e| {
                    ToolError::execution_failed(format!(
                        "Failed to read {}: {}",
                        file_path.display(),
                        e
                    ))
                })?;
                let (window, total_lines) = read_window_streaming(file, start_line, max_lines)
                    .map_err(|e| {
                        ToolError::execution_failed(format!(
                            "Failed to read {}: {}",
                            file_path.display(),
                            e
                        ))
                    })?;
                // The window is a slice; the guard needs the whole file. A
                // second streaming pass digests the rest without ever
                // materializing it. A failure here only costs the guard — the
                // read itself already succeeded, so the window is still
                // returned, just without a hash to pass back to `edit`.
                // Special files are skipped: reopening a FIFO or device can
                // block indefinitely (or re-consume a one-shot stream), and a
                // stream has no stable content an edit guard could pin.
                let hash = match fs::metadata(&file_path) {
                    Ok(meta) if meta.is_file() => hash_file_streaming(&file_path).ok(),
                    _ => None,
                };
                Ok::<_, ToolError>((window, total_lines, hash))
            }
        })
        .await
        .map_err(|e| ToolError::execution_failed(format!("File read task: {e}")))??;
        context.note_file_read(&file_path);

        // `start_line > total_lines` is not an error — it lets the model
        // page past the end without raising. Returns an empty-content
        // sentinel so subsequent reads can stop.
        if start_line > total_lines {
            let hash_attr = hash
                .as_deref()
                .map(|hash| format!(" content_hash=\"{hash}\""))
                .unwrap_or_default();
            let output = format!(
                "<file path=\"{path_str}\" total_lines=\"{total_lines}\" shown_lines=\"none\" truncated=\"false\"{hash_attr}>\n\
                 \n\
                 [NO CONTENT] start_line {start_line} is beyond total_lines {total_lines}.\n\
                 </file>"
            );
            return Ok(ToolResult::success(output).with_metadata(json!({
                "evidence_routing": "inline",
                "content_hash": hash
            })));
        }

        Ok(render_line_window(
            path_str,
            &window,
            total_lines,
            start_line,
            max_lines,
            hash.as_deref(),
        ))
    }
}

// Bounded output for large files. The small-file fast path keeps the
// historical "return contents unchanged" behavior so existing flows
// (small configs, single source files, etc.) don't suddenly start
// seeing wrapped output. Once a file is large or the caller asks
// for an explicit range, we switch to a numbered, line-tagged
// window with continuation hints so the model can page through
// without re-loading the entire file on every turn. Harvested
// from PR #1451 by @Oliver-ZPLiu, closes part of #1450.
// One bound, not two competing ones. The real cost of a read is BYTES of
// context, and `MAX_VISIBLE_BYTES` already enforces that. A separate 200-line
// default fired long before the byte budget on any prose file — a 229-line,
// 12 KB document truncated at line 200 with a third of the budget unspent,
// costing a second round trip to fetch 29 lines. The line cap now only guards
// pathologically short lines, where 500 lines is still a small read.
const DEFAULT_READ_LINES: usize = HARD_MAX_READ_LINES;
const HARD_MAX_READ_LINES: usize = 500;
const MAX_VISIBLE_BYTES: usize = 16 * 1024;
const SMALL_FILE_LINES: usize = HARD_MAX_READ_LINES;
const SMALL_FILE_BYTES: usize = 16 * 1024;

/// Stream a line window out of `file`: skip `start_line - 1` lines, collect
/// up to `max_lines`, then keep counting (and validating UTF-8) to EOF.
/// Returns the collected window plus the total line count. Only the window
/// is ever held in memory.
fn read_window_streaming(
    file: fs::File,
    start_line: usize,
    max_lines: usize,
) -> std::io::Result<(Vec<String>, usize)> {
    use std::io::BufRead;

    let mut reader = std::io::BufReader::new(file);
    let mut raw: Vec<u8> = Vec::new();
    let mut window: Vec<String> = Vec::new();
    let mut total_lines = 0usize;
    let start_idx = start_line - 1;

    loop {
        raw.clear();
        let n = reader.read_until(b'\n', &mut raw)?;
        if n == 0 {
            break;
        }
        // Mirror `str::lines`: strip the trailing '\n', and a '\r' only when
        // it directly precedes that '\n'.
        let mut end = raw.len();
        if raw[..end].ends_with(b"\n") {
            end -= 1;
            if raw[..end].ends_with(b"\r") {
                end -= 1;
            }
        }
        // Validate every line so invalid UTF-8 anywhere in the file fails
        // exactly like the previous whole-file read_to_string did.
        let line = std::str::from_utf8(&raw[..end]).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
        })?;
        if total_lines >= start_idx && window.len() < max_lines {
            window.push(line.to_string());
        }
        total_lines += 1;
    }

    Ok((window, total_lines))
}

/// Marker placed between the retained head and tail when a read window is
/// truncated by the byte budget. Mirrors qwen-code's truncation style so the
/// model sees both ends of the range.
const BYTE_TRUNCATION_SEPARATOR: &str = "\n\n---\n... [CONTENT TRUNCATED] ...\n---\n\n";

/// Split `content` into a head of at most `head_budget` bytes and a tail that
/// fills the remainder of `total_budget` (separator accounted for). Never
/// overlaps and never splits mid-codepoint. Style matches qwen-code:
/// `head_budget = total_budget / 5`.
fn head_tail_for_budget(content: &str, total_budget: usize) -> (String, String) {
    let head_budget = (total_budget / 5).max(1);
    let head_end = (0..=head_budget.min(content.len()))
        .rev()
        .find(|&i| content.is_char_boundary(i))
        .unwrap_or(0);
    let sep_len = BYTE_TRUNCATION_SEPARATOR.len();
    let tail_budget = total_budget
        .saturating_sub(head_end)
        .saturating_sub(sep_len)
        .max(1);
    let tail_floor = content.len().saturating_sub(tail_budget).max(head_end);
    let tail_start = (tail_floor..=content.len())
        .find(|&i| content.is_char_boundary(i))
        .unwrap_or(content.len());
    (
        content[..head_end].to_string(),
        content[tail_start..].to_string(),
    )
}

/// Render a collected line window into the `<file …>` wrapper used for
/// ranged/large reads. `window` must hold the lines for
/// `start_line..start_line + max_lines` (clamped to EOF).
fn render_line_window(
    path_str: &str,
    window: &[String],
    total_lines: usize,
    start_line: usize,
    max_lines: usize,
    content_hash: Option<&str>,
) -> ToolResult {
    let zero_based_start = start_line - 1;
    let zero_based_end = std::cmp::min(zero_based_start + max_lines, total_lines);
    let shown_first = start_line;
    let shown_last = zero_based_end; // 1-based inclusive line number of the last shown line

    let mut numbered = String::new();
    for (offset, line) in window.iter().enumerate() {
        let line_no = start_line + offset;
        numbered.push_str(&format!("{line_no:>6}│ {line}\n"));
    }

    // UTF-8-safe byte truncation of the rendered range. Qwen-style: keep a
    // short head (budget/5) plus the matching tail so the model sees both
    // ends of a long range. The full file already lives at `path_str` — the
    // recovery note names that absolute/workspace path for a re-read.
    let visible_bytes =
        crate::tools::large_output_router::WorkshopConfig::active_read_result_max_bytes()
            .map(|n| n.clamp(MAX_VISIBLE_BYTES, READ_RESULT_ABSOLUTE_MAX_BYTES))
            .unwrap_or(MAX_VISIBLE_BYTES);
    let truncated_by_bytes = numbered.len() > visible_bytes;
    let shown_content = if truncated_by_bytes {
        let (head, tail) = head_tail_for_budget(&numbered, visible_bytes);
        format!("{head}{BYTE_TRUNCATION_SEPARATOR}{tail}")
    } else {
        numbered
    };

    let truncated_by_lines = zero_based_end < total_lines;
    let truncated = truncated_by_lines || truncated_by_bytes;
    let next_start = zero_based_end + 1;

    let mut attrs = format!(
        "path=\"{path_str}\" total_lines=\"{total_lines}\" shown_lines=\"{shown_first}-{shown_last}\" truncated=\"{truncated}\""
    );
    if truncated_by_lines {
        attrs.push_str(&format!(" next_start_line=\"{next_start}\""));
    }
    // Hashes the whole file, not the shown window — a partial read still
    // yields a guard the model can pass to `edit`/`patch`.
    if let Some(hash) = content_hash {
        attrs.push_str(&format!(" content_hash=\"{hash}\""));
    }

    let mut output = format!("<file {attrs}>\n{shown_content}");
    if truncated_by_lines {
        output.push_str(&format!(
            "\n[TRUNCATED] Showing lines {shown_first}-{shown_last} of {total_lines}. To continue, call read with path=\"{path_str}\" offset={next_start} limit={max_lines}\n"
        ));
    }
    if truncated_by_bytes {
        if shown_first == shown_last {
            // One line alone exceeds the byte budget: no start_line/max_lines
            // combination can ever reveal the elided middle, so the note must
            // not pretend otherwise — name the escape hatch that works.
            output.push_str(&format!(
                "\n[TRUNCATED] Line {shown_first} alone exceeds the {visible_bytes}-byte output budget; showing its head + tail. No line window can reveal the middle of one line — use a searched shell slice when needed.\n"
            ));
        } else {
            let narrower = (shown_last - shown_first).div_ceil(2).max(1);
            output.push_str(&format!(
                "\n[TRUNCATED] The selected range exceeded the {visible_bytes}-byte output budget; showing head + tail of lines {shown_first}-{shown_last}. Re-read narrower windows to see the middle, e.g. offset={shown_first} limit={narrower}, then advance offset.\n"
            ));
        }
    }
    output.push_str("</file>");

    // The file tool self-bounds at its own byte budget and carries its own continuation
    // contract (`next_start_line`), so the large-output spillover envelope
    // must never re-wrap a read result with a second, weaker truncation.
    ToolResult::success(output).with_metadata(json!({
        "evidence_routing": "inline",
        "content_hash": content_hash
    }))
}

async fn read_image_via_ocr(
    path: &Path,
    requested_path: &str,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let text = crate::tools::image_ocr::ocr_image_path(path, context).await?;
    Ok(ToolResult::success(format!(
        "<image_ocr path=\"{requested_path}\">\n{text}\n</image_ocr>"
    )))
}

/// Detect an existing PDF by extension or by sniffing `%PDF` magic bytes.
async fn is_pdf(path: &Path) -> Result<bool, ToolError> {
    let extension_matches = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
    let mut file = tokio::fs::File::open(path).await.map_err(|error| {
        ToolError::execution_failed(format!("Failed to read {}: {error}", path.display()))
    })?;
    if extension_matches {
        return Ok(true);
    }
    let mut buf = [0u8; 4];
    use tokio::io::AsyncReadExt;
    Ok(file.read_exact(&mut buf).await.is_ok() && &buf == b"%PDF")
}

fn is_image_for_ocr(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp"
            )
        })
}

fn parse_pages_arg(spec: &str) -> Option<(u32, u32)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((a, b)) = trimmed.split_once('-') {
        let start: u32 = a.trim().parse().ok()?;
        let end: u32 = b.trim().parse().ok()?;
        if start == 0 || end < start {
            return None;
        }
        Some((start, end))
    } else {
        let n: u32 = trimmed.parse().ok()?;
        if n == 0 {
            return None;
        }
        Some((n, n))
    }
}

/// Clean PDF-extracted text for TUI display: collapse consecutive blank
/// lines (more than 1 becomes 1), replace NUL bytes with U+FFFD, replace
/// non-breaking spaces with regular spaces, and trim trailing whitespace
/// on each line. Produces output that won't clutter the transcript with
/// vertical gaps or invisible control characters.
fn clean_pdf_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank_run = 0usize;
    let mut any_content = false;
    for line in raw.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run = blank_run.saturating_add(1);
            if blank_run <= 1 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            any_content = true;
            // Push cleaned characters directly — avoids a per-line
            // temporary String allocation.
            for c in trimmed.chars() {
                match c {
                    '\0' => out.push('\u{FFFD}'),
                    '\u{A0}' => out.push(' '),
                    other => out.push(other),
                }
            }
            out.push('\n');
        }
    }
    // Trim leading blank lines only — don't use str::trim() which
    // would also strip intentional indentation (e.g. centred titles).
    if any_content {
        let start = out.find(|c: char| c != '\n').unwrap_or(0);
        // Walk back from end to find the last non-newline character.
        let end = out.rfind(|c: char| c != '\n').map_or(out.len(), |i| {
            i + out[i..].chars().next().map_or(1, |c| c.len_utf8())
        });
        out[start..end].to_string()
    } else {
        String::new()
    }
}

async fn read_pdf_if_detected(
    path: &Path,
    pages: Option<&str>,
    command: super::pdf::PdfTextCommand<'_>,
) -> Result<Option<ToolResult>, ToolError> {
    if !is_pdf(path).await? {
        return Ok(None);
    }
    // Validate the `pages` spec once, up front, so both extractor paths
    // surface the same error shape on bad input.
    let page_range = match pages {
        Some(spec) => match parse_pages_arg(spec) {
            Some((start, end)) => Some((start, end)),
            None => {
                return Err(ToolError::invalid_input(format!(
                    "invalid `pages` value `{spec}` (expected `N` or `N-M`, e.g. `1-5`)"
                )));
            }
        },
        None => None,
    };

    read_pdf_with_command(path, page_range, command)
        .await
        .map(Some)
}

async fn read_pdf_with_command(
    path: &Path,
    page_range: Option<(u32, u32)>,
    command: super::pdf::PdfTextCommand<'_>,
) -> Result<ToolResult, ToolError> {
    let text = super::pdf::extract_path(path, page_range, command)
        .await
        .map_err(super::pdf::into_tool_error)?;
    Ok(ToolResult::success(clean_pdf_text(&text)))
}

// === WriteFileTool ===

/// Tool for writing UTF-8 files to the workspace.
pub struct WriteFileTool;

impl WriteFileTool {
    /// Execute the small-contract-shaped lowercase writer. Compatibility-only hash
    /// arguments remain on the hidden `write_file`/`File` paths.
    pub(super) async fn execute_contract_write(
        input: Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        reject_primitive_unknown(&input, "write", &["path", "content"])?;
        let path_str = required_str(&input, "path")?;
        let file_content = required_str(&input, "content")?;
        check_contract_file_size(file_content.len())?;
        let file_path = context.resolve_path(path_str)?;
        let mutation_guard = acquire_file_mutation(&file_path, context).await?;
        check_file_operation_cancelled(context)?;

        let prior = load_contract_source(&file_path, false, context).await?;
        let existed_before = prior.is_some();
        if existed_before {
            context.require_fresh_read_before_write(&file_path, path_str)?;
        }
        let prior_bytes = prior.unwrap_or_default();
        let prior_contents = String::from_utf8_lossy(&prior_bytes);

        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                ToolError::execution_failed(format!(
                    "Failed to create directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        check_file_operation_cancelled(context)?;
        // Preserve the existing file's line-ending style on overwrite (see
        // `preserve_prior_line_endings`); otherwise a CRLF (Windows) file is
        // silently rewritten with LF line endings.
        let mut written = if prior_contents.is_empty() {
            file_content.to_string()
        } else {
            let normalized = normalize_contract_line_endings(file_content);
            let ending = contract_line_ending(&prior_contents);
            let extra = if ending == "\r\n" {
                normalized.bytes().filter(|byte| *byte == b'\n').count()
            } else {
                0
            };
            check_contract_file_size(normalized.len().saturating_add(extra))?;
            restore_contract_line_endings(&normalized, ending)
        };
        guard_edit(
            &file_path,
            path_str,
            existed_before.then(|| prior_contents.as_ref()),
            &written,
        )?;
        if existed_before
            && let Some(normalized) = normalize_edit(&file_path, &prior_contents, &written).await
        {
            written = normalized;
        }
        check_contract_file_size(written.len())?;
        check_file_operation_cancelled(context)?;
        // Once replacement starts, report its actual completion even if cancelled.
        run_blocking_write_atomic(&file_path, written.clone().into_bytes()).await?;
        context.note_file_read(&file_path);
        drop(mutation_guard);

        let outcome = if existed_before { "updated" } else { "created" };
        // Bytes are bytes: the receipt reports what was written to disk,
        // not UTF-16 code units (which differ on non-ASCII content).
        let bytes_written = written.len();
        Ok(contract_mutation_result(
            context,
            &file_path,
            path_str,
            prior_contents.as_ref(),
            &written,
            written.as_bytes(),
            outcome,
            format!("Successfully wrote {bytes_written} bytes to {path_str}"),
        )
        .await)
    }
}

#[async_trait]
impl ToolSpec for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Write content to a UTF-8 file in the workspace. Unlike heredocs (`cat <<EOF > file`) or `echo > file` in `Bash`, its diffs render inline and its approval shows the change. Creates or overwrites; parent directories are auto-created. Pass `expected_hash` (the `content_hash` from a prior `read`) to have the overwrite refused if the file changed since that read."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Alias: `file_path`"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write"
                },
                "expected_hash": {
                    "type": "string",
                    "description": EXPECTED_HASH_DESCRIPTION
                }
            },
            "required": ["path", "content"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::Sandboxable,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Suggest
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let mut input = input;
        apply_param_aliases(&mut input, PATH_ALIASES, "File write")?;
        WRITE_PARAMS.reject_unknown(&input)?;

        let path_str = required_str(&input, "path")?;
        let file_content = required_str(&input, "content")?;
        let expected_hash = optional_str(&input, "expected_hash")?;

        let file_path = context.resolve_path(path_str)?;

        // Snapshot the existing contents (if any) before we overwrite — used
        // to render an inline diff in the tool result. Only a genuinely
        // absent path is "new": a stat that fails for any other reason must
        // not let an existing file be overwritten as if it were empty.
        let existed_before = tokio::fs::try_exists(&file_path).await.map_err(|error| {
            ToolError::execution_failed(format!(
                "Failed to inspect {}: {error}",
                file_path.display()
            ))
        })?;
        let prior_contents = if existed_before {
            tokio::fs::read_to_string(&file_path)
                .await
                .map_err(|error| {
                    ToolError::execution_failed(format!(
                        "Failed to read {}: {error}",
                        file_path.display()
                    ))
                })?
        } else {
            String::new()
        };

        // Content-hash guard (#3979), checked against the same snapshot the
        // diff is rendered from and before any directory or file is touched.
        if let Some(expected) = expected_hash {
            if !existed_before {
                // A hash describes a file that was read. Guarding a create is
                // a contradiction, and silently creating the file anyway would
                // defeat the guard the caller asked for — fail closed.
                return Err(ToolError::execution_failed(format!(
                    "File `write` refused: expected_hash was supplied but {path_str} does not exist, so there is no snapshot to verify and nothing was written. Recovery: drop `expected_hash` to create the file, or read the intended path first."
                )));
            }
            verify_expected_hash(Some(expected), prior_contents.as_bytes(), "write", path_str)?;
        } else if existed_before {
            context.require_fresh_read_before_write(&file_path, path_str)?;
        }

        // Create parent directories if needed
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ToolError::execution_failed(format!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        // Preserve the existing file's line-ending style on overwrite (see
        // `preserve_prior_line_endings`); a full `write_file` over a CRLF
        // (Windows) file otherwise silently rewrites every line ending to LF.
        let mut written = preserve_prior_line_endings(file_content, &prior_contents);

        guard_edit(
            &file_path,
            path_str,
            existed_before.then(|| prior_contents.as_ref()),
            &written,
        )?;
        if existed_before
            && let Some(normalized) = normalize_edit(&file_path, &prior_contents, &written).await
        {
            written = normalized;
        }

        run_blocking_write_atomic(&file_path, written.clone().into_bytes()).await?;
        context.note_file_read(&file_path);

        let display = file_path.display().to_string();
        let diff = make_unified_diff(&display, &prior_contents, &written);
        let summary = if existed_before {
            format!("Wrote {} bytes to {}", written.len(), display)
        } else {
            format!("Created {} ({} bytes)", display, written.len())
        };
        let body = if diff.is_empty() {
            format!("{summary}\n(no changes)")
        } else {
            format!("{diff}\n{summary}")
        };

        // Append LSP diagnostics for the written file when enabled (#428).
        let diag_block = lsp_diagnostics_for_paths(context, &[file_path]).await;
        let full_body = if diag_block.is_empty() {
            body
        } else {
            format!("{body}\n{diag_block}")
        };

        let outcome = if existed_before { "updated" } else { "created" };
        // Keep the execution-owned receipt workspace-relative even though the
        // legacy model-facing output above retains its resolved-path wording.
        let receipt_diff = make_unified_diff(path_str, &prior_contents, &written);
        Ok(ToolResult::success(full_body).with_metadata(json!({
            "event": "file.mutation",
            "mutation": {
                "diff": receipt_diff,
                "files": [mutation_file_entry(path_str, outcome, Some(written.as_bytes()))],
                "renames": []
            }
        })))
    }
}

// === EditFileTool ===

/// Tool for search/replace editing of files.
pub struct EditFileTool;

#[derive(Clone, Debug)]
struct ContractEdit {
    index: usize,
    old_text: String,
    new_text: String,
}

#[derive(Clone, Debug)]
struct ResolvedContractEdit {
    index: usize,
    start: usize,
    end: usize,
    replacement: String,
}

fn normalize_contract_line_endings(text: &str) -> String {
    if !text.as_bytes().contains(&b'\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn contract_line_ending(text: &str) -> &'static str {
    match text.find('\n') {
        Some(index) if index > 0 && text.as_bytes()[index - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

fn restore_contract_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// First code point of the placeholder range that carries one non-UTF-8 byte
/// through a text edit (Supplementary Private Use Area-A, U+F0000..=U+F00FF).
const RAW_BYTE_PLACEHOLDER_BASE: u32 = 0xF_0000;

fn is_raw_byte_placeholder(ch: char) -> bool {
    (RAW_BYTE_PLACEHOLDER_BASE..=RAW_BYTE_PLACEHOLDER_BASE + 0xFF).contains(&u32::from(ch))
}

/// Decode `bytes` for a text edit without losing any of them: valid UTF-8 is
/// kept as text and each invalid byte becomes a placeholder code point that
/// [`encode_lossless_text`] turns back into the same byte. A file that is not
/// UTF-8 and already uses the placeholder range (or edits that do) cannot be
/// round-tripped, so the edit is refused rather than risk a silent rewrite.
///
/// The flag is `true` only when placeholders were introduced. A valid UTF-8
/// file keeps its characters as they are, including any in the placeholder
/// range (Nerd Font icons live there), and is written back as plain UTF-8.
fn decode_bytes_losslessly(
    bytes: &[u8],
    edits: &[ContractEdit],
    path: &str,
) -> Result<(String, bool), ToolError> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok((text.to_string(), false));
    }
    let refuse = || {
        ToolError::execution_failed(format!(
            "Could not edit file {path}: it is not valid UTF-8 and uses characters Codewhale needs to keep its raw bytes intact. The file was not changed; use File `patch` or a shell tool for this file."
        ))
    };
    if edits.iter().any(|edit| {
        edit.old_text.chars().any(is_raw_byte_placeholder)
            || edit.new_text.chars().any(is_raw_byte_placeholder)
    }) {
        return Err(refuse());
    }
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        if chunk.valid().chars().any(is_raw_byte_placeholder) {
            return Err(refuse());
        }
        out.push_str(chunk.valid());
        for &byte in chunk.invalid() {
            out.push(
                char::from_u32(RAW_BYTE_PLACEHOLDER_BASE + u32::from(byte))
                    .expect("placeholder range holds valid code points"),
            );
        }
    }
    Ok((out, true))
}

/// Inverse of [`decode_bytes_losslessly`] for a file it decoded with
/// placeholders. Never call it on text from a valid UTF-8 file.
fn encode_lossless_text(text: &str) -> Vec<u8> {
    if !text.chars().any(is_raw_byte_placeholder) {
        return text.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut buf = [0_u8; 4];
    for ch in text.chars() {
        if is_raw_byte_placeholder(ch) {
            out.push((u32::from(ch) - RAW_BYTE_PLACEHOLDER_BASE) as u8);
        } else {
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

// Raw-byte placeholders use four UTF-8 bytes in memory but encode as one.
fn contract_encoded_size(text: &str, has_raw_bytes: bool) -> usize {
    if has_raw_bytes {
        text.chars()
            .map(|ch| {
                if is_raw_byte_placeholder(ch) {
                    1
                } else {
                    ch.len_utf8()
                }
            })
            .sum()
    } else {
        text.len()
    }
}

fn append_contract_text(
    result: &mut String,
    encoded_size: &mut usize,
    text: &str,
    has_raw_bytes: bool,
) -> Result<(), ToolError> {
    let next = encoded_size.saturating_add(contract_encoded_size(text, has_raw_bytes));
    check_contract_file_size(next)?;
    result.push_str(text);
    *encoded_size = next;
    Ok(())
}

/// The line terminator of each line in `text`, in order (`\r\n`, `\n` or a
/// lone `\r`). The k-th entry ends the k-th line of the LF-normalized text.
fn line_terminators(text: &str) -> Vec<&'static str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                out.push("\r\n");
                index += 2;
            }
            b'\r' => {
                out.push("\r");
                index += 1;
            }
            b'\n' => {
                out.push("\n");
                index += 1;
            }
            _ => index += 1,
        }
    }
    out
}

/// Give every line an edit left alone its original terminator back, and the
/// file's dominant `fallback` terminator to lines the edit wrote (B6). The
/// edit ran on LF-normalized text; restoring one style for the whole file
/// rewrote mixed-ending files on lines nobody touched.
fn restore_line_endings_per_line(
    original: &str,
    normalized_original: &str,
    updated: &str,
    fallback: &str,
    has_raw_bytes: bool,
) -> Result<String, ToolError> {
    let terminators = line_terminators(original);
    if terminators.iter().all(|ending| *ending == fallback) {
        let extra = if fallback == "\r\n" {
            updated.bytes().filter(|byte| *byte == b'\n').count()
        } else {
            0
        };
        check_contract_file_size(
            contract_encoded_size(updated, has_raw_bytes).saturating_add(extra),
        )?;
        return Ok(restore_contract_line_endings(updated, fallback));
    }
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_secs(1))
        .diff_lines(normalized_original, updated);
    let mut out = String::new();
    let mut encoded_size = 0;
    for change in diff.iter_all_changes() {
        let ending = match change.tag() {
            similar::ChangeTag::Delete => continue,
            similar::ChangeTag::Equal => change
                .old_index()
                .and_then(|index| terminators.get(index).copied())
                .unwrap_or(fallback),
            similar::ChangeTag::Insert => fallback,
        };
        let line = change.value();
        match line.strip_suffix('\n') {
            Some(body) => {
                append_contract_text(&mut out, &mut encoded_size, body, has_raw_bytes)?;
                append_contract_text(&mut out, &mut encoded_size, ending, has_raw_bytes)?;
            }
            None => append_contract_text(&mut out, &mut encoded_size, line, has_raw_bytes)?,
        }
    }
    Ok(out)
}

/// Rewrite `content` to match the line-ending style of an existing file's
/// `prior` content, so a full-file overwrite (`write_file` / contract `write`)
/// does not silently flip a CRLF (Windows) file to LF — the same policy
/// `edit_file` applies. A brand-new file (no prior content) is returned
/// verbatim: there is no style to preserve.
fn preserve_prior_line_endings(content: &str, prior: &str) -> String {
    if prior.is_empty() {
        return content.to_string();
    }
    restore_contract_line_endings(
        &normalize_contract_line_endings(content),
        contract_line_ending(prior),
    )
}

/// Fallback matching view used only after a literal match fails. It follows
/// The small-contract normalization categories while leaving the public schema as
/// exact-text replacement rather than teaching a second edit mode.
fn normalize_contract_fuzzy(text: &str) -> String {
    let compatible = text.nfkc().collect::<String>();
    let mut trimmed = String::with_capacity(compatible.len());
    for (index, line) in compatible.split('\n').enumerate() {
        if index > 0 {
            trimmed.push('\n');
        }
        trimmed.push_str(line.trim_end());
    }
    trimmed
        .chars()
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn text_matches(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    haystack
        .match_indices(needle)
        .map(|(start, matched)| (start, start + matched.len()))
        .collect()
}

fn contract_edit_not_found(path: &str, index: usize, total: usize) -> ToolError {
    if total == 1 {
        ToolError::execution_failed(format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        ))
    } else {
        ToolError::execution_failed(format!(
            "Could not find edits[{index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        ))
    }
}

fn contract_edit_duplicate(path: &str, index: usize, total: usize, matches: usize) -> ToolError {
    if total == 1 {
        ToolError::execution_failed(format!(
            "Found {matches} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        ))
    } else {
        ToolError::execution_failed(format!(
            "Found {matches} occurrences of edits[{index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        ))
    }
}

fn prepare_contract_edit_input(mut input: Value) -> Result<Value, ToolError> {
    for key in ["oldText", "newText"] {
        if let Some(text) = input.get(key).and_then(Value::as_str) {
            check_contract_file_size(text.len())?;
        }
    }
    if let Some(encoded) = input.get("edits").and_then(Value::as_str) {
        check_contract_file_size(encoded.len())?;
    }
    let object = input
        .as_object_mut()
        .ok_or_else(|| ToolError::invalid_input("edit input must be an object"))?;
    if let Some(Value::String(encoded)) = object.get("edits")
        && let Ok(decoded) = serde_json::from_str::<Value>(encoded)
        && decoded.is_array()
    {
        object.insert("edits".to_string(), decoded);
    }

    let legacy_old = object
        .get("oldText")
        .and_then(Value::as_str)
        .map(str::to_string);
    let legacy_new = object
        .get("newText")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) {
        let legacy = json!({"oldText": old_text, "newText": new_text});
        let mut edits = match object.remove("edits") {
            Some(Value::Array(edits)) => edits,
            _ => Vec::new(),
        };
        edits.push(legacy);
        object.insert("edits".to_string(), Value::Array(edits));
        object.remove("oldText");
        object.remove("newText");
    }
    Ok(input)
}

fn parse_contract_edits(input: &Value) -> Result<Vec<ContractEdit>, ToolError> {
    let raw = input
        .get("edits")
        .and_then(Value::as_array)
        .ok_or_else(|| ToolError::invalid_input("edits must be an array"))?;
    if raw.is_empty() {
        return Err(ToolError::invalid_input(
            "edit requires at least one replacement in edits",
        ));
    }
    let mut replacement_bytes = 0usize;
    raw.iter()
        .enumerate()
        .map(|(index, edit)| {
            reject_primitive_unknown(edit, &format!("edits[{index}]"), &["oldText", "newText"])?;
            let old_text = required_str(edit, "oldText")?;
            let new_text = required_str(edit, "newText")?;
            check_contract_file_size(old_text.len())?;
            check_contract_file_size(new_text.len())?;
            // All non-overlapping replacements survive into the output. Count
            // normalized bytes before copying another large replacement.
            replacement_bytes = replacement_bytes
                .saturating_add(new_text.len() - new_text.match_indices("\r\n").count());
            check_contract_file_size(replacement_bytes)?;
            if old_text.is_empty() {
                return Err(ToolError::invalid_input(format!(
                    "edits[{index}].oldText must not be empty"
                )));
            }
            Ok(ContractEdit {
                index,
                old_text: normalize_contract_line_endings(old_text),
                new_text: normalize_contract_line_endings(new_text),
            })
        })
        .collect()
}

fn apply_resolved_edits(
    base: &str,
    edits: &[ResolvedContractEdit],
    offset: usize,
    has_raw_bytes: bool,
) -> Result<String, ToolError> {
    let mut encoded_size = contract_encoded_size(base, has_raw_bytes);
    check_contract_file_size(encoded_size)?;
    let mut updated = base.to_string();
    for edit in edits.iter().rev() {
        let range = edit.start.saturating_sub(offset)..edit.end.saturating_sub(offset);
        let removed = &updated[range.clone()];
        let projected_string_size = updated
            .len()
            .saturating_sub(removed.len())
            .saturating_add(edit.replacement.len());
        encoded_size = encoded_size
            .saturating_sub(contract_encoded_size(removed, has_raw_bytes))
            .saturating_add(contract_encoded_size(&edit.replacement, has_raw_bytes));
        // Reject each intermediate replacement before String::replace_range
        // allocates; raw-byte placeholders are at most four bytes in memory.
        check_contract_file_size(encoded_size)?;
        if projected_string_size > CONTRACT_FILE_MAX_BYTES * if has_raw_bytes { 4 } else { 1 } {
            check_contract_file_size(CONTRACT_FILE_MAX_BYTES + 1)?;
        }
        updated.replace_range(range, &edit.replacement);
    }
    Ok(updated)
}

fn lines_with_endings(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    }
}

fn line_spans(text: &str) -> Vec<(usize, usize)> {
    let mut offset = 0usize;
    lines_with_endings(text)
        .into_iter()
        .map(|line| {
            let span = (offset, offset + line.len());
            offset = span.1;
            span
        })
        .collect()
}

fn touched_line_range(
    spans: &[(usize, usize)],
    edit: &ResolvedContractEdit,
) -> Result<(usize, usize), ToolError> {
    let start = spans
        .iter()
        .position(|(line_start, line_end)| edit.start >= *line_start && edit.start < *line_end)
        .ok_or_else(|| ToolError::execution_failed("edit match fell outside the file"))?;
    let mut end = start;
    while end < spans.len() && spans[end].1 < edit.end {
        end += 1;
    }
    if end >= spans.len() {
        return Err(ToolError::execution_failed(
            "edit match fell outside the file",
        ));
    }
    Ok((start, end + 1))
}

fn apply_fuzzy_edits_preserving_other_lines(
    original: &str,
    normalized: &str,
    edits: &[ResolvedContractEdit],
    has_raw_bytes: bool,
) -> Result<String, ToolError> {
    let original_lines = lines_with_endings(original);
    let spans = line_spans(normalized);
    if original_lines.len() != spans.len() {
        return Err(ToolError::execution_failed(
            "fuzzy edit could not preserve the file's untouched lines",
        ));
    }

    #[derive(Debug)]
    struct Group {
        start_line: usize,
        end_line: usize,
        edits: Vec<ResolvedContractEdit>,
    }

    let mut groups: Vec<Group> = Vec::new();
    for edit in edits {
        let (start_line, end_line) = touched_line_range(&spans, edit)?;
        if let Some(group) = groups.last_mut()
            && start_line < group.end_line
        {
            group.end_line = group.end_line.max(end_line);
            group.edits.push(edit.clone());
        } else {
            groups.push(Group {
                start_line,
                end_line,
                edits: vec![edit.clone()],
            });
        }
    }

    let mut result = String::new();
    let mut encoded_size = 0;
    let mut original_line = 0usize;
    for group in groups {
        for line in &original_lines[original_line..group.start_line] {
            append_contract_text(&mut result, &mut encoded_size, line, has_raw_bytes)?;
        }
        let group_start = spans[group.start_line].0;
        let group_end = spans[group.end_line - 1].1;
        let replacement = apply_resolved_edits(
            &normalized[group_start..group_end],
            &group.edits,
            group_start,
            has_raw_bytes,
        )?;
        append_contract_text(&mut result, &mut encoded_size, &replacement, has_raw_bytes)?;
        original_line = group.end_line;
    }
    for line in &original_lines[original_line..] {
        append_contract_text(&mut result, &mut encoded_size, line, has_raw_bytes)?;
    }
    Ok(result)
}

fn apply_contract_edits(
    base: &str,
    edits: &[ContractEdit],
    path: &str,
    has_raw_bytes: bool,
) -> Result<String, ToolError> {
    let fuzzy_base = normalize_contract_fuzzy(base);
    let initial = edits
        .iter()
        .map(|edit| {
            if base.contains(&edit.old_text) {
                Ok(false)
            } else if fuzzy_base.contains(&normalize_contract_fuzzy(&edit.old_text)) {
                Ok(true)
            } else {
                Err(contract_edit_not_found(path, edit.index, edits.len()))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let use_fuzzy = initial.into_iter().any(|used| used);
    let replacement_base = if use_fuzzy { fuzzy_base.as_str() } else { base };

    let mut resolved = Vec::with_capacity(edits.len());
    for edit in edits {
        let exact = text_matches(replacement_base, &edit.old_text);
        let fuzzy_old = normalize_contract_fuzzy(&edit.old_text);
        let fuzzy_occurrences = text_matches(&fuzzy_base, &fuzzy_old).len();
        if fuzzy_occurrences > 1 {
            return Err(contract_edit_duplicate(
                path,
                edit.index,
                edits.len(),
                fuzzy_occurrences,
            ));
        }
        let matches = if exact.is_empty() {
            text_matches(replacement_base, &fuzzy_old)
        } else {
            exact
        };
        let Some(&(start, end)) = matches.first() else {
            return Err(contract_edit_not_found(path, edit.index, edits.len()));
        };
        if matches.len() > 1 {
            return Err(contract_edit_duplicate(
                path,
                edit.index,
                edits.len(),
                matches.len(),
            ));
        }
        resolved.push(ResolvedContractEdit {
            index: edit.index,
            start,
            end,
            replacement: edit.new_text.clone(),
        });
    }

    resolved.sort_by_key(|edit| (edit.start, edit.end));
    for pair in resolved.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(ToolError::execution_failed(format!(
                "edits[{}] and edits[{}] overlap in {path}; merge them or target separate regions",
                pair[0].index, pair[1].index
            )));
        }
    }

    let updated = if use_fuzzy {
        apply_fuzzy_edits_preserving_other_lines(base, replacement_base, &resolved, has_raw_bytes)?
    } else {
        apply_resolved_edits(replacement_base, &resolved, 0, has_raw_bytes)?
    };
    if updated == base {
        return Err(ToolError::execution_failed(format!(
            "No changes made to {path}; the replacement produced identical content."
        )));
    }
    Ok(updated)
}

impl EditFileTool {
    pub(super) async fn execute_contract_edits(
        input: Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input = prepare_contract_edit_input(input)?;
        reject_primitive_unknown(&input, "edit", &["path", "edits"])?;
        let path_str = required_str(&input, "path")?;
        let edits = parse_contract_edits(&input)?;
        let file_path = context.resolve_path(path_str)?;
        let mutation_guard = acquire_file_mutation(&file_path, context).await?;
        check_file_operation_cancelled(context)?;

        let raw_bytes = load_contract_source(&file_path, true, context)
            .await?
            .ok_or_else(|| {
                ToolError::execution_failed(format!(
                    "Could not edit file {path_str}: file not found"
                ))
            })?;
        check_file_operation_cancelled(context)?;
        // Bytes that are not UTF-8 ride through the edit as placeholders and
        // are written back unchanged (B6), instead of becoming U+FFFD.
        let (raw, has_raw_bytes) = decode_bytes_losslessly(&raw_bytes, &edits, path_str)?;
        let (bom, without_bom) = raw
            .strip_prefix('\u{FEFF}')
            .map_or(("", raw.as_str()), |text| ("\u{FEFF}", text));
        let ending = contract_line_ending(without_bom);
        let normalized = normalize_contract_line_endings(without_bom);
        let updated = apply_contract_edits(&normalized, &edits, path_str, has_raw_bytes)?;
        check_file_operation_cancelled(context)?;
        let restored = restore_line_endings_per_line(
            without_bom,
            &normalized,
            &updated,
            ending,
            has_raw_bytes,
        )?;
        check_contract_file_size(
            bom.len()
                .saturating_add(contract_encoded_size(&restored, has_raw_bytes)),
        )?;
        let mut final_content = format!("{bom}{restored}");
        guard_edit(&file_path, path_str, Some(&raw), &final_content)?;
        if let Some(normalized) = normalize_edit(&file_path, &raw, &final_content).await {
            final_content = normalized;
        }

        check_contract_file_size(contract_encoded_size(&final_content, has_raw_bytes))?;
        check_file_operation_cancelled(context)?;
        let bytes = if has_raw_bytes {
            encode_lossless_text(&final_content)
        } else {
            final_content.clone().into_bytes()
        };
        check_contract_file_size(bytes.len())?;
        check_file_operation_cancelled(context)?;
        // The mutation worker owns completion after atomic replacement starts.
        run_blocking_write_atomic(&file_path, bytes.clone()).await?;
        context.note_file_read(&file_path);
        drop(mutation_guard);

        Ok(contract_mutation_result(
            context,
            &file_path,
            path_str,
            &raw,
            &final_content,
            &bytes,
            "updated",
            format!(
                "Successfully replaced {} block(s) in {path_str}.",
                edits.len()
            ),
        )
        .await)
    }
}

#[async_trait]
impl ToolSpec for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Replace text in a single file via exact search/replace after the file has been read with File `read` in this session. It makes one unambiguous in-place edit, the File-tool counterpart of `sed -i` in `Bash`. `search` must match exactly one location by default; when no exact match is found the tool retries with leading-whitespace-tolerant fuzzy matching automatically. Returns a compact unified diff, not the full file. Pass `expected_hash` (the `content_hash` from that `read`) to have the edit refused, with the file untouched, if it changed in between. File `patch` handles structural, multi-block, or cross-file changes, and `write` replaces a whole file."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Alias: `file_path`"
                },
                "search": {
                    "type": "string",
                    "description": "Exact text to search for, including whitespace, indentation, and newlines. Aliases: `old_string`, `old_str`, `oldText`"
                },
                "replace": {
                    "type": "string",
                    "description": "Text to replace with. Aliases: `new_string`, `new_str`, `newText`"
                },
                "expected_hash": {
                    "type": "string",
                    "description": EXPECTED_HASH_DESCRIPTION
                }
            },
            "required": ["path", "search", "replace"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::Sandboxable,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Suggest
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        // Translate known cross-harness spellings (`old_string`/`new_string`,
        // `old_str`/`new_str`, …) onto `search`/`replace` first, then reject
        // whatever is left that we do not implement. #5209 required that a
        // mis-named edit never produce a success-shaped receipt for a file
        // that did not change; performing the edit the model unambiguously
        // asked for satisfies that more directly than refusing it did.
        let mut input = input;
        apply_param_aliases(&mut input, PATH_ALIASES, "File edit")?;
        apply_param_aliases(&mut input, EDIT_ALIASES, "File edit")?;
        EDIT_PARAMS.reject_unknown(&input)?;

        let path_str = required_str(&input, "path")?;
        let search = required_str(&input, "search")?;
        let replace = required_str(&input, "replace")?;
        let expected_hash = optional_str(&input, "expected_hash")?;

        if search == replace {
            // #5003 — long-text edits repeatedly failed here because the model
            // generated a `replace` identical to `search`. A bare "no change"
            // message gave no hint of the root cause, so the model retried the
            // same broken call. Spell out the failure and the recovery path.
            let char_count = search.chars().count();
            let line_count = search.lines().count();
            return Err(ToolError::invalid_input(format!(
                "search and replace are identical ({char_count} chars, {line_count} lines), so no change is possible. This usually means `replace` was copied verbatim from `search` instead of carrying the intended edits. Recovery: re-read the file with File action=\"read\", then retry with a `replace` that is genuinely different from `search`; for large multi-line rewrites prefer apply_patch with a unified diff."
            )));
        }
        if search.is_empty() {
            return Err(ToolError::invalid_input("search must not be empty"));
        }
        if let Some(reason) = edit_payload_looks_corrupted(search, replace) {
            return Err(ToolError::invalid_input(format!(
                "edit_file refused corrupted payload: {reason}. Recovery: re-read the file and retry with a complete replace (or use apply_patch for brace-heavy multi-line edits)."
            )));
        }

        let file_path = context.resolve_path(path_str)?;
        context.require_fresh_file_read(&file_path, path_str)?;

        let contents = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
            ToolError::execution_failed(format!("Failed to read {}: {}", file_path.display(), e))
        })?;

        // Content-hash guard (#3979). Verified against `contents` — the exact
        // snapshot every match below is computed from and that the write is
        // derived from — and before any search/replace work, so a stale hash
        // can never reach the filesystem regardless of what the search would
        // have matched.
        verify_expected_hash(expected_hash, contents.as_bytes(), "edit", path_str)?;

        // Models provide LF newlines even when the file on disk uses CRLF.
        // Match in a newline-normalized view, while retaining the sparse
        // positions where CR bytes were removed so only the original span is
        // replaced and the rest of the file stays byte-for-byte untouched.
        let (normalized_contents, crlf_positions) = normalize_crlf_with_positions(&contents);
        let normalized_search = normalize_crlf(search);
        let mut exact_ranges = normalized_contents
            .match_indices(normalized_search.as_ref())
            .map(|(start, matched)| (start, start + matched.len()));
        let first_exact_match = exact_ranges
            .next()
            .map(|range| map_normalized_range(range, crlf_positions.as_deref()));
        let exact_count = usize::from(first_exact_match.is_some()) + exact_ranges.count();

        let ((match_start, match_end), fuzz_kind) = if exact_count == 0 {
            // First fallback: tolerate indentation differences.
            let indent_matches = map_normalized_ranges(
                leading_whitespace_fuzzy_matches(
                    normalized_contents.as_ref(),
                    normalized_search.as_ref(),
                ),
                crlf_positions.as_deref(),
            );
            match indent_matches.as_slice() {
                [(start, end)] => ((*start, *end), Some("indentation")),
                [] => {
                    // Second fallback: tolerate typographic-punctuation
                    // drift (smart quotes, em-dashes, NBSP). Picks up the
                    // copy-paste failure mode where a browser/chat client
                    // silently substituted Unicode punctuation in for the
                    // ASCII the file actually contains.
                    let punct_matches = map_normalized_ranges(
                        punctuation_normalized_matches(
                            normalized_contents.as_ref(),
                            normalized_search.as_ref(),
                        ),
                        crlf_positions.as_deref(),
                    );
                    match punct_matches.as_slice() {
                        [] => {
                            // #5003 — the model could not tell why its search
                            // missed; show the first lines of the search text
                            // so it can compare against the file's contents.
                            return Err(ToolError::execution_failed(format!(
                                "Search string not found in {}. The search text starts with:\n{}\n{}Recovery: retry with the search copied from the lines above, or call File with action=\"read\" path=\"{path_str}\" to inspect the current contents.",
                                file_path.display(),
                                preview_search_for_error(search),
                                nearest_match_hint(
                                    normalized_contents.as_ref(),
                                    normalized_search.as_ref(),
                                    crlf_positions.is_some(),
                                    search.contains('\r'),
                                ),
                            )));
                        }
                        [(start, end)] => ((*start, *end), Some("punctuation")),
                        _ => {
                            return Err(ToolError::execution_failed(format!(
                                "File `edit` search is non-unique after punctuation normalization: matched {} locations in {}. Recovery: call File with action=\"read\" path=\"{path_str}\" and retry with surrounding lines that make the search unique.",
                                punct_matches.len(),
                                file_path.display()
                            )));
                        }
                    }
                }
                _ => {
                    return Err(ToolError::execution_failed(format!(
                        "File `edit` search is non-unique after indentation normalization: matched {} locations in {}. Recovery: call File with action=\"read\" path=\"{path_str}\" and retry with surrounding lines that make the search unique.",
                        indent_matches.len(),
                        file_path.display()
                    )));
                }
            }
        } else if exact_count > 1 {
            return Err(ToolError::execution_failed(format!(
                "File `edit` search is non-unique: matched {} locations in {}. \
                 Recovery: call File with action=\"read\" path=\"{path_str}\" and retry with surrounding lines that make the search unique.",
                exact_count,
                file_path.display()
            )));
        } else {
            let Some((start, end)) = first_exact_match else {
                return Err(ToolError::execution_failed(
                    "edit_file internal range accounting failed — refusing write",
                ));
            };
            let fuzz_kind = (&contents[start..end] != search).then_some("line endings");
            ((start, end), fuzz_kind)
        };

        let effective_replace =
            normalize_replacement_line_endings(replace, crlf_positions.is_some());
        let mut updated = contents.clone();
        updated.replace_range(match_start..match_end, &effective_replace);
        if updated == contents {
            return Err(ToolError::invalid_input(
                "search and replace resolve to identical file contents after line-ending normalization, no change intended",
            ));
        }

        if let Some(reason) = invalid_preprocessor_edit(&file_path, &contents, &updated) {
            return Err(ToolError::invalid_input(format!(
                "edit_file refused corrupted payload: {reason}. Recovery: re-read the file and retry with a complete replace (or use apply_patch for brace-heavy multi-line edits)."
            )));
        }

        // Fidelity: the intended replace text must appear in the updated buffer
        // (empty replace is a valid deletion). Catches host/tool bridges that
        // claim success after mangling the payload.
        if !effective_replace.is_empty() && !updated.contains(&effective_replace) {
            return Err(ToolError::execution_failed(
                "edit_file internal fidelity check failed: replace text missing from updated buffer — refusing write",
            ));
        }

        guard_edit(&file_path, path_str, Some(&contents), &updated)?;

        // #6205 — normalize after the syntax gate so the next turn's anchors
        // match the bytes on disk rather than the text the model emitted.
        let normalized_formatting = match normalize_edit(&file_path, &contents, &updated).await {
            Some(normalized) => {
                updated = normalized;
                true
            }
            None => false,
        };

        run_blocking_write_atomic(&file_path, updated.clone().into_bytes()).await?;

        // #5209 — never emit a success receipt unless the on-disk write
        // actually applied. A fabricated "Replaced 1 occurrence" + diff is
        // worse than a hard error: models trust it and re-edit the same
        // span 3–5× before noticing nothing changed.
        let on_disk = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
            ToolError::execution_failed(format!(
                "Failed to verify write to {}: {}",
                file_path.display(),
                e
            ))
        })?;
        if on_disk != updated {
            return Err(ToolError::execution_failed(format!(
                "edit_file write verification failed for {}: on-disk contents do not match the applied edit — refusing success receipt",
                file_path.display()
            )));
        }

        context.note_file_read(&file_path);

        let display = file_path.display().to_string();
        let diff = make_unified_diff(&display, &contents, &updated);
        let fuzz_note = match fuzz_kind {
            Some("indentation") => " (fuzzy indentation match)",
            Some("punctuation") => {
                " (fuzzy punctuation match — typographic quotes/dashes normalized)"
            }
            Some("line endings") => " (CRLF/LF-normalized match)",
            Some(other) => other,
            None => "",
        };
        let format_note = if normalized_formatting {
            NORMALIZED_NOTE
        } else {
            ""
        };
        let summary = format!("Replaced 1 occurrence in {display}{fuzz_note}{format_note}");
        let body = if diff.is_empty() {
            format!("{summary}\n(no textual changes)")
        } else {
            format!("{diff}\n{summary}")
        };

        // Append LSP diagnostics for the edited file when enabled (#428).
        let diag_block = lsp_diagnostics_for_paths(context, &[file_path]).await;
        let full_body = if diag_block.is_empty() {
            body
        } else {
            format!("{body}\n{diag_block}")
        };

        // The structured receipt uses the requested workspace path instead of
        // the resolved host path retained by the legacy model-facing body.
        let receipt_diff = make_unified_diff(path_str, &contents, &updated);
        Ok(ToolResult::success(full_body).with_metadata(json!({
            "event": "file.mutation",
            "mutation": {
                "diff": receipt_diff,
                "files": [mutation_file_entry(path_str, "updated", Some(updated.as_bytes()))],
                "renames": []
            }
        })))
    }
}

/// Detect catastrophic argument corruption of brace-structured edits.
///
/// Models (and some host XML/JSON bridges) occasionally deliver a `replace`
/// payload where a multi-line `{ ... }` block collapsed to empty `[]` or `{}`
/// while `search` still contains the full structured original. Writing that
/// would brick Rust match arms / JSON objects. Fail closed with recovery text
/// instead of applying the mangled payload (dogfood 2026-07-24).
///
/// Unbalanced-to-unbalanced edits with the **same** brace/bracket delta are
/// legitimate (e.g. adding `});` inside a nested fragment). Only a *change*
/// in balance is treated as truncation/mangling. Empty-bracket collapse and
/// extreme-shrinkage guards remain.
fn edit_payload_looks_corrupted(search: &str, replace: &str) -> Option<&'static str> {
    let search_curly_open = search.matches('{').count();
    let search_curly_close = search.matches('}').count();
    let replace_curly_open = replace.matches('{').count();
    let replace_curly_close = replace.matches('}').count();
    let search_square_open = search.matches('[').count();
    let search_square_close = search.matches(']').count();
    let replace_square_open = replace.matches('[').count();
    let replace_square_close = replace.matches(']').count();

    let search_curly_delta = search_curly_open as i32 - search_curly_close as i32;
    let replace_curly_delta = replace_curly_open as i32 - replace_curly_close as i32;
    let search_square_delta = search_square_open as i32 - search_square_close as i32;
    let replace_square_delta = replace_square_open as i32 - replace_square_close as i32;

    // Same delta on both sides (including both unbalanced the same way) is
    // normal for fragment edits. Divergent deltas usually mean truncation.
    if search_curly_delta != replace_curly_delta {
        return Some(
            "search/replace change `{`/`}` brace balance — the tool-call arguments were likely truncated or mangled before apply",
        );
    }
    if search_square_delta != replace_square_delta {
        return Some(
            "search/replace change `[`/`]` bracket balance — the tool-call arguments were likely truncated or mangled before apply",
        );
    }

    // Dogfood 2026-07-24: multi-line Rust `{ ... }` search collapsed into an
    // empty `[ ... ]` placeholder (host/XML arg bridge ate the brace body).
    // Count non-whitespace, non-bracket payload chars; a near-empty bracket
    // husk with a tiny tail like `=> {},` is the signature of that failure.
    if search_curly_open >= 1 && replace_square_open >= 1 {
        let significant = replace
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '[' && *c != ']')
            .count();
        if significant <= 12 {
            return Some(
                "replace collapsed a brace-structured search block into an empty/placeholder bracket span — refusing to brick the file; re-send the full replace text (prefer apply_patch for multi-line match arms)",
            );
        }
    }

    // Extreme shrinkage with lost braces (e.g. 200-char match arm -> tiny stub).
    // Balanced-to-balanced nesting changes that shrink hard still look like
    // mangling; keep this guard even when deltas match.
    if search.len() >= 80
        && replace.len() * 8 < search.len()
        && search_curly_open >= 1
        && replace_curly_open < search_curly_open
    {
        return Some(
            "replace is drastically shorter than search and lost brace structure — likely argument mangling; refuse apply",
        );
    }

    None
}

const PREPROCESSOR_CONDITIONAL_ERROR: &str = "replace would change the C/C++ preprocessor conditional balance (#if/#ifdef/#ifndef vs #endif) — the search or replace text is missing a matching directive; copy the complete block including both its opening and closing directives";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PreprocessorConditionalDebt {
    orphaned_closes: usize,
    unclosed_opens: usize,
}

impl PreprocessorConditionalDebt {
    fn total(self) -> usize {
        self.orphaned_closes + self.unclosed_opens
    }
}

/// Reject an edit only when it introduces new conditional-structure damage in
/// a file whose extension identifies it as C-family source. The whole file is
/// checked before and after the edit: complete block insertion/removal is safe,
/// while an orphaned opener or closer increases the structural debt. Existing
/// debt may be preserved or reduced so this guard never prevents a repair.
fn invalid_preprocessor_edit(path: &Path, before: &str, after: &str) -> Option<&'static str> {
    if !is_c_family_source(path) {
        return None;
    }

    let before_debt = preprocessor_conditional_debt(before);
    let after_debt = preprocessor_conditional_debt(after);
    let safe = after_debt == before_debt
        || after_debt.total() == 0
        || after_debt.total() < before_debt.total();

    (!safe).then_some(PREPROCESSOR_CONDITIONAL_ERROR)
}

fn is_c_family_source(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &[
        "c", "cc", "cp", "cpp", "cxx", "h", "h++", "hh", "hpp", "hxx", "inl", "ipp", "ixx", "m",
        "mm", "tpp", "cu", "cuh", "cppm",
    ];

    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

/// Measure unmatched preprocessor conditionals across an entire source file.
/// Tracking nesting (instead of comparing span-level tuple counts) also catches
/// an `#endif` moved before its opener. Whitespace between `#` and the directive
/// name is accepted, as it is by C preprocessors.
fn preprocessor_conditional_debt(text: &str) -> PreprocessorConditionalDebt {
    let mut depth = 0usize;
    let mut orphaned_closes = 0usize;

    for line in text.lines() {
        match preprocessor_directive(line) {
            Some("if" | "ifdef" | "ifndef") => depth += 1,
            Some("endif") if depth == 0 => orphaned_closes += 1,
            Some("endif") => depth -= 1,
            _ => {}
        }
    }

    PreprocessorConditionalDebt {
        orphaned_closes,
        unclosed_opens: depth,
    }
}

fn preprocessor_directive(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    let name_end = rest
        .find(|character: char| !character.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    (name_end > 0).then_some(&rest[..name_end])
}

/// Build a short, line-truncated preview of a (possibly very long) search
/// payload for error messages, so the model can compare what it searched for
/// against the file's actual contents without the error message ballooning.
/// The file region most like a search that did not match (#6542), with
/// 1-based line numbers and a note on whitespace / line-ending differences,
/// so the next edit can copy the real text instead of re-reading the file.
///
/// Known limitation: candidates are anchored on the search's first
/// non-blank line, so a search whose first line is also wrong may report
/// no similar region even when later lines exist in the file.
fn nearest_match_hint(
    contents: &str,
    search: &str,
    file_has_crlf: bool,
    search_has_cr: bool,
) -> String {
    const MAX_SCANNED_LINES: usize = 50_000;
    const MAX_EXCERPT_LINES: usize = 12;
    const MAX_EXCERPT_LINE_LEN: usize = 200;
    const MIN_SCORE: f32 = 0.5;

    let line_ratio = |a: &str, b: &str| -> f32 {
        let (a, b) = (a.trim(), b.trim());
        if a == b {
            1.0
        } else {
            similar::TextDiff::from_chars(a, b).ratio()
        }
    };
    let file_lines: Vec<&str> = contents.lines().take(MAX_SCANNED_LINES).collect();
    let search_lines: Vec<&str> = search.lines().collect();
    let Some(anchor) = search_lines.iter().position(|line| !line.trim().is_empty()) else {
        return String::new();
    };
    let window = search_lines.len().min(file_lines.len()).max(1);

    let mut anchors: Vec<(f32, usize)> = file_lines
        .iter()
        .enumerate()
        .filter(|(index, line)| *index >= anchor && !line.trim().is_empty())
        .map(|(index, line)| (line_ratio(search_lines[anchor], line), index - anchor))
        .collect();
    anchors.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let best = anchors
        .into_iter()
        .take(8)
        .map(|(_, start)| {
            let end = (start + window).min(file_lines.len());
            let score = search_lines
                .iter()
                .zip(&file_lines[start..end])
                .map(|(want, have)| line_ratio(want, have))
                .sum::<f32>()
                / window as f32;
            (score, start, end)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)));

    let mut notes = Vec::new();
    if search_has_cr && !file_has_crlf {
        notes.push(
            "the search contains carriage returns (CRLF) but the file uses LF line endings"
                .to_string(),
        );
    }
    let Some((score, start, end)) = best.filter(|(score, ..)| *score >= MIN_SCORE) else {
        let mut hint = String::from("No similar region found in the file.\n");
        for note in notes {
            hint.push_str(&format!("Note: {note}.\n"));
        }
        return hint;
    };
    let region = &file_lines[start..end];
    let strip_trailing = |lines: &[&str]| -> Vec<String> {
        lines
            .iter()
            .map(|line| line.trim_end().to_string())
            .collect()
    };
    let collapse = |lines: &[&str]| -> String {
        lines
            .iter()
            .flat_map(|line| line.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ")
    };
    if strip_trailing(region) == strip_trailing(&search_lines) {
        notes.push("the closest region differs only in trailing whitespace".to_string());
    } else if collapse(region) == collapse(&search_lines) {
        let tabs = |lines: &[&str]| lines.iter().any(|line| line.starts_with('\t'));
        if tabs(region) != tabs(&search_lines) {
            notes
                .push("the closest region differs only in whitespace (tabs vs spaces)".to_string());
        } else {
            notes.push("the closest region differs only in whitespace".to_string());
        }
    }
    if file_has_crlf {
        notes.push("the file uses CRLF line endings; LF in the search is fine".to_string());
    }

    let width = end.to_string().len();
    let mut hint = format!(
        "Closest match (lines {}-{}, {:.0}% similar):\n",
        start + 1,
        end,
        score * 100.0
    );
    for (offset, line) in region.iter().take(MAX_EXCERPT_LINES).enumerate() {
        let mut shown: String = line.chars().take(MAX_EXCERPT_LINE_LEN).collect();
        if line.chars().count() > MAX_EXCERPT_LINE_LEN {
            shown.push_str("...");
        }
        hint.push_str(&format!("{:>width$}\t{shown}\n", start + offset + 1));
    }
    if region.len() > MAX_EXCERPT_LINES {
        hint.push_str(&format!(
            "... ({} more lines)\n",
            region.len() - MAX_EXCERPT_LINES
        ));
    }
    for note in notes {
        hint.push_str(&format!("Note: {note}.\n"));
    }
    hint
}

fn preview_search_for_error(search: &str) -> String {
    const MAX_PREVIEW_LINES: usize = 3;
    const MAX_PREVIEW_LINE_LEN: usize = 80;
    search
        .lines()
        .take(MAX_PREVIEW_LINES)
        .map(|line| {
            if line.chars().count() > MAX_PREVIEW_LINE_LEN {
                let mut truncated: String = line.chars().take(MAX_PREVIEW_LINE_LEN).collect();
                truncated.push_str("...");
                truncated
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Normalize Windows CRLF pairs to LF while retaining the normalized byte
/// positions where a `\r` was removed. Lone carriage returns are preserved.
/// Inputs without CRLF are borrowed and use identity offsets.
///
/// A normalized boundary maps back to the original by adding the number of
/// removed CR bytes strictly before it. At the normalized newline itself that
/// excludes the current CR, so the start maps to `\r`; after the newline (or
/// at EOF) it includes that CR and spans the full pair.
fn normalize_crlf(input: &str) -> Cow<'_, str> {
    if input.contains("\r\n") {
        Cow::Owned(input.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(input)
    }
}

fn normalize_crlf_with_positions(input: &str) -> (Cow<'_, str>, Option<Vec<usize>>) {
    if !input.contains("\r\n") {
        return (Cow::Borrowed(input), None);
    }

    let mut normalized = String::with_capacity(input.len());
    let mut crlf_positions = Vec::new();
    let mut chars = input.char_indices().peekable();

    while let Some((_, ch)) = chars.next() {
        if ch == '\r' && matches!(chars.peek(), Some((_, '\n'))) {
            let _ = chars.next();
            crlf_positions.push(normalized.len());
            normalized.push('\n');
            continue;
        }

        normalized.push(ch);
    }

    (Cow::Owned(normalized), Some(crlf_positions))
}

fn map_normalized_range(
    (start, end): (usize, usize),
    crlf_positions: Option<&[usize]>,
) -> (usize, usize) {
    let Some(crlf_positions) = crlf_positions else {
        return (start, end);
    };
    let map_boundary =
        |offset| offset + crlf_positions.partition_point(|position| *position < offset);
    (map_boundary(start), map_boundary(end))
}

fn map_normalized_ranges(
    ranges: impl IntoIterator<Item = (usize, usize)>,
    crlf_positions: Option<&[usize]>,
) -> Vec<(usize, usize)> {
    ranges
        .into_iter()
        .map(|range| map_normalized_range(range, crlf_positions))
        .collect()
}

/// Convert model-provided replacement newlines to the base file's convention.
/// Fold CRLF first so an already-CRLF payload never becomes `\r\r\n`.
fn normalize_replacement_line_endings(replace: &str, use_crlf: bool) -> String {
    let lf = replace.replace("\r\n", "\n");
    if use_crlf {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

fn strip_line_leading_whitespace_with_map(input: &str) -> (String, Vec<usize>) {
    let mut normalized = String::with_capacity(input.len());
    let mut byte_map = Vec::with_capacity(input.len());
    let mut at_line_start = true;
    for (idx, ch) in input.char_indices() {
        if at_line_start && matches!(ch, ' ' | '\t') {
            continue;
        }
        normalized.push(ch);
        for _ in 0..ch.len_utf8() {
            byte_map.push(idx);
        }
        at_line_start = ch == '\n';
    }
    (normalized, byte_map)
}

fn line_start_before(input: &str, idx: usize) -> usize {
    input[..idx]
        .rfind('\n')
        .map_or(0, |newline| newline.saturating_add(1))
}

fn next_char_boundary(input: &str, idx: usize) -> usize {
    if idx >= input.len() {
        return input.len();
    }

    let mut next = idx.saturating_add(1);
    while next < input.len() && !input.is_char_boundary(next) {
        next = next.saturating_add(1);
    }
    next
}

fn leading_whitespace_fuzzy_matches(contents: &str, search: &str) -> Vec<(usize, usize)> {
    let (normalized_contents, byte_map) = strip_line_leading_whitespace_with_map(contents);
    let (normalized_search, _) = strip_line_leading_whitespace_with_map(search);
    if normalized_search.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut cursor = 0;
    while let Some(rel_idx) = normalized_contents[cursor..].find(&normalized_search) {
        let norm_start = cursor + rel_idx;
        let norm_end = norm_start + normalized_search.len();
        let Some(&mapped_start) = byte_map.get(norm_start) else {
            break;
        };
        // Use the actual match start position, expanding to line start only
        // when the match begins at a line boundary in the normalized text.
        // This prevents destroying preceding text on the same line when
        // the match starts mid-line after whitespace stripping.
        let original_start =
            if norm_start == 0 || normalized_contents.as_bytes()[norm_start - 1] == b'\n' {
                // Match starts at a line boundary — use line start for full-line replacement.
                line_start_before(contents, mapped_start)
            } else {
                // Match starts mid-line — use the exact mapped position.
                mapped_start
            };
        let original_end = byte_map.get(norm_end).copied().unwrap_or(contents.len());
        matches.push((original_start, original_end));
        cursor = next_char_boundary(&normalized_contents, norm_start);
    }
    matches
}

/// Normalize typographic punctuation to its ASCII counterpart:
///
/// * `"` `"` / U+201C U+201D → `"`
/// * `'` `'` / U+2018 U+2019 → `'`
/// * `–` `—` / U+2013 U+2014 → `-`
/// * U+00A0 (non-breaking space) → ASCII space
///
/// Returns the normalized string plus a byte-map sized to
/// `normalized.len()` whose i-th entry is the original byte offset of
/// the character that produced normalized byte i. Used to recover the
/// original-byte range after finding a match in normalized space.
fn punctuation_normalized_with_map(input: &str) -> (String, Vec<usize>) {
    let mut normalized = String::with_capacity(input.len());
    let mut byte_map = Vec::with_capacity(input.len());
    for (idx, ch) in input.char_indices() {
        let replacement: Option<char> = match ch {
            '\u{201C}' | '\u{201D}' => Some('"'),
            '\u{2018}' | '\u{2019}' => Some('\''),
            '\u{2013}' | '\u{2014}' => Some('-'),
            '\u{00A0}' => Some(' '),
            _ => None,
        };
        let written = replacement.unwrap_or(ch);
        normalized.push(written);
        for _ in 0..written.len_utf8() {
            byte_map.push(idx);
        }
    }
    (normalized, byte_map)
}

/// Try to find `search` inside `contents` after normalizing typographic
/// punctuation in both. Catches the copy-paste failure mode where a
/// browser, word processor, or chat client silently converted ASCII
/// quotes/dashes to their Unicode "pretty" forms.
fn punctuation_normalized_matches(contents: &str, search: &str) -> Vec<(usize, usize)> {
    let (norm_contents, byte_map) = punctuation_normalized_with_map(contents);
    let (norm_search, _) = punctuation_normalized_with_map(search);
    if norm_search.is_empty() {
        return Vec::new();
    }
    // If normalization didn't change anything, the exact-match pass
    // already considered this case — skip to avoid double-reporting.
    if norm_contents == contents && norm_search == search {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut cursor = 0;
    while let Some(rel_idx) = norm_contents[cursor..].find(&norm_search) {
        let norm_start = cursor + rel_idx;
        let norm_end = norm_start + norm_search.len();
        let Some(&original_start) = byte_map.get(norm_start) else {
            break;
        };
        let original_end = byte_map.get(norm_end).copied().unwrap_or(contents.len());
        matches.push((original_start, original_end));
        cursor = next_char_boundary(&norm_contents, norm_start);
    }
    matches
}

// === ListDirTool ===

/// Tool for listing directory contents.
pub struct ListDirTool;

const LIST_DIR_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on entries returned by a single `list_dir` call so a huge directory
/// (node_modules, build output, photo dumps) can't balloon the tool result.
/// Mirrors the bounded-output idiom of `read_file`'s `HARD_MAX_READ_LINES`.
/// Directories at or under the cap keep the historical plain-array response;
/// larger ones return an object with truncation metadata.
const LIST_DIR_MAX_ENTRIES: usize = 500;

#[async_trait]
impl ToolSpec for ListDirTool {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn model_visible(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "List entries in a workspace directory. This bounded, sandbox-aware tool is searchable when the core read/write/edit/bash toolbox is not enough."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to inspect (relative to workspace, absolute, or ~/ home-relative; default: .)"
                }
            },
            "required": []
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let mut input = input;
        apply_param_aliases(&mut input, PATH_ALIASES, "File list")?;
        LIST_PARAMS.reject_unknown(&input)?;

        let path_str = optional_str(&input, "path")?.unwrap_or(".");
        // S1: enumerating a denied directory is a read of it — `list_dir ~/.ssh`
        // hands back the key file names. Seatbelt's `deny file-read*` blocks
        // readdir of denied dirs, so refusing here matches the OS layer. The
        // raw spelling is checked first (F2) so the refusal names the caller's
        // path, never a symlink target it might resolve to.
        enforce_read_denylist(Path::new(path_str), "list_dir")?;
        let dir_path = context.resolve_path(path_str)?;
        enforce_read_denylist(&dir_path, "list_dir")?;

        let entries =
            list_dir_entries_async(dir_path, context.cancel_token.clone(), LIST_DIR_TIMEOUT)
                .await?;

        ToolResult::json(&entries).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

async fn list_dir_entries_async(
    dir_path: PathBuf,
    cancel_token: Option<CancellationToken>,
    timeout: Duration,
) -> Result<Value, ToolError> {
    let worker_cancel_token = cancel_token.clone();
    run_blocking_list_dir(timeout, cancel_token, move || {
        list_dir_entries(&dir_path, worker_cancel_token.as_ref())
    })
    .await
}

async fn run_blocking_list_dir<F>(
    timeout: Duration,
    cancel_token: Option<CancellationToken>,
    list_dir: F,
) -> Result<Value, ToolError>
where
    F: FnOnce() -> Result<Value, ToolError> + Send + 'static,
{
    if cancel_token
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(list_dir_cancelled());
    }

    let task = tokio::task::spawn_blocking(list_dir);
    let result = match cancel_token {
        Some(token) => {
            tokio::select! {
                biased;
                () = token.cancelled() => return Err(list_dir_cancelled()),
                result = tokio::time::timeout(timeout, task) => result,
            }
        }
        None => tokio::time::timeout(timeout, task).await,
    };

    let joined = result.map_err(|_| list_dir_timeout(timeout))?;
    joined.map_err(|err| {
        ToolError::execution_failed(format!("list_dir worker failed before completion: {err}"))
    })?
}

fn list_dir_entries(
    dir_path: &Path,
    cancel_token: Option<&CancellationToken>,
) -> Result<Value, ToolError> {
    check_list_dir_cancelled(cancel_token)?;

    let mut entries = Vec::new();
    let mut total_entries = 0usize;

    for entry in fs::read_dir(dir_path).map_err(|e| {
        ToolError::execution_failed(format!(
            "Failed to read directory {}: {}",
            dir_path.display(),
            e
        ))
    })? {
        check_list_dir_cancelled(cancel_token)?;

        let entry = entry.map_err(|e| ToolError::execution_failed(e.to_string()))?;
        total_entries += 1;
        // Past the cap, keep counting for the truncation metadata but stop
        // materializing entries.
        if entries.len() >= LIST_DIR_MAX_ENTRIES {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;

        entries.push(json!({
            "name": entry.file_name().to_string_lossy().to_string(),
            "is_dir": file_type.is_dir(),
        }));
    }

    if total_entries > entries.len() {
        Ok(json!({
            "entries": entries,
            "listed_entries": LIST_DIR_MAX_ENTRIES,
            "total_entries": total_entries,
            "truncated": true,
        }))
    } else {
        Ok(Value::Array(entries))
    }
}

fn check_list_dir_cancelled(cancel_token: Option<&CancellationToken>) -> Result<(), ToolError> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(list_dir_cancelled());
    }
    Ok(())
}

fn list_dir_cancelled() -> ToolError {
    ToolError::cancelled("list_dir cancelled before completion")
}

fn list_dir_timeout(timeout: Duration) -> ToolError {
    ToolError::Timeout {
        seconds: timeout.as_secs().max(1),
    }
}

// === Unit Tests ===

#[cfg(test)]
#[path = "file/tests.rs"]
mod pdf_tests;

#[cfg(test)]
#[path = "file/tests/tools.rs"]
mod tests;
