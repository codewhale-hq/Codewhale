//! Real dispatcher regression: nested config edits retain TOML types and
//! unrelated bytes. Only config commands run; no provider, player or OS banner.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(
        std::env::var_os("CARGO_BIN_EXE_codewhale")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_codewhale"))),
    )
    .env_clear()
    .env("HOME", root)
    .env("USERPROFILE", root)
    .env("CODEWHALE_HOME", root.join("state"))
    .env("CODEWHALE_SECRET_BACKEND", "file")
    .current_dir(root)
    .arg("--config")
    .arg(root.join("config.toml"))
    .arg("config")
    .args(args)
    .output()
    .unwrap()
}
fn ok(root: &Path, args: &[&str]) -> String {
    let out = run(root, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn nested_notification_cli_set_get_unset_is_typed_lossless_and_validated() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = root.join("config.toml");
    fs::write(&path, "# keep operator note\n\"notifications.quiet\" = \"false\"\n[notifications]\nfuture = \"keep\"\n[notifications.events]\ninput-needed = false\n").unwrap();
    for (key, value) in [
        ("sound", "whale"),
        ("quiet", "true"),
        ("threshold_secs", "17"),
        ("events.approval-needed", "false"),
        ("event_sound.events", r#"["model-notify", "input-needed"]"#),
        ("sound_file", "custom call.wav"),
    ] {
        ok(root, &["set", &format!("notifications.{key}"), value]);
    }
    assert_eq!(ok(root, &["get", "notifications.sound"]).trim(), "whale");
    assert_eq!(ok(root, &["get", "notifications.quiet"]).trim(), "true");
    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# keep operator note"));
    let raw: toml::Value = toml::from_str(&saved).unwrap();
    assert_eq!(raw["notifications"]["quiet"].as_bool(), Some(true));
    assert_eq!(
        raw["notifications"]["threshold_secs"].as_integer(),
        Some(17)
    );
    assert_eq!(
        raw["notifications"]["event_sound"]["events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        raw["notifications"]["events"]["input-needed"].as_bool(),
        Some(false)
    );
    assert_eq!(raw["notifications"]["future"].as_str(), Some("keep"));
    assert!(!raw.as_table().unwrap().contains_key("notifications.quiet"));
    for (key, value) in [
        ("notifications", "false"),
        ("notifications.quiet", "maybe"),
        ("notifications.threshold_secs", "18446744073709551615"),
        ("notifications.event_sound.events", r#"["unknown"]"#),
    ] {
        assert!(!run(root, &["set", key, value]).status.success());
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
    }
    ok(root, &["unset", "notifications.quiet"]);
    assert_eq!(ok(root, &["get", "notifications.quiet"]).trim(), "false");
    assert_eq!(ok(root, &["get", "notifications.sound"]).trim(), "whale");
    assert!(
        ok(root, &["get", "notifications"])
            .contains("notifications.events.approval-needed = false")
    );
}
