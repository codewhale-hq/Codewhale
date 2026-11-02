//! Garbage-collection behavior. Every test builds a private plugin home; none
//! touches the developer's real one. Decisions are exercised through the same
//! `apply_with` / `dry_run_with` the command and the startup pass use, with the
//! process list and the "embedded" bundle injected.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::*;
use crate::manifest::PluginInventory;
use crate::registry::TrustReceipt;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const CURRENT: char = 'c';

fn hex64(fill: char) -> String {
    fill.to_string().repeat(64)
}

fn bundles() -> Vec<EmbeddedBundle> {
    vec![EmbeddedBundle {
        name: "computer-use",
        digest: hex64(CURRENT),
    }]
}

struct Home {
    _tmp: TempDir,
    home: PathBuf,
    state_path: PathBuf,
}

impl Home {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        fs::create_dir_all(&home).unwrap();
        ensure_private_plugin_state_directory(&home.join("plugins")).unwrap();
        let state_path = home.join("plugins/state.json");
        Self {
            _tmp: tmp,
            home,
            state_path,
        }
    }

    fn snapshots(&self) -> PathBuf {
        self.home.join("builtin-plugins/snapshots")
    }

    /// A snapshot directory for `digest`'s fill character, last started `days` ago.
    fn snapshot(&self, fill: char, age: Duration) -> PathBuf {
        let dir = self
            .snapshots()
            .join(format!("computer-use-{}", hex64(fill)));
        fs::create_dir_all(dir.join("computer-use")).unwrap();
        fs::write(dir.join("computer-use/plugin.json"), b"{}").unwrap();
        fs::write(dir.join(STAMP_NAME), hex64(fill)).unwrap();
        set_age(&dir.join(STAMP_NAME), age);
        set_age(&dir, age);
        dir
    }

    fn snapshot_id(&self, fill: char) -> String {
        let root = self
            .snapshots()
            .join(format!("computer-use-{}", hex64(fill)))
            .join("computer-use")
            .canonicalize()
            .unwrap();
        plugin_id(PluginScope::Builtin, "computer-use", &root).0
    }

    /// A runtime stage for `id`, hardened the way `trust` leaves it.
    fn stage(&self, id: &str, age: Duration) -> PathBuf {
        let key = runtime_stage_key(&PluginId(id.to_string()));
        let dir = self.home.join("plugins/.runtime/v2").join(key);
        let content = dir.join(hex64('9'));
        fs::create_dir_all(content.join("skills")).unwrap();
        fs::write(content.join("skills/SKILL.md"), b"body").unwrap();
        harden(&content);
        set_age(&dir, age);
        dir
    }

    fn write_state(&self, entries: Vec<(String, PersistedPluginState)>) {
        let mut state = PluginStateFile::default();
        state
            .plugins
            .extend(entries.into_iter().map(|(id, entry)| (PluginId(id), entry)));
        save_state(&self.state_path, &state).unwrap();
    }

    fn state(&self) -> PluginStateFile {
        load_state(&self.state_path).unwrap()
    }

    fn ids(&self) -> BTreeSet<String> {
        self.state().plugins.keys().map(|id| id.0.clone()).collect()
    }
}

impl Drop for Home {
    /// Staged runtime trees are read-only on purpose; let the tempdir remove them.
    fn drop(&mut self) {
        let _ = make_tree_removable(&self.home);
    }
}

#[cfg(unix)]
fn harden(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    for entry in fs::read_dir(path).unwrap().flatten() {
        if entry.path().is_dir() {
            harden(&entry.path());
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o500)).unwrap();
}
#[cfg(not(unix))]
fn harden(_: &Path) {}

fn set_age(path: &Path, age: Duration) {
    let then = SystemTime::now() - age;
    open_for_times(path).unwrap().set_modified(then).unwrap();
}

