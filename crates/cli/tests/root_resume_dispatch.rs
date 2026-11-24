//! `codewhale --resume <id>`, `--resume=<id>`, `-r <id>` and `--session-id <id>`
//! are root dispatcher flags (the operations runbook advertises the first)
//! that forward to the in-process TUI's `--resume`. They used to be swallowed
//! by the trailing prompt positional and forwarded as `--prompt "--resume
//! <id>"`, which the TUI's own parser rejected (`error: unexpected argument
//! '--resume <id>' found`, exit 2). A non-TTY launch must now get past
//! argument parsing and fail only on the interactive-terminal contract, and
//! must not create session state under a sealed HOME on the way out.
//! Fresh-session and mouse flags exercise the same boundary: an unrecognized
//! `--fresh` previously swallowed the remaining flags as one prompt value.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn codewhale_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_codewhale").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = option_env!("CARGO_BIN_EXE_codewhale") {
        return PathBuf::from(path);
    }
    let mut path = std::env::current_exe().expect("current test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
    path
}

fn isolated_command(home: &Path, codewhale_home: &Path, workspace: &Path) -> Command {
    let mut command = Command::new(codewhale_binary());
    command
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CODEWHALE_HOME", codewhale_home)
        .env("CODEWHALE_SECRET_BACKEND", "file")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_TELEMETRY", "0")
        .env("NO_COLOR", "1")
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn root_session_flags_reach_the_tui_launch_contract_without_touching_state() {
    for argv in [
        &["--resume", "800596e6"][..],
        &["--resume=800596e6"][..],
        &["-r", "800596e6"][..],
        &["--session-id", "800596e6"][..],
        &["--continue"][..],
        &["--no-project-config", "--fresh", "--mouse-capture"][..],
        &["--no-project-config", "--mouse-capture", "--fresh"][..],
    ] {
        let fixture = TempDir::new().expect("fixture root");
        let home = fixture.path().join("sealed-home");
        let codewhale_home = fixture.path().join("sealed-codewhale-home");
        let workspace = fixture.path().join("workspace with spaces");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let output = isolated_command(&home, &codewhale_home, &workspace)
            .arg("--workspace")
            .arg(&workspace)
            .args(argv)
            .output()
            .expect("run codewhale");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !output.status.success(),
            "{argv:?} must not start an interactive TUI without a TTY:\n{stdout}\n{stderr}"
        );
        assert_ne!(
            output.status.code(),
            Some(2),
            "{argv:?} must parse at the root dispatcher (clap usage error):\n{stderr}"
        );
        assert!(
            !stderr.contains("unexpected argument") && !stderr.contains("--prompt <PROMPT>"),
            "{argv:?} must not be forwarded as a prompt:\n{stderr}"
        );
        assert!(
            stderr.contains("requires an interactive terminal"),
            "{argv:?} must fail on the TTY contract, not earlier:\n{stdout}\n{stderr}"
        );
        assert!(
            !stderr.contains("Recovered interrupted session"),
            "{argv:?} must not consume a checkpoint when the TUI cannot start:\n{stderr}"
        );
        let sessions_dir = codewhale_home.join("sessions");
        let persisted: Vec<_> = std::fs::read_dir(&sessions_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .collect();
        assert!(
            persisted.is_empty(),
            "{argv:?} must not write session files under a sealed home: {persisted:?}"
        );
    }
}
