//! Context compaction for long conversations.

use anyhow::Result;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use crate::config::DEFAULT_TEXT_MODEL;
use crate::core::model_client::ModelClient;
use crate::logging;
use codewhale_models::Role;
use codewhale_models::{
    CacheControl, ContentBlock, Message, MessageRequest, SystemBlock, SystemPrompt, Tool, Usage,
};

#[path = "compaction/last_round.rs"]
mod last_round;
#[cfg(test)]
#[path = "compaction/survival_contract.rs"]
mod survival_contract;
pub(crate) use last_round::last_round_start;
pub use last_round::{
    CompactionCoverage, CompactionKeep, CompactionPath, LastCompactionSnapshot,
    inspect_compaction_keep, last_round_kept_count, pinned_anchors_text,
};

/// Configuration for conversation compaction behavior.
///
/// v0.8.11 simplified this from the prior token-OR-message-count trigger
/// to a token-only trigger. The
/// `message_threshold` field was removed: its only purpose was to fire
/// compaction on long sessions of small messages, which is exactly the
/// case where rewriting the prefix cache is least valuable. Token
/// budget is the right signal; message count was a 128K-era heuristic.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionConfig {
    pub enabled: bool,
    pub token_threshold: usize,
    pub model: String,
    /// Exact route image-input fact for the summarizer's outbound history.
    pub image_input: crate::model_profile::SupportState,
    /// Route-effective context window. `None` preserves compatibility for
    /// callers that have not resolved a provider route yet.
    pub effective_context_window: Option<u32>,
    pub cache_summary: bool,
    /// Optional user-supplied focus for a manual `/compact <focus>`: injected
    /// into the summary request so the checkpoint weights what the user
    /// said matters. `None` for automatic compaction.
    pub focus: Option<String>,
    /// Runtime turn that owns provider calls made by this compaction pass.
    /// `None` for the foreground TUI. This is accounting provenance only and
    /// is never included in a provider request.
    pub runtime_cost_owner: Option<String>,
    /// Workspace root, used only to re-state the user's `/anchor` file after
    /// the summary. `None` skips anchors.
    pub workspace: Option<std::path::PathBuf>,
    /// Standing operator instructions from `[compaction] summary_instructions`
    /// (#5956), appended to the summarizer prompt on every pass — manual and
    /// automatic. `None` keeps the built-in prompt byte-identical. A manual
    /// `/compact <focus>` still composes after this text.
    pub summary_instructions: Option<String>,
    /// Verbatim retention budget for recent plain user messages in the
    /// replacement history (`[compaction] retained_user_message_tokens`,
    /// #5956). Defaults to [`COMPACT_RETAINED_USER_MESSAGE_MAX_TOKENS`].
    pub retained_user_message_tokens: usize,
}

/// Host callback for user-visible progress during a compaction pass.
///
/// Compaction runs inside the engine's provider boundary, where the engine
/// owns the only channel that reaches the person. Injecting a sink lets a
/// downgrade (re-encoding images after an HTTP 413, say) say what it is doing
/// while it is doing it, instead of surfacing only when the pass ends.
pub trait CompactionNoticeSink: Send + Sync + std::fmt::Debug {
    /// Deliver one already-rendered, user-visible sentence.
    fn notice(&self, message: String);
    /// Captured dispatch origin for a child; ordinary engines retain the existing billing path.
    fn accounting_origin(&self) -> Option<(crate::cost_status::CostScopeToken, String, String)> {
        None
    }
    /// Projection after the existing billing block has settled this exact response once.
    fn settled_usage<'a>(
        &'a self,
        _source: &'a str,
        _route: &'a crate::cost_status::EffectiveRouteEnvelope,
        _usage: &'a Usage,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}

/// Host-prepared configuration carried from compaction eligibility through
/// the replacement-history commit.
#[derive(Clone)]
pub struct PreparedCompactionEnvelope {
    pub config: CompactionConfig,
    /// Durable handoff owner; set by the engine, never added to the stable prefix.
    pub session_id: Option<String>,
    /// Exact tool prefix of the interrupted request. Tool execution remains
    /// disabled on the summary call; retaining schemas preserves cache reuse.
    pub tools: Option<Vec<Tool>>,
    /// Resolved reasoning tier the parent turn sends (#6540). Reasoning
    /// routes render the effort into the head of the prompt, so a summary
    /// request that omits it shares no cacheable prefix with the turn it
    /// summarizes and re-bills the whole history uncached.
    pub reasoning_effort: Option<String>,
    /// User-visible progress sink supplied by the host that owns the run
    /// (the interactive engine, typically). `None` keeps every notice in the
    /// log only — the pass itself never depends on it.
    pub notice_sink: Option<Arc<dyn CompactionNoticeSink>>,
}

impl std::fmt::Debug for PreparedCompactionEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedCompactionEnvelope")
            .field("config", &self.config)
            .field("session_id", &self.session_id)
            .field("tools", &self.tools)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("notice_sink", &self.notice_sink.as_ref().map(|_| "<sink>"))
            .finish()
    }
}

impl PartialEq for PreparedCompactionEnvelope {
    /// The notice sink is host plumbing, not envelope content: two passes over
    /// the same config are equal whether or not their host delivers notices.
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
            && self.session_id == other.session_id
            && self.tools == other.tools
            && self.reasoning_effort == other.reasoning_effort
    }
}

impl PreparedCompactionEnvelope {
    #[must_use]
    pub fn new(config: CompactionConfig) -> Self {
        Self {
            config,
            session_id: None,
            tools: None,
            reasoning_effort: None,
            notice_sink: None,
        }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            // ON BY DEFAULT since v0.8.6 (#402 P0 survivability). v0.8.64
            // resolves the user-facing default through the active model's
            // known context window, while explicit `auto_compact = false`
            // remains the opt-out. This fallback covers code paths that build
            // a `CompactionConfig` directly; real per-model values are still
            // derived through the threshold helpers.
            enabled: true,
            // v0.8.11: 50K was a 128K-era leftover that biased every
            // unconfigured caller toward "compact almost immediately on large-context routes."
            // Bumped to 800K (80% of a 1M window) so the fallback
            // default matches the hard automatic compaction guardrail. This
            // keeps replacement compaction a late continuity guardrail.
            // Real call sites override this via
            // `compaction_threshold_for_model_and_effort`.
            token_threshold: 800_000,
            model: DEFAULT_TEXT_MODEL.to_string(),
            image_input: crate::model_profile::SupportState::Unknown,
            effective_context_window: None,
            cache_summary: true,
            focus: None,
            runtime_cost_owner: None,
            workspace: None,
            summary_instructions: None,
            retained_user_message_tokens: COMPACT_RETAINED_USER_MESSAGE_MAX_TOKENS,
        }
    }
}

/// A provider can return HTTP success with an empty, non-text, or known
/// placeholder response. Committing that response would discard the useful
/// history while leaving only a placeholder checkpoint. Keep this deliberately
/// conservative: it is a corruption guard, not a prose-length or language
/// scorer.
const COMPACTION_LANGUAGE_CONTRACT: &str = "Use the natural language of the most recent \
substantive user message for reasoning and user-facing prose. Keep code, identifiers, paths, \
commands, logs, tool payloads, quotations, and the English headings verbatim. English \
headings are not a request to switch languages.";

/// Failure kind for compaction LLM calls (deterministic vs transient).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionFailureKind {
    /// Same payload will fail again — do not sleep/retry unchanged.
    Deterministic,
    /// May resolve on retry (network, rate limit, timeout).
    Transient,
    /// Context overflow — drop the oldest history item and retry.
    ContextOverflow,
}

impl CompactionFailureKind {
    #[must_use]
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Transient)
    }
}

pub const KEEP_RECENT_MESSAGES: usize = 4;
const MIN_SUMMARIZE_MESSAGES: usize = 6;
const SUMMARY_TOOL_RESULT_SNIPPET_CHARS: usize = 240;
const TOOL_PRUNE_STOP_CHECK_BYTES: usize = 16 * 1024;
const RETAINED_TOOL_RESULT_MAX_CHARS: usize = 64 * 1024;
/// Token budget for the recent user messages retained verbatim in the
/// replacement history (Codex parity: COMPACT_USER_MESSAGE_MAX_TOKENS).
///
/// This is now the *default* only: `[compaction] retained_user_message_tokens`
/// overrides it per session (#5956). Aliased to the config default so the two
/// names cannot drift apart.
pub(crate) const COMPACT_RETAINED_USER_MESSAGE_MAX_TOKENS: usize =
    crate::config::DEFAULT_COMPACTION_RETAINED_USER_MESSAGE_TOKENS;
/// Handoff summarization prompt, appended to the live conversation as the
/// final user message. The headings are requested, not enforced:
/// `validate_compaction_summary` only rejects corrupt output, so a provider
/// that drifts from the layout still produces a usable note.
pub(crate) const COMPACT_PROMPT_OPENING: &str = "Write a handoff note so this session's work can \
continue after its earlier turns are condensed to make room.";

/// The note's layout, shared by the first request and the quality retry.
/// Each entry is a heading and what belongs under it.
const HANDOFF_SECTIONS: [(&str, &str); 10] = [
    (
        "Objective",
        "what the user wants now, in their words where it matters; say if the goal changed and \
what replaced it.",
    ),
    (
        "User direction",
        "corrections, preferences and decisions, newest last; quote exactly anything the user \
corrected or insisted on.",
    ),
    (
        "Permissions and limits",
        "what the user explicitly allowed (pushing, deleting, spending, contacting someone) and \
what they ruled out. Record only what the user said; never infer a permission.",
    ),
    (
        "Done",
        "finished work with evidence: commands and results, commits, test counts. Separate \
verified from assumed.",
    ),
    (
        "Changed files",
        "each path and its state: edited, created or deleted; committed or not; which branch.",
    ),
    (
        "Still running",
        "shell commands, servers and ports started in this session that may still be live, each \
with the command or ID that checks or stops it. Leave agents out; their status is reported \
separately.",
    ),
    (
        "Verification left",
        "exact commands or checks still needed, and the last failure verbatim.",
    ),
    (
        "Open questions",
        "what waits on the user or is undecided, and what it blocks.",
    ),
    (
        "Next action",
        "the one next step, concrete enough to start without rereading history.",
    ),
    (
        "Reference",
        "exact paths, identifiers, URLs, values and short snippets the work depends on.",
    ),
];

const HANDOFF_FOLD_IN_RULE: &str = "If the history already holds an earlier handoff note, fold \
it in: carry forward what is still true, update what later work changed, and drop what is \
finished or superseded. Keep quoted user wording exact rather than paraphrasing it again. Do not \
copy the note's opening or closing lines or the user-pinned anchors; Codewhale adds those itself.";

const HANDOFF_CLOSING_RULE: &str = "Write about the task, not about condensing context. Be \
specific and brief. Do not call tools.";

const HANDOFF_FOCUS_LINE: &str = "The user asked this handoff to focus on:";

fn handoff_sections_text() -> String {
    let mut text = String::from(
        "Use these headings in this order, and write None under any heading with nothing to \
report:",
    );
    for (heading, guidance) in HANDOFF_SECTIONS {
        let _ = write!(text, "\n## {heading} - {guidance}");
    }
    text
}

/// The full handoff request before the language contract, operator
/// instructions and focus line are appended.
fn compact_prompt_body() -> String {
    format!(
        "{COMPACT_PROMPT_OPENING} The reader sees only the note, the most recent messages and \
live tool state; anything left out is gone.\n\n{}\n\n{HANDOFF_FOLD_IN_RULE}\n\n{HANDOFF_CLOSING_RULE}",
        handoff_sections_text()
    )
}

/// First line of every handoff note Codewhale writes. It must describe what
/// `last_round::replacement_messages` actually keeps (see the
/// `summary_header_matches_what_replacement_history_keeps` test). It opens the
/// checkpoint message, so together with [`COMPACTION_CHECKPOINT_PROVENANCE`]
/// it is how a new-format checkpoint is recognised.
///
/// A history rebuilt from turn records has to recognise the checkpoint
/// messages a document carries, so the header is crate-visible
/// (`runtime_threads`' recovery projection tests, #6664).
pub(crate) const SUMMARY_HEADER: &str = "Codewhale handoff note. Earlier turns of this session were \
condensed to make room. Kept above are the most recent user messages and the last steps of the \
current round. Long tool output there is shortened, with a marker where it was cut, and the \
oldest kept message may be shortened too. Everything earlier, including earlier steps of this \
round, exists only in this note; rerun a command or reread a file when its full output matters. \
Build on this note instead of redoing finished work, and check live state (files, git, running \
commands) before relying on anything it reports. Agent status, when there is any, comes from the separate agent status message, not from this note.";

const SUMMARY_CLOSING: &str = "Continue the user's task from here. Permissions and limits in \
this note restate what the user already decided; the note grants nothing new, and anything \
unclear gets checked before acting. Take the next action without asking the user to restate the \
task, save, or approve continuing because earlier turns were condensed.";

/// Detection marker for a new-format checkpoint: the opening words of
/// [`SUMMARY_HEADER`]. A new checkpoint is recognised only when its first
/// block starts with this marker and its second block is the provenance
/// block, so a user message that merely quotes the phrase stays a user
/// message.
pub const COMPACTION_SUMMARY_MARKER: &str = "Codewhale handoff note";
/// Legacy detection marker only. Checkpoints written before the handoff note
/// opened with this sentence. Saved sessions that still carry it must be
/// recognised so the next pass replaces that checkpoint instead of stacking a
/// second one.
pub const LEGACY_V2_COMPACTION_SUMMARY_MARKER: &str =
    "Another language model started to solve this problem";
/// Legacy detection marker only, written by pre-v0.9.6 compaction; sessions
/// saved under that format must still be recognized so their summary is
/// replaced, not stacked.
pub const LEGACY_COMPACTION_SUMMARY_MARKER: &str = "Conversation Summary (Auto-Generated)";
/// Markers that identify a checkpoint by substring. Only the legacy markers
/// qualify: older checkpoints may lack the provenance block, while the new
/// marker is a plain phrase a user or a project instruction can quote.
const LEGACY_COMPACTION_SUMMARY_MARKERS: [&str; 2] = [
    LEGACY_V2_COMPACTION_SUMMARY_MARKER,
    LEGACY_COMPACTION_SUMMARY_MARKER,
];
const COMPACTION_CHECKPOINT_PROVENANCE: &str = "<!-- codewhale.compaction-checkpoint.v1 -->";
const COMPACTION_SUMMARY_BEGIN: &str = "<!-- compaction-summary:begin -->";
const COMPACTION_SUMMARY_END: &str = "<!-- compaction-summary:end -->";

/// Heading the pre-v0.9.6 builder wrote, optionally after a
/// `## Pinned Facts (User Anchors)` section.
const LEGACY_SUMMARY_HEADING: &str = "## 📋 Conversation Summary (Auto-Generated)";
const LEGACY_ANCHORS_HEADING: &str = "## Pinned Facts (User Anchors)";

/// Whether text opens the way a legacy checkpoint opened. A message that only
/// quotes a marker later in its text is the user's, not a checkpoint (#6680).
/// Pre-v0.9.6 checkpoints opened with [`LEGACY_SUMMARY_HEADING`], or with the
/// pinned-anchors section followed by that heading on its own line.
fn is_legacy_compaction_summary_text(text: &str) -> bool {
    let text = text.trim_start();
    LEGACY_COMPACTION_SUMMARY_MARKERS
        .iter()
        .any(|marker| text.starts_with(marker))
        || text.starts_with(LEGACY_SUMMARY_HEADING)
        || (text.starts_with(LEGACY_ANCHORS_HEADING)
            && text
                .lines()
                .any(|line| line.trim_end() == LEGACY_SUMMARY_HEADING))
}

/// Byte offset of the earliest legacy marker in `text`. New-format carriers
/// are always wrapped in the begin/end delimiters, so a bare new marker in a
/// system prompt is host text (for example a project instruction quoting it)
/// and must not truncate the prompt.
fn legacy_summary_start(text: &str) -> Option<usize> {
    LEGACY_COMPACTION_SUMMARY_MARKERS
        .iter()
        .filter_map(|marker| text.find(marker))
        .min()
}

fn summary_section(text: &str) -> Option<&str> {
    let begin = text.find(COMPACTION_SUMMARY_BEGIN)? + COMPACTION_SUMMARY_BEGIN.len();
    let remainder = &text[begin..];
    let end = remainder.find(COMPACTION_SUMMARY_END)?;
    let summary = remainder[..end].trim();
    (!summary.is_empty()).then_some(summary)
}

