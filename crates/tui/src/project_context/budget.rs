//! Per-scope instruction budgets, trim markers, and skip/trim notices (#6526).
//!
//! Standing instruction files are assembled into two independent scopes —
//! `global` (user-wide: the global `AGENTS.md` chain, `~/.claude/CLAUDE.md`,
//! and the configured `instructions = [...]` array) and `project` (the
//! repository-root → workspace chain, co-read foreign files, and the rules
//! block). Each scope has its own ceiling, expressed in **estimated tokens**
//! (`min(absolute_tokens, percent × usable_window)`), so a huge project can no
//! longer shrink the user's global preferences and vice versa.
//!
//! Every loss is reported. A scope that does not fit is trimmed from the
//! broadest end first — the root-most ancestor goes first, the workspace file
//! last — and a single file that alone exceeds its remaining budget keeps its
//! head and drops its tail with a marker carrying bytes and tokens. Skips are
//! not trims: an oversized file, an empty file, a rules-directory overflow, or
//! a symlinked duplicate is reported as `skipped` with the limit that produced
//! it, and never makes the workspace unopenable.
//!
//! Known limitations (deliberate, not bugs):
//!
//! - Token counts are *estimates* from
//!   [`crate::compaction::estimate_text_tokens_conservative`], not a
//!   provider tokenizer. The estimator reads multibyte text conservatively,
//!   but a CJK- or emoji-heavy file can still be off by a comparable margin;
//!   the notice says "estimated" for that reason.
//! - Opted-in foreign *fragments* (`.cursorrules`, `.clinerules`, …) are
//!   loaded by `codewhale_core::fragments` into the world-state block with
//!   their own cap. They are reported in `/context` but are not charged
//!   against the project scope here; only the instruction chain and the rules
//!   block are.
//! - The static-prefix guard measures every system block plus the tool
//!   schemas the request carries, so memory, environment and goal blocks count
//!   toward it even though only the two instruction scopes can be trimmed to
//!   satisfy it.
//! - Trimming is one pass per scope. The guard then makes one bounded pass
//!   over both scopes, largest first, with at most two attempts per scope. A
//!   prefix that is still over the guard afterwards is reported, not looped
//!   on, and host-owned prompts (child, isolated, RLM) are measured but never
//!   trimmed.

use std::path::PathBuf;

use codewhale_config::instruction_budget::{
    DEFAULT_FILE_MAX_BYTES, DEFAULT_SCOPE_BUDGET_PERCENT, DEFAULT_SCOPE_BUDGET_TOKENS,
    DEFAULT_STATIC_PREFIX_GUARD_PERCENT,
};
pub use codewhale_config::instruction_budget::{InstructionBudgetConfig, InstructionBudgetPolicy};
/// Share of the window kept free for the response and estimator drift.
const SAFETY_RESERVE_PERCENT: usize = 10;
/// Floor for [`SAFETY_RESERVE_PERCENT`] so small windows keep real headroom.
const SAFETY_RESERVE_MIN_TOKENS: usize = 2_048;

/// Which scope an instruction source belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(crate) enum InstructionScope {
    /// User-wide authority: global files and configured `instructions` entries.
    Global,
    /// Workspace authority: the per-directory chain and the rules block.
    Project,
}

impl InstructionScope {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// Where an instruction source came from, for notices and `/context`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum InstructionOrigin {
    /// An `AGENTS.md` (project chain or global chain).
    Agents,
    /// A `CLAUDE.md` / `.claude/CLAUDE.md` / `.claude/instructions.md`.
    Claude,
    /// Codewhale's own `.codewhale/instructions.md` or legacy
    /// `.deepseek/instructions.md`.
    Instructions,
    /// The assembled rules block.
    Rules,
    /// The configured `instructions = [...]` array.
    Configured,
}

impl InstructionOrigin {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Claude => "claude",
            Self::Instructions => "instructions",
            Self::Rules => "rules",
            Self::Configured => "configured",
        }
    }
}

/// What happened to one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum InstructionAction {
    Kept,
    Trimmed,
    Skipped,
    Refused,
}

impl InstructionAction {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Trimmed => "trimmed",
            Self::Skipped => "skipped",
            Self::Refused => "refused",
        }
    }
}

/// The limit that produced a loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum InstructionLimit {
    None,
    /// The scope's own token budget.
    ScopeBudget,
    /// The static-prefix guard trimmed the largest scope by an overage.
    StaticGuard,
    /// `file_max_bytes`.
    FileCeiling,
    FileContent,
    /// The file was whitespace-only.
    EmptyFile,
    /// `MAX_RULES_FILES` per directory.
    RulesCap,
    /// The project scope budget was exhausted before the rules block.
    RulesBudget,
    /// The same canonical file was already loaded in another scope.
    DuplicatePath,
}

impl InstructionLimit {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ScopeBudget => "scope_budget",
            Self::StaticGuard => "static_guard",
            Self::FileCeiling => "file_ceiling",
            Self::FileContent => "file_content",
            Self::EmptyFile => "empty_file",
            Self::RulesCap => "rules_cap",
            Self::RulesBudget => "rules_budget",
            Self::DuplicatePath => "duplicate_path",
        }
    }
}

