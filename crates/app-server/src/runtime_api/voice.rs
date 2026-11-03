//! Voice/dictation HTTP for native clients (APPS-98).
//!
//! The runtime owns the host microphone and the ASR dispatch — recording and
//! transcription are the same implementation the TUI's `/voice` commands run,
//! exposed headlessly so a desktop client gets text back instead of driving a
//! terminal. There is deliberately no second speech stack and no audio upload
//! path: dictate means "record on the host this runtime runs on".
//!
//! Fail-closed as data: no recorder, no speech, missing provider auth, or an
//! ASR failure all answer `200` with `ok: false` + a machine-readable
//! `reason` — a desktop client reads `GET /v1/voice` first and disables its
//! dictation affordance when `available` is false.
//!
//! Routes:
//!   GET  /v1/voice          — capability: recorder, ASR selection, send phrases
//!   POST /v1/voice/dictate  — record + transcribe → insert text
//!   POST /v1/voice/send     — record + transcribe + send-suffix detection
//!   POST /v1/voice/control  — record + assisted dictation; body {"composer"}

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, RuntimeApiState};
use codewhale_engine::voice as voice_core;
use voice_core::{DictateError, DictateMode};

/// One mic per host — serialize captures so concurrent dictate requests get
/// an honest "no speech" for the loser rather than fighting over the device.
static DICTATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `GET /v1/voice` — what this host can do: whether a recorder exists, which
/// ASR backend the live config resolves to, and the send-suffix contract.
pub(super) async fn voice_status(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    let (asr_kind, asr_model) = {
        let config = state.config.read();
        voice_core::asr_choice(&config)
    };
    Ok(Json(json!({
        "available": voice_core::is_available(),
        "recorder": voice_core::recorder_command(),
        "asr": { "kind": asr_kind, "model": asr_model },
        "modes": ["insert", "send", "control"],
        "send_phrases": voice_core::SEND_PHRASES,
        "max_record_seconds": voice_core::MAX_RECORD_SECS,
    })))
}

fn dictate_failure(error: DictateError) -> Json<Value> {
    Json(json!({
        "ok": false,
        "reason": error.reason(),
        "message": error.to_string(),
    }))
}

async fn dictate(state: &RuntimeApiState, mode: DictateMode) -> Result<Json<Value>, ApiError> {
    // Clone before recording: holding the config lock across a ~10s capture
    // would stall unrelated config writes for the duration.
    let config = state.config.read().clone();
    let _permit = DICTATE_LOCK.lock().await;
    match voice_core::dictate_once(&config, mode).await {
        Ok(outcome) => Ok(Json(json!({
            "ok": true,
            "text": outcome.text,
            "send": outcome.send,
            "assisted": outcome.assisted,
            "asr": { "kind": outcome.asr_kind, "model": outcome.asr_model },
        }))),
        Err(error) => Ok(dictate_failure(error)),
    }
}

/// `POST /v1/voice/dictate` — record + transcribe → text to insert.
pub(super) async fn voice_dictate(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    dictate(&state, DictateMode::Insert).await
}

/// `POST /v1/voice/send` — dictate with send-suffix detection; the client
/// submits when `send` is true (empty `text` = submit the current draft).
pub(super) async fn voice_send(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    dictate(&state, DictateMode::Send).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceControlRequest {
    /// The client's current composer text — the model sees it for
    /// AI-assisted dictation (the `/voice-control` pipeline).
    #[serde(default)]
    composer: String,
}

/// `POST /v1/voice/control` — assisted dictation with composer context.
/// `assisted: false` in the response means a free ASR kind handled the audio
/// and the composer text was never seen.
pub(super) async fn voice_control(
    State(state): State<RuntimeApiState>,
    body: Option<Json<VoiceControlRequest>>,
) -> Result<Json<Value>, ApiError> {
    let composer = body
        .map(|Json(request)| request.composer)
        .unwrap_or_default();
    dictate(&state, DictateMode::Control(composer)).await
}