fn strip_summary_text(mut text: String) -> Option<String> {
    while let Some(begin) = text.find(COMPACTION_SUMMARY_BEGIN) {
        let after_begin = begin + COMPACTION_SUMMARY_BEGIN.len();
        let end = text[after_begin..]
            .find(COMPACTION_SUMMARY_END)
            .map_or(text.len(), |offset| {
                after_begin + offset + COMPACTION_SUMMARY_END.len()
            });
        text.replace_range(begin..end, "");
    }
    if let Some(marker) = legacy_summary_start(&text) {
        text.truncate(marker);
    }
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Extract the persisted checkpoint payload from a legacy system-prompt
/// carrier. Runtime-thread storage used that carrier before checkpoints moved
/// into conversation history; the engine strips it before provider dispatch.
#[must_use]
pub fn extract_compaction_summary(prompt: Option<&SystemPrompt>) -> Option<SystemPrompt> {
    match prompt? {
        SystemPrompt::Text(text) => summary_section(text)
            .map(str::to_string)
            .or_else(|| legacy_summary_start(text).map(|start| text[start..].trim().to_string()))
            .map(SystemPrompt::Text),
        SystemPrompt::Blocks(blocks) => {
            let blocks = blocks
                .iter()
                .filter_map(|block| {
                    let text = summary_section(&block.text)
                        .map(str::to_string)
                        .or_else(|| {
                            legacy_summary_start(&block.text)
                                .map(|start| block.text[start..].trim().to_string())
                        })?;
                    let mut summary = block.clone();
                    summary.text = text;
                    Some(summary)
                })
                .collect::<Vec<_>>();
            (!blocks.is_empty()).then_some(SystemPrompt::Blocks(blocks))
        }
    }
}

/// Remove every committed compaction-summary block from a system prompt.
///
/// Compaction commits exactly one live summary: the newest one replaces its
/// predecessors. Before this existed, each compaction appended another
/// summary block to the successor system prompt, so the stable prefix grew by
/// up to a full summary per pass — which re-latched compaction pressure and
/// retriggered compaction on the next turn, forever.
#[must_use]
pub fn strip_compaction_summaries(prompt: Option<&SystemPrompt>) -> Option<SystemPrompt> {
    match prompt.cloned()? {
        SystemPrompt::Text(text) => strip_summary_text(text).map(SystemPrompt::Text),
        SystemPrompt::Blocks(blocks) => {
            let blocks = blocks
                .into_iter()
                .filter_map(|mut block| {
                    block.text = strip_summary_text(block.text)?;
                    Some(block)
                })
                .collect::<Vec<_>>();
            (!blocks.is_empty()).then_some(SystemPrompt::Blocks(blocks))
        }
    }
}

/// Flatten a committed summary prompt to the text stored in history.
#[must_use]
pub fn summary_prompt_text(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

#[must_use]
pub(crate) fn compaction_checkpoint_message(prompt: &SystemPrompt) -> Message {
    Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: summary_prompt_text(prompt),
                cache_control: None,
            },
            ContentBlock::Text {
                text: COMPACTION_CHECKPOINT_PROVENANCE.to_string(),
                cache_control: None,
            },
        ],
    }
}

/// Whether a history message is a compaction checkpoint. New-format
/// checkpoints are recognised only structurally (see
/// [`is_wire_compaction_checkpoint_message`]); substring matching is kept for
/// the two legacy markers, whose older checkpoints may lack provenance.
#[must_use]
pub(crate) fn is_compaction_checkpoint_message(message: &Message) -> bool {
    is_wire_compaction_checkpoint_message(message)
        || user_text_of(message).is_some_and(|text| is_legacy_compaction_summary_text(&text))
}

/// Request-time recognition: exactly the header text block plus the
/// provenance block. User text merely quoting a marker keeps its original
/// wire position. Checkpoints saved before the handoff note still pass, so
/// their position survives restore and the next pass replaces them.
pub(crate) fn is_wire_compaction_checkpoint_message(message: &Message) -> bool {
    let [
        ContentBlock::Text {
            text,
            cache_control: None,
        },
        ContentBlock::Text {
            text: provenance,
            cache_control: None,
        },
    ] = message.content.as_slice()
    else {
        return false;
    };
    message.role == Role::User
        && (text.starts_with(COMPACTION_SUMMARY_MARKER)
            || text.starts_with(LEGACY_V2_COMPACTION_SUMMARY_MARKER))
        && provenance == COMPACTION_CHECKPOINT_PROVENANCE
}

/// Keep the checkpoint at its original historical boundary on session load.
/// Later user turns must remain after the saved compaction boundary.
pub(crate) fn restore_compaction_checkpoint(
    mut messages: Vec<Message>,
    checkpoint: Option<&SystemPrompt>,
) -> Vec<Message> {
    let typed_position = messages
        .iter()
        .position(is_wire_compaction_checkpoint_message);
    let checkpoint_index = if let Some(index) = typed_position {
        messages.retain(|message| !is_wire_compaction_checkpoint_message(message));
        index
    } else {
        // Legacy sessions have no independent provenance. Only replace
        // header-prefixed messages when a saved checkpoint exists; identical
        // user text is still ambiguous in that legacy format.
        if checkpoint.is_some() {
            messages.retain(|message| !is_compaction_checkpoint_message(message));
        }
        messages.len()
    };
    if let Some(checkpoint) = checkpoint {
        messages.insert(
            checkpoint_index.min(messages.len()),
            compaction_checkpoint_message(checkpoint),
        );
    }
    messages
}

pub(crate) fn estimate_tokens_for_message(message: &Message, include_thinking: bool) -> usize {
    message
        .content
        .iter()
        .map(|c| match c {
            ContentBlock::Text { text, .. } => text.len() / 4,
            // Replay-capable routes retain reasoning even on text-only
            // assistant messages and across later user turns.
            ContentBlock::Thinking { thinking, .. } if include_thinking => thinking.len() / 4,
            ContentBlock::Thinking { .. } => 0,
            ContentBlock::ToolUse { input, .. } => {
                compact_json_len(input).map_or(100, |len| len / 4)
            }
            ContentBlock::ToolResult {
                content,
                content_blocks,
                ..
            } => {
                let images = content_blocks.as_ref().map_or(0, |blocks| {
                    blocks
                        .iter()
                        .filter(|block| {
                            block.get("type").and_then(serde_json::Value::as_str) == Some("image")
                        })
                        .count()
                });
                content.len() / 4 + images * IMAGE_TOKEN_ESTIMATE
            }
            // An inline image is real input the model pays for; estimating it
            // at 0 undercounts the budget and risks overflow in image-heavy
            // sessions. Use a conservative flat per-image estimate (vision
            // tiles are typically ~1k tokens); erring high compacts slightly
            // early rather than overflowing.
            ContentBlock::ImageUrl { .. } => IMAGE_TOKEN_ESTIMATE,
            ContentBlock::ServerToolUse { input, .. } => {
                compact_json_len(input).unwrap_or_default() / 4
            }
            ContentBlock::ToolSearchToolResult { content, .. }
            | ContentBlock::CodeExecutionToolResult { content, .. } => {
                compact_json_len(content).unwrap_or_default() / 4
            }
        })
        .sum::<usize>()
}

fn compact_json_len(value: &serde_json::Value) -> Option<usize> {
    let mut counter = crate::utils::CountingWriter::new();
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.count())
}

/// Conservative flat token estimate for an inline image (`ContentBlock::ImageUrl`).
/// Vision models bill images by resized tile count; ~1k tokens is a safe
/// mid-range estimate that keeps the compaction trigger from under-reading an
/// image-heavy session.
const IMAGE_TOKEN_ESTIMATE: usize = 1000;

pub fn estimate_tokens(messages: &[Message]) -> usize {
    // Rough estimate: ~4 bytes per token. Count every retained reasoning
    // block: DeepSeek/Kimi replay text-only assistant reasoning too. This
    // route-neutral estimate cannot assume a transport will omit it.
    messages
        .iter()
        .map(|message| estimate_tokens_for_message(message, true))
        .sum()
}

pub(crate) fn message_has_tool_use(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
}

/// Conservative text estimate: three characters per token, but never below
/// the bytes/4 estimate the message path uses. Characters alone read
/// multibyte (CJK) text at ~0.33 tokens each, *under* the byte estimate's
/// ~0.75, which made the "conservative" figure the smaller one exactly where
/// it matters.
///
/// Known limitation: both are heuristics, not a tokenizer. CJK costs roughly
/// 0.6-1.5 tokens per character depending on the provider's tokenizer, so a
/// CJK-heavy prompt can still be underestimated; the compaction trigger
/// bounds that by taking the larger of this estimate and the provider-billed
/// prompt size.
pub(crate) fn estimate_text_tokens_conservative(text: &str) -> usize {
    text.chars().count().div_ceil(3).max(text.len().div_ceil(4))
}

fn estimate_system_tokens_conservative(system: Option<&SystemPrompt>) -> usize {
    match system {
        Some(SystemPrompt::Text(text)) => estimate_text_tokens_conservative(text),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| estimate_text_tokens_conservative(&block.text))
            .sum(),
        None => 0,
    }
}

/// Conservative estimate for full request input tokens (messages + system + framing).
#[must_use]
pub fn estimate_input_tokens_conservative(
    messages: &[Message],
    system: Option<&SystemPrompt>,
) -> usize {
    let message_tokens = estimate_tokens(messages).saturating_mul(3).div_ceil(2);
    let system_tokens = estimate_system_tokens_conservative(system);
    let framing_overhead = messages.len().saturating_mul(12).saturating_add(48);
    message_tokens
        .saturating_add(system_tokens)
        .saturating_add(framing_overhead)
}

/// Best-effort estimate of real request input tokens, without the 1.5×
/// safety inflation used by overflow math.
///
/// Compaction *pressure* compares against a threshold whose percentage means
/// "fraction of the context window" on the user-facing meter. Feeding the
/// inflated overflow estimate into that comparison made an 80% setting fire
/// at roughly half the real usage. Overflow protection keeps its inflated
/// estimator; the pressure trigger uses this one, preferring provider-billed
/// prompt tokens when the caller has them.
#[must_use]
pub fn estimate_input_tokens_for_pressure(
    messages: &[Message],
    system: Option<&SystemPrompt>,
) -> usize {
    let message_tokens = estimate_tokens(messages);
    let system_tokens = estimate_system_tokens_conservative(system);
    let framing_overhead = messages.len().saturating_mul(12).saturating_add(48);
    message_tokens
        .saturating_add(system_tokens)
        .saturating_add(framing_overhead)
}

fn estimate_retained_floor_conservative(
    messages: &[Message],
    system_prompt: Option<&SystemPrompt>,
    prepared: &PreparedCompactionEnvelope,
) -> usize {
    let config = &prepared.config;
    let retained = last_round::replacement_messages(messages, config.retained_user_message_tokens);
    let retained_tokens = estimate_tokens(&retained).saturating_mul(3).div_ceil(2);
    let framing = retained.len().saturating_mul(12).saturating_add(48);
    let anchors = user_anchors_section(config.workspace.as_deref());
    let summary_scaffolding_tokens =
        estimate_text_tokens_conservative(&build_compaction_summary_block_text("", &anchors));

    // Post-compaction the committed summary is REPLACED, not stacked, so prior
    // summary blocks must not inflate the floor. Count only the exact installed
    // scaffolding here; the model owns the concise summary length, just as it
    // owns the answer length on an ordinary turn.
    let retained_system_prompt = strip_compaction_summaries(system_prompt);
    retained_tokens
        .saturating_add(estimate_system_tokens_conservative(
            retained_system_prompt.as_ref(),
        ))
        .saturating_add(framing)
        .saturating_add(summary_scaffolding_tokens)
}

/// Whether the current canonical request has reached the configured automatic
/// compaction pressure. This deliberately excludes eligibility/reclaimability:
/// local tool-result pruning uses it to decide when pressure has actually
/// cleared, even if the remaining transcript cannot support an LLM summary.
#[must_use]
pub fn compaction_pressure_reached(
    messages: &[Message],
    system_prompt: Option<&SystemPrompt>,
    config: &CompactionConfig,
) -> bool {
    compaction_pressure_reached_with_billed(messages, system_prompt, config, None)
}

/// Pressure check that additionally honors provider-billed prompt tokens.
///
/// Billed usage is the ground truth for how large the context actually is;
/// the estimator undercounts non-ASCII text and cannot see server-side
/// framing. Whichever signal is higher decides, so an undercounting estimate
/// cannot hide pressure the provider already billed for. Callers must only
/// pass a billed count that describes the message list being checked —
/// post-prune re-checks pass `None` and fall back to the estimate.
#[must_use]
pub fn compaction_pressure_reached_with_billed(
    messages: &[Message],
    system_prompt: Option<&SystemPrompt>,
    config: &CompactionConfig,
    billed_input_tokens: Option<u64>,
) -> bool {
    if !config.enabled {
        return false;
    }
    let billed = billed_input_tokens
        .and_then(|tokens| usize::try_from(tokens).ok())
        .unwrap_or(0);
    // Billing alone proving pressure short-circuits the walk (#perf-r5):
    // `estimated.max(billed) >= threshold` is unconditionally true when
    // `billed >= threshold`, so estimating cannot change the answer and the
    // O(transcript) pass is skipped.
    if billed >= config.token_threshold {
        return true;
    }
    let estimated = estimate_input_tokens_for_pressure(messages, system_prompt);
    estimated.max(billed) >= config.token_threshold
}

/// Estimate-only eligibility check ([`should_compact_with_billed`] with no
/// billed tokens): used by the request preview, where no provider bill exists.
pub fn should_compact(
    messages: &[Message],
    system_prompt: Option<&SystemPrompt>,
    prepared: &PreparedCompactionEnvelope,
) -> bool {
    should_compact_with_billed(messages, system_prompt, prepared, None)
}

/// Why an over-pressure context still did not start an automatic pass.
///
/// A refusal is not a bug by itself — each guard exists for a reason — but a
/// silent refusal is: the user watches a full context meter while
/// auto-compaction appears broken (#5577). Callers surface these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionRefusal {
    /// Too few messages for a summary pass to mean anything.
    TooFewMessages { count: usize },
    /// The conservative retained floor (system prompt + kept messages +
    /// summary allowance) cannot get below the trigger, so a pass would
    /// recur on every step without relieving pressure.
    RetainedFloor { floor: usize, threshold: usize },
}

/// Outcome of the automatic-compaction eligibility check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionDecision {
    /// Disabled, or pressure not reached: nothing to do, nothing to explain.
    NotNeeded,
    /// Start a pass.
    Compact,
    /// Pressure is real but a guard declined; the reason names the guard.
    Refused(CompactionRefusal),
}

/// Eligibility check that honors provider-billed prompt tokens for the
/// pressure gate, mirroring [`compaction_pressure_reached_with_billed`].
pub fn should_compact_with_billed(
    messages: &[Message],
    system_prompt: Option<&SystemPrompt>,
    prepared: &PreparedCompactionEnvelope,
    billed_input_tokens: Option<u64>,
) -> bool {
    matches!(
        compaction_decision_with_billed(messages, system_prompt, prepared, billed_input_tokens),
        CompactionDecision::Compact
    )
}

/// Full eligibility decision, including *why* an over-pressure context was
/// refused, so hosts can tell the user instead of silently holding.
#[must_use]
pub fn compaction_decision_with_billed(
    messages: &[Message],
    system_prompt: Option<&SystemPrompt>,
    prepared: &PreparedCompactionEnvelope,
    billed_input_tokens: Option<u64>,
) -> CompactionDecision {
    let config = &prepared.config;
    if !config.enabled {
        return CompactionDecision::NotNeeded;
    }
    // Pressure gate + prune projection share one estimate (#perf-r5): both
    // consume `estimate_input_tokens_for_pressure` over the same
    // `(messages, system_prompt)`, a pure function, so it is computed at
    // most once. `billed >= threshold` proves pressure without estimating
    // (max is unconditionally >= threshold then); the estimate is deferred
    // until something actually needs it — the prune projection below — so
    // the billed-corner still reaches the TooFew and RetainedFloor guards
    // unchanged, and skips the walk entirely when no prune candidates exist.
    let billed = billed_input_tokens
        .and_then(|tokens| usize::try_from(tokens).ok())
        .unwrap_or(0);
    let estimated: Option<usize> = if billed < config.token_threshold {
        let estimate = estimate_input_tokens_for_pressure(messages, system_prompt);
        if estimate.max(billed) < config.token_threshold {
            return CompactionDecision::NotNeeded;
        }
        Some(estimate)
    } else {
        None
    };

    // The execution path mechanically prunes old verbose tool results before
    // asking the model for a summary. Local pruning alone may be enough to
    // clear pressure even when the transcript is too small for an LLM pass.
    // Project that outcome from the measured plan — per-block deltas use the
    // estimator's own arithmetic, so this equals re-estimating a pruned copy
    // without cloning a multi-megabyte transcript on every step.
    let prune_plan = plan_tool_result_prunes(messages, KEEP_RECENT_MESSAGES);
    if !prune_plan.is_empty() {
        let estimate = match estimated {
            Some(value) => value,
            None => estimate_input_tokens_for_pressure(messages, system_prompt),
        };
        let reclaimed_tokens: usize = prune_plan.iter().map(PlannedPrune::tokens_reclaimed).sum();
        let projected = estimate.saturating_sub(reclaimed_tokens);
        if projected < config.token_threshold.saturating_mul(4) / 5 {
            return CompactionDecision::Compact;
        }
    }

    if messages.len() < MIN_SUMMARIZE_MESSAGES {
        return CompactionDecision::Refused(CompactionRefusal::TooFewMessages {
            count: messages.len(),
        });
    }

    // Reclaimability guard: do not start a pass whose replacement request
    // (system prompt + retained user messages + summary allowance)
    // cannot get below the trigger, or a large stable prefix would cause
    // auto-compaction on every tool step.
    let floor = estimate_retained_floor_conservative(messages, system_prompt, prepared);
    if floor >= config.token_threshold {
        return CompactionDecision::Refused(CompactionRefusal::RetainedFloor {
            floor,
            threshold: config.token_threshold,
        });
    }
    CompactionDecision::Compact
}

/// Whether a compaction pass could shrink this history at all: enough
/// messages to summarize, or old tool output to prune. A one- or two-message
/// conversation that is over budget is over budget because of its fixed
/// prefix or its newest message, and summarizing it only spends a model call
/// before the same failure (experience mark 2).
#[must_use]
pub fn has_compactable_history(messages: &[Message]) -> bool {
    messages.len() >= MIN_SUMMARIZE_MESSAGES
        || !plan_tool_result_prunes(messages, KEEP_RECENT_MESSAGES).is_empty()
}

