use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::Result;
use codewhale_config::AppMode;

use crate::commands::{self, CommandInfo, CommandResult};
use crate::config::{Config, ProviderIdentity, ProviderKind};
use crate::provider_lake::models_for_provider;
use crate::tui::app::{App, AppAction};
use crate::tui::command_palette::{
    CommandPaletteView, build_entries_with_plugins as build_command_palette_entries,
};
use codewhale_localization::{Locale, MessageId, tr};

pub const HOTBAR_COMPACT_LABEL_MAX_WIDTH: usize = 7;

/// Result of firing a hotbar action.
#[allow(clippy::large_enum_variant)] // AppAction is intentionally large; boxing would force clone churn on the hot path
#[derive(Debug, Clone, PartialEq)]
pub enum HotbarDispatch {
    /// The action was fully handled by mutating [`App`].
    Handled,
    /// The event loop must handle an existing application action.
    AppAction(AppAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HotbarActionCategory {
    App,
    Route,
    Slash,
    Mcp,
    Skill,
    Plugin,
}

impl HotbarActionCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Route => "route",
            Self::Slash => "slash",
            Self::Mcp => "mcp",
            Self::Skill => "skill",
            Self::Plugin => "plugin",
        }
    }

    #[must_use]
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "app" => Some(Self::App),
            "route" => Some(Self::Route),
            "slash" => Some(Self::Slash),
            "mcp" => Some(Self::Mcp),
            "skill" => Some(Self::Skill),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotbarArgsBehavior {
    None,
    Optional,
    Required,
}

