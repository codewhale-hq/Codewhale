//! Prompt-bytes family: the model-visible prefix of a real first request.
//!
//! `docs/CACHE.md`: the system prompt and tool catalog are a session-pinned
//! KV-cache prefix, so a migration that moves any contributor (a tool adapter,
//! an MCP catalog, a slash command) must not change those bytes by accident.
//! Each case runs one real turn through the engine with the scripted provider
//! and captures the first `MessageRequest` — the exact prefix production would
//! send — then pins it three ways:
//!
//! - `<case>.golden.json`: sha256 of the system prompt, the tool catalog and
//!   the combined prefix (bytes as serialized, key order included), sizes, and
//!   the tool names in catalog order;
//! - `<case>.system.golden.txt`: the flat system prompt, readable in review;
//! - `<case>.tools.golden.json`: the tool catalog, readable in review.
//!
//! Masks are only the genuinely host-specific facts: temp paths, and the
//! environment block's `platform` / `shell` lines. Tools the catalog admits
//! only after probing the host for a binary are removed from the pinned
//! catalog and pinned one by one in `host_probed_tools.golden.json`, checked
//! whenever the probe succeeds on the running host.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::events::run_scripted_turn;
use super::golden::{self, Failures, Sandbox};

const FAMILY: &str = "prompt";

/// Tools registered only when a host binary or OS service is found
/// (`tool_catalog.rs` python probe, `registry.rs` pandoc / OCR probes,
/// `js_execution`'s node probe).
const HOST_PROBED_TOOLS: &[&str] = &[
    "code_execution",
    "image_ocr",
    "js_execution",
    "pandoc_convert",
];

fn default_script() -> Value {
    json!([{ "events": [
        { "type": "message_start", "message": {
            "id": "msg_prompt", "type": "message", "role": "assistant", "content": [],
            "model": "conformance", "stop_reason": null, "stop_sequence": null,
            "usage": { "input_tokens": 0, "output_tokens": 0 } } },
        { "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } },
        { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "ok" } },
        { "type": "content_block_stop", "index": 0 },
        { "type": "message_delta", "delta": { "stop_reason": "end_turn" } },
        { "type": "message_stop" }
    ] }])
}

struct Prefix {
    system_json: String,
    system_flat: String,
    system_blocks: usize,
    tools: Vec<Value>,
    probed: BTreeMap<String, Value>,
}

fn mask_host_lines(text: &str) -> String {
    let platform = regex::Regex::new(r"(?m)^- platform: .*$").expect("platform regex");
    let shell = regex::Regex::new(r"(?m)^- shell: .*$").expect("shell regex");
    let text = platform.replace_all(text, "- platform: <PLATFORM>");
    shell.replace_all(&text, "- shell: <SHELL>").into_owned()
}

/// Apply [`mask_host_lines`] inside every string of a JSON tree (the lines
/// live inside JSON strings, where serialization escapes their newlines).
fn mask_host_value(value: &mut Value) {
    match value {
        Value::String(text) => *text = mask_host_lines(text),
        Value::Array(items) => items.iter_mut().for_each(mask_host_value),
        Value::Object(map) => map.values_mut().for_each(mask_host_value),
        _ => {}
    }
}

fn capture_prefix(name: &str, case: &Value) -> Result<Prefix, String> {
    let sandbox = Sandbox::new(case);
    let script = case
        .get("provider_script")
        .cloned()
        .unwrap_or_else(default_script);
    let (record, provider) = run_scripted_turn(&sandbox, case, &script);
    if let Some(error) = record.failure {
        return Err(error);
    }
    let requests = provider.captured();
    let first = requests
        .first()
        .ok_or_else(|| format!("{name}: the engine sent no model request"))?;
    let mut masker = sandbox.masker(&[]);

    let mut system = serde_json::to_value(&first.system).expect("system serializes");
    masker.value(&mut system);
    mask_host_value(&mut system);
    let system_json = serde_json::to_string(&system).expect("system json");
    let system_flat = first
        .system
        .as_ref()
        .map(crate::prompts::system_prompt_flat_text)
        .unwrap_or_default();
    let system_flat = mask_host_lines(&masker.text(&system_flat));
    let system_blocks = match &first.system {
        Some(codewhale_models::SystemPrompt::Blocks(blocks)) => blocks.len(),
        Some(codewhale_models::SystemPrompt::Text(_)) => 1,
        None => 0,
    };
    if system_blocks == 0 {
        return Err(format!("{name}: the captured system prefix is empty"));
    }

    let mut tools = Vec::new();
    let mut probed = BTreeMap::new();
    for tool in first.tools.iter().flatten() {
        let mut value = serde_json::to_value(tool).expect("tool serializes");
        masker.value(&mut value);
        mask_host_value(&mut value);
        if HOST_PROBED_TOOLS.contains(&tool.name.as_str()) {
            probed.insert(tool.name.clone(), value);
        } else {
            tools.push(value);
        }
    }
    if tools.is_empty() && probed.is_empty() {
        return Err(format!("{name}: the captured tool catalog is empty"));
    }
    Ok(Prefix {
        system_json,
        system_flat,
        system_blocks,
        tools,
        probed,
    })
}

