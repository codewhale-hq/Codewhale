//! Which file owns a `codewhale config set` key, and which config.toml keys
//! nothing reads (#6563).
//!
//! `config set` used to write any key it did not recognise into config.toml,
//! so `config set calm_mode flase` and `config set totally_bogus_key 42` both
//! exited 0 and changed nothing that runs: `calm_mode` lives in settings.toml.
//! This module answers the one question both `config set` and `config doctor`
//! need — is this key read from config.toml, from settings.toml, or by
//! nothing — from the structs that actually read the files, so the answer
//! cannot drift from a hand-kept list.

use std::path::PathBuf;

use anyhow::Result;
use codewhale_config::ConfigToml;
use codewhale_config::settings_schema::SETTINGS_SCHEMA;

use crate::config::Config;
use crate::settings::Settings;

/// Root keys read from config.toml outside both the TUI [`Config`] struct and
/// the dispatcher's [`ConfigToml`] typed fields. Each has a named reader:
/// profile overlays (`ConfigFile::profiles`), per-project trust
/// (`config::project_trust_*`), user workspace entries
/// (`merge_user_workspace_config_from_doc` reads `[workspace.'<path>']` and
/// the legacy `[projects]` spelling), the route-preference migration
/// (`config_persistence`), the MCP stdio dispatcher's literal JSON key, the
/// stream-timeout fallbacks in `ConfigToml::stream_chunk_timeout_secs`, and
/// the legacy top-level `base_url` / `api_key`, which
/// `codewhale_config::legacy_root` moves into `[providers.<name>]` (#6394).
const OTHER_READER_ROOT_KEYS: &[&str] = &[
    "profiles",
    "base_url",
    "baseUrl",
    "api_key",
    "apiKey",
    "projects",
    "workspace",
    "route_preferences_version",
    "route_preferences_migration",
    "mcp.server_definitions",
    "stream_chunk_timeout_secs",
    "tui.stream_chunk_timeout_secs",
];

/// [`OTHER_READER_ROOT_KEYS`] whose value is a table of user entries
/// (`[profiles.<name>]`, `[projects.'<path>']`, `[workspace.'<path>']`).
/// `config doctor` counts them as read, but `config set <key> <value>` would
/// replace the whole table with a string, so a scalar write is refused.
const TABLE_ROOT_KEYS: &[&str] = &["profiles", "projects", "workspace"];

/// Where a key a user asked `config set` to write is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKeyHome {
    /// config.toml: a typed field, a table something deserializes, or a
    /// dotted path whose own validation lives in `ConfigToml::set_value`.
    ConfigToml,
    /// settings.toml: a key `Settings::set` accepts.
    SettingsToml,
    /// Nothing reads it.
    Unknown,
}

/// Classify `key` for `codewhale config set`. config.toml wins when both
/// files know a spelling (`sandbox_mode`, `reasoning_effort`), which keeps
/// every key that already took effect writing where it did before.
#[must_use]
pub fn config_key_home(key: &str) -> ConfigKeyHome {
    let key = key.trim();
    if key.contains('.') || is_config_toml_root_key(key) {
        ConfigKeyHome::ConfigToml
    } else if Settings::canonical_key(key).is_some() {
        ConfigKeyHome::SettingsToml
    } else {
        ConfigKeyHome::Unknown
    }
}

/// Whether some reader consumes `key` at the root of config.toml.
fn is_config_toml_root_key(key: &str) -> bool {
    tui_config_fields().contains(&key)
        || OTHER_READER_ROOT_KEYS.contains(&key)
        || is_config_toml_typed_field(key)
}

/// Root field names of the TUI [`Config`], taken from serde's own field table
/// (renames and aliases included) rather than restated here.
fn tui_config_fields() -> &'static [&'static str] {
    static FIELDS: std::sync::OnceLock<&'static [&'static str]> = std::sync::OnceLock::new();
    FIELDS.get_or_init(struct_fields::<Config>)
}

