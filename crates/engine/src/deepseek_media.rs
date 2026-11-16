use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use codewhale_config::ProviderKind;
use codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL;
use codewhale_runtime::config::Config;

pub const DEEPSEEK_ASR_MODEL: &str = "deepseek-asr";
pub const DEEPSEEK_TTS_MODEL: &str = "deepseek-tts";
pub const DEEPSEEK_OCR_MODEL: &str = "deepseek-ocr";
pub const DEEPSEEK_ASR_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const DEEPSEEK_OCR_MAX_BYTES: usize = 10 * 1024 * 1024;
pub const DEEPSEEK_TTS_MAX_CHARS: usize = 10_000;
pub const DEEPSEEK_TTS_VOICES: &[&str] = &["stella", "tide"];
const DEEPSEEK_ASR_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
const DEEPSEEK_TTS_RESPONSE_MAX_BYTES: usize = 32 * 1024 * 1024;
const DEEPSEEK_OCR_RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DeepseekAsrResponse {
    #[serde(default)]
    pub request_id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepseekTtsResponse {
    pub audio_bytes: Vec<u8>,
    pub content_type: String,
    pub format: String,
    pub model: String,
    pub voice: String,
}

#[derive(Debug, Deserialize)]
struct DeepseekOcrPage {
    index: usize,
    #[serde(default)]
    text: String,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeepseekOcrPayload {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    page_count: Option<usize>,
    #[serde(default)]
    pages: Vec<DeepseekOcrPage>,
}

#[derive(Serialize)]
struct DeepseekTtsPayload<'a> {
    text: &'a str,
    voice_id: &'a str,
    format: &'a str,
    filter_markdown: bool,
}

fn strip_root_suffix(url: &str) -> String {
    let mut current = url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    loop {
        let mut stripped = false;
        for suffix in ["/anthropic", "/beta", "/v1"] {
            if let Some(prefix) = current.strip_suffix(suffix) {
                current = prefix.trim_end_matches('/');
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    current.to_string()
}

pub fn deepseek_root_url(base_url: &str) -> String {
    let stripped = strip_root_suffix(base_url);
    if stripped.is_empty() {
        strip_root_suffix(DEFAULT_DEEPSEEK_BASE_URL)
    } else {
        stripped
    }
}

pub fn deepseek_tools_url(base_url: &str, tool: &str) -> String {
    format!(
        "{}/v1/tools/{}",
        deepseek_root_url(base_url),
        tool.trim().trim_start_matches('/')
    )
}

pub fn normalize_deepseek_voice_id(voice: Option<&str>) -> Result<&'static str, String> {
    let normalized = voice.unwrap_or("").trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "default" | "mimo_default" | "female_calm" | "stella" => Ok("stella"),
        "tide" => Ok("tide"),
        _ => Err(format!(
            "unsupported DeepSeek TTS voice '{}' (allowed: {})",
            voice.unwrap_or_default().trim(),
            DEEPSEEK_TTS_VOICES.join(", ")
        )),
    }
}

pub fn normalize_deepseek_tts_format(format: &str) -> Result<(&'static str, &'static str), String> {
    match format.trim().to_ascii_lowercase().as_str() {
        "" | "wav" => Ok(("wav", "audio/wav")),
        "pcm" | "pcm16" => Ok(("pcm", "audio/pcm")),
        other => Err(format!(
            "unsupported DeepSeek TTS format '{other}' (allowed: wav, pcm)"
        )),
    }
}

pub fn normalize_deepseek_asr_mime(mime_type: &str) -> Result<&'static str, String> {
    let base = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "" | "audio/wav" | "audio/x-wav" | "audio/wave" => Ok("audio/wav"),
        "audio/ogg" => Ok("audio/ogg"),
        other => Err(format!(
            "unsupported DeepSeek ASR mime type '{other}' (allowed: audio/wav, audio/ogg)"
        )),
    }
}

pub fn normalize_deepseek_ocr_mime(mime_type: &str) -> Result<&'static str, String> {
    let base = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "image/png" => Ok("image/png"),
        "image/jpeg" | "image/jpg" => Ok("image/jpeg"),
        "image/tiff" | "image/tif" => Ok("image/tiff"),
        "application/pdf" => Ok("application/pdf"),
        other => Err(format!(
            "unsupported DeepSeek OCR mime type '{other}' (allowed: image/png, image/jpeg, image/tiff, application/pdf)"
        )),
    }
}

