//! Every Codewhale copy on this machine, who owns it, and what we may do with it.
//!
//! The inventory never executes a file it finds. It reads names, metadata, the
//! install receipt, package-manager metadata and, for candidate files, the
//! bytes (bounded) to look for a marker compiled into every Codewhale build.
//!
//! What we may change is decided here once, for `doctor`, `doctor --fix`,
//! `update`, `uninstall` and the installers:
//!
//! - receipt-owned and adoptable copies may be updated;
//! - backup leftovers may be moved into the recovery folder, never deleted;
//! - package-managed, app-bundled, symlinked, other-user and foreign files are
//!   only ever reported, with the owner's own command where there is one.
//!
//! Known limits: candidates come from `PATH`, the running executable's
//! directory, `~/.local/bin` and `~/.cargo/bin`, not the whole disk. A copy
//! whose bytes differ from the running one reports "version unknown" because
//! it is never run. Recovery expiry deletes only what a recovery manifest lists.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::install::InstallMethod;

pub const RECEIPT_FILE: &str = ".codewhale-install.sha256";
#[cfg(windows)]
pub const RECEIPT_COMMANDS: &[&str] = &["codewhale.exe", "codew.exe", "codewhale.bat"];
#[cfg(not(windows))]
pub const RECEIPT_COMMANDS: &[&str] = &["codewhale", "codew"];
pub const RECOVERY_DIR: &str = "recovery";
pub const RECOVERY_MANIFEST: &str = "manifest.tsv";
pub const RECOVERY_RETENTION_DAYS: u64 = 30;
const VOLUME_RECOVERY_DIR: &str = ".codewhale-recovery";
const MAX_SCAN_BYTES: u64 = 1024 * 1024 * 1024;
const COMMAND_NAMES: &[&str] = &[
    "codewhale-tui",
    "deepseek-tui",
    "codewhale",
    "deepseek",
    "codew",
];
const MARKERS: &[&[u8]] = &[crate::UPDATE_USER_AGENT.as_bytes(), b"deepseek-tui"];
const LEFTOVER_DIR_PREFIXES: &[&str] = &[
    ".codewhale-backup-",
    ".codewhale-uninstall-",
    ".codewhale-update-",
    ".codewhale-install.",
    ".codewhale-receipt.",
];
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallClass {
    ReceiptOwned,
    PackageManaged(InstallMethod),
    AppBundle,
    OursUnreceipted,
    BackupLeftover,
    NotOurs(&'static str),
}

impl InstallClass {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::ReceiptOwned => "receipt-owned",
            Self::PackageManaged(_) => "package-managed",
            Self::AppBundle => "desktop app",
            Self::OursUnreceipted => "unreceipted",
            Self::BackupLeftover => "backup",
            Self::NotOurs(_) => "left alone",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallEntry {
    pub path: PathBuf,
    pub class: InstallClass,
    pub is_dir: bool,
    pub bytes: u64,
    pub active: bool,
    pub same_as_running: bool,
}

impl InstallEntry {
    #[must_use]
    pub fn owner_command(&self) -> Option<String> {
        match &self.class {
            InstallClass::PackageManaged(method) => Some(manager_command(*method, &self.path)),
            InstallClass::AppBundle => Some(crate::install::DESKTOP_APP_UPDATE.to_string()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub entries: Vec<InstallEntry>,
    pub running: Option<PathBuf>,
}

impl Inventory {
    pub fn leftovers(&self) -> impl Iterator<Item = &InstallEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.class == InstallClass::BackupLeftover)
    }

    #[must_use]
    pub fn leftover_bytes(&self) -> u64 {
        self.leftovers().map(|entry| entry.bytes).sum()
    }

    #[must_use]
    pub fn active(&self, command: &str) -> Option<&InstallEntry> {
        self.entries
            .iter()
            .find(|entry| entry.active && command_stem(&entry.path) == Some(command))
    }

    #[must_use]
    pub fn summary_lines(&self, brief: bool, version: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let others: Vec<&InstallEntry> = self
            .entries
            .iter()
            .filter(|entry| {
                entry.class != InstallClass::BackupLeftover
                    && !entry.active
                    && Some(entry.path.as_path()) != self.running.as_deref()
                    && !(entry.same_as_running
                        && entry.path.parent() == self.running.as_deref().and_then(Path::parent))
            })
            .collect();
        let leftovers = self.leftovers().count();
        if brief && leftovers == 0 && others.is_empty() {
            return lines;
        }
        match self.active("codewhale") {
            Some(active) => lines.push(format!(
                "Your shell runs {} ({}, {}).",
                display(&active.path),
                if active.same_as_running {
                    format!("v{version}")
                } else {
                    "version unknown".to_string()
                },
                active.class.label()
            )),
            None => lines.push("No codewhale command is on PATH.".to_string()),
        }
        if leftovers > 0 {
            lines.push(format!(
                "{leftovers} old Codewhale backup(s) use {}. `codewhale doctor --fix` moves them to the recovery folder (kept {RECOVERY_RETENTION_DAYS} days); nothing is deleted.",
                human_bytes(self.leftover_bytes())
            ));
        }
        for entry in self
            .entries
            .iter()
            .filter(|entry| entry.class == InstallClass::OursUnreceipted && !entry.same_as_running)
        {
            lines.push(format!(
                "{} is an older Codewhale without a receipt; `codewhale update` or `codewhale doctor --fix` brings it in line.",
                display(&entry.path)
            ));
        }
        if !others.is_empty() {
            lines.push("Left alone (not changed):".to_string());
            for entry in others.iter().take(if brief { 6 } else { usize::MAX }) {
                let why = match &entry.class {
                    InstallClass::PackageManaged(method) => format!(
                        "managed by {}: {}",
                        method.label(),
                        entry.owner_command().unwrap_or_default()
                    ),
                    InstallClass::AppBundle => {
                        format!(
                            "part of the Codewhale app: {}",
                            crate::install::DESKTOP_APP_UPDATE
                        )
                    }
                    InstallClass::NotOurs(reason) => (*reason).to_string(),
                    InstallClass::ReceiptOwned => "another receipted install".to_string(),
                    InstallClass::OursUnreceipted => "another Codewhale copy".to_string(),
                    InstallClass::BackupLeftover => unreachable!(),
                };
                lines.push(format!("  {}  {why}", display(&entry.path)));
            }
            if brief && others.len() > 6 {
                lines.push(format!(
                    "  and {} more (codewhale doctor --installs)",
                    others.len() - 6
                ));
            }
        }
        lines
    }
}

#[must_use]
pub fn inventory(running: Option<&Path>) -> Inventory {
    let running_hash = running.and_then(|path| file_sha256(path).ok());
    let path_dirs: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let home = home_dir();
    let mut push = |dir: PathBuf| {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            return;
        }
        let key = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !dirs
            .iter()
            .any(|seen| fs::canonicalize(seen).unwrap_or_else(|_| seen.clone()) == key)
        {
            dirs.push(dir);
        }
    };
    for dir in &path_dirs {
        push(dir.clone());
    }
    if let Some(parent) = running.and_then(Path::parent) {
        push(parent.to_path_buf());
    }
    if let Some(home) = &home {
        push(home.join(".local").join("bin"));
        push(home.join(".cargo").join("bin"));
    }

    let owner = home.as_deref().and_then(owner_id);
    let mut entries = Vec::new();
    for dir in &dirs {
        let Ok(read) = fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<_> = read.flatten().map(|entry| entry.file_name()).collect();
        names.sort();
        for name in names {
            let Some(name) = name.to_str() else { continue };
            let path = dir.join(name);
            if let Some(class) = classify(&path, name, owner, running_hash.as_deref()) {
                let meta = fs::symlink_metadata(&path).ok();
                let is_dir = meta.as_ref().is_some_and(fs::Metadata::is_dir);
                let bytes = if is_dir {
                    dir_bytes(&path)
                } else {
                    meta.as_ref().map_or(0, fs::Metadata::len)
                };
                let same_as_running = !is_dir
                    && running_hash.is_some()
                    && meta.as_ref().is_some_and(|meta| meta.is_file())
                    && command_stem(&path).is_some()
                    && file_sha256(&path).ok() == running_hash;
                entries.push(InstallEntry {
                    path,
                    class,
                    is_dir,
                    bytes,
                    active: false,
                    same_as_running,
                });
            }
        }
    }
    for command in ["codewhale", "codew"] {
        let file = format!("{command}{}", std::env::consts::EXE_SUFFIX);
        if let Some(first) = path_dirs
            .iter()
            .map(|dir| dir.join(&file))
            .find(|path| fs::symlink_metadata(path).is_ok())
            && let Some(entry) = entries.iter_mut().find(|entry| entry.path == first)
        {
            entry.active = true;
        }
    }
    Inventory {
        entries,
        running: running.map(Path::to_path_buf),
    }
}

fn classify(
    path: &Path,
    name: &str,
    owner: Option<u64>,
    running_hash: Option<&str>,
) -> Option<InstallClass> {
    let meta = fs::symlink_metadata(path).ok()?;
    let stem = name
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .filter(|_| !std::env::consts::EXE_SUFFIX.is_empty())
        .unwrap_or(name);
    let exact = COMMAND_NAMES.contains(&stem);
    let leftover_name = is_backup_name(name);
    let leftover_dir = LEFTOVER_DIR_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix));
    let host_recovery = name.starts_with(".codewhale-host-recovery-");
    let related = exact
        || leftover_name
        || leftover_dir
        || host_recovery
        || name.starts_with("codew")
        || name.starts_with("deepseek");
    if !related {
        return None;
    }
    let method = InstallMethod::owner(path);
    if method == InstallMethod::DesktopApp {
        return Some(InstallClass::AppBundle);
    }
    // Cargo owns only the names its record lists; other files in its `bin`
    // (old backups, renamed copies) are judged below like any directory.
    if !method.supports_self_update()
        && (method != InstallMethod::Cargo || meta.is_symlink() || cargo_lists(path, name))
    {
        return Some(InstallClass::PackageManaged(method));
    }
    if meta.is_symlink() {
        return Some(InstallClass::NotOurs("symlink; not changed"));
    }
    if !owned_by(&meta, owner) {
        return Some(InstallClass::NotOurs("owned by another user"));
    }
    if leftover_dir {
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_AFTER);
        return Some(if !stale {
            InstallClass::NotOurs("changed within the last hour, so it may be in use; not changed")
        } else if meta.is_dir() || name.starts_with(".codewhale-update-") {
            InstallClass::BackupLeftover
        } else {
            InstallClass::NotOurs("an install receipt or marker file, not a backup; not changed")
        });
    }
    if host_recovery {
        return Some(if meta.is_file() && meta.len() <= 64 * 1024 {
            InstallClass::BackupLeftover
        } else {
            InstallClass::NotOurs("not a Codewhale backup")
        });
    }
    if !meta.is_file() {
        return Some(InstallClass::NotOurs("not a regular file"));
    }
    if exact {
        if receipt_owns(path, name).unwrap_or(false) {
            return Some(InstallClass::ReceiptOwned);
        }
        if running_hash.is_some() && file_sha256(path).ok().as_deref() == running_hash {
            return Some(InstallClass::OursUnreceipted);
        }
        return Some(if is_codewhale_binary(path) {
            InstallClass::OursUnreceipted
        } else {
            InstallClass::NotOurs("not a Codewhale binary")
        });
    }
    if leftover_name {
        return Some(if is_codewhale_binary(path) {
            InstallClass::BackupLeftover
        } else {
            InstallClass::NotOurs("not a Codewhale binary")
        });
    }
    Some(InstallClass::NotOurs("different program"))
}

