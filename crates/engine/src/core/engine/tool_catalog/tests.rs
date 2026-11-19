use super::prepare_tools_for_strict_mode;
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
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use codewhale_dependencies::ExternalTool as _;

    let _env_lock = lock_test_env();
    if !codewhale_dependencies::Python::available() {
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
            .any(|name| name == codewhale_tools::names::MULTI_TOOL_PARALLEL_NAME)
    );
}

#[test]
fn first_turn_surface_is_stable_across_plan_work_and_operate() {
    assert_eq!(
        DEFAULT_ACTIVE_NATIVE_TOOLS,
        &[
            "read",
            "write",
            "edit",
            "list_dir",
            "file_search",
            "grep_files",
            "bash",
            "agent",
            "todo_write",
            "create_goal",
            "present",
            "load_skill"
        ]
    );
    let expected = [
        "agent",
        "bash",
        "create_goal",
        "edit",
        "file_search",
        "grep_files",
        "list_dir",
        "load_skill",
        "present",
        "read",
        "todo_write",
        "tool_search",
        "write",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    let mut expected_prefix = None;
    for mode in [AppMode::Plan, AppMode::Agent] {
        let mut catalog = [
            "read",
            "write",
            "edit",
            "list_dir",
            "file_search",
            "grep_files",
            "bash",
            "agent",
            "workflow",
            "todo_write",
            "create_goal",
            "get_goal",
            "update_goal",
            "task_shell_wait",
            "present",
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
    for mode in [AppMode::Plan, AppMode::Agent] {
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
    let max = crate::core::session::TOOL_ACTIVATION_CACHE_MAX_NAMES;
    let catalog = (0..=max)
        .map(|index| {
            let mut definition = tool(&format!("deferred-{index}"));
            definition.defer_loading = Some(true);
            definition
        })
        .collect::<Vec<_>>();
    let mut cache = ToolActivationCache::default();
    let first = (0..max)
        .map(|index| format!("deferred-{index}"))
        .collect::<Vec<_>>();
    let mut active = HashSet::new();
    let delta = cache.activate(&catalog, &first);
    active.extend(delta.admitted);

    let oldest = format!("deferred-{}", max - 1);
    let next_oldest = format!("deferred-{}", max - 2);
    assert!(cache.record_use(&oldest));
    let delta = cache.activate(&catalog, &[format!("deferred-{max}")]);
    remove_evicted_cache_activations(&catalog, &mut active, delta.evicted);
    active.extend(delta.admitted);
    // The batch is ranked best-first; using its oldest retained match
    // promotes it ahead of the next batch without granting an unseen tool.
    assert!(cache.names().any(|name| name == oldest));
    assert!(cache.names().any(|name| name == "deferred-0"));
    assert!(!cache.names().any(|name| name == next_oldest));

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
        codewhale_tools::names::TOOL_SEARCH_NAME,
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
fn regex_search_matches_displayed_tool_names_case_insensitively() {
    let mut catalog = vec![tool("Web"), tool("automation")];
    for tool in &mut catalog {
        tool.defer_loading = Some(true);
    }
    let mut active = initial_active_tools(&catalog);
    let mut cache = ToolActivationCache::default();

    let result = execute_tool_search_with_cache(
        codewhale_tools::names::TOOL_SEARCH_NAME,
        &json!({"query": "^(automation|Web)$", "match": "regex"}),
        &catalog,
        &mut active,
        &mut cache,
    )
    .expect("tool search should succeed");

    let metadata = result.metadata.expect("search metadata");
    assert_eq!(metadata["tool_references"], json!(["Web", "automation"]));

    let err = execute_tool_search_with_cache(
        codewhale_tools::names::TOOL_SEARCH_NAME,
        &json!({"query": "[unclosed", "match": "regex"}),
        &catalog,
        &mut active,
        &mut cache,
    )
    .expect_err("invalid regex should fail")
    .to_string();
    assert!(err.contains("[unclosed"), "{err}");
    assert!(!err.contains("(?i)"), "{err}");
}

#[test]
fn tool_search_description_lists_only_deferred_tools_in_the_catalog() {
    let mut catalog = vec![tool("read"), tool("Web"), tool("lsp")];
    apply_native_tool_deferral(&mut catalog, &HashSet::new());
    catalog.retain(|tool| tool.name != "lsp");
    ensure_advanced_tooling(
        &mut catalog,
        AppMode::Plan,
        &HashSet::new(),
        ToolMode::Direct,
    );
    let description = &catalog
        .iter()
        .find(|tool| tool.name == codewhale_tools::names::TOOL_SEARCH_NAME)
        .expect("tool_search")
        .description;

    assert!(description.contains("`Web`"));
    assert!(!description.contains("`Git`"));
    assert!(!description.contains("`Run`"));
    assert!(!description.contains("`lsp`"));
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
fn compact_surface_keeps_agent_eager_defers_workflow() {
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

    for (name, deferred) in [("agent", false), ("workflow", true)] {
        assert_eq!(
            catalog
                .iter()
                .find(|definition| definition.name == name)
                .and_then(|definition| definition.defer_loading),
            Some(deferred),
            "{name}"
        );
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
    use codewhale_runtime::features::{Feature, Features};

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
    if codewhale_dependencies::resolve_python_interpreter().is_none() {
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
    let grandchild = codewhale_runtime::process_tree::drop_once_pid_written(run, &pid_file).await;
    assert!(
        codewhale_runtime::process_tree::wait_for_pid_exit(
            grandchild,
            std::time::Duration::from_secs(5)
        ),
        "a process the script started outlived the dropped code_execution call"
    );
}

fn test_tool(name: &str, input_schema: serde_json::Value) -> Tool {
    Tool {
        input_schema,
        ..tool(name)
    }
}

#[test]
fn strict_mode_applies_per_tool_in_mixed_catalog() {
    let mut tools = vec![
        test_tool(
            "lookup",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": []
            }),
        ),
        test_tool(
            "either",
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": "string"},
                    "b": {"type": "string"}
                },
                "anyOf": [
                    {"required": ["a"]},
                    {"required": ["b"]}
                ]
            }),
        ),
        test_tool(
            "nested",
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "oneOf": [
                            {"type": "string"},
                            {"type": "integer"}
                        ]
                    }
                }
            }),
        ),
    ];

    assert!(!prepare_tools_for_strict_mode(&mut tools));
    assert_eq!(tools[0].strict, Some(true));
    assert_eq!(tools[0].input_schema["required"], json!([]));
    assert!(tools[0].input_schema.get("additionalProperties").is_none());
    assert_eq!(tools[1].strict, None);
    assert!(tools[1].input_schema.get("anyOf").is_some());
    assert_eq!(tools[2].strict, None);
    assert!(
        tools[2].input_schema["properties"]["value"]
            .get("oneOf")
            .is_some()
    );
}