/// One instruction source that was kept, trimmed, skipped, or refused.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct InstructionNotice {
    pub(crate) path: Option<PathBuf>,
    pub(crate) scope: InstructionScope,
    pub(crate) origin: InstructionOrigin,
    pub(crate) action: InstructionAction,
    pub(crate) bytes_kept: u64,
    pub(crate) bytes_dropped: u64,
    pub(crate) tokens_kept: usize,
    pub(crate) tokens_dropped: usize,
    pub(crate) limit: InstructionLimit,
    /// Lower-precedence file selected after the preferred one was skipped.
    pub(crate) fallback_selected: Option<String>,
    /// Typed reason for notices that are not a plain budget loss (a refused
    /// symlinked rules directory, an unreadable file, a cap).
    pub(crate) detail: Option<InstructionDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum InstructionDetail {
    DroppedBroadest,
    NoRoomForContent { budget_tokens: usize },
    RulesBudgetExhausted { budget_tokens: usize },
    OverFileCeiling { max_bytes: u64 },
    Unreadable { reason: String },
    AlreadyLoaded,
    RulesCap { max_files: usize },
    SymlinkedRulesDir,
}

fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let replacement = tail.find('}').and_then(|end| {
            values
                .iter()
                .find(|(key, _)| *key == &tail[..end])
                .map(|(_, value)| (*value, end))
        });
        match replacement {
            Some((value, end)) => {
                out.push_str(value);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('{');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

impl InstructionDetail {
    pub(crate) fn unreadable(error: &dyn std::fmt::Display) -> Self {
        Self::Unreadable {
            reason: codewhale_runtime::safe_label::safe_error_text(&error.to_string()),
        }
    }

    fn render(&self, locale: codewhale_localization::Locale) -> String {
        use codewhale_localization::{MessageId, tr};
        match self {
            Self::DroppedBroadest => {
                tr(locale, MessageId::InstructionDetailDroppedBroadest).into_owned()
            }
            Self::NoRoomForContent { budget_tokens } => fill(
                &tr(locale, MessageId::InstructionDetailNoRoom),
                &[("tokens", &budget_tokens.to_string())],
            ),
            Self::RulesBudgetExhausted { budget_tokens } => fill(
                &tr(locale, MessageId::InstructionDetailRulesExhausted),
                &[("tokens", &budget_tokens.to_string())],
            ),
            Self::OverFileCeiling { max_bytes } => fill(
                &tr(locale, MessageId::InstructionDetailOverCeiling),
                &[("bytes", &max_bytes.to_string())],
            ),
            Self::Unreadable { reason } => fill(
                &tr(locale, MessageId::InstructionDetailUnreadable),
                &[("reason", reason)],
            ),
            Self::AlreadyLoaded => {
                tr(locale, MessageId::InstructionDetailAlreadyLoaded).into_owned()
            }
            Self::RulesCap { max_files } => fill(
                &tr(locale, MessageId::InstructionDetailRulesCap),
                &[("count", &max_files.to_string())],
            ),
            Self::SymlinkedRulesDir => {
                tr(locale, MessageId::InstructionDetailSymlinkedRules).into_owned()
            }
        }
    }
}

const NOTICE_LABEL_MAX_CHARS: usize = 80;

fn inline_text(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .take(NOTICE_LABEL_MAX_CHARS)
        .collect()
}

impl InstructionNotice {
    pub(crate) fn loss(
        scope: InstructionScope,
        origin: InstructionOrigin,
        path: Option<PathBuf>,
        limit: InstructionLimit,
        action: InstructionAction,
    ) -> Self {
        Self {
            path,
            scope,
            origin,
            action,
            bytes_kept: 0,
            bytes_dropped: 0,
            tokens_kept: 0,
            tokens_dropped: 0,
            limit,
            fallback_selected: None,
            detail: None,
        }
    }

    /// Notice for the static-prefix guard: either the trim it performed or the
    /// terminal state where the prefix is still over the guard.
    pub(crate) fn static_guard(
        scope: InstructionScope,
        tokens_dropped: usize,
        unsatisfiable: bool,
    ) -> Self {
        Self {
            path: None,
            scope,
            origin: InstructionOrigin::Instructions,
            action: if unsatisfiable {
                InstructionAction::Skipped
            } else {
                InstructionAction::Trimmed
            },
            bytes_kept: 0,
            bytes_dropped: 0,
            tokens_kept: 0,
            tokens_dropped,
            limit: InstructionLimit::StaticGuard,
            fallback_selected: None,
            detail: None,
        }
    }

    /// Whether the notice describes a loss an operator must be told about.
    #[must_use]
    pub(crate) fn is_loss(&self) -> bool {
        self.action != InstructionAction::Kept
    }

    /// Display name of the source: the file name, or the scope name when the
    /// loss covers a whole scope (for example the rules block).
    #[must_use]
    pub(crate) fn label(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| inline_text(&name.to_string_lossy()))
            .unwrap_or_else(|| format!("{} instructions", self.scope.as_str()))
    }

    /// One line per loss, suitable for a system history cell or a synthetic
    /// model message. The scope, origin, action and limit are machine keys and
    /// stay as written; every sentence comes from the locale pack, and numbers
    /// are always present with their unit so "trimmed" never hides how much
    /// was dropped.
    #[must_use]
    pub(crate) fn render(&self, locale: codewhale_localization::Locale) -> String {
        use codewhale_localization::{MessageId, tr};
        if self.limit == InstructionLimit::StaticGuard {
            let tokens = self.tokens_dropped.to_string();
            return if self.action == InstructionAction::Skipped {
                fill(
                    &tr(locale, MessageId::InstructionNoticeStaticGuardOver),
                    &[("tokens", &tokens)],
                )
            } else {
                fill(
                    &tr(locale, MessageId::InstructionNoticeStaticGuardTrimmed),
                    &[("scope", self.scope.as_str()), ("tokens", &tokens)],
                )
            };
        }
        let mut line = fill(
            &tr(locale, MessageId::InstructionNoticeLine),
            &[
                ("scope", self.scope.as_str()),
                ("label", &self.label()),
                ("origin", self.origin.as_str()),
                ("action", self.action.as_str()),
                ("limit", self.limit.as_str()),
            ],
        );
        let numbers = match self.action {
            InstructionAction::Trimmed => true,
            InstructionAction::Refused => self.bytes_kept > 0,
            InstructionAction::Kept | InstructionAction::Skipped => false,
        };
        if numbers {
            line.push_str(&fill(
                &tr(locale, MessageId::InstructionNoticeTrimmed),
                &[
                    ("kept_bytes", &self.bytes_kept.to_string()),
                    (
                        "total_bytes",
                        &self
                            .bytes_kept
                            .saturating_add(self.bytes_dropped)
                            .to_string(),
                    ),
                    ("kept_tokens", &self.tokens_kept.to_string()),
                    (
                        "total_tokens",
                        &self
                            .tokens_kept
                            .saturating_add(self.tokens_dropped)
                            .to_string(),
                    ),
                ],
            ));
        } else if matches!(
            self.action,
            InstructionAction::Skipped | InstructionAction::Refused
        ) && self.bytes_dropped > 0
        {
            line.push_str(&fill(
                &tr(locale, MessageId::InstructionNoticeDroppedBytes),
                &[("bytes", &self.bytes_dropped.to_string())],
            ));
        }
        if let Some(fallback) = self.fallback_selected.as_deref() {
            line.push_str(&fill(
                &tr(locale, MessageId::InstructionNoticeFallback),
                &[("source", &inline_text(fallback))],
            ));
        }
        if let Some(detail) = self.detail.as_ref() {
            line.push_str("; ");
            line.push_str(&detail.render(locale));
        }
        line
    }
}

impl std::fmt::Display for InstructionNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render(codewhale_localization::Locale::En))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstructionRefusal {
    Loss(InstructionNotice),
    OverGuard { measured: usize, guard: usize },
    ReceiptMismatch,
}

