//! Voice capture and transcription core: recorder detection, WAV encoding,
//! ASR selection and dispatch, the send-suffix contract, and the headless
//! record→transcribe cycle.
//!
//! Shared by the TUI's `/voice` commands (`commands::voice`, which keeps the
//! slash commands and the composer-updating capture loop) and the runtime
//! API's `/v1/voice` routes (`runtime_api::voice`). No UI types live here.
//!
//! ## Recording
//!
//! Uses platform-specific command-line tools (sox, rec, arecord) to capture
//! 16kHz mono 16-bit PCM audio. Records until a silence gap is detected or
//! the maximum duration is reached (default 10 s).

use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use codewhale_runtime::config::Config;

/// Transcription model requested from the provider's chat-completions API.
const ASR_MODEL: &str = "mimo-v2.5-asr";
const GROQ_ASR_MODEL: &str = "whisper-large-v3-turbo";
pub const UPLOAD_AUDIO_MAX_BYTES: usize = crate::deepseek_media::DEEPSEEK_ASR_MAX_BYTES;
/// Local whisper binary names to probe (whisper.cpp, faster-whisper, OpenAI whisper).
const LOCAL_WHISPER_BINS: &[&str] = &["whisper", "whisper.cpp", "whisper-cpp", "faster-whisper"];
/// Model used for the AI-assisted voice-control pipeline.
const VOICE_CONTROL_MODEL: &str = "mimo-v2.5";

// --- Recorder detection ----------------------------------------------------

/// Platform-specific recorder definitions.
#[derive(Debug, Clone)]
struct Recorder {
    cmd: &'static str,
    /// CLI arguments for piping raw 16kHz mono S16_LE PCM to stdout.
    pipe_args: &'static [&'static str],
}

fn detect_recorder() -> Option<Recorder> {
    let probe = || {
        if std::env::var_os("CODEWHALE_DISABLE_VOICE").is_some() {
            return None;
        }
        let candidates: &[Recorder] = if cfg!(target_os = "macos") {
            &[
                Recorder {
                    cmd: "sox",
                    pipe_args: &["-d", "-r", "16000", "-c", "1", "-b", "16", "-t", "raw", "-"],
                },
                Recorder {
                    cmd: "rec",
                    pipe_args: &["-r", "16000", "-c", "1", "-b", "16", "-t", "raw", "-"],
                },
            ]
        } else if cfg!(target_os = "linux") {
            &[
                Recorder {
                    cmd: "arecord",
                    pipe_args: &["-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "raw"],
                },
                Recorder {
                    cmd: "sox",
                    pipe_args: &["-d", "-r", "16000", "-c", "1", "-b", "16", "-t", "raw", "-"],
                },
            ]
        } else if cfg!(target_os = "windows") {
            &[Recorder {
                cmd: "sox",
                pipe_args: &["-d", "-r", "16000", "-c", "1", "-b", "16", "-t", "raw", "-"],
            }]
        } else {
            &[]
        };

        candidates
            .iter()
            .find(|r| {
                Command::new(r.cmd)
                    .arg("--version")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .is_ok()
            })
            .cloned()
    };
    #[cfg(any(test, feature = "test-support"))]
    {
        crate::test_support::with_test_env_lock(probe)
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        probe()
    }
}

/// Check whether voice recording is available on this system.
pub fn is_available() -> bool {
    detect_recorder().is_some()
}

// --- WAV encoding ----------------------------------------------------------

/// Encode raw 16kHz mono S16_LE PCM samples as a WAV buffer.
fn encode_wav(samples: &[i16]) -> Vec<u8> {
    encode_wav_with_rate(samples, 16000)
}

fn encode_wav_with_rate(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_size = (samples.len() * 2) as u32;
    let mut buf = Vec::with_capacity(44 + data_size as usize);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_size).to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate.saturating_mul(2)).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for &sample in samples {
        buf.extend_from_slice(&sample.to_le_bytes());
    }

    buf
}

// --- Recording -------------------------------------------------------------

/// Maximum recording duration in seconds before auto-stopping.
pub const MAX_RECORD_SECS: u64 = 10;
/// Minimum segment duration in seconds to consider as valid speech.
const MIN_SEGMENT_SECS: f64 = 0.3;

