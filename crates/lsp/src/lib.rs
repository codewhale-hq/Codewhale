//! LSP integration: post-edit diagnostics injection (#136).
//!
//! After the agent performs a successful file edit (`edit_file`,
//! `apply_patch`, or `write_file`) the engine asks the [`LspManager`] for
//! diagnostics on that file. The manager spawns the appropriate LSP server
//! lazily on first use, sends `didOpen`/`didChange`, waits up to a bounded
//! timeout for `publishDiagnostics`, normalizes the result, and returns it
//! to the engine.
//!
//! Failure modes are non-blocking by design: a missing LSP binary, a
//! crashed server, or a timeout all degrade to "no diagnostics this turn"
//! rather than stalling the agent. We log a one-time warning per language
//! when the binary is missing.
//!
//! # Wiring
//!
//! ```text
//! Engine  ── after successful edit ──▶  LspManager.diagnostics_for(path, seq)
//!                                              │
//!                                              ▼
//!                                       per-language LspClient
//!                                              │
//!                                              ▼
//!                                      LspTransport (stdio)
//! ```
//!
//! # Configuration
//!
//! The `[lsp]` table in `~/.deepseek/config.toml` controls behavior:
//! `enabled`, `poll_after_edit_ms`, `max_diagnostics_per_file`, `include_warnings`,
//! an optional `servers` override, and a `custom` table for registering LSP
//! servers for file extensions not covered by the built-in registry (e.g. Ruby,
//! PHP, C#). See [`LspConfig`] for defaults and `config.example.toml` for
//! documentation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

pub mod client;
pub mod diagnostics;
pub mod registry;

pub use client::{LspTransport, StdioLspTransport};
pub use diagnostics::{Diagnostic, DiagnosticBlock, Severity, render_blocks};
pub use registry::Language;

use codewhale_config::CustomLspDef;

/// `[lsp]` config schema. Mirrors the TOML keys documented in
/// `config.example.toml`. Unknown keys are ignored.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LspConfig {
    /// Master switch. When `false`, the manager skips every operation and
    /// returns an empty diagnostics list.
    pub enabled: bool,
    /// Maximum time in milliseconds to wait for the LSP server to publish
    /// diagnostics after a `didOpen`/`didChange`. Default 5000 ms.
    pub poll_after_edit_ms: u64,
    /// Maximum diagnostics to keep per file. Excess items are dropped after
    /// sorting by severity. Default 20.
    pub max_diagnostics_per_file: usize,
    /// When `true`, warnings (severity 2) are kept in the output. When
    /// `false` (default), only errors (severity 1) are surfaced.
    pub include_warnings: bool,
    /// Optional override for the `Language -> (cmd, args)` table. Keys use
    /// [`Language::as_key`] (e.g. `"rust"`).
    pub servers: HashMap<String, Vec<String>>,
    /// User-defined LSP servers for file extensions not in the built-in
    /// registry. Keyed by extension (e.g. `"php"`, `"rb"`).
    #[serde(default)]
    pub custom: HashMap<String, CustomLspDef>,
}

impl From<codewhale_config::LspConfigToml> for LspConfig {
    fn from(toml: codewhale_config::LspConfigToml) -> Self {
        let defaults = Self::default();
        Self {
            enabled: toml.enabled.unwrap_or(defaults.enabled),
            poll_after_edit_ms: toml
                .poll_after_edit_ms
                .unwrap_or(defaults.poll_after_edit_ms),
            max_diagnostics_per_file: toml
                .max_diagnostics_per_file
                .unwrap_or(defaults.max_diagnostics_per_file),
            include_warnings: toml.include_warnings.unwrap_or(defaults.include_warnings),
            servers: toml.servers.unwrap_or_default().into_iter().collect(),
            custom: toml.custom.unwrap_or_default().into_iter().collect(),
        }
    }
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_after_edit_ms: 5_000,
            max_diagnostics_per_file: 20,
            include_warnings: false,
            servers: HashMap::new(),
            custom: HashMap::new(),
        }
    }
}

impl LspConfig {
    /// Resolve `(command, args)` for `lang`. User-supplied overrides take
    /// precedence over the built-in registry.
    pub fn resolve_command(&self, lang: Language) -> Option<(String, Vec<String>)> {
        if let Some(parts) = self.servers.get(lang.as_key())
            && let Some((first, rest)) = parts.split_first()
        {
            return Some((first.clone(), rest.to_vec()));
        }
        let (cmd, args) = registry::server_for(lang)?;
        Some((
            cmd.to_string(),
            args.iter().map(|a| (*a).to_string()).collect(),
        ))
    }
}

type TransportSlot = Arc<AsyncMutex<Option<Arc<dyn LspTransport>>>>;

/// The LspManager holds a lazily populated map of `Language -> Transport`.
/// One transport is reused across files of the same language for the
/// session's lifetime.
pub struct LspManager {
    config: LspConfig,
    workspace: PathBuf,
    /// One startup slot per language. Cold callers share a handshake without
    /// holding the map lock or blocking unrelated language servers.
    transports: AsyncMutex<HashMap<Language, TransportSlot>>,
    /// Per-language "we already warned the user that the binary is missing"
    /// guard so we do not spam the audit log on every edit.
    missing_warned: AsyncMutex<HashSet<Language>>,
    /// Test seam: when set, `diagnostics_for` uses these instead of spawning
    /// real LSP processes. Keyed by language.
    test_transports: AsyncMutex<HashMap<Language, Arc<dyn LspTransport>>>,
    /// Per-extension transports for user-defined custom language servers.
    custom_transports: AsyncMutex<HashMap<String, TransportSlot>>,
    /// Per-extension "we already warned" guard for custom servers.
    custom_missing_warned: AsyncMutex<HashSet<String>>,
}

/// Per-file outcome for the model-facing `read_lints` operation. Unlike the
/// best-effort post-edit hook, this preserves the distinction between an
/// honest empty result, a server/read error, and a timeout.
#[derive(Debug)]
pub struct LintReadResult {
    pub file: PathBuf,
    pub status: LintReadStatus,
    pub items: Vec<Diagnostic>,
    pub freshness: DiagnosticFreshness,
    /// Number of diagnostics after severity selection but before the
    /// configured per-file cap was applied. Unknown when the request did not
    /// complete successfully.
    pub total_diagnostic_count: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug)]
pub enum LintReadStatus {
    Success,
    Error(String),
    Timeout { wait_ms: u64 },
}

#[derive(Debug, Default, serde::Serialize)]
pub struct DiagnosticFreshness {
    pub source_revision: Option<String>,
    pub document_version: Option<i64>,
    pub diagnostic_version: Option<i64>,
    /// Only an exact publication version match proves the text was checked.
    pub freshness: Option<&'static str>,
}

struct DiagnosticPollSuccess {
    block: DiagnosticBlock,
    freshness: DiagnosticFreshness,
    total_diagnostic_count: usize,
    truncated: bool,
}

enum DiagnosticPollOutcome {
    Success(DiagnosticPollSuccess),
    Error(String),
    Timeout,
}

