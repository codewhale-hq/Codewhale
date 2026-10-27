//! Sanitized environment handling for child processes.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};

#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
#[cfg(windows)]
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
#[cfg(windows)]
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    RegCloseKey, RegEnumValueW, RegOpenKeyExW,
};
#[cfg(windows)]
use windows::core::{PCWSTR, PWSTR};

/// Convert a string env map into owned OS strings for child env helpers.
pub fn string_map_env(
    env: &HashMap<String, String>,
) -> impl Iterator<Item = (OsString, OsString)> + '_ {
    env.iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
}

/// Return the environment for a child process after dropping parent secrets.
///
/// `overrides` are trusted call-site values, such as sandbox markers, hook
/// variables, MCP server config, or RLM context path. They are applied after the
/// parent allowlist so explicit values win.
pub fn sanitized_child_env<I, K, V>(overrides: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut env = Vec::new();
    #[cfg(windows)]
    append_sanitized_child_env_candidates(&mut env, windows_registry_env_vars());
    for (key, value) in std::env::vars_os() {
        append_sanitized_child_env_candidate(&mut env, key, value);
    }
    for (key, value) in overrides {
        upsert_env(
            &mut env,
            key.as_ref().to_os_string(),
            value.as_ref().to_os_string(),
        );
    }
    #[cfg(windows)]
    fill_windows_common_program_files(&mut env);
    // OS sandboxes set CODEWHALE_SANDBOX (seatbelt / bwrap / windows). sccache
    // and other RUSTC_WRAPPER compilers need sockets or files those sandboxes
    // deny, so nested `cargo` fails with "Operation not permitted" before any
    // crate compiles (seen on macOS CI seatbelt). Drop the wrapper so rustc
    // still runs under the sandbox; unsandboxed children keep it. A
    // CARGO_BUILD_BUILD_DIR outside the sandbox's writable roots fails the
    // same way when nested cargo creates that directory.
    if env
        .iter()
        .any(|(key, _)| normalize_key(key) == "CODEWHALE_SANDBOX")
    {
        env.retain(|(key, _)| {
            let key = normalize_key(key);
            key != "RUSTC_WRAPPER" && key != "CARGO_BUILD_BUILD_DIR"
        });
    }
    env
}

/// Environment for a Codewhale runtime child (a Fleet worker), built from a
/// snapshot of the parent environment.
///
/// It uses the same allowlist as [`sanitized_child_env`], so provider keys and
/// other secret-shaped variables are dropped, with one deliberate difference:
/// proxy URLs keep their `user:password@` part. The runtime child is Codewhale
/// itself, not a model-chosen program, and must reach its provider through the
/// same authenticated proxy as the parent. Every tool it starts builds its own
/// environment through [`sanitized_child_env`], which strips that userinfo at
/// the model-facing boundary.
pub fn sanitized_runtime_env_from<B, K, V>(base_environment: B) -> Vec<(OsString, OsString)>
where
    B: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut env = Vec::new();
    for (key, value) in base_environment {
        if is_allowed_parent_env_key(key.as_ref()) {
            upsert_env(
                &mut env,
                key.as_ref().to_os_string(),
                value.as_ref().to_os_string(),
            );
        }
    }
    env
}

pub fn apply_to_command<I, K, V>(cmd: &mut std::process::Command, overrides: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    cmd.env_clear();
    for (key, value) in sanitized_child_env(overrides) {
        cmd.env(key, value);
    }
}

pub fn apply_to_tokio_command<I, K, V>(cmd: &mut tokio::process::Command, overrides: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    cmd.env_clear();
    for (key, value) in sanitized_child_env(overrides) {
        cmd.env(key, value);
    }
}

/// Parent variables git needs beyond the base allowlist: the ssh agent for
/// remote transports, where the user's global config lives, a pinned ssh
/// transport, and author/committer identity. None of them is a secret value.
const GIT_PASSTHROUGH_KEYS: &[&str] = &[
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GNUPGHOME",
    "XDG_CONFIG_HOME",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_EXEC_PATH",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CEILING_DIRECTORIES",
    "GIT_SSL_CAINFO",
    "GIT_SSL_CAPATH",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
];

/// Start a git child from the sanitized environment plus
/// `GIT_PASSTHROUGH_KEYS`. Repository config (`core.fsmonitor`, hooks,
/// filters, `core.sshCommand`) can make any git command run a program chosen
/// by whoever wrote the workspace, so git must not hand that program the
/// parent's credentials.
pub fn apply_to_git_command(cmd: &mut std::process::Command) {
    let passthrough: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            let normalized = normalize_key(key);
            GIT_PASSTHROUGH_KEYS.contains(&normalized.as_str())
        })
        .collect();
    apply_to_command(cmd, passthrough);
}

#[cfg(not(target_env = "ohos"))]
pub fn apply_to_pty_command<I, K, V>(cmd: &mut portable_pty::CommandBuilder, overrides: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    cmd.env_clear();
    for (key, value) in sanitized_child_env(overrides) {
        cmd.env(key, value);
    }
}

/// Build the sanitized child environment used for MCP stdio servers.
///
/// MCP stdio servers are user-configured integrations declared in
/// `~/.deepseek/mcp.json` (or equivalent). They are not arbitrary processes
/// the agent decided to launch on its own. To avoid breaking common
/// `npx ...` / `uvx ...` / `python -m mcp_server_*` setups (#1244), the
/// MCP-launch allowlist is wider than the base shell-tool allowlist: it
/// also passes through Node, npm, Python, Ruby, Java, proxy, and CA-bundle
/// bootstrap variables, plus the non-secret AWS profile/region/config-path
/// selectors. It still drops arbitrary parent env so secret-bearing vars
/// (AWS keys and session tokens, `*_API_KEY`, `GITHUB_TOKEN`, …) are not
/// silently exported.
pub fn sanitized_mcp_env<I, K, V>(overrides: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut env = Vec::new();
    for (key, value) in std::env::vars_os() {
        if is_allowed_mcp_env_key(&key) {
            upsert_env(&mut env, key, value);
        }
    }
    for (key, value) in overrides {
        upsert_env(
            &mut env,
            key.as_ref().to_os_string(),
            value.as_ref().to_os_string(),
        );
    }
    env
}

