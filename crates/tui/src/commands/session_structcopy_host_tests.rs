//! Real-host preservation fixtures, captured before FEAT-026 production edits.
//! These fixtures stay outside the portable session command closure.
use super::{CommandResult, execute};
use crate::config::Config;
use crate::tools::plan::{PlanItemArg, StepStatus, UpdatePlanArgs};
use crate::tui::app::{App, TuiOptions};
use crate::tui::clipboard::ClipboardHandler;
use codewhale_localization::{Locale, tr};
use codewhale_models::{ContentBlock, ImageUrlContent, Message, Role, ToolCaller};
use serde_json::{Value, json};
use tempfile::TempDir;

fn app(temp: &TempDir) -> App {
    let options = TuiOptions {
        skills_dir: temp.path().join("skills"),
        memory_path: temp.path().join("memory.md"),
        notes_path: temp.path().join("notes.txt"),
        mcp_config_path: temp.path().join("mcp.json"),
        ..crate::tui::test_support::test_tui_options(temp.path())
    };
    let mut app = App::new(options, &Config::default());
    app.ui_locale = Locale::En;
    app.current_session_id = Some("structcopy-baseline-session".into());
    app
}

fn call(name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        execution_id: None,
        id: "call-golden".into(),
        name: name.into(),
        input: json!({"url":"https://alice:secret@example.test/path?token=x#frag", "api_key":"secret-value"}),
        caller: Some(ToolCaller {
            caller_type: "code_execution_20250825".into(),
            tool_id: None,
        }),
        thought_signature: Some("private-call-signature".into()),
    }
}

fn result(content: &str, is_error: Option<bool>) -> ContentBlock {
    ContentBlock::ToolResult {
        execution_id: None,
        tool_use_id: "call-golden".into(),
        content: content.into(),
        is_error,
        content_blocks: Some(vec![
            json!({"type":"image","mime_type":"image/png","data":"private-base64"}),
            json!({"type":"text","text":"visible tool text"}),
        ]),
    }
}

fn seed(app: &mut App) {
    app.api_messages = std::sync::Arc::new(vec![
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text {
                text: "hidden system instruction".into(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "visible text".into(),
                    cache_control: None,
                },
                ContentBlock::Thinking {
                    thinking: "hidden reasoning".into(),
                    signature: Some("hidden-signature".into()),
                    state: None,
                },
                call("first-call"),
                result("first-result", Some(false)),
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: "data:image/png;base64,private".into(),
                    },
                },
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: "https://a:b@example.test/image.png?q=x#y".into(),
                    },
                },
                ContentBlock::ServerToolUse {
                    id: "server-only".into(),
                    name: "server".into(),
                    input: json!({"x":1}),
                },
                ContentBlock::ToolSearchToolResult {
                    tool_use_id: "search".into(),
                    content: json!({"matches":["one"]}),
                },
                ContentBlock::CodeExecutionToolResult {
                    tool_use_id: "exec".into(),
                    content: json!({"stdout":"ok"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![result("last-result", None), call("last-call")],
        },
    ]);
    app.plan_state.try_lock().unwrap().update(UpdatePlanArgs {
        title: Some("Golden plan".into()),
        objective: Some("Preserve output".into()),
        context_summary: Some("Context".into()),
        explanation: Some("Explanation".into()),
        sources_used: vec!["source".into()],
        critical_files: vec!["src/file.rs".into()],
        constraints: vec!["read only".into()],
        recommended_approach: Some("typed observations".into()),
        verification_plan: Some("golden comparison".into()),
        risks_and_unknowns: Some("unknown remains null".into()),
        handoff_packet: Some("ready".into()),
        plan: vec![
            PlanItemArg {
                step: "first".into(),
                status: StepStatus::Pending,
            },
            PlanItemArg {
                step: "second".into(),
                status: StepStatus::InProgress,
            },
            PlanItemArg {
                step: "third".into(),
                status: StepStatus::Completed,
            },
        ],
    });
}

fn observation(result: CommandResult, app: &App) -> Value {
    assert!(
        result.action.is_none(),
        "structcopy must never emit an action"
    );
    json!({"message":result.message,"is_error":result.is_error,"clipboard":app.clipboard.last_written_text()})
}

