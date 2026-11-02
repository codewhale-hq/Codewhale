use crate::config_store::{config_toml_error_location, parse_config_toml_str};
use crate::config_toml::ConfigToml;
use crate::external_credentials::quote_os_path;
use crate::path_io::read_checked_config_file;
use crate::provider_kind::ProviderKind;
use crate::provider_toml::CONFIG_FILE_NAME;
use anyhow::{Result, bail};
use codewhale_paths::{CODEWHALE_APP_DIR, LEGACY_APP_DIR};
use std::fs;
use std::path::{Path, PathBuf};

/// Values config.toml's root `approval_policy` accepts, compared trimmed and
/// case-insensitively. settings.toml's `approval_policy` is a different
/// vocabulary (`use-tui-default`, `ask`, `auto-review`, `full-access`).
pub const CONFIG_TOML_APPROVAL_POLICIES: &[&str] =
    &["on-request", "untrusted", "never", "auto", "suggest"];

/// Values config.toml's root `sandbox_mode` accepts.
pub const CONFIG_TOML_SANDBOX_MODES: &[&str] = &[
    "read-only",
    "workspace-write",
    "danger-full-access",
    "external-sandbox",
];

/// Values config.toml's root `verbosity` accepts.
pub const CONFIG_TOML_VERBOSITIES: &[&str] = &["normal", "concise"];

/// The closed vocabulary of a config.toml root key, if it has one.
#[must_use]
pub fn config_toml_choices(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "approval_policy" => Some(CONFIG_TOML_APPROVAL_POLICIES),
        "sandbox_mode" => Some(CONFIG_TOML_SANDBOX_MODES),
        "verbosity" => Some(CONFIG_TOML_VERBOSITIES),
        _ => None,
    }
}

/// Refuse a value the config.toml loader would reject for a closed-vocabulary
/// root key, so shared setters cannot write a file the TUI then fails to load.
pub fn check_config_toml_choice(key: &str, value: &str) -> Result<()> {
    let Some(choices) = config_toml_choices(key) else {
        return Ok(());
    };
    if choices.contains(&value.trim().to_ascii_lowercase().as_str()) {
        return Ok(());
    }
    let settings_note = if key == "approval_policy" {
        " (`use-tui-default`, `ask`, `auto-review` and `full-access` are settings.toml \
         values for the /settings editor; config.toml does not read them.)"
    } else {
        ""
    };
    let value = codewhale_secrets::redact::redact_secrets(value);
    bail!(
        "invalid value '{value}' for '{key}': config.toml accepts {}.{settings_note} \
         No value was changed.\nfix: codewhale config set {key} {}",
        choices.join(", "),
        choices[0]
    )
}

#[must_use]
pub fn project_approval_policy_is_allowed(current: Option<&str>, project: &str) -> bool {
    let Some(project_rank) = approval_policy_rank(project) else {
        return false;
    };
    match current.and_then(approval_policy_rank) {
        Some(current_rank) => project_rank >= current_rank,
        None => project_rank >= 2,
    }
}

#[must_use]
pub fn project_sandbox_mode_is_allowed(current: Option<&str>, project: &str) -> bool {
    let normalized_project = project.trim().to_ascii_lowercase();
    if normalized_project == "external-sandbox" {
        return current
            .map(|value| value.trim().eq_ignore_ascii_case("external-sandbox"))
            .unwrap_or(false);
    }

    let Some(project_rank) = sandbox_mode_rank(project) else {
        return false;
    };
    match current.and_then(sandbox_mode_rank) {
        Some(current_rank) => project_rank >= current_rank,
        None => project_rank >= 2,
    }
}

pub(crate) fn approval_policy_rank(value: &str) -> Option<u8> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(0),
        "suggest" | "suggested" | "on-request" | "untrusted" => Some(1),
        "never" | "deny" | "denied" => Some(2),
        _ => None,
    }
}

pub(crate) fn sandbox_mode_rank(value: &str) -> Option<u8> {
    match value.trim().to_ascii_lowercase().as_str() {
        "danger-full-access" => Some(0),
        "external-sandbox" => Some(0),
        "workspace-write" => Some(1),
        "read-only" => Some(2),
        _ => None,
    }
}

