use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use codewhale_config::private_directory::PrivateDirectory;
use serde::{Deserialize, Serialize};

const ID: &str = "default";
const FILE: &str = "SOUL.md";
pub const MAX_BYTES: usize = 16 * 1024;

#[derive(Serialize)]
pub struct Soul {
    pub path: PathBuf,
    pub text: String,
    pub revision: String,
    pub writable: bool,
}

#[derive(Deserialize)]
struct LocalWhale {
    id: String,
    account_agent_id: Option<String>,
}

fn directory() -> Result<PathBuf> {
    Ok(codewhale_config::codewhale_home()?.join("whales").join(ID))
}

fn read_file(dir: &PrivateDirectory, name: &str) -> Result<Option<String>> {
    let bytes = dir.read_private_receipt(name, MAX_BYTES)?;
    bytes
        .map(|(bytes, _)| String::from_utf8(bytes).context("Whale documents must be UTF-8"))
        .transpose()
}

fn snapshot(path: &Path, dir: Option<&PrivateDirectory>) -> Result<Soul> {
    let manifest = dir
        .map(|dir| read_file(dir, "whale.toml"))
        .transpose()?
        .flatten();
    let identity = manifest
        .as_deref()
        .map(toml::from_str::<LocalWhale>)
        .transpose()
        .context("Invalid whale identity file")?;
    ensure!(
        identity.as_ref().is_none_or(|whale| whale.id == ID),
        "Whale identity changed"
    );
    let writable = identity
        .as_ref()
        .is_none_or(|whale| whale.account_agent_id.is_none());
    let text = dir.map(|dir| read_file(dir, FILE)).transpose()?.flatten();
    ensure!(
        text.is_none() || identity.is_some(),
        "An existing SOUL.md requires whale.toml with its local whale identity"
    );
    let revision = text.as_ref().map_or_else(
        || "new".to_owned(),
        |text| codewhale_runtime::hashing::sha256_hex(text.as_bytes()),
    );
    Ok(Soul {
        path: path.join(FILE),
        text: text.unwrap_or_default(),
        revision,
        writable,
    })
}

pub fn load() -> Result<Soul> {
    let path = directory()?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return snapshot(&path, None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let dir = PrivateDirectory::inspect(&path)?;
    snapshot(&path, Some(&dir))
}

pub fn save(expected: &str, text: &str) -> Result<Option<Soul>> {
    ensure!(
        text.len() <= MAX_BYTES,
        "SOUL.md exceeds the local whale file limit"
    );
    let path = directory()?;
    let dir = PrivateDirectory::admit(&path)?;
    let lock = dir.open_internal_file("soul.lock")?;
    lock.try_lock()
        .context("The local whale voice is being edited or cannot be locked; retry the edit")?;
    ensure!(dir.is_at_selected_path()?, "Whale directory changed");
    let current = snapshot(&path, Some(&dir))?;
    if current.revision != expected {
        return Ok(None);
    }
    ensure!(current.writable, "The account owns this imported whale");
    if read_file(&dir, "whale.toml")?.is_none() {
        dir.write_owned_file("whale.toml", b"id = \"default\"\n", false)?;
    }
    dir.write_owned_file(FILE, text.as_bytes(), true)?;
    Ok(Some(snapshot(&path, Some(&dir))?))
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Voice {
    path: PathBuf,
    revision: String,
    text: String,
}

impl std::fmt::Debug for Voice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhaleVoice")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl Voice {
    pub(crate) fn segment(&self) -> crate::project_context::InstructionSegment {
        use crate::project_context::{InstructionOrigin, InstructionScope, InstructionSegment};
        let mut segment = InstructionSegment::new(
            InstructionScope::Global,
            InstructionOrigin::WhaleSoul,
            Some(self.path.clone()),
            self.text.clone(),
        );
        segment.label = Some("SOUL.md".into());
        segment.canonical_path = Some(self.path.clone());
        segment
    }
}

pub(crate) async fn capture() -> Result<Option<Arc<Voice>>> {
    codewhale_runtime::runtime_control::owner_work(|| {
        let soul = load().context("Could not read the local whale identity and SOUL.md safely")?;
        Ok((soul.writable && !soul.text.trim().is_empty()).then(|| {
            Arc::new(Voice {
                path: soul.path,
                revision: soul.revision,
                text: soul.text,
            })
        }))
    })
    .await
}
