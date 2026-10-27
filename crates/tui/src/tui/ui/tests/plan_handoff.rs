//! Plan hand-off: the answer to the "plan ready" question is applied by the
//! host — permission first, then the mode — and never touches an engine
//! `request_user_input` call.

use super::*;
use crate::tui::plan_handoff::{self, PlanHandoffChoice};
use crate::tui::ui::handlers::{
    apply_plan_handoff, apply_plan_handoff_with_checkpoint, enter_work_for_plan,
    is_plan_handoff_request,
};

/// A session sitting in Plan whose Work permission is Ask.
fn app_in_plan() -> App {
    let mut app = create_test_app();
    app.set_agent_approval_posture(ApprovalMode::Suggest);
    let _ = app.set_mode(AppMode::Plan);
    assert_eq!(app.mode, AppMode::Plan);
    app
}

#[tokio::test]
async fn work_choice_leaves_plan_with_the_chosen_permission() {
    let mut app = app_in_plan();
    let mut engine = mock_engine_handle();

    assert!(
        enter_work_for_plan(
            &mut app,
            &Config::default(),
            &engine.handle,
            ApprovalMode::Suggest
        )
        .await
    );

    assert_eq!(app.mode, AppMode::Agent);
    assert_eq!(app.approval_mode, ApprovalMode::Suggest);
    match engine.rx_op.recv().await.expect("change mode op") {
        crate::core::ops::Op::ChangeMode {
            mode,
            approval_mode,
            ..
        } => {
            assert_eq!(mode, AppMode::Agent);
            assert_eq!(approval_mode, ApprovalMode::Suggest);
        }
        other => panic!("expected ChangeMode, got {other:?}"),
    }
}

#[tokio::test]
async fn a_locked_permission_keeps_the_session_in_plan() {
    let mut app = app_in_plan();
    app.mark_approval_policy_locked();
    let mut engine = mock_engine_handle();

    // Ask is in force and locked; the person asked for Auto-Review. Starting
    // the work under a permission they did not pick is the wrong outcome.
    assert!(
        !enter_work_for_plan(
            &mut app,
            &Config::default(),
            &engine.handle,
            ApprovalMode::Auto
        )
        .await
    );

    assert_eq!(app.mode, AppMode::Plan);
    assert_eq!(app.agent_approval_baseline(), ApprovalMode::Suggest);
    assert!(
        engine.rx_op.try_recv().is_err(),
        "no mode change may reach the engine"
    );
}

#[tokio::test]
async fn keep_planning_changes_nothing_and_sends_nothing() {
    let mut app = app_in_plan();
    let request_id = completed_plan(&mut app, "Inspect first, then make the change.", false);
    let history_len = app.history.len();
    let mut engine = mock_engine_handle();

    apply_plan_handoff(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::KeepPlanning,
    )
    .await
    .expect("keep planning");

    assert_eq!(app.mode, AppMode::Plan);
    assert_eq!(app.history.len(), history_len);
    assert!(!app.is_loading);
    assert!(app.pending_plan_handoff.is_none());
    assert!(app.todos.lock().await.snapshot().is_empty());
    assert!(engine.rx_op.try_recv().is_err());
}

#[test]
fn an_engine_question_with_the_handoff_id_still_goes_to_the_engine() {
    let mut app = app_in_plan();
    assert!(is_plan_handoff_request(&app, plan_handoff::REQUEST_ID));
    assert!(!is_plan_handoff_request(&app, "call_1"));

    // Tool-call ids come from the provider. One that collides with the
    // hand-off id is still the engine's question while it is pending.
    app.pending_user_input_prompt = Some((
        plan_handoff::REQUEST_ID.to_string(),
        plan_handoff::request(app.ui_locale),
    ));
    assert!(!is_plan_handoff_request(&app, plan_handoff::REQUEST_ID));
}

