use super::*;

fn table(body: &str) -> toml::Table {
    toml::from_str(body).expect("fixture parses")
}

fn canonical(body: &str) -> (toml::Table, LegacyRootMigration) {
    let mut root = table(body);
    let receipt = apply_to_table(&mut root);
    (root, receipt)
}

fn at<'a>(root: &'a toml::Table, path: &[&str]) -> Option<&'a str> {
    let (last, parents) = path.split_last()?;
    let mut current = root;
    for part in parents {
        current = current.get(*part)?.as_table()?;
    }
    current.get(*last)?.as_str()
}

fn document(body: &str) -> toml_edit::DocumentMut {
    body.parse().expect("fixture parses as a document")
}

#[test]
fn rule_three_moves_a_deepseek_endpoint_and_key_into_the_deepseek_table() {
    let (root, receipt) = canonical(
        r#"
base_url = "https://proxy.example.test/v1"
api_key = "sk-root"
"#,
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://proxy.example.test/v1")
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-root")
    );
    assert!(!root.contains_key("base_url") && !root.contains_key("api_key"));
    assert!(root.get("provider").is_none(), "no guess for a plain host");
    assert_eq!(
        receipt.summary().as_deref(),
        Some("moved top-level base_url to [providers.deepseek], api_key to [providers.deepseek]")
    );
}

#[test]
fn camel_case_aliases_move_too() {
    let (root, _) = canonical("baseUrl = \"https://proxy.example.test/v1\"\napiKey = \"sk\"\n");
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://proxy.example.test/v1")
    );
    assert_eq!(at(&root, &["providers", "deepseek", "api_key"]), Some("sk"));
    assert!(!has_legacy_root_keys(&root));
}

#[test]
fn rule_one_turns_the_literal_custom_route_into_a_custom_table() {
    let (root, receipt) = canonical(
        r#"
provider = "custom"
base_url = "http://127.0.0.1:18181/v1"
api_key = "sk-custom"
default_text_model = "my-local-model"
"#,
    );
    assert_eq!(
        at(&root, &["providers", "custom", "base_url"]),
        Some("http://127.0.0.1:18181/v1")
    );
    assert_eq!(
        at(&root, &["providers", "custom", "api_key"]),
        Some("sk-custom")
    );
    assert_eq!(
        at(&root, &["providers", "custom", "model"]),
        Some("my-local-model")
    );
    assert_eq!(at(&root, &["default_text_model"]), Some("my-local-model"));
    assert!(
        root["providers"]
            .as_table()
            .unwrap()
            .get("deepseek")
            .is_none()
    );
    assert!(
        receipt
            .notes
            .iter()
            .any(|note| matches!(note, LegacyRootNote::CopiedModel { .. }))
    );
}

#[test]
fn literal_custom_with_its_own_table_leaves_the_root_to_deepseek() {
    let (root, _) = canonical(
        r#"
provider = "custom"
base_url = "https://proxy.example.test/v1"

[providers.custom]
base_url = "http://127.0.0.1:9/v1"
"#,
    );
    assert_eq!(
        at(&root, &["providers", "custom", "base_url"]),
        Some("http://127.0.0.1:9/v1")
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://proxy.example.test/v1")
    );
}

#[test]
fn rule_two_sends_a_foreign_official_host_to_its_vendor() {
    for (url, table) in [
        ("https://api.xiaomimimo.com/v1", "xiaomi_mimo"),
        ("https://token-plan-sgp.xiaomimimo.com/v1", "xiaomi_mimo"),
        ("https://openrouter.ai/api/v1", "openrouter"),
        ("https://api.moonshot.ai/v1", "moonshot"),
        ("https://api.openai.com/v1", "openai"),
        ("https://chatgpt.com/backend-api", "openai_codex"),
        ("https://integrate.api.nvidia.com/v1", "nvidia_nim"),
        (
            "https://ark.cn-beijing.volces.com/api/coding/v3",
            "volcengine",
        ),
    ] {
        let (root, _) = canonical(&format!(
            "provider = \"deepseek\"\nbase_url = \"{url}\"\napi_key = \"sk-deepseek\"\n"
        ));
        assert_eq!(
            at(&root, &["providers", table, "base_url"]),
            Some(url),
            "{url} belongs to {table}"
        );
        // A leftover DeepSeek key never follows a URL to another vendor.
        assert_eq!(
            at(&root, &["providers", "deepseek", "api_key"]),
            Some("sk-deepseek")
        );
        assert!(at(&root, &["providers", table, "api_key"]).is_none());
    }
}

