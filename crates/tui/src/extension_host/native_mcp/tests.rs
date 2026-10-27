//! Real installed Native composition, committed host bundle and local MCP peer.
//! No provider traffic; the Node-only suite uses a fake Core separately.
use super::*;
use crate::extension_host::composition_scope::NativePresetRef;
use crate::extension_host::protocol::RegisterSpecWire;
use crate::extension_host::registry::OwnerRegistry;
use crate::extension_host::tests::{FixturePlugins, fake_authority, node_for_tests};
use crate::extension_host::{ExtensionHostManager, HostAttachment, TestManagerGuard};
use crate::mcp::McpPool;
use crate::plugins::activation::TestPolicyGuard;
use codewhale_config::McpBackend;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

fn selected(fixture: &FixturePlugins, tag: &str) -> Arc<PluginRegistry> {
    let plugins = fixture.registry();
    let (sources, problems) = crate::plugins::runtime::active_component_sources(
        &plugins,
        PluginActivationCapability::Native,
    );
    assert!(problems.is_empty(), "{problems:?}");
    let source = sources
        .into_iter()
        .find(|source| source.path.ends_with(format!("native/{tag}.mjs")))
        .unwrap();
    let bytes = std::fs::read(&source.path).unwrap();
    Arc::new(
        plugins
            .with_native_preset(NativePresetRef {
                plugin_id: source.authority.plugin_id.to_string(),
                content_hash: source.authority.content_hash,
                entry: EntryRef {
                    path: source.path.to_string_lossy().into(),
                    sha256: codewhale_runtime::hashing::sha256_hex(&bytes),
                },
            })
            .unwrap(),
    )
}
/// Surface the existing owner diagnostic before catalog assertions discard it.
/// This does not admit or repair a definition; final consumer assertions remain.
fn assert_reviewed_owner_active(manager: &ExtensionHostManager, caller: &HostAttachment) {
    let plugins = caller.plugin_view();
    let entries = plugins.selected_native_entries();
    assert!(
        !entries.is_empty(),
        "fixture must keep its exact reviewed selection"
    );
    for selected in entries {
        let report = manager.owner_report(&selected.plugin_id);
        let state = report.as_ref().and_then(|report| report.state.as_ref());
        let diagnostics = report.as_ref().map(|report| &report.diagnostics);
        assert_eq!(
            state,
            Some(&super::super::registry::OwnerState::Active),
            "selected entry {:?}: existing owner diagnostics: {diagnostics:?}",
            selected.entry
        );
        let scope_state = {
            let registry = manager.shared.registry.lock().expect("registry lock");
            registry
                .owner(&selected.plugin_id)
                .map(|owner| registry.check_scope(&owner.owner, Some(&selected.entry), true))
        };
        assert_eq!(
            scope_state,
            Some(Ok(())),
            "selected entry {:?}: existing owner diagnostics: {diagnostics:?}",
            selected.entry
        );
    }
}
fn pool(fixture: &FixturePlugins, caller: &HostAttachment, backend: McpBackend) -> McpPool {
    McpPool::from_config_path_with_workspace_and_plugins(
        &fixture.root.join("mcp.json"),
        fixture.workspace(),
        caller.plugin_view(),
    )
    .unwrap()
    .with_backend(backend)
}
async fn connect_echo(pool: &mut McpPool) -> String {
    let errors = pool.connect_all().await;
    assert!(errors.is_empty(), "{errors:?}");
    let tools = pool.all_tools();
    assert_eq!(tools.len(), 1, "{tools:?}");
    tools[0].0.clone()
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_native_graphs_use_one_core_catalog_and_selected_child_pool_on_both_backends() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("mixed_native_graphs_use_one_core_catalog") else {
        return;
    };
    let fixture = FixturePlugins::new(&["raw-dsh-mcp"]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let a = manager.attach(selected(&fixture, "a"));
    let b = manager.attach(selected(&fixture, "b"));
    a.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &a);
    let a_view = a.plugin_view();
    let b_view = b.plugin_view();
    let a_defs = for_plugins(&a_view).unwrap();
    let b_defs = for_plugins(&b_view).unwrap();
    assert_eq!(a_defs.len(), 1);
    assert_eq!(b_defs.len(), 1);
    assert_ne!(
        a_defs[0].3.catalog_identity(),
        b_defs[0].3.catalog_identity()
    );
    // Same owner and public names, distinct exact selected entry handles.
    assert_eq!(a_defs[0].0, b_defs[0].0);
    // Path::ends_with is separator-agnostic. A string suffix of "native/a.mjs"
    // fails on Windows because scope paths keep backslashes (and may carry a
    // verbatim prefix from canonicalize).
    assert!(
        Path::new(&a_defs[0].3.registration.scope.path).ends_with(Path::new("native/a.mjs")),
        "{}",
        a_defs[0].3.registration.scope.path
    );
    assert!(
        Path::new(&b_defs[0].3.registration.scope.path).ends_with(Path::new("native/b.mjs")),
        "{}",
        b_defs[0].3.registration.scope.path
    );
    for backend in [McpBackend::Rust, McpBackend::Host] {
        let mut parent = pool(&fixture, &a, backend);
        let name = connect_echo(&mut parent).await;
        let result = parent.call_tool(&name, json!({"value":1})).await.unwrap();
        assert_eq!(result["content"][0]["text"], "a:{\"value\":1}");
        parent.validate_native_caller(Some(&a_view)).unwrap();
        assert!(parent.validate_native_caller(Some(&b_view)).is_err());
        let mut child = parent.fork_for_plugins(Arc::clone(&b_view)).unwrap();
        let child_name = connect_echo(&mut child).await;
        assert_eq!(child_name, name);
        child.validate_native_caller(Some(&b_view)).unwrap();
        let result = child
            .call_tool(&child_name, json!({"child":true}))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "b:{\"child\":true}");
    }
    let mut tools = crate::tools::registry::ToolRegistryBuilder::new().build(
        crate::tools::ToolContext::new(fixture.workspace()).with_plugin_registry(a.plugin_view()),
    );
    assert_eq!(a.install_tools(&mut tools), vec!["mixed_echo"]);
    let catalog = crate::skills::discover_for_workspace_and_dir_with_mode_and_plugins(
        fixture.workspace(),
        &fixture.workspace().join("empty-skills"),
        crate::skills::SkillDiscoveryMode::CodeWhaleOnly,
        Some(&a.plugin_view()),
    );
    assert!(catalog.get("raw-dsh-mcp:mixed-check").is_some());
    assert!(
        a.prompt_sections()
            .await
            .unwrap()
            .iter()
            .any(|section| section.text == "Selected a")
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn caller_revision_withdraws_pending_mcp_without_revoking_sibling_definition() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("caller_revision_withdraws_pending_mcp") else {
        return;
    };
    let fixture = FixturePlugins::new(&["raw-dsh-mcp"]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let a = manager.attach(selected(&fixture, "a"));
    let sibling = manager.attach(selected(&fixture, "a"));
    a.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &a);
    let old = for_plugins(&a.plugin_view()).unwrap().remove(0).3;
    let retained = for_plugins(&sibling.plugin_view()).unwrap().remove(0).3;
    let mut pending = pool(&fixture, &a, McpBackend::Host);
    let name = connect_echo(&mut pending).await;
    let operation = pending.call_tool(&name, json!({"hold":true}));
    tokio::pin!(operation);
    tokio::select! {
        biased;
        result = &mut operation => panic!("held peer must remain pending: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(30)) => {},
    }
    // Existing caller authority is the cancellation source, before reconcile.
    a.set_plugins(selected(&fixture, "b"));
    assert!(old.validate().is_err());
    retained.validate().unwrap();
    tokio::time::timeout(Duration::from_secs(1), old.withdrawn())
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), operation)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("not replayed"), "{error:#}");
    a.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &a);
    let mut live = pool(&fixture, &sibling, McpBackend::Host);
    let name = connect_echo(&mut live).await;
    assert_eq!(
        live.call_tool(&name, json!({})).await.unwrap()["content"][0]["text"],
        "a:{}"
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn retained_root_pool_rebinds_current_revision_and_refuses_foreign_attachment() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("retained_root_pool_rebinds_current_revision") else {
        return;
    };
    let fixture = FixturePlugins::new(&["raw-dsh-mcp"]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let caller = manager.attach(selected(&fixture, "a"));
    let foreign = manager.attach(selected(&fixture, "a"));
    caller.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &caller);
    let mut retained = pool(&fixture, &caller, McpBackend::Host);
    let name = connect_echo(&mut retained).await;
    assert!(retained.bind_caller_plugins(foreign.plugin_view()).is_err());
    assert_eq!(
        retained.call_tool(&name, json!({})).await.unwrap()["content"][0]["text"],
        "a:{}"
    );
    caller.set_plugins(selected(&fixture, "b"));
    caller.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &caller);
    retained.bind_caller_plugins(caller.plugin_view()).unwrap();
    assert_eq!(connect_echo(&mut retained).await, name);
    assert_eq!(
        retained.call_tool(&name, json!({})).await.unwrap()["content"][0]["text"],
        "b:{}"
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_mcp_receipt_refuses_persisted_disable_and_source_tamper() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("native_mcp_receipt_refuses_persisted_disable") else {
        return;
    };
    let fixture = FixturePlugins::new(&["raw-dsh-mcp"]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let caller = manager.attach(selected(&fixture, "a"));
    caller.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &caller);
    let mut active = pool(&fixture, &caller, McpBackend::Host);
    let name = connect_echo(&mut active).await;
    fixture.disable("raw-dsh-mcp");
    assert!(active.call_tool(&name, json!({})).await.is_err());
    assert!(for_plugins(&caller.plugin_view()).unwrap().is_empty());
    manager.shutdown().await;
    // A second independently reviewed fixture covers changed source bytes.
    let fixture = FixturePlugins::new(&["raw-dsh-mcp"]).await;
    let manager = fixture.manager(node_for_tests("native_mcp_source_tamper").unwrap());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let caller = manager.attach(selected(&fixture, "a"));
    caller.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &caller);
    let mut active = pool(&fixture, &caller, McpBackend::Host);
    let name = connect_echo(&mut active).await;
    let plugin = fixture.registry().get("raw-dsh-mcp").unwrap().clone();
    std::fs::write(
        plugin.canonical_root.join("source/peer.mjs"),
        "export const tampered=true",
    )
    .unwrap();
    assert!(active.call_tool(&name, json!({})).await.is_err());
    manager.shutdown().await;
}