/// Complete actual typed output inside the current turn; no tool event is needed.
fn completed_plan(app: &mut App, text: &str, checklist: bool) -> String {
    app.current_session_id = Some("plan-session".into());
    app.runtime_turn_id = Some("plan-turn".into());
    app.ocean_turn_history_start = app.history.len();
    if checklist {
        push_checklist_receipt(app);
    }
    let index = app.history.len();
    app.push_history_cell(HistoryCell::Assistant {
        content: text.into(),
        streaming: false,
    });
    app.record_completed_assistant_output(index, text);
    let has_open_todos = app
        .todos
        .try_lock()
        .expect("test To-dos available")
        .snapshot()
        .items
        .iter()
        .any(|item| !item.status.is_settled());
    app.prepare_plan_handoff(
        crate::core::events::TurnOutcomeStatus::Completed,
        Some("plan-turn"),
        has_open_todos,
    )
    .expect("completed current-turn plan")
}

fn push_checklist_receipt(app: &mut App) {
    app.push_history_cell(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
        name: "todo_write".into(),
        status: ToolStatus::Success,
        input_summary: None,
        output: None,
        prompts: None,
        spillover_path: None,
        output_summary: None,
        is_diff: false,
    })));
}

// Exercise the real graph publication boundary without owning the singleton actor.
fn accept_checkpoint(
    app: &mut App,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + '_>> {
    Box::pin(publish_pending_work_projection(app))
}

fn reject_checkpoint(
    _: &mut App,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + '_>> {
    Box::pin(async { Err("test checkpoint admission refusal".to_string()) })
}

fn reject_checkpoint_after_other_work_changes(
    app: &mut App,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + '_>> {
    Box::pin(async move {
        let work = app.runtime_services.work.as_ref().unwrap();
        let mut todos = work.current_todos().await?;
        todos.items[0].content = "Updated unrelated work".into();
        todos.items[0].status = crate::tools::todo::TodoStatus::InProgress;
        let id = todos.items.iter().map(|item| item.id).max().unwrap() + 1;
        todos.items.push(crate::tools::todo::TodoItem {
            id,
            content: "Added while preparing work".into(),
            status: crate::tools::todo::TodoStatus::Pending,
        });
        work.apply_todo_update("plan-session", "todo_write", &todos)
            .await?;
        Err("test checkpoint admission refusal".to_string())
    })
}

fn reject_checkpoint_after_owned_seed_status_changes(
    app: &mut App,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + '_>> {
    Box::pin(async move {
        let work = app.runtime_services.work.as_ref().unwrap();
        let mut todos = work.current_todos().await?;
        todos.items.last_mut().unwrap().status = crate::tools::todo::TodoStatus::InProgress;
        work.apply_todo_update("plan-session", "todo_write", &todos)
            .await?;
        Err("test checkpoint admission refusal".to_string())
    })
}

#[test]
fn completed_plan_receipt_excludes_old_empty_failed_and_interrupted_output() {
    let mut app = app_in_plan();
    completed_plan(&mut app, "An older plan", false);
    app.pending_plan_handoff = None;
    app.ocean_turn_history_start = app.history.len();
    assert!(
        app.prepare_plan_handoff(
            crate::core::events::TurnOutcomeStatus::Completed,
            Some("plan-turn"),
            false,
        )
        .is_none()
    );
    for status in [
        crate::core::events::TurnOutcomeStatus::Interrupted,
        crate::core::events::TurnOutcomeStatus::Failed,
    ] {
        app.ocean_turn_history_start = 0;
        assert!(
            app.prepare_plan_handoff(status, Some("plan-turn"), false)
                .is_none()
        );
    }
    app.ocean_turn_history_start = app.history.len();
    let index = app.history.len();
    app.push_history_cell(HistoryCell::Assistant {
        content: "  \n".into(),
        streaming: false,
    });
    app.record_completed_assistant_output(index, "  \n");
    assert!(
        app.prepare_plan_handoff(
            crate::core::events::TurnOutcomeStatus::Completed,
            Some("plan-turn"),
            false,
        )
        .is_none()
    );
}

