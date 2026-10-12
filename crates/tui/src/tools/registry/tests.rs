use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::tempdir;

use crate::config::ToolOverride;
use crate::tools::ToolRegistryBuilder;
use crate::tools::shell::BashTool;
use crate::tools::spec::{
    ApprovalRequirement, ToolAuthorityEnvelope, ToolCapability, ToolContext, ToolError,
    ToolMutationAuthority, ToolResult, ToolSpec, required_str,
};

use super::{
    MCP_IMAGE_TEXT_PLACEHOLDER, ToolRegistry, enforce_tool_authority,
    mcp_result_to_bounded_rich_tool_result, mcp_tool_adapter_for_test,
};

#[tokio::test]
async fn shell_denial_reaches_registry_and_direct_delegation_sinks() {
    use crate::tools::run_tool::RunTool;
    use crate::tools::tasks::TasksTool;
    use crate::tools::terminal_session::{TerminalResetTool, TerminalRunTool, TerminalSendTool};
    use crate::tools::test_runner::RunTestsTool;
    use crate::tools::verifier::RunVerifiersTool;
    let tmp = tempdir().unwrap();
    let mut context = ToolContext::new(tmp.path());
    context.auto_approve = true;
    context.disallowed_tools = vec!["Bash".into()];
    let command = "printf forbidden > denial-canary.txt";
    let cases: Vec<(Arc<dyn ToolSpec>, Value)> = vec![
        (Arc::new(BashTool::new("Bash")), json!({"command":command})),
        (
            Arc::new(BashTool::alias("exec_interact", "interact")),
            json!({"task_id":"missing", "stdin":command, "action":"wait"}),
        ),
        (
            Arc::new(TasksTool::new("tasks")),
            json!({"action":"gate_run", "gate":"custom", "command":command}),
        ),
        (
            Arc::new(TasksTool::alias("task_gate_run", "gate_run")),
            json!({"action":"list", "gate":"custom", "command":command}),
        ),
        (Arc::new(TerminalRunTool), json!({"command":command})),
        (
            Arc::new(TerminalSendTool),
            json!({"session":"missing", "text":command}),
        ),
        (Arc::new(TerminalResetTool), json!({"session":"missing"})),
        (
            Arc::new(RunTool::new("Run")),
            json!({"action":"verifiers", "commands":[{"program":"sh", "args":["-c", command]}]}),
        ),
        (
            Arc::new(RunTestsTool),
            json!({"args":"--config build.rustc=malicious"}),
        ),
        (
            Arc::new(RunVerifiersTool),
            json!({"commands":[{"program":"sh", "args":["-c",command]}]}),
        ),
    ];
    for (tool, input) in cases {
        let mut registry = ToolRegistry::new(context.clone());
        registry.register(tool.clone());
        for result in [
            registry.execute_full(tool.name(), input.clone()).await,
            tool.execute(input, &context).await,
        ] {
            let error = result.expect_err(tool.name());
            assert!(
                error.to_string().contains("disallowed-tools"),
                "{}: {error}",
                tool.name()
            );
            assert!(!tmp.path().join("denial-canary.txt").exists());
        }
    }
}

#[test]
fn shell_denial_keeps_the_existing_bounded_child_read_only_exception() {
    use crate::core::engine::tool_catalog::enforce_tool_denial;
    use crate::worker_profile::ShellPolicy;
    let tmp = tempdir().unwrap();
    let mut context = ToolContext::new(tmp.path()).with_shell_policy(ShellPolicy::ReadOnly);
    context.disallowed_tools = vec!["Bash".into()];
    assert!(enforce_tool_denial(&context, "bash", &json!({"command":"pwd"})).is_err());
    context = context.with_owner_agent("fixture-child", "fixture");
    assert!(enforce_tool_denial(&context, "bash", &json!({"command":"pwd"})).is_ok());
    for (name, input) in [
        ("Bash", json!({"command":"pwd"})),
        ("bash", json!({"command":"printf bad > denied"})),
        ("bash", json!({"command":"pwd", "background":true})),
        ("task_shell_start", json!({"command":"pwd"})),
        (
            "terminal/send",
            json!({"session":"existing", "text":"pwd\n"}),
        ),
    ] {
        assert!(
            enforce_tool_denial(&context, name, &input).is_err(),
            "{name}: {input}"
        );
    }
}

#[test]
fn mcp_iserror_result_maps_to_tool_error_preserving_text() {
    // #5123-class: MCP servers report tool failure via isError on an
    // otherwise successful response; the model must see a failure, not a
    // success carrying an error message body.
    let error_payload = json!({
        "content": [
            {"type": "text", "text": "delete failed: permission denied"}
        ],
        "isError": true
    });
    let result = mcp_result_to_bounded_rich_tool_result(error_payload).result;
    assert!(!result.success, "isError must not be reported as success");
    assert_eq!(result.content, "delete failed: permission denied");

    let ok_payload = json!({
        "content": [{"type": "text", "text": "wrote 3 rows"}]
    });
    let result = mcp_result_to_bounded_rich_tool_result(ok_payload).result;
    assert!(result.success);
    assert!(result.content.contains("wrote 3 rows"));

    // isError without text content falls back to the serialized payload.
    let bare_error = json!({"isError": true, "content": []});
    let result = mcp_result_to_bounded_rich_tool_result(bare_error).result;
    assert!(!result.success);
    assert!(result.content.contains("isError"));
}

#[test]
fn mcp_image_result_uses_typed_block_without_base64_in_text() {
    let image_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    let payload = json!({
        "content": [
            {"type": "text", "text": "screenshot captured"},
            {"type": "image", "data": image_data, "mimeType": "image/png"}
        ],
        "structuredContent": {"page": "https://example.com"},
        "isError": false
    });

    let rich = mcp_result_to_bounded_rich_tool_result(payload);

    assert!(rich.result.success);
    let sanitized: Value = serde_json::from_str(&rich.result.content).expect("sanitized MCP JSON");
    assert_eq!(sanitized["content"][0]["text"], "screenshot captured");
    assert_eq!(sanitized["content"][1]["data"], MCP_IMAGE_TEXT_PLACEHOLDER);
    assert_eq!(
        sanitized["structuredContent"],
        json!({"page": "https://example.com"})
    );
    assert_eq!(sanitized["isError"], false);
    assert!(!rich.result.content.contains(image_data));
    assert_eq!(
        rich.content_blocks,
        vec![codewhale_tools::ToolResultContentBlock::Image {
            mime_type: "image/png".to_string(),
            data: image_data.to_string(),
        }]
    );
}

#[test]
fn mcp_invalid_image_is_removed_with_a_visible_receipt() {
    let payload = json!({
        "content": [
            {"type": "image", "data": "not base64", "mimeType": "image/png"}
        ]
    });

    let rich = mcp_result_to_bounded_rich_tool_result(payload);

    assert!(rich.content_blocks.is_empty());
    assert!(rich.result.content.contains("MCP image payload removed"));
    assert!(
        rich.result
            .content
            .contains("1 tool-result image block(s) omitted")
    );
    assert!(!rich.result.content.contains("not base64"));
}

#[test]
fn mcp_malformed_images_are_removed_with_a_visible_receipt() {
    let image_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    let payload = json!({
        "content": [
            {"type": "image", "data": image_data},
            {"type": "image", "data": {"nested": image_data}, "mimeType": "image/png"}
        ]
    });

    let rich = mcp_result_to_bounded_rich_tool_result(payload);

    assert!(rich.content_blocks.is_empty());
    assert!(rich.result.content.contains("MCP image payload removed"));
    assert!(
        rich.result
            .content
            .contains("2 tool-result image block(s) omitted")
    );
    assert!(!rich.result.content.contains(image_data));
}

#[test]
fn mcp_image_limits_keep_one_valid_block_and_report_the_rest() {
    let image_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    let oversized = "A".repeat(crate::image_attach::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4);
    let payload = json!({
        "content": [
            {"type": "image", "data": oversized, "mimeType": "image/png"},
            {"type": "image", "data": image_data, "mimeType": "image/png"},
            {"type": "image", "data": image_data, "mimeType": "image/png"}
        ]
    });

    let rich = mcp_result_to_bounded_rich_tool_result(payload);

    assert_eq!(
        rich.content_blocks,
        vec![codewhale_tools::ToolResultContentBlock::Image {
            mime_type: "image/png".to_string(),
            data: image_data.to_string(),
        }]
    );
    assert!(
        rich.result
            .content
            .contains("2 tool-result image block(s) omitted")
    );
    assert!(!rich.result.content.contains(&oversized));
}