fn truncate_chars(text: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }
    let start_char = total_chars.saturating_sub(max_chars);
    let start_idx = text
        .char_indices()
        .nth(start_char)
        .map_or(0, |(idx, _)| idx);
    text[start_idx..].to_string()
}

#[derive(Debug, Clone)]
struct ToolUseInfo {
    name: String,
    key: String,
    args_preview: String,
}

fn tool_use_key(name: &str, input: &serde_json::Value) -> String {
    format!(
        "{name}:{}",
        serde_json::to_string(input).unwrap_or_else(|_| input.to_string())
    )
}

fn tool_args_preview(input: &serde_json::Value) -> String {
    let redacted = codewhale_config::persistence::redact_json_secrets(input);
    let raw = serde_json::to_string(&redacted).unwrap_or_else(|_| redacted.to_string());
    truncate_chars(&raw, 120).to_string()
}

fn collect_tool_uses(
    messages: &[Message],
) -> HashMap<codewhale_models::ToolCallKey<'_>, Option<(&str, ToolUseInfo)>> {
    let mut tool_uses = HashMap::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse {
                id, name, input, ..
            } = block
                && let Some(key) = block.tool_call_key()
                && !key.as_str().trim().is_empty()
            {
                // Ambiguous old or malformed new history is left intact; a
                // later call must not supply the earlier result's metadata.
                tool_uses
                    .entry(key)
                    .and_modify(|entry| *entry = None)
                    .or_insert_with(|| {
                        Some((
                            id.as_str(),
                            ToolUseInfo {
                                name: name.clone(),
                                key: tool_use_key(name, input),
                                args_preview: tool_args_preview(input),
                            },
                        ))
                    });
            }
        }
    }
    tool_uses
}

struct ToolResultPruneCandidate {
    message_idx: usize,
    block_idx: usize,
    key: String,
    tool_name: String,
    args_preview: String,
    original_len: usize,
}

fn tool_result_content_blocks_len(content_blocks: Option<&[serde_json::Value]>) -> usize {
    content_blocks
        .and_then(|blocks| serde_json::to_vec(blocks).ok())
        .map_or(0, |bytes| bytes.len())
}

#[cfg(test)]
fn prune_tool_results(messages: &mut [Message], protected_window: usize) -> usize {
    prune_tool_results_until(messages, protected_window, |_, _| false)
}

/// Mechanically prune old verbose tool results before paying for an LLM summary.
///
/// The most recent `protected_window` messages stay byte-for-byte intact. Older
/// duplicate tool results keep the freshest full body and replace earlier
/// copies with one-line summaries; non-duplicate old results are summarized only
/// when they exceed the normal summary snippet size.
fn prune_tool_results_until<F>(
    messages: &mut [Message],
    protected_window: usize,
    mut should_stop: F,
) -> usize
where
    F: FnMut(&[Message], usize) -> bool,
{
    let plan = plan_tool_result_prunes(messages, protected_window);
    let mut bytes_saved = 0usize;
    for planned in plan {
        if let ContentBlock::ToolResult {
            content,
            content_blocks,
            ..
        } = &mut messages[planned.message_idx].content[planned.block_idx]
        {
            bytes_saved = bytes_saved.saturating_add(planned.bytes_reclaimed());
            *content = planned.summary;
            *content_blocks = None;

            if should_stop(messages, bytes_saved) {
                break;
            }
        }
    }
    bytes_saved
}

/// One tool-result replacement the pruner has decided on, measured up front
/// so eligibility checks can project the outcome without cloning a
/// multi-megabyte transcript ([`compaction_decision_with_billed`] used to
/// copy the entire message list every over-pressure step just to ask "would
/// pruning be enough?").
struct PlannedPrune {
    message_idx: usize,
    block_idx: usize,
    summary: String,
    content_len: usize,
    blocks_len: usize,
    image_count: usize,
}

impl PlannedPrune {
    /// Byte reduction this replacement realizes, matching the pruner's
    /// accounting exactly.
    fn bytes_reclaimed(&self) -> usize {
        self.content_len
            .saturating_sub(self.summary.len())
            .saturating_add(self.blocks_len)
    }

    /// Estimator-token reduction, using the same per-block arithmetic as
    /// [`estimate_tokens_for_message`] so a projection built from these
    /// deltas equals re-estimating the pruned transcript.
    fn tokens_reclaimed(&self) -> usize {
        let before = self.content_len / 4 + self.image_count * IMAGE_TOKEN_ESTIMATE;
        let after = self.summary.len() / 4;
        before.saturating_sub(after)
    }
}

/// Decide, without mutating anything, which old tool results pruning would
/// replace. The most recent `protected_window` messages stay untouched; older
/// duplicate results keep the freshest full body; non-duplicates are replaced
/// only when they exceed the summary snippet size.
fn plan_tool_result_prunes(messages: &[Message], protected_window: usize) -> Vec<PlannedPrune> {
    let cutoff = messages.len().saturating_sub(protected_window);
    if cutoff == 0 {
        return Vec::new();
    }

    let tool_uses = collect_tool_uses(messages);
    let mut candidates = Vec::new();
    let mut latest_by_key: HashMap<String, usize> = HashMap::new();
    let mut count_by_key: HashMap<String, usize> = HashMap::new();

    for (message_idx, message) in messages.iter().take(cutoff).enumerate() {
        for (block_idx, block) in message.content.iter().enumerate() {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                content_blocks,
                ..
            } = block
            else {
                continue;
            };
            let Some((provider_id, info)) = block
                .tool_call_key()
                .and_then(|key| tool_uses.get(&key))
                .and_then(Option::as_ref)
            else {
                continue;
            };
            if provider_id != &tool_use_id.as_str() {
                continue;
            }
            latest_by_key.insert(info.key.clone(), message_idx);
            *count_by_key.entry(info.key.clone()).or_insert(0) += 1;
            candidates.push(ToolResultPruneCandidate {
                message_idx,
                block_idx,
                key: info.key.clone(),
                tool_name: info.name.clone(),
                args_preview: info.args_preview.clone(),
                original_len: content
                    .len()
                    .saturating_add(tool_result_content_blocks_len(content_blocks.as_deref())),
            });
        }
    }

    // The maps above are fully populated before planning completes, so the order
    // below only changes which message bytes are rewritten first. Planning from
    // newest to oldest lets the pruner stop as soon as enough bytes were saved,
    // preserving the earlier JSON request prefix for byte-level KV caches.
    candidates.reverse();

    let mut plan = Vec::new();
    for candidate in candidates {
        let duplicate_count = count_by_key.get(&candidate.key).copied().unwrap_or(0);
        let is_latest_duplicate = duplicate_count > 1
            && latest_by_key.get(&candidate.key) == Some(&candidate.message_idx);
        if is_latest_duplicate {
            continue;
        }
        if duplicate_count <= 1 && candidate.original_len <= SUMMARY_TOOL_RESULT_SNIPPET_CHARS {
            continue;
        }

        let summary = format!(
            "[{}] tool result pruned ({} bytes; args: {})",
            candidate.tool_name, candidate.original_len, candidate.args_preview
        );
        if summary.len() >= candidate.original_len {
            continue;
        }

        let ContentBlock::ToolResult {
            content,
            content_blocks,
            ..
        } = &messages[candidate.message_idx].content[candidate.block_idx]
        else {
            continue;
        };
        plan.push(PlannedPrune {
            message_idx: candidate.message_idx,
            block_idx: candidate.block_idx,
            summary,
            content_len: content.len(),
            blocks_len: tool_result_content_blocks_len(content_blocks.as_deref()),
            image_count: content_blocks.as_ref().map_or(0, |blocks| {
                blocks
                    .iter()
                    .filter(|block| {
                        block.get("type").and_then(serde_json::Value::as_str) == Some("image")
                    })
                    .count()
            }),
        });
    }
    plan
}

fn truncate_retained_block(label: &str, content: &mut String, max_chars: usize) -> bool {
    let char_count = content.chars().count();
    if char_count <= max_chars {
        return false;
    }

    let snippet_budget = max_chars.saturating_sub(256).max(1024);
    let head_chars = snippet_budget / 2;
    let tail_chars_budget = snippet_budget.saturating_sub(head_chars);
    let head = truncate_chars(content, head_chars).to_string();
    let tail = tail_chars(content, tail_chars_budget);
    *content =
        format!("[{label} retained-history truncated from {char_count} chars]\n{head}\n…\n{tail}");
    true
}

// Retained reasoning is replay protocol even without a signature (DeepSeek
// tool turns). Summarize older exchanges as units; do not rewrite their peers.
fn sanitize_retained_messages(mut messages: Vec<Message>) -> Vec<Message> {
    for message in &mut messages {
        for block in &mut message.content {
            if let ContentBlock::ToolResult {
                content,
                content_blocks,
                ..
            } = block
                && truncate_retained_block("tool result", content, RETAINED_TOOL_RESULT_MAX_CHARS)
            {
                *content_blocks = None;
            }
        }
    }
    messages
}

/// Result of a compaction operation with metadata.
#[derive(Debug)]
pub struct CompactionResult {
    /// Compacted messages
    pub messages: Vec<Message>,
    /// Host-persistence copy of the history checkpoint.
    pub summary_prompt: Option<SystemPrompt>,
    /// Number of retries used before success
    pub retries_used: u32,
    /// Last-round coverage for inspector receipts.
    pub coverage: CompactionCoverage,
}

/// Classify a compaction LLM failure for the retry / input-ladder policy.
fn classify_compaction_failure(e: &anyhow::Error) -> CompactionFailureKind {
    if let Some(error) = llm_error_in_chain(e) {
        return match error {
            crate::llm_client::LlmError::ContextLengthError(_) => {
                CompactionFailureKind::ContextOverflow
            }
            crate::llm_client::LlmError::QuotaExhausted(_) => CompactionFailureKind::Deterministic,
            error if error.is_retryable() => CompactionFailureKind::Transient,
            _ => CompactionFailureKind::Deterministic,
        };
    }

    let text = e.to_string();
    if is_context_window_error_message(&text) {
        return CompactionFailureKind::ContextOverflow;
    }
    let category = crate::error_taxonomy::classify_error_message(&text);
    match category {
        crate::error_taxonomy::ErrorCategory::Network
        | crate::error_taxonomy::ErrorCategory::RateLimit
        | crate::error_taxonomy::ErrorCategory::Timeout => CompactionFailureKind::Transient,
        _ => CompactionFailureKind::Deterministic,
    }
}

fn llm_error_in_chain(error: &anyhow::Error) -> Option<&crate::llm_client::LlmError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<crate::llm_client::LlmError>())
}

/// Record and render a compaction failure as actionable, credential-safe text.
///
/// This classifies only the error supplied by the failed request; it never
/// infers a cause from later provider failures. Unknown diagnostics stay
/// visible after central secret/path redaction, and the same safe detail is
/// written to the runtime log so a transient status message remains auditable.
#[must_use]
pub fn report_compaction_failure(
    prefix: &str,
    id: &str,
    auto: bool,
    error: &anyhow::Error,
) -> String {
    let raw = error.to_string();
    let safe_raw = crate::safe_label::safe_error_text(&raw);
    tracing::warn!(
        compaction_id = %id,
        auto,
        error = %safe_raw,
        "context compaction failed"
    );
    let detail = match llm_error_in_chain(error) {
        Some(crate::llm_client::LlmError::QuotaExhausted(_)) => {
            "provider plan quota exhausted — switch provider/model or renew the provider plan"
                .to_string()
        }
        Some(crate::llm_client::LlmError::RateLimited { .. }) => {
            "provider rate limit blocked making room — retry after the limit resets or switch provider/model"
                .to_string()
        }
        Some(crate::llm_client::LlmError::AuthenticationError(_)) => {
            "provider authentication failed — sign in or replace the credential, then retry"
                .to_string()
        }
        Some(crate::llm_client::LlmError::AuthorizationError(_)) => {
            "provider authorization rejected making room — verify account access or switch provider/model"
                .to_string()
        }
        _ => match crate::error_taxonomy::classify_error_message(&raw) {
            crate::error_taxonomy::ErrorCategory::RateLimit => {
                "provider rate limit blocked making room — retry after the limit resets or switch provider/model"
                    .to_string()
            }
            crate::error_taxonomy::ErrorCategory::Authentication => {
                "provider authentication failed — sign in or replace the credential, then retry"
                    .to_string()
            }
            crate::error_taxonomy::ErrorCategory::Authorization => {
                "provider authorization rejected making room — verify account access or switch provider/model"
                    .to_string()
            }
            _ => safe_raw,
        },
    };

    format!("{prefix}: {detail}")
}

/// Check if an error is transient and worth retrying. Categories that map to
/// transient retry: Network, RateLimit, Timeout. Context overflow is *not*
/// transient — it needs a smaller input (ladder), not the same payload.
fn is_transient_error(e: &anyhow::Error) -> bool {
    classify_compaction_failure(e).is_transient()
}

fn is_context_window_error_message(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("too long for this model")
        || lower.contains("prompt is too long")
        || lower.contains("maximum prompt length")
        || lower.contains("maximum context length")
        || lower.contains("context_length_exceeded")
        || lower.contains("context window")
        || (lower.contains("context")
            && (lower.contains("token") || lower.contains("too long") || lower.contains("maximum")))
}

/// Compact messages with retry and backoff for transient errors.
///
/// This function wraps `compact_messages` with retry logic to handle
/// transient network errors and rate limits. It uses exponential backoff
/// with delays of 1s, 2s, 4s between retries.
///
/// # Safety
/// - Never panics
/// - Never corrupts the original messages (returns error instead)
/// - Only retries on transient errors (network, rate limit, etc.)
///
/// `invocation_usage` retains every decoded response, including rejected
/// summaries, across retries and cancellation of this future.
pub async fn compact_messages_safe(
    client: &dyn ModelClient,
    messages: &[Message],
    system_prompt: Option<&SystemPrompt>,
    prepared: &PreparedCompactionEnvelope,
    invocation_usage: &mut Usage,
) -> Result<CompactionResult> {
    const MAX_RETRIES: u32 = 3;
    const BASE_DELAY_MS: u64 = 1000;

    // Persist the complete pre-compaction history before any pruning or provider
    // call. Failure leaves the original context intact. The model-authored
    // handoff is saved separately before replacement is returned to the engine.
    let checkpoint_id = uuid::Uuid::new_v4().to_string();
    if let Some(session_id) = prepared.session_id.as_deref() {
        let bytes = serde_json::to_vec(&codewhale_config::persistence::redact_json_secrets(
            &serde_json::to_value(messages)?,
        ))?;
        crate::artifacts::write_session_relative_immutable(
            session_id,
            &std::path::PathBuf::from("artifacts")
                .join(format!("context-transfer-{checkpoint_id}.json")),
            &bytes,
        )?;
    }

    let config = &prepared.config;
    let was_over_threshold = compaction_pressure_reached(messages, system_prompt, config);
    // Leave room for useful work after a local prune. Clearing the trigger
    // by a few tokens caused another prefix rewrite on the next tool result.
    let prune_target = config.token_threshold.saturating_mul(4) / 5;
    let mut pruned_messages = messages.to_vec();
    let mut now_under_threshold = false;
    let mut next_stop_check_bytes = 0usize;
    let pruned_bytes = prune_tool_results_until(
        &mut pruned_messages,
        KEEP_RECENT_MESSAGES,
        |candidate_messages, bytes_saved| {
            if !was_over_threshold || bytes_saved < next_stop_check_bytes {
                return false;
            }

            // Stop at the first suffix-side prune check that clears the target.
            // The check itself is a full compaction-plan pass, so bound it by saved
            // bytes instead of running it after every candidate in huge sessions.
            next_stop_check_bytes = bytes_saved.saturating_add(TOOL_PRUNE_STOP_CHECK_BYTES);
            now_under_threshold =
                estimate_input_tokens_for_pressure(candidate_messages, system_prompt)
                    < prune_target;
            now_under_threshold
        },
    );
    if was_over_threshold && pruned_bytes > 0 && !now_under_threshold {
        // The throttled in-loop check may skip the exact candidate that clears the
        // budget. Do one final pass so a successful local prune still avoids LLM compaction.
        now_under_threshold =
            estimate_input_tokens_for_pressure(&pruned_messages, system_prompt) < prune_target;
    }

    if pruned_bytes > 0 {
        logging::info(format!(
            "Local tool-result prune saved {pruned_bytes} bytes before LLM compaction"
        ));
        if was_over_threshold && now_under_threshold {
            let kept = sanitize_retained_messages(pruned_messages);
            last_round::validate_last_round_coverage(messages, &kept)?;
            let coverage = last_round::measure_coverage(
                messages,
                &kept,
                CompactionPath::PruneOnly,
                pinned_anchors_text(config.workspace.as_deref())
                    .map(|text| text.chars().count())
                    .unwrap_or(0),
            );
            return Ok(CompactionResult {
                messages: kept,
                summary_prompt: None,
                retries_used: 0,
                coverage,
            });
        }
    }

    let mut last_error: Option<anyhow::Error> = None;
    let mut quality_retries = 0u32;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            // Exponential backoff: 1s, 2s, 4s
            let delay = Duration::from_millis(BASE_DELAY_MS * (1 << (attempt - 1)));
            tokio::time::sleep(delay).await;
        }

        match compact_messages_with_metadata(
            client,
            // If a local prune cannot clear pressure, summarize the original
            // evidence. Pruning first both erased facts the handoff needs and
            // invalidated the cached history prefix for the summary request.
            messages,
            config,
            system_prompt,
            prepared.tools.as_deref(),
            prepared.reasoning_effort.as_deref(),
            prepared.notice_sink.as_deref(),
            &mut quality_retries,
            invocation_usage,
        )
        .await
        {
            Ok((msgs, prompt, mut coverage)) => {
                let kept = sanitize_retained_messages(msgs);
                last_round::validate_last_round_coverage(messages, &kept)?;
                if config.enabled
                    && compaction_pressure_reached(messages, system_prompt, config)
                    && estimate_input_tokens_for_pressure(&kept, system_prompt)
                        >= estimate_input_tokens_for_pressure(messages, system_prompt)
                {
                    anyhow::bail!(
                        "Making room did not shrink the context; the original conversation was preserved."
                    );
                }
                let keep: CompactionKeep = inspect_compaction_keep(&kept);
                coverage.last_round_messages = keep.last_round_messages;
                coverage.last_round_tool_results = keep.last_round_tool_results;
                coverage.last_round_assistant = keep.last_round_assistant;
                if let (Some(session_id), Some(summary)) =
                    (prepared.session_id.as_deref(), prompt.as_ref())
                {
                    let text = summary_prompt_text(summary);
                    let redacted = codewhale_config::persistence::redact_json_secrets(
                        &serde_json::Value::String(text),
                    );
                    crate::artifacts::write_session_relative_immutable(
                        session_id,
                        &std::path::PathBuf::from("artifacts")
                            .join(format!("context-transfer-{checkpoint_id}.md")),
                        redacted.as_str().unwrap_or_default().as_bytes(),
                    )?;
                }
                return Ok(CompactionResult {
                    messages: kept,
                    summary_prompt: prompt,
                    retries_used: attempt.saturating_add(quality_retries),
                    coverage,
                });
            }
            Err(e) => {
                // Only retry on transient errors
                if !is_transient_error(&e) {
                    return Err(e);
                }
                last_error = Some(e);
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("Making room failed after {MAX_RETRIES} retries")))
}

