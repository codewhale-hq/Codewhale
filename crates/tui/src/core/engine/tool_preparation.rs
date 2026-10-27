//! Side-effect-free preparation of concrete tool inputs.
//!
//! The turn loop remains the authority orchestrator. This module only makes
//! the input-specific policy decision inspectable and reusable, including a
//! mandatory second preparation after a hook rewrites input.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

use codewhale_execpolicy::ApprovalMode;

use crate::mcp::McpPool;
use crate::tools::ToolRegistry;
use crate::tools::approval_cache::{computer_use_batch_hidden_gate, computer_use_user_gate};
use crate::tools::spec::{ApprovalRequirement, PreparedToolCall, ResourceClaim, ToolError};

use super::dispatch::{
    mcp_tool_approval_description, mcp_tool_is_parallel_safe, mcp_tool_is_read_only,
};
use super::tool_catalog::{
    CODE_EXECUTION_TOOL_NAME, EXECUTE_TOOLS_TOOL_NAME, JS_EXECUTION_TOOL_NAME,
};
use codewhale_tools::names::is_tool_search_tool;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PreparedToolPolicy {
    pub(super) call: PreparedToolCall,
    pub(super) auto_approve: bool,
}

/// Prepare a concrete call without mutating external state.
pub(super) fn prepare_tool_call(
    name: &str,
    input: Value,
    registry: Option<&ToolRegistry>,
    session_auto_approve: bool,
) -> Result<PreparedToolPolicy, ToolError> {
    if McpPool::is_mcp_tool(name) {
        // CW-11: a reviewed plugin's `readOnlyHint` makes its tool run like
        // the built-in resource reads. A declared `destructiveHint` only
        // withholds that relaxation and labels the card: Full Access still
        // covers it (#3866), because a host that answers approvals from its
        // own flag (`exec` with a Full Access `approval_policy`) would
        // otherwise deny a call its posture already allows.
        let read_only = mcp_tool_is_read_only(name)
            || crate::mcp::mcp_tool_approval_hint(name)
                == Some(crate::mcp::McpToolApprovalHint::TrustedReadOnly);
        // A bounded worker keeps the execution gate's rule (built-in resource
        // reads only), so preparation never admits a call that
        // `tool_execution` then refuses.
        if !mcp_tool_is_read_only(name)
            && let Some(authority) =
                registry.and_then(|registry| registry.context().tool_authority.as_ref())
        {
            return Err(ToolError::permission_denied(format!(
                "worker '{}' cannot run mutating MCP tool {name}: it has no bounded file target under the machine-readable authority envelope",
                authority.owner
            )));
        }
        // K1/K2 stopgap: Computer Use consent and `app_script` need a human
        // decision. Never auto-approve them, and refuse them outright in a
        // posture that has no person at the card (Auto-Review, Never). Full
        // Access is not such a posture: it has a UI, so it asks. Denying there
        // made the most permissive posture the one posture where Computer Use
        // could never be authorized at all.
        if let Some(inner) = computer_use_batch_hidden_gate(name, &input) {
            return Err(ToolError::permission_denied(format!(
                "Computer Use {inner} cannot run inside {name}: consent and scripts need their own approval card. Call it on its own so the user can decide."
            )));
        }
        if computer_use_user_gate(name, &input).is_some() {
            let posture = registry.map(|registry| {
                let context = registry.context();
                (context.auto_approve, context.approval_mode)
            });
            let can_ask = posture.is_none_or(|(auto_approve, approval_mode)| {
                approval_mode == ApprovalMode::Suggest
                    || auto_approve
                    || approval_mode == ApprovalMode::Bypass
            });
            if !can_ask {
                let label = posture.map_or("Full Access", |(auto_approve, approval_mode)| {
                    if auto_approve {
                        ApprovalMode::Bypass.permission_chip_label()
                    } else {
                        approval_mode.permission_chip_label()
                    }
                });
                return Err(ToolError::permission_denied(format!(
                    "Computer Use call {name} needs your own approval, and the current {label} posture has no one to ask: consent and scripts cannot be granted by a model tool call. Switch to Ask or Full Access to review it."
                )));
            }
            return Ok(PreparedToolPolicy {
                call: PreparedToolCall {
                    name: name.to_string(),
                    description: mcp_tool_approval_description(name, &input),
                    input,
                    read_only: false,
                    supports_parallel: false,
                    starts_detached: false,
                    approval: ApprovalRequirement::Required,
                    resources: vec![ResourceClaim::GlobalExclusive],
                },
                auto_approve: false,
            });
        }
        return Ok(PreparedToolPolicy {
            call: PreparedToolCall {
                name: name.to_string(),
                description: mcp_tool_approval_description(name, &input),
                input,
                read_only,
                supports_parallel: mcp_tool_is_parallel_safe(name),
                starts_detached: false,
                approval: if read_only {
                    ApprovalRequirement::Auto
                } else {
                    ApprovalRequirement::Suggest
                },
                resources: vec![ResourceClaim::GlobalExclusive],
            },
            auto_approve: session_auto_approve,
        });
    }

    if let Some(registry) = registry
        && let Some(spec) = registry.get(name)
    {
        let mut call = spec.prepare(input, registry.context())?;
        call.resources = registered_resource_claims(name, &call.input, registry.context())?;
        return Ok(PreparedToolPolicy {
            call,
            auto_approve: registry.context().auto_approve,
        });
    }

    if name == CODE_EXECUTION_TOOL_NAME {
        reject_unbounded_execution_under_authority(name, registry)?;
        return Ok(conservative_execution_policy(
            name,
            input,
            "Run model-provided Python code with the current execution policy",
            session_auto_approve,
        ));
    }

    if name == EXECUTE_TOOLS_TOOL_NAME {
        reject_unbounded_execution_under_authority(name, registry)?;
        let first_line = input
            .get("code")
            .and_then(Value::as_str)
            .and_then(|code| code.lines().map(str::trim).find(|line| !line.is_empty()))
            .unwrap_or("execute_tools program");
        let mut policy = conservative_execution_policy(
            name,
            input.clone(),
            &format!("execute_tools: {first_line}"),
            session_auto_approve,
        );
        // #6562: every nested call is planned and approved through the same
        // gate as a direct call (or, without an engine gate, limited to
        // read-only auto-approved calls), so approving the program itself
        // would grant nothing. It stays exclusive and non-read-only.
        policy.call.approval = ApprovalRequirement::Auto;
        return Ok(policy);
    }

    if name == JS_EXECUTION_TOOL_NAME {
        reject_unbounded_execution_under_authority(name, registry)?;
        return Ok(conservative_execution_policy(
            name,
            input,
            "Run model-provided JavaScript code with the current execution policy",
            session_auto_approve,
        ));
    }

    if is_tool_search_tool(name) {
        return Ok(PreparedToolPolicy {
            call: PreparedToolCall {
                name: name.to_string(),
                input,
                description: "Search tool catalog".to_string(),
                read_only: true,
                supports_parallel: false,
                starts_detached: false,
                approval: ApprovalRequirement::Auto,
                resources: Vec::new(),
            },
            auto_approve: session_auto_approve,
        });
    }

    Err(ToolError::not_available(format!(
        "tool '{name}' has no preparation path"
    )))
}

