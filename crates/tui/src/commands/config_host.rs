//! Shared conversion between the config-group contract actions and the TUI
//! `AppAction` vocabulary (FEAT-028 D5).
//!
//! The portable handlers and the movable config closure depend only on
//! `codewhale-command-contract`. This adapter module is host-owned: it maps the
//! unified `ConfigAction` to the concrete `AppAction` the event loop applies,
//! and back for the host helpers that still return a TUI `CommandResult`.
//!

use crate::compaction::CompactionConfig;
use crate::tui::app::{AppAction, ScreenMode};
use codewhale_command_contract::config::{
    CommandCapabilityState, CommandCompactionConfig, CommandCompletionSound,
    CommandNotificationCondition, CommandNotificationEvent, CommandNotificationMethod,
    CommandNotificationUpdate, CommandSearchProvider, CommandSubagentCompletion,
    CommandSubagentRuntimeConfig,
};
use codewhale_command_contract::outcome::ConfigAction;
use codewhale_command_contract::types::{CommandMode, CommandProviderId, CommandScreenMode};
use codewhale_config::AppMode;
use codewhale_config::ProviderId;
use codewhale_config::notifications::{
    CompletionSound, NotificationCondition, NotificationConfigUpdate, NotificationEvent,
    NotificationMethod, SubagentCompletionNotification,
};
use codewhale_config::route::CapabilityState;
use codewhale_runtime::config::SearchProvider;

/// Convert a portable config action to the concrete TUI action it requests.
pub(crate) fn config_action_to_app_action(action: ConfigAction) -> AppAction {
    match action {
        ConfigAction::PermissionRulesChanged => AppAction::PermissionRulesChanged,
        ConfigAction::StartXaiDeviceLogin => AppAction::StartXaiDeviceLogin,
        ConfigAction::StartClaudeLogin => AppAction::StartClaudeLogin,
        ConfigAction::StartClaudeRevoke => AppAction::StartClaudeRevoke,
        ConfigAction::StartChatgptPkceLogin => AppAction::StartChatgptPkceLogin,
        ConfigAction::StartChatgptRevoke => AppAction::StartChatgptRevoke,
        ConfigAction::StartOrcarouterPkceLogin => AppAction::StartOrcarouterPkceLogin,
        ConfigAction::StartOrcarouterRevoke => AppAction::StartOrcarouterRevoke,
        ConfigAction::OpenConfigView => AppAction::OpenConfigView,
        ConfigAction::OpenModePicker => AppAction::OpenModePicker,
        ConfigAction::OpenThemePicker => AppAction::OpenThemePicker,
        ConfigAction::OpenStatusPicker => AppAction::OpenStatusPicker,
        ConfigAction::OpenProviderPicker => AppAction::OpenProviderPicker,
        ConfigAction::StartPluginLogin { provider } => AppAction::StartPluginLogin { provider },
        ConfigAction::StartPluginLogout { provider } => AppAction::StartPluginLogout { provider },
        ConfigAction::SetScreenMode(mode) => AppAction::SetScreenMode(to_screen_mode(mode)),
        ConfigAction::SetWorkspaceTrust { trusted, save } => {
            AppAction::SetWorkspaceTrust { trusted, save }
        }
        ConfigAction::ModeChanged(mode) => AppAction::ModeChanged(to_app_mode(mode)),
        ConfigAction::ApprovalPolicyChanged { policy } => {
            AppAction::ApprovalPolicyPersisted { policy }
        }
        ConfigAction::SwitchProvider { provider, model } => AppAction::SwitchProvider {
            provider: ProviderId::from(provider.0),
            model,
        },
        ConfigAction::CompactionChanged(config) => {
            AppAction::UpdateCompaction(to_compaction_config(config))
        }
        ConfigAction::StreamChunkTimeoutChanged(timeout) => {
            AppAction::UpdateStreamChunkTimeout(timeout)
        }
        ConfigAction::SubagentRuntimeChanged(config) => AppAction::UpdateSubagentRuntimeConfig {
            enabled: config.enabled,
            max_subagents: config.max_subagents,
            launch_concurrency: config.launch_concurrency,
            max_spawn_depth: config.max_spawn_depth,
            api_timeout_secs: config.api_timeout_secs,
            heartbeat_timeout_secs: config.heartbeat_timeout_secs,
        },
        ConfigAction::SearchProviderChanged(provider) => AppAction::UpdateSearchProvider {
            provider: to_search_provider(provider),
        },
        ConfigAction::PromptSuggestionChanged { enabled } => {
            AppAction::UpdatePromptSuggestion { enabled }
        }
        ConfigAction::NotificationChanged(update) => AppAction::UpdateNotification {
            update: to_notification_update(update),
        },
    }
}

