//! Owned registrations: the Rust side is the authority (design §3).
//!
//! Every registration belongs to exactly one `(plugin_id, generation)` owner.
//! Revocation is synchronous and never waits for the host: removing an owner
//! removes its tools from every later turn's registry at once, and
//! `HostToolSpec` re-checks liveness before each call. Handles are never
//! reused, so undoing one registration can never touch a newer one.
//!
//! One registry serves both trust tiers ([`super::tier`]): an owner records
//! which host process it lives in, `begin_owner` refuses an id the tier cannot
//! hold (`host:<module>` is tier 0's alone), and a host exit removes only its
//! own tier's owners and registrations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use super::protocol::{RegisterKind, RegisterParams};
use super::tier::HostTier;
use codewhale_config::PluginAuthority;
use codewhale_config::{EntryRef, OwnerRef};

/// Largest accepted tool input schema, serialized.
pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
/// Largest accepted tool description.
pub const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
pub const MAX_TOOLS_PER_OWNER: usize = 128;
pub const MAX_TOOLS_PER_HOST: usize = 1024;
/// Commands are listed in the palette and `/help`, so the caps are tighter.
pub const MAX_COMMANDS_PER_OWNER: usize = 64;
pub const MAX_COMMANDS_PER_HOST: usize = 256;
/// A command description is one palette line; a hint is a short placeholder.
pub const MAX_COMMAND_DESCRIPTION_BYTES: usize = 1024;
pub const MAX_COMMAND_HINT_BYTES: usize = 256;

/// Name prefixes no extension may use: MCP's namespace, and one kept free
/// for future core-issued extension names.
const RESERVED_PREFIXES: &[&str] = &["mcp_", "ext_"];
const NAME_HINT: &str = "use a plugin-specific prefix, for example `myplugin_read_x`";

/// Core tool names that exist outside the native registry builder (catalog
/// meta-tools) and so never show up in a registry snapshot.
const RESERVED_NAMES: &[&str] = &[
    "tool_search",
    "tool_search_tool_regex",
    "tool_search_tool_bm25",
    "retrieve_tool_result",
    "execute_tools",
    "code_execution",
    "js_execution",
    "request_user_input",
    "multi_tool_use.parallel",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerState {
    Activating,
    Active,
    Failed(String),
    Faulted(String),
    Revoked,
}

#[derive(Debug, Clone)]
pub struct OwnerEntry {
    pub owner: OwnerRef,
    /// The host process this owner lives in. Fixed at [`OwnerRegistry::begin_owner`],
    /// which refuses an id the tier cannot hold.
    pub tier: HostTier,
    pub plugin_name: String,
    /// The reviewed plugin authority (plugin tier). A built-in module has none:
    /// what it is bound to is the source digest the Rust table pins, which is
    /// its `content_hash`.
    pub authority: Option<PluginAuthority>,
    pub content_hash: String,
    /// Digest of the plugin config this activation was given (empty until
    /// [`OwnerRegistry::set_config_hash`]). A change of config is a different
    /// activation: reconcile revokes the owner and activates a new generation.
    pub config_hash: String,
    pub state: OwnerState,
    pub scopes: HashMap<EntryRef, OwnerState>,
}

/// Most schema violations one refused call reports back to the model.
const MAX_REPORTED_INPUT_ERRORS: usize = 5;
/// Bound on the refusal text (violations quote the offending values).
const MAX_INPUT_ERROR_BYTES: usize = 2048;

/// A tool's input schema, compiled once at registration. The core checks every
/// call's input against it before anything is sent to the host: a plugin that
/// registers a plain object receives only input its own schema admits, without
/// having to validate it itself.
///
/// Compiled by `jsonschema` (already linked for Workflow `responseSchema`),
/// which resolves no external `$ref` here (no network or file resolver is
/// enabled); a schema it cannot compile is refused at registration.
#[derive(Clone)]
pub struct InputValidator(Arc<jsonschema::Validator>);

impl InputValidator {
    /// Compile `schema`, or say why it cannot be.
    pub fn compile(schema: &Value) -> Result<Self, String> {
        jsonschema::validator_for(schema)
            .map(|validator| Self(Arc::new(validator)))
            .map_err(|error| error.to_string())
    }

    /// `Ok` when `input` satisfies the schema; otherwise the violations, as
    /// text a model can correct its call from.
    pub fn check(&self, input: &Value) -> Result<(), String> {
        let mut errors = self.0.iter_errors(input);
        let Some(first) = errors.next() else {
            return Ok(());
        };
        let describe = |error: &jsonschema::ValidationError<'_>| {
            let at = error.instance_path().to_string();
            if at.is_empty() {
                error.to_string()
            } else {
                format!("{error} (at {at})")
            }
        };
        let mut reasons = vec![describe(&first)];
        let mut more = 0usize;
        for error in errors {
            if reasons.len() < MAX_REPORTED_INPUT_ERRORS {
                reasons.push(describe(&error));
            } else {
                more += 1;
            }
        }
        let mut text = reasons.join("; ");
        if more > 0 {
            text.push_str(&format!("; and {more} more"));
        }
        if text.len() > MAX_INPUT_ERROR_BYTES {
            let mut end = MAX_INPUT_ERROR_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push('…');
        }
        Err(text)
    }
}

impl PartialEq for InputValidator {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for InputValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputValidator").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: Option<EntryRef>,
    /// The tier of `owner`'s host.
    pub tier: HostTier,
    pub plugin_name: String,
    /// The reviewed bundle content hash of the owner that registered it: the
    /// receipt its approval grants are bound to.
    pub content_hash: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// `input_schema`, compiled: every call is checked against it.
    pub input_validator: InputValidator,
}

/// An admitted slash command. Owned exactly like a tool: one owner
/// generation, a never-reused handle, removed with its owner.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: Option<EntryRef>,
    /// The tier of `owner`'s host.
    pub tier: HostTier,
    pub plugin_name: String,
    /// The reviewed bundle content hash of the registering owner.
    pub content_hash: String,
    /// The slash-command name, without the slash (lower case).
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
}

/// A programmable admission listener, owned and revoked like a tool.
#[derive(Debug, Clone, PartialEq)]
pub struct HookRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: Option<EntryRef>,
    pub tier: HostTier,
    pub plugin_name: String,
    pub content_hash: String,
    pub event: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptSectionRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: Option<EntryRef>,
    pub tier: HostTier,
    pub plugin_name: String,
    pub content_hash: String,
    pub id: String,
    pub text: String,
    pub interpolate: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ShellHookRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: Option<EntryRef>,
    pub content_hash: String,
    pub hook: codewhale_config::hooks::Hook,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellSpec {
    dialect: String,
    point: String,
    #[serde(default)]
    matcher: Option<String>,
    hook: codewhale_config::hooks::Hook,
}

#[derive(Debug, Default)]
pub struct OwnerRegistry {
    next_handle: u64,
    next_generation: u64,
    owners: HashMap<String, OwnerEntry>,
    tools: BTreeMap<u64, ToolRegistration>,
    /// Lower-cased tool name → handle, so `Read` cannot impersonate `read`.
    by_name: HashMap<String, u64>,
    commands: BTreeMap<u64, CommandRegistration>,
    /// Command name → handle. Commands and tools are separate namespaces: a
    /// tool is called by the model, a command by the user.
    commands_by_name: HashMap<String, u64>,
    hooks: BTreeMap<u64, HookRegistration>,
    shell_hooks: BTreeMap<u64, ShellHookRegistration>,
    prompt_sections: BTreeMap<u64, PromptSectionRegistration>,
    skill_roots: BTreeMap<u64, super::skills::SkillRootRegistration>,
    avatar_packs: BTreeMap<u64, super::avatars::AvatarRegistration>,
    mcp_servers: BTreeMap<u64, super::native_mcp::McpRegistration>,
    /// Lower-cased names of every native tool any engine's turn build has
    /// reported, plus the static set. Only ever grows: engines in one
    /// process build different native surfaces, and a name that is native
    /// anywhere is refused everywhere.
    native_names: HashSet<String>,
}

