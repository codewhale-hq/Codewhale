//! MCP server implementation for exposing Codewhale tools over stdio.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
    Implementation, InitializeRequestParams, InitializeResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use codewhale_engine::tools::spec::{ToolError, ToolResult};
use codewhale_engine::tools::{ToolContext, ToolRegistryBuilder};
use codewhale_skill_install::skill_parse::SkillInvocation;
use codewhale_skills::SkillSource;

const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[
    ProtocolVersion::V_2024_11_05,
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_11_25,
    ProtocolVersion::V_2026_07_28,
];
const RESOURCE_PAGE_SIZE: usize = 100;
const MAX_RESOURCE_CURSOR_BYTES: usize = 4096;

#[derive(Debug, Default, Deserialize)]
struct McpServerConfigFile {
    #[serde(default)]
    server: McpServerSection,
}

#[derive(Debug, Default, Deserialize)]
struct McpServerSection {
    expose_tools: Option<Vec<String>>,
    /// Withhold tools that write files or run commands. This stdio server has
    /// no channel for an out-of-band approval prompt, so "approval required"
    /// means those tools are refused. Defaults to `true`; only the operator's
    /// own config file can turn it off.
    require_approval: Option<bool>,
}

#[derive(Debug, Clone)]
struct McpServerSettings {
    expose_tools: Vec<String>,
    require_approval: bool,
}

impl McpServerSettings {
    fn load() -> Result<Self> {
        let path = default_config_path();
        if let Some(path) = path.filter(|p| p.exists()) {
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read MCP server config: {}", path.display()))?;
            Self::from_toml(&contents)
                .with_context(|| format!("Failed to parse MCP server config: {}", path.display()))
        } else {
            Ok(Self {
                expose_tools: default_expose_tools(),
                require_approval: true,
            })
        }
    }

    fn from_toml(contents: &str) -> Result<Self> {
        let config: McpServerConfigFile = toml::from_str(contents)?;
        Ok(Self {
            expose_tools: config
                .server
                .expose_tools
                .unwrap_or_else(default_expose_tools),
            require_approval: config.server.require_approval.unwrap_or(true),
        })
    }
}

#[derive(Debug, Clone)]
struct ExposedTool {
    public: String,
    internal: String,
}

pub async fn run_mcp_server(
    workspace: PathBuf,
    skills_dir: PathBuf,
    skills_config: std::sync::Arc<codewhale_runtime::config::SkillsConfig>,
) -> Result<()> {
    // Settings load is a synchronous config read; keep it off the async
    // worker per the blocking-call convention.
    let settings = tokio::task::spawn_blocking(McpServerSettings::load)
        .await
        .context("MCP server settings task failed")??;
    let server = McpServer::new(workspace, settings, skills_dir, skills_config)?;
    // stdout carries the protocol; the notice goes to stderr so an operator
    // whose config lists write tools sees why they are missing.
    for name in server.withheld_tools() {
        eprintln!(
            "codewhale mcp server: not exposing '{name}': it writes files or runs \
             commands and require_approval is on. Set require_approval = false under \
             [server] in the MCP server config to allow it."
        );
    }
    let (input, output) = rmcp::transport::stdio_pipes();
    server.run_io(input, output).await
}

struct McpServer {
    workspace: PathBuf,
    registry: codewhale_engine::tools::ToolRegistry,
    exposed_tools: Vec<ExposedTool>,
    tools: Vec<Tool>,
    require_approval: bool,
    initialized: AtomicBool,
    tool_execution: Mutex<()>,
    skills_dir: PathBuf,
    skills_config: std::sync::Arc<codewhale_runtime::config::SkillsConfig>,
}

struct SkillResource {
    resource: Resource,
    body: String,
}

