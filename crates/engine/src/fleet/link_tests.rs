use std::fs;
use std::os::unix::fs::symlink;

use codewhale_workflow::{
    FleetDocument, NamedFleetError, ReasoningRouterError, ReasoningRouterProfile,
};
use tempfile::tempdir;

use super::exact::fleet_search_roots;
use super::store::{
    FleetFile, FleetMember, FleetOperator, FleetScope, list_fleets, load_fleet_in_scope,
    read_declared_schema, save_fleet, v2_fleet_candidates,
};
use crate::test_support::SealedHome;

const LEGACY: &str = "name = \"OUTSIDE-MARKER\"\n\n[roles]\nbuilder = \"builder\"\n";
const ROUTER: &str = "name = \"luna-low\"\nschema = \"reasoning_router\"\nschema_revision = 1\nprovider = \"openai\"\nmodel = \"gpt-5.6-luna\"\n";
const NOT_TOML: &str = "machine example.test login OUTSIDE-MARKER\n";

fn legacy(name: &str) -> String {
    format!("name = \"{name}\"\n\n[roles]\nbuilder = \"builder\"\n")
}

fn team() -> FleetFile {
    let mut fleet = FleetFile::new("Team".to_string(), None).expect("valid fleet");
    fleet.operator = Some(FleetOperator {
        provider: "deepseek".to_string(),
        model: "deepseek-v4-flash".to_string(),
        reasoning: Some("low".to_string()),
    });
    fleet.members.push(FleetMember {
        id: "scout".to_string(),
        display_name: None,
        shortlist: false,
        role: "scout".to_string(),
        provider: None,
        model: None,
        reasoning: None,
        instructions: None,
        requires: Vec::new(),
    });
    fleet
}

fn assert_refused_without_content(error: &NamedFleetError) {
    assert!(matches!(error, NamedFleetError::Io { .. }), "{error:?}");
    assert!(!error.to_string().contains("OUTSIDE-MARKER"), "{error}");
}

#[test]
fn linked_fleet_and_router_files_are_refused_whether_or_not_the_target_exists() {
    let _home = SealedHome::new();
    let ws = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::create_dir_all(ws.path().join("fleets")).unwrap();
    fs::create_dir_all(ws.path().join("routers")).unwrap();
    fs::write(outside.path().join("fleet.toml"), LEGACY).unwrap();
    fs::write(outside.path().join("router.txt"), NOT_TOML).unwrap();
    symlink(
        outside.path().join("fleet.toml"),
        ws.path().join("fleets").join("linked.toml"),
    )
    .unwrap();
    symlink(
        outside.path().join("absent.toml"),
        ws.path().join("fleets").join("dangling.toml"),
    )
    .unwrap();
    symlink(
        outside.path().join("router.txt"),
        ws.path().join("routers").join("leak.toml"),
    )
    .unwrap();
    symlink(
        outside.path().join("absent.toml"),
        ws.path().join("routers").join("gone.toml"),
    )
    .unwrap();
    let roots = fleet_search_roots(ws.path());

    for name in ["linked", "dangling"] {
        let error = FleetDocument::load_by_name(name, &roots).expect_err(name);
        assert_refused_without_content(&error);
    }
    for name in ["leak", "gone"] {
        let error = ReasoningRouterProfile::load_by_name(name, &roots).expect_err(name);
        assert!(
            matches!(error, ReasoningRouterError::Io { .. }),
            "{error:?}"
        );
        assert!(!error.to_string().contains("OUTSIDE-MARKER"), "{error}");
    }
}