fn dispatch(app: &mut App, command: &str) -> Value {
    app.clipboard = ClipboardHandler::new();
    let before_messages = app.api_messages.clone();
    let before_plan = app.plan_state.try_lock().unwrap().snapshot();
    let before_session = app.current_session_id.clone();
    let result = execute(command, app);
    assert_eq!(app.api_messages, before_messages);
    assert_eq!(app.plan_state.try_lock().unwrap().snapshot(), before_plan);
    assert_eq!(app.current_session_id, before_session);
    observation(result, app)
}

fn baseline_observations() -> Value {
    let temp = TempDir::new().unwrap();
    let mut app = app(&temp);
    seed(&mut app);
    let mut captures = serde_json::Map::new();
    for (name, command) in [
        ("turn_internal", "/structcopy turn 1 stdout"),
        ("turn_blocks", "/structcopy turn 2 stdout"),
        ("turn_unknown_result", "/structcopy turn 3 stdout"),
        ("tool_duplicate_last", "/structcopy tool call-golden stdout"),
        ("plan_full", "/structcopy plan stdout"),
        ("usize_plus_and_stdout_case", "/structcopy turn +2 StDoUt"),
        (
            "server_tool_unavailable",
            "/structcopy tool server-only stdout",
        ),
    ] {
        captures.insert(name.into(), dispatch(&mut app, command));
    }
    // Upstream now refuses duplicate identities instead of selecting the last.
    assert_eq!(captures["tool_duplicate_last"]["is_error"], true);
    assert_eq!(captures["tool_duplicate_last"]["clipboard"], Value::Null);
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![call("lonely")],
    }]);
    captures.insert(
        "tool_missing_result".into(),
        dispatch(&mut app, "/structcopy tool call-golden stdout"),
    );
    for (label, flag) in [
        ("false", Some(false)),
        ("true", Some(true)),
        ("unknown", None),
    ] {
        // Each tri-state case has one result; accumulated duplicates are now
        // rejected by upstream's execution-identity contract.
        app.api_messages_mut()[0].content.truncate(1);
        app.api_messages_mut()[0]
            .content
            .push(result("result", flag));
        captures.insert(
            format!("tool_error_{label}"),
            dispatch(&mut app, "/structcopy tool call-golden stdout"),
        );
    }
    seed(&mut app);
    for (locale, label) in [(Locale::En, "en"), (Locale::ZhHans, "zh_hans")] {
        app.ui_locale = locale;
        for (index, command) in [
            "/structcopy",
            "/structcopy stdout",
            "/structcopy turn 0",
            "/structcopy turn -1",
            "/structcopy turn 99999999999999999999999999",
            "/structcopy Turn 1",
            "/structcopy plan extra",
            "/structcopy turn 99 stdout",
            "/structcopy workflow absent stdout",
        ]
        .iter()
        .enumerate()
        {
            captures.insert(
                format!("{label}_error_{index}"),
                dispatch(&mut app, command),
            );
        }
        for (transport, clipboard) in [
            ("native", ClipboardHandler::new()),
            ("queued", ClipboardHandler::terminal_only_for_test()),
            ("failed", ClipboardHandler::unavailable_for_test(false)),
        ] {
            app.clipboard = clipboard;
            let value = execute("/structcopy turn 2", &mut app);
            captures.insert(format!("{label}_{transport}"), observation(value, &app));
        }
        let plan = app.plan_state.clone();
        let _guard = plan.try_lock().unwrap();
        app.clipboard = ClipboardHandler::new();
        let busy = execute("/structcopy plan stdout", &mut app);
        assert!(busy.is_error);
        assert!(app.clipboard.last_written_text().is_none());
        captures.insert(format!("{label}_busy_plan"), observation(busy, &app));
    }
    app.ui_locale = Locale::En;
    assert!(
        !temp.path().join(".codewhale").exists(),
        "missing workflow lookup created state"
    );
    crate::tools::workflow::structcopy_test_seed_run(
        temp.path(),
        "golden-run",
        "structcopy-baseline-session",
    );
    let mut workflow = dispatch(&mut app, "/structcopy workflow golden-run stdout");
    let message = workflow["message"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(message).unwrap();
    assert_eq!(parsed["object"]["leaf_count"], Value::Null);
    // Only a volatile source timestamp is normalized. Keep the 13-byte width,
    // so receipt payload_bytes still checks the exact baseline envelope.
    let timestamp = parsed["object"]["started_at_ms"].as_u64().unwrap();
    assert_eq!(timestamp.to_string().len(), 13);
    workflow["message"] = Value::String(message.replace(
        &format!("\"started_at_ms\":{timestamp}"),
        "\"started_at_ms\":1700000000000",
    ));
    captures.insert("workflow_owned".into(), workflow);
    app.current_session_id = Some("another-session".into());
    captures.insert(
        "workflow_wrong_owner".into(),
        dispatch(&mut app, "/structcopy workflow golden-run stdout"),
    );
    app.current_session_id = None;
    captures.insert(
        "workflow_no_session".into(),
        dispatch(&mut app, "/structcopy workflow golden-run stdout"),
    );
    let temp2 = TempDir::new().unwrap();
    let mut empty = self::app(&temp2);
    captures.insert(
        "empty_plan".into(),
        dispatch(&mut empty, "/structcopy plan stdout"),
    );
    captures.insert(
        "empty_turn".into(),
        dispatch(&mut empty, "/structcopy turn 1 stdout"),
    );
    let info = super::get_command_info("structcopy").unwrap();
    use super::traits::CommandGroup;
    captures.insert("metadata".into(),json!({
        "name":info.name,"aliases":info.aliases,"usage":info.usage,
        "description_en":tr(Locale::En,info.description_id),
        "description_zh_hans":tr(Locale::ZhHans,info.description_id),
        "session_order":super::session_group::SessionCommands.commands().iter().map(|c|c.info().name).collect::<Vec<_>>(),
        "native_model_tool":crate::core::engine::default_active_native_tool_names().contains(&"structcopy"),
    }));
    Value::Object(captures)
}