/// Record audio from the default microphone.
///
/// Returns raw 16kHz mono S16_LE PCM samples. Returns `None` if no recorder
/// is available, the recording failed, or no speech was detected.
pub fn record_audio() -> Option<(Vec<i16>, Duration)> {
    let recorder = detect_recorder()?;
    let start = std::time::Instant::now();

    let mut child = Command::new(recorder.cmd)
        .args(recorder.pipe_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    let mut reader = std::io::BufReader::new(stdout);
    let mut all_samples: Vec<i16> = Vec::with_capacity(16000 * MAX_RECORD_SECS as usize);

    // Read until timeout or silence
    let mut buf = [0u8; 320]; // 10ms of 16kHz S16_LE
    let max_duration = Duration::from_secs(MAX_RECORD_SECS);
    let mut silence_samples = 0u32;
    let mut had_speech = false;
    let speech_threshold: i16 = 500; // RMS-based speech detection threshold
    let silence_duration_samples = 16000u32; // 1 second of silence to stop

    loop {
        use std::io::Read;
        match reader.read_exact(&mut buf) {
            Ok(()) => {
                let chunk: Vec<i16> = buf
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .copied()
                    .map(i16::from_le_bytes)
                    .collect();

                // Simple RMS-based VAD
                let rms = (chunk.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
                    / chunk.len() as f64)
                    .sqrt();
                let is_speech = rms > speech_threshold as f64;

                if is_speech {
                    had_speech = true;
                    silence_samples = 0;
                } else if had_speech {
                    silence_samples += chunk.len() as u32;
                }

                if had_speech {
                    all_samples.extend_from_slice(&chunk);
                }

                if start.elapsed() > max_duration {
                    let _ = child.kill();
                    break;
                }
                if had_speech && silence_samples >= silence_duration_samples {
                    let _ = child.kill();
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => {
                let _ = child.kill();
                break;
            }
        }
    }

    let _ = child.wait();
    let elapsed = start.elapsed();

    let min_samples = (MIN_SEGMENT_SECS * 16000.0) as usize;
    if all_samples.len() < min_samples {
        return None;
    }

    Some((all_samples, elapsed))
}

// --- Auto-send suffix ------------------------------------------------------

/// Trailing phrases that mean "submit this" — the human-readable form of
/// `SEND_SUFFIX_RE`; keep in sync with the regex when either changes.
pub const SEND_PHRASES: &[&str] = &["send it", "发送", "發送"];

/// Matches an explicit send instruction at the end of transcribed text:
/// "send it" (any spacing/case) or 发送/發送, with trailing punctuation.
static SEND_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s,，.。!！?？]+)(?:send\s*it|发送|發送)[\s.。!！?？]*$").unwrap()
});

/// Split a transcript into the message remainder and whether it ended with an
/// explicit send instruction. `"ship the fix, send it"` → `("ship the fix", true)`.
pub fn split_send_suffix(text: &str) -> (&str, bool) {
    match SEND_SUFFIX_RE.find(text) {
        Some(found) => (text[..found.start()].trim(), true),
        None => (text.trim(), false),
    }
}

// --- Transcription ---------------------------------------------------------

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

async fn post_chat_completions(
    api_key: &str,
    base_url: &str,
    mut body: serde_json::Value,
    openrouter_vendor: Option<&str>,
) -> Result<serde_json::Value, String> {
    crate::client::apply_openrouter_vendor(&mut body, openrouter_vendor);
    let _inference = crate::client::acquire_remote_control_inference_participant().await;
    let client = codewhale_release::tls::reqwest_client();
    let resp = client
        .post(chat_completions_url(base_url))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
        .timeout(Duration::from_secs(30))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    resp.json()
        .await
        .map_err(|e| format!("failed to parse response: {e}"))
}

/// Send audio to the provider's API for plain transcription.
///
/// Uses the chat completions endpoint with `input_audio` content blocks.
pub async fn transcribe(
    api_key: &str,
    base_url: &str,
    audio_samples: &[i16],
    openrouter_vendor: Option<&str>,
) -> Result<String, String> {
    transcribe_with_model(
        api_key,
        base_url,
        audio_samples,
        ASR_MODEL,
        openrouter_vendor,
    )
    .await
}

async fn transcribe_with_model(
    api_key: &str,
    base_url: &str,
    audio_samples: &[i16],
    model: &str,
    openrouter_vendor: Option<&str>,
) -> Result<String, String> {
    let wav = encode_wav(audio_samples);
    let data_url = format!("data:audio/wav;base64,{}", base64_encode(&wav));

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": data_url
                        }
                    }
                ]
            }
        ],
        "asr_options": {
            "language": "auto"
        }
    });

    let data = post_chat_completions(api_key, base_url, body, openrouter_vendor).await?;
    data["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "no transcription in response".to_string())
}

/// Process audio through the voice-control pipeline: AI-assisted dictation
/// that sees the current composer text, mirroring MiMo Code's
/// `processVoiceControl`. Used when `/voice-control` is enabled.
pub async fn process_voice_control(
    api_key: &str,
    base_url: &str,
    audio_samples: &[i16],
    current_text: &str,
    openrouter_vendor: Option<&str>,
) -> Result<String, String> {
    let wav = encode_wav(audio_samples);
    let data_url = format!("data:audio/wav;base64,{}", base64_encode(&wav));

    let user_context = serde_json::json!({
        "current_text": current_text,
        "cursor": "end",
    });

    let body = serde_json::json!({
        "model": VOICE_CONTROL_MODEL,
        "messages": [
            {
                "role": "system",
                "content": "You are a voice input assistant. Transcribe the user's speech. Output JSON: {\"text\": \"transcribed text\"}."
            },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": user_context.to_string() },
                    { "type": "input_audio", "input_audio": { "data": data_url } }
                ]
            }
        ],
        "response_format": { "type": "json_object" }
    });

    let data = post_chat_completions(api_key, base_url, body, openrouter_vendor).await?;
    let content = data["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| "no response content".to_string())?;

    let parsed: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("failed to parse voice control JSON: {e}"))?;

    parsed["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no text field in voice control response".to_string())
}

