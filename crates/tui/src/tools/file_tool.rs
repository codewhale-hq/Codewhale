//! Model-facing file tools and the legacy action-family compatibility wrapper.
//!
//! New turns see the small small-contract-style `read`, `write`, and `edit` primitives.
//! `File` remains registered but hidden so saved v0.9.x transcripts can replay
//! without teaching new sessions the older action-family schema.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::apply_patch::ApplyPatchTool;
use super::canonical_action::required_action;
use super::file::{EditFileTool, ListDirTool, ReadFileTool, WriteFileTool};
use super::file_search::FileSearchTool;
use super::search::GrepFilesTool;
use super::spec::{
    ApprovalRequirement, RichToolResult, ToolCapability, ToolContext, ToolError, ToolResult,
    ToolSpec,
};

/// Lift a parameter description out of the tool that implements the action.
///
/// Returns an empty string when the key is absent, which the
/// `wrapper_borrows_every_inner_description` test turns into a build-time
/// failure — a renamed inner parameter must not silently blank the only
/// description the model gets to read.
fn borrowed(schema: &Value, key: &str) -> String {
    schema
        .get("properties")
        .and_then(|properties| properties.get(key))
        .and_then(|property| property.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `borrowed`, tagged with the action the parameter belongs to.
fn describe(schema: &Value, key: &str, action: &str) -> String {
    let base = borrowed(schema, key);
    if base.is_empty() {
        return String::new();
    }
    format!("{} — {action}.", base.trim_end_matches('.'))
}

pub struct FileTool {
    name: &'static str,
    allow_writes: bool,
    allow_patch: bool,
}

/// Small model-facing reader with the same public contract as the small-contract built-in
/// `read` tool. The legacy [`ReadFileTool`] remains separately registered for
/// saved Codewhale transcripts.
pub struct ReadTool;

#[async_trait]
impl ToolSpec for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read a file. A text file comes back whole in one call when it fits this call's output budget — 100000 bytes by default, raisable to 500000 with max_bytes. There is no line cap. Use offset and limit for an exact line range; when output is budget-limited the footer names the exact offset to continue from. Every text response reports the file's byte size, line count, and whether output was truncated. A PNG, JPEG, GIF or WebP image comes back as image content you can see (large images are downscaled), including one the user attached from outside the workspace; read an image instead of running OCR or taking screenshots of it."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative or absolute)."
                },
                "offset": {
                    "type": "number",
                    "description": "Line number to start reading from (1-indexed)."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of lines to read."
                },
                "max_bytes": {
                    "type": "number",
                    "description": "Output budget in bytes for this one call. Defaults to 100000; values above the 500000 maximum are clamped down rather than rejected, and a value below the active default leaves the default in place."
                },
                "expected_revision": {
                    "type": "string",
                    "description": "Optional snapshot guard from a code navigation reference. Pass the reference's path, offset, limit and expected_revision together; changed source is refused instead of silently reading stale offsets. Omit for an ordinary read."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        ReadFileTool.capabilities()
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        ReadFileTool::execute_contract_read(input, context)
            .await
            .map(RichToolResult::into_result)
    }

    async fn execute_rich(
        &self,
        input: Value,
        context: &ToolContext,
    ) -> Result<RichToolResult, ToolError> {
        ReadFileTool::execute_contract_read(input, context).await
    }
}

/// Small model-facing whole-file writer backed by [`WriteFileTool`].
pub struct WriteTool;

#[async_trait]
impl ToolSpec for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        "Write content to a file. Creates the file if it does not exist, overwrites it if it does, and creates parent directories automatically."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative or absolute)."
                },
                "content": { "type": "string", "description": "Content to write to the file." }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        WriteFileTool.capabilities()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        WriteFileTool.approval_requirement()
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        WriteFileTool::execute_contract_write(input, context).await
    }
}

/// small-contract-style multi-edit surface. Every `oldText` is resolved against the same
/// original file and ranges must be unique and non-overlapping.
pub struct EditTool;