#[test]
fn structcopy_public_workflow_matches_frozen_baseline() {
    let mut expected: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/commands/fixtures/structcopy_baseline.json"
        ))
        .expect("frozen baseline fixture"),
    )
    .unwrap();
    // Keep the original capture intact. Upstream's execution-identity change
    // supersedes exactly its duplicate-last observation with safe refusal.
    expected["tool_duplicate_last"] = expected["server_tool_unavailable"].clone();
    // The integrated localization audit changed these four Chinese strings.
    // Preserve the captured payloads and English receipts byte-for-byte.
    for key in ["zh_hans_error_7", "zh_hans_native", "zh_hans_queued"] {
        expected[key]["message"] = Value::String(
            expected[key]["message"]
                .as_str()
                .unwrap()
                .replace("轮次", "回合"),
        );
    }
    expected["metadata"]["description_zh_hans"] =
        Value::String("将一个会话对象复制为已脱敏的 JSON；不是模型工具".into());
    assert_eq!(
        baseline_observations(),
        expected,
        "observable structcopy behavior changed"
    );
}

#[test]
fn structcopy_host_exposes_exact_authority_and_filters_private_data_before_crossing() {
    use super::groups::session::StructcopyRegistration;
    use codewhale_command_contract::facets::*;
    use codewhale_command_contract::handler::{CommandCapabilities, CommandHandler, ContextParts};
    use codewhale_command_contract::metadata::RegisterCommand;
    let CommandHandler::Contextual { capabilities, .. } = StructcopyRegistration::handler() else {
        panic!("contract registration required");
    };
    assert_eq!(
        capabilities,
        CommandCapabilities::SESSION_STRUCTCOPY.union(CommandCapabilities::PRESENTATION)
    );
    let tmp = TempDir::new().unwrap();
    let mut app = app(&tmp);
    seed(&mut app);
    let mut bundle = app.command_contexts();
    let ContextParts {
        structcopy,
        presentation,
        session,
        model,
        cost,
        mode_policy,
        system_prompt,
        skills,
        workspace,
        media,
        memory,
        project,
        skill_group,
        plugin,
        lifecycle,
        control,
        export,
        debug_receipts,
        debug_change,
        debug_history,
        debug_diff,
        debug_undo,
        debug_diagnostics,
        permissions,
        config_status,
        config,
        pet,
        trust,
        login,
        work_surface,
        navigation,
        voice_input,
    } = bundle.contexts(capabilities).into_parts();
    assert!(
        permissions.is_none()
            && config_status.is_none()
            && config.is_none()
            && pet.is_none()
            && trust.is_none()
            && login.is_none()
            && work_surface.is_none()
            && navigation.is_none()
            && voice_input.is_none()
    );
    assert!(presentation.is_some());
    assert!(
        session.is_none()
            && model.is_none()
            && cost.is_none()
            && mode_policy.is_none()
            && system_prompt.is_none()
            && skills.is_none()
            && workspace.is_none()
            && media.is_none()
            && memory.is_none()
            && project.is_none()
            && skill_group.is_none()
            && plugin.is_none()
            && lifecycle.is_none()
            && control.is_none()
            && export.is_none()
            && debug_receipts.is_none()
            && debug_change.is_none()
            && debug_history.is_none()
            && debug_diff.is_none()
            && debug_undo.is_none()
            && debug_diagnostics.is_none()
    );
    let copy = structcopy.unwrap();
    assert_eq!(copy.transcript_item(0), Err(StructcopyError::Unavailable));
    assert_eq!(
        copy.transcript_item(1).unwrap().content,
        StructcopyContent::InternalContext
    );
    let visible = copy.transcript_item(2).unwrap();
    let debug = format!("{visible:?}");
    for forbidden in [
        "hidden reasoning",
        "hidden-signature",
        "private-call-signature",
        "private-base64",
        "data:image",
    ] {
        assert!(!debug.contains(forbidden), "{debug}");
    }
    let StructcopyContent::Visible(blocks) = visible.content else {
        panic!("visible blocks")
    };
    assert!(blocks.contains(&StructcopyBlock::ThinkingOmitted));
    assert!(blocks.contains(&StructcopyBlock::ImageOmitted));
    assert_eq!(
        copy.tool_pair("call-golden"),
        Err(StructcopyError::Unavailable)
    );
    assert_eq!(
        copy.tool_pair("server-only"),
        Err(StructcopyError::Unavailable)
    );
    assert_eq!(
        copy.plan_snapshot().unwrap().title.as_deref(),
        Some("Golden plan")
    );
}

