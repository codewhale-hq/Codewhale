//! Portable config command handlers (FEAT-028 Phase 5).
//!
//! Every handler consumes only the `CONFIG`/`PET`/`TRUST` capability facets and
//! returns a contract-owned `CommandResult<ConfigAction>`. No handler names
//! `App`, `AppAction` or any concrete host type, so the whole group compiles
//! without `codewhale-tui` and can move into `codewhale-commands` without a
//! circular dependency. The concrete implementations live in
//! `crate::commands::config_main_host`, reached only through the host adapters.

use codewhale_command_contract::config::{
    CommandAccountState, CommandPetBodyStyle, CommandPetControl, CommandPetLabel,
    CommandPetTextLanguage, CommandWorkSurfaceContext, CommandWorkflowSummary, ConfigMessage,
    ConfigReply, TrustPathError,
};
use codewhale_command_contract::facets::CommandModeSelection;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts};
use codewhale_command_contract::outcome::{ConfigAction, ConfigCommandResult};
use codewhale_command_contract::types::{
    CommandMode, CommandRailPanel, CommandScreenMode, CommandWorkSurfacePlacement,
};

/// `/config` only: config authority plus `PRESENTATION` for its keyed messages
/// and `PERMISSIONS` because `ask-rules` forwards to the permission-rules editor.
pub const CONFIG_CAPS: CommandCapabilities = CommandCapabilities::CONFIG
    .union(CommandCapabilities::PRESENTATION)
    .union(CommandCapabilities::PERMISSIONS);
/// Commands that observe config and render keyed messages (`/theme`).
pub const CONFIG_PRESENTATION_CAPS: CommandCapabilities =
    CommandCapabilities::CONFIG.union(CommandCapabilities::PRESENTATION);
/// Commands that observe config and render only literals
/// (`/settings`, `/verbose`, `/fullscreen`, `/inline`, `/import-claude`).
pub const CONFIG_ONLY_CAPS: CommandCapabilities = CommandCapabilities::CONFIG;
/// Capabilities declared by `/pet`.
pub const PET_CAPS: CommandCapabilities = CommandCapabilities::PET;
/// Capabilities declared by `/trust`.
pub const TRUST_CAPS: CommandCapabilities = CommandCapabilities::TRUST;
/// Capabilities declared by `/login` and `/logout`.
pub const LOGIN_CAPS: CommandCapabilities = CommandCapabilities::LOGIN;
/// Capabilities declared by `/mode` (reuses the mode-policy facet).
pub const MODE_CAPS: CommandCapabilities = CommandCapabilities::MODE_POLICY;
/// Capabilities declared by `/workbar`.
pub const WORK_SURFACE_CAPS: CommandCapabilities = CommandCapabilities::WORK_SURFACE;

/// Resolve a host reply into a command result.
///
/// Literal messages never require the presentation shape. A `Keyed` message
/// needs it, and a missing required capability fails loudly (it must not be
/// substituted while the reply keeps `is_error == false`).
fn to_result(
    reply: ConfigReply,
    parts: &mut codewhale_command_contract::handler::ContextParts<'_>,
    prepared: &PreparedTemplates,
) -> ConfigCommandResult {
    let message = match reply.message {
        None => None,
        Some(message) => {
            if message_needs_presentation(&message) {
                match parts.presentation.as_deref_mut() {
                    Some(_) => match render_message(message, prepared) {
                        Ok(text) => Some(text),
                        Err(error) => {
                            return ConfigCommandResult {
                                message: Some(error),
                                action: None,
                                is_error: true,
                            };
                        }
                    },
                    None => {
                        return ConfigCommandResult {
                            message: Some(
                                "Error: Command capability unavailable: presentation".to_string(),
                            ),
                            action: None,
                            is_error: true,
                        };
                    }
                }
            } else {
                render_literals(message)
            }
        }
    };
    let message = match (message, reply.is_error) {
        (Some(text), true) if !text.starts_with("Error: ") => Some(format!("Error: {text}")),
        (other, _) => other,
    };
    ConfigCommandResult {
        message,
        action: reply.action,
        is_error: reply.is_error,
    }
}

/// Whether any part of the message needs the presentation shape.
fn message_needs_presentation(message: &ConfigMessage) -> bool {
    match message {
        ConfigMessage::Literal(_) => false,
        ConfigMessage::Keyed { .. } => true,
        ConfigMessage::Concat(parts) => parts.iter().any(message_needs_presentation),
    }
}

/// Render a literal-only message without the presentation shape.
fn render_literals(message: ConfigMessage) -> Option<String> {
    match message {
        ConfigMessage::Literal(text) => Some(text),
        ConfigMessage::Concat(parts) => {
            let mut out = String::new();
            for part in parts {
                out.push_str(&render_literals(part)?);
            }
            Some(out)
        }
        ConfigMessage::Keyed { .. } => None,
    }
}

/// The parameter names left as sentinels in a prepared template.
fn sentinel_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('\u{1}') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('\u{1}') else {
            break;
        };
        names.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    names
}

/// Sentinel left in place of a template placeholder during preflight.
fn sentinel(name: &str) -> String {
    format!("\u{1}{name}\u{1}")
}

/// Localized templates validated before any effect, keyed by message key.
///
/// Rendering never translates again. Replies outside the prepared key/parameter
/// contract fail without an action; inserted values remain literal.
#[derive(Default)]
struct PreparedTemplates {
    templates: std::collections::HashMap<String, String>,
}

impl PreparedTemplates {
    fn get(&self, key: &str) -> Option<&str> {
        self.templates.get(key).map(String::as_str)
    }
}

/// Render a reply message, propagating any translation failure (including one
/// nested inside a parameter) instead of converting it into ordinary text.
fn render_message(message: ConfigMessage, prepared: &PreparedTemplates) -> Result<String, String> {
    match message {
        ConfigMessage::Literal(text) => Ok(text),
        ConfigMessage::Concat(parts) => {
            let mut out = String::new();
            for part in parts {
                out.push_str(&render_message(part, prepared)?);
            }
            Ok(out)
        }
        ConfigMessage::Keyed { key, params } => {
            let mut resolved: Vec<(String, String)> = Vec::with_capacity(params.len());
            for (name, param) in params {
                resolved.push((name, render_message(param, prepared)?));
            }
            match prepared.get(&key) {
                // Validated before any effect: substitute the sentinel left by
                // preflight, never re-translate. The reply's parameter names
                // must match the prepared template exactly.
                Some(template) => {
                    // The template may repeat a placeholder; only the *set* of
                    // names has to match, and the reply must not duplicate one.
                    let expected: std::collections::BTreeSet<String> =
                        sentinel_names(template).into_iter().collect();
                    let mut seen = std::collections::BTreeSet::new();
                    for (name, _) in &resolved {
                        if !seen.insert(name.clone()) {
                            return Err(format!(
                                "invalid translation replacement contract for {key}: duplicate {name}"
                            ));
                        }
                    }
                    if seen != expected {
                        return Err(format!(
                            "invalid translation replacement contract for {key}"
                        ));
                    }
                    let mut out = String::with_capacity(template.len());
                    let mut rest = template;
                    while let Some(start) = rest.find('\u{1}') {
                        let after = &rest[start + 1..];
                        let Some(end) = after.find('\u{1}') else {
                            break;
                        };
                        out.push_str(&rest[..start]);
                        let name = &after[..end];
                        if let Some((_, value)) = resolved.iter().find(|(key, _)| key == name) {
                            out.push_str(value);
                        }
                        rest = &after[end + 1..];
                    }
                    out.push_str(rest);
                    Ok(out)
                }
                // The effect-reply contract is prepared before the mutation; an
                // unprepared key is a host contract violation, never a reason to
                // translate after the effect.
                None => Err(format!("unprepared message key: {key}")),
            }
        }
    }
}

/// The complete effect-reply contract, validated before every mutation.
const REQUIRED_TEMPLATES: &[(&str, &[&str])] =
    codewhale_command_contract::config::EFFECT_REPLY_TEMPLATES;

/// Templates `/settings` renders through the presentation shape.
const SETTINGS_TEMPLATES: &[(&str, &[&str])] =
    &[("settings_title", &[]), ("settings_config_file", &[])];

/// Keyed explanatory notes the `/config status` audit renders.
const AUDIT_TEMPLATES: &[(&str, &[&str])] = &[
    ("config_audit_search_provider", &[]),
    ("config_audit_prompt_suggestion", &[]),
    ("config_audit_notifications", &[]),
];

/// Validate required capabilities and translation templates before any effect.
#[allow(clippy::result_large_err)]
fn preflight(
    parts: &mut codewhale_command_contract::handler::ContextParts<'_>,
) -> Result<PreparedTemplates, ConfigCommandResult> {
    preflight_with(parts, REQUIRED_TEMPLATES)
}

