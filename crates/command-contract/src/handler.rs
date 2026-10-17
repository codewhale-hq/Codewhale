//! Generic handler transport for staged command migration.
//!
//! The output type is generic so FEAT-014 does not move or duplicate the
//! TUI-owned `CommandResult`. During in-place adoption, the TUI instantiates
//! `CommandHandler<crate::commands::CommandResult>`.

use crate::facets::{
    CommandConfigStatusContext, CommandCostContext, CommandDebugChangeContext,
    CommandDebugDiagnosticsContext, CommandDebugDiffContext, CommandDebugHistoryContext,
    CommandDebugReceiptsContext, CommandDebugUndoContext, CommandMediaContext,
    CommandMemoryContext, CommandModePolicyContext, CommandModelContext, CommandPermissionsContext,
    CommandPluginContext, CommandPresentationContext, CommandProjectContext, CommandSessionContext,
    CommandSessionControlContext, CommandSessionExportContext, CommandSessionLifecycleContext,
    CommandSessionStructcopyContext, CommandSkillGroupContext, CommandSkillsContext,
    CommandSystemPromptContext, CommandWorkspaceContext,
};

/// Exact host capabilities exposed to one contextual command handler.
///
/// The set lives in the external contract crate so command registrations can
/// declare least authority without naming the TUI host. The dispatcher uses
/// the declaration to populate only those slots in [`CommandContexts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommandCapabilities(u32);

impl CommandCapabilities {
    pub const NONE: Self = Self(0);
    pub const SESSION: Self = Self(1 << 0);
    pub const MODEL: Self = Self(1 << 1);
    pub const COST: Self = Self(1 << 2);
    pub const MODE_POLICY: Self = Self(1 << 3);
    pub const SYSTEM_PROMPT: Self = Self(1 << 4);
    pub const SKILLS: Self = Self(1 << 5);
    pub const WORKSPACE: Self = Self(1 << 6);
    pub const PRESENTATION: Self = Self(1 << 7);
    pub const MEDIA: Self = Self(1 << 8);
    /// Memory-group host data (FEAT-019 D1).
    pub const MEMORY: Self = Self(1 << 9);
    /// Project-group host data (FEAT-021 D1).
    pub const PROJECT: Self = Self(1 << 10);
    /// Skills-group host data (FEAT-022 D1).
    pub const SKILL_GROUP: Self = Self(1 << 11);
    /// Plugin-group host data (FEAT-020 D1), appended after current main capabilities.
    pub const PLUGIN: Self = Self(1 << 12);
    /// Session-lifecycle host data (FEAT-023 D3), the next non-conflicting bit
    /// after `PLUGIN`. Required only by the seven host-dependent lifecycle
    /// commands; `/compact` and `/purge` remain pure. Never widened by the
    /// basic session capability.
    pub const SESSION_LIFECYCLE: Self = Self(1 << 13);
    /// Session-control host data (FEAT-024 D3), the next non-conflicting bit
    /// after `SESSION_LIFECYCLE`. Required only by the six host-dependent
    /// control commands (`/relay`, `/rename`, `/resume`, `/rc`, `/remote-env`,
    /// `/title`); `/remote-env` also declares `PRESENTATION`. Bit 14 fit the
    /// original `u16` backing without speculative widening in FEAT-023.
    pub const SESSION_CONTROL: Self = Self(1 << 14);
    /// Session-export host data (FEAT-025 D1), the next non-conflicting bit
    /// after `SESSION_CONTROL`. Required only by the host-dependent `/export`
    /// command (and its `/daochu` alias) and by `/share`, which publishes the
    /// same redacted projection; every concrete App, snapshot, clipboard,
    /// filesystem, history, and turn-handoff access stays behind the TUI export
    /// adapter.
    ///
    /// This filled the original 16-bit space. FEAT-029 widened the backing
    /// storage before allocating the next independent diagnostics authority;
    /// the published identity of this bit remains unchanged.
    pub const SESSION_EXPORT: Self = Self(1 << 15);
    /// Debug diagnostics host data (FEAT-029 D3/D4). This is the first bit in
    /// the widened backing storage; other debug commands retain their own independent
    /// authority and do not borrow this facet.
    pub const DEBUG_DIAGNOSTICS: Self = Self(1 << 16);

