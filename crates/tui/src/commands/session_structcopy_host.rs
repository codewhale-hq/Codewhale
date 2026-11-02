//! Host observations and registration replacing the concrete App access in
//! groups/session/structcopy.rs. The existing runtime owners still enforce
//! workflow/session scope, plan locking and safe structured-image projection.

use super::SharedCommandHost;
#[cfg(test)]
use crate::commands::CommandResult as HostResult;
use codewhale_command_contract::facets::*;
#[cfg(test)]
use codewhale_command_contract::outcome::StructcopyCommandResult;
use codewhale_models::ContentBlock;
use codewhale_secrets::sanitize::is_internal_role;
use std::path::Path;

pub(super) struct SessionStructcopyAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl CommandSessionStructcopyContext for SessionStructcopyAdapter<'_> {
    fn transcript_item(&self, index: usize) -> Result<StructcopyTranscript, StructcopyError> {
        let app = self.host.app.borrow();
        let message = index
            .checked_sub(1)
            .and_then(|index| app.api_messages.get(index))
            .ok_or(StructcopyError::Unavailable)?;
        let content = if is_internal_role(message.role.as_str()) {
            StructcopyContent::InternalContext
        } else {
            StructcopyContent::Visible(message.content.iter().map(project_block).collect())
        };
        Ok(StructcopyTranscript {
            index,
            role: message.role.to_string(),
            content,
        })
    }

    fn tool_pair(&self, call_id: &str) -> Result<StructcopyToolPair, StructcopyError> {
        let app = self.host.app.borrow();
        if call_id.trim().is_empty() {
            return Err(StructcopyError::Unavailable);
        }
        // Preserve upstream execution identity, including ambiguous local/legacy
        // collisions. An explicit execution ID never falls back to a wire ID.
        let mut calls = app
            .api_messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => block
                    .tool_call_key()
                    .filter(|key| key.as_str() == call_id)
                    .map(|key| (key, id, name, input)),
                _ => None,
            });
        let Some((key, provider_id, name, input)) = calls.next() else {
            return Err(StructcopyError::Unavailable);
        };
        if calls.next().is_some() || provider_id.trim().is_empty() {
            return Err(StructcopyError::Unavailable);
        }
        let mut result = None;
        for block in app.api_messages.iter().flat_map(|message| &message.content) {
            if block.tool_call_key() != Some(key) {
                continue;
            }
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                content_blocks,
                ..
            } = block
            {
                if tool_use_id != provider_id || result.is_some() {
                    return Err(StructcopyError::Unavailable);
                }
                result = Some(project_result(
                    content,
                    *is_error,
                    content_blocks.as_deref(),
                ));
            }
        }
        Ok(StructcopyToolPair {
            name: name.clone(),
            input: input.clone(),
            result,
        })
    }

    fn plan_snapshot(&self) -> Result<StructcopyPlan, StructcopyError> {
        let app = self.host.app.borrow();
        let snapshot = app
            .plan_state
            .try_lock()
            .map_err(|_| StructcopyError::Busy)?
            .snapshot();
        if snapshot.is_empty() {
            return Err(StructcopyError::Unavailable);
        }
        // Preserve the original owner's serialization/omission rules, then
        // validate its known projection into the contract shape. No host blob
        // crosses the boundary and no plan-state implementation is duplicated.
        serde_json::to_value(snapshot)
            .and_then(serde_json::from_value)
            .map_err(|error| StructcopyError::Preparation(error.to_string()))
    }

    fn workflow_projection(&self, run_id: &str) -> Result<StructcopyWorkflow, StructcopyError> {
        let app = self.host.app.borrow();
        let projection = codewhale_engine::tools::workflow::structcopy_run_projection(
            &app.workspace,
            run_id,
            app.current_session_id.as_deref(),
        )
        .ok_or(StructcopyError::Unavailable)?;
        // This is the existing bounded projection, not serialized runtime
        // state: ownership, retention and filename-only policy stay upstream.
        serde_json::from_value(projection)
            .map_err(|error| StructcopyError::Preparation(error.to_string()))
    }

    fn path_roots(&self) -> StructcopyPathRoots {
        observe_path_roots(&self.host.app.borrow().workspace)
    }

    fn write_clipboard(&self, text: &str) -> Result<StructcopyTransport, String> {
        use crate::tui::clipboard::CopyTransport;
        self.host
            .app
            .borrow_mut()
            .clipboard
            .write_text_status(text)
            .map(|transport| match transport {
                CopyTransport::Native => StructcopyTransport::Native,
                CopyTransport::Terminal => StructcopyTransport::TerminalQueued,
            })
            .map_err(|error| error.to_string())
    }
}

pub(in crate::commands) fn observe_path_roots(workspace: &Path) -> StructcopyPathRoots {
    StructcopyPathRoots {
        workspace: workspace.to_string_lossy().into_owned(),
        canonical_workspace: workspace
            .canonicalize()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        home: std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| home.to_string_lossy().into_owned()),
    }
}

fn project_result(
    content: &str,
    is_error: Option<bool>,
    content_blocks: Option<&[serde_json::Value]>,
) -> StructcopyToolResult {
    StructcopyToolResult {
        content: content.into(),
        is_error,
        content_blocks: codewhale_engine::image_attach::safe_tool_result_content_blocks(
            content_blocks,
        ),
    }
}

fn project_block(block: &ContentBlock) -> StructcopyBlock {
    match block {
        ContentBlock::Text { text, .. } => StructcopyBlock::Text(text.clone()),
        ContentBlock::Thinking { .. } => StructcopyBlock::ThinkingOmitted,
        ContentBlock::ToolUse {
            id,
            name,
            input,
            caller,
            ..
        } => StructcopyBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
            caller_type: caller.as_ref().map(|caller| caller.caller_type.clone()),
        },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => StructcopyBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            result: project_result(content, *is_error, content_blocks.as_deref()),
        },
        ContentBlock::ImageUrl { image_url } => {
            if image_url.url.starts_with("http://") || image_url.url.starts_with("https://") {
                StructcopyBlock::ImageUrl(image_url.url.clone())
            } else {
                StructcopyBlock::ImageOmitted
            }
        }
        ContentBlock::ServerToolUse { id, name, input } => StructcopyBlock::ServerToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        ContentBlock::ToolSearchToolResult {
            tool_use_id,
            content,
        } => StructcopyBlock::ToolSearchToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.clone(),
        },
        ContentBlock::CodeExecutionToolResult {
            tool_use_id,
            content,
        } => StructcopyBlock::CodeExecutionToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.clone(),
        },
    }
}

#[cfg(test)]
pub(in crate::commands) fn host_result(result: StructcopyCommandResult) -> HostResult {
    HostResult {
        message: result.message,
        action: result.action.map(|impossible| match impossible {}),
        is_error: result.is_error,
    }
}
