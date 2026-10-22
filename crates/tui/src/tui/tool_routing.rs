//! Active tool-card routing helpers for the TUI loop.

use std::path::PathBuf;
use std::time::Instant;

use crate::hooks::HookEvent;
use crate::tools::ReviewOutput;
use crate::tools::apply_patch::{NormalizedApplyPatchInput, normalize_apply_patch_input};
use crate::tools::canonical_action::canonical_action_alias;
use crate::tools::plan::PlanSnapshot;
use crate::tools::spec::{ToolError, ToolResult};
use crate::tui::active_cell::ActiveCell;
use crate::tui::app::{App, ToolDetailRecord, ToolEvidence};
use crate::tui::history::{
    ExecCell, ExecSource, ExecWait, ExploringCell, ExploringEntry, GenericToolCell, HistoryCell,
    McpToolCell, PatchSummaryCell, PlanUpdateCell, ReviewCell, ToolCell, ToolStatus, ViewImageCell,
    WebSearchCell, output_looks_like_diff, summarize_mcp_output, summarize_tool_args,
    summarize_tool_output,
};
use crate::tui::workspace_context;

#[allow(clippy::too_many_lines)]
pub(super) fn handle_tool_call_started(
    app: &mut App,
    id: &str,
    name: &str,
    input: &serde_json::Value,
) {
    // #2511: ToolCallBefore gate moved to turn-loop planning loop
    // (Engine::run_turn). Removing observer-only firing
    // here to avoid double-firing hooks for each tool call.
    // Hooks that need observation can configure ToolCallBefore on
    // the turn-loop gate — it processes the denial (exit code 2).

    let id = id.to_string();
    let semantic_name = canonical_action_alias(name, input);

    // All in-flight tool work for the current turn lives in `app.active_cell`
    // until the turn completes. This mirrors Codex's contract: ONE active cell
    // mutates in place; finalized history isn't touched until flush. This
    // keeps the transcript stable while parallel completions arrive in any
    // order.
    if app.active_cell.is_none() {
        app.active_cell = Some(ActiveCell::new());
    }

    if is_exploring_tool(semantic_name) {
        let label = exploring_label(semantic_name, input, &app.workspace);
        push_active_tool_cell(
            app,
            &id,
            name,
            input,
            HistoryCell::Tool(ToolCell::Exploring(ExploringCell {
                entries: vec![ExploringEntry {
                    label,
                    status: ToolStatus::Running,
                    error: None,
                }],
            })),
        );
        let virtual_index = app.tool_cells[&id];
        app.exploring_entries.insert(id, (virtual_index, 0));
        return;
    }

    if is_exec_tool(semantic_name) {
        let command = exec_target_from_input(input);
        let source = exec_source_from_input(input);
        let interaction = exec_interaction_summary(semantic_name, input);
        let mut is_wait = false;

        if let Some((summary, wait)) = interaction.as_ref() {
            is_wait = *wait;
            if is_wait
                && app
                    .last_exec_wait_command
                    .as_ref()
                    .is_some_and(|last| last == &command)
            {
                app.ignored_tool_calls.insert(id);
                return;
            }
            if is_wait {
                app.last_exec_wait_command = Some(command.clone());
            }

            push_active_tool_cell(
                app,
                &id,
                name,
                input,
                HistoryCell::Tool(ToolCell::Exec(ExecCell {
                    command,
                    status: ToolStatus::Running,
                    output: None,
                    live_output: None,
                    shell_task_id: None,
                    owner_agent_id: None,
                    owner_agent_name: None,
                    started_at: None,
                    duration_ms: None,
                    stale_elapsed_since_output_ms: None,
                    waiting: None,
                    source,
                    description: None,
                    interaction: Some(summary.clone()),
                    output_summary: None,
                })),
            );
            return;
        }

        if exec_is_background(input)
            && app
                .last_exec_wait_command
                .as_ref()
                .is_some_and(|last| last == &command)
        {
            app.ignored_tool_calls.insert(id);
            return;
        }
        if exec_is_background(input) && !is_wait {
            app.last_exec_wait_command = Some(command.clone());
        }

        push_active_tool_cell(
            app,
            &id,
            name,
            input,
            HistoryCell::Tool(ToolCell::Exec(ExecCell {
                command,
                status: ToolStatus::Running,
                output: None,
                live_output: None,
                shell_task_id: None,
                owner_agent_id: None,
                owner_agent_name: None,
                started_at: None,
                duration_ms: None,
                stale_elapsed_since_output_ms: None,
                waiting: None,
                source,
                description: input
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                interaction: None,
                output_summary: None,
            })),
        );
        return;
    }

    if semantic_name == "update_plan" {
        let snapshot = parse_plan_input(input);
        push_active_tool_cell(
            app,
            &id,
            name,
            input,
            HistoryCell::Tool(ToolCell::PlanUpdate(PlanUpdateCell {
                snapshot,
                status: ToolStatus::Running,
            })),
        );
        return;
    }

    if matches!(semantic_name, "write_file" | "edit_file" | "apply_patch") {
        let (path, summary) = parse_file_mutation_summary(semantic_name, input);
        push_active_tool_cell(
            app,
            &id,
            name,
            input,
            HistoryCell::Tool(ToolCell::PatchSummary(PatchSummaryCell {
                path,
                summary,
                status: ToolStatus::Running,
                error: None,
                receipt: None,
            })),
        );
        return;
    }

    if semantic_name == "review" {
        let target = review_target_label(input);
        push_active_tool_cell(
            app,
            &id,
            name,
            input,
            HistoryCell::Tool(ToolCell::Review(ReviewCell {
                target,
                status: ToolStatus::Running,
                output: None,
                error: None,
            })),
        );
        return;
    }

    if is_mcp_tool(semantic_name) {
        push_active_tool_cell(
            app,
            &id,
            name,
            input,
            HistoryCell::Tool(ToolCell::Mcp(McpToolCell {
                tool: name.to_string(),
                status: ToolStatus::Running,
                content: None,
                is_image: false,
            })),
        );
        return;
    }

    if is_view_image_tool(semantic_name) {
        if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
            let raw_path = PathBuf::from(path);
            let display_path = raw_path
                .strip_prefix(&app.workspace)
                .unwrap_or(&raw_path)
                .to_path_buf();
            push_active_tool_cell(
                app,
                &id,
                name,
                input,
                HistoryCell::Tool(ToolCell::ViewImage(ViewImageCell { path: display_path })),
            );
        }
        return;
    }

    if is_web_search_tool(semantic_name) {
        let query = web_search_query(input);
        push_active_tool_cell(
            app,
            &id,
            name,
            input,
            HistoryCell::Tool(ToolCell::WebSearch(WebSearchCell {
                query,
                status: ToolStatus::Running,
                summary: None,
                source: None,
                degraded: None,
                ref_count: 0,
            })),
        );
        return;
    }

    let mut input_summary = summarize_tool_args(input);
    // Lead the `agent` args summary with the non-default action so renderers
    // can tell inspections (peek/status/wait) apart from spawns without a
    // schema change — a peek must not draw the same "delegate done" line as
    // a launch (#4112, dogfood A5).
    if name == "agent"
        && let Some(action) = input.get("action").and_then(serde_json::Value::as_str)
    {
        let action = action.trim().to_ascii_lowercase();
        let already_leads = input_summary
            .as_deref()
            .is_some_and(|summary| summary.starts_with("action:"));
        if !action.is_empty()
            && !already_leads
            && action != "start"
            && action != "spawn"
            && action != "run"
        {
            input_summary = Some(match input_summary {
                Some(rest) => format!("action: {action} {rest}"),
                None => format!("action: {action}"),
            });
        }
    }
    push_active_tool_cell(
        app,
        &id,
        name,
        input,
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: semantic_name.to_string(),
            status: ToolStatus::Running,
            input_summary,
            output: None,
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })),
    );
}

/// Push a tool cell as a new entry in `active_cell`, register the tool id,
/// and write a stub detail record so the pager / Ctrl+O can find it.
fn push_active_tool_cell(
    app: &mut App,
    tool_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
    cell: HistoryCell,
) {
    if app.active_cell.is_none() {
        app.active_cell = Some(ActiveCell::new());
    }
    let active = app.active_cell.as_mut().expect("active_cell just ensured");
    let entry_idx = active.push_tool(tool_id.to_string(), cell);
    app.active_tool_entry_completed_at.remove(&entry_idx);
    let virtual_index = app.history.len() + entry_idx;
    register_tool_cell(app, tool_id, tool_name, input, virtual_index);
    app.mark_history_updated();
}

pub(crate) fn hold_exec_cards_for_approval(app: &mut App, tool_id: &str) {
    if !crate::tools::subagent::SubAgentManager::is_child_approval_id(tool_id) {
        let queued: Vec<usize> = app
            .tool_cells
            .iter()
            .filter(|(id, _)| id.as_str() != tool_id)
            .map(|(_, &index)| index)
            .filter(|&index| {
                matches!(
                    app.cell_at_virtual_index(index),
                    Some(HistoryCell::Tool(ToolCell::Exec(exec)))
                        if exec.status == ToolStatus::Running
                            && exec.started_at.is_none()
                            && exec.waiting.is_none()
                )
            })
            .collect();
        for index in queued {
            set_exec_card_waiting(app, index, Some(ExecWait::Queue));
        }
    }
    if let Some(&index) = app.tool_cells.get(tool_id) {
        set_exec_card_waiting(app, index, Some(ExecWait::Approval));
    }
}

pub(crate) fn start_exec_card(app: &mut App, tool_id: &str) {
    if let Some(&index) = app.tool_cells.get(tool_id) {
        set_exec_card_waiting(app, index, None);
    }
}

fn set_exec_card_waiting(app: &mut App, index: usize, waiting: Option<ExecWait>) {
    let is_change = matches!(
        app.cell_at_virtual_index(index),
        Some(HistoryCell::Tool(ToolCell::Exec(exec)))
            if exec.status == ToolStatus::Running
                && (exec.waiting != waiting || (waiting.is_none() && exec.started_at.is_none()))
    );
    if !is_change {
        return;
    }
    if let Some(HistoryCell::Tool(ToolCell::Exec(exec))) = app.cell_at_virtual_index_mut(index) {
        exec.waiting = waiting;
        exec.started_at = waiting.is_none().then(Instant::now);
        app.needs_redraw = true;
    }
}

fn register_tool_cell(
    app: &mut App,
    tool_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
    cell_index: usize,
) {
    app.tool_cells.insert(tool_id.to_string(), cell_index);
    let record = ToolDetailRecord {
        tool_id: tool_id.to_string(),
        tool_name: tool_name.to_string(),
        input: input.clone(),
        output: None,
    };
    if cell_index < app.history.len() {
        app.tool_details_by_cell.insert(cell_index, record);
    } else {
        // Active-cell entry: keep the detail record in `active_tool_details`
        // until the active cell flushes. `flush_active_cell` migrates these
        // records into `tool_details_by_cell` keyed by the eventual real
        // cell index.
        app.active_tool_details.insert(tool_id.to_string(), record);
    }
    app.bump_details_epoch();
}

