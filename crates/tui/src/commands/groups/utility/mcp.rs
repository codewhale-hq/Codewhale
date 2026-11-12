//! In-TUI MCP manager command parser.

use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;
use crate::tui::app::{AppAction, McpUiAction};

const GITHUB_MCP_URL: &str = "https://api.githubcopilot.com/mcp/";
const CHROME_DEVTOOLS_MCP_PACKAGE: &str = "chrome-devtools-mcp@1.7.0";
const PLAYWRIGHT_MCP_PACKAGE: &str = "@playwright/mcp@0.0.79";
const PLAYWRIGHT_MCP_SOURCE: &str = "https://github.com/microsoft/playwright-mcp";
const CONTAINER_USE_SOURCE: &str = "https://github.com/dagger/container-use";

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "mcp",
    aliases: &["mcps"],
    usage: "/mcp [init|review|approve <digest>|revoke|import|import approve <name>|import decline <name>|recommendations|add recommended <id>|add stdio <name> <command> [args...]|add http <name> <url>|enable <name>|disable <name>|remove <name>|retry <name>|doctor|validate|restart|reload]",
    description_key: "cmd_mcp_description",
};

pub(in crate::commands) struct McpCmd;

impl RegisterCommand<CommandResult> for McpCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::PRESENTATION,
            handler: mcp_contextual,
        }
    }
}

fn mcp_contextual(contexts: CommandContexts<'_>, args: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    mcp(presentation, args)
}

fn mcp(presentation: &mut dyn CommandPresentationContext, args: Option<&str>) -> CommandResult {
    let raw = args.unwrap_or("").trim();
    if raw.is_empty() {
        return CommandResult::action(AppAction::OpenExtensions {
            tab: crate::tui::views::extensions::ExtensionsTab::Mcp,
        });
    }
    if raw.eq_ignore_ascii_case("status") || raw.eq_ignore_ascii_case("list") {
        return CommandResult::action(AppAction::Mcp(McpUiAction::Show));
    }

    let mut parts = raw.split_whitespace();
    let action = parts.next().unwrap_or("").to_ascii_lowercase();
    match action.as_str() {
        "init" => CommandResult::action(AppAction::Mcp(McpUiAction::Init {
            force: parts.any(|part| part == "--force" || part == "-f"),
        })),
        "review" => CommandResult::action(AppAction::Mcp(McpUiAction::ProjectReview)),
        "approve" => match parts.next() {
            Some(digest) => CommandResult::action(AppAction::Mcp(McpUiAction::ProjectApprove {
                digest: digest.to_string(),
            })),
            None => CommandResult::error("Usage: /mcp approve <digest>"),
        },
        "revoke" => CommandResult::action(AppAction::Mcp(McpUiAction::ProjectRevoke)),
        "recommend" | "recommended" | "recommendations" => {
            CommandResult::message(recommended_mcp_text(presentation))
        }
        "add" => parse_add(presentation, parts.collect()),
        "enable" => match parse_name(parts.next(), "Usage: /mcp enable <name>") {
            Ok(name) => CommandResult::action(AppAction::Mcp(McpUiAction::Enable { name })),
            Err(msg) => CommandResult::error(msg),
        },
        "disable" => match parse_name(parts.next(), "Usage: /mcp disable <name>") {
            Ok(name) => CommandResult::action(AppAction::Mcp(McpUiAction::Disable { name })),
            Err(msg) => CommandResult::error(msg),
        },
        "remove" | "rm" => match parse_name(parts.next(), "Usage: /mcp remove <name>") {
            Ok(name) => CommandResult::action(AppAction::Mcp(McpUiAction::Remove { name })),
            Err(msg) => CommandResult::error(msg),
        },
        "login" => match parse_name(parts.next(), "Usage: /mcp login <name> [--scope scope]") {
            Ok(name) => CommandResult::action(AppAction::Mcp(McpUiAction::Login {
                name,
                scopes: parse_scopes(parts.collect()),
            })),
            Err(msg) => CommandResult::error(msg),
        },
        "logout" => match parse_name(parts.next(), "Usage: /mcp logout <name>") {
            Ok(name) => CommandResult::action(AppAction::Mcp(McpUiAction::Logout { name })),
            Err(msg) => CommandResult::error(msg),
        },
        "retry" => match parse_name(parts.next(), "Usage: /mcp retry <name>") {
            Ok(name) => CommandResult::action(AppAction::Mcp(McpUiAction::Retry { name })),
            Err(msg) => CommandResult::error(msg),
        },
        "import" | "marketplace" | "sources" => {
            let sub = parts.next().unwrap_or("").to_ascii_lowercase();
            match sub.as_str() {
                "" | "list" | "status" => {
                    CommandResult::action(AppAction::Mcp(McpUiAction::ImportList))
                }
                "approve" | "add" => {
                    match parse_name(parts.next(), "Usage: /mcp import approve <name>") {
                        Ok(name) => {
                            CommandResult::action(AppAction::Mcp(McpUiAction::ImportApprove {
                                name,
                            }))
                        }
                        Err(msg) => CommandResult::error(msg),
                    }
                }
                "decline" | "deny" | "reject" => {
                    match parse_name(parts.next(), "Usage: /mcp import decline <name>") {
                        Ok(name) => {
                            CommandResult::action(AppAction::Mcp(McpUiAction::ImportDecline {
                                name,
                            }))
                        }
                        Err(msg) => CommandResult::error(msg),
                    }
                }
                _ => {
                    CommandResult::error("Usage: /mcp import [list|approve <name>|decline <name>]")
                }
            }
        }
        "validate" | "doctor" => match parts.next() {
            Some(name) => CommandResult::action(AppAction::Mcp(McpUiAction::Diagnose {
                name: name.to_string(),
            })),
            None => CommandResult::action(AppAction::Mcp(McpUiAction::Validate)),
        },
        "reload" | "reconnect" | "restart" => {
            CommandResult::action(AppAction::Mcp(McpUiAction::Reload))
        }
        _ => CommandResult::error(
            "Usage: /mcp [init|review|approve <digest>|revoke|import|recommendations|add recommended <id>|add stdio <name> <command> [args...]|add http <name> <url>|enable <name>|disable <name>|remove <name>|login <name>|logout <name>|retry <name>|doctor|validate|restart|reload]",
        ),
    }
}

