//! macOS Seatbelt (sandbox-exec) profile generation.
//!
//! Seatbelt is Apple's mandatory access control framework that uses the
//! Scheme-based policy language to define what system resources a process
//! can access. This module generates sandbox profiles dynamically based
//! on the configured `SandboxPolicy`.
//!
//! # How it works
//!
//! 1. We generate a Seatbelt policy string in the SBPL format
//! 2. We invoke `/usr/bin/sandbox-exec -p <policy>` to run the command
//! 3. The kernel enforces the policy, blocking unauthorized operations
//!
//! # References
//!
//! - Apple's sandbox(7) man page
//! - <https://reverse.put.as/wp-content/uploads/2011/09/Apple-Sandbox-Guide-v1.0.pdf>

// Note: cfg(target_os = "macos") is already applied at the module level in mod.rs

use super::policy::SandboxPolicy;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Path to the sandbox-exec binary on macOS.
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Base seatbelt policy that provides minimal process functionality.
///
/// This policy:
/// - Denies everything by default
/// - Allows process execution and forking
/// - Allows signals within the same sandbox
/// - Allows reading user preferences (needed by many tools)
/// - Allows basic process introspection
/// - Allows writing to /dev/null
/// - Allows reading sysctl values
/// - Allows POSIX semaphores and pseudo-TTY operations
const SEATBELT_BASE_POLICY: &str = r#"
(version 1)
(deny default)

; Core process operations
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))

; User preferences (needed by many CLI tools)
(allow user-preference-read)

; Consume only filesystem access tokens already granted by macOS
(allow file-read* (extension "com.apple.app-sandbox.read"))
(allow file-read* (extension "com.apple.app-sandbox.read-write"))

; Basic I/O to /dev/null
(allow file-write-data
  (require-all
    (path "/dev/null")
    (vnode-type CHARACTER-DEVICE)))

; System information
(allow sysctl-read)

; IPC primitives
(allow ipc-posix-sem)
(allow ipc-posix-shm-read*)
(allow ipc-posix-shm-write-create)
(allow ipc-posix-shm-write-data)
(allow ipc-posix-shm-write-unlink)

