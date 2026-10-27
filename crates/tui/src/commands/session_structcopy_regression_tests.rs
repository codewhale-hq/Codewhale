//! Original real-host structcopy assertions relocated unchanged across the new boundary.
use super::contract::structcopy_host::observe_path_roots;
use super::groups::session::structcopy::{self, *};
use crate::commands::CommandResult;
use crate::tools::plan::{PlanItemArg, StepStatus, UpdatePlanArgs};
use crate::tui::app::App;
use crate::tui::app::TuiOptions;
use crate::tui::clipboard::ClipboardHandler;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message};
use codewhale_models::{ImageUrlContent, ToolCaller};
use codewhale_runtime::config::Config;
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;

fn test_app(tmpdir: &TempDir) -> App {
    let options = TuiOptions {
        skills_dir: tmpdir.path().join("skills"),
        memory_path: tmpdir.path().join("memory.md"),
        notes_path: tmpdir.path().join("notes.txt"),
        mcp_config_path: tmpdir.path().join("mcp.json"),
        ..crate::tui::test_support::test_tui_options(tmpdir.path())
    };
    let mut app = App::new(options, &Config::default());
    app.ui_locale = Locale::En;
    app
}

fn stdout_json(result: &CommandResult) -> String {
    assert!(!result.is_error, "{:?}", result.message);
    result.message.clone().expect("stdout payload")
}

fn parsed(json: &str) -> Value {
    serde_json::from_str(json).expect("structcopy output must be valid JSON")
}

fn seed_transcript(app: &mut App) {
    app.api_messages = std::sync::Arc::new(vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "please run the fetch".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "private chain of thought".to_string(),
                    signature: Some("signature-secret".to_string()),
                    state: None,
                    redacted_data: None,
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-7".to_string(),
                    name: "fetch_url".to_string(),
                    input: json!({
                        "url": "https://alice:hunter2@example.com/path?token=abc123&ok=1#frag",
                        "api_key": "literal-api-secret",
                    }),
                    caller: Some(ToolCaller {
                        caller_type: "code_execution_20250825".to_string(),
                        tool_id: None,
                    }),
                    thought_signature: None,
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-7".to_string(),
                content: "Authorization: Bearer result-secret-token\nfetch ok".to_string(),
                is_error: Some(false),
                content_blocks: None,
            }],
        },
    ]);
}

#[test]
fn turn_copy_projects_one_item_and_redacts() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);

    let json = stdout_json(&execute_structcopy(&mut app, Some("turn 2 stdout")));
    let value = parsed(&json);
    assert_eq!(value["receipt"]["schema"], json!(SCHEMA_ID));
    assert_eq!(value["receipt"]["kind"], json!("turn"));
    assert_eq!(value["receipt"]["selector"], json!(2));
    assert_eq!(value["object"]["role"], json!("assistant"));
    let content = value["object"]["content"].as_array().expect("content");
    assert_eq!(content[0]["type"], json!("thinking"));
    assert!(content[0].get("thinking").is_none());
    assert_eq!(
        content[0]["omission_code"],
        json!("internal_reasoning_and_signature")
    );
    assert_eq!(content[1]["type"], json!("tool_use"));
    assert_eq!(content[1]["caller_type"], json!("code_execution_20250825"));
    for forbidden in [
        "private chain of thought",
        "signature-secret",
        "literal-api-secret",
        "hunter2",
        "abc123",
        "frag",
    ] {
        assert!(!json.contains(forbidden), "leaked {forbidden:?}: {json}");
    }
    // URL userinfo/query/fragment are stripped outright.
    assert!(json.contains("https://example.com/path"), "{json}");
    assert!(json.contains(SENSITIVE_VALUE_REDACTION_MARKER), "{json}");
    for prose in [
        "internal context omitted",
        "internal reasoning and signature omitted",
        "inline or local image payload omitted",
        "[redacted private key]",
        "Bearer [redacted]",
        "[redacted token]",
        "[redacted]",
    ] {
        assert!(!json.contains(prose), "prose marker {prose:?}: {json}");
    }
}

#[test]
fn generated_omissions_are_language_neutral_codes() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.api_messages = std::sync::Arc::new(vec![
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text {
                text: "must not be copied".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/png;base64,private".to_string(),
                },
            }],
        },
    ]);

    let internal = parsed(&stdout_json(&execute_structcopy(
        &mut app,
        Some("turn 1 stdout"),
    )));
    assert_eq!(
        internal["object"]["omission_code"],
        json!("internal_context")
    );
    assert!(internal["object"].get("omitted").is_none());

    let image = parsed(&stdout_json(&execute_structcopy(
        &mut app,
        Some("turn 2 stdout"),
    )));
    assert_eq!(
        image["object"]["content"][0]["omission_code"],
        json!("inline_or_local_image_payload")
    );
    assert!(image["object"]["content"][0].get("omitted").is_none());

    let english = stdout_json(&execute_structcopy(&mut app, Some("turn 2 stdout")));
    app.ui_locale = Locale::ZhHans;
    let chinese_ui = stdout_json(&execute_structcopy(&mut app, Some("turn 2 stdout")));
    assert_eq!(
        english, chinese_ui,
        "machine payload must not vary with the UI locale"
    );
}

#[test]
fn tool_copy_pairs_call_and_result() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);

    let json = stdout_json(&execute_structcopy(&mut app, Some("tool call-7 stdout")));
    let value = parsed(&json);
    assert_eq!(value["receipt"]["kind"], json!("tool"));
    assert_eq!(value["receipt"]["selector"], json!("call-7"));
    assert_eq!(value["object"]["name"], json!("fetch_url"));
    assert_eq!(value["object"]["result"]["found"], json!(true));
    assert_eq!(value["object"]["result"]["is_error"], json!(false));
    assert!(!json.contains("result-secret-token"), "{json}");

    // A call without a result is honest, not fabricated.
    app.api_messages_mut()[1]
        .content
        .push(ContentBlock::ToolUse {
            execution_id: None,
            id: "call-lonely".to_string(),
            name: "view_image".to_string(),
            input: json!({}),
            caller: None,
            thought_signature: None,
        });
    let json = stdout_json(&execute_structcopy(
        &mut app,
        Some("tool call-lonely stdout"),
    ));
    let value = parsed(&json);
    assert_eq!(value["object"]["result"]["found"], json!(false));
}

