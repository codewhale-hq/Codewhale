use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use codewhale_config::private_directory::PrivateDirectory;
use serde::{Deserialize, Deserializer, Serialize};

pub const DEFAULT_ID: &str = "default";
const SOUL_FILE: &str = "SOUL.md";
const MEMORY_FILE: &str = "MEMORY.md";
const MANIFEST_FILE: &str = "whale.toml";
const LOCK_FILE: &str = "soul.lock";
pub const MAX_BYTES: usize = 16 * 1024;
const SOUL_PREVIEW_CHARS: usize = 280;

#[derive(Debug)]
pub enum WhaleError {
    InvalidId(String),
    BadRequest(String),
    NotFound(String),
    DefaultProtected,
    PreconditionFailed,
    ReadOnly(String),
    Conflict(String),
    Other(anyhow::Error),
}

impl fmt::Display for WhaleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(msg)
            | Self::BadRequest(msg)
            | Self::NotFound(msg)
            | Self::ReadOnly(msg)
            | Self::Conflict(msg) => f.write_str(msg),
            Self::DefaultProtected => f.write_str("The default whale cannot be deleted"),
            Self::PreconditionFailed => f.write_str("Whale changed since it was opened"),
            Self::Other(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for WhaleError {}

impl From<anyhow::Error> for WhaleError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<std::io::Error> for WhaleError {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.into())
    }
}

#[derive(Serialize)]
pub struct Soul {
    pub path: PathBuf,
    pub text: String,
    pub revision: String,
    pub writable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WhaleManifest {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_posture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WhaleRecord {
    pub id: String,
    pub name: String,
    pub purpose: String,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_posture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    pub folder_path: PathBuf,
    pub soul_path: PathBuf,
    pub memory_path: PathBuf,
    pub revision: String,
    pub soul_revision: String,
    pub writable: bool,
    pub soul_preview: String,
    pub soul: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateWhaleInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default, alias = "instructions")]
    pub soul: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, alias = "modelProviderId")]
    pub model_provider_id: Option<String>,
    #[serde(default, alias = "externalAgentId")]
    pub external_agent_id: Option<String>,
    #[serde(default, alias = "permissionPosture")]
    pub permission_posture: Option<String>,
    #[serde(default, alias = "childPosture")]
    pub child_posture: Option<String>,
    #[serde(default, alias = "homeThreadId")]
    pub home_thread_id: Option<String>,
}

fn deserialize_patch_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateWhaleInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default, alias = "instructions")]
    pub soul: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default, deserialize_with = "deserialize_patch_string")]
    pub model: Option<Option<String>>,
    #[serde(
        default,
        alias = "modelProviderId",
        deserialize_with = "deserialize_patch_string"
    )]
    pub model_provider_id: Option<Option<String>>,
    #[serde(
        default,
        alias = "externalAgentId",
        deserialize_with = "deserialize_patch_string"
    )]
    pub external_agent_id: Option<Option<String>>,
    #[serde(
        default,
        alias = "permissionPosture",
        deserialize_with = "deserialize_patch_string"
    )]
    pub permission_posture: Option<Option<String>>,
    #[serde(
        default,
        alias = "childPosture",
        deserialize_with = "deserialize_patch_string"
    )]
    pub child_posture: Option<Option<String>>,
    #[serde(
        default,
        alias = "homeThreadId",
        deserialize_with = "deserialize_patch_string"
    )]
    pub home_thread_id: Option<Option<String>>,
}

fn is_windows_reserved_name(id: &str) -> bool {
    matches!(
        id,
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    )
}

pub fn validate_id(id: &str) -> std::result::Result<&str, WhaleError> {
    if id.is_empty()
        || id.len() > 64
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || is_windows_reserved_name(id)
    {
        return Err(WhaleError::InvalidId(
            "Whale id must match ^[a-z0-9][a-z0-9_-]{0,63}$".into(),
        ));
    }
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return Err(WhaleError::InvalidId(
            "Whale id must match ^[a-z0-9][a-z0-9_-]{0,63}$".into(),
        ));
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(WhaleError::InvalidId(
            "Whale id must match ^[a-z0-9][a-z0-9_-]{0,63}$".into(),
        ));
    }
    if !bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
    }) {
        return Err(WhaleError::InvalidId(
            "Whale id must match ^[a-z0-9][a-z0-9_-]{0,63}$".into(),
        ));
    }
    Ok(id)
}

