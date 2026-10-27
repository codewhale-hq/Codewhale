use std::io::Write;
use std::time::Duration;

use super::payload::{NotificationKind, NotificationPayload};
use super::{DeliveryOutcome, Method, NotificationGate};

/// Resolve `Auto` to a concrete method by inspecting `$TERM_PROGRAM`,
/// `$LC_TERMINAL`, and `$TERM`.
///
/// Resolution table:
/// - `iTerm.app`, `WezTerm`, `Cmux` → `Osc9`
/// - `Ghostty` → `Ghostty` (OSC 777)
/// - `kitty` → `Kitty` (OSC 99)
/// - `$LC_TERMINAL` matches OSC-9 capable → `Osc9` (Cmux that sets LC_TERMINAL)
/// - `$TERM` contains `ghostty` → `Osc9` (cmux etc.)
/// - `$TERM` contains `kitty` → `Kitty`
/// - Unknown terminal → `Off` (never invent an audible fallback)
#[must_use]
pub fn resolve_method() -> Method {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    match term_program.as_str() {
        "iTerm.app" | "WezTerm" | "Cmux" => return Method::Osc9,
        "Ghostty" => return Method::Ghostty,
        "kitty" => return Method::Kitty,
        _ => {}
    }

    // LC_TERMINAL fallback for terminals (e.g. Cmux) that set
    // LC_TERMINAL instead of TERM_PROGRAM.
    let lc_terminal = std::env::var("LC_TERMINAL").unwrap_or_default();
    match lc_terminal.as_str() {
        "iTerm.app" | "Ghostty" | "WezTerm" | "Cmux" => return Method::Osc9,
        _ => {}
    }

    // A banner selection must never invent audio. Windows users who want the
    // system sound can explicitly select `method = "bel"` or a completion
    // sound; unknown automatic transports fail closed.
    if cfg!(target_os = "windows") {
        return Method::Off;
    }

    if cfg!(target_os = "macos") {
        return Method::MacOS;
    }

    // Ghostty-based terminals (cmux, etc.) may not set their own
    // TERM_PROGRAM but do set TERM=xterm-ghostty. Likewise for Kitty.
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("ghostty") {
        Method::Osc9
    } else if term.contains("kitty") {
        Method::Kitty
    } else {
        Method::Off
    }
}

/// Wrap an escape sequence for terminal multiplexer passthrough.
///
/// tmux intercepts escape sequences; DCS passthrough tunnels them to
/// the outer terminal unmodified. Every ESC inside the payload is
/// doubled so tmux does not interpret it as DCS end.
fn wrap_for_multiplexer(seq: &str, in_tmux: bool) -> String {
    if in_tmux {
        let escaped = seq.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;{escaped}\x1b\\")
    } else {
        seq.to_string()
    }
}

/// Build the raw escape bytes for the given method and message.
///
/// When `in_tmux` is `true`, OSC sequences are wrapped in DCS passthrough
/// so tmux forwards them to the outer terminal.
#[must_use]
fn build_escape(method: Method, in_tmux: bool, msg: &str) -> Vec<u8> {
    match method {
        Method::Bel => vec![b'\x07'],
        Method::Osc9 => {
            let inner = format!("\x1b]9;{msg}\x07");
            if in_tmux {
                let escaped_inner = inner.replace('\x1b', "\x1b\x1b");
                format!("\x1bPtmux;{escaped_inner}\x1b\\").into_bytes()
            } else {
                inner.into_bytes()
            }
        }
        Method::Kitty => {
            // Kitty notification: OSC 99 ; params ST
            // ST terminator (ESC \) instead of BEL to avoid audible beep.
            let title_seq = "\x1b]99;d=0:p=title\x1b\\";
            let body_seq = format!("\x1b]99;p=body;{msg}\x1b\\");
            let focus_seq = "\x1b]99;d=1:a=focus\x1b\\";
            let combined = format!("{title_seq}{body_seq}{focus_seq}");
            wrap_for_multiplexer(&combined, in_tmux).into_bytes()
        }
        Method::Ghostty => {
            // Ghostty notification: OSC 777 ; notify ; title ; message BEL
            let seq = format!("\x1b]777;notify;codewhale;{msg}\x07");
            wrap_for_multiplexer(&seq, in_tmux).into_bytes()
        }
        // Auto and Off and MacOS should not reach build_escape.
        Method::Auto | Method::Off | Method::MacOS => vec![],
    }
}

