//! Real programmable host listeners on the actual Engine admission path.
use super::*;
use crate::llm_client::mock::{MockLlmClient, canned};

#[tokio::test]
async fn typescript_mod_rewrites_denies_and_regates_native_tools_on_the_real_engine() {
    let Some(node) =
        crate::extension_host::test_support::node_for_tests("typescript_mod_real_engine")
    else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(true);
    let fixture = crate::extension_host::test_support::FixturePlugins::new(&["hook-policy"]).await;
    for (name, text) in [
        ("before.txt", "original"),
        ("after.txt", "rewritten content"),
        ("blocked.txt", "must not be returned"),
        ("malformed.txt", "must not be returned"),
        ("throw.txt", "must not be returned"),
        ("rewrite-action.txt", "original read"),
    ] {
        fs::write(fixture.workspace().join(name), text).unwrap();
    }
    let manager = fixture.manager(node);
    let warm = manager.attach(fixture.registry());
    warm.sync().await.expect("real host activation");
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("rewrite", "read", r#"{"path":"before.txt"}"#),
        canned::tool_call_turn("deny", "read", r#"{"path":"blocked.txt"}"#),
        canned::tool_call_turn("malformed", "read", r#"{"path":"malformed.txt"}"#),
        canned::tool_call_turn("throw", "read", r#"{"path":"throw.txt"}"#),
        canned::tool_call_turn(
            "action",
            "File",
            r#"{"action":"read","path":"rewrite-action.txt"}"#,
        ),
        canned::simple_text_turn("Finished checking the mod."),
        canned::tool_call_turn("after-disable", "read", r#"{"path":"before.txt"}"#),
        canned::simple_text_turn("The mod is disabled."),
    ]));
    let config = Config::default();
    let mut engine_config = deterministic_engine_config(fixture.workspace());
    engine_config.features.enable(Feature::ExtensionHost);
    engine_config.plugin_registry = Some(fixture.registry());
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, mock.clone());
    manager.reconcile().await.unwrap();
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Check the programmable mod",
            AppMode::Agent,
            &config,
        ))
        .await
        .unwrap();
    let mut outcomes = HashMap::new();
    let mut approvals = 0;
    let mut rx = handle.rx_event.write().await;
    loop {
        match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::ApprovalRequired { id, input, .. } => {
                approvals += 1;
                assert_eq!(
                    input,
                    serde_json::json!({
                        "action": "write", "path": "rewritten.txt", "content": "requires fresh approval"
                    }),
                    "the approval must be for the re-prepared revised write"
                );
                handle.deny_tool_call(id).await.unwrap();
            }
            Event::ToolCallComplete {
                model_call: Some(call),
                result,
                ..
            } => {
                outcomes.insert(call.provider_id, result);
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    assert!(
        outcomes
            .remove("rewrite")
            .unwrap()
            .unwrap()
            .content
            .contains("rewritten content")
    );
    assert!(
        outcomes
            .remove("deny")
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("blocked by fixture")
    );
    for id in ["malformed", "throw"] {
        assert!(
            outcomes
                .remove(id)
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("returned no verdict")
        );
    }
    assert!(outcomes.remove("action").unwrap().is_err());
    assert_eq!(approvals, 1, "only the revised write needs an approval");
    assert!(!fixture.workspace().join("rewritten.txt").exists());
    let first_request = mock.captured_requests().into_iter().next().unwrap();
    let first_prompt = first_request
        .messages
        .iter()
        .find_map(crate::runtime_handoff::extension_prompt_contributions_display)
        .expect("the authored prompt contribution is delivered as a complete runtime snapshot");
    assert!(first_prompt.contains("Use the repository style guide when preparing release notes."));
    assert!(first_prompt.contains("repo-style"));
    drop(rx);
    let disabled = fixture.disable("hook-policy");
    crate::extension_host::plugins_changed(disabled);
    manager.reconcile().await.unwrap();
    handle
        .send(external_user_message_op(
            "Read again after disabling",
            AppMode::Agent,
            &config,
        ))
        .await
        .unwrap();
    let mut rx = handle.rx_event.write().await;
    let mut original = false;
    loop {
        match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::ToolCallComplete {
                model_call: Some(call),
                result: Ok(result),
                ..
            } if call.provider_id == "after-disable" => {
                original = result.content.contains("original")
                    && !result.content.contains("rewritten content");
            }
            Event::ApprovalRequired { .. } => panic!("disabled hook cannot request approval"),
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    assert!(original, "disabled listener must not rewrite the next turn");
    let after_disable = mock.last_request().unwrap();
    let latest_context = after_disable
        .messages
        .iter()
        .rev()
        .find_map(crate::runtime_handoff::extension_prompt_contributions_display)
        .expect("disabling the authored section records a withdrawn runtime snapshot");
    assert!(latest_context.contains("All earlier extension prompt contributions are withdrawn"));
    assert!(
        !latest_context.contains("Use the repository style guide when preparing release notes.")
    );
    assert!(!latest_context.contains("repo-style"));
    drop(rx);
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();
    manager.shutdown().await;
}
