//! Byte-level canonicalization of JSON Schema for prefix-cache stability.
//!
//! When MCP servers return tool schemas, the field order within each schema
//! object and the order of entries in `required` / `dependentRequired` arrays
//! can vary across reconnections. This module normalizes those orderings so
//! that two logically equivalent schemas always produce identical bytes after
//! serialization.
//!
//! The approach mirrors `reasonix/internal/provider/schema_canonicalize.go`:
//!
//! 1. Sort every `"required"` array alphabetically.
//! 2. Sort every `"dependentRequired"` sub-array alphabetically.
//! 3. Recurse into all nested objects and arrays.
//!
//! `serde_json::Value::Object` uses `IndexMap` when `preserve_order` is
//! enabled (which this crate does). We therefore rebuild the map with sorted
//! keys to guarantee deterministic key ordering.

use serde_json::Value;

/// Recursively canonicalize a JSON Schema value in-place.
///
/// After canonicalization, two schemas that are semantically equivalent
/// (same keys, same `required` set, same `dependentRequired` sets) will
/// serialize to byte-identical JSON regardless of the original field or
/// array order.
pub fn canonicalize_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // Sort `required` arrays (they are sets per JSON Schema spec).
            if let Some(Value::Array(req)) = map.get_mut("required") {
                sort_string_array(req);
            }
            // Sort `dependentRequired` sub-arrays.
            if let Some(Value::Object(deps)) = map.get_mut("dependentRequired") {
                for dep_value in deps.values_mut() {
                    if let Value::Array(arr) = dep_value {
                        sort_string_array(arr);
                    }
                }
            }
            // Recurse into every child value.
            for v in map.values_mut() {
                canonicalize_schema(v);
            }
            // Rebuild the map with sorted keys so serialization is deterministic.
            // serde_json::Map backed by IndexMap (preserve_order) doesn't have
            // drain(), so we swap to a temporary and rebuild.
            let old = std::mem::take(map);
            let mut entries: Vec<(String, Value)> = old.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in entries {
                map.insert(k, v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                canonicalize_schema(v);
            }
        }
        _ => {}
    }
}

/// Sort a JSON array of string values alphabetically in-place.
///
/// Non-string entries are left at the end in their original relative order.
fn sort_string_array(arr: &mut [Value]) {
    arr.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_required_array() {
        let mut schema = json!({
            "type": "object",
            "required": ["z", "a", "m"],
            "properties": {}
        });
        canonicalize_schema(&mut schema);
        assert_eq!(schema["required"], json!(["a", "m", "z"]));
    }

    #[test]
    fn equivalent_ordering_matches() {
        // Two schemas that differ only in field order and required order
        // must serialize to identical bytes.
        let mut a = json!({
            "required": ["b", "a"],
            "properties": {"x": {}, "y": {}},
            "type": "object"
        });
        let mut b = json!({
            "type": "object",
            "properties": {"y": {}, "x": {}},
            "required": ["a", "b"]
        });
        canonicalize_schema(&mut a);
        canonicalize_schema(&mut b);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "logically equivalent schemas must produce identical bytes"
        );
    }

    #[test]
    fn sorts_dependent_required() {
        let mut schema = json!({
            "type": "object",
            "dependentRequired": {
                "x": ["z", "a"],
                "y": ["m", "b"]
            }
        });
        canonicalize_schema(&mut schema);
        assert_eq!(schema["dependentRequired"]["x"], json!(["a", "z"]));
        assert_eq!(schema["dependentRequired"]["y"], json!(["b", "m"]));
    }

    #[test]
    fn recursive_into_properties() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "nested": {
                    "type": "object",
                    "required": ["z", "a"],
                    "properties": {}
                }
            }
        });
        canonicalize_schema(&mut schema);
        assert_eq!(
            schema["properties"]["nested"]["required"],
            json!(["a", "z"])
        );
    }

    #[test]
    fn preserves_non_required_array_order() {
        // Arrays that are not `required` or `dependentRequired` should
        // keep their semantic order (e.g. enum values, oneOf items).
        let mut schema = json!({
            "type": "string",
            "enum": ["z", "a", "m"]
        });
        canonicalize_schema(&mut schema);
        assert_eq!(schema["enum"], json!(["z", "a", "m"]));
    }

    #[test]
    fn handles_empty_schema() {
        let mut schema = json!({});
        canonicalize_schema(&mut schema);
        assert_eq!(schema, json!({}));
    }

    #[test]
    fn handles_deeply_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "level1": {
                    "type": "object",
                    "properties": {
                        "level2": {
                            "type": "object",
                            "required": ["z", "a"]
                        }
                    }
                }
            }
        });
        canonicalize_schema(&mut schema);
        assert_eq!(
            schema["properties"]["level1"]["properties"]["level2"]["required"],
            json!(["a", "z"])
        );
    }

    #[test]
    fn key_order_is_alphabetical_after_canonicalize() {
        let mut schema = json!({
            "z_field": 1,
            "a_field": 2,
            "m_field": 3
        });
        canonicalize_schema(&mut schema);
        let keys: Vec<&str> = schema
            .as_object()
            .unwrap()
            .keys()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(keys, vec!["a_field", "m_field", "z_field"]);
    }
}
