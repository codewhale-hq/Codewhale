use codewhale_engine::conversation_modes::{
    self, CREATOR, ConversationMode, ConversationModeTable, STANDARD,
};
use codewhale_engine::extension_host::ExtensionHostOptions;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_runtime::config::Config;

const LABEL_CHARS: usize = 40;

pub(crate) fn options(config: &Config) -> ExtensionHostOptions {
    ExtensionHostOptions::from_config(config.extension_host.as_ref())
}

pub(crate) async fn current_table(config: &Config) -> ConversationModeTable {
    let plugin_creator_enabled = config
        .features()
        .enabled(codewhale_runtime::features::Feature::PluginCreator);
    conversation_modes::current_table(options(config), plugin_creator_enabled).await
}

pub(crate) fn current_table_blocking(config: Option<&Config>) -> ConversationModeTable {
    let options = config.map_or_else(ExtensionHostOptions::default, options);
    let plugin_creator_enabled = config.map_or_else(
        || {
            codewhale_runtime::features::Features::with_defaults()
                .enabled(codewhale_runtime::features::Feature::PluginCreator)
        },
        |config| {
            config
                .features()
                .enabled(codewhale_runtime::features::Feature::PluginCreator)
        },
    );
    let future = conversation_modes::current_table(options, plugin_creator_enabled);
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        _ => std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("conversation mode runtime")
                        .block_on(future)
                })
                .join()
                .expect("conversation mode probe")
        }),
    }
}

pub(crate) fn display_name(locale: Locale, mode: &ConversationMode) -> String {
    match mode.id.as_str() {
        STANDARD => tr(locale, MessageId::ConversationModeStandardName).into_owned(),
        CREATOR => tr(locale, MessageId::ConversationModeCreatorName).into_owned(),
        _ => mode.name.clone(),
    }
}

pub(crate) fn display_description(locale: Locale, mode: &ConversationMode) -> String {
    match mode.id.as_str() {
        STANDARD => tr(locale, MessageId::ConversationModeStandardDescription).into_owned(),
        CREATOR => tr(locale, MessageId::ConversationModeCreatorDescription).into_owned(),
        _ => mode.description.clone(),
    }
}

pub(crate) fn display_reason(locale: Locale, reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|reason| !reason.is_empty()) {
        Some(reason) => reason.to_string(),
        None => tr(locale, MessageId::ConversationModeUnavailableDefault).into_owned(),
    }
}

fn clean(text: &str) -> String {
    let mut out: String = text
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        })
        .take(LABEL_CHARS + 1)
        .collect();
    if out.chars().count() > LABEL_CHARS {
        out = out.chars().take(LABEL_CHARS).collect::<String>() + "…";
    }
    out
}

pub(crate) fn name_for_stored(
    locale: Locale,
    stored: Option<&str>,
    table: Option<&ConversationModeTable>,
) -> String {
    let id = conversation_modes::effective_id(stored);
    match table.and_then(|table| table.modes.iter().find(|mode| mode.id == id)) {
        Some(mode) => display_name(locale, mode),
        None if id == STANDARD => tr(locale, MessageId::ConversationModeStandardName).into_owned(),
        None if id == CREATOR => tr(locale, MessageId::ConversationModeCreatorName).into_owned(),
        None => clean(id),
    }
}

