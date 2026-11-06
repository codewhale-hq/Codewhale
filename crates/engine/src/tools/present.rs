use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_str, type_mismatch,
};

const MAX_FILES: usize = 32;
const READ_BUFFER_BYTES: usize = 64 * 1024;

pub struct PresentTool;

#[async_trait]
impl ToolSpec for PresentTool {
    fn name(&self) -> &'static str {
        "present"
    }

    fn description(&self) -> &'static str {
        "Declare existing workspace files as final deliverables and return a handoff receipt. \
         Read-only: the files are not modified, moved, copied, or uploaded. For each of 1-32 \
         workspace-relative paths it records the display path, byte length, and SHA-256. Call \
         it after finishing an artifact so the tool call is the presentation point. It does \
         not create a delivery folder or a second artifact store."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "description": "Workspace-relative paths of existing regular files to present (1-32, no duplicates).",
                    "items": { "type": "string", "minLength": 1 },
                    "minItems": 1,
                    "maxItems": MAX_FILES
                },
                "note": {
                    "type": "string",
                    "description": "Optional short note about the presented artifacts."
                }
            },
            "required": ["files"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let paths = match input.get("files") {
            Some(Value::Array(paths)) => paths,
            Some(other) => {
                return Err(type_mismatch("files", other, "an array of paths"));
            }
            None => return Err(ToolError::missing_field("files")),
        };
        if paths.is_empty() {
            return Err(ToolError::invalid_input(
                "`files` must contain at least one path",
            ));
        }
        if paths.len() > MAX_FILES {
            return Err(ToolError::invalid_input(format!(
                "`files` contains {} paths; present at most {MAX_FILES} per call",
                paths.len()
            )));
        }
        let note = optional_str(&input, "note")?
            .map(str::trim)
            .filter(|note| !note.is_empty());
        let workspace = tokio::fs::canonicalize(&context.workspace)
            .await
            .unwrap_or_else(|_| context.workspace.clone());
        let mut seen = HashSet::with_capacity(paths.len());
        let mut presented = Vec::with_capacity(paths.len());

        for (index, value) in paths.iter().enumerate() {
            let Some(raw) = value.as_str() else {
                let field = format!("files[{index}]");
                return Err(type_mismatch(&field, value, "a path string"));
            };
            if raw.trim().is_empty() {
                return Err(ToolError::invalid_input(format!(
                    "`files[{index}]` must not be empty"
                )));
            }
            let resolved = context.resolve_path(raw)?;
            if !context.trust_mode
                && !resolved.starts_with(&workspace)
                && !context
                    .trusted_external_paths
                    .iter()
                    .any(|root| resolved.starts_with(root))
            {
                return Err(ToolError::path_escape(resolved));
            }
            if !seen.insert(resolved.clone()) {
                return Err(ToolError::invalid_input(format!(
                    "`files` names the same file more than once: {raw}"
                )));
            }
            let display = resolved
                .strip_prefix(&workspace)
                .unwrap_or(resolved.as_path())
                .to_string_lossy()
                .into_owned();
            let (bytes, sha256) = hash_file(&resolved, &display).await?;
            presented.push((display, bytes, sha256));
        }

        let entries = presented
            .iter()
            .map(|(path, bytes, sha256)| json!({ "path": path, "bytes": bytes, "sha256": sha256 }))
            .collect::<Vec<_>>();
        let mut metadata = json!({ "presented": entries });
        if let Some(note) = note {
            metadata["note"] = Value::String(note.to_string());
        }

        let mut summary = format!(
            "Presented {} file{}. Handoff receipt only; nothing was written, moved, copied, or uploaded.",
            presented.len(),
            if presented.len() == 1 { "" } else { "s" }
        );
        for (path, bytes, sha256) in &presented {
            summary.push_str(&format!("\n- {path} ({bytes} bytes, sha256 {sha256})"));
        }
        if let Some(note) = note {
            summary.push_str(&format!("\nNote: {note}"));
        }

        Ok(ToolResult::success(summary).with_metadata(metadata))
    }
}

async fn hash_file(path: &Path, display: &str) -> Result<(u64, String), ToolError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| ToolError::invalid_input(format!("cannot present {display}: {error}")))?;
    if !metadata.is_file() {
        return Err(ToolError::invalid_input(format!(
            "cannot present {display}: not a regular file"
        )));
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| ToolError::invalid_input(format!("cannot present {display}: {error}")))?;
    if !file
        .metadata()
        .await
        .map_err(|error| {
            ToolError::execution_failed(format!("failed to inspect {display}: {error}"))
        })?
        .is_file()
    {
        return Err(ToolError::invalid_input(format!(
            "cannot present {display}: not a regular file"
        )));
    }
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).await.map_err(|error| {
            ToolError::execution_failed(format!("failed to read {display}: {error}"))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    Ok((
        bytes,
        codewhale_runtime::hashing::hex_bytes(hasher.finalize()),
    ))
}