/// An unknown `Option<bool>` must serialize as JSON `null`. Collapsing it
/// to `false` would assert an outcome nothing observed.
#[test]
fn unknown_optional_booleans_stay_null_and_are_not_dropped() {
    assert_eq!(optional_bool(None), Value::Null);
    assert_eq!(optional_bool(Some(false)), Value::Bool(false));
    assert_eq!(optional_bool(Some(true)), Value::Bool(true));

    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.api_messages = std::sync::Arc::new(vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call-unknown".to_string(),
                name: "exec_command".to_string(),
                input: json!({}),
                // No caller recorded: also an unknown, also null.
                caller: None,
                thought_signature: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-unknown".to_string(),
                content: "no error flag was recorded".to_string(),
                is_error: None,
                content_blocks: None,
            }],
        },
    ]);

    // Tool-pair projection.
    let json = stdout_json(&execute_structcopy(
        &mut app,
        Some("tool call-unknown stdout"),
    ));
    let value = parsed(&json);
    let result = value["object"]["result"].as_object().expect("result");
    assert!(
        result.contains_key("is_error"),
        "the unknown flag must be present, not dropped: {json}"
    );
    assert_eq!(result["is_error"], Value::Null);
    assert_ne!(result["is_error"], json!(false));

    // Turn projection of the same result block, plus the unknown caller.
    let json = stdout_json(&execute_structcopy(&mut app, Some("turn 2 stdout")));
    let value = parsed(&json);
    let block = &value["object"]["content"][0];
    assert!(
        block.as_object().expect("block").contains_key("is_error"),
        "{json}"
    );
    assert_eq!(block["is_error"], Value::Null);

    let json = stdout_json(&execute_structcopy(&mut app, Some("turn 1 stdout")));
    let value = parsed(&json);
    let block = &value["object"]["content"][0];
    assert!(
        block
            .as_object()
            .expect("block")
            .contains_key("caller_type"),
        "{json}"
    );
    assert_eq!(block["caller_type"], Value::Null);
}

#[test]
fn plan_copy_snapshots_current_plan() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    {
        let mut state = app.plan_state.try_lock().expect("plan lock");
        state.update(UpdatePlanArgs {
            title: Some("Ship structcopy".to_string()),
            plan: vec![
                PlanItemArg {
                    step: "Read seams".to_string(),
                    status: StepStatus::Completed,
                },
                PlanItemArg {
                    step: "Copy exactly one object".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
            ..Default::default()
        });
    }

    let json = stdout_json(&execute_structcopy(&mut app, Some("plan stdout")));
    let value = parsed(&json);
    assert_eq!(value["receipt"]["kind"], json!("plan"));
    assert_eq!(value["object"]["title"], json!("Ship structcopy"));
    let items = value["object"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[1]["status"], json!("in_progress"));
}

#[test]
fn workflow_copy_projects_existing_run_without_side_effects() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.current_session_id = Some("structcopy-workflow-test-session".to_string());

    // Unknown run, no state: honest error, and the read must not create
    // the workflow journal on disk.
    let missing = execute_structcopy(&mut app, Some("workflow nope stdout"));
    assert!(missing.is_error);
    assert!(
        missing
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"),
        "{:?}",
        missing.message
    );
    assert!(
        !tmpdir.path().join(".codewhale").exists(),
        "read-only copy must not create the workflow journal"
    );

    crate::tools::workflow::structcopy_test_seed_run(
        tmpdir.path(),
        "structcopy-test-run-alpha",
        app.current_session_id
            .as_deref()
            .expect("test session identity"),
    );
    let json = stdout_json(&execute_structcopy(
        &mut app,
        Some("workflow structcopy-test-run-alpha stdout"),
    ));
    let value = parsed(&json);
    assert_eq!(value["receipt"]["kind"], json!("workflow"));
    assert_eq!(
        value["object"]["run_id"],
        json!("structcopy-test-run-alpha")
    );
    assert_eq!(value["object"]["status"], json!("running"));
    assert_eq!(value["object"]["leaf_count"], Value::Null);
    assert_eq!(value["object"]["branch_count"], Value::Null);
    assert_eq!(value["object"]["control_count"], Value::Null);
    assert!(
        value["object"].get("source_path").is_none(),
        "filesystem paths must not leave the projection: {json}"
    );

    let unknown = execute_structcopy(&mut app, Some("workflow nope stdout"));
    assert!(unknown.is_error);
    let message = unknown.message.as_deref().unwrap_or_default();
    assert!(message.contains("unavailable"), "{message}");
    assert!(
        !message.contains("structcopy-test-run-alpha"),
        "unavailable errors must not enumerate private run ids: {message}"
    );
}

#[test]
fn unavailable_selectors_are_reported_not_fabricated() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);

    let empty_turn = execute_structcopy(&mut app, Some("turn 1 stdout"));
    assert!(empty_turn.is_error);
    assert!(
        empty_turn
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"),
        "{:?}",
        empty_turn.message
    );

    let empty_plan = execute_structcopy(&mut app, Some("plan stdout"));
    assert!(empty_plan.is_error);
    assert!(
        empty_plan
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"),
        "{:?}",
        empty_plan.message
    );

    seed_transcript(&mut app);
    let out_of_range = execute_structcopy(&mut app, Some("turn 99 stdout"));
    assert!(out_of_range.is_error);
    assert!(
        out_of_range
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"),
        "{:?}",
        out_of_range.message
    );

    let missing_tool = execute_structcopy(&mut app, Some("tool call-nope stdout"));
    assert!(missing_tool.is_error);
    assert!(
        missing_tool
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"),
        "{:?}",
        missing_tool.message
    );

    for bad in [
        None,
        Some(""),
        Some("turn 0"),
        Some("turn x"),
        Some("turn -1"),
        Some("turn 99999999999999999999999999"),
        Some("plan extra"),
        Some("tool"),
        Some("workflow"),
        Some("stdout"),
        Some("   "),
    ] {
        let result = execute_structcopy(&mut app, bad);
        assert!(result.is_error, "{bad:?}: {:?}", result.message);
    }
}