impl LspManager {
    /// Build a new manager. Does not spawn any LSP servers — that is lazy.
    #[must_use]
    pub fn new(config: LspConfig, workspace: PathBuf) -> Self {
        Self {
            config,
            workspace,
            transports: AsyncMutex::new(HashMap::new()),
            missing_warned: AsyncMutex::new(HashSet::new()),
            test_transports: AsyncMutex::new(HashMap::new()),
            custom_transports: AsyncMutex::new(HashMap::new()),
            custom_missing_warned: AsyncMutex::new(HashSet::new()),
        }
    }

    /// Read-only access to the resolved config. Used by the engine to skip
    /// the post-edit hook entirely when `enabled = false`.
    #[must_use]
    pub fn config(&self) -> &LspConfig {
        &self.config
    }

    async fn read_workspace_text(&self, file: &Path) -> std::io::Result<String> {
        let root = self.workspace.clone();
        let requested = file.to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            // The caller may already have resolved the configured workspace
            // alias (macOS /var -> /private/var, or a workspace-root symlink).
            // Resolve only that authorized root, never the requested file:
            // internal links must still be rejected by the confined opener.
            let canonical_root = root.canonicalize()?;
            let relative = requested
                .strip_prefix(&root)
                .or_else(|_| requested.strip_prefix(&canonical_root))
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "LSP file is outside the workspace",
                    )
                })?;
            // Match the existing workspace file serving ceiling. Decode only
            // bounded UTF-8 bytes from the same no-follow workspace opener.
            const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;
            let file =
                codewhale_util::files::WorkspaceFile::open(&canonical_root, relative, false)?;
            let mut bytes = Vec::new();
            file.open_file()?
                .take(MAX_DOCUMENT_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "LSP file exceeds the document limit",
                ));
            }
            String::from_utf8(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .await
        .map_err(std::io::Error::other)?
    }

    /// Inject a fake transport for a language. Used by tests so we never
    /// fork a real LSP server in CI.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn install_test_transport(&self, lang: Language, transport: Arc<dyn LspTransport>) {
        self.test_transports.lock().await.insert(lang, transport);
    }

    /// Poll the LSP server for diagnostics on `file`. Returns the rendered
    /// [`DiagnosticBlock`] (already truncated to the configured per-file
    /// max) or `None` when the manager is disabled / has no server / the
    /// poll times out.
    ///
    /// The `_edit_seq` argument is currently a no-op; it exists in the
    /// signature so the engine can correlate diagnostics back to a specific
    /// edit when we add request batching in v0.7.x.
    pub async fn diagnostics_for(&self, file: &Path, _edit_seq: u64) -> Option<DiagnosticBlock> {
        if !self.config.enabled {
            return None;
        }

        let lang = registry::detect_language(file);
        if lang == Language::Other {
            // Custom extension fallback: check user-defined LSP servers
            // for file extensions not covered by the built-in registry.
            if let Some(custom) = self.config.custom_for_extension(file) {
                return self.diagnostics_for_custom(file, custom).await;
            }
            return None;
        }

        let text = match self.read_workspace_text(file).await {
            Ok(text) => text,
            Err(err) => {
                tracing::debug!(?err, file = %file.display(), "lsp: read file failed");
                return None;
            }
        };

        let transport = match self.transport_for(lang).await {
            Some(t) => t,
            None => return None,
        };

        self.poll_diagnostics(file, &text, transport).await
    }

    /// Shared diagnostics polling for the best-effort post-edit hook. This
    /// keeps the legacy configured severity filter and `None` failure shape.
    async fn poll_diagnostics(
        &self,
        file: &Path,
        text: &str,
        transport: Arc<dyn LspTransport>,
    ) -> Option<DiagnosticBlock> {
        match self.poll_diagnostics_outcome(file, text, transport).await {
            DiagnosticPollOutcome::Success(result) if !result.block.items.is_empty() => {
                Some(result.block)
            }
            DiagnosticPollOutcome::Success(_)
            | DiagnosticPollOutcome::Error(_)
            | DiagnosticPollOutcome::Timeout => None,
        }
    }

    /// Send didOpen/didChange, wait, filter, sort, and truncate while retaining
    /// enough state for callers that must distinguish empty, error, and timeout.
    async fn poll_diagnostics_outcome(
        &self,
        file: &Path,
        text: &str,
        transport: Arc<dyn LspTransport>,
    ) -> DiagnosticPollOutcome {
        let wait = Duration::from_millis(self.config.poll_after_edit_ms);
        // The outer bound owns timeout reporting, including time waiting for
        // another operation on the same transport. An explicit empty
        // publishDiagnostics payload is a successful publication.
        let inner_wait = wait.saturating_add(Duration::from_millis(100));
        let raw = match timeout(wait, transport.diagnostics_for(file, text, inner_wait)).await {
            Ok(Ok(items)) => items,
            Ok(Err(err)) => {
                tracing::debug!(?err, file = %file.display(), "lsp: diagnostics call failed");
                return if err.to_string().contains("timed out") {
                    DiagnosticPollOutcome::Timeout
                } else {
                    DiagnosticPollOutcome::Error(err.to_string())
                };
            }
            Err(_) => {
                tracing::debug!(file = %file.display(), "lsp: diagnostics timed out");
                return DiagnosticPollOutcome::Timeout;
            }
        };

        let freshness = DiagnosticFreshness {
            source_revision: Some(codewhale_runtime::hashing::sha256_hex(text.as_bytes())),
            document_version: raw.document_version,
            diagnostic_version: raw.diagnostic_version,
            freshness: Some(raw.freshness()),
        };
        // Filter, sort, and truncate.
        let include_warnings = self.config.include_warnings;
        let mut items: Vec<Diagnostic> = raw
            .items
            .into_iter()
            .filter(|d| match d.severity {
                Severity::Error => true,
                Severity::Warning => include_warnings,
                _ => false,
            })
            .collect();
        items.sort_by_key(|d| match d.severity {
            Severity::Error => 0u8,
            Severity::Warning => 1u8,
            Severity::Information => 2u8,
            Severity::Hint => 3u8,
        });
        let total_diagnostic_count = items.len();
        let truncated = total_diagnostic_count > self.config.max_diagnostics_per_file;
        let mut block = DiagnosticBlock {
            file: relative_to_workspace(&self.workspace, file),
            items,
        };
        block.truncate(self.config.max_diagnostics_per_file);
        DiagnosticPollOutcome::Success(DiagnosticPollSuccess {
            block,
            freshness,
            total_diagnostic_count,
            truncated,
        })
    }

    /// Diagnostics path for a user-defined custom language server.
    async fn diagnostics_for_custom(
        &self,
        file: &Path,
        custom: &CustomLspDef,
    ) -> Option<DiagnosticBlock> {
        let ext = file.extension()?.to_str()?.to_ascii_lowercase();
        let text = match self.read_workspace_text(file).await {
            Ok(t) => t,
            Err(err) => {
                tracing::debug!(?err, file = %file.display(), "lsp: read file failed");
                return None;
            }
        };
        let transport = match self.transport_for_custom(&ext, custom).await {
            Some(t) => t,
            None => return None,
        };
        self.poll_diagnostics(file, &text, transport).await
    }

    /// Lazy-spawn a custom LSP server for an extension.
    async fn transport_for_custom(
        &self,
        ext: &str,
        def: &CustomLspDef,
    ) -> Option<Arc<dyn LspTransport>> {
        let slot = self
            .custom_transports
            .lock()
            .await
            .entry(ext.to_owned())
            .or_default()
            .clone();
        match self
            .cached_transport(&slot, &def.command, &def.args, &def.language_id)
            .await
        {
            Ok(transport) => Some(transport),
            Err(err) => {
                let key = ext.to_string();
                let mut warned = self.custom_missing_warned.lock().await;
                if warned.insert(key) {
                    tracing::warn!(
                        extension = %ext,
                        command = %def.command,
                        error = %err,
                        "lsp: custom server unavailable; diagnostics disabled for this extension"
                    );
                }
                None
            }
        }
    }

    /// Resolve (and lazily spawn) the transport for `lang`. Tests can
    /// short-circuit this via `install_test_transport` (cfg-test only).
    async fn transport_for(&self, lang: Language) -> Option<Arc<dyn LspTransport>> {
        if let Some(t) = self.test_transports.lock().await.get(&lang) {
            return Some(t.clone());
        }

        let (cmd, args) = self.config.resolve_command(lang)?;
        let slot = self
            .transports
            .lock()
            .await
            .entry(lang)
            .or_default()
            .clone();
        match self
            .cached_transport(&slot, &cmd, &args, lang.language_id())
            .await
        {
            Ok(transport) => Some(transport),
            Err(err) => {
                self.warn_missing_once(lang, &cmd, &err).await;
                None
            }
        }
    }

    async fn cached_transport(
        &self,
        slot: &TransportSlot,
        command: &str,
        args: &[String],
        language_id: &str,
    ) -> anyhow::Result<Arc<dyn LspTransport>> {
        let mut cached = slot.lock().await;
        if let Some(transport) = cached.as_ref().filter(|transport| transport.is_alive()) {
            return Ok(transport.clone());
        }
        // Only a later caller retries a dead process; never replay a failed
        // operation or spawn a background restart loop.
        if let Some(dead) = cached.take() {
            dead.shutdown().await;
        }
        let transport: Arc<dyn LspTransport> = Arc::new(
            StdioLspTransport::spawn(command, args, language_id, self.workspace.clone()).await?,
        );
        *cached = Some(transport.clone());
        Ok(transport)
    }

    async fn warn_missing_once(&self, lang: Language, cmd: &str, err: &anyhow::Error) {
        let mut warned = self.missing_warned.lock().await;
        if warned.insert(lang) {
            tracing::warn!(
                language = %lang.as_key(),
                command = %cmd,
                error = %err,
                "lsp: server unavailable; diagnostics disabled for this language"
            );
        }
    }

    /// Resolve a transport for `file` without creating a second server
    /// lifecycle. Reuses the same lazy map as post-edit diagnostics.
    async fn transport_for_path(&self, file: &Path) -> Option<Arc<dyn LspTransport>> {
        if !self.config.enabled {
            return None;
        }
        let lang = registry::detect_language(file);
        if lang != Language::Other {
            return self.transport_for(lang).await;
        }
        if let Some(custom) = self.config.custom_for_extension(file) {
            let ext = file.extension()?.to_str()?.to_ascii_lowercase();
            return self.transport_for_custom(&ext, custom).await;
        }
        None
    }

    /// Model-facing intelligence query. Shares the existing transport pool.
    /// `operation` is one of: `diagnostics`, `symbols`, `definition`, `references`.
    pub async fn intelligence(
        &self,
        operation: &str,
        file: &Path,
        line: Option<u32>,
        character: Option<u32>,
        query: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.intelligence_at_revision(operation, file, line, character, query, None)
            .await
    }

    /// Optional source revision for native navigation; raw tool results remain
    /// available. A target revision proves bytes read here, not server analysis.
    pub async fn intelligence_at_revision(
        &self,
        operation: &str,
        file: &Path,
        line: Option<u32>,
        character: Option<u32>,
        query: Option<&str>,
        expected_revision: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if !self.config.enabled {
            return Err("LSP is disabled ([lsp] enabled = false)".to_string());
        }
        let wait = Duration::from_millis(self.config.poll_after_edit_ms);
        match operation {
            "diagnostics" => {
                let result = self
                    .diagnostics_for_paths(&[file.to_path_buf()])
                    .await?
                    .into_iter()
                    .next()
                    .ok_or("LSP diagnostics returned no outcome")?;
                match result.status {
                    LintReadStatus::Error(error) => Err(error),
                    LintReadStatus::Timeout { wait_ms } => {
                        Err(format!("LSP diagnostics timed out after {wait_ms} ms"))
                    }
                    LintReadStatus::Success => Ok(serde_json::json!({
                        "file": result.file.display().to_string(),
                        "items": result.items.iter().map(|d| serde_json::json!({
                            "line": d.line, "column": d.column,
                            "severity": format!("{:?}", d.severity).to_ascii_lowercase(),
                            "message": d.message,
                        })).collect::<Vec<_>>(),
                        "source_revision": result.freshness.source_revision,
                        "document_version": result.freshness.document_version,
                        "diagnostic_version": result.freshness.diagnostic_version,
                        "freshness": result.freshness.freshness,
                        "total_diagnostic_count": result.total_diagnostic_count,
                        "truncated": result.truncated,
                    })),
                }
            }
            "symbols" | "definition" | "references" => {
                let text = self
                    .read_workspace_text(file)
                    .await
                    .map_err(|err| format!("read {}: {err}", file.display()))?;
                let source_revision = codewhale_runtime::hashing::sha256_hex(text.as_bytes());
                if expected_revision.is_some_and(|expected| expected != source_revision) {
                    return Err("stale_document".into());
                }
                let root = tokio::fs::canonicalize(&self.workspace)
                    .await
                    .map_err(|_| "workspace unavailable")?;
                let source_path = file
                    .strip_prefix(&self.workspace)
                    .or_else(|_| file.strip_prefix(&root))
                    .ok()
                    .and_then(semantic_relative_path)
                    .ok_or("source path is not workspace-relative UTF-8")?;
                let uri = client::uri_from_path(file);
                let (method, params) = if operation == "symbols" {
                    if let Some(q) = query.filter(|s| !s.trim().is_empty()) {
                        if q.len() > 1024 {
                            return Err("symbol query is too long".into());
                        }
                        ("workspace/symbol", serde_json::json!({"query":q}))
                    } else {
                        (
                            "textDocument/documentSymbol",
                            serde_json::json!({"textDocument":{"uri":uri}}),
                        )
                    }
                } else {
                    let position = SemanticPosition {
                        line: line
                            .and_then(|value| value.checked_sub(1))
                            .ok_or("line must be 1-based")?,
                        character: character
                            .unwrap_or(1)
                            .checked_sub(1)
                            .ok_or("character must be 1-based")?,
                    };
                    if !position.valid_in(&text) {
                        return Err(
                            "position is outside the document or splits a UTF-16 character".into(),
                        );
                    }
                    let method = if operation == "definition" {
                        "textDocument/definition"
                    } else {
                        "textDocument/references"
                    };
                    let mut params =
                        serde_json::json!({"textDocument":{"uri":uri}, "position":position});
                    if operation == "references" {
                        params["context"] = serde_json::json!({"includeDeclaration":true});
                    }
                    (method, params)
                };
                let transport = self
                    .transport_for_path(file)
                    .await
                    .ok_or_else(|| format!("no LSP server for {}", file.display()))?;
                let reply = transport
                    .request_for_document(file, &text, method, params, wait)
                    .await
                    .map_err(|err| err.to_string())?;
                let (locations, truncated, omitted) =
                    self.semantic_locations(&reply.result, file, &text).await;
                // Re-read after the request and normalization; a changed or
                // replaced source cannot publish a successful navigation result.
                let current = self
                    .read_workspace_text(file)
                    .await
                    .map_err(|_| "stale_document")?;
                if codewhale_runtime::hashing::sha256_hex(current.as_bytes()) != source_revision {
                    return Err("stale_document".into());
                }
                Ok(serde_json::json!({
                    "operation": operation,
                    "file": source_path,
                    "semantic_contract_version": 1,
                    "position_encoding": "utf-16",
                    "source_revision": source_revision,
                    "document_version": reply.document_version,
                    "freshness": if reply.document_version.is_some() { "verified" } else { "unverified" },
                    "locations": locations, "truncated": truncated, "omitted": omitted,
                    "result": truncate_intelligence_result(reply.result),
                }))
            }
            other => Err(format!(
                "unknown LSP operation '{other}'; use diagnostics, symbols, definition, or references"
            )),
        }
    }

    async fn semantic_locations(
        &self,
        raw: &serde_json::Value,
        source: &Path,
        source_text: &str,
    ) -> (Vec<SemanticLocation>, bool, usize) {
        const MAX_LOCATIONS: usize = 40;
        const MAX_NODES: usize = 256;
        const MAX_TARGET_BYTES: usize = 16 * 1024 * 1024;
        let root = match tokio::fs::canonicalize(&self.workspace).await {
            Ok(root) => root,
            Err(_) => return (vec![], false, 1),
        };
        let mut pending = vec![(raw, 0usize)];
        let mut locations = Vec::new();
        let mut visited = 0;
        let mut target_bytes: usize = 0;
        let mut truncated = false;
        let mut omitted = 0;
        while let Some((value, depth)) = pending.pop() {
            visited += 1;
            if visited > MAX_NODES || locations.len() >= MAX_LOCATIONS {
                truncated = true;
                break;
            }
            if value.is_null() {
                continue;
            }
            if let Some(items) = value.as_array() {
                let remaining = MAX_NODES.saturating_sub(visited + pending.len());
                truncated |= items.len() > remaining;
                pending.extend(items.iter().take(remaining).rev().map(|item| (item, depth)));
                continue;
            }
            if let Some(children) = value.get("children").and_then(serde_json::Value::as_array) {
                if depth >= 16 {
                    truncated |= !children.is_empty();
                } else {
                    let remaining = MAX_NODES.saturating_sub(visited + pending.len());
                    truncated |= children.len() > remaining;
                    pending.extend(
                        children
                            .iter()
                            .take(remaining)
                            .rev()
                            .map(|item| (item, depth + 1)),
                    );
                }
            }
            let location = value.get("location").unwrap_or(value);
            let uri = location.get("uri").or_else(|| value.get("targetUri"));
            let target = match uri {
                Some(uri) => uri.as_str().and_then(client::path_from_uri),
                None if value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                    && value.get("selectionRange").is_some() =>
                {
                    Some(source.to_path_buf())
                }
                _ => None,
            };
            let range = value
                .get("targetSelectionRange")
                .or_else(|| value.get("selectionRange"))
                .or_else(|| location.get("range"))
                .and_then(|range| serde_json::from_value::<SemanticRange>(range.clone()).ok());
            let (Some(target), Some(range)) = (target, range) else {
                omitted += 1;
                continue;
            };
            let relative = match target.strip_prefix(&self.workspace).or_else(|_| target.strip_prefix(&root)) {
                Ok(relative) if !relative.as_os_str().is_empty()
                    && relative.components().all(|component| matches!(component, std::path::Component::Normal(part) if !codewhale_util::git_metadata::is_git_metadata_name(part))) => relative,
                _ => { omitted += 1; continue; }
            };
            let Some(path) = semantic_relative_path(relative) else {
                omitted += 1;
                continue;
            };
            let text = if target == source {
                source_text.to_owned()
            } else {
                match self.read_workspace_text(&target).await {
                    Ok(text) => text,
                    Err(_) => {
                        omitted += 1;
                        continue;
                    }
                }
            };
            target_bytes = target_bytes.saturating_add(text.len());
            if target_bytes > MAX_TARGET_BYTES {
                truncated = true;
                break;
            }
            if range.start > range.end || !range.start.valid_in(&text) || !range.end.valid_in(&text)
            {
                omitted += 1;
                continue;
            }
            locations.push(SemanticLocation {
                path,
                range,
                target_revision: codewhale_runtime::hashing::sha256_hex(text.as_bytes()),
                target_freshness: "unverified",
                name: value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(|name| name.chars().filter(|c| !c.is_control()).take(256).collect()),
                kind: value
                    .get("kind")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|kind| u32::try_from(kind).ok())
                    .filter(|kind| (1..=26).contains(kind)),
            });
        }
        (locations, truncated, omitted)
    }

    /// Read diagnostics for several existing files through the shared LSP
    /// transport pool. Every attempted file gets an explicit outcome, and the
    /// same configured severity policy as post-edit diagnostics is applied:
    /// errors are always retained and warnings depend on `include_warnings`.
    pub async fn diagnostics_for_paths(
        &self,
        files: &[PathBuf],
    ) -> Result<Vec<LintReadResult>, String> {
        if !self.config.enabled {
            return Err("LSP is disabled ([lsp] enabled = false)".to_string());
        }

        let mut results = Vec::with_capacity(files.len());
        for file in files {
            let relative_file = relative_to_workspace(&self.workspace, file);
            let text = match self.read_workspace_text(file).await {
                Ok(text) => text,
                Err(err) => {
                    results.push(LintReadResult {
                        file: relative_file,
                        status: LintReadStatus::Error(format!("failed to read file: {err}")),
                        items: Vec::new(),
                        freshness: DiagnosticFreshness::default(),
                        total_diagnostic_count: None,
                        truncated: false,
                    });
                    continue;
                }
            };
            let Some(transport) = self.transport_for_path(file).await else {
                results.push(LintReadResult {
                    file: relative_file,
                    status: LintReadStatus::Error(
                        "no LSP server is available for this file".to_string(),
                    ),
                    items: Vec::new(),
                    freshness: DiagnosticFreshness::default(),
                    total_diagnostic_count: None,
                    truncated: false,
                });
                continue;
            };
            let outcome = self.poll_diagnostics_outcome(file, &text, transport).await;
            results.push(match outcome {
                DiagnosticPollOutcome::Success(outcome) => LintReadResult {
                    file: outcome.block.file,
                    status: LintReadStatus::Success,
                    items: outcome.block.items,
                    freshness: outcome.freshness,
                    total_diagnostic_count: Some(outcome.total_diagnostic_count),
                    truncated: outcome.truncated,
                },
                DiagnosticPollOutcome::Error(error) => LintReadResult {
                    file: relative_file,
                    status: LintReadStatus::Error(format!(
                        "LSP diagnostics request failed: {error}"
                    )),
                    items: Vec::new(),
                    freshness: DiagnosticFreshness::default(),
                    total_diagnostic_count: None,
                    truncated: false,
                },
                DiagnosticPollOutcome::Timeout => LintReadResult {
                    file: relative_file,
                    status: LintReadStatus::Timeout {
                        wait_ms: self.config.poll_after_edit_ms,
                    },
                    items: Vec::new(),
                    freshness: DiagnosticFreshness::default(),
                    total_diagnostic_count: None,
                    truncated: false,
                },
            });
        }
        Ok(results)
    }

    /// Best-effort shutdown of every spawned transport. Called when the
    /// session ends.
    pub async fn shutdown_all(&self) {
        let transports: Vec<TransportSlot> =
            self.transports.lock().await.values().cloned().collect();
        let custom: Vec<TransportSlot> = self
            .custom_transports
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for slot in transports.into_iter().chain(custom) {
            if let Some(transport) = slot.lock().await.take() {
                transport.shutdown().await;
            }
        }
    }
}