#[tokio::test]
async fn prose_work_seeds_preserves_other_work_and_queues_exact_text_once() {
    use crate::tools::todo::TodoStatus;
    let mut app = app_in_plan();
    app.offline_mode = true; // ordinary composer queue; no provider admission.
    app.todos
        .lock()
        .await
        .add("Unrelated work".into(), TodoStatus::Pending);
    let text = format!(
        "先检查输入。\n\n{}\nThen implement and verify.\n",
        "long plan 🐳 ".repeat(150)
    );
    let request_id = completed_plan(&mut app, &text, false);
    let mut engine = mock_engine_handle();
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::Work(ApprovalMode::Suggest),
        accept_checkpoint,
    )
    .await
    .expect("admitted Work handoff");
    assert_eq!(app.mode, AppMode::Agent);
    let todos = app.todos.lock().await.snapshot();
    assert_eq!(todos.items.len(), 2);
    assert_eq!(todos.items[0].content, "Unrelated work");
    assert_eq!(todos.items[1].status, TodoStatus::Pending);
    assert_eq!(todos.items[1].content, "先检查输入。");
    assert!(app.pending_plan_handoff.is_none());
    assert_eq!(app.queued_message_count(), 1);
    assert_eq!(
        app.queued_messages.front().unwrap().display,
        format!(
            "{}\n\n{text}\n\n{}",
            app.tr(MessageId::PlanHandoffProceed),
            app.tr(MessageId::PlanHandoffTodoNote)
                .replace("{title}", &todos.items[1].content)
        )
    );
    assert!(matches!(
        engine.rx_op.try_recv(),
        Ok(crate::core::ops::Op::ChangeMode { .. })
    ));
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::Work(ApprovalMode::Suggest),
        accept_checkpoint,
    )
    .await
    .unwrap();
    assert_eq!(app.queued_message_count(), 1);
    assert_eq!(app.todos.lock().await.snapshot(), todos);
    assert!(engine.rx_op.try_recv().is_err());
}

#[tokio::test]
async fn existing_current_turn_checklist_is_not_replaced_or_seeded_again() {
    use crate::tools::todo::TodoStatus;
    let mut app = app_in_plan();
    app.offline_mode = true;
    app.todos
        .lock()
        .await
        .add("Existing execution step".into(), TodoStatus::Pending);
    let before = app.todos.lock().await.snapshot();
    let request_id = completed_plan(&mut app, "Execute the existing checklist.", true);
    let engine = mock_engine_handle();
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::Work(ApprovalMode::Suggest),
        accept_checkpoint,
    )
    .await
    .unwrap();
    assert_eq!(app.todos.lock().await.snapshot(), before);
    assert_eq!(app.queued_message_count(), 1);
}