fn slugify_name(name: &str) -> String {
    let mut out = String::new();
    let mut prev_sep = false;
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            out.push(lower);
            prev_sep = false;
        } else if !out.is_empty() && !prev_sep {
            out.push('-');
            prev_sep = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    while out.ends_with('-') || out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() || out == DEFAULT_ID || is_windows_reserved_name(&out) {
        "whale".to_owned()
    } else {
        out
    }
}

fn normalize_opt(value: Option<String>, max_chars: usize) -> Option<String> {
    let trimmed = value?.trim().to_owned();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .chars()
            .filter(|ch| !ch.is_control())
            .take(max_chars)
            .collect(),
    )
}

fn normalize_color(color: Option<&str>) -> String {
    match color.map(str::trim) {
        Some("scout") | None | Some("") => "scout".to_owned(),
        Some("patch") => "patch".to_owned(),
        Some("violet") => "violet".to_owned(),
        Some("amber") => "amber".to_owned(),
        Some(other) => {
            let cleaned: String = other
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
                .take(24)
                .collect();
            if cleaned.is_empty() {
                "scout".to_owned()
            } else {
                cleaned
            }
        }
    }
}

fn preview_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SOUL_PREVIEW_CHARS {
        collapsed
    } else {
        let mut out: String = collapsed
            .chars()
            .take(SOUL_PREVIEW_CHARS.saturating_sub(1))
            .collect();
        out.push('…');
        out
    }
}

fn whales_root() -> Result<PathBuf> {
    Ok(codewhale_config::codewhale_home()?.join("whales"))
}

fn whale_directory(id: &str) -> std::result::Result<PathBuf, WhaleError> {
    let id = validate_id(id)?;
    Ok(whales_root()?.join(id))
}

fn read_file(dir: &PrivateDirectory, name: &str) -> Result<Option<String>> {
    let bytes = dir.read_private_receipt(name, MAX_BYTES)?;
    bytes
        .map(|(bytes, _)| String::from_utf8(bytes).context("Whale documents must be UTF-8"))
        .transpose()
}

fn compute_whale_revision(
    manifest_raw: Option<&str>,
    soul: Option<&str>,
    memory: Option<&str>,
) -> String {
    if manifest_raw.is_none() && soul.is_none() && memory.is_none() {
        return "new".to_owned();
    }
    let mut material = Vec::new();
    if let Some(manifest) = manifest_raw {
        material.extend_from_slice(b"manifest:");
        material.extend_from_slice(manifest.as_bytes());
    }
    material.push(0);
    if let Some(soul) = soul {
        material.extend_from_slice(b"soul:");
        material.extend_from_slice(soul.as_bytes());
    }
    material.push(0);
    if let Some(memory) = memory {
        material.extend_from_slice(b"memory:");
        material.extend_from_slice(memory.as_bytes());
    }
    codewhale_runtime::hashing::sha256_hex(&material)
}

