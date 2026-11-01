//! Exact effective base-prompt preview (#3928).
//!
//! `/constitution preview` shows the *structured user constitution*. This module
//! shows something different and stricter: **the exact bytes the next turn's
//! system prompt is assembled from**, block by block, with provenance, digests,
//! and byte/token measures.
//!
//! Three properties make it trustworthy:
//!
//! - **Exactness by construction.** [`preview`] takes the very
//!   [`SystemPrompt`] the caller is about to send. It never re-derives, re-orders,
//!   or re-renders anything, so "what you previewed" and "what was sent" cannot
//!   drift. [`BasePromptPreview::exact_digest`] is computed over the unredacted
//!   assembled text and equals the digest of
//!   [`system_prompt_flat_text`](super::system_prompt_flat_text).
//! - **Truthful provenance.** Each segment says where its bytes came from:
//!   the bundled constant, a config-directory override (with the opt-in gate
//!   that let it in), an embedder composer hook, the user-global constitution
//!   file, workspace-generated context, or a marked WorldState fragment. No
//!   segment is described by a source-tree path it does not actually load from.
//! - **Human-only.** This is a pure function of a `&SystemPrompt`. It makes no
//!   provider call, expands no tool catalog, spawns nothing, and takes no
//!   registry — so previewing costs nothing and cannot change the next turn.
//!
//! Display text is redacted (home paths, key-shaped tokens, URL userinfo) while
//! the measures and digests stay over the real bytes, so a redacted preview
//! still tells the truth about size and identity.

use std::path::Path;

use codewhale_models::{SystemBlock, SystemPrompt};

/// Pager title for the preview surface.
pub const PREVIEW_TITLE: &str = "Effective Base Prompt";

/// Bytes-per-token divisor for the coarse, deterministic token estimate.
/// Shared with the constitution's own projection so the two agree.
pub const APPROX_BYTES_PER_TOKEN: usize = codewhale_config::APPROX_BYTES_PER_TOKEN;

/// Deterministic size measures for a run of prompt bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measures {
    pub byte_len: usize,
    pub char_len: usize,
    /// Coarse estimate, not a tokenizer result. Labeled as approximate wherever
    /// it is displayed.
    pub approx_tokens: usize,
}

impl Measures {
    #[must_use]
    pub fn of(text: &str) -> Self {
        let byte_len = text.len();
        Self {
            byte_len,
            char_len: text.chars().count(),
            approx_tokens: byte_len.div_ceil(APPROX_BYTES_PER_TOKEN),
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            byte_len: self.byte_len + other.byte_len,
            char_len: self.char_len + other.char_len,
            approx_tokens: self.approx_tokens + other.approx_tokens,
        }
    }
}

/// Where a segment's bytes actually came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentProvenance {
    /// A compile-time constant shipped in the binary.
    Bundled { symbol: &'static str },
    /// A file under `$CODEWHALE_HOME` that replaced a bundled constant, plus the
    /// opt-in environment gate that allowed it.
    ConfigOverride {
        path: String,
        opt_in_env: &'static str,
    },
    /// An embedder replaced the static composition through the public hook.
    EmbedderComposer,
    /// The structured user-global constitution file.
    UserGlobalConstitution { path: String },
    /// Repo-local law or project instructions read from the workspace.
    Workspace { path: String },
    /// Generated in memory from the workspace (no file backs it).
    WorkspaceGenerated,
    /// A marked, volatile WorldState fragment below the cache boundary.
    WorldStateFragment { marker: String },
    /// Composed from several of the above; the label names which.
    Composed { detail: String },
}

impl SegmentProvenance {
    /// Short label for the preview surface.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Bundled { symbol } => format!("bundled ({symbol})"),
            Self::ConfigOverride { path, opt_in_env } => {
                format!("config override {path} (opted in via {opt_in_env})")
            }
            Self::EmbedderComposer => "embedder composer hook".to_string(),
            Self::UserGlobalConstitution { path } => format!("user-global constitution {path}"),
            Self::Workspace { path } => format!("workspace {path}"),
            Self::WorkspaceGenerated => "workspace-generated (no file)".to_string(),
            Self::WorldStateFragment { marker } => format!("world-state fragment {marker}"),
            Self::Composed { detail } => format!("composed: {detail}"),
        }
    }

    /// True when these bytes belong to the cache-stable prefix. WorldState
    /// fragments drift turn-over-turn and are not cacheable.
    #[must_use]
    pub fn is_cache_stable(&self) -> bool {
        !matches!(self, Self::WorldStateFragment { .. })
    }
}

