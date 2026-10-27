//! Extension host tests.
//!
//! Unit tests (protocol corpus, registry rules, tool gating) need no Node.
//! Integration tests spawn the *committed* bundle under a real Node ≥22.19:
//! they skip with a printed reason when none is found, unless
//! `CODEWHALE_EXT_HOST_TESTS=1` is set (CI), where a missing Node fails.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::{
    self, RegisterKind, RegisterParams, RegisterSpecWire, parse_core_message, parse_host_message,
};
use super::registry::{OwnerRegistry, OwnerState};
use super::tier::HostTier;
use super::{ExtensionHostManager, ExtensionHostOptions, HostAttachment, HostStatus};
use crate::command_catalog::{BuiltinCommand, CatalogGuard, CommandCatalog, UserCommand};
use crate::plugins::PluginRegistry;
use crate::plugins::activation::TestPolicyGuard;
use crate::plugins::discovery::{DiscoveryConfig, discover_with_config};
use crate::tools::spec::{ApprovalRequirement, ToolContext, ToolError, ToolSpec};
use codewhale_config::OwnerRef;

/// The integration tests below run the host on Node unless they say
/// otherwise; the Bun ones pin Bun (`bun_for_tests`).
const NODE: codewhale_config::extension_host::ExtensionHostRuntime =
    codewhale_config::extension_host::ExtensionHostRuntime::Node;

/// What the registry is told is a built-in command in the tests that are not
/// about the command table. Those that are (`commands::extension_host_tests`)
/// install the real one: this module may not depend on `crate::commands`.
#[derive(Debug)]
struct StubBuiltinCommands;

impl CommandCatalog for StubBuiltinCommands {
    fn answers_to(&self, name: &str) -> bool {
        matches!(
            name,
            "help" | "trust" | "model" | "jihua" | "zidong" | "stub-alias"
        )
    }
    fn builtins(&self) -> Vec<BuiltinCommand> {
        Vec::new()
    }
    fn user_commands(&self, _: &Path) -> Vec<UserCommand> {
        Vec::new()
    }
}

/// The stub catalog, for this thread until the guard drops.
fn stub_builtin_commands() -> CatalogGuard {
    CatalogGuard::install(Arc::new(StubBuiltinCommands))
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extension_host")
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

#[test]
fn protocol_corpus_parses_and_round_trips_in_both_directions() {
    let dir = fixtures_dir().join("protocol");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("corpus dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    let (mut valid, mut invalid) = (0, 0);
    for path in entries {
        let case: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let frame = case["frame"].clone();
        let direction = case["direction"].as_str().unwrap();
        let expect_valid = case["valid"].as_bool().unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        // Every method allows both tiers today, so a case reads the same under
        // either (the tier rule itself is `protocol::tests`).
        for tier in HostTier::ALL {
            let reencoded = match direction {
                "host_to_core" => parse_host_message(frame.clone(), tier).map(|m| m.to_value()),
                "core_to_host" => parse_core_message(frame.clone(), tier).map(|m| m.to_value()),
                other => panic!("unknown direction {other}"),
            };
            if expect_valid {
                let reencoded = reencoded.unwrap_or_else(|e| panic!("{name} ({tier:?}): {e}"));
                assert_eq!(
                    reencoded, frame,
                    "{name} ({tier:?}) must round-trip exactly"
                );
            } else {
                assert!(reencoded.is_err(), "{name} ({tier:?}) must be rejected");
            }
        }
        if expect_valid {
            let bytes = protocol::encode_frame(&frame).unwrap();
            assert_eq!(&bytes[..4], b"CWX1");
            valid += 1;
        } else {
            invalid += 1;
        }
    }
    assert!(
        valid >= 15 && invalid >= 8,
        "corpus too small: {valid} valid, {invalid} invalid"
    );
}

#[tokio::test]
async fn frames_decode_in_order_and_violations_are_typed() {
    let mut bytes = protocol::encode_frame(&json!({"a": 1})).unwrap();
    bytes.extend(protocol::encode_frame(&json!({"b": "ü"})).unwrap());
    let mut reader = bytes.as_slice();
    assert_eq!(
        protocol::read_frame(&mut reader).await.unwrap(),
        Some(json!({"a": 1}))
    );
    assert_eq!(
        protocol::read_frame(&mut reader).await.unwrap(),
        Some(json!({"b": "ü"}))
    );
    assert_eq!(protocol::read_frame(&mut reader).await.unwrap(), None);

    let mut bad = b"NOPE\0\0\0\0".as_slice();
    assert!(matches!(
        protocol::read_frame(&mut bad).await,
        Err(protocol::FrameError::BadMagic)
    ));
    let mut huge = Vec::from(*b"CWX1");
    huge.extend(((protocol::MAX_FRAME + 1) as u32).to_le_bytes());
    assert!(matches!(
        protocol::read_frame(&mut huge.as_slice()).await,
        Err(protocol::FrameError::TooLarge(_))
    ));
}

// ---------------------------------------------------------------------------
// Owner registry
// ---------------------------------------------------------------------------

pub(crate) fn fake_authority(plugin_id: &str) -> codewhale_config::PluginAuthority {
    codewhale_config::PluginAuthority {
        plugin_id: codewhale_config::PluginId(plugin_id.to_string()),
        plugin_name: plugin_id.to_string(),
        workspace: PathBuf::from("/w"),
        state_path: PathBuf::from("/s"),
        managed_policy_path: Some(PathBuf::from("/managed-policy.json")),
        managed_policy_source: Some(PathBuf::from("/managed-policy.json")),
        source_manifest: PathBuf::from("/m"),
        staged_manifest: PathBuf::from("/sm"),
        content_hash: format!("hash-{plugin_id}"),
        capability_hash: "cap".to_string(),
        state_generation: 1,
    }
}

fn register(registry: &mut OwnerRegistry, owner: &OwnerRef, name: &str) -> Result<u64, String> {
    registry.register_tool(&RegisterParams {
        scope: None,
        owner: owner.clone(),
        kind: RegisterKind::Tool,
        spec: RegisterSpecWire {
            name: name.to_string(),
            description: "d".to_string(),
            input_schema: json!({"type": "object", "properties": {}})
                .as_object()
                .cloned(),
            argument_hint: None,
        },
    })
}

#[test]
fn registry_refuses_shadowing_and_foreign_names_and_undoes_exactly_one_entry() {
    let mut registry = OwnerRegistry::new();
    registry.add_native_names(["grep_files"]);
    let a = registry
        .begin_owner(
            HostTier::Plugin,
            "a",
            "a",
            Some(fake_authority("a")),
            "hash-a",
        )
        .unwrap();
    let b = registry
        .begin_owner(
            HostTier::Plugin,
            "b",
            "b",
            Some(fake_authority("b")),
            "hash-b",
        )
        .unwrap();

    // Built-ins (static, snapshot, and case-folded) and reserved prefixes.
    for name in [
        "read_file",
        "grep_files",
        "READ",
        "tool_search",
        "mcp_x_y",
        "ext_z",
    ] {
        assert!(
            register(&mut registry, &a, name).is_err(),
            "{name} must be refused"
        );
    }
    assert!(register(&mut registry, &a, "bad name").is_err());

    let first = register(&mut registry, &a, "shared_name").unwrap();
    // Another owner cannot take it, in any case.
    assert!(register(&mut registry, &b, "Shared_Name").is_err());
    // Same owner re-registering retires the old handle.
    let second = register(&mut registry, &a, "shared_name").unwrap();
    assert_ne!(first, second);
    registry.mark_active(&a);
    registry.unregister(&a, first); // stale: must not remove the newer entry
    assert_eq!(registry.live_tools().len(), 1);
    assert!(registry.is_live(second, &a));
    // A foreign owner cannot unregister it either.
    registry.unregister(&b, second);
    assert!(registry.is_live(second, &a));

    // A stale token is refused.
    let mut stale = a.clone();
    stale.owner_token = "not-the-token".to_string();
    assert!(register(&mut registry, &stale, "other").is_err());

    // Revocation is synchronous and total.
    assert_eq!(registry.revoke_owner("a"), Some(a.clone()));
    assert!(!registry.is_live(second, &a));
    assert!(registry.live_tools().is_empty());
    assert!(register(&mut registry, &a, "after_revoke").is_err());
}

/// Names that the approval tables key by name must never reach an extension:
/// a `fetch_url` session grant for github.com is `net:github.com`, and a
/// plugin tool called `web_fetch` would otherwise get that same key.
#[test]
fn registry_refuses_names_the_approval_tables_special_case() {
    let mut registry = OwnerRegistry::new();
    let a = registry
        .begin_owner(
            HostTier::Plugin,
            "a",
            "a",
            Some(fake_authority("a")),
            "hash-a",
        )
        .unwrap();
    // Special-cased by name somewhere in the approval path; some are also
    // natives in some modes.
    for name in [
        "web_fetch",
        "exec_wait",
        "exec_interact",
        "task_shell_start",
        "web_search",
        "run_tests",
        "run_verifiers",
        "fim_edit",
        "Bash",
        "read_workspace_deps",
        "list_things",
        "get_secret",
        "start_mcp_server",
    ] {
        let refused =
            register(&mut registry, &a, name).expect_err(&format!("{name} must be refused"));
        assert!(
            refused.contains("reserved") || refused.contains("collides with a built-in"),
            "{name}: {refused}"
        );
    }
    // Not natives in any mode: only the classifier probe refuses these.
    for name in [
        "web_fetch",
        "exec_wait",
        "exec_interact",
        "read_workspace_deps",
    ] {
        let refused = register(&mut registry, &a, name).unwrap_err();
        assert!(refused.contains("reserved"), "{name}: {refused}");
    }
    // The fetch-family key really is shared by name: this is what the refusal
    // protects.
    let input = json!({"url": "https://github.com/x"});
    assert_eq!(
        crate::tools::approval_cache::build_approval_grouping_key("web_fetch", &input),
        crate::tools::approval_cache::build_approval_grouping_key("fetch_url", &input),
    );
    // Opaque names are admitted and keyed as themselves.
    for name in ["load_workspace_dependencies", "slow_wait", "probe_read"] {
        register(&mut registry, &a, name).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn registry_enforces_schema_and_count_caps() {
    let mut registry = OwnerRegistry::new();
    let a = registry
        .begin_owner(
            HostTier::Plugin,
            "a",
            "a",
            Some(fake_authority("a")),
            "hash-a",
        )
        .unwrap();
    let mut params = RegisterParams {
        scope: None,
        owner: a.clone(),
        kind: RegisterKind::Tool,
        spec: RegisterSpecWire {
            name: "big".to_string(),
            description: "x".repeat(super::registry::MAX_DESCRIPTION_BYTES + 1),
            input_schema: json!({"type": "object"}).as_object().cloned(),
            argument_hint: None,
        },
    };
    assert!(
        registry
            .register_tool(&params)
            .unwrap_err()
            .contains("description")
    );
    params.spec.description = "ok".to_string();
    params.spec.input_schema =
        json!({"type": "object", "description": "y".repeat(super::registry::MAX_SCHEMA_BYTES)})
            .as_object()
            .cloned();
    assert!(
        registry
            .register_tool(&params)
            .unwrap_err()
            .contains("schema")
    );
    params.spec.input_schema = json!({"type": "string"}).as_object().cloned();
    assert!(
        registry
            .register_tool(&params)
            .unwrap_err()
            .contains("object")
    );
    for index in 0..super::registry::MAX_TOOLS_PER_OWNER {
        register(&mut registry, &a, &format!("t{index}")).unwrap();
    }
    assert!(
        register(&mut registry, &a, "one_too_many")
            .unwrap_err()
            .contains("at most")
    );
}

/// A tool registered through the real admission path, as the registry hands it
/// to `HostToolSpec`, with `schema` as its input schema.
fn admitted_tool(schema: Value) -> Result<super::registry::ToolRegistration, String> {
    let mut registry = OwnerRegistry::new();
    let owner = registry
        .begin_owner(
            HostTier::Plugin,
            "probe",
            "probe",
            Some(fake_authority("probe")),
            "hash-probe",
        )
        .unwrap();
    registry.register_tool(&RegisterParams {
        scope: None,
        owner: owner.clone(),
        kind: RegisterKind::Tool,
        spec: RegisterSpecWire {
            name: "probe_tool".to_string(),
            description: "d".to_string(),
            input_schema: schema.as_object().cloned(),
            argument_hint: None,
        },
    })?;
    assert!(registry.mark_active(&owner));
    Ok(registry.live_tools().remove(0))
}

#[test]
fn an_uncompilable_tool_schema_is_refused_at_registration_with_a_reason() {
    for (label, schema) in [
        (
            "an unknown type",
            json!({"type": "object", "properties": {"a": {"type": "nonsense"}}}),
        ),
        (
            "an external $ref the core will not fetch",
            json!({"type": "object", "properties": {"a": {"$ref": "https://example.invalid/schema.json"}}}),
        ),
        (
            "a required list that is not a list",
            json!({"type": "object", "required": "a"}),
        ),
    ] {
        let reason = admitted_tool(schema).expect_err(label);
        assert!(
            reason.contains("probe_tool") && reason.contains("not a valid JSON Schema"),
            "{label}: {reason}"
        );
    }
    admitted_tool(json!({"type": "object", "properties": {"a": {"type": "string"}}}))
        .expect("a valid schema is admitted");
}

/// What the model gets back, and what the host never sees: the tool checks the
/// input against its registered schema in `prepare` (before any approval
/// card) and again in `execute`.
#[tokio::test]
async fn tool_input_is_checked_against_the_registered_schema_before_approval_and_execution() {
    let registration = admitted_tool(json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "count": {"type": "integer", "minimum": 0}
        },
        "required": ["name"],
        "additionalProperties": false
    }))
    .unwrap();
    // No host is running: a call that reached it would say so (`NotAvailable`).
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    let tool = super::tool::HostToolSpec::new(registration, Arc::clone(&manager.shared));
    let context = ToolContext::new(Path::new("/w"));

    let rejected = |input: Value, expect: &[&str]| {
        let error = tool.prepare(input.clone(), &context).unwrap_err();
        let ToolError::InvalidInput { message } = error else {
            panic!("{input}: not an invalid-input error: {error:?}");
        };
        assert!(
            message.contains("probe_tool") && expect.iter().all(|part| message.contains(part)),
            "{input}: {message}"
        );
        input
    };
    // Valid input passes `prepare`, with the Rust-composed approval card.
    let prepared = tool
        .prepare(json!({"name": "x", "count": 2}), &context)
        .expect("valid input is admitted");
    assert_eq!(prepared.approval, ApprovalRequirement::Required);
    // An extra property.
    let extra = rejected(json!({"name": "x", "surprise": true}), &["surprise"]);
    // A wrong type, and the path of the field.
    let wrong_type = rejected(json!({"name": 7}), &["name"]);
    rejected(json!({"name": "x", "count": -1}), &["count"]);
    // A missing required field, and a non-object.
    rejected(json!({}), &["name"]);
    rejected(json!("name"), &[]);

    // `execute` refuses the same inputs without touching the host (which is
    // not running, so reaching it would be `NotAvailable`)...
    for input in [extra, wrong_type] {
        let error = tool.execute(input.clone(), &context).await.unwrap_err();
        assert!(
            matches!(error, ToolError::InvalidInput { .. }),
            "{input}: {error:?}"
        );
    }
    assert_eq!(manager.spawn_attempts(), 0);
    // ...while valid input gets past the check and meets the dead host.
    let error = tool
        .execute(json!({"name": "x"}), &context)
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::NotAvailable { .. }), "{error:?}");
}

// ---------------------------------------------------------------------------
// Licence notices for the embedded bundle
// ---------------------------------------------------------------------------

/// Every `node_modules/<package>` the bundle was built from, read from the
/// bundler's own `// node_modules/...` markers in the embedded bundle (not
/// from the generator that writes the notices).
fn bundled_packages() -> BTreeSet<String> {
    let mut packages = BTreeSet::new();
    for bytes in [
        super::BUNDLE,
        include_bytes!("../../extension-host/dist/builtin/mcp.mjs").as_slice(),
    ] {
        let text = std::str::from_utf8(bytes).expect("the bundle is UTF-8");
        for line in text.lines() {
            let Some(path) = line.strip_prefix("// ") else {
                continue;
            };
            let Some((_, after)) = path.rsplit_once("node_modules/") else {
                continue;
            };
            let mut parts = after.split('/');
            let first = parts.next().unwrap_or_default();
            let name = if first.starts_with('@') {
                format!("{first}/{}", parts.next().unwrap_or_default())
            } else {
                first.to_string()
            };
            packages.insert(name);
        }
    }
    packages
}

/// `name@version` of every package the embedded notices list.
fn noticed_packages() -> Vec<(String, String)> {
    let text = std::str::from_utf8(super::NOTICES).expect("the notices are UTF-8");
    let listed = text
        .split_once("\nPackages:\n")
        .expect("the notices list their packages")
        .1;
    listed
        .lines()
        .take_while(|line| line.starts_with("  "))
        .map(|line| {
            let entry = line.trim().split(" (").next().unwrap();
            let (name, version) = entry.rsplit_once('@').expect("name@version");
            (name.to_string(), version.to_string())
        })
        .collect()
}

#[test]
fn materialized_bundle_directory_carries_its_licence_notices() {
    let home = tempfile::tempdir().unwrap();
    let bundle = super::materialize_bundle(home.path()).unwrap();
    let dir = bundle.parent().unwrap().to_path_buf();
    assert_eq!(
        dir,
        super::supervisor::bundle_dir(home.path(), super::bundle_sha256()),
        "the notices live in the bundle's own digest-named directory"
    );
    let notices = dir.join("LICENSES.txt");
    assert_eq!(std::fs::read(&notices).unwrap(), super::NOTICES);
    assert_eq!(std::fs::read(&bundle).unwrap(), super::BUNDLE);
    let builtin = dir.join("builtin/mcp.mjs");
    let builtin_bytes = include_bytes!("../../extension-host/dist/builtin/mcp.mjs");
    assert_eq!(std::fs::read(&builtin).unwrap(), builtin_bytes);
    let pinned = super::tier::BUILTIN_MODULES
        .iter()
        .find(|module| module.id == "mcp")
        .unwrap();
    assert_eq!(
        super::hex(Sha256::digest(builtin_bytes)),
        pinned.source_sha256
    );
    assert!(
        std::str::from_utf8(super::NOTICES)
            .unwrap()
            .contains("Copyright (c) 2021-present Shigma"),
        "the notices carry the licence text, not only package names"
    );
    // Written like the bundle: read-only, no staging files left behind.
    #[cfg(unix)]
    for path in [&bundle, &notices, &builtin] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o400,
            "{}",
            path.display()
        );
    }
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["LICENSES.txt", "builtin", "codewhale-extension-host.mjs"]
    );
    assert_eq!(
        std::fs::read_dir(dir.join("builtin")).unwrap().count(),
        super::tier::BUILTIN_MODULES.len()
    );
    for module in super::tier::BUILTIN_MODULES {
        let path = dir.join("builtin").join(format!("{}.mjs", module.id));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(super::hex(Sha256::digest(&bytes)), module.source_sha256);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o400
            );
        }
    }

    // A directory from a build that wrote only the bundle gets its notices; a
    // tampered or replaced notices file is rewritten, never trusted.
    std::fs::remove_file(&notices).unwrap();
    super::materialize_bundle(home.path()).unwrap();
    assert_eq!(std::fs::read(&notices).unwrap(), super::NOTICES);
    std::fs::remove_file(&notices).unwrap();
    std::fs::write(&notices, "tampered").unwrap();
    super::materialize_bundle(home.path()).unwrap();
    assert_eq!(std::fs::read(&notices).unwrap(), super::NOTICES);
    #[cfg(unix)]
    {
        let elsewhere = home.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, "not the notices").unwrap();
        std::fs::remove_file(&notices).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &notices).unwrap();
        super::materialize_bundle(home.path()).unwrap();
        assert!(
            !std::fs::symlink_metadata(&notices)
                .unwrap()
                .file_type()
                .is_symlink(),
            "a symlink at the notices name is replaced, not followed"
        );
        assert_eq!(std::fs::read(&notices).unwrap(), super::NOTICES);
        assert_eq!(
            std::fs::read_to_string(&elsewhere).unwrap(),
            "not the notices",
            "the symlink's target is never written through"
        );
    }
}

#[test]
fn every_package_in_the_bundle_has_a_licence_notice_and_a_third_party_entry() {
    let bundled = bundled_packages();
    assert!(
        bundled.contains("@deepseek-ai/cordis"),
        "the scan of the bundle found no packages: {bundled:?}"
    );
    let noticed = noticed_packages();
    let third_party = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../THIRD_PARTY_NOTICES.md"),
    )
    .expect("THIRD_PARTY_NOTICES.md");
    let notice_text = std::str::from_utf8(super::NOTICES).unwrap();
    for package in &bundled {
        let Some((_, version)) = noticed.iter().find(|(name, _)| name == package) else {
            panic!("`{package}` is in the host bundle but not in dist/LICENSES.txt: {noticed:?}");
        };
        assert!(
            third_party.contains(&format!("`{package}` {version}")),
            "`{package}` {version} is bundled but THIRD_PARTY_NOTICES.md does not list it"
        );
        let heading = format!("{package}@{version} (");
        assert!(
            notice_text.matches(&heading).count() >= 2,
            "dist/LICENSES.txt lists `{package}` but carries no section with its licence text"
        );
    }
    // The verbatim excerpts are not node_modules inputs; the notices still
    // name them, and THIRD_PARTY_NOTICES.md must too.
    for (package, version) in &noticed {
        assert!(
            third_party.contains(&format!("`{package}` {version}")),
            "`{package}` {version} is in dist/LICENSES.txt but not in THIRD_PARTY_NOTICES.md"
        );
    }
}

// ---------------------------------------------------------------------------
// Integration: real Node, real bundle
// ---------------------------------------------------------------------------

/// A Node for the integration tests, or `None` (skip) when there is none and
/// the tests were not explicitly required.
pub(crate) fn node_for_tests(test: &str) -> Option<PathBuf> {
    let resolution = codewhale_dependencies::resolve_extension_host_runtime(
        codewhale_config::extension_host::ExtensionHostRuntime::Node,
        None,
        None,
        crate::extension_host::bundle_sha256(),
    );
    match resolution.selected.as_ref() {
        Some(runtime) => Some(runtime.path.clone()),
        None if std::env::var_os("CODEWHALE_EXT_HOST_TESTS").is_some() => panic!(
            "{test}: CODEWHALE_EXT_HOST_TESTS is set but no Node ^22.19 || >=24 was found: {}",
            resolution.failure()
        ),
        None => {
            eprintln!(
                "skipping {test}: no Node ^22.19 || >=24 ({})",
                resolution.failure()
            );
            None
        }
    }
}

