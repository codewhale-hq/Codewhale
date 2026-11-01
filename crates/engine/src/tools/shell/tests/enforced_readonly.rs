use super::*;
use tempfile::tempdir;

#[test]
fn enforced_readonly_mode_is_an_execution_contract_and_preserves_plain_classification() {
    let plain = json!({"command": "python3 -c 'print(1)'"});
    let input = json!({"command": "python3 -c 'print(1)'", "read_only": true});
    assert!(!agent_readonly_bash_input(&plain));
    assert!(!LowercaseBashTool.is_read_only_for(&plain));
    assert!(agent_readonly_bash_input(&input));
    assert!(LowercaseBashTool.is_read_only_for(&input));
    assert!(LowercaseBashTool.supports_parallel_for(&input));
    assert_eq!(
        LowercaseBashTool.input_schema()["properties"]["read_only"]["type"],
        "boolean"
    );
    for invalid in [json!(null), json!("true"), json!(1)] {
        let mut input = input.clone();
        input["read_only"] = invalid;
        assert!(contract_bash_legacy_input(&input).is_err());
        assert!(!agent_readonly_bash_input(&input));
    }
}

#[tokio::test]
async fn enforced_readonly_rejects_incompatible_shapes_before_running_anything() {
    let tmp = tempdir().unwrap();
    let context = ToolContext::new(tmp.path());
    for (key, value) in [
        ("action", json!("wait")),
        ("background", json!(true)),
        ("interactive", json!(true)),
        ("tty", json!(true)),
        ("stdin", json!("input")),
        ("sandbox_permissions", json!("danger-full-access")),
        ("justification", json!("please widen")),
    ] {
        let mut input = json!({"command": "touch should-not-exist", "read_only": true});
        input[key] = value;
        assert!(
            exec_shell_input_agent_readonly_verdict(&input).is_err(),
            "{input}"
        );
        let error = BashTool::new("Bash")
            .execute(input, &context)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("incompatible"), "{error}");
        assert!(!tmp.path().join("should-not-exist").exists());
    }
}

#[test]
fn enforced_readonly_refuses_an_unenforced_or_writable_prepared_environment() {
    let tmp = tempdir().unwrap();
    let mut environment = ExecEnv {
        command: vec!["must-not-run".into()],
        cwd: tmp.path().into(),
        env: HashMap::new(),
        timeout: Duration::from_secs(1),
        sandbox_type: SandboxType::None,
        policy: ExecutionSandboxPolicy::read_only(),
    };
    assert!(require_native_readonly_execution(&environment).is_err());
    #[cfg(target_os = "macos")]
    {
        environment.sandbox_type = SandboxType::MacosSeatbelt;
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        environment.sandbox_type = SandboxType::LinuxBubblewrap;
    }
    environment.policy = ExecutionSandboxPolicy::DangerFullAccess;
    assert!(require_native_readonly_execution(&environment).is_err());
}

struct RefusingExternalBackend;
#[async_trait]
impl codewhale_sandbox::backend::SandboxBackend for RefusingExternalBackend {
    fn kind(&self) -> codewhale_sandbox::backend::SandboxKind {
        codewhale_sandbox::backend::SandboxKind::Unsupported
    }
    async fn exec(
        &self,
        _command: &str,
        _env: &HashMap<String, String>,
    ) -> Result<codewhale_sandbox::backend::SandboxOutput> {
        panic!("enforced read-only must never reach an unattested external executor")
    }
}

#[tokio::test]
async fn enforced_readonly_refuses_external_backend_without_dispatch() {
    let tmp = tempdir().unwrap();
    let mut context = ToolContext::new(tmp.path());
    context.sandbox_backend = Some(std::sync::Arc::new(RefusingExternalBackend));
    let error = LowercaseBashTool
        .execute(
            json!({"command": "touch should-not-exist", "read_only": true}),
            &context,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("external backends"), "{error}");
    assert!(!tmp.path().join("should-not-exist").exists());
}