/// Detect best free ASR for this host — local whisper > Groq free > provider fallback.
/// Works on macOS (brew install whisper-cpp), Windows (whisper.cpp binary),
/// Linux (apt), and HarmonyOS (falls back to cloud).
fn detect_free_asr() -> &'static str {
    for bin in LOCAL_WHISPER_BINS {
        if Command::new(bin)
            .arg("--help")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return "local-whisper";
        }
    }
    if std::env::var("GROQ_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return "groq";
    }
    "provider"
}

/// Transcribe via local whisper.cpp (free, offline, cross-platform).
///
/// The whole body is synchronous — temp-file I/O plus `Command::output()`,
/// which blocks for the entire subprocess run — so it runs on the blocking
/// pool rather than a Tokio worker (blocking-call convention, #6149).
pub async fn transcribe_local_whisper(audio_samples: &[i16]) -> Result<String, String> {
    let wav = encode_wav(audio_samples);
    tokio::task::spawn_blocking(move || transcribe_local_whisper_blocking(&wav))
        .await
        .map_err(|e| e.to_string())?
}

fn transcribe_local_whisper_blocking(wav: &[u8]) -> Result<String, String> {
    // C01-10: a fresh, exclusively created file per call. The old
    // `cw-voice-<pid>.wav` name was shared by every transcription in the
    // process (an interim pass, the final pass, a runtime-API dictation) and
    // predictable in a shared temp dir. Dropping `audio` removes it.
    let audio = tempfile::Builder::new()
        .prefix("cw-voice-")
        .suffix(".wav")
        .tempfile()
        .map_err(|e| e.to_string())?;
    let tmp = audio.path().to_path_buf();
    std::fs::write(&tmp, wav).map_err(|e| e.to_string())?;
    // Try each local binary until one succeeds; whisper.cpp outputs to stdout or file.
    for bin in LOCAL_WHISPER_BINS {
        let output = Command::new(bin)
            .arg(tmp.to_string_lossy().as_ref())
            .arg("--model")
            .arg("tiny")
            .arg("--language")
            .arg("auto")
            .arg("--output-txt")
            .output();
        if let Ok(out) = output
            && out.status.success()
        {
            let txt = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !txt.is_empty() {
                return Ok(txt);
            }
            // Some builds write to .txt sidecar
            let sidecar = tmp.with_extension("txt");
            if let Ok(s) = std::fs::read_to_string(&sidecar) {
                let _ = std::fs::remove_file(&sidecar);
                if !s.trim().is_empty() {
                    return Ok(s.trim().to_string());
                }
            }
        }
    }
    Err("local whisper not available".into())
}

/// Transcribe via Groq Whisper large-v3-turbo (free tier, ~$0.04/hr, fast).
/// Groq is NOT a full CodeWhale provider yet — this is a direct ASR call
/// using `GROQ_API_KEY` only (no provider setup needed). Uses the same
/// chat-completions `input_audio` path as Xiaomi so no `multipart` feature.
pub async fn transcribe_groq(audio_samples: &[i16]) -> Result<String, String> {
    let api_key = std::env::var("GROQ_API_KEY").map_err(|_| "GROQ_API_KEY not set".to_string())?;
    let base_url = "https://api.groq.com/openai/v1";
    transcribe_with_model(&api_key, base_url, audio_samples, GROQ_ASR_MODEL, None).await
}

pub fn resolve_asr_choice(config: &Config) -> (String, String) {
    let read = || {
        let explicit = std::env::var("CODEWHALE_ASR_MODEL")
            .ok()
            .and_then(|value| explicit_asr_choice(&value));
        let (kind, model) = explicit.unwrap_or_else(|| match detect_free_asr() {
            "local-whisper" => ("local-whisper", "tiny"),
            "groq" => ("groq", GROQ_ASR_MODEL),
            _ if config.active_provider_identity().is_ok_and(|identity| {
                matches!(
                    identity.provider,
                    codewhale_config::ProviderKind::Deepseek
                        | codewhale_config::ProviderKind::DeepseekAnthropic
                )
            }) =>
            {
                ("deepseek", crate::deepseek_media::DEEPSEEK_ASR_MODEL)
            }
            _ => ("provider", ASR_MODEL),
        });
        (kind.to_string(), model.to_string())
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

fn explicit_asr_choice(value: &str) -> Option<(&'static str, &'static str)> {
    let m = value.trim().to_ascii_lowercase();
    if m.contains("local") || m.contains("whisper.cpp") || m.contains("whisper-cpp") {
        return Some(("local-whisper", "tiny"));
    }
    if m.contains("groq") || m.contains("whisper") {
        return Some(("groq", GROQ_ASR_MODEL));
    }
    if m.contains("deepseek") {
        return Some(("deepseek", crate::deepseek_media::DEEPSEEK_ASR_MODEL));
    }
    if m.contains("mimo") || m.contains("xiaomi") {
        return Some(("provider", ASR_MODEL));
    }
    None
}

// --- Headless capture (HTTP/native-client path) ----------------------------

/// What a headless dictation should do with the finished transcript.
#[derive(Debug, Clone)]
pub enum DictateMode {
    /// Transcribe and return the text for insertion into the composer.
    Insert,
    /// Transcribe, then apply the "send it" / 发送 suffix contract. The
    /// outcome's `send` flag tells the client to submit; a bare send
    /// instruction yields empty `text` so the client submits its own draft.
    Send,
    /// AI-assisted dictation that sees the client's composer text — the
    /// `/voice-control` pipeline. Only provider ASR can see context; free
    /// ASR kinds degrade to plain transcription with `assisted: false`.
    Control(String),
}

/// Machine-readable failure for the headless path so HTTP clients can
/// localize by `reason` rather than parsing message text.
#[derive(Debug)]
pub enum DictateError {
    NoRecorder,
    NoSpeech,
    NoProviderAuth,
    InvalidInput(String),
    Transcription(String),
}

impl DictateError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoRecorder => "no_recorder",
            Self::NoSpeech => "no_speech",
            Self::NoProviderAuth => "no_provider_auth",
            Self::InvalidInput(_) => "invalid_input",
            Self::Transcription(_) => "transcription_failed",
        }
    }
}

