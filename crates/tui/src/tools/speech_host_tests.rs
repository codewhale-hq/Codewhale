//! Actual CLI/ToolSpec consumers, pinned host and loopback provider. No live provider calls.
use super::*;
use crate::extension_host::tests::node_for_tests;
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use crate::plugins::activation::TestPolicyGuard;
use codewhale_runtime::config::Config;
use codewhale_runtime::features::{Feature, Features, FeaturesToml};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub(crate) fn manager(node: PathBuf, home: &Path) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(home.join("host")),
        ..ExtensionHostOptions::default()
    }))
}
fn context(root: &Path, host: bool) -> ToolContext {
    let mut features = Features::with_defaults();
    if host {
        features.enable(Feature::SpeechHost);
    }
    ToolContext::new(root).with_features(features)
}
pub(crate) fn config(url: &str, host: bool) -> Config {
    let mut config = {
        let __cw_config_init_0_0 = Some("xiaomi-mimo".into());
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value
    };
    config
        .set_provider_base_url_override(
            &config.test_identity_for_kind(ProviderKind::XiaomiMimo),
            Some(url.into()),
        )
        .unwrap();
    config
        .set_provider_api_key_override(
            &config.test_identity_for_kind(ProviderKind::XiaomiMimo),
            Some("local-fixture-only".into()),
        )
        .unwrap();
    config.features = Some(FeaturesToml {
        entries: [("speech_host".into(), host)].into_iter().collect(),
    });
    config
}
pub(crate) async fn provider(server: &MockServer, count: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices":[{"message":{"audio":{"data":"aGk=","transcript":"hi"}}}]
        })))
        .expect(count)
        .mount(server)
        .await;
}
fn equal_results(rust: &ToolResult, host: &ToolResult) {
    assert_eq!(host.success, rust.success);
    assert_eq!(
        host.content, rust.content,
        "exact model-visible result bytes"
    );
    assert_eq!(host.metadata, rust.metadata);
}

#[test]
fn speech_host_is_independent_and_defaults_to_rust() {
    let flags = Features::with_defaults();
    assert!(!flags.enabled(Feature::SpeechHost));
    assert_eq!(
        codewhale_runtime::features::feature_from_key("speech_host"),
        Some(Feature::SpeechHost)
    );
    let mut flags = flags;
    flags.enable(Feature::SpeechHost);
    assert!(!flags.enabled(Feature::FinanceHost));
    assert!(!flags.enabled(Feature::DataHost));
    assert!(!flags.enabled(Feature::ExtensionHost));
}

