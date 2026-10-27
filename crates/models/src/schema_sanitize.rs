//! Schema sanitizer for tool `input_schema` before sending to provider APIs.
//!
//! DeepSeek's `/beta/chat/completions` strict tool mode is harsh. MCP tool
//! schemas frequently arrive with Pydantic-style `anyOf:[{type:"string"},
//! {type:"null"}]` unions, bare `{type:"object"}` with no `properties`, or
//! `required` entries that don't appear in `properties`. These dirty schemas
//! cause silent 400s that users can't diagnose.
//!
//! The default sanitizer runs in-place on every schema returned by
//! `ToolRegistry::tools_for_api()` before the registry hands them off.
//! Provider-specific helpers below add stricter DeepSeek and OpenAI Responses
//! compatibility passes where their request shapes need it.

use std::collections::HashSet;

use serde_json::{Map, Value};

/// Sanitize a JSON Schema in-place for DeepSeek strict-tool compatibility.
///
/// Applies a sequence of normalisations chosen to be semantics-preserving:
/// - Collapse `{"anyOf":[X, {"type":"null"}]}` → `X ∪ {"nullable": true}`
/// - Inject `"properties": {}` on bare-object schemas
/// - Prune dangling `required` entries
/// - Collapse single-element `oneOf` / `allOf`
/// - Walk recursively through all subschemas
pub fn sanitize(schema: &mut Value) {
    collapse_nullable_unions(schema);
    inject_properties_on_bare_objects(schema);
    prune_dangling_required(schema);
    collapse_single_element_unions(schema);
    // Recurse into all sub-schemas
    if let Some(obj) = schema.as_object_mut() {
        for (_, v) in obj.iter_mut() {
            sanitize(v);
        }
    } else if let Some(arr) = schema.as_array_mut() {
        for v in arr.iter_mut() {
            sanitize(v);
        }
    }
}

/// Sanitize a schema for DeepSeek strict function-calling.
///
/// This extends the general sanitizer with the official strict-mode object
/// rules: every object must set `additionalProperties: false`, and every
/// property must be listed in `required`.
pub fn sanitize_for_strict(schema: &mut Value) {
    sanitize(schema);
    enforce_strict_subset(schema);
}

/// Sanitize a tool `parameters` schema for xAI chat completions.
///
/// xAI validates that the parameters root is an object schema and rejects a
/// root-level `anyOf`/`oneOf` union with any non-object branch
/// ("tool parameter root must be an object type"). The built-in `apply_patch`
/// schema's `oneOf: [{required:["patch"]}, {required:["changes"]}]` trips this
/// with a 400 on the first tool-bearing request. The Responses-API pass
/// performs exactly the required normalization — merge root composition
/// properties, force `type: object`, drop root-only composition keywords —
/// so reuse it and surface the dropped constraint as a description note.
pub fn sanitize_for_xai_parameters(parameters: &mut Value) -> Option<String> {
    sanitize_for_responses(parameters)
}

/// Sanitize a schema for OpenAI Responses function tools.
///
/// The Responses API requires the top-level `parameters` schema to be an object
/// and rejects top-level `oneOf` / `anyOf` / `allOf` / `enum` / `not`. Keep the
/// schema permissive rather than changing tool semantics: merge any root
/// alternative properties we can see, then remove the root-only composition
/// keywords while preserving nested schemas.
///
/// Returns a short description note when root composition constraints with
/// meaningful `required` groups are dropped.
pub fn sanitize_for_responses(schema: &mut Value) -> Option<String> {
    let constraint_note = schema
        .as_object()
        .and_then(root_composition_constraint_note);
    let dependent_note = drop_dependent_keywords(schema);

    sanitize(schema);

    if !schema.is_object() {
        *schema = Value::Object(Map::new());
    }

    let Some(obj) = schema.as_object_mut() else {
        return combine_constraint_notes(constraint_note, dependent_note);
    };

    merge_root_composition_properties(obj);
    obj.insert("type".into(), Value::String("object".to_string()));
    obj.remove("oneOf");
    obj.remove("anyOf");
    obj.remove("allOf");
    obj.remove("enum");
    obj.remove("not");
    ensure_properties_object(obj);
    prune_dangling_required(schema);
    combine_constraint_notes(constraint_note, dependent_note)
}

pub fn strict_schema_supported(schema: &Value) -> bool {
    let mut normalized = schema.clone();
    sanitize(&mut normalized);
    !has_strict_incompatible_composition(&normalized, true)
}

fn has_strict_incompatible_composition(schema: &Value, is_root: bool) -> bool {
    if let Some(obj) = schema.as_object() {
        if obj.contains_key("oneOf")
            || obj.contains_key("allOf")
            || obj.contains_key("dependentSchemas")
        {
            return true;
        }
        if is_root && obj.contains_key("anyOf") {
            return true;
        }
        return obj
            .values()
            .any(|value| has_strict_incompatible_composition(value, false));
    }
    schema.as_array().is_some_and(|arr| {
        arr.iter()
            .any(|value| has_strict_incompatible_composition(value, false))
    })
}

/// Collapse `{"anyOf":[X, {"type":"null"}]}` → `X ∪ {"nullable": true}`.
///
/// Same treatment for `oneOf`. Only collapses when exactly one non-null
/// member and exactly one null-type member are present.
fn collapse_nullable_unions(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    for key in ["anyOf", "oneOf"] {
        let members: Vec<Value> = match obj.get(key).and_then(|v| v.as_array()) {
            Some(arr) => arr.clone(),
            None => continue,
        };
        let (nulls, nons): (Vec<_>, Vec<_>) = members.into_iter().partition(is_null_type);
        if nulls.len() == 1 && nons.len() == 1 {
            // `nullable` is only meaningful when it annotates a concrete
            // schema type. Collapsing `$ref | null` or another untyped branch
            // would manufacture an annotation-only schema and lose the
            // terminating union needed by MFJS reference validation.
            let has_concrete_non_null_type = nons[0]
                .as_object()
                .and_then(|branch| branch.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|schema_type| schema_type != "null");
            if !has_concrete_non_null_type {
                continue;
            }
            obj.remove(key);
            if let Value::Object(non_obj) = nons.into_iter().next().unwrap() {
                for (k, v) in non_obj {
                    if k != "type" || v != "null" {
                        obj.insert(k, v);
                    }
                }
            }
            obj.insert("nullable".into(), Value::Bool(true));
        }
    }
}

fn is_null_type(v: &Value) -> bool {
    v.as_object()
        .and_then(|o| o.get("type"))
        .and_then(|t| t.as_str())
        == Some("null")
}

/// Bare `{"type": "object"}` (no `properties`, no `additionalProperties`)
/// → inject `"properties": {}` so DeepSeek's strict validator doesn't 400.
fn inject_properties_on_bare_objects(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    if obj.get("type").and_then(|t| t.as_str()) != Some("object") {
        return;
    }
    if obj.contains_key("properties") || obj.contains_key("additionalProperties") {
        return;
    }
    obj.insert("properties".into(), Value::Object(Map::new()));
}

/// Remove entries from `required` that aren't keys in `properties`.
///
/// A schema with neither `properties` nor `type` is a composition branch
/// whose `required` names properties declared by the enclosing schema —
/// `oneOf: [{"required": ["patch"]}, ...]` (#6561 D04-11). Its list is kept:
/// pruning emptied every branch, turning apply_patch's "exactly one of"
/// into an unsatisfiable `oneOf` of three `{}` and the finance tool's
/// "ticker or symbol" into no constraint.
///
/// Known limitation: a `type: "object"` schema without `properties` still
/// loses `required` entries (a bare object gets `properties: {}` injected
/// first). Strict validators (DeepSeek strict, MFJS) reject `required`
/// names absent from local `properties`, and this provider-neutral pass
/// cannot invent their property types.
fn prune_dangling_required(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    if !obj.contains_key("properties") && !obj.contains_key("type") {
        return;
    }
    // Collect known property names first (immutable borrow), then prune.
    let known_keys: Vec<String> = obj
        .get("properties")
        .and_then(|v| v.as_object())
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default();
    let Some(required) = obj.get_mut("required").and_then(|v| v.as_array_mut()) else {
        return;
    };
    required.retain(|entry| {
        entry
            .as_str()
            .is_some_and(|k| known_keys.iter().any(|known| known == k))
    });
    if required.is_empty() {
        obj.remove("required");
    }
}

/// Collapse `{"oneOf": [X]}` → X, same for `allOf`.
///
/// Single-element unions are semantically equivalent to the element itself;
/// DeepSeek's strict validator doesn't always flatten them.
fn collapse_single_element_unions(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    for key in ["oneOf", "allOf", "anyOf"] {
        let single = match obj.get(key).and_then(|v| v.as_array()) {
            Some(arr) if arr.len() == 1 => arr[0].clone(),
            _ => continue,
        };
        obj.remove(key);
        if let Value::Object(inner) = single {
            for (k, v) in inner {
                if !obj.contains_key(&k) {
                    obj.insert(k, v);
                }
            }
        }
    }
}

fn enforce_strict_subset(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        strip_unsupported_strict_keywords(obj);
        if is_object_schema(obj) {
            let originally_required = required_names(obj);
            let properties = ensure_properties_object(obj);
            let mut property_names: Vec<String> = properties.keys().cloned().collect();
            property_names.sort();
            for property_name in &property_names {
                if !originally_required
                    .iter()
                    .any(|required| required == property_name)
                    && let Some(property_schema) = properties.get_mut(property_name)
                {
                    mark_nullable(property_schema);
                }
            }
            obj.insert(
                "required".into(),
                Value::Array(property_names.into_iter().map(Value::String).collect()),
            );
            obj.insert("additionalProperties".into(), Value::Bool(false));
        }

        for value in obj.values_mut() {
            enforce_strict_subset(value);
        }
    } else if let Some(arr) = schema.as_array_mut() {
        for value in arr {
            enforce_strict_subset(value);
        }
    }
}

fn strip_unsupported_strict_keywords(obj: &mut Map<String, Value>) {
    obj.remove("patternProperties");
    match obj.get("type").and_then(Value::as_str) {
        Some("string") => {
            obj.remove("minLength");
            obj.remove("maxLength");
        }
        Some("array") => {
            obj.remove("minItems");
            obj.remove("maxItems");
        }
        _ => {}
    }
}

fn is_object_schema(obj: &Map<String, Value>) -> bool {
    obj.get("type").and_then(Value::as_str) == Some("object") || obj.contains_key("properties")
}

