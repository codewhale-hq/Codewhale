//! Direct-fetch HTTP tool. Complements `web_search` for cases where the user
//! already knows the URL — a known repo, a blog post, a spec page — and
//! search is overkill or actively unhelpful.
//!
//! Returns a structured `{url, status, content_type, content, truncated}`
//! payload. HTML responses are stripped to readable text by default
//! (`format = "markdown"`); pass `format = "raw"` to keep the bytes intact
//! when the model wants to do its own parsing.

use super::handle::query_jsonpath;
use super::pdf::PdfTextCommand;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64,
};
use super::web::extract::{DocumentKind, ExtractedDocument, decode_response_body};
use super::web::fetch::{
    DEFAULT_MAX_BYTES, DEFAULT_TIMEOUT, FetchAttempt, FetchOptions, HARD_MAX_BYTES,
    HARD_MAX_TIMEOUT, Readable, fetch_readable,
};
use super::web::overflow::bound_text as bound_web_text;
#[cfg(test)]
use super::web::overflow::inline_char_budget;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

const FETCH_ACCEPT: &str = "text/html,text/markdown,text/plain,application/json,application/pdf,image/*,audio/*,video/*,*/*;q=0.5";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Markdown,
    Raw,
}

impl Format {
    fn parse(value: Option<&str>) -> Result<Self, ToolError> {
        match value
            .unwrap_or("markdown")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "text" | "txt" | "plain" => Ok(Self::Text),
            "markdown" | "md" => Ok(Self::Markdown),
            "raw" | "html" | "bytes" => Ok(Self::Raw),
            other => Err(ToolError::invalid_input(format!(
                "unknown format `{other}` (allowed: text, markdown, raw)"
            ))),
        }
    }
}

#[derive(Debug, Serialize)]
struct FetchResponse {
    ref_id: String,
    url: String,
    status: u16,
    headers: BTreeMap<String, String>,
    content_type: String,
    content: String,
    truncated: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    receipt: FetchReceipt,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<BTreeMap<String, Vec<Value>>>,
}

#[derive(Debug, Serialize)]
struct FetchReceipt {
    cache_hit: bool,
    retries: usize,
    redirects: usize,
    /// Every request this fetch made, in order: session `cache_hit`, whether
    /// the attempt bypassed caches, which one produced content, and the
    /// response headers that explain the cache state (#5904).
    attempts: Vec<FetchAttempt>,
}

#[derive(Debug)]
struct ArtifactWrite {
    session_id: String,
    absolute_path: std::path::PathBuf,
    relative_path: std::path::PathBuf,
    byte_size: u64,
    preview: String,
}

pub struct FetchUrlTool;