#[test]
fn mcp_error_text_and_typed_image_are_both_preserved() {
    let image_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    let payload = json!({
        "content": [
            {"type": "text", "text": "capture failed after partial screenshot"},
            {"type": "image", "data": image_data, "mimeType": "image/png"}
        ],
        "structuredContent": {"retryable": true},
        "isError": true
    });

    let rich = mcp_result_to_bounded_rich_tool_result(payload);

    assert!(!rich.result.success);
    assert_eq!(
        rich.result.content,
        "capture failed after partial screenshot"
    );
    assert_eq!(
        rich.content_blocks,
        vec![codewhale_tools::ToolResultContentBlock::Image {
            mime_type: "image/png".to_string(),
            data: image_data.to_string(),
        }]
    );
}

/// A simple test tool for unit testing
struct TestTool {
    name: String,
    description: String,
}

#[async_trait::async_trait]
impl ToolSpec for TestTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            },
            "required": ["message"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        let message = required_str(&input, "message")?;
        Ok(ToolResult::success(format!("Echo: {message}")))
    }
}

fn make_test_tool(name: &str) -> Arc<TestTool> {
    Arc::new(TestTool {
        name: name.to_string(),
        description: "A test tool".to_string(),
    })
}

#[test]
fn mcp_read_helpers_remain_auto_and_eagerly_loaded() {
    for name in [
        "list_mcp_resources",
        "list_mcp_resource_templates",
        "mcp_read_resource",
        "read_mcp_resource",
        "mcp_get_prompt",
    ] {
        let adapter = mcp_tool_adapter_for_test(name);
        assert_eq!(
            adapter.approval_requirement(),
            ApprovalRequirement::Auto,
            "{name} should remain an automatic read helper"
        );
        assert!(adapter.is_read_only(), "{name} should remain read-only");
        assert!(!adapter.defer_loading(), "{name} should remain loaded");
    }
}

#[test]
fn mcp_actions_require_approval_with_exact_helper_matching() {
    for name in [
        "mcp_github_create_pull_request",
        "mcp_github_list_mcp_resources_export",
        "read_mcp_resource_and_delete",
    ] {
        let adapter = mcp_tool_adapter_for_test(name);
        assert_eq!(
            adapter.approval_requirement(),
            ApprovalRequirement::Required,
            "{name} must not inherit read-helper approval"
        );
        assert!(
            adapter
                .capabilities()
                .contains(&ToolCapability::RequiresApproval),
            "{name} should advertise approval gating"
        );
        assert!(adapter.defer_loading(), "{name} should remain deferred");
    }
}

#[test]
fn test_registry_register_and_get() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);

    let tool = make_test_tool("test_tool");
    registry.register(tool);

    assert!(registry.contains("test_tool"));
    assert!(!registry.contains("nonexistent"));
    assert_eq!(registry.all().len(), 1);
}

#[test]
fn resolve_exact_match_is_ascii_case_insensitive() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);

    registry.register(make_test_tool("read_file"));

    assert_eq!(registry.resolve("READ_FILE"), Some("read_file"));
}

#[test]
fn resolve_never_executes_a_fuzzy_prefix_guess() {
    // #5123-class: a hallucinated name that merely shares a prefix with a
    // real tool must NOT resolve — executing a prefix guess dispatched an
    // arbitrary sibling tool ("agents" -> "agents/interrupt"). Exact and
    // lossless normalizations still resolve; guesses return None so the
    // caller can surface "unknown tool, did you mean: …".
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);

    registry.register(make_test_tool("agents/interrupt"));
    registry.register(make_test_tool("read_file"));

    // Prefix guesses in both directions are rejected.
    assert_eq!(registry.resolve("agents"), None);
    assert_eq!(registry.resolve("agents/int"), None);
    assert_eq!(registry.resolve("read"), None);
    assert_eq!(registry.resolve("read_file_extra"), None);

    // Lossless normalizations still resolve.
    let mut hyphen_registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
    hyphen_registry.register(make_test_tool("read_file"));
    assert_eq!(hyphen_registry.resolve("read-file"), Some("read_file"));
    assert_eq!(hyphen_registry.resolve("ReadFile"), Some("read_file"));
    assert_eq!(hyphen_registry.resolve("read_file_tool"), Some("read_file"));
}

#[test]
fn work_update_is_the_only_registered_progress_surface() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_todo_tool(crate::tools::todo::new_shared_todo_list())
        .build(ctx);

    // Canonical is todo_write; work_update/TodoWrite/todo are hidden compat aliases.
    assert!(registry.contains("todo_write"));
    for alias in ["work_update", "TodoWrite", "todo"] {
        assert!(
            registry.contains(alias),
            "{alias} compat alias must be registered"
        );
        // Hidden aliases are distinct entries (same handler, model_visible=false).
        assert_eq!(
            registry.resolve(alias),
            Some(alias),
            "{alias} must be directly resolvable as hidden alias"
        );
        let tool = registry.get(alias).expect("alias tool");
        assert!(
            !tool.model_visible(),
            "{alias} hidden alias must not be model-visible"
        );
    }
    // Only todo_write is model-visible.
    let api_names = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();

    assert!(
        api_names.iter().any(|name| name == "todo_write"),
        "todo_write should be the sole model-visible progress surface"
    );
    assert_eq!(
        api_names.iter().filter(|n| *n == "todo_write").count(),
        1,
        "canonical todo_write must appear exactly once in model catalog"
    );
    for hidden in [
        "work_update",
        "TodoWrite",
        "todo",
        "checklist_write",
        "checklist_update",
        "checklist_add",
        "checklist_list",
        "todo_add",
        "todo_update",
        "todo_list",
    ] {
        assert!(
            api_names.iter().all(|name| name != hidden),
            "{hidden} must not appear in the model catalog"
        );
    }
    // But hidden aliases still execute via registry dispatch.
    assert!(registry.contains("checklist_write"));
    assert!(registry.contains("checklist_update"));
}

#[test]
fn rlm_is_the_only_registered_session_surface() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_rlm_tool()
        .with_harness_tool()
        .build(ctx);

    assert!(registry.contains("rlm"));
    assert!(
        registry.contains("harness"),
        "the durable continual harness must accompany the persistent RLM surface"
    );
    for retired in [
        "rlm_session_objects",
        "rlm_open",
        "rlm_eval",
        "rlm_configure",
        "rlm_close",
    ] {
        assert!(
            !registry.contains(retired),
            "{retired} must no longer be callable"
        );
    }
}

/// The names an engine treats as built in: whatever is registered before the
/// plugin directory loads (see `configure_plugin_tools`).
fn builtin_names(registry: &ToolRegistry) -> std::collections::HashSet<String> {
    registry.names().into_iter().map(str::to_string).collect()
}

#[test]
fn apply_overrides_removes_original_when_replacement_is_missing() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistryBuilder::new().with_file_tools().build(ctx);
    let builtins = builtin_names(&registry);
    std::fs::write(
        tmp.path().join("reader.sh"),
        "# name: custom-reader\n# description: drop-in reader\n",
    )
    .unwrap();
    registry.load_plugins(tmp.path());
    assert!(registry.contains("custom-reader"));

    let mut overrides = HashMap::new();
    overrides.insert(
        "custom-reader".to_string(),
        ToolOverride::Script {
            path: "missing-wrapper.sh".to_string(),
            args: None,
        },
    );

    registry.apply_overrides(&overrides, tmp.path(), &builtins);

    assert!(!registry.contains("custom-reader"));
    assert!(registry.contains("File"));
}

/// D4 (CURRENT_DECISIONS §26): a `[tools.overrides]` script or command cannot
/// replace a built-in. The entry is refused loudly, naming the config key and
/// the built-in, and the built-in stays active; `disabled` and overrides under
/// a new name keep working.
#[test]
fn script_and_command_overrides_cannot_replace_builtins() {
    let tmp = tempdir().unwrap();
    let mut registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_patch_tools()
        .build(ToolContext::new(tmp.path()));
    let builtins = builtin_names(&registry);
    let file = registry.get("File").unwrap();
    let patch = registry.get("apply_patch").unwrap();
    std::fs::write(
        tmp.path().join("wrapper.sh"),
        "# name: wrapper\n# description: audit wrapper\n",
    )
    .unwrap();
    let script = || ToolOverride::Script {
        path: "wrapper.sh".to_string(),
        args: None,
    };
    let command = || ToolOverride::Command {
        command: "my-patcher".to_string(),
        args: None,
    };
    let overrides = HashMap::from([
        ("File".to_string(), script()),
        ("apply_patch".to_string(), command()),
        ("audited_file".to_string(), script()),
        ("my_patcher".to_string(), command()),
    ]);

    let mut refused = Vec::new();
    let errors = capture_registration_warnings(|| {
        refused = registry.apply_overrides(&overrides, tmp.path(), &builtins);
    });

    assert!(Arc::ptr_eq(&registry.get("File").unwrap(), &file));
    assert!(Arc::ptr_eq(&registry.get("apply_patch").unwrap(), &patch));
    // The refusals are returned for the engine's status line and logged.
    refused.sort();
    assert_eq!(refused, ["File", "apply_patch"]);
    for name in ["File", "apply_patch"] {
        assert!(
            errors.contains(&super::override_refusal_notice(name)),
            "{errors}"
        );
    }
    assert_eq!(
        registry.get("audited_file").unwrap().description(),
        "audit wrapper"
    );
    assert!(registry.contains("my_patcher"));

    let disable = HashMap::from([("File".to_string(), ToolOverride::Disabled)]);
    registry.apply_overrides(&disable, tmp.path(), &builtins);
    assert!(!registry.contains("File"));
}