/// Whether [`ConfigToml`] deserializes `key` into a typed field. Its unknown
/// keys flatten into `extras`, so a probe document either fails to type-check
/// against the field or lands somewhere other than `extras`; both mean a
/// typed field owns the key.
fn is_config_toml_typed_field(key: &str) -> bool {
    let mut probe = toml::Table::new();
    probe.insert(key.to_string(), toml::Value::Integer(0));
    match toml::Value::Table(probe).try_into::<ConfigToml>() {
        Ok(config) => !config.extras.contains_key(key),
        Err(_) => true,
    }
}

/// The field list a derived `Deserialize` hands to `deserialize_struct`.
/// Returns an empty slice for types that deserialize any other way.
fn struct_fields<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
    struct Probe(Option<&'static [&'static str]>);

    impl<'de> serde::Deserializer<'de> for &mut Probe {
        type Error = serde::de::value::Error;

        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            _visitor: V,
        ) -> Result<V::Value, Self::Error> {
            Err(serde::de::Error::custom("field probe"))
        }

        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self,
            _name: &'static str,
            fields: &'static [&'static str],
            _visitor: V,
        ) -> Result<V::Value, Self::Error> {
            self.0 = Some(fields);
            Err(serde::de::Error::custom("field probe"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map enum identifier ignored_any
        }
    }

    let mut probe = Probe(None);
    let _ = T::deserialize(&mut probe);
    probe.0.unwrap_or(&[])
}

/// Every key `config set` can write, for did-you-mean: the declared settings
/// (`SETTINGS_SCHEMA`) that resolve to a file, plus config.toml's own roots.
fn settable_keys() -> impl Iterator<Item = &'static str> {
    SETTINGS_SCHEMA
        .iter()
        .map(|def| def.key)
        .filter(|key| {
            codewhale_config::notifications::in_namespace(key)
                || key.starts_with("stream.")
                || (!key.contains('.') && config_key_home(key) != ConfigKeyHome::Unknown)
        })
        // serde's field table carries the camelCase aliases too; suggest the
        // documented snake_case spelling only.
        .chain(
            tui_config_fields()
                .iter()
                .copied()
                .filter(|key| !key.chars().any(|c| c.is_ascii_uppercase())),
        )
        .chain(
            OTHER_READER_ROOT_KEYS
                .iter()
                .copied()
                .filter(|key| !key.contains('.') && !TABLE_ROOT_KEYS.contains(key)),
        )
}

/// The closest of `candidates` to `query`, if any is close enough to suggest.
pub fn nearest_key<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    candidates
        .into_iter()
        .filter_map(|candidate| {
            codewhale_config::suggest::best_suggestion_score(query, [candidate])
                .map(|score| (score, candidate))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, candidate)| candidate)
}

/// The refusal `config set` gives for a key nothing reads.
#[must_use]
pub fn unknown_config_key_message(key: &str) -> String {
    let hint = nearest_key(key, settable_keys())
        .map(|candidate| format!(" Did you mean `{candidate}`?"))
        .unwrap_or_default();
    format!(
        "unknown config key `{key}`: nothing reads it, so it was not saved.{hint} \
         Run `codewhale config list` for config.toml keys or `/settings text` for settings."
    )
}

/// Effective stream values for dispatcher diagnostics, resolved by the same
/// Config accessors as Engine/client construction. Start from the unredacted
/// document to parse its types; return only this non-secret table. The caller
/// keeps the existing redaction path for every other config value.
pub fn resolved_stream_config(config: &ConfigToml) -> Result<toml::Value> {
    let document = toml::Value::try_from(config)?;
    // Parse only the two owning tables: dispatcher-specific root values must
    // not change their existing dump behavior just to inspect transport.
    let mut transport = toml::Table::new();
    for key in ["stream", "tui"] {
        if let Some(value) = document.get(key) {
            transport.insert(key.to_string(), value.clone());
        }
    }
    let config: Config = toml::Value::Table(transport).try_into()?;
    Ok(toml::Value::try_from(config.resolved_stream_settings())?)
}

