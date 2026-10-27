//! Compact session context inspector.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::compaction::{
    CompactionPath, estimate_input_tokens_for_pressure, inspect_compaction_keep,
    last_round_kept_count, last_round_start, pinned_anchors_text,
};
use crate::session_manager::SessionContextReference;
use crate::tui::app::{App, ToolDetailRecord};
use crate::tui::menu_style;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
    render_underwater_surface,
};
use codewhale_core::ContextReferenceSource;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_models::{SystemPrompt, Tool};
use codewhale_palette as palette;
use codewhale_runtime::context_budget::{
    CONTEXT_CRITICAL_THRESHOLD_PERCENT, CONTEXT_WARNING_THRESHOLD_PERCENT,
};

/// Marker used by per-turn working-set metadata. Replicated here so the
/// context inspector can distinguish stable prompt blocks from volatile
/// working-set context without importing engine internals.
const WORKING_SET_MARKER: &str = "## Repo Working Set";

const MAX_REFERENCE_ROWS: usize = 12;
const MAX_TOOL_ROWS: usize = 8;
const MAX_SCHEMA_COST_ROWS: usize = 24;
const SCHEMA_TOKEN_DIVISOR: usize = 4;

const SYSTEM_LAYER_MARKERS: &[(&str, &str, PromptLayerKind)] = &[
    (
        "Bundled constitution",
        "## Codewhale",
        PromptLayerKind::Static,
    ),
    ("Language policy", "## Language", PromptLayerKind::Static),
    (
        "Output formatting",
        "## Output Formatting",
        PromptLayerKind::Static,
    ),
    (
        "Repository constitution",
        "<codewhale_repo_constitution",
        PromptLayerKind::Static,
    ),
    (
        "Project context",
        "<project_instructions",
        PromptLayerKind::Static,
    ),
    (
        "Project context pack",
        "## Project Context Pack",
        PromptLayerKind::Static,
    ),
    ("Environment", "## Environment", PromptLayerKind::Static),
    ("Skills", "## Skills", PromptLayerKind::Static),
    (
        "Core execution",
        "## Core Execution",
        PromptLayerKind::Static,
    ),
    ("Compact template", "## Compact", PromptLayerKind::Static),
    (
        "Configured instructions",
        "<instructions ",
        PromptLayerKind::Dynamic,
    ),
    ("User memory", "## User Memory", PromptLayerKind::Dynamic),
    (
        "Current session goal",
        "## Current Session Goal",
        PromptLayerKind::Dynamic,
    ),
    (
        "Previous session relay",
        "## Previous Session Relay",
        PromptLayerKind::Dynamic,
    ),
    (
        "Volatile working set",
        WORKING_SET_MARKER,
        PromptLayerKind::Dynamic,
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromptLayerKind {
    Static,
    Dynamic,
}

impl PromptLayerKind {
    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::Static => tr(locale, MessageId::CtxInspCacheFriendly),
            Self::Dynamic => tr(locale, MessageId::CtxInspChangesByTurn),
        }
    }
}

/// Localize well-known layer labels that already have inspector MessageIds.
/// Other layer names stay as English product identifiers.
fn layer_display_name(name: &'static str, locale: Locale) -> Cow<'static, str> {
    match name {
        "Volatile working set" => tr(locale, MessageId::CtxInspVolatileWorkingSet),
        other => Cow::Borrowed(other),
    }
}

#[derive(Debug)]
struct PromptTextLayer<'a> {
    name: &'static str,
    kind: PromptLayerKind,
    body: &'a str,
}

#[must_use]
pub fn build_context_inspector_text(app: &App, locale: Locale) -> String {
    let mut out = String::new();
    let usage = context_usage(app);
    let (used, max, percent) = usage;

    let _ = writeln!(out, "{}", tr(locale, MessageId::CtxInspSessionContext));
    let _ = writeln!(out, "---------------");
    let _ = writeln!(
        out,
        "{}: {}",
        tr(locale, MessageId::CtxInspModel),
        app.model
    );
    let _ = writeln!(
        out,
        "{}: {}",
        tr(locale, MessageId::CtxInspWorkspace),
        codewhale_util::utils::display_path(&app.workspace)
    );
    if let Some(session_id) = app.current_session_id.as_deref() {
        let _ = writeln!(
            out,
            "{}: {}",
            tr(locale, MessageId::CtxInspSession),
            crate::session_manager::truncate_id(session_id)
        );
    }
    // Real provider-token cache hit rate from the same records /cache
    // aggregates (C3): only provider-reported cache telemetry counts, so the
    // number is what the user actually paid to keep, not a predicted guess.
    let mut cache_turns = 0u64;
    let (cache_hit, cache_miss) =
        app.session
            .turn_cache_history
            .iter()
            .fold((0u64, 0u64), |(hit, miss), record| {
                let Some(hit_tokens_u32) = record.cache_hit_tokens else {
                    return (hit, miss);
                };
                let hit_tokens = u64::from(hit_tokens_u32);
                let miss_tokens = u64::from(
                    record
                        .cache_miss_tokens
                        .unwrap_or(record.input_tokens.saturating_sub(hit_tokens_u32)),
                );
                cache_turns += 1;
                (hit + hit_tokens, miss + miss_tokens)
            });
    let cache_total = cache_hit + cache_miss;
    if cache_turns > 0 && cache_total > 0 {
        let cache_percent = (cache_hit as f64 / cache_total as f64 * 100.0).clamp(0.0, 100.0);
        let _ = writeln!(
            out,
            "Provider cache hit rate: {cache_percent:.1}% over {cache_turns} cache-aware turn{}",
            if cache_turns == 1 { "" } else { "s" },
        );
    } else {
        let _ = writeln!(out, "Provider cache hit rate: no cache telemetry yet");
    }
    let status_label = match context_status(percent) {
        ContextPressure::Critical => tr(locale, MessageId::CtxInspCritical),
        ContextPressure::High => tr(locale, MessageId::CtxInspHigh),
        ContextPressure::Ok => tr(locale, MessageId::CtxInspOk),
    };
    let tokens_unit = tr(locale, MessageId::CtxInspTokens);
    let _ = writeln!(
        out,
        "{ctx_label}: {status_label} - ~{used}/{max} {tokens_unit} ({percent:.1}%)",
        ctx_label = tr(locale, MessageId::CtxInspContext),
    );
    let cells = tr(locale, MessageId::CtxInspCells);
    let api_msgs = tr(locale, MessageId::CtxInspApiMessages);
    let _ = writeln!(
        out,
        "{label}: {} {cells}, {} {api_msgs}",
        app.history.len(),
        app.api_messages.len(),
        label = tr(locale, MessageId::CtxInspTranscript),
    );
    if let Some(kept) = last_round_kept_count(&app.api_messages) {
        let _ = writeln!(
            out,
            "Last compaction: kept last round verbatim ({kept} messages); earlier turns summarized."
        );
    }
    let _ = writeln!(
        out,
        "{}: {}",
        tr(locale, MessageId::CtxInspWorkspaceStatus),
        app.workspace_context
            .as_deref()
            .unwrap_or(&*tr(locale, MessageId::CtxInspNotSampledYet))
    );

    let _ = writeln!(out);
    push_compaction_and_anchors(&mut out, app, locale);
    let _ = writeln!(out);
    push_system_prompt_structure(&mut out, app, locale);
    if let Some(record) = app
        .api_messages
        .iter()
        .rev()
        .find_map(crate::runtime_handoff::constitution_display)
    {
        let source = match record
            .split_once("\n\n")
            .map(|(_, guidance)| guidance.lines().next().unwrap_or_default())
        {
            Some(line) if line.starts_with("Account profile constitution, settings revision ") => {
                tr(locale, MessageId::CtxInspConstitutionAccount).replace(
                    "{revision}",
                    line.trim_start_matches("Account profile constitution, settings revision ")
                        .trim_end_matches('.'),
                )
            }
            Some(_) => tr(locale, MessageId::CtxInspConstitutionLocal).into_owned(),
            None => tr(locale, MessageId::CtxInspConstitutionWithdrawn).into_owned(),
        };
        let label = tr(locale, MessageId::CtxInspPersonalConstitution);
        let tokens = record.chars().count().div_ceil(3);
        let unit = tr(locale, MessageId::CtxInspTokens);
        let cadence = tr(locale, MessageId::CtxInspChangesByTurn);
        let _ = writeln!(out, "\n{label} ({source}): ~{tokens} {unit} [{cadence}]");
    }
    let _ = writeln!(out);
    push_references(&mut out, &app.session_context_references, locale);
    let _ = writeln!(out);
    push_tools(&mut out, app, locale);
    push_tool_schema_costs(&mut out, app, locale);

    out
}

