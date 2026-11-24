//! Model-bound redaction opt-out (`[redaction] model_bound`).
//!
//! Codewhale masks credential-looking values in tool output before it is sent
//! to an upstream model (the "model boundary"). That masking is a security
//! backstop: a file read by a tool can contain a configured API key, a bare
//! provider token, or a credential-shaped opaque string, and the model must
//! never see those bytes.
//!
//! This module adds a deliberate, documented way to turn that masking off for
//! users who must edit files that contain real credentials. Because it lowers
//! a security boundary, it is not a plain boolean:
//!
//! * Setting `[redaction] model_bound = "disabled"` in `config.toml` only
//!   records a *request*.
//! * The request takes effect only after a restart of the interactive TUI and
//!   an explicit confirmation on the startup gate screen, which persists a
//!   receipt next to the config file actually loaded by that launch.
//! * Non-interactive entry points (`codewhale exec`, hooks, automation) never
//!   confirm anything; as long as no confirmation receipt exists they resolve
//!   to the safe default (`Enabled`), whatever the config file says.
//! * Dismissing the gate (choosing "keep masking on") leaves the config field
//!   and the receipt untouched, so the next launch asks again until the user
//!   confirms or edits the field back to `"enabled"`.
//!
//! A confirmation receipt is bound to the loaded config file, including an
//! explicit `--config` or `CODEWHALE_CONFIG_PATH`. Different config filenames
//! have independent receipts. A receipt is honored only while the canonical
//! path, contents and modification time still match and the config requests
//! `"disabled"`.
//! Editing the field back to `"enabled"` - or changing `config.toml` in any
//! way - and later re-requesting `"disabled"` always asks for a fresh
//! confirmation, even when no process ran in between.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Name of the confirmation-receipt file, stored next to `config.toml` in the
/// Codewhale home directory.
pub const MODEL_BOUND_STATE_FILE_NAME: &str = "redaction-state.json";

/// Whether credential-shaped values are masked at the model boundary.
///
/// Parsing is deliberately forgiving on the way in — the config value is a
/// security switch and users reach for boolean spellings — so `true`/`false`,
/// `"on"`/`"off"`, and `"enabled"`/`"disabled"` (any casing) all resolve to
/// the same two states. Serialization always writes the canonical
/// `"enabled"` / `"disabled"` words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelBoundMasking {
    /// Mask credential-shaped tool output before it reaches the model (default).
    #[default]
    Enabled,
    /// Let the model see the raw bytes of tool output, credentials included.
    /// Only effective after an explicit startup confirmation (see the module
    /// docs); until then it resolves to [`ModelBoundMasking::Enabled`].
    Disabled,
}

impl ModelBoundMasking {
    pub fn is_disabled(self) -> bool {
        self == Self::Disabled
    }
}

impl<'de> serde::Deserialize<'de> for ModelBoundMasking {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Flag(bool),
            Word(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Flag(true) => Ok(ModelBoundMasking::Enabled),
            Raw::Flag(false) => Ok(ModelBoundMasking::Disabled),
            Raw::Word(word) => match word.to_ascii_lowercase().as_str() {
                "enabled" | "on" | "true" => Ok(ModelBoundMasking::Enabled),
                "disabled" | "off" | "false" => Ok(ModelBoundMasking::Disabled),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["enabled", "disabled", "on", "off", "true", "false"],
                )),
            },
        }
    }
}

/// The `[redaction]` table of `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactionToml {
    /// Model-bound masking policy: `"enabled"` (default) or `"disabled"`.
    /// Boolean spellings are also accepted: `false` / `"off"` mean the same
    /// as `"disabled"`, and `true` / `"on"` mean `"enabled"`.
    ///
    /// A `"disabled"` request is honored only after a TUI restart and a one-time
    /// confirmation on the startup gate; see the module documentation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_bound: Option<ModelBoundMasking>,
}

impl RedactionToml {
    /// The requested masking mode, defaulting to [`ModelBoundMasking::Enabled`].
    pub fn model_bound_masking(&self) -> ModelBoundMasking {
        self.model_bound.unwrap_or_default()
    }
}