pub(crate) fn build_compaction_summary_block_text(summary: &str, anchors: &str) -> String {
    let summary = summary.trim();
    let summary = if summary.is_empty() {
        "(no summary available)"
    } else {
        summary
    };
    let mut text = format!("{SUMMARY_HEADER}\n\n{summary}");
    text.push_str(anchors);
    text.push_str("\n\n");
    text.push_str(SUMMARY_CLOSING);
    text
}

/// Codex-parity replacement history: the most recent user-role messages,
/// selected newest-first within a fixed token budget and restored to
/// transcript order. Content boundaries carry runtime provenance and image
/// turns, so structured messages are retained whole or dropped whole. Only a
/// single text block can be truncated to fit the remaining budget.
/// Result blocks answer a tool call that lives in an earlier message. A
/// retained older turn has already lost that call to the summary, so a kept
/// result block becomes an orphan providers reject outright (#6119).
fn is_orphaned_result_block(block: &ContentBlock) -> bool {
    matches!(
        block,
        ContentBlock::ToolResult { .. }
            | ContentBlock::ToolSearchToolResult { .. }
            | ContentBlock::CodeExecutionToolResult { .. }
    )
}

pub(crate) fn retained_user_messages(messages: &[Message], max_tokens: usize) -> Vec<Message> {
    let mut selected: Vec<Message> = Vec::new();
    let mut remaining = max_tokens;
    for msg in messages.iter().rev() {
        if remaining == 0 {
            break;
        }
        if msg.role != Role::User
            || crate::runtime_handoff::is_runtime_owned_user_message(msg)
            || (user_text_of(msg).is_none()
                && !msg
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ImageUrl { .. })))
        {
            continue;
        }
        if is_compaction_checkpoint_message(msg) {
            continue;
        }
        let tokens: usize = msg
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text, .. } => estimate_text_tokens_conservative(text),
                ContentBlock::ImageUrl { .. } => IMAGE_TOKEN_ESTIMATE,
                _ => 0,
            })
            .sum();
        let mut retained = msg.clone();
        if tokens <= remaining {
            // Keep the text and images; never a result block whose call was
            // summarized away with the region around it (#6119).
            retained
                .content
                .retain(|block| !is_orphaned_result_block(block));
            remaining -= tokens;
        } else {
            let [ContentBlock::Text { text, .. }] = retained.content.as_mut_slice() else {
                // Never flatten or partially retain an engine metadata block:
                // that would turn runtime-owned traffic into a user prompt.
                break;
            };
            *text = truncate_chars(text, remaining.saturating_mul(3)).to_string();
            remaining = 0;
        }
        selected.push(retained);
    }
    selected.reverse();
    selected
}

/// User-pinned facts from `/anchor` (`.codewhale/anchors.md`). These are the
/// user's own words, re-stated after the summary because the command promises
/// they survive compaction.
fn user_anchors_section(workspace: Option<&std::path::Path>) -> String {
    match pinned_anchors_text(workspace) {
        Some(contents) => format!("\n\nUser-pinned anchors (verbatim):\n{contents}"),
        None => String::new(),
    }
}

#[cfg(test)]
async fn compact_messages(
    client: &dyn ModelClient,
    messages: &[Message],
    config: &CompactionConfig,
) -> Result<(Vec<Message>, Option<SystemPrompt>, Vec<Message>)> {
    let mut quality_retries = 0;
    let mut invocation_usage = Usage::default();
    let (messages, summary_prompt, _coverage) = compact_messages_with_metadata(
        client,
        messages,
        config,
        None,
        None,
        None,
        None,
        &mut quality_retries,
        &mut invocation_usage,
    )
    .await?;
    Ok((messages, summary_prompt, Vec::new()))
}

async fn compact_messages_with_metadata(
    client: &dyn ModelClient,
    messages: &[Message],
    config: &CompactionConfig,
    system_prompt: Option<&SystemPrompt>,
    tools: Option<&[Tool]>,
    reasoning_effort: Option<&str>,
    notice_sink: Option<&dyn CompactionNoticeSink>,
    quality_retries: &mut u32,
    invocation_usage: &mut Usage,
) -> Result<(Vec<Message>, Option<SystemPrompt>, CompactionCoverage)> {
    if messages.is_empty() {
        return Ok((Vec::new(), None, CompactionCoverage::default()));
    }

    let summary = create_summary(
        client,
        messages,
        config,
        system_prompt,
        tools,
        reasoning_effort,
        notice_sink,
        quality_retries,
        invocation_usage,
    )
    .await?;
    let anchors = user_anchors_section(config.workspace.as_deref());
    let checkpoint_text = build_compaction_summary_block_text(&summary, &anchors);
    let summary_block = SystemBlock {
        block_type: "text".to_string(),
        text: checkpoint_text.clone(),
        cache_control: config.cache_summary.then(|| CacheControl {
            cache_type: "ephemeral".to_string(),
        }),
    };

    let retained = last_round::build_replacement_history(
        messages,
        &checkpoint_text,
        pinned_anchors_text(config.workspace.as_deref()).as_deref(),
        config.retained_user_message_tokens,
    )?;
    let mut coverage = last_round::measure_coverage(
        messages,
        &retained,
        CompactionPath::Summary,
        pinned_anchors_text(config.workspace.as_deref())
            .map(|text| text.chars().count())
            .unwrap_or(0),
    );
    // Report the tuning actually in force so the receipt shows the operator
    // their knobs took effect (#5956).
    coverage.retained_user_message_tokens = config.retained_user_message_tokens;
    coverage.operator_instructions_applied =
        operator_instructions_section(config.summary_instructions.as_deref()).is_some();
    Ok((
        retained,
        Some(SystemPrompt::Blocks(vec![summary_block])),
        coverage,
    ))
}

/// Delimiters around the operator's standing summarizer instructions. The
/// summarizer sees a plain user message, so the section must announce itself:
/// unfenced free text reads as more conversation to summarize.
const OPERATOR_INSTRUCTIONS_HEADER: &str = "--- Additional instructions from the operator ---";
const OPERATOR_INSTRUCTIONS_FOOTER: &str = "--- End of additional instructions ---";

/// Render `[compaction] summary_instructions` as a delimited prompt suffix.
///
/// `None` (unset, or whitespace-only) produces no section at all, which is
/// what keeps the default prompt byte-identical to the pre-#5956 constant.
/// The cap is enforced here rather than at config load so the warning fires
/// once per compaction pass instead of once per turn.
fn operator_instructions_section(instructions: Option<&str>) -> Option<String> {
    let text = instructions
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let max_chars = crate::config::COMPACTION_SUMMARY_INSTRUCTIONS_MAX_CHARS;
    let text = if text.chars().count() > max_chars {
        logging::warn(format!(
            "[compaction] summary_instructions is longer than {max_chars} characters; \
             the summarizer prompt suffix was truncated"
        ));
        truncate_chars(text, max_chars)
    } else {
        text
    };
    Some(format!(
        "\n\n{OPERATOR_INSTRUCTIONS_HEADER}\n{text}\n{OPERATOR_INSTRUCTIONS_FOOTER}"
    ))
}

fn compact_prompt(focus: Option<&str>, instructions: Option<&str>) -> String {
    with_instructions_and_focus(
        format!("{} {COMPACTION_LANGUAGE_CONTRACT}", compact_prompt_body()),
        focus,
        instructions,
    )
}

fn compact_quality_retry_prompt(focus: Option<&str>, instructions: Option<&str>) -> String {
    with_instructions_and_focus(
        format!(
            "The previous reply was empty or a placeholder, so there is still no handoff note. \
Write it now, with real content from this session under each heading.\n\n{}\n\n\
{HANDOFF_FOLD_IN_RULE}\n\n{HANDOFF_CLOSING_RULE} Do not refuse or return a placeholder. \
{COMPACTION_LANGUAGE_CONTRACT}",
            handoff_sections_text()
        ),
        focus,
        instructions,
    )
}

/// Standing operator instructions first, then the one-off `/compact <focus>`.
fn with_instructions_and_focus(
    mut prompt: String,
    focus: Option<&str>,
    instructions: Option<&str>,
) -> String {
    if let Some(section) = operator_instructions_section(instructions) {
        prompt.push_str(&section);
    }
    if let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        let _ = write!(prompt, "\n\n{HANDOFF_FOCUS_LINE} {focus}");
    }
    prompt
}

fn validate_compaction_summary(summary: &str) -> Result<()> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        anyhow::bail!("The summary for making room was unusable: no text was returned.");
    }

    // Strip every non-word edge, not just ASCII punctuation. Providers can
    // return visually non-empty Unicode punctuation or emoji-only payloads;
    // neither is a usable continuation checkpoint. `is_alphanumeric` keeps
    // this language-neutral for CJK and other scripts without imposing a
    // prose-length heuristic.
    let normalized = trimmed
        .trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_ascii_lowercase();
    if normalized.is_empty() {
        anyhow::bail!(
            "The summary for making room was unusable: only whitespace or punctuation was returned."
        );
    }
    if matches!(
        normalized.as_str(),
        "no summary available"
            | "summary unavailable"
            | "no summary"
            | "n/a"
            | "na"
            | "not available"
            | "i cannot provide a summary"
            | "i can't provide a summary"
            | "unable to provide a summary"
    ) {
        anyhow::bail!("The summary for making room was unusable: a placeholder was returned.");
    }
    Ok(())
}

/// Drop the oldest history message before retrying an over-window summary
/// request, plus any tool results the removal orphans (strict providers
/// reject unpaired results). `messages` ends with the handoff instruction.
///
/// The newest checkpoint is never dropped: it carries the previous handoff
/// note, and without it the fold-in has nothing to carry forward, so user
/// corrections and limits would vanish without notice. Returns `false`, having
/// changed nothing, when no other history message can go and at least one
/// would remain; the caller then fails the pass instead of summarizing
/// without the note.
fn drop_oldest_history_messages(messages: &mut Vec<Message>) -> bool {
    let instruction = messages.len().saturating_sub(1);
    let history = &messages[..instruction];
    let mut keep = history
        .iter()
        .rposition(is_wire_compaction_checkpoint_message);
    let Some(index) = (0..instruction).find(|&index| Some(index) != keep) else {
        return false;
    };
    if history.len() <= 1 {
        return false;
    }
    messages.remove(index);
    if keep.is_some_and(|keep_at| keep_at > index) {
        keep = keep.map(|keep_at| keep_at - 1);
    }
    while index + 1 < messages.len()
        && Some(index) != keep
        && messages[index].content.iter().any(is_orphaned_result_block)
    {
        messages.remove(index);
        if keep.is_some_and(|keep_at| keep_at > index) {
            keep = keep.map(|keep_at| keep_at - 1);
        }
    }
    true
}

/// The summary request for one compaction pass: the parent turn's exact
/// request inputs — model, system prompt, tools, reasoning tier, and stored
/// history in order — plus one trailing user instruction. Only per-request
/// controls differ (output cap, streaming, tool choice), so the provider's
/// prefix cache covers the history the turn already paid for (#6540).
///
/// Known limit: `tool_choice: "none"` keeps the summary from executing tools.
/// Anthropic Messages documents a `tool_choice` change as invalidating the
/// cached *message* blocks (system and tools stay cached), so on those routes
/// the history is still re-read uncached. The Responses (Codex) builder does
/// not send this field; its cache effect on Chat Completions routes has not
/// been measured live.
pub(crate) fn compaction_summary_request(
    history: Vec<Message>,
    config: &CompactionConfig,
    system_prompt: Option<&SystemPrompt>,
    tools: Option<&[Tool]>,
    reasoning_effort: Option<&str>,
    max_tokens: u32,
) -> MessageRequest {
    MessageRequest {
        model: config.model.clone(),
        messages: history,
        max_tokens,
        system: system_prompt.cloned(),
        tools: tools.map(<[Tool]>::to_vec),
        // Tool schemas stay for prefix parity; execution stays off.
        tool_choice: tools
            .filter(|tools| !tools.is_empty())
            .map(|_| serde_json::json!("none")),
        metadata: None,
        thinking: None,
        reasoning_effort: reasoning_effort.map(str::to_string),
        stream: Some(false),
        // Route parity with ordinary turns: turns send no sampling
        // params, so every provider's own normalization/defaults apply.
        // A hard-coded 0.3 leaked to the wire on routes that pass
        // temperature through (e.g. Kimi Code membership), where the
        // fixed-sampling contract rejects it and the whole compaction
        // pass fails.
        temperature: None,
        top_p: None,
    }
}

