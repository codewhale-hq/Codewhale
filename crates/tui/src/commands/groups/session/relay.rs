//! `/relay` command — portable handler over the session-control facet.

use std::fmt::Write as _;

use super::CommandResult;
use codewhale_command_contract::facets::{
    CommandSessionControlContext, PlanProjection, PlanStepStatus, RelayProjection,
};
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub(in crate::commands) struct RelayCmd;

// ---------------------------------------------------------------------------
// FEAT-024 Phase 4 (D4/D6/D7): portable contextual registration and handler.
// The handler owns complete relay-instruction composition (sections, labels,
// list formatting, focus normalization, byte-identical text); the facet
// supplies the semantic projection. Missing control authority fails safely
// with the exact capability error (never a panic).
// ---------------------------------------------------------------------------

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "relay",
    aliases: &["batonpass", "接力"],
    usage: "/relay [focus]",
    description_key: "cmd_relay_description",
};

impl ContractRegisterCommand<CommandResult> for RelayCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::SESSION_CONTROL,
            handler: relay_contextual,
        }
    }
}

pub(in crate::commands) fn relay_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(control) = parts.control.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: session_control".to_string());
    };
    relay_portable(control, arg)
}

pub(in crate::commands) fn relay_portable(
    control: &mut dyn CommandSessionControlContext,
    arg: Option<&str>,
) -> CommandResult {
    let focus = arg.map(str::trim).filter(|value| !value.is_empty());
    let projection = control.relay_projection();
    let status = format!("Preparing session relay at {}...", projection.handoff_path);
    let message = build_relay_instruction(projection, focus);
    CommandResult::with_message_and_action(
        status,
        codewhale_command_contract::outcome::SessionAction::SendMessage(message),
    )
}

/// Compose the relay instruction from the portable snapshot.
///
/// The relay is written to `projection.handoff_path`, the path the next
/// session reads first. A relay written to the legacy location is ignored
/// whenever a file exists at that path.
fn build_relay_instruction(projection: RelayProjection, focus: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Create a compact session relay (接力) for a future Codewhale thread."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Write or update `{}`.", projection.handoff_path);
    let _ = writeln!(
        out,
        "Write exactly this path so the next session loads it, and title the artifact `# Session relay`."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Use this relay structure:");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", projection.compact_template.trim());
    let _ = writeln!(out);
    let _ = writeln!(out, "Current session snapshot:");
    let _ = writeln!(out, "- Workspace: {}", projection.workspace);
    let _ = writeln!(out, "- Mode: {}", projection.mode);
    let _ = writeln!(out, "- Model: {}", projection.model);
    if let Some(focus) = focus {
        let _ = writeln!(out, "- Requested relay focus: {focus}");
    }
    if let Some(objective) = projection.goal_objective.as_deref() {
        let _ = writeln!(out, "- Goal objective: {objective}");
    }
    if let Some(budget) = projection.goal_token_budget {
        let _ = writeln!(out, "- Goal token budget: {budget}");
    }
    match projection.todos {
        codewhale_command_contract::facets::TodoProjection::Body(body) => {
            let _ = writeln!(out, "\nCurrent To-do:");
            let _ = writeln!(out, "{body}");
        }
        codewhale_command_contract::facets::TodoProjection::Absent => {}
        codewhale_command_contract::facets::TodoProjection::Unavailable => {
            let _ = writeln!(out, "\nTo-do: unavailable because the list is busy.");
        }
    }
    match projection.plan {
        PlanProjection::Sections(sections) => {
            let _ = writeln!(
                out,
                "\nConversational strategy notes from update_plan (reasoning context, not a Work surface):"
            );
            write_plan_field(&mut out, "Title", sections.title.as_deref());
            write_plan_field(&mut out, "Objective", sections.objective.as_deref());
            write_plan_field(&mut out, "Context", sections.context_summary.as_deref());
            write_plan_field(&mut out, "Explanation", sections.explanation.as_deref());
            write_plan_list(&mut out, "Source", &sections.sources_used);
            write_plan_list(&mut out, "Critical file", &sections.critical_files);
            write_plan_list(&mut out, "Constraint", &sections.constraints);
            write_plan_field(
                &mut out,
                "Recommended approach",
                sections.recommended_approach.as_deref(),
            );
            write_plan_field(
                &mut out,
                "Verification plan",
                sections.verification_plan.as_deref(),
            );
            write_plan_field(
                &mut out,
                "Risks and unknowns",
                sections.risks_and_unknowns.as_deref(),
            );
            write_plan_field(
                &mut out,
                "Handoff packet",
                sections.handoff_packet.as_deref(),
            );
            write_plan_list(&mut out, "Replan if", &sections.replan_if);
            for item in sections.items {
                let _ = writeln!(
                    out,
                    "- [{}] {}",
                    plan_step_status_label(item.status),
                    item.text
                );
            }
        }
        PlanProjection::Absent => {}
        PlanProjection::Busy => {
            let _ = writeln!(
                out,
                "\nStrategy metadata: unavailable because plan state is busy."
            );
        }
    }
    let _ = writeln!(
        out,
        "\nBefore writing, inspect the current transcript context and any live tool evidence you need. Do not invent test results, file changes, blockers, or decisions."
    );
    let _ = writeln!(
        out,
        "\nKeep it under about 900 words unless the session genuinely needs more. After writing, report the path and the single next action."
    );
    out
}

