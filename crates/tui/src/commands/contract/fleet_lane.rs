use super::SharedCommandHost;
use crate::tui::app::App;
use codewhale_command_contract::fleet_lane::{
    CommandFleetContext, CommandLaneContext, ControlOperationHelp, ControlReceiptOutcome,
    FleetControlVerb, FleetRouteSaveKind, FleetUsageKind, LaneControlVerb,
};
use codewhale_command_contract::navigation::{CommandNavigationContext, SubagentRosterDisplay};
use codewhale_engine::fleet::control::execute_fleet_control;
use codewhale_lane::control::{execute_lane_control, operations_for_domain};
use codewhale_lane::{ControlDomain, ControlOperation, ControlSurface};
use codewhale_localization::{MessageId, tr};

pub(super) struct FleetAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

pub(super) struct LaneAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

fn control_operations_for(domain: ControlDomain) -> Vec<ControlOperationHelp> {
    operations_for_domain(domain)
        .into_iter()
        .map(|descriptor| ControlOperationHelp {
            slash_invocation: descriptor.slash_invocation(),
            authority: descriptor.authority.as_str().to_string(),
            summary: descriptor.summary.to_string(),
            cli_invocation: descriptor.cli_invocation.to_string(),
        })
        .collect()
}

fn lane_operation(verb: LaneControlVerb) -> ControlOperation {
    match verb {
        LaneControlVerb::List => ControlOperation::LaneList,
        LaneControlVerb::Status => ControlOperation::LaneStatus,
        LaneControlVerb::Interrupt => ControlOperation::LaneInterrupt,
        LaneControlVerb::Restart => ControlOperation::LaneRestart,
        LaneControlVerb::Resume => ControlOperation::LaneResume,
    }
}

fn fleet_operation(verb: FleetControlVerb) -> ControlOperation {
    match verb {
        FleetControlVerb::List => ControlOperation::FleetList,
        FleetControlVerb::Status => ControlOperation::FleetStatus,
        FleetControlVerb::Interrupt => ControlOperation::FleetInterrupt,
        FleetControlVerb::Restart => ControlOperation::FleetRestart,
        FleetControlVerb::Resume => ControlOperation::FleetResume,
    }
}

fn fleet_models_text(app: &App) -> String {
    use codewhale_engine::fleet::members::fleet_models;
    let locale = app.ui_locale;
    let models = match fleet_models(&app.workspace) {
        Ok(models) if models.is_empty() => {
            return tr(locale, MessageId::FleetModelsEmpty).into_owned();
        }
        Ok(models) => models,
        Err(error) => {
            return tr(locale, MessageId::FleetModelsBroken).replace("{error}", &error.to_string());
        }
    };
    let mut lines = vec![
        tr(locale, MessageId::FleetModelsHeader)
            .replace("{fleet}", &models[0].fleet)
            .replace("{count}", &models.len().to_string()),
    ];
    for member in &models {
        let provider = codewhale_config::ProviderKind::parse(&member.provider);
        let facts = provider
            .and_then(|p| {
                codewhale_engine::provider_lake::catalog_offering_for_model(p, &member.model)
            })
            .map(|row| {
                let mut parts = Vec::new();
                if let Some(cost) = row.cost.as_ref()
                    && let (Some(input), Some(output)) = (cost.input, cost.output)
                {
                    parts.push(
                        tr(locale, MessageId::FleetModelsFactPrice)
                            .replace("{input}", &format!("{input:.2}"))
                            .replace("{output}", &format!("{output:.2}")),
                    );
                }
                if let Some(limit) = row.limit.as_ref()
                    && let Some(context) = limit.context
                {
                    parts.push(
                        tr(locale, MessageId::FleetModelsFactContext)
                            .replace("{context}", &(context / 1000).to_string()),
                    );
                }
                if row.tool_call == Some(true) {
                    parts.push(tr(locale, MessageId::FleetModelsFactTools).into_owned());
                }
                parts.join(" · ")
            })
            .filter(|facts| !facts.is_empty())
            .map(|facts| format!(" · {facts}"))
            .unwrap_or_default();
        lines.push(format!(
            "  {}/{} · {}{facts}",
            member.provider,
            member.model,
            member.roles_label()
        ));
    }
    lines.push(tr(locale, MessageId::FleetModelsFooter).into_owned());
    lines.join("\n")
}