impl std::fmt::Display for DictateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRecorder => write!(f, "no supported voice recorder on this host"),
            Self::NoSpeech => write!(f, "no speech detected"),
            Self::NoProviderAuth => {
                write!(f, "provider ASR requires a configured API key")
            }
            Self::InvalidInput(e) | Self::Transcription(e) => write!(f, "{e}"),
        }
    }
}

/// Result of one headless record→transcribe cycle.
#[derive(Debug)]
pub struct DictationOutcome {
    /// Final transcript (send suffix already stripped for `Send` mode).
    pub text: String,
    /// `Send` mode only: the transcript ended with an explicit send phrase.
    pub send: bool,
    /// `Control` mode only: the composer context reached the model. False
    /// when a free ASR kind handled the audio and never saw the context.
    pub assisted: bool,
    /// Which ASR backend was selected for this capture.
    pub asr_kind: String,
    pub asr_model: String,
}

/// Detected recorder binary name, if any (`"sox"`, `"arecord"`, `"rec"`).
pub fn recorder_command() -> Option<&'static str> {
    detect_recorder().map(|r| r.cmd)
}

/// Resolved ASR selection (`kind`, `model`) for capability reporting.
pub fn asr_choice(config: &Config) -> (String, String) {
    resolve_asr_choice(config)
}

/// One record→transcribe cycle with no UI surface: the HTTP/native-client
/// equivalent of `commands::voice::capture_and_transcribe`. Recording runs on a blocking
/// thread; transcription follows the same ASR dispatch as the TUI —
/// explicit `CODEWHALE_ASR_MODEL` > local whisper > Groq > provider —
/// but resolves the provider key lazily so free ASR kinds work without
/// provider auth.
pub async fn dictate_once(
    config: &Config,
    mode: DictateMode,
) -> Result<DictationOutcome, DictateError> {
    if !is_available() {
        return Err(DictateError::NoRecorder);
    }
    let (samples, _duration) = tokio::task::spawn_blocking(record_audio)
        .await
        .ok()
        .flatten()
        .ok_or(DictateError::NoSpeech)?;

    let (asr_kind, asr_model) = resolve_asr_choice(config);
    let assisted = asr_kind == "provider" && matches!(&mode, DictateMode::Control(_));
    let composer = match &mode {
        DictateMode::Control(composer) => Some(composer.as_str()),
        _ => None,
    };
    let text = transcribe_selected(config, &asr_kind, &samples, composer).await?;

    let clean = text.trim().to_string();
    let (text, send) = match mode {
        DictateMode::Send => {
            let (remainder, wants_send) = split_send_suffix(&clean);
            (remainder.to_string(), wants_send)
        }
        _ => (clean, false),
    };
    Ok(DictationOutcome {
        text,
        send,
        assisted,
        asr_kind,
        asr_model,
    })
}

