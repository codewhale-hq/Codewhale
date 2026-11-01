//! Unified inventory for Codewhale extensions.
//!
//! This is deliberately a projection over the existing owners of Hooks,
//! Plugins, Marketplace catalogs, Skills, and MCP. It has no registry, trust
//! database, installer, or network fetch of its own. Future actions emitted by
//! this view must delegate to the existing command/mutation controllers.
//! The skills mutation manager remains at `/skills manage`; MCP setup is a
//! read-only suggestions handoff, not an inline installer or credential editor.

use codewhale_engine::mcp::McpServerConfigExt;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::{
    CommandPaletteAction, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
    render_underwater_surface, truncate_view_text,
};
use crate::tui::app::App;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_tui_kit::menu_style;

fn localize(locale: Locale, id: MessageId, replacements: &[(&str, &str)]) -> String {
    let mut value = tr(locale, id).into_owned();
    for (name, replacement) in replacements {
        value = value.replace(&format!("{{{name}}}"), replacement);
    }
    value
}

/// All extension surfaces in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionsTab {
    Hooks,
    Plugins,
    Marketplace,
    Skills,
    Mcp,
}

impl ExtensionsTab {
    pub const ALL: [Self; 5] = [
        Self::Hooks,
        Self::Plugins,
        Self::Marketplace,
        Self::Skills,
        Self::Mcp,
    ];

    #[must_use]
    fn label(self, locale: Locale) -> String {
        match self {
            Self::Hooks => tr(locale, MessageId::ExtensionsTabHooks),
            Self::Plugins => tr(locale, MessageId::ExtensionsTabPlugins),
            Self::Marketplace => tr(locale, MessageId::ExtensionsTabMarketplace),
            Self::Skills => tr(locale, MessageId::HelpSkills),
            Self::Mcp => tr(locale, MessageId::ConfigSectionMcp),
        }
        .into_owned()
    }

    const fn index(self) -> usize {
        match self {
            Self::Hooks => 0,
            Self::Plugins => 1,
            Self::Marketplace => 2,
            Self::Skills => 3,
            Self::Mcp => 4,
        }
    }

    const fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    const fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// A real capability contributed by one plugin product.
///
/// Recommendations use the same component vocabulary as installed plugin
/// bundles: a component is an MCP server or a Skill, the two things this
/// panel can actually install and switch on. Kinds that named a runtime
/// nobody could install from here — a browser driver, a sandbox helper —
/// were removed rather than left advertising an unreachable action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginProductComponentKind {
    Mcp,
    Skills,
}

impl PluginProductComponentKind {
    fn label(self, locale: Locale) -> String {
        match self {
            Self::Mcp => tr(locale, MessageId::ConfigSectionMcp),
            Self::Skills => tr(locale, MessageId::HelpSkills),
        }
        .into_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProductComponent {
    pub kind: PluginProductComponentKind,
    pub name: String,
}

/// Marketplace-facing recommendation model.
///
/// `source_reference` is display provenance only. It is intentionally not an
/// install command or executable plan; explicit installation still enters the
/// reviewed plugin installer and trust flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProduct {
    pub id: String,
    pub name: String,
    pub description: String,
    pub publisher: String,
    pub source_reference: String,
    pub components: Vec<PluginProductComponent>,
    pub maturity: String,
}

impl PluginProduct {
    fn into_row(self, locale: Locale) -> ExtensionItem {
        let mut components = String::new();
        for (index, component) in self.components.iter().enumerate() {
            if index > 0 {
                components.push_str(", ");
            }
            let _ = write!(
                components,
                "{} ({})",
                component.name,
                component.kind.label(locale)
            );
        }
        ExtensionItem {
            id: self.id,
            label: self.name,
            tone: ExtensionTone::Idle,
            description: self.description,
            state: self.maturity,
            detail: localize(
                locale,
                MessageId::ExtensionsProductDetail,
                &[
                    ("publisher", &self.publisher),
                    ("components", &components),
                    ("source", &self.source_reference),
                ],
            ),
            action: None,
            toggle: None,
            remove: None,
        }
    }
}

/// What a row's state *means*, independent of the words it uses to say it.
///
/// Every row on this screen used to paint in one colour, so twenty servers,
/// four of them broken, read as one undifferentiated wall — "incredibly
/// boring, plain, and hard on the eyes because of the sameness". The tone is
/// typed rather than sniffed out of the localized state string, because a
/// screen that only colours correctly in English is not coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtensionTone {
    /// Working: connected, enabled, active.
    Ready,
    /// Wants a person: auth required, disconnected, not yet reviewed.
    Attention,
    /// Broken: an error or a rejected entry.
    Failure,
    /// Deliberately off, or simply not configured.
    #[default]
    Idle,
}

impl ExtensionTone {
    fn ink(self) -> codewhale_palette::ChromeInk {
        use codewhale_palette::ChromeInk;
        match self {
            Self::Ready => ChromeInk::Outcome,
            Self::Attention => ChromeInk::Attention,
            Self::Failure => ChromeInk::Failure,
            Self::Idle => ChromeInk::Metadata,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub state: String,
    /// Semantic reading of `state`, resolved through the theme's ink grammar.
    pub tone: ExtensionTone,
    pub detail: String,
    pub action: Option<ExtensionAction>,
    /// Reversible on/off toggle for the row (`e`): enable or disable a
    /// plugin or MCP server without leaving the panel.
    pub toggle: Option<ExtensionAction>,
    /// Destructive removal for the row (`d` / Delete, or the row's
    /// right-click menu; confirmed in two steps either way). Only MCP servers
    /// offer it today; plugins keep their reviewed uninstall flow.
    pub remove: Option<ExtensionAction>,
}

/// Where a row's command lands when the user activates it.
///
/// Every row used to close the panel and drop a slash command into the
/// transcript — inspecting a plugin closed the list and pasted its detail
/// into chat, and a mutation left every other row reading open-time state.
/// Only the row knows which its command is, so the disposition lives on the
/// action: mutations and inspects act in place, and flows that own a
/// different surface (an editor, OAuth login, a composer-bound trust token)
/// still yield the panel to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowActionDisposition {
    /// Run the command with the panel open; the host refreshes the snapshot
    /// afterwards so every row re-reads live state.
    InPlace,
    /// In place, and the command's text output renders in a pager stacked on
    /// the panel — the inspect path that keeps detail out of the transcript.
    InPlacePager,
    /// The command owns a different surface; the panel yields to it.
    LeavePanel,
}

/// A row affordance. Executable actions route back through the existing slash
/// command controller; status-only actions explain why Enter will not mutate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionAction {
    Command {
        label: String,
        command: String,
        disposition: RowActionDisposition,
    },
    Status {
        label: String,
    },
}

impl ExtensionAction {
    fn label(&self) -> &str {
        match self {
            Self::Command { label, .. } | Self::Status { label } => label,
        }
    }