#[must_use]
pub fn is_backup_name(name: &str) -> bool {
    let name = name
        .strip_suffix(".exe")
        .filter(|_| cfg!(windows))
        .unwrap_or(name);
    COMMAND_NAMES.iter().any(|base| {
        let Some(rest) = name.strip_prefix(base) else {
            return false;
        };
        let Some(suffix) = rest.strip_prefix(['-', '.']) else {
            return false;
        };
        let suffix = suffix.strip_prefix(['v', 'V']).unwrap_or(suffix);
        if [
            "bak", "before-", "pre-", "pre_", "prev", "debug-", "orig", "backup",
        ]
        .iter()
        .any(|prefix| suffix.starts_with(prefix))
        {
            return true;
        }
        if let Some(pid) = suffix.strip_prefix("old-") {
            return pid.starts_with(|c: char| c.is_ascii_digit());
        }
        semver_backup_suffix(suffix)
    })
}

fn semver_backup_suffix(suffix: &str) -> bool {
    let mut parts = suffix.splitn(3, '.');
    let (Some(major), Some(minor), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if !digits(major) || !digits(minor) {
        return false;
    }
    let patch_len = rest.chars().take_while(char::is_ascii_digit).count();
    if patch_len == 0 {
        return false;
    }
    let mut tail = &rest[patch_len..];
    if let Some(hex) = tail.strip_prefix('-') {
        let len = hex.chars().take_while(char::is_ascii_hexdigit).count();
        if len == 0 {
            return false;
        }
        tail = &hex[len..];
    }
    tail.is_empty()
        || tail.starts_with(".bak")
        || tail.starts_with("-bak")
        || tail.starts_with(".old")
}

fn command_stem(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .filter(|_| !std::env::consts::EXE_SUFFIX.is_empty())
        .unwrap_or(name);
    COMMAND_NAMES.contains(&stem).then_some(stem)
}

#[must_use]
pub fn is_codewhale_binary(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    let native = magic == *b"\x7fELF"
        || magic[..2] == *b"MZ"
        || matches!(
            u32::from_be_bytes(magic),
            0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe | 0xcafe_babe
        );
    if !native {
        return false;
    }
    let longest = MARKERS.iter().map(|m| m.len()).max().unwrap_or(0);
    let mut reader = std::io::BufReader::with_capacity(1 << 20, file.take(MAX_SCAN_BYTES));
    let mut carry: Vec<u8> = magic.to_vec();
    loop {
        let chunk = match reader.fill_buf() {
            Ok([]) => return false,
            Ok(chunk) => chunk,
            Err(_) => return false,
        };
        let len = chunk.len();
        carry.extend_from_slice(chunk);
        reader.consume(len);
        if MARKERS
            .iter()
            .any(|marker| carry.windows(marker.len()).any(|window| window == *marker))
        {
            return true;
        }
        let keep = carry.len().saturating_sub(longest);
        carry.drain(..keep);
    }
}

pub fn file_sha256(path: &Path) -> Result<String> {
    let file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    reader_sha256(file).with_context(|| format!("failed to read {}", path.display()))
}

pub fn reader_sha256(mut reader: impl Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()[..]))
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes)[..])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn receipt_owns(path: &Path, name: &str) -> Result<bool> {
    let receipt = path.with_file_name(RECEIPT_FILE);
    let Ok(text) = read_small(&receipt, 4096) else {
        return Ok(false);
    };
    let digest = file_sha256(path)?;
    Ok(text.lines().any(|line| {
        let mut parts = line.split_whitespace();
        parts.next() == Some(digest.as_str())
            && parts.next().map(|n| n.trim_start_matches('*')) == Some(name)
    }))
}

