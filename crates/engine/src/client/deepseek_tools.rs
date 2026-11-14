use anyhow::{Context, Result, bail};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use codewhale_models::{MessageRequest, Tool};

use super::CodewhaleClient;
use super::prepared::WireDialect;
use super::wire::{ERROR_BODY_MAX_BYTES, bounded_error_text, read_bounded_response_bytes};

pub const DEEPSEEK_WEB_SEARCH_TOOL: &str = "deepseek_web_search";
pub const DEEPSEEK_READ_URL_TOOL: &str = "deepseek_read_url";
pub(super) const DEEPSEEK_FILE_PARSE_MAX_BYTES: usize = 100 * 1024 * 1024;
pub(super) const DEEPSEEK_USER_DATA_MAX_BYTES: usize = 32 * 1024 * 1024;
pub(super) const DEEPSEEK_FILE_RESPONSE_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeepseekFilesPurpose {
    TextRecognition,
    RichLayoutRecognition,
    UserData,
}

impl DeepseekFilesPurpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextRecognition => "text_recognition",
            Self::RichLayoutRecognition => "rich_layout_recognition",
            Self::UserData => "user_data",
        }
    }

    #[must_use]
    pub const fn max_upload_bytes(self) -> usize {
        match self {
            Self::TextRecognition | Self::RichLayoutRecognition => DEEPSEEK_FILE_PARSE_MAX_BYTES,
            Self::UserData => DEEPSEEK_USER_DATA_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepseekUploadedFile {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub status_details: Option<Value>,
}

pub(super) fn deepseek_web_tool_kind(tool: &Tool) -> Option<&'static str> {
    let raw = tool.tool_type.as_deref().unwrap_or(tool.name.as_str());
    match raw {
        DEEPSEEK_WEB_SEARCH_TOOL => Some(DEEPSEEK_WEB_SEARCH_TOOL),
        DEEPSEEK_READ_URL_TOOL => Some(DEEPSEEK_READ_URL_TOOL),
        _ => None,
    }
}

pub(super) fn validate_deepseek_web_tools(tools: &[Tool], dialect: WireDialect) -> Result<bool> {
    let mut saw_search = false;
    let mut saw_read_url = false;
    let mut saw_incompatible_responses_tool = None;

    for tool in tools {
        if let Some(kind) = deepseek_web_tool_kind(tool) {
            match kind {
                DEEPSEEK_WEB_SEARCH_TOOL => {
                    if saw_search {
                        bail!("duplicate deepseek_web_search tool declaration");
                    }
                    saw_search = true;
                }
                DEEPSEEK_READ_URL_TOOL => {
                    if saw_read_url {
                        bail!("duplicate deepseek_read_url tool declaration");
                    }
                    saw_read_url = true;
                }
                _ => {}
            }
            if let Some(obj) = tool.input_schema.as_object() {
                for forbidden in ["allowed_domains", "blocked_domains", "user_location"] {
                    if obj.contains_key(forbidden) {
                        bail!("{kind} does not support `{forbidden}`");
                    }
                }
            }
        } else if dialect == WireDialect::OpenAiResponses
            && let Some(tool_type) = tool.tool_type.as_deref()
            && matches!(
                tool_type,
                "web_search" | "custom" | "namespace" | "tool_search"
            )
        {
            saw_incompatible_responses_tool = Some(tool_type.to_string());
        }
    }

    let has_deepseek_web_tools = saw_search || saw_read_url;
    if has_deepseek_web_tools && let Some(other) = saw_incompatible_responses_tool {
        bail!(
            "cannot combine deepseek_web_search/deepseek_read_url with `{other}` in Responses API"
        );
    }

    Ok(has_deepseek_web_tools)
}

pub(super) fn validate_deepseek_web_tool_choice(
    tool_choice: &Value,
    has_deepseek_web_tools: bool,
) -> Result<()> {
    let targeted_name = tool_choice
        .as_str()
        .filter(|s| !matches!(*s, "auto" | "none" | "any" | "required"))
        .or_else(|| {
            tool_choice
                .get("type")
                .and_then(Value::as_str)
                .filter(|t| matches!(*t, DEEPSEEK_WEB_SEARCH_TOOL | DEEPSEEK_READ_URL_TOOL))
        })
        .or_else(|| {
            let choice_type = tool_choice.get("type").and_then(Value::as_str);
            if matches!(choice_type, Some("tool" | "function")) || choice_type.is_none() {
                tool_choice.get("name").and_then(Value::as_str).or_else(|| {
                    tool_choice
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                })
            } else {
                None
            }
        });

    if let Some(name) = targeted_name
        && matches!(name, DEEPSEEK_WEB_SEARCH_TOOL | DEEPSEEK_READ_URL_TOOL)
    {
        bail!("tool_choice cannot force `{name}`");
    }

    if has_deepseek_web_tools
        && tool_choice
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t == "web_search")
    {
        bail!("tool_choice cannot force `web_search` when deepseek web tools are declared");
    }

    if has_deepseek_web_tools && tool_choice_is_required(tool_choice) {
        bail!(
            "tool_choice cannot require tool execution when deepseek_web_search or deepseek_read_url is declared"
        );
    }

    Ok(())
}

