use super::*;

#[tokio::test]
async fn get_v1_workspace_instructions_lists_effective_sources() -> Result<()> {
    // No `lock_test_env`/`EnvVarGuard` here: the handler resolves the global
    // layer's home inside `spawn_blocking`, and a blocking-pool thread is not
    // enrolled in the test's env scope — sealing the environment would
    // deadlock the request against the lock this test would be holding.
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("instructions-route");
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join(".git"))?;
    fs::write(
        workspace.join(".git").join("HEAD"),
        "ref: refs/heads/main\n",
    )?;
    fs::write(workspace.join("AGENTS.md"), "# Repo rules\n")?;
    // Exists but shadowed: AGENTS.md outranks it in the same directory.
    fs::create_dir_all(workspace.join(".codewhale"))?;
    fs::write(
        workspace.join(".codewhale").join("instructions.md"),
        "shadowed\n",
    )?;
    fs::create_dir_all(workspace.join(".codewhale").join("rules"))?;
    fs::write(
        workspace.join(".codewhale").join("rules").join("style.md"),
        "keep it small\n",
    )?;

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace(
            root,
            sessions_dir,
            None,
            false,
            workspace.clone(),
        )
        .await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();

    let body: serde_json::Value = client
        .get(format!("http://{addr}/v1/workspace/instructions"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let sources = body["sources"].as_array().expect("sources array");

    let agents = sources
        .iter()
        .find(|source| {
            source["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("AGENTS.md"))
        })
        .expect("AGENTS.md row");
    assert_eq!(agents["kind"], "project");
    assert_eq!(agents["status"], "loaded");
    assert_eq!(agents["relative_path"], "AGENTS.md");
    assert_eq!(agents["exists"], true);
    assert!(agents["bytes"].as_u64().is_some());

    let shadowed = sources
        .iter()
        .find(|source| {
            source["path"]
                .as_str()
                // `path` is the native absolute path, so it is backslashed on
                // Windows; compare separator-agnostically rather than against
                // one platform's spelling.
                .is_some_and(|p| p.replace('\\', "/").ends_with(".codewhale/instructions.md"))
        })
        .expect("shadowed workspace instructions row");
    assert_eq!(shadowed["status"], "shadowed");

    let rule = sources
        .iter()
        .find(|source| source["kind"] == "rule")
        .expect("rules file row");
    assert_eq!(rule["status"], "loaded");
    assert_eq!(rule["relative_path"], ".codewhale/rules/style.md");

    // Unchecked-but-real candidates are reported too, so clients can render
    // the full precedence map instead of a post-hoc file scan.
    assert!(
        sources.iter().any(|source| source["status"] == "missing"),
        "missing candidates must be listed: {sources:?}"
    );

    assert_eq!(body["generated_fallback"], false);
    // #6526: the per-scope token ceilings replaced the single byte cap.
    assert!(body["global_budget_tokens"].as_u64().unwrap() > 0);
    assert!(body["project_budget_tokens"].as_u64().unwrap() > 0);
    assert!(body["usable_window_tokens"].is_null());

    handle.abort();
    Ok(())
}