/// D4 (CURRENT_DECISIONS §26): a script cannot approve itself. `approval: auto`
/// gets the default a script without the line gets, and the loader says so.
#[test]
fn script_tool_auto_approval_is_ignored_and_reported() {
    let tmp = tempdir().unwrap();
    let mut registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .build(ToolContext::new(tmp.path()));
    let builtins = builtin_names(&registry);
    let plugin_dir = tmp.path().join("tools");
    std::fs::create_dir(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("greet.sh"),
        "# name: greet\n# description: Say hello\n# approval: auto\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("audit.sh"),
        "# name: ignored\n# description: audit\n# approval: auto\n",
    )
    .unwrap();
    let overrides = HashMap::from([(
        "audited".to_string(),
        ToolOverride::Script {
            path: tmp.path().join("audit.sh").to_string_lossy().into_owned(),
            args: None,
        },
    )]);

    let warnings = capture_registration_warnings(|| {
        registry.load_plugins(&plugin_dir);
        registry.apply_overrides(&overrides, &plugin_dir, &builtins);
    });

    for name in ["greet", "audited"] {
        assert_eq!(
            registry.get(name).unwrap().approval_requirement(),
            ApprovalRequirement::Suggest,
            "{name}"
        );
        assert!(
            warnings.contains(&format!(
                "Script tool '{name}': `approval: auto` is no longer supported for script tools"
            )),
            "{warnings}"
        );
    }
}

#[test]
fn builder_registers_speech_alias_tools() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_speech_tools(None, None)
        .build(ctx);

    assert!(registry.contains("speech"));
    assert!(registry.contains("tts"));
    // One capability, one catalog entry: the alias stays callable for replay
    // but is not advertised (#5941).
    let visible: Vec<String> = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert!(visible.iter().any(|name| name == "speech"));
    assert!(!visible.iter().any(|name| name == "tts"), "{visible:?}");
}

#[test]
fn agent_runtime_surface_skips_speech_without_a_client() {
    use super::AgentToolSurfaceOptions;
    use crate::worker_profile::ShellPolicy;
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_agent_runtime_surface(
            None,
            "test-model".to_string(),
            AgentToolSurfaceOptions::new(ShellPolicy::Full),
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        )
        .build(ctx);
    assert!(!registry.contains("speech"));
    assert!(!registry.contains("tts"));
}

#[test]
fn model_visible_tool_descriptions_name_no_vendor() {
    use super::AgentToolSurfaceOptions;
    use crate::worker_profile::ShellPolicy;
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut options = AgentToolSurfaceOptions::new(ShellPolicy::Full);
    options.web_search_enabled = true;
    let registry = ToolRegistryBuilder::new()
        .with_agent_runtime_surface(
            None,
            "test-model".to_string(),
            options,
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        )
        .with_speech_tools(None, None)
        .build(ctx);
    let vendors = [
        "xiaomi",
        "mimo",
        "claude",
        "anthropic",
        "openai",
        "gpt-",
        "deepseek",
        "gemini",
        "kimi",
        "qwen",
        "grok",
        "mistral",
    ];
    for tool in registry.to_api_tools() {
        let description = tool.description.to_ascii_lowercase();
        for vendor in vendors {
            assert!(
                !description.contains(vendor),
                "tool {} names a vendor ({vendor}) in its model-facing description",
                tool.name
            );
        }
    }
}

#[test]
fn test_registry_names() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);

    registry.register(make_test_tool("tool_a"));
    registry.register(make_test_tool("tool_b"));

    let names = registry.names();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"tool_a"));
    assert!(names.contains(&"tool_b"));
}

#[test]
fn test_registry_to_api_tools() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);

    registry.register(make_test_tool("my_tool"));

    let api_tools = registry.to_api_tools();
    assert_eq!(api_tools.len(), 1);
    assert_eq!(api_tools[0].name, "my_tool");
    assert_eq!(api_tools[0].description, "A test tool");
}

#[test]
fn api_tools_with_cache_marks_last_tool_ephemeral() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);

    registry.register(make_test_tool("tool_a"));
    registry.register(make_test_tool("tool_b"));

    let api_tools = registry.to_api_tools_with_cache(true);
    assert_eq!(api_tools.len(), 2);
    assert!(api_tools[0].cache_control.is_none());
    assert_eq!(
        api_tools[1]
            .cache_control
            .as_ref()
            .map(|c| c.cache_type.as_str()),
        Some("ephemeral")
    );
}

/// Tool whose `description()` advances through a script of pre-built
/// strings, one per call. Used to demonstrate that the api-tools cache
/// pins the description bytes on first read instead of re-sampling them
/// each turn (#263 follow-up; mirrors reference-cc's `getToolSchemaCache`).
struct VaryingDescriptionTool {
    name: String,
    descriptions: Vec<String>,
    next: std::sync::atomic::AtomicUsize,
}

impl VaryingDescriptionTool {
    fn new(name: &str, descriptions: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            descriptions: descriptions.iter().map(|s| (*s).to_string()).collect(),
            next: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl ToolSpec for VaryingDescriptionTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        let idx = self
            .next
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .min(self.descriptions.len() - 1);
        &self.descriptions[idx]
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}, "required": []})
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::success("ok".to_string()))
    }
}

#[test]
fn to_api_tools_pins_description_bytes_across_calls() {
    // Regression for the cache-stability follow-up: an MCP adapter that
    // returns a different `description()` on reconnect (or any other
    // tool whose description isn't a `&'static str`) would otherwise
    // rewrite the catalog bytes mid-session and miss the prefix cache.
    // The registry pins the first call's value until it's mutated.
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);
    registry.register(Arc::new(VaryingDescriptionTool::new(
        "varying",
        &["first description", "second description"],
    )));

    let first = registry.to_api_tools();
    let second = registry.to_api_tools();

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].description, "first description");
    assert_eq!(
        first, second,
        "api-tools catalog must be byte-identical across reads with no mutation in between"
    );
}

#[test]
fn register_invalidates_api_tools_cache() {
    // Counter-test: when a real change happens (a new tool registers,
    // an existing one is removed, or `clear` is called), the cache must
    // be discarded so the next read reflects the live registry.
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);
    registry.register(Arc::new(VaryingDescriptionTool::new(
        "varying",
        &["first description", "second description"],
    )));

    let before = registry.to_api_tools();
    assert_eq!(before.len(), 1);

    registry.register(make_test_tool("late_arrival"));

    let after = registry.to_api_tools();
    assert_eq!(after.len(), 2, "cache must rebuild after register");
    assert!(after.iter().any(|t| t.name == "varying"));
    assert!(after.iter().any(|t| t.name == "late_arrival"));
    // The varying tool's description advances on cache rebuild — the
    // first read above sampled `first description`; this rebuild samples
    // `second description`. The point is just that the bytes *can*
    // change after a real mutation, not that they always do.
    let varying_after = after
        .iter()
        .find(|t| t.name == "varying")
        .expect("varying tool present");
    assert_eq!(varying_after.description, "second description");
}

#[test]
fn remove_tool_invalidates_api_tools_cache() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let mut registry = ToolRegistry::new(ctx);
    registry.register(make_test_tool("alpha"));
    registry.register(make_test_tool("beta"));

    let before = registry.to_api_tools();
    assert_eq!(before.len(), 2);

    assert!(registry.remove_tool("alpha"));
    let after_remove = registry.to_api_tools();
    assert_eq!(after_remove.len(), 1);
    assert_eq!(after_remove[0].name, "beta");
}

#[test]
fn to_api_tools_emits_alphabetical_order_regardless_of_registration_order() {
    // Regression for #263: HashMap iteration is non-deterministic across
    // process launches, which busts DeepSeek's KV prefix cache for every
    // cross-session resume. `to_api_tools` must emit by name regardless
    // of registration order so two consecutive calls (and two distinct
    // launches) produce byte-identical output.
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let order_a = {
        let mut registry = ToolRegistry::new(ctx.clone());
        registry.register(make_test_tool("zebra"));
        registry.register(make_test_tool("alpha"));
        registry.register(make_test_tool("mango"));
        registry
            .to_api_tools()
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
    };

    let order_b = {
        let mut registry = ToolRegistry::new(ctx.clone());
        registry.register(make_test_tool("alpha"));
        registry.register(make_test_tool("mango"));
        registry.register(make_test_tool("zebra"));
        registry
            .to_api_tools()
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
    };

    assert_eq!(order_a, vec!["alpha", "mango", "zebra"]);
    assert_eq!(order_a, order_b);
}

