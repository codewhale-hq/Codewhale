//! First-party plugin bundles that ship inside the binary.
//!
//! [`super::discovery::DiscoveryConfig::builtin_plugin_dirs`] and
//! [`super::types::PluginScope::Builtin`] have existed since plugin discovery
//! landed, with no producer: every construction site passed an empty list, so
//! the in-repo `crates/tui/plugins/computer-use` bundle reached nobody who had
//! not cloned the repository. This module is that producer. It is not a second install
//! path — installed bundles still arrive through
//! [`super::install`], and discovery, trust, and enablement are unchanged.
//!
//! The bundle is embedded with `include_bytes!` (the same way locale packs and
//! the mobile client are embedded) and written under
//! `$CODEWHALE_HOME/builtin-plugins` on first run, so one binary carries it to
//! every distribution channel — npm, tarball, `cargo install`, brew — without
//! any of them learning about plugin files.
//! macOS builds also carry the native helper built from the vendored sources,
//! so operating the computer never requires a compiler or a separate app
//! installation. The helper targets macOS 13+; OS permissions are still user
//! controlled, and the MCP server uses the host's Node.js runtime.
//!
//! Two properties this must not lose:
//!
//! * **Materializing is not enabling.** A freshly written builtin bundle is
//!   `NeverReviewed` and disabled like any other, because
//!   [`super::registry`] enables only what the user's `state.json` says.
//!   Computer use can drive the desktop; it waits to be reviewed.
//! * **Each build keeps its own complete tree.** A unique private stage is
//!   published once under its embedded-content digest. Discovery receives
//!   only that snapshot root, so another binary cannot replace a live bundle.
//!   Old bundles are not migrated. Their review is: when a new build's
//!   bundle has the same capability hash, the prior review and enablement
//!   carry to its new id; otherwise it reports `capabilities-changed`
//!   ([`super::registry::PluginRegistry::carry_forward_builtin_trust`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use super::path_identity::metadata_is_link_or_reparse;

/// Directory under the Codewhale home containing built-in bundles.
/// Deliberately *not* inside `plugins/`: that root is scanned as
/// [`super::types::PluginScope::User`], and a bundle found twice is a
/// duplicate-root diagnostic rather than a plugin.
const BUILTIN_DIR_NAME: &str = "builtin-plugins";
const SNAPSHOTS_DIR_NAME: &str = "snapshots";

/// Publication marker, outside the plugin itself. It is checked along with
/// every embedded byte and directory entry, never used as proof by itself.
pub(crate) const STAMP_NAME: &str = ".stamp";

const COMPUTER_USE: &str = "computer-use";

macro_rules! bundle_file {
    ($relative:literal) => {
        (
            $relative,
            include_bytes!(concat!("../../plugins/computer-use/", $relative)),
        )
    };
}

