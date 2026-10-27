use codewhale_protocol::{
    AppRequest, EventFrame, ThreadGoal, ThreadGoalProgressParams, ThreadGoalSetParams,
    ThreadGoalStatus, ThreadListParams, ThreadRequest, ThreadResumeParams, ToolOutput,
    UserInputAnswerEvent, UserInputOptionEvent, UserInputQuestionEvent, UserInputRequestEvent,
    runtime::{RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION, RuntimeEventEnvelope},
};
use serde_json::{Value, json};

#[test]
fn mcp_tool_output_public_shape_remains_source_compatible() {
    let output = ToolOutput::Mcp {
        result: json!({"content": []}),
    };
    let ToolOutput::Mcp { result } = output else {
        panic!("constructed MCP output changed variant")
    };
    assert_eq!(result, json!({"content": []}));
}

#[test]
fn tool_output_success_accessor_has_function_and_mcp_parity() {
    let cases = [
        (
            ToolOutput::Function {
                body: Some(json!({"kind": "function-success"})),
                success: true,
            },
            true,
        ),
        (
            ToolOutput::Function {
                body: Some(json!({"kind": "function-failure"})),
                success: false,
            },
            false,
        ),
        (
            ToolOutput::Mcp {
                result: json!({"kind": "mcp-success", "isError": false}),
            },
            true,
        ),
        (
            ToolOutput::Mcp {
                result: json!({"kind": "mcp-failure", "isError": true}),
            },
            false,
        ),
    ];

    assert_eq!(
        serde_json::to_string(&cases[0].0).expect("serialize successful function output"),
        r#"{"type":"function","body":{"kind":"function-success"},"success":true}"#,
        "successful Function output must retain its existing wire shape"
    );
    for (output, expected) in cases {
        assert_eq!(output.success(), expected, "output: {output:?}");
    }
}