impl InstructionRefusal {
    #[must_use]
    pub(crate) fn render(&self, locale_tag: &str) -> String {
        use codewhale_localization::{MessageId, resolve_locale, tr};
        let locale = resolve_locale(locale_tag);
        let reason = match self {
            Self::Loss(notice) => notice.render(locale),
            Self::OverGuard { measured, guard } => fill(
                &tr(locale, MessageId::InstructionBudgetOverGuard),
                &[
                    ("measured", &measured.to_string()),
                    ("guard", &guard.to_string()),
                ],
            ),
            Self::ReceiptMismatch => {
                return "instruction receipt does not match the installed prompt".to_string();
            }
        };
        fill(
            &tr(locale, MessageId::InstructionBudgetRefused),
            &[("reason", &reason)],
        )
    }
}

/// Render the whole loss list as the body of the startup notice.
#[must_use]
pub(crate) fn render_notice_body(
    notices: &[InstructionNotice],
    locale_tag: &str,
) -> Option<String> {
    use codewhale_localization::{MessageId, resolve_locale, tr};
    let losses: Vec<&InstructionNotice> = notices.iter().filter(|n| n.is_loss()).collect();
    if losses.is_empty() {
        return None;
    }
    // Bounded on purpose: a rules directory beyond the cap yields one loss per
    // extra file, and an unbounded summary would be a first-turn message larger
    // than the budget it describes.
    const MAX_LINES: usize = 12;
    const MAX_BYTES: usize = 2_000;
    let locale = resolve_locale(locale_tag);
    let footer = tr(locale, MessageId::InstructionNoticeFooter);
    let more = tr(locale, MessageId::InstructionNoticeMore);
    let reserve = footer.len() + more.len() + 16;
    let mut body = tr(locale, MessageId::InstructionNoticeHeader).into_owned();
    body.push('\n');
    let mut shown = 0usize;
    for notice in &losses {
        let line = format!("- {}\n", notice.render(locale));
        if shown >= MAX_LINES || body.len() + line.len() + reserve > MAX_BYTES {
            break;
        }
        body.push_str(&line);
        shown += 1;
    }
    if shown < losses.len() {
        body.push_str("- ");
        body.push_str(&fill(
            &more,
            &[("count", &(losses.len() - shown).to_string())],
        ));
        body.push('\n');
    }
    body.push_str(&footer);
    Some(body)
}

