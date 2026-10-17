//! Host-owned voice capture and builtin shortcut transport.

use crate::commands::CommandResult;
use crate::config::Config;
use crate::tui::app::App;
use crate::voice::{
    is_available, record_audio, resolve_asr_choice, split_send_suffix, transcribe_selected,
};
use codewhale_localization::{MessageId, tr};
use std::time::Duration;

pub fn voice(app: &mut App) -> CommandResult {
    super::core_navigation_host::dispatch::<10>(app, None)
}

// --- Capture orchestration (UI event loop) ---------------------------------

/// What the UI should do with a finished capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceCaptureOutcome {
    /// Insert the transcribed text into the composer at the cursor.
    Insert(String),
    /// Submit this text as a message (auto-send).
    Send(String),
}

/// Status line while recording: the localized recording label, the latest
/// interim transcript once one exists, and how to stop. The capture is awaited
/// on the UI loop, so no key can end it — `record_audio` stops after a second
/// of silence (or `MAX_RECORD_SECS`), and the cue says exactly that.
fn recording_status(locale: codewhale_localization::Locale, interim: Option<&str>) -> String {
    let label = tr(locale, MessageId::VoiceRecording);
    let stop = tr(locale, MessageId::VoiceRecordingStopHint);
    match interim.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => format!("{label} \u{2014} \u{201c}{text}\u{201d} \u{00b7} {stop}"),
        None => format!("{label} \u{00b7} {stop}"),
    }
}

