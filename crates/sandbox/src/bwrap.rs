//! Bubblewrap (bwrap) passthrough for Linux sandbox (#2184).
//!
//! Bubblewrap is a setuid-less container runtime used by Flatpak and other
//! projects. It creates a new mount namespace with configurable bind mounts,
//! providing filesystem isolation without requiring root privileges.
//!
//! # How it works
//!
//! When `/usr/bin/bwrap` actually works — it is executable AND it can create
//! its namespaces on this host — sandboxed exec_shell commands are routed
//! through it by default. `prefer_bwrap = false` in the config opts out and
//! leaves Linux commands unwrapped; the posture then reports policy-only.
//! The bwrap invocation looks like:
//!
//! ```text
//! bwrap \
//!   --unshare-all \
//!   --ro-bind / / \
//!   --dev /dev \
//!   --proc /proc \
//!   --tmpfs /tmp \
//!   --bind <writable-root> <writable-root> \
//!   --chdir <cwd> \
//!   -- <program> <args>
//! ```
//!
//! This creates a read-only view of the entire filesystem with write access
//! limited to the policy-derived writable roots. Policies that allow network
//! access add `--share-net` after `--unshare-all`. A private `/dev` and
//! `/proc` plus a tmpfs `/tmp` keep standard toolchain expectations working
//! (#5410); user-configured extra roots and device nodes append after them.
//! The read deny-list masks go last; exceptions inside a masked directory
//! (`SandboxManager::set_denied_read_exceptions`) are bound again after them.
//!
//! The extension host (`extension_host::supervisor`) launches through this
//! builder whenever bubblewrap works, without the `prefer_bwrap` opt-in; it
//! probes the finished command first and reports the host as unsandboxed,
//! with bwrap's own error, when it does not run.
//!
//! # Important
//!
//! We do NOT vendor bwrap. The user must install it themselves:
//!
//! - Ubuntu/Debian: `apt install bubblewrap`
//! - Fedora: `dnf install bubblewrap`
//! - Arch: `pacman -S bubblewrap`
//!
//! If bwrap is missing or cannot run (e.g. user namespaces are restricted,
//! as on Ubuntu 24.04 with `kernel.apparmor_restrict_unprivileged_userns`),
//! Codewhale reports no Linux OS sandbox and runs the command without an OS
//! wrapper. It never labels that fallback as sandboxed.

#[cfg(target_os = "linux")]
use super::policy::WritableRoot;
#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

/// Crate-visible wrapper over [`existing_directory`] so the sandbox module's
/// `BwrapMountExtensions::resolve` applies the same canonicalize + is_dir
/// rule to user-configured read-only roots (#5410).
#[cfg(target_os = "linux")]
pub(crate) fn existing_directory_shim(path: &Path) -> Option<PathBuf> {
    existing_directory(path)
}

/// Canonical path to the bubblewrap binary.
#[cfg(target_os = "linux")]
pub const BWRAP_PATH: &str = "/usr/bin/bwrap";

pub(super) fn spawn<T: Send + 'static>(
    launch: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<T> {
    use std::sync::{LazyLock, mpsc};
    type Launch = Box<dyn FnOnce() + Send>;
    static SPAWNER: LazyLock<std::io::Result<mpsc::Sender<Launch>>> = LazyLock::new(|| {
        let (sender, receiver) = mpsc::channel::<Launch>();
        std::thread::Builder::new()
            .name("codewhale-sandbox-spawner".into())
            .spawn(move || {
                for launch in receiver {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(launch));
                }
            })?;
        Ok(sender)
    });
    let spawner = SPAWNER
        .as_ref()
        .map_err(|error| std::io::Error::new(error.kind(), error.to_string()))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    spawner
        .send(Box::new(move || {
            let _ = sender.send(launch());
        }))
        .map_err(|_| std::io::Error::other("sandbox spawner stopped"))?;
    receiver
        .recv()
        .map_err(|_| std::io::Error::other("sandbox spawn did not return"))
}