/// Raw instruction content, kept per source so a scope can be trimmed by
/// whole segments instead of by unmovable joined text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct InstructionSegment {
    pub(crate) scope: InstructionScope,
    pub(crate) origin: InstructionOrigin,
    /// `None` for generated content that has no file behind it.
    pub(crate) path: Option<PathBuf>,
    /// Display label used in the rendered block: a stable relative path or
    /// basename for file sources, or a synthetic name for inline sources.
    pub(crate) label: Option<String>,
    /// Canonical path, used to load one physical file once across scopes.
    pub(crate) canonical_path: Option<PathBuf>,
    pub(crate) text: String,
    /// Lower-precedence file chosen after the preferred one was skipped.
    pub(crate) fallback_selected: Option<String>,
}

impl InstructionSegment {
    pub(crate) fn new(
        scope: InstructionScope,
        origin: InstructionOrigin,
        path: Option<PathBuf>,
        text: String,
    ) -> Self {
        Self {
            scope,
            origin,
            path,
            label: None,
            canonical_path: None,
            text,
            fallback_selected: None,
        }
    }
}

/// Resolved, per-route budgets. Percentages are of the *usable* window, never
/// the raw window: the response and the safety reserve are not available to
/// standing instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct InstructionBudgets {
    pub(crate) global_tokens: usize,
    pub(crate) project_tokens: usize,
    pub(crate) static_guard_tokens: Option<usize>,
    pub(crate) file_max_bytes: u64,
    pub(crate) policy: InstructionBudgetPolicy,
    pub(crate) claude_fallback: bool,
    pub(crate) usable_window_tokens: Option<usize>,
}

impl InstructionBudgets {
    /// Resolve config + route window into concrete token ceilings.
    ///
    /// `window` is the route's context window; `output` its resolved output
    /// reservation when the caller has one. Unknown capacity leaves relative
    /// ceilings unavailable and retains the absolute per-scope caps.
    pub(crate) fn resolve(
        config: Option<&InstructionBudgetConfig>,
        window: Option<usize>,
        output: Option<usize>,
    ) -> Result<Self, String> {
        if let Some(config) = config {
            config.validate()?;
        }
        Ok(Self::resolve_unchecked(config, window, output))
    }

    #[must_use]
    pub(crate) fn resolve_or_default(
        config: &InstructionBudgetConfig,
        window: Option<usize>,
        output: Option<usize>,
    ) -> Self {
        Self::resolve(Some(config), window, output)
            .unwrap_or_else(|_| Self::resolve_unchecked(None, window, output))
    }

    fn resolve_unchecked(
        config: Option<&InstructionBudgetConfig>,
        window: Option<usize>,
        output: Option<usize>,
    ) -> Self {
        let usable = window
            .zip(output)
            .map(|(window, output)| usable_window_tokens(window, Some(output)));
        let global_percent = config
            .and_then(|c| c.global_budget_percent)
            .unwrap_or(DEFAULT_SCOPE_BUDGET_PERCENT);
        let project_percent = config
            .and_then(|c| c.project_budget_percent)
            .unwrap_or(DEFAULT_SCOPE_BUDGET_PERCENT);
        let guard_percent = config
            .and_then(|c| c.static_prefix_guard_percent)
            .unwrap_or(DEFAULT_STATIC_PREFIX_GUARD_PERCENT);
        let global_tokens = config
            .and_then(|c| c.global_budget_tokens)
            .unwrap_or(DEFAULT_SCOPE_BUDGET_TOKENS)
            .min(usable.map_or(usize::MAX, |usable| share_of(usable, global_percent)));
        let project_tokens = config
            .and_then(|c| c.project_budget_tokens)
            .unwrap_or(DEFAULT_SCOPE_BUDGET_TOKENS)
            .min(usable.map_or(usize::MAX, |usable| share_of(usable, project_percent)));
        Self {
            global_tokens,
            project_tokens,
            static_guard_tokens: usable.map(|usable| share_of(usable, guard_percent)),
            file_max_bytes: config
                .and_then(|c| c.file_max_bytes)
                .unwrap_or(DEFAULT_FILE_MAX_BYTES),
            policy: config
                .and_then(|c| c.policy)
                .unwrap_or(InstructionBudgetPolicy::Trim),
            claude_fallback: config.and_then(|c| c.claude_fallback).unwrap_or(false),
            usable_window_tokens: usable,
        }
    }

    /// The shipped defaults for a route window, with no user table.
    #[must_use]
    pub(crate) fn defaults_for(window: Option<usize>) -> Self {
        Self::resolve_unchecked(None, window, None)
    }

