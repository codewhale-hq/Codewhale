mod fleet;
mod lane;

pub use fleet::FleetCmd;
pub use lane::LaneCmd;

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::fleet_lane::{
        CommandFleetContext, CommandLaneContext, ControlOperationHelp, ControlReceiptOutcome,
        FleetAction, FleetControlVerb, FleetRouteSaveKind, FleetUsageKind, LaneControlVerb,
    };
    use codewhale_command_contract::handler::{
        CommandCapabilities, CommandContexts, CommandHandler,
    };
    use codewhale_command_contract::metadata::RegisterCommand;
    use codewhale_command_contract::outcome::CommandResult;

    struct FakeLane {
        calls: Vec<(LaneControlVerb, Option<String>)>,
    }

    impl CommandLaneContext for FakeLane {
        fn control_operations(&self) -> Vec<ControlOperationHelp> {
            vec![ControlOperationHelp {
                slash_invocation: "/lane list".into(),
                authority: "read".into(),
                summary: "List lanes".into(),
                cli_invocation: "codewhale lane list".into(),
            }]
        }

        fn resolve_control_verb(&self, verb: &str) -> Option<LaneControlVerb> {
            match verb {
                "list" => Some(LaneControlVerb::List),
                "stop" => Some(LaneControlVerb::Interrupt),
                _ => None,
            }
        }

        fn execute_control(
            &mut self,
            verb: LaneControlVerb,
            target: Option<&str>,
        ) -> ControlReceiptOutcome {
            self.calls.push((verb, target.map(str::to_string)));
            ControlReceiptOutcome {
                rendered: format!("lane:{verb:?}:{target:?}"),
                is_error: false,
            }
        }
    }

    struct FakeFleet {
        workers_prepared: bool,
        last_control: Option<(FleetControlVerb, Option<String>)>,
    }

    impl CommandFleetContext for FakeFleet {
        fn control_operations(&self) -> Vec<ControlOperationHelp> {
            vec![ControlOperationHelp {
                slash_invocation: "/fleet list".into(),
                authority: "read".into(),
                summary: "List fleet runs".into(),
                cli_invocation: "codewhale fleet list".into(),
            }]
        }

        fn resolve_control_verb(&self, verb: &str) -> Option<FleetControlVerb> {
            match verb {
                "status" => Some(FleetControlVerb::Status),
                _ => None,
            }
        }

        fn run_control(
            &mut self,
            verb: FleetControlVerb,
            target: Option<&str>,
        ) -> ControlReceiptOutcome {
            self.last_control = Some((verb, target.map(str::to_string)));
            ControlReceiptOutcome {
                rendered: format!("fleet:{verb:?}:{target:?}"),
                is_error: false,
            }
        }

        fn fleet_models_text(&self) -> String {
            "models-ok".into()
        }

        fn save_route(&mut self, kind: FleetRouteSaveKind) -> String {
            format!("saved:{kind:?}")
        }

        fn usage_text(&self, kind: FleetUsageKind) -> String {
            format!("Usage: /fleet {kind:?}")
        }

        fn prepare_workers_view(&mut self) {
            self.workers_prepared = true;
        }
    }

    struct RejectingAction;
    impl TryFrom<FleetAction> for RejectingAction {
        type Error = String;
        fn try_from(_: FleetAction) -> Result<Self, String> {
            Err("action rejected by host".into())
        }
    }

    #[test]
    fn fleet_and_lane_declare_exact_capability_bits() {
        assert_eq!(
            match <FleetCmd as RegisterCommand<CommandResult<FleetAction>>>::handler() {
                CommandHandler::Contextual { capabilities, .. } => capabilities,
                CommandHandler::Pure(_) => panic!("expected contextual"),
            },
            CommandCapabilities::FLEET
        );
        assert_eq!(
            match <LaneCmd as RegisterCommand<CommandResult<std::convert::Infallible>>>::handler() {
                CommandHandler::Contextual { capabilities, .. } => capabilities,
                CommandHandler::Pure(_) => panic!("expected contextual"),
            },
            CommandCapabilities::LANE
        );
    }

    #[test]
    fn lane_rejects_unknown_verbs_and_routes_through_single_control_method() {
        let CommandHandler::Contextual { handler, .. } =
            <LaneCmd as RegisterCommand<CommandResult<std::convert::Infallible>>>::handler()
        else {
            panic!("expected contextual");
        };
        let mut lane = FakeLane { calls: Vec::new() };
        let rejected = handler(
            CommandContexts::empty().with_lane(&mut lane),
            Some("obliterate lane-1"),
        );
        assert!(rejected.is_error);
        assert!(
            rejected
                .message
                .as_deref()
                .is_some_and(|m| m.contains("Unknown /lane verb 'obliterate'"))
        );
        assert!(lane.calls.is_empty());

        let listed = handler(CommandContexts::empty().with_lane(&mut lane), None);
        assert!(!listed.is_error);
        assert_eq!(listed.message.as_deref(), Some("lane:List:None"));

        let stopped = handler(
            CommandContexts::empty().with_lane(&mut lane),
            Some("stop lane-1@2"),
        );
        assert!(!stopped.is_error);
        assert_eq!(
            lane.calls,
            vec![
                (LaneControlVerb::List, None),
                (LaneControlVerb::Interrupt, Some("lane-1@2".to_string())),
            ]
        );
    }

    #[test]
    fn fleet_parses_actions_validates_with_try_from_and_routes_control() {
        let CommandHandler::Contextual { handler, .. } =
            <FleetCmd as RegisterCommand<CommandResult<FleetAction>>>::handler()
        else {
            panic!("expected contextual");
        };
        let open = handler(CommandContexts::empty(), None);
        assert_eq!(open.action, Some(FleetAction::OpenRoster));

        let added = handler(
            CommandContexts::empty(),
            Some("add openrouter z-ai/glm-5.3-flash explore"),
        );
        assert_eq!(
            added.action,
            Some(FleetAction::AddModel {
                provider: "openrouter".into(),
                model: "z-ai/glm-5.3-flash".into(),
                roles: vec!["explore".into()],
            })
        );

        let mut fleet = FakeFleet {
            workers_prepared: false,
            last_control: None,
        };
        let workers = handler(
            CommandContexts::empty().with_fleet(&mut fleet),
            Some("workers"),
        );
        assert!(fleet.workers_prepared);
        assert_eq!(workers.action, Some(FleetAction::OpenWorkers));

        let status = handler(
            CommandContexts::empty().with_fleet(&mut fleet),
            Some("status"),
        );
        assert!(!status.is_error);
        assert_eq!(fleet.last_control, Some((FleetControlVerb::Status, None)));

        let CommandHandler::Contextual {
            handler: reject_handler,
            ..
        } = <FleetCmd as RegisterCommand<CommandResult<RejectingAction>>>::handler()
        else {
            panic!("expected contextual");
        };
        let rejected = reject_handler(CommandContexts::empty(), None);
        assert!(rejected.is_error);
        assert!(rejected.action.is_none());
        assert_eq!(
            rejected.message.as_deref(),
            Some("Error: action rejected by host")
        );
    }
}