pub fn resolve_deepseek_config_auth(config: &Config) -> Option<(String, String)> {
    let read = || {
        if let Ok(identity) = config.active_provider_identity()
            && matches!(
                identity.provider,
                ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
            )
        {
            let api_key = config
                .active_route_api_key_read_only()
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())?;
            return Some((deepseek_root_url(&config.active_route_base_url()), api_key));
        }

        if codewhale_runtime::config::cli_api_key_source().as_deref() == Some("cli")
            || codewhale_runtime::config::explicit_cli_api_key_override().is_some()
        {
            return None;
        }

        for candidate in [ProviderKind::Deepseek, ProviderKind::DeepseekAnthropic] {
            let mut scoped = config.clone();
            scoped.provider = Some(candidate.as_str().to_string());
            let Ok(identity) = scoped.active_provider_identity() else {
                continue;
            };
            let Some(api_key) = scoped
                .active_route_api_key_read_only()
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let base_url = scoped.base_url_for_route(&identity);
            return Some((deepseek_root_url(&base_url), api_key));
        }
        None
    };
    #[cfg(any(test, feature = "test-support"))]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        read()
    }
}

pub fn deepseek_ocr_explicitly_enabled() -> bool {
    let read = || {
        std::env::var("CODEWHALE_DEEPSEEK_OCR")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
    };
    #[cfg(any(test, feature = "test-support"))]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        read()
    }
}

pub fn resolve_deepseek_ocr_auth() -> Option<(String, String)> {
    let read = || {
        if !deepseek_ocr_explicitly_enabled() {
            return None;
        }
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .map(|value| codewhale_secrets::normalize_api_key(&value))
            .filter(|value| !value.trim().is_empty())?;
        let base_url = std::env::var("DEEPSEEK_BASE_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE_URL.to_string());
        Some((deepseek_root_url(&base_url), api_key))
    };
    #[cfg(any(test, feature = "test-support"))]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        read()
    }
}