    /// Set one scope's ceiling to an absolute value, for the static guard.
    ///
    /// Deliberately absolute rather than "ceiling minus the overage": a scope
    /// that uses less than its ceiling would otherwise keep every byte and the
    /// prefix would stay over the guard. The guard computes the new ceiling
    /// from the scope's *retained* usage minus the overage.
    pub(crate) fn with_scope_ceiling(mut self, scope: InstructionScope, tokens: usize) -> Self {
        match scope {
            InstructionScope::Global => self.global_tokens = self.global_tokens.min(tokens),
            InstructionScope::Project => self.project_tokens = self.project_tokens.min(tokens),
        }
        self
    }
}

/// Usable window: the route window minus the response ceiling and a safety
/// reserve of 10% (minimum 2,048 tokens).
#[must_use]
pub(crate) fn usable_window_tokens(window: usize, output: Option<usize>) -> usize {
    let output = output.unwrap_or(0);
    let reserve = (window / 100 * SAFETY_RESERVE_PERCENT).max(SAFETY_RESERVE_MIN_TOKENS);
    window.saturating_sub(output).saturating_sub(reserve)
}

fn share_of(total: usize, percent: u8) -> usize {
    total / 100 * usize::from(percent) + (total % 100) * usize::from(percent) / 100
}

/// Estimated tokens for instruction text. One estimator for budgets,
/// reports, and markers, so a number shown to the user is the number the
/// trim used.
#[must_use]
pub(crate) fn estimate_instruction_tokens(text: &str) -> usize {
    crate::compaction::estimate_text_tokens_conservative(text)
}

/// Result of trimming one piece of instruction text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TrimmedText {
    pub(crate) text: String,
    pub(crate) bytes_kept: u64,
    pub(crate) bytes_dropped: u64,
    pub(crate) tokens_kept: usize,
    pub(crate) tokens_dropped: usize,
    /// True when the marker alone consumed the budget and no source text
    /// survived.
    pub(crate) empty: bool,
}

impl TrimmedText {
    fn whole(text: &str) -> Self {
        let bytes = text.len() as u64;
        Self {
            text: text.to_string(),
            bytes_kept: bytes,
            bytes_dropped: 0,
            tokens_kept: estimate_instruction_tokens(text),
            tokens_dropped: 0,
            empty: text.is_empty(),
        }
    }
}

/// Marker inserted where a head-trim dropped the tail.
#[must_use]
pub(crate) fn instruction_trim_marker(
    scope: InstructionScope,
    kept: usize,
    dropped: usize,
) -> String {
    format!(
        "\n\n[…{} instructions trimmed: ~{kept} estimated tokens kept, ~{dropped} estimated tokens dropped at the {} scope budget…]",
        scope.as_str(),
        scope.as_str()
    )
}

/// Marker inserted when the rules block was trimmed.
#[must_use]
pub(crate) fn rules_trim_marker(kept: usize, dropped: usize) -> String {
    format!(
        "\n\n[…rules block trimmed: ~{kept} estimated tokens kept, ~{dropped} estimated tokens dropped at the project scope budget…]"
    )
}

/// Keep the longest UTF-8 prefix of `text` whose estimated token count, plus
/// the marker, fits `budget_tokens`.
///
/// The cut point is found by binary search over character boundaries, so the
/// result is deterministic and never splits a character.
#[must_use]
pub(crate) fn keep_head_within_tokens(
    text: &str,
    budget_tokens: usize,
    marker: impl Fn(usize, usize) -> String,
) -> TrimmedText {
    let total_tokens = estimate_instruction_tokens(text);
    if total_tokens <= budget_tokens {
        return TrimmedText::whole(text);
    }
    // Search over byte offsets, but only ever cut on a character boundary.
    // Both bounds move every iteration (`low` up to a strictly larger
    // boundary, `high` down past the probed one), so the loop terminates for
    // multibyte text — where a naive midpoint search can settle on the same
    // floored boundary forever.
    let floor_boundary = |index: usize| {
        let mut index = index.min(text.len());
        while index > 0 && !text.is_char_boundary(index) {
            index -= 1;
        }
        index
    };
    let next_boundary = |index: usize| {
        let mut index = index.min(text.len());
        while index < text.len() && !text.is_char_boundary(index) {
            index += 1;
        }
        index
    };
    let mut best = 0usize;
    let mut low = 0usize;
    let mut high = text.len();
    while low < high {
        let mut middle = floor_boundary(low + (high - low) / 2);
        if middle <= low {
            middle = next_boundary(middle + 1);
            if middle > high {
                break;
            }
        }
        let head = text[..middle].trim_end();
        let kept = estimate_instruction_tokens(head);
        let rendered = format!("{head}{}", marker(kept, total_tokens.saturating_sub(kept)));
        if estimate_instruction_tokens(&rendered) <= budget_tokens {
            best = middle;
            low = middle;
        } else {
            if middle == 0 {
                break;
            }
            high = middle - 1;
        }
    }

    let head = text[..best].trim_end();
    let kept_tokens = estimate_instruction_tokens(head);
    let dropped_tokens = total_tokens.saturating_sub(kept_tokens);
    if head.is_empty() {
        return TrimmedText {
            text: String::new(),
            bytes_kept: 0,
            bytes_dropped: text.len() as u64,
            tokens_kept: 0,
            tokens_dropped: total_tokens,
            empty: true,
        };
    }
    let marker = marker(kept_tokens, dropped_tokens);
    TrimmedText {
        text: format!("{head}{marker}"),
        bytes_kept: head.len() as u64,
        bytes_dropped: (text.len() - head.len()) as u64,
        tokens_kept: kept_tokens,
        tokens_dropped: dropped_tokens,
        empty: false,
    }
}