/// As [`preflight`], with a caller-supplied template set. Returns the
/// validated templates so rendering cannot translate again after an effect.
#[allow(clippy::result_large_err)]
fn preflight_with(
    parts: &mut codewhale_command_contract::handler::ContextParts<'_>,
    templates: &[(&str, &[&str])],
) -> Result<PreparedTemplates, ConfigCommandResult> {
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return Err(missing("presentation"));
    };
    let mut prepared = PreparedTemplates::default();
    for (key, params) in templates {
        let dummies: Vec<String> = params.iter().map(|name| sentinel(name)).collect();
        let refs: Vec<(&str, &str)> = params
            .iter()
            .zip(&dummies)
            .map(|(name, dummy)| (*name, dummy.as_str()))
            .collect();
        let template = presentation
            .translate(key, &refs)
            .map_err(|error| ConfigCommandResult {
                message: Some(error),
                action: None,
                is_error: true,
            })?;
        prepared.templates.insert((*key).to_string(), template);
    }
    Ok(prepared)
}

fn missing(facet: &str) -> ConfigCommandResult {
    ConfigCommandResult {
        message: Some(format!("Command capability unavailable: {facet}")),
        action: None,
        is_error: true,
    }
}

/// `/config [ask-rules|status|<key> [value]|preset|workflow|...]`.
/// `/config [ask-rules|status|<key> [value]|preset|workflow|subagents|search|notifications|tui|web|native]`.
///
/// The command owns the top-level subcommand routing, the `--save` parse and the
/// ask-rules forwarding; the facet performs the per-key work behind typed
/// operations.
pub fn config(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let raw = arg.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return ConfigCommandResult::action(ConfigAction::OpenConfigView);
    }
    if matches!(
        raw.to_ascii_lowercase().as_str(),
        "audit" | "editability" | "editable" | "status"
    ) {
        let mut parts = contexts.into_parts();
        if parts.config.is_none() {
            return missing("config");
        }
        let prepared = match preflight_with(&mut parts, AUDIT_TEMPLATES) {
            Ok(prepared) => prepared,
            Err(result) => return result,
        };
        let report = match parts.config.as_deref_mut() {
            Some(facet) => match facet.audit() {
                Ok(report) => report,
                Err(error) => return ConfigCommandResult::error(error),
            },
            None => return missing("config"),
        };
        let mut message = vec![ConfigMessage::literal(format!(
            "Config editability audit\nConfig path: {}\nKey | Current | Editability | Command / reason",
            report.config_path
        ))];
        for row in &report.rows {
            message.push(ConfigMessage::literal(format!(
                "\n{} | {} | {} | {}\n  ",
                row.key, row.current, row.editability, row.command
            )));
            message.push(row.note.clone());
        }
        return to_result(
            ConfigReply::message(ConfigMessage::concat(message)),
            &mut parts,
            &prepared,
        );
    }
    let mut raw_words = raw.splitn(2, char::is_whitespace);
    let first_word = raw_words.next();
    if first_word.is_some_and(is_ask_rules_config_token) {
        let rest = raw_words.next().unwrap_or("").trim();
        return super::policy::permissions::execute(contexts, Some(rest));
    }
    let is_workflow = first_word.is_some_and(|token| {
        token.eq_ignore_ascii_case("workflow") || token.eq_ignore_ascii_case("goal")
    }) && raw_words
        .clone()
        .next()
        .is_none_or(|rest| rest.trim().is_empty());
    let is_subagents = first_word.is_some_and(|token| token.eq_ignore_ascii_case("subagents"));
    let is_search = first_word.is_some_and(|token| token.eq_ignore_ascii_case("search"));
    let is_notifications = first_word.is_some_and(|token| {
        token.eq_ignore_ascii_case("notifications") || token.eq_ignore_ascii_case("notification")
    });
    let is_preset = first_word.is_some_and(|token| token.eq_ignore_ascii_case("preset"));

    let mut parts = contexts.into_parts();
    if parts.config.is_none() {
        return missing("config");
    }
    let prepared = match preflight(&mut parts) {
        Ok(prepared) => prepared,
        Err(result) => return result,
    };
    let Some(facet) = parts.config.as_deref_mut() else {
        return missing("config");
    };
    if is_workflow {
        return ConfigCommandResult::message(render_workflow_summary(facet.workflow_summary()));
    }
    if is_subagents {
        let rest = raw_words.next().unwrap_or("").trim();
        let (op, persist) = parse_subagent_rest(rest);
        let reply = match op {
            SubagentOp::Status => {
                return match facet.subagents_status() {
                    Ok(report) => {
                        let mut lines = vec![
                            format!(
                                "Sub-agents: {}",
                                report.disabled_reason.as_deref().map_or_else(
                                    || "enabled".to_string(),
                                    |reason| format!("disabled ({reason})"),
                                )
                            ),
                            format!("Config path: {}", report.config_path),
                            format!(
                                "Active provider: {} ({})",
                                report.provider_key, report.provider_label
                            ),
                            format!(
                                "subagents.enabled = {}",
                                report.explicit_enabled.map_or_else(
                                    || "default true".to_string(),
                                    |value| value.to_string()
                                )
                            ),
                        ];
                        for limit in &report.limits {
                            lines.push(format!(
                                "subagents.{} = {} (resolved global {}; active provider {})",
                                limit.key, limit.raw, limit.global, limit.provider
                            ));
                        }
                        match &report.provider_rows {
                            Some(rows) => {
                                for row in rows {
                                    lines.push(format!(
                                        "subagents.providers.{}.{} = {}",
                                        report.provider_key, row.key, row.value
                                    ));
                                }
                            }
                            None => lines.push(format!(
                                "subagents.providers.{} = inherits global",
                                report.provider_key
                            )),
                        }
                        ConfigCommandResult::message(lines.join("\n"))
                    }
                    Err(error) => ConfigCommandResult::error(error),
                };
            }
            SubagentOp::Get(key) => facet.subagent(key),
            SubagentOp::Set { key, value } => facet.set_subagent(key, value, persist),
            SubagentOp::Usage => {
                return ConfigCommandResult::error(
                    "Usage: /config subagents [status|on|off|enabled|max_concurrent|max_depth|launch_concurrency|api_timeout_secs|heartbeat_timeout_secs <value>] [--save]",
                );
            }
        };
        return to_result(reply, &mut parts, &prepared);
    }
    if is_search {
        let rest = raw_words.next().unwrap_or("").trim();
        let (op, persist) = parse_search_rest(rest);
        let reply = match op {
            SearchOp::Status => facet.search_provider_status(),
            SearchOp::Set(value) => facet.set_search_provider(value, persist),
            SearchOp::Usage => {
                return to_result(
                    ConfigReply::error(ConfigMessage::concat(vec![
                        ConfigMessage::keyed("help_usage_label"),
                        ConfigMessage::literal(format!(
                            " /config search.provider <{}> [--save]",
                            facet.search_provider_names(),
                        )),
                    ])),
                    &mut parts,
                    &prepared,
                );
            }
        };
        return to_result(reply, &mut parts, &prepared);
    }
    if is_notifications {
        let rest = raw_words.next().unwrap_or("").trim();
        let (op, persist) = parse_notifications_rest(rest);
        let reply = match op {
            NotificationsOp::Status => facet.notifications_status(),
            NotificationsOp::Get(key) => facet.notification(key),
            NotificationsOp::Set { key, value } => facet.set_notification(key, value, persist),
        };
        return to_result(reply, &mut parts, &prepared);
    }

    if is_preset {
        let rest = raw_words.next().unwrap_or("").trim();
        let tokens = rest.split_whitespace().collect::<Vec<_>>();
        let persist = matches!(tokens.last(), Some(&"--save") | Some(&"-s"));
        let name = tokens.first().copied().unwrap_or("");
        return to_result(facet.apply_preset(name, persist), &mut parts, &prepared);
    }
    let parts2 = raw.splitn(2, ' ').collect::<Vec<&str>>();
    if parts2.len() == 1 {
        let token = parts2[0];
        if matches!(
            token.to_ascii_lowercase().as_str(),
            "tui" | "web" | "native"
        ) {
            return ConfigCommandResult::action(ConfigAction::OpenConfigView);
        }
        to_result(facet.get_value(token), &mut parts, &prepared)
    } else {
        let raw_value = parts2[1];
        let persist = raw_value.ends_with(" --save") || raw_value.ends_with(" -s");
        let value = if persist {
            raw_value
                .strip_suffix(" --save")
                .or_else(|| raw_value.strip_suffix(" -s"))
                .unwrap_or(raw_value)
        } else {
            raw_value
        };
        to_result(
            facet.set_value(parts2[0], value, persist),
            &mut parts,
            &prepared,
        )
    }
}

/// Whether a `/config` token forwards to the permission-rules editor.
fn is_ask_rules_config_token(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "ask-rules"
            | "ask_rules"
            | "askrules"
            | "rules"
            | "permission-rules"
            | "permission_rules"
            | "permissions"
    )
}

