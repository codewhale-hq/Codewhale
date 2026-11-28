use crate::commands::CommandResult;
use crate::tui::app::{App, AppAction};
use codewhale_command_contract::workflow::*;
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::ContentBlock;
use codewhale_models::Role;
use std::path::PathBuf;

use crate::tui::app::TuiOptions;

fn test_app() -> App {
    let options = TuiOptions {
        ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
    };
    App::new(options, &codewhale_runtime::config::Config::default())
}

#[test]
fn auto_sets_auto_review_and_explains_the_trio() {
    let mut app = test_app();
    app.ui_locale = codewhale_localization::Locale::En;
    app.set_agent_approval_posture(ApprovalMode::Suggest);

    let result = auto(&mut app, None);
    assert!(!result.is_error, "{:?}", result.message);
    assert_eq!(app.approval_mode, ApprovalMode::Auto);
    let text = result.message.as_deref().unwrap();
    assert!(text.contains("Auto-Review"));
    assert!(text.contains("/goal"));
    assert!(text.contains("/workflow"));
    assert!(result.action.is_none());
}

#[test]
fn auto_rejects_arguments() {
    let mut app = test_app();
    let result = auto(&mut app, Some("now"));
    assert!(result.is_error);
    assert!(result.message.as_deref().unwrap().contains("Usage: /auto"));
}

#[test]
fn auto_errors_when_managed_auto_review_is_disabled() {
    let mut app = test_app();
    app.auto_review_disabled = true;
    app.set_agent_approval_posture(ApprovalMode::Suggest);

    let result = auto(&mut app, None);
    assert!(result.is_error);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("auto_review.disabled = true")
    );
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
}

#[test]
fn auto_emits_broad_allow_pause_note_only_when_broad_rules_are_suspended() {
    let mut plain_app = test_app();
    plain_app.ui_locale = codewhale_localization::Locale::En;
    plain_app.set_agent_approval_posture(ApprovalMode::Suggest);
    let plain_result = auto(&mut plain_app, None);
    assert!(!plain_result.is_error);
    assert!(
        !plain_result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("Broad allow rules are paused"),
        "{:?}",
        plain_result.message
    );

    let mut broad_app = test_app();
    broad_app.ui_locale = codewhale_localization::Locale::En;
    broad_app.set_agent_approval_posture(ApprovalMode::Suggest);
    broad_app.exec_policy_engine.set_ruleset(
        codewhale_execpolicy::Ruleset::user(vec![], vec![]).with_ask_rules(vec![
            codewhale_execpolicy::ToolAskRule {
                tool: "exec_shell".to_string(),
                command: Some("*".to_string()),
                action: codewhale_execpolicy::PermissionAction::Allow,
                ..codewhale_execpolicy::ToolAskRule::new("")
            },
        ]),
    );
    let broad_result = auto(&mut broad_app, None);
    assert!(!broad_result.is_error);
    let broad_msg = broad_result.message.as_deref().unwrap_or_default();
    assert!(
        broad_msg.contains("Broad allow rules are paused while Auto-Review is on"),
        "{broad_msg}"
    );
}

#[test]
fn ordinary_workflow_is_a_toolless_review_turn() {
    let mut app = test_app();
    let result = workflow(&mut app, Some("audit provider error handling"));
    assert!(!result.is_error);
    let Some(AppAction::WorkflowInstruction {
        display,
        instruction,
    }) = result.action
    else {
        panic!("expected WorkflowInstruction action");
    };
    assert!(display.contains("audit provider error handling"));
    assert!(!display.contains("workflow` tool"));
    assert!(
        display.len() < 80,
        "visible transcript line must stay compact"
    );
    assert!(instruction.starts_with(WORKFLOW_DRAFT_INSTRUCTION_PREFIX));
    assert!(
        instruction.len() < 700,
        "draft instruction grew into a manual"
    );
    assert!(!instruction.contains("parallel()"));
    assert!(!instruction.contains("responseSchema"));

    let queued = crate::tui::app::QueuedMessage::new(display, Some(instruction));
    assert_eq!(
        crate::tui::ui::allowed_tools_for_message(None, &queued),
        Some(Vec::new()),
        "the host, not model compliance, must prevent same-turn execution"
    );
}

