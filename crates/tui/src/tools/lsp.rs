//! Model-facing LSP code-intelligence tool.
//!
//! Extends the existing [`codewhale_lsp::LspManager`] lifecycle — never spawns a
//! competing server pool. Operations: diagnostics, read_lints, symbols,
//! definition, references.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

use codewhale_lsp::LintReadStatus;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, required_str,
};

/// Model-callable LSP intelligence surface.
pub struct LspTool;

#[async_trait]
impl ToolSpec for LspTool {
    fn name(&self) -> &'static str {
        "lsp"
    }

    fn description(&self) -> &'static str {
        "Query the configured session LSP for diagnostics, symbols, definitions, \
         references, or read_lints: 1-16 newline-separated files with \
         success/error/timeout; warnings follow include_warnings."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["diagnostics", "read_lints", "symbols", "definition", "references"],
                    "description": "Operation. read_lints reports per-file status, counts/count_complete, and truncation."
                },
                "path": {
                    "type": "string",
                    "description": "Source path. For read_lints: 1-16 newline-separated workspace-relative files."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line."
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "default": 1,
                    "description": "1-based column (default 1)."
                },
                "query": {
                    "type": "string",
                    "description": "Workspace symbol query."
                }
            },
            "required": ["operation", "path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let operation = required_str(&input, "operation")?;
        let path_raw = required_str(&input, "path")?;
        let line = input.get("line").and_then(|v| v.as_u64()).map(|n| n as u32);
        let character = input
            .get("character")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let query = optional_str(&input, "query")?;

        if operation == "read_lints" {
            let paths = path_raw
                .split('\n')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            return execute_read_lints(json!({"paths": paths}), context).await;
        }

        let manager = context.lsp_manager.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "LSP manager is not attached to this tool context (LSP unavailable for this session)",
            )
        })?;

        let path = context.resolve_path(path_raw)?;
        let payload = manager
            .intelligence(operation, &path, line, character, query)
            .await
            .map_err(ToolError::execution_failed)?;

        Ok(ToolResult::success(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        ))
    }
}

const MAX_LINT_PATHS: usize = 16;
const MAX_LINT_DIAGNOSTICS: usize = 100;
const MAX_LINT_MESSAGE_CHARS: usize = 512;
const MAX_LINT_OUTPUT_BYTES: usize = 12_000;

#[derive(Serialize)]
struct ReadLintsDiagnostic {
    line: u32,
    column: u32,
    severity: String,
    message: String,
    #[serde(skip_serializing_if = "is_false")]
    message_truncated: bool,
}

#[derive(Serialize)]
struct ReadLintsFile {
    #[serde(flatten)]
    freshness: codewhale_lsp::DiagnosticFreshness,
    file: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    error_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
    diagnostics: Vec<ReadLintsDiagnostic>,
    diagnostic_count: usize,
    total_diagnostic_count: Option<usize>,
    count_complete: bool,
    truncated: bool,
}

#[derive(Serialize)]
struct ReadLintsOutput {
    files: Vec<ReadLintsFile>,
    file_count: usize,
    total_file_count: usize,
    diagnostic_count: usize,
    total_diagnostic_count: Option<usize>,
    count_complete: bool,
    truncated: bool,
}

