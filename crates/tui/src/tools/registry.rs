//! Tool registry for managing and executing tools.
//!
//! The registry provides:
//! - Dynamic tool registration
//! - Tool lookup by name
//! - Conversion to API Tool format
//! - Filtering by capability

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use std::path::{Path, PathBuf};

use codewhale_protocol::runtime::DynamicToolSpec;
use serde_json::Value;

use crate::client::CodewhaleClient;
use crate::tools::goal::SharedGoalState;
use codewhale_models::Tool;

use super::schema_canonicalize;
use super::schema_sanitize;
use super::spec::{
    ApprovalRequirement, RichToolResult, ToolCapability, ToolContext, ToolError, ToolResult,
    ToolResultContentBlock, ToolSpec,
};

// === Types ===

/// Registry that holds all available tools.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn ToolSpec>>,
    context: ToolContext,
    /// Memoised serialised tool catalog. Rebuilt lazily on first
    /// `to_api_tools` call after a mutation; pinned across reads so the
    /// description and schema bytes stay byte-stable for DeepSeek's KV
    /// prefix cache. Invalidated on `register` / `remove_tool`.
    api_cache: OnceLock<Vec<Tool>>,
}

/// The one sentence naming a `[tools.overrides]` entry that D4 refused, shared
/// by the runtime log and the engine's user-facing status line.
pub(crate) fn override_refusal_notice(tool_name: &str) -> String {
    format!(
        "Refused [tools.overrides.{name}]: a script or command override cannot replace the built-in tool '{name}', which stays active. Set type = \"disabled\" to turn it off, or key the override by a new tool name.",
        name = crate::safe_label::SafeLabel::identifier(tool_name)
    )
}

impl ToolRegistry {
    /// Create a new empty registry with the given context.
    #[must_use]
    pub fn new(context: ToolContext) -> Self {
        Self {
            tools: HashMap::new(),
            context,
            api_cache: OnceLock::new(),
        }
    }

    /// Register a tool in the registry.
    pub fn register(&mut self, tool: Arc<dyn ToolSpec>) {
        let name = tool.name().to_string();
        if let Some(previous) = self.tools.get(&name) {
            tracing::warn!(
                previous_origin = ?previous.registration_origin(),
                replacement_origin = ?tool.registration_origin(),
                "Overwriting existing tool: {}", crate::safe_label::SafeLabel::identifier(&name)
            );
        }
        self.tools.insert(name, tool);
        self.invalidate_api_cache();
    }

    /// Register multiple tools at once.
    pub fn register_all(&mut self, tools: Vec<Arc<dyn ToolSpec>>) {
        for tool in tools {
            self.register(tool);
        }
    }

