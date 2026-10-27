//! Structural, offline-by-default diagnostics shared by doctor renderers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

/// Canonical user-scoped paths reported by both human and JSON doctor output.
///
/// Resolution is read-only: this type does not construct managers, create
/// directories, or trigger legacy migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DoctorPathReport {
    pub(crate) home: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) settings: PathBuf,
    pub(crate) sessions: PathBuf,
    pub(crate) logs: PathBuf,
    pub(crate) automations: PathBuf,
    pub(crate) task_manager_root: PathBuf,
    pub(crate) task_manager_tasks: PathBuf,
    pub(crate) task_manager_artifacts: PathBuf,
    pub(crate) runtime_store: PathBuf,
    pub(crate) runtime_events: PathBuf,
    pub(crate) personal_fleet_definitions: PathBuf,
    pub(crate) personal_fleet_agents: PathBuf,
    pub(crate) secrets: PathBuf,
}

impl DoctorPathReport {
    pub(crate) fn resolve(config_override: Option<&Path>) -> Result<Self> {
        let home = codewhale_paths::codewhale_home()
            .map_err(anyhow::Error::new)?
            .context("could not resolve the canonical Codewhale state root")?;
        let config = match config_override {
            Some(path) => codewhale_config::resolve_config_path(Some(path.to_path_buf()))
                .context("could not normalize the explicit config path")?,
            None => codewhale_config::resolve_config_path(None)
                .unwrap_or_else(|_| home.join(codewhale_config::CONFIG_FILE_NAME)),
        };
        let settings = codewhale_runtime::settings::Settings::path()
            .context("could not resolve the canonical settings path")?;
        let sessions = codewhale_config::resolve_state_dir("sessions")
            .context("could not resolve the sessions path")?;
        let logs = crate::runtime_log::log_directory()
            .context("could not resolve the runtime log directory")?;
        let automations = crate::automation_manager::default_automations_dir();
        let task_manager_root = crate::task_manager::default_tasks_dir();
        let task_manager_tasks = task_manager_root.join("tasks");
        let task_manager_artifacts = task_manager_root.join("artifacts");
        let runtime_config = crate::runtime_threads::RuntimeThreadManagerConfig::from_task_data_dir(
            task_manager_root.clone(),
        );
        let runtime_store = runtime_config.data_dir;
        let runtime_events = runtime_store.join("events");
        let personal_fleet_definitions = crate::fleet::exact::personal_fleet_definitions_dir()
            .context("could not resolve the personal Fleet definitions directory")?;
        let personal_fleet_agents = crate::fleet::profile::personal_agent_profile_dir()
            .context("could not resolve the personal Fleet agent directory")?;
        let (secrets, _) = codewhale_secrets::FileKeyringStore::default_paths_read_only()
            .context("could not resolve the file secret backend path")?;
        Ok(Self {
            home,
            config,
            settings,
            sessions,
            logs,
            automations,
            task_manager_root,
            task_manager_tasks,
            task_manager_artifacts,
            runtime_store,
            runtime_events,
            personal_fleet_definitions,
            personal_fleet_agents,
            secrets,
        })
    }

