//! Typed, bounded, redaction-aware desktop notification payloads (#4834).
//!
//! Before this module every desktop notification was a single free-form
//! `String` assembled at the call site and handed straight to the OS.
//! Notification Center on macOS (and the equivalent surface behind OSC 9 /
//! OSC 99 / OSC 777) is lock-screen capable: whatever happened to be in
//! that string — a pasted API key, an absolute path that names the user
//! and their client, the full shell command awaiting approval — was
//! rendered verbatim to anyone looking at the machine.
//!
//! [`NotificationPayload`] replaces the string with a closed set of event
//! kinds and three bounded fields:
//!
//! | field      | max chars | contents                                  |
//! |------------|-----------|-------------------------------------------|
//! | `headline` | 80        | localized event label (+ elapsed/cost)     |
//! | `detail`   | 120       | short, event-specific identifier           |
//! | `preview`  | 200       | assistant text — two kinds only            |
//!
//! Every field passes through [`sanitize_field`], which strips control
//! bytes, collapses newlines and whitespace runs, and redacts credentials,
//! absolute local paths, and structured tool input. There is no
//! constructor that bypasses it, and `preview` is gated by
//! [`NotificationKind::allows_preview`] rather than by the caller.
//!
//! ## What each kind is allowed to show
//!
//! - [`NotificationKind::TurnComplete`] — the localized "Turn complete"
//!   headline (plus elapsed/cost when `include_summary` is on) and a
//!   preview of the assistant's own reply. Unchanged in spirit from the
//!   previous behavior; now bounded and redacted.
//! - [`NotificationKind::SubagentTerminal`] — localized status headline,
//!   the sub-agent's display name as detail, and a preview of the child's summary
//!   line.
//! - [`NotificationKind::BackgroundTerminal`] — shell/task or mixed batch
//!   headline and labels only. Commands, task prompts and errors stay in the UI;
//!   this kind never carries a preview, even if an agent is in the batch.
//! - [`NotificationKind::ApprovalNeeded`] — headline plus the *tool name*.
//!   Never the tool description or arguments: an approval prompt fires
//!   precisely when those arguments are untrusted, and the previous code
//!   put the full description on the lock screen.
//! - [`NotificationKind::InputNeeded`] — headline only. The question text
//!   stays in the terminal.
//! - [`NotificationKind::ElevationNeeded`] — headline plus tool name and
//!   the sandbox denial reason. The reason is engine-authored but not a
//!   closed vocabulary, so it is sanitized like everything else.
//! - [`NotificationKind::ModelNotify`] — the model-callable `notify` tool.
//!   Title and body are model-authored, so they are the least trusted
//!   input here and carry no preview on top.

use std::sync::OnceLock;

use regex::Regex;

/// Maximum characters in the headline (the macOS subtitle line).
pub const HEADLINE_MAX_CHARS: usize = 80;
/// Maximum characters in the detail line.
pub const DETAIL_MAX_CHARS: usize = 120;
/// Maximum characters in the assistant preview.
pub const PREVIEW_MAX_CHARS: usize = 200;

/// Separator between the detail and preview segments of a rendered body.
const BODY_SEPARATOR: &str = " — ";

/// Placeholder substituted for anything that must never reach a
/// lock-screen-capable surface.
pub const REDACTED: &str = "[redacted]";
/// Placeholder substituted for structured tool input/output.
pub const HIDDEN_DETAILS: &str = "[details hidden]";

/// Fallback headline when sanitization leaves nothing behind.
const FALLBACK_HEADLINE: &str = "codewhale";

/// The closed set of events that can produce a desktop notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    /// An agent turn finished successfully.
    TurnComplete,
    /// A sub-agent reached a terminal status (complete/failed/cancelled/…).
    SubagentTerminal,
    /// Shell/task completion, possibly batched with agents; no raw previews.
    BackgroundTerminal,
    /// A tool call is blocked waiting for the user to approve it.
    ApprovalNeeded,
    /// The agent asked the user a question and is blocked on the answer.
    InputNeeded,
    /// The sandbox denied an operation and the user must elevate.
    ElevationNeeded,
    /// The model called the `notify` tool.
    ModelNotify,
}

impl NotificationKind {
    /// Whether this kind may carry assistant preview text at all.
    ///
    /// Interactive prompts (approval/input/elevation) never do: the whole
    /// point of the prompt is that the pending content is not yet trusted.
    /// `ModelNotify` does not either — its body *is* model-authored text
    /// and already occupies the body budget.
    #[must_use]
    pub const fn allows_preview(self) -> bool {
        matches!(self, Self::TurnComplete | Self::SubagentTerminal)
    }
}