fn reject_unbounded_execution_under_authority(
    name: &str,
    registry: Option<&ToolRegistry>,
) -> Result<(), ToolError> {
    let Some(authority) = registry.and_then(|registry| registry.context().tool_authority.as_ref())
    else {
        return Ok(());
    };
    Err(ToolError::permission_denied(format!(
        "worker '{}' cannot run {name}: arbitrary code execution cannot prove a bounded file target under the machine-readable authority envelope",
        authority.owner
    )))
}

/// Re-run preparation from the rewritten input rather than patching any
/// previously derived field.
pub(super) fn reprepare_tool_call_after_hook(
    name: &str,
    updated_input: Value,
    registry: Option<&ToolRegistry>,
    session_auto_approve: bool,
) -> Result<PreparedToolPolicy, ToolError> {
    prepare_tool_call(name, updated_input, registry, session_auto_approve)
}

fn conservative_execution_policy(
    name: &str,
    input: Value,
    description: &str,
    auto_approve: bool,
) -> PreparedToolPolicy {
    PreparedToolPolicy {
        call: PreparedToolCall {
            name: name.to_string(),
            input,
            description: description.to_string(),
            read_only: false,
            supports_parallel: false,
            starts_detached: false,
            approval: ApprovalRequirement::Suggest,
            resources: vec![ResourceClaim::GlobalExclusive],
        },
        auto_approve,
    }
}