#[cfg(not(windows))]
fn open_for_times(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

/// Windows refuses `SetFileTime` on a read-only handle, and refuses to open a
/// directory at all without `FILE_FLAG_BACKUP_SEMANTICS`. Both failed every
/// age-dependent doctor test with "Access is denied" on hosted Windows.
#[cfg(windows)]
fn open_for_times(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

fn rfc3339_ago(age: Duration) -> String {
    chrono::DateTime::<chrono::Utc>::from(SystemTime::now() - age).to_rfc3339()
}

fn receipt(capability: &str, age: Duration) -> TrustReceipt {
    TrustReceipt {
        content_hash: "content".into(),
        capability_hash: capability.into(),
        reviewed_capabilities: PluginInventory::default(),
        reviewed_at: rfc3339_ago(age),
    }
}

/// Trusted (and optionally enabled) record reviewed `age` ago.
fn trusted(capability: &str, enabled: bool, age: Duration) -> PersistedPluginState {
    let receipt = receipt(capability, age);
    PersistedPluginState {
        generation: 1,
        enabled,
        trust: Some(receipt.clone()),
        review_history: vec![receipt],
    }
}

/// Never trusted, disabled, last reviewed `age` ago.
fn inert(age: Duration) -> PersistedPluginState {
    PersistedPluginState {
        generation: 4,
        enabled: false,
        trust: None,
        review_history: vec![receipt("cap", age)],
    }
}

fn opts() -> GcOptions {
    GcOptions::default()
}

fn nothing_running() -> ProcessScan {
    ProcessScan::fixed("")
}

fn live(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

fn apply_safe(h: &Home, live: &BTreeSet<String>) -> GcReport {
    apply_with(
        &h.state_path,
        live,
        &opts(),
        GcTier::Safe,
        &bundles(),
        &nothing_running(),
    )
    .unwrap()
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn tree_listing(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            out.push((entry.path().display().to_string(), meta.len()));
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn superseded_builds_are_retired_and_the_current_one_survives() {
    let h = Home::new();
    let current = h.snapshot(CURRENT, Duration::ZERO);
    let old = h.snapshot('a', 3 * DAY);
    let older = h.snapshot('b', 10 * DAY);
    let recent = h.snapshot('d', Duration::from_secs(3600));
    let (cur_id, old_id, older_id, recent_id) = (
        h.snapshot_id(CURRENT),
        h.snapshot_id('a'),
        h.snapshot_id('b'),
        h.snapshot_id('d'),
    );
    let orphan_id = "builtin/deadbeef0000/computer-use".to_string();
    h.write_state(vec![
        (cur_id.clone(), trusted("cap", true, Duration::ZERO)),
        (old_id.clone(), trusted("cap", true, 3 * DAY)),
        (older_id.clone(), trusted("cap", false, 10 * DAY)),
        (
            recent_id.clone(),
            trusted("cap", true, Duration::from_secs(3600)),
        ),
        (orphan_id.clone(), trusted("cap", true, 30 * DAY)),
    ]);
    let old_stage = h.stage(&old_id, 3 * DAY);
    let cur_stage = h.stage(&cur_id, Duration::ZERO);
    let before = fs::read(&h.state_path).unwrap();

    let report = apply_safe(&h, &live(&[&cur_id]));

    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(h.ids(), live(&[&cur_id, &recent_id]));
    assert!(exists(&current) && exists(&recent));
    assert!(!exists(&old) && !exists(&older), "superseded snapshots go");
    assert!(
        !exists(&old_stage),
        "the retired record's runtime stage goes"
    );
    assert!(exists(&cur_stage), "the current record's stage stays");
    assert_eq!((report.records_before, report.records_after), (5, 2));
    assert!(report.state_bytes_after < report.state_bytes_before);
    // The one-deep backup is the pre-cleanup state, byte for byte semantically.
    let backup: PluginStateFile =
        serde_json::from_slice(&fs::read(h.state_path.with_file_name(BACKUP_NAME)).unwrap())
            .unwrap();
    assert_eq!(backup.plugins.len(), 5);
    assert!(String::from_utf8(before).unwrap().contains(&orphan_id));
    // No temporary or tombstone debris is left behind.
    for dir in [h.snapshots(), h.home.join("plugins/.runtime/v2")] {
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".retired-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
    // A second pass has nothing left to do and does not rewrite the file.
    let stamp = fs::metadata(&h.state_path).unwrap().modified().unwrap();
    assert!(apply_safe(&h, &live(&[&cur_id])).items.is_empty());
    assert_eq!(
        fs::metadata(&h.state_path).unwrap().modified().unwrap(),
        stamp
    );
}

#[test]
fn dry_run_reports_without_touching_anything() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    h.snapshot('a', 3 * DAY);
    let (cur_id, old_id) = (h.snapshot_id(CURRENT), h.snapshot_id('a'));
    h.write_state(vec![
        (cur_id.clone(), trusted("cap", true, Duration::ZERO)),
        (old_id.clone(), trusted("cap", true, 3 * DAY)),
        (
            "user/aaaaaaaaaaaa/gone".into(),
            trusted("cap", true, 40 * DAY),
        ),
    ]);
    h.stage(&old_id, 3 * DAY);
    let state_before = fs::read(&h.state_path).unwrap();
    let home_before = tree_listing(&h.home);

    let report = dry_run_with(
        &h.state_path,
        &live(&[&cur_id]),
        &opts(),
        &bundles(),
        &nothing_running(),
    )
    .unwrap();

    assert!(!report.applied);
    let by_target = |needle: &str| {
        report
            .items
            .iter()
            .find(|item| item.target.contains(needle))
            .unwrap_or_else(|| panic!("no item for {needle}: {:?}", report.items))
    };
    assert!(
        !by_target(&old_id).explicit,
        "superseded builds run at startup"
    );
    assert!(
        by_target("user/aaaaaaaaaaaa/gone").explicit,
        "authority needs --fix"
    );
    assert!(report.items.iter().any(|i| i.kind == GcKind::Snapshot));
    assert!(report.items.iter().any(|i| i.kind == GcKind::RuntimeStage));
    assert_eq!(fs::read(&h.state_path).unwrap(), state_before);
    assert_eq!(tree_listing(&h.home), home_before);
}

#[test]
fn recent_or_running_snapshots_and_their_records_are_kept() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    let busy = h.snapshot('a', 5 * DAY);
    let loaded = h.snapshot('b', 5 * DAY);
    let gone = h.snapshot('d', 5 * DAY);
    let (cur_id, busy_id, loaded_id, gone_id) = (
        h.snapshot_id(CURRENT),
        h.snapshot_id('a'),
        h.snapshot_id('b'),
        h.snapshot_id('d'),
    );
    h.write_state(vec![
        (cur_id.clone(), trusted("cap", true, Duration::ZERO)),
        (busy_id.clone(), trusted("cap", true, 5 * DAY)),
        (loaded_id.clone(), trusted("cap", true, 5 * DAY)),
        (gone_id.clone(), trusted("cap", true, 5 * DAY)),
    ]);

    let report = apply_with(
        &h.state_path,
        &live(&[&cur_id, &loaded_id]),
        &opts(),
        GcTier::Safe,
        &bundles(),
        &ProcessScan::fixed(&format!(
            "/usr/bin/node {}/computer-use/mcp/server.mjs\n",
            busy.display()
        )),
    )
    .unwrap();

    assert!(
        exists(&busy),
        "a snapshot a running process names is in use"
    );
    assert!(
        exists(&loaded),
        "a snapshot behind a loaded plugin is in use"
    );
    assert!(!exists(&gone));
    assert_eq!(h.ids(), live(&[&cur_id, &busy_id, &loaded_id]));
    assert!(report.kept.iter().any(|k| k.contains("running process")));
    assert!(report.kept.iter().any(|k| k.contains("loaded plugin")));
}

#[test]
fn an_unreadable_process_list_assumes_everything_is_in_use() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    let old = h.snapshot('a', 5 * DAY);
    let (cur_id, old_id) = (h.snapshot_id(CURRENT), h.snapshot_id('a'));
    h.write_state(vec![
        (cur_id.clone(), trusted("cap", true, Duration::ZERO)),
        (old_id.clone(), trusted("cap", true, 5 * DAY)),
    ]);
    let unknown = ProcessScan {
        output: OnceCell::from(None),
    };
    let report = apply_with(
        &h.state_path,
        &live(&[&cur_id]),
        &opts(),
        GcTier::Safe,
        &bundles(),
        &unknown,
    )
    .unwrap();
    assert!(exists(&old));
    assert!(h.ids().contains(&old_id));
    assert!(report.items.is_empty());
}

#[test]
fn the_carry_forward_source_is_kept_until_the_current_build_has_a_record() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    h.snapshot('a', 5 * DAY);
    h.snapshot('b', 9 * DAY);
    let (cur_id, newer_id, older_id) = (
        h.snapshot_id(CURRENT),
        h.snapshot_id('a'),
        h.snapshot_id('b'),
    );
    // The current build has no record yet: carry-forward has not run.
    h.write_state(vec![
        (newer_id.clone(), trusted("cap", true, 5 * DAY)),
        (older_id.clone(), trusted("cap", true, 9 * DAY)),
    ]);

    apply_safe(&h, &live(&[&cur_id]));
    assert_eq!(
        h.ids(),
        live(&[&newer_id]),
        "only the review carry-forward would use survives"
    );

    // Once the current build holds its own record, the source is expendable.
    let mut state = h.state();
    state.plugins.insert(
        PluginId(cur_id.clone()),
        trusted("cap", true, Duration::ZERO),
    );
    save_state(&h.state_path, &state).unwrap();
    apply_safe(&h, &live(&[&cur_id]));
    assert_eq!(h.ids(), live(&[&cur_id]));
}

#[test]
fn without_this_builds_snapshot_no_builtin_state_is_touched() {
    let h = Home::new();
    let old = h.snapshot('a', 30 * DAY);
    let old_id = h.snapshot_id('a');
    h.write_state(vec![(old_id.clone(), trusted("cap", true, 30 * DAY))]);
    let report = apply_safe(&h, &live(&[]));
    assert!(exists(&old));
    assert!(h.ids().contains(&old_id));
    assert!(report.items.is_empty());
    assert!(
        report.notes.iter().any(|n| n.contains("not present")),
        "{:?}",
        report.notes
    );
}

#[test]
fn records_that_hold_authority_are_retired_only_with_fix() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    let kept_dir = h.home.join("plugins/installed");
    fs::create_dir_all(&kept_dir).unwrap();
    let kept_id = format!(
        "user/{}/installed",
        plugin_root_hash(PluginScope::User, &kept_dir.canonicalize().unwrap())
    );
    let vanished = "user/aaaaaaaaaaaa/vanished".to_string();
    let inert_vanished = "user/bbbbbbbbbbbb/vanished-inert".to_string();
    h.write_state(vec![
        (kept_id.clone(), trusted("cap", true, 60 * DAY)),
        (vanished.clone(), trusted("cap", true, 60 * DAY)),
        (inert_vanished.clone(), inert(60 * DAY)),
    ]);

    apply_safe(&h, &live(&[]));
    assert_eq!(
        h.ids(),
        live(&[&kept_id, &vanished]),
        "startup drops only the inert orphan"
    );

    let report = apply_with(
        &h.state_path,
        &live(&[]),
        &opts(),
        GcTier::Explicit,
        &bundles(),
        &nothing_running(),
    )
    .unwrap();
    assert_eq!(h.ids(), live(&[&kept_id]), "--fix drops the trusted orphan");
    assert!(report.items.iter().any(|i| i.target == vanished));
    assert!(
        kept_dir.is_dir(),
        "a user-installed bundle is never deleted"
    );
}

#[test]
fn inert_workspace_records_age_out_but_authority_and_live_ones_stay() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    h.write_state(vec![
        ("workspace/111111111111/old-demo".into(), inert(30 * DAY)),
        ("workspace/222222222222/fresh-demo".into(), inert(2 * DAY)),
        (
            "workspace/333333333333/trusted-demo".into(),
            trusted("cap", true, 90 * DAY),
        ),
        ("workspace/444444444444/live-demo".into(), inert(90 * DAY)),
        (
            "workspace/555555555555/never-reviewed".into(),
            PersistedPluginState::default(),
        ),
    ]);
    apply_safe(&h, &live(&["workspace/444444444444/live-demo"]));
    assert_eq!(
        h.ids(),
        live(&[
            "workspace/222222222222/fresh-demo",
            "workspace/333333333333/trusted-demo",
            "workspace/444444444444/live-demo",
        ])
    );
}