#[test]
fn structcopy_public_workflow_preserves_payload_across_locales_and_transports() {
    use codewhale_localization::MessageId;
    let temp = TempDir::new().unwrap();
    let mut app = app(&temp);
    seed(&mut app);
    // This successful transport matrix needs a unique tool pair. Duplicate
    // identities are exercised by the separate refusal regressions.
    app.api_messages_mut()[2].content.clear();
    crate::tools::workflow::structcopy_test_seed_run(
        temp.path(),
        "transport-run",
        "structcopy-baseline-session",
    );
    for (selector, kind) in [
        ("turn 2", MessageId::CmdStructcopyKindTurn),
        ("tool call-golden", MessageId::CmdStructcopyKindTool),
        ("plan", MessageId::CmdStructcopyKindPlan),
        (
            "workflow transport-run",
            MessageId::CmdStructcopyKindWorkflow,
        ),
    ] {
        let command = format!("/structcopy {selector}");
        let expected = dispatch(&mut app, &format!("{command} stdout"));
        assert_eq!(expected["is_error"], false);
        let payload = expected["message"].as_str().unwrap();
        assert_eq!(expected["clipboard"], Value::Null);
        for &locale in Locale::shipped() {
            app.ui_locale = locale;
            let stdout = dispatch(&mut app, &format!("{command} stdout"));
            assert_eq!(stdout["message"], payload);
            assert_eq!(stdout["clipboard"], Value::Null);
            for (clipboard, id) in [
                (
                    ClipboardHandler::new(),
                    MessageId::CmdStructcopyClipboardAccepted,
                ),
                (
                    ClipboardHandler::terminal_only_for_test(),
                    MessageId::CmdStructcopyClipboardQueued,
                ),
            ] {
                app.clipboard = clipboard;
                let before_messages = app.api_messages.clone();
                let before_plan = app.plan_state.try_lock().unwrap().snapshot();
                let before_session = app.current_session_id.clone();
                let result = execute(&command, &mut app);
                assert!(!result.is_error);
                assert!(result.action.is_none());
                assert_eq!(
                    result.message.as_deref(),
                    Some(
                        tr(locale, id)
                            .replace("{kind}", &tr(locale, kind))
                            .replace("{bytes}", &payload.len().to_string())
                            .as_str()
                    )
                );
                if id == MessageId::CmdStructcopyClipboardAccepted {
                    assert_eq!(app.clipboard.last_written_text(), Some(payload));
                } else {
                    // Terminal writes are queued; this accessor observes native writes only.
                    assert!(app.clipboard.last_written_text().is_none());
                }
                assert_eq!(app.api_messages, before_messages);
                assert_eq!(app.plan_state.try_lock().unwrap().snapshot(), before_plan);
                assert_eq!(app.current_session_id, before_session);
            }
        }
    }
}