fn snapshot_whale(
    expected_id: &str,
    path: &Path,
    dir: Option<&PrivateDirectory>,
) -> Result<(Option<WhaleManifest>, WhaleRecord)> {
    let manifest_raw = dir
        .map(|dir| read_file(dir, MANIFEST_FILE))
        .transpose()?
        .flatten();
    let manifest = manifest_raw
        .as_deref()
        .map(toml::from_str::<WhaleManifest>)
        .transpose()
        .context("Invalid whale identity file")?;
    ensure!(
        manifest
            .as_ref()
            .is_none_or(|whale| whale.id == expected_id),
        "Whale identity changed"
    );
    let writable = manifest
        .as_ref()
        .is_none_or(|whale| whale.account_agent_id.is_none());
    let soul_opt = dir
        .map(|dir| read_file(dir, SOUL_FILE))
        .transpose()?
        .flatten();
    let memory_opt = dir
        .map(|dir| read_file(dir, MEMORY_FILE))
        .transpose()?
        .flatten();
    ensure!(
        (soul_opt.is_none() && memory_opt.is_none()) || manifest.is_some(),
        "An existing SOUL.md requires whale.toml with its local whale identity"
    );
    let soul_revision = soul_opt.as_ref().map_or_else(
        || "new".to_owned(),
        |text| codewhale_runtime::hashing::sha256_hex(text.as_bytes()),
    );
    let revision = compute_whale_revision(
        manifest_raw.as_deref(),
        soul_opt.as_deref(),
        memory_opt.as_deref(),
    );
    let soul = soul_opt.unwrap_or_default();
    let soul_preview = preview_text(&soul);
    let default_name = if expected_id == DEFAULT_ID {
        "Whale"
    } else {
        expected_id
    };
    let name = manifest
        .as_ref()
        .and_then(|m| m.name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_name)
        .to_owned();
    let purpose = manifest
        .as_ref()
        .and_then(|m| m.purpose.clone())
        .unwrap_or_default();
    let color = normalize_color(manifest.as_ref().and_then(|m| m.color.as_deref()));
    let record = WhaleRecord {
        id: expected_id.to_owned(),
        name,
        purpose,
        color,
        model: manifest.as_ref().and_then(|m| m.model.clone()),
        model_provider_id: manifest.as_ref().and_then(|m| m.model_provider_id.clone()),
        external_agent_id: manifest.as_ref().and_then(|m| m.external_agent_id.clone()),
        permission_posture: manifest.as_ref().and_then(|m| m.permission_posture.clone()),
        child_posture: manifest.as_ref().and_then(|m| m.child_posture.clone()),
        home_thread_id: manifest.as_ref().and_then(|m| m.home_thread_id.clone()),
        account_agent_id: manifest.as_ref().and_then(|m| m.account_agent_id.clone()),
        created_at: manifest.as_ref().and_then(|m| m.created_at),
        updated_at: manifest.as_ref().and_then(|m| m.updated_at),
        folder_path: path.to_path_buf(),
        soul_path: path.join(SOUL_FILE),
        memory_path: path.join(MEMORY_FILE),
        revision,
        soul_revision,
        writable,
        soul_preview,
        soul,
        memory: memory_opt,
    };
    Ok((manifest, record))
}

fn record_to_soul(record: WhaleRecord) -> Soul {
    Soul {
        path: record.soul_path,
        text: record.soul,
        revision: record.soul_revision,
        writable: record.writable,
    }
}

pub fn load() -> Result<Soul> {
    let path = whale_directory(DEFAULT_ID).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let (_, record) = snapshot_whale(DEFAULT_ID, &path, None)?;
            return Ok(record_to_soul(record));
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let dir = PrivateDirectory::inspect(&path)?;
    let (_, record) = snapshot_whale(DEFAULT_ID, &path, Some(&dir))?;
    Ok(record_to_soul(record))
}

pub fn save(expected: &str, text: &str) -> Result<Option<Soul>> {
    ensure!(
        text.len() <= MAX_BYTES,
        "SOUL.md exceeds the local whale file limit"
    );
    let path = whale_directory(DEFAULT_ID).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let dir = PrivateDirectory::admit(&path)?;
    let lock = dir.open_internal_file(LOCK_FILE)?;
    lock.try_lock()
        .context("The local whale voice is being edited or cannot be locked; retry the edit")?;
    ensure!(dir.is_at_selected_path()?, "Whale directory changed");
    let (_, current) = snapshot_whale(DEFAULT_ID, &path, Some(&dir))?;
    if current.soul_revision != expected {
        return Ok(None);
    }
    ensure!(current.writable, "The account owns this imported whale");
    if read_file(&dir, MANIFEST_FILE)?.is_none() {
        dir.write_owned_file(MANIFEST_FILE, b"id = \"default\"\n", false)?;
    }
    dir.write_owned_file(SOUL_FILE, text.as_bytes(), true)?;
    let (_, saved) = snapshot_whale(DEFAULT_ID, &path, Some(&dir))?;
    Ok(Some(record_to_soul(saved)))
}