#[test]
fn loopback_and_deepseek_hosts_stay_with_deepseek() {
    for url in [
        "http://localhost:11434/v1",
        "https://api.deepseek.com/beta",
        "https://api.deepseek.com/anthropic",
    ] {
        let (root, _) = canonical(&format!("base_url = \"{url}\"\n"));
        assert_eq!(at(&root, &["providers", "deepseek", "base_url"]), Some(url));
    }
}

#[test]
fn provider_guesses_are_written_only_when_no_layer_sets_provider() {
    let (root, receipt) = canonical("base_url = \"https://integrate.api.nvidia.com/v1\"\n");
    assert_eq!(at(&root, &["provider"]), Some("nvidia-nim"));
    assert!(receipt.notes.iter().any(|note| matches!(
        note,
        LegacyRootNote::GuessedProvider {
            provider: "nvidia-nim",
            ..
        }
    )));

    let (root, _) = canonical("base_url = \"https://api.deepseeki.com/v1\"\n");
    assert_eq!(at(&root, &["provider"]), Some("deepseek-cn"));
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://api.deepseeki.com/v1")
    );

    let (root, _) =
        canonical("provider = \"deepseek\"\nbase_url = \"https://integrate.api.nvidia.com/v1\"\n");
    assert_eq!(at(&root, &["provider"]), Some("deepseek"));

    // A profile inherits the base file's provider: no guess inside it.
    let (root, _) = canonical(
        r#"
provider = "deepseek"

[profiles.nim]
base_url = "https://integrate.api.nvidia.com/v1"
"#,
    );
    assert!(at(&root, &["profiles", "nim", "provider"]).is_none());
}

#[test]
fn a_key_follows_the_endpoint_only_to_an_explicitly_selected_vendor() {
    // The shipped v0.10.0 `[profiles.nvidia-nim]` shape.
    let (root, _) = canonical(
        r#"
[profiles.nvidia-nim]
provider = "nvidia-nim"
api_key = "nvapi-key"
base_url = "https://integrate.api.nvidia.com/v1"
"#,
    );
    assert_eq!(
        at(
            &root,
            &[
                "profiles",
                "nvidia-nim",
                "providers",
                "nvidia_nim",
                "api_key"
            ]
        ),
        Some("nvapi-key")
    );
    assert_eq!(
        at(
            &root,
            &[
                "profiles",
                "nvidia-nim",
                "providers",
                "nvidia_nim",
                "base_url"
            ]
        ),
        Some("https://integrate.api.nvidia.com/v1")
    );

    // The vendor already has a key: the root key stays DeepSeek's.
    let (root, _) = canonical(
        r#"
provider = "nvidia-nim"
api_key = "sk-root"
base_url = "https://integrate.api.nvidia.com/v1"

[providers.nvidia_nim]
api_key = "nvapi-own"
"#,
    );
    assert_eq!(
        at(&root, &["providers", "nvidia_nim", "api_key"]),
        Some("nvapi-own")
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-root")
    );

    // No explicit provider: the key stays DeepSeek's even on a NIM host.
    let (root, _) =
        canonical("api_key = \"sk-root\"\nbase_url = \"https://integrate.api.nvidia.com/v1\"\n");
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-root")
    );
    assert!(at(&root, &["providers", "nvidia_nim", "api_key"]).is_none());
}

#[test]
fn empty_values_are_dropped() {
    let (root, receipt) = canonical("base_url = \"  \"\napi_key = \"\"\n");
    assert!(!has_legacy_root_keys(&root));
    assert!(root.get("providers").is_none());
    assert_eq!(
        receipt
            .notes
            .iter()
            .filter(|note| matches!(note, LegacyRootNote::DroppedEmpty { .. }))
            .count(),
        2
    );
}

#[test]
fn equal_values_merge_silently() {
    let (root, receipt) = canonical(
        r#"
base_url = "https://api.deepseek.com/beta"

[providers.deepseek]
base_url = "https://api.deepseek.com/beta"
"#,
    );
    assert!(!has_legacy_root_keys(&root));
    assert!(matches!(receipt.notes[..], [LegacyRootNote::Merged { .. }]));
}

const CONFLICT: &str = r#"# my config
provider = "deepseek"
base_url = "https://root.example.test/v1"
api_key = "sk-root"

