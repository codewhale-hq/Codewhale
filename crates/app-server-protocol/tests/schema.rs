#![cfg(feature = "schema")]

use codewhale_app_server_protocol::runtime::{
    DynamicToolCallResult, DynamicToolSpec, RuntimeEventEnvelope, RuntimeImageInput,
};
use std::collections::BTreeMap;

#[test]
fn runtime_schema_matches_real_nullable_and_flattened_wire_fields() {
    let schema = codewhale_app_server_protocol::export::runtime_schema();
    let definitions = schema["$defs"].as_object().unwrap();
    let event = &definitions["RuntimeEventEnvelope"];
    let turn = &event["properties"]["turn_id"];
    let nullable = turn["type"]
        .as_array()
        .is_some_and(|types| types.iter().any(|ty| ty == "null"))
        || turn["anyOf"]
            .as_array()
            .is_some_and(|types| types.iter().any(|ty| ty["type"] == "null"));
    assert!(nullable);
    let required = event["required"].as_array().unwrap();
    for field in ["schema_version", "turn_id", "item_id", "payload"] {
        assert!(required.iter().any(|name| name == field));
    }
    assert!(!required.iter().any(|name| name == "created_at"));
    assert_eq!(event["properties"]["created_at"]["type"], "string");
    for name in ["RuntimeCapabilities", "RuntimeExperimentalCapabilities"] {
        let record = &definitions[name];
        let required = record["required"].as_array().unwrap();
        for field in record["properties"].as_object().unwrap().keys() {
            assert!(
                required
                    .iter()
                    .any(|name| name.as_str() == Some(field.as_str()))
            );
        }
    }
    let spec = &definitions["DynamicToolSpec"];
    assert!(
        !spec["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "defer_loading")
    );
    let published_spec = &definitions["DynamicToolSpecOutput"];
    assert!(
        published_spec["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "defer_loading")
    );
    assert_eq!(published_spec["properties"]["namespace"]["type"], "string");
    assert!(
        !definitions["DynamicToolCallResult"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "content")
    );
    let input: DynamicToolSpec = serde_json::from_value(
        serde_json::json!({"name":"tool","description":"fixture","input_schema":{}}),
    )
    .unwrap();
    assert!(!input.defer_loading);
    let result: DynamicToolCallResult =
        serde_json::from_value(serde_json::json!({"success":false})).unwrap();
    assert!(result.content.is_empty());
    assert!(
        serde_json::from_value::<RuntimeImageInput>(
            serde_json::json!({"mime":"image/png","dataBase64":"","extra":true})
        )
        .is_err()
    );
    assert!(event["properties"].get("extra").is_none());
    assert_eq!(
        definitions["RuntimeImageInput"]["additionalProperties"],
        false
    );
    let value = serde_json::to_value(RuntimeEventEnvelope {
        schema_version: 1,
        seq: 4,
        event: "future.name".into(),
        kind: "future.name".into(),
        thread_id: "thread".into(),
        turn_id: None,
        item_id: None,
        timestamp: "now".into(),
        created_at: None,
        payload: serde_json::json!({"receipt": null}),
        extra: BTreeMap::new(),
    })
    .unwrap();
    assert!(value["turn_id"].is_null());
    assert!(value.get("created_at").is_none());
    let typescript = codewhale_app_server_protocol::export::runtime_typescript(&schema);
    assert!(typescript.contains("turn_id: (string | null)"));
    assert!(!typescript.contains("turn_id?:"));
    assert!(typescript.contains("created_at?: string"));
    assert!(typescript.contains("export type RuntimeImageInput ="));
    assert!(typescript.contains("Json[] | { [key: string]: Json | undefined }"));
    assert!(typescript.contains("type: 'input_text'"));
    assert!(typescript.contains("dataBase64: string"));
    assert!(typescript.contains("[key: string]: Json | undefined"));
}

#[test]
fn committed_runtime_schema_and_sdk_exports_match_one_rust_contract() {
    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = package.parent().and_then(std::path::Path::parent).unwrap();
    let schema = codewhale_app_server_protocol::export::runtime_schema();
    let json = serde_json::to_string_pretty(&schema).unwrap() + "\n";
    let typescript = codewhale_app_server_protocol::export::runtime_typescript(&schema);
    assert_eq!(
        std::fs::read_to_string(package.join("schema/json/runtime.schema.json")).unwrap(),
        json
    );
    assert_eq!(
        std::fs::read_to_string(package.join("schema/typescript/runtime.d.ts")).unwrap(),
        typescript
    );
    assert_eq!(
        std::fs::read_to_string(repository.join("npm/runtime-sdk/generated/runtime.d.ts")).unwrap(),
        typescript
    );
}
