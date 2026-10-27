//! `image_analyze` tool — analyze images using a dedicated vision model.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};

use crate::client::CodewhaleClient;
use crate::llm_client::{LlmError, RetryConfig, sanitize_http_error_body, with_retry};
use crate::tools::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};
use codewhale_config::ProviderKind;
use codewhale_runtime::config::VisionModelConfig;

pub struct ImageAnalyzeTool {
    config: VisionModelConfig,
    client: reqwest::Client,
    route_client: Option<CodewhaleClient>,
}

/// Total envelope for one image_analyze call, retry attempts and response
/// body consumption included. reqwest's `read_timeout` is *not* a per-read
/// idle bound for the request phase: its timer starts at `send()` and is
/// never reset until the response headers arrive, so it silently acts as a
/// total deadline on the multi-MB upload plus the full non-streaming vision
/// generation — precisely the healthy work a 120s cap used to kill. The
/// client therefore bounds only the connect handshake, and this envelope
/// (the same 30-minute wall clock as engine streaming,
/// `DEFAULT_STREAM_MAX_DURATION_SECS`) is the sole total bound; a stalled
/// connection errors out through it instead of hanging.
const VISION_REQUEST_ENVELOPE: Duration = Duration::from_secs(1800);

fn vision_request_envelope() -> Duration {
    if cfg!(test) {
        Duration::from_secs(2)
    } else {
        VISION_REQUEST_ENVELOPE
    }
}

impl ImageAnalyzeTool {
    #[cfg(test)]
    #[must_use]
    pub fn new(config: VisionModelConfig) -> Self {
        Self::new_with_route_client(config, None)
    }

    #[must_use]
    pub fn new_with_route_client(
        config: VisionModelConfig,
        route_client: Option<CodewhaleClient>,
    ) -> Self {
        let client = codewhale_release::tls::reqwest_client_builder()
            // Bound only the connect handshake. A client- or request-level
            // `read_timeout` would start counting at `send()` and never
            // reset before the response headers, quietly re-introducing a
            // total deadline on the upload + long non-streaming generation;
            // the total bound lives in VISION_REQUEST_ENVELOPE instead.
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");
        Self {
            config,
            client,
            route_client,
        }
    }