async fn create_summary(
    client: &dyn ModelClient,
    messages: &[Message],
    config: &CompactionConfig,
    system_prompt: Option<&SystemPrompt>,
    tools: Option<&[Tool]>,
    reasoning_effort: Option<&str>,
    notice_sink: Option<&dyn CompactionNoticeSink>,
    quality_retries: &mut u32,
    invocation_usage: &mut Usage,
) -> Result<String> {
    // The summarization request IS the live conversation plus one final user
    // message asking for the handoff summary, so the provider's prefix cache
    // covers everything already sent this session.
    let mut request_messages = messages.to_vec();
    let stripped_images = crate::image_attach::strip_images_when_unsupported(
        &mut request_messages,
        config.image_input,
        &config.model,
    );
    if stripped_images > 0 {
        logging::warn(format!(
            "Compaction omitted {stripped_images} image block(s) unsupported by its route"
        ));
    }
    request_messages.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: compact_prompt(
                config.focus.as_deref(),
                config.summary_instructions.as_deref(),
            ),
            cache_control: None,
        }],
    });

    let mut quality_retry_used = false;
    // Request-size ladder: an HTTP 413 caps the request *body*, which the
    // token-side budget cannot predict (a flat per-image token estimate can
    // hide megabytes of base64). A refused summary call gets two byte-side
    // downgrades before it fails — re-encode the inline images smaller, then
    // replace them with text notes — each followed by exactly one retry.
    let mut size_ladder = RequestSizeLadder::Start;
    loop {
        // Codex compaction is a normal model generation over the existing
        // cached prefix. Do the same here: the resolved route decides how
        // much output the model may need instead of imposing a smaller,
        // compaction-only ceiling that can be consumed by hidden reasoning.
        let cost_route = client.effective_route_envelope(&config.model, chrono::Utc::now());
        let request = compaction_summary_request(
            request_messages.clone(),
            config,
            system_prompt,
            tools,
            reasoning_effort,
            client.effective_max_output_tokens(&cost_route.model),
        );

        // Capture the session scope before awaiting so a late response cannot
        // accrue into a subsequently loaded/new session.
        let accounting_origin = notice_sink.and_then(CompactionNoticeSink::accounting_origin);
        let cost_scope = accounting_origin
            .as_ref()
            .map_or_else(crate::cost_status::scope_token, |origin| origin.0);
        let response = match client.create_message(request).await {
            Ok(response) => response,
            // A byte-side size rejection can also read like a length problem
            // (gateway HTML pages carry their own wording); the size ladder
            // owns it, not the drop-oldest ladder.
            Err(err) if !is_request_too_large_error(&err) && is_context_window_error(&err) => {
                if !drop_oldest_history_messages(&mut request_messages) {
                    logging::warn(
                        "Compaction summary input is over the context window with nothing \
                         left to drop but the previous handoff note; the pass fails instead \
                         of losing that note",
                    );
                    return Err(err);
                }
                logging::warn(format!(
                    "Compaction summary input over the context window ({err}); \
                     dropped the oldest history item and retrying"
                ));
                continue;
            }
            Err(err) if is_request_too_large_error(&err) => match size_ladder {
                RequestSizeLadder::Start => {
                    // Decoding, resizing and re-encoding megabytes of inline
                    // images is CPU-bound work; run it off the async worker so
                    // the engine keeps servicing events while it happens.
                    let mut outbound = std::mem::take(&mut request_messages);
                    let joined = tokio::task::spawn_blocking(move || {
                        let shrunk = crate::image_attach::shrink_images_for_request(&mut outbound);
                        (outbound, shrunk)
                    })
                    .await;
                    let (outbound, shrunk) = match joined {
                        Ok(joined) => joined,
                        Err(join_error) => {
                            return Err(err.context(format!(
                                "The summary request exceeded the provider's request-body limit and re-encoding its inline images failed: {join_error}"
                            )));
                        }
                    };
                    request_messages = outbound;
                    if shrunk.images_seen == 0 {
                        return Err(err.context(
                            "The summary request exceeded the provider's request-body limit and the history carries no inline images to re-encode",
                        ));
                    }
                    size_ladder = RequestSizeLadder::ImagesShrunk;
                    if shrunk.images > 0 {
                        let message = format!(
                            "Making room exceeded the provider's request-body limit (HTTP 413); re-encoded {} inline image(s) smaller ({} to {}) and is retrying the summary.",
                            shrunk.images,
                            crate::image_attach::human_bytes(shrunk.bytes_before),
                            crate::image_attach::human_bytes(shrunk.bytes_after),
                        );
                        logging::warn(&message);
                        deliver_compaction_notice(notice_sink, message);
                        continue;
                    }
                    // Images are present but every one already fits its share of
                    // the byte budget, and the body was still refused: the cap
                    // sits below the budget. A retry would send identical bytes,
                    // so replace the images now instead of failing the pass.
                    if let Err(replace_error) =
                        replace_inline_images_for_retry(&mut request_messages, notice_sink)
                    {
                        return Err(err.context(format!(
                            "The summary request exceeded the provider's request-body limit; {replace_error}"
                        )));
                    }
                    size_ladder = RequestSizeLadder::ImagesReplaced;
                    continue;
                }
                RequestSizeLadder::ImagesShrunk => {
                    if let Err(replace_error) =
                        replace_inline_images_for_retry(&mut request_messages, notice_sink)
                    {
                        return Err(err.context(format!(
                            "The summary request still exceeded the provider's request-body limit and {replace_error}"
                        )));
                    }
                    size_ladder = RequestSizeLadder::ImagesReplaced;
                    continue;
                }
                RequestSizeLadder::ImagesReplaced => {
                    return Err(err.context(
                        "The summary request exceeded the provider's request-body limit even after re-encoding and then replacing every inline image",
                    ));
                }
            },
            Err(err) => return Err(err),
        };

        // Keep the caller's total before any validation or subsequent await.
        // A rejected summary or canceled retry still consumed these tokens.
        crate::core::turn::add_usage_to(invocation_usage, &response.usage);

        let source_id = format!(
            "compaction:dispatch:{}:response:{}",
            cost_route
                .dispatched_at
                .timestamp_nanos_opt()
                .unwrap_or_default(),
            response.id
        );
        if let Some((scope, session, agent)) = accounting_origin.as_ref() {
            if crate::core::engine::turn_loop::usage_has_reported_data(&response.usage) {
                if let Some(owner) = config.runtime_cost_owner.as_deref() {
                    crate::cost_status::report_effective_route_for_runtime(
                        *scope,
                        Some(owner),
                        &source_id,
                        &cost_route,
                        &response.usage,
                    );
                } else {
                    crate::cost_status::report_effective_route_for_interactive_origin(
                        *scope,
                        session,
                        agent,
                        &source_id,
                        &cost_route,
                        &response.usage,
                    );
                }
            } else if let Some(owner) = config.runtime_cost_owner.as_deref() {
                crate::cost_status::report_unreceipted_provider_success(
                    *scope,
                    Some(owner),
                    &source_id,
                    &cost_route,
                );
            } else {
                crate::cost_status::report_unreceipted_for_interactive_origin(
                    *scope,
                    session,
                    agent,
                    &source_id,
                    &cost_route,
                );
            }
        } else {
            crate::cost_status::report_effective_route_for_runtime(
                cost_scope,
                config.runtime_cost_owner.as_deref(),
                &source_id,
                &cost_route,
                &response.usage,
            );
        }
        if let Some(sink) = notice_sink {
            sink.settled_usage(&source_id, &cost_route, &response.usage)
                .await;
        }

        // Usage above is already billed; a provider-declared incomplete
        // summary must still fail rather than replace the session history
        // with a fragment.
        if codewhale_models::is_incomplete_stop_reason(response.stop_reason.as_deref()) {
            anyhow::bail!(
                "The summary for making room was incomplete: provider stop reason `{}`; the partial summary was not accepted.",
                codewhale_models::stop_reason_detail(response.stop_reason.as_deref())
            );
        }
        if response
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
        {
            anyhow::bail!(
                "Making room returned a tool call instead of a summary; the original conversation was preserved."
            );
        }

        let summary = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if let Err(error) = validate_compaction_summary(&summary) {
            if quality_retry_used {
                return Err(error.context(
                    "Compaction summary remained unusable after one conservative retry; \
no replacement checkpoint was committed",
                ));
            }

            quality_retry_used = true;
            *quality_retries = (*quality_retries).saturating_add(1);
            logging::warn(
                "Compaction provider returned an unusable successful response; retrying once with the conservative handoff prompt",
            );
            let Some(instruction) = request_messages.last_mut() else {
                return Err(error.context(
                    "Compaction summary validation failed and the retry instruction was missing",
                ));
            };
            instruction.content = vec![ContentBlock::Text {
                text: compact_quality_retry_prompt(
                    config.focus.as_deref(),
                    config.summary_instructions.as_deref(),
                ),
                cache_control: None,
            }];
            continue;
        }

        return Ok(summary);
    }
}

/// How far the request-size ladder for one summary call has descended.
///
/// The ladder exists because HTTP 413 rejects the request *body* by bytes,
/// which the token-side context budget that governs compaction cannot see.
/// Each rung is one retry: re-encode the inline images under a byte budget,
/// then replace them with text notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestSizeLadder {
    /// No size downgrade applied yet.
    Start,
    /// Inline images were re-encoded smaller.
    ImagesShrunk,
    /// Inline images were replaced with text notes.
    ImagesReplaced,
}

/// Whether the provider refused the request body for size (HTTP 413 and its
/// common wordings). A smaller payload can succeed where this one did not,
/// which is exactly what the request-size ladder trades on.
///
/// Walks the whole error chain: the rejection may be stated by a fronting
/// gateway (an HTML page from openresty reading "413 Request Entity Too
/// Large") and then wrapped again by the client's own error text, so the
/// top-level message alone is not reliable.
fn is_request_too_large_error(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        let lower = cause.to_string().to_lowercase();
        // The client always renders the status code into the message
        // ("HTTP 413"), so that token is the primary signal; the phrase list
        // below catches gateways that state the condition without it.
        lower.contains("http 413")
            || lower.contains("payload too large")
            || lower.contains("request entity too large")
            || lower.contains("request body too large")
            || lower.contains("length limit exceeded")
    })
}

/// Forward one user-visible progress sentence, when the host supplied a sink.
fn deliver_compaction_notice(sink: Option<&dyn CompactionNoticeSink>, message: String) {
    if let Some(sink) = sink {
        sink.notice(message);
    }
}

/// Replace every inline image for one retry and announce it.
///
/// Shared by the two rungs that can reach the replace step: after a shrink
/// pass that changed something, and directly when the images already fit the
/// byte budget but the body was refused anyway.
fn replace_inline_images_for_retry(
    messages: &mut [Message],
    notice_sink: Option<&dyn CompactionNoticeSink>,
) -> Result<usize> {
    let replaced = crate::image_attach::replace_images_with_placeholders(
        messages,
        "the summary request exceeded the provider's request-body limit (HTTP 413)",
    );
    if replaced == 0 {
        anyhow::bail!("no inline images were left to replace");
    }
    let message = format!(
        "Making room still exceeded the provider's request-body limit; replaced {replaced} inline image(s) with text notes for this summary pass and is retrying."
    );
    logging::warn(&message);
    deliver_compaction_notice(notice_sink, message);
    Ok(replaced)
}

fn is_context_window_error(e: &anyhow::Error) -> bool {
    let text = e.to_string();
    if crate::error_taxonomy::classify_error_message(&text)
        != crate::error_taxonomy::ErrorCategory::InvalidInput
    {
        return false;
    }

    // Only genuine overflow wording drops history. The category alone is far
    // too wide: it now covers every rejected request (#6843), so a bare
    // `token` or `maximum` ("max_tokens must be <= 8192", "temperature exceeds
    // maximum 2") must not peel the summary input away on each retry.
    let lower = text.to_lowercase();
    is_context_window_error_message(&text)
        || (lower.contains("requested") && lower.contains("tokens") && lower.contains("maximum"))
}

