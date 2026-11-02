//! Actual CLI consumers share the pinned speech adapter with ToolSpecs.
use codewhale_engine::extension_host::TestManagerGuard;
use codewhale_engine::extension_host::test_support::node_for_tests;
use codewhale_engine::tools::speech::test_support::{config, manager, provider};
use codewhale_plugins::activation::TestPolicyGuard;
use std::sync::Arc;
use wiremock::MockServer;

#[tokio::test(flavor = "current_thread")]
async fn real_cli_speech_and_tts_alias_share_host_plan_and_write_same_audio() {
    use clap::Parser;
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let Some(node) = node_for_tests("real_cli_speech_host") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let server = MockServer::start().await;
    provider(&server, 2).await;
    let parsed = crate::Cli::try_parse_from(["codewhale", "tts", "hello"]).unwrap();
    assert!(matches!(parsed.command, Some(crate::Commands::Speech(_))));
    let args = || crate::SpeechArgs {
        text: "hello".into(),
        output: None,
        output_dir: Some(home.path().join("audio")),
        model: Some("mimo-tts".into()),
        voice: Some("Mia".into()),
        instruction: Some(" warm ".into()),
        voice_prompt: None,
        clone_voice: None,
        format: "pcm".into(),
        json: true,
    };
    crate::run_speech(&config(&server.uri(), false), args())
        .await
        .unwrap();
    crate::run_speech(&config(&server.uri(), true), args())
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(
        std::fs::read(home.path().join("audio/speech.pcm16")).unwrap(),
        b"hi"
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn selected_speech_host_refusal_never_falls_back_or_writes_cli_audio() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let manager = manager(home.path().join("missing-node"), home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let args = crate::SpeechArgs {
        text: "hello".into(),
        output: Some(home.path().join("cli.wav")),
        output_dir: None,
        model: None,
        voice: None,
        instruction: None,
        voice_prompt: None,
        clone_voice: Some(home.path().join("missing.wav")),
        format: "wav".into(),
        json: true,
    };
    assert!(
        crate::run_speech(&config(&server.uri(), true), args)
            .await
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!home.path().join("cli.wav").exists());
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn cli_speech_preserves_sample_file_error_before_format_validation() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let Some(node) = node_for_tests("cli_speech_error_order") else {
        return;
    };
    let manager = manager(node, home.path());
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    // CLI's sample-read error still takes precedence over unsupported format.
    let args = || crate::SpeechArgs {
        text: "hello".into(),
        output: None,
        output_dir: Some(home.path().into()),
        model: None,
        voice: None,
        instruction: None,
        voice_prompt: None,
        clone_voice: Some(home.path().join("missing.wav")),
        format: "flac".into(),
        json: true,
    };
    let rust = crate::run_speech(&config(&server.uri(), false), args())
        .await
        .unwrap_err();
    let host = crate::run_speech(&config(&server.uri(), true), args())
        .await
        .unwrap_err();
    assert_eq!(host.to_string(), rust.to_string());
    assert!(server.received_requests().await.unwrap().is_empty());
    manager.shutdown().await;
}