    async fn read_image_file(path: &Path) -> Result<(Vec<u8>, String), ToolError> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::execution_failed(format!("Failed to read image file: {e}")))?;

        let mime_type = Self::detect_mime_type(path)?;
        Ok((bytes, mime_type))
    }

    fn resolve_image_path(workspace: &Path, image_path: &str) -> Result<PathBuf, ToolError> {
        let image_path_buf = Path::new(image_path);
        if image_path_buf.components().any(|c| {
            matches!(
                c,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        }) {
            return Err(ToolError::execution_failed(
                "image_path must be a relative path within the workspace and cannot escape it.",
            ));
        }

        let workspace = workspace.canonicalize().map_err(|e| {
            ToolError::execution_failed(format!("Failed to resolve workspace path: {e}"))
        })?;
        let candidate = workspace.join(image_path_buf);
        let resolved = candidate.canonicalize().map_err(|e| {
            ToolError::execution_failed(format!("Failed to resolve image file: {e}"))
        })?;
        if !resolved.starts_with(&workspace) {
            return Err(ToolError::execution_failed(
                "image_path must resolve within the workspace and cannot escape it.",
            ));
        }
        Ok(resolved)
    }

    /// Header-only dimensions of the same bytes sent to the vision model,
    /// using the extension-derived MIME type. Run on a blocking worker; omit
    /// metadata for unparsable containers (including unsupported BMP).
    /// Animated GIF/WebP yield the first frame's size.
    fn image_dimensions(bytes: &[u8], mime_type: &str) -> Option<(u32, u32, String)> {
        let format = mime_type.strip_prefix("image/")?.to_string();
        let reader = image::ImageReader::with_format(
            std::io::Cursor::new(bytes),
            image::ImageFormat::from_mime_type(mime_type)?,
        );
        let (width, height) = reader.into_dimensions().ok()?;
        Some((width, height, format))
    }

    fn detect_mime_type(path: &Path) -> Result<String, ToolError> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match extension.as_str() {
            "png" => Ok("image/png".to_string()),
            "jpg" | "jpeg" => Ok("image/jpeg".to_string()),
            "gif" => Ok("image/gif".to_string()),
            "webp" => Ok("image/webp".to_string()),
            "bmp" => Ok("image/bmp".to_string()),
            _ => Err(ToolError::execution_failed(format!(
                "Unsupported image format: {extension}"
            ))),
        }
    }

    fn base_url(&self) -> String {
        self.config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }

    fn api_key(&self) -> String {
        self.config.api_key.clone().unwrap_or_default()
    }

    fn is_xiaomi_mimo_model(model: &str) -> bool {
        let normalized = model.trim().to_ascii_lowercase();
        let normalized = normalized.strip_prefix("xiaomi/").unwrap_or(&normalized);
        normalized.starts_with("mimo-")
    }

    fn uses_max_completion_tokens(config: &VisionModelConfig) -> bool {
        if Self::is_xiaomi_mimo_model(&config.model) {
            return true;
        }

        let base_url = config.base_url.as_deref().unwrap_or_default();
        let Ok(url) = reqwest::Url::parse(base_url) else {
            return false;
        };
        let Some(domain) = url.domain() else {
            return false;
        };

        domain.eq_ignore_ascii_case("xiaomimimo.com")
            || domain.to_ascii_lowercase().ends_with(".xiaomimimo.com")
    }

    fn wire_model(&self) -> String {
        let model = self.config.model.trim();
        if !Self::is_xiaomi_mimo_model(model) || model.contains('/') {
            return self.config.model.clone();
        }
        codewhale_runtime::config::canonical_xiaomi_mimo_model_id(model)
            .map_or_else(|| self.config.model.clone(), str::to_string)
    }

    fn request_payload(&self, prompt: &str, image_data: &str, mime_type: &str) -> Value {
        let mut payload = json!({
            "model": self.wire_model(),
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": prompt},
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", mime_type, image_data)
                            }
                        }
                    ]
                }
            ]
        });

        let token_limit_field = if Self::uses_max_completion_tokens(&self.config) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        let configured_base = self.base_url();
        let route_cap = self
            .route_client
            .as_ref()
            .filter(|client| {
                client.base_url().trim_end_matches('/') == configured_base.trim_end_matches('/')
            })
            .map_or_else(
                || {
                    // A standalone `[vision_model]` route has no resolved
                    // max-model-len fact. Do not guess one or let a process
                    // override turn a capability maximum into an unbounded
                    // request; a matched active client above carries exact
                    // route limits when the vision route is shared.
                    crate::route_budget::effective_max_output_tokens_for_route(
                        ProviderKind::Custom,
                        &self.config.model,
                        None,
                    )
                    .min(65_536)
                },
                |client| client.effective_max_output_tokens(&self.config.model),
            );
        payload[token_limit_field] = json!(route_cap);
        if let Some(client) = self.route_client.as_ref().filter(|client| {
            client.base_url().trim_end_matches('/') == configured_base.trim_end_matches('/')
        }) {
            client.apply_provider_routing(&mut payload);
        }

        payload
    }
}

#[async_trait]
impl ToolSpec for ImageAnalyzeTool {
    fn name(&self) -> &str {
        "image_analyze"
    }

