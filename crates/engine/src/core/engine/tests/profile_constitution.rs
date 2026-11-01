use super::*;
use codewhale_config::user_constitution::{ProfileConstitution, ProfileConstitutionSnapshot};

#[tokio::test]
async fn profile_constitution_reaches_real_engine_requests_and_survives_history_repair() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    let _home = crate::test_support::SealedHome::new();
    let workspace = tempfile::tempdir().unwrap();
    let config = Config::default();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("Done"),
        canned::simple_text_turn("Done again"),
        canned::simple_text_turn("Follow-up done"),
    ]));
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        mock.clone(),
    );
    let drainer =
        tokio::spawn(async move { while handle.rx_event.write().await.recv().await.is_some() {} });
    for (revision, note) in [
        (1, "Profile alpha preference"),
        (2, "Profile beta preference"),
    ] {
        let Op::SendMessage(mut spec) =
            external_user_message_op("Say hello", AppMode::Agent, &config)
        else {
            unreachable!()
        };
        spec.profile_constitution = Some(ProfileConstitutionSnapshot {
            account_id: "acct_fixture".into(),
            revision,
            constitution: ProfileConstitution {
                notes: note.into(),
                ..ProfileConstitution::default()
            },
        });
        let outcome = engine.handle_send_message(spec).await;
        assert!(!matches!(outcome, SendMessageOutcome::NotStarted { .. }));
        let requests = mock.captured_requests();
        let request = serde_json::to_string(requests.last().unwrap()).unwrap();
        assert!(request.contains(note));
        assert!(request.contains("replaces all earlier personal constitution snapshots"));
        assert!(request.contains("do not change permissions"));
    }
    let latest = engine
        .session
        .messages
        .iter()
        .rev()
        .find(|message| crate::runtime_handoff::constitution_display(message).is_some())
        .unwrap()
        .clone();
    assert!(
        crate::runtime_handoff::constitution_display(&latest)
            .unwrap()
            .contains("revision 2")
    );
    let Op::SendMessage(mut continuation) =
        external_user_message_op("A background task finished", AppMode::Agent, &config)
    else {
        unreachable!()
    };
    continuation.provenance = UserInputProvenance::Runtime;
    // A host can admit a runtime follow-up itself, so this must not depend on
    // the Engine's autonomous scheduling flag.
    let outcome = engine.handle_admitted_message(continuation, false).await;
    assert!(!matches!(outcome, SendMessageOutcome::NotStarted { .. }));
    assert!(
        serde_json::to_string(mock.captured_requests().last().unwrap())
            .unwrap()
            .contains("Profile beta preference")
    );
    assert_eq!(
        engine
            .session
            .messages
            .iter()
            .rev()
            .find(|message| crate::runtime_handoff::constitution_display(message).is_some()),
        Some(&latest)
    );
    engine.session.messages.clear();
    engine.record_current_constitution().await;
    assert_eq!(engine.session.messages.last(), Some(&latest));
    let count = engine.session.messages.len();
    engine.record_current_constitution().await;
    assert_eq!(engine.session.messages.len(), count);
    engine.constitution_block = None;
    engine.record_current_constitution().await;
    assert!(
        crate::runtime_handoff::constitution_display(engine.session.messages.last().unwrap())
            .unwrap()
            .contains("withdrawn")
    );
    drainer.abort();
}

const END_OF_EVENTS: &str = "profile constitution test: end of events";

/// Every event delivered before the `END_OF_EVENTS` status, in order. The
/// sentinel keeps the receiver drained during a turn without racing the turn.
fn collect_events(handle: EngineHandle) -> tokio::task::JoinHandle<Vec<Event>> {
    tokio::spawn(async move {
        let mut events = Vec::new();
        let mut receiver = handle.rx_event.write().await;
        while let Some(event) = receiver.recv().await {
            if matches!(&event, Event::Status { message } if message == END_OF_EVENTS) {
                break;
            }
            events.push(event);
        }
        events
    })
}