/// The runtime tree of `crates/tui/plugins/computer-use`, relative path → contents.
///
/// Development-only files (`tests/`, `scripts/smoke.mjs`,
/// `README.md`) are deliberately absent. The package manifest, lockfile and
/// Docker context are runtime inputs for creating an isolated desktop.
const COMPUTER_USE_FILES: &[(&str, &[u8])] = &[
    bundle_file!("LICENSE"),
    bundle_file!("package.json"),
    bundle_file!("package-lock.json"),
    bundle_file!(".dockerignore"),
    bundle_file!("docker/Dockerfile"),
    bundle_file!("docker/entrypoint.sh"),
    bundle_file!("docker/agent-exec.sh"),
    bundle_file!("plugin.json"),
    bundle_file!("mcp.json"),
    bundle_file!("commands/computer.md"),
    bundle_file!("skills/computer-use/SKILL.md"),
    bundle_file!("skills/computer-use/references/operating-details.md"),
    bundle_file!("skills/computer-use/references/quick-reference.md"),
    bundle_file!("skills/computer-use/references/refusal-codes.md"),
    bundle_file!("skills/recording/SKILL.md"),
    bundle_file!("agent.mjs"),
    bundle_file!("app/daemon.mjs"),
    bundle_file!("app/background-check.mjs"),
    bundle_file!("app/install-macos.mjs"),
    bundle_file!("app/updates.mjs"),
    bundle_file!("mcp/server.mjs"),
    bundle_file!("src/app-handler.mjs"),
    bundle_file!("src/app-script-policy.mjs"),
    bundle_file!("src/app-socket.mjs"),
    bundle_file!("src/browser-cdp.mjs"),
    bundle_file!("src/consent.mjs"),
    bundle_file!("src/spawn.mjs"),
    bundle_file!("src/exec.mjs"),
    bundle_file!("src/lease.mjs"),
    bundle_file!("src/png-size.mjs"),
    bundle_file!("src/recordings.mjs"),
    bundle_file!("src/registry.mjs"),
    bundle_file!("src/remote-runtime.mjs"),
    bundle_file!("src/ssh-args.mjs"),
    bundle_file!("src/tools.mjs"),
    bundle_file!("src/trajectory.mjs"),
    bundle_file!("src/transport.mjs"),
    bundle_file!("src/backends/darwin.mjs"),
    bundle_file!("src/backends/darwin-accessibility.m"),
    bundle_file!("src/backends/darwin-recording.h"),
    bundle_file!("src/backends/darwin-ocr.h"),
    bundle_file!("src/backends/harmonyos.mjs"),
    bundle_file!("src/backends/linux.mjs"),
    bundle_file!("src/backends/win32.mjs"),
    #[cfg(target_os = "macos")]
    (
        "bin/darwin/accessibility",
        include_bytes!(concat!(env!("OUT_DIR"), "/computer-use-accessibility")),
    ),
];

/// Digest of one bundle's entire contents, including its file names, so a
/// renamed or removed file is as much a change as an edited one.
pub(crate) fn digest(files: &[(&str, &[u8])]) -> String {
    let mut hasher = Sha256::new();
    for (relative, contents) in files {
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(contents);
    }
    super::manifest::hex_digest(hasher.finalize())
}

/// A bundle this build embeds, identified by the digest of its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddedBundle {
    pub name: &'static str,
    pub digest: String,
}

impl EmbeddedBundle {
    /// Directory name of this build's snapshot under the snapshots root.
    pub(crate) fn snapshot_dir_name(&self) -> String {
        format!("{}-{}", self.name, self.digest)
    }
}

/// The bundles this build would materialize. Garbage collection compares
/// on-disk snapshots against this list to decide which belong to other builds.
pub(crate) fn embedded_bundles() -> Vec<EmbeddedBundle> {
    vec![EmbeddedBundle {
        name: COMPUTER_USE,
        digest: digest(COMPUTER_USE_FILES),
    }]
}

/// `<home>/builtin-plugins/snapshots`, the only place snapshots are published.
pub(crate) fn snapshots_dir(home: &Path) -> PathBuf {
    home.join(BUILTIN_DIR_NAME).join(SNAPSHOTS_DIR_NAME)
}