/// One selected ASR route for both native/HTTP and TUI capture. A failure
/// stays on that route: it must not upload the recording to another provider.
pub async fn transcribe_selected(
    config: &Config,
    kind: &str,
    samples: &[i16],
    composer: Option<&str>,
) -> Result<String, DictateError> {
    let text = match kind {
        "local-whisper" => transcribe_local_whisper(samples).await,
        "groq" => transcribe_groq(samples).await,
        "deepseek" => {
            let (base_url, api_key) = crate::deepseek_media::resolve_deepseek_config_auth(config)
                .ok_or(DictateError::NoProviderAuth)?;
            let wav = encode_wav(samples);
            crate::deepseek_media::transcribe_deepseek_bytes(
                &base_url,
                &api_key,
                &wav,
                "voice.wav",
                "audio/wav",
                None,
            )
            .await
            .map(|response| response.text)
        }
        _ => {
            let key = config
                .active_route_api_key()
                .map_err(|_| DictateError::NoProviderAuth)?;
            let base = config.active_route_base_url();
            let vendor = config
                .openrouter_vendor()
                .map_err(|error| DictateError::Transcription(error.to_string()))?;
            match composer {
                Some(composer) => {
                    process_voice_control(&key, &base, samples, composer, vendor.as_deref()).await
                }
                None => transcribe(&key, &base, samples, vendor.as_deref()).await,
            }
        }
    };
    text.map_err(DictateError::Transcription)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceTtsOutcome {
    pub audio_bytes: Vec<u8>,
    pub content_type: String,
    pub format: String,
    pub provider: String,
    pub model: String,
    pub voice: String,
}

pub const MIMO_TTS_VOICES: &[&str] = &["mimo_default", "Mia", "Chloe", "Milo", "Dean"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceTtsError {
    NoProviderAuth(String),
    InvalidInput(String),
    SynthesisFailed(String),
}

impl VoiceTtsError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoProviderAuth(_) => "no_provider_auth",
            Self::InvalidInput(_) => "invalid_input",
            Self::SynthesisFailed(_) => "synthesis_failed",
        }
    }
}

impl std::fmt::Display for VoiceTtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProviderAuth(msg) | Self::InvalidInput(msg) | Self::SynthesisFailed(msg) => {
                write!(f, "{msg}")
            }
        }
    }
}

impl std::error::Error for VoiceTtsError {}

fn active_mimo_tts_ready(config: &Config) -> bool {
    config
        .active_provider_identity()
        .is_ok_and(|identity| identity.provider == codewhale_config::ProviderKind::XiaomiMimo)
        && config
            .active_route_api_key_read_only()
            .is_ok_and(|value| !value.trim().is_empty())
}

fn resample_pcm16_mono(samples: &[i16], src_rate: u32, dst_rate: u32) -> Vec<i16> {
    if samples.is_empty() || src_rate == dst_rate {
        return samples.to_vec();
    }
    let out_len = ((samples.len() as u64).saturating_mul(dst_rate as u64) / (src_rate as u64))
        .max(1) as usize;
    if out_len == 1 || samples.len() == 1 {
        return vec![samples[0]; out_len];
    }
    let ratio = (samples.len() as f64) / (out_len as f64);
    let max_idx = samples.len() - 1;
    (0..out_len)
        .map(|i| {
            let pos = (i as f64) * ratio;
            let idx = (pos.floor() as usize).min(max_idx);
            let next = (idx + 1).min(max_idx);
            let frac = pos - (idx as f64);
            let a = samples[idx] as f64;
            let b = samples[next] as f64;
            (a + (b - a) * frac)
                .round()
                .clamp(i16::MIN as f64, i16::MAX as f64) as i16
        })
        .collect()
}

pub fn decode_wav_pcm16_mono(wav_bytes: &[u8]) -> Result<Vec<i16>, String> {
    if wav_bytes.len() < 12 || &wav_bytes[0..4] != b"RIFF" || &wav_bytes[8..12] != b"WAVE" {
        return Err("invalid WAV header: expected RIFF/WAVE".to_string());
    }
    let mut sample_rate: Option<u32> = None;
    let mut data_chunk: Option<&[u8]> = None;
    let mut offset = 12usize;
    while offset + 8 <= wav_bytes.len() {
        let chunk_id = &wav_bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes([
            wav_bytes[offset + 4],
            wav_bytes[offset + 5],
            wav_bytes[offset + 6],
            wav_bytes[offset + 7],
        ]) as usize;
        let data_start = offset + 8;
        let Some(data_end) = data_start.checked_add(chunk_size) else {
            return Err("invalid WAV chunk length".to_string());
        };
        if data_end > wav_bytes.len() {
            return Err("truncated WAV chunk".to_string());
        }
        let chunk = &wav_bytes[data_start..data_end];
        if chunk_id == b"fmt " {
            if chunk.len() < 16 {
                return Err("truncated WAV fmt chunk".to_string());
            }
            let audio_format = u16::from_le_bytes([chunk[0], chunk[1]]);
            let channels = u16::from_le_bytes([chunk[2], chunk[3]]);
            let parsed_rate = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            let bits_per_sample = u16::from_le_bytes([chunk[14], chunk[15]]);
            if audio_format != 1 || channels != 1 || bits_per_sample != 16 {
                return Err(format!(
                    "unsupported WAV encoding: expected PCM16 mono, got format={audio_format} channels={channels} bits={bits_per_sample}"
                ));
            }
            if !(8_000..=192_000).contains(&parsed_rate) {
                return Err(format!(
                    "unsupported WAV sample rate {parsed_rate} Hz (expected 8000..=192000 Hz)"
                ));
            }
            sample_rate = Some(parsed_rate);
        } else if chunk_id == b"data" {
            data_chunk = Some(chunk);
        }
        offset = data_end.saturating_add(chunk_size & 1);
    }
    let Some(sample_rate) = sample_rate else {
        return Err("missing WAV fmt chunk".to_string());
    };
    let Some(data) = data_chunk else {
        return Err("missing WAV data chunk".to_string());
    };
    if data.len() % 2 != 0 {
        return Err("WAV PCM16 data length must be even".to_string());
    }
    let raw_samples: Vec<i16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(i16::from_le_bytes)
        .collect();
    Ok(resample_pcm16_mono(&raw_samples, sample_rate, 16_000))
}

