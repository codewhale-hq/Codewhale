//! End-to-end harness composing [`PtySession`] + [`Frame`].
//!
//! Tests build a [`Harness`] via [`Harness::builder`], drive the TUI with
//! [`Harness::send`] / [`Harness::paste`], poll the parsed terminal state
//! with [`Harness::wait_for`], and assert on [`Harness::frame`] /
//! filesystem state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

use super::{Frame, PtySession};

/// Scale a wait budget for shared CI runners.
///
/// PTY scenarios boot a real binary and wait on real terminal output, and the
/// budgets in the scenarios are tuned for a developer laptop running one test
/// at a time. CI runs the whole workspace suite on a shared runner, where the
/// same output can legitimately arrive several times later. Every budget this
/// scales is a deadline on a poll that returns as soon as the condition holds,
/// so a larger budget never slows a passing run — it only changes how long a
/// genuinely stuck scenario waits before failing. Local runs keep the tight
/// value so a real hang still surfaces quickly while developing.
pub fn ci_scaled(base: Duration) -> Duration {
    if std::env::var_os("CI").is_some() {
        base * 4
    } else {
        base
    }
}

pub struct Harness {
    pty: PtySession,
    frame: Frame,
    last_pump: Instant,
    cursor_query_tail: Vec<u8>,
    program: PathBuf,
    sealed_home: Option<PathBuf>,
    diagnostic_root: PathBuf,
    terminal_environment: String,
}

pub struct HarnessBuilder {
    program: PathBuf,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: HashMap<String, String>,
    rows: u16,
    cols: u16,
    clear_env: bool,
    seal_home: Option<PathBuf>,
}

impl HarnessBuilder {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        // PTY scenarios must never emit product telemetry merely because they
        // launch a real binary in a fresh HOME. Tests that explicitly exercise
        // the first-run disclosure can override this value on their builder.
        let env = HashMap::from([("CODEWHALE_TELEMETRY".to_string(), "0".to_string())]);
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env,
            rows: 40,
            cols: 120,
            clear_env: false,
            seal_home: None,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, p: impl Into<PathBuf>) -> Self {
        self.cwd = Some(p.into());
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }

    pub fn size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }

    pub fn clear_env(mut self) -> Self {
        self.clear_env = true;
        self
    }

    /// Point `$HOME` (and config/cache defaults) at a fresh dir so the spawned
    /// binary cannot read or mutate the developer's real user config.
    pub fn seal_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.seal_home = Some(home.into());
        self
    }

    pub fn spawn(self) -> Result<Harness> {
        let mut builder = PtySession::builder(&self.program)
            .args(self.args.iter().cloned())
            .size(self.rows, self.cols);
        if self.clear_env {
            builder = builder.clear_env(true);
        }
        if let Some(cwd) = self.cwd.as_deref() {
            builder = builder.cwd(cwd);
        }
        if let Some(home) = self.seal_home.as_deref() {
            std::fs::create_dir_all(home).context("create sealed HOME")?;
            let codewhale_config = home.join(".codewhale").join("config.toml");
            let deepseek_config = home.join(".deepseek").join("config.toml");
            builder = builder
                .env("HOME", home.to_string_lossy())
                .env("XDG_CONFIG_HOME", home.join(".config").to_string_lossy())
                .env("XDG_DATA_HOME", home.join(".local/share").to_string_lossy())
                .env("XDG_CACHE_HOME", home.join(".cache").to_string_lossy())
                .env("USERPROFILE", home.to_string_lossy())
                .env("CODEWHALE_CONFIG_PATH", codewhale_config.to_string_lossy())
                .env("DEEPSEEK_CONFIG_PATH", deepseek_config.to_string_lossy())
                // Sealing the filesystem is not enough on its own: the startup
                // Ollama probe reaches the developer's machine over loopback,
                // and adopting a live :11434 catalog rewrites the very launch
                // screen these suites wait for.
                .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1");
        }
        for (k, v) in &self.env {
            builder = builder.env(k, v);
        }

        let diagnostic_root = self
            .env
            .get("QA_PTY_DIAGNOSTICS_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("QA_PTY_DIAGNOSTICS_DIR").map(PathBuf::from))
            .unwrap_or_else(|| std::env::temp_dir().join("codewhale-pty-failures"));
        // Report only terminal capabilities, never the inherited environment
        // (which may contain developer credentials on unsealed scenarios).
        let terminal_environment = ["TERM", "COLORTERM", "NO_COLOR"]
            .into_iter()
            .map(|key| {
                let default = match key {
                    "TERM" => "xterm-256color",
                    "COLORTERM" => "truecolor",
                    _ => "<unset>",
                };
                let value = self
                    .env
                    .get(key)
                    .cloned()
                    .or_else(|| {
                        (!self.clear_env && key == "NO_COLOR")
                            .then(|| std::env::var(key).ok())
                            .flatten()
                    })
                    .unwrap_or_else(|| default.to_string());
                format!("{key}={value:?}")
            })
            .collect::<Vec<_>>()
            .join(" ");

        // Arm the stall watchdog before the child exists, so a spawn that wedges
        // is covered too. Idempotent per process.
        super::watchdog::arm();
        let pty = builder.spawn().context("spawn PtySession")?;
        let frame = Frame::new(self.rows, self.cols);
        Ok(Harness {
            pty,
            frame,
            last_pump: Instant::now(),
            cursor_query_tail: Vec::new(),
            program: self.program,
            sealed_home: self.seal_home,
            diagnostic_root,
            terminal_environment,
        })
    }
}

