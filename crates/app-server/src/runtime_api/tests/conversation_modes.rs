use super::*;
use codewhale_engine::conversation_modes::CreatorAvailabilityGuard;

#[tokio::test]
async fn modes_route_lists_the_table_and_threads_carry_and_refuse_modes() -> Result<()> {
    let _unavailable = CreatorAvailabilityGuard::unforced();
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
    assert_eq!(table["modes"][1]["available"], false);
    assert_eq!(
        table["modes"][1]["reason"],
        codewhale_engine::conversation_modes::CREATOR_NOT_READY_REASON
    );

    let unavailable = client
        .post(format!("{base}/v1/threads"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?;
    assert_eq!(unavailable.status(), reqwest::StatusCode::FAILED_DEPENDENCY);
    let body: Value = unavailable.json().await?;
    assert_eq!(body["error"]["code"], "mode_unavailable");

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
    let summary: Value = client
        .get(format!("{base}/v1/threads/summary"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let row = summary
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id.as_str()))
        .expect("summary row");
    assert_eq!(row["agent_preset"], "standard");

    let patched = client
        .patch(format!("{base}/v1/threads/{id}"))
        .json(&json!({"agent_preset": "creator"}))
        .send()
        .await?;
    assert_eq!(patched.status(), reqwest::StatusCode::FAILED_DEPENDENCY);

    handle.abort();
    let _ = handle.await;
    Ok(())
}