    pub(crate) fn entries(&self) -> [(&'static str, &Path); 14] {
        [
            ("home", self.home.as_path()),
            ("config", self.config.as_path()),
            ("settings", self.settings.as_path()),
            ("sessions", self.sessions.as_path()),
            ("logs", self.logs.as_path()),
            ("automations", self.automations.as_path()),
            ("task_manager_root", self.task_manager_root.as_path()),
            ("task_manager_tasks", self.task_manager_tasks.as_path()),
            (
                "task_manager_artifacts",
                self.task_manager_artifacts.as_path(),
            ),
            ("runtime_store", self.runtime_store.as_path()),
            ("runtime_events", self.runtime_events.as_path()),
            (
                "personal_fleet_definitions",
                self.personal_fleet_definitions.as_path(),
            ),
            (
                "personal_fleet_agents",
                self.personal_fleet_agents.as_path(),
            ),
            ("secrets", self.secrets.as_path()),
        ]
    }
}

/// Render structural secret-backend facts for the human doctor report.
///
/// The input type cannot carry secret values, keeping this renderer safe by
/// construction.
pub(crate) fn secret_backend_human_lines(
    diagnostic: &codewhale_secrets::SecretBackendDiagnostic,
) -> Vec<String> {
    use codewhale_secrets::{
        SecretBackendDiagnosticKind, SecretBackendInspection, SecretBackendPresence,
    };

    let presence = |value| match value {
        SecretBackendPresence::Present => "present",
        SecretBackendPresence::Absent => "absent",
        SecretBackendPresence::Unknown => "unknown",
    };
    let inspection = match diagnostic.inspection {
        SecretBackendInspection::MetadataOnly => "metadata_only",
        SecretBackendInspection::NotProbed => "not_probed",
    };
    let mut lines = match diagnostic.backend {
        SecretBackendDiagnosticKind::File => vec![
            "backend: file".to_string(),
            format!("presence: {} ({inspection})", presence(diagnostic.presence)),
        ],
        SecretBackendDiagnosticKind::System => vec![
            "backend: system".to_string(),
            "status: unknown (not_probed)".to_string(),
        ],
        SecretBackendDiagnosticKind::Unknown => vec![
            "backend: unknown".to_string(),
            "status: unknown (not_probed; unsupported configuration)".to_string(),
        ],
    };
    if let Some(path) = diagnostic.path.as_deref() {
        lines.push(format!("path: {}", path.display()));
    }
    if let Some(path) = diagnostic.legacy_path.as_deref() {
        lines.push(format!(
            "legacy_path: {} ({}, {inspection})",
            path.display(),
            presence(diagnostic.legacy_presence)
        ));
    }
    lines.push("No credential-store values were read or printed by this check.".to_string());
    lines
}

/// Report key names — never values — for config entries whose value is
/// shaped like a bearer credential. `config.toml` is plain text, not a
/// secret store; doctor warns so tokens migrate to the secret backend
/// (morning-report issue: a plaintext OAuth token sat beside a `[redacted]`
/// sibling entry).
pub(crate) fn config_credential_shaped_keys(raw: &str) -> Vec<String> {
    fn strong_shape(value: &str) -> bool {
        const PREFIXES: [&str; 9] = [
            "sk-",
            "sk_",
            "xai-",
            "ghp_",
            "gho_",
            "github_pat_",
            "xoxb-",
            "xoxp-",
            "eyJ",
        ];
        value.len() >= 20 && PREFIXES.iter().any(|prefix| value.starts_with(prefix))
    }
    fn suspect_key(key: &str) -> bool {
        let key = key.to_ascii_lowercase();
        [
            "token",
            "secret",
            "password",
            "credential",
            "api_key",
            "apikey",
            "access_key",
        ]
        .iter()
        .any(|needle| key.contains(needle))
    }
    fn random_shape(value: &str) -> bool {
        value.len() >= 24
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            && value.chars().any(|ch| ch.is_ascii_digit())
            && value.chars().any(|ch| ch.is_ascii_alphabetic())
    }

    let mut flagged: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let value = value.trim();
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            continue;
        };
        if value.is_empty() || value.eq_ignore_ascii_case("[redacted]") {
            continue;
        }
        if (strong_shape(value) || (suspect_key(key) && random_shape(value)))
            && !flagged.iter().any(|existing| existing == key)
        {
            flagged.push(key.to_string());
        }
    }
    flagged
}

/// Explicit live operations requested for one doctor invocation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DoctorProbeRequest {
    pub(crate) check_updates: bool,
    pub(crate) probe_api: bool,
    pub(crate) probe_local: bool,
    pub(crate) probe_mcp: bool,
    pub(crate) probe_search: bool,
}

impl DoctorProbeRequest {
    /// Whether a release service may be contacted.
    pub(crate) fn should_check_updates(self) -> bool {
        self.check_updates
    }

    /// Whether the configured provider endpoint may be contacted.
    ///
    /// Hosted and local endpoints have separate opt-ins because a local probe
    /// can wake a desktop-managed daemon.
    pub(crate) fn should_probe_api(self, endpoint_is_local: bool) -> bool {
        if endpoint_is_local {
            self.probe_local
        } else {
            self.probe_api
        }
    }

