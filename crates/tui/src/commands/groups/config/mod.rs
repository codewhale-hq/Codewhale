//! Config command area: settings, modes, themes, trust, and status surfaces.

// This group dir intentionally has a `config.rs` child module with the same
// name. The module_inception allow is a permanent structure rationale, not
// migration scaffolding; see docs/architecture/command-dispatch.md.
#[allow(clippy::module_inception)]
pub mod config;
mod import_claude;
pub(in crate::commands) mod policy;

use crate::commands::CommandResult;
use crate::commands::traits::{Command, CommandGroup, CommandInfo, FunctionCommand};
use crate::tui::app::App;
use codewhale_localization::MessageId;

pub struct ConfigCommands;

impl CommandGroup for ConfigCommands {
    fn commands(&self) -> &'static [Box<dyn Command>] {
        cached_command_list!(vec![
            Box::new(FunctionCommand::new(&CONFIG_INFO, run_config)),
            Box::new(FunctionCommand::new(&IMPORT_CLAUDE_INFO, run_import_claude)),
            crate::commands::config_policy_host::permissions_registration(),
            Box::new(FunctionCommand::new(&LOGIN_INFO, run_login)),
            Box::new(FunctionCommand::new(&AUTH_INFO, run_auth)),
            Box::new(FunctionCommand::new(&RAIL_INFO, run_rail)),
            Box::new(FunctionCommand::new(&PET_INFO, run_pet)),
            Box::new(FunctionCommand::new(&SETTINGS_INFO, run_settings)),
            crate::commands::config_policy_host::status_registration(),
            Box::new(FunctionCommand::new(&STATUSLINE_INFO, run_statusline)),
            Box::new(FunctionCommand::new(&MODE_INFO, run_mode)),
            Box::new(FunctionCommand::new(&FULLSCREEN_INFO, run_fullscreen)),
            Box::new(FunctionCommand::new(&INLINE_INFO, run_inline)),
            Box::new(FunctionCommand::new(&THEME_INFO, run_theme)),
            Box::new(FunctionCommand::new(&VERBOSE_INFO, run_verbose)),
            Box::new(FunctionCommand::new(&TRUST_INFO, run_trust)),
            Box::new(FunctionCommand::new(&LOGOUT_INFO, run_logout)),
        ])
    }
}