#[test]
fn command_feedback_uses_the_active_locale() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.ui_locale = Locale::ZhHans;

    let invalid = execute_structcopy(&mut app, Some("unknown"));
    assert!(invalid.is_error);
    let expected = tr(Locale::ZhHans, MessageId::CmdStructcopyUsageError)
        .replace("{usage}", COMMAND_INFO.usage);
    assert!(
        invalid
            .message
            .as_deref()
            .is_some_and(|message| message.ends_with(&expected)),
        "{:?}",
        invalid.message
    );

    let unavailable = execute_structcopy(&mut app, Some("plan stdout"));
    assert!(unavailable.is_error);
    let expected = tr(Locale::ZhHans, MessageId::CmdStructcopyUnavailable).replace(
        "{kind}",
        &tr(Locale::ZhHans, MessageId::CmdStructcopyKindPlan),
    );
    assert!(
        unavailable
            .message
            .as_deref()
            .is_some_and(|message| message.ends_with(&expected)),
        "{:?}",
        unavailable.message
    );
}

/// An unavailable selector is never echoed. An available selector is
/// scrubbed and bounded in both the receipt and copied object.
#[test]
fn hostile_selectors_are_redacted_and_bounded_everywhere() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    let workspace = tmpdir.path().to_string_lossy().into_owned();
    seed_transcript(&mut app);

    // Unavailable selector: no attacker-influenced bytes are echoed.
    let hostile = format!(
        "\u{1b}[31mred\u{1b}[0m-Bearer-abcdef1234567890-https://u:p@evil.test/x?k=v#f-{workspace}-{}",
        "A".repeat(4096)
    );
    let result = execute_structcopy(&mut app, Some(&format!("tool {hostile} stdout")));
    assert!(result.is_error);
    let message = result.message.as_deref().unwrap_or_default();
    assert!(message.len() < 400, "status message unbounded: {message}");
    for forbidden in [
        "\u{1b}[31m",
        "abcdef1234567890",
        "u:p@evil.test",
        "k=v",
        workspace.as_str(),
    ] {
        assert!(
            !message.contains(forbidden),
            "leaked {forbidden:?}: {message}"
        );
    }
    assert!(!message.contains('\n'), "status label must be one line");
    assert!(message.contains("unavailable"), "{message}");

    // Receipt path: a long but *available* selector is bounded too.
    let long_id = format!("call-{}", "z".repeat(4096));
    app.api_messages_mut()[1]
        .content
        .push(ContentBlock::ToolUse {
            execution_id: None,
            id: long_id.clone(),
            name: "exec_command".to_string(),
            input: json!({}),
            caller: None,
            thought_signature: None,
        });
    let json = stdout_json(&execute_structcopy(
        &mut app,
        Some(&format!("tool {long_id} stdout")),
    ));
    let value = parsed(&json);
    let selector = value["receipt"]["selector"].as_str().expect("selector");
    assert!(
        selector.len() <= MAX_SELECTOR_BYTES,
        "selector {} bytes exceeds the {MAX_SELECTOR_BYTES}-byte cap",
        selector.len()
    );
    assert!(selector.ends_with('…'), "{selector}");

    // Composer selectors cannot contain a whitespace-delimited `Bearer`
    // header, so delimiter-shaped bearer tokens are scrubbed too.
    for bearer_id in [
        "call-Bearer-abcdef1234567890",
        "call-Bearer=zyxwvutsrqponmlk",
    ] {
        app.api_messages_mut()[1]
            .content
            .push(ContentBlock::ToolUse {
                execution_id: None,
                id: bearer_id.to_string(),
                name: "exec_command".to_string(),
                input: json!({}),
                caller: None,
                thought_signature: None,
            });
        let json = stdout_json(&execute_structcopy(
            &mut app,
            Some(&format!("tool {bearer_id} stdout")),
        ));
        assert!(!json.contains("abcdef1234567890"), "{json}");
        assert!(!json.contains("zyxwvutsrqponmlk"), "{json}");
        assert!(json.contains(BEARER_REDACTION_MARKER), "{json}");
    }
}

/// Object keys are attacker-influenced too (a model can name a tool-input
/// field anything). Keys must be sanitized, bounded, and de-collided
/// deterministically without dropping a value.
#[test]
fn hostile_object_keys_are_scrubbed_bounded_and_deduped_deterministically() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    let workspace = tmpdir.path().to_string_lossy().into_owned();

    // Three keys that collapse onto the same bounded form, one key with
    // ANSI + newlines, and one key carrying a workspace path.
    let long_a = format!("k{}A", "x".repeat(MAX_KEY_BYTES));
    let long_b = format!("k{}B", "x".repeat(MAX_KEY_BYTES));
    let long_c = format!("k{}C", "x".repeat(MAX_KEY_BYTES));
    let input = json!({
        long_a.clone(): 1,
        long_b.clone(): 2,
        long_c.clone(): 3,
        // Not a credential-shaped name: a key ending in `key` is redacted by
        // the shared vocabulary, which other tests cover.
        "\u{1b}[31mansi\u{1b}[0m\nlabel": 4,
        format!("at {workspace}/src"): 5,
    });
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-keys".to_string(),
            name: "exec_command".to_string(),
            input,
            caller: None,
            thought_signature: None,
        }],
    }]);

    let first = stdout_json(&execute_structcopy(&mut app, Some("tool call-keys stdout")));
    let second = stdout_json(&execute_structcopy(&mut app, Some("tool call-keys stdout")));
    assert_eq!(
        first, second,
        "key collision handling must be deterministic"
    );

    let value = parsed(&first);
    let object = value["object"]["input"].as_object().expect("input");
    // No value is lost to a collision.
    assert_eq!(object.len(), 5, "{object:?}");
    let mut values: Vec<u64> = object
        .values()
        .map(|item| item.as_u64().expect("number"))
        .collect();
    values.sort_unstable();
    assert_eq!(values, vec![1, 2, 3, 4, 5]);

    for key in object.keys() {
        assert!(
            key.len() <= MAX_KEY_BYTES,
            "key {} bytes exceeds the {MAX_KEY_BYTES}-byte cap",
            key.len()
        );
        assert!(!key.contains('\u{1b}'), "ANSI survived in key {key:?}");
        assert!(!key.contains('\n'), "newline survived in key {key:?}");
        assert!(!key.contains(&workspace), "workspace path in key {key:?}");
    }
    assert!(
        object.keys().any(|key| key.contains("<workspace>")),
        "{object:?}"
    );

    let counts = &value["receipt"]["counts"];
    assert_eq!(
        counts["object_keys_original"],
        counts["object_keys_retained"]
    );
    assert_eq!(counts["object_keys_truncated"], json!(3));
    assert!(
        counts["object_keys_deduped"].as_u64().expect("deduped") >= 2,
        "{counts}"
    );
    let reasons = value["receipt"]["reasons"].as_array().expect("reasons");
    assert!(
        reasons.contains(&json!("object_key_bytes_cap")),
        "{reasons:?}"
    );
    assert!(
        reasons.contains(&json!("object_key_collision")),
        "{reasons:?}"
    );
}