#[tokio::test(flavor = "current_thread")]
async fn real_host_speech_options_and_formats_match_both_legacy_surfaces() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_speech_options") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    for surface in [SpeechSurface::Tool, SpeechSurface::Cli] {
        let cases = [
            (None, None, None, None, false),
            (Some("mimo-tts"), Some("Mia"), Some(" warm "), None, false),
            (
                None,
                None,
                Some(" slow "),
                Some("\u{85}Bright\u{3000}"),
                false,
            ),
            (
                None,
                None,
                Some("\u{feff}calm\u{feff}"),
                Some("\u{2007}"),
                false,
            ),
            (
                None,
                Some("data:audio/wav;base64,c2FtcGxl"),
                None,
                None,
                false,
            ),
            (None, None, None, None, true),
            (Some("mimo-chat"), None, None, None, false),
            (Some("mimo-v2.5-tts-voiceclone"), None, None, None, false),
            (Some("mimo-v2.5-tts-voicedesign"), None, None, None, false),
            (None, Some(""), None, None, true),
            (None, Some(""), None, None, false),
            (
                Some("mimo-v2.5-tts-voicedesign"),
                None,
                Some("warm"),
                None,
                true,
            ),
        ];
        for (model, voice, instruction, voice_prompt, has_clone_path) in cases {
            let inputs = || SpeechPreparation {
                model,
                voice,
                instruction: instruction.map(str::to_string),
                voice_prompt: voice_prompt.map(str::to_string),
                has_clone_path,
                surface,
            };
            let rust = prepare_speech_options(inputs(), &context(home.path(), false)).await;
            let host = prepare_speech_options(inputs(), &context(home.path(), true)).await;
            match (rust, host) {
                (Ok(rust), Ok(host)) => assert_eq!(host, rust),
                (Err(rust), Err(host)) => assert_eq!(host.to_string(), rust.to_string()),
                values => panic!("speech preparation parity mismatch: {values:?}"),
            }
        }
        for format in [
            "WAV",
            " pcm ",
            "\u{85}MP3\u{85}",
            "pcm16",
            "flac",
            "\u{feff}wav",
        ] {
            let rust = prepare_speech_format(format, surface, &context(home.path(), false)).await;
            let host = prepare_speech_format(format, surface, &context(home.path(), true)).await;
            assert_eq!(
                host.map_err(|error| error.to_string()),
                rust.map_err(|error| error.to_string())
            );
        }
    }
    assert!(!crate::plugins::activation::extension_host_policy_enabled());
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_host_speech_tool_and_hidden_alias_match_requests_results_and_audio() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_host_speech_tool") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let server = MockServer::start().await;
    let client = CodewhaleClient::new(&config(&server.uri(), false)).unwrap();
    std::fs::write(home.path().join("sample.wav"), b"sample").unwrap();
    let inputs = [
        json!({"text":" hello ","model":"mimo-tts","format":"pcm","output":"audio/result.pcm16"}),
        json!({"text":"hello","voice_prompt":"\u{85}Bright\u{3000}","instruction":" slow ","output":"audio/result.wav"}),
        json!({"text":"hello","voice":"data:audio/wav;base64,c2FtcGxl","instruction":"clone","output":"audio/result.wav"}),
        json!({"text":"hello","clone_voice":"sample.wav","output":"audio/result.wav"}),
        json!({"text":"hello","model":"mimo-v2.5-tts-voicedesign","clone_voice":"sample.wav","instruction":"warm","output":"audio/result.wav"}),
    ];
    for name in ["speech", "tts"] {
        let tool = if name == "tts" {
            SpeechTool::alias(name, Some(client.clone()), None)
        } else {
            SpeechTool::new(name, Some(client.clone()), None)
        };
        assert_eq!(tool.model_visible(), name == "speech");
        for input in &inputs {
            server.reset().await;
            provider(&server, 2).await;
            let expected = tool
                .execute(input.clone(), &context(home.path(), false))
                .await
                .unwrap();
            let actual = tool
                .execute(input.clone(), &context(home.path(), true))
                .await
                .unwrap();
            equal_results(&expected, &actual);
            assert_eq!(
                std::fs::read(home.path().join(input["output"].as_str().unwrap())).unwrap(),
                b"hi"
            );
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(
                requests[0].body, requests[1].body,
                "exact provider request bytes"
            );
        }
    }
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn speech_host_refusal_and_cancel_never_fallback_or_write() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let client = CodewhaleClient::new(&config(&server.uri(), false)).unwrap();
    let manager = manager(home.path().join("missing-node"), home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let tool = SpeechTool::new("speech", Some(client), None);
    let input =
        json!({"text":"hello","clone_voice":"unread-missing.wav","output":"must-not-exist.wav"});
    let error = tool
        .execute(input.clone(), &context(home.path(), true))
        .await
        .unwrap_err();
    assert!(!matches!(error, ToolError::InvalidInput { .. }));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!home.path().join("must-not-exist.wav").exists());
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let error = tool
        .execute(input, &context(home.path(), true).with_cancel_token(token))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cancel"));
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn speech_tool_preserves_validation_and_file_error_order() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("speech_error_order") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let server = MockServer::start().await;
    let tool = SpeechTool::new(
        "speech",
        Some(CodewhaleClient::new(&config(&server.uri(), false)).unwrap()),
        None,
    );
    for input in [
        json!({"text":"hello","format":"flac","output":"../escape.wav","model":"mimo-chat"}),
        json!({"text":"hello","output":"../escape.wav","model":"mimo-chat"}),
        json!({"text":"hello","clone_voice":"missing.wav","format":"pcm"}),
        json!({"text":"hello","voice_prompt":"","model":"mimo-v2.5-tts-voicedesign"}),
    ] {
        let rust = tool
            .execute(input.clone(), &context(home.path(), false))
            .await
            .unwrap_err();
        let host = tool
            .execute(input, &context(home.path(), true))
            .await
            .unwrap_err();
        assert_eq!(host.to_string(), rust.to_string());
    }
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_host_speech_retains_core_network_policy_before_provider_calls() {
    use codewhale_net::network_policy::{NetworkPolicy, NetworkPolicyDecider};
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("speech_network_policy") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let server = MockServer::start().await;
    let tool = SpeechTool::new(
        "speech",
        Some(CodewhaleClient::new(&config(&server.uri(), false)).unwrap()),
        None,
    );
    for decision in [Decision::Deny, Decision::Prompt] {
        let policy = || {
            NetworkPolicyDecider::new(
                NetworkPolicy {
                    default: decision.into(),
                    allow: Vec::new(),
                    deny: Vec::new(),
                    proxy: Vec::new(),
                    proxy_fake_ip_cidrs: Vec::new(),
                    audit: false,
                },
                None,
            )
        };
        let input = json!({"text":"hello","output":"blocked.wav"});
        let rust = tool
            .execute(
                input.clone(),
                &context(home.path(), false).with_network_policy(policy()),
            )
            .await
            .unwrap_err();
        let host = tool
            .execute(
                input,
                &context(home.path(), true).with_network_policy(policy()),
            )
            .await
            .unwrap_err();
        assert_eq!(host.to_string(), rust.to_string());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!home.path().join("blocked.wav").exists());
    manager.shutdown().await;
}