    fn description(&self) -> &str {
        "Analyze an image using the configured vision model. \
         Supports PNG, JPEG, GIF, WebP, and BMP formats. \
         When the runtime can determine them from the image container, the \
         result includes the image's stored pixel width and height, plus a \
         format label derived from the file extension — describe image size \
         from that metadata instead of guessing by eye. The dimensions are \
         as stored: camera rotation metadata is not applied, and the fields \
         are omitted when the container cannot be sized."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "image_path": {
                    "type": "string",
                    "description": "Path to the image file to analyze"
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional prompt to guide the analysis."
                }
            },
            "required": ["image_path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let image_path = required_str(&input, "image_path")?;
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("Describe this image in detail.");

        let resolved_path = Self::resolve_image_path(&context.workspace, image_path)?;
        let (image_bytes, mime_type) = Self::read_image_file(&resolved_path).await?;
        let dimension_mime_type = mime_type.clone();
        // Header parsing and encoding stay off the async worker and use one
        // file snapshot. A failed probe omits metadata without failing vision.
        let (image_data, dimensions) = tokio::task::spawn_blocking(move || {
            let dimensions = Self::image_dimensions(&image_bytes, &dimension_mime_type);
            (BASE64.encode(&image_bytes), dimensions)
        })
        .await
        .map_err(|e| ToolError::execution_failed(format!("Failed to prepare image file: {e}")))?;

        let payload = self.request_payload(prompt, &image_data, &mime_type);

        let url = format!("{}/chat/completions", self.base_url());
        let api_key = self.api_key();

        let retry_config = RetryConfig {
            max_retries: 3,
            initial_delay: 1.0,
            max_delay: 30.0,
            enabled: true,
            ..Default::default()
        };
        let _inference = match self.route_client.as_ref() {
            Some(client) => client.acquire_remote_control_inference_permit().await,
            None => Some(crate::client::acquire_remote_control_inference_participant().await),
        };

        let response_json = tokio::time::timeout(vision_request_envelope(), async {
            let response = with_retry(
                &retry_config,
                || {
                    let client = self.client.clone();
                    let url = url.clone();
                    let api_key = api_key.clone();
                    let payload = payload.clone();
                    async move {
                        let response = client
                            .post(&url)
                            .header("Content-Type", "application/json")
                            .header("Authorization", format!("Bearer {api_key}"))
                            .json(&payload)
                            .send()
                            .await
                            .map_err(|e| LlmError::from_reqwest(&e))?;

                        let status = response.status();
                        if !status.is_success() {
                            let error_text = response
                                .text()
                                .await
                                .unwrap_or_else(|_| "Unknown error".to_string());
                            let error_text = sanitize_http_error_body(
                                Some("Vision provider"),
                                status.as_u16(),
                                &error_text,
                            );
                            return Err(LlmError::from_http_response(status.as_u16(), &error_text));
                        }
                        Ok(response)
                    }
                },
                None,
            )
            .await
            .map_err(|e| ToolError::execution_failed(format!("Vision API request failed: {e}")))?;

            let json: Value = response.json().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to parse response: {e}"))
            })?;
            Ok::<Value, ToolError>(json)
        })
        .await
        .map_err(|_| ToolError::Timeout {
            seconds: vision_request_envelope().as_secs(),
        })??;

        let content = response_json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let model = response_json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or(&self.config.model)
            .to_string();

        let mut result = json!({
            "analysis": content,
            "model": model,
        });
        if let Some((width, height, format)) = dimensions {
            result["width"] = json!(width);
            result["height"] = json!(height);
            result["format"] = json!(format);
        }

        ToolResult::json(&result)
            .map_err(|e| ToolError::execution_failed(format!("Failed to serialize result: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[cfg(unix)]
    fn create_file_symlink(
        target: &std::path::Path,
        link: &std::path::Path,
    ) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_file_symlink(
        target: &std::path::Path,
        link: &std::path::Path,
    ) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    fn fake_config() -> VisionModelConfig {
        VisionModelConfig {
            model: "test-vision-model".to_string(),
            api_key: Some("test-key".to_string()),
            base_url: Some("https://example.invalid/v1".to_string()),
        }
    }

    /// The cap reads the `*_MAX_OUTPUT_TOKENS` override from the process
    /// environment, as the payload does. Tests that compare the two hold the
    /// test env lock, so an engine test setting the override cannot land
    /// between the two reads in a shared process.
    fn standalone_vision_cap(model: &str) -> u64 {
        u64::from(
            crate::route_budget::effective_max_output_tokens_for_route(
                ProviderKind::Custom,
                model,
                None,
            )
            .min(65_536),
        )
    }

    #[test]
    fn tool_metadata_is_read_only_and_named_image_analyze() {
        let tool = ImageAnalyzeTool::new(fake_config());
        assert_eq!(tool.name(), "image_analyze");
        assert!(tool.capabilities().contains(&ToolCapability::ReadOnly));
    }

    #[test]
    fn mime_type_detection_covers_common_formats() {
        for (ext, expected) in [
            ("png", "image/png"),
            ("PNG", "image/png"),
            ("jpg", "image/jpeg"),
            ("jpeg", "image/jpeg"),
            ("gif", "image/gif"),
            ("webp", "image/webp"),
            ("bmp", "image/bmp"),
        ] {
            let path = std::path::PathBuf::from(format!("test.{ext}"));
            let mime = ImageAnalyzeTool::detect_mime_type(&path)
                .unwrap_or_else(|_| panic!("must detect {ext}"));
            assert_eq!(mime, expected);
        }
    }

    #[test]
    fn mime_type_detection_rejects_unsupported_extension() {
        let path = std::path::PathBuf::from("test.svg");
        let err = ImageAnalyzeTool::detect_mime_type(&path)
            .expect_err("svg is intentionally out of scope for vision tool");
        assert!(err.to_string().contains("Unsupported image format"));
    }

    #[test]
    fn generic_vision_payload_uses_max_tokens() {
        let _env = crate::test_support::lock_test_env();
        let tool = ImageAnalyzeTool::new(fake_config());

        let payload = tool.request_payload("describe", "abc123", "image/png");

        assert_eq!(
            payload.get("max_tokens").and_then(Value::as_u64),
            Some(standalone_vision_cap(&tool.config.model))
        );
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("max_completion_tokens").is_none());
    }

    #[test]
    fn xiaomi_mimo_vision_payload_uses_max_completion_tokens() {
        let _env = crate::test_support::lock_test_env();
        let mut config = fake_config();
        config.model = "mimo-v2.5".to_string();
        config.base_url = Some("https://api.xiaomimimo.com/v1".to_string());
        let tool = ImageAnalyzeTool::new(config);

        let payload = tool.request_payload("describe", "abc123", "image/png");

        assert_eq!(
            payload.get("max_completion_tokens").and_then(Value::as_u64),
            Some(standalone_vision_cap(&tool.config.model))
        );
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("max_tokens").is_none());
    }

    #[test]
    fn xiaomi_mimo_vision_payload_uses_max_completion_tokens_with_custom_proxy() {
        let _env = crate::test_support::lock_test_env();
        let mut config = fake_config();
        config.model = "mimo-v2.5".to_string();
        config.base_url = Some("https://vision-proxy.example.invalid/v1".to_string());
        let tool = ImageAnalyzeTool::new(config);

        let payload = tool.request_payload("describe", "abc123", "image/png");

        assert_eq!(
            payload.get("max_completion_tokens").and_then(Value::as_u64),
            Some(standalone_vision_cap(&tool.config.model))
        );
        assert!(payload.get("max_tokens").is_none());
    }

    #[test]
    fn vision_vendor_pin_requires_the_matching_bound_route() {
        let _lock = crate::test_support::lock_test_env();
        let base_url = "http://127.0.0.1:18080/v1";
        let client = CodewhaleClient::new(&{
            let __cw_config_init_0_0 = Some("openrouter".to_string());
            let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
                openrouter: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("fixture-openrouter-key".to_string()),
                    base_url: Some(base_url.to_string()),
                    model: Some("fixture/vision".to_string()),
                    vendor: Some("chutes/region-fixture".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        })
        .unwrap();
        for (vision_base, matched_client, pinned) in [
            (base_url, Some(client.clone()), true),
            ("http://127.0.0.1:18081/v1", Some(client.clone()), false),
            (base_url, None, false),
        ] {
            let tool = ImageAnalyzeTool::new_with_route_client(
                VisionModelConfig {
                    model: "fixture/vision".to_string(),
                    api_key: Some("fixture-vision-key".to_string()),
                    base_url: Some(vision_base.to_string()),
                },
                matched_client,
            );
            let body = tool.request_payload("describe", "abc123", "image/png");
            if pinned {
                assert_eq!(
                    body["provider"],
                    json!({
                        "order": ["chutes/region-fixture"], "allow_fallbacks": false
                    })
                );
            } else {
                assert!(body.get("provider").is_none());
            }
        }
    }

    #[test]
    fn matched_vision_route_uses_bound_client_window_cap() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "384000");
        let base_url = "http://127.0.0.1:18080/v1".to_string();
        let model = "DeepSeek-V4-Flash".to_string();
        let client = CodewhaleClient::new(&{
            let __cw_config_init_1_0 = Some("vllm".to_string());
            let __cw_config_init_1_1 = Some(codewhale_runtime::config::ProvidersConfig {
                vllm: codewhale_runtime::config::ProviderConfig {
                    base_url: Some(base_url.clone()),
                    model: Some(model.clone()),
                    context_window: Some(327_680),
                    ..codewhale_runtime::config::ProviderConfig::default()
                },
                ..codewhale_runtime::config::ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: codewhale_runtime::config::Config =
                codewhale_runtime::config::Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        })
        .expect("bound vLLM client");
        let tool = ImageAnalyzeTool::new_with_route_client(
            VisionModelConfig {
                model,
                api_key: None,
                base_url: Some(base_url),
            },
            Some(client),
        );

        let payload = tool.request_payload("describe", "abc123", "image/png");
        assert_eq!(payload["max_tokens"], 325_632);
    }

    #[tokio::test]
    async fn execute_rejects_absolute_path() {
        // Trust-boundary pin: image_path must stay inside the workspace
        // — an absolute path or a `..`-traversing path must reject
        // before any base64 / API call.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ImageAnalyzeTool::new(fake_config());
        let outside_workspace = if cfg!(windows) {
            r"C:\Windows\System32\drivers\etc\hosts"
        } else {
            "/etc/hosts"
        };
        let err = tool
            .execute(json!({"image_path": outside_workspace}), &ctx)
            .await
            .expect_err("absolute path must reject");
        assert!(
            err.to_string()
                .contains("relative path within the workspace"),
            "error must call out the workspace boundary; got {err}"
        );
    }

    #[tokio::test]
    async fn execute_rejects_parent_dir_traversal() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ImageAnalyzeTool::new(fake_config());
        let err = tool
            .execute(json!({"image_path": "../escape.png"}), &ctx)
            .await
            .expect_err("`..`-traversal must reject");
        assert!(
            err.to_string()
                .contains("relative path within the workspace"),
            "error must call out the workspace boundary; got {err}"
        );
    }

    #[tokio::test]
    async fn execute_rejects_symlink_that_resolves_outside_workspace() {
        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        let outside_image = outside.path().join("outside.png");
        std::fs::write(&outside_image, b"not a real png").expect("write outside image");
        let link = workspace.path().join("linked.png");
        if let Err(err) = create_file_symlink(&outside_image, &link) {
            eprintln!("skipping symlink assertion: {err}");
            return;
        }

        let ctx = ToolContext::new(workspace.path().to_path_buf());
        let tool = ImageAnalyzeTool::new(fake_config());
        let err = tool
            .execute(json!({"image_path": "linked.png"}), &ctx)
            .await
            .expect_err("symlink target outside workspace must reject before reading");
        assert!(
            err.to_string().contains("resolve within the workspace"),
            "error must call out the canonical workspace boundary; got {err}"
        );
    }

    fn vision_response_body() -> Value {
        json!({
            "model": "test-vision-model",
            "choices": [
                { "message": { "content": "a red square" } }
            ]
        })
    }

    fn tool_with_base_url(base_url: String) -> ImageAnalyzeTool {
        ImageAnalyzeTool::new(VisionModelConfig {
            model: "test-vision-model".to_string(),
            api_key: Some("test-key".to_string()),
            base_url: Some(base_url),
        })
    }

    fn write_workspace_image(workspace: &std::path::Path) {
        std::fs::write(workspace.join("sample.png"), b"not a real png")
            .expect("write sample image");
    }

    #[tokio::test]
    async fn envelope_bounds_a_stalled_vision_provider() {
        let server = MockServer::start().await;
        // The stalled provider never answers within the test envelope: the
        // upload + non-streaming generation window must be cut off by the
        // envelope, not by a client read timeout (which reqwest turns into
        // a hidden total deadline from `send()`).
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(vision_response_body())
                    .set_delay(Duration::from_secs(30)),
            )
            .mount(&server)
            .await;

        let workspace = tempdir().expect("workspace tempdir");
        write_workspace_image(workspace.path());
        let ctx = ToolContext::new(workspace.path().to_path_buf());
        let tool = tool_with_base_url(server.uri());

        let err = tool
            .execute(json!({"image_path": "sample.png"}), &ctx)
            .await
            .expect_err("a provider that never answers must hit the envelope");
        assert!(
            err.to_string().contains("timed out after"),
            "envelope timeout must be reported as such; got {err}"
        );
    }

    #[tokio::test]
    async fn prompt_answer_within_the_envelope_is_returned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vision_response_body()))
            .mount(&server)
            .await;

        let workspace = tempdir().expect("workspace tempdir");
        write_workspace_image(workspace.path());
        let ctx = ToolContext::new(workspace.path().to_path_buf());
        let tool = tool_with_base_url(server.uri());

        let result = tool
            .execute(
                json!({"image_path": "sample.png", "prompt": "what is this?"}),
                &ctx,
            )
            .await
            .expect("a prompt answer must flow through the envelope");
        let payload: Value =
            serde_json::from_str(&result.content).expect("tool result must carry json");
        assert_eq!(payload["analysis"], "a red square");
    }

    fn create_test_png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut cursor = std::io::Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Png)
            .expect("fixture png encodes");
        cursor.into_inner()
    }

    fn create_test_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([120, 200, 50]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Jpeg)
            .expect("fixture jpeg encodes");
        cursor.into_inner()
    }

    /// Stand-in vision endpoint answering `/chat/completions` the way the
    /// tool expects, so `execute` can be exercised end to end offline.
    async fn mock_vision_endpoint() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "test-vision-model",
                "choices": [{"message": {"content": "a tiny square"}}]
            })))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn execute_reports_real_pixel_dimensions_and_format() {
        let server = mock_vision_endpoint().await;
        let workspace = tempdir().expect("workspace tempdir");
        std::fs::write(
            workspace.path().join("tiny.png"),
            create_test_png(64, 48, [12, 34, 56, 255]),
        )
        .expect("write fixture");
        std::fs::write(workspace.path().join("tiny.jpg"), create_test_jpeg(30, 20))
            .expect("write fixture");

        let ctx = ToolContext::new(workspace.path().to_path_buf());
        let tool = tool_with_base_url(server.uri());

        let cases: [(&str, u32, u32, &str); 2] =
            [("tiny.png", 64, 48, "png"), ("tiny.jpg", 30, 20, "jpeg")];
        for (name, width, height, format) in cases {
            let result = tool
                .execute(json!({"image_path": name}), &ctx)
                .await
                .expect("tool must succeed");
            assert!(result.success);
            let payload: Value = serde_json::from_str(&result.content).expect("json tool content");
            assert_eq!(
                payload.get("width").and_then(Value::as_u64),
                Some(u64::from(width)),
                "{name} must report real pixel width"
            );
            assert_eq!(
                payload.get("height").and_then(Value::as_u64),
                Some(u64::from(height)),
                "{name} must report real pixel height"
            );
            assert_eq!(
                payload.get("format").and_then(Value::as_str),
                Some(format),
                "{name} format must match the mime-derived label"
            );
            let requests = server.received_requests().await.expect("recorded requests");
            let request: Value = requests
                .last()
                .expect("vision request")
                .body_json()
                .unwrap();
            let image_url = request["messages"][0]["content"][1]["image_url"]["url"]
                .as_str()
                .expect("uploaded image URL");
            let uploaded = BASE64
                .decode(
                    image_url
                        .strip_prefix(&format!("data:image/{format};base64,"))
                        .expect("uploaded image MIME type matches metadata"),
                )
                .expect("uploaded image base64");
            assert_eq!(
                ImageAnalyzeTool::image_dimensions(&uploaded, &format!("image/{format}")),
                Some((width, height, format.to_string())),
                "{name} metadata must describe the actual uploaded bytes"
            );
        }
    }

    #[tokio::test]
    async fn execute_omits_dimension_metadata_for_unparsable_bytes() {
        let server = mock_vision_endpoint().await;
        let workspace = tempdir().expect("workspace tempdir");
        std::fs::write(
            workspace.path().join("broken.png"),
            b"definitely not a png header",
        )
        .expect("write fixture");

        let ctx = ToolContext::new(workspace.path().to_path_buf());
        let tool = tool_with_base_url(server.uri());

        let result = tool
            .execute(json!({"image_path": "broken.png"}), &ctx)
            .await
            .expect("metadata failure must not fail the tool");
        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.content).expect("json tool content");
        assert!(payload.get("width").is_none(), "width must be omitted");
        assert!(payload.get("height").is_none(), "height must be omitted");
        assert!(payload.get("format").is_none(), "format must be omitted");
        assert_eq!(
            payload.get("analysis").and_then(Value::as_str),
            Some("a tiny square")
        );
    }

    #[test]
    fn image_dimensions_respects_the_available_bmp_decoder() {
        // Workspace feature unification can enable BMP on some platforms.
        // Report its dimensions when supported; otherwise omit metadata.
        const MINIMAL_BMP: &[u8] = &[
            b'B', b'M', //
            0x3a, 0x00, 0x00,
            0x00, // file size: 14-byte header + 40-byte DIB + 4-byte padded row
            0x00, 0x00, 0x00, 0x00, // reserved
            0x36, 0x00, 0x00, 0x00, // pixel data offset: 54
            0x28, 0x00, 0x00, 0x00, // DIB header size: 40
            0x01, 0x00, 0x00, 0x00, // width: 1
            0x01, 0x00, 0x00, 0x00, // height: 1
            0x01, 0x00, // planes
            0x18, 0x00, // bits per pixel: 24
            0x00, 0x00, 0x00, 0x00, // compression: none
            0x04, 0x00, 0x00, 0x00, // image size: one padded row
            0x00, 0x00, 0x00, 0x00, // x pixels per meter
            0x00, 0x00, 0x00, 0x00, // y pixels per meter
            0x00, 0x00, 0x00, 0x00, // colors used
            0x00, 0x00, 0x00, 0x00, // important colors
            0x00, 0x00, 0x00, 0x00, // single BGR pixel plus 1 pad byte
        ];
        let decoder = image::ImageReader::with_format(
            std::io::Cursor::new(MINIMAL_BMP),
            image::ImageFormat::Bmp,
        )
        .into_decoder();
        match decoder {
            Ok(decoder) => {
                assert_eq!(image::ImageDecoder::dimensions(&decoder), (1, 1));
                assert_eq!(
                    ImageAnalyzeTool::image_dimensions(MINIMAL_BMP, "image/bmp"),
                    Some((1, 1, "bmp".to_string()))
                );
            }
            Err(image::ImageError::Unsupported(error)) => {
                assert!(matches!(
                    error.kind(),
                    image::error::UnsupportedErrorKind::Format(
                        image::error::ImageFormatHint::Exact(image::ImageFormat::Bmp)
                    )
                ));
                assert_eq!(
                    ImageAnalyzeTool::image_dimensions(MINIMAL_BMP, "image/bmp"),
                    None
                );
            }
            Err(error) => panic!("invalid BMP fixture: {error}"),
        }
    }
}