/// Convert a TUI action produced by a moved config helper back to the contract
/// vocabulary. Returns `None` for actions the config group never emits.
pub(crate) fn app_action_to_config_action(action: AppAction) -> Option<ConfigAction> {
    Some(match action {
        AppAction::PermissionRulesChanged => ConfigAction::PermissionRulesChanged,
        AppAction::StartXaiDeviceLogin => ConfigAction::StartXaiDeviceLogin,
        AppAction::StartClaudeLogin => ConfigAction::StartClaudeLogin,
        AppAction::StartClaudeRevoke => ConfigAction::StartClaudeRevoke,
        AppAction::StartChatgptPkceLogin => ConfigAction::StartChatgptPkceLogin,
        AppAction::StartChatgptRevoke => ConfigAction::StartChatgptRevoke,
        AppAction::StartOrcarouterPkceLogin => ConfigAction::StartOrcarouterPkceLogin,
        AppAction::StartOrcarouterRevoke => ConfigAction::StartOrcarouterRevoke,
        AppAction::OpenConfigView => ConfigAction::OpenConfigView,
        AppAction::OpenModePicker => ConfigAction::OpenModePicker,
        AppAction::OpenThemePicker => ConfigAction::OpenThemePicker,
        AppAction::OpenStatusPicker => ConfigAction::OpenStatusPicker,
        AppAction::OpenProviderPicker => ConfigAction::OpenProviderPicker,
        AppAction::StartPluginLogin { provider } => ConfigAction::StartPluginLogin { provider },
        AppAction::StartPluginLogout { provider } => ConfigAction::StartPluginLogout { provider },
        AppAction::SetScreenMode(mode) => ConfigAction::SetScreenMode(from_screen_mode(mode)),
        AppAction::SetWorkspaceTrust { trusted, save } => {
            ConfigAction::SetWorkspaceTrust { trusted, save }
        }
        AppAction::ModeChanged(mode) => ConfigAction::ModeChanged(from_app_mode(mode)),
        AppAction::ApprovalPolicyPersisted { policy } => {
            ConfigAction::ApprovalPolicyChanged { policy }
        }
        AppAction::SwitchProvider { provider, model } => ConfigAction::SwitchProvider {
            provider: CommandProviderId(provider.as_str().to_string()),
            model,
        },
        AppAction::UpdateCompaction(config) => {
            ConfigAction::CompactionChanged(from_compaction_config(config))
        }
        AppAction::UpdateStreamChunkTimeout(timeout) => {
            ConfigAction::StreamChunkTimeoutChanged(timeout)
        }
        AppAction::UpdateSubagentRuntimeConfig {
            enabled,
            max_subagents,
            launch_concurrency,
            max_spawn_depth,
            api_timeout_secs,
            heartbeat_timeout_secs,
        } => ConfigAction::SubagentRuntimeChanged(CommandSubagentRuntimeConfig {
            enabled,
            max_subagents,
            launch_concurrency,
            max_spawn_depth,
            api_timeout_secs,
            heartbeat_timeout_secs,
        }),
        AppAction::UpdateSearchProvider { provider } => {
            ConfigAction::SearchProviderChanged(from_search_provider(provider))
        }
        AppAction::UpdatePromptSuggestion { enabled } => {
            ConfigAction::PromptSuggestionChanged { enabled }
        }
        AppAction::UpdateNotification { update } => {
            ConfigAction::NotificationChanged(from_notification_update(update))
        }
        _ => return None,
    })
}