fn mint_token() -> String {
    // Two v4 UUIDs: 244 random bits from the OS generator.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn valid_tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// DSH's command grammar (`^[a-z][a-z0-9_-]*$`), bounded. Lower case only:
/// the user's input is lower-cased before it is looked up.
fn valid_command_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Why the core would treat a tool called `name` as something other than an
/// opaque extension tool, if it would.
///
/// Approval keys, approval-card summaries, and the approval/auto-review
/// category are all derived from the tool *name*. A name any of them
/// special-cases would let an extension borrow a native tool's identity: a
/// session grant for `fetch_url` on a host (`net:<host>`) would approve a
/// plugin tool named `web_fetch`, and a `read_*` name would be classified as
/// a read. Such names need not be registered natives (`web_fetch`,
/// `exec_wait`, and mode-dependent tools such as `task_shell_start` are not),
/// so they are refused by probing the classifiers themselves rather than by a
/// hand-kept list that would drift from them.
fn core_special_case(name: &str) -> Option<&'static str> {
    use crate::core::authority::{ToolCategory, get_tool_category_for_call};
    use crate::tools::approval_cache::{build_approval_grouping_key, build_approval_key};
    let empty = Value::Object(serde_json::Map::new());
    let spellings = [name.to_string(), name.to_ascii_lowercase()];
    for spelling in &spellings {
        let generic = format!("tool:{spelling}:");
        if !build_approval_key(spelling, &empty).0.starts_with(&generic)
            || !build_approval_grouping_key(spelling, &empty)
                .0
                .starts_with(&generic)
        {
            return Some("the approval cache keys it as a built-in tool family");
        }
        if codewhale_execpolicy::canonical_action::canonical_action_alias(spelling, &empty)
            != spelling
        {
            return Some("it is an alias of a built-in tool");
        }
        if crate::tools::approval_summary::approval_summary(spelling, &empty, None)
            != format!("Use the {spelling} tool")
        {
            return Some("approval cards describe it as a built-in tool");
        }
        if get_tool_category_for_call(spelling, &empty) != ToolCategory::Unknown {
            return Some(
                "the approval policy classifies it by name (read, write, shell, network, MCP or agent)",
            );
        }
    }
    None
}

impl OwnerRegistry {
    #[must_use]
    pub fn new() -> Self {
        let mut native_names: HashSet<String> = RESERVED_NAMES
            .iter()
            .chain(crate::core::engine::tool_catalog::DEFAULT_ACTIVE_NATIVE_TOOLS)
            .map(|name| name.to_ascii_lowercase())
            .collect();
        for (family, _, alias) in codewhale_execpolicy::canonical_action::CANONICAL_ACTION_ALIASES {
            native_names.insert(family.to_ascii_lowercase());
            native_names.insert(alias.to_ascii_lowercase());
        }
        Self {
            native_names,
            ..Self::default()
        }
    }