/// One system block, measured and attributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSegment {
    /// 0-based position in the assembled prompt.
    pub index: usize,
    pub label: String,
    pub provenance: SegmentProvenance,
    /// Measures over the **real** bytes, before redaction.
    pub measures: Measures,
    /// Digest over the real bytes.
    pub digest: String,
    /// Display text, redacted. May differ from the real bytes; `redacted` says so.
    pub text: String,
    pub redacted: bool,
}

/// The exact effective base prompt for the next turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasePromptPreview {
    pub segments: Vec<PreviewSegment>,
    /// Totals over every segment's real bytes.
    pub total: Measures,
    /// Totals over the cache-stable prefix only.
    pub cache_stable: Measures,
    /// Digest of the full assembled, unredacted prompt text. This is the
    /// identity check: it must equal the digest of what is actually sent.
    pub exact_digest: String,
    /// How many segments were redacted for display.
    pub redacted_segments: usize,
}

impl BasePromptPreview {
    /// True when nothing needed masking, so the displayed text is byte-exact.
    #[must_use]
    pub fn display_is_exact(&self) -> bool {
        self.redacted_segments == 0
    }

    /// Rejoin the displayed segments the same way the prompt is assembled.
    #[must_use]
    pub fn display_text(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Which source the effective base prompt currently comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BasePromptSource {
    Bundled,
    ConfigOverride { path: String },
    EmbedderComposer,
}

impl BasePromptSource {
    fn provenance(&self) -> SegmentProvenance {
        match self {
            Self::Bundled => SegmentProvenance::Bundled {
                symbol: "BASE_PROMPT",
            },
            Self::ConfigOverride { path } => SegmentProvenance::ConfigOverride {
                path: path.clone(),
                opt_in_env: super::BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
            },
            Self::EmbedderComposer => SegmentProvenance::EmbedderComposer,
        }
    }
}

/// Sources the caller already knows, so the preview never guesses.
///
/// Everything here is provenance metadata. Supplying it wrong makes the labels
/// wrong; it can never change the previewed bytes, which come from the
/// `SystemPrompt` alone.
#[derive(Debug, Clone, Default)]
pub struct PreviewSources<'a> {
    /// Where the effective base prompt came from. `None` means bundled.
    pub base_prompt: Option<BasePromptSource>,
    /// Path of the loaded user-global constitution, when one is active.
    pub user_constitution_path: Option<&'a Path>,
    /// Workspace root, used to shorten workspace-relative paths in display text.
    pub workspace: Option<&'a Path>,
    /// Home directory, masked to `~` in display text.
    pub home: Option<&'a Path>,
}

/// Build the exact effective base-prompt preview.
///
/// Pure over `prompt`: no I/O beyond the caller-supplied path metadata, no
/// provider call, no tool expansion.
#[must_use]
pub fn preview(prompt: &SystemPrompt, sources: &PreviewSources<'_>) -> BasePromptPreview {
    let blocks: Vec<SystemBlock> = match prompt {
        SystemPrompt::Text(text) => vec![SystemBlock {
            block_type: "text".to_string(),
            text: text.clone(),
            cache_control: None,
        }],
        SystemPrompt::Blocks(blocks) => blocks.clone(),
    };

    let mut segments = Vec::with_capacity(blocks.len());
    let mut total = Measures::default();
    let mut cache_stable = Measures::default();
    let mut redacted_segments = 0;

    for (index, block) in blocks.iter().enumerate() {
        let provenance = classify(index, &block.text, sources);
        let measures = Measures::of(&block.text);
        let display = redact(&block.text, sources);
        let redacted = display != block.text;
        if redacted {
            redacted_segments += 1;
        }
        total = total.add(measures);
        if provenance.is_cache_stable() {
            cache_stable = cache_stable.add(measures);
        }
        segments.push(PreviewSegment {
            index,
            label: segment_label(index, &block.text),
            provenance,
            measures,
            digest: digest(&block.text),
            text: display,
            redacted,
        });
    }

    let exact = super::system_prompt_flat_text(prompt);
    BasePromptPreview {
        segments,
        total,
        cache_stable,
        exact_digest: digest(&exact),
        redacted_segments,
    }
}

/// Render the preview as the pager body: a provenance/measure table, then the
/// segments themselves.
#[must_use]
pub fn render_report(preview: &BasePromptPreview) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("Exact effective base prompt for the next turn\n\n");
    let _ = writeln!(
        out,
        "Total: {} bytes, {} chars, ~{} tokens (estimate) across {} block(s)",
        preview.total.byte_len,
        preview.total.char_len,
        preview.total.approx_tokens,
        preview.segments.len()
    );
    let _ = writeln!(
        out,
        "Cache-stable prefix: {} bytes, ~{} tokens (estimate)",
        preview.cache_stable.byte_len, preview.cache_stable.approx_tokens
    );
    let _ = writeln!(
        out,
        "Digest of the exact sent bytes: {}",
        preview.exact_digest
    );
    if preview.display_is_exact() {
        out.push_str("Displayed text is byte-exact; nothing needed redaction.\n");
    } else {
        let _ = writeln!(
            out,
            "{} block(s) are redacted below for display. Measures and digests are over the real bytes.",
            preview.redacted_segments
        );
    }
    out.push_str(
        "\nNo provider request was made and no tool catalog was expanded to build this preview.\n",
    );