fn context_usage(app: &App) -> (usize, u32, f64) {
    let max = crate::route_budget::route_context_window_tokens(
        app.api_provider,
        app.effective_model_for_budget(),
        app.active_route_limits,
    );
    // The meter must show the SAME pressure signal the auto-compaction trigger
    // decides on (compaction::estimate_input_tokens_for_pressure, the
    // non-inflated estimate with framing overhead). The old conservative
    // overflow estimator was ~1.5x larger, so the meter showed the trigger
    // point as "free" while compaction was still far away (ops T1) — and vice
    // versa the meter read "free" as negative when the engine was only halfway.
    let estimated =
        estimate_input_tokens_for_pressure(&app.api_messages, app.system_prompt.as_ref());
    // The trigger decides on max(estimate, provider-billed prompt); the meter
    // must too, or a provider billing above the local estimate (non-ASCII
    // text, server-side framing) makes the meter under-show real pressure
    // (#5577). The billed receipt is per model call, so it goes stale only
    // until the next step or compaction updates it.
    let used = estimated.max(
        app.last_billed_input_tokens
            .map(|tokens| tokens as usize)
            .unwrap_or(0),
    );
    let percent = ((used as f64 / f64::from(max)) * 100.0).clamp(0.0, 100.0);
    (used, max, percent)
}

enum ContextPressure {
    Ok,
    High,
    Critical,
}

fn context_status(percent: f64) -> ContextPressure {
    if percent >= CONTEXT_CRITICAL_THRESHOLD_PERCENT {
        ContextPressure::Critical
    } else if percent >= CONTEXT_WARNING_THRESHOLD_PERCENT {
        ContextPressure::High
    } else {
        ContextPressure::Ok
    }
}

fn compaction_path_label(path: CompactionPath, locale: Locale) -> Cow<'static, str> {
    match path {
        CompactionPath::Summary => tr(locale, MessageId::CtxInspCompactionPathSummary),
        CompactionPath::PruneOnly => tr(locale, MessageId::CtxInspCompactionPathPrune),
    }
}

fn compaction_assistant_clause(kept: bool, locale: Locale) -> Cow<'static, str> {
    if kept {
        tr(locale, MessageId::CtxInspCompactionAssistantKept)
    } else {
        Cow::Borrowed("")
    }
}

fn last_round_messages(messages: &[codewhale_models::Message]) -> &[codewhale_models::Message] {
    let start = last_round_start(messages).min(messages.len());
    &messages[start..]
}

fn compaction_detail_for_app(app: &App, locale: Locale) -> (String, usize) {
    let keep = inspect_compaction_keep(&app.api_messages);
    let assistant = compaction_assistant_clause(keep.last_round_assistant, locale);
    let last_round_tokens =
        estimate_input_tokens_for_pressure(last_round_messages(&app.api_messages), None);
    let detail = if let Some(snapshot) = app.last_compaction.as_ref() {
        tr(locale, MessageId::CtxInspCompactionDetail)
            .replace(
                "{path}",
                &compaction_path_label(snapshot.coverage.path, locale),
            )
            .replace("{before}", &snapshot.messages_before.to_string())
            .replace("{after}", &snapshot.messages_after.to_string())
            .replace(
                "{round}",
                &snapshot.coverage.last_round_messages.to_string(),
            )
            .replace(
                "{tools}",
                &snapshot.coverage.last_round_tool_results.to_string(),
            )
            .replace("{assistant}", &assistant)
    } else if keep.has_checkpoint {
        tr(locale, MessageId::CtxInspCompactionRestored)
            .replace("{round}", &keep.last_round_messages.to_string())
            .replace("{tools}", &keep.last_round_tool_results.to_string())
            .replace("{assistant}", &assistant)
    } else {
        tr(locale, MessageId::CtxInspCompactionNever).into_owned()
    };
    (detail, last_round_tokens)
}