fn scoped_context(workspace: &std::path::Path) -> ToolContext {
    ToolContext::new(workspace.to_path_buf())
        .with_tool_authority(
            ToolAuthorityEnvelope {
                schema_version: 1,
                owner: "fleet-worker-1".to_string(),
                authority: ToolMutationAuthority::ScopedWrite,
                network_access: None,
                shell: crate::tools::spec::ToolShellAuthority::None,
                verification: crate::tools::spec::ToolVerificationAuthority::None,
                writable_roots: vec!["src".to_string()],
                writable_files: Vec::new(),
                coordination_contracts: Vec::new(),
            }
            .normalized()
            .expect("test authority"),
        )
        .expect("test context authority")
}

fn readonly_scout_context(workspace: &std::path::Path, network_access: bool) -> ToolContext {
    ToolContext::new(workspace.to_path_buf())
        .with_tool_authority(ToolAuthorityEnvelope {
            schema_version: 1,
            owner: "scout-1".to_string(),
            authority: ToolMutationAuthority::ReadOnly,
            network_access: Some(network_access),
            shell: crate::tools::spec::ToolShellAuthority::ReadOnly,
            verification: crate::tools::spec::ToolVerificationAuthority::None,
            writable_roots: Vec::new(),
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
        })
        .expect("read-only Scout authority")
}

fn readonly_verifier_context(workspace: &std::path::Path) -> ToolContext {
    ToolContext::new(workspace.to_path_buf())
        .with_tool_authority(ToolAuthorityEnvelope {
            schema_version: 1,
            owner: "verifier-1".to_string(),
            authority: ToolMutationAuthority::ReadOnly,
            network_access: Some(true),
            shell: crate::tools::spec::ToolShellAuthority::None,
            verification: crate::tools::spec::ToolVerificationAuthority::Bounded,
            writable_roots: Vec::new(),
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
        })
        .expect("bounded verifier authority")
}

#[test]
fn machine_verifier_catalog_and_dispatch_add_only_bounded_run() {
    let tmp = tempdir().expect("tempdir");
    let registry = ToolRegistryBuilder::new()
        .with_agent_tools_policy(
            crate::worker_profile::ShellPolicy::None,
            crate::tools::user_input::UserInputLimits::default(),
        )
        .with_web_tools()
        .with_todo_tool(crate::tools::todo::new_shared_todo_list())
        .build(readonly_verifier_context(tmp.path()));
    let tools = registry.to_api_tools();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, {
        let mut expected = vec![
            "Run",
            "Web",
            "diagnostics",
            "file_search",
            "finance",
            "grep_files",
            "handle_read",
            "list_dir",
            "load_skill",
            "lsp",
            "project_map",
            "read",
            "read_media",
            "request_user_input",
            "retrieve_tool_result",
            "todo_write",
            "tui_help",
            "validate_data",
            "web.run",
        ];
        if crate::tools::image_ocr::ocr_available() {
            expected.insert(7, "image_ocr");
        }
        expected
    });
    let run = registry.get("Run").expect("bounded Run registered");
    assert!(
        tools
            .iter()
            .find(|tool| tool.name == "Run")
            .unwrap()
            .input_schema["properties"]
            .get("commands")
            .is_none(),
        "the catalog must not advertise operator-supplied verifier programs"
    );
    enforce_tool_authority(
        "Run",
        &json!({"action": "tests", "args": "-p codewhale-tui ordinary_scout"}),
        run.as_ref(),
        registry.context(),
    )
    .expect("pure test selection fits bounded verifier authority");
    for input in [
        json!({"action": "tests", "args": "--manifest-path ../other/Cargo.toml"}),
        json!({"action": "verifiers", "commands": [{"name": "escape", "program": "sh"}]}),
    ] {
        let error = enforce_tool_authority("Run", &input, run.as_ref(), registry.context())
            .expect_err("unbounded verification must remain refused")
            .to_string();
        assert!(error.contains("unbounded verification"), "{error}");
    }
    assert!(!registry.contains("bash"), "Verifier never gains raw shell");
    assert!(!registry.contains("Bash"), "Verifier never gains raw shell");
}

#[tokio::test]
async fn fleet_authority_allows_scoped_file_writes_and_rejects_outside_paths() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    std::fs::create_dir(tmp.path().join("docs")).expect("docs");
    let registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_patch_tools()
        .build(scoped_context(tmp.path()));

    registry
        .execute_full(
            "File",
            json!({"action": "write", "path": "src/ok.txt", "content": "ok\n"}),
        )
        .await
        .expect("scoped File write");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("src/ok.txt")).expect("written file"),
        "ok\n"
    );

    let error = registry
        .execute_full(
            "File",
            json!({"action": "write", "path": "docs/no.txt", "content": "no\n"}),
        )
        .await
        .expect_err("out-of-scope File write")
        .to_string();
    assert!(error.contains("outside its machine-readable"), "{error}");
    assert!(!tmp.path().join("docs/no.txt").exists());
}

#[tokio::test]
async fn fleet_authority_allows_only_classifier_proven_readonly_bash() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    std::fs::write(
        tmp.path().join("src/evidence.txt"),
        "first\nsecond\nthird\n",
    )
    .expect("inspection fixture");
    let registry = ToolRegistryBuilder::new()
        .with_shell_tools()
        .build(readonly_scout_context(tmp.path(), true));

    let shell = BashTool::new("Bash");
    for command in [
        "pwd",
        "git status --short",
        "rg needle src",
        "gh issue list --limit 10",
        "gh issue view 5287 --json title,state",
        "sed -n '2,3p' src/evidence.txt",
        // #6015: durable workers accept the same grammar as in-session
        // agents — pipelines, chains, find, git -C and a leading cd.
        "rg -n foo src | head -5",
        "find . -name '*.rs'",
        "git -C . log --oneline -3",
        "cd src && git diff",
        "git diff HEAD && echo '=== FILES ===' && ls -la",
        "rg -n foo src 2>/dev/null",
        "sed -n '2p' src/evidence.txt | head -n 1",
        "sed -n '2p' src/evidence.txt | gh issue list",
        "gh issue list | sed -n '2p'",
        "find src -name '*.rs'",
    ] {
        enforce_tool_authority(
            "Bash",
            &json!({"action": "run", "command": command}),
            &shell,
            registry.context(),
        )
        .unwrap_or_else(|error| panic!("{command} should fit read-only Scout authority: {error}"));
    }

    let result = registry
        .execute_full("Bash", json!({"action": "run", "command": "pwd"}))
        .await
        .expect("bounded read-only Bash survives machine authority");
    assert!(result.success, "{}", result.content);

    #[cfg(unix)]
    for name in ["bash", "Bash"] {
        let result = registry
            .execute_full(name, json!({"command": "sed -n '2,3p' src/evidence.txt"}))
            .await
            .expect("numeric sed inspection survives machine authority");
        assert!(result.success, "{}", result.content);
        assert!(
            result.content.contains("second\nthird"),
            "{}",
            result.content
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("src/evidence.txt")).expect("fixture"),
            "first\nsecond\nthird\n"
        );
    }

    for command in [
        "touch src/no.txt",
        "git checkout -- src/lib.rs",
        "git push origin main",
        "gh issue close 5287",
        "gh issue edit 5287 --title changed",
        "gh issue create --title nope --body nope",
        "gh issue view 5287 > issue.txt",
        "gh issue view 5287 &",
        "bash -lc 'git status'",
        "sed -i -n '2p' src/evidence.txt",
        "sed -n '2p' src/evidence.txt -i",
        "sed -n '2p' src/evidence.txt -e 'w src/no.txt'",
        "sed -n '2p' src/evidence.txt -f src/evidence.txt",
        "sed -n 'w src/no.txt' src/evidence.txt",
        "sed -n 'e touch src/no.txt' src/evidence.txt",
        "sed -n 's/first/changed/w src/no.txt' src/evidence.txt",
        "sed -n '2p' $(touch src/no.txt)",
        "sed -n '2p' src/evidence.txt > src/no.txt",
        "sed -n '2p' src/evidence.txt && touch src/no.txt",
        "find src -delete",
        "awk '1' src/evidence.txt",
        "git commit -m x",
        "sort -o src/no.txt src/evidence.txt",
        "cd /etc; cat passwd",
    ] {
        let error = registry
            .execute_full("Bash", json!({"action": "run", "command": command}))
            .await
            .expect_err("mutating Bash remains outside machine authority")
            .to_string();
        // The refusal names the rule, from the same classifier every
        // read-only gate uses.
        assert!(error.contains("[shell.readonly.command]"), "{error}");
        assert!(error.contains("File tool"), "{error}");
    }
    assert!(!tmp.path().join("src/no.txt").exists());

    let no_shell = scoped_context(tmp.path());
    let error = enforce_tool_authority(
        "Bash",
        &json!({"action": "run", "command": "pwd"}),
        &shell,
        &no_shell,
    )
    .expect_err("mutation authority must not imply shell authority")
    .to_string();
    assert!(error.contains("does not grant read-only shell"), "{error}");
}