#[async_trait]
impl ToolSpec for FetchUrlTool {
    fn name(&self) -> &'static str {
        "fetch_url"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Fetch a known URL directly (HTTP GET) and return its content with a session-scoped citation ref_id. Use this instead of `curl` in `exec_shell` — sandboxed, network-policy aware, and properly decoded. Plain-text endpoints (`.md`, `.txt`, `.json`, `.yaml`, `raw.githubusercontent.com`, public APIs) prefer this over the browser/automation stack. For unknown queries, use `web_search` first. If a login or authorization wall is returned, treat the wall as the result; do not claim the protected page was read."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Absolute HTTP/HTTPS URL to fetch."
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "markdown", "raw"],
                    "description": "Post-processing for the response body. `markdown` (default) uses readability extraction and real HTML-to-Markdown conversion; `text` returns readable plain text; `raw` preserves textual response bytes. Binary media is saved as a session artifact."
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Truncate response body after this many bytes (default 1,000,000; hard max 10,485,760)."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Request timeout in milliseconds (default 15,000; max 60,000)."
                },
                "fields": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional JSONPath projections for JSON responses. Supports $, .field, [index], [*], and ['field']; returns matches under `fields`."
                }
            },
            "required": ["url"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Network]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // Read-only HTTP can still disclose local data through a URL or query.
        // Host allowlisting controls reachability, not approval of this payload.
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let url = input
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid_input("`url` is required"))?
            .trim()
            .to_string();

        if url.is_empty() {
            return Err(ToolError::invalid_input("`url` cannot be empty"));
        }
        let scheme_ok = url.starts_with("http://") || url.starts_with("https://");
        if !scheme_ok {
            return Err(ToolError::invalid_input(
                "only http:// and https:// URLs are supported",
            ));
        }

        let format = Format::parse(input.get("format").and_then(Value::as_str))?;
        let max_bytes =
            usize::try_from(optional_u64(&input, "max_bytes", DEFAULT_MAX_BYTES as u64)?)
                .unwrap_or(HARD_MAX_BYTES)
                .clamp(1, HARD_MAX_BYTES);
        let timeout_ms = optional_u64(&input, "timeout_ms", DEFAULT_TIMEOUT.as_millis() as u64)?
            .clamp(1, HARD_MAX_TIMEOUT.as_millis() as u64);
        let requested_fields = parse_fields(&input)?;
        // Bound as a reference so the `Fn` extractor can run twice without
        // moving the field list into its first future.
        let requested_fields = &requested_fields;
        // A 2xx that extracts to nothing is re-fetched once past every cache
        // before it becomes a partial page or an error; the receipt keeps both
        // attempts (#5904).
        let readable = fetch_readable(
            &url,
            &FetchOptions::new(Duration::from_millis(timeout_ms), max_bytes, FETCH_ACCEPT),
            context,
            "fetch_url",
            |fetched: super::web::fetch::FetchedPayload| {
                Box::pin(async move {
                    let is_success = (200..300).contains(&fetched.status);
                    let body_text = if requested_fields.is_empty() {
                        None
                    } else {
                        // JSON is never allowed to discover an encoding from body markup.
                        Some(decode_response_body(
                            &fetched.bytes,
                            Some(&fetched.content_type),
                            false,
                        )?)
                    };
                    let fields = match body_text.as_deref() {
                        Some(body) => {
                            project_json_fields(body, &fetched.content_type, requested_fields)?
                        }
                        None => None,
                    };
                    let extracted = extract_fetched_document(
                        format,
                        &fetched.url,
                        &fetched.content_type,
                        &fetched.bytes,
                        fetched.truncated,
                        is_success,
                        body_text.as_deref(),
                        PdfTextCommand::system(Some(context)),
                    )
                    .await?;
                    Ok((extracted, fields))
                })
            },
        )
        .await?;
        let super::web::fetch::ReadableFetch {
            payload: fetched,
            document,
            attempts,
        } = readable;
        let ((extracted, fields), note) = match document {
            Readable::Document(document) => (document, None),
            Readable::Partial(page) => ((page.document, None), Some(page.note)),
        };
        let is_success = (200..300).contains(&fetched.status);

        let citation_title = extracted.title.clone();
        let (processed, artifact_write) = render_extracted(
            &fetched.url,
            &fetched.content_type,
            format,
            extracted,
            &fetched.bytes,
            context,
        )?;
        let artifact = artifact_write.as_ref().map(|write| {
            codewhale_runtime::artifacts::format_artifact_relative_path(&write.relative_path)
        });

        let citation = super::web::citations::register(
            &context.state_namespace,
            &fetched.url,
            citation_title.as_deref(),
        )
        .ok_or_else(|| ToolError::execution_failed("fetched URL could not be registered"))?;
        let response = FetchResponse {
            ref_id: citation.ref_id,
            url: citation.url,
            status: fetched.status,
            headers: fetched.headers,
            content_type: fetched.content_type,
            content: processed,
            truncated: fetched.truncated,
            partial: note.is_some(),
            note,
            receipt: FetchReceipt {
                cache_hit: fetched.cache_hit,
                retries: fetched.retries,
                redirects: fetched.redirects,
                attempts,
            },
            artifact,
            fields,
        };

        let content = serde_json::to_string_pretty(&response).map_err(|error| {
            ToolError::execution_failed(format!("failed to serialize response: {error}"))
        })?;
        let metadata = artifact_write.map(artifact_metadata);

        if !is_success {
            // Don't `Err` on 4xx/5xx — the caller often wants to see the body
            // (e.g. a JSON error envelope). Mark the result as a failure so the
            // engine renders it as such.
            return Ok(ToolResult {
                content,
                success: false,
                metadata,
            });
        }

        Ok(ToolResult {
            content,
            success: true,
            metadata,
        })
    }
}