    /// Debug receipts authority; independent from diagnostics and other debug operations.
    pub const DEBUG_RECEIPTS: Self = Self(1 << 17);
    /// Debug change authority; independent from diagnostics and other debug operations.
    pub const DEBUG_CHANGE: Self = Self(1 << 18);
    /// Debug history authority; independent from diagnostics and other debug operations.
    pub const DEBUG_HISTORY: Self = Self(1 << 19);
    /// Debug diff authority; independent from diagnostics and other debug operations.
    pub const DEBUG_DIFF: Self = Self(1 << 20);
    /// Debug undo authority; independent from diagnostics and other debug operations.
    pub const DEBUG_UNDO: Self = Self(1 << 21);

    /// One human-selected structural copy; independent from export/recovery.
    pub const SESSION_STRUCTCOPY: Self = Self(1 << 22);
    /// Permission observations and token-checked removal, independent of status.
    pub const PERMISSIONS: Self = Self(1 << 23);
    /// Read-only status observations, with no permission mutation authority.
    pub const CONFIG_STATUS: Self = Self(1 << 24);
    /// Config-group host data (FEAT-028 D3), the next non-conflicting bit after
    /// `CONFIG_STATUS`. Read/write settings, keys, presets, the editability
    /// audit and login/logout observations for the config command group. It does
    /// not grant pet, trust, mode, or workspace authority.
    pub const CONFIG: Self = Self(1 << 25);
    /// Pet presence/mode/sound/habitat host data (FEAT-028 D3), independent of
    /// general configuration and system settings.
    pub const PET: Self = Self(1 << 26);
    /// Workspace trust and trusted-path allow-list host data (FEAT-028 D3),
    /// independent of the general config facet and of `WORKSPACE` operations.
    pub const TRUST: Self = Self(1 << 27);
    /// Account/provider session state for `/login` and `/logout` (FEAT-028 D3).
    pub const LOGIN: Self = Self(1 << 28);
    /// Workbar placement/panel state for `/workbar` (FEAT-028 D3).
    pub const WORK_SURFACE: Self = Self(1 << 29);
    pub const NAVIGATION: Self = Self(1 << 30);
    pub const VOICE_INPUT: Self = Self(1 << 31);