impl CommandLaneContext for LaneAdapter<'_> {
    fn control_operations(&self) -> Vec<ControlOperationHelp> {
        control_operations_for(ControlDomain::Lane)
    }

    fn resolve_control_verb(&self, verb: &str) -> Option<LaneControlVerb> {
        match ControlOperation::parse_verb(ControlDomain::Lane, verb)? {
            ControlOperation::LaneList => Some(LaneControlVerb::List),
            ControlOperation::LaneStatus => Some(LaneControlVerb::Status),
            ControlOperation::LaneInterrupt => Some(LaneControlVerb::Interrupt),
            ControlOperation::LaneRestart => Some(LaneControlVerb::Restart),
            ControlOperation::LaneResume => Some(LaneControlVerb::Resume),
            _ => None,
        }
    }

    fn execute_control(
        &mut self,
        verb: LaneControlVerb,
        target: Option<&str>,
    ) -> ControlReceiptOutcome {
        let operation = lane_operation(verb);
        let receipt = if operation.descriptor().authority.is_write() {
            self.host
                .app
                .borrow_mut()
                .lane_control
                .submit(operation, target, None)
        } else {
            execute_lane_control(ControlSurface::Slash, operation, target)
        };
        ControlReceiptOutcome {
            rendered: receipt.render(),
            is_error: receipt.is_error(),
        }
    }
}

impl CommandFleetContext for FleetAdapter<'_> {
    fn control_operations(&self) -> Vec<ControlOperationHelp> {
        control_operations_for(ControlDomain::Fleet)
    }

    fn resolve_control_verb(&self, verb: &str) -> Option<FleetControlVerb> {
        match ControlOperation::parse_verb(ControlDomain::Fleet, verb)? {
            ControlOperation::FleetList => Some(FleetControlVerb::List),
            ControlOperation::FleetStatus => Some(FleetControlVerb::Status),
            ControlOperation::FleetInterrupt => Some(FleetControlVerb::Interrupt),
            ControlOperation::FleetRestart => Some(FleetControlVerb::Restart),
            ControlOperation::FleetResume => Some(FleetControlVerb::Resume),
            _ => None,
        }
    }

    fn run_control(
        &mut self,
        verb: FleetControlVerb,
        target: Option<&str>,
    ) -> ControlReceiptOutcome {
        let receipt = execute_fleet_control(
            ControlSurface::Slash,
            &self.host.app.borrow().workspace,
            fleet_operation(verb),
            target,
        );
        ControlReceiptOutcome {
            rendered: receipt.render(),
            is_error: receipt.is_error(),
        }
    }

    fn fleet_models_text(&self) -> String {
        fleet_models_text(&self.host.app.borrow())
    }

    fn save_route(&mut self, kind: FleetRouteSaveKind) -> String {
        let choice = match kind {
            FleetRouteSaveKind::UpdateFleet => {
                crate::tui::views::route_save_prompt::RouteSaveChoice::UpdateFleet
            }
            FleetRouteSaveKind::SaveAsNewFleet => {
                crate::tui::views::route_save_prompt::RouteSaveChoice::SaveAsNewFleet
            }
        };
        self.host.app.borrow_mut().apply_route_save_choice(choice)
    }

    fn usage_text(&self, kind: FleetUsageKind) -> String {
        let id = match kind {
            FleetUsageKind::Add => MessageId::FleetAddUsage,
            FleetUsageKind::Remove => MessageId::FleetRemoveUsage,
        };
        tr(self.host.app.borrow().ui_locale, id).into_owned()
    }

    fn prepare_workers_view(&mut self) {
        super::NavigationAdapter {
            host: self.host.clone(),
        }
        .prepare_subagent_roster(SubagentRosterDisplay::Modal);
    }
}