/// Result of one budget pass over both scopes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstructionAssembly {
    /// The rendered `<project_instructions>` body, already trimmed.
    pub(crate) block: Option<String>,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) constitution_source_path: Option<PathBuf>,
    pub(crate) constitution_tokens: usize,
    pub(crate) warnings: Vec<String>,
    #[serde(skip)]
    pub(crate) prompt_range: Option<std::ops::Range<usize>>,
    /// Losses from trimming, plus the loader's skip notices.
    pub(crate) notices: Vec<InstructionNotice>,
    /// Estimated tokens kept per scope after trimming.
    pub(crate) global_tokens: usize,
    pub(crate) project_tokens: usize,
    /// The budgets this pass used.
    pub(crate) budgets: InstructionBudgets,
}

impl InstructionAssembly {
    pub(crate) fn receipt_json(&self) -> serde_json::Value {
        serde_json::json!({ "global_tokens": self.global_tokens, "project_tokens": self.project_tokens, "budgets": self.budgets, "notices": self.notices, "source_path": self.source_path, "constitution_source_path": self.constitution_source_path, "constitution_tokens": self.constitution_tokens, "retained_block_tokens": self.block.as_deref().map_or(0, estimate_instruction_tokens), "warnings": self.warnings })
    }
}

/// Apply each scope's ceiling to the raw segments, then charge the rules
/// block to what is left of the project scope.
///
/// Segments are dropped whole from the broadest end of their scope first; a
/// single segment above its remaining budget keeps its head. Mutating the
/// segments in place is what lets the joined string be rebuilt from the
/// trimmed truth instead of being trimmed as an opaque blob.
pub(crate) fn apply_instruction_budgets(
    segments: &mut Vec<InstructionSegment>,
    rules_block: &mut Option<String>,
    budgets: &InstructionBudgets,
) -> Vec<InstructionNotice> {
    let mut notices = Vec::new();
    trim_scope(
        segments,
        InstructionScope::Global,
        budgets.global_tokens,
        &mut notices,
    );
    let project_kept = trim_scope(
        segments,
        InstructionScope::Project,
        budgets.project_tokens,
        &mut notices,
    );

    // Anything that survived without a loss notice is reported as `kept`, so
    // `/context report` can list every source's action rather than only the
    // ones that lost content.
    for segment in segments.iter() {
        if notices.iter().any(|notice| {
            notice.is_loss()
                && notice.scope == segment.scope
                && notice.origin == segment.origin
                && notice.path == segment.path
        }) {
            continue;
        }
        notices.push(InstructionNotice {
            path: segment.path.clone(),
            scope: segment.scope,
            origin: segment.origin,
            action: InstructionAction::Kept,
            bytes_kept: segment.text.len() as u64,
            bytes_dropped: 0,
            tokens_kept: estimate_instruction_tokens(&segment.text),
            tokens_dropped: 0,
            limit: InstructionLimit::None,
            fallback_selected: None,
            detail: None,
        });
    }

    // Rules are project-scope authority and read after the chain, so they get
    // whatever the instruction chain could not spend.
    if let Some(rules) = rules_block.as_mut() {
        let total = estimate_instruction_tokens(rules);
        let remaining = budgets
            .project_tokens
            .saturating_sub(project_kept)
            .saturating_sub(usize::from(project_kept > 0) * 2);
        if total > remaining {
            if remaining == 0 {
                notices.push(InstructionNotice {
                    path: None,
                    scope: InstructionScope::Project,
                    origin: InstructionOrigin::Rules,
                    action: InstructionAction::Skipped,
                    bytes_kept: 0,
                    bytes_dropped: rules.len() as u64,
                    tokens_kept: 0,
                    tokens_dropped: total,
                    limit: InstructionLimit::RulesBudget,
                    fallback_selected: None,
                    detail: Some(InstructionDetail::RulesBudgetExhausted {
                        budget_tokens: budgets.project_tokens,
                    }),
                });
                *rules_block = None;
            } else {
                let trimmed = keep_head_within_tokens(rules, remaining, rules_trim_marker);
                notices.push(InstructionNotice {
                    path: None,
                    scope: InstructionScope::Project,
                    origin: InstructionOrigin::Rules,
                    action: if trimmed.empty {
                        InstructionAction::Skipped
                    } else {
                        InstructionAction::Trimmed
                    },
                    bytes_kept: trimmed.bytes_kept,
                    bytes_dropped: trimmed.bytes_dropped,
                    tokens_kept: trimmed.tokens_kept,
                    tokens_dropped: trimmed.tokens_dropped,
                    limit: InstructionLimit::RulesBudget,
                    fallback_selected: None,
                    detail: None,
                });
                *rules_block = if trimmed.text.is_empty() {
                    None
                } else {
                    Some(trimmed.text)
                };
            }
        } else {
            notices.push(InstructionNotice {
                path: None,
                scope: InstructionScope::Project,
                origin: InstructionOrigin::Rules,
                action: InstructionAction::Kept,
                bytes_kept: rules.len() as u64,
                bytes_dropped: 0,
                tokens_kept: estimate_instruction_tokens(rules),
                tokens_dropped: 0,
                limit: InstructionLimit::None,
                fallback_selected: None,
                detail: None,
            });
        }
    }

    notices
}