/// Build the environment for a reviewed plugin-contributed MCP child.
///
/// Unlike user-authored MCP configuration, a plugin must name every extra
/// environment source during trust review. Start from the ordinary
/// secret-scrubbed child environment, remove ambient proxy variables whose
/// URLs may themselves contain credentials, then apply only reviewed
/// overrides. `NO_PROXY` remains safe routing metadata.
#[cfg(test)]
pub fn sanitized_plugin_mcp_env<I, K, V>(overrides: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    sanitized_plugin_mcp_env_from(std::env::vars_os(), overrides)
}

/// Build a reviewed plugin child environment from an immutable host snapshot.
///
/// This is separate from `sanitized_plugin_mcp_env` so a repository-local
/// dotenv file loaded after startup cannot add or replace inherited values.
pub fn sanitized_plugin_mcp_env_from<B, BK, BV, I, K, V>(
    base_environment: B,
    overrides: I,
) -> Vec<(OsString, OsString)>
where
    B: IntoIterator<Item = (BK, BV)>,
    BK: AsRef<OsStr>,
    BV: AsRef<OsStr>,
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut env = Vec::new();
    for (key, value) in base_environment {
        if is_allowed_parent_env_key(key.as_ref()) {
            upsert_env(
                &mut env,
                key.as_ref().to_os_string(),
                value.as_ref().to_os_string(),
            );
        }
    }
    env.retain(|(key, _)| {
        !matches!(
            normalize_key(key).as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "FTP_PROXY"
        )
    });
    for (key, value) in overrides {
        upsert_env(
            &mut env,
            key.as_ref().to_os_string(),
            value.as_ref().to_os_string(),
        );
    }
    env
}

pub fn apply_to_tokio_command_mcp<I, K, V>(cmd: &mut tokio::process::Command, overrides: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    cmd.env_clear();
    for (key, value) in sanitized_mcp_env(overrides) {
        cmd.env(key, value);
    }
}

fn is_allowed_parent_env_key(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    let normalized = key.to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "PATH"
            // Desktop connection metadata. Computer-use tools must reach
            // the existing X11/Wayland and accessibility bus sessions. Keep
            // this list exact; never inherit arbitrary XDG/DBUS namespaces
            // or read the Xauthority credential file into the environment.
            | "DISPLAY"
            | "WAYLAND_DISPLAY"
            | "XDG_RUNTIME_DIR"
            | "XDG_SESSION_TYPE"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "XAUTHORITY"
            | "HOME"
            | "USER"
            | "USERNAME"
            | "LOGNAME"
            | "LANG"
            | "LANGUAGE"
            | "LC_ALL"
            | "LC_CTYPE"
            | "LC_MESSAGES"
            | "TERM"
            | "COLORTERM"
            | "NO_COLOR"
            | "FORCE_COLOR"
            | "SHELL"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "__CF_USER_TEXT_ENCODING"
            | "SYSTEMROOT"
            | "WINDIR"
            | "COMSPEC"
            | "PATHEXT"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
            // Preserve Windows toolchain context when the parent shell has
            // already loaded VsDevCmd / vcvars. Without these, `exec_shell`
            // can find `link.exe` via PATH but still fail to resolve
            // SDK/CRT libraries like `kernel32.lib`, so any model-driven
            // `cargo build` from inside the TUI silently breaks on
            // Windows installs that don't run inside a Developer Command
            // Prompt. Harvested from PR #1487.
            | "LIB"
            | "LIBPATH"
            | "INCLUDE"
            | "VSINSTALLDIR"
            | "VCINSTALLDIR"
            | "VCTOOLSINSTALLDIR"
            | "WINDOWSSDKDIR"
            | "WINDOWSSDKVERSION"
            | "UNIVERSALCRTSDKDIR"
            | "UCRTVERSION"
            | "EXTENSIONSDKDIR"
            | "DEVENVDIR"
            | "VISUALSTUDIOVERSION"
            // Windows app-data + .NET/NuGet paths. `dotnet restore` (and npm,
            // pip, etc.) resolve their package caches, HTTP cache, and config
            // under %APPDATA% / %LOCALAPPDATA% / %ProgramData% / %ProgramFiles%.
            // The sanitized child env dropped these, so restore failed through
            // `exec_shell` even though it worked in the user's own shell, where
            // the full environment is present (#1857). `DOTNET_*` (below) covers
            // DOTNET_ROOT and the CLI flags.
            | "APPDATA"
            | "LOCALAPPDATA"
            | "PROGRAMDATA"
            | "ALLUSERSPROFILE"
            | "PROGRAMFILES"
            | "PROGRAMFILES(X86)"
            | "PROGRAMW6432"
            | "COMMONPROGRAMFILES"
            | "COMMONPROGRAMFILES(X86)"
            | "COMMONPROGRAMW6432"
            | "PROCESSOR_ARCHITECTURE"
            | "NUGET_PACKAGES"
            | "NUGET_HTTP_CACHE_PATH"
            // Standard proxy variables are needed by shell tasks in
            // corporate and WSL environments where direct internet egress is
            // blocked. They intentionally exclude token/API-key-shaped vars.
            | "HTTP_PROXY"
            | "HTTPS_PROXY"
            | "NO_PROXY"
            | "ALL_PROXY"
            | "FTP_PROXY"
            // Python uses these to pick stdio/default encodings when stdout is
            // piped instead of attached to a Windows console (#4202).
            | "PYTHONIOENCODING"
            | "PYTHONUTF8"
            // Rustup installs `cargo`/`rustc` as shims and resolves the real
            // toolchain through these non-secret bootstrap paths. Dropping
            // them makes an otherwise working Rust toolchain unusable in
            // official Rust containers and other non-default installations.
            | "CARGO_HOME"
            | "RUSTUP_HOME"
            | "RUSTUP_TOOLCHAIN"
            // Non-secret build and toolchain configuration. Model-run
            // builds and tests (`run_tests`, verifier and task gates,
            // language servers) must honour the same target dir, job limit,
            // flags, active virtualenv and CA bundle as the user's own shell;
            // dropping them caused full rebuilds, lost memory limits and TLS
            // failures. `CARGO_*` is handled below without its secret-shaped
            // and registry keys. Connection strings such as `DATABASE_URL`
            // stay out: they usually embed a password. `RUSTC_WRAPPER` is
            // kept for unsandboxed children only; see the CODEWHALE_SANDBOX
            // scrub in `sanitized_child_env`.
            | "RUSTFLAGS"
            | "RUSTDOCFLAGS"
            | "RUSTC_WRAPPER"
            | "RUST_BACKTRACE"
            | "RUST_LOG"
            | "RUST_MIN_STACK"
            | "RUST_TEST_THREADS"
            | "VIRTUAL_ENV"
            | "CONDA_PREFIX"
            | "CONDA_DEFAULT_ENV"
            | "PYTHONPATH"
            | "JAVA_HOME"
            | "GOPATH"
            | "GOROOT"
            | "GOBIN"
            | "GOFLAGS"
            | "GOCACHE"
            | "GOMODCACHE"
            | "GOTOOLCHAIN"
            | "GO111MODULE"
            | "NVM_DIR"
            | "NVM_BIN"
            | "NVM_INC"
            | "VOLTA_HOME"
            | "NODE_PATH"
            | "NODE_OPTIONS"
            | "NODE_EXTRA_CA_CERTS"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            | "REQUESTS_CA_BUNDLE"
            | "CURL_CA_BUNDLE"
    ) || normalized.starts_with("LC_")
        || is_allowed_cargo_config_key(&normalized)
        // .NET CLI / SDK configuration (DOTNET_ROOT, DOTNET_CLI_*,
        // DOTNET_NOLOGO, DOTNET_CLI_TELEMETRY_OPTOUT, …). Paths and flags
        // only — no secret-shaped values (#1857).
        || normalized.starts_with("DOTNET_")
        || is_allowed_platform_path_like_child_env_key(&normalized)
}