    out.push_str("\nBlocks\n");
    for segment in &preview.segments {
        let _ = writeln!(
            out,
            "- [{}] {} — {} — {} bytes, ~{} tokens, digest {}{}",
            segment.index,
            segment.label,
            segment.provenance.label(),
            segment.measures.byte_len,
            segment.measures.approx_tokens,
            segment.digest,
            if segment.redacted { " (redacted)" } else { "" }
        );
    }

    for segment in &preview.segments {
        let _ = write!(
            out,
            "\n--- [{}] {} ---\n{}\n",
            segment.index, segment.label, segment.text
        );
    }
    out
}

fn segment_label(index: usize, text: &str) -> String {
    if let Some(marker) = world_state_marker(text) {
        return format!("world state: {marker}");
    }
    if index == 0 {
        return "constitution prefix (cache-stable)".to_string();
    }
    if text.starts_with("## Authority Recap") {
        return "authority recap trailer".to_string();
    }
    format!("trailer block {index}")
}

fn classify(index: usize, text: &str, sources: &PreviewSources<'_>) -> SegmentProvenance {
    if let Some(marker) = world_state_marker(text) {
        return SegmentProvenance::WorldStateFragment { marker };
    }
    if index > 0 {
        return SegmentProvenance::Bundled {
            symbol: "AUTHORITY_RECAP / locale trailer",
        };
    }

    // Block 0 is the composed cache-stable prefix. Name every source that
    // actually contributed rather than pretending it is one file.
    let base = sources
        .base_prompt
        .clone()
        .unwrap_or(BasePromptSource::Bundled);
    let mut parts = vec![base.provenance().label()];
    if text.contains("<codewhale_user_constitution") {
        parts.push(match sources.user_constitution_path {
            Some(path) => SegmentProvenance::UserGlobalConstitution {
                path: display_path(path, sources),
            }
            .label(),
            // The block is present but the caller did not tell us which file it
            // came from. Say that, rather than naming a path we did not read.
            None => "user-global constitution (path not reported)".to_string(),
        });
    }
    if text.contains("<project_context") || text.contains("## Project Context") {
        parts.push(SegmentProvenance::WorkspaceGenerated.label());
    }
    if parts.len() == 1 {
        return base.provenance();
    }
    SegmentProvenance::Composed {
        detail: parts.join(" + "),
    }
}

fn world_state_marker(text: &str) -> Option<String> {
    let first = text.lines().next()?.trim();
    let inner = first.strip_prefix("<!-- cw:ctx:")?.strip_suffix("-->")?;
    Some(inner.trim().to_string())
}

fn display_path(path: &Path, sources: &PreviewSources<'_>) -> String {
    let text = path.display().to_string();
    redact(&text, sources)
}

