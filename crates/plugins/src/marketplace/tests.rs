//! Marketplace parser tests. Every fixture is a real published schema:
//! the Kimi fixture is `MoonshotAI/kimi-code/plugins/marketplace.json`
//! verbatim; the Kimi custom-marketplace, Claude, and Codex fixtures are
//! the documented examples from each format's official docs, verbatim.
//! No synthetic formats, no invented fields.

use serde_json::Value;

use super::parsers::MarketplaceDocument;
use super::parsers::parse_catalog;
use super::types::{
    CatalogTier, MarketplaceCatalogId, MarketplaceFormat, MarketplaceInstallPlan,
    MarketplaceSourceSpec,
};

/// MoonshotAI/kimi-code `plugins/marketplace.json`, verbatim.
const KIMI_OFFICIAL_MARKETPLACE: &str = r#"{
  "version": "1",
  "plugins": [
    {
      "id": "kimi-datasource",
      "tier": "official",
      "displayName": "Kimi Datasource",
      "version": "3.3.0",
      "description": "Official datasource workflows.",
      "keywords": ["data", "mcp"],
      "source": "./official/kimi-datasource"
    },
    {
      "id": "kimi-webbridge",
      "tier": "official",
      "displayName": "Kimi WebBridge",
      "version": "1.11.3",
      "description": "Control your real browser from Kimi Code.",
      "keywords": ["browser", "automation", "webbridge"],
      "source": "./official/kimi-webbridge"
    },
    {
      "id": "superpowers",
      "tier": "curated",
      "displayName": "Superpowers",
      "description": "Planning, TDD, debugging, and delivery workflows for coding agents.",
      "homepage": "https://github.com/obra/superpowers",
      "keywords": ["skills", "planning", "tdd", "debugging", "code-review"],
      "source": "https://github.com/obra/superpowers"
    },
    {
      "id": "vercel-plugin",
      "tier": "curated",
      "displayName": "Vercel Plugin",
      "description": "Comprehensive Vercel ecosystem plugin — skills, agents, and conventions for the Vercel platform.",
      "homepage": "https://vercel.com/docs/agent-resources/vercel-plugin",
      "keywords": ["vercel", "deployment", "nextjs", "skills", "agents"],
      "source": "https://github.com/vercel/vercel-plugin"
    },
    {
      "id": "modern-web-guidance",
      "tier": "curated",
      "displayName": "Modern Web Guidance",
      "description": "Modern web platform expertise, best practices, and browser compatibility data for coding agents, from the Google Chrome team.",
      "homepage": "https://github.com/GoogleChrome/modern-web-guidance",
      "keywords": ["web", "css", "browser", "frontend", "skills"],
      "source": "https://github.com/GoogleChrome/modern-web-guidance"
    }
  ]
}"#;

/// Kimi docs "Custom marketplace JSON" example, verbatim.
const KIMI_DOCS_CUSTOM: &str = r#"{
  "version": "2",
  "plugins": [
    {
      "id": "my-plugin",
      "displayName": "My Plugin",
      "source": "./my-plugin"
    }
  ]
}"#;

/// Claude plugin-marketplaces docs example, verbatim.
const CLAUDE_DOCS_MARKETPLACE: &str = r#"{
  "name": "company-tools",
  "owner": {
    "name": "DevTools Team",
    "email": "devtools@example.com"
  },
  "plugins": [
    {
      "name": "code-formatter",
      "source": "./plugins/formatter",
      "description": "Automatic code formatting on save",
      "version": "2.1.0",
      "author": {
        "name": "DevTools Team"
      }
    },
    {
      "name": "deployment-tools",
      "source": {
        "source": "github",
        "repo": "company/deploy-plugin"
      },
      "description": "Deployment automation tools"
    }
  ]
}"#;

/// Codex plugin packaging docs marketplace example, verbatim.
const CODEX_DOCS_MARKETPLACE: &str = r#"{
  "name": "local-repo",
  "plugins": [
    {
      "name": "my-plugin",
      "source": {
        "source": "local",
        "path": "./plugins/my-plugin"
      },
      "policy": {
        "installation": "AVAILABLE",
        "authentication": "ON_INSTALL"
      },
      "category": "Productivity"
    }
  ]
}"#;