/// Fixture plugins installed into a private user plugin dir through the
/// reviewed installer (`plugins::install`, local path), then reviewed
/// (trusted) and enabled through the real registry. Callers must hold a
/// `TestPolicyGuard::extension_host(true)` on this thread.
pub(crate) struct FixturePlugins {
    _temp: tempfile::TempDir,
    pub config: DiscoveryConfig,
    pub root: PathBuf,
    /// The stub built-in command catalog, so a fixture plugin's commands can
    /// register; a test about the real table installs its own after this.
    _commands: CatalogGuard,
}

impl FixturePlugins {
    pub(crate) async fn new(names: &[&str]) -> Self {
        use crate::plugins::install::{
            DEFAULT_MAX_SIZE_BYTES, PluginInstallOutcome, PluginInstallSource, install,
        };
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        let user = temp.path().join("user");
        std::fs::create_dir_all(&workspace).unwrap();
        for name in names {
            let outcome = install(
                PluginInstallSource::LocalPath(fixtures_dir().join(name)),
                &user,
                DEFAULT_MAX_SIZE_BYTES,
                &codewhale_net::network_policy::NetworkPolicy::default(),
                false,
                &|_| None,
            )
            .await
            .unwrap_or_else(|error| panic!("install {name}: {error:#}"));
            assert!(
                matches!(outcome, PluginInstallOutcome::Installed(ref installed) if installed.name == *name),
                "install {name}: {outcome:?}"
            );
        }
        let config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: user,
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: Vec::new(),
            state_path: temp.path().join("state/plugin-state.json"),
        };
        let mut registry = discover_with_config(&config);
        for name in names {
            registry
                .trust(name)
                .unwrap_or_else(|e| panic!("trust {name}: {e}"));
            registry
                .enable(name)
                .unwrap_or_else(|e| panic!("enable {name}: {e}"));
        }
        let root = temp.path().join("home");
        let fixture = Self {
            _temp: temp,
            config,
            root,
            _commands: stub_builtin_commands(),
        };
        let registry = fixture.registry();
        for name in names {
            assert!(
                registry.is_active(name),
                "{name} must be active under policy v4"
            );
        }
        fixture
    }

    pub(crate) fn registry(&self) -> Arc<PluginRegistry> {
        Arc::new(discover_with_config(&self.config))
    }

    pub(crate) fn disable(&self, name: &str) -> Arc<PluginRegistry> {
        let mut registry = discover_with_config(&self.config);
        registry.disable(name).unwrap();
        self.registry()
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.config.workspace
    }

    pub(crate) fn manager(&self, node: PathBuf) -> Arc<ExtensionHostManager> {
        Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: NODE,
            node_override: Some(node),
            root: Some(self.root.clone()),
            ..Default::default()
        }))
    }
}

pub(crate) fn host_tool(
    engine: &HostAttachment,
    workspace: &Path,
    name: &str,
) -> Arc<dyn ToolSpec> {
    let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
        .build(ToolContext::new(workspace).with_plugin_registry(engine.plugin_view()));
    let installed = engine.install_tools(&mut registry);
    assert!(
        installed.contains(&name.to_string()),
        "{name} not installed: {installed:?}"
    );
    registry.get(name).unwrap()
}

/// Start budget for `dsh_plugin_runs_end_to_end_behind_the_approval_gate`:
/// bundle materialization, the runtime probe, the launch plan (on Linux, the
/// bwrap probe), spawn, handshake and one plugin's activation, under
/// nextest's full-core load on every CI OS. The host alone is ready in
/// ~50 ms (the JS suite's `READY_BUDGET_MS` gates that), but Windows CI
/// under load has taken over 5 s to hand-shake (`HANDSHAKE_DEADLINE`).
/// 20 s fails a start drifting toward that 30 s deadline, whose miss
/// disables every extension for the session, without failing on CI load.
const HOST_START_BUDGET: Duration = Duration::from_secs(20);

/// Resident-size budget for a host with one plugin active, summed over its
/// process tree (bwrap's two processes included on Linux). 160 MiB is 2.4x
/// the largest idle host measured (67 MB, Node in a Linux container:
/// `supervisor::HOST_MEMORY_CAP`; 57 MiB Node 26 and 41 MiB Bun 1.4 with this
/// plugin on macOS arm64, 2026-09-30) and far below the 1 GiB cap. Not
/// checked where there is no `ps` (Windows).
const HOST_RSS_BUDGET_MIB: u64 = 160;

/// Resident size in KiB of `pid` and every process under it — under bwrap,
/// `pid` is bwrap's and the runtime two levels down — from `ps`; `None` where
/// `ps` is missing or does not list `pid`.
fn tree_rss_kib(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,rss="])
        .output()
        .ok()?;
    let rows: Vec<[u64; 3]> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let fields: Vec<u64> = line
                .split_whitespace()
                .filter_map(|field| field.parse().ok())
                .collect();
            <[u64; 3]>::try_from(fields).ok()
        })
        .collect();
    let root = u64::from(pid);
    if !rows.iter().any(|row| row[0] == root) {
        return None;
    }
    let mut members = vec![root];
    let mut next = 0;
    while let Some(&parent) = members.get(next) {
        members.extend(
            rows.iter()
                .filter(|row| row[1] == parent && row[0] != parent)
                .map(|row| row[0]),
        );
        next += 1;
    }
    Some(
        rows.iter()
            .filter(|row| members.contains(&row[0]))
            .map(|row| row[2])
            .sum(),
    )
}

#[tokio::test]
async fn dsh_plugin_runs_end_to_end_behind_the_approval_gate() {
    let Some(node) = node_for_tests("dsh_plugin_runs_end_to_end_behind_the_approval_gate") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["dsh-workspace-deps"]).await;
    let manager = fixture.manager(node);
    assert_eq!(
        manager.status(),
        HostStatus::Idle,
        "nothing starts before sync"
    );

    let started = Instant::now();
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let elapsed = started.elapsed();
    let pid = manager.host_pid().expect("host running");
    let rss = tree_rss_kib(pid);
    eprintln!(
        "extension host: spawn + handshake + activation {:.1} ms (budget {HOST_START_BUDGET:?}); RSS {} KiB (budget {HOST_RSS_BUDGET_MIB} MiB)",
        elapsed.as_secs_f64() * 1000.0,
        rss.map_or_else(|| "?".to_string(), |kib| kib.to_string())
    );
    assert!(
        elapsed <= HOST_START_BUDGET,
        "the extension host took {elapsed:?} to start, over its {HOST_START_BUDGET:?} budget"
    );
    match rss {
        Some(kib) => assert!(
            kib <= HOST_RSS_BUDGET_MIB * 1024,
            "the extension host is {kib} KiB resident, over its {HOST_RSS_BUDGET_MIB} MiB budget"
        ),
        None => eprintln!("extension host RSS budget not checked: no `ps` listing here"),
    }
    assert_eq!(
        manager.live_tool_names(),
        vec!["load_workspace_dependencies"]
    );
    assert_eq!(
        manager.owner_state(
            fixture
                .registry()
                .get("dsh-workspace-deps")
                .unwrap()
                .id
                .as_str()
        ),
        Some(OwnerState::Active)
    );

    let tool = host_tool(&engine, fixture.workspace(), "load_workspace_dependencies");
    assert_eq!(tool.registration_origin(), "extension:dsh-workspace-deps");
    // The plugin declares `presentCall: kind 'read'`; approval stays Required.
    assert_eq!(
        tool.approval_requirement_for(&json!({})),
        ApprovalRequirement::Required
    );
    assert!(!tool.is_read_only_for(&json!({})));
    assert!(tool.defer_loading());
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let prepared = tool.prepare(json!({}), &context).unwrap();
    assert_eq!(prepared.approval, ApprovalRequirement::Required);
    assert!(
        prepared
            .description
            .contains("extension:dsh-workspace-deps")
    );

    let result = tool.execute(json!({}), &context).await.unwrap();
    assert!(result.success);
    let payload: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(payload["pythonDistributions"]["numpy"], "2.1.0");
    assert!(payload["python"].as_str().unwrap().contains("dependencies"));
    manager.shutdown().await;
}

