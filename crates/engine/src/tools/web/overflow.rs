//! Shared route-sized inline output with recoverable session spillover.

use std::path::PathBuf;

use crate::tools::spec::{ToolContext, ToolError};

#[derive(Debug)]
pub struct OverflowArtifact {
    pub session_id: String,
    pub absolute_path: PathBuf,
    pub relative_path: PathBuf,
    pub byte_size: u64,
    pub preview: String,
}

#[derive(Debug)]
pub struct BoundedText {
    pub content: String,
    pub artifact: Option<OverflowArtifact>,
}

/// The route's inline budget for one tool result; the same number the engine
/// measures every tool result against (#6508).
pub fn inline_char_budget(context: &ToolContext) -> usize {
    crate::route_budget::route_inline_char_budget(context.route_context_window)
}

pub fn bound_text<F>(
    content: String,
    context: &ToolContext,
    artifact_id: F,
    subject: &str,
) -> Result<BoundedText, ToolError>
where
    F: FnOnce(&str) -> String,
{
    let budget = inline_char_budget(context);
    if content.chars().count() <= budget {
        return Ok(BoundedText {
            content,
            artifact: None,
        });
    }

    let artifact_id = artifact_id(&content);
    let (absolute_path, relative_path) = codewhale_runtime::artifacts::write_session_artifact(
        &context.state_namespace,
        &artifact_id,
        &content,
    )
    .map_err(|error| {
        ToolError::execution_failed(format!(
            "failed to preserve {subject} content artifact: {error}"
        ))
    })?;
    let relative = codewhale_runtime::artifacts::format_artifact_relative_path(&relative_path);
    let absolute = absolute_path.display().to_string();
    let inline = crate::tools::truncate::fit_to_inline_budget(
        &content,
        budget,
        Some(&absolute),
        Some(&relative),
    );
    let preview = content.chars().take(200).collect();

    Ok(BoundedText {
        content: inline,
        artifact: Some(OverflowArtifact {
            session_id: context.state_namespace.clone(),
            absolute_path,
            relative_path,
            byte_size: content.len() as u64,
            preview,
        }),
    })
}