#[test]
fn fleet_authority_sed_inspection_preserves_policy_boundaries() {
    let tmp = tempdir().expect("tempdir");
    let context = readonly_scout_context(tmp.path(), false);
    let registry = ToolRegistryBuilder::new().with_shell_tools().build(context);
    for name in ["bash", "Bash"] {
        let shell = registry.get(name).expect("shell tool");
        let input = if name == "bash" {
            json!({"command": "sed -n '300,400p' src/lib.rs", "timeout": 10})
        } else {
            json!({"action": "run", "command": "sed -n '300,400p' src/lib.rs", "timeout_ms": 10_000})
        };
        enforce_tool_authority(name, &input, shell.as_ref(), registry.context())
            .expect("local numeric sed inspection needs no network grant");
        assert!(
            !shell.is_read_only_for(&input),
            "parent classification stays strict"
        );
        assert!(
            !shell.supports_parallel_for(&input),
            "parallel policy stays strict"
        );
        assert_eq!(
            shell.approval_requirement_for(&input),
            ApprovalRequirement::Required
        );

        let mut denied = registry.context().clone();
        denied.disallowed_tools = vec!["Bash".into()];
        assert!(enforce_tool_authority(name, &input, shell.as_ref(), &denied).is_err());
        assert!(
            enforce_tool_authority(name, &input, shell.as_ref(), &scoped_context(tmp.path()))
                .is_err(),
            "write authority does not grant shell authority"
        );
        assert!(
            enforce_tool_authority(
                name,
                &input,
                shell.as_ref(),
                &readonly_verifier_context(tmp.path())
            )
            .is_err(),
            "shell-less evidence authority stays shell-less"
        );
        for field in [
            json!({"background": true}),
            json!({"tty": true}),
            json!({"interactive": true}),
            json!({"stdin": ""}),
            json!({"action": "wait"}),
            json!({"action": "interact"}),
            json!({"action": "cancel"}),
            json!({"action": 3}),
            json!({"task_id": "shell_1"}),
            json!({"persist": true}),
            json!({"sandbox_permissions": "danger-full-access", "justification": "test"}),
        ] {
            let mut rejected = input.clone();
            rejected
                .as_object_mut()
                .unwrap()
                .extend(field.as_object().unwrap().clone());
            assert!(
                enforce_tool_authority(name, &rejected, shell.as_ref(), registry.context())
                    .is_err(),
                "{name}: {rejected}"
            );
        }
    }
}

#[test]
fn fleet_authority_intersects_readonly_github_bash_with_network_ceiling() {
    let tmp = tempdir().expect("tempdir");
    let shell = BashTool::new("Bash");
    let input = json!({"action": "run", "command": "gh issue view 5287"});
    let networked = ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(ToolAuthorityEnvelope {
            schema_version: 1,
            owner: "scout".to_string(),
            authority: ToolMutationAuthority::ReadOnly,
            network_access: Some(true),
            shell: crate::tools::spec::ToolShellAuthority::ReadOnly,
            verification: crate::tools::spec::ToolVerificationAuthority::None,
            writable_roots: Vec::new(),
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
        })
        .expect("networked scout");
    enforce_tool_authority("Bash", &input, &shell, &networked)
        .expect("networked scout may inspect GitHub");

    let offline = ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(ToolAuthorityEnvelope {
            schema_version: 1,
            owner: "offline-scout".to_string(),
            authority: ToolMutationAuthority::ReadOnly,
            network_access: Some(false),
            shell: crate::tools::spec::ToolShellAuthority::ReadOnly,
            verification: crate::tools::spec::ToolVerificationAuthority::None,
            writable_roots: Vec::new(),
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
        })
        .expect("offline scout");
    let error = enforce_tool_authority("Bash", &input, &shell, &offline)
        .expect_err("network denial must win")
        .to_string();
    assert!(error.contains("does not grant network access"), "{error}");

    // #6015: an admitted network read cannot hide inside a pipeline or chain.
    for command in [
        "gh pr view 1 | head",
        "ls && gh issue list",
        "cd . && gh pr view 1",
    ] {
        let input = json!({"action": "run", "command": command});
        enforce_tool_authority("Bash", &input, &shell, &networked)
            .unwrap_or_else(|error| panic!("{command}: {error}"));
        let error = enforce_tool_authority("Bash", &input, &shell, &offline)
            .expect_err("network denial must win inside compositions")
            .to_string();
        assert!(
            error.contains("does not grant network access"),
            "{command}: {error}"
        );
    }
    // Network access alone cannot authorize npm's configured destinations.
    for command in [
        "npm view x",
        "npm view @scope/pkg --json",
        "cd sub && npm view x",
    ] {
        let input = json!({"action": "run", "command": command});
        for context in [&networked, &offline] {
            let error = enforce_tool_authority("Bash", &input, &shell, context)
                .expect_err("npm metadata reads require ordinary shell authority")
                .to_string();
            assert!(error.contains("configuration"), "{command}: {error}");
        }
    }
}

#[tokio::test]
async fn fleet_authority_denies_git_even_when_the_action_is_nominally_read_only() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    let registry = ToolRegistryBuilder::new()
        .with_git_tools()
        .with_git_history_tools()
        .with_review_tool(None, "fixture-model".to_string())
        .build(scoped_context(tmp.path()));

    for (name, input) in [
        ("Git", json!({"action": "status"})),
        ("Git", json!({"action": "diff"})),
        ("Git", json!({"action": "show", "revision": "HEAD"})),
        ("Git", json!({"action": "blame", "path": "src/lib.rs"})),
        ("review", json!({"target": "diff"})),
    ] {
        let error = registry
            .execute_full(name, input)
            .await
            .expect_err("Git subprocesses remain unprovable under Fleet authority")
            .to_string();
        assert!(error.contains("Git helpers"), "{name}: {error}");
    }
}

#[tokio::test]
async fn fleet_authority_rejects_fim_edit_outside_its_write_scope() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    std::fs::create_dir(tmp.path().join("docs")).expect("docs");
    std::fs::write(tmp.path().join("docs/outside.txt"), "before\nafter\n").expect("fixture");
    let registry = ToolRegistryBuilder::new()
        .with_tool(Arc::new(crate::tools::fim::FimEditTool::new(
            None,
            "fixture-model".to_string(),
        )))
        .build(scoped_context(tmp.path()));

    let error = registry
        .execute_full(
            "fim_edit",
            json!({
                "path": "docs/outside.txt",
                "prefix_anchor": "before\n",
                "suffix_anchor": "after\n"
            }),
        )
        .await
        .expect_err("FIM mutation must be checked before model execution")
        .to_string();
    assert!(error.contains("outside its machine-readable"), "{error}");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("docs/outside.txt")).unwrap(),
        "before\nafter\n"
    );
}

struct MixedExecutionTool;

#[async_trait::async_trait]
impl ToolSpec for MixedExecutionTool {
    fn name(&self) -> &str {
        "mixed_execution"
    }

    fn description(&self) -> &str {
        "inspect or start a child"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExecutesCode]
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        input.get("action").and_then(Value::as_str) == Some("inspect")
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::success("observed"))
    }
}

#[tokio::test]
async fn fleet_authority_allows_read_only_actions_but_denies_mixed_family_starts() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    let registry = ToolRegistryBuilder::new()
        .with_tool(Arc::new(MixedExecutionTool))
        .build(scoped_context(tmp.path()));

    registry
        .execute_full("mixed_execution", json!({"action": "inspect"}))
        .await
        .expect("read-only status/inspect actions remain usable");
    let error = registry
        .execute_full("mixed_execution", json!({"action": "start"}))
        .await
        .expect_err("child/code starts remain denied")
        .to_string();
    assert!(error.contains("child execution"), "{error}");
}

struct UnscopedMutator;

#[async_trait::async_trait]
impl ToolSpec for UnscopedMutator {
    fn name(&self) -> &str {
        "unscoped_mutator"
    }

    fn description(&self) -> &str {
        "mutates state without a file target"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }

    fn is_read_only_for(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::success("mutated"))
    }
}

#[tokio::test]
async fn fleet_authority_denies_every_unscoped_mutator_not_only_file_capabilities() {
    let tmp = tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).expect("src");
    let registry = ToolRegistryBuilder::new()
        .with_tool(Arc::new(UnscopedMutator))
        .build(scoped_context(tmp.path()));

    let error = registry
        .execute_full("unscoped_mutator", json!({}))
        .await
        .expect_err("unscoped mutation must fail closed")
        .to_string();
    assert!(error.contains("mutating tool"), "{error}");
}

#[test]
fn test_builder_basic() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new()
        .with_tool(make_test_tool("custom"))
        .build(ctx);

    assert!(registry.contains("custom"));
}