/// Per-record ceiling on a retained tool output (#5472 finding 3).
///
/// These strings are kept for the transcript's expand-tool-output view, which
/// shows an excerpt — nothing reads the whole thing. `Bash` already arrives
/// truncated at 30 KB, but tools with no such contract (`rlm`, large file
/// reads, MCP responses) previously stored whatever they returned, for every
/// call, until the 5,000-cell history fold.
const TOOL_DETAIL_OUTPUT_MAX_BYTES: usize = 64 * 1024;

/// Ceiling on retained tool outputs across the whole transcript.
///
/// The history cap is counted in *cells*, so 5,000 cells each holding a large
/// output was bounded only in principle. Past this budget the oldest cells'
/// outputs are released — oldest first, because both other consumers of this
/// map (`context_inspector`, `file_picker_relevance`) already read only the
/// most recent records, and the expand view degrades to "not retained" rather
/// than lying about the content.
const TOOL_DETAIL_TOTAL_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Truncate to a whole-character boundary, naming what was dropped.
fn bounded_tool_detail_output(mut text: String) -> String {
    if text.len() <= TOOL_DETAIL_OUTPUT_MAX_BYTES {
        return text;
    }
    let original = text.len();
    let mut end = TOOL_DETAIL_OUTPUT_MAX_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(&format!(
        "\n\n[Tool output retained up to {TOOL_DETAIL_OUTPUT_MAX_BYTES} bytes of {original}; \
         the transcript keeps an excerpt, not the whole result.]"
    ));
    text
}

fn store_tool_detail_output(
    app: &mut App,
    tool_id: &str,
    cell_index: usize,
    result: &Result<ToolResult, ToolError>,
) {
    let payload = bounded_tool_detail_output(match result {
        Ok(tool_result) => tool_result.content.clone(),
        Err(err) => err.to_string(),
    });
    if cell_index < app.history.len()
        && let Some(detail) = app.tool_details_by_cell.get_mut(&cell_index)
    {
        detail.output = Some(payload.clone());
    }
    // Also write to the active table while the entry might still live there;
    // some callsites pre-rewrite cell_index but the active_tool_details map is
    // the canonical source for in-flight outputs.
    if let Some(detail) = app.active_tool_details.get_mut(tool_id) {
        detail.output = Some(payload);
    }
    release_oldest_tool_detail_outputs(app);
    app.bump_details_epoch();
}

/// Hold the retained-output total under [`TOOL_DETAIL_TOTAL_BUDGET_BYTES`] by
/// dropping the oldest cells' outputs. The records themselves stay, so the
/// inspector still lists the call and its input.
pub(super) fn release_oldest_tool_detail_outputs(app: &mut App) {
    let mut total = 0usize;
    for detail in app.tool_details_by_cell.values() {
        total = total.saturating_add(detail.output.as_ref().map_or(0, String::len));
    }
    if total <= TOOL_DETAIL_TOTAL_BUDGET_BYTES {
        return;
    }
    let mut oldest_first: Vec<usize> = app
        .tool_details_by_cell
        .iter()
        .filter(|(_, detail)| detail.output.is_some())
        .map(|(index, _)| *index)
        .collect();
    oldest_first.sort_unstable();
    for index in oldest_first {
        if total <= TOOL_DETAIL_TOTAL_BUDGET_BYTES {
            break;
        }
        if let Some(detail) = app.tool_details_by_cell.get_mut(&index) {
            let freed = detail.output.as_ref().map_or(0, String::len);
            detail.output = None;
            total = total.saturating_sub(freed);
        }
    }
}

#[allow(clippy::too_many_lines)]
/// Inspect a tool's success metadata for the `child_*` token-usage
/// fields that tools spawning their own LLM calls populate (e.g.
/// `rlm`). Roll any reported child-token cost into the session's
/// running sub-agent cost counter so the footer total reflects all
/// tokens the user is actually billed for, not just the parent turn's
/// tokens.
///
/// Without this hook, an RLM-heavy session shows a fraction of the
/// real spend because the parent turn's `Usage` only counts the
/// orchestrator's tokens, not the dozens of `deepseek-v4-flash` child
/// rounds RLM fans out under the hood (#524).
fn accrue_child_token_cost_if_any(app: &mut App, result: &Result<ToolResult, ToolError>) {
    let Ok(tool_result) = result else { return };
    let Some(metadata) = tool_result.metadata.as_ref() else {
        return;
    };
    if let Some(batch) = crate::cost_status::child_usage_records_from_metadata(metadata) {
        for record in &batch.records {
            accrue_child_route_usage(app, &record.usage);
        }
        for record in &batch.drop_records {
            let pending = crate::cost_status::background_cost_for_runtime_drop(record);
            app.absorb_pending_background_cost(&pending);
        }
        let residual_dropped_records = batch
            .dropped_records
            .saturating_sub(u64::try_from(batch.drop_records.len()).unwrap_or(u64::MAX));
        if residual_dropped_records > 0 {
            let dropped = u32::try_from(residual_dropped_records).unwrap_or(u32::MAX);
            app.session.cost_unpriced_turns =
                app.session.cost_unpriced_turns.saturating_add(dropped);
            app.session.cost_cny_unpriced_turns =
                app.session.cost_cny_unpriced_turns.saturating_add(dropped);
            app.session
                .cost_unpriced_reasons
                .insert("routed_usage_receipt_missing".to_string());
            app.session
                .cost_cny_unpriced_reasons
                .insert("routed_usage_receipt_missing".to_string());
        }
        return;
    }
    let Some(route) = crate::cost_status::child_route_envelope_from_metadata(metadata) else {
        return;
    };
    // Use the same parser as the runtime host. It deliberately returns a
    // zero-valued usage record when the producer emitted the canonical child
    // fields: a model-backed call is still an auditable/priced-zero call, and
    // replay/server-tool telemetry must not disappear in the TUI projection.
    let Some(usage) = crate::cost_status::child_usage_from_metadata(metadata) else {
        return;
    };
    accrue_child_route_usage(
        app,
        &crate::cost_status::EffectiveRouteUsage { route, usage },
    );
}

fn accrue_child_route_usage(app: &mut App, routed: &crate::cost_status::EffectiveRouteUsage) {
    // `route` is the child's own dispatch receipt, rehydrated from the
    // complete `child_*` metadata `attach_child_usage_metadata` emits at the
    // child's wire boundary (review/verify/rlm are the three producers). An
    // incomplete or legacy payload rehydrates as `RouteBillingMode::Unknown`,
    // so a child never inherits the live `app.billing_presentation` chip and a
    // `/provider` switch between dispatch and arrival cannot retro-bill it.
    //
    // Sub-agent spend lands in the same displayed total as parent turns, so it
    // has to feed the same completeness counters — otherwise `/cost` would call
    // a total complete while an unpriced child turn is missing from it.
    let audit = routed.route.audit(&routed.usage);
    app.record_turn_cost_audit(&audit);
    app.record_turn_cost_route_receipt(routed.route.receipt(&audit));
    if let Some(cost) = audit.estimate {
        app.accrue_subagent_cost_estimate(cost);
    }
}

fn record_spillover_artifact_if_any(
    app: &mut App,
    id: &str,
    name: &str,
    result: &Result<ToolResult, ToolError>,
) {
    let Ok(tool_result) = result else { return };
    let Some(path) = tool_result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("spillover_path"))
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
    else {
        return;
    };
    let metadata = tool_result.metadata.as_ref();
    let session_id = metadata
        .and_then(|metadata| metadata.get("artifact_session_id"))
        .and_then(serde_json::Value::as_str)
        .or(app.current_session_id.as_deref())
        .unwrap_or("");
    let storage_path = metadata
        .and_then(|metadata| metadata.get("artifact_relative_path"))
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| path.clone());
    let content_for_preview = metadata
        .and_then(|metadata| metadata.get("artifact_preview"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&tool_result.content);
    let byte_size = metadata
        .and_then(|metadata| metadata.get("artifact_byte_size"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| {
            std::fs::metadata(&storage_path)
                .map(|metadata| metadata.len())
                .unwrap_or(tool_result.content.len() as u64)
        });
    if app
        .session_artifacts
        .iter()
        .any(|artifact| artifact.tool_call_id == id && artifact.storage_path == storage_path)
    {
        return;
    }
    app.session_artifacts
        .push(crate::artifacts::record_tool_output_artifact_with_size(
            session_id,
            id,
            name,
            storage_path,
            byte_size,
            content_for_preview,
        ));
}

pub(super) fn evidence_completion_should_be_ignored(
    app: &App,
    id: &str,
    result: &Result<ToolResult, ToolError>,
) -> bool {
    evidence_completion_identity_should_be_ignored(
        app.current_session_id.as_deref(),
        app.session_artifacts
            .iter()
            .map(|artifact| (artifact.id.as_str(), artifact.tool_call_id.as_str())),
        id,
        result,
    )
}

fn evidence_completion_identity_should_be_ignored<'a>(
    current_session: Option<&str>,
    known_artifacts: impl IntoIterator<Item = (&'a str, &'a str)>,
    id: &str,
    result: &Result<ToolResult, ToolError>,
) -> bool {
    let Some(metadata) = result
        .as_ref()
        .ok()
        .and_then(|result| result.metadata.as_ref())
    else {
        return false;
    };
    let origin = metadata
        .get("artifact_session_id")
        .and_then(serde_json::Value::as_str);
    if let (Some(origin), Some(current)) = (origin, current_session)
        && origin != current
    {
        return true;
    }
    metadata
        .get("artifact_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|artifact_id| {
            known_artifacts
                .into_iter()
                .any(|(known_id, known_call)| known_id == artifact_id && known_call == id)
        })
}

/// #3031: shell/tasks tools embed the literal `"(no output)"` into successful
/// `ToolResult` content (the model-facing transcript needs a non-empty tool
/// result). Treat it as no output on the TUI side so the compact-mode
/// suppression gate in `history.rs` actually fires; the raw content remains
/// available through the tool-detail store.
fn visible_tool_output(content: &str) -> Option<String> {
    if content.trim() == "(no output)" {
        None
    } else {
        Some(content.to_string())
    }
}