/// What [`load_project_config_outcome`] found in the workspace.
///
/// The distinction between "no project config" and "a project config that is
/// broken" is security-relevant, so it is in the type rather than in a log
/// line. A project config can only *tighten* `approval_policy` /
/// `sandbox_mode` beyond the user's baseline; if a typo makes it unparseable
/// and that is reported as absence, the project silently loses its
/// restrictions and falls back to the user's more permissive baseline.
#[derive(Debug, Clone)]
pub enum ProjectConfigOutcome {
    /// No project config file exists in this workspace.
    Missing,
    /// A project config was found and parsed.
    Loaded(Box<ConfigToml>),
    /// A project config file exists but could not be used. Its contents are
    /// deliberately not included — a config file holds credentials.
    Invalid {
        /// The offending file.
        path: PathBuf,
        /// Why it could not be used, safe to display.
        reason: String,
    },
}

impl ProjectConfigOutcome {
    /// The parsed config, discarding the reason a broken one was rejected.
    #[must_use]
    pub fn into_config(self) -> Option<ConfigToml> {
        match self {
            Self::Loaded(config) => Some(*config),
            Self::Missing | Self::Invalid { .. } => None,
        }
    }

    /// The path and reason when a project config exists but is unusable.
    #[must_use]
    pub fn invalid(&self) -> Option<(&Path, &str)> {
        match self {
            Self::Invalid { path, reason } => Some((path.as_path(), reason.as_str())),
            Self::Missing | Self::Loaded(_) => None,
        }
    }
}

/// Load a project-level config from the workspace, reporting why a file that
/// exists could not be used.
///
/// Checks `$WORKSPACE/.codewhale/config.toml` first, falling back to
/// `$WORKSPACE/.deepseek/config.toml` for backward compatibility.
pub fn load_project_config_outcome(workspace: &Path) -> ProjectConfigOutcome {
    for dir in [CODEWHALE_APP_DIR, LEGACY_APP_DIR] {
        let path = workspace.join(dir).join(CONFIG_FILE_NAME);
        if !project_config_candidate_exists(&path) {
            continue;
        }
        let raw = match read_checked_config_file(&path) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!("Failed to read project config {}: {e:#}", path.display());
                return ProjectConfigOutcome::Invalid {
                    path,
                    reason: format!("could not be read: {e}"),
                };
            }
        };
        match parse_config_toml_str(&raw) {
            Ok(config) => {
                let raw_provider = toml::from_str::<toml::Value>(&raw)
                    .ok()
                    .and_then(|document| document.get("provider").cloned())
                    .and_then(|provider| provider.as_str().map(str::to_string));
                if config.provider == ProviderKind::Custom
                    && raw_provider.as_deref() != Some(ProviderKind::Custom.as_str())
                {
                    // An unrecognized provider name deserializes to `Custom`
                    // rather than failing, so a typo would otherwise be
                    // accepted as a deliberate custom-provider selection.
                    tracing::warn!(
                        "Failed to parse project config {}; file contents were omitted",
                        quote_os_path(&path)
                    );
                    return ProjectConfigOutcome::Invalid {
                        path,
                        reason: match raw_provider {
                            // A key pasted into `provider =` must not be
                            // echoed; an ordinary typo stays readable.
                            Some(name) => format!(
                                "unknown provider '{}'",
                                codewhale_secrets::redact::redact_secrets(&name)
                            ),
                            None => "unknown provider".to_string(),
                        },
                    };
                }
                return ProjectConfigOutcome::Loaded(Box::new(config));
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to parse project config {}; file contents were omitted",
                    quote_os_path(&path)
                );
                return ProjectConfigOutcome::Invalid {
                    path,
                    // Position only: `toml`'s message and snippet can quote
                    // the offending value, which may be a credential.
                    reason: format!("invalid TOML at {}", config_toml_error_location(&raw, &err)),
                };
            }
        }
    }
    ProjectConfigOutcome::Missing
}

/// Load a project-level config from the workspace.
///
/// Returns `None` both when no project config exists and when one exists but
/// is unusable. Callers that act on the *absence* of project restrictions —
/// anything deciding whether a project tightens `approval_policy` or
/// `sandbox_mode` — should use [`load_project_config_outcome`] instead, so a
/// broken file is not read as "this project asked for nothing."
pub fn load_project_config(workspace: &Path) -> Option<ConfigToml> {
    load_project_config_outcome(workspace).into_config()
}

pub(crate) fn project_config_candidate_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        let file_type = metadata.file_type();
        file_type.is_file() || file_type.is_symlink()
    })
}