#[test]
fn sensitive_keys_are_classified_after_control_and_ansi_normalization() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-obfuscated-keys".to_string(),
            name: "exec_command".to_string(),
            input: json!({
                "api\u{1b}[31m_key": "plain-value-that-must-not-leak",
                "pass\u{7}word": "another-plain-value-that-must-not-leak",
            }),
            caller: None,
            thought_signature: None,
        }],
    }]);

    let json = stdout_json(&execute_structcopy(
        &mut app,
        Some("tool call-obfuscated-keys stdout"),
    ));
    assert!(!json.contains("plain-value-that-must-not-leak"), "{json}");
    assert!(
        !json.contains("another-plain-value-that-must-not-leak"),
        "{json}"
    );
    let value = parsed(&json);
    assert_eq!(
        value["object"]["input"]["api_key"],
        json!(SENSITIVE_VALUE_REDACTION_MARKER)
    );
    assert_eq!(
        value["object"]["input"]["password"],
        json!(SENSITIVE_VALUE_REDACTION_MARKER)
    );
}

#[test]
fn collision_suffix_reserve_reports_its_own_truncation() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    let exact = "x".repeat(MAX_KEY_BYTES);
    let same_after_flatten = format!("{exact}\n");
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-reserve".to_string(),
            name: "exec_command".to_string(),
            input: json!({exact: 1, same_after_flatten: 2}),
            caller: None,
            thought_signature: None,
        }],
    }]);

    let json = stdout_json(&execute_structcopy(
        &mut app,
        Some("tool call-reserve stdout"),
    ));
    let value = parsed(&json);
    let input = value["object"]["input"].as_object().expect("input");
    assert_eq!(input.len(), 2);
    assert!(input.keys().all(|key| key.len() <= MAX_KEY_BYTES));
    let counts = &value["receipt"]["counts"];
    assert_eq!(counts["object_keys_deduped"], json!(1));
    assert_eq!(counts["object_keys_truncated"], json!(1));
    let reasons = value["receipt"]["reasons"].as_array().expect("reasons");
    assert!(
        reasons.contains(&json!("object_key_collision")),
        "{reasons:?}"
    );
    assert!(
        reasons.contains(&json!("object_key_bytes_cap")),
        "{reasons:?}"
    );
}

#[test]
fn output_is_deterministic_with_recursively_sorted_keys() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);

    let first = stdout_json(&execute_structcopy(&mut app, Some("tool call-7 stdout")));
    let second = stdout_json(&execute_structcopy(&mut app, Some("tool call-7 stdout")));
    assert_eq!(first, second, "output must be byte-for-byte deterministic");

    let value = parsed(&first);
    let top: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(top, ["object", "receipt"]);
    let receipt: Vec<&String> = value["receipt"]
        .as_object()
        .expect("receipt")
        .keys()
        .collect();
    let mut sorted = receipt.clone();
    sorted.sort();
    assert_eq!(receipt, sorted, "receipt keys must be sorted");
    let counts: Vec<&String> = value["receipt"]["counts"]
        .as_object()
        .expect("counts")
        .keys()
        .collect();
    let mut sorted_counts = counts.clone();
    sorted_counts.sort();
    assert_eq!(counts, sorted_counts, "counts keys must be sorted");
    let object: Vec<&String> = value["object"]
        .as_object()
        .expect("object")
        .keys()
        .collect();
    let mut sorted_object = object.clone();
    sorted_object.sort();
    assert_eq!(object, sorted_object, "object keys must be sorted");
}

#[test]
fn hostile_content_is_redacted_before_serialization() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    let workspace = tmpdir.path().to_string_lossy().into_owned();
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: format!(
                "escaped \\\"api_key\\\": \\\"sk-escapedsecret99\\\"\n\
                 bearer: Bearer abcdef1234567890\n\
                 jwt eyJhbGciOiJIUzI1NiIsFAKE.eyJGQUtFIjoiZml4dHVyZSJ9.FAKEFIXTURESIGNATUREnotasecret000\n\
                 url https://bob:s3cret@example.com/deep?session_token=xyz&ok=1#section\n\
                 path {workspace}/src/main.rs"
            ),
            cache_control: None,
        }],
    }]);

    let json = stdout_json(&execute_structcopy(&mut app, Some("turn 1 stdout")));
    for forbidden in [
        "sk-escapedsecret99",
        "abcdef1234567890",
        "eyJhbGciOiJIUzI1NiIs",
        "s3cret",
        "session_token=xyz",
        "section",
        workspace.as_str(),
    ] {
        assert!(!json.contains(forbidden), "leaked {forbidden:?}: {json}");
    }
    assert!(json.contains("https://example.com/deep"), "{json}");
    assert!(json.contains("<workspace>/src/main.rs"), "{json}");
    assert!(parsed(&json).is_object());
}

