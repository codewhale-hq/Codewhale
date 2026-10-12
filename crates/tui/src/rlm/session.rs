//! Persistent RLM session state for the v0.8.33 head/hands tool surface.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::repl::PythonRuntime;
use codewhale_models::{ContentBlock, Message, SystemPrompt};

pub type SharedRlmSessionStore = Arc<Mutex<HashMap<String, Arc<Mutex<RlmSession>>>>>;

#[must_use]
pub fn new_shared_rlm_session_store() -> SharedRlmSessionStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Debug)]
pub struct RlmSession {
    pub name: String,
    pub id: String,
    pub kernel: Option<PythonRuntime>,
    pub context_meta: ContextMeta,
    pub config: RlmSessionConfig,
    pub rpc_count: u32,
    pub total_duration: Duration,
    pub peak_var_count: usize,
    pub final_count: usize,
    pub created_at: Instant,
    pub last_used_at: Instant,
    pub context_path: PathBuf,
}

impl RlmSession {
    #[must_use]
    pub fn new(
        name: String,
        kernel: PythonRuntime,
        context_meta: ContextMeta,
        context_path: PathBuf,
    ) -> Self {
        let now = Instant::now();
        Self {
            name,
            id: format!("rlm:{}", Uuid::new_v4().simple()),
            kernel: Some(kernel),
            context_meta,
            config: RlmSessionConfig::default(),
            rpc_count: 0,
            total_duration: Duration::ZERO,
            peak_var_count: 0,
            final_count: 0,
            created_at: now,
            last_used_at: now,
            context_path,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMeta {
    pub length: usize,
    #[serde(rename = "type")]
    pub type_name: String,
    pub preview_500: String,
    pub sha256: String,
}

impl ContextMeta {
    #[must_use]
    pub fn from_body(body: &str, type_name: impl Into<String>) -> Self {
        Self {
            length: body.chars().count(),
            type_name: type_name.into(),
            preview_500: body.chars().take(500).collect(),
            sha256: sha256_hex(body.as_bytes()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputFeedback {
    Full,
    Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlmSessionConfig {
    pub output_feedback: OutputFeedback,
    pub sub_query_timeout_secs: u64,
    pub sub_rlm_max_depth: u32,
    pub share_session: bool,
}

impl Default for RlmSessionConfig {
    fn default() -> Self {
        Self {
            output_feedback: OutputFeedback::Full,
            sub_query_timeout_secs: 120,
            sub_rlm_max_depth: 1,
            share_session: false,
        }
    }
}

pub fn write_context_file(body: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("deepseek_rlm_ctx");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "session_{}_{}.txt",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    std::fs::write(&path, body)?;
    Ok(path)
}

#[derive(Debug, Clone)]
pub struct SessionObjectSnapshot {
    pub session_id: String,
    pub model: String,
    pub workspace: PathBuf,
    pub system_prompt: Option<SystemPrompt>,
    pub messages: Arc<Vec<Message>>,
}

impl SessionObjectSnapshot {
    #[must_use]
    pub fn new(
        session_id: String,
        model: String,
        workspace: PathBuf,
        system_prompt: Option<SystemPrompt>,
        messages: impl Into<Arc<Vec<Message>>>,
    ) -> Self {
        Self {
            session_id,
            model,
            workspace,
            system_prompt,
            messages: messages.into(),
        }
    }

    #[must_use]
    pub fn object_cards(&self) -> Vec<SessionObjectCard> {
        let mut cards = Vec::new();
        for object in self.base_objects() {
            cards.push(SessionObjectCard::from_resolved(&object));
        }
        for index in 0..self.messages.len() {
            if let Some(object) = self.resolve(&format!("session://active/messages/{index}")) {
                cards.push(SessionObjectCard::from_resolved(&object));
            }
        }
        cards
    }

    #[must_use]
    pub fn resolve(&self, object_ref: &str) -> Option<ResolvedSessionObject> {
        let normalized = normalize_session_object_ref(object_ref);
        match normalized.as_str() {
            "session://active/session" => Some(self.session_metadata_object()),
            "session://active/system_prompt" => self.system_prompt_object(),
            "session://active/transcript" => Some(self.transcript_object()),
            "session://active/latest_user" => self.latest_user_object(),
            _ => self.message_object(&normalized),
        }
    }

    fn base_objects(&self) -> Vec<ResolvedSessionObject> {
        let mut objects = vec![self.session_metadata_object()];
        if let Some(object) = self.system_prompt_object() {
            objects.push(object);
        }
        objects.push(self.transcript_object());
        if let Some(object) = self.latest_user_object() {
            objects.push(object);
        }
        objects
    }

    fn session_metadata_object(&self) -> ResolvedSessionObject {
        let body = json!({
            "session_id": self.session_id,
            "model": self.model,
            "workspace": self.workspace.display().to_string(),
            "message_count": self.messages.len(),
            "object_refs": {
                "system_prompt": "session://active/system_prompt",
                "transcript": "session://active/transcript",
                "latest_user": "session://active/latest_user",
                "message_prefix": "session://active/messages/"
            }
        })
        .to_string();
        ResolvedSessionObject::new(
            "session://active/session",
            "session_metadata",
            "Active session metadata",
            body,
        )
    }

    fn system_prompt_object(&self) -> Option<ResolvedSessionObject> {
        let prompt = self.system_prompt.as_ref()?;
        Some(ResolvedSessionObject::new(
            "session://active/system_prompt",
            "system_prompt",
            "Active system prompt",
            render_system_prompt(prompt),
        ))
    }

    fn transcript_object(&self) -> ResolvedSessionObject {
        let body = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| compact_message_json(index, message).to_string())
            .collect::<Vec<_>>()
            .join("\n");
        ResolvedSessionObject::new(
            "session://active/transcript",
            "transcript",
            "Active transcript as JSONL",
            body,
        )
    }

    fn latest_user_object(&self) -> Option<ResolvedSessionObject> {
        self.messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| message.role == "user")
            .map(|(index, message)| message_resolved_object(index, message, "Latest user message"))
    }

    fn message_object(&self, normalized: &str) -> Option<ResolvedSessionObject> {
        let index = normalized
            .strip_prefix("session://active/messages/")?
            .parse::<usize>()
            .ok()?;
        self.messages
            .get(index)
            .map(|message| message_resolved_object(index, message, "Transcript message"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionObjectCard {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub length: usize,
    pub preview_500: String,
    pub sha256: String,
}

impl SessionObjectCard {
    #[must_use]
    pub fn from_resolved(object: &ResolvedSessionObject) -> Self {
        Self {
            id: object.id.clone(),
            kind: object.kind.clone(),
            title: object.title.clone(),
            length: object.body.chars().count(),
            preview_500: object.body.chars().take(500).collect(),
            sha256: sha256_hex(object.body.as_bytes()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSessionObject {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
}

impl ResolvedSessionObject {
    fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
        }
    }
}

fn normalize_session_object_ref(object_ref: &str) -> String {
    let trimmed = object_ref.trim();
    if trimmed.starts_with("session://") {
        trimmed.to_string()
    } else {
        format!("session://active/{}", trimmed.trim_start_matches('/'))
    }
}

fn render_system_prompt(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn message_resolved_object(index: usize, message: &Message, title: &str) -> ResolvedSessionObject {
    ResolvedSessionObject::new(
        format!("session://active/messages/{index}"),
        "message",
        format!("{title} {index} ({})", message.role),
        compact_message_json(index, message).to_string(),
    )
}

fn compact_message_json(index: usize, message: &Message) -> Value {
    json!({
        "index": index,
        "role": message.role,
        "content": message.content.iter().map(compact_content_block).collect::<Vec<_>>(),
    })
}

fn compact_content_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text, .. } => json!({
            "type": "text",
            "text": text,
        }),
        ContentBlock::Thinking { thinking, .. } => json!({
            "type": "thinking",
            "redacted": true,
            "chars": thinking.chars().count(),
            "sha256": sha256_hex(thinking.as_bytes()),
            "preview_240": truncate_chars(thinking, 240),
        }),
        ContentBlock::ToolUse {
            id,
            name,
            input,
            caller,
            ..
        } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
            "caller": caller,
        }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => {
            let chars = content.chars().count();
            let large = chars > 2_000;
            json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "is_error": is_error,
                "content": if large { Value::Null } else { Value::String(content.clone()) },
                "content_preview": truncate_chars(content, 500),
                "content_chars": chars,
                "content_sha256": sha256_hex(content.as_bytes()),
                "content_redacted": large,
                "content_blocks": crate::image_attach::safe_tool_result_content_blocks(
                    content_blocks.as_deref(),
                ),
            })
        }
        ContentBlock::ServerToolUse { id, name, input } => json!({
            "type": "server_tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        ContentBlock::ToolSearchToolResult {
            tool_use_id,
            content,
        } => json!({
            "type": "tool_search_tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        }),
        ContentBlock::CodeExecutionToolResult {
            tool_use_id,
            content,
        } => json!({
            "type": "code_execution_tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        }),
        ContentBlock::ImageUrl { .. } => serde_json::Value::Null,
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let take = max_chars.saturating_sub(3);
    let mut out: String = text.chars().take(take).collect();
    out.push_str("...");
    out
}

#[must_use]
pub fn derive_session_name(source_hint: Option<&str>) -> String {
    let hint = source_hint
        .and_then(|raw| {
            Path::new(raw)
                .file_name()
                .and_then(|name| name.to_str())
                .or(Some(raw))
        })
        .unwrap_or("context");
    let mut out = String::new();
    for ch in hint.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "context".to_string()
    } else {
        out.to_string()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::hashing::sha256_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;

    #[test]
    fn derive_session_name_slugifies_path() {
        assert_eq!(
            derive_session_name(Some("src/Big File.rs")),
            "big_file_rs".to_string()
        );
    }

    #[test]
    fn context_meta_hashes_and_previews_body() {
        let meta = ContextMeta::from_body("abcdef", "text");
        assert_eq!(meta.length, 6);
        assert_eq!(meta.preview_500, "abcdef");
        assert_eq!(
            meta.sha256,
            "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721"
        );
    }

    #[test]
    fn session_objects_expose_prompt_and_transcript_cards() {
        let snapshot = SessionObjectSnapshot::new(
            "session-1".to_string(),
            "deepseek-v4-pro".to_string(),
            PathBuf::from("/tmp/work"),
            Some(SystemPrompt::Text("system body".to_string())),
            vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello RLM".to_string(),
                    cache_control: None,
                }],
            }],
        );

        let cards = snapshot.object_cards();
        assert!(
            cards
                .iter()
                .any(|card| card.id == "session://active/system_prompt")
        );
        assert!(
            cards
                .iter()
                .any(|card| card.id == "session://active/messages/0")
        );

        let transcript = snapshot
            .resolve("session://active/transcript")
            .expect("transcript object");
        assert!(transcript.body.contains("hello RLM"));
    }

    #[test]
    fn session_object_transcript_keeps_large_tool_results_compact() {
        let large = "tool output\n".repeat(400);
        let snapshot = SessionObjectSnapshot::new(
            "session-1".to_string(),
            "deepseek-v4-pro".to_string(),
            PathBuf::from("/tmp/work"),
            None,
            vec![Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_1".to_string(),
                    content: large.clone(),
                    is_error: None,
                    content_blocks: None,
                }],
            }],
        );

        let object = snapshot
            .resolve("session://active/messages/0")
            .expect("message object");
        assert!(object.body.contains("\"content_redacted\":true"));
        assert!(object.body.len() < large.len());
    }
}
