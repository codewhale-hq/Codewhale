//! Read-only projections for the Tideline terminal workbench contract.
//!
//! [`App`] remains the sole owner of runtime state. This module neither
//! replaces it nor introduces another settings store, event loop, or engine;
//! it gives render and input code typed snapshots of facts that existing
//! owners have already resolved.

use ratatui::layout::{Position, Rect};

use crate::tui::app::App;

/// A bounded view of the context window currently owned by the active route.
///
/// Percent is stored in basis points (`10_000 == 100%`) so snapshots remain
/// equality-testable without making renderers compare floating-point values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudgetSnapshot {
    pub used_tokens: u32,
    pub max_tokens: u32,
    pub percent_basis_points: u16,
}

impl ContextBudgetSnapshot {
    /// Project the existing context estimator without becoming a second
    /// context-budget owner.
    #[must_use]
    pub(crate) fn from_app(app: &App) -> Option<Self> {
        let (used, max_tokens, _) = crate::tui::ui::context_usage_snapshot(app)?;
        let used_tokens = u32::try_from(used.max(0))
            .unwrap_or(u32::MAX)
            .min(max_tokens);
        let percent_basis_points = if max_tokens == 0 {
            0
        } else {
            let numerator = u64::from(used_tokens).saturating_mul(10_000);
            let rounded = numerator
                .saturating_add(u64::from(max_tokens) / 2)
                .checked_div(u64::from(max_tokens))
                .unwrap_or(0)
                .min(10_000);
            u16::try_from(rounded).unwrap_or(10_000)
        };

        Some(Self {
            used_tokens,
            max_tokens,
            percent_basis_points,
        })
    }
}

/// The owner whose value currently wins for a setting fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "later settings surfaces consume the non-session authority variants"
)]
pub enum SettingAuthority {
    Session,
    UserSettings,
    WorkspaceConfiguration,
    ManagedPolicy,
    /// An environment variable or session (SSH) forces the effective value.
    Environment,
    /// The terminal program forces the effective value.
    Terminal,
}

/// When an edit to a setting becomes observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "later settings editors consume the non-current apply variants"
)]
pub enum SettingApplySemantics {
    EffectiveNow,
    Immediate,
    NextSession,
    RestartRequired,
    ReadOnly,
    /// Persisted and the live owner updated now, but a running consumer keeps
    /// the old value until it is explicitly reloaded (MCP servers after an
    /// `mcp_config_path` change: `/mcp reload`).
    ReloadRequired,
    /// The UI applies the edit now while engine tools only read it at startup
    /// (`workspace_follow_symlinks`).
    UiNowEngineRestart,
}

/// One setting without collapsing live, resolved, startup, and persisted
/// values into an ambiguous `Session`/`Saved` label.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the settings view consumes this projection in the next Tideline slice"
)]
pub struct SettingFact<T> {
    /// Value currently held by the live owner before further resolution.
    pub current: Option<T>,
    /// Value actually in force after route, policy, or session overrides.
    pub effective: Option<T>,
    /// Value a fresh session is expected to start with, when observed.
    pub startup: Option<T>,
    /// Exact persisted value last read from its owning store, when observed.
    pub saved: Option<T>,
    pub authority: SettingAuthority,
    pub apply: SettingApplySemantics,
}

#[allow(
    dead_code,
    reason = "the settings view consumes this projection in the next Tideline slice"
)]
impl<T: Clone> SettingFact<T> {
    /// A fact already owned by the active session.
    #[must_use]
    pub fn active_session(value: T) -> Self {
        Self {
            current: Some(value.clone()),
            effective: Some(value),
            startup: None,
            saved: None,
            authority: SettingAuthority::Session,
            apply: SettingApplySemantics::EffectiveNow,
        }
    }
}

/// The narrow, read-only workbench projection available in this slice.
///
/// `App` intentionally does not retain a resident [`codewhale_runtime::settings::Settings`]
/// value. Consequently this projection never reloads disk or guesses startup
/// defaults: those lanes remain `None` until the settings owner supplies them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the composed workbench consumes this projection in the next Tideline slice"
)]
pub struct UiSnapshot {
    pub context_budget: Option<ContextBudgetSnapshot>,
    pub provider: SettingFact<String>,
    pub model: SettingFact<String>,
}

#[allow(
    dead_code,
    reason = "the composed workbench consumes this projection in the next Tideline slice"
)]
impl UiSnapshot {
    #[must_use]
    pub(crate) fn from_app(app: &App) -> Self {
        let (provider, model) = app.effective_route_identity_display();
        Self {
            context_budget: ContextBudgetSnapshot::from_app(app),
            provider: SettingFact::active_session(provider),
            model: SettingFact::active_session(model),
        }
    }
}

