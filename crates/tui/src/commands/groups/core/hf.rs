//! `/hf` - Hugging Face MCP and provider concept helpers.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::mcp::{McpConfig, McpServerConfig};
use crate::tui::app::App;
use codewhale_localization::MessageId;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "hf",
    aliases: &["huggingface"],
    usage: "/hf [mcp <status|setup>|concepts]",
    description_id: MessageId::CmdHfDescription,
};

pub(in crate::commands) struct HfCmd;

impl RegisterCommand for HfCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        hf(app, arg)
    }
}

const HF_MCP_SETTINGS_URL: &str = "https://huggingface.co/settings/mcp";
const HF_MCP_DOCS_URL: &str = "https://huggingface.co/docs/hub/hf-mcp-server";
const HF_MCP_SERVER_URL: &str = "https://huggingface.co/mcp";

const HF_MCP_CONFIG_SKELETON: &str = r#"{
  "servers": {
    "huggingface": {
      "url": "https://huggingface.co/mcp",
      "headers": {
        "Authorization": "Bearer ${HF_TOKEN}"
      }
    }
  }
}"#;

/// Explainer shown by `/hf concepts`.
const HF_CONCEPTS: &str = "\
Codewhale has three distinct Hugging Face surfaces:

1. Hugging Face provider route - chat inference
   Switch the active LLM backend to Hugging Face Inference Providers.
   Use: /provider huggingface
   Config: provider = \"huggingface\" or [providers.huggingface]
   Auth: HF_TOKEN or HUGGINGFACE_API_KEY

2. Hugging Face MCP - Hub, docs, datasets, Spaces, and community tools
   Connect Codewhale to Hugging Face's MCP server through mcp.json.
   Use: /hf mcp status or /hf mcp setup
   Then: /mcp reload or restart Codewhale so model-visible tools reload.

3. Hugging Face Hub workflows - publish, upload, or manage repositories
   Use explicit Hub tooling such as huggingface_hub or git-based flows.
   Codewhale does not upload to the Hub through /hf.";

pub fn hf(app: &mut App, args: Option<&str>) -> CommandResult {
    let raw = args.unwrap_or("").trim();
    if raw.is_empty() {
        return usage();
    }

    let mut parts = raw.split_whitespace();
    let subcommand = parts.next().unwrap_or_default().to_ascii_lowercase();
    match subcommand.as_str() {
        "mcp" => hf_mcp(app, parts.next()),
        "concepts" | "explain" => CommandResult::message(HF_CONCEPTS),
        _ => CommandResult::error(format!(
            "Unknown /hf subcommand: {subcommand}. Use /hf mcp <status|setup> or /hf concepts."
        )),
    }
}

fn usage() -> CommandResult {
    CommandResult::message(
        "Usage: /hf mcp <status|setup>\n\
         /hf concepts\n\n\
         Hugging Face MCP settings: https://huggingface.co/settings/mcp",
    )
}

fn hf_mcp(app: &mut App, action: Option<&str>) -> CommandResult {
    match action.unwrap_or("status").to_ascii_lowercase().as_str() {
        "status" => hf_mcp_status(app),
        "setup" => CommandResult::message(hf_mcp_setup_message(app)),
        other => CommandResult::error(format!(
            "Unknown /hf mcp subcommand: {other}. Use status or setup."
        )),
    }
}

fn hf_mcp_status(app: &App) -> CommandResult {
    match crate::mcp::load_config(&app.mcp_config_path) {
        Ok(config) => {
            if let Some(server_name) = configured_hf_mcp_server(&config) {
                CommandResult::message(format!(
                    "Hugging Face MCP appears configured as `{server_name}` in {}.\n\
                     Run /mcp reload to rebuild the live model-visible tool pool if tools are not visible yet.",
                    app.mcp_config_path.display()
                ))
            } else {
                CommandResult::message(format!(
                    "Hugging Face MCP is not configured in {}.\n\
                     Run /hf mcp setup for the settings-generated config workflow.",
                    app.mcp_config_path.display()
                ))
            }
        }
        Err(err) => CommandResult::error(format!(
            "Could not read MCP config {}: {err}",
            app.mcp_config_path.display()
        )),
    }
}

