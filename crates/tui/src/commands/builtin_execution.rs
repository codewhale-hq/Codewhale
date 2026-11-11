//! Remote built-in command execution over the engine's host snapshot.
//!
//! The app-server assembles a [`BuiltinCommandHostSnapshot`] from thread
//! state and calls [`execute_builtin`] through the installed catalog. This
//! module is the terminal host's side: it maps the snapshot into the command
//! contract's facet envelope and runs the portable debug handlers.
//!
//! Phase 1 serves exactly the read-only diagnostics commands whose every
//! data path the snapshot backs faithfully (`/system`, `/tokens`, `/cost`).
//! Two gates keep the surface honest: the [`SERVED`] allowlist, and the
//! handler-declared capability mask checked against [`PROVIDED`]. Commands
//! outside both are refused by returning `None`, which the API renders as
//! an unknown command rather than a degraded answer.

use codewhale_command_contract::facets::{
    CommandDebugDiagnosticsContext, CommandPresentationContext, DebugBalanceProjection,
    DebugCacheCall, DebugCacheInspectionUnavailable, DebugCacheRates, DebugCacheTelemetry,
    DebugCostProjection, DebugPromptContext, DebugPromptSourceMap, DebugSystemProjection,
    DebugTokenProjection, DebugToolSnapshot,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_engine::command_catalog::{BuiltinCommandExecution, BuiltinCommandHostSnapshot};
use codewhale_localization::{Locale, tr};

use crate::commands::contract::{apply_named_replacements, resolve_message_id};
use codewhale_commands::debug as portable_debug;

/// Facets this remote host can honestly provide from the snapshot.
const PROVIDED: CommandCapabilities =
    CommandCapabilities::DEBUG_DIAGNOSTICS.union(CommandCapabilities::PRESENTATION);

/// The phase-1 served set: read-only diagnostics whose every projection the
/// snapshot carries. Widening this list requires widening the snapshot with
/// faithfully assembled data first — never an adapter that guesses.
const SERVED: [&str; 3] = ["system", "tokens", "cost"];

// ---------------------------------------------------------------------------
// Facet adapters over the snapshot
// ---------------------------------------------------------------------------

struct SnapshotDiagnostics<'a> {
    snapshot: &'a BuiltinCommandHostSnapshot,
}

impl CommandDebugDiagnosticsContext for SnapshotDiagnostics<'_> {
    /// Never served remotely: the provider-balance probe is a host action,
    /// and the allowlist keeps `/balance` off this surface. The projection
    /// states what the snapshot genuinely knows: nothing.
    fn balance_projection(&self) -> DebugBalanceProjection {
        DebugBalanceProjection {
            provider_display_name: "unavailable".to_string(),
            supports_balance_api: false,
        }
    }

    fn system_projection(&self) -> DebugSystemProjection {
        DebugSystemProjection {
            mode_label: self.snapshot.mode_label.clone(),
            prompt: self.snapshot.system_prompt.clone(),
        }
    }

    fn token_projection(&self) -> DebugTokenProjection {
        self.snapshot.tokens.clone()
    }

    fn cost_projection(&self) -> DebugCostProjection {
        self.snapshot.tokens.cost.clone()
    }

    /// Assembled prompt-layer telemetry is a terminal-host report; the
    /// remote surface does not serve `/context`, so this stays an explicitly
    /// empty map rather than a partial reconstruction.
    fn context_source_map(&self) -> DebugPromptSourceMap {
        DebugPromptSourceMap {
            entries: Vec::new(),
            total_estimated_tokens: 0,
            active_context_estimated_tokens: 0,
            overflow_guard_estimated_tokens: None,
            context_window_tokens: None,
            context_window_source: None,
            budget_used_percent: None,
            pressure_label: String::new(),
            context_window_verified: false,
            generated_at: String::new(),
            note: "not assembled over the remote command surface".to_string(),
        }
    }

    fn prompt_context(&self) -> DebugPromptContext {
        DebugPromptContext {
            schema_version: 0,
            provider: String::new(),
            model: self.snapshot.model.clone(),
            system_prompt_state: String::new(),
            tool_catalog_state: String::new(),
            sections: Vec::new(),
            tools: Vec::new(),
            source_map: self.context_source_map(),
        }
    }

    fn tool_snapshot(&self) -> Option<DebugToolSnapshot> {
        None
    }

    fn inspect_cache(
        &self,
    ) -> Result<
        codewhale_command_contract::DebugCacheInspectionObservation,
        DebugCacheInspectionUnavailable,
    > {
        Err(DebugCacheInspectionUnavailable::NoConcreteRoute)
    }

    fn remember_cache_inspection(
        &mut self,
        _inspection: codewhale_command_contract::facets::DebugPromptInspection,
    ) {
        // Read-only surface: nothing to remember between remote executions.
    }

    /// The remote surface does not serve `/cache`; the honest encoding of
    /// "not assembled" is absent telemetry, never zeros dressed as data.
    fn cache_telemetry(&self) -> DebugCacheTelemetry {
        DebugCacheTelemetry {
            model: self.snapshot.model.clone(),
            session_cache_rates: DebugCacheRates::default(),
            history: Vec::new(),
            calls: Vec::<DebugCacheCall>::new(),
            calls_total: 0,
            calls_currency: self.snapshot.tokens.cost.currency,
            calls_legacy_coverage_unknown: false,
            history_capacity: 0,
            prefix_stability_pct: None,
            prefix_checks_total: 0,
            prefix_change_count: 0,
            prefix_drift_count: 0,
            prefix_context_updates: 0,
            prefix_pin_reason: None,
            prefix_last_miss_reason: None,
            last_prefix_change_desc: None,
            last_pinned_prefix_hash: None,
            api_message_count: self.snapshot.tokens.api_message_count,
            non_system_message_count: self.snapshot.tokens.chat_message_count,
        }
    }
}