/// A manifest may declare several `native` entries. They activate under one
/// owner, in order. Entry-scoped failure retires that entry without taking
/// away a sibling selected by another caller.
#[tokio::test]
async fn a_plugin_with_two_native_entries_activates_both_under_one_owner() {
    let Some(node) = node_for_tests("a_plugin_with_two_native_entries") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["two-entries", "two-entries-failing"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let registry = fixture.registry();
    let id = |name: &str| registry.get(name).unwrap().id.as_str().to_string();

    // Both entries are live under the one owner.
    assert_eq!(
        manager.owner_state(&id("two-entries")),
        Some(OwnerState::Active)
    );
    let mut tools = manager.live_tool_names();
    tools.sort();
    assert_eq!(tools, ["tef_first", "two_first", "two_second"]);
    assert_eq!(manager.live_command_names(), ["two-hello"]);
    let report = manager.owner_report(&id("two-entries")).unwrap();
    assert!(
        report.diagnostics.iter().any(
            |line| line.contains("tools: two_first, two_second") && line.contains("/two-hello")
        ),
        "{:?}",
        report.diagnostics
    );
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    for (name, answer) in [("two_first", "first"), ("two_second", "second")] {
        let result = host_tool(&engine, fixture.workspace(), name)
            .execute(json!({}), &context)
            .await
            .unwrap();
        assert_eq!(result.content, answer);
    }

    // The failing entry is retired while its already-active sibling survives.
    let failing = id("two-entries-failing");
    assert_eq!(manager.owner_state(&failing), Some(OwnerState::Active));
    assert!(manager.live_tool_names().contains(&"tef_first".to_string()));
    assert!(
        manager
            .shared
            .registry
            .lock()
            .unwrap()
            .owner(&failing)
            .unwrap()
            .scopes
            .values()
            .any(|state| matches!(state, OwnerState::Failed(_)))
    );
    // The healthy plugin sharing the host is untouched, and a later reconcile
    // does not retry the failed bytes.
    engine.sync().await.unwrap();
    let mut tools = manager.live_tool_names();
    tools.sort();
    assert_eq!(tools, ["tef_first", "two_first", "two_second"]);
    manager.shutdown().await;
}

fn plugin_settings(
    name: &str,
    config: &str,
) -> std::collections::BTreeMap<String, codewhale_config::extension_host::PluginSettings> {
    std::collections::BTreeMap::from([(
        name.to_string(),
        codewhale_config::extension_host::PluginSettings {
            config: Some(toml::from_str(config).expect("config TOML")),
        },
    )])
}

/// Run an extension tool and parse its JSON answer.
async fn call_json(
    tool: &Arc<dyn ToolSpec>,
    input: Value,
    workspace: &Path,
    engine: &HostAttachment,
) -> Value {
    let result = tool
        .execute(
            input,
            &ToolContext::new(workspace).with_plugin_registry(engine.plugin_view()),
        )
        .await
        .unwrap_or_else(|error| panic!("{error:?}"));
    serde_json::from_str(&result.content).expect("a JSON answer")
}

/// The plugin's settings come from the user's config file, are bounded, and
/// the keys (never the values) are what `/plugin show` can list.
#[test]
fn plugin_settings_are_read_from_the_user_config_and_bounded() {
    use super::plugin_config::{MAX_PLUGIN_CONFIG_BYTES, PluginConfigs};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // Other keys are none of this reader's business; `[plugins]` is.
    std::fs::write(
        &path,
        "model = \"x\"\n[plugins.\"greeter\".config]\ngreeting = \"Hi\"\nlimit = 3\n[plugins.\"greeter\".config.nested]\ntags = [\"a\", \"b\"]\n[plugins.\"quiet\"]\n",
    )
    .unwrap();
    let settings = codewhale_runtime::config::read_plugin_settings(&path).unwrap();
    let mut configs = PluginConfigs::default();
    configs.replace(&settings, Some(path.clone()));
    assert_eq!(configs.source(), Some(path.clone()));
    let greeter = configs.select("greeter").unwrap();
    assert_eq!(
        greeter.value,
        json!({"greeting": "Hi", "limit": 3, "nested": {"tags": ["a", "b"]}})
    );
    // Keys only, in order; a plugin with a table but no config has nothing to list.
    assert_eq!(
        configs.summary("greeter").unwrap().unwrap(),
        ["greeting", "limit", "nested"]
    );
    assert!(configs.summary("quiet").is_none());
    assert!(configs.summary("nobody").is_none());
    // No settings is an empty object, with a digest of its own.
    let none = configs.select("nobody").unwrap();
    assert_eq!(none.value, json!({}));
    assert_ne!(none.hash, greeter.hash);
    // The digest follows the value: equal for the same settings, new for a change.
    let mut again = PluginConfigs::default();
    again.replace(&settings, None);
    assert_eq!(again.select("greeter").unwrap().hash, greeter.hash);
    assert_eq!(again.source(), None, "a reload names no new source");
    configs.replace_reloaded(&plugin_settings("greeter", "greeting = \"Yo\""));
    assert_ne!(configs.select("greeter").unwrap().hash, greeter.hash);
    assert_eq!(
        configs.source(),
        Some(path.clone()),
        "the source survives a reload"
    );

    // A missing file has no settings; an unreadable shape or a stray key fails loudly.
    assert!(
        codewhale_runtime::config::read_plugin_settings(&dir.path().join("absent.toml"))
            .unwrap()
            .is_empty()
    );
    std::fs::write(&path, "[plugins.\"greeter\"]\nenabled = true\n").unwrap();
    assert!(
        codewhale_runtime::config::read_plugin_settings(&path)
            .unwrap_err()
            .contains("cannot parse")
    );

    // Refusals name the plugin and the rule; the config is never half-delivered.
    let refuse = |config: &str| -> String {
        let mut configs = PluginConfigs::default();
        configs.replace(&plugin_settings("greeter", config), None);
        configs.select("greeter").unwrap_err()
    };
    let big = refuse(&format!(
        "blob = \"{}\"",
        "x".repeat(MAX_PLUGIN_CONFIG_BYTES)
    ));
    assert!(
        big.contains("greeter") && big.contains("byte limit"),
        "{big}"
    );
    let at_limit = format!("blob = \"{}\"", "x".repeat(MAX_PLUGIN_CONFIG_BYTES - 20));
    let mut ok = PluginConfigs::default();
    ok.replace(&plugin_settings("greeter", &at_limit), None);
    assert!(
        ok.select("greeter").is_ok(),
        "just under the cap is accepted"
    );
    let when = refuse("when = 1979-05-27T07:32:00Z");
    assert!(when.contains("date-time"), "{when}");
    let deep = refuse(&format!("x = {}1{}", "[".repeat(20), "]".repeat(20)));
    assert!(deep.contains("nests deeper"), "{deep}");
    // A refused config has a digest too, so it is not retried every turn but is once the file changes.
    let refused: Result<super::plugin_config::PluginConfig, String> = Err(big);
    assert!(super::plugin_config::activation_hash(&refused).starts_with("refused:"));
}

/// The plugin's context in a real host: its settings (checked by its own
/// `Config` schema), the workspace of each call, and its own data directory;
/// a change of settings re-activates it under a new generation; a refused one
/// fails it with the reason.
#[tokio::test]
async fn plugin_context_reaches_the_plugin_and_changed_settings_reactivate_it() {
    let Some(node) = node_for_tests("plugin_context_reaches_the_plugin") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["plugin-context"]).await;
    let manager = fixture.manager(node);
    let _manager = super::TestManagerGuard::install(Arc::clone(&manager));
    let id = fixture
        .registry()
        .get("plugin-context")
        .unwrap()
        .id
        .as_str()
        .to_string();
    let generation = |manager: &ExtensionHostManager| {
        manager
            .shared
            .registry
            .lock()
            .unwrap()
            .owner(&id)
            .map(|entry| entry.owner.generation)
    };
    manager.set_plugin_settings(
        &plugin_settings("plugin-context", "greeting = \"Hi\""),
        None,
    );
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    assert_eq!(manager.owner_state(&id), Some(OwnerState::Active));
    let first_generation = generation(&manager).unwrap();

    let data_dir = super::supervisor::plugin_data_dir(&fixture.root, &id, "plugin-context");
    assert!(
        data_dir.is_dir(),
        "the core made the plugin's directory before activation"
    );
    assert!(
        data_dir.starts_with(fixture.root.join("extension-host/data/plugins")),
        "{}",
        data_dir.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&data_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    let probe = host_tool(&engine, fixture.workspace(), "ctx_probe");
    let seen = call_json(&probe, json!({}), fixture.workspace(), &engine).await;
    // The plugin's own schema supplied the default for `limit`.
    assert_eq!(seen["config"], json!({"greeting": "Hi", "limit": 3}));
    assert_eq!(seen["workspace"], fixture.workspace().to_str().unwrap());
    assert_eq!(seen["dataDir"], data_dir.to_str().unwrap());
    // The workspace is the call's own, not the process's or the plugin's.
    let elsewhere = fixture.workspace().join("elsewhere");
    let seen = call_json(&probe, json!({}), &elsewhere, &engine).await;
    assert_eq!(seen["workspace"], elsewhere.to_str().unwrap());
    // Nothing else about the machine is in the context.
    assert_eq!(
        seen["keys"],
        json!(["args", "callId", "dataDir", "signal", "workspace"])
    );
    // The schema refuses an unknown field before the host is asked.
    let error = probe
        .execute(
            json!({"home": true}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::InvalidInput { .. }), "{error:?}");

    // The plugin can write in its own directory, inside the host's sandbox.
    let note = host_tool(&engine, fixture.workspace(), "ctx_note");
    let written = call_json(
        &note,
        json!({"text": "remember"}),
        fixture.workspace(),
        &engine,
    )
    .await;
    assert_eq!(written["file"], data_dir.join("note.txt").to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(data_dir.join("note.txt")).unwrap(),
        "remember"
    );

    // A command is told the workspace it was loaded for, and the same directory.
    let entries = manager.commands_for_plugins(engine.plugin_view().as_ref());
    let command = entries
        .first()
        .expect("the plugin's command is live")
        .reference();
    let super::command::CommandOutcome::Show { text } =
        super::command::run(&manager.shared, &command, "", None)
            .await
            .unwrap()
    else {
        panic!("a show answer");
    };
    let said: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(said["workspace"], fixture.workspace().to_str().unwrap());
    assert_eq!(said["dataDir"], data_dir.to_str().unwrap());
    assert_eq!(said["config"]["greeting"], "Hi");

    // What `/plugin show` can list: keys, not values.
    assert_eq!(
        manager
            .plugin_config_summary("plugin-context")
            .unwrap()
            .unwrap(),
        ["greeting"]
    );

    // Unchanged settings never churn the owner...
    manager.set_plugin_settings(
        &plugin_settings("plugin-context", "greeting = \"Hi\""),
        None,
    );
    engine.sync().await.unwrap();
    assert_eq!(generation(&manager), Some(first_generation));
    // ...a changed value is a new generation with the new value...
    manager.set_plugin_settings(
        &plugin_settings("plugin-context", "greeting = \"Yo\""),
        None,
    );
    engine.sync().await.unwrap();
    assert_eq!(manager.owner_state(&id), Some(OwnerState::Active));
    assert!(generation(&manager).unwrap() > first_generation);
    let probe = host_tool(&engine, fixture.workspace(), "ctx_probe");
    assert_eq!(
        call_json(&probe, json!({}), fixture.workspace(), &engine).await["config"]["greeting"],
        "Yo"
    );
    // The same directory serves every generation: the note survived.
    assert_eq!(
        std::fs::read_to_string(data_dir.join("note.txt")).unwrap(),
        "remember"
    );

    // ...a config the plugin's own schema refuses fails it with the field named
    // and leaves nothing live...
    manager.set_plugin_settings(&plugin_settings("plugin-context", "limit = 99"), None);
    engine.sync().await.unwrap();
    assert!(
        matches!(manager.owner_state(&id), Some(OwnerState::Failed(ref reason)) if reason.contains("limit")),
        "{:?}",
        manager.owner_state(&id)
    );
    assert!(manager.live_tool_names().is_empty());
    assert!(
        manager
            .commands_for_plugins(engine.plugin_view().as_ref())
            .is_empty()
    );
    // ...so does one over the size cap (refused before the host is asked), and
    // `/plugin show` says why...
    manager.set_plugin_settings(
        &plugin_settings(
            "plugin-context",
            &format!(
                "blob = \"{}\"",
                "x".repeat(super::plugin_config::MAX_PLUGIN_CONFIG_BYTES)
            ),
        ),
        None,
    );
    engine.sync().await.unwrap();
    assert!(
        matches!(manager.owner_state(&id), Some(OwnerState::Failed(ref reason)) if reason.contains("byte limit")),
        "{:?}",
        manager.owner_state(&id)
    );
    assert!(
        manager
            .plugin_config_summary("plugin-context")
            .unwrap()
            .is_err()
    );
    // ...and fixing the settings brings it back, without a restart.
    manager.set_plugin_settings(
        &plugin_settings("plugin-context", "greeting = \"Hello again\""),
        None,
    );
    engine.sync().await.unwrap();
    assert_eq!(manager.owner_state(&id), Some(OwnerState::Active));
    manager.shutdown().await;
}

#[tokio::test]
async fn execute_tools_refuses_extension_tools_before_any_host_call() {
    let Some(node) = node_for_tests("execute_tools_refuses_extension_tools_before_any_host_call")
    else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
        .build(ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()));
    engine.install_tools(&mut registry);
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let started = Instant::now();
    let result = crate::tools::codemode::execute_tools_tool(
        &json!({"code": "return await tools.call('slow_wait', { ms: 5000 })"}),
        &registry,
        &context,
    )
    .await
    .unwrap();
    // Refused at the gate: had the call reached the host it would take 5 s.
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(!result.success, "{}", result.content);
    assert!(
        result.content.contains("can mutate") || result.content.contains("needs approval"),
        "{}",
        result.content
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn disabling_mid_call_revokes_at_once_and_teardown_waits_for_async_disposers() {
    let Some(node) = node_for_tests("disabling_mid_call") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let tool = host_tool(&engine, fixture.workspace(), "slow_wait");
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let call = tokio::spawn(async move { tool.execute(json!({}), &context).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let disabled = fixture.disable("slow-tool");
    // The revocation scan (a full re-hash of the staged tree) runs first;
    // the clock for the 500 ms bound starts when the registry drops the
    // handle, which is the moment revocation takes effect. A plain thread
    // watches for it, because this runtime is single-threaded (the policy
    // override is thread-local) and would only look when `sync` yields.
    let watcher = {
        let manager = Arc::clone(&manager);
        std::thread::spawn(move || {
            let started = Instant::now();
            while !manager.live_tool_names().is_empty() {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "revocation never happened"
                );
                std::thread::yield_now();
            }
            Instant::now()
        })
    };
    let sync_started = Instant::now();
    engine.set_plugins(disabled);
    let sync = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.reconcile().await })
    };
    let outcome = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("call resolves")
        .unwrap();
    let resolved_at = Instant::now();
    let revoked_at = watcher.join().unwrap();
    let call_resolved = resolved_at.saturating_duration_since(revoked_at);
    assert!(
        matches!(outcome, Err(ToolError::Cancelled { .. })),
        "{outcome:?}"
    );
    eprintln!(
        "extension host: in-flight call resolved as cancelled {:.1} ms after revocation",
        call_resolved.as_secs_f64() * 1000.0
    );
    assert!(
        call_resolved < Duration::from_millis(500),
        "{call_resolved:?}"
    );
    sync.await.unwrap().unwrap();
    let teardown = sync_started.elapsed();
    assert!(
        teardown >= Duration::from_millis(300),
        "ack must wait for the 300 ms async disposer (got {teardown:?})"
    );
    let diagnostics = manager.diagnostics();
    assert!(
        !diagnostics.iter().any(|d| d.contains("teardown")),
        "disposed with nothing leaked: {diagnostics:?}"
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn killed_host_fails_calls_once_and_replays_with_fresh_owners() {
    let Some(node) = node_for_tests("killed_host") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    assert_eq!(manager.spawn_attempts(), 1);
    let pid = manager.host_pid().unwrap();
    let generation = manager
        .shared
        .ready_host(HostTier::Plugin)
        .unwrap()
        .generation;
    let tool = host_tool(&engine, fixture.workspace(), "slow_wait");
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let call = tokio::spawn(async move { tool.execute(json!({}), &context).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let outcome = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("call resolves")
        .unwrap();
    match outcome {
        Err(ToolError::NotAvailable { message }) => {
            assert!(message.contains("extension host exited"), "{message}")
        }
        other => panic!("expected a typed not-available error, got {other:?}"),
    }
    wait_host(&manager, || {
        manager.spawn_attempts() == 2 && manager.live_tool_names().contains(&"slow_wait".into())
    })
    .await;
    assert!(matches!(manager.status(), HostStatus::Ready { .. }));
    assert!(
        manager
            .shared
            .ready_host(HostTier::Plugin)
            .unwrap()
            .generation
            > generation
    );
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .len(),
        1
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn approval_providing_plugin_fails_activation_and_leaves_nothing_registered() {
    let Some(node) = node_for_tests("approval_providing_plugin") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["refuses-approval", "clash-native"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let registry = fixture.registry();
    for (name, needle) in [
        ("refuses-approval", "approval"),
        ("clash-native", "read_file"),
    ] {
        let id = registry.get(name).unwrap().id.as_str().to_string();
        match manager.owner_state(&id) {
            Some(OwnerState::Failed(reason)) => {
                assert!(reason.contains(needle), "{name}: {reason}")
            }
            other => panic!("{name}: expected failed activation, got {other:?}"),
        }
    }
    assert!(manager.live_tool_names().is_empty());
    // A failed activation of the same bytes is not retried every turn.
    let attempts = manager.spawn_attempts();
    engine.sync().await.unwrap();
    assert_eq!(manager.spawn_attempts(), attempts);
    manager.shutdown().await;
}

/// Stands in for a `~/.codewhale/tools` script tool.
struct FakeScriptTool;

#[async_trait::async_trait]
impl ToolSpec for FakeScriptTool {
    fn name(&self) -> &str {
        "fixture_script_tool"
    }
    fn registration_origin(&self) -> std::borrow::Cow<'_, str> {
        "plugin script fixture_script_tool".into()
    }
    fn description(&self) -> &str {
        "script"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn capabilities(&self) -> Vec<crate::tools::spec::ToolCapability> {
        Vec::new()
    }
    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<crate::tools::spec::ToolResult, ToolError> {
        Ok(crate::tools::spec::ToolResult::success("from the script"))
    }
}

#[tokio::test]
async fn an_extension_named_like_a_script_tool_is_skipped_at_turn_build() {
    let Some(node) = node_for_tests("script_name_clash") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["clash-script"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    assert_eq!(manager.live_tool_names(), vec!["fixture_script_tool"]);
    let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
        .build(ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()));
    registry.register(Arc::new(FakeScriptTool));
    let installed = engine.install_tools(&mut registry);
    assert!(installed.is_empty());
    assert_eq!(
        registry
            .get("fixture_script_tool")
            .unwrap()
            .registration_origin(),
        "plugin script fixture_script_tool",
        "the script tool is unaffected"
    );
    assert!(
        manager
            .diagnostics()
            .iter()
            .any(|d| d.contains("fixture_script_tool") && d.contains("skipped")),
        "{:?}",
        manager.diagnostics()
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn with_no_native_plugin_the_host_is_never_spawned() {
    let _policy = TestPolicyGuard::extension_host(true);
    let temp = tempfile::tempdir().unwrap();
    let registry = Arc::new(PluginRegistry::empty(temp.path()));
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        node_override: None,
        root: Some(temp.path().join("home")),
        ..Default::default()
    }));
    let engine = manager.attach(registry);
    engine.sync().await.unwrap();
    assert_eq!(manager.spawn_attempts(), 0);
    assert_eq!(manager.status(), HostStatus::Idle);
    assert!(!temp.path().join("home").exists(), "nothing materialized");
}

async fn probe(tool: &Arc<dyn ToolSpec>, path: &Path, context: &ToolContext) -> Value {
    let result = tool
        .execute(json!({"path": path.to_string_lossy()}), context)
        .await
        .unwrap();
    serde_json::from_str(&result.content).unwrap()
}

#[tokio::test]
async fn sandboxed_host_cannot_read_codewhale_secrets_or_write_outside_its_data_dir() {
    let Some(node) = node_for_tests("sandboxed_host") else {
        return;
    };
    sandboxed_host_boundary(NODE, node).await;
}

/// Same actual Rust manager, review/activation, tool and filesystem scenario,
/// using the exact compiled image selected by the existing Bun resolver.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[tokio::test]
async fn compiled_native_host_cannot_read_secrets_or_write_outside_its_data_dir() {
    let Some(binary) = compiled_image_for_tests() else {
        return;
    };
    sandboxed_host_boundary(
        codewhale_config::extension_host::ExtensionHostRuntime::Bun,
        binary,
    )
    .await;
    // Emitted only after the complete actual Rust admission/tool scenario.
    // CI extracts this same full-run success output; no fake-Core promotion.
    eprintln!(
        "compiled-native-containment=passed platform={} arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
}

/// Resolve the exact requested image through the same production runtime
/// admission for every compiled Native scenario. Required inputs cannot skip.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn compiled_image_for_tests() -> Option<PathBuf> {
    let Some(binary) = std::env::var_os("CODEWHALE_COMPILED_HOST_TEST_BINARY") else {
        assert!(
            std::env::var_os("CODEWHALE_EXT_HOST_TESTS").is_none(),
            "required compiled Native image input is missing"
        );
        eprintln!("compiled Native receipt unavailable: name a matching canonical compiled image");
        return None;
    };
    let binary = PathBuf::from(binary);
    let resolution = codewhale_dependencies::resolve_extension_host_runtime(
        codewhale_config::extension_host::ExtensionHostRuntime::Bun,
        None,
        Some(&binary),
        crate::extension_host::bundle_sha256(),
    );
    let runtime = resolution
        .selected
        .as_ref()
        .unwrap_or_else(|| panic!("compiled Native runtime: {}", resolution.failure()));
    assert!(
        runtime.compiled,
        "receipt requires an actual canonical compiled image, not system Bun"
    );
    Some(binary)
}

async fn sandboxed_host_boundary(
    choice: codewhale_config::extension_host::ExtensionHostRuntime,
    runtime_path: PathBuf,
) {
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["secret-probe"]).await;
    // Synthetic credential stores created before launch remain outside read grants.
    let secrets = fixture.root.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let token = secrets.join("token");
    std::fs::write(&token, "s3cret-value").unwrap();
    // Any other entry of the Codewhale home is denied too (config backups,
    // OAuth tokens, state), not only the named stores.
    let backup = fixture.root.join("config.toml.bak-20260925");
    std::fs::write(&backup, "api_key = \"s3cret-backup\"").unwrap();
    let tokens = fixture.root.join("tokens");
    std::fs::create_dir_all(&tokens).unwrap();
    std::fs::write(tokens.join("codex.json"), "s3cret-oauth").unwrap();
    // Native hosts refuse ungranted workspace reads on every platform.
    let readable = fixture.workspace().join("readable.txt");
    std::fs::write(&readable, "plain").unwrap();
    let dotenv = fixture.workspace().join(".env");
    std::fs::write(&dotenv, "TOKEN=synthetic-private-value").unwrap();
    #[cfg(unix)]
    let external_metadata = {
        let worktree = fixture.workspace().join("external-worktree");
        let common = fixture.workspace().join("external-repository/.git");
        let metadata = common.join("worktrees/external");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&metadata).unwrap();
        let pointer = worktree.join(".git");
        std::fs::write(&pointer, format!("gitdir: {}\n", metadata.display())).unwrap();
        std::fs::write(metadata.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            metadata.join("gitdir"),
            pointer.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let data = fixture.root.join("extension-host/data");
        std::fs::create_dir_all(&data).unwrap();
        std::os::unix::fs::symlink(&pointer, data.join(".git")).unwrap();
        metadata
    };

    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: choice,
        node_override: (choice == NODE).then_some(runtime_path.clone()),
        bun_override: (choice == codewhale_config::extension_host::ExtensionHostRuntime::Bun)
            .then_some(runtime_path.clone()),
        root: Some(fixture.root.clone()),
        ..Default::default()
    }));
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let HostStatus::Ready { sandbox, .. } = manager.status() else {
        panic!("host not ready: {:?}", manager.status());
    };
    let sandbox = match sandbox {
        super::supervisor::HostSandbox::Wrapped(name) => name,
        super::supervisor::HostSandbox::Unsandboxed(reason) => {
            panic!("Native containment requires a verified sandbox: {reason}");
        }
    };
    assert!(super::render_status(&manager).contains(&format!("{sandbox} sandbox")));

    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let read = host_tool(&engine, fixture.workspace(), "probe_read");
    let write = host_tool(&engine, fixture.workspace(), "probe_write");

    let plain = probe(&read, &readable, &context).await;
    assert_eq!(
        plain["ok"], false,
        "Native hosts refuse ungranted workspace reads"
    );
    let late_project = fixture.workspace().join("late.env");
    std::fs::write(&late_project, "TOKEN=synthetic-private-value").unwrap();
    for denied in [
        token,
        backup,
        tokens.join("codex.json"),
        dotenv.clone(),
        late_project,
    ] {
        let secret = probe(&read, &denied, &context).await;
        assert_eq!(secret["ok"], false, "{} was readable", denied.display());
        assert!(
            !secret.to_string().contains("s3cret"),
            "{} leaked its contents",
            denied.display()
        );
    }
    // A store created after the host started remains outside read grants.
    let state = fixture.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("late.json"), "s3cret-late").unwrap();
    let late = probe(&read, &state.join("late.json"), &context).await;
    assert_eq!(
        late["ok"], false,
        "a store created after start was readable"
    );
    // Migrated history can first appear after launch and remains unreadable.
    let history = fixture.root.join("composer_history.jsonl");
    std::fs::write(&history, "\"private synthetic prompt\"\n").unwrap();
    let late_history = probe(&read, &history, &context).await;
    assert_eq!(late_history["ok"], false, "new history was readable");
    // The Codex credential file Codewhale itself reads, when this machine has
    // one. Only `ok` is reported, never the content.
    let codex_auth = codewhale_runtime::oauth::auth_file_path();
    if codex_auth.is_file() {
        let codex = probe(&read, &codex_auth, &context).await;
        assert_eq!(codex["ok"], false, "code: {}", codex["code"]);
    }

    let data = fixture.root.join("extension-host/data/probe.txt");
    assert_eq!(probe(&write, &data, &context).await["ok"], true);
    assert_eq!(
        probe(&read, &data, &context).await,
        json!({"ok": true, "text": "probe"})
    );
    let scratch = fixture.root.join("extension-host/data/tmp/probe.txt");
    assert_eq!(probe(&write, &scratch, &context).await["ok"], true);
    assert_eq!(probe(&read, &scratch, &context).await["ok"], true);
    #[cfg(unix)]
    {
        assert_eq!(
            probe(&read, &external_metadata.join("commondir"), &context).await["ok"],
            false
        );
        let escaped = external_metadata.join("native-write-escape");
        assert_eq!(probe(&write, &escaped, &context).await["ok"], false);
        assert!(!escaped.exists());
        let linked = fixture.root.join("extension-host/data/project-secret");
        std::os::unix::fs::symlink(&dotenv, &linked).unwrap();
        assert_eq!(probe(&read, &linked, &context).await["ok"], false);
        if choice == NODE {
            for (path, expected) in [(&data, true), (&dotenv, false), (&linked, false)] {
                let result = read
                    .execute(
                        json!({"path": path.to_string_lossy(), "child": true}),
                        &context,
                    )
                    .await
                    .unwrap();
                let result: Value = serde_json::from_str(&result.content).unwrap();
                assert_eq!(
                    result["ok"],
                    expected,
                    "Native child read {}: {result}",
                    path.display()
                );
            }
        }
    }
    let outside = fixture.workspace().join("native-write-escape");
    assert_eq!(probe(&write, &outside, &context).await["ok"], false);
    assert!(!outside.exists());
    // Outside the data directory: the crate's source directory, unless the
    // checkout itself sits in a temp directory.
    let crate_dir = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let in_temp = [std::env::temp_dir(), PathBuf::from("/tmp")]
        .iter()
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .any(|dir| crate_dir.starts_with(dir));
    if !in_temp {
        let escape = crate_dir.join(format!(".ext-host-probe-{}", uuid::Uuid::new_v4().simple()));
        let escaped = probe(&write, &escape, &context).await;
        let leaked = escape.exists();
        let _ = std::fs::remove_file(&escape);
        assert_eq!(escaped["ok"], false, "{escaped}");
        assert!(!leaked);
    }
    // Bun's TCP error adapter collapses Darwin's EPERM into ECONNREFUSED.
    // Its UDP adapter retains the OS errno, so use a real bound datagram
    // receiver there; every other runtime/platform keeps the TCP control.
    let (denied, accepted_count, protocol) = if cfg!(target_os = "macos")
        && choice == codewhale_config::extension_host::ExtensionHostRuntime::Bun
    {
        let listener = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let positive = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let marker = b"controller-network-probe";
        assert_eq!(
            positive.send_to(marker, address).await.unwrap(),
            marker.len()
        );
        let mut packet = [0_u8; 64];
        let (bytes, sender) =
            tokio::time::timeout(Duration::from_secs(2), listener.recv_from(&mut packet))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&packet[..bytes], marker);
        assert_eq!(sender, positive.local_addr().unwrap());
        let mut accepted_count = 1;
        let send = host_tool(&engine, fixture.workspace(), "probe_send");
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            send.execute(json!({"port": address.port()}), &context),
        )
        .await
        .unwrap()
        .unwrap();
        let denied: Value = serde_json::from_str(&result.content).unwrap();
        if tokio::time::timeout(Duration::from_millis(100), listener.recv_from(&mut packet))
            .await
            .is_ok()
        {
            accepted_count += 1;
        }
        (denied, accepted_count, "udp")
    } else {
        // The unsandboxed controller reaches this actual listener. The Native
        // host must fail the same connection at its own OS sandbox boundary.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let positive = tokio::net::TcpStream::connect(address).await.unwrap();
        let (accepted, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut accepted_count = 1;
        drop(positive);
        drop(accepted);
        let connect = host_tool(&engine, fixture.workspace(), "probe_connect");
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            connect.execute(json!({"port": address.port()}), &context),
        )
        .await
        .unwrap()
        .unwrap();
        let denied: Value = serde_json::from_str(&result.content).unwrap();
        if tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_ok()
        {
            accepted_count += 1;
        }
        (denied, accepted_count, "tcp")
    };
    assert_eq!(
        denied["ok"], false,
        "Native host reached {protocol} controller: {denied}"
    );
    #[cfg(target_os = "linux")]
    {
        let controller = std::fs::read_link("/proc/self/ns/net").unwrap();
        let controller = controller.to_str().unwrap();
        let host = denied["network_namespace"].as_str().unwrap();
        let namespace_id = |name: &str| {
            name.strip_prefix("net:[")
                .and_then(|name| name.strip_suffix(']'))
                .and_then(|id| id.parse::<u64>().ok())
                .filter(|id| *id != 0)
                .expect("an actual bounded Linux kernel network namespace")
        };
        assert_ne!(
            namespace_id(host),
            namespace_id(controller),
            "Native host must remain in its isolated kernel network namespace"
        );
        assert_eq!(
            denied["code"], "ECONNREFUSED",
            "the isolated loopback cannot reach the controller: {denied}"
        );
    }
    #[cfg(not(target_os = "linux"))]
    assert!(
        matches!(denied["code"].as_str(), Some("EPERM" | "EACCES")),
        "connection must be refused by the OS sandbox: {denied}"
    );
    assert_eq!(
        accepted_count, 1,
        "only the controller reached the listener"
    );
    if choice == codewhale_config::extension_host::ExtensionHostRuntime::Bun {
        eprintln!(
            "compiled-native-network=passed controller_accepts=1 host_errno={} platform={} arch={} protocol={protocol}",
            denied["code"].as_str().unwrap(),
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
    }
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// Receipt-bound approval keys (design §4.3)
// ---------------------------------------------------------------------------

fn keys_for(
    manager: &ExtensionHostManager,
    registration: super::registry::ToolRegistration,
    input: &Value,
) -> (String, String) {
    let name = registration.name.clone();
    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(Path::new("/w")));
    registry.register(Arc::new(super::tool::HostToolSpec::new(
        registration,
        Arc::clone(&manager.shared),
    )));
    let (exact, grouping) =
        crate::tools::approval_cache::approval_keys_for_call(Some(&registry), &name, input);
    (exact.0, grouping.0)
}

/// A session grant for an extension tool covers one reviewed plugin build:
/// an update of the plugin, or another plugin that later registers the same
/// tool name, gets a different key and is asked again.
#[test]
fn extension_approval_keys_are_bound_to_the_plugin_receipt() {
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    let input = json!({"path": "x"});
    let mut owners = OwnerRegistry::new();
    let live = |owners: &mut OwnerRegistry, owner: &OwnerRef| {
        register(owners, owner, "shared_tool").unwrap();
        owners.mark_active(owner);
        owners.live_tools().pop().unwrap()
    };

    let first = owners
        .begin_owner(
            HostTier::Plugin,
            "a",
            "a",
            Some(fake_authority("a")),
            "hash-a1",
        )
        .unwrap();
    let first = keys_for(&manager, live(&mut owners, &first), &input);
    assert!(
        first.0.starts_with("ext:a@hash-a1:") && first.0.contains(":shared_tool:"),
        "{first:?}"
    );
    assert_eq!(first.0, first.1, "a grant covers the exact call only");
    let generic = crate::tools::approval_cache::build_approval_grouping_key("shared_tool", &input);
    assert_ne!(first.1, generic.0, "never the name-derived family key");

    // Same plugin, same input, updated bytes: a different grant.
    let updated = owners
        .begin_owner(
            HostTier::Plugin,
            "a",
            "a",
            Some(fake_authority("a")),
            "hash-a2",
        )
        .unwrap();
    let updated = keys_for(&manager, live(&mut owners, &updated), &input);
    assert_ne!(first.1, updated.1);

    // Another plugin takes the name once the first is gone.
    owners.revoke_owner("a");
    let other = owners
        .begin_owner(
            HostTier::Plugin,
            "b",
            "b",
            Some(fake_authority("b")),
            "hash-a1",
        )
        .unwrap();
    let other = keys_for(&manager, live(&mut owners, &other), &input);
    assert_ne!(first.1, other.1);
    assert_ne!(updated.1, other.1);

    // Tools without a scope keep their existing keys.
    let shell = json!({"command": "cargo build --release"});
    let (exact, grouping) =
        crate::tools::approval_cache::approval_keys_for_call(None, "exec_shell", &shell);
    assert_eq!(
        exact,
        crate::tools::approval_cache::build_approval_key("exec_shell", &shell)
    );
    assert_eq!(
        grouping,
        crate::tools::approval_cache::build_approval_grouping_key("exec_shell", &shell)
    );
}

// ---------------------------------------------------------------------------
// The native-entry rule at validate / review time
// ---------------------------------------------------------------------------

fn native_bundle(user: &Path, name: &str, native_path: &str, files: &[&str]) {
    let root = user.join(name);
    for file in files {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "export const name = 'x'\nexport function apply() {}\n",
        )
        .unwrap();
    }
    std::fs::write(
        root.join("plugin.json"),
        serde_json::to_vec_pretty(&json!({
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": name,
            "version": "0.1.0",
            "description": "native entry rule fixture",
            "license": "MIT",
            "extensions": {"net.codewhale": {"native": {"path": native_path}}}
        }))
        .unwrap(),
    )
    .unwrap();
}

/// `/plugin validate` and the review screen read plugin diagnostics, so an
/// entry that activation would refuse must fail there too, with the flag on;
/// with it off `native` is inventory-only and any path stays valid.
#[test]
fn native_entry_rule_fails_validation_when_the_host_is_enabled() {
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user");
    native_bundle(&user, "dir-entry", "lib", &["lib/index.mjs"]);
    native_bundle(&user, "ts-entry", "index.ts", &["index.ts"]);
    native_bundle(&user, "good-entry", "index.mjs", &["index.mjs"]);
    native_bundle(&user, "typed-entry", "index.mts", &["index.mts"]);
    let config = DiscoveryConfig {
        workspace: temp.path().join("project"),
        user_plugins_dir: user,
        workspace_plugins_dir: temp.path().join("project/.codewhale/plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: temp.path().join("state/plugin-state.json"),
    };
    let native_errors = |registry: &PluginRegistry, name: &str| -> Vec<String> {
        registry
            .get(name)
            .unwrap_or_else(|| panic!("{name} not discovered: {:?}", registry.diagnostics()))
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "native-entry-invalid")
            .map(|diagnostic| {
                assert_eq!(
                    diagnostic.level,
                    crate::plugins::types::PluginDiagnosticLevel::Error
                );
                diagnostic.message.clone()
            })
            .collect()
    };

    {
        let _policy = TestPolicyGuard::extension_host(true);
        let registry = discover_with_config(&config);
        for name in ["dir-entry", "ts-entry"] {
            let errors = native_errors(&registry, name);
            assert_eq!(errors.len(), 1, "{name}: {errors:?}");
            assert!(
                errors[0].contains(".mjs, .js or .mts"),
                "{name}: {errors:?}"
            );
        }
        assert!(native_errors(&registry, "good-entry").is_empty());
        assert!(native_errors(&registry, "typed-entry").is_empty());
        assert!(!registry.validation_is_clean());
    }
    let _policy = TestPolicyGuard::extension_host(false);
    let registry = discover_with_config(&config);
    for name in ["dir-entry", "ts-entry", "good-entry", "typed-entry"] {
        assert!(native_errors(&registry, name).is_empty(), "{name}");
    }
}