/// Mask home paths, key-shaped tokens, and URL userinfo for display.
///
/// Deliberately conservative and local: the preview shows prompt bytes, so it
/// must not become a way to read a secret that leaked into one.
fn redact(text: &str, sources: &PreviewSources<'_>) -> String {
    let mut out = text.to_string();
    if let Some(home) = sources.home {
        let home = home.display().to_string();
        if !home.is_empty() && home != "/" {
            out = out.replace(&home, "~");
        }
    }
    out = redact_userinfo(&out);
    redact_key_shaped(&out)
}

/// Replace `scheme://user:secret@host` with `scheme://***@host`.
fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '/' || c == '"')
            .unwrap_or(tail.len());
        let (authority, remainder) = tail.split_at(end);
        match authority.rsplit_once('@') {
            Some((_, host)) => {
                out.push_str("***@");
                out.push_str(host);
            }
            None => out.push_str(authority),
        }
        rest = remainder;
    }
    out.push_str(rest);
    out
}

/// Mask long opaque tokens that look like credentials (`sk-…`, `ghp_…`, bearer
/// values). Ordinary prose and paths are left alone.
fn redact_key_shaped(text: &str) -> String {
    const PREFIXES: &[&str] = &["sk-", "sk_", "ghp_", "gho_", "xoxb-", "xoxp-", "Bearer "];
    let mut out = String::with_capacity(text.len());
    for token in text.split_inclusive(char::is_whitespace) {
        let trimmed = token.trim_end();
        let looks_secret = PREFIXES
            .iter()
            .any(|prefix| trimmed.starts_with(prefix) && trimmed.len() > prefix.len() + 8);
        if looks_secret {
            out.push_str("[redacted]");
            out.push_str(&token[trimmed.len()..]);
        } else {
            out.push_str(token);
        }
    }
    out
}

