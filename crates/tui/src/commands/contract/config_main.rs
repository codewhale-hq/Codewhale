//! Host adapters for the config command group (FEAT-028 Phase 4).
//!
//! Sole host owner of concrete machinery for the config-group commands. Each
//! delegate calls the existing command implementation and converts its TUI
//! `CommandResult` (message + `AppAction`) into a portable `ConfigReply`
//! (message + `ConfigAction`). The portable handlers depend only on the
//! `CommandConfigContext`/`CommandPetContext`/`CommandTrustContext` facets.

use super::SharedCommandHost;
use crate::tui::app::ScreenMode;
use codewhale_command_contract::config::{
    CommandAccountState, CommandClaudeApplyOutcome, CommandClaudeArtifacts, CommandClaudePlan,
    CommandConfigContext, CommandLoginContext, CommandLoginStatus, CommandLogoutOutcome,
    CommandPetContext, CommandPetControl, CommandPetLabel, CommandTrustContext,
    CommandWorkSurfaceContext, CommandWorkflowSummary, ConfigReply, LogoutFailure, TrustPathError,
};
use codewhale_command_contract::types::CommandScreenMode;
use codewhale_command_contract::types::{CommandRailPanel, CommandWorkSurfacePlacement};
use std::path::PathBuf;

pub(crate) struct ConfigAdapter<'a> {
    pub(crate) host: SharedCommandHost<'a>,
}

impl CommandConfigContext for ConfigAdapter<'_> {
    fn translation_enabled(&self) -> bool {
        self.host.app.borrow().translation_enabled
    }

    fn set_translation_enabled(&mut self, enabled: bool) {
        self.host.app.borrow_mut().translation_enabled = enabled;
    }

    fn config_path(&self) -> Option<PathBuf> {
        self.host.app.borrow().config_path.clone()
    }

    fn audit(
        &mut self,
    ) -> Result<codewhale_command_contract::config::CommandConfigAuditReport, String> {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::config_editability_audit(&app)
    }

    fn workflow_summary(&mut self) -> CommandWorkflowSummary {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::workflow_summary(&app)
    }

    fn subagents_status(
        &mut self,
    ) -> Result<codewhale_command_contract::config::CommandSubagentsReport, String> {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::subagents_status(&app)
    }

    fn subagent(&mut self, key: &str) -> ConfigReply {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::show_subagents_setting(&app, key)
    }

    fn set_subagent(&mut self, key: &str, value: &str, persist: bool) -> ConfigReply {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_host::set_subagents_config_value(&mut app, key, value, persist)
    }

    fn search_provider_status(&mut self) -> ConfigReply {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::show_single_setting(&app, "search.provider")
    }

    fn search_provider_names(&self) -> &'static str {
        crate::config::SearchProvider::names_hint()
    }

    fn set_search_provider(&mut self, value: &str, persist: bool) -> ConfigReply {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_host::set_search_provider(&mut app, value, persist)
    }

    fn notifications_status(&mut self) -> ConfigReply {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::show_notifications_status(&app)
    }

    fn notification(&mut self, key: &str) -> ConfigReply {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::show_notifications_setting(&app, key)
    }

    fn set_notification(&mut self, key: &str, value: &str, persist: bool) -> ConfigReply {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_host::set_notifications_value(&mut app, key, value, persist)
    }

    fn apply_preset(&mut self, name: &str, persist: bool) -> ConfigReply {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_host::apply_preset(&mut app, name, persist)
    }

    fn get_value(&mut self, key: &str) -> ConfigReply {
        let app = self.host.app.borrow();
        crate::commands::config_main_host::show_single_setting(&app, key)
    }

    fn set_value(&mut self, key: &str, value: &str, persist: bool) -> ConfigReply {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_host::set_config_value(&mut app, key, value, persist)
    }

    fn show_settings(
        &mut self,
    ) -> Result<codewhale_command_contract::config::CommandSettingsReport, String> {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_host::show_settings(&mut app)
    }

    fn theme_schema(&self) -> String {
        codewhale_palette::user_theme_schema_json().to_string()
    }

    fn themes_dir(&self) -> Result<String, String> {
        codewhale_palette::user_themes_dir().map(|path| path.display().to_string())
    }

    fn verbose_enabled(&self) -> bool {
        self.host.app.borrow().verbose_transcript
    }

    fn set_verbose_enabled(&mut self, enabled: bool) {
        let mut app = self.host.app.borrow_mut();
        app.verbose_transcript = enabled;
        app.mark_history_updated();
    }

    fn screen_mode(&self) -> CommandScreenMode {
        match self.host.app.borrow().screen_mode {
            ScreenMode::Fullscreen => CommandScreenMode::Fullscreen,
            ScreenMode::Inline => CommandScreenMode::Inline,
        }
    }

    fn build_plan(&mut self) -> Option<CommandClaudePlan> {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_import_host::build_plan(&mut app)
    }

    fn write_artifacts(&mut self, report: &str, bundle: Option<&str>) -> CommandClaudeArtifacts {
        crate::commands::config_main_import_host::write_artifacts(report, bundle)
    }

    fn apply_import(
        &mut self,
        plan: &CommandClaudePlan,
        report_path: &std::path::Path,
        wrote_report: bool,
    ) -> CommandClaudeApplyOutcome {
        let mut app = self.host.app.borrow_mut();
        crate::commands::config_main_import_host::apply_plan(
            &mut app,
            plan,
            report_path,
            wrote_report,
        )
    }

    fn display_path(&self, path: &std::path::Path) -> String {
        crate::utils::display_path(path).to_string()
    }
}

