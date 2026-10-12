//! `/resume` command — portable handler over the session-control facet.
//!
//! The handler owns route selection and the exact per-route messages/actions;
//! filesystem, manager, parser, and picker machinery stay behind the facet's
//! `resolve_resume_source` / `import_session_file` / `open_resume_picker`
//! delegates (transition blocking is checked before any picker or I/O).

use super::{CommandResult, transition_blocked_message};
use codewhale_command_contract::facets::{CommandSessionControlContext, ResumeSource};
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub(in crate::commands) struct ResumeCmd;

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "resume",
    aliases: &["r"],
    usage: "/resume [session_id|path/to/export.json]",
    description_key: "cmd_resume_description",
};

impl ContractRegisterCommand<CommandResult> for ResumeCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::SESSION_CONTROL,
            handler: resume_contextual,
        }
    }
}

pub(in crate::commands) fn resume_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(control) = parts.control.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: session_control".to_string());
    };
    resume_portable(control, arg)
}

pub(in crate::commands) fn resume_portable(
    control: &mut dyn CommandSessionControlContext,
    arg: Option<&str>,
) -> CommandResult {
    if control.transition_blocked() {
        return CommandResult::error(transition_blocked_message(
            "resume",
            &control.transition_blockers(),
        ));
    }
    let Some(raw) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
        control.open_resume_picker();
        return CommandResult::ok();
    };
    match control.resolve_resume_source(raw) {
        Ok(ResumeSource::File(path)) => match control.import_session_file(path) {
            Ok(receipt) => CommandResult::with_message_and_action(
                format!(
                    "Imported foreign session as {} ({} entries, leaf {})",
                    receipt.truncated_id, receipt.entry_count, receipt.leaf_display
                ),
                codewhale_command_contract::outcome::SessionAction::SyncSession(receipt.sync),
            ),
            Err(error) => CommandResult::error(error),
        },
        Ok(ResumeSource::Imported(receipt)) => CommandResult::with_message_and_action(
            format!(
                "Imported foreign session as {} ({} entries, leaf {})",
                receipt.truncated_id, receipt.entry_count, receipt.leaf_display
            ),
            codewhale_command_contract::outcome::SessionAction::SyncSession(receipt.sync),
        ),
        Ok(ResumeSource::Session {
            load_path,
            truncated_id,
            title,
        }) => match load_path {
            Some(path) => CommandResult::action(
                codewhale_command_contract::outcome::SessionAction::LoadSession(path),
            ),
            None => CommandResult::message(format!("Resuming session {truncated_id} ({title})")),
        },
        Ok(ResumeSource::NotFound { raw, error }) => CommandResult::error(format!(
            "Cannot resume '{raw}': {error}\nUse `/resume` without args to pick, or pass a session id, or a path to an exported session JSON."
        )),
        Err(error) => CommandResult::error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::super::control_test_support::message;
    use super::*;
    use codewhale_command_contract::facets::ResumeImportReceipt;
    use std::path::PathBuf;

    fn control_fake() -> super::super::control_test_support::FakeControl {
        super::super::control_test_support::FakeControl::default()
    }

    #[test]
    fn resume_transition_blocking_wins_before_any_route() {
        let mut fake = control_fake();
        fake.blocked = true;
        fake.blockers = vec!["the session is still busy with the current turn".to_string()];
        let result = resume_portable(&mut fake, Some("anything"));
        assert!(result.is_error);
        assert_eq!(
            message(&result),
            "Cannot resume while runtime work is active:\n  • the session is still busy with the current turn\n\nWait for the work to finish, or stop what you can: Ctrl+C stops a running turn, and /jobs cancel-all cancels running shell jobs."
        );
        assert_eq!(
            fake.calls.borrow().as_slice(),
            ["transition_blocked", "transition_blockers"],
            "the gate executes exactly once before route work, then names its blockers"
        );
    }

    #[test]
    fn resume_bare_opens_the_picker() {
        let mut fake = control_fake();
        let result = resume_portable(&mut fake, None);
        assert!(!result.is_error);
        assert!(result.action.is_none());
        assert!(result.message.is_none());
        assert_eq!(
            fake.calls.borrow().as_slice(),
            ["transition_blocked", "open_resume_picker"]
        );
    }

    #[test]
    fn resume_file_and_import_routes_compose_exact_receipts() {
        let mut fake = control_fake();
        fake.resume = Some(Ok(ResumeSource::File(PathBuf::from("/tmp/import.json"))));
        fake.import = Some(Ok(ResumeImportReceipt {
            truncated_id: "imp-9".to_string(),
            entry_count: 12,
            leaf_display: "leaf-3".to_string(),
            sync: super::super::lifecycle_test_support::sync_payload("imp-9"),
        }));
        let result = resume_portable(&mut fake, Some("/tmp/import.json"));
        assert!(!result.is_error);
        assert_eq!(
            message(&result),
            "Imported foreign session as imp-9 (12 entries, leaf leaf-3)"
        );
        // The engine must adopt the imported conversation; a message-only
        // result would leave it on the previous session.
        assert!(
            matches!(
                result.action,
                Some(codewhale_command_contract::outcome::SessionAction::SyncSession(codewhale_command_contract::facets::SessionSyncPayload { ref session_id, .. }))
                if session_id.as_deref() == Some("imp-9")
            ),
            "{result:?}"
        );

        let mut fake = control_fake();
        fake.resume = Some(Ok(ResumeSource::File(PathBuf::from("/tmp/x.json"))));
        fake.import = Some(Err(
            "File x.json is not a recognized session export".to_string()
        ));
        let result = resume_portable(&mut fake, Some("/tmp/x.json"));
        assert!(result.is_error);
        assert_eq!(
            message(&result),
            "File x.json is not a recognized session export"
        );

        let mut fake = control_fake();
        fake.resume = Some(Ok(ResumeSource::Imported(ResumeImportReceipt {
            truncated_id: "c-1".to_string(),
            entry_count: 0,
            leaf_display: "(none)".to_string(),
            sync: super::super::lifecycle_test_support::sync_payload("c-1"),
        })));
        let result = resume_portable(&mut fake, Some("inline-json"));
        assert_eq!(
            message(&result),
            "Imported foreign session as c-1 (0 entries, leaf (none))"
        );
        assert!(
            matches!(
                result.action,
                Some(codewhale_command_contract::outcome::SessionAction::SyncSession(codewhale_command_contract::facets::SessionSyncPayload { ref session_id, .. }))
                if session_id.as_deref() == Some("c-1")
            ),
            "{result:?}"
        );
    }

    #[test]
    fn resume_session_and_not_found_routes_are_exact() {
        let mut fake = control_fake();
        fake.resume = Some(Ok(ResumeSource::Session {
            load_path: Some(PathBuf::from("/tmp/sessions/abc123.json")),
            truncated_id: "abc123".to_string(),
            title: "Control Session".to_string(),
        }));
        let result = resume_portable(&mut fake, Some("abc123"));
        assert!(!result.is_error);
        assert!(matches!(
            result.action,
            Some(codewhale_command_contract::outcome::SessionAction::LoadSession(path)) if path == *"/tmp/sessions/abc123.json"
        ));

        let mut fake = control_fake();
        fake.resume = Some(Ok(ResumeSource::Session {
            load_path: None,
            truncated_id: "abc123".to_string(),
            title: "Control Session".to_string(),
        }));
        let result = resume_portable(&mut fake, Some("abc123"));
        assert_eq!(
            message(&result),
            "Resuming session abc123 (Control Session)"
        );

        let mut fake = control_fake();
        fake.resume = Some(Ok(ResumeSource::NotFound {
            raw: "nope".to_string(),
            error: "no such session".to_string(),
        }));
        let result = resume_portable(&mut fake, Some("nope"));
        assert!(result.is_error);
        assert_eq!(
            message(&result),
            "Cannot resume 'nope': no such session\nUse `/resume` without args to pick, or pass a session id, or a path to an exported session JSON."
        );
    }

    #[test]
    fn resume_host_lookup_errors_pass_through() {
        let mut fake = control_fake();
        fake.resume = Some(Err("could not open sessions directory: boom".to_string()));
        let result = resume_portable(&mut fake, Some("abc"));
        assert!(result.is_error);
        assert_eq!(message(&result), "could not open sessions directory: boom");
    }

    #[test]
    fn resume_missing_control_authority_fails_safely() {
        let contexts = codewhale_command_contract::handler::CommandContexts::empty();
        let result = resume_contextual(contexts, None);
        assert!(result.is_error);
        assert_eq!(
            message(&result),
            "Command capability unavailable: session_control"
        );
    }
}
