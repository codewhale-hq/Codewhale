//! `/home` command.

use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::NavigationOverview;
use codewhale_command_contract::outcome::CoreNavigationCommandResult as CommandResult;
use codewhale_command_contract::types::CommandMode;
use std::fmt::Write;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "home",
    aliases: &["zhuye", "shouye"],
    usage: "/home",
    description_key: "cmd_home_description",
};

pub struct HomeCmd;
pub struct OverviewCmd;

impl RegisterCommand<CommandResult> for HomeCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION.union(CommandCapabilities::PRESENTATION),
            handler: home,
        }
    }
}

impl RegisterCommand<CommandResult> for OverviewCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "overview",
            aliases: &["stats"],
            usage: "/overview",
            description_key: "cmd_overview_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION.union(CommandCapabilities::PRESENTATION),
            handler: overview,
        }
    }
}

fn home(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(navigation) = parts.navigation else {
        return CommandResult::error("Command capability unavailable: navigation");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    if navigation.home_is_blocked() {
        return match presentation.translate("home_navigation_busy", &[]) {
            Ok(message) => CommandResult::error(message),
            Err(error) => CommandResult::error(error),
        };
    }
    navigation.restore_home();
    CommandResult::ok()
}

fn overview(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(navigation) = parts.navigation else {
        return CommandResult::error("Command capability unavailable: navigation");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    render_overview(presentation, navigation.overview())
        .map(CommandResult::message)
        .unwrap_or_else(CommandResult::error)
}

fn render_overview(
    presentation: &dyn CommandPresentationContext,
    facts: NavigationOverview,
) -> Result<String, String> {
    let label = |key| presentation.translate(key, &[]);
    let mut stats = String::new();
    let _ = writeln!(stats, "{}", label("home_dashboard_title")?);
    let _ = writeln!(stats, "============================================");
    let _ = writeln!(stats, "{}      {}", label("home_model")?, facts.model);
    let _ = writeln!(stats, "{}       {}", label("home_mode")?, facts.mode_label);
    let _ = writeln!(
        stats,
        "{}  {}",
        label("home_workspace")?,
        facts.workspace.display()
    );
    let _ = writeln!(
        stats,
        "{}    {} messages",
        label("home_history")?,
        facts.history_count
    );
    let _ = writeln!(
        stats,
        "{}     {} (session)",
        label("home_tokens")?,
        facts.total_tokens
    );
    if facts.queued_count > 0 {
        let _ = writeln!(
            stats,
            "{}     {} messages",
            label("home_queued")?,
            facts.queued_count
        );
    }
    if facts.subagent_count > 0 {
        let _ = writeln!(
            stats,
            "{} {} active",
            label("home_subagents")?,
            facts.subagent_count
        );
    }
    if let Some(skill) = &facts.active_skill {
        let _ = writeln!(stats, "{}      {} (active)", label("home_skill")?, skill);
    }
    let _ = writeln!(stats, "\n{}", label("home_quick_actions")?);
    let _ = writeln!(stats, "--------------------------------------------");
    for key in [
        "home_quick_workspace",
        "home_quick_restore",
        "home_quick_tokens",
        "home_quick_links",
        "home_quick_skills",
        "home_quick_config",
        "home_quick_settings",
        "home_quick_model",
        "home_quick_subagents",
        "home_quick_task_list",
        "home_quick_help",
    ] {
        let _ = writeln!(stats, "{}", label(key)?);
    }
    let _ = writeln!(stats, "\n{}", label("home_mode_tips")?);
    let _ = writeln!(stats, "--------------------------------------------");
    let tips: &[&str] = match facts.mode {
        CommandMode::Agent => &[
            "home_agent_mode_tip",
            "home_agent_mode_review_tip",
            "home_agent_mode_yolo_tip",
        ],
        CommandMode::Operate => &["home_operate_mode_tip", "home_operate_mode_fleet_tip"],
        CommandMode::Plan => &["home_plan_mode_tip", "home_plan_mode_checklist_tip"],
    };
    for key in tips {
        let _ = writeln!(stats, "{}", label(key)?);
    }
    Ok(stats)
}