fn read_small(path: &Path, limit: u64) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > limit {
        bail!("{} is not a small regular file", path.display());
    }
    Ok(fs::read_to_string(path)?)
}

fn cargo_root_beside(path: &Path) -> Option<PathBuf> {
    let bin = path.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let root = bin.parent()?;
    [".crates.toml", ".crates2.json"]
        .iter()
        .any(|name| fs::symlink_metadata(root.join(name)).is_ok())
        .then(|| root.to_path_buf())
}

pub(crate) fn cargo_owns_a_command_beside(path: &Path) -> bool {
    let Some(root) = cargo_root_beside(path) else {
        return false;
    };
    [".crates.toml", ".crates2.json"].iter().any(|file| {
        let record = root.join(file);
        if fs::symlink_metadata(&record).is_err() {
            return false;
        }
        read_small(&record, 4 * 1024 * 1024).map_or(true, |text| {
            RECEIPT_COMMANDS
                .iter()
                .any(|name| text.contains(&format!("\"{name}\"")))
        })
    })
}

fn cargo_lists(path: &Path, name: &str) -> bool {
    let Some(root) = cargo_root_beside(path) else {
        return false;
    };
    let quoted = format!("\"{name}\"");
    [".crates.toml", ".crates2.json"].iter().any(|file| {
        read_small(&root.join(file), 4 * 1024 * 1024).is_ok_and(|text| text.contains(&quoted))
    })
}