fn fallback_notice(engine: &Engine) -> String {
    codewhale_localization::tr(
        codewhale_localization::resolve_locale(&engine.config.locale_tag),
        codewhale_localization::MessageId::ProfileConstitutionUnavailableLocal,
    )
    .to_string()
}

fn save_local_constitution() {
    codewhale_config::UserConstitution {
        about: Some("Local fallback preference.".to_string()),
        ..Default::default()
    }
    .save()
    .unwrap();
}

#[tokio::test]
async fn engine_loaded_profile_failure_uses_the_local_constitution_and_says_so_once() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    let _home = crate::test_support::SealedHome::new();
    save_local_constitution();
    let workspace = tempfile::tempdir().unwrap();
    let config = Config::default();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("Done"),
        canned::simple_text_turn("Done again"),
    ]));
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        mock.clone(),
    );
    let events = collect_events(handle);
    let _unavailable = crate::profile_constitution::fail_account_load_for_test(
        "Your profile could not be loaded (HTTP 401). Sign in again or retry",
    );
    for _ in 0..2 {
        let Op::SendMessage(spec) = external_user_message_op("Say hello", AppMode::Agent, &config)
        else {
            unreachable!()
        };
        let outcome = engine.handle_send_message(spec).await;
        assert!(!matches!(outcome, SendMessageOutcome::NotStarted { .. }));
        let requests = mock.captured_requests();
        let request = serde_json::to_string(requests.last().unwrap()).unwrap();
        assert!(request.contains("Local fallback preference."));
        assert!(!request.contains("Account profile constitution,"));
        assert!(
            engine
                .constitution_block
                .as_deref()
                .is_some_and(|block| block.contains("Local fallback preference."))
        );
    }
    assert!(engine.profile_constitution_fallback_noticed);
    let notice = fallback_notice(&engine);
    engine
        .send_event(Event::status(END_OF_EVENTS))
        .await
        .unwrap();
    let events = events.await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Status { message } if *message == notice))
            .count(),
        1,
        "the fallback is announced once per engine, not once per turn"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Error { envelope, .. }
            if envelope.code == "profile_constitution_unavailable"))
    );
}

#[tokio::test]
async fn host_supplied_invalid_profile_refuses_the_turn_without_a_local_fallback() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    let _home = crate::test_support::SealedHome::new();
    save_local_constitution();
    let workspace = tempfile::tempdir().unwrap();
    let config = Config::default();
    let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "must not run",
    )]));
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        mock.clone(),
    );
    let events = collect_events(handle);
    // An unavailable account service must not turn a host's own snapshot into
    // a local-fallback turn either.
    let _unavailable =
        crate::profile_constitution::fail_account_load_for_test("account service unreachable");
    let Op::SendMessage(mut spec) = external_user_message_op("Say hello", AppMode::Agent, &config)
    else {
        unreachable!()
    };
    spec.profile_constitution = Some(ProfileConstitutionSnapshot {
        account_id: "acct_fixture".into(),
        revision: 1,
        constitution: ProfileConstitution {
            notes: "x".repeat(codewhale_config::user_constitution::MAX_NOTES_LEN + 1),
            ..ProfileConstitution::default()
        },
    });
    let outcome = engine.handle_send_message(spec).await;
    let SendMessageOutcome::NotStarted { error: Some(error) } = outcome else {
        panic!("an invalid host-supplied profile must refuse its turn");
    };
    assert!(error.contains("Constitution notes cannot exceed"));
    assert_eq!(mock.call_count(), 0);
    assert!(!engine.profile_constitution_fallback_noticed);
    assert!(engine.constitution_block.is_none());
    let notice = fallback_notice(&engine);
    engine
        .send_event(Event::status(END_OF_EVENTS))
        .await
        .unwrap();
    let events = events.await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Error { envelope, .. }
                if envelope.code == "profile_constitution_unavailable"))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::TurnStarted { .. } | Event::TurnComplete { .. }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Status { message } if *message == notice))
    );
}