#[async_trait]
impl ToolSpec for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Edit one file with targeted text replacements. Every edits[].oldText must identify a unique, non-overlapping region of the original file. Merge nearby or overlapping changes into one edit."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative or absolute)."
                },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Text identifying one unique region to replace." },
                            "newText": {
                                "type": "string",
                                "description": "Replacement text; may be empty to delete the matched span."
                            }
                        },
                        "required": ["oldText", "newText"],
                        "additionalProperties": false
                    },
                    "description": "One or more disjoint replacements, all matched against the original file."
                }
            },
            "required": ["path", "edits"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        EditFileTool.capabilities()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        EditFileTool.approval_requirement()
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        EditFileTool::execute_contract_edits(input, context).await
    }
}

impl FileTool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            allow_writes: true,
            allow_patch: false,
        }
    }

    pub const fn with_patch(name: &'static str) -> Self {
        Self {
            name,
            allow_writes: true,
            allow_patch: true,
        }
    }

    #[cfg(test)]
    pub const fn read_only(name: &'static str) -> Self {
        Self {
            name,
            allow_writes: false,
            allow_patch: false,
        }
    }

    /// The action set this instance actually dispatches, in schema order.
    ///
    /// The `enum` the model reads and the name list its errors quote are both
    /// built from this, so a mode that hides `write` can never advertise it or
    /// suggest it in a refusal.
    fn available_actions(&self) -> Vec<&'static str> {
        let mut actions = vec!["read", "list", "search_name", "search_content"];
        if self.allow_writes {
            actions.extend(["write", "edit"]);
        }
        if self.allow_patch {
            actions.push("patch");
        }
        actions
    }

    /// Policy-side action resolution.
    ///
    /// Approval, read-only, and parallel-safety predicates cannot fail, so a
    /// missing action resolves to the most restrictive answer they can give.
    /// `execute` does **not** share this fallback — see `required_action`.
    fn resolve_action<'a>(&self, input: &'a Value) -> &'a str {
        input
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("read")
    }

    /// Execution-side action resolution: `action` is required, as the schema
    /// has always said it is.
    fn required_action(&self, input: &Value) -> Result<String, ToolError> {
        required_action(input, self.name, &self.available_actions())
    }

    fn strip_action(&self, input: Value) -> Result<Value, ToolError> {
        let mut input = input;
        if let Some(obj) = input.as_object_mut() {
            obj.remove("action");
            Ok(input)
        } else {
            Err(ToolError::invalid_input(
                "File tool input must be an object",
            ))
        }
    }
}