    /// Raw bit pattern, for tests that pin the capability-space capacity.
    ///
    /// Kept `#[cfg(test)]` so the `u32` backing stays an implementation detail
    /// and nothing can widen it accidentally through a public accessor.
    #[cfg(test)]
    pub(crate) const fn bits_for_test(self) -> u32 {
        self.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, capability: Self) -> bool {
        !capability.is_empty() && self.0 & capability.0 == capability.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for CommandCapabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

/// A command handler that is either argument-only or capability-scoped.
#[derive(Clone, Copy)]
pub enum CommandHandler<R> {
    Pure(fn(Option<&str>) -> R),
    Contextual {
        capabilities: CommandCapabilities,
        handler: fn(CommandContexts<'_>, Option<&str>) -> R,
    },
}

/// Transport envelope with one independently optional facet slot.
pub struct CommandContexts<'a> {
    session: Option<&'a mut dyn CommandSessionContext>,
    model: Option<&'a mut dyn CommandModelContext>,
    cost: Option<&'a mut dyn CommandCostContext>,
    mode_policy: Option<&'a mut dyn CommandModePolicyContext>,
    system_prompt: Option<&'a mut dyn CommandSystemPromptContext>,
    skills: Option<&'a mut dyn CommandSkillsContext>,
    workspace: Option<&'a mut dyn CommandWorkspaceContext>,
    presentation: Option<&'a mut dyn CommandPresentationContext>,
    media: Option<&'a mut dyn CommandMediaContext>,
    memory: Option<&'a mut dyn CommandMemoryContext>,
    project: Option<&'a mut dyn CommandProjectContext>,
    skill_group: Option<&'a mut dyn CommandSkillGroupContext>,
    plugin: Option<&'a mut dyn CommandPluginContext>,
    lifecycle: Option<&'a mut dyn CommandSessionLifecycleContext>,
    control: Option<&'a mut dyn CommandSessionControlContext>,
    export: Option<&'a mut dyn CommandSessionExportContext>,
    structcopy: Option<&'a mut dyn CommandSessionStructcopyContext>,
    debug_receipts: Option<&'a mut dyn CommandDebugReceiptsContext>,
    debug_change: Option<&'a mut dyn CommandDebugChangeContext>,
    debug_history: Option<&'a mut dyn CommandDebugHistoryContext>,
    debug_diff: Option<&'a mut dyn CommandDebugDiffContext>,
    debug_undo: Option<&'a mut dyn CommandDebugUndoContext>,
    debug_diagnostics: Option<&'a mut dyn CommandDebugDiagnosticsContext>,
    permissions: Option<&'a mut dyn CommandPermissionsContext>,
    config_status: Option<&'a mut dyn CommandConfigStatusContext>,
    config: Option<&'a mut dyn crate::config::CommandConfigContext>,
    pet: Option<&'a mut dyn crate::config::CommandPetContext>,
    trust: Option<&'a mut dyn crate::config::CommandTrustContext>,
    login: Option<&'a mut dyn crate::config::CommandLoginContext>,
    work_surface: Option<&'a mut dyn crate::config::CommandWorkSurfaceContext>,
    navigation: Option<&'a mut dyn crate::navigation::CommandNavigationContext>,
    voice_input: Option<&'a mut dyn crate::navigation::CommandVoiceInputContext>,
}

/// Consumed envelope used when one handler needs several independent facets.
pub struct ContextParts<'a> {
    pub session: Option<&'a mut dyn CommandSessionContext>,
    pub model: Option<&'a mut dyn CommandModelContext>,
    pub cost: Option<&'a mut dyn CommandCostContext>,
    pub mode_policy: Option<&'a mut dyn CommandModePolicyContext>,
    pub system_prompt: Option<&'a mut dyn CommandSystemPromptContext>,
    pub skills: Option<&'a mut dyn CommandSkillsContext>,
    pub workspace: Option<&'a mut dyn CommandWorkspaceContext>,
    pub presentation: Option<&'a mut dyn CommandPresentationContext>,
    pub media: Option<&'a mut dyn CommandMediaContext>,
    pub memory: Option<&'a mut dyn CommandMemoryContext>,
    pub project: Option<&'a mut dyn CommandProjectContext>,
    pub skill_group: Option<&'a mut dyn CommandSkillGroupContext>,
    pub plugin: Option<&'a mut dyn CommandPluginContext>,
    pub lifecycle: Option<&'a mut dyn CommandSessionLifecycleContext>,
    pub control: Option<&'a mut dyn CommandSessionControlContext>,
    pub export: Option<&'a mut dyn CommandSessionExportContext>,
    pub structcopy: Option<&'a mut dyn CommandSessionStructcopyContext>,
    pub debug_receipts: Option<&'a mut dyn CommandDebugReceiptsContext>,
    pub debug_change: Option<&'a mut dyn CommandDebugChangeContext>,
    pub debug_history: Option<&'a mut dyn CommandDebugHistoryContext>,
    pub debug_diff: Option<&'a mut dyn CommandDebugDiffContext>,
    pub debug_undo: Option<&'a mut dyn CommandDebugUndoContext>,
    pub debug_diagnostics: Option<&'a mut dyn CommandDebugDiagnosticsContext>,
    pub permissions: Option<&'a mut dyn CommandPermissionsContext>,
    pub config_status: Option<&'a mut dyn CommandConfigStatusContext>,
    pub config: Option<&'a mut dyn crate::config::CommandConfigContext>,
    pub pet: Option<&'a mut dyn crate::config::CommandPetContext>,
    pub trust: Option<&'a mut dyn crate::config::CommandTrustContext>,
    pub login: Option<&'a mut dyn crate::config::CommandLoginContext>,
    pub work_surface: Option<&'a mut dyn crate::config::CommandWorkSurfaceContext>,
    pub navigation: Option<&'a mut dyn crate::navigation::CommandNavigationContext>,
    pub voice_input: Option<&'a mut dyn crate::navigation::CommandVoiceInputContext>,
}

impl<'a> CommandContexts<'a> {
    pub fn empty() -> Self {
        Self {
            session: None,
            model: None,
            cost: None,
            mode_policy: None,
            system_prompt: None,
            skills: None,
            workspace: None,
            presentation: None,
            media: None,
            memory: None,
            project: None,
            skill_group: None,
            plugin: None,
            lifecycle: None,
            control: None,
            export: None,
            structcopy: None,
            debug_receipts: None,
            debug_change: None,
            debug_history: None,
            debug_diff: None,
            debug_undo: None,
            debug_diagnostics: None,
            permissions: None,
            config_status: None,
            config: None,
            pet: None,
            trust: None,
            login: None,
            work_surface: None,
            navigation: None,
            voice_input: None,
        }
    }