fn parse(id: &str, format: MarketplaceFormat, body: &str) -> super::types::MarketplaceCatalog {
    let root: Value = serde_json::from_str(body).expect("fixture parses as JSON");
    parse_catalog(MarketplaceDocument {
        catalog_id: MarketplaceCatalogId::new(id),
        format,
        root,
        base: None,
    })
}

fn parse_auto(id: &str, body: &str) -> super::types::MarketplaceCatalog {
    parse(id, MarketplaceFormat::Auto, body)
}

#[test]
fn native_catalog_keeps_identity_without_fetching_artwork_or_granting_trust() {
    let snapshot: Value =
        serde_json::from_str(include_str!("../../assets/first-party-marketplace.json")).unwrap();
    let icon = snapshot["catalog"]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "computer-use")
        .unwrap()["icon"]
        .as_str()
        .unwrap();
    let mut entry = serde_json::json!({"name":"computer-use","source":"github:codewhale-hq/codewhale-cu-plugin","display_name":"Computer Use","author":"Codewhale","icon":icon,"platforms":["macos","windows","linux"]});
    let catalog = parse(
        "identity",
        MarketplaceFormat::Codewhale,
        &serde_json::json!({"name":"identity","plugins":[entry.clone()]}).to_string(),
    );
    let candidate = catalog.candidate_by_name("computer-use").unwrap();
    assert_eq!(candidate.display_name.as_deref(), Some("Computer Use"));
    assert_eq!(candidate.author.as_deref(), Some("Codewhale"));
    assert_eq!(candidate.icon.as_deref(), Some(icon));
    assert_eq!(
        candidate.when.as_ref().unwrap().os.as_ref().unwrap().len(),
        3
    );
    assert_eq!(candidate.provenance.tier, CatalogTier::Community);
    assert!(!candidate.has_errors());
    entry["icon"] = serde_json::json!("https://publisher.example/tracker.png");
    let rejected = parse(
        "identity",
        MarketplaceFormat::Codewhale,
        &serde_json::json!({"name":"identity","plugins":[entry]}).to_string(),
    );
    let candidate = rejected.candidate_by_name("computer-use").unwrap();
    assert!(candidate.icon.is_none());
    assert!(candidate.has_errors());
}

#[test]
fn kimi_official_marketplace_parses_all_five_real_entries() {
    let catalog = parse_auto("kimi", KIMI_OFFICIAL_MARKETPLACE);
    assert_eq!(catalog.format, MarketplaceFormat::Kimi);
    assert_eq!(catalog.total_candidates(), 5);
    assert_eq!(catalog.error_count(), 0, "real catalog must parse cleanly");

    let datasource = catalog.candidate_by_name("kimi-datasource").unwrap();
    assert_eq!(datasource.provenance.tier, CatalogTier::Official);
    assert!(datasource.install_plan.is_supported());
    assert_eq!(
        datasource.source,
        MarketplaceSourceSpec::LocalPath {
            path: "./official/kimi-datasource".into()
        }
    );

    let superpowers = catalog.candidate_by_name("superpowers").unwrap();
    assert_eq!(superpowers.provenance.tier, CatalogTier::Curated);
    assert!(superpowers.install_plan.is_supported());
    assert!(matches!(
        &superpowers.source,
        MarketplaceSourceSpec::GitHub { owner, repo, .. }
            if owner == "obra" && repo == "superpowers"
    ));
}

#[test]
fn kimi_docs_custom_marketplace_parses() {
    let catalog = parse_auto("kimi-custom", KIMI_DOCS_CUSTOM);
    assert_eq!(catalog.format, MarketplaceFormat::Kimi);
    assert_eq!(catalog.version.as_deref(), Some("2"));
    let plugin = catalog.candidate_by_name("my-plugin").unwrap();
    assert_eq!(
        plugin.display_name.as_deref(),
        Some("My Plugin"),
        "camelCase displayName is the documented field"
    );
}

