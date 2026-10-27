//! Versioned plugin activation policy.
//!
//! This is the single source of truth for which reviewed component adapters
//! this Codewhale build will execute. Compatibility, `active()` decisions,
//! consumption-boundary checks, and the capability hash all read this policy.
//! Enabling a new adapter later must change the policy (version and/or mask)
//! so existing trust receipts fail closed as `CapabilitiesChanged`.

use sha2::Digest;

/// Capability-hash domain for the current activation-policy binding.
pub const CAPABILITY_HASH_DOMAIN_V3: &[u8] = b"codewhale-plugin-capabilities-v3\0";

/// Historical policy domain kept so persisted v2 receipts are intentionally
/// invalidated when the declarative Commands, Agents, and Hooks adapters ship.
pub const CAPABILITY_HASH_DOMAIN_V2: &[u8] = b"codewhale-plugin-capabilities-v2\0";

/// Historical domain used before the activation policy was bound into the
/// receipt. Kept so discovery can prove a v1 receipt no longer matches.
pub const CAPABILITY_HASH_DOMAIN_V1: &[u8] = b"codewhale-plugin-capabilities-v1\0";

pub const ACTIVATION_POLICY_VERSION: u32 = 5;

/// Policy version selected when `[features] extension_host` is on: `Native`
/// (host code run by the TypeScript extension host) moves from inactive to
/// supported. Every receipt reviewed under v5 fails closed as
/// `CapabilitiesChanged` under v6 and the reverse, so toggling the flag in
/// either direction re-reviews every plugin. That is intended.
pub const EXTENSION_HOST_POLICY_VERSION: u32 = 6;

pub(crate) const DSH_COMPATIBILITY: &[u8] =
    include_bytes!("../../extension-host/dist/dsh-compatibility.json");

pub(crate) fn dsh_row_bridge(name: &str) -> bool {
    static TABLE: std::sync::OnceLock<Option<serde_json::Value>> = std::sync::OnceLock::new();
    TABLE
        .get_or_init(|| serde_json::from_slice(DSH_COMPATIBILITY).ok())
        .as_ref()
        .and_then(|table| table.get("row_bridges"))
        .and_then(|bridges| bridges.get(name))
        .is_some_and(serde_json::Value::is_string)
}

/// Process-wide policy selection, set once at boot from config
/// (`install_extension_host_policy`). A config reload never flips it
/// mid-process; unset means the shipping v5 policy, which also covers tests.
static EXTENSION_HOST_POLICY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[cfg(test)]
thread_local! {
    /// Test-only per-thread override so one test can exercise v6 without
    /// changing the policy every other (parallel) test observes.
    static TEST_POLICY_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Select the activation policy for this process. The first call wins.
pub fn install_extension_host_policy(extension_host_enabled: bool) {
    let _ = EXTENSION_HOST_POLICY.set(extension_host_enabled);
}

/// Whether this process activates `Native` (extension host) components.
#[must_use]
pub fn extension_host_policy_enabled() -> bool {
    #[cfg(test)]
    if let Some(value) = TEST_POLICY_OVERRIDE.with(std::cell::Cell::get) {
        return value;
    }
    EXTENSION_HOST_POLICY.get().copied().unwrap_or(false)
}

/// Carries the calling thread's policy into a `spawn_blocking` closure. In
/// production the policy is process-wide, so this is a no-op; under test it
/// re-applies the per-thread override on the blocking thread.
pub(crate) struct PolicyScope {
    #[cfg(test)]
    _guard: TestPolicyGuard,
}

impl PolicyScope {
    #[must_use]
    pub(crate) fn propagate(extension_host_enabled: bool) -> Self {
        #[cfg(test)]
        {
            Self {
                _guard: TestPolicyGuard::extension_host(extension_host_enabled),
            }
        }
        #[cfg(not(test))]
        {
            let _ = extension_host_enabled;
            Self {}
        }
    }
}

/// Test guard selecting the v6 (or v5) policy on the current thread only.
#[cfg(test)]
pub(crate) struct TestPolicyGuard {
    previous: Option<bool>,
}

#[cfg(test)]
impl TestPolicyGuard {
    pub(crate) fn extension_host(enabled: bool) -> Self {
        let previous = TEST_POLICY_OVERRIDE.with(|cell| cell.replace(Some(enabled)));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for TestPolicyGuard {
    fn drop(&mut self) {
        TEST_POLICY_OVERRIDE.with(|cell| cell.set(self.previous));
    }
}

/// A runtime adapter or inventoried capability that a bundle may declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginActivationCapability {
    Skills,
    McpStdio,
    McpRemote,
    Commands,
    Agents,
    Hooks,
    Lsp,
    Native,
    Providers,
    FilesystemRoots,
    LifecycleMutation,
}

impl PluginActivationCapability {
    pub const ALL: &'static [Self] = &[
        Self::Skills,
        Self::McpStdio,
        Self::McpRemote,
        Self::Commands,
        Self::Agents,
        Self::Hooks,
        Self::Lsp,
        Self::Native,
        Self::Providers,
        Self::FilesystemRoots,
        Self::LifecycleMutation,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::McpStdio => "mcp-stdio",
            Self::McpRemote => "mcp-remote",
            Self::Commands => "commands",
            Self::Agents => "agents",
            Self::Hooks => "hooks",
            Self::Lsp => "lsp",
            Self::Native => "native",
            Self::Providers => "providers",
            Self::FilesystemRoots => "filesystem-roots",
            Self::LifecycleMutation => "lifecycle-mutation",
        }
    }
}