fn ensure_properties_object(obj: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let needs_replacement = !matches!(obj.get("properties"), Some(Value::Object(_)));
    if needs_replacement {
        obj.insert("properties".into(), Value::Object(Map::new()));
    }
    obj.get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("properties was just ensured as object")
}

fn required_names(obj: &Map<String, Value>) -> Vec<String> {
    obj.get("required")
        .and_then(Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn mark_nullable(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        obj.insert("nullable".into(), Value::Bool(true));
    }
}

fn merge_root_composition_properties(obj: &mut Map<String, Value>) {
    let mut merged = Map::new();
    for key in ["oneOf", "anyOf", "allOf"] {
        let Some(items) = obj.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(properties) = item.get("properties").and_then(Value::as_object) else {
                continue;
            };
            for (name, schema) in properties {
                merged.entry(name.clone()).or_insert_with(|| schema.clone());
            }
        }
    }

    if merged.is_empty() {
        return;
    }

    let properties = ensure_properties_object(obj);
    for (name, schema) in merged {
        properties.entry(name).or_insert(schema);
    }
}

fn root_composition_constraint_note(obj: &Map<String, Value>) -> Option<String> {
    for (key, prefix) in [
        ("oneOf", "Exactly one"),
        ("anyOf", "At least one"),
        ("allOf", "All"),
    ] {
        let Some(items) = obj.get(key).and_then(Value::as_array) else {
            continue;
        };
        let mut groups: Vec<String> = items.iter().filter_map(required_group_label).collect();
        groups.sort();
        groups.dedup();
        if groups.len() >= 2 {
            return Some(format!(
                "{prefix} of these parameter groups must be provided: {}.",
                groups.join(" | ")
            ));
        }
    }
    None
}

/// Strip `dependentSchemas` / `dependentRequired` from every node.
///
/// MFJS validates each node against a closed keyword allow-list, so a schema
/// carrying either keyword is refused outright — and that refusal reaches
/// `sanitize_moonshot_chat_tools`, which fails the whole request build, so one
/// composed schema would break *every* tool-bearing Moonshot turn rather than
/// degrade its own tool.
fn drop_dependent_keywords(schema: &mut Value) -> Option<String> {
    strip_dependent_keywords(schema).then(|| {
        "This provider cannot express conditional requirements from the original schema. \
         Honor any such requirements documented by the tool when calling it."
            .to_string()
    })
}

fn combine_constraint_notes(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first} {second}")),
        (Some(note), None) | (None, Some(note)) => Some(note),
        (None, None) => None,
    }
}

fn strip_dependent_keywords(schema: &mut Value) -> bool {
    match schema {
        Value::Object(obj) => {
            let mut dropped = obj.remove("dependentRequired").is_some();
            dropped |= obj.remove("dependentSchemas").is_some();
            for value in obj.values_mut() {
                dropped |= strip_dependent_keywords(value);
            }
            dropped
        }
        Value::Array(items) => {
            let mut dropped = false;
            for item in items {
                dropped |= strip_dependent_keywords(item);
            }
            dropped
        }
        _ => false,
    }
}

fn required_group_label(item: &Value) -> Option<String> {
    let mut names: Vec<String> = item
        .get("required")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(|name| format!("`{name}`"))
        .collect();
    if names.is_empty() {
        None
    } else {
        names.sort();
        names.dedup();
        Some(names.join(" + "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collapses_nullable_anyof() {
        let mut schema = json!({
            "anyOf": [
                {"type": "string"},
                {"type": "null"}
            ]
        });
        sanitize(&mut schema);
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["nullable"], true);
        assert!(schema.get("anyOf").is_none());
    }

    #[test]
    fn collapses_nullable_oneof() {
        let mut schema = json!({
            "oneOf": [
                {"type": "null"},
                {"type": "integer", "minimum": 0}
            ]
        });
        sanitize(&mut schema);
        assert_eq!(schema["type"], "integer");
        assert_eq!(schema["minimum"], 0);
        assert_eq!(schema["nullable"], true);
    }

    #[test]
    fn preserves_non_null_anyof() {
        let original = json!({
            "anyOf": [
                {"type": "string"},
                {"type": "integer"}
            ]
        });
        let mut schema = original.clone();
        sanitize(&mut schema);
        // Multi-typed anyOf should collapse to single element after
        // recursive walk — but here neither is null so the collapse
        // doesn't trigger. The anyOf array itself remains.
        assert!(schema.get("anyOf").is_some());
    }

    #[test]
    fn injects_properties_on_bare_object() {
        let mut schema = json!({"type": "object"});
        sanitize(&mut schema);
        assert!(schema.get("properties").is_some());
        assert_eq!(schema["properties"], json!({}));
    }

    #[test]
    fn does_not_inject_properties_when_present() {
        let mut schema = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}}
        });
        let expected = schema.clone();
        sanitize(&mut schema);
        assert_eq!(schema, expected);
    }

    #[test]
    fn prunes_dangling_required() {
        let mut schema = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name", "email"]
        });
        sanitize(&mut schema);
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "name");
    }

    #[test]
    fn removes_required_when_all_pruned() {
        let mut schema = json!({
            "type": "object",
            "properties": {},
            "required": ["ghost"]
        });
        sanitize(&mut schema);
        assert!(schema.get("required").is_none());
    }

    /// #6561 D04-11: composition branches name the parent's properties.
    #[test]
    fn keeps_required_groups_of_composition_branches() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "patch": {"type": "string"},
                "replace": {"type": "array"},
                "changes": {"type": "array"}
            },
            "oneOf": [
                {"required": ["patch"]},
                {"required": ["replace"]},
                {"required": ["changes"]}
            ]
        });
        sanitize(&mut schema);
        assert_eq!(
            schema["oneOf"],
            json!([
                {"required": ["patch"]},
                {"required": ["replace"]},
                {"required": ["changes"]}
            ])
        );
        // A typed object still has its dangling names pruned.
        let mut typed = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name", "ghost"]
        });
        sanitize(&mut typed);
        assert_eq!(typed["required"], json!(["name"]));
    }

    #[test]
    fn collapses_single_element_oneof() {
        let mut schema = json!({
            "oneOf": [{"type": "string", "minLength": 1}]
        });
        sanitize(&mut schema);
        assert!(schema.get("oneOf").is_none());
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["minLength"], 1);
    }

    #[test]
    fn collapses_single_element_anyof() {
        let mut schema = json!({
            "anyOf": [{"type": "boolean"}]
        });
        sanitize(&mut schema);
        assert!(schema.get("anyOf").is_none());
        assert_eq!(schema["type"], "boolean");
    }

    #[test]
    fn recursive_walk_into_properties() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "opt_name": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "null"}
                    ]
                }
            }
        });
        sanitize(&mut schema);
        let prop = &schema["properties"]["opt_name"];
        assert_eq!(prop["type"], "string");
        assert_eq!(prop["nullable"], true);
    }

    #[test]
    fn recursive_walk_into_items() {
        let mut schema = json!({
            "type": "array",
            "items": {
                "anyOf": [
                    {"type": "integer"},
                    {"type": "null"}
                ]
            }
        });
        sanitize(&mut schema);
        let items = &schema["items"];
        assert_eq!(items["type"], "integer");
        assert_eq!(items["nullable"], true);
    }

    #[test]
    fn nested_anyof_in_nullable_union_stays_structural() {
        // Pydantic can nest unions: Optional[Union[str, int]].
        let mut schema = json!({
            "anyOf": [
                {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "integer"}
                    ]
                },
                {"type": "null"}
            ]
        });
        sanitize(&mut schema);
        // The non-null branch has no concrete type of its own. Collapsing the
        // outer union would manufacture an annotation-only `nullable` schema,
        // so retain both levels and their explicit null termination.
        assert!(schema.get("nullable").is_none());
        assert_eq!(schema["anyOf"][1], json!({"type": "null"}));
        assert_eq!(
            schema["anyOf"][0]["anyOf"],
            json!([{"type": "string"}, {"type": "integer"}])
        );
    }

    #[test]
    fn idempotent() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "maybe": {
                    "anyOf": [{"type": "integer"}, {"type": "null"}]
                }
            },
            "required": ["name", "missing_field"]
        });
        sanitize(&mut schema);
        let after_first = schema.clone();
        sanitize(&mut schema);
        assert_eq!(schema, after_first, "sanitize must be idempotent");
    }

    #[test]
    fn strict_sanitize_requires_all_object_properties_and_closes_extra_keys() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"}
            },
            "required": ["name"],
            "additionalProperties": {"type": "string"}
        });

        sanitize_for_strict(&mut schema);

        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["count", "name"]));
        assert_eq!(schema["properties"]["count"]["nullable"], true);
        assert!(schema["properties"]["name"].get("nullable").is_none());
    }

    #[test]
    fn strict_sanitize_preserves_optional_properties_as_nullable() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer"},
                "max_lines": {"type": "integer"},
                "options": {
                    "type": "object",
                    "properties": {
                        "encoding": {"type": "string"},
                        "trim": {"type": "boolean"}
                    },
                    "required": ["encoding"]
                }
            },
            "required": ["path", "options"]
        });

        sanitize_for_strict(&mut schema);

        assert_eq!(
            schema["required"],
            json!(["max_lines", "options", "path", "start_line"])
        );
        assert!(schema["properties"]["path"].get("nullable").is_none());
        assert!(schema["properties"]["options"].get("nullable").is_none());
        assert_eq!(schema["properties"]["start_line"]["nullable"], true);
        assert_eq!(schema["properties"]["max_lines"]["nullable"], true);
        assert_eq!(
            schema["properties"]["options"]["required"],
            json!(["encoding", "trim"])
        );
        assert!(
            schema["properties"]["options"]["properties"]["encoding"]
                .get("nullable")
                .is_none()
        );
        assert_eq!(
            schema["properties"]["options"]["properties"]["trim"]["nullable"],
            true
        );
    }

    #[test]
    fn strict_sanitize_applies_object_rules_recursively() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": {
                        "inner": {"type": "string"}
                    },
                    "required": []
                }
            },
            "required": []
        });

        sanitize_for_strict(&mut schema);

        assert_eq!(schema["required"], json!(["outer"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["outer"]["required"], json!(["inner"]));
        assert_eq!(schema["properties"]["outer"]["additionalProperties"], false);
    }

    #[test]
    fn strict_sanitize_removes_unsupported_string_and_array_bounds() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 64,
                    "pattern": "^[a-z]+$"
                },
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 5,
                    "items": {"type": "string"}
                },
                "score": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 5
                }
            }
        });

        sanitize_for_strict(&mut schema);

        let name = &schema["properties"]["name"];
        assert!(name.get("minLength").is_none());
        assert!(name.get("maxLength").is_none());
        assert_eq!(name["pattern"], "^[a-z]+$");

        let items = &schema["properties"]["items"];
        assert!(items.get("minItems").is_none());
        assert!(items.get("maxItems").is_none());

        let score = &schema["properties"]["score"];
        assert_eq!(score["minimum"], 1);
        assert_eq!(score["maximum"], 5);
    }

    #[test]
    fn responses_sanitize_removes_root_composition_from_apply_patch_shape() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "patch": {"type": "string"},
                "replace": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "content": {"type": "string"}
                        },
                        "required": ["path", "content"]
                    }
                },
                "changes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "content": {"type": "string"}
                        },
                        "required": ["path", "content"]
                    }
                }
            },
            "oneOf": [
                {"required": ["patch"]},
                {"required": ["replace"]},
                {"required": ["changes"]}
            ]
        });

        let note = sanitize_for_responses(&mut schema);

        assert_eq!(schema["type"], "object");
        assert!(schema.get("oneOf").is_none());
        assert!(schema.get("anyOf").is_none());
        assert!(schema.get("allOf").is_none());
        assert!(schema.get("enum").is_none());
        assert!(schema.get("not").is_none());
        assert!(schema["properties"].get("patch").is_some());
        assert!(schema["properties"].get("replace").is_some());
        assert!(schema["properties"].get("changes").is_some());
        assert_eq!(
            note.as_deref(),
            Some(
                "Exactly one of these parameter groups must be provided: `changes` | `patch` | `replace`."
            )
        );
    }

    #[test]
    fn responses_sanitize_merges_root_alternative_properties() {
        let mut schema = json!({
            "anyOf": [
                {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                },
                {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string"}
                    },
                    "required": ["url"]
                }
            ]
        });

        let note = sanitize_for_responses(&mut schema);

        assert_eq!(schema["type"], "object");
        assert!(schema.get("anyOf").is_none());
        assert!(schema["properties"].get("path").is_some());
        assert!(schema["properties"].get("url").is_some());
        assert!(schema.get("required").is_none());
        assert_eq!(
            note.as_deref(),
            Some("At least one of these parameter groups must be provided: `path` | `url`.")
        );
    }

    #[test]
    fn responses_sanitize_preserves_nested_alternatives() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "integer"}
                    ]
                }
            }
        });

        let note = sanitize_for_responses(&mut schema);

        assert_eq!(schema["type"], "object");
        assert!(schema.get("anyOf").is_none());
        assert!(schema["properties"]["value"].get("anyOf").is_some());
        assert_eq!(note, None);
    }

    #[test]
    fn responses_sanitize_plain_object_has_no_constraint_note() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        });

        let note = sanitize_for_responses(&mut schema);

        assert_eq!(schema["type"], "object");
        assert_eq!(note, None);
    }

    #[test]
    fn responses_constraint_note_is_sorted_and_deduped() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"},
                "c": {"type": "string"}
            },
            "oneOf": [
                {"required": ["b", "a", "a"]},
                {"required": ["c"]},
                {"required": ["a", "b"]}
            ]
        });

        let note = sanitize_for_responses(&mut schema);

        assert_eq!(
            note.as_deref(),
            Some("Exactly one of these parameter groups must be provided: `a` + `b` | `c`.")
        );
    }
}

