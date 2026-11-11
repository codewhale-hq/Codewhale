use super::*;
use crate::conversation_modes::{CREATOR, CreatorAvailabilityGuard, ModeRefusal, STANDARD};

fn refusal_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<ModeRefusal>()
        .expect("typed refusal")
        .code
}

fn create_request(preset: Option<&str>) -> CreateThreadRequest {
    CreateThreadRequest {
        agent_preset: preset.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn thread_record_without_agent_preset_loads_as_standard() -> Result<()> {
    let thread = sample_thread("thr_modes_legacy");
    let mut raw = serde_json::to_value(&thread)?;
    assert!(raw.get("agent_preset").is_none());
    let legacy: ThreadRecord = serde_json::from_value(raw.clone())?;
    assert_eq!(legacy.agent_preset, None);
    assert_eq!(
        crate::conversation_modes::effective_id(legacy.agent_preset.as_deref()),
        STANDARD
    );
    raw["agent_preset"] = json!(CREATOR);
    let creator: ThreadRecord = serde_json::from_value(raw)?;
    assert_eq!(creator.agent_preset.as_deref(), Some(CREATOR));
    Ok(())
}

#[tokio::test]
async fn mode_is_set_at_creation_and_refused_when_unknown_or_unavailable() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let manager = test_manager(tmp.path().to_path_buf())?;
    {
        let _available = CreatorAvailabilityGuard::set(None);
        let created = manager.create_thread(create_request(Some(CREATOR))).await?;
        assert_eq!(created.agent_preset.as_deref(), Some(CREATOR));
        let standard = manager
            .create_thread(create_request(Some(STANDARD)))
            .await?;
        assert_eq!(standard.agent_preset, None);
        let unknown = manager
            .create_thread(create_request(Some("minimal")))
            .await
            .expect_err("unknown id");
        assert_eq!(refusal_code(&unknown), ModeRefusal::UNKNOWN_MODE);
    }
    let _missing = CreatorAvailabilityGuard::set(Some("no runtime"));
    let unavailable = manager
        .create_thread(create_request(Some(CREATOR)))
        .await
        .expect_err("unavailable");
    assert_eq!(refusal_code(&unavailable), ModeRefusal::MODE_UNAVAILABLE);
    assert_eq!(unavailable.to_string(), "no runtime");
    let table = crate::conversation_modes::current_table(manager.extension_host_options()).await;
    assert!(table.modes[0].available && table.modes[0].reason.is_none());
    assert!(!table.modes[1].available);
    assert_eq!(table.modes[1].reason.as_deref(), Some("no runtime"));
    Ok(())
}

#[tokio::test]
async fn mode_changes_only_before_the_first_turn_and_never_with_an_executor() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let manager = test_manager(tmp.path().to_path_buf())?;
    let _available = CreatorAvailabilityGuard::set(None);
    let thread = manager.create_thread(create_request(None)).await?;
    let patch = |preset: &str| UpdateThreadRequest {
        agent_preset: Some(preset.to_string()),
        ..Default::default()
    };

    let updated = manager.update_thread(&thread.id, patch(CREATOR)).await?;
    assert_eq!(updated.agent_preset.as_deref(), Some(CREATOR));
    let unknown = manager
        .update_thread(&thread.id, patch("nope"))
        .await
        .expect_err("unknown id");
    assert_eq!(refusal_code(&unknown), ModeRefusal::UNKNOWN_MODE);

    let mut with_turn = manager.store.load_thread(&thread.id)?;
    with_turn.latest_turn_id = Some("turn_first".to_string());
    manager.store.save_thread(&with_turn)?;
    let locked = manager
        .update_thread(&thread.id, patch(STANDARD))
        .await
        .expect_err("locked");
    assert_eq!(refusal_code(&locked), ModeRefusal::MODE_LOCKED);
    manager.update_thread(&thread.id, patch(CREATOR)).await?;

    let with_executor = CreateThreadRequest {
        agent_preset: Some(CREATOR.to_string()),
        executor: Some(CreateThreadExecutor {
            kind: "acp".to_string(),
            agent_id: "opencode".to_string(),
            config: CreateThreadExecutorConfig::default(),
        }),
        ..Default::default()
    };
    let refused = manager
        .create_thread(with_executor)
        .await
        .expect_err("executor");
    assert_eq!(
        refusal_code(&refused),
        ModeRefusal::MODE_NOT_SUPPORTED_WITH_EXECUTOR
    );
    Ok(())
}

#[tokio::test]
async fn creator_is_listed_unavailable_until_it_loads_its_tools() -> Result<()> {
    let _real = CreatorAvailabilityGuard::unforced();
    let tmp = tempfile::tempdir()?;
    let manager = test_manager(tmp.path().to_path_buf())?;
    let table = crate::conversation_modes::current_table(manager.extension_host_options()).await;
    assert!(table.modes[0].available);
    assert!(!table.modes[1].available);
    assert_eq!(
        table.modes[1].reason.as_deref(),
        Some(crate::conversation_modes::CREATOR_NOT_READY_REASON)
    );
    let refused = manager
        .create_thread(create_request(Some(CREATOR)))
        .await
        .expect_err("not ready");
    assert_eq!(refusal_code(&refused), ModeRefusal::MODE_UNAVAILABLE);
    Ok(())
}