pub fn get_whale(id: &str) -> std::result::Result<WhaleRecord, WhaleError> {
    let id = validate_id(id)?;
    let path = whale_directory(id)?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if id == DEFAULT_ID {
                let (_, record) = snapshot_whale(DEFAULT_ID, &path, None)?;
                return Ok(record);
            }
            return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let dir = PrivateDirectory::inspect(&path).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let (manifest, record) =
        snapshot_whale(id, &path, Some(&dir)).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    if id != DEFAULT_ID && manifest.is_none() {
        return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
    }
    Ok(record)
}

pub fn list_whales() -> std::result::Result<Vec<WhaleRecord>, WhaleError> {
    let root = whales_root()?;
    let default_record = get_whale(DEFAULT_ID)?;
    let mut records = vec![default_record];
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let _root_dir = PrivateDirectory::inspect(&root)
        .or_else(|_| PrivateDirectory::open(&root))
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name == DEFAULT_ID || name.starts_with('.') {
            continue;
        }
        if validate_id(&name).is_ok() {
            ids.push(name);
        }
    }
    ids.sort();
    for id in ids {
        let path = root.join(&id);
        let dir =
            PrivateDirectory::inspect(&path).map_err(|e| WhaleError::Conflict(e.to_string()))?;
        let (manifest, record) = snapshot_whale(&id, &path, Some(&dir))
            .map_err(|e| WhaleError::Conflict(e.to_string()))?;
        if manifest.is_some() {
            records.push(record);
        }
    }
    Ok(records)
}

pub fn create_whale(input: CreateWhaleInput) -> std::result::Result<WhaleRecord, WhaleError> {
    let name = normalize_opt(Some(input.name), 80)
        .ok_or_else(|| WhaleError::BadRequest("Whale name is required".into()))?;
    if let Some(soul) = input.soul.as_ref()
        && soul.len() > MAX_BYTES
    {
        return Err(WhaleError::BadRequest(
            "SOUL.md exceeds the local whale file limit".into(),
        ));
    }
    if let Some(memory) = input.memory.as_ref()
        && memory.len() > MAX_BYTES
    {
        return Err(WhaleError::BadRequest(
            "MEMORY.md exceeds the local whale file limit".into(),
        ));
    }
    let requested_id = match input.id {
        Some(raw) => {
            let validated = validate_id(raw.trim())?.to_owned();
            if validated == DEFAULT_ID {
                return Err(WhaleError::Conflict(
                    "The default whale already exists".into(),
                ));
            }
            Some(validated)
        }
        None => None,
    };
    let root = whales_root()?;
    let _root_dir =
        PrivateDirectory::open(&root).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let id = match requested_id {
        Some(validated) => {
            if std::fs::symlink_metadata(root.join(&validated)).is_ok() {
                return Err(WhaleError::Conflict(format!(
                    "Whale '{validated}' already exists"
                )));
            }
            validated
        }
        None => {
            let base = slugify_name(&name);
            let mut candidate = base.clone();
            let mut suffix = 2u32;
            while candidate == DEFAULT_ID
                || is_windows_reserved_name(&candidate)
                || std::fs::symlink_metadata(root.join(&candidate)).is_ok()
            {
                let tail = format!("-{suffix}");
                let max_prefix = 64usize.saturating_sub(tail.len());
                let prefix: String = base.chars().take(max_prefix).collect();
                candidate = format!("{prefix}{tail}");
                suffix = suffix.saturating_add(1);
            }
            validate_id(&candidate)?.to_owned()
        }
    };
    let path = root.join(&id);
    let dir = PrivateDirectory::admit(&path).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let lock = dir
        .open_internal_file(LOCK_FILE)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    lock.try_lock().map_err(|_| {
        WhaleError::Conflict(
            "The local whale is being edited or cannot be locked; retry the edit".into(),
        )
    })?;
    if !dir
        .is_at_selected_path()
        .map_err(|e| WhaleError::Conflict(e.to_string()))?
    {
        return Err(WhaleError::Conflict("Whale directory changed".into()));
    }
    if read_file(&dir, MANIFEST_FILE)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?
        .is_some()
    {
        return Err(WhaleError::Conflict(format!("Whale '{id}' already exists")));
    }
    let now = Utc::now();
    let manifest = WhaleManifest {
        id: id.clone(),
        name: Some(name),
        purpose: normalize_opt(input.purpose, 240),
        color: Some(normalize_color(input.color.as_deref())),
        model: normalize_opt(input.model, 160),
        model_provider_id: normalize_opt(input.model_provider_id, 160),
        external_agent_id: normalize_opt(input.external_agent_id, 160),
        permission_posture: normalize_opt(input.permission_posture, 64),
        child_posture: normalize_opt(input.child_posture, 64),
        home_thread_id: normalize_opt(input.home_thread_id, 128),
        account_agent_id: None,
        created_at: Some(now),
        updated_at: Some(now),
    };
    let serialized = toml::to_string(&manifest).context("Failed to serialize whale manifest")?;
    dir.write_owned_file(MANIFEST_FILE, serialized.as_bytes(), false)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let soul_text = input.soul.unwrap_or_default();
    dir.write_owned_file(SOUL_FILE, soul_text.as_bytes(), true)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    if let Some(memory) = input.memory {
        dir.write_owned_file(MEMORY_FILE, memory.as_bytes(), true)
            .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    }
    let (_, saved) =
        snapshot_whale(&id, &path, Some(&dir)).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    Ok(saved)
}