/// Fire `tool_call_after` for every settled tool call, plus `on_error` when
/// the call failed.
///
/// `on_error` is documented as covering tool failures, not just transport and
/// auth failures, so the tool path has to raise it too — the engine-error path
/// in `apply_engine_error_to_app` never sees a tool that returned
/// `success: false`.
///
/// Both are observer events: their stdout is ignored and neither can change
/// the result that goes back to the model. That is a statement about
/// Codewhale's control flow only — the commands themselves are arbitrary
/// shells and may have any external side effect.
fn fire_tool_completion_hooks(
    app: &mut App,
    id: &str,
    name: &str,
    result: &Result<ToolResult, ToolError>,
) {
    let wants_after = app.hooks.has_hooks_for_event(HookEvent::ToolCallAfter);
    let wants_error = app
        .hooks
        .has_hooks_for_event(crate::hooks::HookEvent::OnError);
    if !wants_after && !wants_error {
        // Fast path: skip the result clone and HookContext allocation when
        // the user has configured neither event.
        return;
    }

    let input = app
        .active_tool_details
        .get(id)
        .or_else(|| {
            app.tool_cells
                .get(id)
                .and_then(|index| app.tool_details_by_cell.get(index))
        })
        .map(|detail| detail.input.clone());
    let context = app
        .base_hook_context()
        .with_tool_name(name)
        .with_tool_call_id(id)
        .with_tool_outcome(result);
    let context = input.as_ref().map_or_else(
        || context.clone(),
        |input| context.clone().with_tool_args(input),
    );
    let failed = context.tool_success == Some(false);
    let error_context = (wants_error && failed).then(|| {
        let text = context.tool_result.as_deref().unwrap_or_default();
        let message = format!("tool `{name}` failed: {text}");
        context.clone().with_error(&message)
    });

    if wants_after && let Err(error) = app.submit_hooks(HookEvent::ToolCallAfter, context) {
        app.surface_observer_hook_submission_failure(error);
    }

    if let Some(context) = error_context
        && let Err(error) = app.submit_hooks(crate::hooks::HookEvent::OnError, context)
    {
        app.surface_observer_hook_submission_failure(error);
    }
}

pub(super) fn handle_tool_call_complete(
    app: &mut App,
    id: &str,
    name: &str,
    result: &Result<ToolResult, ToolError>,
) {
    if app.ignored_tool_calls.remove(id) {
        // "Ignored" is a *presentation* decision: these are real settled
        // results — repeated `wait` polls, background-shell status reads —
        // that the transcript deliberately does not redraw. Observers still
        // have to see them, or `tool_call_after` silently skips a whole class
        // of completions while claiming to fire after each tool call. Fired
        // here and returned immediately, so each id emits exactly once.
        fire_tool_completion_hooks(app, id, name, result);
        return;
    }
    // Preserve the execution/audit name while recovering the action-qualified
    // semantic name from the registered call input. Active entries and
    // already-flushed history use separate detail stores.
    let detail = app.active_tool_details.get(id).or_else(|| {
        app.tool_cells
            .get(id)
            .and_then(|cell_index| app.tool_details_by_cell.get(cell_index))
    });
    let semantic_name = detail
        .map_or(name, |detail| canonical_action_alias(name, &detail.input))
        .to_string();
    let explicit_background = detail.is_some_and(|detail| {
        exec_is_background(&detail.input)
            || detail.input.get("tty").and_then(serde_json::Value::as_bool) == Some(true)
    });

    // Roll any child-LLM token usage the tool reports into the
    // session-cost counter. Runs unconditionally so future tools that
    // spawn their own LLM calls (RLM, summarizers, retrieval helpers)
    // get accrued without needing a per-tool hook (#524).
    accrue_child_token_cost_if_any(app, result);
    record_spillover_artifact_if_any(app, id, name, result);

    // #455: fire `tool_call_after` (and `on_error` for failures) here, before
    // any of the presentation early-returns below. Firing it further down meant
    // exploring-tool completions and orphaned completions never emitted the
    // event at all, so "fires after each tool call" was not true.
    fire_tool_completion_hooks(app, id, name, result);

    // The engine prefixes an approved call's result with a note for the model
    // ("[approval] This tool call required approval…"). The person gave that
    // approval a moment ago; repeating it as the first line of the output
    // reads as an internal log (#6566). The model's copy keeps the note. The
    // engine's approval stamp decides what is a note, never the text alone.
    let displayed;
    let result = match result {
        Ok(tool_result) => {
            let shown_content = crate::core::engine::content_without_approval_note(tool_result);
            if shown_content.len() == tool_result.content.len() {
                result
            } else {
                let mut shown = tool_result.clone();
                shown.content = shown_content.to_string();
                displayed = Ok(shown);
                &displayed
            }
        }
        Err(_) => result,
    };

    // Exploring entries land in the per-tool map regardless of whether they
    // live in the active cell or in finalized history; the path is the same.
    if let Some((cell_index, entry_index)) = app.exploring_entries.remove(id) {
        app.tool_cells.remove(id);
        app.bump_details_epoch();
        store_tool_detail_output(app, id, cell_index, result);
        if let Some(HistoryCell::Tool(ToolCell::Exploring(cell))) =
            app.cell_at_virtual_index_mut(cell_index)
            && let Some(entry) = cell.entries.get_mut(entry_index)
        {
            entry.status = tool_status_from_result(result);
            entry.error = if entry.status == ToolStatus::Failed {
                match result {
                    Ok(tool_result) => visible_tool_output(&tool_result.content),
                    Err(err) => Some(err.to_string()),
                }
            } else {
                None
            };
            app.mark_history_updated();
            // Mutating the in-flight exploring cell needs an active-cell
            // revision bump so the transcript cache invalidates the synthetic
            // tail row.
            if cell_index >= app.history.len() {
                app.active_cell_revision = app.active_cell_revision.wrapping_add(1);
                if let Some(active) = app.active_cell.as_mut() {
                    active.bump_revision();
                }
            }
        }
        refresh_active_tool_completion_timestamp(app, cell_index);
        return;
    }

    // Look up the cell by tool id. If the id isn't registered, that's an
    // orphan completion (race condition where the started event was lost or
    // a tool result arrived after the active cell was already flushed). Build
    // a finalized standalone cell from the result so the user can still see
    // the output, but DO NOT touch the active cell.
    let Some(cell_index) = app.tool_cells.remove(id) else {
        push_orphan_tool_completion(app, id, name, result);
        return;
    };
    app.bump_details_epoch();

    store_tool_detail_output(app, id, cell_index, result);
    let in_active = cell_index >= app.history.len();

    let status = tool_status_from_result(result);
    let mutation_receipt = matches!(
        semantic_name.as_str(),
        "write_file" | "edit_file" | "apply_patch"
    )
    .then(|| {
        result.as_ref().ok().and_then(|tool_result| {
            crate::tui::history::FileMutationReceipt::from_success(&app.workspace, tool_result)
        })
    })
    .flatten();
    let mut workflow_panel_output: Option<String> = None;
    let mut moved_to_jobs = false;

    if let Some(cell) = app.cell_at_virtual_index_mut(cell_index) {
        match cell {
            HistoryCell::Tool(ToolCell::Exec(exec)) => {
                exec.status = status;
                if let Ok(tool_result) = result.as_ref() {
                    let shell_task_id = tool_result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("task_id"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|task_id| !task_id.trim().is_empty())
                        .map(str::to_string);
                    if shell_task_id.is_some() {
                        exec.shell_task_id = shell_task_id;
                    }
                    exec.owner_agent_id = tool_result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("owner_agent_id"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|agent_id| !agent_id.trim().is_empty())
                        .map(str::to_string);
                    exec.owner_agent_name = tool_result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("owner_agent_name"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|agent_name| !agent_name.trim().is_empty())
                        .map(str::to_string);
                    if let Some(meta_command) = tool_result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("command"))
                        .and_then(serde_json::Value::as_str)
                        && !meta_command.trim().is_empty()
                        && (exec.command == "command" || exec.command.starts_with("command "))
                    {
                        exec.command = meta_command.to_string();
                        if exec.interaction.as_deref().is_some_and(|interaction| {
                            interaction.starts_with("Waiting for command")
                        }) {
                            let task_suffix = tool_result
                                .metadata
                                .as_ref()
                                .and_then(|m| m.get("task_id"))
                                .and_then(serde_json::Value::as_str)
                                .map(|task_id| format!(" ({task_id})"))
                                .unwrap_or_default();
                            exec.interaction =
                                Some(format!("Waiting for \"{meta_command}\"{task_suffix}"));
                        }
                    }
                    exec.duration_ms = tool_result
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("duration_ms"))
                        .and_then(serde_json::Value::as_u64);
                    if status != ToolStatus::Running && exec.interaction.is_none() {
                        exec.output = visible_tool_output(&tool_result.content);
                        exec.output_summary = exec
                            .output
                            .as_deref()
                            .map(super::history::summarize_tool_output);
                        exec.live_output = None;
                    } else if status == ToolStatus::Running && exec.interaction.is_none() {
                        moved_to_jobs = !explicit_background;
                    }
                } else if let Err(err) = result.as_ref()
                    && exec.interaction.is_none()
                {
                    exec.output = Some(err.to_string());
                    exec.output_summary =
                        Some(super::history::summarize_tool_output(&err.to_string()));
                }
                app.mark_history_updated();
            }
            HistoryCell::Tool(ToolCell::PlanUpdate(plan)) => {
                plan.status = status;
                app.mark_history_updated();
            }
            HistoryCell::Tool(ToolCell::PatchSummary(patch)) => {
                patch.status = status;
                patch.receipt = mutation_receipt;
                match result.as_ref() {
                    Ok(tool_result) if tool_result.success => {
                        if let Ok(json) =
                            serde_json::from_str::<serde_json::Value>(&tool_result.content)
                            && let Some(message) = json.get("message").and_then(|v| v.as_str())
                        {
                            patch.summary = message.to_string();
                        }
                    }
                    Ok(tool_result) => {
                        patch.error = Some(tool_result.content.clone());
                    }
                    Err(err) => {
                        patch.error = Some(err.to_string());
                    }
                }
                app.mark_history_updated();
            }
            HistoryCell::Tool(ToolCell::Review(review)) => {
                review.status = status;
                match result.as_ref() {
                    Ok(tool_result) => {
                        if tool_result.success {
                            review.output = Some(ReviewOutput::from_str(&tool_result.content));
                        } else {
                            review.error = Some(tool_result.content.clone());
                        }
                    }
                    Err(err) => {
                        review.error = Some(err.to_string());
                    }
                }
                app.mark_history_updated();
            }
            HistoryCell::Tool(ToolCell::Mcp(mcp)) => {
                match result.as_ref() {
                    Ok(tool_result) => {
                        let summary = summarize_mcp_output(&tool_result.content);
                        if status == ToolStatus::Hydrated {
                            mcp.status = status;
                        } else if summary.is_error == Some(true) {
                            mcp.status = ToolStatus::Failed;
                        } else {
                            mcp.status = status;
                        }
                        mcp.is_image = summary.is_image;
                        mcp.content = summary.content;
                    }
                    Err(err) => {
                        mcp.status = status;
                        mcp.content = Some(err.to_string());
                    }
                }
                app.mark_history_updated();
            }
            HistoryCell::Tool(ToolCell::WebSearch(search)) => {
                search.status = status;
                match result.as_ref() {
                    Ok(tool_result) => {
                        search.summary = Some(summarize_tool_output(&tool_result.content));
                        let presentation = web_search_presentation(&tool_result.content);
                        search.source = presentation.source;
                        search.degraded = presentation.degraded;
                        search.ref_count = presentation.ref_count;
                    }
                    Err(err) => {
                        search.summary = Some(err.to_string());
                    }
                }
                app.mark_history_updated();
            }
            HistoryCell::Tool(ToolCell::Generic(generic)) => {
                generic.status = status;
                match result.as_ref() {
                    Ok(tool_result) => {
                        generic.output = visible_tool_output(&tool_result.content);
                        generic.output_summary =
                            generic.output.as_deref().map(summarize_tool_output);
                        generic.is_diff = output_looks_like_diff(&tool_result.content);
                    }
                    Err(err) => {
                        generic.output = Some(err.to_string());
                        generic.output_summary = Some(summarize_tool_output(&err.to_string()));
                        generic.is_diff = false;
                    }
                }
                // #4121: capture workflow JSON before releasing the cell borrow
                // so we can hydrate the panel without overlapping borrows.
                if generic.name == "workflow" {
                    workflow_panel_output = generic.output.clone();
                }
                app.mark_history_updated();
            }
            _ => {}
        }
    }

    // #4121 / #4122: feed typed workflow events into the panel *and* keep the
    // history card snapshot in sync. Live streaming also arrives via
    // `Event::WorkflowUi`; this path covers tool-complete hydration.
    if let Some(output) = workflow_panel_output.as_deref() {
        apply_workflow_output_to_panel(app, output);
    }
    if moved_to_jobs {
        let notice = app
            .tr(codewhale_localization::MessageId::ShellWaitReleased)
            .replace("{jobs}", "/jobs");
        app.push_status_toast(notice, crate::tui::app::StatusToastLevel::Info, Some(6_000));
    }

    // If the mutated cell lived inside the active group, bump the active-cell
    // revision so the transcript cache re-renders the synthetic tail row.
    if in_active {
        app.active_cell_revision = app.active_cell_revision.wrapping_add(1);
        if let Some(active) = app.active_cell.as_mut() {
            active.bump_revision();
        }
        refresh_active_tool_completion_timestamp(app, cell_index);
    }

    if refreshes_workspace_context_on_completion(&semantic_name) && status != ToolStatus::Running {
        workspace_context::refresh_now(app, Instant::now());
    }

    // Collect evidence for the post-turn receipt.
    let evidence_summary = match result.as_ref() {
        Ok(tool_result) => {
            if tool_result.success {
                summarize_tool_output(&tool_result.content)
            } else {
                format!("failed: {}", summarize_tool_output(&tool_result.content))
            }
        }
        Err(err) => format!("error: {err}"),
    };
    app.tool_evidence.push(ToolEvidence {
        tool_name: name.to_string(),
        summary: evidence_summary,
    });
}