fn anchors_detail_for_app(app: &App, locale: Locale) -> (String, usize) {
    match pinned_anchors_text(Some(&app.workspace)) {
        Some(text) => {
            let chars = text.chars().count();
            (
                tr(locale, MessageId::CtxInspAnchorsPresent).replace("{chars}", &chars.to_string()),
                chars.div_ceil(3),
            )
        }
        None => (tr(locale, MessageId::CtxInspAnchorsNone).into_owned(), 0),
    }
}

fn push_compaction_and_anchors(out: &mut String, app: &App, locale: Locale) {
    let (compaction_detail, _) = compaction_detail_for_app(app, locale);
    let (anchors_detail, _) = anchors_detail_for_app(app, locale);
    let _ = writeln!(out, "{}", tr(locale, MessageId::CtxInspRowCompaction));
    let _ = writeln!(out, "----------");
    let _ = writeln!(out, "{compaction_detail}");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", tr(locale, MessageId::CtxInspRowAnchors));
    let _ = writeln!(out, "-------");
    let _ = writeln!(out, "{anchors_detail}");
}

/// Inspect the system prompt structure, split into cache-friendly stable
/// prefix blocks and the volatile working-set tail block.
fn push_system_prompt_structure(out: &mut String, app: &App, locale: Locale) {
    let _ = writeln!(out, "{}", tr(locale, MessageId::CtxInspSystemPrompt));
    let _ = writeln!(out, "-----------------------");

    // Conservative token estimate: ~3 chars per token (consistent with
    // compaction.rs internal helpers — replicated here to avoid depending
    // on a private function).
    let text_tokens = |text: &str| text.chars().count().div_ceil(3);

    let total_est = match &app.system_prompt {
        Some(SystemPrompt::Text(t)) => text_tokens(t),
        Some(SystemPrompt::Blocks(blocks)) => blocks.iter().map(|b| text_tokens(&b.text)).sum(),
        None => 0,
    };

    let stable_lbl = tr(locale, MessageId::CtxInspStablePrefix);
    let volatile_lbl = tr(locale, MessageId::CtxInspVolatileWorkingSet);
    let first_line_lbl = tr(locale, MessageId::CtxInspFirstLine);
    let total_lbl = tr(locale, MessageId::CtxInspTotal);
    let text_prompt_lbl = tr(locale, MessageId::CtxInspTextPromptLayers);
    let single_blob_lbl = tr(locale, MessageId::CtxInspSingleTextBlob);
    let blocks_unit = tr(locale, MessageId::CtxInspBlocks);
    let block_unit = tr(locale, MessageId::CtxInspBlock);
    let tokens_unit = tr(locale, MessageId::CtxInspTokens);
    let layers_unit = tr(locale, MessageId::CtxInspLayers);
    let none_lbl = tr(locale, MessageId::CtxInspNone);
    let empty_lbl = tr(locale, MessageId::CtxInspEmpty);
    let cache_friendly = tr(locale, MessageId::CtxInspCacheFriendly);
    let changes_by_turn = tr(locale, MessageId::CtxInspChangesByTurn);
    let stable_only = tr(locale, MessageId::CtxInspStablePrefixOnly);
    let no_system_prompt = tr(locale, MessageId::CtxInspNoSystemPrompt);
    match &app.system_prompt {
        Some(SystemPrompt::Blocks(blocks)) => {
            let working_set_idx = blocks
                .iter()
                .position(|b| b.text.contains(WORKING_SET_MARKER));
            let (stable_count, working_block) = match working_set_idx {
                Some(idx) => (idx, Some(&blocks[idx])),
                None => (blocks.len(), None),
            };

            let stable_tokens: usize = blocks
                .iter()
                .take(stable_count)
                .map(|b| text_tokens(&b.text))
                .sum();
            let working_tokens = working_block.map(|b| text_tokens(&b.text)).unwrap_or(0);

            let _ = writeln!(
                out,
                "  {stable_lbl}: {stable_count} {blocks_unit}, ~{stable_tokens} {tokens_unit}  [{cache_friendly}]"
            );
            if let Some(block) = working_block {
                let _ = writeln!(
                    out,
                    "  {volatile_lbl}: 1 {block_unit}, ~{working_tokens} {tokens_unit}  [{changes_by_turn}]"
                );
                let _ = writeln!(
                    out,
                    "    {first_line_lbl}: {}",
                    block.text.lines().next().unwrap_or(&*empty_lbl)
                );
            } else {
                let _ = writeln!(out, "  {volatile_lbl}: {none_lbl}");
            }
            let _ = writeln!(
                out,
                "  {total_lbl}: {} {blocks_unit}, ~{total_est} {tokens_unit}",
                blocks.len()
            );
            let layers = blocks
                .iter()
                .flat_map(|block| split_text_prompt_layers(&block.text))
                .filter(|layer| !layer.body.is_empty())
                .collect::<Vec<_>>();
            if layers.iter().any(|layer| layer.name != "System prompt") {
                let _ = writeln!(out, "  {text_prompt_lbl}:");
                for layer in layers {
                    let tokens = text_tokens(layer.body);
                    let kind_lbl = layer.kind.label(locale);
                    let layer_name = layer_display_name(layer.name, locale);
                    let _ = writeln!(
                        out,
                        "  - {layer_name}: ~{tokens} {tokens_unit} [{kind_lbl}]",
                    );
                }
            }
        }
        Some(SystemPrompt::Text(text)) => {
            let layers = split_text_prompt_layers(text);
            if layers.len() > 1
                || layers
                    .first()
                    .is_some_and(|layer| layer.name != "System prompt")
            {
                let _ = writeln!(
                    out,
                    "  {text_prompt_lbl}: {} {layers_unit}, ~{total_est} {tokens_unit}",
                    layers.len()
                );
                for layer in layers {
                    let tokens = text_tokens(layer.body);
                    let kind_lbl = layer.kind.label(locale);
                    let layer_name = layer_display_name(layer.name, locale);
                    let _ = writeln!(
                        out,
                        "  - {layer_name}: ~{tokens} {tokens_unit} [{kind_lbl}]",
                    );
                }
            } else {
                let _ = writeln!(
                    out,
                    "  {single_blob_lbl} (~{total_est} {tokens_unit}) [{stable_only}]"
                );
            }
        }
        None => {
            let _ = writeln!(out, "  {no_system_prompt}");
        }
    }

    // Cache-economics hint
    let _ = writeln!(out, "  {}", tr(locale, MessageId::CtxInspCacheTip));
}

