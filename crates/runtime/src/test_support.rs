//! Shared test-only helpers.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub use codewhale_util::test_env_lock::{
    EnvScopeMembership, EnvScopeTicket, TestEnvLock, current_env_scope_generation,
    current_thread_holds_test_env_lock, env_scope_ticket, join_env_scope, lock_test_env,
    with_test_env_lock, with_test_env_lock_if_uncontended,
};

/// Mark `workspace` trusted in the test's config, creating it first so the
/// trust key is the canonical path. Repository-supplied commands and skills
/// load only in a trusted workspace.
pub fn trust_workspace(workspace: &Path) {
    std::fs::create_dir_all(workspace).expect("create test workspace");
    crate::config::save_workspace_trust(workspace).expect("trust test workspace");
}

/// Process-wide state root for unit tests that do not intentionally provide an
/// explicit config/settings path.
///
/// The production fallback is the user's real home. That is useful at runtime
/// and unsafe in a parallel test binary: an unguarded save can otherwise read
/// or overwrite the developer's config. Tests that exercise path precedence
/// still hold [`lock_test_env`] and provide explicit temporary environment
/// values; every other test is confined here — enforced by
/// [`guarded_environment_provides_state_paths`], not assumed.
pub fn isolated_test_state_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codewhale-tui-test-state-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap_or_else(|error| {
            panic!(
                "failed to create isolated unit-test state root {}: {error}",
                root.display()
            )
        });
        // Match resolvers that canonicalize their root (macOS aliases /var to
        // /private/var). Every fence must name the same physical test store.
        root.canonicalize().expect("canonical test state root")
    })
}

/// Where the calling test's state should live when it has not sealed the
/// environment itself.
///
/// Two different callers land here. A test that never took [`lock_test_env`]
/// gets the shared root, exactly as before — those tests already coexist there
/// under [`with_test_state_io_lock`]. A test that *holds* the lock but sealed
/// nothing gets a private directory instead: before #5359 it resolved the
/// developer's real home, so it has never shared the process root, and several
/// such tests run full settings transactions. Adding that traffic to the shared
/// root pushed the transaction lock past its deadline and hung unrelated
/// `config_command_*` tests. Keep them isolated from the developer *and* from
/// each other.
pub fn unsealed_test_state_root() -> PathBuf {
    let shared = isolated_test_state_root();
    if !current_thread_holds_test_env_lock() {
        return shared.to_path_buf();
    }
    // libtest runs each test in a fresh thread. Keep one root for that thread:
    // a settings save resolves its path more than once, while different tests
    // must not inherit each other's files.
    HOLDER_ROOT.with(|cached| {
        cached
            .get_or_init(|| {
                let root = shared.join(format!("env-holder-{:?}", std::thread::current().id()));
                std::fs::create_dir_all(&root).unwrap_or_else(|error| {
                    panic!(
                        "failed to create per-holder test state root {}: {error}",
                        root.display()
                    )
                });
                root
            })
            .clone()
    })
}

thread_local! {
    static HOLDER_ROOT: OnceLock<PathBuf> = const { OnceLock::new() };
}

/// Where a state resolver must put `name` instead of the user's home: the
/// isolated test root when no test has sealed the home (see [`home_is_sealed`]),
/// `None` when one has and the resolver should follow the environment as
/// production does.
///
/// Every resolver that reaches `~/.codewhale` and is reachable from a test
/// needs this fence, because incidental callers (an `App` constructor, a
/// command dispatch) never think about the home at all. Call it first, under
/// `#[cfg(test)]`, so the production path stays byte-for-byte unchanged.
///
/// The directory is under the shared isolated root, not the per-holder one
/// [`unsealed_test_state_root`] hands to lock holders: these resolvers are
/// reached from worker threads as well as the test thread, and a test must see
/// one directory from both. The directory exists when this returns, so callers
/// need no filesystem call of their own on the fenced branch.
pub fn unsealed_state_dir(name: impl AsRef<Path>) -> Option<PathBuf> {
    if home_is_sealed() {
        return None;
    }
    let dir = isolated_test_state_root().join(name);
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| {
        panic!(
            "failed to create the unsealed test state dir {}: {error}",
            dir.display()
        )
    });
    Some(dir)
}