#[derive(Debug, Default, PartialEq, Eq)]
struct WebSearchPresentation {
    source: Option<String>,
    degraded: Option<String>,
    ref_count: usize,
}

fn web_search_presentation(content: &str) -> WebSearchPresentation {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return WebSearchPresentation::default();
    };
    let surfaces = if value.get("receipt").is_some() {
        vec![&value]
    } else {
        value
            .get("search_query")
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    };
    let source = surfaces
        .iter()
        .filter_map(|surface| surface.get("source").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .next();
    let mut degraded = Vec::new();
    let mut ref_count = 0usize;
    for surface in surfaces {
        if let Some(results) = surface.get("results").and_then(serde_json::Value::as_array) {
            ref_count = ref_count.saturating_add(
                results
                    .iter()
                    .filter(|result| {
                        result
                            .get("ref_id")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|ref_id| !ref_id.is_empty())
                    })
                    .count(),
            );
        }
        if let Some(reasons) = surface
            .pointer("/receipt/degraded")
            .and_then(serde_json::Value::as_array)
        {
            for reason in reasons {
                if let Some(label) = degraded_reason_label(reason)
                    && !degraded.contains(&label)
                {
                    degraded.push(label);
                }
            }
        }
    }
    WebSearchPresentation {
        source,
        degraded: (!degraded.is_empty()).then(|| degraded.join("; ")),
        ref_count,
    }
}

fn degraded_reason_label(reason: &serde_json::Value) -> Option<String> {
    let kind = reason.get("kind")?.as_str()?;
    let backend = |field: &str| {
        reason
            .get(field)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
    };
    Some(match kind {
        "backend_unavailable" => format!("{} unavailable", backend("backend")),
        "no_usable_results" => format!("{} returned no usable results", backend("backend")),
        "backend_fallback" => format!("{} -> {}", backend("from"), backend("to")),
        "challenge_detected" => format!("{} challenge", backend("backend")),
        "scrape_fallback" => format!("{} -> {} scrape", backend("from"), backend("to")),
        "knob_ignored" => format!(
            "{} ignored",
            reason
                .get("knob")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("filter")
        ),
        "post_filtered" => format!(
            "{} post-filtered",
            reason
                .get("knob")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("results")
        ),
        "synthesized_results" => "synthesized results".to_string(),
        other => other.replace('_', " "),
    })
}

/// Hydrate or advance a workflow run from a workflow tool JSON payload: a
/// run record (with its retained `events` tail) or a status envelope. A
/// status envelope only refreshes runs this view already holds — polling an
/// old run must not bring it back, or its finish line would be written twice.
fn apply_workflow_output_to_panel(app: &mut App, output: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return;
    };
    if value.get("action").and_then(|v| v.as_str()) == Some("status") {
        if let Some(runs) = value.get("runs").and_then(|r| r.as_array()) {
            for run in runs {
                apply_workflow_run_record(app, run, false);
            }
        }
    } else {
        apply_workflow_run_record(app, &value, true);
    }
    app.announce_settled_workflows();
}

