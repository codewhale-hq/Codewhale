#[path = "../src/codegen/mod.rs"]
mod codegen;

use codewhale_app_server_protocol::methods::{
    self, ClientNotificationMethod, ClientRequestMethod, Disposition, ServerNotificationMethod,
    ServerRequestMethod, Tier, error_code,
};
use codewhale_app_server_protocol::rpc::RpcCall;
use codewhale_app_server_protocol::v2;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MANIFEST: &str = include_str!("../contract/codex-v2.json");
const PINNED: &str = include_str!("pinned/codex/codex_app_server_protocol.schemas.json");
const COMMON: &str = include_str!("pinned/codex/common.rs");

fn manifest() -> Value {
    serde_json::from_str(MANIFEST).unwrap()
}

fn pinned() -> Value {
    serde_json::from_str(PINNED).unwrap()
}

#[test]
fn generated_outputs_have_not_drifted_from_the_manifest_and_pinned_export() {
    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |path: &str| std::fs::read_to_string(package.join(path)).unwrap();
    assert_eq!(read(codegen::MANIFEST_PATH), MANIFEST);
    assert_eq!(read(codegen::PINNED_SCHEMA_PATH), PINNED);
    let outputs = codegen::generate(MANIFEST, PINNED).unwrap();
    let committed: BTreeMap<&str, &str> = BTreeMap::from([
        ("src/methods.rs", include_str!("../src/methods.rs")),
        ("src/v2.rs", include_str!("../src/v2.rs")),
        ("METHODS.md", include_str!("../METHODS.md")),
        (
            "schema/json/codex-v2.schema.json",
            include_str!("../schema/json/codex-v2.schema.json"),
        ),
        (
            "schema/typescript/codex-v2.d.ts",
            include_str!("../schema/typescript/codex-v2.d.ts"),
        ),
    ]);
    assert_eq!(outputs.files.len(), committed.len());
    for (path, text) in outputs.files {
        assert!(
            committed[path] == text,
            "{path} drifted; run cargo run -p codewhale-app-server-protocol --bin codewhale-codex-contract -- --write"
        );
    }
}

const KINDS: [(&str, &str, &str); 4] = [
    (
        "clientRequests",
        include_str!("pinned/codex/ClientRequest.ts"),
        "ClientRequest",
    ),
    (
        "clientNotifications",
        include_str!("pinned/codex/ClientNotification.ts"),
        "ClientNotification",
    ),
    (
        "serverRequests",
        include_str!("pinned/codex/ServerRequest.ts"),
        "ServerRequest",
    ),
    (
        "serverNotifications",
        include_str!("pinned/codex/ServerNotification.ts"),
        "ServerNotification",
    ),
];

fn pinned_ts_methods(source: &str) -> Vec<&str> {
    source
        .split("\"method\": \"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap())
        .collect()
}

fn typed(kind: &str) -> Vec<(&'static str, Tier)> {
    match kind {
        "clientRequests" => ClientRequestMethod::ALL
            .iter()
            .map(|method| (method.method(), method.tier()))
            .collect(),
        "clientNotifications" => ClientNotificationMethod::ALL
            .iter()
            .map(|method| (method.method(), method.tier()))
            .collect(),
        "serverRequests" => ServerRequestMethod::ALL
            .iter()
            .map(|method| (method.method(), method.tier()))
            .collect(),
        _ => ServerNotificationMethod::ALL
            .iter()
            .map(|method| (method.method(), method.tier()))
            .collect(),
    }
}

fn disposition(kind: &str, method: &str) -> Option<Disposition> {
    match kind {
        "clientRequests" => methods::client_request_disposition(method),
        "clientNotifications" => methods::client_notification_disposition(method),
        "serverRequests" => methods::server_request_disposition(method),
        _ => methods::server_notification_disposition(method),
    }
}

fn upstream_response(method: &str) -> String {
    let marker = format!("=> \"{method}\" {{");
    let start = COMMON
        .find(&marker)
        .unwrap_or_else(|| panic!("pinned common.rs has no {method}"));
    let rest = &COMMON[start..];
    let end = rest.find("\n    },").unwrap();
    let body = &rest[..end];
    let response = body
        .split("response: ")
        .nth(1)
        .unwrap_or_else(|| panic!("{method} has no response in common.rs"));
    let ty = response.split(',').next().unwrap().trim();
    ty.rsplit("::").next().unwrap().to_owned()
}

fn pinned_union_params(pinned: &Value, union: &str, method: &str) -> Option<String> {
    pinned["definitions"][union]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["method"]["enum"][0] == method)
        .unwrap_or_else(|| panic!("pinned {union} has no {method}"))["properties"]["params"]["$ref"]
        .as_str()
        .map(|reference| reference.rsplit('/').next().unwrap().to_owned())
}