pub fn update_whale(
    id: &str,
    expected_revision: &str,
    input: UpdateWhaleInput,
) -> std::result::Result<WhaleRecord, WhaleError> {
    let id = validate_id(id)?;
    if let Some(soul) = input.soul.as_ref()
        && soul.len() > MAX_BYTES
    {
        return Err(WhaleError::BadRequest(
            "SOUL.md exceeds the local whale file limit".into(),
        ));
    }
    if let Some(memory) = input.memory.as_ref()
        && memory.len() > MAX_BYTES
    {
        return Err(WhaleError::BadRequest(
            "MEMORY.md exceeds the local whale file limit".into(),
        ));
    }
    let path = whale_directory(id)?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if id != DEFAULT_ID {
                return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
            }
            if expected_revision != "new" {
                return Err(WhaleError::PreconditionFailed);
            }
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let dir = PrivateDirectory::admit(&path).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let lock = dir
        .open_internal_file(LOCK_FILE)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    lock.try_lock().map_err(|_| {
        WhaleError::Conflict(
            "The local whale is being edited or cannot be locked; retry the edit".into(),
        )
    })?;
    if !dir
        .is_at_selected_path()
        .map_err(|e| WhaleError::Conflict(e.to_string()))?
    {
        return Err(WhaleError::Conflict("Whale directory changed".into()));
    }
    let (existing_manifest, current) =
        snapshot_whale(id, &path, Some(&dir)).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    if id != DEFAULT_ID && existing_manifest.is_none() {
        return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
    }
    if current.revision != expected_revision {
        return Err(WhaleError::PreconditionFailed);
    }
    if !current.writable {
        return Err(WhaleError::ReadOnly(
            "The account owns this imported whale".into(),
        ));
    }
    let now = Utc::now();
    let mut manifest = existing_manifest.unwrap_or_else(|| WhaleManifest {
        id: id.to_owned(),
        created_at: Some(now),
        ..Default::default()
    });
    manifest.id = id.to_owned();
    if let Some(name) = input.name {
        let cleaned = normalize_opt(Some(name), 80)
            .ok_or_else(|| WhaleError::BadRequest("Whale name cannot be empty".into()))?;
        manifest.name = Some(cleaned);
    }
    if let Some(purpose) = input.purpose {
        manifest.purpose = normalize_opt(Some(purpose), 240);
    }
    if let Some(color) = input.color {
        manifest.color = Some(normalize_color(Some(&color)));
    }
    if let Some(model) = input.model {
        manifest.model = normalize_opt(model, 160);
    }
    if let Some(provider_id) = input.model_provider_id {
        manifest.model_provider_id = normalize_opt(provider_id, 160);
    }
    if let Some(agent_id) = input.external_agent_id {
        manifest.external_agent_id = normalize_opt(agent_id, 160);
    }
    if let Some(posture) = input.permission_posture {
        manifest.permission_posture = normalize_opt(posture, 64);
    }
    if let Some(child_posture) = input.child_posture {
        manifest.child_posture = normalize_opt(child_posture, 64);
    }
    if let Some(home_thread_id) = input.home_thread_id {
        manifest.home_thread_id = normalize_opt(home_thread_id, 128);
    }
    if manifest.created_at.is_none() {
        manifest.created_at = Some(now);
    }
    manifest.updated_at = Some(now);
    let serialized = toml::to_string(&manifest).context("Failed to serialize whale manifest")?;
    dir.write_owned_file(MANIFEST_FILE, serialized.as_bytes(), true)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    if let Some(soul) = input.soul {
        dir.write_owned_file(SOUL_FILE, soul.as_bytes(), true)
            .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    }
    if let Some(memory) = input.memory {
        dir.write_owned_file(MEMORY_FILE, memory.as_bytes(), true)
            .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    }
    let (_, saved) =
        snapshot_whale(id, &path, Some(&dir)).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    Ok(saved)
}