fn parse_name(name: Option<&str>, usage: &str) -> Result<String, String> {
    match name {
        Some(name) if !name.trim().is_empty() => Ok(name.to_string()),
        _ => Err(usage.to_string()),
    }
}

fn parse_add(presentation: &mut dyn CommandPresentationContext, parts: Vec<&str>) -> CommandResult {
    parse_add_for_platform(presentation, parts, cfg!(windows))
}

fn parse_add_for_platform(
    presentation: &mut dyn CommandPresentationContext,
    parts: Vec<&str>,
    windows: bool,
) -> CommandResult {
    if parts
        .first()
        .is_some_and(|part| part.eq_ignore_ascii_case("recommended"))
    {
        return match parts.as_slice() {
            [_, id] if id.eq_ignore_ascii_case("hugging-face") || id.eq_ignore_ascii_case("hf") => {
                CommandResult::action(AppAction::Mcp(McpUiAction::AddHttp {
                    name: "hugging-face".to_string(),
                    url: "https://huggingface.co/mcp".to_string(),
                    transport: None,
                }))
            }
            [_, id] if id.eq_ignore_ascii_case("github") || id.eq_ignore_ascii_case("gh") => {
                CommandResult::action(AppAction::Mcp(McpUiAction::AddHttp {
                    name: "github".to_string(),
                    url: GITHUB_MCP_URL.to_string(),
                    transport: None,
                }))
            }
            [_, id]
                if id.eq_ignore_ascii_case("chrome-devtools")
                    || id.eq_ignore_ascii_case("chrome") =>
            {
                CommandResult::action(AppAction::Mcp(McpUiAction::AddStdio {
                    name: "chrome-devtools".to_string(),
                    command: recommended_npx_command_for(windows).to_string(),
                    args: vec!["-y".to_string(), CHROME_DEVTOOLS_MCP_PACKAGE.to_string()],
                }))
            }
            [_, id] if id.eq_ignore_ascii_case("playwright") => {
                CommandResult::action(AppAction::Mcp(McpUiAction::AddStdio {
                    name: "playwright".to_string(),
                    command: recommended_npx_command_for(windows).to_string(),
                    args: vec![
                        "-y".to_string(),
                        PLAYWRIGHT_MCP_PACKAGE.to_string(),
                        "--isolated".to_string(),
                    ],
                }))
            }
            [_, id]
                if id.eq_ignore_ascii_case("container-use")
                    || id.eq_ignore_ascii_case("container") =>
            {
                CommandResult::action(AppAction::Mcp(McpUiAction::AddStdio {
                    name: "container-use".to_string(),
                    command: "container-use".to_string(),
                    args: vec!["stdio".to_string()],
                }))
            }
            [_, _] => CommandResult::error(mcp_unknown_id(presentation)),
            _ => CommandResult::error("Usage: /mcp add recommended <id>"),
        };
    }
    if parts.len() < 3 {
        return CommandResult::error(
            "Usage: /mcp add stdio <name> <command> [args...] OR /mcp add http <name> <url>",
        );
    }
    match parts[0].to_ascii_lowercase().as_str() {
        "stdio" => CommandResult::action(AppAction::Mcp(McpUiAction::AddStdio {
            name: parts[1].to_string(),
            command: parts[2].to_string(),
            args: parts[3..].iter().map(|s| (*s).to_string()).collect(),
        })),
        "http" => CommandResult::action(AppAction::Mcp(McpUiAction::AddHttp {
            name: parts[1].to_string(),
            url: parts[2].to_string(),
            transport: None,
        })),
        "sse" => CommandResult::action(AppAction::Mcp(McpUiAction::AddHttp {
            name: parts[1].to_string(),
            url: parts[2].to_string(),
            transport: Some("sse".to_string()),
        })),
        _ => CommandResult::error(
            "Usage: /mcp add stdio <name> <command> [args...] OR /mcp add http <name> <url>",
        ),
    }
}