fn registered_resource_claims(
    name: &str,
    input: &Value,
    context: &crate::tools::ToolContext,
) -> Result<Vec<ResourceClaim>, ToolError> {
    let canonical = codewhale_execpolicy::canonical_action::canonical_action_alias(name, input);
    match canonical {
        "read_file" => path_claim(input, "path", None, context, ResourceClaim::ReadPath),
        "write_file" | "edit_file" => {
            path_claim(input, "path", None, context, ResourceClaim::WritePath)
        }
        "list_dir" | "grep_files" | "file_search" => {
            path_claim(input, "path", Some("."), context, ResourceClaim::ReadTree)
        }
        "apply_patch" => apply_patch_resource_claims(input, context),
        "terminal/run" => Ok(terminal_claim(input, "session", Some("term-1"))),
        "terminal/send" | "terminal/wait" | "terminal/cancel" | "terminal/reset" => {
            Ok(terminal_claim(input, "session", None))
        }
        "exec_shell_wait"
        | "exec_wait"
        | "exec_shell_interact"
        | "exec_interact"
        | "exec_shell_cancel" => Ok(terminal_claim(input, "task_id", None)),
        _ => Ok(global_exclusive_claim()),
    }
}

fn path_claim(
    input: &Value,
    key: &str,
    default: Option<&str>,
    context: &crate::tools::ToolContext,
    build: fn(PathBuf) -> ResourceClaim,
) -> Result<Vec<ResourceClaim>, ToolError> {
    let raw = input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .or(default);
    let Some(raw) = raw else {
        return Ok(global_exclusive_claim());
    };
    Ok(context
        .resolve_path(raw)
        .map_or_else(|_| global_exclusive_claim(), |path| vec![build(path)]))
}

fn apply_patch_resource_claims(
    input: &Value,
    context: &crate::tools::ToolContext,
) -> Result<Vec<ResourceClaim>, ToolError> {
    let Ok(preflight) = codewhale_tools::patch_preflight::preflight_apply_patch(input) else {
        return Ok(global_exclusive_claim());
    };
    if preflight.touched_files.is_empty() {
        return Ok(global_exclusive_claim());
    }

    let mut claims = BTreeSet::new();
    for path in preflight.touched_files {
        let Ok(path) = context.resolve_path(&path) else {
            return Ok(global_exclusive_claim());
        };
        claims.insert(ResourceClaim::WritePath(path));
    }
    Ok(claims.into_iter().collect())
}

fn terminal_claim(input: &Value, key: &str, default: Option<&str>) -> Vec<ResourceClaim> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or(default)
        .map_or_else(global_exclusive_claim, |id| {
            vec![ResourceClaim::Terminal(id.to_string())]
        })
}