/// Normalize a tool's function schema for Kimi / Moonshot API compatibility.
///
/// Kimi's API enforces stricter JSON Schema validation: when a schema uses
/// `anyOf` / `oneOf`, the `type` field must be placed inside each item rather
/// than on the parent object.  This function walks the schema root and any
/// nested objects, pushing `"type": "object"` down into `anyOf` / `oneOf`
/// items when present.
///
/// Invariant: only mutates objects that carry a top-level `type` + an
/// `anyOf` or `oneOf` array — pure schemas without conditional alternatives
/// are left untouched.
pub fn sanitize_for_kimi(schema: &mut serde_json::Value) {
    if let Some(obj) = schema.as_object_mut() {
        // Recurse first so a type injected into this object's alternatives is
        // not immediately removed again by processing that freshly-mutated item.
        for map_key in ["properties", "$defs"] {
            if let Some(children) = obj.get_mut(map_key).and_then(Value::as_object_mut) {
                for child in children.values_mut() {
                    sanitize_for_kimi(child);
                }
            }
        }
        if let Some(items) = obj.get_mut("items") {
            sanitize_for_kimi(items);
        }
        if let Some(additional) = obj.get_mut("additionalProperties")
            && additional.is_object()
        {
            sanitize_for_kimi(additional);
        }
        for union_key in ["anyOf", "oneOf"] {
            if let Some(branches) = obj.get_mut(union_key).and_then(Value::as_array_mut) {
                for branch in branches {
                    sanitize_for_kimi(branch);
                }
            }
        }

        // If this object has `type` + `anyOf`/`oneOf`, push `type` into
        // each item and remove it from the parent. Otherwise leave it alone.
        let should_push =
            obj.contains_key("type") && (obj.contains_key("anyOf") || obj.contains_key("oneOf"));
        if should_push && let Some(type_val) = obj.remove("type") {
            for key in ["anyOf", "oneOf"] {
                if let Some(items) = obj.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for item in items {
                        if let Some(item_obj) = item.as_object_mut()
                            && !item_obj.contains_key("type")
                        {
                            item_obj.insert("type".to_string(), type_val.clone());
                        }
                    }
                }
            }
            // The provider-neutral sanitizer injects an empty `properties`
            // map on every bare object before this provider pass. MFJS permits
            // only annotations beside `anyOf`, so remove that semantic no-op
            // after moving the object type into each branch.
            if obj
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(Map::is_empty)
            {
                obj.remove("properties");
            }
        }
    }
}

/// A safe, provider-facing reason that Kimi parameters could not be emitted.
///
/// These diagnostics deliberately never include the schema or `$ref` value:
/// tool schemas can be supplied by MCP servers and may contain private data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KimiParameterSchemaError {
    #[error("Moonshot function parameters root must be a JSON object schema")]
    RootMustBeObject,
    #[error("Moonshot function parameters contain an unsupported root reference")]
    UnsupportedRootReference,
    #[error("Moonshot function parameters contain an unresolved internal root reference")]
    UnresolvedRootReference,
    #[error("Moonshot function parameters contain a cyclic internal root reference")]
    CyclicRootReference,
    #[error("Moonshot function parameters root reference must resolve to an object schema")]
    ReferencedRootMustBeObject,
    #[error("Moonshot function parameters contain unsupported nested allOf composition")]
    UnsupportedNestedAllOf,
    #[error("Moonshot function parameters contain conflicting nested union composition")]
    ConflictingNestedUnion,
    #[error("Moonshot function parameters contain an unsupported const literal")]
    UnsupportedConstLiteral,
    #[error("Moonshot function parameters contain conflicting literal constraints")]
    ConflictingLiteralConstraint,
    #[error("Moonshot function parameters contain an invalid nullable marker")]
    InvalidNullable,
    #[error("Moonshot function parameters contain an unsupported MFJS keyword")]
    UnsupportedKeyword,
    #[error("Moonshot function parameters contain an invalid MFJS schema node")]
    InvalidSchemaNode,
    #[error("Moonshot function parameters contain an invalid MFJS keyword value")]
    InvalidKeywordValue,
    #[error("Moonshot function parameters contain an invalid MFJS reference")]
    InvalidReference,
    #[error("Moonshot function parameters contain an MFJS schema without a concrete type")]
    MissingType,
    #[error("Moonshot function parameters exceed an MFJS resource limit")]
    ResourceLimitExceeded,
    #[error("Moonshot function parameters contain a non-terminating MFJS reference")]
    NonTerminatingReference,
    #[error("Moonshot function parameters contain an invalid MFJS default")]
    InvalidDefault,
    #[error("Moonshot function parameters contain an invalid MFJS range")]
    InvalidRange,
}

/// Normalize a complete Kimi / Moonshot `function.parameters` object.
///
/// Function parameters have an additional MFJS constraint: the root must end
/// as a plain `type: "object"` schema. Root composition is flattened using the
/// same compatibility pass as Responses and xAI, while supported nested
/// `anyOf` branches remain nested. Internal root `$ref` values are resolved and
/// inlined before normalization so we never manufacture the invalid
/// `type + allOf($ref)` shape rejected by MFJS.
///
/// Unsupported, unresolved, cyclic, and non-object root references fail
/// closed with a non-secret diagnostic instead of being sent to Moonshot.
///
/// MFJS differences from JSON Schema:
/// <https://github.com/MoonshotAI/walle/blob/main/docs/mfjs-walle-vs-draft-2020-12.md>
pub fn sanitize_for_kimi_parameters(
    parameters: &mut Value,
) -> Result<Option<String>, KimiParameterSchemaError> {
    // Work on a clone so a rejected schema remains byte-for-byte unchanged for
    // callers that retain the catalog and retry against another provider.
    let mut candidate = parameters.clone();
    let constraint_note = sanitize_kimi_parameters_candidate(&mut candidate)?;
    validate_mfjs_parameters(&candidate)?;
    *parameters = candidate;
    Ok(constraint_note)
}

fn sanitize_kimi_parameters_candidate(
    parameters: &mut Value,
) -> Result<Option<String>, KimiParameterSchemaError> {
    let Some(root) = parameters.as_object() else {
        return Err(KimiParameterSchemaError::RootMustBeObject);
    };
    if root
        .get("type")
        .is_some_and(|schema_type| schema_type != "object")
    {
        return Err(KimiParameterSchemaError::RootMustBeObject);
    }

    inline_internal_kimi_root_ref(parameters)?;

    // A function schema's root cannot carry MFJS composition because it must
    // simultaneously be `type: object`. Flatten the actual root shape used by
    // apply_patch and retain its dropped required-group contract as a prompt
    // note for the model.
    // MFJS has no conditional keywords. The shared compatibility pass drops
    // them with a bounded description note instead of failing the whole turn.
    let constraint_note = sanitize_for_responses(parameters);

    // Restore nullable unions collapsed by the registry's provider-neutral
    // sanitizer, translate MFJS-safe scalar const values, and normalize nested
    // composition. This changes model-facing schema guidance only; each tool
    // keeps responsibility for validating its own arguments. allOf fails closed.
    normalize_kimi_compatibility(parameters, true)?;

    // MFJS requires `type` to live inside each anyOf branch, never alongside
    // the union keyword. The root is composition-free at this point, so this
    // only adjusts valid nested unions.
    sanitize_for_kimi(parameters);

    // JSON Schema reads an empty `items` schema as "items are unconstrained",
    // which is exactly the MFJS-legal shape with the keyword absent. MFJS
    // itself has no empty schema node and fails the whole tool with
    // MissingType, so repair the equivalent shape instead of dropping the
    // tool from the request.
    remove_empty_mfjs_items_schemas(parameters);

    let Some(root) = parameters.as_object() else {
        return Err(KimiParameterSchemaError::RootMustBeObject);
    };
    if root.get("type").and_then(Value::as_str) != Some("object")
        || root.contains_key("anyOf")
        || root.contains_key("oneOf")
        || root.contains_key("allOf")
        || root.contains_key("$ref")
    {
        return Err(KimiParameterSchemaError::RootMustBeObject);
    }

    Ok(constraint_note)
}