pub(crate) struct PetAdapter<'a> {
    pub(crate) host: SharedCommandHost<'a>,
}

impl CommandPetContext for PetAdapter<'_> {
    fn enabled(&self) -> bool {
        self.host.app.borrow().pet_watch.enabled
    }

    fn is_open(&self) -> bool {
        let app = self.host.app.borrow();
        crate::tui::pet_watch::is_open(&app)
    }

    fn status(&self) -> String {
        self.host.app.borrow().pet_watch.status()
    }

    fn avatar_choices(&self) -> String {
        self.host.app.borrow().pet_watch.avatar_choices()
    }

    fn select_avatar(&mut self, key: &str) -> bool {
        let mut app = self.host.app.borrow_mut();
        let ok = app.pet_watch.select_avatar(key);
        if ok {
            app.needs_redraw = true;
        }
        ok
    }

    fn preview_avatar(&mut self, name: &str, view: bool) -> bool {
        let mut app = self.host.app.borrow_mut();
        let ok = app.pet_watch.preview_avatar(name, view);
        if ok {
            app.needs_redraw = true;
        }
        ok
    }

    fn set_enabled(&mut self, enabled: bool, persist: bool) {
        let mut app = self.host.app.borrow_mut();
        crate::tui::pet_watch::set_enabled(&mut app, enabled);
        if persist {
            app.startup_defaults
                .spawn(crate::tui::startup_defaults::StartupDefaults::pet_mode(
                    enabled,
                ));
        }
    }

    fn set_sound(&mut self, enabled: bool) {
        let mut app = self.host.app.borrow_mut();
        app.pet_watch.set_sound(enabled);
        app.needs_redraw = true;
    }

    fn sound_label(&self) -> String {
        let app = self.host.app.borrow();
        codewhale_localization::tr(app.ui_locale, app.pet_watch.sound_label()).into_owned()
    }

    fn export(&mut self) -> bool {
        self.host.app.borrow_mut().pet_watch.export()
    }

    fn open_habitat(&mut self) {
        let mut app = self.host.app.borrow_mut();
        crate::tui::pet_watch::open_habitat(&mut app);
    }

    fn queue(&mut self, control: CommandPetControl) -> bool {
        use crate::tui::pet_watch::{BodyStyle, Control, TextLanguage};
        let control = match control {
            CommandPetControl::Sound => Control::Sound,
            CommandPetControl::Browser => Control::Browser,
            CommandPetControl::Window => Control::Window,
            CommandPetControl::Select => Control::Select,
            CommandPetControl::FocusAgents => Control::FocusAgents,
            CommandPetControl::Appearance { body, language } => Control::Appearance {
                body: body.and_then(|body| BodyStyle::parse(body.as_str())),
                language: language.and_then(|language| TextLanguage::parse(language.as_str())),
            },
        };
        let mut app = self.host.app.borrow_mut();
        crate::tui::pet_watch::command(&mut app, control)
    }

    fn label(&self, which: CommandPetLabel) -> String {
        use codewhale_localization::MessageId;
        let app = self.host.app.borrow();
        let id = match which {
            CommandPetLabel::ModeOn => MessageId::PetModeOn,
            CommandPetLabel::ModeOff => MessageId::PetModeOff,
            CommandPetLabel::ModeOnLabel => MessageId::PetModeOnLabel,
            CommandPetLabel::ModeOffLabel => MessageId::PetModeOffLabel,
            CommandPetLabel::ViewOpen => MessageId::PetViewOpen,
            CommandPetLabel::ViewClosed => MessageId::PetViewClosed,
            CommandPetLabel::HabitatQueued => MessageId::PetHabitatQueued,
            CommandPetLabel::ExportQueued => MessageId::PetWatchExportQueued,
            CommandPetLabel::ExportUnavailable => MessageId::PetWatchExportUnavailable,
            CommandPetLabel::SoundOn => MessageId::PetWatchSoundOn,
            CommandPetLabel::WatchUnavailable => MessageId::PetWatchUnavailable,
        };
        codewhale_localization::tr(app.ui_locale, id).into_owned()
    }
}

