//! Plugin-state garbage collection (`/plugin doctor`).
//!
//! Every build materializes its own built-in snapshot and, because a plugin id
//! is bound to its root path, gets its own `state.json` record. Nothing ever
//! retired the old ones: a developer home collects a record and a snapshot per
//! build, plus inert workspace records whose workspace is long gone. This
//! module retires them without ever deciding on evidence it does not have.
//!
//! Safety rules, in the order they bind:
//!
//! * **Fail closed.** A state file that cannot be parsed aborts the run before
//!   anything is touched. The current build's snapshot must exist in the home
//!   being cleaned, or no built-in work is done at all.
//! * **Dry run first.** [`dry_run`] reads only. It reports what [`apply`] would
//!   do, split into the *automatic* subset that runs at startup and the
//!   *explicit* subset that needs `--fix`.
//! * **Records before directories.** State is rewritten atomically (private
//!   temp file, fsync, rename, directory fsync) under the registry's own state
//!   lock, after a one-deep `state.json.pre-gc` backup. Directories go second:
//!   a crash in between leaves unreferenced directories the next run removes,
//!   never a record pointing at a missing one.
//! * **Directories move before they die.** A directory is renamed to a
//!   `.retired-*` tombstone inside its own root, then deleted, so a partial
//!   delete never leaves a half-removed live-looking tree. Only direct
//!   children of the two Codewhale-owned roots are eligible, never links.
//! * **Trust is carried by the registry, not by this module.** A superseded
//!   built-in record is retired only once the current build has its own
//!   record (which the registry's carry-forward created, honouring the
//!   capability-hash rule), or it is not the record carry-forward would use.
//! * **Nothing in use goes.** The current build's snapshot, anything backing a
//!   loaded plugin, anything touched inside the grace window, and anything a
//!   running process names on its command line are kept. When the process
//!   list cannot be read, everything is assumed in use.
//! * **Never user-installed bundles.** Only records are ever pruned for user
//!   and workspace plugins, and only when the evidence is unambiguous.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{
    PersistedPluginState, PluginRegistry, PluginStateFile, builtin_predecessor,
    ensure_private_plugin_state_directory, harden_plugin_state_file, load_state,
    load_state_unlocked, metadata_is_link_or_reparse, open_state_lock, path_entry_exists,
    runtime_stage_key, save_state, save_state_with_hardener, state_lock_path,
};
use crate::plugins::builtin::{
    EmbeddedBundle, STAMP_NAME, embedded_bundles, parse_snapshot_dir_name, snapshots_dir,
};
use crate::plugins::discovery::{plugin_id, plugin_root_hash};
use crate::plugins::types::PluginScope;
use codewhale_config::PluginId;

/// How long a snapshot or stage must sit untouched before it may be removed.
/// Every start of a build re-stamps its snapshot, so this is "not started by
/// any binary for a day", which keeps parallel checkouts' builds safe.
pub(crate) const DEFAULT_GRACE: Duration = Duration::from_secs(24 * 60 * 60);
/// How long an inert (disabled, never trusted) record must be idle to be pruned.
pub(crate) const DEFAULT_INERT_STALE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

const BACKUP_NAME: &str = "state.json.pre-gc";
const LEFTOVER_PREFIXES: [&str; 2] = [".staging-", ".retired-"];

/// Which findings a run may act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GcTier {
    /// Superseded builds, inert records, leftovers: nothing that holds authority.
    Safe,
    /// Also records that still hold trust or enablement for a vanished bundle.
    Explicit,
}

#[derive(Debug, Clone)]
pub(crate) struct GcOptions {
    pub grace: Duration,
    pub inert_stale: Duration,
    pub now: SystemTime,
}