fn global_exclusive_claim() -> Vec<ResourceClaim> {
    vec![ResourceClaim::GlobalExclusive]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::tools::spec::{ToolCapability, ToolContext, ToolResult, ToolSpec};

    use super::*;

    struct InputDependentTool;

    #[async_trait]
    impl ToolSpec for InputDependentTool {
        fn name(&self) -> &str {
            "input_dependent"
        }

        fn description(&self) -> &str {
            "characterization tool"
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::WritesFiles]
        }

        fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
            if input.get("safe").and_then(Value::as_bool) == Some(true) {
                ApprovalRequirement::Auto
            } else {
                ApprovalRequirement::Required
            }
        }

        fn is_read_only_for(&self, input: &Value) -> bool {
            input.get("safe").and_then(Value::as_bool) == Some(true)
        }

        fn supports_parallel_for(&self, input: &Value) -> bool {
            self.is_read_only_for(input)
        }

        fn starts_detached_for(&self, input: &Value) -> bool {
            input.get("detached").and_then(Value::as_bool) == Some(true)
        }

        async fn execute(
            &self,
            _input: Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            unreachable!("preparation must not execute the tool")
        }
    }

    fn registry() -> (tempfile::TempDir, ToolRegistry) {
        let root = tempdir().expect("tempdir");
        let mut context = ToolContext::new(root.path().to_path_buf());
        context.auto_approve = true;
        let mut registry = ToolRegistry::new(context);
        registry.register(Arc::new(InputDependentTool));
        (root, registry)
    }

    #[test]
    fn prepared_policy_matches_existing_input_specific_decisions() {
        let (_root, registry) = registry();
        let spec = registry.get("input_dependent").expect("registered tool");

        for input in [
            json!({"safe": true, "detached": false}),
            json!({"safe": false, "detached": true}),
        ] {
            let prepared =
                prepare_tool_call("input_dependent", input.clone(), Some(&registry), false)
                    .expect("prepare");

            assert_eq!(
                prepared.call.approval,
                spec.approval_requirement_for(&input)
            );
            assert_eq!(prepared.call.read_only, spec.is_read_only_for(&input));
            assert_eq!(
                prepared.call.supports_parallel,
                spec.supports_parallel_for(&input)
            );
            assert_eq!(
                prepared.call.starts_detached,
                spec.starts_detached_for(&input)
            );
            assert!(prepared.auto_approve);
        }
    }

    #[test]
    fn hook_rewrite_discards_every_original_prepared_decision() {
        let (_root, registry) = registry();
        let original = prepare_tool_call(
            "input_dependent",
            json!({"safe": true, "detached": false}),
            Some(&registry),
            false,
        )
        .expect("prepare original");
        let rewritten = reprepare_tool_call_after_hook(
            "input_dependent",
            json!({"safe": false, "detached": true}),
            Some(&registry),
            false,
        )
        .expect("reprepare rewritten input");

        assert_eq!(original.call.approval, ApprovalRequirement::Auto);
        assert!(original.call.read_only);
        assert!(original.call.supports_parallel);
        assert!(!original.call.starts_detached);

        assert_eq!(rewritten.call.approval, ApprovalRequirement::Required);
        assert!(!rewritten.call.read_only);
        assert!(!rewritten.call.supports_parallel);
        assert!(rewritten.call.starts_detached);
        assert_eq!(
            rewritten.call.input,
            json!({"safe": false, "detached": true})
        );
    }

    #[test]
    fn bypass_preparation_preserves_legacy_policy_table() {
        struct Expected {
            name: &'static str,
            approval: ApprovalRequirement,
            read_only: bool,
            supports_parallel: bool,
            global_exclusive: bool,
        }

        for expected in [
            Expected {
                name: "read_mcp_resource",
                approval: ApprovalRequirement::Auto,
                read_only: true,
                supports_parallel: true,
                global_exclusive: true,
            },
            Expected {
                name: "mcp_filesystem_write",
                approval: ApprovalRequirement::Suggest,
                read_only: false,
                supports_parallel: false,
                global_exclusive: true,
            },
            Expected {
                name: CODE_EXECUTION_TOOL_NAME,
                approval: ApprovalRequirement::Suggest,
                read_only: false,
                supports_parallel: false,
                global_exclusive: true,
            },
            Expected {
                name: JS_EXECUTION_TOOL_NAME,
                approval: ApprovalRequirement::Suggest,
                read_only: false,
                supports_parallel: false,
                global_exclusive: true,
            },
            Expected {
                name: "tool_search",
                approval: ApprovalRequirement::Auto,
                read_only: true,
                supports_parallel: false,
                global_exclusive: false,
            },
        ] {
            let prepared = prepare_tool_call(expected.name, json!({}), None, false)
                .unwrap_or_else(|error| panic!("prepare {}: {error}", expected.name));
            assert_eq!(
                prepared.call.approval, expected.approval,
                "{}",
                expected.name
            );
            assert_eq!(
                prepared.call.read_only, expected.read_only,
                "{}",
                expected.name
            );
            assert_eq!(
                prepared.call.supports_parallel, expected.supports_parallel,
                "{}",
                expected.name
            );
            assert_eq!(
                prepared.call.resources == vec![ResourceClaim::GlobalExclusive],
                expected.global_exclusive,
                "{}",
                expected.name
            );
            assert!(!prepared.call.starts_detached, "{}", expected.name);
            assert!(!prepared.auto_approve, "{}", expected.name);
        }
    }

    #[test]
    fn mcp_annotation_hints_drive_approval() {
        use crate::mcp::{McpToolApprovalHint, set_mcp_tool_approval_hint_for_test};

        let read_only = "mcp_plugin-9-cw11test_page_snapshot";
        set_mcp_tool_approval_hint_for_test(read_only, Some(McpToolApprovalHint::TrustedReadOnly));
        let prepared = prepare_tool_call(read_only, json!({}), None, false)
            .expect("prepare trusted read-only MCP tool");
        assert_eq!(prepared.call.approval, ApprovalRequirement::Auto);
        assert!(prepared.call.read_only);

        // A bounded worker keeps the execution gate's rule: only the built-in
        // resource reads, so preparation never admits a call execution refuses.
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = crate::tools::ToolContext::new(workspace.path().to_path_buf())
            .with_tool_authority(crate::tools::spec::ToolAuthorityEnvelope {
                schema_version: 1,
                owner: "cw11-worker".to_string(),
                authority: crate::tools::spec::ToolMutationAuthority::ScopedWrite,
                network_access: None,
                shell: crate::tools::spec::ToolShellAuthority::None,
                verification: crate::tools::spec::ToolVerificationAuthority::None,
                writable_roots: Vec::new(),
                writable_files: vec!["src/named.rs".to_string()],
                coordination_contracts: Vec::new(),
            })
            .expect("valid envelope");
        let registry = crate::tools::ToolRegistry::new(context);
        let refused = prepare_tool_call(read_only, json!({}), Some(&registry), false)
            .expect_err("a bounded worker cannot run a plugin-declared read");
        assert!(refused.to_string().contains("cw11-worker"), "{refused}");

        let destructive = "mcp_cw11test_drop_table";
        set_mcp_tool_approval_hint_for_test(destructive, Some(McpToolApprovalHint::Destructive));
        let prepared = prepare_tool_call(destructive, json!({}), None, false)
            .expect("prepare destructive MCP tool");
        assert_eq!(prepared.call.approval, ApprovalRequirement::Suggest);
        assert!(!prepared.call.read_only);
        assert!(
            prepared.call.description.contains("destructive"),
            "{}",
            prepared.call.description
        );
        // Full Access covers it like any other promptable tool (#3866): a
        // host answering from its own flag must not deny what the posture
        // allows.
        let prepared = prepare_tool_call(destructive, json!({}), None, true)
            .expect("prepare destructive MCP tool under Full Access");
        assert!(prepared.auto_approve);

        set_mcp_tool_approval_hint_for_test(read_only, None);
        set_mcp_tool_approval_hint_for_test(destructive, None);
    }

    #[test]
    fn mcp_write_preparation_respects_session_auto_approval() {
        let prepared = prepare_tool_call("mcp_filesystem_write", json!({}), None, true)
            .expect("prepare MCP write tool with session auto-approval");

        assert_eq!(prepared.call.approval, ApprovalRequirement::Suggest);
        assert!(!prepared.call.read_only);
        assert!(!prepared.call.supports_parallel);
        assert_eq!(
            prepared.call.resources,
            vec![ResourceClaim::GlobalExclusive]
        );
        assert!(prepared.auto_approve);
        assert!(!super::super::turn_loop::registered_tool_approval_required(
            &prepared.call.name,
            prepared.call.approval,
            prepared.auto_approve,
        ));
    }

    /// K1: the model cannot grant itself Computer Use consent. A posture with
    /// no person at the card refuses at preparation; Ask and Full Access both
    /// reach the card, always require approval, are never session
    /// auto-approved, and a session grant for one app does not cover another.
    #[test]
    fn model_issued_computer_use_consent_never_auto_approves() {
        let consent = "mcp_plugin-12-computer-use-computer_consent";
        let allow_safari =
            json!({"action": "allow", "app": "Safari", "bundle_id": "com.apple.Safari"});
        let foreground = json!({"action": "allow", "scope": "foreground"});

        // Postures with nobody at the card refuse before approval routing.
        for (session_auto, context_auto, mode) in [
            (false, false, ApprovalMode::Auto),
            (false, false, ApprovalMode::Never),
        ] {
            let root = tempdir().expect("tempdir");
            let mut context = ToolContext::new(root.path().to_path_buf());
            context.auto_approve = context_auto;
            context.approval_mode = mode;
            let registry = ToolRegistry::new(context);
            for (name, input) in [
                (consent, allow_safari.clone()),
                (consent, foreground.clone()),
                (
                    "mcp_codewhale-cu_consent_revoke",
                    json!({"app": "Terminal"}),
                ),
                (
                    "mcp_plugin-12-computer-use-computer_app_script",
                    json!({"script": "do shell script \"id\""}),
                ),
            ] {
                let error = prepare_tool_call(name, input.clone(), Some(&registry), session_auto)
                    .expect_err("model-issued consent must not run without a human");
                assert!(
                    matches!(error, ToolError::PermissionDenied { .. }),
                    "{name} {mode:?}: {error}"
                );
            }
        }
        // Full Access shows the card instead of refusing it. The call still
        // requires approval and is never auto-approved, so a posture cannot
        // stand in for the person's answer.
        for (session_auto, context_auto, mode) in [
            (true, false, ApprovalMode::Suggest),
            (false, true, ApprovalMode::Suggest),
            (false, false, ApprovalMode::Bypass),
        ] {
            let root = tempdir().expect("tempdir");
            let mut context = ToolContext::new(root.path().to_path_buf());
            context.auto_approve = context_auto;
            context.approval_mode = mode;
            let registry = ToolRegistry::new(context);
            let prepared =
                prepare_tool_call(consent, allow_safari.clone(), Some(&registry), session_auto)
                    .expect("Full Access reaches the consent card");
            assert_eq!(prepared.call.approval, ApprovalRequirement::Required);
            assert!(!prepared.auto_approve, "{mode:?}");
            assert!(super::super::turn_loop::registered_tool_approval_required(
                &prepared.call.name,
                prepared.call.approval,
                prepared.auto_approve,
            ));
        }

        // Ask posture: a Required card that names the app, bundle and scope.
        let root = tempdir().expect("tempdir");
        let registry = ToolRegistry::new(ToolContext::new(root.path().to_path_buf()));
        let prepared = prepare_tool_call(consent, allow_safari.clone(), Some(&registry), false)
            .expect("Ask posture opens a card");
        assert_eq!(prepared.call.approval, ApprovalRequirement::Required);
        assert!(!prepared.auto_approve);
        assert!(!prepared.call.read_only);
        assert!(super::super::turn_loop::registered_tool_approval_required(
            &prepared.call.name,
            prepared.call.approval,
            prepared.auto_approve,
        ));
        let description = &prepared.call.description;
        assert!(description.contains("Safari"), "{description}");
        assert!(description.contains("com.apple.Safari"), "{description}");
        assert!(description.contains("scope: app"), "{description}");
        let foreground_card = prepare_tool_call(consent, foreground, Some(&registry), false)
            .expect("foreground card");
        assert!(
            foreground_card
                .call
                .description
                .contains("scope: foreground")
        );
        // An irreversible-action confirm token is named as such, not as an
        // "<unnamed app>" consent; a multi-line script says it is truncated.
        let confirm_card = prepare_tool_call(
            consent,
            json!({"action": "allow", "confirm": "tok-1"}),
            Some(&registry),
            false,
        )
        .expect("confirm card");
        assert_eq!(confirm_card.call.approval, ApprovalRequirement::Required);
        assert!(
            confirm_card
                .call
                .description
                .contains("irreversible action"),
            "{}",
            confirm_card.call.description
        );
        let script_card = prepare_tool_call(
            "mcp_plugin-12-computer-use-computer_app_script",
            json!({"script": "tell application \"Finder\" to activate\ndo shell script \"id\""}),
            Some(&registry),
            false,
        )
        .expect("script card");
        assert!(
            script_card.call.description.contains("first of 2 lines"),
            "{}",
            script_card.call.description
        );

        // After a session grant for Safari, a consent for Terminal still
        // prompts: the grant key is the exact call, not the MCP kind.
        let granted =
            crate::tools::approval_cache::build_approval_grouping_key(consent, &allow_safari);
        let terminal = crate::tools::approval_cache::build_approval_grouping_key(
            consent,
            &json!({"action": "allow", "app": "Terminal", "bundle_id": "com.apple.Terminal"}),
        );
        assert_ne!(granted, terminal);

        // K1: run_actions cannot smuggle a consent grant or a script past
        // the per-call card, in any posture (Ask included).
        let batch = "mcp_plugin-12-computer-use-computer_run_actions";
        for (step, session_auto) in [
            (
                json!({"tool": "consent_allow", "arguments": {"app": "Terminal"}}),
                false,
            ),
            (
                json!({"tool": "consent", "arguments": {"action": "allow", "scope": "foreground"}}),
                false,
            ),
            (
                json!({"tool": "consent_revoke", "arguments": {"app": "Terminal"}}),
                true,
            ),
            (
                json!({"tool": "app_script", "arguments": {"script": "do shell script \"id\""}}),
                false,
            ),
        ] {
            let input = json!({"steps": [{"tool": "click", "arguments": {"x": 1, "y": 1}}, step]});
            let error = prepare_tool_call(batch, input, Some(&registry), session_auto)
                .expect_err("a batched consent or script must be refused");
            assert!(
                matches!(error, ToolError::PermissionDenied { .. }),
                "{error}"
            );
        }
        let plain_batch = json!({"steps": [
            {"tool": "click", "arguments": {"x": 1, "y": 1}},
            {"tool": "consent", "arguments": {"action": "status"}},
        ]});
        assert!(prepare_tool_call(batch, plain_batch, Some(&registry), false).is_ok());

        // Reading the ledger is unaffected.
        let status = prepare_tool_call(consent, json!({"action": "status"}), Some(&registry), true)
            .expect("status is not gated");
        assert!(status.auto_approve);
    }

    #[test]
    fn hook_rewrite_reprepares_resource_claims_from_final_input() {
        let root = tempdir().expect("tempdir");
        let context = ToolContext::new(root.path().to_path_buf());
        let original_path = context.resolve_path("before.rs").expect("original path");
        let rewritten_path = context.resolve_path("after.rs").expect("rewritten path");
        let mut registry = ToolRegistry::new(context);
        registry.register(Arc::new(crate::tools::file::ReadFileTool));

        let original = prepare_tool_call(
            "read_file",
            json!({"path": "before.rs"}),
            Some(&registry),
            false,
        )
        .expect("prepare original read");
        let rewritten = reprepare_tool_call_after_hook(
            "read_file",
            json!({"path": "after.rs"}),
            Some(&registry),
            false,
        )
        .expect("reprepare rewritten read");

        assert_eq!(
            original.call.resources,
            vec![ResourceClaim::ReadPath(original_path)]
        );
        assert_eq!(
            rewritten.call.resources,
            vec![ResourceClaim::ReadPath(rewritten_path)]
        );
    }

    #[test]
    fn registered_file_claims_are_canonical_and_input_specific() {
        let root = tempdir().expect("tempdir");
        let context = ToolContext::new(root.path().to_path_buf());
        let exact = context.resolve_path("src/lib.rs").expect("exact path");
        let tree = context.resolve_path("src").expect("tree path");

        assert_eq!(
            registered_resource_claims("read_file", &json!({"path": "src/lib.rs"}), &context,)
                .expect("read claim"),
            vec![ResourceClaim::ReadPath(exact.clone())]
        );
        assert_eq!(
            registered_resource_claims("edit_file", &json!({"path": "src/lib.rs"}), &context,)
                .expect("write claim"),
            vec![ResourceClaim::WritePath(exact)]
        );
        assert_eq!(
            registered_resource_claims("grep_files", &json!({"path": "src"}), &context)
                .expect("tree claim"),
            vec![ResourceClaim::ReadTree(tree)]
        );
        assert_eq!(
            registered_resource_claims("read_file", &json!({"path": "../../outside"}), &context,)
                .expect("path escape must fall back conservatively"),
            vec![ResourceClaim::GlobalExclusive]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_resolve_to_the_same_file_claim() {
        use std::os::unix::fs::symlink;

        let root = tempdir().expect("tempdir");
        let real_dir = root.path().join("real");
        std::fs::create_dir(&real_dir).expect("create real directory");
        std::fs::write(real_dir.join("lib.rs"), "fn main() {}\n").expect("write real file");
        symlink("real", root.path().join("alias")).expect("create directory symlink");
        let context = ToolContext::new(root.path().to_path_buf());
        let canonical_real = real_dir
            .join("lib.rs")
            .canonicalize()
            .expect("canonical file");

        let read_alias =
            registered_resource_claims("read_file", &json!({"path": "alias/lib.rs"}), &context)
                .expect("alias claim");
        let write_real =
            registered_resource_claims("write_file", &json!({"path": "real/lib.rs"}), &context)
                .expect("real claim");

        assert_eq!(read_alias, vec![ResourceClaim::ReadPath(canonical_real)]);
        assert!(read_alias[0].conflicts_with(&write_real[0]));
    }

    #[test]
    fn apply_patch_claims_every_resolved_target_or_falls_back_global() {
        let root = tempdir().expect("tempdir");
        let context = ToolContext::new(root.path().to_path_buf());
        let a = context.resolve_path("a.rs").expect("a path");
        let b = context.resolve_path("b.rs").expect("b path");

        let claims = registered_resource_claims(
            "apply_patch",
            &json!({
                "replace": [
                    {"path": "b.rs", "content": "b"},
                    {"path": "a.rs", "content": "a"}
                ]
            }),
            &context,
        )
        .expect("patch claims");
        assert_eq!(
            claims,
            vec![ResourceClaim::WritePath(a), ResourceClaim::WritePath(b)]
        );

        assert_eq!(
            registered_resource_claims(
                "apply_patch",
                &json!({"patch": "not a unified diff"}),
                &context,
            )
            .expect("fallback claim"),
            vec![ResourceClaim::GlobalExclusive]
        );
        assert_eq!(
            registered_resource_claims(
                "apply_patch",
                &json!({"replace": [{"path": "../../outside", "content": "nope"}]}),
                &context,
            )
            .expect("escaped target fallback"),
            vec![ResourceClaim::GlobalExclusive]
        );
    }

    #[test]
    fn terminal_and_unknown_tools_keep_conservative_claims() {
        let root = tempdir().expect("tempdir");
        let context = ToolContext::new(root.path().to_path_buf());

        assert_eq!(
            registered_resource_claims("terminal/run", &json!({}), &context)
                .expect("default terminal"),
            vec![ResourceClaim::Terminal("term-1".to_string())]
        );
        assert_eq!(
            registered_resource_claims(
                "exec_shell_interact",
                &json!({"task_id": "task-7"}),
                &context,
            )
            .expect("task terminal"),
            vec![ResourceClaim::Terminal("task-7".to_string())]
        );
        assert_eq!(
            registered_resource_claims("plugin_tool", &json!({}), &context).expect("unknown tool"),
            vec![ResourceClaim::GlobalExclusive]
        );
    }
}
