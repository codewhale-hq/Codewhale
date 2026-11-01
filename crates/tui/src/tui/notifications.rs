//! Desktop notifications for turn completion.
//!
//! Supports five delivery mechanisms:
//! - **OSC 9** — terminal escape sequence (`\x1b]9;…\x07`) for iTerm2,
//!   Ghostty, WezTerm, and tmux (with DCS passthrough).
//! - **Kitty** — OSC 99 protocol with ST terminator (no audible beep).
//! - **Ghostty** — OSC 777 notification protocol.
//! - **BEL** — an explicit audio-only notification transport.
//!
//! When `method = "auto"`, the resolver picks the best method for the
//! current terminal. Unknown terminals fail closed to `Off`; an audible BEL
//! is emitted only by an explicitly selected sound or `method = "bel"`.
//!
//! Every mechanism is fed a [`NotificationPayload`] — a typed, bounded,
//! redaction-aware value — rather than a free-form `String` (#4834). See
//! [`codewhale_notify::payload`] for the per-kind disclosure
//! policy.
//!
//! Delivery is governed by one [`NotificationGate`] (#5041):
//! `[notifications].quiet` silences everything and
//! `[notifications.events]` disables individual categories, enforced at
//! the emission path so no protocol can leak a suppressed event.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::atomic::{AtomicU8, AtomicU64};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use codewhale_notify::delivery::notify_with_sinks;
#[cfg(test)]
use codewhale_notify::delivery::resolve_method;
#[cfg(test)]
use codewhale_notify::payload::NotificationKind;
pub use codewhale_notify::payload::NotificationPayload;
use codewhale_notify::{
    AttentionCondition, DeliveryOutcome, Method, NotificationGate, attention_delivery_allowed_at,
    settings_projection,
};

/// Process-wide configured delivery method. Installed before the event loop
/// starts and updated by live Settings, so producers such as the model-facing
/// `notify` tool cannot silently bypass `method = "off"`.
static CONFIGURED_METHOD: AtomicU8 = AtomicU8::new(0);

fn method_to_u8(method: Method) -> u8 {
    match method {
        Method::Auto => 0,
        Method::Osc9 => 1,
        Method::Bel => 2,
        Method::MacOS => 3,
        Method::Kitty => 4,
        Method::Ghostty => 5,
        Method::Off => 6,
    }
}

/// Install `method` as the process-wide notification method for paths that
/// do not resolve a method of their own (the `notify` tool); `pub(crate)`
/// so the tool's tests can arrange the installed method.
pub(crate) fn install_configured_method(method: Method) {
    CONFIGURED_METHOD.store(method_to_u8(method), Ordering::SeqCst);
}

/// Delivery method currently selected by Settings.
#[must_use]
pub fn configured_method() -> Method {
    match CONFIGURED_METHOD.load(Ordering::SeqCst) {
        1 => Method::Osc9,
        2 => Method::Bel,
        3 => Method::MacOS,
        4 => Method::Kitty,
        5 => Method::Ghostty,
        6 => Method::Off,
        _ => Method::Auto,
    }
}

/// Everything on, quiet off — the pre-#5041 behavior, and the effective
/// policy until the first [`settings`] call installs the configured gate.
const GATE_DEFAULT_BITS: u8 = 0b0111_1110;

/// Process-wide gate, packed to one byte so reads on the emission path are
/// a single atomic load.
static NOTIFICATION_GATE: AtomicU8 = AtomicU8::new(GATE_DEFAULT_BITS);

static ATTENTION_CONDITION: AtomicU8 = AtomicU8::new(AttentionCondition::Unfocused as u8);
static UNFOCUSED_SINCE_MS: AtomicU64 = AtomicU64::new(0);

fn attention_clock_ms() -> u64 {
    static STARTED_AT: OnceLock<std::time::Instant> = OnceLock::new();
    // Reserve zero for "no observed focus loss".
    STARTED_AT
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis()
        .saturating_add(1) as u64
}

fn install_attention_condition(condition: AttentionCondition) {
    ATTENTION_CONDITION.store(condition as u8, Ordering::SeqCst);
}

fn current_attention_condition() -> AttentionCondition {
    match ATTENTION_CONDITION.load(Ordering::SeqCst) {
        0 => AttentionCondition::Always,
        2 => AttentionCondition::Never,
        _ => AttentionCondition::Unfocused,
    }
}

#[must_use]
fn attention_delivery_allowed() -> bool {
    attention_delivery_allowed_at(
        current_attention_condition(),
        TERMINAL_FOCUSED.load(Ordering::SeqCst),
        UNFOCUSED_SINCE_MS.load(Ordering::SeqCst),
        attention_clock_ms(),
    )
}

/// Install `gate` as the process-wide notification policy.
pub fn install_notification_gate(gate: NotificationGate) {
    NOTIFICATION_GATE.store(gate.to_bits(), Ordering::SeqCst);
}

/// The currently installed process-wide notification gate.
#[must_use]
pub fn current_notification_gate() -> NotificationGate {
    NotificationGate::from_bits(NOTIFICATION_GATE.load(Ordering::SeqCst))
}

/// Emit a notification to `sink` if the elapsed time meets or exceeds
/// `threshold`, `method` is not `Off`, and `gate` allows the payload's
/// category.
///
/// This variant takes a `W: Write` sink and an explicit gate for
/// testability; production callers go through [`notify_done`], which
/// loads the installed process-wide gate.
#[cfg(test)]
pub fn notify_done_to<W: Write>(
    method: Method,
    in_tmux: bool,
    payload: &NotificationPayload,
    threshold: Duration,
    elapsed: Duration,
    gate: NotificationGate,
    sink: &mut W,
) -> DeliveryOutcome {
    let mut policy = codewhale_notify::sound_policy::EventSoundPolicy::default();
    notify_with_sinks(
        method,
        in_tmux,
        payload,
        threshold,
        elapsed,
        gate,
        true,
        sink,
        &mut |kind, bell| {
            policy.decide(
                codewhale_notify::sound_policy::event_for_kind(kind),
                0,
                bell,
            )
        },
        &mut codewhale_notify::audio::emit_terminal,
        &mut |_| DeliveryOutcome::UnsupportedTransport,
    )
}

/// Emit a notification to **stdout** if `elapsed >= threshold`.
///
/// With `method = Auto`, selects the best protocol for the current terminal
/// (OSC 9, Kitty OSC 99, Ghostty OSC 777, or macOS). Unknown terminals
/// remain unsupported; explicit method Off suppresses audio and banners.
/// See [`codewhale_notify::delivery::resolve_method`] for the canonical resolution table. Pass
/// `in_tmux = true` (i.e. `$TMUX` is non-empty at runtime) to wrap OSC
/// sequences in a DCS passthrough.
pub fn notify_done(
    method: Method,
    in_tmux: bool,
    payload: &NotificationPayload,
    threshold: Duration,
    elapsed: Duration,
) -> DeliveryOutcome {
    let reserved = std::cell::Cell::new(None);
    notify_with_sinks(
        method,
        in_tmux,
        payload,
        threshold,
        elapsed,
        current_notification_gate(),
        attention_delivery_allowed(),
        &mut io::stdout(),
        &mut |kind, bell| {
            let now_ms = codewhale_notify::sound_policy::epoch_millis_now();
            let decision = codewhale_notify::sound_policy::decide(kind, now_ms, bell);
            if matches!(
                decision,
                codewhale_notify::sound_policy::SoundDecision::Play(_)
            ) {
                reserved.set(Some((kind, now_ms)));
            }
            decision
        },
        &mut |cue, sink| {
            let outcome = codewhale_notify::audio::dispatch(cue, sink);
            // A cue that never played must not spend the category's repeat
            // window: the next notification may try again (U06-m3).
            if matches!(
                outcome,
                codewhale_notify::audio::AudioOutcome::Busy
                    | codewhale_notify::audio::AudioOutcome::Unsupported
                    | codewhale_notify::audio::AudioOutcome::Failed
            ) && let Some((kind, reserved_ms)) = reserved.take()
            {
                codewhale_notify::sound_policy::release(kind, reserved_ms);
            }
            outcome
        },
        &mut dispatch_native,
    )
}

/// The `notify` tool's delivery: a typed model-notify payload through the
/// configured method, installed gate and attention policy, threshold zero.
pub(crate) fn notify_model(title: &str, body: Option<&str>) -> &'static str {
    // #4834: model-authored text is the least trusted input that can reach
    // Notification Center, so it goes through the typed payload like every
    // other event kind: bounded, control-byte-stripped, and redacted for
    // credentials, absolute paths, and raw tool JSON.
    let payload = NotificationPayload::model_notify(title, body);
    let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
    notify_done(
        configured_method(),
        in_tmux,
        &payload,
        Duration::ZERO,
        Duration::from_secs(1),
    )
    .receipt()
}

/// Set the terminal taskbar progress state via OSC 9 ; 4.
///
/// Windows Terminal supports this to show progress on the taskbar icon:
/// - `state = 0` — no progress (clear)
/// - `state = 1` — indeterminate (cycling green)
/// - `state = 2` — normal (0-100, requires progress param)
/// - `state = 3` — error (red)
/// - `state = 4` — paused (yellow)
///
/// Other terminals (iTerm2, WezTerm) ignore the sequence silently.
/// Best-effort — write failures are ignored.
/// Build the OSC 9;4 taskbar-progress sequence. Split from the write so the
/// bytes can be asserted without depending on whether the test runner owns a
/// terminal.
#[must_use]
fn taskbar_progress_sequence(state: u8, progress: Option<u8>) -> String {
    match progress {
        Some(pct) => format!("\x1b]9;4;{state};{pct}\x07"),
        None => format!("\x1b]9;4;{state}\x07"),
    }
}

const MAX_TERMINAL_TITLE_CHARS: usize = 160;