#[test]
fn owner_reports_keep_bounded_attributed_logs_and_ignore_stale_hosts() {
    use super::supervisor::HostEvents;

    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    manager
        .shared
        .plugin
        .host_generation
        .store(2, std::sync::atomic::Ordering::SeqCst);
    {
        let mut registry = manager.shared.registry.lock().unwrap();
        for id in ["alpha", "beta"] {
            let owner = registry
                .begin_owner(HostTier::Plugin, id, id, Some(fake_authority(id)), "hash")
                .unwrap();
            registry.mark_active(&owner);
            register(&mut registry, &owner, &format!("{id}_probe")).unwrap();
        }
    }
    let current = super::Events {
        shared: Arc::downgrade(&manager.shared),
        tier: HostTier::Plugin,
        generation: 2,
    };
    let stale = super::Events {
        shared: Arc::downgrade(&manager.shared),
        tier: HostTier::Plugin,
        generation: 1,
    };
    for index in 0..25 {
        current.log(&protocol::LogParams {
            level: "warn".into(),
            msg: format!("alpha message {index}"),
            plugin_id: Some("alpha".into()),
        });
    }
    let mut log = protocol::LogParams {
        level: "error".into(),
        msg: "beta only".into(),
        plugin_id: Some("beta".into()),
    };
    current.log(&log);
    log.msg = "stale message".into();
    stale.log(&log);
    log.plugin_id = Some("unknown".into());
    current.log(&log);
    log.plugin_id = Some("alpha".into());
    log.level = "debug".into();
    current.log(&log);
    let alpha = manager.owner_report("alpha").unwrap();
    assert!(matches!(alpha.state, Some(OwnerState::Active)));
    assert_eq!(alpha.tools, ["alpha_probe"]);
    assert_eq!(alpha.diagnostics.len(), 20);
    assert_eq!(alpha.diagnostics[0], "warn: alpha message 5");
    assert_eq!(
        manager.owner_report("beta").unwrap().diagnostics,
        ["error: beta only"]
    );
    assert!(manager.owner_report("unknown").is_none());
    manager
        .shared
        .plugin_diagnostic(&"x".repeat(10_000), "oversized id".into());
    assert!(
        manager
            .shared
            .diagnostics
            .lock()
            .unwrap()
            .back()
            .unwrap()
            .plugin_id
            .is_none()
    );
    for _ in 0..80 {
        manager.shared.plugin_diagnostic("alpha", "🦀".repeat(3000));
    }
    assert_eq!(manager.diagnostics().len(), 64);
    assert!(
        manager
            .diagnostics()
            .iter()
            .all(|line| line.len() <= super::MAX_DIAGNOSTIC_BYTES + '…'.len_utf8())
    );
    assert!(
        manager
            .owner_report("alpha")
            .unwrap()
            .diagnostics
            .iter()
            .all(|line| line.ends_with('…'))
    );
}

#[tokio::test]
async fn typed_author_example_is_reviewed_before_its_tool_can_execute() {
    use crate::plugins::install::{DEFAULT_MAX_SIZE_BYTES, PluginInstallSource, install};

    let Some(node) = node_for_tests("typed author example") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&[]).await;
    let example =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/plugins/hello-extension");
    install(
        PluginInstallSource::LocalPath(example),
        &fixture.config.user_plugins_dir,
        DEFAULT_MAX_SIZE_BYTES,
        &codewhale_net::network_policy::NetworkPolicy::default(),
        false,
        &|_| None,
    )
    .await
    .unwrap();
    let mut plugins = discover_with_config(&fixture.config);
    assert!(!plugins.is_active("hello-extension"));
    plugins.trust("hello-extension").unwrap();
    assert!(
        !plugins.is_active("hello-extension"),
        "trust alone does not enable code"
    );
    plugins.enable("hello-extension").unwrap();
    // This tests plugin review and typed loading, not hang detection. The
    // 600 ms watchdog used by supervision fault tests can kill a healthy
    // typed-plugin load on a busy runner before registration completes.
    let manager = fixture.manager(node);
    let engine = manager.attach(Arc::new(plugins));
    engine.sync().await.unwrap();
    let tool = host_tool(&engine, fixture.workspace(), "hello_greet");
    assert_eq!(tool.approval_requirement(), ApprovalRequirement::Required);
    let result = tool
        .execute(
            json!({"name": "Codewhale"}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
        )
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(payload["greeting"], "Hello, Codewhale!");
    assert!(payload["callId"].as_str().is_some_and(|id| !id.is_empty()));

    // The example registers a plain object whose schema says
    // `additionalProperties: false` and `name: string`. The core enforces it;
    // the example's own `execute` would have accepted either input. Neither
    // call reaches the host.
    let sent = manager.host_requests_started();
    for input in [
        json!({"name": "Codewhale", "surprise": true}),
        json!({"name": 7}),
    ] {
        let error = tool
            .execute(
                input.clone(),
                &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, ToolError::InvalidInput { ref message } if message.contains("hello_greet")),
            "{input}: {error:?}"
        );
    }
    assert_eq!(
        manager.host_requests_started(),
        sent,
        "a call the schema refuses sends the host nothing"
    );
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// Engines sharing one process-wide host
// ---------------------------------------------------------------------------

#[test]
fn attachment_changes_discard_the_complete_scan_before_owner_side_effects() {
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
    let old = Arc::new(PluginRegistry::empty(Path::new("/old")));
    let current = Arc::new(PluginRegistry::empty(Path::new("/current")));
    let attachment = manager.attach(Arc::clone(&old));
    let scan = |plugins: &Arc<PluginRegistry>| super::DesiredScan {
        attachments: vec![(
            attachment.id,
            Arc::clone(plugins),
            [("plugin".into(), "hash-plugin".into())].into(),
        )],
        owners: [(
            "plugin".into(),
            super::DesiredOwner {
                plugin_name: "plugin".into(),
                authority: fake_authority("plugin"),
                entries: Vec::new(),
            },
        )]
        .into(),
        errors: Vec::new(),
    };

    // An old scan finishes after the engine has already changed workspace.
    attachment.set_plugins(Arc::clone(&current));
    let current_view = attachment.plugin_view();
    let mut attachments = manager.shared.attachments.lock().unwrap();
    assert!(scan(&old).publish(&mut attachments).is_none());
    assert!(attachments[&attachment.id].desired.is_empty());
    // A current scan publishes both the engine view and owner union.
    let (owners, _) = scan(&current_view).publish(&mut attachments).unwrap();
    assert!(owners.contains_key("plugin"));
    assert_eq!(attachments[&attachment.id].desired["plugin"], "hash-plugin");
    drop(attachments);

    // A newly attached engine also invalidates the complete scan, even when
    // it uses the same snapshot: otherwise its owners could be revoked.
    let other = manager.attach(Arc::clone(&current));
    assert!(
        scan(&current_view)
            .publish(&mut manager.shared.attachments.lock().unwrap())
            .is_none()
    );
    drop(other);
    let stale = scan(&current_view);
    drop(attachment);
    assert!(
        stale
            .publish(&mut manager.shared.attachments.lock().unwrap())
            .is_none()
    );
}

pub(crate) fn installed(engine: &HostAttachment, workspace: &Path) -> Vec<String> {
    let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
        .build(ToolContext::new(workspace).with_plugin_registry(engine.plugin_view()));
    engine.install_tools(&mut registry)
}

pub(crate) fn plugin_id(fixture: &FixturePlugins, name: &str) -> String {
    fixture
        .registry()
        .get(name)
        .unwrap()
        .id
        .as_str()
        .to_string()
}

/// Two engines for different workspaces in one process: syncing either
/// keeps the other's plugin active and its in-flight call running, neither
/// receives the other's tools, and detaching one revokes only its plugin.
#[tokio::test]
async fn engines_in_one_process_never_revoke_each_others_plugins() {
    let Some(node) = node_for_tests("engines_in_one_process") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let slow = FixturePlugins::new(&["slow-tool"]).await;
    let deps = FixturePlugins::new(&["dsh-workspace-deps"]).await;
    let (slow_id, deps_id) = (
        plugin_id(&slow, "slow-tool"),
        plugin_id(&deps, "dsh-workspace-deps"),
    );
    let manager = slow.manager(node);
    let first = manager.attach(slow.registry());
    let second = manager.attach(deps.registry());
    first.sync().await.unwrap();
    let tool = host_tool(&first, slow.workspace(), "slow_wait");
    let context = ToolContext::new(slow.workspace()).with_plugin_registry(first.plugin_view());
    let call = tokio::spawn(async move { tool.execute(json!({"ms": 600}), &context).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Both snapshot stores are granted at launch; the second engine syncs mid-call.
    second.sync().await.unwrap();
    assert_eq!(manager.owner_state(&slow_id), Some(OwnerState::Active));
    assert_eq!(manager.owner_state(&deps_id), Some(OwnerState::Active));
    let result = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("call resolves")
        .unwrap()
        .expect("the first engine's in-flight call completes");
    assert!(result.success, "{}", result.content);
    assert!(result.content.contains("600"), "{}", result.content);

    assert_eq!(installed(&first, slow.workspace()), vec!["slow_wait"]);
    assert_eq!(
        installed(&second, deps.workspace()),
        vec!["load_workspace_dependencies"]
    );
    first.sync().await.unwrap();
    assert_eq!(manager.owner_state(&deps_id), Some(OwnerState::Active));
    assert!(
        !manager.diagnostics().iter().any(|d| d.contains("revoked")),
        "{:?}",
        manager.diagnostics()
    );

    #[cfg(not(windows))]
    {
        let late = FixturePlugins::new(&["clash-script"]).await;
        let late_id = plugin_id(&late, "clash-script");
        let late_engine = manager.attach(late.registry());
        late_engine.sync().await.unwrap();
        assert!(
            matches!(manager.owner_state(&late_id), Some(OwnerState::Failed(ref reason))
                if reason.contains("Native snapshot store was not granted to this host")
                    && reason.contains("restart Codewhale")),
            "{:?}",
            manager.owner_state(&late_id)
        );
        assert_eq!(manager.owner_state(&slow_id), Some(OwnerState::Active));
        assert_eq!(manager.owner_state(&deps_id), Some(OwnerState::Active));
        assert_eq!(manager.spawn_attempts(), 1);
    }

    // Detaching does not revoke by itself; the next reconcile revokes only
    // what no remaining engine desires.
    drop(second);
    assert_eq!(manager.owner_state(&deps_id), Some(OwnerState::Active));
    first.sync().await.unwrap();
    assert_eq!(manager.owner_state(&deps_id), None);
    assert_eq!(manager.owner_state(&slow_id), Some(OwnerState::Active));
    assert_eq!(manager.spawn_attempts(), 1);
    manager.shutdown().await;
}

/// Each snapshot is re-verified against persisted plugin state, so a disable
/// made through one engine's registry revokes the plugin for an engine still
/// holding the older snapshot.
#[tokio::test]
async fn a_disable_through_either_registry_revokes_for_every_engine() {
    let Some(node) = node_for_tests("a_disable_through_either_registry") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool"]).await;
    let id = plugin_id(&fixture, "slow-tool");
    let manager = fixture.manager(node);
    let first = manager.attach(fixture.registry());
    let second = manager.attach(fixture.registry());
    first.sync().await.unwrap();
    assert_eq!(installed(&first, fixture.workspace()), vec!["slow_wait"]);
    assert_eq!(installed(&second, fixture.workspace()), vec!["slow_wait"]);

    second.set_plugins(fixture.disable("slow-tool"));
    second.sync().await.unwrap();
    assert_eq!(manager.owner_state(&id), None, "revoked and forgotten");
    assert!(installed(&first, fixture.workspace()).is_empty());
    assert!(installed(&second, fixture.workspace()).is_empty());
    manager.shutdown().await;
}

fn fast_supervision() -> super::SupervisionOptions {
    super::SupervisionOptions {
        heartbeat_interval: Duration::from_millis(50),
        ping_timeout: Duration::from_millis(150),
        hang_timeout: Duration::from_millis(600),
        restart_backoff: Duration::from_millis(25),
        ..Default::default()
    }
}

fn supervised_manager(fixture: &FixturePlugins, node: PathBuf) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: NODE,
        node_override: Some(node),
        bun_override: None,
        root: Some(fixture.root.clone()),
        supervision: fast_supervision(),
    }))
}

async fn wait_host(manager: &ExtensionHostManager, predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "host wait timed out: {:?}; {:?}",
            manager.status(),
            manager.diagnostics()
        )
    });
}

#[test]
fn crash_budget_is_bounded_and_expires_only_with_the_window() {
    let options = super::SupervisionOptions::default();
    let mut state = super::SupervisionState::default();
    let now = Instant::now();
    assert!(state.record_crash(now, &options));
    assert!(state.record_crash(now + Duration::from_secs(1), &options));
    assert!(!state.record_crash(now + Duration::from_secs(2), &options));
    assert_eq!(state.crashes.len(), 3);
    assert!(state.record_crash(
        now + options.crash_window + Duration::from_secs(3),
        &options
    ));
    assert_eq!(state.crashes.len(), 1);
}

#[test]
fn dirty_teardown_window_is_bounded_and_a_requested_restart_waits_for_idle() {
    let options = super::SupervisionOptions::default();
    let mut state = super::SupervisionState::default();
    let now = Instant::now();
    state.record_dirty_teardown(now, &options);
    assert!(!state.dirty_restart_pending);
    state.record_dirty_teardown(now + options.dirty_window, &options);
    assert!(!state.dirty_restart_pending, "the first event expired");
    state.record_dirty_teardown(
        now + options.dirty_window + Duration::from_secs(1),
        &options,
    );
    assert!(state.dirty_restart_pending);
    for second in 2..100 {
        state.record_dirty_teardown(
            now + options.dirty_window + Duration::from_secs(second),
            &options,
        );
    }
    assert_eq!(state.dirty_teardowns.len(), 2);
    state.record_dirty_teardown(now + options.dirty_window * 3, &options);
    assert!(
        state.dirty_restart_pending,
        "an idle request does not expire"
    );
    assert!(state.crashes.is_empty());
}

#[tokio::test]
async fn a_call_past_its_method_deadline_is_cancelled_and_the_host_stays_usable() {
    let Some(node) = node_for_tests("method deadline") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool", "clash-script"]).await;
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: NODE,
        node_override: Some(node),
        bun_override: None,
        root: Some(fixture.root.clone()),
        supervision: super::SupervisionOptions {
            tool_call_deadline: Duration::from_millis(300),
            ..Default::default()
        },
    }));
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let pid = manager.host_pid();
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    // `slow_wait` answers after 30 s unless it is cancelled.
    let slow = host_tool(&engine, fixture.workspace(), "slow_wait");
    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(5), slow.execute(json!({}), &context))
        .await
        .expect("the method deadline bounds the call");
    let elapsed = started.elapsed();
    assert!(
        matches!(outcome, Err(ToolError::Timeout { .. })),
        "{outcome:?}"
    );
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    // The same host takes the next call.
    let quick = host_tool(&engine, fixture.workspace(), "fixture_script_tool");
    let result = quick.execute(json!({}), &context).await.unwrap();
    assert!(result.success, "{}", result.content);
    assert_eq!(manager.host_pid(), pid);
    assert_eq!(manager.spawn_attempts(), 1);
    // `slow-tool`'s disposer awaits its in-flight call, so a clean teardown
    // proves the host received `$/cancel` and aborted the timed-out call
    // (uncancelled, it outlives the host's 2 s dispose deadline).
    engine.set_plugins(fixture.disable("slow-tool"));
    engine.sync().await.unwrap();
    let diagnostics = manager.diagnostics();
    assert!(
        !diagnostics.iter().any(|line| line.contains("teardown")),
        "{diagnostics:?}"
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn ordinary_exit_rejects_requests_from_a_drained_calls_waker() {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Wake, Waker};

    use super::supervisor::{HostCallError, HostProcess};

    struct AdmissionProbe {
        host: Arc<HostProcess>,
        result: Mutex<Option<Result<(), HostCallError>>>,
        woke: tokio::sync::Notify,
    }

    impl Wake for AdmissionProbe {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            let mut result = self.result.lock().unwrap();
            if result.is_none() {
                // oneshot send wakes synchronously: probe the exact gap after
                // the exit watcher drains pending and starts failing its calls.
                *result = Some(
                    self.host
                        .start_request(protocol::CoreRequest::Ping, None)
                        .map(|(id, _)| self.host.forget(id)),
                );
                self.woke.notify_one();
            }
        }
    }

    let Some(node) = node_for_tests("ordinary exit admission") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool"]).await;
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: NODE,
        node_override: Some(node),
        bun_override: None,
        root: Some(fixture.root.clone()),
        supervision: super::SupervisionOptions {
            heartbeat_interval: Duration::from_secs(60),
            ..Default::default()
        },
    }));
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let host = manager.shared.ready_host(HostTier::Plugin).unwrap();
    let registration = manager.shared.registry.lock().unwrap().live_tools()[0].clone();
    let (_, mut call) = host
        .start_request(
            protocol::CoreRequest::ToolCall(protocol::ToolCallParams {
                handle: registration.handle,
                call_id: "exit-admission".into(),
                input: json!({"ms": 30_000}),
                deadline_ms: 60_000,
                workspace: None,
                ticket: None,
                checkpoint_ticket: None,
                question_ticket: None,
                session_id: None,
                agent_id: None,
                origin_turn_id: None,
            }),
            Some(registration.owner.plugin_id),
        )
        .unwrap();
    // A following response proves the writer flushed the slow call and is
    // waiting for another frame, so a closed outbound queue cannot mask the bug.
    host.call(protocol::CoreRequest::Ping, None).await.unwrap();
    let probe = Arc::new(AdmissionProbe {
        host: Arc::clone(&host),
        result: Mutex::new(None),
        woke: tokio::sync::Notify::new(),
    });
    let waker = Waker::from(Arc::clone(&probe));
    assert!(
        Pin::new(&mut call)
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert!(
        !host.is_retiring(),
        "this exercises exit, not maintenance sealing"
    );
    host.terminate("ordinary exit admission regression".into());
    tokio::time::timeout(Duration::from_secs(5), probe.woke.notified())
        .await
        .expect("exit must fail the pending call");
    assert!(matches!(
        probe.result.lock().unwrap().take().unwrap(),
        Err(HostCallError::Exited(_))
    ));
    assert!(matches!(call.await.unwrap(), Err(HostCallError::Exited(_))));
    manager.shutdown().await;
}

#[tokio::test]
async fn idle_retirement_seals_admission_and_does_not_wait_for_heartbeat() {
    use futures_util::FutureExt;

    let Some(node) = node_for_tests("idle admission") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool"]).await;
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let host = manager.shared.ready_host(HostTier::Plugin).unwrap();
    let registration = manager.shared.registry.lock().unwrap().live_tools()[0].clone();
    let (_, call) = host
        .start_request(
            protocol::CoreRequest::ToolCall(protocol::ToolCallParams {
                handle: registration.handle,
                call_id: "idle-admission".into(),
                input: json!({"ms": 100}),
                deadline_ms: 5000,
                workspace: None,
                ticket: None,
                checkpoint_ticket: None,
                question_ticket: None,
                session_id: None,
                agent_id: None,
                origin_turn_id: None,
            }),
            Some(registration.owner.plugin_id),
        )
        .unwrap();
    assert!(!host.terminate_if_idle(super::DIRTY_RESTART_REASON));
    assert!(call.await.unwrap().is_ok());
    // No await between admission and retirement: this heartbeat is still in
    // the pending map, but it does not make the process busy.
    let (_, _heartbeat) = host
        .start_request(protocol::CoreRequest::Ping, None)
        .unwrap();
    assert!(host.terminate_if_idle(super::DIRTY_RESTART_REASON));
    assert!(matches!(
        manager.shared.ready_host(HostTier::Plugin),
        Err(HostStatus::Restarting { .. })
    ));
    assert!(matches!(manager.status(), HostStatus::Restarting { .. }));
    // Poll without yielding to the exit watcher. A reconcile in this exact
    // gap must not start activation on the sealed process and falsely fail
    // a valid receipt before replay.
    assert!(
        manager
            .ensure_host(HostTier::Plugin, true)
            .now_or_never()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        manager.owner_state(&plugin_id(&fixture, "slow-tool")),
        Some(OwnerState::Active)
    );
    assert!(matches!(
        host.start_request(protocol::CoreRequest::Ping, None),
        Err(super::supervisor::HostCallError::Exited(_))
    ));
    assert!(!host.terminate_if_idle(super::DIRTY_RESTART_REASON));
    manager.shutdown().await;
}