#[test]
fn manifest_inventory_matches_the_pinned_method_sets_and_typed_enums() {
    let manifest = manifest();
    let pinned = pinned();
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    for (kind, ts, union) in KINDS {
        let pinned_methods = pinned_ts_methods(ts);
        let pinned_set: BTreeSet<&str> = pinned_methods.iter().copied().collect();
        assert_eq!(pinned_methods.len(), pinned_set.len(), "{kind}: duplicate");
        let rows = manifest["methods"][kind].as_array().unwrap();
        let mut seen = BTreeSet::new();
        for row in rows {
            let method = row["method"].as_str().unwrap();
            let tier = row["tier"].as_str().unwrap();
            let status = row["status"].as_str().unwrap();
            assert!(seen.insert(method), "{kind}: {method} twice");
            assert!(pinned_set.contains(method), "{kind}: {method} not pinned");
            assert!(matches!(tier, "A" | "B" | "C" | "undecided"));
            assert!(matches!(
                status,
                "implemented" | "partial" | "missing" | "method-not-found" | "never-sent"
            ));
            assert_eq!(
                tier == "C",
                matches!(status, "method-not-found" | "never-sent"),
                "{kind}: {method} status {status} does not fit tier {tier}"
            );
            assert_eq!(
                row.get("recommended").is_some(),
                tier == "undecided",
                "{kind}: {method} recommendation belongs to undecided rows only"
            );
            let expected = match tier {
                "A" => Disposition::Selected(Tier::A),
                "B" => Disposition::Selected(Tier::B),
                "C" => Disposition::C,
                _ => Disposition::Undecided,
            };
            assert_eq!(
                disposition(kind, method),
                Some(expected),
                "{kind}: {method}"
            );
            *totals.entry(format!("{kind}/{tier}")).or_default() += 1;
            if matches!(tier, "A" | "B") {
                let params = pinned_union_params(&pinned, union, method);
                assert_eq!(
                    row["params"].as_str().map(str::to_owned),
                    params,
                    "{kind}: {method} params disagree with the pinned {union} union"
                );
                if matches!(kind, "clientRequests" | "serverRequests") {
                    assert_eq!(
                        row["response"].as_str().unwrap(),
                        upstream_response(method),
                        "{kind}: {method} result disagrees with pinned common.rs"
                    );
                }
            }
        }
        assert_eq!(
            seen, pinned_set,
            "{kind}: manifest and pinned export differ"
        );
        let typed_set: BTreeSet<&str> = typed(kind).iter().map(|(method, _)| *method).collect();
        let selected: BTreeSet<&str> = rows
            .iter()
            .filter(|row| matches!(row["tier"].as_str(), Some("A" | "B")))
            .map(|row| row["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            typed_set, selected,
            "{kind}: typed enum and selected rows differ"
        );
    }
    eprintln!("inventory totals: {totals:?}");
    assert_eq!(totals["clientRequests/A"] + totals["clientRequests/B"], 44);
    assert_eq!(totals["serverRequests/A"], 5);
    assert_eq!(
        totals["serverNotifications/A"] + totals["serverNotifications/B"],
        33
    );
}

#[test]
fn typed_methods_round_trip_their_wire_names() {
    for method in ClientRequestMethod::ALL {
        assert_eq!(
            ClientRequestMethod::from_method(method.method()),
            Some(*method)
        );
        let wire = serde_json::to_value(method).unwrap();
        assert_eq!(wire, method.method());
        assert_eq!(
            serde_json::from_value::<ClientRequestMethod>(wire).unwrap(),
            *method
        );
    }
    for method in ServerNotificationMethod::ALL {
        assert_eq!(
            ServerNotificationMethod::from_method(method.method()),
            Some(*method)
        );
    }
    for method in ServerRequestMethod::ALL {
        assert_eq!(
            ServerRequestMethod::from_method(method.method()),
            Some(*method)
        );
    }
    assert!(serde_json::from_value::<ClientRequestMethod>("feedback/upload".into()).is_err());
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Full,
    Minimal,
}

struct Instances {
    definitions: Value,
}

const DEPTH: usize = 5;

impl Instances {
    fn new() -> Self {
        let mut definitions = pinned()["definitions"].clone();
        definitions["codewhale"] = manifest()["extensions"]["definitions"].clone();
        Instances { definitions }
    }

    fn resolve(&self, reference: &str) -> &Value {
        let mut node = &self.definitions;
        for part in reference.trim_start_matches("#/definitions/").split('/') {
            node = &node[part];
        }
        assert!(!node.is_null(), "unresolved {reference}");
        node
    }

    fn lookup(&self, name: &str) -> Option<(String, &Value)> {
        for namespace in ["v2/", "", "codewhale/"] {
            let reference = format!("#/definitions/{namespace}{name}");
            let mut node = &self.definitions;
            for part in reference.trim_start_matches("#/definitions/").split('/') {
                node = &node[part];
            }
            if !node.is_null() && !(namespace.is_empty() && name == "v2") {
                return Some((reference, node));
            }
        }
        None
    }

    fn validator(&self, reference: &str) -> jsonschema::Validator {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "definitions": self.definitions,
            "allOf": [{"$ref": reference}],
        });
        jsonschema::validator_for(&schema).unwrap()
    }

    fn members<'a>(&self, schema: &'a Value) -> Option<&'a Vec<Value>> {
        schema["oneOf"].as_array().or(schema["anyOf"].as_array())
    }

    fn value(&self, schema: &Value, mode: Mode, depth: usize) -> Value {
        if schema == &Value::Bool(true) {
            return json!({"any": [1, "x"]});
        }
        if let Some(reference) = schema["$ref"].as_str() {
            return self.value(self.resolve(reference), mode, depth + 1);
        }
        if let Some(all) = schema["allOf"].as_array() {
            return self.value(&all[0], mode, depth);
        }
        if let Some(members) = self.members(schema) {
            let member = members
                .iter()
                .find(|member| member["type"] != "null")
                .unwrap();
            return self.member_value(schema, member, mode, depth);
        }
        if let Some(values) = schema["enum"].as_array() {
            return values[0].clone();
        }
        let ty = match &schema["type"] {
            Value::String(ty) => ty.clone(),
            Value::Array(list) => list
                .iter()
                .find(|ty| *ty != "null")
                .and_then(Value::as_str)
                .unwrap()
                .to_owned(),
            _ => return json!({"any": [1, "x"]}),
        };
        match ty.as_str() {
            "string" => json!("s"),
            "integer" => json!(1),
            "boolean" => json!(true),
            "array" => {
                if mode == Mode::Full && depth < DEPTH {
                    json!([self.value(&schema["items"], mode, depth + 1)])
                } else {
                    json!([])
                }
            }
            "object" => self.object(schema, mode, depth),
            other => panic!("unsupported type {other}"),
        }
    }

    fn object(&self, schema: &Value, mode: Mode, depth: usize) -> Value {
        let mut out = Map::new();
        let required: BTreeSet<&str> = schema["required"]
            .as_array()
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let full = mode == Mode::Full && depth < DEPTH;
        if let Some(properties) = schema["properties"].as_object() {
            for (name, property) in properties {
                if required.contains(name.as_str()) || full {
                    out.insert(name.clone(), self.value(property, mode, depth + 1));
                } else if let Some(default) =
                    property.get("default").filter(|value| !value.is_null())
                {
                    out.insert(name.clone(), default.clone());
                }
            }
            if full && schema["additionalProperties"] == true {
                out.insert("x-additional".into(), json!(1));
            }
        } else if full {
            match &schema["additionalProperties"] {
                Value::Object(_) => {
                    out.insert(
                        "k".into(),
                        self.value(&schema["additionalProperties"], mode, depth + 1),
                    );
                }
                Value::Bool(true) => {
                    out.insert("k".into(), json!(1));
                }
                _ => {}
            }
        }
        Value::Object(out)
    }

    fn member_value(&self, schema: &Value, member: &Value, mode: Mode, depth: usize) -> Value {
        let mut value = self.value(member, mode, depth);
        if schema.get("properties").is_some()
            && let (Value::Object(base), Value::Object(extra)) =
                (self.object(schema, mode, depth), &mut value)
        {
            for (key, field) in base {
                extra.entry(key).or_insert(field);
            }
        }
        value
    }

    fn cases(&self, schema: &Value) -> Vec<(String, Value)> {
        let mut cases = vec![
            ("full".to_owned(), self.value(schema, Mode::Full, 0)),
            ("minimal".to_owned(), self.value(schema, Mode::Minimal, 0)),
        ];
        if let Some(members) = self.members(schema) {
            for (index, member) in members.iter().enumerate() {
                if member["type"] == "null" {
                    continue;
                }
                if let Some(values) = member["enum"].as_array() {
                    for value in values {
                        cases.push((format!("member {index} {value}"), value.clone()));
                    }
                } else {
                    cases.push((
                        format!("member {index} full"),
                        self.member_value(schema, member, Mode::Full, 0),
                    ));
                    cases.push((
                        format!("member {index} minimal"),
                        self.member_value(schema, member, Mode::Minimal, 0),
                    ));
                }
            }
        } else if let Some(values) = schema["enum"].as_array() {
            for value in values {
                cases.push((format!("enum {value}"), value.clone()));
            }
        }
        cases
    }
}