/// Stable identifier from the Tideline wiring manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InteractionTargetId(&'static str);

impl InteractionTargetId {
    pub const HEADER_CONTEXT: Self = Self("header.context");
    /// The rendered route/model segment. This is intentionally an affordance
    /// id only: the provider picker remains the owner of route catalog and
    /// readiness facts.
    pub const HEADER_ROUTE: Self = Self("header.route");
    /// The model name and effort tier inside the info line's route segment.
    pub const HEADER_MODEL: Self = Self("header.model");
    pub const HEADER_UPDATE: Self = Self("header.update");
    /// A live count in the posture bar; opens the dock view it counts.
    pub const FOOTER_COUNT: Self = Self("footer.count");
    pub const DOCK_TAB_AGENTS: Self = Self("dock.tab.agents");
    pub const DOCK_TAB_TASKS: Self = Self("dock.tab.tasks");
    pub const DOCK_TAB_BACKGROUND: Self = Self("dock.tab.background");
    pub const DOCK_TAB_TERMINAL: Self = Self("dock.tab.terminal");
    pub const DOCK_TAB_FILES: Self = Self("dock.tab.files");
    pub const DOCK_TAB_NOTEPAD: Self = Self("dock.tab.notepad");
    pub const DOCK_TAB_CONTEXT: Self = Self("dock.tab.context");
    pub const DOCK_TAB_GIT: Self = Self("dock.tab.git");
    pub const DOCK_TAB_PRICE: Self = Self("dock.tab.price");
    pub const DOCK_CLOSE: Self = Self("dock.close");
}

/// Typed destination shared by keyboard and mouse input routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionAction {
    InspectContext,
    /// Open the existing provider/route picker without making this chrome
    /// target another source of catalog or runtime authority.
    OpenProviderPicker,
    /// Open the existing `/model` picker. Same discipline as the provider
    /// entry: an entry point, never a second catalog.
    OpenModelPicker,
    OpenUpdate,
    /// Open the existing automations manager.
    OpenAutomations,
    ShowDockPanel(crate::tui::work_surface::RailPanel),
    DismissDock,
}

/// Focus metadata for a selectable target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "ordered focus traversal lands with the later multi-target surfaces"
)]
pub enum InteractionFocus {
    /// The target has a direct keyboard shortcut but is not in traversal yet.
    Direct,
    /// The target participates in ordered focus traversal.
    Traversable { order: u16, focused: bool },
}

/// Typed, non-prose evidence made available to an inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectDetail {
    ContextBudget(ContextBudgetSnapshot),
    /// The topbar exposes a route entry point, not a copied route snapshot.
    /// `ProviderPickerView` remains the authoritative presentation owner.
    Route,
    Update,
}

/// A selectable region painted in the current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionTarget {
    pub id: InteractionTargetId,
    pub area: Rect,
    pub focus: InteractionFocus,
    pub keyboard_action: Option<InteractionAction>,
    pub mouse_action: Option<InteractionAction>,
    pub inspect_detail: InspectDetail,
}

/// Frame-scoped interaction geometry.
///
/// Targets are cleared before every render. Hit testing runs newest-first so a
/// later modal or overlay can safely own cells also covered by a lower layer.
#[derive(Debug, Default)]
pub struct InteractionRegistry {
    targets: Vec<InteractionTarget>,
}

impl InteractionRegistry {
    pub fn clear(&mut self) {
        self.targets.clear();
    }

    pub fn register(&mut self, target: InteractionTarget) {
        if target.area.width > 0 && target.area.height > 0 {
            self.targets.push(target);
        }
    }

