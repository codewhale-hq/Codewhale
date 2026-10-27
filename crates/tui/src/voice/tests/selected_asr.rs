//! `transcribe_selected` keeps a recording on the ASR route that was selected.
//!
//! The configured provider in every fixture below has a literal key and a
//! loopback wiremock endpoint, so a regression that falls back to it is
//! observable as a recorded request rather than inferred from a sleep.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::test_support::{EnvVarGuard, lock_test_env};
use crate::voice::{ASR_MODEL, DictateError, VOICE_CONTROL_MODEL, transcribe_selected};
use codewhale_runtime::config::Config;

const FIXTURE_KEY: &str = "fixture-provider-key";

async fn provider_fixture(content: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{ "message": { "content": content } }]
        })))
        .mount(&server)
        .await;
    server
}

/// Fully explicit provider route: the key and endpoint come from the config
/// table, so nothing is read from real auth state or ambient endpoint env.
fn fixture_config(server: &MockServer) -> Config {
    toml::from_str(&format!(
        "provider = \"openai\"\n\n[providers.openai]\napi_key = \"{FIXTURE_KEY}\"\nbase_url = \"{}/v1\"\nmodel = \"fixture-model\"\n",
        server.uri()
    ))
    .expect("fixture config parses")
}

/// Hold every env mutation for the whole test: the returned guards live until
/// the caller drops them, which is after any `spawn_blocking` transcriber has
/// been awaited to completion.
struct IsolatedEnv {
    // Field order is drop order: guards restore before the lock releases.
    _guards: Vec<EnvVarGuard>,
    _home: tempfile::TempDir,
    _lock: crate::test_support::TestEnvLock,
}

fn isolated_env(path_override: Option<&std::path::Path>) -> IsolatedEnv {
    let lock = lock_test_env();
    let home = tempfile::tempdir().expect("isolated codewhale home");
    let mut guards = vec![
        EnvVarGuard::set("CODEWHALE_HOME", home.path()),
        EnvVarGuard::remove("GROQ_API_KEY"),
        EnvVarGuard::remove("CODEWHALE_BASE_URL"),
        EnvVarGuard::remove("DEEPSEEK_BASE_URL"),
        EnvVarGuard::remove("OPENAI_BASE_URL"),
        EnvVarGuard::remove("OPENAI_API_KEY"),
        EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV),
        EnvVarGuard::remove(codewhale_config::CLI_API_KEY_SOURCE_ENV),
        EnvVarGuard::remove(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV),
    ];
    if let Some(dir) = path_override {
        guards.push(EnvVarGuard::set("PATH", dir));
    }
    IsolatedEnv {
        _guards: guards,
        _home: home,
        _lock: lock,
    }
}

#[tokio::test]
async fn failed_local_whisper_never_uploads_to_the_configured_provider() {
    // No whisper binary resolves from an empty PATH; HOME is left alone.
    let empty_path = tempfile::tempdir().expect("empty PATH dir");
    let _env = isolated_env(Some(empty_path.path()));
    let server = provider_fixture("{\"text\":\"must not be requested\"}").await;
    let config = fixture_config(&server);

    let result = transcribe_selected(&config, "local-whisper", &[0; 16], None).await;

    assert!(
        matches!(&result, Err(DictateError::Transcription(msg)) if msg.contains("local whisper")),
        "{result:?}"
    );
    let requests = server.received_requests().await.expect("request log");
    assert_eq!(
        requests.len(),
        0,
        "audio reached the provider: {requests:?}"
    );
}

#[tokio::test]
async fn failed_groq_without_a_key_never_falls_back_to_the_configured_provider() {
    let _env = isolated_env(None);
    let server = provider_fixture("{\"text\":\"must not be requested\"}").await;
    let config = fixture_config(&server);

    let result = transcribe_selected(&config, "groq", &[0; 16], Some("existing text")).await;

    assert!(
        matches!(&result, Err(DictateError::Transcription(msg)) if msg.contains("GROQ_API_KEY")),
        "{result:?}"
    );
    let requests = server.received_requests().await.expect("request log");
    assert_eq!(
        requests.len(),
        0,
        "audio reached the provider: {requests:?}"
    );
}

#[tokio::test]
async fn selected_provider_asr_uses_plain_or_composer_aware_request() {
    let _env = isolated_env(None);
    let server = provider_fixture("{\"text\":\"hello from provider\"}").await;
    let config = fixture_config(&server);

    // Plain ASR returns the raw message content.
    let plain = transcribe_selected(&config, "provider", &[0; 16], None)
        .await
        .expect("plain provider ASR");
    assert_eq!(plain, "{\"text\":\"hello from provider\"}");
    // Voice control parses the JSON reply and sends the composer text along.
    let assisted = transcribe_selected(&config, "provider", &[0; 16], Some("draft so far"))
        .await
        .expect("voice-control provider ASR");
    assert_eq!(assisted, "hello from provider");

    let requests = server.received_requests().await.expect("request log");
    assert_eq!(requests.len(), 2, "{requests:?}");
    for request in &requests {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(format!("Bearer {FIXTURE_KEY}").as_str())
        );
    }
    let plain_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(plain_body["model"], ASR_MODEL);
    assert!(!plain_body.to_string().contains("draft so far"));

    let control_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(control_body["model"], VOICE_CONTROL_MODEL);
    let context = control_body["messages"][1]["content"][0]["text"]
        .as_str()
        .expect("composer context block");
    let context: serde_json::Value = serde_json::from_str(context).unwrap();
    assert_eq!(context["current_text"], "draft so far");
}