    pub fn into_parts(self) -> ContextParts<'a> {
        ContextParts {
            session: self.session,
            model: self.model,
            cost: self.cost,
            mode_policy: self.mode_policy,
            system_prompt: self.system_prompt,
            skills: self.skills,
            workspace: self.workspace,
            presentation: self.presentation,
            media: self.media,
            memory: self.memory,
            project: self.project,
            skill_group: self.skill_group,
            plugin: self.plugin,
            lifecycle: self.lifecycle,
            control: self.control,
            export: self.export,
            structcopy: self.structcopy,
            debug_receipts: self.debug_receipts,
            debug_change: self.debug_change,
            debug_history: self.debug_history,
            debug_diff: self.debug_diff,
            debug_undo: self.debug_undo,
            debug_diagnostics: self.debug_diagnostics,
            permissions: self.permissions,
            config_status: self.config_status,
            config: self.config,
            pet: self.pet,
            trust: self.trust,
            login: self.login,
            work_surface: self.work_surface,
            navigation: self.navigation,
            voice_input: self.voice_input,
        }
    }

    pub fn with_config(mut self, value: &'a mut dyn crate::config::CommandConfigContext) -> Self {
        assert!(
            self.config.replace(value).is_none(),
            "config facet already set"
        );
        self
    }

    pub fn with_pet(mut self, value: &'a mut dyn crate::config::CommandPetContext) -> Self {
        assert!(self.pet.replace(value).is_none(), "pet facet already set");
        self
    }

    pub fn with_trust(mut self, value: &'a mut dyn crate::config::CommandTrustContext) -> Self {
        assert!(
            self.trust.replace(value).is_none(),
            "trust facet already set"
        );
        self
    }

    pub fn with_login(mut self, value: &'a mut dyn crate::config::CommandLoginContext) -> Self {
        assert!(
            self.login.replace(value).is_none(),
            "login facet already set"
        );
        self
    }

    pub fn with_work_surface(
        mut self,
        value: &'a mut dyn crate::config::CommandWorkSurfaceContext,
    ) -> Self {
        assert!(
            self.work_surface.replace(value).is_none(),
            "work-surface facet already set"
        );
        self
    }

    pub fn with_navigation(
        mut self,
        value: &'a mut dyn crate::navigation::CommandNavigationContext,
    ) -> Self {
        assert!(
            self.navigation.replace(value).is_none(),
            "navigation facet already set"
        );
        self
    }

    pub fn with_voice_input(
        mut self,
        value: &'a mut dyn crate::navigation::CommandVoiceInputContext,
    ) -> Self {
        assert!(
            self.voice_input.replace(value).is_none(),
            "voice-input facet already set"
        );
        self
    }

    pub fn with_permissions(mut self, value: &'a mut dyn CommandPermissionsContext) -> Self {
        assert!(
            self.permissions.replace(value).is_none(),
            "permissions facet already set"
        );
        self
    }
    pub fn with_config_status(mut self, value: &'a mut dyn CommandConfigStatusContext) -> Self {
        assert!(
            self.config_status.replace(value).is_none(),
            "config status facet already set"
        );
        self
    }

    pub fn with_session(mut self, value: &'a mut dyn CommandSessionContext) -> Self {
        assert!(
            self.session.replace(value).is_none(),
            "session facet already set"
        );
        self
    }

    pub fn with_model(mut self, value: &'a mut dyn CommandModelContext) -> Self {
        assert!(
            self.model.replace(value).is_none(),
            "model facet already set"
        );
        self
    }

    pub fn with_cost(mut self, value: &'a mut dyn CommandCostContext) -> Self {
        assert!(self.cost.replace(value).is_none(), "cost facet already set");
        self
    }

    pub fn with_mode_policy(mut self, value: &'a mut dyn CommandModePolicyContext) -> Self {
        assert!(
            self.mode_policy.replace(value).is_none(),
            "mode-policy facet already set"
        );
        self
    }