/// Collect text from a user message without treating tool-result payloads
/// as new user instructions.
fn user_text_of(msg: &Message) -> Option<String> {
    if msg.role != "user" {
        return None;
    }
    let text = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
#[path = "compaction/tests.rs"]
mod quota_tests;

#[cfg(test)]
mod tests {
    use codewhale_models::{ImageUrlContent, Message};

    #[test]
    fn legacy_checkpoint_headings_are_recognised_but_quotes_are_not() {
        for text in [
            "## 📋 Conversation Summary (Auto-Generated)\n\nkey facts.",
            "## Pinned Facts (User Anchors)\n\n- keep tabs\n\n---\n\n## 📋 Conversation Summary (Auto-Generated)\n\nfacts",
            "Conversation Summary (Auto-Generated)\nold",
            "Another language model started to solve this problem\nold",
        ] {
            assert!(is_legacy_compaction_summary_text(text), "{text}");
        }
        for text in [
            "Explain: ## 📋 Conversation Summary (Auto-Generated)",
            "## Pinned Facts (User Anchors)\nsee Conversation Summary (Auto-Generated) above",
            "Codewhale handoff note\nnot structural",
        ] {
            assert!(!is_legacy_compaction_summary_text(text), "{text}");
        }
    }

    #[test]
    fn restore_without_typed_checkpoint_preserves_user_marker_quotes() {
        // The current marker is recognised only structurally (header block +
        // provenance block), so only the legacy substring markers apply here.
        for marker in [
            LEGACY_V2_COMPACTION_SUMMARY_MARKER,
            LEGACY_COMPACTION_SUMMARY_MARKER,
        ] {
            let quoted = Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("Explain this marker: {marker}"),
                    cache_control: None,
                }],
            };
            let legacy = Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("{marker}\nold summary"),
                    cache_control: None,
                }],
            };
            let messages = vec![quoted.clone(), legacy.clone()];
            assert_eq!(
                restore_compaction_checkpoint(messages.clone(), None),
                messages
            );

            let summary = SystemPrompt::Text(build_compaction_summary_block_text("Summary", ""));
            let restored =
                restore_compaction_checkpoint(vec![quoted.clone(), legacy], Some(&summary));
            assert_eq!(restored.len(), 2);
            assert_eq!(restored[0], quoted);
            assert!(is_wire_compaction_checkpoint_message(&restored[1]));
        }
    }

    #[test]
    fn restore_replaces_duplicate_generated_checkpoints_without_deleting_user_quote() {
        let summary = SystemPrompt::Text(build_compaction_summary_block_text("Summary", ""));
        let generated = compaction_checkpoint_message(&summary);
        let user_quote = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: summary_prompt_text(&summary),
                cache_control: None,
            }],
        };
        assert!(!is_wire_compaction_checkpoint_message(&user_quote));
        let restored = restore_compaction_checkpoint(
            vec![generated.clone(), user_quote.clone(), generated],
            Some(&summary),
        );
        assert_eq!(restored.len(), 2);
        assert!(is_wire_compaction_checkpoint_message(&restored[0]));
        assert_eq!(restored[1], user_quote);

        // With a saved summary, legacy header-prefixed copies are replaced.
        let legacy = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!("{LEGACY_V2_COMPACTION_SUMMARY_MARKER}\nold summary"),
                cache_control: None,
            }],
        };
        let legacy_restored =
            restore_compaction_checkpoint(vec![legacy.clone(), legacy], Some(&summary));
        assert_eq!(legacy_restored.len(), 1);
        assert!(is_wire_compaction_checkpoint_message(&legacy_restored[0]));
    }

    #[test]
    fn restore_anchors_at_the_real_carrier_when_a_pasted_summary_precedes_it() {
        // Mirror order of the duplicate-carrier test above: the pasted full
        // summary comes BEFORE the real provenance carrier — the order in
        // which a content-based anchor would land on the pasted turn's
        // index. Restore must anchor at the real carrier's index, replace
        // the carrier with the saved summary there, and keep the pasted
        // turn verbatim as user content. The carrier's text differs from
        // the saved summary so the assertions cannot pass by leaving the
        // history untouched.
        let pasted_summary = SystemPrompt::Text(build_compaction_summary_block_text(
            "Please analyze this text",
            "",
        ));
        let pasted = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: summary_prompt_text(&pasted_summary),
                cache_control: None,
            }],
        };
        assert!(
            !is_wire_compaction_checkpoint_message(&pasted),
            "a pasted summary without the provenance block is user content"
        );
        let carrier = compaction_checkpoint_message(&SystemPrompt::Text(
            build_compaction_summary_block_text("Compacted summary", ""),
        ));
        let authoritative = SystemPrompt::Text(build_compaction_summary_block_text(
            "Authoritative summary",
            "",
        ));
        let restored =
            restore_compaction_checkpoint(vec![pasted.clone(), carrier], Some(&authoritative));
        assert_eq!(
            restored.len(),
            2,
            "restore must not drop the pasted turn: {restored:?}"
        );
        assert_eq!(
            restored[0], pasted,
            "the pasted full summary must survive restore verbatim"
        );
        assert!(
            is_wire_compaction_checkpoint_message(&restored[1]),
            "the real carrier keeps the anchor position: {restored:?}"
        );
        assert_eq!(
            restored[1],
            compaction_checkpoint_message(&authoritative),
            "the saved summary replaces the carrier at its index: {restored:?}"
        );
    }

    #[test]
    fn inline_image_estimates_nonzero_tokens() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/png;base64,AAAA".to_string(),
                },
            }],
        };
        assert!(
            estimate_tokens_for_message(&msg, false) >= IMAGE_TOKEN_ESTIMATE,
            "an inline image must not estimate to 0 tokens"
        );
    }

    use super::*;
    use serde_json::json;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: Role::from(role),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn prepared(config: &CompactionConfig) -> PreparedCompactionEnvelope {
        PreparedCompactionEnvelope::new(config.clone())
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
                thought_signature: None,
            }],
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    #[test]
    fn truncate_chars_respects_unicode_boundaries() {
        let text = "abc😀é";
        assert_eq!(truncate_chars(text, 0), "");
        assert_eq!(truncate_chars(text, 1), "a");
        assert_eq!(truncate_chars(text, 3), "abc");
        assert_eq!(truncate_chars(text, 4), "abc😀");
        assert_eq!(truncate_chars(text, 5), "abc😀é");
    }

    #[test]
    fn prune_tool_results_summarizes_old_verbose_outputs() {
        let verbose = "x".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 80);
        let mut messages = vec![
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &verbose),
            msg("user", "recent question"),
            msg("assistant", "recent answer"),
        ];

        let saved = prune_tool_results(&mut messages, 2);

        assert!(saved > 0);
        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert!(content.contains("[read_file] tool result pruned"));
        assert!(content.contains("Cargo.toml"));
        assert!(content.len() < verbose.len());
    }

    #[test]
    fn prune_tool_results_preserves_protected_tail() {
        let verbose = "x".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 80);
        let mut messages = vec![
            msg("user", "older context"),
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &verbose),
        ];

        let saved = prune_tool_results(&mut messages, 2);

        assert_eq!(saved, 0);
        let ContentBlock::ToolResult { content, .. } = &messages[2].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, &verbose);
    }

    #[test]
    fn prune_tool_results_preserves_prefix_bytes_when_reverse_prune_is_enough() {
        let older_verbose = "old ".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 40);
        let newer_verbose = "new ".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 40);
        let mut messages = vec![
            tool_use("call-old", "read_file", json!({"path": "old.txt"})),
            tool_result("call-old", &older_verbose),
            tool_use("call-new", "read_file", json!({"path": "new.txt"})),
            tool_result("call-new", &newer_verbose),
            msg("user", "protected tail"),
        ];
        let original = messages.clone();

        // Simulate the caller clearing its token budget after one suffix prune.
        let saved = prune_tool_results_until(&mut messages, 1, |_, saved| saved > 0);

        assert!(saved > 0);
        assert_eq!(&messages[..3], &original[..3]);
        assert_eq!(&messages[4..], &original[4..]);
        let ContentBlock::ToolResult { content, .. } = &messages[3].content[0] else {
            panic!("expected pruned tool result");
        };
        assert!(content.contains("[read_file] tool result pruned"));
        assert!(content.contains("new.txt"));
        assert!(content.len() < newer_verbose.len());
    }

    #[test]
    fn prune_tool_results_stops_after_newest_duplicate_prune() {
        let oldest = "oldest ".repeat(80);
        let middle = "middle ".repeat(80);
        let latest = "latest ".repeat(80);
        let mut messages = vec![
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &oldest),
            tool_use("call-2", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-2", &middle),
            tool_use("call-3", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-3", &latest),
            msg("user", "protected tail"),
        ];
        let original = messages.clone();

        let saved = prune_tool_results_until(&mut messages, 1, |_, saved| saved > 0);

        assert!(saved > 0);
        assert_eq!(&messages[..3], &original[..3]);
        assert_eq!(&messages[4..], &original[4..]);
        let ContentBlock::ToolResult { content, .. } = &messages[3].content[0] else {
            panic!("expected middle duplicate to be pruned");
        };
        assert!(content.contains("[read_file] tool result pruned"));
    }

    #[test]
    fn prune_tool_results_dedupes_identical_reads_but_keeps_latest_full_body() {
        let first = "first ".repeat(80);
        let second = "second ".repeat(80);
        let mut messages = vec![
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &first),
            tool_use("call-2", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-2", &second),
            msg("user", "tail"),
        ];

        let saved = prune_tool_results(&mut messages, 1);

        assert!(saved > 0);
        let ContentBlock::ToolResult { content: older, .. } = &messages[1].content[0] else {
            panic!("expected older tool result");
        };
        assert!(older.contains("tool result pruned"));
        let ContentBlock::ToolResult {
            content: latest, ..
        } = &messages[3].content[0]
        else {
            panic!("expected latest tool result");
        };
        assert_eq!(latest, &second);
    }

    #[test]
    fn context_window_errors_are_detected_for_summary_fallback() {
        for msg in [
            "HTTP 400 Bad Request: maximum context length is 1000000 tokens",
            "invalid_request_error: prompt is too long for the current model",
            "You requested 1000001 tokens but the maximum is 1000000",
            "request exceeds context window",
        ] {
            assert!(
                is_context_window_error(&anyhow::anyhow!(msg)),
                "expected context-window detection for `{msg}`",
            );
        }

        assert!(!is_context_window_error(&anyhow::anyhow!(
            "Invalid request: missing required field"
        )));
        assert!(!is_context_window_error(&anyhow::anyhow!(
            "503 Service Unavailable"
        )));
        // A rejected parameter names a token or a maximum without being a
        // length overflow; dropping history cannot fix it.
        for msg in [
            r#"Invalid request (400): {"message":"max_tokens must be <= 8192","type":"invalid_request_error"}"#,
            "HTTP 422: temperature exceeds maximum 2",
        ] {
            assert!(
                !is_context_window_error(&anyhow::anyhow!(msg)),
                "a non-length rejection must not trigger the drop-oldest ladder: `{msg}`",
            );
        }
    }

    #[test]
    fn tool_args_preview_redacts_sensitive_first_without_dropping_siblings() {
        let input: serde_json::Value = serde_json::from_str(
            r#"{"api_key":"sk-tool-secret-value","command":"cargo test -p auth"}"#,
        )
        .unwrap();

        let preview: serde_json::Value = serde_json::from_str(&tool_args_preview(&input)).unwrap();

        assert_eq!(preview["api_key"], codewhale_config::persistence::REDACTED);
        assert_eq!(preview["command"], "cargo test -p auth");
    }

    #[test]
    fn tool_args_preview_redacts_sensitive_later_without_touching_earlier_fields() {
        let input: serde_json::Value =
            serde_json::from_str(r#"{"command":"cargo test","api_key":"plain-secret-value"}"#)
                .unwrap();

        let preview: serde_json::Value = serde_json::from_str(&tool_args_preview(&input)).unwrap();

        assert_eq!(preview["command"], "cargo test");
        assert_eq!(preview["api_key"], codewhale_config::persistence::REDACTED);
    }

    #[test]
    fn tool_args_preview_redacts_nested_sensitive_values_recursively() {
        let input: serde_json::Value = serde_json::from_str(
            r#"{"meta":{"token":"nested-secret","keep":"yes"},"steps":[{"password":"pw","name":"a"}]}"#,
        )
        .unwrap();

        let preview: serde_json::Value = serde_json::from_str(&tool_args_preview(&input)).unwrap();

        assert_eq!(
            preview["meta"]["token"],
            codewhale_config::persistence::REDACTED
        );
        assert_eq!(preview["meta"]["keep"], "yes");
        assert_eq!(
            preview["steps"][0]["password"],
            codewhale_config::persistence::REDACTED
        );
        assert_eq!(preview["steps"][0]["name"], "a");
    }

    #[test]
    fn tool_args_preview_redacts_complete_multi_word_secret_value() {
        let input: serde_json::Value =
            serde_json::from_str(r#"{"command":"run this","password":"hunter two words"}"#)
                .unwrap();

        let serialized = tool_args_preview(&input);
        let preview: serde_json::Value = serde_json::from_str(&serialized).unwrap();

        assert_eq!(preview["command"], "run this");
        assert_eq!(preview["password"], codewhale_config::persistence::REDACTED);
        assert!(!serialized.contains("hunter"));
        assert!(!serialized.contains("two words"));
    }

    struct FixedSummaryClient {
        request: std::sync::Mutex<Option<MessageRequest>>,
        provider: &'static str,
        model: &'static str,
    }

    impl Default for FixedSummaryClient {
        fn default() -> Self {
            Self {
                request: std::sync::Mutex::new(None),
                provider: "test",
                model: "test-model",
            }
        }
    }

    impl FixedSummaryClient {
        fn for_route(provider: &'static str, model: &'static str) -> Self {
            Self {
                request: std::sync::Mutex::new(None),
                provider,
                model,
            }
        }
    }

    const FIXED_SUMMARY: &str = "1. Primary request and intent — migrate the session store. \
        2. Key technical concepts — sqlite. 7. Pending tasks — finish the fixed clock. \
        8. Current work — rerunning the session tests.";

    /// A real PNG whose bytes are worth shrinking. Noise defeats compression,
    /// which is the point: the shrink ladder must actually re-encode.
    fn noisy_png_bytes(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let mut pixels = image::RgbImage::new(width, height);
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = image::Rgb([
                (x.wrapping_mul(31) ^ y.wrapping_mul(17)) as u8,
                (x.wrapping_mul(7) ^ y.wrapping_mul(29)) as u8,
                (x.wrapping_add(y).wrapping_mul(13)) as u8,
            ]);
        }
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut bytes,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::NoFilter,
        )
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .expect("encode fixture png");
        bytes
    }

    fn png_data_url(width: u32, height: u32) -> String {
        use base64::Engine as _;
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(noisy_png_bytes(width, height))
        )
    }

    fn user_image_message(data_url: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "look at this screenshot".to_string(),
                    cache_control: None,
                },
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: data_url.to_string(),
                    },
                },
            ],
        }
    }

    /// Total inline-image URL bytes a request carries, the byte side an HTTP
    /// 413 boundary actually measures.
    fn inline_image_bytes(request: &MessageRequest) -> usize {
        request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .map(|block| match block {
                ContentBlock::ImageUrl { image_url } => image_url.url.len(),
                ContentBlock::ToolResult { content_blocks, .. } => {
                    content_blocks.as_ref().map_or(0, |blocks| {
                        blocks.iter().fold(0, |sum, block| {
                            let nested = block
                                .get("data")
                                .and_then(serde_json::Value::as_str)
                                .map_or(0, str::len);
                            sum + nested
                        })
                    })
                }
                _ => 0,
            })
            .sum()
    }

    fn summary_content() -> Vec<ContentBlock> {
        vec![ContentBlock::Text {
            text: "1. Primary request: keep working on the session store. \
                   7. Pending: rerun the tests."
                .to_string(),
            cache_control: None,
        }]
    }

    fn request_body_413() -> anyhow::Error {
        anyhow::anyhow!(
            "LLM error: HTTP 413: Failed to buffer the request body: length limit exceeded"
        )
    }

    /// The same boundary stated by a fronting gateway (openresty) instead of
    /// the API's own body reader. Reported on 2026-09-26; the ladder must
    /// treat it as the same byte-side rejection.
    fn request_body_413_html_gateway() -> anyhow::Error {
        anyhow::anyhow!(
            "LLM error: HTTP 413: DeepSeek API returned an HTML error page (HTTP 413): \
             413 Request Entity Too Large 413 Request Entity Too Large openresty"
        )
    }

    #[derive(Debug, Default)]
    struct RecordingNoticeSink {
        messages: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingNoticeSink {
        fn messages(&self) -> Vec<String> {
            self.messages.lock().expect("notice sink").clone()
        }
    }

    impl CompactionNoticeSink for RecordingNoticeSink {
        fn notice(&self, message: String) {
            self.messages.lock().expect("notice sink").push(message);
        }
    }

    /// A 900x900 noise PNG exceeds the 2 MiB inline-image budget, so the
    /// ladder must actually rewrite it.
    fn body_413_retry_envelope(
        sink: std::sync::Arc<RecordingNoticeSink>,
    ) -> PreparedCompactionEnvelope {
        let mut envelope = prepared(&CompactionConfig {
            enabled: false,
            ..CompactionConfig::default()
        });
        envelope.notice_sink = Some(sink);
        envelope
    }

    #[tokio::test]
    async fn request_body_413_reencodes_images_smaller_and_retries() {
        let _environment = crate::test_support::lock_test_env();
        let messages = vec![
            msg("user", "context before the screenshots"),
            user_image_message(&png_data_url(900, 900)),
        ];
        let client = ScriptedSummaryClient::with_outcomes(vec![
            Err(request_body_413()),
            Ok(summary_content()),
        ]);
        let sink = std::sync::Arc::new(RecordingNoticeSink::default());
        let envelope = body_413_retry_envelope(sink.clone());
        let mut usage = Usage::default();

        let result = compact_messages_safe(&client, &messages, None, &envelope, &mut usage).await;
        assert!(result.is_ok(), "the retry after shrinking must succeed");

        let requests = client.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 2, "one rejection, one retry");
        let first = inline_image_bytes(&requests[0]);
        let second = inline_image_bytes(&requests[1]);
        assert!(first > 0, "the fixture must carry the image");
        assert!(
            second > 0 && second < first,
            "the retry must carry smaller image bytes ({second} < {first})"
        );
        let notices = sink.messages();
        assert_eq!(notices.len(), 1, "one notice per downgrade: {notices:?}");
        assert!(
            notices[0].contains("re-encoded") && notices[0].contains("413"),
            "the notice must name the downgrade: {notices:?}"
        );
    }

    #[tokio::test]
    async fn request_body_413_after_shrinking_replaces_images_with_notes() {
        let _environment = crate::test_support::lock_test_env();
        let messages = vec![
            msg("user", "context before the screenshots"),
            user_image_message(&png_data_url(900, 900)),
        ];
        let client = ScriptedSummaryClient::with_outcomes(vec![
            Err(request_body_413()),
            Err(request_body_413()),
            Ok(summary_content()),
        ]);
        let sink = std::sync::Arc::new(RecordingNoticeSink::default());
        let envelope = body_413_retry_envelope(sink.clone());
        let mut usage = Usage::default();

        let result = compact_messages_safe(&client, &messages, None, &envelope, &mut usage).await;
        assert!(
            result.is_ok(),
            "the retry after replacing images must succeed"
        );

        let requests = client.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 3, "reject, shrink retry, replace retry");
        let first = inline_image_bytes(&requests[0]);
        let second = inline_image_bytes(&requests[1]);
        let third = inline_image_bytes(&requests[2]);
        assert!(second > 0 && second < first);
        assert_eq!(third, 0, "the last rung carries no image bytes");
        let note_present = requests[2].messages.iter().any(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::Text { text, .. } => text.contains("omitted from this summary pass"),
                _ => false,
            })
        });
        assert!(note_present, "the summarizer is told what was there");
        let notices = sink.messages();
        assert_eq!(notices.len(), 2, "one notice per rung: {notices:?}");
        assert!(notices[1].contains("replaced"), "{notices:?}");
    }

    #[tokio::test]
    async fn request_body_413_from_a_gateway_html_page_enters_the_same_ladder() {
        let _environment = crate::test_support::lock_test_env();
        let messages = vec![
            msg("user", "context before the screenshots"),
            user_image_message(&png_data_url(900, 900)),
        ];
        let client = ScriptedSummaryClient::with_outcomes(vec![
            Err(request_body_413_html_gateway()),
            Ok(summary_content()),
        ]);
        let sink = std::sync::Arc::new(RecordingNoticeSink::default());
        let envelope = body_413_retry_envelope(sink.clone());
        let mut usage = Usage::default();

        let result = compact_messages_safe(&client, &messages, None, &envelope, &mut usage).await;
        assert!(
            result.is_ok(),
            "the gateway-page rejection must enter the ladder"
        );
        let requests = client.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 2, "one rejection, one retry");
        assert!(
            inline_image_bytes(&requests[1]) < inline_image_bytes(&requests[0]),
            "the retry must carry smaller image bytes"
        );
        let notices = sink.messages();
        assert_eq!(
            notices.len(),
            1,
            "the user hears about the downgrade: {notices:?}"
        );
        assert!(notices[0].contains("413"), "{notices:?}");
    }

    #[tokio::test]
    async fn request_body_413_with_in_budget_images_skips_the_noop_retry_and_replaces() {
        // The images fit their share of the 2 MiB budget, yet the endpoint
        // refused the body: the cap sits below the budget. The ladder must not
        // report "no images" (which would fail the pass outright) and must not
        // resend identical bytes — it goes straight to the replace rung.
        let _environment = crate::test_support::lock_test_env();
        let messages = vec![
            msg("user", "context before the screenshot"),
            user_image_message(&png_data_url(64, 64)),
        ];
        let client = ScriptedSummaryClient::with_outcomes(vec![
            Err(request_body_413()),
            Ok(summary_content()),
        ]);
        let sink = std::sync::Arc::new(RecordingNoticeSink::default());
        let envelope = body_413_retry_envelope(sink.clone());
        let mut usage = Usage::default();

        let result = compact_messages_safe(&client, &messages, None, &envelope, &mut usage).await;
        assert!(
            result.is_ok(),
            "in-budget images must still let the ladder finish"
        );
        let requests = client.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 2, "replace directly, no identical retry");
        assert!(
            inline_image_bytes(&requests[0]) > 0,
            "the fixture carried the image"
        );
        assert_eq!(
            inline_image_bytes(&requests[1]),
            0,
            "the retry carries notes, not bytes"
        );
        let notices = sink.messages();
        assert_eq!(
            notices.len(),
            1,
            "one notice for the replace rung: {notices:?}"
        );
        assert!(notices[0].contains("replaced"), "{notices:?}");
    }

    #[tokio::test]
    async fn request_body_413_without_inline_images_fails_with_context() {
        let _environment = crate::test_support::lock_test_env();
        let messages = vec![msg("user", "no images anywhere in this history")];
        let client = ScriptedSummaryClient::with_outcomes(vec![Err(request_body_413())]);
        let sink = std::sync::Arc::new(RecordingNoticeSink::default());
        let envelope = body_413_retry_envelope(sink.clone());
        let mut usage = Usage::default();

        let error = compact_messages_safe(&client, &messages, None, &envelope, &mut usage)
            .await
            .expect_err("nothing left to shrink, the pass must fail");
        let text = format!("{error:#}");
        assert!(
            text.contains("request-body limit"),
            "the failure must name the boundary: {text}"
        );
        let requests = client.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 1, "no pointless identical retry");
        assert!(sink.messages().is_empty(), "nothing was downgraded");
    }

    #[tokio::test]
    async fn request_body_413_after_replacements_fails_with_the_full_ladder() {
        let _environment = crate::test_support::lock_test_env();
        let messages = vec![
            msg("user", "context before the screenshots"),
            user_image_message(&png_data_url(900, 900)),
        ];
        let client = ScriptedSummaryClient::with_outcomes(vec![
            Err(request_body_413()),
            Err(request_body_413()),
            Err(request_body_413()),
        ]);
        let sink = std::sync::Arc::new(RecordingNoticeSink::default());
        let envelope = body_413_retry_envelope(sink.clone());
        let mut usage = Usage::default();

        let error = compact_messages_safe(&client, &messages, None, &envelope, &mut usage)
            .await
            .expect_err("every rung refused, the pass must fail");
        let text = format!("{error:#}");
        assert!(
            text.contains("even after re-encoding and then replacing"),
            "the failure must report the full ladder: {text}"
        );
        let requests = client.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 3, "two retries, then stop");
        assert_eq!(sink.messages().len(), 2, "both rungs announced");
    }

    struct ScriptedSummaryClient {
        responses: std::sync::Mutex<std::collections::VecDeque<anyhow::Result<Vec<ContentBlock>>>>,
        requests: std::sync::Mutex<Vec<MessageRequest>>,
        retry_started: Option<std::sync::Arc<tokio::sync::Notify>>,
    }

    impl ScriptedSummaryClient {
        fn new(responses: Vec<Vec<ContentBlock>>) -> Self {
            Self::with_outcomes(responses.into_iter().map(Ok).collect())
        }

        fn with_outcomes(responses: Vec<anyhow::Result<Vec<ContentBlock>>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                requests: std::sync::Mutex::new(Vec::new()),
                retry_started: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::core::model_client::ModelClient for ScriptedSummaryClient {
        fn provider_name(&self) -> &str {
            "test"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        async fn create_message(
            &self,
            request: MessageRequest,
        ) -> anyhow::Result<codewhale_models::MessageResponse> {
            self.requests
                .lock()
                .expect("capture scripted summary request")
                .push(request);
            let outcome = self
                .responses
                .lock()
                .expect("read scripted summary response")
                .pop_front();
            if outcome.is_none()
                && let Some(retry_started) = &self.retry_started
            {
                retry_started.notify_one();
                return std::future::pending().await;
            }
            let content = outcome
                .ok_or_else(|| anyhow::anyhow!("scripted summary responses exhausted"))??;
            Ok(codewhale_models::MessageResponse {
                id: "summary-scripted".to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content,
                model: "test-model".to_string(),
                stop_reason: None,
                stop_sequence: None,
                container: None,
                usage: Usage {
                    input_tokens: 17,
                    output_tokens: 3,
                    prompt_cache_hit_tokens: Some(5),
                    reasoning_tokens: Some(2),
                    ..Usage::default()
                },
            })
        }

        async fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
            anyhow::bail!("streaming is unused by compaction")
        }

        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[async_trait::async_trait]
    impl crate::core::model_client::ModelClient for FixedSummaryClient {
        fn provider_name(&self) -> &str {
            self.provider
        }

        fn model(&self) -> &str {
            self.model
        }

        async fn create_message(
            &self,
            request: MessageRequest,
        ) -> anyhow::Result<codewhale_models::MessageResponse> {
            *self.request.lock().expect("capture summary request") = Some(request);
            Ok(codewhale_models::MessageResponse {
                id: "summary-fixture".to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: FIXED_SUMMARY.to_string(),
                    cache_control: None,
                }],
                model: self.model.to_string(),
                stop_reason: None,
                stop_sequence: None,
                container: None,
                usage: codewhale_models::Usage::default(),
            })
        }

        async fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
            anyhow::bail!("streaming is unused by compaction")
        }

        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn compaction_persists_original_and_model_handoff_before_returning_replacement() {
        let _environment = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let original = (0..12)
            .map(|i| {
                msg(
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("Work item {i}"),
                )
            })
            .collect::<Vec<_>>();
        let mut envelope = prepared(&CompactionConfig::default());
        envelope.session_id = Some("handoff-test".into());
        let client = FixedSummaryClient::default();
        let mut usage = Usage::default();
        let result = compact_messages_safe(&client, &original, None, &envelope, &mut usage)
            .await
            .unwrap();
        assert!(result.summary_prompt.is_some());
        let files = std::fs::read_dir(root.path().join("sessions/handoff-test/artifacts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let json = files
            .iter()
            .find(|path| path.extension().is_some_and(|ext| ext == "json"))
            .unwrap();
        let restored: Vec<Message> = serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
        assert_eq!(restored, original);
        let markdown = files
            .iter()
            .find(|path| path.extension().is_some_and(|ext| ext == "md"))
            .unwrap();
        assert!(
            std::fs::read_to_string(markdown)
                .unwrap()
                .contains("migrate the session store")
        );
        // An unwritable artifact destination must abort before a provider call.
        envelope.session_id = Some("blocked-handoff".into());
        std::fs::write(
            root.path().join("sessions/blocked-handoff"),
            b"not a directory",
        )
        .unwrap();
        let blocked = FixedSummaryClient::default();
        assert!(
            compact_messages_safe(&blocked, &original, None, &envelope, &mut usage)
                .await
                .is_err()
        );
        assert!(blocked.request.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn compaction_commits_summary_and_retains_recent_user_messages() {
        let messages = vec![
            msg(
                "user",
                "Objective: migrate the session store to sqlite without breaking existing logins",
            ),
            msg("assistant", "Working on it."),
            tool_use(
                "t1",
                "Bash",
                json!({"command": "cargo test -p session-store"}),
            ),
            tool_result("t1", "test session_store::roundtrip ... ok\nexit code 0"),
            msg("user", "Sounds good, do it"),
            msg("assistant", "Nearly done, rerunning the suite."),
        ];
        let config = CompactionConfig {
            model: "test-model".to_string(),
            cache_summary: false,
            ..Default::default()
        };
        let client = FixedSummaryClient::default();

        let (retained, summary_prompt, _) =
            compact_messages(&client, &messages, &config).await.unwrap();

        let request = client
            .request
            .lock()
            .expect("read summary request")
            .clone()
            .expect("summary request was captured");
        assert_eq!(&request.messages[..messages.len()], messages.as_slice());
        assert_eq!(request.messages.len(), messages.len() + 1);
        let ContentBlock::Text { text, .. } = &request.messages.last().unwrap().content[0] else {
            panic!("final compaction instruction must be text");
        };
        assert!(!text.contains(COMPACTION_SUMMARY_MARKER));
        assert_eq!(request.temperature, None);
        assert_eq!(request.top_p, None);
        assert_eq!(
            request.max_tokens,
            crate::route_budget::effective_max_output_tokens_for_route(
                crate::config::ProviderKind::Custom,
                "test-model",
                None,
            )
        );

        let Some(SystemPrompt::Blocks(blocks)) = summary_prompt else {
            panic!("compaction must produce a summary system block");
        };
        let text = &blocks[0].text;
        assert!(text.contains(FIXED_SUMMARY));
        assert!(text.starts_with(COMPACTION_SUMMARY_MARKER));

        // Replacement history keeps older user turns, then the open round
        // verbatim (user + assistant + tools), then one checkpoint.
        assert!(retained.iter().any(|message| {
            user_text_of(message).is_some_and(|text| text.contains("Objective: migrate"))
        }));
        assert!(
            retained
                .iter()
                .any(|message| { user_text_of(message).as_deref() == Some("Sounds good, do it") })
        );
        assert!(retained.iter().any(|message| {
            message.role.is_assistant_like()
                && message.content.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Text { text, .. }
                            if text.contains("Nearly done, rerunning the suite.")
                    )
                })
        }));
        assert!(retained.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { content, .. }
                        if content.contains("session_store::roundtrip")
                )
            })
        }));
        assert!(is_compaction_checkpoint_message(retained.last().unwrap()));
        assert!(is_wire_compaction_checkpoint_message(
            retained.last().unwrap()
        ));
        assert!(matches!(
            &retained.last().unwrap().content[0],
            ContentBlock::Text { text: checkpoint, .. } if checkpoint == text
        ));
        last_round::validate_last_round_coverage(&messages, &retained[..retained.len() - 1])
            .unwrap();
    }

    #[tokio::test]
    async fn uninterrupted_task_compacts_repeatedly_with_original_prefix_and_recent_tool_pairs() {
        let system = SystemPrompt::Text("stable project instructions and permissions".into());
        let mut prepared = PreparedCompactionEnvelope::new(CompactionConfig {
            token_threshold: 40_000,
            model: "test-model".into(),
            ..Default::default()
        });
        prepared.tools = Some(vec![
            serde_json::from_value(json!({
                "name": "File", "description": "Read a file", "input_schema": {"type": "object"}
            }))
            .unwrap(),
        ]);
        let client = FixedSummaryClient::default();
        let mut messages = vec![msg(
            "user",
            "Finish the migration. Preserve logins; do not publish.",
        )];
        for epoch in 0..3 {
            for step in 0..20 {
                let id = format!("{epoch}-{step}");
                let mut call = tool_use(&id, "File", json!({"path":"session.rs"}));
                call.content.insert(
                    0,
                    ContentBlock::Text {
                        text: format!("Evidence {id}: {}", "x".repeat(12_000)),
                        cache_control: None,
                    },
                );
                if step == 19 {
                    call.content.insert(
                        0,
                        ContentBlock::Thinking {
                            thinking: "retained reasoning ".repeat(2000),
                            signature: None,
                            state: None,
                        },
                    );
                }
                messages.push(call);
                messages.push(tool_result(
                    &id,
                    &format!("Observed {id}: {}", "e".repeat(1000)),
                ));
            }
            let original = messages.clone();
            let mut usage = Usage::default();
            let result =
                compact_messages_safe(&client, &messages, Some(&system), &prepared, &mut usage)
                    .await
                    .unwrap();
            let request = client.request.lock().unwrap().clone().unwrap();
            assert_eq!(request.system.as_ref(), Some(&system));
            assert_eq!(request.tools, prepared.tools);
            assert_eq!(request.tool_choice, Some(json!("none")));
            assert_eq!(
                &request.messages[..original.len()],
                original.as_slice(),
                "summary must see the original evidence and reusable history prefix"
            );
            assert!(estimate_tokens(&result.messages) < estimate_tokens(&original) / 2);
            assert_eq!(
                result
                    .messages
                    .iter()
                    .filter(|m| is_compaction_checkpoint_message(m))
                    .count(),
                1
            );
            for step in [18, 19] {
                let id = format!("{epoch}-{step}");
                let expected = original.iter().find(|m| m.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { id: found, .. } if found == &id))).unwrap();
                assert!(
                    result.messages.contains(expected),
                    "retained assistant text, calls and reasoning must survive unchanged"
                );
                assert!(result.messages.iter().any(|m| m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == &id))));
            }
            assert_eq!(result.messages[0], original[0]);
            messages = result.messages;
        }
    }

    #[test]
    fn coverage_floor_rejects_a_replacement_that_drops_last_round_assistant() {
        let original = vec![
            msg("user", "What failed?"),
            msg("assistant", "session_store::roundtrip panics on reload."),
        ];
        let gutting = vec![msg("user", "What failed?")];
        let error = last_round::validate_last_round_coverage(&original, &gutting)
            .expect_err("dropping last-round assistant text must fail closed");
        assert!(error.to_string().contains("assistant"), "{error}");
    }

    #[tokio::test]
    async fn compaction_preserves_runtime_provenance_and_saved_user_title() {
        let runtime = crate::runtime_handoff::operate_contract_runtime_message();
        let mut user = msg("user", "Build a focus timer");
        user.content.push(ContentBlock::Text {
            text: "<turn_meta>\nInput provenance: external_user\nInput authority: external_current_turn\n</turn_meta>".to_string(),
            cache_control: None,
        });
        let messages = vec![
            runtime.clone(),
            msg("assistant", "Ready."),
            user.clone(),
            msg("assistant", "Building the timer."),
            msg("user", "Keep the controls simple"),
            msg("assistant", "Adding start and stop."),
        ];
        let (retained, summary, _) = compact_messages(
            &FixedSummaryClient::default(),
            &messages,
            &CompactionConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(retained.first(), Some(&runtime));
        assert!(retained.contains(&user));
        assert!(crate::runtime_handoff::is_operate_contract_message(
            &retained[0]
        ));
        let saved = crate::session_manager::create_saved_session_with_mode(
            &retained,
            "test-model",
            std::path::Path::new("."),
            0,
            summary.as_ref(),
            None,
        );
        let restored: crate::session_manager::SavedSession =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(restored.metadata.title, "Build a focus timer");
        assert_eq!(restored.messages, retained);
    }

    #[test]
    fn retained_literal_runtime_xml_remains_user_authored() {
        let runtime = crate::runtime_handoff::operate_contract_runtime_message();
        // A user may paste the exact bytes, including a metadata example, in
        // one ordinary text block. Compaction must not split it into authority.
        let literal = user_text_of(&runtime).unwrap();
        let user = msg("user", &literal);
        let retained = retained_user_messages(std::slice::from_ref(&user), usize::MAX);
        assert_eq!(retained, vec![user]);
        assert_eq!(
            crate::runtime_handoff::classify_user_turn_prompt(&retained[0]),
            crate::runtime_handoff::UserTurnPromptKind::Editable,
        );
        assert_eq!(
            crate::session_manager::conversation_title_prompt(&retained),
            Some(literal.as_str()),
        );
        assert!(!crate::runtime_handoff::is_operate_contract_message(
            &retained[0]
        ));
    }

    #[test]
    fn retained_image_turn_and_structured_content_keep_their_boundaries() {
        let image = Message {
            role: Role::User,
            content: vec![ContentBlock::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/png;base64,AAAA".to_string(),
                },
            }],
        };
        let mut mixed = msg("user", "Compare these views");
        mixed.content.extend(image.content.clone());
        mixed.content.push(ContentBlock::Text {
            text: "Keep the original colors".to_string(),
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
            }),
        });
        let messages = vec![image, mixed];
        let retained = retained_user_messages(&messages, 3_000);
        assert_eq!(retained, messages);
        let saved = crate::session_manager::create_saved_session_with_mode(
            &retained,
            "test-model",
            std::path::Path::new("."),
            0,
            None,
            None,
        );
        assert_eq!(
            saved.metadata.title,
            crate::session_manager::DEFAULT_SESSION_TITLE
        );
        assert!(retained_user_messages(&messages[..1], IMAGE_TOKEN_ESTIMATE - 1).is_empty());
    }

    #[test]
    fn retained_budget_never_partially_promotes_structural_metadata() {
        let runtime = crate::runtime_handoff::operate_contract_runtime_message();
        let text = msg("user", "αβγδεζηθικ");
        let retained = retained_user_messages(&[runtime.clone(), text.clone()], 5);
        assert_eq!(retained, vec![text]);
        assert!(retained_user_messages(&[runtime], 1).is_empty());
        assert_eq!(
            user_text_of(&retained_user_messages(&[msg("user", "αβγδεζηθικ")], 2)[0]).as_deref(),
            Some("αβγδεζ"),
        );
    }

    #[test]
    fn retained_older_turn_drops_result_blocks_whose_call_was_summarized() {
        // #6119: a host-supplied user message can mix text with a tool
        // result; the tool_use it answers lives in the summarized region, so
        // the retained copy must keep the text and drop the orphaned result.
        let mut mixed = msg("user", "Please keep this context.");
        mixed.content.push(ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: "toolu_orphan_1".to_string(),
            content: "{\"ok\":true}".to_string(),
            is_error: None,
            content_blocks: None,
        });
        let retained = retained_user_messages(std::slice::from_ref(&mixed), usize::MAX);
        assert_eq!(retained.len(), 1);
        assert!(
            !retained[0]
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. })),
            "the retained copy must not keep an orphaned tool_result"
        );
        assert_eq!(
            user_text_of(&retained[0]).as_deref(),
            Some("Please keep this context.")
        );
        // Insufficient budget still refuses to partially retain a multi-block
        // turn; the structural-metadata guard is unchanged.
        assert!(retained_user_messages(std::slice::from_ref(&mixed), 1).is_empty());
    }

    #[test]
    fn summary_quality_gate_rejects_empty_and_known_placeholder_text() {
        for summary in [
            "",
            " \n\t ",
            "...",
            "。。。",
            "🫧",
            "N/A",
            "(no summary available)",
            "I cannot provide a summary.",
        ] {
            let error = validate_compaction_summary(summary)
                .expect_err("degenerate summary must fail closed");
            assert!(error.to_string().contains("unusable"), "{error}");
        }
        validate_compaction_summary(FIXED_SUMMARY)
            .expect("a substantive continuation handoff must be accepted");
        validate_compaction_summary(
            "目的: #4394の空要約を防止。完了: 検証と実装。制約: 履歴を変更しない。次: テスト実行。",
        )
        .expect("a concise multilingual handoff must not be rejected by prose length");
    }

    #[tokio::test]
    async fn empty_successful_summary_retries_once_without_replacing_history() {
        let original = vec![
            msg(
                "user",
                "Keep the migration transactional and preserve existing sessions.",
            ),
            msg(
                "assistant",
                "I am updating the session store and its fixtures.",
            ),
        ];
        let client = ScriptedSummaryClient::new(vec![
            vec![ContentBlock::Text {
                text: " \n\t ".to_string(),
                cache_control: None,
            }],
            vec![ContentBlock::Text {
                text: FIXED_SUMMARY.to_string(),
                cache_control: None,
            }],
        ]);
        let config = CompactionConfig {
            model: "test-model".to_string(),
            cache_summary: false,
            ..Default::default()
        };

        let mut invocation_usage = Usage::default();
        let result = compact_messages_safe(
            &client,
            &original,
            None,
            &prepared(&config),
            &mut invocation_usage,
        )
        .await
        .expect("the conservative retry should recover a usable summary");
        assert_eq!(invocation_usage.input_tokens, 34);
        assert_eq!(invocation_usage.output_tokens, 6);
        assert_eq!(invocation_usage.prompt_cache_hit_tokens, Some(10));
        assert_eq!(invocation_usage.reasoning_tokens, Some(4));

        let requests = client
            .requests
            .lock()
            .expect("read scripted summary requests");
        assert_eq!(requests.len(), 2, "quality failure retries exactly once");
        let ContentBlock::Text { text, .. } = &requests[1]
            .messages
            .last()
            .expect("retry instruction")
            .content[0]
        else {
            panic!("retry instruction must be text");
        };
        assert!(text.contains("previous reply was empty or a placeholder"));
        drop(requests);

        assert_eq!(
            result.retries_used, 1,
            "quality retry must reach diagnostics"
        );
        assert_eq!(original[0].role, "user", "source history remains untouched");
        assert!(result.messages.iter().any(is_compaction_checkpoint_message));
        let Some(SystemPrompt::Blocks(blocks)) = result.summary_prompt else {
            panic!("recovered summary must be committed");
        };
        assert!(blocks[0].text.contains(FIXED_SUMMARY));
        assert!(!blocks[0].text.contains("(no summary available)"));
    }

    #[tokio::test]
    async fn quality_retry_count_survives_a_later_transient_failure() {
        let client = ScriptedSummaryClient::with_outcomes(vec![
            Ok(vec![ContentBlock::Text {
                text: "...".to_string(),
                cache_control: None,
            }]),
            Err(anyhow::anyhow!("request timed out")),
            Ok(vec![ContentBlock::Text {
                text: FIXED_SUMMARY.to_string(),
                cache_control: None,
            }]),
        ]);
        let config = CompactionConfig {
            model: "test-model".to_string(),
            cache_summary: false,
            ..Default::default()
        };

        let mut invocation_usage = Usage::default();
        let result = compact_messages_safe(
            &client,
            &[msg("user", "Preserve the current migration state.")],
            None,
            &prepared(&config),
            &mut invocation_usage,
        )
        .await
        .expect("the outer retry should recover after the transient failure");
        assert_eq!(invocation_usage.input_tokens, 34);
        assert_eq!(invocation_usage.output_tokens, 6);
        assert_eq!(invocation_usage.prompt_cache_hit_tokens, Some(10));
        assert_eq!(invocation_usage.reasoning_tokens, Some(4));

        assert_eq!(
            result.retries_used, 2,
            "one quality retry plus one outer transient retry must be reported"
        );
        assert_eq!(
            client
                .requests
                .lock()
                .expect("read scripted summary requests")
                .len(),
            3,
            "the diagnostic count must match the two calls after the initial request"
        );
    }

    #[tokio::test]
    async fn compaction_usage_survives_cancellation_during_quality_retry() {
        let _cost_scope = crate::cost_status::test_scope();
        let retry_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut client = ScriptedSummaryClient::new(vec![vec![ContentBlock::Text {
            text: "...".to_string(),
            cache_control: None,
        }]]);
        client.retry_started = Some(std::sync::Arc::clone(&retry_started));
        let messages = vec![msg("user", "Preserve the migration state.")];
        let prepared = prepared(&CompactionConfig::default());
        let mut invocation_usage = Usage::default();
        {
            let compaction =
                compact_messages_safe(&client, &messages, None, &prepared, &mut invocation_usage);
            tokio::pin!(compaction);
            tokio::select! {
                result = &mut compaction => panic!("retry must remain pending: {result:?}"),
                _ = retry_started.notified() => {},
                _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("quality retry did not start"),
            }
        }
        assert_eq!(invocation_usage.input_tokens, 17);
        assert_eq!(invocation_usage.output_tokens, 3);
        assert_eq!(invocation_usage.prompt_cache_hit_tokens, Some(5));
        assert_eq!(invocation_usage.reasoning_tokens, Some(2));
        assert_eq!(client.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn non_text_summary_failure_preserves_history_after_one_retry() {
        let original = vec![
            msg(
                "user",
                "Do not lose the current branch or the failing test name.",
            ),
            msg("assistant", "The failing test is session_store::roundtrip."),
        ];
        let client = ScriptedSummaryClient::new(vec![
            vec![ContentBlock::thinking("internal-only response")],
            vec![ContentBlock::thinking("still no user-visible handoff")],
        ]);
        let config = CompactionConfig {
            model: "test-model".to_string(),
            cache_summary: false,
            ..Default::default()
        };

        let mut invocation_usage = Usage::default();
        let error = compact_messages_safe(
            &client,
            &original,
            None,
            &prepared(&config),
            &mut invocation_usage,
        )
        .await
        .expect_err("two non-text responses must not replace history");
        assert_eq!(invocation_usage.input_tokens, 34);
        assert_eq!(invocation_usage.output_tokens, 6);
        assert_eq!(invocation_usage.prompt_cache_hit_tokens, Some(10));
        assert_eq!(invocation_usage.reasoning_tokens, Some(4));

        assert!(
            error
                .to_string()
                .contains("remained unusable after one conservative retry"),
            "{error}"
        );
        assert_eq!(
            client
                .requests
                .lock()
                .expect("read scripted summary requests")
                .len(),
            2,
            "quality failure gets one retry, not the transient retry ladder"
        );
        assert_eq!(
            original,
            vec![
                msg(
                    "user",
                    "Do not lose the current branch or the failing test name."
                ),
                msg("assistant", "The failing test is session_store::roundtrip."),
            ],
            "borrowed source history must remain byte-for-byte unchanged"
        );
    }
    #[tokio::test]
    async fn compaction_uses_the_resolved_route_output_allowance() {
        for (route_label, provider, model) in [
            (
                "thinking-default route",
                crate::config::ProviderKind::Deepseek,
                "deepseek-v4-flash",
            ),
            (
                "fixed-sampling route",
                crate::config::ProviderKind::Moonshot,
                "k3",
            ),
        ] {
            let client = FixedSummaryClient::for_route(provider.as_str(), model);
            let config = CompactionConfig {
                model: model.to_string(),
                cache_summary: false,
                ..Default::default()
            };
            compact_messages(&client, &[msg("user", "summarize this task")], &config)
                .await
                .expect("route compaction should complete");

            let request = client
                .request
                .lock()
                .expect("read summary request")
                .clone()
                .expect("summary request was captured");
            assert_eq!(
                request.max_tokens,
                crate::route_budget::effective_max_output_tokens_for_route(provider, model, None),
                "{route_label} must use the ordinary route output policy"
            );
            assert_eq!(request.temperature, None);
            assert_eq!(request.top_p, None);
        }
    }

    struct TruncatedSummaryClient;

    #[async_trait::async_trait]
    impl crate::core::model_client::ModelClient for TruncatedSummaryClient {
        fn provider_name(&self) -> &str {
            "test"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        async fn create_message(
            &self,
            _request: MessageRequest,
        ) -> anyhow::Result<codewhale_models::MessageResponse> {
            Ok(codewhale_models::MessageResponse {
                id: "summary-truncated".to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "1. Primary request and intent — mig".to_string(),
                    cache_control: None,
                }],
                model: "test-model".to_string(),
                stop_reason: Some("max_tokens".to_string()),
                stop_sequence: None,
                container: None,
                usage: codewhale_models::Usage::default(),
            })
        }

        async fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
            anyhow::bail!("streaming is unused by compaction")
        }

        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    /// A provider-truncated summary must fail compaction instead of replacing
    /// session history with a fragment.
    #[tokio::test]
    async fn truncated_summary_response_fails_compaction() {
        let messages: Vec<Message> = (0..40)
            .map(|index| {
                msg(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    &format!("padding message {index} with enough text to compact"),
                )
            })
            .collect();
        let config = CompactionConfig {
            model: "test-model".to_string(),
            cache_summary: false,
            ..Default::default()
        };

        let error = compact_messages(&TruncatedSummaryClient, &messages, &config)
            .await
            .expect_err("a truncated summary must not be committed");
        let text = error.to_string();
        assert!(text.contains("incomplete"), "{text}");
        assert!(text.contains("max_tokens"), "{text}");
    }

    #[test]
    fn estimate_tokens_empty_messages() {
        let messages: Vec<Message> = vec![];
        assert_eq!(estimate_tokens(&messages), 0);
    }

    #[test]
    fn estimate_tokens_with_text() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello, world!".to_string(), // 13 chars = ~3 tokens
                cache_control: None,
            }],
        }];
        let tokens = estimate_tokens(&messages);
        assert!(tokens > 0 && tokens < 10);
    }

    #[test]
    fn conservative_estimate_never_reads_cjk_below_the_byte_estimate() {
        let cjk = "中".repeat(1200); // 3,600 UTF-8 bytes
        assert!(estimate_text_tokens_conservative(&cjk) >= cjk.len() / 4);
        // ASCII keeps its three-characters-per-token reading.
        assert_eq!(estimate_text_tokens_conservative(&"a".repeat(300)), 100);
    }

    #[test]
    fn pressure_counts_text_only_reasoning_and_server_tool_payloads() {
        let payload = "retained evidence ".repeat(1000);
        let blocks = vec![
            ContentBlock::thinking(payload.clone()),
            ContentBlock::ServerToolUse {
                id: "server-call".into(),
                name: "code_execution".into(),
                input: json!({"code": payload}),
            },
            ContentBlock::CodeExecutionToolResult {
                tool_use_id: "server-call".into(),
                content: json!({"stdout": payload}),
            },
            ContentBlock::ToolSearchToolResult {
                tool_use_id: "search-call".into(),
                content: json!({"description": payload}),
            },
        ];
        for block in blocks {
            let messages = vec![Message {
                role: Role::Assistant,
                content: vec![block],
            }];
            assert!(estimate_tokens(&messages) >= payload.len() / 4);
            assert!(estimate_input_tokens_for_pressure(&messages, None) >= payload.len() / 4);
        }
    }

    #[test]
    fn estimate_tokens_counts_tool_round_thinking_across_turns() {
        // Per DeepSeek thinking-mode rules, any assistant message that
        // performed a tool call keeps its reasoning_content in the request
        // forever, including across new user turns. Token estimates must
        // count those bytes.
        let thinking = "reasoning ".repeat(800);
        let current_messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Use a tool".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        thinking: thinking.clone(),
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "tool-1".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"path": "Cargo.toml"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "tool-1".to_string(),
                    content: "manifest".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let historical_messages = {
            let mut messages = current_messages.clone();
            messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Done.".to_string(),
                    cache_control: None,
                }],
            });
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Next question.".to_string(),
                    cache_control: None,
                }],
            });
            messages
        };
        let completed_messages = {
            let mut messages = current_messages.clone();
            messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Done.".to_string(),
                    cache_control: None,
                }],
            });
            messages
        };

        let lower_bound = thinking.len() / 5;
        assert!(estimate_tokens(&current_messages) > lower_bound);
        assert!(estimate_tokens(&completed_messages) > lower_bound);
        assert!(estimate_tokens(&historical_messages) > lower_bound);
    }

    #[test]
    fn should_compact_respects_enabled_flag() {
        let config = CompactionConfig {
            enabled: false,
            ..Default::default()
        };
        // Even with many messages, disabled compaction should return false
        let messages: Vec<Message> = (0..100)
            .map(|_| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "test".to_string(),
                    cache_control: None,
                }],
            })
            .collect();
        assert!(!should_compact(&messages, None, &prepared(&config)));
    }

    /// The #5577 acceptance case: a session whose provider bills 842K prompt
    /// tokens on a 1M window (threshold 800K) MUST compact even when the
    /// local estimate is far lower — the bounded working list undercounts
    /// what the provider actually saw, and billed truth wins.
    #[test]
    fn billed_842k_on_a_1m_window_compacts_despite_a_small_estimate() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 800_000,
            ..Default::default()
        };
        let messages: Vec<Message> = (0..40)
            .map(|i| Message {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![ContentBlock::Text {
                    text: format!("short message {i}"),
                    cache_control: None,
                }],
            })
            .collect();
        // Estimate alone stays far under the trigger…
        assert!(!should_compact(&messages, None, &prepared(&config)));
        // …but the provider's billed prompt total decides.
        assert_eq!(
            compaction_decision_with_billed(&messages, None, &prepared(&config), Some(842_000)),
            CompactionDecision::Compact
        );
    }

    /// A refusal under real pressure must name its guard so the host can
    /// tell the user, instead of the silent hold that reads as a broken
    /// auto-compactor (#5577).
    #[test]
    fn refusals_under_pressure_name_their_guard() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 100,
            ..Default::default()
        };
        // Too few messages to summarize: over-pressure, short transcript.
        let few: Vec<Message> = (0..3)
            .map(|i| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("message {i} {}", "x".repeat(300)),
                    cache_control: None,
                }],
            })
            .collect();
        assert_eq!(
            compaction_decision_with_billed(&few, None, &prepared(&config), None),
            CompactionDecision::Refused(CompactionRefusal::TooFewMessages { count: few.len() })
        );

        // Retained floor above the trigger: a giant system prompt no pass
        // can reclaim. The refusal carries the numbers the user needs.
        let many: Vec<Message> = (0..12)
            .map(|i| Message {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![ContentBlock::Text {
                    text: format!("message {i} {}", "y".repeat(200)),
                    cache_control: None,
                }],
            })
            .collect();
        let system = SystemPrompt::Text("s".repeat(4_000));
        match compaction_decision_with_billed(&many, Some(&system), &prepared(&config), None) {
            CompactionDecision::Refused(CompactionRefusal::RetainedFloor { floor, threshold }) => {
                assert_eq!(threshold, 100);
                assert!(floor >= threshold, "floor {floor} must be over {threshold}");
            }
            other => panic!("expected a retained-floor refusal, got {other:?}"),
        }
    }

    /// v0.8.11: message-count is no longer a compaction trigger. Long
    /// chats of small messages stay uncompacted because rewriting the
    /// prefix cache for a tiny budget reclaim is net-negative. Only token
    /// pressure (and the explicit `/compact` slash command) trigger
    /// compaction.
    #[test]
    fn message_count_no_longer_triggers_compaction() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 1_000_000,
            ..Default::default()
        };

        // 200 tiny messages, well above the prior message threshold.
        let many_messages: Vec<Message> = (0..200)
            .map(|_| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "x".to_string(),
                    cache_control: None,
                }],
            })
            .collect();
        // Token total stays minuscule so the token threshold is not hit;
        // without the prior message-count trigger, no compaction.
        assert!(!should_compact(&many_messages, None, &prepared(&config)));
    }

    // ========================================================================
    // Additional Compaction Trigger Tests
    // ========================================================================

    #[test]
    fn full_request_pressure_crosses_token_threshold() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 20_000,
            ..Default::default()
        };

        // Create messages that exceed token threshold
        let messages: Vec<Message> = (0..20).map(|_| msg("user", &"x".repeat(5_000))).collect();

        assert!(compaction_pressure_reached(&messages, None, &config));
    }

    #[test]
    fn auto_compaction_uses_full_request_pressure_across_context_sizes() {
        for (window, output_reserve) in [
            (128_000_u64, 4_096_u64),
            (272_000, 4_096),
            // Large windows use the same ordinary request reservation; there
            // is no second, non-wire reasoning allowance.
            (1_000_000, 65_536),
        ] {
            let budget = crate::context_budget::ContextBudget::new(window, 0, output_reserve);
            let threshold = usize::try_from(budget.compaction_trigger_for_percent(80.0))
                .expect("test threshold fits usize");
            let raw_target = threshold.saturating_mul(7) / 10;
            let chars_per_message = raw_target.saturating_mul(4) / 14;
            let messages: Vec<Message> = (0..14)
                .map(|index| {
                    msg(
                        if index % 2 == 0 { "user" } else { "assistant" },
                        &"x".repeat(chars_per_message),
                    )
                })
                .collect();
            let raw = estimate_tokens(&messages);
            let full = estimate_input_tokens_for_pressure(&messages, None);
            let config = CompactionConfig {
                enabled: true,
                token_threshold: threshold,
                ..Default::default()
            };

            assert!(
                raw < threshold,
                "raw message estimator alone must not cross {window}"
            );
            // The pressure estimate adds per-message framing on top of the
            // raw message tokens; billed usage from the provider can also
            // cross the trigger on its own.
            assert!(
                full < threshold,
                "70%-filled fixture must stay under the {window} trigger: {full} >= {threshold}"
            );
            assert!(
                crate::compaction::compaction_pressure_reached_with_billed(
                    &messages,
                    None,
                    &config,
                    Some(threshold as u64),
                ),
                "billed prompt tokens at the trigger must reach pressure for {window}"
            );
            assert!(
                crate::compaction::should_compact_with_billed(
                    &messages,
                    None,
                    &prepared(&config),
                    Some(threshold as u64),
                ),
                "billed pressure must trigger eligibility for a {window}-token route"
            );
        }
    }

    #[test]
    fn auto_compaction_skips_pressure_that_cannot_be_reclaimed_below_trigger() {
        let messages: Vec<Message> = (0..20)
            .map(|index| {
                msg(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    &"x".repeat(500),
                )
            })
            .collect();
        let system = SystemPrompt::Text("s".repeat(24_000));
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 10_000,
            ..Default::default()
        };

        assert!(
            estimate_input_tokens_conservative(&messages, Some(&system)) >= config.token_threshold,
            "fixture must be under full-request pressure"
        );
        assert!(
            !should_compact(&messages, Some(&system), &prepared(&config)),
            "a pinned/system floor above the trigger would loop every tool step"
        );
    }

    #[test]
    fn full_request_threshold_is_inclusive() {
        let messages: Vec<Message> = (0..10)
            .map(|index| msg(if index % 2 == 0 { "user" } else { "assistant" }, "payload"))
            .collect();
        let threshold = estimate_input_tokens_for_pressure(&messages, None);
        let config = CompactionConfig {
            enabled: true,
            token_threshold: threshold,
            ..Default::default()
        };

        assert!(compaction_pressure_reached(&messages, None, &config));
    }

    #[test]
    fn test_should_compact_below_token_threshold() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 1000,
            ..Default::default()
        };

        // Create short messages
        let messages: Vec<Message> = (0..5).map(|_| msg("user", "short")).collect();

        assert!(!should_compact(&messages, None, &prepared(&config)));
    }

    #[test]
    fn auto_compaction_uses_token_threshold_without_fixed_floor() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 20_000,
            ..Default::default()
        };

        // Long sessions are dominated by assistant/tool output; the retained
        // user tail stays small, so the pass is reclaimable.
        let messages: Vec<Message> = (0..20)
            .map(|index| {
                if index % 2 == 0 {
                    msg("user", &"x".repeat(100))
                } else {
                    msg("assistant", &"x".repeat(10_000))
                }
            })
            .collect();
        assert!(should_compact(&messages, None, &prepared(&config)));
    }

    #[test]
    fn test_compaction_result_retries_used() {
        // This test verifies the CompactionResult structure
        let result = CompactionResult {
            messages: vec![],
            summary_prompt: None,
            retries_used: 2,
            coverage: CompactionCoverage::default(),
        };

        assert_eq!(result.retries_used, 2);
        assert!(result.messages.is_empty());
    }
}