#[async_trait]
impl ToolSpec for FileTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Read, list, search, write, edit, or patch workspace files. Use read before edit; edit performs one exact replacement, while patch is best for multi-hunk or multi-file changes. Read/list/search actions are parallel-safe and do not require approval. Available actions depend on the active mode and feature policy."
    }

    fn input_schema(&self) -> Value {
        let actions = self.available_actions();
        // Every per-action parameter description is borrowed from the tool
        // that implements the action rather than restated here. The wrapper is
        // the only schema the model ever reads, and the restated copy had gone
        // stale: it advertised `max_lines` "default 200" long after the real
        // default became 500-with-a-16KB-budget, and a `blame` action `File`
        // has never had. Borrowing makes that class of drift impossible.
        let read = ReadFileTool.input_schema();
        let name_search = FileSearchTool.input_schema();
        let content_search = GrepFilesTool.input_schema();
        let write = WriteFileTool.input_schema();
        let edit = EditFileTool.input_schema();
        let patch = ApplyPatchTool.input_schema();
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": actions,
                    "description": "Action to perform"
                },
                "path": {
                    "type": "string",
                    "description": format!(
                        "{} Optional for list and search (default: .).",
                        borrowed(&read, "path"),
                    )
                },
                "start_line": {
                    "type": "integer",
                    "description": describe(&read, "start_line", "action=read")
                },
                "max_lines": {
                    "type": "integer",
                    "description": describe(&read, "max_lines", "action=read")
                },
                "pages": {
                    "type": "string",
                    "description": describe(&read, "pages", "action=read")
                },
                "content": {
                    "type": "string",
                    "description": describe(&write, "content", "action=write")
                },
                "search": {
                    "type": "string",
                    "description": describe(&edit, "search", "action=edit")
                },
                "replace": {
                    "oneOf": [
                        { "type": "string", "description": describe(&edit, "replace", "action=edit") },
                        { "type": "array", "items": { "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } }, "required": ["path", "content"] }, "description": "Full-file replacements — action=patch." }
                    ]
                },
                "fuzz": {
                    "type": "integer",
                    "description": describe(&patch, "fuzz", "action=patch")
                },
                "query": {
                    "type": "string",
                    "description": describe(&name_search, "query", "action=search_name")
                },
                "pattern": {
                    "type": "string",
                    "description": describe(&content_search, "pattern", "action=search_content")
                },
                "limit": {
                    "type": "integer",
                    "description": describe(&name_search, "limit", "action=search_name")
                },
                "max_results": {
                    "type": "integer",
                    "description": format!(
                        "{} search_name: alias for `limit`.",
                        describe(&content_search, "max_results", "action=search_content"),
                    )
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": describe(&name_search, "extensions", "action=search_name")
                },
                "include": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": describe(&content_search, "include", "action=search_content")
                },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": format!(
                        "{} Also action=search_name.",
                        describe(&content_search, "exclude", "action=search_content"),
                    )
                },
                "context_lines": {
                    "type": "integer",
                    "description": describe(&content_search, "context_lines", "action=search_content")
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": describe(&content_search, "case_insensitive", "action=search_content")
                },
                "patch": {
                    "type": "string",
                    "description": "Unified diff patch content for action=patch"
                },
                "changes": {
                    "type": "array",
                    "items": { "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } }, "required": ["path", "content"] },
                    "description": "Deprecated alias for replace in action=patch"
                },
                "create_if_missing": {
                    "type": "boolean",
                    "description": "Create files if missing for action=patch"
                },
                "expected_hash": {
                    "type": "string",
                    "description": describe(&edit, "expected_hash", "action=write/edit/patch")
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        let mut capabilities = vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable];
        if self.allow_writes || self.allow_patch {
            capabilities.extend([
                ToolCapability::WritesFiles,
                ToolCapability::RequiresApproval,
            ]);
        }
        capabilities
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match self.resolve_action(input) {
            "read" | "list" | "search_name" | "search_content" => ApprovalRequirement::Auto,
            "write" | "edit" | "patch" => ApprovalRequirement::Suggest,
            _ => ApprovalRequirement::Auto,
        }
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        matches!(
            self.resolve_action(input),
            "read" | "list" | "search_name" | "search_content"
        )
    }

    fn supports_parallel_for(&self, input: &Value) -> bool {
        matches!(
            self.resolve_action(input),
            "read" | "list" | "search_name" | "search_content"
        )
    }

    fn starts_detached_for(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = self.required_action(&input)?;
        if matches!(action.as_str(), "write" | "edit") && !self.allow_writes {
            return Err(ToolError::not_available(format!(
                "File action=\"{action}\" is unavailable in the current mode; nothing was written. Available actions here: {}. The user can change modes with /mode.",
                self.available_actions().join(", ")
            )));
        }
        if action == "patch" && !self.allow_patch {
            return Err(ToolError::not_available(format!(
                "File action=\"patch\" is unavailable because the patch feature is disabled; nothing was written. Available actions here: {}. When writes are available in this mode, action=\"edit\" replaces a single exact match and action=\"write\" replaces the whole file.",
                self.available_actions().join(", ")
            )));
        }
        let input = self.strip_action(input)?;

        match action.as_str() {
            "read" => ReadFileTool.execute(input, context).await,
            "list" => ListDirTool.execute(input, context).await,
            // The cross-action spellings the wrapper advertises
            // (`max_results` on search_name, `query`/`limit` on
            // search_content) used to be copied here. They are alias-table
            // entries on the implementing tools now — one mechanism, applied
            // before the same unknown-parameter check every other action runs,
            // and a direct call to the inner tool behaves identically.
            "search_name" => FileSearchTool.execute(input, context).await,
            "search_content" => GrepFilesTool.execute(input, context).await,
            "write" => WriteFileTool.execute(input, context).await,
            "edit" => EditFileTool.execute(input, context).await,
            "patch" => ApplyPatchTool.execute(input, context).await,
            other => Err(ToolError::invalid_input(format!(
                "Unknown File action \"{other}\"; nothing was run. Pass one of: {}.",
                self.available_actions().join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