    pub fn with_system_prompt(mut self, value: &'a mut dyn CommandSystemPromptContext) -> Self {
        assert!(
            self.system_prompt.replace(value).is_none(),
            "system-prompt facet already set"
        );
        self
    }

    pub fn with_skills(mut self, value: &'a mut dyn CommandSkillsContext) -> Self {
        assert!(
            self.skills.replace(value).is_none(),
            "skills facet already set"
        );
        self
    }

    pub fn with_workspace(mut self, value: &'a mut dyn CommandWorkspaceContext) -> Self {
        assert!(
            self.workspace.replace(value).is_none(),
            "workspace facet already set"
        );
        self
    }

    pub fn with_presentation(mut self, value: &'a mut dyn CommandPresentationContext) -> Self {
        assert!(
            self.presentation.replace(value).is_none(),
            "presentation facet already set"
        );
        self
    }

    pub fn with_media(mut self, value: &'a mut dyn CommandMediaContext) -> Self {
        assert!(
            self.media.replace(value).is_none(),
            "media facet already set"
        );
        self
    }

    pub fn with_memory(mut self, value: &'a mut dyn CommandMemoryContext) -> Self {
        assert!(
            self.memory.replace(value).is_none(),
            "memory facet already set"
        );
        self
    }

    pub fn with_project(mut self, value: &'a mut dyn CommandProjectContext) -> Self {
        assert!(
            self.project.replace(value).is_none(),
            "project facet already set"
        );
        self
    }

    pub fn with_skill_group(mut self, value: &'a mut dyn CommandSkillGroupContext) -> Self {
        assert!(
            self.skill_group.replace(value).is_none(),
            "skill-group facet already set"
        );
        self
    }

    pub fn with_plugin(mut self, value: &'a mut dyn CommandPluginContext) -> Self {
        assert!(
            self.plugin.replace(value).is_none(),
            "plugin facet already set"
        );
        self
    }

    pub fn with_lifecycle(mut self, value: &'a mut dyn CommandSessionLifecycleContext) -> Self {
        assert!(
            self.lifecycle.replace(value).is_none(),
            "lifecycle facet already set"
        );
        self
    }

    pub fn with_control(mut self, value: &'a mut dyn CommandSessionControlContext) -> Self {
        assert!(
            self.control.replace(value).is_none(),
            "control facet already set"
        );
        self
    }

    pub fn with_structcopy(mut self, value: &'a mut dyn CommandSessionStructcopyContext) -> Self {
        assert!(
            self.structcopy.replace(value).is_none(),
            "structcopy facet already set"
        );
        self
    }

    pub fn with_export(mut self, value: &'a mut dyn CommandSessionExportContext) -> Self {
        assert!(
            self.export.replace(value).is_none(),
            "export facet already set"
        );
        self
    }

    pub fn with_debug_receipts(mut self, value: &'a mut dyn CommandDebugReceiptsContext) -> Self {
        assert!(
            self.debug_receipts.replace(value).is_none(),
            "debug_receipts facet already set"
        );
        self
    }

    pub fn with_debug_change(mut self, value: &'a mut dyn CommandDebugChangeContext) -> Self {
        assert!(
            self.debug_change.replace(value).is_none(),
            "debug_change facet already set"
        );
        self
    }

    pub fn with_debug_history(mut self, value: &'a mut dyn CommandDebugHistoryContext) -> Self {
        assert!(
            self.debug_history.replace(value).is_none(),
            "debug_history facet already set"
        );
        self
    }

    pub fn with_debug_diff(mut self, value: &'a mut dyn CommandDebugDiffContext) -> Self {
        assert!(
            self.debug_diff.replace(value).is_none(),
            "debug_diff facet already set"
        );
        self
    }

    pub fn with_debug_undo(mut self, value: &'a mut dyn CommandDebugUndoContext) -> Self {
        assert!(
            self.debug_undo.replace(value).is_none(),
            "debug_undo facet already set"
        );
        self
    }

    pub fn with_debug_diagnostics(
        mut self,
        value: &'a mut dyn CommandDebugDiagnosticsContext,
    ) -> Self {
        assert!(
            self.debug_diagnostics.replace(value).is_none(),
            "debug diagnostics facet already set"
        );
        self
    }
}

impl Default for CommandContexts<'_> {
    fn default() -> Self {
        Self::empty()
    }
}
