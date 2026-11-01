//! Cross-process admission for expensive local commands.
//!
//! Fleet and Workflow workers execute in separate Codewhale processes, so an
//! in-process semaphore cannot protect the host. Heavy shell commands instead
//! take one of a small number of filesystem-backed permits under
//! `CODEWHALE_HOME`. The default of two permits is deliberately conservative
//! for the 36 GiB laptop class from #4864.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use fd_lock::{RwLock, RwLockWriteGuard};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_HEAVY_COMMAND_LIMIT: usize = 2;
const MAX_HEAVY_COMMAND_LIMIT: usize = 16;
const ADMISSION_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// When the host free-RAM fraction drops to/below these thresholds the
/// effective heavy-command admission limit tightens so a saturated host stops
/// admitting new link graphs (#4864 req 7). Values are deliberately generous
/// because the measurement is advisory, not authoritative.
const CONSTRAINED_FREE_FRACTION: f64 = 0.30;
const CRITICAL_FREE_FRACTION: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandExpense {
    Normal,
    Heavy,
}

/// Measured host memory pressure used to tighten heavy-command admission.
///
/// `Unknown` means "could not be measured"; admission then fails open (uses the
/// configured limit) rather than risk blocking on an unmeasurable host. This
/// keeps the gate safe on any CI runner where the probe is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPressure {
    Unknown,
    Nominal,
    Constrained,
    Critical,
}

/// Pluggable memory probe so the admission policy is unit-testable without
/// having to drive the host into real memory pressure.
pub trait MemoryProbe: Send + Sync {
    /// Free-RAM fraction in `0.0..=1.0`, or `None` when it cannot be measured.
    fn free_fraction(&self) -> Option<f64>;
}

struct HostMemoryProbe;

impl MemoryProbe for HostMemoryProbe {
    fn free_fraction(&self) -> Option<f64> {
        host_memory_free_fraction()
    }
}

fn classify_memory_pressure(free_fraction: Option<f64>) -> MemoryPressure {
    match free_fraction {
        None => MemoryPressure::Unknown,
        Some(fraction) if fraction <= CRITICAL_FREE_FRACTION => MemoryPressure::Critical,
        Some(fraction) if fraction <= CONSTRAINED_FREE_FRACTION => MemoryPressure::Constrained,
        Some(_) => MemoryPressure::Nominal,
    }
}

/// Effective admission limit after applying host memory pressure. `Critical`
/// yields zero so queued heavy commands wait for the host to recover instead of
/// snowballing; `Constrained` halves the budget (never below one).
fn effective_admission_limit(configured: usize, pressure: MemoryPressure) -> usize {
    match pressure {
        MemoryPressure::Critical => 0,
        MemoryPressure::Constrained => configured.div_ceil(2).max(1),
        MemoryPressure::Nominal | MemoryPressure::Unknown => configured,
    }
}

#[derive(Debug)]
struct HeavyPermitSlot {
    _guard: RwLockWriteGuard<'static, File>,
    // The guard borrows the lock. Keeping the boxed lock here makes that
    // allocation outlive the guard; field drop order is guard, then lock.
    _lock: Box<RwLock<File>>,
}

/// A held cross-process heavy-command permit.
#[derive(Debug)]
pub struct HeavyCommandPermit {
    _slot: HeavyPermitSlot,
    queued_for: Duration,
    limit: usize,
    memory_pressure: MemoryPressure,
}

