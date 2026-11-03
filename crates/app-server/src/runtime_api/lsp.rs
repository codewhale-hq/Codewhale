//! LSP-over-HTTP for native clients (APPS-93).
//!
//! A native file view needs live diagnostics and semantic references, not just
//! receipt-projected ones. These routes serve the *server workspace* through
//! one lazily-built [`LspManager`]; engine threads keep their own per-thread
//! managers for the post-edit hook. Language servers spawn on first use only —
//! a server that never serves an LSP route never pays for one.
//!
//! Fail-closed as data: a file with no language server, a disabled `[lsp]`
//! config, or an LSP timeout all answer `200` with `ok: false` + a machine-
//! readable `reason`. Only malformed input (bad path, missing `line`) is an
//! HTTP error — a file without a server is a normal state, not a failure.
//!
//! Routes:
//!   GET /v1/lsp          — capability: enabled, languages, operations
//!   GET /v1/diagnostics  — ?path= (workspace-relative)
//!   GET /v1/definition   — ?path=&line=&character= (1-based)
//!   GET /v1/references   — ?path=&line=&character= (1-based)
//!   GET /v1/symbols      — ?path=&query= (query empty → document symbols)

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use super::workspace::{canonical_workspace, precheck_file_target, relative_request_path};
use super::{ApiError, RuntimeApiState};
use codewhale_lsp::LspManager;
use codewhale_lsp::registry::Language;

const LSP_OPERATIONS: &[&str] = &["diagnostics", "symbols", "definition", "references"];
const LANGUAGES: &[Language] = &[
    Language::Rust,
    Language::Go,
    Language::Python,
    Language::TypeScript,
    Language::JavaScript,
    Language::Java,
    Language::Php,
    Language::Vue,
    Language::C,
    Language::Cpp,
];

/// The shared workspace manager: built once, from the live config's `[lsp]`
/// table and the canonical workspace root.
async fn lsp_manager(state: &RuntimeApiState) -> Result<Arc<LspManager>, ApiError> {
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
    let config = state
        .config
        .read()
        .lsp
        .clone()
        .map(codewhale_lsp::LspConfig::from)
        .unwrap_or_default();
    Ok(state
        .workspace_scope
        .lsp
        .get_or_init(|| {
            Arc::new(LspManager::new(
                config,
                state.workspace_scope.canonical.clone(),
            ))
        })
        .clone())
}

/// Resolve a workspace-relative `path` to an absolute file inside the
/// workspace, refusing traversal, `.git`, links, and missing files — the same
/// confinement the file routes apply.
///
/// Async because the confinement it applies is filesystem work:
/// `canonical_workspace` canonicalizes and `precheck_file_target` stats every
/// component. Both are blocking syscalls, so they ride `spawn_blocking` rather
/// than the caller's Tokio worker (#6149). The blocking-calls budget cannot
/// see this — the `std::fs` calls live in `workspace.rs`, so a handler calling
/// straight through to them is invisible to a per-file scanner.
async fn resolve_workspace_file(state: &RuntimeApiState, raw: &str) -> Result<PathBuf, ApiError> {
    let relative = relative_request_path(raw, false)?;
    let workspace = state.workspace.clone();
    tokio::task::spawn_blocking(move || {
        let root = canonical_workspace(&workspace)?;
        precheck_file_target(&root, &relative)?
            .ok_or_else(|| ApiError::not_found("file not found"))?;
        Ok(root.join(&relative))
    })
    .await
    .map_err(|_| ApiError::internal("workspace file resolution failed"))?
}

/// `intelligence` reports ordinary states (disabled, no server) as error
/// strings; split them back into the honest machine-readable reasons.
fn lsp_failure(error: String) -> Value {
    let reason = if error == "stale_document" {
        "stale_document"
    } else if error.contains("no LSP server") {
        "no_server"
    } else if error.contains("disabled") {
        "lsp_disabled"
    } else if error.contains("timed out") {
        "timeout"
    } else {
        "lsp_error"
    };
    json!({ "ok": false, "reason": reason, "detail": error })
}