impl LspConfig {
    /// Look up a [`CustomLspDef`] for `file` when the built-in registry
    /// would return `Language::Other`. Returns `None` when the extension is
    /// unknown or no custom server is registered for it.
    fn custom_for_extension(&self, file: &Path) -> Option<&CustomLspDef> {
        let ext = file.extension()?.to_str()?;
        self.custom.get(&ext.to_ascii_lowercase())
    }
}

fn semantic_relative_path(path: &Path) -> Option<String> {
    let parts = path
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part)
                if !codewhale_util::git_metadata::is_git_metadata_name(part) =>
            {
                part.to_str().filter(|part| !part.contains('\\'))
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let path = parts.join("/");
    (!path.is_empty() && path.len() <= 4096).then_some(path)
}

/// LSP positions are zero-based UTF-16 code units, never UTF-8 byte offsets.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
struct SemanticPosition {
    line: u32,
    character: u32,
}
impl SemanticPosition {
    fn valid_in(self, text: &str) -> bool {
        let Some(line) = text.split('\n').nth(self.line as usize) else {
            return false;
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut column = 0;
        for ch in line.chars() {
            if column == self.character {
                return true;
            }
            column += ch.len_utf16() as u32;
            if column > self.character {
                return false;
            }
        }
        column == self.character
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct SemanticRange {
    start: SemanticPosition,
    end: SemanticPosition,
}

#[derive(Debug, serde::Serialize)]
struct SemanticLocation {
    path: String,
    range: SemanticRange,
    target_revision: String,
    target_freshness: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<u32>,
}

/// Cap intelligence payloads so a chatty language server cannot flood the
/// model context. Arrays keep the first `MAX` entries and set `truncated`.
fn truncate_intelligence_result(value: serde_json::Value) -> serde_json::Value {
    const MAX_ITEMS: usize = 40;
    const MAX_CHARS: usize = 12_000;
    let bounded = match value {
        serde_json::Value::Array(mut items) if items.len() > MAX_ITEMS => {
            let total = items.len();
            items.truncate(MAX_ITEMS);
            serde_json::json!({"items":items, "truncated":true, "total":total})
        }
        other => other,
    };
    let rendered = bounded.to_string();
    if rendered.len() > MAX_CHARS {
        let mut boundary = MAX_CHARS;
        while !rendered.is_char_boundary(boundary) {
            boundary -= 1;
        }
        serde_json::json!({
            "truncated": true,
            "preview": &rendered[..boundary],
            "total_chars": rendered.len(),
        })
    } else {
        bounded
    }
}

/// Render `path` relative to the workspace when possible. Falls back to
/// `path.file_name()` (per the issue's hard rule about not using
/// `display().to_string()` on the bare path) when relativization fails.
fn relative_to_workspace(workspace: &Path, path: &Path) -> PathBuf {
    if let Ok(rel) = path.strip_prefix(workspace) {
        return rel.to_path_buf();
    }
    PathBuf::from(
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("unknown")),
    )
}

/// Used for tests / no-op runs. Builds an empty manager that always returns
/// `None`. Needed because the engine constructs an `LspManager` even when
/// the user has disabled LSP, so the field is always present.
impl LspManager {
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(
            LspConfig {
                enabled: false,
                ..LspConfig::default()
            },
            PathBuf::new(),
        )
    }
}

#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(not(test), allow(dead_code, unused_imports))]
pub mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Fake transport: returns a fixed list of diagnostics. Used by
    /// integration tests so we never spawn a real LSP server in CI.
    pub struct FakeTransport {
        items: Vec<Diagnostic>,
        calls: AtomicUsize,
    }