    /// Get a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolSpec>> {
        self.tools.get(name).cloned()
    }

    /// Check if a tool exists.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Get all registered tool names.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(std::string::String::as_str).collect()
    }

    /// Get all registered tools.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<dyn ToolSpec>> {
        self.tools.values().cloned().collect()
    }

    /// Execute a tool by name, returning the full `ToolResult`.
    pub async fn execute_full(&self, name: &str, input: Value) -> Result<ToolResult, ToolError> {
        self.execute_rich_full(name, input)
            .await
            .map(RichToolResult::into_result)
    }

    pub(crate) async fn execute_rich_full(
        &self,
        name: &str,
        input: Value,
    ) -> Result<RichToolResult, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::not_available(format!("tool '{name}' is not registered")))?;

        crate::extension_host::validate_caller_plugins(self.context.plugin_registry.as_deref())
            .map_err(ToolError::not_available)?;
        let child = self.admit_child_call(name, &input, &self.context).await?;
        enforce_tool_authority(name, &input, tool.as_ref(), &self.context)?;
        let rich = tool
            .execute_rich(input, &self.context)
            .await
            .map(crate::image_attach::bound_rich_tool_result)?;
        if rich.result.success
            && let Some((authority, writes)) = child
        {
            authority.record_settled_writes(writes).await;
        }
        Ok(rich)
    }

    pub(crate) async fn execute_rich_full_with_context<'a, F>(
        &self,
        name: &str,
        input: Value,
        context_override: Option<&ToolContext>,
        on_admitted: F,
    ) -> Result<RichToolResult, ToolError>
    where
        F: FnOnce() -> Result<
                Option<crate::core::engine::tool_execution::OperationSpanGuard<'a>>,
                ToolError,
            > + Send,
    {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::not_available(format!("tool '{name}' is not registered")))?;

        let ctx = context_override.unwrap_or(&self.context);
        crate::extension_host::validate_caller_plugins(ctx.plugin_registry.as_deref())
            .map_err(ToolError::not_available)?;
        let child = self.admit_child_call(name, &input, ctx).await?;
        enforce_tool_authority(name, &input, tool.as_ref(), ctx)?;
        let operation_span = on_admitted()?;
        let outcome = tool.execute_rich(input.clone(), ctx).await;
        if let Some(operation_span) = operation_span {
            operation_span
                .complete(super::activity::operation_outcome(
                    &outcome,
                    ctx.cancel_token
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled),
                ))
                .await;
        }
        let mut rich = crate::image_attach::bound_rich_tool_result(outcome?);
        if rich.result.success
            && let Some((authority, writes)) = child
        {
            authority.record_settled_writes(writes).await;
        }
        let result = &mut rich.result;

        // Adaptive evidence routing (#4619) is an explicit opt-in
        // (`CODEWHALE_ADAPTIVE_OUTPUT_ROUTING`) and is storage-free here
        // because this layer does not own a call id. The engine/subagent
        // completion boundary publishes the exact artifact. Under the default
        // classic lane nothing happens at this layer — the same boundary owns
        // the bounded spillover preview.
        if crate::tools::large_output_router::adaptive_output_routing_enabled()
            && let Some(router) = ctx.large_output_router.as_ref()
        {
            use crate::tools::large_output_router::EvidenceRouting;
            let (estimated_routing, estimated_tokens, threshold) =
                router.evidence_routing(name, result);
            let metadata = result.metadata.get_or_insert_with(|| serde_json::json!({}));
            if let Some(object) = metadata.as_object_mut() {
                // A tool that self-bounds its output behind its own
                // recovery contract (e.g. read_file's `next_start_line`
                // paging) declares its routing itself; the size estimate
                // must not override that and double-wrap the result.
                let routing = object
                    .get("evidence_routing")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<EvidenceRouting>(value).ok())
                    .unwrap_or(estimated_routing);
                object.insert(
                    "evidence_routing".to_string(),
                    serde_json::to_value(routing).unwrap_or_else(|_| serde_json::json!("inline")),
                );
                object.insert(
                    "evidence_estimated_tokens".to_string(),
                    estimated_tokens.into(),
                );
                object.insert("evidence_threshold_tokens".to_string(), threshold.into());
            }
        }

        Ok(rich)
    }

    pub(crate) async fn admit_child_call(
        &self,
        name: &str,
        input: &Value,
        context: &ToolContext,
    ) -> Result<Option<(Arc<super::subagent::engine::ChildAuthority>, Vec<String>)>, ToolError>
    {
        let child = self
            .context
            .child_host
            .as_ref()
            .or(context.child_host.as_ref());
        let Some(child) = child else {
            return Ok(None);
        };
        if self
            .context
            .child_host
            .as_ref()
            .zip(context.child_host.as_ref())
            .is_some_and(|(expected, actual)| !Arc::ptr_eq(expected, actual))
        {
            return Err(ToolError::permission_denied(
                "child context cannot replace its captured grant",
            ));
        }
        child.validate_context(context)?;
        let writes = child
            .validate_claim(self, name, input)
            .await
            .map_err(super::subagent::engine::ChildAuthority::typed_error)?;
        // Claim lookup may wait; current caller liveness and immutable child ceiling are rechecked before effects.
        child.validate_context(context)?;
        crate::extension_host::validate_caller_plugins(context.plugin_registry.as_deref())
            .map_err(ToolError::not_available)?;
        child
            .validate(self, name, input)
            .map_err(super::subagent::engine::ChildAuthority::typed_error)?;
        Ok(Some((child.clone(), writes)))
    }

    /// Get the current tool context.
    #[must_use]
    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    /// Convert all tools to API Tool format for sending to the model.
    ///
    /// Output is sorted by tool name for **prefix-cache stability** (#263).
    /// Rust's `HashMap` uses a randomly-seeded hasher per process, so a raw
    /// `self.tools.values()` iteration emits tools in a different order on
    /// every `deepseek` launch, invalidating DeepSeek's KV prefix cache for
    /// every cross-session resume. Sorting here matches the way Claude Code
    /// stabilises its tool array (`assembleToolPool` in their reference).
    ///
    /// The serialised catalog is memoised on first call and pinned across
    /// reads so each tool's `description()` and `input_schema()` are sampled
    /// exactly once per registration. MCP adapters whose upstream description
    /// drifts on reconnect would otherwise rewrite the catalog mid-session
    /// and bust the prefix cache. The cache is invalidated on `register`,
    /// `remove`, and `clear`.
    #[must_use]
    pub fn to_api_tools(&self) -> Vec<Tool> {
        self.api_cache
            .get_or_init(|| self.build_api_tools())
            .clone()
    }

    fn build_api_tools(&self) -> Vec<Tool> {
        let read_only_authority = self.context.tool_authority.as_deref().filter(|authority| {
            authority.authority == super::spec::ToolMutationAuthority::ReadOnly
        });
        let evidence_only = read_only_authority.is_some();
        let evidence_network = self
            .context
            .tool_authority
            .as_ref()
            .is_none_or(|authority| authority.network_access == Some(true));
        let mut tools: Vec<&Arc<dyn ToolSpec>> = self.tools.values().collect();
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
            .into_iter()
            .filter(|tool| tool.model_visible())
            .filter(|tool| {
                read_only_authority.is_none_or(|authority| {
                    readonly_evidence_tool(tool.as_ref())
                        || (tool.name() == "Run"
                            && authority.verification
                                == super::spec::ToolVerificationAuthority::Bounded)
                })
            })
            .filter(|tool| evidence_network || !matches!(tool.name(), "Web" | "web.run"))
            .map(|tool| {
                let mut schema = tool.input_schema();
                if evidence_only {
                    project_readonly_evidence_schema(tool.name(), &mut schema);
                }
                schema_sanitize::sanitize(&mut schema);
                schema_canonicalize::canonicalize_schema(&mut schema);
                Tool {
                    tool_type: None,
                    name: tool.name().to_string(),
                    description: if evidence_only
                        && matches!(tool.name(), "bash" | "Bash" | "exec_shell")
                    {
                        format!(
                            "{} {}",
                            tool.description(),
                            codewhale_execpolicy::command_safety::readonly_command_help()
                        )
                    } else {
                        tool.description().to_string()
                    },
                    input_schema: schema,
                    allowed_callers: Some(vec!["direct".to_string()]),
                    defer_loading: Some(tool.defer_loading()),
                    input_examples: None,
                    strict: None,
                    cache_control: None,
                }
            })
            .collect()
    }

    fn invalidate_api_cache(&mut self) {
        self.api_cache = OnceLock::new();
    }

    /// Convert tools to API Tool format with optional cache control on the last tool.
    #[must_use]
    pub fn to_api_tools_with_cache(&self, enable_cache: bool) -> Vec<Tool> {
        let mut tools = self.to_api_tools();
        if enable_cache && let Some(last) = tools.last_mut() {
            last.cache_control = Some(codewhale_models::CacheControl {
                cache_type: "ephemeral".to_string(),
            });
        }
        tools
    }

    /// Flatten every registered tool into the exact facts the read-only
    /// request projection is allowed to report: name, description, model
    /// visibility, declared capabilities, declared approval requirement, and
    /// whether the tool came from the plugin surface.
    ///
    /// This hands out *data*, never tool objects, so the projection layer
    /// cannot execute anything. Output is sorted by name and does not touch the
    /// registry's own ordering or the memoised API catalog.
    #[must_use]
    pub fn registry_facts(
        &self,
        plugin_names: &std::collections::HashSet<String>,
    ) -> Vec<crate::tool_inspection::RegistryFacts> {
        let mut facts: Vec<crate::tool_inspection::RegistryFacts> = self
            .tools
            .values()
            .map(|tool| crate::tool_inspection::RegistryFacts {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                model_visible: tool.model_visible(),
                capabilities: tool
                    .capabilities()
                    .iter()
                    .map(|capability| format!("{capability:?}"))
                    .collect(),
                approval: format!("{:?}", tool.approval_requirement()),
                plugin: plugin_names.contains(tool.name()),
            })
            .collect();
        facts.sort_by(|a, b| a.name.cmp(&b.name));
        facts
    }

    /// Resolve a non-canonical tool name to a registered canonical name.
    ///
    /// Runs a deterministic ladder against the registered tool names:
    /// 1. Lowercase exact match.
    /// 2. Hyphens/spaces → underscores (read-file → read_file).
    /// 3. CamelCase → snake_case (ReadFile → read_file).
    /// 4. Strip trailing `_tool` / `-tool` suffix (twice).
    ///
    /// Returns `None` when no normalization matches (the caller surfaces
    /// "Unknown tool … did you mean: …"). There is deliberately **no fuzzy
    /// step**: a prefix guess over the registry would execute an arbitrary
    /// sibling tool the model never asked for (#5123-class) — a hallucinated
    /// name must fail, never dispatch.
    #[must_use]
    pub fn resolve(&self, requested: &str) -> Option<&str> {
        let names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        let lower = requested.to_lowercase();

        // 1. ASCII case-insensitive exact
        if let Some(n) = names.iter().find(|n| n.eq_ignore_ascii_case(requested)) {
            return Some(n);
        }
        // 2. hyphen/space → underscore
        let snaked = lower.replace(['-', ' '], "_");
        if let Some(n) = names.iter().find(|n| **n == snaked) {
            return Some(n);
        }
        // 3. CamelCase → snake_case
        let cc = to_snake_case(requested);
        if let Some(n) = names.iter().find(|n| **n == cc) {
            return Some(n);
        }
        // 4. strip _tool/-tool/tool suffix, twice
        let mut stripped = cc.clone();
        for _ in 0..2 {
            for suf in ["_tool", "-tool", "tool"] {
                if let Some(s) = stripped.strip_suffix(suf) {
                    stripped = s.to_string();
                    break;
                }
            }
        }
        if !stripped.is_empty()
            && let Some(n) = names.iter().find(|n| **n == stripped)
        {
            return Some(n);
        }
        None
    }

    /// Remove a tool from the registry by name. Returns `true` if the tool
    /// was present and removed, `false` if no tool with that name existed.
    pub fn remove_tool(&mut self, name: &str) -> bool {
        let existed = self.tools.remove(name).is_some();
        if existed {
            self.invalidate_api_cache();
        }
        existed
    }

    /// Apply config.toml tool overrides to this registry.
    ///
    /// For each entry in `overrides`:
    /// - `Disabled` removes the tool, built-ins included.
    /// - `Script` / `Command` registers the user's implementation under a name
    ///   no built-in owns: a new tool, or a replacement for a drop-in plugin
    ///   script of that name.
    /// - `Script` / `Command` keyed by a name in `builtin_names` is refused and
    ///   the built-in stays active: script tools cannot shadow built-ins (D4,
    ///   CURRENT_DECISIONS §26).
    ///
    /// Returns the refused override names so the engine can name each one to
    /// the user ([`override_refusal_notice`]); the runtime log records every
    /// refusal. `/plugin` does not list `[tools.overrides]` entries, so it
    /// cannot show them there.
    ///
    /// `plugin_dir` is used as the base for relative script paths.
    #[cfg(test)]
    pub fn apply_overrides(
        &mut self,
        overrides: &std::collections::HashMap<String, crate::config::ToolOverride>,
        plugin_dir: &Path,
        builtin_names: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        self.apply_overrides_with_executor(
            overrides,
            plugin_dir,
            builtin_names,
            crate::tools::plugin::PluginExecutor::for_engine(),
        )
    }
    pub(crate) fn apply_overrides_with_executor(
        &mut self,
        overrides: &std::collections::HashMap<String, crate::config::ToolOverride>,
        plugin_dir: &Path,
        builtin_names: &std::collections::HashSet<String>,
        executor: crate::tools::plugin::PluginExecutor,
    ) -> Vec<String> {
        let mut refused = Vec::new();
        for (tool_name, override_cfg) in overrides {
            match override_cfg {
                crate::config::ToolOverride::Disabled => {
                    if self.remove_tool(tool_name) {
                        tracing::info!("Tool '{}' disabled via config override", tool_name);
                    } else {
                        tracing::warn!("Cannot disable tool '{}': not registered", tool_name);
                    }
                }
                _ if builtin_names.contains(tool_name) => {
                    tracing::error!("{}", override_refusal_notice(tool_name));
                    refused.push(tool_name.clone());
                }
                _ => {
                    // Script and Command overrides create replacement tools.
                    use crate::tools::plugin::tool_from_override_with_executor;
                    match tool_from_override_with_executor(
                        tool_name,
                        override_cfg,
                        plugin_dir,
                        executor.clone(),
                    ) {
                        Some(replacement) => {
                            self.register(replacement);
                            tracing::info!("Tool '{}' replaced via config override", tool_name);
                        }
                        None => {
                            if self.remove_tool(tool_name) {
                                tracing::warn!(
                                    "Tool '{}' override did not create a replacement; removed the original tool to avoid override fallthrough",
                                    tool_name
                                );
                            } else {
                                tracing::warn!(
                                    "Tool '{}' override did not create a replacement and no registered tool existed",
                                    tool_name
                                );
                            }
                        }
                    }
                }
            }
        }
        refused
    }

    /// Load and register plugin tools from a directory.
    ///
    /// Each script with valid frontmatter (`# name:`, `# description:`, etc.)
    /// becomes a registered `ScriptPluginTool`. Name collisions are refused:
    /// a script tool never replaces a registered tool.
    #[cfg(test)]
    pub fn load_plugins(&mut self, plugin_dir: &Path) {
        self.load_plugins_with_executor(
            plugin_dir,
            crate::tools::plugin::PluginExecutor::for_engine(),
        );
    }
    pub(crate) fn load_plugins_with_executor(
        &mut self,
        plugin_dir: &Path,
        executor: crate::tools::plugin::PluginExecutor,
    ) {
        if !plugin_dir.exists() {
            tracing::debug!(
                "Plugin directory {} does not exist, skipping",
                plugin_dir.display()
            );
            return;
        }
        let plugins = crate::tools::plugin::load_plugin_tools_with_executor(plugin_dir, executor);
        let mut count = 0;
        for tool in plugins {
            if let Some(previous) = self.get(tool.name()) {
                tracing::error!(
                    previous_origin = ?previous.registration_origin(),
                    plugin_origin = ?tool.registration_origin(),
                    "Cannot load plugin tool '{}': name is already registered; script tools cannot replace a registered tool, so give the script its own name",
                    crate::safe_label::SafeLabel::identifier(tool.name())
                );
                continue;
            }
            self.register(tool);
            count += 1;
        }
        if count > 0 {
            tracing::info!(
                "Loaded {count} plugin tool(s) from {}",
                plugin_dir.display()
            );
        }
    }
}

