//! Mod instructions use complete recorded history snapshots after the pinned prefix.
use super::*;

#[tokio::test]
async fn extension_prompt_changes_and_retirement_are_recorded_without_rewriting_the_prefix() {
    let _home = crate::test_support::SealedHome::new();
    let workspace = tempfile::tempdir().expect("workspace");
    let (mut engine, _) = Engine::new(
        EngineConfig {
            workspace: workspace.path().to_path_buf(),
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &Config::default(),
    );
    let context = engine.installed_next_turn_prompt_context();
    assert!(
        engine
            .refresh_pinned_header_for_turn(&context)
            .unwrap()
            .is_none()
    );
    let pinned =
        codewhale_core::prefix_cache::system_prompt_text(engine.session.system_prompt.as_ref());
    let sections: Vec<_> = (0..3)
        .map(|i| crate::extension_host::prompt::PromptSection {
            plugin_id: "user/fixture/repo-style".to_string(),
            plugin_name: "repo-style".to_string(),
            generation: 1,
            content_hash: "fixture-reviewed-hash".to_string(),
            id: format!("repo-style-{i}"),
            text: format!("{}\nLast instruction {i}.", "x".repeat(4000)),
            scope: None,
            interpolate: false,
        })
        .collect();
    let block = crate::extension_host::prompt::render_prompt_sections(&sections)
        .expect("render")
        .expect("sections");
    assert!(block.len() > codewhale_core::prefix_cache::CONTEXT_UPDATE_MAX_BYTES);
    engine
        .record_extension_prompt_contributions(Some(&block))
        .await;
    let initial_count = engine.session.messages.len();
    engine
        .record_extension_prompt_contributions(Some(&block))
        .await;
    assert_eq!(
        engine.session.messages.len(),
        initial_count,
        "unchanged instructions do not repeat"
    );
    let recorded = engine.session.messages.last().expect("recorded snapshot");
    let display = crate::runtime_handoff::extension_prompt_contributions_display(recorded)
        .expect("runtime event");
    assert!(
        display.contains(&block),
        "the entire admitted snapshot reaches the model"
    );
    assert!(codewhale_models::turn_classify::is_runtime_owned_user_message(recorded));
    assert!(
        engine
            .refresh_pinned_header_for_turn(&context)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        codewhale_core::prefix_cache::system_prompt_text(engine.session.system_prompt.as_ref()),
        pinned
    );

    engine.record_extension_prompt_contributions(None).await;
    let retired_count = engine.session.messages.len();
    engine.record_extension_prompt_contributions(None).await;
    assert_eq!(engine.session.messages.len(), retired_count);
    assert!(
        crate::runtime_handoff::extension_prompt_contributions_display(
            engine.session.messages.last().expect("withdrawal")
        )
        .is_some_and(|text| text.contains("withdrawn"))
    );
    assert_eq!(
        codewhale_core::prefix_cache::system_prompt_text(engine.session.system_prompt.as_ref()),
        pinned
    );

    // Restore/compaction cannot make a process-local baseline swallow the text.
    engine.extension_prompt_block = Some(block.clone());
    engine.session.messages.clear();
    engine.record_current_extension_prompt_contributions().await;
    assert_eq!(engine.session.messages.len(), 1);
    assert!(
        crate::runtime_handoff::extension_prompt_contributions_display(&engine.session.messages[0])
            .is_some_and(|text| text.contains(&block))
    );
}