impl HotbarArgsBehavior {
    #[must_use]
    fn for_command(info: &CommandInfo) -> Self {
        if info.requires_required_argument() {
            Self::Required
        } else if info.requires_argument() {
            Self::Optional
        } else {
            Self::None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotbarSafetyClass {
    LocalUi,
    LocalState,
    ConfigChange,
    ExternalInput,
    ExistingCommand,
    RequiresApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotbarRecommendation {
    Default,
    Eligible,
    Advanced,
}

impl HotbarRecommendation {
    #[must_use]
    pub const fn is_recommendable(self) -> bool {
        matches!(self, Self::Default | Self::Eligible)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotbarActionMetadata {
    pub id: String,
    pub source_id: String,
    pub display_name: String,
    pub compact_label: String,
    pub description: String,
    pub category: HotbarActionCategory,
    pub args: HotbarArgsBehavior,
    pub safety: HotbarSafetyClass,
    pub recommendation: HotbarRecommendation,
}

impl HotbarActionMetadata {
    #[must_use]
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.id.trim().is_empty() {
            errors.push("id must not be empty".to_string());
        }
        if self.source_id.trim().is_empty() {
            errors.push(format!("{} source_id must not be empty", self.id));
        }
        if self.display_name.trim().is_empty() {
            errors.push(format!("{} display_name must not be empty", self.id));
        }
        if self.compact_label.trim().is_empty() {
            errors.push(format!("{} compact_label must not be empty", self.id));
        }
        if unicode_width::UnicodeWidthStr::width(self.compact_label.as_str())
            > HOTBAR_COMPACT_LABEL_MAX_WIDTH
        {
            errors.push(format!(
                "{} compact_label {:?} exceeds {} display cells",
                self.id, self.compact_label, HOTBAR_COMPACT_LABEL_MAX_WIDTH
            ));
        }
        if self.description.trim().is_empty() {
            errors.push(format!("{} description must not be empty", self.id));
        }
        errors
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotbarRecommendationEntry {
    pub metadata: HotbarActionMetadata,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotbarRecommendationOptions {
    pub max_total: usize,
    pub max_eligible_per_category: usize,
    pub include_required_args: bool,
}

impl HotbarRecommendationOptions {
    #[must_use]
    pub const fn for_setup_wizard() -> Self {
        Self {
            max_total: usize::MAX,
            max_eligible_per_category: usize::MAX,
            include_required_args: false,
        }
    }
}

impl Default for HotbarRecommendationOptions {
    fn default() -> Self {
        Self {
            max_total: usize::from(codewhale_config::HOTBAR_SLOT_COUNT),
            max_eligible_per_category: usize::MAX,
            include_required_args: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotbarSourceDispatchBoundary {
    /// The action is handled directly by existing in-app state mutation.
    DirectApp,
    /// The action routes through the existing provider/model picker apply path.
    ModelRoute,
    /// The action routes through the slash command registry/dispatcher.
    SlashCommand,
    /// The action only prefills the composer with a reference; nothing
    /// executes until the user reviews and sends the message themselves.
    ComposerPrefill,
    /// The source is visible as a future hotbar source, but binding/dispatch is
    /// intentionally deferred until its safety contract is wired.
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotbarSourceSafetyMode {
    /// Pressing the bound hotbar slot directly fires the existing action path.
    DirectFire,
    /// Pressing the bound hotbar slot opens/prefills the composer for arguments.
    ComposerPrefill,
    /// The source must not register bindable actions until its gates are wired.
    Disabled,
    /// The source may dispatch only through an approval/trust-enforced path.
    ApprovalGated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotbarSourceDescriptor {
    pub category: HotbarActionCategory,
    pub boundary: HotbarSourceDispatchBoundary,
    pub safety_modes: &'static [HotbarSourceSafetyMode],
    pub dispatch_path: &'static str,
    pub status: &'static str,
}

impl HotbarSourceDescriptor {
    #[must_use]
    pub fn registers_dispatchable_actions(self) -> bool {
        self.boundary != HotbarSourceDispatchBoundary::Deferred
            && !self
                .safety_modes
                .contains(&HotbarSourceSafetyMode::Disabled)
    }
}

const HOTBAR_DIRECT_APP_SAFETY: &[HotbarSourceSafetyMode] = &[HotbarSourceSafetyMode::DirectFire];
const HOTBAR_ROUTE_SAFETY: &[HotbarSourceSafetyMode] = &[HotbarSourceSafetyMode::DirectFire];
const HOTBAR_SLASH_SAFETY: &[HotbarSourceSafetyMode] = &[
    HotbarSourceSafetyMode::DirectFire,
    HotbarSourceSafetyMode::ComposerPrefill,
];
const HOTBAR_MCP_SAFETY: &[HotbarSourceSafetyMode] = &[HotbarSourceSafetyMode::ComposerPrefill];
const HOTBAR_SKILL_SAFETY: &[HotbarSourceSafetyMode] = &[HotbarSourceSafetyMode::DirectFire];
const HOTBAR_DEFERRED_SAFETY: &[HotbarSourceSafetyMode] = &[
    HotbarSourceSafetyMode::Disabled,
    HotbarSourceSafetyMode::ApprovalGated,
];

const HOTBAR_SOURCE_DESCRIPTORS: &[HotbarSourceDescriptor] = &[
    HotbarSourceDescriptor {
        category: HotbarActionCategory::App,
        boundary: HotbarSourceDispatchBoundary::DirectApp,
        safety_modes: HOTBAR_DIRECT_APP_SAFETY,
        dispatch_path: "AppHotbarAction::dispatch",
        status: "dispatchable",
    },
    HotbarSourceDescriptor {
        category: HotbarActionCategory::Route,
        boundary: HotbarSourceDispatchBoundary::ModelRoute,
        safety_modes: HOTBAR_ROUTE_SAFETY,
        dispatch_path: "AppAction::SwitchModelRoute -> apply_model_picker_choice",
        status: "dispatchable",
    },
    HotbarSourceDescriptor {
        category: HotbarActionCategory::Slash,
        boundary: HotbarSourceDispatchBoundary::SlashCommand,
        safety_modes: HOTBAR_SLASH_SAFETY,
        dispatch_path: "commands::execute or composer prefill for required arguments",
        status: "dispatchable",
    },
    HotbarSourceDescriptor {
        category: HotbarActionCategory::Mcp,
        boundary: HotbarSourceDispatchBoundary::ComposerPrefill,
        safety_modes: HOTBAR_MCP_SAFETY,
        dispatch_path: "composer prefill of the MCP tool reference; execution stays behind the \
                        existing tool approval flow",
        status: "dispatchable",
    },
    HotbarSourceDescriptor {
        category: HotbarActionCategory::Skill,
        boundary: HotbarSourceDispatchBoundary::SlashCommand,
        safety_modes: HOTBAR_SKILL_SAFETY,
        dispatch_path: "commands::execute via the $<skill> alias (local activation with a \
                        visible receipt cell)",
        status: "dispatchable",
    },
    HotbarSourceDescriptor {
        category: HotbarActionCategory::Plugin,
        boundary: HotbarSourceDispatchBoundary::Deferred,
        safety_modes: HOTBAR_DEFERRED_SAFETY,
        dispatch_path: "plugin command/tool registry until plugin approval gates are wired",
        status: "exploratory",
    },
];

#[must_use]
pub const fn hotbar_source_descriptors() -> &'static [HotbarSourceDescriptor] {
    HOTBAR_SOURCE_DESCRIPTORS
}

/// Adapter for one source of bindable hotbar actions.
pub trait HotbarActionSource {
    fn descriptor(&self) -> HotbarSourceDescriptor;
    fn register_actions(&self, registry: &mut HotbarActionRegistry);
}

/// Uniform interface for actions that can be bound to a hotbar slot.
#[cfg_attr(not(test), expect(dead_code))]
pub trait HotbarAction: Send + Sync {
    /// Stable action id used in config and dispatch.
    fn id(&self) -> &str;

    /// Complete metadata used by renderers, setup wizard recommendations, and
    /// future source adapters.
    fn metadata(&self, locale: Locale) -> HotbarActionMetadata;

    /// Compact cell label. Built-ins keep this at seven characters or less.
    fn short_label(&self) -> &str;

    /// Source category, such as `app`, `route`, `slash`, `mcp`, `skill`, or
    /// `plugin`.
    fn category(&self) -> &str;

    /// Whether the action is currently active in the supplied app state.
    fn is_active(&self, app: &App) -> bool;

    /// Dynamic unavailable reason. `None` means the action is dispatchable
    /// through its normal safety path.
    fn disabled_reason(&self, _app: &App) -> Option<String> {
        None
    }

    /// Fire the action.
    fn dispatch(&self, app: &mut App, config: &Config) -> Result<HotbarDispatch>;
}

#[must_use]
pub fn recommend_hotbar_actions(
    app: &App,
    options: HotbarRecommendationOptions,
) -> Vec<HotbarRecommendationEntry> {
    let mut entries = app
        .hotbar_actions
        .iter()
        .filter_map(|action| {
            let metadata = action.metadata(app.ui_locale);
            if !metadata.recommendation.is_recommendable() {
                return None;
            }
            if matches!(metadata.args, HotbarArgsBehavior::Required)
                && !options.include_required_args
            {
                return None;
            }
            let disabled_reason = action.disabled_reason(app);
            if disabled_reason.is_some() {
                return None;
            }
            Some(HotbarRecommendationEntry {
                metadata,
                disabled_reason,
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by(|a, b| compare_recommendation_metadata(&a.metadata, &b.metadata));

    let mut selected = Vec::new();
    let mut eligible_by_category: BTreeMap<HotbarActionCategory, usize> = BTreeMap::new();
    for entry in entries {
        if selected.len() >= options.max_total {
            break;
        }
        if !matches!(entry.metadata.recommendation, HotbarRecommendation::Default) {
            let count = eligible_by_category
                .entry(entry.metadata.category)
                .or_insert(0);
            if *count >= options.max_eligible_per_category {
                continue;
            }
            *count += 1;
        }
        selected.push(entry);
    }
    selected
}

#[must_use]
#[cfg_attr(not(test), expect(dead_code))]
pub fn recommended_hotbar_bindings(
    app: &App,
    options: HotbarRecommendationOptions,
) -> Vec<codewhale_config::HotbarBindingToml> {
    recommend_hotbar_actions(app, options)
        .into_iter()
        .take(usize::from(codewhale_config::HOTBAR_SLOT_COUNT))
        .enumerate()
        .map(|(idx, entry)| codewhale_config::HotbarBindingToml {
            slot: u8::try_from(idx + 1).expect("recommended hotbar slot fits in u8"),
            action: entry.metadata.id,
            label: Some(entry.metadata.compact_label),
        })
        .collect()
}

fn default_hotbar_position(action_id: &str) -> Option<usize> {
    codewhale_config::DEFAULT_HOTBAR_ACTIONS
        .iter()
        .position(|default_id| *default_id == action_id)
}

fn compare_recommendation_metadata(a: &HotbarActionMetadata, b: &HotbarActionMetadata) -> Ordering {
    match (
        default_hotbar_position(&a.id),
        default_hotbar_position(&b.id),
    ) {
        (Some(a_pos), Some(b_pos)) => return a_pos.cmp(&b_pos),
        (Some(_), None) => return Ordering::Less,
        (None, Some(_)) => return Ordering::Greater,
        (None, None) => {}
    }

    a.category
        .cmp(&b.category)
        .then_with(|| {
            a.display_name
                .to_ascii_lowercase()
                .cmp(&b.display_name.to_ascii_lowercase())
        })
        .then_with(|| a.id.cmp(&b.id))
}

#[derive(Default, Clone)]
pub struct HotbarActionRegistry {
    actions: BTreeMap<String, Arc<dyn HotbarAction>>,
}

impl HotbarActionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register_builtins();
        registry.register_slash_commands();
        registry
    }

    #[must_use]
    pub fn with_configured_routes(
        config: &Config,
        active_provider: Option<&ProviderIdentity>,
        active_model: &str,
        provider_models: &HashMap<String, String>,
    ) -> Self {
        let mut registry = Self::with_builtins();
        registry.register_configured_routes(config, active_provider, active_model, provider_models);
        registry
    }

    pub fn register(&mut self, action: impl HotbarAction + 'static) {
        let id = action.id().to_string();
        assert!(!id.trim().is_empty(), "hotbar action id must not be empty");
        assert!(
            self.actions.insert(id.clone(), Arc::new(action)).is_none(),
            "duplicate hotbar action id {id}"
        );
    }

    pub fn register_source(&mut self, source: &dyn HotbarActionSource) {
        let descriptor = source.descriptor();
        debug_assert!(
            hotbar_source_descriptors()
                .iter()
                .any(|registered| registered.category == descriptor.category
                    && registered.boundary == descriptor.boundary),
            "hotbar source descriptor must be registered: {descriptor:?}"
        );
        debug_assert!(!descriptor.dispatch_path.trim().is_empty());
        debug_assert!(!descriptor.status.trim().is_empty());
        debug_assert!(!descriptor.safety_modes.is_empty());
        let before = self.actions.len();
        source.register_actions(self);
        if !descriptor.registers_dispatchable_actions() {
            assert_eq!(
                self.actions.len(),
                before,
                "deferred hotbar source {:?} must not register dispatchable actions before safety gates are wired",
                descriptor.category
            );
        }
    }

    pub(crate) fn register_builtins(&mut self) {
        self.register_source(&BuiltinHotbarActionSource);
    }

    pub(crate) fn register_slash_commands(&mut self) {
        self.register_source(&SlashCommandHotbarActionSource);
    }

    pub(crate) fn register_configured_routes(
        &mut self,
        config: &Config,
        active_provider: Option<&ProviderIdentity>,
        active_model: &str,
        provider_models: &HashMap<String, String>,
    ) {
        let source = ConfiguredRouteHotbarActionSource {
            config,
            active_provider,
            active_model,
            provider_models,
        };
        self.register_source(&source);
    }

    /// Register the already-discovered skills (name, description pairs from
    /// `App::cached_skills`) as bindable hotbar actions. No filesystem I/O
    /// happens here; the hotbar only lists skills the app already knows.
    pub(crate) fn register_skills(&mut self, skills: &[(String, String)]) {
        self.register_source(&SkillHotbarActionSource { skills });
    }

    /// Atomically replace the Skill-derived action source while retaining
    /// built-ins, configured routes, slash commands, and live MCP actions.
    /// Plugin lifecycle changes call this from the same cache refresh that
    /// updates command dispatch, preventing stale revoked bindings.
    pub(crate) fn replace_skills(&mut self, skills: &[(String, String)]) {
        self.actions
            .retain(|_, action| action.category() != HotbarActionCategory::Skill.as_str());
        self.register_skills(skills);
    }

    /// Replace the MCP-tool hotbar actions with the tools in `snapshot`.
    ///
    /// Called when a live MCP discovery snapshot lands (or is refreshed) so
    /// the hotbar only ever lists tools that are already loaded; the hotbar
    /// itself never triggers server connections.
    pub fn replace_mcp_tools(&mut self, snapshot: Option<&crate::mcp::McpManagerSnapshot>) {
        self.actions
            .retain(|_, action| action.category() != HotbarActionCategory::Mcp.as_str());
        if let Some(snapshot) = snapshot {
            self.register_source(&McpToolHotbarActionSource { snapshot });
        }
    }
}

struct BuiltinHotbarActionSource;

impl HotbarActionSource for BuiltinHotbarActionSource {
    fn descriptor(&self) -> HotbarSourceDescriptor {
        HOTBAR_SOURCE_DESCRIPTORS
            .iter()
            .copied()
            .find(|descriptor| descriptor.category == HotbarActionCategory::App)
            .expect("app hotbar source descriptor exists")
    }

    fn register_actions(&self, registry: &mut HotbarActionRegistry) {
        registry.register(AppHotbarAction::new(
            "voice.toggle",
            "voice",
            "Voice input",
            "Toggle voice capture from the terminal microphone.",
            AppHotbarKind::VoiceToggle,
        ));
        registry.register(AppHotbarAction::new(
            "session.compact",
            "compact",
            "Compact session",
            "Shrink this conversation to free context.",
            AppHotbarKind::SessionCompact,
        ));
        registry.register(AppHotbarAction::new(
            "mode.plan",
            "plan",
            "Plan mode",
            "Think through a plan before acting.",
            AppHotbarKind::Mode(AppMode::Plan),
        ));
        registry.register(AppHotbarAction::new(
            "mode.agent",
            "agent",
            "Act mode",
            "Do direct work in the current session.",
            AppHotbarKind::Mode(AppMode::Agent),
        ));
        registry.register(AppHotbarAction::new(
            "mode.operate",
            "operate",
            "Operate mode",
            "Send tasks while Fleet workers run in parallel.",
            AppHotbarKind::Mode(AppMode::Operate),
        ));
        registry.register(AppHotbarAction::new(
            "reasoning.cycle",
            "reason",
            "Cycle reasoning",
            "Step through reasoning levels for the active provider.",
            AppHotbarKind::ReasoningCycle,
        ));
        registry.register(AppHotbarAction::new(
            "sidebar.toggle",
            "side",
            "Toggle workbar",
            "Show or hide the workbar.",
            AppHotbarKind::SidebarToggle,
        ));
        registry.register(AppHotbarAction::new(
            "filetree.toggle",
            "files",
            "Toggle file tree",
            "Show or hide the workspace file tree.",
            AppHotbarKind::FileTreeToggle,
        ));
        registry.register(AppHotbarAction::new(
            "palette.open",
            "palette",
            "Command palette",
            "Open the command palette.",
            AppHotbarKind::PaletteOpen,
        ));
        registry.register(AppHotbarAction::new(
            "trust.toggle",
            "trust",
            "Toggle trust",
            "Turn workspace trust on or off.",
            AppHotbarKind::TrustToggle,
        ));
    }
}

struct SlashCommandHotbarActionSource;

impl HotbarActionSource for SlashCommandHotbarActionSource {
    fn descriptor(&self) -> HotbarSourceDescriptor {
        HOTBAR_SOURCE_DESCRIPTORS
            .iter()
            .copied()
            .find(|descriptor| descriptor.category == HotbarActionCategory::Slash)
            .expect("slash hotbar source descriptor exists")
    }

    fn register_actions(&self, registry: &mut HotbarActionRegistry) {
        // Every command registers, including unlisted ones. The hotbar is a
        // binding substrate, not a discovery surface: `codewhale-lane`'s
        // control-plane descriptors resolve their `slash.<verb>` action id
        // through this registry, so dropping an unlisted command here breaks
        // a real contract (`control_plane_commands_are_bound_and_bare_dispatch_is_read_only`).
        // Unlisted governs what is *advertised* — the slash menu, `/help`,
        // and the command palette.
        for info in commands::command_infos() {
            registry.register(SlashHotbarAction::new(info));
        }
    }
}

/// Adapter exposing already-discovered skills as hotbar actions (#2069).
///
/// Follows the slash-command source pattern: the source only lists entries an
/// existing registry already knows about (`App::cached_skills`), and dispatch
/// reuses the existing `$<skill>` alias through `commands::execute`, which
/// activates the skill locally and posts a visible receipt cell.
struct SkillHotbarActionSource<'a> {
    skills: &'a [(String, String)],
}

impl HotbarActionSource for SkillHotbarActionSource<'_> {
    fn descriptor(&self) -> HotbarSourceDescriptor {
        HOTBAR_SOURCE_DESCRIPTORS
            .iter()
            .copied()
            .find(|descriptor| descriptor.category == HotbarActionCategory::Skill)
            .expect("skill hotbar source descriptor exists")
    }

    fn register_actions(&self, registry: &mut HotbarActionRegistry) {
        let mut seen = BTreeSet::new();
        for (name, description) in self.skills {
            let name = name.trim();
            // Guard against duplicate names across skill roots: the registry
            // asserts unique action ids, and the first discovery wins (the
            // same shadowing order the skill registry itself uses).
            if name.is_empty() || !seen.insert(name.to_string()) {
                continue;
            }
            registry.register(SkillHotbarAction::new(name, description));
        }
    }
}

/// Adapter exposing already-discovered MCP tools as hotbar actions (#2068).
///
/// Deferred-source safety: the source only lists tools from an existing
/// discovery snapshot (enabled servers), and dispatch never executes a tool —
/// it prefills the composer with the tool's model-visible name so the actual
/// call still goes through the agent and the tool approval flow.
struct McpToolHotbarActionSource<'a> {
    snapshot: &'a crate::mcp::McpManagerSnapshot,
}

impl HotbarActionSource for McpToolHotbarActionSource<'_> {
    fn descriptor(&self) -> HotbarSourceDescriptor {
        HOTBAR_SOURCE_DESCRIPTORS
            .iter()
            .copied()
            .find(|descriptor| descriptor.category == HotbarActionCategory::Mcp)
            .expect("mcp hotbar source descriptor exists")
    }

    fn register_actions(&self, registry: &mut HotbarActionRegistry) {
        let mut seen = BTreeSet::new();
        for server in &self.snapshot.servers {
            if !server.enabled {
                continue;
            }
            for tool in &server.tools {
                if tool.model_name.trim().is_empty() {
                    continue;
                }
                let action = McpToolHotbarAction::new(&server.name, tool);
                if !seen.insert(action.id.clone()) {
                    continue;
                }
                registry.register(action);
            }
        }
    }
}

struct ConfiguredRouteHotbarActionSource<'a> {
    config: &'a Config,
    active_provider: Option<&'a ProviderIdentity>,
    active_model: &'a str,
    provider_models: &'a HashMap<String, String>,
}

impl HotbarActionSource for ConfiguredRouteHotbarActionSource<'_> {
    fn descriptor(&self) -> HotbarSourceDescriptor {
        HOTBAR_SOURCE_DESCRIPTORS
            .iter()
            .copied()
            .find(|descriptor| descriptor.category == HotbarActionCategory::Route)
            .expect("route hotbar source descriptor exists")
    }

    fn register_actions(&self, registry: &mut HotbarActionRegistry) {
        let Some(active) = self
            .active_provider
            .filter(|identity| self.config.verify_provider_identity(identity).is_ok())
        else {
            return;
        };
        let mut identities = self.config.provider_identities();
        identities.sort_by(|left, right| {
            let left_label = left
                .compatibility()
                .map_or(left.key.as_str(), |row| row.label);
            let right_label = right
                .compatibility()
                .map_or(right.key.as_str(), |row| row.label);
            left_label
                .cmp(right_label)
                .then_with(|| left.key.as_str().cmp(right.key.as_str()))
        });
        for identity in identities {
            if identity.provider == ProviderKind::Antigravity {
                continue;
            }
            if !crate::config::provider_is_configured_for_active(self.config, &identity, active) {
                continue;
            }
            for model in configured_route_models_for_provider(
                self.config,
                &identity,
                active,
                self.active_model,
                self.provider_models,
            ) {
                registry.register(RouteHotbarAction::new(identity.clone(), model));
            }
        }
    }
}

impl HotbarActionRegistry {
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<dyn HotbarAction>> {
        self.actions.get(id).cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    #[expect(dead_code)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn HotbarAction> {
        self.actions.values().map(Arc::as_ref)
    }

    #[cfg_attr(not(test), expect(dead_code))]
    #[must_use]
    pub fn metadata(&self, locale: Locale) -> Vec<HotbarActionMetadata> {
        self.iter().map(|action| action.metadata(locale)).collect()
    }

    #[cfg_attr(not(test), expect(dead_code))]
    #[must_use]
    pub fn metadata_validation_errors(&self, locale: Locale) -> Vec<String> {
        let mut errors = Vec::new();
        for action in self.iter() {
            let metadata = action.metadata(locale);
            if metadata.id != action.id() {
                errors.push(format!(
                    "{} metadata id {:?} does not match action id",
                    action.id(),
                    metadata.id
                ));
            }
            if metadata.compact_label != action.short_label() {
                errors.push(format!(
                    "{} metadata compact_label {:?} does not match short_label {:?}",
                    action.id(),
                    metadata.compact_label,
                    action.short_label()
                ));
            }
            if metadata.category.as_str() != action.category() {
                errors.push(format!(
                    "{} metadata category {:?} does not match category {:?}",
                    action.id(),
                    metadata.category.as_str(),
                    action.category()
                ));
            }
            errors.extend(metadata.validation_errors());
        }
        errors
    }
}

fn dispatch_command_result(app: &mut App, result: CommandResult) -> HotbarDispatch {
    app.status_message = result.message;
    result
        .action
        .map_or(HotbarDispatch::Handled, HotbarDispatch::AppAction)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppHotbarKind {
    VoiceToggle,
    SessionCompact,
    Mode(AppMode),
    ReasoningCycle,
    SidebarToggle,
    FileTreeToggle,
    PaletteOpen,
    TrustToggle,
}

struct AppHotbarAction {
    id: &'static str,
    short_label: &'static str,
    display_name: &'static str,
    description: &'static str,
    kind: AppHotbarKind,
}

impl AppHotbarAction {
    const fn new(
        id: &'static str,
        short_label: &'static str,
        display_name: &'static str,
        description: &'static str,
        kind: AppHotbarKind,
    ) -> Self {
        Self {
            id,
            short_label,
            display_name,
            description,
            kind,
        }
    }

    fn safety(&self) -> HotbarSafetyClass {
        match self.kind {
            AppHotbarKind::VoiceToggle => HotbarSafetyClass::ExternalInput,
            AppHotbarKind::TrustToggle => HotbarSafetyClass::LocalState,
            AppHotbarKind::SessionCompact | AppHotbarKind::ReasoningCycle => {
                HotbarSafetyClass::LocalState
            }
            AppHotbarKind::Mode(_)
            | AppHotbarKind::SidebarToggle
            | AppHotbarKind::FileTreeToggle
            | AppHotbarKind::PaletteOpen => HotbarSafetyClass::LocalUi,
        }
    }

    fn recommendation(&self) -> HotbarRecommendation {
        if codewhale_config::DEFAULT_HOTBAR_ACTIONS.contains(&self.id) {
            HotbarRecommendation::Default
        } else {
            HotbarRecommendation::Eligible
        }
    }

    fn localized_display_name(&self, locale: Locale) -> String {
        let Some(id) = self.display_name_id() else {
            return self.display_name.to_string();
        };
        tr(locale, id).into_owned()
    }

    fn localized_description(&self, locale: Locale) -> String {
        let Some(id) = self.description_id() else {
            return self.description.to_string();
        };
        tr(locale, id).into_owned()
    }

    fn display_name_id(&self) -> Option<MessageId> {
        Some(match self.kind {
            AppHotbarKind::VoiceToggle => MessageId::HotbarActionVoiceToggleName,
            AppHotbarKind::SessionCompact => MessageId::HotbarActionSessionCompactName,
            AppHotbarKind::Mode(AppMode::Plan) => MessageId::HotbarActionModePlanName,
            AppHotbarKind::Mode(AppMode::Agent) => MessageId::HotbarActionModeAgentName,
            AppHotbarKind::Mode(AppMode::Operate) => MessageId::HotbarActionModeOperateName,
            AppHotbarKind::ReasoningCycle => MessageId::HotbarActionReasoningCycleName,
            AppHotbarKind::SidebarToggle => MessageId::HotbarActionSidebarToggleName,
            AppHotbarKind::FileTreeToggle => MessageId::HotbarActionFileTreeToggleName,
            AppHotbarKind::PaletteOpen => MessageId::HotbarActionPaletteOpenName,
            AppHotbarKind::TrustToggle => MessageId::HotbarActionTrustToggleName,
        })
    }

    fn description_id(&self) -> Option<MessageId> {
        Some(match self.kind {
            AppHotbarKind::VoiceToggle => MessageId::HotbarActionVoiceToggleDescription,
            AppHotbarKind::SessionCompact => MessageId::HotbarActionSessionCompactDescription,
            AppHotbarKind::Mode(AppMode::Plan) => MessageId::HotbarActionModePlanDescription,
            AppHotbarKind::Mode(AppMode::Agent) => MessageId::HotbarActionModeAgentDescription,
            AppHotbarKind::Mode(AppMode::Operate) => MessageId::HotbarActionModeOperateDescription,
            AppHotbarKind::ReasoningCycle => MessageId::HotbarActionReasoningCycleDescription,
            AppHotbarKind::SidebarToggle => MessageId::HotbarActionSidebarToggleDescription,
            AppHotbarKind::FileTreeToggle => MessageId::HotbarActionFileTreeToggleDescription,
            AppHotbarKind::PaletteOpen => MessageId::HotbarActionPaletteOpenDescription,
            AppHotbarKind::TrustToggle => MessageId::HotbarActionTrustToggleDescription,
        })
    }
}

impl HotbarAction for AppHotbarAction {
    fn id(&self) -> &str {
        self.id
    }

    fn metadata(&self, locale: Locale) -> HotbarActionMetadata {
        HotbarActionMetadata {
            id: self.id.to_string(),
            source_id: "builtin".to_string(),
            display_name: self.localized_display_name(locale),
            compact_label: self.short_label.to_string(),
            description: self.localized_description(locale),
            category: HotbarActionCategory::App,
            args: HotbarArgsBehavior::None,
            safety: self.safety(),
            recommendation: self.recommendation(),
        }
    }

    fn short_label(&self) -> &str {
        self.short_label
    }

    fn category(&self) -> &str {
        "app"
    }

    fn is_active(&self, app: &App) -> bool {
        match self.kind {
            AppHotbarKind::VoiceToggle => app.voice_enabled,
            AppHotbarKind::SessionCompact => app.is_compacting || app.manual_compaction_queued,
            AppHotbarKind::Mode(mode) => app.mode == mode,
            AppHotbarKind::ReasoningCycle => {
                app.reasoning_effort != crate::reasoning_preference::ReasoningEffort::Off
            }
            AppHotbarKind::SidebarToggle => {
                app.work_surface.placement != crate::tui::work_surface::WorkSurfacePlacement::Off
            }
            AppHotbarKind::FileTreeToggle => app.file_tree.is_some(),
            AppHotbarKind::PaletteOpen => false,
            AppHotbarKind::TrustToggle => app.trust_mode,
        }
    }

    fn dispatch(&self, app: &mut App, _config: &Config) -> Result<HotbarDispatch> {
        match self.kind {
            AppHotbarKind::VoiceToggle => {
                let result = crate::commands::voice::voice(app);
                Ok(dispatch_command_result(app, result))
            }
            AppHotbarKind::SessionCompact => {
                Ok(HotbarDispatch::AppAction(AppAction::CompactContext {
                    focus: None,
                }))
            }
            AppHotbarKind::Mode(mode) => {
                // User-facing selection: persists the startup default too.
                let outcome = app.select_mode(mode);
                // Only a live change needs an `AppAction`; a persisted-same
                // selection still gets its own receipt so the row does not look
                // inert when it actually wrote the startup default.
                app.report_mode_selection(mode, outcome);
                if outcome.changed_live_state() {
                    Ok(HotbarDispatch::AppAction(AppAction::ModeChanged(mode)))
                } else {
                    Ok(HotbarDispatch::Handled)
                }
            }
            AppHotbarKind::ReasoningCycle => {
                if app.cycle_effort().changed_live_state() {
                    Ok(HotbarDispatch::AppAction(AppAction::UpdateCompaction(
                        app.compaction_config(),
                    )))
                } else {
                    Ok(HotbarDispatch::Handled)
                }
            }
            AppHotbarKind::SidebarToggle => {
                if app.work_surface.placement == crate::tui::work_surface::WorkSurfacePlacement::Off
                {
                    app.work_surface.placement =
                        crate::tui::work_surface::WorkSurfacePlacement::Bottom;
                    app.status_message = Some("Workbar: bottom placement".to_string());
                } else {
                    app.work_surface.placement =
                        crate::tui::work_surface::WorkSurfacePlacement::Off;
                    app.status_message = Some("Workbar is off".to_string());
                }
                app.needs_redraw = true;
                Ok(HotbarDispatch::Handled)
            }
            AppHotbarKind::FileTreeToggle => {
                if app.file_tree.is_some() {
                    app.file_tree = None;
                    app.status_message = Some("File tree closed".to_string());
                } else {
                    app.file_tree = Some(crate::tui::file_tree::FileTreeState::new(&app.workspace));
                    app.status_message =
                        Some("File tree: ↑/↓ navigate  Enter select  Esc close".to_string());
                }
                app.needs_redraw = true;
                Ok(HotbarDispatch::Handled)
            }
            AppHotbarKind::PaletteOpen => {
                app.view_stack.push(CommandPaletteView::new_for_locale(
                    app.ui_locale,
                    build_command_palette_entries(
                        app.ui_locale,
                        &app.skills_dir,
                        app.skills_discovery_mode,
                        &app.workspace,
                        &app.mcp_config_path,
                        app.mcp_snapshot.as_ref(),
                        app.extension_plugin_view().as_ref(),
                    ),
                ));
                Ok(HotbarDispatch::Handled)
            }
            AppHotbarKind::TrustToggle => {
                app.trust_mode = !app.trust_mode;
                app.status_message = Some(if app.trust_mode {
                    "Workspace trust mode enabled.".to_string()
                } else {
                    "Workspace trust mode disabled.".to_string()
                });
                Ok(HotbarDispatch::Handled)
            }
        }
    }
}

struct SlashHotbarAction {
    info: &'static CommandInfo,
    id: String,
    short_label: String,
}

impl SlashHotbarAction {
    fn new(info: &'static CommandInfo) -> Self {
        let short_label = match info.name {
            "workflow" => "wf".to_string(),
            other => other.chars().take(7).collect(),
        };
        Self {
            info,
            id: format!("slash.{}", info.name),
            short_label,
        }
    }

    fn prefill_composer(&self, app: &mut App) {
        app.clear_input_recoverable();
        app.input = format!("/{} ", self.info.name);
        app.cursor_position = app.input.chars().count();
        app.slash_menu_hidden = false;
        app.needs_redraw = true;
        app.status_message = Some(format!(
            "Command needs arguments; complete {}",
            app.input.trim_end()
        ));
    }
}

impl HotbarAction for SlashHotbarAction {
    fn id(&self) -> &str {
        &self.id
    }

    fn metadata(&self, locale: Locale) -> HotbarActionMetadata {
        let recommendation = if codewhale_config::DEFAULT_HOTBAR_ACTIONS.contains(&self.id.as_str())
        {
            HotbarRecommendation::Default
        } else {
            match self.info.discovery() {
                crate::commands::traits::CommandDiscovery::Primary => {
                    HotbarRecommendation::Eligible
                }
                crate::commands::traits::CommandDiscovery::Advanced
                | crate::commands::traits::CommandDiscovery::Compatibility => {
                    HotbarRecommendation::Advanced
                }
            }
        };
        HotbarActionMetadata {
            id: self.id.clone(),
            source_id: format!("command:{}", self.info.name),
            display_name: format!("/{}", self.info.name),
            compact_label: self.short_label.clone(),
            description: self.info.description_for(locale).to_string(),
            category: HotbarActionCategory::Slash,
            args: HotbarArgsBehavior::for_command(self.info),
            safety: HotbarSafetyClass::ExistingCommand,
            recommendation,
        }
    }

    fn short_label(&self) -> &str {
        &self.short_label
    }

    fn category(&self) -> &str {
        "slash"
    }

    fn is_active(&self, _app: &App) -> bool {
        false
    }

    fn dispatch(&self, app: &mut App, config: &Config) -> Result<HotbarDispatch> {
        if self.info.requires_required_argument() {
            self.prefill_composer(app);
            return Ok(HotbarDispatch::Handled);
        }

        let input = format!("/{}", self.info.name);
        let result = commands::execute_with_config(&input, app, config);
        Ok(dispatch_command_result(app, result))
    }
}

struct RouteHotbarAction {
    identity: ProviderIdentity,
    model: String,
    id: String,
    short_label: String,
}

impl RouteHotbarAction {
    fn new(identity: ProviderIdentity, model: String) -> Self {
        let trimmed_model = model.trim().to_string();
        Self {
            id: route_action_id(&identity, &trimmed_model),
            short_label: crate::tui::ui_text::truncate_line_to_width(
                identity.key.as_str(),
                HOTBAR_COMPACT_LABEL_MAX_WIDTH,
            ),
            model: trimmed_model,
            identity,
        }
    }
}

impl HotbarAction for RouteHotbarAction {
    fn id(&self) -> &str {
        &self.id
    }

    fn metadata(&self, _locale: Locale) -> HotbarActionMetadata {
        HotbarActionMetadata {
            id: self.id.clone(),
            source_id: format!("route:{}", self.identity.key.as_str()),
            display_name: format!(
                "{} · {}",
                self.identity
                    .compatibility()
                    .map_or(self.identity.key.as_str(), |row| row.label),
                self.model
            ),
            compact_label: self.short_label.clone(),
            description: format!(
                "Switch to {} on {} through the existing /model route path.",
                self.model,
                self.identity
                    .compatibility()
                    .map_or(self.identity.key.as_str(), |row| row.label)
            ),
            category: HotbarActionCategory::Route,
            args: HotbarArgsBehavior::None,
            safety: HotbarSafetyClass::ConfigChange,
            recommendation: HotbarRecommendation::Eligible,
        }
    }

    fn short_label(&self) -> &str {
        &self.short_label
    }

    fn category(&self) -> &str {
        "route"
    }

    fn is_active(&self, app: &App) -> bool {
        !app.auto_model
            && app.provider_identity.as_ref() == Some(&self.identity)
            && if self.identity.provider == ProviderKind::Custom {
                app.model.trim() == self.model.trim()
            } else {
                app.model.trim().eq_ignore_ascii_case(self.model.trim())
            }
    }

    fn dispatch(&self, _app: &mut App, _config: &Config) -> Result<HotbarDispatch> {
        Ok(HotbarDispatch::AppAction(AppAction::SwitchModelRoute {
            identity: self.identity.clone(),
            model: self.model.clone(),
        }))
    }
}

struct SkillHotbarAction {
    name: String,
    id: String,
    short_label: String,
    description: String,
}

impl SkillHotbarAction {
    fn new(name: &str, description: &str) -> Self {
        let description = description.trim();
        Self {
            name: name.to_string(),
            id: format!("skill.{name}"),
            short_label: crate::tui::ui_text::truncate_line_to_width(
                name,
                HOTBAR_COMPACT_LABEL_MAX_WIDTH,
            ),
            description: if description.is_empty() {
                format!("Activate the {name} skill for the next message.")
            } else {
                description.to_string()
            },
        }
    }
}

impl HotbarAction for SkillHotbarAction {
    fn id(&self) -> &str {
        &self.id
    }

    fn metadata(&self, _locale: Locale) -> HotbarActionMetadata {
        HotbarActionMetadata {
            id: self.id.clone(),
            source_id: format!("skill:{}", self.name),
            display_name: format!("${}", self.name),
            compact_label: self.short_label.clone(),
            description: self.description.clone(),
            category: HotbarActionCategory::Skill,
            args: HotbarArgsBehavior::None,
            safety: HotbarSafetyClass::ExistingCommand,
            recommendation: HotbarRecommendation::Eligible,
        }
    }

    fn short_label(&self) -> &str {
        &self.short_label
    }

    fn category(&self) -> &str {
        "skill"
    }

    fn is_active(&self, app: &App) -> bool {
        // `activate_skill` stores the full instruction block; the heading line
        // inside it is the stable marker for which skill is armed.
        app.active_skill
            .as_deref()
            .is_some_and(|instruction| instruction.contains(&format!("# Skill: {}\n", self.name)))
    }

    fn dispatch(&self, app: &mut App, config: &Config) -> Result<HotbarDispatch> {
        // Same path as typing `$<name>`: activates the skill for the next
        // message (local state plus a visible receipt cell); nothing is sent
        // to the model until the user submits a message.
        let input = format!("${}", self.name);
        let result = commands::execute_with_config(&input, app, config);
        Ok(dispatch_command_result(app, result))
    }
}

struct McpToolHotbarAction {
    server: String,
    tool_name: String,
    model_name: String,
    description: Option<String>,
    id: String,
    short_label: String,
}

impl McpToolHotbarAction {
    fn new(server: &str, tool: &crate::mcp::McpDiscoveredItem) -> Self {
        Self {
            server: server.to_string(),
            tool_name: tool.name.clone(),
            model_name: tool.model_name.trim().to_string(),
            description: tool.description.clone(),
            id: format!("mcp.{server}.{}", tool.name),
            short_label: crate::tui::ui_text::truncate_line_to_width(
                &tool.name,
                HOTBAR_COMPACT_LABEL_MAX_WIDTH,
            ),
        }
    }

    fn prefill_composer(&self, app: &mut App) {
        app.clear_input_recoverable();
        app.input = format!("{} ", self.model_name);
        app.cursor_position = app.input.chars().count();
        app.needs_redraw = true;
        app.status_message = Some(format!(
            "MCP tool needs a request; complete {}",
            app.input.trim_end()
        ));
    }
}

impl HotbarAction for McpToolHotbarAction {
    fn id(&self) -> &str {
        &self.id
    }

    fn metadata(&self, _locale: Locale) -> HotbarActionMetadata {
        let description = match self.description.as_deref().map(str::trim) {
            Some(desc) if !desc.is_empty() => {
                format!("Prefill the composer with {} — {desc}", self.model_name)
            }
            _ => format!(
                "Prefill the composer with {}; the call still runs through tool approval.",
                self.model_name
            ),
        };
        HotbarActionMetadata {
            id: self.id.clone(),
            source_id: format!("mcp:{}", self.server),
            display_name: format!("mcp:{}:{}", self.server, self.tool_name),
            compact_label: self.short_label.clone(),
            description,
            category: HotbarActionCategory::Mcp,
            args: HotbarArgsBehavior::Required,
            safety: HotbarSafetyClass::RequiresApproval,
            recommendation: HotbarRecommendation::Advanced,
        }
    }

    fn short_label(&self) -> &str {
        &self.short_label
    }

    fn category(&self) -> &str {
        "mcp"
    }

    fn is_active(&self, _app: &App) -> bool {
        false
    }

    fn dispatch(&self, app: &mut App, _config: &Config) -> Result<HotbarDispatch> {
        // Never execute the tool from the hotbar: prefill the composer with
        // the model-visible tool name (same text the command palette's
        // "> use" entry inserts) and let the user describe the call. The
        // eventual invocation stays behind the normal tool approval flow.
        self.prefill_composer(app);
        Ok(HotbarDispatch::Handled)
    }
}

fn configured_route_models_for_provider(
    config: &Config,
    identity: &ProviderIdentity,
    active: &ProviderIdentity,
    active_model: &str,
    provider_models: &HashMap<String, String>,
) -> Vec<String> {
    let mut models = Vec::new();
    if identity == active {
        push_route_model(&mut models, active_model);
    }
    if let Some(model) = provider_models.get(identity.key.as_str()) {
        push_route_model(&mut models, model);
    }
    if let Some(model) = config
        .provider_config_for(identity)
        .and_then(|entry| entry.model.as_deref())
    {
        push_route_model(&mut models, model);
    }
    for model in models_for_provider(config, identity)
        .into_iter()
        .filter(|model| !model.trim().eq_ignore_ascii_case("auto"))
        .take(1)
    {
        push_route_model(&mut models, &model);
    }
    models
}

fn push_route_model(models: &mut Vec<String>, model: &str) {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        return;
    }
    if models.iter().any(|existing| existing == trimmed) {
        return;
    }
    models.push(trimmed.to_string());
}

fn route_action_id(identity: &ProviderIdentity, model: &str) -> String {
    format!("route.{}.{}", identity.key, model.trim())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::PathBuf;

    use crate::config::{Config, ProviderKind};
    use crate::reasoning_preference::ReasoningEffort;
    use crate::tui::app::TuiOptions;
    use crate::tui::views::ModalKind;

    use super::*;

    fn test_app_with_paths_and_config(
        workspace: PathBuf,
        skills_dir: PathBuf,
        config: &Config,
    ) -> App {
        let options = TuiOptions {
            skills_dir,
            start_in_agent_mode: true,
            ..crate::test_support::test_tui_options(workspace)
        };
        let mut app = App::new(options, config);
        app.ui_locale = codewhale_localization::Locale::En;
        app
    }

    fn test_app_with_paths(workspace: PathBuf, skills_dir: PathBuf) -> App {
        test_app_with_paths_and_config(workspace, skills_dir, &Config::default())
    }

    fn test_app() -> App {
        test_app_with_paths(PathBuf::from("."), PathBuf::from("."))
    }

    fn test_mcp_snapshot() -> crate::mcp::McpManagerSnapshot {
        use crate::mcp::{McpDiscoveredItem, McpManagerSnapshot, McpServerSnapshot};
        let server = |name: &str, enabled: bool, tools: Vec<McpDiscoveredItem>| McpServerSnapshot {
            name: name.to_string(),
            enabled,
            required: false,
            transport: "stdio".to_string(),
            command_or_url: format!("{name}-server"),
            connect_timeout: 5,
            execute_timeout: 5,
            read_timeout: 5,
            connected: enabled,
            error: None,
            auth_required: false,
            capability_metadata: if enabled {
                crate::mcp::McpServerCapabilityMetadata::LegacyFallback
            } else {
                crate::mcp::McpServerCapabilityMetadata::NotObserved
            },
            tools,
            resources: Vec::new(),
            prompts: Vec::new(),
        };
        McpManagerSnapshot {
            config_path: PathBuf::from("mcp.json"),
            config_exists: true,
            reload_required: false,
            servers: vec![
                server(
                    "search",
                    true,
                    vec![
                        McpDiscoveredItem {
                            name: "web_search".to_string(),
                            model_name: "mcp_search_web_search".to_string(),
                            description: Some("Search the web".to_string()),
                        },
                        McpDiscoveredItem {
                            name: "broken".to_string(),
                            model_name: "  ".to_string(),
                            description: None,
                        },
                    ],
                ),
                server(
                    "offline",
                    false,
                    vec![McpDiscoveredItem {
                        name: "other_tool".to_string(),
                        model_name: "mcp_offline_other_tool".to_string(),
                        description: None,
                    }],
                ),
            ],
        }
    }

    struct TestHotbarAction {
        id: &'static str,
    }

    impl HotbarAction for TestHotbarAction {
        fn id(&self) -> &str {
            self.id
        }

        fn metadata(&self, _locale: Locale) -> HotbarActionMetadata {
            HotbarActionMetadata {
                id: self.id.to_string(),
                source_id: "test".to_string(),
                display_name: "Test action".to_string(),
                compact_label: "test".to_string(),
                description: "Test action descriptor".to_string(),
                category: HotbarActionCategory::App,
                args: HotbarArgsBehavior::None,
                safety: HotbarSafetyClass::LocalUi,
                recommendation: HotbarRecommendation::Eligible,
            }
        }

        fn short_label(&self) -> &str {
            "test"
        }

        fn category(&self) -> &str {
            "app"
        }

        fn is_active(&self, _app: &App) -> bool {
            false
        }

        fn dispatch(&self, _app: &mut App, _config: &Config) -> Result<HotbarDispatch> {
            Ok(HotbarDispatch::Handled)
        }
    }

    struct DeferredTestHotbarSource;

    impl HotbarActionSource for DeferredTestHotbarSource {
        fn descriptor(&self) -> HotbarSourceDescriptor {
            HOTBAR_SOURCE_DESCRIPTORS
                .iter()
                .copied()
                .find(|descriptor| descriptor.category == HotbarActionCategory::Plugin)
                .expect("plugin descriptor exists")
        }

        fn register_actions(&self, registry: &mut HotbarActionRegistry) {
            registry.register(TestHotbarAction {
                id: "plugin.deferred-test",
            });
        }
    }

    #[test]
    #[should_panic(expected = "duplicate hotbar action id duplicate.action")]
    fn registry_rejects_duplicate_action_ids() {
        let mut registry = HotbarActionRegistry::new();
        registry.register(TestHotbarAction {
            id: "duplicate.action",
        });
        registry.register(TestHotbarAction {
            id: "duplicate.action",
        });
    }

    #[test]
    fn registry_metadata_contract_covers_registered_actions() {
        let registry = HotbarActionRegistry::with_builtins();
        let errors = registry.metadata_validation_errors(Locale::En);
        assert!(errors.is_empty(), "metadata validation failed: {errors:?}");

        let metadata = registry.metadata(Locale::En);
        assert_eq!(metadata.len(), registry.len());

        let ids = metadata
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>();
        let mut sorted_ids = ids.clone();
        sorted_ids.sort_unstable();
        assert_eq!(
            ids, sorted_ids,
            "registry metadata should have stable id order"
        );
        assert_eq!(
            ids.iter().copied().collect::<BTreeSet<_>>().len(),
            ids.len(),
            "metadata ids must be unique"
        );

        for entry in metadata {
            assert_eq!(
                HotbarActionCategory::parse(entry.category.as_str()),
                Some(entry.category)
            );
            let entry_errors = entry.validation_errors();
            assert!(
                entry_errors.is_empty(),
                "metadata entry failed validation: {entry_errors:?}"
            );
            assert!(
                unicode_width::UnicodeWidthStr::width(entry.compact_label.as_str())
                    <= HOTBAR_COMPACT_LABEL_MAX_WIDTH,
                "compact label should be validated: {entry:?}"
            );
        }
    }

    #[test]
    fn source_descriptors_cover_dispatch_boundaries() {
        let descriptors = hotbar_source_descriptors();
        let categories = descriptors
            .iter()
            .map(|descriptor| descriptor.category)
            .collect::<BTreeSet<_>>();

        assert_eq!(
            categories,
            BTreeSet::from([
                HotbarActionCategory::App,
                HotbarActionCategory::Route,
                HotbarActionCategory::Slash,
                HotbarActionCategory::Mcp,
                HotbarActionCategory::Skill,
                HotbarActionCategory::Plugin,
            ])
        );
        assert_eq!(
            descriptors
                .iter()
                .find(|descriptor| descriptor.category == HotbarActionCategory::App)
                .map(|descriptor| (
                    descriptor.boundary,
                    descriptor.safety_modes,
                    descriptor.registers_dispatchable_actions()
                )),
            Some((
                HotbarSourceDispatchBoundary::DirectApp,
                HOTBAR_DIRECT_APP_SAFETY,
                true
            ))
        );
        assert_eq!(
            descriptors
                .iter()
                .find(|descriptor| descriptor.category == HotbarActionCategory::Route)
                .map(|descriptor| (
                    descriptor.boundary,
                    descriptor.safety_modes,
                    descriptor.registers_dispatchable_actions()
                )),
            Some((
                HotbarSourceDispatchBoundary::ModelRoute,
                HOTBAR_ROUTE_SAFETY,
                true
            ))
        );
        assert_eq!(
            descriptors
                .iter()
                .find(|descriptor| descriptor.category == HotbarActionCategory::Slash)
                .map(|descriptor| (
                    descriptor.boundary,
                    descriptor.safety_modes,
                    descriptor.registers_dispatchable_actions()
                )),
            Some((
                HotbarSourceDispatchBoundary::SlashCommand,
                HOTBAR_SLASH_SAFETY,
                true
            ))
        );
        assert_eq!(
            descriptors
                .iter()
                .find(|descriptor| descriptor.category == HotbarActionCategory::Mcp)
                .map(|descriptor| (
                    descriptor.boundary,
                    descriptor.safety_modes,
                    descriptor.registers_dispatchable_actions()
                )),
            Some((
                HotbarSourceDispatchBoundary::ComposerPrefill,
                HOTBAR_MCP_SAFETY,
                true
            ))
        );
        assert_eq!(
            descriptors
                .iter()
                .find(|descriptor| descriptor.category == HotbarActionCategory::Skill)
                .map(|descriptor| (
                    descriptor.boundary,
                    descriptor.safety_modes,
                    descriptor.registers_dispatchable_actions()
                )),
            Some((
                HotbarSourceDispatchBoundary::SlashCommand,
                HOTBAR_SKILL_SAFETY,
                true
            ))
        );
        let plugin = descriptors
            .iter()
            .find(|descriptor| descriptor.category == HotbarActionCategory::Plugin)
            .expect("missing descriptor for Plugin");
        assert_eq!(plugin.boundary, HotbarSourceDispatchBoundary::Deferred);
        assert_eq!(plugin.safety_modes, HOTBAR_DEFERRED_SAFETY);
        assert_eq!(plugin.status, "exploratory");
        assert!(
            !plugin.registers_dispatchable_actions(),
            "deferred Plugin source must not be dispatchable"
        );
    }

    #[test]
    #[should_panic(
        expected = "deferred hotbar source Plugin must not register dispatchable actions"
    )]
    fn deferred_sources_cannot_register_dispatchable_actions() {
        let mut registry = HotbarActionRegistry::new();
        registry.register_source(&DeferredTestHotbarSource);
    }

    #[test]
    fn source_adapters_register_previous_default_registry_surface() {
        let mut registry = HotbarActionRegistry::new();
        registry.register_source(&BuiltinHotbarActionSource);
        registry.register_source(&SlashCommandHotbarActionSource);

        let adapter_ids = registry
            .iter()
            .map(|action| action.id().to_string())
            .collect::<Vec<_>>();
        let default_ids = HotbarActionRegistry::with_builtins()
            .iter()
            .map(|action| action.id().to_string())
            .collect::<Vec<_>>();

        assert_eq!(adapter_ids, default_ids);
        assert_eq!(
            BuiltinHotbarActionSource.descriptor().category,
            HotbarActionCategory::App
        );
        assert_eq!(
            SlashCommandHotbarActionSource.descriptor().category,
            HotbarActionCategory::Slash
        );
    }

    #[test]
    fn slash_source_matches_command_palette_command_entries() {
        // Hermetic: the palette builder also reads `~/.claude/skills`, so a
        // host with imported Claude skills would leak entries into one side
        // and fail the comparison. Isolate HOME the way the other
        // environment-reading tests in this file do.
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", tmp.path());
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join(".codewhale"));
        let palette_slash_ids = build_command_palette_entries(
            Locale::En,
            tmp.path(),
            crate::skills::SkillDiscoveryMode::CodeWhaleOnly,
            tmp.path(),
            &tmp.path().join("mcp.json"),
            None,
            &crate::plugins::PluginRegistry::empty(tmp.path()),
        )
        .into_iter()
        .filter(|entry| entry.section() == crate::tui::command_palette::PaletteSection::Command)
        .filter_map(|entry| {
            entry
                .label
                .strip_prefix('/')
                .map(|name| format!("slash.{name}"))
        })
        .collect::<BTreeSet<_>>();

        let mut registry = HotbarActionRegistry::new();
        registry.register_source(&SlashCommandHotbarActionSource);
        let hotbar_slash_ids = registry
            .iter()
            .map(|action| action.id().to_string())
            .collect::<BTreeSet<_>>();

        // The hotbar is a binding substrate and registers every command; the
        // palette is a browsing surface and omits the unlisted ones. So the
        // palette is a subset, and the difference is exactly the unlisted set.
        let unlisted_ids = commands::command_infos()
            .iter()
            .filter(|info| info.is_unlisted())
            .map(|info| format!("slash.{}", info.name))
            .collect::<BTreeSet<_>>();
        assert!(
            palette_slash_ids.is_subset(&hotbar_slash_ids),
            "the palette must not offer a command the hotbar cannot bind"
        );
        assert_eq!(
            hotbar_slash_ids
                .difference(&palette_slash_ids)
                .cloned()
                .collect::<BTreeSet<_>>(),
            unlisted_ids,
            "the only commands the hotbar has and the palette hides are the unlisted ones"
        );
    }

    #[test]
    fn default_hotbar_actions_have_registered_default_metadata() {
        let registry = HotbarActionRegistry::with_builtins();

        for id in codewhale_config::DEFAULT_HOTBAR_ACTIONS {
            let action = registry
                .get(id)
                .unwrap_or_else(|| panic!("missing default hotbar action {id}"));
            let metadata = action.metadata(Locale::En);
            if id.starts_with("slash.") {
                assert_eq!(metadata.category, HotbarActionCategory::Slash);
            } else {
                assert_eq!(metadata.category, HotbarActionCategory::App);
                assert_eq!(metadata.args, HotbarArgsBehavior::None);
            }
            assert_eq!(metadata.recommendation, HotbarRecommendation::Default);
            assert!(
                metadata.recommendation.is_recommendable(),
                "default action must be recommendable: {metadata:?}"
            );
            assert!(!metadata.display_name.trim().is_empty());
            assert!(!metadata.description.trim().is_empty());
        }
    }

    #[test]
    fn slash_action_metadata_describes_args_and_recommendations() {
        let registry = HotbarActionRegistry::with_builtins();

        let compact = registry
            .get("slash.compact")
            .expect("compact slash action")
            .metadata(Locale::En);
        assert_eq!(compact.category, HotbarActionCategory::Slash);
        assert_eq!(compact.source_id, "command:compact");
        assert_eq!(compact.display_name, "/compact");
        // `/compact [focus]` takes an optional summary focus (2026-07-23).
        assert_eq!(compact.args, HotbarArgsBehavior::Optional);
        assert_eq!(compact.safety, HotbarSafetyClass::ExistingCommand);
        assert_eq!(compact.recommendation, HotbarRecommendation::Eligible);

        let mode = registry
            .get("slash.mode")
            .expect("mode slash action")
            .metadata(Locale::En);
        assert_eq!(mode.args, HotbarArgsBehavior::Optional);

        let rename = registry
            .get("slash.rename")
            .expect("rename slash action")
            .metadata(Locale::En);
        assert_eq!(rename.args, HotbarArgsBehavior::Required);
        assert_eq!(rename.recommendation, HotbarRecommendation::Advanced);
    }

    #[test]
    fn reasoning_action_remains_available_for_auto_model_routing() {
        let registry = HotbarActionRegistry::with_builtins();
        let reasoning = registry.get("reasoning.cycle").expect("reasoning action");
        let mut app = test_app();

        let metadata = reasoning.metadata(Locale::En);
        assert_eq!(metadata.category, HotbarActionCategory::App);
        assert_eq!(metadata.safety, HotbarSafetyClass::LocalState);
        assert_eq!(metadata.recommendation, HotbarRecommendation::Eligible);
        assert!(reasoning.disabled_reason(&app).is_none());

        app.auto_model = true;
        assert!(reasoning.disabled_reason(&app).is_none());
    }

    #[test]
    fn hotbar_recommendations_default_to_stable_slot_order() {
        let app = test_app();

        let recommendations =
            recommend_hotbar_actions(&app, HotbarRecommendationOptions::default());

        assert_eq!(
            recommendations
                .iter()
                .map(|entry| entry.metadata.id.as_str())
                .collect::<Vec<_>>(),
            codewhale_config::DEFAULT_HOTBAR_ACTIONS
        );
        assert!(recommendations.iter().all(|entry| {
            entry.metadata.recommendation == HotbarRecommendation::Default
                && entry.disabled_reason.is_none()
        }));
    }

    #[test]
    fn hotbar_recommendations_keep_reasoning_for_auto_model() {
        let mut app = test_app();
        app.auto_model = true;

        let recommendations =
            recommend_hotbar_actions(&app, HotbarRecommendationOptions::for_setup_wizard());

        assert!(
            recommendations
                .iter()
                .any(|entry| entry.metadata.id == "reasoning.cycle")
        );
    }

    #[test]
    fn hotbar_recommendations_exclude_required_args_by_default() {
        let app = test_app();

        let recommendations =
            recommend_hotbar_actions(&app, HotbarRecommendationOptions::for_setup_wizard());

        assert!(
            !recommendations
                .iter()
                .any(|entry| entry.metadata.id == "slash.rename")
        );
    }

    #[test]
    fn hotbar_recommendations_limit_eligible_actions_by_category() {
        let app = test_app();
        let recommendations = recommend_hotbar_actions(
            &app,
            HotbarRecommendationOptions {
                max_total: usize::MAX,
                max_eligible_per_category: 1,
                include_required_args: false,
            },
        );

        for default_id in codewhale_config::DEFAULT_HOTBAR_ACTIONS {
            assert!(
                recommendations
                    .iter()
                    .any(|entry| entry.metadata.id == default_id),
                "default recommendation {default_id} should not be category-capped"
            );
        }
        let slash_recommendations = recommendations
            .iter()
            .filter(|entry| entry.metadata.category == HotbarActionCategory::Slash)
            .collect::<Vec<_>>();
        let default_slash = slash_recommendations
            .iter()
            .filter(|entry| entry.metadata.recommendation == HotbarRecommendation::Default)
            .count();
        let eligible_slash = slash_recommendations
            .iter()
            .filter(|entry| entry.metadata.recommendation == HotbarRecommendation::Eligible)
            .count();
        assert_eq!(default_slash, 3);
        assert_eq!(eligible_slash, 1);
    }

    #[test]
    fn recommended_hotbar_bindings_serialize_action_ids_and_labels() {
        let app = test_app();

        let bindings = recommended_hotbar_bindings(&app, HotbarRecommendationOptions::default());

        assert_eq!(
            bindings
                .iter()
                .map(|binding| binding.action.as_str())
                .collect::<Vec<_>>(),
            codewhale_config::DEFAULT_HOTBAR_ACTIONS
        );
        assert_eq!(
            bindings
                .iter()
                .map(|binding| (binding.slot, binding.label.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (1, Some("wf")),
                (2, Some("goal")),
                (3, Some("auto")),
                (4, Some("plan")),
                (5, Some("agent")),
                (6, Some("operate")),
                (7, Some("palette")),
                (8, Some("side")),
            ]
        );

        let config = codewhale_config::ConfigToml {
            hotbar: Some(bindings.clone()),
            ..Default::default()
        };
        let serialized = toml::to_string_pretty(&config).expect("serialize hotbar recommendations");
        let round_tripped: codewhale_config::ConfigToml =
            toml::from_str(&serialized).expect("deserialize hotbar recommendations");
        assert_eq!(round_tripped.hotbar, Some(bindings));
    }

    #[test]
    fn builtins_register_expected_actions() {
        let mut registry = HotbarActionRegistry::new();
        registry.register_builtins();
        let ids = registry.iter().map(HotbarAction::id).collect::<Vec<_>>();

        assert_eq!(
            ids,
            vec![
                "filetree.toggle",
                "mode.agent",
                "mode.operate",
                "mode.plan",
                "palette.open",
                "reasoning.cycle",
                "session.compact",
                "sidebar.toggle",
                "trust.toggle",
                "voice.toggle",
            ]
        );
        assert!(registry.get("missing.action").is_none());
        for action in registry.iter() {
            assert_eq!(action.category(), "app");
            assert!(
                unicode_width::UnicodeWidthStr::width(action.short_label())
                    <= HOTBAR_COMPACT_LABEL_MAX_WIDTH,
                "{} has an overlong short label",
                action.id()
            );
        }
    }

    #[test]
    fn app_starts_with_builtin_hotbar_registry() {
        let app = test_app();
        assert!(app.hotbar_actions.len() > HotbarActionRegistry::with_builtins().len());
        assert!(app.hotbar_actions.get("mode.agent").is_some());
        assert!(app.hotbar_actions.get("slash.help").is_some());
        assert!(app.hotbar_actions.get("slash.mode").is_some());
        assert!(
            app.hotbar_actions
                .iter()
                .any(|action| action.metadata(Locale::En).category == HotbarActionCategory::Route)
        );
    }

    #[test]
    fn configured_routes_register_provider_model_actions() {
        let mut config = Config::default();
        config
            .provider_config_for_mut(
                &config
                    .builtin_provider_identity(ProviderKind::Openrouter)
                    .unwrap(),
            )
            .unwrap()
            .model = Some("anthropic/claude-sonnet-4".to_string());
        let mut provider_models = HashMap::new();
        provider_models.insert(
            ProviderKind::Openrouter.as_str().to_string(),
            "openai/gpt-4o".to_string(),
        );
        let registry = HotbarActionRegistry::with_configured_routes(
            &config,
            Some(&config.active_provider_identity().unwrap()),
            "deepseek-v4-pro",
            &provider_models,
        );

        let active = registry
            .get("route.deepseek.deepseek-v4-pro")
            .expect("active DeepSeek route");
        assert_eq!(active.category(), "route");
        assert_eq!(
            active.metadata(Locale::En).safety,
            HotbarSafetyClass::ConfigChange
        );

        let openrouter = registry
            .get("route.openrouter.anthropic/claude-sonnet-4")
            .expect("configured OpenRouter route");
        let metadata = openrouter.metadata(Locale::En);
        assert_eq!(metadata.category, HotbarActionCategory::Route);
        assert!(metadata.display_name.contains("OpenRouter"));
        assert!(metadata.display_name.contains("anthropic/claude-sonnet-4"));

        let mut app = test_app();
        assert_eq!(
            openrouter
                .dispatch(&mut app, &config)
                .expect("dispatch route"),
            HotbarDispatch::AppAction(AppAction::SwitchModelRoute {
                identity: config
                    .builtin_provider_identity(ProviderKind::Openrouter)
                    .unwrap(),
                model: "anthropic/claude-sonnet-4".to_string(),
            })
        );
    }

    #[test]
    fn slash_commands_register_as_hotbar_actions() {
        let registry = HotbarActionRegistry::with_builtins();

        for info in commands::command_infos() {
            let action_id = format!("slash.{}", info.name);
            let action = registry
                .get(&action_id)
                .unwrap_or_else(|| panic!("missing slash hotbar action for /{}", info.name));
            assert_eq!(action.category(), "slash");
            assert!(!action.is_active(&test_app()));
            assert!(
                unicode_width::UnicodeWidthStr::width(action.short_label())
                    <= HOTBAR_COMPACT_LABEL_MAX_WIDTH,
                "{action_id} has an overlong short label"
            );
        }
    }

    /// #1888: the hotbar is not a control surface. It binds the owning slash
    /// command and dispatches it through `commands::execute` with no argument,
    /// so what runs is the slash surface — there is no hotbar verb table and
    /// no `ControlSurface::Hotbar` for a test to assert into existence.
    ///
    /// What must hold is narrower and real: every owning command is bound and
    /// directly dispatchable, and a bare press can only reach a verb that
    /// declares `hotbar_bare_dispatch` — which is necessarily targetless and
    /// read-only, because a keypress supplies no id.
    #[test]
    fn control_plane_commands_are_bound_and_bare_dispatch_is_read_only() {
        use codewhale_lane::control::OPERATIONS;
        use codewhale_lane::{ControlAuthority, ControlSurface, TargetKind};

        let registry = HotbarActionRegistry::with_builtins();
        for descriptor in OPERATIONS {
            let action_id = descriptor.hotbar_action_id();
            assert_eq!(action_id, format!("slash.{}", descriptor.slash_command));
            let action = registry
                .get(&action_id)
                .unwrap_or_else(|| panic!("{} has no hotbar action {action_id}", descriptor.id));
            assert_eq!(action.category(), "slash");
            // The dispatch runs as the slash surface, which must therefore be
            // one the descriptor actually offers.
            assert!(descriptor.offers(ControlSurface::Slash));

            // A bare hotbar press fires the command with no arguments, so the
            // owning command must never require one — otherwise the slot would
            // silently become a composer prefill instead of the verb.
            let info = commands::get_command_info(descriptor.slash_command)
                .unwrap_or_else(|| panic!("/{} is not registered", descriptor.slash_command));
            assert!(
                !info.requires_required_argument(),
                "/{} must stay directly dispatchable",
                descriptor.slash_command
            );

            if descriptor.hotbar_bare_dispatch {
                assert_eq!(descriptor.target, TargetKind::None, "{}", descriptor.id);
                assert_eq!(
                    descriptor.authority,
                    ControlAuthority::Read,
                    "{} would mutate durable state from one keypress",
                    descriptor.id
                );
            }
        }

        // Both control domains are bound.
        for id in ["slash.lane", "slash.fleet"] {
            assert!(registry.get(id).is_some(), "{id} must be bindable");
        }
    }

    #[test]
    fn retired_slash_pod_binding_stays_unbound() {
        let registry = HotbarActionRegistry::with_builtins();
        assert!(
            registry.get("slash.pod").is_none(),
            "the retired pod id must not resolve to any action"
        );
    }

    #[test]
    fn slash_hotbar_action_dispatches_argless_command() {
        let registry = HotbarActionRegistry::with_builtins();
        let mode = registry.get("slash.mode").expect("mode slash action");
        let mut app = test_app();

        assert_eq!(
            mode.dispatch(&mut app, &Config::default())
                .expect("dispatch /mode"),
            HotbarDispatch::AppAction(AppAction::OpenModePicker)
        );
        assert!(app.input.is_empty());
    }

    #[test]
    fn slash_hotbar_action_dispatches_optional_argument_command_with_no_args() {
        let registry = HotbarActionRegistry::with_builtins();
        let task = registry.get("slash.task").expect("task slash action");
        let mut app = test_app();

        assert_eq!(
            task.dispatch(&mut app, &Config::default())
                .expect("dispatch /task"),
            HotbarDispatch::AppAction(AppAction::TaskList)
        );
        assert!(app.input.is_empty());
    }

    #[test]
    fn slash_hotbar_action_prefills_required_argument_command() {
        let registry = HotbarActionRegistry::with_builtins();
        let rename = registry.get("slash.rename").expect("rename slash action");
        let mut app = test_app();
        app.input = "draft".to_string();
        app.cursor_position = app.input.chars().count();

        assert_eq!(
            rename
                .dispatch(&mut app, &Config::default())
                .expect("dispatch /rename"),
            HotbarDispatch::Handled
        );
        assert_eq!(app.input, "/rename ");
        assert_eq!(app.cursor_position, app.input.chars().count());
        assert_eq!(app.clear_undo_buffer.as_deref(), Some("draft"));
        assert_eq!(
            app.status_message.as_deref(),
            Some("Command needs arguments; complete /rename")
        );
    }

    #[test]
    fn skill_source_registers_known_skills_with_dedup() {
        let skills = vec![
            ("demo".to_string(), "Demo skill".to_string()),
            ("demo".to_string(), "Shadowed duplicate".to_string()),
            ("   ".to_string(), "ignored blank name".to_string()),
        ];
        let mut registry = HotbarActionRegistry::new();
        registry.register_skills(&skills);

        assert_eq!(registry.len(), 1);
        let action = registry.get("skill.demo").expect("skill action");
        assert_eq!(action.category(), "skill");
        let metadata = action.metadata(Locale::En);
        assert_eq!(metadata.category, HotbarActionCategory::Skill);
        assert_eq!(metadata.source_id, "skill:demo");
        assert_eq!(metadata.display_name, "$demo");
        assert_eq!(metadata.description, "Demo skill");
        assert_eq!(metadata.args, HotbarArgsBehavior::None);
        assert_eq!(metadata.safety, HotbarSafetyClass::ExistingCommand);
        assert_eq!(metadata.recommendation, HotbarRecommendation::Eligible);
        assert!(registry.metadata_validation_errors(Locale::En).is_empty());
    }

    #[test]
    fn replacing_skills_removes_stale_plugin_actions_atomically() {
        let mut registry = HotbarActionRegistry::with_builtins();
        registry.register_skills(&[
            ("native".to_string(), "native Skill".to_string()),
            (
                "demo:review".to_string(),
                "reviewed plugin Skill".to_string(),
            ),
        ]);
        assert!(registry.get("skill.demo:review").is_some());
        let builtin_count = registry
            .iter()
            .filter(|action| action.category() != HotbarActionCategory::Skill.as_str())
            .count();

        registry.replace_skills(&[("native".to_string(), "refreshed".to_string())]);

        assert!(registry.get("skill.demo:review").is_none());
        assert!(registry.get("skill.native").is_some());
        assert_eq!(
            registry
                .iter()
                .filter(|action| action.category() != HotbarActionCategory::Skill.as_str())
                .count(),
            builtin_count,
            "refresh must preserve every non-Skill action source"
        );
    }

    #[test]
    fn skill_hotbar_action_activates_skill_through_dollar_alias() {
        let workspace = tempfile::TempDir::new().expect("workspace");
        let skills_dir = tempfile::TempDir::new().expect("skills dir");
        let skill_dir = skills_dir.path().join("hotbar-demo-skill");
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: hotbar-demo-skill\ndescription: Demo skill for hotbar tests\n---\n\nFollow the demo instructions.\n",
        )
        .expect("write SKILL.md");
        let config = Config {
            skills_dir: Some(skills_dir.path().to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = test_app_with_paths_and_config(
            workspace.path().to_path_buf(),
            skills_dir.path().to_path_buf(),
            &config,
        );

        let action = app
            .hotbar_actions
            .get("skill.hotbar-demo-skill")
            .expect("skill registered from the startup skill cache");
        assert!(!action.is_active(&app));
        assert_eq!(
            action.dispatch(&mut app, &config).expect("dispatch skill"),
            HotbarDispatch::Handled
        );
        assert!(app.active_skill.is_some());
        assert!(action.is_active(&app));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("activated"))
        );
    }

    #[test]
    fn skill_hotbar_action_reports_unknown_skill() {
        let workspace = tempfile::TempDir::new().expect("workspace");
        let skills_dir = tempfile::TempDir::new().expect("skills dir");
        let mut app = test_app_with_paths(
            workspace.path().to_path_buf(),
            skills_dir.path().to_path_buf(),
        );

        let mut registry = HotbarActionRegistry::new();
        registry.register_skills(&[(
            "hotbar-skill-that-does-not-exist".to_string(),
            "Stale cache entry".to_string(),
        )]);
        let action = registry
            .get("skill.hotbar-skill-that-does-not-exist")
            .expect("stale skill action");

        assert_eq!(
            action
                .dispatch(&mut app, &Config::default())
                .expect("dispatch stale skill"),
            HotbarDispatch::Handled
        );
        assert!(app.active_skill.is_none());
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("Unknown skill"))
        );
    }

    #[test]
    fn mcp_source_registers_enabled_server_tools_only() {
        let snapshot = test_mcp_snapshot();
        let mut registry = HotbarActionRegistry::new();
        registry.replace_mcp_tools(Some(&snapshot));

        // Only the enabled server's tool with a model name registers: the
        // blank-model-name tool and the disabled server's tool never do.
        assert_eq!(registry.len(), 1);
        let action = registry
            .get("mcp.search.web_search")
            .expect("mcp tool action");
        assert_eq!(action.category(), "mcp");
        let metadata = action.metadata(Locale::En);
        assert_eq!(metadata.category, HotbarActionCategory::Mcp);
        assert_eq!(metadata.source_id, "mcp:search");
        assert_eq!(metadata.display_name, "mcp:search:web_search");
        assert!(metadata.description.contains("mcp_search_web_search"));
        assert!(metadata.description.contains("Search the web"));
        assert_eq!(metadata.args, HotbarArgsBehavior::Required);
        assert_eq!(metadata.safety, HotbarSafetyClass::RequiresApproval);
        assert_eq!(metadata.recommendation, HotbarRecommendation::Advanced);
        assert!(registry.metadata_validation_errors(Locale::En).is_empty());
    }

    #[test]
    fn mcp_hotbar_action_prefills_composer_instead_of_executing() {
        let snapshot = test_mcp_snapshot();
        let mut registry = HotbarActionRegistry::new();
        registry.replace_mcp_tools(Some(&snapshot));
        let action = registry
            .get("mcp.search.web_search")
            .expect("mcp tool action");
        let mut app = test_app();
        app.input = "draft".to_string();
        app.cursor_position = app.input.chars().count();

        assert_eq!(
            action
                .dispatch(&mut app, &Config::default())
                .expect("dispatch mcp tool"),
            HotbarDispatch::Handled
        );
        assert_eq!(app.input, "mcp_search_web_search ");
        assert_eq!(app.cursor_position, app.input.chars().count());
        assert_eq!(app.clear_undo_buffer.as_deref(), Some("draft"));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("mcp_search_web_search"))
        );
    }

    #[test]
    fn replace_mcp_tools_refreshes_and_clears_mcp_actions() {
        let mut registry = HotbarActionRegistry::with_builtins();
        let baseline = registry.len();
        let snapshot = test_mcp_snapshot();

        registry.replace_mcp_tools(Some(&snapshot));
        assert_eq!(registry.len(), baseline + 1);
        // Re-applying a refreshed snapshot must not panic on duplicate ids.
        registry.replace_mcp_tools(Some(&snapshot));
        assert_eq!(registry.len(), baseline + 1);

        registry.replace_mcp_tools(None);
        assert_eq!(registry.len(), baseline);
        assert!(registry.get("mcp.search.web_search").is_none());
    }

    #[test]
    fn mode_actions_report_active_state_and_dispatch() {
        let registry = HotbarActionRegistry::with_builtins();
        let plan = registry.get("mode.plan").expect("plan action");
        let agent = registry.get("mode.agent").expect("agent action");
        let operate = registry.get("mode.operate").expect("operate action");
        let mut app = test_app();

        assert!(agent.is_active(&app));
        assert!(!plan.is_active(&app));
        assert!(registry.get("mode.yolo").is_none());

        assert_eq!(
            plan.dispatch(&mut app, &Config::default())
                .expect("dispatch plan"),
            HotbarDispatch::AppAction(AppAction::ModeChanged(AppMode::Plan))
        );
        assert_eq!(app.mode, AppMode::Plan);
        assert!(plan.is_active(&app));
        assert!(!agent.is_active(&app));

        assert_eq!(
            operate
                .dispatch(&mut app, &Config::default())
                .expect("dispatch operate"),
            HotbarDispatch::AppAction(AppAction::ModeChanged(AppMode::Operate))
        );
        assert_eq!(app.mode, AppMode::Operate);
        assert!(operate.is_active(&app));
        assert!(!agent.is_active(&app));
    }

    #[test]
    fn compact_action_emits_existing_app_action() {
        let registry = HotbarActionRegistry::with_builtins();
        let compact = registry.get("session.compact").expect("compact action");
        let mut app = test_app();

        assert!(!compact.is_active(&app));
        assert_eq!(
            compact
                .dispatch(&mut app, &Config::default())
                .expect("dispatch compact"),
            HotbarDispatch::AppAction(AppAction::CompactContext { focus: None })
        );
        app.is_compacting = true;
        assert!(compact.is_active(&app));
        assert_eq!(
            compact
                .dispatch(&mut app, &Config::default())
                .expect("dispatch compact while busy"),
            HotbarDispatch::AppAction(AppAction::CompactContext { focus: None })
        );
    }

    #[test]
    fn reasoning_cycle_updates_effort_and_compaction() {
        let registry = HotbarActionRegistry::with_builtins();
        let reasoning = registry.get("reasoning.cycle").expect("reasoning action");
        let mut app = test_app();
        app.api_provider = ProviderKind::Deepseek;
        app.reasoning_effort = ReasoningEffort::Off;

        assert!(!reasoning.is_active(&app));
        assert!(matches!(
            reasoning
                .dispatch(&mut app, &Config::default())
                .expect("dispatch reasoning"),
            HotbarDispatch::AppAction(AppAction::UpdateCompaction(_))
        ));
        assert_eq!(app.reasoning_effort, ReasoningEffort::Low);
        assert!(reasoning.is_active(&app));
        assert_eq!(app.status_message.as_deref(), Some("Reasoning effort: low"));

        app.auto_model = true;
        assert!(reasoning.is_active(&app));
        assert!(matches!(
            reasoning
                .dispatch(&mut app, &Config::default())
                .expect("dispatch reasoning under auto model"),
            HotbarDispatch::AppAction(AppAction::UpdateCompaction(_))
        ));
        assert_eq!(app.reasoning_effort, ReasoningEffort::Medium);
    }

    #[test]
    fn reasoning_cycle_is_inert_while_a_turn_is_running() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", tmp.path());
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join(".codewhale"));
        let _deepseek_config = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_config = crate::test_support::EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _writes = crate::tui::startup_defaults::allow_writes_in_tests();