/// Write one settings.toml key through the same validator and locked
/// transaction as `/settings`. Returns the file written.
pub fn set_settings_value(key: &str, value: &str) -> Result<PathBuf> {
    Settings::transact(|settings| settings.set(key, value))?;
    Settings::path()
}

/// The saved settings.toml value for `key`, without terminal or environment
/// overlays, so `config get` reports what `config set` wrote.
pub fn settings_value(key: &str) -> Result<Option<String>> {
    Ok(Settings::load_persisted()?.value(key))
}

/// The canonical settings.toml keys: each declared setting that `/set`
/// accepts under its own name. Internal flags, actions, receipts and retired
/// schema defs are not settable and are left out, so `/config <key>` and its
/// did-you-mean only ever name a key a user can change.
pub fn settings_toml_keys() -> impl Iterator<Item = &'static str> {
    SETTINGS_SCHEMA
        .iter()
        .map(|def| def.key)
        .filter(|key| Settings::canonical_key(key) == Some(*key))
}

/// The TOML value `codewhale config set` stores for a config.toml root key
/// whose reader needs more than `ConfigToml::set_value`'s string fallthrough,
/// or `Ok(None)` when that fallthrough is already right.
///
/// - `approval_policy`, `sandbox_mode` and `verbosity` are refused unless
///   the value is one [`codewhale_config::config_toml_choices`] names, since
///   the TUI loader rejects the whole file otherwise.
/// - `reasoning_effort` is checked against its reader,
///   [`codewhale_config::settings_schema::canonical_reasoning_effort`], and stored in canonical spelling.
/// - A root field the TUI `Config` reads but `SETTINGS_SCHEMA` does not
///   declare (`yolo`, `max_subagents`, `mcp_oauth_callback_port`, ...) is
///   typed by that field's own deserializer: a string in a boolean or
///   integer field fails the TUI's strict parse of the whole file, so the
///   value is stored as the first of string, boolean, integer or number the
///   field accepts, and refused when it accepts none.
///
pub fn config_toml_value(key: &str, value: &str) -> Result<Option<toml::Value>> {
    let key = key.trim();
    if TABLE_ROOT_KEYS.contains(&key) {
        anyhow::bail!(
            "`{key}` is a table of entries, so `config set {key}` would replace all of \
             them; nothing was saved. Edit the [{key}.<name>] table in config.toml instead."
        );
    }
    codewhale_config::check_config_toml_choice(key, value)?;
    if key == "reasoning_effort" {
        let effort = codewhale_config::settings_schema::canonical_reasoning_effort(value)
            .map_err(|error| anyhow::anyhow!("invalid value for '{key}': {error}"))?;
        return Ok(Some(toml::Value::String(effort.to_string())));
    }
    if key.contains('.')
        || codewhale_config::setting(key).is_some()
        || !tui_config_fields().contains(&key)
        || is_config_toml_typed_field(key)
    {
        return Ok(None);
    }
    let text = toml::Value::String(value.to_string());
    if tui_config_accepts(key, &text) {
        return Ok(None);
    }
    let trimmed = value.trim();
    let candidates = [
        crate::settings::parse_bool(trimmed)
            .ok()
            .map(toml::Value::Boolean),
        trimmed.parse::<i64>().ok().map(toml::Value::Integer),
        trimmed
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .map(toml::Value::Float),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|candidate| tui_config_accepts(key, candidate))
        .map(Some)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "invalid value '{value}' for '{key}': its config.toml reader does not accept \
                 it. Edit `{key}` in config.toml with a TOML value of the documented type. \
                 No value was changed."
            )
        })
}

/// Whether the TUI [`Config`] deserializes `value` at root key `key`.
fn tui_config_accepts(key: &str, value: &toml::Value) -> bool {
    let mut probe = toml::Table::new();
    probe.insert(key.to_string(), value.clone());
    toml::Value::Table(probe).try_into::<Config>().is_ok()
}