static CONFIG_INFO: CommandInfo = CommandInfo {
    name: "config",
    aliases: &[],
    usage: "/config [ask-rules|status|<key> [value]]",
    description_id: MessageId::CmdConfigDescription,
};
static IMPORT_CLAUDE_INFO: CommandInfo = CommandInfo {
    name: "import-claude",
    aliases: &["import_claude"],
    usage: "/import-claude [--apply]",
    description_id: MessageId::CmdImportClaudeDescription,
};
static LOGIN_INFO: CommandInfo = CommandInfo {
    name: "login",
    aliases: &[],
    usage: "/login [status|account|key|<provider>]",
    description_id: MessageId::CmdLoginDescription,
};
static AUTH_INFO: CommandInfo = CommandInfo {
    name: "auth",
    aliases: &[],
    usage: "/auth xai-device|chatgpt|chatgpt-revoke|orcarouter|orcarouter-revoke",
    description_id: MessageId::CmdAuthDescription,
};
static RAIL_INFO: CommandInfo = CommandInfo {
    name: "workbar",
    // /rail and /sidebar are the names users already know; both now drive
    // the one workbar.
    aliases: &["rail", "sidebar"],
    usage: "/workbar [bottom|top|left|right|off|tasks|agents|context|pinned] [--save]",
    description_id: MessageId::CmdSidebarDescription,
};
static PET_INFO: CommandInfo = CommandInfo {
    name: "pet",
    aliases: &[],
    usage: "/pet [on|off|inspect|status|appearance|window|source|export|sound on|off|avatar [key]|action [name|live]|view [name|live]]",
    description_id: MessageId::CmdPetDescription,
};
static SETTINGS_INFO: CommandInfo = CommandInfo {
    name: "settings",
    aliases: &[],
    usage: "/settings [text]",
    description_id: MessageId::CmdSettingsDescription,
};
static STATUSLINE_INFO: CommandInfo = CommandInfo {
    name: "statusline",
    aliases: &[],
    usage: "/statusline",
    description_id: MessageId::CmdStatuslineDescription,
};
static MODE_INFO: CommandInfo = CommandInfo {
    name: "mode",
    aliases: &["jihua", "zidong"],
    usage: "/mode [act|plan|operate|1|2|3]",
    description_id: MessageId::CmdModeDescription,
};
static FULLSCREEN_INFO: CommandInfo = CommandInfo {
    name: "fullscreen",
    aliases: &[],
    usage: "/fullscreen",
    description_id: MessageId::CmdFullscreenDescription,
};
static INLINE_INFO: CommandInfo = CommandInfo {
    name: "inline",
    aliases: &[],
    usage: "/inline",
    description_id: MessageId::CmdInlineDescription,
};
static THEME_INFO: CommandInfo = CommandInfo {
    name: "theme",
    aliases: &[],
    usage: "/theme [name|underwater|custom:<name>|schema|path]",
    description_id: MessageId::CmdThemeDescription,
};
static VERBOSE_INFO: CommandInfo = CommandInfo {
    name: "verbose",
    aliases: &[],
    usage: "/verbose [on|off]",
    description_id: MessageId::CmdVerboseDescription,
};
static TRUST_INFO: CommandInfo = CommandInfo {
    name: "trust",
    aliases: &["xinren"],
    usage: "/trust [on|off [--save]|add <path>|remove <path>|list]",
    description_id: MessageId::CmdTrustDescription,
};
static LOGOUT_INFO: CommandInfo = CommandInfo {
    name: "logout",
    aliases: &[],
    usage: "/logout [<provider>]",
    description_id: MessageId::CmdLogoutDescription,
};
fn run_registered(app: &mut App, name: &str, arg: Option<&str>) -> CommandResult {
    dispatch(app, name, arg).expect("registered config command should dispatch")
}

