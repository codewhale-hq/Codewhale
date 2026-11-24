use super::*;

#[tokio::test]
async fn multi_whale_routes_support_crud_fencing_safety_and_non_destructive_recycle() -> Result<()>
{
    let _env_lock = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("whales-route");
    let cw_home = root.join("cw-home");
    let _cw_guard = EnvVarGuard::set("CODEWHALE_HOME", &cw_home);
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;

    let token = "whales-fixture-token";
    let Some((addr, _, handle)) = spawn_test_server_with_root_token_mobile_workspace(
        root.clone(),
        root.join("sessions"),
        Some(token.into()),
        false,
        workspace.clone(),
    )
    .await?
    else {
        bail!("loopback server unavailable");
    };

    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}");

    assert_eq!(
        client
            .get(format!("{base}/v1/whales"))
            .send()
            .await?
            .status(),
        401
    );

    let initial: Vec<Value> = client
        .get(format!("{base}/v1/whales"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0]["id"], "default");
    assert_eq!(initial[0]["revision"], "new");
    assert!(
        std::path::Path::new(initial[0]["soul_path"].as_str().unwrap())
            .ends_with("whales/default/SOUL.md")
    );
    assert!(
        std::path::Path::new(initial[0]["folder_path"].as_str().unwrap())
            .ends_with("whales/default")
    );

    let default_soul: Value = client
        .put(format!("{base}/v1/whale/soul"))
        .bearer_auth(token)
        .header("if-match", "\"new\"")
        .json(&json!({ "text": "Keep answers grounded." }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(default_soul["text"], "Keep answers grounded.");

    assert_eq!(
        client
            .delete(format!("{base}/v1/whales/default"))
            .bearer_auth(token)
            .send()
            .await?
            .status(),
        400
    );

    assert_eq!(
        client
            .post(format!("{base}/v1/whales"))
            .bearer_auth(token)
            .json(&json!({
                "id": "../escape",
                "name": "Bad Whale"
            }))
            .send()
            .await?
            .status(),
        400
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/whales/BadUpper"))
            .bearer_auth(token)
            .send()
            .await?
            .status(),
        400
    );
    let encoded_traversal_status = client
        .get(format!("{base}/v1/whales/..%2Fescape"))
        .bearer_auth(token)
        .send()
        .await?
        .status();
    assert!(
        encoded_traversal_status == 400 || encoded_traversal_status == 404,
        "unexpected status for encoded traversal: {encoded_traversal_status}"
    );

    let created_resp = client
        .post(format!("{base}/v1/whales"))
        .bearer_auth(token)
        .json(&json!({
            "name": "Docs Keeper",
            "purpose": "Maintain architecture docs",
            "color": "patch",
            "soul": "Write crisp markdown docs.",
            "memory": "Keep ADR numbering monotonic.",
            "permissionPosture": "ask",
            "childPosture": "accept_edits"
        }))
        .send()
        .await?;
    assert_eq!(created_resp.status(), 201);
    let created: Value = created_resp.json().await?;
    assert_eq!(created["id"], "docs-keeper");
    assert_eq!(created["name"], "Docs Keeper");
    assert_eq!(created["color"], "patch");
    assert_eq!(created["soul"], "Write crisp markdown docs.");
    assert_eq!(created["memory"], "Keep ADR numbering monotonic.");
    let rev1 = created["revision"].as_str().unwrap().to_string();

    assert_eq!(
        client
            .patch(format!("{base}/v1/whales/docs-keeper"))
            .bearer_auth(token)
            .header(
                "if-match",
                "\"0000000000000000000000000000000000000000000000000000000000000000\""
            )
            .json(&json!({ "purpose": "Stale edit" }))
            .send()
            .await?
            .status(),
        412
    );

    let patched: Value = client
        .patch(format!("{base}/v1/whales/docs-keeper"))
        .bearer_auth(token)
        .header("if-match", format!("\"{rev1}\""))
        .json(&json!({
            "purpose": "Maintain docs and changelogs",
            "soul": "Write crisp markdown docs and link sources."
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(patched["purpose"], "Maintain docs and changelogs");
    assert_eq!(
        patched["soul"],
        "Write crisp markdown docs and link sources."
    );
    assert_eq!(patched["memory"], "Keep ADR numbering monotonic.");

    let first_recycle: Value = client
        .post(format!("{base}/v1/whales/docs-keeper/recycle"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let first_thread_id = first_recycle["home_thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(first_recycle.get("previous_home_thread_id").is_none());
    assert_eq!(first_recycle["thread"]["whale_id"], "docs-keeper");
    assert_eq!(first_recycle["thread"]["permission_posture"], "ask");

    let second_recycle: Value = client
        .post(format!("{base}/v1/whales/docs-keeper/recycle"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let second_thread_id = second_recycle["home_thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first_thread_id, second_thread_id);
    assert_eq!(
        second_recycle["previous_home_thread_id"],
        json!(first_thread_id)
    );
    assert_eq!(
        second_recycle["whale"]["home_thread_id"],
        json!(second_thread_id)
    );
    assert_eq!(
        second_recycle["whale"]["soul"],
        "Write crisp markdown docs and link sources."
    );
    assert_eq!(
        second_recycle["whale"]["memory"],
        "Keep ADR numbering monotonic."
    );

    let prior_thread: Value = client
        .get(format!("{base}/v1/threads/{first_thread_id}"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(prior_thread["thread"]["id"], first_thread_id);

    let deleted: Value = client
        .delete(format!("{base}/v1/whales/docs-keeper"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(deleted["deleted"], true);
    assert_eq!(
        client
            .get(format!("{base}/v1/whales/docs-keeper"))
            .bearer_auth(token)
            .send()
            .await?
            .status(),
        404
    );

    handle.abort();
    Ok(())
}
