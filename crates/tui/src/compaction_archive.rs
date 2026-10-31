//! Model-facing view of the pre-compaction transcript.
//!
//! Compaction already persists the complete history before replacing it, in
//! `artifacts/context-transfer-<checkpoint>.json`. This module turns those
//! snapshots into addressable entries. An id hashes the message structure —
//! role plus block kinds, block ids and text — so the same message keeps one id
//! across cumulative snapshots, while two separate calls that happened to render
//! identically stay distinct.
//!
//! Known limits: thinking blocks are not archived and images render as their
//! url only; two messages whose blocks are byte-identical are one entry, so a
//! repeated `continue` shows up once; a tool result that a later pruning pass
//! rewrote to a stub is a second entry, because its text is no longer the same;
//! only the most recent [`MAX_LOAD_BYTES`] of snapshots are loaded; every tool
//! call reloads and re-parses the snapshots, so paging through one large entry
//! repeats that work; and snapshots are keyed by the engine's session namespace,
//! which a sub-agent inherits from its caller, so one archive can hold snapshots
//! written by several engines of the same session tree. A snippet marks its hit
//! with `<<` and `>>`; text that already contains those characters reads
//! ambiguously, and `match_offset` is the precise locator. Ids keep 48 bits, so
//! among a million messages a birthday collision is roughly 0.2% likely, and
//! `load` keeps the first entry for a repeated id; two executions of one tool
//! call that share provider id, name and input are one entry because
//! `execution_id` and `caller` stay out of the hash.

use std::io::{self, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

use codewhale_models::{ContentBlock, Message};
use sha2::{Digest, Sha256};

pub const TRANSFER_DIR_NAME: &str = "artifacts";
const TRANSFER_PREFIX: &str = "context-transfer-";
const TRANSFER_SUFFIX: &str = ".json";
const ID_HEX_LEN: usize = 12;
const MAX_LOAD_BYTES: u64 = 64 * 1024 * 1024;

/// One addressable message from a pre-compaction snapshot.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub id: String,
    pub seq: usize,
    pub role: String,
    pub at: u64,
    pub text: String,
}

/// What a load covered, so the tools can say what they could and could not see.
#[derive(Debug, Clone)]
pub struct ArchiveLoad {
    pub entries: Vec<ArchiveEntry>,
    pub snapshots: usize,
    pub skipped_snapshots: usize,
    pub truncation_note: Option<String>,
    pub directory_present: bool,
}

/// Render one message into the plain text an archive entry stores.
#[must_use]
pub fn render_message(message: &Message) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text, .. } => {
                if !text.trim().is_empty() {
                    parts.push(text.clone());
                }
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                parts.push(format!("[tool_use {name}#{id}] {input}"));
            }
            ContentBlock::ServerToolUse { id, name, input } => {
                parts.push(format!("[server_tool_use {name}#{id}] {input}"));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } => {
                let label = if is_error.unwrap_or(false) {
                    "[tool_result error]"
                } else {
                    "[tool_result]"
                };
                parts.push(format!("{label} {tool_use_id}\n{content}"));
            }
            ContentBlock::ToolSearchToolResult {
                tool_use_id,
                content,
            } => {
                parts.push(format!(
                    "[tool_search_tool_result] {tool_use_id}\n{content}"
                ));
            }
            ContentBlock::CodeExecutionToolResult {
                tool_use_id,
                content,
            } => {
                parts.push(format!(
                    "[code_execution_tool_result] {tool_use_id}\n{content}"
                ));
            }
            ContentBlock::ImageUrl { image_url } => {
                parts.push(render_image(&image_url.url));
            }
            _ => {}
        }
    }
    parts.join("\n\n")
}

fn render_image(url: &str) -> String {
    let mut head: String = url.chars().take(120).collect();
    if head.chars().count() < url.chars().count() {
        head.push('\u{2026}');
    }
    format!("[image] {head}")
}