async fn run_intelligence(
    state: &RuntimeApiState,
    operation: &str,
    file: PathBuf,
    line: Option<u32>,
    character: Option<u32>,
    query: Option<String>,
    expected_revision: Option<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = lsp_manager(state).await?;
    if !manager.config().enabled {
        return Ok(Json(
            json!({ "ok": false, "reason": "lsp_disabled", "enabled": false }),
        ));
    }
    let result = manager
        .intelligence_at_revision(
            operation,
            &file,
            line,
            character,
            query.as_deref(),
            expected_revision.as_deref(),
        )
        .await;
    match result {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.insert("ok".to_string(), Value::Bool(true));
            }
            Ok(Json(value))
        }
        Err(error) => Ok(Json(lsp_failure(error))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LspFileQuery {
    path: String,
    expected_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LspPositionQuery {
    path: String,
    expected_revision: Option<String>,
    /// 1-based line; required by definition/references.
    line: Option<u32>,
    /// 1-based column; defaults to 1.
    character: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LspSymbolsQuery {
    path: String,
    expected_revision: Option<String>,
    query: Option<String>,
}

/// `GET /v1/lsp` — capability report: what this runtime can serve without
/// probing or spawning anything.
pub(super) async fn lsp_status(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    let manager = lsp_manager(&state).await?;
    let config = manager.config();
    let languages: Vec<Value> = LANGUAGES
        .iter()
        .filter_map(|language| {
            config
                .resolve_command(*language)
                .map(|(command, _)| json!({ "language": language.as_key(), "server": command }))
        })
        .collect();
    let custom: Vec<Value> = config
        .custom
        .iter()
        .map(|(extension, def)| {
            json!({
                "extension": extension,
                "language_id": def.language_id,
                "server": def.command,
            })
        })
        .collect();
    Ok(Json(json!({
        "enabled": config.enabled,
        "diagnostics_contract_version": 1,
        "semantic_contract_version": 1,
        "position_encoding": "utf-16",
        "capability_source": "configuration",
        "server_probe": "on_request",
        "workspace": state.workspace.display().to_string(),
        "operations": LSP_OPERATIONS,
        "languages": languages,
        "custom_languages": custom,
        "poll_after_edit_ms": config.poll_after_edit_ms,
        "max_diagnostics_per_file": config.max_diagnostics_per_file,
        "include_warnings": config.include_warnings,
    })))
}

pub(super) async fn lsp_diagnostics(
    State(state): State<RuntimeApiState>,
    Query(query): Query<LspFileQuery>,
) -> Result<Json<Value>, ApiError> {
    let file = resolve_workspace_file(&state, &query.path).await?;
    let Json(result) =
        run_intelligence(&state, "diagnostics", file, None, None, None, None).await?;
    if result.get("ok").and_then(Value::as_bool) == Some(true)
        && query.expected_revision.as_deref().is_some_and(|expected| {
            result.get("source_revision").and_then(Value::as_str) != Some(expected)
        })
    {
        return Ok(Json(json!({"ok":false,"reason":"stale_document"})));
    }
    Ok(Json(result))
}

pub(super) async fn lsp_definition(
    State(state): State<RuntimeApiState>,
    Query(query): Query<LspPositionQuery>,
) -> Result<Json<Value>, ApiError> {
    let line = query
        .line
        .ok_or_else(|| ApiError::bad_request("definition requires line (1-based)"))?;
    let file = resolve_workspace_file(&state, &query.path).await?;
    run_intelligence(
        &state,
        "definition",
        file,
        Some(line),
        query.character,
        None,
        query.expected_revision,
    )
    .await
}

pub(super) async fn lsp_references(
    State(state): State<RuntimeApiState>,
    Query(query): Query<LspPositionQuery>,
) -> Result<Json<Value>, ApiError> {
    let line = query
        .line
        .ok_or_else(|| ApiError::bad_request("references requires line (1-based)"))?;
    let file = resolve_workspace_file(&state, &query.path).await?;
    run_intelligence(
        &state,
        "references",
        file,
        Some(line),
        query.character,
        None,
        query.expected_revision,
    )
    .await
}

pub(super) async fn lsp_symbols(
    State(state): State<RuntimeApiState>,
    Query(query): Query<LspSymbolsQuery>,
) -> Result<Json<Value>, ApiError> {
    let file = resolve_workspace_file(&state, &query.path).await?;
    run_intelligence(
        &state,
        "symbols",
        file,
        None,
        None,
        query.query,
        query.expected_revision,
    )
    .await
}

#[cfg(test)]
mod diagnostic_freshness_tests {
    use super::*;

    #[test]
    fn semantic_queries_accept_expected_revision_without_changing_position_units() {
        let position: LspPositionQuery = serde_json::from_value(
            json!({"path":"a.rs","line":2,"character":4,"expected_revision":"abc"}),
        )
        .unwrap();
        assert_eq!(position.expected_revision.as_deref(), Some("abc"));
        assert_eq!(position.line, Some(2));
        assert_eq!(position.character, Some(4));
        let symbols: LspSymbolsQuery =
            serde_json::from_value(json!({"path":"a.rs","expected_revision":"abc"})).unwrap();
        assert_eq!(symbols.expected_revision.as_deref(), Some("abc"));
        assert_eq!(
            lsp_failure("stale_document".into())["reason"],
            "stale_document"
        );
    }

    #[test]
    fn failures_keep_unavailable_disabled_and_timeout_distinct() {
        for (error, reason) in [
            ("no LSP server is available for this file", "no_server"),
            ("LSP is disabled ([lsp] enabled = false)", "lsp_disabled"),
            ("LSP diagnostics timed out after 5 ms", "timeout"),
            (
                "LSP diagnostics request failed: server crashed",
                "lsp_error",
            ),
        ] {
            let result = lsp_failure(error.to_owned());
            assert_eq!(result["ok"], false);
            assert_eq!(result["reason"], reason);
        }
    }
}
