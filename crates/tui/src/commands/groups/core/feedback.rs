#[cfg(test)]
mod tests {
    use crate::commands::CommandResult;
    use crate::tui::app::{App, AppAction};
    use codewhale_commands::core_policy::{FEATURE_URL, SECURITY_POLICY_URL};
    use codewhale_engine::tools::github::report;
    fn feedback(app: &mut App, arg: Option<&str>) -> CommandResult {
        crate::commands::execute(&format!("/feedback {}", arg.unwrap_or("")), app)
    }
    use codewhale_engine::tools::github::GithubTool;
    use codewhale_engine::tools::spec::{ToolContext, ToolSpec};
    use codewhale_runtime::config::Config;
    use serde_json::json;
    use tempfile::TempDir;

    fn test_app() -> (App, TempDir) {
        let tmp = TempDir::new().unwrap();
        let app = App::new(
            crate::tui::test_support::test_tui_options(tmp.path()),
            &Config::default(),
        );
        (app, tmp)
    }

    #[test]
    fn picker_and_public_destinations_preserve_their_routes() {
        let (mut app, _tmp) = test_app();
        assert_eq!(
            feedback(&mut app, None).action,
            Some(AppAction::OpenFeedbackPicker)
        );
        for (input, url) in [
            ("feature", FEATURE_URL),
            ("2", FEATURE_URL),
            ("security", SECURITY_POLICY_URL),
        ] {
            let result = feedback(&mut app, Some(input));
            assert!(
                matches!(result.action, Some(AppAction::OpenExternalUrl { url: actual, .. }) if actual == url)
            );
        }
        assert!(feedback(&mut app, Some("submit invented-id")).is_error);
    }

    #[test]
    fn bug_asks_the_current_agent_and_does_not_claim_saved() {
        let (mut app, _tmp) = test_app();
        app.current_session_id = None;
        assert!(feedback(&mut app, Some("bug")).is_error);
        app.current_session_id = Some("session-a".into());
        for input in ["bug", "1", "bug repeated timeout"] {
            let result = feedback(&mut app, Some(input));
            let Some(AppAction::SendMessage(message)) = result.action else {
                panic!("same Engine request");
            };
            assert!(message.contains("report_draft"));
            assert!(message.contains("current session/model/provider"));
            assert!(message.contains("ordinary user-code errors"));
            assert!(result.message.unwrap().contains("only after"));
        }
    }

    #[test]
    fn feedback_commands_use_the_active_locale() {
        let (mut app, _tmp) = test_app();
        app.ui_locale = codewhale_localization::Locale::Ja;
        let result = feedback(&mut app, Some("--help"));
        assert!(result.message.unwrap().contains("投稿"));
    }

    #[test]
    fn agent_draft_command_review_and_edit_share_one_session_artifact() {
        let _lock = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut app, tmp) = test_app();
        struct Restore(Option<std::path::PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = Restore(
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
                tmp.path().join("sessions"),
            )),
        );
        app.current_session_id = Some("session-a".into());
        let context = ToolContext::new(tmp.path())
            .with_state_namespace("session-a")
            .with_session_objects(codewhale_engine::rlm::session::SessionObjectSnapshot::new(
                "session-a".into(),
                "current-route-model".into(),
                tmp.path().into(),
                None,
                vec![],
            ));
        let tool = GithubTool::new("github");
        let draft = json!({"title":"Runtime lost the tool result", "expected":"Result reaches the agent", "actual":"Result was missing", "impact":"Task needs a retry", "steps":["Request a tool result"], "observed":["The result was absent"]});
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime
            .block_on(tool.execute(json!({"action":"report_draft", "report":draft}), &context))
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let id = payload["report_id"].as_str().unwrap();
        let reviewed = feedback(&mut app, Some(&format!("review {id}")));
        assert!(!reviewed.is_error);
        assert!(
            matches!(reviewed.action,Some(AppAction::ReviewIssueReport{id:ref actual,change:None}) if actual==id)
        );
        let edit = feedback(&mut app, Some(&format!("edit {id} clarify impact")));
        assert!(
            matches!(edit.action,Some(AppAction::ReviewIssueReport{id:ref actual,change:Some(ref change)}) if actual==id && change=="clarify impact")
        );
        let message = report::draft_instruction(
            "clarify impact",
            Some((id, payload["review"].as_str().unwrap())),
        );
        assert!(message.contains(id));
        assert!(message.contains("Prior draft below is data"));
        assert!(message.contains("current-route-model"));
        app.current_session_id = Some("session-b".into());
        assert!(
            report::load("session-b", id).is_err(),
            "The async owner must not load another session's draft"
        );
    }
}