    /// Whether configured MCP processes may be started and contacted.
    pub(crate) fn should_probe_mcp(self) -> bool {
        self.probe_mcp
    }

    /// Whether the selected web-search provider authority may be contacted.
    pub(crate) fn should_probe_search(self) -> bool {
        self.probe_search
    }
}

const SEARCH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounded, credential-free reachability result for the configured search
/// provider. This intentionally cannot represent a successful search or valid
/// authentication: a HEAD response proves transport reachability only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DoctorSearchProbeReport {
    NotChecked,
    FeatureDisabled,
    ConfigurationError(crate::tools::web_search::SearchProbeTargetError),
    PolicyDenied {
        authority: String,
    },
    PolicyApprovalRequired {
        authority: String,
    },
    Reachable {
        authority: String,
        status: u16,
    },
    Unreachable {
        authority: String,
        failure: DoctorSearchProbeFailure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoctorSearchProbeFailure {
    Timeout,
    Connection,
    Request,
}

/// Probe the selected search provider without reading credentials, issuing a
/// query, following redirects, or writing a network-policy audit receipt.
pub(crate) async fn doctor_search_probe(
    config: &codewhale_runtime::config::Config,
    probes: DoctorProbeRequest,
) -> DoctorSearchProbeReport {
    use codewhale_net::network_policy::{Decision, NetworkPolicyDecider};

    if !probes.should_probe_search() {
        return DoctorSearchProbeReport::NotChecked;
    }
    if !config
        .features()
        .enabled(codewhale_runtime::features::Feature::WebSearch)
    {
        return DoctorSearchProbeReport::FeatureDisabled;
    }

    let provider = config.search_provider();
    let base_url = config
        .search
        .as_ref()
        .and_then(|search| search.base_url.as_deref());
    let target = match crate::tools::web_search::search_probe_target(provider, base_url) {
        Ok(target) => target,
        Err(error) => return DoctorSearchProbeReport::ConfigurationError(error),
    };
    let authority = codewhale_config::structural_url_authority(target.url.as_str());

    if let Some(policy) = config.network.clone().map(|network| {
        // Doctor is a read-only diagnostic. Evaluate the same typed policy as
        // web_search, but do not attach the runtime audit writer.
        NetworkPolicyDecider::new(
            codewhale_net::network_policy::NetworkPolicy::from(network),
            None,
        )
    }) {
        match policy.evaluate(&target.host, "web_search") {
            Decision::Allow => {}
            Decision::Deny => return DoctorSearchProbeReport::PolicyDenied { authority },
            Decision::Prompt => {
                return DoctorSearchProbeReport::PolicyApprovalRequired { authority };
            }
        }
    }

    let client = match codewhale_release::tls::reqwest_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(SEARCH_PROBE_TIMEOUT)
        .timeout(SEARCH_PROBE_TIMEOUT)
        .user_agent(concat!("codewhale-doctor/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return DoctorSearchProbeReport::Unreachable {
                authority,
                failure: DoctorSearchProbeFailure::Request,
            };
        }
    };

    match client.head(target.url).send().await {
        Ok(response) => DoctorSearchProbeReport::Reachable {
            authority,
            status: response.status().as_u16(),
        },
        Err(error) => {
            let failure = if error.is_timeout() {
                DoctorSearchProbeFailure::Timeout
            } else if error.is_connect() {
                DoctorSearchProbeFailure::Connection
            } else {
                DoctorSearchProbeFailure::Request
            };
            DoctorSearchProbeReport::Unreachable { authority, failure }
        }
    }
}