pub fn set_home_thread_id(
    id: &str,
    expected_revision: Option<&str>,
    home_thread_id: &str,
) -> std::result::Result<(Option<String>, WhaleRecord), WhaleError> {
    let id = validate_id(id)?;
    let path = whale_directory(id)?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if id != DEFAULT_ID {
                return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
            }
            if let Some(expected) = expected_revision
                && expected != "new"
            {
                return Err(WhaleError::PreconditionFailed);
            }
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let dir = PrivateDirectory::admit(&path).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let lock = dir
        .open_internal_file(LOCK_FILE)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    lock.try_lock().map_err(|_| {
        WhaleError::Conflict(
            "The local whale is being edited or cannot be locked; retry the edit".into(),
        )
    })?;
    if !dir
        .is_at_selected_path()
        .map_err(|e| WhaleError::Conflict(e.to_string()))?
    {
        return Err(WhaleError::Conflict("Whale directory changed".into()));
    }
    let (existing_manifest, current) =
        snapshot_whale(id, &path, Some(&dir)).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    if id != DEFAULT_ID && existing_manifest.is_none() {
        return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
    }
    if let Some(expected) = expected_revision
        && current.revision != expected
    {
        return Err(WhaleError::PreconditionFailed);
    }
    if !current.writable {
        return Err(WhaleError::ReadOnly(
            "The account owns this imported whale".into(),
        ));
    }
    let previous = current.home_thread_id.clone();
    let now = Utc::now();
    let mut manifest = existing_manifest.unwrap_or_else(|| WhaleManifest {
        id: id.to_owned(),
        created_at: Some(now),
        ..Default::default()
    });
    manifest.id = id.to_owned();
    manifest.home_thread_id = normalize_opt(Some(home_thread_id.to_owned()), 128);
    if manifest.created_at.is_none() {
        manifest.created_at = Some(now);
    }
    manifest.updated_at = Some(now);
    let serialized = toml::to_string(&manifest).context("Failed to serialize whale manifest")?;
    dir.write_owned_file(MANIFEST_FILE, serialized.as_bytes(), true)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let (_, saved) =
        snapshot_whale(id, &path, Some(&dir)).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    Ok((previous, saved))
}