fn plan_step_status_label(status: PlanStepStatus) -> &'static str {
    match status {
        PlanStepStatus::Pending => "pending",
        PlanStepStatus::InProgress => "in_progress",
        PlanStepStatus::Completed => "completed",
    }
}

fn write_plan_field(out: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        let _ = writeln!(out, "- {label}: {value}");
    }
}

fn write_plan_list(out: &mut String, label: &str, values: &[String]) {
    for value in values {
        let value = value.trim();
        if !value.is_empty() {
            let _ = writeln!(out, "- {label}: {value}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::control_test_support::message;
    use super::*;
    use codewhale_command_contract::facets::{PlanSections, PlanStep, TodoProjection};

    #[test]
    fn relay_composes_exact_instruction_and_action() {
        let mut fake = super::super::control_test_support::FakeControl {
            relay: Some(super::super::control_test_support::relay_projection_fixture()),
            ..super::super::control_test_support::FakeControl::default()
        };
        let result = relay_portable(&mut fake, Some("focus on the handoff"));
        assert!(!result.is_error);
        let message = match result.action {
            Some(codewhale_command_contract::outcome::SessionAction::SendMessage(message)) => {
                message
            }
            other => panic!("expected SendMessage action, got {other:?}"),
        };
        assert!(
            message
                .contains("Create a compact session relay (接力) for a future Codewhale thread.")
        );
        assert!(message.contains("- Workspace: /work"));
        assert!(message.contains("- Mode: operate"));
        assert!(message.contains("- Model: model-x"));
        assert!(message.contains("- Requested relay focus: focus on the handoff"));
        assert!(message.contains("- Goal objective: objective-y"));
        assert!(message.contains("- Goal token budget: 900"));
        assert!(message.contains("\n\nWrite or update `.codewhale/handoff.md`.\nWrite exactly this path so the next session loads it, and title the artifact `# Session relay`.\n"));
        assert!(!message.contains(".deepseek"));
        assert!(message.contains("Before writing, inspect the current transcript context"));
        assert!(message.contains("Keep it under about 900 words"));
        assert!(!message.contains("Current To-do:"));
        assert!(!message.contains("strategy notes"));
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|m| m == "Preparing session relay at .codewhale/handoff.md...")
        );
        assert_eq!(fake.calls.borrow().as_slice(), ["relay_projection"]);
    }

    #[test]
    fn relay_names_the_path_the_host_reads_back() {
        let mut fake = super::super::control_test_support::FakeControl {
            relay: Some(RelayProjection {
                handoff_path: "state/relay.md".to_string(),
                ..super::super::control_test_support::relay_projection_fixture()
            }),
            ..super::super::control_test_support::FakeControl::default()
        };
        let result = relay_portable(&mut fake, None);
        assert_eq!(
            message(&result),
            "Preparing session relay at state/relay.md..."
        );
        let instruction = match result.action {
            Some(codewhale_command_contract::outcome::SessionAction::SendMessage(message)) => {
                message
            }
            other => panic!("expected SendMessage, got {other:?}"),
        };
        assert!(instruction.contains("Write or update `state/relay.md`."));
        assert!(!instruction.contains("handoff.md"));
    }

    #[test]
    fn relay_render_optional_sections_and_busy_states_exactly() {
        let mut fake = super::super::control_test_support::FakeControl {
            relay: Some(RelayProjection {
                compact_template: "# Session relay".to_string(),
                handoff_path: ".codewhale/handoff.md".to_string(),
                workspace: "/work".to_string(),
                mode: "operate".to_string(),
                model: "model-x".to_string(),
                goal_objective: None,
                goal_token_budget: None,
                todos: TodoProjection::Unavailable,
                plan: codewhale_command_contract::facets::PlanProjection::Busy,
            }),
            ..super::super::control_test_support::FakeControl::default()
        };
        let result = relay_portable(&mut fake, None);
        let message = match result.action {
            Some(codewhale_command_contract::outcome::SessionAction::SendMessage(message)) => {
                message
            }
            other => panic!("expected SendMessage, got {other:?}"),
        };
        assert!(message.contains("\nTo-do: unavailable because the list is busy."));
        assert!(message.contains("\nStrategy metadata: unavailable because plan state is busy."));
        assert!(!message.contains("Requested relay focus"));
        assert!(!message.contains("Goal objective"));
        assert!(!message.contains("Goal token budget"));
    }

    #[test]
    fn relay_renders_plan_sections_with_exact_labels() {
        let mut fake = super::super::control_test_support::FakeControl {
            relay: Some(RelayProjection {
                compact_template: "template".to_string(),
                handoff_path: ".codewhale/handoff.md".to_string(),
                workspace: "/w".to_string(),
                mode: "m".to_string(),
                model: "mo".to_string(),
                goal_objective: None,
                goal_token_budget: None,
                todos: TodoProjection::Absent,
                plan: codewhale_command_contract::facets::PlanProjection::Sections(PlanSections {
                    title: Some("Relay Plan".to_string()),
                    explanation: Some("  because  ".to_string()),
                    sources_used: vec!["repo-a".to_string(), "  ".to_string()],
                    replan_if: vec!["abort on schema mismatch".to_string(), "  ".to_string()],
                    items: vec![PlanStep {
                        status: PlanStepStatus::InProgress,
                        text: "port relay".to_string(),
                    }],
                    ..PlanSections::default()
                }),
            }),
            ..super::super::control_test_support::FakeControl::default()
        };
        let result = relay_portable(&mut fake, None);
        let message = match result.action {
            Some(codewhale_command_contract::outcome::SessionAction::SendMessage(message)) => {
                message
            }
            other => panic!("expected SendMessage, got {other:?}"),
        };
        assert!(message.contains("- Title: Relay Plan"));
        assert!(message.contains("- Explanation: because"));
        assert!(message.contains("- Source: repo-a"));
        assert!(message.contains("- Replan if: abort on schema mismatch"));
        assert!(message.contains("- [in_progress] port relay"));
    }

    #[test]
    fn relay_owns_every_portable_plan_status_label() {
        assert_eq!(plan_step_status_label(PlanStepStatus::Pending), "pending");
        assert_eq!(
            plan_step_status_label(PlanStepStatus::InProgress),
            "in_progress"
        );
        assert_eq!(
            plan_step_status_label(PlanStepStatus::Completed),
            "completed"
        );
    }

    #[test]
    fn relay_missing_control_authority_fails_safely() {
        let contexts = codewhale_command_contract::handler::CommandContexts::empty();
        let result = relay_contextual(contexts, None);
        assert!(result.is_error);
        assert_eq!(
            message(&result),
            "Command capability unavailable: session_control"
        );
    }
}