#[test]
fn kimi_relative_sources_keep_resolution_for_install_time() {
    let catalog = parse("kimi", MarketplaceFormat::Kimi, KIMI_OFFICIAL_MARKETPLACE);
    let datasource = catalog.candidate_by_name("kimi-datasource").unwrap();
    match &datasource.install_plan {
        MarketplaceInstallPlan::Supported { spec, .. } => {
            assert_eq!(spec, "path:./official/kimi-datasource");
        }
        other => panic!("relative path must be a supported local plan: {other:?}"),
    }
    assert_eq!(catalog.base, None, "parser never invents a base");
}

#[test]
fn kimi_zip_sources_are_visible_but_not_advertised_as_installable() {
    let body = r#"{
      "version": "2",
      "plugins": [
        {"id": "zipped", "source": "https://example.invalid/zipped.zip"},
        {"id": "tarred", "source": "https://example.invalid/tarred.tgz"}
      ]
    }"#;
    let catalog = parse("kimi", MarketplaceFormat::Kimi, body);
    let zipped = catalog.candidate_by_name("zipped").unwrap();
    assert!(matches!(
        zipped.source,
        MarketplaceSourceSpec::ArchiveUrl { .. }
    ));
    assert!(matches!(
        &zipped.install_plan,
        MarketplaceInstallPlan::Unsupported { reason, .. }
            if reason == super::parsers::kimi::KIMI_ZIP_UNSUPPORTED_REASON
    ));

    let tarred = catalog.candidate_by_name("tarred").unwrap();
    assert!(matches!(
        &tarred.install_plan,
        MarketplaceInstallPlan::Supported { source_kind, .. }
            if source_kind == super::parsers::kimi::KIMI_GZIP_TARBALL_SOURCE_KIND
    ));
}

#[test]
fn claude_docs_marketplace_parses_both_source_forms() {
    let catalog = parse_auto("claude", CLAUDE_DOCS_MARKETPLACE);
    assert_eq!(catalog.format, MarketplaceFormat::Claude);
    assert_eq!(catalog.total_candidates(), 2);
    assert_eq!(catalog.error_count(), 0);

    let formatter = catalog.candidate_by_name("code-formatter").unwrap();
    assert!(formatter.install_plan.is_supported());
    assert!(matches!(
        formatter.source,
        MarketplaceSourceSpec::LocalPath { .. }
    ));
    assert_eq!(formatter.version.as_deref(), Some("2.1.0"));
    assert_eq!(formatter.author.as_deref(), Some("DevTools Team"));

    let deploy = catalog.candidate_by_name("deployment-tools").unwrap();
    assert!(matches!(
        &deploy.source,
        MarketplaceSourceSpec::GitHub { owner, repo, .. }
            if owner == "company" && repo == "deploy-plugin"
    ));
    match &deploy.install_plan {
        MarketplaceInstallPlan::Supported { spec, .. } => {
            assert_eq!(spec, "github:company/deploy-plugin");
        }
        other => panic!("github source must map to a supported plan: {other:?}"),
    }
}

