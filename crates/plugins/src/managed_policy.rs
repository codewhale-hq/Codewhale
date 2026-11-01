//! Managed (organization/fleet) plugin policy: the Runtime-side enforcement primitive.
//!
//! An organization governs its members' Runtimes by delivering a policy document
//! to each machine; this module only defines the document and enforces it.
//! Delivery (network fetch, control-plane client, MDM deployment) is out of
//! scope: the policy arrives as a local file.
//!
//! Semantics, in order of importance:
//! - **Absent policy means today's exact behaviour.** Every plugin that enables
//!   now must still enable when no policy file exists.
//! - **Local state can never defeat the policy.** Enforcement lives in the
//!   registry's `apply_state` choke point, so a plugin enabled before the
//!   policy arrived, or an `enabled: true` bit hand-edited into `state.json`,
//!   comes back disabled. Execution boundaries re-read the trusted policy
//!   source, and the existing MCP watcher cancels newly forbidden work on its
//!   next poll. `PluginRegistry::enable` also re-reads the document.
//! - **A malformed policy fails closed.** An unreadable, unparseable, or
//!   wrong-schema document refuses every enablement with a clear error; it
//!   never silently degrades to "allow everything".
//!
//! The document is resolved to a single source: `managed-policy.json` next to
//! the plugin `state.json`, overridable by `CODEWHALE_MANAGED_POLICY_PATH` for
//! testing. The override is read from the pre-dotenv [`HostEnvironment`]
//! snapshot and frozen to an absolute source before dotenv, so a workspace
//! dotenv file or later working-directory change cannot redirect governance.
//!
//! This is deliberately a different concept from
//! [`PluginActivationPolicy`](super::activation::PluginActivationPolicy), which
//! describes which capability *kinds* this build will ever activate. The managed
//! policy describes which plugin *identities* this machine may enable.
//!
use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::context::HostEnvironment;
use codewhale_config::PluginId;

/// Schema version of the managed policy document, versioned exactly like
/// `PluginStateFile`: a required `schema_version` field that must match.
pub const MANAGED_POLICY_SCHEMA_VERSION: u32 = 1;

/// File name resolved next to the plugin state file (e.g.
/// `~/.codewhale/plugins/managed-policy.json`).
pub const MANAGED_POLICY_FILE_NAME: &str = "managed-policy.json";

/// Process-environment override for the policy path. Read from the pre-dotenv
/// host snapshot, never from ambient process state after dotenv loads.
pub const MANAGED_POLICY_PATH_ENV: &str = "CODEWHALE_MANAGED_POLICY_PATH";

/// Organization-supplied allowlist of plugin identities permitted on this machine.
///
/// Identities are full discovery [`PluginId`]s (`scope/hash/name`), matched
/// exactly: a renamed or relocated bundle has a different id and is not
/// covered by an entry written for another id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedPluginPolicy {
    pub schema_version: u32,
    #[serde(default)]
    pub allowed_plugins: BTreeSet<PluginId>,
    #[serde(default)]
    pub allow_unlisted: bool,
}

impl ManagedPluginPolicy {
    /// True when `id` may be enabled under this policy: either it is
    /// allowlisted, or the policy permits plugins outside the allowlist.
    #[must_use]
    pub fn allows(&self, id: &PluginId) -> bool {
        self.allow_unlisted || self.allowed_plugins.contains(id)
    }
}

/// Outcome of loading the managed policy document. There is no "ignore and
/// allow" outcome: anything other than absent-or-valid fails closed.
pub enum ManagedPolicyOutcome {
    Absent,
    Loaded(ManagedPluginPolicy),
    Invalid(String),
}

/// Resolve the single policy source: the env override when set to a non-empty
/// value, otherwise `managed-policy.json` next to the plugin state file,
/// frozen as an absolute path while trusted discovery inputs are captured.
pub fn resolve_managed_policy_path(
    state_path: &Path,
    host_environment: Option<&HostEnvironment>,
) -> Result<PathBuf, String> {
    let override_path =
        match host_environment.map(|environment| environment.var(MANAGED_POLICY_PATH_ENV)) {
            Some(Ok(value)) if !value.trim().is_empty() => Some(value),
            Some(Err(std::env::VarError::NotUnicode(_))) => {
                return Err("Managed plugin policy override is not valid Unicode".to_string());
            }
            _ => None,
        };
    let path = override_path.map(PathBuf::from).unwrap_or_else(|| {
        state_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join(MANAGED_POLICY_FILE_NAME)
    });
    std::path::absolute(&path)
        .map_err(|error| format!("Managed plugin policy source could not be resolved: {error}"))
}

/// Load the policy document. A missing file is [`ManagedPolicyOutcome::Absent`]
/// (today's behaviour); any other failure is [`ManagedPolicyOutcome::Invalid`].
pub fn load_managed_policy(path: &Path) -> ManagedPolicyOutcome {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return ManagedPolicyOutcome::Absent;
        }
        Err(error) => {
            return ManagedPolicyOutcome::Invalid(format!(
                "failed to read {}: {error}",
                path.display()
            ));
        }
    };
    let policy: ManagedPluginPolicy = match serde_json::from_str(&raw) {
        Ok(policy) => policy,
        Err(error) => {
            return ManagedPolicyOutcome::Invalid(format!(
                "failed to parse {}: {error}",
                path.display()
            ));
        }
    };
    if policy.schema_version != MANAGED_POLICY_SCHEMA_VERSION {
        return ManagedPolicyOutcome::Invalid(format!(
            "unsupported managed plugin policy schema {}; expected {MANAGED_POLICY_SCHEMA_VERSION}",
            policy.schema_version
        ));
    }
    ManagedPolicyOutcome::Loaded(policy)
}