#[test]
fn every_selected_shape_round_trips_against_the_pinned_schema() {
    let instances = Instances::new();
    let mut checked = 0usize;
    let mut typed = 0usize;
    let mut inline = Vec::new();
    for name in v2::TYPE_NAMES {
        let Some((reference, schema)) = instances.lookup(name) else {
            inline.push(*name);
            continue;
        };
        typed += 1;
        let validator = instances.validator(&reference);
        for (case, value) in instances.cases(schema) {
            assert!(
                validator.is_valid(&value),
                "{name} {case}: generated instance is not valid upstream: {value}"
            );
            let again = v2::round_trip(name, value.clone())
                .unwrap()
                .unwrap_or_else(|error| {
                    panic!("{name} {case}: typed decode failed: {error}: {value}")
                });
            assert!(
                validator.is_valid(&again),
                "{name} {case}: typed output is not valid upstream: {again}"
            );
            assert_eq!(
                again, value,
                "{name} {case}: typed round trip changed the value"
            );
            checked += 1;
        }
    }
    eprintln!(
        "typed definitions with a pinned schema: {typed}; inline helper types: {}; round-tripped instances: {checked}",
        inline.len()
    );
    assert!(typed > 250 && checked > typed * 2);
}

#[test]
fn typed_envelopes_match_the_pinned_unions_without_jsonrpc() {
    let instances = Instances::new();
    let client_request = instances.validator("#/definitions/ClientRequest");
    let server_request = instances.validator("#/definitions/ServerRequest");
    let server_notification = instances.validator("#/definitions/ServerNotification");
    let client_notification = instances.validator("#/definitions/ClientNotification");
    let mut counts = BTreeMap::new();
    for method in ClientRequestMethod::ALL {
        let (_, params_schema) = instances.lookup(method.params_type().unwrap()).unwrap();
        let (_, result_schema) = instances.lookup(method.response_type()).unwrap();
        for mode in [Mode::Full, Mode::Minimal] {
            let params = instances.value(params_schema, mode, 0);
            let envelope = json!({"id": 7, "method": method.method(), "params": params});
            assert!(client_request.is_valid(&envelope), "{method}: {envelope}");
            let call: RpcCall = serde_json::from_value(envelope).unwrap();
            let typed: v2::ClientRequestParams =
                serde_json::from_value(json!({"method": call.method, "params": call.params}))
                    .unwrap_or_else(|error| panic!("{method}: {error}"));
            assert_eq!(typed.method(), *method);
            let encoded = serde_json::to_value(&typed).unwrap();
            assert!(encoded.get("jsonrpc").is_none());
            assert_eq!(encoded["params"], params);
            let result = instances.value(result_schema, mode, 0);
            let response = v2::ClientResponse::decode(*method, result.clone()).unwrap();
            assert_eq!(response.method(), *method);
            assert_eq!(serde_json::to_value(&response).unwrap(), result);
        }
        *counts.entry("client requests").or_insert(0) += 1;
    }
    for method in ServerRequestMethod::ALL {
        let (_, params_schema) = instances.lookup(method.params_type().unwrap()).unwrap();
        let (_, result_schema) = instances.lookup(method.response_type()).unwrap();
        for mode in [Mode::Full, Mode::Minimal] {
            let params = instances.value(params_schema, mode, 0);
            let typed: v2::ServerRequestParams =
                serde_json::from_value(json!({"method": method.method(), "params": params}))
                    .unwrap();
            let mut encoded = serde_json::to_value(&typed).unwrap();
            encoded["id"] = json!("server-1");
            assert!(server_request.is_valid(&encoded), "{method}: {encoded}");
            let result = instances.value(result_schema, mode, 0);
            let answer = v2::ServerRequestResponse::decode(*method, result.clone()).unwrap();
            assert_eq!(answer.method(), *method);
            assert_eq!(serde_json::to_value(&answer).unwrap(), result);
        }
        *counts.entry("server requests").or_insert(0) += 1;
    }
    for method in ServerNotificationMethod::ALL {
        let (_, params_schema) = instances.lookup(method.params_type().unwrap()).unwrap();
        for mode in [Mode::Full, Mode::Minimal] {
            let params = instances.value(params_schema, mode, 0);
            let typed: v2::ServerNotification =
                serde_json::from_value(json!({"method": method.method(), "params": params}))
                    .unwrap();
            assert_eq!(typed.method(), *method);
            let encoded = serde_json::to_value(&typed).unwrap();
            assert!(encoded.get("jsonrpc").is_none() && encoded.get("id").is_none());
            assert!(
                server_notification.is_valid(&encoded),
                "{method}: {encoded}"
            );
        }
        *counts.entry("server notifications").or_insert(0) += 1;
    }
    let initialized = serde_json::to_value(v2::ClientNotification::Initialized).unwrap();
    assert_eq!(initialized, json!({"method": "initialized"}));
    assert!(client_notification.is_valid(&initialized));
    eprintln!("typed envelopes checked: {counts:?}");
    assert_eq!(counts["client requests"], 44);
    assert_eq!(counts["server requests"], 5);
    assert_eq!(counts["server notifications"], 33);
}