; Terminal support (essential for shell commands)
(allow pseudo-tty)
(allow file-read* file-write* file-ioctl (literal "/dev/ptmx"))
(allow file-read* file-write* file-ioctl (literal "/dev/tty"))
(allow file-read* file-write* file-ioctl (regex #"^/dev/ttys[0-9]+$"))

; macOS-specific device access
(allow file-read* (literal "/dev/urandom"))
(allow file-read* (literal "/dev/random"))
(allow file-ioctl (literal "/dev/dtracehelper"))

; Mach IPC (needed by many system services)
(allow mach-lookup)
"#;

/// Network access policy additions.
const SEATBELT_NETWORK_POLICY: &str = r"
; Network access
(allow network-outbound)
(allow network-inbound)
(allow system-socket)
(allow network-bind)
";

const SEATBELT_READ_ONLY_NETWORK_POLICY: &str = r#"
; Outbound IPv4 network and the system DNS resolver; no loopback, IPv6 or other local sockets
(allow network-outbound (remote ip))
(allow network-outbound (remote unix-socket (path-literal "/private/var/run/mDNSResponder")))
(allow system-socket)
(deny network-outbound (remote ip "localhost:*"))
(deny network-outbound (remote tcp6 "*:*"))
(deny network-outbound (remote udp6 "*:*"))
"#;

/// AppleEvents/LaunchServices allowances for the trusted (full-disk-write)
/// tier only (#4828).
///
/// `open`, `osascript`, and `launchctl` send AppleEvents and drive
/// LaunchServices; under `(deny default)` those calls die with exit -54.
/// The base policy already allows `mach-lookup` broadly, so the missing
/// operations are `appleevent-send` and the LaunchServices `lsopen`
/// operation. The launchservicesd/appleevents mach names are listed
/// explicitly so a future narrowing of the blanket `mach-lookup` rule
/// cannot silently break this tier.
///
/// Restrictive tiers (workspace-write, read-only) intentionally stay locked
/// down: AppleEvents automation can instruct other apps to act outside the
/// sandbox, which would defeat the write restrictions.
const SEATBELT_TRUSTED_AUTOMATION_POLICY: &str = r#"
; AppleEvents + LaunchServices (trusted full-access tier only)
(allow appleevent-send)
(allow lsopen)
(allow mach-lookup
  (global-name "com.apple.coreservices.launchservicesd")
  (global-name "com.apple.coreservices.appleevents"))
"#;

/// Check if sandbox-exec is available and permitted on this system.
pub fn is_available() -> bool {
    static SEATBELT_AVAILABLE: OnceLock<bool> = OnceLock::new();

    *SEATBELT_AVAILABLE.get_or_init(|| {
        if !Path::new(SANDBOX_EXEC_PATH).exists() {
            return false;
        }

        let output = Command::new(SANDBOX_EXEC_PATH)
            .args(["-p", "(version 1)(allow default)", "--", "/usr/bin/true"])
            .output();

        match output {
            Ok(result) => result.status.success(),
            Err(_) => false,
        }
    })
}

/// Create the command-line arguments for sandbox-exec.
///
/// Returns a Vec of arguments that should be prepended to the command.
/// The format is: `sandbox-exec -p <policy> -D KEY=VALUE ... -- <original command>`
pub fn create_seatbelt_args(
    command: Vec<String>,
    policy: &SandboxPolicy,
    sandbox_cwd: &Path,
    denied_read_subpaths: &[std::path::PathBuf],
) -> Vec<String> {
    let full_policy = generate_policy(policy, sandbox_cwd, denied_read_subpaths);
    let params = generate_params(policy, sandbox_cwd);

    let mut args = vec!["-p".to_string(), full_policy];

    // Add parameter definitions for variable substitution
    for (key, value) in params {
        args.push(format!("-D{}={}", key, value.to_string_lossy()));
    }

    // Separator between sandbox-exec args and the actual command
    args.push("--".to_string());
    args.extend(command);

    args
}

pub(crate) fn create_restricted_seatbelt_args(
    command: Vec<String>,
    sandbox_cwd: &Path,
    readable_paths: &[PathBuf],
    denied_read_subpaths: &[PathBuf],
) -> Vec<String> {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process-exec)\n(allow process-fork)\n(allow signal (target same-sandbox))\n(allow process-info* (target same-sandbox))\n(allow sysctl-read)\n(allow file-read* (literal \"/\"))\n(allow file-write-data (literal \"/dev/null\"))\n(allow file-ioctl (literal \"/dev/dtracehelper\"))\n",
    );
    let escape = |path: &Path| {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    };
    let mut ancestors = std::collections::BTreeSet::new();
    for path in readable_paths {
        let predicate = if path.is_dir() { "subpath" } else { "literal" };
        profile.push_str(&format!(
            "(allow file-read* ({predicate} \"{}\"))\n",
            escape(path)
        ));
        ancestors.extend(path.ancestors().skip(1).map(Path::to_path_buf));
    }
    for path in ancestors {
        profile.push_str(&format!(
            "(allow file-read* (literal \"{}\"))\n",
            escape(&path)
        ));
    }
    if let Ok(data) = sandbox_cwd.canonicalize() {
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            escape(&data)
        ));
    }
    for path in denied_read_subpaths {
        profile.push_str(&format!(
            "\n(deny file-read* (subpath \"{}\"))",
            escape(path)
        ));
    }
    let mut args = vec!["-p".to_string(), profile, "--".to_string()];
    args.extend(command);
    args
}