/// Workspace/home paths retain useful labels. Every other absolute POSIX,
/// drive-letter, and UNC path is removed from copied values.
#[test]
fn path_labels_preserve_known_roots_and_scrub_every_other_absolute_path() {
    let tmpdir = TempDir::new().expect("tempdir");
    let workspace = tmpdir.path().to_path_buf();
    let labels = PathLabels::new(&observe_path_roots(&workspace));
    let literal = workspace.to_string_lossy().into_owned();

    let folded = labels.apply(&format!("open {literal}/src/main.rs now"));
    assert_eq!(folded, "open <workspace>/src/main.rs now");
    assert!(!folded.contains(&literal));
    assert_eq!(
        scrub_string(&format!("open {literal}/src/main.rs now"), &labels),
        "open <workspace>/src/main.rs now"
    );

    // The canonical form folds too (macOS /var -> /private/var).
    if let Ok(canonical) = workspace.canonicalize() {
        let canonical = canonical.to_string_lossy().into_owned();
        let folded = labels.apply(&format!("open {canonical}/src/main.rs"));
        assert_eq!(folded, "open <workspace>/src/main.rs");
    }

    // Repeated occurrences all fold, not just the first.
    let folded = labels.apply(&format!("{literal}/a and {literal}/b"));
    assert_eq!(folded, "<workspace>/a and <workspace>/b");

    // Prefix folding itself only handles known roots; the composed scrub
    // removes every foreign absolute path before serialization.
    let foreign = "/opt/other/place/file.txt";
    assert_eq!(labels.apply(foreign), foreign);
    assert_eq!(scrub_string(foreign, &labels), PATH_OMISSION_MARKER);
    assert_eq!(
        scrub_string(r"C:\Users\customer\secret.txt", &labels),
        PATH_OMISSION_MARKER
    );
    assert_eq!(
        scrub_string(r"\\server\private\customer.txt", &labels),
        PATH_OMISSION_MARKER
    );
    let spaced = scrub_string(
        "open /Volumes/Client Name/private file.txt then continue\nsecond line",
        &labels,
    );
    assert_eq!(spaced, format!("open {PATH_OMISSION_MARKER}\nsecond line"));

    // A workspace nested inside $HOME folds to <workspace>, not <home>.
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().into_owned();
        if home.len() > 3 {
            let nested =
                PathLabels::new(&observe_path_roots(Path::new(&format!("{home}/nested/ws"))));
            let folded = nested.apply(&format!("{home}/nested/ws/src"));
            assert_eq!(folded, "<workspace>/src");
            assert_eq!(
                nested.apply(&format!("{home}/elsewhere")),
                "<home>/elsewhere"
            );
            assert_eq!(
                scrub_string(&format!("{home}/elsewhere/file.rs"), &nested),
                "<home>/elsewhere/file.rs"
            );
        }
    }
}

#[test]
fn absolute_paths_are_scrubbed_from_values_keys_and_selectors() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    let call_id = "call=/opt/customer/private-id";
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: call_id.to_string(),
            name: "exec_command".to_string(),
            input: json!({
                "/Volumes/ClientSecret/source.rs": "open C:\\Users\\customer\\secret.txt",
                "unc": r"\\server\private\customer.txt",
            }),
            caller: None,
            thought_signature: None,
        }],
    }]);

    let json = stdout_json(&execute_structcopy(
        &mut app,
        Some(&format!("tool {call_id} stdout")),
    ));
    for forbidden in [
        "/opt/customer/private-id",
        "/Volumes/ClientSecret/source.rs",
        r"C:\Users\customer\secret.txt",
        r"\\server\private\customer.txt",
        "ClientSecret",
        "customer",
    ] {
        assert!(!json.contains(forbidden), "leaked {forbidden:?}: {json}");
    }
    assert!(json.contains(PATH_OMISSION_MARKER), "{json}");
}

#[test]
fn string_bytes_cap_truncates_grapheme_safely() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "emoji cluster test: 👨‍👩‍👧‍👦🏳️‍🌈 repeated many times over".repeat(20),
            cache_control: None,
        }],
    }]);
    let caps = Caps {
        max_string_bytes: 40,
        ..DEFAULT_CAPS
    };
    let json = render_copy(&mut app, &CopyKind::Turn(1), &caps).expect("render");
    let value = parsed(&json);
    let text = value["object"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.ends_with('…'), "{text}");
    assert!(text.len() <= 40, "{} bytes", text.len());
    assert_eq!(value["receipt"]["counts"]["strings_truncated"], json!(1));
    assert_eq!(value["receipt"]["reasons"], json!(["string_bytes_cap"]));
    let original = value["receipt"]["counts"]["string_bytes_original"]
        .as_u64()
        .expect("original");
    let retained = value["receipt"]["counts"]["string_bytes_retained"]
        .as_u64()
        .expect("retained");
    assert!(original > retained);
}

/// A cap below the ellipsis's own 3 bytes has no representable
/// "truncated" form. It must stay in-bounds and stay honest rather than
/// panic, overflow, or emit partial content.
#[test]
fn string_cap_below_the_ellipsis_is_safe() {
    for max_bytes in 0..=4usize {
        for text in ["", "a", "ab", "abc", "abcd", "é", "👨‍👩‍👧‍👦", "héllo wörld"]
        {
            let (out, truncated) = truncate_string_grapheme_safe(text, max_bytes);
            assert!(
                out.len() <= max_bytes.max(text.len()),
                "cap {max_bytes} text {text:?} -> {out:?}"
            );
            if text.len() <= max_bytes {
                assert!(!truncated);
                assert_eq!(out, text);
            } else {
                assert!(truncated, "cap {max_bytes} text {text:?}");
                assert!(
                    out.len() <= max_bytes,
                    "cap {max_bytes} text {text:?} -> {} bytes",
                    out.len()
                );
                if max_bytes < 3 {
                    assert!(
                        out.is_empty(),
                        "no partial content may escape below the marker size: {out:?}"
                    );
                } else {
                    assert!(out.ends_with('…'), "cap {max_bytes} -> {out:?}");
                }
            }
            assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        }
    }

    // End to end: the whole pipeline survives a sub-ellipsis cap and the
    // receipt still reports the truncation.
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "a longer body that cannot fit".to_string(),
            cache_control: None,
        }],
    }]);
    let caps = Caps {
        max_string_bytes: 1,
        ..DEFAULT_CAPS
    };
    let json = render_copy(&mut app, &CopyKind::Turn(1), &caps).expect("render");
    let value = parsed(&json);
    assert_eq!(value["object"]["content"][0]["text"], json!(""));
    assert!(
        value["receipt"]["counts"]["strings_truncated"]
            .as_u64()
            .expect("truncated")
            >= 1
    );
}

#[test]
fn array_items_cap_counts_original_and_retained_exactly() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    {
        let mut state = app.plan_state.try_lock().expect("plan lock");
        state.update(UpdatePlanArgs {
            plan: (0..10)
                .map(|index| PlanItemArg {
                    step: format!("step {index}"),
                    status: StepStatus::Pending,
                })
                .collect(),
            ..Default::default()
        });
    }
    let caps = Caps {
        max_array_items: 3,
        ..DEFAULT_CAPS
    };
    let json = render_copy(&mut app, &CopyKind::Plan, &caps).expect("render");
    let value = parsed(&json);
    assert_eq!(value["object"]["items"].as_array().expect("items").len(), 3);
    assert_eq!(
        value["receipt"]["counts"]["array_items_original"],
        json!(10)
    );
    assert_eq!(value["receipt"]["counts"]["array_items_retained"], json!(3));
    assert_eq!(value["receipt"]["reasons"], json!(["array_items_cap"]));
}