impl HeavyCommandPermit {
    pub fn queued_for(&self) -> Duration {
        self.queued_for
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn memory_pressure(&self) -> MemoryPressure {
        self.memory_pressure
    }
}

pub fn infer_command_expense(command: &str) -> CommandExpense {
    let heavy = command
        .split(['\n', '\r', ';', '|', '&'])
        .any(segment_is_heavy);

    if heavy {
        CommandExpense::Heavy
    } else {
        CommandExpense::Normal
    }
}

/// Commands that run another command from their arguments. The classifier
/// looks through them (#6560 D01-11): `sudo cargo build`, `timeout 600
/// cargo test`, `xargs cargo check` and `bash -c "cargo build"` compile
/// exactly like the bare command and need the same permit. This is resource
/// admission, not a security boundary: shell policy still judges the whole
/// command.
const COMMAND_WRAPPERS: &[&str] = &[
    "sudo",
    "doas",
    "env",
    "nice",
    "nohup",
    "time",
    "timeout",
    "gtimeout",
    "xargs",
    "command",
    "exec",
    "stdbuf",
    "ionice",
    "caffeinate",
    "watch",
    "eval",
    "bash",
    "sh",
    "zsh",
    "dash",
];

fn segment_is_heavy(segment: &str) -> bool {
    let tokens: Vec<String> = segment
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(['"', '\'', '(', ')', '{', '}', '`', '$'])
                .to_string()
        })
        .collect();
    let stem = |token: &str| {
        Path::new(token)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let Some(mut index) = tokens
        .iter()
        .position(|token| !token.is_empty() && !token.contains('=') && token != "env")
    else {
        return false;
    };
    if COMMAND_WRAPPERS.contains(&stem(&tokens[index]).as_str()) {
        // Wrapper options and their values (`-u user`, `600`, `-n1`) sit
        // between the wrapper and the wrapped command; take the first
        // compiler that follows.
        let Some(offset) = tokens[index + 1..].iter().position(|token| {
            matches!(
                stem(token).as_str(),
                "cargo" | "rustc" | "dev-test" | "dev-cargo"
            )
        }) else {
            return false;
        };
        index += 1 + offset;
    }
    let executable = stem(&tokens[index]);
    if matches!(executable.as_str(), "dev-test" | "dev-cargo") {
        return true;
    }
    if !matches!(executable.as_str(), "cargo" | "rustc") {
        return false;
    }
    if executable == "rustc" {
        return true;
    }
    tokens[index + 1..]
        .iter()
        .map(|arg| arg.trim().to_ascii_lowercase())
        .find(|arg| !arg.is_empty() && !arg.starts_with(['-', '+']) && !arg.contains('='))
        .is_some_and(|subcommand| {
            matches!(
                subcommand.as_str(),
                "build" | "test" | "check" | "clippy" | "rustc"
            )
        })
}

pub async fn acquire_heavy_command_permit(
    command: &str,
    cancel: Option<&CancellationToken>,
) -> Result<Option<HeavyCommandPermit>> {
    if infer_command_expense(command) == CommandExpense::Normal {
        return Ok(None);
    }

    let limit = configured_heavy_command_limit();
    let root = admission_root();
    let probe = HostMemoryProbe;
    acquire_heavy_command_permit_at(&root, limit, cancel, &probe)
        .await
        .map(Some)
}

async fn acquire_heavy_command_permit_at(
    root: &Path,
    limit: usize,
    cancel: Option<&CancellationToken>,
    probe: &dyn MemoryProbe,
) -> Result<HeavyCommandPermit> {
    tokio::fs::create_dir_all(root)
        .await
        .with_context(|| format!("creating resource admission directory {}", root.display()))?;
    let started = Instant::now();

    loop {
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Err(anyhow!(
                "heavy command canceled while queued for resource admission"
            ));
        }
        // Re-measure each iteration: under memory pressure the effective limit
        // tightens so a saturated host stops admitting new heavy link graphs
        // (#4864 req 7). Critical pressure yields zero slots, so the command
        // waits for recovery instead of snowballing.
        let pressure = classify_memory_pressure(probe.free_fraction());
        let effective = effective_admission_limit(limit, pressure);
        for slot in 0..effective {
            let path = root.join(format!("heavy-{slot}.lock"));
            match try_lock_slot(&path) {
                Ok(Some(slot)) => {
                    return Ok(HeavyCommandPermit {
                        _slot: slot,
                        queued_for: started.elapsed(),
                        limit,
                        memory_pressure: pressure,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("acquiring heavy command permit {}", path.display())
                    });
                }
            }
        }
        tokio::time::sleep(ADMISSION_POLL_INTERVAL).await;
    }
}