async fn extract_fetched_document(
    format: Format,
    url: &str,
    content_type: &str,
    bytes: &[u8],
    truncated: bool,
    is_success: bool,
    decoded_body: Option<&str>,
    pdf_command: PdfTextCommand<'_>,
) -> super::web::adapter::AdapterResult<ExtractedDocument> {
    let extraction = if format == Format::Raw
        && super::web::extract::validate_pdf_response(url, Some(content_type), bytes)?
    {
        Ok(ExtractedDocument {
            kind: DocumentKind::Pdf,
            title: Some("PDF Document".to_string()),
            text: String::new(),
            markdown: String::new(),
            cleaned_html: None,
            pdf_pages: None,
            media_extension: None,
        })
    } else {
        super::web::extract::extract_document_with_pdf_command(
            url,
            Some(content_type),
            bytes,
            truncated,
            pdf_command,
        )
        .await
    };
    match extraction {
        Ok(document) => Ok(document),
        Err(error)
            if error.content()
                && (format == Format::Raw || !is_success)
                && is_declared_textual(content_type) =>
        {
            let body_text = match decoded_body {
                Some(body_text) => body_text.to_string(),
                None => {
                    decode_response_body(bytes, Some(content_type), is_declared_html(content_type))?
                }
            };
            Ok(ExtractedDocument {
                kind: DocumentKind::Text,
                title: None,
                text: body_text.clone(),
                markdown: body_text,
                cleaned_html: None,
                pdf_pages: None,
                media_extension: None,
            })
        }
        Err(error) => Err(error),
    }
}

fn is_declared_textual(content_type: &str) -> bool {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    content_type.starts_with("text/")
        || content_type.contains("html")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("yaml")
        || content_type.contains("javascript")
}

fn is_declared_html(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "text/html" | "application/xhtml+xml"
    )
}

fn render_extracted(
    url: &str,
    content_type: &str,
    format: Format,
    document: ExtractedDocument,
    bytes: &[u8],
    context: &ToolContext,
) -> Result<(String, Option<ArtifactWrite>), ToolError> {
    if document.kind == DocumentKind::Pdf && format != Format::Raw {
        let extracted = match format {
            Format::Text => document.text,
            Format::Markdown => document.markdown,
            Format::Raw => unreachable!("raw PDF handled below"),
        };
        return bound_text(url, extracted, context);
    }

    if document.kind == DocumentKind::Media || document.kind == DocumentKind::Pdf {
        let extension = document
            .media_extension
            .unwrap_or(if document.kind == DocumentKind::Pdf {
                "pdf"
            } else {
                "bin"
            });
        let artifact = write_binary_artifact(url, extension, bytes, context)?;
        let relative =
            codewhale_runtime::artifacts::format_artifact_relative_path(&artifact.relative_path);
        let label = if document.kind == DocumentKind::Pdf {
            "PDF"
        } else {
            "media"
        };
        let content =
            format!("[{label} response saved to {relative}; content type: {content_type}.]");
        return Ok((content, Some(artifact)));
    }

    let content = match format {
        Format::Raw => decode_response_body(
            bytes,
            Some(content_type),
            document.kind == DocumentKind::Html,
        )?,
        Format::Text => document.text,
        Format::Markdown => document.markdown,
    };
    bound_text(url, content, context)
}