/// `/settings [text]`.
/// `/settings [text]`. The command owns the argument routing; the facet returns
/// the resolved settings text.
pub fn settings(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    match arg.map(str::trim).filter(|value| !value.is_empty()) {
        None => ConfigCommandResult::action(ConfigAction::OpenConfigView),
        Some("text" | "show" | "diagnostic" | "diagnostics") => {
            let mut parts = contexts.into_parts();
            if parts.config.is_none() {
                return missing("config");
            }
            let prepared = match preflight_with(&mut parts, SETTINGS_TEMPLATES) {
                Ok(prepared) => prepared,
                Err(result) => return result,
            };
            let Some(facet) = parts.config.as_deref_mut() else {
                return missing("config");
            };
            let reply = match facet.show_settings() {
                Ok(report) => {
                    let mut message = vec![
                        ConfigMessage::keyed("settings_title"),
                        ConfigMessage::literal("\n─────────────────────────────\n"),
                    ];
                    let mut rows = String::new();
                    for row in &report.rows {
                        rows.push_str(&format!(
                            "  {:<width$}{}",
                            row.label,
                            row.value,
                            width = row.label_width
                        ));
                        rows.push('\n');
                    }
                    message.push(ConfigMessage::literal(rows));
                    message.push(ConfigMessage::literal("\n"));
                    message.push(ConfigMessage::keyed("settings_config_file"));
                    let mut tail = format!(" {}", report.config_file);
                    if !report.from_settings.is_empty() || !report.session_overrides.is_empty() {
                        tail.push_str("\n\n");
                        if !report.from_settings.is_empty() {
                            tail.push_str(&format!(
                                "  from settings.toml: {}",
                                report.from_settings.join(", ")
                            ));
                        }
                        if !report.session_overrides.is_empty() {
                            if !report.from_settings.is_empty() {
                                tail.push('\n');
                            }
                            tail.push_str(&format!(
                                "  session override: {}",
                                report.session_overrides.join(", ")
                            ));
                        }
                    }
                    message.push(ConfigMessage::literal(tail));
                    ConfigReply::message(ConfigMessage::concat(message))
                }
                Err(error) => ConfigReply::error(error),
            };
            to_result(reply, &mut parts, &prepared)
        }
        Some(_) => ConfigCommandResult::error("Usage: /settings [text]"),
    }
}

/// `/mode [act|plan|operate|1|2|3]` and the `/jihua`/`/zidong` aliases.
/// `/mode [act|plan|operate|1|2|3]` (aliases `/jihua`, `/zidong`).
///
/// The command owns the alias handling, parse and message composition; the
/// mode-policy facet applies the selection and reports the outcome.
pub fn mode(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let Some(arg) = arg.filter(|value| !value.trim().is_empty()) else {
        return ConfigCommandResult::action(ConfigAction::OpenModePicker);
    };
    let mut parts = contexts.into_parts();
    let Some(facet) = parts.mode_policy.as_deref_mut() else {
        return missing("mode_policy");
    };
    if CommandMode::is_legacy_bypass_alias(arg) {
        // The legacy alias is a one-way permission shorthand: its receipt names
        // Full Access (Work), not the hidden mode it folds to.
        return match facet.select_yolo_compat() {
            CommandModeSelection::Changed => ConfigCommandResult::with_message_and_action(
                format!("Switched to {}.", facet.yolo_compat_label()),
                ConfigAction::ModeChanged(CommandMode::Agent),
            ),
            CommandModeSelection::PersistedSame => {
                ConfigCommandResult::message(facet.mode_startup_default_receipt(CommandMode::Agent))
            }
            CommandModeSelection::Refused => {
                ConfigCommandResult::message(facet.setting_locked_mode_message())
            }
        };
    }
    match CommandMode::parse(arg) {
        Some(mode) => {
            let selection = facet.select_mode(mode);
            let label = facet.mode_label(mode);
            match selection {
                CommandModeSelection::Changed => ConfigCommandResult::with_message_and_action(
                    format!("Switched to {label} mode."),
                    ConfigAction::ModeChanged(mode),
                ),
                CommandModeSelection::PersistedSame => {
                    ConfigCommandResult::message(facet.mode_startup_default_receipt(mode))
                }
                CommandModeSelection::Refused => {
                    ConfigCommandResult::message(facet.setting_locked_mode_message())
                }
            }
        }
        None => ConfigCommandResult::error(format!("Usage: {MODE_USAGE}")),
    }
}

/// `/mode` usage, shared with the registration metadata.
pub(super) const MODE_USAGE: &str = "/mode [work|plan|operate|1|2|3]";

/// `/pet` usage, shared with the registration metadata.
pub const PET_USAGE: &str = "/pet [on|off|inspect|status|appearance|style [smooth|pixel]|language [auto|en|zh-Hans|zh-Hant|ja]|window|source|export|sound [on|off]|avatar [whale|cove|braille|pixels|particles|whale-girl|key]|action [name|live]|view [name|live]]";

/// `/connect [<provider>]` — sign in to an enabled plugin provider.
pub fn connect(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let raw = arg.map(str::trim).unwrap_or("");
    if raw.split_whitespace().count() > 1 {
        return ConfigCommandResult::error("Usage: /connect [<provider>]");
    }
    let mut parts = contexts.into_parts();
    let Some(facet) = parts.login.as_deref_mut() else {
        return missing("login");
    };
    let provider = (!raw.is_empty()).then_some(raw);
    let reply = facet.connect(provider);
    to_result(reply, &mut parts, &PreparedTemplates::default())
}

/// `/disconnect <provider>` — sign out of an enabled plugin provider.
pub fn disconnect(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let raw = arg.map(str::trim).unwrap_or("");
    if raw.is_empty() || raw.split_whitespace().count() > 1 {
        return ConfigCommandResult::error("Usage: /disconnect <provider>");
    }
    let mut parts = contexts.into_parts();
    let Some(facet) = parts.login.as_deref_mut() else {
        return missing("login");
    };
    let reply = facet.disconnect(raw);
    to_result(reply, &mut parts, &PreparedTemplates::default())
}

/// `/theme [name|underwater|custom:<name>|schema|path]`.
/// `/theme [name|underwater|custom:<name>|schema|path]`.
pub fn theme(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let mut parts = contexts.into_parts();
    if parts.config.is_none() {
        return missing("config");
    }
    match arg.map(str::trim).filter(|s| !s.is_empty()) {
        // `schema`/`path`/no-arg render no config message, so they do not
        // depend on the config translation templates.
        None => ConfigCommandResult::action(ConfigAction::OpenThemePicker),
        Some("schema") => match parts.config.as_deref_mut() {
            Some(facet) => ConfigCommandResult::message(facet.theme_schema()),
            None => missing("config"),
        },
        Some("path") => {
            let Some(facet) = parts.config.as_deref_mut() else {
                return missing("config");
            };
            match facet.themes_dir() {
                Ok(path) => ConfigCommandResult::message(format!(
                    "User themes: {path}\nSelect with: /theme custom:<name>"
                )),
                Err(error) => ConfigCommandResult::error(error),
            }
        }
        Some(name) => {
            let prepared = match preflight(&mut parts) {
                Ok(prepared) => prepared,
                Err(result) => return result,
            };
            let Some(facet) = parts.config.as_deref_mut() else {
                return missing("config");
            };
            to_result(facet.set_value("theme", name, true), &mut parts, &prepared)
        }
    }
}

/// `/verbose [on|off]`.
///
/// The command keeps its own parsing and message composition; the facet only
/// reads and writes the flag (FEAT-028: keep logic in the command, replace only
/// the surgical host touch).
pub fn verbose(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let mut parts = contexts.into_parts();
    if parts.config.is_none() {
        return missing("config");
    }
    let Some(facet) = parts.config.as_deref_mut() else {
        return missing("config");
    };
    let next = match arg.map(str::trim).filter(|s| !s.is_empty()) {
        None => !facet.verbose_enabled(),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "on" | "true" | "1" | "yes" => true,
            "off" | "false" | "0" | "no" => false,
            "toggle" => !facet.verbose_enabled(),
            _ => {
                return ConfigCommandResult {
                    message: Some(
                        "Usage: /verbose [on|off]. Compact thinking remains available when verbose is off."
                            .to_string(),
                    ),
                    action: None,
                    is_error: true,
                };
            }
        },
    };
    facet.set_verbose_enabled(next);
    ConfigCommandResult::message(if next {
        "Verbose transcript on: live thinking renders in full."
    } else {
        "Verbose transcript off: live thinking stays compact."
    })
}

/// `/fullscreen`.
pub fn fullscreen(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    screen(contexts, false, arg)
}

/// `/inline`.
pub fn inline(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    screen(contexts, true, arg)
}

/// The command keeps its argument check and the "already on" message; the facet
/// only reports the current screen mode.
fn screen(contexts: CommandContexts<'_>, inline: bool, arg: Option<&str>) -> ConfigCommandResult {
    let target = if inline {
        CommandScreenMode::Inline
    } else {
        CommandScreenMode::Fullscreen
    };
    let name = if inline { "inline" } else { "fullscreen" };
    if let Some(extra) = arg.map(str::trim).filter(|value| !value.is_empty()) {
        return ConfigCommandResult {
            message: Some(format!(
                "/{name} takes no argument (got {extra:?}). Use /fullscreen or /inline."
            )),
            action: None,
            is_error: true,
        };
    }
    let mut parts = contexts.into_parts();
    if parts.config.is_none() {
        return missing("config");
    }
    let Some(facet) = parts.config.as_deref_mut() else {
        return missing("config");
    };
    if target == facet.screen_mode() {
        return ConfigCommandResult::message(match target {
            CommandScreenMode::Fullscreen => {
                "Already on the fullscreen screen (alternate screen). /inline keeps the terminal's own scrollback instead."
            }
            CommandScreenMode::Inline => {
                "Already inline: a full-height viewport with no alternate screen, so this terminal's scrollback survives the session. /fullscreen returns to the alternate screen."
            }
        });
    }
    ConfigCommandResult::action(ConfigAction::SetScreenMode(target))
}