fn state_io_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serialize read/merge/write operations against the process-wide isolated
/// test state root.
///
/// Path isolation protects the developer's files, but parallel tests still
/// share the same temporary files. Settings persistence is a multi-step
/// operation, so it needs this second barrier around the complete I/O
/// transaction rather than only around path resolution.
pub fn with_test_state_io_lock<T>(operation: impl FnOnce() -> T) -> T {
    let _guard = match state_io_lock().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    operation()
}

/// Build a test phase's future inside this call and box it (#6362).
///
/// In debug builds every inline `async {}` value gets a stack slot in the
/// enclosing poll frame the size of that future's whole state machine, and
/// the slots are never reused, so a body that awaits four phases inline
/// carries all four state machines on its own frame at once (measured at
/// 806 KiB for the runtime-store binding test). Constructing the phase here
/// leaves the caller holding a pointer, and the phase's own temporaries die
/// with its poll frame.
pub fn boxed_phase<'a, T, M, F>(
    make: M,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>
where
    M: FnOnce() -> F,
    F: std::future::Future<Output = T> + 'a,
{
    Box::pin(make())
}

/// Drive a test future on a thread with libtest's default 2 MiB stack,
/// whatever `RUST_MIN_STACK` says (#6362).
///
/// CI exports a 16 MiB `RUST_MIN_STACK` for every test thread, so a test
/// that only fits because of that export never learns it overflowed the
/// stack a contributor's plain `cargo test` gives it. The future is built on
/// the spawned thread (so it need not be `Send`) and pinned before
/// `block_on`, exactly as `#[tokio::test]` drives a current-thread runtime;
/// a panic inside propagates to the caller unchanged. An overflow still
/// aborts the process with "has overflowed its stack": that is the reported
/// symptom, not something this helper can turn into a panic.
pub fn block_on_default_test_stack<M, F, T>(make: M) -> T
where
    M: FnOnce() -> F + Send + 'static,
    F: std::future::Future<Output = T>,
    T: Send + 'static,
{
    const DEFAULT_TEST_THREAD_STACK: usize = 2 * 1024 * 1024;
    std::thread::Builder::new()
        .name("default-test-stack".into())
        .stack_size(DEFAULT_TEST_THREAD_STACK)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("current-thread test runtime");
            let future = make();
            tokio::pin!(future);
            runtime.block_on(future)
        })
        .expect("spawn the default-stack test thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Restore one environment variable when dropped.
///
/// Callers that mutate process-global environment variables must hold
/// [`lock_test_env`] until after this guard is dropped.
///
/// Every live guard is also recorded in [`guarded_env_keys`], so path
/// resolution can distinguish a test that deliberately redirected `HOME`
/// from one that merely holds the lock to serialize unrelated env access —
/// see [`guarded_environment_provides_state_paths`].
pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

fn guarded_env_keys() -> &'static Mutex<std::collections::HashMap<&'static str, usize>> {
    static KEYS: OnceLock<Mutex<std::collections::HashMap<&'static str, usize>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn register_guarded_env_key(key: &'static str) {
    let mut keys = match guarded_env_keys().lock() {
        Ok(keys) => keys,
        Err(poisoned) => poisoned.into_inner(),
    };
    *keys.entry(key).or_insert(0) += 1;
}

fn unregister_guarded_env_key(key: &'static str) {
    let mut keys = match guarded_env_keys().lock() {
        Ok(keys) => keys,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(count) = keys.get_mut(key) {
        *count -= 1;
        if *count == 0 {
            keys.remove(key);
        }
    }
}

static STATE_FROM_ENVIRONMENT: AtomicBool = AtomicBool::new(false);

pub fn resolve_state_from_environment() {
    STATE_FROM_ENVIRONMENT.store(true, Ordering::SeqCst);
}

pub fn state_from_environment() -> bool {
    STATE_FROM_ENVIRONMENT.load(Ordering::SeqCst)
}

/// Whether some live [`EnvVarGuard`] currently covers `key`.
pub fn env_var_currently_guarded(key: &str) -> bool {
    if state_from_environment() {
        return true;
    }
    match guarded_env_keys().lock() {
        Ok(keys) => keys.contains_key(key),
        Err(poisoned) => poisoned.into_inner().contains_key(key),
    }
}

/// Whether the calling test actually provided the state-path environment it
/// is about to resolve.
///
/// Holding [`lock_test_env`] alone is not that: many tests hold the lock only
/// to serialize access to unrelated variables (`TERM_PROGRAM`, API keys) and
/// have provided no temporary paths at all. Trusting the lock routed those
/// tests to the developer's real `~/.codewhale` state, which is exactly the
/// leak the isolated root exists to prevent (#5359). A test earns environment
/// resolution by holding the lock *and* either setting one of the explicit
/// override variables or redirecting `HOME`/`USERPROFILE` through
/// [`EnvVarGuard`].
pub fn guarded_environment_provides_state_paths() -> bool {
    if state_from_environment() {
        return true;
    }
    if !current_thread_holds_test_env_lock() {
        return false;
    }
    let guarded_path_is_present = |var: &str| {
        env_var_currently_guarded(var)
            && std::env::var_os(var)
                .is_some_and(|value| value.to_str().is_none_or(|text| !text.trim().is_empty()))
    };
    [
        "CODEWHALE_HOME",
        "CODEWHALE_CONFIG_PATH",
        "DEEPSEEK_CONFIG_PATH",
        "HOME",
        "USERPROFILE",
    ]
    .iter()
    .any(|var| guarded_path_is_present(var))
}

/// Whether the calling test's live seal pins the user's *home* — the root that
/// user state (sessions, snapshots, plugin bundles, logs) resolves under — as
/// opposed to merely redirecting a config file.
///
/// The owner and workers enrolled through [`join_env_scope`] follow the seal.
/// Unrelated parallel tests and workers without that scope use the isolated
/// root: following another test's environment would race its restoration to
/// the developer's real profile. Pass [`env_scope_ticket`] to workers that need
/// their owner's home.
///
/// Stricter than [`guarded_environment_provides_state_paths`], which also
/// accepts a guarded config-path override: such a test still resolves
/// `~/.codewhale` for everything else. `CODEWHALE_HOME` outranks `HOME` when it
/// is set, so a guarded `HOME` seals nothing while an unguarded
/// `CODEWHALE_HOME` is in the ambient environment.
pub fn home_is_sealed() -> bool {
    if state_from_environment() {
        return true;
    }
    if !current_thread_holds_test_env_lock() {
        return false;
    }
    let present = |var: &str| {
        std::env::var_os(var)
            .is_some_and(|value| value.to_str().is_none_or(|text| !text.trim().is_empty()))
    };
    if present("CODEWHALE_HOME") {
        return env_var_currently_guarded("CODEWHALE_HOME");
    }
    ["HOME", "USERPROFILE"]
        .iter()
        .any(|var| env_var_currently_guarded(var) && present(var))
}

impl EnvVarGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        debug_assert!(
            current_thread_holds_test_env_lock(),
            "EnvVarGuard::set({key}) requires lock_test_env()"
        );
        let previous = std::env::var_os(key);
        // SAFETY: callers hold the process-wide test env mutex.
        unsafe { std::env::set_var(key, value) };
        register_guarded_env_key(key);
        Self { key, previous }
    }

    pub fn remove(key: &'static str) -> Self {
        debug_assert!(
            current_thread_holds_test_env_lock(),
            "EnvVarGuard::remove({key}) requires lock_test_env()"
        );
        let previous = std::env::var_os(key);
        // SAFETY: callers hold the process-wide test env mutex.
        unsafe { std::env::remove_var(key) };
        register_guarded_env_key(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Withdraw the claim *before* restoring the value: a reader on another
        // thread that still saw the key as guarded would otherwise follow the
        // restored — ambient, possibly real — path for the instant in between.
        unregister_guarded_env_key(self.key);
        // SAFETY: callers hold the process-wide test env mutex until after this
        // guard is dropped.
        unsafe {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

/// Seal the user's home onto a directory the test owns, for the life of one
/// test.
///
/// This is the one way a test takes the user's state out of play. It pins
/// `HOME`, `USERPROFILE` and `CODEWHALE_HOME` through [`EnvVarGuard`] — so
/// [`guarded_environment_provides_state_paths`] recognises the seal — and
/// removes the aliases that would otherwise route around them
/// (`HOMEDRIVE`/`HOMEPATH`, `CODEWHALE_CONFIG_PATH`, `DEEPSEEK_CONFIG_PATH`,
/// `DEEPSEEK_HOME`). Pinning `HOME` alone is not a seal: an ambient
/// `CODEWHALE_HOME` takes precedence over it, so a test that "scoped" only
/// `HOME` still wrote the developer's real profile.
///
/// It holds the process-wide environment lock, which is not reentrant: keep
/// one seal per test, and build any fixture that takes the lock itself
/// (settings, diagnostics harnesses) after it. Anything that reads or writes
/// the user's state — instructions, skills, sessions, snapshots, settings,
/// credentials — then lands in the seal.
pub struct SealedHome {
    // Drop order matters: restore the environment, then delete the directory,
    // then release the lock.
    _vars: Vec<EnvVarGuard>,
    _dir: Option<tempfile::TempDir>,
    home: PathBuf,
    codewhale_home: PathBuf,
    _lock: TestEnvLock,
}

#[expect(clippy::new_without_default)]
impl SealedHome {
    /// Seal onto fresh, empty directories: `<tmp>/home` and
    /// `<tmp>/codewhale-home`.
    pub fn new() -> Self {
        let lock = lock_test_env();
        let dir = tempfile::TempDir::new().expect("sealed home tempdir");
        let home = dir.path().join("home");
        let codewhale_home = dir.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("sealed home dir");
        std::fs::create_dir_all(&codewhale_home).expect("sealed codewhale home dir");
        Self::pin(lock, Some(dir), home, codewhale_home)
    }

    /// Seal onto a home the test already owns: `HOME` is `home` and the
    /// Codewhale home is `home/.codewhale`, the layout an unset
    /// `CODEWHALE_HOME` resolves to. For tests whose fixtures are built under
    /// one tempdir that also plays the home.
    pub fn at(home: &Path) -> Self {
        let lock = lock_test_env();
        let codewhale_home = home.join(".codewhale");
        Self::pin(lock, None, home.to_path_buf(), codewhale_home)
    }

    fn pin(
        lock: TestEnvLock,
        dir: Option<tempfile::TempDir>,
        home: PathBuf,
        codewhale_home: PathBuf,
    ) -> Self {
        // `CODEWHALE_HOME` first: it is dropped first, so the seal never
        // outlives the override that outranks `HOME`.
        let vars = vec![
            EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home),
            EnvVarGuard::set("HOME", &home),
            EnvVarGuard::set("USERPROFILE", &home),
            EnvVarGuard::remove("HOMEDRIVE"),
            EnvVarGuard::remove("HOMEPATH"),
            EnvVarGuard::remove("CODEWHALE_CONFIG_PATH"),
            EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH"),
            EnvVarGuard::remove("DEEPSEEK_HOME"),
        ];
        Self {
            _vars: vars,
            _dir: dir,
            home,
            codewhale_home,
            _lock: lock,
        }
    }

    /// The sealed `HOME` / `USERPROFILE`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The sealed `CODEWHALE_HOME`.
    pub fn codewhale_home(&self) -> &Path {
        &self.codewhale_home
    }
}

/// Find the byte position of the first divergence between two strings,
/// returning a windowed view (`±32 bytes` around the divergence) so failures
/// in cache-prefix-stability tests show *which* bytes drifted, not just that
/// they did. Returns `None` when the strings are byte-identical.
pub fn first_divergence(a: &str, b: &str) -> Option<(usize, String, String)> {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let max = a_bytes.len().min(b_bytes.len());
    for i in 0..max {
        if a_bytes[i] != b_bytes[i] {
            let lo = i.saturating_sub(32);
            let a_hi = (i + 32).min(a_bytes.len());
            let b_hi = (i + 32).min(b_bytes.len());
            let a_ctx = String::from_utf8_lossy(&a_bytes[lo..a_hi]).into_owned();
            let b_ctx = String::from_utf8_lossy(&b_bytes[lo..b_hi]).into_owned();
            return Some((i, a_ctx, b_ctx));
        }
    }
    if a_bytes.len() != b_bytes.len() {
        return Some((
            max,
            format!("(len={})", a_bytes.len()),
            format!("(len={})", b_bytes.len()),
        ));
    }
    None
}

/// Assert two strings are byte-identical, panicking with a windowed diff
/// around the first divergence when they aren't. Used by the prefix-cache
/// stability harness (#263, #280) to pin construction surfaces that land in
/// DeepSeek's KV cache prefix.
#[track_caller]
pub fn assert_byte_identical(label: &str, a: &str, b: &str) {
    if let Some((pos, a_ctx, b_ctx)) = first_divergence(a, b) {
        panic!(
            "{label}: prompt construction is non-deterministic — first diff at byte {pos}\n\
             ── side A (±32B) ──\n{a_ctx:?}\n── side B (±32B) ──\n{b_ctx:?}",
        );
    }
}

pub fn read_shell_sentinel(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('"', "")
        .trim()
        .to_string()
}

/// Build a syntactically valid, non-secret JWT fixture without embedding a
/// high-entropy token-shaped literal in Git history.
pub fn future_test_jwt(label: &str) -> String {
    use base64::Engine as _;

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":9999999999}"#);
    format!("test.{payload}.{label}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_codewhale_home_is_not_a_test_seal() {
        let _lock = lock_test_env();
        let _ambient = EnvVarGuard::set("CODEWHALE_HOME", "/tmp/ambient-codewhale-home");
        unregister_guarded_env_key("CODEWHALE_HOME");

        let sealed = guarded_environment_provides_state_paths();

        register_guarded_env_key("CODEWHALE_HOME");
        assert!(!sealed, "ambient developer state must remain confined");
    }

    #[test]
    fn a_config_path_guard_or_a_home_shadowed_by_the_ambient_override_is_not_a_home_seal() {
        let _lock = lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        {
            // Redirecting the config file leaves `~/.codewhale` in play.
            let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", dir.path().join("config.toml"));
            let _no_home_override = EnvVarGuard::remove("CODEWHALE_HOME");
            assert!(guarded_environment_provides_state_paths());
            assert!(!home_is_sealed());
        }
        {
            // `CODEWHALE_HOME` outranks `HOME`: guarding only `HOME` while an
            // ambient `CODEWHALE_HOME` is set seals nothing.
            let _ambient = EnvVarGuard::set("CODEWHALE_HOME", dir.path().join("ambient"));
            unregister_guarded_env_key("CODEWHALE_HOME");
            let _home = EnvVarGuard::set("HOME", dir.path());
            let sealed = home_is_sealed();
            register_guarded_env_key("CODEWHALE_HOME");
            assert!(!sealed, "an ambient CODEWHALE_HOME shadows a guarded HOME");
        }
        {
            let _no_home_override = EnvVarGuard::remove("CODEWHALE_HOME");
            let _home = EnvVarGuard::set("HOME", dir.path());
            assert!(home_is_sealed());
        }
    }

    #[test]
    fn removing_overrides_does_not_seal_the_ambient_home() {
        let _lock = lock_test_env();
        let _codewhale_home = EnvVarGuard::remove("CODEWHALE_HOME");
        let _codewhale_config = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _deepseek_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

        assert!(
            !guarded_environment_provides_state_paths(),
            "removing an override must not expose the developer's HOME"
        );
    }
}