#[cfg(unix)]
fn owner_id(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|meta| u64::from(meta.uid()))
}

#[cfg(not(unix))]
fn owner_id(_path: &Path) -> Option<u64> {
    None
}

#[cfg(unix)]
fn owned_by(meta: &fs::Metadata, owner: Option<u64>) -> bool {
    use std::os::unix::fs::MetadataExt;
    owner.is_none_or(|owner| u64::from(meta.uid()) == owner)
}

#[cfg(not(unix))]
fn owned_by(_meta: &fs::Metadata, _owner: Option<u64>) -> bool {
    true
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| fs::symlink_metadata(entry.path()).ok().map(|m| (entry, m)))
        .map(|(entry, meta)| {
            if meta.is_dir() {
                dir_bytes(&entry.path())
            } else if meta.is_file() {
                meta.len()
            } else {
                0
            }
        })
        .sum()
}

fn display(path: &Path) -> String {
    if let Some(home) = home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

/// The command that removes the copy at `executable` through its owner.
#[must_use]
pub fn manager_command(method: InstallMethod, executable: &Path) -> String {
    owner_command(method, executable, false)
}

/// The command that updates the copy at `executable` through its owner,
/// with the exact Cargo root or npm prefix that installed it.
#[must_use]
pub fn manager_update_command(method: InstallMethod, executable: &Path) -> String {
    owner_command(method, executable, true)
}

/// One line answering "can this copy update itself, and if not, what does?"
/// Shared by `codewhale update`, `/update`, rollback and `doctor`.
#[must_use]
pub fn update_answer(method: InstallMethod, executable: &Path) -> String {
    match method {
        InstallMethod::DesktopApp => crate::install::DESKTOP_APP_UPDATE_HELP.to_string(),
        InstallMethod::System => "This copy is in a system or package directory, so it cannot update itself; update it with the package manager that installed it.".to_string(),
        method if method.supports_self_update() => {
            "This copy updates itself: `codewhale update`.".to_string()
        }
        method => format!(
            "This copy belongs to {}, so it cannot update itself; update it with `{}`.",
            method.label(),
            manager_update_command(method, executable)
        ),
    }
}

fn owner_command(method: InstallMethod, executable: &Path, update: bool) -> String {
    let real = fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    let names: Vec<_> = real.components().map(|part| part.as_os_str()).collect();
    let position = |marker: &str| {
        names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(marker))
    };
    let fallback = || {
        if update {
            method.update_command()
        } else {
            method.uninstall_command()
        }
        .to_string()
    };
    match method {
        InstallMethod::Cargo => match real.parent().and_then(Path::parent) {
            Some(root) if update => format!(
                "cargo install codewhale-cli --locked --force --root {}",
                quoted(root)
            ),
            Some(root) => format!("cargo uninstall codewhale-cli --root {}", quoted(root)),
            None => fallback(),
        },
        InstallMethod::Homebrew if !update => {
            match position("Cellar").and_then(|index| names.get(index + 1)) {
                Some(formula) => format!("brew uninstall {}", formula.to_string_lossy()),
                None => fallback(),
            }
        }
        InstallMethod::Npm => match position("node_modules") {
            Some(index) if index > 0 => {
                let mut root: PathBuf = names[..index].iter().collect();
                if !cfg!(windows) && root.file_name().is_some_and(|name| name == "lib") {
                    root.pop();
                }
                if update {
                    format!("npm install -g codewhale@latest --prefix {}", quoted(&root))
                } else {
                    format!("npm uninstall -g codewhale --prefix {}", quoted(&root))
                }
            }
            _ => fallback(),
        },
        InstallMethod::WindowsInstaller if !update => {
            match crate::install::windows_installer_uninstaller(&real) {
                Some(uninstaller) => format!("{}, or run {}", fallback(), quoted(&uninstaller)),
                None => fallback(),
            }
        }
        _ => fallback(),
    }
}