/// The adapters this exact Codewhale build will activate, plus the inventoried
/// surfaces that stay inactive. Fields are public so tests can construct a
/// mutated policy and prove the capability hash moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginActivationPolicy {
    pub version: u32,
    pub supported: &'static [PluginActivationCapability],
    pub inactive: &'static [PluginActivationCapability],
    pub dsh_compatibility: &'static [u8],
}

impl PluginActivationPolicy {
    /// The policy this process runs under: v5, or v6 when the experimental
    /// extension host is enabled (see [`install_extension_host_policy`]).
    #[must_use]
    pub fn current() -> Self {
        if extension_host_policy_enabled() {
            Self::extension_host()
        } else {
            Self::v3()
        }
    }

    /// v6: identical to v5 except `Native` (host code) is supported.
    #[must_use]
    pub const fn extension_host() -> Self {
        Self {
            version: EXTENSION_HOST_POLICY_VERSION,
            dsh_compatibility: DSH_COMPATIBILITY,
            supported: &[
                PluginActivationCapability::Skills,
                PluginActivationCapability::McpStdio,
                PluginActivationCapability::McpRemote,
                PluginActivationCapability::Commands,
                PluginActivationCapability::Agents,
                PluginActivationCapability::Hooks,
                PluginActivationCapability::Providers,
                PluginActivationCapability::Native,
            ],
            inactive: &[
                PluginActivationCapability::Lsp,
                PluginActivationCapability::FilesystemRoots,
                PluginActivationCapability::LifecycleMutation,
            ],
        }
    }

    /// Shipping declarative policy (v5). The historical `v3` method name is
    /// retained; providers intentionally invalidate older review receipts.
    #[must_use]
    pub const fn v3() -> Self {
        Self {
            version: ACTIVATION_POLICY_VERSION,
            dsh_compatibility: &[],
            supported: &[
                PluginActivationCapability::Skills,
                PluginActivationCapability::McpStdio,
                PluginActivationCapability::McpRemote,
                PluginActivationCapability::Commands,
                PluginActivationCapability::Agents,
                PluginActivationCapability::Hooks,
                PluginActivationCapability::Providers,
            ],
            inactive: &[
                PluginActivationCapability::Lsp,
                PluginActivationCapability::Native,
                PluginActivationCapability::FilesystemRoots,
                PluginActivationCapability::LifecycleMutation,
            ],
        }
    }

    #[must_use]
    pub fn is_supported(self, capability: PluginActivationCapability) -> bool {
        self.supported.contains(&capability)
    }

    pub fn write_hash_material(self, hasher: &mut impl Digest) {
        hasher.update(CAPABILITY_HASH_DOMAIN_V3);
        hasher.update(b"policy-version\0");
        hasher.update(self.version.to_string().as_bytes());
        hasher.update(b"\0");
        for capability in self.supported {
            hasher.update(b"supported\0");
            hasher.update(capability.as_str().as_bytes());
            hasher.update(b"\0");
        }
        for capability in self.inactive {
            hasher.update(b"inactive\0");
            hasher.update(capability.as_str().as_bytes());
            hasher.update(b"\0");
        }
        if !self.dsh_compatibility.is_empty() {
            hasher.update(b"dsh-compatibility\0");
            hasher.update((self.dsh_compatibility.len() as u64).to_le_bytes());
            hasher.update(self.dsh_compatibility);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_activation_policy_partitions_known_capabilities() {
        let policy = PluginActivationPolicy::current();
        for capability in PluginActivationCapability::ALL {
            let supported = policy.supported.contains(capability);
            let inactive = policy.inactive.contains(capability);
            assert_ne!(
                supported, inactive,
                "{capability:?} must be supported or inactive, not both or neither"
            );
        }
    }

    fn policy_digest(policy: PluginActivationPolicy) -> String {
        let mut hasher = sha2::Sha256::new();
        policy.write_hash_material(&mut hasher);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Pin the shipping provider-aware policy independently of the optional
    /// native extension host; changes require deliberate receipt migration.
    #[test]
    fn flag_off_policy_hashes_exactly_as_shipping_provider_policy() {
        let _guard = TestPolicyGuard::extension_host(false);
        let current = PluginActivationPolicy::current();
        assert_eq!(current, PluginActivationPolicy::v3());
        assert_eq!(
            policy_digest(current),
            "f0584fdb007b7987b0934517c8723b96190137771795ed33b38499b9353aa7cf"
        );
        assert!(!current.is_supported(PluginActivationCapability::Native));
    }

    #[test]
    fn flag_on_policy_supports_native_and_moves_the_hash() {
        let _guard = TestPolicyGuard::extension_host(true);
        let current = PluginActivationPolicy::current();
        assert_eq!(current.version, EXTENSION_HOST_POLICY_VERSION);
        assert!(current.is_supported(PluginActivationCapability::Native));
        assert_ne!(
            policy_digest(current),
            policy_digest(PluginActivationPolicy::v3())
        );
        let changed = PluginActivationPolicy {
            dsh_compatibility: b"{\"provided_services\":[\"new-service\"]}",
            ..current
        };
        assert_ne!(policy_digest(current), policy_digest(changed));
        let table: serde_json::Value = serde_json::from_slice(DSH_COMPATIBILITY).unwrap();
        assert_eq!(table["version"], 2);
        assert!(dsh_row_bridge("@deepseek-ai/dsh-persona"));
        assert!(!dsh_row_bridge("@deepseek-ai/dsh-agent-loop"));
        for capability in PluginActivationCapability::ALL {
            assert_ne!(
                current.supported.contains(capability),
                current.inactive.contains(capability),
                "{capability:?}"
            );
        }
    }
}