#[test]
fn tier_c_and_undecided_methods_are_method_not_found_and_never_typed() {
    let manifest = manifest();
    let mut refused = 0;
    for (kind, _, _) in KINDS {
        for row in manifest["methods"][kind].as_array().unwrap() {
            if matches!(row["tier"].as_str(), Some("A" | "B")) {
                continue;
            }
            let method = row["method"].as_str().unwrap();
            assert!(!disposition(kind, method).unwrap().selected());
            let envelope = json!({"method": method, "params": {}});
            match kind {
                "clientRequests" => {
                    assert_eq!(
                        methods::unselected_request_error(method),
                        Some(error_code::METHOD_NOT_FOUND)
                    );
                    assert!(ClientRequestMethod::from_method(method).is_none());
                    assert!(serde_json::from_value::<v2::ClientRequestParams>(envelope).is_err());
                }
                "serverRequests" => {
                    assert!(ServerRequestMethod::from_method(method).is_none());
                    assert!(serde_json::from_value::<v2::ServerRequestParams>(envelope).is_err());
                }
                "serverNotifications" => {
                    assert!(ServerNotificationMethod::from_method(method).is_none());
                    assert!(serde_json::from_value::<v2::ServerNotification>(envelope).is_err());
                }
                _ => {}
            }
            refused += 1;
        }
    }
    assert_eq!(error_code::METHOD_NOT_FOUND, -32601);
    assert_eq!(methods::unselected_request_error("thread/start"), None);
    assert_eq!(
        methods::unselected_request_error("not/in/the/export"),
        Some(error_code::METHOD_NOT_FOUND)
    );
    eprintln!("unselected methods refused: {refused}");
    assert!(refused > 100);
}