#[test]
fn claude_source_object_matrix_matches_documented_forms() {
    // One entry per documented `source` discriminator, from the docs
    // source-form table. npm/command stay honestly unsupported.
    let body = r#"{
        "name": "matrix", "owner": {"name": "t"},
        "plugins": [
          {"name": "gh", "source": {"source": "github", "repo": "a/b", "ref": "v1"}},
          {"name": "u", "source": {"source": "url", "url": "https://git.example.com/x.git"}},
          {"name": "gs", "source": {"source": "git-subdir", "url": "https://github.com/a/sub", "path": "p"}},
          {"name": "np", "source": {"source": "npm", "package": "pkg"}},
          {"name": "ar", "source": {"source": "archive", "url": "https://x.example.com/p.tgz", "sha256": "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789"}},
          {"name": "cm", "source": {"source": "command", "command": "curl x"}}
        ]
      }"#;
    let catalog = parse("claude", MarketplaceFormat::Claude, body);
    assert_eq!(catalog.total_candidates(), 6);

    let gh = catalog.candidate_by_name("gh").unwrap();
    assert!(gh.install_plan.is_supported());
    assert!(matches!(&gh.source,
        MarketplaceSourceSpec::GitHub { git_ref: Some(r), .. } if r == "v1"));
    assert!(gh.diagnostics.iter().any(|d| d.code == "UNAPPLIED_PIN"));

    let url = catalog.candidate_by_name("u").unwrap();
    assert!(!url.install_plan.is_supported());
    assert!(matches!(url.source, MarketplaceSourceSpec::GitUrl { .. }));

    let gs = catalog.candidate_by_name("gs").unwrap();
    assert!(gs.install_plan.is_supported());
    assert!(matches!(&gs.source,
        MarketplaceSourceSpec::GitHub { owner, repo, .. }
            if owner == "a" && repo == "sub"));

    let np = catalog.candidate_by_name("np").unwrap();
    assert!(!np.install_plan.is_supported());
    assert!(matches!(&np.source, MarketplaceSourceSpec::Npm { package } if package == "pkg"));

    let ar = catalog.candidate_by_name("ar").unwrap();
    assert!(ar.install_plan.is_supported());
    let pin = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    assert!(matches!(&ar.source,
        MarketplaceSourceSpec::ArchiveUrl { url, sha256: Some(s) }
            if url == "https://x.example.com/p.tgz" && s == pin));
    assert!(matches!(&ar.install_plan,
        MarketplaceInstallPlan::Supported { spec, .. }
            if *spec == format!("https://x.example.com/p.tgz#sha256={pin}")));

    let cm = catalog.candidate_by_name("cm").unwrap();
    assert!(!cm.install_plan.is_supported());
    assert!(matches!(cm.source, MarketplaceSourceSpec::Refused { .. }));
}

#[test]
fn claude_component_declarations_drive_compatibility() {
    let body = r#"{
        "name": "comp", "owner": {"name": "t"},
        "plugins": [
          {"name": "skills-only", "source": "./a", "skills": ["s1", "s2"]},
          {"name": "mixed", "source": "./b", "skills": ["s1"], "commands": ["c1"], "hooks": ["h1"]},
          {"name": "commands-only", "source": "./c", "commands": ["c1"]},
          {"name": "nothing-declared", "source": "./d"},
          {"name": "mcp-only", "source": "./e", "mcpServers": {"docs": {"command": "x"}}}
        ]
      }"#;
    let catalog = parse("claude", MarketplaceFormat::Claude, body);
    use crate::manifest::PluginCompatibility as C;
    assert_eq!(
        catalog
            .candidate_by_name("skills-only")
            .unwrap()
            .compatibility,
        Some(C::Full)
    );
    assert_eq!(
        catalog.candidate_by_name("mixed").unwrap().compatibility,
        Some(C::Partial)
    );
    assert_eq!(
        catalog
            .candidate_by_name("commands-only")
            .unwrap()
            .compatibility,
        Some(C::Unsupported)
    );
    assert_eq!(
        catalog
            .candidate_by_name("nothing-declared")
            .unwrap()
            .compatibility,
        None,
        "no declaration means decided at install review, not guessed"
    );
    assert_eq!(
        catalog.candidate_by_name("mcp-only").unwrap().compatibility,
        Some(C::Full),
        "declared MCP without transport is treated as supported-capable"
    );
}

#[test]
fn codex_docs_marketplace_parses_with_policy_as_display_only() {
    let catalog = parse_auto("codex", CODEX_DOCS_MARKETPLACE);
    assert_eq!(catalog.format, MarketplaceFormat::Codex);
    assert_eq!(catalog.total_candidates(), 1);

    let plugin = catalog.candidate_by_name("my-plugin").unwrap();
    assert!(plugin.install_plan.is_supported());
    assert!(matches!(
        plugin.source,
        MarketplaceSourceSpec::LocalPath { .. }
    ));
    assert_eq!(plugin.categories, vec!["Productivity".to_string()]);
    assert!(
        !plugin
            .diagnostics
            .iter()
            .any(|d| d.code == "NO_AUTO_INSTALL"),
        "AVAILABLE policy needs no warning"
    );
}