pub fn upload_available(config: &Config) -> bool {
    let (asr_kind, _) = resolve_asr_choice(config);
    match asr_kind.as_str() {
        "local-whisper" => true,
        "groq" => {
            let has_groq_key =
                || std::env::var("GROQ_API_KEY").is_ok_and(|value| !value.trim().is_empty());
            #[cfg(any(test, feature = "test-support"))]
            {
                crate::test_support::with_test_env_lock(has_groq_key)
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                has_groq_key()
            }
        }
        "deepseek" => crate::deepseek_media::resolve_deepseek_config_auth(config).is_some(),
        _ => config
            .active_route_api_key_read_only()
            .is_ok_and(|value| !value.trim().is_empty()),
    }
}

pub fn tts_available(config: &Config) -> bool {
    active_mimo_tts_ready(config)
        || crate::deepseek_media::resolve_deepseek_config_auth(config).is_some()
}

pub fn tts_voices(config: &Config) -> &'static [&'static str] {
    if active_mimo_tts_ready(config) {
        MIMO_TTS_VOICES
    } else if crate::deepseek_media::resolve_deepseek_config_auth(config).is_some() {
        crate::deepseek_media::DEEPSEEK_TTS_VOICES
    } else {
        &[]
    }
}

pub async fn transcribe_uploaded_audio(
    config: &Config,
    audio_bytes: &[u8],
    mime_type: &str,
    mode: DictateMode,
    language: Option<&str>,
) -> Result<DictationOutcome, DictateError> {
    if audio_bytes.is_empty() {
        return Err(DictateError::InvalidInput(
            "audio payload cannot be empty".to_string(),
        ));
    }
    if audio_bytes.len() > UPLOAD_AUDIO_MAX_BYTES {
        return Err(DictateError::InvalidInput(format!(
            "audio payload exceeds maximum of {UPLOAD_AUDIO_MAX_BYTES} bytes"
        )));
    }
    let canonical_mime = crate::deepseek_media::normalize_deepseek_asr_mime(mime_type)
        .map_err(DictateError::InvalidInput)?;
    let (asr_kind, asr_model) = resolve_asr_choice(config);
    let assisted = asr_kind == "provider" && matches!(&mode, DictateMode::Control(_));
    let text = if asr_kind == "deepseek" {
        let (base_url, api_key) = crate::deepseek_media::resolve_deepseek_config_auth(config)
            .ok_or(DictateError::NoProviderAuth)?;
        let file_name = if canonical_mime == "audio/ogg" {
            "voice.ogg"
        } else {
            "voice.wav"
        };
        crate::deepseek_media::transcribe_deepseek_bytes(
            &base_url,
            &api_key,
            audio_bytes,
            file_name,
            canonical_mime,
            language,
        )
        .await
        .map(|response| response.text)
        .map_err(DictateError::Transcription)?
    } else {
        if canonical_mime != "audio/wav" {
            return Err(DictateError::InvalidInput(format!(
                "mime type '{canonical_mime}' requires DeepSeek ASR; send audio/wav for '{asr_kind}'"
            )));
        }
        let samples = decode_wav_pcm16_mono(audio_bytes).map_err(DictateError::InvalidInput)?;
        if samples.is_empty() {
            return Err(DictateError::NoSpeech);
        }
        let composer = match &mode {
            DictateMode::Control(composer) => Some(composer.as_str()),
            _ => None,
        };
        transcribe_selected(config, &asr_kind, &samples, composer).await?
    };

    let clean = text.trim().to_string();
    let (text, send) = match mode {
        DictateMode::Send => {
            let (remainder, wants_send) = split_send_suffix(&clean);
            (remainder.to_string(), wants_send)
        }
        _ => (clean, false),
    };
    Ok(DictationOutcome {
        text,
        send,
        assisted,
        asr_kind,
        asr_model,
    })
}

pub async fn synthesize_voice_tts(
    config: &Config,
    text: &str,
    voice: Option<&str>,
    format: Option<&str>,
) -> Result<VoiceTtsOutcome, VoiceTtsError> {
    synthesize_voice_tts_with_options(config, text, voice, format, true).await
}