/// A bounded, sanitized notification ready to hand to the OS.
///
/// Construct via the per-kind constructors; every one of them sanitizes
/// and truncates. There is no way to smuggle raw text through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPayload {
    kind: NotificationKind,
    headline: String,
    detail: Option<String>,
    preview: Option<String>,
}

impl NotificationPayload {
    fn new(kind: NotificationKind, headline: &str, detail: Option<&str>) -> Self {
        let headline = bounded(headline, HEADLINE_MAX_CHARS);
        Self {
            kind,
            headline: if headline.is_empty() {
                FALLBACK_HEADLINE.to_string()
            } else {
                headline
            },
            detail: detail
                .map(|d| bounded(d, DETAIL_MAX_CHARS))
                .filter(|d| !d.is_empty()),
            preview: None,
        }
    }

    /// Turn finished. `headline` is the already-localized status line
    /// (optionally carrying elapsed/cost when `include_summary` is on).
    #[must_use]
    pub fn turn_complete(headline: &str) -> Self {
        Self::new(NotificationKind::TurnComplete, headline, None)
    }

    /// Sub-agent reached a terminal status. `detail` is the agent's display
    /// name, the one every other surface shows.
    #[must_use]
    pub fn subagent_terminal(headline: &str, agent_name: &str) -> Self {
        Self::new(
            NotificationKind::SubagentTerminal,
            headline,
            Some(agent_name),
        )
    }

    /// Finished background work. Callers supply only display labels, never
    /// commands or task prompts; output and errors remain in the terminal.
    #[must_use]
    pub fn background_terminal(headline: &str, labels: &str) -> Self {
        Self::new(NotificationKind::BackgroundTerminal, headline, Some(labels))
    }

    /// A tool call needs approval. Only the tool *name* is disclosed —
    /// never the description or the arguments.
    #[must_use]
    pub fn approval_needed(headline: &str, tool_name: &str) -> Self {
        Self::new(NotificationKind::ApprovalNeeded, headline, Some(tool_name))
    }

    /// The agent is blocked on a user answer. The question stays in the
    /// terminal; the banner only says "come back".
    #[must_use]
    pub fn input_needed(headline: &str) -> Self {
        Self::new(NotificationKind::InputNeeded, headline, None)
    }

    /// The sandbox denied an operation and the user must decide whether
    /// to elevate.
    #[must_use]
    pub fn elevation_needed(headline: &str, tool_name: &str, reason: &str) -> Self {
        let detail = if reason.trim().is_empty() {
            tool_name.to_string()
        } else {
            format!("{tool_name}{BODY_SEPARATOR}{reason}")
        };
        Self::new(NotificationKind::ElevationNeeded, headline, Some(&detail))
    }

    /// The model-callable `notify` tool. Both fields are model-authored
    /// and therefore fully sanitized like everything else.
    #[must_use]
    pub fn model_notify(title: &str, body: Option<&str>) -> Self {
        Self::new(NotificationKind::ModelNotify, title, body)
    }

    /// Attach assistant preview text.
    ///
    /// A no-op unless the kind permits a preview. Callers cannot override
    /// the kind policy — routing every preview through this method is
    /// what makes "approval banners never show the command" a type-level
    /// property instead of a call-site convention.
    #[must_use]
    pub fn with_preview(mut self, preview: Option<&str>) -> Self {
        if !self.kind.allows_preview() {
            return self;
        }
        self.preview = preview
            .map(|p| bounded(p, PREVIEW_MAX_CHARS))
            .filter(|p| !p.is_empty());
        self
    }

    /// The event kind.
    #[must_use]
    pub const fn kind(&self) -> NotificationKind {
        self.kind
    }

    /// Bounded, sanitized headline. Never empty.
    #[must_use]
    pub fn headline(&self) -> &str {
        &self.headline
    }

    /// Bounded, sanitized detail line, if the kind carries one.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Bounded, sanitized assistant preview. `None` unless the kind
    /// allows one and the caller supplied non-empty text.
    #[must_use]
    pub fn preview(&self) -> Option<&str> {
        self.preview.as_deref()
    }