#[test]
fn only_direct_children_of_the_owned_roots_are_ever_removed() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    let outside = h._tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("precious.txt"), b"keep").unwrap();
    #[cfg(unix)]
    {
        // Snapshot-shaped and tombstone-shaped names that are links.
        std::os::unix::fs::symlink(
            &outside,
            h.snapshots().join(format!("computer-use-{}", hex64('a'))),
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, h.snapshots().join(".retired-link")).unwrap();
    }
    // Unrelated, user-owned neighbours are not in any pattern.
    let stranger = h.snapshots().join("my-notes");
    fs::create_dir_all(&stranger).unwrap();
    set_age(&stranger, 90 * DAY);
    h.write_state(vec![]);

    let report = apply_safe(&h, &live(&[]));

    assert!(outside.join("precious.txt").exists());
    assert!(stranger.exists());
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    // The helper itself refuses what a hostile caller might hand it.
    assert!(retire_dir(&h.snapshots(), &outside).is_err(), "not a child");
    #[cfg(unix)]
    assert!(retire_dir(&h.snapshots(), &h.snapshots().join(".retired-link")).is_err());
    assert!(outside.join("precious.txt").exists());
}

#[test]
fn a_malformed_state_file_aborts_before_any_directory_is_touched() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    let old = h.snapshot('a', 30 * DAY);
    fs::write(&h.state_path, b"{ this is not json").unwrap();
    let err = apply_with(
        &h.state_path,
        &live(&[]),
        &opts(),
        GcTier::Explicit,
        &bundles(),
        &nothing_running(),
    )
    .unwrap_err();
    assert!(err.contains("parse"), "{err}");
    assert!(exists(&old));
    assert_eq!(fs::read(&h.state_path).unwrap(), b"{ this is not json");
    assert!(!h.state_path.with_file_name(BACKUP_NAME).exists());
}

