//! `/hf` - Hugging Face MCP and provider concept helpers.

use codewhale_command_contract::config::CommandConfigContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "hf",
    aliases: &["huggingface"],
    usage: "/hf [mcp <status|setup>|concepts]",
    description_key: "cmd_hf_description",
};

pub struct HfCmd;

impl<A> RegisterCommand<CommandResult<A>> for HfCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::CONFIG,
            handler: hf,
        }
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

fn hf<A>(contexts: CommandContexts<'_>, args: Option<&str>) -> CommandResult<A> {
    let raw = args.unwrap_or("").trim();
    if raw.is_empty() {
        return usage();
    }

    let mut parts = raw.split_whitespace();
    let subcommand = parts.next().unwrap_or_default().to_ascii_lowercase();
    match subcommand.as_str() {
        "mcp" => match contexts.into_parts().config {
            Some(config) => hf_mcp(config, parts.next()),
            None => CommandResult::error("Command capability unavailable: config"),
        },
        "concepts" | "explain" => CommandResult::message(HF_CONCEPTS),
        _ => CommandResult::error(format!(
            "Unknown /hf subcommand: {subcommand}. Use /hf mcp <status|setup> or /hf concepts."
        )),
    }
}

fn usage<A>() -> CommandResult<A> {
    CommandResult::message(
        "Usage: /hf mcp <status|setup>\n\
         /hf concepts\n\n\
         Hugging Face MCP settings: https://huggingface.co/settings/mcp",
    )
}

fn hf_mcp<A>(config: &dyn CommandConfigContext, action: Option<&str>) -> CommandResult<A> {
    match action.unwrap_or("status").to_ascii_lowercase().as_str() {
        "status" => hf_mcp_status(config),
        "setup" => CommandResult::message(hf_mcp_setup_message(&config.mcp_config_path())),
        other => CommandResult::error(format!(
            "Unknown /hf mcp subcommand: {other}. Use status or setup."
        )),
    }
}

fn hf_mcp_status<A>(config: &dyn CommandConfigContext) -> CommandResult<A> {
    let path = config.mcp_config_path();
    match config.hugging_face_mcp_server_name() {
        Ok(server) => {
            if let Some(server_name) = server {
                CommandResult::message(format!(
                    "Hugging Face MCP appears configured as `{server_name}` in {}.\n\
                     Run /mcp reload to rebuild the live model-visible tool pool if tools are not visible yet.",
                    path.display()
                ))
            } else {
                CommandResult::message(format!(
                    "Hugging Face MCP is not configured in {}.\n\
                     Run /hf mcp setup for the settings-generated config workflow.",
                    path.display()
                ))
            }
        }
        Err(err) => CommandResult::error(format!(
            "Could not read MCP config {}: {err}",
            path.display()
        )),
    }
}

fn hf_mcp_setup_message(path: &std::path::Path) -> String {
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
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn hf_usage_and_setup_do_not_advertise_hub_search() {
        let usage = usage::<()>().message.expect("usage");
        let setup = hf_mcp_setup_message(std::path::Path::new("mcp.json"));

        assert!(!usage.contains("/hf search"));
        assert!(!setup.contains("/hf search"));
        assert!(setup.contains(HF_MCP_SETTINGS_URL));
    }
}