/// The complete model-visible and dispatchable surface for a machine or role
/// whose contract is evidence collection without project/process mutation.
pub(crate) fn readonly_evidence_tool_name(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "bash"
            | "File"
            | "Bash"
            | "Web"
            | "web.run"
            | "load_skill"
            | "handle_read"
            | "retrieve_tool_result"
            | "todo_write"
    )
}

/// True when a concrete registered tool is safe on the read-only evidence
/// surface. Static read-only capability is sufficient except for Git/review:
/// those may invoke repository-configured helpers, so their safety cannot be
/// proven from the tool declaration alone. Scouts can still inspect Git through
/// classifier-bounded lowercase `bash` commands.
pub(crate) fn readonly_evidence_tool(tool: &dyn ToolSpec) -> bool {
    readonly_evidence_tool_name(tool.name())
        || !matches!(tool.name(), "Git" | "review") && tool.is_read_only()
}

fn project_readonly_evidence_schema(name: &str, schema: &mut Value) {
    if name == "Bash" {
        *schema = super::shell::readonly_bash_input_schema();
        return;
    }
    if name == "Run" {
        // The shared classifier remains authoritative for `args`; the schema
        // removes the only field that can name verifier programs.
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            properties.remove("commands");
        }
        return;
    }
    // Probe with `pointer_mut`, never `schema["properties"]["action"]["enum"]`:
    // serde_json's IndexMut auto-vivifies missing keys by inserting Null, so
    // the old probe left `properties.action = {"enum": null}` inside schemas
    // that have no action property (e.g. lowercase `bash`). Strict
    // OpenAI-compatible validators then reject the whole request with
    // `Invalid schema for function 'bash': null is not of type "array"`
    // (observed on Fleet read-only workers; see registry tests).
    // The same probe idiom lives in `tools/subagent` (grep `pointer_mut(
    // "/properties/action/enum")`); keep the two sites greppable as one.
    let Some(actions) = schema
        .pointer_mut("/properties/action/enum")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    match name {
        "File" => actions.retain(|action| {
            action.as_str().is_some_and(|action| {
                matches!(action, "read" | "list" | "search_name" | "search_content")
            })
        }),
        "Web" => actions.retain(|action| {
            action
                .as_str()
                .is_some_and(|action| matches!(action, "search" | "fetch"))
        }),
        _ => {}
    }
}

