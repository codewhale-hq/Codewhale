use codewhale_command_contract::core_policy::*;
use codewhale_command_contract::handler::{
    CommandCapabilities as Caps, CommandContexts, CommandHandler,
};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

pub const SECURITY_POLICY_URL: &str = "https://github.com/codewhale-hq/CodeWhale/security/policy";
pub const FEATURE_URL: &str =
    "https://github.com/codewhale-hq/CodeWhale/issues/new?template=feature_request.yml";

pub struct ClearCmd;
impl<A: From<ClearedConversation>> RegisterCommand<CommandResult<A>> for ClearCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "clear",
            aliases: &["qingping"],
            usage: "/clear",
            description_key: "cmd_clear_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: Caps::CLEAR.union(Caps::PRESENTATION),
            handler: |ctx, _| clear(ctx).unwrap_or_else(CommandResult::error),
        }
    }
}

fn clear<A: From<ClearedConversation>>(
    mut ctx: CommandContexts<'_>,
) -> Result<CommandResult<A>, String> {
    let lifecycle = ctx
        .take_clear()
        .ok_or("Command capability unavailable: conversation reset")?;
    let text = ctx
        .into_parts()
        .presentation
        .ok_or("Command capability unavailable: presentation")?;
    let success = text.translate("clear_conversation", &[])?;
    match lifecycle.start_empty_conversation() {
        Ok(receipt) => Ok(CommandResult::with_message_and_action(
            success,
            receipt.into(),
        )),
        Err(ClearFailure::Transition(reason)) => Err(reason),
        Err(ClearFailure::Busy(blockers)) => {
            let mut message = text.translate("clear_conversation_busy", &[])?;
            for blocker in blockers {
                message.push_str("\n  • ");
                message.push_str(&blocker);
            }
            Err(message)
        }
    }
}

pub struct EffortCmd;
impl<A> RegisterCommand<CommandResult<A>> for EffortCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "effort",
            aliases: &["thinking", "reasoning", "reason"],
            usage: "/effort [<none|minimal|low|medium|high|xhigh|ultra|max|auto|off>]",
            description_key: "cmd_effort_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: Caps::EFFORT.union(Caps::PRESENTATION),
            handler: |ctx, arg| effort(ctx, arg).unwrap_or_else(CommandResult::error),
        }
    }
}

fn effort<A>(mut ctx: CommandContexts<'_>, arg: Option<&str>) -> Result<CommandResult<A>, String> {
    let effort = ctx
        .take_effort()
        .ok_or("Command capability unavailable: reasoning effort")?;
    let text = ctx
        .into_parts()
        .presentation
        .ok_or("Command capability unavailable: presentation")?;
    let arg = arg.unwrap_or("").trim().to_ascii_lowercase();
    let label_list = |labels: Vec<String>| {
        if labels.is_empty() {
            "none|minimal|low|medium|high|xhigh|ultra|max|auto|off".to_string()
        } else {
            labels.join("|")
        }
    };
    if arg.is_empty() {
        let labels = label_list(effort.available_labels());
        let (prev, next) = effort.cycle_checked()?;
        return Ok(CommandResult::message(format!(
            "Effort: {prev} → {next} (available: {labels})"
        )));
    }
    match effort.set_supported_effort_checked(&arg) {
        Err(EffortChangeFailure::Receipt(message)) => Err(message),
        Ok(label) => Ok(CommandResult::message(format!("Effort set to {label}"))),
        Err(EffortChangeFailure::Selection(EffortFailure::Unknown)) => Err(format!(
            "{} — Usage: /effort <{}> (also /thinking)",
            text.translate("cmd_effort_description", &[])?,
            label_list(effort.available_labels())
        )),
        Err(EffortChangeFailure::Selection(EffortFailure::Unavailable { model, labels })) => {
            Err(format!(
                "'{arg}' not available for {model} — available: {}",
                label_list(labels)
            ))
        }
    }
}

pub struct FeedbackCmd;
impl<A: From<FeedbackAction>> RegisterCommand<CommandResult<A>> for FeedbackCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "feedback",
            aliases: &[],
            usage: "/feedback [bug [focus]|review <id>|edit <id> <change>|feature|security]",
            description_key: "cmd_feedback_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: Caps::FEEDBACK.union(Caps::PRESENTATION),
            handler: |ctx, arg| feedback(ctx, arg).unwrap_or_else(CommandResult::error),
        }
    }
}

fn feedback<A: From<FeedbackAction>>(
    mut ctx: CommandContexts<'_>,
    arg: Option<&str>,
) -> Result<CommandResult<A>, String> {
    let report = ctx
        .take_feedback()
        .ok_or("Command capability unavailable: feedback")?;
    let text = ctx
        .into_parts()
        .presentation
        .ok_or("Command capability unavailable: presentation")?;
    let help = || {
        text.translate("feedback_help", &[]).map(|intro| format!("{intro}\n\n/feedback bug [focus]\n/feedback review <id>\n/feedback edit <id> <change>\n/feedback feature\n/feedback security"))
    };
    let raw = arg.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok(CommandResult::action(FeedbackAction::OpenPicker.into()));
    }
    let (kind, rest) = raw.split_once(char::is_whitespace).unwrap_or((raw, ""));
    let rest = rest.trim();
    Ok(match kind.to_ascii_lowercase().as_str() {
        "help" | "--help" | "-h" => CommandResult::message(help()?),
        "1" | "bug" | "bug-report" | "bug_report" => {
            if !report.has_session() {
                return Err(text.translate("feedback_no_session", &[])?);
            }
            let focus = if rest.is_empty() {
                String::new()
            } else {
                report.safe_text(rest)?
            };
            CommandResult::with_message_and_action(
                text.translate("feedback_draft_requested", &[])?,
                FeedbackAction::Draft(focus).into(),
            )
        }
        "review" | "edit" => {
            if !report.has_session() {
                return Err(text.translate("feedback_no_session", &[])?);
            };
            let (id, change) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let editing = kind.eq_ignore_ascii_case("edit");
            if id.is_empty()
                || (editing && change.trim().is_empty())
                || (!editing && !change.trim().is_empty())
            {
                return Err(help()?);
            }
            let change = if editing {
                Some(report.safe_text(change)?)
            } else {
                None
            };
            CommandResult::action(
                FeedbackAction::Review {
                    id: id.into(),
                    change,
                }
                .into(),
            )
        }
        "2" | "feature" | "feature-request" | "feature_request" | "enhancement"
            if rest.is_empty() =>
        {
            CommandResult::with_message_and_action(
                format!(
                    "Trying to open GitHub feature request template in your browser. If that fails, open this URL manually:\n\n{FEATURE_URL}"
                ),
                FeedbackAction::Feature.into(),
            )
        }
        "3" | "security" | "vulnerability" | "private" if rest.is_empty() => {
            CommandResult::with_message_and_action(
                format!(
                    "Review the project's security policy before reporting a vulnerability.\n\nTrying to open it in your browser. If that fails, open this URL manually:\n\n{SECURITY_POLICY_URL}\n\nDo not include sensitive security details in a public issue."
                ),
                FeedbackAction::Security.into(),
            )
        }
        _ => CommandResult::error(help()?),
    })
}