fn apply_workflow_run_record(app: &mut App, value: &serde_json::Value, may_create: bool) {
    let Some(run_id) = value
        .get("run_id")
        .and_then(|v| v.as_str())
        .filter(|run_id| !run_id.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("events")
                .and_then(|events| events.as_array())
                .and_then(|events| {
                    events.iter().find_map(|event| {
                        event
                            .get("run_id")
                            .and_then(|v| v.as_str())
                            .filter(|run_id| !run_id.trim().is_empty())
                            .map(str::to_string)
                    })
                })
        })
    else {
        return;
    };
    if app.workflow_run(&run_id).is_none() && !may_create {
        return;
    }
    let label = value
        .get("workflow_goal")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("workflow_id").and_then(|v| v.as_str()))
        .unwrap_or(&run_id)
        .to_string();
    let at_ms = value
        .get("started_at_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Prefer the typed event stream when present.
    if let Some(events) = value.get("events").and_then(|e| e.as_array()) {
        if app.workflow_run(&run_id).is_none() {
            let mut panel = crate::tui::widgets::workflow_panel::WorkflowPanel::new(
                run_id.clone(),
                label,
                at_ms,
            );
            panel.locale = app.ui_locale;
            app.push_workflow_run(panel);
        }
        let Some(panel) = app.workflow_run_mut(&run_id) else {
            return;
        };
        let injected: Vec<serde_json::Value> = events
            .iter()
            .map(|event| {
                let mut event = event.clone();
                if let Some(obj) = event.as_object_mut() {
                    // The top-level run record is authoritative. Do not let a
                    // stale/malformed embedded id retarget one replay event.
                    obj.insert(
                        "run_id".to_string(),
                        serde_json::Value::String(run_id.clone()),
                    );
                }
                event
            })
            .collect();
        // A replayed tail can repeat this run's `run_started`, which resets
        // its state; whether the finish line was written survives that.
        let announced = panel.finish_announced;
        panel.apply_json_events(&injected);
        panel.finish_announced = announced || panel.finish_announced;
        // Completion/status payloads replay a retained event tail. Merge
        // the authoritative exact count + bounded structured ledger after
        // replay so live dispatch failures are neither duplicated nor
        // lost when older events have fallen out of the tail (#5528).
        panel.merge_dispatch_failures_from_run_json(value);
        if let Some(summary) = value
            .get("result")
            .and_then(crate::tools::workflow::workflow_result_preview)
        {
            panel.result_summary = Some(summary);
        }
        if let Some(path) = value.get("source_path").and_then(|v| v.as_str()) {
            panel.source_path = Some(PathBuf::from(path));
        }
        app.needs_redraw = true;
        sync_workflow_history_card(app, &run_id);
        return;
    }

    // A summary/phases snapshot hydrates the whole run.
    if let Some(mut panel) =
        crate::tui::widgets::workflow_panel::WorkflowPanel::from_run_json(value)
    {
        panel.locale = app.ui_locale;
        match app.workflow_run_mut(&run_id) {
            Some(existing) => {
                panel.finish_announced = existing.finish_announced;
                *existing = panel;
            }
            None => app.push_workflow_run(panel),
        }
        app.needs_redraw = true;
        sync_workflow_history_card(app, &run_id);
        return;
    }

    // Fallback: bare run record without events — at least surface its state.
    use crate::tui::widgets::workflow_panel::{WorkflowPanelEvent, WorkflowPanelLifecycle};
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("running");
    if app.workflow_run(&run_id).is_none() {
        app.apply_workflow_panel_event(
            &run_id,
            WorkflowPanelEvent::RunStarted {
                run_id: run_id.clone(),
                workflow_id: value
                    .get("workflow_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                workflow_goal: Some(label),
                source_path: value
                    .get("source_path")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from),
                token_budget: value.get("token_budget").and_then(|v| v.as_u64()),
                at_ms,
            },
        );
    }
    let life = match status {
        "completed" | "succeeded" => WorkflowPanelLifecycle::Succeeded,
        "degraded" => WorkflowPanelLifecycle::Degraded,
        "failed" => WorkflowPanelLifecycle::Failed,
        "cancelled" | "canceled" => WorkflowPanelLifecycle::Cancelled,
        _ => WorkflowPanelLifecycle::Running,
    };
    if life != WorkflowPanelLifecycle::Running
        && app
            .workflow_run(&run_id)
            .is_some_and(|run| !run.lifecycle.is_terminal())
    {
        app.apply_workflow_panel_event(
            &run_id,
            WorkflowPanelEvent::RunCompleted {
                status: life,
                error: value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                at_ms: value
                    .get("completed_at_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(at_ms),
            },
        );
    }
    sync_workflow_history_card(app, &run_id);
}

/// Apply one live `WorkflowUi` engine event to its run.
///
/// Progress is applied to state at once but costs nothing else: only a run's
/// start and end touch the transcript, so a 75-agent fan-out's stream of
/// task and budget events never rescans history or reserializes a card.
pub(super) fn apply_workflow_ui_event(app: &mut App, run_id: &str, event: &serde_json::Value) {
    use crate::tui::widgets::workflow_panel::WorkflowPanelEvent;

    let mut event = event.clone();
    if let Some(obj) = event.as_object_mut() {
        // The engine envelope owns route identity. An embedded stale id must
        // not move this event onto another run's state.
        obj.insert(
            "run_id".to_string(),
            serde_json::Value::String(run_id.to_string()),
        );
    }
    let event_type = event
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    // The engine names what the run returned; keep it for the finish line.
    // Set before the event applies, so the line written on this very event
    // already carries it.
    if event_type == "run_completed"
        && let Some(preview) = event
            .get("result_preview")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        && let Some(run) = app.workflow_run_mut(run_id)
    {
        run.result_summary = Some(preview);
    }
    let Some(panel_event) = WorkflowPanelEvent::from_json_value(&event) else {
        return;
    };
    if !app.apply_workflow_panel_event(run_id, panel_event) {
        return;
    }
    if event_type == "run_started" {
        sync_workflow_history_card(app, run_id);
    }
}

/// Apply a live workflow event only when its immutable owner is the active
/// conversation. This check deliberately sits in the mutation helper so every
/// caller fails closed before touching the run or transcript history.
pub(super) fn apply_owned_workflow_ui_event(
    app: &mut App,
    owner_session_id: &str,
    run_id: &str,
    event: &serde_json::Value,
) -> bool {
    if app.current_session_id.as_deref() != Some(owner_session_id) {
        return false;
    }
    apply_workflow_ui_event(app, run_id, event);
    true
}

/// A foreground `run` card has no output until the tool returns, but its
/// start line names the run. Give the newest still-running `workflow` card —
/// the one this run belongs to, or one that has no run yet — the run's
/// snapshot. A card whose tool has returned keeps its own output.
fn sync_workflow_history_card(app: &mut App, run_id: &str) {
    let Some(snapshot) = app
        .workflow_run(run_id)
        .map(|panel| panel.to_run_json().to_string())
    else {
        return;
    };
    let history_len = app.history.len();
    let total = history_len
        + app
            .active_cell
            .as_ref()
            .map(|a| a.entries().len())
            .unwrap_or(0);
    let mut target = None;
    for idx in (0..total).rev() {
        let Some(HistoryCell::Tool(ToolCell::Generic(generic))) = app.cell_at_virtual_index(idx)
        else {
            continue;
        };
        if generic.name != "workflow" || generic.status != ToolStatus::Running {
            continue;
        }
        let card_run = generic
            .output
            .as_deref()
            .and_then(|out| serde_json::from_str::<serde_json::Value>(out).ok())
            .and_then(|v| {
                v.get("run_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            });
        match card_run.as_deref() {
            Some(id) if id == run_id => {
                target = Some(idx);
                break;
            }
            None if target.is_none() => target = Some(idx),
            _ => {}
        }
    }
    let Some(idx) = target else {
        return;
    };
    if let Some(HistoryCell::Tool(ToolCell::Generic(generic))) = app.cell_at_virtual_index_mut(idx)
    {
        generic.output = Some(snapshot);
        generic.output_summary = Some(format!("workflow {run_id}"));
        app.mark_history_updated();
    }
}

fn refresh_active_tool_completion_timestamp(app: &mut App, cell_index: usize) {
    if cell_index < app.history.len() {
        return;
    }
    let entry_idx = cell_index - app.history.len();
    let Some(cell) = app.cell_at_virtual_index(cell_index) else {
        app.active_tool_entry_completed_at.remove(&entry_idx);
        return;
    };

    if history_cell_has_running_tool(cell) {
        app.active_tool_entry_completed_at.remove(&entry_idx);
    } else {
        app.active_tool_entry_completed_at
            .entry(entry_idx)
            .or_insert_with(Instant::now);
    }
}

fn history_cell_has_running_tool(cell: &HistoryCell) -> bool {
    let HistoryCell::Tool(tool) = cell else {
        return false;
    };
    match tool {
        ToolCell::Exec(exec) => exec.status == ToolStatus::Running,
        ToolCell::Exploring(explore) => explore
            .entries
            .iter()
            .any(|entry| entry.status == ToolStatus::Running),
        ToolCell::PlanUpdate(plan) => plan.status == ToolStatus::Running,
        ToolCell::PatchSummary(patch) => patch.status == ToolStatus::Running,
        ToolCell::Review(review) => review.status == ToolStatus::Running,
        ToolCell::Mcp(mcp) => mcp.status == ToolStatus::Running,
        ToolCell::ViewImage(_) => false,
        ToolCell::WebSearch(search) => search.status == ToolStatus::Running,
        ToolCell::Generic(generic) => generic.status == ToolStatus::Running,
    }
}

/// Build a finalized standalone history cell for a tool completion whose
/// start was never registered (orphan). This preserves the contract that
/// every tool result is visible somewhere; the alternative (silently
/// dropping it) hides errors and breaks debuggability.
///
/// Choice of cell type: success-only mutation metadata is sufficient to
/// reconstruct a structured File receipt; other orphans stay generic because
/// no input payload remains. The pager remains usable in both cases because
/// `tool_details_by_cell` is populated with the result text.
///
/// ## Index drift
///
/// If an active cell is in flight when the orphan arrives, pushing the
/// orphan into `app.history` shifts every active-cell virtual index forward
/// by 1. We must rewrite `tool_cells` / `exploring_entries` accordingly so
/// later completion lookups still find the right entries.
fn push_orphan_tool_completion(
    app: &mut App,
    tool_id: &str,
    name: &str,
    result: &Result<ToolResult, ToolError>,
) {
    let status = tool_status_from_result(result);
    let output = match result.as_ref() {
        Ok(tool_result) => Some(summarize_tool_output(&tool_result.content)),
        Err(err) => Some(err.to_string()),
    };
    let spillover_path = result
        .as_ref()
        .ok()
        .and_then(|r| r.metadata.as_ref())
        .and_then(|m| m.get("spillover_path"))
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from);
    let output_summary = output.as_deref().map(summarize_tool_output);
    let is_diff = output.as_deref().is_some_and(output_looks_like_diff);
    let mutation_receipt = result.as_ref().ok().and_then(|tool_result| {
        crate::tui::history::FileMutationReceipt::from_success(&app.workspace, tool_result)
    });
    let cell = if let Some(receipt) = mutation_receipt {
        let path = receipt
            .files
            .first()
            .map_or_else(|| "<file>".to_string(), |file| file.path.clone());
        let summary = receipt.semantic_summary();
        HistoryCell::Tool(ToolCell::PatchSummary(PatchSummaryCell {
            path,
            summary,
            status,
            error: None,
            receipt: Some(receipt),
        }))
    } else {
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: name.to_string(),
            status,
            input_summary: None,
            output,
            prompts: None,
            spillover_path,
            output_summary,
            is_diff,
        }))
    };
    app.add_message(cell);
    let cell_index = app.history.len().saturating_sub(1);
    app.tool_details_by_cell.insert(
        cell_index,
        ToolDetailRecord {
            tool_id: tool_id.to_string(),
            tool_name: name.to_string(),
            input: serde_json::Value::Null,
            output: match result.as_ref() {
                Ok(tool_result) => Some(tool_result.content.clone()),
                Err(err) => Some(err.to_string()),
            },
        },
    );
    app.bump_details_epoch();

    // The virtual-index rebase this path used to do inline now lives in
    // `App::add_message`, so every mid-turn history insert gets it — not just
    // orphan completions. That gap was #5478: `/rename`'s note shifted the
    // indices with nothing to re-base them.
}

fn tool_status_from_result(result: &Result<ToolResult, ToolError>) -> ToolStatus {
    match result.as_ref() {
        Ok(tool_result) if is_deferred_schema_hydration(tool_result) => ToolStatus::Hydrated,
        Ok(tool_result) => match tool_result
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("status"))
            .and_then(serde_json::Value::as_str)
        {
            Some("Running") => ToolStatus::Running,
            Some("Killed") => ToolStatus::Stopped,
            _ => {
                if tool_result.success {
                    ToolStatus::Success
                } else {
                    ToolStatus::Failed
                }
            }
        },
        Err(_) => ToolStatus::Failed,
    }
}

fn is_deferred_schema_hydration(tool_result: &ToolResult) -> bool {
    if !tool_result.success {
        return false;
    }
    let Some(metadata) = tool_result.metadata.as_ref() else {
        return false;
    };
    metadata
        .get("event")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|event| event == "tool.schema_hydrated")
        && metadata
            .get("executed")
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|executed| !executed)
}

fn is_exploring_tool(name: &str) -> bool {
    matches!(name, "read_file" | "list_dir" | "grep_files" | "list_files")
}

fn is_exec_tool(name: &str) -> bool {
    matches!(
        name,
        "exec_shell"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "exec_shell_cancel"
            | "exec_wait"
            | "exec_interact"
    )
}

pub(super) fn refreshes_workspace_context_on_completion(name: &str) -> bool {
    matches!(
        name,
        "exec_shell"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "exec_shell_cancel"
            | "exec_wait"
            | "exec_interact"
            | "task_shell_start"
            | "task_shell_wait"
            | "write_file"
            | "edit_file"
            | "apply_patch"
    )
}

pub(super) fn exploring_label(
    name: &str,
    input: &serde_json::Value,
    workspace: &std::path::Path,
) -> String {
    let fallback = format!("{name} tool");
    let obj = input.as_object();
    let path = || {
        obj.and_then(|o| o.get("path"))
            .and_then(|v| v.as_str())
            .map(|path| crate::utils::workspace_relative(path, workspace))
    };
    match name {
        "read_file" => path().map_or(fallback, |path| format!("Reading {path}")),
        "list_dir" => path().map_or("Listing directory".to_string(), |path| {
            format!("Listing {path}")
        }),
        "grep_files" => {
            let pattern = obj
                .and_then(|o| o.get("pattern"))
                .and_then(|v| v.as_str())
                .unwrap_or("pattern");
            format!("Searching for `{pattern}`")
        }
        "list_files" => "Listing files".to_string(),
        _ => fallback,
    }
}

fn is_mcp_tool(name: &str) -> bool {
    name.starts_with("mcp_")
}

fn is_view_image_tool(name: &str) -> bool {
    matches!(name, "view_image" | "view_image_file" | "view_image_tool")
}

fn is_web_search_tool(name: &str) -> bool {
    matches!(name, "web_search" | "search_web" | "search" | "web.run")
        || name.ends_with("_web_search")
}

fn web_search_query(input: &serde_json::Value) -> String {
    if let Some(searches) = input.get("search_query").and_then(|v| v.as_array())
        && let Some(first) = searches.first()
        && let Some(q) = first.get("q").and_then(|v| v.as_str())
    {
        return q.to_string();
    }

    input
        .get("query")
        .or_else(|| input.get("q"))
        .or_else(|| input.get("search"))
        .and_then(|v| v.as_str())
        .unwrap_or("Web search")
        .to_string()
}