/// Drop every empty `items` schema node (`"items": {}`) from a Kimi
/// parameters document.
///
/// Empty-schema nodes sit exactly where schema values live: property values,
/// `items`, object-form `additionalProperties`, `anyOf` branches, and `$defs`
/// entries. `properties` itself is a name map, so its own emptiness is legal
/// and only its values are visited. `oneOf` needs no visit: this runs after
/// `normalize_kimi_compatibility` has converted every non-root `oneOf` to
/// `anyOf`, and root composition was already flattened before that.
///
/// Children are visited before the node's own `items` check so a nested
/// repair can empty the parent (`"items": {"items": {}}` is vacuously true
/// once the inner keyword is gone, so dropping the outer keyword is still
/// semantics-preserving). Every removal keeps the document equivalent: an
/// absent `items` keyword already means unconstrained items, so a legal
/// document passes through unchanged.
fn remove_empty_mfjs_items_schemas(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if let Some(properties) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for child in properties.values_mut() {
            remove_empty_mfjs_items_schemas(child);
        }
    }
    if let Some(definitions) = obj.get_mut("$defs").and_then(Value::as_object_mut) {
        for child in definitions.values_mut() {
            remove_empty_mfjs_items_schemas(child);
        }
    }
    if let Some(branches) = obj.get_mut("anyOf").and_then(Value::as_array_mut) {
        for branch in branches.iter_mut() {
            remove_empty_mfjs_items_schemas(branch);
        }
    }
    if let Some(additional) = obj.get_mut("additionalProperties")
        && additional.is_object()
    {
        remove_empty_mfjs_items_schemas(additional);
    }
    if let Some(items) = obj.get_mut("items") {
        remove_empty_mfjs_items_schemas(items);
    }
    if obj
        .get("items")
        .is_some_and(|items| items.as_object().is_some_and(Map::is_empty))
    {
        obj.shift_remove("items");
    }
}

fn inline_internal_kimi_root_ref(parameters: &mut Value) -> Result<(), KimiParameterSchemaError> {
    let document = parameters.clone();
    let Some(document_root) = document.as_object() else {
        return Err(KimiParameterSchemaError::RootMustBeObject);
    };
    let Some(root_ref) = document_root.get("$ref") else {
        return Ok(());
    };
    let Some(mut reference) = root_ref.as_str() else {
        return Err(KimiParameterSchemaError::UnsupportedRootReference);
    };

    let mut visited = HashSet::new();
    let resolved = loop {
        if !reference.starts_with("#/") {
            return Err(KimiParameterSchemaError::UnsupportedRootReference);
        }
        if !visited.insert(reference.to_string()) {
            return Err(KimiParameterSchemaError::CyclicRootReference);
        }
        let target = document
            .pointer(&reference[1..])
            .ok_or(KimiParameterSchemaError::UnresolvedRootReference)?;
        let target = target
            .as_object()
            .ok_or(KimiParameterSchemaError::ReferencedRootMustBeObject)?;
        if let Some(next_ref) = target.get("$ref") {
            reference = next_ref
                .as_str()
                .ok_or(KimiParameterSchemaError::UnsupportedRootReference)?;
            continue;
        }
        if target.get("type").and_then(Value::as_str) != Some("object") {
            return Err(KimiParameterSchemaError::ReferencedRootMustBeObject);
        }
        break target.clone();
    };

    let mut inlined = resolved;
    for (key, value) in document_root {
        if key != "$ref" {
            inlined.insert(key.clone(), value.clone());
        }
    }
    *parameters = Value::Object(inlined);
    Ok(())
}

fn normalize_kimi_compatibility(
    schema: &mut Value,
    is_root: bool,
) -> Result<(), KimiParameterSchemaError> {
    let Some(obj) = schema.as_object_mut() else {
        return Err(KimiParameterSchemaError::InvalidSchemaNode);
    };

    if !is_root {
        if obj.contains_key("allOf") {
            return Err(KimiParameterSchemaError::UnsupportedNestedAllOf);
        }
        if let Some(one_of) = obj.remove("oneOf") {
            if obj.contains_key("anyOf") {
                return Err(KimiParameterSchemaError::ConflictingNestedUnion);
            }
            obj.insert("anyOf".to_string(), one_of);
        }
    }

    if let Some(constant) = obj.remove("const") {
        if !is_mfjs_enum_literal(&constant) {
            return Err(KimiParameterSchemaError::UnsupportedConstLiteral);
        }
        if !obj.contains_key("type") {
            let inferred = mfjs_literal_kind(&constant)
                .ok_or(KimiParameterSchemaError::UnsupportedConstLiteral)?;
            let schema_type = match inferred {
                MfjsLiteralKind::Integer => "integer",
                MfjsLiteralKind::Number => "number",
                MfjsLiteralKind::String => "string",
            };
            obj.insert("type".to_string(), Value::String(schema_type.to_string()));
        }
        if let Some(existing) = obj.get("enum") {
            let agrees = existing
                .as_array()
                .is_some_and(|values| values.as_slice() == [constant.clone()]);
            if !agrees {
                return Err(KimiParameterSchemaError::ConflictingLiteralConstraint);
            }
        } else {
            obj.insert("enum".to_string(), Value::Array(vec![constant]));
        }
    }

    let nullable = obj.remove("nullable");
    if nullable.is_some()
        && !obj
            .get("type")
            .is_some_and(|schema_type| schema_type.as_str().is_some_and(is_mfjs_concrete_type))
    {
        return Err(KimiParameterSchemaError::InvalidNullable);
    }
    match nullable.as_ref().map(Value::as_bool) {
        None => {}
        Some(Some(false)) => {}
        Some(Some(true)) if is_root => {
            // Function parameters are required to be an object at the root;
            // null was never a valid wire instance there.
        }
        Some(Some(true)) => {
            let non_null = Value::Object(std::mem::take(obj));
            *schema = serde_json::json!({
                "anyOf": [non_null, {"type": "null"}]
            });
        }
        Some(None) => return Err(KimiParameterSchemaError::InvalidNullable),
    }

    normalize_kimi_child_schemas(schema)?;
    Ok(())
}

fn normalize_kimi_child_schemas(schema: &mut Value) -> Result<(), KimiParameterSchemaError> {
    let Some(obj) = schema.as_object_mut() else {
        return Err(KimiParameterSchemaError::InvalidSchemaNode);
    };

    for map_key in ["properties", "$defs"] {
        if let Some(children) = obj.get_mut(map_key).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                normalize_kimi_compatibility(child, false)?;
            }
        }
    }

    if let Some(items) = obj.get_mut("items") {
        normalize_kimi_compatibility(items, false)?;
    }
    if let Some(additional) = obj.get_mut("additionalProperties")
        && additional.is_object()
    {
        normalize_kimi_compatibility(additional, false)?;
    }
    if let Some(branches) = obj.get_mut("anyOf").and_then(Value::as_array_mut) {
        for branch in branches {
            normalize_kimi_compatibility(branch, false)?;
        }
    }
    Ok(())
}

fn is_mfjs_enum_literal(value: &Value) -> bool {
    value.is_string() || value.is_number()
}

/// Validate one fully normalized MFJS function-parameters schema.
///
/// Every error is a fixed enum variant: schemas can originate in MCP or
/// runtime tools and may contain private names or values, so diagnostics must
/// never echo a keyword, property, reference, or literal from the document.
pub fn validate_mfjs_parameters(parameters: &Value) -> Result<(), KimiParameterSchemaError> {
    let root = parameters
        .as_object()
        .ok_or(KimiParameterSchemaError::RootMustBeObject)?;
    if root.get("type").and_then(Value::as_str) != Some("object")
        || root.contains_key("anyOf")
        || root.contains_key("oneOf")
        || root.contains_key("allOf")
        || root.contains_key("$ref")
    {
        return Err(KimiParameterSchemaError::RootMustBeObject);
    }
    if serde_json::to_vec(parameters)
        .map(|encoded| encoded.len() > MFJS_MAX_SCHEMA_BYTES)
        .unwrap_or(true)
    {
        return Err(KimiParameterSchemaError::ResourceLimitExceeded);
    }

    let mut state = MfjsValidationState::new(parameters);
    state.validate_schema(parameters, true, false, 0, 0)?;

    let mut visiting_refs = HashSet::new();
    if !mfjs_schema_can_terminate(parameters, parameters, &mut visiting_refs, 0)? {
        return Err(KimiParameterSchemaError::NonTerminatingReference);
    }

    validate_mfjs_expanded_depth(parameters, parameters, 0, &mut HashSet::new(), 0)?;
    if let Some(definitions) = root.get("$defs").and_then(Value::as_object) {
        for definition in definitions.values() {
            validate_mfjs_expanded_depth(definition, parameters, 0, &mut HashSet::new(), 0)?;
        }
    }
    Ok(())
}

const MFJS_MAX_ANY_OF_ITEMS: usize = 10;
const MFJS_MAX_OBJECT_DEPTH: usize = 5;
const MFJS_MAX_TOTAL_PROPERTIES: usize = 100;
const MFJS_MAX_TOTAL_ENUM_VALUES: usize = 500;
const MFJS_ENUM_LENGTH_CHECK_THRESHOLD: usize = 250;
const MFJS_MAX_ENUM_STRING_LENGTH: usize = 7_500;
const MFJS_MAX_SCHEMA_BYTES: usize = 120_000;
const MFJS_MAX_STRUCTURAL_DEPTH: usize = 64;
const MFJS_MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

struct MfjsValidationState<'a> {
    document: &'a Value,
    total_properties: usize,
    total_enum_values: usize,
}

impl<'a> MfjsValidationState<'a> {
    fn new(document: &'a Value) -> Self {
        Self {
            document,
            total_properties: 0,
            total_enum_values: 0,
        }
    }