#[test]
fn oversized_multibyte_objective_is_bounded_once_and_confirmed_exactly() {
    let mut app = test_app();
    let original = "鲸".repeat(WORKFLOW_OBJECTIVE_MAX_CHARS + 50);
    let drafted = workflow(&mut app, Some(&original));
    let Some(AppAction::WorkflowInstruction {
        display,
        instruction,
    }) = drafted.action
    else {
        panic!("expected WorkflowInstruction action");
    };

    assert!(display.chars().count() <= WORKFLOW_DISPLAY_MAX_CHARS);
    assert!(!display.contains('\n'));
    let draft = envelope_from_instruction(WORKFLOW_DRAFT_INSTRUCTION_PREFIX, &instruction)
        .expect("typed workflow draft envelope");
    let objective = draft.objective.as_deref().expect("bounded objective");
    assert_eq!(objective.chars().count(), WORKFLOW_OBJECTIVE_MAX_CHARS);
    assert!(objective.ends_with('…'));

    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: instruction,
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Reviewed bounded objective and proposed phases.".to_string(),
            cache_control: None,
        }],
    });

    let confirmed = workflow(&mut app, Some("confirm"));
    let Some(AppAction::WorkflowInstruction {
        display,
        instruction,
    }) = confirmed.action
    else {
        panic!("expected confirmed WorkflowInstruction action");
    };
    assert!(display.chars().count() <= WORKFLOW_DISPLAY_MAX_CHARS);
    let confirmed = envelope_from_instruction(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX, &instruction)
        .expect("typed workflow confirmation envelope");
    assert_eq!(confirmed.objective.as_deref(), Some(objective));
}

#[test]
fn confirm_survives_ordinary_messages_between_draft_and_confirm() {
    let mut app = test_app();
    let drafted = workflow(&mut app, Some("audit provider error handling"));
    let Some(AppAction::WorkflowInstruction {
        display,
        instruction,
    }) = drafted.action
    else {
        panic!("expected WorkflowInstruction action");
    };
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: format!("{instruction}\n\n---\n\nUser request: {display}"),
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Objective, phases, workers, and risks. Run /workflow confirm to start."
                .to_string(),
            cache_control: None,
        }],
    });

    // The user types or the model replies again before confirming — the
    // explicit confirm must still find the reviewed draft (regression: the
    // old supersede rule cancelled the draft on any ordinary message and
    // made repeated confirm attempts impossible).
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hmm it won't let me confirm it lol".to_string(),
            cache_control: None,
        }],
    });
    let confirmed = workflow(&mut app, Some("confirm"));
    assert!(!confirmed.is_error, "{:?}", confirmed.message);
    let Some(AppAction::WorkflowInstruction { instruction, .. }) = confirmed.action else {
        panic!("expected confirmed WorkflowInstruction action");
    };
    assert!(instruction.starts_with(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX));

    // A newer DRAFT still supersedes the older one.
    let redrafted = workflow(&mut app, Some("fix the confirm flow"));
    let Some(AppAction::WorkflowInstruction {
        instruction: redraft_instruction,
        display: redraft_display,
    }) = redrafted.action
    else {
        panic!("expected WorkflowInstruction action");
    };
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: format!("{redraft_instruction}\n\n---\n\nUser request: {redraft_display}"),
            cache_control: None,
        }],
    });
    let still_unreviewed = workflow(&mut app, Some("confirm"));
    assert!(
        still_unreviewed.is_error
            && still_unreviewed
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("not been reviewed"),
        "a newer, unreviewed draft must be confirmed only after its review: {:?}",
        still_unreviewed.message
    );
}

#[test]
fn workflow_confirmation_is_a_separate_tool_enabled_turn() {
    let mut app = test_app();
    let drafted = workflow(&mut app, Some("audit provider error handling"));
    let Some(AppAction::WorkflowInstruction {
        display,
        instruction,
    }) = drafted.action
    else {
        panic!("expected WorkflowInstruction action");
    };
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: format!("{instruction}\n\n---\n\nUser request: {display}"),
            cache_control: None,
        }],
    });
    assert!(
        workflow(&mut app, Some("confirm")).is_error,
        "a failed or unfinished draft turn is not a reviewed plan"
    );
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Objective, phases, workers, and risks. Run /workflow confirm to start."
                .to_string(),
            cache_control: None,
        }],
    });

    let confirmed = workflow(&mut app, Some("confirm"));
    assert!(!confirmed.is_error);
    let Some(AppAction::WorkflowInstruction {
        display,
        instruction,
    }) = confirmed.action
    else {
        panic!("expected confirmed WorkflowInstruction action");
    };
    assert!(instruction.starts_with(WORKFLOW_CONFIRM_INSTRUCTION_PREFIX));
    let queued = crate::tui::app::QueuedMessage::new(display, Some(instruction.clone()));
    assert_eq!(
        crate::tui::ui::allowed_tools_for_message(None, &queued),
        None,
        "only the later explicit confirmation restores the normal catalog"
    );

    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: instruction,
            cache_control: None,
        }],
    });
    let replay = workflow(&mut app, Some("confirm"));
    assert!(replay.is_error, "one draft may not be confirmed twice");
}

