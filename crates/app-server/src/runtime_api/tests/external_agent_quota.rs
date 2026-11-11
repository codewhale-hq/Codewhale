use super::*;
use reqwest::StatusCode as HttpStatus;
use std::collections::BTreeMap;

const FIXTURE_TOKEN: &str = "cw-fixture-token-7993-do-not-echo";

fn reasons(body: &Value) -> BTreeMap<String, String> {
    body["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .map(|entry| {
            assert!(
                entry["quota"].get("source").is_none(),
                "no source without a reader: {entry}"
            );
            assert_eq!(entry["quota"]["status"], "unknown", "{entry}");
            (
                entry["id"].as_str().expect("id").to_string(),
                entry["quota"]["reason"]
                    .as_str()
                    .expect("reason")
                    .to_string(),
            )
        })
        .collect()
}

fn snapshot(paths: &[PathBuf]) -> Vec<(Vec<u8>, std::time::SystemTime)> {
    paths
        .iter()
        .map(|path| {
            (
                fs::read(path).expect("fixture credential"),
                fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .expect("mtime"),
            )
        })
        .collect()
}

#[tokio::test]
async fn agent_quota_reads_nothing_until_flag_and_consent_and_never_echoes_a_token() -> Result<()> {
    let _env = lock_test_env();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let mut credentials = Vec::new();
    for (dir, file) in [
        (".claude", ".credentials.json"),
        (".codex", "auth.json"),
        (".gemini", "oauth_creds.json"),
        (".kimi", "credentials.json"),
        (".grok", "auth.json"),
    ] {
        let dir = home.join(dir);
        fs::create_dir_all(&dir)?;
        let path = dir.join(file);
        fs::write(
            &path,
            format!(r#"{{"access_token":"{FIXTURE_TOKEN}","refresh_token":"{FIXTURE_TOKEN}"}}"#),
        )?;
        credentials.push(path);
    }
    let before = snapshot(&credentials);
    let config_file = root.path().join("config").join("config.toml");
    fs::create_dir_all(config_file.parent().unwrap())?;
    fs::write(
        &config_file,
        "provider = \"deepseek\"\napi_key = \"runtime-api-test-key\"\nbase_url = \"http://127.0.0.1:1/v1\"\n",
    )?;
    let _home = EnvVarGuard::set("HOME", &home);
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config_file);
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_config_path(config_file.clone()).await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let mut seen = String::new();

    let read = || {
        let client = client.clone();
        let url = format!("http://{addr}/v1/agents/external/quota");
        async move {
            let text = client
                .get(url)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            Ok::<String, anyhow::Error>(text)
        }
    };
    let set = |key: &'static str, value: &'static str| {
        let client = client.clone();
        let url = format!("http://{addr}/v1/config");
        async move {
            let response = client
                .post(url)
                .json(&json!({"key": key, "value": value, "persist": true}))
                .send()
                .await?;
            let status = response.status();
            Ok::<(HttpStatus, String), anyhow::Error>((status, response.text().await?))
        }
    };

    let text = read().await?;
    seen.push_str(&text);
    let body: Value = serde_json::from_str(&text)?;
    assert_eq!(body["default_source"], "agent_reported");
    let initial = reasons(&body);
    assert_eq!(initial.len(), codewhale_engine::fleet::agents::AGENTS.len());
    assert!(
        initial.values().all(|reason| reason == "flag_off"),
        "{initial:?}"
    );

    for (key, value) in [
        ("agents.quota.claude", "on"),
        ("agents.quota.codex.consent", "granted"),
    ] {
        let (status, text) = set(key, value).await?;
        seen.push_str(&text);
        assert_eq!(status, HttpStatus::OK, "{key}: {text}");
    }
    let text = read().await?;
    seen.push_str(&text);
    let gated = reasons(&serde_json::from_str(&text)?);
    assert_eq!(gated["claude"], "consent_missing");
    assert_eq!(gated["codex"], "flag_off");
    assert_eq!(gated["grok"], "flag_off");

    for (key, value) in [
        ("agents.quota.claude.consent", "granted"),
        ("agents.quota.codex", "true"),
    ] {
        let (status, text) = set(key, value).await?;
        seen.push_str(&text);
        assert_eq!(status, HttpStatus::OK, "{key}: {text}");
    }
    let saved = codewhale_runtime::settings::Settings::load_persisted()?;
    assert!(saved.agents.quota("claude").enabled);
    assert!(saved.agents.quota("claude").consented_at.is_some());
    let text = read().await?;
    seen.push_str(&text);
    let consented = reasons(&serde_json::from_str(&text)?);
    assert_eq!(consented["claude"], "not_implemented");
    assert_eq!(consented["codex"], "not_implemented");
    assert_eq!(consented["opencode"], "flag_off");

    for (key, value) in [
        ("agents.quota.claude.consent", "revoked"),
        ("agents.quota.codex", "off"),
    ] {
        let (status, text) = set(key, value).await?;
        seen.push_str(&text);
        assert_eq!(status, HttpStatus::OK, "{key}: {text}");
    }
    let text = read().await?;
    seen.push_str(&text);
    let revoked = reasons(&serde_json::from_str(&text)?);
    assert_eq!(revoked["claude"], "consent_missing");
    assert_eq!(revoked["codex"], "flag_off");

    for (key, value) in [
        ("agents.quota.claude.consent", FIXTURE_TOKEN),
        ("agents.quota.Claude/../x", "on"),
        ("agents.quota.claude", FIXTURE_TOKEN),
    ] {
        let (status, text) = set(key, value).await?;
        assert_eq!(status, HttpStatus::BAD_REQUEST, "{key}: {text}");
        seen.push_str(&text);
    }

    let (status, text) = set("agents.quota.not-an-agent", "on").await?;
    seen.push_str(&text);
    assert_eq!(status, HttpStatus::OK, "{text}");
    let text = read().await?;
    seen.push_str(&text);
    assert_eq!(
        reasons(&serde_json::from_str(&text)?)["not-an-agent"],
        "unknown_agent"
    );

    let settings_file = fs::read_to_string(config_file.with_file_name("settings.toml"))?;
    assert!(
        settings_file.contains("[agents.quota.claude]"),
        "{settings_file}"
    );
    assert!(!settings_file.contains(FIXTURE_TOKEN));
    assert!(
        !seen.contains(FIXTURE_TOKEN),
        "a response echoed the fixture token"
    );
    assert_eq!(snapshot(&credentials), before, "a credential file changed");

    handle.abort();
    Ok(())
}