/// One line per config.toml root key that nothing reads, for `config doctor`.
/// A settings.toml key is named as misplaced so the fix is obvious.
#[must_use]
pub fn unread_config_keys<'a>(root_keys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    root_keys
        .into_iter()
        .filter(|key| !is_config_toml_root_key(key))
        .map(|key| {
            if Settings::canonical_key(key).is_some() {
                format!(
                    "`{key}` belongs in settings.toml; config.toml's copy is never read \
                     (move it: `codewhale config set {key} <value>`, then \
                     `codewhale config unset {key}`)"
                )
            } else {
                let hint = nearest_key(key, settable_keys())
                    .map(|candidate| format!("; did you mean `{candidate}`?"))
                    .unwrap_or_default();
                format!("`{key}` is not read by anything{hint}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_key_home_routes_by_reader() {
        // settings.toml keys, including an alias `Settings::set` accepts.
        assert_eq!(config_key_home("calm_mode"), ConfigKeyHome::SettingsToml);
        assert_eq!(config_key_home("calm"), ConfigKeyHome::SettingsToml);
        assert_eq!(
            config_key_home("tool_collapse"),
            ConfigKeyHome::SettingsToml
        );
        // config.toml: dispatcher-typed, TUI-typed, and other named readers.
        for key in [
            "provider",
            "api_key",
            "verbosity",
            "log_level",
            "hook_sinks",
            "allow_shell",
            "max_subagents",
            "skills_dir",
            "tui",
            "features",
            "profiles",
            "projects",
            "stream_chunk_timeout_secs",
        ] {
            assert_eq!(config_key_home(key), ConfigKeyHome::ConfigToml, "{key}");
        }
        // Both files know these spellings; config.toml keeps winning.
        assert_eq!(config_key_home("sandbox_mode"), ConfigKeyHome::ConfigToml);
        assert_eq!(
            config_key_home("reasoning_effort"),
            ConfigKeyHome::ConfigToml
        );
        // Dotted keys keep their validation in `ConfigToml::set_value`.
        assert_eq!(
            config_key_home("providers.deepseek.model"),
            ConfigKeyHome::ConfigToml
        );
        assert_eq!(config_key_home("totally_bogus_key"), ConfigKeyHome::Unknown);
    }

    #[test]
    fn every_shipped_example_root_key_is_read_by_something() {
        let example: toml::Table =
            toml::from_str(include_str!("../../../config.example.toml")).expect("example parses");
        let unread = unread_config_keys(example.keys().map(String::as_str));
        assert!(unread.is_empty(), "{unread:#?}");
    }

    #[test]
    fn unknown_config_key_message_suggests_from_current_keys() {
        let message = unknown_config_key_message("calm_mod");
        assert!(message.contains("Did you mean `calm_mode`?"), "{message}");
        assert!(message.contains("not saved"), "{message}");

        let message = unknown_config_key_message("zzqqxxyy");
        assert!(!message.contains("Did you mean"), "{message}");
    }

    #[test]
    fn config_toml_value_types_fields_by_their_reader() {
        // Typed TUI fields the schema does not declare get their field's type.
        assert_eq!(
            config_toml_value("yolo", "true").unwrap(),
            Some(toml::Value::Boolean(true))
        );
        assert_eq!(
            config_toml_value("strict_tool_mode", "off").unwrap(),
            Some(toml::Value::Boolean(false))
        );
        assert_eq!(
            config_toml_value("max_subagents", " 4 ").unwrap(),
            Some(toml::Value::Integer(4))
        );
        assert_eq!(
            config_toml_value("mcp_oauth_callback_port", "8765").unwrap(),
            Some(toml::Value::Integer(8765))
        );
        // A value the field cannot hold is refused, not stored as text.
        for (key, value) in [
            ("yolo", "flase"),
            ("max_subagents", "lots"),
            ("mcp_oauth_callback_port", "70000"),
        ] {
            let error = config_toml_value(key, value).expect_err(key);
            assert!(
                format!("{error:#}").contains(&format!("invalid value '{value}' for '{key}'")),
                "{error:#}"
            );
        }
        // String fields, schema-declared keys and dotted keys keep
        // `ConfigToml::set_value`.
        assert_eq!(
            config_toml_value("skills_dir", "/tmp/skills").unwrap(),
            None
        );
        assert_eq!(config_toml_value("allow_shell", "on").unwrap(), None);
        assert_eq!(
            config_toml_value("providers.deepseek.model", "x").unwrap(),
            None
        );

        // `reasoning_effort` accepts its reader's aliases, canonicalized.
        for (alias, canonical) in [("none", "off"), ("mid", "medium"), ("maximum", "max")] {
            assert_eq!(
                config_toml_value("reasoning_effort", alias).unwrap(),
                Some(toml::Value::String(canonical.into())),
                "{alias}"
            );
        }
        assert!(config_toml_value("reasoning_effort", "bogus").is_err());
    }

    #[test]
    fn config_toml_value_refuses_values_the_loader_rejects() {
        for (key, value) in [
            ("approval_policy", " On-Request "),
            ("approval_policy", "never"),
            ("sandbox_mode", "workspace-write"),
            ("verbosity", "concise"),
        ] {
            assert_eq!(
                config_toml_value(key, value).unwrap(),
                None,
                "{key}={value}"
            );
        }
        for (key, value, fix) in [
            ("approval_policy", "ask", "on-request"),
            ("sandbox_mode", "full", "read-only"),
            ("verbosity", "quiet", "normal"),
        ] {
            let error = format!("{:#}", config_toml_value(key, value).expect_err(key));
            assert!(
                error.contains(&format!("invalid value '{value}' for '{key}'")),
                "{error}"
            );
            assert!(
                error.contains(&format!("fix: codewhale config set {key} {fix}")),
                "{error}"
            );
        }
        let error = format!(
            "{:#}",
            config_toml_value("approval_policy", "ask").unwrap_err()
        );
        assert!(error.contains("on-request, untrusted, never"), "{error}");
        assert!(error.contains("settings.toml"), "{error}");
    }

    #[test]
    fn settings_toml_keys_are_user_settable_only() {
        let keys: Vec<&str> = settings_toml_keys().collect();
        assert!(keys.contains(&"calm_mode"), "{keys:?}");
        assert!(keys.contains(&"auto_compact"), "{keys:?}");
        for internal in [
            "feature_intro_shown",
            "yolo_deprecation_shown",
            "mcp_open",
            "fast_model",
            "effective_auto_compact",
        ] {
            assert!(!keys.contains(&internal), "{internal} is not settable");
        }
    }

    #[test]
    fn scalar_set_of_a_table_root_key_is_refused() {
        // `config set workspace /path` used to store `workspace = "/path"`,
        // replacing every `[workspace.'<path>']` entry on save.
        for key in TABLE_ROOT_KEYS {
            assert_eq!(config_key_home(key), ConfigKeyHome::ConfigToml, "{key}");
            let error = config_toml_value(key, "/some/path").expect_err(key);
            assert!(error.to_string().contains("nothing was saved"), "{error}");
        }
    }

    #[test]
    fn config_set_and_loader_accept_the_same_closed_vocabulary() {
        for (key, value, valid) in [
            ("approval_policy", " ON-REQUEST ", true),
            ("approval_policy", "ask", false),
            ("verbosity", " CONCISE ", true),
            ("verbosity", "quiet", false),
            ("sandbox_mode", " WORKSPACE-WRITE ", true),
            ("sandbox_mode", "full", false),
        ] {
            let mut config = crate::config::Config::default();
            match key {
                "approval_policy" => config.approval_policy = Some(value.to_string()),
                "verbosity" => config.verbosity = Some(value.to_string()),
                "sandbox_mode" => config.sandbox_mode = Some(value.to_string()),
                _ => unreachable!(),
            }
            assert_eq!(config_toml_value(key, value).is_ok(), valid, "{key}");
            assert_eq!(config.validate().is_ok(), valid, "{key}");
        }
    }

    #[test]
    fn loader_choice_errors_redact_pasted_keys_but_keep_ordinary_typos() {
        let token = ["sk-live-", "Z7qX4mNb2Vc9Lk3PwR8t"].concat();
        for (key, correction) in [
            ("approval_policy", "on-request"),
            ("verbosity", "normal"),
            ("sandbox_mode", "workspace-write"),
        ] {
            for value in [token.as_str(), "misspelled-choice"] {
                let mut config = crate::config::Config::default();
                match key {
                    "approval_policy" => config.approval_policy = Some(value.to_string()),
                    "verbosity" => config.verbosity = Some(value.to_string()),
                    "sandbox_mode" => config.sandbox_mode = Some(value.to_string()),
                    _ => unreachable!(),
                }
                let error = config.validate().expect_err("invalid choice");
                let diagnostic = crate::config::SafeConfigDiagnostic::find_in(&error)
                    .expect("shared choice validation keeps the structured diagnostic");
                let shareable = diagnostic.display_message();
                assert!(shareable.starts_with(&format!("Invalid {key} (value not shown):")));
                assert!(!shareable.contains(value), "{shareable}");
                let fix = diagnostic
                    .fix()
                    .expect("invalid choices keep a recovery action");
                assert!(
                    fix.starts_with(&format!("codewhale config set {key} {correction} (if ")),
                    "{fix}"
                );
                assert!(
                    fix.contains("profile") && fix.contains("managed config"),
                    "{fix}"
                );
                if key != "verbosity" {
                    assert!(
                        fix.contains(&format!("CODEWHALE_{}", key.to_ascii_uppercase())),
                        "{fix}"
                    );
                }
                let error = error.to_string();
                assert!(!error.contains(&token));
                assert!(error.contains("expected"));
                assert!(error.contains(if value == token {
                    "[redacted]"
                } else {
                    "misspelled-choice"
                }));
            }
        }
    }

    #[test]
    fn documented_user_workspace_entry_is_read() {
        // The block docs/CONFIGURATION.md gives under "User workspace entries".
        let documented: toml::Table =
            toml::from_str("[workspace.'/absolute/path/to/project']\nallow_shell = true\n")
                .expect("documented block parses");
        let unread = unread_config_keys(documented.keys().map(String::as_str));
        assert!(unread.is_empty(), "{unread:#?}");
    }

    #[test]
    fn unread_config_keys_names_misplaced_settings_and_unknown_keys() {
        let findings = unread_config_keys(["verbosity", "calm_mode", "totally_bogus_key", "tui"]);
        assert_eq!(findings.len(), 2, "{findings:#?}");
        assert!(
            findings[0].contains("`calm_mode` belongs in settings.toml"),
            "{findings:#?}"
        );
        assert!(
            findings[1].contains("`totally_bogus_key` is not read by anything"),
            "{findings:#?}"
        );
    }
    #[test]
    fn stream_dump_uses_runtime_resolution_without_exposing_other_config() {
        let _env = crate::test_support::lock_test_env();
        let _open =
            crate::test_support::EnvVarGuard::set("CODEWHALE_STREAM_OPEN_TIMEOUT_SECS", "88");
        let _pin = crate::test_support::EnvVarGuard::set("CODEWHALE_FORCE_HTTP1", "1");
        let config: ConfigToml = toml::from_str("[providers.deepseek]\napi_key='fixture-secret'\n[tui]\nstream_max_resumes=6\n[stream]\nhttp2_keep_alive_timeout_secs=0\n").unwrap();
        let stream = resolved_stream_config(&config).unwrap();
        assert_eq!(stream["open_timeout_secs"].as_integer(), Some(88));
        assert_eq!(stream["max_resumes"].as_integer(), Some(6));
        assert_eq!(stream["force_http1"].as_bool(), Some(true));
        assert_eq!(
            stream["http2_keep_alive_timeout_secs"].as_integer(),
            Some(20)
        );
        assert!(!stream.to_string().contains("fixture-secret"));
        assert!(unread_config_keys(["stream"]).is_empty());
        for key in stream.as_table().unwrap().keys() {
            assert!(
                codewhale_config::setting(&format!("stream.{key}")).is_some(),
                "{key}"
            );
        }
        assert_eq!(
            config.extras["stream"].as_table().unwrap().len(),
            1,
            "dump never persists resolved defaults"
        );
    }
}