    /// The body lines below the headline, joined for surfaces that take a
    /// single body string (macOS Notification Center).
    #[must_use]
    pub fn body(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(2);
        if let Some(detail) = self.detail() {
            parts.push(detail);
        }
        if let Some(preview) = self.preview() {
            parts.push(preview);
        }
        parts.join(BODY_SEPARATOR)
    }

    /// Single-line rendering for terminal escape protocols (OSC 9 / 99 /
    /// 777), which cannot express a title/subtitle/body hierarchy.
    #[must_use]
    pub fn render_inline(&self) -> String {
        let body = self.body();
        if body.is_empty() {
            self.headline.clone()
        } else {
            format!("{}: {body}", self.headline)
        }
    }
}

/// Sanitize then truncate to `max_chars`, appending an ellipsis when the
/// input was longer. Character-based, not byte-based, so multi-byte text
/// is never sliced mid-scalar.
fn bounded(text: &str, max_chars: usize) -> String {
    truncate_chars(&sanitize_field(text), max_chars)
}

/// Truncate to `max_chars` characters *inclusive* of the `...` marker, so
/// the result never exceeds the declared bound.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let take = max_chars.saturating_sub(3);
    let mut out: String = text.chars().take(take).collect();
    out.push_str("...");
    out
}

/// Strip control bytes and redact anything that must not reach a
/// lock-screen-capable surface.
///
/// Redaction runs per line so a credential cannot be hidden by wrapping,
/// then the lines are joined into one bounded field.
#[must_use]
pub fn sanitize_field(text: &str) -> String {
    // Strip whole escape sequences *before* `sanitize_stream_chunk`, which
    // drops the ESC byte but leaves the parameter tail behind — good
    // enough for a terminal that will never re-interpret it, wrong for a
    // notification banner that would render a literal `[31m`.
    codewhale_secrets::sanitize::sanitize_text(&strip_escape_sequences(text))
        .lines()
        .map(|line| {
            let redacted = redact_structured(line.trim());
            let redacted = redact_credentials(&redacted);
            let redacted = redact_absolute_paths(&redacted);
            // Collapse whitespace runs so a bounded field cannot be
            // padded out with invisible filler.
            redacted.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn regex_cache<const N: usize>(
    cell: &'static OnceLock<Vec<Regex>>,
    patterns: [&str; N],
) -> &'static [Regex] {
    cell.get_or_init(|| {
        patterns
            .iter()
            .map(|p| Regex::new(p).expect("static notification redaction pattern must compile"))
            .collect()
    })
}

/// Remove complete ANSI escape sequences (CSI, OSC, and single-character
/// escapes) so neither the sequence nor its parameter tail survives into a
/// notification field.
fn strip_escape_sequences(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = regex_cache(
        &PATTERNS,
        [
            // OSC: ESC ] … terminated by BEL or ST.
            r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?",
            // CSI: ESC [ params intermediates final.
            r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]?",
            // Any remaining two-character escape.
            r"\x1b.",
        ],
    );
    let mut out = text.to_string();
    for re in res {
        out = re.replace_all(&out, "").into_owned();
    }
    out
}

/// Replace structured tool input/output (JSON objects and arrays) with a
/// placeholder. Raw tool arguments are the single most likely place for a
/// credential or a private path to appear verbatim.
fn redact_structured(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = regex_cache(
        &PATTERNS,
        [
            // A JSON-ish object: braces containing a `"key":` pair.
            r#"\{[^{}]*"[^"]*"\s*:[^{}]*\}"#,
            // A JSON-ish array of objects or quoted strings.
            r#"\[\s*(?:\{[^\[\]]*\}|"[^"]*"(?:\s*,\s*"[^"]*")*)\s*\]"#,
        ],
    );
    let mut out = text.to_string();
    for re in res {
        out = re.replace_all(&out, HIDDEN_DETAILS).into_owned();
    }
    // A field that is *entirely* a structured blob (possibly nested, so
    // the brace-matching patterns above may not have fired) is dropped
    // whole rather than partially rewritten.
    let trimmed = out.trim();
    if (trimmed.starts_with('{') || trimmed.starts_with('[')) && trimmed.contains('"') {
        return HIDDEN_DETAILS.to_string();
    }
    out
}

/// Replace credential-shaped substrings with [`REDACTED`].
///
/// This is deliberately over-eager: a notification banner is a glance
/// surface, so losing a long opaque identifier costs almost nothing while
/// leaking one is unrecoverable.
fn redact_credentials(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = regex_cache(
        &PATTERNS,
        [
            // PEM private key headers.
            r"-----BEGIN[A-Z ]*PRIVATE KEY-----",
            // Provider-prefixed keys: OpenAI/Anthropic/DeepSeek style
            // `sk-…`, GitHub `ghp_/gho_/ghu_/ghs_/ghr_`, AWS `AKIA…`,
            // Slack `xoxb-…`, Google `AIza…`.
            r"(?i)\bsk-[A-Za-z0-9_\-]{8,}",
            r"\bgh[pousr]_[A-Za-z0-9]{16,}",
            r"\bAKIA[0-9A-Z]{12,}",
            r"(?i)\bxox[baprse]-[A-Za-z0-9\-]{8,}",
            r"\bAIza[0-9A-Za-z_\-]{20,}",
            // `Bearer <token>` / `Basic <token>` authorization values.
            r"(?i)\b(?:bearer|basic)\s+[A-Za-z0-9_\-\.=+/]{8,}",
            // `NAME=value` / `name: value` where the name says secret.
            r"(?i)\b[A-Za-z0-9_\-]*(?:api[_\-]?key|secret|token|password|passwd|credential)[A-Za-z0-9_\-]*\s*[:=]\s*\S+",
            // Long opaque blobs with no word structure.
            r"\b[A-Za-z0-9_\-]{40,}\b",
        ],
    );
    let mut out = text.to_string();
    for re in res {
        out = re.replace_all(&out, REDACTED).into_owned();
    }
    out
}

/// Replace absolute local filesystem paths with `…/<basename>`.
///
/// The identifying information in `/Users/jane/clients/acme/contract.md`
/// is the prefix, not the leaf: it names the account, the machine layout,
/// and often the customer. Keeping only the basename preserves the "which
/// file?" utility of the banner while the identifying prefix never
/// reaches the lock screen.
///
/// URLs are left alone — the POSIX pattern only fires when the slash run
/// is not preceded by `:` or another `/`.
fn redact_absolute_paths(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = regex_cache(
        &PATTERNS,
        [
            // Windows UNC and extended paths reveal server/share names.
            r"(^|[^A-Za-z0-9_\\])(\\\\(?:\?\\)?[^\\/\s]+\\[^\\/\s]+(?:\\[^\\/\s]*)*)",
            // Leave the double slash after a URL scheme alone.
            r"(^|[^A-Za-z0-9_:/])(//[^/\s]+/[^/\s]+(?:/[^/\s]*)*)",
            // POSIX: at least two components so a bare `/tmp` or a lone
            // slash in prose is not mangled.
            r"(^|[^A-Za-z0-9_:/\\])((?:/[A-Za-z0-9._~%+@\-]+){2,}/?)",
            // Windows drive-letter paths.
            r"(^|[^A-Za-z0-9_])([A-Za-z]:[\\/](?:[^\\/:*?<>|\s]+[\\/]?)+)",
        ],
    );
    let mut out = text.to_string();
    for re in res {
        out = re
            .replace_all(&out, |caps: &regex::Captures<'_>| {
                let lead = caps.get(1).map_or("", |m| m.as_str());
                let path = caps.get(2).map_or("", |m| m.as_str());
                let basename = path
                    .trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or_default();
                if basename.is_empty() {
                    format!("{lead}…")
                } else {
                    format!("{lead}…/{basename}")
                }
            })
            .into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One payload of every kind, fed pathological input. This is the
    /// enumeration test the issue asks for: if a new kind is added
    /// without a bound, this array stops compiling or the assertion
    /// fires.
    fn every_kind(text: &str) -> Vec<NotificationPayload> {
        vec![
            NotificationPayload::turn_complete(text).with_preview(Some(text)),
            NotificationPayload::subagent_terminal(text, text).with_preview(Some(text)),
            NotificationPayload::background_terminal(text, text).with_preview(Some(text)),
            NotificationPayload::approval_needed(text, text),
            NotificationPayload::input_needed(text),
            NotificationPayload::elevation_needed(text, text, text),
            NotificationPayload::model_notify(text, Some(text)),
        ]
    }

    #[test]
    fn every_kind_renders_within_declared_bounds() {
        for payload in every_kind(&"word ".repeat(400)) {
            assert!(
                payload.headline().chars().count() <= HEADLINE_MAX_CHARS,
                "{:?} headline unbounded: {}",
                payload.kind(),
                payload.headline()
            );
            assert!(
                payload
                    .detail()
                    .is_none_or(|d| d.chars().count() <= DETAIL_MAX_CHARS),
                "{:?} detail unbounded",
                payload.kind()
            );
            assert!(
                payload
                    .preview()
                    .is_none_or(|p| p.chars().count() <= PREVIEW_MAX_CHARS),
                "{:?} preview unbounded",
                payload.kind()
            );
            assert!(!payload.headline().is_empty());
        }
    }

    /// The redaction guarantee, asserted for *every* event kind rather
    /// than one convenient constructor: a payload carrying an API key, an
    /// absolute local path, and raw tool JSON must leak none of them.
    #[test]
    fn no_kind_leaks_credentials_paths_or_raw_tool_input() {
        let hostile = concat!(
            "sk-proj-abc123DEF456ghi789jkl012 ",
            "wrote /Users/jane/clients/acme/contract.md ",
            r#"input {"command":"curl -H 'Authorization: Bearer abcdef123456'","cwd":"/Users/jane"}"#,
        );

        for payload in every_kind(hostile) {
            let rendered = payload.render_inline();
            for leak in [
                "sk-proj-abc123DEF456ghi789jkl012",
                "/Users/jane",
                "clients/acme",
                "Bearer abcdef123456",
                "\"command\"",
            ] {
                assert!(
                    !rendered.contains(leak),
                    "{:?} leaked {leak:?}: {rendered}",
                    payload.kind()
                );
            }
        }
    }

    #[test]
    fn bounds_are_char_based_not_byte_based() {
        let payload = NotificationPayload::turn_complete(&"日".repeat(200));
        assert_eq!(payload.headline().chars().count(), HEADLINE_MAX_CHARS);
        assert!(payload.headline().ends_with("..."));
    }

    #[test]
    fn preview_is_kind_gated_not_caller_gated() {
        let on = NotificationPayload::turn_complete("Turn complete")
            .with_preview(Some("assistant said something"));
        assert_eq!(on.preview(), Some("assistant said something"));

        // Prompt kinds refuse a preview no matter what the caller does.
        for payload in [
            NotificationPayload::background_terminal("Shell failed", "shell"),
            NotificationPayload::approval_needed("Approval needed", "bash"),
            NotificationPayload::input_needed("Input needed"),
            NotificationPayload::elevation_needed("Elevation needed", "bash", "network blocked"),
            NotificationPayload::model_notify("Build done", None),
        ] {
            let kind = payload.kind();
            assert_eq!(
                payload.with_preview(Some("leaky")).preview(),
                None,
                "{kind:?} must never carry assistant preview"
            );
        }
    }

    /// #4834: the approval banner used to render
    /// `Approval needed: {tool} - {description}`, where the description
    /// is the pending shell command. Only the tool name survives.
    #[test]
    fn approval_payload_carries_only_the_tool_name() {
        let payload = NotificationPayload::approval_needed("Approval needed", "bash");
        assert_eq!(payload.detail(), Some("bash"));
        assert_eq!(payload.render_inline(), "Approval needed: bash");
    }

    #[test]
    fn input_needed_body_is_empty() {
        let payload = NotificationPayload::input_needed("Input needed");
        assert_eq!(payload.detail(), None);
        assert_eq!(payload.body(), "");
        assert_eq!(payload.render_inline(), "Input needed");
    }

    #[test]
    fn api_keys_are_redacted() {
        let cases = [
            "here is the key sk-proj-abc123DEF456ghi789jkl012",
            "token ghp_0123456789abcdefghijABCDEFGHIJ0123",
            "aws AKIAIOSFODNN7EXAMPLE",
            "slack xoxb-1234567890-abcdefghij",
            "google AIzaSyA1234567890abcdefghijklmnopqrstu",
            // Deliberately NOT a JWT-shaped literal. The obvious fixture here
            // is the textbook base64 JWT header, but that is exactly what
            // secret scanners match: it fired a bearer-token incident on the
            // first push of this branch and trains people to ignore the
            // scanner. What this case actually exercises is the
            // `Bearer <value>` authorization rule, which does not care about
            // the value's shape.
            "Authorization: Bearer not-a-real-token-0123456789abcdef",
            "DEEPSEEK_API_KEY=sk-livekeyvalue1234567890",
            "password: hunter2correctbattery",
            "-----BEGIN RSA PRIVATE KEY-----",
        ];
        for case in cases {
            let payload = NotificationPayload::model_notify("Heads up", Some(case));
            let body = payload.body();
            assert!(
                body.contains(REDACTED),
                "expected redaction marker for {case:?}, got {body:?}"
            );
            for leak in [
                "sk-proj-abc123DEF456ghi789jkl012",
                "ghp_0123456789abcdefghijABCDEFGHIJ0123",
                "AKIAIOSFODNN7EXAMPLE",
                "xoxb-1234567890-abcdefghij",
                "AIzaSyA1234567890abcdefghijklmnopqrstu",
                "not-a-real-token-0123456789abcdef",
                "sk-livekeyvalue1234567890",
                "hunter2correctbattery",
                "PRIVATE KEY",
            ] {
                assert!(
                    !body.contains(leak),
                    "leaked {leak:?} from {case:?}: {body:?}"
                );
            }
        }
    }

    /// Deliberate over-eagerness, pinned so it is a decision and not a
    /// surprise: an unbroken 40+ character run has no word structure, so
    /// it is treated as credential-shaped even when it is not.
    #[test]
    fn long_opaque_runs_are_treated_as_credential_shaped() {
        let payload = NotificationPayload::turn_complete("Turn complete")
            .with_preview(Some(&"a".repeat(500)));
        assert_eq!(payload.preview(), Some(REDACTED));
    }

    #[test]
    fn absolute_paths_are_reduced_to_basename() {
        let payload = NotificationPayload::turn_complete("Turn complete").with_preview(Some(
            "wrote /Users/jane/clients/acme/contract.md and C:\\Users\\jane\\secret\\plan.docx",
        ));
        let preview = payload.preview().expect("preview should survive");
        assert!(!preview.contains("/Users/jane"), "{preview}");
        assert!(!preview.contains("clients/acme"), "{preview}");
        assert!(!preview.contains("C:\\Users"), "{preview}");
        assert!(preview.contains("…/contract.md"), "{preview}");
        assert!(preview.contains("…/plan.docx"), "{preview}");
    }

    #[test]
    fn urls_survive_path_redaction() {
        let payload = NotificationPayload::model_notify(
            "Deployed",
            Some("live at https://app.example.com/status/ok"),
        );
        assert!(
            payload.body().contains("https://app.example.com/status/ok"),
            "{}",
            payload.body()
        );
    }

    #[test]
    fn raw_tool_input_json_is_hidden() {
        let raw =
            r#"{"command":"curl -H 'Authorization: Bearer abc' https://x","cwd":"/Users/jane"}"#;
        let payload = NotificationPayload::model_notify("Ran tool", Some(raw));
        let body = payload.body();
        assert_eq!(body, HIDDEN_DETAILS, "{body}");
    }

    #[test]
    fn embedded_tool_json_is_hidden_inline() {
        let payload = NotificationPayload::turn_complete("Turn complete").with_preview(Some(
            r#"called write with {"path":"/etc/passwd"} then stopped"#,
        ));
        let preview = payload.preview().expect("preview should survive");
        assert!(preview.contains(HIDDEN_DETAILS), "{preview}");
        assert!(!preview.contains("/etc/passwd"), "{preview}");
    }

    #[test]
    fn control_bytes_and_newlines_are_collapsed() {
        let payload = NotificationPayload::turn_complete("Turn\x1b[31m complete\n\nsecond line");
        assert_eq!(payload.headline(), "Turn complete second line");
    }

    #[test]
    fn empty_input_still_yields_a_headline() {
        let payload = NotificationPayload::turn_complete("   \n  ");
        assert_eq!(payload.headline(), FALLBACK_HEADLINE);
    }
    #[test]
    fn notification_payload_masks_unc_server_share_and_directories() {
        for path in [
            r"\\fileserver\share\clients\case.txt",
            r"\\?\C:\Users\fixture\clients\case.txt",
            "//fileserver/share/clients/case.txt",
        ] {
            let payload =
                NotificationPayload::turn_complete("Turn complete").with_preview(Some(path));
            let preview = payload.preview().expect("preview");
            assert_eq!(preview, "…/case.txt");
            assert!(!preview.contains("fileserver"));
            assert!(!preview.contains("clients"));
        }
        let payload = NotificationPayload::turn_complete("Turn complete")
            .with_preview(Some("see https://example.com/docs/page"));
        assert_eq!(payload.preview(), Some("see https://example.com/docs/page"));
    }
}