fn to_screen_mode(mode: CommandScreenMode) -> ScreenMode {
    match mode {
        CommandScreenMode::Fullscreen => ScreenMode::Fullscreen,
        CommandScreenMode::Inline => ScreenMode::Inline,
    }
}

fn from_screen_mode(mode: ScreenMode) -> CommandScreenMode {
    match mode {
        ScreenMode::Fullscreen => CommandScreenMode::Fullscreen,
        ScreenMode::Inline => CommandScreenMode::Inline,
    }
}

fn to_app_mode(mode: CommandMode) -> AppMode {
    match mode {
        CommandMode::Agent => AppMode::Agent,
        CommandMode::Plan => AppMode::Plan,
        CommandMode::Operate => AppMode::Operate,
    }
}

fn from_app_mode(mode: AppMode) -> CommandMode {
    match mode {
        AppMode::Agent => CommandMode::Agent,
        AppMode::Plan => CommandMode::Plan,
        AppMode::Operate => CommandMode::Operate,
    }
}

fn to_capability_state(state: CommandCapabilityState) -> CapabilityState {
    match state {
        CommandCapabilityState::Supported => CapabilityState::Supported,
        CommandCapabilityState::Unsupported => CapabilityState::Unsupported,
        CommandCapabilityState::Unknown => CapabilityState::Unknown,
    }
}

fn from_capability_state(state: CapabilityState) -> CommandCapabilityState {
    match state {
        CapabilityState::Supported => CommandCapabilityState::Supported,
        CapabilityState::Unsupported => CommandCapabilityState::Unsupported,
        CapabilityState::Unknown => CommandCapabilityState::Unknown,
    }
}

fn to_compaction_config(config: CommandCompactionConfig) -> CompactionConfig {
    CompactionConfig {
        enabled: config.enabled,
        token_threshold: config.token_threshold,
        model: config.model,
        image_input: to_capability_state(config.image_input),
        effective_context_window: config.effective_context_window,
        cache_summary: config.cache_summary,
        focus: config.focus,
        runtime_cost_owner: config.runtime_cost_owner,
        workspace: config.workspace,
        summary_instructions: config.summary_instructions,
        retained_user_message_tokens: config.retained_user_message_tokens,
    }
}

fn from_compaction_config(config: CompactionConfig) -> CommandCompactionConfig {
    CommandCompactionConfig {
        enabled: config.enabled,
        token_threshold: config.token_threshold,
        model: config.model,
        image_input: from_capability_state(config.image_input),
        effective_context_window: config.effective_context_window,
        cache_summary: config.cache_summary,
        focus: config.focus,
        runtime_cost_owner: config.runtime_cost_owner,
        workspace: config.workspace,
        summary_instructions: config.summary_instructions,
        retained_user_message_tokens: config.retained_user_message_tokens,
    }
}

fn to_search_provider(provider: CommandSearchProvider) -> SearchProvider {
    match provider {
        CommandSearchProvider::Bing => SearchProvider::Bing,
        CommandSearchProvider::DuckDuckGo => SearchProvider::DuckDuckGo,
        CommandSearchProvider::Firecrawl => SearchProvider::Firecrawl,
        CommandSearchProvider::Tavily => SearchProvider::Tavily,
        CommandSearchProvider::Bocha => SearchProvider::Bocha,
        CommandSearchProvider::Metaso => SearchProvider::Metaso,
        CommandSearchProvider::Searxng => SearchProvider::Searxng,
        CommandSearchProvider::Baidu => SearchProvider::Baidu,
        CommandSearchProvider::Volcengine => SearchProvider::Volcengine,
        CommandSearchProvider::Sofya => SearchProvider::Sofya,
        CommandSearchProvider::Serply => SearchProvider::Serply,
        CommandSearchProvider::DeepSeek => SearchProvider::DeepSeek,
    }
}