/// Build a bounded OSC 0 window-title sequence. User-controlled session names
/// can reach this boundary, so control and bidi-format characters are removed
/// before the title is embedded in a terminal escape sequence.
#[must_use]
fn terminal_title_sequence(title: &str) -> String {
    let safe: String = codewhale_engine::session_manager::sanitize_session_title(title)
        .chars()
        .take(MAX_TERMINAL_TITLE_CHARS)
        .collect();
    format!("\x1b]0;{safe}\x07")
}

/// Whether raw terminal control sequences may be written to stdout.
///
/// OSC 9;4 (taskbar progress) and OSC 0 (window title) are *control* bytes,
/// not content. A terminal that understands them renders nothing visible; a
/// pipe, a file, or a CI log renders them literally, so `cargo test` output
/// and redirected sessions pick up stray `]9;4;1]0;` noise. Gate on stdout
/// actually being a TTY — there is no one to control otherwise.
fn stdout_accepts_control_sequences() -> bool {
    use std::io::IsTerminal;
    io::stdout().is_terminal()
}

pub fn set_taskbar_progress(state: u8, progress: Option<u8>) {
    if !stdout_accepts_control_sequences() {
        return;
    }
    let seq = taskbar_progress_sequence(state, progress);
    let mut stdout = io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

/// Set taskbar progress to indeterminate (cycling) — call at turn start.
pub fn set_taskbar_progress_busy() {
    set_taskbar_progress(1, None);
}

/// Clear taskbar progress — call at turn end.
pub fn clear_taskbar_progress() {
    set_taskbar_progress(0, None);
}

/// User-configured window-title prefix, rendered as `[prefix] …` in front of
/// every terminal window title. Empty means no prefix — the historical
/// byte-for-byte behavior. Set via the `/title` command (session level) or
/// the `title` config key (default level); the render loop syncs it here
/// through [`set_title_prefix`].
static TITLE_PREFIX: OnceLock<Mutex<String>> = OnceLock::new();

pub(crate) fn title_prefix_slot() -> &'static Mutex<String> {
    TITLE_PREFIX.get_or_init(|| Mutex::new(String::new()))
}

/// Serialise tests that touch the process-global title prefix so parallel
/// threads cannot leak a prefix into an unrelated assertion. Also used by
/// `underwater` tests that drive [`set_title_prefix`] through the render
/// loop.
#[cfg(test)]
pub(crate) fn title_prefix_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Set the `[prefix] …` window-title prefix, or clear it with `None`/empty.
///
/// Change detection keeps the per-frame render-loop sync free when the title
/// did not move; on an actual change the running title is redrawn immediately
/// so alt-tabbed sessions pick up the new identity without waiting for the
/// next activity-verb update.
pub fn set_title_prefix(prefix: Option<&str>) {
    let prefix = prefix.unwrap_or_default().trim();
    let changed = {
        let mut slot = title_prefix_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.as_str() == prefix {
            false
        } else {
            slot.clear();
            slot.push_str(prefix);
            true
        }
    };
    // Redraw only after the prefix lock is released: the title render path
    // re-locks [`title_prefix_slot`] through `decorate_title`, and a `Mutex`
    // is not reentrant — drawing while holding it would deadlock the
    // render loop on the first `/title` during an active turn.
    if !changed {
        return;
    }
    if TITLE_ANIMATION_RUNNING.load(Ordering::SeqCst) {
        let base = title_animation_base()
            .lock()
            .map_or_else(|_| "codewhale".to_string(), |base| base.clone());
        let motion = TITLE_MOTION_ENABLED.load(Ordering::SeqCst);
        set_terminal_title(&title_activity_label(
            &base,
            Duration::ZERO,
            TERMINAL_FOCUSED.load(Ordering::SeqCst),
            motion,
        ));
    } else {
        // At rest nothing else repaints OSC 0 until the next turn starts, so
        // `/title` or `/rename` between turns must redraw the resting title
        // itself — otherwise the tab keeps the old name while the command
        // already reported success.
        set_terminal_title(&decorate_title(resting_title_body()));
    }
}

/// The undecorated title body shown between turns: the completion marker
/// while it is still on display, otherwise the plain product name.
fn resting_title_body() -> &'static str {
    if COMPLETION_MARKER_SHOWN.load(Ordering::SeqCst) {
        "✓ done"
    } else {
        "codewhale"
    }
}

/// Shared flag controlling the title activity marker. Set to `true` by
/// `start_title_animation()`, cleared by `stop_title_animation()`.
static TITLE_ANIMATION_RUNNING: AtomicBool = AtomicBool::new(false);
/// Focus reporting starts enabled before the event loop begins, so treating
/// the terminal as focused is the safe default: never flood window chrome
/// unless the terminal has explicitly reported `FocusLost` or motion is on.
static TERMINAL_FOCUSED: AtomicBool = AtomicBool::new(true);
/// When false, the title keeps a static whale + state (reduced motion /
/// status animation off) instead of cycling frames.
static TITLE_MOTION_ENABLED: AtomicBool = AtomicBool::new(true);
/// Invalidates a previous animation worker when a new turn starts or ends.
static TITLE_ANIMATION_GENERATION: AtomicU64 = AtomicU64::new(0);
static TITLE_ANIMATION_BASE: OnceLock<Mutex<String>> = OnceLock::new();
static TITLE_ACTIVITY_VERB: OnceLock<Mutex<String>> = OnceLock::new();
/// Whale frames restored from #1871 (`cd357de0c`). Cycle slowly so the
/// terminal title communicates life without competing with in-app spinners.
const TITLE_FRAME_HOLD: Duration = Duration::from_millis(800);
const TITLE_WHALE_FRAMES: &[&str] = &["🐳", "🐋", "🐳", "🐋"];

fn title_animation_base() -> &'static Mutex<String> {
    TITLE_ANIMATION_BASE.get_or_init(|| Mutex::new("codewhale".to_string()))
}

fn title_activity_verb() -> &'static Mutex<String> {
    TITLE_ACTIVITY_VERB.get_or_init(|| Mutex::new("in the current…".to_string()))
}

