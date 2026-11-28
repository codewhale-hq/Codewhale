// Unix only: the fixture redirects the home directory through `HOME`,
// and Windows resolves the profile through the known-folder API instead.
#[cfg(unix)]
#[test]
fn sandbox_read_guard_denies_active_and_known_agent_credentials_in_real_file_tool() {
    use codewhale_engine::tools::spec::{ToolContext, ToolSpec};
    let _env = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().expect("private fixture");
    let home = temp.path().join("home");
    let active_home = temp.path().join("active");
    let codex = temp.path().join("codex");
    let _home = crate::test_support::EnvVarGuard::set("HOME", home.to_str().unwrap());
    let _active =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", active_home.to_str().unwrap());
    let _codex = crate::test_support::EnvVarGuard::set("CODEX_HOME", codex.to_str().unwrap());
    let _auth = crate::test_support::EnvVarGuard::remove("OPENAI_CODEX_AUTH_FILE");
    assert_eq!(dirs::home_dir().as_deref(), Some(home.as_path()));
    let denied = [
        active_home.join("credentials/fixture.json"),
        codex.join("auth.json"),
        home.join(".kimi/credentials/fixture.json"),
        home.join(".claude/.credentials.json"),
        home.join(".codewhale-cu/recordings/trajectories/fixture.jsonl"),
    ];
    for path in &denied {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "read-s10-synthetic-value").unwrap();
    }
    codewhale_sandbox::read_guard::with_test_active_denylist(
        codewhale_sandbox::read_guard::ReadDenylist::build(true, &[], &[]),
        || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let context = ToolContext::new(temp.path());
            for path in denied {
                let result = runtime.block_on(
                    codewhale_engine::tools::file::ReadFileTool
                        .execute(serde_json::json!({"path": path}), &context),
                );
                assert!(result.is_err(), "file tool read a credential source");
            }
            let safe = home.join(".claude/settings.json");
            std::fs::write(&safe, "ordinary-readable-setting").unwrap();
            let allowed = runtime
                .block_on(
                    codewhale_engine::tools::file::ReadFileTool
                        .execute(serde_json::json!({"path": safe}), &context),
                )
                .expect("ordinary settings remain readable");
            assert!(allowed.content.contains("ordinary-readable-setting"));
        },
    );
}

#[cfg(unix)]
#[test]
fn sandbox_wait_preserves_the_leader_until_output_is_drained() {
    use codewhale_runtime::process_tree::{leader_exited_unreaped, spawn_contained_std};
    use std::process::Command;
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    let (child, tree) = spawn_contained_std(Command::new("sh").args(["-c", "exit 7"])).unwrap();
    let pid = child.id();
    let (done_tx, done_rx) = sync_channel(0);
    let observer = std::thread::spawn(move || {
        done_tx.send(()).unwrap();
        let leader_still_waitable = leader_exited_unreaped(pid);
        done_tx.send(()).unwrap();
        leader_still_waitable
    });
    let status = crate::wait_for_sandbox_output(child, tree, done_rx, Duration::from_secs(5))
        .expect("wait for the command and both streams");
    assert!(observer.join().unwrap().unwrap());
    assert_eq!(status.code(), Some(7));
}

#[cfg(unix)]
#[test]
fn sandbox_wait_collects_pending_output_after_cleanup() {
    use codewhale_runtime::process_tree::{
        leader_exited_unreaped, spawn_contained_std, wait_leader_exit_until,
    };
    use std::process::Command;
    use std::sync::mpsc::sync_channel;
    use std::time::{Duration, Instant};

    let (child, tree) = spawn_contained_std(Command::new("sh").args(["-c", "exit 0"])).unwrap();
    assert!(wait_leader_exit_until(&child, Instant::now() + Duration::from_secs(5)).unwrap());
    let pid = child.id();
    let (done_tx, done_rx) = sync_channel(0);
    let last_output = std::thread::spawn(move || {
        done_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let error = loop {
            if let Err(error) = leader_exited_unreaped(pid) {
                break error;
            }
            assert!(Instant::now() < deadline, "command was not reaped");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        done_tx.send(()).expect("collect the final output");
    });
    let status = crate::wait_for_sandbox_output(child, tree, done_rx, Duration::from_millis(350))
        .expect("the command exited before the deadline");
    last_output.join().unwrap();
    assert!(status.success());
}