#[test]
fn strict_mode_rejects_nested_unsupported_composition() {
    let mut tools = vec![Tool {
        tool_type: None,
        name: "nested".to_string(),
        description: "Nested oneOf".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "value": {
                    "oneOf": [
                        {"type": "string"},
                        {"type": "integer"}
                    ]
                }
            }
        }),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    }];

    assert!(!prepare_tools_for_strict_mode(&mut tools));
    assert_eq!(tools[0].strict, None);
}

#[test]
fn strict_mode_leaves_dependent_schema_tools_non_strict() {
    let schema = json!({
        "type": "object",
        "properties": {
            "action": {"type": "string"},
            "message": {"type": "string"}
        },
        "dependentSchemas": {
            "action": {
                "properties": {"message": {}},
                "required": ["message"]
            }
        }
    });
    let mut tools = vec![test_tool("agent", schema.clone())];

    assert!(!prepare_tools_for_strict_mode(&mut tools));
    assert_eq!(tools[0].strict, None);
    assert_eq!(tools[0].input_schema, schema);
}

#[test]
fn strict_mode_marks_compatible_tools_strict() {
    let mut tools = vec![Tool {
        tool_type: None,
        name: "lookup".to_string(),
        description: "Lookup".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": []
        }),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    }];

    assert!(prepare_tools_for_strict_mode(&mut tools));
    assert_eq!(tools[0].strict, Some(true));
    assert_eq!(tools[0].input_schema["required"], json!([]));
    assert!(tools[0].input_schema.get("additionalProperties").is_none());
}