/// Configure whether the title whale cycles frames.
///
/// Call once at startup (and whenever motion settings change). Reduced motion
/// and `status_indicator = "off"` both freeze the title to a single whale.
pub fn set_title_motion_enabled(enabled: bool) {
    TITLE_MOTION_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Update the truthful activity verb shown next to the title whale
/// (`in the current…`, `reasoning…`, `using tool…`, `verifying…`, `waiting on you…`).
pub fn set_title_activity_verb(verb: &str) {
    let verb = verb.trim();
    if verb.is_empty() {
        return;
    }
    if let Ok(mut slot) = title_activity_verb().lock() {
        if slot.as_str() == verb {
            return;
        }
        verb.clone_into(&mut *slot);
    }
    if !TITLE_ANIMATION_RUNNING.load(Ordering::SeqCst) {
        return;
    }
    let base = title_animation_base()
        .lock()
        .map_or_else(|_| "codewhale".to_string(), |base| base.clone());
    set_terminal_title(&title_activity_label(
        &base,
        Duration::ZERO,
        TERMINAL_FOCUSED.load(Ordering::SeqCst),
        TITLE_MOTION_ENABLED.load(Ordering::SeqCst),
    ));
}

#[must_use]
fn title_activity_label(base: &str, elapsed: Duration, focused: bool, motion: bool) -> String {
    let verb = title_activity_verb()
        .lock()
        .map_or_else(|_| "in the current…".to_string(), |v| v.clone());
    let body = if verb.is_empty() {
        base.to_string()
    } else {
        verb
    };
    // Static title when motion is off or the window is focused: one whale +
    // state, no competing spinner in the focused app chrome.
    if !motion || focused {
        return decorate_title(&format!("🐳 {body}"));
    }
    let frame = TITLE_WHALE_FRAMES
        [(elapsed.as_millis() / TITLE_FRAME_HOLD.as_millis()) as usize % TITLE_WHALE_FRAMES.len()];
    decorate_title(&format!("{frame} {body}"))
}

/// Apply the `[prefix] ` decoration to a raw window-title body.
///
/// With no configured prefix this returns the input unchanged, so existing
/// installs keep the exact titles they had before this feature landed.
fn decorate_title(raw: &str) -> String {
    let prefix = title_prefix_slot()
        .lock()
        .map_or_else(|_| String::new(), |prefix| prefix.clone());
    if prefix.is_empty() {
        raw.to_string()
    } else {
        format!("[{prefix}] {raw}")
    }
}

/// Write OSC 0 (set window title) sequence.
fn set_terminal_title(title: &str) {
    if !stdout_accepts_control_sequences() {
        return;
    }
    let seq = terminal_title_sequence(title);
    let mut stdout = io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

/// Tracks whether the completion marker was set, so
/// `reset_title_on_interaction()` can skip redundant writes.
static COMPLETION_MARKER_SHOWN: AtomicBool = AtomicBool::new(false);

/// Mark the terminal title as active with the animated whale + state verb.
///
/// While focused (or under reduced motion), the title stays a static whale
/// with the current verb. After `FocusLost` with motion enabled, the whale
/// frames cycle so alt-tabbed sessions still communicate progress.
pub fn start_title_animation(original: &str) {
    if let Ok(mut base) = title_animation_base().lock() {
        original.clone_into(&mut base);
    }
    if let Ok(mut verb) = title_activity_verb().lock()
        && verb.is_empty()
    {
        "in the current…".clone_into(&mut *verb);
    }
    COMPLETION_MARKER_SHOWN.store(false, Ordering::SeqCst);
    TITLE_ANIMATION_RUNNING.store(true, Ordering::SeqCst);
    let generation = TITLE_ANIMATION_GENERATION
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    let focused = TERMINAL_FOCUSED.load(Ordering::SeqCst);
    let motion = TITLE_MOTION_ENABLED.load(Ordering::SeqCst);
    set_terminal_title(&title_activity_label(
        original,
        Duration::ZERO,
        focused,
        motion,
    ));

    let base = original.to_string();
    std::thread::spawn(move || {
        let started_at = std::time::Instant::now();
        loop {
            std::thread::sleep(TITLE_FRAME_HOLD);
            if !TITLE_ANIMATION_RUNNING.load(Ordering::SeqCst)
                || TITLE_ANIMATION_GENERATION.load(Ordering::SeqCst) != generation
            {
                break;
            }
            let motion = TITLE_MOTION_ENABLED.load(Ordering::SeqCst);
            // Only advance frames when unfocused + motion is on. Focused
            // windows keep the static whale so the title is not a second
            // spinner competing with in-app activity chrome.
            if motion && !TERMINAL_FOCUSED.load(Ordering::SeqCst) {
                set_terminal_title(&title_activity_label(
                    &base,
                    started_at.elapsed(),
                    false,
                    true,
                ));
            }
        }
    });
}

/// Update the focus gate used by the title activity signal.
///
/// Focus gain immediately restores the steady whale + verb. Focus loss emits
/// the first animation frame immediately, then the worker advances it at the
/// debounced whale cadence.
pub fn set_terminal_focused(focused: bool) {
    let was_focused = TERMINAL_FOCUSED.swap(focused, Ordering::SeqCst);
    if focused {
        UNFOCUSED_SINCE_MS.store(0, Ordering::SeqCst);
    } else if was_focused {
        // Only a real focused -> unfocused transition starts the grace period;
        // duplicate FocusLost reports must not keep postponing delivery.
        UNFOCUSED_SINCE_MS.store(attention_clock_ms(), Ordering::SeqCst);
    }
    if !TITLE_ANIMATION_RUNNING.load(Ordering::SeqCst) {
        return;
    }
    let base = title_animation_base()
        .lock()
        .map_or_else(|_| "codewhale".to_string(), |base| base.clone());
    let motion = TITLE_MOTION_ENABLED.load(Ordering::SeqCst);
    set_terminal_title(&title_activity_label(
        &base,
        Duration::ZERO,
        focused,
        motion,
    ));
}

/// Stop the title animation and show a completion marker.
///
/// Sets the title to `✓ done` so alt-tabbed users see at a glance that
/// processing finished. The marker is overwritten on the next turn by
/// [`start_title_animation`].
pub fn stop_title_animation() {
    stop_title_animation_with(set_terminal_title);
}

fn stop_title_animation_with(set_title: impl FnOnce(&str)) {
    TITLE_ANIMATION_RUNNING.store(false, Ordering::SeqCst);
    TITLE_ANIMATION_GENERATION.fetch_add(1, Ordering::SeqCst);
    // Always show the completion marker so quiet-sound modes still communicate
    // finish state in the window title; interaction clears it.
    COMPLETION_MARKER_SHOWN.store(true, Ordering::SeqCst);
    set_title(&decorate_title("✓ done"));
}

/// Stop the title animation without playing the completion sound.
///
/// Cancellation and failed turns should return the terminal title to rest
/// without presenting them as completed work.
pub fn stop_title_animation_quietly() {
    TITLE_ANIMATION_RUNNING.store(false, Ordering::SeqCst);
    TITLE_ANIMATION_GENERATION.fetch_add(1, Ordering::SeqCst);
    COMPLETION_MARKER_SHOWN.store(false, Ordering::SeqCst);
    set_terminal_title(&decorate_title("codewhale"));
}

/// Clear the completion marker from the title when the user interacts.
///
/// Call this on every user input event (key press, mouse click) so the
/// marker doesn't persist once the user is back at the terminal.
pub fn reset_title_on_interaction() {
    if COMPLETION_MARKER_SHOWN.swap(false, Ordering::SeqCst) {
        set_terminal_title(&decorate_title("codewhale"));
    }
}

/// Show a macOS Notification Center alert via `osascript`.
///
/// Runs on a dedicated background thread so the caller is not blocked.
///
/// The notification includes:
/// - **Title**: "Codewhale"
/// - **Subtitle**: [`NotificationPayload::headline`] (≤ 80 chars)
/// - **Body**: [`NotificationPayload::body`] (≤ 322 chars: a ≤ 120-char
///   detail, a separator, and a ≤ 200-char preview)
/// - **Sound**: none in the AppleScript; the unified notification decision
///   dispatches the selected audio cue.
///
/// Both fields arrive already sanitized, redacted, and character-bounded
/// by [`NotificationPayload`]; this function does not re-derive them from
/// free-form text (#4834).
///
/// **Security**: The message is passed to `osascript` as a command-line
/// argument via `ARGV`, never embedded inline in the AppleScript source.
/// AppleScript does not treat backslash as an escape inside double-quoted
/// string literals, so the previous `\"` approach would terminate the
/// string at the `"` and leave any text between unbalanced quotes
/// evaluated as raw AppleScript code — a code-injection vector for
/// AI-generated notification text. Passing via `ARGV` avoids this
/// entirely because the message is never parsed as AppleScript syntax.
/// Keep it that way.
///
/// **Attribution**: the banner is posted on behalf of `osascript`, which
/// is unbundled, so macOS attributes it to `com.apple.ScriptEditor2`. See
/// [`Method::MacOS`] — that is not fixable from here.
///
/// This is best-effort: if `osascript` is not available (e.g. headless SSH
/// session) the error is logged via `tracing::warn!` instead of silently
/// swallowed.
#[cfg(target_os = "macos")]
const MACOS_DISPLAY_NOTIFICATION_SCRIPT: &str =
    "display notification theBody with title \"Codewhale\" subtitle theSubtitle";

#[cfg(all(target_os = "macos", not(test)))]
fn macos_display_notification(payload: &NotificationPayload) -> DeliveryOutcome {
    let (subtitle, body) = macos_notification_parts(payload);

    // Spawn on a background thread so we don't block the caller.
    // osascript itself is fast (~50 ms), but spawning a subprocess
    // synchronously from an async context steals a tokio thread.
    let result = std::thread::Builder::new()
        .name("osascript-notif".into())
        .spawn(move || {
            // Build AppleScript that receives the message via ARGV
            // instead of inline string interpolation. AppleScript does
            // not treat backslash as an escape inside double-quoted
            // string literals, so `\"` would terminate the string at
            // the `"` and leave a dangling `\`. Passing the message as
            // a command-line argument avoids any injection risk.
            let args = [
                "-e".to_string(),
                "on run argv".to_string(),
                "-e".to_string(),
                "set theBody to item 1 of argv".to_string(),
                "-e".to_string(),
                "set theSubtitle to item 2 of argv".to_string(),
                "-e".to_string(),
                MACOS_DISPLAY_NOTIFICATION_SCRIPT.to_string(),
                "-e".to_string(),
                "end run".to_string(),
                "--".to_string(),
                body,
                subtitle,
            ];

            match std::process::Command::new("osascript").args(&args).output() {
                Ok(output) if !output.status.success() => {
                    tracing::warn!("osascript notification failed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "osascript notification error");
                }
                _ => {}
            }
        });
    if result.is_ok() {
        DeliveryOutcome::Dispatched(Method::MacOS)
    } else {
        DeliveryOutcome::DeliveryFailed
    }
}

fn dispatch_native(payload: &NotificationPayload) -> DeliveryOutcome {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        macos_display_notification(payload)
    }
    #[cfg(any(not(target_os = "macos"), test))]
    {
        let _ = payload;
        DeliveryOutcome::UnsupportedTransport
    }
}

/// Split a payload into the `(subtitle, body)` pair `display notification`
/// wants. Both halves are already bounded and redacted by the payload
/// constructors, so this is a projection, not a sanitizer.
#[cfg(target_os = "macos")]
fn macos_notification_parts(payload: &NotificationPayload) -> (String, String) {
    (payload.headline().to_string(), payload.body())
}

// ── Per-turn notification composition ────────────────────────────────
//
// The helpers below decide *whether* to notify on a completed turn and
// *what message* to put in the body. The low-level dispatcher is
// `notify_done`; everything in this block sits in front of it.

use crate::tui::app::App;
use codewhale_engine::tools::subagent::SubAgentStatus;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_models::{ContentBlock, Message};

pub fn settings(config: &codewhale_runtime::config::Config) -> Option<(Method, Duration, bool)> {
    apply_settings(&config.notifications_config())
}

/// Install the process-wide method, gate, sound policy and attention
/// condition from `[notifications]`; returns the resolved projection.
pub(crate) fn apply_settings(
    notif: &codewhale_config::notifications::NotificationsConfig,
) -> Option<(Method, Duration, bool)> {
    // Install the category/quiet gate (#5041) so `notify_done` honors
    // `[notifications].quiet` and `[notifications.events]`.
    install_notification_gate(NotificationGate::from_config(notif));
    codewhale_notify::sound_policy::reconfigure(
        codewhale_notify::sound_policy::EventSoundPolicy::from_config(notif),
    );
    let projection = settings_projection(notif);
    let method = projection.map_or(Method::Off, |(method, _, _)| method);
    install_configured_method(method);

    let condition = notif
        .condition
        .unwrap_or(codewhale_config::notifications::NotificationCondition::Unfocused);
    match condition {
        codewhale_config::notifications::NotificationCondition::Always => {
            install_attention_condition(AttentionCondition::Always);
        }
        codewhale_config::notifications::NotificationCondition::Unfocused => {
            install_attention_condition(AttentionCondition::Unfocused);
        }
        codewhale_config::notifications::NotificationCondition::Never => {
            install_attention_condition(AttentionCondition::Never);
        }
    }

    projection
}

/// Build the notification payload for a completed turn. Prefers the live
/// streaming text the user just saw; falls back to the latest assistant
/// message in `api_messages` if streaming text is empty (for example, the
/// turn finished entirely through tool output). When `include_summary` is
/// true, an elapsed/cost suffix is appended to the headline.
///
/// The assistant text becomes the payload's *preview*, which means it is
/// redacted and capped at 200 characters before it can reach the OS.
pub fn completed_turn_payload(
    app: &App,
    current_streaming_text: &str,
    include_summary: bool,
    turn_elapsed: Duration,
    turn_cost: Option<codewhale_engine::pricing::CostEstimate>,
) -> NotificationPayload {
    let headline = completion_status(
        &tr(app.ui_locale, MessageId::NotificationTurnComplete),
        include_summary,
        turn_elapsed,
        turn_cost.map(|cost| app.format_cost_estimate(cost)),
    );

    let preview =
        text_summary(current_streaming_text).or_else(|| latest_assistant_text(&app.api_messages));

    NotificationPayload::turn_complete(&headline).with_preview(preview.as_deref())
}

pub(crate) fn subagent_terminal_label(status: &SubAgentStatus) -> MessageId {
    match status {
        SubAgentStatus::Completed => MessageId::NotificationSubagentComplete,
        SubAgentStatus::Failed(_) => MessageId::NotificationSubagentFailed,
        SubAgentStatus::Interrupted(_) => MessageId::NotificationSubagentInterrupted,
        SubAgentStatus::Cancelled => MessageId::NotificationSubagentCancelled,
        SubAgentStatus::BudgetExhausted => MessageId::NotificationSubagentBudgetExhausted,
        SubAgentStatus::Running => MessageId::SubagentsStatusRunning,
    }
}

/// Action-first approval banner (#5041): leads with the decision the user
/// must make and names the tool it concerns. The tool *description* — the
/// pending command — intentionally stays in the terminal (#4834).
#[must_use]
pub fn approval_needed_payload(locale: Locale, tool_name: &str) -> NotificationPayload {
    NotificationPayload::approval_needed(
        &tr(locale, MessageId::NotificationApprovalNeeded).replace("{tool}", tool_name),
        tool_name,
    )
}

/// Action-first blocked-on-input banner (#5041): says what to do and
/// where. The question text itself never leaves the terminal (#4834).
#[must_use]
pub fn input_needed_payload(locale: Locale) -> NotificationPayload {
    NotificationPayload::input_needed(&tr(locale, MessageId::NotificationInputNeeded))
}

/// Action-first sandbox-elevation banner (#5041): leads with the decision
/// and names the blocked tool; the denial reason rides in the body.
#[must_use]
pub fn elevation_needed_payload(
    locale: Locale,
    tool_name: &str,
    denial_reason: &str,
) -> NotificationPayload {
    NotificationPayload::elevation_needed(
        &tr(locale, MessageId::NotificationElevationNeeded).replace("{tool}", tool_name),
        tool_name,
        denial_reason,
    )
}

pub(crate) fn completion_status(
    label: &str,
    include_summary: bool,
    elapsed: Duration,
    cost: Option<String>,
) -> String {
    if !include_summary {
        return label.to_string();
    }

    let human = codewhale_command_contract::elapsed::format_elapsed_secs(elapsed.as_secs());
    match cost {
        Some(cost) => format!("{label} ({human}, {cost})"),
        None => format!("{label} ({human})"),
    }
}

/// Find the latest assistant message in `messages` and return a
/// notification-ready summary of its `Text` content. Thinking blocks,
/// tool calls, and tool results are skipped — only the user-visible
/// reply contributes to the body.
pub fn latest_assistant_text(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|message| {
            message.role == "assistant"
                || message.role == codewhale_models::INTERRUPTED_ASSISTANT_ROLE
        })
        .and_then(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    ContentBlock::Thinking { .. }
                    | ContentBlock::ToolUse { .. }
                    | ContentBlock::ToolResult { .. }
                    | ContentBlock::ServerToolUse { .. }
                    | ContentBlock::ToolSearchToolResult { .. }
                    | ContentBlock::CodeExecutionToolResult { .. } => None,
                    ContentBlock::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            text_summary(&text)
        })
}

