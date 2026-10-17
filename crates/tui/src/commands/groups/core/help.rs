//! `/help` command.

use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::{
    CommandNavigationContext, NavigationSkill, NavigationUserHelp,
};
use codewhale_command_contract::outcome::CoreNavigationCommandResult as CommandResult;
use std::fmt::Write;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "help",
    aliases: &["?", "bangzhu", "帮助"],
    usage: "/help [command]",
    description_key: "cmd_help_description",
};

pub struct HelpCmd;

impl RegisterCommand<CommandResult> for HelpCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION.union(CommandCapabilities::PRESENTATION),
            handler: help,
        }
    }
}

fn help(contexts: CommandContexts<'_>, topic: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(navigation) = parts.navigation else {
        return CommandResult::error("Command capability unavailable: navigation");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    render_help(navigation, presentation, topic).unwrap_or_else(CommandResult::error)
}

fn render_help(
    navigation: &mut dyn CommandNavigationContext,
    presentation: &dyn CommandPresentationContext,
    topic: Option<&str>,
) -> Result<CommandResult, String> {
    if let Some(topic) = topic {
        let facts = navigation.help_topic(topic);
        if let Some(command) = facts.user {
            return Ok(CommandResult::message(user_command_help(
                presentation,
                &command,
            )?));
        }
        if let Some(command) = facts.builtin {
            let mut help = format!(
                "{}\n\n  {}\n\n  {} {}",
                command.name,
                command.description,
                presentation.translate("help_usage_label", &[])?,
                command.usage,
            );
            let visible_aliases = command
                .aliases
                .iter()
                .filter(|alias| !facts.shadowed_names.contains(alias))
                .map(String::as_str)
                .collect::<Vec<_>>();
            if !visible_aliases.is_empty() {
                let _ = write!(
                    help,
                    "\n  {} {}",
                    presentation.translate("help_aliases_label", &[])?,
                    visible_aliases.join(", ")
                );
            }
            if command.name == "config" {
                help.push_str("\n\n  Provider context window: set `context_window = 262144` under the active `[providers.<name>]` table to cap a 1M model to 256K. Use `/config context_window` to inspect the configured and effective values.");
                help.push('\n');
                help.push_str(&presentation.translate("config_help_discoverable", &[])?);
            }
            return Ok(CommandResult::message(help));
        }
        if let Some(help) = skill_help(presentation, &navigation.cached_skills(), topic)? {
            return Ok(CommandResult::message(help));
        }
        return Ok(CommandResult::error(
            presentation.translate("help_unknown_command", &[("topic", topic)])?,
        ));
    }
    if !navigation.help_is_open() {
        navigation.open_help();
    }
    Ok(CommandResult::ok())
}

fn skill_help(
    presentation: &dyn CommandPresentationContext,
    skills: &[NavigationSkill],
    topic: &str,
) -> Result<Option<String>, String> {
    let needle = topic
        .trim()
        .trim_start_matches('$')
        .trim_start_matches('/')
        .trim_start_matches("skill ")
        .trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let Some(skill) = skills
        .iter()
        .find(|skill| skill.name.eq_ignore_ascii_case(needle))
    else {
        return Ok(None);
    };
    let name = &skill.name;
    let mut help = format!("${name}");
    if !skill.description.trim().is_empty() {
        let _ = write!(help, "\n\n  {}", skill.description.trim());
    }
    let _ = write!(
        help,
        "\n\n  {} ${name}\n  {} /skill {name}",
        presentation.translate("help_usage_label", &[])?,
        presentation.translate("help_usage_label", &[])?,
    );
    Ok(Some(help))
}

fn user_command_help(
    presentation: &dyn CommandPresentationContext,
    command: &NavigationUserHelp,
) -> Result<String, String> {
    let mut help = command.name.clone();
    if let Some(description) = command
        .description
        .as_deref()
        .filter(|description| !description.trim().is_empty())
    {
        let _ = write!(help, "\n\n  {description}");
    }
    let usage = command
        .usage
        .clone()
        .unwrap_or_else(|| format!("/{}", command.name));
    let _ = write!(
        help,
        "\n\n  {} {}",
        presentation.translate("help_usage_label", &[])?,
        usage
    );
    if !command.aliases.is_empty() {
        let _ = write!(
            help,
            "\n  {} {}",
            presentation.translate("help_aliases_label", &[])?,
            command.aliases.join(", ")
        );
    }
    Ok(help)
}
