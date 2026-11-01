//! Post-edit LSP diagnostics hooks for engine tool execution.
//!
//! The turn loop only needs to ask "did a successful edit produce diagnostics?"
//! This module owns the tool-input path extraction and the synthetic diagnostic
//! message injection so the top-level engine module stays focused on session
//! orchestration.

use std::path::PathBuf;

use codewhale_tools::patch_preflight::preflight_apply_patch;

use super::*;

/// #136: derive the file path(s) edited by a tool call. Returns the empty
/// vec for tools that don't modify files. We intentionally only handle the
/// three known edit tools — adding more (e.g. specialized refactor tools)
/// is a one-line change here.
pub(super) fn edited_paths_for_tool(tool_name: &str, input: &serde_json::Value) -> Vec<PathBuf> {
    let semantic = codewhale_execpolicy::canonical_action::canonical_action_alias(tool_name, input);
    match semantic {
        "edit_file" | "write_file" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                vec![PathBuf::from(path)]
            } else {
                Vec::new()
            }
        }
        // A section that deletes its file (`+++ /dev/null`) leaves nothing
        // to diagnose, so only files that still exist afterwards are listed.
        "apply_patch" => preflight_apply_patch(input)
            .map(|preflight| {
                preflight
                    .touched_files
                    .into_iter()
                    .filter(|path| !preflight.deletes.contains(path))
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

impl Engine {
    /// #136: post-edit hook. Inspects the tool name + input, derives the
    /// edited file path, and asks the LSP manager for diagnostics. The
    /// rendered block is queued in `pending_lsp_blocks` and flushed to the
    /// session message stream just before the next API request. Failure is
    /// silent by design — a missing/crashing LSP server must never block
    /// the agent.
    pub(super) async fn run_post_edit_lsp_hook(
        &mut self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) {
        if !self.lsp_manager.config().enabled {
            return;
        }
        let paths = edited_paths_for_tool(tool_name, tool_input);
        let mut found = 0usize;
        let mut files = 0usize;
        for path in paths {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                self.session.workspace.join(&path)
            };
            // Use a short edit-sequence based on the existing turn counter so
            // log output stays correlated even though we do not currently
            // batch by sequence.
            let seq = self.turn_counter;
            if let Some(block) = self.lsp_manager.diagnostics_for(&absolute, seq).await {
                found = found.saturating_add(block.items.len());
                files = files.saturating_add(1);
                self.pending_lsp_blocks.push(block);
            }
        }
        if found > 0 {
            let _ = self
                .send_event(Event::LspRepairUpdate {
                    diagnostics_found: found,
                    files,
                    injected: false,
                })
                .await;
        }
    }

    /// Drain `pending_lsp_blocks` into a single synthetic user message so the
    /// model sees the diagnostics on its next request. Skips when nothing is
    /// pending. The message uses the standard `text` content block shape
    /// (the same shape as the post-tool steer messages) so we don't need to
    /// invent a new envelope.
    pub(super) async fn flush_pending_lsp_diagnostics(&mut self) {
        if self.pending_lsp_blocks.is_empty() {
            return;
        }
        let blocks = std::mem::take(&mut self.pending_lsp_blocks);
        let found: usize = blocks.iter().map(|b| b.items.len()).sum();
        let files = blocks.len();
        let rendered = codewhale_lsp::render_blocks(&blocks);
        if rendered.is_empty() {
            return;
        }
        self.add_session_message(self.runtime_text_message_with_turn_metadata(
            rendered,
            crate::core::ops::UserInputProvenance::Runtime,
        ))
        .await;
        let _ = self
            .send_event(Event::LspRepairUpdate {
                diagnostics_found: found,
                files,
                injected: true,
            })
            .await;
    }
}

#[cfg(test)]
mod primitive_name_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lowercase_file_mutations_reach_the_lsp_hook() {
        for name in ["write", "edit", "write_file", "edit_file"] {
            assert_eq!(
                edited_paths_for_tool(name, &json!({"path": "src/lib.rs"})),
                vec![PathBuf::from("src/lib.rs")]
            );
        }
    }
}