#[test]
fn deferred_toolbox_runtime_message_is_minimal_and_roundtrips_with_legacy_meta() {
    let message = crate::runtime_handoff::deferred_toolbox_runtime_message(&["Git", "Web"]);
    assert!(codewhale_models::turn_classify::is_runtime_owned_user_message(&message));
    assert_eq!(message.content.len(), 2);
    let meta = match &message.content[1] {
        codewhale_models::ContentBlock::Text { text, .. } => text.as_str(),
        other => panic!("expected text metadata block, got {other:?}"),
    };
    assert_eq!(
        meta,
        "<turn_meta>\nInput provenance: runtime (non-authoritative)\n</turn_meta>"
    );
    assert!(!meta.contains("Current local date:"));
    assert!(!meta.contains("Repo Working Set"));
    assert_eq!(
        crate::runtime_handoff::deferred_toolbox_tools(&message),
        Some(vec!["Git".to_string(), "Web".to_string()])
    );

    let legacy = codewhale_models::Message {
        role: codewhale_models::Role::User,
        content: vec![
            codewhale_models::ContentBlock::Text {
                text: r#"{"type":"current_deferred_toolbox","active_deferred_tools":["Run"]}"#
                    .to_string(),
                cache_control: None,
            },
            codewhale_models::ContentBlock::Text {
                text: "<turn_meta>\nCurrent local date: 2026-11-18\nInput provenance: runtime (non-authoritative)\n## Repo Working Set\n</turn_meta>".to_string(),
                cache_control: None,
            },
        ],
    };
    assert_eq!(
        crate::runtime_handoff::deferred_toolbox_tools(&legacy),
        Some(vec!["Run".to_string()])
    );
}

#[test]
fn eager_wire_tools_never_enter_tool_search_or_activation_cache() {
    let mut catalog = vec![
        tool("mcp_demo_authenticate"),
        tool("mcp_demo_wait_for"),
        tool("mcp_demo_eager_active"),
        tool("mcp_demo_deferred_query"),
    ];
    let always_load = HashSet::from([
        "mcp_demo_authenticate".to_string(),
        "mcp_demo_wait*".to_string(),
    ]);
    apply_mcp_tool_deferral(&mut catalog, AppMode::Agent, &always_load);
    assert_eq!(catalog[0].defer_loading, Some(false));
    assert_eq!(catalog[1].defer_loading, Some(false));
    assert_eq!(catalog[2].defer_loading, Some(true));
    assert_eq!(catalog[3].defer_loading, Some(true));

    let mut active = initial_active_tools(&catalog);
    active.insert("mcp_demo_eager_active".to_string());
    let mut cache = ToolActivationCache::default();

    for query in ["authenticate", "wait", "eager_active", "mcp_demo"] {
        let result = execute_tool_search_with_cache(
            codewhale_tools::names::TOOL_SEARCH_NAME,
            &json!({"query": query, "match": "bm25"}),
            &catalog,
            &mut active,
            &mut cache,
        )
        .expect("search succeeds");
        let refs = result.metadata.expect("metadata")["tool_references"].clone();
        if query == "mcp_demo" {
            assert_eq!(refs, json!(["mcp_demo_deferred_query"]));
        } else {
            assert_eq!(refs, json!([]), "query={query}");
        }
    }
    assert_eq!(
        cache.names().collect::<Vec<_>>(),
        vec!["mcp_demo_deferred_query"]
    );
}

#[test]
fn readmission_search_input_matches_exact_name_without_newline_leak() {
    let input = super::readmission_search_input("web.run");
    let query = input["query"].as_str().expect("query string");
    assert_eq!(query, "^web\\.run$");
    assert!(!query.contains('\n'));
    assert!(!query.contains("\\n"));

    let mut target = tool("web.run");
    target.defer_loading = Some(true);
    let mut prefix_collision = tool("web.run_extended");
    prefix_collision.defer_loading = Some(true);
    let mut desc_collision = tool("other_tool");
    desc_collision.description = "wraps web.run internally".to_string();
    desc_collision.defer_loading = Some(true);

    let catalog = vec![prefix_collision, desc_collision, target];
    let mut active = initial_active_tools(&catalog);
    let mut cache = ToolActivationCache::default();
    let result = execute_tool_search_with_cache(
        codewhale_tools::names::TOOL_SEARCH_NAME,
        &input,
        &catalog,
        &mut active,
        &mut cache,
    )
    .expect("readmission search");
    assert_eq!(
        result.metadata.expect("metadata")["tool_references"],
        json!(["web.run"])
    );
}