/// The structural id a message is addressed by.
///
/// Hashing the rendered text would fold two separate tool calls that happened to
/// render the same, orphaning the second call's result. Hashing the blocks keeps
/// a call distinct by its provider id while still giving the same message the
/// same id in every cumulative snapshot.
#[must_use]
pub fn message_id(role: &str, message: &Message) -> String {
    let mut hasher = Sha256::new();
    hash_str(&mut hasher, role);
    for block in &message.content {
        hasher.update([0u8]);
        match block {
            ContentBlock::Text { text, .. } => {
                hasher.update(b"text");
                hash_str(&mut hasher, text);
            }
            ContentBlock::Thinking { .. } => hasher.update(b"thinking"),
            ContentBlock::ImageUrl { image_url } => {
                hasher.update(b"image");
                hash_str(&mut hasher, &image_url.url);
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                hasher.update(b"tool_use");
                hash_str(&mut hasher, id);
                hash_str(&mut hasher, name);
                hash_json(&mut hasher, input);
            }
            ContentBlock::ServerToolUse { id, name, input } => {
                hasher.update(b"server_tool_use");
                hash_str(&mut hasher, id);
                hash_str(&mut hasher, name);
                hash_json(&mut hasher, input);
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                content_blocks,
                ..
            } => {
                hasher.update(b"tool_result");
                hash_str(&mut hasher, tool_use_id);
                hash_str(&mut hasher, content);
                hasher.update([u8::from(is_error.unwrap_or(false))]);
                if let Some(blocks) = content_blocks {
                    for block in blocks {
                        hash_json(&mut hasher, block);
                    }
                }
            }
            ContentBlock::ToolSearchToolResult {
                tool_use_id,
                content,
            } => {
                hasher.update(b"tool_search_tool_result");
                hash_str(&mut hasher, tool_use_id);
                hash_json(&mut hasher, content);
            }
            ContentBlock::CodeExecutionToolResult {
                tool_use_id,
                content,
            } => {
                hasher.update(b"code_execution_tool_result");
                hash_str(&mut hasher, tool_use_id);
                hash_json(&mut hasher, content);
            }
        }
    }
    hex12(&hasher.finalize())
}

/// Feed one outer string with its length in front, so two different sequences of
/// fields cannot hash the same by bare concatenation (`id="ab" name="c"` versus
/// `id="a" name="bc"`).
fn hash_str(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

/// Feed JSON to the hasher as a prefix code: sorted keys, an item count for
/// arrays and objects, and a byte length for every string and number. Two equal
/// values hash equally and two different values cannot collide by concatenation.
/// Strings the caller feeds around a value must be length-prefixed too; see
/// [`hash_str`].
fn hash_json(hasher: &mut Sha256, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => hasher.update(b"null"),
        serde_json::Value::Bool(flag) => {
            hasher.update(if *flag { &b"true"[..] } else { &b"false"[..] })
        }
        serde_json::Value::Number(number) => {
            let text = number.to_string();
            hasher.update(b"num");
            hasher.update((text.len() as u64).to_le_bytes());
            hasher.update(text.as_bytes());
        }
        serde_json::Value::String(text) => {
            hasher.update(b"str");
            hasher.update((text.len() as u64).to_le_bytes());
            hasher.update(text.as_bytes());
        }
        serde_json::Value::Array(items) => {
            hasher.update(b"arr");
            hasher.update((items.len() as u64).to_le_bytes());
            for item in items {
                hash_json(hasher, item);
            }
        }
        serde_json::Value::Object(map) => {
            hasher.update(b"obj");
            hasher.update((map.len() as u64).to_le_bytes());
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (key, item) in entries {
                hasher.update((key.len() as u64).to_le_bytes());
                hasher.update(key.as_bytes());
                hash_json(hasher, item);
            }
        }
    }
}