fn run_config(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "config", arg)
}
fn run_import_claude(app: &mut App, arg: Option<&str>) -> CommandResult {
    import_claude::import_claude_command(app, arg)
}
fn run_login(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "login", arg)
}
fn run_auth(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "auth", arg)
}
fn run_rail(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "workbar", arg)
}
fn run_pet(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "pet", arg)
}
fn run_settings(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "settings", arg)
}
fn run_statusline(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "statusline", arg)
}
fn run_mode(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "mode", arg)
}
fn run_fullscreen(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "fullscreen", arg)
}
fn run_inline(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "inline", arg)
}
fn run_theme(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "theme", arg)
}
fn run_verbose(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "verbose", arg)
}
fn run_trust(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "trust", arg)
}
fn run_logout(app: &mut App, arg: Option<&str>) -> CommandResult {
    run_registered(app, "logout", arg)
}
pub(in crate::commands) fn dispatch(
    app: &mut App,
    command: &str,
    arg: Option<&str>,
) -> Option<CommandResult> {
    let result = match command {
        "config" => config::config_command(app, arg),
        "permissions" | "permission-rules" | "permission_rules" => {
            crate::commands::config_policy_host::permissions(app, arg)
        }
        "login" => config::login(app, arg),
        "auth" => match arg.map(str::trim) {
            Some("xai-device") | Some("xai_device") => {
                CommandResult::action(crate::tui::app::AppAction::StartXaiDeviceLogin)
            }
            Some("claude") | Some("anthropic") => {
                CommandResult::action(crate::tui::app::AppAction::StartClaudeLogin)
            }
            Some("claude-revoke") => {
                CommandResult::action(crate::tui::app::AppAction::StartClaudeRevoke)
            }
            Some("chatgpt") | Some("openai-codex") | Some("openai_codex") => {
                CommandResult::action(crate::tui::app::AppAction::StartChatgptPkceLogin)
            }
            Some("chatgpt-revoke") | Some("chatgpt_revoke") => {
                CommandResult::action(crate::tui::app::AppAction::StartChatgptRevoke)
            }
            Some("orcarouter") | Some("orca") => {
                CommandResult::action(crate::tui::app::AppAction::StartOrcarouterPkceLogin)
            }
            Some("orcarouter-revoke") | Some("orcarouter_revoke") | Some("orca-revoke") => {
                CommandResult::action(crate::tui::app::AppAction::StartOrcarouterRevoke)
            }
            _ => CommandResult::error(
                "Usage: /auth xai-device|chatgpt|chatgpt-revoke|claude|claude-revoke|orcarouter|orcarouter-revoke",
            ),
        },
        "workbar" | "rail" | "sidebar" => config::sidebar(app, arg),
        "pet" => config::pet(app, arg),
        "settings" => config::settings_command(app, arg),
        "status" => crate::commands::config_policy_host::status(app, arg),
        "statusline" => config::status_line(app),
        "mode" => config::mode(app, arg),
        "fullscreen" => config::screen(app, crate::tui::app::ScreenMode::Fullscreen, arg),
        "inline" => config::screen(app, crate::tui::app::ScreenMode::Inline, arg),
        "jihua" => config::mode(app, Some("plan")),
        "zidong" => config::mode(app, Some("yolo")),
        "theme" => config::theme(app, arg),
        "verbose" => config::verbose(app, arg),
        "trust" | "xinren" => config::trust(app, arg),
        "logout" => match arg.map(str::trim).filter(|arg| !arg.is_empty()) {
            None => config::logout(app),
            Some(provider)
                if app
                    .plugin_registry
                    .active_plugins()
                    .into_iter()
                    .any(|plugin| plugin.manifest.providers.contains_key(provider)) =>
            {
                CommandResult::action(crate::tui::app::AppAction::StartPluginLogout {
                    provider: provider.to_owned(),
                })
            }
            Some(_) => CommandResult::error("Usage: /logout [<enabled-plugin-provider>]"),
        },
        _ => return None,
    };
    Some(result)
}

/// `/workflow settings` and `/config workflow`: the effective `[workflow]`
/// and `[goal]` tables with what each value does, read from the refreshed
/// session table after a config.toml reload (no model turn). The workflow
/// tool reads the same table, so the two surfaces cannot disagree. This
/// surface explains, it does not edit.
pub(in crate::commands) fn workflow_settings(app: &App) -> CommandResult {
    let refreshed = crate::tools::workflow::session_workflow_config(&app.workspace);
    let cfg = refreshed.as_ref().unwrap_or(&app.workflow_config);
    let on = |value: bool| if value { "on" } else { "off" };
    let lines = [
        "[workflow] — config.toml".to_string(),
        format!(
            "automatic = {}  · the agent may start a workflow itself for broad or staged work; off means only /workflow starts one",
            on(cfg.automatic)
        ),
        format!(
            "auto_start_read_only = {}  · read-only plans start without an approval card",
            on(cfg.auto_start_read_only)
        ),
        format!(
            "require_approval_for_writes = {}  · plans that write, use shell/network, or elevate show an approval card first",
            on(cfg.require_approval_for_writes)
        ),
        format!(
            "max_children = {} · max_concurrent = {} · max_depth = {}  · hard ceilings for one run",
            cfg.max_children, cfg.max_concurrent, cfg.max_depth
        ),
        format!(
            "default_token_budget = {}  · shared admission cap for a run and its children (0 = none)",
            cfg.default_token_budget
        ),
        String::new(),
        "[goal] — config.toml".to_string(),
        format!(
            "max_continuations = {}  · automatic continuation passes before a goal pauses; 0 = unlimited (completion, blocked, or you stop it)",
            app.goal_max_continuations
        ),
        format!(
            "enforce_token_budget = {}  · true = a goal's token budget is a hard stop; false = advisory telemetry",
            on(app.goal_enforce_token_budget)
        ),
    ];
    CommandResult::message(lines.join("\n"))
}