pub(crate) fn unavailable_reason(
    stored: Option<&str>,
    table: &ConversationModeTable,
    locale: Locale,
) -> Option<String> {
    let id = conversation_modes::effective_id(stored);
    match table.modes.iter().find(|mode| mode.id == id) {
        Some(mode) => (!mode.available).then(|| display_reason(locale, mode.reason.as_deref())),
        None => Some(
            tr(locale, MessageId::ConversationModeUnknown)
                .replace("{id}", &clean(id))
                .replace(
                    "{modes}",
                    &table
                        .modes
                        .iter()
                        .map(|mode| mode.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
        ),
    }
}

pub(crate) fn label(
    locale: Locale,
    stored: Option<&str>,
    unavailable: Option<&str>,
    table: Option<&ConversationModeTable>,
) -> Option<String> {
    stored?;
    let name = name_for_stored(locale, stored, table);
    Some(if unavailable.is_some() {
        tr(locale, MessageId::ConversationModeLabelUnavailable).replace("{name}", &name)
    } else {
        name
    })
}

pub(crate) fn new_command(id: &str) -> String {
    if id == STANDARD {
        "/new".to_string()
    } else {
        format!("/new {id}")
    }
}

pub(crate) fn locked_refusal(locale: Locale, requested_id: &str) -> String {
    tr(locale, MessageId::ConversationModeLockedRefusal)
        .replace("{command}", &format!("`{}`", new_command(requested_id)))
}

pub(crate) enum Resolution {
    Unknown(String),
    Unavailable {
        name: String,
        reason: String,
    },
    Ready {
        stored: Option<String>,
        name: String,
    },
}

pub(crate) fn resolve(locale: Locale, table: &ConversationModeTable, id: &str) -> Resolution {
    let id = id.trim().to_ascii_lowercase();
    let id = id.as_str();
    match table.modes.iter().find(|mode| mode.id == id) {
        None => Resolution::Unknown(
            tr(locale, MessageId::ConversationModeUnknown)
                .replace("{id}", &clean(id))
                .replace(
                    "{modes}",
                    &table
                        .modes
                        .iter()
                        .map(|mode| mode.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
        ),
        Some(mode) if !mode.available => Resolution::Unavailable {
            name: display_name(locale, mode),
            reason: display_reason(locale, mode.reason.as_deref()),
        },
        Some(mode) => Resolution::Ready {
            stored: conversation_modes::stored_form(&mode.id),
            name: display_name(locale, mode),
        },
    }
}

pub(crate) fn unavailable_message(locale: Locale, name: &str, reason: &str) -> String {
    tr(locale, MessageId::ConversationModeUnavailableRefusal)
        .replace("{name}", name)
        .replace("{reason}", reason)
}

pub(crate) fn saved_unavailable_notice(locale: Locale, name: &str, reason: &str) -> String {
    tr(locale, MessageId::ConversationModeSavedUnavailableNotice)
        .replace("{name}", name)
        .replace("{reason}", reason)
}

pub(crate) async fn set_in_place(
    app: &mut crate::tui::app::App,
    config: &Config,
    engine: &codewhale_engine::core::engine::EngineHandle,
    id: &str,
) -> Result<String, String> {
    let locale = app.ui_locale;
    let table = current_table(config).await;
    match resolve(locale, &table, id) {
        Resolution::Unknown(message) => Err(message),
        Resolution::Unavailable { name, reason } => {
            Err(unavailable_message(locale, &name, &reason))
        }
        Resolution::Ready { stored, name } => {
            let done = tr(locale, MessageId::ConversationModeSet).replace("{name}", &name);
            if stored == app.conversation_mode {
                return Ok(done);
            }
            if app.conversation_mode_locked() {
                return Err(locked_refusal(locale, id.trim()));
            }
            let previous = app.conversation_mode.clone();
            let previous_unavailable = app.conversation_mode_unavailable.clone();
            app.install_conversation_mode(stored, None);
            let synced = engine
                .send(codewhale_engine::core::ops::Op::SyncSession {
                    expected: None,
                    control: app.session_sync_control(),
                    session_id: app.current_session_id.clone(),
                    messages: app.api_messages.as_ref().clone(),
                    system_prompt: app.system_prompt.clone(),
                    system_prompt_override: false,
                    model: app.model.clone(),
                    workspace: app.workspace.clone(),
                    mode: app.mode,
                    agent_preset: app.agent_preset(),
                })
                .await;
            if let Err(error) = synced {
                tracing::warn!(%error, "conversation mode session sync failed");
                app.install_conversation_mode(previous, previous_unavailable);
                return Err(tr(locale, MessageId::ConversationModeSyncFailed).into_owned());
            }
            Ok(done)
        }
    }
}

pub(crate) async fn launch_conversation_mode(
    config: &Config,
    locale: Locale,
    flag: Option<&str>,
    resumed: Option<&codewhale_engine::session_manager::SavedSession>,
) -> Result<Option<String>, String> {
    let Some(id) = flag.map(str::trim) else {
        return Ok(None);
    };
    if let Some(saved) = resumed {
        let requested = conversation_modes::stored_form(&id.to_ascii_lowercase());
        return if requested == saved.agent_preset {
            Ok(requested)
        } else {
            Err(locked_refusal(locale, id))
        };
    }
    let table = current_table(config).await;
    match resolve(locale, &table, id) {
        Resolution::Unknown(message) => Err(message),
        Resolution::Unavailable { name, reason } => {
            Err(unavailable_message(locale, &name, &reason))
        }
        Resolution::Ready { stored, .. } => Ok(stored),
    }
}

pub(crate) fn load_saved_for_resume(
    locale: Locale,
    selector: &str,
    workspace: &std::path::Path,
) -> Result<codewhale_engine::session_manager::SavedSession, String> {
    codewhale_engine::session_manager::SessionManager::default_location()
        .ok()
        .and_then(|manager| {
            if selector == "latest" {
                let id = manager
                    .get_latest_session_for_workspace(workspace)
                    .ok()
                    .flatten()?
                    .id;
                manager.load_session(&id).ok()
            } else {
                manager.load_session_by_prefix(selector).ok()
            }
        })
        .ok_or_else(|| tr(locale, MessageId::ConversationModeResumeUnreadable).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_unavailable(reason: Option<String>) -> ConversationModeTable {
        ConversationModeTable {
            default: STANDARD,
            modes: vec![ConversationMode {
                id: "unavailable-id".to_string(),
                name: "Unavailable".to_string(),
                description: "Unavailable test mode".to_string(),
                source: "builtin",
                available: false,
                reason,
            }],
        }
    }

    #[test]
    fn display_reason_localizes_missing_or_blank_reason() {
        let expected = tr(
            Locale::ZhHans,
            MessageId::ConversationModeUnavailableDefault,
        );
        for raw in [None, Some(""), Some("   ")] {
            let reason = display_reason(Locale::ZhHans, raw);
            assert_eq!(reason, expected.as_ref());
            assert!(!reason.contains("This mode is unavailable."));
            assert!(!reason.contains("unavailable"));
        }
        assert_eq!(
            display_reason(Locale::ZhHans, Some("custom extension reason")),
            "custom extension reason"
        );
    }

    #[test]
    fn resolve_and_unavailable_reason_localize_default_reason() {
        let table = table_with_unavailable(None);
        let expected = tr(
            Locale::ZhHans,
            MessageId::ConversationModeUnavailableDefault,
        );

        let Resolution::Unavailable { reason, .. } =
            resolve(Locale::ZhHans, &table, "unavailable-id")
        else {
            panic!("expected Resolution::Unavailable");
        };
        assert_eq!(reason, expected.as_ref());
        assert!(!reason.contains("This mode is unavailable."));

        let fallback = unavailable_reason(Some("unavailable-id"), &table, Locale::ZhHans)
            .expect("unavailable reason");
        assert_eq!(fallback, expected.as_ref());
        assert!(!fallback.contains("unavailable"));
    }
}
