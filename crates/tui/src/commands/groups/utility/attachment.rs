//! Local media attachment commands.

use std::path::{Path, PathBuf};

use codewhale_command_contract::facets::CommandMediaContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "attach",
    aliases: &["image", "media", "fujian"],
    usage: "/attach <path>",
    description_key: "cmd_attach_description",
};

pub(in crate::commands) struct AttachCmd;

impl RegisterCommand<CommandResult> for AttachCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKSPACE.union(CommandCapabilities::MEDIA),
            handler: attach_contextual,
        }
    }
}

fn attach_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(workspace) = parts.workspace.as_deref() else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    let Some(media) = parts.media.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: media");
    };
    attach(workspace.workspace(), media, arg)
}

fn attach(
    workspace: PathBuf,
    media: &mut dyn CommandMediaContext,
    arg: Option<&str>,
) -> CommandResult {
    let Some(raw_path) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return CommandResult::error("Usage: /attach <image-or-video-path>");
    };

    let path = resolve_attachment_path(raw_path, &workspace);
    match media.attach_media(&path) {
        Ok(receipt) => CommandResult::message(format!(
            "Attached {}: {}",
            receipt.kind,
            receipt.path.display()
        )),
        Err(error) => CommandResult::error(error),
    }
}

fn resolve_attachment_path(raw_path: &str, workspace: &Path) -> PathBuf {
    let unquoted = raw_path.trim().trim_matches('"').trim_matches('\'');
    let path = expand_home(unquoted);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeMedia;
    impl CommandMediaContext for FakeMedia {
        fn attach_media(
            &mut self,
            path: &Path,
        ) -> Result<codewhale_command_contract::facets::MediaAttachmentReceipt, String> {
            if path.extension().and_then(|ext| ext.to_str()) == Some("png") {
                Ok(codewhale_command_contract::facets::MediaAttachmentReceipt {
                    kind: "image".to_string(),
                    path: path.to_path_buf(),
                })
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("mp4") {
                Ok(codewhale_command_contract::facets::MediaAttachmentReceipt {
                    kind: "video".to_string(),
                    path: path.to_path_buf(),
                })
            } else {
                Err("Unsupported attachment type".to_string())
            }
        }
    }

    fn workspace() -> PathBuf {
        PathBuf::from("/workspace")
    }

    #[test]
    fn attach_resolves_relative_and_absolute_paths() {
        let relative = resolve_attachment_path("photo.png", &workspace());
        assert_eq!(relative, PathBuf::from("/workspace/photo.png"));

        let absolute = resolve_attachment_path("/tmp/photo.png", &workspace());
        assert_eq!(absolute, PathBuf::from("/tmp/photo.png"));

        let quoted = resolve_attachment_path("\"photo.png\"", &workspace());
        assert_eq!(quoted, PathBuf::from("/workspace/photo.png"));

        let home = resolve_attachment_path("~/photo.png", &workspace());
        if let Some(home_dir) = std::env::var_os("HOME") {
            assert_eq!(home, PathBuf::from(home_dir).join("photo.png"));
        }
    }

    #[test]
    fn attach_delegates_to_media_facet_and_composes_confirm() {
        let result = attach(workspace(), &mut FakeMedia, Some("photo.png"));
        assert!(result.message.expect("message").contains("Attached image"));
        assert!(!result.is_error);

        let video = attach(workspace(), &mut FakeMedia, Some("clip.mp4"));
        assert!(video.message.expect("message").contains("Attached video"));
    }

    #[test]
    fn attach_requires_a_path() {
        let result = attach(workspace(), &mut FakeMedia, None);
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Usage: /attach"),
            "{:?}",
            result.message
        );
    }

    #[test]
    fn attach_forwards_media_facet_error() {
        let result = attach(workspace(), &mut FakeMedia, Some("notes.txt"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .expect("message")
                .contains("Unsupported attachment type")
        );
    }

    #[test]
    fn handler_is_contextual() {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = AttachCmd::handler()
        else {
            panic!("attach must be contextual");
        };
        assert_eq!(
            capabilities,
            CommandCapabilities::WORKSPACE.union(CommandCapabilities::MEDIA)
        );
        let missing = handler(CommandContexts::empty(), Some("photo.png"));
        assert!(missing.is_error);
        assert_eq!(
            missing.message.as_deref(),
            Some("Error: Command capability unavailable: workspace")
        );
        assert_eq!(AttachCmd::info().description_key, "cmd_attach_description");
        assert_eq!(AttachCmd::info().aliases, &["image", "media", "fujian"]);
    }
}