fn hex12(digest: &[u8]) -> String {
    let mut id = String::with_capacity(ID_HEX_LEN);
    for byte in &digest[..ID_HEX_LEN / 2] {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

fn is_transfer_snapshot(name: &str) -> bool {
    name.starts_with(TRANSFER_PREFIX)
        && name.ends_with(TRANSFER_SUFFIX)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn snapshot_dir(session_id: &str) -> Option<std::path::PathBuf> {
    if !codewhale_runtime::artifacts::is_valid_session_id(session_id) {
        return None;
    }
    codewhale_runtime::artifacts::artifact_sessions_root()
        .map(|root| root.join(session_id).join(TRANSFER_DIR_NAME))
}

struct Snapshot {
    name: String,
    bytes: u64,
    modified: u64,
}

fn list_snapshots(session_id: &str) -> io::Result<(Vec<Snapshot>, bool)> {
    let Some(dir) = snapshot_dir(session_id) else {
        return Ok((Vec::new(), false));
    };
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(error) => return Err(error),
    };
    let mut snapshots: Vec<Snapshot> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_transfer_snapshot(&name) || !entry.file_type().ok()?.is_file() {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default();
            Some(Snapshot {
                name,
                bytes: metadata.len(),
                modified,
            })
        })
        .collect();
    snapshots.sort_by_key(|snapshot| (snapshot.modified, snapshot.name.clone()));
    Ok((snapshots, true))
}

fn read_snapshot(session_id: &str, name: &str) -> io::Result<Vec<Message>> {
    let relative = Path::new(TRANSFER_DIR_NAME).join(name);
    let file = codewhale_runtime::artifacts::open_session_relative(session_id, &relative, false)?;
    let mut body = Vec::new();
    file.open_file()?.read_to_end(&mut body)?;
    serde_json::from_slice::<Vec<Message>>(&body).map_err(io::Error::other)
}

/// Load every reachable snapshot for this session, oldest first.
pub fn load(session_id: &str) -> io::Result<ArchiveLoad> {
    let (snapshots, directory_present) = list_snapshots(session_id)?;
    let mut budget = MAX_LOAD_BYTES;
    let mut selected: Vec<&Snapshot> = Vec::new();
    let mut skipped = 0usize;
    let mut budget_skipped = 0usize;
    let mut unreadable = 0usize;
    for snapshot in snapshots.iter().rev() {
        if snapshot.bytes > budget {
            skipped += 1;
            budget_skipped += 1;
            continue;
        }
        budget -= snapshot.bytes;
        selected.push(snapshot);
    }
    selected.reverse();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<ArchiveEntry> = Vec::new();
    let mut first_error: Option<String> = None;
    for snapshot in &selected {
        let messages = match read_snapshot(session_id, &snapshot.name) {
            Ok(messages) => messages,
            Err(error) => {
                skipped += 1;
                unreadable += 1;
                if first_error.is_none() {
                    first_error = Some(format!("{}: {error}", snapshot.name));
                }
                continue;
            }
        };
        for (seq, message) in messages.iter().enumerate() {
            let role = message.role.as_str().to_ascii_lowercase();
            let text = render_message(message);
            if text.trim().is_empty() {
                continue;
            }
            let id = message_id(&role, message);
            if !seen.insert(id.clone()) {
                continue;
            }
            entries.push(ArchiveEntry {
                id,
                seq,
                role,
                at: snapshot.modified,
                text,
            });
        }
    }
    entries.sort_by_key(|entry| (entry.at, entry.seq));
    let mut notes: Vec<String> = Vec::new();
    if budget_skipped > 0 {
        notes.push(format!(
            "{budget_skipped} pre-compaction snapshots exceeded the {MAX_LOAD_BYTES}-byte load budget and were skipped"
        ));
    }
    if unreadable > 0 {
        notes.push(match &first_error {
            Some(error) => format!("{unreadable} snapshots could not be decoded (first: {error})"),
            None => format!("{unreadable} snapshots could not be decoded"),
        });
    }
    Ok(ArchiveLoad {
        entries,
        snapshots: selected.len(),
        skipped_snapshots: skipped,
        truncation_note: (!notes.is_empty()).then(|| notes.join("; ")),
        directory_present,
    })
}

/// Resolve an id, or a unique id prefix, to one archived message.
pub fn find_entry<'a>(
    entries: &'a [ArchiveEntry],
    reference: &str,
) -> Result<&'a ArchiveEntry, String> {
    let reference = reference
        .trim()
        .trim_start_matches('#')
        .to_ascii_lowercase();
    if reference.is_empty() {
        return Err("an archive id is required".to_string());
    }
    if let Some(entry) = entries.iter().find(|entry| entry.id == reference) {
        return Ok(entry);
    }
    let matches: Vec<&ArchiveEntry> = entries
        .iter()
        .filter(|entry| entry.id.starts_with(&reference))
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "no archived message matches `{reference}`; use context_archive_search to find one"
        )),
        [only] => Ok(*only),
        several => Err(format!(
            "`{reference}` is ambiguous: {} archived messages start with it; use more characters",
            several.len()
        )),
    }
}