/// Sanitize + collapse + truncate streaming text into something fit to
/// hand the OS notification system. Returns `None` when nothing
/// useful remains after sanitization.
pub fn text_summary(text: &str) -> Option<String> {
    const MAX_CHARS: usize = 360;

    let sanitized = codewhale_secrets::sanitize::sanitize_stream_chunk(text);
    let collapsed = sanitized
        .lines()
        .map(str::trim)
        .filter(|line: &&str| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((idx, _)) = trimmed.char_indices().nth(MAX_CHARS) {
        let mut s = String::with_capacity(idx + 3);
        s.push_str(&trimmed[..idx]);
        s.push_str("...");
        Some(s)
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use codewhale_notify::DEFAULT_UNFOCUSED_GRACE;

    /// Moved from `tools::notify`: the `notify` tool's emission chain, where
    /// the installed method decides suppression before any sink write.
    #[test]
    fn configured_method_off_silences_the_tool_emission() {
        let _lock = env_lock();
        let _restore = ConfiguredMethodRestore::capture();
        install_configured_method(Method::Off);

        // The emission chain `execute` drives: the installed method decides
        // suppression before any sink write, with the gate loaded from the
        // process-wide state.
        let payload = NotificationPayload::model_notify("done", None);
        let mut sink = Vec::new();
        notify_done_to(
            configured_method(),
            false,
            &payload,
            std::time::Duration::ZERO,
            std::time::Duration::from_secs(1),
            current_notification_gate(),
            &mut sink,
        );
        assert!(
            sink.is_empty(),
            "configured method=off must silence the notify tool path"
        );
    }

    /// The function the host port calls for the `notify` tool must read the
    /// installed method: `[notifications] method = "off"` silences the tool
    /// and the receipt says so. A `notify_model` that ignored
    /// `configured_method()` would pass the test above but fail this one.
    #[test]
    fn notify_model_honors_the_installed_off_method() {
        let _lock = env_lock();
        let _method_restore = ConfiguredMethodRestore::capture();
        let _gate_restore = NotificationGateRestore::capture();
        let previous_condition = current_attention_condition();
        // `condition = "always"` so the attention policy (checked first) lets
        // the call reach the method check whatever the test runner's focus.
        let off: codewhale_runtime::config::Config = toml::from_str(
            r#"
            [notifications]
            method = "off"
            condition = "always"
            "#,
        )
        .expect("method=off config should parse");
        let _ = settings(&off);

        let receipt = notify_model("done", Some("all tests pass"));

        install_attention_condition(previous_condition);
        assert_eq!(receipt, DeliveryOutcome::SuppressedByMethod.receipt());
    }

    #[test]
    fn title_whale_is_static_when_focused_or_motion_disabled() {
        let _guard = prefix_lock();
        if let Ok(mut verb) = title_activity_verb().lock() {
            "in the current…".clone_into(&mut *verb);
        }
        assert_eq!(
            title_activity_label("codewhale", Duration::ZERO, true, true),
            "🐳 in the current…"
        );
        assert_eq!(
            title_activity_label("codewhale", Duration::ZERO, false, false),
            "🐳 in the current…"
        );
        assert_eq!(
            title_activity_label("codewhale", Duration::ZERO, false, true),
            "🐳 in the current…"
        );
        assert_eq!(
            title_activity_label("codewhale", Duration::from_millis(800), false, true),
            "🐋 in the current…"
        );
    }

    #[test]
    fn title_whale_frames_are_the_restored_emoji_pair() {
        assert_eq!(TITLE_WHALE_FRAMES, &["🐳", "🐋", "🐳", "🐋"]);
        assert_eq!(TITLE_FRAME_HOLD, Duration::from_millis(800));
    }

    /// Serialise tests that touch the process-global title prefix so parallel
    /// threads cannot leak a prefix into an unrelated assertion.
    fn prefix_lock() -> std::sync::MutexGuard<'static, ()> {
        title_prefix_test_lock()
    }

    #[test]
    fn title_prefix_decorates_activity_label() {
        let _guard = prefix_lock();
        set_title_prefix(Some("task-7"));
        if let Ok(mut verb) = title_activity_verb().lock() {
            "reasoning…".clone_into(&mut *verb);
        }
        assert_eq!(
            title_activity_label("codewhale", Duration::ZERO, true, true),
            "[task-7] 🐳 reasoning…"
        );
        assert_eq!(
            title_activity_label("codewhale", Duration::ZERO, false, true),
            "[task-7] 🐳 reasoning…"
        );
        set_title_prefix(None);
        assert_eq!(
            title_activity_label("codewhale", Duration::ZERO, true, true),
            "🐳 reasoning…"
        );
    }

    #[test]
    fn title_prefix_decorates_rest_and_completion_titles() {
        let _guard = prefix_lock();
        set_title_prefix(Some("feature/x"));
        assert_eq!(decorate_title("codewhale"), "[feature/x] codewhale");
        assert_eq!(decorate_title("✓ done"), "[feature/x] ✓ done");
        set_title_prefix(None);
        assert_eq!(decorate_title("codewhale"), "codewhale");
        assert_eq!(decorate_title("✓ done"), "✓ done");
        // Empty/whitespace prefixes behave exactly like `None`.
        set_title_prefix(Some("   "));
        assert_eq!(decorate_title("codewhale"), "codewhale");
        set_title_prefix(None);
    }

    #[test]
    fn title_prefix_change_detection_skips_redundant_writes() {
        let _guard = prefix_lock();
        set_title_prefix(Some("alpha"));
        assert_eq!(title_prefix_slot().lock().unwrap().as_str(), "alpha");
        // Setting the same prefix again must not clear the stored value.
        set_title_prefix(Some("alpha"));
        assert_eq!(title_prefix_slot().lock().unwrap().as_str(), "alpha");
        set_title_prefix(Some("beta"));
        assert_eq!(title_prefix_slot().lock().unwrap().as_str(), "beta");
        set_title_prefix(None);
        assert_eq!(title_prefix_slot().lock().unwrap().as_str(), "");
    }

    #[test]
    fn set_title_prefix_redraws_without_deadlocking_while_animating() {
        // Regression: `set_title_prefix` used to redraw the title while still
        // holding the prefix lock. The redraw path (`title_activity_label` →
        // `decorate_title`) re-locks the same `Mutex`, and `Mutex` is not
        // reentrant — the first `/title` during an active turn froze the
        // whole render loop. Exercise the exact path: prefix change while
        // the animation worker is running.
        let _guard = prefix_lock();
        start_title_animation("codewhale");
        assert!(TITLE_ANIMATION_RUNNING.load(Ordering::SeqCst));
        set_title_prefix(Some("task-7"));
        assert_eq!(title_prefix_slot().lock().unwrap().as_str(), "task-7");
        set_title_prefix(None);
        assert_eq!(title_prefix_slot().lock().unwrap().as_str(), "");
        stop_title_animation_quietly();
        assert!(!TITLE_ANIMATION_RUNNING.load(Ordering::SeqCst));
    }

    /// Serialise tests that mutate process-global environment or notification
    /// sound state while the test harness runs them in parallel threads.
    fn env_lock() -> crate::test_support::TestEnvLock {
        crate::test_support::lock_test_env()
    }

    struct NotificationGateRestore(NotificationGate);

    impl NotificationGateRestore {
        fn capture() -> Self {
            Self(current_notification_gate())
        }
    }

    impl Drop for NotificationGateRestore {
        fn drop(&mut self) {
            install_notification_gate(self.0);
        }
    }

    /// Escape-protocol tests care about the bytes, not the composition
    /// policy, so they go through the least-privileged constructor.
    fn capture(
        method: Method,
        in_tmux: bool,
        msg: &str,
        threshold_secs: u64,
        elapsed_secs: u64,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        notify_done_to(
            method,
            in_tmux,
            &NotificationPayload::input_needed(msg),
            Duration::from_secs(threshold_secs),
            Duration::from_secs(elapsed_secs),
            NotificationGate::default(),
            &mut buf,
        );
        buf
    }

    /// Emit `payload` through OSC 9 under an explicit `gate`, returning the
    /// bytes. The gate is passed by value, so these tests never touch the
    /// process-wide gate and cannot race the other capture tests.
    fn capture_gated(payload: &NotificationPayload, gate: NotificationGate) -> Vec<u8> {
        let mut buf = Vec::new();
        notify_done_to(
            Method::Osc9,
            false,
            payload,
            Duration::ZERO,
            Duration::from_secs(1),
            gate,
            &mut buf,
        );
        buf
    }

    #[test]
    fn gate_defaults_allow_every_kind() {
        let gate = NotificationGate::default();
        for kind in [
            NotificationKind::TurnComplete,
            NotificationKind::SubagentTerminal,
            NotificationKind::BackgroundTerminal,
            NotificationKind::ApprovalNeeded,
            NotificationKind::InputNeeded,
            NotificationKind::ElevationNeeded,
            NotificationKind::ModelNotify,
        ] {
            assert!(gate.allows(kind), "default gate must allow {kind:?}");
        }
    }

    #[test]
    fn quiet_gate_suppresses_every_kind() {
        let gate = NotificationGate {
            quiet: true,
            ..NotificationGate::default()
        };
        for kind in [
            NotificationKind::TurnComplete,
            NotificationKind::SubagentTerminal,
            NotificationKind::BackgroundTerminal,
            NotificationKind::ApprovalNeeded,
            NotificationKind::InputNeeded,
            NotificationKind::ElevationNeeded,
            NotificationKind::ModelNotify,
        ] {
            assert!(!gate.allows(kind), "quiet gate must suppress {kind:?}");
        }
    }

    #[test]
    fn disabled_category_suppresses_only_that_kind() {
        let gate = NotificationGate {
            approval_needed: false,
            ..NotificationGate::default()
        };
        assert!(!gate.allows(NotificationKind::ApprovalNeeded));
        assert!(gate.allows(NotificationKind::TurnComplete));
        assert!(gate.allows(NotificationKind::InputNeeded));
        assert!(gate.allows(NotificationKind::ModelNotify));
    }

    #[test]
    fn gate_bits_roundtrip_and_default_constant_agree() {
        assert_eq!(NotificationGate::default().to_bits(), GATE_DEFAULT_BITS);
        let odd = NotificationGate {
            quiet: true,
            turn_complete: false,
            subagent_terminal: true,
            approval_needed: false,
            input_needed: true,
            elevation_needed: false,
            model_notify: true,
        };
        assert_eq!(NotificationGate::from_bits(odd.to_bits()), odd);
    }

    #[test]
    fn background_attention_waits_for_a_real_focus_loss() {
        let grace_ms = DEFAULT_UNFOCUSED_GRACE.as_millis() as u64;

        assert!(!attention_delivery_allowed_at(
            AttentionCondition::Unfocused,
            true,
            0,
            grace_ms + 10,
        ));
        assert!(!attention_delivery_allowed_at(
            AttentionCondition::Unfocused,
            false,
            0,
            grace_ms + 10,
        ));
        assert!(!attention_delivery_allowed_at(
            AttentionCondition::Unfocused,
            false,
            100,
            100 + grace_ms - 1,
        ));
        assert!(attention_delivery_allowed_at(
            AttentionCondition::Unfocused,
            false,
            100,
            100 + grace_ms,
        ));
    }

    #[test]
    fn explicit_attention_conditions_override_focus() {
        assert!(attention_delivery_allowed_at(
            AttentionCondition::Always,
            true,
            0,
            0,
        ));
        assert!(!attention_delivery_allowed_at(
            AttentionCondition::Never,
            false,
            1,
            u64::MAX,
        ));
    }

    #[test]
    fn duplicate_focus_lost_does_not_restart_attention_grace() {
        let _lock = env_lock();
        set_terminal_focused(true);
        set_terminal_focused(false);
        let first = UNFOCUSED_SINCE_MS.load(Ordering::SeqCst);
        assert!(first > 0);

        set_terminal_focused(false);
        let duplicate = UNFOCUSED_SINCE_MS.load(Ordering::SeqCst);
        assert_eq!(duplicate, first);

        set_terminal_focused(true);
    }

    /// The gate acts on the emission path itself: a suppressed category
    /// produces zero bytes on every protocol entry point, not just a
    /// filtered list somewhere upstream.
    #[test]
    fn gated_emission_produces_no_bytes() {
        let payload = approval_needed_payload(Locale::En, "bash");

        let quiet = NotificationGate {
            quiet: true,
            ..NotificationGate::default()
        };
        assert!(capture_gated(&payload, quiet).is_empty());

        let no_approvals = NotificationGate {
            approval_needed: false,
            ..NotificationGate::default()
        };
        assert!(capture_gated(&payload, no_approvals).is_empty());

        let out = capture_gated(&payload, NotificationGate::default());
        assert!(!out.is_empty(), "enabled category must still emit");
    }

    #[test]
    fn delivery_outcome_reports_why_nothing_was_sent() {
        let payload = input_needed_payload(Locale::En);
        let mut out = Vec::new();
        assert_eq!(
            DeliveryOutcome::SuppressedByAttention.receipt(),
            "notification not sent: attention policy blocked it"
        );
        assert_eq!(
            notify_done_to(
                Method::Off,
                false,
                &payload,
                Duration::ZERO,
                Duration::ZERO,
                NotificationGate::default(),
                &mut out,
            ),
            DeliveryOutcome::SuppressedByMethod
        );
        assert_eq!(
            DeliveryOutcome::SuppressedByMethod.receipt(),
            "notification not sent: notifications are off"
        );

        assert_eq!(
            notify_done_to(
                Method::Osc9,
                false,
                &payload,
                Duration::from_secs(30),
                Duration::ZERO,
                NotificationGate::default(),
                &mut out,
            ),
            DeliveryOutcome::SuppressedByThreshold
        );

        assert_eq!(
            notify_done_to(
                Method::Osc9,
                false,
                &payload,
                Duration::ZERO,
                Duration::ZERO,
                NotificationGate {
                    quiet: true,
                    ..NotificationGate::default()
                },
                &mut out,
            ),
            DeliveryOutcome::SuppressedByGate
        );
        assert!(out.is_empty());
    }

    /// `settings()` is the single place config reaches the emission path;
    /// it must install the configured gate for `notify_done` to load.
    #[test]
    fn settings_installs_gate_from_config() {
        let _lock = env_lock();
        let _gate_restore = NotificationGateRestore::capture();
        let config: codewhale_runtime::config::Config = toml::from_str(
            r#"
            [notifications]
            quiet = true

            [notifications.events]
            approval-needed = false
            "#,
        )
        .expect("gated notifications config should parse");

        let _ = settings(&config);

        let gate = current_notification_gate();
        assert!(gate.quiet);
        assert!(!gate.approval_needed);
        assert!(gate.turn_complete);
    }

    /// Restores the process-wide configured method after a test mutates it.
    struct ConfiguredMethodRestore(Method);

    impl ConfiguredMethodRestore {
        fn capture() -> Self {
            Self(configured_method())
        }
    }

    impl Drop for ConfiguredMethodRestore {
        fn drop(&mut self) {
            install_configured_method(self.0);
        }
    }

    /// Same single-place contract as the gate: `settings()` must install the
    /// configured `[notifications].method` so the `notify` tool (which has
    /// no method of its own) honors it — including `off` (#1322 promise).
    #[test]
    fn settings_installs_configured_method_from_config() {
        let _lock = env_lock();
        let _method_restore = ConfiguredMethodRestore::capture();
        let off: codewhale_runtime::config::Config = toml::from_str(
            r#"
            [notifications]
            method = "off"
            "#,
        )
        .expect("method=off config should parse");
        let _ = settings(&off);
        assert_eq!(configured_method(), Method::Off);

        let osc9: codewhale_runtime::config::Config = toml::from_str(
            r#"
            [notifications]
            method = "osc9"
            "#,
        )
        .expect("method=osc9 config should parse");
        let _ = settings(&osc9);
        assert_eq!(configured_method(), Method::Osc9);
    }

    /// The installed encoding must round-trip every method so an install/read
    /// pair can never silently fall back to `Auto`.
    #[test]
    fn configured_method_round_trips_every_variant() {
        // Serialized with the tests that deliver through the installed method.
        let _lock = env_lock();
        let _restore = ConfiguredMethodRestore::capture();
        for method in [
            Method::Auto,
            Method::Osc9,
            Method::Bel,
            Method::MacOS,
            Method::Kitty,
            Method::Ghostty,
            Method::Off,
        ] {
            install_configured_method(method);
            assert_eq!(configured_method(), method);
        }
    }

    /// #5041 copy contract: interactive banners lead with the action and
    /// name the subject, instead of a bare "Approval needed".
    #[test]
    fn interactive_banners_are_action_first_and_name_the_subject() {
        let approval = approval_needed_payload(Locale::En, "bash");
        assert_eq!(approval.headline(), "Approve or deny 'bash' to continue");

        let input = input_needed_payload(Locale::En);
        assert_eq!(
            input.headline(),
            "Answer the question in the terminal to continue"
        );

        let elevation = elevation_needed_payload(Locale::En, "bash", "network blocked");
        assert_eq!(
            elevation.headline(),
            "Allow or deny elevated access for 'bash'"
        );
        assert!(elevation.body().contains("network blocked"));
    }

    #[test]
    fn interactive_notification_locales_keep_action_severity_independent_of_tool_text() {
        use crate::tui::app::{StatusToast, StatusToastLevel};
        let _guard = crate::test_support::lock_test_env();
        for &locale in Locale::shipped_complete() {
            let mut app = App::new(
                crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
                &codewhale_runtime::config::Config::default(),
            );
            app.ui_locale = locale;
            let tool = "failed";
            let payloads = [
                (
                    approval_needed_payload(locale, tool),
                    MessageId::NotificationApprovalNeeded,
                ),
                (
                    input_needed_payload(locale),
                    MessageId::NotificationInputNeeded,
                ),
                (
                    elevation_needed_payload(locale, tool, "network-policy"),
                    MessageId::NotificationElevationNeeded,
                ),
            ];
            for (index, (payload, key)) in payloads.into_iter().enumerate() {
                assert_eq!(payload.headline(), tr(locale, key).replace("{tool}", tool));
                app.push_status_toast_record(
                    StatusToast::new(payload.headline(), StatusToastLevel::Warning, Some(12_000))
                        .for_action(format!("request-{index}")),
                );
                app.status_message = Some(payload.headline().into());
                app.sync_status_message_to_toasts();
                assert_eq!(app.status_toasts.len(), index + 1);
                let toast = app.status_toasts.back().unwrap();
                assert_eq!(toast.level, StatusToastLevel::Warning);
                assert_eq!(toast.ttl_ms, Some(12_000));
                assert!(app.sticky_status.is_none());
                let facts = crate::tui::phase_strip::tideline_footer_from_app(&mut app, 500);
                assert_eq!(
                    facts.right,
                    Some((
                        payload.headline().into(),
                        codewhale_palette::ChromeInk::Attention
                    ))
                );
            }
        }
    }

    #[test]
    fn osc9_body_format() {
        let out = capture(Method::Osc9, false, "codewhale: done", 0, 1);
        assert_eq!(out, b"\x1b]9;codewhale: done\x07");
    }

    #[test]
    fn bel_emits_exactly_one_byte() {
        let out = capture(Method::Bel, false, "ignored", 0, 1);
        assert_eq!(out, b"\x07");
    }

    #[test]
    fn off_mode_emits_nothing() {
        let out = capture(Method::Off, false, "ignored", 0, 9999);
        assert!(out.is_empty());
    }

    /// #4847 follow-up: OSC 9;4 and OSC 0 are *control* bytes, not content.
    /// A terminal renders nothing visible; a pipe, a file, or a CI log renders
    /// them literally — which is why `cargo test` output carried stray
    /// `]9;4;1]0;` noise. The write is now gated on stdout being a TTY; these
    /// assertions pin the bytes themselves so the gate cannot be "fixed" by
    /// quietly changing what gets emitted.
    #[test]
    fn control_sequences_have_the_exact_documented_bytes() {
        assert_eq!(taskbar_progress_sequence(1, None), "\x1b]9;4;1\x07");
        assert_eq!(taskbar_progress_sequence(1, Some(42)), "\x1b]9;4;1;42\x07");
        assert_eq!(taskbar_progress_sequence(0, None), "\x1b]9;4;0\x07");
        assert_eq!(
            terminal_title_sequence("🐳 in the current…"),
            "\x1b]0;🐳 in the current…\x07"
        );
    }

    #[test]
    fn terminal_title_sequence_strips_control_and_bidi_injection() {
        assert_eq!(
            terminal_title_sequence("safe\u{1b}]2;owned\u{7}\u{202e}title"),
            "\x1b]0;safe]2;ownedtitle\x07"
        );
        let oversized = "x".repeat(MAX_TERMINAL_TITLE_CHARS + 20);
        assert_eq!(
            terminal_title_sequence(&oversized),
            format!("\x1b]0;{}\x07", "x".repeat(MAX_TERMINAL_TITLE_CHARS))
        );
    }

    #[test]
    fn terminal_title_sequence_strips_zero_width_and_bidi_marks_but_keeps_cjk() {
        // C1 controls (0x9C ST, 0x9D OSC), zero-width joiners/spaces, bidi
        // marks and isolates, BOM, soft hyphen, and line separators are all
        // dropped; CJK, emoji, and ordinary punctuation survive untouched.
        assert_eq!(
            terminal_title_sequence(
                "会\u{9d}0;議\u{9c}\u{200b}A\u{200f}B\u{061c}C\u{2066}D\u{2069}\u{feff}E\u{00ad}F\u{2028}G 🐳!"
            ),
            "\x1b]0;会0;議ABCDEFG 🐳!\x07"
        );
        // Length is bounded by chars, so a CJK title keeps whole characters.
        let cjk = "漢".repeat(MAX_TERMINAL_TITLE_CHARS + 5);
        assert_eq!(
            terminal_title_sequence(&cjk),
            format!("\x1b]0;{}\x07", "漢".repeat(MAX_TERMINAL_TITLE_CHARS))
        );
    }

    #[test]
    fn title_prefix_change_at_rest_repaints_the_resting_title() {
        let _guard = prefix_lock();
        TITLE_ANIMATION_RUNNING.store(false, Ordering::SeqCst);
        COMPLETION_MARKER_SHOWN.store(false, Ordering::SeqCst);
        set_title_prefix(Some("Alpha"));
        assert_eq!(decorate_title(resting_title_body()), "[Alpha] codewhale");
        COMPLETION_MARKER_SHOWN.store(true, Ordering::SeqCst);
        assert_eq!(decorate_title(resting_title_body()), "[Alpha] ✓ done");
        COMPLETION_MARKER_SHOWN.store(false, Ordering::SeqCst);
        set_title_prefix(None);
        assert_eq!(decorate_title(resting_title_body()), "codewhale");
    }

    #[test]
    fn kitty_escape_uses_st_terminator() {
        let out = capture(Method::Kitty, false, "done", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("99;"), "should have kitty OSC 99");
        assert!(s.contains("\x1b\\"), "kitty uses ST terminator");
        assert!(!s.contains("\x07"), "kitty should NOT use BEL");
    }

    #[test]
    fn ghostty_escape_format() {
        let out = capture(Method::Ghostty, false, "done", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("777;notify;codewhale;done"),
            "should have ghostty seq"
        );
    }

    #[test]
    fn kitty_tmux_dcs_passthrough() {
        let out = capture(Method::Kitty, true, "hello", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;"), "should start with DCS");
        assert!(s.ends_with("\x1b\\"), "should end with ST");
    }

    #[test]
    fn ghostty_tmux_dcs_passthrough() {
        let out = capture(Method::Ghostty, true, "hello", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;"), "should start with DCS");
        assert!(s.ends_with("\x1b\\"), "should end with ST");
    }

    #[test]
    fn below_threshold_emits_nothing() {
        let out = capture(Method::Osc9, false, "msg", 30, 29);
        assert!(out.is_empty());
    }

    #[test]
    fn at_threshold_emits() {
        let out = capture(Method::Osc9, false, "msg", 30, 30);
        assert!(!out.is_empty());
    }

    /// The subtitle is the localized status headline and the body is
    /// everything else. Previously this was re-derived by splitting a
    /// free-form string on its first newline; now it is a projection of
    /// the typed payload, so the split cannot drift from what the
    /// composer intended (#4834).
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_notification_keeps_localized_status_as_subtitle() {
        let payload = NotificationPayload::turn_complete("ターン完了 (1m 5s)")
            .with_preview(Some("完了しました。"));

        let (subtitle, body) = macos_notification_parts(&payload);

        assert_eq!(subtitle, "ターン完了 (1m 5s)");
        assert_eq!(body, "完了しました。");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_banner_does_not_smuggle_in_an_independent_sound() {
        assert!(!MACOS_DISPLAY_NOTIFICATION_SCRIPT.contains("sound name"));
        assert_eq!(
            MACOS_DISPLAY_NOTIFICATION_SCRIPT,
            "display notification theBody with title \"Codewhale\" subtitle theSubtitle"
        );
    }

    /// The preview is capped at `PREVIEW_MAX_CHARS` *inclusive* of the
    /// ellipsis, so the string handed to `osascript` never exceeds the
    /// declared bound.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_notification_truncates_preview() {
        let payload = NotificationPayload::turn_complete("Turn complete")
            .with_preview(Some(&"assistant preview ".repeat(40)));

        let (subtitle, body) = macos_notification_parts(&payload);

        assert_eq!(subtitle, "Turn complete");
        assert!(body.starts_with("assistant preview"));
        assert!(body.ends_with("..."));
        assert_eq!(
            body.chars().count(),
            codewhale_notify::payload::PREVIEW_MAX_CHARS
        );
    }

    /// #4834: an approval banner is the one place a raw shell command
    /// used to reach Notification Center. Pin the macOS projection, not
    /// just the payload, so a future refactor of either half is caught.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_approval_notification_never_carries_the_command() {
        let payload = NotificationPayload::approval_needed("Approval needed", "bash");

        let (subtitle, body) = macos_notification_parts(&payload);

        assert_eq!(subtitle, "Approval needed");
        assert_eq!(body, "bash");
    }

    #[test]
    fn tmux_dcs_passthrough_wraps_osc9() {
        let out = capture(Method::Osc9, true, "hello", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.starts_with("\x1bPtmux;"),
            "should start with DCS passthrough"
        );
        assert!(s.ends_with("\x1b\\"), "should end with ST");
        assert!(s.contains("hello"), "should contain message");
    }

    #[test]
    fn auto_detect_picks_osc9_for_iterm() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "iTerm.app") };
        let resolved = resolve_method();
        // Restore previous value.
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// Cmux in typical configurations does not set `TERM_PROGRAM`; it sets
    /// `LC_TERMINAL=Cmux` instead. Verify the `LC_TERMINAL` fallback probe
    /// correctly resolves to `Osc9`.
    #[test]
    fn auto_detect_picks_osc9_for_cmux_via_lc_terminal() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::set_var("LC_TERMINAL", "Cmux");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// `LC_TERMINAL` should also match other OSC-9 capable terminals in case
    /// they set it in addition to or instead of `TERM_PROGRAM`.
    #[test]
    fn auto_detect_picks_osc9_for_wezterm_via_lc_terminal() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::set_var("LC_TERMINAL", "WezTerm");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_stays_silent_for_unknown_on_unix() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // SAFETY: test-only; serialised by env_lock().
        // Clear LC_TERMINAL and TERM so the fallback probes don't
        // accidentally pick up an OSC-9 / Kitty / Ghostty capable
        // terminal from the test runner environment.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "xterm-256color");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-256color");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Off);
    }

    /// Unknown Windows terminals must not turn an automatic banner request
    /// into an audible system sound.
    #[test]
    #[cfg(target_os = "windows")]
    fn auto_detect_stays_silent_for_unknown_on_windows() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "Windows Terminal") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Off);
    }

    /// #583: known OSC-9 terminals must still resolve to `Osc9` on
    /// Windows — the off-fallback only applies to unrecognised
    /// `TERM_PROGRAM`. The cross-platform iTerm test above is a thin
    /// proxy because iTerm itself only runs on macOS; if the WezTerm
    /// arm of the match silently disappeared, that test would still
    /// pass on the Windows runner and we'd lose the WezTerm-on-Windows
    /// compatibility guarantee. Pin it directly.
    #[test]
    #[cfg(target_os = "windows")]
    fn auto_detect_picks_osc9_for_wezterm_on_windows() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "WezTerm") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// Ghostty-based terminals (cmux, etc.) may not set
    /// `TERM_PROGRAM` but do set `TERM=xterm-ghostty`. The `$TERM`
    /// fallback should catch them.
    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_picks_osc9_for_xterm_ghostty_term_fallback() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // Simulate a Ghostty-based terminal that only sets TERM.
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-ghostty");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// Ghostty now has its own protocol (OSC 777).
    #[test]
    fn auto_detect_picks_ghostty_from_term_program() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "Ghostty") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Ghostty);
    }

    #[test]
    fn auto_detect_picks_kitty_from_term_program() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "kitty") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Kitty);
    }

    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_picks_kitty_from_term_fallback() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-kitty");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Kitty);
    }

    /// When neither `TERM_PROGRAM` nor `TERM` suggests a known capable
    /// terminal, automatic delivery fails closed rather than ringing BEL.
    ///
    /// On macOS the `MacOS` method takes priority, so this test is
    /// excluded there.
    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_falls_back_to_off_for_unrelated_term() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-256color");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Off);
    }
}