/// `/statusline` — pure: it only opens the statusline picker, so it declares no
/// capability and needs no host facet.
pub fn statusline(_arg: Option<&str>) -> ConfigCommandResult {
    ConfigCommandResult {
        message: None,
        action: Some(ConfigAction::OpenStatusPicker),
        is_error: false,
    }
}

/// `/workbar` (aliases `/rail`, `/sidebar`).
/// `/workbar [bottom|top|left|right|off|panel] [--save]` (aliases `/rail`,
/// `/sidebar`). The command owns token parsing and the status line; the facet
/// observes and applies the work-surface state.
pub fn workbar(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    const USAGE: &str =
        "Usage: /workbar [bottom|top|left|right|off|tasks|agents|terminal|context|pinned] [--save]";
    let raw = arg.map(str::trim).unwrap_or("");
    let mut tokens = raw.split_whitespace().collect::<Vec<_>>();
    let persist = matches!(tokens.last(), Some(&"--save" | &"-s"));
    if persist {
        tokens.pop();
    }
    let mut parts = contexts.into_parts();
    // `/workbar` status and its setter errors are literal, so it needs no
    // presentation and must not depend on the config translation templates.
    let prepared = PreparedTemplates::default();
    let Some(facet) = parts.work_surface.as_deref_mut() else {
        return missing("work_surface");
    };
    match tokens.as_slice() {
        [] => return ConfigCommandResult::message(rail_status(facet)),
        [value] => {
            let value = value.to_ascii_lowercase();
            let placement = match value.as_str() {
                "top" => Some(CommandWorkSurfacePlacement::Top),
                "bottom" | "on" | "show" | "visible" => Some(CommandWorkSurfacePlacement::Bottom),
                "left" => Some(CommandWorkSurfacePlacement::Left),
                "right" => Some(CommandWorkSurfacePlacement::Right),
                "off" | "hide" | "hidden" | "closed" | "none" => {
                    Some(CommandWorkSurfacePlacement::Off)
                }
                _ => None,
            };
            let panel = match value.as_str() {
                "tasks" | "activity" | "live" | "running" | "pinned" | "work" | "plan"
                | "todos" => Some(CommandRailPanel::Tasks),
                "agents" | "subagents" | "sub-agents" => Some(CommandRailPanel::Agents),
                "background" | "shells" | "jobs" => Some(CommandRailPanel::Background),
                "files" | "changes" => Some(CommandRailPanel::Files),
                "terminal" | "terminals" => Some(CommandRailPanel::Terminal),
                "notepad" | "notes" => Some(CommandRailPanel::Notepad),
                "context" | "session" => Some(CommandRailPanel::Context),
                "git" | "branch" => Some(CommandRailPanel::Git),
                "price" | "cost" => Some(CommandRailPanel::Price),
                _ => None,
            };
            match (placement, panel) {
                (Some(placement), None) => {
                    // Surface a setter error whether or not `--save` was given;
                    // discarding it would report a success that did not happen.
                    let reply = facet.set_placement(placement, persist);
                    if reply.is_error {
                        return to_result(reply, &mut parts, &prepared);
                    }
                }
                (None, Some(panel)) => {
                    let reply = facet.set_panel(panel, persist);
                    if reply.is_error {
                        return to_result(reply, &mut parts, &prepared);
                    }
                }
                _ => return ConfigCommandResult::error(USAGE),
            }
        }
        _ => return ConfigCommandResult::error(USAGE),
    }
    facet.mark_redraw();
    ConfigCommandResult::message(rail_status(facet))
}

fn rail_status(facet: &dyn CommandWorkSurfaceContext) -> String {
    let placement = facet.placement();
    if placement == CommandWorkSurfacePlacement::Off {
        return "Workbar is off — no panel renders (/workbar bottom|top|left|right to show it)"
            .to_string();
    }
    let panel = facet.panel();
    let mut message = format!(
        "Workbar: {} placement, {} panel",
        placement.as_setting(),
        facet.panel_title(panel)
    );
    let effective = facet.effective_placement();
    if effective != placement && effective == CommandWorkSurfacePlacement::Top {
        message.push_str(" — side placements need a wider terminal, showing top for now");
    }
    if !facet.area_present() {
        if panel == CommandRailPanel::Tasks {
            message.push_str(" (currently hidden — no work to show)");
        } else {
            message.push_str(" (renders next frame)");
        }
    }
    message
}

/// `/login [status|account|key]`.
/// `/login [status|account|key]`. The command composes the report; the login
/// facet returns the typed account state and provider identity.
pub fn login(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let raw = arg.map(str::trim).unwrap_or("");
    let token = raw.split_whitespace().next().unwrap_or("");
    let mut parts = contexts.into_parts();
    let Some(facet) = parts.login.as_deref_mut() else {
        return missing("login");
    };
    match token {
        "" | "status" => {
            let status = facet.login_status();
            let account = match status.account {
                CommandAccountState::SignedOut => {
                    format!("not signed in (api {})", status.api_base)
                }
                CommandAccountState::Authenticated => {
                    format!("signed in (api {})", status.api_base)
                }
                CommandAccountState::OfflineCached => {
                    format!("offline cached (api {})", status.api_base)
                }
                CommandAccountState::Expired => format!("expired (api {})", status.api_base),
                CommandAccountState::Revoked => format!("revoked (api {})", status.api_base),
                CommandAccountState::Unavailable(error) => format!("unavailable ({error})"),
            };
            ConfigCommandResult::message(format!(
                "Codewhale login\nAccount: {account}\nActive provider: {}\n\n\
                 Sign in: `codewhale login`\n\
                 Provider key: `codewhale auth set --provider <id>` or `/login key`\n\
                 Sign out: `/logout` or `codewhale logout`",
                status.provider
            ))
        }
        "key" | "provider" => ConfigCommandResult::with_message_and_action(
            "Open the provider picker to store an API key. Account sign-in is `codewhale login`.",
            ConfigAction::OpenProviderPicker,
        ),
        "account" => ConfigCommandResult::message(
            "TUI cannot start the browser device flow without freezing the session.\n\
             Run `codewhale login` (same as `codewhale account login`) in a terminal.\n\
             Then `/login` to confirm the session landed."
                .to_string(),
        ),
        other => ConfigCommandResult::error(format!(
            "Usage: /login [status|account|key]\nUnknown argument: {other}\n\
             To sign in to a plugin provider, use `/connect <provider>`."
        )),
    }
}

/// `/logout`. The command composes the confirmation; the login facet clears the
/// provider key, account session and internal token and reports each result.
pub fn logout(contexts: CommandContexts<'_>, _arg: Option<&str>) -> ConfigCommandResult {
    let mut parts = contexts.into_parts();
    let Some(facet) = parts.login.as_deref_mut() else {
        return missing("login");
    };
    match facet.logout() {
        Ok(outcome) => {
            let mut cleared = vec![format!("provider key ({})", outcome.provider)];
            match outcome.account {
                Ok(true) => cleared.push("Codewhale account session".to_string()),
                Ok(false) => {}
                Err(error) => cleared.push(format!("account session not cleared ({error})")),
            }
            match outcome.legacy_sandbox {
                Ok(true) => cleared.push("legacy sandbox token".to_string()),
                Ok(false) => {}
                Err(error) => cleared.push(format!("legacy sandbox token not cleared ({error})")),
            }
            ConfigCommandResult::with_message_and_action(
                format!(
                    "Cleared {}. Use `codewhale login` to sign in again, or `codewhale auth set --provider <id>` to store a provider key.",
                    cleared.join(", ")
                ),
                ConfigAction::OpenProviderPicker,
            )
        }
        Err(failure) => ConfigCommandResult::error(format!(
            "Failed to clear API key for {}: {}",
            failure.provider, failure.error
        )),
    }
}