fn from_search_provider(provider: SearchProvider) -> CommandSearchProvider {
    match provider {
        SearchProvider::Bing => CommandSearchProvider::Bing,
        SearchProvider::DuckDuckGo => CommandSearchProvider::DuckDuckGo,
        SearchProvider::Firecrawl => CommandSearchProvider::Firecrawl,
        SearchProvider::Tavily => CommandSearchProvider::Tavily,
        SearchProvider::Bocha => CommandSearchProvider::Bocha,
        SearchProvider::Metaso => CommandSearchProvider::Metaso,
        SearchProvider::Searxng => CommandSearchProvider::Searxng,
        SearchProvider::Baidu => CommandSearchProvider::Baidu,
        SearchProvider::Volcengine => CommandSearchProvider::Volcengine,
        SearchProvider::Sofya => CommandSearchProvider::Sofya,
        SearchProvider::Serply => CommandSearchProvider::Serply,
        SearchProvider::DeepSeek => CommandSearchProvider::DeepSeek,
    }
}

fn to_notification_method(method: CommandNotificationMethod) -> NotificationMethod {
    match method {
        CommandNotificationMethod::Auto => NotificationMethod::Auto,
        CommandNotificationMethod::Osc9 => NotificationMethod::Osc9,
        CommandNotificationMethod::Bel => NotificationMethod::Bel,
        CommandNotificationMethod::Kitty => NotificationMethod::Kitty,
        CommandNotificationMethod::Ghostty => NotificationMethod::Ghostty,
        CommandNotificationMethod::Off => NotificationMethod::Off,
    }
}

fn from_notification_method(method: NotificationMethod) -> CommandNotificationMethod {
    match method {
        NotificationMethod::Auto => CommandNotificationMethod::Auto,
        NotificationMethod::Osc9 => CommandNotificationMethod::Osc9,
        NotificationMethod::Bel => CommandNotificationMethod::Bel,
        NotificationMethod::Kitty => CommandNotificationMethod::Kitty,
        NotificationMethod::Ghostty => CommandNotificationMethod::Ghostty,
        NotificationMethod::Off => CommandNotificationMethod::Off,
    }
}

fn to_completion_sound(sound: CommandCompletionSound) -> CompletionSound {
    match sound {
        CommandCompletionSound::Off => CompletionSound::Off,
        CommandCompletionSound::Beep => CompletionSound::Beep,
        CommandCompletionSound::Bell => CompletionSound::Bell,
        CommandCompletionSound::Whale => CompletionSound::Whale,
        CommandCompletionSound::File => CompletionSound::File,
    }
}

fn from_completion_sound(sound: CompletionSound) -> CommandCompletionSound {
    match sound {
        CompletionSound::Off => CommandCompletionSound::Off,
        CompletionSound::Beep => CommandCompletionSound::Beep,
        CompletionSound::Bell => CommandCompletionSound::Bell,
        CompletionSound::Whale => CommandCompletionSound::Whale,
        CompletionSound::File => CommandCompletionSound::File,
    }
}

fn to_notification_condition(condition: CommandNotificationCondition) -> NotificationCondition {
    match condition {
        CommandNotificationCondition::Always => NotificationCondition::Always,
        CommandNotificationCondition::Unfocused => NotificationCondition::Unfocused,
        CommandNotificationCondition::Never => NotificationCondition::Never,
    }
}

fn from_notification_condition(condition: NotificationCondition) -> CommandNotificationCondition {
    match condition {
        NotificationCondition::Always => CommandNotificationCondition::Always,
        NotificationCondition::Unfocused => CommandNotificationCondition::Unfocused,
        NotificationCondition::Never => CommandNotificationCondition::Never,
    }
}

fn to_subagent_completion(completion: CommandSubagentCompletion) -> SubagentCompletionNotification {
    match completion {
        CommandSubagentCompletion::Always => SubagentCompletionNotification::Always,
        CommandSubagentCompletion::FinalOnly => SubagentCompletionNotification::FinalOnly,
        CommandSubagentCompletion::Off => SubagentCompletionNotification::Off,
    }
}

