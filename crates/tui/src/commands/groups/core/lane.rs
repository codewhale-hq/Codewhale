//! `/lane` command — durable Lane control from the composer (#1888, #4022).
//!
//! A **Lane** is one running Workflow. This command does not reimplement Lane
//! lifecycle: it resolves the verb through the shared control-plane contract
//! in `codewhale-lane` and calls the same executor `codewhale lane …` calls,
//! so the slash surface, its hotbar action, and the CLI produce identical
//! availability, target selection, outcomes, and receipts.

use codewhale_lane::control::{execute_lane_control, operations_for_domain};
use codewhale_lane::{ControlDomain, ControlOperation, ControlSurface};

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::App;
use codewhale_localization::MessageId;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "lane",
    aliases: &["lanes"],
    usage: "/lane [list|status <lane-id>|interrupt <lane-id>|restart <lane-id>|resume <lane-id>]",
    description_id: MessageId::CmdLaneDescription,
};

pub(in crate::commands) struct LaneCmd;

/// Split `"<verb> <rest>"` into the verb and its raw target tail.
fn split_verb(arg: Option<&str>) -> (&str, Option<&str>) {
    let Some(rest) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        // Bare `/lane` (and therefore a bare hotbar dispatch) lists, matching
        // `codewhale lane list`. Listing is read-only, so a one-key hotbar
        // press can never mutate durable state.
        return ("list", None);
    };
    match rest.split_once(char::is_whitespace) {
        Some((verb, tail)) => (verb, Some(tail.trim())),
        None => (rest, None),
    }
}

fn help_text() -> String {
    let mut out = String::from(
        "Usage: /lane [list|status <lane-id>|interrupt <lane-id>|restart <lane-id>|resume <lane-id>]\n\n\
         A Lane is one running Workflow; Runtime owns where/how it runs. These verbs act on the \
         durable Lane registry under $CODEWHALE_HOME/lanes/, the same records `codewhale lane` \
         reads. Append @<lifecycle-seq> to a lane id to fence a write to the exact generation you \
         observed.\n\n\
         Reads here do not reconcile: folding a finished Runtime exit into the record means \
         probing tmux and taking a lock, which would block the composer. Statuses are as last \
         recorded — `codewhale lane list` reconciles. Interrupt is submitted to an off-loop \
         worker and answered immediately with a queued receipt and a ticket; the terminal \
         result (transitioned, no_change, or conflict) arrives under that ticket.\n",
    );
    for descriptor in operations_for_domain(ControlDomain::Lane) {
        out.push_str(&format!(
            "\n  {:<28} {:<6} {}\n      CLI: {}\n",
            descriptor.slash_invocation(),
            descriptor.authority.as_str(),
            descriptor.summary,
            descriptor.cli_invocation
        ));
    }
    out
}