    fn command(&self) -> Option<&str> {
        match self {
            Self::Command { command, .. } => Some(command),
            Self::Status { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionGroup {
    pub id: String,
    pub label: String,
    pub items: Vec<ExtensionItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionsTabModel {
    pub groups: Vec<ExtensionGroup>,
    pub problem: Option<String>,
}

/// Read model captured when the modal opens. No source is contacted over the
/// network and no extension process is started while building it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionsSnapshot {
    tabs: [ExtensionsTabModel; 5],
    /// MCP manager generation and initializing flag at capture time. The
    /// open panel reports these on its bounded poll so the host rebuilds the
    /// model only when live state actually moved.
    pub mcp_generation: u64,
    pub mcp_initializing: bool,
}

impl ExtensionsSnapshot {
    #[must_use]
    pub fn from_app(app: &App) -> Self {
        let mut snapshot = Self {
            mcp_generation: app.mcp_snapshot_generation,
            mcp_initializing: app.mcp_boot_initializing(),
            ..Self::default()
        };
        snapshot.tabs[ExtensionsTab::Hooks.index()] = hooks_model(app, app.ui_locale);
        snapshot.tabs[ExtensionsTab::Plugins.index()] = plugins_model(app, app.ui_locale);
        snapshot.tabs[ExtensionsTab::Marketplace.index()] = marketplace_model(app, app.ui_locale);
        snapshot.tabs[ExtensionsTab::Skills.index()] = skills_model(app, app.ui_locale);
        snapshot.tabs[ExtensionsTab::Mcp.index()] = mcp_model(app, app.ui_locale);
        snapshot
            .with_recommendations(reviewed_product_catalog(app.ui_locale), app.ui_locale)
            .with_recommended_actions(app)
    }

    #[must_use]
    pub fn with_recommendations(mut self, products: Vec<PluginProduct>, locale: Locale) -> Self {
        if !products.is_empty() {
            self.tabs[ExtensionsTab::Marketplace.index()].groups.insert(
                0,
                ExtensionGroup {
                    id: "recommended".into(),
                    label: tr(locale, MessageId::ExtensionsGroupRecommended).into_owned(),
                    items: products
                        .into_iter()
                        .map(|product| product.into_row(locale))
                        .collect(),
                },
            );
        }
        self
    }

    fn with_recommended_actions(mut self, app: &App) -> Self {
        let configured = codewhale_engine::mcp::load_config_with_workspace_and_plugins(
            &app.mcp_config_path,
            &app.workspace,
            app.plugin_registry.as_ref(),
        )
        .ok();
        let computer_use = self.tabs[ExtensionsTab::Marketplace.index()]
            .groups
            .iter()
            .find(|group| group.id == "codewhale")
            .and_then(|group| {
                group
                    .items
                    .iter()
                    .find(|item| item.id == "codewhale:computer-use")
            })
            .cloned();
        let Some(group) = self.tabs[ExtensionsTab::Marketplace.index()]
            .groups
            .iter_mut()
            .find(|group| group.id == "recommended")
        else {
            return self;
        };

        if configured.is_none() {
            for item in &mut group.items {
                item.action = Some(ExtensionAction::Status {
                    label: tr(app.ui_locale, MessageId::PickerActionUnavailable).into_owned(),
                });
            }
            return self;
        }

        for item in &mut group.items {
            if item.id == "codewhale-computer-use" {
                if let Some(catalog_item) = &computer_use {
                    item.state = catalog_item.state.clone();
                    item.tone = catalog_item.tone;
                    item.action = catalog_item.action.clone();
                } else {
                    item.action = Some(ExtensionAction::Status {
                        label: tr(app.ui_locale, MessageId::PickerActionUnavailable).into_owned(),
                    });
                }
                continue;
            }
            let recommendation = match item.id.as_str() {
                "playwright-browser" => Some(("playwright", "playwright")),
                "chrome-devtools" => Some(("chrome-devtools", "chrome-devtools")),
                _ => None,
            };
            if let Some((server_name, recommendation_id)) = recommendation {
                match configured
                    .as_ref()
                    .and_then(|config| config.servers.get(server_name))
                {
                    None => {
                        item.state =
                            tr(app.ui_locale, MessageId::ExtensionsStateAvailable).into_owned();
                        item.action = Some(ExtensionAction::Command {
                            label: tr(app.ui_locale, MessageId::ExtensionsActionAdd).into_owned(),
                            command: format!("/mcp add recommended {recommendation_id}"),
                            disposition: RowActionDisposition::InPlace,
                        });
                    }
                    Some(server) if !server.is_enabled() => {
                        item.state =
                            tr(app.ui_locale, MessageId::HotbarSetupStatusDisabled).into_owned();
                        item.action = Some(ExtensionAction::Command {
                            label: tr(app.ui_locale, MessageId::ExtensionsActionEnable)
                                .into_owned(),
                            command: format!("/mcp enable {server_name}"),
                            disposition: RowActionDisposition::InPlace,
                        });
                    }
                    Some(_) => {
                        item.state =
                            tr(app.ui_locale, MessageId::PickerActionConfigured).into_owned();
                        item.action = Some(ExtensionAction::Status {
                            label: tr(app.ui_locale, MessageId::PickerActionConfigured)
                                .into_owned(),
                        });
                    }
                }
            } else {
                item.action = Some(ExtensionAction::Status {
                    label: tr(app.ui_locale, MessageId::PickerActionUnavailable).into_owned(),
                });
            }
        }
        self
    }

    fn tab(&self, tab: ExtensionsTab) -> &ExtensionsTabModel {
        &self.tabs[tab.index()]
    }
}

/// Pinned review metadata only. These rows do not contain install commands,
/// do not fetch anything, and do not grant trust. The source-specific plugin
/// manifests produced by the packaging lane remain the installation authority.
///
/// Every row here must resolve to a real action in
/// [`ExtensionsSnapshot::with_recommended_actions`]. Rows that could only
/// ever render `unavailable` — Browser Use, the sandbox runtime — were
/// removed: a recommendation a person cannot act on is an advertisement, and
/// it made the tab read as a list of things Codewhale would not do.
fn reviewed_product_catalog(locale: Locale) -> Vec<PluginProduct> {
    vec![
        // First-party computer use. This is the only computer-use product
        // row: third-party desktop-control MCPs are not recommended here.
        PluginProduct {
            id: "codewhale-computer-use".into(),
            name: "Computer Use".into(),
            description: tr(
                locale,
                MessageId::ExtensionsProductCodewhaleComputerUseDescription,
            )
            .into_owned(),
            publisher: "Codewhale".into(),
            source_reference: "https://github.com/codewhale-hq/codewhale-cu-plugin".into(),
            components: vec![
                PluginProductComponent {
                    kind: PluginProductComponentKind::Mcp,
                    name: "Computer Use MCP".into(),
                },
                PluginProductComponent {
                    kind: PluginProductComponentKind::Skills,
                    name: "Computer Use Skill".into(),
                },
            ],
            maturity: tr(locale, MessageId::ExtensionsStateFirstParty).into_owned(),
        },
        PluginProduct {
            id: "playwright-browser".into(),
            name: "Playwright Browser".into(),
            description: tr(locale, MessageId::ExtensionsProductPlaywrightDescription).into_owned(),
            publisher: "Microsoft".into(),
            source_reference: "microsoft/playwright-mcp".into(),
            components: vec![PluginProductComponent {
                kind: PluginProductComponentKind::Mcp,
                name: "Playwright MCP".into(),
            }],
            maturity: tr(locale, MessageId::ExtensionsStateReviewedCandidate).into_owned(),
        },
        PluginProduct {
            id: "chrome-devtools".into(),
            name: "Chrome DevTools".into(),
            description: tr(locale, MessageId::ExtensionsProductChromeDescription).into_owned(),
            publisher: "Chrome DevTools".into(),
            source_reference: "ChromeDevTools/chrome-devtools-mcp".into(),
            components: vec![PluginProductComponent {
                kind: PluginProductComponentKind::Mcp,
                name: "Chrome DevTools MCP".into(),
            }],
            maturity: tr(locale, MessageId::ExtensionsStateReviewedCandidate).into_owned(),
        },
    ]
}

fn hooks_model(app: &App, locale: Locale) -> ExtensionsTabModel {
    let config = app.hooks.config();
    let configured = config
        .hooks
        .iter()
        .enumerate()
        .map(|(index, hook)| ExtensionItem {
            id: format!("hook-{index}"),
            tone: if config.enabled {
                ExtensionTone::Ready
            } else {
                ExtensionTone::Idle
            },
            label: hook.name.clone().unwrap_or_else(|| {
                localize(
                    locale,
                    MessageId::ExtensionsHookFallback,
                    &[("event", hook.event.as_str())],
                )
            }),
            description: hook.event.as_str().to_string(),
            state: if config.enabled {
                tr(locale, MessageId::ExtensionsStateEnabled)
            } else {
                tr(locale, MessageId::HotbarSetupStatusDisabled)
            }
            .into_owned(),
            detail: localize(
                locale,
                MessageId::ExtensionsHookDetail,
                &[
                    ("timeout", &hook.timeout_secs.to_string()),
                    ("background", &localized_bool(locale, hook.background)),
                    (
                        "continue_on_error",
                        &localized_bool(locale, hook.continue_on_error),
                    ),
                ],
            ),
            action: Some(ExtensionAction::Command {
                label: tr(locale, MessageId::ExtensionsActionEdit).into_owned(),
                command: "/hooks edit".into(),
                disposition: RowActionDisposition::LeavePanel,
            }),
            toggle: None,
            remove: None,
        })
        .collect::<Vec<_>>();
    let problems = config
        .problems
        .iter()
        .enumerate()
        .map(|(index, problem)| ExtensionItem {
            id: format!("hook-problem-{index}"),
            tone: if problem.rejected {
                ExtensionTone::Failure
            } else {
                ExtensionTone::Attention
            },
            label: problem.name.clone().unwrap_or_else(|| {
                tr(locale, MessageId::ExtensionsHooksConfiguration).into_owned()
            }),
            description: problem.detail.clone(),
            state: if problem.rejected {
                tr(locale, MessageId::ExtensionsStateRejected)
            } else {
                tr(locale, MessageId::ExtensionsStateWarning)
            }
            .into_owned(),
            detail: codewhale_engine::hooks::config::problem_summary(problem),
            action: Some(ExtensionAction::Command {
                label: tr(locale, MessageId::ExtensionsActionEdit).into_owned(),
                command: "/hooks edit".into(),
                disposition: RowActionDisposition::LeavePanel,
            }),
            toggle: None,
            remove: None,
        })
        .collect::<Vec<_>>();
    let mut groups = Vec::new();
    if !configured.is_empty() {
        groups.push(ExtensionGroup {
            id: "configured".into(),
            label: tr(locale, MessageId::ExtensionsGroupConfigured).into_owned(),
            items: configured,
        });
    }
    if !problems.is_empty() {
        groups.push(ExtensionGroup {
            id: "problems".into(),
            label: tr(locale, MessageId::ExtensionsGroupProblems).into_owned(),
            items: problems,
        });
    }
    // A screen with nothing on it and nothing to press is where "need to be
    // able to add hooks!" comes from. The row that teaches the file is the
    // row that opens it.
    if groups.is_empty() {
        groups.push(ExtensionGroup {
            id: "start".into(),
            label: tr(locale, MessageId::ExtensionsGroupConfigured).into_owned(),
            items: vec![ExtensionItem {
                id: "hooks-add".into(),
                tone: ExtensionTone::Idle,
                label: tr(locale, MessageId::ExtensionsHooksAddLabel).into_owned(),
                description: tr(locale, MessageId::ExtensionsHooksAddDescription).into_owned(),
                state: tr(locale, MessageId::ExtensionsStateAvailable).into_owned(),
                detail: ".codewhale/hooks.toml".into(),
                action: Some(ExtensionAction::Command {
                    label: tr(locale, MessageId::ExtensionsActionEdit).into_owned(),
                    command: "/hooks edit".into(),
                    disposition: RowActionDisposition::LeavePanel,
                }),
                toggle: None,
                remove: None,
            }],
        });
    }
    ExtensionsTabModel {
        groups,
        problem: None,
    }
}

fn inventory_summary(
    inventory: &codewhale_plugins::manifest::PluginInventory,
    locale: Locale,
) -> String {
    let mut parts = Vec::new();
    if inventory.skills > 0 {
        parts.push(localize(
            locale,
            MessageId::ExtensionsInventorySkills,
            &[("count", &inventory.skills.to_string())],
        ));
    }
    if inventory.mcp_servers > 0 {
        parts.push(localize(
            locale,
            MessageId::ExtensionsInventoryMcp,
            &[("count", &inventory.mcp_servers.to_string())],
        ));
    }
    if inventory.hooks > 0 {
        parts.push(localize(
            locale,
            MessageId::ExtensionsInventoryHooks,
            &[("count", &inventory.hooks.to_string())],
        ));
    }
    if inventory.commands > 0 {
        parts.push(localize(
            locale,
            MessageId::ExtensionsInventoryCommands,
            &[("count", &inventory.commands.to_string())],
        ));
    }
    if inventory.agents > 0 {
        parts.push(localize(
            locale,
            MessageId::ExtensionsInventoryAgents,
            &[("count", &inventory.agents.to_string())],
        ));
    }
    if parts.is_empty() {
        tr(locale, MessageId::ExtensionsInventoryNone).into_owned()
    } else {
        parts.join(", ")
    }
}

fn localized_bool(locale: Locale, value: bool) -> String {
    tr(
        locale,
        if value {
            MessageId::ExtensionsValueYes
        } else {
            MessageId::ExtensionsValueNo
        },
    )
    .into_owned()
}

/// Say what the row *is*, in the words a person already uses for a switch.
///
/// "enabled, untrusted" named an internal pair of booleans and gave no hint
/// that anything could be done about it; a founder read the panel and said
/// "i don't know what untrusted means and i can't even do anything about
/// it". The switch now reads on/off and the review requirement reads
/// "needs review", which is both what it is and the verb `e`/Enter runs.
fn localized_plugin_state(locale: Locale, state: &str) -> String {
    let id = match state {
        "active" => MessageId::ExtensionsStateOn,
        "disabled" | "inactive" => MessageId::ExtensionsStateOff,
        "enabled-untrusted" => MessageId::ExtensionsStateNeedsReview,
        "unstaged" => MessageId::ExtensionsStateUnstaged,
        "inapplicable" => MessageId::ExtensionsStateInapplicable,
        "unsupported" => MessageId::ExtensionsStateUnsupported,
        _ => return state.to_string(),
    };
    tr(locale, id).into_owned()
}

/// The row's state line, which must answer both "is the switch on?" and
/// "is anything standing between this and running?" at once.
///
/// A bundle that is switched off *and* never reviewed reads `off · needs
/// review`, so turning it on is visibly a two-part step rather than a dead
/// `disabled` with no explanation — the exact row (`computer-use`) the
/// founder could not enable.
fn plugin_row_state(locale: Locale, plugin: &codewhale_plugins::types::LoadedPlugin) -> String {
    let state = localized_plugin_state(locale, plugin.state_label());
    if plugin.trusted() || plugin.enabled {
        return state;
    }
    format!(
        "{state} · {}",
        tr(locale, MessageId::ExtensionsStateNeedsReview)
    )
}

fn localized_trust(locale: Locale, trust: &str) -> String {
    let id = match trust {
        "trusted" => MessageId::ExtensionsTrustTrusted,
        "not-reviewed" => MessageId::ExtensionsTrustNotReviewed,
        "content-changed" => MessageId::ExtensionsTrustContentChanged,
        "capabilities-changed" => MessageId::ExtensionsTrustCapabilitiesChanged,
        _ => return trust.to_string(),
    };
    tr(locale, id).into_owned()
}

fn localized_compatibility(locale: Locale, compatibility: &str) -> String {
    let id = match compatibility {
        "full" => MessageId::ExtensionsCompatibilityFull,
        "partial" => MessageId::ExtensionsCompatibilityPartial,
        "unsupported" => MessageId::ExtensionsStateUnsupported,
        _ => return compatibility.to_string(),
    };
    tr(locale, id).into_owned()
}

fn localized_tier(locale: Locale, tier: &str) -> String {
    let id = match tier {
        "community" => MessageId::ExtensionsTierCommunity,
        "official" => MessageId::ExtensionsTierOfficial,
        "curated" => MessageId::ExtensionsTierCurated,
        "partner" => MessageId::ExtensionsTierPartner,
        _ => return tier.to_string(),
    };
    tr(locale, id).into_owned()
}

fn localized_skill_root(locale: Locale, kind: codewhale_skills::roots::SkillRootKind) -> String {
    use codewhale_skills::roots::SkillRootKind;

    match kind {
        SkillRootKind::CodeWhaleProject => {
            tr(locale, MessageId::ExtensionsSkillRootProject).into_owned()
        }
        SkillRootKind::CodeWhaleGlobal => {
            tr(locale, MessageId::ExtensionsSkillRootGlobal).into_owned()
        }
        SkillRootKind::CompatibleProject(harness) => localize(
            locale,
            MessageId::ExtensionsSkillRootCompatibleProject,
            &[("harness", harness.label())],
        ),
        SkillRootKind::CompatibleGlobal(harness) => localize(
            locale,
            MessageId::ExtensionsSkillRootCompatibleGlobal,
            &[("harness", harness.label())],
        ),
        SkillRootKind::Configured => {
            tr(locale, MessageId::ExtensionsSkillRootConfigured).into_owned()
        }
        SkillRootKind::BuiltIn => tr(locale, MessageId::ExtensionsGroupBuiltIn).into_owned(),
        SkillRootKind::ReviewedPluginSnapshot => {
            tr(locale, MessageId::ExtensionsSkillRootReviewedPlugin).into_owned()
        }
        SkillRootKind::RegistryCache => {
            tr(locale, MessageId::ExtensionsSkillRootRegistryCache).into_owned()
        }
    }
}

/// The one action a plugin row offers, wherever that row is drawn.
///
/// The Plugins tab and the marketplace's first-party row both need "what does
/// this plugin want from me right now?", and a second copy of the ladder is
/// how the marketplace ends up offering `Enable` for something already active.
fn plugin_row_action(
    locale: Locale,
    plugin: &codewhale_plugins::types::LoadedPlugin,
) -> ExtensionAction {
    let has_error_diagnostics = plugin.diagnostics.iter().any(|diagnostic| {
        diagnostic.level == codewhale_plugins::types::PluginDiagnosticLevel::Error
    });
    if has_error_diagnostics {
        ExtensionAction::Command {
            label: tr(locale, MessageId::ExtensionsActionDiagnose).into_owned(),
            command: format!("/plugin validate {}", plugin.name()),
            disposition: RowActionDisposition::InPlacePager,
        }
    } else if plugin.active() {
        ExtensionAction::Command {
            label: tr(locale, MessageId::LaunchHintOpen).into_owned(),
            command: format!("/plugin show {}", plugin.name()),
            disposition: RowActionDisposition::InPlacePager,
        }
    } else if plugin.trusted() && !plugin.enabled {
        ExtensionAction::Command {
            label: tr(locale, MessageId::ExtensionsActionEnable).into_owned(),
            command: format!("/plugin enable {}", plugin.name()),
            disposition: RowActionDisposition::InPlace,
        }
    } else {
        // The command opens the exact-content review with its confirmation
        // control stacked on this panel. Confirming the digest runs the
        // trust mutation and the host re-reads the inventory, so the row the
        // person just reviewed reports its new state instead of the stale
        // "not reviewed" it left with.
        ExtensionAction::Command {
            label: tr(locale, MessageId::ExtensionsActionReview).into_owned(),
            command: format!("/plugin trust {}", plugin.name()),
            disposition: RowActionDisposition::InPlace,
        }
    }
}

/// The reversible on/off control for a plugin row — offered on **every**
/// plugin, reviewed or not.
///
/// Withholding the switch from an unreviewed bundle left `computer-use`
/// reading `disabled` with no way to enable it: the panel said what was
/// wrong and then refused the only gesture that could fix it. Nothing about
/// the trust boundary required that. `/plugin enable` already routes an
/// unreviewed bundle into the exact-capability review
/// (`mutate_bundle` in `commands::groups::plugins`) and only flips the
/// switch once the digest is confirmed, so handing `e` to every row widens
/// the affordance without widening what runs unreviewed.
fn plugin_row_toggle(
    locale: Locale,
    plugin: &codewhale_plugins::types::LoadedPlugin,
) -> Option<ExtensionAction> {
    Some(if plugin.enabled {
        ExtensionAction::Command {
            label: tr(locale, MessageId::ExtensionsActionDisable).into_owned(),
            command: format!("/plugin disable {}", plugin.name()),
            disposition: RowActionDisposition::InPlace,
        }
    } else {
        ExtensionAction::Command {
            label: tr(locale, MessageId::ExtensionsActionEnable).into_owned(),
            command: format!("/plugin enable {}", plugin.name()),
            disposition: RowActionDisposition::InPlace,
        }
    })
}

/// How a plugin row reads, using the same ladder as [`plugin_row_action`].
fn plugin_row_tone(plugin: &codewhale_plugins::types::LoadedPlugin) -> ExtensionTone {
    let has_error_diagnostics = plugin.diagnostics.iter().any(|diagnostic| {
        diagnostic.level == codewhale_plugins::types::PluginDiagnosticLevel::Error
    });
    if has_error_diagnostics {
        ExtensionTone::Failure
    } else if plugin.active() {
        ExtensionTone::Ready
    } else if plugin.trusted() {
        // Trusted and deliberately disabled: off, not wrong.
        ExtensionTone::Idle
    } else {
        // Untrusted is not broken either; it is waiting on a person.
        ExtensionTone::Attention
    }
}

fn plugins_model(app: &App, locale: Locale) -> ExtensionsTabModel {
    let mut by_scope = [Vec::new(), Vec::new(), Vec::new()];
    for plugin in app.plugin_registry.list() {
        let scope = match plugin.scope {
            codewhale_plugins::types::PluginScope::Builtin => 0,
            codewhale_plugins::types::PluginScope::User => 1,
            codewhale_plugins::types::PluginScope::Workspace => 2,
        };
        let diagnostic_count = plugin.diagnostics.len();
        let action = plugin_row_action(locale, plugin);
        let toggle = plugin_row_toggle(locale, plugin);
        by_scope[scope].push(ExtensionItem {
            id: plugin.id.as_str().to_string(),
            tone: plugin_row_tone(plugin),
            label: plugin.name().to_string(),
            description: plugin
                .manifest
                .plugin
                .description
                .clone()
                .unwrap_or_else(|| inventory_summary(&plugin.inventory, locale)),
            state: plugin_row_state(locale, plugin),
            // An unreviewed bundle's detail line is the one place with room
            // to say what the review *is*. `trust: not reviewed` restated
            // the state word and taught nobody anything; the sentence says
            // what Codewhale withholds and which key ends the wait.
            detail: if plugin.trusted() {
                localize(
                    locale,
                    MessageId::ExtensionsPluginDetail,
                    &[
                        ("inventory", &inventory_summary(&plugin.inventory, locale)),
                        (
                            "trust",
                            &localized_trust(locale, plugin.trust_status.as_str()),
                        ),
                        (
                            "compatibility",
                            &localized_compatibility(locale, plugin.compatibility().as_str()),
                        ),
                        ("diagnostics", &diagnostic_count.to_string()),
                    ],
                )
            } else {
                format!(
                    "{} · {}",
                    tr(locale, MessageId::ExtensionsReviewExplainer),
                    inventory_summary(&plugin.inventory, locale)
                )
            },
            action: Some(action),
            toggle,
            remove: None,
        });
    }
    let labels = [
        (
            "builtin",
            tr(locale, MessageId::ExtensionsGroupBuiltIn).into_owned(),
        ),
        (
            "user",
            tr(locale, MessageId::ExtensionsGroupUser).into_owned(),
        ),
        (
            "workspace",
            tr(locale, MessageId::ExtensionsGroupWorkspace).into_owned(),
        ),
    ];
    let mut groups = labels
        .into_iter()
        .zip(by_scope)
        .filter(|(_, items)| !items.is_empty())
        .map(|((id, label), items)| ExtensionGroup {
            id: id.into(),
            label,
            items,
        })
        .collect::<Vec<_>>();
    let problems = app
        .plugin_registry
        .diagnostics()
        .iter()
        .enumerate()
        .map(|(index, diagnostic)| ExtensionItem {
            id: format!("plugin-diagnostic-{index}"),
            tone: ExtensionTone::Failure,
            label: diagnostic.code.to_string(),
            description: diagnostic.message.clone(),
            state: if diagnostic.level == codewhale_plugins::types::PluginDiagnosticLevel::Error {
                tr(locale, MessageId::ExtensionsStateInvalid)
            } else {
                tr(locale, MessageId::ExtensionsStateWarning)
            }
            .into_owned(),
            detail: diagnostic
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| diagnostic.message.clone()),
            action: Some(ExtensionAction::Command {
                label: tr(locale, MessageId::ExtensionsActionDiagnose).into_owned(),
                command: "/plugin validate".into(),
                disposition: RowActionDisposition::InPlacePager,
            }),
            toggle: None,
            remove: None,
        })
        .collect::<Vec<_>>();
    if !problems.is_empty() {
        groups.push(ExtensionGroup {
            id: "problems".into(),
            label: tr(locale, MessageId::ExtensionsGroupProblems).into_owned(),
            items: problems,
        });
    }
    ExtensionsTabModel {
        groups,
        problem: app.plugin_registry.state_error().map(ToString::to_string),
    }
}

fn marketplace_model(app: &App, locale: Locale) -> ExtensionsTabModel {
    use codewhale_plugins::marketplace::document::{
        CatalogInstallResolution, resolve_candidate_install,
    };
    let Some(store) = codewhale_plugins::marketplace::store::MarketplaceStore::open(
        app.plugin_registry.state_path(),
    ) else {
        return ExtensionsTabModel {
            groups: Vec::new(),
            problem: Some(tr(locale, MessageId::ExtensionsMarketplaceUnavailable).into_owned()),
        };
    };
    let state = match store.load() {
        Ok(state) => state,
        Err(error) => {
            return ExtensionsTabModel {
                groups: Vec::new(),
                problem: Some(error),
            };
        }
    };
    let groups = state
        .catalogs()
        .values()
        .map(|stored| {
            let catalog = &stored.catalog;
            ExtensionGroup {
                id: catalog.id.as_str().to_string(),
                label: catalog
                    .display_name
                    .clone()
                    .unwrap_or_else(|| catalog.name.clone()),
                items: catalog
                    .candidates
                    .iter()
                    .map(|candidate| {
                        let resolution =
                            resolve_candidate_install(stored, candidate, &app.plugin_registry);
                        if let CatalogInstallResolution::AlreadyPresent { plugin, .. } = &resolution
                        {
                            // A catalog name match is only occupancy. Show and
                            // review the actual local bundle, not catalog claims.
                            return ExtensionItem {
                                id: candidate.id.as_str().to_string(),
                                tone: plugin_row_tone(plugin),
                                label: plugin.name().to_string(),
                                description: plugin
                                    .manifest
                                    .plugin
                                    .description
                                    .clone()
                                    .unwrap_or_default(),
                                state: if plugin.scope
                                    == codewhale_plugins::types::PluginScope::Builtin
                                {
                                    tr(locale, MessageId::ExtensionsStateFirstParty).into_owned()
                                } else {
                                    plugin_row_state(locale, plugin)
                                },
                                detail: plugin.canonical_root.display().to_string(),
                                action: Some(plugin_row_action(locale, plugin)),
                                toggle: None,
                                remove: None,
                            };
                        }
                        let installable =
                            matches!(resolution, CatalogInstallResolution::Supported { .. });
                        ExtensionItem {
                            id: candidate.id.as_str().to_string(),
                            tone: ExtensionTone::Attention,
                            label: candidate
                                .display_name
                                .clone()
                                .unwrap_or_else(|| candidate.name.clone()),
                            description: candidate.description.clone().unwrap_or_default(),
                            state: if candidate.has_errors() {
                                tr(locale, MessageId::ExtensionsStateInvalid)
                            } else if installable {
                                tr(locale, MessageId::ExtensionsStateAvailable)
                            } else {
                                tr(locale, MessageId::AutomationActionInspect)
                            }
                            .into_owned(),
                            detail: {
                                let unknown = tr(locale, MessageId::CmdCostUnknownValue);
                                localize(
                                    locale,
                                    MessageId::ExtensionsMarketplaceDetail,
                                    &[
                                        (
                                            "publisher",
                                            candidate
                                                .provenance
                                                .publisher
                                                .as_deref()
                                                .unwrap_or(unknown.as_ref()),
                                        ),
                                        (
                                            "tier",
                                            &localized_tier(
                                                locale,
                                                candidate.provenance.tier.as_str(),
                                            ),
                                        ),
                                        ("installable", &localized_bool(locale, installable)),
                                    ],
                                )
                            },
                            action: if installable {
                                Some(ExtensionAction::Command {
                                    label: tr(locale, MessageId::ExtensionsActionAdd).into_owned(),
                                    command: format!(
                                        "/plugin marketplace install {} {}",
                                        catalog.id.as_str(),
                                        candidate.name
                                    ),
                                    disposition: RowActionDisposition::InPlace,
                                })
                            } else {
                                Some(ExtensionAction::Status {
                                    label: tr(locale, MessageId::PickerActionUnavailable)
                                        .into_owned(),
                                })
                            },
                            toggle: None,
                            remove: None,
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    ExtensionsTabModel {
        groups,
        problem: None,
    }
}

fn skills_model(app: &App, locale: Locale) -> ExtensionsTabModel {
    use codewhale_skills::audit::{ParserState, SkillAuditMode, scan_with_configured};

    let home = codewhale_paths::user_home();
    let audit = scan_with_configured(
        &app.workspace,
        home.as_deref(),
        Some(&app.skills_dir),
        SkillAuditMode::OwnedOnly,
        None,
    );
    let mut groups = Vec::<ExtensionGroup>::new();
    for skill in audit.skills {
        let group_id = format!("{:?}", skill.root.kind);
        let position = groups.iter().position(|group| group.id == group_id);
        let item = ExtensionItem {
            id: format!("{}:{}", group_id, skill.id.canonical_name),
            tone: ExtensionTone::Ready,
            label: skill.name,
            description: skill.description.unwrap_or_default(),
            state: match skill.parser {
                ParserState::Valid => tr(locale, MessageId::HotbarSetupStatusReady),
                ParserState::Warning(_) => tr(locale, MessageId::ExtensionsStateWarning),
                ParserState::Broken(_) | ParserState::Oversized => {
                    tr(locale, MessageId::ExtensionsStateInvalid)
                }
            }
            .into_owned(),
            detail: skill.safe_display_path,
            // Enter opens the skills manager, which is where install, update,
            // remove and trust already live (`views/skills_manager.rs`, 1,000
            // lines, driving `codewhale_skills::mutation::SkillMutationRequest`). This
            // tab used to dead-end on `action: None` — founder live-test:
            // "skills - no way to delete them or edit or anything either" —
            // even though the manager it needed was one command away. Routing
            // rather than reimplementing: the mutation authority stays in one
            // place.
            action: Some(ExtensionAction::Command {
                label: tr(locale, MessageId::ExtensionsActionManage).into_owned(),
                command: "/skills manage".into(),
                disposition: RowActionDisposition::LeavePanel,
            }),
            toggle: None,
            remove: None,
        };
        if let Some(position) = position {
            groups[position].items.push(item);
        } else {
            groups.push(ExtensionGroup {
                id: group_id.clone(),
                label: localized_skill_root(locale, skill.root.kind),
                items: vec![item],
            });
        }
    }
    ExtensionsTabModel {
        groups,
        problem: None,
    }
}

/// Whether a listed MCP row belongs to the user's own config, and may
/// therefore be removed or toggled from the Extensions panel.
///
/// `owned` is the set of servers in the user's config without plugin
/// contributions; `None` means that config could not be read, in which case
/// ownership is unknown and the gestures are kept rather than silently
/// withdrawn. Plugin-contributed servers are never in that set: their names are
/// synthesized and `/mcp remove` resolves against the config file, so offering
/// the gesture produced a guaranteed "server not found".
fn mcp_row_is_mutable(owned: Option<&BTreeSet<String>>, name: &str) -> bool {
    owned.is_none_or(|owned| owned.contains(name))
}

fn mcp_model(app: &App, locale: Locale) -> ExtensionsTabModel {
    let configured = codewhale_engine::mcp::load_config_with_workspace_and_plugins(
        &app.mcp_config_path,
        &app.workspace,
        app.plugin_registry.as_ref(),
    )
    .ok();
    // The rows above are the union of the user's config and every plugin's
    // contribution. Only the user's own servers can be removed or toggled: a
    // plugin server's name is synthesized (`plugin-{len}-{plugin}-{server}`)
    // and never appears in the config file `/mcp remove` resolves against, so
    // offering the gesture there was a guaranteed 404. Derive the set by
    // loading the same config without plugin contributions and taking the
    // difference, rather than parsing the shape of the synthesized name.
    let user_owned: Option<BTreeSet<String>> =
        codewhale_engine::mcp::load_config_with_workspace(&app.mcp_config_path, &app.workspace)
            .ok()
            .map(|config| config.servers.keys().cloned().collect());
    // Where each row lives. A person asked to "say if they should be global
    // or in a certain project"; the panel could not answer because nothing
    // on the row carried its origin. Resolved once here, not per row.
    let project_owned =
        codewhale_engine::mcp::project_server_names(&app.mcp_config_path, &app.workspace);
    let snapshot = app.mcp_snapshot.as_ref();
    // Configured names are the count authority used by the surrounding shell.
    // Snapshot data enriches those exact rows; it must never independently
    // filter the list down to only the last discovered subset.
    let names = configured.as_ref().map_or_else(
        || {
            snapshot
                .into_iter()
                .flat_map(|snapshot| snapshot.servers.iter().map(|server| server.name.clone()))
                .collect::<BTreeSet<_>>()
        },
        |config| config.servers.keys().cloned().collect::<BTreeSet<_>>(),
    );
    let total = names.len();
    let items: Vec<_> = names
        .into_iter()
        .map(|name| {
            let observed = snapshot
                .and_then(|snapshot| snapshot.servers.iter().find(|server| server.name == name));
            let config = configured
                .as_ref()
                .and_then(|configured| configured.servers.get(&name));
            // The config file just read is the authority for on/off. The
            // snapshot is the live pool as of its last event, and a server
            // switched off (or on) since then used to keep its stale live row
            // — a disabled server offered "reconnect" instead of "enable".
            // When the two disagree, the observation belongs to a config the
            // user already changed, so it is not shown as this row's state.
            let config_enabled = config.map(codewhale_mcp::config::McpServerConfig::is_enabled);
            let stale = observed
                .zip(config_enabled)
                .is_some_and(|(server, enabled)| server.enabled != enabled);
            let observed = observed.filter(|_| !stale);
            let enabled = config_enabled
                .or_else(|| observed.map(|server| server.enabled))
                .unwrap_or(true);
            // A `/mcp retry` the person already asked for: connecting now, or
            // queued behind the running turn.
            let retry = enabled
                .then(|| {
                    app.mcp_retries
                        .iter()
                        .find(|pending| pending.server == name)
                })
                .flatten();
            // `connecting` is the engine's real in-flight set (#6033): under
            // lazy boot a configured-but-unstarted server reads "not connected",
            // never "connecting".
            let initializing = retry.is_some()
                || (enabled
                    && app
                        .mcp_connecting
                        .iter()
                        .any(|connecting| connecting == &name)
                    && observed.is_none_or(|server| !server.connected && server.error.is_none()));
            let state = if !enabled {
                tr(locale, MessageId::HotbarSetupStatusDisabled)
            } else if retry.is_some_and(|pending| pending.queued) {
                tr(locale, MessageId::AutomationRunStatusQueued)
            } else if initializing {
                Cow::Borrowed("connecting")
            } else if observed.is_some_and(|server| server.connected) {
                tr(locale, MessageId::ExtensionsStateConnected)
            } else if observed.is_some_and(|server| server.auth_required) {
                Cow::Owned(crate::tui::session_boot::mcp_auth_required_state_label())
            } else if observed.is_some_and(|server| server.error.is_some()) {
                tr(locale, MessageId::ExtensionsStateError)
            } else if stale {
                // Switched on in the file, still off in the live pool.
                tr(locale, MessageId::McpStateDisconnected)
            } else if observed.is_none() {
                tr(locale, MessageId::ExtensionsStateNotInspected)
            } else if observed.is_some_and(|server| !server.started()) {
                // Nothing in this session has needed it yet. "Not started"
                // read as a failure to someone whose `codewhale mcp tools`
                // had just listed its tools from the shell, which is a
                // separate process with its own connection.
                tr(locale, MessageId::McpStateDisconnected)
            } else {
                tr(locale, MessageId::ExtensionsStateDisconnected)
            }
            .into_owned();
            let not_started = enabled
                && !initializing
                && (stale || observed.is_none_or(|server| !server.started()));
            // The live pool knows this server and has simply not needed it:
            // it has no tool counts to show, only what happens next.
            let waiting_for_first_use = enabled
                && !initializing
                && !stale
                && observed.is_some_and(|server| !server.started());
            let oauth_capable = config.is_some_and(codewhale_engine::mcp::mcp_server_oauth_capable);
            let recovery = match observed {
                Some(server) => server.recovery_kind(oauth_capable),
                None => codewhale_engine::mcp::mcp_recovery_kind(
                    enabled,
                    false,
                    false,
                    None,
                    oauth_capable,
                ),
            };
            let action = match (initializing, recovery) {
                // Still connecting: the state is the whole story.
                (true, _) => ExtensionAction::Status {
                    label: state.clone(),
                },
                // The live pool still holds the pre-edit config, and a
                // single-server retry deliberately never re-reads it; only a
                // reload brings the newly enabled server into the pool.
                (false, Some(_)) if stale && enabled => ExtensionAction::Command {
                    label: tr(locale, MessageId::ExtensionsActionConnect).into_owned(),
                    command: "/mcp reload".into(),
                    disposition: RowActionDisposition::InPlace,
                },
                // Healthy. A row that needs nothing offers nothing — the
                // actionable rows are the ones worth finding in a list of 20.
                (false, None) => ExtensionAction::Status {
                    label: state.clone(),
                },
                (false, Some(recovery))
                    if codewhale_engine::mcp::mcp_name_is_command_safe(&name)
                        || matches!(
                            recovery,
                            codewhale_engine::mcp::McpRecoveryKind::Connect
                                | codewhale_engine::mcp::McpRecoveryKind::Reconnect
                                | codewhale_engine::mcp::McpRecoveryKind::Diagnose
                                | codewhale_engine::mcp::McpRecoveryKind::AwsLogin
                        ) =>
                {
                    ExtensionAction::Command {
                        label: tr(locale, recovery.label_key()).into_owned(),
                        command: recovery.slash_command(&name),
                        // Re-auth hands off to the OAuth login flow; every
                        // other recovery runs against live state the panel
                        // re-reads when it lands.
                        disposition: match recovery {
                            codewhale_engine::mcp::McpRecoveryKind::Reauth => {
                                RowActionDisposition::LeavePanel
                            }
                            _ => RowActionDisposition::InPlace,
                        },
                    }
                }
                (false, Some(_)) => ExtensionAction::Command {
                    label: tr(locale, MessageId::ExtensionsActionDiagnose).into_owned(),
                    command: "/mcp validate".into(),
                    disposition: RowActionDisposition::InPlace,
                },
            };
            let scope = config
                .and_then(|server| server.reviewed_plugin_source())
                .map(|source| {
                    localize(
                        locale,
                        MessageId::ExtensionsScopePlugin,
                        &[("plugin", source.plugin_name())],
                    )
                })
                .unwrap_or_else(|| {
                    tr(
                        locale,
                        if project_owned.contains(&name) {
                            MessageId::ExtensionsScopeProject
                        } else {
                            MessageId::ExtensionsScopeGlobal
                        },
                    )
                    .into_owned()
                });
            let command_safe = codewhale_engine::mcp::mcp_name_is_command_safe(&name);
            let mutable = command_safe && mcp_row_is_mutable(user_owned.as_ref(), &name);
            let toggle = mutable.then(|| {
                if enabled {
                    ExtensionAction::Command {
                        label: tr(locale, MessageId::ExtensionsActionDisable).into_owned(),
                        command: format!("/mcp disable {name}"),
                        disposition: RowActionDisposition::InPlace,
                    }
                } else {
                    ExtensionAction::Command {
                        label: tr(locale, MessageId::ExtensionsActionEnable).into_owned(),
                        command: format!("/mcp enable {name}"),
                        disposition: RowActionDisposition::InPlace,
                    }
                }
            });
            let remove = mutable.then(|| ExtensionAction::Command {
                label: tr(locale, MessageId::ExtensionsActionRemove).into_owned(),
                command: format!("/mcp remove {name}"),
                disposition: RowActionDisposition::InPlace,
            });
            ExtensionItem {
                id: name.clone(),
                tone: match (enabled, initializing, recovery) {
                    (false, ..) => ExtensionTone::Idle,
                    // Lazy boot left it for later: nothing is wrong with it.
                    _ if not_started => ExtensionTone::Idle,
                    (true, true, _) => ExtensionTone::Attention,
                    (true, false, None) => ExtensionTone::Ready,
                    // A server that reports an error is broken; one that only
                    // wants a login or a reconnect is waiting on a person.
                    (true, false, Some(codewhale_engine::mcp::McpRecoveryKind::Diagnose)) => {
                        ExtensionTone::Failure
                    }
                    (true, false, Some(_)) => ExtensionTone::Attention,
                },
                // A plugin's server is shown as `plugin/server`, not as the
                // wire key `plugin-25-<plugin>-<server>`. The length-prefixed
                // form is how the config layer keeps the name unambiguous; it
                // was never meant to be read by a person.
                label: codewhale_engine::mcp::split_qualified_plugin_server_name(&name)
                    .map_or_else(
                        || name.clone(),
                        |(plugin, server)| format!("{plugin}/{server}"),
                    ),
                description: observed.map_or_else(String::new, |server| {
                    if waiting_for_first_use {
                        return server.transport.clone();
                    }
                    localize(
                        locale,
                        MessageId::ExtensionsMcpSummary,
                        &[
                            ("transport", &server.transport),
                            ("tools", &server.tools.len().to_string()),
                            ("resources", &server.resources.len().to_string()),
                        ],
                    )
                }),
                state,
                // The passive snapshot can carry a command line or URL. Do
                // not mirror either into this broad inventory surface.
                detail: format!(
                    "{scope} · {}",
                    observed.map_or_else(
                        || tr(locale, MessageId::ExtensionsMcpNotInspected).into_owned(),
                        |server| {
                            if waiting_for_first_use {
                                return tr(locale, MessageId::ExtensionsMcpNotConnectedYet)
                                    .into_owned();
                            }
                            server.error.clone().unwrap_or_else(|| {
                                localize(
                                    locale,
                                    MessageId::ExtensionsMcpDetail,
                                    &[
                                        ("tools", &server.tools.len().to_string()),
                                        ("resources", &server.resources.len().to_string()),
                                        ("prompts", &server.prompts.len().to_string()),
                                    ],
                                )
                            })
                        },
                    )
                ),
                action: Some(action),
                toggle,
                remove,
            }
        })
        .collect();
    let groups = if items.is_empty() && configured.is_some() {
        vec![ExtensionGroup {
            id: "mcp-start".into(),
            label: tr(locale, MessageId::ExtensionsMcpEmpty).into_owned(),
            items: vec![ExtensionItem {
                id: "mcp-suggestions".into(),
                tone: ExtensionTone::Idle,
                label: tr(locale, MessageId::ExtensionsMcpBrowse).into_owned(),
                description: tr(locale, MessageId::McpRecommendationsHeading).into_owned(),
                state: tr(locale, MessageId::ExtensionsStateAvailable).into_owned(),
                detail: localize(
                    locale,
                    MessageId::McpRecommendationsSafety,
                    &[("restart_command", "/mcp restart")],
                ),
                action: Some(ExtensionAction::Command {
                    label: tr(locale, MessageId::AutomationActionInspect).into_owned(),
                    command: "/mcp recommendations".into(),
                    disposition: RowActionDisposition::InPlacePager,
                }),
                toggle: None,
                remove: None,
            }],
        }]
    } else {
        mcp_groups(locale, items)
    };
    ExtensionsTabModel {
        groups,
        problem: (configured.is_none() && app.mcp_configured_count > total).then(|| {
            localize(
                locale,
                MessageId::ExtensionsMcpRefresh,
                &[("count", &app.mcp_configured_count.to_string())],
            )
        }),
    }
}

/// Group id of the `/mcp` rows whose one action is a login.
const MCP_LOGIN_GROUP_ID: &str = "login";

/// Whether a row's one action is the login flow.
fn mcp_item_needs_login(item: &ExtensionItem) -> bool {
    item.action
        .as_ref()
        .and_then(ExtensionAction::command)
        .is_some_and(|command| command.starts_with("/mcp login "))
}

/// Order the `/mcp` rows by what a person has to do about them. Everything
/// that needs a human leads: with twenty servers configured, the four that
/// failed or want re-auth were impossible to pick out of a flat alphabetical
/// list — founder live-test on the same screen. Within that, the servers
/// that only need a login come first, as their own group, because "failed"
/// is the wrong word for an expired login and the fix is one key (#5926):
/// Enter on the row runs `/mcp login <server>`. Real failures follow with
/// their reason in the detail line; a healthy server renders its state and
/// sorts below.
fn mcp_groups(locale: Locale, items: Vec<ExtensionItem>) -> Vec<ExtensionGroup> {
    let (login, rest): (Vec<_>, Vec<_>) = items.into_iter().partition(mcp_item_needs_login);
    // "Needs attention" is for rows that are broken or waiting on a person.
    // A switched-off server or one lazy boot has not started yet keeps its
    // `enable` / `connect` action but sorts with the rest: listing eight
    // servers as needing attention when six were simply idle buried the two
    // that were actually failing.
    let (attention, healthy): (Vec<_>, Vec<_>) = rest.into_iter().partition(|item| {
        matches!(item.tone, ExtensionTone::Attention | ExtensionTone::Failure)
            && item.action.as_ref().is_some_and(|a| a.command().is_some())
    });
    [
        (
            MCP_LOGIN_GROUP_ID,
            MessageId::ExtensionsGroupNeedsLogin,
            login,
        ),
        (
            "attention",
            MessageId::ExtensionsGroupNeedsAttention,
            attention,
        ),
        ("servers", MessageId::ExtensionsGroupServers, healthy),
    ]
    .into_iter()
    .filter(|(_, _, items)| !items.is_empty())
    .map(|(id, label, items)| ExtensionGroup {
        id: id.into(),
        label: tr(locale, label).into_owned(),
        items,
    })
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtensionsFocus {
    Tabs,
    Search,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisibleEntry<'a> {
    Group(&'a ExtensionGroup),
    Item(&'a ExtensionGroup, &'a ExtensionItem),
    Problem(&'a str),
    Empty,
}

/// Stable identity of a visible row across snapshot refreshes. An item is
/// keyed by its own id alone: a refresh may regroup it (MCP login-first).
#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKey {
    Group(String),
    Item(String),
    Problem,
}

impl EntryKey {
    fn of(entry: VisibleEntry<'_>) -> Option<Self> {
        match entry {
            VisibleEntry::Group(group) => Some(Self::Group(group.id.clone())),
            VisibleEntry::Item(_, item) => Some(Self::Item(item.id.clone())),
            VisibleEntry::Problem(_) => Some(Self::Problem),
            VisibleEntry::Empty => None,
        }
    }
}

#[derive(Default)]
struct HitAreas {
    tabs: Vec<(Rect, ExtensionsTab)>,
    search: Option<Rect>,
    rows: Vec<(Rect, usize)>,
}

pub struct ExtensionsView {
    snapshot: ExtensionsSnapshot,
    locale: Locale,
    active_tab: ExtensionsTab,
    focus: ExtensionsFocus,
    query: String,
    selected: [usize; 5],
    scroll: [usize; 5],
    folded_groups: BTreeSet<String>,
    /// The live theme, captured at open so row ink resolves through the same
    /// grammar the rest of the chrome uses instead of raw palette constants.
    theme: codewhale_palette::UiTheme,
    hits: RefCell<HitAreas>,
    hovered_row: Option<usize>,
    hovered_tab: Option<ExtensionsTab>,
    /// Last time `tick` asked the host for a fresh snapshot. Bounds the poll
    /// so a per-frame tick cannot turn into a rebuild every frame.
    last_poll: std::time::Instant,
    /// Row id whose removal is armed. A second `d` / Delete on the same row
    /// confirms; any navigation or Esc disarms.
    pending_remove: Option<String>,
}

impl ExtensionsView {
    #[must_use]
    pub fn new(app: &App, tab: ExtensionsTab) -> Self {
        let mut view =
            Self::from_snapshot_with_locale(ExtensionsSnapshot::from_app(app), tab, app.ui_locale);
        view.theme = app.ui_theme;
        view
    }

    fn from_snapshot_with_locale(
        snapshot: ExtensionsSnapshot,
        tab: ExtensionsTab,
        locale: Locale,
    ) -> Self {
        let mut view = Self {
            snapshot,
            locale,
            active_tab: tab,
            focus: ExtensionsFocus::List,
            query: String::new(),
            selected: [0; 5],
            scroll: [0; 5],
            folded_groups: BTreeSet::new(),
            theme: codewhale_palette::UI_THEME,
            hits: RefCell::new(HitAreas::default()),
            hovered_row: None,
            hovered_tab: None,
            last_poll: std::time::Instant::now(),
            pending_remove: None,
        };
        // Each tab lands on a real item, preserving login-first MCP sorting.
        // Group headings remain reachable for folding with Up.
        for initial_tab in ExtensionsTab::ALL {
            view.active_tab = initial_tab;
            if let Some(index) = view
                .visible_entries()
                .iter()
                .position(|entry| matches!(entry, VisibleEntry::Item(_, _)))
            {
                view.selected[initial_tab.index()] = index;
            }
        }
        view.active_tab = tab;
        view
    }

    fn fold_key(&self, group: &ExtensionGroup) -> String {
        format!("{}:{}", self.active_tab.index(), group.id)
    }

    fn group_matches(&self, group: &ExtensionGroup, query: &str) -> bool {
        group.label.to_lowercase().contains(query)
            || group.items.iter().any(|item| item_matches(item, query))
    }

    fn visible_entries(&self) -> Vec<VisibleEntry<'_>> {
        let model = self.snapshot.tab(self.active_tab);
        let query = self.query.trim().to_lowercase();
        let searching = !query.is_empty();
        let mut entries = Vec::new();
        if let Some(problem) = model.problem.as_deref() {
            entries.push(VisibleEntry::Problem(problem));
        }
        for group in &model.groups {
            if searching && !self.group_matches(group, &query) {
                continue;
            }
            entries.push(VisibleEntry::Group(group));
            let folded = !searching && self.folded_groups.contains(&self.fold_key(group));
            if folded {
                continue;
            }
            let group_name_matches = searching && group.label.to_lowercase().contains(&query);
            entries.extend(
                group
                    .items
                    .iter()
                    .filter(|item| !searching || group_name_matches || item_matches(item, &query))
                    .map(|item| VisibleEntry::Item(group, item)),
            );
        }
        if entries.is_empty() {
            entries.push(VisibleEntry::Empty);
        }
        entries
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_entries().len();
        let index = self.active_tab.index();
        self.selected[index] = self.selected[index].min(len.saturating_sub(1));
        self.scroll[index] = self.scroll[index].min(self.selected[index]);
    }

    fn move_selection(&mut self, delta: isize) {
        self.pending_remove = None;
        let len = self.visible_entries().len();
        if len == 0 {
            return;
        }
        let index = self.active_tab.index();
        self.selected[index] =
            (self.selected[index] as isize + delta).rem_euclid(len as isize) as usize;
    }

    fn selected_item(&self) -> Option<&ExtensionItem> {
        let selected = self.selected[self.active_tab.index()];
        match self.visible_entries().get(selected).copied() {
            Some(VisibleEntry::Item(_, item)) => Some(item),
            _ => None,
        }
    }

    /// `e`: run the row's reversible on/off command in place.
    fn toggle_selected(&mut self) -> ViewAction {
        self.pending_remove = None;
        match self.selected_item().and_then(|item| item.toggle.as_ref()) {
            Some(ExtensionAction::Command { command, .. }) => {
                ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                    command: command.clone(),
                    pager_title: None,
                })
            }
            _ => ViewAction::None,
        }
    }

    /// `d` / Delete: arm removal on the first gesture, run the
    /// row's remove command on the second. Rows without a remove command
    /// ignore the gesture.
    fn remove_selected(&mut self) -> ViewAction {
        let Some((id, command)) = self.selected_item().and_then(|item| match &item.remove {
            Some(ExtensionAction::Command { command, .. }) => {
                Some((item.id.clone(), command.clone()))
            }
            _ => None,
        }) else {
            self.pending_remove = None;
            return ViewAction::None;
        };
        if self.pending_remove.as_deref() == Some(id.as_str()) {
            self.pending_remove = None;
            return ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                command,
                pager_title: None,
            });
        }
        self.pending_remove = Some(id);
        ViewAction::None
    }

    /// The selected row's context menu: its own action, details, its on/off
    /// switch and — last, behind an in-menu confirm — its removal. Only
    /// what the row actually offers is listed.
    fn row_menu(&self, column: u16, row: u16) -> Option<ViewEvent> {
        use crate::tui::context_menu::ContextMenuEntry;
        use crate::tui::views::{ContextMenuAction, ExtensionMenuVerb};

        let item = self.selected_item()?;
        let entry = |label: String, verb: ExtensionMenuVerb| {
            ContextMenuEntry::new(
                label,
                String::new(),
                ContextMenuAction::Extension {
                    item_id: item.id.clone(),
                    verb,
                },
            )
        };
        let details = tr(self.locale, MessageId::CtxMenuOpenDetails).into_owned();
        let mut entries = Vec::new();
        match &item.action {
            Some(ExtensionAction::Command { label, .. }) => {
                entries.push(entry(sentence_case(label), ExtensionMenuVerb::Activate).primary());
                entries.push(entry(details, ExtensionMenuVerb::Details));
            }
            _ => entries.push(entry(details, ExtensionMenuVerb::Details).primary()),
        }
        if let Some(ExtensionAction::Command { label, .. }) = &item.toggle {
            entries.push(entry(sentence_case(label), ExtensionMenuVerb::Toggle));
        }
        if let Some(ExtensionAction::Command { label, .. }) = &item.remove {
            entries.push(
                entry(
                    format!("{}…", sentence_case(label)),
                    ExtensionMenuVerb::Remove,
                )
                .confirm(tr(self.locale, MessageId::CtxMenuConfirmArmed))
                .section_start(),
            );
        }
        Some(ViewEvent::OpenContextMenu {
            title: item.label.clone(),
            entries,
            column,
            row,
        })
    }

    /// Run a row-menu verb on the row with `item_id`. The menu closed before
    /// this runs and a poll may have rebuilt the list meanwhile, so the row
    /// is found again by id rather than by index. `None` when it is gone.
    pub(crate) fn run_menu_verb(
        &mut self,
        item_id: &str,
        verb: crate::tui::views::ExtensionMenuVerb,
    ) -> Option<ViewAction> {
        use crate::tui::views::ExtensionMenuVerb;

        let index = self
            .visible_entries()
            .iter()
            .position(|entry| matches!(entry, VisibleEntry::Item(_, item) if item.id == item_id))?;
        self.selected[self.active_tab.index()] = index;
        self.pending_remove = None;
        Some(match verb {
            ExtensionMenuVerb::Activate => self.activate_selected(),
            ExtensionMenuVerb::Toggle => self.toggle_selected(),
            ExtensionMenuVerb::Details => {
                let item = self.selected_item()?;
                ViewAction::Emit(ViewEvent::OpenTextPager {
                    title: item.label.clone(),
                    content: format!("{}\n\n{}\n\n{}", item.state, item.description, item.detail),
                })
            }
            // Confirmed in the menu; the command's own result is the receipt.
            ExtensionMenuVerb::Remove => match &self.selected_item()?.remove {
                Some(ExtensionAction::Command { command, .. }) => {
                    ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                        command: command.clone(),
                        pager_title: None,
                    })
                }
                _ => ViewAction::None,
            },
        })
    }

    fn activate_selected(&mut self) -> ViewAction {
        let selected = self.selected[self.active_tab.index()];
        match self.visible_entries().get(selected).copied() {
            Some(VisibleEntry::Group(group)) => {
                let group = group.clone();
                let key = self.fold_key(&group);
                if !self.folded_groups.remove(&key) {
                    self.folded_groups.insert(key);
                }
                self.clamp_selection();
                ViewAction::None
            }
            Some(VisibleEntry::Item(_, item)) => match item.action.as_ref() {
                Some(ExtensionAction::Command {
                    command,
                    disposition,
                    ..
                }) => match disposition {
                    RowActionDisposition::LeavePanel => {
                        ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                            action: CommandPaletteAction::ExecuteCommand {
                                command: command.clone(),
                            },
                        })
                    }
                    RowActionDisposition::InPlace => {
                        ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                            command: command.clone(),
                            pager_title: None,
                        })
                    }
                    RowActionDisposition::InPlacePager => {
                        ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                            command: command.clone(),
                            pager_title: Some(item.label.clone()),
                        })
                    }
                },
                _ => ViewAction::Emit(ViewEvent::OpenTextPager {
                    title: item.label.clone(),
                    content: format!("{}\n\n{}\n\n{}", item.state, item.description, item.detail),
                }),
            },
            Some(VisibleEntry::Problem(problem)) => ViewAction::Emit(ViewEvent::OpenTextPager {
                title: self.active_tab.label(self.locale),
                content: problem.to_string(),
            }),
            _ => ViewAction::None,
        }
    }

    /// Swap in a fresh read model while keeping everything the user is doing:
    /// active tab, focus, search query, selection, scroll, and folded groups
    /// all survive. The selection follows its entity, not its row number: a
    /// refresh that reorders the list (a server logs in, a plugin is
    /// installed) keeps the same item selected, so a later `e`/`d` can never
    /// act on whatever row slid into the old index. It only moves when the
    /// selected entity is gone.
    pub fn refresh_snapshot(&mut self, snapshot: ExtensionsSnapshot) {
        let active = self.active_tab;
        let anchors = ExtensionsTab::ALL.map(|tab| {
            self.active_tab = tab;
            self.visible_entries()
                .get(self.selected[tab.index()])
                .copied()
                .and_then(EntryKey::of)
        });
        self.snapshot = snapshot;
        for (tab, anchor) in ExtensionsTab::ALL.into_iter().zip(anchors) {
            self.active_tab = tab;
            if let Some(index) = anchor.and_then(|anchor| {
                self.visible_entries()
                    .iter()
                    .position(|entry| EntryKey::of(*entry).as_ref() == Some(&anchor))
            }) {
                self.selected[tab.index()] = index;
            }
            self.clamp_selection();
        }
        self.active_tab = active;
    }

    fn set_tab(&mut self, tab: ExtensionsTab) {
        self.hovered_row = None;
        self.hovered_tab = None;
        self.pending_remove = None;
        self.active_tab = tab;
        self.clamp_selection();
    }

    fn selected_status(&self) -> String {
        if let Some(item) = self.selected_item()
            && self.pending_remove.as_deref() == Some(item.id.as_str())
        {
            return localize(
                self.locale,
                MessageId::ExtensionsRemoveArmed,
                &[("name", &item.label)],
            );
        }
        let index = self.selected[self.active_tab.index()];
        match self.visible_entries().get(index).copied() {
            Some(VisibleEntry::Group(group)) => localize(
                self.locale,
                MessageId::ExtensionsGroupStatus,
                &[("count", &group.items.len().to_string())],
            ),
            Some(VisibleEntry::Item(_, item)) => {
                let action = item
                    .action
                    .as_ref()
                    .map(|action| format!(" · {}", action.label()))
                    .unwrap_or_default();
                format!("{} · {}{action} · {}", item.label, item.state, item.detail)
            }
            Some(VisibleEntry::Problem(problem)) => problem.to_string(),
            Some(VisibleEntry::Empty) | None => self.empty_message(),
        }
    }

    fn empty_message(&self) -> String {
        if self.query.is_empty() {
            tr(self.locale, MessageId::ExtensionsNoItems).into_owned()
        } else {
            localize(
                self.locale,
                MessageId::ExtensionsNoMatches,
                &[("query", &self.query)],
            )
        }
    }
}