    impl FakeTransport {
        pub fn new(items: Vec<Diagnostic>) -> Self {
            Self {
                items,
                calls: AtomicUsize::new(0),
            }
        }

        pub fn call_count(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl LspTransport for FakeTransport {
        async fn diagnostics_for(
            &self,
            _path: &Path,
            _text: &str,
            _wait: Duration,
        ) -> anyhow::Result<crate::client::DiagnosticPublication> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.items.clone().into())
        }

        async fn shutdown(&self) {}
    }

    #[cfg(unix)]
    async fn cache_fixture(custom: bool) {
        let root = tempfile::tempdir().unwrap();
        let pids = root.path().join("pids");
        let args = vec![
            "-u".into(),
            "-c".into(),
            client::tests::STDIO_FIXTURE.into(),
            "cache".into(),
            pids.to_string_lossy().into_owned(),
        ];
        let mut config = LspConfig::default();
        if custom {
            config.custom.insert(
                "cachetest".into(),
                CustomLspDef {
                    command: "python3".into(),
                    args,
                    language_id: "cachetest".into(),
                },
            );
        } else {
            let mut command = vec!["python3".into()];
            command.extend(args);
            config.servers.insert("python".into(), command);
        }
        let manager = LspManager::new(config, root.path().to_owned());
        let path = root
            .path()
            .join(if custom { "file.cachetest" } else { "file.py" });
        let (first, second, third) = tokio::join!(
            manager.transport_for_path(&path),
            manager.transport_for_path(&path),
            manager.transport_for_path(&path)
        );
        let first = first.unwrap();
        assert!(Arc::ptr_eq(&first, &second.unwrap()));
        assert!(Arc::ptr_eq(&first, &third.unwrap()));
        assert_eq!(std::fs::read_to_string(&pids).unwrap().lines().count(), 1);
        assert!(
            first
                .request(
                    "fixture/exit",
                    serde_json::json!({}),
                    Duration::from_secs(2)
                )
                .await
                .is_err()
        );
        timeout(Duration::from_secs(2), async {
            while first.is_alive() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let recovered = manager.transport_for_path(&path).await.unwrap();
        assert!(!Arc::ptr_eq(&first, &recovered));
        assert!(
            recovered
                .request(
                    "fixture/ready",
                    serde_json::json!({}),
                    Duration::from_secs(2)
                )
                .await
                .is_ok()
        );
        assert_eq!(std::fs::read_to_string(&pids).unwrap().lines().count(), 2);
        manager.shutdown_all().await;
        assert!(manager.transport_for_path(&path).await.is_some());
        assert_eq!(std::fs::read_to_string(&pids).unwrap().lines().count(), 3);
        manager.shutdown_all().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn built_in_transport_cache_shares_cold_start_and_recovers_after_exit() {
        cache_fixture(false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn custom_transport_cache_shares_cold_start_and_recovers_after_exit() {
        cache_fixture(true).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_transport_initialization_does_not_poison_the_cache() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("server.py");
        let mut config = LspConfig::default();
        config.servers.insert(
            "python".into(),
            vec![
                "python3".into(),
                "-u".into(),
                script.to_string_lossy().into_owned(),
                "cache".into(),
                root.path().join("pids").to_string_lossy().into_owned(),
            ],
        );
        let manager = LspManager::new(config, root.path().to_owned());
        let path = root.path().join("file.py");
        assert!(manager.transport_for_path(&path).await.is_none());
        std::fs::write(&script, client::tests::STDIO_FIXTURE).unwrap();
        assert!(manager.transport_for_path(&path).await.is_some());
        manager.shutdown_all().await;
    }

    struct SemanticFixture {
        result: serde_json::Value,
        mutate: Option<PathBuf>,
    }
    #[async_trait::async_trait]
    impl LspTransport for SemanticFixture {
        async fn diagnostics_for(
            &self,
            _: &Path,
            _: &str,
            _: Duration,
        ) -> anyhow::Result<client::DiagnosticPublication> {
            Ok(vec![].into())
        }
        async fn request(
            &self,
            _: &str,
            _: serde_json::Value,
            _: Duration,
        ) -> anyhow::Result<serde_json::Value> {
            if let Some(path) = &self.mutate {
                tokio::fs::write(path, "changed").await?;
            }
            Ok(self.result.clone())
        }
        async fn shutdown(&self) {}
    }

    #[tokio::test]
    async fn semantic_source_revision_rejects_stale_before_and_after_request() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("main.rs");
        tokio::fs::write(&file, "fn main() {}\n").await.unwrap();
        let manager = LspManager::new(LspConfig::default(), root.path().to_owned());
        assert_eq!(
            manager
                .intelligence_at_revision(
                    "definition",
                    &file,
                    Some(1),
                    Some(1),
                    None,
                    Some("wrong")
                )
                .await
                .unwrap_err(),
            "stale_document"
        );
        manager
            .install_test_transport(
                Language::Rust,
                Arc::new(SemanticFixture {
                    result: serde_json::json!([]),
                    mutate: Some(file.clone()),
                }),
            )
            .await;
        let revision = codewhale_runtime::hashing::sha256_hex(b"fn main() {}\n");
        assert_eq!(
            manager
                .intelligence_at_revision(
                    "definition",
                    &file,
                    Some(1),
                    Some(1),
                    None,
                    Some(&revision)
                )
                .await
                .unwrap_err(),
            "stale_document"
        );
    }

    #[tokio::test]
    async fn semantic_locations_preserve_raw_and_distinguish_target_readback_from_analysis() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("main.rs");
        let target = root.path().join("a b.rs");
        tokio::fs::write(&file, "fn main() {}\n").await.unwrap();
        tokio::fs::write(&target, "🐋foo\n").await.unwrap();
        let raw = serde_json::json!([{"targetUri":client::uri_from_path(&target),"targetSelectionRange":{"start":{"line":0,"character":2},"end":{"line":0,"character":5}}}]);
        let manager = LspManager::new(LspConfig::default(), root.path().to_owned());
        manager
            .install_test_transport(
                Language::Rust,
                Arc::new(SemanticFixture {
                    result: raw.clone(),
                    mutate: None,
                }),
            )
            .await;
        let result = manager
            .intelligence("definition", &file, Some(1), Some(1), None)
            .await
            .unwrap();
        assert_eq!(result["result"], raw);
        assert_eq!(result["semantic_contract_version"], 1);
        assert_eq!(result["position_encoding"], "utf-16");
        assert_eq!(result["freshness"], "unverified");
        assert_eq!(result["locations"][0]["path"], "a b.rs");
        assert_eq!(
            result["locations"][0]["target_revision"],
            codewhale_runtime::hashing::sha256_hex("🐋foo\n".as_bytes())
        );
        assert_eq!(result["locations"][0]["target_freshness"], "unverified");
        assert_eq!(result["omitted"], 0);
    }

    #[tokio::test]
    async fn semantic_locations_reject_unsafe_uri_range_and_symlink_and_bound_symbols() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("main.rs");
        tokio::fs::write(&file, "🐋foo\n").await.unwrap();
        let manager = LspManager::new(LspConfig::default(), root.path().to_owned());
        let good = serde_json::json!({"name":"foo","kind":12,"selectionRange":{"start":{"line":0,"character":2},"end":{"line":0,"character":5}}});
        let mut split = good.clone();
        split["selectionRange"]["start"]["character"] = serde_json::json!(1);
        let mut huge = good.clone();
        huge["selectionRange"]["end"]["character"] = serde_json::json!(u64::MAX);
        let mut unsafe_uri = good.clone();
        unsafe_uri["uri"] = serde_json::json!("file://remote/etc/passwd");
        let mut outside = good.clone();
        outside["uri"] = serde_json::json!("file:///etc/passwd");
        let mut reverse = good.clone();
        reverse["selectionRange"]["end"]["character"] = serde_json::json!(0);
        let cases = vec![good.clone(), split, huge, unsafe_uri, outside, reverse];
        #[cfg(unix)]
        let cases = {
            let mut cases = cases;
            let link = root.path().join("link.rs");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            let mut linked = good.clone();
            linked["uri"] = serde_json::json!(client::uri_from_path(&link));
            cases.push(linked);
            cases
        };
        let expected_omitted = cases.len() - 1;
        let (locations, truncated, omitted) = manager
            .semantic_locations(&serde_json::json!(cases), &file, "🐋foo\n")
            .await;
        assert_eq!(locations.len(), 1);
        assert!(!truncated);
        assert_eq!(omitted, expected_omitted);
        let (locations, truncated, _) = manager
            .semantic_locations(&serde_json::json!(vec![good; 100]), &file, "🐋foo\n")
            .await;
        assert_eq!(locations.len(), 40);
        assert!(truncated);
        assert!(
            !SemanticPosition {
                line: 0,
                character: 1
            }
            .valid_in("🐋")
        );
        assert!(
            SemanticPosition {
                line: 0,
                character: 2
            }
            .valid_in("🐋")
        );
    }

    #[test]
    fn semantic_raw_result_bound_is_unicode_safe_for_arrays_too() {
        let result = truncate_intelligence_result(serde_json::json!(["🐋".repeat(20_000)]));
        assert_eq!(result["truncated"], true);
        assert!(result["preview"].as_str().unwrap().len() <= 12_000);
    }

    #[tokio::test]
    async fn returns_none_when_disabled() {
        let mgr = LspManager::new(
            LspConfig {
                enabled: false,
                ..LspConfig::default()
            },
            PathBuf::from("/tmp"),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.rs");
        tokio::fs::write(&path, b"fn main() {}").await.unwrap();
        assert!(mgr.diagnostics_for(&path, 1).await.is_none());
    }

    #[tokio::test]
    async fn returns_none_for_unknown_language() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = LspManager::new(LspConfig::default(), dir.path().to_path_buf());
        let path = dir.path().join("notes.txt");
        tokio::fs::write(&path, b"hi").await.unwrap();
        assert!(mgr.diagnostics_for(&path, 1).await.is_none());
    }

    #[tokio::test]
    async fn forwards_errors_through_fake_transport() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = LspManager::new(LspConfig::default(), dir.path().to_path_buf());
        let path = dir.path().join("foo.rs");
        tokio::fs::write(&path, b"let x: i32 = \"oops\";")
            .await
            .unwrap();

        let fake = Arc::new(FakeTransport::new(vec![Diagnostic {
            line: 1,
            column: 14,
            severity: Severity::Error,
            message: "expected i32, found &str".to_string(),
        }]));
        mgr.install_test_transport(Language::Rust, fake.clone())
            .await;

        let block = mgr.diagnostics_for(&path, 1).await.expect("has block");
        let rendered = block.render();
        assert!(rendered.contains("ERROR [1:14] expected i32, found &str"));
        assert!(rendered.contains("foo.rs"));
        assert_eq!(fake.call_count(), 1);
    }

    #[tokio::test]
    async fn drops_warnings_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = LspManager::new(LspConfig::default(), dir.path().to_path_buf());
        let path = dir.path().join("foo.rs");
        tokio::fs::write(&path, b"fn main() {}").await.unwrap();

        let fake = Arc::new(FakeTransport::new(vec![
            Diagnostic {
                line: 1,
                column: 1,
                severity: Severity::Warning,
                message: "unused import".to_string(),
            },
            Diagnostic {
                line: 2,
                column: 1,
                severity: Severity::Error,
                message: "type error".to_string(),
            },
        ]));
        mgr.install_test_transport(Language::Rust, fake).await;

        let block = mgr.diagnostics_for(&path, 1).await.expect("has block");
        assert_eq!(block.items.len(), 1);
        assert_eq!(block.items[0].severity, Severity::Error);
    }