#[test]
fn test_builder_with_web_tools_no_longer_includes_finance() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new().with_web_tools().build(ctx);

    // The model-facing web surface is the canonical action-dispatched tool.
    assert!(registry.contains("Web"));
    assert!(registry.contains("web.run"));
    for retired in ["web_search", "fetch_url", "wait_for_dev_server"] {
        assert!(!registry.contains(retired), "{retired} must stay removed");
    }
    assert!(!registry.contains("finance"));
}

#[test]
fn canonical_runtime_tools_hide_compatibility_aliases() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_search_tools()
        .with_git_tools()
        .with_git_history_tools()
        .with_test_runner_tool()
        .with_web_tools()
        .with_patch_tools()
        .build(ctx);

    let api_names = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    for canonical in [
        "read",
        "write",
        "edit",
        "list_dir",
        "file_search",
        "grep_files",
        "Git",
        "Run",
        "Web",
    ] {
        assert!(api_names.iter().any(|name| name == canonical));
    }
    for hidden in ["File", "read_file", "write_file", "edit_file"] {
        assert!(registry.contains(hidden), "{hidden} must remain replayable");
        assert!(
            api_names.iter().all(|name| name != hidden),
            "{hidden} must stay out of new model catalogs"
        );
    }
    for retired in [
        "git_status",
        "git_diff",
        "git_log",
        "git_show",
        "git_blame",
        "run_tests",
        "run_verifiers",
        "web_search",
        "fetch_url",
        "wait_for_dev_server",
    ] {
        assert!(!registry.contains(retired), "{retired} must stay removed");
        assert!(
            api_names.iter().all(|name| name != retired),
            "{retired} must not be advertised"
        );
    }
    // apply_patch remains searchable/deferred outside the Pi-small head.
    assert!(registry.contains("apply_patch"));
    assert!(api_names.iter().any(|name| name == "apply_patch"));
}

#[tokio::test]
async fn canonical_file_actions_share_read_before_edit_state() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("sample.txt"), "before\n").expect("fixture");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new().with_file_tools().build(ctx);

    registry
        .execute_full("File", json!({"action": "read", "path": "sample.txt"}))
        .await
        .expect("canonical read should execute");
    registry
        .execute_full(
            "File",
            json!({
                "action": "edit",
                "path": "sample.txt",
                "search": "before",
                "replace": "after"
            }),
        )
        .await
        .expect("canonical edit should execute after the read");

    assert_eq!(
        std::fs::read_to_string(tmp.path().join("sample.txt")).expect("edited file"),
        "after\n"
    );
}

#[test]
fn read_only_file_surface_does_not_advertise_write_actions() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_read_only_file_tools()
        .with_search_tools()
        .build(ctx);
    let names = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "read"));
    for hidden_or_mutating in ["File", "read_file", "write", "edit"] {
        assert!(
            names.iter().all(|name| name != hidden_or_mutating),
            "{hidden_or_mutating} must not be model-visible"
        );
    }
    assert!(registry.contains("File"));
    assert!(registry.contains("read_file"));
    assert!(!registry.contains("write_file"));
    assert!(!registry.contains("edit_file"));
    let hidden_file = registry
        .get("File")
        .expect("hidden File compatibility tool");
    let schema = hidden_file.input_schema();
    let actions = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum");

    for blocked in ["write", "edit", "patch"] {
        assert!(actions.iter().all(|action| action != blocked));
    }
}

#[test]
fn test_builder_with_finance_tool() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new().with_finance_tool().build(ctx);

    assert!(registry.contains("finance"));
}

#[test]
fn with_verify_tool_registers_and_exposes_verify() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new()
        .with_verify_tool(None, "test-model".to_string())
        .build(ctx);

    assert!(
        registry.contains("verify"),
        "verify tool should be registered"
    );
    let api_names = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(
        api_names.iter().any(|name| name == "verify"),
        "verify tool should be model-visible"
    );
}

#[test]
fn agent_runtime_surface_gates_verify_on_option() {
    use super::AgentToolSurfaceOptions;
    use crate::worker_profile::ShellPolicy;

    let build_surface = |verify_enabled: bool| {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut options = AgentToolSurfaceOptions::new(ShellPolicy::Full);
        options.verify_tool_enabled = verify_enabled;
        ToolRegistryBuilder::new()
            .with_agent_runtime_surface(
                None,
                "test-model".to_string(),
                options,
                crate::tools::todo::new_shared_todo_list(),
                crate::tools::plan::new_shared_plan_state(),
            )
            .build(ctx)
    };

    assert!(
        build_surface(true).contains("verify"),
        "verify should register when enabled"
    );
    assert!(
        !build_surface(false).contains("verify"),
        "verify should be absent when the opt-out disables it"
    );
}

#[test]
fn agent_runtime_surface_gates_request_plugin_install_on_option() {
    use super::AgentToolSurfaceOptions;
    use crate::worker_profile::ShellPolicy;

    // Policy rule 11: hosts without the TUI (exec, runtime API) and sessions
    // with contextual tips off get no plugin-offer tool in any mode.
    let build_surface = |enabled: bool| {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut options = AgentToolSurfaceOptions::new(ShellPolicy::Full);
        options.request_plugin_install_enabled = enabled;
        ToolRegistryBuilder::new()
            .with_agent_runtime_surface(
                None,
                "test-model".to_string(),
                options,
                crate::tools::todo::new_shared_todo_list(),
                crate::tools::plan::new_shared_plan_state(),
            )
            .build(ctx)
    };

    assert!(build_surface(true).contains("request_plugin_install"));
    assert!(!build_surface(false).contains("request_plugin_install"));
}

#[test]
fn test_builder_with_agent_tools_policy_includes_finance() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new()
        .with_agent_tools_policy(
            crate::worker_profile::ShellPolicy::None,
            crate::tools::user_input::UserInputLimits::default(),
        )
        .build(ctx);

    assert!(registry.contains("finance"));
}

#[test]
fn agent_tools_with_shell_policy_none_excludes_shell_tools() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new()
        .with_agent_tools_policy(
            crate::worker_profile::ShellPolicy::None,
            crate::tools::user_input::UserInputLimits::default(),
        )
        .build(ctx);

    assert!(!registry.contains("bash"));
    assert!(!registry.contains("Bash"));
    assert!(
        !registry.contains("exec_shell"),
        "retired exec_shell must remain absent"
    );
    assert!(
        !registry.contains("task_shell_start"),
        "task_shell_start should be excluded when the shell policy is None"
    );
    assert!(
        !registry.contains("task_shell_wait"),
        "task_shell_wait should be excluded when the shell policy is None"
    );
}

#[test]
fn agent_tools_with_shell_policy_readonly_exposes_only_run_only_bash() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new()
        .with_agent_tools_policy(
            crate::worker_profile::ShellPolicy::ReadOnly,
            crate::tools::user_input::UserInputLimits::default(),
        )
        .build(ctx);

    assert!(registry.contains("bash"));
    assert!(registry.contains("Bash"));
    assert!(!registry.contains("exec_shell"));
    assert!(!registry.contains("task_shell_start"));
    assert!(!registry.contains("task_shell_wait"));
    assert!(
        registry
            .names()
            .into_iter()
            .all(|name| !name.starts_with("terminal/"))
    );
    let bash = registry
        .to_api_tools()
        .into_iter()
        .find(|tool| tool.name == "bash")
        .expect("read-only lowercase bash catalog");
    assert_eq!(bash.input_schema["required"], json!(["command"]));
    assert_eq!(
        bash.input_schema["properties"]
            .as_object()
            .expect("bash properties")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "command",
            "justification",
            "read_only",
            "sandbox_permissions",
            "timeout"
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    );
    for hidden in ["action", "background", "tty", "stdin", "task_id", "wait"] {
        assert!(bash.input_schema["properties"].get(hidden).is_none());
    }
    assert!(
        registry
            .to_api_tools()
            .iter()
            .all(|tool| tool.name != "Bash")
    );
}