fn split_text_prompt_layers(text: &str) -> Vec<PromptTextLayer<'_>> {
    let mut starts = SYSTEM_LAYER_MARKERS
        .iter()
        .filter_map(|(name, marker, kind)| text.find(marker).map(|idx| (idx, *name, *kind)))
        .collect::<Vec<_>>();
    starts.sort_by_key(|(idx, _, _)| *idx);

    let Some((first_idx, _, _)) = starts.first().copied() else {
        return vec![PromptTextLayer {
            name: "System prompt",
            kind: PromptLayerKind::Static,
            body: text.trim(),
        }];
    };

    let mut layers = Vec::new();
    if first_idx > 0 {
        layers.push(PromptTextLayer {
            name: "Global system prefix",
            kind: PromptLayerKind::Static,
            body: text[..first_idx].trim(),
        });
    }

    for (i, (start, name, kind)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(text.len(), |(idx, _, _)| *idx);
        layers.push(PromptTextLayer {
            name,
            kind: *kind,
            body: text[*start..end].trim(),
        });
    }

    layers
}

fn push_references(out: &mut String, references: &[SessionContextReference], locale: Locale) {
    let _ = writeln!(out, "{}", tr(locale, MessageId::CtxInspReferences));
    let _ = writeln!(out, "----------");

    let mut seen = HashSet::new();
    let mut rendered = 0usize;
    for record in references {
        let reference = &record.reference;
        let key = format!(
            "{:?}:{:?}:{}:{}",
            reference.source, reference.kind, reference.target, reference.label
        );
        if !seen.insert(key) {
            continue;
        }
        if rendered >= MAX_REFERENCE_ROWS {
            let remaining = references.len().saturating_sub(rendered);
            if remaining > 0 {
                let _ = writeln!(
                    out,
                    "- ... {remaining} {}",
                    tr(locale, MessageId::CtxInspMoreReferences)
                );
            }
            break;
        }

        let prefix = match reference.source {
            ContextReferenceSource::AtMention => "@",
            ContextReferenceSource::Attachment => "/attach ",
        };
        let state = if reference.included {
            if reference.expanded {
                tr(locale, MessageId::CtxInspIncluded)
            } else {
                tr(locale, MessageId::CtxInspAttached)
            }
        } else {
            tr(locale, MessageId::CtxInspNotIncluded)
        };
        let detail = reference
            .detail
            .as_deref()
            .filter(|detail| !detail.trim().is_empty())
            .map(|detail| format!(" - {detail}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "- [{}] {prefix}{} -> {} ({state}{detail})",
            reference.badge, reference.label, reference.target
        );
        rendered += 1;
    }

    if rendered == 0 {
        let _ = writeln!(out, "- {}", tr(locale, MessageId::CtxInspNoReferences));
    }
}

fn push_tools(out: &mut String, app: &App, locale: Locale) {
    let _ = writeln!(out, "{}", tr(locale, MessageId::CtxInspRecentTools));
    let _ = writeln!(out, "------------");

    let mut rows: Vec<(usize, &ToolDetailRecord)> = app
        .tool_details_by_cell
        .iter()
        .map(|(idx, detail)| (*idx, detail))
        .collect();
    rows.sort_by_key(|(idx, _)| std::cmp::Reverse(*idx));

    let mut rendered = 0usize;
    for detail in app.active_tool_details.values() {
        let location = tr(locale, MessageId::CtxInspActive);
        push_tool_row(out, locale, &location, detail);
        rendered += 1;
        if rendered >= MAX_TOOL_ROWS {
            return;
        }
    }
    for (cell_idx, detail) in rows
        .into_iter()
        .take(MAX_TOOL_ROWS.saturating_sub(rendered))
    {
        let location = format!("{} {cell_idx}", tr(locale, MessageId::CtxInspCell));
        push_tool_row(out, locale, &location, detail);
        rendered += 1;
    }

    if rendered == 0 {
        let _ = writeln!(out, "- {}", tr(locale, MessageId::CtxInspNoToolActivity));
    } else {
        let details = crate::tui::shell_key_routing::display_chord(
            crate::tui::shell_key_routing::binding(
                crate::tui::shell_key_routing::ShellBindingId::ToolDetails,
            )
            .footer_chord,
        );
        let _ = writeln!(
            out,
            "- {}",
            tr(locale, MessageId::CtxInspVHint).replace("{details}", details.as_ref())
        );
    }
}

fn push_tool_row(out: &mut String, locale: Locale, location: &str, detail: &ToolDetailRecord) {
    let output_state = if detail.output.as_deref().is_some_and(|out| !out.is_empty()) {
        tr(locale, MessageId::CtxInspOutputCaptured)
    } else {
        tr(locale, MessageId::CtxInspNoOutputYet)
    };
    let _ = writeln!(
        out,
        "- [{}] {} {} ({output_state})",
        location,
        detail.tool_name,
        short_tool_id(&detail.tool_id)
    );
}

fn tool_schema_tokens(tool: &Tool) -> usize {
    serde_json::to_string(tool)
        .map(|schema| schema.chars().count().div_ceil(SCHEMA_TOKEN_DIVISOR))
        .unwrap_or_default()
}

fn push_tool_schema_costs(out: &mut String, app: &App, locale: Locale) {
    let Some(catalog) = app.session.last_tool_catalog.as_ref() else {
        return;
    };

    let _ = writeln!(out);
    let tokens = tr(locale, MessageId::CtxInspTokens);
    let schema_costs_label = tr(locale, MessageId::CtxInspToolSchemaCosts);
    let _ = writeln!(out, "{} ({})", schema_costs_label, tokens);
    let _ = writeln!(out, "------------");

    let mut built_in: Vec<(String, usize)> = catalog
        .iter()
        .filter(|tool| !crate::mcp::McpPool::is_mcp_tool(&tool.name))
        .map(|tool| (tool.name.clone(), tool_schema_tokens(tool)))
        .collect();
    built_in.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let built_in_total: usize = built_in.iter().map(|(_, cost)| cost).sum();
    let _ = writeln!(
        out,
        "- [catalog] ~{built_in_total} {tokens} ({} tools)",
        built_in.len()
    );
    for (name, cost) in built_in.iter().take(MAX_SCHEMA_COST_ROWS) {
        let _ = writeln!(out, "  - {name}: ~{cost} {tokens}");
    }
    if built_in.len() > MAX_SCHEMA_COST_ROWS {
        let _ = writeln!(
            out,
            "  - ... {} more catalog tools",
            built_in.len() - MAX_SCHEMA_COST_ROWS
        );
    }

    let Some(snapshot) = app.mcp_snapshot.as_ref() else {
        return;
    };
    for server in &snapshot.servers {
        let mut server_tokens = 0usize;
        let mut catalog_tools = 0usize;
        for announced in &server.tools {
            if let Some(tool) = catalog
                .iter()
                .find(|tool| tool.name == announced.model_name)
            {
                server_tokens += tool_schema_tokens(tool);
                catalog_tools += 1;
            }
        }
        let _ = writeln!(
            out,
            "- [mcp:{}] ~{server_tokens} {tokens} ({catalog_tools}/{} announced tools)",
            server.name,
            server.tools.len()
        );
    }
}

fn short_tool_id(id: &str) -> String {
    // Slice by characters, not bytes: a tool id from a gateway can contain
    // multibyte characters, and `&id[..8]` panics on a byte index that lands
    // mid-codepoint (2026-08-04 review).
    let mut chars = id.chars();
    let head: String = chars.by_ref().take(8).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

#[derive(Debug, Clone)]
struct ContextBucket {
    label: String,
    tokens: usize,
    percent: f64,
    detail: String,
}

/// Live context surface. The host refreshes its snapshot immediately before
/// every render, so opening it never freezes the underlying session facts.
pub(crate) struct ContextInspectorView {
    used: usize,
    max: u32,
    percent: f64,
    model: String,
    workspace: String,
    threshold: f64,
    rows: Vec<ContextBucket>,
    selected: usize,
    hitboxes: RefCell<Vec<(u16, usize)>>,
    locale: Locale,
}

impl ContextInspectorView {
    #[must_use]
    pub(crate) fn new(app: &App) -> Self {
        let mut view = Self {
            used: 0,
            max: 0,
            percent: 0.0,
            model: String::new(),
            workspace: String::new(),
            threshold: 0.0,
            rows: Vec::new(),
            selected: 0,
            hitboxes: RefCell::new(Vec::new()),
            locale: app.ui_locale,
        };
        view.refresh_from_app(app);
        view
    }

    pub(crate) fn refresh_from_app(&mut self, app: &App) {
        let (used, max, percent) = context_usage(app);
        let system_tokens = estimate_input_tokens_for_pressure(&[], app.system_prompt.as_ref());
        let message_tokens = used.saturating_sub(system_tokens);
        let free_tokens = usize::try_from(max)
            .unwrap_or(usize::MAX)
            .saturating_sub(used);
        let full_detail = build_context_inspector_text(app, app.ui_locale);
        self.used = used;
        self.max = max;
        self.percent = percent;
        self.model = app.model_display_label();
        self.workspace = codewhale_util::utils::display_path(&app.workspace);
        self.threshold = app.auto_compact_threshold_percent;
        self.locale = app.ui_locale;
        let max_f = f64::from(max.max(1));
        let (compaction_detail, compaction_tokens) = compaction_detail_for_app(app, self.locale);
        let (anchors_detail, anchors_tokens) = anchors_detail_for_app(app, self.locale);
        self.rows = vec![
            ContextBucket {
                label: tr(self.locale, MessageId::CtxInspRowSystemPrompt).into_owned(),
                tokens: system_tokens,
                percent: (system_tokens as f64 / max_f) * 100.0,
                detail: full_detail.clone(),
            },
            ContextBucket {
                label: tr(self.locale, MessageId::CtxInspRowMessages).into_owned(),
                tokens: message_tokens,
                percent: (message_tokens as f64 / max_f) * 100.0,
                detail: full_detail,
            },
            ContextBucket {
                label: tr(self.locale, MessageId::CtxInspRowFree).into_owned(),
                tokens: free_tokens,
                percent: (free_tokens as f64 / max_f) * 100.0,
                detail: tr(self.locale, MessageId::CtxInspFreeTokensDetail)
                    .replace("{free}", &free_tokens.to_string())
                    .replace("{threshold}", &format!("{:.0}", self.threshold)),
            },
            ContextBucket {
                label: tr(self.locale, MessageId::CtxInspRowCompaction).into_owned(),
                tokens: compaction_tokens,
                percent: (compaction_tokens as f64 / max_f) * 100.0,
                detail: compaction_detail,
            },
            ContextBucket {
                label: tr(self.locale, MessageId::CtxInspRowAnchors).into_owned(),
                tokens: anchors_tokens,
                percent: (anchors_tokens as f64 / max_f) * 100.0,
                detail: anchors_detail,
            },
        ];
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    #[cfg(test)]
    fn row_labels(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.label.clone()).collect()
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected + delta as usize).min(self.rows.len() - 1)
        };
    }

    fn open_selected(&self) -> ViewAction {
        let Some(row) = self.rows.get(self.selected) else {
            return ViewAction::None;
        };
        ViewAction::Emit(ViewEvent::OpenTextPager {
            title: tr(self.locale, MessageId::CtxInspDrillTitle).replace("{row}", &row.label),
            content: row.detail.clone(),
        })
    }
}