impl Default for GcOptions {
    fn default() -> Self {
        Self {
            grace: DEFAULT_GRACE,
            inert_stale: DEFAULT_INERT_STALE,
            now: SystemTime::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GcKind {
    Record,
    Snapshot,
    RuntimeStage,
    Leftover,
}

impl GcKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Snapshot => "snapshot",
            Self::RuntimeStage => "runtime stage",
            Self::Leftover => "leftover",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GcItem {
    pub kind: GcKind,
    /// Record id, or directory path.
    pub target: String,
    pub reason: String,
    pub bytes: u64,
    /// Needs `--fix`; the startup pass leaves it alone.
    pub explicit: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GcReport {
    pub items: Vec<GcItem>,
    /// Things deliberately left alone, with the reason.
    pub kept: Vec<String>,
    /// Environment facts that limited the run.
    pub notes: Vec<String>,
    pub applied: bool,
    pub failures: Vec<String>,
    pub records_before: usize,
    pub records_after: usize,
    pub state_bytes_before: u64,
    pub state_bytes_after: u64,
}

impl GcReport {
    pub(crate) fn is_clean(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn reclaimable_bytes(&self) -> u64 {
        self.items.iter().map(|item| item.bytes).sum()
    }
}

// ---------------------------------------------------------------------------
// Observation: everything read from disk, before any decision.
// ---------------------------------------------------------------------------

struct Layout {
    plugins_dir: PathBuf,
    snapshots: PathBuf,
    runtime_v2: PathBuf,
}

struct CurrentBuiltin {
    digest: String,
    id: String,
}

struct SnapshotDir {
    bundle: &'static str,
    path: PathBuf,
    id: Option<String>,
    is_current: bool,
    last_used: SystemTime,
}

struct StageDir {
    path: PathBuf,
    key: String,
    modified: SystemTime,
}

struct Leftover {
    path: PathBuf,
    root: PathBuf,
    modified: SystemTime,
}

#[derive(Default)]
struct Observed {
    layout: Option<Layout>,
    state_exists: bool,
    currents: BTreeMap<String, CurrentBuiltin>,
    snapshots: Vec<SnapshotDir>,
    stages: Vec<StageDir>,
    leftovers: Vec<Leftover>,
    /// Path hashes of every real directory under the user plugins root.
    /// `None` when the root could not be listed, which disables user pruning.
    user_hashes: Option<BTreeSet<String>>,
    notes: Vec<String>,
}

fn real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata_is_link_or_reparse(&metadata))
}

/// `<home>/plugins/state.json` is the only layout this module cleans around.
/// Any other state path still gets record pruning, but no directory is touched.
fn layout(state_path: &Path, resolved: &dyn Fn(&Path) -> Option<PathBuf>) -> Option<Layout> {
    if state_path.file_name()? != "state.json" {
        return None;
    }
    let plugins_dir = state_path.parent()?;
    if plugins_dir.file_name()? != "plugins" {
        return None;
    }
    let home = plugins_dir.parent().and_then(resolved)?;
    let plugins_dir = home.join("plugins");
    Some(Layout {
        snapshots: snapshots_dir(&home),
        runtime_v2: plugins_dir.join(".runtime").join("v2"),
        plugins_dir,
    })
}

fn modified(path: &Path) -> SystemTime {
    fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or_else(|_| SystemTime::now())
}

fn observe(
    state_path: &Path,
    bundles: &[EmbeddedBundle],
    resolved: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Observed {
    let mut obs = Observed {
        state_exists: path_entry_exists(state_path).unwrap_or(false),
        ..Observed::default()
    };
    let Some(layout) = layout(state_path, resolved) else {
        obs.notes.push(format!(
            "{} is not <home>/plugins/state.json; only records were examined, no directory is touched",
            state_path.display()
        ));
        return obs;
    };

    if real_dir(&layout.snapshots) {
        for bundle in bundles {
            let dir = layout.snapshots.join(bundle.snapshot_dir_name());
            if !real_dir(&dir) {
                continue;
            }
            if let Some(root) = resolved(&dir.join(bundle.name)) {
                obs.currents.insert(
                    bundle.name.to_string(),
                    CurrentBuiltin {
                        digest: bundle.digest.clone(),
                        id: plugin_id(PluginScope::Builtin, bundle.name, &root).0,
                    },
                );
            }
        }
        if obs.currents.is_empty() {
            obs.notes.push(
                "this build's built-in snapshot is not present in this home; built-in records and snapshots were not examined"
                    .to_string(),
            );
        }
        if let Ok(entries) = fs::read_dir(&layout.snapshots) {
            for entry in entries.flatten() {
                observe_snapshot_entry(&layout, bundles, &mut obs, &entry, resolved);
            }
        }
    } else if fs::symlink_metadata(&layout.snapshots).is_ok() {
        obs.notes.push(format!(
            "{} is not a real directory; left alone",
            layout.snapshots.display()
        ));
    }

    if real_dir(&layout.runtime_v2)
        && let Ok(entries) = fs::read_dir(&layout.runtime_v2)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !real_dir(&path) {
                continue;
            }
            if LEFTOVER_PREFIXES.iter().any(|p| name.starts_with(p)) {
                obs.leftovers.push(Leftover {
                    modified: modified(&path),
                    root: layout.runtime_v2.clone(),
                    path,
                });
            } else if is_sha256_hex(&name) {
                obs.stages.push(StageDir {
                    modified: modified(&path),
                    key: name,
                    path,
                });
            }
        }
    }

    if real_dir(&layout.plugins_dir) {
        obs.user_hashes = fs::read_dir(&layout.plugins_dir).ok().and_then(|entries| {
            let mut hashes = BTreeSet::new();
            for entry in entries {
                let path = entry.ok()?.path();
                if real_dir(&path) {
                    hashes.insert(plugin_root_hash(PluginScope::User, &resolved(&path)?));
                }
            }
            Some(hashes)
        });
    }
    obs.layout = Some(layout);
    obs
}

fn observe_snapshot_entry(
    layout: &Layout,
    bundles: &[EmbeddedBundle],
    obs: &mut Observed,
    entry: &fs::DirEntry,
    resolved: &dyn Fn(&Path) -> Option<PathBuf>,
) {
    let path = entry.path();
    let Some(name) = entry.file_name().to_str().map(str::to_string) else {
        return;
    };
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return;
    };
    if LEFTOVER_PREFIXES.iter().any(|p| name.starts_with(p)) {
        if metadata.is_dir() && !metadata_is_link_or_reparse(&metadata) {
            obs.leftovers.push(Leftover {
                modified: metadata.modified().unwrap_or_else(|_| SystemTime::now()),
                root: layout.snapshots.clone(),
                path,
            });
        }
        return;
    }
    let Some((bundle_name, digest)) = parse_snapshot_dir_name(&name, bundles) else {
        return;
    };
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
        obs.notes.push(format!(
            "ignored {name}: a snapshot name that is not a real directory"
        ));
        return;
    }
    let id = resolved(&path.join(bundle_name))
        .map(|root| plugin_id(PluginScope::Builtin, bundle_name, &root).0);
    let is_current = obs
        .currents
        .get(bundle_name)
        .is_some_and(|current| current.digest == digest);
    let stamp = path.join(STAMP_NAME);
    let last_used = fs::symlink_metadata(&stamp)
        .and_then(|metadata| metadata.modified())
        .unwrap_or_else(|_| modified(&path));
    obs.snapshots.push(SnapshotDir {
        bundle: bundle_name,
        path,
        id,
        is_current,
        last_used,
    });
}