#[test]
fn codex_installed_by_default_never_auto_installs_and_says_so() {
    let body = r#"{
        "name": "n",
        "plugins": [
          {"name": "p", "source": {"source": "local", "path": "./p"},
           "policy": {"installation": "INSTALLED_BY_DEFAULT"}}
        ]
      }"#;
    let catalog = parse("codex", MarketplaceFormat::Codex, body);
    let plugin = catalog.candidate_by_name("p").unwrap();
    assert!(
        plugin
            .diagnostics
            .iter()
            .any(|d| d.code == "NO_AUTO_INSTALL"),
        "auto-install policy must be visibly ignored on the entry itself"
    );
}

#[test]
fn codex_not_available_downgrades_install_plan() {
    let body = r#"{
        "name": "n",
        "plugins": [
          {"name": "p", "source": {"source": "local", "path": "./p"},
           "policy": {"installation": "NOT_AVAILABLE"}}
        ]
      }"#;
    let catalog = parse("codex", MarketplaceFormat::Codex, body);
    let plugin = catalog.candidate_by_name("p").unwrap();
    assert!(!plugin.install_plan.is_supported());
}

#[test]
fn codewhale_native_catalog_maps_install_specs() {
    let body = r#"{
        "name": "team", "description": "Team plugins", "version": "1",
        "plugins": [
          {"name": "fmt", "source": "github:owner/repo", "version": "2.1.0"},
          {"name": "local", "source": "path:./bundled/fmt"},
          {"name": "bad", "source": "github:"}
        ]
      }"#;
    let catalog = parse_auto("native", body);
    assert_eq!(catalog.format, MarketplaceFormat::Codewhale);
    assert_eq!(catalog.total_candidates(), 3);

    assert!(
        catalog
            .candidate_by_name("fmt")
            .unwrap()
            .install_plan
            .is_supported()
    );
    assert!(
        catalog
            .candidate_by_name("local")
            .unwrap()
            .install_plan
            .is_supported()
    );
    let bad = catalog.candidate_by_name("bad").unwrap();
    assert!(
        !bad.install_plan.is_supported(),
        "invalid spec stays visible and unsupported"
    );
}

#[test]
fn malformed_entry_degrades_alone() {
    let body = r#"{
        "version": "1",
        "plugins": [
          {"id": "good", "source": "./good"},
          "not-an-object",
          {"displayName": "missing id and source"},
          {"id": "also-good", "source": "https://github.com/a/b"}
        ]
      }"#;
    let catalog = parse("kimi", MarketplaceFormat::Kimi, body);
    assert_eq!(catalog.total_candidates(), 2, "survivors stay listed");
    assert!(catalog.candidate_by_name("good").is_some());
    assert!(catalog.candidate_by_name("also-good").is_some());
    assert!(catalog.error_count() >= 2);
}

