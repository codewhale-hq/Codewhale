use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

/// A Router setup preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterPreset<R> {
    /// Jev; `None` picks whichever route has a key (OpenRouter first).
    Jev(Option<R>),
    Fast,
    Off,
    Custom,
}

impl<R> RouterPreset<R> {
    /// Parse `jev [openrouter|typesafe]`, `fast`, `off` or `custom`.
    #[must_use]
    pub fn parse(args: &str, parse_route: impl FnOnce(&str) -> Option<R>) -> Option<Self> {
        let mut words = args.split_whitespace();
        let preset = match words.next()?.to_ascii_lowercase().as_str() {
            "jev" => Self::Jev(match words.next() {
                None => None,
                Some(route) => Some(parse_route(route)?),
            }),
            "fast" => Self::Fast,
            "off" | "none" => Self::Off,
            "custom" => Self::Custom,
            _ => return None,
        };
        words.next().is_none().then_some(preset)
    }

    /// The `/router` arguments that select this preset.
    #[must_use]
    pub fn command_args(self, route_name: impl FnOnce(R) -> &'static str) -> String {
        match self {
            Self::Jev(None) => "jev".to_string(),
            Self::Jev(Some(route)) => format!("jev {}", route_name(route)),
            Self::Fast => "fast".to_string(),
            Self::Off => "off".to_string(),
            Self::Custom => "custom".to_string(),
        }
    }
}

/// What `/router` asked the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterRequest<R> {
    /// Open the Router setup view.
    Open,
    /// Make one test call with this preset and show the result.
    Test(RouterPreset<R>),
    /// Persist this preset to `config.toml` and the live config.
    Save(RouterPreset<R>),
}

/// Parse `/router` (or `/model router`) arguments.
pub fn parse_router_args<R>(
    args: Option<&str>,
    parse_route: impl FnOnce(&str) -> Option<R>,
) -> Result<RouterRequest<R>, String> {
    let args = args.map(str::trim).unwrap_or_default();
    if args.is_empty() {
        return Ok(RouterRequest::Open);
    }
    let (save, rest) = match args.split_once(char::is_whitespace) {
        Some((head, rest)) if head.eq_ignore_ascii_case("save") => (true, rest.trim()),
        _ => (false, args),
    };
    let preset = RouterPreset::parse(rest, parse_route).ok_or_else(|| {
        "Usage: /router [jev [openrouter|typesafe]|fast|off|custom] · /router save <preset>"
            .to_string()
    })?;
    Ok(if save {
        RouterRequest::Save(preset)
    } else {
        RouterRequest::Test(preset)
    })
}

pub trait RouterHost {
    type Route;
    fn parse_route(name: &str) -> Option<Self::Route>;
    fn router_action(request: RouterRequest<Self::Route>) -> Self;
}

pub struct RouterCmd;

impl<A: RouterHost> RegisterCommand<CommandResult<A>> for RouterCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "router",
            aliases: &[],
            usage: "/router [jev|fast|off|custom|save]",
            description_key: "cmd_router_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Pure(|args| match parse_router_args(args, A::parse_route) {
            Ok(request) => CommandResult::action(A::router_action(request)),
            Err(reason) => CommandResult::error(reason),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saving_a_preset_requires_an_explicit_verb_and_valid_route() {
        let parse = |args| parse_router_args(args, |name| (name == "vendor").then_some(7));
        assert_eq!(parse(None), Ok(RouterRequest::Open));
        assert_eq!(
            parse(Some("jev vendor")),
            Ok(RouterRequest::Test(RouterPreset::Jev(Some(7))))
        );
        assert_eq!(
            parse(Some("SAVE Jev vendor")),
            Ok(RouterRequest::Save(RouterPreset::Jev(Some(7))))
        );
        assert!(parse(Some("save jev unavailable")).is_err());
        assert!(parse(Some("fast vendor")).is_err());
    }
}
