//! Runtime MCP server management.
//!
//! Provides `StartRuntimeMcpServer` — the entry tool for LLM to dynamically
//! connect to MCP servers from conversation context. Also contains parsing
//! and naming helpers used by the tool.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::mcp::{McpPool, McpServerConfig};
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

// === Parsing Functions ===

#[derive(Debug, Clone)]
pub struct ParsedMcpServer {
    pub name: String,
    pub config: McpServerConfig,
}

/// Parse a command string or URL into an MCP server configuration.
///
/// - Local command: `npx @modelcontextprotocol/server-filesystem /tmp`
/// - Remote URL: `https://huggingface.co/mcp`
pub fn parse_mcp_command(input: &str) -> Result<ParsedMcpServer> {
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!("MCP command cannot be empty");
    }

    if input.starts_with("http://") || input.starts_with("https://") {
        let name = extract_name_from_url(input)?;
        return Ok(ParsedMcpServer {
            name,
            config: McpServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
                url: Some(input.to_string()),
                transport: None,
                connect_timeout: None,
                execute_timeout: None,
                read_timeout: None,
                disabled: false,
                enabled: true,
                required: false,
                lazy: false,
                enabled_tools: Vec::new(),
                disabled_tools: Vec::new(),
                headers: HashMap::new(),
                env_headers: HashMap::new(),
                bearer_token_env_var: None,
                scopes: Vec::new(),
                oauth: None,
                oauth_resource: None,
                reviewed_plugin: None,
                runtime_added: false,
                allow_private_network: false,
            },
        });
    }

    let parts: Vec<String> = shell_words::split(input).unwrap_or_default();
    if parts.is_empty() {
        anyhow::bail!("MCP command cannot be empty");
    }

    let command = parts[0].clone();
    let args: Vec<String> = parts[1..].to_vec();
    let name = infer_server_name(&command, &args)?;

    Ok(ParsedMcpServer {
        name,
        config: McpServerConfig {
            command: Some(command),
            args,
            env: HashMap::new(),
            cwd: None,
            url: None,
            transport: None,
            connect_timeout: None,
            execute_timeout: None,
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            lazy: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
            env_headers: HashMap::new(),
            bearer_token_env_var: None,
            scopes: Vec::new(),
            oauth: None,
            oauth_resource: None,
            reviewed_plugin: None,
            runtime_added: false,
            allow_private_network: false,
        },
    })
}

pub fn extract_name_from_url(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("remote");
    let path = parsed.path().trim_matches('/');

    // Replace dots with dashes in hostname for better readability
    let host_part = host.replace('.', "-");

    // Combine host and path, replacing slashes with underscores
    let name = if path.is_empty() {
        host_part
    } else {
        format!("{}_{}", host_part, path.replace('/', "_"))
    };

    Ok(sanitize_name(&name))
}

fn infer_server_name(command: &str, args: &[String]) -> Result<String> {
    let cmd_path = std::path::Path::new(command);
    let cmd_base = cmd_path.file_stem().unwrap_or_default().to_string_lossy();

    // Windows cmd /c prefix: skip "cmd /c" and recurse on the remaining args
    // e.g. ["cmd", "/c", "npx", "-y", "@modelcontextprotocol/server-memory"]
    if cmd_base.as_ref() == "cmd"
        && args.len() >= 2
        && (args[0] == "/c" || args[0] == "/C" || args[0] == "/k" || args[0] == "/K")
    {
        let inner_cmd = &args[1];
        let inner_args: Vec<String> = args[2..].to_vec();
        return infer_server_name(inner_cmd, &inner_args);
    }

    // Package managers: extract the package name (first non-flag arg)
    if matches!(
        cmd_base.as_ref(),
        "npx" | "npm" | "pnpm" | "yarn" | "bunx" | "bun"
    ) {
        for arg in args {
            if !arg.starts_with('-') && arg != "exec" && arg != "run" && arg != "start" {
                // e.g. "@modelcontextprotocol/server-filesystem" → "filesystem"
                if let Some(name) = arg.split('/').next_back() {
                    if let Some(short) = name.strip_prefix("server-") {
                        return Ok(sanitize_name(short));
                    }
                    return Ok(sanitize_name(name));
                }
            }
        }
    }

    // Script interpreters: extract the script path (first non-flag arg)
    if matches!(
        cmd_base.as_ref(),
        "node" | "python" | "python3" | "uvx" | "uv" | "ruby" | "deno"
    ) && let Some(script) = args.iter().find(|a| !a.starts_with('-'))
    {
        let script_path = std::path::Path::new(script);
        if let Some(stem) = script_path.file_stem() {
            return Ok(sanitize_name(&stem.to_string_lossy()));
        }
    }

    // Fallback: first non-flag argument (script or file)
    if let Some(script) = args.iter().find(|a| !a.starts_with('-')) {
        let script_path = std::path::Path::new(script);
        if let Some(stem) = script_path.file_stem() {
            return Ok(sanitize_name(&stem.to_string_lossy()));
        }
    }

    // Last resort: command name itself
    Ok(sanitize_name(&cmd_base))
}

pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// === Tool: StartRuntimeMcpServer ===

/// Entry tool for dynamically adding MCP servers from conversation context.
///
/// LLM calls this to start a local MCP server (stdio) or connect to a remote
/// one (HTTP). The server config is added to `McpPool.dynamic_servers` and
/// tools are discovered via the existing `McpConnection` / `StdioTransport` flow.
pub struct StartRuntimeMcpServer {
    pool: Arc<AsyncMutex<McpPool>>,
}

impl StartRuntimeMcpServer {
    pub fn new(pool: Arc<AsyncMutex<McpPool>>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ToolSpec for StartRuntimeMcpServer {
    fn name(&self) -> &str {
        "start_mcp_server"
    }

    fn description(&self) -> &str {
        "When a user provides an MCP server command (like 'npx ...') or URL \
         (like 'https://...'), call this tool immediately to start the server \
         and register its tools. Do NOT suggest editing config files. \
         Accepts a local command (stdio) or a remote URL (HTTP/SSE). \
         To reconnect an existing configured server after login, pass only its exact name \
         and omit server; this keeps its saved credentials and configuration. \
         After the server starts, the response lists each tool's callable name. \
         You MUST copy those exact names when calling the tools. \
         Do NOT construct or guess tool names yourself."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "New MCP server command or URL; omit to reconnect a configured server by name"
                },
                "name": {
                    "type": "string",
                    "description": "Exact configured name for reconnect; optional name for a new server"
                }
            },
            "anyOf": [{"required": ["server"]}, {"required": ["name"]}]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network, ToolCapability::ExecutesCode]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let custom_name = input.get("name").and_then(|v| v.as_str());
        if input.get("server").is_none() {
            let name = custom_name
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    ToolError::invalid_input("Provide server or an existing configured name")
                })?;
            // The exact configured key owns its credentials and trust. Do not
            // sanitize it into an alias, replace its config, or reconnect siblings.
            if McpPool::server_denied_by(&context.disallowed_tools, name) {
                return Err(ToolError::not_available(format!(
                    "Failed to find MCP server: {name}"
                )));
            }
            let mut pool = self.pool.lock().await;
            let conn = pool.retry_connection(name).await.map_err(|error| {
                ToolError::execution_failed(connect_failure_message(name, &error))
            })?;
            let transport = if conn.config().url.is_some() {
                "http"
            } else {
                "stdio"
            };
            return Ok(connected_result(&pool, name, transport, context));
        }
        let server = input
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_input("server must be a command or URL string"))?;
        let mut parsed =
            parse_mcp_command(server).map_err(|e| ToolError::invalid_input(e.to_string()))?;
        // Host-supplied override (used by the Registry launcher, whose
        // packages cold-start via npx/uvx downloads). Not exposed on the
        // model-facing schema, so the model cannot widen its own timeouts.
        if let Some(timeout) = input.get("connect_timeout").and_then(Value::as_u64) {
            parsed.config.connect_timeout = Some(timeout);
        }

        // Refuse shell wrappers and anything outside the runtime allowlist.
        if let Some(ref cmd) = parsed.config.command {
            validate_runtime_command(cmd)?;
        }

        // Reject shell metacharacters in arguments to prevent injection.
        // Extracted to `reject_shell_metacharacters` so it is reachable from
        // tests: the `reject_metachar_*` tests used to assert only that their
        // own input string contained the metacharacter and never that this
        // guard refused it, so deleting the guard left them green
        // (2026-08-04 audit).
        reject_shell_metacharacters(&parsed.config.args)?;

        let server_name = custom_name
            .map(sanitize_name)
            .unwrap_or(parsed.name)
            .replace('_', "-");

        // Underscores in server names would cause tool name collision.
        // Tool names are formatted as mcp_{server}_{tool}; underscores in
        // server names would make it ambiguous (server "foo" + tool "bar_x"
        // vs server "foo_bar" + tool "x" both → mcp_foo_bar_x).
        // sanitize_name already converts non-alphanumeric chars to hyphens,
        // but underscores from the original input need explicit conversion.

        let transport = if parsed.config.url.is_some() {
            "http"
        } else {
            "stdio"
        };

        // Ancestor restrictions stay request-local because children share the pool.
        if McpPool::server_denied_by(&context.disallowed_tools, &server_name) {
            return Err(ToolError::not_available(format!(
                "Failed to find MCP server: {server_name}"
            )));
        }
        // Register server config, connect, and collect tool info
        let mut pool = self.pool.lock().await;
        pool.add_runtime_server_config(server_name.clone(), parsed.config)
            .map_err(ToolError::invalid_input)?;
        let conn = match pool.get_or_connect(&server_name).await {
            Ok(conn) => conn,
            Err(error) => {
                let message = connect_failure_message(&server_name, &error);
                pool.remove_runtime_server_config(&server_name);
                return Err(ToolError::execution_failed(message));
            }
        };

        let _ = conn;
        Ok(connected_result(&pool, &server_name, transport, context))
    }
}