#[tokio::test]
async fn refused_checkpoint_rolls_back_seed_and_retry_queues_once() {
    // Cover surrounding/multiline whitespace and a Unicode title.
    for text in [
        " \n\tRead the data.\n\nBuild the report.\r\n ".to_string(),
        format!("\n{} \nKeep the full message.\t", "鲸".repeat(1023)),
    ] {
        let mut app = app_in_plan();
        app.offline_mode = true;
        let request_id = completed_plan(&mut app, &text, false);
        let mut engine = mock_engine_handle();
        apply_plan_handoff_with_checkpoint(
            &mut app,
            &Config::default(),
            &engine.handle,
            &request_id,
            PlanHandoffChoice::Work(ApprovalMode::Suggest),
            reject_checkpoint,
        )
        .await
        .unwrap();
        assert_eq!(app.mode, AppMode::Plan);
        assert_eq!(app.queued_message_count(), 0);
        assert!(
            app.todos.lock().await.snapshot().is_empty(),
            "unpublished seed must stay unpublished"
        );
        assert!(
            app.pending_plan_handoff
                .as_ref()
                .unwrap()
                .seeded_todo_id
                .is_none()
        );
        assert!(
            app.runtime_services
                .work
                .as_ref()
                .unwrap()
                .current_todos()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(app.pending_plan_handoff.as_ref().unwrap().text, text);
        while app.view_stack.pop().is_some() {}
        while let Ok(op) = engine.rx_op.try_recv() {
            assert!(
                matches!(op, crate::core::ops::Op::ChangeMode { .. }),
                "no turn dispatch after refusal"
            );
        }
        apply_plan_handoff_with_checkpoint(
            &mut app,
            &Config::default(),
            &engine.handle,
            &request_id,
            PlanHandoffChoice::Work(ApprovalMode::Suggest),
            accept_checkpoint,
        )
        .await
        .unwrap();
        let todos = app.todos.lock().await.snapshot();
        assert_eq!(todos.items.len(), 1);
        assert_eq!(
            todos.items[0].status,
            crate::tools::todo::TodoStatus::Pending
        );
        assert_eq!(
            todos.items[0].content,
            text.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap()
        );
        assert_eq!(app.queued_message_count(), 1);
        assert_eq!(
            app.queued_messages.front().unwrap().display,
            format!(
                "{}\n\n{text}\n\n{}",
                app.tr(MessageId::PlanHandoffProceed),
                app.tr(MessageId::PlanHandoffTodoNote)
                    .replace("{title}", &todos.items[0].content)
            )
        );
    }
}

#[tokio::test]
async fn failed_preparation_can_keep_revise_or_escape_without_publishing_its_seed() {
    use crate::tools::todo::TodoStatus;
    for action in ["keep", "revise", "escape"] {
        let mut app = app_in_plan();
        app.offline_mode = true;
        app.todos
            .lock()
            .await
            .add("Unrelated work".into(), TodoStatus::Pending);
        let plan_text = "Build the report after approval.";
        let request_id = completed_plan(&mut app, plan_text, false);
        let mut engine = mock_engine_handle();
        apply_plan_handoff_with_checkpoint(
            &mut app,
            &Config::default(),
            &engine.handle,
            &request_id,
            PlanHandoffChoice::Work(ApprovalMode::Suggest),
            reject_checkpoint_after_other_work_changes,
        )
        .await
        .unwrap();
        assert_eq!(app.mode, AppMode::Plan, "{action}");
        assert_eq!(app.queued_message_count(), 0, "{action}");
        assert!(
            app.pending_plan_handoff
                .as_ref()
                .unwrap()
                .seeded_todo_id
                .is_none()
        );
        let work = app.runtime_services.work.as_ref().unwrap().clone();
        let current = work.current_todos().await.unwrap();
        assert_eq!(current.items.len(), 2, "{action}");
        assert_eq!(
            current.items[0].content, "Updated unrelated work",
            "{action}"
        );
        assert_eq!(current.items[0].status, TodoStatus::InProgress, "{action}");
        assert_eq!(
            current.items[1].content, "Added while preparing work",
            "{action}"
        );
        assert_eq!(current.items[1].status, TodoStatus::Pending, "{action}");
        let snapshot = work.capture(Some("plan-session")).unwrap().unwrap();
        let seed = snapshot
            .graph
            .nodes
            .iter()
            .find(|node| node.title == plan_text)
            .unwrap();
        assert!(
            !snapshot.graph.compat.plan_order.contains(&seed.id),
            "{action}"
        );
        assert!(
            !snapshot
                .graph
                .compat
                .todos
                .iter()
                .any(|binding| binding.node == seed.id),
            "{action}"
        );
        // A background checkpoint while the retry question is still open
        // must publish only unrelated work, never the abandoned seed.
        publish_pending_work_projection(&mut app).await.unwrap();
        assert_eq!(app.todos.lock().await.snapshot(), current, "{action}");
        while let Ok(op) = engine.rx_op.try_recv() {
            assert!(
                matches!(op, crate::core::ops::Op::ChangeMode { .. }),
                "{action}"
            );
        }
        let choice = if action == "escape" {
            let events = app.view_stack.handle_key(KeyEvent::from(KeyCode::Esc));
            assert!(
                matches!(events.as_slice(), [ViewEvent::UserInputCancelled { tool_id }]
                if tool_id == &request_id)
            );
            // The host cancellation handler uses this same KeepPlanning path.
            PlanHandoffChoice::KeepPlanning
        } else {
            while app.view_stack.pop().is_some() {}
            if action == "revise" {
                PlanHandoffChoice::Revise("Add a validation step.".into())
            } else {
                PlanHandoffChoice::KeepPlanning
            }
        };
        apply_plan_handoff_with_checkpoint(
            &mut app,
            &Config::default(),
            &engine.handle,
            &request_id,
            choice,
            accept_checkpoint,
        )
        .await
        .unwrap();
        assert_eq!(app.mode, AppMode::Plan, "{action}");
        assert!(app.pending_plan_handoff.is_none(), "{action}");
        assert_eq!(work.current_todos().await.unwrap(), current, "{action}");
        assert_eq!(app.todos.lock().await.snapshot(), current, "{action}");
        assert!(
            engine.rx_op.try_recv().is_err(),
            "no execution dispatch for {action}"
        );
        if action == "revise" {
            assert_eq!(app.queued_message_count(), 1);
            assert_eq!(
                app.queued_messages.front().unwrap().display,
                "Add a validation step."
            );
        } else {
            assert_eq!(app.queued_message_count(), 0, "{action}");
        }
    }
}

#[tokio::test]
async fn cleanup_refusal_preserves_changed_work_and_owned_pending_without_dispatch() {
    use crate::tools::todo::TodoStatus;
    let mut app = app_in_plan();
    app.offline_mode = true;
    app.todos
        .lock()
        .await
        .add("Unrelated work".into(), TodoStatus::Pending);
    let request_id = completed_plan(&mut app, "Build this approved report.", false);
    let mut engine = mock_engine_handle();
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::Work(ApprovalMode::Suggest),
        reject_checkpoint_after_owned_seed_status_changes,
    )
    .await
    .unwrap();
    let seed = app.pending_plan_handoff.as_ref().unwrap().seeded_todo_id;
    assert!(seed.is_some());
    assert_eq!(app.queued_message_count(), 0);
    let work = app.runtime_services.work.as_ref().unwrap().clone();
    let changed = work.current_todos().await.unwrap();
    assert_eq!(changed.items[0].content, "Unrelated work");
    assert_eq!(changed.items[1].content, "Build this approved report.");
    assert_eq!(changed.items[1].status, TodoStatus::InProgress);
    while app.view_stack.pop().is_some() {}
    while let Ok(op) = engine.rx_op.try_recv() {
        assert!(matches!(op, crate::core::ops::Op::ChangeMode { .. }));
    }
    // Equal ID/content with a changed status is not unchanged ownership:
    // retrying Work must refuse before checkpoint or turn admission.
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::Work(ApprovalMode::Suggest),
        accept_checkpoint,
    )
    .await
    .unwrap();
    assert_eq!(
        app.pending_plan_handoff.as_ref().unwrap().seeded_todo_id,
        seed
    );
    assert_eq!(work.current_todos().await.unwrap(), changed);
    assert_eq!(app.queued_message_count(), 0);
    while app.view_stack.pop().is_some() {}
    while let Ok(op) = engine.rx_op.try_recv() {
        assert!(
            matches!(op, crate::core::ops::Op::ChangeMode { .. }),
            "no execution after status change"
        );
    }
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::KeepPlanning,
        accept_checkpoint,
    )
    .await
    .unwrap();
    assert_eq!(
        app.pending_plan_handoff.as_ref().unwrap().seeded_todo_id,
        seed
    );
    assert!(
        !app.view_stack.is_empty(),
        "failed cleanup stays reviewable"
    );
    assert_eq!(work.current_todos().await.unwrap(), changed);
    assert_eq!(app.queued_message_count(), 0);
    assert!(engine.rx_op.try_recv().is_err());
    while app.view_stack.pop().is_some() {}
    app.current_session_id = Some("another-session".into());
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::KeepPlanning,
        accept_checkpoint,
    )
    .await
    .unwrap();
    assert_eq!(
        work.current_todos().await.unwrap(),
        changed,
        "stale cleanup must not touch another session"
    );
    assert_eq!(
        app.pending_plan_handoff.as_ref().unwrap().seeded_todo_id,
        seed
    );
    assert_eq!(app.queued_message_count(), 0);
}

