//! `POST /v1/threads/{id}/commands/builtin` — execute a terminal built-in
//! command against a thread, remotely.
//!
//! The thread's state is assembled into the engine's
//! [`BuiltinCommandHostSnapshot`] (contract projections included) and handed
//! to the installed command catalog, whose terminal-host implementation maps
//! it into the facet envelope and runs the portable handlers. Which commands
//! this serves is the host's decision (`builtin_execution::SERVED`); a name
//! outside that set answers 404 the same way an unknown name does, so the
//! remote surface can never widen without the host widening it first.
//!
//! Phase 1 notes: executions are not receipt-tracked (the phase-1 set is
//! read-only — every served handler is pure over the snapshot), and the
//! `text_only` flag reports commands that also asked the terminal host for an
//! action, which a remote client cannot perform.

use axum::Json;
use axum::extract::{Path, State};
use codewhale_command_contract::facets::{
    DebugCostProjection, DebugRouteCost, DebugSystemPrompt, DebugTokenProjection,
};
use codewhale_command_contract::types::{CommandCurrency, CommandMode};
use codewhale_engine::command_catalog::{self, BuiltinCommandHostSnapshot};
use codewhale_engine::runtime_threads::manager::RuntimeThreadManager;
use serde::Deserialize;
use std::sync::Arc;