#[tokio::test]
async fn two_dirty_teardowns_wait_for_a_live_call_then_replay_without_spending_crash_budget() {
    let Some(node) = node_for_tests("dirty teardown") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["dirty-dispose", "slow-tool"]).await;
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    // An existing unexpected crash must survive planned maintenance.
    manager
        .shared
        .plugin
        .supervision
        .lock()
        .unwrap()
        .record_crash(Instant::now(), &manager.shared.options.supervision);
    engine.set_plugins(fixture.disable("dirty-dispose"));
    engine.sync().await.unwrap();
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .dirty_teardowns
            .len(),
        1
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        manager.spawn_attempts(),
        1,
        "one dirty event is insufficient"
    );

    let mut enabled = discover_with_config(&fixture.config);
    enabled.enable("dirty-dispose").unwrap();
    engine.set_plugins(Arc::new(enabled));
    engine.sync().await.unwrap();
    let old = host_tool(&engine, fixture.workspace(), "slow_wait");
    let host = manager.shared.ready_host(HostTier::Plugin).unwrap();
    let registration = manager.shared.registry.lock().unwrap().live_tools()[0].clone();
    let (_, call) = host
        .start_request(
            protocol::CoreRequest::ToolCall(protocol::ToolCallParams {
                handle: registration.handle,
                call_id: "survives-dirty-teardown".into(),
                input: json!({"ms": 4000}),
                deadline_ms: 10000,
                workspace: None,
                ticket: None,
                checkpoint_ticket: None,
                question_ticket: None,
                session_id: None,
                agent_id: None,
                origin_turn_id: None,
            }),
            Some(registration.owner.plugin_id.clone()),
        )
        .unwrap();
    engine.set_plugins(fixture.disable("dirty-dispose"));
    engine.sync().await.unwrap();
    assert!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .dirty_restart_pending
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(manager.spawn_attempts(), 1, "a live call defers retirement");
    let completed = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(completed["structured"]["waited"], 4000);
    wait_host(&manager, || {
        manager.spawn_attempts() == 2 && manager.live_tool_names().contains(&"slow_wait".into())
    })
    .await;
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .len(),
        1
    );
    assert!(
        !manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .dirty_restart_pending
    );
    let replayed = manager.shared.registry.lock().unwrap().live_tools()[0].clone();
    assert_ne!(registration.owner, replayed.owner);
    assert_ne!(registration.handle, replayed.handle);
    assert!(matches!(
        old.execute(
            json!({"ms": 1}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view())
        )
        .await,
        Err(ToolError::NotAvailable { .. })
    ));
    // A delayed outcome from the retired process cannot dirty its replacement.
    manager.shared.record_dirty_teardown(&host);
    assert!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .dirty_teardowns
            .is_empty()
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn failed_activation_cleanup_also_records_a_dirty_teardown() {
    let Some(node) = node_for_tests("failed activation teardown") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["clash-script"]).await;
    native_bundle(
        &fixture.config.user_plugins_dir,
        "failed-disposal",
        "index.mjs",
        &["index.mjs"],
    );
    std::fs::write(
        fixture.config.user_plugins_dir.join("failed-disposal/index.mjs"),
        "export const name = 'failed-disposal';\nexport function apply(ctx) {\n  ctx.effect(() => () => new Promise(() => {}), 'unfinished activation cleanup');\n  throw new Error('fixture activation failure');\n}\n",
    ).unwrap();
    let mut plugins = discover_with_config(&fixture.config);
    plugins.trust("failed-disposal").unwrap();
    plugins.enable("failed-disposal").unwrap();
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(Arc::new(plugins));
    tokio::time::timeout(Duration::from_secs(15), engine.sync())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        manager.owner_state(&plugin_id(&fixture, "failed-disposal")),
        Some(OwnerState::Failed(_))
    ));
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .dirty_teardowns
            .len(),
        1
    );
    assert!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .is_empty()
    );
    assert_eq!(manager.spawn_attempts(), 1);
    assert!(
        manager
            .live_tool_names()
            .contains(&"fixture_script_tool".into())
    );
    manager.shutdown().await;
}

#[test]
fn host_exit_preserves_failed_receipts_and_blames_only_the_activating_owner() {
    let mut registry = OwnerRegistry::new();
    let active = registry
        .begin_owner(
            HostTier::Plugin,
            "healthy",
            "healthy",
            Some(fake_authority("healthy")),
            "hash-healthy",
        )
        .unwrap();
    registry.mark_active(&active);
    register(&mut registry, &active, "healthy_probe").unwrap();
    let failed = registry
        .begin_owner(
            HostTier::Plugin,
            "failed",
            "failed",
            Some(fake_authority("failed")),
            "hash-failed",
        )
        .unwrap();
    registry.mark_failed(&failed, OwnerState::Faulted("existing fault".into()));
    registry
        .begin_owner(
            HostTier::Plugin,
            "activating",
            "activating",
            Some(fake_authority("activating")),
            "hash-activating",
        )
        .unwrap();
    registry.host_exited(HostTier::Plugin, "fixture crash");
    assert!(registry.owner("healthy").is_none());
    assert!(registry.live_tools().is_empty());
    assert!(matches!(
        registry.owner("failed").unwrap().state,
        OwnerState::Faulted(_)
    ));
    assert!(matches!(
        registry.owner("activating").unwrap().state,
        OwnerState::Failed(_)
    ));
    let replay = registry
        .begin_owner(
            HostTier::Plugin,
            "healthy",
            "healthy",
            Some(fake_authority("healthy")),
            "hash-healthy",
        )
        .unwrap();
    assert_ne!(replay.generation, active.generation);
    assert_ne!(replay.owner_token, active.owner_token);
}

#[test]
fn opening_an_engine_never_resets_a_crash_budget() {
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
    {
        *manager.shared.plugin.host.lock().unwrap() = super::HostSlot::Failed {
            reason: "budget".into(),
            stderr_tail: String::new(),
        };
        let mut state = manager.shared.plugin.supervision.lock().unwrap();
        for _ in 0..3 {
            state.record_crash(Instant::now(), &manager.shared.options.supervision);
        }
    }
    let _engine = manager.attach(Arc::new(PluginRegistry::empty(Path::new("/fixture"))));
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .len(),
        3
    );
    assert!(matches!(manager.status(), HostStatus::Failed { .. }));
    manager.retry();
    assert!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .is_empty()
    );
    assert_eq!(manager.status(), HostStatus::Idle);
}

/// One typed state, `HostStatus`, names why the host is down in `/plugin`
/// and in the error a tool call routed to it gets.
#[tokio::test]
async fn a_host_that_is_down_names_why_in_plugin_status_and_tool_errors() {
    let policy = TestPolicyGuard::extension_host(true);
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
    let registration = {
        let mut registry = manager.shared.registry.lock().unwrap();
        let owner = registry
            .begin_owner(
                HostTier::Plugin,
                "probe",
                "probe",
                Some(fake_authority("probe")),
                "hash",
            )
            .unwrap();
        registry.mark_active(&owner);
        register(&mut registry, &owner, "probe_tool").unwrap();
        registry.live_tools()[0].clone()
    };
    let tool = super::tool::HostToolSpec::new(registration, Arc::clone(&manager.shared));
    let context = ToolContext::new(Path::new("/fixture"));
    let refused = "start failed: host did not apply the requested 1024 MiB kernel memory limit; initialization refused";
    for (slot, why) in [
        (
            super::HostSlot::Restarting {
                reason: "exited with signal: 9 (SIGKILL)".into(),
            },
            "restarting after: exited with signal: 9 (SIGKILL)".to_string(),
        ),
        (
            super::HostSlot::Failed {
                reason: refused.into(),
                stderr_tail: String::new(),
            },
            format!("{refused} (change or reload a plugin to retry)"),
        ),
        (super::HostSlot::Idle, "not started".to_string()),
    ] {
        *manager.shared.plugin.host.lock().unwrap() = slot;
        let report = super::render_status(&manager);
        assert!(
            report.starts_with(&format!("Extension host (experimental): {why}")),
            "{report}"
        );
        match tool.execute(json!({}), &context).await {
            Err(ToolError::NotAvailable { message }) => assert!(
                message.starts_with(&format!("extension host is down: {why}")),
                "{message}"
            ),
            other => panic!("expected a typed not-available error, got {other:?}"),
        }
    }
    drop(policy);
    let _off = TestPolicyGuard::extension_host(false);
    let disabled = "disabled by config ([features] extension_host is off)";
    assert_eq!(
        super::status_report(),
        format!("Extension host (experimental): {disabled}")
    );
    match tool.execute(json!({}), &context).await {
        Err(ToolError::NotAvailable { message }) => {
            assert_eq!(message, format!("extension host is down: {disabled}"))
        }
        other => panic!("expected a typed not-available error, got {other:?}"),
    }
}

#[tokio::test]
async fn three_crashes_stop_replay_until_explicit_retry() {
    let Some(node) = node_for_tests("crash budget") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["crash-tool", "refuses-approval"]).await;
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let failed_id = plugin_id(&fixture, "refuses-approval");
    let mut previous = None;
    for crash in 1..=3 {
        let tool = host_tool(&engine, fixture.workspace(), "crash_probe");
        let registration = manager
            .shared
            .registry
            .lock()
            .unwrap()
            .live_tools()
            .into_iter()
            .find(|t| t.name == "crash_probe")
            .unwrap();
        if let Some(old) = previous {
            assert_ne!(registration.owner, old);
        }
        previous = Some(registration.owner);
        let outcome = tool
            .execute(
                json!({}),
                &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
            )
            .await;
        assert!(matches!(outcome, Err(ToolError::NotAvailable { .. })));
        if crash < 3 {
            wait_host(&manager, || {
                manager.spawn_attempts() == crash + 1
                    && manager.live_tool_names().contains(&"crash_probe".into())
            })
            .await;
            assert!(matches!(
                manager.owner_state(&failed_id),
                Some(OwnerState::Failed(_))
            ));
        } else {
            wait_host(&manager, || {
                matches!(manager.status(), HostStatus::Failed { .. })
            })
            .await;
        }
    }
    assert_eq!(manager.spawn_attempts(), 3);
    let _another = manager.attach(fixture.registry());
    engine.sync().await.ok();
    assert_eq!(manager.spawn_attempts(), 3);
    manager.retry();
    engine.sync().await.unwrap();
    assert_eq!(manager.spawn_attempts(), 4);
    assert!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .is_empty()
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn an_activation_crash_does_not_prevent_other_receipts_replaying() {
    let Some(node) = node_for_tests("activation crash") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["crash-activation", "clash-script"]).await;
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    wait_host(&manager, || {
        manager.spawn_attempts() == 2
            && manager
                .live_tool_names()
                .contains(&"fixture_script_tool".into())
    })
    .await;
    assert!(matches!(
        manager.owner_state(&plugin_id(&fixture, "crash-activation")),
        Some(OwnerState::Failed(_))
    ));
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .len(),
        1
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn heartbeat_recovers_a_delayed_pong_then_kills_a_hung_host() {
    let Some(node) = node_for_tests("heartbeat") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["hang-tool"]).await;
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let tool = host_tool(&engine, fixture.workspace(), "hang_probe");
    let workspace = fixture.workspace().to_path_buf();
    let plugins = engine.plugin_view();
    let task = tokio::spawn(async move {
        tool.execute(
            json!({"ms": 400}),
            &ToolContext::new(&workspace).with_plugin_registry(plugins),
        )
        .await
    });
    wait_host(&manager, || {
        matches!(manager.status(), HostStatus::Unresponsive { .. })
    })
    .await;
    let result = task.await.unwrap();
    assert!(result.is_ok(), "delayed live call failed: {result:?}");
    wait_host(&manager, || {
        matches!(manager.status(), HostStatus::Ready { .. })
    })
    .await;
    assert_eq!(manager.spawn_attempts(), 1);
    let tool = host_tool(&engine, fixture.workspace(), "hang_probe");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tool.execute(
            json!({}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(ToolError::NotAvailable { .. })));
    wait_host(&manager, || {
        manager.spawn_attempts() == 2 && manager.live_tool_names().contains(&"hang_probe".into())
    })
    .await;
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .len(),
        1
    );
    manager.shutdown().await;
}

#[test]
fn old_host_callbacks_cannot_fault_or_remove_a_new_owner() {
    use super::supervisor::HostEvents;
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    manager
        .shared
        .plugin
        .host_generation
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let owner = {
        let mut registry = manager.shared.registry.lock().unwrap();
        let owner = registry
            .begin_owner(
                HostTier::Plugin,
                "fixture",
                "fixture",
                Some(fake_authority("fixture")),
                "hash-fixture",
            )
            .unwrap();
        registry.mark_active(&owner);
        register(&mut registry, &owner, "fixture_probe").unwrap();
        owner
    };
    let old = super::Events {
        shared: Arc::downgrade(&manager.shared),
        tier: HostTier::Plugin,
        generation: 1,
    };
    old.faulted(&protocol::FaultedParams {
        owner,
        error: "stale fault".into(),
    });
    old.exited(1, "stale exit".into(), String::new());
    assert_eq!(manager.owner_state("fixture"), Some(OwnerState::Active));
    assert_eq!(manager.live_tool_names(), ["fixture_probe"]);
    assert!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .is_empty()
    );
}

#[test]
fn a_new_attachment_retries_only_cooled_down_launch_failures() {
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
    *manager.shared.plugin.host.lock().unwrap() = super::HostSlot::Failed {
        reason: "missing Node".into(),
        stderr_tail: String::new(),
    };
    {
        let mut state = manager.shared.plugin.supervision.lock().unwrap();
        state.launch_failed = true;
        state.last_start = Some(Instant::now());
    }
    let plugins = Arc::new(PluginRegistry::empty(Path::new("/fixture")));
    let _first = manager.attach(Arc::clone(&plugins));
    assert!(matches!(manager.status(), HostStatus::Failed { .. }));
    manager.shared.plugin.supervision.lock().unwrap().last_start =
        Some(Instant::now() - Duration::from_secs(61));
    let _later = manager.attach(plugins);
    assert_eq!(manager.status(), HostStatus::Idle);
}

#[tokio::test]
async fn replay_rechecks_persisted_disable_and_keeps_workspace_tools_separate() {
    let Some(node) = node_for_tests("replay authority") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let a = FixturePlugins::new(&["crash-tool"]).await;
    let b = FixturePlugins::new(&["clash-script"]).await;
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: NODE,
        node_override: Some(node),
        bun_override: None,
        root: Some(a.root.clone()),
        supervision: super::SupervisionOptions {
            restart_backoff: Duration::from_secs(1),
            ..fast_supervision()
        },
    }));
    let first = manager.attach(a.registry());
    let second = manager.attach(b.registry());
    first.sync().await.unwrap();
    let tool = host_tool(&first, a.workspace(), "crash_probe");
    assert!(matches!(
        tool.execute(
            json!({}),
            &ToolContext::new(a.workspace()).with_plugin_registry(first.plugin_view())
        )
        .await,
        Err(ToolError::NotAvailable { .. })
    ));
    wait_host(&manager, || {
        matches!(manager.status(), HostStatus::Restarting { .. })
    })
    .await;
    // Keep the engine's snapshot stale deliberately. Replay must consult the
    // persisted state rather than restoring the previous owner's authority.
    a.disable("crash-tool");
    wait_host(&manager, || {
        manager.spawn_attempts() == 2
            && manager
                .live_tool_names()
                .contains(&"fixture_script_tool".into())
    })
    .await;
    assert!(installed(&first, a.workspace()).is_empty());
    assert_eq!(installed(&second, b.workspace()), ["fixture_script_tool"]);
    assert!(manager.owner_state(&plugin_id(&a, "crash-tool")).is_none());
    manager.shutdown().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(manager.status(), HostStatus::Idle);
    assert_eq!(
        manager.spawn_attempts(),
        2,
        "planned shutdown never restarts"
    );
}

#[tokio::test]
async fn explicit_retry_refreshes_same_byte_authority_without_inheriting_old_handles() {
    let Some(node) = node_for_tests("same byte retry") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["clash-script"]).await;
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let old = host_tool(&engine, fixture.workspace(), "fixture_script_tool");
    let old_owner = manager.shared.registry.lock().unwrap().live_tools()[0]
        .owner
        .clone();
    let mut updated = discover_with_config(&fixture.config);
    updated.enable("clash-script").unwrap();
    manager.refresh_workspace(&Arc::new(updated));
    manager.retry();
    engine.sync().await.unwrap();
    let current_owner = manager.shared.registry.lock().unwrap().live_tools()[0]
        .owner
        .clone();
    assert_ne!(old_owner, current_owner);
    assert!(matches!(
        old.execute(
            json!({}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view())
        )
        .await,
        Err(ToolError::NotAvailable { .. })
    ));
    let current = host_tool(&engine, fixture.workspace(), "fixture_script_tool");
    assert!(
        current
            .execute(
                json!({}),
                &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view())
            )
            .await
            .is_ok()
    );
    assert_eq!(
        manager.spawn_attempts(),
        1,
        "a healthy process need not restart"
    );
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// Runtime selection (Bun / Node) and the memory cap
// ---------------------------------------------------------------------------

/// A Bun >= 1.4.0 for the Bun integration tests, or `None` (skip) unless
/// `CODEWHALE_EXT_HOST_BUN_TESTS` requires one.
fn bun_for_tests(test: &str) -> Option<PathBuf> {
    let resolution = codewhale_dependencies::resolve_extension_host_runtime(
        codewhale_config::extension_host::ExtensionHostRuntime::Bun,
        None,
        None,
        crate::extension_host::bundle_sha256(),
    );
    match resolution.selected {
        Some(runtime) => Some(runtime.path),
        None if std::env::var_os("CODEWHALE_EXT_HOST_BUN_TESTS").is_some() => panic!(
            "{test}: CODEWHALE_EXT_HOST_BUN_TESTS is set but {}",
            resolution.failure()
        ),
        None => {
            eprintln!("skipping {test}: {}", resolution.failure());
            None
        }
    }
}

fn bun_manager(
    fixture: &FixturePlugins,
    bun: PathBuf,
    supervision: super::SupervisionOptions,
) -> Arc<ExtensionHostManager> {
    Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Bun,
        node_override: None,
        bun_override: Some(bun),
        root: Some(fixture.root.clone()),
        supervision,
    }))
}

#[test]
fn node_is_the_default_runtime_and_bun_is_an_opt_in() {
    use codewhale_config::extension_host::{ExtensionHostConfig, ExtensionHostRuntime as Choice};
    let parse = |text: &str| toml::from_str::<ExtensionHostConfig>(text).unwrap();
    // Bun is not the default until it is qualified on every platform.
    assert_eq!(parse("").effective_runtime(), Choice::Node);
    assert_eq!(
        parse("node = \"/opt/node\"").effective_runtime(),
        Choice::Node
    );
    // A table that names only a Bun asks for Bun.
    assert_eq!(parse("bun = \"/opt/bun\"").effective_runtime(), Choice::Bun);
    assert_eq!(
        parse("node = \"/n\"\nbun = \"/b\"").effective_runtime(),
        Choice::Node
    );
    assert_eq!(
        parse("runtime = \"bun\"\nnode = \"/n\"").effective_runtime(),
        Choice::Bun
    );
    assert_eq!(
        parse("runtime = \"auto\"").effective_runtime(),
        Choice::Auto
    );
    assert!(toml::from_str::<ExtensionHostConfig>("runtime = \"deno\"").is_err());
    let options = ExtensionHostOptions::from_config(Some(&parse("node = \"~/node\"")));
    assert_eq!(options.runtime, Choice::Node);
    assert!(!options.node_override.unwrap().starts_with("~"));
    assert_eq!(
        ExtensionHostOptions::from_config(None).runtime,
        Choice::Node
    );
    assert_eq!(ExtensionHostOptions::default().runtime, Choice::Node);
}

#[test]
fn launch_plan_gives_each_runtime_its_own_flags() {
    use codewhale_dependencies::{HostRuntime, HostRuntimeKind};
    let home = tempfile::tempdir().unwrap();
    let bundle = home.path().join("codewhale-extension-host.mjs");
    for (kind, expected) in [
        (
            HostRuntimeKind::Bun,
            vec!["--no-install", "--no-env-file", "--config=", "--no-addons"],
        ),
        (
            HostRuntimeKind::Node,
            [
                &[
                    "--max-old-space-size=256",
                    "--disable-proto=throw",
                    "--no-addons",
                ][..],
                if cfg!(windows) {
                    &["--preserve-symlinks", "--preserve-symlinks-main"][..]
                } else {
                    &[][..]
                },
                &["--no-experimental-sqlite", "--no-experimental-ffi"][..],
            ]
            .concat(),
        ),
    ] {
        let runtime = HostRuntime {
            kind,
            path: PathBuf::from("/opt/runtime/bin").join(kind.name()),
            version: (1, 4, 0),
            compiled: false,
            native_code_flags: match kind {
                HostRuntimeKind::Bun => Vec::new(),
                HostRuntimeKind::Node => codewhale_dependencies::NODE_NATIVE_CODE_FLAGS.to_vec(),
            },
        };
        let launch = super::supervisor::plan_launch(
            HostTier::Builtin,
            &runtime,
            &bundle,
            home.path(),
            1 << 30,
            &[],
        )
        .unwrap();
        // Wrapped or not, the runtime's flags come right before the bundle.
        let at = launch
            .args
            .iter()
            .position(|arg| Path::new(arg) == bundle)
            .expect("bundle in argv");
        let flags = &launch.args[at - expected.len()..at];
        for (flag, want) in flags.iter().zip(&expected) {
            assert!(flag.starts_with(want), "{kind:?}: {flags:?}");
        }
        assert_eq!(launch.runtime, runtime);
        assert_eq!(launch.memory_cap, 1 << 30);
        assert_eq!(
            launch.memory,
            super::supervisor::MemoryEnforcement::planned(kind)
        );
        let shadow_realm_off = launch
            .runtime_env
            .contains(&("BUN_JSC_useShadowRealm".to_string(), "0".to_string()));
        // An inherited NODE_OPTIONS preload must not run before the lockdown.
        assert!(
            launch
                .runtime_env
                .contains(&("NODE_OPTIONS".to_string(), String::new())),
            "{:?}",
            launch.runtime_env
        );
        assert_eq!(shadow_realm_off, kind == HostRuntimeKind::Bun);
        if kind == HostRuntimeKind::Bun {
            assert!(
                !launch
                    .args
                    .iter()
                    .any(|arg| arg.starts_with("--max-old-space"))
            );
        }
    }
    // Where the kernel limit comes from on each platform.
    use super::supervisor::MemoryEnforcement;
    let (bun, node) = (
        MemoryEnforcement::planned(HostRuntimeKind::Bun),
        MemoryEnforcement::planned(HostRuntimeKind::Node),
    );
    if cfg!(target_os = "macos") {
        assert_eq!(
            (bun, node),
            (MemoryEnforcement::Jetsam, MemoryEnforcement::Heartbeat)
        );
    } else if cfg!(target_os = "linux") {
        assert_eq!(
            (bun, node),
            (MemoryEnforcement::Rlimit, MemoryEnforcement::Rlimit)
        );
    } else if cfg!(windows) {
        assert_eq!(
            (bun, node),
            (MemoryEnforcement::JobObject, MemoryEnforcement::JobObject)
        );
    }
}