fn trim_scope(
    segments: &mut Vec<InstructionSegment>,
    scope: InstructionScope,
    budget_tokens: usize,
    notices: &mut Vec<InstructionNotice>,
) -> usize {
    let mut order: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, segment)| segment.scope == scope)
        .map(|(index, _)| index)
        .collect();
    if order.is_empty() {
        return 0;
    }
    let scope_tokens = |segments: &[InstructionSegment]| {
        super::render_instruction_scope(segments, scope)
            .as_deref()
            .map_or(0, estimate_instruction_tokens)
    };
    let mut kept = scope_tokens(segments);
    if kept <= budget_tokens {
        return kept;
    }
    let mut dropped: Vec<usize> = Vec::new();
    while order.len() > 1 && kept > budget_tokens {
        let index = order.remove(0);
        dropped.push(index);
        let retained: Vec<_> = order.iter().map(|&index| segments[index].clone()).collect();
        kept = scope_tokens(&retained);
    }

    let dropped_count = dropped.len();
    for index in &dropped {
        let segment = &segments[*index];
        let tokens = estimate_instruction_tokens(&segment.text);
        notices.push(InstructionNotice {
            path: segment.path.clone(),
            scope,
            origin: segment.origin,
            action: InstructionAction::Trimmed,
            bytes_kept: 0,
            bytes_dropped: segment.text.len() as u64,
            tokens_kept: 0,
            tokens_dropped: tokens,
            limit: InstructionLimit::ScopeBudget,
            fallback_selected: None,
            detail: Some(InstructionDetail::DroppedBroadest),
        });
    }
    // Remove them from the segment list: a notice that says content was
    // dropped while the content still renders is worse than no notice. Every
    // dropped index sits before `order[0]`, so removing in descending order
    // keeps the surviving index valid.
    for index in dropped.iter().rev() {
        segments.remove(*index);
    }

    // 2. A single segment above the remaining budget keeps its head.
    let last = order[0] - dropped_count;
    let overhead = scope_tokens(std::slice::from_ref(&segments[last]))
        .saturating_sub(estimate_instruction_tokens(&segments[last].text));
    let mut content_budget = budget_tokens.saturating_sub(overhead.saturating_add(2));
    let trimmed = loop {
        let trimmed =
            keep_head_within_tokens(&segments[last].text, content_budget, |kept, dropped| {
                instruction_trim_marker(scope, kept, dropped)
            });
        let mut candidate = segments[last].clone();
        candidate.text = trimmed.text.clone();
        let measured = if trimmed.empty {
            0
        } else {
            scope_tokens(std::slice::from_ref(&candidate))
        };
        if measured <= budget_tokens || content_budget == 0 {
            break trimmed;
        }
        content_budget = content_budget.saturating_sub(measured - budget_tokens);
    };
    if trimmed.empty {
        let segment = &segments[last];
        notices.push(InstructionNotice {
            path: segment.path.clone(),
            scope,
            origin: segment.origin,
            action: InstructionAction::Skipped,
            bytes_kept: 0,
            bytes_dropped: segment.text.len() as u64,
            tokens_kept: 0,
            tokens_dropped: estimate_instruction_tokens(&segment.text),
            limit: InstructionLimit::ScopeBudget,
            fallback_selected: None,
            detail: Some(InstructionDetail::NoRoomForContent { budget_tokens }),
        });
        segments.remove(last);
    } else if trimmed.bytes_dropped > 0 {
        notices.push(InstructionNotice {
            path: segments[last].path.clone(),
            scope,
            origin: segments[last].origin,
            action: InstructionAction::Trimmed,
            bytes_kept: trimmed.bytes_kept,
            bytes_dropped: trimmed.bytes_dropped,
            tokens_kept: trimmed.tokens_kept,
            tokens_dropped: trimmed.tokens_dropped,
            limit: InstructionLimit::ScopeBudget,
            fallback_selected: None,
            detail: None,
        });
        segments[last].text = trimmed.text;
    }
    // When the surviving segment already fits, nothing is reported for it: a
    // "trimmed" notice on untouched text is exactly the kind of false receipt
    // this contract exists to prevent. The `kept` pass below covers it.

    scope_tokens(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_window_subtracts_output_and_ten_percent() {
        assert_eq!(usable_window_tokens(128_000, Some(2_100)), 113_100);
        assert_eq!(usable_window_tokens(32_000, Some(2_000)), 26_800);
        // The reserve never drops below its floor.
        assert_eq!(usable_window_tokens(10_000, Some(0)), 10_000 - 2_048);
    }

    #[test]
    fn per_scope_budget_takes_the_smaller_of_absolute_and_percent() {
        let budgets =
            InstructionBudgets::resolve(None, Some(128_000), Some(2_100)).expect("resolve");
        // 25% of 113,100 is 28,275, below the 100k absolute cap; 80% is
        // 90,480.
        assert_eq!(budgets.global_tokens, 28_275);
        assert_eq!(budgets.project_tokens, 28_275);
        assert_eq!(budgets.static_guard_tokens, Some(90_480));

        let budgets =
            InstructionBudgets::resolve(None, Some(1_000_000), Some(2_048)).expect("resolve");
        // The absolute cap wins on a large window: 10% per scope, 20% total.
        assert_eq!(budgets.global_tokens, DEFAULT_SCOPE_BUDGET_TOKENS);
        assert_eq!(budgets.project_tokens, DEFAULT_SCOPE_BUDGET_TOKENS);
    }

    #[test]
    fn absolute_cap_is_independent_of_window() {
        let config = InstructionBudgetConfig {
            global_budget_tokens: Some(500),
            project_budget_tokens: Some(25_000),
            ..Default::default()
        };
        let budgets = InstructionBudgets::resolve(Some(&config), Some(128_000), Some(2_100))
            .expect("resolve");
        assert_eq!(budgets.global_tokens, 500);
        assert_eq!(budgets.project_tokens, 25_000);
    }

    #[test]
    fn invalid_percentages_and_zero_budgets_are_rejected() {
        for config in [
            InstructionBudgetConfig {
                global_budget_tokens: Some(0),
                ..Default::default()
            },
            InstructionBudgetConfig {
                project_budget_tokens: Some(0),
                ..Default::default()
            },
            InstructionBudgetConfig {
                file_max_bytes: Some(0),
                ..Default::default()
            },
            InstructionBudgetConfig {
                global_budget_percent: Some(0),
                ..Default::default()
            },
            InstructionBudgetConfig {
                project_budget_percent: Some(101),
                ..Default::default()
            },
            InstructionBudgetConfig {
                static_prefix_guard_percent: Some(0),
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err(), "{config:?} must be rejected");
        }
    }

    #[test]
    fn guard_must_leave_room_for_other_static_content() {
        let too_tight = InstructionBudgetConfig {
            global_budget_percent: Some(25),
            project_budget_percent: Some(25),
            static_prefix_guard_percent: Some(55),
            ..Default::default()
        };
        assert!(too_tight.validate().is_err());

        let inverted = InstructionBudgetConfig {
            global_budget_percent: Some(60),
            project_budget_percent: Some(60),
            static_prefix_guard_percent: Some(80),
            ..Default::default()
        };
        assert!(inverted.validate().is_err());

        let ok = InstructionBudgetConfig {
            global_budget_percent: Some(25),
            project_budget_percent: Some(25),
            static_prefix_guard_percent: Some(70),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn percent_and_guard_bounds_are_validated() {
        for config in [
            InstructionBudgetConfig {
                global_budget_percent: Some(0),
                ..Default::default()
            },
            InstructionBudgetConfig {
                project_budget_percent: Some(101),
                ..Default::default()
            },
            InstructionBudgetConfig {
                static_prefix_guard_percent: Some(101),
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err(), "{config:?}");
        }
    }

    #[test]
    fn unknown_window_reports_absolute_caps_and_unknown_relative_facts() {
        let budgets = InstructionBudgets::resolve(None, None, None).expect("resolve");
        assert_eq!(budgets.global_tokens, DEFAULT_SCOPE_BUDGET_TOKENS);
        assert_eq!(budgets.project_tokens, DEFAULT_SCOPE_BUDGET_TOKENS);
        assert_eq!(budgets.usable_window_tokens, None);
        assert_eq!(budgets.static_guard_tokens, None);
    }

    #[test]
    fn head_trim_keeps_the_head_and_reports_numbers() {
        let text = "руководство ".repeat(20_000);
        let trimmed = keep_head_within_tokens(&text, 1_000, |kept, dropped| {
            instruction_trim_marker(InstructionScope::Project, kept, dropped)
        });
        assert!(!trimmed.empty);
        assert!(trimmed.tokens_kept <= 1_000);
        assert!(trimmed.tokens_dropped > 0);
        assert!(trimmed.text.starts_with("руководство"));
        assert!(trimmed.text.contains("instructions trimmed"));
        assert_eq!(
            trimmed.bytes_kept as usize + trimmed.bytes_dropped as usize,
            text.len()
        );
    }

    #[test]
    fn text_within_budget_is_returned_whole() {
        let text = "short instructions";
        let trimmed = keep_head_within_tokens(text, 1_000, |kept, dropped| {
            instruction_trim_marker(InstructionScope::Global, kept, dropped)
        });
        assert_eq!(trimmed.text, text);
        assert_eq!(trimmed.bytes_dropped, 0);
    }
}