impl McpServer {
    fn new(
        workspace: PathBuf,
        settings: McpServerSettings,
        skills_dir: PathBuf,
        skills_config: std::sync::Arc<codewhale_runtime::config::SkillsConfig>,
    ) -> Result<Self> {
        let exposed_tools = build_exposed_tools(&settings.expose_tools);
        let internal_names: HashSet<&str> = exposed_tools
            .iter()
            .map(|tool| tool.internal.as_str())
            .collect();
        let mut builder = ToolRegistryBuilder::new()
            .with_file_tools()
            .with_search_tools();
        if internal_names.contains("apply_patch") {
            builder = builder.with_patch_tools();
        }
        if internal_names.contains("exec_shell") {
            builder = builder.with_shell_tools();
        }
        let registry = builder.build(
            ToolContext::new(workspace.clone())
                .with_skills_config(skills_dir.clone(), skills_config.clone()),
        );
        let mut tools = Vec::new();
        let mut seen = HashSet::new();
        for entry in &exposed_tools {
            if !seen.insert(entry.public.clone()) {
                continue;
            }
            if let Some(tool) = registry.get(&entry.internal) {
                if settings.require_approval && !tool.is_read_only() {
                    continue;
                }
                let schema = tool.input_schema();
                let schema = schema
                    .as_object()
                    .cloned()
                    .context("Invalid native tool schema")?;
                let mut annotations = ToolAnnotations::new();
                annotations.read_only_hint = Some(tool.is_read_only());
                tools.push(
                    Tool::new(entry.public.clone(), tool.description().to_string(), schema)
                        .with_annotations(annotations),
                );
            }
        }
        Ok(Self {
            workspace,
            registry,
            exposed_tools,
            tools,
            require_approval: settings.require_approval,
            initialized: AtomicBool::new(false),
            tool_execution: Mutex::new(()),
            skills_dir,
            skills_config,
        })
    }