#[tokio::test]
async fn handshake_refuses_a_runtime_or_version_mismatch_and_an_unapplied_kernel_cap() {
    let Some(node) = node_for_tests("handshake_refuses_a_runtime_or_version_mismatch") else {
        return;
    };
    use codewhale_dependencies::HostRuntimeKind;
    let home = tempfile::tempdir().unwrap();
    let bundle = super::materialize_bundle(home.path()).unwrap();
    // Resolved, so the Node gets the native-code flags it accepts.
    let runtime = codewhale_dependencies::resolve_extension_host_runtime(
        NODE,
        Some(node.as_path()),
        None,
        crate::extension_host::bundle_sha256(),
    )
    .selected
    .expect("the test Node resolves");
    struct NoEvents;
    impl super::supervisor::HostEvents for NoEvents {
        fn register(&self, _: &protocol::RegisterParams) -> protocol::RegisterResult {
            unreachable!()
        }
        fn unregister(&self, _: &protocol::UnregisterParams) {}
        fn faulted(&self, _: &protocol::FaultedParams) {}
        fn log(&self, _: &protocol::LogParams) {}
        fn exited(&self, _: u64, _: String, _: String) {}
    }
    for case in ["runtime", "version", "cap"] {
        let mut launch = super::supervisor::plan_launch(
            HostTier::Plugin,
            &runtime,
            &bundle,
            home.path(),
            1 << 30,
            &[],
        )
        .unwrap();
        let expected = match case {
            // The core believes it launched Bun; the host truthfully says Node.
            "runtime" => {
                launch.runtime.kind = HostRuntimeKind::Bun;
                "host reports runtime node but bun was launched"
            }
            // The pinned probe saw another version: the binary at that path
            // was replaced after it was probed.
            "version" => {
                launch.runtime.version = (0, 0, 1);
                "the runtime binary changed mid-session"
            }
            // An actual Node host reports no kernel cap. Even with matching
            // runtime/digest, the requested hard boundary must block admission.
            _ => {
                launch.memory = super::supervisor::MemoryEnforcement::Jetsam;
                "host did not apply the requested 1024 MiB kernel memory limit; initialization refused"
            }
        };
        let error = match super::supervisor::HostProcess::spawn(
            1,
            &launch,
            super::bundle_sha256(),
            Arc::new(NoEvents),
        )
        .await
        {
            Ok(_) => panic!("{expected}"),
            Err(error) => error,
        };
        assert!(error.contains(expected), "{error}");
    }
}

/// A host that reports another tier than the one launched, or built-in module
/// digests other than the ones the core pins, is refused at the handshake like
/// a runtime mismatch: before initialization, with the reason.
#[tokio::test]
async fn handshake_refuses_a_tier_or_built_in_module_digest_mismatch() {
    let Some(node) = node_for_tests("handshake_refuses_a_tier_or_built_in_module_digest_mismatch")
    else {
        return;
    };
    // A table that pins a module the host bundle does not embed.
    const PINNED: &[super::tier::BuiltinModule] = &[super::tier::BuiltinModule {
        id: "demo",
        source_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        tools: &[],
    }];
    struct NoEvents;
    impl super::supervisor::HostEvents for NoEvents {
        fn register(&self, _: &protocol::RegisterParams) -> protocol::RegisterResult {
            unreachable!()
        }
        fn unregister(&self, _: &protocol::UnregisterParams) {}
        fn faulted(&self, _: &protocol::FaultedParams) {}
        fn log(&self, _: &protocol::LogParams) {}
        fn exited(&self, _: u64, _: String, _: String) {}
    }
    let home = tempfile::tempdir().unwrap();
    let bundle = super::materialize_bundle(home.path()).unwrap();
    let runtime = codewhale_dependencies::resolve_extension_host_runtime(
        NODE,
        Some(node.as_path()),
        None,
        crate::extension_host::bundle_sha256(),
    )
    .selected
    .expect("the test Node resolves");
    let launch_for = |tier: HostTier| {
        super::supervisor::plan_launch(tier, &runtime, &bundle, home.path(), 1 << 30, &[]).unwrap()
    };

    // Each tier reports its own tier and the exact embedded module digests.
    for tier in HostTier::ALL {
        let host = super::supervisor::HostProcess::spawn(
            1,
            &launch_for(tier),
            super::bundle_sha256(),
            Arc::new(NoEvents),
        )
        .await
        .unwrap_or_else(|error| panic!("{tier:?} host: {error}"));
        assert_eq!(host.tier, tier);
        host.shutdown().await;
    }

    let mut embedded: Vec<_> = super::tier::BUILTIN_MODULES
        .iter()
        .map(|module| format!("{}={}", module.id, &module.source_sha256[..12]))
        .collect();
    embedded.sort();
    let module_mismatch = format!(
        "host bundle embeds the built-in module digests [{}] but the core pins [demo=0123456789ab]",
        embedded.join(", ")
    );
    for (case, expected) in [
        (
            "tier",
            "host reports the plugin tier but the builtin tier was launched",
        ),
        ("modules", module_mismatch.as_str()),
    ] {
        let mut launch = launch_for(HostTier::Plugin);
        match case {
            // The core believes it launched the builtin tier; the host, started
            // with `--tier=plugin`, truthfully says plugin.
            "tier" => launch.tier = HostTier::Builtin,
            _ => launch.builtin_modules = PINNED,
        }
        let error = match super::supervisor::HostProcess::spawn(
            1,
            &launch,
            super::bundle_sha256(),
            Arc::new(NoEvents),
        )
        .await
        {
            Ok(_) => panic!("{case}: {expected}"),
            Err(error) => error,
        };
        assert!(error.contains(expected), "{case}: {error}");
    }
}

#[tokio::test]
async fn bun_host_runs_the_dsh_plugin_reports_bun_and_restarts_on_bun() {
    let Some(bun) = bun_for_tests("bun_host_runs_the_dsh_plugin") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["dsh-workspace-deps"]).await;
    let manager = bun_manager(&fixture, bun.clone(), fast_supervision());
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let HostStatus::Ready {
        runtime,
        runtime_version,
        ..
    } = manager.status()
    else {
        panic!("{:?} {:?}", manager.status(), manager.diagnostics());
    };
    assert_eq!(runtime, "bun");
    let banner = std::process::Command::new(&bun)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        runtime_version,
        String::from_utf8_lossy(&banner.stdout).trim()
    );
    let summary = manager.runtime_summary().unwrap();
    assert!(summary.starts_with("bun "), "{summary}");
    assert!(summary.contains("(runtime = \"bun\")"), "{summary}");
    let report = super::render_status(&manager);
    assert!(
        report.contains(&format!("· bun {runtime_version} ·")),
        "{report}"
    );
    assert!(report.contains("runtime: bun "), "{report}");

    let tool = host_tool(&engine, fixture.workspace(), "load_workspace_dependencies");
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let result = tool.execute(json!({}), &context).await.unwrap();
    assert!(result.success, "{}", result.content);
    let payload: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(payload["pythonDistributions"]["numpy"], "2.1.0");

    // A crash restarts on the pinned runtime; it is never re-resolved.
    let pid = manager.host_pid().unwrap();
    #[cfg(unix)]
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    #[cfg(windows)]
    assert!(
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait_host(&manager, || {
        manager.spawn_attempts() == 2 && matches!(manager.status(), HostStatus::Ready { .. })
    })
    .await;
    assert!(matches!(
        manager.status(),
        HostStatus::Ready { runtime: "bun", .. }
    ));
    assert_eq!(manager.runtime_summary().unwrap(), summary);
    #[cfg(target_os = "macos")]
    {
        // This kill came from the operator, not the memory hog. The observed
        // signal and configured cap must not invent a cause for it.
        let diagnostics = manager.diagnostics();
        assert!(
            diagnostics
                .iter()
                .any(|line| line.contains("SIGKILL cause unavailable")),
            "{diagnostics:?}"
        );
        assert!(
            !diagnostics
                .iter()
                .any(|line| line.contains("exceeded its memory cap")),
            "{diagnostics:?}"
        );
    }
    manager.shutdown().await;
}

/// `runtime = "auto"`: a Bun that passes the version probe but cannot start
/// the host is reported once, and Node runs the host for the rest of the
/// session. `runtime = "bun"` never falls back, and a launch that never
/// completed a handshake pins nothing.
#[cfg(unix)]
#[tokio::test]
async fn auto_uses_node_for_the_session_when_the_bun_host_fails_to_start() {
    use std::os::unix::fs::PermissionsExt;
    let Some(node) = node_for_tests("auto_uses_node_when_the_bun_host_fails_to_start") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["slow-tool"]).await;
    let bin = tempfile::tempdir().unwrap();
    let bun = bin.path().join("bun");
    std::fs::write(
        &bun,
        "#!/bin/sh\ncase \"$1\" in --version) echo 1.4.0;; *) echo 'simulated Bun start failure' >&2; exit 3;; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&bun, std::fs::Permissions::from_mode(0o755)).unwrap();
    let manager_for = |runtime| {
        Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime,
            node_override: Some(node.clone()),
            bun_override: Some(bun.clone()),
            root: Some(fixture.root.clone()),
            supervision: fast_supervision(),
        }))
    };

    let manager = manager_for(codewhale_config::extension_host::ExtensionHostRuntime::Auto);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    assert!(
        matches!(
            manager.status(),
            HostStatus::Ready {
                runtime: "node",
                ..
            }
        ),
        "{:?} {:?}",
        manager.status(),
        manager.diagnostics()
    );
    let diagnostics = manager.diagnostics();
    let reported: Vec<_> = diagnostics
        .iter()
        .filter(|line| line.contains("uses Node for the rest of this session"))
        .collect();
    assert_eq!(reported.len(), 1, "{diagnostics:?}");
    assert!(
        reported[0].contains(&bun.display().to_string()),
        "{diagnostics:?}"
    );
    let summary = manager.runtime_summary().unwrap();
    assert!(summary.starts_with("node "), "{summary}");
    assert!(summary.contains("(runtime = \"auto\")"), "{summary}");
    assert!(
        summary.contains("Bun failed to start this session"),
        "{summary}"
    );
    assert_eq!(manager.spawn_attempts(), 2);
    manager.shutdown().await;

    let explicit = manager_for(codewhale_config::extension_host::ExtensionHostRuntime::Bun);
    let engine = explicit.attach(fixture.registry());
    let _ = engine.sync().await;
    assert!(
        matches!(explicit.status(), HostStatus::Failed { .. }),
        "{:?} {:?}",
        explicit.status(),
        explicit.diagnostics()
    );
    assert_eq!(explicit.spawn_attempts(), 1);
    assert_eq!(explicit.runtime_summary(), None);
}

/// The cap holds for memory outside the JS heap too (Buffers), which Node's
/// `--max-old-space-size` never bounded. Linux and Windows: the kernel fails
/// the allocation at 1 GiB. macOS: Bun's jetsam limit gets the host killed by
/// the kernel; a Node host is killed by the heartbeat check.
async fn memory_hog_is_stopped(
    manager: Arc<ExtensionHostManager>,
    fixture: &FixturePlugins,
    kind: codewhale_dependencies::HostRuntimeKind,
) {
    use super::supervisor::MemoryEnforcement;
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    // The host confirmed the enforcement this platform plans for it (for
    // macOS + Bun: the jetsam limit it applied to itself, via `host/hello`).
    let HostStatus::Ready { memory, .. } = manager.status() else {
        panic!("{:?} {:?}", manager.status(), manager.diagnostics());
    };
    assert_eq!(memory, MemoryEnforcement::planned(kind));
    let tool = host_tool(&engine, fixture.workspace(), "memory_hog");
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        tool.execute(json!({"mib": 2048}), &context),
    )
    .await
    .expect("the hog is stopped well before 60 s");
    match &outcome {
        Ok(result) => assert!(
            !result.success,
            "allocating 2 GiB must not succeed under the cap: {}",
            result.content
        ),
        Err(ToolError::NotAvailable { message }) => {
            assert!(message.contains("extension host exited"), "{message}");
        }
        // Linux (RLIMIT_DATA) and Windows (Job Object): the kernel refuses the
        // allocation, the runtime throws inside the tool, and the host lives on.
        Err(ToolError::ExecutionFailed { message, .. })
            if matches!(
                memory,
                MemoryEnforcement::Rlimit | MemoryEnforcement::JobObject
            ) =>
        {
            assert!(
                message.contains("allocation failed")
                    || (kind == codewhale_dependencies::HostRuntimeKind::Bun
                        && message.ends_with("RangeError: Out of memory")),
                "{message}"
            );
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
    let stopped_by = match memory {
        MemoryEnforcement::Jetsam => {
            Some("configured kernel memory limit: 400 MiB; SIGKILL cause unavailable")
        }
        MemoryEnforcement::Heartbeat => Some("exceeded its memory cap"),
        _ => None,
    };
    if let Some(stopped_by) = stopped_by {
        // Pending calls settle before the existing exit callback publishes
        // its diagnostic. Observe that callback rather than race its delivery.
        wait_host(&manager, || {
            manager
                .diagnostics()
                .iter()
                .any(|line| line.contains(stopped_by))
        })
        .await;
    }
    manager.shutdown().await;
}

fn memory_cap_supervision() -> super::SupervisionOptions {
    super::SupervisionOptions {
        // Linux cannot start either runtime under much less than 1 GiB of
        // RLIMIT_DATA (see `HOST_MEMORY_CAP`); on macOS 400 MiB keeps the
        // test fast for both the jetsam limit and the heartbeat check.
        memory_cap: if cfg!(target_os = "macos") {
            400 << 20
        } else {
            super::supervisor::HOST_MEMORY_CAP
        },
        // Production hang detection. With the fast 600 ms hang timeout, a host
        // stalled in GC near its cap was killed for a missed heartbeat and
        // restarted before the memory limit itself stopped it, so the test
        // never observed the enforcement it exists to prove.
        ping_timeout: Duration::from_secs(3),
        hang_timeout: super::supervisor::PING_DEADLINE,
        ..fast_supervision()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[tokio::test]
async fn memory_cap_stops_a_node_host() {
    let Some(node) = node_for_tests("memory_cap_stops_a_node_host") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["memory-hog"]).await;
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: NODE,
        node_override: Some(node),
        bun_override: None,
        root: Some(fixture.root.clone()),
        supervision: memory_cap_supervision(),
    }));
    memory_hog_is_stopped(
        manager,
        &fixture,
        codewhale_dependencies::HostRuntimeKind::Node,
    )
    .await;
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[tokio::test]
async fn memory_cap_stops_a_bun_host() {
    let Some(bun) = bun_for_tests("memory_cap_stops_a_bun_host") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["memory-hog"]).await;
    let manager = bun_manager(&fixture, bun, memory_cap_supervision());
    memory_hog_is_stopped(
        manager,
        &fixture,
        codewhale_dependencies::HostRuntimeKind::Bun,
    )
    .await;
}

/// Actual Rust containment/memory authority, using the same exact compiled
/// image as the containment receipt rather than transferring system-Bun proof.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[tokio::test]
async fn compiled_native_host_memory_cap_is_enforced() {
    let Some(binary) = compiled_image_for_tests() else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["memory-hog"]).await;
    let manager = bun_manager(&fixture, binary, memory_cap_supervision());
    memory_hog_is_stopped(
        manager,
        &fixture,
        codewhale_dependencies::HostRuntimeKind::Bun,
    )
    .await;
    eprintln!(
        "compiled-native-memory=passed platform={} arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
}

// ---------------------------------------------------------------------------
// Extension commands
// ---------------------------------------------------------------------------

fn register_command(
    registry: &mut OwnerRegistry,
    owner: &OwnerRef,
    name: &str,
    hint: Option<&str>,
) -> Result<u64, String> {
    registry.register(&RegisterParams {
        scope: None,
        owner: owner.clone(),
        kind: RegisterKind::Command,
        spec: RegisterSpecWire {
            name: name.to_string(),
            description: "d".to_string(),
            input_schema: None,
            argument_hint: hint.map(str::to_string),
        },
    })
}

#[test]
fn command_registry_refuses_shadowing_and_undoes_exactly_one_entry() {
    let _catalog = stub_builtin_commands();
    let mut registry = OwnerRegistry::new();
    let a = registry
        .begin_owner(
            HostTier::Plugin,
            "a",
            "a",
            Some(fake_authority("a")),
            "hash-a",
        )
        .unwrap();
    let b = registry
        .begin_owner(
            HostTier::Plugin,
            "b",
            "b",
            Some(fake_authority("b")),
            "hash-b",
        )
        .unwrap();

    // Built-in names, their aliases, and the fixed mode aliases, as the
    // catalog says (the real table: `commands::extension_host_tests`).
    for name in ["help", "trust", "model", "jihua", "zidong", "stub-alias"] {
        let refused = register_command(&mut registry, &a, name, None)
            .expect_err(&format!("/{name} must be refused"));
        assert!(refused.contains("built-in command"), "{name}: {refused}");
    }
    // Names outside DSH's grammar (upper case, a leading digit or slash,
    // spaces), and over-long ones.
    for name in [
        "Hello",
        "9lives",
        "/slash",
        "two words",
        "",
        &"x".repeat(65),
    ] {
        let refused = register_command(&mut registry, &a, name, None)
            .expect_err(&format!("{name:?} must be refused"));
        assert!(refused.contains("invalid"), "{name:?}: {refused}");
    }
    // A command has no input schema; descriptions and hints are bounded,
    // non-empty, single-line text.
    let mut params = RegisterParams {
        scope: None,
        owner: a.clone(),
        kind: RegisterKind::Command,
        spec: RegisterSpecWire {
            name: "ok-name".to_string(),
            description: "fine".to_string(),
            input_schema: json!({"type": "object"}).as_object().cloned(),
            argument_hint: None,
        },
    };
    assert!(
        registry
            .register(&params)
            .unwrap_err()
            .contains("input schema")
    );
    params.spec.input_schema = None;
    for (description, hint, expect) in [
        ("  ", None, "needs a description"),
        ("two\nlines", None, "control characters"),
        ("fine", Some(""), "must not be empty"),
        ("fine", Some("\u{1b}[31m<x>"), "control characters"),
    ] {
        params.spec.description = description.to_string();
        params.spec.argument_hint = hint.map(str::to_string);
        let refused = registry.register(&params).unwrap_err();
        assert!(
            refused.contains(expect),
            "{description:?}/{hint:?}: {refused}"
        );
    }
    params.spec.description = "x".repeat(super::registry::MAX_COMMAND_DESCRIPTION_BYTES + 1);
    params.spec.argument_hint = None;
    assert!(
        registry
            .register(&params)
            .unwrap_err()
            .contains("description")
    );
    params.spec.description = "fine".to_string();
    params.spec.argument_hint = Some("h".repeat(super::registry::MAX_COMMAND_HINT_BYTES + 1));
    assert!(
        registry
            .register(&params)
            .unwrap_err()
            .contains("argument hint")
    );

    let first = register_command(&mut registry, &a, "shared-name", Some("<x>")).unwrap();
    // Another plugin cannot take it.
    let refused = register_command(&mut registry, &b, "shared-name", None).unwrap_err();
    assert!(
        refused.contains("already registered by extension"),
        "{refused}"
    );
    // Commands and tools are separate namespaces: the model calls one, the
    // user the other.
    register(&mut registry, &b, "shared_name_tool").unwrap();
    register(&mut registry, &a, "shared-name").unwrap();
    // The same owner re-registering retires the old handle.
    let second = register_command(&mut registry, &a, "shared-name", None).unwrap();
    assert_ne!(first, second);
    registry.mark_active(&a);
    registry.unregister(&a, first); // stale: must not remove the newer entry
    let live = registry.live_commands();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].handle, second);
    assert!(registry.live_command(second, "a", a.generation).is_some());
    // A foreign owner cannot unregister it, and a stale generation finds nothing.
    registry.unregister(&b, second);
    assert!(registry.live_command(second, "a", a.generation).is_some());
    assert!(
        registry
            .live_command(second, "a", a.generation + 1)
            .is_none()
    );
    assert!(registry.live_command(second, "b", a.generation).is_none());
    // Unregistering a command leaves the same owner's tool alone.
    registry.unregister(&a, second);
    assert!(registry.live_commands().is_empty());
    let tools = registry.live_tools();
    assert_eq!(tools.len(), 1, "b is not active yet; a's tool stays");
    assert_eq!(tools[0].name, "shared-name");

    // Caps.
    for index in 0..super::registry::MAX_COMMANDS_PER_OWNER {
        register_command(&mut registry, &a, &format!("c{index}"), None).unwrap();
    }
    assert!(
        register_command(&mut registry, &a, "one-too-many", None)
            .unwrap_err()
            .contains("at most")
    );

    // Revocation is synchronous and total, and a revoked owner cannot register.
    registry.mark_active(&a);
    assert_eq!(
        registry.live_commands().len(),
        super::registry::MAX_COMMANDS_PER_OWNER
    );
    assert_eq!(registry.revoke_owner("a"), Some(a.clone()));
    assert!(registry.live_commands().is_empty());
    assert!(register_command(&mut registry, &a, "after-revoke", None).is_err());

    // A host crash drops every command, whoever owned it.
    registry.mark_active(&b);
    let handle = register_command(&mut registry, &b, "survivor", None).unwrap();
    assert!(registry.live_command(handle, "b", b.generation).is_some());
    registry.host_exited(HostTier::Plugin, "exited");
    assert!(registry.live_commands().is_empty());
    assert!(registry.live_command(handle, "b", b.generation).is_none());
}

/// The host protocol gains `command/run` without gaining any core
/// authority: the lint that guards `METHODS` runs in `protocol::tests`; this
/// pins what the new method's request and its answer look like.
#[test]
fn command_run_and_its_answers_have_the_documented_shapes() {
    let request = protocol::CoreRequest::CommandRun(protocol::CommandRunParams {
        handle: 7,
        command_id: "c".to_string(),
        raw_input: "args".to_string(),
        deadline_ms: 30_000,
        workspace: None,
        session_id: None,
        agent_id: None,
        origin_turn_id: None,
    });
    assert_eq!(request.method(), "command/run");
    assert_eq!(request.deadline(), Duration::from_secs(30));
    for (value, expect) in [
        (
            json!({"kind": "success", "text": "t"}),
            protocol::CommandResultWire::Success {
                text: Some("t".into()),
            },
        ),
        (
            json!({"kind": "success"}),
            protocol::CommandResultWire::Success { text: None },
        ),
        (
            json!({"kind": "error", "text": "no"}),
            protocol::CommandResultWire::Error { text: "no".into() },
        ),
        (
            json!({"kind": "submit", "prompt": "p"}),
            protocol::CommandResultWire::Submit {
                prompt: "p".into(),
                text: None,
            },
        ),
    ] {
        let parsed: protocol::CommandResultWire = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed, expect);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    }
    assert!(
        serde_json::from_value::<protocol::CommandResultWire>(json!({"kind": "approve"})).is_err()
    );
}