/// FNV-1a 64-bit, hex. Same construction the constitution projection uses, so
/// digests from the two surfaces are comparable.
fn digest(text: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in text.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(texts: &[&str]) -> SystemPrompt {
        SystemPrompt::Blocks(
            texts
                .iter()
                .map(|text| SystemBlock {
                    block_type: "text".to_string(),
                    text: (*text).to_string(),
                    cache_control: None,
                })
                .collect(),
        )
    }

    #[test]
    fn preview_digest_equals_the_exact_sent_prompt() {
        let prompt = blocks(&[
            "# Constitution\nBe truthful.",
            "<!-- cw:ctx:workspace -->\ncwd: /tmp",
            "## Authority Recap\nConsult Whose word wins.",
        ]);
        let preview = preview(&prompt, &PreviewSources::default());

        let exact = crate::prompts::system_prompt_flat_text(&prompt);
        assert_eq!(preview.exact_digest, digest(&exact));
        // Nothing needed masking, so the display is byte-exact too.
        assert!(preview.display_is_exact());
        assert_eq!(preview.display_text(), exact);
        assert_eq!(preview.total.byte_len, exact.len() - 2 * "\n\n".len());
    }

    #[test]
    fn measures_are_per_block_and_sum_to_the_total() {
        let prompt = blocks(&["aaaa", "bbbbbbbb"]);
        let preview = preview(&prompt, &PreviewSources::default());
        assert_eq!(preview.segments[0].measures.byte_len, 4);
        assert_eq!(preview.segments[0].measures.approx_tokens, 1);
        assert_eq!(preview.segments[1].measures.approx_tokens, 2);
        assert_eq!(preview.total.byte_len, 12);
        assert_eq!(preview.total.approx_tokens, 3);
    }

    #[test]
    fn world_state_fragments_are_named_and_excluded_from_the_cache_stable_total() {
        let prompt = blocks(&[
            "# Constitution",
            "<!-- cw:ctx:route -->\nmodel: glm-5.2",
            "<!-- cw:ctx:token_budget -->\nrelay",
        ]);
        let preview = preview(&prompt, &PreviewSources::default());

        assert_eq!(
            preview.segments[1].provenance,
            SegmentProvenance::WorldStateFragment {
                marker: "route".to_string()
            }
        );
        assert!(preview.segments[1].label.contains("route"));
        assert!(!preview.segments[1].provenance.is_cache_stable());
        assert_eq!(
            preview.cache_stable.byte_len,
            preview.segments[0].measures.byte_len
        );
        assert!(preview.cache_stable.byte_len < preview.total.byte_len);
    }

    #[test]
    fn bundled_and_overridden_base_prompts_are_labeled_differently() {
        let prompt = blocks(&["# Constitution"]);

        let bundled = preview(&prompt, &PreviewSources::default());
        assert_eq!(
            bundled.segments[0].provenance,
            SegmentProvenance::Bundled {
                symbol: "BASE_PROMPT"
            }
        );
        assert!(bundled.segments[0].provenance.label().contains("bundled"));

        let overridden = preview(
            &prompt,
            &PreviewSources {
                base_prompt: Some(BasePromptSource::ConfigOverride {
                    path: "/home/u/.codewhale/prompts/constitution.md".to_string(),
                }),
                ..PreviewSources::default()
            },
        );
        let label = overridden.segments[0].provenance.label();
        assert!(label.contains("config override"), "{label}");
        assert!(
            label.contains(super::super::BASE_PROMPT_OVERRIDE_OPT_IN_ENV),
            "the opt-in gate that allowed the override must be named: {label}"
        );

        let embedder = preview(
            &prompt,
            &PreviewSources {
                base_prompt: Some(BasePromptSource::EmbedderComposer),
                ..PreviewSources::default()
            },
        );
        assert_eq!(
            embedder.segments[0].provenance,
            SegmentProvenance::EmbedderComposer
        );
    }

    #[test]
    fn constitution_block_provenance_never_invents_a_path() {
        let prompt = blocks(&[
            "# Constitution\n\n<codewhale_user_constitution source=\"user-global\">\nx\n</codewhale_user_constitution>",
        ]);
        let unknown = preview(&prompt, &PreviewSources::default());
        let label = unknown.segments[0].provenance.label();
        assert!(label.contains("path not reported"), "{label}");
        assert!(!label.contains("crates/"), "no source-tree path: {label}");

        let known = preview(
            &prompt,
            &PreviewSources {
                user_constitution_path: Some(Path::new("/home/u/.codewhale/constitution.json")),
                ..PreviewSources::default()
            },
        );
        assert!(
            known.segments[0]
                .provenance
                .label()
                .contains("constitution.json")
        );
    }

    #[test]
    fn redaction_masks_secrets_without_lying_about_size() {
        let raw = "key sk-abcdefghijklmnop and https://user:pw@example.com/x";
        let prompt = blocks(&[raw]);
        let preview = preview(&prompt, &PreviewSources::default());
        let segment = &preview.segments[0];

        assert!(segment.redacted);
        assert!(!segment.text.contains("sk-abcdefghijklmnop"));
        assert!(!segment.text.contains("pw@"));
        assert!(segment.text.contains("***@example.com"));
        // Measures and digest still describe the real bytes.
        assert_eq!(segment.measures.byte_len, raw.len());
        assert_eq!(segment.digest, digest(raw));
        assert_eq!(preview.exact_digest, digest(raw));
        assert!(!preview.display_is_exact());
    }

    #[test]
    fn home_paths_are_masked_in_display_only() {
        let raw = "constitution at /Users/someone/.codewhale/constitution.json";
        let prompt = blocks(&[raw]);
        let preview = preview(
            &prompt,
            &PreviewSources {
                home: Some(Path::new("/Users/someone")),
                ..PreviewSources::default()
            },
        );
        assert!(preview.segments[0].text.contains("~/.codewhale"));
        assert!(!preview.segments[0].text.contains("/Users/someone"));
        assert_eq!(preview.segments[0].measures.byte_len, raw.len());
    }

    #[test]
    fn report_states_the_no_request_no_tool_expansion_boundary() {
        let prompt = blocks(&["# Constitution"]);
        let report = render_report(&preview(&prompt, &PreviewSources::default()));
        assert!(report.contains("No provider request was made"));
        assert!(report.contains("no tool catalog was expanded"));
        assert!(report.contains("Digest of the exact sent bytes"));
        assert!(
            report.contains("~"),
            "token estimate must be marked approximate"
        );
    }

    #[test]
    fn text_prompts_preview_as_one_segment() {
        let prompt = SystemPrompt::Text("flat prompt".to_string());
        let preview = preview(&prompt, &PreviewSources::default());
        assert_eq!(preview.segments.len(), 1);
        assert_eq!(preview.exact_digest, digest("flat prompt"));
    }
}