/// Shared receipt for both new servers and configured-name reconnects.
fn connected_result(
    pool: &McpPool,
    server_name: &str,
    transport: &str,
    context: &ToolContext,
) -> ToolResult {
    let owners = pool.resolved_tool_servers();
    let tools_list: Vec<String> = pool
        .all_tools()
        .into_iter()
        .filter(|(name, _)| {
            owners.get(name).map(String::as_str) == Some(server_name)
                && !crate::core::engine::tool_catalog::tool_matches_any_rule(
                    &context.disallowed_tools,
                    name,
                )
        })
        .map(|(name, tool)| {
            format!(
                "- {} → {}",
                name,
                tool.description.as_deref().unwrap_or("no description")
            )
        })
        .collect();
    let result = serde_json::to_string(&json!({
        "status": "connected",
        "transport": transport,
        "server": server_name,
        "new_tools": tools_list.len(),
        "total_mcp_tools": pool.all_tools().iter().filter(|(name, _)| !crate::core::engine::tool_catalog::tool_matches_any_rule(&context.disallowed_tools, name)).count(),
        "message": format!(
            "MCP server '{}' connected via {}. {} tools discovered.\n\nCallable tools (use these exact names):\n{}",
            server_name, transport, tools_list.len(), tools_list.join("\n")
        )
    })).unwrap_or_else(|_| "{}".to_string());
    let mut output = ToolResult::success(result);
    output.metadata = Some(json!({ "mcp_catalog_changed": true }));
    output
}

/// Shell interpreters that would run an arbitrary script as the "server".
const SHELL_WRAPPERS: &[&str] = &["bash", "sh", "zsh", "cmd", "powershell"];

/// Known MCP server runtimes and package managers. Anything else is refused
/// to prevent arbitrary execution.
const ALLOWED_COMMANDS: &[&str] = &[
    "npx", "npm", "pnpm", "yarn", "bunx", "bun", "node", "python", "python3", "uvx", "uv", "deno",
    "ruby", "cargo",
];