// ---------------------------------------------------------------------------
// Tideline notifications inbox (spec §5a "Notifications inbox"): the
// attention surface that replaces the toast soup. Records are typed — the
// same `NotificationKind` disclosure policy the desktop payloads use — and
// the unread mark is the sanctioned gold ◆. Translation scaffolding in the
// topbar mold: a pure deterministic widget over injected records (`App`
// projects `status_toasts`/`sticky_status` into it at the landing slice);
// not wired into `ui/frame.rs` (#5698 gate).

#[cfg(test)]
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
use codewhale_palette::{ChromeInk, UiTheme, chrome_style};

/// One attention record: a typed projection of a status toast / sticky
/// status / desktop payload. `at` is an injected clock string so renders
/// stay deterministic (spec §5a: caller owns the wall clock).
#[derive(Debug, Clone)]
#[cfg(test)] // translation scaffolding: wired by the landing slice
pub struct TidelineInboxRecord {
    pub kind: NotificationKind,
    pub title: String,
    /// One-line body, already disclosure-approved per kind.
    pub body: Option<String>,
    /// Wall-clock label, e.g. `14:42`.
    pub at: String,
    pub read: bool,
}

#[cfg(test)]
impl TidelineInboxRecord {
    /// Kind word — a noun, never "Error" (spec §7 failure microcopy rule).
    #[must_use]
    pub fn kind_word(&self) -> &'static str {
        match self.kind {
            NotificationKind::TurnComplete => "turn done",
            NotificationKind::SubagentTerminal => "whale done",
            NotificationKind::BackgroundTerminal => "work done",
            NotificationKind::ApprovalNeeded => "approval",
            NotificationKind::InputNeeded => "question",
            NotificationKind::ElevationNeeded => "sandbox",
            NotificationKind::ModelNotify => "notify",
        }
    }

    /// Per-kind ink per the §5d table: interactive asks read as cognition
    /// (permission family), completions as outcome, terminal whales as info.
    #[must_use]
    pub fn kind_ink(&self) -> ChromeInk {
        match self.kind {
            NotificationKind::TurnComplete => ChromeInk::Outcome,
            NotificationKind::SubagentTerminal | NotificationKind::BackgroundTerminal => {
                ChromeInk::Info
            }
            NotificationKind::ApprovalNeeded | NotificationKind::InputNeeded => {
                ChromeInk::PermissionAsk
            }
            NotificationKind::ElevationNeeded => ChromeInk::PermissionFullAccess,
            NotificationKind::ModelNotify => ChromeInk::MetadataValue,
        }
    }
}