/// `/import-claude [--apply]`.
/// `/import-claude [--apply]`. The command owns the flow and messages; the facet
/// builds the plan, writes the artifacts and carries over the standing
/// instructions.
pub fn import_claude(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let apply = arg.map(str::trim).is_some_and(|arg| {
        arg.eq_ignore_ascii_case("--apply") || arg.eq_ignore_ascii_case("apply")
    });
    let mut parts = contexts.into_parts();
    if parts.config.is_none() {
        return missing("config");
    }
    let Some(facet) = parts.config.as_deref_mut() else {
        return missing("config");
    };
    let Some(plan) = facet.build_plan() else {
        return ConfigCommandResult::message(
            "No Claude configuration found to import (looked for ~/.claude.json and ~/.claude/settings.json).",
        );
    };
    let report = super::import_claude::report_markdown(&plan, &|path| facet.display_path(path));
    let bundle = if plan.env_safe.is_empty() {
        None
    } else {
        Some(super::import_claude::portable_bundle_json(&plan))
    };
    let artifacts = facet.write_artifacts(&report, bundle.as_deref());
    if apply {
        let outcome = facet.apply_import(&plan, &artifacts.report_path, artifacts.wrote_report);
        let mut message = String::new();
        if outcome.done.is_empty() {
            message.push_str("Nothing was applied.\n");
        } else {
            message.push_str("Applied:\n");
            for item in &outcome.done {
                message.push_str(&format!("  · {}\n", render_follow_up(item)));
            }
        }
        if !outcome.manual.is_empty() {
            message.push_str("\nStill yours to do:\n");
            for item in &outcome.manual {
                message.push_str(&format!("  · {}\n", render_follow_up(item)));
            }
        }
        if !outcome.done.is_empty() {
            message.push_str(
                "\nRun /mcp reload to connect the imported servers after reviewing them.",
            );
        }
        if outcome.wrote_report {
            message.push_str(&format!(
                "\nFull report: {}",
                facet.display_path(&outcome.report_path)
            ));
        }
        return ConfigCommandResult::message(message);
    }
    let mut message = super::import_claude::render_plan(&plan, &|path| facet.display_path(path));
    message.push_str(
        "\n\nNothing above is applied yet. `/import-claude --apply` carries over the \
         standing instructions. Review MCP servers individually with `/mcp import`; hooks and \
         permission rules stay manual because they run code.",
    );
    if artifacts.wrote_report {
        message.push_str(&format!(
            "\nFull report: {}",
            facet.display_path(&artifacts.report_path)
        ));
    } else {
        message.push_str("\n(the report could not be written; the plan above is complete)");
    }
    if artifacts.wrote_bundle {
        message.push_str(&format!(
            "\nPortable bundle (review, then `codewhale config import {}`): apply it there for the consent/rollback path.",
            facet.display_path(&artifacts.bundle_path)
        ));
    }
    ConfigCommandResult::message(message)
}

/// `/pet [on|off|...]`.
/// `/pet [on|off|inspect|status|appearance|window|source|export|sound on|off|avatar [key]|action [name|live]|view [name|live]]`.
/// The command owns the verb parsing and message composition; the pet facet
/// observes and mutates the pet state and resolves the localized labels.
pub fn pet(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let words = arg
        .map(str::trim)
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    let mut parts = contexts.into_parts();
    let Some(facet) = parts.pet.as_deref_mut() else {
        return missing("pet");
    };
    let usage = || ConfigCommandResult::error(format!("Usage: {PET_USAGE}"));
    let queued = |facet: &mut dyn codewhale_command_contract::config::CommandPetContext,
                  control: CommandPetControl|
     -> ConfigCommandResult {
        if facet.queue(control) {
            ConfigCommandResult::message(facet.label(CommandPetLabel::HabitatQueued))
        } else {
            ConfigCommandResult::error(facet.label(CommandPetLabel::WatchUnavailable))
        }
    };
    match words.as_slice() {
        ["inspect"] => {
            facet.open_habitat();
            ConfigCommandResult::ok()
        }
        [] => {
            let enabled = !facet.enabled();
            facet.set_enabled(enabled, true);
            ConfigCommandResult::message(facet.label(if enabled {
                CommandPetLabel::ModeOn
            } else {
                CommandPetLabel::ModeOff
            }))
        }
        ["on"] => {
            facet.set_enabled(true, true);
            ConfigCommandResult::message(facet.label(CommandPetLabel::ModeOn))
        }
        ["off"] => {
            facet.set_enabled(false, true);
            ConfigCommandResult::message(facet.label(CommandPetLabel::ModeOff))
        }
        ["status"] => ConfigCommandResult::message(format!(
            "{} · {} · {}",
            facet.label(if facet.enabled() {
                CommandPetLabel::ModeOnLabel
            } else {
                CommandPetLabel::ModeOffLabel
            }),
            facet.label(if facet.is_open() {
                CommandPetLabel::ViewOpen
            } else {
                CommandPetLabel::ViewClosed
            }),
            facet.status()
        )),
        ["avatar"] => ConfigCommandResult::message(facet.avatar_choices()),
        ["avatar", key] => {
            if !facet.select_avatar(key) {
                return usage();
            }
            ConfigCommandResult::message(format!("/pet avatar {key}"))
        }
        [kind @ ("action" | "view"), name] => {
            if !facet.preview_avatar(name, *kind == "view") {
                return usage();
            }
            ConfigCommandResult::message(format!("/pet {kind} {name}"))
        }
        ["appearance"] => queued(facet, CommandPetControl::Browser),
        ["style", value] => {
            let Some(body) = CommandPetBodyStyle::parse(value) else {
                return usage();
            };
            queued(
                facet,
                CommandPetControl::Appearance {
                    body: Some(body),
                    language: None,
                },
            )
        }
        ["language", value] => {
            let Some(language) = CommandPetTextLanguage::parse(value) else {
                return usage();
            };
            queued(
                facet,
                CommandPetControl::Appearance {
                    body: None,
                    language: Some(language),
                },
            )
        }
        ["window"] => queued(facet, CommandPetControl::Window),
        ["source"] => queued(facet, CommandPetControl::Select),
        ["export"] => {
            if facet.export() {
                ConfigCommandResult::message(facet.label(CommandPetLabel::ExportQueued))
            } else {
                ConfigCommandResult::error(facet.label(CommandPetLabel::ExportUnavailable))
            }
        }
        ["sound", rest @ ..] => {
            let enabled = match rest {
                [] => None,
                ["on"] => Some(true),
                ["off"] => Some(false),
                _ => return usage(),
            };
            if let Some(enabled) = enabled {
                facet.set_sound(enabled);
            }
            let label = if enabled == Some(true) {
                facet.label(CommandPetLabel::SoundOn)
            } else {
                facet.sound_label()
            };
            ConfigCommandResult::message(format!("{label} · /pet sound on|off"))
        }
        _ => usage(),
    }
}

/// `/trust [on|off|add|remove|list]` (alias `/xinren`).
///
/// The command owns subcommand parsing and message composition; the facet only
/// reads and mutates the trust state and the allow-list.
pub fn trust(contexts: CommandContexts<'_>, arg: Option<&str>) -> ConfigCommandResult {
    let raw = arg.map(str::trim).unwrap_or("");
    let mut split = raw.splitn(2, char::is_whitespace);
    let sub = split.next().unwrap_or("").to_lowercase();
    let rest = split.next().map(str::trim).unwrap_or("");

    let mut parts = contexts.into_parts();
    let Some(facet) = parts.trust.as_deref_mut() else {
        return missing("trust");
    };

    match sub.as_str() {
        "" | "status" | "list" => {
            let force_paths = sub == "list";
            let mut lines = Vec::new();
            lines.push(format!(
                "Workspace trust mode: {}",
                if facet.trust_mode() {
                    "enabled"
                } else {
                    "disabled"
                }
            ));
            let paths = facet.trusted_paths();
            if paths.is_empty() {
                lines.push(if force_paths {
                    "No external paths trusted from this workspace.".to_string()
                } else {
                    "No external paths trusted yet. Use `/trust add <path>` to allow a directory."
                        .to_string()
                });
            } else {
                lines.push(format!("Trusted external paths ({}):", paths.len()));
                for path in paths {
                    lines.push(format!("  • {}", path.display()));
                }
            }
            ConfigCommandResult::message(lines.join("\n"))
        }
        "on" | "enable" | "yes" | "y" | "off" | "disable" | "no" | "n" => {
            if !matches!(rest, "" | "--save") {
                return ConfigCommandResult::error(format!(
                    "{} /trust on|off [--save]",
                    facet.usage_label()
                ));
            }
            ConfigCommandResult::action(ConfigAction::SetWorkspaceTrust {
                trusted: matches!(sub.as_str(), "on" | "enable" | "yes" | "y"),
                save: rest == "--save",
            })
        }
        "add" => {
            if rest.is_empty() {
                return ConfigCommandResult::error(
                    "Usage: /trust add <path>. Supply an absolute path or a path relative to the workspace.",
                );
            }
            match facet.add_trusted_path(rest) {
                Ok(stored) => ConfigCommandResult::message(format!(
                    "Added to trust list for this workspace: {}",
                    stored.display()
                )),
                Err(TrustPathError::NotFound(path)) => ConfigCommandResult::error(format!(
                    "Path not found: {} — supply an existing directory or file.",
                    path.display()
                )),
                Err(TrustPathError::UpdateFailed(err)) => {
                    ConfigCommandResult::error(format!("Failed to update trust list: {err}"))
                }
            }
        }
        "remove" | "rm" | "del" | "delete" => {
            if rest.is_empty() {
                return ConfigCommandResult::error("Usage: /trust remove <path>");
            }
            match facet.remove_trusted_path(rest) {
                Ok((path, true)) => ConfigCommandResult::message(format!(
                    "Removed from trust list: {}",
                    path.display()
                )),
                Ok((path, false)) => {
                    ConfigCommandResult::message(format!("Not in trust list: {}", path.display()))
                }
                Err(TrustPathError::UpdateFailed(err)) => {
                    ConfigCommandResult::error(format!("Failed to update trust list: {err}"))
                }
                Err(TrustPathError::NotFound(_)) => {
                    ConfigCommandResult::error("Usage: /trust remove <path>")
                }
            }
        }
        other => ConfigCommandResult::error(format!(
            "Unknown /trust action `{other}`. Use `/trust`, `/trust on|off [--save]`, `/trust add <path>`, or `/trust remove <path>`."
        )),
    }
}