fn item_matches(item: &ExtensionItem, query: &str) -> bool {
    item.label.to_lowercase().contains(query)
        || item.description.to_lowercase().contains(query)
        || item.state.to_lowercase().contains(query)
        || item.detail.to_lowercase().contains(query)
        || item
            .action
            .as_ref()
            .is_some_and(|action| action.label().to_lowercase().contains(query))
}

impl ModalView for ExtensionsView {
    fn kind(&self) -> ModalKind {
        ModalKind::Extensions
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        self.hovered_row = None;
        self.hovered_tab = None;
        // One navigation grammar (grokbuild, the stated authority): Tab and
        // Shift+Tab / BackTab move across the tab bar, always — even during
        // a search, which keeps its query on the new tab. `/` searches, Esc
        // backs out, ↑↓ move, Enter acts. Tab never cycles focus.
        if key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            self.set_tab(self.active_tab.previous());
            return ViewAction::None;
        }
        if key.code == KeyCode::Tab {
            self.set_tab(self.active_tab.next());
            return ViewAction::None;
        }
        if self.focus == ExtensionsFocus::Search {
            match key.code {
                KeyCode::Esc => {
                    if self.query.is_empty() {
                        self.focus = ExtensionsFocus::List;
                    } else {
                        self.query.clear();
                        self.clamp_selection();
                    }
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.clamp_selection();
                }
                KeyCode::Enter | KeyCode::Down => self.focus = ExtensionsFocus::List,
                KeyCode::Char(ch)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.query.push(ch);
                    self.clamp_selection();
                }
                _ => {}
            }
            return ViewAction::None;
        }
        if self.pending_remove.is_some()
            && !matches!(
                key.code,
                KeyCode::Char('d') | KeyCode::Delete | KeyCode::Enter | KeyCode::Char('y')
            )
        {
            // Anything but the confirming key disarms a pending removal.
            self.pending_remove = None;
            if key.code == KeyCode::Esc {
                return ViewAction::None;
            }
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Char('/') => {
                self.focus = ExtensionsFocus::Search;
                ViewAction::None
            }
            KeyCode::Char('e') => {
                self.focus = ExtensionsFocus::List;
                self.toggle_selected()
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                self.focus = ExtensionsFocus::List;
                self.remove_selected()
            }
            KeyCode::Char('y') | KeyCode::Enter if self.pending_remove.is_some() => {
                self.remove_selected()
            }
            // Left/Right and `[`/`]` are the same move for hands that reach
            // for them; the advertised chord is Tab.
            KeyCode::Left | KeyCode::Char('[') | KeyCode::Char('h') => {
                self.set_tab(self.active_tab.previous());
                ViewAction::None
            }
            KeyCode::Right | KeyCode::Char(']') | KeyCode::Char('l') => {
                self.set_tab(self.active_tab.next());
                ViewAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus = ExtensionsFocus::List;
                self.move_selection(-1);
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus = ExtensionsFocus::List;
                self.move_selection(1);
                ViewAction::None
            }
            // Space is the switch. Both references this panel follows bind
            // it that way — grokbuild's `space toggle` and Codex's
            // `space enable/disable` — and it is the gesture a person
            // reaches for on a list of things that are on or off. `e` stays
            // as an alias. A row with no switch (a group heading, a
            // marketplace candidate) keeps Space's old meaning rather than
            // swallowing the key.
            KeyCode::Char(' ') => {
                self.focus = ExtensionsFocus::List;
                match self.toggle_selected() {
                    ViewAction::None => self.activate_selected(),
                    action => action,
                }
            }
            KeyCode::Enter => {
                self.focus = ExtensionsFocus::List;
                self.activate_selected()
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::Moved => {
                let hits = self.hits.borrow();
                let point = (mouse.column, mouse.row).into();
                self.hovered_row = hits
                    .rows
                    .iter()
                    .find_map(|(rect, row)| rect.contains(point).then_some(*row));
                self.hovered_tab = hits
                    .tabs
                    .iter()
                    .find_map(|(rect, tab)| rect.contains(point).then_some(*tab));
                return ViewAction::None;
            }
            // The wheel moves this list, not the transcript behind it.
            MouseEventKind::ScrollUp => {
                self.pending_remove = None;
                self.focus = ExtensionsFocus::List;
                self.move_selection(-1);
                return ViewAction::None;
            }
            MouseEventKind::ScrollDown => {
                self.pending_remove = None;
                self.focus = ExtensionsFocus::List;
                self.move_selection(1);
                return ViewAction::None;
            }
            // Right-click on a row selects it and opens that row's menu. It
            // used to arm (then run) the row's removal, so two right-clicks
            // deleted an extension with no menu ever shown.
            MouseEventKind::Down(MouseButton::Right) => {
                self.pending_remove = None;
                let row = self
                    .hits
                    .borrow()
                    .rows
                    .iter()
                    .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
                    .map(|(_, row)| *row);
                let Some(row) = row else {
                    return ViewAction::None;
                };
                self.focus = ExtensionsFocus::List;
                self.selected[self.active_tab.index()] = row;
                return self
                    .row_menu(mouse.column, mouse.row)
                    .map_or(ViewAction::None, ViewAction::Emit);
            }
            MouseEventKind::Down(MouseButton::Left) => {}
            _ => return ViewAction::None,
        }
        let hits = self.hits.borrow();
        if let Some((_, tab)) = hits
            .tabs
            .iter()
            .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
            .copied()
        {
            drop(hits);
            self.focus = ExtensionsFocus::Tabs;
            self.set_tab(tab);
            return ViewAction::None;
        }
        if hits
            .search
            .is_some_and(|rect| rect.contains((mouse.column, mouse.row).into()))
        {
            drop(hits);
            self.focus = ExtensionsFocus::Search;
            return ViewAction::None;
        }
        if let Some((_, row)) = hits
            .rows
            .iter()
            .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
            .copied()
        {
            drop(hits);
            self.focus = ExtensionsFocus::List;
            self.selected[self.active_tab.index()] = row;
            return self.activate_selected();
        }
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        *self.hits.borrow_mut() = HitAreas::default();
        let body = render_underwater_surface(
            area,
            buf,
            tr(self.locale, MessageId::ExtensionsTitle).into_owned(),
        );
        if body.width == 0 || body.height < 5 {
            return;
        }
        let spacious = body.height >= 16;
        let status = if spacious && self.pending_remove.is_none() {
            self.selected_item().map_or_else(
                || self.selected_status(),
                |item| {
                    format!(
                        "{} · {}\n{}\n{}",
                        item.label, item.state, item.description, item.detail
                    )
                },
            )
        } else {
            self.selected_status()
        };
        let status = Paragraph::new(status)
            .style(Style::default().fg(if self.pending_remove.is_some() {
                palette::STATUS_WARNING
            } else {
                palette::TEXT_MUTED
            }))
            .wrap(Wrap { trim: false });
        let status_height = if spacious {
            u16::try_from(status.line_count(body.width))
                .unwrap_or(u16::MAX)
                .clamp(3, (body.height / 3).max(3))
        } else {
            1
        };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(status_height),
                Constraint::Length(1),
            ])
            .split(body);

        let mut hits = HitAreas::default();
        let mut x = rows[0].x;
        let available = rows[0].right();
        for tab in ExtensionsTab::ALL {
            let label = if body.width < 58 && tab == ExtensionsTab::Marketplace {
                tr(self.locale, MessageId::ExtensionsTabMarketplaceCompact).into_owned()
            } else {
                tab.label(self.locale)
            };
            // Display cells, not chars: a CJK label is two cells per char,
            // and the painted tab and its hit rect share this one width.
            let label_cells =
                u16::try_from(UnicodeWidthStr::width(label.as_str())).unwrap_or(u16::MAX);
            let width = label_cells
                .saturating_add(2)
                .min(available.saturating_sub(x));
            if width == 0 {
                break;
            }
            let tab_area = Rect::new(x, rows[0].y, width, 1);
            let active = tab == self.active_tab;
            let focused = active && self.focus == ExtensionsFocus::Tabs;
            let style = if focused {
                menu_style::selected_row_style()
            } else if self.hovered_tab == Some(tab) {
                menu_style::hovered_row_style().fg(palette::TEXT_PRIMARY)
            } else if active {
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(palette::TEXT_MUTED)
            };
            Paragraph::new(Line::from(Span::styled(format!(" {label} "), style)))
                .render(tab_area, buf);
            hits.tabs.push((tab_area, tab));
            x = x.saturating_add(width);
        }

        let search_style = if self.focus == ExtensionsFocus::Search {
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::TEXT_MUTED)
        };
        let cursor = if self.focus == ExtensionsFocus::Search {
            "_"
        } else {
            ""
        };
        Paragraph::new(Line::from(vec![
            Span::styled(
                tr(self.locale, MessageId::ExtensionsSearchLabel),
                search_style,
            ),
            Span::styled(
                format!("{}{cursor}", self.query),
                Style::default().fg(palette::TEXT_PRIMARY),
            ),
        ]))
        .render(rows[1], buf);
        hits.search = Some(rows[1]);

        let entries = self.visible_entries();
        let list_height = usize::from(rows[2].height);
        let mut scroll = self.scroll[self.active_tab.index()];
        let selected = self.selected[self.active_tab.index()];
        if selected < scroll {
            scroll = selected;
        } else if selected >= scroll.saturating_add(list_height.max(1)) {
            scroll = selected.saturating_sub(list_height.saturating_sub(1));
        }
        for (visible_offset, (entry_index, entry)) in entries
            .iter()
            .enumerate()
            .skip(scroll)
            .take(list_height)
            .enumerate()
        {
            let row_area = Rect::new(
                rows[2].x,
                rows[2].y.saturating_add(visible_offset as u16),
                rows[2].width,
                1,
            );
            let is_selected = entry_index == selected;
            let hovered = self.hovered_row == Some(entry_index);
            let style = if is_selected && self.focus == ExtensionsFocus::List {
                menu_style::selected_row_style()
            } else if hovered {
                menu_style::hovered_row_style().fg(palette::TEXT_PRIMARY)
            } else if is_selected {
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            // Rows are built as (text, optional ink) pairs. The ink is what
            // stops every row on the screen from reading the same: the action
            // chip is an invitation, the state is a verdict, the description
            // is background. A selected row keeps one style — a highlight the
            // eye can follow beats four colours fighting a fill.
            let mut parts: Vec<(String, Option<codewhale_palette::ChromeInk>)> = Vec::new();
            match entry {
                VisibleEntry::Group(group) => {
                    let folded = self.folded_groups.contains(&self.fold_key(group));
                    parts.push((
                        format!(
                            "{} {} ({})",
                            if folded { "▸" } else { "▾" },
                            group.label,
                            group.items.len()
                        ),
                        None,
                    ));
                }
                VisibleEntry::Item(_, item) => {
                    parts.push(("  ".into(), None));
                    if let Some(action) = item.action.as_ref() {
                        parts.push((
                            format!("{} · ", action.label()),
                            Some(match action {
                                ExtensionAction::Command { .. } => {
                                    codewhale_palette::ChromeInk::Identity
                                }
                                ExtensionAction::Status { .. } => item.tone.ink(),
                            }),
                        ));
                    }
                    parts.push((item.label.clone(), None));
                    parts.push((format!(" · {}", item.state), Some(item.tone.ink())));
                }
                VisibleEntry::Problem(problem) => parts.push((
                    format!("! {problem}"),
                    Some(codewhale_palette::ChromeInk::Failure),
                )),
                VisibleEntry::Empty => parts.push((
                    self.empty_message(),
                    Some(codewhale_palette::ChromeInk::MetadataHint),
                )),
            }

            // Truncate across the whole row, not per span, so the width bound
            // is the one the flat row always had.
            let joined = parts
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<String>();
            let clipped = truncate_view_text(&joined, usize::from(row_area.width));
            let spans = if is_selected || hovered || clipped.len() != joined.len() {
                vec![Span::styled(clipped, style)]
            } else {
                parts
                    .into_iter()
                    .filter(|(text, _)| !text.is_empty())
                    .map(|(text, ink)| {
                        let span_style = match ink {
                            Some(ink) => Style::default().fg(ink.color(&self.theme)),
                            None => style,
                        };
                        Span::styled(text, span_style)
                    })
                    .collect()
            };
            Paragraph::new(Line::from(spans))
                .style(style)
                .render(row_area, buf);
            hits.rows.push((row_area, entry_index));
        }

        status.render(rows[3], buf);
        let mut compact_hints = vec![
            super::ActionHint::new("Tab", tr(self.locale, MessageId::ExtensionsActionTabs)),
            super::ActionHint::new("/", tr(self.locale, MessageId::SessionsActionSearch)),
            super::ActionHint::new("Esc", tr(self.locale, MessageId::SessionsActionClose)),
        ];
        let enter_label = match entries.get(selected).copied() {
            Some(VisibleEntry::Item(_, item)) => Some(
                item.action
                    .as_ref()
                    .filter(|action| action.command().is_some())
                    .map_or_else(
                        || tr(self.locale, MessageId::AutomationActionInspect).into_owned(),
                        |action| action.label().to_string(),
                    ),
            ),
            Some(VisibleEntry::Group(_)) => {
                Some(tr(self.locale, MessageId::ExtensionsActionFold).into_owned())
            }
            Some(VisibleEntry::Problem(_)) => {
                Some(tr(self.locale, MessageId::AutomationActionInspect).into_owned())
            }
            _ => None,
        };
        if let Some(label) = enter_label.as_ref() {
            compact_hints.insert(1, super::ActionHint::new("Enter", label.clone()));
        }
        let mut full_hints = vec![
            super::ActionHint::new("Tab", tr(self.locale, MessageId::ExtensionsActionTabs)),
            super::ActionHint::new("↑↓", tr(self.locale, MessageId::LaunchHintMove)),
        ];
        if let Some(label) = enter_label {
            full_hints.push(super::ActionHint::new("Enter", label));
        }
        if let Some(item) = self.selected_item() {
            if let Some(toggle) = item.toggle.as_ref() {
                full_hints.push(super::ActionHint::new("Space", toggle.label().to_string()));
            }
            if let Some(remove) = item.remove.as_ref() {
                full_hints.push(super::ActionHint::new("d", remove.label().to_string()));
            }
        }
        full_hints.push(super::ActionHint::new(
            "/",
            tr(self.locale, MessageId::SessionsActionSearch),
        ));
        full_hints.push(super::ActionHint::new(
            "Esc",
            tr(self.locale, MessageId::SessionsActionClose),
        ));
        render_modal_footer(
            rows[4],
            buf,
            if rows[4].width < 64 {
                &compact_hints
            } else {
                &full_hints
            },
        );
        *self.hits.borrow_mut() = hits;
    }

    fn tick(&mut self) -> ViewAction {
        // MCP rows go live while the panel is open — a retry lands, a login
        // finishes, a diagnosis resolves — and the open-time capture would
        // read stale until reopen. Ask the host for a fresh model at a
        // bounded cadence; it rebuilds only when the generation or the
        // initializing flag actually moved.
        if self.last_poll.elapsed() < std::time::Duration::from_millis(750) {
            return ViewAction::None;
        }
        self.last_poll = std::time::Instant::now();
        ViewAction::Emit(ViewEvent::RefreshExtensions {
            mcp_generation: self.snapshot.mcp_generation,
            mcp_initializing: self.snapshot.mcp_initializing,
        })
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Row action labels are lower-case verbs ("remove", "enable") written for
/// the footer; a menu row starts with a capital.
fn sentence_case(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_engine::mcp::McpRecoveryKind;

    #[test]
    fn passive_rows_open_details_without_recovery_or_mutation() {
        let mut view = view_on_item(ExtensionAction::Status {
            label: "connected".into(),
        });
        let ViewAction::Emit(ViewEvent::OpenTextPager { title, content }) =
            view.activate_selected()
        else {
            panic!("a passive inventory row must have useful details");
        };
        assert_eq!(title, "row");
        assert!(content.contains("state"));
        assert!(view.pending_remove.is_none());
    }

    fn observed_row(
        name: &str,
        enabled: bool,
        connected: bool,
        error: Option<&str>,
        auth_required: bool,
    ) -> codewhale_engine::mcp::McpServerSnapshot {
        codewhale_engine::mcp::McpServerSnapshot {
            name: name.into(),
            enabled,
            required: false,
            transport: "stdio".into(),
            command_or_url: format!("{name}-mcp"),
            connect_timeout: 5,
            execute_timeout: 5,
            read_timeout: 5,
            connected,
            error: error.map(str::to_string),
            auth_required,
            capability_metadata: if connected {
                codewhale_engine::mcp::McpServerCapabilityMetadata::LegacyFallback
            } else {
                codewhale_engine::mcp::McpServerCapabilityMetadata::NotObserved
            },
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
        }
    }

    /// The founder's Extensions > MCP screen listed eight servers under
    /// "Needs attention", six of them "reconnect · configured". Each row now
    /// reads the state the engine's pool actually has, offers the action
    /// that state needs, and only broken or waiting rows need attention.
    #[test]
    fn mcp_rows_show_real_state_with_the_action_that_state_needs() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path());
        let mut app = App::new_with_plugin_registry(
            crate::tui::test_support::test_tui_options(root.path()),
            &codewhale_runtime::config::Config::default(),
            registry,
        );
        app.mcp_config_path = root.path().join("mcp.json");
        std::fs::write(
            &app.mcp_config_path,
            r#"{"servers":{
                "github":{"command":"github-mcp"},
                "playwright":{"command":"npx"},
                "stripe":{"url":"https://mcp.stripe.com"},
                "aws":{"command":"uvx"},
                "linear":{"url":"https://mcp.linear.app/mcp","disabled":true,"enabled":false},
                "chrome-devtools":{"command":"npx"}
            }}"#,
        )
        .unwrap();
        app.mcp_snapshot = Some(codewhale_engine::mcp::McpManagerSnapshot {
            config_path: app.mcp_config_path.clone(),
            config_exists: true,
            reload_required: false,
            servers: vec![
                observed_row("github", true, true, None, false),
                // Lazy boot never started it.
                observed_row("playwright", true, false, None, false),
                observed_row("stripe", true, false, Some("HTTP 401 Unauthorized"), true),
                observed_row(
                    "aws",
                    true,
                    false,
                    Some("MCP server 'aws' rejected initialize (command `uvx`): -32602"),
                    false,
                ),
                // Disabled in the file since this snapshot was taken.
                observed_row("linear", true, false, None, false),
                observed_row("chrome-devtools", true, false, None, false),
            ],
        });
        app.mcp_retries.push(crate::tui::app::PendingMcpRetry {
            server: "chrome-devtools".into(),
            queued: true,
            result: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });

        let model = mcp_model(&app, Locale::En);
        let row = |name: &str| {
            model
                .groups
                .iter()
                .flat_map(|group| {
                    group
                        .items
                        .iter()
                        .map(move |item| (group.id.as_str(), item))
                })
                .find(|(_, item)| item.id == name)
                .unwrap_or_else(|| panic!("row {name}"))
        };
        let command = |item: &ExtensionItem| {
            item.action
                .as_ref()
                .and_then(ExtensionAction::command)
                .map(str::to_string)
        };

        let (group, github) = row("github");
        assert_eq!((group, github.state.as_str()), ("servers", "connected"));
        assert_eq!(command(github), None);

        // Lazy boot has not needed it: the row says so and says what happens
        // next, not "not started" over a row of zero counts.
        let (group, playwright) = row("playwright");
        assert_eq!(
            (group, playwright.state.as_str()),
            ("servers", "not connected")
        );
        assert_eq!(playwright.description, "stdio");
        assert!(
            playwright
                .detail
                .ends_with("Not connected in this session yet. Connects when a tool is needed, or connect it now."),
            "{}",
            playwright.detail
        );
        assert!(
            !playwright.detail.contains("tools: 0"),
            "{}",
            playwright.detail
        );
        assert_eq!(playwright.tone, ExtensionTone::Idle);
        assert_eq!(
            command(playwright).as_deref(),
            Some("/mcp retry playwright")
        );
        assert_eq!(
            playwright.action.as_ref().map(ExtensionAction::label),
            Some("connect")
        );

        let (group, stripe) = row("stripe");
        assert_eq!(group, MCP_LOGIN_GROUP_ID);
        assert_eq!(command(stripe).as_deref(), Some("/mcp login stripe"));

        let (group, aws) = row("aws");
        assert_eq!((group, aws.state.as_str()), ("attention", "error"));
        assert_eq!(aws.tone, ExtensionTone::Failure);
        assert!(aws.detail.contains("rejected initialize"), "{}", aws.detail);

        let (group, linear) = row("linear");
        assert_eq!((group, linear.state.as_str()), ("servers", "disabled"));
        assert_eq!(command(linear).as_deref(), Some("/mcp enable linear"));

        let (_, chrome) = row("chrome-devtools");
        assert_eq!(chrome.state, "queued");
        assert_eq!(
            command(chrome),
            None,
            "a pending retry is not offered twice"
        );
    }

    #[test]
    fn empty_mcp_opens_suggestions_without_installing_and_skills_manage_does_not_loop() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path());
        let app = App::new_with_plugin_registry(
            crate::tui::test_support::test_tui_options(root.path()),
            &codewhale_runtime::config::Config::default(),
            registry,
        );
        let model = mcp_model(&app, Locale::En);
        assert_eq!(model.groups.len(), 1);
        assert_eq!(model.groups[0].label, "No MCP servers configured");
        let item = &model.groups[0].items[0];
        assert!(item.toggle.is_none() && item.remove.is_none());
        assert!(
            matches!(item.action.as_ref(), Some(ExtensionAction::Command {
            command, disposition: RowActionDisposition::InPlacePager, ..
        }) if command == "/mcp recommendations")
        );
        let skills = skills_model(&app, Locale::En);
        for item in skills.groups.iter().flat_map(|group| &group.items) {
            assert_eq!(
                item.action.as_ref().and_then(ExtensionAction::command),
                Some("/skills manage")
            );
        }
        let mut snapshot = ExtensionsSnapshot::default();
        snapshot.tabs[ExtensionsTab::Mcp.index()] = model;
        let mut view =
            ExtensionsView::from_snapshot_with_locale(snapshot, ExtensionsTab::Mcp, Locale::En);
        assert_eq!(view.selected[ExtensionsTab::Mcp.index()], 1);
        assert!(
            matches!(view.activate_selected(), ViewAction::Emit(ViewEvent::ExecutePanelCommand {
            command, pager_title: Some(_)
        }) if command == "/mcp recommendations")
        );
    }

    /// Tab widths are display cells: a two-cell CJK label measured in chars
    /// clipped its own text and let the next tab paint over it, and the hit
    /// rect covered the wrong cells.
    #[test]
    fn wide_tab_labels_get_their_full_cell_width_and_matching_hit_rects() {
        let view = ExtensionsView::from_snapshot_with_locale(
            ExtensionsSnapshot::default(),
            ExtensionsTab::Hooks,
            Locale::ZhHans,
        );
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let tabs = view.hits.borrow().tabs.clone();
        assert_eq!(tabs.len(), ExtensionsTab::ALL.len());
        let mut next_x = tabs[0].0.x;
        for (rect, tab) in tabs {
            let label = tab.label(Locale::ZhHans);
            assert_eq!(
                usize::from(rect.width),
                UnicodeWidthStr::width(label.as_str()) + 2,
                "{tab:?} hit rect must match its painted width"
            );
            assert_eq!(
                rect.x, next_x,
                "{tab:?} must start where the previous tab ended"
            );
            next_x = rect.right();
            // Read the row as a terminal shows it: a wide glyph covers the
            // cell after it.
            let mut painted = String::new();
            let mut x = rect.x;
            while x < rect.right() {
                let symbol = buf[(x, rect.y)].symbol();
                painted.push_str(symbol);
                x += u16::try_from(UnicodeWidthStr::width(symbol).max(1)).unwrap_or(1);
            }
            assert!(
                painted.contains(label.as_str()),
                "{tab:?} label {label:?} clipped: {painted:?}"
            );
        }
    }

    #[test]
    fn workbench_extension_hover_preserves_selection_and_small_resize_clears_targets() {
        let mut view = view_on_item(ExtensionAction::Command {
            label: "enable".into(),
            command: "/plugin enable demo".into(),
            disposition: RowActionDisposition::InPlace,
        });
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let (hit, row) = view.hits.borrow().rows[0];
        let selected = view.selected;
        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(view.hovered_row, Some(row));
        assert_eq!(view.selected, selected);
        view.render(area, &mut buf);
        assert_eq!(buf[(hit.right() - 1, hit.y)].bg, palette::SURFACE_ELEVATED);
        let tiny = Rect::new(0, 0, 20, 4);
        view.render(tiny, &mut Buffer::empty(tiny));
        assert!(view.hits.borrow().rows.is_empty());
        assert!(view.hits.borrow().tabs.is_empty());
    }

    #[test]
    fn a_plugin_contributed_server_is_not_mutable_from_this_panel() {
        // The row's name is synthesized and never appears in the config file
        // `/mcp remove` resolves against, so the gesture could only ever 404.
        let owned = BTreeSet::from(["github".to_string(), "playwright".to_string()]);
        assert!(mcp_row_is_mutable(Some(&owned), "github"));
        assert!(!mcp_row_is_mutable(
            Some(&owned),
            "plugin-25-codewhale-account-plugins-codewhale-plugins"
        ));
    }

    #[test]
    fn an_unreadable_config_keeps_the_gestures_rather_than_withdrawing_them() {
        assert!(mcp_row_is_mutable(None, "anything"));
    }

    /// The founder's report, as a test: `computer-use` is installed disabled and
    /// never reviewed, and the panel said `disabled` while offering no way
    /// to enable it — "computer use is disabled but i can't enable it. tf?".
    /// The row must now say what stands in the way and carry the switch.
    #[test]
    fn an_installed_bundle_says_what_it_needs_and_carries_a_switch() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let plugin_dir = root.path().join("plugins/computer-use");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            root.path().join("plugins"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"computer-use","version":"1.0.0","description":"Locally installed fixture"}"#,
        )
        .unwrap();
        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path());
        let app = App::new_with_plugin_registry(
            crate::tui::test_support::test_tui_options(root.path()),
            &codewhale_runtime::config::Config::default(),
            registry,
        );
        let plugin = app.plugin_registry.get("computer-use").unwrap();
        assert!(!plugin.enabled && !plugin.trusted(), "fixture precondition");

        let model = plugins_model(&app, Locale::En);
        let row = model
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|row| row.label == "computer-use")
            .expect("installed row");

        // Both halves of the truth, in words a person uses for a switch.
        assert_eq!(row.state, "off · needs review", "state: {}", row.state);
        // The switch is offered even though the bundle is unreviewed:
        // `/plugin enable` routes through the capability review itself.
        assert!(
            matches!(
                &row.toggle,
                Some(ExtensionAction::Command { command, .. })
                    if command == "/plugin enable computer-use"
            ),
            "toggle: {:?}",
            row.toggle
        );
        // And the verb on Enter is a verb, not the noun "inspect".
        assert!(
            matches!(
                &row.action,
                Some(ExtensionAction::Command { label, command, .. })
                    if label == "review" && command == "/plugin trust computer-use"
            ),
            "action: {:?}",
            row.action
        );
        assert!(
            row.detail
                .contains("only after you review exactly what it can do"),
            "detail must explain the review, got: {}",
            row.detail
        );
    }

    /// Space is the switch, on the row the founder could not turn on.
    #[test]
    fn space_runs_the_selected_rows_switch() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let plugin_dir = root.path().join("plugins/computer-use");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            root.path().join("plugins"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"computer-use","version":"1.0.0","description":"Locally installed fixture"}"#,
        )
        .unwrap();
        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path());
        let app = App::new_with_plugin_registry(
            crate::tui::test_support::test_tui_options(root.path()),
            &codewhale_runtime::config::Config::default(),
            registry,
        );
        let mut view = ExtensionsView::new(&app, ExtensionsTab::Plugins);
        assert_eq!(
            view.selected_item().map(|item| item.label.as_str()),
            Some("computer-use"),
            "the Plugins tab opens on the installed row"
        );
        let action = view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            matches!(
                action,
                ViewAction::Emit(ViewEvent::ExecutePanelCommand { ref command, .. })
                    if command == "/plugin enable computer-use"
            ),
            "Space must run the row's switch, got {action:?}"
        );
    }

    #[test]
    fn marketplace_installed_bundle_uses_local_metadata_and_review_action() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let plugin_dir = root.path().join("plugins/computer-use");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            root.path().join("plugins"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"computer-use","version":"1.0.0","description":"Locally installed fixture"}"#,
        )
        .unwrap();
        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path());
        let app = App::new_with_plugin_registry(
            crate::tui::test_support::test_tui_options(root.path()),
            &codewhale_runtime::config::Config::default(),
            registry,
        );
        let model = marketplace_model(&app, Locale::En);
        let group = model
            .groups
            .iter()
            .find(|group| group.id == "codewhale")
            .unwrap();
        let row = group
            .items
            .iter()
            .find(|row| row.label == "computer-use")
            .unwrap();
        let installed = app.plugin_registry.get("computer-use").unwrap();
        assert_eq!(
            row.description,
            installed
                .manifest
                .plugin
                .description
                .clone()
                .unwrap_or_default()
        );
        assert_eq!(row.state, plugin_row_state(Locale::En, installed));
        // The exact-content review stacks on the panel so the confirmed
        // digest lands on a row that then re-reads its trust state.
        assert!(
            matches!(&row.action, Some(ExtensionAction::Command { command, disposition: RowActionDisposition::InPlace, .. }) if command == "/plugin trust computer-use")
        );
        assert!(!installed.trusted());
        assert!(!installed.enabled);
        assert!(
            group.items.iter().any(|row| matches!(
                &row.action,
                Some(ExtensionAction::Command { command, .. })
                    if command == "/plugin marketplace install codewhale whalewiki"
            )),
            "an absent catalog bundle should still offer installation"
        );
    }

    #[test]
    fn mcp_item_action_for_stale_oauth_is_login() {
        let recovery = codewhale_engine::mcp::mcp_recovery_kind(
            true,
            true,
            false,
            Some("401 Unauthorized"),
            true,
        )
        .expect("stale oauth needs recovery");
        assert_eq!(recovery, McpRecoveryKind::Reauth);
        assert_eq!(recovery.slash_command("github"), "/mcp login github");
        assert_eq!(tr(Locale::En, recovery.label_key()).as_ref(), "re-auth");
    }

    #[test]
    fn a_healthy_server_offers_no_recovery_action() {
        // Founder live-test: "even the ones that are connected say diagnose
        // lol". Enabled, inspected, connected and erroring on nothing is not
        // a state anything repairs.
        assert_eq!(
            codewhale_engine::mcp::mcp_recovery_kind(true, true, true, None, false),
            None
        );
    }

    #[test]
    fn mcp_item_action_for_disconnected_server_is_reconnect() {
        let recovery = codewhale_engine::mcp::mcp_recovery_kind(true, true, false, None, false)
            .expect("a disconnected server needs recovery");
        assert_eq!(recovery, McpRecoveryKind::Reconnect);
        // The row names one server, so the command it runs must name it too:
        // reloading all of them leaves the row the user aimed at still pending
        // when the list returns, which reads as the key doing nothing.
        assert_eq!(
            recovery.slash_command("playwright"),
            "/mcp retry playwright"
        );
        assert_eq!(tr(Locale::En, recovery.label_key()).as_ref(), "reconnect");
    }

    /// Four distinct tones, four distinct inks, and none of them read out of
    /// a localized string — a screen that only colours correctly in English
    /// is not coloured.
    #[test]
    fn every_tone_paints_a_distinct_ink() {
        use codewhale_palette::ChromeInk;
        let theme = codewhale_palette::ThemeId::Whale.ui_theme();
        let inks: Vec<ChromeInk> = [
            ExtensionTone::Ready,
            ExtensionTone::Attention,
            ExtensionTone::Failure,
            ExtensionTone::Idle,
        ]
        .into_iter()
        .map(ExtensionTone::ink)
        .collect();
        let colors: std::collections::BTreeSet<String> = inks
            .iter()
            .map(|ink| format!("{:?}", ink.color(&theme)))
            .collect();
        assert_eq!(
            colors.len(),
            4,
            "each tone must be visually separable: {inks:?}"
        );
        assert_eq!(ExtensionTone::default(), ExtensionTone::Idle);
    }

    /// The grokbuild grammar: Tab / Shift+Tab move across the tab bar, even
    /// mid-search, and the query rides along to the new tab.
    #[test]
    fn tab_switches_tabs_and_keeps_the_search_query() {
        use crate::tui::views::ModalView;
        let mut view = ExtensionsView::from_snapshot_with_locale(
            ExtensionsSnapshot::default(),
            ExtensionsTab::Plugins,
            Locale::En,
        );
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        view.handle_key(key(KeyCode::Char('/')));
        view.handle_key(key(KeyCode::Char('g')));
        assert_eq!(view.focus, ExtensionsFocus::Search);

        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.active_tab, ExtensionsTab::Marketplace);
        assert_eq!(view.query, "g", "the query carries over to the new tab");

        view.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(view.active_tab, ExtensionsTab::Plugins);
        view.handle_key(key(KeyCode::BackTab));
        assert_eq!(view.active_tab, ExtensionsTab::Hooks);

        // Wraps: the last tab's next is the first.
        view.set_tab(ExtensionsTab::Mcp);
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.active_tab, ExtensionsTab::Hooks);
    }

    fn mcp_row(name: &str, state: &str, detail: &str, action: ExtensionAction) -> ExtensionItem {
        ExtensionItem {
            id: name.into(),
            label: name.into(),
            description: String::new(),
            state: state.into(),
            tone: ExtensionTone::Attention,
            detail: detail.into(),
            action: Some(action),
            toggle: None,
            remove: None,
        }
    }

    fn login_row(name: &str) -> ExtensionItem {
        mcp_row(
            name,
            &crate::tui::session_boot::mcp_auth_required_state_label(),
            "401 Unauthorized: the session is no longer accepted",
            ExtensionAction::Command {
                label: "re-auth".into(),
                command: McpRecoveryKind::Reauth.slash_command(name),
                disposition: RowActionDisposition::LeavePanel,
            },
        )
    }

    /// The founder's receipt (#5926): seven OAuth servers whose login
    /// expired and one that really failed. The expired logins lead in their
    /// own group with the login command on the row; the real failure keeps
    /// its reason; the connected server sorts last.
    #[test]
    fn mcp_rows_list_expired_logins_first_then_failures_with_their_reason() {
        let rows = vec![
            mcp_row(
                "alpha",
                "connected",
                "3 tools",
                ExtensionAction::Status {
                    label: "connected".into(),
                },
            ),
            mcp_row(
                "supabase",
                "error",
                "OAuth token refresh failed: Failed to parse server response",
                ExtensionAction::Command {
                    label: "diagnose".into(),
                    command: McpRecoveryKind::Diagnose.slash_command("supabase"),
                    disposition: RowActionDisposition::InPlace,
                },
            ),
            login_row("slack"),
            login_row("stripe"),
        ];
        let groups = mcp_groups(Locale::En, rows);
        let shape: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|group| {
                (
                    group.id.as_str(),
                    group.items.iter().map(|item| item.label.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("login", vec!["slack", "stripe"]),
                ("attention", vec!["supabase"]),
                ("servers", vec!["alpha"]),
            ]
        );
        assert_eq!(groups[0].label, "Needs login");
        assert_eq!(groups[1].label, "Needs attention");
        let slack = &groups[0].items[0];
        assert_eq!(
            slack.action.as_ref().and_then(ExtensionAction::command),
            Some("/mcp login slack")
        );
        assert!(!slack.state.contains("failed"), "{}", slack.state);
        assert_eq!(
            groups[1].items[0].detail,
            "OAuth token refresh failed: Failed to parse server response"
        );
    }

    /// Opening `/mcp` lands on the first server that needs a login, so Enter
    /// is the login key, not a fold of the group heading. A tab without a
    /// login group but no items keeps the empty-state selection.
    #[test]
    fn mcp_tab_opens_on_the_first_login_row() {
        let mut snapshot = ExtensionsSnapshot::default();
        snapshot.tabs[ExtensionsTab::Mcp.index()] = ExtensionsTabModel {
            groups: mcp_groups(Locale::En, vec![login_row("slack"), login_row("stripe")]),
            problem: None,
        };
        let view = ExtensionsView::from_snapshot_with_locale(
            snapshot.clone(),
            ExtensionsTab::Mcp,
            Locale::En,
        );
        assert_eq!(view.selected[ExtensionsTab::Mcp.index()], 1);
        let entries = view.visible_entries();
        match entries[1] {
            VisibleEntry::Item(group, item) => {
                assert_eq!(group.id, MCP_LOGIN_GROUP_ID);
                assert_eq!(item.label, "slack");
                assert_eq!(
                    item.action.as_ref().and_then(ExtensionAction::command),
                    Some("/mcp login slack")
                );
            }
            other => panic!("expected the first login row, got {other:?}"),
        }

        let plain = ExtensionsView::from_snapshot_with_locale(
            ExtensionsSnapshot::default(),
            ExtensionsTab::Mcp,
            Locale::En,
        );
        assert_eq!(plain.selected[ExtensionsTab::Mcp.index()], 0);
    }

    fn item_with_action(action: ExtensionAction) -> ExtensionItem {
        ExtensionItem {
            id: "row".into(),
            label: "row".into(),
            description: String::new(),
            state: "state".into(),
            tone: ExtensionTone::Idle,
            detail: "detail".into(),
            action: Some(action),
            toggle: None,
            remove: None,
        }
    }

    fn view_on_item(action: ExtensionAction) -> ExtensionsView {
        let mut snapshot = ExtensionsSnapshot::default();
        snapshot.tabs[ExtensionsTab::Plugins.index()] = ExtensionsTabModel {
            groups: vec![ExtensionGroup {
                id: "g".into(),
                label: "g".into(),
                items: vec![item_with_action(action)],
            }],
            problem: None,
        };
        let mut view =
            ExtensionsView::from_snapshot_with_locale(snapshot, ExtensionsTab::Plugins, Locale::En);
        // Land on the item, not its group heading.
        view.selected[ExtensionsTab::Plugins.index()] = 1;
        view
    }

    /// T10: right-click used to arm the row's removal and a second one ran
    /// it, with no menu ever shown. It now opens the row's menu; removal is
    /// its last entry, behind the menu's own confirm, and only the confirmed
    /// entry emits the remove command.
    #[test]
    fn right_click_opens_a_row_menu_instead_of_removing() {
        let mut view = view_on_item(ExtensionAction::Command {
            label: "enable".into(),
            command: "/mcp enable demo".into(),
            disposition: RowActionDisposition::InPlace,
        });
        {
            let item = &mut view.snapshot.tabs[ExtensionsTab::Plugins.index()].groups[0].items[0];
            item.toggle = Some(ExtensionAction::Command {
                label: "disable".into(),
                command: "/mcp disable demo".into(),
                disposition: RowActionDisposition::InPlace,
            });
            item.remove = Some(ExtensionAction::Command {
                label: "remove".into(),
                command: "/mcp remove demo".into(),
                disposition: RowActionDisposition::InPlace,
            });
        }
        let area = Rect::new(0, 0, 100, 30);
        view.render(area, &mut Buffer::empty(area));
        let hit = view
            .hits
            .borrow()
            .rows
            .iter()
            .find(|(_, row)| *row == 1)
            .map(|(rect, _)| *rect)
            .expect("the item row is painted");
        let right_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: hit.x + 1,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        };

        for _ in 0..2 {
            let ViewAction::Emit(ViewEvent::OpenContextMenu { title, entries, .. }) =
                view.handle_mouse(right_click)
            else {
                panic!("right-click must open the row menu");
            };
            assert_eq!(title, "row");
            let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
            assert_eq!(labels, ["Enable", "Open details", "Disable", "Remove…"]);
            assert!(entries[0].primary);
            let remove = entries.last().unwrap();
            assert!(remove.confirm_label.is_some(), "removal is confirmed");
            assert!(view.pending_remove.is_none(), "right-click arms nothing");
        }

        match view.run_menu_verb("row", crate::tui::views::ExtensionMenuVerb::Remove) {
            Some(ViewAction::Emit(ViewEvent::ExecutePanelCommand { command, .. })) => {
                assert_eq!(command, "/mcp remove demo");
            }
            other => panic!("the confirmed entry removes the row, got {other:?}"),
        }
        assert!(
            view.run_menu_verb("gone", crate::tui::views::ExtensionMenuVerb::Remove)
                .is_none(),
            "a row that left the list is reported, not guessed at"
        );
    }

    /// The defect: every row closed the panel and dropped its command into
    /// the transcript. A mutation runs in place — the event carries the
    /// command, and `Emit` (not `EmitAndClose`) is what keeps the panel.
    #[test]
    fn in_place_row_action_emits_without_closing() {
        let mut view = view_on_item(ExtensionAction::Command {
            label: "enable".into(),
            command: "/plugin enable demo".into(),
            disposition: RowActionDisposition::InPlace,
        });
        match view.activate_selected() {
            ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                command,
                pager_title,
            }) => {
                assert_eq!(command, "/plugin enable demo");
                assert_eq!(pager_title, None);
            }
            other => panic!("expected an in-place command, got {other:?}"),
        }
    }

    /// An inspect row keeps the panel open and asks for its text output in a
    /// pager stacked on the panel — the detail belongs to the row, not to a
    /// transcript dump behind the modal.
    #[test]
    fn inspect_row_action_pages_its_output_in_place() {
        let mut view = view_on_item(ExtensionAction::Command {
            label: "open".into(),
            command: "/plugin show demo".into(),
            disposition: RowActionDisposition::InPlacePager,
        });
        match view.activate_selected() {
            ViewAction::Emit(ViewEvent::ExecutePanelCommand {
                command,
                pager_title,
            }) => {
                assert_eq!(command, "/plugin show demo");
                assert_eq!(pager_title.as_deref(), Some("row"));
            }
            other => panic!("expected a paged inspect, got {other:?}"),
        }
    }

    /// A flow that owns another surface — a login, an editor, the composer's
    /// trust token — still yields the panel.
    #[test]
    fn leave_panel_row_action_still_closes() {
        let mut view = view_on_item(ExtensionAction::Command {
            label: "re-auth".into(),
            command: "/mcp login github".into(),
            disposition: RowActionDisposition::LeavePanel,
        });
        match view.activate_selected() {
            ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => assert_eq!(command, "/mcp login github"),
            other => panic!("expected the panel to yield, got {other:?}"),
        }
    }

    /// A refresh swaps the read model without disturbing the session: tab,
    /// query, selection, and folds all survive, and the new MCP generation
    /// the poll compares against rides along.
    #[test]
    fn refresh_preserves_view_state_and_tracks_generation() {
        let mut view = view_on_item(ExtensionAction::Command {
            label: "enable".into(),
            command: "/plugin enable demo".into(),
            disposition: RowActionDisposition::InPlace,
        });
        view.query = "de".into();
        let mut fresh = ExtensionsSnapshot {
            mcp_generation: 7,
            mcp_initializing: true,
            ..ExtensionsSnapshot::default()
        };
        fresh.tabs[ExtensionsTab::Plugins.index()] = ExtensionsTabModel {
            groups: vec![ExtensionGroup {
                id: "g".into(),
                label: "g".into(),
                items: vec![item_with_action(ExtensionAction::Status {
                    label: "enabled".into(),
                })],
            }],
            problem: None,
        };
        view.refresh_snapshot(fresh);
        assert_eq!(view.active_tab, ExtensionsTab::Plugins);
        assert_eq!(view.query, "de");
        assert_eq!(view.snapshot.mcp_generation, 7);
        assert!(view.snapshot.mcp_initializing);
        // The refreshed row's action is the new model's, not the stale one.
        let entries = view.visible_entries();
        match entries[view.selected[ExtensionsTab::Plugins.index()]] {
            VisibleEntry::Item(_, item) => {
                assert!(matches!(item.action, Some(ExtensionAction::Status { .. })));
            }
            other => panic!("expected the refreshed item, got {other:?}"),
        }
    }

    /// U09-05: a refresh that reorders the list keeps the selection on the
    /// same entity, so a later toggle acts on the row the user chose — never
    /// on whatever slid into the old index.
    #[test]
    fn refresh_keeps_selection_on_the_same_entity_when_rows_reorder() {
        fn row(id: &str) -> ExtensionItem {
            ExtensionItem {
                id: id.into(),
                label: id.into(),
                toggle: Some(ExtensionAction::Command {
                    label: "disable".into(),
                    command: format!("/plugin disable {id}"),
                    disposition: RowActionDisposition::InPlace,
                }),
                ..item_with_action(ExtensionAction::Status {
                    label: "enabled".into(),
                })
            }
        }
        fn snapshot(order: &[&str]) -> ExtensionsSnapshot {
            let mut snapshot = ExtensionsSnapshot::default();
            snapshot.tabs[ExtensionsTab::Plugins.index()] = ExtensionsTabModel {
                groups: vec![ExtensionGroup {
                    id: "g".into(),
                    label: "g".into(),
                    items: order.iter().map(|id| row(id)).collect(),
                }],
                problem: None,
            };
            snapshot
        }
        let mut view = ExtensionsView::from_snapshot_with_locale(
            snapshot(&["alpha", "beta"]),
            ExtensionsTab::Plugins,
            Locale::En,
        );
        // Group heading, alpha, beta: select beta.
        view.selected[ExtensionsTab::Plugins.index()] = 2;
        assert_eq!(
            view.selected_item().map(|item| item.id.as_str()),
            Some("beta")
        );

        view.refresh_snapshot(snapshot(&["beta", "alpha"]));
        assert_eq!(
            view.selected_item().map(|item| item.id.as_str()),
            Some("beta")
        );
        match view.toggle_selected() {
            ViewAction::Emit(ViewEvent::ExecutePanelCommand { command, .. }) => {
                assert_eq!(command, "/plugin disable beta");
            }
            other => panic!("expected the toggle command, got {other:?}"),
        }

        // A selected entity that is gone falls back to the clamped row.
        view.refresh_snapshot(snapshot(&["alpha"]));
        assert_eq!(
            view.selected_item().map(|item| item.id.as_str()),
            Some("alpha")
        );
    }
}