/// What the caller owes the inbox render.
#[cfg(test)] // translation scaffolding: wired by the landing slice
pub struct TidelineInbox<'a> {
    pub theme: &'a UiTheme,
    pub records: &'a [TidelineInboxRecord],
    /// Selected row (Enter inspects, `r` marks read, Esc backs out).
    pub selected: usize,
    pub ascii_safe: bool,
}

#[cfg(test)] // translation scaffolding: builder methods feed tests + the landing slice
impl<'a> TidelineInbox<'a> {
    #[must_use]
    pub fn new(theme: &'a UiTheme, records: &'a [TidelineInboxRecord]) -> Self {
        Self {
            theme,
            records,
            selected: 0,
            ascii_safe: false,
        }
    }

    #[must_use]
    pub fn selected(mut self, selected: usize) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn ascii_safe(mut self, ascii_safe: bool) -> Self {
        self.ascii_safe = ascii_safe;
        self
    }

    fn sym(&self, glyph: &str) -> String {
        if !self.ascii_safe {
            return glyph.to_string();
        }
        if let Some(fb) = codewhale_tui_kit::glyphs::ascii_fallback(glyph) {
            return fb.to_string();
        }
        glyph
            .chars()
            .map(|c| {
                codewhale_tui_kit::glyphs::ascii_fallback(&c.to_string())
                    .map(str::to_string)
                    .unwrap_or_else(|| c.to_string())
            })
            .collect()
    }
}