/// Receipt belonging to this exact config file. Keep the established name
/// for config.toml; other filenames get independent receipts even when they
/// live in the same directory and contain identical configuration.
pub fn model_bound_state_path(config_path: &Path) -> PathBuf {
    // --config and environment resolution may spell the same file through
    // different symlinks (for example /var and /private/var on macOS).
    let resolved = config_path.canonicalize().ok();
    let config_path = resolved.as_deref().unwrap_or(config_path);
    if config_path.file_name() == Some(std::ffi::OsStr::new(crate::CONFIG_FILE_NAME)) {
        config_path.with_file_name(MODEL_BOUND_STATE_FILE_NAME)
    } else {
        let identity: String = Sha256::digest(config_path.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        config_path.with_file_name(format!("redaction-state-{identity}.json"))
    }
}

/// Clear only this config's confirmation. A stale receipt is rejected even
/// when filesystem errors prevent this best-effort sweep.
pub fn clear_model_bound_disabled_confirmation(config_path: &Path) -> io::Result<()> {
    let path = model_bound_state_path(config_path);
    for attempt in 0..6 {
        match write_state(&path, config_path, false) {
            Ok(()) => {
                let _ = fs::remove_file(&path);
                return Ok(());
            }
            Err(_) if attempt < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!()
}

/// Persist consent for the config that was actually loaded by the caller.
/// Never infer that source from the receipt directory or the process home.
pub fn record_model_bound_disabled_confirmation(config_path: &Path) -> io::Result<PathBuf> {
    let path = model_bound_state_path(config_path);
    write_state(&path, config_path, true)?;
    Ok(path)
}

pub fn confirmation_required(desired: ModelBoundMasking, config_path: Option<&Path>) -> bool {
    desired.is_disabled() && !confirmed_for_current_request(desired, config_path)
}

/// Unloaded, unreadable, changed and unconfirmed requests remain masked.
pub fn effective_masking(
    desired: ModelBoundMasking,
    config_path: Option<&Path>,
) -> ModelBoundMasking {
    if confirmed_for_current_request(desired, config_path) {
        ModelBoundMasking::Disabled
    } else {
        ModelBoundMasking::Enabled
    }
}

fn confirmed_for_current_request(desired: ModelBoundMasking, config_path: Option<&Path>) -> bool {
    let Some(config_path) = config_path else {
        return false;
    };
    let receipt = read_state(&model_bound_state_path(config_path));
    if !receipt.model_bound_disabled_confirmed {
        return false;
    }
    let current = config_binding(config_path).ok();
    if !desired.is_disabled() || current.is_none() || current != receipt.config_binding {
        let _ = clear_model_bound_disabled_confirmation(config_path);
        return false;
    }
    true
}

// === State-file plumbing (path-parameterized so tests stay hermetic) ===

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct StateFile {
    model_bound_disabled_confirmed: bool,
    config_binding: Option<ConfigBinding>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ConfigBinding {
    config_path: PathBuf,
    sha256: [u8; 32],
    modified: std::time::SystemTime,
}

fn config_binding(config_path: &Path) -> io::Result<ConfigBinding> {
    let config_path = config_path.canonicalize()?;
    let file = fs::File::open(&config_path)?;
    let modified = file.metadata()?.modified()?;
    let body = crate::path_io::read_bounded_file_to_string(
        &file,
        &config_path,
        crate::path_io::MAX_CONFIG_FILE_BYTES,
        "config file",
    )?;
    let config: crate::ConfigToml = toml::from_str(&body)
        .map_err(|_| io::Error::other("cannot confirm an unreadable redaction config"))?;
    if !config.redaction_model_bound_masking().is_disabled() {
        return Err(io::Error::other(
            "config does not request disabling model-bound masking",
        ));
    }
    Ok(ConfigBinding {
        config_path,
        sha256: Sha256::digest(body.as_bytes()).into(),
        modified,
    })
}

fn read_state(path: &std::path::Path) -> StateFile {
    // A receipt is something this process wrote. A link in its place is never
    // read through, so it cannot vouch for an opt-out; unconfirmed is the
    // safe answer.
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return StateFile::default();
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

fn write_state(path: &Path, config_path: &Path, confirmed: bool) -> io::Result<()> {
    let body = serde_json::to_string_pretty(&StateFile {
        model_bound_disabled_confirmed: confirmed,
        config_binding: if confirmed {
            Some(config_binding(config_path)?)
        } else {
            None
        },
    })
    .map_err(io::Error::other)?;
    // Owner-only (0600 on Unix) through a temporary file renamed into place:
    // the receipt's own name is replaced, never written through, so a link
    // planted there cannot redirect the write.
    crate::persistence::atomic_write(path, body.as_bytes())
        .map_err(|error| io::Error::other(format!("{error:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn state_path(tmp: &Path) -> PathBuf {
        tmp.join(MODEL_BOUND_STATE_FILE_NAME)
    }

    #[test]
    fn absent_state_is_not_confirmed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!read_state(&state_path(tmp.path())).model_bound_disabled_confirmed);
    }

    /// The receipt is written owner-only, never through a link planted at its
    /// name, and carries a path, a digest and a timestamp, not config text.
    #[test]
    fn receipt_is_private_never_written_through_a_link_and_holds_no_config_text() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join(crate::CONFIG_FILE_NAME);
        let secret = "sk-redaction-receipt-marker-0123456789abcdef";
        fs::write(
            &config_path,
            format!("api_key = \"{secret}\"\n[redaction]\nmodel_bound = \"disabled\"\n"),
        )
        .expect("write config");

        // Plant a link where the receipt will go.
        #[cfg(unix)]
        let victim = {
            let victim = tmp.path().join("victim.txt");
            fs::write(&victim, "keep me").expect("victim");
            std::os::unix::fs::symlink(&victim, model_bound_state_path(&config_path))
                .expect("plant link");
            assert!(
                !read_state(&model_bound_state_path(&config_path)).model_bound_disabled_confirmed
            );
            victim
        };

        let receipt = record_model_bound_disabled_confirmation(&config_path).expect("record");
        let body = fs::read_to_string(&receipt).expect("receipt");
        assert!(body.contains("\"sha256\""), "{body}");
        assert!(
            !body.contains(secret),
            "a receipt never carries config text"
        );
        assert!(
            !body.contains("api_key"),
            "a receipt never carries config text"
        );
        assert!(read_state(&receipt).model_bound_disabled_confirmed);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
            let metadata = fs::symlink_metadata(&receipt).unwrap();
            assert!(!metadata.file_type().is_symlink(), "the link was replaced");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn confirmation_round_trips_through_the_state_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = state_path(tmp.path());
        fs::write(
            tmp.path().join(crate::CONFIG_FILE_NAME),
            "[redaction]\nmodel_bound = \"disabled\"\n",
        )
        .expect("write config");
        write_state(&path, &tmp.path().join(crate::CONFIG_FILE_NAME), true).expect("write state");
        assert!(read_state(&path).model_bound_disabled_confirmed);
    }

    /// The disable-and-confirm lifecycle belongs to one explicit config.
    #[test]
    fn loaded_path_lifecycle_requires_confirmation_before_disabling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join(crate::CONFIG_FILE_NAME);
        assert!(!confirmed_for_current_request(
            ModelBoundMasking::Disabled,
            Some(&config_path)
        ));

        let desired = ModelBoundMasking::Disabled;
        assert!(confirmation_required(desired, Some(&config_path)));
        assert_eq!(
            effective_masking(desired, Some(&config_path)),
            ModelBoundMasking::Enabled
        );

        assert!(
            record_model_bound_disabled_confirmation(&config_path).is_err(),
            "missing config cannot authorize an opt-out"
        );
        let config_path = tmp.path().join(crate::CONFIG_FILE_NAME);
        let disabled_config = "[redaction]\nmodel_bound = \"disabled\"\n";
        fs::write(&config_path, disabled_config).expect("write disabled config");
        let written = record_model_bound_disabled_confirmation(&config_path).expect("record");
        assert_eq!(
            written,
            tmp.path()
                .canonicalize()
                .unwrap()
                .join(MODEL_BOUND_STATE_FILE_NAME)
        );
        assert!(confirmed_for_current_request(
            ModelBoundMasking::Disabled,
            Some(&config_path)
        ));
        assert!(!confirmation_required(desired, Some(&config_path)));
        assert_eq!(
            effective_masking(desired, Some(&config_path)),
            ModelBoundMasking::Disabled
        );

        // Windows real-time AV scanning can hold a short exclusive lock on a
        // file we just wrote; the confirm -> re-enable sweep below rewrites
        // that same file back-to-back, which is exactly the lock window. The
        // product flow never does this (record and sweep happen on different
        // launches), so back off briefly here to keep the test deterministic
        // on Defender-equipped machines.
        std::thread::sleep(std::time::Duration::from_millis(300));

        // An enabled request never disables, even with a receipt on disk -
        // and going back to enabled invalidates the receipt, so the next
        // disabled request must be confirmed again.
        let enabled = ModelBoundMasking::Enabled;
        assert!(!confirmation_required(enabled, Some(&config_path)));
        assert_eq!(
            effective_masking(enabled, Some(&config_path)),
            ModelBoundMasking::Enabled
        );
        clear_model_bound_disabled_confirmation(&config_path)
            .expect("explicit clear must succeed after re-enabling");
        assert!(
            !confirmed_for_current_request(ModelBoundMasking::Disabled, Some(&config_path)),
            "returning to enabled must clear the confirmation receipt"
        );

        // Re-disabling after an enabled period asks again from scratch.
        assert!(confirmation_required(desired, Some(&config_path)));
        assert_eq!(
            effective_masking(desired, Some(&config_path)),
            ModelBoundMasking::Enabled
        );

        // The receipt is bound to the config it was made against: rewriting
        // config.toml after a fresh confirmation (an enabled -> disabled
        // round trip with zero processes in between) must invalidate it too.
        record_model_bound_disabled_confirmation(&config_path).expect("record again");
        assert!(!confirmation_required(desired, Some(&config_path)));
        // Ensure config.toml is strictly newer than the receipt before the
        // rewrite check runs.
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(
            tmp.path().join(crate::CONFIG_FILE_NAME),
            "[redaction]\nmodel_bound = \"disabled\"\n",
        )
        .expect("touch config after receipt");
        assert!(
            confirmation_required(desired, Some(&config_path)),
            "a config rewritten after the receipt must force a fresh confirmation"
        );
        assert_eq!(
            effective_masking(desired, Some(&config_path)),
            ModelBoundMasking::Enabled
        );

        record_model_bound_disabled_confirmation(&config_path).expect("confirm readable config");
        let original_mtime = fs::metadata(&config_path).unwrap().modified().unwrap();
        fs::write(&config_path, format!("{disabled_config}# changed\n")).unwrap();
        fs::File::options()
            .write(true)
            .open(&config_path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        assert_eq!(
            effective_masking(desired, Some(&config_path)),
            ModelBoundMasking::Enabled,
            "changed bytes with preserved timestamps must invalidate confirmation"
        );

        record_model_bound_disabled_confirmation(&config_path).expect("confirm updated config");
        fs::remove_file(&config_path).unwrap();
        assert_eq!(
            effective_masking(desired, Some(&config_path)),
            ModelBoundMasking::Enabled,
            "missing config metadata must fail closed"
        );
        fs::write(&config_path, disabled_config).unwrap();
        fs::write(&written, r#"{"model_bound_disabled_confirmed":true}"#).unwrap();
        assert_eq!(
            effective_masking(desired, Some(&config_path)),
            ModelBoundMasking::Enabled,
            "legacy receipts without a config binding cannot authorize the opt-out"
        );
        fs::write(&config_path, "[redaction]\nmodel_bound = \"enabled\"\n").unwrap();
        assert!(
            record_model_bound_disabled_confirmation(&config_path).is_err(),
            "a loaded disabled request cannot confirm an enabled file on disk"
        );
    }

    #[test]
    fn custom_configs_cannot_borrow_or_erase_each_others_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let default = temp.path().join("config.toml");
        let custom = temp.path().join("work.toml");
        let other = temp.path().join("personal.toml");
        for path in [&default, &custom, &other] {
            fs::write(path, "[redaction]\nmodel_bound = \"disabled\"\n").unwrap();
        }
        let desired = ModelBoundMasking::Disabled;
        record_model_bound_disabled_confirmation(&default).unwrap();
        assert!(confirmation_required(desired, Some(&custom)));
        record_model_bound_disabled_confirmation(&custom).unwrap();
        assert!(!confirmation_required(desired, Some(&custom)));
        assert!(confirmation_required(desired, Some(&other)));
        assert_ne!(
            model_bound_state_path(&custom),
            model_bound_state_path(&other)
        );
        assert_ne!(
            model_bound_state_path(&custom),
            model_bound_state_path(&default)
        );

        // Re-enabling one config must not revoke either of the other files.
        record_model_bound_disabled_confirmation(&other).unwrap();
        assert_eq!(
            effective_masking(ModelBoundMasking::Enabled, Some(&custom)),
            ModelBoundMasking::Enabled
        );
        assert!(confirmation_required(desired, Some(&custom)));
        assert!(!confirmation_required(desired, Some(&default)));
        assert!(!confirmation_required(desired, Some(&other)));
        assert!(confirmation_required(desired, None));
        assert_eq!(effective_masking(desired, None), ModelBoundMasking::Enabled);
    }

    #[test]
    fn copied_receipt_cannot_confirm_identical_file_with_identical_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("first.toml");
        let destination = temp.path().join("second.toml");
        fs::write(&source, "[redaction]\nmodel_bound = \"disabled\"\n").unwrap();
        fs::copy(&source, &destination).unwrap();
        let modified = fs::metadata(&source).unwrap().modified().unwrap();
        fs::File::options()
            .write(true)
            .open(&destination)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let receipt = record_model_bound_disabled_confirmation(&source).unwrap();
        fs::copy(receipt, model_bound_state_path(&destination)).unwrap();
        assert!(confirmation_required(
            ModelBoundMasking::Disabled,
            Some(&destination)
        ));
        assert!(!confirmation_required(
            ModelBoundMasking::Disabled,
            Some(&source)
        ));
    }

    #[test]
    fn custom_confirmation_reads_loaded_file_instead_of_sibling_config_toml() {
        let temp = tempfile::tempdir().unwrap();
        let custom = temp.path().join("selected.toml");
        let default = temp.path().join("config.toml");
        fs::write(&default, "[redaction]\nmodel_bound = \"disabled\"\n").unwrap();
        assert!(record_model_bound_disabled_confirmation(&custom).is_err());
        fs::write(&custom, "[redaction]\nmodel_bound = \"enabled\"\n").unwrap();
        assert!(record_model_bound_disabled_confirmation(&custom).is_err());
        fs::write(&custom, "[redaction]\nmodel_bound = \"disabled\"\n").unwrap();
        fs::write(&default, "[redaction]\nmodel_bound = \"enabled\"\n").unwrap();
        record_model_bound_disabled_confirmation(&custom).unwrap();
        assert!(!confirmation_required(
            ModelBoundMasking::Disabled,
            Some(&custom)
        ));
        assert!(!model_bound_state_path(&default).exists());
    }

    #[test]
    fn corrupt_state_reads_as_unconfirmed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = state_path(tmp.path());
        std::fs::write(&path, "not json at all").expect("write corrupt state");
        assert!(!read_state(&path).model_bound_disabled_confirmed);
    }

    #[test]
    fn toml_table_parses_and_round_trips() {
        let parsed: crate::ConfigToml =
            toml::from_str("[redaction]\nmodel_bound = \"disabled\"\n").expect("parse");
        assert_eq!(
            parsed
                .redaction
                .as_ref()
                .expect("redaction table")
                .model_bound_masking(),
            ModelBoundMasking::Disabled
        );

        let absent: crate::ConfigToml = toml::from_str("").expect("parse empty");
        assert_eq!(
            absent.redaction_model_bound_masking(),
            ModelBoundMasking::Enabled
        );

        let serialized = toml::to_string(&parsed).expect("serialize");
        assert!(
            serialized.contains("model_bound = \"disabled\""),
            "{serialized}"
        );
    }

    /// The switch reads like a boolean to most people (`model_bound = false`
    /// is the natural way to ask "don't mask"). Accept boolean and on/off
    /// spellings so a plain `false` cannot hard-fail config parsing.
    #[test]
    fn boolean_and_on_off_spellings_parse_to_the_same_states() {
        for (body, expected) in [
            ("model_bound = false", ModelBoundMasking::Disabled),
            ("model_bound = true", ModelBoundMasking::Enabled),
            ("model_bound = \"false\"", ModelBoundMasking::Disabled),
            ("model_bound = \"off\"", ModelBoundMasking::Disabled),
            ("model_bound = \"OFF\"", ModelBoundMasking::Disabled),
            ("model_bound = \"on\"", ModelBoundMasking::Enabled),
            ("model_bound = \"disabled\"", ModelBoundMasking::Disabled),
            ("model_bound = \"ENABLED\"", ModelBoundMasking::Enabled),
        ] {
            let parsed: crate::ConfigToml =
                toml::from_str(&format!("[redaction]\n{body}\n")).expect("parse");
            assert_eq!(parsed.redaction_model_bound_masking(), expected, "{body}");
        }

        // Garbage stays a hard error with a useful message, not a silent default.
        let err = toml::from_str::<crate::ConfigToml>("[redaction]\nmodel_bound = \"maybe\"\n")
            .expect_err("unknown variant must fail");
        assert!(err.to_string().contains("enabled"), "{err}");
    }

    #[test]
    fn config_binding_rejects_oversized_config_before_parse() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join(crate::CONFIG_FILE_NAME);
        let mut body = String::from("[redaction]\nmodel_bound = \"disabled\"\n# ");
        body.push_str(&"x".repeat(crate::path_io::MAX_CONFIG_FILE_BYTES as usize + 64));
        body.push('\n');
        fs::write(&config_path, &body).expect("write oversized config");

        let err = config_binding(&config_path).expect_err("oversized config must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("1 MiB"), "{err}");
        assert!(record_model_bound_disabled_confirmation(&config_path).is_err());
        assert_eq!(
            effective_masking(ModelBoundMasking::Disabled, Some(&config_path)),
            ModelBoundMasking::Enabled
        );
    }
}