/// `/auth xai-device|chatgpt|chatgpt-revoke|claude|claude-revoke|orcarouter|orcarouter-revoke`.
/// Pure: selects a capability-launch action, no host observation.
pub fn auth(arg: Option<&str>) -> ConfigCommandResult {
    let action = match arg.map(str::trim) {
        Some("xai-device") | Some("xai_device") => ConfigAction::StartXaiDeviceLogin,
        Some("claude") | Some("anthropic") => ConfigAction::StartClaudeLogin,
        Some("claude-revoke") => ConfigAction::StartClaudeRevoke,
        Some("chatgpt") | Some("openai-codex") | Some("openai_codex") => {
            ConfigAction::StartChatgptPkceLogin
        }
        Some("chatgpt-revoke") | Some("chatgpt_revoke") => ConfigAction::StartChatgptRevoke,
        Some("orcarouter") | Some("orca") => ConfigAction::StartOrcarouterPkceLogin,
        Some("orcarouter-revoke") | Some("orcarouter_revoke") | Some("orca-revoke") => {
            ConfigAction::StartOrcarouterRevoke
        }
        _ => {
            return ConfigCommandResult {
                message: Some(
                    "Usage: /auth xai-device|chatgpt|chatgpt-revoke|claude|claude-revoke|orcarouter|orcarouter-revoke"
                        .to_string(),
                ),
                action: None,
                is_error: true,
            };
        }
    };
    ConfigCommandResult {
        message: None,
        action: Some(action),
        is_error: false,
    }
}

/// Render the `/config workflow` report from the typed effective facts.
pub fn render_workflow_summary(summary: CommandWorkflowSummary) -> String {
    let on = |value: bool| if value { "on" } else { "off" };
    [
        "[workflow] — config.toml".to_string(),
        format!(
            "automatic = {}  · the agent may start a workflow itself for broad or staged work; off means only /workflow starts one",
            on(summary.automatic)
        ),
        format!(
            "auto_start_read_only = {}  · read-only plans start without an approval card",
            on(summary.auto_start_read_only)
        ),
        format!(
            "require_approval_for_writes = {}  · plans that write, use shell/network, or elevate show an approval card first",
            on(summary.require_approval_for_writes)
        ),
        format!(
            "max_children = {} · max_concurrent = {} · max_depth = {}  · hard ceilings for one run",
            summary.max_children, summary.max_concurrent, summary.max_depth
        ),
        format!(
            "default_token_budget = {}  · shared admission cap for a run and its children (0 = none)",
            summary.default_token_budget
        ),
        String::new(),
        "[goal] — config.toml".to_string(),
        format!(
            "max_continuations = {}  · automatic continuation passes before a goal pauses; 0 = unlimited (completion, blocked, or you stop it)",
            summary.goal_max_continuations
        ),
        format!(
            "enforce_token_budget = {}  · true = a goal's token budget is a hard stop; false = advisory telemetry",
            on(summary.goal_enforce_token_budget)
        ),
    ]
    .join("\n")
}

/// Compose the `/import-claude --apply` follow-up sentence for a typed fact.
fn render_follow_up(item: &codewhale_command_contract::config::CommandImportFollowUp) -> String {
    use codewhale_command_contract::config::CommandImportFollowUp as FollowUp;
    match item {
        FollowUp::InstructionsCopied { destination } => {
            format!("Standing instructions copied to {destination}.")
        }
        FollowUp::InstructionsKeptExisting {
            destination,
            source,
        } => format!(
            "{destination} already exists — left untouched; merge from {source} by hand if you want both."
        ),
        FollowUp::InstructionsCopyFailed {
            source,
            destination,
        } => format!("Could not copy {source} — copy it to {destination} by hand."),
        FollowUp::HomeUnresolved => {
            "Could not resolve the Codewhale home, so standing instructions were not copied."
                .to_string()
        }
        FollowUp::McpBlocked { name } => {
            format!("MCP `{name}` is disabled at its source and was not imported.")
        }
        FollowUp::McpNeedsReview { name } => format!(
            "MCP `{name}` needs a separate review: run /mcp import, then use its displayed approval command. It will be imported OFF."
        ),
        FollowUp::HookEvents { count } => format!(
            "{count} hook event(s) still need mapping with /hooks — hooks run code, so they are never imported for you."
        ),
        FollowUp::PermissionRules => {
            "Permission rules are listed in the report; apply the ones you want with /permissions."
                .to_string()
        }
    }
}

/// A parsed `/config subagents` tail.
#[derive(Debug, PartialEq, Eq)]
enum SubagentOp<'a> {
    Status,
    Get(&'a str),
    Set { key: &'a str, value: &'a str },
    Usage,
}

/// Parse the `/config subagents …` grammar in the command.
fn parse_subagent_rest(rest: &str) -> (SubagentOp<'_>, bool) {
    let mut tokens = rest.split_whitespace().collect::<Vec<_>>();
    let persist = matches!(tokens.last(), Some(&"--save" | &"-s"));
    if persist {
        tokens.pop();
    }
    let op = match tokens.as_slice() {
        [] | ["status"] => SubagentOp::Status,
        ["on"] | ["enable"] | ["enabled"] => SubagentOp::Set {
            key: "enabled",
            value: "true",
        },
        ["off"] | ["disable"] | ["disabled"] => SubagentOp::Set {
            key: "enabled",
            value: "false",
        },
        [key] => SubagentOp::Get(key),
        [key, value] => SubagentOp::Set { key, value },
        _ => SubagentOp::Usage,
    };
    (op, persist)
}

/// A parsed `/config search…` tail.
#[derive(Debug, PartialEq, Eq)]
enum SearchOp<'a> {
    Status,
    Set(&'a str),
    Usage,
}

/// Parse the `/config search[.provider] …` grammar in the command.
fn parse_search_rest(rest: &str) -> (SearchOp<'_>, bool) {
    let mut tokens = rest.split_whitespace().collect::<Vec<_>>();
    let persist = matches!(tokens.last(), Some(&"--save" | &"-s"));
    if persist {
        tokens.pop();
    }
    let op = match tokens.as_slice() {
        [] | ["status"] | ["provider"] => SearchOp::Status,
        ["provider", value] | [value] => SearchOp::Set(value),
        _ => SearchOp::Usage,
    };
    (op, persist)
}

/// A parsed `/config notifications…` tail.
#[derive(Debug, PartialEq, Eq)]
enum NotificationsOp<'a> {
    Status,
    Get(&'a str),
    Set { key: &'a str, value: &'a str },
}