[providers.deepseek]
base_url = "https://table.example.test/v1"
api_key = "sk-table"
"#;

#[test]
fn memory_resolves_a_conflict_with_the_runtime_precedence() {
    let (root, receipt) = canonical(CONFLICT);
    // base_url: the table wins (both crates already did this).
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://table.example.test/v1")
    );
    // api_key: the top-level key wins (the key the TUI sent).
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-root")
    );
    assert!(!has_legacy_root_keys(&root));
    assert_eq!(receipt.unresolved_conflicts().count(), 2);
    assert!(!receipt.changes_file());
    for line in receipt.lines() {
        assert!(!line.contains("sk-"), "never prints a value: {line}");
        assert!(
            !line.contains("example.test"),
            "never prints a value: {line}"
        );
    }
}

#[test]
fn a_document_keeps_a_conflict_unless_told_which_side_to_keep() {
    let mut doc = document(CONFLICT);
    let receipt = apply_to_document(&mut doc, None);
    assert_eq!(
        doc.to_string(),
        CONFLICT,
        "conflicts are never resolved silently"
    );
    assert!(!receipt.changes_file());

    let mut doc = document(CONFLICT);
    apply_to_document(&mut doc, Some(LegacyRootPrefer::TopLevel));
    let root = table(&doc.to_string());
    assert!(!has_legacy_root_keys(&root));
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://root.example.test/v1")
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-root")
    );
    assert!(doc.to_string().starts_with("# my config\n"));

    let mut doc = document(CONFLICT);
    apply_to_document(&mut doc, Some(LegacyRootPrefer::Table));
    let root = table(&doc.to_string());
    assert!(!has_legacy_root_keys(&root));
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://table.example.test/v1")
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-table")
    );
}

#[test]
fn a_document_migration_keeps_comments_and_is_idempotent() {
    let body = r#"# header comment
provider = "deepseek"
# the endpoint
base_url = "https://proxy.example.test/v1"
reasoning_effort = "max"

[tui]
alternate_screen = "auto"
"#;
    let mut doc = document(body);
    let receipt = apply_to_document(&mut doc, None);
    assert!(receipt.changes_file());
    let migrated = doc.to_string();
    assert!(migrated.starts_with("# header comment\n"), "{migrated}");
    assert!(
        migrated.contains("reasoning_effort = \"max\""),
        "{migrated}"
    );
    assert!(migrated.contains("[tui]"), "{migrated}");
    let root = table(&migrated);
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://proxy.example.test/v1")
    );
    assert!(!has_legacy_root_keys(&root));

    let mut again = document(&migrated);
    assert!(apply_to_document(&mut again, None).is_empty());
    assert_eq!(again.to_string(), migrated);
}

#[test]
fn vision_keeps_the_key_it_inherited() {
    let (root, _) = canonical(
        r#"
api_key = "sk-root"

[vision_model]
model = "vision-x"
base_url = "https://vision.example.test/v1"
"#,
    );
    assert_eq!(at(&root, &["vision_model", "api_key"]), Some("sk-root"));
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-root")
    );

    // A vision table with its own key keeps it.
    let (root, _) = canonical("api_key = \"sk-root\"\n[vision_model]\napi_key = \"sk-vision\"\n");
    assert_eq!(at(&root, &["vision_model", "api_key"]), Some("sk-vision"));
}

#[test]
fn profiles_move_into_their_own_provider_tables() {
    let (root, _) = canonical(
        r#"
[providers.deepseek]
base_url = "https://api.deepseek.com/beta"

[profiles.work]
api_key = "WORK"
base_url = "https://work.example.test/v1"
"#,
    );
    assert_eq!(
        at(
            &root,
            &["profiles", "work", "providers", "deepseek", "base_url"]
        ),
        Some("https://work.example.test/v1")
    );
    assert_eq!(
        at(
            &root,
            &["profiles", "work", "providers", "deepseek", "api_key"]
        ),
        Some("WORK")
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://api.deepseek.com/beta")
    );
}