pub(crate) fn enforce_tool_authority(
    name: &str,
    input: &Value,
    tool: &dyn ToolSpec,
    context: &ToolContext,
) -> Result<(), ToolError> {
    crate::core::engine::tool_catalog::enforce_tool_denial(context, name, input)?;
    if let Some(mode) = context.acp_host {
        let canonical = super::canonical_action::canonical_action_alias(name, input);
        if matches!(name, "bash" | "Bash" | "exec_shell") || canonical.starts_with("exec_shell") {
            let prepared = tool.prepare(input.clone(), context)?;
            if context.shell_policy == crate::worker_profile::ShellPolicy::None
                || prepared
                    .input
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("run")
                    != "run"
                || prepared.starts_detached
                || prepared.input.get("interactive").and_then(Value::as_bool) == Some(true)
                || prepared.input.get("persist").and_then(Value::as_bool) == Some(true)
                || prepared
                    .input
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(super::shell::foreground_command_requests_detach)
            {
                return Err(ToolError::permission_denied(
                    "ACP supports only admitted foreground shell commands; stateful and detached execution is unavailable",
                ));
            }
        }
        // The transport has no controls for these lifecycles. A fabricated
        // tool name or alias cannot turn catalog omission into authority.
        if !matches!(
            tool.name(),
            "read"
                | "write"
                | "edit"
                | "File"
                | "read_file"
                | "write_file"
                | "edit_file"
                | "list_dir"
                | "file_search"
                | "grep_files"
                | "Git"
                | "apply_patch"
                | "bash"
                | "Bash"
        ) {
            return Err(ToolError::not_available(format!(
                "{name} is outside the ACP foreground tool profile"
            )));
        }
        if mode == codewhale_config::AppMode::Plan
            && !tool.prepare(input.clone(), context)?.read_only
        {
            return Err(ToolError::permission_denied(
                "ACP Plan mode permits only read-only calls",
            ));
        }
    }

    let Some(authority) = context.tool_authority.as_ref() else {
        return Ok(());
    };
    let evidence_only = authority.authority == super::spec::ToolMutationAuthority::ReadOnly;
    let bounded_verifier = evidence_only
        && name == "Run"
        && authority.verification == super::spec::ToolVerificationAuthority::Bounded;
    if evidence_only && !readonly_evidence_tool(tool) && !bounded_verifier {
        return Err(ToolError::permission_denied(format!(
            "worker '{}' cannot run {name}: it is outside the read-only evidence tool profile",
            authority.owner
        )));
    }
    if evidence_only && matches!(name, "Web" | "web.run") && authority.network_access != Some(true)
    {
        return Err(ToolError::permission_denied(format!(
            "worker '{}' cannot run {name}: its authority envelope does not grant network access",
            authority.owner
        )));
    }
    let capabilities = tool.capabilities();
    if matches!(name, "bash" | "Bash" | "exec_shell") {
        // One authority (#6015): a durable worker's shell is judged by the
        // same agent read-only grammar and input normalization as in-session
        // agents (`agent_readonly_bash_verdict`), and `BashTool::execute`
        // applies it again under the clamped `ShellPolicy::ReadOnly`. The
        // parent's parallel/approval classification (`is_read_only_for`) is
        // not an authority here.
        let verdict = super::shell::agent_readonly_bash_verdict(input);
        if authority.shell != crate::tools::spec::ToolShellAuthority::ReadOnly {
            return Err(ToolError::permission_denied(if verdict.is_ok() {
                format!(
                    "worker '{}' cannot run {name}: its machine-readable authority envelope does not grant read-only shell access",
                    authority.owner
                )
            } else {
                format!(
                    "worker '{}' cannot run {name}: arbitrary command execution is outside its machine-readable authority envelope. {}",
                    authority.owner,
                    codewhale_execpolicy::command_safety::readonly_command_help()
                )
            }));
        }
        if let Err(rejection) = verdict {
            return Err(ToolError::permission_denied(format!(
                "worker '{}' cannot run {name}: {}",
                authority.owner,
                super::shell::readonly_refusal(
                    &rejection,
                    super::shell::readonly_enforced_lane_available(context)
                )
            )));
        }
        let hosts = input
            .get("command")
            .and_then(Value::as_str)
            .map(codewhale_execpolicy::command_safety::readonly_network_reads)
            .unwrap_or_default()
            .into_iter()
            .map(codewhale_execpolicy::command_safety::NetworkRead::host)
            .collect::<Vec<_>>();
        if !hosts.is_empty() && authority.network_access != Some(true) {
            return Err(ToolError::permission_denied(format!(
                "worker '{}' cannot use read-only network access to {}: its machine-readable authority envelope does not grant network access",
                authority.owner,
                hosts.join(", ")
            )));
        }
        return Ok(());
    }
    if name == "Run" {
        if bounded_verifier {
            use crate::tools::execution_envelope::{VerificationBound, classify_verification};

            let canonical = crate::tools::canonical_action::canonical_action_alias(name, input);
            if matches!(
                classify_verification(canonical, input),
                Some(VerificationBound::Default | VerificationBound::Filter)
            ) {
                return Ok(());
            }
            return Err(ToolError::permission_denied(format!(
                "worker '{}' cannot run unbounded verification arguments or commands. Re-run the default gate instead: drop `commands` (run_verifiers) and any flag that can redirect what runs (run_tests `args` may only select tests), and report the blocked probe to the parent rather than working around it.",
                authority.owner
            )));
        }
        return Err(ToolError::permission_denied(format!(
            "worker '{}' cannot run {name}: arbitrary command execution is outside its machine-readable authority envelope. {}",
            authority.owner,
            codewhale_execpolicy::command_safety::readonly_command_help()
        )));
    }
    if name == "Git" || name.starts_with("git_") || name == "review" {
        return Err(ToolError::permission_denied(format!(
            "worker '{}' cannot run {name}: repository-configured Git helpers cannot prove read-only execution under its machine-readable authority envelope",
            authority.owner
        )));
    }
    if tool.is_read_only_for(input) {
        return Ok(());
    }
    if capabilities.contains(&ToolCapability::ExecutesCode) {
        return Err(ToolError::permission_denied(format!(
            "worker '{}' cannot run {name}: code or child execution is outside its machine-readable authority envelope",
            authority.owner
        )));
    }
    if let Some(paths) = authority_mutation_paths(name, input)? {
        if paths.is_empty() {
            return Err(ToolError::permission_denied(format!(
                "worker '{}' mutation through {name} did not expose a bounded file target",
                authority.owner
            )));
        }
        for path in paths {
            if !authority.permits_mutation_path(context, &path)? {
                return Err(ToolError::permission_denied(format!(
                    "worker '{}' cannot mutate '{path}' outside its machine-readable authority envelope",
                    authority.owner
                )));
            }
        }
        return Ok(());
    }
    Err(ToolError::permission_denied(format!(
        "worker '{}' cannot run mutating tool {name}: the call has no authorized file target",
        authority.owner
    )))
}

fn authority_mutation_paths(name: &str, input: &Value) -> Result<Option<Vec<String>>, ToolError> {
    let canonical = crate::tools::canonical_action::canonical_action_alias(name, input);
    let is_patch = canonical == "apply_patch"
        || (name == "File" && input.get("action").and_then(Value::as_str) == Some("patch"));
    if is_patch {
        let mut patch_input = input.clone();
        if let Some(object) = patch_input.as_object_mut() {
            object.remove("action");
        }
        let paths = crate::tools::apply_patch::preflight_apply_patch(&patch_input)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?
            .touched_files;
        return Ok(Some(paths));
    }
    let path_bound = matches!(canonical, "write_file" | "edit_file" | "fim_edit")
        || (name == "File"
            && input
                .get("action")
                .and_then(Value::as_str)
                .is_some_and(|action| matches!(action, "write" | "edit")))
        || (name == "pandoc_convert" && input.get("output_path").is_some());
    if !path_bound {
        return Ok(None);
    }
    Ok(Some(
        input
            .get("path")
            .or_else(|| input.get("output_path"))
            .and_then(Value::as_str)
            .map(|path| vec![path.to_string()])
            .unwrap_or_default(),
    ))
}

/// Builder for constructing a `ToolRegistry` with common tools.
pub struct ToolRegistryBuilder {
    tools: Vec<Arc<dyn ToolSpec>>,
}

/// Feature/config-dependent native Agent-mode tool surface.
///
/// Parent Agent/Yolo turns and default child sub-agents both build through this
/// options object so the catalog does not drift as new first-party tools are
/// gated behind feature flags or config state.
#[derive(Clone)]
pub struct AgentToolSurfaceOptions {
    pub shell_policy: crate::worker_profile::ShellPolicy,
    pub apply_patch_enabled: bool,
    pub web_search_enabled: bool,
    pub memory_tool_enabled: bool,
    pub vision_config: Option<crate::config::VisionModelConfig>,
    pub speech_output_dir: Option<PathBuf>,
    pub goal_state: Option<SharedGoalState>,
    /// Register the agent-callable `verify` self-critique tool (#4196).
    /// Gated by `Feature::Verify` (`[features] verify_tool`), default on.
    pub verify_tool_enabled: bool,
    /// `request_user_input` payload ceilings from `[tools]` (#5949). Carried on
    /// the surface options so model-spawned children inherit the parent's
    /// configured limits instead of silently falling back to the defaults.
    pub user_input_limits: super::user_input::UserInputLimits,
    /// Register `request_plugin_install`. The engine turns this off outside
    /// the interactive TUI and when contextual tips are off (0.10.1 plugin
    /// offering policy, rules 3 and 11); children inherit the parent's value.
    pub request_plugin_install_enabled: bool,
}

impl AgentToolSurfaceOptions {
    #[must_use]
    pub fn new(shell_policy: crate::worker_profile::ShellPolicy) -> Self {
        Self {
            shell_policy,
            apply_patch_enabled: false,
            web_search_enabled: false,
            memory_tool_enabled: false,
            vision_config: None,
            speech_output_dir: None,
            goal_state: None,
            verify_tool_enabled: true,
            user_input_limits: super::user_input::UserInputLimits::default(),
            request_plugin_install_enabled: true,
        }
    }
}