/// What a command shows is plugin-controlled text: escape sequences are
/// stripped, and a prompt too large to submit whole is refused, not cut.
#[tokio::test]
async fn command_output_is_stripped_bounded_and_oversized_prompts_are_refused() {
    use super::command::{self, CommandOutcome};
    // The wire-to-outcome mapping needs a host to answer, so run it against
    // a stub that returns canned results for `command/run`.
    let Some(node) = node_for_tests("command_output") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("canned");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"canned","version":"0.1.0","description":"canned results","license":"MIT","extensions":{"net.codewhale":{"native":{"path":"index.mjs"}}}}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("index.mjs"),
        format!(
            r#"export const name = 'canned'
export const inject = ['commands']
export function apply(ctx) {{
  ctx.commands.register({{ name: 'big-text', description: 'd', handler: () => 'x'.repeat({}) }})
  ctx.commands.register({{ name: 'big-prompt', description: 'd', handler: () => ({{ kind: 'submit', prompt: 'p'.repeat({}) }}) }})
  ctx.commands.register({{ name: 'blank-prompt', description: 'd', handler: () => ({{ kind: 'submit', prompt: '  \u001b[0m ' }}) }})
  ctx.commands.register({{ name: 'bad-result', description: 'd', handler: () => ({{ kind: 'approve' }}) }})
}}
"#,
            command::MAX_TEXT_BYTES * 2,
            command::MAX_PROMPT_BYTES + 1
        ),
    )
    .unwrap();
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&[]).await;
    crate::plugins::install::install(
        crate::plugins::install::PluginInstallSource::LocalPath(plugin),
        &fixture.config.user_plugins_dir,
        crate::plugins::install::DEFAULT_MAX_SIZE_BYTES,
        &codewhale_net::network_policy::NetworkPolicy::default(),
        false,
        &|_| None,
    )
    .await
    .unwrap();
    let mut plugins = discover_with_config(&fixture.config);
    plugins.trust("canned").unwrap();
    plugins.enable("canned").unwrap();
    let manager = fixture.manager(node);
    let engine = manager.attach(Arc::new(plugins));
    engine.sync().await.unwrap();
    let run = |name: &'static str| {
        let manager = Arc::clone(&manager);
        let plugins = engine.plugin_view();
        async move {
            let entry = manager
                .commands_for_plugins(plugins.as_ref())
                .into_iter()
                .find(|entry| entry.registration.name == name)
                .unwrap_or_else(|| panic!("{name} is not live"));
            command::run(&manager.shared, &entry.reference(), "", None).await
        }
    };
    match run("big-text").await.unwrap() {
        CommandOutcome::Show { text } => {
            assert!(
                text.ends_with("(output truncated)"),
                "{}",
                &text[text.len() - 40..]
            );
            assert!(text.len() < command::MAX_TEXT_BYTES + 64);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        run("big-prompt")
            .await
            .unwrap_err()
            .contains("not submitted")
    );
    assert!(
        run("blank-prompt")
            .await
            .unwrap_err()
            .contains("empty prompt")
    );
    assert!(
        run("bad-result")
            .await
            .unwrap_err()
            .contains("unknown result kind")
    );
    manager.shutdown().await;
}

/// With no built-in command catalog installed, a command cannot be checked
/// against the built-in names, so its registration is refused (never accepted
/// unchecked); tools are not affected. With one, the catalog decides.
#[test]
fn a_command_registration_is_refused_when_no_built_in_catalog_is_installed() {
    let owner = |registry: &mut OwnerRegistry| {
        registry
            .begin_owner(
                HostTier::Plugin,
                "a",
                "a",
                Some(fake_authority("a")),
                "hash-a",
            )
            .unwrap()
    };
    let _absent = CatalogGuard::absent();
    let mut registry = OwnerRegistry::new();
    let a = owner(&mut registry);
    let refused = register_command(&mut registry, &a, "fine-name", None).unwrap_err();
    assert!(
        refused.contains("no built-in command catalog is installed"),
        "{refused}"
    );
    assert!(registry.live_commands().is_empty());
    // A name the grammar refuses is refused for its own reason first.
    assert!(
        register_command(&mut registry, &a, "Not Valid", None)
            .unwrap_err()
            .contains("invalid")
    );
    // Tools do not consult the command table.
    register(&mut registry, &a, "a_tool").unwrap();

    let _stub = stub_builtin_commands();
    register_command(&mut registry, &a, "fine-name", None).unwrap();
    let clash = register_command(&mut registry, &a, "help", None).unwrap_err();
    assert!(
        clash.contains("collides with a built-in command"),
        "{clash}"
    );
}

/// A command from a host that is down says so immediately instead of
/// hanging, and a revoked registration cannot be run.
#[tokio::test]
async fn a_command_from_a_dead_host_reports_host_down() {
    let _catalog = stub_builtin_commands();
    let policy = TestPolicyGuard::extension_host(true);
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
    let reference = {
        let mut registry = manager.shared.registry.lock().unwrap();
        let owner = registry
            .begin_owner(
                HostTier::Plugin,
                "probe",
                "probe",
                Some(fake_authority("probe")),
                "hash",
            )
            .unwrap();
        registry.mark_active(&owner);
        let handle = register_command(&mut registry, &owner, "probe-cmd", None).unwrap();
        super::command::ExtensionCommandRef {
            selection: None,
            scope: None,
            content_hash: String::new(),
            handle,
            plugin_id: "probe".into(),
            generation: owner.generation,
            origin: "extension:probe".into(),
            workspace: PathBuf::from("/w"),
        }
    };
    let refused = "start failed: no runtime";
    for (slot, why) in [
        (
            super::HostSlot::Restarting {
                reason: "exited with signal: 9 (SIGKILL)".into(),
            },
            "restarting after: exited with signal: 9 (SIGKILL)".to_string(),
        ),
        (
            super::HostSlot::Failed {
                reason: refused.into(),
                stderr_tail: String::new(),
            },
            format!("{refused} (change or reload a plugin to retry)"),
        ),
        (super::HostSlot::Idle, "not started".to_string()),
    ] {
        *manager.shared.plugin.host.lock().unwrap() = slot;
        let started = Instant::now();
        let error = super::command::run(&manager.shared, &reference, "", None)
            .await
            .unwrap_err();
        assert!(
            error.starts_with(&format!("extension host is down: {why}")),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    drop(policy);
    let _off = TestPolicyGuard::extension_host(false);
    let error = super::command::run(&manager.shared, &reference, "", None)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "extension host is down: disabled by config ([features] extension_host is off)"
    );
    assert!(super::live_commands_for(Path::new("/w")).is_empty());
}

/// The call is bounded by `command_run_deadline` and cancelled in the host,
/// which stays usable; an in-flight command fails with "host down" when the
/// host is killed, and after the restart only a fresh reference works.
#[tokio::test]
async fn a_slow_command_is_cancelled_and_a_killed_host_fails_it_as_down() {
    let Some(node) = node_for_tests("slow_command") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["ext-commands"]).await;
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: NODE,
        node_override: Some(node.clone()),
        bun_override: None,
        root: Some(fixture.root.clone()),
        supervision: super::SupervisionOptions {
            command_run_deadline: Duration::from_millis(300),
            ..fast_supervision()
        },
    }));
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let reference = |manager: &ExtensionHostManager, engine: &HostAttachment, name: &str| {
        manager
            .commands_for_plugins(engine.plugin_view().as_ref())
            .into_iter()
            .find(|entry| entry.registration.name == name)
            .unwrap_or_else(|| panic!("{name} is not live"))
            .reference()
    };
    let pid = manager.host_pid();
    let slow = reference(&manager, &engine, "ext-slow");
    let started = Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        super::command::run(&manager.shared, &slow, "30000", None),
    )
    .await
    .expect("the deadline bounds the command")
    .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    // The same host takes the next command.
    let echo = reference(&manager, &engine, "ext-echo");
    assert_eq!(
        super::command::run(&manager.shared, &echo, "again", None).await,
        Ok(super::command::CommandOutcome::Show {
            text: "echo: again".to_string()
        })
    );
    assert_eq!(manager.host_pid(), pid);
    manager.shutdown().await;

    // Kill the host under a running command with a longer deadline.
    let manager = supervised_manager(&fixture, node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let pid = manager.host_pid();
    let slow = reference(&manager, &engine, "ext-slow");
    let echo = reference(&manager, &engine, "ext-echo");
    let manager2 = Arc::clone(&manager);
    let running = {
        let slow = slow.clone();
        tokio::spawn(
            async move { super::command::run(&manager2.shared, &slow, "30000", None).await },
        )
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.unwrap().to_string()])
        .status()
        .unwrap();
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.unwrap().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let error = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .expect("the call resolves")
        .unwrap()
        .unwrap_err();
    assert!(
        error.starts_with("extension host is down: exited:"),
        "{error}"
    );
    // The supervisor restarts the host and replays the plugin under a new
    // generation: the old reference is stale, a fresh one works.
    wait_host(&manager, || {
        manager.spawn_attempts() == 2 && manager.live_command_names().contains(&"ext-echo".into())
    })
    .await;
    let error = super::command::run(&manager.shared, &echo, "old", None)
        .await
        .unwrap_err();
    assert!(error.contains("no longer registered"), "{error}");
    let fresh = reference(&manager, &engine, "ext-echo");
    assert_ne!(fresh.generation, echo.generation);
    assert_eq!(
        super::command::run(&manager.shared, &fresh, "new", None).await,
        Ok(super::command::CommandOutcome::Show {
            text: "echo: new".to_string()
        })
    );
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// Trust tiers: a plugin host and a built-in (tier 0) host, never one process
// ---------------------------------------------------------------------------

use super::tier::{BuiltinModule, Tier0Tool};
use sha2::{Digest, Sha256};

fn tier_register_params(owner: &OwnerRef, name: &str) -> RegisterParams {
    RegisterParams {
        scope: None,
        owner: owner.clone(),
        kind: RegisterKind::Tool,
        spec: RegisterSpecWire {
            name: name.to_string(),
            description: "d".to_string(),
            input_schema: json!({"type": "object", "properties": {}})
                .as_object()
                .cloned(),
            argument_hint: None,
        },
    }
}

#[test]
fn owners_are_bound_to_their_tier() {
    let mut registry = OwnerRegistry::new();
    let plugin_id = "user/0123456789ab/demo";

    // A `host:` id on the plugin tier, and any other id on the builtin tier.
    let refused = registry
        .begin_owner(
            HostTier::Plugin,
            "host:mcp",
            "mcp",
            Some(fake_authority("host:mcp")),
            "h",
        )
        .unwrap_err();
    assert!(
        refused.contains("a plugin id can never use it"),
        "{refused}"
    );
    let refused = registry
        .begin_owner(HostTier::Builtin, plugin_id, "demo", None, "h")
        .unwrap_err();
    assert!(
        refused.contains("not a built-in host module id"),
        "{refused}"
    );
    let refused = registry
        .begin_owner(HostTier::Builtin, "demo", "demo", None, "h")
        .unwrap_err();
    assert!(
        refused.contains("not a built-in host module id"),
        "{refused}"
    );
    // The authority must fit the tier too.
    assert!(
        registry
            .begin_owner(HostTier::Plugin, plugin_id, "demo", None, "h")
            .unwrap_err()
            .contains("needs its reviewed plugin authority")
    );
    assert!(
        registry
            .begin_owner(
                HostTier::Builtin,
                "host:mcp",
                "mcp",
                Some(fake_authority("host:mcp")),
                "h"
            )
            .unwrap_err()
            .contains("has no plugin authority")
    );
    assert!(
        registry.owners().next().is_none(),
        "a refused owner leaves nothing behind"
    );

    let plugin = registry
        .begin_owner(
            HostTier::Plugin,
            plugin_id,
            "demo",
            Some(fake_authority(plugin_id)),
            "hash-demo",
        )
        .unwrap();
    let other_plugin = registry
        .begin_owner(
            HostTier::Plugin,
            "user/0123456789ab/other",
            "other",
            Some(fake_authority("user/0123456789ab/other")),
            "hash-other",
        )
        .unwrap();
    let builtin = registry
        .begin_owner(HostTier::Builtin, "host:mcp", "mcp", None, "digest-mcp")
        .unwrap();
    for owner in [&plugin, &other_plugin, &builtin] {
        registry.mark_active(owner);
    }
    assert_eq!(registry.owner("host:mcp").unwrap().tier, HostTier::Builtin);
    assert_eq!(registry.owner(plugin_id).unwrap().tier, HostTier::Plugin);
    assert_eq!(registry.tier_of(&builtin), Some(HostTier::Builtin));
    assert!(registry.authority_for(&builtin).is_none());
    assert!(registry.authority_for(&plugin).is_some());
    // Plugins share a process with each other; a built-in module does not
    // share one with any plugin.
    assert_eq!(registry.other_active_owners(plugin_id), 1);
    assert_eq!(registry.other_active_owners("host:mcp"), 0);

    let plugin_tool = register(&mut registry, &plugin, "zz_plugin_probe").unwrap();
    let builtin_tool = register(&mut registry, &builtin, "zz_builtin_probe").unwrap();
    let tiers: Vec<_> = registry
        .live_tools()
        .into_iter()
        .map(|tool| (tool.name, tool.tier))
        .collect();
    assert_eq!(
        tiers,
        [
            ("zz_plugin_probe".to_string(), HostTier::Plugin),
            ("zz_builtin_probe".to_string(), HostTier::Builtin)
        ]
    );
    assert!(registry.is_live(plugin_tool, &plugin));
    assert!(registry.is_live(builtin_tool, &builtin));

    // Each tier's host is its own process: a crash of one takes only its own
    // owners and registrations with it.
    registry.host_exited(HostTier::Plugin, "plugin host crashed");
    assert!(registry.owner(plugin_id).is_none());
    assert_eq!(
        registry.owner("host:mcp").unwrap().state,
        OwnerState::Active
    );
    assert!(registry.is_live(builtin_tool, &builtin));
    assert!(!registry.is_live(plugin_tool, &plugin));
    // ... and a shutdown of the builtin tier leaves the plugin tier's alone.
    let replay = registry
        .begin_owner(
            HostTier::Plugin,
            plugin_id,
            "demo",
            Some(fake_authority(plugin_id)),
            "hash-demo",
        )
        .unwrap();
    registry.mark_active(&replay);
    let again = register(&mut registry, &replay, "zz_plugin_probe").unwrap();
    registry.revoke_all(HostTier::Builtin, "builtin host shut down");
    assert!(registry.is_live(again, &replay));
    assert!(matches!(
        registry.owner("host:mcp").unwrap().state,
        OwnerState::Failed(_)
    ));
    assert_eq!(registry.live_tools().len(), 1);
}

/// A host answers for its own tier's owners only, even if it somehow named
/// the other tier's: the registration, and the log line, are dropped.
#[test]
fn a_host_registers_and_logs_only_for_owners_of_its_own_tier() {
    use super::supervisor::HostEvents;
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    let (plugin, builtin) = {
        let mut registry = manager.shared.registry.lock().unwrap();
        let plugin = registry
            .begin_owner(
                HostTier::Plugin,
                "user/0123456789ab/demo",
                "demo",
                Some(fake_authority("user/0123456789ab/demo")),
                "hash",
            )
            .unwrap();
        let builtin = registry
            .begin_owner(HostTier::Builtin, "host:mcp", "mcp", None, "digest")
            .unwrap();
        registry.mark_active(&plugin);
        registry.mark_active(&builtin);
        (plugin, builtin)
    };
    let events = |tier| super::Events {
        shared: Arc::downgrade(&manager.shared),
        tier,
        generation: 0,
    };
    let refused = events(HostTier::Plugin).register(&tier_register_params(&builtin, "zz_probe"));
    assert!(
        matches!(&refused, protocol::RegisterResult::Refused { refused }
            if refused.contains("belongs to the builtin tier")),
        "{refused:?}"
    );
    let refused = events(HostTier::Builtin).register(&tier_register_params(&plugin, "zz_probe"));
    assert!(
        matches!(&refused, protocol::RegisterResult::Refused { refused }
            if refused.contains("belongs to the plugin tier")),
        "{refused:?}"
    );
    assert!(manager.live_tool_names().is_empty());
    assert!(matches!(
        events(HostTier::Builtin).register(&tier_register_params(&builtin, "zz_probe")),
        protocol::RegisterResult::Admitted { .. }
    ));
    assert!(matches!(
        events(HostTier::Plugin).register(&tier_register_params(&plugin, "zz_other")),
        protocol::RegisterResult::Admitted { .. }
    ));

    let warn = |plugin_id: &str| protocol::LogParams {
        level: "warn".into(),
        msg: "m".into(),
        plugin_id: Some(plugin_id.into()),
    };
    events(HostTier::Plugin).log(&warn("host:mcp"));
    events(HostTier::Builtin).log(&warn("user/0123456789ab/demo"));
    assert!(
        manager
            .owner_report("host:mcp")
            .unwrap()
            .diagnostics
            .is_empty()
    );
    assert!(
        manager
            .owner_report("user/0123456789ab/demo")
            .unwrap()
            .diagnostics
            .is_empty()
    );
    events(HostTier::Builtin).log(&warn("host:mcp"));
    assert_eq!(
        manager.owner_report("host:mcp").unwrap().diagnostics,
        ["warn: m"]
    );
}

/// A manifest name cannot carry a `host:` prefix (names are lower-case ASCII
/// letters, digits and internal `-`/`.`), so discovery never builds a `host:`
/// id, and the plugin that tries fails validation instead of reaching the
/// host.
#[test]
fn a_plugin_named_like_a_tier_zero_owner_fails_validation_and_never_reaches_the_host() {
    let _policy = TestPolicyGuard::extension_host(true);
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user");
    native_bundle(&user, "evil", "index.mjs", &["index.mjs"]);
    native_bundle(&user, "fine", "index.mjs", &["index.mjs"]);
    // The directory is harmless; the manifest's name is the claim.
    let manifest = user.join("evil/plugin.json");
    let rewritten = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("\"name\": \"evil\"", "\"name\": \"host:evil\"");
    assert!(rewritten.contains("host:evil"));
    std::fs::write(&manifest, rewritten).unwrap();
    let config = DiscoveryConfig {
        workspace: temp.path().join("project"),
        user_plugins_dir: user,
        workspace_plugins_dir: temp.path().join("project/.codewhale/plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: temp.path().join("state/plugin-state.json"),
    };
    let registry = discover_with_config(&config);
    assert!(
        registry.get("host:evil").is_none(),
        "a plugin named `host:evil` must not be discovered as a plugin"
    );
    assert!(
        registry
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("host:evil")
                && diagnostic.message.contains("name")),
        "{:?}",
        registry.diagnostics()
    );
    assert!(registry.get("fine").is_some());
    // Whatever discovery yields, no desired owner is a tier-0 id.
    let (desired, _) = super::desired_owners(&registry);
    assert!(
        desired
            .keys()
            .all(|id| HostTier::Plugin.check_owner_id(id).is_ok())
    );
}

#[test]
fn launch_plans_carry_their_tier_and_use_a_data_directory_each() {
    #[cfg(not(windows))]
    use codewhale_dependencies::{HostRuntime, HostRuntimeKind};
    let home = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let bundle = super::materialize_bundle(home.path()).unwrap();
    #[cfg(not(windows))]
    let bundle = home.path().join("codewhale-extension-host.mjs");
    // Windows admission verifies the real copied runtime and exact bundle.
    #[cfg(windows)]
    let runtime = codewhale_dependencies::resolve_extension_host_runtime(
        NODE,
        None,
        None,
        crate::extension_host::bundle_sha256(),
    )
    .selected
    .expect("tier planning requires the test Node runtime on Windows");
    #[cfg(not(windows))]
    let runtime = HostRuntime {
        compiled: false,
        kind: HostRuntimeKind::Node,
        path: PathBuf::from("/opt/runtime/bin/node"),
        version: (22, 19, 0),
        native_code_flags: Vec::new(),
    };
    let mut dirs = Vec::new();
    for tier in HostTier::ALL {
        let launch = match super::supervisor::plan_launch(
            tier,
            &runtime,
            &bundle,
            home.path(),
            1 << 30,
            &[],
        ) {
            Ok(launch) => launch,
            Err(error) if tier == HostTier::Plugin => {
                assert!(
                    error.contains("Native extensions require a verified OS sandbox"),
                    "{error}"
                );
                assert!(matches!(
                    super::supervisor::planned_sandbox(tier, &runtime, home.path()),
                    Ok(super::supervisor::HostSandbox::Unsandboxed(_))
                ));
                let data = super::supervisor::tier_data_dir(home.path(), tier);
                assert!(data.is_dir());
                assert!(super::supervisor::tier_data_dir(home.path(), HostTier::Builtin).is_dir());
                dirs.push(data);
                continue;
            }
            Err(error) => panic!("pinned Builtin plan failed: {error}"),
        };
        assert_eq!(launch.tier, tier);
        // The runtime's own argv ends `<bundle> --tier=<tier>`, wrapped by the
        // OS sandbox or not.
        let at = launch
            .args
            .iter()
            .position(|arg| Path::new(arg) == bundle)
            .expect("bundle in argv");
        assert_eq!(launch.args[at + 1], format!("--tier={}", tier.name()));
        assert_eq!(launch.args.len(), at + 2, "the tier is the last argument");
        assert_eq!(
            launch.cwd,
            super::supervisor::tier_data_dir(home.path(), tier)
        );
        assert!(launch.cwd.is_dir(), "{}", launch.cwd.display());
        dirs.push(launch.cwd);
    }
    assert_ne!(dirs[0], dirs[1], "each tier has its own data directory");
    assert_eq!(HostTier::Plugin.argv_flag(), "--tier=plugin");
    assert_eq!(HostTier::Builtin.argv_flag(), "--tier=builtin");
}

/// A built-in module table that pins `source`, for a test. Production has
/// none: `BUILTIN_MODULES` is empty.
fn test_builtin_table(source: &Path) -> &'static [BuiltinModule] {
    let digest = super::hex(Sha256::digest(std::fs::read(source).unwrap()));
    let tools: &'static [Tier0Tool] = Box::leak(Box::new([Tier0Tool {
        name: "zz_tier0_listed",
        approval: ApprovalRequirement::Auto,
    }]));
    Box::leak(Box::new([BuiltinModule {
        id: "tier0-module",
        source_sha256: Box::leak(digest.into_boxed_str()),
        tools,
    }]))
}