pub(crate) struct TrustAdapter<'a> {
    pub(crate) host: SharedCommandHost<'a>,
}

impl CommandTrustContext for TrustAdapter<'_> {
    fn trust_mode(&self) -> bool {
        self.host.app.borrow().trust_mode
    }

    fn trusted_paths(&self) -> Vec<PathBuf> {
        let workspace = self.host.app.borrow().workspace.clone();
        crate::workspace_trust::WorkspaceTrust::load_for(&workspace)
            .paths()
            .to_vec()
    }

    fn add_trusted_path(&mut self, raw: &str) -> Result<PathBuf, TrustPathError> {
        let workspace = self.host.app.borrow().workspace.clone();
        let path = PathBuf::from(crate::commands::config_main_host::expand_tilde(raw));
        if !path.exists() {
            return Err(TrustPathError::NotFound(path));
        }
        crate::workspace_trust::add(&workspace, &path)
            .map_err(|err| TrustPathError::UpdateFailed(err.to_string()))
    }

    fn remove_trusted_path(&mut self, raw: &str) -> Result<(PathBuf, bool), TrustPathError> {
        let workspace = self.host.app.borrow().workspace.clone();
        let path = PathBuf::from(crate::commands::config_main_host::expand_tilde(raw));
        crate::workspace_trust::remove(&workspace, &path)
            .map(|removed| (path, removed))
            .map_err(|err| TrustPathError::UpdateFailed(err.to_string()))
    }

    fn usage_label(&self) -> String {
        let app = self.host.app.borrow();
        codewhale_localization::tr(
            app.ui_locale,
            codewhale_localization::MessageId::HelpUsageLabel,
        )
        .into_owned()
    }
}

pub(crate) struct LoginAdapter<'a> {
    pub(crate) host: SharedCommandHost<'a>,
}

fn is_enabled_plugin_provider(app: &crate::tui::app::App, provider: &str) -> bool {
    app.plugin_registry
        .active_plugins()
        .into_iter()
        .any(|plugin| plugin.manifest.providers.contains_key(provider))
}

