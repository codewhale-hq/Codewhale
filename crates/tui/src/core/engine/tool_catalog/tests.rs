use super::{
    CODE_EXECUTION_DESCRIPTION, DEFAULT_ACTIVE_NATIVE_TOOLS, ToolMode,
    allowlist_is_native_file_and_shell_only, apply_mcp_tool_deferral, apply_native_tool_deferral,
    build_model_tool_catalog_with_surface, default_synthetic_catalog_tool_names,
    ensure_advanced_tooling, execute_tool_search_with_cache, initial_active_tools,
    is_synthetic_catalog_tool, remove_evicted_cache_activations, requested_tool_mode,
    tool_matches_any_rule,
};
use crate::core::session::ToolActivationCache;
use codewhale_config::AppMode;
use codewhale_models::Tool;
use serde_json::json;
use std::collections::{BTreeSet, HashSet};

fn tool(name: &str) -> Tool {
    Tool {
        tool_type: None,
        name: name.to_string(),
        description: format!("{name} test tool"),
        input_schema: json!({"type": "object", "properties": {}}),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    }
}

/// The shared launcher applies policy only where enforcement is available.
/// The model-facing description must not promise unconditional isolation.
#[test]
fn code_execution_description_does_not_claim_process_sandboxing() {
    assert!(CODE_EXECUTION_DESCRIPTION.contains("local Python interpreter"));
    assert!(!CODE_EXECUTION_DESCRIPTION.contains("sandbox"));
}