fn bound_text(
    url: &str,
    content: String,
    context: &ToolContext,
) -> Result<(String, Option<ArtifactWrite>), ToolError> {
    let bounded = bound_web_text(
        content,
        context,
        |body| fetch_artifact_id(url, body.as_bytes()),
        "page",
    )?;
    let artifact = bounded.artifact.map(|artifact| ArtifactWrite {
        session_id: artifact.session_id,
        absolute_path: artifact.absolute_path,
        relative_path: artifact.relative_path,
        byte_size: artifact.byte_size,
        preview: artifact.preview,
    });
    Ok((bounded.content, artifact))
}

fn write_binary_artifact(
    url: &str,
    extension: &str,
    bytes: &[u8],
    context: &ToolContext,
) -> Result<ArtifactWrite, ToolError> {
    let artifact_id = fetch_artifact_id(url, bytes);
    let (absolute_path, relative_path) =
        codewhale_runtime::artifacts::write_session_artifact_bytes(
            &context.state_namespace,
            &artifact_id,
            extension,
            bytes,
        )
        .map_err(|error| {
            ToolError::execution_failed(format!(
                "failed to preserve fetched media artifact: {error}"
            ))
        })?;
    Ok(ArtifactWrite {
        session_id: context.state_namespace.clone(),
        absolute_path,
        relative_path,
        byte_size: bytes.len() as u64,
        preview: format!("Fetched {extension} artifact from {url}"),
    })
}

fn fetch_artifact_id(url: &str, bytes: &[u8]) -> String {
    let mut identity = Vec::with_capacity(url.len() + bytes.len());
    identity.extend_from_slice(url.as_bytes());
    identity.extend_from_slice(bytes);
    let digest = codewhale_runtime::hashing::sha256_hex(&identity);
    format!("fetch_{}", &digest[..16])
}

fn artifact_metadata(write: ArtifactWrite) -> Value {
    json!({
        "spillover_path": write.absolute_path.display().to_string(),
        "artifact_session_id": write.session_id,
        "artifact_relative_path": codewhale_runtime::artifacts::format_artifact_relative_path(&write.relative_path),
        "artifact_byte_size": write.byte_size,
        "artifact_preview": write.preview,
        // Every artifact this tool writes is retrievable evidence, so flag it
        // for the engine's `activate_result_dependencies` (same contract as
        // the shell-truncation spillover): the next turn auto-activates
        // `retrieve_tool_result`. Text overflows name that tool in their
        // footer; binary PDF/media saves name only the saved-artifact path in
        // their inline pointer, so this flag is what makes that pointer
        // actionable rather than a dead end.
        "evidence_available": true,
    })
}

fn parse_fields(input: &Value) -> Result<Vec<String>, ToolError> {
    let Some(values) = input.get("fields") else {
        return Ok(Vec::new());
    };
    let Some(values) = values.as_array() else {
        return Err(ToolError::invalid_input("`fields` must be an array"));
    };
    let mut fields = Vec::new();
    for value in values {
        let Some(field) = value.as_str() else {
            return Err(ToolError::invalid_input(
                "`fields` entries must be JSONPath strings",
            ));
        };
        let field = field.trim();
        if !field.is_empty() {
            fields.push(field.to_string());
        }
    }
    Ok(fields)
}

fn project_json_fields(
    body_text: &str,
    content_type: &str,
    fields: &[String],
) -> Result<Option<BTreeMap<String, Vec<Value>>>, ToolError> {
    if fields.is_empty() {
        return Ok(None);
    }
    if !content_type.to_ascii_lowercase().contains("json") {
        return Err(ToolError::invalid_input(
            "`fields` can only be used with JSON responses",
        ));
    }
    let body_json: Value = serde_json::from_str(body_text).map_err(|e| {
        ToolError::execution_failed(format!("response body is not valid JSON for `fields`: {e}"))
    })?;
    let mut out = BTreeMap::new();
    for field in fields {
        let matches = query_jsonpath(&body_json, field).map_err(|e| {
            ToolError::invalid_input(format!("invalid JSONPath `{field}` in `fields`: {e}"))
        })?;
        out.insert(field.clone(), matches);
    }
    Ok(Some(out))
}