impl ToolRegistryBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Add a custom tool.
    #[must_use]
    pub fn with_tool(mut self, tool: Arc<dyn ToolSpec>) -> Self {
        // A later builder step that supplies an existing name is an intended
        // upgrade (`with_patch_tools` swaps the default `File` for the
        // patch-capable one), so replace in place. `ToolRegistry::register`
        // keeps warning about the collisions that are not planned (#5934).
        let name = tool.name().to_string();
        if let Some(slot) = self
            .tools
            .iter_mut()
            .find(|existing| existing.name() == name)
        {
            *slot = tool;
        } else {
            self.tools.push(tool);
        }
        self
    }

    /// Add client-executed runtime tools. A dynamic tool never replaces a
    /// tool already in the builder (#6559 D04-10): `with_tool` treats a
    /// repeated name as a planned upgrade, which let a client's `exec_shell`
    /// or `read` silently take over the builtin handler and its approval
    /// policy. The model-facing name is the bare `name`, so a second dynamic
    /// tool with the same name in another namespace is refused the same way.
    /// Both refusals are logged with the two origins.
    ///
    /// Known limitation: the refusal reaches the log, not the runtime client
    /// that sent the spec; rejecting it at the API boundary, and exposing a
    /// namespaced model-facing name, are protocol changes.
    #[must_use]
    pub fn with_dynamic_tools(mut self, dynamic_tools: &[DynamicToolSpec]) -> Self {
        for spec in dynamic_tools {
            let tool: Arc<dyn ToolSpec> =
                Arc::new(super::dynamic::RuntimeDynamicTool::new(spec.clone()));
            if let Some(existing) = self
                .tools
                .iter()
                .find(|existing| existing.name() == tool.name())
            {
                tracing::warn!(
                    existing_origin = ?existing.registration_origin(),
                    refused_origin = ?tool.registration_origin(),
                    "Refusing runtime dynamic tool that collides with a registered tool: {}",
                    crate::safe_label::SafeLabel::identifier(tool.name())
                );
                continue;
            }
            self.tools.push(tool);
        }
        self
    }

    /// Include file tools (read, write, edit, list).
    #[must_use]
    pub fn with_file_tools(self) -> Self {
        use super::file::{EditFileTool, ListDirTool, ReadFileTool, WriteFileTool};
        use super::file_tool::{EditTool, FileTool, ReadTool, WriteTool};
        self.with_tool(Arc::new(ReadTool))
            .with_tool(Arc::new(WriteTool))
            .with_tool(Arc::new(EditTool))
            // Compatibility-only execution names for saved transcripts and
            // protocol clients. `model_visible=false` keeps them out of new
            // catalogs.
            .with_tool(Arc::new(FileTool::new("File")))
            .with_tool(Arc::new(ReadFileTool))
            .with_tool(Arc::new(WriteFileTool))
            .with_tool(Arc::new(EditFileTool))
            .with_tool(Arc::new(ListDirTool))
    }

    /// Include only read-only file tools (read, list).
    #[must_use]
    #[cfg(test)]
    pub fn with_read_only_file_tools(self) -> Self {
        use super::file::{ListDirTool, ReadFileTool};
        use super::file_tool::FileTool;
        use super::file_tool::ReadTool;
        self.with_tool(Arc::new(ReadTool))
            .with_tool(Arc::new(FileTool::read_only("File")))
            .with_tool(Arc::new(ReadFileTool))
            .with_tool(Arc::new(ListDirTool))
            .with_tool(Arc::new(
                super::tool_result_retrieval::RetrieveToolResultTool,
            ))
    }

    /// Include shell execution tools.
    ///
    /// New turns expose lowercase `bash`; uppercase `Bash` remains a hidden
    /// compatibility name for saved v0.9.x transcripts.
    #[must_use]
    pub fn with_shell_tools(self) -> Self {
        self.with_foreground_shell_tools().with_terminal_tools()
    }

    /// Include only the cancellable foreground shell tool.
    ///
    /// Protocol hosts that cannot safely own a persistent PTY session use
    /// this surface instead of [`Self::with_shell_tools`].
    #[must_use]
    pub fn with_foreground_shell_tools(self) -> Self {
        use super::shell::{BashTool, LowercaseBashTool};
        self.with_tool(Arc::new(LowercaseBashTool))
            .with_tool(Arc::new(BashTool::new("Bash")))
    }

    /// Include only the foreground, direct-argv read-only shell surface.
    #[must_use]
    pub fn with_read_only_shell_tool(self) -> Self {
        use super::shell::{BashTool, LowercaseBashTool};
        self.with_tool(Arc::new(LowercaseBashTool))
            .with_tool(Arc::new(BashTool::read_only("Bash")))
    }

    /// Include the stateful PTY terminal tools. Like `exec_shell`, these are
    /// only exposed when the active shell policy allows shell access.
    #[cfg(not(target_env = "ohos"))]
    #[must_use]
    pub fn with_terminal_tools(self) -> Self {
        use super::terminal_session::{
            TerminalCancelTool, TerminalResetTool, TerminalRunTool, TerminalSendTool,
            TerminalWaitTool,
        };
        self.with_tool(Arc::new(TerminalRunTool))
            .with_tool(Arc::new(TerminalSendTool))
            .with_tool(Arc::new(TerminalWaitTool))
            .with_tool(Arc::new(TerminalCancelTool))
            .with_tool(Arc::new(TerminalResetTool))
    }

    /// OpenHarmony does not include the `portable-pty` dependency, so keep the
    /// ordinary shell tools without advertising unavailable persistent PTYs.
    #[cfg(target_env = "ohos")]
    #[must_use]
    pub fn with_terminal_tools(self) -> Self {
        self
    }

    /// Search is part of the canonical `File` action surface.
    #[must_use]
    pub fn with_search_tools(self) -> Self {
        self.with_tool(Arc::new(super::file_search::FileSearchTool))
            .with_tool(Arc::new(super::search::GrepFilesTool))
    }

    /// Include the canonical `Git` inspection/history surface.
    #[must_use]
    pub fn with_git_tools(self) -> Self {
        use super::git_tool::GitTool;
        self.with_tool(Arc::new(GitTool::new("Git")))
    }

    /// Git history is part of the canonical `Git` action surface.
    #[must_use]
    pub fn with_git_history_tools(self) -> Self {
        self
    }

    /// Include workspace diagnostics tool.
    #[must_use]
    pub fn with_diagnostics_tool(self) -> Self {
        use super::diagnostics::DiagnosticsTool;
        self.with_tool(Arc::new(DiagnosticsTool))
    }

    /// Include the `tui_help` command/keybinding reference (#1708). The
    /// catalog it reads is compiled in, so there is nothing to probe.
    #[must_use]
    pub fn with_tui_help_tool(self) -> Self {
        use super::tui_help::TuiHelpTool;
        self.with_tool(Arc::new(TuiHelpTool))
    }

    /// Include the `pandoc_convert` tool only when the `pandoc`
    /// binary is present on this host. Same probe-then-decide
    /// pattern v0.8.31 introduced for Python — when pandoc is
    /// missing the tool is not registered, so the model never
    /// sees a binary it can't actually use.
    #[must_use]
    pub fn with_pandoc_tools(self) -> Self {
        if crate::dependencies::host_tool_available("pandoc_convert", || {
            crate::dependencies::resolve_pandoc().is_some()
        }) {
            use super::pandoc::PandocConvertTool;
            self.with_tool(Arc::new(PandocConvertTool))
        } else {
            self
        }
    }

    /// Include the `image_ocr` tool only when a local OCR backend is present.
    /// macOS uses the built-in Vision framework, while other platforms use
    /// Tesseract when installed.
    #[must_use]
    pub fn with_image_ocr_tools(self) -> Self {
        if crate::dependencies::host_tool_available("image_ocr", super::image_ocr::ocr_available) {
            use super::image_ocr::ImageOcrTool;
            self.with_tool(Arc::new(ImageOcrTool))
        } else {
            self
        }
    }

    /// Include the `read_media` tool for safe multimodal media inspection.
    #[must_use]
    pub fn with_read_media_tool(self) -> Self {
        use super::read_media::ReadMediaTool;
        self.with_tool(Arc::new(ReadMediaTool))
    }

    /// Include the `load_skill` tool (#434) so the model can pull a
    /// SKILL.md body + companion file list into context with one
    /// call instead of `read_file` + `list_dir` against the path
    /// shown in the system prompt's `## Skills` section.
    #[must_use]
    pub fn with_skill_tools(self) -> Self {
        use super::skill::LoadSkillTool;
        self.with_tool(Arc::new(LoadSkillTool))
    }

    /// Include project mapping tools.
    #[must_use]
    pub fn with_project_tools(self) -> Self {
        use super::project::ProjectMapTool;
        self.with_tool(Arc::new(ProjectMapTool))
    }

    /// Include cargo test runner tool.
    #[must_use]
    pub fn with_test_runner_tool(self) -> Self {
        use super::run_tool::RunTool;
        self.with_tool(Arc::new(RunTool::new("Run")))
    }

    /// Include structured data validation tool (`validate_data`).
    #[must_use]
    pub fn with_validation_tools(self) -> Self {
        use super::validate_data::ValidateDataTool;
        self.with_tool(Arc::new(ValidateDataTool))
    }

    /// Include retrieval for spilled historical tool results.
    #[must_use]
    pub fn with_tool_result_retrieval_tool(self) -> Self {
        use super::tool_result_retrieval::RetrieveToolResultTool;
        self.with_tool(Arc::new(RetrieveToolResultTool))
    }

    /// Include durable task, gate, PR-attempt, GitHub, and automation tools.
    ///
    /// Each family is one tool with an `action` parameter (`tasks`, `github`,
    /// `automation`). Per-action execution aliases were removed in v0.9.3.
    ///
    /// The shell-related task tool (`task_shell_wait`) is *not* included here —
    /// use `with_runtime_task_shell_tools` to register it when `allow_shell`
    /// is true.
    #[must_use]
    pub fn with_runtime_task_tools(self) -> Self {
        use super::automation::AutomationTool;
        use super::github::GithubTool;
        use super::send_later::SendLaterTool;
        use super::tasks::TasksTool;

        self.with_tool(Arc::new(TasksTool::new("tasks")))
            .with_tool(Arc::new(GithubTool::new("github")))
            .with_tool(Arc::new(AutomationTool::new("automation")))
            .with_tool(Arc::new(SendLaterTool::new("send_later")))
    }

    /// Include the shell-related task tool (`task_shell_wait`).
    ///
    /// Gated behind `allow_shell` because it polls and cancels the background
    /// jobs that `Bash` starts; background work has one entry point, the shell
    /// tool's `background=true`.
    #[must_use]
    pub fn with_runtime_task_shell_tools(self) -> Self {
        use super::tasks::TaskShellWaitTool;
        self.with_tool(Arc::new(TaskShellWaitTool))
    }

    /// Include only read-only durable task, PR-attempt, GitHub, and automation
    /// inspection tools. Plan mode uses this surface so it can observe state
    /// without starting work, changing remotes, or mutating automation config.
    ///
    /// The model sees the same canonical `tasks` / `github` / `automation` /
    /// `send_later` tools as the full surface, restricted to their read-only
    /// actions.
    #[must_use]
    pub fn with_runtime_read_only_task_tools(self) -> Self {
        use super::automation::AutomationTool;
        use super::github::GithubTool;
        use super::send_later::SendLaterTool;
        use super::tasks::TasksTool;

        self.with_tool(Arc::new(TasksTool::read_only("tasks")))
            .with_tool(Arc::new(GithubTool::read_only("github")))
            .with_tool(Arc::new(AutomationTool::read_only("automation")))
            .with_tool(Arc::new(SendLaterTool::read_only("send_later")))
    }

    /// Include web search and fetch tools.
    ///
    /// These are feature-gated behind `Feature::WebSearch` in `tool_setup.rs`.
    /// `finance` is registered separately via `with_finance_tool()` and is
    /// NOT gated behind the web-search feature.
    #[must_use]
    pub fn with_web_tools(self) -> Self {
        use super::web_run::WebRunTool;
        use super::web_tool::WebTool;
        self.with_tool(Arc::new(WebTool::new("Web")))
            .with_tool(Arc::new(WebRunTool))
    }

    /// Include the `finance` market-data tool.
    ///
    /// This tool is registered unconditionally for agent modes and is NOT
    /// gated behind `Feature::WebSearch` (it fetches financial data, not
    /// web search results).
    #[must_use]
    pub fn with_finance_tool(self) -> Self {
        use super::finance::FinanceTool;
        self.with_tool(Arc::new(FinanceTool::new()))
    }

    /// Register the `image_analyze` vision tool.
    /// Only registered when `[vision_model]` is configured in config.toml.
    #[must_use]
    pub fn with_vision_tools(
        self,
        config: crate::config::VisionModelConfig,
        route_client: Option<CodewhaleClient>,
    ) -> Self {
        use crate::vision::tools::ImageAnalyzeTool;
        self.with_tool(Arc::new(ImageAnalyzeTool::new_with_route_client(
            config,
            route_client,
        )))
    }

    /// Include request_user_input tool under the session's configured payload
    /// ceilings (`[tools] user_input_max_questions` / `user_input_max_options`,
    /// #5949). The limits ride the tool instance so the validator, the JSON
    /// schema, and the model-visible description cannot drift apart.
    #[must_use]
    pub fn with_user_input_tool(self, limits: super::user_input::UserInputLimits) -> Self {
        use super::user_input::RequestUserInputTool;
        self.with_tool(Arc::new(RequestUserInputTool::new(limits)))
    }

    /// Include patch tools (`apply_patch`).
    #[must_use]
    pub fn with_patch_tools(self) -> Self {
        use super::file_tool::FileTool;
        self.with_tool(Arc::new(FileTool::with_patch("File")))
            .with_tool(Arc::new(super::apply_patch::ApplyPatchTool))
    }

    /// Include the `revert_turn` tool. Approval-gated since it mutates
    /// the workspace; the model uses it when the user asks to "undo my
    /// last edit". Backed by the per-workspace snapshot side-repo
    /// (`crate::snapshot`).
    #[must_use]
    pub fn with_revert_turn_tool(self) -> Self {
        use super::revert_turn::RevertTurnTool;
        self.with_tool(Arc::new(RevertTurnTool))
    }

    /// Include the speech/TTS tool: `speech` is model-visible, `tts` is a
    /// hidden compat alias for saved-transcript replay (#5941).
    #[must_use]
    pub fn with_speech_tools(
        self,
        client: Option<CodewhaleClient>,
        output_dir: Option<PathBuf>,
    ) -> Self {
        use super::speech::SpeechTool;
        self.with_tool(Arc::new(SpeechTool::new(
            "speech",
            client.clone(),
            output_dir.clone(),
        )))
        .with_tool(Arc::new(SpeechTool::alias("tts", client, output_dir)))
    }

    /// Include the canonical persistent RLM session tool.
    #[must_use]
    pub fn with_rlm_tool(self) -> Self {
        use super::rlm::RlmTool;
        self.with_tool(Arc::new(RlmTool::new(super::rlm::RLM_TOOL_NAME)))
    }

    /// Include the persistent, project-scoped continual-harness controller.
    #[must_use]
    pub fn with_harness_tool(self) -> Self {
        use super::harness::HarnessTool;
        self.with_tool(Arc::new(HarnessTool))
    }

    /// Include `handle_read`, the bounded projection reader for symbolic
    /// `var_handle` payloads.
    #[must_use]
    pub fn with_handle_tools(self) -> Self {
        use super::handle::HandleReadTool;
        self.with_tool(Arc::new(HandleReadTool))
    }

    /// Include the review tool.
    #[must_use]
    pub fn with_review_tool(self, client: Option<CodewhaleClient>, model: String) -> Self {
        use super::review::ReviewTool;
        self.with_tool(Arc::new(ReviewTool::new(client, model)))
    }

    /// Include the agent-callable `verify` self-critique tool (#4196). The
    /// critic runs at elevated reasoning (default `Max`) independent of the
    /// session tier and is given no tools, so it cannot recurse into `verify`.
    #[must_use]
    pub fn with_verify_tool(self, client: Option<CodewhaleClient>, model: String) -> Self {
        use super::verify::VerifyTool;
        self.with_tool(Arc::new(VerifyTool::new(client, model)))
    }

    /// Include note tool.
    #[must_use]
    pub fn with_note_tool(self) -> Self {
        use super::shell::NoteTool;
        self.with_tool(Arc::new(NoteTool))
    }

    /// Include the FIM (Fill-in-the-Middle) edit tool.
    #[must_use]
    pub fn with_fim_tool(self, client: Option<CodewhaleClient>, model: String) -> Self {
        use super::fim::FimEditTool;
        let Some(client) = client.filter(CodewhaleClient::can_request_fim_completion) else {
            return self;
        };
        self.with_tool(Arc::new(FimEditTool::new(Some(client), model)))
    }

    /// Include the `remember` tool — model-callable bullet-add into the
    /// user memory file (#489). Only register when the user has opted
    /// in to the memory feature; without that, the tool would surface
    /// in the model's catalog but always fail with "memory disabled".
    #[must_use]
    pub fn with_remember_tool(self) -> Self {
        use super::remember::RememberTool;
        self.with_tool(Arc::new(RememberTool))
    }

    /// Include the native-memory retrieval tools alongside reviewed capture.
    #[must_use]
    pub fn with_native_memory_tools(self) -> Self {
        use super::native_memory::{MemoryGetTool, MemorySearchTool};
        self.with_tool(Arc::new(MemorySearchTool))
            .with_tool(Arc::new(MemoryGetTool))
    }

    /// Include the prior-session recall tools (#5715). Always-on: they are
    /// read-only and workspace-scoped, so there is no opt-in to honor.
    #[must_use]
    pub fn with_session_recall_tools(self) -> Self {
        use super::session::{SessionGetTool, SessionSearchTool};
        self.with_tool(Arc::new(SessionSearchTool))
            .with_tool(Arc::new(SessionGetTool))
    }

    /// Include the model-facing LSP intelligence tools. They reuse the
    /// session [`crate::lsp::LspManager`] attached to `ToolContext` and never
    /// spawn a second server lifecycle.
    #[must_use]
    pub fn with_lsp_tool(self) -> Self {
        use super::lsp::LspTool;
        self.with_tool(Arc::new(LspTool))
    }

    /// Include the `notify` tool — model-callable desktop notification
    /// (#1322). Routes through the existing `tui::notifications` OSC 9 /
    /// BEL pipeline so the user's `[notifications].method` config is
    /// honoured automatically (including `off`). Always safe to register
    /// because the tool has no side effects beyond a single terminal
    /// escape write.
    #[must_use]
    pub fn with_notify_tool(self) -> Self {
        use super::notify::NotifyTool;
        self.with_tool(Arc::new(NotifyTool))
    }

    /// Include `request_plugin_install` — model-callable review request.
    /// Never installs; the TUI surfaces `/plugin trust` or catalog install
    /// for the human.
    #[must_use]
    pub fn with_request_plugin_install_tool(self) -> Self {
        use super::request_plugin_install::RequestPluginInstallTool;
        self.with_tool(Arc::new(RequestPluginInstallTool))
    }

    /// Include MCP tools from a connected pool as first-class registry
    /// citizens. Each MCP tool is wrapped in a lightweight adapter that
    /// implements `ToolSpec`, so the unified `ToolRegistryBuilder` flow
    /// handles them alongside native tools.
    ///
    /// MCP tools are marked `defer_loading` by default (except discovery
    /// helpers) to keep the model-visible catalog compact.
    #[must_use]
    pub fn with_mcp_tools(
        mut self,
        mcp_pool: std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>,
    ) -> Self {
        // Snapshot the current tool list from the pool (non-blocking).
        // The adapter lazily resolves at execution time via the pool.
        if let Ok(pool) = mcp_pool.try_lock() {
            let tool_servers = pool.resolved_tool_servers();
            for (name, tool) in pool.all_tools() {
                let adapter = Arc::new(McpToolAdapter {
                    server_name: tool_servers.get(&name).cloned(),
                    name: name.clone(),
                    tool: tool.clone(),
                    pool: mcp_pool.clone(),
                });
                self.tools.push(adapter);
            }
        }
        self
    }

    /// Register the `start_mcp_server` tool for dynamically adding MCP servers
    /// from conversation context. Does not register MCP tool adapters — those
    /// are returned by `pool.to_api_tools()` in `engine.mcp_tools()`.
    #[must_use]
    pub fn with_runtime_mcp_tool(
        mut self,
        mcp_pool: std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>,
    ) -> Self {
        self.tools
            .push(Arc::new(super::runtime_mcp::StartRuntimeMcpServer::new(
                mcp_pool,
            )));
        self
    }

    /// Register the `registry_sync` tool for fetching and caching
    /// MCP Registry server metadata.
    #[must_use]
    pub fn with_registry_mcp_sync_tool(mut self) -> Self {
        self.tools
            .push(Arc::new(super::mcp_registry::McpSyncRegistry::new()));
        self
    }

    /// Register the structured Registry launcher. Unlike `start_mcp_server`,
    /// this accepts no free-form command and can only launch cached,
    /// zero-environment stdio candidates.
    #[must_use]
    pub fn with_registry_mcp_start_tool(
        mut self,
        mcp_pool: std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>,
    ) -> Self {
        self.tools
            .push(Arc::new(super::mcp_registry::StartRegistryMcpServer::new(
                mcp_pool,
            )));
        self
    }

    /// Include all agent tools under a typed shell policy.
    #[must_use]
    pub fn with_agent_tools_policy(
        self,
        shell_policy: crate::worker_profile::ShellPolicy,
        user_input_limits: super::user_input::UserInputLimits,
    ) -> Self {
        let builder = self
            .with_file_tools()
            .with_note_tool()
            .with_search_tools()
            .with_user_input_tool(user_input_limits)
            .with_git_tools()
            .with_git_history_tools()
            .with_diagnostics_tool()
            .with_tui_help_tool()
            .with_lsp_tool()
            .with_project_tools()
            .with_skill_tools()
            .with_test_runner_tool()
            .with_validation_tools()
            .with_tool_result_retrieval_tool()
            .with_handle_tools()
            .with_runtime_task_tools()
            .with_revert_turn_tool()
            .with_pandoc_tools()
            .with_image_ocr_tools()
            .with_read_media_tool()
            .with_finance_tool();

        match shell_policy {
            crate::worker_profile::ShellPolicy::Full => {
                builder.with_shell_tools().with_runtime_task_shell_tools()
            }
            crate::worker_profile::ShellPolicy::ReadOnly => builder.with_read_only_shell_tool(),
            crate::worker_profile::ShellPolicy::None => builder,
        }
    }

    /// Include the native Agent-mode surface shared by the parent runtime and
    /// default child sub-agents, excluding the `agent` launcher itself.
    #[must_use]
    pub fn with_agent_runtime_surface(
        self,
        client: Option<CodewhaleClient>,
        model: String,
        options: AgentToolSurfaceOptions,
        todo_list: super::todo::SharedTodoList,
        plan_state: super::plan::SharedPlanState,
    ) -> Self {
        let speech_client = client.clone();
        let vision_client = client.clone();
        let verify_client = client.clone();
        let verify_model = model.clone();
        let mut builder = self
            .with_agent_tools_policy(options.shell_policy, options.user_input_limits)
            .with_todo_tool(todo_list)
            .with_plan_tool(plan_state)
            .with_review_tool(client.clone(), model.clone())
            .with_rlm_tool()
            .with_harness_tool()
            .with_fim_tool(client, model);

        // No client means no speech provider to call: do not advertise a
        // capability the session cannot deliver (#5941).
        if speech_client.is_some() {
            builder = builder.with_speech_tools(speech_client, options.speech_output_dir.clone());
        }

        if options.verify_tool_enabled {
            builder = builder.with_verify_tool(verify_client, verify_model);
        }
        if let Some(goal_state) = options.goal_state {
            builder = builder.with_goal_tools(goal_state);
        }
        if options.apply_patch_enabled {
            builder = builder.with_patch_tools();
        }
        if options.web_search_enabled {
            builder = builder.with_web_tools();
        }
        if options.memory_tool_enabled {
            builder = builder.with_remember_tool().with_native_memory_tools();
        }
        if let Some(vision_config) = options.vision_config {
            builder = builder.with_vision_tools(vision_config, vision_client);
        }

        builder = builder.with_notify_tool();
        if options.request_plugin_install_enabled {
            builder = builder.with_request_plugin_install_tool();
        }
        builder.with_session_recall_tools()
    }

    /// Include the full child-inherited Agent surface under resolved
    /// feature/config options.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_full_agent_surface_options(
        self,
        client: Option<CodewhaleClient>,
        model: String,
        manager: super::subagent::SharedSubAgentManager,
        runtime: super::subagent::SubAgentRuntime,
        options: AgentToolSurfaceOptions,
        todo_list: super::todo::SharedTodoList,
        plan_state: super::plan::SharedPlanState,
    ) -> Self {
        self.with_agent_runtime_surface(client, model, options, todo_list, plan_state)
            .with_subagent_tools(manager, runtime)
    }

    /// Include the canonical work-progress tool with a shared `TodoList`.
    /// Canonical is `todo_write`; `work_update`/`TodoWrite`/`todo` are hidden
    /// compat aliases (not model-visible) for saved-transcript replay.
    #[must_use]
    pub fn with_todo_tool(self, todo_list: super::todo::SharedTodoList) -> Self {
        use super::todo::TodoWriteTool;
        self.with_tool(Arc::new(TodoWriteTool::new(todo_list.clone())))
            .with_tool(Arc::new(TodoWriteTool::alias(
                "work_update",
                todo_list.clone(),
            )))
            .with_tool(Arc::new(TodoWriteTool::alias(
                "TodoWrite",
                todo_list.clone(),
            )))
            .with_tool(Arc::new(TodoWriteTool::alias("todo", todo_list.clone())))
            .with_tool(Arc::new(TodoWriteTool::alias(
                "checklist_write",
                todo_list.clone(),
            )))
            .with_tool(Arc::new(TodoWriteTool::alias(
                "checklist_update",
                todo_list,
            )))
    }

    /// Include the plan tool with a shared `PlanState`.
    #[must_use]
    pub fn with_plan_tool(self, plan_state: super::plan::SharedPlanState) -> Self {
        use super::plan::UpdatePlanTool;
        self.with_tool(Arc::new(UpdatePlanTool::new(plan_state)))
    }

    /// Include runtime goal tools (`create_goal`, `get_goal`, `update_goal`).
    #[must_use]
    pub fn with_goal_tools(self, goal_state: super::goal::SharedGoalState) -> Self {
        use super::goal::{CreateGoalTool, GetGoalTool, UpdateGoalTool};
        self.with_tool(Arc::new(CreateGoalTool::new(goal_state.clone())))
            .with_tool(Arc::new(GetGoalTool::new(goal_state.clone())))
            .with_tool(Arc::new(UpdateGoalTool::new(goal_state)))
    }

    /// Include sub-agent management tools.
    #[must_use]
    pub fn with_subagent_tools(
        self,
        manager: super::subagent::SharedSubAgentManager,
        runtime: super::subagent::SubAgentRuntime,
    ) -> Self {
        use super::subagent::AgentTool;
        use super::subagent::register_coordination_tools;
        use super::workflow::WorkflowTool;

        let builder = self
            .with_tool(Arc::new(WorkflowTool::new(
                Arc::clone(&manager),
                runtime.clone(),
            )))
            .with_tool(Arc::new(AgentTool::new(
                Arc::clone(&manager),
                runtime.clone(),
            )));
        register_coordination_tools(builder, manager, runtime)
    }

    /// Build the registry with the given context.
    #[must_use]
    pub fn build(self, context: ToolContext) -> ToolRegistry {
        // A route known to be text-only cannot see what `read_media` returns,
        // so it is not offered there (`image_ocr` remains for text in images).
        let blind = context.route_capabilities.image_input
            == codewhale_config::route::CapabilityState::Unsupported;
        let mut registry = ToolRegistry::new(context);
        registry.register_all(
            self.tools
                .into_iter()
                .filter(|tool| !(blind && tool.name() == "read_media"))
                .collect(),
        );
        registry
    }
}

