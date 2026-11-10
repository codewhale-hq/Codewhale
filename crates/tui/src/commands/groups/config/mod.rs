//! Portable config command group metadata and inventory (FEAT-028).
//!
//! Mirrors `groups/session/mod.rs`: this module is part of the movable source
//! closure and imports only `codewhale-command-contract` — no `App`, no
//! `crate::tui`, no TUI `Command`/`CommandGroup` traits. Host registration,
//! concrete logic and action conversion live in
//! `crate::commands::config_group` and `crate::commands::config_main_host`.

// This group dir intentionally has a `config.rs` child module with the same
// name. The module_inception allow is a permanent structure rationale, not
// migration scaffolding; see docs/architecture/command-dispatch.md.
#[allow(clippy::module_inception)]
pub mod config;
pub mod import_claude;
pub mod policy;

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::CommandInfo;
use codewhale_command_contract::outcome::ConfigCommandResult;

static CONFIG_INFO: CommandInfo = CommandInfo {
    name: "config",
    aliases: &[],
    usage: "/config [ask-rules|status|<key> [value]]",
    description_key: "cmd_config_description",
};
static IMPORT_CLAUDE_INFO: CommandInfo = CommandInfo {
    name: "import-claude",
    aliases: &["import_claude"],
    usage: "/import-claude [--apply]",
    description_key: "cmd_import_claude_description",
};
static LOGIN_INFO: CommandInfo = CommandInfo {
    name: "login",
    aliases: &[],
    usage: "/login [status|account|key]",
    description_key: "cmd_login_description",
};
static CONNECT_INFO: CommandInfo = CommandInfo {
    name: "connect",
    aliases: &[],
    usage: "/connect [account|<provider>]",
    description_key: "cmd_connect_description",
};
static DISCONNECT_INFO: CommandInfo = CommandInfo {
    name: "disconnect",
    aliases: &[],
    usage: "/disconnect <provider>",
    description_key: "cmd_disconnect_description",
};
static AUTH_INFO: CommandInfo = CommandInfo {
    name: "auth",
    aliases: &[],
    usage: "/auth xai-device|chatgpt|chatgpt-revoke|orcarouter|orcarouter-revoke",
    description_key: "cmd_auth_description",
};
static RAIL_INFO: CommandInfo = CommandInfo {
    name: "workbar",
    // /rail and /sidebar are the names users already know; both now drive
    // the one workbar.
    aliases: &["rail", "sidebar"],
    usage: "/workbar [bottom|top|left|right|off|tasks|agents|context|pinned] [--save]",
    description_key: "cmd_sidebar_description",
};
static PET_INFO: CommandInfo = CommandInfo {
    name: "pet",
    aliases: &[],
    usage: config::PET_USAGE,
    description_key: "cmd_pet_description",
};
static SETTINGS_INFO: CommandInfo = CommandInfo {
    name: "settings",
    aliases: &[],
    usage: "/settings [text]",
    description_key: "cmd_settings_description",
};
static STATUSLINE_INFO: CommandInfo = CommandInfo {
    name: "statusline",
    aliases: &[],
    usage: "/statusline",
    description_key: "cmd_statusline_description",
};
static MODE_INFO: CommandInfo = CommandInfo {
    name: "mode",
    aliases: &["jihua", "zidong"],
    usage: config::MODE_USAGE,
    description_key: "cmd_mode_description",
};
static FULLSCREEN_INFO: CommandInfo = CommandInfo {
    name: "fullscreen",
    aliases: &[],
    usage: "/fullscreen",
    description_key: "cmd_fullscreen_description",
};
static INLINE_INFO: CommandInfo = CommandInfo {
    name: "inline",
    aliases: &[],
    usage: "/inline",
    description_key: "cmd_inline_description",
};
static THEME_INFO: CommandInfo = CommandInfo {
    name: "theme",
    aliases: &[],
    usage: "/theme [name|underwater|custom:<name>|schema|path]",
    description_key: "cmd_theme_description",
};
static VERBOSE_INFO: CommandInfo = CommandInfo {
    name: "verbose",
    aliases: &[],
    usage: "/verbose [on|off]",
    description_key: "cmd_verbose_description",
};
static TRUST_INFO: CommandInfo = CommandInfo {
    name: "trust",
    aliases: &["xinren"],
    usage: "/trust [on|off [--save]|add <path>|remove <path>|list]",
    description_key: "cmd_trust_description",
};
static LOGOUT_INFO: CommandInfo = CommandInfo {
    name: "logout",
    aliases: &[],
    usage: "/logout",
    description_key: "cmd_logout_description",
};

fn contextual(
    capabilities: CommandCapabilities,
    handler: fn(CommandContexts<'_>, Option<&str>) -> ConfigCommandResult,
) -> CommandHandler<ConfigCommandResult> {
    CommandHandler::Contextual {
        capabilities,
        handler,
    }
}

/// Complete group inventory (19 commands), shared by the host registration and
/// the extraction proof. Order is the registration order.
pub fn portable_handlers() -> [(&'static CommandInfo, CommandHandler<ConfigCommandResult>); 19] {
    // The policy slice (permissions, status) already exposes a portable
    // inventory; re-publish its metadata through this group's inventory.
    let [permissions, status] = policy::portable_handlers();
    [
        (
            &CONFIG_INFO,
            contextual(config::CONFIG_CAPS, config::config),
        ),
        (
            &IMPORT_CLAUDE_INFO,
            contextual(config::CONFIG_ONLY_CAPS, config::import_claude),
        ),
        permissions,
        (&LOGIN_INFO, contextual(config::LOGIN_CAPS, config::login)),
        (
            &CONNECT_INFO,
            contextual(config::LOGIN_CAPS, config::connect),
        ),
        (
            &DISCONNECT_INFO,
            contextual(config::LOGIN_CAPS, config::disconnect),
        ),
        (&AUTH_INFO, CommandHandler::Pure(config::auth)),
        (
            &RAIL_INFO,
            contextual(config::WORK_SURFACE_CAPS, config::workbar),
        ),
        (&PET_INFO, contextual(config::PET_CAPS, config::pet)),
        (
            &SETTINGS_INFO,
            contextual(config::CONFIG_PRESENTATION_CAPS, config::settings),
        ),
        status,
        (&STATUSLINE_INFO, CommandHandler::Pure(config::statusline)),
        (&MODE_INFO, contextual(config::MODE_CAPS, config::mode)),
        (
            &FULLSCREEN_INFO,
            contextual(config::CONFIG_ONLY_CAPS, config::fullscreen),
        ),
        (
            &INLINE_INFO,
            contextual(config::CONFIG_ONLY_CAPS, config::inline),
        ),
        (
            &THEME_INFO,
            contextual(config::CONFIG_PRESENTATION_CAPS, config::theme),
        ),
        (
            &VERBOSE_INFO,
            contextual(config::CONFIG_ONLY_CAPS, config::verbose),
        ),
        (&TRUST_INFO, contextual(config::TRUST_CAPS, config::trust)),
        (&LOGOUT_INFO, contextual(config::LOGIN_CAPS, config::logout)),
    ]
}