#[test]
fn missing_plugins_array_is_a_catalog_error() {
    for (format, body) in [
        (MarketplaceFormat::Kimi, r#"{"version": "1"}"#),
        (
            MarketplaceFormat::Claude,
            r#"{"name": "x", "owner": {"name": "o"}}"#,
        ),
        (MarketplaceFormat::Codex, r#"{"name": "x"}"#),
        (MarketplaceFormat::Codewhale, r#"{"name": "x"}"#),
    ] {
        let catalog = parse("t", format, body);
        assert_eq!(catalog.total_candidates(), 0);
        assert!(catalog.error_count() >= 1, "{format} needs a plugins array");
    }
}

#[test]
fn provenance_tiers_never_grant_trust() {
    let catalog = parse("kimi", MarketplaceFormat::Kimi, KIMI_OFFICIAL_MARKETPLACE);
    for candidate in &catalog.candidates {
        assert!(!candidate.provenance.grants_trust());
        assert!(
            candidate.compatibility.is_none() || !candidate.has_errors(),
            "official/curated labels are display-only"
        );
    }
    let official = catalog.candidate_by_name("kimi-datasource").unwrap();
    assert_eq!(official.provenance.tier, CatalogTier::Official);
}

#[test]
fn unknown_fields_warn_instead_of_being_reinterpreted() {
    let body = r#"{
        "version": "1",
        "plugins": [
          {"id": "p", "source": "./p", "download_url": "https://x", "entries": []}
        ]
      }"#;
    let catalog = parse("kimi", MarketplaceFormat::Kimi, body);
    assert_eq!(catalog.total_candidates(), 1);
    let plugin = catalog.candidate_by_name("p").unwrap();
    assert!(
        plugin
            .diagnostics
            .iter()
            .any(|d| d.code == "UNKNOWN_FIELD" && d.message.contains("download_url")),
        "invented fields must be visibly ignored, not parsed"
    );
}

#[test]
fn archive_url_catalog_detection_preserves_native_and_kimi_identity() {
    for url in [
        "https://example.invalid/plugins.tar.gz#path=plugins/tools",
        "http://127.0.0.1:9000/plugins.tar.gz#path=plugins/tools",
    ] {
        let native = serde_json::json!({"plugins": [{"name": "tools", "source": url}]});
        let catalog = parse_auto("native", &native.to_string());
        assert_eq!(catalog.format, MarketplaceFormat::Codewhale);
        assert_eq!(catalog.error_count(), 0);
        assert!(matches!(
            &catalog.candidate_by_name("tools").unwrap().install_plan,
            MarketplaceInstallPlan::Supported { spec, .. } if spec == url
        ));

        let kimi = serde_json::json!({"plugins": [{"id": "tools", "source": url}]});
        assert_eq!(
            parse_auto("kimi", &kimi.to_string()).format,
            MarketplaceFormat::Kimi
        );
    }
}

#[test]
fn auto_detection_reports_ambiguity_instead_of_guessing() {
    let body = r#"{"plugins": [{"name": "p", "source": "./p"}]}"#;
    let catalog = parse_auto("t", body);
    assert_eq!(catalog.total_candidates(), 0);
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|d| d.code == "AMBIGUOUS_FORMAT")
    );
}

#[test]
fn non_object_catalog_fails_closed() {
    let catalog = parse_auto("t", "[1, 2, 3]");
    assert_eq!(catalog.total_candidates(), 0);
    assert!(catalog.error_count() >= 1);
}

#[test]
fn claude_relative_install_source_is_rooted_outside_catalog_metadata() {
    use super::document::{
        CatalogInstallResolution, load_catalog_document, resolve_candidate_install,
    };
    let tmp = tempfile::tempdir().unwrap();
    let metadata = tmp.path().join(".claude-plugin");
    std::fs::create_dir_all(&metadata).unwrap();
    let path = metadata.join("marketplace.json");
    std::fs::write(&path, r#"{"name":"official","owner":{"name":"Example"},"plugins":[{"name":"linear","source":"./external_plugins/linear"}]}"#).unwrap();
    let loaded = load_catalog_document("official", tmp.path(), path.to_str().unwrap()).unwrap();
    let candidate = loaded.entry.catalog.candidate_by_name("linear").unwrap();
    let registry = crate::PluginRegistry::empty(tmp.path());
    let CatalogInstallResolution::Supported { spec, .. } =
        resolve_candidate_install(&loaded.entry, candidate, &registry)
    else {
        panic!("expected path source")
    };
    assert_eq!(
        std::path::Path::new(spec.strip_prefix("path:").unwrap()),
        tmp.path().join("external_plugins/linear")
    );
}

#[test]
fn catalog_names_are_validated_and_duplicates_fail_closed() {
    let body = r#"{
        "name": "team",
        "plugins": [
          {"name": "fmt", "source": "github:owner/fmt"},
          {"name": "fmt", "source": "github:someone-else/fmt"},
          {"name": "bad\u001b[31mname", "source": "github:owner/x"},
          {"name": "ok", "source": "github:owner/ok"}
        ]
      }"#;
    let catalog = parse_auto("native", body);
    assert_eq!(catalog.format, MarketplaceFormat::Codewhale);
    assert_eq!(
        catalog.total_candidates(),
        3,
        "a control-bearing name never becomes a candidate"
    );
    assert!(catalog.diagnostics.iter().any(|d| d.code == "INVALID_NAME"));
    let duplicates: Vec<_> = catalog
        .candidates
        .iter()
        .filter(|candidate| candidate.name == "fmt")
        .collect();
    assert_eq!(duplicates.len(), 2);
    for candidate in duplicates {
        assert!(
            candidate
                .diagnostics
                .iter()
                .any(|d| d.code == "DUPLICATE_NAME"),
            "an ambiguous identity must not be installable"
        );
        assert!(candidate.has_errors());
    }
    assert!(!catalog.candidate_by_name("ok").unwrap().has_errors());
}