/// All side effects sit behind injected sinks. A disallowed event reaches none.
#[allow(clippy::too_many_arguments)]
pub fn notify_with_sinks(
    method: Method,
    in_tmux: bool,
    payload: &NotificationPayload,
    threshold: Duration,
    elapsed: Duration,
    gate: NotificationGate,
    attention_allowed: bool,
    sink: &mut dyn Write,
    decide_sound: &mut dyn FnMut(NotificationKind, bool) -> crate::sound_policy::SoundDecision,
    audio: &mut dyn FnMut(
        &crate::sound_policy::SoundCue,
        &mut dyn Write,
    ) -> crate::audio::AudioOutcome,
    native: &mut dyn FnMut(&NotificationPayload) -> DeliveryOutcome,
) -> DeliveryOutcome {
    use crate::audio::AudioOutcome;
    use crate::sound_policy::SoundDecision;
    if !attention_allowed {
        return DeliveryOutcome::SuppressedByAttention;
    }
    if elapsed < threshold {
        return DeliveryOutcome::SuppressedByThreshold;
    }
    if method == Method::Off {
        return DeliveryOutcome::SuppressedByMethod;
    }
    if !gate.allows(payload.kind()) {
        return DeliveryOutcome::SuppressedByGate;
    }
    let effective = if method == Method::Auto {
        resolve_method()
    } else {
        method
    };
    if effective == Method::Off {
        return DeliveryOutcome::UnsupportedTransport;
    }
    let banner = match effective {
        Method::MacOS => native(payload),
        Method::Bel => DeliveryOutcome::Delivered(effective),
        _ => {
            let bytes = build_escape(effective, in_tmux, &payload.render_inline());
            if bytes.is_empty() {
                return DeliveryOutcome::UnsupportedTransport;
            }
            if sink.write_all(&bytes).and_then(|()| sink.flush()).is_err() {
                return DeliveryOutcome::DeliveryFailed;
            }
            DeliveryOutcome::Delivered(effective)
        }
    };
    if !matches!(
        banner,
        DeliveryOutcome::Delivered(_) | DeliveryOutcome::Dispatched(_)
    ) {
        return banner;
    }
    // BEL is itself audio: select and emit one cue instead of adding a
    // transport bell to the chosen sound. Never fall back after suppression.
    match decide_sound(payload.kind(), effective == Method::Bel) {
        SoundDecision::Suppress(_) if effective == Method::Bel => {
            DeliveryOutcome::SuppressedBySound
        }
        SoundDecision::Suppress(_) => banner,
        SoundDecision::Play(cue) => match audio(&cue, sink) {
            AudioOutcome::Emitted => banner,
            AudioOutcome::Dispatched if effective == Method::Bel => {
                DeliveryOutcome::Dispatched(effective)
            }
            AudioOutcome::Dispatched => banner,
            AudioOutcome::Busy if effective == Method::Bel => DeliveryOutcome::SuppressedBySound,
            AudioOutcome::Unsupported if effective == Method::Bel => {
                DeliveryOutcome::UnsupportedTransport
            }
            AudioOutcome::Failed if effective == Method::Bel => DeliveryOutcome::DeliveryFailed,
            AudioOutcome::Busy => banner,
            AudioOutcome::Unsupported | AudioOutcome::Failed => {
                if matches!(banner, DeliveryOutcome::Dispatched(_)) {
                    DeliveryOutcome::DispatchedWithoutSound(effective)
                } else {
                    DeliveryOutcome::DeliveredWithoutSound(effective)
                }
            }
        },
    }
}