#[must_use]
pub fn utc_stamp(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn stamp_unix(stamp: &str) -> Option<u64> {
    let digits: String = stamp
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    if digits.len() != 14 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| digits[range].parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(4..6)?, num(6..8)?);
    let (hour, minute, second) = (num(8..10)?, num(10..12)?, num(12..14)?);
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryMove {
    pub from: PathBuf,
    pub to: PathBuf,
    pub bytes: u64,
}

impl RecoveryMove {
    #[must_use]
    pub fn undo_command(&self) -> String {
        if cfg!(windows) {
            format!(
                "Move-Item -LiteralPath '{}' -Destination '{}'",
                self.to.display(),
                self.from.display()
            )
        } else {
            format!(
                "mv -n {} {}",
                shell_quote(&self.to),
                shell_quote(&self.from)
            )
        }
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

pub struct RecoveryBatch {
    pub stamp: String,
    pub dir: PathBuf,
    pub moves: Vec<RecoveryMove>,
    next: usize,
}

impl RecoveryBatch {
    pub fn open(home: &Path) -> Result<Self> {
        let root = home.join(RECOVERY_DIR);
        create_private_dir(&root)?;
        let base = utc_stamp(now_unix());
        for attempt in 0..100 {
            let stamp = if attempt == 0 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            let dir = root.join(&stamp);
            match fs::create_dir(&dir) {
                Ok(()) => {
                    return Ok(Self {
                        stamp,
                        dir,
                        moves: Vec::new(),
                        next: 1,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to create {}", dir.display()));
                }
            }
        }
        bail!(
            "could not choose a recovery folder under {}",
            root.display()
        )
    }

    pub fn move_in(&mut self, path: &Path) -> Result<RecoveryMove> {
        self.move_in_replaced(path, None)
    }

    fn move_in_replaced(&mut self, path: &Path, replacement: Option<&str>) -> Result<RecoveryMove> {
        let original = path.to_string_lossy();
        if original.contains(['\t', '\n', '\r']) {
            bail!(
                "refusing to move {}: its name contains a tab or newline",
                path.display()
            );
        }
        let meta = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if meta.is_symlink() {
            bail!("refusing to move symlink {}", path.display());
        }
        let digest = if meta.is_file() && meta.len() <= MAX_SCAN_BYTES {
            file_sha256(path)?
        } else {
            "-".to_string()
        };
        let bytes = if meta.is_dir() {
            dir_bytes(path)
        } else {
            meta.len()
        };
        let name = path
            .file_name()
            .context("path has no file name")?
            .to_string_lossy()
            .into_owned();
        let stored_name = format!("{:03}-{name}", self.next);
        self.next += 1;
        let mut to = self.dir.join(&stored_name);
        if let Err(error) = fs::rename(path, &to) {
            if !is_cross_device(&error) {
                return Err(error).with_context(|| {
                    format!("failed to move {} to {}", path.display(), to.display())
                });
            }
            let volume = path
                .parent()
                .context("path has no parent")?
                .join(VOLUME_RECOVERY_DIR)
                .join(&self.stamp);
            fs::create_dir_all(&volume)
                .with_context(|| format!("failed to create {}", volume.display()))?;
            to = volume.join(&stored_name);
            fs::rename(path, &to).with_context(|| {
                format!("failed to move {} to {}", path.display(), to.display())
            })?;
        }
        let suffix = replacement
            .map(|digest| format!("\t{digest}"))
            .unwrap_or_default();
        let line = format!("{original}\t{}\t{digest}\t{bytes}{suffix}\n", to.display());
        let mut manifest = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(RECOVERY_MANIFEST))
            .context("failed to open the recovery manifest")?;
        manifest
            .write_all(line.as_bytes())
            .and_then(|()| manifest.sync_all())
            .context("failed to record the move in the recovery manifest")?;
        let moved = RecoveryMove {
            from: path.to_path_buf(),
            to,
            bytes,
        };
        self.moves.push(moved.clone());
        Ok(moved)
    }

    pub fn finish(self) -> Vec<RecoveryMove> {
        if self.moves.is_empty() {
            let _ = fs::remove_dir(&self.dir);
        }
        self.moves
    }
}

fn is_cross_device(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::CrossesDevices || error.raw_os_error() == Some(18)
}

fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RecoveryRecord {
    pub original: PathBuf,
    pub stored: PathBuf,
    pub sha256: Option<String>,
    pub bytes: u64,
    pub replacement_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecoveryFolder {
    pub stamp: String,
    pub dir: PathBuf,
    pub records: Vec<RecoveryRecord>,
    pub created_unix: Option<u64>,
}

impl RecoveryFolder {
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.records.iter().map(|record| record.bytes).sum()
    }

    #[must_use]
    pub fn expires_in_days(&self, now: u64) -> Option<u64> {
        let created = self.created_unix?;
        let expiry = created + RECOVERY_RETENTION_DAYS * 86_400;
        Some(expiry.saturating_sub(now).div_ceil(86_400))
    }
}

#[must_use]
pub fn recovery_folders(home: &Path) -> Vec<RecoveryFolder> {
    let root = home.join(RECOVERY_DIR);
    let Ok(read) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut folders: Vec<RecoveryFolder> = read
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let stamp = entry.file_name().to_str()?.to_string();
            let dir = entry.path();
            let records = read_manifest(&dir);
            Some(RecoveryFolder {
                created_unix: stamp_unix(&stamp),
                stamp,
                dir,
                records,
            })
        })
        .collect();
    folders.sort_by(|a, b| a.stamp.cmp(&b.stamp));
    folders
}

fn read_manifest(dir: &Path) -> Vec<RecoveryRecord> {
    let Ok(text) = fs::read_to_string(dir.join(RECOVERY_MANIFEST)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let original = PathBuf::from(fields.next()?);
            let stored = PathBuf::from(fields.next()?);
            let sha256 = fields
                .next()
                .filter(|digest| digest.len() == 64)
                .map(str::to_ascii_lowercase);
            let bytes = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            Some(RecoveryRecord {
                original,
                stored,
                sha256,
                bytes,
                replacement_sha256: fields.next().map(str::to_string),
            })
        })
        .collect()
}

fn stored_inside_recovery(folder: &RecoveryFolder, stored: &Path) -> bool {
    stored.starts_with(&folder.dir)
        || stored.parent().is_some_and(|parent| {
            parent.file_name().and_then(|n| n.to_str()) == Some(folder.stamp.as_str())
                && parent
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == VOLUME_RECOVERY_DIR)
        })
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub restored: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub displaced: Option<String>,
}