/// Plain, bounded doctor copy for a search reachability result.
pub(crate) fn doctor_search_probe_lines(report: &DoctorSearchProbeReport) -> Vec<String> {
    use crate::tools::web_search::SearchProbeTargetError;

    match report {
        DoctorSearchProbeReport::NotChecked => vec![
            "· not checked (offline default)".to_string(),
            "Run `codewhale doctor --probe-search` to test the selected provider authority."
                .to_string(),
        ],
        DoctorSearchProbeReport::FeatureDisabled => vec![
            "· not checked because the web_search feature is disabled".to_string(),
            "Enable web_search before probing its provider.".to_string(),
        ],
        DoctorSearchProbeReport::ConfigurationError(error) => {
            let detail = match error {
                SearchProbeTargetError::Missing => {
                    "SearXNG requires an explicit [search] base_url"
                }
                SearchProbeTargetError::Unsupported => {
                    "[search] base_url is supported only for DuckDuckGo-compatible or SearXNG providers"
                }
                SearchProbeTargetError::Invalid => {
                    "the configured [search] base_url is not a valid HTTP(S) authority"
                }
            };
            vec![format!("! not probed: {detail}")]
        }
        DoctorSearchProbeReport::PolicyDenied { authority } => vec![
            format!("! {authority} blocked by configured network policy"),
            "The same policy blocks web_search; update /network only if this authority should be allowed."
                .to_string(),
        ],
        DoctorSearchProbeReport::PolicyApprovalRequired { authority } => vec![
            format!("! {authority} requires network approval"),
            "Allow the authority with /network, then rerun this probe.".to_string(),
        ],
        DoctorSearchProbeReport::Reachable { authority, status } => vec![
            format!("✓ {authority} responded (HTTP {status})"),
            "Transport only; authentication and search results were not tested.".to_string(),
        ],
        DoctorSearchProbeReport::Unreachable { authority, failure } => {
            let detail = match failure {
                DoctorSearchProbeFailure::Timeout => "timed out",
                DoctorSearchProbeFailure::Connection => "connection, DNS, or TLS setup failed",
                DoctorSearchProbeFailure::Request => "request failed",
            };
            vec![
                format!("✗ {authority} {detail}"),
                "No search API key or query was sent; provider error details were omitted."
                    .to_string(),
            ]
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DoctorUpdateReport {
    NotChecked,
    UpdateAvailable { latest: String },
    UpToDate { latest: String },
    CurrentNewer { latest: String },
    ReleaseMetadataInvalid,
    ReleaseCheckFailed,
}

fn doctor_update_report<E>(
    current_version: &str,
    latest_result: Result<String, E>,
) -> DoctorUpdateReport {
    let Ok(raw_latest) = latest_result else {
        return DoctorUpdateReport::ReleaseCheckFailed;
    };
    let Some(latest) = doctor_safe_release_tag(&raw_latest) else {
        return DoctorUpdateReport::ReleaseMetadataInvalid;
    };
    match codewhale_release::compare_release_versions(current_version, &latest) {
        Ok(std::cmp::Ordering::Less) => DoctorUpdateReport::UpdateAvailable { latest },
        Ok(std::cmp::Ordering::Equal) => DoctorUpdateReport::UpToDate { latest },
        Ok(std::cmp::Ordering::Greater) => DoctorUpdateReport::CurrentNewer { latest },
        Err(_) => DoctorUpdateReport::ReleaseMetadataInvalid,
    }
}

/// Canonicalize a release tag before any doctor renderer sees it. A release
/// server response is untrusted input: it may not become an error echo or a
/// terminal control sequence merely because the user opted into an update
/// check.
fn doctor_safe_release_tag(raw: &str) -> Option<String> {
    let version = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    semver::Version::parse(version)
        .ok()
        .map(|version| format!("v{version}"))
}

/// `update_command` is the install-method-aware upgrade command
/// ([`codewhale_release::InstallMethod::update_command`]): an npm, Homebrew,
/// cargo or Omarchy install must be upgraded by its package manager, never by
/// `codewhale update`, which refuses to replace a managed binary.
fn doctor_update_report_lines(report: &DoctorUpdateReport, update_command: &str) -> Vec<String> {
    match report {
        DoctorUpdateReport::NotChecked => vec![
            "latest: unknown (not checked; offline default)".to_string(),
            "Run `codewhale doctor --check-updates` to opt in.".to_string(),
        ],
        DoctorUpdateReport::UpdateAvailable { latest } => vec![
            format!("latest: {latest}"),
            format!("Update available. Run `{update_command}` to install."),
        ],
        DoctorUpdateReport::UpToDate { latest } => {
            vec![
                format!("latest: {latest}"),
                "Already up to date.".to_string(),
            ]
        }
        DoctorUpdateReport::CurrentNewer { latest } => vec![
            format!("latest: {latest}"),
            "Current build is newer than the latest published release.".to_string(),
        ],
        DoctorUpdateReport::ReleaseMetadataInvalid => vec![
            "latest: unknown (release metadata invalid; details omitted)".to_string(),
            "Run `codewhale update --check` to retry.".to_string(),
        ],
        DoctorUpdateReport::ReleaseCheckFailed => vec![
            "latest: unknown (release check failed; details omitted)".to_string(),
            "Run `codewhale update --check` to retry.".to_string(),
        ],
    }
}

/// Print the update portion of the human doctor report.
///
/// The release service is contacted only when `--check-updates` populated the
/// explicit request bit. The default branch returns before constructing an
/// HTTP request. Failure details are deliberately typed and generic because
/// transport errors and release metadata are untrusted strings.
pub(crate) async fn print_update_report(probes: DoctorProbeRequest) {
    let current_version = env!("CARGO_PKG_VERSION");
    println!("  · current: v{current_version}");
    let report = if probes.should_check_updates() {
        doctor_update_report(
            current_version,
            codewhale_release::latest_release_tag_async(codewhale_release::ReleaseChannel::Stable)
                .await,
        )
    } else {
        DoctorUpdateReport::NotChecked
    };
    let method = codewhale_release::current_install_method();
    for (index, line) in doctor_update_report_lines(&report, method.update_command())
        .into_iter()
        .enumerate()
    {
        let indent = if index == 0 { "  ·" } else { "   " };
        println!("{indent} {line}");
    }
}

pub(crate) fn is_keyless_ds4_route(config: &codewhale_runtime::config::Config) -> bool {
    config
        .provider
        .as_deref()
        .is_some_and(|provider| provider.eq_ignore_ascii_case("ds4"))
        && codewhale_runtime::config::base_url_uses_local_host(&config.active_route_base_url())
        && codewhale_config::auth_mode_disables_api_key(
            config
                .active_provider_identity()
                .ok()
                .and_then(|identity| config.auth_mode_for_provider(&identity))
                .as_deref(),
        )
}

/// Probe DS4 through its cheap `/v1/models` contract instead of waking the
/// model for a completion. The selected model must be advertised.
pub(crate) async fn probe_ds4_models(
    config: &codewhale_runtime::config::Config,
) -> anyhow::Result<()> {
    use crate::client::CodewhaleClient;
    use crate::core::model_client::ModelClient;

    let endpoint =
        codewhale_secrets::sanitize::redact_url_for_display(&config.active_route_base_url());
    let client = CodewhaleClient::new(config)?;
    let configured_alias = client.model().to_string();
    let models = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client.list_models(),
    )
    .await
    {
        Ok(Ok(models)) => models,
        Ok(Err(error)) => anyhow::bail!(ds4_probe_error(config, &error.to_string())),
        Err(_) => anyhow::bail!("DS4 /v1/models timed out after 15 seconds at {}", endpoint),
    };
    if !models
        .iter()
        .any(|available| available.id == configured_alias)
    {
        let advertised = models
            .iter()
            .take(8)
            .map(|available| available.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "DS4 /v1/models at {} did not list configured alias '{configured_alias}' (advertised: {})",
            endpoint,
            if advertised.is_empty() {
                "none"
            } else {
                advertised.as_str()
            }
        );
    }
    Ok(())
}

fn ds4_probe_error(config: &codewhale_runtime::config::Config, error: &str) -> String {
    let endpoint =
        codewhale_secrets::sanitize::redact_url_for_display(&config.active_route_base_url());
    let status = error
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|pair| {
            (pair[0].eq_ignore_ascii_case("HTTP")
                && pair[1].trim_matches(|ch: char| !ch.is_ascii_digit()).len() == 3)
                .then(|| {
                    pair[1]
                        .trim_matches(|ch: char| !ch.is_ascii_digit())
                        .to_string()
                })
        });
    match status {
        Some(status) => format!("DS4 /v1/models returned HTTP {status} at {}", endpoint),
        None => format!(
            "DS4 /v1/models could not be reached at {} (is ds4-server running?)",
            endpoint
        ),
    }
}

#[cfg(test)]
#[path = "doctor/tests.rs"]
mod tests;
