//! `/remote-env` — portable handler over the session-control + presentation
//! facets. Opens the hosted Work launcher without taking source custody.

use super::CommandResult;
use codewhale_command_contract::facets::{
    CommandPresentationContext, CommandSessionControlContext,
};
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub(in crate::commands) struct RemoteEnvCmd;

// ---------------------------------------------------------------------------
// FEAT-024 Phase 4 (D3/D6/D7): portable contextual registration and handler.
// `/remote-env` and `/color` receive PRESENTATION for runtime localization.
// The handler translates the
// exact stable keys with the baseline placeholder sets; Git/URL machinery and
// credential-safe target resolution stay behind `resolve_hosted_work_target`.
// ---------------------------------------------------------------------------

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "remote-env",
    aliases: &[],
    usage: "/remote-env [open]",
    description_key: "cmd_remote_env_description",
};

impl ContractRegisterCommand<CommandResult> for RemoteEnvCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::SESSION_CONTROL
                .union(codewhale_command_contract::handler::CommandCapabilities::PRESENTATION),
            handler: remote_env_contextual,
        }
    }
}

pub(in crate::commands) fn remote_env_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(control) = parts.control.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: session_control".to_string());
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation".to_string());
    };
    remote_env_portable(control, presentation, arg)
}

pub(in crate::commands) fn remote_env_portable(
    control: &mut dyn CommandSessionControlContext,
    presentation: &mut dyn CommandPresentationContext,
    arg: Option<&str>,
) -> CommandResult {
    match arg.map(str::trim).filter(|value| !value.is_empty()) {
        None => match presentation.translate(
            "cmd_remote_env_overview",
            &[("command", "/remote-env open")],
        ) {
            Ok(copy) => CommandResult::message(copy),
            Err(error) => CommandResult::error(error),
        },
        Some("open") => open_hosted_work(control, presentation),
        Some(_) => match presentation.translate(
            "cmd_remote_env_source_custody_policy",
            &[("command", "/remote-env open")],
        ) {
            Ok(copy) => CommandResult::error(copy),
            Err(error) => CommandResult::error(error),
        },
    }
}

fn open_hosted_work(
    control: &dyn CommandSessionControlContext,
    presentation: &mut dyn CommandPresentationContext,
) -> CommandResult {
    let Some(target) = control.resolve_hosted_work_target() else {
        return match presentation.translate(
            "cmd_remote_env_unavailable",
            &[("command", "/remote-env open"), ("origin", "origin")],
        ) {
            Ok(copy) => CommandResult::error(copy),
            Err(error) => CommandResult::error(error),
        };
    };
    let message = match presentation.translate(
        "cmd_remote_env_opening",
        &[
            ("origin", "origin"),
            ("url", &target.url),
            ("repo", &target.repo),
            ("branch", &target.branch),
        ],
    ) {
        Ok(copy) => copy,
        Err(error) => return CommandResult::error(error),
    };
    let label = match presentation.translate("cmd_remote_env_browser_label", &[]) {
        Ok(label) => label,
        Err(error) => return CommandResult::error(error),
    };
    CommandResult::with_message_and_action(
        message,
        codewhale_command_contract::outcome::SessionAction::OpenExternalUrl {
            url: target.url,
            label,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::super::control_test_support::message;
    use super::*;
    use codewhale_command_contract::facets::HostedWorkTarget;
    use std::cell::RefCell;

    /// Deterministic fake presentation facet resolving the five remote-env
    /// keys with exact placeholder-set enforcement like the real adapter.
    #[derive(Default)]
    struct FakePresentation {
        calls: RefCell<Vec<(String, String)>>,
    }

    impl CommandPresentationContext for FakePresentation {
        fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push((key.to_string(), format!("{replacements:?}")));
            let base = match key {
                "cmd_remote_env_overview" => "Overview {command}".to_string(),
                "cmd_remote_env_opening" => {
                    "Opening {repo}/{branch} from {origin} at {url}".to_string()
                }
                "cmd_remote_env_unavailable" => "Unavailable {command} {origin}".to_string(),
                "cmd_remote_env_source_custody_policy" => "Policy {command}".to_string(),
                "cmd_remote_env_browser_label" => "Label".to_string(),
                other => return Err(format!("unknown key {other}")),
            };
            let mut out = base;
            for (name, value) in replacements {
                out = out.replace(&format!("{{{name}}}"), value);
            }
            Ok(out)
        }
    }

    fn fake_control(
        target: Option<HostedWorkTarget>,
    ) -> super::super::control_test_support::FakeControl {
        super::super::control_test_support::FakeControl {
            hosted: Some(target),
            ..super::super::control_test_support::FakeControl::default()
        }
    }

    #[test]
    fn remote_env_overview_and_policy_branches_are_exact() {
        let mut control = fake_control(None);
        let mut presentation = FakePresentation::default();
        let overview = remote_env_portable(&mut control, &mut presentation, None);
        assert!(!overview.is_error);
        assert_eq!(message(&overview), "Overview /remote-env open");
        assert!(overview.action.is_none());

        let mut control = fake_control(None);
        let invalid =
            remote_env_portable(&mut control, &mut FakePresentation::default(), Some("sync"));
        assert!(invalid.is_error);
        assert_eq!(message(&invalid), "Policy /remote-env open");
        assert!(invalid.action.is_none());
    }

    #[test]
    fn remote_env_open_composes_exact_url_message_and_action() {
        let mut control = fake_control(Some(HostedWorkTarget {
            url: "https://app.codewhale.net/work?repo=A%2FB&branch=main".to_string(),
            repo: "A/B".to_string(),
            branch: "main".to_string(),
        }));
        let result =
            remote_env_portable(&mut control, &mut FakePresentation::default(), Some("open"));
        assert!(!result.is_error);
        assert_eq!(
            message(&result),
            "Opening A/B/main from origin at https://app.codewhale.net/work?repo=A%2FB&branch=main"
        );
        assert!(matches!(
            result.action,
            Some(codewhale_command_contract::outcome::SessionAction::OpenExternalUrl { ref url, ref label })
                if url == "https://app.codewhale.net/work?repo=A%2FB&branch=main" && label == "Label"
        ));
        assert_eq!(
            control.calls.borrow().as_slice(),
            ["resolve_hosted_work_target"]
        );

        let mut control = fake_control(None);
        let result =
            remote_env_portable(&mut control, &mut FakePresentation::default(), Some("open"));
        assert!(result.is_error);
        assert_eq!(message(&result), "Unavailable /remote-env open origin");
        assert!(result.action.is_none());
    }

    #[test]
    fn remote_env_missing_authority_fails_safely() {
        let contexts = codewhale_command_contract::handler::CommandContexts::empty();
        let result = remote_env_contextual(contexts, None);
        assert!(result.is_error);
        assert_eq!(
            message(&result),
            "Command capability unavailable: session_control"
        );

        let mut control = fake_control(None);
        let contexts = codewhale_command_contract::handler::CommandContexts::empty()
            .with_control(&mut control);
        let result = remote_env_contextual(contexts, None);
        assert!(result.is_error);
        assert_eq!(
            message(&result),
            "Command capability unavailable: presentation"
        );
        assert!(
            control.calls.borrow().is_empty(),
            "translation authority is checked before command work"
        );
    }
}