#[test]
fn terminal_underscore_tools_are_visible_and_slash_aliases_stay_hidden() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = crate::tools::registry::ToolRegistryBuilder::new()
        .with_terminal_tools()
        .build(crate::tools::spec::ToolContext::new(tmp.path()));
    let visible: BTreeSet<String> = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    let canonical = [
        "terminal_run",
        "terminal_send",
        "terminal_wait",
        "terminal_cancel",
        "terminal_reset",
    ];
    let legacy = [
        "terminal/run",
        "terminal/send",
        "terminal/wait",
        "terminal/cancel",
        "terminal/reset",
    ];
    for (new_name, old_name) in canonical.iter().zip(legacy.iter()) {
        assert!(
            visible.contains(*new_name),
            "{new_name} must be model-visible"
        );
        assert!(!visible.contains(*old_name), "{old_name} must be hidden");
        assert!(
            registry.contains(old_name),
            "{old_name} must remain replayable"
        );
        assert!(tool_matches_any_rule(&[(*old_name).to_string()], new_name));
        assert!(tool_matches_any_rule(&[(*new_name).to_string()], old_name));
        assert!(tool_matches_any_rule(&["terminal/*".to_string()], new_name));
        assert!(tool_matches_any_rule(&["terminal_*".to_string()], old_name));
    }
}

#[test]
fn deferred_schema_hydration_returns_error_with_field_details() {
    let mut deferred = test_tool(
        "update_goal",
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["complete", "blocked", "yield"],
                    "description": "Terminal or yield transition for the active goal."
                },
                "verification": {
                    "type": "object",
                    "description": "Structured proof for completion.",
                    "required": ["status"],
                    "properties": {
                        "status": {"type": "string", "enum": ["passed", "not_applicable"]},
                        "check": {"type": "string"}
                    }
                },
                "tags": {
                    "type": "array",
                    "description": "Optional labels.",
                    "items": {"type": "string"}
                }
            },
            "required": ["status"]
        }),
    );
    deferred.defer_loading = Some(true);

    let result = super::deferred_tool_schema_hydration_result(&deferred, &json!({}));
    assert!(!result.success);
    assert!(result.content.contains("status: string (complete | blocked | yield) required — Terminal or yield transition for the active goal."), "{}", result.content);
    assert!(result.content.contains("verification: object {check: string, status: string (passed | not_applicable) required} — Structured proof for completion."), "{}", result.content);
    assert!(
        result
            .content
            .contains("tags: array<string> — Optional labels."),
        "{}",
        result.content
    );
}

#[test]
fn goal_followup_tools_defer_by_default_and_auto_activate_together() {
    let mut catalog = vec![tool("create_goal"), tool("get_goal"), tool("update_goal")];
    apply_native_tool_deferral(&mut catalog, &HashSet::new());
    assert_eq!(catalog[0].defer_loading, Some(false));
    assert_eq!(catalog[1].defer_loading, Some(true));
    assert_eq!(catalog[2].defer_loading, Some(true));

    let mut active = initial_active_tools(&catalog);
    assert!(active.contains("create_goal"));
    assert!(!active.contains("get_goal"));
    assert!(!active.contains("update_goal"));

    let mut cache = ToolActivationCache::default();
    let changed =
        super::activate_goal_followup_tools(&catalog, &mut active, &mut cache, &mut |names| {
            ToolActivationCache::test_measure(&catalog, names)
        })
        .expect("goal followup activation");
    assert!(changed);
    assert!(active.contains("get_goal"));
    assert!(active.contains("update_goal"));

    let changed_again =
        super::activate_goal_followup_tools(&catalog, &mut active, &mut cache, &mut |_| {
            panic!("already-active goal tools must not re-measure")
        })
        .expect("idempotent activation");
    assert!(!changed_again);
}