#[test]
fn catalog_archive_sources_must_be_remote() {
    // F01-03: a non-local source's spec is re-parsed by the installer, which
    // reads any unprefixed value as a local directory. An archive `url` that
    // is really a path must not reach it as one.
    use super::document::{
        CatalogInstallResolution, load_catalog_document, resolve_candidate_install,
    };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("marketplace.json");
    std::fs::write(
        &path,
        r#"{"name":"n","owner":{"name":"t"},"plugins":[
            {"name":"absolute","source":{"source":"archive","url":"/etc"}},
            {"name":"parent","source":{"source":"archive","url":"../../outside"}},
            {"name":"prefixed","source":{"source":"archive","url":"path:/etc"}},
            {"name":"remote","source":{"source":"archive","url":"https://x.example.com/p.tgz"}}
        ]}"#,
    )
    .unwrap();
    let loaded = load_catalog_document("n", tmp.path(), path.to_str().unwrap()).unwrap();
    assert_eq!(loaded.entry.catalog.format, MarketplaceFormat::Claude);
    let registry = crate::PluginRegistry::empty(tmp.path());
    for name in ["absolute", "parent", "prefixed"] {
        let candidate = loaded.entry.catalog.candidate_by_name(name).unwrap();
        match resolve_candidate_install(&loaded.entry, candidate, &registry) {
            CatalogInstallResolution::Unsupported { reason } => {
                assert!(reason.contains("is not a remote"), "{reason}");
            }
            _ => panic!("archive `{name}` must not resolve to a local install"),
        }
    }
    let remote = loaded.entry.catalog.candidate_by_name("remote").unwrap();
    let CatalogInstallResolution::Supported { spec, .. } =
        resolve_candidate_install(&loaded.entry, remote, &registry)
    else {
        panic!("a remote archive stays installable")
    };
    assert_eq!(spec, "https://x.example.com/p.tgz");
}

#[test]
fn catalog_local_sources_cannot_leave_the_catalog_directory() {
    use super::document::{
        CatalogInstallResolution, load_catalog_document, resolve_candidate_install,
    };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("marketplace.json");
    std::fs::write(
        &path,
        r#"{"name":"n","plugins":[
            {"name":"absolute","source":{"source":"local","path":"/etc"}},
            {"name":"parent","source":{"source":"local","path":"./plugins/../../outside"}},
            {"name":"inside","source":{"source":"local","path":"./plugins/inside"}}
        ]}"#,
    )
    .unwrap();
    let loaded = load_catalog_document("n", tmp.path(), path.to_str().unwrap()).unwrap();
    assert_eq!(loaded.entry.catalog.format, MarketplaceFormat::Codex);
    let registry = crate::PluginRegistry::empty(tmp.path());
    for name in ["absolute", "parent"] {
        let candidate = loaded.entry.catalog.candidate_by_name(name).unwrap();
        match resolve_candidate_install(&loaded.entry, candidate, &registry) {
            CatalogInstallResolution::Unsupported { reason } => {
                assert!(reason.contains("leaves the catalog directory"), "{reason}");
            }
            _ => panic!("`{name}` must not resolve outside the catalog"),
        }
    }
    let inside = loaded.entry.catalog.candidate_by_name("inside").unwrap();
    let CatalogInstallResolution::Supported { spec, .. } =
        resolve_candidate_install(&loaded.entry, inside, &registry)
    else {
        panic!("a path inside the catalog stays installable")
    };
    assert_eq!(
        std::path::Path::new(spec.strip_prefix("path:").unwrap()),
        tmp.path().join("./plugins/inside")
    );
}