impl Harness {
    pub fn builder(program: impl Into<PathBuf>) -> HarnessBuilder {
        HarnessBuilder::new(program)
    }

    pub fn pid(&self) -> Option<u32> {
        self.pty.pid()
    }

    pub fn send(&mut self, bytes: impl AsRef<[u8]>) -> Result<()> {
        self.pty.write_bytes(bytes.as_ref())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.pty.resize(rows, cols)?;
        self.frame.resize(rows, cols);
        Ok(())
    }

    pub fn paste(&mut self, text: &str) -> Result<()> {
        self.pty.write_bytes(&super::paste::bracketed(text))
    }

    pub fn paste_unbracketed(&mut self, text: &str) -> Result<()> {
        self.pty.write_bytes(&super::paste::unbracketed(text))
    }

    /// Type a line of plain text and submit it: the text goes in as a
    /// bracketed paste — this harness's terminal advertises bracketed
    /// paste, and bulk text delivered as one zero-gap keystroke write is
    /// (correctly) paste-classified by the burst heuristic, whose Enter
    /// suppression window then swallows the submit — then a beat of idle,
    /// then Enter. The real `Event::Paste` also disarms the heuristic for
    /// the rest of the session, so later scripted typing behaves like a
    /// terminal with verified bracketed paste. Slash commands don't need
    /// this helper (Enter flushes buffered command text); plain prompts do.
    pub fn type_line(&mut self, text: &str) -> Result<()> {
        self.paste(text)?;
        self.wait_for_text(text, Duration::from_secs(10))?;
        std::thread::sleep(Duration::from_millis(150));
        self.pty.write_bytes(&super::keys::key::enter())
    }

    /// Pull whatever the child has written since last call into the frame
    /// parser. Returns `true` if any new bytes arrived.
    pub fn pump(&mut self) -> bool {
        // Every bounded wait loops through here, so this is the harness's
        // liveness signal for the stall watchdog.
        super::watchdog::progress("pump");
        let bytes = self.pty.drain();
        let any = !bytes.is_empty();
        if any {
            let cursor_queries =
                consume_cursor_position_queries(&mut self.cursor_query_tail, &bytes);
            self.frame.feed(&bytes);
            if cursor_queries > 0 {
                let (row, col) = self.frame.cursor();
                let response = format!("\x1b[{};{}R", row.saturating_add(1), col.saturating_add(1));
                for _ in 0..cursor_queries {
                    if self.pty.write_bytes(response.as_bytes()).is_err() {
                        break;
                    }
                }
            }
            self.last_pump = Instant::now();
        }
        any
    }

    /// Pump output and return the parsed frame. Convenience for asserts.
    pub fn frame(&mut self) -> &Frame {
        self.pump();
        &self.frame
    }

