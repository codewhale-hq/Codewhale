use super::*;

#[tokio::test]
async fn profile_constitution_preview_is_authenticated_and_uses_the_engine_renderer() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("constitution-route");
    let Some((addr, _, handle)) = spawn_test_server_with_root_token_mobile_workspace(
        root.clone(),
        root.join("sessions"),
        Some("constitution-fixture-token".into()),
        false,
        root.join("workspace"),
    )
    .await?
    else {
        bail!("loopback server unavailable");
    };
    let client = codewhale_release::tls::reqwest_client();
    let url = format!("http://{addr}/v1/constitution/preview");
    let document = codewhale_config::user_constitution::ProfileConstitution {
        notes: "Show the recommendation first. 🐋".into(),
        ..Default::default()
    };
    assert_eq!(
        client.post(&url).json(&document).send().await?.status(),
        401
    );
    let response: Value = client
        .post(&url)
        .bearer_auth("constitution-fixture-token")
        .json(&document)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(response["saved"], false);
    assert_eq!(
        response["modelGuidance"],
        document.as_user_constitution()?.render_body()
    );
    let mut invalid = serde_json::to_value(&document)?;
    invalid["permissions"] = "all".into();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("constitution-fixture-token")
            .json(&invalid)
            .send()
            .await?
            .status(),
        422
    );
    handle.abort();
    Ok(())
}