/// Localized unknown-recommendation error through the presentation facet (D3).
fn mcp_unknown_id(presentation: &mut dyn CommandPresentationContext) -> String {
    presentation
        .translate(
            "mcp_recommended_unknown_id",
            &[("recommendations_command", "/mcp recommendations")],
        )
        .unwrap_or_else(|_| {
            "Unknown MCP suggestion. Run /mcp recommendations to see the list.".to_string()
        })
}

fn recommended_mcp_text(presentation: &mut dyn CommandPresentationContext) -> String {
    let heading = presentation
        .translate("mcp_recommendations_heading", &[])
        .unwrap_or_else(|_| "Suggested MCP servers (nothing installs automatically)".to_string());
    let safety = presentation
        .translate(
            "mcp_recommendations_safety",
            &[("restart_command", "/mcp restart")],
        )
        .unwrap_or_else(|_| "Viewing this list adds or enables nothing.".to_string());
    let github = presentation
        .translate(
            "mcp_recommendation_github",
            &[
                ("endpoint", GITHUB_MCP_URL),
                ("login_command", "/mcp login github"),
                ("add_command", "/mcp add recommended github"),
            ],
        )
        .unwrap_or_else(|_| {
            format!(
                "• github — GitHub's official remote MCP endpoint\n  endpoint: {GITHUB_MCP_URL}"
            )
        });
    let chrome = presentation
        .translate(
            "mcp_recommendation_chrome",
            &[
                ("package", CHROME_DEVTOOLS_MCP_PACKAGE),
                ("launcher", "npx/npx.cmd"),
                ("restart_command", "/mcp restart"),
                ("add_command", "/mcp add recommended chrome-devtools"),
            ],
        )
        .unwrap_or_else(|_| {
            format!("• chrome-devtools — official Chrome DevTools MCP via pinned npm package\n  package: {CHROME_DEVTOOLS_MCP_PACKAGE}")
        });
    let playwright = presentation
        .translate(
            "mcp_recommendation_playwright",
            &[
                ("package", PLAYWRIGHT_MCP_PACKAGE),
                ("source", PLAYWRIGHT_MCP_SOURCE),
                ("launcher", "npx/npx.cmd"),
                ("restart_command", "/mcp restart"),
                ("add_command", "/mcp add recommended playwright"),
            ],
        )
        .unwrap_or_else(|_| {
            format!("• playwright — Microsoft's official Playwright MCP via pinned npm package\n  package: {PLAYWRIGHT_MCP_PACKAGE}")
        });
    let container_use = presentation
        .translate(
            "mcp_recommendation_container_use",
            &[
                ("source", CONTAINER_USE_SOURCE),
                ("restart_command", "/mcp restart"),
                ("add_command", "/mcp add recommended container-use"),
            ],
        )
        .unwrap_or_else(|_| format!("• container-use — Dagger's experimental container-use MCP\n  source: {CONTAINER_USE_SOURCE}"));
    format!(
        "{heading}\n\
         {safety}\n\
         \n\
         • hugging-face — remote Hugging Face MCP endpoint\n\
           provenance: bundled Codewhale recommendation\n\
           add explicitly: /mcp add recommended hugging-face\n\
           then inspect: /mcp doctor · reload all configured servers: /mcp restart\n\
         \n\
         {github}\n\
         \n\
         {chrome}\n\
         \n\
         {playwright}\n\
         \n\
         {container_use}\n\
         \n\
         External sources (~/.claude.json, .mcp.json, marketplace manifests):\n\
           /mcp import — list candidates with provenance (keyboard/mouse status)\n\
           /mcp import approve <name> — create managed connector after consent\n\
           /mcp import decline <name> — durable decline until source content changes\n\
         enabled=false is a hard block and will never import. Nothing is auto-imported."
    )
}

