//! Effort / Thinking slash commands — `/effort` and `/thinking`.
//!
//! Lets users change the model's reasoning effort via slash command as well as `Ctrl+T`.
//! Mirrors the Muse Spark UX where `/effort [<none|minimal|low|medium|high|xhigh|ultra>]`
//! is offered via autocomplete. Both `/effort` and `/thinking` are aliases for the same
//! underlying `ReasoningEffort` state, keeping `Thinking` as the canonical UI label
//! (per user request to keep it as Thinking and fix for everything).

use crate::commands::{CommandResult, traits::CommandInfo};
use crate::tui::app::App;
use codewhale_engine::reasoning_preference::ReasoningEffort;
use codewhale_localization::{MessageId, tr};

pub const EFFORT_INFO: CommandInfo = CommandInfo {
    name: "effort",
    aliases: &["thinking", "reasoning", "reason"],
    usage: "/effort [<none|minimal|low|medium|high|xhigh|ultra|max|auto|off>]",
    description_id: MessageId::CmdEffortDescription,
};

pub fn effort(app: &mut App, args: Option<&str>) -> CommandResult {
    let arg = args.unwrap_or("").trim().to_ascii_lowercase();
    // Determine available efforts for the current model (like model picker does)
    let available: Vec<ReasoningEffort> = {
        let provider = app.api_provider;
        let base_url = app.active_route_base_url.clone();
        // Try to get the current wire model; fallback to app.model
        let wire_model = crate::tui::model_picker::picker_efforts_for_route(
            provider,
            &base_url,
            &app.model,
            app.auto_model,
        );
        // If picker returns empty, fallback to the model's catalog efforts
        if !wire_model.is_empty() {
            wire_model
        } else {
            vec![]
        }
    };
    let available_labels: Vec<String> = if available.is_empty() {
        vec![
            "none".into(),
            "minimal".into(),
            "low".into(),
            "medium".into(),
            "high".into(),
            "xhigh".into(),
            "ultra".into(),
            "max".into(),
            "auto".into(),
            "off".into(),
        ]
    } else {
        available
            .iter()
            .map(|e| e.display_label_for_provider(app.api_provider).to_string())
            .collect()
    };
    if arg.is_empty() {
        // No arg: same path as Ctrl+T / the hotbar so cycle, persist, and
        // receipts cannot drift by command surface.
        let prev = app.reasoning_effort;
        let _ = app.cycle_effort();
        return CommandResult::message(format!(
            "Effort: {} → {} (available: {})",
            prev.display_label_for_provider(app.api_provider),
            app.reasoning_effort
                .display_label_for_provider(app.api_provider),
            available_labels.join("|")
        ));
    }
    let Ok(effort) = ReasoningEffort::parse_strict(&arg) else {
        let hint = format!(
            "Usage: /effort <{}> (also /thinking)",
            available_labels.join("|")
        );
        return CommandResult::error(format!(
            "{} — {}",
            tr(app.ui_locale, MessageId::CmdEffortDescription),
            hint
        ));
    };
    let requested = if app.auto_model {
        effort
    } else {
        effort.normalize_for_route(app.api_provider, &app.active_route_base_url, &app.model)
    };
    if !available.is_empty() && !available.contains(&requested) && !available.contains(&effort) {
        return CommandResult::error(format!(
            "'{}' not available for {} — available: {}",
            arg,
            app.model,
            available_labels.join("|")
        ));
    }
    app.commit_reasoning_effort(requested);
    CommandResult::message(format!(
        "Effort set to {}",
        requested.display_label_for_provider(app.api_provider)
    ))
}