fn summary(prefix: &Prefix) -> Value {
    let tools_json = serde_json::to_string(&prefix.tools).expect("tools json");
    let combined = format!("{}\n{}", prefix.system_json, tools_json);
    json!({
        "system_sha256": codewhale_runtime::hashing::sha256_hex(prefix.system_json.as_bytes()),
        "system_bytes": prefix.system_json.len(),
        "system_blocks": prefix.system_blocks,
        "tools_sha256": codewhale_runtime::hashing::sha256_hex(tools_json.as_bytes()),
        "tools_bytes": tools_json.len(),
        "tool_count": prefix.tools.len(),
        "tool_names": prefix.tools.iter().map(|tool| tool["name"].clone()).collect::<Vec<_>>(),
        "prefix_sha256": codewhale_runtime::hashing::sha256_hex(combined.as_bytes()),
        "masks": ["<WORKSPACE>", "<HOME>", "<TMP>", "- platform: <PLATFORM>", "- shell: <SHELL>"],
        "host_probed_tools_excluded": HOST_PROBED_TOOLS,
    })
}

#[test]
fn model_visible_prefix_bytes_match_goldens() {
    let dir = golden::family_dir(FAMILY);
    let names = golden::case_names(FAMILY);
    let mut failures = Failures::default();
    let mut probed_seen: BTreeMap<String, Value> = BTreeMap::new();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        let prefix = match capture_prefix(name, &case) {
            Ok(prefix) => prefix,
            Err(message) => {
                failures.push(name, message);
                continue;
            }
        };
        // A prefix that differs between two identical sessions is a cache
        // bug in its own right (map iteration order, a clock, a counter).
        match capture_prefix(name, &case) {
            Ok(again) if again.system_json == prefix.system_json && again.tools == prefix.tools => {
            }
            Ok(_) => {
                failures.push(
                    name,
                    "the prefix differs between two identical sessions in one process",
                );
                continue;
            }
            Err(message) => {
                failures.push(name, message);
                continue;
            }
        }
        for (tool, definition) in &prefix.probed {
            if let Some(previous) = probed_seen.get(tool)
                && previous != definition
            {
                failures.push(
                    name,
                    format!("host-probed tool `{tool}` differs across cases"),
                );
            }
            probed_seen.insert(tool.clone(), definition.clone());
        }
        failures.record(
            name,
            golden::check_golden(
                &dir.join(format!("{name}.golden.json")),
                &golden::pretty(&summary(&prefix)),
            ),
        );
        failures.record(
            name,
            golden::check_golden(
                &dir.join(format!("{name}.system.golden.txt")),
                &format!("{}\n", prefix.system_flat),
            ),
        );
        failures.record(
            name,
            golden::check_golden(
                &dir.join(format!("{name}.tools.golden.json")),
                &golden::pretty(&Value::Array(prefix.tools.clone())),
            ),
        );
    }

    // Host-probed tools: every definition this host produced must match the
    // pinned one. A tool this host cannot offer is skipped, not failed.
    let probed_path = dir.join("host_probed_tools.golden.json");
    let mut pinned: BTreeMap<String, Value> = std::fs::read_to_string(&probed_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let mut drift = Vec::new();
    for (tool, definition) in &probed_seen {
        if pinned.get(tool) != Some(definition) {
            drift.push(tool.clone());
            pinned.insert(tool.clone(), definition.clone());
        }
    }
    if !drift.is_empty() {
        let merged = Value::Object(pinned.into_iter().collect());
        failures.record(
            "host_probed_tools",
            golden::check_golden(&probed_path, &golden::pretty(&merged)).map_err(|message| {
                format!("host-probed tool definition(s) {drift:?} drifted: {message}")
            }),
        );
    }
    let skipped: Vec<&str> = HOST_PROBED_TOOLS
        .iter()
        .copied()
        .filter(|tool| !probed_seen.contains_key(*tool))
        .collect();
    if !skipped.is_empty() {
        eprintln!(
            "conformance: host cannot offer {skipped:?}; their pinned definitions were not checked"
        );
    }
    failures.finish(FAMILY, names.len());
}