fn try_lock_slot(path: &Path) -> io::Result<Option<HeavyPermitSlot>> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    let lock = Box::new(RwLock::new(file));
    let lock_ptr = Box::into_raw(lock);
    // SAFETY: `lock_ptr` remains allocated in `HeavyPermitSlot::_lock` for the
    // lifetime of `_guard`, and the guard is dropped before that box.
    let guard = match unsafe { (&mut *lock_ptr).try_write() } {
        Ok(guard) => guard,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            // SAFETY: no guard was created, so reclaim the allocation now.
            unsafe { drop(Box::from_raw(lock_ptr)) };
            return Ok(None);
        }
        Err(error) => {
            // SAFETY: no guard was created, so reclaim the allocation now.
            unsafe { drop(Box::from_raw(lock_ptr)) };
            return Err(error);
        }
    };
    // SAFETY: the allocation is owned exactly once by this box and is stable on
    // the heap even if `HeavyPermitSlot` moves.
    let lock = unsafe { Box::from_raw(lock_ptr) };
    // SAFETY: the boxed lock remains alive until after `_guard` is dropped.
    let guard = unsafe {
        std::mem::transmute::<RwLockWriteGuard<'_, File>, RwLockWriteGuard<'static, File>>(guard)
    };
    Ok(Some(HeavyPermitSlot {
        _guard: guard,
        _lock: lock,
    }))
}

fn configured_heavy_command_limit() -> usize {
    std::env::var("CODEWHALE_HEAVY_COMMAND_LIMIT")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(DEFAULT_HEAVY_COMMAND_LIMIT)
        .min(MAX_HEAVY_COMMAND_LIMIT)
}

fn admission_root() -> PathBuf {
    if let Some(home) = codewhale_paths::codewhale_home_override().ok().flatten() {
        return home.join("resource-admission");
    }
    if let Some(home) = codewhale_paths::user_home() {
        return home.join(".codewhale").join("resource-admission");
    }
    std::env::temp_dir().join("codewhale-resource-admission")
}

/// Host free-RAM fraction in `0.0..=1.0`, or `None` when it cannot be measured.
///
/// Each implementation shells out to a standard, always-present tool so no new
/// crate or build-feature dependency is introduced, and every error path returns
/// `None` so admission fails open (never blocks on an unmeasurable host). This
/// keeps the gate safe on any CI runner, while protecting the macOS dogfood host
/// and Linux/Windows machines where the tool exists.
#[cfg(target_os = "linux")]
fn host_memory_free_fraction() -> Option<f64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let total = parse_meminfo_kb(&meminfo, "MemTotal:")?;
    let available = parse_meminfo_kb(&meminfo, "MemAvailable:")?;
    (total > 0).then(|| (available as f64 / total as f64).clamp(0.0, 1.0))
}

#[cfg(target_os = "linux")]
fn parse_meminfo_kb(meminfo: &str, prefix: &str) -> Option<u64> {
    meminfo
        .lines()
        .find(|line| line.starts_with(prefix))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
}

#[cfg(target_os = "macos")]
fn host_memory_free_fraction() -> Option<f64> {
    let total = run_capture("/usr/sbin/sysctl", &["-n", "hw.memsize"])
        .and_then(|bytes| bytes.trim().parse::<u64>().ok())?;
    let page_size = run_capture("/usr/bin/pagesize", &[])?
        .trim()
        .parse::<u64>()
        .ok()?;
    let stats = run_capture("/usr/bin/vm_stat", &[])?;
    let free_pages = memory_pages_from_vm_stat(&stats, &["Pages free:", "Pages inactive:"])?;
    let free_bytes = free_pages.checked_mul(page_size)?;
    (total > 0).then(|| (free_bytes as f64 / total as f64).clamp(0.0, 1.0))
}

#[cfg(target_os = "macos")]
fn memory_pages_from_vm_stat(vm_stat: &str, prefixes: &[&str]) -> Option<u64> {
    let mut total = 0u64;
    for prefix in prefixes {
        let pages = vm_stat
            .lines()
            .find(|line| line.trim_start().starts_with(prefix))
            .and_then(|line| {
                line.split('.')
                    .nth(1)
                    .and_then(|rest| rest.trim().parse::<u64>().ok())
            })?;
        total = total.checked_add(pages)?;
    }
    Some(total)
}