/// Cargo's `CARGO_*` configuration namespace (`CARGO_TARGET_DIR`,
/// `CARGO_BUILD_JOBS`, `CARGO_INCREMENTAL`, ...) minus anything that can carry
/// a credential: registry tokens and credential providers live under
/// `CARGO_REGISTRY_` / `CARGO_REGISTRIES_`, and secret-shaped names are
/// dropped wherever they appear. `CARGO_HTTP_PROXY` is passed with its
/// userinfo removed, like the other proxy variables.
fn is_allowed_cargo_config_key(normalized: &str) -> bool {
    normalized.starts_with("CARGO_")
        && !normalized.starts_with("CARGO_REGISTRY_")
        && !normalized.starts_with("CARGO_REGISTRIES_")
        && !is_secret_like_child_env_key(normalized)
}

/// Proxy variables whose URL may embed `user:password@`.
fn is_proxy_url_key(normalized: &str) -> bool {
    matches!(
        normalized,
        "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "FTP_PROXY" | "CARGO_HTTP_PROXY"
    )
}

/// Remove the `user:password@` part of a proxy URL so a child keeps the route
/// (`scheme://host:port`) but not the proxy credentials. Values that are not
/// valid UTF-8 cannot be inspected and are dropped.
fn strip_proxy_userinfo(value: &OsStr) -> Option<OsString> {
    let value = value.to_str()?;
    let (scheme, rest) = match value.find("://") {
        Some(index) => value.split_at(index + 3),
        None => ("", value),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    Some(OsString::from(format!("{scheme}{host}{tail}")))
}

#[cfg(windows)]
fn is_allowed_platform_path_like_child_env_key(normalized: &str) -> bool {
    is_allowed_path_like_child_env_key(normalized)
}

#[cfg(not(windows))]
fn is_allowed_platform_path_like_child_env_key(_normalized: &str) -> bool {
    false
}

#[cfg(windows)]
fn is_allowed_path_like_child_env_key(normalized: &str) -> bool {
    if is_secret_like_child_env_key(normalized) {
        return false;
    }
    normalized.ends_with("_ROOT")
        || normalized.ends_with("_DIR")
        || normalized.ends_with("_HOME")
        || normalized.ends_with("_PATH")
        || normalized.ends_with("_PATHS")
        || normalized.ends_with("SDKROOT")
}

fn is_secret_like_child_env_key(normalized: &str) -> bool {
    normalized.contains("SECRET")
        || normalized.contains("TOKEN")
        || normalized.contains("PASSWORD")
        || normalized.contains("PASSWD")
        || normalized.contains("CREDENTIAL")
        || normalized.contains("API_KEY")
        || normalized.contains("ACCESS_KEY")
        || normalized.contains("PRIVATE_KEY")
        || normalized.ends_with("_KEY")
}

/// Allowlist for MCP stdio launches. Strict superset of
/// `is_allowed_parent_env_key`. See `sanitized_mcp_env` for rationale.
fn is_allowed_mcp_env_key(key: &OsStr) -> bool {
    if is_allowed_parent_env_key(key) {
        return true;
    }
    let key_str = key.to_string_lossy();
    let normalized = key_str.to_ascii_uppercase();
    if matches!(
        normalized.as_str(),
        // Node.js / npm / npx / pnpm / yarn / volta / corepack
        "NVM_DIR"
            | "NVM_BIN"
            | "NVM_INC"
            | "VOLTA_HOME"
            | "COREPACK_HOME"
            | "NODE_PATH"
            | "NODE_OPTIONS"
            | "NODE_EXTRA_CA_CERTS"
            // Python ecosystem
            | "PYTHONPATH"
            | "PYTHONHOME"
            | "PYTHONDONTWRITEBYTECODE"
            | "PYTHONUNBUFFERED"
            | "VIRTUAL_ENV"
            | "POETRY_HOME"
            | "PIPX_HOME"
            | "PIPX_BIN_DIR"
            // Ruby ecosystem
            | "GEM_HOME"
            | "GEM_PATH"
            | "BUNDLE_PATH"
            | "BUNDLE_GEMFILE"
            // Java
            | "JAVA_HOME"
            // Network proxies (uppercase form; lowercase handled below)
            | "HTTP_PROXY"
            | "HTTPS_PROXY"
            | "NO_PROXY"
            | "ALL_PROXY"
            | "FTP_PROXY"
            // Custom CA bundles for corporate TLS interception
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            | "REQUESTS_CA_BUNDLE"
            | "CURL_CA_BUNDLE"
            // AWS profile/region selection and config-file locations. These
            // name *which* credentials to use, never the credentials
            // themselves: an AWS MCP server launched with `--profile x`
            // still needs the user's config file and region to resolve an
            // SSO session. Keys, secrets and session tokens
            // (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
            // `AWS_SESSION_TOKEN`, …) stay dropped.
            | "AWS_PROFILE"
            | "AWS_REGION"
            | "AWS_DEFAULT_REGION"
            | "AWS_CONFIG_FILE"
            | "AWS_SHARED_CREDENTIALS_FILE"
    ) {
        return true;
    }
    // npm config namespace (NPM_CONFIG_PREFIX, NPM_CONFIG_CACHE, …) and
    // uv (UV_CACHE_DIR, UV_PYTHON, …) — both ecosystems use a stable prefix
    // for their bootstrap configuration, so allow the whole namespace.
    if normalized.starts_with("NPM_CONFIG_") || normalized.starts_with("UV_") {
        return true;
    }
    false
}

#[cfg(windows)]
fn append_sanitized_child_env_candidates<I, K, V>(
    env: &mut Vec<(OsString, OsString)>,
    candidates: I,
) where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    for (key, value) in candidates {
        append_sanitized_child_env_candidate(env, key.into(), value.into());
    }
}