pub fn delete_whale(id: &str) -> std::result::Result<(), WhaleError> {
    let id = validate_id(id)?;
    if id == DEFAULT_ID {
        return Err(WhaleError::DefaultProtected);
    }
    let path = whale_directory(id)?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let dir = PrivateDirectory::inspect(&path).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    let lock = dir
        .open_internal_file(LOCK_FILE)
        .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    lock.try_lock().map_err(|_| {
        WhaleError::Conflict(
            "The local whale is being edited or cannot be locked; retry the edit".into(),
        )
    })?;
    if !dir
        .is_at_selected_path()
        .map_err(|e| WhaleError::Conflict(e.to_string()))?
    {
        return Err(WhaleError::Conflict("Whale directory changed".into()));
    }
    let (manifest, current) =
        snapshot_whale(id, &path, Some(&dir)).map_err(|e| WhaleError::Conflict(e.to_string()))?;
    if manifest.is_none() {
        return Err(WhaleError::NotFound(format!("Whale '{id}' does not exist")));
    }
    if !current.writable {
        return Err(WhaleError::ReadOnly(
            "The account owns this imported whale".into(),
        ));
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&path)? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    drop(lock);
    for name in names {
        dir.remove_raw(&name)
            .map_err(|e| WhaleError::Conflict(e.to_string()))?;
    }
    drop(dir);
    std::fs::remove_dir(&path)?;
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Voice {
    path: PathBuf,
    revision: String,
    text: String,
    memory_path: Option<PathBuf>,
    memory_text: Option<String>,
}

impl std::fmt::Debug for Voice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhaleVoice")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl Voice {
    pub(crate) fn segments(&self) -> Vec<crate::project_context::InstructionSegment> {
        use crate::project_context::{InstructionOrigin, InstructionScope, InstructionSegment};
        let mut out = Vec::new();
        if !self.text.trim().is_empty() {
            let mut segment = InstructionSegment::new(
                InstructionScope::Global,
                InstructionOrigin::WhaleSoul,
                Some(self.path.clone()),
                self.text.clone(),
            );
            segment.label = Some(SOUL_FILE.into());
            segment.canonical_path = Some(self.path.clone());
            out.push(segment);
        }
        if let Some(memory) = self.memory_text.as_ref().filter(|m| !m.trim().is_empty()) {
            let mut segment = InstructionSegment::new(
                InstructionScope::Global,
                InstructionOrigin::WhaleSoul,
                self.memory_path.clone(),
                memory.clone(),
            );
            segment.label = Some(MEMORY_FILE.into());
            segment.canonical_path = self.memory_path.clone();
            out.push(segment);
        }
        out
    }
}

pub(crate) fn capture_sync_for(whale_id: Option<&str>) -> Result<Option<Arc<Voice>>> {
    let target = whale_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(DEFAULT_ID);
    let record = match get_whale(target) {
        Ok(record) => record,
        Err(WhaleError::NotFound(_)) => return Ok(None),
        Err(err) => {
            return Err(anyhow::anyhow!(err.to_string()))
                .context("Could not read the local whale identity and SOUL.md safely");
        }
    };
    let has_soul = !record.soul.trim().is_empty();
    let has_memory = record
        .memory
        .as_deref()
        .is_some_and(|memory| !memory.trim().is_empty());
    Ok((record.writable && (has_soul || has_memory)).then(|| {
        Arc::new(Voice {
            path: record.soul_path,
            revision: record.revision,
            text: record.soul,
            memory_path: has_memory.then_some(record.memory_path),
            memory_text: record.memory.filter(|m| !m.trim().is_empty()),
        })
    }))
}

