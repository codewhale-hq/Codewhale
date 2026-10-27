//! Trust tiers of the extension host.
//!
//! One process is one trust domain (design §1.5), so the host runs as two
//! processes that never share code, state or a data directory:
//!
//! * **Plugin** (tier 1): reviewed third-party plugins. Owner ids are the
//!   plugin ids discovery builds (`<scope>/<12 hex>/<name>`). This is the only
//!   tier serves reviewed third-party code independently of the selected
//!   builtin MCP protocol backend.
//! * **Builtin** (tier 0): Codewhale's own host code, so that it never shares
//!   a process with third-party code. Owner ids are `host:<module>`. A module
//!   is admitted by a row of [`BUILTIN_MODULES`], which pins the SHA-256 of its
//!   source and names each of its tools with the approval the core gives it
//!   (`Auto` or `Required`). The table is Rust data: nothing a module says
//!   about itself can lower its approval, and a tool the table does not list is
//!   `Required`.
//!
//! The two id spaces cannot meet. A plugin id starts with a scope name and a
//! manifest name cannot hold `:`, so discovery never builds a `host:` id;
//! [`HostTier::check_owner_id`] is the one place that says so, and the owner
//! registry refuses a `host:` id on the plugin tier and any other id on the
//! builtin tier (`OwnerRegistry::begin_owner`).
//!
//! Production pins the MCP SDK module, activated only when a selected Host
//! stdio connection asks for it. Plugin-tier frames cannot use proc/* or mcp/*.
//! Rust owns launch, exact operation tickets, credentials and decision keys;
//! the builtin is a protocol owner, not an execution or approval authority.
//! The pinned digest detects changed bytes; it does not sandbox code already
//! executing as the current OS user. Sources are embedded and materialized at
//! `<bundle dir>/builtin/<module>.mjs` with the host's notices.

use crate::tools::spec::ApprovalRequirement;

/// Every tier-0 owner id starts with this: `host:<module>`.
pub(crate) const HOST_OWNER_PREFIX: &str = "host:";

/// Which of the two host processes something belongs to. Also the wire value
/// of `host/hello.tier` and of a method's tier allow-list
/// (`protocol::MethodSpec::tiers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub(crate) enum HostTier {
    /// Codewhale's own host code (tier 0).
    Builtin,
    /// Reviewed third-party plugins (tier 1).
    Plugin,
}

impl HostTier {
    /// Plugin first for ordinary extension discovery; builtin stays separate.
    pub(crate) const ALL: [Self; 2] = [Self::Plugin, Self::Builtin];

    /// The value of `--tier=` and of the data directory's name.
    #[must_use]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Plugin => "plugin",
        }
    }

    /// The argument the core launches this tier's host with.
    #[must_use]
    pub(crate) fn argv_flag(self) -> String {
        format!("--tier={}", self.name())
    }

    /// How diagnostics and errors name this tier's host process. The plugin
    /// tier keeps the name it has always had.
    #[must_use]
    pub(crate) fn host_label(self) -> &'static str {
        match self {
            Self::Builtin => "built-in extension host",
            Self::Plugin => "extension host",
        }
    }

    /// The tier an owner id belongs to. Total: the id decides.
    #[must_use]
    pub(crate) fn of_owner_id(owner_id: &str) -> Self {
        if owner_id.starts_with(HOST_OWNER_PREFIX) {
            Self::Builtin
        } else {
            Self::Plugin
        }
    }

    /// Whether `owner_id` may be an owner on this tier, or why not.
    pub(crate) fn check_owner_id(self, owner_id: &str) -> Result<(), String> {
        match self {
            Self::Plugin if owner_id.starts_with(HOST_OWNER_PREFIX) => Err(format!(
                "`{owner_id}` is in the built-in host namespace (`{HOST_OWNER_PREFIX}<module>`); a plugin id can never use it"
            )),
            Self::Plugin => Ok(()),
            Self::Builtin => match owner_id.strip_prefix(HOST_OWNER_PREFIX) {
                Some(module) if valid_module_id(module) => Ok(()),
                _ => Err(format!(
                    "`{owner_id}` is not a built-in host module id: the built-in tier takes only `{HOST_OWNER_PREFIX}<module>` (a lower-case module name of letters, digits and `-`)"
                )),
            },
        }
    }
}

