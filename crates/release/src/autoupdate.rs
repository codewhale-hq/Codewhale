//! Automatic updates for release-binary installs: the mode setting, the
//! organization policy that can override it, and the on-disk state under
//! `<CODEWHALE_HOME>/updates/`.
//!
//! - `staged/` holds one downloaded, checksum-verified release binary and
//!   `staged.json`. It is applied at the start of the next `codewhale` process,
//!   never while a session runs. An unsigned stage applies only after the
//!   person says yes once; a signed one applies by itself.
//! - `previous/` holds the binary that the last update or rollback replaced, so
//!   `codewhale update --rollback` can swap it back.
//! - `state.json` remembers a rolled-back version so it is not staged again,
//!   and the last staging attempt so offline or rate-limited machines retry at
//!   the check interval, not on every launch.
//!
//! Known limits: the user's `[update] mode` is read when the TUI stages; a
//! change to it before the next start does not cancel an already staged
//! update (policy and environment opt-outs do). Package-managed and app-bundled
//! copies never stage.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const UPDATES_DIR: &str = "updates";
pub const MODE_ENV: &str = "CODEWHALE_UPDATE_MODE";
pub const POLICY_PATH_ENV: &str = "CODEWHALE_UPDATE_POLICY_PATH";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMode {
    Auto,
    Notify,
    Off,
}

impl UpdateMode {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "on" | "true" => Some(Self::Auto),
            "notify" | "check" | "check-only" => Some(Self::Notify),
            "off" | "false" | "never" | "disabled" => Some(Self::Off),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Notify => "notify",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMode {
    pub mode: UpdateMode,
    pub source: String,
}

impl ResolvedMode {
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{} ({})", self.mode.label(), self.source)
    }
}

#[derive(Debug, Deserialize)]
struct PolicyFile {
    #[serde(default)]
    update: PolicyUpdate,
}

#[derive(Debug, Default, Deserialize)]
struct PolicyUpdate {
    mode: Option<String>,
}

fn policy_paths() -> Vec<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os(POLICY_PATH_ENV).filter(|p| !p.is_empty())
    {
        return vec![PathBuf::from(path)];
    }
    if cfg!(windows) {
        std::env::var_os("ProgramData")
            .map(|root| vec![PathBuf::from(root).join("Codewhale").join("policy.toml")])
            .unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/Library/Application Support/Codewhale/policy.toml"),
            PathBuf::from("/etc/codewhale/policy.toml"),
        ]
    } else {
        vec![PathBuf::from("/etc/codewhale/policy.toml")]
    }
}

#[cfg(unix)]
fn trusted_policy_owner(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    cfg!(debug_assertions) && std::env::var_os(POLICY_PATH_ENV).is_some()
        || (meta.uid() == 0 && meta.mode() & 0o022 == 0)
}

#[cfg(not(unix))]
fn trusted_policy_owner(_meta: &fs::Metadata) -> bool {
    true
}

#[must_use]
pub fn managed_policy_mode() -> Option<(UpdateMode, PathBuf)> {
    for path in policy_paths() {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || !trusted_policy_owner(&meta) || meta.len() > 64 * 1024 {
            continue;
        }
        let mode = fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<PolicyFile>(&text).ok())
            .map(|policy| policy.update.mode.as_deref().map(UpdateMode::parse));
        match mode {
            Some(None) => continue,
            Some(Some(Some(mode))) => return Some((mode, path)),
            Some(Some(None)) | None => return Some((UpdateMode::Off, path)),
        }
    }
    None
}

#[must_use]
pub fn resolve_mode(config_mode: Option<&str>, check_for_updates: bool) -> ResolvedMode {
    if let Some((mode, path)) = managed_policy_mode() {
        return ResolvedMode {
            mode,
            source: format!("set by your organization in {}", path.display()),
        };
    }
    if let Some(mode) = std::env::var(MODE_ENV)
        .ok()
        .as_deref()
        .and_then(UpdateMode::parse)
    {
        return ResolvedMode {
            mode,
            source: MODE_ENV.to_string(),
        };
    }
    if let Some(reason) = crate::suppression_reason() {
        return ResolvedMode {
            mode: UpdateMode::Off,
            source: reason.variable().to_string(),
        };
    }
    if !check_for_updates {
        return ResolvedMode {
            mode: UpdateMode::Off,
            source: "update.check_for_updates = false".to_string(),
        };
    }
    if let Some(mode) = config_mode.and_then(UpdateMode::parse) {
        return ResolvedMode {
            mode,
            source: "update.mode".to_string(),
        };
    }
    ResolvedMode {
        mode: UpdateMode::Auto,
        source: "default".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedUpdate {
    pub version: String,
    pub sha256: String,
    pub signed: bool,
    pub source: String,
    pub target_dir: PathBuf,
    #[serde(default)]
    pub declined: bool,
    #[serde(default)]
    pub staged_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousBinary {
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    #[serde(default)]
    pub skip_version: Option<String>,
    #[serde(default)]
    pub last_stage_attempt_unix: u64,
    #[serde(default)]
    pub last_stage_version: Option<String>,
}

#[must_use]
pub fn updates_dir(home: &Path) -> PathBuf {
    home.join(UPDATES_DIR)
}

#[must_use]
pub fn staged_dir(home: &Path) -> PathBuf {
    updates_dir(home).join("staged")
}

#[must_use]
pub fn previous_dir(home: &Path) -> PathBuf {
    updates_dir(home).join("previous")
}

#[must_use]
pub fn binary_name() -> String {
    format!("codewhale{}", std::env::consts::EXE_SUFFIX)
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub fn store_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path.parent().context("state path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("failed to write {}", path.display()))
}

#[must_use]
pub fn load_staged(home: &Path) -> Option<StagedUpdate> {
    load_json(&staged_dir(home).join("staged.json"))
}

pub fn store_staged(home: &Path, staged: &StagedUpdate) -> Result<()> {
    store_json(&staged_dir(home).join("staged.json"), staged)
}

pub fn discard_staged(home: &Path) {
    let _ = fs::remove_dir_all(staged_dir(home));
}

#[must_use]
pub fn load_previous(home: &Path) -> Option<PreviousBinary> {
    load_json(&previous_dir(home).join("previous.json"))
}

#[must_use]
pub fn load_state(home: &Path) -> UpdateState {
    load_json(&updates_dir(home).join("state.json")).unwrap_or_default()
}

pub fn store_state(home: &Path, state: &UpdateState) -> Result<()> {
    store_json(&updates_dir(home).join("state.json"), state)
}

#[must_use]
pub fn changelog_url(version: &str) -> String {
    format!(
        "https://codewhale.net/en/changelog/{}",
        version.trim().trim_start_matches('v')
    )
}