#[test]
fn machine_readonly_catalog_is_exactly_the_evidence_profile() {
    let tmp = tempdir().expect("tempdir");
    let registry = ToolRegistryBuilder::new()
        .with_agent_tools_policy(
            crate::worker_profile::ShellPolicy::ReadOnly,
            crate::tools::user_input::UserInputLimits::default(),
        )
        .with_web_tools()
        .with_todo_tool(crate::tools::todo::new_shared_todo_list())
        .build(readonly_scout_context(tmp.path(), true));
    let tools = registry.to_api_tools();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, {
        let mut expected = vec![
            "Web",
            "bash",
            "diagnostics",
            "file_search",
            "finance",
            "grep_files",
            "handle_read",
            "list_dir",
            "load_skill",
            "lsp",
            "project_map",
            "read",
            "read_media",
            "request_user_input",
            "retrieve_tool_result",
            "todo_write",
            "tui_help",
            "validate_data",
            "web.run",
        ];
        if crate::tools::image_ocr::ocr_available() {
            expected.insert(7, "image_ocr");
        }
        expected
    });
    assert!(registry.contains("File"));
    assert!(registry.contains("Bash"));
    assert!(tools.iter().all(|tool| tool.name != "File"));
    assert!(tools.iter().all(|tool| tool.name != "Bash"));
    let shell = tools.iter().find(|tool| tool.name == "bash").unwrap();
    assert!(shell.description.contains("`cd <dir> &&`"));
    assert!(shell.description.contains("git log"));
    assert!(shell.description.contains("cannot change its own role"));
    let bash = registry.get("bash").unwrap();
    enforce_tool_authority(
        "bash",
        &json!({"command": "cd src && git status"}),
        bash.as_ref(),
        registry.context(),
    )
    .expect("a leading cd moves into the working directory (#6015)");
    for command in ["git branch -a", "git rev-parse HEAD"] {
        let error = enforce_tool_authority(
            "bash",
            &json!({"command":command}),
            bash.as_ref(),
            registry.context(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("subcommand:") && error.contains("git log"),
            "{error}"
        );
    }
    let web = tools.iter().find(|tool| tool.name == "Web").unwrap();
    assert_eq!(
        web.input_schema["properties"]["action"]["enum"],
        json!(["search", "fetch"])
    );
    let lsp = registry
        .get("lsp")
        .expect("registered but catalog-hidden lsp");
    enforce_tool_authority("lsp", &json!({}), lsp.as_ref(), registry.context())
        .expect("machine read-only dispatch uses the same positive profile as the catalog");
    let offline = ToolRegistryBuilder::new()
        .with_web_tools()
        .build(readonly_scout_context(tmp.path(), false));
    assert!(
        offline
            .to_api_tools()
            .iter()
            .all(|tool| !matches!(tool.name.as_str(), "Web" | "web.run"))
    );
}

#[test]
fn agent_tools_with_shell_policy_full_includes_shell_tools() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let registry = ToolRegistryBuilder::new()
        .with_agent_tools_policy(
            crate::worker_profile::ShellPolicy::Full,
            crate::tools::user_input::UserInputLimits::default(),
        )
        .build(ctx);

    assert!(registry.contains("bash"));
    assert!(registry.contains("Bash"));
    assert!(!registry.contains("exec_shell"));
    assert!(
        !registry.contains("task_shell_start"),
        "task_shell_start is removed; background shell work goes through bash background=true"
    );
    assert!(
        registry.contains("task_shell_wait"),
        "task_shell_wait should be included when the shell policy is Full"
    );
    let api_names = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(api_names.iter().any(|name| name == "bash"));
    assert!(api_names.iter().all(|name| name != "Bash"));
}

/// v0.9.3 removes the per-action shell aliases entirely.
#[test]
fn shell_surface_exposes_lowercase_bash_and_hides_legacy_handler() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new().with_shell_tools().build(ctx);

    for alias in [
        "exec_shell",
        "exec_wait",
        "exec_interact",
        "exec_shell_wait",
        "exec_shell_interact",
        "exec_shell_cancel",
    ] {
        assert!(!registry.contains(alias), "{alias} must be removed");
    }

    let api_names: Vec<String> = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    assert!(registry.contains("bash"));
    assert!(registry.contains("Bash"));

    // Only lowercase bash is model-visible.
    assert!(
        api_names.iter().any(|n| n == "bash"),
        "bash should be model-visible"
    );
    assert!(api_names.iter().all(|n| n != "Bash"));

    // Removed names also cannot leak back into the model catalog.
    for alias in [
        "exec_shell",
        "exec_wait",
        "exec_interact",
        "exec_shell_wait",
        "exec_shell_interact",
        "exec_shell_cancel",
    ] {
        assert!(
            api_names.iter().all(|n| n != alias),
            "{alias} should be hidden from the model catalog"
        );
    }
}

/// Each durable-work family exposes one canonical action tool; v0.9.3
/// removes the per-action execution aliases.
#[test]
fn runtime_task_families_expose_only_canonical_tools() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_runtime_task_tools()
        .build(ctx);

    let legacy_aliases = [
        "task_create",
        "task_list",
        "task_read",
        "task_cancel",
        "task_gate_run",
        "pr_attempt_record",
        "pr_attempt_list",
        "pr_attempt_read",
        "pr_attempt_preflight",
        "github_issue_context",
        "github_pr_context",
        "github_comment",
        "github_close_issue",
        "github_close_pr",
        "automation_create",
        "automation_list",
        "automation_read",
        "automation_update",
        "automation_pause",
        "automation_resume",
        "automation_delete",
        "automation_run",
    ];
    for alias in legacy_aliases {
        assert!(!registry.contains(alias), "{alias} must be removed");
    }

    let api_names: Vec<String> = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    // Only the canonical tools are model-visible.
    for canonical in ["tasks", "github", "automation"] {
        assert!(
            api_names.iter().any(|n| n == canonical),
            "{canonical} should be model-visible"
        );
    }
    // Removed aliases also cannot leak back into the model catalog.
    for alias in legacy_aliases {
        assert!(
            api_names.iter().all(|n| n != alias),
            "{alias} should be hidden from the model catalog"
        );
    }
}

/// The Plan-mode read-only surface registers only the canonical families,
/// restricted to their read actions.
#[test]
fn read_only_task_surface_contains_no_per_action_aliases() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new()
        .with_runtime_read_only_task_tools()
        .build(ctx);

    for name in [
        "task_list",
        "task_read",
        "pr_attempt_list",
        "pr_attempt_read",
        "github_issue_context",
        "github_pr_context",
        "automation_list",
        "automation_read",
        "task_create",
        "task_cancel",
        "task_gate_run",
        "pr_attempt_record",
        "pr_attempt_preflight",
        "github_comment",
        "github_close_issue",
        "github_close_pr",
        "automation_create",
        "automation_update",
        "automation_pause",
        "automation_resume",
        "automation_delete",
        "automation_run",
    ] {
        assert!(!registry.contains(name), "{name} must be removed");
    }

    let api_names: Vec<String> = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(api_names.len(), 4);
    for canonical in ["tasks", "github", "automation", "send_later"] {
        assert!(
            api_names.iter().any(|n| n == canonical),
            "{canonical} should be model-visible on the read-only surface"
        );
    }
    // Every registered tool stays read-only (Plan-mode invariant).
    for tool in registry.all() {
        let caps = tool.capabilities();
        assert!(
            !caps.contains(&ToolCapability::WritesFiles)
                && !caps.contains(&ToolCapability::ExecutesCode),
            "read-only surface must not register write/exec tools: {}",
            tool.name()
        );
    }
}

/// The action-shaped RLM family is registered only for compatibility.
#[test]
fn rlm_family_removes_legacy_aliases() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let registry = ToolRegistryBuilder::new().with_rlm_tool().build(ctx);

    for alias in [
        "rlm_session_objects",
        "rlm_open",
        "rlm_eval",
        "rlm_configure",
        "rlm_close",
    ] {
        assert!(!registry.contains(alias), "{alias} must stay removed");
    }

    let api_names: Vec<String> = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert!(
        api_names.iter().all(|n| n != "rlm"),
        "the compatibility RLM surface must not be advertised to new model turns"
    );
    for retired in [
        "rlm_session_objects",
        "rlm_open",
        "rlm_eval",
        "rlm_configure",
        "rlm_close",
    ] {
        assert!(
            api_names.iter().all(|n| n != retired),
            "{retired} must not be advertised"
        );
    }
}

#[test]
fn a_builder_upgrade_replaces_the_tool_instead_of_registering_it_twice() {
    // `with_patch_tools` swaps the default `File` for the patch-capable one;
    // that used to reach `register` as a second `File` and warn on every
    // registry rebuild (#5934).
    let builder = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_patch_tools();
    let file_tools = builder
        .tools
        .iter()
        .filter(|tool| tool.name() == "File")
        .count();
    assert_eq!(file_tools, 1, "one File tool after the upgrade");
    assert!(
        builder
            .tools
            .iter()
            .any(|tool| tool.name() == "apply_patch"),
        "the upgrade still adds apply_patch"
    );
    let tmp = tempdir().unwrap();
    let warnings = capture_registration_warnings(|| {
        let registry = builder.build(ToolContext::new(tmp.path()));
        assert!(registry.contains("File"));
        assert!(registry.contains("apply_patch"));
    });
    assert!(warnings.is_empty(), "normal File composition: {warnings}");
}

fn capture_registration_warnings(action: impl FnOnce()) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut output = tempfile::tempfile().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::sync::Mutex::new(output.try_clone().unwrap()))
        .finish();
    tracing::subscriber::with_default(subscriber, action);
    output.seek(SeekFrom::Start(0)).unwrap();
    let mut warnings = String::new();
    output.read_to_string(&mut warnings).unwrap();
    warnings
}

