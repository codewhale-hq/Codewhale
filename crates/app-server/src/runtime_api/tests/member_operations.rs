use super::*;
use codewhale_engine::llm_client::mock::MockLlmClient;
use codewhale_engine::runtime_threads::job_operations::{
    MEMBER_RESERVED, MemberOperation, MemberOperationRecord,
};

struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn fleet() -> Value {
    json!({
        "schema": "fleet",
        "schema_revision": 2,
        "name": "Review team",
        "members": [
            { "id": "reviewer", "role": "reviewer", "provider": "deepseek", "model": "deepseek-v4-pro" }
        ]
    })
}

fn start_body(key: &str, prompt: &str) -> Value {
    json!({
        "operation_key": key,
        "member_id": "reviewer",
        "display_name": "Reviewer",
        "prompt": prompt,
        "fleet": fleet(),
    })
}

fn reserve(
    threads: &SharedRuntimeThreadManager,
    thread_id: &str,
    key: &str,
    generation: &str,
) -> Result<()> {
    threads
        .job_operation_store()
        .with_job_operations(|transaction| {
            let now = chrono::Utc::now().timestamp_millis();
            transaction.save_member(&MemberOperationRecord {
                schema_version: 1,
                operation: MemberOperation {
                    id: transaction.member_operation_id(thread_id, key)?,
                    store_id: threads.job_operation_store_id().to_string(),
                    thread_id: thread_id.to_string(),
                    member_id: "reviewer".to_string(),
                    operation_key: key.to_string(),
                    agent_id: "agent_reservedfixture".to_string(),
                    state: MEMBER_RESERVED.to_string(),
                    created_at_ms: now,
                    updated_at_ms: now,
                },
                request_fingerprint: "a".repeat(64),
                owner_generation: generation.to_string(),
            })
        })
}

#[tokio::test]
async fn member_starts_are_durable_replayable_and_refuse_foreign_or_mismatched_replay() -> Result<()>
{
    let _env = lock_test_env();
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let config = Config::default().with_legacy_root(
        Some("member-fixture-key".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    let (addr, threads, server) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root.clone(),
        root.join("sessions"),
        None,
        false,
        workspace.clone(),
        TestServerOverrides {
            config: Some(config),
            ..TestServerOverrides::default()
        },
    )
    .await?
    .context("the member fixture needs a loopback listener")?;
    let _server = AbortOnDrop(server.abort_handle());
    threads.set_test_model_client(Arc::new(MockLlmClient::new(Vec::new())));
    let client = codewhale_release::tls::reqwest_client();
    let url = |path: &str| format!("http://{addr}{path}");
    let create_thread = || async {
        let created: Value = client
            .post(url("/v1/threads"))
            .json(&json!({ "model": "deepseek-v4-pro", "mode": "agent", "workspace": workspace }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::Ok(created["id"].as_str().context("thread id")?.to_string())
    };
    let thread = create_thread().await?;
    let other = create_thread().await?;
    let info: Value = client
        .get(url("/v1/runtime/info"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        info["capabilities"]["thread_fleet_member_durable_operations_v1"],
        true
    );
    let store_id = threads.job_operation_store_id().to_string();
    let members = url(&format!("/v1/threads/{thread}/members"));

    let first = client
        .post(&members)
        .json(&start_body("team-start-1", "Review the diff"))
        .send()
        .await?;
    let status = first.status();
    let receipt: Value = first.json().await?;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["state"], "accepted");
    assert_eq!(receipt["runtime_store_id"], store_id.as_str());
    assert_eq!(receipt["operation_key"], "team-start-1");
    let agent_id = receipt["agent_id"]
        .as_str()
        .context("agent id")?
        .to_string();
    let runs = member_runs(&client, addr, &thread).await?;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        runs[0]["spec"]["worker_id"],
        agent_id.as_str(),
        "the receipt names the worker it started"
    );

    let replayed: Value = client
        .post(&members)
        .json(&start_body("team-start-1", "Review the diff"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        replayed, receipt,
        "exact replay returns the original receipt"
    );
    let read: Value = client
        .get(url(&format!(
            "/v1/threads/{thread}/members/operations/team-start-1"
        )))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(read, receipt);

    let mismatched = client
        .post(&members)
        .json(&start_body("team-start-1", "Review something else"))
        .send()
        .await?;
    assert_eq!(mismatched.status(), StatusCode::CONFLICT);
    let body: Value = mismatched.json().await?;
    assert_eq!(body["error"]["code"], "member_operation_mismatch");

    let foreign = client
        .get(url(&format!(
            "/v1/threads/{other}/members/operations/team-start-1"
        )))
        .send()
        .await?;
    assert_eq!(
        foreign.status(),
        StatusCode::NOT_FOUND,
        "a key never crosses threads"
    );

    let mut ghost = start_body("team-ghost", "Review the diff");
    ghost["member_id"] = json!("ghost");
    let refused = client.post(&members).json(&ghost).send().await?;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let withdrawn = client
        .get(url(&format!(
            "/v1/threads/{thread}/members/operations/team-ghost"
        )))
        .send()
        .await?;
    assert_eq!(
        withdrawn.status(),
        StatusCode::NOT_FOUND,
        "a start the Engine refused withdraws its reservation"
    );

    let mut stale = start_body("team-start-2", "Review the diff");
    stale["expected_store_id"] = json!("another-store");
    let stale = client.post(&members).json(&stale).send().await?;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let body: Value = stale.json().await?;
    assert_eq!(body["error"]["code"], "member_operation_store_changed");

    let mut forged = start_body("team-start-3", "Review the diff");
    forged["request_fingerprint"] = json!("b".repeat(64));
    let forged = client.post(&members).json(&forged).send().await?;
    assert_eq!(forged.status(), StatusCode::BAD_REQUEST);

    let generation = threads.job_operation_generation()?;
    reserve(&threads, &other, "in-flight", &generation)?;
    let deleted = threads.delete_thread(&other).await;
    assert!(
        deleted
            .as_ref()
            .is_err_and(|error| error.to_string().contains("pending member operation")),
        "a thread with a start in flight is not deleted: {deleted:?}"
    );

    reserve(
        &threads,
        &other,
        "before-restart",
        &Uuid::new_v4().to_string(),
    )?;
    let abandoned: Value = client
        .get(url(&format!(
            "/v1/threads/{other}/members/operations/before-restart"
        )))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        abandoned["state"], "abandoned",
        "a reservation an earlier Engine never started is settled, not left unknown"
    );

    let cancelled = client
        .post(url(&format!("/v1/agent-runs/{agent_id}/cancel")))
        .send()
        .await?;
    assert!(cancelled.status().is_success(), "{}", cancelled.status());
    let after_cancel: Value = client
        .post(&members)
        .json(&start_body("team-start-1", "Review the diff"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        after_cancel["agent_id"], agent_id,
        "owner cancellation never makes a replay start a second worker"
    );
    assert_eq!(member_runs(&client, addr, &thread).await?.len(), 1);
    Ok(())
}

async fn member_runs(
    client: &reqwest::Client,
    addr: SocketAddr,
    thread: &str,
) -> Result<Vec<Value>> {
    let listed: Value = client
        .get(format!("http://{addr}/v1/agent-runs"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(listed["runs"]
        .as_array()
        .context("runs")?
        .iter()
        .filter(|run| run["owner_session_id"] == thread)
        .cloned()
        .collect())
}
