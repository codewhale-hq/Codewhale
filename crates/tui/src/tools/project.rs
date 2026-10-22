//! Project mapping tool for understanding codebase structure.

use crate::utils::{is_key_file, project_tree, summarize_key_files, summarize_project};
use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64,
};

pub struct ProjectMapTool;

#[derive(Debug, Serialize)]
struct ProjectMap {
    tree: String,
    summary: String,
    key_files: Vec<String>,
    key_file_soundings: std::collections::BTreeMap<String, String>,
    key_files_omitted: usize,
}

#[async_trait]
impl ToolSpec for ProjectMapTool {
    fn name(&self) -> &'static str {
        "project_map"
    }

    fn description(&self) -> &'static str {
        "Get a high-level map of the project structure, including key files and a tree view."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum depth for the tree view (default: 3)."
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let max_depth = optional_u64(&input, "max_depth", 3)? as usize;
        let workspace = context.workspace.clone();
        let follow_symlinks = context.follow_symlinks;
        let map = tokio::task::spawn_blocking(move || {
            generate_project_map(&workspace, max_depth, follow_symlinks)
        })
        .await
        .map_err(|error| {
            ToolError::execution_failed(format!("project_map worker failed: {error}"))
        })??;
        ToolResult::json(&map).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

fn generate_project_map(
    root: &std::path::Path,
    max_depth: usize,
    follow_symlinks: bool,
) -> Result<ProjectMap, ToolError> {
    let tree = project_tree(root, max_depth, follow_symlinks);

    // Reuse this observation for the summary only when both walks have the
    // same symlink policy.
    let mut key_files = Vec::new();
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(follow_symlinks)
        .max_depth(Some(2));
    let walker = builder.build();

    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_symlink()) && !follow_symlinks {
            continue;
        }
        if is_key_file(entry.path())
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            key_files.push(rel.to_string_lossy().to_string());
        }
    }

    // `follow_symlinks == true` keeps the original non-follow summary walk so
    // symlinked key files never leak into the summary.
    let summary = if follow_symlinks {
        summarize_project(root)
    } else {
        summarize_key_files(key_files.clone())
    };

    // Echolocation sounding: one line of symbols per key file (sorted,
    // capped), so the map says what lives where, not just what exists.
    let (key_file_soundings, key_files_omitted) =
        super::echolocation::sound_key_files(root, &key_files);

    Ok(ProjectMap {
        tree,
        summary,
        key_files,
        key_file_soundings,
        key_files_omitted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn project_map_sounds_key_file_symbols() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\nfn serve() {}\n")
            .expect("fixture");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("fixture");
        let context = ToolContext::new(dir.path().to_path_buf());
        let result = ProjectMapTool
            .execute(json!({}), &context)
            .await
            .expect("execute");
        assert!(result.success);
        let body: Value = serde_json::from_str(&result.content).expect("json result");
        assert_eq!(
            body["key_file_soundings"]["main.rs"],
            "2 symbols: fn main:1, fn serve:2"
        );
        assert!(body["key_file_soundings"].get("Cargo.toml").is_none());
    }
}