fn hf_mcp_setup_message(app: &App) -> String {
    format!(
        "Use Hugging Face's settings-generated MCP configuration when available:\n\
         1. Open {HF_MCP_SETTINGS_URL} while signed in.\n\
         2. Choose your MCP client and copy the generated configuration snippet.\n\
         3. Paste the Hugging Face server entry into {}.\n\
         4. Run /mcp reload to rebuild the live model-visible tool pool.\n\n\
         Codewhale-compatible placeholder shape:\n\n\
         ```json\n{HF_MCP_CONFIG_SKELETON}\n```\n\n\
         The placeholder is intentionally not runnable until your private MCP config has a real token value. \
         Do not commit real Hugging Face tokens.\n\n\
         Docs: {HF_MCP_DOCS_URL}\n\
         Server: {HF_MCP_SERVER_URL}",
        app.mcp_config_path.display()
    )
}

fn configured_hf_mcp_server(config: &McpConfig) -> Option<&str> {
    config
        .servers
        .iter()
        .find(|(name, server)| looks_like_hf_mcp_server(name, server))
        .map(|(name, _)| name.as_str())
}

fn looks_like_hf_mcp_server(name: &str, server: &McpServerConfig) -> bool {
    let compact_name: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect();
    if matches!(
        compact_name.as_str(),
        "huggingface" | "huggingfacemcp" | "hfmcp" | "hfmcpserver"
    ) {
        return true;
    }

    server.url.as_deref().is_some_and(|url| {
        let url = url.to_ascii_lowercase();
        url.contains("huggingface.co/mcp") || url.contains("huggingface.co/api/mcp")
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use tempfile::tempdir;

    use super::*;

    fn app_with_mcp_path(mcp_config_path: PathBuf) -> App {
        App::new(
            TuiOptions {
                screen_mode: crate::tui::app::ScreenMode::Inline,
                max_subagents: 2,
                mcp_config_path,
                ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
            },
            &Config::default(),
        )
    }

    #[test]
    fn hf_mcp_config_skeleton_keeps_token_placeholder_only() {
        assert!(HF_MCP_CONFIG_SKELETON.contains("${HF_TOKEN}"));
        assert!(!HF_MCP_CONFIG_SKELETON.contains("hf_"));
        assert!(!HF_MCP_CONFIG_SKELETON.contains("Bearer hf_"));
        serde_json::from_str::<serde_json::Value>(HF_MCP_CONFIG_SKELETON)
            .expect("skeleton should be valid JSON");
    }

    #[test]
    fn hf_concepts_explains_provider_mcp_and_hub_surfaces() {
        assert!(HF_CONCEPTS.contains("provider route"));
        assert!(HF_CONCEPTS.contains("Hugging Face MCP"));
        assert!(HF_CONCEPTS.contains("Hub workflows"));
        assert!(HF_CONCEPTS.contains("/provider huggingface"));
        assert!(HF_CONCEPTS.contains("/hf mcp"));
    }

    #[test]
    fn hf_mcp_status_detects_settings_named_server() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        fs::write(
            &path,
            r#"{"mcpServers":{"hf-mcp-server":{"url":"https://huggingface.co/mcp"}}}"#,
        )
        .expect("write mcp config");
        let app = app_with_mcp_path(path);

        let result = hf_mcp_status(&app);

        assert!(!result.is_error);
        let message = result.message.expect("status message");
        assert!(message.contains("appears configured"));
        assert!(message.contains("hf-mcp-server"));
    }

    #[test]
    fn hf_mcp_status_reports_missing_server_without_network() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        fs::write(&path, r#"{"servers":{"local":{"command":"node"}}}"#).expect("write mcp config");
        let app = app_with_mcp_path(path);

        let result = hf_mcp_status(&app);

        assert!(!result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("not configured")
        );
    }

    #[test]
    fn hf_usage_and_setup_do_not_advertise_hub_search() {
        let app = app_with_mcp_path(PathBuf::from("mcp.json"));
        let usage = usage().message.expect("usage");
        let setup = hf_mcp_setup_message(&app);

        assert!(!usage.contains("/hf search"));
        assert!(!setup.contains("/hf search"));
        assert!(setup.contains(HF_MCP_SETTINGS_URL));
    }
}
