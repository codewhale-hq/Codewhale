use codewhale_models::schema_sanitize::{sanitize_for_kimi_parameters, sanitize_for_responses};
use serde_json::{Value, json};

fn root_one_of_apply_patch_fixture() -> Value {
    json!({
        "type": "object",
        "properties": {
            "patch": { "type": "string" },
            "replace": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            },
            "changes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        "oneOf": [
            { "required": ["patch"] },
            { "required": ["replace"] },
            { "required": ["changes"] }
        ]
    })
}

#[test]
fn xai_sanitize_flattens_apply_patch_root_one_of() {
    let mut schema = root_one_of_apply_patch_fixture();
    assert!(schema.get("oneOf").is_some(), "fixture must match the tool");

    let note = sanitize_for_responses(&mut schema);

    assert_eq!(schema["type"], "object");
    assert!(schema.get("oneOf").is_none());
    assert!(schema.get("anyOf").is_none());
    assert!(schema["properties"].get("patch").is_some());
    assert!(schema["properties"].get("changes").is_some());
    assert_eq!(
        note.as_deref(),
        Some(
            "Exactly one of these parameter groups must be provided: `changes` | `patch` | `replace`."
        )
    );
}

#[test]
fn kimi_parameters_flattens_actual_apply_patch_root_and_returns_constraint_note() {
    let mut schema = root_one_of_apply_patch_fixture();

    let note = sanitize_for_kimi_parameters(&mut schema).unwrap();

    assert_eq!(schema["type"], "object");
    assert!(schema.get("oneOf").is_none());
    assert!(schema.get("anyOf").is_none());
    assert!(schema.get("allOf").is_none());
    assert_eq!(schema["properties"]["patch"]["type"], "string");
    assert_eq!(schema["properties"]["replace"]["type"], "array");
    assert_eq!(schema["properties"]["changes"]["type"], "array");
    assert_eq!(
        note.as_deref(),
        Some(
            "Exactly one of these parameter groups must be provided: `changes` | `patch` | `replace`."
        )
    );
}