#[test]
fn workflow_status_and_cancel_answer_from_the_host_without_a_model_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = test_app();
    app.workspace = dir.path().to_path_buf();
    app.current_session_id = Some("workflow-host-test-session".to_string());

    // Nothing has run in this workspace: status is a plain answer, and it
    // must not create the run journal just to say so.
    let result = workflow(&mut app, Some("status"));
    assert!(!result.is_error);
    assert!(
        result.action.is_none(),
        "status must not send a model message"
    );
    assert!(
        result
            .message
            .as_deref()
            .unwrap()
            .contains("No workflow runs")
    );
    assert!(!dir.path().join(".codewhale/workflow-runs.jsonl").exists());

    let result = workflow(&mut app, Some("status wf_missing"));
    assert!(result.is_error);
    assert!(result.action.is_none());

    // A seeded run is listed and described from host state.
    codewhale_engine::tools::workflow::structcopy_test_seed_run(
        dir.path(),
        "workflow_seed",
        app.current_session_id
            .as_deref()
            .expect("test session identity"),
    );
    let result = workflow(&mut app, Some("runs"));
    let text = result.message.unwrap();
    assert!(text.contains("workflow_seed"), "{text}");
    assert!(text.contains("queued"), "{text}");
    assert!(result.action.is_none());

    // Cancel with one active run needs no id and never asks the model.
    // The seeded record has no live controller (no VM ran); cancel still
    // marks the journal cancelled with an honest nothing-live receipt.
    let result = workflow(&mut app, Some("cancel"));
    assert!(result.action.is_none());
    assert!(!result.is_error, "{:?}", result.message);
    let text = result.message.as_deref().unwrap();
    assert!(text.contains("workflow_seed"), "{text}");
    assert!(text.contains("cancelled"), "{text}");
    let after = codewhale_engine::tools::workflow::host_workflow_runs(
        &app.workspace,
        app.current_session_id.as_deref(),
    );
    assert_eq!(
        after
            .iter()
            .find(|line| line.run_id == "workflow_seed")
            .map(|line| line.status),
        Some("cancelled")
    );

    let result = workflow(&mut app, Some("cancel with spaces"));
    assert!(result.is_error);

    let result = workflow(&mut app, Some("help"));
    assert!(result.message.unwrap().contains("/workflow status"));

    // `/workflow run <path>` now drafts a tool-less review. Only a later
    // explicit confirmation may launch the exact saved source.
    let result = workflow(&mut app, Some("run workflows/tiny.workflow.js"));
    let Some(AppAction::WorkflowInstruction {
        display,
        instruction,
    }) = result.action
    else {
        panic!("expected WorkflowInstruction action");
    };
    assert!(display.contains("workflows/tiny.workflow.js"), "{display}");
    let draft = envelope_from_instruction(WORKFLOW_DRAFT_INSTRUCTION_PREFIX, &instruction)
        .expect("typed saved-workflow draft");
    assert_eq!(
        draft.source_path.as_deref(),
        Some("workflows/tiny.workflow.js")
    );
    let queued = crate::tui::app::QueuedMessage::new(display, Some(instruction.clone()));
    assert_eq!(
        crate::tui::ui::allowed_tools_for_message(None, &queued),
        Some(Vec::new())
    );

    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: instruction,
            cache_control: None,
        }],
    });
    app.api_messages_mut().push(codewhale_models::Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Saved Workflow path and risks reviewed.".to_string(),
            cache_control: None,
        }],
    });
    let confirmed = workflow(&mut app, Some("confirm"));
    let Some(AppAction::WorkflowInstruction { instruction, .. }) = confirmed.action else {
        panic!("expected confirmed saved Workflow action");
    };
    assert!(instruction.contains("`source_path`"), "{instruction}");
    assert!(
        instruction.contains("workflows/tiny.workflow.js"),
        "{instruction}"
    );

    assert!(workflow(&mut app, Some("run ../outside.workflow.js")).is_error);
    assert!(workflow(&mut app, Some("run /tmp/outside.workflow.js")).is_error);
}