fn append_sanitized_child_env_candidate(
    env: &mut Vec<(OsString, OsString)>,
    key: OsString,
    value: OsString,
) {
    if !is_allowed_parent_env_key(&key) {
        return;
    }
    if is_proxy_url_key(&normalize_key(&key)) {
        if let Some(value) = strip_proxy_userinfo(&value) {
            upsert_env(env, key, value);
        }
        return;
    }
    upsert_env(env, key, value);
}

fn upsert_env(env: &mut Vec<(OsString, OsString)>, key: OsString, value: OsString) {
    let normalized = normalize_key(&key);
    env.retain(|(existing, _)| normalize_key(existing) != normalized);
    env.push((key, value));
}

#[cfg(windows)]
fn windows_registry_env_vars() -> Vec<(OsString, OsString)> {
    let mut env = Vec::new();
    append_windows_registry_env_key(
        &mut env,
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    );
    append_windows_registry_env_key(&mut env, HKEY_CURRENT_USER, "Environment");
    env
}

#[cfg(windows)]
fn append_windows_registry_env_key(env: &mut Vec<(OsString, OsString)>, root: HKEY, subkey: &str) {
    let mut key = HKEY::default();
    let subkey_wide = windows_wide_null(OsStr::new(subkey));
    // SAFETY: `subkey_wide` is NUL-terminated and live; `key` is live.
    let open =
        unsafe { RegOpenKeyExW(root, PCWSTR(subkey_wide.as_ptr()), None, KEY_READ, &mut key) };
    if open != ERROR_SUCCESS {
        return;
    }

    let mut index = 0;
    loop {
        match read_windows_registry_env_value(key, index) {
            RegistryEnvValue::Value(name, value) => {
                upsert_env(env, name, value);
                index += 1;
            }
            RegistryEnvValue::Skip => {
                index += 1;
            }
            RegistryEnvValue::Done => break,
        }
    }

    // SAFETY: `key` was opened above and is not used after.
    let _ = unsafe { RegCloseKey(key) };
}

#[cfg(windows)]
enum RegistryEnvValue {
    Value(OsString, OsString),
    Skip,
    Done,
}

#[cfg(windows)]
fn read_windows_registry_env_value(key: HKEY, index: u32) -> RegistryEnvValue {
    let mut name = vec![0u16; 32_767];
    let mut data = vec![0u8; 65_536];

    loop {
        let mut name_len = name.len() as u32;
        let mut data_len = data.len() as u32;
        let mut value_type = 0u32;
        // SAFETY: buffers are live with matching lengths passed.
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut value_type),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };

        if status == ERROR_NO_MORE_ITEMS {
            return RegistryEnvValue::Done;
        }
        if status == ERROR_MORE_DATA && resize_registry_data_buffer(&mut data, data_len) {
            continue;
        }
        if status != ERROR_SUCCESS {
            return RegistryEnvValue::Skip;
        }
        if value_type != REG_SZ.0 && value_type != REG_EXPAND_SZ.0 {
            return RegistryEnvValue::Skip;
        }

        let name = OsString::from_wide(&name[..name_len as usize]);
        let value = registry_utf16_value_from_bytes(&data[..data_len as usize]);
        let value = if REG_VALUE_TYPE(value_type) == REG_EXPAND_SZ {
            expand_windows_env_string(&value).unwrap_or(value)
        } else {
            value
        };
        return RegistryEnvValue::Value(name, value);
    }
}

