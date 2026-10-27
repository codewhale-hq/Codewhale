// Unix only: the fixture redirects the home directory through `HOME`,
// and Windows resolves the profile through the known-folder API instead.
#[cfg(unix)]
#[test]
fn sandbox_read_guard_denies_active_and_known_agent_credentials_in_real_file_tool() {
    use crate::tools::spec::{ToolContext, ToolSpec};
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
                    crate::tools::file::ReadFileTool
                        .execute(serde_json::json!({"path": path}), &context),
                );
                assert!(result.is_err(), "file tool read a credential source");
            }
            let safe = home.join(".claude/settings.json");
            std::fs::write(&safe, "ordinary-readable-setting").unwrap();
            let allowed = runtime
                .block_on(
                    crate::tools::file::ReadFileTool
                        .execute(serde_json::json!({"path": safe}), &context),
                )
                .expect("ordinary settings remain readable");
            assert!(allowed.content.contains("ordinary-readable-setting"));
        },
    );
}
