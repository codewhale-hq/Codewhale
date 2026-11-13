//! Install identity and the constant half of the batch envelope.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::buffer;
use crate::event::{Arch, InstallAge, Libc, Os};

/// How long an install id may live before it is replaced.
///
/// A never-rotating id plus one batch per session from the user's IP is a
/// longitudinal IP and travel trace. Rotation bounds that join. It costs
/// longitudinal accuracy, and the docs say so in those words: **no count derived
/// from `install_id` is a user count.**
pub const ROTATION_DAYS: i64 = 90;

/// The on-disk install identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallId {
    /// Format version of this file.
    pub schema_version: u32,
    /// A random v4 UUID.
    ///
    /// Never derived from hostname, MAC, `machine-id`, `$HOME`, username, or
    /// executable path. A derived id is a device fingerprint: it survives
    /// reinstall and re-identifies a user across their own opt-out, which is the
    /// single thing an install id must never do.
    pub install_id: String,
    /// When this id was minted, RFC3339 UTC.
    pub rotated_at: String,
}

/// Per-machine telemetry bookkeeping. Never contains anything about the user's
/// work — only what this crate needs to avoid re-reporting an install and to
/// rate-limit its own flushes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryState {
    /// Format version of this file.
    #[serde(default)]
    pub schema_version: u32,
    /// The app version last seen on this machine.
    #[serde(default)]
    pub last_version: Option<String>,
    /// When a flush was last *attempted*, RFC3339 UTC. Attempt, not success, so
    /// a permanently offline machine tries at most once per interval.
    #[serde(default)]
    pub last_flush: Option<String>,
    /// The UTC day (`YYYY-MM-DD`) this state was first created. Local only: it
    /// is read to pick an [`InstallAge`] bucket and is never sent.
    #[serde(default)]
    pub first_seen_day: Option<String>,
    /// Set when this file predates `first_seen_day`, so its age is unknown and
    /// is reported as the oldest bucket rather than as new.
    #[serde(default)]
    pub first_seen_unknown: bool,
}

enum StateRead {
    Missing,
    Unreadable,
    Present(TelemetryState),
}

fn read_state_checked(root: &Path) -> StateRead {
    match std::fs::read_to_string(buffer::state_path(root)) {
        Ok(body) => serde_json::from_str::<TelemetryState>(&body)
            .map_or(StateRead::Unreadable, StateRead::Present),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => StateRead::Missing,
        Err(_) => StateRead::Unreadable,
    }
}

/// Today's date in UTC.
#[must_use]
pub fn utc_today() -> chrono::NaiveDate {
    chrono::Utc::now().date_naive()
}

/// Record the first-seen day if this state has none. A missing state file is a
/// new install; a file that has a recorded version but no day predates this
/// field and is marked unknown. An unreadable file is left untouched.
pub(crate) fn stamp_first_seen(root: &Path, today: chrono::NaiveDate) {
    let mut state = match read_state_checked(root) {
        StateRead::Unreadable => return,
        StateRead::Missing => TelemetryState::default(),
        StateRead::Present(state) => state,
    };
    if state.first_seen_day.is_some() || state.first_seen_unknown {
        return;
    }
    if state.last_version.is_some() {
        state.first_seen_unknown = true;
    } else {
        state.first_seen_day = Some(today.format("%Y-%m-%d").to_string());
    }
    let _ = write_state(root, &state);
}

/// The coarse age bucket for this installation, or `None` when the local state
/// cannot be read. A missing state is new.
pub(crate) fn install_age_at(root: &Path, today: chrono::NaiveDate) -> Option<InstallAge> {
    let state = match read_state_checked(root) {
        StateRead::Unreadable => return None,
        StateRead::Missing => return Some(InstallAge::NewToday),
        StateRead::Present(state) => state,
    };
    if state.first_seen_unknown {
        return Some(InstallAge::Older);
    }
    let Some(day) = state.first_seen_day else {
        return Some(InstallAge::NewToday);
    };
    let first = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok()?;
    Some(match today.signed_duration_since(first).num_days() {
        ..=0 => InstallAge::NewToday,
        1..=6 => InstallAge::UnderWeek,
        _ => InstallAge::Older,
    })
}

/// Read the install id, minting a fresh one if it is missing, unreadable,
/// **not a UUID**, or older than [`ROTATION_DAYS`].
///
/// The UUID check is not a formatting nicety. `install_id` is the one
/// envelope field read verbatim off disk into a batch, so without it the file
/// is a free-form string slot on the wire for anything that can write
/// `$CODEWHALE_HOME/telemetry/install_id.json`. Minting a fresh random id is
/// always the safe direction — the cost is one rotation, and the docs already
/// say no count derived from `install_id` is a user count.
pub fn read_or_create_install_id(root: &Path) -> Result<InstallId> {
    buffer::try_with_lock(root, || {
        if buffer::tombstone_present(root) {
            anyhow::bail!("telemetry is disabled");
        }
        let path = buffer::install_id_path(root);
        let existing = std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str::<InstallId>(&body).ok())
            .filter(|record| is_canonical_random_uuid(&record.install_id))
            .filter(|record| !is_expired(&record.rotated_at));
        if let Some(record) = existing {
            return Ok(record);
        }
        let record = InstallId {
            schema_version: 1,
            install_id: uuid::Uuid::new_v4().to_string(),
            rotated_at: now_rfc3339(),
        };
        codewhale_config::persistence::atomic_write_json(&path, &record)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(record)
    })?
    .ok_or_else(|| anyhow::anyhow!("telemetry privacy lock is held"))
}