    /// Block (briefly sleeping) until `predicate(frame)` is true or `timeout`
    /// elapses. Pumps the PTY on each tick.
    pub fn wait_for<F>(&mut self, mut predicate: F, timeout: Duration) -> Result<()>
    where
        F: FnMut(&Frame) -> bool,
    {
        let budget = ci_scaled(timeout);
        let deadline = Instant::now() + budget;
        loop {
            self.pump();
            if predicate(&self.frame) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "wait_for timed out after {:?}.\n{}",
                    budget,
                    self.failure_diagnostics(budget)
                ));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    /// Wait for the literal substring to appear anywhere on the screen.
    pub fn wait_for_text(&mut self, needle: &str, timeout: Duration) -> Result<()> {
        let owned = needle.to_string();
        self.wait_for(move |f| f.contains(&owned), timeout)
    }

    /// Wait for the composer. A launch with no model key opens the provider
    /// picker first (#6566); tests that only need the composer close it with
    /// Esc, which returns to the composer without connecting anything.
    pub fn wait_for_composer(&mut self, timeout: Duration) -> Result<()> {
        const PICKER: &str = "Choose your model provider";
        const COMPOSER: &str = "Type a message";
        self.wait_for(|f| f.contains(PICKER) || f.contains(COMPOSER), timeout)?;
        if self.frame().contains(PICKER) {
            self.send(super::keys::key::esc())?;
            self.wait_for(|f| !f.contains(PICKER) && f.contains(COMPOSER), timeout)?;
        }
        Ok(())
    }