#[test]
fn a_missing_state_file_does_not_make_every_runtime_stage_an_orphan() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    let stage = h.stage("user/cccccccccccc/precious", 30 * DAY);
    assert!(!h.state_path.exists());
    apply_safe(&h, &live(&[]));
    assert!(exists(&stage));
    assert!(
        !h.state_path.exists(),
        "cleanup never creates the state file"
    );
}

#[test]
fn orphaned_stages_and_abandoned_staging_dirs_go_after_the_grace_window() {
    let h = Home::new();
    h.snapshot(CURRENT, Duration::ZERO);
    let referenced = "user/dddddddddddd/kept".to_string();
    h.write_state(vec![(referenced.clone(), inert(Duration::ZERO))]);
    let kept_stage = h.stage(&referenced, 30 * DAY);
    let orphan = h.stage("user/eeeeeeeeeeee/orphan", 30 * DAY);
    let fresh_orphan = h.stage("user/ffffffffffff/just-staged", Duration::from_secs(60));
    let dead_staging = h.snapshots().join(".staging-computer-use-xyz");
    let live_staging = h.snapshots().join(".staging-computer-use-abc");
    for (dir, age) in [
        (&dead_staging, 3 * DAY),
        (&live_staging, Duration::from_secs(5)),
    ] {
        fs::create_dir_all(dir.join("computer-use")).unwrap();
        set_age(dir, age);
    }

    apply_safe(&h, &live(&[]));

    assert!(exists(&kept_stage) && exists(&fresh_orphan) && exists(&live_staging));
    assert!(!exists(&orphan) && !exists(&dead_staging));
}