impl CommandLoginContext for LoginAdapter<'_> {
    fn login_status(&self) -> CommandLoginStatus {
        use codewhale_secrets::account::{
            ACCOUNT_API_BASE_ENV, AccountSessionState, AccountSessionStore,
            DEFAULT_ACCOUNT_API_BASE, secure_account_session_secrets,
        };
        let app = self.host.app.borrow();
        let api_base = std::env::var(ACCOUNT_API_BASE_ENV)
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_ACCOUNT_API_BASE.to_string());
        let account = match secure_account_session_secrets() {
            Ok(secrets) => {
                match AccountSessionStore::new(secrets, None, &api_base)
                    .runtime_info_at(chrono::Utc::now())
                {
                    Ok(info) => match info.state {
                        AccountSessionState::SignedOut => CommandAccountState::SignedOut,
                        AccountSessionState::Authenticated => CommandAccountState::Authenticated,
                        AccountSessionState::OfflineCached => CommandAccountState::OfflineCached,
                        AccountSessionState::Expired => CommandAccountState::Expired,
                        AccountSessionState::Revoked => CommandAccountState::Revoked,
                    },
                    Err(error) => CommandAccountState::Unavailable(error.to_string()),
                }
            }
            Err(error) => CommandAccountState::Unavailable(error.to_string()),
        };
        CommandLoginStatus {
            account,
            api_base,
            provider: app.provider_identity_for_persistence().to_string(),
        }
    }

    fn logout(&mut self) -> Result<CommandLogoutOutcome, LogoutFailure> {
        let mut app = self.host.app.borrow_mut();
        let provider = app.provider_identity_for_persistence().to_string();
        match crate::config::clear_active_provider_api_key(&provider) {
            Ok(()) => {
                app.onboarding = crate::tui::app::OnboardingState::Provider;
                app.onboarding_needs_api_key = true;
                app.onboarding_provider = app.api_provider;
                app.onboarding_missing_key_recovery = true;
                app.api_key_env_only = false;
                let account = crate::commands::config_main_host::clear_local_account_session();
                let legacy_sandbox = crate::commands::config_main_host::clear_legacy_sandbox_slot();
                Ok(CommandLogoutOutcome {
                    provider,
                    account,
                    legacy_sandbox,
                })
            }
            Err(error) => Err(LogoutFailure {
                provider,
                error: error.to_string(),
            }),
        }
    }
    fn connect(
        &mut self,
        provider: Option<&str>,
    ) -> codewhale_command_contract::config::ConfigReply {
        use codewhale_command_contract::config::ConfigReply;
        let app = self.host.app.borrow();
        match provider {
            None => ConfigReply::action(
                codewhale_command_contract::outcome::ConfigAction::OpenProviderPicker,
            ),
            Some(provider) if is_enabled_plugin_provider(&app, provider) => ConfigReply::action(
                codewhale_command_contract::outcome::ConfigAction::StartPluginLogin {
                    provider: provider.to_owned(),
                },
            ),
            Some(other) => ConfigReply::error(format!(
                "Usage: /connect [<provider>]\nUnknown or inactive plugin provider: {other}\nCodewhale account sign-in is `/login`."
            )),
        }
    }

    fn disconnect(&mut self, provider: &str) -> codewhale_command_contract::config::ConfigReply {
        use codewhale_command_contract::config::ConfigReply;
        let app = self.host.app.borrow();
        if is_enabled_plugin_provider(&app, provider) {
            ConfigReply::action(
                codewhale_command_contract::outcome::ConfigAction::StartPluginLogout {
                    provider: provider.to_owned(),
                },
            )
        } else {
            ConfigReply::error(format!(
                "Usage: /disconnect <provider>\nUnknown or inactive plugin provider: {provider}\nSigning out of Codewhale is `/logout`."
            ))
        }
    }
}

pub(crate) struct WorkSurfaceAdapter<'a> {
    pub(crate) host: SharedCommandHost<'a>,
}

fn to_placement(
    placement: crate::tui::work_surface::WorkSurfacePlacement,
) -> CommandWorkSurfacePlacement {
    use crate::tui::work_surface::WorkSurfacePlacement as P;
    match placement {
        P::Bottom => CommandWorkSurfacePlacement::Bottom,
        P::Top => CommandWorkSurfacePlacement::Top,
        P::Left => CommandWorkSurfacePlacement::Left,
        P::Right => CommandWorkSurfacePlacement::Right,
        P::Off => CommandWorkSurfacePlacement::Off,
    }
}