#[test]
fn mcp_is_error_round_trips_and_legacy_bytes_remain_successful() {
    let failed = ToolOutput::Mcp {
        result: json!({"message": "application failure", "isError": true}),
    };
    let encoded = serde_json::to_string(&failed).expect("serialize failed MCP output");
    assert!(encoded.contains(r#""isError":true"#));
    let decoded: ToolOutput = serde_json::from_str(&encoded).expect("round-trip MCP output");
    assert!(!decoded.success());

    let legacy = r#"{"type":"mcp","result":{"message":"legacy success"}}"#;
    let decoded: ToolOutput = serde_json::from_str(legacy).expect("deserialize legacy MCP output");
    assert!(decoded.success());
    assert_eq!(
        serde_json::to_string(&decoded).expect("re-serialize legacy MCP output"),
        legacy,
        "success=true must preserve the legacy MCP wire representation"
    );
}

#[test]
fn mcp_is_error_metadata_fails_closed_when_present_but_not_boolean() {
    for malformed in [
        Value::Null,
        json!("unknown"),
        json!(1),
        json!({"unexpected": true}),
        json!([false]),
    ] {
        let output = ToolOutput::Mcp {
            result: json!({"content": [], "isError": malformed}),
        };
        assert!(!output.success(), "malformed output: {output:?}");
    }
}

#[test]
fn thread_resume_params_round_trip() {
    let request = ThreadRequest::Resume(ThreadResumeParams {
        operation_key: None,
        thread_id: "thread-123".to_string(),
        history: None,
        path: None,
        model: Some("deepseek-v4-pro".to_string()),
        model_provider: Some("deepseek".to_string()),
        cwd: None,
        approval_policy: Some("on-request".to_string()),
        sandbox: Some("workspace-write".to_string()),
        config: None,
        base_instructions: Some("base".to_string()),
        developer_instructions: Some("dev".to_string()),
        personality: Some("default".to_string()),
        persist_extended_history: true,
    });

    let encoded = serde_json::to_string(&request).expect("serialize request");
    let decoded: ThreadRequest = serde_json::from_str(&encoded).expect("deserialize request");
    match decoded {
        ThreadRequest::Resume(params) => {
            assert_eq!(params.thread_id, "thread-123");
            assert_eq!(params.model.as_deref(), Some("deepseek-v4-pro"));
            assert!(params.persist_extended_history);
        }
        other => panic!("unexpected request: {other:?}"),
    }
}

#[test]
fn thread_list_params_defaults_are_serializable() {
    let request = ThreadRequest::List(ThreadListParams {
        include_archived: false,
        limit: Some(20),
    });
    let encoded = serde_json::to_string_pretty(&request).expect("serialize list request");
    assert!(encoded.contains("include_archived"));
}

#[test]
fn event_frame_serialization_contains_expected_tag() {
    let frame = EventFrame::TurnComplete {
        turn_id: "turn-1".to_string(),
    };
    let encoded = serde_json::to_string(&frame).expect("serialize frame");
    assert!(encoded.contains("turn_complete"));
}

#[test]
fn thread_goal_set_request_round_trip() {
    let request = ThreadRequest::GoalSet(ThreadGoalSetParams {
        thread_id: "thread-123".to_string(),
        objective: "Release 0.8.59".to_string(),
        token_budget: Some(42_000),
    });

    let encoded = serde_json::to_string(&request).expect("serialize goal request");
    assert!(encoded.contains("goal_set"));
    let decoded: ThreadRequest = serde_json::from_str(&encoded).expect("deserialize request");
    match decoded {
        ThreadRequest::GoalSet(params) => {
            assert_eq!(params.thread_id, "thread-123");
            assert_eq!(params.objective, "Release 0.8.59");
            assert_eq!(params.token_budget, Some(42_000));
        }
        other => panic!("unexpected request: {other:?}"),
    }
}

#[test]
fn thread_goal_event_serializes_status_and_accounting() {
    let goal = ThreadGoal {
        thread_id: "thread-123".to_string(),
        goal_id: "goal-1".to_string(),
        objective: "Release 0.8.59".to_string(),
        status: ThreadGoalStatus::BudgetLimited,
        token_budget: Some(42_000),
        tokens_used: 42_001,
        time_used_seconds: 3600,
        continuation_count: 7,
        last_gap_fingerprint: None,
        repeated_gap_count: 0,
        last_gap_pass: None,
        pause_reason: None,
        created_at: 1,
        updated_at: 2,
    };

    let frame = EventFrame::ThreadGoalUpdated { goal };
    let encoded = serde_json::to_value(&frame).expect("serialize goal event");
    assert_eq!(encoded["event"], "thread_goal_updated");
    assert_eq!(encoded["goal"]["status"], "budget_limited");
    assert_eq!(encoded["goal"]["tokens_used"], 42_001);
    assert_eq!(encoded["goal"]["continuation_count"], 7);
}

#[test]
fn thread_goal_progress_request_round_trip() {
    let request = ThreadRequest::GoalRecordProgress(ThreadGoalProgressParams {
        thread_id: "thread-123".to_string(),
        token_delta: 750,
        time_delta_seconds: 9,
        record_continuation: true,
    });

    let encoded = serde_json::to_string(&request).expect("serialize goal progress request");
    assert!(encoded.contains("goal_record_progress"));
    let decoded: ThreadRequest = serde_json::from_str(&encoded).expect("deserialize request");
    match decoded {
        ThreadRequest::GoalRecordProgress(params) => {
            assert_eq!(params.thread_id, "thread-123");
            assert_eq!(params.token_delta, 750);
            assert_eq!(params.time_delta_seconds, 9);
            assert!(params.record_continuation);
        }
        other => panic!("unexpected request: {other:?}"),
    }
}

#[test]
fn runtime_event_envelope_roundtrip() {
    let input = json!({
        "schema_version": 1,
        "seq": 12,
        "event": "item.delta",
        "kind": "item.delta",
        "thread_id": "thr_123",
        "turn_id": "turn_456",
        "item_id": "item_789",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "created_at": "2026-02-11T20:18:49.123Z",
        "payload": { "delta": "ok", "kind": "agent_message" },
    });
    let envelope: RuntimeEventEnvelope =
        serde_json::from_value(input).expect("deserialize runtime event envelope");
    assert_eq!(envelope.schema_version, 1);
    assert_eq!(envelope.seq, 12);
    assert_eq!(envelope.event, "item.delta");
    assert_eq!(envelope.kind, "item.delta");
    assert_eq!(envelope.thread_id, "thr_123");

    let encoded = serde_json::to_value(&envelope).expect("serialize runtime event envelope");
    assert_eq!(encoded["event"], encoded["kind"]);
    assert_eq!(encoded["schema_version"], 1);
    assert_eq!(encoded["seq"], 12);
    assert_eq!(encoded["thread_id"], "thr_123");
    assert_eq!(encoded["turn_id"], "turn_456");
    assert_eq!(encoded["item_id"], "item_789");
    assert_eq!(encoded["timestamp"], "2026-02-11T20:18:49.123Z");
    assert_eq!(encoded["created_at"], "2026-02-11T20:18:49.123Z");
    assert_eq!(
        encoded["payload"],
        json!({ "delta": "ok", "kind": "agent_message" })
    );
}

#[test]
fn runtime_event_envelope_defaults_to_api_schema_version() {
    let input = json!({
        "seq": 15,
        "event": "thread.started",
        "kind": "thread.started",
        "thread_id": "thr_default_version",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": {},
    });
    let envelope: RuntimeEventEnvelope = serde_json::from_value(input)
        .expect("deserialize runtime event envelope without schema version");

    assert_eq!(
        envelope.schema_version,
        RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION
    );
}

#[test]
fn runtime_event_envelope_thread_level_keeps_turn_and_item_ids() {
    let input = json!({
        "schema_version": 1,
        "seq": 14,
        "event": "thread.started",
        "kind": "thread.started",
        "thread_id": "thr_thread",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": { "thread": { "id": "thr_thread" } },
    });
    let envelope: RuntimeEventEnvelope = serde_json::from_value(input)
        .expect("deserialize runtime event envelope without thread-level turn/item ids");
    assert!(envelope.turn_id.is_none());
    assert!(envelope.item_id.is_none());

    let encoded = serde_json::to_value(envelope).expect("serialize runtime event envelope");
    assert!(encoded.get("turn_id").is_some());
    assert!(encoded.get("item_id").is_some());
    assert!(encoded["turn_id"].is_null());
    assert!(encoded["item_id"].is_null());
}

#[test]
fn runtime_event_envelope_preserves_unknown_fields() {
    let input: Value = json!({
        "schema_version": 1,
        "seq": 13,
        "event": "turn.completed",
        "kind": "turn.completed",
        "thread_id": "thr_unknown",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": {},
        "forward_compatibility_hint": "v2-ready",
    });
    let envelope: RuntimeEventEnvelope = serde_json::from_value(input.clone())
        .expect("deserialize runtime event envelope with unknown field");
    assert!(envelope.extra.contains_key("forward_compatibility_hint"));

    let encoded = serde_json::to_value(envelope).expect("serialize runtime event envelope");
    assert_eq!(encoded["forward_compatibility_hint"], "v2-ready");
    assert_eq!(encoded["schema_version"], 1);
    assert_eq!(encoded["seq"], 13);
    assert_eq!(encoded["event"], "turn.completed");
    assert_eq!(encoded["kind"], "turn.completed");
    assert_eq!(encoded["thread_id"], "thr_unknown");
    assert!(encoded["turn_id"].is_null());
    assert!(encoded["item_id"].is_null());
}

#[test]
fn user_input_request_event_frame_round_trip() {
    // issue #3102: the new EventFrame::UserInputRequest variant must tag as
    // "user_input_request" and round-trip the full nested question schema,
    // including the allow_free_text / multi_select booleans.
    let frame = EventFrame::UserInputRequest {
        request: UserInputRequestEvent {
            call_id: "call-1".to_string(),
            turn_id: "turn-1".to_string(),
            request_id: "ui-1".to_string(),
            require_typed_answers: true,
            questions: vec![UserInputQuestionEvent {
                header: "Scope".to_string(),
                id: "scope".to_string(),
                question: "Which surfaces?".to_string(),
                detail: Some("Detailed plan".to_string()),
                intent: Some(codewhale_protocol::UserInputIntentEvent::PlanReview {
                    approve: "All".into(),
                    call_id: Some("plan-call".into()),
                }),
                options: vec![
                    UserInputOptionEvent {
                        label: "TUI".to_string(),
                        description: "Modal flow".to_string(),
                    },
                    UserInputOptionEvent {
                        label: "All".to_string(),
                        description: "TUI + headless".to_string(),
                    },
                ],
                allow_free_text: true,
                multi_select: true,
            }],
        },
    };

    let encoded = serde_json::to_value(&frame).expect("serialize user input frame");
    assert_eq!(encoded["event"], "user_input_request");
    assert_eq!(encoded["request"]["call_id"], "call-1");
    assert_eq!(encoded["request"]["require_typed_answers"], true);
    assert_eq!(
        encoded["request"]["questions"][0]["detail"],
        "Detailed plan"
    );
    assert_eq!(
        encoded["request"]["questions"][0]["intent"]["kind"],
        "plan-review"
    );
    assert_eq!(
        encoded["request"]["questions"][0]["intent"]["call_id"],
        "plan-call"
    );
    assert_eq!(encoded["request"]["request_id"], "ui-1");
    assert_eq!(encoded["request"]["questions"][0]["header"], "Scope");
    assert_eq!(encoded["request"]["questions"][0]["allow_free_text"], true);
    assert_eq!(encoded["request"]["questions"][0]["multi_select"], true);
    assert_eq!(
        encoded["request"]["questions"][0]["options"][0]["label"],
        "TUI"
    );

    // Round-trips back through serde.
    let decoded: EventFrame =
        serde_json::from_value(encoded).expect("deserialize user input frame");
    let EventFrame::UserInputRequest { request } = decoded else {
        panic!("expected user_input_request frame after round-trip");
    };
    assert_eq!(request.request_id, "ui-1");
    assert_eq!(request.questions.len(), 1);
    assert!(request.questions[0].allow_free_text);
    assert!(request.questions[0].multi_select);
}

#[test]
fn user_input_request_event_defaults_flags_when_omitted() {
    // Backwards compatibility: omitting allow_free_text/multi_select in the
    // wire JSON must deserialize both to false (matching the TUI's leniency).
    let input = json!({
        "event": "user_input_request",
        "request": {
            "call_id": "c",
            "turn_id": "t",
            "request_id": "r",
            "questions": [{
                "header": "H",
                "id": "i",
                "question": "Q?",
                "options": [
                    { "label": "A", "description": "a" },
                    { "label": "B", "description": "b" }
                ]
            }]
        }
    });
    let decoded: EventFrame = serde_json::from_value(input).expect("deserialize without flags");
    let EventFrame::UserInputRequest { request } = decoded else {
        panic!("expected user_input_request frame");
    };
    assert!(!request.require_typed_answers);
    assert_eq!(request.questions[0].detail, None);
    assert_eq!(request.questions[0].intent, None);
    assert!(!request.questions[0].allow_free_text);
    assert!(!request.questions[0].multi_select);
}

#[test]
fn submit_user_input_app_request_round_trip() {
    // issue #3102: the headless client→server reply variant must tag as
    // "submit_user_input" and carry the answer list.
    let req = AppRequest::SubmitUserInput {
        request_id: "ui-1".to_string(),
        answers: vec![UserInputAnswerEvent {
            kind: Some(codewhale_protocol::UserInputAnswerKindEvent::Choice),
            id: "scope".to_string(),
            label: "All".to_string(),
            value: "All".to_string(),
        }],
    };
    let encoded = serde_json::to_string(&req).expect("serialize submit request");
    assert!(encoded.contains("submit_user_input"));
    assert!(encoded.contains("\"request_id\":\"ui-1\""));

    let decoded: AppRequest = serde_json::from_str(&encoded).expect("deserialize submit request");
    let AppRequest::SubmitUserInput {
        request_id,
        answers,
    } = decoded
    else {
        panic!("expected submit_user_input after round-trip");
    };
    assert_eq!(request_id, "ui-1");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].label, "All");
    assert_eq!(
        answers[0].kind,
        Some(codewhale_protocol::UserInputAnswerKindEvent::Choice)
    );
}

#[test]
fn retained_thread_operation_keys_are_additive_and_survive_explicit_retry() {
    for (method, params) in [
        ("start", json!({})),
        ("resume", json!({"thread_id":"thread-123"})),
        ("fork", json!({"thread_id":"thread-123"})),
    ] {
        let legacy = json!({"type":method,"params":params});
        // Use the enum's actual serde tag rather than a second fixture protocol.
        let mut wire = match method {
            "start" => serde_json::to_value(ThreadRequest::Start(
                serde_json::from_value(json!({})).unwrap(),
            ))
            .unwrap(),
            "resume" => serde_json::to_value(ThreadRequest::Resume(
                serde_json::from_value(legacy["params"].clone()).unwrap(),
            ))
            .unwrap(),
            _ => serde_json::to_value(ThreadRequest::Fork(
                serde_json::from_value(legacy["params"].clone()).unwrap(),
            ))
            .unwrap(),
        };
        assert!(wire.get("operation_key").is_none());
        wire["operation_key"] = json!("captured-client-intent");
        let decoded: ThreadRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["operation_key"],
            "captured-client-intent"
        );
    }
}