    fn validate_schema(
        &mut self,
        schema: &Value,
        is_root: bool,
        allow_empty: bool,
        property_depth: usize,
        structural_depth: usize,
    ) -> Result<(), KimiParameterSchemaError> {
        if structural_depth > MFJS_MAX_STRUCTURAL_DEPTH {
            return Err(KimiParameterSchemaError::ResourceLimitExceeded);
        }
        let obj = schema
            .as_object()
            .ok_or(KimiParameterSchemaError::InvalidSchemaNode)?;
        if obj.is_empty() {
            return if allow_empty {
                Ok(())
            } else {
                Err(KimiParameterSchemaError::MissingType)
            };
        }

        const ALLOWED_KEYWORDS: &[&str] = &[
            "$id",
            "$ref",
            "$defs",
            "anyOf",
            "properties",
            "additionalProperties",
            "items",
            "type",
            "enum",
            "required",
            "maxLength",
            "minLength",
            "maximum",
            "minimum",
            "maxItems",
            "minItems",
            "title",
            "description",
            "default",
        ];
        if obj
            .keys()
            .any(|keyword| !ALLOWED_KEYWORDS.contains(&keyword.as_str()))
        {
            return Err(KimiParameterSchemaError::UnsupportedKeyword);
        }

        for annotation in ["title", "description"] {
            if obj.get(annotation).is_some_and(|value| !value.is_string()) {
                return Err(KimiParameterSchemaError::InvalidKeywordValue);
            }
        }
        if obj.get("$id").is_some_and(|value| !value.is_string())
            || (!is_root && obj.contains_key("$id"))
        {
            return Err(KimiParameterSchemaError::InvalidKeywordValue);
        }

        if let Some(definitions) = obj.get("$defs") {
            if !is_root {
                return Err(KimiParameterSchemaError::InvalidKeywordValue);
            }
            let definitions = definitions
                .as_object()
                .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
            for (name, definition) in definitions {
                if name.is_empty() || name.contains('/') {
                    return Err(KimiParameterSchemaError::InvalidKeywordValue);
                }
                self.validate_schema(definition, false, false, 0, structural_depth + 1)?;
            }
        }

        if let Some(reference) = obj.get("$ref") {
            let reference = reference
                .as_str()
                .ok_or(KimiParameterSchemaError::InvalidReference)?;
            if resolve_mfjs_reference(self.document, reference).is_none() {
                return Err(KimiParameterSchemaError::InvalidReference);
            }
            let allowed_ref_sibling = |key: &str| {
                matches!(key, "$ref" | "title" | "description")
                    || (is_root && matches!(key, "$defs" | "$id"))
            };
            if obj.keys().any(|key| !allowed_ref_sibling(key)) {
                return Err(KimiParameterSchemaError::InvalidReference);
            }
            return Ok(());
        }

        if let Some(any_of) = obj.get("anyOf") {
            let branches = any_of
                .as_array()
                .filter(|branches| !branches.is_empty() && branches.len() <= MFJS_MAX_ANY_OF_ITEMS)
                .ok_or(KimiParameterSchemaError::ResourceLimitExceeded)?;
            let allowed_union_sibling = |key: &str| {
                matches!(key, "anyOf" | "title" | "description")
                    || (is_root && matches!(key, "$defs" | "$id"))
            };
            if obj.keys().any(|key| !allowed_union_sibling(key)) {
                return Err(KimiParameterSchemaError::InvalidKeywordValue);
            }
            for branch in branches {
                self.validate_schema(branch, false, false, property_depth, structural_depth + 1)?;
            }
            return Ok(());
        }

        let schema_type = obj
            .get("type")
            .and_then(Value::as_str)
            .filter(|schema_type| is_mfjs_concrete_type(schema_type))
            .ok_or(KimiParameterSchemaError::MissingType)?;
        if obj
            .keys()
            .any(|keyword| !mfjs_keyword_allowed_for_type(keyword, schema_type, is_root))
        {
            return Err(KimiParameterSchemaError::InvalidKeywordValue);
        }

        if let Some(values) = obj.get("enum") {
            validate_mfjs_enum(values, schema_type, self)?;
        }
        if let Some(default) = obj.get("default") {
            validate_mfjs_default(default, schema_type)?;
        }

        if let Some(properties) = obj.get("properties") {
            let properties = properties
                .as_object()
                .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
            self.total_properties = self
                .total_properties
                .checked_add(properties.len())
                .ok_or(KimiParameterSchemaError::ResourceLimitExceeded)?;
            if self.total_properties > MFJS_MAX_TOTAL_PROPERTIES {
                return Err(KimiParameterSchemaError::ResourceLimitExceeded);
            }
            for (name, property) in properties {
                if name.is_empty()
                    || matches!(
                        name.as_str(),
                        "$defs" | "$ref" | "anyOf" | "required" | "additionalProperties"
                    )
                {
                    return Err(KimiParameterSchemaError::InvalidKeywordValue);
                }
                let child_depth = property_depth
                    .checked_add(1)
                    .ok_or(KimiParameterSchemaError::ResourceLimitExceeded)?;
                if child_depth > MFJS_MAX_OBJECT_DEPTH {
                    return Err(KimiParameterSchemaError::ResourceLimitExceeded);
                }
                self.validate_schema(property, false, false, child_depth, structural_depth + 1)?;
            }
        }

        if let Some(required) = obj.get("required") {
            let required = required
                .as_array()
                .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
            let properties = obj
                .get("properties")
                .and_then(Value::as_object)
                .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
            let mut seen = HashSet::new();
            for name in required {
                let name = name
                    .as_str()
                    .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
                if name.is_empty() || !properties.contains_key(name) || !seen.insert(name) {
                    return Err(KimiParameterSchemaError::InvalidKeywordValue);
                }
            }
        }

        if let Some(additional) = obj.get("additionalProperties") {
            match additional {
                Value::Bool(_) => {}
                Value::Object(_) => self.validate_schema(
                    additional,
                    false,
                    true,
                    property_depth,
                    structural_depth + 1,
                )?,
                _ => return Err(KimiParameterSchemaError::InvalidKeywordValue),
            }
        }

        if let Some(items) = obj.get("items") {
            self.validate_schema(items, false, false, property_depth, structural_depth + 1)?;
        }

        validate_mfjs_bounds(obj, schema_type)?;
        Ok(())
    }
}

fn is_mfjs_concrete_type(schema_type: &str) -> bool {
    matches!(
        schema_type,
        "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
    )
}