    /// Add native tool names from one engine's turn build (the registry
    /// before scripts, plugins or extensions are added). Never removes one.
    pub fn add_native_names<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        self.native_names
            .extend(names.into_iter().map(str::to_ascii_lowercase));
    }

    /// Start a new activation for `plugin_id` on `tier`, superseding any
    /// previous one. Refuses an id the tier cannot hold (a `host:` id on the
    /// plugin tier, any other on the builtin tier) and an authority that does
    /// not fit it: a plugin owner is bound to its reviewed plugin authority, a
    /// built-in module to none (its pinned digest is `content_hash`).
    pub fn begin_owner(
        &mut self,
        tier: HostTier,
        plugin_id: &str,
        plugin_name: &str,
        authority: Option<PluginAuthority>,
        content_hash: &str,
    ) -> Result<OwnerRef, String> {
        tier.check_owner_id(plugin_id)?;
        match (tier, authority.is_some()) {
            (HostTier::Plugin, false) => {
                return Err(format!(
                    "plugin owner `{plugin_id}` needs its reviewed plugin authority"
                ));
            }
            (HostTier::Builtin, true) => {
                return Err(format!(
                    "built-in module `{plugin_id}` has no plugin authority; the digest the Rust table pins is its authority"
                ));
            }
            _ => {}
        }
        self.revoke_owner(plugin_id);
        self.next_generation += 1;
        let owner = OwnerRef {
            plugin_id: plugin_id.to_string(),
            generation: self.next_generation,
            owner_token: mint_token(),
        };
        self.owners.insert(
            plugin_id.to_string(),
            OwnerEntry {
                owner: owner.clone(),
                tier,
                plugin_name: plugin_name.to_string(),
                authority,
                content_hash: content_hash.to_string(),
                config_hash: String::new(),
                state: OwnerState::Activating,
                scopes: HashMap::new(),
            },
        );
        Ok(owner)
    }

    /// Record which config `owner`'s activation was given.
    pub fn set_config_hash(&mut self, owner: &OwnerRef, hash: &str) {
        if let Some(entry) = self.owners.get_mut(&owner.plugin_id)
            && entry.owner == *owner
        {
            entry.config_hash = hash.to_string();
        }
    }

    #[must_use]
    pub fn owner(&self, plugin_id: &str) -> Option<&OwnerEntry> {
        self.owners.get(plugin_id)
    }

    pub fn owners(&self) -> impl Iterator<Item = &OwnerEntry> {
        self.owners.values()
    }

    /// The exact current, not-yet-revoked owner (token and generation match).
    fn current(&self, owner: &OwnerRef) -> Option<&OwnerEntry> {
        self.owners.get(&owner.plugin_id).filter(|entry| {
            entry.owner == *owner
                && matches!(entry.state, OwnerState::Activating | OwnerState::Active)
        })
    }

    pub fn mark_active(&mut self, owner: &OwnerRef) -> bool {
        match self.owners.get_mut(&owner.plugin_id) {
            Some(entry) if entry.owner == *owner && entry.state == OwnerState::Activating => {
                entry.state = OwnerState::Active;
                super::native_mcp::changed();
                super::command::bump_epoch();
                true
            }
            _ => false,
        }
    }

    /// Mark an owner failed or faulted and drop everything it registered.
    pub fn mark_failed(&mut self, owner: &OwnerRef, state: OwnerState) -> bool {
        let matches = self
            .owners
            .get(&owner.plugin_id)
            .is_some_and(|entry| entry.owner == *owner);
        if matches {
            self.remove_registrations_of(&owner.plugin_id);
            if let Some(entry) = self.owners.get_mut(&owner.plugin_id) {
                entry.state = state;
            }
        }
        matches
    }

    /// Only core invokes this after the existing Native inventory/receipt check.
    pub(super) fn begin_scope(&mut self, owner: &OwnerRef, scope: EntryRef) -> Result<(), String> {
        let entry = self
            .owners
            .get_mut(&owner.plugin_id)
            .filter(|entry| entry.owner == *owner)
            .ok_or("scope owner was withdrawn")?;
        if entry.tier != HostTier::Plugin || entry.authority.is_none() {
            return Err("scope requires reviewed Native authority".into());
        }
        if entry.scopes.contains_key(&scope) {
            return Err("scope is already admitted".into());
        }
        entry.scopes.insert(scope, OwnerState::Activating);
        Ok(())
    }
    pub(super) fn mark_scope_active(&mut self, owner: &OwnerRef, scope: &EntryRef) -> bool {
        let Some(entry) = self
            .owners
            .get_mut(&owner.plugin_id)
            .filter(|entry| entry.owner == *owner)
        else {
            return false;
        };
        let Some(state) = entry.scopes.get_mut(scope) else {
            return false;
        };
        if *state != OwnerState::Activating {
            return false;
        }
        *state = OwnerState::Active;
        super::native_mcp::changed();
        super::command::bump_epoch();
        true
    }
    pub(super) fn check_scope(
        &self,
        owner: &OwnerRef,
        scope: Option<&EntryRef>,
        active: bool,
    ) -> Result<(), String> {
        let entry = self.current(owner).ok_or("scope owner was withdrawn")?;
        if scope.is_none() && entry.tier == HostTier::Plugin && !entry.scopes.is_empty() {
            return Err("Native contribution must name its core-admitted entry scope".into());
        }
        if let Some(scope) = scope {
            let state = entry
                .scopes
                .get(scope)
                .ok_or("scope was not admitted by core")?;
            if !matches!(state, OwnerState::Active)
                && (active || !matches!(state, OwnerState::Activating))
            {
                return Err("scope was withdrawn or is not ready".into());
            }
        }
        Ok(())
    }
    pub(super) fn fail_scope(&mut self, owner: &OwnerRef, scope: &EntryRef) {
        let _ = self.revoke_scope(owner, scope);
        if let Some(entry) = self
            .owners
            .get_mut(&owner.plugin_id)
            .filter(|entry| entry.owner == *owner)
        {
            entry.scopes.insert(
                scope.clone(),
                OwnerState::Failed("Native entry activation failed".into()),
            );
        }
    }
    pub(super) fn revoke_scope(&mut self, owner: &OwnerRef, scope: &EntryRef) -> Vec<u64> {
        if let Some(entry) = self
            .owners
            .get_mut(&owner.plugin_id)
            .filter(|entry| entry.owner == *owner)
        {
            entry.scopes.remove(scope);
        }
        let handles: Vec<_> = self
            .tools
            .values()
            .filter(|r| r.owner == *owner && r.scope.as_ref() == Some(scope))
            .map(|r| r.handle)
            .chain(
                self.commands
                    .values()
                    .filter(|r| r.owner == *owner && r.scope.as_ref() == Some(scope))
                    .map(|r| r.handle),
            )
            .chain(
                self.hooks
                    .values()
                    .filter(|r| r.owner == *owner && r.scope.as_ref() == Some(scope))
                    .map(|r| r.handle),
            )
            .chain(
                self.shell_hooks
                    .values()
                    .filter(|r| r.owner == *owner && r.scope.as_ref() == Some(scope))
                    .map(|r| r.handle),
            )
            .chain(
                self.prompt_sections
                    .values()
                    .filter(|r| r.owner == *owner && r.scope.as_ref() == Some(scope))
                    .map(|r| r.handle),
            )
            .chain(
                self.skill_roots
                    .values()
                    .filter(|r| r.owner == *owner && r.scope.as_ref() == Some(scope))
                    .map(|r| r.handle),
            )
            .chain(
                self.avatar_packs
                    .values()
                    .filter(|r| r.owner == *owner && r.scope.as_ref() == Some(scope))
                    .map(|r| r.handle),
            )
            .chain(
                self.mcp_servers
                    .values()
                    .filter(|r| r.owner == *owner && r.scope == *scope)
                    .map(|r| r.handle),
            )
            .collect();
        for handle in &handles {
            self.unregister(owner, *handle);
        }
        super::command::bump_epoch();
        handles
    }

    /// Admit or refuse one `registry/register`, whatever its kind.
    pub fn register(&mut self, params: &RegisterParams) -> Result<u64, String> {
        match params.kind {
            RegisterKind::Tool => self.register_tool(params),
            RegisterKind::Command => self.register_command(params),
            RegisterKind::Hook => self.register_hook(params),
            RegisterKind::ShellHook => self.register_shell_hook(params),
            RegisterKind::PromptSection | RegisterKind::PromptTemplate => {
                self.register_prompt_section(params)
            }
            RegisterKind::McpServer => {
                Err("MCP definitions require reviewed snapshot admission".into())
            }
            RegisterKind::AvatarPack => {
                Err("avatar packs require reviewed snapshot admission".into())
            }
            RegisterKind::SkillRoot => {
                Err("skill roots require reviewed snapshot admission".to_string())
            }
        }
    }

    pub(super) fn register_mcp(
        &mut self,
        params: &RegisterParams,
        config: crate::mcp::McpServerConfig,
        host_generation: u64,
    ) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        params.check_spec()?;
        if params.kind != RegisterKind::McpServer
            || params.spec.description.len() > super::native_mcp::MAX_DEFINITION_BYTES
        {
            return Err("Invalid MCP definition".into());
        }
        let scope = params
            .scope
            .clone()
            .ok_or("MCP definition requires an entry scope")?;
        let owner = self.current(&params.owner).ok_or("MCP owner is stale")?;
        if owner.tier != HostTier::Plugin {
            return Err("MCP definition owner must be Native".into());
        }
        let content_hash = owner.content_hash.clone();
        let owned = self
            .mcp_servers
            .values()
            .filter(|r| r.owner == params.owner)
            .collect::<Vec<_>>();
        if owned.len() >= super::native_mcp::MAX_PER_OWNER
            || self.mcp_servers.len() >= super::native_mcp::MAX_PER_HOST
        {
            return Err("MCP owner or host definition limit reached".into());
        }
        if owned
            .iter()
            .any(|r| r.name == params.spec.name && r.scope == scope)
        {
            return Err("MCP server is already registered in this entry".into());
        }
        self.next_handle += 1;
        let handle = self.next_handle;
        self.mcp_servers.insert(
            handle,
            super::native_mcp::McpRegistration {
                handle,
                owner: params.owner.clone(),
                scope,
                content_hash,
                host_generation,
                name: params.spec.name.clone(),
                config,
                cancel: tokio_util::sync::CancellationToken::new(),
            },
        );
        super::native_mcp::changed();
        Ok(handle)
    }
    pub(super) fn live_mcp(&self) -> Vec<super::native_mcp::McpRegistration> {
        self.mcp_servers
            .values()
            .filter(|r| self.is_live_mcp(r))
            .cloned()
            .collect()
    }
    pub(super) fn is_live_mcp(&self, r: &super::native_mcp::McpRegistration) -> bool {
        self.mcp_servers.get(&r.handle).is_some_and(|current| {
            current.owner == r.owner
                && current.scope == r.scope
                && current.host_generation == r.host_generation
                && current.content_hash == r.content_hash
        }) && self.owners.get(&r.owner.plugin_id).is_some_and(|owner| {
            owner.owner == r.owner
                && owner.state == OwnerState::Active
                && self.check_scope(&r.owner, Some(&r.scope), true).is_ok()
        })
    }

    pub(crate) fn register_avatar(
        &mut self,
        params: &RegisterParams,
        pack: codewhale_ratatui::avatar::Pack,
        png: Vec<Vec<u8>>,
        host_generation: u64,
    ) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        params.check_spec()?;
        if params.kind != RegisterKind::AvatarPack || !params.spec.description.is_empty() {
            return Err("invalid avatar pack registration".into());
        }
        pack.validate()?;
        if png.len() != pack.atlases.len() {
            return Err("avatar pages do not match manifest".into());
        }
        for page in &png {
            pack.validate_png(page)?;
        }
        let entry = self.current(&params.owner).ok_or("stale avatar owner")?;
        if entry.tier != HostTier::Plugin || entry.authority.is_none() {
            return Err("avatar pack owner has no reviewed bundle".into());
        }
        let content_hash = entry.content_hash.clone();
        let owned: Vec<_> = self
            .avatar_packs
            .values()
            .filter(|r| r.owner == params.owner)
            .collect();
        if owned.len() >= 4
            || self.avatar_packs.len() >= 16
            || self
                .avatar_packs
                .values()
                .map(|r| r.png.iter().map(Vec::len).sum::<usize>())
                .sum::<usize>()
                + png.iter().map(Vec::len).sum::<usize>()
                > 32 * 1024 * 1024
            || owned
                .iter()
                .any(|r| r.pack.id == pack.id && r.scope == params.scope)
        {
            return Err("avatar pack duplicate or registration budget exceeded".into());
        }
        self.next_handle += 1;
        let handle = self.next_handle;
        self.avatar_packs.insert(
            handle,
            super::avatars::AvatarRegistration {
                handle,
                owner: params.owner.clone(),
                scope: params.scope.clone(),
                host_generation,
                content_hash,
                pack,
                png: Arc::new(png),
            },
        );
        Ok(handle)
    }

    pub(crate) fn live_avatars(&self) -> Vec<super::avatars::AvatarRegistration> {
        self.avatar_packs
            .values()
            .filter(|r| {
                self.current(&r.owner)
                    .is_some_and(|entry| entry.state == OwnerState::Active)
                    && self.check_scope(&r.owner, r.scope.as_ref(), true).is_ok()
            })
            .cloned()
            .collect()
    }

    pub(crate) fn register_skill_root(
        &mut self,
        params: &RegisterParams,
        snapshots: Vec<crate::plugins::types::PluginSkillSnapshot>,
        host_generation: u64,
    ) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        use super::skills::*;
        params.check_spec()?;
        root_path(&params.spec.name)?;
        if params.kind != RegisterKind::SkillRoot || !params.spec.description.is_empty() {
            return Err("invalid skill root spec".to_string());
        }
        let entry = self
            .current(&params.owner)
            .ok_or_else(|| "stale or unknown skill owner".to_string())?;
        if entry.tier != HostTier::Plugin {
            return Err("skill root owner has no reviewed bundle".to_string());
        }
        let content_hash = entry.content_hash.clone();
        let owned: Vec<_> = self
            .skill_roots
            .values()
            .filter(|root| root.owner == params.owner)
            .collect();
        let bytes = snapshots.iter().map(snapshot_bytes).sum::<usize>();
        if snapshots.is_empty()
            || owned
                .iter()
                .any(|root| root.path == params.spec.name && root.scope == params.scope)
        {
            return Err(
                "skill root is empty or already registered; dispose it before registering it again"
                    .to_string(),
            );
        }
        let names: HashSet<_> = owned
            .iter()
            .filter(|root| root.scope == params.scope)
            .flat_map(|root| root.snapshots.iter().map(|skill| &skill.name))
            .collect();
        if snapshots.iter().any(|skill| names.contains(&skill.name)) {
            return Err("skill name is duplicated across this owner's roots".to_string());
        }
        if owned.len() >= MAX_ROOTS_PER_OWNER
            || self.skill_roots.len() >= MAX_ROOTS_PER_HOST
            || owned.iter().map(|root| root.snapshots.len()).sum::<usize>() + snapshots.len()
                > MAX_SKILLS_PER_OWNER
            || self
                .skill_roots
                .values()
                .map(|root| root.snapshots.len())
                .sum::<usize>()
                + snapshots.len()
                > MAX_SKILLS_PER_HOST
            || owned.iter().map(|root| root.bytes).sum::<usize>() + bytes > MAX_BYTES_PER_OWNER
            || self
                .skill_roots
                .values()
                .map(|root| root.bytes)
                .sum::<usize>()
                + bytes
                > MAX_BYTES_PER_HOST
        {
            return Err(
                "skill root owner or host count/instruction-byte limit reached".to_string(),
            );
        }
        self.next_handle += 1;
        let handle = self.next_handle;
        self.skill_roots.insert(
            handle,
            SkillRootRegistration {
                handle,
                owner: params.owner.clone(),
                scope: params.scope.clone(),
                host_generation,
                content_hash,
                path: params.spec.name.clone(),
                snapshots,
                bytes,
            },
        );
        super::command::bump_epoch();
        Ok(handle)
    }

    pub(crate) fn live_skill_roots(&self) -> Vec<super::skills::SkillRootRegistration> {
        self.skill_roots
            .values()
            .filter(|root| {
                self.owners.get(&root.owner.plugin_id).is_some_and(|entry| {
                    entry.owner == root.owner
                        && entry.state == OwnerState::Active
                        && self
                            .check_scope(&root.owner, root.scope.as_ref(), true)
                            .is_ok()
                })
            })
            .cloned()
            .collect()
    }

    pub(crate) fn is_live_skill_root(
        &self,
        handle: u64,
        plugin_id: &str,
        generation: u64,
        host_generation: u64,
        content_hash: &str,
        state_generation: u64,
    ) -> bool {
        self.skill_roots.get(&handle).is_some_and(|root| {
            root.owner.plugin_id == plugin_id
                && root.owner.generation == generation
                && root.host_generation == host_generation
                && root.content_hash == content_hash
                && self
                    .check_scope(&root.owner, root.scope.as_ref(), true)
                    .is_ok()
                && self.owners.get(plugin_id).is_some_and(|entry| {
                    entry.owner == root.owner
                        && entry.state == OwnerState::Active
                        && entry
                            .authority
                            .as_ref()
                            .is_some_and(|authority| authority.state_generation == state_generation)
                })
        })
    }

    pub(crate) fn register_prompt_section(
        &mut self,
        params: &RegisterParams,
    ) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        use super::prompt::{
            MAX_PROMPT_HOST_BYTES, MAX_PROMPT_OWNER_BYTES, MAX_PROMPT_SECTION_BYTES,
            MAX_PROMPT_SECTIONS_PER_HOST, MAX_PROMPT_SECTIONS_PER_OWNER,
        };
        params.check_spec()?;
        if !matches!(
            params.kind,
            RegisterKind::PromptSection | RegisterKind::PromptTemplate
        ) {
            return Err("prompt registration has an invalid kind".to_string());
        }
        let id = &params.spec.name;
        let text = &params.spec.description;
        if params.kind == RegisterKind::PromptTemplate {
            super::prompt::validate_prompt_template(text)?;
        }
        if !valid_command_name(id)
            || id.len() > 64
            || text.trim().is_empty()
            || text.len() > MAX_PROMPT_SECTION_BYTES
            || text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err("prompt section needs a valid short id and bounded non-empty text without control characters".to_string());
        }
        let entry = self
            .current(&params.owner)
            .ok_or_else(|| "stale or unknown owner".to_string())?;
        let owned: Vec<_> = self
            .prompt_sections
            .values()
            .filter(|section| section.owner == params.owner)
            .collect();
        if owned
            .iter()
            .any(|section| &section.id == id && section.scope == params.scope)
        {
            return Err(
                "prompt section id is already registered; dispose it before registering it again"
                    .to_string(),
            );
        }
        if owned.len() >= MAX_PROMPT_SECTIONS_PER_OWNER
            || self.prompt_sections.len() >= MAX_PROMPT_SECTIONS_PER_HOST
            || owned
                .iter()
                .map(|section| section.text.len())
                .sum::<usize>()
                + text.len()
                > MAX_PROMPT_OWNER_BYTES
            || self
                .prompt_sections
                .values()
                .map(|section| section.text.len())
                .sum::<usize>()
                + text.len()
                > MAX_PROMPT_HOST_BYTES
        {
            return Err("prompt section owner or host byte/registration limit reached".to_string());
        }
        let section = PromptSectionRegistration {
            handle: self.next_handle + 1,
            owner: params.owner.clone(),
            scope: params.scope.clone(),
            tier: entry.tier,
            plugin_name: entry.plugin_name.clone(),
            content_hash: entry.content_hash.clone(),
            id: id.clone(),
            text: text.clone(),
            interpolate: params.kind == RegisterKind::PromptTemplate,
        };
        self.next_handle += 1;
        self.prompt_sections.insert(section.handle, section);
        Ok(self.next_handle)
    }

    fn register_shell_hook(&mut self, params: &RegisterParams) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        params.check_spec()?;
        let entry = self
            .current(&params.owner)
            .ok_or("shell hook owner is stale")?;
        if entry.tier != HostTier::Plugin {
            return Err("shell hooks require reviewed Native code".into());
        }
        if params.spec.description.len() > 64 * 1024 || params.spec.name.len() > 256 {
            return Err("shell hook definition is oversized".into());
        }
        let mut spec: ShellSpec = serde_json::from_str(&params.spec.description)
            .map_err(|_| "invalid shell hook definition")?;
        let expected = match spec.point.as_str() {
            "SessionStart" => codewhale_config::hooks::HookEvent::SessionStart,
            "UserPromptSubmit" => codewhale_config::hooks::HookEvent::MessageSubmit,
            "PreToolUse" => codewhale_config::hooks::HookEvent::ToolCallBefore,
            "PostToolUse" => codewhale_config::hooks::HookEvent::ToolCallAfter,
            "Stop" => codewhale_config::hooks::HookEvent::TurnEnd,
            "SubagentStart" => codewhale_config::hooks::HookEvent::SubagentSpawn,
            "SubagentStop" => codewhale_config::hooks::HookEvent::SubagentComplete,
            _ => return Err("unsupported dialect hook point".into()),
        };
        if !matches!(spec.dialect.as_str(), "claude-code" | "codex")
            || spec.hook.event != expected
            || (spec.dialect == "codex"
                && matches!(spec.point.as_str(), "SubagentStart" | "SubagentStop"))
        {
            return Err("dialect hook point does not match its core event".into());
        }
        if spec.matcher.as_ref().is_some_and(|m| m.len() > 1024)
            || spec.hook.command.len() > 32 * 1024
            || spec.hook.timeout_secs == 0
            || spec.hook.timeout_secs > 86_400
            || spec.hook.background
        {
            return Err("shell hook command/matcher/timeout is not bounded".into());
        }
        if self.shell_hooks.len() >= 1024
            || self
                .shell_hooks
                .values()
                .filter(|h| h.owner == params.owner)
                .count()
                >= 128
        {
            return Err("shell hook registration cap reached".into());
        }
        let authority = entry
            .authority
            .clone()
            .ok_or("Native shell hook authority is absent")?;
        let content_hash = entry.content_hash.clone();
        let handle = self.next_handle + 1;
        spec.hook.native_shell = Some(codewhale_config::hooks::NativeShellHook {
            owner: params.owner.clone(),
            scope: params.scope.clone(),
            handle,
            dialect: spec.dialect,
            point: spec.point,
            matcher: spec.matcher,
        });
        spec.hook.plugin_authority = Some(authority);
        spec.hook.project_authority = None;
        spec.hook.continue_on_error = false;
        self.next_handle = handle;
        self.shell_hooks.insert(
            handle,
            ShellHookRegistration {
                handle,
                owner: params.owner.clone(),
                scope: params.scope.clone(),
                content_hash,
                hook: spec.hook,
            },
        );
        Ok(handle)
    }
    pub(crate) fn live_shell_hooks(&self) -> Vec<ShellHookRegistration> {
        self.shell_hooks
            .values()
            .filter(|h| {
                self.check_shell_hook(h.hook.native_shell.as_ref().expect("Native reference"))
                    .is_ok()
            })
            .cloned()
            .collect()
    }
    pub(crate) fn check_shell_hook(
        &self,
        native: &codewhale_config::hooks::NativeShellHook,
    ) -> Result<(), String> {
        self.check_scope(&native.owner, native.scope.as_ref(), true)?;
        let owner = self
            .current(&native.owner)
            .filter(|o| o.state == OwnerState::Active)
            .ok_or("Native shell hook owner is not active")?;
        self.shell_hooks
            .get(&native.handle)
            .filter(|h| {
                h.owner == native.owner
                    && h.scope == native.scope
                    && h.content_hash == owner.content_hash
            })
            .ok_or("Native shell hook was withdrawn")?;
        Ok(())
    }

    fn register_hook(&mut self, params: &RegisterParams) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        params.check_spec()?;
        if params.spec.name != "tools/pre-execute" {
            return Err("only `tools/pre-execute` admission listeners are supported".to_string());
        }
        let entry = self
            .current(&params.owner)
            .ok_or_else(|| "stale or unknown owner".to_string())?;
        if self.hooks.len() >= 1024
            || self
                .hooks
                .values()
                .filter(|hook| hook.owner == params.owner)
                .count()
                >= 128
        {
            return Err("extension hook registration limit reached".to_string());
        }
        let registration = HookRegistration {
            handle: self.next_handle + 1,
            owner: params.owner.clone(),
            scope: params.scope.clone(),
            tier: entry.tier,
            plugin_name: entry.plugin_name.clone(),
            content_hash: entry.content_hash.clone(),
            event: params.spec.name.clone(),
        };
        self.next_handle += 1;
        self.hooks.insert(registration.handle, registration);
        Ok(self.next_handle)
    }

    /// Admit or refuse one command registration. An extension command never
    /// shadows a built-in command or another plugin's command; a clash with
    /// a user, workspace or manifest (markdown) command is resolved when the
    /// user registry loads (the markdown command wins and the extension
    /// command is not loaded), because only that registry knows the workspace.
    pub fn register_command(&mut self, params: &RegisterParams) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        let entry = self
            .current(&params.owner)
            .ok_or_else(|| "stale or unknown owner".to_string())?;
        let plugin_name = entry.plugin_name.clone();
        let content_hash = entry.content_hash.clone();
        let tier = entry.tier;
        let spec = &params.spec;
        let name = spec.name.as_str();
        const COMMAND_HINT: &str = "a command name is lower case, starts with a letter, and uses only a-z, 0-9, `_` and `-` (at most 64 characters)";
        if !valid_command_name(name) {
            return Err(format!(
                "command name `{}` is invalid: {COMMAND_HINT}",
                codewhale_runtime::safe_label::SafeLabel::identifier(name)
            ));
        }
        // Fail closed: with no catalog installed there is nothing to check the
        // name against, so the registration is refused, not accepted unchecked.
        let Some(catalog) = crate::command_catalog::installed() else {
            return Err(format!(
                "command `/{name}` cannot be checked against the built-in commands: no built-in command catalog is installed, so extension commands are refused"
            ));
        };
        if catalog.answers_to(name) {
            return Err(format!(
                "command `/{name}` collides with a built-in command; extensions never shadow core commands; use a plugin-specific name, for example `/myplugin-{name}`"
            ));
        }
        if spec.input_schema.is_some() {
            return Err(format!("command `/{name}` has no input schema"));
        }
        let description = spec.description.trim();
        if description.is_empty() {
            return Err(format!("command `/{name}` needs a description"));
        }
        if description.len() > MAX_COMMAND_DESCRIPTION_BYTES {
            return Err(format!(
                "command `/{name}` description exceeds {MAX_COMMAND_DESCRIPTION_BYTES} bytes"
            ));
        }
        let hint = spec.argument_hint.as_deref().map(str::trim);
        if hint.is_some_and(str::is_empty) {
            return Err(format!("command `/{name}` argument hint must not be empty"));
        }
        if hint.is_some_and(|hint| hint.len() > MAX_COMMAND_HINT_BYTES) {
            return Err(format!(
                "command `/{name}` argument hint exceeds {MAX_COMMAND_HINT_BYTES} bytes"
            ));
        }
        // The palette, `/help` and the composer print these verbatim.
        if description.chars().any(char::is_control)
            || hint.is_some_and(|h| h.chars().any(char::is_control))
        {
            return Err(format!(
                "command `/{name}` description and argument hint must be single-line text without control characters"
            ));
        }
        let mut replaced = None;
        if let Some(existing) = self
            .commands_by_name
            .get(&super::composition_scope::name_key(
                &params.owner.plugin_id,
                name,
                params.scope.as_ref(),
            ))
            .and_then(|handle| self.commands.get(handle))
        {
            if existing.owner.plugin_id != params.owner.plugin_id {
                return Err(format!(
                    "command `/{name}` is already registered by extension `{}`; use a plugin-specific name",
                    existing.plugin_name
                ));
            }
            // Same owner re-registering a name: the new handle retires the old.
            replaced = Some(existing.handle);
        }
        let owned = self
            .commands
            .values()
            .filter(|command| command.owner.plugin_id == params.owner.plugin_id)
            .count()
            - usize::from(replaced.is_some());
        if owned >= MAX_COMMANDS_PER_OWNER {
            return Err(format!(
                "an extension may register at most {MAX_COMMANDS_PER_OWNER} commands"
            ));
        }
        if self.commands.len() - usize::from(replaced.is_some()) >= MAX_COMMANDS_PER_HOST {
            return Err(format!(
                "the extension host holds at most {MAX_COMMANDS_PER_HOST} commands"
            ));
        }
        if let Some(old) = replaced {
            self.commands.remove(&old);
        }
        self.next_handle += 1;
        let handle = self.next_handle;
        self.commands.insert(
            handle,
            CommandRegistration {
                handle,
                owner: params.owner.clone(),
                scope: params.scope.clone(),
                tier,
                plugin_name,
                content_hash,
                name: name.to_string(),
                description: description.to_string(),
                argument_hint: hint.map(str::to_string),
            },
        );
        self.commands_by_name.insert(
            super::composition_scope::name_key(
                &params.owner.plugin_id,
                name,
                params.scope.as_ref(),
            ),
            handle,
        );
        super::command::bump_epoch();
        Ok(handle)
    }

    /// Admit or refuse one tool registration.
    pub fn register_tool(&mut self, params: &RegisterParams) -> Result<u64, String> {
        self.check_scope(&params.owner, params.scope.as_ref(), false)?;
        let entry = self
            .current(&params.owner)
            .ok_or_else(|| "stale or unknown owner".to_string())?;
        let plugin_name = entry.plugin_name.clone();
        let content_hash = entry.content_hash.clone();
        let tier = entry.tier;
        let spec = &params.spec;
        let name = spec.name.as_str();
        if !valid_tool_name(name) {
            return Err(format!(
                "tool name `{}` must match ^[A-Za-z][A-Za-z0-9_-]{{0,63}}$; {NAME_HINT}",
                codewhale_runtime::safe_label::SafeLabel::identifier(name)
            ));
        }
        let plain_key = name.to_ascii_lowercase();
        let key = super::composition_scope::name_key(
            &params.owner.plugin_id,
            &plain_key,
            params.scope.as_ref(),
        );
        if RESERVED_PREFIXES
            .iter()
            .any(|prefix| plain_key.starts_with(prefix))
        {
            return Err(format!(
                "tool name `{name}` uses a reserved prefix; {NAME_HINT}"
            ));
        }
        if self.native_names.contains(&plain_key) {
            return Err(format!(
                "tool name `{name}` collides with a built-in tool; extensions never shadow core tools; {NAME_HINT}"
            ));
        }
        if let Some(reason) = core_special_case(name) {
            return Err(format!(
                "tool name `{name}` is reserved: {reason}; extension tools never borrow a built-in's approval identity; {NAME_HINT}"
            ));
        }
        if spec.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(format!(
                "tool `{name}` description exceeds {MAX_DESCRIPTION_BYTES} bytes"
            ));
        }
        let schema = Value::Object(
            spec.input_schema
                .clone()
                .ok_or_else(|| format!("tool `{name}` needs an input schema"))?,
        );
        let schema_bytes = serde_json::to_vec(&schema)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if schema_bytes > MAX_SCHEMA_BYTES {
            return Err(format!(
                "tool `{name}` input schema exceeds {MAX_SCHEMA_BYTES} bytes"
            ));
        }
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(format!(
                "tool `{name}` input schema must be a JSON object schema (`\"type\": \"object\"`)"
            ));
        }
        let input_validator = InputValidator::compile(&schema).map_err(|reason| {
            format!("tool `{name}` input schema is not a valid JSON Schema: {reason}")
        })?;
        let mut replaced = None;
        if let Some(existing) = self
            .by_name
            .get(&key)
            .and_then(|handle| self.tools.get(handle))
        {
            if existing.owner.plugin_id != params.owner.plugin_id {
                return Err(format!(
                    "tool name `{name}` is already registered by extension `{}`; {NAME_HINT}",
                    existing.plugin_name
                ));
            }
            // Same owner re-registering a name: the new handle retires the old.
            replaced = Some(existing.handle);
        }
        let owned = self
            .tools
            .values()
            .filter(|tool| tool.owner.plugin_id == params.owner.plugin_id)
            .count()
            - usize::from(replaced.is_some());
        if owned >= MAX_TOOLS_PER_OWNER {
            return Err(format!(
                "an extension may register at most {MAX_TOOLS_PER_OWNER} tools"
            ));
        }
        if self.tools.len() - usize::from(replaced.is_some()) >= MAX_TOOLS_PER_HOST {
            return Err(format!(
                "the extension host holds at most {MAX_TOOLS_PER_HOST} tools"
            ));
        }
        if let Some(old) = replaced {
            self.tools.remove(&old);
        }
        self.next_handle += 1;
        let handle = self.next_handle;
        self.tools.insert(
            handle,
            ToolRegistration {
                handle,
                owner: params.owner.clone(),
                scope: params.scope.clone(),
                tier,
                plugin_name,
                content_hash,
                name: name.to_string(),
                description: spec.description.clone(),
                input_schema: schema,
                input_validator,
            },
        );
        self.by_name.insert(key, handle);
        Ok(handle)
    }

    /// Undo exactly one registration. Idempotent; a stale or foreign handle is a no-op.
    pub fn unregister(&mut self, owner: &OwnerRef, handle: u64) {
        if self
            .mcp_servers
            .get(&handle)
            .is_some_and(|r| r.owner == *owner)
        {
            if let Some(r) = self.mcp_servers.remove(&handle) {
                r.cancel.cancel();
            }
            super::native_mcp::changed();
            return;
        }

        if self
            .shell_hooks
            .get(&handle)
            .is_some_and(|h| h.owner == *owner)
        {
            self.shell_hooks.remove(&handle);
            return;
        }
        if self
            .avatar_packs
            .get(&handle)
            .is_some_and(|r| r.owner == *owner)
        {
            self.avatar_packs.remove(&handle);
            return;
        }
        if self
            .skill_roots
            .get(&handle)
            .is_some_and(|root| root.owner == *owner)
        {
            self.skill_roots.remove(&handle);
            super::command::bump_epoch();
            return;
        }
        if self
            .prompt_sections
            .get(&handle)
            .is_some_and(|section| section.owner == *owner)
        {
            self.prompt_sections.remove(&handle);
            return;
        }
        if self
            .hooks
            .get(&handle)
            .is_some_and(|hook| hook.owner == *owner)
        {
            self.hooks.remove(&handle);
            return;
        }
        if self
            .commands
            .get(&handle)
            .is_some_and(|command| command.owner == *owner)
            && let Some(command) = self.commands.remove(&handle)
        {
            if self
                .commands_by_name
                .get(&super::composition_scope::name_key(
                    &command.owner.plugin_id,
                    &command.name,
                    command.scope.as_ref(),
                ))
                == Some(&handle)
            {
                self.commands_by_name
                    .remove(&super::composition_scope::name_key(
                        &command.owner.plugin_id,
                        &command.name,
                        command.scope.as_ref(),
                    ));
            }
            super::command::bump_epoch();
            return;
        }
        let owned = self
            .tools
            .get(&handle)
            .is_some_and(|tool| tool.owner == *owner);
        if !owned {
            return;
        }
        if let Some(tool) = self.tools.remove(&handle) {
            let key = super::composition_scope::name_key(
                &tool.owner.plugin_id,
                &tool.name.to_ascii_lowercase(),
                tool.scope.as_ref(),
            );
            if self.by_name.get(&key) == Some(&handle) {
                self.by_name.remove(&key);
            }
        }
    }

    fn remove_commands_of(&mut self, plugin_id: &str) {
        // Called by `remove_registrations_of`, so every revocation path that
        // drops an owner's tools drops its commands too.
        let handles: Vec<u64> = self
            .commands
            .values()
            .filter(|command| command.owner.plugin_id == plugin_id)
            .map(|command| command.handle)
            .collect();
        for handle in handles {
            if let Some(command) = self.commands.remove(&handle)
                && self
                    .commands_by_name
                    .get(&super::composition_scope::name_key(
                        &command.owner.plugin_id,
                        &command.name,
                        command.scope.as_ref(),
                    ))
                    == Some(&handle)
            {
                self.commands_by_name
                    .remove(&super::composition_scope::name_key(
                        &command.owner.plugin_id,
                        &command.name,
                        command.scope.as_ref(),
                    ));
            }
        }
        super::command::bump_epoch();
    }

    fn remove_registrations_of(&mut self, plugin_id: &str) -> Vec<u64> {
        let count = self.mcp_servers.len();
        self.mcp_servers.retain(|_, r| {
            if r.owner.plugin_id == plugin_id {
                r.cancel.cancel();
                false
            } else {
                true
            }
        });
        if count != self.mcp_servers.len() {
            super::native_mcp::changed()
        }

        self.shell_hooks
            .retain(|_, h| h.owner.plugin_id != plugin_id);
        self.skill_roots
            .retain(|_, root| root.owner.plugin_id != plugin_id);
        self.avatar_packs
            .retain(|_, r| r.owner.plugin_id != plugin_id);
        self.prompt_sections
            .retain(|_, section| section.owner.plugin_id != plugin_id);
        self.hooks
            .retain(|_, hook| hook.owner.plugin_id != plugin_id);
        self.remove_commands_of(plugin_id);
        let handles: Vec<u64> = self
            .tools
            .values()
            .filter(|tool| tool.owner.plugin_id == plugin_id)
            .map(|tool| tool.handle)
            .collect();
        for handle in &handles {
            if let Some(tool) = self.tools.remove(handle) {
                let key = super::composition_scope::name_key(
                    &tool.owner.plugin_id,
                    &tool.name.to_ascii_lowercase(),
                    tool.scope.as_ref(),
                );
                if self.by_name.get(&key) == Some(handle) {
                    self.by_name.remove(&key);
                }
            }
        }
        handles
    }

    /// Revoke an owner synchronously. Returns the owner that was live, if any.
    pub fn revoke_owner(&mut self, plugin_id: &str) -> Option<OwnerRef> {
        self.remove_registrations_of(plugin_id);
        let entry = self.owners.get_mut(plugin_id)?;
        let was_live = matches!(entry.state, OwnerState::Activating | OwnerState::Active);
        entry.state = OwnerState::Revoked;
        was_live.then(|| entry.owner.clone())
    }

    /// Forget an owner entirely (after revocation, when its plugin is gone).
    pub fn forget_owner(&mut self, plugin_id: &str) {
        self.remove_registrations_of(plugin_id);
        self.owners.remove(plugin_id);
    }

    /// Forget owners that are not live (failed, faulted, revoked) so a new
    /// explicit plugin mutation retries them.
    pub fn forget_inactive(&mut self) {
        for owner in self.owners.values_mut() {
            owner
                .scopes
                .retain(|_, state| matches!(state, OwnerState::Activating | OwnerState::Active));
        }
        self.owners
            .retain(|_, entry| matches!(entry.state, OwnerState::Activating | OwnerState::Active));
    }

    /// Drop every registration owned by `tier`'s host: the host that held
    /// them is gone, and the other tier's host is not.
    fn clear_tier_registrations(&mut self, tier: HostTier) {
        if tier == HostTier::Plugin && !self.mcp_servers.is_empty() {
            for r in self.mcp_servers.values() {
                r.cancel.cancel();
            }
            self.mcp_servers.clear();
            super::native_mcp::changed()
        }

        if tier == HostTier::Plugin {
            self.skill_roots.clear();
            self.avatar_packs.clear();
        }
        self.prompt_sections
            .retain(|_, section| section.tier != tier);
        if tier == HostTier::Plugin {
            self.shell_hooks.clear();
        }
        self.hooks.retain(|_, hook| hook.tier != tier);
        self.tools.retain(|_, tool| tool.tier != tier);
        let tools = &self.tools;
        self.by_name.retain(|_, handle| tools.contains_key(handle));
        self.commands.retain(|_, command| command.tier != tier);
        let commands = &self.commands;
        self.commands_by_name
            .retain(|_, handle| commands.contains_key(handle));
        super::command::bump_epoch();
    }

    /// `tier`'s host exited: its crash drops its live registrations, preserves
    /// its failed/faulted receipts, and blames its sole activating owner.
    /// Other owners of that tier are replayable only after reconciliation
    /// verifies their current persisted authority again. The other tier's
    /// owners are not touched: they live in another process.
    pub fn host_exited(&mut self, tier: HostTier, reason: &str) {
        self.clear_tier_registrations(tier);
        let activating: Vec<_> = self
            .owners
            .values()
            .filter(|entry| entry.tier == tier && entry.state == OwnerState::Activating)
            .map(|entry| entry.owner.plugin_id.clone())
            .collect();
        if let [plugin] = activating.as_slice() {
            self.owners.get_mut(plugin).expect("activating owner").state =
                OwnerState::Failed(format!("host crashed during activation: {reason}"));
        }
        self.owners.retain(|_, entry| {
            entry.tier != tier
                || matches!(entry.state, OwnerState::Failed(_) | OwnerState::Faulted(_))
        });
    }

    /// Planned test shutdown drops `tier`'s tools and fails its remaining live owners.
    #[cfg(test)]
    pub fn revoke_all(&mut self, tier: HostTier, reason: &str) {
        self.clear_tier_registrations(tier);
        for entry in self.owners.values_mut() {
            if entry.tier == tier
                && matches!(entry.state, OwnerState::Activating | OwnerState::Active)
            {
                entry.state = OwnerState::Failed(reason.to_string());
            }
        }
    }

    /// Tools of active owners, in handle order.
    #[must_use]
    pub fn live_tools(&self) -> Vec<ToolRegistration> {
        self.tools
            .values()
            .filter(|tool| {
                self.owners.get(&tool.owner.plugin_id).is_some_and(|entry| {
                    entry.owner == tool.owner
                        && entry.state == OwnerState::Active
                        && self
                            .check_scope(&tool.owner, tool.scope.as_ref(), true)
                            .is_ok()
                })
            })
            .cloned()
            .collect()
    }

    /// Hooks of active owners, in registration order. Multiple listeners for
    /// the same event coexist; withdrawing one never removes another.
    pub fn live_hooks(&self) -> Vec<HookRegistration> {
        self.hooks
            .values()
            .filter(|hook| self.is_live_hook(hook.handle, &hook.owner))
            .cloned()
            .collect()
    }

    pub fn live_prompt_sections(&self) -> Vec<PromptSectionRegistration> {
        let mut sections: Vec<_> = self
            .prompt_sections
            .values()
            .filter(|section| self.is_live_prompt_section(section.handle, &section.owner))
            .cloned()
            .collect();
        sections.sort_by(|a, b| (&a.owner.plugin_id, &a.id).cmp(&(&b.owner.plugin_id, &b.id)));
        sections
    }

    pub fn is_live_prompt_section(&self, handle: u64, owner: &OwnerRef) -> bool {
        self.prompt_sections.get(&handle).is_some_and(|section| {
            section.owner == *owner
                && self
                    .check_scope(owner, section.scope.as_ref(), true)
                    .is_ok()
        }) && self
            .owners
            .get(&owner.plugin_id)
            .is_some_and(|entry| entry.owner == *owner && entry.state == OwnerState::Active)
    }

    pub fn is_live_hook(&self, handle: u64, owner: &OwnerRef) -> bool {
        self.hooks.get(&handle).is_some_and(|hook| {
            hook.owner == *owner && self.check_scope(owner, hook.scope.as_ref(), true).is_ok()
        }) && self
            .owners
            .get(&owner.plugin_id)
            .is_some_and(|entry| entry.owner == *owner && entry.state == OwnerState::Active)
    }

    /// Commands of active owners, in handle order.
    #[must_use]
    pub fn live_commands(&self) -> Vec<CommandRegistration> {
        self.commands
            .values()
            .filter(|command| {
                self.owners
                    .get(&command.owner.plugin_id)
                    .is_some_and(|entry| {
                        entry.owner == command.owner
                            && entry.state == OwnerState::Active
                            && self
                                .check_scope(&command.owner, command.scope.as_ref(), true)
                                .is_ok()
                    })
            })
            .cloned()
            .collect()
    }

    /// The command behind `handle`, if it is still admitted for exactly this
    /// owner generation of `plugin_id`. A stale reference finds nothing.
    #[must_use]
    pub fn live_command(
        &self,
        handle: u64,
        plugin_id: &str,
        generation: u64,
    ) -> Option<CommandRegistration> {
        let command = self.commands.get(&handle)?;
        (command.owner.plugin_id == plugin_id
            && command.owner.generation == generation
            && self.owners.get(plugin_id).is_some_and(|entry| {
                entry.owner == command.owner
                    && entry.state == OwnerState::Active
                    && self
                        .check_scope(&command.owner, command.scope.as_ref(), true)
                        .is_ok()
            }))
        .then(|| command.clone())
    }

    /// Whether `handle` is still admitted for exactly this owner generation.
    #[must_use]
    pub fn is_live(&self, handle: u64, owner: &OwnerRef) -> bool {
        self.tools.get(&handle).is_some_and(|tool| {
            tool.owner == *owner && self.check_scope(owner, tool.scope.as_ref(), true).is_ok()
        }) && self
            .owners
            .get(&owner.plugin_id)
            .is_some_and(|entry| entry.owner == *owner && entry.state == OwnerState::Active)
    }

    /// Active owners other than `plugin_id` sharing its host process (the
    /// other owners of its tier).
    #[must_use]
    pub fn other_active_owners(&self, plugin_id: &str) -> usize {
        let tier = HostTier::of_owner_id(plugin_id);
        self.owners
            .values()
            .filter(|entry| {
                entry.tier == tier
                    && entry.owner.plugin_id != plugin_id
                    && entry.state == OwnerState::Active
            })
            .count()
    }

    /// The plugin authority of the exact current owner: `None` for a stale
    /// owner and for a built-in module, which has none.
    #[must_use]
    pub fn authority_for(&self, owner: &OwnerRef) -> Option<PluginAuthority> {
        self.current(owner)
            .and_then(|entry| entry.authority.clone())
    }

    /// The tier of the exact current owner.
    #[must_use]
    pub fn tier_of(&self, owner: &OwnerRef) -> Option<HostTier> {
        self.current(owner).map(|entry| entry.tier)
    }
}

