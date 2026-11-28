use codewhale_command_contract::fleet_lane::ControlOperationHelp;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "lane",
    aliases: &["lanes"],
    usage: "/lane [list|status <lane-id>|interrupt <lane-id>|restart <lane-id>|resume <lane-id>]",
    description_key: "cmd_lane_description",
};

pub struct LaneCmd;

impl<A> RegisterCommand<CommandResult<A>> for LaneCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::LANE,
            handler: lane,
        }
    }
}

fn split_verb(arg: Option<&str>) -> (&str, Option<&str>) {
    let Some(rest) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return ("list", None);
    };
    match rest.split_once(char::is_whitespace) {
        Some((verb, tail)) => (verb, Some(tail.trim())),
        None => (rest, None),
    }
}

fn help_text(operations: &[ControlOperationHelp]) -> String {
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
    for descriptor in operations {
        out.push_str(&format!(
            "\n  {:<28} {:<6} {}\n      CLI: {}\n",
            descriptor.slash_invocation,
            descriptor.authority,
            descriptor.summary,
            descriptor.cli_invocation
        ));
    }
    out
}

fn lane<A>(mut contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult<A> {
    let (verb, target) = split_verb(arg);
    let Some(lane_ctx) = contexts.take_lane() else {
        return CommandResult::error("Command capability unavailable: lane");
    };
    if matches!(verb, "help" | "?") {
        return CommandResult::message(help_text(&lane_ctx.control_operations()));
    }
    let Some(operation) = lane_ctx.resolve_control_verb(verb) else {
        return CommandResult::error(format!(
            "Unknown /lane verb '{verb}'. Use list, status, interrupt, restart, or resume."
        ));
    };
    let receipt = lane_ctx.execute_control(operation, target);
    if receipt.is_error {
        CommandResult::error(receipt.rendered)
    } else {
        CommandResult::message(receipt.rendered)
    }
}