/// Refuse a runtime MCP command that is a shell wrapper or is not a known
/// server runtime. Both checks read the command's lowercased file stem, so
/// `/bin/zsh` and `powershell.exe` are named for what they are. Kept out of
/// `execute` (as [`reject_shell_metacharacters`] is) so tests exercise the
/// real guard rather than a copy of its list.
fn validate_runtime_command(cmd: &str) -> Result<(), ToolError> {
    let cmd_base = std::path::Path::new(cmd)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if SHELL_WRAPPERS.contains(&cmd_base.as_str()) {
        return Err(ToolError::invalid_input(format!(
            "Shell wrapper commands ({cmd}) are not allowed. \
             Provide the actual MCP server command directly, \
             e.g. 'npx @modelcontextprotocol/server-filesystem /tmp'"
        )));
    }
    if !ALLOWED_COMMANDS.contains(&cmd_base.as_str()) {
        return Err(ToolError::invalid_input(format!(
            "Command '{cmd}' is not in the allowed list. \
             Permitted commands: {}",
            ALLOWED_COMMANDS.join(", ")
        )));
    }
    Ok(())
}

/// Refuse MCP server arguments carrying shell metacharacters.
///
/// Redirects (`>`), pipes (`|`), chaining (`;`, `&`), subshells (`` ` ``), and
/// variable expansion (`$`) are all dangerous in an argv that may reach a
/// shell. Kept as a free function rather than inline in `execute` so it is
/// directly testable: the `reject_metachar_*` tests previously asserted only
/// that their own input contained the metacharacter, so deleting the guard
/// left every one of them green (2026-08-04 audit).
fn reject_shell_metacharacters(args: &[String]) -> Result<(), ToolError> {
    for arg in args {
        if arg.contains(['>', '|', ';', '&', '`', '$']) {
            return Err(ToolError::invalid_input(format!(
                "Argument contains shell metacharacters: '{arg}'. \
                 MCP server arguments must not contain redirects, pipes, \
                 command chaining, or variable expansion."
            )));
        }
    }
    Ok(())
}

