#![cfg(any(unix, windows))]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

const TOKEN: &str = "owner-crash-reentry-fixture-token";
const LISTENING: &str = "Runtime API listening on http://";

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        #[cfg(unix)]
        let root = tempfile::Builder::new()
            .prefix("cw-ocr-")
            .tempdir_in(
                std::env::temp_dir()
                    .canonicalize()
                    .expect("canonical temporary directory"),
            )
            .expect("fixture root");
        #[cfg(windows)]
        let root = tempfile::tempdir().expect("fixture root");
        for name in ["home", "state", "tmp", "work"] {
            std::fs::create_dir_all(root.path().join(name)).expect("fixture directory");
        }
        std::fs::write(root.path().join("config.toml"), "").expect("fixture config");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codewhale"));
        command.current_dir(self.path("work")).env_clear();
        for name in ["PATH", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let home = self.path("home");
        let tmp = self.path("tmp");
        command
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("CODEWHALE_HOME", self.path("state"))
            .env("CODEWHALE_SECRET_BACKEND", "file")
            .env("CODEWHALE_TELEMETRY", "0")
            .env("CODEWHALE_RUNTIME_TOKEN", TOKEN)
            .env("TEMP", &tmp)
            .env("TMP", &tmp)
            .env("TMPDIR", &tmp)
            .arg("--config")
            .arg(self.path("config.toml"))
            .arg("--no-project-config");
        command
    }

    fn receipt(&self) -> (Vec<u8>, Value) {
        #[cfg(unix)]
        let path = codewhale_paths::daemon_socket::resolve_socket_path(
            &codewhale_paths::daemon_socket::SocketPathInputs {
                explicit: None,
                codewhale_home_override: Some(self.path("state")),
                xdg_runtime_dir: None,
                user_home: Some(self.path("home")),
                macos: cfg!(target_os = "macos"),
            },
        )
        .expect("fixture socket path")
        .with_file_name("daemon.sock.owner.json");
        #[cfg(windows)]
        let path = self.path("state").join("run").join("daemon.owner.json");
        let bytes = std::fs::read(path).expect("published owner receipt");
        let value = serde_json::from_slice(&bytes).expect("owner receipt JSON");
        (bytes, value)
    }

    fn stderr(&self, label: &str) -> String {
        std::fs::read_to_string(self.path(&format!("{label}.stderr"))).unwrap_or_default()
    }
}

struct Owner {
    child: Child,
    base: String,
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_owner(fixture: &Fixture, label: &str) -> Owner {
    let stderr = std::fs::File::create(fixture.path(&format!("{label}.stderr")))
        .expect("owner stderr capture");
    let mut child = fixture
        .command()
        .args(["app-server", "--http", "--host", "127.0.0.1", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn codewhale app-server");
    let stdout = child.stdout.take().expect("owner stdout");
    let (ready, listening) = mpsc::channel();
    std::thread::spawn(move || {
        let mut ready = Some(ready);
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(address) = line.strip_prefix(LISTENING)
                && let Some(ready) = ready.take()
            {
                let _ = ready.send(address.trim().to_string());
            }
        }
    });
    let mut owner = Owner {
        child,
        base: String::new(),
    };
    match listening.recv_timeout(Duration::from_secs(120)) {
        Ok(address) => owner.base = format!("http://{address}"),
        Err(_) => {
            let status = owner.child.try_wait().ok().flatten();
            drop(owner);
            panic!(
                "{label} never reported readiness (exit: {status:?})\nstderr:\n{}",
                fixture.stderr(label)
            );
        }
    }
    owner
}

fn http() -> reqwest::blocking::Client {
    codewhale_release::tls::reqwest_blocking_client_builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("loopback HTTP client")
}

#[test]
fn killed_owner_is_replaced_by_the_next_app_server() {
    let fixture = Fixture::new();
    let client = http();

    let mut first = spawn_owner(&fixture, "first");
    let (first_bytes, first_receipt) = fixture.receipt();
    assert_eq!(first_receipt["pid"], json!(first.child.id()));
    let created = client
        .post(format!("{}/v1/threads", first.base))
        .bearer_auth(TOKEN)
        .json(&json!({}))
        .send()
        .expect("create thread on the first owner");
    let status = created.status();
    let created: Value = created.json().expect("created thread JSON");
    assert!(status.is_success(), "create thread: {status} {created}");
    let thread_id = created["id"]
        .as_str()
        .expect("created thread id")
        .to_string();

    first.child.kill().expect("kill the first owner");
    first.child.wait().expect("reap the first owner");
    let (dead_bytes, dead_receipt) = fixture.receipt();
    assert_eq!(dead_bytes, first_bytes);
    assert_eq!(dead_receipt["pid"], json!(first.child.id()));

    let second = spawn_owner(&fixture, "second");
    let (_, second_receipt) = fixture.receipt();
    assert_eq!(second_receipt["pid"], json!(second.child.id()));
    assert_ne!(
        second_receipt["lease_generation"],
        first_receipt["lease_generation"]
    );
    assert_eq!(second_receipt["data_dir"], first_receipt["data_dir"]);
    assert_eq!(
        second_receipt["execution_scope"],
        first_receipt["execution_scope"]
    );

    let listed: Value = client
        .get(format!("{}/v1/threads", second.base))
        .bearer_auth(TOKEN)
        .send()
        .expect("list threads on the second owner")
        .json()
        .expect("thread list JSON");
    assert!(
        listed
            .as_array()
            .expect("thread list")
            .iter()
            .any(|thread| thread["id"] == json!(thread_id)),
        "{thread_id} missing from {listed}"
    );

    let attached = fixture
        .command()
        .args(["thread", "list"])
        .stdin(Stdio::null())
        .output()
        .expect("run codewhale thread list");
    let stdout = String::from_utf8_lossy(&attached.stdout);
    assert!(
        attached.status.success() && stdout.contains(&thread_id),
        "thread list through the second owner failed ({})\nstdout:\n{stdout}\nstderr:\n{}",
        attached.status,
        String::from_utf8_lossy(&attached.stderr)
    );
}