#[test]
fn runtime_surface_hardening_plugin_collisions_preserve_registered_tools() {
    let tmp = tempdir().unwrap();
    let mut registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .build(ToolContext::new(tmp.path()));
    let original = registry.get("File").unwrap();
    let original_catalog = registry.to_api_tools();
    std::fs::write(
        tmp.path().join("tool.sh"),
        "# name: File\n# description: custom file tool\n",
    )
    .unwrap();
    let errors = capture_registration_warnings(|| registry.load_plugins(tmp.path()));
    assert!(
        errors.contains("Cannot load plugin tool 'File': name is already registered"),
        "{errors}"
    );
    assert!(errors.contains(&format!(
        "previous_origin={:?}",
        original.registration_origin()
    )));
    let plugin_origin = format!(
        "plugin script tool.sh ({})",
        crate::safe_label::SafeLabel::identifier(&tmp.path().join("tool.sh").to_string_lossy())
    );
    assert!(errors.contains(&format!("plugin_origin={plugin_origin:?}")));
    assert!(!errors.contains(tmp.path().to_string_lossy().as_ref()));
    assert!(Arc::ptr_eq(&registry.get("File").unwrap(), &original));
    assert_eq!(
        serde_json::to_value(registry.to_api_tools()).unwrap(),
        serde_json::to_value(original_catalog).unwrap()
    );

    std::fs::write(
        tmp.path().join("other.sh"),
        "# name: custom-reader\n# description: custom reader\n",
    )
    .unwrap();
    registry.load_plugins(tmp.path());
    assert!(registry.contains("custom-reader"));

    // An explicit override still wins over a drop-in script of the same name;
    // it never replaces a built-in (see
    // `script_and_command_overrides_cannot_replace_builtins`).
    let builtins = std::collections::HashSet::from(["File".to_string()]);
    let overrides = std::collections::HashMap::from([(
        "custom-reader".to_string(),
        crate::config::ToolOverride::Command {
            command: "my-reader".to_string(),
            args: None,
        },
    )]);
    registry.apply_overrides(&overrides, tmp.path(), &builtins);
    assert!(
        registry
            .get("custom-reader")
            .unwrap()
            .description()
            .contains("my-reader")
    );
    assert!(Arc::ptr_eq(&registry.get("File").unwrap(), &original));
}

#[test]
fn registration_adapter_origins_are_bounded_and_exclude_execution_payloads() {
    use crate::tools::dynamic::RuntimeDynamicTool;
    use crate::tools::plugin::tool_from_override;
    use codewhale_protocol::runtime::DynamicToolSpec;
    let tmp = tempdir().unwrap();
    let hostile = format!(
        "\u{1b}[31m\nhttps://private.invalid/token?{}",
        "x".repeat(500)
    );
    let command = "do-not-log-command";
    let argument = "do-not-log-argument";
    let schema_payload = "do-not-log-schema";
    let cases: Vec<(Arc<dyn ToolSpec>, &str)> = vec![
        (
            Arc::new(RuntimeDynamicTool::new(DynamicToolSpec {
                name: hostile.clone(),
                namespace: Some(hostile.clone()),
                description: command.into(),
                input_schema: json!({"description":schema_payload}),
                defer_loading: false,
            })),
            "runtime dynamic namespace sha256:",
        ),
        (
            Arc::new(super::McpToolAdapter {
                name: hostile.clone(),
                server_name: Some("plugin-4-demo-server_with_underscores".into()),
                tool: crate::mcp::McpTool {
                    name: hostile.clone(),
                    description: Some(command.into()),
                    input_schema: json!({"description":schema_payload}),
                    annotations: None,
                },
                pool: Arc::new(tokio::sync::Mutex::new(crate::mcp::McpPool::new(
                    crate::mcp::McpConfig::default(),
                ))),
            }),
            "MCP server plugin-4-demo-server_with_underscores, tool sha256:",
        ),
        (
            tool_from_override(
                &hostile,
                &ToolOverride::Command {
                    command: command.into(),
                    args: Some(vec![argument.into()]),
                },
                tmp.path(),
            )
            .unwrap(),
            "config [tools.overrides.sha256:",
        ),
    ];
    for (replacement, expected_origin) in cases {
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path()));
        registry.register(make_test_tool(&hostile));
        let warnings = capture_registration_warnings(|| registry.register(replacement.clone()));
        assert_eq!(warnings.lines().count(), 1, "{warnings}");
        assert!(
            warnings.contains("Overwriting existing tool: sha256:"),
            "{warnings}"
        );
        assert!(warnings.contains(expected_origin), "{warnings}");
        assert!(warnings.len() < 600, "{warnings}");
        for excluded in [
            &hostile,
            command,
            argument,
            schema_payload,
            "https://private.invalid",
            "\u{1b}",
        ] {
            assert!(
                !warnings.contains(excluded),
                "unexpected payload: {warnings}"
            );
        }
        assert!(Arc::ptr_eq(&registry.get(&hostile).unwrap(), &replacement));
    }
}

/// Regression probe for the fleet-52663788 class of provider 400
/// (`Invalid schema for function 'bash': null is not of type "array"`):
/// a read-only Fleet worker (reviewer) projects its tool schemas before the
/// wire; no projected schema may carry a JSON null, because strict
/// OpenAI-compatible validators reject null where arrays/objects are typed.
#[test]
fn fleet_readonly_reviewer_wire_catalog_carries_no_null_schema_fields() {
    use crate::tools::spec::{
        ToolMutationAuthority, ToolShellAuthority, ToolVerificationAuthority,
    };

    fn collect_null_paths(value: &Value, path: String, out: &mut Vec<String>) {
        match value {
            Value::Null => out.push(path),
            Value::Object(map) => {
                for (key, child) in map {
                    collect_null_paths(child, format!("{path}.{key}"), out);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    collect_null_paths(child, format!("{path}[{index}]"), out);
                }
            }
            _ => {}
        }
    }

    let tmp = tempdir().expect("tempdir");
    let reviewer_authority = ToolAuthorityEnvelope {
        schema_version: 1,
        owner: "reviewer".to_string(),
        authority: ToolMutationAuthority::ReadOnly,
        network_access: Some(false),
        shell: ToolShellAuthority::ReadOnly,
        verification: ToolVerificationAuthority::None,
        writable_roots: Vec::new(),
        writable_files: Vec::new(),
        coordination_contracts: Vec::new(),
    };
    let context = ToolContext::new(tmp.path().to_path_buf())
        .with_tool_authority(reviewer_authority)
        .expect("reviewer authority");

    let registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_foreground_shell_tools()
        .with_search_tools()
        .build(context);
    let tools = registry.to_api_tools();
    assert!(
        tools.iter().any(|tool| tool.name == "bash"),
        "reviewer keeps classifier-bounded bash"
    );

    for tool in &tools {
        let mut nulls = Vec::new();
        collect_null_paths(&tool.input_schema, "$".to_string(), &mut nulls);
        assert!(
            nulls.is_empty(),
            "tool {} schema carries null at {nulls:?}: {}",
            tool.name,
            tool.input_schema
        );
    }
}

#[test]
fn read_media_is_not_offered_to_a_text_only_route() {
    use codewhale_config::route::CapabilityState;
    let tmp = tempdir().unwrap();
    for (state, offered) in [
        (CapabilityState::Unsupported, false),
        (CapabilityState::Unknown, true),
        (CapabilityState::Supported, true),
    ] {
        let mut context = ToolContext::new(tmp.path());
        context.route_capabilities.image_input = state;
        let registry = ToolRegistryBuilder::new()
            .with_read_media_tool()
            .build(context);
        assert_eq!(registry.get("read_media").is_some(), offered, "{state:?}");
    }
}

/// #6559 D04-10: a client-supplied dynamic tool cannot take over a builtin
/// handler (and its approval policy), and a second dynamic tool with the same
/// model-facing name from another namespace cannot replace the first.
#[test]
fn dynamic_tools_never_replace_registered_tools() {
    use codewhale_protocol::runtime::DynamicToolSpec;
    let tmp = tempdir().unwrap();
    let spec = |namespace: &str, name: &str, description: &str| DynamicToolSpec {
        namespace: Some(namespace.to_string()),
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({"type": "object"}),
        defer_loading: false,
    };
    let registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_dynamic_tools(&[
            spec("client", "read", "client read"),
            spec("first", "lookup", "first lookup"),
            spec("second", "lookup", "second lookup"),
        ])
        .build(ToolContext::new(tmp.path()));

    let read = registry.get("read").expect("builtin read");
    assert_ne!(read.description(), "client read");
    assert!(
        !read.registration_origin().contains("runtime dynamic"),
        "{}",
        read.registration_origin()
    );
    assert_eq!(
        registry
            .get("lookup")
            .expect("first dynamic tool")
            .description(),
        "first lookup"
    );
}
