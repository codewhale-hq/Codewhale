use super::*;
use codewhale_engine::command_catalog::{BuiltinCommand, CommandCatalog, UserCommand};

struct Catalog(Arc<std::sync::Mutex<Option<String>>>);

impl CommandCatalog for Catalog {
    fn answers_to(&self, _name: &str) -> bool {
        false
    }
    fn builtins(&self) -> Vec<BuiltinCommand> {
        Vec::new()
    }
    fn user_commands(&self, _workspace: &Path) -> Vec<UserCommand> {
        let plugin_id = self.0.lock().expect("plugin id").clone();
        let command = |name: &str, plugin: Option<String>| UserCommand {
            name: name.to_string(),
            aliases: Vec::new(),
            description: Some("A command.".to_string()),
            usage: None,
            takes_arguments: false,
            hidden: false,
            extension: false,
            plugin_id: plugin,
            workspace_sourced: false,
            builtin_bindings: Vec::new(),
        };
        vec![command("mine", None), command("ship", plugin_id)]
    }
}

#[tokio::test]
async fn the_index_lists_every_kind_once_and_a_plugin_carries_what_it_adds() -> Result<()> {
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("index-route");
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let plugin_id = Arc::new(std::sync::Mutex::new(None));
    let _catalog = codewhale_engine::command_catalog::CatalogGuard::install(Arc::new(Catalog(
        plugin_id.clone(),
    )));
    let sources = root.join("sources");
    fs::create_dir_all(&sources)?;
    let bundle = write_plugin_source_bundle(&sources, "indexed")?;
    fs::create_dir_all(bundle.join("commands"))?;
    fs::write(bundle.join("commands/ship.md"), "Ship it.\n")?;
    fs::create_dir_all(bundle.join("agents"))?;
    fs::write(bundle.join("agents/reviewer.toml"), "name = \"reviewer\"\n")?;
    fs::write(
        bundle.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"indexed\"\nversion = \"1.0.0\"\n\
         description = \"test bundle\"\n\n[skills]\npath = \"skills\"\n\
         [commands]\npath = \"commands\"\n[agents]\npath = \"agents\"\n",
    )?;

    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    plugin_ui::install_active(
        &client,
        &format!("http://{addr}/v1/apps/plugins"),
        bundle.display().to_string(),
    )
    .await?;

    let listed: Value = client
        .get(format!("http://{addr}/v1/apps/plugins"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = listed["plugins"]
        .as_array()
        .and_then(|plugins| plugins.iter().find(|plugin| plugin["name"] == "indexed"))
        .and_then(|plugin| plugin["id"].as_str())
        .expect("installed plugin id")
        .to_string();
    *plugin_id.lock().expect("plugin id") = Some(id.clone());

    let commands: Value = client
        .get(format!("http://{addr}/v1/commands"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ship = commands["commands"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "ship"))
        .expect("ship row");
    assert_eq!(ship["plugin_id"], id.as_str());

    let index: Value = client
        .get(format!("http://{addr}/v1/apps/plugin-index"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let items = index["items"].as_array().expect("items");
    let find = |id: &str| items.iter().find(|item| item["id"] == id);

    let plugin = items
        .iter()
        .find(|item| item["kind"] == "plugin" && item["row"]["name"] == "indexed")
        .unwrap_or_else(|| panic!("plugin row in {index}"));
    assert_eq!(plugin["kind"], "plugin");
    assert_eq!(plugin["state"], "enabled");
    assert_eq!(plugin["adds"]["skills"], serde_json::json!(["greet"]));
    assert_eq!(plugin["adds"]["commands"], serde_json::json!(["ship"]));
    assert_eq!(plugin["adds"]["profiles"], serde_json::json!(["reviewer"]));
    assert_eq!(
        plugin["labels"],
        serde_json::json!(["skill", "command", "profile"])
    );
    assert_eq!(plugin["row"]["name"], "indexed");

    assert!(find("skill:greet").is_none());
    assert!(find("command:ship").is_none());
    let mine = find("command:mine").expect("a hand-written command is a row");
    assert_eq!(mine["source"], "user");
    assert_eq!(mine["labels"], serde_json::json!(["command"]));
    assert!(
        items
            .iter()
            .any(|item| item["kind"] == "plugin" && item["tier"] == "builtin"),
        "built-in switches are rows"
    );
    assert_eq!(
        index["unavailable"].as_array().map(Vec::len),
        Some(0),
        "{index}"
    );

    handle.abort();
    Ok(())
}
