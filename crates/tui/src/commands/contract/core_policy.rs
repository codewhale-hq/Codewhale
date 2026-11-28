pub(super) struct PolicyAdapter<'a> {
    pub(super) host: super::SharedCommandHost<'a>,
}
use crate::tui::app::AppAction;
use codewhale_command_contract::core_policy::*;
use codewhale_engine::reasoning_preference::ReasoningEffort;
use codewhale_engine::tools::github::report;
use codewhale_localization::MessageId;

pub(super) fn resolve_message(key: &str) -> Option<MessageId> {
    Some(match key {
        "clear_conversation" => MessageId::ClearConversation,
        "clear_conversation_busy" => MessageId::ClearConversationBusy,
        "feedback_help" => MessageId::FeedbackHelp,
        "feedback_no_session" => MessageId::FeedbackNoSession,
        "feedback_draft_requested" => MessageId::FeedbackDraftRequested,
        "cmd_effort_description" => MessageId::CmdEffortDescription,
        _ => return None,
    })
}

impl ClearContext for PolicyAdapter<'_> {
    fn start_empty_conversation(&mut self) -> Result<ClearedConversation, ClearFailure> {
        let mut app = self.host.app.borrow_mut();
        let blockers = app.session_transition_blockers();
        if !blockers.is_empty() {
            return Err(ClearFailure::Busy(blockers));
        }
        let new_id = uuid::Uuid::new_v4().to_string();
        let queue = crate::tui::ui::prepare_offline_queue_transition(&app, &new_id)
            .map_err(ClearFailure::Transition)?;
        if !crate::commands::groups::core::reset_conversation_state(&mut app) {
            return Err(ClearFailure::Busy(Vec::new()));
        }
        crate::tui::ui::install_offline_queue_transition(&mut app, queue);
        app.set_current_session_id(Some(new_id.clone()));
        crate::tui::ui::expect_fresh_session(&mut app, &new_id)
            .map_err(ClearFailure::Transition)?;
        app.current_session_metadata = None;
        app.session_title = None;
        app.window_title = None;
        app.composer_color = None;
        Ok(ClearedConversation {
            session_id: new_id,
            model: app.model.clone(),
            workspace: app.workspace.clone(),
            mode: super::to_command_mode(app.mode),
            agent_preset: app.agent_preset(),
        })
    }
}

impl From<ClearedConversation> for AppAction {
    fn from(receipt: ClearedConversation) -> Self {
        Self::SyncSession {
            session_id: Some(receipt.session_id),
            messages: Vec::new(),
            system_prompt: None,
            model: receipt.model,
            workspace: receipt.workspace,
            mode: super::from_command_mode(receipt.mode),
            agent_preset: receipt.agent_preset,
        }
    }
}

impl EffortContext for PolicyAdapter<'_> {
    fn available_labels(&self) -> Vec<String> {
        let app = self.host.app.borrow();
        crate::tui::model_picker::picker_efforts_for_route(
            app.api_provider,
            &app.active_route_base_url,
            &app.model,
            app.auto_model,
        )
        .iter()
        .map(|effort| {
            effort
                .display_label_for_provider(app.api_provider)
                .to_owned()
        })
        .collect()
    }
    fn cycle(&mut self) -> (String, String) {
        let mut app = self.host.app.borrow_mut();
        let prev = app
            .reasoning_effort
            .display_label_for_provider(app.api_provider)
            .to_owned();
        let _ = app.cycle_effort();
        (
            prev,
            app.reasoning_effort
                .display_label_for_provider(app.api_provider)
                .to_owned(),
        )
    }
    fn set_supported_effort(&mut self, value: &str) -> Result<String, EffortFailure> {
        let mut app = self.host.app.borrow_mut();
        let effort = ReasoningEffort::parse_strict(value).map_err(|_| EffortFailure::Unknown)?;
        let requested = if app.auto_model {
            effort
        } else {
            effort.normalize_for_route(app.api_provider, &app.active_route_base_url, &app.model)
        };
        let available = crate::tui::model_picker::picker_efforts_for_route(
            app.api_provider,
            &app.active_route_base_url,
            &app.model,
            app.auto_model,
        );
        if !available.is_empty() && !available.contains(&requested) && !available.contains(&effort)
        {
            return Err(EffortFailure::Unavailable {
                model: app.model.clone(),
                labels: available
                    .iter()
                    .map(|effort| {
                        effort
                            .display_label_for_provider(app.api_provider)
                            .to_owned()
                    })
                    .collect(),
            });
        }
        app.commit_reasoning_effort(requested);
        Ok(requested
            .display_label_for_provider(app.api_provider)
            .to_owned())
    }
}

impl FeedbackContext for PolicyAdapter<'_> {
    fn has_session(&self) -> bool {
        self.host.app.borrow().current_session_id.is_some()
    }
    fn safe_text(&self, text: &str) -> Result<String, String> {
        report::safe_text(text, 1600, &mut Default::default()).map_err(|_| {
            self.host
                .app
                .borrow()
                .tr(MessageId::FeedbackUnavailable)
                .to_string()
        })
    }
}

impl From<FeedbackAction> for AppAction {
    fn from(action: FeedbackAction) -> Self {
        use codewhale_commands::core_policy::{FEATURE_URL, SECURITY_POLICY_URL};
        match action {
            FeedbackAction::OpenPicker => Self::OpenFeedbackPicker,
            FeedbackAction::Feature => Self::OpenExternalUrl {
                url: FEATURE_URL.into(),
                label: "GitHub feature request".into(),
            },
            FeedbackAction::Security => Self::OpenExternalUrl {
                url: SECURITY_POLICY_URL.into(),
                label: "GitHub security policy".into(),
            },
            FeedbackAction::Draft(focus) => {
                Self::SendMessage(report::draft_instruction(&focus, None))
            }
            FeedbackAction::Review { id, change } => Self::ReviewIssueReport { id, change },
        }
    }
}