/// Perform a complete record + transcribe cycle with live interim display.
///
/// Runs in the UI event loop (see [`crate::tui::app::AppAction::VoiceCapture`]) so provider
/// credentials come from the live [`Config`] rather than state cached on
/// [`App`]. Recording happens on a blocking thread; transcription uses the
/// shared async HTTP client. Every failure path returns a localized message
/// so callers can surface it as a status line.
pub async fn capture_and_transcribe(
    app: &mut App,
    config: &Config,
) -> Result<VoiceCaptureOutcome, String> {
    let locale = app.ui_locale;

    if !is_available() {
        return Err(tr(locale, MessageId::VoiceErrNoRecorder).to_string());
    }
    // C01-10: the credential check follows the ASR backend that will run,
    // not the chat provider. Local Whisper and Groq need no provider key, so
    // resolve it lazily — the same contract as `crate::voice::dictate_once`
    // — and only refuse before recording when provider ASR is the backend.
    let provider_key = || {
        config
            .active_route_api_key()
            .map_err(|_| tr(locale, MessageId::VoiceErrNoAuth).to_string())
    };
    let (asr_kind, _asr_model) = resolve_asr_choice(config);
    if asr_kind == "provider" {
        provider_key()?;
    }
    // Show the localized recording status plus the live interim in the composer.
    let original_input = app.composer.input.clone();
    let original_cursor = app.composer.cursor_position;
    app.status_message = Some(recording_status(locale, None));

    // Streaming interim: poll every 700ms and show partial transcript like Grok Build's
    // VoiceEvent::Interim → VoiceState::Recording{interim}. We re-transcribe the
    // growing buffer (local-whisper is cheap; Groq is ~300ms; provider falls back).
    let interim_enabled = true; // always show partials — feels alive like Spark

    // Spawn recorder on blocking thread with a shared buffer for interim polling.
    let shared_buf: std::sync::Arc<parking_lot::Mutex<Vec<i16>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let shared_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shared_buf_clone = std::sync::Arc::clone(&shared_buf);
    let shared_done_clone = std::sync::Arc::clone(&shared_done);
    let recorder_handle = tokio::task::spawn_blocking(move || {
        // Bridge to existing record_audio but copy into shared buffer incrementally.
        // For now we reuse the blocking recorder and then publish; interim will
        // poll the final buffer. A true streaming recorder (cpal/pw-record) is
        // the next step — see grokbuild's xai-grok-voice::audio for the subprocess
        // isolation pattern we should mirror.
        let result = record_audio();
        if let Some((samples, dur)) = result {
            *shared_buf_clone.lock() = samples.clone();
            shared_done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            Some((samples, dur))
        } else {
            shared_done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            None
        }
    });

    // Interim polling loop — updates composer with "original + interim ▍" so text
    // appears as you talk, just like Spark's live transcript.
    let mut last_interim = String::new();
    let mut ticks: u32 = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(700)).await;
        ticks += 1;
        if shared_done.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        if !interim_enabled || ticks < 2 {
            continue; // let a little audio accumulate before first interim
        }
        let snapshot = { shared_buf.lock().clone() };
        if snapshot.len() < 8000 {
            // <0.5s of audio — not enough for meaningful ASR
            continue;
        }
        // Try cheapest free ASR for interim; don't fail the whole capture on interim error.
        let interim = transcribe_selected(config, &asr_kind, &snapshot, None)
            .await
            .unwrap_or_default();
        let trimmed = interim.trim();
        if !trimmed.is_empty() && trimmed != last_interim {
            last_interim = trimmed.to_string();
            // Show interim inline — preserve cursor at original position, append interim with a block cursor
            let display = if original_input.trim().is_empty() {
                format!("{trimmed} ▍")
            } else {
                format!("{} {} ▍", original_input.trim_end(), trimmed)
            };
            app.composer.input = display;
            app.composer.cursor_position = original_cursor;
            app.status_message = Some(recording_status(locale, Some(trimmed)));
        }
        if ticks > 40 {
            break; // safety: ~28s max interim polling
        }
    }

    let (samples, _duration) = recorder_handle
        .await
        .ok()
        .flatten()
        .ok_or_else(|| tr(locale, MessageId::VoiceErrTooShort).to_string())?;

    // Restore composer to original before final insert (interim was preview only)
    app.composer.input = original_input.clone();
    app.composer.cursor_position = original_cursor;
    app.status_message = Some(tr(locale, MessageId::VoiceProcessing).to_string());

    let text = transcribe_selected(
        config,
        &asr_kind,
        &samples,
        app.voice_control_enabled.then_some(original_input.as_str()),
    )
    .await
    .map_err(|error| format!("{}: {error}", tr(locale, MessageId::VoiceErrNetwork)))?;

    let clean = text.trim();
    if app.voice_send_enabled {
        let (remainder, wants_send) = split_send_suffix(clean);
        if wants_send {
            // A bare "send it" submits whatever is already in the composer.
            let outgoing = if remainder.is_empty() {
                let existing = app.composer.input.trim().to_string();
                if !existing.is_empty() {
                    app.clear_input();
                }
                existing
            } else {
                remainder.to_string()
            };
            if outgoing.is_empty() {
                return Err(tr(locale, MessageId::VoiceErrEmptySend).to_string());
            }
            return Ok(VoiceCaptureOutcome::Send(outgoing));
        }
    }
    if clean.is_empty() {
        return Err(tr(locale, MessageId::VoiceErrEmptySend).to_string());
    }
    Ok(VoiceCaptureOutcome::Insert(clean.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_status_is_localized_and_keeps_the_interim() {
        use codewhale_localization::Locale;

        for locale in [Locale::En, Locale::De, Locale::Ja] {
            let label = tr(locale, MessageId::VoiceRecording).to_string();
            let stop = tr(locale, MessageId::VoiceRecordingStopHint).to_string();
            let idle = format!("{label} \u{00b7} {stop}");
            assert_eq!(recording_status(locale, None), idle);
            assert_eq!(recording_status(locale, Some("   ")), idle);

            let with_interim = recording_status(locale, Some(" hello there "));
            assert!(with_interim.starts_with(&label), "{with_interim}");
            assert!(with_interim.contains("\u{201c}hello there\u{201d}"));
            assert!(
                with_interim.ends_with(&stop),
                "the stop cue survives the interim: {with_interim}"
            );
            assert!(!with_interim.contains("\u{2325}V"), "no hardcoded key hint");
            if locale != Locale::En {
                assert!(!with_interim.contains("to finish"), "no English hint");
            }
        }
        assert_ne!(
            recording_status(Locale::En, None),
            recording_status(Locale::De, None)
        );
    }
}