#[test]
fn a_state_path_outside_the_standard_layout_gets_record_cleanup_only() {
    let tmp = tempfile::tempdir().unwrap();
    let state_path = tmp.path().join("state/plugin-state.json");
    let neighbour = tmp
        .path()
        .join("builtin-plugins/snapshots/computer-use-aaaa");
    fs::create_dir_all(&neighbour).unwrap();
    let mut state = PluginStateFile::default();
    state.plugins.insert(
        PluginId("workspace/111111111111/demo".into()),
        inert(40 * DAY),
    );
    save_state(&state_path, &state).unwrap();

    let report = apply_with(
        &state_path,
        &live(&[]),
        &opts(),
        GcTier::Safe,
        &bundles(),
        &nothing_running(),
    )
    .unwrap();

    assert_eq!(report.records_after, 0);
    assert!(neighbour.exists());
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("no directory is touched"))
    );
}

// --- end to end through the real registry: trust must come out intact -------

mod through_the_registry {
    use super::*;
    use crate::context::{HostEnvironment, PluginDiscoveryContext};
    use crate::discovery::DiscoveryConfig;
    use crate::types::PluginTrustStatus;
    use sha2::{Digest, Sha256};

    const MANIFEST: &[u8] = br#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"fixture","version":"1.0.0"}"#;
    const SKILL: &[u8] = b"---\nname: extra\ndescription: An added skill.\n---\nBody.\n";

    type Build = Vec<(&'static str, &'static [u8])>;

    fn digest(files: &[(&str, &[u8])]) -> String {
        let mut hasher = Sha256::new();
        for (relative, contents) in files {
            hasher.update((relative.len() as u64).to_le_bytes());
            hasher.update(relative.as_bytes());
            hasher.update((contents.len() as u64).to_le_bytes());
            hasher.update(contents);
        }
        crate::manifest::hex_digest(hasher.finalize())
    }