impl Default for ToolRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert CamelCase to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Adapter that wraps an MCP tool definition so it can live in the
/// unified `ToolRegistry` alongside native tools (§5.B).
struct McpToolAdapter {
    name: String,
    /// Diagnostic snapshot from the pool's exact route projection.
    server_name: Option<String>,
    tool: crate::mcp::McpTool,
    pool: std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>,
}

fn is_mcp_read_helper(name: &str) -> bool {
    matches!(
        name,
        "list_mcp_resources"
            | "list_mcp_resource_templates"
            | "mcp_read_resource"
            | "read_mcp_resource"
            | "mcp_get_prompt"
    )
}

#[async_trait::async_trait]
impl ToolSpec for McpToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn registration_origin(&self) -> std::borrow::Cow<'_, str> {
        use crate::safe_label::SafeLabel;
        match &self.server_name {
            Some(server) => format!(
                "MCP server {}, tool {}",
                SafeLabel::identifier(server),
                SafeLabel::identifier(&self.tool.name)
            )
            .into(),
            None => format!(
                "MCP tool {} (server unknown)",
                SafeLabel::identifier(&self.name)
            )
            .into(),
        }
    }

    fn description(&self) -> &str {
        // McpTool.description is Option<String>; fall back to the
        // prefixed name when absent.
        self.tool.description.as_deref().unwrap_or(&self.name)
    }

    fn input_schema(&self) -> Value {
        self.tool.input_schema.clone()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Conservatively treat MCP tools as requiring approval and
        // network access unless they're known discovery helpers.
        if is_mcp_read_helper(&self.name) {
            vec![ToolCapability::ReadOnly]
        } else {
            vec![ToolCapability::Network, ToolCapability::RequiresApproval]
        }
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        if is_mcp_read_helper(&self.name) {
            ApprovalRequirement::Auto
        } else {
            ApprovalRequirement::Required
        }
    }

    fn defer_loading(&self) -> bool {
        // Discovery helpers stay loaded; everything else is deferred.
        !is_mcp_read_helper(&self.name)
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        self.execute_rich(input, context)
            .await
            .map(RichToolResult::into_result)
    }

    async fn execute_rich(
        &self,
        input: Value,
        context: &ToolContext,
    ) -> Result<RichToolResult, ToolError> {
        let mut pool = self.pool.lock().await;
        if let Some(plugins) = context.plugin_registry.as_ref() {
            pool.bind_caller_plugins(Arc::clone(plugins))
                .map_err(|error| ToolError::not_available(error.to_string()))?;
        }
        pool.validate_native_caller(context.plugin_registry.as_deref())
            .map_err(|error| ToolError::not_available(error.to_string()))?;
        let result = pool
            .call_tool_with_disallowed(
                &self.name,
                input,
                &context.disallowed_tools,
                context.human_decision.as_ref(),
            )
            .await
            .map_err(|e| ToolError::execution_failed(format!("MCP tool failed: {e}")))?;
        Ok(mcp_result_to_bounded_rich_tool_result(result))
    }
}