#[tokio::test]
async fn old_checklist_evidence_does_not_suppress_current_prose_seed() {
    use crate::tools::todo::TodoStatus;
    let mut app = app_in_plan();
    app.offline_mode = true;
    push_checklist_receipt(&mut app);
    app.tool_evidence.push(crate::tui::app::ToolEvidence {
        tool_name: "todo_write".into(),
        summary: "Earlier turn's checklist".into(),
    });
    app.todos
        .lock()
        .await
        .add("Earlier unrelated work".into(), TodoStatus::Pending);
    let request_id = completed_plan(&mut app, "Build this newly approved report.", false);
    assert!(
        !app.pending_plan_handoff
            .as_ref()
            .unwrap()
            .has_current_checklist
    );
    let engine = mock_engine_handle();
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::Work(ApprovalMode::Suggest),
        accept_checkpoint,
    )
    .await
    .unwrap();
    let todos = app.todos.lock().await.snapshot();
    assert_eq!(todos.items.len(), 2);
    assert_eq!(todos.items[0].content, "Earlier unrelated work");
    assert_eq!(todos.items[1].content, "Build this newly approved report.");
    assert_eq!(app.queued_message_count(), 1);
}

#[tokio::test]
async fn stale_cross_session_busy_and_permission_rejected_answers_seed_nothing() {
    for kind in [
        "stale-id",
        "session",
        "turn",
        "epoch",
        "busy",
        "draft",
        "permission",
        "engine",
    ] {
        let mut app = app_in_plan();
        let mut request_id = completed_plan(&mut app, "Keep this exact approved plan.", false);
        let mut engine = mock_engine_handle();
        let posture = if kind == "permission" {
            ApprovalMode::Auto
        } else {
            ApprovalMode::Suggest
        };
        match kind {
            "stale-id" => request_id.push_str("-older"),
            "session" => app.current_session_id = Some("another-session".into()),
            "turn" => app.runtime_turn_id = Some("another-turn".into()),
            "epoch" => app.transcript_identity_epoch += 1,
            "busy" => app.is_loading = true,
            "draft" => app.input = "unsent draft".into(),
            "permission" => app.mark_approval_policy_locked(),
            "engine" => engine.rx_op.close(),
            _ => unreachable!(),
        }
        apply_plan_handoff_with_checkpoint(
            &mut app,
            &Config::default(),
            &engine.handle,
            &request_id,
            PlanHandoffChoice::Work(posture),
            accept_checkpoint,
        )
        .await
        .unwrap();
        assert_eq!(app.mode, AppMode::Plan, "{kind}");
        assert_eq!(app.queued_message_count(), 0, "{kind}");
        assert!(app.todos.lock().await.snapshot().is_empty(), "{kind}");
        assert!(
            !app.runtime_services
                .work
                .as_ref()
                .unwrap()
                .has_pending_publish(),
            "{kind}"
        );
        assert!(engine.rx_op.try_recv().is_err(), "{kind}");
    }
}

#[tokio::test]
async fn revise_keeps_plan_without_execution_seed() {
    let mut app = app_in_plan();
    app.offline_mode = true;
    let request_id = completed_plan(&mut app, "Original plan", false);
    let engine = mock_engine_handle();
    apply_plan_handoff_with_checkpoint(
        &mut app,
        &Config::default(),
        &engine.handle,
        &request_id,
        PlanHandoffChoice::Revise("先补充测试".into()),
        accept_checkpoint,
    )
    .await
    .unwrap();
    assert_eq!(app.mode, AppMode::Plan);
    assert!(app.todos.lock().await.snapshot().is_empty());
    assert!(app.pending_plan_handoff.is_none());
    assert_eq!(app.queued_messages.front().unwrap().display, "先补充测试");
}