/// How long the availability probe may take. A working bwrap runs a wrapped
/// `/bin/true` in well under a second, even on a loaded host.
#[cfg(target_os = "linux")]
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether bwrap is executable AND can actually confine a child here.
///
/// The exec bit alone lies on hosts where user namespaces are restricted:
/// bwrap starts but cannot create the sandbox, so every wrapped command
/// would fail instead of running unsandboxed. The check therefore runs a
/// real wrapped `/bin/true` once, built by [`build_bwrap_command`] itself so
/// the probe covers the same argument shape the real path uses, and caches
/// the verdict for the process.
#[cfg(target_os = "linux")]
pub fn is_available() -> bool {
    fn probe() -> bool {
        use wait_timeout::ChildExt as _;
        if !is_executable(std::path::Path::new(BWRAP_PATH)) {
            return false;
        }
        let command = build_bwrap_command(
            std::path::Path::new("/"),
            "/bin/true",
            &[],
            &[],
            false,
            &crate::BwrapMountExtensions::default(),
            &[],
            &[],
        );
        let Some((program, args)) = command.split_first() else {
            return false;
        };
        let mut child = match std::process::Command::new(program)
            .args(args)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return false,
        };
        match child.wait_timeout(PROBE_DEADLINE) {
            Ok(Some(status)) => status.success(),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                false
            }
        }
    }
    static VERDICT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VERDICT.get_or_init(probe)
}

#[cfg(target_os = "linux")]
pub(crate) fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(target_os = "linux"))]
pub fn is_available() -> bool {
    false
}