/// Exactly what [`read_or_create_install_id`] mints: a random (v4, RFC 4122
/// variant) UUID in lowercase hyphenated form, byte for byte.
///
/// `Uuid::parse_str` alone also accepts surrounding whitespace after a trim,
/// braced/URN/simple spellings (each a different wire string for one id),
/// the nil UUID, and v1 ids, which embed a MAC address and a clock — a device
/// fingerprint, the one thing an install id must never be.
fn is_canonical_random_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| {
        id.get_version() == Some(uuid::Version::Random)
            && id.get_variant() == uuid::Variant::RFC4122
            && id.hyphenated().to_string() == value
    })
}

fn is_expired(rotated_at: &str) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(rotated_at) else {
        // An unreadable timestamp is treated as expired: minting a fresh random
        // id is always the safe direction.
        return true;
    };
    let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
    // A future `rotated_at` has a negative age that would never reach the
    // rotation window, pinning the id forever. It is expired, not fresh.
    age < chrono::TimeDelta::zero() || age.num_days() >= ROTATION_DAYS
}

/// Read `state.json`, or a default when it is missing or unreadable.
#[must_use]
pub fn read_state(root: &Path) -> TelemetryState {
    std::fs::read_to_string(buffer::state_path(root))
        .ok()
        .and_then(|body| serde_json::from_str::<TelemetryState>(&body).ok())
        .unwrap_or_default()
}

/// Write `state.json`.
pub fn write_state(root: &Path, state: &TelemetryState) -> Result<()> {
    buffer::try_with_lock(root, || {
        if buffer::tombstone_present(root) {
            anyhow::bail!("telemetry is disabled");
        }
        let path = buffer::state_path(root);
        codewhale_config::persistence::atomic_write_json(&path, state)
            .with_context(|| format!("failed to write {}", path.display()))
    })?
    .ok_or_else(|| anyhow::anyhow!("telemetry privacy lock is held"))
}

/// RFC3339 UTC at second precision. The only timestamp this crate produces, and
/// it is per-**batch**: individual events carry no timestamps at all.
#[must_use]
pub fn now_rfc3339() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .to_string()
}

/// The build sha of a release-CI binary, or `None`.
///
/// Sourced from `CODEWHALE_RELEASE_BUILD_SHA`, a rustc-env this crate's build
/// script emits **only** when `CODEWHALE_BUILD_SHA` or its legacy build-only
/// alias was present in the build environment (never the ambient
/// `GITHUB_SHA` every Actions job carries). `null` for
/// every locally built binary, unconditionally, with no runtime lookup of any
/// kind.
///
/// Never `CODEWHALE_BUILD_COMMIT` — that falls back to the builder's own `HEAD`
/// on a local build. Never `Thread.git_sha` — that is the *user's* workspace
/// commit and a red line, one identifier away by name.
#[must_use]
pub fn release_build_sha() -> Option<String> {
    option_env!("CODEWHALE_RELEASE_BUILD_SHA").and_then(short_hex_sha)
}

/// Reduce a full sha to the first 12 lowercase hex characters, rejecting
/// anything that is not a sha.
#[must_use]
pub fn short_hex_sha(value: &str) -> Option<String> {
    let trimmed = value.trim().to_ascii_lowercase();
    if trimmed.len() < 12 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(trimmed.chars().take(12).collect())
}

/// The OS family this binary is running on, mapped onto the closed whitelist.
#[must_use]
pub fn current_os() -> Os {
    match std::env::consts::OS {
        "linux" => Os::Linux,
        "macos" => Os::Macos,
        "windows" => Os::Windows,
        "freebsd" => Os::Freebsd,
        "android" => Os::Android,
        _ => Os::Other,
    }
}

/// The CPU family, mapped onto the closed whitelist.
#[must_use]
pub fn current_arch() -> Arch {
    match std::env::consts::ARCH {
        "x86_64" => Arch::X86_64,
        "aarch64" => Arch::Aarch64,
        _ => Arch::Other,
    }
}

/// The libc this binary was **compiled** against.
#[must_use]
pub fn current_libc() -> Libc {
    if cfg!(target_env = "gnu") {
        Libc::Gnu
    } else if cfg!(target_env = "musl") {
        Libc::Musl
    } else {
        Libc::None
    }
}

/// Whether both stdin and stdout are terminals.
///
/// This varies because consent is machine-scoped: a decision recorded on a TTY
/// authorizes later headless runs on the same home.
#[must_use]
pub fn current_tty() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Reduce a panic location to something that is safe to send.
///
/// Emit a `crates/…` path verbatim; reduce **everything else** to the literal
/// `<dep>`. There is no `--remap-path-prefix` in this repo, so a panic inside a
/// registry dependency yields
/// `/Users/<builder>/.cargo/registry/src/…/ratatui-0.29.0/src/…` — the build
/// machine's username, shipped from every user's binary.
/// The allowlist itself lives in [`crate::event::is_reduced_panic_site`], and
/// this function is defined as "the candidate if the predicate accepts it".
/// Two copies of one charset would drift, and the drain path re-checks the
/// predicate against events read back off disk — a reducer that could emit
/// something the checker rejects would silently delete real panics.
#[must_use]
pub fn reduce_panic_site(file: &str, line: u32, column: u32) -> String {
    let candidate = format!("{}:{line}:{column}", file.replace('\\', "/"));
    if crate::event::is_reduced_panic_site(&candidate) {
        candidate
    } else {
        "<dep>".to_string()
    }
}