    async fn run_io<R, W>(self, input: R, output: W) -> Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let server = Arc::new(self);
        let transport = AsyncRwTransport::new_server(input, output)
            .with_max_line_length(codewhale_engine::mcp::MAX_MCP_RESPONSE_BYTES);
        let service = server
            .clone()
            .serve(transport)
            .await
            .map_err(|_| anyhow::anyhow!("MCP server protocol setup failed"))?;
        let outcome = service.waiting().await.context("MCP server task failed");
        let _drained = server.tool_execution.lock().await;
        outcome?;
        Ok(())
    }

    /// Configured tools that `require_approval` keeps out of `tools/list`.
    fn withheld_tools(&self) -> Vec<String> {
        if !self.require_approval {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        self.exposed_tools
            .iter()
            .filter(|entry| seen.insert(entry.public.clone()))
            .filter(|entry| {
                self.registry
                    .get(&entry.internal)
                    .is_some_and(|tool| !tool.is_read_only())
            })
            .map(|entry| entry.public.clone())
            .collect()
    }

    async fn skill_resources(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<SkillResource>, ErrorData> {
        let workspace = self.workspace.clone();
        let skills_dir = self.skills_dir.clone();
        let skills_config = self.skills_config.clone();
        let discovery = tokio::task::spawn_blocking(move || {
            let registry = codewhale_skills::discover_for_workspace(
                &workspace,
                Some(&skills_dir),
                &skills_config,
                None,
            );
            let mut resources: Vec<_> = registry
                .list()
                .iter()
                .filter(|skill| {
                    matches!(skill.source, SkillSource::Native)
                        && skill.invocation != SkillInvocation::Disabled
                })
                .map(|skill| {
                    let uri = format!("codewhale://skills/{}", urlencoding::encode(&skill.name));
                    let description: String = skill
                        .description
                        .chars()
                        .take(codewhale_skills::DEFAULT_SKILL_DESCRIPTION_CHARS)
                        .collect();
                    SkillResource {
                        resource: Resource::new(uri, skill.name.clone())
                            .with_title(format!("{} instructions", skill.name))
                            .with_description(description)
                            .with_mime_type("text/markdown")
                            .with_size(skill.body.len() as u64),
                        body: skill.body.clone(),
                    }
                })
                .collect();
            resources.sort_by(|a, b| a.resource.uri.cmp(&b.resource.uri));
            resources
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled_request()),
            result = discovery => result.map_err(|_| ErrorData::internal_error("Skill discovery failed", None)),
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .with_server_info(Implementation::new(
            "codewhale-mcp-server",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_PROTOCOL_VERSIONS)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        if request.client_info.name.trim().is_empty()
            || request.client_info.version.trim().is_empty()
            || request.protocol_version.as_str().trim().is_empty()
        {
            return Err(ErrorData::invalid_params(
                "Invalid MCP initialize parameters",
                None,
            ));
        }
        if self.initialized.swap(true, Ordering::AcqRel) {
            return Err(ErrorData::invalid_request(
                "Initialize may only be sent once",
                None,
            ));
        }
        let result = self.negotiate_initialize(&request)?;
        context.peer.set_peer_info(request);
        Ok(result)
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        reject_cursor(request)?;
        Ok(ListToolsResult::with_all_items(self.tools.clone())
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let entry = self
            .exposed_tools
            .iter()
            .find(|tool| tool.public == request.name.as_ref())
            .ok_or_else(|| ErrorData::invalid_params("Tool not exposed", None))?;
        let tool = self
            .registry
            .get(&entry.internal)
            .ok_or_else(|| ErrorData::invalid_params("Tool not exposed", None))?;
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        // Approval comes from the operator's config, never from the caller:
        // a request cannot vouch for itself.
        if self.require_approval && !(tool.is_read_only() && tool.is_read_only_for(&arguments)) {
            return Err(ErrorData::new(
                ErrorCode(-32001),
                "Tool requires approval, which this server cannot request; the operator must explicitly permit it in the MCP server configuration",
                None,
            ));
        }
        let _execution = tokio::select! {
            biased;
            _ = context.ct.cancelled() => return Err(cancelled_request()),
            guard = self.tool_execution.lock() => guard,
        };
        if context.ct.is_cancelled() {
            return Err(cancelled_request());
        }
        let mut tool_context = self.registry.context().clone();
        tool_context.cancel_token = Some(context.ct.clone());
        let result = self
            .registry
            .execute_rich_full_with_context(&entry.internal, arguments, Some(&tool_context), || {
                Ok(None)
            })
            .await
            .map(|result| result.into_result());
        Ok(tool_result_to_mcp(result).into())
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = self.skill_resources(&context.ct).await?;
        let start = match request.and_then(|request| request.cursor) {
            None => 0,
            Some(cursor) => {
                let uri = cursor
                    .strip_prefix("v1:")
                    .filter(|_| cursor.len() <= MAX_RESOURCE_CURSOR_BYTES)
                    .ok_or_else(|| ErrorData::invalid_params("Invalid resource cursor", None))?;
                resources
                    .binary_search_by(|item| item.resource.uri.as_str().cmp(uri))
                    .map(|index| index + 1)
                    .map_err(|_| {
                        ErrorData::invalid_params("Invalid or stale resource cursor", None)
                    })?
            }
        };
        let end = start
            .saturating_add(RESOURCE_PAGE_SIZE)
            .min(resources.len());
        let mut result = ListResourcesResult::with_all_items(
            resources[start..end]
                .iter()
                .map(|item| item.resource.clone())
                .collect(),
        )
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private);
        if end < resources.len() {
            result.next_cursor = Some(format!("v1:{}", resources[end - 1].resource.uri));
        }
        bounded_resource_result(result)
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        reject_cursor(request)?;
        Ok(ListResourceTemplatesResult::default()
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri.len() > MAX_RESOURCE_CURSOR_BYTES
            || !request.uri.starts_with("codewhale://skills/")
        {
            return Err(ErrorData::invalid_params("Resource not found", None));
        }
        let item = self
            .skill_resources(&context.ct)
            .await?
            .into_iter()
            .find(|item| item.resource.uri == request.uri)
            .ok_or_else(|| ErrorData::invalid_params("Resource not found", None))?;
        if item.body.len() > codewhale_engine::mcp::MAX_MCP_RESPONSE_BYTES {
            return Err(ErrorData::internal_error(
                "Skill resource exceeds the response limit",
                None,
            ));
        }
        let result = ReadResourceResult::new(vec![
            ResourceContents::text(item.body, item.resource.uri).with_mime_type("text/markdown"),
        ])
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private);
        Ok(bounded_resource_result(result)?.into())
    }
}

fn cancelled_request() -> ErrorData {
    ErrorData::new(ErrorCode(-32800), "Request cancelled", None)
}

fn reject_cursor(request: Option<PaginatedRequestParams>) -> Result<(), ErrorData> {
    if request.and_then(|request| request.cursor).is_some() {
        Err(ErrorData::invalid_params("Invalid cursor", None))
    } else {
        Ok(())
    }
}

fn bounded_resource_result<T: Serialize>(result: T) -> Result<T, ErrorData> {
    let bytes = serde_json::to_vec(&result)
        .map_err(|_| ErrorData::internal_error("Resource serialization failed", None))?;
    if bytes.len() > codewhale_engine::mcp::MAX_MCP_RESPONSE_BYTES - 1024 {
        return Err(ErrorData::internal_error(
            "Skill resource exceeds the response limit",
            None,
        ));
    }
    Ok(result)
}

fn default_config_path() -> Option<PathBuf> {
    codewhale_paths::user_home().map(|home| home.join(".deepseek").join("mcp_server.toml"))
}

/// Read-only by default: writing or executing tools must be named in the
/// operator's config and allowed with `require_approval = false`.
fn default_expose_tools() -> Vec<String> {
    vec!["file_read".to_string(), "search".to_string()]
}

fn build_exposed_tools(names: &[String]) -> Vec<ExposedTool> {
    let mut tools = Vec::new();
    for name in names {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let public = trimmed.to_string();
        let internal = match trimmed {
            "file_read" => "read_file",
            "file_write" => "write_file",
            "file_edit" => "edit_file",
            "shell" => "exec_shell",
            "search" => "grep_files",
            "file_search" => "file_search",
            other => other,
        }
        .to_string();
        tools.push(ExposedTool { public, internal });
    }
    tools
}

fn tool_result_to_mcp(result: Result<ToolResult, ToolError>) -> CallToolResult {
    match result {
        Ok(tool_result) => {
            let mut response =
                CallToolResult::success(vec![ContentBlock::text(tool_result.content)]);
            response.is_error = Some(!tool_result.success);
            response.structured_content = tool_result.metadata;
            response
        }
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use tokio::io::{
        AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
    };

    fn initialize_request() -> Value {
        json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "clientInfo": {"name": "native-server-test", "version": "1"},
                "capabilities": {}
            }
        })
    }

    fn test_server(workspace: PathBuf, settings: McpServerSettings) -> McpServer {
        let skills_dir = workspace.join(".fixture-skills");
        McpServer::new(
            workspace,
            settings,
            skills_dir,
            std::sync::Arc::new(codewhale_runtime::config::SkillsConfig {
                scan_codewhale_only: Some(true),
                ..Default::default()
            }),
        )
        .expect("build server")
    }

    struct Pipe {
        reader: BufReader<ReadHalf<DuplexStream>>,
        writer: WriteHalf<DuplexStream>,
        task: tokio::task::JoinHandle<Result<()>>,
    }

    impl Pipe {
        fn new(server: McpServer) -> Self {
            let (client, server_io) = tokio::io::duplex(8192);
            let (reader, writer) = tokio::io::split(client);
            let (input, output) = tokio::io::split(server_io);
            let task = tokio::spawn(server.run_io(input, output));
            Self {
                reader: BufReader::new(reader),
                writer,
                task,
            }
        }

        async fn send(&mut self, request: Value) {
            let mut bytes = serde_json::to_vec(&request).unwrap();
            bytes.push(b'\n');
            self.writer.write_all(&bytes).await.unwrap();
            self.writer.flush().await.unwrap();
        }

        async fn response(&mut self) -> Value {
            let mut line = String::new();
            let count = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.reader.read_line(&mut line),
            )
            .await
            .expect("server response timeout")
            .unwrap();
            assert!(count > 0, "server closed before responding");
            serde_json::from_str(&line).unwrap()
        }

        async fn request(&mut self, request: Value) -> Value {
            self.send(request).await;
            self.response().await
        }

        async fn finish(self) -> Result<()> {
            drop(self.writer);
            drop(self.reader);
            tokio::time::timeout(std::time::Duration::from_secs(5), self.task)
                .await
                .expect("server shutdown timeout")
                .unwrap()
        }
    }

    async fn complete_handshake(pipe: &mut Pipe) {
        let response = pipe.request(initialize_request()).await;
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        pipe.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
    }

    #[tokio::test]
    async fn native_server_validates_identity_without_echoing_request_data() {
        let mut pipe = Pipe::new(test_server(
            PathBuf::from("."),
            McpServerSettings {
                expose_tools: default_expose_tools(),
                require_approval: true,
            },
        ));
        complete_handshake(&mut pipe).await;
        for request in [
            json!([]),
            Value::Null,
            json!({"method": "ping"}),
            json!({"jsonrpc": "2", "id": 1, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": true, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 1.5, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": 7}),
            json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": "PRIVATE_TOKEN=sentinel"}),
        ] {
            let response = pipe.request(request).await;
            assert!(response["id"].is_null(), "{response}");
            assert_eq!(response["error"]["code"], -32600, "{response}");
            assert!(!response.to_string().contains("PRIVATE_TOKEN"));
            assert!(!response.to_string().contains("sentinel"));
        }
        let response = pipe
            .request(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}))
            .await;
        assert_eq!(response["result"], json!({}));
        pipe.finish().await.unwrap();
    }

    #[tokio::test]
    async fn native_server_requires_valid_lifecycle_before_any_tool_effect() {
        let workspace = tempfile::tempdir().unwrap();
        let mut pipe = Pipe::new(test_server(
            workspace.path().to_path_buf(),
            McpServerSettings {
                expose_tools: vec!["file_write".into()],
                require_approval: false,
            },
        ));
        let write = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "file_write", "arguments": {"path": "canary.txt", "content": "written"}
        }});
        let response = pipe.request(write.clone()).await;
        assert!(response.get("error").is_some(), "{response}");
        assert!(!workspace.path().join("canary.txt").exists());
        let mut null_id = initialize_request();
        null_id["id"] = Value::Null;
        assert_eq!(pipe.request(null_id).await["error"]["code"], -32600);
        assert!(pipe.request(write.clone()).await.get("error").is_some());
        assert!(!workspace.path().join("canary.txt").exists());
        let response = pipe.request(initialize_request()).await;
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(
            pipe.request(initialize_request()).await["error"]["code"],
            -32600
        );
        let mut unacknowledged = write.clone();
        unacknowledged.as_object_mut().unwrap().remove("id");
        pipe.send(unacknowledged).await;
        let response = pipe
            .request(json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}))
            .await;
        assert_eq!(response["id"], 3);
        assert!(!workspace.path().join("canary.txt").exists());
        let response = pipe.request(write).await;
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("canary.txt")).unwrap(),
            "written"
        );
        pipe.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        pipe.finish().await.unwrap();
    }

    async fn run_native_pipe(input: &[u8], expected_responses: usize) -> (Result<()>, Vec<u8>) {
        let mut pipe = Pipe::new(test_server(
            PathBuf::from("."),
            McpServerSettings {
                expose_tools: default_expose_tools(),
                require_approval: true,
            },
        ));
        complete_handshake(&mut pipe).await;
        let mut output = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            pipe.writer.write_all(input).await.unwrap();
            pipe.writer.flush().await.unwrap();
            for _ in 0..expected_responses {
                let response = pipe.response().await;
                output.extend(serde_json::to_vec(&response).unwrap());
                output.push(b'\n');
            }
            pipe.writer.shutdown().await.unwrap();
            pipe.reader.read_to_end(&mut output).await.unwrap();
        })
        .await
        .expect("the bounded server pipe must settle");
        (pipe.finish().await, output)
    }

    #[tokio::test]
    async fn native_pipe_discards_invalid_json_and_preserves_the_following_frame() {
        let (result, output) = run_native_pipe(b"{\"PRIVATE_TOKEN\":\"sentinel\",\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n", 1).await;
        result.unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("PRIVATE_TOKEN"));
        assert!(!text.contains("sentinel"));
        let responses: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], 7);
        assert_eq!(responses[0]["result"], json!({}));
    }

    #[tokio::test]
    async fn native_pipe_refuses_oversized_unterminated_input() {
        let input = vec![b'x'; codewhale_engine::mcp::MAX_MCP_RESPONSE_BYTES + 8192];
        let (result, output) = run_native_pipe(&input, 0).await;
        result.unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn exposed_tools_map_aliases() {
        let names = vec![
            "file_read".to_string(),
            "file_write".to_string(),
            "search".to_string(),
            "apply_patch".to_string(),
            "shell".to_string(),
        ];
        let mut map = HashMap::new();
        for tool in build_exposed_tools(&names) {
            map.insert(tool.public, tool.internal);
        }
        assert_eq!(map.get("file_read").map(String::as_str), Some("read_file"));
        assert_eq!(
            map.get("file_write").map(String::as_str),
            Some("write_file")
        );
        assert_eq!(map.get("search").map(String::as_str), Some("grep_files"));
        assert_eq!(
            map.get("apply_patch").map(String::as_str),
            Some("apply_patch")
        );
        assert_eq!(map.get("shell").map(String::as_str), Some("exec_shell"));
    }

    #[tokio::test]
    async fn list_responses_omit_null_next_cursor() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let skills_dir = root.path().join("skills");
        std::fs::create_dir_all(&workspace).unwrap();
        for index in 0..125 {
            let name = format!("skill-{index:03}");
            let path = skills_dir.join(&name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!(
                    "---\nname: {name}\ndescription: Fixture instructions\n---\nbody-{index}\n"
                ),
            )
            .unwrap();
        }
        let server = McpServer::new(
            workspace,
            McpServerSettings {
                expose_tools: vec!["file_read".into(), "apply_patch".into()],
                require_approval: false,
            },
            skills_dir,
            std::sync::Arc::new(codewhale_runtime::config::SkillsConfig {
                scan_codewhale_only: Some(true),
                ..Default::default()
            }),
        )
        .unwrap();
        let mut pipe = Pipe::new(server);
        complete_handshake(&mut pipe).await;
        let tools = pipe
            .request(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await;
        assert!(tools["result"]["tools"].is_array());
        assert!(tools["result"].get("nextCursor").is_none());
        let first = pipe
            .request(json!({"jsonrpc":"2.0","id":2,"method":"resources/list"}))
            .await;
        assert_eq!(
            first["result"]["resources"].as_array().unwrap().len(),
            RESOURCE_PAGE_SIZE
        );
        let cursor = first["result"]["nextCursor"].as_str().unwrap();
        let second = pipe.request(json!({"jsonrpc":"2.0","id":3,"method":"resources/list","params":{"cursor":cursor}})).await;
        assert_eq!(second["result"]["resources"].as_array().unwrap().len(), 25);
        assert!(second["result"].get("nextCursor").is_none());
        let uri = first["result"]["resources"][0]["uri"].as_str().unwrap();
        let body = pipe
            .request(json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":uri}}))
            .await;
        assert_eq!(body["result"]["contents"][0]["uri"], uri);
        assert_eq!(body["result"]["contents"][0]["mimeType"], "text/markdown");
        assert_eq!(
            body["result"]["contents"][0]["text"]
                .as_str()
                .unwrap()
                .trim(),
            "body-0"
        );
        let invalid = pipe.request(json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"file:///etc/passwd"}})).await;
        assert_eq!(invalid["error"]["code"], -32602);
        let invalid = pipe.request(json!({"jsonrpc":"2.0","id":6,"method":"resources/list","params":{"cursor":"invalid"}})).await;
        assert_eq!(invalid["error"]["code"], -32602);
        pipe.finish().await.unwrap();
    }

    #[tokio::test]
    async fn retired_deepseek_tools_are_not_exposed() {
        let server = test_server(
            PathBuf::from("."),
            McpServerSettings {
                expose_tools: vec!["deepseek".into(), "deepseek-reply".into()],
                require_approval: false,
            },
        );
        assert!(server.tools.is_empty());
        let mut pipe = Pipe::new(server);
        complete_handshake(&mut pipe).await;
        let response = pipe
            .request(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"deepseek","arguments":{"prompt":"hi"}
                }}),
            )
            .await;
        assert_eq!(response["error"]["code"], -32602, "{response}");
        pipe.finish().await.unwrap();
    }

    #[test]
    fn default_settings_expose_only_read_only_tools() {
        assert_eq!(default_expose_tools(), vec!["file_read", "search"]);
        let server = test_server(
            PathBuf::from("."),
            McpServerSettings {
                expose_tools: default_expose_tools(),
                require_approval: true,
            },
        );
        let names: Vec<&str> = server.tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, vec!["file_read", "search"]);
        assert!(
            server
                .tools
                .iter()
                .all(|tool| tool.annotations.as_ref().unwrap().read_only_hint == Some(true))
        );
    }

    #[test]
    fn existing_config_without_require_approval_withholds_and_names_write_tools() {
        let settings = McpServerSettings::from_toml(
            "[server]\nexpose_tools = [\"file_read\", \"file_write\", \"apply_patch\"]\n",
        )
        .expect("parse config");
        assert!(settings.require_approval);
        let server = test_server(PathBuf::from("."), settings);
        assert_eq!(server.withheld_tools(), vec!["file_write", "apply_patch"]);
        let allowed = McpServerSettings::from_toml(
            "[server]\nexpose_tools = [\"file_write\"]\nrequire_approval = false\n",
        )
        .expect("parse config");
        assert!(
            test_server(PathBuf::from("."), allowed)
                .withheld_tools()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn caller_cannot_self_approve_write_tools() {
        let workspace = tempfile::tempdir().unwrap();
        let canary = workspace.path().join("canary.txt");
        let server = test_server(
            workspace.path().to_path_buf(),
            McpServerSettings {
                expose_tools: vec![
                    "file_read".into(),
                    "file_write".into(),
                    "apply_patch".into(),
                ],
                require_approval: true,
            },
        );
        let names: Vec<&str> = server.tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, vec!["file_read"]);
        let mut pipe = Pipe::new(server);
        complete_handshake(&mut pipe).await;
        for (name, arguments) in [
            ("file_write", json!({"path":"canary.txt","content":"x"})),
            (
                "apply_patch",
                json!({"patch":"--- /dev/null\n+++ b/canary.txt\n@@ -0,0 +1 @@\n+x\n"}),
            ),
        ] {
            let response = pipe
                .request(
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                        "name":name,"approved":true,"arguments":arguments
                    }}),
                )
                .await;
            assert_eq!(response["error"]["code"], -32001, "{name}: {response}");
            assert!(!canary.exists());
        }
        pipe.finish().await.unwrap();
    }

    #[tokio::test]
    async fn operator_config_can_allow_write_tools() {
        let workspace = tempfile::tempdir().unwrap();
        let mut pipe = Pipe::new(test_server(
            workspace.path().to_path_buf(),
            McpServerSettings {
                expose_tools: vec!["file_write".into()],
                require_approval: false,
            },
        ));
        complete_handshake(&mut pipe).await;
        let response = pipe
            .request(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"file_write","arguments":{"path":"allowed.txt","content":"x"}
                }}),
            )
            .await;
        assert!(response.get("error").is_none(), "{response}");
        assert!(workspace.path().join("allowed.txt").exists(), "{response}");
        pipe.finish().await.unwrap();
    }

    #[tokio::test]
    async fn initialize_uses_standard_mcp_shape_and_codewhale_identity() {
        let settings = McpServerSettings {
            expose_tools: default_expose_tools(),
            require_approval: true,
        };
        let mut pipe = Pipe::new(test_server(PathBuf::from("."), settings.clone()));
        let response = pipe.request(initialize_request()).await;
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(
            response["result"]["serverInfo"]["name"],
            "codewhale-mcp-server"
        );
        assert_eq!(
            response["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            response["result"]["capabilities"],
            json!({"tools":{},"resources":{}})
        );
        pipe.finish().await.unwrap();
        let mut pipe = Pipe::new(test_server(PathBuf::from("."), settings));
        let mut malformed = initialize_request();
        malformed["params"]["clientInfo"]["name"] = json!("");
        let response = pipe.request(malformed).await;
        assert_eq!(response["error"]["code"], -32602);
        assert!(pipe.finish().await.is_err());
    }

    #[tokio::test]
    async fn initialize_negotiates_supported_revisions() {
        let settings = McpServerSettings {
            expose_tools: default_expose_tools(),
            require_approval: true,
        };
        for requested in [
            "2024-11-05",
            "2025-03-26",
            "2025-06-18",
            "2025-11-25",
            "2026-07-28",
            "2099-01-01",
        ] {
            let mut pipe = Pipe::new(test_server(PathBuf::from("."), settings.clone()));
            let mut request = initialize_request();
            request["params"]["protocolVersion"] = json!(requested);
            let response = pipe.request(request).await;
            let expected = if requested < "2026-07-28" {
                requested
            } else {
                "2025-11-25"
            };
            assert_eq!(response["result"]["protocolVersion"], expected);
            pipe.finish().await.unwrap();
        }
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientInfo":{"name":"modern-test","version":"1"},
            "io.modelcontextprotocol/clientCapabilities":{}
        });
        for discover_first in [false, true] {
            let mut pipe = Pipe::new(test_server(PathBuf::from("."), settings.clone()));
            if discover_first {
                let discovery = pipe.request(json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":meta}})).await;
                assert_eq!(discovery["result"]["resultType"], "complete");
                assert_eq!(
                    discovery["result"]["capabilities"],
                    json!({"tools":{},"resources":{}})
                );
                assert!(
                    discovery["result"]["supportedVersions"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("2026-07-28"))
                );
            }
            let tools = pipe
                .request(
                    json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":meta}}),
                )
                .await;
            assert_eq!(tools["result"]["resultType"], "complete");
            assert_eq!(tools["result"]["ttlMs"], 0);
            assert_eq!(tools["result"]["cacheScope"], "private");
            let missing = pipe
                .request(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
                .await;
            assert_eq!(missing["error"]["code"], -32602);
            pipe.finish().await.unwrap();
        }
    }
}