fn tool_choice_is_required(tool_choice: &Value) -> bool {
    tool_choice.as_str() == Some("required")
        || matches!(
            tool_choice.get("type").and_then(Value::as_str),
            Some("required" | "any")
        )
}

pub(super) fn validate_and_apply_safety_check_and_title(
    request: &MessageRequest,
    body: &mut Value,
    is_deepseek_route: bool,
) -> Result<()> {
    if !is_deepseek_route {
        if request.safety_check.is_some() || request.title.is_some() {
            bail!("safety_check and title are only supported on DeepSeek routes");
        }
        return Ok(());
    }
    if let Some(safety_check) = request.safety_check.as_ref() {
        let level = safety_check.level.as_str();
        if !matches!(level, "low" | "high") {
            bail!("safety_check.level must be `low` or `high`");
        }
        body["safety_check"] = json!({ "level": level });
    }
    if let Some(title) = request.title.as_ref() {
        body["title"] = json!({ "enabled": title.enabled });
    }
    Ok(())
}

pub(super) fn deepseek_root_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    for suffix in ["/anthropic", "/beta", "/v1"] {
        if let Some(root) = trimmed.strip_suffix(suffix) {
            return root.to_string();
        }
    }
    trimmed.to_string()
}

fn sanitize_multipart_filename(filename: &str) -> String {
    let sanitized: String = filename
        .chars()
        .map(|ch| {
            if ch == '"' || ch == '\\' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .collect();
    if sanitized.trim().is_empty() {
        "upload.bin".to_string()
    } else {
        sanitized
    }
}

fn is_http_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

fn validate_multipart_mime_type(mime_type: &str) -> Result<&str> {
    if mime_type.is_empty()
        || mime_type
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == ';')
    {
        bail!("invalid multipart mime_type `{mime_type}`");
    }
    let Some((kind, subtype)) = mime_type.split_once('/') else {
        bail!("invalid multipart mime_type `{mime_type}`");
    };
    if kind.is_empty()
        || subtype.is_empty()
        || !kind.chars().all(is_http_token_char)
        || !subtype.chars().all(is_http_token_char)
    {
        bail!("invalid multipart mime_type `{mime_type}`");
    }
    Ok(mime_type)
}

fn validate_deepseek_upload_size(purpose: DeepseekFilesPurpose, byte_len: usize) -> Result<()> {
    if byte_len == 0 {
        bail!("upload_deepseek_file requires non-empty file bytes");
    }
    let max_bytes = purpose.max_upload_bytes();
    if byte_len > max_bytes {
        bail!(
            "DeepSeek file upload exceeds {} byte limit for `{}` ({byte_len} bytes)",
            max_bytes,
            purpose.as_str()
        );
    }
    Ok(())
}

fn build_multipart_body(
    boundary: &str,
    purpose: Option<DeepseekFilesPurpose>,
    filename: &str,
    mime_type: &str,
    bytes: &[u8],
) -> Result<Vec<u8>> {
    let validated_mime = validate_multipart_mime_type(mime_type)?;
    let mut body = Vec::with_capacity(bytes.len() + 256);
    if let Some(purpose) = purpose {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"purpose\"\r\n\r\n");
        body.extend_from_slice(purpose.as_str().as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    let clean_name = sanitize_multipart_filename(filename);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{clean_name}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {validated_mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok(body)
}

impl CodewhaleClient {
    pub async fn upload_deepseek_file(
        &self,
        filename: &str,
        bytes: Vec<u8>,
        mime_type: Option<&str>,
        purpose: DeepseekFilesPurpose,
    ) -> Result<DeepseekUploadedFile> {
        validate_deepseek_upload_size(purpose, bytes.len())?;
        let boundary = format!("cw-ds-{}", uuid::Uuid::new_v4().simple());
        let mime = mime_type.unwrap_or("application/octet-stream");
        let payload = build_multipart_body(&boundary, Some(purpose), filename, mime, &bytes)?;
        let url = format!("{}/files", deepseek_root_url(&self.base_url));
        let content_type = format!("multipart/form-data; boundary={boundary}");
        let _inference_permit = self.acquire_remote_control_inference_permit().await;
        let _request_permit = self.acquire_provider_request_permit().await;
        let response = self
            .send_with_retry(|| {
                self.http_client
                    .post(&url)
                    .header(CONTENT_TYPE, &content_type)
                    .body(payload.clone())
            })
            .await
            .context("DeepSeek Files API upload failed")?;

        let status = response.status();
        if !status.is_success() {
            let raw = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            bail!("DeepSeek Files API returned HTTP {status}: {raw}");
        }
        let raw = read_bounded_response_bytes(response, DEEPSEEK_FILE_RESPONSE_MAX_BYTES)
            .await
            .context("Failed to read DeepSeek Files API response")?;
        serde_json::from_slice::<DeepseekUploadedFile>(&raw)
            .context("Failed to decode DeepSeek Files API response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn deepseek_test_client(base_url: &str) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut config = codewhale_runtime::config::Config::default();
        config.provider = Some("deepseek".to_string());
        config.providers = Some(codewhale_runtime::config::ProvidersConfig {
            deepseek: codewhale_runtime::config::ProviderConfig {
                api_key: Some("test-key".to_string()),
                base_url: Some(base_url.to_string()),
                ..Default::default()
            },
            ..Default::default()
        });
        CodewhaleClient::new(&config).expect("DeepSeek client constructs")
    }

    #[tokio::test]
    async fn upload_deepseek_file_places_purpose_before_file_and_decodes_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .respond_with(|req: &wiremock::Request| {
                let ct = req
                    .headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");
                let body = String::from_utf8_lossy(&req.body);
                let purpose_idx = body
                    .find("name=\"purpose\"")
                    .expect("multipart must contain purpose");
                let file_idx = body
                    .find("name=\"file\"; filename=\"spec.pdf\"")
                    .expect("multipart must contain file");
                assert!(
                    purpose_idx < file_idx,
                    "purpose field must precede file field in multipart body"
                );
                assert!(body.contains("rich_layout_recognition"));
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "file_ds_1",
                    "object": "file",
                    "bytes": 11,
                    "filename": "spec.pdf",
                    "purpose": "rich_layout_recognition",
                    "status": "parsed"
                }))
            })
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_test_client(&server.uri());
        let uploaded = client
            .upload_deepseek_file(
                "spec.pdf",
                b"%PDF-1.4...".to_vec(),
                Some("application/pdf"),
                DeepseekFilesPurpose::RichLayoutRecognition,
            )
            .await
            .expect("upload succeeds");
        assert_eq!(uploaded.id, "file_ds_1");
        assert_eq!(uploaded.status.as_deref(), Some("parsed"));
    }

    #[tokio::test]
    async fn upload_deepseek_file_rejects_injected_mime_type_and_sanitizes_control_filename() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .respond_with(|req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                assert!(body.contains("filename=\"report____?.pdf\""));
                assert!(!body.contains("filename=\"report\r\n"));
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "file_ds_sanitized",
                    "object": "file"
                }))
            })
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_test_client(&server.uri());
        for bad_mime in [
            "application/pdf\r\nX-Injected: true",
            "text/plain; charset=utf-8",
            "application/pdf\x00evil",
            "application/",
            "invalid",
            " application/pdf",
        ] {
            assert!(
                client
                    .upload_deepseek_file(
                        "spec.pdf",
                        b"hello".to_vec(),
                        Some(bad_mime),
                        DeepseekFilesPurpose::UserData,
                    )
                    .await
                    .is_err(),
                "mime `{bad_mime}` must be rejected"
            );
        }

        let uploaded = client
            .upload_deepseek_file(
                "report\r\n\"\x07?.pdf",
                b"%PDF-1.4".to_vec(),
                Some("application/pdf"),
                DeepseekFilesPurpose::TextRecognition,
            )
            .await
            .expect("control characters in filename are neutralized");
        assert_eq!(uploaded.id, "file_ds_sanitized");
    }

    #[tokio::test]
    async fn upload_deepseek_file_enforces_upload_and_response_byte_limits() {
        let server = MockServer::start().await;
        let oversize_body = vec![b'a'; DEEPSEEK_FILE_RESPONSE_MAX_BYTES + 128];
        Mock::given(method("POST"))
            .and(path("/files"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(oversize_body))
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_test_client(&server.uri());
        let user_data_err = client
            .upload_deepseek_file(
                "big.txt",
                vec![b'x'; DEEPSEEK_USER_DATA_MAX_BYTES + 1],
                Some("text/plain"),
                DeepseekFilesPurpose::UserData,
            )
            .await
            .expect_err("user_data over 32 MiB must fail client-side");
        assert!(user_data_err.to_string().contains("byte limit"));

        assert!(
            validate_deepseek_upload_size(
                DeepseekFilesPurpose::RichLayoutRecognition,
                DEEPSEEK_FILE_PARSE_MAX_BYTES + 1,
            )
            .is_err()
        );
        assert!(
            validate_deepseek_upload_size(
                DeepseekFilesPurpose::TextRecognition,
                DEEPSEEK_FILE_PARSE_MAX_BYTES,
            )
            .is_ok()
        );

        let response_err = client
            .upload_deepseek_file(
                "ok.pdf",
                b"ok".to_vec(),
                Some("application/pdf"),
                DeepseekFilesPurpose::TextRecognition,
            )
            .await
            .expect_err("oversized JSON response must fail bounded reader");
        assert!(
            response_err
                .chain()
                .any(|cause| cause.to_string().contains("byte limit")),
            "{response_err:#}"
        );
    }
}