use super::{ApiError, RuntimeApiState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuiltinCommandApiRequest {
    /// The command name (canonical or alias), with or without the slash.
    pub name: String,
    /// The argument tail after the command word, if any.
    #[serde(default)]
    pub args: Option<String>,
}

#[derive(serde::Serialize)]
pub(super) struct BuiltinCommandApiResponse {
    pub name: String,
    pub text: String,
    pub is_error: bool,
    /// `false` when the command also asked the terminal host for an action
    /// the remote client cannot perform.
    pub text_only: bool,
}

pub(super) async fn run_builtin_command(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
    Json(request): Json<BuiltinCommandApiRequest>,
) -> Result<Json<BuiltinCommandApiResponse>, ApiError> {
    let catalog = command_catalog::installed()
        .ok_or_else(|| ApiError::internal("command catalog is not installed"))?;
    // Refuse a name outside the host's remote surface before reading any
    // thread state: an unknown command and a known-but-unserved one get the
    // same answer, and neither can be distinguished by what the read says
    // about the daemon's idle threads.
    if !catalog.serves_builtin(&request.name) {
        return Err(ApiError::not_found(format!(
            "no remotely executable builtin command '{}'",
            request.name
        )));
    }

    let thread = state
        .runtime_threads
        .get_thread(&thread_id)
        .await
        .map_err(super::map_thread_err)?;

    let snapshot = assemble_snapshot(&state, &thread_id, &thread).await?;

    match catalog.execute_builtin(&request.name, request.args.as_deref(), &snapshot) {
        Some(execution) => Ok(Json(BuiltinCommandApiResponse {
            name: execution.name,
            text: execution.text,
            is_error: execution.is_error,
            text_only: execution.text_only,
        })),
        // The host refused the name: the same answer an unknown command gets.
        None => Err(ApiError::not_found(format!(
            "no remotely executable builtin command '{}'",
            request.name
        ))),
    }
}

/// Assemble the host snapshot for one thread from the record, the live
/// engine's context budget, the usage aggregate, and the canonical session
/// document. Every projection field is filled from measured data or left at
/// its honest absence encoding; nothing is estimated into place.
async fn assemble_snapshot(
    state: &RuntimeApiState,
    thread_id: &str,
    thread: &codewhale_engine::runtime_threads::ThreadRecord,
) -> Result<BuiltinCommandHostSnapshot, ApiError> {
    // The live engine carries the context budget; an idle daemon thread has
    // no honest context estimate, so the served diagnostics refuse rather
    // than print zeros.
    let engine = state
        .runtime_threads
        .get_engine(thread_id)
        .await
        .map_err(super::map_thread_err)?;
    let budget = engine
        .get_context_budget()
        .await
        .map_err(super::map_thread_err)?
        .ok_or_else(|| {
            ApiError::conflict("this thread has no live context estimate yet; send a message first")
        })?;

    let usage = state
        .runtime_threads
        .aggregate_usage_for_thread(thread_id)
        .await
        .map_err(super::map_thread_err)?;
    let parent = &usage.parent;
    let children = &usage.routed_children;
    let combined = usage.combined();

    // The daemon tracks no UI chat history (that is the client's layer), so
    // the chat count is a true zero; the model transcript count comes from
    // the canonical session document.
    let (api_message_count, chat_message_count) = session_message_counts(
        &state.runtime_threads,
        state.sessions_dir.as_path(),
        thread_id,
    )
    .await;

    // Currency follows the same rule the TUI's cost display applies, so the
    // same config answers the same way on both surfaces: a Simplified-Chinese
    // UI reading the default "usd" shows CNY, and every other spelling goes
    // through the canonical parser (which accepts rmb and yuan) rather than a
    // bare string compare. The CNY column is the provider-published subtotal
    // only; a USD-picked currency keeps the authoritative USD numbers.
    let settings = codewhale_runtime::settings::Settings::load().unwrap_or_default();
    let locale = codewhale_localization::resolve_locale(&settings.locale);
    let currency = match (settings.cost_currency.as_str(), locale.tag()) {
        ("usd", "zh-Hans") => CommandCurrency::Cny,
        _ => match codewhale_config::pricing::CostCurrency::from_setting(&settings.cost_currency) {
            Some(codewhale_config::pricing::CostCurrency::Cny) => CommandCurrency::Cny,
            _ => CommandCurrency::Usd,
        },
    };
    let currency_amount =
        |totals: &codewhale_engine::runtime_threads::manager::UsageTotals| match currency {
            CommandCurrency::Cny => totals.cost_cny,
            CommandCurrency::Usd => totals.cost_usd,
        };
    let parent_amount = currency_amount(parent);
    let children_amount = currency_amount(children);
    let total_amount = currency_amount(&combined);
    let (priced_turns, unpriced_turns, unpriced_reasons) = match currency {
        CommandCurrency::Cny => (
            combined.cny_priced_turns,
            combined.cny_unpriced_turns,
            &combined.cny_unpriced_reasons,
        ),
        CommandCurrency::Usd => (
            combined.priced_turns,
            combined.unpriced_turns,
            &combined.unpriced_reasons,
        ),
    };

    let mode = CommandMode::parse(&thread.mode).unwrap_or(CommandMode::Agent);
    let mode_label = codewhale_config::AppMode::from_setting(&thread.mode)
        .label()
        .to_string();
    let system_prompt = thread
        .system_prompt
        .clone()
        .map_or(DebugSystemPrompt::None, DebugSystemPrompt::Text);

    let cost = DebugCostProjection {
        currency,
        // The combined subtotal is the authoritative total, and it is by
        // construction the parent and children amounts added up; the
        // terminal's #244 reconciliation floor only exists because its
        // display total and its recomputed components can drift apart. The
        // remote aggregate cannot drift from itself, so there is no floor to
        // report and none is invented.
        total: total_amount,
        parent_turns: parent_amount,
        subagents: children_amount,
        display_floor: 0.0,
        priced_turns: priced_turns.clamp(0, u32::MAX as u64) as u32,
        unpriced_turns: unpriced_turns.clamp(0, u32::MAX as u64) as u32,
        legacy_coverage_unknown: !combined.cost_complete,
        // Pricing-provenance bookkeeping (user-declared estimates, live
        // defects, route receipts) is terminal-session state the daemon
        // does not track; the honest encoding is "none recorded".
        user_declared_estimates: false,
        // Per-route attribution needs per-turn provider identities, which
        // the remote snapshot does not assemble yet; the coverage counts
        // above stay authoritative. With no route rows, no turn is itemized:
        // claiming otherwise would make the renderer state a breakdown it
        // cannot show.
        itemized_turns: 0,
        route_amounts: Vec::<DebugRouteCost>::new(),
        turn_history_capacity: 0,
        unpriced_reason_labels: unpriced_reasons.iter().cloned().collect(),
        unpriced_classes: combined.unpriced_classes.iter().cloned().collect(),
        pricing_provenances: Vec::new(),
        live_pricing_defects: Vec::new(),
        unusable_pricing_defects: Vec::new(),
        route_receipts: Vec::new(),
    };

    Ok(BuiltinCommandHostSnapshot {
        mode,
        mode_label,
        locale,
        model: thread.model.clone(),
        system_prompt,
        tokens: DebugTokenProjection {
            active_context_used: (budget.input_tokens as usize).min(budget.window_tokens as usize),
            context_window: budget.window_tokens.clamp(0, u32::MAX as u64) as u32,
            // The context budget carries no per-turn telemetry, so the
            // last-turn columns take their honest absence encoding rather
            // than a stale or fabricated value.
            last_input: None,
            last_output: None,
            cache_hit: None,
            cache_miss: None,
            total_tokens: combined.input_tokens.saturating_add(combined.output_tokens),
            cache_write_tokens: combined.cache_write_tokens,
            api_message_count,
            chat_message_count,
            model: thread.model.clone(),
            cost,
        },
    })
}

/// The canonical session document's transcript length, and the zero chat
/// count the daemon genuinely holds. Missing or unreadable documents count
/// as zero rather than failing a read-only diagnostic.
async fn session_message_counts(
    runtime: &Arc<RuntimeThreadManager>,
    sessions_dir: &std::path::Path,
    thread_id: &str,
) -> (usize, usize) {
    match super::thread_history::snapshot_in_runtime(runtime, sessions_dir, thread_id.to_string())
        .await
    {
        Ok((snapshot, _)) => match serde_json::from_value::<
            codewhale_engine::session_manager::SavedSession,
        >(snapshot.session)
        {
            Ok(session) => (session.messages.len(), 0),
            Err(_) => (0, 0),
        },
        Err(_) => (0, 0),
    }
}