fn params(owner: &OwnerRef, scope: EntryRef, name: String) -> RegisterParams {
    RegisterParams {
        owner: owner.clone(),
        scope: Some(scope),
        kind: RegisterKind::McpServer,
        spec: RegisterSpecWire {
            name,
            description: "{}".into(),
            input_schema: None,
            argument_hint: None,
        },
    }
}
#[test]
fn mcp_definition_bounds_are_owner_wide_and_exact_unregister_cancels_only_its_handle() {
    let mut registry = OwnerRegistry::default();
    let owner = registry
        .begin_owner(
            HostTier::Plugin,
            "fixture",
            "fixture",
            Some(fake_authority("fixture")),
            "build",
        )
        .unwrap();
    let a = EntryRef {
        path: "/reviewed/a.mjs".into(),
        sha256: "a".repeat(64),
    };
    let b = EntryRef {
        path: "/reviewed/b.mjs".into(),
        sha256: "b".repeat(64),
    };
    registry.begin_scope(&owner, a.clone()).unwrap();
    registry.begin_scope(&owner, b.clone()).unwrap();
    let config: McpServerConfig = serde_json::from_value(json!({"command":"node"})).unwrap();
    for i in 0..MAX_PER_OWNER {
        registry
            .register_mcp(
                &params(
                    &owner,
                    if i % 2 == 0 { a.clone() } else { b.clone() },
                    format!("s{i}"),
                ),
                config.clone(),
                1,
            )
            .unwrap();
    }
    assert!(
        registry
            .register_mcp(&params(&owner, b.clone(), "over".into()), config, 1)
            .is_err()
    );
    registry.mark_scope_active(&owner, &a);
    registry.mark_scope_active(&owner, &b);
    registry.mark_active(&owner);
    let definitions = registry.live_mcp();
    assert_eq!(definitions.len(), MAX_PER_OWNER);
    let first = definitions[0].clone();
    let second = definitions[1].clone();
    registry.unregister(&owner, first.handle);
    assert!(first.cancel.is_cancelled());
    assert!(!second.cancel.is_cancelled());
    registry.revoke_scope(&owner, &second.scope);
    assert!(second.cancel.is_cancelled());
}

#[path = "remote_tests.rs"]
mod remote_tests;