    #[must_use]
    pub fn target_at(&self, column: u16, row: u16) -> Option<&InteractionTarget> {
        let position = Position::new(column, row);
        self.targets
            .iter()
            .rev()
            .find(|target| target.area.contains(position))
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &InteractionTarget> {
        self.targets.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContextBudgetSnapshot, InspectDetail, InteractionAction, InteractionFocus,
        InteractionRegistry, InteractionTarget, InteractionTargetId, SettingApplySemantics,
        SettingAuthority, SettingFact, UiSnapshot,
    };
    use codewhale_config::ProviderKind;
    use ratatui::layout::Rect;

    fn target(area: Rect, used_tokens: u32) -> InteractionTarget {
        InteractionTarget {
            id: InteractionTargetId::HEADER_CONTEXT,
            area,
            focus: InteractionFocus::Direct,
            keyboard_action: Some(InteractionAction::InspectContext),
            mouse_action: Some(InteractionAction::InspectContext),
            inspect_detail: InspectDetail::ContextBudget(ContextBudgetSnapshot {
                used_tokens,
                max_tokens: 10_000,
                percent_basis_points: 3_000,
            }),
        }
    }

    #[test]
    fn topbar_route_target_is_typed_without_copying_route_facts() {
        let target = InteractionTarget {
            id: InteractionTargetId::HEADER_ROUTE,
            area: Rect::new(20, 0, 24, 1),
            focus: InteractionFocus::Direct,
            keyboard_action: Some(InteractionAction::OpenProviderPicker),
            mouse_action: Some(InteractionAction::OpenProviderPicker),
            inspect_detail: InspectDetail::Route,
        };

        assert_eq!(target.id, InteractionTargetId::HEADER_ROUTE);
        assert_eq!(
            target.keyboard_action,
            Some(InteractionAction::OpenProviderPicker)
        );
        assert_eq!(target.mouse_action, target.keyboard_action);
        assert_eq!(target.inspect_detail, InspectDetail::Route);
    }

    #[test]
    fn ui_snapshot_uses_active_route_without_claiming_saved_defaults() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.pending_turn_route = Some((ProviderKind::Zai, "GLM-5.3".to_string(), false));

        let snapshot = UiSnapshot::from_app(&app);

        assert_eq!(
            snapshot.provider.current.as_deref(),
            Some(ProviderKind::Zai.provider().display_name())
        );
        assert_eq!(snapshot.provider.current, snapshot.provider.effective);
        assert_eq!(snapshot.model.current.as_deref(), Some("GLM-5.3"));
        assert_eq!(snapshot.model.current, snapshot.model.effective);
        assert!(snapshot.provider.startup.is_none());
        assert!(snapshot.provider.saved.is_none());
        assert_eq!(snapshot.provider.authority, SettingAuthority::Session);
        assert_eq!(snapshot.provider.apply, SettingApplySemantics::EffectiveNow);
    }

    #[test]
    fn context_budget_projection_reuses_and_bounds_the_existing_estimate() {
        let app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        let (used, max_tokens, _) =
            crate::tui::ui::context_usage_snapshot(&app).expect("existing context estimate");
        let snapshot = ContextBudgetSnapshot::from_app(&app).expect("Tideline projection");

        assert_eq!(snapshot.used_tokens, u32::try_from(used).unwrap());
        assert_eq!(snapshot.max_tokens, max_tokens);
        assert!(snapshot.used_tokens <= snapshot.max_tokens);
        assert!(snapshot.percent_basis_points <= 10_000);
    }

    #[test]
    fn setting_fact_keeps_live_startup_and_saved_lanes_distinct() {
        let fact = SettingFact {
            current: Some("session"),
            effective: Some("managed"),
            startup: Some("next"),
            saved: Some("disk"),
            authority: SettingAuthority::ManagedPolicy,
            apply: SettingApplySemantics::NextSession,
        };

        assert_eq!(fact.current, Some("session"));
        assert_eq!(fact.effective, Some("managed"));
        assert_eq!(fact.startup, Some("next"));
        assert_eq!(fact.saved, Some("disk"));
    }

    #[test]
    fn apply_semantics_distinguish_reload_and_partial_restart_from_full_restart() {
        let reload = SettingFact {
            current: Some("live"),
            effective: None,
            startup: None,
            saved: Some("disk"),
            authority: SettingAuthority::UserSettings,
            apply: SettingApplySemantics::ReloadRequired,
        };
        let partial = SettingFact {
            apply: SettingApplySemantics::UiNowEngineRestart,
            ..reload.clone()
        };
        assert_ne!(reload.apply, SettingApplySemantics::RestartRequired);
        assert_ne!(partial.apply, SettingApplySemantics::RestartRequired);
        assert_ne!(reload.apply, partial.apply);
        assert_ne!(reload.apply, SettingApplySemantics::Immediate);
    }

    #[test]
    fn registry_ignores_empty_geometry_and_prefers_the_topmost_target() {
        let mut registry = InteractionRegistry::default();

        registry.register(target(Rect::new(2, 2, 6, 3), 3_000));
        registry.register(target(Rect::new(4, 3, 6, 3), 4_000));
        registry.register(target(Rect::new(0, 0, 0, 1), 5_000));

        assert_eq!(registry.iter().count(), 2);
        assert_eq!(
            registry.target_at(5, 3).map(|target| target.area),
            Some(Rect::new(4, 3, 6, 3))
        );
        assert_eq!(
            registry.target_at(2, 2).map(|target| target.area),
            Some(Rect::new(2, 2, 6, 3))
        );
        assert!(registry.target_at(20, 20).is_none());

        registry.clear();
        assert_eq!(registry.iter().count(), 0);
    }
}