fn is_sha256_hex(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Process list used to spot snapshots a running program still names.
/// Read at most once per run.
#[derive(Default)]
struct ProcessScan {
    output: OnceCell<Option<String>>,
}

impl ProcessScan {
    #[cfg(test)]
    fn fixed(text: &str) -> Self {
        Self {
            output: OnceCell::from(Some(text.to_string())),
        }
    }

    /// Unknown counts as in use: failing to look must never allow a delete.
    fn names(&self, needle: &str) -> bool {
        match self.output.get_or_init(capture_process_list) {
            Some(text) => text.contains(needle),
            None => true,
        }
    }

    fn uses(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| self.names(name))
    }
}

#[cfg(unix)]
fn capture_process_list() -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-axww", "-o", "command="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(unix))]
fn capture_process_list() -> Option<String> {
    // Windows refuses to delete files a process has open, which is the same
    // protection; there is no portable command-line scan to add.
    Some(String::new())
}

// ---------------------------------------------------------------------------
// Planning: pure decisions over (state, observation, options).
// ---------------------------------------------------------------------------

struct Retire {
    id: PluginId,
    reason: String,
}

struct DirItem {
    kind: GcKind,
    path: PathBuf,
    root: PathBuf,
    reason: String,
}

#[derive(Default)]
struct Plan {
    retire: Vec<Retire>,
    dirs: Vec<DirItem>,
    kept: Vec<String>,
}

