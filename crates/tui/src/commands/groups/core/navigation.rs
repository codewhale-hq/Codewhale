#[path = "anchor.rs"]
pub mod anchor;
#[path = "help.rs"]
pub mod help;
#[path = "home.rs"]
pub mod home;
#[path = "hotbar.rs"]
pub mod hotbar;
#[path = "links.rs"]
pub mod links;
#[path = "transcript.rs"]
pub mod transcript;
#[path = "translate.rs"]
pub mod translate;
#[path = "turn.rs"]
pub mod turn;
#[path = "voice.rs"]
pub mod voice;
#[path = "workspace.rs"]
pub mod workspace;

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CoreNavigationCommandResult;

pub fn portable_handlers() -> [(
    &'static CommandInfo,
    CommandHandler<CoreNavigationCommandResult>,
); 13] {
    [
        (anchor::AnchorCmd::info(), anchor::AnchorCmd::handler()),
        (help::HelpCmd::info(), help::HelpCmd::handler()),
        (hotbar::HotbarCmd::info(), hotbar::HotbarCmd::handler()),
        (links::LinksCmd::info(), links::LinksCmd::handler()),
        (
            transcript::TranscriptCmd::info(),
            transcript::TranscriptCmd::handler(),
        ),
        (turn::TurnCmd::info(), turn::TurnCmd::handler()),
        (home::HomeCmd::info(), home::HomeCmd::handler()),
        (home::OverviewCmd::info(), home::OverviewCmd::handler()),
        (
            workspace::WorkspaceCmd::info(),
            workspace::WorkspaceCmd::handler(),
        ),
        (
            translate::TranslateCmd::info(),
            translate::TranslateCmd::handler(),
        ),
        (voice::VoiceCmd::info(), voice::VoiceCmd::handler()),
        (voice::VoiceSendCmd::info(), voice::VoiceSendCmd::handler()),
        (
            voice::VoiceControlCmd::info(),
            voice::VoiceControlCmd::handler(),
        ),
    ]
}