fn review_target_label(input: &serde_json::Value) -> String {
    let target = input
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("review")
        .trim();
    let kind = input
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let staged = input
        .get("staged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let target_lower = target.to_ascii_lowercase();

    if kind == "diff"
        || target_lower == "diff"
        || target_lower == "git diff"
        || target_lower == "staged"
        || target_lower == "cached"
    {
        if staged || target_lower == "staged" || target_lower == "cached" {
            return "git diff --cached".to_string();
        }
        return "git diff".to_string();
    }

    target.to_string()
}

fn parse_plan_input(input: &serde_json::Value) -> PlanSnapshot {
    PlanSnapshot::from_tool_input(input)
}

fn parse_file_mutation_summary(semantic_name: &str, input: &serde_json::Value) -> (String, String) {
    if semantic_name != "apply_patch" {
        let path = input
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .unwrap_or("<file>")
            .to_string();
        let summary = match semantic_name {
            "write_file" => "Writing file",
            "edit_file" => "Editing file",
            _ => "Changing file",
        }
        .to_string();
        return (path, summary);
    }
    let patch_text = match normalize_apply_patch_input(input) {
        Ok(NormalizedApplyPatchInput::Replacement {
            entries: changes, ..
        }) => {
            let count = changes.len();
            let path = changes
                .first()
                .and_then(|c| c.get("path"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "<file>".to_string());
            let label = if count <= 1 {
                path
            } else {
                format!("{count} files")
            };
            let summary = format!("Changes: {count} file(s)");
            return (label, summary);
        }
        Ok(NormalizedApplyPatchInput::Patch(patch)) => patch,
        Err(_) => "",
    };
    let paths = extract_patch_paths(patch_text);
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            if paths.len() == 1 {
                paths.first().cloned()
            } else if paths.is_empty() {
                None
            } else {
                Some(format!("{} files", paths.len()))
            }
        })
        .unwrap_or_else(|| "<file>".to_string());

    let (adds, removes) = count_patch_changes(patch_text);
    let summary = if adds == 0 && removes == 0 {
        "Patch applied".to_string()
    } else {
        format!("Changes: +{adds} / -{removes}")
    };
    (path, summary)
}

fn extract_patch_paths(patch: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let raw = rest.trim();
            if raw == "/dev/null" || raw == "dev/null" {
                continue;
            }
            let raw = raw.strip_prefix("b/").unwrap_or(raw);
            if !paths.contains(&raw.to_string()) {
                paths.push(raw.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if let Some(path) = parts.get(1).or_else(|| parts.first()) {
                let raw = path.trim();
                let raw = raw
                    .strip_prefix("b/")
                    .or_else(|| raw.strip_prefix("a/"))
                    .unwrap_or(raw);
                if !paths.contains(&raw.to_string()) {
                    paths.push(raw.to_string());
                }
            }
        }
    }
    paths
}

fn count_patch_changes(patch: &str) -> (usize, usize) {
    let mut adds = 0;
    let mut removes = 0;
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            adds += 1;
        } else if line.starts_with('-') {
            removes += 1;
        }
    }
    (adds, removes)
}

fn exec_command_from_input(input: &serde_json::Value) -> Option<String> {
    input
        .get("command")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
}

fn exec_target_from_input(input: &serde_json::Value) -> String {
    exec_command_from_input(input).unwrap_or_else(|| {
        input
            .get("task_id")
            .or_else(|| input.get("id"))
            .and_then(|v| v.as_str())
            .map(|task_id| format!("command {task_id}"))
            .unwrap_or_else(|| "command".to_string())
    })
}

fn exec_source_from_input(input: &serde_json::Value) -> ExecSource {
    match input.get("source").and_then(|v| v.as_str()) {
        Some(source) if source.eq_ignore_ascii_case("user") => ExecSource::User,
        _ => ExecSource::Assistant,
    }
}

fn exec_interaction_summary(name: &str, input: &serde_json::Value) -> Option<(String, bool)> {
    let command = exec_target_from_input(input);
    let command_display = format!("\"{command}\"");
    let interaction_input = input
        .get("input")
        .or_else(|| input.get("stdin"))
        .or_else(|| input.get("data"))
        .and_then(|v| v.as_str());

    let is_wait_tool = matches!(name, "exec_shell_wait" | "exec_wait");
    let is_interact_tool = matches!(name, "exec_shell_interact" | "exec_interact");
    let is_cancel_tool = name == "exec_shell_cancel";

    if is_cancel_tool {
        let summary = if input.get("all").and_then(serde_json::Value::as_bool) == Some(true) {
            "Cancelled all background commands".to_string()
        } else if let Some(task_id) = input
            .get("task_id")
            .or_else(|| input.get("id"))
            .and_then(serde_json::Value::as_str)
        {
            format!("Cancelled command {task_id}")
        } else {
            "Cancelled background command".to_string()
        };
        return Some((summary, false));
    }

    if is_interact_tool || interaction_input.is_some() {
        let preview = interaction_input.map(summarize_interaction_input);
        let summary = if let Some(preview) = preview {
            format!("Interacted with {command_display}, sent {preview}")
        } else {
            format!("Interacted with {command_display}")
        };
        return Some((summary, false));
    }

    if is_wait_tool || input.get("wait").and_then(serde_json::Value::as_bool) == Some(true) {
        if exec_command_from_input(input).is_none()
            && let Some(task_id) = input
                .get("task_id")
                .or_else(|| input.get("id"))
                .and_then(|v| v.as_str())
        {
            return Some((format!("Waiting for command {task_id}"), true));
        }
        return Some((format!("Waited for {command_display}"), true));
    }

    None
}

fn summarize_interaction_input(input: &str) -> String {
    let mut single_line = input.replace('\r', "");
    single_line = single_line.replace('\n', "\\n");
    single_line = single_line.replace('\"', "'");
    let max_len = 80;
    if single_line.chars().count() <= max_len {
        return format!("\"{single_line}\"");
    }
    let mut out = String::new();
    for ch in single_line.chars().take(max_len.saturating_sub(3)) {
        out.push(ch);
    }
    out.push_str("...");
    format!("\"{out}\"")
}