#[cfg(unix)]
#[allow(clippy::print_stderr)] // Test receipt distinguishes unavailable enforcement from a real probe.
fn native_context(root: &std::path::Path) -> Option<ToolContext> {
    let mut context = ToolContext::new(root);
    context.auto_approve = true;
    // Exercise narrowing from the broadest incoming posture.
    context.elevated_sandbox_policy = Some(ExecutionSandboxPolicy::DangerFullAccess);
    context.shell_policy = ShellPolicy::ReadOnly;
    #[cfg(target_os = "linux")]
    context.shell_manager.lock().unwrap().set_prefer_bwrap(true);
    if !context
        .shell_manager
        .lock()
        .unwrap()
        .configured_sandbox_type()
        .is_some_and(is_native_readonly_sandbox)
    {
        eprintln!("UNRUN: native read_only execution probe; no enforcing sandbox available");
        return None;
    }
    Some(context)
}

#[cfg(unix)]
fn python(script: &str) -> String {
    let binary = [
        "/usr/bin/python3",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
    ]
    .into_iter()
    .find(|candidate| std::path::Path::new(candidate).is_file())
    .expect("Python fixture runtime");
    format!("{binary} -I -B -c {}", shell_words::quote(script))
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::print_stderr)] // Native enforcement receipt, outside the TUI runtime.
async fn enforced_readonly_native_python_reads_sqlite_and_cannot_write() {
    // #6305: the sandbox replaces /tmp with a fresh tmpfs, so a fixture
    // rooted there vanishes before --chdir reaches it.
    let tmp = crate::test_support::sandbox_visible_tempdir();
    let Some(context) = native_context(tmp.path()) else {
        return;
    };
    let database = rusqlite::Connection::open(tmp.path().join("fixture.sqlite")).unwrap();
    database
        .execute_batch(
            "CREATE TABLE fixture(value TEXT); INSERT INTO fixture VALUES ('read-receipt');",
        )
        .unwrap();
    drop(database);
    std::fs::write(tmp.path().join("peer.txt"), "preserve peer bytes").unwrap();
    let read = LowercaseBashTool.execute(json!({
        "command": python("import sqlite3; c=sqlite3.connect('file:fixture.sqlite?mode=ro', uri=True); print(c.execute('SELECT value FROM fixture').fetchone()[0])"),
        "read_only": true
    }), &context).await.unwrap();
    assert!(
        read.success && read.content.contains("read-receipt"),
        "{}",
        read.content
    );
    assert_eq!(read.metadata.as_ref().unwrap()["sandboxed"], true);
    let refused = LowercaseBashTool
        .execute(
            json!({
                "command": python("open('peer.txt', 'w').write('corrupt')"), "read_only": true
            }),
            &context,
        )
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("Operation not permitted")
            || refused.to_string().contains("Read-only file system")
            || refused.to_string().contains("Permission denied"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("peer.txt")).unwrap(),
        "preserve peer bytes"
    );
    eprintln!(
        "NATIVE_READONLY_ENFORCED: SQLite read succeeded; write denied and peer bytes preserved"
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::print_stderr)] // Native enforcement receipt, outside the TUI runtime.
async fn enforced_readonly_native_python_cannot_reach_a_loopback_listener() {
    // #6305: the sandbox replaces /tmp with a fresh tmpfs, so a fixture
    // rooted there vanishes before --chdir reaches it.
    let tmp = crate::test_support::sandbox_visible_tempdir();
    let Some(mut context) = native_context(tmp.path()) else {
        return;
    };
    context.elevated_sandbox_policy = Some(ExecutionSandboxPolicy::default());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "import socket; s=socket.socket(); s.settimeout(1); s.connect(('127.0.0.1', {port})); print('connected')"
    );
    let refused = LowercaseBashTool
        .execute(
            json!({"command": python(&script), "read_only": true}),
            &context,
        )
        .await
        .unwrap_err();
    // Python includes the submitted source line in its traceback. Only an
    // actual output line means the script reached the print after connect.
    assert!(
        !refused
            .to_string()
            .lines()
            .any(|line| line.trim() == "connected"),
        "{refused}"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    eprintln!(
        "NATIVE_READONLY_ENFORCED: loopback connection denied; listener received no connection"
    );
}