#[test]
fn activate_result_dependencies_skips_when_already_active_or_eager() {
    let mut retrieval = tool("retrieve_tool_result");
    retrieval.defer_loading = Some(true);
    let catalog = vec![retrieval];
    let mut active = initial_active_tools(&catalog);
    active.remove("retrieve_tool_result");
    let mut cache = ToolActivationCache::default();
    let spilled = crate::tools::spec::ToolResult::success("preview")
        .with_metadata(json!({"evidence_available": true}));

    let first = super::activate_result_dependencies(
        &catalog,
        &mut active,
        &mut cache,
        &spilled,
        &mut |names| ToolActivationCache::test_measure(&catalog, names),
    )
    .expect("first spill activates retrieval");
    assert!(first);
    assert!(active.contains("retrieve_tool_result"));

    let second = super::activate_result_dependencies(
        &catalog,
        &mut active,
        &mut cache,
        &spilled,
        &mut |_| panic!("second spill must not re-measure or re-activate"),
    )
    .expect("second spill is a no-op");
    assert!(!second);
}

#[test]
fn task_shell_wait_defers_by_default_and_auto_activates_on_background_shell_job() {
    let mut catalog = vec![tool("bash"), tool("task_shell_wait")];
    apply_native_tool_deferral(&mut catalog, &HashSet::new());
    assert_eq!(catalog[0].defer_loading, Some(false));
    assert_eq!(catalog[1].defer_loading, Some(true));

    let mut active = initial_active_tools(&catalog);
    assert!(active.contains("bash"));
    assert!(!active.contains("task_shell_wait"));

    let mut cache = ToolActivationCache::default();
    let background_bash = crate::tools::spec::ToolResult::success("Background task started")
        .with_metadata(json!({"status": "Running", "backgrounded": true, "task_id": "shell_1"}));
    let activated = super::activate_result_dependencies(
        &catalog,
        &mut active,
        &mut cache,
        &background_bash,
        &mut |names| ToolActivationCache::test_measure(&catalog, names),
    )
    .expect("background bash activates task_shell_wait");
    assert!(activated);
    assert!(active.contains("task_shell_wait"));

    let repeated =
        super::activate_background_shell_tools(&catalog, &mut active, &mut cache, &mut |_| {
            panic!("already-active task_shell_wait must not re-measure")
        })
        .expect("idempotent activation");
    assert!(!repeated);

    let mut active_exec = initial_active_tools(&catalog);
    let mut cache_exec = ToolActivationCache::default();
    let exec_running = crate::tools::spec::ToolResult::success("Background task started")
        .with_metadata(json!({"status": "Running", "task_id": "shell_2"}));
    let activated_exec = super::activate_result_dependencies(
        &catalog,
        &mut active_exec,
        &mut cache_exec,
        &exec_running,
        &mut |names| ToolActivationCache::test_measure(&catalog, names),
    )
    .expect("exec_shell running task activates task_shell_wait");
    assert!(activated_exec);
    assert!(active_exec.contains("task_shell_wait"));
}