pub fn parse_command_receipt(text: &str) -> Result<HashMap<String, String>> {
    let hashes = crate::parse_checksum_manifest(text)?;
    if hashes.is_empty()
        || hashes.len() != text.lines().count()
        || hashes
            .keys()
            .any(|name| !RECEIPT_COMMANDS.contains(&name.as_str()))
    {
        bail!("invalid command ownership receipt");
    }
    Ok(hashes)
}

fn verified_recovery_file(
    folder: &RecoveryFolder,
    record: &RecoveryRecord,
) -> Result<fs::Metadata> {
    if !stored_inside_recovery(folder, &record.stored) {
        bail!("manifest entry points outside the recovery folder");
    }
    let meta = fs::symlink_metadata(&record.stored).context("no longer in the recovery folder")?;
    if meta.is_symlink() {
        bail!("stored entry is a symlink");
    }
    if record.sha256.is_some() && !meta.is_file() {
        bail!("stored entry is no longer a regular file");
    }
    if meta.is_file() {
        match &record.sha256 {
            Some(expected) => {
                if &file_sha256(&record.stored)? != expected {
                    bail!("its bytes changed in the recovery folder");
                }
            }
            None if record.bytes != meta.len() => {
                bail!("its size changed in the recovery folder");
            }
            None => {}
        }
    }
    Ok(meta)
}

struct DisplacedReceipt {
    path: PathBuf,
    retained: Vec<(String, String)>,
}

fn command_receipt_to_displace(
    directory: &Path,
    records: &BTreeMap<&Path, &RecoveryRecord>,
    owner: Option<u64>,
) -> Result<Option<DisplacedReceipt>> {
    let receipt = directory.join(RECEIPT_FILE);
    let meta = match fs::symlink_metadata(&receipt) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let dir_meta = fs::symlink_metadata(directory)?;
    if !dir_meta.is_dir()
        || !owned_by(&dir_meta, owner)
        || !directory_is_private(directory)
        || !meta.is_file()
        || !owned_by(&meta, owner)
    {
        bail!("preserved untrusted command receipt {}", receipt.display());
    }
    let claims = parse_command_receipt(&read_small(&receipt, 512)?)?;
    let mut changes_owned_bytes = false;
    let mut retained = Vec::new();
    for name in RECEIPT_COMMANDS {
        let command = directory.join(name);
        if let Some(record) = records.get(command.as_path())
            && record.replacement_sha256.is_none()
            && !claims.contains_key(*name)
            && fs::symlink_metadata(&command).is_ok()
        {
            bail!("preserved unclaimed command {}", command.display());
        }
    }
    for (name, expected) in claims {
        let path = directory.join(&name);
        match fs::symlink_metadata(&path) {
            Ok(current)
                if current.is_file()
                    && owned_by(&current, owner)
                    && file_sha256(&path)? == expected => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && records.contains_key(path.as_path()) => {}
            _ => bail!("preserved modified command {}", path.display()),
        }
        if let Some(record) = records.get(path.as_path()) {
            let changes = match &record.sha256 {
                Some(digest) => digest != &expected,
                None => file_sha256(&record.stored)? != expected,
            };
            if changes {
                changes_owned_bytes = true;
                continue;
            }
        }
        retained.push((name, expected));
    }
    let saved = records.contains_key(receipt.as_path());
    if saved {
        retained.clear();
    }
    Ok((changes_owned_bytes || saved).then_some(DisplacedReceipt {
        path: receipt,
        retained,
    }))
}