/// Generate the complete Seatbelt policy string for the given policy.
fn generate_policy(
    policy: &SandboxPolicy,
    cwd: &Path,
    denied_read_subpaths: &[std::path::PathBuf],
) -> String {
    let mut full_policy = SEATBELT_BASE_POLICY.to_string();

    // Base read grant. Emitted unconditionally, including when a deny-list is
    // in force: SBPL is last-match-wins, so the `(deny file-read* …)` rules
    // appended at the end of this profile override it for exactly the denied
    // subpaths and nothing else. Dropping the broad allow instead would make
    // every posture unable to read `/usr/bin`, and nothing would run.
    full_policy.push_str("\n; Full filesystem read access\n(allow file-read*)");

    // Add write access policy
    let file_write_policy = generate_write_policy(policy, cwd);
    if !file_write_policy.is_empty() {
        full_policy.push_str("\n\n; Write access policy\n");
        full_policy.push_str(&file_write_policy);
    }

    // Add network policy if enabled
    if policy.has_network_access() {
        full_policy.push('\n');
        if matches!(policy, SandboxPolicy::ReadOnly { .. }) {
            full_policy.push_str(SEATBELT_READ_ONLY_NETWORK_POLICY);
            for port in loopback_proxy_ports() {
                full_policy.push_str(&format!(
                    "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
                ));
            }
        } else {
            full_policy.push_str(SEATBELT_NETWORK_POLICY);
        }
    }

    // Trusted tier (#4828): full-disk-write policies also get AppleEvents +
    // LaunchServices so `open`/`osascript`/`launchctl` work when a
    // full-access policy is still routed through seatbelt (e.g. a forced
    // sandbox); in the normal flow danger-full-access bypasses the wrap
    // entirely via `should_sandbox()`.
    if policy.has_full_disk_write_access() {
        full_policy.push('\n');
        full_policy.push_str(SEATBELT_TRUSTED_AUTOMATION_POLICY);
    }

    // Darwin user cache: read always; write only when the policy allows any
    // write (same gate as cargo/npm). ReadOnly must not get a cache escape.
    full_policy.push_str("\n\n; Darwin user cache directory\n");
    full_policy.push_str(r#"(allow file-read* (subpath (param "DARWIN_USER_CACHE_DIR")))"#);
    if !matches!(policy, SandboxPolicy::ReadOnly { .. }) {
        full_policy.push('\n');
        full_policy.push_str(r#"(allow file-write* (subpath (param "DARWIN_USER_CACHE_DIR")))"#);
    }

    // Add common macOS directories that tools often need
    full_policy.push_str("\n\n; Common macOS directories\n");
    full_policy.push_str(r#"(allow file-read* (subpath "/usr/lib"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/usr/share"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/System/Library"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/Library/Preferences"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/private/var/db"))"#);

    // Cargo home (#558): cargo build/test/publish reach into ~/.cargo/registry
    // and ~/.cargo/git for crate metadata, downloaded tarballs, and unpacked
    // sources. Sandboxed workspace-write was previously rejecting these,
    // making `cargo publish` unrunnable from inside the TUI's shell tool.
    // Read access is always allowed; write access is granted whenever the
    // policy allows any write at all (the registry caches need to be
    // mutable for `cargo build` to populate them on a cache miss). Skipped
    // entirely when neither `CARGO_HOME` nor `HOME` is set — without one of
    // those we have no path to plumb into the policy params.
    if resolve_cargo_home().is_some() {
        full_policy.push_str("\n\n; Cargo home (~/.cargo) — registry/index/git caches\n");
        full_policy.push_str(r#"(allow file-read* (subpath (param "CARGO_HOME")))"#);
        if !matches!(policy, SandboxPolicy::ReadOnly { .. }) {
            full_policy.push('\n');
            full_policy.push_str(r#"(allow file-write* (subpath (param "CARGO_HOME_REGISTRY")))"#);
            full_policy.push('\n');
            full_policy.push_str(r#"(allow file-write* (subpath (param "CARGO_HOME_GIT")))"#);
        }
    }

    // npm cache (#1267): npx-based MCP servers write to ~/.npm when downloading
    // packages on first run. Without write access the npx subprocess fails
    // immediately with "Stdio transport closed", making all stdio MCP servers
    // broken on macOS under the default workspace-write policy.
    // Read access is always allowed; write access mirrors the cargo pattern —
    // granted for all policies that allow any write, skipped for ReadOnly.
    // Skipped entirely when neither `NPM_CONFIG_CACHE` nor `HOME` is set.
    if resolve_npm_cache_dir().is_some() {
        full_policy.push_str("\n\n; npm cache (~/.npm) — npx package downloads\n");
        full_policy.push_str(r#"(allow file-read* (subpath (param "NPM_CACHE_DIR")))"#);
        if !matches!(policy, SandboxPolicy::ReadOnly { .. }) {
            full_policy.push('\n');
            full_policy.push_str(r#"(allow file-write* (subpath (param "NPM_CACHE_DIR")))"#);
        }
    }

    // Opt-in read deny-list (S1, #5568). Appended LAST deliberately: SBPL is
    // last-match-wins, so these rules override every broad read allowance
    // above — including the full-disk `(allow file-read*)` — for the listed
    // subpaths. Metadata reads are denied too so the paths do not enumerate.
    // A read deny does not stop connect(2) to a socket under the path, so with
    // network on, unix-socket connects under each path are denied as well.
    if !denied_read_subpaths.is_empty() {
        full_policy.push_str("\n\n; Opt-in read deny-list (user-configured)\n");
        for path in denied_read_subpaths {
            let escaped = path
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            full_policy.push_str(&format!("(deny file-read* (subpath \"{escaped}\"))\n"));
            if policy.has_network_access() {
                full_policy.push_str(&format!(
                    "(deny network-outbound (remote unix-socket (subpath \"{escaped}\")))\n"
                ));
            }
        }
    }

    full_policy
}

/// Resolve the user's cargo home — `CARGO_HOME` if set, else `$HOME/.cargo`.
/// Returns `None` only on hosts where neither env var is set (essentially
/// never on a real macOS user account; can happen in CI containers without
/// `HOME` exported).
fn resolve_cargo_home() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("CARGO_HOME")
        && !explicit.trim().is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".cargo"))
}

/// Resolve the npm cache directory — `NPM_CONFIG_CACHE` if set, else `$HOME/.npm`.
/// Returns `None` only on hosts where neither env var is set.
fn resolve_npm_cache_dir() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("NPM_CONFIG_CACHE")
        && !explicit.trim().is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".npm"))
}

fn loopback_proxy_ports() -> Vec<u16> {
    let mut ports: Vec<u16> = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .filter_map(|value| {
        let value = value.trim();
        let url = if value.contains("://") {
            url::Url::parse(value)
        } else {
            url::Url::parse(&format!("http://{value}"))
        }
        .ok()?;
        let host = url.host_str()?;
        if host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]" {
            url.port_or_known_default()
        } else {
            None
        }
    })
    .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Generate the write access portion of the Seatbelt policy.
fn generate_write_policy(policy: &SandboxPolicy, cwd: &Path) -> String {
    // Full disk write access
    if policy.has_full_disk_write_access() {
        return r#"(allow file-write* (regex #"^/"))"#.to_string();
    }

    // Read-only - no write policy needed
    if matches!(policy, SandboxPolicy::ReadOnly { .. }) {
        return String::new();
    }

    // Workspace write - enumerate allowed paths
    let writable_roots = policy.get_writable_roots(cwd);
    if writable_roots.is_empty() {
        return String::new();
    }

    let mut policies = Vec::new();

    for (index, root) in writable_roots.iter().enumerate() {
        let root_param = format!("WRITABLE_ROOT_{index}");

        let mut root_parts = vec![format!("(subpath (param \"{root_param}\"))")];
        for (subpath_index, _) in root.read_only_subpaths.iter().enumerate() {
            let ro_param = format!("WRITABLE_ROOT_{index}_RO_{subpath_index}");
            root_parts.push(format!("(require-not (subpath (param \"{ro_param}\")))"));
        }

        let root_policy = if root_parts.len() == 1 {
            root_parts[0].clone()
        } else {
            format!("(require-all {})", root_parts.join(" "))
        };
        policies.push(root_policy);

        // File Provider paths can require an inherited macOS extension even
        // when their logical path is already an approved root. Keep Codewhale's
        // root and protected-subpath restrictions authoritative by requiring
        // the extension and every root predicate in the same conjunction.
        let mut extension_parts =
            vec![r#"(extension "com.apple.app-sandbox.read-write")"#.to_string()];
        extension_parts.extend(root_parts);
        policies.push(format!("(require-all {})", extension_parts.join(" ")));
    }

    if policies.is_empty() {
        return String::new();
    }

    // Combine all write policies with allow
    format!("(allow file-write*\n  {})", policies.join("\n  "))
}

/// Generate parameter definitions for variable substitution in the policy.
///
/// sandbox-exec allows -DKEY=VALUE to substitute `(param "KEY")` in the policy.
fn generate_params(policy: &SandboxPolicy, cwd: &Path) -> Vec<(String, PathBuf)> {
    let mut params = Vec::new();

    // Add writable root parameters
    let writable_roots = policy.get_writable_roots(cwd);

    for (index, root) in writable_roots.iter().enumerate() {
        let canonical = root
            .root
            .canonicalize()
            .unwrap_or_else(|_| root.root.clone());
        params.push((format!("WRITABLE_ROOT_{index}"), canonical));

        // Add parameters for read-only subpaths
        for (subpath_index, subpath) in root.read_only_subpaths.iter().enumerate() {
            let canonical_subpath = subpath.canonicalize().unwrap_or_else(|_| {
                match (
                    subpath.parent().map(Path::canonicalize),
                    subpath.file_name(),
                ) {
                    (Some(Ok(parent)), Some(name)) => parent.join(name),
                    _ => subpath.clone(),
                }
            });
            params.push((
                format!("WRITABLE_ROOT_{index}_RO_{subpath_index}"),
                canonical_subpath,
            ));
        }
    }

    // Add Darwin user cache directory
    if let Some(cache_dir) = get_darwin_user_cache_dir() {
        params.push(("DARWIN_USER_CACHE_DIR".to_string(), cache_dir));
    } else {
        // Fallback to a reasonable default
        if let Ok(home) = std::env::var("HOME") {
            params.push((
                "DARWIN_USER_CACHE_DIR".to_string(),
                PathBuf::from(format!("{home}/Library/Caches")),
            ));
        }
    }

    // Cargo home (#558): paired with the policy lines emitted by
    // `generate_policy` when `resolve_cargo_home()` succeeds. Both helpers
    // use the same fallback chain so the policy text and the -DKEY=VALUE
    // params stay in sync — emit one without the other and sandbox-exec
    // refuses to load the profile.
    if let Some(home) = resolve_cargo_home() {
        let canonical_home = home.canonicalize().unwrap_or_else(|_| home.clone());
        params.push((
            "CARGO_HOME_REGISTRY".to_string(),
            canonical_home.join("registry"),
        ));
        params.push(("CARGO_HOME_GIT".to_string(), canonical_home.join("git")));
        params.push(("CARGO_HOME".to_string(), canonical_home));
    }

    // npm cache (#1267): paired with the policy lines emitted by
    // `generate_policy` when `resolve_npm_cache_dir()` succeeds. Both helpers
    // use the same fallback chain so the policy text and the -DKEY=VALUE
    // params stay in sync.
    if let Some(npm_cache) = resolve_npm_cache_dir() {
        let canonical = npm_cache
            .canonicalize()
            .unwrap_or_else(|_| npm_cache.clone());
        params.push(("NPM_CACHE_DIR".to_string(), canonical));
    }

    params
}

/// Get the Darwin user cache directory using confstr.
///
/// This returns the per-user cache directory that macOS assigns,
/// typically something like /var/folders/xx/xxx.../C/
fn get_darwin_user_cache_dir() -> Option<PathBuf> {
    // Use libc to call confstr for _CS_DARWIN_USER_CACHE_DIR
    let mut buf = vec![0i8; (libc::PATH_MAX as usize) + 1];

    // Safety: `buf` is a writable buffer sized to PATH_MAX + 1 for confstr.
    let len =
        unsafe { libc::confstr(libc::_CS_DARWIN_USER_CACHE_DIR, buf.as_mut_ptr(), buf.len()) };

    if len == 0 {
        return None;
    }

    // Convert the C string to a Rust PathBuf
    // Safety: confstr guarantees a NUL-terminated string in `buf` when len > 0.
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    let path_str = cstr.to_str().ok()?;
    let path = PathBuf::from(path_str);

    // Try to canonicalize, but return the raw path if that fails
    path.canonicalize().ok().or(Some(path))
}

/// Detect sandbox denial from command output.
///
/// Returns true if the output suggests the sandbox blocked an operation.
pub fn detect_denial(exit_code: i32, stderr: &str) -> bool {
    if exit_code == 0 {
        return false;
    }

    // Common sandbox denial messages
    let denial_patterns = [
        "Operation not permitted",
        "sandbox-exec",
        "deny(",
        "Sandbox: ",
    ];

    denial_patterns.iter().any(|p| stderr.contains(p))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod existing_tests {
    use super::*;

    // Tests that mutate HOME/CARGO_HOME use codewhale_runtime::test_support::lock_test_env()
    // so they don't race with sibling tests in this crate that read those vars.
    #[test]
    fn test_generate_policy_with_network() {
        let policy = SandboxPolicy::workspace_with_network();
        let cwd = Path::new("/tmp/test");
        let result = generate_policy(&policy, cwd, &[]);

        assert!(result.contains("network-outbound"));
        assert!(result.contains("network-inbound"));
    }

    #[test]
    fn test_generate_params() {
        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let params = generate_params(&policy, cwd);

        // Should have at least the cache dir param
        assert!(params.iter().any(|(k, _)| k == "DARWIN_USER_CACHE_DIR"));
    }

    #[test]
    fn test_darwin_user_cache_write_skipped_for_read_only() {
        let cwd = Path::new("/tmp/test");

        let default_text = generate_policy(&SandboxPolicy::default(), cwd, &[]);
        assert!(
            default_text
                .contains(r#"(allow file-read* (subpath (param "DARWIN_USER_CACHE_DIR")))"#),
            "default policy should allow reading the Darwin user cache"
        );
        assert!(
            default_text
                .contains(r#"(allow file-write* (subpath (param "DARWIN_USER_CACHE_DIR")))"#),
            "default policy should allow writing the Darwin user cache"
        );

        let read_only_text = generate_policy(&SandboxPolicy::read_only(), cwd, &[]);
        assert!(
            read_only_text
                .contains(r#"(allow file-read* (subpath (param "DARWIN_USER_CACHE_DIR")))"#),
            "read-only mode should still allow reading the Darwin user cache"
        );
        assert!(
            !read_only_text
                .contains(r#"(allow file-write* (subpath (param "DARWIN_USER_CACHE_DIR")))"#),
            "read-only mode must NOT grant write access to the Darwin user cache"
        );
        assert!(
            !read_only_text.contains(
                r#"(allow file-read* file-write* (subpath (param "DARWIN_USER_CACHE_DIR")))"#
            ),
            "read-only mode must not combine Darwin cache write into the read rule"
        );
    }

    /// #558: cargo publish reaches into ~/.cargo/registry; the seatbelt has
    /// to allow read+write inside it. Both the policy text and the param
    /// table must be in sync — emitting one without the other makes
    /// sandbox-exec refuse to load the profile.
    #[test]
    fn test_cargo_home_paths_emitted_in_policy_and_params_when_home_set() {
        let _guard = codewhale_runtime::test_support::lock_test_env();

        // SAFETY: HOME / CARGO_HOME are process-global. lock_test_env
        // serializes tests that mutate them, and we always restore the
        // prior value before returning.
        let saved_home = std::env::var_os("HOME");
        let saved_cargo = std::env::var_os("CARGO_HOME");
        unsafe {
            std::env::set_var("HOME", "/tmp/seatbelt-cargo-test");
            std::env::remove_var("CARGO_HOME");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");

        let policy_text = generate_policy(&policy, cwd, &[]);
        assert!(policy_text.contains(r#"(allow file-read* (subpath (param "CARGO_HOME")))"#));
        assert!(policy_text.contains("CARGO_HOME_REGISTRY"));
        assert!(policy_text.contains("CARGO_HOME_GIT"));

        let params = generate_params(&policy, cwd);
        assert!(params.iter().any(|(k, _)| k == "CARGO_HOME"));
        assert!(params.iter().any(|(k, _)| k == "CARGO_HOME_REGISTRY"));
        assert!(params.iter().any(|(k, _)| k == "CARGO_HOME_GIT"));

        // Read-only policy should still emit CARGO_HOME read rule but skip writes.
        let read_only_text = generate_policy(&SandboxPolicy::read_only(), cwd, &[]);
        assert!(
            read_only_text.contains(r#"(allow file-read* (subpath (param "CARGO_HOME")))"#),
            "read-only mode should still allow reading the cargo registry: {read_only_text}"
        );
        assert!(
            !read_only_text
                .contains(r#"(allow file-write* (subpath (param "CARGO_HOME_REGISTRY")))"#),
            "read-only mode must NOT grant write access to the cargo registry"
        );

        // Restore.
        // SAFETY: restoring the prior value the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_cargo {
                Some(v) => std::env::set_var("CARGO_HOME", v),
                None => std::env::remove_var("CARGO_HOME"),
            }
        }
    }

    /// #558: if neither `CARGO_HOME` nor `HOME` is set, the cargo lines and
    /// their params must both be omitted — emitting one without the other
    /// would crash sandbox-exec on profile load.
    #[test]
    fn test_cargo_home_skipped_when_no_env() {
        let _guard = codewhale_runtime::test_support::lock_test_env();

        let saved_home = std::env::var_os("HOME");
        let saved_cargo = std::env::var_os("CARGO_HOME");
        // SAFETY: HOME/CARGO_HOME are process-global; lock_test_env serializes
        // mutations here and we restore the prior values before returning.
        unsafe {
            std::env::remove_var("HOME");
            std::env::remove_var("CARGO_HOME");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let policy_text = generate_policy(&policy, cwd, &[]);
        let params = generate_params(&policy, cwd);

        assert!(!policy_text.contains("CARGO_HOME"));
        assert!(!params.iter().any(|(k, _)| k.starts_with("CARGO_HOME")));

        // Restore.
        // SAFETY: restoring the prior values the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_cargo {
                Some(v) => std::env::set_var("CARGO_HOME", v),
                None => std::env::remove_var("CARGO_HOME"),
            }
        }
    }

    /// #1267: npx MCP servers write to ~/.npm on first run; the seatbelt must
    /// allow writes to the npm cache directory. Both the policy text and the
    /// param table must be in sync — emitting one without the other makes
    /// sandbox-exec refuse to load the profile.
    #[test]
    fn test_npm_cache_paths_emitted_in_policy_and_params_when_home_set() {
        let _guard = codewhale_runtime::test_support::lock_test_env();

        let saved_home = std::env::var_os("HOME");
        let saved_npm = std::env::var_os("NPM_CONFIG_CACHE");
        // SAFETY: HOME/NPM_CONFIG_CACHE are process-global; lock_test_env
        // serializes mutations here, and we always restore the prior value.
        unsafe {
            std::env::set_var("HOME", "/tmp/seatbelt-npm-test");
            std::env::remove_var("NPM_CONFIG_CACHE");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");

        let policy_text = generate_policy(&policy, cwd, &[]);
        assert!(
            policy_text.contains(r#"(allow file-read* (subpath (param "NPM_CACHE_DIR")))"#),
            "npm cache read rule missing from policy"
        );
        assert!(
            policy_text.contains(r#"(allow file-write* (subpath (param "NPM_CACHE_DIR")))"#),
            "npm cache write rule missing from default policy"
        );

        let params = generate_params(&policy, cwd);
        assert!(
            params.iter().any(|(k, _)| k == "NPM_CACHE_DIR"),
            "NPM_CACHE_DIR param missing"
        );

        // ReadOnly policy: read access allowed, write access must be absent.
        let read_only_text = generate_policy(&SandboxPolicy::read_only(), cwd, &[]);
        assert!(
            read_only_text.contains(r#"(allow file-read* (subpath (param "NPM_CACHE_DIR")))"#),
            "read-only mode should allow reading the npm cache"
        );
        assert!(
            !read_only_text.contains(r#"(allow file-write* (subpath (param "NPM_CACHE_DIR")))"#),
            "read-only mode must NOT grant write access to the npm cache"
        );

        // Restore.
        // SAFETY: restoring the prior values the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_npm {
                Some(v) => std::env::set_var("NPM_CONFIG_CACHE", v),
                None => std::env::remove_var("NPM_CONFIG_CACHE"),
            }
        }
    }

    /// #1267: if neither `NPM_CONFIG_CACHE` nor `HOME` is set, the npm lines
    /// and their param must both be omitted.
    #[test]
    fn test_npm_cache_skipped_when_no_env() {
        let _guard = codewhale_runtime::test_support::lock_test_env();

        let saved_home = std::env::var_os("HOME");
        let saved_npm = std::env::var_os("NPM_CONFIG_CACHE");
        // SAFETY: HOME/NPM_CONFIG_CACHE are process-global; lock_test_env
        // serializes mutations here and we restore the prior values before returning.
        unsafe {
            std::env::remove_var("HOME");
            std::env::remove_var("NPM_CONFIG_CACHE");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let policy_text = generate_policy(&policy, cwd, &[]);
        let params = generate_params(&policy, cwd);

        assert!(!policy_text.contains("NPM_CACHE_DIR"));
        assert!(!params.iter().any(|(k, _)| k == "NPM_CACHE_DIR"));

        // Restore.
        // SAFETY: restoring the prior values the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_npm {
                Some(v) => std::env::set_var("NPM_CONFIG_CACHE", v),
                None => std::env::remove_var("NPM_CONFIG_CACHE"),
            }
        }
    }

    #[test]
    fn test_generate_policy_allows_dev_tty() {
        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let policy_text = generate_policy(&policy, cwd, &[]);

        assert!(
            policy_text
                .contains(r#"(allow file-read* file-write* file-ioctl (literal "/dev/tty"))"#),
            "TTY-mode shells need /dev/tty access for sshpass/sudo prompts"
        );
    }

    #[test]
    fn seatbelt_profile_grants_network_only_when_the_policy_does() {
        // The OS layer and the application-level policy must agree. The
        // seatbelt base profile is `(deny default)` with no network rules, so
        // absence of SEATBELT_NETWORK_POLICY is a real denial, not a gap.
        let cwd = Path::new("/tmp/test");

        let restricted = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![cwd.to_path_buf()],
            network_access: false,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        };
        let text = generate_policy(&restricted, cwd, &[]);
        assert!(
            !text.contains("network-outbound"),
            "a network-restricted policy must not emit outbound rules:\n{text}"
        );
        assert!(!text.contains("network-inbound"));
        assert!(!text.contains("network-bind"));

        let allowed = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![cwd.to_path_buf()],
            network_access: true,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        };
        let text = generate_policy(&allowed, cwd, &[]);
        assert!(
            text.contains("network-outbound"),
            "an explicitly granted policy must emit outbound rules:\n{text}"
        );

        // Default construction is restricted, so the shipped default profile
        // carries no network rules.
        assert!(!generate_policy(&SandboxPolicy::default(), cwd, &[]).contains("network-outbound"));
    }

    #[test]
    fn test_create_seatbelt_args() {
        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let command = vec!["echo".to_string(), "hello".to_string()];

        let args = create_seatbelt_args(command, &policy, cwd, &[]);

        // Should start with -p and the policy
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("(version 1)"));

        // Should contain the separator
        assert!(args.contains(&"--".to_string()));

        // Should end with the original command
        assert!(args.contains(&"echo".to_string()));
        assert!(args.contains(&"hello".to_string()));
    }

    /// #4828: `open`/`osascript`/`launchctl` die with exit -54 under
    /// `(deny default)` because AppleEvent sends and the LaunchServices
    /// `lsopen` operation are blocked. Only the trusted (full-disk-write)
    /// tier gains those allowances; the restrictive tiers must stay locked
    /// down since AppleEvents automation can drive other apps to act
    /// outside the sandbox.
    #[test]
    fn test_apple_events_allowed_only_in_trusted_tier() {
        let cwd = Path::new("/tmp/test");

        let trusted = generate_policy(&SandboxPolicy::DangerFullAccess, cwd, &[]);
        assert!(
            trusted.contains("(allow appleevent-send)"),
            "trusted tier must allow AppleEvent sends: {trusted}"
        );
        assert!(
            trusted.contains("(allow lsopen)"),
            "trusted tier must allow LaunchServices lsopen: {trusted}"
        );
        assert!(
            trusted.contains(r#"(global-name "com.apple.coreservices.launchservicesd")"#),
            "trusted tier must pin the launchservicesd mach name: {trusted}"
        );
        assert!(
            trusted.contains(r#"(global-name "com.apple.coreservices.appleevents")"#),
            "trusted tier must pin the appleevents mach name: {trusted}"
        );

        for (name, restrictive) in [
            (
                "workspace-write",
                generate_policy(&SandboxPolicy::default(), cwd, &[]),
            ),
            (
                "workspace-write+network",
                generate_policy(&SandboxPolicy::workspace_with_network(), cwd, &[]),
            ),
            (
                "read-only",
                generate_policy(&SandboxPolicy::read_only(), cwd, &[]),
            ),
        ] {
            assert!(
                !restrictive.contains("appleevent-send"),
                "{name} tier must NOT allow AppleEvent sends: {restrictive}"
            );
            assert!(
                !restrictive.contains("lsopen"),
                "{name} tier must NOT allow LaunchServices lsopen: {restrictive}"
            );
        }
    }

    /// #4828: the trusted-tier policy (with the AppleEvents/LaunchServices
    /// additions) must still be a valid SBPL profile that sandbox-exec
    /// accepts.
    #[test]
    fn test_trusted_tier_policy_parses_under_sandbox_exec() {
        // generate_policy/generate_params both read HOME/CARGO_HOME; take the
        // env lock so sibling tests mutating those vars can't desync the
        // policy text from its -DKEY=VALUE params mid-call.
        let _guard = codewhale_runtime::test_support::lock_test_env();

        assert!(
            is_available(),
            "UNRUN: macOS sandbox-exec is unavailable; generated policy was not parsed"
        );

        let cwd = std::env::temp_dir();
        let args = create_seatbelt_args(
            vec!["/usr/bin/true".to_string()],
            &SandboxPolicy::DangerFullAccess,
            &cwd,
            &[],
        );
        let output = Command::new(SANDBOX_EXEC_PATH)
            .args(args)
            .current_dir(&cwd)
            .output()
            .expect("run sandbox-exec with trusted-tier policy");
        assert!(
            output.status.success(),
            "sandbox-exec rejected the trusted-tier policy: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_detect_denial() {
        assert!(detect_denial(1, "Operation not permitted"));
        assert!(detect_denial(1, "Sandbox: ls denied file-write*"));
        assert!(!detect_denial(0, "Operation not permitted"));
        assert!(!detect_denial(1, "File not found"));
    }
}