/// One search hit: the message, where in it the query landed, and a snippet.
pub struct SearchHit<'a> {
    pub entry: &'a ArchiveEntry,
    pub char_offset: usize,
    pub snippet: String,
}

/// ASCII-case-insensitive substring search across archived messages.
///
/// `char_offset` is a character offset into `entry.text`, so a caller can hand
/// it straight to [`slice_chars`]. `skip` pages through further matches while
/// `total` keeps counting every hit.
#[must_use]
pub fn search_entries<'a>(
    entries: &'a [ArchiveEntry],
    query: &str,
    skip: usize,
    limit: usize,
) -> (Vec<SearchHit<'a>>, usize) {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return (Vec::new(), 0);
    }
    let mut hits = Vec::new();
    let mut total = 0usize;
    for entry in entries {
        let haystack = entry.text.to_ascii_lowercase();
        let Some(position) = haystack.find(&needle) else {
            continue;
        };
        total += 1;
        if total <= skip || hits.len() >= limit {
            continue;
        }
        hits.push(SearchHit {
            entry,
            char_offset: entry.text[..position].chars().count(),
            snippet: snippet(&entry.text, position, needle.len()),
        });
    }
    (hits, total)
}

const SNIPPET_RADIUS_CHARS: usize = 80;

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// A short window around one hit, marked so the model can see where it landed.
fn snippet(text: &str, byte_position: usize, needle_len: usize) -> String {
    let start = text[..byte_position]
        .char_indices()
        .rev()
        .nth(SNIPPET_RADIUS_CHARS)
        .map_or(0, |(index, _)| index);
    let hit_end = ceil_char_boundary(text, (byte_position + needle_len).min(text.len()));
    let end = text[hit_end..]
        .char_indices()
        .nth(SNIPPET_RADIUS_CHARS)
        .map_or(text.len(), |(index, _)| index + hit_end);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&text[start..byte_position]);
    out.push_str("<<");
    out.push_str(&text[byte_position..hit_end]);
    out.push_str(">>");
    out.push_str(&text[hit_end..end]);
    if end < text.len() {
        out.push('…');
    }
    out
}

/// Slice `text` by character offset, returning the slice and its total char count.
#[must_use]
pub fn slice_chars(text: &str, offset: usize, max_chars: usize) -> (String, usize) {
    let total = text.chars().count();
    let slice: String = text.chars().skip(offset).take(max_chars).collect();
    (slice, total)
}

#[cfg(test)]
mod tests {
    use super::{hash_json, hex12};
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};

    fn digest(value: &Value) -> String {
        let mut hasher = Sha256::new();
        hash_json(&mut hasher, value);
        hex12(&hasher.finalize())
    }

    fn little_endian_text(value: u64) -> String {
        value
            .to_le_bytes()
            .iter()
            .map(|byte| char::from(*byte))
            .collect()
    }

    #[test]
    fn a_number_cannot_swallow_the_field_after_it() {
        let long = "x".repeat(291);
        let tail = format!("str{}{long}", little_endian_text(295));
        let mut left = Map::new();
        left.insert(String::new(), json!(1));
        left.insert(format!("\u{0}a{tail}"), json!(0));
        let left = Value::Object(left);
        let right = json!({ "": 10, "a": format!("{long}num0") });
        assert_ne!(digest(&left), digest(&right));
    }
}