#[cfg(windows)]
fn resize_registry_data_buffer(data: &mut Vec<u8>, required_len: u32) -> bool {
    let Ok(required_len) = usize::try_from(required_len) else {
        return false;
    };
    if required_len <= data.len() {
        return false;
    }
    data.resize(required_len, 0);
    true
}

#[cfg(windows)]
fn registry_utf16_value_from_bytes(data: &[u8]) -> OsString {
    let mut wide = data
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    while wide.last() == Some(&0) {
        wide.pop();
    }
    OsString::from_wide(&wide)
}

#[cfg(windows)]
fn expand_windows_env_string(value: &OsStr) -> Option<OsString> {
    let src = windows_wide_null(value);
    // SAFETY: `src` is NUL-terminated and live.
    let required_len = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), None) };
    if required_len == 0 {
        return None;
    }

    let mut expanded = vec![0u16; required_len as usize];
    // SAFETY: `src` is NUL-terminated; `expanded` has the queried length.
    let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut expanded)) };
    if written == 0 || written > required_len {
        return None;
    }

    let len = usize::try_from(written).ok()?.saturating_sub(1);
    Some(OsString::from_wide(&expanded[..len]))
}

#[cfg(windows)]
fn windows_wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(any(windows, test))]
fn fill_windows_common_program_files(env: &mut Vec<(OsString, OsString)>) {
    for (key, default) in [
        ("CommonProgramFiles", r"C:\Program Files\Common Files"),
        (
            "CommonProgramFiles(x86)",
            r"C:\Program Files (x86)\Common Files",
        ),
        ("CommonProgramW6432", r"C:\Program Files\Common Files"),
    ] {
        let existing = env
            .iter()
            .find(|(existing, _)| normalize_key(existing) == normalize_key(OsStr::new(key)))
            .map(|(_, value)| value.to_string_lossy().trim().is_empty());
        if existing.unwrap_or(true) {
            upsert_env(env, OsString::from(key), OsString::from(default));
        }
    }
}