#[cfg(test)]
#[path = "fetch_url/tests.rs"]
mod pdf_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::ToolContext;
    use std::path::PathBuf;

    struct ArtifactRootRestore(Option<PathBuf>);

    impl Drop for ArtifactRootRestore {
        fn drop(&mut self) {
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
        }
    }

    fn ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."))
    }

    /// `fetch_url` can disclose local data through a URL or query, so it always
    /// asks: the tool declares `Required`, and the default-ask policy resolves
    /// that to a prompt rather than running it.
    #[test]
    fn fetch_url_always_requires_approval() {
        use crate::tools::spec::{ApprovalRequirement, ToolSpec};
        assert_eq!(
            FetchUrlTool.approval_requirement(),
            ApprovalRequirement::Required
        );
        assert!(
            FetchUrlTool
                .capabilities()
                .contains(&ToolCapability::Network)
        );

        // Under the default Ask posture that requirement resolves to a prompt,
        // and only full access (or an explicit bypass) lets it through.
        use crate::core::authority::{ToolPermission, TurnAuthority, resolve_tool_permission};
        use codewhale_config::AppMode;
        use codewhale_execpolicy::ApprovalMode;
        let requirement = FetchUrlTool.approval_requirement();
        let ask = TurnAuthority::from_effective_fields(
            AppMode::Agent,
            true,
            false,
            false,
            ApprovalMode::Suggest,
        );
        assert_eq!(
            resolve_tool_permission(&ask, requirement, false),
            ToolPermission::Prompt
        );
        let never = TurnAuthority::from_effective_fields(
            AppMode::Agent,
            true,
            false,
            false,
            ApprovalMode::Never,
        );
        assert_eq!(
            resolve_tool_permission(&never, requirement, false),
            ToolPermission::Deny
        );
    }

    #[test]
    fn format_parse_accepts_aliases_and_rejects_unknown() {
        assert_eq!(Format::parse(Some("markdown")).unwrap(), Format::Markdown);
        assert_eq!(Format::parse(Some("MD")).unwrap(), Format::Markdown);
        assert_eq!(Format::parse(Some("text")).unwrap(), Format::Text);
        assert_eq!(Format::parse(Some("raw")).unwrap(), Format::Raw);
        assert_eq!(Format::parse(None).unwrap(), Format::Markdown);
        assert!(Format::parse(Some("yaml")).is_err());
    }

    #[test]
    fn raw_text_uses_declared_charset_and_rejects_nul_binary() {
        let document = || ExtractedDocument {
            kind: DocumentKind::Text,
            title: None,
            text: String::new(),
            markdown: String::new(),
            cleaned_html: None,
            pdf_pages: None,
            media_extension: None,
        };
        let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode("café");
        let (content, artifact) = render_extracted(
            "https://example.com/plain",
            "text/plain; charset=windows-1252",
            Format::Raw,
            document(),
            &bytes,
            &ctx(),
        )
        .expect("decode raw text");
        assert_eq!(content, "café");
        assert!(artifact.is_none());

        let error = render_extracted(
            "https://example.com/not-text",
            "text/plain",
            Format::Raw,
            document(),
            b"binary\0payload",
            &ctx(),
        )
        .expect_err("raw text must retain the binary guard");
        assert!(error.to_string().contains("NUL bytes"));
    }

    #[test]
    fn textual_and_html_fallback_classification_is_exact() {
        assert!(is_declared_textual("Application/JSON; charset=utf-8"));
        assert!(is_declared_html("TEXT/HTML; charset=gbk"));
        assert!(!is_declared_html("text/plain; note=text/html"));
        assert!(!is_declared_textual("application/octet-stream"));
    }

    #[test]
    fn route_budget_overflow_round_trips_through_session_artifact() {
        let _lock = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
            tmp.path().join("sessions"),
        ));
        let _restore = ArtifactRootRestore(prior);
        let context = ToolContext::new(".")
            .with_state_namespace("fetch-overflow")
            .with_route_context_window(10_000);
        let full = "Whale content. ".repeat(200);

        let (inline, artifact) =
            bound_text("https://example.com/large", full.clone(), &context).unwrap();
        let artifact = artifact.expect("overflow artifact");

        assert!(inline.contains("retrieve_tool_result"));
        assert!(inline.chars().count() <= inline_char_budget(&context));
        assert_eq!(
            std::fs::read_to_string(&artifact.absolute_path).unwrap(),
            full
        );
        // The footer names `retrieve_tool_result` as the recovery path; the
        // evidence flag is what makes the engine auto-activate that tool on
        // the next turn, so the named tool is actually present.
        let metadata = artifact_metadata(artifact);
        assert_eq!(
            metadata.get("evidence_available"),
            Some(&json!(true)),
            "artifact metadata must flag retrievable evidence:\n{metadata}"
        );
    }

    #[test]
    fn project_json_fields_returns_requested_jsonpath_matches() {
        let fields = vec!["$.items[*].name".to_string(), "$.count".to_string()];
        let projected = project_json_fields(
            r#"{"items":[{"name":"alpha"},{"name":"beta"}],"count":2}"#,
            "application/json",
            &fields,
        )
        .expect("project")
        .expect("some");

        assert_eq!(
            projected.get("$.items[*].name").unwrap(),
            &vec![json!("alpha"), json!("beta")]
        );
        assert_eq!(projected.get("$.count").unwrap(), &vec![json!(2)]);
    }

    #[test]
    fn project_json_fields_rejects_non_json_content_type() {
        let fields = vec!["$.name".to_string()];
        let err = project_json_fields("{}", "text/plain", &fields).expect_err("must reject");
        assert!(format!("{err}").contains("JSON responses"));
    }

    #[tokio::test]
    async fn rejects_non_http_schemes() {
        let tool = FetchUrlTool;
        let res = tool
            .execute(json!({"url": "file:///etc/passwd"}), &ctx())
            .await;
        let err = res.unwrap_err();
        assert!(format!("{err:?}").contains("http"));
    }

    #[tokio::test]
    async fn rejects_empty_url() {
        let tool = FetchUrlTool;
        let res = tool.execute(json!({"url": "   "}), &ctx()).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn rejects_missing_url() {
        let tool = FetchUrlTool;
        let res = tool.execute(json!({}), &ctx()).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn rejects_localhost_hostname() {
        let tool = FetchUrlTool;
        let res = tool
            .execute(json!({"url": "http://localhost:8080/admin"}), &ctx())
            .await;
        let err = res.unwrap_err();
        assert!(format!("{err}").contains("localhost"));
    }

    #[tokio::test]
    async fn network_policy_denies_blocked_host() {
        use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};
        let policy = NetworkPolicy {
            default: Decision::Deny.into(),
            allow: vec!["api.deepseek.com".to_string()],
            deny: vec![],
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ctx = ToolContext::new(PathBuf::from(".")).with_network_policy(decider);
        let tool = FetchUrlTool;
        let res = tool
            .execute(json!({"url": "https://example.com/foo"}), &ctx)
            .await;
        let err = res.expect_err("blocked host should fail");
        assert!(format!("{err}").contains("blocked"));
    }

    #[tokio::test]
    async fn proxy_opt_in_does_not_allow_restricted_ip_literal() {
        use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: vec!["198.18.0.1".to_string()],
            proxy_fake_ip_cidrs: vec!["198.18.0.0/15".to_string()],
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ctx = ToolContext::new(PathBuf::from(".")).with_network_policy(decider);
        let tool = FetchUrlTool;

        let err = tool
            .execute(json!({"url": "http://198.18.0.1/status"}), &ctx)
            .await
            .expect_err("literal restricted IP URLs must stay blocked");

        assert!(format!("{err}").contains("IP 198.18.0.1 is a restricted address"));
    }
}