    fn write_bundle(root: &Path, name: &str, files: &[(&str, &[u8])]) -> std::io::Result<PathBuf> {
        let hash = digest(files);
        let destination = root.join(format!("{name}-{hash}"));
        if !destination.exists() {
            let stage = tempfile::tempdir_in(root)?;
            for (relative, contents) in files {
                let path = stage.path().join(name).join(relative);
                fs::create_dir_all(path.parent().unwrap())?;
                fs::write(path, contents)?;
            }
            fs::write(stage.path().join(STAMP_NAME), hash)?;
            crate::builtin::publish_snapshot(stage.path(), &destination)?;
        }
        Ok(destination)
    }

    fn same_caps(version: &'static [u8]) -> Build {
        vec![("plugin.json", MANIFEST), ("body.txt", version)]
    }
    fn more_caps(version: &'static [u8]) -> Build {
        vec![
            ("plugin.json", MANIFEST),
            ("body.txt", version),
            ("skills/extra/SKILL.md", SKILL),
        ]
    }

    struct Fixture {
        h: Home,
        workspace: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let h = Home::new();
            let workspace = h.home.join("workspace");
            fs::create_dir_all(&workspace).unwrap();
            fs::create_dir_all(h.snapshots()).unwrap();
            Self { h, workspace }
        }

        fn config(&self, root: PathBuf) -> DiscoveryConfig {
            DiscoveryConfig {
                workspace: self.workspace.clone(),
                user_plugins_dir: self.h.home.join("plugins"),
                workspace_plugins_dir: self.workspace.join(".codewhale/plugins"),
                builtin_plugin_dirs: vec![root],
                state_path: self.h.state_path.clone(),
            }
        }

        /// Publish a historical fixture, discover it, and carry its review.
        fn boot(&self, files: &Build) -> (PluginRegistry, EmbeddedBundle) {
            let root = write_bundle(&self.h.snapshots(), "fixture", files).unwrap();
            let context = PluginDiscoveryContext::from_config_and_environment(
                &self.config(root),
                HostEnvironment::default(),
            );
            let registry = (*context.registry_for_workspace(&self.workspace)).clone();
            let bundle = EmbeddedBundle {
                name: "fixture",
                digest: digest(files),
            };
            (registry, bundle)
        }

        fn age_snapshot(&self, files: &Build, age: Duration) {
            let dir = self
                .h
                .snapshots()
                .join(format!("fixture-{}", digest(files)));
            set_age(&dir.join(STAMP_NAME), age);
            set_age(&dir, age);
        }

        fn collect(&self, registry: &PluginRegistry, bundle: &EmbeddedBundle) -> GcReport {
            let live = registry.plugins.keys().map(|id| id.0.clone()).collect();
            apply_with(
                &self.h.state_path,
                &live,
                &opts(),
                GcTier::Safe,
                std::slice::from_ref(bundle),
                &nothing_running(),
            )
            .unwrap()
        }