fn normalize_key(key: &OsStr) -> String {
    key.to_string_lossy().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvVarGuard;

    #[test]
    fn mcp_env_allowlist_inherits_base_keys() {
        for key in [
            "PATH",
            "HOME",
            "USER",
            "TERM",
            "LANG",
            "SHELL",
            "LIB",
            "LIBPATH",
            "INCLUDE",
            "VCTOOLSINSTALLDIR",
            "WINDOWSSDKDIR",
        ] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should inherit base key {key}"
            );
        }
    }

    #[test]
    fn mcp_env_allowlist_includes_node_bootstrap_keys() {
        for key in [
            "NVM_DIR",
            "NVM_BIN",
            "NVM_INC",
            "NODE_PATH",
            "NODE_OPTIONS",
            "NODE_EXTRA_CA_CERTS",
            "VOLTA_HOME",
            "COREPACK_HOME",
        ] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should include {key}"
            );
        }
    }

    #[test]
    fn mcp_env_allowlist_includes_npm_config_prefix() {
        for key in [
            "NPM_CONFIG_PREFIX",
            "NPM_CONFIG_CACHE",
            "NPM_CONFIG_REGISTRY",
            "NPM_CONFIG_USERCONFIG",
        ] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should include npm config key {key}"
            );
        }
    }

    #[test]
    fn mcp_env_allowlist_includes_proxy_keys_either_case() {
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "no_proxy",
            "all_proxy",
        ] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should include proxy key {key}"
            );
        }
    }

    #[test]
    fn child_env_allowlist_includes_proxy_keys_either_case() {
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
            "ALL_PROXY",
            "FTP_PROXY",
            "http_proxy",
            "https_proxy",
            "no_proxy",
            "all_proxy",
            "ftp_proxy",
        ] {
            assert!(
                is_allowed_parent_env_key(OsStr::new(key)),
                "child env allowlist should include proxy key {key}"
            );
        }
    }

    #[test]
    fn child_env_allowlist_includes_dotnet_and_windows_appdata_keys() {
        // #1857: dotnet restore / NuGet need these to find caches and config.
        for key in [
            "APPDATA",
            "LOCALAPPDATA",
            "PROGRAMDATA",
            "ALLUSERSPROFILE",
            "PROGRAMFILES",
            "PROGRAMFILES(X86)",
            "PROGRAMW6432",
            "COMMONPROGRAMFILES",
            "COMMONPROGRAMFILES(X86)",
            "COMMONPROGRAMW6432",
            "PROCESSOR_ARCHITECTURE",
            "NUGET_PACKAGES",
            "DOTNET_ROOT",
            "DOTNET_CLI_TELEMETRY_OPTOUT",
            "DOTNET_NOLOGO",
            // Case-insensitive: the real Windows var is `ProgramFiles`.
            "ProgramFiles",
            "dotnet_root",
        ] {
            assert!(
                is_allowed_parent_env_key(OsStr::new(key)),
                "child env allowlist should include {key}"
            );
        }
        // Guard: NuGet credential env vars must still be dropped.
        assert!(
            !is_allowed_parent_env_key(OsStr::new("NuGetPackageSourceCredentials_feed")),
            "NuGet credential vars must not be exported to child processes"
        );
    }

    #[test]
    fn child_env_allowlist_includes_python_stdio_encoding_vars() {
        for key in ["PYTHONIOENCODING", "PYTHONUTF8", "pythonioencoding"] {
            assert!(
                is_allowed_parent_env_key(OsStr::new(key)),
                "child env allowlist should include Python stdio encoding key {key}"
            );
        }
    }

    #[test]
    fn child_env_allowlist_includes_rust_toolchain_bootstrap_keys() {
        for key in [
            "CARGO_HOME",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
            "cargo_home",
        ] {
            assert!(
                is_allowed_parent_env_key(OsStr::new(key)),
                "child env allowlist should include Rust bootstrap key {key}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn child_env_allowlist_includes_custom_path_like_vars_without_secrets() {
        // #3572: SDK/toolchain roots created through Windows Environment
        // Variables are often project-specific and cannot be exhaustively
        // named in the static allowlist.
        for key in [
            "BIMRV_SDK_ROOT",
            "ACME_TOOLCHAIN_HOME",
            "PROJECT_SDK_DIR",
            "CMAKE_PREFIX_PATH",
            "ANDROID_SDKROOT",
        ] {
            assert!(
                is_allowed_parent_env_key(OsStr::new(key)),
                "child env allowlist should include path-like key {key}"
            );
        }

        for key in [
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "MY_SECRET_ROOT",
            "SERVICE_PASSWORD_DIR",
            "AWS_ACCESS_KEY_ID",
            "PRIVATE_KEY_PATH",
            "NuGetPackageSourceCredentials_feed",
        ] {
            assert!(
                !is_allowed_parent_env_key(OsStr::new(key)),
                "secret-like key {key} must not be exported to child processes"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn sanitized_child_env_preserves_custom_sdk_root_vars() {
        let _guard = crate::test_support::lock_test_env();
        let _sdk = EnvVarGuard::set("BIMRV_SDK_ROOT", r"F:\Lib\BimRv27.5");
        let _secret = EnvVarGuard::set("MY_SECRET_ROOT", r"F:\Secrets");

        let env = sanitized_child_env(std::iter::empty::<(OsString, OsString)>());

        assert!(
            env.iter()
                .any(|(key, value)| key == "BIMRV_SDK_ROOT" && value == r"F:\Lib\BimRv27.5"),
            "child env should preserve custom SDK roots"
        );
        assert!(
            env.iter().all(|(key, _)| key != "MY_SECRET_ROOT"),
            "secret-like path vars must still be dropped"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_registry_env_candidates_preserve_custom_sdk_roots() {
        use windows::Win32::System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RegCreateKeyW, RegDeleteTreeW, RegSetValueExW,
        };

        let subkey = format!(r"Software\CodeWhaleTest\child_env_{}", std::process::id());
        let subkey_wide = windows_wide_null(OsStr::new(&subkey));
        let mut key = HKEY::default();
        let created =
            unsafe { RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(subkey_wide.as_ptr()), &mut key) };
        assert_eq!(created, ERROR_SUCCESS);

        set_registry_string_value(key, "BIMRV_SDK_ROOT", r"F:\Lib\BimRv27.5");
        set_registry_string_value(key, "MY_SECRET_ROOT", r"F:\Secrets");
        let _ = unsafe { RegCloseKey(key) };

        let mut candidates = Vec::new();
        append_windows_registry_env_key(&mut candidates, HKEY_CURRENT_USER, &subkey);
        let mut env = Vec::new();
        append_sanitized_child_env_candidates(&mut env, candidates);

        let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey_wide.as_ptr())) };

        assert!(
            env.iter()
                .any(|(key, value)| key == "BIMRV_SDK_ROOT" && value == r"F:\Lib\BimRv27.5"),
            "registry child env should preserve custom SDK roots"
        );
        assert!(
            env.iter().all(|(key, _)| key != "MY_SECRET_ROOT"),
            "secret-like registry vars must still be dropped"
        );

        fn set_registry_string_value(key: HKEY, name: &str, value: &str) {
            let name_wide = windows_wide_null(OsStr::new(name));
            let data = value
                .encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            let status = unsafe {
                RegSetValueExW(key, PCWSTR(name_wide.as_ptr()), None, REG_SZ, Some(&data))
            };
            assert_eq!(status, ERROR_SUCCESS);
        }
    }

    #[test]
    fn windows_common_program_files_defaults_replace_empty_values() {
        let mut env = vec![
            (OsString::from("CommonProgramFiles"), OsString::new()),
            (
                OsString::from("CommonProgramFiles(x86)"),
                OsString::from(" "),
            ),
            (
                OsString::from("CommonProgramW6432"),
                OsString::from(r"D:\Common Files"),
            ),
        ];

        fill_windows_common_program_files(&mut env);

        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| normalize_key(key) == normalize_key(OsStr::new(name)))
                .map(|(_, value)| value.to_string_lossy().into_owned())
        };
        assert_eq!(
            get("CommonProgramFiles").as_deref(),
            Some(r"C:\Program Files\Common Files")
        );
        assert_eq!(
            get("CommonProgramFiles(x86)").as_deref(),
            Some(r"C:\Program Files (x86)\Common Files")
        );
        assert_eq!(
            get("CommonProgramW6432").as_deref(),
            Some(r"D:\Common Files")
        );
    }

    #[test]
    fn mcp_env_allowlist_includes_python_bootstrap_keys() {
        for key in [
            "PYTHONPATH",
            "PYTHONHOME",
            "VIRTUAL_ENV",
            "PIPX_HOME",
            "PIPX_BIN_DIR",
            "POETRY_HOME",
        ] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should include python bootstrap key {key}"
            );
        }
    }

    #[test]
    fn mcp_env_allowlist_includes_uv_prefixed_keys() {
        for key in ["UV_CACHE_DIR", "UV_INDEX_URL", "UV_PYTHON"] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should include uv prefixed key {key}"
            );
        }
    }

    #[test]
    fn mcp_env_allowlist_includes_ca_bundles() {
        for key in [
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "REQUESTS_CA_BUNDLE",
            "CURL_CA_BUNDLE",
        ] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should include CA bundle key {key}"
            );
        }
    }

    #[test]
    fn mcp_env_allowlist_includes_aws_profile_and_region_selectors() {
        for key in [
            "AWS_PROFILE",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "AWS_CONFIG_FILE",
            "AWS_SHARED_CREDENTIALS_FILE",
        ] {
            assert!(
                is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist should include non-secret AWS selector {key}"
            );
        }
    }

    #[test]
    fn mcp_env_allowlist_excludes_secrets_and_creds() {
        for key in [
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SESSION_TOKEN",
            "AWS_SECURITY_TOKEN",
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "DEEPSEEK_API_KEY",
            "SLACK_TOKEN",
            "MY_RANDOM_SECRET",
        ] {
            assert!(
                !is_allowed_mcp_env_key(OsStr::new(key)),
                "MCP allowlist must NOT include {key}"
            );
        }
    }

    #[test]
    fn sanitized_mcp_env_passes_through_node_bootstrap() {
        let _guard = crate::test_support::lock_test_env();
        let _nvm_dir = EnvVarGuard::set("NVM_DIR", "/tmp/test-nvm");

        let env = sanitized_mcp_env(std::iter::empty::<(OsString, OsString)>());

        let nvm_dir = env
            .iter()
            .find(|(key, _)| normalize_key(key) == "NVM_DIR")
            .map(|(_, value)| value.clone());
        assert_eq!(nvm_dir, Some(OsString::from("/tmp/test-nvm")));
    }

    #[test]
    fn sanitized_mcp_env_drops_unrelated_secret_like_values() {
        let _guard = crate::test_support::lock_test_env();
        let _secret = EnvVarGuard::set("DEEPSEEK_MCP_TEST_SECRET", "should-not-leak");

        let env = sanitized_mcp_env(std::iter::empty::<(OsString, OsString)>());

        assert!(
            env.iter().all(|(key, _)| key != "DEEPSEEK_MCP_TEST_SECRET"),
            "MCP env should not pass arbitrary parent vars"
        );
    }

    #[test]
    fn reviewed_plugin_mcp_env_requires_explicit_proxy_provenance() {
        let _guard = crate::test_support::lock_test_env();
        let synthetic_proxy = format!(
            "{}://{}:{}@{}",
            "http", "fixture-user", "fixture-password", "127.0.0.1:9"
        );
        let _proxy = EnvVarGuard::set("HTTP_PROXY", synthetic_proxy);

        let ambient = sanitized_plugin_mcp_env(std::iter::empty::<(OsString, OsString)>());
        let explicit = sanitized_plugin_mcp_env([("HTTP_PROXY", "http://proxy.invalid")]);

        assert!(
            ambient
                .iter()
                .all(|(key, _)| normalize_key(key) != "HTTP_PROXY"),
            "reviewed plugins must not inherit a credential-capable proxy URL"
        );
        assert!(explicit.iter().any(|(key, value)| {
            normalize_key(key) == "HTTP_PROXY" && value == "http://proxy.invalid"
        }));
    }

    #[test]
    fn reviewed_plugin_mcp_env_keeps_desktop_routing_without_secret_namespaces() {
        let desktop = [
            ("DISPLAY", ":1"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
            ("XAUTHORITY", "/run/user/1000/.Xauthority"),
        ];
        let unexpected = [
            ("OPENAI_API_KEY", "provider-fixture"),
            ("XDG_PRIVATE_TOKEN", "xdg-fixture"),
            ("DBUS_PRIVATE_TOKEN", "dbus-fixture"),
            ("CODEWHALE_CU_APP_BUNDLE", "/stale/helper.app"),
        ];
        let child = sanitized_plugin_mcp_env_from(
            desktop.into_iter().chain(unexpected),
            std::iter::empty::<(&str, &str)>(),
        );
        for (key, value) in desktop {
            assert!(
                child
                    .iter()
                    .any(|(found, content)| found == key && content == value)
            );
        }
        for (key, _) in unexpected {
            assert!(child.iter().all(|(found, _)| found != key));
        }
    }

    #[test]
    fn child_env_allowlist_keeps_build_config_but_not_credentials() {
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_JOBS",
            "CARGO_INCREMENTAL",
            "RUSTFLAGS",
            "RUST_BACKTRACE",
            "RUST_LOG",
            "VIRTUAL_ENV",
            "JAVA_HOME",
            "GOPATH",
            "NVM_DIR",
            "NODE_OPTIONS",
            "SSL_CERT_FILE",
            "REQUESTS_CA_BUNDLE",
        ] {
            assert!(
                is_allowed_parent_env_key(OsStr::new(key)),
                "child env should keep {key}"
            );
        }
        for key in [
            "CARGO_REGISTRY_TOKEN",
            "CARGO_REGISTRIES_PRIVATE_TOKEN",
            "CARGO_REGISTRIES_PRIVATE_CREDENTIAL_PROVIDER",
            "CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS",
            "CARGO_SOME_SECRET",
            "CARGO_SIGNING_KEY",
            "DATABASE_URL",
            "OPENAI_API_KEY",
        ] {
            assert!(
                !is_allowed_parent_env_key(OsStr::new(key)),
                "child env must not keep {key}"
            );
        }
    }

    #[test]
    fn proxy_userinfo_is_removed_before_reaching_a_child() {
        for (input, expected) in [
            ("http://user:secret@proxy:3128", "http://proxy:3128"),
            (
                "http://user:secret@proxy:3128/path?x=1",
                "http://proxy:3128/path?x=1",
            ),
            ("socks5h://u:p@w@10.0.0.1:1080", "socks5h://10.0.0.1:1080"),
            ("user:secret@proxy:3128", "proxy:3128"),
            ("http://proxy:3128", "http://proxy:3128"),
            ("http://proxy:3128/a@b", "http://proxy:3128/a@b"),
        ] {
            assert_eq!(
                strip_proxy_userinfo(OsStr::new(input)),
                Some(OsString::from(expected)),
                "{input}"
            );
        }
    }

    #[test]
    fn sanitized_child_env_strips_proxy_credentials() {
        let _guard = crate::test_support::lock_test_env();
        let _https = EnvVarGuard::set(
            "HTTPS_PROXY",
            "http://fixture-user:fixture-pass@proxy.invalid:3128",
        );
        let _cargo = EnvVarGuard::set(
            "CARGO_HTTP_PROXY",
            "http://fixture-user:fixture-pass@proxy.invalid:3128",
        );
        let _no_proxy = EnvVarGuard::set("NO_PROXY", "localhost");

        let env = sanitized_child_env(std::iter::empty::<(OsString, OsString)>());
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| normalize_key(key) == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            get("HTTPS_PROXY"),
            Some(OsString::from("http://proxy.invalid:3128"))
        );
        assert_eq!(
            get("CARGO_HTTP_PROXY"),
            Some(OsString::from("http://proxy.invalid:3128"))
        );
        assert_eq!(get("NO_PROXY"), Some(OsString::from("localhost")));
        assert!(
            env.iter()
                .all(|(_, value)| !value.to_string_lossy().contains("fixture-pass"))
        );
        // Explicit call-site overrides are trusted and kept as given.
        let explicit = sanitized_child_env([("HTTPS_PROXY", "http://a:b@proxy.invalid:1")]);
        assert!(explicit.iter().any(|(key, value)| {
            normalize_key(key) == "HTTPS_PROXY" && value == "http://a:b@proxy.invalid:1"
        }));
    }

    #[test]
    fn sanitized_child_env_drops_parent_secret_like_values() {
        let _guard = crate::test_support::lock_test_env();
        let _secret = EnvVarGuard::set("DEEPSEEK_CHILD_ENV_TEST_SECRET", "parent-secret");

        let env = sanitized_child_env(std::iter::empty::<(OsString, OsString)>());

        assert!(
            env.iter()
                .all(|(key, _)| key != "DEEPSEEK_CHILD_ENV_TEST_SECRET")
        );
    }

    #[test]
    fn sanitized_child_env_drops_rustc_wrapper_under_os_sandbox() {
        let _guard = crate::test_support::lock_test_env();
        let _wrapper = EnvVarGuard::set("RUSTC_WRAPPER", "sccache");

        let unsandboxed = sanitized_child_env(std::iter::empty::<(OsString, OsString)>());
        assert!(
            unsandboxed
                .iter()
                .any(|(key, value)| normalize_key(key) == "RUSTC_WRAPPER" && value == "sccache"),
            "unsandboxed children keep RUSTC_WRAPPER"
        );

        let sandboxed = sanitized_child_env([(
            OsString::from("CODEWHALE_SANDBOX"),
            OsString::from("seatbelt"),
        )]);
        assert!(
            sandboxed.iter().any(
                |(key, value)| normalize_key(key) == "CODEWHALE_SANDBOX" && value == "seatbelt"
            ),
            "sandbox marker must survive"
        );
        assert!(
            sandboxed
                .iter()
                .all(|(key, _)| normalize_key(key) != "RUSTC_WRAPPER"),
            "OS sandbox children must not keep RUSTC_WRAPPER"
        );
    }

    #[test]
    fn explicit_child_env_values_win_over_parent_allowlist() {
        // The real parent PATH is enough to prove the override wins; setting
        // PATH here would break every concurrent test that spawns a program.
        assert_ne!(
            std::env::var_os("PATH"),
            Some(OsString::from("/explicit/bin"))
        );

        let env = sanitized_child_env([(OsString::from("PATH"), OsString::from("/explicit/bin"))]);

        let path = env
            .iter()
            .find(|(key, _)| normalize_key(key) == "PATH")
            .map(|(_, value)| value);
        assert_eq!(path, Some(&OsString::from("/explicit/bin")));
    }

    #[test]
    fn sanitized_child_env_preserves_windows_toolchain_vars() {
        let _guard = crate::test_support::lock_test_env();
        let _lib = EnvVarGuard::set("LIB", r"C:\sdk\lib");
        let _include = EnvVarGuard::set("INCLUDE", r"C:\sdk\include");
        let _sdk = EnvVarGuard::set("WINDOWSSDKDIR", r"C:\sdk");

        let env = sanitized_child_env(std::iter::empty::<(OsString, OsString)>());

        assert!(
            env.iter()
                .any(|(key, value)| key == "LIB" && value == r"C:\sdk\lib"),
            "child env should preserve LIB"
        );
        assert!(
            env.iter()
                .any(|(key, value)| key == "INCLUDE" && value == r"C:\sdk\include"),
            "child env should preserve INCLUDE"
        );
        assert!(
            env.iter()
                .any(|(key, value)| key == "WINDOWSSDKDIR" && value == r"C:\sdk"),
            "child env should preserve WINDOWSSDKDIR"
        );
    }
}