/// Build a bwrap command that wraps the given program and arguments.
///
/// The returned command vector is suitable for use as `ExecEnv.command` —
/// it replaces the normal program+args with a bwrap invocation that sets
/// up a read-only root filesystem with write access only to the specified
/// policy roots.
///
/// # Arguments
///
/// - `cwd` — working directory and sandbox chdir target
/// - `program` — the program to run inside the container
/// - `args` — arguments to pass to the program
/// - `writable_roots` — policy-derived directories to remount read-write
/// - `network_access` — whether to retain the caller's network namespace
/// - `extensions` — user-configured extra read-only roots and writable
///   device nodes (#5410); skipped when they do not exist on the host
///
/// # Returns
///
/// A `Vec<String>` representing the full bwrap invocation.
#[cfg(target_os = "linux")]
pub fn build_bwrap_command(
    cwd: &std::path::Path,
    program: &str,
    args: &[String],
    writable_roots: &[WritableRoot],
    network_access: bool,
    extensions: &crate::BwrapMountExtensions,
    denied_read_subpaths: &[std::path::PathBuf],
    denied_read_exceptions: &[std::path::PathBuf],
) -> Vec<String> {
    build_bwrap_command_with_read_paths(
        cwd,
        program,
        args,
        writable_roots,
        network_access,
        extensions,
        denied_read_subpaths,
        denied_read_exceptions,
        None,
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn build_bwrap_command_with_read_paths(
    cwd: &std::path::Path,
    program: &str,
    args: &[String],
    writable_roots: &[WritableRoot],
    network_access: bool,
    extensions: &crate::BwrapMountExtensions,
    denied_read_subpaths: &[std::path::PathBuf],
    denied_read_exceptions: &[std::path::PathBuf],
    readable_paths: Option<&[PathBuf]>,
) -> Vec<String> {
    let (writable_mounts, read_only_mounts) = safe_mounts(writable_roots);
    let (extra_read_only, device_mounts) = extensions.resolve();
    let mut cmd: Vec<String> =
        Vec::with_capacity(10 + args.len() + 3 * (writable_mounts.len() + read_only_mounts.len()));

    cmd.push(BWRAP_PATH.to_string());

    // Isolate every supported namespace by default. `--share-net` selectively
    // retains only the network namespace when the resolved policy allows it.
    cmd.push("--unshare-all".to_string());
    if network_access {
        cmd.push("--share-net".to_string());
    }
    // Tie the sandbox to the outer bwrap process: if it is killed (the TUI's
    // parent-death cleanup, a group kill), bwrap's PID-namespace init dies
    // too and takes every process in the sandbox with it (#6654).
    cmd.push("--die-with-parent".to_string());

    // Default commands read the host filesystem; Native hosts bind only their grants.
    if readable_paths.is_none() {
        cmd.push("--ro-bind".to_string());
        cmd.push("/".to_string());
        cmd.push("/".to_string());
    }

    // Standard container essentials (#5410): a private `/dev` (so device
    // nodes exist fresh and writable — `>/dev/null` under the read-only
    // root bind is EROFS without this), `/proc`, and a writable isolated
    // `/tmp` for toolchain scratch space.
    cmd.push("--dev".to_string());
    cmd.push("/dev".to_string());
    cmd.push("--proc".to_string());
    cmd.push("/proc".to_string());
    cmd.push("--tmpfs".to_string());
    cmd.push("/tmp".to_string());

    if let Some(paths) = readable_paths {
        for path in paths {
            cmd.push("--ro-bind".to_string());
            cmd.push(path.to_string_lossy().into_owned());
            cmd.push(path.to_string_lossy().into_owned());
        }
    }

    // User-configured writable device nodes (#5410), e.g. a host `/dev/null`
    // when the caller needs the host's, not the fresh private one.
    for device in device_mounts {
        let device = device.to_string_lossy().into_owned();
        cmd.push("--dev-bind".to_string());
        cmd.push(device.clone());
        cmd.push(device);
    }

    for root in &writable_mounts {
        let root = root.to_string_lossy().into_owned();
        cmd.push("--bind".to_string());
        cmd.push(root.clone());
        cmd.push(root);
    }

    // Re-apply protected descendants after all writable parents so a broad
    // writable root cannot make .codewhale/.deepseek exceptions writable.
    for root in read_only_mounts {
        let root = root.to_string_lossy().into_owned();
        cmd.push("--ro-bind".to_string());
        cmd.push(root.clone());
        cmd.push(root);
    }

    // User-configured extra read-only roots (#5410) apply last so they can
    // narrow (re-mount read-only over) any earlier writable bind if the
    // user explicitly lists a path the policy made writable.
    for root in extra_read_only {
        let root = root.to_string_lossy().into_owned();
        cmd.push("--ro-bind".to_string());
        cmd.push(root.clone());
        cmd.push(root);
    }

    // Opt-in read deny-list (S1, #5568), applied after every bind so nothing
    // re-exposes a denied path: an existing directory is masked with an empty
    // tmpfs, an existing file with a bind of /dev/null. Non-existent paths
    // are skipped — there is nothing to deny.
    for denied in denied_read_subpaths {
        cmd.extend(super::bwrap_mask_args(denied));
    }

    // Exceptions inside a masked directory (the extension host's own files
    // and plugin code under a Codewhale home it denies whole) are bound again
    // after the masks, then any denied path inside them is masked again.
    cmd.extend(super::bwrap_exception_args(
        denied_read_subpaths,
        denied_read_exceptions,
        &writable_mounts,
    ));

    // Change to the working directory inside the container.
    let cwd_str = cwd.to_string_lossy().to_string();
    cmd.push("--chdir".to_string());
    cmd.push(cwd_str);

    // Separator between bwrap args and the command to run.
    cmd.push("--".to_string());

    // The actual program and its arguments.
    cmd.push(program.to_string());
    cmd.extend(args.iter().cloned());

    cmd
}

#[cfg(target_os = "linux")]
fn safe_mounts(writable_roots: &[WritableRoot]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut writable = BTreeSet::new();
    let mut read_only = BTreeSet::new();

    for root in writable_roots {
        let Some(canonical_root) = safe_existing_directory(&root.root) else {
            continue;
        };
        writable.insert(canonical_root.clone());

        for exception in &root.read_only_subpaths {
            let Some(canonical_exception) = exception.canonicalize().ok() else {
                continue;
            };
            if canonical_exception.starts_with(&canonical_root) {
                read_only.insert(canonical_exception);
            }
        }
    }

    (
        writable.into_iter().collect(),
        read_only.into_iter().collect(),
    )
}

#[cfg(target_os = "linux")]
fn safe_existing_directory(path: &Path) -> Option<PathBuf> {
    let canonical = existing_directory(path)?;
    (canonical != Path::new("/")).then_some(canonical)
}

#[cfg(target_os = "linux")]
fn existing_directory(path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    canonical.is_dir().then_some(canonical)
}

/// Detect a failure attributable to the bubblewrap boundary.
#[cfg(target_os = "linux")]
pub fn detect_denial(exit_code: i32, stderr: &str) -> bool {
    exit_code != 0
        && (super::bwrap_setup_failure(stderr).is_some()
            || stderr.contains("Read-only file system"))
}

#[cfg(not(target_os = "linux"))]
pub fn detect_denial(_exit_code: i32, _stderr: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available_does_not_panic() {
        let _ = is_available();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_build_bwrap_command_structure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let cmd = build_bwrap_command(
            cwd,
            "sh",
            &["-c".to_string(), "echo hi".to_string()],
            &[WritableRoot::new(cwd.to_path_buf())],
            false,
            &crate::BwrapMountExtensions::default(),
            &[],
            &[],
        );

        // Should start with bwrap
        assert_eq!(cmd[0], "/usr/bin/bwrap");

        // Should have ro-bind for root
        assert!(cmd.contains(&"--ro-bind".to_string()));

        // Standard container essentials (#5410): private /dev, /proc, tmpfs /tmp.
        assert!(cmd.contains(&"--dev".to_string()));
        assert!(cmd.contains(&"--proc".to_string()));
        assert!(cmd.contains(&"--tmpfs".to_string()));

        // Should have --chdir
        assert!(cmd.contains(&"--chdir".to_string()));

        // Network stays isolated unless the policy explicitly allows it.
        assert!(cmd.contains(&"--unshare-all".to_string()));
        assert!(!cmd.contains(&"--share-net".to_string()));

        // The sandbox dies with the outer bwrap process (#6654).
        assert!(cmd.contains(&"--die-with-parent".to_string()));

        // Should end with the command
        assert_eq!(cmd[cmd.len() - 1], "echo hi");
        assert_eq!(cmd[cmd.len() - 2], "-c");
        assert_eq!(cmd[cmd.len() - 3], "sh");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn read_only_command_does_not_remount_the_working_directory_writable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let cmd = build_bwrap_command(
            cwd,
            "true",
            &[],
            &[],
            false,
            &crate::BwrapMountExtensions::default(),
            &[],
            &[],
        );

        assert!(!cmd.iter().any(|arg| arg == "--bind"));
        assert!(!cmd.iter().any(|arg| arg == "--share-net"));
        assert!(
            cmd.windows(2)
                .any(|args| args[0] == "--chdir" && args[1] == cwd.to_string_lossy())
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn workspace_write_mounts_every_safe_root_and_protects_read_only_descendants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let extra = dir.path().join("extra");
        let protected = workspace.join(".codewhale");
        std::fs::create_dir_all(&protected).expect("protected directory");
        std::fs::create_dir_all(&extra).expect("extra directory");

        let roots = vec![
            WritableRoot::with_exceptions(workspace.clone(), vec![protected.clone()]),
            WritableRoot::new(extra.clone()),
            WritableRoot::new(dir.path().join("missing")),
            WritableRoot::new(PathBuf::from("/")),
        ];
        let cmd = build_bwrap_command(
            &workspace,
            "true",
            &[],
            &roots,
            true,
            &crate::BwrapMountExtensions::default(),
            &[],
            &[],
        );

        for root in [&workspace, &extra] {
            let canonical = root.canonicalize().expect("canonical root");
            assert!(has_mount(&cmd, "--bind", &canonical));
        }
        assert!(has_mount(
            &cmd,
            "--ro-bind",
            &protected.canonicalize().expect("canonical protected path")
        ));
        assert!(!has_mount(&cmd, "--bind", Path::new("/")));
        assert!(!cmd.iter().any(|arg| arg.ends_with("/missing")));

        let unshare = cmd
            .iter()
            .position(|arg| arg == "--unshare-all")
            .expect("unshare all");
        let share = cmd
            .iter()
            .position(|arg| arg == "--share-net")
            .expect("share net");
        assert!(share > unshare);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn extensions_add_ro_roots_and_device_nodes_and_skip_invalid_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let extra_ro = dir.path().join("vendor-libs");
        std::fs::create_dir_all(&extra_ro).expect("extra ro dir");

        let extensions = crate::BwrapMountExtensions {
            read_only_roots: vec![
                extra_ro.clone(),
                dir.path().join("missing-ro"), // silently skipped
            ],
            device_roots: vec![
                // A real char device on every Linux host: /dev/null.
                PathBuf::from("/dev/null"),
                // Not a device: skipped even though it exists.
                extra_ro.clone(),
                // Missing: skipped.
                PathBuf::from("/dev/does-not-exist"),
            ],
        };
        let cmd = build_bwrap_command(cwd, "true", &[], &[], false, &extensions, &[], &[]);

        assert!(has_mount(
            &cmd,
            "--ro-bind",
            &extra_ro.canonicalize().expect("canonical extra"),
        ));
        assert!(has_mount(
            &cmd,
            "--dev-bind",
            &PathBuf::from("/dev/null")
                .canonicalize()
                .expect("canonical null")
        ));
        // The non-device directory must never appear as a dev-bind — the key
        // is not a writable-root escape hatch.
        assert!(!cmd.iter().any(|arg| arg.ends_with("/missing-ro")));
        let dev_binds_of_extra = cmd
            .windows(3)
            .any(|args| args[0] == "--dev-bind" && args[1].as_str() == extra_ro.to_string_lossy());
        assert!(!dev_binds_of_extra, "directories must not be dev-bound");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn extension_ro_roots_apply_after_writable_binds_so_they_can_narrow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let narrowed = workspace.join("vendor-libs");
        std::fs::create_dir_all(&narrowed).expect("dirs");

        let roots = vec![WritableRoot::new(workspace.clone())];
        let extensions = crate::BwrapMountExtensions {
            read_only_roots: vec![narrowed.clone()],
            device_roots: vec![],
        };
        let cmd = build_bwrap_command(
            &workspace,
            "true",
            &[],
            &roots,
            false,
            &extensions,
            &[],
            &[],
        );

        let writable_pos = cmd
            .windows(3)
            .position(|args| {
                args[0] == "--bind"
                    && args[1].as_str() == workspace.canonicalize().unwrap().to_string_lossy()
            })
            .expect("workspace writable bind");
        let narrow_pos = cmd
            .windows(3)
            .position(|args| {
                args[0] == "--ro-bind"
                    && args[1].as_str() == narrowed.canonicalize().unwrap().to_string_lossy()
            })
            .expect("narrowed ro bind");
        assert!(
            narrow_pos > writable_pos,
            "extra ro roots must apply after writable binds so they can narrow them"
        );
    }

    /// The extension host's form of the deny-list: its Codewhale home masked
    /// whole, then its own files bound again read-only and its data dir
    /// writable again, all after the mask; a missing exception is skipped.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_home_denied_whole_keeps_its_exceptions_after_the_mask() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("home/extension-host/data")).expect("data dir");
        std::fs::create_dir_all(dir.path().join("home/secrets")).expect("secrets dir");
        let home = dir
            .path()
            .join("home")
            .canonicalize()
            .expect("canonical home");
        let bundle = home.join("extension-host");
        let data = bundle.join("data");
        let cmd = build_bwrap_command(
            &data,
            "true",
            &[],
            &[WritableRoot::new(data.clone())],
            false,
            &crate::BwrapMountExtensions::default(),
            std::slice::from_ref(&home),
            &[bundle.clone(), home.join("plugins")],
        );
        let at = |flag: &str, path: &Path| -> Vec<usize> {
            cmd.windows(3)
                .enumerate()
                .filter(|(_, args)| args[0] == flag && args[1] == path.to_string_lossy())
                .map(|(index, _)| index)
                .collect()
        };
        let mask = at("--tmpfs", &home);
        let exception = at("--ro-bind", &bundle);
        let data_binds = at("--bind", &data);
        assert_eq!(mask.len(), 1, "{cmd:?}");
        assert_eq!(exception.len(), 1, "{cmd:?}");
        assert!(mask[0] < exception[0], "{cmd:?}");
        assert!(data_binds.last() > Some(&exception[0]), "{cmd:?}");
        assert!(!cmd.iter().any(|arg| arg.ends_with("/plugins")), "{cmd:?}");
    }

    #[cfg(target_os = "linux")]
    fn has_mount(command: &[String], flag: &str, path: &Path) -> bool {
        let path = path.to_string_lossy();
        command.windows(3).any(|args| {
            args[0] == flag
                && args[1].as_str() == path.as_ref()
                && args[2].as_str() == path.as_ref()
        })
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn executable_probe_requires_a_regular_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bwrap");
        std::fs::write(&path, b"fixture").expect("write fixture");
        assert!(!is_executable(&path));

        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("set executable bit");
        assert!(is_executable(&path));
        assert!(!is_executable(dir.path()));
    }

    #[test]
    fn denial_detection_requires_a_failed_sandbox_signal() {
        assert!(!detect_denial(0, "bwrap: ignored on success"));
        #[cfg(target_os = "linux")]
        {
            assert!(detect_denial(1, "bwrap: Creating new namespace failed"));
            assert!(detect_denial(1, "Read-only file system"));
            assert!(!detect_denial(1, "child output mentions bwrap: casually"));
            assert!(!detect_denial(1, "Permission denied"));
            assert!(!detect_denial(1, "Operation not permitted"));
            assert!(!detect_denial(1, "ordinary command failure"));
        }
    }
}