fn validate_saved_command_receipt(
    record: &RecoveryRecord,
    pending: &BTreeMap<&Path, &RecoveryRecord>,
) -> Result<()> {
    let directory = record
        .original
        .parent()
        .context("command receipt has no directory")?;
    for (name, expected) in parse_command_receipt(&read_small(&record.stored, 512)?)? {
        let command = directory.join(name);
        let bytes = pending
            .get(command.as_path())
            .map_or(command.as_path(), |entry| entry.stored.as_path());
        if !fs::symlink_metadata(bytes)?.is_file() || file_sha256(bytes)? != expected {
            bail!(
                "preserved recovery receipt: {} has different bytes",
                command.display()
            );
        }
    }
    Ok(())
}

pub fn restore(home: &Path, stamp: Option<&str>) -> Result<(String, RestoreReport)> {
    let folders = recovery_folders(home);
    let folder = match stamp {
        Some(stamp) => folders
            .into_iter()
            .find(|folder| folder.stamp == stamp)
            .with_context(|| {
                format!(
                    "no recovery folder {stamp} under {}",
                    home.join(RECOVERY_DIR).display()
                )
            })?,
        None => folders
            .into_iter()
            .rev()
            .find(|folder| !folder.records.is_empty())
            .context("there is no recovery folder to restore")?,
    };
    let mut report = RestoreReport::default();
    let mut remaining = Vec::new();
    let records: BTreeMap<_, _> = folder
        .records
        .iter()
        .map(|record| (record.original.as_path(), record))
        .collect();
    let directories: BTreeSet<_> = records
        .keys()
        .filter_map(|path| {
            path.parent().filter(|_| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|name| name == RECEIPT_FILE || RECEIPT_COMMANDS.contains(&name))
            })
        })
        .collect();
    for record in records.values() {
        if let Some(expected) = &record.replacement_sha256 {
            let current = match fs::symlink_metadata(&record.original) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !current.is_file()
                || !owned_by(&current, owner_id(home))
                || file_sha256(&record.original)? != *expected
            {
                bail!("preserved modified command {}", record.original.display());
            }
        }
    }
    let mut receipts = Vec::new();
    for dir in directories {
        let receipt = dir.join(RECEIPT_FILE);
        let displace = command_receipt_to_displace(dir, &records, owner_id(home))?;
        let saved = records.get(receipt.as_path());
        if saved.is_some() || displace.is_some() {
            for name in RECEIPT_COMMANDS {
                if let Some(command) = records.get(dir.join(name).as_path())
                    && !verified_recovery_file(&folder, command)?.is_file()
                {
                    bail!(
                        "saved command {} is not a regular file",
                        command.stored.display()
                    );
                }
            }
        }
        if let Some(saved) = saved {
            verified_recovery_file(&folder, saved)?;
            validate_saved_command_receipt(saved, &records)?;
        }
        if let Some(receipt) = displace {
            receipts.push(receipt);
        }
    }
    let mut displaced = None;
    if !receipts.is_empty() {
        let mut batch = RecoveryBatch::open(home)?;
        for receipt in &receipts {
            batch.move_in(&receipt.path)?;
        }
        displaced = Some(batch);
    }
    let mut ordered: Vec<_> = folder.records.iter().collect();
    ordered.sort_by_key(|record| {
        record
            .original
            .file_name()
            .is_some_and(|name| name == RECEIPT_FILE)
    });
    for record in ordered {
        let outcome = (|| -> Result<()> {
            let meta = verified_recovery_file(&folder, record)?;
            if record
                .original
                .file_name()
                .is_some_and(|name| name == RECEIPT_FILE)
            {
                validate_saved_command_receipt(record, &BTreeMap::new())?;
            }
            if let Ok(current) = fs::symlink_metadata(&record.original) {
                if !current.is_file() || !meta.is_file() {
                    bail!("something else is at the original path now");
                }
                if displaced.is_none() {
                    displaced = Some(RecoveryBatch::open(home)?);
                }
                let batch = displaced.as_mut().expect("batch was just opened");
                let replacement = record
                    .original
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| RECEIPT_COMMANDS.contains(name))
                    .map(|_| file_sha256(&record.stored))
                    .transpose()?;
                batch.move_in_replaced(&record.original, replacement.as_deref())?;
            }
            fs::rename(&record.stored, &record.original).context("move back failed")?;
            Ok(())
        })();
        match outcome {
            Ok(()) => report.restored.push(record.original.clone()),
            Err(error) => {
                if fs::symlink_metadata(&record.stored).is_ok() {
                    remaining.push(record.clone());
                }
                report
                    .skipped
                    .push((record.original.clone(), format!("{error:#}")));
            }
        }
    }
    if let Some(batch) = displaced {
        let stamp = batch.stamp.clone();
        if !batch.finish().is_empty() {
            report.displaced = Some(stamp);
        }
    }
    for receipt in receipts {
        if !receipt.retained.is_empty()
            && !adopt_receipt(
                receipt.path.parent().context("receipt has no directory")?,
                &receipt.retained,
            )?
        {
            bail!("command ownership receipt changed during restore");
        }
    }
    rewrite_manifest(&folder.dir, &remaining)?;
    if remaining.is_empty() {
        let _ = fs::remove_file(folder.dir.join(RECOVERY_MANIFEST));
        let _ = fs::remove_dir(&folder.dir);
    }
    Ok((folder.stamp, report))
}

