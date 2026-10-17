//! Voice input commands — `/voice`, `/voice-send`, `/voice-control`.

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::{
    CoreNavigationAction as Action, CoreNavigationCommandResult as CommandResult,
};

pub const VOICE_INFO: CommandInfo = CommandInfo {
    name: "voice",
    aliases: &["yuyin", "语音"],
    usage: "/voice",
    description_key: "cmd_voice_description",
};
pub const VOICE_SEND_INFO: CommandInfo = CommandInfo {
    name: "voicesend",
    aliases: &["voice-send", "yuyinsend", "语音发送"],
    usage: "/voicesend",
    description_key: "cmd_voice_send_description",
};
pub const VOICE_CONTROL_INFO: CommandInfo = CommandInfo {
    name: "voicecontrol",
    aliases: &["voice-control", "yuyincontrol", "语音控制"],
    usage: "/voicecontrol",
    description_key: "cmd_voice_control_description",
};

pub struct VoiceCmd;
pub struct VoiceSendCmd;
pub struct VoiceControlCmd;

impl RegisterCommand<CommandResult> for VoiceCmd {
    fn info() -> &'static CommandInfo {
        &VOICE_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::VOICE_INPUT.union(CommandCapabilities::PRESENTATION),
            handler: voice,
        }
    }
}
impl RegisterCommand<CommandResult> for VoiceSendCmd {
    fn info() -> &'static CommandInfo {
        &VOICE_SEND_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::VOICE_INPUT.union(CommandCapabilities::PRESENTATION),
            handler: voice_send,
        }
    }
}
impl RegisterCommand<CommandResult> for VoiceControlCmd {
    fn info() -> &'static CommandInfo {
        &VOICE_CONTROL_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::VOICE_INPUT.union(CommandCapabilities::PRESENTATION),
            handler: voice_control,
        }
    }
}

fn voice(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(voice) = parts.voice_input else {
        return CommandResult::error("Command capability unavailable: voice_input");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let enabled = !voice.voice_enabled();
    if enabled && !voice.recorder_available() {
        let message = presentation
            .translate("voice_err_no_recorder", &[])
            .unwrap_or_else(std::convert::identity);
        return CommandResult::error(message);
    }
    let key = if enabled {
        "voice_enabled"
    } else {
        "voice_disabled"
    };
    let message = match presentation.translate(key, &[]) {
        Ok(message) => message,
        Err(error) => return CommandResult::error(error),
    };
    voice.set_voice_enabled(enabled);
    if enabled {
        CommandResult::with_message_and_action(message, Action::VoiceCapture)
    } else {
        CommandResult::message(message)
    }
}

fn voice_send(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(voice) = parts.voice_input else {
        return CommandResult::error("Command capability unavailable: voice_input");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let enabled = !voice.send_enabled();
    let key = if enabled {
        "voice_send_enabled"
    } else {
        "voice_send_disabled"
    };
    let message = match presentation.translate(key, &[]) {
        Ok(message) => message,
        Err(error) => return CommandResult::error(error),
    };
    voice.set_send_enabled(enabled);
    CommandResult::message(message)
}

fn voice_control(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(voice) = parts.voice_input else {
        return CommandResult::error("Command capability unavailable: voice_input");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let enabled = !voice.control_enabled();
    let key = if enabled {
        "voice_control_enabled"
    } else {
        "voice_control_disabled"
    };
    let message = match presentation.translate(key, &[]) {
        Ok(message) => message,
        Err(error) => return CommandResult::error(error),
    };
    voice.set_control_enabled(enabled);
    CommandResult::message(message)
}