#[test]
fn depth_cap_omits_deep_subtrees() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-deep".to_string(),
            name: "exec_command".to_string(),
            input: json!({"a": {"b": {"c": {"d": {"e": "too deep"}}}}}),
            caller: None,
            thought_signature: None,
        }],
    }]);
    let caps = Caps {
        max_depth: 3,
        ..DEFAULT_CAPS
    };
    let json =
        render_copy(&mut app, &CopyKind::Tool("call-deep".to_string()), &caps).expect("render");
    let value = parsed(&json);
    assert!(json.contains(DEPTH_OMISSION_MARKER), "{json}");
    assert!(!json.contains("too deep"), "{json}");
    let omissions = value["receipt"]["counts"]["depth_omissions"]
        .as_u64()
        .expect("omissions");
    assert!(omissions >= 1, "{omissions}");
    assert!(
        value["receipt"]["reasons"]
            .as_array()
            .expect("reasons")
            .contains(&json!("depth_cap"))
    );
}

/// The original counts describe the full redacted tree; the retained
/// counts describe exactly what was emitted, marker strings included.
/// Both must be checkable against the artifact itself.
#[test]
fn counts_stay_exact_across_a_depth_omission() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    // Two strings and two array items live below the depth cut, plus one
    // string and one array item above it.
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-counts".to_string(),
            name: "exec_command".to_string(),
            input: json!({
                "shallow": ["kept"],
                "deep": {"one": {"two": ["cut-a", "cut-b"]}},
            }),
            caller: None,
            thought_signature: None,
        }],
    }]);
    let caps = Caps {
        max_depth: 3,
        ..DEFAULT_CAPS
    };
    let json =
        render_copy(&mut app, &CopyKind::Tool("call-counts".to_string()), &caps).expect("render");
    let value = parsed(&json);
    let counts = &value["receipt"]["counts"];

    // Independently recount the emitted object and compare.
    let mut emitted = BoundStats::default();
    collect_original_counts(&value["object"], &mut emitted);
    assert_eq!(
        counts["strings_retained"].as_u64().expect("retained"),
        emitted.strings_total,
        "retained string count must match the emitted artifact: {json}"
    );
    assert_eq!(
        counts["string_bytes_retained"]
            .as_u64()
            .expect("retained bytes"),
        emitted.string_bytes_original,
        "retained bytes must include the depth marker: {json}"
    );
    assert_eq!(
        counts["array_items_retained"].as_u64().expect("items"),
        emitted.array_items_original,
        "{json}"
    );

    // Originals cover the *whole* tree, including the omitted subtree.
    assert!(
        counts["strings_total"].as_u64().expect("total")
            > counts["strings_retained"].as_u64().expect("retained"),
        "originals must count strings under the depth cut: {counts}"
    );
    assert!(
        counts["array_items_original"].as_u64().expect("original")
            > counts["array_items_retained"].as_u64().expect("retained"),
        "originals must count array items under the depth cut: {counts}"
    );
    assert_eq!(counts["depth_omissions"], json!(1));
    assert!(
        counts["object_keys_original"]
            .as_u64()
            .expect("original keys")
            > counts["object_keys_retained"]
                .as_u64()
                .expect("retained keys"),
        "keys under the depth cut must be original-only: {counts}"
    );
}

#[test]
fn omitted_key_transformations_do_not_claim_emitted_reasons() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    let long_a = format!("{}A", "private-key-name-".repeat(32));
    let long_b = format!("{}B", "private-key-name-".repeat(32));
    app.api_messages = std::sync::Arc::new(vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: "call-deep-keys".to_string(),
            name: "exec_command".to_string(),
            input: json!({"deep": {"one": {long_a: 1, long_b: 2}}}),
            caller: None,
            thought_signature: None,
        }],
    }]);
    let caps = Caps {
        max_depth: 3,
        ..DEFAULT_CAPS
    };
    let json = render_copy(
        &mut app,
        &CopyKind::Tool("call-deep-keys".to_string()),
        &caps,
    )
    .expect("render");
    let value = parsed(&json);
    let counts = &value["receipt"]["counts"];
    assert!(
        counts["object_keys_original"].as_u64().expect("original")
            > counts["object_keys_retained"].as_u64().expect("retained"),
        "{counts}"
    );
    assert_eq!(counts["object_keys_truncated"], json!(0));
    assert_eq!(counts["object_keys_deduped"], json!(0));
    let reasons = value["receipt"]["reasons"].as_array().expect("reasons");
    assert!(
        !reasons.contains(&json!("object_key_bytes_cap")),
        "{reasons:?}"
    );
    assert!(
        !reasons.contains(&json!("object_key_collision")),
        "{reasons:?}"
    );
}

#[test]
fn output_bytes_cap_omits_payload_then_fails_closed() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    {
        let mut state = app.plan_state.try_lock().expect("plan lock");
        state.update(UpdatePlanArgs {
            title: Some("large plan".to_string()),
            plan: (0..60)
                .map(|index| PlanItemArg {
                    step: format!("step {index}: {}", "padding ".repeat(40)),
                    status: StepStatus::Pending,
                })
                .collect(),
            ..Default::default()
        });
    }

    // Tight byte cap: payload must be omitted while the receipt survives.
    let caps = Caps {
        max_output_bytes: 2 * 1024,
        ..DEFAULT_CAPS
    };
    let json = render_copy(&mut app, &CopyKind::Plan, &caps).expect("render");
    assert!(json.len() <= 2 * 1024, "{} bytes", json.len());
    let value = parsed(&json);
    assert_eq!(value["object"], Value::Null);
    let reasons = value["receipt"]["reasons"].as_array().expect("reasons");
    assert!(
        reasons.contains(&json!("payload_omitted_output_bytes_cap")),
        "{reasons:?}"
    );
    // Nothing was emitted, so no retained counter and no bounding reason
    // may claim otherwise.
    for retained in [
        "array_items_retained",
        "string_bytes_retained",
        "strings_retained",
        "strings_truncated",
        "depth_omissions",
        "object_keys_retained",
        "object_keys_truncated",
        "object_keys_deduped",
    ] {
        assert_eq!(
            value["receipt"]["counts"][retained],
            json!(0),
            "{retained} must be zero when nothing was emitted: {json}"
        );
    }
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert_eq!(
        value["receipt"]["counts"]["array_items_original"],
        json!(60)
    );
    assert!(
        value["receipt"]["counts"]["object_keys_original"]
            .as_u64()
            .expect("original keys")
            > 0
    );

    // Below the metadata floor the command fails closed and emits nothing.
    let tiny = Caps {
        max_output_bytes: 64,
        ..DEFAULT_CAPS
    };
    let err = render_copy(&mut app, &CopyKind::Plan, &tiny).expect_err("must fail closed");
    assert!(err.contains("refusing to emit"), "{err}");
    let result = execute_structcopy(&mut app, Some("plan stdout"));
    assert!(!result.is_error, "default caps fit: {:?}", result.message);
}