/// Parse the `/config notifications …` grammar in the command.
fn parse_notifications_rest(rest: &str) -> (NotificationsOp<'_>, bool) {
    let (rest, persist) = rest
        .strip_suffix(" --save")
        .or_else(|| rest.strip_suffix(" -s"))
        .map_or((rest, false), |value| (value.trim_end(), true));
    let op = if rest.is_empty() || rest == "status" {
        NotificationsOp::Status
    } else {
        match rest.split_once(char::is_whitespace) {
            Some((key, value)) => NotificationsOp::Set {
                key,
                value: value.trim(),
            },
            None => NotificationsOp::Get(rest),
        }
    };
    (op, persist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::config::{
        CommandClaudeApplyOutcome, CommandClaudeArtifacts, CommandClaudePlan, CommandConfigContext,
    };
    use codewhale_command_contract::facets::CommandPresentationContext;

    #[test]
    fn missing_facets_fail_safely_with_the_inherited_error_contract() {
        for (name, result) in [
            ("config", config(CommandContexts::empty(), Some("model"))),
            ("pet", pet(CommandContexts::empty(), None)),
            ("trust", trust(CommandContexts::empty(), None)),
        ] {
            assert!(result.is_error, "{name} must fail without its facet");
            assert_eq!(
                result.message.as_deref(),
                Some(format!("Command capability unavailable: {name}").as_str())
            );
            assert!(result.action.is_none(), "no action may cross on failure");
        }
    }

    #[test]
    fn capability_declarations_are_exact() {
        assert!(CONFIG_CAPS.contains(CommandCapabilities::CONFIG));
        assert!(CONFIG_CAPS.contains(CommandCapabilities::PRESENTATION));
        assert!(CONFIG_CAPS.contains(CommandCapabilities::PERMISSIONS));
        // Commands that never consume permission authority must not expose it.
        assert!(CONFIG_ONLY_CAPS.contains(CommandCapabilities::CONFIG));
        assert!(!CONFIG_ONLY_CAPS.contains(CommandCapabilities::PRESENTATION));
        assert!(!CONFIG_ONLY_CAPS.contains(CommandCapabilities::PERMISSIONS));
        assert!(CONFIG_PRESENTATION_CAPS.contains(CommandCapabilities::PRESENTATION));
        assert!(!CONFIG_PRESENTATION_CAPS.contains(CommandCapabilities::PERMISSIONS));
        assert!(PET_CAPS.contains(CommandCapabilities::PET));
        assert!(!PET_CAPS.contains(CommandCapabilities::CONFIG));
        assert!(TRUST_CAPS.contains(CommandCapabilities::TRUST));
        assert!(!TRUST_CAPS.contains(CommandCapabilities::WORKSPACE));
        assert!(!TRUST_CAPS.contains(CommandCapabilities::PRESENTATION));
        assert!(!TRUST_CAPS.contains(CommandCapabilities::CONFIG));
    }

    #[test]
    fn literal_error_needs_no_presentation_capability() {
        // `/workbar` declares only WORK_SURFACE, but its save-error message is a
        // literal: it must render without the presentation facet.
        let mut parts = CommandContexts::empty().into_parts();
        let prepared = PreparedTemplates::default();
        let reply = ConfigReply {
            message: Some(ConfigMessage::literal("Failed to save: permission denied")),
            action: None,
            is_error: true,
        };
        let result = to_result(reply, &mut parts, &prepared);
        assert!(result.is_error);
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Failed to save: permission denied")
        );
    }

    #[test]
    fn keyed_message_without_presentation_fails_loudly() {
        let mut parts = CommandContexts::empty().into_parts();
        let prepared = PreparedTemplates::default();
        let reply = ConfigReply {
            message: Some(ConfigMessage::keyed("config_scope_session")),
            action: Some(ConfigAction::OpenConfigView),
            is_error: false,
        };
        let result = to_result(reply, &mut parts, &prepared);
        assert!(result.is_error, "a missing required capability must fail");
        assert!(result.action.is_none(), "no action on a failed capability");
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Command capability unavailable: presentation")
        );
    }

    /// A config facet that panics the moment any method is touched, so a test
    /// can prove the command validated capabilities before any effect.
    struct PanicConfig;

    impl CommandConfigContext for PanicConfig {
        fn translation_enabled(&self) -> bool {
            panic!("config facet touched")
        }
        fn set_translation_enabled(&mut self, _: bool) {
            panic!("config facet touched")
        }
        fn config_path(&self) -> Option<std::path::PathBuf> {
            panic!("config facet touched")
        }
        fn audit(
            &mut self,
        ) -> Result<codewhale_command_contract::config::CommandConfigAuditReport, String> {
            panic!("config facet touched")
        }
        fn workflow_summary(&mut self) -> CommandWorkflowSummary {
            panic!("config facet touched")
        }
        fn subagents_status(
            &mut self,
        ) -> Result<codewhale_command_contract::config::CommandSubagentsReport, String> {
            panic!("config facet touched")
        }
        fn subagent(&mut self, _key: &str) -> ConfigReply {
            panic!("config facet touched")
        }
        fn set_subagent(&mut self, _key: &str, _value: &str, _persist: bool) -> ConfigReply {
            panic!("config facet touched")
        }
        fn search_provider_status(&mut self) -> ConfigReply {
            panic!("config facet touched")
        }
        fn search_provider_names(&self) -> &'static str {
            panic!("config facet touched")
        }
        fn set_search_provider(&mut self, _value: &str, _persist: bool) -> ConfigReply {
            panic!("config facet touched")
        }
        fn notifications_status(&mut self) -> ConfigReply {
            panic!("config facet touched")
        }
        fn notification(&mut self, _key: &str) -> ConfigReply {
            panic!("config facet touched")
        }
        fn set_notification(&mut self, _key: &str, _value: &str, _persist: bool) -> ConfigReply {
            panic!("config facet touched")
        }
        fn apply_preset(&mut self, _name: &str, _persist: bool) -> ConfigReply {
            panic!("config facet touched")
        }
        fn get_value(&mut self, _key: &str) -> ConfigReply {
            panic!("config facet touched")
        }
        fn set_value(&mut self, _key: &str, _value: &str, _persist: bool) -> ConfigReply {
            panic!("config facet touched")
        }
        fn show_settings(
            &mut self,
        ) -> Result<codewhale_command_contract::config::CommandSettingsReport, String> {
            panic!("config facet touched")
        }
        fn theme_schema(&self) -> String {
            panic!("config facet touched")
        }
        fn themes_dir(&self) -> Result<String, String> {
            panic!("config facet touched")
        }
        fn verbose_enabled(&self) -> bool {
            panic!("config facet touched")
        }
        fn set_verbose_enabled(&mut self, _enabled: bool) {
            panic!("config facet touched")
        }
        fn screen_mode(&self) -> codewhale_command_contract::types::CommandScreenMode {
            panic!("config facet touched")
        }
        fn build_plan(&mut self) -> Option<CommandClaudePlan> {
            panic!("config facet touched")
        }
        fn write_artifacts(
            &mut self,
            _report: &str,
            _bundle: Option<&str>,
        ) -> CommandClaudeArtifacts {
            panic!("config facet touched")
        }
        fn apply_import(
            &mut self,
            _plan: &CommandClaudePlan,
            _report_path: &std::path::Path,
            _wrote_report: bool,
        ) -> CommandClaudeApplyOutcome {
            panic!("config facet touched")
        }
        fn display_path(&self, _path: &std::path::Path) -> String {
            panic!("config facet touched")
        }
    }

    /// Presentation that resolves every template except `failing`.
    struct OneMissingTemplate(&'static str);

    impl CommandPresentationContext for OneMissingTemplate {
        fn translate(&self, key: &str, _r: &[(&str, &str)]) -> Result<String, String> {
            if key == self.0 {
                Err(format!("{key}: unknown translation key"))
            } else {
                Ok(key.to_string())
            }
        }
    }

    /// Presentation that succeeds once per key and fails on any second call,
    /// proving prepared rendering never re-translates after an effect.
    #[derive(Default)]
    struct OncePerKey(std::cell::RefCell<std::collections::HashSet<String>>);

    impl CommandPresentationContext for OncePerKey {
        fn translate(&self, key: &str, r: &[(&str, &str)]) -> Result<String, String> {
            if !self.0.borrow_mut().insert(key.to_string()) {
                return Err(format!("{key} translated twice"));
            }
            // Echo the replacement values so the preflight sentinels survive.
            Ok(r.iter().map(|(_, value)| *value).collect())
        }
    }

    #[test]
    fn prepared_rendering_never_translates_again_after_preflight() {
        let mut presentation = OncePerKey::default();
        let contexts = CommandContexts::empty().with_presentation(&mut presentation);
        let mut parts = contexts.into_parts();
        let prepared = preflight(&mut parts).expect("preflight validates every template once");
        let reply = ConfigReply::message(ConfigMessage::keyed_with(
            "config_search_updated",
            vec![
                ("value".to_string(), ConfigMessage::literal("duckduckgo")),
                (
                    "scope".to_string(),
                    ConfigMessage::keyed("config_scope_saved"),
                ),
            ],
        ));
        let result = to_result(reply, &mut parts, &prepared);
        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(result.message.as_deref(), Some("duckduckgo"));
    }

    /// Presentation that echoes replacement values and counts translations.
    #[derive(Default)]
    struct EchoWithCount(std::cell::Cell<usize>);

    impl CommandPresentationContext for EchoWithCount {
        fn translate(&self, _key: &str, r: &[(&str, &str)]) -> Result<String, String> {
            self.0.set(self.0.get() + 1);
            Ok(r.iter().map(|(_, value)| *value).collect())
        }
    }

    #[test]
    fn prepared_rendering_rejects_missing_duplicate_and_extra_parameters() {
        for (name, params) in [
            (
                "missing scope",
                vec![("value".to_string(), ConfigMessage::literal("duckduckgo"))],
            ),
            (
                "duplicate value",
                vec![
                    ("value".to_string(), ConfigMessage::literal("a")),
                    ("value".to_string(), ConfigMessage::literal("b")),
                    (
                        "scope".to_string(),
                        ConfigMessage::keyed("config_scope_saved"),
                    ),
                ],
            ),
            (
                "extra parameter",
                vec![
                    ("value".to_string(), ConfigMessage::literal("duckduckgo")),
                    (
                        "scope".to_string(),
                        ConfigMessage::keyed("config_scope_saved"),
                    ),
                    ("extra".to_string(), ConfigMessage::literal("x")),
                ],
            ),
        ] {
            let mut presentation = EchoWithCount::default();
            let contexts = CommandContexts::empty().with_presentation(&mut presentation);
            let mut parts = contexts.into_parts();
            let prepared = preflight(&mut parts).expect("preflight");
            let reply = ConfigReply::with_message_and_action(
                ConfigMessage::keyed_with("config_search_updated", params),
                ConfigAction::OpenConfigView,
            );
            let result = to_result(reply, &mut parts, &prepared);
            assert!(result.is_error, "{name} must fail");
            assert!(result.action.is_none(), "{name} must not keep the action");
            assert!(
                result
                    .message
                    .as_deref()
                    .is_some_and(|m| m.contains("invalid translation replacement contract")),
                "{name}: {:?}",
                result.message
            );
        }
    }

    /// Work-surface facet that rejects every setter.
    struct RejectingWorkSurface;

    impl codewhale_command_contract::config::CommandWorkSurfaceContext for RejectingWorkSurface {
        fn placement(&self) -> codewhale_command_contract::types::CommandWorkSurfacePlacement {
            codewhale_command_contract::types::CommandWorkSurfacePlacement::Bottom
        }
        fn panel(&self) -> codewhale_command_contract::types::CommandRailPanel {
            codewhale_command_contract::types::CommandRailPanel::Tasks
        }
        fn effective_placement(
            &self,
        ) -> codewhale_command_contract::types::CommandWorkSurfacePlacement {
            codewhale_command_contract::types::CommandWorkSurfacePlacement::Bottom
        }
        fn area_present(&self) -> bool {
            true
        }
        fn panel_title(
            &self,
            _panel: codewhale_command_contract::types::CommandRailPanel,
        ) -> String {
            "Tasks".to_string()
        }
        fn set_placement(
            &mut self,
            _placement: codewhale_command_contract::types::CommandWorkSurfacePlacement,
            _persist: bool,
        ) -> ConfigReply {
            ConfigReply::error("workbar placement rejected")
        }
        fn set_panel(
            &mut self,
            _panel: codewhale_command_contract::types::CommandRailPanel,
            _persist: bool,
        ) -> ConfigReply {
            ConfigReply::error("workbar panel rejected")
        }
        fn mark_redraw(&mut self) {}
    }

    #[test]
    fn workbar_surfaces_setter_errors_without_save() {
        // No presentation facet: the setter error is literal, so `/workbar`
        // must not depend on the config templates.
        let mut work_surface = RejectingWorkSurface;
        let contexts = CommandContexts::empty().with_work_surface(&mut work_surface);
        // No `--save`: the setter error must still surface.
        let result = workbar(contexts, Some("left"));
        assert!(result.is_error, "{:?}", result.message);
        assert_eq!(
            result.message.as_deref(),
            Some("Error: workbar placement rejected")
        );
    }

    #[test]
    fn workbar_status_needs_no_presentation() {
        let mut work_surface = RejectingWorkSurface;
        let contexts = CommandContexts::empty().with_work_surface(&mut work_surface);
        let result = workbar(contexts, None);
        assert!(!result.is_error, "{:?}", result.message);
        assert!(result.message.is_some());
    }

    /// Presentation whose `config_search_updated` template repeats `{value}`.
    struct RepeatedPlaceholder;

    impl CommandPresentationContext for RepeatedPlaceholder {
        fn translate(&self, key: &str, r: &[(&str, &str)]) -> Result<String, String> {
            if key == "config_search_updated" && r.len() == 2 {
                return Ok(format!("{} {} {}", r[0].1, r[0].1, r[1].1));
            }
            Ok(r.iter().map(|(_, value)| *value).collect())
        }
    }

    #[test]
    fn prepared_rendering_allows_a_repeated_placeholder() {
        let mut presentation = RepeatedPlaceholder;
        let contexts = CommandContexts::empty().with_presentation(&mut presentation);
        let mut parts = contexts.into_parts();
        let prepared = preflight(&mut parts).expect("preflight");
        let reply = ConfigReply::with_message_and_action(
            ConfigMessage::keyed_with(
                "config_search_updated",
                vec![
                    ("value".to_string(), ConfigMessage::literal("duckduckgo")),
                    (
                        "scope".to_string(),
                        ConfigMessage::keyed("config_scope_saved"),
                    ),
                ],
            ),
            ConfigAction::OpenConfigView,
        );
        let result = to_result(reply, &mut parts, &prepared);
        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(result.action, Some(ConfigAction::OpenConfigView));
        assert_eq!(result.message.as_deref(), Some("duckduckgo duckduckgo "));
    }

    #[test]
    fn unprepared_key_errors_without_translating_after_an_effect() {
        let mut presentation = EchoWithCount::default();
        let contexts = CommandContexts::empty().with_presentation(&mut presentation);
        let mut parts = contexts.into_parts();
        // No preflight: an unprepared key must never reach the facet.
        let prepared = PreparedTemplates::default();
        let reply = ConfigReply::with_message_and_action(
            ConfigMessage::keyed("not_a_prepared_key"),
            ConfigAction::OpenConfigView,
        );
        let result = to_result(reply, &mut parts, &prepared);
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|m| m.contains("unprepared message key")),
            "{:?}",
            result.message
        );
        assert_eq!(
            presentation.0.get(),
            0,
            "an unprepared key must not translate after the effect"
        );
    }

    #[test]
    fn preflight_rejects_missing_presentation_before_effects() {
        let mut parts = CommandContexts::empty().into_parts();
        let Err(result) = preflight(&mut parts) else {
            panic!("a missing presentation capability must fail preflight");
        };
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(
            result.message.as_deref(),
            Some("Command capability unavailable: presentation")
        );
    }

    #[test]
    fn config_fails_before_touching_the_facet_when_presentation_is_missing() {
        // `PanicConfig` panics on any call: passing proves no effect ran.
        let mut facet = PanicConfig;
        let contexts = CommandContexts::empty().with_config(&mut facet);
        let result = config(contexts, Some("search duckduckgo --save"));
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(
            result.message.as_deref(),
            Some("Command capability unavailable: presentation")
        );
    }

    #[test]
    fn config_fails_before_mutating_when_a_required_template_is_missing() {
        let mut facet = PanicConfig;
        let mut presentation = OneMissingTemplate("config_search_updated");
        let contexts = CommandContexts::empty()
            .with_config(&mut facet)
            .with_presentation(&mut presentation);
        let result = config(contexts, Some("search duckduckgo --save"));
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|m| m.contains("config_search_updated"))
        );
    }

    #[test]
    fn translation_failure_inside_a_nested_parameter_propagates() {
        let mut presentation = OneMissingTemplate("config_scope_saved");
        let contexts = CommandContexts::empty().with_presentation(&mut presentation);
        let mut parts = contexts.into_parts();
        let prepared = PreparedTemplates::default();
        let reply = ConfigReply::with_message_and_action(
            ConfigMessage::keyed_with(
                "config_search_updated",
                vec![
                    ("value".to_string(), ConfigMessage::literal("duckduckgo")),
                    (
                        "scope".to_string(),
                        ConfigMessage::keyed("config_scope_saved"),
                    ),
                ],
            ),
            ConfigAction::OpenConfigView,
        );
        let result = to_result(reply, &mut parts, &prepared);
        assert!(result.is_error, "a nested translation failure must fail");
        assert!(result.action.is_none(), "no action on a failed translation");
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|m| m.contains("config_scope_saved"))
        );
    }

    #[test]
    fn subagent_grammar_lives_in_the_command() {
        assert_eq!(parse_subagent_rest(""), (SubagentOp::Status, false));
        assert_eq!(parse_subagent_rest("status"), (SubagentOp::Status, false));
        assert_eq!(
            parse_subagent_rest("on"),
            (
                SubagentOp::Set {
                    key: "enabled",
                    value: "true",
                },
                false,
            )
        );
        assert_eq!(
            parse_subagent_rest("off --save"),
            (
                SubagentOp::Set {
                    key: "enabled",
                    value: "false",
                },
                true,
            )
        );
        assert_eq!(
            parse_subagent_rest("max_depth"),
            (SubagentOp::Get("max_depth"), false)
        );
        assert_eq!(
            parse_subagent_rest("max_depth 4 -s"),
            (
                SubagentOp::Set {
                    key: "max_depth",
                    value: "4",
                },
                true,
            )
        );
        assert_eq!(parse_subagent_rest("a b c"), (SubagentOp::Usage, false));
    }

    #[test]
    fn search_grammar_lives_in_the_command() {
        assert_eq!(parse_search_rest(""), (SearchOp::Status, false));
        assert_eq!(parse_search_rest("status"), (SearchOp::Status, false));
        assert_eq!(parse_search_rest("provider"), (SearchOp::Status, false));
        assert_eq!(
            parse_search_rest("duckduckgo"),
            (SearchOp::Set("duckduckgo"), false)
        );
        assert_eq!(
            parse_search_rest("provider tavily --save"),
            (SearchOp::Set("tavily"), true)
        );
        assert_eq!(parse_search_rest("a b"), (SearchOp::Usage, false));
    }

    #[test]
    fn notifications_grammar_lives_in_the_command() {
        assert_eq!(
            parse_notifications_rest(""),
            (NotificationsOp::Status, false)
        );
        assert_eq!(
            parse_notifications_rest("status"),
            (NotificationsOp::Status, false)
        );
        assert_eq!(
            parse_notifications_rest("method"),
            (NotificationsOp::Get("method"), false)
        );
        assert_eq!(
            parse_notifications_rest("method sound --save"),
            (
                NotificationsOp::Set {
                    key: "method",
                    value: "sound",
                },
                true,
            )
        );
        assert_eq!(
            parse_notifications_rest("threshold_secs  30 -s"),
            (
                NotificationsOp::Set {
                    key: "threshold_secs",
                    value: "30",
                },
                true,
            )
        );
    }
}
