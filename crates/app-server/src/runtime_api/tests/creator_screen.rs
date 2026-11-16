use std::path::{Path, PathBuf};
use tempfile::tempdir;

use super::*;

fn pinned_decoration_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../skills/assets/dsh-creator/skills/cordis-plugin-development/templates/decoration")
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn creator_screen_template_seat_is_admitted_and_served() -> Result<()> {
    codewhale_plugins::activation::install_extension_host_policy(true);
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(true);
    let root = tempdir()?;
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let src = workspace.join("my-decoration");
    copy_dir_all(&pinned_decoration_dir(), &src)?;

    let (addr, handle) = spawn_plugin_api_server(root.path().to_path_buf(), workspace.clone())
        .await?
        .expect("loopback runtime server must bind");
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");
    let installed =
        plugin_ui::install_active(&client, &base, format!("dsh:{}", src.display())).await?;
    let name = installed["name"]
        .as_str()
        .expect("installed plugin selector")
        .to_string();

    let admitted_response = client
        .get(format!("{base}/{name}/ui?slot=conversation.composer.dock"))
        .send()
        .await?;
    assert_eq!(admitted_response.status(), StatusCode::OK);
    let ui: serde_json::Value = admitted_response.json().await?;
    assert_eq!(ui["slot"], "conversation.composer.dock");
    assert_eq!(ui["dsh_slot"], "conversation.composer.dock");
    assert_eq!(ui["bridge"]["kind"], "dsh-client");
    assert_eq!(ui["entry"], "__codewhale_dsh_bootstrap.js");

    let asset_base = ui["base"].as_str().expect("plugin ui base");
    let entry = ui["entry"].as_str().expect("plugin ui entry");
    let revision = ui["revision"].as_str().expect("plugin ui revision");
    let asset_response = client
        .get(format!("{asset_base}{entry}"))
        .header(reqwest::header::IF_MATCH, revision)
        .send()
        .await?;
    assert_eq!(asset_response.status(), StatusCode::OK);
    let script = asset_response.text().await?;
    assert!(
        script.contains("@local/my-decoration"),
        "expected package id in bootstrap script:\n{script}"
    );
    assert!(
        script.contains("conversation.composer.dock"),
        "expected slot name in bootstrap script:\n{script}"
    );

    let denied_response = client
        .get(format!("{base}/{name}/ui?slot=workpane.panel"))
        .send()
        .await?;
    assert_eq!(denied_response.status(), StatusCode::NOT_FOUND);
    let denied_body: serde_json::Value = denied_response.json().await?;
    assert_eq!(denied_body["error"]["code"], "plugin_ui_seat_not_admitted");

    handle.abort();
    Ok(())
}
