use crate::tools::apply_patch::ApplyPatchTool;
use crate::tools::spec::ToolSpec;
use codewhale_models::schema_sanitize::{
    sanitize_for_kimi_parameters, sanitize_for_xai_parameters,
};

#[test]
fn xai_sanitize_flattens_apply_patch_root_one_of() {
    // The exact shape that produced the live 400:
    // "apply_patch: tool parameter root must be an object type (root
    // schema is an anyOf/oneOf union with a non-object branch)".
    let mut schema = ApplyPatchTool.input_schema();
    assert!(schema.get("oneOf").is_some(), "fixture must match the tool");

    let note = sanitize_for_xai_parameters(&mut schema);

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
    let mut schema = ApplyPatchTool.input_schema();

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