/// Python output must match our UTF-8 decoder even with non-UTF-8 parent stdio.
#[tokio::test]
async fn code_execution_returns_utf8_stdout_and_stderr() {
    use crate::dependencies::ExternalTool as _;
    use crate::test_support::{EnvVarGuard, lock_test_env};

    let _env_lock = lock_test_env();
    if !crate::dependencies::Python::available() {
        return;
    }
    let _encoding = EnvVarGuard::set("PYTHONIOENCODING", "gbk");
    let tmp = tempfile::tempdir().expect("tempdir");
    let result = super::execute_code_execution_tool(
        &json!({"code": r#"import sys; print("\u4e2d\u6587"); print("\u9519\u8bef", file=sys.stderr)"#}),
        tmp.path(),
        &crate::tools::spec::ToolContext::new(tmp.path()),
    )
    .await
    .expect("code execution should run");
    let payload = result.metadata.expect("payload");
    assert_eq!(
        (
            payload["stdout"].as_str().map(str::trim_end),
            payload["stderr"].as_str().map(str::trim_end),
        ),
        (Some("中文"), Some("错误")),
    );
}

/// The published synthetic-name list and the predicate that classifies a
/// catalog entry as synthetic must agree. A name that appears in the list
/// but is not classified synthetic would let the request projection report
/// a provenance the engine itself disputes.
#[test]
fn published_synthetic_names_agree_with_the_synthetic_predicate() {
    let names = default_synthetic_catalog_tool_names();
    assert!(!names.is_empty());
    for name in &names {
        assert!(
            is_synthetic_catalog_tool(name),
            "'{name}' is published as synthetic but the predicate disagrees"
        );
    }
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names, sorted, "the list must be sorted and deduplicated");

    // MCP names resolve through the real pool, so they are deliberately
    // absent here even though the predicate accepts them.
    assert!(!names.iter().any(|name| name.starts_with("mcp_")));

    // `multi_tool_use.parallel` is a call name, never a catalog entry, so
    // it has no catalog provenance and must not be published as synthetic.
    assert!(
        !names
            .iter()
            .any(|name| name == super::MULTI_TOOL_PARALLEL_NAME)
    );
}

#[test]
fn first_turn_surface_is_stable_across_plan_work_and_operate() {
    // Token diet: agent+workflow (21KB of schema) and get/update_goal
    // (meaningless without an active goal) stay deferred; discovery via
    // tool_search restores them on demand.
    assert_eq!(
        DEFAULT_ACTIVE_NATIVE_TOOLS,
        &[
            "read",
            "write",
            "edit",
            "file_search",
            "bash",
            "todo_write",
            "task_shell_wait",
            "load_skill",
            "create_goal"
        ]
    );
    let expected = [
        "bash",
        "create_goal",
        "edit",
        "file_search",
        "load_skill",
        "read",
        "task_shell_wait",
        "todo_write",
        "tool_search",
        "write",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    let mut expected_prefix = None;
    for mode in [AppMode::Plan, AppMode::Agent, AppMode::Operate] {
        let mut catalog = [
            "read",
            "write",
            "edit",
            "file_search",
            "bash",
            "agent",
            "workflow",
            "todo_write",
            "create_goal",
            "get_goal",
            "update_goal",
            "task_shell_wait",
            "Git",
            "Run",
            "tasks",
            "load_skill",
        ]
        .into_iter()
        .map(tool)
        .collect::<Vec<_>>();
        let always_load = HashSet::new();
        apply_native_tool_deferral(&mut catalog, &always_load);
        ensure_advanced_tooling(&mut catalog, mode, &always_load, ToolMode::Direct);
        let active_names = initial_active_tools(&catalog);
        let active = active_names.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(active, expected, "{mode:?}");
        let prefix = serde_json::to_string(&super::active_tools_for_step(&catalog, &active_names))
            .expect("serialize first-turn tool prefix");
        if let Some(expected) = &expected_prefix {
            assert_eq!(&prefix, expected, "{mode:?} must preserve schema bytes");
        } else {
            expected_prefix = Some(prefix);
        }
    }
}

#[test]
fn eager_workflow_still_respects_command_allow_and_deny_gates() {
    for mode in [AppMode::Plan, AppMode::Agent, AppMode::Operate] {
        for (allow, deny, expected) in [
            (None, None, true),
            (Some("read"), None, false),
            (Some("workflow"), None, true),
            (None, Some("workflow"), false),
            (Some("workflow"), Some("workflow"), false),
        ] {
            let catalog = build_model_tool_catalog_with_surface(
                ["read", "agent", "workflow"]
                    .into_iter()
                    .map(tool)
                    .collect(),
                Vec::new(),
                mode,
                &HashSet::new(),
                crate::model_profile::ToolSurfaceBudget::Standard,
            );
            let policy = super::ToolSurfacePolicy::new(
                crate::tools::ToolRegistry::new(crate::tools::ToolContext::for_empty_registry()),
                Some(catalog),
                mode,
                &HashSet::new(),
                &["workflow"], // Cached activation cannot restore a denied tool.
                false,
                allow.map(|name| vec![name.to_string()]),
                deny.map(|name| vec![name.to_string()]),
                None,
                ToolMode::Direct,
            );
            assert_eq!(policy.allows_tool("workflow"), expected);
            assert_eq!(policy.active_names.contains("workflow"), expected);
            assert_eq!(
                policy.catalog.iter().any(|tool| tool.name == "workflow"),
                expected
            );
        }
    }
}

#[test]
fn mcp_tools_are_searchable_not_eager_in_every_mode() {
    for mode in [AppMode::Plan, AppMode::Agent] {
        let mut catalog = vec![tool("read_mcp_resource"), tool("mcp_acme_lookup")];
        apply_mcp_tool_deferral(&mut catalog, mode, &HashSet::new());
        assert!(
            catalog
                .iter()
                .all(|definition| definition.defer_loading == Some(true)),
            "{mode:?}: {catalog:?}"
        );
    }
}

#[test]
fn cache_eviction_does_not_hide_a_tool_that_became_eager() {
    let mut catalog = vec![tool("promoted")];
    catalog[0].defer_loading = Some(true);
    let mut cache = ToolActivationCache::default();
    cache.activate(&catalog, &["promoted".to_string()]);

    catalog[0].defer_loading = Some(false);
    let mut active = initial_active_tools(&catalog);
    let evicted = cache.revalidate(&catalog);
    remove_evicted_cache_activations(&catalog, &mut active, evicted);

    assert!(active.contains("promoted"));
    assert_eq!(cache.names().count(), 0);
}

#[test]
fn successful_cached_execution_updates_lru_without_granting_uncached_names() {
    let catalog = (0..=8)
        .map(|index| {
            let mut definition = tool(&format!("deferred-{index}"));
            definition.defer_loading = Some(true);
            definition
        })
        .collect::<Vec<_>>();
    let mut cache = ToolActivationCache::default();
    let first = (0..8)
        .map(|index| format!("deferred-{index}"))
        .collect::<Vec<_>>();
    let mut active = HashSet::new();
    let delta = cache.activate(&catalog, &first);
    active.extend(delta.admitted);

    assert!(cache.record_use("deferred-7"));
    let delta = cache.activate(&catalog, &["deferred-8".to_string()]);
    remove_evicted_cache_activations(&catalog, &mut active, delta.evicted);
    active.extend(delta.admitted);
    // The batch is ranked best-first; using its oldest retained match
    // promotes it ahead of the next batch without granting an unseen tool.
    assert!(cache.names().any(|name| name == "deferred-7"));
    assert!(cache.names().any(|name| name == "deferred-0"));
    assert!(!cache.names().any(|name| name == "deferred-6"));

    assert!(!cache.record_use("never-activated"));
    assert!(!active.contains("never-activated"));
}

#[test]
fn searching_for_an_eager_tool_is_not_reported_as_cache_rejected() {
    let mut catalog = vec![tool("read")];
    catalog[0].defer_loading = Some(false);
    let mut active = initial_active_tools(&catalog);
    let mut cache = ToolActivationCache::default();

    let result = execute_tool_search_with_cache(
        super::TOOL_SEARCH_NAME,
        &json!({"query": "read"}),
        &catalog,
        &mut active,
        &mut cache,
    )
    .expect("tool search should succeed");

    let metadata = result.metadata.expect("search metadata");
    assert_eq!(metadata["tool_references"], json!([]));
    assert_eq!(metadata["unavailable_tool_references"], json!([]));
    assert!(active.contains("read"));
    assert_eq!(cache.names().count(), 0);
}

#[test]
fn allow_and_deny_rules_cover_visible_and_hidden_compat_aliases_symmetrically() {
    for family in [
        &["read", "read_file"][..],
        &["write", "write_file"][..],
        &["edit", "edit_file"][..],
        &["bash", "Bash", "exec_shell"][..],
    ] {
        for rule in family {
            let rules = vec![(*rule).to_string()];
            for tool_name in family {
                assert!(
                    tool_matches_any_rule(&rules, tool_name),
                    "rule {rule:?} should cover alias {tool_name:?}"
                );
            }
        }
    }

    assert!(tool_matches_any_rule(&["exec_shell*".to_string()], "bash"));
    assert!(tool_matches_any_rule(
        &["exec_shell*".to_string()],
        "exec_shell_wait"
    ));
    for primitive in ["read", "write", "edit"] {
        assert!(tool_matches_any_rule(&["File".to_string()], primitive));
    }
    assert!(!tool_matches_any_rule(&["read".to_string()], "write"));
}

#[test]
fn native_file_and_shell_allowlist_can_skip_mcp_startup() {
    let native = ["bash", "read", "write", "edit"].map(str::to_string);
    assert!(allowlist_is_native_file_and_shell_only(Some(&native)));
    assert!(!allowlist_is_native_file_and_shell_only(None));
}

#[test]
fn unknown_and_wildcard_allowlists_keep_mcp_startup() {
    for rule in ["mcp_github_list_prs", "mcp_*", "m*", "*", "other_tool"] {
        assert!(
            !allowlist_is_native_file_and_shell_only(Some(&[rule.to_string()])),
            "{rule} may admit an MCP-backed tool"
        );
    }
}

#[test]
fn compact_surface_keeps_agent_and_workflow_deferred() {
    let catalog = build_model_tool_catalog_with_surface(
        [
            "read",
            "write",
            "edit",
            "bash",
            "agent",
            "workflow",
            "todo_write",
        ]
        .into_iter()
        .map(tool)
        .collect(),
        Vec::new(),
        AppMode::Agent,
        &HashSet::new(),
        crate::model_profile::ToolSurfaceBudget::Compact,
    );

    for name in ["agent", "workflow"] {
        assert_eq!(
            catalog
                .iter()
                .find(|definition| definition.name == name)
                .and_then(|definition| definition.defer_loading),
            Some(true),
            "{name}"
        );
    }
}

#[test]
fn deferred_core_tools_restore_through_always_load() {
    // The diet's escape hatch: discovery/activation and the goal-aware
    // catalog build in `engine.rs` re-eager exactly these names.
    let mut catalog = ["agent", "workflow", "get_goal", "update_goal", "read"]
        .into_iter()
        .map(tool)
        .collect::<Vec<_>>();
    let always_load = ["agent", "workflow", "get_goal", "update_goal"]
        .into_iter()
        .map(str::to_string)
        .collect::<HashSet<_>>();
    apply_native_tool_deferral(&mut catalog, &always_load);
    for definition in &catalog {
        assert_eq!(definition.defer_loading, Some(false), "{}", definition.name);
    }
}

/// The per-tool Registry paragraph is gone; the catalog builder must leave the
/// shell tool's description exactly as the registry produced it, so the KV
/// prefix stays byte-stable and there is one Registry authority (the prompt).
#[test]
fn catalog_build_does_not_append_registry_guidance_to_the_shell_tool() {
    let described = tool("bash");
    let catalog = build_model_tool_catalog_with_surface(
        vec![described.clone()],
        Vec::new(),
        AppMode::Agent,
        &HashSet::new(),
        crate::model_profile::ToolSurfaceBudget::Standard,
    );

    let shell = catalog
        .iter()
        .find(|definition| definition.name == "bash")
        .expect("shell tool");
    assert_eq!(shell.description, described.description);
    assert!(!shell.description.contains("registry_sync"));
}

#[test]
fn requested_tool_mode_prefers_model_hint_then_flag_then_direct() {
    use crate::features::{Feature, Features};

    // #6562: code mode is the default; `[features] code_mode = false` is the
    // escape hatch back to Direct.
    let on = Features::with_defaults();
    assert_eq!(requested_tool_mode(None, &on), ToolMode::CodeMode);

    let mut off = Features::with_defaults();
    off.disable(Feature::CodeMode);
    assert_eq!(requested_tool_mode(None, &off), ToolMode::Direct);

    // Model metadata wins over config, in both directions (Codex parity).
    assert_eq!(
        requested_tool_mode(Some(ToolMode::Direct), &on),
        ToolMode::Direct
    );
    assert_eq!(
        requested_tool_mode(Some(ToolMode::CodeMode), &off),
        ToolMode::CodeMode
    );
}

#[test]
fn execute_tools_is_eager_in_code_mode_and_deferred_in_direct() {
    for (mode, expected_defer) in [(ToolMode::Direct, true), (ToolMode::CodeMode, false)] {
        let mut catalog = vec![tool("read")];
        ensure_advanced_tooling(&mut catalog, AppMode::Agent, &HashSet::new(), mode);
        let injected = catalog
            .iter()
            .find(|definition| definition.name == "execute_tools")
            .expect("execute_tools is injected outside Plan");
        assert_eq!(injected.defer_loading, Some(expected_defer), "{mode:?}");
    }

    // Plan hides the surface under every tool mode.
    let mut catalog = vec![tool("read")];
    ensure_advanced_tooling(
        &mut catalog,
        AppMode::Plan,
        &HashSet::new(),
        ToolMode::CodeMode,
    );
    assert!(
        catalog
            .iter()
            .all(|definition| definition.name != "execute_tools")
    );
}

/// A timed-out or cancelled `code_execution` kills the interpreter and what the
/// script started; a bare `output()` left both running.
#[cfg(unix)]
#[tokio::test]
async fn dropped_code_execution_kills_the_interpreter_tree() {
    if crate::dependencies::resolve_python_interpreter().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_file = tmp.path().join("grandchild.pid");
    let code = format!(
        "import subprocess\np = subprocess.Popen(['sleep', '300'])\nopen({:?}, 'w').write(str(p.pid))\np.wait()\n",
        pid_file.display().to_string()
    );
    let input = json!({ "code": code });
    let context = crate::tools::spec::ToolContext::new(tmp.path());
    let run = super::execute_code_execution_tool(&input, tmp.path(), &context);
    let grandchild = crate::process_tree::drop_once_pid_written(run, &pid_file).await;
    assert!(
        crate::process_tree::wait_for_pid_exit(grandchild, std::time::Duration::from_secs(5)),
        "a process the script started outlived the dropped code_execution call"
    );
}