pub(crate) async fn capture_for(whale_id: Option<&str>) -> Result<Option<Arc<Voice>>> {
    let owned = whale_id.map(str::to_owned);
    codewhale_runtime::runtime_control::owner_work(move || capture_sync_for(owned.as_deref())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};

    #[test]
    fn validate_id_rejects_path_traversal_and_invalid_names() {
        for valid in ["default", "scout", "release-bot_1", "a", "9whale"] {
            assert!(validate_id(valid).is_ok(), "expected valid id: {valid}");
        }
        for invalid in [
            "",
            "..",
            ".",
            "../evil",
            "evil/../escape",
            "a/b",
            "a\\b",
            "bad\0id",
            "-leading",
            "_leading",
            "UpperCase",
            "with space",
            "con",
            "nul",
            "aux",
            "prn",
            "com1",
            "lpt9",
            &"a".repeat(65),
        ] {
            assert!(
                matches!(validate_id(invalid), Err(WhaleError::InvalidId(_))),
                "expected InvalidId for {invalid:?}"
            );
        }
    }

    #[test]
    fn multi_whale_crud_and_default_backward_compatibility() -> Result<()> {
        let _lock = lock_test_env();
        let temp = tempfile::tempdir()?;
        let cw_home = temp.path().join("cw-home");
        let _guard = EnvVarGuard::set("CODEWHALE_HOME", &cw_home);

        let initial_soul = load()?;
        assert_eq!(initial_soul.revision, "new");
        assert_eq!(initial_soul.text, "");
        assert!(initial_soul.writable);

        let initial_list = list_whales().expect("list empty whales");
        assert_eq!(initial_list.len(), 1);
        assert_eq!(initial_list[0].id, "default");
        assert_eq!(initial_list[0].revision, "new");
        assert!(matches!(
            update_whale(
                "default",
                "0000000000000000000000000000000000000000000000000000000000000000",
                UpdateWhaleInput {
                    purpose: Some("stale".into()),
                    ..Default::default()
                }
            ),
            Err(WhaleError::PreconditionFailed)
        ));
        assert!(!cw_home.join("whales/default").exists());
        assert!(matches!(
            create_whale(CreateWhaleInput {
                id: Some("../bad".into()),
                name: "Bad".into(),
                ..Default::default()
            }),
            Err(WhaleError::InvalidId(_))
        ));
        assert!(!cw_home.join("whales").exists());

        let saved_soul = save("new", "Be concise and verify facts.")?.expect("save default soul");
        assert_eq!(saved_soul.text, "Be concise and verify facts.");
        assert_ne!(saved_soul.revision, "new");
        assert!(save("new", "stale")?.is_none());

        let created = create_whale(CreateWhaleInput {
            id: None,
            name: "Security Reviewer".into(),
            purpose: Some("Audit auth boundaries".into()),
            color: Some("violet".into()),
            soul: Some("Check every trust boundary.".into()),
            memory: Some("Remember token rotation invariant.".into()),
            model: Some("deepseek-reasoner".into()),
            model_provider_id: Some("deepseek".into()),
            external_agent_id: None,
            permission_posture: Some("ask".into()),
            child_posture: Some("plan".into()),
            home_thread_id: None,
        })
        .expect("create specialist whale");
        assert_eq!(created.id, "security-reviewer");
        assert_eq!(created.name, "Security Reviewer");
        assert_eq!(created.color, "violet");
        assert_eq!(created.soul, "Check every trust boundary.");
        assert_eq!(
            created.memory.as_deref(),
            Some("Remember token rotation invariant.")
        );
        assert_eq!(created.model.as_deref(), Some("deepseek-reasoner"));
        assert_eq!(created.permission_posture.as_deref(), Some("ask"));
        assert_eq!(created.child_posture.as_deref(), Some("plan"));

        assert!(matches!(
            update_whale(
                "security-reviewer",
                "wrong-revision",
                UpdateWhaleInput {
                    purpose: Some("Updated".into()),
                    ..Default::default()
                }
            ),
            Err(WhaleError::PreconditionFailed)
        ));

        let updated = update_whale(
            "security-reviewer",
            &created.revision,
            UpdateWhaleInput {
                purpose: Some("Audit auth and sandbox boundaries".into()),
                soul: Some("Check auth and path confinement.".into()),
                ..Default::default()
            },
        )
        .expect("update specialist whale");
        assert_ne!(updated.revision, created.revision);
        assert_eq!(updated.purpose, "Audit auth and sandbox boundaries");
        assert_eq!(updated.soul, "Check auth and path confinement.");
        assert_eq!(
            updated.memory.as_deref(),
            Some("Remember token rotation invariant.")
        );

        let all = list_whales().expect("list whales");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "default");
        assert_eq!(all[1].id, "security-reviewer");

        assert!(matches!(
            delete_whale("default"),
            Err(WhaleError::DefaultProtected)
        ));
        assert!(matches!(
            delete_whale("../security-reviewer"),
            Err(WhaleError::InvalidId(_))
        ));

        delete_whale("security-reviewer").expect("delete specialist whale");
        assert!(matches!(
            get_whale("security-reviewer"),
            Err(WhaleError::NotFound(_))
        ));
        assert_eq!(list_whales().expect("list after delete").len(), 1);
        Ok(())
    }
}
