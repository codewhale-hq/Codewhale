use super::*;
use reqwest::StatusCode as HttpStatus;

fn options_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/acp-options-agent.mjs")
        .canonicalize()
        .expect("the options fixture agent")
}

fn with_query(base: &str, pairs: &[(&str, &str)]) -> reqwest::Url {
    let mut url = reqwest::Url::parse(base).expect("a url");
    for (key, value) in pairs {
        url.query_pairs_mut().append_pair(key, value);
    }
    url
}

fn received(workspace: &Path) -> Vec<Value> {
    fs::read_to_string(workspace.join("options-received.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn wait_for_usage_total(
    manager: &SharedRuntimeThreadManager,
    thread_id: &str,
) -> Result<RuntimeEventRecord> {
    for _ in 0..600 {
        let events = manager.events_since_async(thread_id, None).await?;
        if let Some(event) = events.into_iter().find(|event| {
            event.event == "turn.agent_usage" && event.payload["kind"] == "turn_total"
        }) {
            return Ok(event);
        }
        sleep(Duration::from_millis(50)).await;
    }
    bail!("the agent's usage was never recorded")
}

#[tokio::test]
async fn the_options_route_offers_the_agents_choices_and_a_chosen_one_reaches_it() -> Result<()> {
    let _env = lock_test_env();
    let _fixture = EnvVarGuard::set("CODEWHALE_ACP_FIXTURE_AGENT", options_fixture());
    let Some((addr, manager, handle)) = spawn_test_server().await? else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let workspace = tempfile::tempdir()?;
    let cwd = workspace.path().canonicalize()?;
    let url = format!("http://{addr}/v1/agents/external/fixture/options");

    let offered: Value = client
        .get(with_query(&url, &[("cwd", cwd.to_str().unwrap())]))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(offered["agent"]["id"], "fixture");
    let controls = offered["controls"].as_array().unwrap();
    let model = controls.iter().find(|c| c["id"] == "model").unwrap();
    let values: Vec<&str> = model["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["value"].as_str().unwrap())
        .collect();
    assert!(values.len() >= 2, "{values:?}");
    assert!(controls.iter().any(|c| c["id"] == "mode"));
    assert!(
        controls.iter().any(|c| c["id"] == "config:verbose"
            && c["unavailable"]["code"] == "control_type_unsupported")
    );
    assert!(
        offered["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["action"] == "steering" && a["supported"] == false)
    );
    let leaked = fs::read_to_string(cwd.join("options-env.json"))?;
    assert!(!leaked.contains("CODEWHALE_ACP_FIXTURE_AGENT"));

    for (query, status, code) in [
        (vec![], HttpStatus::BAD_REQUEST, None),
        (vec![("cwd", "relative/dir")], HttpStatus::BAD_REQUEST, None),
        (
            vec![("cwd", "/definitely/not/here")],
            HttpStatus::NOT_FOUND,
            None,
        ),
    ] {
        let response = client.get(with_query(&url, &query)).send().await?;
        assert_eq!(response.status(), status, "{query:?}");
        let body: Value = response.json().await?;
        assert_eq!(body["error"]["code"].as_str(), code, "{body}");
    }
    let unknown = client
        .get(with_query(
            &format!("http://{addr}/v1/agents/external/nope/options"),
            &[("cwd", cwd.to_str().unwrap())],
        ))
        .send()
        .await?;
    assert_eq!(unknown.status(), HttpStatus::NOT_FOUND);
    let body: Value = unknown.json().await?;
    assert_eq!(body["error"]["code"], "unknown_agent");
    let bad_id = client
        .get(with_query(
            &format!("http://{addr}/v1/agents/external/..%2Fsh/options"),
            &[("cwd", cwd.to_str().unwrap())],
        ))
        .send()
        .await?;
    assert!(bad_id.status().is_client_error());

    let chosen = values[1];
    let created: Value = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "workspace": cwd,
            "permission_posture": "ask",
            "executor": {"kind": "acp", "agent_id": "fixture", "config": {"model": chosen}}
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let thread_id = created["id"].as_str().unwrap().to_string();

    let refused = client
        .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
        .json(&json!({"prompt": "x", "model": "other"}))
        .send()
        .await?;
    assert_eq!(refused.status(), HttpStatus::FAILED_DEPENDENCY);
    let body: Value = refused.json().await?;
    assert_eq!(body["error"]["code"], "unsupported_control");

    client
        .post(format!("http://{addr}/v1/threads/{thread_id}/turns"))
        .json(&json!({"prompt": "hello"}))
        .send()
        .await?
        .error_for_status()?;
    let total = wait_for_usage_total(&manager, &thread_id).await?;
    assert_eq!(total.payload["source"], "agent reported");
    assert_eq!(total.payload["cost"], "unknown");
    assert_eq!(total.payload["tokens"]["total_tokens"], 150);
    let log = received(&cwd);
    let prompt = log
        .iter()
        .find(|e| e["method"] == "session/prompt")
        .unwrap();
    assert_eq!(prompt["state"]["model"], chosen);

    let stale = client
        .post(format!("http://{addr}/v1/threads"))
        .json(&json!({
            "workspace": cwd,
            "executor": {"kind": "acp", "agent_id": "fixture", "config": {"model": "model-z"}}
        }))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    let stale_id = stale["id"].as_str().unwrap();
    let response = client
        .post(format!("http://{addr}/v1/threads/{stale_id}/turns"))
        .json(&json!({"prompt": "x"}))
        .send()
        .await?;
    assert_eq!(response.status(), HttpStatus::FAILED_DEPENDENCY);
    let body: Value = response.json().await?;
    assert_eq!(body["error"]["code"], "choice_not_offered");

    handle.abort();
    Ok(())
}
