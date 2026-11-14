use axum::Json;
use axum::extract::State;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, RuntimeApiState};
use codewhale_engine::deepseek_media::DEEPSEEK_TTS_MAX_CHARS;
use codewhale_engine::voice as voice_core;
use voice_core::{DictateError, DictateMode, UPLOAD_AUDIO_MAX_BYTES};

static DICTATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) async fn voice_status(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    let (asr_kind, asr_model, upload_available, tts_available, tts_voices) = {
        let config = state.config.read();
        let (asr_kind, asr_model) = voice_core::asr_choice(&config);
        let upload_available = voice_core::upload_available(&config);
        let tts_available = voice_core::tts_available(&config);
        let tts_voices = voice_core::tts_voices(&config);
        (
            asr_kind,
            asr_model,
            upload_available,
            tts_available,
            tts_voices,
        )
    };
    Ok(Json(json!({
        "available": voice_core::is_available(),
        "upload_available": upload_available,
        "tts_available": tts_available,
        "recorder": voice_core::recorder_command(),
        "asr": { "kind": asr_kind, "model": asr_model },
        "modes": ["insert", "send", "control"],
        "send_phrases": voice_core::SEND_PHRASES,
        "max_record_seconds": voice_core::MAX_RECORD_SECS,
        "max_upload_bytes": UPLOAD_AUDIO_MAX_BYTES,
        "tts_voices": tts_voices,
    })))
}

fn dictate_failure(error: DictateError) -> Json<Value> {
    Json(json!({
        "ok": false,
        "reason": error.reason(),
        "message": error.to_string(),
    }))
}

fn dictate_success(outcome: voice_core::DictationOutcome) -> Json<Value> {
    Json(json!({
        "ok": true,
        "text": outcome.text,
        "send": outcome.send,
        "assisted": outcome.assisted,
        "asr": { "kind": outcome.asr_kind, "model": outcome.asr_model },
    }))
}

async fn dictate(state: &RuntimeApiState, mode: DictateMode) -> Result<Json<Value>, ApiError> {
    let config = state.config.read().clone();
    let _permit = DICTATE_LOCK.lock().await;
    match voice_core::dictate_once(&config, mode).await {
        Ok(outcome) => Ok(dictate_success(outcome)),
        Err(error) => Ok(dictate_failure(error)),
    }
}

pub(super) async fn voice_dictate(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    dictate(&state, DictateMode::Insert).await
}

pub(super) async fn voice_send(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    dictate(&state, DictateMode::Send).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceControlRequest {
    #[serde(default)]
    composer: String,
}

pub(super) async fn voice_control(
    State(state): State<RuntimeApiState>,
    body: Option<Json<VoiceControlRequest>>,
) -> Result<Json<Value>, ApiError> {
    let composer = body
        .map(|Json(request)| request.composer)
        .unwrap_or_default();
    dictate(&state, DictateMode::Control(composer)).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceTranscribeRequest {
    audio_base64: String,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    composer: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

pub(super) async fn voice_transcribe(
    State(state): State<RuntimeApiState>,
    Json(request): Json<VoiceTranscribeRequest>,
) -> Result<Json<Value>, ApiError> {
    let max_encoded_len = (UPLOAD_AUDIO_MAX_BYTES / 3 + 2) * 4 + 16;
    if request.audio_base64.len() > max_encoded_len {
        return Ok(dictate_failure(DictateError::InvalidInput(format!(
            "encoded audio exceeds maximum size for {UPLOAD_AUDIO_MAX_BYTES} byte upload"
        ))));
    }
    let Ok(audio_bytes) =
        base64::engine::general_purpose::STANDARD.decode(request.audio_base64.trim())
    else {
        return Ok(dictate_failure(DictateError::InvalidInput(
            "audio_base64 is not valid base64".to_string(),
        )));
    };
    let mode = match request.mode.as_deref().unwrap_or("insert").trim() {
        "" | "insert" => DictateMode::Insert,
        "send" => DictateMode::Send,
        "control" => DictateMode::Control(request.composer.unwrap_or_default()),
        other => {
            return Ok(dictate_failure(DictateError::InvalidInput(format!(
                "unsupported voice mode '{other}'"
            ))));
        }
    };
    let mime_type = request.mime_type.as_deref().unwrap_or("audio/wav");
    let config = state.config.read().clone();
    match voice_core::transcribe_uploaded_audio(
        &config,
        &audio_bytes,
        mime_type,
        mode,
        request.language.as_deref(),
    )
    .await
    {
        Ok(outcome) => Ok(dictate_success(outcome)),
        Err(error) => Ok(dictate_failure(error)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceTtsRequest {
    text: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    voice_id: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    filter_markdown: Option<bool>,
}

fn tts_failure(reason: &str, message: impl Into<String>) -> Json<Value> {
    Json(json!({
        "ok": false,
        "reason": reason,
        "message": message.into(),
    }))
}

pub(super) async fn voice_tts(
    State(state): State<RuntimeApiState>,
    Json(request): Json<VoiceTtsRequest>,
) -> Result<Json<Value>, ApiError> {
    if request.text.trim().is_empty() {
        return Ok(tts_failure("invalid_input", "speech text cannot be empty"));
    }
    if request.text.chars().count() > DEEPSEEK_TTS_MAX_CHARS {
        return Ok(tts_failure(
            "invalid_input",
            format!("speech text exceeds maximum of {DEEPSEEK_TTS_MAX_CHARS} characters"),
        ));
    }
    let voice = request.voice.as_deref().or(request.voice_id.as_deref());
    let filter_markdown = request.filter_markdown.unwrap_or(true);
    let config = state.config.read().clone();
    match voice_core::synthesize_voice_tts_with_options(
        &config,
        &request.text,
        voice,
        request.format.as_deref(),
        filter_markdown,
    )
    .await
    {
        Ok(outcome) => Ok(Json(json!({
            "ok": true,
            "mime_type": outcome.content_type,
            "audio_base64": base64::engine::general_purpose::STANDARD.encode(&outcome.audio_bytes),
            "voice_id": outcome.voice,
            "provider": outcome.provider,
            "model": outcome.model,
            "format": outcome.format,
        }))),
        Err(err) => Ok(tts_failure(err.reason(), err.to_string())),
    }
}