        fn rediscover(&self, files: &Build) -> PluginRegistry {
            self.boot(files).0
        }
    }

    #[test]
    fn same_capabilities_keep_their_review_and_enablement_across_a_collection() {
        let f = Fixture::new();
        let (mut v1, _) = f.boot(&same_caps(b"v1"));
        v1.trust("fixture").unwrap();
        v1.enable("fixture").unwrap();
        let v1_id = v1.get("fixture").unwrap().id.clone();

        let (v2, bundle) = f.boot(&same_caps(b"v2"));
        assert!(
            v2.get("fixture").unwrap().active(),
            "carried by the registry"
        );
        let v2_id = v2.get("fixture").unwrap().id.clone();
        f.age_snapshot(&same_caps(b"v1"), 3 * DAY);

        let report = f.collect(&v2, &bundle);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(
            !h_ids(&f).contains(&v1_id.0),
            "the superseded record is retired"
        );
        assert!(h_ids(&f).contains(&v2_id.0));
        assert!(
            !f.h.snapshots()
                .join(format!("fixture-{}", digest(&same_caps(b"v1"))))
                .exists()
        );

        // A restart sees the same review, enabled, with no re-review.
        let restarted = f.rediscover(&same_caps(b"v2"));
        let plugin = restarted.get("fixture").unwrap();
        assert_eq!(plugin.trust_status, PluginTrustStatus::Trusted);
        assert!(plugin.active());
    }

    #[test]
    fn changed_capabilities_stay_unreviewed_after_their_predecessors_are_gone() {
        let f = Fixture::new();
        let (mut v1, _) = f.boot(&same_caps(b"v1"));
        v1.trust("fixture").unwrap();
        v1.enable("fixture").unwrap();
        let (_, _) = f.boot(&same_caps(b"v2"));
        f.age_snapshot(&same_caps(b"v1"), 4 * DAY);
        f.age_snapshot(&same_caps(b"v2"), 3 * DAY);

        // v3 adds a skill: carry-forward records the old receipt as is.
        let (v3, bundle) = f.boot(&more_caps(b"v3"));
        let plugin = v3.get("fixture").unwrap();
        assert_eq!(plugin.trust_status, PluginTrustStatus::CapabilitiesChanged);
        assert!(!plugin.enabled);
        let v3_id = plugin.id.0.clone();

        f.collect(&v3, &bundle);
        assert_eq!(h_ids(&f), live(&[&v3_id]), "v1 and v2 are both retired");

        let restarted = f.rediscover(&more_caps(b"v3"));
        let plugin = restarted.get("fixture").unwrap();
        assert_eq!(
            plugin.trust_status,
            PluginTrustStatus::CapabilitiesChanged,
            "collecting must not launder a capability change into trust"
        );
        assert!(!plugin.enabled && !plugin.active());
    }

    #[test]
    fn a_review_that_carry_forward_has_not_yet_consumed_is_never_collected() {
        let f = Fixture::new();
        let (mut v1, _) = f.boot(&same_caps(b"v1"));
        v1.trust("fixture").unwrap();
        v1.enable("fixture").unwrap();
        f.age_snapshot(&same_caps(b"v1"), 30 * DAY);

        // Discover v2 but do not carry (GC raced ahead of the registry).
        let root = write_bundle(&f.h.snapshots(), "fixture", &same_caps(b"v2")).unwrap();
        let uncarried = crate::discovery::discover_with_config(&f.config(root));
        let bundle = EmbeddedBundle {
            name: "fixture",
            digest: digest(&same_caps(b"v2")),
        };
        f.collect(&uncarried, &bundle);

        let (v2, _) = f.boot(&same_caps(b"v2"));
        assert!(
            v2.get("fixture").unwrap().active(),
            "trust survived and carried"
        );
    }

    fn h_ids(f: &Fixture) -> BTreeSet<String> {
        f.h.ids()
    }
}

/// Not a regression test: a rehearsal of the real startup path against a COPY
/// of a real home (`cp -Rp ~/.codewhale/{plugins,builtin-plugins} <copy>/`).
/// It discovers historical snapshots, prints what `/plugin doctor` would
/// report, and with
/// `CW_GC_REHEARSAL_APPLY=1` applies `--fix` to the copy and re-checks trust.
#[test]
#[ignore = "set CW_GC_REHEARSAL_HOME to a copy of a real home"]
fn rehearsal_against_a_copy_of_a_real_home() {
    use crate::context::PluginDiscoveryContext;

    let Some(home) = std::env::var_os("CW_GC_REHEARSAL_HOME").map(PathBuf::from) else {
        return;
    };
    if let Some(origin) = std::env::var_os("CW_GC_REHEARSAL_ORIGIN") {
        remap_copied_ids(Path::new(&origin), &home);
    }
    let _lock = codewhale_runtime::test_support::lock_test_env();
    let _home = codewhale_runtime::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
    let workspace = tempfile::tempdir().unwrap();
    let registry =
        PluginDiscoveryContext::capture_pre_dotenv().registry_for_workspace(workspace.path());
    let state_path = registry.state_path().unwrap().to_path_buf();
    assert!(
        state_path.starts_with(&home),
        "must operate on the copy only"
    );
    let live: BTreeSet<String> = registry.plugins.keys().map(|id| id.0.clone()).collect();
    let status = |registry: &PluginRegistry| {
        registry
            .get("computer-use")
            .map(|p| format!("{} enabled={}", p.trust_status.as_str(), p.enabled))
    };
    println!("live plugins: {live:?}");
    println!("computer-use before: {:?}", status(&registry));

    let report = dry_run(&state_path, &live, &opts()).unwrap();
    println!(
        "DRY RUN: {} records -> {} ({} -> {}), reclaim {} on disk",
        report.records_before,
        report.records_after,
        format_bytes(report.state_bytes_before),
        format_bytes(report.state_bytes_after),
        format_bytes(report.reclaimable_bytes())
    );
    for kind in [
        GcKind::Record,
        GcKind::Snapshot,
        GcKind::RuntimeStage,
        GcKind::Leftover,
    ] {
        let items: Vec<_> = report.items.iter().filter(|i| i.kind == kind).collect();
        let explicit = items.iter().filter(|i| i.explicit).count();
        println!(
            "  {:>13}: {} ({} need --fix)",
            kind.label(),
            items.len(),
            explicit
        );
        for item in items {
            println!("      {} :: {}", item.target, item.reason);
        }
    }
    for line in &report.kept {
        println!("  kept: {line}");
    }
    for note in &report.notes {
        println!("  note: {note}");
    }

    if std::env::var_os("CW_GC_REHEARSAL_APPLY").is_some() {
        let applied = apply(&state_path, &live, &opts(), GcTier::Explicit).unwrap();
        println!(
            "APPLIED to the copy: {} -> {} records, {} failures {:?}",
            applied.records_before,
            applied.records_after,
            applied.failures.len(),
            applied.failures
        );
        let after =
            PluginDiscoveryContext::capture_pre_dotenv().registry_for_workspace(workspace.path());
        println!("computer-use after:  {:?}", status(&after));
        assert_eq!(status(&registry), status(&after), "trust must be unchanged");
        let again = dry_run(&state_path, &live, &opts()).unwrap();
        assert!(again.items.is_empty(), "idempotent: {:?}", again.items);
    }
}