fn from_placement(
    placement: CommandWorkSurfacePlacement,
) -> crate::tui::work_surface::WorkSurfacePlacement {
    use crate::tui::work_surface::WorkSurfacePlacement as P;
    match placement {
        CommandWorkSurfacePlacement::Bottom => P::Bottom,
        CommandWorkSurfacePlacement::Top => P::Top,
        CommandWorkSurfacePlacement::Left => P::Left,
        CommandWorkSurfacePlacement::Right => P::Right,
        CommandWorkSurfacePlacement::Off => P::Off,
    }
}

fn to_rail_panel(panel: crate::tui::work_surface::RailPanel) -> CommandRailPanel {
    use crate::tui::work_surface::RailPanel as P;
    match panel {
        P::Agents => CommandRailPanel::Agents,
        P::Tasks => CommandRailPanel::Tasks,
        P::Background => CommandRailPanel::Background,
        P::Terminal => CommandRailPanel::Terminal,
        P::Files => CommandRailPanel::Files,
        P::Notepad => CommandRailPanel::Notepad,
        P::Context => CommandRailPanel::Context,
        P::Git => CommandRailPanel::Git,
        P::Price => CommandRailPanel::Price,
    }
}

fn from_rail_panel(panel: CommandRailPanel) -> crate::tui::work_surface::RailPanel {
    use crate::tui::work_surface::RailPanel as P;
    match panel {
        CommandRailPanel::Agents => P::Agents,
        CommandRailPanel::Tasks => P::Tasks,
        CommandRailPanel::Background => P::Background,
        CommandRailPanel::Terminal => P::Terminal,
        CommandRailPanel::Files => P::Files,
        CommandRailPanel::Notepad => P::Notepad,
        CommandRailPanel::Context => P::Context,
        CommandRailPanel::Git => P::Git,
        CommandRailPanel::Price => P::Price,
    }
}

impl CommandWorkSurfaceContext for WorkSurfaceAdapter<'_> {
    fn placement(&self) -> CommandWorkSurfacePlacement {
        to_placement(self.host.app.borrow().work_surface.placement)
    }

    fn panel(&self) -> CommandRailPanel {
        to_rail_panel(self.host.app.borrow().work_surface.panel)
    }

    fn effective_placement(&self) -> CommandWorkSurfacePlacement {
        to_placement(self.host.app.borrow().work_surface.effective_placement())
    }

    fn area_present(&self) -> bool {
        self.host.app.borrow().work_surface.last_area.is_some()
    }

    fn panel_title(&self, panel: CommandRailPanel) -> String {
        from_rail_panel(panel).title().to_string()
    }

    fn set_placement(
        &mut self,
        placement: CommandWorkSurfacePlacement,
        persist: bool,
    ) -> ConfigReply {
        let mut app = self.host.app.borrow_mut();
        app.work_surface.placement = from_placement(placement);
        app.work_surface.focused = false;
        if persist {
            let result = crate::commands::config_main_host::set_config_value(
                &mut app,
                "work_surface_placement",
                from_placement(placement).as_setting(),
                true,
            );
            return result;
        }
        ConfigReply::ok()
    }

    fn set_panel(&mut self, panel: CommandRailPanel, persist: bool) -> ConfigReply {
        let mut app = self.host.app.borrow_mut();
        crate::tui::work_surface::select_dock_panel(&mut app, from_rail_panel(panel));
        if persist {
            let result = crate::commands::config_main_host::set_config_value(
                &mut app,
                "rail_panel",
                from_rail_panel(panel).as_setting(),
                true,
            );
            return result;
        }
        ConfigReply::ok()
    }

    fn mark_redraw(&mut self) {
        self.host.app.borrow_mut().needs_redraw = true;
    }
}