#[test]
fn restore_conflicts_puts_back_a_pair_a_typed_save_dropped() {
    // What a typed save renders: the in-memory view, root keys gone.
    let (canonical_root, _) = canonical(CONFLICT);
    let rendered = toml::to_string(&canonical_root).unwrap();
    let mut doc = document(&rendered);
    assert!(restore_conflicts(&mut doc, CONFLICT));
    let root = table(&doc.to_string());
    assert_eq!(
        at(&root, &["base_url"]),
        Some("https://root.example.test/v1")
    );
    assert_eq!(at(&root, &["api_key"]), Some("sk-root"));
    assert_eq!(
        at(&root, &["providers", "deepseek", "base_url"]),
        Some("https://table.example.test/v1")
    );
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-table")
    );

    // The user changed the key during the session: their value wins and the
    // top-level key stays gone.
    let mut changed = canonical_root.clone();
    changed["providers"]["deepseek"]
        .as_table_mut()
        .unwrap()
        .insert("api_key".into(), "sk-new".into());
    let mut doc = document(&toml::to_string(&changed).unwrap());
    restore_conflicts(&mut doc, CONFLICT);
    let root = table(&doc.to_string());
    assert!(root.get("api_key").is_none());
    assert_eq!(
        at(&root, &["providers", "deepseek", "api_key"]),
        Some("sk-new")
    );
    assert_eq!(
        at(&root, &["base_url"]),
        Some("https://root.example.test/v1")
    );
}

#[test]
fn settle_after_write_ends_a_conflict_the_write_touched() {
    let mut doc = document(CONFLICT);
    let snapshot = conflict_snapshot(&doc);
    assert_eq!(snapshot.len(), 2);
    crate::set_config_document_value(&mut doc, &["providers", "deepseek", "api_key"], "sk-set")
        .unwrap();
    settle_conflicts_after_write(&mut doc, snapshot);
    let root = table(&doc.to_string());
    assert!(root.get("api_key").is_none());
    assert_eq!(
        at(&root, &["base_url"]),
        Some("https://root.example.test/v1")
    );
}

#[test]
fn notices_drain_exactly_once() {
    // Other tests may queue notices concurrently; follow only this one.
    let marker = "/tmp/notices-drain-once/config.toml.pre-migrate.bak";
    let (_, receipt) = canonical("base_url = \"https://proxy.example.test/v1\"\n");
    queue_notice(&receipt, std::path::Path::new(marker));
    queue_notice(&receipt, std::path::Path::new(marker));
    let ours: Vec<String> = take_notices()
        .into_iter()
        .filter(|notice| notice.contains(marker))
        .collect();
    assert_eq!(ours.len(), 1, "{ours:?}");
    assert!(ours[0].contains("[providers.deepseek]"), "{ours:?}");
    assert!(!take_notices().iter().any(|notice| notice.contains(marker)));
}

#[test]
fn apply_to_document_omits_move_receipt_when_destination_parent_is_not_a_table() {
    let body = "api_key = \"sk-root\"\nproviders = \"legacy\"\n";
    let mut doc = document(body);
    let receipt = apply_to_document(&mut doc, None);
    assert!(!receipt.changes_file(), "{receipt:?}");
    assert!(receipt.is_empty(), "{receipt:?}");
    assert_eq!(doc.to_string(), body);
    assert!(!preview_document(&doc, None).changes_file());
}

#[test]
fn migrate_legacy_root_config_errors_and_writes_no_backup_on_non_table_providers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("config.toml");
    let body = "[profiles.work]\napi_key = \"sk-work\"\nproviders = \"legacy\"\n";
    std::fs::write(&path, body).expect("write config");

    let err = crate::migrate_legacy_root_config(&path, None)
        .expect_err("non-table providers parent must fail migration");
    assert!(format!("{err:#}").contains("providers"), "{err:#}");
    assert_eq!(std::fs::read_to_string(&path).expect("read config"), body);
    let backup = path.with_file_name("config.toml.pre-migrate.bak");
    assert!(!backup.exists(), "failed migration must not write a backup");
}

#[test]
fn preview_and_migrate_agree_when_profile_has_non_table_providers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("config.toml");
    let body =
        "api_key = \"sk-root\"\n\n[profiles.work]\napi_key = \"sk-work\"\nproviders = \"legacy\"\n";
    std::fs::write(&path, body).expect("write config");

    let preview_err = crate::preview_legacy_root_config(&path, None)
        .expect_err("dry-run preview must fail on the same non-table providers shape");
    let migrate_err = crate::migrate_legacy_root_config(&path, None)
        .expect_err("real migration must fail on non-table providers");
    assert!(format!("{preview_err:#}").contains("providers"));
    assert!(format!("{migrate_err:#}").contains("providers"));

    let doc = document(body);
    assert!(!preview_document(&doc, None).changes_file());
    assert_eq!(std::fs::read_to_string(&path).expect("read config"), body);
}