    /// Wait for stable output: no new bytes for `quiet_for` consecutive
    /// pump ticks, bounded by `max`. Useful for "let the UI settle".
    pub fn wait_for_idle(&mut self, quiet_for: Duration, max: Duration) -> Result<()> {
        // Only the ceiling scales: `quiet_for` is the definition of "settled",
        // not a budget, and stretching it would change what the test asserts.
        let budget = ci_scaled(max);
        let max_deadline = Instant::now() + budget;
        let mut quiet_since = Instant::now();
        loop {
            if self.pump() {
                quiet_since = Instant::now();
            }
            if quiet_since.elapsed() >= quiet_for {
                return Ok(());
            }
            if Instant::now() >= max_deadline {
                return Err(anyhow!(
                    "wait_for_idle: never settled within {:?}\n{}",
                    budget,
                    self.failure_diagnostics(budget)
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Capture evidence while the child and sealed HOME still exist. A blank
    /// parsed frame cannot distinguish no first draw from a later clear, and
    /// the TUI redirects stderr into its runtime log before drawing. This is
    /// diagnostics only: predicates, deadlines and teardown are unchanged.
    fn failure_diagnostics(&mut self, budget: Duration) -> String {
        self.pump();
        let transcript = self.pty.transcript();
        let pid = self.pty.pid();
        let exit = self.pty.wait_until(Instant::now());
        let signal = self.pty.signal().map(str::to_owned);
        let program = self.program.clone();
        let diagnostic_root = self.diagnostic_root.clone();
        let sealed_home = self.sealed_home.clone();
        let terminal_environment = self.terminal_environment.clone();
        let frame_dump = self.frame.debug_dump();
        let modes_dump = self.terminal_modes().debug_dump();
        // Failure evidence can hash a large binary and read redirected logs.
        // Keep that I/O on a dedicated worker, then join before fixture teardown
        // can remove the sealed HOME. Readiness itself has already timed out.
        let worker = std::thread::Builder::new()
            .name("qa-pty-diagnostics".into())
            .spawn(move || {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let destination = diagnostic_root
                    .join(format!("{}-{nonce}", pid.unwrap_or_default()));
                let mut report = format!(
                    "program={:?} host={}/{} pid={pid:?} observed_exit={exit:?} signal={signal:?} wait_budget={budget:?} parent_CI={} {}\nPTY bytes={}\n",
                    program,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    std::env::var_os("CI").is_some(),
                    terminal_environment,
                    transcript.len(),
                );
                report.push_str(&format!("diagnostic_worker={:?}\n", std::thread::current().name()));
                // Hash the running Linux executable when available; the launch path
                // may have been replaced by a concurrent build. Other hosts retain the
                // launch-path digest, explicitly labelled as such.
                let executable = pid
                    .map(|pid| PathBuf::from(format!("/proc/{pid}/exe")))
                    .filter(|path| path.exists())
                    .unwrap_or_else(|| program.clone());
                let digest = (|| -> std::io::Result<String> {
                    use sha2::{Digest, Sha256};
                    let mut file = std::fs::File::open(&executable)?;
                    let mut hasher = Sha256::new();
                    use std::io::Read;
                    let mut buffer = [0_u8; 64 * 1024];
                    loop {
                        let count = file.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        hasher.update(&buffer[..count]);
                    }
                    Ok(hasher
                        .finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect())
                })();
                report.push_str(&format!("executable={executable:?} sha256={digest:?}\n"));
                // /proc is local, read-only and cheap. No debugger dependency or
                // process environment is needed to locate a blocked Linux thread.
                if let Some(pid) = pid {
                    let process = PathBuf::from(format!("/proc/{pid}"));
                    for name in ["status", "wchan"] {
                        if let Ok(text) = std::fs::read_to_string(process.join(name)) {
                            report.push_str(&format!("process {name}:\n{text}\n"));
                        }
                    }
                    if let Ok(entries) = std::fs::read_dir(process.join("task")) {
                        for entry in entries.flatten().take(128) {
                            let thread = entry.path();
                            for name in ["comm", "wchan", "stack"] {
                                let text = std::fs::read_to_string(thread.join(name))
                                    .unwrap_or_else(|error| format!("unavailable: {error}"));
                                report
                                    .push_str(&format!("thread {:?} {name}: {text}\n", entry.file_name()));
                            }
                        }
                    }
                }
                let saved = (|| -> std::io::Result<()> {
                    let mut directories = std::fs::DirBuilder::new();
                    directories.recursive(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::DirBuilderExt;
                        directories.mode(0o700);
                    }
                    directories.create(&destination)?;
                    let write_private = |path: &Path, contents: &[u8]| -> std::io::Result<()> {
                        use std::io::Write;
                        let mut options = std::fs::OpenOptions::new();
                        options.write(true).create_new(true);
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::OpenOptionsExt;
                            options.mode(0o600);
                        }
                        options.open(path)?.write_all(contents)
                    };
                    write_private(&destination.join("pty.raw"), &transcript)?;
                    write_private(
                        &destination.join("frame.txt"),
                        frame_dump.as_bytes(),
                    )?;
                    // Copy only runtime logs under the explicitly sealed fixture;
                    // never walk a developer's HOME or copy configuration/credentials.
                    if let Some(home) = &sealed_home {
                        for relative in [".codewhale/logs", ".deepseek/logs"] {
                            let directory = home.join(relative);
                            if let Ok(entries) = std::fs::read_dir(&directory) {
                                for entry in entries.flatten() {
                                    let name = entry.file_name();
                                    let name_text = name.to_string_lossy();
                                    if !name_text.starts_with("tui-")
                                        || !name_text.ends_with(".log")
                                        || !entry.file_type()?.is_file()
                                    {
                                        continue;
                                    }
                                    let target = destination.join(relative).join(&name);
                                    directories.create(target.parent().unwrap())?;
                                    write_private(&target, &std::fs::read(entry.path())?)?;
                                    report.push_str(&format!("runtime stderr: {}\n", target.display()));
                                }
                            }
                        }
                    }
                    write_private(&destination.join("process.txt"), report.as_bytes())?;
                    Ok(())
                })();
                match saved {
                    Ok(()) => report.push_str(&format!("failure artifacts: {}\n", destination.display())),
                    Err(error) => report.push_str(&format!("failure artifact capture failed: {error}\n")),
                }
                let tail = &transcript[transcript.len().saturating_sub(4096)..];
                format!(
                    "{}{}\nPTY tail: {:?}\n{}",
                    frame_dump,
                    modes_dump,
                    String::from_utf8_lossy(tail),
                    report
                )
            });
        match worker {
            Ok(worker) => worker.join().unwrap_or_else(|_| {
                format!(
                    "{}\nfailure diagnostic worker panicked",
                    self.frame.debug_dump()
                )
            }),
            Err(error) => format!(
                "{}\nfailure diagnostic worker could not start: {error}",
                self.frame.debug_dump()
            ),
        }
    }

    /// Resolve the canonical executable, including an explicit QA_TUI_BIN selection.
    pub fn codewhale_binary() -> PathBuf {
        crate::binary::codewhale()
    }

    /// Best-effort cooperative shutdown.
    pub fn shutdown(self) -> Option<i32> {
        self.pty.shutdown(Duration::from_secs(2))
    }

    /// Wait for the child process to exit without sending it a signal.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<i32> {
        self.pty.wait_until(Instant::now() + ci_scaled(timeout))
    }

    pub fn debug_dump(&mut self) -> String {
        self.pump();
        self.frame.debug_dump()
    }

    /// Every byte the child has written, from spawn to now. Survives `pump`,
    /// so terminal-mode assertions stay valid after the frame parser has
    /// consumed the stream.
    pub fn transcript(&self) -> Vec<u8> {
        self.pty.transcript()
    }

    /// Replay the transcript into a [`TerminalModeLedger`].
    pub fn terminal_modes(&self) -> super::TerminalModeLedger {
        super::TerminalModeLedger::from_transcript(&self.transcript())
    }

    /// Frame dump plus terminal-mode ledger. Every bounded wait in the matrix
    /// fails with this rather than a bare `assertion failed`, so a CI timeout
    /// carries the screen *and* the control-stream state that produced it.
    pub fn diagnostics(&mut self) -> String {
        let modes = self.terminal_modes().debug_dump();
        format!("{}{modes}", self.debug_dump())
    }
}

const CURSOR_POSITION_QUERIES: [&[u8]; 2] = [b"\x1b[6n", b"\x1b[?6n"];

/// Consume terminal cursor-position queries from a chunked PTY output stream.
///
/// Crossterm asks the terminal for its cursor after Ratatui clears the screen.
/// A real terminal answers that DSR request; the QA PTY must do the same or the
/// child waits for crossterm's timeout before it can paint its first frame.
fn consume_cursor_position_queries(tail: &mut Vec<u8>, bytes: &[u8]) -> usize {
    let mut stream = std::mem::take(tail);
    stream.extend_from_slice(bytes);

    let mut count = 0;
    let mut index = 0;
    while index < stream.len() {
        if let Some(query) = CURSOR_POSITION_QUERIES
            .iter()
            .find(|query| stream[index..].starts_with(query))
        {
            count += 1;
            index += query.len();
        } else {
            index += 1;
        }
    }

    let max_tail = CURSOR_POSITION_QUERIES
        .iter()
        .map(|query| query.len().saturating_sub(1))
        .max()
        .unwrap_or(0)
        .min(stream.len());
    let keep = (1..=max_tail)
        .rev()
        .find(|&len| {
            CURSOR_POSITION_QUERIES
                .iter()
                .any(|query| len < query.len() && query.starts_with(&stream[stream.len() - len..]))
        })
        .unwrap_or(0);
    tail.extend_from_slice(&stream[stream.len() - keep..]);
    count
}

/// Construct a sealed-`HOME` workspace under a `tempfile::TempDir` so the
/// scenario can never read or mutate the developer's real config / skills.
pub fn make_sealed_workspace() -> Result<SealedWorkspace> {
    sealed_workspace_at(|tmp, _home| tmp.join("workspace"))
}

/// Like [`make_sealed_workspace`], but the workspace is `HOME/<project>`, so
/// the TUI displays it as `~/<project>` the way a real user's checkout reads
/// instead of an absolute tempdir path. Used for published website captures.
pub fn make_sealed_workspace_in_home(project: &str) -> Result<SealedWorkspace> {
    sealed_workspace_at(|_tmp, home| home.join(project))
}

fn sealed_workspace_at(workspace: impl FnOnce(&Path, &Path) -> PathBuf) -> Result<SealedWorkspace> {
    let tmp = tempfile::TempDir::new().context("tempdir")?;
    let home = tmp.path().join("home");
    let workspace = workspace(tmp.path(), &home);
    std::fs::create_dir_all(&workspace).context("mkdir workspace")?;
    std::fs::create_dir_all(home.join(".codewhale")).context("mkdir home/.codewhale")?;
    std::fs::create_dir_all(home.join(".deepseek")).context("mkdir home/.deepseek")?;
    let silent_notifications = "[notifications]\nmethod = \"off\"\ncompletion_sound = \"off\"\n";
    std::fs::write(
        home.join(".codewhale").join("config.toml"),
        silent_notifications,
    )
    .context("write silent CodeWhale PTY config")?;
    std::fs::write(
        home.join(".deepseek").join("config.toml"),
        silent_notifications,
    )
    .context("write silent legacy PTY config")?;
    Ok(SealedWorkspace {
        _tmp: tmp,
        workspace,
        home,
    })
}

pub struct SealedWorkspace {
    _tmp: tempfile::TempDir,
    pub workspace: PathBuf,
    pub home: PathBuf,
}

impl SealedWorkspace {
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub fn home(&self) -> &Path {
        &self.home
    }
    pub fn user_skills_dir(&self) -> PathBuf {
        self.home.join(".deepseek").join("skills")
    }
    pub fn trust_workspace(&self) -> Result<()> {
        let key = toml::Value::String(self.workspace.to_string_lossy().into_owned()).to_string();
        let entry = format!("\n[projects.{key}]\ntrust_level = \"trusted\"\n");
        for relative in [".codewhale/config.toml", ".deepseek/config.toml"] {
            let path = self.home.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut config = std::fs::read_to_string(&path).unwrap_or_default();
            config.push_str(&entry);
            std::fs::write(&path, config)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::consume_cursor_position_queries;

    #[cfg(unix)]
    #[test]
    fn failed_wait_retains_raw_output_and_sealed_stderr_before_teardown() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let logs = home.join(".codewhale/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("tui-fixture.log"),
            "startup stopped before first draw",
        )
        .unwrap();
        std::fs::write(home.join("secret.txt"), "must never be copied").unwrap();
        let artifacts = directory.path().join("evidence");
        let mut harness = super::Harness::builder("/bin/sh")
            .clear_env()
            .seal_home(&home)
            .env("QA_PTY_DIAGNOSTICS_DIR", artifacts.to_string_lossy())
            .args([
                "-c",
                r"printf '\033[Hdrawn then erased\033[2J\033[H'; sleep 3",
            ])
            .spawn()
            .unwrap();
        // Wait for the raw clear itself, without relying on a scheduling sleep.
        let deadline =
            std::time::Instant::now() + super::ci_scaled(std::time::Duration::from_secs(2));
        while !harness
            .transcript()
            .windows(4)
            .any(|part| part == b"\x1b[2J")
        {
            harness.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "fixture did not emit clear"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let error = harness
            .wait_for_text("never emitted", std::time::Duration::from_millis(20))
            .unwrap_err();
        assert!(error.to_string().contains("failure artifacts:"));
        assert!(!harness.frame().contains("drawn then erased"));
        let evidence = std::fs::read_dir(&artifacts)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        harness.shutdown();
        let raw = std::fs::read(evidence.join("pty.raw")).unwrap();
        assert!(raw.windows(17).any(|part| part == b"drawn then erased"));
        assert_eq!(
            std::fs::read_to_string(evidence.join(".codewhale/logs/tui-fixture.log")).unwrap(),
            "startup stopped before first draw"
        );
        assert!(!evidence.join("secret.txt").exists());
        assert!(
            !error
                .to_string()
                .contains("startup stopped before first draw")
        );
        use std::os::unix::fs::PermissionsExt;
        for path in [
            evidence.clone(),
            evidence.join("pty.raw"),
            evidence.join(".codewhale/logs/tui-fixture.log"),
        ] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        let process = std::fs::read_to_string(evidence.join("process.txt")).unwrap();
        assert!(process.contains("pid=Some("));
        assert!(process.contains("diagnostic_worker=Some(\"qa-pty-diagnostics\")"));
        assert!(process.contains("wait_budget="));
        assert!(process.contains("sha256=Ok("));
        assert!(process.contains("TERM=\"xterm-256color\""));
    }

    #[test]
    fn cursor_position_queries_survive_chunk_boundaries() {
        let mut tail = Vec::new();
        assert_eq!(
            consume_cursor_position_queries(&mut tail, b"before\x1b["),
            0
        );
        assert_eq!(consume_cursor_position_queries(&mut tail, b"6nafter"), 1);
        assert!(tail.is_empty());
    }

    #[test]
    fn cursor_position_queries_accept_standard_and_dec_forms() {
        let mut tail = Vec::new();
        assert_eq!(
            consume_cursor_position_queries(&mut tail, b"\x1b[6n\x1b[?6n"),
            2
        );
        assert!(tail.is_empty());
    }
}