#[test]
fn bm25_tool_search_expands_curated_synonyms_without_false_substring_hits() {
    let mut catalog = [
        "read_media",
        "Git",
        "Run",
        "terminal_run",
        "task_shell_wait",
        "agent",
        "workflow",
        "remember",
        "Web",
        "lsp",
        "todo_write",
        "tasks",
        "thread_status",
        "prune_cache",
    ]
    .into_iter()
    .map(|name| {
        let mut entry = tool(name);
        entry.description = format!("Tool {name}");
        entry.defer_loading = Some(true);
        entry
    })
    .collect::<Vec<_>>();
    catalog.push({
        let mut entry = tool("mcp_git_status");
        entry.description = "MCP repo status".to_string();
        entry.defer_loading = Some(true);
        entry
    });

    let active = HashSet::new();
    let cases = [
        ("screenshot", "read_media"),
        ("pdf", "read_media"),
        ("image", "read_media"),
        ("branch", "Git"),
        ("diff", "Git"),
        ("commit", "Git"),
        ("lint", "Run"),
        ("build", "Run"),
        ("check", "Run"),
        ("pty", "terminal_run"),
        ("bg", "task_shell_wait"),
        ("background", "task_shell_wait"),
        ("subagent", "agent"),
        ("delegate", "agent"),
        ("worker", "agent"),
        ("spawn", "agent"),
        ("memory", "remember"),
        ("note", "remember"),
        ("http", "Web"),
        ("url", "Web"),
        ("definition", "lsp"),
        ("references", "lsp"),
        ("diagnostics", "lsp"),
        ("checklist", "todo_write"),
        ("plan", "todo_write"),
    ];
    for (query, expected) in cases {
        let matches = super::discover_tools_with_bm25_like(&catalog, query, 8, &active, None);
        assert!(
            matches.iter().any(|name| name == expected),
            "query {query:?} should find {expected:?}, got {matches:?}"
        );
    }

    let pdf_matches = super::discover_tools_with_bm25_like(&catalog, "pdf", 8, &active, None);
    assert!(
        !pdf_matches.iter().any(|name| name == "thread_status"),
        "synonym expansion must not match substrings inside words: {pdf_matches:?}"
    );
    let lint_matches = super::discover_tools_with_bm25_like(&catalog, "lint", 8, &active, None);
    assert!(
        !lint_matches.iter().any(|name| name == "prune_cache"),
        "synonym expansion must not match substrings inside words: {lint_matches:?}"
    );
    let exact_git = super::discover_tools_with_bm25_like(&catalog, "git", 8, &active, None);
    assert_eq!(exact_git.first().map(String::as_str), Some("Git"));
}

#[test]
fn deferred_named_batch_keeps_execute_tools_schema_minimal() {
    let step_schema = json!({"type": "object", "properties": {
        "target": {"type": "string", "description": "x".repeat(400)},
        "find": {"type": "object", "properties": {"query": {"type": "string"}}}
    }});
    let mut batch = tool("mcp_cu_run_actions");
    batch.description = "Run several computer-use steps.".repeat(20);
    batch.defer_loading = Some(true);
    batch.input_schema = json!({
        "type": "object",
        "required": ["steps"],
        "properties": {"steps": {
            "type": "array", "minItems": 1, "maxItems": 8,
            "items": {"type": "object", "required": ["tool"], "properties": {
                "tool": {"type": "string", "enum": ["click", "drag"]},
                "arguments": {"type": "object", "oneOf": [step_schema.clone(), step_schema]}
            }}
        }}
    });
    let mut catalog = vec![crate::tools::codemode::execute_tools_tool_definition(), batch];
    let members = vec!["click".to_string(), "drag".to_string()];
    assert!(super::configure_composition_entry(
        &mut catalog,
        Some("mcp_cu_run_actions"),
        &members
    ));
    let offered = &catalog[0].input_schema["properties"]["batch"];
    let rendered = offered.to_string();
    assert!(!rendered.contains("xxxxxxxx"), "{rendered}");
    assert!(!rendered.contains("computer-use steps"), "{rendered}");
    assert!(rendered.contains("tool_search"), "{rendered}");
    assert!(super::offered_batch_allows(
        &catalog,
        "mcp_cu_run_actions",
        &json!({"steps": [{"tool": "click", "arguments": {}}]})
    ));
}

#[test]
fn tool_search_ranks_first_party_names_above_mcp_description_hits() {
    let mut run = tool("Run");
    run.defer_loading = Some(true);
    let mut git = tool("Git");
    git.defer_loading = Some(true);
    let mut computer = tool("mcp_cu_run_actions");
    computer.description = "Run tests, run actions, run anything on the desktop.".to_string();
    computer.defer_loading = Some(true);
    let mut notebook = tool("mcp_db_run_notebook");
    notebook.description = "Run a notebook".to_string();
    notebook.defer_loading = Some(true);
    let catalog = vec![computer, notebook, run, git];
    let found = super::discover_tools_with_bm25_like(
        &catalog,
        "run tests cargo",
        super::TOOL_SEARCH_DEFAULT_MAX_RESULTS,
        &HashSet::new(),
        None,
    );
    assert_eq!(found.first().map(String::as_str), Some("Run"), "{found:?}");
    assert!(!found.iter().any(|name| name == "Git"), "{found:?}");
    assert!(found.len() <= 4, "{found:?}");
}