fn from_subagent_completion(
    completion: SubagentCompletionNotification,
) -> CommandSubagentCompletion {
    match completion {
        SubagentCompletionNotification::Always => CommandSubagentCompletion::Always,
        SubagentCompletionNotification::FinalOnly => CommandSubagentCompletion::FinalOnly,
        SubagentCompletionNotification::Off => CommandSubagentCompletion::Off,
    }
}

fn to_notification_event(event: CommandNotificationEvent) -> NotificationEvent {
    match event {
        CommandNotificationEvent::TurnComplete => NotificationEvent::TurnComplete,
        CommandNotificationEvent::SubagentTerminal => NotificationEvent::SubagentTerminal,
        CommandNotificationEvent::ApprovalNeeded => NotificationEvent::ApprovalNeeded,
        CommandNotificationEvent::InputNeeded => NotificationEvent::InputNeeded,
        CommandNotificationEvent::ElevationNeeded => NotificationEvent::ElevationNeeded,
        CommandNotificationEvent::ModelNotify => NotificationEvent::ModelNotify,
    }
}

fn from_notification_event(event: NotificationEvent) -> CommandNotificationEvent {
    match event {
        NotificationEvent::TurnComplete => CommandNotificationEvent::TurnComplete,
        NotificationEvent::SubagentTerminal => CommandNotificationEvent::SubagentTerminal,
        NotificationEvent::ApprovalNeeded => CommandNotificationEvent::ApprovalNeeded,
        NotificationEvent::InputNeeded => CommandNotificationEvent::InputNeeded,
        NotificationEvent::ElevationNeeded => CommandNotificationEvent::ElevationNeeded,
        NotificationEvent::ModelNotify => CommandNotificationEvent::ModelNotify,
    }
}

fn to_notification_update(update: CommandNotificationUpdate) -> NotificationConfigUpdate {
    match update {
        CommandNotificationUpdate::Method(method) => {
            NotificationConfigUpdate::Method(to_notification_method(method))
        }
        CommandNotificationUpdate::ThresholdSecs(value) => {
            NotificationConfigUpdate::ThresholdSecs(value)
        }
        CommandNotificationUpdate::IncludeSummary(value) => {
            NotificationConfigUpdate::IncludeSummary(value)
        }
        CommandNotificationUpdate::Quiet(value) => NotificationConfigUpdate::Quiet(value),
        CommandNotificationUpdate::Sound(sound) => {
            NotificationConfigUpdate::Sound(sound.map(to_completion_sound))
        }
        CommandNotificationUpdate::Condition(condition) => {
            NotificationConfigUpdate::Condition(to_notification_condition(condition))
        }
        CommandNotificationUpdate::CompletionSound(sound) => {
            NotificationConfigUpdate::CompletionSound(to_completion_sound(sound))
        }
        CommandNotificationUpdate::SubagentCompletion(completion) => {
            NotificationConfigUpdate::SubagentCompletion(to_subagent_completion(completion))
        }
        CommandNotificationUpdate::SoundFile(path) => NotificationConfigUpdate::SoundFile(path),
        CommandNotificationUpdate::Event(event, enabled) => {
            NotificationConfigUpdate::Event(to_notification_event(event), enabled)
        }
        CommandNotificationUpdate::EventSoundEnabled(enabled) => {
            NotificationConfigUpdate::EventSoundEnabled(enabled)
        }
        CommandNotificationUpdate::EventSoundEvents(events) => {
            NotificationConfigUpdate::EventSoundEvents(events)
        }
        CommandNotificationUpdate::EventSoundMinIntervalMs(value) => {
            NotificationConfigUpdate::EventSoundMinIntervalMs(value)
        }
        CommandNotificationUpdate::EventSoundQuiet(quiet) => {
            NotificationConfigUpdate::EventSoundQuiet(quiet)
        }
    }
}

