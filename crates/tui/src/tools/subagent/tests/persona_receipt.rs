//! Actual child loop, selected Native templates and an approved local route replacement.
use super::*;
use crate::extension_host::TestManagerGuard;
use crate::extension_host::composition_scope::NativePresetRef;
use crate::extension_host::tests::{FixturePlugins, node_for_tests};
use crate::plugins::activation::{PluginActivationCapability, TestPolicyGuard};
use codewhale_config::EntryRef;

#[cfg(test)]
fn preset(fixture: &FixturePlugins, tag: &str) -> NativePresetRef {
    let plugins = fixture.registry();
    let (sources, problems) = crate::plugins::runtime::active_component_sources(
        &plugins,
        PluginActivationCapability::Native,
    );
    assert!(problems.is_empty(), "{problems:?}");
    let source = sources
        .into_iter()
        .find(|source| source.path.ends_with(format!("{tag}.mjs")))
        .unwrap();
    NativePresetRef {
        plugin_id: source.authority.plugin_id.to_string(),
        content_hash: source.authority.content_hash,
        entry: EntryRef {
            path: source.path.to_string_lossy().into(),
            sha256: codewhale_runtime::hashing::sha256_hex(std::fs::read(source.path).unwrap()),
        },
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_child_persona_selection_and_approved_route_replace_use_current_model_and_cwd() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("actual_child_persona_selection_and_route_replace") else {
        return;
    };
    let fixture = FixturePlugins::new(&["child-persona"]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let original = fixture.registry();
    let a = preset(&fixture, "a");
    let b = preset(&fixture, "b");
    let parent = manager.attach(Arc::new(original.with_native_preset(a.clone()).unwrap()));
    parent.reconcile().await.unwrap();
    assert_eq!(
        parent.prompt_sections().await.unwrap()[0].text,
        "Persona A model={{model}} cwd={{cwd}}"
    );

    // Reuse the existing actual HTTP chat recorder and approved-replacement
    // config authority. No prompt store, fake Engine or alternate renderer.
    let (backup, backup_calls, bodies, _fixture_config) =
        delayed_chat_client(Duration::ZERO, "persona done").await;
    let (pin_url, pin_calls) = refusing_chat_server().await;
    let config_path = fixture.workspace().join("persona-route.toml");
    write_replacement_config(
        &config_path,
        &pin_url,
        backup.base_url(),
        r#"
[subagents.roles.reviewer]
model = "PinRoute/fixture-pin-model"
replacements = ["BackupRoute/fixture-backup-model"]
"#,
    )
    .await;
    let mut config = crate::plugins::providers::load_config(Some(config_path), None).unwrap();
    config.set_feature("extension_host", true).unwrap();
    let mut runtime = stub_runtime().with_api_config(config.clone());
    runtime.client = CodewhaleClient::new(&config).unwrap();
    runtime.context = ToolContext::new(fixture.workspace())
        .with_features(config.features())
        .with_plugin_registry(parent.plugin_view());
    runtime.manager = new_shared_subagent_manager(fixture.workspace().to_path_buf(), 2);
    let request =
        parse_spawn_request(&json!({"type":"reviewer","prompt":"Report your selected persona."}))
            .unwrap();
    let (_, source, _) = bind_spawn_model_route(&mut runtime, &request, None, true, true)
        .await
        .unwrap();
    assert_eq!(source, SpawnRouteSource::RolePin);
    assert_eq!(runtime.model, "fixture-pin-model");
    assert_eq!(runtime.route_replacements.len(), 1);

    // The parent keeps A alive while B executes under the same admitted owner.
    // Then a second child selects A; no sibling or previous-child template leaks.
    for (tag, selected) in [("B", b), ("A", a)] {
        let mut assignment = request.assignment.clone();
        assignment.native_preset = Some(selected);
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let route = mint_child_route_receipt(
                &RequestedChildRoute {
                    requested_type: request.agent_type.as_str().to_owned(),
                    requested_profile: request.profile.clone(),
                    requested_reasoning: subagent_thinking_label(request.thinking).to_owned(),
                },
                &request,
                None,
                &runtime,
                runtime.model.clone(),
                source.as_str(),
                None,
            )
            .unwrap();
            let started = runtime
                .manager
                .write()
                .await
                .spawn_background_with_assignment_options(
                    Arc::clone(&runtime.manager),
                    runtime.clone(),
                    request.agent_type.clone(),
                    "Report your selected persona.".into(),
                    assignment,
                    Some(Vec::new()),
                    SubAgentSpawnOptions {
                        name: Some(format!("persona_child_{tag}")),
                        model: Some(runtime.model.clone()),
                        model_route: Some(ModelRoute::Fixed(runtime.model.clone())),
                        child_route: Some(route),
                        max_steps: Some(3),
                        ..Default::default()
                    },
                    None,
                )
                .expect("actual persona child admission");
            assert_eq!(started.workspace.as_deref(), Some(fixture.workspace()));
            assert_eq!(
                started.child_route.as_ref().unwrap().provider_id,
                "PinRoute"
            );
            loop {
                let result = runtime
                    .manager
                    .read()
                    .await
                    .get_result(&started.agent_id)
                    .expect("admitted persona child remains registered");
                if result.status != SubAgentStatus::Running {
                    break result;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("local child settles");
        assert_eq!(
            result.status,
            SubAgentStatus::Completed,
            "{:?}",
            result.status
        );
        assert_eq!(result.result.as_deref(), Some("persona done"));
        let expected = format!(
            "Persona {tag} model=fixture-backup-model cwd={}",
            fixture.workspace().display()
        );
        let body = bodies.lock().unwrap().last().unwrap().clone();
        assert_eq!(body["model"], "fixture-backup-model");
        // `body` is searched as serialized JSON, so search for `expected` in
        // its JSON-escaped form: a Windows cwd's `\` serializes as `\\`.
        let expected_json = serde_json::Value::String(expected.clone()).to_string();
        let expected_json = &expected_json[1..expected_json.len() - 1];
        assert!(
            body.to_string().contains(expected_json),
            "the actual replacement request must include the new captured model: {body}"
        );
        let other = if tag == "B" { "A" } else { "B" };
        assert!(
            !body
                .to_string()
                .contains(&format!("Persona {other} model=")),
            "an owner union must not cross selected entries"
        );
        let checkpoint = result
            .checkpoint
            .as_ref()
            .expect("actual child transcript checkpoint");
        assert!(
            !checkpoint.messages.iter().any(|message| {
                crate::runtime_handoff::is_internal_runtime_handoff(message)
                    && message.content.iter().any(|block| {
                        matches!(block, ContentBlock::Text { text, .. }
                            if text.contains("<codewhale:subagent.done>"))
                    })
            }),
            "a persona child's checkpoint must not consume its root parent's sibling completions"
        );
        let latest = checkpoint
            .messages
            .iter()
            .rev()
            .find_map(crate::runtime_handoff::extension_prompt_contributions_display)
            .unwrap();
        assert!(
            latest.contains(&expected),
            "latest full snapshot follows the installed replacement: {latest}"
        );
        assert!(!latest.contains("model=fixture-pin-model"));
    }
    assert_eq!(
        pin_calls.load(Ordering::SeqCst),
        2,
        "each explicit child tries the exact pin once"
    );
    assert_eq!(
        backup_calls.load(Ordering::SeqCst),
        2,
        "each approved replacement receives one request"
    );
    assert_eq!(
        parent.prompt_sections().await.unwrap()[0].text,
        "Persona A model={{model}} cwd={{cwd}}",
        "Core-captured turn facts never mutate the registered template"
    );
    manager.shutdown().await;
}
