use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use codewhale_config::{PluginAuthority, PluginId};

use super::activation::{PluginActivationCapability, PluginActivationPolicy};
use super::manifest::{
    PluginCompatibility, PluginInventory, PluginManifest, ResolvedPluginComponents,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginScope {
    Builtin,
    User,
    Workspace,
}

impl PluginScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::Workspace => "workspace",
        }
    }
}

impl fmt::Display for PluginScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOrigin {
    Builtin,
    CodeWhaleHome,
    Workspace,
}

impl PluginOrigin {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "codewhale-builtin",
            Self::CodeWhaleHome => "codewhale-home",
            Self::Workspace => "workspace-codewhale",
        }
    }
}

impl fmt::Display for PluginOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginDiagnosticLevel {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiagnostic {
    pub level: PluginDiagnosticLevel,
    pub code: &'static str,
    pub message: String,
    pub path: Option<PathBuf>,
}

impl PluginDiagnostic {
    #[must_use]
    pub fn warning(code: &'static str, message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self {
            level: PluginDiagnosticLevel::Warning,
            code,
            message: message.into(),
            path,
        }
    }

    #[must_use]
    pub fn error(code: &'static str, message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self {
            level: PluginDiagnosticLevel::Error,
            code,
            message: message.into(),
            path,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginTrustStatus {
    Trusted,
    NeverReviewed,
    ContentChanged,
    CapabilitiesChanged,
}

impl PluginTrustStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::NeverReviewed => "not-reviewed",
            Self::ContentChanged => "content-changed",
            Self::CapabilitiesChanged => "capabilities-changed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginSkillSnapshot {
    pub name: String,
    pub legacy_activation_name: Option<String>,
    pub description: String,
    pub localized_descriptions: HashMap<String, String>,
    pub invocation: codewhale_skill_install::skill_parse::SkillInvocation,
    pub aliases: Vec<String>,
    pub argument_hint: Option<String>,
    pub body: String,
    pub path: PathBuf,
    /// Digest of the exact UTF-8 bytes parsed into this snapshot. This is the
    /// corresponding entry in the reviewed bundle's file-hash inventory.
    pub source_hash: String,
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub id: PluginId,
    pub manifest: PluginManifest,
    pub base_path: PathBuf,
    pub canonical_root: PathBuf,
    /// Codewhale-owned, content-addressed copy used for active execution.
    /// `None` means the reviewed bundle has not been staged safely and cannot
    /// become active even if an older state file says it was enabled.
    pub staged_root: Option<PathBuf>,
    pub scope: PluginScope,
    pub origin: PluginOrigin,
    pub enabled: bool,
    pub trust_status: PluginTrustStatus,
    pub applicable: bool,
    pub inventory: PluginInventory,
    pub components: ResolvedPluginComponents,
    pub content_hash: String,
    pub capability_hash: String,
    pub state_generation: u64,
    pub skill_snapshots: Vec<PluginSkillSnapshot>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

impl LoadedPlugin {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.manifest.plugin.name
    }

    #[must_use]
    pub fn trusted(&self) -> bool {
        self.trust_status == PluginTrustStatus::Trusted
    }

    /// Explicit-review confirmation token binding a trust confirmation to
    /// both complete SHA-256 receipts, so a same-inventory bundle cannot
    /// collide through a short content prefix. The TUI `/plugin trust` flow
    /// and the Runtime API trust endpoint both compare against this value.
    #[must_use]
    pub fn review_token(&self) -> String {
        format!("{}.{}", self.content_hash, self.capability_hash)
    }

    #[must_use]
    pub fn compatibility(&self) -> PluginCompatibility {
        self.inventory.compatibility()
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.enabled
            && self.trusted()
            && self.staged_root.is_some()
            && self.applicable
            && self.inventory.can_activate_supported_components()
            && !self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level == PluginDiagnosticLevel::Error)
    }

    /// Bundle-wide `active()` is not enough at a consumption boundary. Skills
    /// must request Skills; each MCP server must request its stdio or remote
    /// transport. A later policy that drops an adapter refuses that adapter
    /// even if the bundle stays active for another supported surface.
    #[must_use]
    pub fn component_active(&self, capability: PluginActivationCapability) -> bool {
        self.active() && PluginActivationPolicy::current().is_supported(capability)
    }

    #[must_use]
    pub(super) fn authority(
        &self,
        state_path: PathBuf,
        workspace: PathBuf,
        managed_policy_path: PathBuf,
    ) -> Option<PluginAuthority> {
        let staged_root = self.staged_root.as_ref()?;
        // The staged tree mirrors the source bundle byte-for-byte, so both
        // share one manifest file name (`plugin.json` or legacy
        // `plugin.toml`). Fall back to the legacy name for synthetic
        // instances whose roots are not on disk.
        let manifest_name = super::agent_plugin::resolve_manifest_path(&self.canonical_root)
            .and_then(|path| path.file_name().map(|name| name.to_os_string()))
            .unwrap_or_else(|| std::ffi::OsString::from("plugin.toml"));
        Some(PluginAuthority {
            plugin_id: self.id.clone(),
            plugin_name: self.name().to_string(),
            workspace,
            state_path,
            managed_policy_path: Some(managed_policy_path.clone()),
            managed_policy_source: Some(managed_policy_path),
            source_manifest: self.canonical_root.join(&manifest_name),
            staged_manifest: staged_root.join(&manifest_name),
            content_hash: self.content_hash.clone(),
            capability_hash: self.capability_hash.clone(),
            state_generation: self.state_generation,
        })
    }

    #[must_use]
    pub fn state_label(&self) -> &'static str {
        if self.active() {
            "active"
        } else if !self.enabled {
            "disabled"
        } else if !self.trusted() {
            "enabled-untrusted"
        } else if self.staged_root.is_none() {
            "unstaged"
        } else if !self.applicable {
            "inapplicable"
        } else if !self.inventory.can_activate_supported_components() {
            "unsupported"
        } else {
            "inactive"
        }
    }
}
