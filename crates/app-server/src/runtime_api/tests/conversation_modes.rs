use super::*;
use codewhale_engine::conversation_modes::CreatorAvailabilityGuard;

#[tokio::test]
async fn modes_route_lists_the_table_and_threads_carry_and_refuse_modes() -> Result<()> {
    let mut guard = CreatorAvailabilityGuard::set(None);
    codewhale_engine::conversation_modes::clear_probe_cache_for_tests();
    let Some((addr, _threads, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}");

    let table: Value = client
        .get(format!("{base}/v1/modes"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(table["default"], "standard");
    assert_eq!(table["modes"][0]["id"], "standard");
    assert_eq!(table["modes"][0]["source"], "builtin");
    assert_eq!(table["modes"][0]["available"], true);
    assert!(table["modes"][0].get("reason").is_none());
    assert_eq!(table["modes"][1]["id"], "creator");
    assert_eq!(table["modes"][1]["available"], true);
    assert!(table["modes"][1].get("reason").is_none());

    let creator_created: Value = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let creator_id = creator_created["id"]
        .as_str()
        .expect("creator thread id")
        .to_string();
    assert_eq!(creator_created["agent_preset"], "creator");

    let unknown = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "minimal"}))
        .send()
        .await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = unknown.json().await?;
    assert_eq!(body["error"]["code"], "unknown_mode");

    let created: Value = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "standard"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = created["id"].as_str().expect("thread id").to_string();
    let second_standard: Value = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "standard"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let second_id = second_standard["id"]
        .as_str()
        .expect("second standard thread id")
        .to_string();
    let summary: Value = client
        .get(format!("{base}/v1/threads/summary"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = summary.as_array().expect("summary rows");
    let creator_row = rows
        .iter()
        .find(|row| row["id"] == creator_id.as_str())
        .expect("creator summary row");
    assert_eq!(creator_row["agent_preset"], "creator");
    let row = rows
        .iter()
        .find(|row| row["id"] == id.as_str())
        .expect("standard summary row");
    assert_eq!(row["agent_preset"], "standard");

    let patched_ok: Value = client
        .patch(format!("{base}/v1/threads/{id}"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(patched_ok["agent_preset"], "creator");

    drop(guard);
    guard = CreatorAvailabilityGuard::set(Some("missing runtime"));
    let unavailable = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?;
    assert_eq!(unavailable.status(), reqwest::StatusCode::FAILED_DEPENDENCY);
    let body: Value = unavailable.json().await?;
    assert_eq!(body["error"]["code"], "mode_unavailable");

    let patched = client
        .patch(format!("{base}/v1/threads/{second_id}"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?;
    assert_eq!(patched.status(), reqwest::StatusCode::FAILED_DEPENDENCY);
    drop(guard);

    if codewhale_engine::extension_host::test_support::node_for_tests(
        "modes_route_lists_the_table_and_threads_carry_and_refuse_modes",
    )
    .is_some()
    {
        let _unforced = CreatorAvailabilityGuard::unforced();
        codewhale_engine::conversation_modes::clear_probe_cache_for_tests();
        let live_table: Value = client
            .get(format!("{base}/v1/modes"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(live_table["modes"][1]["id"], "creator");
        assert_eq!(live_table["modes"][1]["available"], true);
        assert!(live_table["modes"][1].get("reason").is_none());

        let live_created: Value = client
            .post(format!("{base}/v1/threads"))
            .json(&json!({"agent_preset": "creator"}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(live_created["agent_preset"], "creator");
    }

    handle.abort();
    let _ = handle.await;
    Ok(())
}

#[tokio::test]
async fn catalog_lists_plugin_creator_matching_modes_and_disable_updates_modes() -> Result<()> {
    let guard = CreatorAvailabilityGuard::set(None);
    codewhale_engine::conversation_modes::clear_probe_cache_for_tests();
    let dir = tempfile::TempDir::new()?;
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "# initial\n")?;
    let Some((addr, _threads, handle)) =
        spawn_test_server_with_config_path(config_path.clone()).await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}");

    let catalog: Value = client
        .get(format!("{base}/v1/apps/plugins/catalog"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let modes: Value = client
        .get(format!("{base}/v1/modes"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let catalog_row = catalog["builtin"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == "plugin_creator"))
        .expect("plugin_creator row in builtin catalog");
    assert_eq!(catalog_row["enabled"], true);
    assert_eq!(catalog_row["available"], true);
    assert!(catalog_row["unavailable_reason"].is_null());
    assert_eq!(catalog_row["available"], modes["modes"][1]["available"]);
    assert_eq!(
        catalog_row["unavailable_reason"],
        modes["modes"][1]["reason"]
    );

    let disabled: Value = client
        .post(format!(
            "{base}/v1/apps/plugins/catalog/plugin_creator/disable"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(disabled["row"]["id"], "plugin_creator");
    assert_eq!(disabled["row"]["enabled"], false);
    assert_eq!(disabled["row"]["available"], false);
    assert_eq!(
        disabled["row"]["unavailable_reason"],
        codewhale_engine::conversation_modes::CREATOR_KEY_OFF_REASON
    );
    let saved = std::fs::read_to_string(&config_path)?;
    assert!(saved.contains("plugin_creator = false"), "{saved}");

    let modes_after: Value = client
        .get(format!("{base}/v1/modes"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(modes_after["modes"][1]["id"], "creator");
    assert_eq!(modes_after["modes"][1]["available"], false);
    assert_eq!(
        modes_after["modes"][1]["reason"],
        codewhale_engine::conversation_modes::CREATOR_KEY_OFF_REASON
    );

    let enabled: Value = client
        .post(format!(
            "{base}/v1/apps/plugins/catalog/plugin_creator/enable"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(enabled["row"]["enabled"], true);
    assert_eq!(enabled["row"]["available"], true);
    assert!(enabled["row"]["unavailable_reason"].is_null());

    let modes_restored: Value = client
        .get(format!("{base}/v1/modes"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(modes_restored["modes"][1]["available"], true);
    assert!(modes_restored["modes"][1].get("reason").is_none());
    drop(guard);

    if codewhale_engine::extension_host::test_support::node_for_tests(
        "catalog_lists_plugin_creator_matching_modes_and_disable_updates_modes",
    )
    .is_some()
    {
        let _unforced = CreatorAvailabilityGuard::unforced();
        codewhale_engine::conversation_modes::clear_probe_cache_for_tests();
        let live_catalog: Value = client
            .get(format!("{base}/v1/apps/plugins/catalog"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let live_modes: Value = client
            .get(format!("{base}/v1/modes"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let live_row = live_catalog["builtin"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == "plugin_creator"))
            .expect("plugin_creator row in live builtin catalog");
        assert_eq!(live_row["available"], true);
        assert!(live_row["unavailable_reason"].is_null());
        assert_eq!(live_modes["modes"][1]["available"], true);
        assert!(live_modes["modes"][1].get("reason").is_none());
    }

    handle.abort();
    let _ = handle.await;
    Ok(())
}

#[tokio::test]
async fn warmed_thread_sees_plugin_creator_disable_and_rejects_switch_to_creator() -> Result<()> {
    let _guard = CreatorAvailabilityGuard::set(None);
    codewhale_engine::conversation_modes::clear_probe_cache_for_tests();
    let dir = tempfile::TempDir::new()?;
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        "provider = \"deepseek\"\n[providers.deepseek]\napi_key = \"fixture-key\"\nbase_url = \"http://127.0.0.1:1/v1\"\n",
    )?;
    let Some((addr, threads, handle)) = spawn_test_server_with_config_path(config_path).await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}");

    let std_created: Value = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "standard"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let std_id = std_created["id"]
        .as_str()
        .expect("standard thread id")
        .to_string();

    let creator_created: Value = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let creator_id = creator_created["id"]
        .as_str()
        .expect("creator thread id")
        .to_string();

    client
        .post(format!("{base}/v1/threads/{std_id}/resume"))
        .send()
        .await?
        .error_for_status()?;
    client
        .post(format!("{base}/v1/threads/{creator_id}/resume"))
        .send()
        .await?
        .error_for_status()?;
    assert!(threads.loaded_engine(&std_id).await.is_some());
    assert!(threads.loaded_engine(&creator_id).await.is_some());

    let disabled: Value = client
        .post(format!(
            "{base}/v1/apps/plugins/catalog/plugin_creator/disable"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(disabled["row"]["enabled"], false);
    assert!(threads.loaded_engine(&std_id).await.is_none());
    assert!(threads.loaded_engine(&creator_id).await.is_none());

    let switched = client
        .patch(format!("{base}/v1/threads/{std_id}"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?;
    assert_eq!(switched.status(), reqwest::StatusCode::FAILED_DEPENDENCY);
    let body: Value = switched.json().await?;
    assert_eq!(body["error"]["code"], "mode_unavailable");

    client
        .post(format!("{base}/v1/threads/{std_id}/resume"))
        .send()
        .await?
        .error_for_status()?;
    assert!(threads.loaded_engine(&std_id).await.is_some());

    client
        .post(format!(
            "{base}/v1/apps/plugins/catalog/plugin_creator/enable"
        ))
        .send()
        .await?
        .error_for_status()?;
    assert!(threads.loaded_engine(&std_id).await.is_none());

    let enabled_switch: Value = client
        .patch(format!("{base}/v1/threads/{std_id}"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(enabled_switch["agent_preset"], "creator");

    handle.abort();
    let _ = handle.await;
    Ok(())
}