struct SnapshotPresentation {
    locale: Locale,
}

impl CommandPresentationContext for SnapshotPresentation {
    fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String> {
        let message_id = resolve_message_id(key).ok_or("unknown translation key")?;
        let template = tr(self.locale, message_id);
        apply_named_replacements(&template, replacements)
            .ok_or_else(|| "invalid translation replacement contract".to_string())
    }

    fn language_name(&self) -> &'static str {
        self.locale.translation_target_name()
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Execute one built-in command against the snapshot, or `None` when the
/// command is outside the remote surface.
pub fn execute_builtin(
    name: &str,
    args: Option<&str>,
    snapshot: &BuiltinCommandHostSnapshot,
) -> Option<BuiltinCommandExecution> {
    let (info, handler) = resolve(name)?;
    let CommandHandler::Contextual {
        capabilities,
        handler: run,
    } = handler
    else {
        return None;
    };
    // The capability mask is the second gate: a handler that grew a new
    // facet requirement is refused until the snapshot grows the data for it.
    if !PROVIDED.contains(capabilities) {
        return None;
    }

    let mut diagnostics = SnapshotDiagnostics { snapshot };
    let mut presentation = SnapshotPresentation {
        locale: snapshot.locale,
    };
    let contexts = CommandContexts::empty()
        .with_debug_diagnostics(&mut diagnostics)
        .with_presentation(&mut presentation);
    let result: portable_debug::CommandResult = run(contexts, args);
    Some(BuiltinCommandExecution {
        name: info.name.to_string(),
        text: result.message.unwrap_or_default(),
        is_error: result.is_error,
        text_only: result.action.is_none(),
    })
}

/// Whether `name` is on the remote surface at all. Shares [`resolve`] with
/// [`execute_builtin`], so the two gates can never disagree about a name.
pub fn serves_builtin(name: &str) -> bool {
    resolve(name).is_some()
}

/// The portable handler a raised spelling (canonical name or alias) names,
/// gated by the [`SERVED`] allowlist and the [`PROVIDED`] capability mask.
fn resolve(
    name: &str,
) -> Option<(
    &'static codewhale_command_contract::metadata::CommandInfo,
    CommandHandler<portable_debug::CommandResult>,
)> {
    let spelling = name.trim_start_matches('/').to_ascii_lowercase();
    let (info, handler) = portable_debug::portable_handlers()
        .into_iter()
        .find(|(info, _)| {
            info.name == spelling.as_str() || info.aliases.contains(&spelling.as_str())
        })?;
    // The allowlist gates on the canonical name, so an alias resolves to the
    // same served command its canonical spelling is.
    if !SERVED.contains(&info.name) {
        return None;
    }
    // A non-contextual handler asks nothing of the host, so it has nothing to
    // read and is refused here rather than served an empty envelope.
    if !matches!(handler, CommandHandler::Contextual { capabilities, .. } if PROVIDED.contains(capabilities))
    {
        return None;
    }
    Some((info, handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::facets::DebugSystemPrompt;
    use codewhale_command_contract::types::{CommandCurrency, CommandMode};

    fn snapshot() -> BuiltinCommandHostSnapshot {
        BuiltinCommandHostSnapshot {
            mode: CommandMode::Agent,
            mode_label: "WORK".to_string(),
            locale: Locale::En,
            model: "deepseek-v4-pro".to_string(),
            system_prompt: DebugSystemPrompt::Text("You are helpful.".to_string()),
            tokens: DebugTokenProjection {
                active_context_used: 4_200,
                context_window: 131_072,
                last_input: Some(3_900),
                last_output: Some(312),
                cache_hit: Some(3_400),
                cache_miss: Some(500),
                total_tokens: 21_000,
                cache_write_tokens: 640,
                api_message_count: 12,
                chat_message_count: 8,
                model: "deepseek-v4-pro".to_string(),
                cost: DebugCostProjection {
                    currency: CommandCurrency::Usd,
                    total: 0.012_5,
                    parent_turns: 0.012,
                    subagents: 0.000_5,
                    display_floor: 0.0,
                    priced_turns: 6,
                    unpriced_turns: 0,
                    legacy_coverage_unknown: false,
                    user_declared_estimates: false,
                    itemized_turns: 6,
                    route_amounts: Vec::new(),
                    turn_history_capacity: 40,
                    unpriced_reason_labels: Vec::new(),
                    unpriced_classes: Vec::new(),
                    pricing_provenances: Vec::new(),
                    live_pricing_defects: Vec::new(),
                    unusable_pricing_defects: Vec::new(),
                    route_receipts: Vec::new(),
                },
            },
        }
    }

    #[test]
    fn serves_system_with_mode_label_and_prompt() {
        let execution = execute_builtin("system", None, &snapshot()).expect("served");
        assert_eq!(execution.name, "system");
        assert!(!execution.is_error);
        assert!(execution.text_only);
        assert!(execution.text.contains("WORK"));
        assert!(execution.text.contains("You are helpful."));
    }

    #[test]
    fn serves_tokens_with_published_usage() {
        let execution = execute_builtin("/tokens", None, &snapshot()).expect("served");
        assert_eq!(execution.name, "tokens");
        assert!(execution.text.contains("21,000") || execution.text.contains("21000"));
        assert!(execution.text.contains("deepseek-v4-pro"));
    }

    #[test]
    fn serves_cost_with_the_priced_subtotal() {
        let execution = execute_builtin("cost", None, &snapshot()).expect("served");
        assert!(!execution.is_error);
        assert!(!execution.text.is_empty());
    }

    #[test]
    fn resolves_engine_aliases_to_the_canonical_command() {
        let execution = execute_builtin("xitong", None, &snapshot()).expect("served");
        assert_eq!(execution.name, "system");
    }

    #[test]
    fn the_served_gate_agrees_with_execution_for_every_spelling() {
        // The route checks `serves_builtin` before assembling any thread
        // state, then calls `execute_builtin`: a name the first gate admits
        // must never be refused by the second, and vice versa.
        let snapshot = snapshot();
        let mut spellings: Vec<String> = SERVED.iter().map(|name| (*name).to_string()).collect();
        for (info, _) in portable_debug::portable_handlers() {
            spellings.push(info.name.to_string());
            spellings.extend(info.aliases.iter().map(|alias| (*alias).to_string()));
        }
        spellings.push("nope".to_string());
        for name in spellings {
            for raised in [name.clone(), format!("/{name}"), name.to_uppercase()] {
                let served = serves_builtin(&raised);
                let executed = execute_builtin(&raised, None, &snapshot).is_some();
                assert_eq!(served, executed, "{raised}");
            }
        }
    }

    #[test]
    fn refuses_commands_outside_the_phase1_allowlist() {
        // /balance is a host action (FetchBalance), /cache needs assembled
        // telemetry, /undo needs the undo facet: none are honestly served.
        for name in [
            "balance", "cache", "undo", "retry", "context", "tools", "nope",
        ] {
            assert!(execute_builtin(name, None, &snapshot()).is_none(), "{name}");
        }
    }
}