#[test]
fn workflows_opens_the_run_dashboard_and_keeps_host_verbs_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = test_app();
    app.workspace = dir.path().to_path_buf();
    app.current_session_id = Some("workflow-host-test-session".to_string());

    // Bare `/workflows` opens the dashboard: a host action, never a
    // model turn — observation carries no orchestration authority.
    let result = workflows(&mut app, None);
    assert!(!result.is_error);
    assert!(matches!(
        result.action,
        Some(AppAction::OpenWorkflowsManager)
    ));

    // Host control verbs still answer inline (the old alias surface).
    let result = workflows(&mut app, Some("status"));
    assert!(!result.is_error);
    assert!(
        result.action.is_none(),
        "status must not send a model message"
    );
    assert!(
        result
            .message
            .as_deref()
            .unwrap()
            .contains("No workflow runs")
    );

    // Orchestration attempts are redirected to /workflow, the only
    // surface that carries launch authority.
    let result = workflows(&mut app, Some("audit provider errors"));
    assert!(result.is_error);
    assert!(result.action.is_none());
    assert!(result.message.unwrap().contains("never launches"));
}

#[test]
fn workflow_settings_explains_the_session_table() {
    let mut app = test_app();
    app.workflow_config.automatic = false;
    app.workflow_config.require_approval_for_writes = false;
    app.goal_max_continuations = 25;
    let result = workflow(&mut app, Some("settings"));
    assert!(result.action.is_none());
    let text = result.message.unwrap();
    assert!(text.contains("automatic = off"), "{text}");
    assert!(text.contains("require_approval_for_writes = off"), "{text}");
    assert!(text.contains("max_continuations = 25"), "{text}");
}

#[test]
fn workflow_settings_and_tool_share_a_refreshed_session_table() {
    use codewhale_engine::tools::spec::{ApprovalRequirement, ToolContext, ToolSpec};
    use codewhale_engine::tools::subagent::{SubAgentRuntime, new_shared_subagent_manager};
    use codewhale_engine::tools::workflow::WorkflowTool;
    use serde_json::json;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = test_app();
    app.workspace = dir.path().to_path_buf();

    let mut table = app.workflow_config.clone();
    table.automatic = false;
    table.require_approval_for_writes = false;
    table.auto_start_read_only = false;
    codewhale_engine::tools::workflow::set_session_workflow_config(&app.workspace, table.clone());
    app.workflow_config = table;

    let result = workflow(&mut app, Some("settings"));
    assert!(result.action.is_none());
    let text = result.message.unwrap();
    assert!(text.contains("automatic = off"), "{text}");
    assert!(text.contains("require_approval_for_writes = off"), "{text}");
    assert!(text.contains("auto_start_read_only = off"), "{text}");

    let ctx = ToolContext::new(dir.path().to_path_buf());
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 2);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = codewhale_engine::client::CodewhaleClient::new(
        &codewhale_runtime::config::Config::default()
            .with_legacy_root(Some("test-key".to_string()), None),
    )
    .expect("stub client");
    let mut runtime = SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".to_string(),
        ctx,
        true,
        None,
        manager.clone(),
    );
    // Stale snapshot: product defaults still require write approval.
    runtime.api_config = Some(std::sync::Arc::new(
        codewhale_runtime::config::Config::default(),
    ));
    let tool = WorkflowTool::new(manager, runtime);

    let write_plan = json!({
        "action": "start",
        "plan": {
            "goal": "write freely",
            "risk": "writes",
            "children": [{ "prompt": "edit", "type": "implementer" }]
        }
    });
    let read_only = json!({
        "action": "start",
        "plan": {
            "goal": "scout crates",
            "risk": "read_only",
            "children": [{ "prompt": "look", "type": "explore" }]
        }
    });
    assert_eq!(
        tool.approval_requirement_for(&write_plan),
        ApprovalRequirement::Auto,
        "refreshed require_approval_for_writes = false must win over the stale runtime snapshot"
    );
    assert_eq!(
        tool.approval_requirement_for(&read_only),
        ApprovalRequirement::Required,
        "refreshed auto_start_read_only = false must still ask"
    );
}

fn workflow(app: &mut App, arg: Option<&str>) -> CommandResult {
    crate::commands::execute(&format!("/workflow {}", arg.unwrap_or_default()), app)
}

fn workflows(app: &mut App, arg: Option<&str>) -> CommandResult {
    crate::commands::execute(&format!("/workflows {}", arg.unwrap_or_default()), app)
}

fn auto(app: &mut App, arg: Option<&str>) -> CommandResult {
    crate::commands::execute(&format!("/auto {}", arg.unwrap_or_default()), app)
}