fn from_notification_update(update: NotificationConfigUpdate) -> CommandNotificationUpdate {
    match update {
        NotificationConfigUpdate::Method(method) => {
            CommandNotificationUpdate::Method(from_notification_method(method))
        }
        NotificationConfigUpdate::ThresholdSecs(value) => {
            CommandNotificationUpdate::ThresholdSecs(value)
        }
        NotificationConfigUpdate::IncludeSummary(value) => {
            CommandNotificationUpdate::IncludeSummary(value)
        }
        NotificationConfigUpdate::Quiet(value) => CommandNotificationUpdate::Quiet(value),
        NotificationConfigUpdate::Sound(sound) => {
            CommandNotificationUpdate::Sound(sound.map(from_completion_sound))
        }
        NotificationConfigUpdate::Condition(condition) => {
            CommandNotificationUpdate::Condition(from_notification_condition(condition))
        }
        NotificationConfigUpdate::CompletionSound(sound) => {
            CommandNotificationUpdate::CompletionSound(from_completion_sound(sound))
        }
        NotificationConfigUpdate::SubagentCompletion(completion) => {
            CommandNotificationUpdate::SubagentCompletion(from_subagent_completion(completion))
        }
        NotificationConfigUpdate::SoundFile(path) => CommandNotificationUpdate::SoundFile(path),
        NotificationConfigUpdate::Event(event, enabled) => {
            CommandNotificationUpdate::Event(from_notification_event(event), enabled)
        }
        NotificationConfigUpdate::EventSoundEnabled(enabled) => {
            CommandNotificationUpdate::EventSoundEnabled(enabled)
        }
        NotificationConfigUpdate::EventSoundEvents(events) => {
            CommandNotificationUpdate::EventSoundEvents(events)
        }
        NotificationConfigUpdate::EventSoundMinIntervalMs(value) => {
            CommandNotificationUpdate::EventSoundMinIntervalMs(value)
        }
        NotificationConfigUpdate::EventSoundQuiet(quiet) => {
            CommandNotificationUpdate::EventSoundQuiet(quiet)
        }
    }
}

/// Render a contract config reply for a host-side caller (interactive UI views
/// and tests) that needs a concrete `CommandResult`.
pub(crate) fn render_config_reply(
    reply: codewhale_command_contract::config::ConfigReply,
    locale: codewhale_localization::Locale,
) -> crate::commands::CommandResult {
    use codewhale_command_contract::config::ConfigMessage;

    /// Render a message, propagating an unknown key or an invalid replacement
    /// contract instead of emitting a raw key or an untranslated template.
    fn message_text(
        message: ConfigMessage,
        locale: codewhale_localization::Locale,
    ) -> Result<String, String> {
        match message {
            ConfigMessage::Literal(text) => Ok(text),
            ConfigMessage::Concat(parts) => {
                let mut out = String::new();
                for part in parts {
                    out.push_str(&message_text(part, locale)?);
                }
                Ok(out)
            }
            ConfigMessage::Keyed { key, params } => {
                let mut resolved: Vec<(String, String)> = Vec::with_capacity(params.len());
                for (name, param) in params {
                    resolved.push((name, message_text(param, locale)?));
                }
                let refs: Vec<(&str, &str)> = resolved
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let Some(id) = crate::commands::contract::config_messages::resolve(&key) else {
                    return Err("unknown translation key".to_string());
                };
                let template = codewhale_localization::tr(locale, id);
                crate::commands::contract::apply_named_replacements(&template, &refs)
                    .ok_or_else(|| "invalid translation replacement contract".to_string())
            }
        }
    }

    // Same failure policy as the portable renderer: a failed render is a
    // failure with no action, never a successful reply with a raw key.
    let message = match reply.message {
        None => None,
        Some(message) => match message_text(message, locale) {
            Ok(text) => Some(text),
            Err(error) => {
                return crate::commands::CommandResult {
                    message: Some(format!("Error: {error}")),
                    action: None,
                    is_error: true,
                };
            }
        },
    };
    let message = match (message, reply.is_error) {
        (Some(text), true) if !text.starts_with("Error: ") => Some(format!("Error: {text}")),
        (other, _) => other,
    };
    crate::commands::CommandResult {
        message,
        action: reply.action.map(config_action_to_app_action),
        is_error: reply.is_error,
    }
}
