//! Symbolic handle storage and bounded reads.
//!
//! `var_handle` is the shared protocol that lets expensive environments
//! (RLM sessions, sub-agent transcripts, large artifacts) hand the parent a
//! small symbolic reference instead of copying the whole payload into the
//! parent transcript.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

/// Ceiling on everything the handle store holds in memory (#5472 findings 4-5).
///
/// Handles exist so an expensive payload never enters the parent transcript,
/// but the payload still lives here. Eviction was per-session only
/// (`evict_session`), driven by sub-agent retirement — so RLM sessions, whose
/// producers have no such lifecycle, had no eviction path at all, and a long
/// session accumulated every handle it ever minted. Past this budget the
/// least-recently-inserted records are dropped; `handle_read` on an evicted
/// handle already reports "not found" rather than inventing content.
const HANDLE_STORE_MAX_BYTES: usize = 64 * 1024 * 1024;

const DEFAULT_MAX_CHARS: usize = 12_000;
const HARD_MAX_CHARS: usize = 50_000;
const REPR_PREVIEW_CHARS: usize = 160;

pub type SharedHandleStore = Arc<Mutex<HandleStore>>;

#[must_use]
pub fn new_shared_handle_store() -> SharedHandleStore {
    Arc::new(Mutex::new(HandleStore::default()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VarHandle {
    pub kind: String,
    pub session_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub length: usize,
    pub repr_preview: String,
    pub sha256: String,
}

impl VarHandle {
    #[must_use]
    pub fn key(&self) -> HandleKey {
        HandleKey {
            session_id: self.session_id.clone(),
            name: self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandleKey {
    pub session_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct HandleRecord {
    pub handle: VarHandle,
    pub value: HandleValue,
    /// Insertion order, for least-recently-inserted eviction under the store's
    /// byte budget. `HashMap` has no order of its own.
    seq: u64,
    /// Payload size, measured once at insert so the budget sweep does not
    /// re-serialize every JSON value it inspects.
    bytes: usize,
}

#[derive(Debug, Clone)]
pub enum HandleValue {
    Text(String),
    Json(Value),
}

impl HandleValue {
    fn length(&self) -> usize {
        match self {
            Self::Text(text) => text.chars().count(),
            Self::Json(Value::Array(items)) => items.len(),
            Self::Json(Value::Object(map)) => map.len(),
            Self::Json(value) => value.to_string().chars().count(),
        }
    }

    fn type_name(&self) -> String {
        match self {
            Self::Text(_) => "str".to_string(),
            Self::Json(Value::Array(_)) => "list".to_string(),
            Self::Json(Value::Object(_)) => "dict".to_string(),
            Self::Json(Value::String(_)) => "str".to_string(),
            Self::Json(Value::Bool(_)) => "bool".to_string(),
            Self::Json(Value::Number(_)) => "number".to_string(),
            Self::Json(Value::Null) => "null".to_string(),
        }
    }

    fn stable_bytes(&self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.as_bytes().to_vec(),
            Self::Json(value) => serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    fn repr_preview(&self) -> String {
        match self {
            Self::Text(text) => truncate_chars(text, REPR_PREVIEW_CHARS),
            Self::Json(value) => truncate_chars(&value.to_string(), REPR_PREVIEW_CHARS),
        }
    }
}

#[derive(Debug, Default)]
pub struct HandleStore {
    records: HashMap<HandleKey, HandleRecord>,
    next_seq: u64,
    retained_bytes: usize,
}

impl HandleStore {
    #[must_use]
    pub fn insert_text(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> VarHandle {
        self.insert(session_id, name, HandleValue::Text(text.into()))
    }

    #[must_use]
    pub fn insert_json(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        value: Value,
    ) -> VarHandle {
        self.insert(session_id, name, HandleValue::Json(value))
    }

    #[must_use]
    pub fn get(&self, handle: &VarHandle) -> Option<&HandleRecord> {
        self.records.get(&handle.key())
    }

    /// Host-owned named JSON, without minting a model-facing var_handle.
    pub(crate) fn get_json_named(&self, namespace: &str, name: &str) -> Option<&Value> {
        let record = self.records.get(&HandleKey {
            session_id: namespace.to_string(),
            name: name.to_string(),
        })?;
        match &record.value {
            HandleValue::Json(value) => Some(value),
            HandleValue::Text(_) => None,
        }
    }

    /// Remove all handles for `session_id`. Called when an agent's records
    /// are retired so resident transcript payloads are freed without waiting
    /// for a full session reset (#3885).
    pub fn evict_session(&mut self, session_id: &str) {
        let mut freed = 0usize;
        self.records.retain(|key, record| {
            if key.session_id == session_id {
                freed = freed.saturating_add(record.bytes);
                false
            } else {
                true
            }
        });
        self.retained_bytes = self.retained_bytes.saturating_sub(freed);
    }

    fn insert(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        value: HandleValue,
    ) -> VarHandle {
        let session_id = session_id.into();
        let name = name.into();
        let handle = VarHandle {
            kind: "var_handle".to_string(),
            session_id: session_id.clone(),
            name: name.clone(),
            type_name: value.type_name(),
            length: value.length(),
            repr_preview: value.repr_preview(),
            sha256: sha256_hex(&value.stable_bytes()),
        };
        let key = HandleKey { session_id, name };
        let bytes = value.stable_bytes().len();
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        if let Some(replaced) = self.records.insert(
            key,
            HandleRecord {
                handle: handle.clone(),
                value,
                seq,
                bytes,
            },
        ) {
            self.retained_bytes = self.retained_bytes.saturating_sub(replaced.bytes);
        }
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        self.enforce_byte_budget();
        handle
    }

    /// Drop least-recently-inserted records until the store fits its budget.
    fn enforce_byte_budget(&mut self) {
        if self.retained_bytes <= HANDLE_STORE_MAX_BYTES {
            return;
        }
        let mut oldest_first: Vec<(u64, HandleKey)> = self
            .records
            .iter()
            .map(|(key, record)| (record.seq, key.clone()))
            .collect();
        oldest_first.sort_unstable_by_key(|(seq, _)| *seq);
        for (_, key) in oldest_first {
            if self.retained_bytes <= HANDLE_STORE_MAX_BYTES {
                break;
            }
            if let Some(removed) = self.records.remove(&key) {
                self.retained_bytes = self.retained_bytes.saturating_sub(removed.bytes);
            }
        }
    }

    /// Bytes currently held across every session. Exercised from the
    /// tests below; no production caller today.
    #[cfg_attr(not(test), expect(dead_code))]
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

/// `handle_read` is deferred on the default catalog, so model-facing text
/// that points at it also teaches how to activate it (#6747).
pub(crate) const HANDLE_READ_ACTIVATION_HINT: &str =
    "if `handle_read` is not in your tool list, load it with `tool_search` first";

pub struct HandleReadTool;

#[async_trait]
impl ToolSpec for HandleReadTool {
    fn name(&self) -> &'static str {
        "handle_read"
    }

    fn description(&self) -> &'static str {
        "Read a bounded projection from a var_handle returned by tools such \
         as RLM sessions or sub-agents. This does not read artifact ids \
         (`art_...`), tool-call ids (`call_...`), SHA refs, or files; use \
         retrieve_tool_result for spilled tool results/artifacts and \
         `read` (path=...) for workspace files. Provide \
         exactly one projection: `slice` for char/line slices, `range` for \
         one-based line ranges, `count` for metadata counts, or `jsonpath` \
         for a small JSON-path projection. This retrieves from the handle's \
         backing environment instead of asking the parent transcript to hold \
         the full payload."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["handle"],
            "properties": {
                "handle": {
                    "description": "A var_handle object, or a compact `session_id/name` string. Not an `art_...`, `call_...`, SHA, or file path ref.",
                    "oneOf": [
                        {
                            "type": "object",
                            "required": ["kind", "session_id", "name"],
                            "properties": {
                                "kind": { "type": "string", "const": "var_handle" },
                                "session_id": { "type": "string" },
                                "name": { "type": "string" },
                                "type": { "type": "string" },
                                "length": { "type": "integer" },
                                "repr_preview": { "type": "string" },
                                "sha256": { "type": "string" }
                            }
                        },
                        { "type": "string" }
                    ]
                },
                "slice": {
                    "type": "object",
                    "description": "Zero-based half-open slice over chars or lines.",
                    "properties": {
                        "start": { "type": "integer", "minimum": 0 },
                        "end": { "type": "integer", "minimum": 0 },
                        "unit": { "type": "string", "enum": ["chars", "lines"], "default": "chars" }
                    }
                },
                "range": {
                    "type": "object",
                    "description": "One-based inclusive line range.",
                    "required": ["start", "end"],
                    "properties": {
                        "start": { "type": "integer", "minimum": 1 },
                        "end": { "type": "integer", "minimum": 1 }
                    }
                },
                "count": {
                    "type": "boolean",
                    "description": "Return counts for the handle payload."
                },
                "jsonpath": {
                    "type": "string",
                    "description": "Small JSONPath subset: $, .field, [index], [*], and ['field']."
                },
                "introspect": {
                    "type": "boolean",
                    "description": "Return supported projections, size hints, and copy-pasteable examples for this handle."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return in this projection. Defaults to 12000; hard-capped at 50000."
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let handle = parse_handle(
            input
                .get("handle")
                .ok_or_else(|| ToolError::missing_field("handle"))?,
        )?;
        let projection = parse_projection(&input)?;
        let max_chars = input
            .get("max_chars")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).min(HARD_MAX_CHARS))
            .unwrap_or(DEFAULT_MAX_CHARS);

        let store = context.runtime.handle_store.lock().await;
        let record = store.get(&handle).ok_or_else(|| {
            ToolError::invalid_input(format!(
                "handle_read: no payload found for handle {}/{}",
                handle.session_id, handle.name
            ))
        })?;
        if !handle.sha256.is_empty() && handle.sha256 != record.handle.sha256 {
            return Err(ToolError::invalid_input(
                "handle_read: handle sha256 does not match stored payload",
            ));
        }

        let output = match projection {
            Projection::Count => count_projection(record),
            Projection::Slice { start, end, unit } => {
                slice_projection(record, start, end, unit, max_chars)
            }
            Projection::Range { start, end } => {
                line_range_projection(record, start, end, max_chars)
            }
            Projection::JsonPath(path) => jsonpath_projection(record, &path, max_chars)?,
            Projection::Introspect => introspect_projection(record),
        };

        ToolResult::json(&output).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[derive(Debug, Clone, Copy)]
enum SliceUnit {
    Chars,
    Lines,
}

enum Projection {
    Count,
    Slice {
        start: usize,
        end: Option<usize>,
        unit: SliceUnit,
    },
    Range {
        start: usize,
        end: usize,
    },
    JsonPath(String),
    Introspect,
}

fn parse_handle(value: &Value) -> Result<VarHandle, ToolError> {
    if let Some(raw) = value.as_str() {
        if looks_like_tool_result_ref(raw) {
            return Err(ToolError::invalid_input(
                "handle_read only accepts var_handle objects or `session_id/name` strings. \
                 This looks like an artifact/tool-result ref; use `retrieve_tool_result` instead.",
            ));
        }
        let Some((session_id, name)) = raw.rsplit_once('/') else {
            return Err(ToolError::invalid_input(
                "handle_read: string handles must use `session_id/name`. \
                 For `art_...`, `call_...`, SHA, or file refs, use `retrieve_tool_result`.",
            ));
        };
        return Ok(VarHandle {
            kind: "var_handle".to_string(),
            session_id: session_id.to_string(),
            name: name.to_string(),
            type_name: String::new(),
            length: 0,
            repr_preview: String::new(),
            sha256: String::new(),
        });
    }

    let handle: VarHandle = serde_json::from_value(value.clone()).map_err(|e| {
        ToolError::invalid_input(format!("handle_read: invalid var_handle object: {e}"))
    })?;
    if handle.kind != "var_handle" {
        return Err(ToolError::invalid_input(
            "handle_read: handle.kind must be `var_handle`",
        ));
    }
    if handle.session_id.trim().is_empty() || handle.name.trim().is_empty() {
        return Err(ToolError::invalid_input(
            "handle_read: handle.session_id and handle.name must be non-empty",
        ));
    }
    Ok(handle)
}

fn looks_like_tool_result_ref(raw: &str) -> bool {
    let trimmed = raw.trim();
    let sha_candidate = trimmed
        .strip_prefix("sha:")
        .or_else(|| trimmed.strip_prefix("sha_"))
        .unwrap_or(trimmed);
    trimmed.starts_with("art_")
        || trimmed.starts_with("call_")
        || trimmed.starts_with("tool_result:")
        || trimmed.ends_with(".txt")
        || crate::tools::truncate::is_valid_sha256(&sha_candidate.to_ascii_lowercase())
}

fn parse_projection(input: &Value) -> Result<Projection, ToolError> {
    let mut count = 0usize;
    count += usize::from(input.get("slice").is_some());
    count += usize::from(input.get("range").is_some());
    count += usize::from(input.get("count").and_then(Value::as_bool).unwrap_or(false));
    count += usize::from(input.get("jsonpath").is_some());
    count += usize::from(
        input
            .get("introspect")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    if count != 1 {
        return Err(ToolError::invalid_input(projection_usage_hint()));
    }

    if input
        .get("introspect")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(Projection::Introspect);
    }
    if input.get("count").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(Projection::Count);
    }
    if let Some(path) = input.get("jsonpath") {
        let path = path
            .as_str()
            .ok_or_else(|| ToolError::invalid_input("handle_read: jsonpath must be a string"))?
            .trim();
        if path.is_empty() {
            return Err(ToolError::invalid_input(
                "handle_read: jsonpath must not be empty",
            ));
        }
        return Ok(Projection::JsonPath(path.to_string()));
    }
    if let Some(slice) = input.get("slice") {
        let start = slice.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
        let end = slice.get("end").and_then(Value::as_u64).map(|n| n as usize);
        if let Some(end) = end
            && end < start
        {
            return Err(ToolError::invalid_input(
                "handle_read: slice.end must be greater than or equal to slice.start",
            ));
        }
        let unit = match slice.get("unit").and_then(Value::as_str).unwrap_or("chars") {
            "chars" => SliceUnit::Chars,
            "lines" => SliceUnit::Lines,
            other => {
                return Err(ToolError::invalid_input(format!(
                    "handle_read: unsupported slice.unit `{other}`"
                )));
            }
        };
        return Ok(Projection::Slice { start, end, unit });
    }
    let range = input
        .get("range")
        .ok_or_else(|| ToolError::invalid_input("handle_read: missing projection"))?;
    let start = range
        .get("start")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::missing_field("range.start"))? as usize;
    let end = range
        .get("end")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::missing_field("range.end"))? as usize;
    if start == 0 || end == 0 || end < start {
        return Err(ToolError::invalid_input(
            "handle_read: range is one-based inclusive and end must be >= start",
        ));
    }
    Ok(Projection::Range { start, end })
}

fn projection_usage_hint() -> String {
    "handle_read: provide exactly one projection: `slice`, `range`, `count: true`, `jsonpath`, or `introspect: true`. \
     Examples: {\"handle\":{\"kind\":\"var_handle\",\"session_id\":\"rlm:abc\",\"name\":\"final_1\"},\"slice\":{\"start\":0,\"end\":500}}; \
     {\"handle\":\"rlm:abc/final_1\",\"count\":true}; \
     {\"handle\":\"rlm:abc/final_1\",\"introspect\":true}."
        .to_string()
}

fn count_projection(record: &HandleRecord) -> Value {
    match &record.value {
        HandleValue::Text(text) => json!({
            "handle": record.handle,
            "projection": "count",
            "chars": text.chars().count(),
            "lines": text.lines().count(),
            "bytes": text.len(),
        }),
        HandleValue::Json(value) => {
            let bytes = {
                let mut cw = crate::utils::CountingWriter::new();
                let _ = serde_json::to_writer(&mut cw, value);
                cw.count()
            };
            json!({
                "handle": record.handle,
                "projection": "count",
                "json_type": json_type(value),
                "length": record.handle.length,
                "bytes": bytes,
            })
        }
    }
}

fn introspect_projection(record: &HandleRecord) -> Value {
    let string_handle = format!("{}/{}", record.handle.session_id, record.handle.name);
    let object_handle = json!(record.handle.clone());
    let mut projections = vec![
        json!({"name": "count", "example": {"handle": string_handle, "count": true}}),
        json!({"name": "slice_chars", "example": {"handle": object_handle.clone(), "slice": {"start": 0, "end": 500}}}),
        json!({"name": "range_lines", "example": {"handle": object_handle.clone(), "range": {"start": 1, "end": 20}}}),
    ];
    if matches!(record.value, HandleValue::Json(_)) {
        projections.push(
            json!({"name": "jsonpath", "example": {"handle": object_handle, "jsonpath": "$"}}),
        );
    }

    json!({
        "handle": record.handle,
        "projection": "introspect",
        "value_type": match &record.value {
            HandleValue::Text(_) => "text",
            HandleValue::Json(value) => json_type(value),
        },
        "length": record.handle.length,
        "repr_preview": record.handle.repr_preview,
        "projections": projections,
    })
}

fn slice_projection(
    record: &HandleRecord,
    start: usize,
    end: Option<usize>,
    unit: SliceUnit,
    max_chars: usize,
) -> Value {
    let text = record_text(record);
    match unit {
        SliceUnit::Chars => {
            let total = text.chars().count();
            let end = end.unwrap_or(total).min(total);
            let raw = char_slice(&text, start.min(total), end);
            bounded_text_projection(
                record,
                "slice",
                raw,
                max_chars,
                json!({
                    "unit": "chars",
                    "start": start.min(total),
                    "end": end,
                    "total_chars": total,
                }),
            )
        }
        SliceUnit::Lines => {
            let lines: Vec<&str> = text.lines().collect();
            let total = lines.len();
            let end = end.unwrap_or(total).min(total);
            let raw = if start >= end {
                String::new()
            } else {
                lines[start.min(total)..end].join("\n")
            };
            bounded_text_projection(
                record,
                "slice",
                raw,
                max_chars,
                json!({
                    "unit": "lines",
                    "start": start.min(total),
                    "end": end,
                    "total_lines": total,
                }),
            )
        }
    }
}

fn line_range_projection(
    record: &HandleRecord,
    start: usize,
    end: usize,
    max_chars: usize,
) -> Value {
    let text = record_text(record);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let zero_start = start.saturating_sub(1).min(total);
    let zero_end = end.min(total);
    let raw = if zero_start >= zero_end {
        String::new()
    } else {
        lines[zero_start..zero_end].join("\n")
    };
    bounded_text_projection(
        record,
        "range",
        raw,
        max_chars,
        json!({
            "start": start,
            "end": end,
            "shown_start": zero_start + 1,
            "shown_end": zero_end,
            "total_lines": total,
        }),
    )
}

fn jsonpath_projection(
    record: &HandleRecord,
    path: &str,
    max_chars: usize,
) -> Result<Value, ToolError> {
    let HandleValue::Json(value) = &record.value else {
        return Err(ToolError::invalid_input(
            "handle_read: jsonpath projection requires a JSON handle",
        ));
    };
    let matches = query_jsonpath(value, path)
        .map_err(|e| ToolError::invalid_input(format!("handle_read: {e}")))?;
    let mut payload = json!({
        "handle": record.handle,
        "projection": "jsonpath",
        "jsonpath": path,
        "count": matches.len(),
        "matches": matches,
        "truncated": false,
    });
    let rendered = serde_json::to_string(&payload).unwrap_or_default();
    if rendered.chars().count() > max_chars {
        payload["matches"] = json!([]);
        payload["preview"] = json!(truncate_chars(&rendered, max_chars));
        payload["truncated"] = json!(true);
    }
    Ok(payload)
}

fn bounded_text_projection(
    record: &HandleRecord,
    projection: &str,
    raw: String,
    max_chars: usize,
    extra: Value,
) -> Value {
    let raw_chars = raw.chars().count();
    let content = truncate_chars(&raw, max_chars);
    let shown_chars = content.chars().count();
    json!({
        "handle": record.handle,
        "projection": projection,
        "content": content,
        "truncated": shown_chars < raw_chars,
        "shown_chars": shown_chars,
        "omitted_chars": raw_chars.saturating_sub(shown_chars),
        "meta": extra,
    })
}

fn record_text(record: &HandleRecord) -> std::borrow::Cow<'_, str> {
    match &record.value {
        HandleValue::Text(text) => std::borrow::Cow::Borrowed(text),
        HandleValue::Json(value) => {
            std::borrow::Cow::Owned(serde_json::to_string_pretty(value).unwrap_or_default())
        }
    }
}

pub(crate) fn query_jsonpath(root: &Value, path: &str) -> Result<Vec<Value>, String> {
    if !path.starts_with('$') {
        return Err("jsonpath must start with `$`".to_string());
    }
    let mut idx = 1usize;
    let bytes = path.as_bytes();
    let mut current = vec![root];
    while idx < bytes.len() {
        match bytes[idx] {
            b'.' => {
                idx += 1;
                if idx < bytes.len() && bytes[idx] == b'.' {
                    return Err("recursive descent (`..`) is not supported".to_string());
                }
                let start = idx;
                while idx < bytes.len()
                    && (bytes[idx].is_ascii_alphanumeric() || bytes[idx] == b'_')
                {
                    idx += 1;
                }
                if start == idx {
                    return Err("expected field name after `.`".to_string());
                }
                let field = &path[start..idx];
                current = current
                    .into_iter()
                    .filter_map(|value| value.get(field))
                    .collect();
            }
            b'[' => {
                let Some(close_rel) = path[idx + 1..].find(']') else {
                    return Err("unterminated `[` segment".to_string());
                };
                let close = idx + 1 + close_rel;
                let token = path[idx + 1..close].trim();
                idx = close + 1;
                current = apply_bracket_token(current, token)?;
            }
            other => {
                return Err(format!(
                    "unexpected character `{}` in jsonpath",
                    other as char
                ));
            }
        }
    }
    Ok(current.into_iter().cloned().collect())
}

fn apply_bracket_token<'a>(values: Vec<&'a Value>, token: &str) -> Result<Vec<&'a Value>, String> {
    if token == "*" {
        let mut out = Vec::new();
        for value in values {
            match value {
                Value::Array(items) => out.extend(items),
                Value::Object(map) => out.extend(map.values()),
                _ => {}
            }
        }
        return Ok(out);
    }

    if let Some(field) = quoted_field(token) {
        return Ok(values
            .into_iter()
            .filter_map(|value| value.get(field))
            .collect());
    }

    let index = token
        .parse::<usize>()
        .map_err(|_| format!("unsupported bracket token `{token}`"))?;
    Ok(values
        .into_iter()
        .filter_map(|value| value.as_array().and_then(|items| items.get(index)))
        .collect())
}

fn quoted_field(token: &str) -> Option<&str> {
    if token.len() < 2 {
        return None;
    }
    let bytes = token.as_bytes();
    let quote = bytes[0];
    if !matches!(quote, b'\'' | b'"') || bytes[token.len() - 1] != quote {
        return None;
    }
    Some(&token[1..token.len() - 1])
}

fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx == max_chars {
            break;
        }
        out.push(ch);
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::hashing::sha256_hex(bytes)
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> ToolContext {
        ToolContext::new(".")
    }

    #[tokio::test]
    async fn handle_read_slices_text_by_chars() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_text("rlm:test", "matches", "abcdef")
        };

        let result = HandleReadTool
            .execute(
                json!({"handle": handle, "slice": {"start": 1, "end": 4}}),
                &ctx,
            )
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["content"], "bcd");
        assert_eq!(body["truncated"], false);
    }

    #[tokio::test]
    async fn handle_read_ranges_text_by_one_based_lines() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_text("agent:test", "transcript", "one\ntwo\nthree\nfour")
        };

        let result = HandleReadTool
            .execute(
                json!({"handle": handle, "range": {"start": 2, "end": 3}}),
                &ctx,
            )
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["content"], "two\nthree");
        assert_eq!(body["meta"]["shown_start"], 2);
        assert_eq!(body["meta"]["shown_end"], 3);
    }

    #[tokio::test]
    async fn handle_read_counts_json_collections() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_json("rlm:test", "items", json!([{"a": 1}, {"a": 2}]))
        };

        let result = HandleReadTool
            .execute(json!({"handle": handle, "count": true}), &ctx)
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["json_type"], "array");
        assert_eq!(body["length"], 2);
    }

    #[tokio::test]
    async fn handle_read_introspects_object_handle_with_examples() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_json("rlm:test", "items", json!({"items": [{"a": 1}]}))
        };

        let result = HandleReadTool
            .execute(json!({"handle": handle, "introspect": true}), &ctx)
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["projection"], "introspect");
        assert_eq!(body["handle"]["kind"], "var_handle");
        assert!(
            body["projections"]
                .as_array()
                .expect("projection examples")
                .iter()
                .any(|entry| entry["name"] == "jsonpath"),
            "json handles should advertise jsonpath examples"
        );
    }

    #[tokio::test]
    async fn handle_read_projects_jsonpath_subset() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_json(
                "rlm:test",
                "items",
                json!({"items": [{"name": "a"}, {"name": "b"}]}),
            )
        };

        let result = HandleReadTool
            .execute(
                json!({"handle": handle, "jsonpath": "$.items[*].name"}),
                &ctx,
            )
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["matches"], json!(["a", "b"]));
        assert_eq!(body["count"], 2);
    }

    #[tokio::test]
    async fn handle_read_rejects_unbounded_projection_requests() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_text("rlm:test", "body", "abc")
        };

        let err = HandleReadTool
            .execute(json!({"handle": handle}), &ctx)
            .await
            .expect_err("projection required");
        let message = err.to_string();
        assert!(message.contains("exactly one"));
        assert!(message.contains("slice"));
        assert!(message.contains("introspect"));
    }

    #[tokio::test]
    async fn handle_read_points_artifact_refs_to_tool_result_retrieval() {
        let ctx = ctx();
        let err = HandleReadTool
            .execute(json!({"handle": "art_call_abc123", "count": true}), &ctx)
            .await
            .expect_err("artifact refs are not var handles");
        let message = err.to_string();
        assert!(message.contains("retrieve_tool_result"));
        assert!(message.contains("artifact/tool-result ref"));
    }

    // === #5472 findings 4-5: the store is bounded across all sessions ===

    #[test]
    fn handle_store_evicts_oldest_records_past_its_byte_budget() {
        let mut store = HandleStore::default();
        // 96 x 1 MiB across distinct sessions — the RLM shape, which had no
        // eviction path at all because nothing ever calls `evict_session` for it.
        let payload = "z".repeat(1024 * 1024);
        for index in 0..96 {
            let _ = store.insert_text(format!("rlm-session-{index}"), "value", payload.clone());
        }
        assert!(
            store.retained_bytes() <= HANDLE_STORE_MAX_BYTES,
            "store held {} bytes, over the {HANDLE_STORE_MAX_BYTES} budget",
            store.retained_bytes()
        );
        let newest = VarHandle {
            kind: "var_handle".to_string(),
            session_id: "rlm-session-95".to_string(),
            name: "value".to_string(),
            type_name: "str".to_string(),
            length: payload.chars().count(),
            repr_preview: String::new(),
            sha256: String::new(),
        };
        assert!(
            store.get(&newest).is_some(),
            "the most recent handle must survive — it is the one still referenced"
        );
    }

    #[test]
    fn evicting_a_session_returns_its_bytes_to_the_budget() {
        let mut store = HandleStore::default();
        let _ = store.insert_text("session-a", "value", "a".repeat(4096));
        let _ = store.insert_text("session-b", "value", "b".repeat(4096));
        let before = store.retained_bytes();
        assert_eq!(before, 8192);
        store.evict_session("session-a");
        assert_eq!(
            store.retained_bytes(),
            4096,
            "per-session eviction must not leave phantom bytes on the budget"
        );
    }
}