#[cfg(windows)]
fn host_memory_free_fraction() -> Option<f64> {
    // `wmic` is deprecated but present on every supported Windows runner and
    // avoids adding a GlobalMemoryStatusEx build dependency. Fail open on error.
    let out = run_capture(
        "C:\\Windows\\System32\\wbem\\wmic.exe",
        &[
            "OS",
            "get",
            "FreePhysicalMemory,TotalVisibleMemorySize",
            "/value",
        ],
    )?;
    let free_kb = wmic_value(&out, "FreePhysicalMemory=")?;
    let total_kb = wmic_value(&out, "TotalVisibleMemorySize=")?;
    (total_kb > 0).then(|| (free_kb as f64 / total_kb as f64).clamp(0.0, 1.0))
}

#[cfg(windows)]
fn wmic_value(output: &str, key: &str) -> Option<u64> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(key))
        .and_then(|value| value.trim().parse::<u64>().ok())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn host_memory_free_fraction() -> Option<f64> {
    None
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Deterministic probe returning a fixed fraction so admission behavior is
    /// independent of the host running the test suite.
    struct StaticMemoryProbe(Option<f64>);
    impl MemoryProbe for StaticMemoryProbe {
        fn free_fraction(&self) -> Option<f64> {
            self.0
        }
    }

    const NOMINAL_PROBE: StaticMemoryProbe = StaticMemoryProbe(Some(0.9));

    #[test]
    fn whitespace_codewhale_home_uses_shared_user_home_for_admission_state() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("temporary root");
        let home = tmp.path().join("home");
        let userprofile = tmp.path().join("userprofile");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &userprofile);
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", " \t ");

        assert_eq!(
            admission_root(),
            home.join(".codewhale").join("resource-admission")
        );
    }

    #[test]
    fn memory_pressure_classification_and_effective_limit() {
        // Unknown (unmeasurable) fails open to the configured limit.
        assert_eq!(classify_memory_pressure(None), MemoryPressure::Unknown);
        assert_eq!(effective_admission_limit(2, MemoryPressure::Unknown), 2);
        assert_eq!(classify_memory_pressure(Some(0.9)), MemoryPressure::Nominal);
        assert_eq!(
            classify_memory_pressure(Some(0.30)),
            MemoryPressure::Constrained
        );
        assert_eq!(
            classify_memory_pressure(Some(0.15)),
            MemoryPressure::Critical
        );
        assert_eq!(
            classify_memory_pressure(Some(0.0)),
            MemoryPressure::Critical
        );
        assert_eq!(effective_admission_limit(4, MemoryPressure::Nominal), 4);
        assert_eq!(effective_admission_limit(4, MemoryPressure::Constrained), 2);
        assert_eq!(effective_admission_limit(1, MemoryPressure::Constrained), 1);
        assert_eq!(effective_admission_limit(4, MemoryPressure::Critical), 0);
    }

    #[test]
    fn infers_only_expensive_rust_compilation_commands() {
        for command in [
            "cargo test -p codewhale-tui shell::tests",
            "env CARGO_BUILD_JOBS=2 cargo build --workspace",
            "cargo check",
            "cargo clippy --all-targets",
            "/usr/bin/rustc src/main.rs",
            "printf ok && cargo rustc -- --emit=metadata",
            // #6560 D01-11: wrappers run the same compilation.
            "sudo cargo build --release",
            "sudo -u builder cargo test",
            "timeout 600 cargo test --workspace",
            "nice -n 10 cargo check",
            "bash -c \"cargo build\"",
            "sh -c 'cargo clippy --all-targets'",
            "git ls-files | xargs cargo check",
            "(cargo build)",
            "env RUSTFLAGS=-Dwarnings nohup cargo build",
        ] {
            assert_eq!(
                infer_command_expense(command),
                CommandExpense::Heavy,
                "{command}"
            );
        }
        for command in [
            "cargo fmt --check",
            "cargo metadata",
            "git status",
            "echo cargo test",
            "sudo cargo fmt",
            "timeout 5 git status",
            "bash -c \"ls\"",
            "xargs rm",
        ] {
            assert_eq!(
                infer_command_expense(command),
                CommandExpense::Normal,
                "{command}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cross_task_heavy_admission_never_exceeds_limit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..12 {
            let root = temp.path().to_path_buf();
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                let _permit = acquire_heavy_command_permit_at(&root, 2, None, &NOMINAL_PROBE)
                    .await
                    .expect("permit");
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for task in tasks {
            task.await.expect("admission task");
        }

        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn queued_admission_observes_cancellation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _held = acquire_heavy_command_permit_at(temp.path(), 1, None, &NOMINAL_PROBE)
            .await
            .expect("initial permit");
        let cancel = CancellationToken::new();
        let wait_cancel = cancel.clone();
        let root = temp.path().to_path_buf();
        let waiter = tokio::spawn(async move {
            acquire_heavy_command_permit_at(&root, 1, Some(&wait_cancel), &NOMINAL_PROBE).await
        });

        tokio::time::sleep(Duration::from_millis(75)).await;
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("bounded cancellation")
            .expect("waiter task")
            .expect_err("queued command must cancel");
        assert!(error.to_string().contains("canceled while queued"));
    }

    #[tokio::test]
    async fn critical_memory_pressure_queues_without_admitting() {
        let temp = tempfile::tempdir().expect("tempdir");
        // Critical pressure -> zero effective slots -> the command cannot be
        // admitted and must observe cancellation rather than spin forever.
        let critical = StaticMemoryProbe(Some(0.05));
        let cancel = CancellationToken::new();
        let wait_cancel = cancel.clone();
        let root = temp.path().to_path_buf();
        let waiter = tokio::spawn(async move {
            acquire_heavy_command_permit_at(&root, 2, Some(&wait_cancel), &critical).await
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("bounded cancellation")
            .expect("waiter task")
            .expect_err("critical-pressure command must not be admitted");
        assert!(error.to_string().contains("canceled while queued"));
    }

    #[test]
    fn host_memory_probe_is_fail_safe() {
        // On any supported host the probe either measures a plausible fraction
        // or admits it cannot; it must never panic or return an out-of-range.
        if let Some(fraction) = host_memory_free_fraction() {
            assert!(
                (0.0..=1.0).contains(&fraction),
                "measured free fraction out of range: {fraction}"
            );
        }
    }

    /// Opt-in real cargo acceptance (#4864 req 8): drive an actual heavy-class
    /// cargo invocation through the admission path end to end. `cargo check
    /// --version` classifies as heavy (subcommand `check`) but exits instantly,
    /// so this is fast. Gated behind an env var so CI never runs a real cargo.
    #[cfg(unix)]
    #[tokio::test]
    async fn real_cargo_command_is_admitted_and_released() {
        if std::env::var_os("CODEWHALE_RESOURCE_ADMISSION_RUST_ACCEPTANCE").is_none() {
            tracing::info!(
                "skipping opt-in real-rust admission acceptance; \
                 set CODEWHALE_RESOURCE_ADMISSION_RUST_ACCEPTANCE=1 to run"
            );
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let permit = acquire_heavy_command_permit_at(temp.path(), 2, None, &NOMINAL_PROBE)
            .await
            .expect("heavy permit for real cargo");
        assert_eq!(permit.limit(), 2);
        assert_eq!(permit.memory_pressure(), MemoryPressure::Nominal);
        let cargo = std::process::Command::new("cargo")
            .arg("check")
            .arg("--version")
            .output()
            .expect("run cargo");
        assert!(cargo.status.success(), "cargo check --version failed");
        drop(permit);
        let _again = acquire_heavy_command_permit_at(temp.path(), 2, None, &NOMINAL_PROBE)
            .await
            .expect("re-acquire after release");
    }
}