fn exec_is_background(input: &serde_json::Value) -> bool {
    input
        .get("background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::plan::StepStatus;
    use serde_json::json;

    #[test]
    fn concurrent_runs_keep_their_own_events() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        apply_workflow_ui_event(
            &mut app,
            "run-a",
            &json!({
                "type": "run_started",
                "workflow_goal": "first run",
                "at_ms": 1_000,
            }),
        );
        apply_workflow_ui_event(
            &mut app,
            "run-b",
            &json!({
                "type": "run_started",
                "workflow_goal": "second run",
                "at_ms": 2_000,
            }),
        );
        apply_workflow_ui_event(
            &mut app,
            "run-b",
            &json!({"type": "phase_started", "title": "Build", "at_ms": 2_100}),
        );
        let before = app.workflow_run("run-b").expect("run B").to_run_json();

        // Both runs are rows of their own; a delayed start for A touches A.
        apply_workflow_ui_event(
            &mut app,
            "run-a",
            &json!({
                "type": "run_started",
                "workflow_goal": "delayed first run",
                "at_ms": 1_500,
            }),
        );
        // The immutable envelope says A even if a malformed embedded field
        // claims B. Neither this failure nor A's terminal event belongs to B.
        apply_workflow_ui_event(
            &mut app,
            "run-a",
            &json!({
                "type": "task_dispatch_failed",
                "run_id": "run-b",
                "label": "late task",
                "message": "late A failure",
                "at_ms": 2_200,
            }),
        );
        apply_workflow_ui_event(
            &mut app,
            "run-a",
            &json!({
                "type": "run_completed",
                "status": "failed",
                "error": "late A completion",
                "at_ms": 2_300,
            }),
        );

        assert_eq!(app.workflow_runs.len(), 2, "two runs, two rows");
        assert_eq!(
            app.workflow_run("run-b").expect("run B").to_run_json(),
            before
        );
        let run_a = app.workflow_run("run-a").expect("run A");
        assert_eq!(
            run_a.lifecycle,
            crate::tui::widgets::workflow_panel::WorkflowPanelLifecycle::Failed
        );
        assert_eq!(run_a.dispatch_failure_count, 1);
    }

    #[test]
    fn a_completion_replay_for_another_run_never_touches_this_one() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        apply_workflow_ui_event(
            &mut app,
            "run-b",
            &json!({
                "type": "run_started",
                "workflow_goal": "active run",
                "at_ms": 2_000,
            }),
        );
        apply_workflow_ui_event(
            &mut app,
            "run-b",
            &json!({"type": "phase_started", "title": "Verify", "at_ms": 2_100}),
        );
        let before = app.workflow_run("run-b").expect("run B").to_run_json();

        // A retained completion tail can contain run_started. The top-level
        // run identity keeps the whole replay on run A.
        apply_workflow_output_to_panel(
            &mut app,
            &json!({
                "run_id": "run-a",
                "workflow_goal": "prior run",
                "started_at_ms": 1_000,
                "completed_at_ms": 2_200,
                "status": "failed",
                "events": [
                    {
                        "type": "run_started",
                        "run_id": "run-a",
                        "workflow_goal": "prior run",
                        "at_ms": 1_000,
                    },
                    {
                        "type": "task_dispatch_failed",
                        "run_id": "run-a",
                        "message": "prior failure",
                        "at_ms": 1_100,
                    },
                    {
                        "type": "run_completed",
                        "run_id": "run-a",
                        "status": "failed",
                        "at_ms": 2_200,
                    }
                ],
                "dispatch_failure_count": 1,
                "dispatch_failures": [{
                    "message": "prior failure",
                    "at_ms": 1_100,
                }],
            })
            .to_string(),
        );

        assert_eq!(
            app.workflow_run("run-b").expect("run B").to_run_json(),
            before
        );
        assert!(app.workflow_run("run-a").is_some());
    }

    #[test]
    fn workflow_completion_replay_uses_authoritative_dispatch_failure_ledger() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        let failure = json!({
            "type": "task_dispatch_failed",
            "label": "review docs",
            "phase": "Analyze",
            "message": "profile unavailable",
            "at_ms": 1_250,
        });
        apply_workflow_ui_event(
            &mut app,
            "run-1",
            &json!({
                "type": "run_started",
                "workflow_goal": "audit",
                "at_ms": 1_000,
            }),
        );
        apply_workflow_ui_event(&mut app, "run-1", &failure);
        assert_eq!(
            app.workflow_run("run-1")
                .expect("live run")
                .dispatch_failure_count,
            1
        );

        // A long run's retained tail may no longer include run_started, so
        // this event is a replay of the live failure rather than a new slot.
        apply_workflow_output_to_panel(
            &mut app,
            &json!({
                "run_id": "run-1",
                "workflow_goal": "audit",
                "started_at_ms": 1_000,
                "events": [failure],
                "dispatch_failure_count": 1,
                "dispatch_failures": [{
                    "label": "review docs",
                    "phase": "Analyze",
                    "message": "profile unavailable",
                    "at_ms": 1_250,
                }],
            })
            .to_string(),
        );

        let panel = app.workflow_run("run-1").expect("completed run");
        assert_eq!(panel.dispatch_failure_count, 1);
        assert_eq!(panel.dispatch_failures.len(), 1);
        assert_eq!(panel.failure_cancel_counts(), (1, 0));
    }

    #[test]
    fn degraded_run_writes_one_warning_finish_line_after_its_start_card() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        app.history
            .push(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
                name: "workflow".to_string(),
                status: ToolStatus::Running,
                input_summary: Some("action: run".to_string()),
                output: None,
                prompts: None,
                spillover_path: None,
                output_summary: None,
                is_diff: false,
            })));

        apply_workflow_output_to_panel(
            &mut app,
            &json!({
                "run_id": "run-partial",
                "workflow_goal": "audit",
                "status": "degraded",
                "started_at_ms": 1_000,
                "completed_at_ms": 2_000,
                "dispatch_failure_count": 1,
                "dispatch_failures": [{
                    "label": "review docs",
                    "message": "profile unavailable",
                    "at_ms": 1_500,
                }],
            })
            .to_string(),
        );

        let HistoryCell::Tool(ToolCell::Generic(receipt)) = app.history.last().expect("receipt")
        else {
            panic!("the finish line is a workflow card")
        };
        assert_eq!(receipt.status, ToolStatus::Warning);
        let output: serde_json::Value =
            serde_json::from_str(receipt.output.as_deref().expect("snapshot")).expect("json");
        assert_eq!(output["transcript_line"], "finished");
        assert_eq!(output["run_id"], "run-partial");
        assert!(!history_cell_has_running_tool(
            app.history.last().expect("receipt")
        ));

        // The live stream delivering the same terminal event later does not
        // write the line again.
        let cells = app.history.len();
        apply_workflow_ui_event(
            &mut app,
            "run-partial",
            &json!({"type": "run_completed", "status": "degraded", "at_ms": 2_000}),
        );
        assert_eq!(app.history.len(), cells);
    }

    #[test]
    fn a_foreground_run_card_carries_its_own_finish_line() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        let mut active = crate::tui::active_cell::ActiveCell::new();
        active.push_untracked(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "workflow".to_string(),
            status: ToolStatus::Running,
            input_summary: None,
            output: None,
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })));
        app.active_cell = Some(active);
        apply_workflow_ui_event(
            &mut app,
            "run-fg",
            &json!({"type": "run_started", "workflow_goal": "survey", "at_ms": 1_000}),
        );
        apply_workflow_ui_event(
            &mut app,
            "run-fg",
            &json!({"type": "run_completed", "status": "completed", "result_preview": "3 findings", "at_ms": 5_000}),
        );
        // The tool returns the settled record into its own card.
        if let Some(HistoryCell::Tool(ToolCell::Generic(card))) = app
            .active_cell
            .as_mut()
            .and_then(|active| active.entry_mut(0))
        {
            card.status = ToolStatus::Success;
            card.output = Some(
                json!({
                    "run_id": "run-fg",
                    "workflow_goal": "survey",
                    "status": "completed",
                    "started_at_ms": 1_000,
                    "completed_at_ms": 5_000,
                    "result": {"summary": "3 findings"},
                })
                .to_string(),
            );
        }
        app.flush_active_cell();
        assert_eq!(
            app.history.len(),
            1,
            "the card owns the finish; no second line"
        );
        let rendered = app.history[0]
            .lines(100)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The settled record replaces `started`: one row for the run.
        assert!(!rendered.contains("started"), "{rendered}");
        assert!(rendered.contains("finished"), "{rendered}");
        assert!(rendered.contains("3 findings"), "{rendered}");
    }

    #[test]
    fn a_detached_finish_waits_for_its_start_card_to_leave_the_active_group() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        let start_card = HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "workflow".to_string(),
            status: ToolStatus::Success,
            input_summary: None,
            output: Some(
                json!({"run_id": "run-fast", "workflow_goal": "quick", "status": "running"})
                    .to_string(),
            ),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        }));
        let mut active = crate::tui::active_cell::ActiveCell::new();
        active.push_untracked(start_card);
        app.active_cell = Some(active);
        for event in [
            json!({"type": "run_started", "workflow_goal": "quick", "at_ms": 1_000}),
            json!({"type": "run_completed", "status": "failed", "error": "script error, line 3", "at_ms": 1_400}),
        ] {
            apply_workflow_ui_event(&mut app, "run-fast", &event);
        }
        assert!(
            app.history.is_empty(),
            "the finish line must not jump above its start"
        );

        app.flush_active_cell();
        // One row per run: the start card becomes the finish, in place.
        assert_eq!(app.history.len(), 1, "the start card is the finish line");
        let HistoryCell::Tool(ToolCell::Generic(finish)) = &app.history[0] else {
            panic!("finish line is a workflow card");
        };
        assert_eq!(finish.status, ToolStatus::Failed);
        let rendered = app.history[0]
            .lines(100)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("failed"), "{rendered}");
        assert!(rendered.contains("quick"), "{rendered}");
        assert!(rendered.contains("script error, line 3"), "{rendered}");
        assert!(!rendered.contains("started"), "{rendered}");

        // The live stream repeating the terminal event writes nothing more.
        apply_workflow_ui_event(
            &mut app,
            "run-fast",
            &json!({"type": "run_completed", "status": "failed", "error": "script error, line 3", "at_ms": 1_400}),
        );
        assert_eq!(app.history.len(), 1);
    }

    fn workflow_card(record: serde_json::Value) -> HistoryCell {
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "workflow".to_string(),
            status: ToolStatus::Success,
            input_summary: None,
            output: Some(record.to_string()),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        }))
    }

    fn is_finish_card(cell: &HistoryCell) -> bool {
        let HistoryCell::Tool(ToolCell::Generic(tool)) = cell else {
            return false;
        };
        tool.output
            .as_deref()
            .and_then(|out| serde_json::from_str::<serde_json::Value>(out).ok())
            .is_some_and(|value| value.get("transcript_line").is_some())
    }

    fn settle_run(app: &mut App, run_id: &str) {
        for event in [
            json!({"type": "run_started", "workflow_goal": "quick", "at_ms": 1_000}),
            json!({"type": "run_completed", "status": "failed", "error": "script error", "at_ms": 1_355}),
        ] {
            apply_workflow_ui_event(app, run_id, &event);
        }
    }

    #[test]
    fn a_status_poll_that_returned_the_settled_record_is_the_only_finish() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        let mut active = crate::tui::active_cell::ActiveCell::new();
        active.push_untracked(workflow_card(
            json!({"run_id": "run-x", "workflow_goal": "quick", "status": "running"}),
        ));
        active.push_untracked(workflow_card(
            json!({"run_id": "run-x", "workflow_goal": "quick", "status": "failed"}),
        ));
        app.active_cell = Some(active);
        settle_run(&mut app, "run-x");
        app.flush_active_cell();

        assert_eq!(app.history.len(), 2, "no extra finish appended");
        assert!(
            !app.history.iter().any(is_finish_card),
            "the poll already shows the finish; the start card must not repeat it"
        );
    }

    #[test]
    fn the_finish_rewrites_the_start_card_not_a_later_status_poll() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        let running = json!({"run_id": "run-x", "workflow_goal": "quick", "status": "running"});
        app.add_message(workflow_card(running.clone()));
        app.add_message(workflow_card(running));
        settle_run(&mut app, "run-x");

        assert_eq!(app.history.len(), 2);
        assert!(is_finish_card(&app.history[0]), "the start card settles");
        assert!(!is_finish_card(&app.history[1]), "the poll is left alone");
    }

    #[test]
    fn a_run_that_settles_after_the_conversation_moved_on_finishes_at_the_tail() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        app.add_message(workflow_card(
            json!({"run_id": "run-x", "workflow_goal": "quick", "status": "running"}),
        ));
        app.add_message(HistoryCell::User {
            content: "meanwhile, something else".to_string(),
        });
        settle_run(&mut app, "run-x");

        assert_eq!(app.history.len(), 3, "the finish is appended");
        assert!(!is_finish_card(&app.history[0]));
        assert!(
            is_finish_card(&app.history[2]),
            "the finish sits at the tail"
        );
    }

    #[cfg(unix)]
    fn hook_log_lines_eventually(path: &std::path::Path, expected: usize) -> Vec<String> {
        for _ in 0..100 {
            let lines = std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            if lines.len() >= expected {
                return lines;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A UI-ignored completion is still a completion. `tool_call_after` and
    /// `on_error` must fire for it — exactly once — or the documented "fires
    /// after each tool call" silently excludes repeated `wait` and background
    /// results, which is the class of call an observer most wants to record.
    #[cfg(unix)]
    #[test]
    fn ignored_tool_calls_still_fire_after_and_error_hooks_once() {
        use crate::hooks::{Hook, HookEvent, HookExecutor, HooksConfig};

        let dir = tempfile::tempdir().expect("tempdir");
        let after_log = dir.path().join("after.log");
        let error_log = dir.path().join("error.log");
        let script = |path: &std::path::Path| {
            format!(
                "printf '%s\\n' \"$DEEPSEEK_TOOL_CALL_ID\" >> {}",
                path.display()
            )
        };

        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(dir.path()),
        );
        app.workspace = dir.path().to_path_buf();
        app.hooks = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![
                    Hook::new(HookEvent::ToolCallAfter, &script(&after_log)).with_name("after"),
                    Hook::new(HookEvent::OnError, &script(&error_log)).with_name("error"),
                ],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let id = "call_ignored_1";
        app.ignored_tool_calls.insert(id.to_string());
        let failed: Result<ToolResult, ToolError> = Ok(ToolResult::error("boom"));

        handle_tool_call_complete(&mut app, id, "exec_shell", &failed);

        // The presentation state still consumed the id...
        assert!(!app.ignored_tool_calls.contains(id));
        // ...and both observers saw the call, once each.
        let after = hook_log_lines_eventually(&after_log, 1);
        let errors = hook_log_lines_eventually(&error_log, 1);
        assert_eq!(after, vec![id]);
        assert_eq!(errors, vec![id]);

        // A successful ignored completion fires `tool_call_after` only.
        let second = "call_ignored_2";
        app.ignored_tool_calls.insert(second.to_string());
        handle_tool_call_complete(
            &mut app,
            second,
            "exec_shell",
            &Ok(ToolResult::success("ok")),
        );
        let after = hook_log_lines_eventually(&after_log, 2);
        let errors = hook_log_lines_eventually(&error_log, 1);
        assert_eq!(after, vec![id, second]);
        assert_eq!(errors, vec![id]);
    }

    /// #6582: hooks see a `bash` command's real exit code and status, for a
    /// failing command as well as a passing one. A nonzero exit is a
    /// `ToolError`. A foreground wait that expires is not: the command moves
    /// to the background and stays running, so `on_error` does not fire for it.
    #[cfg(unix)]
    #[test]
    fn bash_completion_hooks_get_exit_code_and_status_for_failures() {
        use crate::hooks::{Hook, HookEvent, HookExecutor, HooksConfig};
        use crate::tools::spec::{ToolContext, ToolSpec};

        let dir = tempfile::tempdir().expect("tempdir");
        let after_log = dir.path().join("after.log");
        let error_log = dir.path().join("error.log");
        let script = |path: &std::path::Path| {
            format!(
                "printf '%s %s %s %s\\n' \"$DEEPSEEK_TOOL_CALL_ID\" \"${{DEEPSEEK_TOOL_EXIT_CODE-unset}}\" \"${{DEEPSEEK_TOOL_STATUS-unset}}\" \"$DEEPSEEK_TOOL_SUCCESS\" >> {}",
                path.display()
            )
        };
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(dir.path()),
        );
        app.workspace = dir.path().to_path_buf();
        app.hooks = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![
                    Hook::new(HookEvent::ToolCallAfter, &script(&after_log)),
                    Hook::new(HookEvent::OnError, &script(&error_log)),
                ],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let context = ToolContext::new(dir.path());
        let cases = [
            ("call-exit-0", json!({"command": "exit 0"})),
            ("call-exit-1", json!({"command": "exit 1"})),
            (
                "call-exit-127",
                json!({"command": "codewhale-no-such-command-6582"}),
            ),
            (
                "call-timeout",
                json!({"command": "sleep 5", "timeout": 0.2}),
            ),
        ];
        for (id, input) in cases {
            let result =
                runtime.block_on(crate::tools::shell::LowercaseBashTool.execute(input, &context));
            handle_tool_call_complete(&mut app, id, "bash", &result);
        }

        let mut after = hook_log_lines_eventually(&after_log, 4);
        let mut errors = hook_log_lines_eventually(&error_log, 2);
        after.sort();
        errors.sort();
        assert_eq!(
            after,
            vec![
                "call-exit-0 0 completed true",
                "call-exit-1 1 failed false",
                "call-exit-127 127 failed false",
                "call-timeout unset running true",
            ]
        );
        assert_eq!(
            errors,
            vec![
                "call-exit-1 1 failed false",
                "call-exit-127 127 failed false",
            ]
        );
    }

    #[test]
    fn adaptive_evidence_late_foreign_and_duplicate_completions_are_ignored() {
        let result = Ok(ToolResult::success("bounded").with_metadata(json!({
            "artifact_session_id": "session-a",
            "artifact_id": "art_call-a"
        })));
        assert!(evidence_completion_identity_should_be_ignored(
            Some("session-b"),
            std::iter::empty(),
            "call-a",
            &result,
        ));
        assert!(evidence_completion_identity_should_be_ignored(
            Some("session-a"),
            [("art_call-a", "call-a")],
            "call-a",
            &result,
        ));
        assert!(!evidence_completion_identity_should_be_ignored(
            Some("session-a"),
            std::iter::empty(),
            "call-a",
            &result,
        ));
    }

    #[test]
    fn web_search_presentation_reads_source_degradation_and_citation_count() {
        let presentation = web_search_presentation(
            &json!({
                "source": "provider-native/xai/grok-4.5",
                "results": [
                    {"ref_id": "web_a", "url": "https://example.com/a"},
                    {"ref_id": "web_b", "url": "https://example.com/b"}
                ],
                "receipt": {
                    "degraded": [
                        {"kind": "backend_unavailable", "backend": "provider_native"},
                        {"kind": "backend_fallback", "from": "provider_native", "to": "tavily"}
                    ]
                }
            })
            .to_string(),
        );

        assert_eq!(
            presentation.source.as_deref(),
            Some("provider-native/xai/grok-4.5")
        );
        assert_eq!(
            presentation.degraded.as_deref(),
            Some("provider_native unavailable; provider_native -> tavily")
        );
        assert_eq!(presentation.ref_count, 2);
    }

    #[test]
    fn web_run_presentation_reads_nested_search_receipts() {
        let presentation = web_search_presentation(
            &json!({
                "search_query": [{
                    "source": "duckduckgo",
                    "results": [{"ref_id": "web_a"}],
                    "receipt": {
                        "degraded": [{"kind": "knob_ignored", "knob": "recency"}]
                    }
                }]
            })
            .to_string(),
        );

        assert_eq!(presentation.source.as_deref(), Some("duckduckgo"));
        assert_eq!(presentation.degraded.as_deref(), Some("recency ignored"));
        assert_eq!(presentation.ref_count, 1);
    }

    #[test]
    fn parse_plan_input_accepts_legacy_payload() {
        let snapshot = parse_plan_input(&json!({
            "explanation": "Legacy explanation",
            "plan": [
                { "step": "inspect", "status": "completed" },
                { "step": "patch", "status": "in_progress" }
            ]
        }));

        assert_eq!(snapshot.explanation.as_deref(), Some("Legacy explanation"));
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[0].status, StepStatus::Completed);
        assert_eq!(snapshot.items[1].status, StepStatus::InProgress);
    }

    #[test]
    fn parse_plan_input_extracts_rich_artifact_fields() {
        let snapshot = parse_plan_input(&json!({
            "title": " PlanArtifact ",
            "objective": "Make Plan mode reviewable",
            "context_summary": "Grounded in issue #2691",
            "sources_used": [" gh issue view 2691 ", ""],
            "critical_files": ["crates/tui/src/tools/plan.rs"],
            "constraints": ["No secrets"],
            "recommended_approach": "Enrich update_plan",
            "verification_plan": "Run focused tests",
            "risks_and_unknowns": "Replay may drift",
            "handoff_packet": "Continue with session replay",
            "plan": [
                { "step": " ", "status": "completed" },
                { "step": "render all fields", "status": "weird" }
            ]
        }));

        assert_eq!(snapshot.title.as_deref(), Some("PlanArtifact"));
        assert_eq!(snapshot.sources_used, vec!["gh issue view 2691"]);
        assert_eq!(
            snapshot.critical_files,
            vec!["crates/tui/src/tools/plan.rs"]
        );
        assert_eq!(snapshot.constraints, vec!["No secrets"]);
        assert_eq!(
            snapshot.verification_plan.as_deref(),
            Some("Run focused tests")
        );
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].step, "render all fields");
        assert_eq!(snapshot.items[0].status, StepStatus::Pending);
    }

    #[test]
    fn parse_patch_summary_treats_replace_and_legacy_changes_equally() {
        let replacements = json!([{
            "path": "src/lib.rs",
            "content": "fn replacement() {}\n"
        }]);

        let canonical =
            parse_file_mutation_summary("apply_patch", &json!({"replace": replacements.clone()}));
        let legacy = parse_file_mutation_summary("apply_patch", &json!({"changes": replacements}));

        assert_eq!(canonical, legacy);
    }

    // ── #3031: "(no output)" placeholder must not defeat compact rendering ─

    #[test]
    fn visible_tool_output_maps_no_output_placeholder_to_none() {
        assert_eq!(visible_tool_output("(no output)"), None);
        assert_eq!(visible_tool_output("  (no output)\n"), None);
    }

    #[test]
    fn visible_tool_output_preserves_real_content() {
        assert_eq!(
            visible_tool_output("compiled 3 crates").as_deref(),
            Some("compiled 3 crates")
        );
        // Output that merely CONTAINS the placeholder is real output.
        assert_eq!(
            visible_tool_output("step 1: (no output) — continuing").as_deref(),
            Some("step 1: (no output) — continuing")
        );
        assert_eq!(visible_tool_output("").as_deref(), Some(""));
    }

    #[test]
    fn exec_cell_without_output_suppresses_placeholder_in_live_mode() {
        use crate::tui::history::{ExecCell, ExecSource, ToolCell, ToolStatus};

        let cell = ToolCell::Exec(ExecCell {
            command: "true".to_string(),
            status: ToolStatus::Success,
            output: None,
            live_output: None,
            shell_task_id: None,
            owner_agent_id: None,
            owner_agent_name: None,
            started_at: None,
            duration_ms: Some(120),
            stale_elapsed_since_output_ms: None,
            waiting: None,
            source: ExecSource::Assistant,
            description: None,
            interaction: None,
            output_summary: None,
        });

        let live: String = cell
            .lines(80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            !live.contains("(no output)"),
            "Live mode must suppress the placeholder: {live:?}"
        );

        let transcript: String = cell
            .transcript_lines(80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            transcript.contains("(no output)"),
            "Transcript mode still records the placeholder: {transcript:?}"
        );
    }

    // === #5472 finding 3: retained tool outputs are bounded ===

    #[test]
    fn tool_detail_output_is_capped_and_says_what_it_dropped() {
        let small = "short output".to_string();
        assert_eq!(super::bounded_tool_detail_output(small.clone()), small);

        let huge = "x".repeat(super::TOOL_DETAIL_OUTPUT_MAX_BYTES * 3);
        let bounded = super::bounded_tool_detail_output(huge);
        assert!(
            bounded.len() < super::TOOL_DETAIL_OUTPUT_MAX_BYTES + 200,
            "retained {} bytes",
            bounded.len()
        );
        assert!(bounded.contains("the transcript keeps an excerpt"));
    }

    #[test]
    fn tool_detail_cap_never_splits_a_character() {
        // Every char is 3 bytes, so a byte-exact cut lands mid-character.
        let wide = "宽".repeat(super::TOOL_DETAIL_OUTPUT_MAX_BYTES);
        let bounded = super::bounded_tool_detail_output(wide);
        assert!(bounded.starts_with('宽'));
        assert!(bounded.contains("excerpt"));
    }

    #[test]
    fn oldest_tool_outputs_are_released_once_the_budget_is_exceeded() {
        let mut app = crate::tui::app::App::new(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
            &crate::config::Config::default(),
        );
        // 200 records x 64 KiB = 12.8 MiB, well past the 8 MiB budget.
        let record_count = 200usize;
        for index in 0..record_count {
            let tool_id = format!("tool-{index}");
            super::handle_tool_call_started(
                &mut app,
                &tool_id,
                "read_file",
                &serde_json::json!({ "path": format!("file-{index}.txt") }),
            );
            app.active_tool_details.insert(
                tool_id.clone(),
                ToolDetailRecord {
                    tool_id,
                    tool_name: "read_file".to_string(),
                    input: serde_json::Value::Null,
                    output: Some("y".repeat(super::TOOL_DETAIL_OUTPUT_MAX_BYTES)),
                },
            );
        }
        app.flush_active_cell();

        let retained: usize = app
            .tool_details_by_cell
            .values()
            .map(|detail| detail.output.as_ref().map_or(0, String::len))
            .sum();
        assert!(
            retained <= super::TOOL_DETAIL_TOTAL_BUDGET_BYTES,
            "retained {retained} bytes over the {} budget",
            super::TOOL_DETAIL_TOTAL_BUDGET_BYTES
        );
        assert_eq!(
            app.tool_details_by_cell.len(),
            record_count,
            "records stay listed; only their outputs are released"
        );
        assert!(
            app.tool_details_by_cell[&(record_count - 1)]
                .output
                .is_some(),
            "the newest output must survive — it is the one the user can still expand"
        );
        assert!(
            app.tool_details_by_cell[&0].output.is_none(),
            "the oldest output is the first to go"
        );
    }
}