#[test]
fn linked_fleet_directories_are_refused() {
    let _home = SealedHome::new();
    let ws = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::create_dir_all(outside.path().join("fleets")).unwrap();
    fs::create_dir_all(outside.path().join("routers")).unwrap();
    fs::write(
        outside.path().join("fleets").join("pair.toml"),
        legacy("pair"),
    )
    .unwrap();
    fs::write(outside.path().join("routers").join("luna-low.toml"), ROUTER).unwrap();
    symlink(outside.path().join("fleets"), ws.path().join("fleets")).unwrap();
    symlink(outside.path().join("routers"), ws.path().join("routers")).unwrap();
    let roots = fleet_search_roots(ws.path());

    FleetDocument::load_by_name("pair", &roots).expect_err("a linked fleets directory");
    ReasoningRouterProfile::load_by_name("luna-low", &roots)
        .expect_err("a linked routers directory");

    let state = tempdir().unwrap();
    fs::create_dir_all(state.path().join("fleets")).unwrap();
    fs::write(
        state.path().join("fleets").join("held.toml"),
        legacy("held"),
    )
    .unwrap();
    let linked_state = tempdir().unwrap();
    symlink(state.path(), linked_state.path().join(".codewhale")).unwrap();
    let roots = fleet_search_roots(linked_state.path());
    FleetDocument::load_by_name("workspace/held", &roots).expect_err("a linked .codewhale");
}

#[test]
fn plain_files_and_operator_owned_links_still_load() {
    let home = SealedHome::new();
    let ws = tempdir().unwrap();
    fs::create_dir_all(ws.path().join("fleets")).unwrap();
    fs::create_dir_all(ws.path().join(".codewhale").join("fleets")).unwrap();
    fs::create_dir_all(ws.path().join("routers")).unwrap();
    fs::write(ws.path().join("fleets").join("root.toml"), legacy("root")).unwrap();
    fs::write(
        ws.path()
            .join(".codewhale")
            .join("fleets")
            .join("state.toml"),
        legacy("state"),
    )
    .unwrap();
    fs::write(ws.path().join("routers").join("luna-low.toml"), ROUTER).unwrap();
    let relocated = tempdir().unwrap();
    fs::write(relocated.path().join("mine.toml"), legacy("mine")).unwrap();
    fs::create_dir_all(home.codewhale_home()).unwrap();
    symlink(relocated.path(), home.codewhale_home().join("fleets")).unwrap();
    let roots = fleet_search_roots(ws.path());

    for (name, origin) in [
        ("root", "workspace_root"),
        ("state", "workspace"),
        ("mine", "codewhale_home"),
    ] {
        let (document, id) = FleetDocument::load_by_name(name, &roots).expect(name);
        assert_eq!(document.name(), name);
        assert_eq!(id.origin, origin);
    }
    let (profile, id) = ReasoningRouterProfile::load_by_name("luna-low", &roots).expect("router");
    assert_eq!(profile.name, "luna-low");
    assert_eq!(id.origin, "workspace_root");
}

#[test]
fn the_fleet_store_refuses_linked_workspace_files_and_directories() {
    let _home = SealedHome::new();
    let source = tempdir().unwrap();
    let saved = save_fleet(&team(), FleetScope::Workspace, source.path()).expect("save");

    let ws = tempdir().unwrap();
    let fleets = ws.path().join(".codewhale").join("fleets");
    fs::create_dir_all(&fleets).unwrap();
    let linked = fleets.join(saved.file_name().expect("file name"));
    symlink(&saved, &linked).unwrap();

    assert!(v2_fleet_candidates("Team", ws.path()).is_empty());
    assert!(read_declared_schema(&linked).is_none());
    load_fleet_in_scope("Team", FleetScope::Workspace, ws.path()).expect_err("a linked Fleet file");
    assert!(
        list_fleets(ws.path())
            .iter()
            .all(|entry| entry.parse_error.is_some() || entry.path != linked)
    );

    let dir_linked = tempdir().unwrap();
    fs::create_dir_all(dir_linked.path().join(".codewhale")).unwrap();
    symlink(
        saved.parent().expect("fleets dir"),
        dir_linked.path().join(".codewhale").join("fleets"),
    )
    .unwrap();
    assert!(v2_fleet_candidates("Team", dir_linked.path()).is_empty());
    assert!(list_fleets(dir_linked.path()).is_empty());
    load_fleet_in_scope("Team", FleetScope::Workspace, dir_linked.path())
        .expect_err("a linked fleets directory");
}