fn split_id(id: &str) -> Option<(&str, &str, &str)> {
    let mut parts = id.splitn(3, '/');
    Some((parts.next()?, parts.next()?, parts.next()?))
}

fn is_inert(entry: &PersistedPluginState) -> bool {
    !entry.enabled && entry.trust.is_none()
}

/// The most recent time a record was reviewed, if it ever was.
fn last_activity(entry: &PersistedPluginState) -> Option<SystemTime> {
    entry
        .trust
        .iter()
        .chain(entry.review_history.iter())
        .filter_map(|receipt| chrono::DateTime::parse_from_rfc3339(&receipt.reviewed_at).ok())
        .map(SystemTime::from)
        .max()
}

fn age(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or_default()
}

fn short_age(duration: Duration) -> String {
    let days = duration.as_secs() / 86_400;
    if days > 0 {
        format!("{days}d")
    } else {
        format!("{}h", duration.as_secs() / 3_600)
    }
}

fn plan(
    state: &PluginStateFile,
    obs: &Observed,
    live: &BTreeSet<String>,
    opts: &GcOptions,
    tier: GcTier,
    in_use: &dyn Fn(&Path) -> bool,
) -> Plan {
    let mut plan = Plan::default();
    let mut retire: BTreeMap<PluginId, String> = BTreeMap::new();

    // Which superseded snapshots may go. A snapshot belongs to another build
    // when its digest is not the one this binary embeds.
    let mut removable: BTreeMap<PathBuf, String> = BTreeMap::new();
    // Only bundles whose current snapshot is present in this home are
    // examined: without it there is nothing to compare "superseded" against.
    for snapshot in obs
        .snapshots
        .iter()
        .filter(|s| !s.is_current && obs.currents.contains_key(s.bundle))
    {
        let name = snapshot
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if snapshot.id.as_ref().is_some_and(|id| live.contains(id)) {
            plan.kept
                .push(format!("snapshot {name}: backs a loaded plugin"));
        } else if age(opts.now, snapshot.last_used) < opts.grace {
            plan.kept.push(format!(
                "snapshot {name}: started {} ago, inside the grace window",
                short_age(age(opts.now, snapshot.last_used))
            ));
        } else if in_use(&snapshot.path) {
            plan.kept
                .push(format!("snapshot {name}: named by a running process"));
        } else {
            removable.insert(
                snapshot.path.clone(),
                format!(
                    "built by another binary; last started {} ago",
                    short_age(age(opts.now, snapshot.last_used))
                ),
            );
        }
    }

    for (id, entry) in &state.plugins {
        if live.contains(id.as_str()) {
            continue;
        }
        let Some((scope, hash, name)) = split_id(id.as_str()) else {
            continue;
        };
        let stale = || last_activity(entry).is_none_or(|t| age(opts.now, t) >= opts.inert_stale);
        let reason = match scope {
            "builtin" => {
                let Some(current) = obs.currents.get(name) else {
                    continue;
                };
                if current.id == id.as_str() {
                    continue;
                }
                let current_id = PluginId(current.id.clone());
                // Until the current build has its own record, the registry's
                // carry-forward still needs the predecessor it would pick.
                if !state.plugins.contains_key(&current_id)
                    && builtin_predecessor(state, &current_id, name)
                        .is_some_and(|source| std::ptr::eq(source, entry))
                {
                    plan.kept.push(format!(
                        "{id}: the newest review, kept until this build carries it forward"
                    ));
                    continue;
                }
                match obs
                    .snapshots
                    .iter()
                    .find(|s| s.id.as_deref() == Some(id.as_str()))
                {
                    Some(snapshot) => match removable.get(&snapshot.path) {
                        Some(why) => format!("superseded built-in; {why}"),
                        None => {
                            plan.kept
                                .push(format!("{id}: its snapshot is still in use or recent"));
                            continue;
                        }
                    },
                    None => match last_activity(entry) {
                        Some(t) if age(opts.now, t) < opts.grace => {
                            plan.kept
                                .push(format!("{id}: reviewed inside the grace window"));
                            continue;
                        }
                        None if entry.trust.is_some() => continue,
                        _ => "superseded built-in whose snapshot no longer exists".to_string(),
                    },
                }
            }
            "user" => {
                let Some(hashes) = &obs.user_hashes else {
                    continue;
                };
                if hashes.contains(hash) {
                    continue;
                }
                if is_inert(entry) {
                    if !stale() {
                        continue;
                    }
                    "inert record for a bundle directory that no longer exists".to_string()
                } else if tier >= GcTier::Explicit {
                    "holds trust or enablement for a bundle directory that no longer exists"
                        .to_string()
                } else {
                    continue;
                }
            }
            "workspace" => {
                if !is_inert(entry) || !stale() {
                    continue;
                }
                format!(
                    "inert (never trusted, disabled) and idle for {}",
                    last_activity(entry).map_or_else(
                        || "an unknown time".to_string(),
                        |t| short_age(age(opts.now, t))
                    )
                )
            }
            _ => continue,
        };
        retire.insert(id.clone(), reason);
    }

    for (path, reason) in removable {
        if let Some(layout) = &obs.layout {
            plan.dirs.push(DirItem {
                kind: GcKind::Snapshot,
                path,
                root: layout.snapshots.clone(),
                reason,
            });
        }
    }

    for leftover in &obs.leftovers {
        if age(opts.now, leftover.modified) >= opts.grace && !in_use(&leftover.path) {
            plan.dirs.push(DirItem {
                kind: GcKind::Leftover,
                path: leftover.path.clone(),
                root: leftover.root.clone(),
                reason: "abandoned staging or tombstone directory".to_string(),
            });
        }
    }

    // A runtime stage is referenced by the record whose id hashes to its key.
    // Only with a state file on disk: a missing file proves nothing, and must
    // not read as "every stage is an orphan".
    if obs.state_exists && obs.layout.is_some() {
        let referenced: BTreeSet<String> = state
            .plugins
            .keys()
            .filter(|id| !retire.contains_key(*id))
            .map(runtime_stage_key)
            .chain(
                live.iter()
                    .map(|id| runtime_stage_key(&PluginId(id.clone()))),
            )
            .collect();
        for stage in &obs.stages {
            if referenced.contains(&stage.key)
                || age(opts.now, stage.modified) < opts.grace
                || in_use(&stage.path)
            {
                continue;
            }
            if let Some(layout) = &obs.layout {
                plan.dirs.push(DirItem {
                    kind: GcKind::RuntimeStage,
                    path: stage.path.clone(),
                    root: layout.runtime_v2.clone(),
                    reason: "runtime snapshot of a plugin with no remaining record".to_string(),
                });
            }
        }
    }

    plan.retire = retire
        .into_iter()
        .map(|(id, reason)| Retire { id, reason })
        .collect();
    plan
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 200_000 {
                return total;
            }
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata_is_link_or_reparse(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

fn serialized_len(state: &PluginStateFile) -> u64 {
    serde_json::to_string_pretty(state).map_or(0, |body| body.len() as u64 + 1)
}

fn items_of(plan: &Plan, explicit: impl Fn(GcKind, &str) -> bool) -> Vec<GcItem> {
    let records = plan.retire.iter().map(|retire| {
        let target = retire.id.0.clone();
        GcItem {
            kind: GcKind::Record,
            explicit: explicit(GcKind::Record, &target),
            target,
            reason: retire.reason.clone(),
            bytes: 0,
        }
    });
    let dirs = plan.dirs.iter().map(|dir| {
        let target = dir.path.display().to_string();
        GcItem {
            kind: dir.kind,
            explicit: explicit(dir.kind, &target),
            bytes: dir_size(&dir.path),
            target,
            reason: dir.reason.clone(),
        }
    });
    records.chain(dirs).collect()
}

fn state_after(state: &PluginStateFile, plan: &Plan) -> PluginStateFile {
    let mut next = state.clone();
    for retire in &plan.retire {
        next.plugins.remove(&retire.id);
    }
    next
}

fn report_header(state: &PluginStateFile, obs: &Observed, plan: &Plan) -> GcReport {
    let after = state_after(state, plan);
    GcReport {
        kept: plan.kept.clone(),
        notes: obs.notes.clone(),
        records_before: state.plugins.len(),
        records_after: after.plugins.len(),
        state_bytes_before: serialized_len(state),
        state_bytes_after: serialized_len(&after),
        ..GcReport::default()
    }
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Read-only: what `apply(.., GcTier::Explicit)` would do, with the startup
/// subset distinguished from the `--fix`-only remainder.
pub(crate) fn dry_run(
    state_path: &Path,
    live: &BTreeSet<String>,
    opts: &GcOptions,
) -> Result<GcReport, String> {
    dry_run_with(
        state_path,
        live,
        opts,
        &embedded_bundles(),
        &ProcessScan::default(),
    )
}

fn dry_run_with(
    state_path: &Path,
    live: &BTreeSet<String>,
    opts: &GcOptions,
    bundles: &[EmbeddedBundle],
    scan: &ProcessScan,
) -> Result<GcReport, String> {
    let state = load_state(state_path)?;
    let resolved = resolved_paths(state_path, bundles);
    let lookup = |path: &Path| resolved.get(path).cloned();
    let obs = observe(state_path, bundles, &lookup);
    let in_use = |path: &Path| scan.uses(path);
    let all = plan(&state, &obs, live, opts, GcTier::Explicit, &in_use);
    let safe = plan(&state, &obs, live, opts, GcTier::Safe, &in_use);
    let safe_targets: BTreeSet<(GcKind, String)> = safe
        .retire
        .iter()
        .map(|retire| (GcKind::Record, retire.id.0.clone()))
        .chain(
            safe.dirs
                .iter()
                .map(|dir| (dir.kind, dir.path.display().to_string())),
        )
        .collect();
    let mut report = report_header(&state, &obs, &all);
    report.items = items_of(&all, |kind, target| {
        !safe_targets.contains(&(kind, target.to_string()))
    });
    Ok(report)
}

/// Apply every finding at or below `tier`.
pub(crate) fn apply(
    state_path: &Path,
    live: &BTreeSet<String>,
    opts: &GcOptions,
    tier: GcTier,
) -> Result<GcReport, String> {
    apply_with(
        state_path,
        live,
        opts,
        tier,
        &embedded_bundles(),
        &ProcessScan::default(),
    )
}

fn apply_with(
    state_path: &Path,
    live: &BTreeSet<String>,
    opts: &GcOptions,
    tier: GcTier,
    bundles: &[EmbeddedBundle],
    scan: &ProcessScan,
) -> Result<GcReport, String> {
    // A malformed or future-schema state file aborts here, before any change.
    let state = load_state(state_path)?;
    let resolved = resolved_paths(state_path, bundles);
    let lookup = |path: &Path| resolved.get(path).cloned();
    let obs = observe(state_path, bundles, &lookup);
    let in_use = |path: &Path| scan.uses(path);
    let mut chosen = plan(&state, &obs, live, opts, tier, &in_use);
    let mut report = report_header(&state, &obs, &chosen);

    if !chosen.retire.is_empty() {
        // Re-derive the record decisions under the registry's own state lock,
        // from the file as it is now, so a concurrent trust/enable is honoured.
        let lock_path = state_lock_path(state_path);
        if let Some(parent) = lock_path.parent() {
            ensure_private_plugin_state_directory(parent)?;
        }
        let lock_file = open_state_lock(&lock_path, true)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .map_err(|error| format!("failed to lock plugin state for cleanup: {error}"))?;
        let current = load_state_unlocked(state_path)?;
        chosen = plan(&current, &obs, live, opts, tier, &in_use);
        report = report_header(&current, &obs, &chosen);
        if !chosen.retire.is_empty() {
            let next = state_after(&current, &chosen);
            if next.plugins.len() + chosen.retire.len() != current.plugins.len() {
                return Err("plugin cleanup would remove an unexpected number of records".into());
            }
            let backup = state_path.with_file_name(BACKUP_NAME);
            save_state_with_hardener(&backup, &current, harden_plugin_state_file)?;
            save_state(state_path, &next)?;
        }
    }

    report.items = items_of(&chosen, |_, _| false);
    report.applied = true;
    for dir in &chosen.dirs {
        if let Err(error) = retire_dir(&dir.root, &dir.path) {
            report
                .failures
                .push(format!("{}: {error}", dir.path.display()));
        }
    }
    Ok(report)
}

/// Remove one direct child of a Codewhale-owned root. The child is first made
/// removable (staged runtime trees are deliberately read-only), renamed to a
/// tombstone inside the same root, then deleted. Links are never followed.
fn retire_dir(root: &Path, path: &Path) -> Result<(), String> {
    if path.parent() != Some(root) {
        return Err("not a direct child of its root".to_string());
    }
    if !real_dir(root) {
        return Err("root is not a real directory".to_string());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err("not a real directory; left alone".to_string());
    }
    make_tree_removable(path).map_err(|error| format!("could not unlock for removal: {error}"))?;
    let tombstone = root.join(format!(".retired-{}", uuid::Uuid::new_v4().simple()));
    fs::rename(path, &tombstone).map_err(|error| format!("could not retire: {error}"))?;
    fs::remove_dir_all(&tombstone).map_err(|error| {
        format!(
            "retired to {} but could not finish deleting: {error}",
            tombstone.display()
        )
    })
}

fn make_tree_removable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata_is_link_or_reparse(&metadata) {
        return Ok(());
    }
    if metadata.is_dir() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(not(unix))]
        {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
        }
        for entry in fs::read_dir(path)? {
            make_tree_removable(&entry?.path())?;
        }
    } else {
        #[cfg(not(unix))]
        {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

impl PluginRegistry {
    /// The automatic startup pass: the [`GcTier::Safe`] subset, at most once
    /// per process and state file, after built-in trust has been carried to
    /// this build. Never fatal: a failure is logged and startup continues.
    pub(crate) fn collect_garbage_at_startup(&self) {
        static DONE: std::sync::Mutex<BTreeSet<PathBuf>> = std::sync::Mutex::new(BTreeSet::new());
        let Some(state_path) = self.state_path() else {
            return;
        };
        if self.state_error.is_some() || !path_entry_exists(state_path).unwrap_or(false) {
            return;
        }
        let first = DONE
            .lock()
            .map(|mut done| done.insert(state_path.to_path_buf()))
            .unwrap_or(false);
        if !first {
            return;
        }
        let live = self.plugins.keys().map(|id| id.0.clone()).collect();
        match run(
            state_path.to_path_buf(),
            live,
            GcOptions::default(),
            Some(GcTier::Safe),
        ) {
            Ok(report) if !report.is_clean() => tracing::info!(
                target: "plugins",
                items = report.items.len(),
                bytes = report.reclaimable_bytes(),
                failures = report.failures.len(),
                "retired superseded plugin state; see /plugin doctor"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                target: "plugins",
                %error,
                "plugin state cleanup skipped"
            ),
        }
    }
}

/// The command and the startup pass. `tier` is `None` for the read-only
/// report; startup applies [`GcTier::Safe`] and `--fix` applies
/// [`GcTier::Explicit`]. Path identity is resolved inside the walk, off the
/// caller's thread.
pub(crate) fn run(
    state_path: PathBuf,
    live: BTreeSet<String>,
    opts: GcOptions,
    tier: Option<GcTier>,
) -> Result<GcReport, String> {
    let bundles = embedded_bundles();
    let scan = ProcessScan::default();
    match tier {
        Some(tier) => apply_with(&state_path, &live, &opts, tier, &bundles, &scan),
        None => dry_run_with(&state_path, &live, &opts, &bundles, &scan),
    }
}

/// Resolve the paths the walk identifies records by.
///
/// One blocking task resolves the home, lists that resolved home, and
/// resolves what the listing found. Listing the link and walking the target
/// would see none of the same snapshots, and the doctor would then refuse to
/// clean. A directory created after this returns is absent from the map, and
/// the walk keeps its record rather than retiring it for a hash it could not
/// compute.
fn resolved_paths(state_path: &Path, bundles: &[EmbeddedBundle]) -> BTreeMap<PathBuf, PathBuf> {
    let bundles = bundles.to_vec();
    let state_path = state_path.to_path_buf();
    // `spawn_blocking` needs a runtime, and startup and the tests have none.
    // Enter one first, then spawn. The brace stays on the `spawn_blocking`
    // line: that is the scope the ratchet treats as off the runtime.
    let run = async move {
        match tokio::task::spawn_blocking(move || {
            let mut resolved = BTreeMap::new();
            let Some(plugins_dir) = state_path.parent() else {
                return resolved;
            };
            if plugins_dir.file_name().is_none_or(|name| name != "plugins") {
                return resolved;
            }
            let Some(link) = plugins_dir.parent() else {
                return resolved;
            };
            let Ok(home) = link.canonicalize() else {
                return resolved;
            };
            // Both keys: the walk asks with the path it was given, and the
            // listing below uses the resolved one. They differ when the home
            // is a link.
            resolved.insert(link.to_path_buf(), home.clone());
            resolved.insert(home.clone(), home.clone());
            let mut paths = Vec::new();
            collect_children(&home, &bundles, &mut paths);
            for path in paths {
                if let Ok(real) = path.canonicalize() {
                    resolved.insert(path, real);
                }
            }
            resolved
        })
        .await
        {
            Ok(resolved) => resolved,
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        }
    };
    // Already on a worker: leave it before waiting on another runtime.
    match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::block_in_place(|| POOL.block_on(run)),
        Err(_) => POOL.block_on(run),
    }
}

/// Snapshot and user-plugin directories under an already-resolved home.
fn collect_children(home: &Path, bundles: &[EmbeddedBundle], paths: &mut Vec<PathBuf>) {
    let snapshots = snapshots_dir(home);
    if real_dir(&snapshots) {
        for bundle in bundles {
            let dir = snapshots.join(bundle.snapshot_dir_name());
            if real_dir(&dir) {
                paths.push(dir.join(bundle.name));
            }
        }
        if let Ok(entries) = fs::read_dir(&snapshots) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if LEFTOVER_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                {
                    continue;
                }
                let Some((bundle_name, _)) = parse_snapshot_dir_name(&name, bundles) else {
                    continue;
                };
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if !metadata_is_link_or_reparse(&metadata) && metadata.is_dir() {
                    paths.push(path.join(bundle_name));
                }
            }
        }
    }
    let plugins_dir = home.join("plugins");
    if real_dir(&plugins_dir)
        && let Ok(entries) = fs::read_dir(&plugins_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if real_dir(&path) {
                paths.push(path);
            }
        }
    }
}

static POOL: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .thread_name("plugin-doctor")
        .build()
        .expect("plugin doctor path resolver")
});

/// Human-readable size for reports.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= KIB * KIB {
        format!("{:.1} MB", value / (KIB * KIB))
    } else if value >= KIB {
        format!("{:.1} KB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