/// Plugin ids hash the bundle's absolute path, so a copied home at another
/// path would look entirely orphaned. Rewrite the copy's ids (and the runtime
/// stage directories keyed by them) from the origin's paths to the copy's, so
/// the rehearsal sees the same relationships the real home has. Workspace
/// records cannot be remapped (their paths are not recorded) and stay as-is,
/// exactly as in the real home.
fn remap_copied_ids(origin: &Path, copy: &Path) {
    let (origin, copy) = (origin.canonicalize().unwrap(), copy.canonicalize().unwrap());
    let state_path = copy.join("plugins/state.json");
    let mut state = load_state(&state_path).unwrap();
    let mut hashes: BTreeMap<(String, String), String> = BTreeMap::new();
    let dirs = |root: PathBuf| -> Vec<(PathBuf, String)> {
        fs::read_dir(root)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| (e.path(), e.file_name().to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default()
    };
    for (_, name) in dirs(copy.join("plugins")) {
        let (old, new) = (
            origin.join("plugins").join(&name),
            copy.join("plugins").join(&name),
        );
        if let (Ok(old), Ok(new)) = (old.canonicalize(), new.canonicalize()) {
            hashes.insert(
                ("user".into(), plugin_root_hash(PluginScope::User, &old)),
                plugin_root_hash(PluginScope::User, &new),
            );
        }
    }
    for (_, name) in dirs(copy.join("builtin-plugins/snapshots")) {
        let rel = Path::new("builtin-plugins/snapshots")
            .join(&name)
            .join("computer-use");
        if let (Ok(old), Ok(new)) = (
            origin.join(&rel).canonicalize(),
            copy.join(&rel).canonicalize(),
        ) {
            hashes.insert(
                (
                    "builtin".into(),
                    plugin_root_hash(PluginScope::Builtin, &old),
                ),
                plugin_root_hash(PluginScope::Builtin, &new),
            );
        }
    }
    let mut next = PluginStateFile::default();
    let stage_root = copy.join("plugins/.runtime/v2");
    for (id, entry) in std::mem::take(&mut state.plugins) {
        let renamed = split_id(id.as_str()).and_then(|(scope, hash, name)| {
            hashes
                .get(&(scope.to_string(), hash.to_string()))
                .map(|new| PluginId(format!("{scope}/{new}/{name}")))
        });
        if let Some(new_id) = &renamed {
            let (old_dir, new_dir) = (
                stage_root.join(runtime_stage_key(&id)),
                stage_root.join(runtime_stage_key(new_id)),
            );
            if old_dir.exists() && !new_dir.exists() {
                fs::rename(old_dir, new_dir).unwrap();
            }
        }
        next.plugins.insert(renamed.unwrap_or(id), entry);
    }
    save_state(&state_path, &next).unwrap();
}
