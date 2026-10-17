use super::{SharedCommandHost, to_command_mode};
use crate::tui::app::AppModeUi;
use crate::tui::views::{HelpView, ModalKind};
use codewhale_command_contract::navigation::{
    CommandNavigationContext, NavigationBuiltinHelp, NavigationHelpTopic, NavigationLinks,
    NavigationOverview, NavigationProviderKind, NavigationProviderLinks, NavigationSkill,
    NavigationUserHelp,
};
use codewhale_localization::MessageId;

pub(super) struct NavigationAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl CommandNavigationContext for NavigationAdapter<'_> {
    fn help_topic(&self, topic: &str) -> NavigationHelpTopic {
        let app = self.host.app.borrow();
        let builtin = crate::commands::get_command_info(topic);
        crate::commands::user_registry::with_registry_for_app(&app, |registry| {
            NavigationHelpTopic {
                user: registry.get(topic).map(|command| NavigationUserHelp {
                    name: command.name.clone(),
                    description: command.description.clone(),
                    usage: command.display_usage().map(str::to_owned),
                    aliases: command.aliases.clone(),
                }),
                shadowed_names: builtin
                    .map(|command| {
                        command
                            .aliases
                            .iter()
                            .filter(|alias| registry.get(alias).is_some())
                            .map(|alias| (*alias).to_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
                builtin: builtin.map(|command| NavigationBuiltinHelp {
                    name: command.name.to_owned(),
                    usage: command.usage.to_owned(),
                    description: command.description_for(app.ui_locale).into_owned(),
                    aliases: command
                        .aliases
                        .iter()
                        .map(|alias| (*alias).to_owned())
                        .collect(),
                }),
            }
        })
    }

    fn cached_skills(&self) -> Vec<NavigationSkill> {
        self.host
            .app
            .borrow()
            .cached_skills
            .iter()
            .map(|(name, description)| NavigationSkill {
                name: name.clone(),
                description: description.clone(),
            })
            .collect()
    }

    fn help_is_open(&self) -> bool {
        self.host.app.borrow().view_stack.top_kind() == Some(ModalKind::Help)
    }

    fn open_help(&mut self) {
        let mut app = self.host.app.borrow_mut();
        let help = HelpView::new_for_app(&app, false).with_groups_expanded(app.help_expand_groups);
        app.view_stack.push(help);
    }

    fn home_is_blocked(&self) -> bool {
        self.host.app.borrow().session_transition_blocked()
    }

    fn restore_home(&mut self) {
        let mut app = self.host.app.borrow_mut();
        app.launch.workspace = app.workspace.clone();
        app.launch.restore_card();
        app.launch.visible = true;
        app.launch.return_to_session = true;
        app.needs_redraw = true;
    }

    fn overview(&self) -> NavigationOverview {
        let app = self.host.app.borrow();
        NavigationOverview {
            model: app.model.clone(),
            mode: to_command_mode(app.mode),
            mode_label: app.mode.display_name_localized(app.ui_locale).into_owned(),
            workspace: app.workspace.clone(),
            history_count: app.history.len(),
            total_tokens: app.session.displayed_total_conversation_tokens(),
            queued_count: app.queued_messages.len(),
            subagent_count: app.subagent_cache.len(),
            active_skill: app.active_skill.clone(),
        }
    }

    fn links(&self) -> NavigationLinks {
        let app = self.host.app.borrow();
        NavigationLinks {
            active_provider: app.api_provider.as_str().to_owned(),
            locale_tag: app.ui_locale.tag().to_owned(),
            providers: codewhale_config::provider::providers_sorted_for_display()
                .into_iter()
                .map(|provider| {
                    let links = provider.credential_help();
                    NavigationProviderLinks {
                        id: provider.id().to_owned(),
                        name: provider.display_name().to_owned(),
                        credential_url: links.credential_url.map(str::to_owned),
                        docs_url: links.docs_url.map(str::to_owned),
                        guidance: links.guidance.to_owned(),
                        env_names: provider
                            .env_vars()
                            .iter()
                            .map(|name| (*name).to_owned())
                            .collect(),
                        kind: if provider.kind() == codewhale_config::ProviderKind::Moonshot {
                            NavigationProviderKind::Moonshot
                        } else {
                            NavigationProviderKind::Other
                        },
                    }
                })
                .collect(),
            kimi_route: crate::config::DEFAULT_KIMI_CODE_BASE_URL.to_owned(),
            kimi_console: crate::config::KIMI_CODE_MEMBERSHIP_PLAN_CONSOLE_URL.to_owned(),
        }
    }
}

pub(super) struct VoiceInputAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl codewhale_command_contract::navigation::CommandVoiceInputContext for VoiceInputAdapter<'_> {
    fn recorder_available(&self) -> bool {
        crate::voice::is_available()
    }
    fn voice_enabled(&self) -> bool {
        self.host.app.borrow().voice_enabled
    }
    fn set_voice_enabled(&mut self, enabled: bool) {
        self.host.app.borrow_mut().voice_enabled = enabled;
    }
    fn send_enabled(&self) -> bool {
        self.host.app.borrow().voice_send_enabled
    }
    fn set_send_enabled(&mut self, enabled: bool) {
        self.host.app.borrow_mut().voice_send_enabled = enabled;
    }
    fn control_enabled(&self) -> bool {
        self.host.app.borrow().voice_control_enabled
    }
    fn set_control_enabled(&mut self, enabled: bool) {
        self.host.app.borrow_mut().voice_control_enabled = enabled;
    }
}

pub(super) fn anchors_path(workspace: &std::path::Path) -> std::path::PathBuf {
    let primary = workspace.join(".codewhale").join("anchors.md");
    if primary.symlink_metadata().is_ok() || workspace.join(".codewhale").is_symlink() {
        return primary;
    }
    workspace.join(".deepseek").join("anchors.md")
}

pub(super) fn read_anchor_document(workspace: &std::path::Path) -> Result<Option<String>, String> {
    match crate::fs_confined::read_to_string(workspace, &anchors_path(workspace)) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn append_anchor_bytes(
    workspace: &std::path::Path,
    parts: &[&[u8]],
) -> Result<std::path::PathBuf, codewhale_command_contract::navigation::AnchorWriteError> {
    use codewhale_command_contract::navigation::AnchorWriteError;
    use std::io::Write;
    let path = anchors_path(workspace);
    let mut file = crate::fs_confined::open_append(workspace, &path)
        .map_err(|error| AnchorWriteError::Open(error.to_string()))?;
    for bytes in parts {
        file.write_all(bytes)
            .map_err(|error| AnchorWriteError::Write(error.to_string()))?;
    }
    Ok(path)
}

pub(super) fn replace_anchor_document(
    workspace: &std::path::Path,
    bytes: &[u8],
) -> Result<(), String> {
    crate::fs_confined::write(workspace, &anchors_path(workspace), bytes)
        .map_err(|error| error.to_string())
}

pub(super) fn resolve_message(key: &str) -> Option<MessageId> {
    Some(match key {
        "voice_disabled" => MessageId::VoiceDisabled,
        "voice_enabled" => MessageId::VoiceEnabled,
        "voice_err_no_recorder" => MessageId::VoiceErrNoRecorder,
        "voice_send_enabled" => MessageId::VoiceSendEnabled,
        "voice_send_disabled" => MessageId::VoiceSendDisabled,
        "voice_control_enabled" => MessageId::VoiceControlEnabled,
        "voice_control_disabled" => MessageId::VoiceControlDisabled,
        "cmd_translate_on" => MessageId::CmdTranslateOn,
        "cmd_translate_off" => MessageId::CmdTranslateOff,
        "workspace_current" => MessageId::WorkspaceCurrent,
        "workspace_not_found" => MessageId::WorkspaceNotFound,
        "workspace_not_directory" => MessageId::WorkspaceNotDirectory,
        "workspace_switching" => MessageId::WorkspaceSwitching,
        "workspace_home_unresolved" => MessageId::WorkspaceHomeUnresolved,
        "help_aliases_label" => MessageId::HelpAliasesLabel,
        "help_unknown_command" => MessageId::HelpUnknownCommand,
        "config_help_discoverable" => MessageId::ConfigHelpDiscoverable,
        "home_navigation_busy" => MessageId::HomeNavigationBusy,
        "home_dashboard_title" => MessageId::HomeDashboardTitle,
        "home_model" => MessageId::HomeModel,
        "home_mode" => MessageId::HomeMode,
        "home_workspace" => MessageId::HomeWorkspace,
        "home_history" => MessageId::HomeHistory,
        "home_tokens" => MessageId::HomeTokens,
        "home_queued" => MessageId::HomeQueued,
        "home_subagents" => MessageId::HomeSubagents,
        "home_skill" => MessageId::HomeSkill,
        "home_quick_actions" => MessageId::HomeQuickActions,
        "home_quick_workspace" => MessageId::HomeQuickWorkspace,
        "home_quick_restore" => MessageId::HomeQuickRestore,
        "home_quick_tokens" => MessageId::HomeQuickTokens,
        "home_quick_links" => MessageId::HomeQuickLinks,
        "home_quick_skills" => MessageId::HomeQuickSkills,
        "home_quick_config" => MessageId::HomeQuickConfig,
        "home_quick_settings" => MessageId::HomeQuickSettings,
        "home_quick_model" => MessageId::HomeQuickModel,
        "home_quick_subagents" => MessageId::HomeQuickSubagents,
        "home_quick_task_list" => MessageId::HomeQuickTaskList,
        "home_quick_help" => MessageId::HomeQuickHelp,
        "home_mode_tips" => MessageId::HomeModeTips,
        "home_agent_mode_tip" => MessageId::HomeAgentModeTip,
        "home_agent_mode_review_tip" => MessageId::HomeAgentModeReviewTip,
        "home_agent_mode_yolo_tip" => MessageId::HomeAgentModeYoloTip,
        "home_operate_mode_tip" => MessageId::HomeOperateModeTip,
        "home_operate_mode_fleet_tip" => MessageId::HomeOperateModeFleetTip,
        "home_plan_mode_tip" => MessageId::HomePlanModeTip,
        "home_plan_mode_checklist_tip" => MessageId::HomePlanModeChecklistTip,
        "links_project_title" => MessageId::LinksProjectTitle,
        "links_documentation" => MessageId::LinksDocumentation,
        "links_community" => MessageId::LinksCommunity,
        "links_git_hub" => MessageId::LinksGitHub,
        "links_managed_app" => MessageId::LinksManagedApp,
        "links_managed_app_note" => MessageId::LinksManagedAppNote,
        "links_title" => MessageId::LinksTitle,
        "links_dashboard" => MessageId::LinksDashboard,
        "links_docs" => MessageId::LinksDocs,
        "links_kimi_code_route_note" => MessageId::LinksKimiCodeRouteNote,
        "links_tip" => MessageId::LinksTip,
        _ => return None,
    })
}