fn rewrite_manifest(dir: &Path, records: &[RecoveryRecord]) -> Result<()> {
    let text: String = records
        .iter()
        .map(|record| {
            format!(
                "{}\t{}\t{}\t{}{}\n",
                record.original.display(),
                record.stored.display(),
                record.sha256.as_deref().unwrap_or("-"),
                record.bytes,
                record
                    .replacement_sha256
                    .as_ref()
                    .map(|digest| format!("\t{digest}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    let tmp = dir.join(format!("{RECOVERY_MANIFEST}.tmp"));
    fs::write(&tmp, text).context("failed to write the recovery manifest")?;
    fs::rename(&tmp, dir.join(RECOVERY_MANIFEST)).context("failed to replace the recovery manifest")
}

pub fn prune_expired(home: &Path) -> Vec<(String, u64)> {
    let now = now_unix();
    let mut pruned = Vec::new();
    for folder in recovery_folders(home) {
        let Some(created) = folder.created_unix else {
            continue;
        };
        if now.saturating_sub(created) < RECOVERY_RETENTION_DAYS * 86_400 {
            continue;
        }
        let mut complete = true;
        for record in &folder.records {
            if !stored_inside_recovery(&folder, &record.stored) {
                complete = false;
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&record.stored) else {
                continue;
            };
            let removed = if meta.is_dir() {
                fs::remove_dir_all(&record.stored)
            } else {
                fs::remove_file(&record.stored)
            };
            complete &= removed.is_ok();
            if let Some(parent) = record.stored.parent()
                && !parent.starts_with(&folder.dir)
            {
                let _ = fs::remove_dir(parent);
                if let Some(volume) = parent.parent() {
                    let _ = fs::remove_dir(volume);
                }
            }
        }
        if complete {
            let _ = fs::remove_file(folder.dir.join(RECOVERY_MANIFEST));
            if fs::remove_dir(&folder.dir).is_ok() {
                pruned.push((folder.stamp.clone(), folder.bytes()));
            }
        }
    }
    pruned
}

pub fn adopt_receipt(directory: &Path, entries: &[(String, String)]) -> Result<bool> {
    let receipt = directory.join(RECEIPT_FILE);
    if fs::symlink_metadata(&receipt).is_ok() {
        return Ok(false);
    }
    if !directory_is_private(directory) {
        bail!(
            "{} is writable by group or others, so a receipt there could not be trusted by `codewhale uninstall`; run `chmod go-w {}` first",
            directory.display(),
            directory.display()
        );
    }
    if entries.is_empty() {
        bail!("no commands to record in a receipt");
    }
    let mut seen = BTreeSet::new();
    let mut text = String::new();
    for (name, digest) in entries {
        if !seen.insert(name.clone()) {
            continue;
        }
        let path = directory.join(name);
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file() || &file_sha256(&path)? != digest {
            bail!("{} changed while writing its receipt", path.display());
        }
        text.push_str(&format!("{digest}  {name}\n"));
    }
    let tmp = directory.join(format!(".codewhale-receipt.{}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    let published = written.and_then(|()| fs::hard_link(&tmp, &receipt));
    let _ = fs::remove_file(&tmp);
    match published {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to write {}", receipt.display())),
    }
}

pub fn write_file_atomically(dir: &Path, name: &str, bytes: &[u8], executable: bool) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    fs::rename(&tmp, dir.join(name))
        .with_context(|| format!("failed to write {}", dir.join(name).display()))
}

#[cfg(unix)]
#[must_use]
pub fn directory_is_private(directory: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(directory).is_ok_and(|meta| meta.mode() & 0o022 == 0)
}

#[cfg(not(unix))]
#[must_use]
pub fn directory_is_private(_directory: &Path) -> bool {
    true
}