#[cfg(test)]
fn chrome(theme: &UiTheme, ink: ChromeInk) -> Style {
    chrome_style(theme, ink)
}

#[cfg(test)]
fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) {
    buf.set_stringn(x, y, text, text.width(), style);
}

/// Paint the notifications inbox: header row (count of unread), then one
/// row per record — unread gold ◆, read hollow ○, selected `▸`, kind word,
/// title, injected time. Truncates, never wraps.
#[cfg(test)] // translation scaffolding: wired by the landing slice
pub fn render_tideline_inbox(area: Rect, buf: &mut Buffer, inbox: &TidelineInbox<'_>) {
    if area.width < 8 || area.height < 2 {
        return;
    }
    let theme = inbox.theme;
    let unread = inbox.records.iter().filter(|record| !record.read).count();
    let header = if unread == 0 {
        "NOTIFICATIONS".to_string()
    } else {
        format!("NOTIFICATIONS · {unread} unread")
    };
    put(
        buf,
        area.x,
        area.y,
        &header,
        chrome(theme, ChromeInk::Metadata).add_modifier(Modifier::BOLD),
    );

    if inbox.records.is_empty() {
        put(
            buf,
            area.x,
            area.y + 1,
            "quiet water — nothing needs you",
            chrome(theme, ChromeInk::MetadataHint),
        );
        return;
    }

    let width = area.width as usize;
    let mut y = area.y + 1;
    for (index, record) in inbox.records.iter().enumerate() {
        if y >= area.y + area.height {
            break;
        }
        let selected = inbox.selected == index;
        let marker = if selected { "▸ " } else { "  " };
        let mark = if record.read { "○" } else { "◆" };
        let mark_ink = if record.read {
            ChromeInk::MetadataDim
        } else {
            ChromeInk::Attention
        };
        let row = format!("{} {} — {}", record.kind_word(), record.title, record.at);
        let row = truncate_to_width_owned(&inbox.sym(&row), width.saturating_sub(6));
        put(
            buf,
            area.x + 2,
            y,
            &inbox.sym(marker),
            chrome(theme, ChromeInk::Identity),
        );
        put(
            buf,
            area.x + 4,
            y,
            &inbox.sym(mark),
            chrome(theme, mark_ink),
        );
        let mut style = chrome(theme, record.kind_ink());
        if record.read {
            style = chrome(theme, ChromeInk::MetadataDim);
        }
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        put(buf, area.x + 6, y, &row, style);
        // The selected record's approved body earns its own indented row —
        // the inspect affordance; other bodies stay collapsed.
        if selected
            && let Some(body) = record.body.as_deref()
            && y + 1 < area.y + area.height
        {
            put(
                buf,
                area.x + 8,
                y + 1,
                &truncate_to_width_owned(&inbox.sym(body), width.saturating_sub(10)),
                chrome(theme, ChromeInk::MetadataHint),
            );
            y += 1;
        }
        y += 1;
    }
}

