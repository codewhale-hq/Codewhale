//! Installed-dispatcher acceptance for the pipe-only credential handoff.

#![cfg(unix)]

use std::fs::{self, File};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use codewhale_secrets::{FileKeyringStore, KeyringStore};
use tempfile::TempDir;

const SENTINEL: &str = "cw-handoff-child-sentinel-7b30";
const TERMINAL_ERROR: &str =
    "error: refusing terminal output; pipe credential handoff to the intended local client\n";

#[test]
fn terminal_refusal_precedes_config_reads_and_secret_migration() {
    let malformed = TempDir::new().expect("malformed fixture root");
    let malformed_home = malformed.path().join("home");
    let malformed_codewhale_home = malformed.path().join("codewhale-home");
    fs::create_dir_all(&malformed_codewhale_home).expect("create explicit Codewhale home");
    let malformed_config = malformed_codewhale_home.join("config.toml");
    let malformed_bytes = format!("provider = \"openrouter\"\ninvalid = [{SENTINEL}\n");
    fs::write(&malformed_config, &malformed_bytes).expect("write malformed config trap");

    let mut command = isolated_command(&malformed_home, Some(&malformed_codewhale_home));
    command.args(["auth", "print-api-key", "--provider", "openrouter"]);
    let output = output_with_terminal_stdout(command);
    assert!(!output.status.success(), "terminal handoff must refuse");
    assert_eq!(String::from_utf8_lossy(&output.stderr), TERMINAL_ERROR);
    assert_eq!(
        fs::read_to_string(&malformed_config).expect("malformed config remains readable"),
        malformed_bytes,
    );
    assert!(
        !malformed_codewhale_home.join("secrets").exists(),
        "terminal preflight must not construct durable credential state"
    );

    let migration = TempDir::new().expect("migration fixture root");
    let sealed_home = migration.path().join("home");
    let primary_home = sealed_home.join(".codewhale");
    fs::create_dir_all(&primary_home).expect("create primary home");
    fs::write(
        primary_home.join("config.toml"),
        "provider = \"openrouter\"\n",
    )
    .expect("write valid config");
    let legacy_store = sealed_home
        .join(".deepseek")
        .join("secrets")
        .join("secrets.json");
    FileKeyringStore::new(&legacy_store)
        .set("openrouter", SENTINEL)
        .expect("seed synthetic legacy credential");
    let legacy_before = fs::read(&legacy_store).expect("read legacy fixture");
    let primary_store = primary_home.join("secrets").join("secrets.json");

    let mut command = isolated_command(&sealed_home, None);
    command.args(["auth", "print-api-key", "--provider", "openrouter"]);
    let output = output_with_terminal_stdout(command);
    assert!(!output.status.success(), "terminal handoff must refuse");
    assert_eq!(String::from_utf8_lossy(&output.stderr), TERMINAL_ERROR);
    assert_eq!(
        fs::read(&legacy_store).expect("legacy store remains readable"),
        legacy_before,
        "terminal preflight must not rewrite the legacy credential store"
    );
    assert!(
        !primary_store.exists(),
        "terminal preflight must not migrate a legacy credential"
    );
}

#[test]
fn initialization_failures_are_source_free() {
    let fixture = TempDir::new().expect("fixture root");
    let sealed_home = fixture.path().join("home");
    let codewhale_home = fixture.path().join(format!("codewhale-home-{SENTINEL}"));
    fs::create_dir_all(&codewhale_home).expect("create Codewhale home");
    fs::write(
        codewhale_home.join("config.toml"),
        format!("invalid = [{SENTINEL}\n"),
    )
    .expect("write malformed config trap");

    let mut command = isolated_command(&sealed_home, Some(&codewhale_home));
    let output = command
        .args([
            "--api-key",
            SENTINEL,
            "auth",
            "print-api-key",
            "--provider",
            "openrouter",
        ])
        .output()
        .expect("run pipe handoff with malformed initialization");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: unavailable credential\n"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(SENTINEL),
        "neither a secret nor a sentinel-bearing path may reach stderr"
    );
}

#[test]
fn installed_unix_dispatcher_settles_a_closed_pipe_cleanly() {
    let fixture = TempDir::new().expect("fixture root");
    let sealed_home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("codewhale-home");
    let mut command = isolated_command(&sealed_home, Some(&codewhale_home));
    let output = command
        .args([
            "--api-key",
            SENTINEL,
            "auth",
            "print-api-key",
            "--provider",
            "openrouter",
        ])
        .stdout(closed_pipe_writer())
        .output()
        .expect("run installed dispatcher against a closed pipe");

    assert!(
        output.status.success(),
        "closed consumer must be a clean settlement, not SIGPIPE or failure: {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // A sealed fresh home has never seen the usage policy, so the only thing
    // allowed on stderr is that one-time disclosure. Nothing else may leak.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty()
            || stderr == format!("{}\n", codewhale_telemetry::notice::STARTUP_DISCLOSURE),
        "unexpected stderr: {stderr}"
    );
    assert!(!stderr.contains(SENTINEL));
}

fn isolated_command(home: &Path, codewhale_home: Option<&Path>) -> Command {
    let mut command = Command::new(codewhale_binary());
    command
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CODEWHALE_SECRET_BACKEND", "file")
        .stdin(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(codewhale_home) = codewhale_home {
        command.env("CODEWHALE_HOME", codewhale_home);
    }
    command
}

fn output_with_terminal_stdout(mut command: Command) -> Output {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: `openpty` initializes both descriptors on success. Each is
    // immediately transferred into exactly one `File`, which owns the close.
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0, "open a pseudo-terminal");
    // SAFETY: successful `openpty` returned two fresh owned descriptors.
    let master = unsafe { File::from_raw_fd(master_fd) };
    // SAFETY: successful `openpty` returned two fresh owned descriptors.
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let output = command
        .stdout(Stdio::from(slave))
        .output()
        .expect("run dispatcher with terminal stdout");
    drop(master);
    output
}

fn closed_pipe_writer() -> Stdio {
    let mut descriptors = [-1; 2];
    // SAFETY: `pipe` initializes both descriptors on success.
    let result = unsafe { libc::pipe(descriptors.as_mut_ptr()) };
    assert_eq!(result, 0, "create pipe fixture");
    // No reader exists before the child starts, so its first write
    // deterministically receives EPIPE (or SIGPIPE if the guard regresses).
    // SAFETY: `descriptors[0]` is a fresh descriptor and is closed once here.
    assert_eq!(unsafe { libc::close(descriptors[0]) }, 0);
    // SAFETY: `descriptors[1]` is the remaining fresh owned descriptor.
    let writer = unsafe { File::from_raw_fd(descriptors[1]) };
    Stdio::from(writer)
}

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