#[cfg(test)]
mod shell_hook_tests {
    use super::super::protocol::RegisterSpecWire;
    use super::*;
    fn spec(owner: &OwnerRef, scope: Option<EntryRef>) -> RegisterParams {
        RegisterParams {owner:owner.clone(),scope,kind:RegisterKind::ShellHook,spec:RegisterSpecWire {name:"claude:PreToolUse:1".into(),description:serde_json::json!({"dialect":"claude-code","point":"PreToolUse","matcher":"write","hook":{"event":"tool_call_before","command":"true","timeout_secs":3,"background":false,"continue_on_error":false}}).to_string(),input_schema:None,argument_hint:None}}
    }
    fn new_owner(registry: &mut OwnerRegistry, id: &str) -> OwnerRef {
        registry
            .begin_owner(
                HostTier::Plugin,
                id,
                id,
                Some(super::super::tests::fake_authority(id)),
                "build",
            )
            .unwrap()
    }
    #[test]
    fn native_shell_hooks_withdraw_exact_scope_and_host_exit_releases_budget() {
        let mut registry = OwnerRegistry::new();
        let owner = new_owner(&mut registry, "hook-a");
        let scope = EntryRef {
            path: "/reviewed/a.mjs".into(),
            sha256: "a".repeat(64),
        };
        registry.begin_scope(&owner, scope.clone()).unwrap();
        let handle = registry
            .register(&spec(&owner, Some(scope.clone())))
            .unwrap();
        assert!(registry.live_shell_hooks().is_empty());
        registry.mark_scope_active(&owner, &scope);
        registry.mark_active(&owner);
        let native = registry.live_shell_hooks()[0]
            .hook
            .native_shell
            .clone()
            .unwrap();
        assert_eq!(native.handle, handle);
        registry.host_exited(HostTier::Builtin, "builtin test exit");
        assert!(registry.check_shell_hook(&native).is_ok());
        registry.revoke_scope(&owner, &scope);
        assert!(registry.check_shell_hook(&native).is_err());
        assert!(registry.shell_hooks.is_empty());
        let other = new_owner(&mut registry, "hook-b");
        let admitted = registry.register(&spec(&other, None)).unwrap();
        registry.mark_active(&other);
        assert_eq!(registry.live_shell_hooks()[0].handle, admitted);
        registry.host_exited(HostTier::Plugin, "test crash");
        assert!(registry.shell_hooks.is_empty());
    }
    #[test]
    fn native_shell_registration_refuses_wrong_event_oversized_and_foreign_withdrawal() {
        let mut registry = OwnerRegistry::new();
        let a = new_owner(&mut registry, "a");
        let b = new_owner(&mut registry, "b");
        let mut wrong = spec(&a, None);
        wrong.spec.description = wrong
            .spec
            .description
            .replace("tool_call_before", "shell_env");
        assert!(registry.register(&wrong).is_err());
        let mut huge = spec(&a, None);
        huge.spec.description = "x".repeat(65537);
        assert!(registry.register(&huge).is_err());
        let handle = registry.register(&spec(&a, None)).unwrap();
        registry.mark_active(&a);
        registry.unregister(&b, handle);
        assert_eq!(registry.live_shell_hooks().len(), 1);
        registry.unregister(&a, handle);
        assert!(registry.live_shell_hooks().is_empty());
    }
}