fn recommended_npx_command_for(windows: bool) -> &'static str {
    if windows { "npx.cmd" } else { "npx" }
}

fn parse_scopes(parts: Vec<&str>) -> Vec<String> {
    let mut scopes = Vec::new();
    let mut iter = parts.into_iter();
    while let Some(part) = iter.next() {
        if part == "--scope" {
            let Some(value) = iter.next() else {
                continue;
            };
            for scope in value.split(',') {
                let scope = scope.trim();
                if !scope.is_empty() {
                    scopes.push(scope.to_string());
                }
            }
            continue;
        }
        let value = part.strip_prefix("--scope=");
        let Some(value) = value else {
            for scope in part.split(',') {
                let scope = scope.trim();
                if !scope.is_empty() {
                    scopes.push(scope.to_string());
                }
            }
            continue;
        };
        for scope in value.split(',') {
            let scope = scope.trim();
            if !scope.is_empty() {
                scopes.push(scope.to_string());
            }
        }
    }
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakePresentation;
    impl CommandPresentationContext for FakePresentation {
        fn translate(&self, key: &str, r: &[(&str, &str)]) -> Result<String, String> {
            let mut out = match key {
                "mcp_recommended_unknown_id" => {
                    "Unknown MCP suggestion. Run {recommendations_command} to see the list.".to_string()
                }
                "mcp_recommendations_heading" => {
                    "Suggested MCP servers (nothing installs automatically)".to_string()
                }
                "mcp_recommendations_safety" => {
                    "Looking adds nothing. Adding writes config only — review it before {restart_command} connects anything."
                        .to_string()
                }
                "mcp_recommendation_github" => {
                    "• github — GitHub's official remote MCP endpoint\n  endpoint: {endpoint}\n  auth is separate: use {login_command} only when the server advertises OAuth;\n  otherwise configure a least-privilege PAT outside command history.\n  add: {add_command}".to_string()
                }
                "mcp_recommendation_chrome" => {
                    "• chrome-devtools — official Chrome DevTools MCP via pinned npm package\n  package: {package} ({launcher})\n  it can inspect/control Chrome and read authenticated pages.\n  add: {add_command}".to_string()
                }
                "mcp_recommendation_playwright" => {
                    "• playwright — Microsoft's official Playwright MCP via pinned npm package\n  package: {package} ({launcher})\n  source: {source}\n  --isolated starts a fresh browser profile. It can browse/control pages and read authenticated pages.\n  add: {add_command}".to_string()
                }
                "mcp_recommendation_container_use" => {
                    "• container-use — Dagger's experimental container-use MCP\n  command: container-use stdio\n  source: {source}\n  requires the separately installed container-use binary; Codewhale never downloads or installs this binary.\n  add: {add_command}".to_string()
                }
                _ => return Err("unknown translation key".to_string()),
            };
            for (name, value) in r {
                out = out.replace(&format!("{{{name}}}"), value);
            }
            Ok(out)
        }
    }

    #[test]
    fn parses_add_and_validate() {
        let add = mcp(
            &mut FakePresentation,
            Some("add stdio local node server.js"),
        );
        assert!(matches!(
            add.action,
            Some(AppAction::Mcp(McpUiAction::AddStdio { name, command, args }))
                if name == "local" && command == "node" && args == vec!["server.js".to_string()]
        ));

        let validate = mcp(&mut FakePresentation, Some("validate"));
        assert!(matches!(
            validate.action,
            Some(AppAction::Mcp(McpUiAction::Validate))
        ));

        let doctor = mcp(&mut FakePresentation, Some("doctor"));
        assert!(matches!(
            doctor.action,
            Some(AppAction::Mcp(McpUiAction::Validate))
        ));
        for command in ["validate github", "doctor github"] {
            assert!(matches!(
                mcp(&mut FakePresentation, Some(command)).action,
                Some(AppAction::Mcp(McpUiAction::Diagnose { name })) if name == "github"
            ));
        }
        let restart = mcp(&mut FakePresentation, Some("restart"));
        assert!(matches!(
            restart.action,
            Some(AppAction::Mcp(McpUiAction::Reload))
        ));

        let recommended = mcp(&mut FakePresentation, Some("recommendations"))
            .message
            .expect("recommendations text");
        assert!(recommended.contains("nothing installs automatically"));
        assert!(recommended.contains("provenance:"));
        assert!(recommended.contains("https://api.githubcopilot.com/mcp/"));
        assert!(recommended.contains("chrome-devtools-mcp@1.7.0"));
        assert!(recommended.contains("@playwright/mcp@0.0.79"));
        assert!(recommended.contains("https://github.com/microsoft/playwright-mcp"));
        assert!(recommended.contains("https://github.com/dagger/container-use"));
        assert!(!recommended.contains("https://github.com/trycua/cua"));
        assert!(!recommended.to_ascii_lowercase().contains("cua-driver"));
        assert!(recommended.contains("least-privilege PAT outside command history"));
        assert!(recommended.contains("read authenticated pages"));

        let unknown = mcp(&mut FakePresentation, Some("add recommended unknown"))
            .message
            .expect("localized unknown recommendation error");
        assert!(unknown.contains("Unknown MCP suggestion"), "{unknown}");

        let add_recommended = mcp(&mut FakePresentation, Some("add recommended hugging-face"));
        assert!(matches!(
            add_recommended.action,
            Some(AppAction::Mcp(McpUiAction::AddHttp { name, url, transport: None }))
                if name == "hugging-face" && url == "https://huggingface.co/mcp"
        ));

        let add_github = mcp(&mut FakePresentation, Some("add recommended github"));
        assert!(matches!(
            add_github.action,
            Some(AppAction::Mcp(McpUiAction::AddHttp { name, url, transport: None }))
                if name == "github" && url == GITHUB_MCP_URL
        ));

        let add_chrome = mcp(
            &mut FakePresentation,
            Some("add recommended chrome-devtools"),
        );
        assert!(matches!(
            add_chrome.action,
            Some(AppAction::Mcp(McpUiAction::AddStdio { name, command, args }))
                if name == "chrome-devtools"
                    && command == recommended_npx_command_for(cfg!(windows))
                    && args == vec!["-y".to_string(), CHROME_DEVTOOLS_MCP_PACKAGE.to_string()]
        ));

        let add_playwright = mcp(&mut FakePresentation, Some("add recommended playwright"));
        assert!(matches!(
            add_playwright.action,
            Some(AppAction::Mcp(McpUiAction::AddStdio { name, command, args }))
                if name == "playwright"
                    && command == recommended_npx_command_for(cfg!(windows))
                    && args == vec![
                        "-y".to_string(),
                        PLAYWRIGHT_MCP_PACKAGE.to_string(),
                        "--isolated".to_string(),
                    ]
        ));

        let add_container = mcp(&mut FakePresentation, Some("add recommended container-use"));
        assert!(matches!(
            add_container.action,
            Some(AppAction::Mcp(McpUiAction::AddStdio { name, command, args }))
                if name == "container-use"
                    && command == "container-use"
                    && args == vec!["stdio".to_string()]
        ));

        let add_cua = mcp(&mut FakePresentation, Some("add recommended cua"));
        assert!(
            add_cua.is_error,
            "Cua is not a Codewhale computer-use recommendation"
        );
        assert!(
            add_cua
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown MCP suggestion")),
            "{:?}",
            add_cua.message
        );

        let import_list = mcp(&mut FakePresentation, Some("import"));
        assert!(matches!(
            import_list.action,
            Some(AppAction::Mcp(McpUiAction::ImportList))
        ));
        let import_approve = mcp(&mut FakePresentation, Some("import approve local-tools"));
        assert!(matches!(
            import_approve.action,
            Some(AppAction::Mcp(McpUiAction::ImportApprove { name }))
                if name == "local-tools"
        ));
        let import_decline = mcp(&mut FakePresentation, Some("import decline local-tools"));
        assert!(matches!(
            import_decline.action,
            Some(AppAction::Mcp(McpUiAction::ImportDecline { name }))
                if name == "local-tools"
        ));
        let marketplace = mcp(&mut FakePresentation, Some("marketplace"));
        assert!(matches!(
            marketplace.action,
            Some(AppAction::Mcp(McpUiAction::ImportList))
        ));

        let login = mcp(
            &mut FakePresentation,
            Some("login remote --scope tools/read,tools/write"),
        );
        assert!(matches!(
            login.action,
            Some(AppAction::Mcp(McpUiAction::Login { name, scopes }))
                if name == "remote"
                    && scopes == vec!["tools/read".to_string(), "tools/write".to_string()]
        ));

        let retry = mcp(&mut FakePresentation, Some("retry remote"));
        assert!(matches!(
            retry.action,
            Some(AppAction::Mcp(McpUiAction::Retry { name })) if name == "remote"
        ));
    }

    #[test]
    fn recommended_chrome_launcher_is_native_on_unix_and_windows() {
        assert_eq!(recommended_npx_command_for(false), "npx");
        assert_eq!(recommended_npx_command_for(true), "npx.cmd");

        let windows = parse_add_for_platform(
            &mut FakePresentation,
            vec!["recommended", "playwright"],
            true,
        );
        assert!(matches!(
            windows.action,
            Some(AppAction::Mcp(McpUiAction::AddStdio { name, command, args }))
                if name == "playwright"
                    && command == "npx.cmd"
                    && args == vec![
                        "-y".to_string(),
                        PLAYWRIGHT_MCP_PACKAGE.to_string(),
                        "--isolated".to_string(),
                    ]
        ));
    }

    #[test]
    fn recommendations_state_execution_and_install_boundaries() {
        let text = recommended_mcp_text(&mut FakePresentation);
        assert!(text.contains("nothing installs automatically"));
        assert!(text.contains("Suggested MCP servers"));
        assert!(text.contains("never downloads or"));
        assert!(text.contains("installs this binary"));
        assert!(text.contains("experimental"));
        assert!(text.contains("--isolated"));
        assert!(!text.contains("operating-system permissions"));
        assert!(!text.to_ascii_lowercase().contains("cua"));
    }

    #[test]
    fn handler_is_contextual_and_requests_presentation_facet() {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = McpCmd::handler()
        else {
            panic!("mcp must be contextual");
        };
        assert_eq!(capabilities, CommandCapabilities::PRESENTATION);
        let missing = handler(CommandContexts::empty(), Some("list"));
        assert!(missing.is_error);
        assert_eq!(
            missing.message.as_deref(),
            Some("Error: Command capability unavailable: presentation")
        );
        assert_eq!(McpCmd::info().description_key, "cmd_mcp_description");
        assert_eq!(McpCmd::info().aliases, &["mcps"]);
    }
}
