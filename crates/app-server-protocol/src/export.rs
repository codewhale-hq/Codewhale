use crate::rpc::{
    ThreadLoadedListParams, ThreadLoadedListResponse, ThreadSetNameParams, ThreadSetNameResponse,
};
use crate::runtime::*;
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn output_references(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.get("$ref").and_then(Value::as_str) == Some("#/$defs/DynamicToolSpec") {
                object.insert(
                    "$ref".into(),
                    Value::String("#/$defs/DynamicToolSpecOutput".into()),
                );
            }
            for child in object.values_mut() {
                output_references(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                output_references(child);
            }
        }
        _ => {}
    }
}

pub fn runtime_schema() -> Value {
    let mut input = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator();
    let _ = input.subschema_for::<RuntimeImageInput>();
    let _ = input.subschema_for::<ThreadLoadedListParams>();
    let _ = input.subschema_for::<ThreadSetNameParams>();
    let _ = input.subschema_for::<DynamicToolSpec>();
    let _ = input.subschema_for::<DynamicToolItemStatus>();
    let _ = input.subschema_for::<DynamicToolCallResult>();
    let _ = input.subschema_for::<DynamicToolCallContent>();
    let _ = input.subschema_for::<TurnEnvironmentParams>();
    let mut output = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    let _ = output.subschema_for::<RuntimeEventEnvelope>();
    let _ = output.subschema_for::<ThreadLoadedListResponse>();
    let _ = output.subschema_for::<ThreadSetNameResponse>();
    let _ = output.subschema_for::<RuntimeCapabilities>();
    let _ = output.subschema_for::<FleetProfileDocument>();
    let _ = output.subschema_for::<RuntimeExperimentalCapabilities>();
    let _ = output.subschema_for::<DynamicToolCallParams>();
    let _ = output.subschema_for::<DynamicToolSpec>();
    let _ = output.subschema_for::<TurnEnvironmentParams>();
    let _ = output.subschema_for::<DynamicToolItemStatus>();
    let mut definitions = input.take_definitions(false);
    for (name, mut schema) in output.take_definitions(false) {
        output_references(&mut schema);
        let name = if name == "DynamicToolSpec" {
            "DynamicToolSpecOutput".to_string()
        } else {
            name
        };
        if let Some(previous) = definitions.get(&name) {
            assert_eq!(previous, &schema, "input/output schema conflict for {name}");
        } else {
            definitions.insert(name, schema);
        }
    }
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "$defs": definitions})
}

pub fn runtime_typescript(schema: &Value) -> String {
    let definitions: BTreeMap<String, crate::schema::Def> = schema["$defs"]
        .as_object()
        .expect("Runtime schema definitions")
        .iter()
        .map(|(name, schema)| {
            (
                name.clone(),
                crate::schema::parse_def(name, schema, crate::schema::SchemaMode::Runtime),
            )
        })
        .collect();
    crate::schema::typescript_defs(&definitions, crate::schema::SchemaMode::Runtime)
}