const MCP_IMAGE_TEXT_PLACEHOLDER: &str = "[MCP image payload removed from text output]";

/// Map an MCP `tools/call` result to the provider-neutral rich result used by
/// native tools. Image payloads travel as typed blocks instead of being
/// duplicated into the JSON text as multi-megabyte base64 strings.
///
/// MCP servers signal tool failure with `isError: true` on an otherwise
/// successful JSON-RPC response. Error results keep their text payload
/// verbatim so the model still sees the server's message (#5123-class).
fn mcp_result_to_rich_tool_result(mut result: Value) -> RichToolResult {
    let mut content_blocks = Vec::new();
    if let Some(items) = result.get_mut("content").and_then(Value::as_array_mut) {
        for item in items {
            let Some(object) = item.as_object_mut() else {
                continue;
            };
            if object.get("type").and_then(Value::as_str) != Some("image") {
                continue;
            }

            let mime_type = object
                .get("mimeType")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let data = object.remove("data");
            if data.is_some() {
                object.insert(
                    "data".to_string(),
                    Value::String(MCP_IMAGE_TEXT_PLACEHOLDER.to_string()),
                );
            }
            // Keep malformed image entries in the typed stream with empty
            // fields so the shared rich-result boundary rejects them and
            // emits the same visible omission receipt as invalid base64,
            // unsupported MIME types, oversized images, and extra images.
            // Dropping them here would silently remove the payload before the
            // boundary had anything to count.
            let (mime_type, data) = match (mime_type, data) {
                (Some(mime_type), Some(Value::String(data))) => (mime_type, data),
                _ => (String::new(), String::new()),
            };
            content_blocks.push(ToolResultContentBlock::Image { mime_type, data });
        }
    }

    let content = serde_json::to_string(&result).unwrap_or_else(|_| result.to_string());
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let result = if is_error {
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty())
            .unwrap_or(content);
        ToolResult::error(text)
    } else {
        ToolResult::success(content)
    };
    RichToolResult::with_content_blocks(result, content_blocks)
}

/// Convert and bound an MCP result at the shared direct/parallel execution
/// seam. The registry applies the same boundary to every rich tool; keeping it
/// here too protects the engine's MCP fast path and text-only adapter callers.
pub(crate) fn mcp_result_to_bounded_rich_tool_result(result: Value) -> RichToolResult {
    crate::image_attach::bound_rich_tool_result(mcp_result_to_rich_tool_result(result))
}

#[cfg(test)]
pub(super) fn mcp_tool_adapter_for_test(name: &str) -> Arc<dyn ToolSpec> {
    Arc::new(McpToolAdapter {
        name: name.to_string(),
        server_name: None,
        tool: crate::mcp::McpTool {
            name: name.to_string(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        },
        pool: Arc::new(tokio::sync::Mutex::new(crate::mcp::McpPool::new(
            crate::mcp::McpConfig::default(),
        ))),
    })
}

// === Unit Tests ===

#[cfg(test)]
mod tests;