impl ModalView for ContextInspectorView {
    fn kind(&self) -> ModalKind {
        ModalKind::ContextInspector
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                ViewAction::None
            }
            KeyCode::Enter => self.open_selected(),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.move_selection(-1);
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.move_selection(1);
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .hitboxes
                    .borrow()
                    .iter()
                    .find_map(|(y, idx)| (*y == mouse.row).then_some(*idx));
                let Some(idx) = hit else {
                    return ViewAction::None;
                };
                if idx == self.selected {
                    self.open_selected()
                } else {
                    self.selected = idx;
                    ViewAction::None
                }
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let inner =
            render_underwater_surface(area, buf, tr(self.locale, MessageId::CtxInspSurfaceTitle));
        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new("↑/↓", tr(self.locale, MessageId::CtxInspActionSelect)),
                ActionHint::new("Enter", tr(self.locale, MessageId::CtxInspActionDrillDown)),
                ActionHint::new("Esc", tr(self.locale, MessageId::CtxInspActionClose)),
            ],
        );
        let width = usize::from(content.width);
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    tr(self.locale, MessageId::CtxInspUsedTokens)
                        .replace("{used}", &self.used.to_string())
                        .replace("{max}", &self.max.to_string()),
                    Style::default()
                        .fg(palette::WHALE_ACTION)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {:.1}% · {}", self.percent, self.model),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
            ]),
            Line::from(Span::styled(
                crate::tui::ui_text::semantic_truncate(&self.workspace, width),
                Style::default().fg(palette::TEXT_DIM),
            )),
            Line::from(""),
        ];

        if content.height >= 11 && content.width >= 24 {
            let cells = usize::from(content.width.saturating_sub(2)).min(60);
            let system_cells = ((self.rows[0].percent / 100.0) * cells as f64).round() as usize;
            let message_cells = ((self.rows[1].percent / 100.0) * cells as f64).round() as usize;
            let system_cells = system_cells.min(cells);
            let message_cells = message_cells.min(cells.saturating_sub(system_cells));
            let free_cells = cells.saturating_sub(system_cells + message_cells);
            lines.push(Line::from(vec![
                Span::styled(
                    "#".repeat(system_cells),
                    Style::default().fg(palette::WHALE_ACTION),
                ),
                Span::styled(
                    "=".repeat(message_cells),
                    Style::default().fg(palette::TEXT_PRIMARY),
                ),
                Span::styled(
                    ".".repeat(free_cells),
                    Style::default().fg(palette::TEXT_DIM),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                tr(self.locale, MessageId::CtxInspAutoCompactAt)
                    .replace("{threshold}", &format!("{:.0}", self.threshold)),
                Style::default().fg(palette::TEXT_HINT),
            )));
            lines.push(Line::from(""));
        }

        self.hitboxes.borrow_mut().clear();
        for (idx, row) in self.rows.iter().enumerate() {
            let selected = idx == self.selected;
            let marker = crate::tui::glyphs::selection_marker(selected);
            let style = if selected {
                menu_style::selected_row_style()
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            let value = tr(self.locale, MessageId::CtxInspRowTokens)
                .replace("{tokens}", &row.tokens.to_string())
                .replace("{percent}", &format!("{:.1}", row.percent));
            // Display cells, not bytes: a CJK or localized row measured in
            // bytes padded short and pushed the value off the row (U04-m5).
            let value_width = crate::tui::ui_text::text_display_width(&value);
            let label_width = width.saturating_sub(value_width + 5);
            let label = crate::tui::ui_text::semantic_truncate(&row.label, label_width);
            let gap = width
                .saturating_sub(crate::tui::ui_text::text_display_width(&label) + value_width + 3);
            let y = content
                .y
                .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX));
            self.hitboxes.borrow_mut().push((y, idx));
            lines.push(Line::from(Span::styled(
                format!("{marker} {label}{}{value}", " ".repeat(gap)),
                style,
            )));
        }
        Paragraph::new(lines).render(content, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;
    use codewhale_runtime::config::Config;

    #[test]
    fn short_tool_id_never_panics_on_multibyte() {
        // ASCII short/long behave as before.
        assert_eq!(short_tool_id("abc"), "abc");
        assert_eq!(short_tool_id("0123456789"), "01234567...");
        // A multibyte id must truncate on a char boundary, not panic.
        // 10 chars → first 8 kept, then the ellipsis.
        assert_eq!(short_tool_id("日本語のツールid名"), "日本語のツールi...");
        assert_eq!(short_tool_id("café"), "café");
    }

    use crate::mcp::{McpDiscoveredItem, McpManagerSnapshot, McpServerSnapshot};
    use crate::session_manager::SessionContextReference;
    use crate::tui::app::TuiOptions;
    use crate::tui::history::HistoryCell;
    use codewhale_core::{ContextReference, ContextReferenceKind, ContextReferenceSource};
    use codewhale_models::{ContentBlock, Message, Tool};
    use std::path::PathBuf;

    use codewhale_localization::Locale;

    fn test_app() -> App {
        let mut app = App::new(
            TuiOptions {
                model: "unknown-model".to_string(),
                skills_dir: PathBuf::from("/tmp/skills"),
                notes_path: PathBuf::from("notes.md"),
                ..crate::tui::test_support::test_tui_options(PathBuf::from("/tmp/project"))
            },
            &Config::default(),
        );
        // Pin the route identity: App::new consults the developer's real
        // saved settings, so on a machine with customized provider/model
        // the context-window assertions computed against a different route.
        app.api_provider = codewhale_config::ProviderKind::Deepseek;
        app.auto_model = false;
        app.last_effective_model = None;
        app.active_route_limits = None;
        app.active_context_window_override = None;
        app
    }

    #[test]
    fn inspector_reports_the_provider_cache_hit_rate() {
        let mut app = test_app();
        for (input, hit) in [(1_000u32, 800u32), (1_000, 500)] {
            app.session
                .turn_cache_history
                .push_back(crate::tui::app::TurnCacheRecord {
                    provider: None,
                    provider_identity: None,
                    model: None,
                    auto_model: false,
                    input_tokens: input,
                    output_tokens: 0,
                    cache_hit_tokens: Some(hit),
                    cache_miss_tokens: None,
                    reasoning_replay_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                    cost_audit: None,
                    recorded_at: std::time::Instant::now(),
                });
        }
        let text = build_context_inspector_text(&app, Locale::En);
        assert!(
            text.contains("Provider cache hit rate: 65.0% over 2 cache-aware turns"),
            "{text}"
        );

        let empty = build_context_inspector_text(&test_app(), Locale::En);
        assert!(empty.contains("no cache telemetry yet"), "{empty}");
    }

    #[test]
    fn inspector_formats_empty_state() {
        let app = test_app();
        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("Session Context"));
        assert!(text.contains("No file, folder, or media references yet."));
        assert!(text.contains("No tool activity yet."));
    }

    fn schema_tool(name: &str, property_count: usize) -> Tool {
        let properties = (0..property_count)
            .map(|index| {
                (
                    format!("field_{index}"),
                    serde_json::json!({"type": "string"}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        Tool {
            tool_type: Some("function".to_string()),
            name: name.to_string(),
            description: format!("schema for {name}"),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": properties,
            }),
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    #[test]
    fn inspector_reports_catalog_and_mcp_schema_costs_with_bounded_rows() {
        let mut app = test_app();
        let mut catalog = (0..(MAX_SCHEMA_COST_ROWS + 2))
            .map(|index| schema_tool(&format!("tool_{index}"), index + 1))
            .collect::<Vec<_>>();
        catalog.push(schema_tool("mcp_local_echo", 3));
        app.session.last_tool_catalog = Some(catalog);
        app.mcp_snapshot = Some(McpManagerSnapshot {
            config_path: PathBuf::from("/tmp/mcp.json"),
            config_exists: true,
            reload_required: false,
            servers: vec![
                McpServerSnapshot {
                    name: "local".to_string(),
                    enabled: true,
                    required: false,
                    transport: "stdio".to_string(),
                    command_or_url: "echo".to_string(),
                    connect_timeout: 1,
                    execute_timeout: 1,
                    read_timeout: 1,
                    connected: true,
                    error: None,
                    auth_required: false,
                    capability_metadata: Default::default(),
                    tools: vec![McpDiscoveredItem {
                        name: "echo".to_string(),
                        model_name: "mcp_local_echo".to_string(),
                        description: Some("echoes input".to_string()),
                    }],
                    resources: Vec::new(),
                    prompts: Vec::new(),
                },
                McpServerSnapshot {
                    name: "empty".to_string(),
                    enabled: true,
                    required: false,
                    transport: "stdio".to_string(),
                    command_or_url: "empty".to_string(),
                    connect_timeout: 1,
                    execute_timeout: 1,
                    read_timeout: 1,
                    connected: true,
                    error: None,
                    auth_required: false,
                    capability_metadata: Default::default(),
                    tools: Vec::new(),
                    resources: Vec::new(),
                    prompts: Vec::new(),
                },
            ],
        });

        let text = build_context_inspector_text(&app, Locale::En);
        assert_eq!(
            text.matches("Recent Tools").count(),
            1,
            "schema costs need a distinct section heading: {text}"
        );
        assert!(
            text.contains("\n\nTool schema costs (tokens)\n------------"),
            "schema costs need a separated localized section: {text}"
        );
        assert!(text.contains("[catalog]"), "catalog total missing: {text}");
        assert!(text.contains("tool_25"), "catalog row missing: {text}");
        assert!(
            text.contains("more catalog tools"),
            "catalog bound missing: {text}"
        );
        assert!(
            text.contains("[mcp:local]"),
            "MCP server row missing: {text}"
        );
        assert!(
            text.contains("1/1 announced tools"),
            "MCP tool cost missing: {text}"
        );
        assert!(
            text.contains("[mcp:empty] ~0"),
            "empty MCP row missing: {text}"
        );
    }

    #[test]
    fn inspector_uses_compact_session_id() {
        let mut app = test_app();
        app.current_session_id = Some("1234567890abcdef".to_string());

        let text = build_context_inspector_text(&app, Locale::En);

        assert!(text.contains("Session: 12345678"), "{text}");
        assert!(!text.contains("1234567890abcdef"), "{text}");
    }

    #[test]
    fn inspector_lists_context_references() {
        let mut app = test_app();
        app.history.push(HistoryCell::User {
            content: "read @src/main.rs".to_string(),
        });
        app.session_context_references
            .push(SessionContextReference {
                message_index: 0,
                reference: ContextReference {
                    kind: ContextReferenceKind::File,
                    source: ContextReferenceSource::AtMention,
                    badge: "file".to_string(),
                    label: "src/main.rs".to_string(),
                    target: "/tmp/project/src/main.rs".to_string(),
                    included: true,
                    expanded: true,
                    detail: Some("included".to_string()),
                },
            });

        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("[file] @src/main.rs -> /tmp/project/src/main.rs"));
    }

    #[test]
    fn inspector_marks_high_context_pressure() {
        let mut app = test_app();
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "x".repeat(4_000_000),
                cache_control: None,
            }],
        });

        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("Context: critical"), "{text}");
    }

    #[test]
    fn inspector_uses_effective_auto_model_context_window() {
        let mut app = test_app();
        app.model = "auto".to_string();
        app.auto_model = true;
        app.last_effective_model = Some("deepseek-v4-pro".to_string());

        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("Model: auto"), "{text}");
        assert!(text.contains("/1000000 tokens"), "{text}");
    }

    #[test]
    fn inspector_no_system_prompt_shows_section() {
        let app = test_app();
        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("System Prompt Structure"));
        assert!(text.contains("No system prompt set."));
    }

    #[test]
    fn inspector_blocks_format_shows_stable_prefix_and_working_set() {
        let mut app = test_app();
        use codewhale_models::SystemBlock;
        app.system_prompt = Some(SystemPrompt::Blocks(vec![
            SystemBlock {
                block_type: "text".to_string(),
                text: "## Stable Base\n\nYou are CodeWhale.".to_string(),
                cache_control: None,
            },
            SystemBlock {
                block_type: "text".to_string(),
                text: format!("{WORKING_SET_MARKER}\nsrc/main.rs changed"),
                cache_control: None,
            },
        ]));

        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("System Prompt Structure"));
        assert!(
            text.contains("Stable prefix: 1 block"),
            "stable prefix count: {text}"
        );
        assert!(
            text.contains("Volatile working set: 1 block"),
            "working set section: {text}"
        );
        assert!(
            text.contains("[cache-friendly]"),
            "cache hint for stable: {text}"
        );
        assert!(
            text.contains("[changes by session/turn]"),
            "volatile marker: {text}"
        );
        assert!(
            text.contains("First line: ## Repo Working Set"),
            "first line of working set: {text}"
        );
    }

    #[test]
    fn inspector_blocks_without_working_set_shows_stable_only() {
        let mut app = test_app();
        use codewhale_models::SystemBlock;
        app.system_prompt = Some(SystemPrompt::Blocks(vec![
            SystemBlock {
                block_type: "text".to_string(),
                text: "## Stable Base".to_string(),
                cache_control: None,
            },
            SystemBlock {
                block_type: "text".to_string(),
                text: "## Personality\nCalm".to_string(),
                cache_control: None,
            },
        ]));

        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("Stable prefix: 2 block(s)"));
        assert!(text.contains("Volatile working set: none"));
    }

    #[test]
    fn inspector_text_prompt_shows_layer_map() {
        let mut app = test_app();
        app.system_prompt = Some(SystemPrompt::Text(
            "## Codewhale\nBundled base law.\n\n## Language\nUse English.\n\n## Output Formatting\nBe clear.\n\n<codewhale_repo_constitution>\nRepo law\n</codewhale_repo_constitution>\n\n<project_instructions source=\"AGENTS.md\">\nRules\n</project_instructions>\n\n## Project Context Pack\n{}\n\n## Environment\n- lang: en\n\n## Skills\n- rust\n\n## Core Execution\nInspect, edit, verify.\n\n## Compact\nTemplate\n\n## Repo Working Set\nsrc/".to_string(),
        ));

        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("System Prompt Structure"));
        assert!(text.contains("Text prompt layers"));
        assert!(text.contains("Bundled constitution"));
        assert!(text.contains("Language policy"));
        assert!(text.contains("Output formatting"));
        assert!(text.contains("Repository constitution"));
        assert!(text.contains("Project context"));
        assert!(text.contains("Project context pack"));
        assert!(text.contains("Environment"));
        assert!(text.contains("Skills"));
        assert!(text.contains("Core execution"));
        assert!(text.contains("Compact template"));
        assert!(text.contains("Volatile working set"));
        assert!(text.contains("changes by session/turn"));
    }

    #[test]
    fn inspector_text_prompt_without_markers_shows_single_blob() {
        let mut app = test_app();
        app.system_prompt = Some(SystemPrompt::Text("You are CodeWhale.".to_string()));

        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("Single text blob"));
        assert!(text.contains("stable prefix only"));
    }

    #[test]
    fn inspector_localizes_to_zh_hans() {
        use codewhale_models::SystemBlock;
        let mut app = test_app();
        app.system_prompt = Some(SystemPrompt::Blocks(vec![
            SystemBlock {
                block_type: "text".to_string(),
                text: "## Base\nYou are CodeWhale.".to_string(),
                cache_control: None,
            },
            SystemBlock {
                block_type: "text".to_string(),
                text: format!("{WORKING_SET_MARKER}\nsrc/main.rs changed"),
                cache_control: None,
            },
        ]));
        let text = build_context_inspector_text(&app, Locale::ZhHans);

        // Positive: key ZhHans labels present
        assert!(text.contains("会话上下文"), "session header: {text}");
        assert!(text.contains("模型"), "model label: {text}");
        assert!(text.contains("工作区"), "workspace: {text}");
        assert!(text.contains("系统提示结构"), "sysprompt section: {text}");
        assert!(text.contains("稳定前缀"), "stable prefix: {text}");
        assert!(text.contains("易变工作集"), "volatile ws: {text}");
        assert!(text.contains("第一行"), "first line: {text}");
        assert!(text.contains("总计"), "total line: {text}");
        assert!(text.contains("引用"), "references: {text}");
        assert!(text.contains("最近使用的工具"), "tools: {text}");
        assert!(text.contains("个区块"), "blocks unit: {text}");
        assert!(text.contains("个 token"), "tokens unit: {text}");
        assert!(text.contains("缓存友好"), "cache-friendly: {text}");
        assert!(text.contains("提示"), "cache tip: {text}");

        // Negative: no English labels leak
        assert!(!text.contains("Session Context"), "EN session leaked");
        assert!(!text.contains("Model:"), "EN model leaked");
        assert!(!text.contains("cells"), "EN cells leaked");
        assert!(!text.contains("API messages"), "EN API msgs leaked");
        assert!(!text.contains("Stable prefix"), "EN stable prefix leaked");
        assert!(
            !text.contains("Volatile working set"),
            "EN volatile ws leaked"
        );
        assert!(!text.contains("First line"), "EN first line leaked");
        assert!(!text.contains("Total:"), "EN total leaked");
        assert!(!text.contains("Text prompt layers"), "EN layers leaked");
        assert!(!text.contains("cache-friendly"), "EN cache-friendly leaked");
        assert!(!text.contains("more reference"), "EN more refs leaked");
        assert!(!text.contains("no output yet"), "EN no output leaked");
        assert!(text.contains("压缩"), "compaction row: {text}");
        assert!(text.contains("锚点"), "anchors row: {text}");
    }

    #[test]
    fn inspector_meter_matches_compaction_pressure_signal() {
        let mut app = test_app();
        app.api_messages_mut().push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "x".repeat(4_000),
                cache_control: None,
            }],
        });
        app.last_billed_input_tokens = Some(12_000);
        let estimated =
            estimate_input_tokens_for_pressure(&app.api_messages, app.system_prompt.as_ref());
        let (used, _, _) = context_usage(&app);
        assert_eq!(used, estimated.max(12_000));
        app.last_billed_input_tokens = None;
        let (estimated_only, _, _) = context_usage(&app);
        assert_eq!(estimated_only, estimated);
        assert_ne!(
            estimated_only,
            crate::compaction::estimate_input_tokens_conservative(
                &app.api_messages,
                app.system_prompt.as_ref()
            )
        );
    }

    #[test]
    fn inspector_rows_name_compaction_and_anchors() {
        let mut app = test_app();
        app.last_compaction = Some(crate::compaction::LastCompactionSnapshot {
            auto: true,
            coverage: crate::compaction::CompactionCoverage {
                path: crate::compaction::CompactionPath::Summary,
                last_round_messages: 4,
                last_round_tool_results: 1,
                last_round_assistant: true,
                dropped_messages: 12,
                anchors_chars: 0,
                retained_user_message_tokens: 20_000,
                operator_instructions_applied: false,
            },
            messages_before: 16,
            messages_after: 4,
        });
        let text = build_context_inspector_text(&app, Locale::En);
        assert!(text.contains("making room"), "{text}");
        assert!(text.contains("16 → 4 messages"), "{text}");
        let view = ContextInspectorView::new(&app);
        assert!(view.row_labels().iter().any(|label| label == "making room"));
        assert!(view.row_labels().iter().any(|label| label == "anchors"));
    }
}