impl RegisterCommand for LaneCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        let (verb, target) = split_verb(arg);
        if matches!(verb, "help" | "?") {
            return CommandResult::message(help_text());
        }
        let Some(operation) = ControlOperation::parse_verb(ControlDomain::Lane, verb) else {
            return CommandResult::error(format!(
                "Unknown /lane verb '{verb}'. Use list, status, interrupt, restart, or resume."
            ));
        };
        // Writes tear down a Runtime (subprocess + advisory lock). They are
        // submitted to the off-loop worker and answered with a `queued`
        // receipt; reads run inline because they only touch the registry.
        let receipt = if operation.descriptor().authority.is_write() {
            app.lane_control.submit(operation, target, None)
        } else {
            execute_lane_control(ControlSurface::Slash, operation, target)
        };
        let rendered = receipt.render();
        if receipt.is_error() {
            CommandResult::error(rendered)
        } else {
            CommandResult::message(rendered)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn bare_lane_and_list_resolve_to_the_same_read_verb() {
        assert_eq!(split_verb(None), ("list", None));
        assert_eq!(split_verb(Some("   ")), ("list", None));
        assert_eq!(split_verb(Some("list")), ("list", None));
        assert_eq!(
            split_verb(Some("status lane-a1b2c3d4")),
            ("status", Some("lane-a1b2c3d4"))
        );
        assert_eq!(
            split_verb(Some("interrupt  lane-a1b2c3d4@3 ")),
            ("interrupt", Some("lane-a1b2c3d4@3"))
        );
    }

    #[test]
    fn every_slash_verb_maps_onto_a_shared_lane_operation() {
        for descriptor in operations_for_domain(ControlDomain::Lane) {
            let (verb, _) = split_verb(Some(descriptor.verb));
            assert_eq!(
                ControlOperation::parse_verb(ControlDomain::Lane, verb),
                Some(descriptor.operation),
                "/lane {} must resolve to {}",
                descriptor.verb,
                descriptor.id
            );
        }
        // Compatibility spellings resolve onto the same verbs, never new ones.
        for (spelling, expected) in [
            ("stop", ControlOperation::LaneInterrupt),
            ("cancel", ControlOperation::LaneInterrupt),
            ("inspect", ControlOperation::LaneStatus),
            ("ls", ControlOperation::LaneList),
        ] {
            assert_eq!(
                ControlOperation::parse_verb(ControlDomain::Lane, spelling),
                Some(expected)
            );
        }
    }

    #[test]
    fn unknown_verbs_are_rejected_without_touching_the_registry() {
        let mut app = test_app();
        let result = LaneCmd::execute(&mut app, Some("obliterate lane-a1b2c3d4"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown /lane verb 'obliterate'"))
        );
    }

    #[test]
    fn help_lists_every_verb_with_its_authority_and_cli_twin() {
        let help = help_text();
        for descriptor in operations_for_domain(ControlDomain::Lane) {
            assert!(
                help.contains(descriptor.cli_invocation),
                "help must name the CLI twin of {}",
                descriptor.id
            );
            assert!(
                help.contains(&descriptor.slash_invocation()),
                "help must name the slash form of {}",
                descriptor.id
            );
        }
        assert!(help.contains("read"));
        assert!(help.contains("write"));
        assert!(
            help.contains("$CODEWHALE_HOME/lanes/"),
            "help must say which durable store it reads"
        );
    }

    #[test]
    fn restart_and_resume_report_that_they_have_no_backend() {
        // #1888: a surface must never advertise a backend that does not exist.
        let mut app = test_app();
        for verb in ["restart", "resume"] {
            let result = LaneCmd::execute(&mut app, Some(&format!("{verb} lane-a1b2c3d4")));
            assert!(result.is_error, "/lane {verb}");
            let message = result.message.as_deref().unwrap_or_default();
            assert!(
                message.contains("backend_not_implemented") || message.contains("no_lane_registry"),
                "/lane {verb} must explain itself, got: {message}"
            );
        }
    }

    #[test]
    fn slash_command_and_cli_agree_on_lane_verb_ids() {
        for descriptor in operations_for_domain(ControlDomain::Lane) {
            assert_eq!(descriptor.slash_command, COMMAND_INFO.name);
            assert!(
                COMMAND_INFO.usage.contains(descriptor.verb),
                "/lane usage must document {}",
                descriptor.verb
            );
            assert!(descriptor.offers(ControlSurface::Slash));
            assert!(descriptor.offers(ControlSurface::Cli));
        }
    }

    /// #4022: only `lane.list` is reachable from a bare hotbar press, and the
    /// hotbar reports the slash surface because that is what actually runs.
    #[test]
    fn only_the_bare_dispatch_verb_is_hotbar_reachable() {
        for descriptor in operations_for_domain(ControlDomain::Lane) {
            let (verb, target) = split_verb(None);
            let bare = ControlOperation::parse_verb(ControlDomain::Lane, verb);
            assert_eq!(target, None);
            if descriptor.hotbar_bare_dispatch {
                assert_eq!(
                    bare,
                    Some(descriptor.operation),
                    "{} claims bare dispatch but `/lane` resolves elsewhere",
                    descriptor.id
                );
            } else {
                assert_ne!(bare, Some(descriptor.operation), "{}", descriptor.id);
            }
        }
    }

    #[test]
    fn bare_lane_is_read_only_so_the_hotbar_cannot_mutate_state() {
        // The hotbar registers one action per slash command and fires it with
        // no arguments, so a bare `/lane` must resolve to a read verb.
        assert_eq!(split_verb(None).0, "list");
        let descriptor = ControlOperation::LaneList.descriptor();
        assert_eq!(
            descriptor.authority,
            codewhale_lane::ControlAuthority::Read,
            "a bare hotbar press must not be a write"
        );
        assert!(
            !COMMAND_INFO.requires_required_argument(),
            "/lane must be directly runnable from the palette and hotbar"
        );
        assert_eq!(descriptor.hotbar_action_id(), "slash.lane");
    }
}