    #[tokio::test]
    async fn keeps_warnings_when_opted_in() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = LspManager::new(
            LspConfig {
                include_warnings: true,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        );
        let path = dir.path().join("foo.rs");
        tokio::fs::write(&path, b"fn main() {}").await.unwrap();

        let fake = Arc::new(FakeTransport::new(vec![
            Diagnostic {
                line: 1,
                column: 1,
                severity: Severity::Warning,
                message: "unused".to_string(),
            },
            Diagnostic {
                line: 2,
                column: 1,
                severity: Severity::Error,
                message: "broken".to_string(),
            },
        ]));
        mgr.install_test_transport(Language::Rust, fake).await;

        let block = mgr.diagnostics_for(&path, 1).await.expect("has block");
        assert_eq!(block.items.len(), 2);
        // Errors come first after sorting.
        assert_eq!(block.items[0].severity, Severity::Error);
        assert_eq!(block.items[1].severity, Severity::Warning);
    }

    #[tokio::test]
    async fn truncates_to_max_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = LspManager::new(
            LspConfig {
                max_diagnostics_per_file: 3,
                ..LspConfig::default()
            },
            dir.path().to_path_buf(),
        );
        let path = dir.path().join("foo.rs");
        tokio::fs::write(&path, b"fn main() {}").await.unwrap();

