//! Portable numbered, confirmation-gated permission editor.
use super::interpolate;
use super::policy_messages::{PermissionsMessages as Messages, PermissionsText as Text};
use codewhale_command_contract::config_policy::*;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{
    ConfigPolicyAction, ConfigPolicyCommandResult as CommandResult,
};
use codewhale_command_contract::types::CommandApprovalMode;
use codewhale_protocol::display::quote_os_path;

pub const CAPABILITIES: CommandCapabilities =
    CommandCapabilities::PERMISSIONS.union(CommandCapabilities::PRESENTATION);
pub struct PermissionsCmd;
impl RegisterCommand<CommandResult> for PermissionsCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "permissions",
            aliases: &["permission-rules", "permission_rules"],
            usage: "/permissions [list|remove <rule-number> [--confirm <token>]]",
            description_key: "cmd_permissions_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CAPABILITIES,
            handler: execute,
        }
    }
}
pub fn execute(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(permissions) = parts.permissions else {
        return CommandResult::error("Command capability unavailable: permissions");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let messages = match Messages::load(presentation) {
        Ok(messages) => messages,
        Err(error) => return CommandResult::error(error),
    };
    run(permissions, &messages, arg)
}
fn render(m: &Messages, id: Text, values: &[(&str, &str)]) -> String {
    interpolate(&m.text(id), values)
}
fn run(
    permissions: &mut dyn CommandPermissionsContext,
    m: &Messages,
    arg: Option<&str>,
) -> CommandResult {
    let raw = arg.map(str::trim).unwrap_or("");
    if raw.is_empty() || raw.eq_ignore_ascii_case("list") || raw.eq_ignore_ascii_case("status") {
        return match permissions.snapshot() {
            Ok(view) => CommandResult::message(format_snapshot(m, &view)),
            Err(error) => operation_error(m, &error),
        };
    }
    let parts: Vec<_> = raw.split_whitespace().collect();
    if !parts
        .first()
        .is_some_and(|part| part.eq_ignore_ascii_case("remove"))
        || !matches!(parts.len(), 2 | 4)
    {
        return CommandResult::error(m.text(Text::Usage));
    }
    let Ok(display_index) = parts[1].parse::<usize>() else {
        return CommandResult::error(m.text(Text::Usage));
    };
    let Some(index) = display_index.checked_sub(1) else {
        return rule_not_found(m, display_index);
    };
    if parts.len() == 2 {
        let view = match permissions.snapshot() {
            Ok(view) => view,
            Err(error) => return operation_error(m, &error),
        };
        let Some(rule) = view.rules.get(index) else {
            return rule_not_found(m, display_index);
        };
        let command = format!(
            "/permissions remove {display_index} --confirm {}",
            rule.removal_token
        );
        return CommandResult::message(render(
            m,
            Text::RemovePreview,
            &[
                ("{index}", &display_index.to_string()),
                ("{rule}", &format_rule(m, display_index, rule)),
                ("{command}", &command),
            ],
        ));
    }
    if !parts[2].eq_ignore_ascii_case("--confirm") || parts[3].is_empty() {
        return CommandResult::error(m.text(Text::Usage));
    }
    // Every translation contract was validated before the atomic host operation.
    match permissions.remove_rule(index, parts[3]) {
        Ok(removed) => CommandResult::with_message_and_action(
            render(
                m,
                Text::Removed,
                &[
                    ("{index}", &display_index.to_string()),
                    ("{action}", action_name(removed.action)),
                    ("{tool}", &escape_field(&removed.tool)),
                ],
            ),
            ConfigPolicyAction::PermissionRulesChanged,
        ),
        Err(error) => operation_error(m, &error),
    }
}
fn format_snapshot(m: &Messages, view: &PermissionsView) -> String {
    let state = match view.file_state {
        CommandPermissionsFileState::Missing => Text::FileMissing,
        CommandPermissionsFileState::Empty => Text::FileEmpty,
        CommandPermissionsFileState::Present => Text::FilePresent,
    };
    let mut output = render(
        m,
        Text::ListHeader,
        &[
            ("{count}", &view.rules.len().to_string()),
            ("{file_state}", &m.text(state)),
            ("{path}", &quote_os_path(&view.path)),
        ],
    );
    if view.rules.is_empty() {
        output.push('\n');
        output.push_str(&m.text(Text::NoRules));
    } else {
        for (index, rule) in view.rules.iter().enumerate() {
            output.push_str("\n\n");
            output.push_str(&format_rule(m, index + 1, rule));
        }
    }
    output.push_str("\n\n");
    let (label, explanation) = match view.approval_mode {
        CommandApprovalMode::Suggest => ("Ask", Text::PostureAsk),
        CommandApprovalMode::AcceptEdits => ("Accept Edits", Text::PostureAcceptEdits),
        CommandApprovalMode::Auto => ("Auto-Review", Text::PostureAuto),
        CommandApprovalMode::Bypass => ("Full Access", Text::PostureBypass),
        CommandApprovalMode::Never => ("Never", Text::PostureNever),
    };
    output.push_str(&render(m, Text::PostureHeader, &[("{posture}", label)]));
    output.push('\n');
    output.push_str(&m.text(explanation));
    output.push('\n');
    let path = view
        .audit_path
        .as_deref()
        .map(quote_os_path)
        .unwrap_or_else(|| "$CODEWHALE_HOME/audit.log".into());
    output.push_str(&render(m, Text::ReceiptsNote, &[("{audit_path}", &path)]));
    output
}
fn format_rule(m: &Messages, display_index: usize, rule: &PermissionRule) -> String {
    let scope = rule.workspace.as_deref().map_or_else(
        || m.text(Text::ScopeGlobal).into_owned(),
        |workspace| {
            render(
                m,
                Text::ScopeRepo,
                &[("{workspace}", &escape_field(workspace))],
            )
        },
    );
    let applicability = m.text(if rule.applies_here {
        Text::AppliesHere
    } else {
        Text::InactiveHere
    });
    let mut matchers = Vec::new();
    if let Some(command) = rule.command.as_deref() {
        matchers.push(render(
            m,
            if rule.command_exact {
                Text::MatchExactCommand
            } else {
                Text::MatchCommandPrefix
            },
            &[("{command}", &escape_field(command))],
        ));
    }
    if let Some(path) = rule.path.as_deref() {
        matchers.push(render(
            m,
            Text::MatchExactPath,
            &[("{path}", &escape_field(path))],
        ));
    }
    let matcher = if matchers.is_empty() {
        m.text(Text::MatchAnyInvocation).into_owned()
    } else {
        matchers.join(" + ")
    };
    render(
        m,
        Text::RuleEntry,
        &[
            ("{index}", &display_index.to_string()),
            ("{action}", action_name(rule.action)),
            ("{tool}", &escape_field(&rule.tool)),
            ("{matcher}", &matcher),
            ("{scope}", &scope),
            ("{applicability}", &applicability),
        ],
    )
}
fn action_name(action: CommandPermissionAction) -> &'static str {
    match action {
        CommandPermissionAction::Allow => "allow",
        CommandPermissionAction::Ask => "ask",
        CommandPermissionAction::Deny => "deny",
    }
}
fn rule_not_found(m: &Messages, index: usize) -> CommandResult {
    CommandResult::error(render(
        m,
        Text::RuleNotFound,
        &[("{index}", &index.to_string())],
    ))
}
fn operation_error(m: &Messages, error: &str) -> CommandResult {
    CommandResult::error(render(m, Text::OperationFailed, &[("{error}", error)]))
}

fn escape_field(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character
                if character.is_control()
                    || codewhale_sanitize::sanitize::is_bidi_control(character)
                    || matches!(character, '\u{2028}' | '\u{2029}') =>
            {
                escaped.extend(character.escape_unicode());
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn displayed_rule_fields_escape_terminal_and_bidi_controls() {
        assert_eq!(
            escape_field("cargo\u{1b}\n\u{202e}test"),
            "cargo\\u{1b}\\n\\u{202e}test"
        );
    }
}
