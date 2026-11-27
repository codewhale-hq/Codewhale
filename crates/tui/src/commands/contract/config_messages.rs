//! Host localization keys for the portable config group (FEAT-028).
//!
//! The command renders `ConfigMessage::Keyed` through the presentation shape;
//! this resolver maps each stable snake_case key to its `MessageId`.
use codewhale_localization::MessageId;

pub(in crate::commands) fn resolve(key: &str) -> Option<MessageId> {
    Some(match key {
        "config_audit_notifications" => MessageId::ConfigAuditNotifications,
        "config_audit_prompt_suggestion" => MessageId::ConfigAuditPromptSuggestion,
        "config_audit_search_provider" => MessageId::ConfigAuditSearchProvider,
        "config_command_invalid_value" => MessageId::ConfigCommandInvalidValue,
        "config_command_source" => MessageId::ConfigCommandSource,
        "config_default_value" => MessageId::ConfigDefaultValue,
        "config_notification_updated" => MessageId::ConfigNotificationUpdated,
        "config_notifications_set_hint" => MessageId::ConfigNotificationsSetHint,
        "config_prompt_suggestion_updated" => MessageId::ConfigPromptSuggestionUpdated,
        "config_scope_saved" => MessageId::ConfigScopeSaved,
        "config_scope_session" => MessageId::ConfigScopeSession,
        "config_search_updated" => MessageId::ConfigSearchUpdated,
        "contextual_tips_not_saved" => MessageId::ContextualTipsNotSaved,
        "help_usage_label" => MessageId::HelpUsageLabel,
        "pet_mode_off" => MessageId::PetModeOff,
        "pet_mode_on" => MessageId::PetModeOn,
        "setting_subject_mode" => MessageId::SettingSubjectMode,
        "setting_subject_model" => MessageId::SettingSubjectModel,
        "settings_config_file" => MessageId::SettingsConfigFile,
        "settings_title" => MessageId::SettingsTitle,
        "setting_subject_permissions" => MessageId::SettingSubjectPermissions,
        "setting_subject_provider" => MessageId::SettingSubjectProvider,
        "setting_subject_thinking" => MessageId::SettingSubjectThinking,
        "startup_default_not_saved" => MessageId::StartupDefaultNotSaved,
        _ => return None,
    })
}