        let fake = Arc::new(FakeTransport::new(
            (0..10)
                .map(|i| Diagnostic {
                    line: i + 1,
                    column: 1,
                    severity: Severity::Error,
                    message: format!("err {i}"),
                })
                .collect(),
        ));
        mgr.install_test_transport(Language::Rust, fake).await;

        let block = mgr.diagnostics_for(&path, 1).await.expect("has block");
        assert_eq!(block.items.len(), 3);
    }

    #[tokio::test]
    async fn render_blocks_concatenates() {
        let blocks = vec![
            DiagnosticBlock {
                file: PathBuf::from("a.rs"),
                items: vec![Diagnostic {
                    line: 1,
                    column: 1,
                    severity: Severity::Error,
                    message: "err in a".to_string(),
                }],
            },
            DiagnosticBlock {
                file: PathBuf::from("b.rs"),
                items: vec![Diagnostic {
                    line: 2,
                    column: 2,
                    severity: Severity::Error,
                    message: "err in b".to_string(),
                }],
            },
        ];
        let rendered = render_blocks(&blocks);
        assert!(rendered.contains("file=\"a.rs\""));
        assert!(rendered.contains("file=\"b.rs\""));
    }

    #[test]
    fn relative_path_falls_back_to_filename_when_outside_workspace() {
        let workspace = PathBuf::from("/foo/bar");
        let path = PathBuf::from("/baz/qux.rs");
        assert_eq!(
            relative_to_workspace(&workspace, &path),
            PathBuf::from("qux.rs")
        );
    }

    #[test]
    fn config_resolve_uses_overrides() {
        let mut cfg = LspConfig::default();
        cfg.servers.insert(
            "rust".to_string(),
            vec!["custom-rls".to_string(), "--lsp".to_string()],
        );
        let (cmd, args) = cfg.resolve_command(Language::Rust).unwrap();
        assert_eq!(cmd, "custom-rls");
        assert_eq!(args, vec!["--lsp".to_string()]);
    }

    #[test]
    fn config_resolve_falls_back_to_registry() {
        let cfg = LspConfig::default();
        let (cmd, _) = cfg.resolve_command(Language::Rust).unwrap();
        assert_eq!(cmd, "rust-analyzer");
    }

    // ── custom server extension tests ─────────────────────────────────────

    #[test]
    fn custom_for_extension_none_for_empty_config() {
        let cfg = LspConfig::default();
        assert!(cfg.custom_for_extension(&PathBuf::from("foo.rb")).is_none());
    }

    #[test]
    fn custom_for_extension_finds_registered_extension() {
        let mut cfg = LspConfig::default();
        cfg.custom.insert(
            "rb".to_string(),
            CustomLspDef {
                language_id: "ruby".to_string(),
                command: "ruby-lsp".to_string(),
                args: vec!["--stdio".to_string()],
            },
        );
        let def = cfg
            .custom_for_extension(&PathBuf::from("lib/hello.rb"))
            .expect("should find rb");
        assert_eq!(def.language_id, "ruby");
        assert_eq!(def.command, "ruby-lsp");
    }

    #[test]
    fn custom_for_extension_case_insensitive() {
        let mut cfg = LspConfig::default();
        cfg.custom.insert(
            "cs".to_string(),
            CustomLspDef {
                language_id: "csharp".to_string(),
                command: "csharp-ls".to_string(),
                args: vec![],
            },
        );
        assert!(cfg.custom_for_extension(&PathBuf::from("App.CS")).is_some());
        assert!(cfg.custom_for_extension(&PathBuf::from("App.Cs")).is_some());
    }

    #[tokio::test]
    async fn custom_fallback_only_for_other_language() {
        // Even if [lsp.custom.go] is configured, .go files must still use
        // the built-in gopls path — custom is a fallback, not an override.
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = LspConfig::default();
        cfg.custom.insert(
            "go".to_string(),
            CustomLspDef {
                language_id: "go".to_string(),
                command: "custom-gopls".to_string(),
                args: vec![],
            },
        );
        let mgr = LspManager::new(cfg, dir.path().to_path_buf());
        let path = dir.path().join("main.go");
        tokio::fs::write(&path, b"package main\n").await.unwrap();

        // Inject a fake transport for the built-in Go path; we do NOT
        // inject one for the custom path — so if it accidentally takes
        // the custom route it will return None.
        let fake = Arc::new(FakeTransport::new(vec![Diagnostic {
            line: 1,
            column: 1,
            severity: Severity::Error,
            message: "builtin-go-diag".to_string(),
        }]));
        mgr.install_test_transport(Language::Go, fake).await;

        // No custom transport injected — if it hits custom, it returns None.
        // If it hits built-in, it returns the fake diagnostic.
        let block = mgr.diagnostics_for(&path, 1).await.expect("has block");
        let rendered = block.render();
        assert!(
            rendered.contains("builtin-go-diag"),
            "should use built-in Go transport, not custom override: {rendered}"
        );
    }

    #[tokio::test]
    async fn diagnostics_for_custom_returns_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = LspConfig::default();
        cfg.custom.insert(
            "rb".to_string(),
            CustomLspDef {
                language_id: "ruby".to_string(),
                command: "ruby-lsp".to_string(),
                args: vec![],
            },
        );
        let mgr = LspManager::new(cfg, dir.path().to_path_buf());
        let path = dir.path().join("app.rb");
        tokio::fs::write(&path, b"def foo; end\n").await.unwrap();

        // Inject fake transport into the custom-transport map.
        let fake = Arc::new(FakeTransport::new(vec![Diagnostic {
            line: 1,
            column: 5,
            severity: Severity::Error,
            message: "ruby type error".to_string(),
        }]));
        mgr.custom_transports.lock().await.insert(
            "rb".to_string(),
            Arc::new(AsyncMutex::new(Some(fake.clone()))),
        );

        let block = mgr.diagnostics_for(&path, 1).await.expect("has block");
        let rendered = block.render();
        assert!(rendered.contains("ruby type error"));
        assert_eq!(fake.call_count(), 1);
    }

    #[tokio::test]
    async fn custom_unregistered_extension_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = LspConfig::default();
        let mgr = LspManager::new(cfg, dir.path().to_path_buf());
        let path = dir.path().join("script.lua");
        tokio::fs::write(&path, b"print('hi')\n").await.unwrap();

        // No custom config for .lua and Lua is not built-in → should be None.
        assert!(mgr.diagnostics_for(&path, 1).await.is_none());
    }

    #[tokio::test]
    async fn diagnostic_freshness_intelligence_distinguishes_no_server_and_unverified_empty() {
        let root = tempfile::tempdir().unwrap();
        let unknown = root.path().join("notes.unsupported_extension");
        std::fs::write(&unknown, "text").unwrap();
        let manager = LspManager::new(LspConfig::default(), root.path().to_owned());
        let error = manager
            .intelligence("diagnostics", &unknown, None, None, None)
            .await
            .unwrap_err();
        assert!(error.contains("no LSP server"));
        let file = root.path().join("main.rs");
        let text = "fn main() { /* 🐋 */ }";
        std::fs::write(&file, text).unwrap();
        manager
            .install_test_transport(Language::Rust, Arc::new(FakeTransport::new(vec![])))
            .await;
        let result = manager
            .intelligence("diagnostics", &file, None, None, None)
            .await
            .unwrap();
        assert_eq!(result["items"], serde_json::json!([]));
        assert_eq!(
            result["source_revision"],
            codewhale_runtime::hashing::sha256_hex(text.as_bytes())
        );
        assert_eq!(result["freshness"], "unverified");
        assert!(result["diagnostic_version"].is_null());
        assert_eq!(result["total_diagnostic_count"], 0);
    }

    #[tokio::test]
    async fn diagnostic_freshness_missing_binary_is_not_clean() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let mut config = LspConfig::default();
        config.servers.insert(
            "rust".into(),
            vec![
                root.path()
                    .join("nonexistent-language-server")
                    .display()
                    .to_string(),
            ],
        );
        let manager = LspManager::new(config, root.path().to_owned());
        assert!(
            manager
                .intelligence("diagnostics", &file, None, None, None)
                .await
                .unwrap_err()
                .contains("no LSP server")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn diagnostic_freshness_confined_read_rejects_replaced_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.rs");
        std::fs::write(&secret, "outside text").unwrap();
        let file = root.path().join("main.rs");
        std::os::unix::fs::symlink(&secret, &file).unwrap();
        let manager = LspManager::new(LspConfig::default(), root.path().to_owned());
        let fake = Arc::new(FakeTransport::new(vec![]));
        manager
            .install_test_transport(Language::Rust, fake.clone())
            .await;
        assert!(
            manager
                .intelligence("diagnostics", &file, None, None, None)
                .await
                .unwrap_err()
                .contains("read file")
        );
        assert_eq!(
            fake.call_count(),
            0,
            "outside bytes must not reach the language server"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn diagnostic_freshness_accepts_verified_workspace_root_alias_only() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let alias = root.path().join("workspace-alias");
        std::os::unix::fs::symlink(&workspace, &alias).unwrap();
        let original = workspace.join("main.rs");
        std::fs::write(&original, "fn main() {} 🐋").unwrap();
        let manager = LspManager::new(LspConfig::default(), alias.clone());
        assert_eq!(
            manager
                .read_workspace_text(&original.canonicalize().unwrap())
                .await
                .unwrap(),
            "fn main() {} 🐋"
        );
        assert_eq!(
            manager
                .read_workspace_text(&alias.join("main.rs"))
                .await
                .unwrap(),
            "fn main() {} 🐋"
        );
        let outside = root.path().join("outside.rs");
        std::fs::write(&outside, "not in workspace").unwrap();
        assert!(manager.read_workspace_text(&outside).await.is_err());
        std::os::unix::fs::symlink(&outside, workspace.join("escape.rs")).unwrap();
        assert!(
            manager
                .read_workspace_text(&alias.join("escape.rs"))
                .await
                .is_err()
        );
        assert!(
            manager
                .read_workspace_text(&workspace.canonicalize().unwrap().join("escape.rs"))
                .await
                .is_err()
        );
    }
}