/// When the byte cap forces tighter caps than the declared contract, the
/// receipt must say so instead of advertising caps that never ran.
#[test]
fn receipt_reports_the_caps_that_actually_ran() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    {
        let mut state = app.plan_state.try_lock().expect("plan lock");
        state.update(UpdatePlanArgs {
            title: Some("padded plan".to_string()),
            plan: (0..40)
                .map(|index| PlanItemArg {
                    step: format!("step {index}: {}", "padding ".repeat(30)),
                    status: StepStatus::Pending,
                })
                .collect(),
            ..Default::default()
        });
    }
    let caps = Caps {
        max_output_bytes: 6 * 1024,
        ..DEFAULT_CAPS
    };
    let json = render_copy(&mut app, &CopyKind::Plan, &caps).expect("render");
    let value = parsed(&json);
    assert_eq!(
        value["receipt"]["caps"]["max_output_bytes"],
        json!(6 * 1024)
    );
    let applied = &value["receipt"]["applied_caps"];
    assert!(
        applied["max_array_items"].as_u64().expect("items") <= DEFAULT_CAPS.max_array_items as u64
    );
    if applied != &value["receipt"]["caps"] {
        assert!(
            value["receipt"]["reasons"]
                .as_array()
                .expect("reasons")
                .contains(&json!("caps_tightened_output_bytes_cap")),
            "{json}"
        );
    }

    // The unconstrained case declares no tightening.
    let json = stdout_json(&execute_structcopy(&mut app, Some("plan stdout")));
    let value = parsed(&json);
    assert_eq!(value["receipt"]["applied_caps"], value["receipt"]["caps"]);
    assert!(
        !value["receipt"]["reasons"]
            .as_array()
            .expect("reasons")
            .contains(&json!("caps_tightened_output_bytes_cap"))
    );
}

#[test]
fn clipboard_is_default_and_stdout_is_explicit() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);

    // Default: clipboard target; the payload never appears in the message.
    let default = execute_structcopy(&mut app, Some("turn 1"));
    assert!(!default.is_error, "{:?}", default.message);
    let message = default.message.as_deref().unwrap_or_default();
    assert!(message.contains("handed to the clipboard"), "{message}");
    // The receipt must not overclaim delivery.
    assert!(
        !message.contains("copied to the local clipboard"),
        "{message}"
    );
    assert!(!message.contains("\"receipt\""), "{message}");
    let payload = app
        .clipboard
        .last_written_text()
        .expect("clipboard payload");
    assert!(payload.contains("\"receipt\""));

    // Explicit stdout: payload in the message, clipboard untouched.
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);
    let stdout = execute_structcopy(&mut app, Some("turn 1 stdout"));
    assert!(
        stdout
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("\"receipt\"")
    );
    assert!(app.clipboard.last_written_text().is_none());
}

/// The terminal-client path queues a background write; the message must
/// not claim the copy landed, and must not claim a transport the session
/// does not have.
#[test]
fn terminal_client_receipt_says_queued_not_delivered() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);
    // SSH with no display: the write goes to the terminal writer, as in
    // production, and the receipt follows the transport that took it.
    app.clipboard = ClipboardHandler::terminal_only_for_test();
    assert!(app.clipboard.requires_terminal_paste());

    let result = execute_structcopy(&mut app, Some("turn 1"));
    assert!(!result.is_error, "{:?}", result.message);
    let message = result.message.as_deref().unwrap_or_default();
    assert!(message.contains("queued"), "{message}");
    assert!(message.contains("not confirmed"), "{message}");
    assert!(
        !message.contains("copied to"),
        "must not claim delivery: {message}"
    );
}

#[test]
fn clipboard_failure_is_honest_and_suggests_stdout() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);
    app.clipboard = ClipboardHandler::unavailable_for_test(false);

    let failed = execute_structcopy(&mut app, Some("turn 1"));
    assert!(failed.is_error);
    let message = failed.message.as_deref().unwrap_or_default();
    assert!(message.contains("Nothing was written"), "{message}");
    assert!(message.contains("stdout"), "{message}");
    assert!(app.clipboard.last_written_text().is_none());
}

#[test]
fn copy_does_not_mutate_session_state() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    seed_transcript(&mut app);
    {
        let mut state = app.plan_state.try_lock().expect("plan lock");
        state.update(UpdatePlanArgs {
            title: Some("immutable".to_string()),
            ..Default::default()
        });
    }
    let plan_before = app.plan_state.try_lock().expect("plan lock").snapshot();
    let messages_before = app.api_messages.clone();
    let history_before = app.history.len();
    let work_before = app.work_state_snapshot().expect("Work snapshot");

    for arg in [
        "turn 1 stdout",
        "turn 2",
        "tool call-7 stdout",
        "plan stdout",
        "turn 99 stdout",
        "tool call-nope stdout",
        "workflow nope stdout",
    ] {
        let _ = execute_structcopy(&mut app, Some(arg));
    }

    assert_eq!(app.api_messages, messages_before);
    assert_eq!(app.history.len(), history_before);
    assert_eq!(
        app.plan_state.try_lock().expect("plan lock").snapshot(),
        plan_before
    );
    assert_eq!(
        app.work_state_snapshot().expect("Work snapshot after copy"),
        work_before,
        "structcopy must not mutate Work"
    );
}