/// `^[a-z][a-z0-9-]{0,63}$`: a module name is also a file name and a
/// directory name, so it is as plain as one.
fn valid_module_id(module: &str) -> bool {
    let mut chars = module.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && module.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// One tool of a built-in module and the approval the core gives it: `Auto` or
/// `Required` (the core's own [`ApprovalRequirement`]; [`tool_approval`] reads
/// anything but `Auto` as `Required`). Only this table can say `Auto`.
#[derive(Debug)]
pub(crate) struct Tier0Tool {
    pub name: &'static str,
    pub approval: ApprovalRequirement,
}

/// One built-in host module: Codewhale's own code, pinned.
#[derive(Debug)]
pub(crate) struct BuiltinModule {
    /// The module name; its owner id is `host:<id>`.
    pub id: &'static str,
    /// SHA-256 (lower-case hex) of the module's source file, as the host
    /// build records it in `dist/builtin-modules.json`. The core refuses to
    /// activate a file with any other digest.
    pub source_sha256: &'static str,
    pub tools: &'static [Tier0Tool],
}

impl BuiltinModule {
    /// The owner id this module activates under.
    #[must_use]
    pub(crate) fn owner_id(&self) -> String {
        format!("{HOST_OWNER_PREFIX}{}", self.id)
    }
}

/// The built-in modules the core pins. MCP is started only by the explicit
/// Host SDK backend. The host build records the same digest; the drift
/// test refuses any row/file mismatch.
pub(crate) const BUILTIN_MODULES: &[BuiltinModule] = &[
    BuiltinModule {
        id: "harness",
        source_sha256: "ad24207f93d963dcffe1967f6863f416117d9acd67cdfccbcef59c12ccb39f92",
        tools: &[],
    },
    BuiltinModule {
        id: "mcp",
        source_sha256: "50516f554ba8786b3282c8f576a11e86df189ac7fd226176a5d1b618042246ea",
        tools: &[],
    },
];

/// The approval for `tool` of the module that owns `owner_id`, from `modules`
/// and nowhere else: `Auto` only where a row says so, `Required` for a module
/// or tool the table does not list and for any row that says anything else.
#[must_use]
pub(crate) fn tool_approval(
    modules: &[BuiltinModule],
    owner_id: &str,
    tool: &str,
) -> ApprovalRequirement {
    let listed = owner_id
        .strip_prefix(HOST_OWNER_PREFIX)
        .and_then(|module_id| modules.iter().find(|module| module.id == module_id))
        .and_then(|module| module.tools.iter().find(|listed| listed.name == tool));
    match listed {
        Some(Tier0Tool {
            approval: ApprovalRequirement::Auto,
            ..
        }) => ApprovalRequirement::Auto,
        _ => ApprovalRequirement::Required,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// The host build (`extension-host/build.mjs`) writes the digest of every
    /// built-in module's source to `dist/builtin-modules.json`; the Rust table
    /// must say exactly the same, both ways: a module with no row, a row with
    /// no module and a changed source all fail here. Same pattern as the
    /// generated-protocol drift test in `protocol/tests.rs`, except the build
    /// is the source of truth, so the fix is to update the table.
    #[test]
    fn table_matches_the_host_build() {
        let built: serde_json::Value = serde_json::from_str(include_str!(
            "../../extension-host/dist/builtin-modules.json"
        ))
        .expect("dist/builtin-modules.json is JSON");
        let built: BTreeMap<String, String> = built["modules"]
            .as_object()
            .expect("`modules` is an object")
            .iter()
            .map(|(id, digest)| (id.clone(), digest.as_str().expect("a digest").to_string()))
            .collect();
        let table: BTreeMap<String, String> = BUILTIN_MODULES
            .iter()
            .map(|module| (module.id.to_string(), module.source_sha256.to_string()))
            .collect();
        assert_eq!(
            table.len(),
            BUILTIN_MODULES.len(),
            "BUILTIN_MODULES lists a module twice"
        );
        assert_eq!(
            table, built,
            "BUILTIN_MODULES and crates/tui/extension-host/dist/builtin-modules.json disagree. \
             Rebuild the host (`npm run build` in crates/tui/extension-host) and make the table \
             list exactly the modules and digests the build recorded."
        );
    }

    #[test]
    fn every_row_is_well_formed() {
        for module in BUILTIN_MODULES {
            assert!(valid_module_id(module.id), "{}", module.id);
            assert!(
                module.source_sha256.len() == 64
                    && module
                        .source_sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{}: not a lower-case SHA-256",
                module.id
            );
            assert_eq!(HostTier::of_owner_id(&module.owner_id()), HostTier::Builtin);
        }
    }

    #[test]
    fn the_two_id_spaces_cannot_meet() {
        assert!(
            HostTier::Plugin
                .check_owner_id("user/0123456789ab/demo")
                .is_ok()
        );
        assert!(HostTier::Plugin.check_owner_id("a").is_ok());
        assert!(HostTier::Builtin.check_owner_id("host:mcp").is_ok());
        for host_id in ["host:mcp", "host:", "host:evil name"] {
            let refused = HostTier::Plugin.check_owner_id(host_id).unwrap_err();
            assert!(
                refused.contains("a plugin id can never use it"),
                "{refused}"
            );
        }
        for id in [
            "user/0123456789ab/demo",
            "mcp",
            "host",
            "host:",
            "host:UPPER",
            "host:../x",
            "host:a/b",
            "HOST:mcp",
        ] {
            let refused = HostTier::Builtin.check_owner_id(id).unwrap_err();
            assert!(
                refused.contains("not a built-in host module id"),
                "{id}: {refused}"
            );
        }
        assert_eq!(HostTier::of_owner_id("host:mcp"), HostTier::Builtin);
        assert_eq!(
            HostTier::of_owner_id("user/0123456789ab/demo"),
            HostTier::Plugin
        );
    }

    #[test]
    fn a_tier_zero_tool_is_required_unless_the_table_says_otherwise() {
        const TOOLS: &[Tier0Tool] = &[
            Tier0Tool {
                name: "open",
                approval: ApprovalRequirement::Auto,
            },
            Tier0Tool {
                name: "write",
                approval: ApprovalRequirement::Required,
            },
            Tier0Tool {
                name: "suggest",
                approval: ApprovalRequirement::Suggest,
            },
        ];
        let table = [BuiltinModule {
            id: "demo",
            source_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            tools: TOOLS,
        }];
        assert_eq!(
            tool_approval(&table, "host:demo", "open"),
            ApprovalRequirement::Auto
        );
        assert_eq!(
            tool_approval(&table, "host:demo", "write"),
            ApprovalRequirement::Required
        );
        // Only `Auto` lowers anything: any other row reads as `Required`.
        assert_eq!(
            tool_approval(&table, "host:demo", "suggest"),
            ApprovalRequirement::Required
        );
        // Unlisted tool, unlisted module, and an id that is not tier 0 at all.
        assert_eq!(
            tool_approval(&table, "host:demo", "other"),
            ApprovalRequirement::Required
        );
        assert_eq!(
            tool_approval(&table, "host:other", "open"),
            ApprovalRequirement::Required
        );
        assert_eq!(
            tool_approval(&table, "demo", "open"),
            ApprovalRequirement::Required
        );
        assert_eq!(
            tool_approval(&[], "host:demo", "open"),
            ApprovalRequirement::Required
        );
    }
}