/// Build the connect-failure message returned to the model. A spawned
/// package that prints its CLI help and exits (the classic
/// missing-subcommand case, e.g. `npx -y agentic-mermaid@0.1.2` without
/// `mcp`) surfaces as `Stdio transport closed` before the handshake
/// completes — a bare transport error gives the model no signal about
/// *why*, and it tends to abandon the MCP route after one failed server.
/// Classify that early-exit shape, note when the captured output looks
/// like usage help, and point recovery at the registry: verify the exact
/// structured arguments returned by `registry_sync`, then fall through to
/// the next candidate from the search results instead of giving up.
fn connect_failure_message(server_name: &str, err: &anyhow::Error) -> String {
    let text = format!("{err:#}");
    let base = format!("Failed to connect to MCP server '{server_name}': {text}");
    let early_exit =
        text.contains("Stdio transport closed") || text.contains("Stdio transport read error");
    if !early_exit {
        return base;
    }
    let looks_like_help = text.contains("usage")
        || text.contains("Usage")
        || text.contains("--help")
        || text.contains("Commands:");
    let help_note = if looks_like_help {
        " Its output above looks like CLI usage help."
    } else {
        ""
    };
    format!(
        "{base}\n\nThe server process exited before completing the MCP handshake.{help_note} The launch arguments are usually incomplete in this case (missing subcommand or required argument). For Registry-discovered servers, verify the structured required_args returned by registry_sync and retry; if this server still will not start, try the next candidate from the Registry catalog."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mcp_ceiling_runtime_registration_respects_child_policy_before_connection() {
        let directory = tempfile::tempdir().unwrap();
        let pool = Arc::new(AsyncMutex::new(McpPool::new(
            crate::mcp::McpConfig::default(),
        )));
        let tool = StartRuntimeMcpServer::new(Arc::clone(&pool));
        let mut context = ToolContext::new(directory.path());
        context.disallowed_tools = vec!["mcp_private-*".to_string()];
        // The command would execute if the child ceiling were ignored.
        let error = tool
            .execute(
                json!({"server":"node nonexistent-mcp.js", "name":"private_a"}),
                &context,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to find MCP server: private-a")
        );
        assert!(pool.lock().await.server_names().is_empty());
        assert!(pool.lock().await.connected_servers().is_empty());
    }

    #[test]
    fn parse_command_stdio() {
        let parsed = parse_mcp_command("npx @modelcontextprotocol/server-filesystem /tmp").unwrap();
        assert!(parsed.config.command.is_some());
        assert!(parsed.config.url.is_none());
    }

    #[test]
    fn parse_command_url() {
        let parsed = parse_mcp_command("https://huggingface.co/mcp").unwrap();
        assert!(parsed.config.command.is_none());
        assert!(parsed.config.url.is_some());
        assert_eq!(parsed.name, "huggingface-co-mcp");
    }

    #[test]
    fn parse_command_url_with_subdomain() {
        let parsed = parse_mcp_command("https://api.example.com/mcp").unwrap();
        assert!(parsed.config.command.is_none());
        assert!(parsed.config.url.is_some());
        assert_eq!(parsed.name, "api-example-com-mcp");
    }

    #[test]
    fn parse_command_empty() {
        assert!(parse_mcp_command("").is_err());
        assert!(parse_mcp_command("   ").is_err());
    }

    #[test]
    fn extract_name_from_url_with_path() {
        assert_eq!(
            extract_name_from_url("https://huggingface.co/mcp").unwrap(),
            "huggingface-co-mcp"
        );
    }

    #[test]
    fn extract_name_from_url_with_subdomain() {
        assert_eq!(
            extract_name_from_url("https://api.example.com/mcp").unwrap(),
            "api-example-com-mcp"
        );
    }

    #[test]
    fn extract_name_from_url_no_path() {
        assert_eq!(
            extract_name_from_url("https://example.com").unwrap(),
            "example-com"
        );
    }

    #[test]
    fn extract_name_from_url_empty_path() {
        assert_eq!(
            extract_name_from_url("https://example.com/").unwrap(),
            "example-com"
        );
    }

    #[test]
    fn connect_failure_message_flags_early_exit_with_help_output() {
        let err = anyhow::anyhow!(
            "Stdio transport closed\nMCP server stderr (last 2 lines):\nUsage: agentic-mermaid [OPTIONS] <COMMAND>"
        );
        let msg = connect_failure_message("agentic-mermaid", &err);
        assert!(msg.contains("Failed to connect to MCP server 'agentic-mermaid'"));
        assert!(msg.contains("exited before completing the MCP handshake"));
        assert!(msg.contains("looks like CLI usage help"));
        assert!(msg.contains("required_args"));
        assert!(msg.contains("next candidate"));
    }

    #[test]
    fn connect_failure_message_flags_early_exit_without_help_output() {
        let err = anyhow::anyhow!("Stdio transport closed");
        let msg = connect_failure_message("x", &err);
        assert!(msg.contains("exited before completing the MCP handshake"));
        assert!(!msg.contains("usage help"));
        assert!(msg.contains("required_args"));
    }

    #[test]
    fn connect_failure_message_passes_other_errors_through() {
        let err = anyhow::anyhow!("connection refused");
        let msg = connect_failure_message("x", &err);
        assert_eq!(
            msg,
            "Failed to connect to MCP server 'x': connection refused"
        );
    }

    // === shell_words split tests ===

    #[test]
    fn shell_words_simple() {
        assert_eq!(
            shell_words::split("npx server /tmp").unwrap(),
            vec!["npx", "server", "/tmp"]
        );
    }

    #[test]
    fn shell_words_double_quotes() {
        assert_eq!(
            shell_words::split(r#"npx server --env="MY KEY""#).unwrap(),
            vec!["npx", "server", "--env=MY KEY"]
        );
    }

    #[test]
    fn shell_words_single_quotes() {
        assert_eq!(
            shell_words::split("npx server --env='MY KEY'").unwrap(),
            vec!["npx", "server", "--env=MY KEY"]
        );
    }

    #[test]
    fn shell_words_mixed_quotes() {
        assert_eq!(
            shell_words::split(r#"cmd --opt="hello world" --flag 'single'"#).unwrap(),
            vec!["cmd", "--opt=hello world", "--flag", "single"]
        );
    }

    #[test]
    fn shell_words_escaped_quote() {
        assert_eq!(
            shell_words::split(r#"cmd arg\"with\"quotes"#).unwrap(),
            vec!["cmd", r#"arg"with"quotes"#]
        );
    }

    #[test]
    fn shell_words_empty() {
        assert!(shell_words::split("").unwrap().is_empty());
        assert!(shell_words::split("   ").unwrap().is_empty());
    }

    #[test]
    fn shell_words_postgres_url() {
        assert_eq!(
            shell_words::split(
                r#"npx -y @modelcontextprotocol/server-postgres "postgresql://user:pass@host/db""#
            )
            .unwrap(),
            vec![
                "npx",
                "-y",
                "@modelcontextprotocol/server-postgres",
                "postgresql://user:pass@host/db"
            ]
        );
    }

    #[test]
    fn parse_command_with_quoted_args() {
        let parsed =
            parse_mcp_command(r#"npx @modelcontextprotocol/server-filesystem /tmp --env="MY KEY""#)
                .unwrap();
        assert_eq!(parsed.config.command, Some("npx".to_string()));
        assert_eq!(
            parsed.config.args,
            vec![
                "@modelcontextprotocol/server-filesystem",
                "/tmp",
                "--env=MY KEY"
            ]
        );
    }

    // === infer_server_name tests ===

    #[test]
    fn infer_name_npx_package() {
        let parsed = parse_mcp_command("npx @modelcontextprotocol/server-filesystem /tmp").unwrap();
        assert_eq!(parsed.name, "filesystem");
    }

    #[test]
    fn infer_name_npx_simple() {
        let parsed = parse_mcp_command("npx my-mcp-server").unwrap();
        assert_eq!(parsed.name, "my-mcp-server");
    }

    #[test]
    fn infer_name_pnpm_exec() {
        let parsed = parse_mcp_command("pnpm exec @modelcontextprotocol/server-postgres").unwrap();
        assert_eq!(parsed.name, "postgres");
    }

    #[test]
    fn infer_name_node_script() {
        let parsed = parse_mcp_command("node ./my-mcp-server.js").unwrap();
        assert_eq!(parsed.name, "my-mcp-server");
    }

    #[test]
    fn infer_name_python_script() {
        let parsed = parse_mcp_command("python3 mcp_server.py").unwrap();
        assert_eq!(parsed.name, "mcp-server");
    }

    #[test]
    fn infer_name_uvx_package() {
        let parsed = parse_mcp_command("uvx mcp-server-git").unwrap();
        assert_eq!(parsed.name, "mcp-server-git");
    }

    #[test]
    fn infer_name_bare_command() {
        let parsed = parse_mcp_command("/usr/local/bin/my-server").unwrap();
        assert_eq!(parsed.name, "my-server");
    }

    #[test]
    fn infer_name_windows_cmd_prefix() {
        let parsed =
            parse_mcp_command("cmd /c npx -y @modelcontextprotocol/server-memory").unwrap();
        assert_eq!(parsed.name, "memory");
    }

    #[test]
    fn infer_name_windows_cmd_uppercase() {
        let parsed =
            parse_mcp_command("cmd /C npx @modelcontextprotocol/server-filesystem /tmp").unwrap();
        assert_eq!(parsed.name, "filesystem");
    }

    #[test]
    fn infer_name_only_command_no_args() {
        // No args at all — falls through to last resort: command name itself
        let parsed = parse_mcp_command("my-server").unwrap();
        assert_eq!(parsed.name, "my-server");
    }

    #[test]
    fn infer_name_only_command_no_args_path() {
        // Absolute path, no args — uses file_stem of command
        let parsed = parse_mcp_command("/usr/local/bin/my-server").unwrap();
        assert_eq!(parsed.name, "my-server");
    }

    // === sanitize_name tests ===

    #[test]
    fn sanitize_name_preserves_hyphens() {
        assert_eq!(sanitize_name("my-server"), "my-server");
    }

    #[test]
    fn sanitize_name_converts_underscores_to_hyphens() {
        assert_eq!(sanitize_name("my_server"), "my-server");
    }

    #[test]
    fn sanitize_name_converts_special_chars_to_hyphens() {
        assert_eq!(sanitize_name("my@server!"), "my-server");
    }

    #[test]
    fn sanitize_name_trims_leading_trailing_hyphens() {
        assert_eq!(sanitize_name("_my_server_"), "my-server");
    }

    #[test]
    fn sanitize_name_preserves_alphanumeric() {
        assert_eq!(sanitize_name("server123"), "server123");
    }

    #[test]
    fn sanitize_name_empty_input() {
        assert_eq!(sanitize_name(""), "");
    }

    // === command validation tests ===

    #[test]
    fn shell_wrappers_and_unlisted_commands_are_refused() {
        for (cmd, reason) in [
            ("bash", "Shell wrapper"),
            ("sh", "Shell wrapper"),
            ("/bin/zsh", "Shell wrapper"),
            ("powershell", "Shell wrapper"),
            ("PowerShell.exe", "Shell wrapper"),
            ("curl", "not in the allowed list"),
        ] {
            let err =
                super::validate_runtime_command(cmd).expect_err(&format!("{cmd} must be refused"));
            assert!(err.to_string().contains(reason), "{cmd}: {err}");
        }
        // The parsed command, not the raw string, is what gets checked.
        let parsed = parse_mcp_command("bash -c 'npx server'").expect("parses");
        let cmd = parsed.config.command.expect("stdio command");
        assert!(super::validate_runtime_command(&cmd).is_err());
    }

    #[test]
    fn known_runtimes_pass_the_command_guard() {
        for cmd in ["npx", "/usr/local/bin/node", "python3", "uvx", "cargo"] {
            assert!(super::validate_runtime_command(cmd).is_ok(), "{cmd}");
        }
    }

    /// These used to assert only that their own input string contained the
    /// metacharacter — never that the guard refused it — so deleting the
    /// defense left all four green (2026-08-04 audit). They now call the
    /// guard.
    #[test]
    fn shell_metacharacters_in_args_are_refused() {
        for bad in [
            "--out>file",
            "arg|cat",
            "a;rm -rf /",
            "a&&b",
            "`whoami`",
            "$HOME",
        ] {
            let args = vec!["server".to_string(), bad.to_string()];
            let err = super::reject_shell_metacharacters(&args)
                .expect_err("metacharacter must be refused: {bad}");
            assert!(
                err.to_string().contains("shell metacharacters"),
                "refusal must name the reason for {bad}: {err}"
            );
        }
    }

    #[test]
    fn ordinary_args_pass_the_metacharacter_guard() {
        let args = vec![
            "@modelcontextprotocol/server-filesystem".to_string(),
            "/tmp/workspace".to_string(),
            "--read-only".to_string(),
        ];
        assert!(super::reject_shell_metacharacters(&args).is_ok());
    }

    // === approval-gate contract ===

    #[test]
    fn start_mcp_server_declares_required_approval() {
        // Security invariant (#3866): spawning a runtime MCP server is
        // side-effecting (child process / network connection), so the tool
        // spec itself must declare `ApprovalRequirement::Required`. Combined
        // with the engine's non-bypassable gate (see engine tests), this
        // guarantees an unapproved start is rejected before `execute` runs.
        let pool = Arc::new(AsyncMutex::new(McpPool::new(
            crate::mcp::McpConfig::default(),
        )));
        let tool = StartRuntimeMcpServer::new(pool);
        assert_eq!(tool.name(), "start_mcp_server");
        assert!(
            matches!(tool.approval_requirement(), ApprovalRequirement::Required),
            "start_mcp_server must require approval before spawning"
        );
    }
}
