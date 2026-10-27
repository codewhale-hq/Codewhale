//! `/links` command.

use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::navigation::{NavigationLinks, NavigationProviderKind};
use codewhale_command_contract::outcome::CoreNavigationCommandResult as CommandResult;
use std::fmt::Write;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "links",
    aliases: &["dashboard", "api", "lianjie"],
    usage: "/links",
    description_key: "cmd_links_description",
};

pub struct LinksCmd;

impl RegisterCommand<CommandResult> for LinksCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::NAVIGATION.union(CommandCapabilities::PRESENTATION),
            handler: links,
        }
    }
}

fn links(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(navigation) = parts.navigation else {
        return CommandResult::error("Command capability unavailable: navigation");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    render_links(presentation, navigation.links())
        .map(CommandResult::message)
        .unwrap_or_else(CommandResult::error)
}

pub(crate) fn public_site_locale_segment(tag: &str) -> &'static str {
    match tag {
        "zh-Hans" | "zh-Hant" => "zh",
        "ja" => "ja",
        "vi" => "vi",
        "ko" => "ko",
        "es-419" => "es",
        "pt-BR" => "pt-BR",
        "ru" => "ru",
        "uk" => "uk",
        _ => "en",
    }
}

fn render_links(
    presentation: &dyn CommandPresentationContext,
    facts: NavigationLinks,
) -> Result<String, String> {
    let label = |key| presentation.translate(key, &[]);
    let site_locale = public_site_locale_segment(&facts.locale_tag);
    let mut message = format!(
        "{}\n─────────────────────────────\n",
        label("links_project_title")?
    );
    let _ = writeln!(
        message,
        "{} `https://codewhale.net/{site_locale}/docs`",
        label("links_documentation")?
    );
    let _ = writeln!(
        message,
        "{} `https://codewhale.net/{site_locale}/community`",
        label("links_community")?
    );
    let _ = writeln!(
        message,
        "{} `https://github.com/codewhale-hq/CodeWhale`",
        label("links_git_hub")?
    );
    let _ = writeln!(
        message,
        "{} `https://app.codewhale.net`",
        label("links_managed_app")?
    );
    let _ = writeln!(message, "{}", label("links_managed_app_note")?);
    let _ = write!(
        message,
        "\n{}\n─────────────────────────────\n",
        label("links_title")?
    );
    for provider in facts.providers {
        let active_marker = if provider.id == facts.active_provider {
            " <- current"
        } else {
            ""
        };
        let _ = writeln!(
            message,
            "\n{} ({}){}",
            provider.name, provider.id, active_marker
        );
        if let Some(key_url) = provider.credential_url {
            let _ = writeln!(message, "{} `{}`", label("links_dashboard")?, key_url);
        } else {
            let _ = writeln!(
                message,
                "{} {}",
                label("links_dashboard")?,
                provider.guidance
            );
        }
        if let Some(docs_url) = provider.docs_url {
            let _ = writeln!(message, "{}      `{}`", label("links_docs")?, docs_url);
        }
        if provider.kind == NavigationProviderKind::Moonshot {
            let _ = writeln!(
                message,
                "{}",
                presentation.translate(
                    "links_kimi_code_route_note",
                    &[
                        ("route", &facts.kimi_route),
                        ("console", &facts.kimi_console),
                    ]
                )?
            );
        }
        if provider.env_names.is_empty() {
            let _ = writeln!(message, "Env: none");
        } else {
            let _ = writeln!(message, "Env: {}", provider.env_names.join(", "));
        }
    }
    let _ = writeln!(message, "\n{}", label("links_tip")?);
    Ok(message)
}