#[test]
fn structcopy_is_registered_human_only_and_absent_from_model_catalog() {
    // Registered as a human slash command.
    assert!(
        crate::commands::command_infos()
            .iter()
            .any(|info| info.name == "structcopy"),
        "structcopy must be a registered slash command"
    );

    // Never a model-visible tool: neither in the native tool catalog nor
    // in the legacy tool registry surface sent to providers.
    assert!(
        !crate::core::engine::default_active_native_tool_names().contains(&"structcopy"),
        "structcopy must not be a native tool"
    );
    let tmpdir = TempDir::new().expect("tempdir");
    let context = crate::tools::spec::ToolContext::new(tmpdir.path().to_path_buf());
    let registry = crate::tools::ToolRegistryBuilder::new()
        .with_file_tools()
        .with_read_only_file_tools()
        .with_shell_tools()
        .with_search_tools()
        .with_git_tools()
        .with_git_history_tools()
        .with_diagnostics_tool()
        .with_skill_tools()
        .with_validation_tools()
        .with_project_tools()
        .with_test_runner_tool()
        .with_tool_result_retrieval_tool()
        .with_web_tools()
        .with_finance_tool()
        .build(context);
    let names: Vec<String> = registry
        .to_api_tools()
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert!(
        !names.is_empty(),
        "builder surface must register model tools for this contract to be meaningful"
    );
    assert!(
        !names.iter().any(|name| name.contains("structcopy")),
        "no model tool may reference structcopy: {names:?}"
    );
}

fn execute_structcopy(app: &mut App, arg: Option<&str>) -> CommandResult {
    let mut bundle = app.command_contexts();
    super::contract::structcopy_host::host_result(structcopy::execute_structcopy(
        bundle.contexts(CAPABILITIES),
        arg,
    ))
}

fn render_copy(app: &mut App, kind: &CopyKind, caps: &Caps) -> Result<String, String> {
    let mut bundle = app.command_contexts();
    let parts = bundle.contexts(CAPABILITIES).into_parts();
    structcopy::render_copy(
        parts.structcopy.unwrap(),
        parts.presentation.unwrap(),
        kind,
        caps,
    )
}

#[test]
fn tool_copy_selects_execution_when_provider_reuses_wire_id() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    app.api_messages = std::sync::Arc::new(
        serde_json::from_value(json!([
            {"role":"assistant", "content":[{"type":"tool_use", "id":"wire",
                "execution_id":"first", "name":"read_file", "input":{"path":"first.txt"}}]},
            {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"wire",
                "execution_id":"first", "content":"first output"}]},
            {"role":"assistant", "content":[{"type":"tool_use", "id":"wire",
                "execution_id":"second", "name":"read_file", "input":{"path":"second.txt"}}]},
            {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"wire",
                "execution_id":"second", "content":"second output"}]}
        ]))
        .expect("transcript"),
    );
    let before = serde_json::to_value(app.api_messages.as_ref()).expect("transcript");
    for (selector, path, output) in [
        ("first", "first.txt", "first output"),
        ("second", "second.txt", "second output"),
    ] {
        let value = parsed(&stdout_json(&execute_structcopy(
            &mut app,
            Some(&format!("tool {selector} stdout")),
        )));
        assert_eq!(value["receipt"]["selector"], selector);
        assert_eq!(value["object"]["call_id"], selector);
        assert_eq!(value["object"]["input"]["path"], path);
        assert_eq!(value["object"]["result"]["content"], output);
    }
    assert!(execute_structcopy(&mut app, Some("tool wire stdout")).is_error);
    assert_eq!(
        serde_json::to_value(app.api_messages.as_ref()).unwrap(),
        before
    );
}

#[test]
fn tool_copy_refuses_ambiguous_or_inconsistent_identity() {
    let tmpdir = TempDir::new().expect("tempdir");
    let mut app = test_app(&tmpdir);
    let call = |execution_id: Option<&str>, provider: &str| {
        json!({
            "type":"tool_use", "id":provider, "execution_id":execution_id,
            "name":"read_file", "input":{"path":"selected.txt"}
        })
    };
    let result = |execution_id: Option<&str>, provider: &str| {
        json!({
            "type":"tool_result", "tool_use_id":provider, "execution_id":execution_id,
            "content":"must not borrow this output"
        })
    };
    for (label, selector, calls, results, unavailable) in [
        (
            "duplicate local calls",
            "exec",
            vec![call(Some("exec"), "wire"), call(Some("exec"), "wire")],
            vec![result(Some("exec"), "wire")],
            true,
        ),
        (
            "duplicate legacy calls",
            "wire",
            vec![call(None, "wire"), call(None, "wire")],
            vec![result(None, "wire")],
            true,
        ),
        (
            "duplicate local results",
            "exec",
            vec![call(Some("exec"), "wire")],
            vec![result(Some("exec"), "wire"), result(Some("exec"), "wire")],
            true,
        ),
        (
            "duplicate legacy results",
            "wire",
            vec![call(None, "wire")],
            vec![result(None, "wire"), result(None, "wire")],
            true,
        ),
        (
            "wrong provider",
            "exec",
            vec![call(Some("exec"), "wire")],
            vec![result(Some("exec"), "other")],
            true,
        ),
        (
            "empty local identity",
            "wire",
            vec![call(Some(""), "wire")],
            vec![result(Some(""), "wire")],
            true,
        ),
        (
            "colliding domains",
            "exec",
            vec![call(Some("exec"), "wire"), call(None, "exec")],
            vec![result(Some("exec"), "wire"), result(None, "exec")],
            true,
        ),
        (
            "no local fallback",
            "exec",
            vec![call(Some("exec"), "wire")],
            vec![result(Some("wrong"), "wire"), result(None, "wire")],
            false,
        ),
        (
            "no legacy fallback",
            "wire",
            vec![call(None, "wire")],
            vec![result(Some("wire"), "wire")],
            false,
        ),
    ] {
        app.api_messages = std::sync::Arc::new(
            serde_json::from_value(json!([
                {"role":"assistant", "content":calls}, {"role":"user", "content":results}
            ]))
            .expect("transcript"),
        );
        let before = serde_json::to_value(app.api_messages.as_ref()).unwrap();
        let copied = execute_structcopy(&mut app, Some(&format!("tool {selector} stdout")));
        assert_eq!(copied.is_error, unavailable, "{label}");
        if !unavailable {
            let value = parsed(&stdout_json(&copied));
            assert_eq!(value["object"]["result"]["found"], false, "{label}");
        }
        assert!(
            !copied
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("must not borrow"),
            "{label}"
        );
        assert_eq!(
            serde_json::to_value(app.api_messages.as_ref()).unwrap(),
            before,
            "{label}"
        );
    }
}