fn sanitize_multipart_header_value(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| match ch {
            '\r' | '\n' | '"' | '\\' | '/' => '_',
            c if c.is_control() => '_',
            _ => ch,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_multipart_body(
    fields: &[(&str, &str)],
    file_field: &str,
    file_name: &str,
    file_mime: &str,
    file_bytes: &[u8],
) -> (String, Vec<u8>) {
    let boundary = format!("----codewhale-boundary-{}", uuid::Uuid::new_v4().simple());
    let safe_name = sanitize_multipart_header_value(file_name, "upload.bin");
    let mut body = Vec::with_capacity(file_bytes.len() + fields.len() * 128 + 256);

    for (name, value) in fields {
        let safe_field = sanitize_multipart_header_value(name, "field");
        body.extend_from_slice(b"--");
        body.extend_from_slice(boundary.as_bytes());
        body.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
        body.extend_from_slice(safe_field.as_bytes());
        body.extend_from_slice(b"\"\r\n\r\n");
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(b"--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
    body.extend_from_slice(file_field.as_bytes());
    body.extend_from_slice(b"\"; filename=\"");
    body.extend_from_slice(safe_name.as_bytes());
    body.extend_from_slice(b"\"\r\nContent-Type: ");
    body.extend_from_slice(file_mime.as_bytes());
    body.extend_from_slice(b"\r\n\r\n");
    body.extend_from_slice(file_bytes);
    body.extend_from_slice(b"\r\n--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"--\r\n");

    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn standalone_http_client() -> Result<reqwest::Client, String> {
    crate::platform_client_builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|err| {
            format!(
                "failed to build DeepSeek HTTP client: {}",
                err.without_url()
            )
        })
}

fn redact_secret(text: &str, api_key: &str) -> String {
    if api_key.trim().is_empty() {
        text.to_string()
    } else {
        text.replace(api_key, "[redacted]")
    }
}

fn truncate_utf8_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

async fn read_bounded_response_bytes(
    response: reqwest::Response,
    max_bytes: usize,
    label: &str,
    api_key: &str,
) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;

    if let Some(len) = response.content_length()
        && len > max_bytes as u64
    {
        return Err(format!(
            "{label} response exceeded maximum of {max_bytes} bytes"
        ));
    }
    let initial = response
        .content_length()
        .and_then(|len| usize::try_from(len).ok())
        .map(|len| len.min(max_bytes))
        .unwrap_or(0);
    let mut stream = response.bytes_stream();
    let mut buf = Vec::with_capacity(initial.min(64 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| {
            redact_secret(
                &format!("failed to read {label} response: {}", err.without_url()),
                api_key,
            )
        })?;
        if buf.len().saturating_add(chunk.len()) > max_bytes {
            return Err(format!(
                "{label} response exceeded maximum of {max_bytes} bytes"
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn format_error_response(status: reqwest::StatusCode, body: &str, api_key: &str) -> String {
    let raw_detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string());
    let redacted = redact_secret(raw_detail.trim(), api_key);
    let summary = if redacted.len() > 512 {
        format!("{}...", truncate_utf8_prefix(&redacted, 512))
    } else {
        redacted
    };
    if summary.is_empty() {
        format!("DeepSeek Tools API returned HTTP {}", status.as_u16())
    } else {
        format!(
            "DeepSeek Tools API returned HTTP {}: {summary}",
            status.as_u16()
        )
    }
}

pub fn validate_deepseek_tts_inputs(
    model: Option<&str>,
    text: &str,
    instruction: Option<&str>,
    voice: Option<&str>,
    format: &str,
) -> Result<(String, &'static str, &'static str, &'static str), String> {
    let trimmed_text = text.trim();
    if trimmed_text.is_empty() {
        return Err("speech text cannot be empty".to_string());
    }
    if text.chars().count() > DEEPSEEK_TTS_MAX_CHARS {
        return Err(format!(
            "DeepSeek TTS text exceeds maximum of {DEEPSEEK_TTS_MAX_CHARS} characters"
        ));
    }
    if instruction.is_some_and(|value| !value.trim().is_empty()) {
        return Err(
            "DeepSeek TTS does not support voice instructions or voice design prompts".to_string(),
        );
    }
    if let Some(raw_voice) = voice
        && raw_voice.trim_start().starts_with("data:")
    {
        return Err("DeepSeek TTS does not support voice cloning data URIs".to_string());
    }
    let normalized_model = match model.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "" | "auto" | "deepseek-tts" | "mimo-v2.5-tts" | "mimo-v2-tts" => DEEPSEEK_TTS_MODEL,
        other if other.contains("voiceclone") || other.contains("voicedesign") => {
            return Err(format!(
                "DeepSeek TTS does not support model '{other}'; use '{DEEPSEEK_TTS_MODEL}'"
            ));
        }
        other => {
            return Err(format!(
                "unsupported DeepSeek TTS model '{other}'; use '{DEEPSEEK_TTS_MODEL}'"
            ));
        }
    };
    let voice_id = normalize_deepseek_voice_id(voice)?;
    let (wire_format, content_type) = normalize_deepseek_tts_format(format)?;
    Ok((
        normalized_model.to_string(),
        voice_id,
        wire_format,
        content_type,
    ))
}

pub async fn transcribe_deepseek_bytes(
    base_url: &str,
    api_key: &str,
    audio_bytes: &[u8],
    file_name: &str,
    mime_type: &str,
    language: Option<&str>,
) -> Result<DeepseekAsrResponse, String> {
    if api_key.trim().is_empty() {
        return Err("DeepSeek ASR requires a configured API key".to_string());
    }
    if audio_bytes.is_empty() {
        return Err("DeepSeek ASR audio payload cannot be empty".to_string());
    }
    if audio_bytes.len() > DEEPSEEK_ASR_MAX_BYTES {
        return Err(format!(
            "DeepSeek ASR audio payload exceeds maximum of {DEEPSEEK_ASR_MAX_BYTES} bytes"
        ));
    }
    let canonical_mime = normalize_deepseek_asr_mime(mime_type)?;
    let default_name = if canonical_mime == "audio/ogg" {
        "voice.ogg"
    } else {
        "voice.wav"
    };
    let chosen_name = if file_name.trim().is_empty() {
        default_name
    } else {
        file_name
    };
    let mut fields = Vec::new();
    let cleaned_language = language
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(ref lang) = cleaned_language {
        fields.push(("language", lang.as_str()));
    }
    let (content_type, body) =
        build_multipart_body(&fields, "file", chosen_name, canonical_mime, audio_bytes);
    let url = deepseek_tools_url(base_url, "asr");
    let client = standalone_http_client()?;
    let response = client
        .post(&url)
        .bearer_auth(api_key)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await
        .map_err(|err| {
            redact_secret(
                &format!("DeepSeek ASR request failed: {}", err.without_url()),
                api_key,
            )
        })?;

    let status = response.status();
    if !status.is_success() {
        let error_text = crate::client::wire::bounded_error_text(
            response,
            crate::client::wire::ERROR_BODY_MAX_BYTES,
        )
        .await;
        return Err(format_error_response(status, &error_text, api_key));
    }
    let response_bytes = read_bounded_response_bytes(
        response,
        DEEPSEEK_ASR_RESPONSE_MAX_BYTES,
        "DeepSeek ASR",
        api_key,
    )
    .await?;
    let response_text = String::from_utf8(response_bytes)
        .map_err(|err| format!("DeepSeek ASR response was not valid UTF-8: {err}"))?;
    serde_json::from_str::<DeepseekAsrResponse>(&response_text)
        .map_err(|err| format!("failed to parse DeepSeek ASR response JSON: {err}"))
}

pub async fn synthesize_deepseek_with_client(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    request: crate::client::SpeechSynthesisRequest,
) -> anyhow::Result<crate::client::SpeechSynthesisResponse> {
    synthesize_deepseek_with_client_and_options(http_client, base_url, api_key, request, true).await
}

pub async fn synthesize_deepseek_with_client_and_options(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    request: crate::client::SpeechSynthesisRequest,
    filter_markdown: bool,
) -> anyhow::Result<crate::client::SpeechSynthesisResponse> {
    if api_key.trim().is_empty() {
        anyhow::bail!("DeepSeek TTS requires a configured API key");
    }
    let (model, voice_id, wire_format, _content_type) = validate_deepseek_tts_inputs(
        Some(request.model.as_str()),
        &request.text,
        request.instruction.as_deref(),
        request.voice.as_deref(),
        &request.audio_format,
    )
    .map_err(anyhow::Error::msg)?;
    let url = deepseek_tools_url(base_url, "tts");
    let payload = DeepseekTtsPayload {
        text: &request.text,
        voice_id,
        format: wire_format,
        filter_markdown,
    };
    let response = http_client
        .post(&url)
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|err| {
            anyhow::anyhow!(redact_secret(
                &format!("DeepSeek TTS request failed: {}", err.without_url()),
                api_key
            ))
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = crate::client::wire::bounded_error_text(
            response,
            crate::client::wire::ERROR_BODY_MAX_BYTES,
        )
        .await;
        anyhow::bail!("{}", format_error_response(status, &body, api_key));
    }
    let audio_bytes = read_bounded_response_bytes(
        response,
        DEEPSEEK_TTS_RESPONSE_MAX_BYTES,
        "DeepSeek TTS",
        api_key,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    if audio_bytes.is_empty() {
        anyhow::bail!("DeepSeek TTS response audio body was empty");
    }
    Ok(crate::client::SpeechSynthesisResponse {
        model,
        audio_format: wire_format.to_string(),
        audio_bytes,
        transcript: None,
        voice: Some(voice_id.to_string()),
    })
}

pub async fn synthesize_deepseek_bytes(
    base_url: &str,
    api_key: &str,
    text: &str,
    voice: Option<&str>,
    format: &str,
) -> Result<DeepseekTtsResponse, String> {
    synthesize_deepseek_bytes_with_options(base_url, api_key, text, voice, format, true).await
}

pub async fn synthesize_deepseek_bytes_with_options(
    base_url: &str,
    api_key: &str,
    text: &str,
    voice: Option<&str>,
    format: &str,
    filter_markdown: bool,
) -> Result<DeepseekTtsResponse, String> {
    let client = standalone_http_client()?;
    let (model, voice_id, wire_format, content_type) =
        validate_deepseek_tts_inputs(Some(DEEPSEEK_TTS_MODEL), text, None, voice, format)?;
    let response = synthesize_deepseek_with_client_and_options(
        &client,
        base_url,
        api_key,
        crate::client::SpeechSynthesisRequest {
            model,
            text: text.to_string(),
            instruction: None,
            audio_format: wire_format.to_string(),
            voice: Some(voice_id.to_string()),
        },
        filter_markdown,
    )
    .await
    .map_err(|err| err.to_string())?;
    Ok(DeepseekTtsResponse {
        audio_bytes: response.audio_bytes,
        content_type: content_type.to_string(),
        format: response.audio_format,
        model: response.model,
        voice: response.voice.unwrap_or_else(|| voice_id.to_string()),
    })
}

pub fn ocr_mime_for_path(_path: &Path, bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.len() >= 12 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.len() >= 12 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok("image/jpeg");
    }
    if bytes.len() >= 12 && (bytes.starts_with(b"II*\x00") || bytes.starts_with(b"MM\x00*")) {
        return Ok("image/tiff");
    }
    if bytes.len() >= 5 && bytes.starts_with(b"%PDF-") {
        return Ok("application/pdf");
    }
    Err("unsupported file signature for DeepSeek OCR (allowed: png, jpeg, tiff, pdf)".to_string())
}

pub async fn ocr_deepseek_bytes(
    base_url: &str,
    api_key: &str,
    file_bytes: &[u8],
    file_name: &str,
    mime_type: &str,
) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("DeepSeek OCR requires a configured API key".to_string());
    }
    if file_bytes.is_empty() {
        return Err("DeepSeek OCR input file cannot be empty".to_string());
    }
    if file_bytes.len() > DEEPSEEK_OCR_MAX_BYTES {
        return Err(format!(
            "DeepSeek OCR input exceeds maximum of {DEEPSEEK_OCR_MAX_BYTES} bytes"
        ));
    }
    let canonical_mime = normalize_deepseek_ocr_mime(mime_type)?;
    let fields = [("purpose", "text_recognition")];
    let (content_type, body) = build_multipart_body(
        &fields,
        "file",
        if file_name.trim().is_empty() {
            "document"
        } else {
            file_name
        },
        canonical_mime,
        file_bytes,
    );
    let url = deepseek_tools_url(base_url, "ocr");
    let client = standalone_http_client()?;
    let response = client
        .post(&url)
        .bearer_auth(api_key)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await
        .map_err(|err| {
            redact_secret(
                &format!("DeepSeek OCR request failed: {}", err.without_url()),
                api_key,
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let error_text = crate::client::wire::bounded_error_text(
            response,
            crate::client::wire::ERROR_BODY_MAX_BYTES,
        )
        .await;
        return Err(format_error_response(status, &error_text, api_key));
    }
    let raw_bytes = read_bounded_response_bytes(
        response,
        DEEPSEEK_OCR_RESPONSE_MAX_BYTES,
        "DeepSeek OCR",
        api_key,
    )
    .await?;
    let raw = String::from_utf8(raw_bytes)
        .map_err(|err| format!("DeepSeek OCR response was not valid UTF-8: {err}"))?;
    let mut payload = serde_json::from_str::<DeepseekOcrPayload>(raw.trim())
        .map_err(|err| format!("failed to parse DeepSeek OCR response JSON: {err}"))?;
    let _ = (&payload.id, &payload.object, payload.page_count);
    payload.pages.sort_by_key(|page| page.index);
    let text = payload
        .pages
        .into_iter()
        .map(|page| {
            let _ = page.finish_reason;
            page.text.trim().to_string()
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn root_url_strips_suffixes_and_falls_back_to_default() {
        assert_eq!(
            deepseek_root_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            deepseek_root_url("https://api.deepseek.com/anthropic"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            deepseek_root_url("https://api.deepseek.com/beta"),
            "https://api.deepseek.com"
        );
        assert_eq!(deepseek_root_url("   "), "https://api.deepseek.com");
        assert_eq!(
            deepseek_root_url("https://api.deepseek.com/anthropic/v1"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            deepseek_root_url("https://api.deepseek.com/beta/v1?foo=bar"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            deepseek_tools_url("https://api.deepseek.com/v1", "asr"),
            "https://api.deepseek.com/v1/tools/asr"
        );
    }

    #[test]
    fn voice_and_format_normalization() {
        assert_eq!(normalize_deepseek_voice_id(None).unwrap(), "stella");
        assert_eq!(
            normalize_deepseek_voice_id(Some("mimo_default")).unwrap(),
            "stella"
        );
        assert_eq!(
            normalize_deepseek_voice_id(Some("female_calm")).unwrap(),
            "stella"
        );
        assert_eq!(normalize_deepseek_voice_id(Some("tide")).unwrap(), "tide");
        assert!(normalize_deepseek_voice_id(Some("unknown")).is_err());

        assert_eq!(
            normalize_deepseek_tts_format("wav").unwrap(),
            ("wav", "audio/wav")
        );
        assert_eq!(
            normalize_deepseek_tts_format("pcm16").unwrap(),
            ("pcm", "audio/pcm")
        );
        assert!(normalize_deepseek_tts_format("mp3").is_err());
    }

    #[tokio::test]
    async fn asr_wiremock_sends_multipart_and_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/asr"))
            .and(header("authorization", "Bearer ds-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "request_id": "req-1",
                "text": "hello deepseek",
                "duration_ms": 1240
            })))
            .mount(&server)
            .await;

        let res = transcribe_deepseek_bytes(
            &format!("{}/v1", server.uri()),
            "ds-secret",
            b"RIFFdata",
            "voice.wav",
            "audio/x-wav",
            Some("en"),
        )
        .await
        .expect("asr succeeds");

        assert_eq!(res.text, "hello deepseek");
        assert_eq!(res.duration_ms, Some(1240));
        let received = server.received_requests().await.expect("requests");
        let raw = String::from_utf8_lossy(&received[0].body);
        assert!(raw.contains("name=\"language\"\r\n\r\nen\r\n"));
        assert!(raw.contains("name=\"file\"; filename=\"voice.wav\""));
        assert!(raw.contains("Content-Type: audio/wav"));
    }

    #[tokio::test]
    async fn tts_wiremock_sends_json_and_rejects_clone_or_overflow() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/tts"))
            .and(header("authorization", "Bearer ds-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"RIFF_WAV".to_vec()))
            .mount(&server)
            .await;

        let out = synthesize_deepseek_bytes(
            &format!("{}/anthropic", server.uri()),
            "ds-secret",
            "Read **this** aloud",
            Some("tide"),
            "wav",
        )
        .await
        .expect("tts succeeds");
        assert_eq!(out.audio_bytes, b"RIFF_WAV");
        assert_eq!(out.content_type, "audio/wav");
        assert_eq!(out.voice, "tide");

        let received = server.received_requests().await.expect("requests");
        let sent: Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(
            sent,
            serde_json::json!({
                "text": "Read **this** aloud",
                "voice_id": "tide",
                "format": "wav",
                "filter_markdown": true
            })
        );

        assert!(
            validate_deepseek_tts_inputs(
                Some("deepseek-tts"),
                "hi",
                None,
                Some("data:audio/wav;base64,AA=="),
                "wav"
            )
            .is_err()
        );
        let too_long = "a".repeat(DEEPSEEK_TTS_MAX_CHARS + 1);
        assert!(validate_deepseek_tts_inputs(None, &too_long, None, None, "wav").is_err());
    }

    #[tokio::test]
    async fn ocr_multipart_puts_purpose_before_file_and_strips_keepalives() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/ocr"))
            .and(header("authorization", "Bearer ds-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "\n\n\n{\"id\":\"ocr-1\",\"object\":\"ocr\",\"page_count\":2,\"pages\":[{\"index\":1,\"text\":\"Page two\",\"finish_reason\":\"stop\"},{\"index\":0,\"text\":\"Page one\",\"finish_reason\":\"stop\"}]}",
            ))
            .mount(&server)
            .await;

        let text = ocr_deepseek_bytes(
            &server.uri(),
            "ds-secret",
            b"\x89PNG\r\n\x1a\npayload",
            "shot.png",
            "image/png",
        )
        .await
        .expect("ocr succeeds");
        assert_eq!(text, "Page one\n\nPage two");

        let received = server.received_requests().await.expect("requests");
        let raw = String::from_utf8_lossy(&received[0].body);
        let purpose_pos = raw
            .find("name=\"purpose\"\r\n\r\ntext_recognition")
            .unwrap();
        let file_pos = raw.find("name=\"file\"; filename=\"shot.png\"").unwrap();
        assert!(purpose_pos < file_pos);
        assert!(
            ocr_mime_for_path(Path::new("notes.png"), b"plain text pretending to be png").is_err()
        );
        assert!(ocr_mime_for_path(Path::new("short.png"), b"\x89PNG\r\n\x1a\n123").is_err());
        assert!(ocr_mime_for_path(Path::new("fake.pdf"), b"not a pdf payload").is_err());
        assert_eq!(
            ocr_mime_for_path(Path::new("doc.pdf"), b"%PDF-1.7\nbody").unwrap(),
            "application/pdf"
        );
        assert_eq!(
            ocr_mime_for_path(Path::new("img.png"), b"\x89PNG\r\n\x1a\n0123").unwrap(),
            "image/png"
        );
    }

    #[test]
    fn ocr_opt_in_requires_flag_and_api_key() {
        let _lock = lock_test_env();
        let _flag_off = EnvVarGuard::remove("CODEWHALE_DEEPSEEK_OCR");
        let _key = EnvVarGuard::set("DEEPSEEK_API_KEY", "ds-ocr-key");
        let _base = EnvVarGuard::set("DEEPSEEK_BASE_URL", "https://ds.example/v1");
        assert!(resolve_deepseek_ocr_auth().is_none());

        let _flag_on = EnvVarGuard::set("CODEWHALE_DEEPSEEK_OCR", "1");
        assert_eq!(
            resolve_deepseek_ocr_auth(),
            Some(("https://ds.example".to_string(), "ds-ocr-key".to_string()))
        );
    }

    #[test]
    fn error_formatting_redacts_before_multibyte_utf8_truncation() {
        let secret = "ds-secret-key-12345";
        let straddling_body = format!("{}{secret}{}", "é".repeat(250), "错误".repeat(120));
        let formatted =
            format_error_response(reqwest::StatusCode::BAD_REQUEST, &straddling_body, secret);
        assert!(!formatted.contains(secret), "{formatted}");
        assert!(formatted.contains("[redacted]"), "{formatted}");
        assert!(formatted.ends_with("..."), "{formatted}");
    }

    #[test]
    fn config_auth_isolates_foreign_cli_api_key_and_supports_deepseek_anthropic() {
        let _lock = lock_test_env();
        let _ds_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _ds_base = EnvVarGuard::remove("DEEPSEEK_BASE_URL");
        let _cw_base = EnvVarGuard::remove("CODEWHALE_BASE_URL");

        let openai_with_ds: Config = toml::from_str(
            "provider = \"openai\"\n\n[providers.deepseek]\napi_key = \"ds-config-key\"\nbase_url = \"https://api.deepseek.com/v1\"\n",
        )
        .expect("openai+deepseek config parses");
        {
            let _cli_source = EnvVarGuard::set(codewhale_config::CLI_API_KEY_SOURCE_ENV, "cli");
            let _legacy_cli_source =
                EnvVarGuard::remove(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV);
            let _cli_key = EnvVarGuard::set(codewhale_config::CLI_API_KEY_ENV, "sk-foreign-cli");
            assert_eq!(resolve_deepseek_config_auth(&openai_with_ds), None);
        }

        let _cli_source = EnvVarGuard::remove(codewhale_config::CLI_API_KEY_SOURCE_ENV);
        let _legacy_cli_source =
            EnvVarGuard::remove(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV);
        let _cli_key = EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let anthropic_only: Config = toml::from_str(
            "provider = \"openai\"\n\n[providers.deepseek-anthropic]\napi_key = \"ds-ant\"\nbase_url = \"https://api.deepseek.com/anthropic/v1\"\n",
        )
        .expect("deepseek-anthropic config parses");
        assert_eq!(
            resolve_deepseek_config_auth(&anthropic_only),
            Some(("https://api.deepseek.com".to_string(), "ds-ant".to_string()))
        );
    }

    #[tokio::test]
    async fn standalone_client_refuses_http_redirects() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/tts"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", &format!("{}/elsewhere", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/elsewhere"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SHOULD_NOT_FOLLOW".to_vec()))
            .mount(&server)
            .await;

        let err = synthesize_deepseek_bytes(&server.uri(), "ds-secret", "Hello", None, "wav")
            .await
            .expect_err("redirects must fail closed");
        assert!(err.contains("HTTP 302"), "{err}");
        let received = server.received_requests().await.expect("requests");
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].url.path(), "/v1/tools/tts");
    }
}