pub async fn synthesize_voice_tts_with_options(
    config: &Config,
    text: &str,
    voice: Option<&str>,
    format: Option<&str>,
    filter_markdown: bool,
) -> Result<VoiceTtsOutcome, VoiceTtsError> {
    if text.trim().is_empty() {
        return Err(VoiceTtsError::InvalidInput(
            "speech text cannot be empty".to_string(),
        ));
    }
    if text.chars().count() > crate::deepseek_media::DEEPSEEK_TTS_MAX_CHARS {
        return Err(VoiceTtsError::InvalidInput(format!(
            "speech text exceeds maximum of {} characters",
            crate::deepseek_media::DEEPSEEK_TTS_MAX_CHARS
        )));
    }
    if active_mimo_tts_ready(config) {
        let req_format = format.unwrap_or("wav").trim();
        let format_value = if req_format.is_empty() {
            "wav".to_string()
        } else {
            crate::tools::speech::normalize_speech_format(req_format).ok_or_else(|| {
                VoiceTtsError::InvalidInput(format!(
                    "unsupported speech format '{req_format}' (allowed: wav, mp3, pcm16)"
                ))
            })?
        };
        let requested_voice = voice
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("mimo_default");
        let client = crate::client::CodewhaleClient::new(config)
            .map_err(|err| VoiceTtsError::SynthesisFailed(err.to_string()))?;
        let response = client
            .synthesize_speech(crate::client::SpeechSynthesisRequest {
                model: "mimo-v2.5-tts".to_string(),
                text: text.to_string(),
                instruction: None,
                audio_format: format_value,
                voice: Some(requested_voice.to_string()),
            })
            .await
            .map_err(|err| VoiceTtsError::SynthesisFailed(err.to_string()))?;
        let content_type = match response.audio_format.as_str() {
            "mp3" => "audio/mpeg",
            "pcm" | "pcm16" => "audio/pcm",
            _ => "audio/wav",
        };
        return Ok(VoiceTtsOutcome {
            audio_bytes: response.audio_bytes,
            content_type: content_type.to_string(),
            format: response.audio_format,
            provider: "xiaomi-mimo".to_string(),
            model: response.model,
            voice: response
                .voice
                .unwrap_or_else(|| requested_voice.to_string()),
        });
    }

    if let Some((base_url, api_key)) = crate::deepseek_media::resolve_deepseek_config_auth(config) {
        crate::deepseek_media::validate_deepseek_tts_inputs(
            Some(crate::deepseek_media::DEEPSEEK_TTS_MODEL),
            text,
            None,
            voice,
            format.unwrap_or("wav"),
        )
        .map_err(VoiceTtsError::InvalidInput)?;
        let outcome = crate::deepseek_media::synthesize_deepseek_bytes_with_options(
            &base_url,
            &api_key,
            text,
            voice,
            format.unwrap_or("wav"),
            filter_markdown,
        )
        .await
        .map_err(VoiceTtsError::SynthesisFailed)?;
        return Ok(VoiceTtsOutcome {
            audio_bytes: outcome.audio_bytes,
            content_type: outcome.content_type,
            format: outcome.format,
            provider: "deepseek".to_string(),
            model: outcome.model,
            voice: outcome.voice,
        });
    }

    Err(VoiceTtsError::NoProviderAuth(
        "voice TTS requires a configured DeepSeek or Xiaomi MiMo API key".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    mod selected_asr;

    use super::*;

    #[tokio::test]
    async fn voice_requests_preserve_openrouter_vendor_pin() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "{\"text\":\"hello\"}" } }]
            })))
            .expect(3)
            .mount(&server)
            .await;
        let base_url = format!("{}/v1", server.uri());
        transcribe(
            "fixture-key",
            &base_url,
            &[0; 16],
            Some("chutes/region-fixture"),
        )
        .await
        .unwrap();
        process_voice_control(
            "fixture-key",
            &base_url,
            &[0; 16],
            "existing text",
            Some("chutes/region-fixture"),
        )
        .await
        .unwrap();
        transcribe_with_model("fixture-key", &base_url, &[0; 16], GROQ_ASR_MODEL, None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        for request in &requests[..2] {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(
                body["provider"],
                serde_json::json!({"order": ["chutes/region-fixture"], "allow_fallbacks": false})
            );
        }
        let independent: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
        assert!(independent.get("provider").is_none());
    }

    #[test]
    fn explicit_local_whisper_is_not_routed_to_groq() {
        // C01-10: every local spelling contains "whisper"; none may reach the
        // hosted Groq backend (which needs a key and sends audio off-host).
        for local in ["local-whisper", "whisper.cpp", "whisper-cpp", " LOCAL "] {
            assert_eq!(
                explicit_asr_choice(local),
                Some(("local-whisper", "tiny")),
                "{local}"
            );
        }
        for hosted in ["groq", "whisper", "whisper-large-v3-turbo"] {
            assert_eq!(
                explicit_asr_choice(hosted),
                Some(("groq", GROQ_ASR_MODEL)),
                "{hosted}"
            );
        }
        assert_eq!(explicit_asr_choice("mimo"), Some(("provider", ASR_MODEL)));
        assert_eq!(explicit_asr_choice("something-else"), None);
    }

    #[test]
    fn wav_encoding_produces_valid_header() {
        let samples = vec![0i16; 16000]; // 1 second of silence
        let wav = encode_wav(&samples);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        // data size = 16000 * 2 = 32000
        assert_eq!(&wav[4..8], &(36 + 32000u32).to_le_bytes());
    }

    #[test]
    fn wav_encoding_empty_is_minimal() {
        let wav = encode_wav(&[]);
        assert_eq!(wav.len(), 44);
        assert_eq!(&wav[4..8], &36u32.to_le_bytes());
    }

    #[test]
    fn send_suffix_detected_and_stripped() {
        assert_eq!(split_send_suffix("send it"), ("", true));
        assert_eq!(split_send_suffix("Send It!"), ("", true));
        assert_eq!(split_send_suffix("发送"), ("", true));
        assert_eq!(split_send_suffix("發送。"), ("", true));
        assert_eq!(
            split_send_suffix("ship the fix, send it"),
            ("ship the fix", true)
        );
        assert_eq!(
            split_send_suffix("修复这个问题，发送"),
            ("修复这个问题", true)
        );
    }

    #[test]
    fn send_suffix_leaves_plain_text_alone() {
        assert_eq!(split_send_suffix("send it now"), ("send it now", false));
        assert_eq!(
            split_send_suffix("帮我发送一封邮件"),
            ("帮我发送一封邮件", false)
        );
        assert_eq!(split_send_suffix("发送邮件"), ("发送邮件", false));
        assert_eq!(
            split_send_suffix("resend it to the queue"),
            ("resend it to the queue", false)
        );
    }

    #[test]
    fn recorder_detection_does_not_crash() {
        let _lock = crate::test_support::lock_test_env();
        let _ = is_available();
    }

    #[test]
    fn explicit_deepseek_asr_choice_resolves_model() {
        assert_eq!(
            explicit_asr_choice("deepseek"),
            Some(("deepseek", crate::deepseek_media::DEEPSEEK_ASR_MODEL))
        );
        assert_eq!(
            explicit_asr_choice("DeepSeek-ASR"),
            Some(("deepseek", crate::deepseek_media::DEEPSEEK_ASR_MODEL))
        );
    }

    #[test]
    fn wav_decoder_roundtrips_pcm16_mono_and_rejects_bad_headers() {
        let wav = encode_wav(&[10, -250, 1234, -4096]);
        let decoded = decode_wav_pcm16_mono(&wav).expect("valid wav");
        assert_eq!(decoded, vec![10, -250, 1234, -4096]);
        assert!(decode_wav_pcm16_mono(b"not-wav").is_err());

        let zero_rate = encode_wav_with_rate(&[1, 2, 3], 0);
        assert!(decode_wav_pcm16_mono(&zero_rate).is_err());

        let wav_48k = encode_wav_with_rate(&vec![1000i16; 480], 48_000);
        let resampled = decode_wav_pcm16_mono(&wav_48k).expect("48kHz wav decodes");
        assert_eq!(resampled.len(), 160);
    }

    #[tokio::test]
    async fn deepseek_uploaded_audio_and_tts_work_with_deepseek_config() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/asr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "request_id": "req-upload",
                "text": "commit the slice send it",
                "duration_ms": 950
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/tts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"RIFF_TTS".to_vec()))
            .mount(&server)
            .await;

        let _lock = lock_test_env();
        let _asr_env = EnvVarGuard::remove("CODEWHALE_ASR_MODEL");
        let _groq_env = EnvVarGuard::remove("GROQ_API_KEY");
        let _cli_source = EnvVarGuard::remove(codewhale_config::CLI_API_KEY_SOURCE_ENV);
        let _legacy_cli_source =
            EnvVarGuard::remove(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV);
        let _cli_key = EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let _ds_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _ds_base = EnvVarGuard::remove("DEEPSEEK_BASE_URL");
        let _cw_base = EnvVarGuard::remove("CODEWHALE_BASE_URL");
        let _path = EnvVarGuard::set("PATH", "/nonexistent-voice-path");

        let config: Config = toml::from_str(&format!(
            "provider = \"deepseek\"\n\n[providers.deepseek]\napi_key = \"ds-voice-key\"\nbase_url = \"{}/v1\"\n",
            server.uri()
        ))
        .expect("deepseek config parses");

        assert_eq!(
            resolve_asr_choice(&config),
            ("deepseek".to_string(), "deepseek-asr".to_string())
        );
        assert!(upload_available(&config));
        assert!(tts_available(&config));

        let wav = encode_wav(&[0; 320]);
        let outcome =
            transcribe_uploaded_audio(&config, &wav, "audio/wav", DictateMode::Send, None)
                .await
                .expect("upload transcription");
        assert_eq!(outcome.text, "commit the slice");
        assert!(outcome.send);
        assert!(!outcome.assisted);
        assert_eq!(outcome.asr_kind, "deepseek");
        assert_eq!(outcome.asr_model, "deepseek-asr");

        let tts = synthesize_voice_tts(&config, "All tests passed", Some("tide"), Some("wav"))
            .await
            .expect("voice tts");
        assert_eq!(tts.audio_bytes, b"RIFF_TTS");
        assert_eq!(tts.provider, "deepseek");
        assert_eq!(tts.voice, "tide");
    }
}