        crate::settings::Settings::transact(|settings| {
            settings.reasoning_effort = Some("off".to_string());
            Ok(())
        })
        .expect("seed startup reasoning");

        let registry = HotbarActionRegistry::with_builtins();
        let reasoning = registry.get("reasoning.cycle").expect("reasoning action");
        let mut app = test_app();
        app.api_provider = ProviderKind::Deepseek;
        app.auto_model = false;
        app.reasoning_effort = ReasoningEffort::Off;
        app.is_loading = true;

        assert_eq!(
            reasoning
                .dispatch(&mut app, &Config::default())
                .expect("dispatch while busy"),
            HotbarDispatch::Handled
        );
        assert_eq!(app.reasoning_effort, ReasoningEffort::Off);
        assert_eq!(app.startup_defaults.pending_len(), 0);
        assert_eq!(
            crate::settings::Settings::load()
                .expect("reload settings")
                .reasoning_effort
                .as_deref(),
            Some("off"),
            "a refused hotbar action must not persist a different tier"
        );
    }

    #[test]
    fn reasoning_cycle_uses_codex_effort_tiers() {
        // Codex tiers are now per-model, read from the OAuth roster. Point
        // CODEX_HOME at an empty directory so this exercises the static
        // fallback ladder instead of whatever roster the developer's own
        // machine happens to have cached.
        let _lock = crate::test_support::lock_test_env();
        let codex_home = tempfile::TempDir::new().expect("codex home");
        let _codex_home = crate::test_support::EnvVarGuard::set("CODEX_HOME", codex_home.path());
        let registry = HotbarActionRegistry::with_builtins();
        let reasoning = registry.get("reasoning.cycle").expect("reasoning action");
        let mut app = test_app();
        app.set_provider_identity_record(
            crate::config::Config::default()
                .resolve_provider_identity(ProviderKind::OpenaiCodex.as_str())
                .expect("captured fixture provider"),
        );
        app.auto_model = false;
        app.reasoning_effort = ReasoningEffort::Low;

        for (expected_effort, expected_label) in [
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Max, "max"),
            (ReasoningEffort::Low, "low"),
        ] {
            assert!(matches!(
                reasoning
                    .dispatch(&mut app, &Config::default())
                    .expect("dispatch reasoning"),
                HotbarDispatch::AppAction(AppAction::UpdateCompaction(_))
            ));
            assert_eq!(app.reasoning_effort, expected_effort);
            let expected_message = format!("Reasoning effort: {expected_label}");
            assert_eq!(
                app.status_message.as_deref(),
                Some(expected_message.as_str())
            );
        }
    }

    #[test]
    fn sidebar_toggle_reports_visibility_and_dispatches() {
        let registry = HotbarActionRegistry::with_builtins();
        let sidebar = registry.get("sidebar.toggle").expect("sidebar action");
        let mut app = test_app();
        app.work_surface.placement = crate::tui::work_surface::WorkSurfacePlacement::Top;

        assert!(sidebar.is_active(&app));
        assert_eq!(
            sidebar
                .dispatch(&mut app, &Config::default())
                .expect("dispatch rail hide"),
            HotbarDispatch::Handled
        );
        assert_eq!(
            app.work_surface.placement,
            crate::tui::work_surface::WorkSurfacePlacement::Off
        );
        assert!(!sidebar.is_active(&app));

        sidebar
            .dispatch(&mut app, &Config::default())
            .expect("dispatch rail show");
        assert_eq!(
            app.work_surface.placement,
            crate::tui::work_surface::WorkSurfacePlacement::Bottom,
            "toggling the rail back on restores the round-3 default"
        );
        assert!(sidebar.is_active(&app));
    }

    #[tokio::test]
    async fn filetree_toggle_reports_open_state_and_dispatches() {
        let registry = HotbarActionRegistry::with_builtins();
        let filetree = registry.get("filetree.toggle").expect("filetree action");
        let mut app = test_app();

        assert!(!filetree.is_active(&app));
        assert_eq!(
            filetree
                .dispatch(&mut app, &Config::default())
                .expect("dispatch filetree open"),
            HotbarDispatch::Handled
        );
        assert!(app.file_tree.is_some());
        assert!(filetree.is_active(&app));

        filetree
            .dispatch(&mut app, &Config::default())
            .expect("dispatch filetree close");
        assert!(app.file_tree.is_none());
        assert!(!filetree.is_active(&app));
    }

    #[test]
    fn palette_action_opens_command_palette() {
        let registry = HotbarActionRegistry::with_builtins();
        let palette = registry.get("palette.open").expect("palette action");
        let mut app = test_app();

        assert!(!palette.is_active(&app));
        assert_eq!(
            palette
                .dispatch(&mut app, &Config::default())
                .expect("dispatch palette"),
            HotbarDispatch::Handled
        );
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::CommandPalette));
    }

    #[test]
    fn trust_toggle_reports_trust_state_and_dispatches() {
        let registry = HotbarActionRegistry::with_builtins();
        let trust = registry.get("trust.toggle").expect("trust action");
        let mut app = test_app();
        app.trust_mode = false;

        assert!(!trust.is_active(&app));
        assert_eq!(
            trust
                .dispatch(&mut app, &Config::default())
                .expect("dispatch trust on"),
            HotbarDispatch::Handled
        );
        assert!(app.trust_mode);
        assert!(trust.is_active(&app));

        trust
            .dispatch(&mut app, &Config::default())
            .expect("dispatch trust off");
        assert!(!app.trust_mode);
        assert!(!trust.is_active(&app));
    }

    #[test]
    fn voice_toggle_dispatches_the_voice_command() {
        let registry = HotbarActionRegistry::with_builtins();
        let voice = registry.get("voice.toggle").expect("voice action");
        let mut app = test_app();

        assert!(!voice.is_active(&app));
        // The toggle is wired to the /voice command. With a recorder on the
        // host it arms voice input and defers capture to the UI event loop;
        // without one it fails gracefully with a localized error. No audio
        // is recorded in either case.
        let result = voice
            .dispatch(&mut app, &Config::default())
            .expect("dispatch voice");
        assert!(app.status_message.is_some());
        // The old placeholder message must be gone — voice is implemented.
        assert_ne!(
            app.status_message.as_deref(),
            Some("Voice input is not available in this terminal session yet.")
        );
        if app.voice_enabled {
            assert_eq!(
                result,
                HotbarDispatch::AppAction(crate::tui::app::AppAction::VoiceCapture)
            );
            assert!(voice.is_active(&app));
            // A second press toggles voice input back off.
            let off = voice
                .dispatch(&mut app, &Config::default())
                .expect("dispatch voice off");
            assert_eq!(off, HotbarDispatch::Handled);
            assert!(!app.voice_enabled);
            assert!(!voice.is_active(&app));
        } else {
            assert_eq!(result, HotbarDispatch::Handled);
        }
    }
}