/// Split `<bundle>-<64 hex>` into the embedded bundle name and its digest.
/// Anything else in the snapshots root is not ours to classify.
pub(crate) fn parse_snapshot_dir_name(
    dir_name: &str,
    bundles: &[EmbeddedBundle],
) -> Option<(&'static str, String)> {
    bundles.iter().find_map(|bundle| {
        let digest = dir_name.strip_prefix(bundle.name)?.strip_prefix('-')?;
        (digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then(|| (bundle.name, digest.to_string()))
    })
}

/// Record that this build started with `snapshot`, so garbage collection can
/// tell a snapshot a recent binary still uses from one nobody has opened in
/// weeks. Best effort and mtime only: the stamp's bytes are never touched.
fn mark_snapshot_used(snapshot: &Path) {
    if let Ok(Some(stamp)) =
        super::registry::open_existing_regular_file(&snapshot.join(STAMP_NAME), true)
    {
        let _ = stamp.set_modified(std::time::SystemTime::now());
    }
}

/// Discovery roots holding the built-in bundles, writing them out if what is
/// on disk is absent. Existing snapshots must exactly match this build. An
/// empty list is the honest answer when materialization fails: discovery finds no
/// built-in plugin, rather than a broken one.
///
/// Deliberately not memoized. The result is derived from `$CODEWHALE_HOME`,
/// and caching a home-derived path process-wide would pin whichever caller ran
/// first — which is wrong the moment the home differs between callers, as it
/// does across tests in one process. Reuse verifies the full embedded tree;
/// a matching stamp cannot bless changed bytes or a redirected path.
#[must_use]
pub fn materialized_dirs() -> Vec<PathBuf> {
    match materialize() {
        Ok(Some(root)) => vec![root],
        Ok(None) => Vec::new(),
        Err(error) => {
            tracing::warn!(
                target: "plugins",
                %error,
                "built-in plugin bundles could not be written; they will not be discovered"
            );
            Vec::new()
        }
    }
}

/// `Ok(None)` when there is no Codewhale home to write into yet.
///
/// Startup runs this for *every* command, `doctor` and `setup status`
/// included, and those are contractually read-only: they must not bring a
/// home directory into existence as a side effect of inventorying plugins
/// (`crates/tui/tests/integration/diagnostic_read_only.rs`). Materializing
/// into an existing home only keeps that promise, and costs nothing in
/// practice — the home exists from the moment Codewhale is configured or run.
fn materialize() -> io::Result<Option<PathBuf>> {
    let home = materialization_home()?;
    materialize_at_home(&home)
}

/// Startup materializes into any existing home, so an unsealed test would
/// write the developer's real `~/.codewhale/builtin-plugins`; it gets a
/// private home instead.
#[cfg(test)]
fn materialization_home() -> io::Result<PathBuf> {
    match crate::test_support::unsealed_state_dir(".") {
        Some(home) => Ok(home),
        None => codewhale_config::codewhale_home().map_err(io::Error::other),
    }
}

#[cfg(not(test))]
fn materialization_home() -> io::Result<PathBuf> {
    codewhale_config::codewhale_home().map_err(io::Error::other)
}

fn materialize_at_home(home: &Path) -> io::Result<Option<PathBuf>> {
    // The user-selected home may be an alias (the shared home resolver retains
    // it verbatim). Resolve it once before appending any Codewhale-owned paths,
    // so retargeting the alias cannot redirect this snapshot's discovery root.
    // Descendant links must still be rejected, never canonicalized away.
    let home = match home.canonicalize() {
        Ok(home) => home,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match fs::symlink_metadata(&home) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_dir() || metadata_is_link_or_reparse(&metadata) => {
            return Err(invalid_bundle("Codewhale home must be a real directory"));
        }
        Ok(_) => {}
    }
    let root = home.join(BUILTIN_DIR_NAME);
    reject_symlink(&root)?;
    match fs::create_dir(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    reject_symlink(&root)?;
    // Keep the old mutable root intact for older binaries. New snapshots live
    // in an owner-only namespace that those binaries neither scan nor replace.
    let snapshots = root.join(SNAPSHOTS_DIR_NAME);
    reject_symlink(&snapshots)?;
    super::registry::ensure_private_plugin_state_directory(&snapshots).map_err(io::Error::other)?;
    write_bundle(&snapshots, COMPUTER_USE, COMPUTER_USE_FILES).map(Some)
}

/// Return a discovery root containing exactly this build's bundle. Publication
/// never replaces an existing entry, including an empty or damaged directory.
/// Concurrent publishers of identical bytes converge after verifying the winner;
/// different builds retain different source paths and therefore trust identities.
pub(crate) fn write_bundle(
    root: &Path,
    name: &str,
    files: &[(&str, &[u8])],
) -> io::Result<PathBuf> {
    reject_symlink(root)?;
    if !super::agent_plugin::is_standard_plugin_name(name) || files.is_empty() {
        return Err(invalid_bundle(
            "invalid embedded plugin name or empty bundle",
        ));
    }
    let want = digest(files);
    let destination = root.join(format!("{name}-{want}"));
    let mut expected = BTreeMap::from([(PathBuf::from(STAMP_NAME), want.as_bytes())]);
    for (relative, contents) in files {
        let path = Path::new(relative);
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || expected
                .insert(Path::new(name).join(path), *contents)
                .is_some()
        {
            return Err(invalid_bundle("invalid or duplicate embedded bundle path"));
        }
    }
    if snapshot_exists(&destination)? {
        verify_snapshot(&destination, &expected)?;
        mark_snapshot_used(&destination);
        return Ok(destination);
    }

    let staging = tempfile::Builder::new()
        .prefix(&format!(".staging-{name}-"))
        .tempdir_in(root)?;
    for (relative, contents) in files {
        let path = staging.path().join(name).join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, contents)?;
        #[cfg(unix)]
        if *relative == "bin/darwin/accessibility" {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
    }
    fs::write(staging.path().join(STAMP_NAME), &want)?;
    verify_snapshot(staging.path(), &expected)?;
    match publish_snapshot(staging.path(), &destination) {
        Ok(()) => {
            // Only this operation's private temporary directory is ever cleaned
            // up. Its old path no longer belongs to us after publication.
            let _ = staging.keep();
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            // A competing publisher won. Do not remove its directory or assume
            // it is complete merely because its name/stamp matches our digest.
        }
        Err(error) => return Err(error),
    }
    verify_snapshot(&destination, &expected)?;
    Ok(destination)
}

fn snapshot_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn invalid_bundle(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Verify only the bounded embedded inventory. An unexpected file, directory,
/// link, executable bit, missing byte or forged stamp rejects the whole snapshot.
fn verify_snapshot(root: &Path, files: &BTreeMap<PathBuf, &[u8]>) -> io::Result<()> {
    let mut directories = BTreeSet::from([PathBuf::new()]);
    for relative in files.keys() {
        directories.extend(relative.ancestors().skip(1).map(Path::to_path_buf));
    }
    for relative in &directories {
        // Joining the empty root marker adds a trailing separator on Unix;
        // lstat("link/") follows that link before inspecting its target.
        let directory = if relative.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(relative)
        };
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata_is_link_or_reparse(&metadata) {
            return Err(invalid_bundle(
                "built-in snapshot directory is not a real directory",
            ));
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let child = relative.join(entry.file_name());
            if !files.contains_key(&child) && !directories.contains(&child) {
                return Err(invalid_bundle(
                    "built-in snapshot contains unexpected content",
                ));
            }
        }
    }
    for (relative, contents) in files {
        let path = root.join(relative);
        let mut file = super::registry::open_existing_regular_file(&path, false)
            .map_err(io::Error::other)?
            .ok_or_else(|| invalid_bundle("built-in snapshot file is missing"))?;
        let metadata = file.metadata()?;
        if metadata.len() != contents.len() as u64 {
            return Err(invalid_bundle("built-in snapshot content changed"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let executable = relative.ends_with("bin/darwin/accessibility");
            if (metadata.permissions().mode() & 0o111 != 0) != executable {
                return Err(invalid_bundle(
                    "built-in snapshot executable permissions changed",
                ));
            }
        }
        let mut buffer = vec![0; 64 * 1024];
        for chunk in contents.chunks(buffer.len()) {
            file.read_exact(&mut buffer[..chunk.len()])?;
            if &buffer[..chunk.len()] != chunk {
                return Err(invalid_bundle("built-in snapshot content changed"));
            }
        }
        if file.read(&mut buffer[..1])? != 0 {
            return Err(invalid_bundle(
                "built-in snapshot content changed during verification",
            ));
        }
    }
    Ok(())
}

/// Atomic no-replace directory publication. A check followed by ordinary Unix
/// rename is insufficient: rename is allowed to replace an existing empty dir.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn publish_snapshot(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: the nul-terminated paths remain alive for the syscall. Exclusive
    // rename never follows/replaces the destination entry, even if it is a link.
    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        // Static musl may lack the libc wrapper; use the same kernel operation.
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(super) fn publish_snapshot(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    use windows::core::PCWSTR;

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both paths are nul-terminated and live through the call. Omitting
    // MOVEFILE_REPLACE_EXISTING preserves every existing destination entry.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| io::Error::last_os_error())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(super) fn publish_snapshot(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic built-in snapshot publication is unsupported on this platform",
    ))
}

/// Refuse to write through a symbolic link or reparse point, the same rule
/// [`super::discovery`] applies when it scans a plugin root.
fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata_is_link_or_reparse(&metadata) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "built-in plugin path may not be a symbolic link or reparse point: {}",
                path.display()
            ),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod snapshot_tests;

#[cfg(test)]
mod tests {
    use super::*;

    use crate::plugins::types::{PluginScope, PluginTrustStatus};

    /// The vendored tree and the embed list are two views of one bundle.
    /// This pins them together so a refreshed bundle can never leave a
    /// runtime file out of `COMPUTER_USE_FILES` — the materialized server
    /// would crash at import the first time it needed the missing module —
    /// and an embed entry can never outlive its file. Development-only
    /// files stay out of the binary by the documented policy on
    /// `COMPUTER_USE_FILES`.
    #[test]
    fn computer_use_embed_list_matches_the_vendored_runtime_tree() {
        const DEV_ONLY: &[&str] = &["README.md", "scripts/smoke.mjs"];

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/computer-use");
        let mut on_disk: Vec<String> = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let relative = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative.starts_with("tests/") || DEV_ONLY.contains(&relative.as_str()) {
                    continue;
                }
                on_disk.push(relative);
            }
        }
        on_disk.sort();

        let mut embedded: Vec<&str> = COMPUTER_USE_FILES
            .iter()
            .map(|(relative, _)| *relative)
            // Built from the vendored native sources, never committed as an artifact.
            .filter(|relative| *relative != "bin/darwin/accessibility")
            .collect();
        embedded.sort_unstable();

        let expected: Vec<&str> = on_disk.iter().map(String::as_str).collect();
        assert_eq!(
            embedded, expected,
            "COMPUTER_USE_FILES and crates/tui/plugins/computer-use disagree — sync the embed \
             list with the vendored runtime tree"
        );
    }

    #[test]
    fn computer_use_is_discovered_but_never_auto_enabled() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

        let registry = crate::plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(&workspace);
        let plugin = registry
            .get(COMPUTER_USE)
            .expect("the built-in computer-use bundle must be discovered");

        assert_eq!(plugin.scope, PluginScope::Builtin);
        // Computer use can drive the desktop. Shipping it is not consenting to
        // it: the user reviews and enables it like any other bundle.
        assert!(!plugin.enabled);
        assert_eq!(plugin.trust_status, PluginTrustStatus::NeverReviewed);
        assert!(!home.join("plugins/state.json").exists(), "read-only");
    }

    /// A stock macOS install has neither a cloned plugin nor clang. The
    /// reviewed runtime snapshot must carry an executable native helper.
    #[cfg(target_os = "macos")]
    #[test]
    fn reviewed_computer_use_carries_a_runnable_native_helper() {
        use std::os::unix::fs::PermissionsExt as _;

        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let mut registry = crate::plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(&workspace);
        let registry = std::sync::Arc::get_mut(&mut registry).unwrap();
        registry.trust(COMPUTER_USE).unwrap();
        registry.enable(COMPUTER_USE).unwrap();
        let plugin = registry.get(COMPUTER_USE).unwrap();
        let staged = plugin.staged_root.as_ref().unwrap();
        let helper = staged.join("bin/darwin/accessibility");
        assert_eq!(
            fs::metadata(&helper).unwrap().permissions().mode() & 0o777,
            0o500
        );
        let result = std::process::Command::new(&helper)
            .arg(r#"{"tool":"permissions","args":{}}"#)
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let reply: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert!(reply.get("trusted").is_some());
        let capabilities = std::process::Command::new(&helper)
            .arg(r#"{"tool":"input_capabilities","args":{}}"#)
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(capabilities.status.success());
        let capabilities: serde_json::Value = serde_json::from_slice(&capabilities.stdout).unwrap();
        assert_eq!(capabilities["background_focus_guard"], 1);
        // This refusal precedes app resolution and input; no desktop target
        // or Accessibility grant is needed to qualify the embedded guard.
        let refused = std::process::Command::new(&helper)
            .arg(r#"{"tool":"bg_key","args":{"foreground_input":false}}"#)
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("background_focus_required"));
    }

    #[test]
    fn build_snapshots_preserve_live_authority_and_require_independent_review() {
        use crate::plugins::discovery::{DiscoveryConfig, discover_with_config};
        use crate::plugins::registry::verify_plugin_authority;

        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&cache).unwrap();
        fs::create_dir(&workspace).unwrap();
        let first: &[(&str, &[u8])] = &[
            ("plugin.json", br#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"fixture","version":"1.0.0"}"#),
            ("body.txt", b"first bundle"),
        ];
        let second: &[(&str, &[u8])] = &[
            ("plugin.json", br#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"fixture","version":"1.0.0"}"#),
            ("body.txt", b"other bundle"),
        ];
        let mut config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: temp.path().join("plugins"),
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: vec![cache.clone()],
            state_path: temp.path().join("plugins/state.json"),
        };
        // An older binary's source and path-bound receipt survive the layout
        // transition. Neither is an authority for the new physical source.
        let legacy = cache.join("fixture");
        fs::create_dir(&legacy).unwrap();
        for (path, contents) in first {
            fs::write(legacy.join(path), contents).unwrap();
        }
        let mut old = discover_with_config(&config);
        old.trust("fixture").unwrap();
        old.enable("fixture").unwrap();
        let old_id = old.get("fixture").unwrap().id.clone();
        let old_authority = old.authority_for("fixture").unwrap();
        let old_state = fs::read(&config.state_path).unwrap();

        let first_root = write_bundle(&cache, "fixture", first).unwrap();
        config.builtin_plugin_dirs = vec![first_root.clone()];
        let mut current = discover_with_config(&config);
        let plugin = current.get("fixture").unwrap();
        assert_eq!(plugin.scope, PluginScope::Builtin);
        assert_ne!(plugin.id, old_id);
        assert_eq!(plugin.trust_status, PluginTrustStatus::NeverReviewed);
        assert!(!plugin.enabled);
        assert_eq!(fs::read(&config.state_path).unwrap(), old_state);
        verify_plugin_authority(&old_authority).unwrap();

        current.trust("fixture").unwrap();
        current.enable("fixture").unwrap();
        let first_id = current.get("fixture").unwrap().id.clone();
        let first_authority = current.authority_for("fixture").unwrap();
        let first_catalog = current.live_catalog_stamp();
        let state_before_materialization = fs::read(&config.state_path).unwrap();
        let second_root = write_bundle(&cache, "fixture", second).unwrap();
        assert_ne!(first_root, second_root);
        assert_eq!(write_bundle(&cache, "fixture", first).unwrap(), first_root);
        assert_eq!(
            fs::read(&config.state_path).unwrap(),
            state_before_materialization
        );
        assert_eq!(current.live_catalog_stamp(), first_catalog);
        verify_plugin_authority(&first_authority).unwrap();
        verify_plugin_authority(&old_authority).unwrap();

        // Rediscovery uses the process's frozen root even after another build
        // publishes next to it; same embedded bytes retain identity and trust.
        let reloaded = current.rediscover_for_workspace(&workspace);
        let plugin = reloaded.get("fixture").unwrap();
        assert_eq!(plugin.id, first_id);
        assert!(plugin.active());
        config.builtin_plugin_dirs = vec![second_root];
        let mut next = discover_with_config(&config);
        let plugin = next.get("fixture").unwrap();
        assert_ne!(plugin.id, first_id);
        assert_eq!(plugin.trust_status, PluginTrustStatus::NeverReviewed);
        assert!(!plugin.enabled);
        next.trust("fixture").unwrap();
        next.enable("fixture").unwrap();
        let next_authority = next.authority_for("fixture").unwrap();
        verify_plugin_authority(&first_authority).unwrap();
        verify_plugin_authority(&next_authority).unwrap();

        // A matching publisher stamp cannot launder a changed reviewed source.
        fs::write(first_root.join("fixture/body.txt"), b"other bundle").unwrap();
        assert!(write_bundle(&cache, "fixture", first).is_err());
        assert!(verify_plugin_authority(&first_authority).is_err());
        verify_plugin_authority(&next_authority).unwrap();
        verify_plugin_authority(&old_authority).unwrap();
        next.revoke_trust("fixture").unwrap();
        assert!(verify_plugin_authority(&next_authority).is_err());
    }

    #[test]
    fn an_upgrade_carries_builtin_review_unless_capabilities_change_or_trust_was_revoked() {
        use crate::plugins::context::{HostEnvironment, PluginDiscoveryContext};
        use crate::plugins::discovery::DiscoveryConfig;
        use crate::plugins::registry::verify_plugin_authority;

        const MANIFEST: &[u8] = br#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"fixture","version":"1.0.0"}"#;
        const SKILL: &[u8] = b"---\nname: extra\ndescription: An added skill.\n---\nBody.\n";
        let builds: [&[(&str, &[u8])]; 5] = [
            &[("plugin.json", MANIFEST), ("body.txt", b"v1")],
            &[("plugin.json", MANIFEST), ("body.txt", b"v2")],
            &[
                ("plugin.json", MANIFEST),
                ("body.txt", b"v3"),
                ("skills/extra/SKILL.md", SKILL),
            ],
            &[
                ("plugin.json", MANIFEST),
                ("body.txt", b"v4"),
                ("skills/extra/SKILL.md", SKILL),
            ],
            &[
                ("plugin.json", MANIFEST),
                ("body.txt", b"v5"),
                ("skills/extra/SKILL.md", SKILL),
            ],
        ];
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&cache).unwrap();
        fs::create_dir(&workspace).unwrap();
        let registry_for = |files: &[(&str, &[u8])]| {
            let config = DiscoveryConfig {
                workspace: workspace.clone(),
                user_plugins_dir: temp.path().join("plugins"),
                workspace_plugins_dir: workspace.join(".codewhale/plugins"),
                builtin_plugin_dirs: vec![write_bundle(&cache, "fixture", files).unwrap()],
                state_path: temp.path().join("plugins/state.json"),
            };
            let context = PluginDiscoveryContext::from_config_and_environment(
                &config,
                HostEnvironment::default(),
            );
            (*context.registry_for_workspace(&workspace)).clone()
        };

        // A first install has nothing to carry: it waits for review.
        let mut v1 = registry_for(builds[0]);
        let plugin = v1.get("fixture").unwrap();
        assert_eq!(plugin.trust_status, PluginTrustStatus::NeverReviewed);
        assert!(!plugin.enabled);
        v1.trust("fixture").unwrap();
        v1.enable("fixture").unwrap();
        let v1_id = v1.get("fixture").unwrap().id.clone();
        let v1_authority = v1.authority_for("fixture").unwrap();

        // New bytes, same capabilities: the review and enablement carry, the
        // new build is live, and the older build keeps its own authority.
        let v2 = registry_for(builds[1]);
        let plugin = v2.get("fixture").unwrap();
        assert_ne!(plugin.id, v1_id);
        assert_eq!(plugin.trust_status, PluginTrustStatus::Trusted);
        assert!(plugin.enabled);
        assert!(plugin.active());
        verify_plugin_authority(&v2.authority_for("fixture").unwrap()).unwrap();
        verify_plugin_authority(&v1_authority).unwrap();
        // Carrying is once per build: rediscovery changes nothing.
        let state = fs::read(temp.path().join("plugins/state.json")).unwrap();
        let again = registry_for(builds[1]);
        assert!(again.get("fixture").unwrap().active());
        assert_eq!(
            fs::read(temp.path().join("plugins/state.json")).unwrap(),
            state
        );

        // Changed capabilities never carry silently: review the changes.
        let v3 = registry_for(builds[2]);
        let plugin = v3.get("fixture").unwrap();
        assert_eq!(plugin.trust_status, PluginTrustStatus::CapabilitiesChanged);
        assert!(!plugin.enabled);

        // A revocation anywhere in the line blocks carrying.
        let mut v3 = v3;
        v3.revoke_trust("fixture").unwrap();
        let mut v4 = registry_for(builds[3]);
        let plugin = v4.get("fixture").unwrap();
        assert_eq!(plugin.trust_status, PluginTrustStatus::NeverReviewed);
        assert!(!plugin.enabled);

        // A revocation blocks only until the next review: once the user
        // reviews and enables a later build, upgrades carry that review again.
        v4.trust("fixture").unwrap();
        v4.enable("fixture").unwrap();
        let v5 = registry_for(builds[4]);
        let plugin = v5.get("fixture").unwrap();
        assert_eq!(plugin.trust_status, PluginTrustStatus::Trusted);
        assert!(plugin.active());
    }

    #[test]
    fn a_home_that_does_not_exist_yet_is_never_created() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("absent-home");
        let _guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

        // Read-only diagnostics run this on every startup; conjuring the home
        // here would break their contract.
        assert!(materialized_dirs().is_empty());
        assert!(!home.exists());
    }
}