impl ReadLintsOutput {
    fn refresh_returned_metadata(&mut self) {
        for file in &mut self.files {
            file.diagnostic_count = file.diagnostics.len();
            file.truncated |= file
                .total_diagnostic_count
                .is_some_and(|total| file.diagnostic_count < total);
        }
        self.file_count = self.files.len();
        self.diagnostic_count = self.files.iter().map(|file| file.diagnostic_count).sum();
        self.truncated =
            self.file_count < self.total_file_count || self.files.iter().any(|file| file.truncated);
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn bounded_text(value: &str, max_chars: usize) -> (String, bool) {
    let mut chars = value.chars();
    let bounded = chars.by_ref().take(max_chars).collect();
    (bounded, chars.next().is_some())
}

/// Read bounded diagnostics for several existing files without requiring a
/// preceding edit. The model-facing entry point is the `lsp` operation above;
/// keeping this as a helper avoids adding a second catalog tool name.
async fn execute_read_lints(input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let raw_paths = input
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| ToolError::invalid_input("paths must be a non-empty array"))?;
    if raw_paths.is_empty() || raw_paths.len() > MAX_LINT_PATHS {
        return Err(ToolError::invalid_input(format!(
            "paths must contain between 1 and {MAX_LINT_PATHS} files"
        )));
    }

    let paths = raw_paths
        .iter()
        .map(|value| {
            let raw = value
                .as_str()
                .ok_or_else(|| ToolError::invalid_input("each paths entry must be a string"))?;
            resolve_lint_path(&context.workspace, raw)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let manager = context.lsp_manager.as_ref().ok_or_else(|| {
        ToolError::execution_failed(
            "LSP manager is not attached to this tool context; enable LSP for this session",
        )
    })?;
    let results = manager
        .diagnostics_for_paths(&paths)
        .await
        .map_err(ToolError::execution_failed)?;

    let total_file_count = results.len();
    let total_diagnostic_count = results.iter().try_fold(0usize, |count, result| {
        result
            .total_diagnostic_count
            .map(|total| count.saturating_add(total))
    });
    let count_complete = total_diagnostic_count.is_some();
    let mut remaining_diagnostics = MAX_LINT_DIAGNOSTICS;
    let mut files = Vec::with_capacity(results.len());
    for result in results {
        let file_total_diagnostic_count = result.total_diagnostic_count;
        let file_count_complete = file_total_diagnostic_count.is_some();
        let (status, error, error_truncated, timeout_ms) = match result.status {
            LintReadStatus::Success => ("success", None, false, None),
            LintReadStatus::Error(error) => {
                let (error, truncated) = bounded_text(&error, MAX_LINT_MESSAGE_CHARS);
                ("error", Some(error), truncated, None)
            }
            LintReadStatus::Timeout { wait_ms } => (
                "timeout",
                Some(format!("LSP diagnostics timed out after {wait_ms} ms")),
                false,
                Some(wait_ms),
            ),
        };
        let available_count = result.items.len();
        let take_count = available_count.min(remaining_diagnostics);
        remaining_diagnostics -= take_count;
        let mut message_was_truncated = false;
        let diagnostics = result
            .items
            .into_iter()
            .take(take_count)
            .map(|diagnostic| {
                let (message, message_truncated) =
                    bounded_text(&diagnostic.message, MAX_LINT_MESSAGE_CHARS);
                message_was_truncated |= message_truncated;
                ReadLintsDiagnostic {
                    line: diagnostic.line,
                    column: diagnostic.column,
                    severity: format!("{:?}", diagnostic.severity).to_ascii_lowercase(),
                    message,
                    message_truncated,
                }
            })
            .collect::<Vec<_>>();
        files.push(ReadLintsFile {
            freshness: result.freshness,
            file: result.file.display().to_string(),
            status,
            error,
            error_truncated,
            timeout_ms,
            diagnostic_count: diagnostics.len(),
            total_diagnostic_count: file_total_diagnostic_count,
            count_complete: file_count_complete,
            truncated: result.truncated
                || take_count < available_count
                || message_was_truncated
                || error_truncated,
            diagnostics,
        });
    }

    let mut output = ReadLintsOutput {
        files,
        file_count: total_file_count,
        total_file_count,
        diagnostic_count: 0,
        total_diagnostic_count,
        count_complete,
        truncated: false,
    };
    output.refresh_returned_metadata();
    loop {
        let output_len = serde_json::to_vec(&output)
            .map_err(|error| ToolError::execution_failed(error.to_string()))?
            .len();
        if output_len <= MAX_LINT_OUTPUT_BYTES {
            break;
        }
        if let Some(file) = output
            .files
            .iter_mut()
            .rev()
            .find(|file| !file.diagnostics.is_empty())
        {
            file.diagnostics.pop();
            file.truncated = true;
        } else if output.files.pop().is_none() {
            return Err(ToolError::execution_failed(
                "read_lints metadata exceeded its output bound",
            ));
        }
        output.refresh_returned_metadata();
    }

    ToolResult::json(&output).map_err(|error| ToolError::execution_failed(error.to_string()))
}

fn resolve_lint_path(workspace: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let raw = raw.trim();
    let candidate = Path::new(raw);
    if raw.is_empty() || candidate.is_absolute() {
        return Err(ToolError::permission_denied(
            "read_lints paths must be non-empty workspace-relative files",
        ));
    }
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ToolError::permission_denied(
            "read_lints paths cannot contain '..' traversal",
        ));
    }
    let workspace = workspace.canonicalize().map_err(|error| {
        ToolError::execution_failed(format!("failed to resolve workspace: {error}"))
    })?;
    let path = workspace.join(candidate).canonicalize().map_err(|error| {
        ToolError::execution_failed(format!("failed to read_lints path {raw}: {error}"))
    })?;
    if !path.starts_with(&workspace) {
        return Err(ToolError::permission_denied(
            "read_lints path resolves outside the workspace",
        ));
    }
    if !path.is_file() {
        return Err(ToolError::invalid_input(format!(
            "read_lints path is not a file: {raw}"
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::ToolContext;
    use async_trait::async_trait;
    use codewhale_lsp::{Diagnostic, Language, LspConfig, LspManager, Severity};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tempfile::tempdir;

    struct CountingTransport {
        calls: AtomicUsize,
        request_calls: AtomicUsize,
    }

    #[async_trait]
    impl codewhale_lsp::LspTransport for CountingTransport {
        async fn diagnostics_for(
            &self,
            _path: &Path,
            _text: &str,
            _wait: Duration,
        ) -> anyhow::Result<codewhale_lsp::client::DiagnosticPublication> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![Diagnostic {
                line: 1,
                column: 1,
                severity: Severity::Error,
                message: "boom".into(),
            }]
            .into())
        }

        async fn request(
            &self,
            method: &str,
            _params: Value,
            _wait: Duration,
        ) -> anyhow::Result<Value> {
            self.request_calls.fetch_add(1, Ordering::Relaxed);
            Ok(json!({ "method": method, "locations": [] }))
        }

        async fn shutdown(&self) {}
    }

    struct EmptyTransport;

    #[async_trait]
    impl codewhale_lsp::LspTransport for EmptyTransport {
        async fn diagnostics_for(
            &self,
            _path: &Path,
            _text: &str,
            _wait: Duration,
        ) -> anyhow::Result<codewhale_lsp::client::DiagnosticPublication> {
            Ok(Vec::new().into())
        }

        async fn request(
            &self,
            _method: &str,
            _params: Value,
            _wait: Duration,
        ) -> anyhow::Result<Value> {
            Ok(json!({}))
        }

        async fn shutdown(&self) {}
    }

    struct FixedTransport {
        items: Vec<Diagnostic>,
    }

    #[async_trait]
    impl codewhale_lsp::LspTransport for FixedTransport {
        async fn diagnostics_for(
            &self,
            _path: &Path,
            _text: &str,
            _wait: Duration,
        ) -> anyhow::Result<codewhale_lsp::client::DiagnosticPublication> {
            Ok(self.items.clone().into())
        }

        async fn shutdown(&self) {}
    }

    struct ErrorTransport;

    #[async_trait]
    impl codewhale_lsp::LspTransport for ErrorTransport {
        async fn diagnostics_for(
            &self,
            _path: &Path,
            _text: &str,
            _wait: Duration,
        ) -> anyhow::Result<codewhale_lsp::client::DiagnosticPublication> {
            anyhow::bail!("server exploded")
        }

        async fn shutdown(&self) {}
    }

    struct TimeoutTransport;

    #[async_trait]
    impl codewhale_lsp::LspTransport for TimeoutTransport {
        async fn diagnostics_for(
            &self,
            _path: &Path,
            _text: &str,
            _wait: Duration,
        ) -> anyhow::Result<codewhale_lsp::client::DiagnosticPublication> {
            std::future::pending().await
        }

        async fn shutdown(&self) {}
    }

    #[test]
    fn schema_documents_read_lints_contract_query_and_character_default() {
        assert!(LspTool.description().contains("1-16 newline-separated"));
        assert!(LspTool.description().contains("success/error/timeout"));
        assert!(LspTool.description().contains("include_warnings"));
        let schema = LspTool.input_schema();
        let properties = &schema["properties"];
        assert!(
            properties["operation"]["description"]
                .as_str()
                .unwrap()
                .contains("counts/count_complete")
        );
        assert!(
            properties["path"]["description"]
                .as_str()
                .unwrap()
                .contains("1-16 newline-separated")
        );
        assert_eq!(properties["line"]["description"], "1-based line.");
        assert_eq!(properties["character"]["default"], 1);
        assert_eq!(
            properties["character"]["description"],
            "1-based column (default 1)."
        );
        assert_eq!(
            properties["query"]["description"],
            "Workspace symbol query."
        );
    }

    #[tokio::test]
    async fn intelligence_paths_cannot_escape_before_transport_dispatch() {
        let root = tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("secret.rs");
        std::fs::write(&outside, "fn secret() {}\n").unwrap();
        std::fs::write(workspace.join("lib.rs"), "fn local() {}\n").unwrap();
        let mgr = Arc::new(LspManager::new(LspConfig::default(), workspace.clone()));
        let transport = Arc::new(CountingTransport {
            calls: AtomicUsize::new(0),
            request_calls: AtomicUsize::new(0),
        });
        mgr.install_test_transport(Language::Rust, transport.clone())
            .await;
        let ctx = ToolContext::new(&workspace).with_lsp_manager(mgr);
        let denied = vec![outside.display().to_string(), "../secret.rs".into()];
        #[cfg(unix)]
        let denied = {
            std::os::unix::fs::symlink(&outside, workspace.join("escape.rs")).unwrap();
            let mut denied = denied;
            denied.push("escape.rs".into());
            denied
        };
        for operation in ["diagnostics", "symbols", "definition", "references"] {
            for path in &denied {
                let result = LspTool
                    .execute(
                        json!({"operation": operation, "path": path, "line": 1}),
                        &ctx,
                    )
                    .await;
                assert!(result.is_err(), "{operation}: {path}");
            }
        }
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
        assert_eq!(transport.request_calls.load(Ordering::Relaxed), 0);
        for path in [
            "lib.rs".to_string(),
            workspace.join("lib.rs").display().to_string(),
        ] {
            assert!(
                LspTool
                    .execute(
                        json!({"operation": "definition", "path": path, "line": 1}),
                        &ctx
                    )
                    .await
                    .unwrap()
                    .success
            );
        }
        assert_eq!(transport.request_calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn tool_reuses_single_manager_transport_for_definition() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        tokio::fs::write(&path, b"fn main() {}").await.unwrap();

        let mgr = Arc::new(LspManager::new(
            LspConfig::default(),
            dir.path().to_path_buf(),
        ));
        let transport = Arc::new(CountingTransport {
            calls: AtomicUsize::new(0),
            request_calls: AtomicUsize::new(0),
        });
        mgr.install_test_transport(Language::Rust, transport.clone())
            .await;

        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let tool = LspTool;
        for _ in 0..2 {
            let result = tool
                .execute(
                    json!({
                        "operation": "definition",
                        "path": "lib.rs",
                        "line": 1,
                        "character": 4
                    }),
                    &ctx,
                )
                .await
                .expect("definition succeeds");
            assert!(result.success, "{}", result.content);
            assert!(result.content.contains("definition"));
        }
        assert_eq!(
            transport.request_calls.load(Ordering::Relaxed),
            2,
            "two definition calls"
        );
    }

    #[tokio::test]
    async fn diagnostics_operation_returns_items() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        tokio::fs::write(&path, b"fn main() {}").await.unwrap();

        let mgr = Arc::new(LspManager::new(
            LspConfig::default(),
            dir.path().to_path_buf(),
        ));
        let transport = Arc::new(CountingTransport {
            calls: AtomicUsize::new(0),
            request_calls: AtomicUsize::new(0),
        });
        mgr.install_test_transport(Language::Rust, transport.clone())
            .await;

        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(
                json!({ "operation": "diagnostics", "path": "lib.rs" }),
                &ctx,
            )
            .await
            .expect("diagnostics");
        assert!(result.success);
        assert!(result.content.contains("boom"));
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn read_lints_returns_structured_diagnostics_for_multiple_files() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("lib.rs");
        let second = dir.path().join("main.rs");
        tokio::fs::write(&first, b"fn lib() {}\n").await.unwrap();
        tokio::fs::write(&second, b"fn main() {}\n").await.unwrap();

        let mgr = Arc::new(LspManager::new(
            LspConfig::default(),
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(
            Language::Rust,
            Arc::new(CountingTransport {
                calls: AtomicUsize::new(0),
                request_calls: AtomicUsize::new(0),
            }),
        )
        .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(
                json!({
                    "operation": "read_lints",
                    "path": "lib.rs\nmain.rs"
                }),
                &ctx,
            )
            .await
            .expect("read_lints");
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(payload["files"].as_array().unwrap().len(), 2);
        assert_eq!(payload["file_count"], 2);
        assert_eq!(payload["total_file_count"], 2);
        assert_eq!(payload["diagnostic_count"], 2);
        assert_eq!(payload["total_diagnostic_count"], 2);
        assert_eq!(payload["count_complete"], true);
        assert_eq!(payload["truncated"], false);
        assert_eq!(payload["files"][0]["status"], "success");
        assert_eq!(payload["files"][0]["diagnostic_count"], 1);
        assert_eq!(payload["files"][0]["total_diagnostic_count"], 1);
        assert_eq!(payload["files"][0]["count_complete"], true);
        assert_eq!(payload["files"][0]["diagnostics"][0]["line"], 1);
        assert_eq!(payload["files"][0]["diagnostics"][0]["severity"], "error");
        assert_eq!(payload["files"][0]["diagnostics"][0]["message"], "boom");
    }

    #[tokio::test]
    async fn read_lints_preserves_files_with_empty_diagnostics() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        tokio::fs::write(&path, b"fn main() {}\n").await.unwrap();

        let mgr = Arc::new(LspManager::new(
            LspConfig::default(),
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(Language::Rust, Arc::new(EmptyTransport))
            .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(json!({"operation": "read_lints", "path": "lib.rs"}), &ctx)
            .await
            .expect("empty diagnostics are a successful read");
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(payload["diagnostic_count"], 0);
        assert_eq!(payload["total_diagnostic_count"], 0);
        assert_eq!(payload["count_complete"], true);
        assert_eq!(payload["truncated"], false);
        assert_eq!(payload["files"][0]["status"], "success");
        assert_eq!(payload["files"][0]["diagnostic_count"], 0);
        assert_eq!(payload["files"][0]["total_diagnostic_count"], 0);
        assert_eq!(payload["files"][0]["count_complete"], true);
        assert_eq!(payload["files"][0]["truncated"], false);
        assert_eq!(payload["files"][0]["diagnostics"], json!([]));
    }

    #[tokio::test]
    async fn read_lints_reports_file_read_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid.rs");
        tokio::fs::write(&path, [0xff]).await.unwrap();

        let mgr = Arc::new(LspManager::new(
            LspConfig::default(),
            dir.path().to_path_buf(),
        ));
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(
                json!({"operation": "read_lints", "path": "invalid.rs"}),
                &ctx,
            )
            .await
            .expect("read failure is a per-file result");
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(payload["files"][0]["status"], "error");
        assert!(
            payload["files"][0]["error"]
                .as_str()
                .unwrap()
                .contains("failed to read file")
        );
        assert_eq!(payload["files"][0]["total_diagnostic_count"], Value::Null);
        assert_eq!(payload["files"][0]["count_complete"], false);
        assert_eq!(payload["total_diagnostic_count"], Value::Null);
        assert_eq!(payload["count_complete"], false);
        assert_eq!(payload["files"][0]["diagnostics"], json!([]));
    }

    #[tokio::test]
    async fn read_lints_distinguishes_server_error_and_timeout() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("error.rs"), b"fn main() {}\n")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("timeout.py"), b"pass\n")
            .await
            .unwrap();

        let mgr = Arc::new(LspManager::new(
            LspConfig {
                poll_after_edit_ms: 5,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(Language::Rust, Arc::new(ErrorTransport))
            .await;
        mgr.install_test_transport(Language::Python, Arc::new(TimeoutTransport))
            .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(
                json!({
                    "operation": "read_lints",
                    "path": "error.rs\ntimeout.py"
                }),
                &ctx,
            )
            .await
            .expect("per-file failures remain structured results");
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(payload["files"][0]["status"], "error");
        assert!(
            payload["files"][0]["error"]
                .as_str()
                .unwrap()
                .contains("server exploded")
        );
        assert_eq!(payload["files"][1]["status"], "timeout");
        assert_eq!(payload["files"][1]["timeout_ms"], 5);
        assert!(
            payload["files"][1]["error"]
                .as_str()
                .unwrap()
                .contains("timed out")
        );
        assert_eq!(payload["files"][0]["total_diagnostic_count"], Value::Null);
        assert_eq!(payload["files"][1]["total_diagnostic_count"], Value::Null);
        assert_eq!(payload["files"][0]["count_complete"], false);
        assert_eq!(payload["files"][1]["count_complete"], false);
        assert_eq!(payload["total_diagnostic_count"], Value::Null);
        assert_eq!(payload["count_complete"], false);
    }

    #[tokio::test]
    async fn read_lints_excludes_warnings_when_include_warnings_is_false() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        tokio::fs::write(&path, b"fn main() {}\n").await.unwrap();
        let items = vec![
            Diagnostic {
                line: 4,
                column: 1,
                severity: Severity::Hint,
                message: "hint".into(),
            },
            Diagnostic {
                line: 2,
                column: 1,
                severity: Severity::Warning,
                message: "warning".into(),
            },
            Diagnostic {
                line: 3,
                column: 1,
                severity: Severity::Information,
                message: "information".into(),
            },
            Diagnostic {
                line: 1,
                column: 1,
                severity: Severity::Error,
                message: "error".into(),
            },
        ];
        let mgr = Arc::new(LspManager::new(
            LspConfig::default(),
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(
            Language::Rust,
            Arc::new(FixedTransport {
                items: items.clone(),
            }),
        )
        .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr.clone());

        let result = LspTool
            .execute(json!({"operation": "read_lints", "path": "lib.rs"}), &ctx)
            .await
            .expect("read_lints");
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        let severities = payload["files"][0]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["severity"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(severities, vec!["error"]);
        assert_eq!(payload["files"][0]["total_diagnostic_count"], 1);
        assert_eq!(payload["files"][0]["count_complete"], true);
    }

    #[tokio::test]
    async fn read_lints_includes_warnings_when_configured() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("lib.rs"), b"fn main() {}\n")
            .await
            .unwrap();
        let items = vec![
            Diagnostic {
                line: 3,
                column: 1,
                severity: Severity::Information,
                message: "information".into(),
            },
            Diagnostic {
                line: 2,
                column: 1,
                severity: Severity::Warning,
                message: "warning".into(),
            },
            Diagnostic {
                line: 1,
                column: 1,
                severity: Severity::Error,
                message: "error".into(),
            },
        ];
        let mgr = Arc::new(LspManager::new(
            LspConfig {
                include_warnings: true,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(Language::Rust, Arc::new(FixedTransport { items }))
            .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(json!({"operation": "read_lints", "path": "lib.rs"}), &ctx)
            .await
            .expect("read_lints");
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        let severities = payload["files"][0]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["severity"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(severities, vec!["error", "warning"]);
        assert_eq!(payload["files"][0]["total_diagnostic_count"], 2);
        assert_eq!(payload["files"][0]["count_complete"], true);
    }

    #[tokio::test]
    async fn read_lints_propagates_underlying_truncation_and_total_count() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("lib.rs"), b"fn main() {}\n")
            .await
            .unwrap();
        let items = (0..4)
            .map(|index| Diagnostic {
                line: index + 1,
                column: 1,
                severity: Severity::Error,
                message: format!("error {index}"),
            })
            .collect();
        let mgr = Arc::new(LspManager::new(
            LspConfig {
                max_diagnostics_per_file: 2,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(Language::Rust, Arc::new(FixedTransport { items }))
            .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(json!({"operation": "read_lints", "path": "lib.rs"}), &ctx)
            .await
            .expect("read_lints");
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(payload["diagnostic_count"], 2);
        assert_eq!(payload["total_diagnostic_count"], 4);
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["files"][0]["diagnostic_count"], 2);
        assert_eq!(payload["files"][0]["total_diagnostic_count"], 4);
        assert_eq!(payload["files"][0]["truncated"], true);
    }

    #[tokio::test]
    async fn read_lints_outer_bounds_keep_returned_counts_truthful() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("lib.rs"), b"fn main() {}\n")
            .await
            .unwrap();
        let items = (0..105)
            .map(|index| Diagnostic {
                line: index + 1,
                column: 1,
                severity: Severity::Error,
                message: "x".repeat(MAX_LINT_MESSAGE_CHARS + 10),
            })
            .collect();
        let mgr = Arc::new(LspManager::new(
            LspConfig {
                max_diagnostics_per_file: 200,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(Language::Rust, Arc::new(FixedTransport { items }))
            .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(json!({"operation": "read_lints", "path": "lib.rs"}), &ctx)
            .await
            .expect("read_lints");
        assert!(result.content.len() <= MAX_LINT_OUTPUT_BYTES);
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        let returned = payload["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["diagnostics"].as_array().unwrap().len())
            .sum::<usize>();
        assert_eq!(payload["diagnostic_count"], returned);
        assert_eq!(payload["files"][0]["diagnostic_count"], returned);
        assert_eq!(payload["total_diagnostic_count"], 105);
        assert_eq!(payload["files"][0]["total_diagnostic_count"], 105);
        assert!(returned < MAX_LINT_DIAGNOSTICS);
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["files"][0]["truncated"], true);
        assert_eq!(
            payload["files"][0]["diagnostics"][0]["message_truncated"],
            true
        );
    }

    #[tokio::test]
    async fn read_lints_caps_short_diagnostics_at_one_hundred_globally() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("lib.rs"), b"fn lib() {}\n")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("main.rs"), b"fn main() {}\n")
            .await
            .unwrap();
        let items = (0..60)
            .map(|index| Diagnostic {
                line: index + 1,
                column: 1,
                severity: Severity::Error,
                message: "e".to_string(),
            })
            .collect();
        let mgr = Arc::new(LspManager::new(
            LspConfig {
                max_diagnostics_per_file: 200,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        ));
        mgr.install_test_transport(Language::Rust, Arc::new(FixedTransport { items }))
            .await;
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);

        let result = LspTool
            .execute(
                json!({"operation": "read_lints", "path": "lib.rs\nmain.rs"}),
                &ctx,
            )
            .await
            .expect("read_lints");
        let payload: Value = serde_json::from_str(&result.content).unwrap();

        assert_eq!(payload["diagnostic_count"], MAX_LINT_DIAGNOSTICS);
        assert_eq!(payload["total_diagnostic_count"], 120);
        assert_eq!(payload["count_complete"], true);
        assert_eq!(payload["files"][0]["diagnostic_count"], 60);
        assert_eq!(payload["files"][1]["diagnostic_count"], 40);
        assert_eq!(payload["files"][1]["total_diagnostic_count"], 60);
        assert_eq!(payload["files"][1]["truncated"], true);
        assert_eq!(payload["truncated"], true);
        assert!(result.content.len() <= MAX_LINT_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn disabled_lsp_hard_blocks_tool() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        tokio::fs::write(&path, b"fn main() {}").await.unwrap();
        let mgr = Arc::new(LspManager::new(
            LspConfig {
                enabled: false,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        ));
        let mut ctx = ToolContext::new(dir.path());
        ctx = ctx.with_lsp_manager(mgr);
        let err = LspTool
            .execute(
                json!({ "operation": "diagnostics", "path": "lib.rs" }),
                &ctx,
            )
            .await
            .expect_err("disabled must fail");
        assert!(
            err.to_string().contains("disabled"),
            "unexpected error: {err}"
        );

        let path_error = LspTool
            .execute(
                json!({"operation": "read_lints", "path": "../outside.rs"}),
                &ctx,
            )
            .await
            .expect_err("path traversal must fail closed");
        assert!(path_error.to_string().contains("cannot contain"));
    }
}