fn mfjs_keyword_allowed_for_type(keyword: &str, schema_type: &str, is_root: bool) -> bool {
    if is_root && matches!(keyword, "$defs" | "$id") {
        return true;
    }
    if matches!(keyword, "type" | "title" | "description") {
        return true;
    }
    match schema_type {
        "object" => matches!(keyword, "properties" | "required" | "additionalProperties"),
        "array" => matches!(keyword, "items" | "minItems" | "maxItems"),
        "string" => matches!(keyword, "enum" | "default" | "minLength" | "maxLength"),
        "number" | "integer" => {
            matches!(keyword, "enum" | "default" | "minimum" | "maximum")
        }
        "boolean" | "null" => keyword == "default",
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MfjsLiteralKind {
    Integer,
    Number,
    String,
}

fn mfjs_literal_kind(value: &Value) -> Option<MfjsLiteralKind> {
    if value.is_string() {
        return Some(MfjsLiteralKind::String);
    }
    let number = value.as_number()?;
    if number.is_i64() || number.is_u64() {
        Some(MfjsLiteralKind::Integer)
    } else {
        Some(MfjsLiteralKind::Number)
    }
}

fn validate_mfjs_enum(
    value: &Value,
    schema_type: &str,
    state: &mut MfjsValidationState<'_>,
) -> Result<(), KimiParameterSchemaError> {
    let values = value
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
    state.total_enum_values = state
        .total_enum_values
        .checked_add(values.len())
        .ok_or(KimiParameterSchemaError::ResourceLimitExceeded)?;
    if state.total_enum_values > MFJS_MAX_TOTAL_ENUM_VALUES {
        return Err(KimiParameterSchemaError::ResourceLimitExceeded);
    }

    let values_match_type = match schema_type {
        "string" => values.iter().all(Value::is_string),
        "integer" => values
            .iter()
            .all(|value| mfjs_integer_value(value).is_some()),
        "number" => values.iter().all(Value::is_number),
        _ => false,
    };
    if !values_match_type {
        return Err(KimiParameterSchemaError::InvalidKeywordValue);
    }

    if values.len() > MFJS_ENUM_LENGTH_CHECK_THRESHOLD {
        let encoded_length = values.iter().try_fold(0usize, |total, value| {
            let literal_length = value
                .as_str()
                .map(str::len)
                .unwrap_or_else(|| value.to_string().len());
            total.checked_add(literal_length)
        });
        if encoded_length.is_none_or(|length| length > MFJS_MAX_ENUM_STRING_LENGTH) {
            return Err(KimiParameterSchemaError::ResourceLimitExceeded);
        }
    }
    Ok(())
}

fn validate_mfjs_default(value: &Value, schema_type: &str) -> Result<(), KimiParameterSchemaError> {
    let valid = match schema_type {
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => mfjs_integer_value(value).is_some(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(KimiParameterSchemaError::InvalidDefault)
    }
}

fn mfjs_integer_value(value: &Value) -> Option<f64> {
    let value = value.as_number()?.as_f64()?;
    (value.is_finite() && value.fract() == 0.0 && value.abs() <= MFJS_MAX_SAFE_INTEGER)
        .then_some(value)
}

fn validate_mfjs_bounds(
    obj: &Map<String, Value>,
    schema_type: &str,
) -> Result<(), KimiParameterSchemaError> {
    validate_mfjs_unsigned_range(obj, schema_type, "string", "minLength", "maxLength")?;
    validate_mfjs_unsigned_range(obj, schema_type, "array", "minItems", "maxItems")?;

    let minimum = obj.get("minimum");
    let maximum = obj.get("maximum");
    if minimum.is_some() || maximum.is_some() {
        let parse_bound = |value: &Value| match schema_type {
            "integer" => mfjs_integer_value(value),
            "number" => value.as_number().and_then(serde_json::Number::as_f64),
            _ => None,
        };
        let minimum = minimum
            .map(|value| parse_bound(value).ok_or(KimiParameterSchemaError::InvalidRange))
            .transpose()?;
        let maximum = maximum
            .map(|value| parse_bound(value).ok_or(KimiParameterSchemaError::InvalidRange))
            .transpose()?;
        if minimum.zip(maximum).is_some_and(|(min, max)| min > max) {
            return Err(KimiParameterSchemaError::InvalidRange);
        }
    }
    Ok(())
}

fn validate_mfjs_unsigned_range(
    obj: &Map<String, Value>,
    schema_type: &str,
    expected_type: &str,
    minimum_keyword: &str,
    maximum_keyword: &str,
) -> Result<(), KimiParameterSchemaError> {
    let minimum = obj.get(minimum_keyword);
    let maximum = obj.get(maximum_keyword);
    if minimum.is_none() && maximum.is_none() {
        return Ok(());
    }
    if schema_type != expected_type {
        return Err(KimiParameterSchemaError::InvalidRange);
    }
    let minimum = minimum
        .map(|value| value.as_u64().ok_or(KimiParameterSchemaError::InvalidRange))
        .transpose()?;
    let maximum = maximum
        .map(|value| value.as_u64().ok_or(KimiParameterSchemaError::InvalidRange))
        .transpose()?;
    if minimum.zip(maximum).is_some_and(|(min, max)| min > max) {
        return Err(KimiParameterSchemaError::InvalidRange);
    }
    Ok(())
}

fn resolve_mfjs_reference<'a>(document: &'a Value, reference: &str) -> Option<&'a Value> {
    if reference == "#" {
        return Some(document);
    }
    let name = reference.strip_prefix("#/$defs/")?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    document
        .pointer(&reference[1..])
        .filter(|target| target.is_object())
}

fn mfjs_schema_can_terminate(
    schema: &Value,
    document: &Value,
    visiting_refs: &mut HashSet<String>,
    structural_depth: usize,
) -> Result<bool, KimiParameterSchemaError> {
    if structural_depth > MFJS_MAX_STRUCTURAL_DEPTH {
        return Err(KimiParameterSchemaError::ResourceLimitExceeded);
    }
    let obj = schema
        .as_object()
        .ok_or(KimiParameterSchemaError::InvalidSchemaNode)?;

    if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
        if !visiting_refs.insert(reference.to_string()) {
            return Ok(false);
        }
        let target = resolve_mfjs_reference(document, reference)
            .ok_or(KimiParameterSchemaError::InvalidReference)?;
        let terminates =
            mfjs_schema_can_terminate(target, document, visiting_refs, structural_depth + 1)?;
        visiting_refs.remove(reference);
        return Ok(terminates);
    }

    if let Some(branches) = obj.get("anyOf").and_then(Value::as_array) {
        for branch in branches {
            let mut branch_refs = visiting_refs.clone();
            if mfjs_schema_can_terminate(branch, document, &mut branch_refs, structural_depth + 1)?
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    match obj.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(required) = obj.get("required").and_then(Value::as_array) else {
                return Ok(true);
            };
            if required.is_empty() {
                return Ok(true);
            }
            let properties = obj
                .get("properties")
                .and_then(Value::as_object)
                .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
            for name in required {
                let property = name
                    .as_str()
                    .and_then(|name| properties.get(name))
                    .ok_or(KimiParameterSchemaError::InvalidKeywordValue)?;
                let mut property_refs = visiting_refs.clone();
                if !mfjs_schema_can_terminate(
                    property,
                    document,
                    &mut property_refs,
                    structural_depth + 1,
                )? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Some("array") => {
            if obj.get("minItems").and_then(Value::as_u64).unwrap_or(0) == 0 {
                return Ok(true);
            }
            let Some(items) = obj.get("items") else {
                return Ok(true);
            };
            mfjs_schema_can_terminate(items, document, visiting_refs, structural_depth + 1)
        }
        Some(schema_type) if is_mfjs_concrete_type(schema_type) => Ok(true),
        _ => Err(KimiParameterSchemaError::MissingType),
    }
}

fn validate_mfjs_expanded_depth(
    schema: &Value,
    document: &Value,
    property_depth: usize,
    visiting_refs: &mut HashSet<String>,
    structural_depth: usize,
) -> Result<(), KimiParameterSchemaError> {
    if property_depth > MFJS_MAX_OBJECT_DEPTH || structural_depth > MFJS_MAX_STRUCTURAL_DEPTH {
        return Err(KimiParameterSchemaError::ResourceLimitExceeded);
    }
    let obj = schema
        .as_object()
        .ok_or(KimiParameterSchemaError::InvalidSchemaNode)?;

    if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
        if !visiting_refs.insert(reference.to_string()) {
            return Ok(());
        }
        let target = resolve_mfjs_reference(document, reference)
            .ok_or(KimiParameterSchemaError::InvalidReference)?;
        let result = validate_mfjs_expanded_depth(
            target,
            document,
            property_depth,
            visiting_refs,
            structural_depth + 1,
        );
        visiting_refs.remove(reference);
        return result;
    }

    if let Some(properties) = obj.get("properties").and_then(Value::as_object) {
        for property in properties.values() {
            validate_mfjs_expanded_depth(
                property,
                document,
                property_depth + 1,
                visiting_refs,
                structural_depth + 1,
            )?;
        }
    }
    if let Some(items) = obj.get("items") {
        validate_mfjs_expanded_depth(
            items,
            document,
            property_depth,
            visiting_refs,
            structural_depth + 1,
        )?;
    }
    if let Some(additional) = obj
        .get("additionalProperties")
        .filter(|additional| additional.is_object())
    {
        validate_mfjs_expanded_depth(
            additional,
            document,
            property_depth,
            visiting_refs,
            structural_depth + 1,
        )?;
    }
    if let Some(branches) = obj.get("anyOf").and_then(Value::as_array) {
        for branch in branches {
            validate_mfjs_expanded_depth(
                branch,
                document,
                property_depth,
                visiting_refs,
                structural_depth + 1,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod kimi_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kimi_parameters_repairs_empty_items_as_unconstrained() {
        // A browser-automation MCP tool ships
        // `args: {"type": "array", "items": {}}`. JSON Schema reads the empty
        // items schema as unconstrained, but MFJS fails it with MissingType
        // and the request builder silently dropped the whole tool — the
        // model kept seeing a tool_search reference it could never call. The
        // MFJS-legal equivalent shape drops the keyword instead.
        let mut schema = json!({
            "type": "object",
            "properties": {
                "function": {"type": "string"},
                "args": {"type": "array", "items": {}}
            },
            "required": ["function"],
            "additionalProperties": true
        });
        sanitize_for_kimi_parameters(&mut schema)
            .expect("empty items must repair to the keyword being absent");
        assert_eq!(schema["properties"]["args"], json!({"type": "array"}));

        // Nested occurrences repair too; other untyped schema nodes still
        // fail closed because no semantics-preserving MFJS shape exists.
        let mut nested = json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": {
                        "inner": {"type": "array", "items": {}}
                    },
                    "required": ["inner"]
                }
            }
        });
        sanitize_for_kimi_parameters(&mut nested).expect("nested empty items repair");
        assert_eq!(
            nested["properties"]["outer"]["properties"]["inner"],
            json!({"type": "array"})
        );

        let mut described_only = json!({
            "type": "object",
            "properties": {
                "args": {"type": "array", "items": {"description": "anything"}}
            }
        });
        assert_eq!(
            sanitize_for_kimi_parameters(&mut described_only).unwrap_err(),
            KimiParameterSchemaError::MissingType
        );
    }

    #[test]
    fn kimi_parameters_repairs_nested_and_sibling_empty_items() {
        // items-in-items: the inner keyword repairs to nothing, which empties
        // the outer node; `{"items": X}` is vacuously true for non-arrays and
        // `{"items": {}}` accepts everything, so dropping the outer keyword
        // preserves the accepted set. The emptiness check must run after the
        // recursion or the parent is stranded as `{}` and fails MissingType.
        let mut grid = json!({
            "type": "object",
            "properties": {
                "grid": {"type": "array", "items": {"items": {}}}
            }
        });
        sanitize_for_kimi_parameters(&mut grid).expect("items-in-items repair");
        assert_eq!(grid["properties"]["grid"], json!({"type": "array"}));

        // A root `items` keyword beside `$defs`: the repair must visit every
        // schema-value sibling of the root node, not just the removed key's
        // own subtree, or `$defs` stays unrepaired and fails the whole tool.
        let mut with_defs = json!({
            "type": "object",
            "items": {},
            "$defs": {"rows": {"type": "array", "items": {}}},
            "properties": {"rows": {"$ref": "#/$defs/rows"}}
        });
        sanitize_for_kimi_parameters(&mut with_defs).expect("root items beside $defs repair");
        assert!(with_defs.get("items").is_none());
        assert_eq!(with_defs["$defs"]["rows"], json!({"type": "array"}));

        // Every schema-value position the validator recurses into must be
        // visited: an empty `items` inside an object-form
        // `additionalProperties` or an `anyOf` branch repairs too, and a
        // walker that skips either position would strand the node and fail
        // the whole tool with MissingType.
        let mut nested_positions = json!({
            "type": "object",
            "properties": {
                "map": {
                    "type": "object",
                    "additionalProperties": {"type": "array", "items": {}}
                },
                "choice": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {}}
                    ]
                }
            }
        });
        sanitize_for_kimi_parameters(&mut nested_positions)
            .expect("additionalProperties and anyOf empty items repair");
        assert_eq!(
            nested_positions["properties"]["map"]["additionalProperties"],
            json!({"type": "array"})
        );
        assert_eq!(
            nested_positions["properties"]["choice"]["anyOf"][1],
            json!({"type": "array"})
        );
    }

    #[test]
    fn kimi_sanitize_pushes_type_into_anyof_items() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "object",
                    "anyOf": [
                        {"type": "string"},
                        {"type": "null"}
                    ]
                }
            }
        });
        sanitize_for_kimi(&mut schema);
        let handle = &schema["properties"]["handle"];
        assert!(
            !handle.as_object().unwrap().contains_key("type"),
            "root type should be removed"
        );
        let any_of = handle["anyOf"].as_array().unwrap();
        assert_eq!(any_of[0]["type"], "string");
        assert_eq!(any_of[1]["type"], "null");
    }

    #[test]
    fn kimi_sanitize_injects_missing_anyof_item_types() {
        let mut schema = json!({
            "type": "object",
            "anyOf": [
                {"properties": {"path": {"type": "string"}}},
                {"required": ["url"], "properties": {"url": {"type": "string"}}}
            ]
        });

        sanitize_for_kimi(&mut schema);

        assert!(
            !schema.as_object().unwrap().contains_key("type"),
            "parent type should be removed"
        );
        let any_of = schema["anyOf"].as_array().unwrap();
        assert_eq!(any_of[0]["type"], "object");
        assert_eq!(any_of[1]["type"], "object");
    }

    #[test]
    fn kimi_sanitize_preserves_type_injected_into_nested_anyof_item() {
        let mut schema = json!({
            "type": "object",
            "anyOf": [
                {
                    "anyOf": [
                        {"properties": {"path": {"type": "string"}}}
                    ]
                }
            ]
        });

        sanitize_for_kimi(&mut schema);

        let outer_item = &schema["anyOf"][0];
        assert_eq!(outer_item["type"], "object");
        assert!(
            !schema.as_object().unwrap().contains_key("type"),
            "outer parent type should be removed"
        );
    }

    #[test]
    fn kimi_sanitize_leaves_pure_object_untouched() {
        let original = json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        });
        let mut schema = original.clone();
        sanitize_for_kimi(&mut schema);
        assert_eq!(schema, original);
    }

    #[test]
    fn kimi_parameters_add_type_to_empty_root() {
        let mut schema = json!({});
        sanitize_for_kimi_parameters(&mut schema).unwrap();
        assert_eq!(schema, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn kimi_parameters_add_type_to_properties_root_without_corrupting_properties_map() {
        let mut schema = json!({
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"]
        });

        sanitize_for_kimi_parameters(&mut schema).unwrap();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert!(schema["properties"].get("type").is_none());
    }

    // Function parameters must end as a plain object root. Composition stays
    // available only in valid nested anyOf positions.

    #[test]
    fn kimi_parameters_add_type_to_anyof_root() {
        let mut schema = json!({
            "anyOf": [
                {"type": "object", "properties": {"path": {"type": "string"}}},
                {"type": "null"}
            ]
        });
        sanitize_for_kimi_parameters(&mut schema).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("anyOf").is_none());
        assert_eq!(schema["properties"]["path"]["type"], "string");
    }

    #[test]
    fn kimi_parameters_add_type_to_allof_root() {
        let mut schema = json!({
            "allOf": [
                {"type": "object", "properties": {"name": {"type": "string"}}}
            ]
        });
        sanitize_for_kimi_parameters(&mut schema).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("allOf").is_none());
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }

    #[test]
    fn kimi_parameters_add_type_to_oneof_root() {
        let mut schema = json!({
            "oneOf": [
                {"type": "object", "properties": {"id": {"type": "integer"}}},
                {"type": "object", "properties": {"name": {"type": "string"}}}
            ]
        });
        sanitize_for_kimi_parameters(&mut schema).unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("oneOf").is_none());
        assert_eq!(schema["properties"]["id"]["type"], "integer");
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }

    #[test]
    fn kimi_parameters_preserves_nested_anyof_branches() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "object",
                    "anyOf": [
                        {"properties": {"path": {"type": "string"}}},
                        {"properties": {"id": {"type": "integer"}}}
                    ]
                }
            }
        });

        sanitize_for_kimi_parameters(&mut schema).unwrap();

        assert_eq!(schema["type"], "object");
        let selector = &schema["properties"]["selector"];
        assert!(selector.get("type").is_none());
        let branches = selector["anyOf"].as_array().unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches.iter().all(|branch| branch["type"] == "object"));
    }

    #[test]
    fn kimi_parameters_converts_nested_oneof_to_supported_anyof() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "object",
                    "oneOf": [
                        {"properties": {"path": {"type": "string"}}},
                        {"properties": {"id": {"type": "integer"}}}
                    ]
                }
            }
        });

        sanitize_for_kimi_parameters(&mut schema).unwrap();

        let selector = &schema["properties"]["selector"];
        assert!(selector.get("oneOf").is_none());
        assert!(selector["anyOf"].is_array());
        assert!(selector.get("type").is_none());
    }

    #[test]
    fn kimi_parameters_restores_registry_collapsed_nullable_anyof() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "query": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "null"}
                    ]
                }
            }
        });

        // Exercise the exact two-stage production path: ToolRegistry applies
        // the provider-neutral pass before the Moonshot request adapter sees
        // the schema.
        sanitize(&mut schema);
        assert_eq!(schema["properties"]["query"]["nullable"], true);
        assert!(schema["properties"]["query"].get("anyOf").is_none());

        sanitize_for_kimi_parameters(&mut schema).unwrap();

        let query = &schema["properties"]["query"];
        assert!(query.get("nullable").is_none(), "{query}");
        assert_eq!(
            query["anyOf"],
            json!([{"type": "string"}, {"type": "null"}])
        );
        validate_mfjs_parameters(&schema).unwrap();
    }

    #[test]
    fn kimi_parameters_recursively_translates_safe_const_to_enum() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "envelope": {
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "kind": {"type": "string", "const": "var_handle"}
                                },
                                "required": ["kind"]
                            }
                        }
                    }
                }
            }
        });

        sanitize_for_kimi_parameters(&mut schema).unwrap();

        let kind = schema
            .pointer("/properties/envelope/properties/items/items/properties/kind")
            .expect("nested kind schema");
        assert!(kind.get("const").is_none(), "{kind}");
        assert_eq!(kind["enum"], json!(["var_handle"]));
    }

    #[test]
    fn kimi_parameters_rejects_unsafe_const_without_mutating_or_leaking() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "private-toggle-8172": {"type": "boolean", "const": true}
            }
        });
        let original = schema.clone();

        let error = sanitize_for_kimi_parameters(&mut schema).unwrap_err();

        assert_eq!(error, KimiParameterSchemaError::UnsupportedConstLiteral);
        assert!(!error.to_string().contains("private-toggle-8172"));
        assert_eq!(schema, original, "a rejected schema must remain reusable");
    }

    #[test]
    fn kimi_parameters_validator_fails_closed_without_echoing_schema_values() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "private-field-4921": {
                    "type": "string",
                    "pattern": "private-pattern-value-7395"
                }
            }
        });
        let original = schema.clone();

        let error = sanitize_for_kimi_parameters(&mut schema).unwrap_err();
        let diagnostic = error.to_string();

        assert_eq!(error, KimiParameterSchemaError::UnsupportedKeyword);
        assert!(!diagnostic.contains("private-field-4921"));
        assert!(!diagnostic.contains("private-pattern-value-7395"));
        assert_eq!(schema, original, "failed validation must be transactional");
    }

    #[test]
    fn kimi_parameters_rejects_untyped_schema_and_nullable_transactionally() {
        for (mut schema, expected, sentinels) in [
            (
                json!({
                    "type": "object",
                    "properties": {
                        "private-missing-type-1207": {
                            "description": "private-description-1208"
                        }
                    }
                }),
                KimiParameterSchemaError::MissingType,
                ["private-missing-type-1207", "private-description-1208"],
            ),
            (
                json!({
                    "type": "object",
                    "properties": {
                        "private-nullable-1209": {
                            "nullable": true,
                            "description": "private-nullable-description-1210"
                        }
                    }
                }),
                KimiParameterSchemaError::InvalidNullable,
                ["private-nullable-1209", "private-nullable-description-1210"],
            ),
        ] {
            let original = schema.clone();
            let error = sanitize_for_kimi_parameters(&mut schema).unwrap_err();
            assert_eq!(error, expected);
            for sentinel in sentinels {
                assert!(!error.to_string().contains(sentinel));
            }
            assert_eq!(schema, original, "rejection must be transactional");
        }
    }

    #[test]
    fn kimi_parameters_infers_safe_types_for_untyped_const() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "kind": {"const": "var_handle"},
                "count": {"const": 7},
                "ratio": {"const": 1.25}
            }
        });

        sanitize_for_kimi_parameters(&mut schema).unwrap();

        assert_eq!(schema["properties"]["kind"]["type"], "string");
        assert_eq!(schema["properties"]["kind"]["enum"], json!(["var_handle"]));
        assert_eq!(schema["properties"]["count"]["type"], "integer");
        assert_eq!(schema["properties"]["count"]["enum"], json!([7]));
        assert_eq!(schema["properties"]["ratio"]["type"], "number");
        assert_eq!(schema["properties"]["ratio"]["enum"], json!([1.25]));
    }

    #[test]
    fn kimi_parameters_allows_only_the_documented_empty_schema_exception() {
        let mut schema = json!({
            "type": "object",
            "additionalProperties": {}
        });
        sanitize_for_kimi_parameters(&mut schema).unwrap();
        assert_eq!(schema["additionalProperties"], json!({}));

        let mut invalid = json!({
            "type": "object",
            "properties": {"value": {}}
        });
        assert_eq!(
            sanitize_for_kimi_parameters(&mut invalid).unwrap_err(),
            KimiParameterSchemaError::MissingType
        );
    }

    #[test]
    fn kimi_parameters_rejects_required_direct_and_mutual_recursion() {
        let fixtures = [
            json!({
                "type": "object",
                "properties": {
                    "private-root-node-2201": {"$ref": "#/$defs/private-node-2202"}
                },
                "required": ["private-root-node-2201"],
                "$defs": {
                    "private-node-2202": {
                        "type": "object",
                        "properties": {
                            "private-next-2203": {"$ref": "#/$defs/private-node-2202"}
                        },
                        "required": ["private-next-2203"]
                    }
                }
            }),
            json!({
                "type": "object",
                "properties": {
                    "private-root-a-2204": {"$ref": "#/$defs/private-a-2205"}
                },
                "required": ["private-root-a-2204"],
                "$defs": {
                    "private-a-2205": {
                        "type": "object",
                        "properties": {
                            "private-to-b-2206": {"$ref": "#/$defs/private-b-2207"}
                        },
                        "required": ["private-to-b-2206"]
                    },
                    "private-b-2207": {
                        "type": "object",
                        "properties": {
                            "private-to-a-2208": {"$ref": "#/$defs/private-a-2205"}
                        },
                        "required": ["private-to-a-2208"]
                    }
                }
            }),
        ];

        for mut schema in fixtures {
            let original = schema.clone();
            let error = sanitize_for_kimi_parameters(&mut schema).unwrap_err();
            assert_eq!(error, KimiParameterSchemaError::NonTerminatingReference);
            for sentinel in ["private-root", "private-node", "private-to"] {
                assert!(!error.to_string().contains(sentinel));
            }
            assert_eq!(schema, original, "recursive rejection must be atomic");
        }
    }

    #[test]
    fn kimi_parameters_preserves_optional_and_nullable_recursive_termination() {
        let mut optional = json!({
            "type": "object",
            "properties": {
                "node": {"$ref": "#/$defs/Node"}
            },
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "next": {"$ref": "#/$defs/Node"}
                    },
                    "required": ["next"]
                }
            }
        });
        sanitize_for_kimi_parameters(&mut optional).unwrap();

        let mut nullable = json!({
            "type": "object",
            "properties": {
                "node": {
                    "anyOf": [
                        {"$ref": "#/$defs/Node"},
                        {"type": "null"}
                    ]
                }
            },
            "required": ["node"],
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "next": {
                            "anyOf": [
                                {"$ref": "#/$defs/Node"},
                                {"type": "null"}
                            ]
                        }
                    },
                    "required": ["next"]
                }
            }
        });
        sanitize_for_kimi_parameters(&mut nullable).unwrap();
    }

    #[test]
    fn kimi_parameters_enforces_anyof_and_aggregate_resource_limits() {
        let mut too_many_branches = json!({
            "type": "object",
            "properties": {
                "choice": {
                    "anyOf": (0..11)
                        .map(|_| json!({"type": "string"}))
                        .collect::<Vec<_>>()
                }
            }
        });
        assert_eq!(
            sanitize_for_kimi_parameters(&mut too_many_branches).unwrap_err(),
            KimiParameterSchemaError::ResourceLimitExceeded
        );

        let string_property = || json!({"type": "string"});
        let mut root_properties = Map::new();
        for index in 0..51 {
            root_properties.insert(format!("root_{index}"), string_property());
        }
        let mut definition_properties = Map::new();
        for index in 0..50 {
            definition_properties.insert(format!("definition_{index}"), string_property());
        }
        let mut too_many_properties = json!({
            "type": "object",
            "properties": Value::Object(root_properties),
            "$defs": {
                "Holder": {
                    "type": "object",
                    "properties": Value::Object(definition_properties)
                }
            }
        });
        assert_eq!(
            sanitize_for_kimi_parameters(&mut too_many_properties).unwrap_err(),
            KimiParameterSchemaError::ResourceLimitExceeded
        );

        let enum_values = |start: usize, count: usize| {
            Value::Array((start..start + count).map(|value| json!(value)).collect())
        };
        let mut too_many_enum_values = json!({
            "type": "object",
            "properties": {
                "first": {"type": "integer", "enum": enum_values(0, 250)},
                "second": {"type": "integer", "enum": enum_values(250, 251)}
            }
        });
        assert_eq!(
            sanitize_for_kimi_parameters(&mut too_many_enum_values).unwrap_err(),
            KimiParameterSchemaError::ResourceLimitExceeded
        );
    }

    #[test]
    fn kimi_parameters_enforces_depth_and_enum_text_limits() {
        let mut too_deep = json!({"type": "string"});
        for index in (0..6).rev() {
            let mut properties = Map::new();
            properties.insert(format!("level_{index}"), too_deep);
            too_deep = json!({
                "type": "object",
                "properties": Value::Object(properties)
            });
        }
        assert_eq!(
            sanitize_for_kimi_parameters(&mut too_deep).unwrap_err(),
            KimiParameterSchemaError::ResourceLimitExceeded
        );

        let long_values = Value::Array(
            (0..251)
                .map(|index| Value::String(format!("private-enum-{index:04}-xxxxxxxxxxxxxx")))
                .collect(),
        );
        let mut too_much_enum_text = json!({
            "type": "object",
            "properties": {
                "choice": {"type": "string", "enum": long_values}
            }
        });
        assert_eq!(
            sanitize_for_kimi_parameters(&mut too_much_enum_text).unwrap_err(),
            KimiParameterSchemaError::ResourceLimitExceeded
        );
    }

    #[test]
    fn kimi_parameters_validates_default_type_and_placement_without_leaks() {
        let mut valid = json!({
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean", "default": true},
                "count": {"type": "integer", "default": 3},
                "ratio": {"type": "number", "default": 1.5},
                "label": {"type": "string", "default": "default-label"},
                "empty": {"type": "null", "default": null}
            }
        });
        sanitize_for_kimi_parameters(&mut valid).unwrap();

        for (mut invalid, expected) in [
            (
                json!({
                    "type": "object",
                    "properties": {
                        "private-default-3301": {
                            "type": "integer",
                            "default": "private-default-value-3302"
                        }
                    }
                }),
                KimiParameterSchemaError::InvalidDefault,
            ),
            (
                json!({
                    "type": "object",
                    "properties": {
                        "private-untyped-default-3303": {"default": 1}
                    }
                }),
                KimiParameterSchemaError::MissingType,
            ),
            (
                json!({
                    "type": "object",
                    "properties": {
                        "private-object-default-3304": {
                            "type": "object",
                            "default": {}
                        }
                    }
                }),
                KimiParameterSchemaError::InvalidKeywordValue,
            ),
        ] {
            let original = invalid.clone();
            let error = sanitize_for_kimi_parameters(&mut invalid).unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains("private-default"));
            assert_eq!(invalid, original);
        }
    }

    #[test]
    fn kimi_parameters_rejects_inverted_and_fractional_integer_bounds() {
        for mut schema in [
            json!({
                "type": "object",
                "properties": {
                    "value": {"type": "string", "minLength": 5, "maxLength": 4}
                }
            }),
            json!({
                "type": "object",
                "properties": {
                    "value": {"type": "array", "minItems": 3, "maxItems": 2}
                }
            }),
            json!({
                "type": "object",
                "properties": {
                    "value": {"type": "number", "minimum": 10, "maximum": 9}
                }
            }),
            json!({
                "type": "object",
                "properties": {
                    "value": {"type": "integer", "minimum": 1.5}
                }
            }),
            json!({
                "type": "object",
                "properties": {
                    "value": {"type": "integer", "maximum": 2.5}
                }
            }),
        ] {
            assert_eq!(
                sanitize_for_kimi_parameters(&mut schema).unwrap_err(),
                KimiParameterSchemaError::InvalidRange
            );
        }
    }

    #[test]
    fn kimi_parameters_inlines_valid_internal_object_root_ref() {
        let mut schema = json!({
            "$ref": "#/$defs/FileArgs",
            "$defs": {
                "FileArgs": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            },
            "description": "File arguments"
        });

        sanitize_for_kimi_parameters(&mut schema).unwrap();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["description"], "File arguments");
        assert!(schema["$defs"].is_object());
        assert!(schema.get("$ref").is_none());
        assert!(schema.get("allOf").is_none());
    }

    #[test]
    fn kimi_parameters_rejects_unresolved_root_ref_without_leaking_it() {
        let mut schema = json!({
            "$ref": "#/$defs/private-schema-name-9217",
            "$defs": {}
        });
        let original = schema.clone();

        let error = sanitize_for_kimi_parameters(&mut schema).unwrap_err();

        assert_eq!(error, KimiParameterSchemaError::UnresolvedRootReference);
        assert!(!error.to_string().contains("private-schema-name-9217"));
        assert_eq!(schema, original, "a rejected schema must never be emitted");
    }

    fn contains_key_anywhere(value: &Value, key: &str) -> bool {
        match value {
            Value::Object(map) => {
                map.contains_key(key) || map.values().any(|v| contains_key_anywhere(v, key))
            }
            Value::Array(items) => items.iter().any(|v| contains_key_anywhere(v, key)),
            _ => false,
        }
    }

    fn action_discriminated_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["start", "status"]},
                "prompt": {"type": "string"},
                "agent_id": {"type": "string"}
            },
            "required": ["action"],
            "dependentRequired": {
                "prompt": ["action"]
            },
            "dependentSchemas": {
                "action": {"required": ["prompt"]}
            }
        })
    }

    #[test]
    fn kimi_degrades_dependent_keywords_to_the_flat_schema() {
        let mut schema = action_discriminated_schema();

        let note = sanitize_for_kimi_parameters(&mut schema).expect("must not refuse the schema");

        assert!(!contains_key_anywhere(&schema, "dependentSchemas"));
        assert!(!contains_key_anywhere(&schema, "dependentRequired"));
        let properties = schema["properties"].as_object().expect("properties");
        for name in ["action", "prompt", "agent_id"] {
            assert!(properties.contains_key(name), "{name} left the flat schema");
        }
        assert_eq!(schema["required"], json!(["action"]));
        validate_mfjs_parameters(&schema).expect("degraded schema must validate");

        let note = note.expect("dropping a requirement must be reported to the model");
        assert_eq!(
            note,
            "This provider cannot express conditional requirements from the original schema. \
             Honor any such requirements documented by the tool when calling it."
        );
    }

    #[test]
    fn kimi_leaves_schemas_without_dependent_keywords_unannotated() {
        let mut schema = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        });

        let note = sanitize_for_kimi_parameters(&mut schema).expect("plain schema");

        assert_eq!(
            note, None,
            "a schema that lost nothing must not gain a note"
        );
        assert_eq!(schema["required"], json!(["path"]));
    }

    #[test]
    fn kimi_degrades_dependent_keywords_nested_under_properties() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string"},
                        "path": {"type": "string"}
                    },
                    "dependentRequired": {"kind": ["path"]}
                }
            }
        });

        let note = sanitize_for_kimi_parameters(&mut schema).expect("nested composition");

        assert!(!contains_key_anywhere(&schema, "dependentRequired"));
        assert!(
            schema["properties"]["target"]["properties"]
                .as_object()
                .expect("nested properties")
                .contains_key("path")
        );
        validate_mfjs_parameters(&schema).expect("degraded schema must validate");
        assert!(note.is_some(), "nested drop must still be reported");
    }

    #[test]
    fn kimi_reports_malformed_dependent_keywords_that_it_drops() {
        let mut schema = json!({
            "type": "object",
            "properties": {},
            "dependentRequired": false,
            "dependentSchemas": ["invalid"]
        });

        let note = sanitize_for_kimi_parameters(&mut schema).expect("degraded schema");

        assert!(note.is_some(), "every dropped dependency must be reported");
        assert!(!contains_key_anywhere(&schema, "dependentRequired"));
        assert!(!contains_key_anywhere(&schema, "dependentSchemas"));
    }

    #[test]
    fn kimi_parameters_rejects_non_object_root_ref_without_leaking_it() {
        let mut schema = json!({
            "$ref": "#/$defs/private-scalar-name-4831",
            "$defs": {
                "private-scalar-name-4831": {"type": "string"}
            }
        });
        let original = schema.clone();

        let error = sanitize_for_kimi_parameters(&mut schema).unwrap_err();

        assert_eq!(error, KimiParameterSchemaError::ReferencedRootMustBeObject);
        assert!(!error.to_string().contains("private-scalar-name-4831"));
        assert_eq!(schema, original, "a rejected schema must never be emitted");
    }
}