/// Put `bytes` where the core looks for built-in module `id`'s source under
/// the home `root`.
fn place_builtin_source(root: &Path, id: &str, bytes: &[u8]) {
    let path = super::supervisor::bundle_dir(root, super::bundle_sha256())
        .join("builtin")
        .join(format!("{id}.mjs"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The production table pins MCP, but ordinary plugin attachment never
/// starts that independent builtin backend until the Engine selects it.
#[tokio::test]
async fn plugin_attachment_never_spawns_the_independent_builtin_mcp_backend() {
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    assert_eq!(
        manager.shared.builtin_modules.len(),
        super::tier::BUILTIN_MODULES.len()
    );
    assert!(
        manager
            .shared
            .builtin_modules
            .iter()
            .any(|module| module.id == "mcp")
    );
    let Some(node) =
        node_for_tests("plugin_attachment_never_spawns_the_independent_builtin_mcp_backend")
    else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["dsh-workspace-deps"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    assert!(matches!(manager.status(), HostStatus::Ready { .. }));
    assert_eq!(manager.tier_spawn_attempts(HostTier::Plugin), 1);
    assert_eq!(manager.tier_spawn_attempts(HostTier::Builtin), 0);
    assert_eq!(manager.tier_status(HostTier::Builtin), HostStatus::Idle);
    // Planning creates an empty sibling so the Native sandbox can mask it,
    // including on Linux where bubblewrap requires the denied root to exist.
    // That directory is not evidence of a Builtin process or backend.
    let builtin_data = super::supervisor::tier_data_dir(&fixture.root, HostTier::Builtin);
    assert!(builtin_data.is_dir());
    assert_eq!(std::fs::read_dir(builtin_data).unwrap().count(), 0);
    let report = super::render_status(&manager);
    assert!(!report.contains("built-in host"), "{report}");
    manager.shutdown().await;
}

/// A module whose source is not the digest the table pins, or is missing, is
/// refused with the reason, and no host is started for it.
#[tokio::test]
async fn a_built_in_module_that_is_not_the_pinned_source_is_refused_and_starts_nothing() {
    let _policy = TestPolicyGuard::extension_host(true);
    let source = fixtures_dir().join("tier0-module/module.mjs");
    let modules = test_builtin_table(&source);
    for (case, bytes, expected) in [
        (
            "tampered",
            Some(&b"export const name = 'tier0-module'\nexport function apply() {}\n"[..]),
            "the core pins",
        ),
        ("missing", None, "cannot read the source"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("home");
        if let Some(bytes) = bytes {
            place_builtin_source(&root, "tier0-module", bytes);
        }
        let manager = Arc::new(ExtensionHostManager::with_builtin_modules(
            ExtensionHostOptions {
                root: Some(root),
                ..Default::default()
            },
            modules,
        ));
        let engine = manager.attach(Arc::new(PluginRegistry::empty(temp.path())));
        engine.sync().await.unwrap();
        assert_eq!(manager.spawn_attempts(), 0, "{case}");
        assert_eq!(manager.tier_status(HostTier::Builtin), HostStatus::Idle);
        let Some(OwnerState::Failed(reason)) = manager.owner_state("host:tier0-module") else {
            panic!("{case}: {:?}", manager.owner_state("host:tier0-module"));
        };
        assert!(reason.contains(expected), "{case}: {reason}");
        assert!(manager.live_tool_names().is_empty());
        // Not retried every turn.
        engine.sync().await.unwrap();
        assert_eq!(
            manager.diagnostics().len(),
            1,
            "{:?}",
            manager.diagnostics()
        );
    }
}

/// The two tiers are two processes under one manager: a built-in module
/// activates under `host:<module>` in its own host, its tool's approval is
/// whatever the table says (and `Required` where it says nothing), plugin
/// tools stay `Required`, and crashing the plugin host disturbs nothing of
/// the builtin one.
#[tokio::test]
async fn a_tier_zero_host_runs_apart_from_the_plugin_host_and_its_tool_approval_follows_the_table()
{
    let Some(node) = node_for_tests("a_tier_zero_host_runs_apart") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["dsh-workspace-deps"]).await;
    let source = fixtures_dir().join("tier0-module/module.mjs");
    place_builtin_source(
        &fixture.root,
        "tier0-module",
        &std::fs::read(&source).unwrap(),
    );
    let manager = Arc::new(ExtensionHostManager::with_builtin_modules(
        ExtensionHostOptions {
            runtime: NODE,
            node_override: Some(node),
            root: Some(fixture.root.clone()),
            ..Default::default()
        },
        test_builtin_table(&source),
    ));
    assert_eq!(manager.tier_status(HostTier::Builtin), HostStatus::Idle);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();

    // Two hosts, two processes, each launched once.
    let plugin_pid = manager.host_pid().expect("plugin host running");
    let builtin_pid = manager
        .shared
        .ready_host(HostTier::Builtin)
        .expect("builtin host running")
        .pid
        .expect("builtin pid");
    assert_ne!(plugin_pid, builtin_pid);
    assert_eq!(manager.tier_spawn_attempts(HostTier::Plugin), 1);
    assert_eq!(manager.tier_spawn_attempts(HostTier::Builtin), 1);
    let plugin_host_id = plugin_id(&fixture, "dsh-workspace-deps");
    assert_eq!(
        manager.owner_state("host:tier0-module"),
        Some(OwnerState::Active)
    );
    assert_eq!(
        manager.owner_state(&plugin_host_id),
        Some(OwnerState::Active)
    );
    {
        let registry = manager.shared.registry.lock().unwrap();
        assert_eq!(
            registry.owner("host:tier0-module").unwrap().tier,
            HostTier::Builtin
        );
        assert_eq!(
            registry.owner(&plugin_host_id).unwrap().tier,
            HostTier::Plugin
        );
        assert_eq!(
            registry.owner("host:tier0-module").unwrap().content_hash,
            test_builtin_table(&source)[0].source_sha256
        );
    }
    // The module's data directory is under the builtin tier's, apart from
    // every plugin's.
    assert!(
        super::supervisor::owner_data_dir(
            &fixture.root,
            HostTier::Builtin,
            "host:tier0-module",
            "tier0-module"
        )
        .is_dir()
    );

    // An engine installs plugin tools only; tier-0 tools are not offered to
    // the model by a plugin snapshot.
    let installed = installed(&engine, fixture.workspace());
    assert_eq!(installed, ["load_workspace_dependencies"]);

    // Approval follows the table for tier 0, and stays Required for plugins.
    let registrations = manager.shared.registry.lock().unwrap().live_tools();
    let spec_for = |name: &str| {
        let registration = registrations
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} not registered"))
            .clone();
        super::tool::HostToolSpec::new(registration, Arc::clone(&manager.shared))
    };
    let listed = spec_for("zz_tier0_listed");
    let unlisted = spec_for("zz_tier0_unlisted");
    let plugin_tool = spec_for("load_workspace_dependencies");
    assert_eq!(listed.registration_origin(), "host:tier0-module");
    assert_eq!(listed.approval_requirement(), ApprovalRequirement::Auto);
    assert_eq!(
        listed.approval_requirement_for(&json!({})),
        ApprovalRequirement::Auto
    );
    assert_eq!(
        unlisted.approval_requirement(),
        ApprovalRequirement::Required
    );
    assert_eq!(
        plugin_tool.approval_requirement(),
        ApprovalRequirement::Required
    );
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    assert_eq!(
        listed.prepare(json!({}), &context).unwrap().approval,
        ApprovalRequirement::Auto
    );
    assert_eq!(
        unlisted.prepare(json!({}), &context).unwrap().approval,
        ApprovalRequirement::Required
    );
    // Even an Auto tool is host code: never read-only, never plan-mode safe.
    assert!(!listed.is_read_only_for(&json!({})));

    // The call goes to the builtin host and comes back.
    let result = listed.execute(json!({}), &context).await.unwrap();
    assert!(result.success);
    assert_eq!(
        serde_json::from_str::<Value>(&result.content).unwrap(),
        json!({"tier": "zero", "listed": true})
    );

    // Kill the plugin host: its owners go and come back, the builtin host and
    // its module are untouched, and its crash budget is not spent.
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .args(["-9", &plugin_pid.to_string()])
        .status()
        .unwrap();
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &plugin_pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    wait_host(&manager, || {
        manager.tier_spawn_attempts(HostTier::Plugin) == 2
            && manager
                .live_tool_names()
                .contains(&"load_workspace_dependencies".to_string())
    })
    .await;
    assert_eq!(
        manager.shared.ready_host(HostTier::Builtin).unwrap().pid,
        Some(builtin_pid)
    );
    assert_eq!(manager.tier_spawn_attempts(HostTier::Builtin), 1);
    assert_eq!(
        manager.owner_state("host:tier0-module"),
        Some(OwnerState::Active)
    );
    assert!(
        manager
            .shared
            .builtin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .is_empty()
    );
    assert_eq!(
        manager
            .shared
            .plugin
            .supervision
            .lock()
            .unwrap()
            .crashes
            .len(),
        1
    );
    assert!(listed.execute(json!({}), &context).await.unwrap().success);

    // The status page shows the builtin host and keeps its tools off the
    // plugin list.
    let report = super::render_status(&manager);
    assert!(
        report.contains("built-in host (tier 0): running"),
        "{report}"
    );
    assert!(!report.contains("tool zz_tier0_listed"), "{report}");
    assert!(!report.contains("command /"), "{report}");
    manager.shutdown().await;
}

/// The existing installer, registry projection and final spec invocation use
/// one attachment receipt. Two entry scopes under one owner never union into
/// either caller, and withdrawing one does not cancel its sibling.
#[tokio::test(flavor = "current_thread")]
async fn native_preset_membership_filters_discovery_and_final_tool_invocation() {
    let Some(node) = node_for_tests("native_preset_membership") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let _catalog = stub_builtin_commands();
    let fixture = FixturePlugins::new(&["two-entries"]).await;
    let manager = fixture.manager(node);
    let _manager = super::TestManagerGuard::install(Arc::clone(&manager));
    let initial = manager.attach(fixture.registry());
    initial.sync().await.unwrap();
    let presets = super::native_presets_for_plugins(initial.plugin_view().as_ref());
    let first = presets
        .iter()
        .find(|(preset, _)| preset.entry.path.ends_with("tools.mjs"))
        .unwrap()
        .0
        .clone();
    let second = presets
        .iter()
        .find(|(preset, _)| preset.entry.path.ends_with("commands.mjs"))
        .unwrap()
        .0
        .clone();
    let a = manager.attach(Arc::new(
        fixture.registry().with_native_preset(first).unwrap(),
    ));
    let b = manager.attach(Arc::new(
        fixture.registry().with_native_preset(second).unwrap(),
    ));
    drop(initial);
    a.sync().await.unwrap();
    assert_eq!(installed(&a, fixture.workspace()), ["two_first"]);
    assert_eq!(installed(&b, fixture.workspace()), ["two_second"]);
    assert!(
        manager
            .commands_for_plugins(a.plugin_view().as_ref())
            .is_empty()
    );
    assert_eq!(
        manager.commands_for_plugins(b.plugin_view().as_ref())[0]
            .registration
            .name,
        "two-hello"
    );
    let old = host_tool(&a, fixture.workspace(), "two_first");
    let a_context = ToolContext::new(fixture.workspace()).with_plugin_registry(a.plugin_view());
    let b_context = ToolContext::new(fixture.workspace()).with_plugin_registry(b.plugin_view());
    assert!(
        old.prepare(json!({}), &b_context).is_err(),
        "a retained spec cannot execute under another caller"
    );
    assert_eq!(
        old.execute(json!({}), &a_context).await.unwrap().content,
        "first"
    );
    a.set_plugins(Arc::new(PluginRegistry::empty(fixture.workspace())));
    assert!(
        old.prepare(json!({}), &a_context).is_err(),
        "withdrawal rejects a retained approval/spec receipt before reconciliation"
    );
    a.sync().await.unwrap();
    assert_eq!(
        host_tool(&b, fixture.workspace(), "two_second")
            .execute(json!({}), &b_context)
            .await
            .unwrap()
            .content,
        "second"
    );
    // Rediscovery must preserve a now-invalid narrowed selector. A disabled
    // build never turns a selected caller into a broad default caller.
    manager.refresh_workspace(&fixture.disable("two-entries"));
    assert!(!b.plugin_view().selected_native_entries().is_empty());
    b.sync().await.unwrap();
    assert!(installed(&b, fixture.workspace()).is_empty());
    manager.shutdown().await;
}

/// A real installed raw roster: initial default and two child snapshots share
/// one owner but all five discovery paths and final invocation use one receipt.
#[tokio::test(flavor = "current_thread")]
async fn raw_agent_presets_use_one_default_and_all_five_caller_views() {
    let Some(node) = node_for_tests("raw_agent_presets_use_one_default_and_all_five_caller_views")
    else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let _catalog = stub_builtin_commands();
    let fixture = FixturePlugins::new(&["raw-agent-presets"]).await;
    let original = fixture.registry();
    assert_eq!(original.selected_native_entries().len(), 1);
    assert!(
        Path::new(&original.selected_native_entries()[0].entry.path).file_name()
            == Some(std::ffi::OsStr::new("a.mjs"))
    );
    let manager = fixture.manager(node);
    let _manager = super::TestManagerGuard::install(Arc::clone(&manager));
    let a = manager.attach(Arc::clone(&original));
    a.sync().await.unwrap();
    let roster = super::native_presets_for_plugins(a.plugin_view().as_ref());
    assert_eq!(
        roster.len(),
        2,
        "roster discovery offers admitted alternatives without activating a union"
    );
    assert_eq!(a.prompt_sections().await.unwrap()[0].text, "A:a");
    let selected_b = roster
        .iter()
        .find(|(preset, _)| {
            Path::new(&preset.entry.path).file_name() == Some(std::ffi::OsStr::new("b.mjs"))
        })
        .unwrap()
        .0
        .clone();
    let b = manager.attach(Arc::new(original.with_native_preset(selected_b).unwrap()));
    b.sync().await.unwrap();
    let check = |view: &HostAttachment, expected: &str| {
        assert_eq!(installed(view, fixture.workspace()), ["preset_echo"]);
        let commands = manager.commands_for_plugins(view.plugin_view().as_ref());
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].registration.name, "preset-echo");
        let roots = super::skills::roots_for_plugins(view.plugin_view().as_ref());
        assert_eq!(roots.len(), 1);
        assert!(roots[0].0.path.ends_with(expected));
        assert_eq!(roots[0].0.snapshots[0].name, "preset-note");
    };
    check(&a, "a");
    check(&b, "b");
    assert_eq!(a.prompt_sections().await.unwrap()[0].text, "A:a");
    assert_eq!(b.prompt_sections().await.unwrap()[0].text, "B:b");
    let hook_payload = protocol::HookCallPayload {
        name: "read".into(),
        call_id: "raw-preset-hook".into(),
        input: json!({}),
        mode: "Agent".into(),
        workspace: fixture.workspace().to_string_lossy().into_owned(),
        model: "fixture".into(),
    };
    let hooks_a = a.tool_before_hooks(hook_payload.clone()).await;
    let hooks_b = b.tool_before_hooks(hook_payload).await;
    assert_eq!(hooks_a.len(), 1);
    assert_eq!(hooks_b.len(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&hooks_a[0].stdout).unwrap()["additionalContext"],
        "A:a"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&hooks_b[0].stdout).unwrap()["additionalContext"],
        "B:b"
    );
    let a_context = ToolContext::new(fixture.workspace()).with_plugin_registry(a.plugin_view());
    let b_context = ToolContext::new(fixture.workspace()).with_plugin_registry(b.plugin_view());
    let retained = host_tool(&a, fixture.workspace(), "preset_echo");
    assert!(retained.prepare(json!({}), &b_context).is_err());
    assert_eq!(
        retained
            .execute(json!({}), &a_context)
            .await
            .unwrap()
            .content,
        "A:a"
    );
    let command = manager.commands_for_plugins(a.plugin_view().as_ref())[0].reference();
    assert!(
        super::run_command_for_plugins(&command, "", None, b.plugin_view().as_ref())
            .await
            .is_err()
    );
    assert_eq!(
        super::run_command_for_plugins(&command, "", None, a.plugin_view().as_ref())
            .await
            .unwrap(),
        super::command::CommandOutcome::Show { text: "A:a".into() }
    );
    a.set_plugins(Arc::new(PluginRegistry::empty(fixture.workspace())));
    assert!(retained.prepare(json!({}), &a_context).is_err());
    a.sync().await.unwrap();
    assert!(a.prompt_sections().await.unwrap().is_empty());
    assert_eq!(
        host_tool(&b, fixture.workspace(), "preset_echo")
            .execute(json!({}), &b_context)
            .await
            .unwrap()
            .content,
        "B:b"
    );
    let disabled = fixture.disable("raw-agent-presets");
    manager.refresh_workspace(&disabled);
    assert!(!b.plugin_view().selected_native_entries().is_empty());
    b.sync().await.unwrap();
    assert!(installed(&b, fixture.workspace()).is_empty());
    assert!(b.prompt_sections().await.unwrap().is_empty());
    assert!(super::skills::roots_for_plugins(b.plugin_view().as_ref()).is_empty());
    assert!(
        manager
            .commands_for_plugins(b.plugin_view().as_ref())
            .is_empty()
    );
    assert!(
        b.tool_before_hooks(protocol::HookCallPayload {
            name: "read".into(),
            call_id: "withdrawn".into(),
            input: json!({}),
            mode: "Agent".into(),
            workspace: fixture.workspace().to_string_lossy().into_owned(),
            model: "fixture".into()
        })
        .await
        .is_empty()
    );
    manager.shutdown().await;
}

/// No default is an upstream fact. Catalog discovery must neither start the
/// host nor grant any contribution until the exact child receipt is selected.
#[tokio::test(flavor = "current_thread")]
async fn raw_agent_presets_without_default_require_explicit_child_selection() {
    let Some(node) =
        node_for_tests("raw_agent_presets_without_default_require_explicit_child_selection")
    else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let _catalog = stub_builtin_commands();
    let fixture = FixturePlugins::new(&["raw-agent-presets-no-default"]).await;
    let original = fixture.registry();
    assert!(original.selected_native_entries().is_empty());
    let manager = fixture.manager(node);
    let _manager = super::TestManagerGuard::install(Arc::clone(&manager));
    let roster = super::native_presets_for_plugins(original.as_ref());
    assert_eq!(
        roster.len(),
        2,
        "healthy admitted catalog is available before a host exists"
    );
    let idle = manager.attach(Arc::clone(&original));
    idle.sync().await.unwrap();
    assert!(installed(&idle, fixture.workspace()).is_empty());
    assert!(idle.prompt_sections().await.unwrap().is_empty());
    assert!(
        manager
            .commands_for_plugins(idle.plugin_view().as_ref())
            .is_empty()
    );
    assert!(super::skills::roots_for_plugins(idle.plugin_view().as_ref()).is_empty());
    assert!(
        idle.tool_before_hooks(protocol::HookCallPayload {
            name: "read".into(),
            call_id: "unselected".into(),
            input: json!({}),
            mode: "Agent".into(),
            workspace: fixture.workspace().to_string_lossy().into_owned(),
            model: "fixture".into()
        })
        .await
        .is_empty()
    );
    let selected = roster
        .iter()
        .find(|(preset, _)| {
            Path::new(&preset.entry.path).file_name() == Some(std::ffi::OsStr::new("b.mjs"))
        })
        .unwrap()
        .0
        .clone();
    let child = manager.attach(Arc::new(original.with_native_preset(selected).unwrap()));
    child.sync().await.unwrap();
    assert_eq!(installed(&child, fixture.workspace()), ["preset_echo"]);
    assert_eq!(child.prompt_sections().await.unwrap()[0].text, "B:b");
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(child.plugin_view());
    assert_eq!(
        host_tool(&child, fixture.workspace(), "preset_echo")
            .execute(json!({}), &context)
            .await
            .unwrap()
            .content,
        "B:b"
    );
    manager.refresh_workspace(&original);
    idle.sync().await.unwrap();
    assert!(idle.plugin_view().selected_native_entries().is_empty());
    assert!(
        installed(&idle, fixture.workspace()).is_empty(),
        "another caller's selected owner cannot broaden the catalog-only caller"
    );
    assert!(idle.prompt_sections().await.unwrap().is_empty());
    assert_eq!(
        super::native_presets_for_plugins(idle.plugin_view().as_ref()).len(),
        2
    );
    manager.shutdown().await;
}

/// The real configured-hook consumers redeem the pinned Builtin across every
/// existing firepoint, and a changed project receipt cannot fall back.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn all_fifteen_project_hook_events_use_the_pinned_runner_and_reject_changed_receipts() {
    let _env = crate::test_support::lock_test_env();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("all_fifteen_project_hook_events") else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".codewhale")).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let _config = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_CONFIG_PATH",
        home.path().join("config.toml"),
    );
    let mut config = codewhale_config::hooks::HooksConfig {
        enabled: true,
        ..codewhale_config::hooks::HooksConfig::default()
    };
    for event in codewhale_config::hooks::ALL_HOOK_EVENTS {
        config.hooks.push(codewhale_config::hooks::Hook::new(
            event,
            &format!(
                "printf '%s|%s|%s' '{}' \"$DEEPSEEK_SESSION_ID\" \"$DEEPSEEK_TOOL_CALL_ID\"",
                event.as_str()
            ),
        ));
    }
    let hook_path = workspace.join(".codewhale/hooks.toml");
    std::fs::write(&hook_path, toml::to_string(&config).unwrap()).unwrap();
    codewhale_runtime::config::save_workspace_trust(&workspace).unwrap();
    let (reviewed, _) = crate::hooks::authority::review_project_hooks(&workspace).unwrap();
    crate::hooks::authority::approve_project_hooks(&workspace, &reviewed.digest).unwrap();
    let admitted = crate::hooks::config::load_with_project(
        codewhale_config::hooks::HooksConfig {
            enabled: true,
            ..codewhale_config::hooks::HooksConfig::default()
        },
        &workspace,
    );
    assert_eq!(admitted.hooks.len(), 15);
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        node_override: Some(node),
        root: Some(home.path().join("host")),
        ..ExtensionHostOptions::default()
    }));
    manager.bind_engine_handle(tokio::runtime::Handle::current());
    let _manager = super::TestManagerGuard::install(Arc::clone(&manager));
    let executor = crate::hooks::HookExecutor::new(admitted, workspace.clone());
    let context = crate::hooks::HookContext::new()
        .with_session_id("actual-session")
        .with_tool_call_id("actual-call")
        .with_caller(crate::hooks::HookCaller {
            workspace,
            plugins: None,
            session_id: Some("actual-session".into()),
            agent_id: Some("actual-agent".into()),
            origin_turn_id: Some("actual-turn".into()),
            origin_call_id: Some("actual-call".into()),
        });
    let env_scope = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        let _env = crate::test_support::join_env_scope(env_scope);
        let _policy = crate::plugins::activation::PolicyScope::propagate(true);
        for event in codewhale_config::hooks::ALL_HOOK_EVENTS {
            let results = executor.execute(event, &context);
            assert_eq!(results.len(), 1, "{event:?}");
            assert!(results[0].success, "{event:?}: {:?}", results[0]);
            assert_eq!(
                results[0].stdout,
                format!("{}|actual-session|actual-call", event.as_str()),
                "{event:?}"
            );
        }
        std::fs::write(hook_path, "# changed after admission\n").unwrap();
        let rejected = executor.execute(codewhale_config::hooks::HookEvent::SessionStart, &context);
        assert_eq!(rejected.len(), 1);
        assert!(!rejected[0].success);
        assert!(rejected[0].stdout.is_empty());
        assert!(rejected[0].exit_code.is_none());
    })
    .await
    .unwrap();
    assert!(matches!(
        manager.tier_status(HostTier::Builtin),
        HostStatus::Ready { .. }
    ));
    assert!(!matches!(manager.status(), HostStatus::Ready { .. }));
    manager.shutdown().await;
}
