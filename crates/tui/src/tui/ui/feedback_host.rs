//! Async local report review on the existing dispatch completion mailbox.
use super::*;
use codewhale_engine::hooks::HookCaller;
use codewhale_engine::tools::github::report;
use std::sync::Arc;

#[derive(Clone)]
struct Scope {
    caller: HookCaller,
    host: bool,
}
impl Scope {
    fn capture(app: &App, config: &Config) -> Result<Self> {
        Ok(Self {
            caller: app
                .base_hook_context()
                .caller
                .context("Feedback caller is unavailable")?,
            host: config
                .features()
                .enabled(codewhale_runtime::features::Feature::GithubHost),
        })
    }
    fn current(&self, app: &App, config: &Config) -> bool {
        self.caller.workspace == app.workspace
            && self.caller.session_id == app.current_session_id
            && self.caller.agent_id == app.agent_focus.as_ref().map(|focus| focus.agent_id.clone())
            && self.caller.origin_turn_id == app.runtime_turn_id
            && self.host
                == config
                    .features()
                    .enabled(codewhale_runtime::features::Feature::GithubHost)
            && self
                .caller
                .plugins
                .as_ref()
                .is_some_and(|plugins| Arc::ptr_eq(plugins, &app.extension_plugin_view()))
    }
}
pub(crate) struct EditReady {
    scope: Scope,
    instruction: String,
}

pub(super) fn start(
    app: &mut App,
    config: &Config,
    id: String,
    change: Option<String>,
) -> Result<()> {
    if app.feedback_dispatch.is_some() {
        anyhow::bail!("A feedback edit is already waiting for dispatch");
    }
    let scope = Scope::capture(app, config)?;
    let session = scope
        .caller
        .session_id
        .clone()
        .filter(|session| !session.is_empty())
        .context("Feedback review requires a current session")?;
    let permit = app
        .dispatch_completion_tx
        .clone()
        .context("Feedback completion mailbox is unavailable")?
        .try_reserve_owned()
        .context("Feedback completion mailbox is full or closed")?;
    let manager = codewhale_engine::extension_host::manager();
    let handle = tokio::runtime::Handle::try_current()
        .context("Feedback Engine scheduler is unavailable")?;
    manager.bind_engine_handle(handle.clone());
    let notice = app.tr(MessageId::FeedbackReviewNotice).into_owned();
    let unavailable = app.tr(MessageId::FeedbackUnavailable).into_owned();
    let draft_notice = app.tr(MessageId::FeedbackDraftRequested).into_owned();
    #[cfg(test)]
    let env_scope = crate::test_support::env_scope_ticket();
    handle.spawn(async move {
        #[cfg(test)]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let result = if scope.host {
            manager
                .execute_github_review(scope.caller.clone(), id.clone())
                .await
                .map(|result| result.content)
                .map_err(|_| ())
        } else {
            #[cfg(test)]
            let env_scope = crate::test_support::env_scope_ticket();
            let session = session.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                report::load(&session, &id)
                    .map(|report| report.render_review())
                    .map_err(|_| ())
            })
            .await
            .unwrap_or(Err(()))
        };
        let apply: crate::tui::app::DispatchApplyFn = Box::new(move |app, _, config| {
            if !scope.current(app, config) {
                return Ok(());
            }
            match result {
                Err(()) => app.add_message(HistoryCell::System {
                    content: unavailable,
                }),
                Ok(review) => {
                    if let Some(change) = change {
                        if app.feedback_dispatch.is_some() {
                            app.add_message(HistoryCell::System {
                                content: unavailable,
                            });
                        } else {
                            app.add_message(HistoryCell::System {
                                content: draft_notice,
                            });
                            app.feedback_dispatch = Some(EditReady {
                                scope,
                                instruction: report::draft_instruction(
                                    &change,
                                    Some((&id, &review)),
                                ),
                            });
                        }
                    } else {
                        app.add_message(HistoryCell::System {
                            content: format!("{notice}\n\n{review}"),
                        });
                    }
                }
            }
            app.needs_redraw = true;
            Ok(())
        });
        permit.send(apply);
    });
    Ok(())
}

pub(super) async fn dispatch_ready(
    app: &mut App,
    config: &Config,
    engine: &EngineHandle,
) -> Result<()> {
    let Some(ready) = app.feedback_dispatch.take() else {
        return Ok(());
    };
    if !ready.scope.current(app, config) {
        return Ok(());
    }
    let message = build_queued_message(app, ready.instruction);
    let action = ComposerSubmitAction::Submit(app.decide_submit_disposition());
    dispatch_composer_message(
        app,
        config,
        engine,
        message,
        DispatchRecovery::Immediate,
        action,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_feedback_result_rejects_session_workspace_and_plugin_view_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::default();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(tmp.path()),
            &config,
        );
        app.current_session_id = Some("session-a".into());
        let scope = Scope::capture(&app, &config).unwrap();
        assert!(scope.current(&app, &config));
        app.current_session_id = Some("session-b".into());
        assert!(!scope.current(&app, &config));
        app.current_session_id = Some("session-a".into());
        app.runtime_turn_id = Some("new-turn".into());
        assert!(!scope.current(&app, &config));
        app.runtime_turn_id = scope.caller.origin_turn_id.clone();
        app.workspace = tmp.path().join("other");
        assert!(!scope.current(&app, &config));
        app.workspace = scope.caller.workspace.clone();
        app.plugin_registry = Arc::new(codewhale_plugins::PluginRegistry::new());
        assert!(!scope.current(&app, &config));
    }
    #[test]
    fn feedback_edit_slot_refuses_replacement_and_feature_change_rejects_apply() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(tmp.path()),
            &config,
        );
        app.current_session_id = Some("session".into());
        let scope = Scope::capture(&app, &config).unwrap();
        app.feedback_dispatch = Some(EditReady {
            scope: scope.clone(),
            instruction: "first edit".into(),
        });
        assert!(
            start(
                &mut app,
                &config,
                "other".into(),
                Some("second edit".into())
            )
            .unwrap_err()
            .to_string()
            .contains("already waiting")
        );
        assert_eq!(
            app.feedback_dispatch.as_ref().unwrap().instruction,
            "first edit"
        );
        config.set_feature("github_host", true).unwrap();
        assert!(!scope.current(&app, &config));
        let mut wrong_agent = scope;
        wrong_agent.caller.agent_id = Some("other-agent".into());
        assert!(!wrong_agent.current(&app, &Config::default()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_or_closed_feedback_mailbox_refuses_before_any_read_or_host_job() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::default();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(tmp.path()),
            &config,
        );
        app.current_session_id = Some("session".into());
        let (tx, rx) = tokio::sync::mpsc::channel::<crate::tui::app::DispatchApplyFn>(1);
        let _reserved = tx.clone().try_reserve_owned().unwrap();
        app.dispatch_completion_tx = Some(tx);
        assert!(
            start(&mut app, &config, "invalid".into(), None)
                .unwrap_err()
                .to_string()
                .contains("full or closed")
        );
        drop(rx);
        assert!(start(&mut app, &config, "invalid".into(), None).is_err());
        assert!(app.feedback_dispatch.is_none());
    }
}