#[test]
fn codewhale_extensions_stay_optional_and_valid_upstream() {
    let instances = Instances::new();
    let thread = instances.validator("#/definitions/v2/Thread");
    let (_, schema) = instances.lookup("Thread").unwrap();
    let mut value = instances.value(schema, Mode::Minimal, 0);
    let plain: v2::Thread = serde_json::from_value(value.clone()).unwrap();
    assert!(plain.executor.is_none() && plain.environment.is_none());
    assert_eq!(serde_json::to_value(&plain).unwrap(), value);
    value["executor"] = json!({"kind": "acp", "agentId": "claude", "agentName": "Claude"});
    value["environment"] = json!({"id": "env-1", "providerKind": "remote"});
    assert!(thread.is_valid(&value));
    let placed: v2::Thread = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(placed.executor.as_ref().unwrap().agent_id, "claude");
    assert_eq!(serde_json::to_value(&placed).unwrap(), value);
    let refusal = instances.validator("#/definitions/codewhale/CapabilityRefusalData");
    let data = serde_json::to_value(v2::CapabilityRefusalData {
        codewhale_refusal: v2::CapabilityRefusal {
            code: v2::CapabilityRefusalCode::EnvironmentNotSupported,
            method: Some("turn/start".into()),
            thread_id: Some("thread-1".into()),
        },
    })
    .unwrap();
    assert!(refusal.is_valid(&data));
    assert_eq!(
        data["codewhaleRefusal"]["code"],
        "environment_not_supported"
    );
    assert_eq!(error_code::CAPABILITY_UNSUPPORTED, -32020);
    assert!(
        serde_json::from_value::<v2::CapabilityRefusal>(json!({"code": "no_acp", "token": "x"}))
            .is_err()
    );
}