#[cfg(test)]
fn truncate_to_width_owned(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// Row hitboxes for one render (spec §6): one rect per record, matching the
/// painted rows exactly — the selected record's body row belongs to its
/// rect. Must be called with the same inputs as [`render_tideline_inbox`].
#[must_use]
#[cfg(test)] // translation scaffolding: wired by the landing slice
pub fn tideline_inbox_hitboxes(area: Rect, inbox: &TidelineInbox<'_>) -> Vec<Rect> {
    let mut out = Vec::new();
    if area.width < 8 || area.height < 2 {
        return out;
    }
    let mut y = area.y + 1;
    for (index, record) in inbox.records.iter().enumerate() {
        let mut height = 1;
        if inbox.selected == index && record.body.is_some() {
            height = 2;
        }
        if y + height > area.y + area.height {
            break;
        }
        out.push(Rect {
            x: area.x + 2,
            y,
            width: area.width.saturating_sub(2),
            height,
        });
        y += height;
    }
    out
}

#[cfg(test)]
mod tideline_tests;

#[cfg(test)]
mod unified_audio_tests {
    use super::*;
    use codewhale_config::notifications::{
        CompletionSound, NotificationConfigUpdate, NotificationsConfig,
    };
    use codewhale_notify::audio::AudioOutcome;
    use codewhale_notify::sound_policy::{self, EventSoundPolicy, SoundCue, SoundDecision};

    fn payloads() -> [NotificationPayload; 6] {
        [
            NotificationPayload::turn_complete("done"),
            NotificationPayload::subagent_terminal("done", "a"),
            NotificationPayload::approval_needed("approve", "shell"),
            NotificationPayload::input_needed("answer"),
            NotificationPayload::elevation_needed("access", "shell", "denied"),
            NotificationPayload::model_notify("notice", None),
        ]
    }

    #[test]
    fn every_gate_blocks_terminal_native_audio_and_the_repeat_clock() {
        for payload in payloads() {
            let mut disabled = NotificationsConfig::default();
            disabled
                .apply_update(NotificationConfigUpdate::Event(
                    sound_policy::event_for_kind(payload.kind()),
                    false,
                ))
                .unwrap();
            for method in [Method::Kitty, Method::MacOS, Method::Bel] {
                for (selected, gate, attention, threshold, expected) in [
                    (
                        Method::Off,
                        NotificationGate::default(),
                        true,
                        Duration::ZERO,
                        DeliveryOutcome::SuppressedByMethod,
                    ),
                    (
                        method,
                        NotificationGate {
                            quiet: true,
                            ..Default::default()
                        },
                        true,
                        Duration::ZERO,
                        DeliveryOutcome::SuppressedByGate,
                    ),
                    (
                        method,
                        NotificationGate::from_config(&disabled),
                        true,
                        Duration::ZERO,
                        DeliveryOutcome::SuppressedByGate,
                    ),
                    (
                        method,
                        NotificationGate::default(),
                        false,
                        Duration::ZERO,
                        DeliveryOutcome::SuppressedByAttention,
                    ),
                    (
                        method,
                        NotificationGate::default(),
                        true,
                        Duration::from_secs(2),
                        DeliveryOutcome::SuppressedByThreshold,
                    ),
                ] {
                    let mut out = Vec::new();
                    let result = notify_with_sinks(
                        selected,
                        false,
                        &payload,
                        threshold,
                        Duration::from_secs(1),
                        gate,
                        attention,
                        &mut out,
                        &mut |_, _| panic!("suppressed event reached sound decision"),
                        &mut |_, _| panic!("suppressed event reached audio"),
                        &mut |_| panic!("suppressed event reached native banner"),
                    );
                    assert_eq!(result, expected);
                    assert!(out.is_empty());
                }
            }
        }
    }

    #[test]
    fn explicit_bell_transport_and_selected_whale_emit_only_one_cue() {
        for payload in payloads() {
            let mut policy = EventSoundPolicy::from_config(&NotificationsConfig {
                sound: Some(CompletionSound::Whale),
                ..Default::default()
            });
            let mut out = Vec::new();
            let mut cues = Vec::new();
            let result = notify_with_sinks(
                Method::Bel,
                false,
                &payload,
                Duration::ZERO,
                Duration::ZERO,
                NotificationGate::default(),
                true,
                &mut out,
                &mut |kind, bell| policy.decide(sound_policy::event_for_kind(kind), 0, bell),
                &mut |cue, _| {
                    cues.push(cue.clone());
                    AudioOutcome::Dispatched
                },
                &mut |_| panic!("audio-only transport reached native banner"),
            );
            assert_eq!(result, DeliveryOutcome::Dispatched(Method::Bel));
            assert_eq!(cues, [SoundCue::Whale]);
            assert!(out.is_empty(), "no second transport BEL");
        }
    }

    #[test]
    fn audio_off_keeps_banner_but_silences_bell_transport() {
        for method in [Method::Kitty, Method::Bel] {
            let mut policy = EventSoundPolicy::from_config(&NotificationsConfig {
                sound: Some(CompletionSound::Off),
                completion_sound: CompletionSound::Bell,
                ..Default::default()
            });
            let mut out = Vec::new();
            let result = notify_with_sinks(
                method,
                false,
                &NotificationPayload::turn_complete("done"),
                Duration::ZERO,
                Duration::ZERO,
                NotificationGate::default(),
                true,
                &mut out,
                &mut |kind, bell| policy.decide(sound_policy::event_for_kind(kind), 0, bell),
                &mut |_, _| panic!("off reached audio"),
                &mut |_| panic!("unexpected native"),
            );
            assert_eq!(
                result,
                if method == Method::Bel {
                    DeliveryOutcome::SuppressedBySound
                } else {
                    DeliveryOutcome::Delivered(method)
                }
            );
            assert!(!out.contains(&7));
        }
    }

    #[test]
    fn unsupported_failed_and_busy_audio_have_truthful_receipts_without_fallback() {
        for (audio_result, expected) in [
            (
                AudioOutcome::Unsupported,
                DeliveryOutcome::UnsupportedTransport,
            ),
            (AudioOutcome::Failed, DeliveryOutcome::DeliveryFailed),
            (AudioOutcome::Busy, DeliveryOutcome::SuppressedBySound),
        ] {
            let mut out = Vec::new();
            let mut count = 0;
            let result = notify_with_sinks(
                Method::Bel,
                false,
                &NotificationPayload::input_needed("answer"),
                Duration::ZERO,
                Duration::ZERO,
                NotificationGate::default(),
                true,
                &mut out,
                &mut |_, _| SoundDecision::Play(SoundCue::Whale),
                &mut |_, _| {
                    count += 1;
                    audio_result
                },
                &mut |_| panic!("unexpected native"),
            );
            assert_eq!(result, expected);
            assert_eq!(count, 1);
            assert!(out.is_empty());
        }
    }

    #[test]
    fn native_failure_and_terminal_failure_do_not_trigger_orphan_audio() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for method in [Method::Kitty, Method::MacOS] {
            let result = notify_with_sinks(
                method,
                false,
                &NotificationPayload::input_needed("answer"),
                Duration::ZERO,
                Duration::ZERO,
                NotificationGate::default(),
                true,
                &mut Broken,
                &mut |_, _| panic!("failed delivery reached policy"),
                &mut |_, _| panic!("failed delivery reached audio"),
                &mut |_| DeliveryOutcome::DeliveryFailed,
            );
            assert_eq!(result, DeliveryOutcome::DeliveryFailed);
        }
    }

    #[test]
    fn native_worker_receipt_does_not_claim_os_acceptance() {
        let mut out = Vec::new();
        let mut cues = 0;
        let result = notify_with_sinks(
            Method::MacOS,
            false,
            &NotificationPayload::input_needed("answer"),
            Duration::ZERO,
            Duration::ZERO,
            NotificationGate::default(),
            true,
            &mut out,
            &mut |_, _| SoundDecision::Play(SoundCue::Whale),
            &mut |_, _| {
                cues += 1;
                AudioOutcome::Dispatched
            },
            &mut |_| DeliveryOutcome::Dispatched(Method::MacOS),
        );
        assert_eq!(result.receipt(), "notification dispatch attempted");
        assert_eq!(cues, 1);
        assert!(out.is_empty());
    }

    #[test]
    fn failed_audio_never_upgrades_native_dispatch_evidence() {
        let mut out = Vec::new();
        let result = notify_with_sinks(
            Method::MacOS,
            false,
            &NotificationPayload::input_needed("answer"),
            Duration::ZERO,
            Duration::ZERO,
            NotificationGate::default(),
            true,
            &mut out,
            &mut |_, _| SoundDecision::Play(SoundCue::Whale),
            &mut |_, _| AudioOutcome::Failed,
            &mut |_| DeliveryOutcome::Dispatched(Method::MacOS),
        );
        assert_eq!(
            result.receipt(),
            "notification dispatch attempted; sound unavailable"
        );
    }

    #[test]
    fn title_completion_is_only_a_visual_marker_and_cannot_consume_audio() {
        let _guard = crate::test_support::lock_test_env();
        sound_policy::configure(EventSoundPolicy::from_config(&NotificationsConfig {
            sound: Some(CompletionSound::Whale),
            ..Default::default()
        }));
        let mut title = String::new();
        stop_title_animation_with(|value| title = value.to_string());
        assert!(title.contains("✓ done"));
        assert_eq!(
            sound_policy::decide(NotificationKind::TurnComplete, 0, false),
            SoundDecision::Play(SoundCue::Whale)
        );
        sound_policy::configure(EventSoundPolicy::default());
        COMPLETION_MARKER_SHOWN.store(false, Ordering::SeqCst);
    }
}
