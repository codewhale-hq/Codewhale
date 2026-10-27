//! Notification policy shared by every host: the typed payload, the
//! delivery-method and category gate, attention (focus) rules, the
//! per-event sound policy and audio cues.
//!
//! This crate owns *what* may notify and *which* sound it selects. The
//! terminal UI owns the *terminal*: `tui::notifications` is the only code that
//! hands a real terminal to [`delivery::notify_with_sinks`], writes the taskbar
//! progress sequence or the window title. Runtime callers that need a delivery
//! (the `notify` tool, headless exec) reach it through `codewhale_runtime::host_terminal`,
//! never by importing the TUI.
//!
//! Known limitation: [`delivery::notify_with_sinks`] builds the escape bytes
//! for every transport but writes only to the sink it is handed; the runtime
//! API's native notification preparation hands it a null sink and never
//! reaches a terminal transport.

pub mod audio;
pub mod delivery;
pub mod payload;
pub mod sound_policy;

use std::time::Duration;

use codewhale_config::notifications::{
    NotificationCondition, NotificationMethod, NotificationsConfig,
};
use payload::NotificationKind;

/// Notification delivery method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// Automatically pick the best protocol for the current terminal.
    /// See [`delivery::resolve_method`] for the canonical resolution table.
    #[default]
    Auto,
    /// OSC 9 escape: `\x1b]9;<msg>\x07`
    Osc9,
    /// Plain BEL character: `\x07`
    Bel,
    /// macOS Notification Center via `osascript`.
    ///
    /// Only reachable through [`Method::Auto`], and only on the macOS
    /// terminals that expose no notification escape of their own (Apple
    /// Terminal, the VS Code and JetBrains embedded terminals, plain tmux
    /// without `LC_TERMINAL`). iTerm2, WezTerm, Ghostty, and kitty are
    /// matched earlier in [`delivery::resolve_method`] and never get here.
    ///
    /// Known limitation (#4834): `display notification` is a Standard
    /// Additions command, so the banner is attributed to the *bundled*
    /// host process. `/usr/bin/osascript` is unbundled, so macOS credits
    /// `com.apple.ScriptEditor2` — which is what supplies the Script
    /// Editor icon and owns the System Settings → Notifications entry
    /// (alert style, previews, Do Not Disturb). `display notification`
    /// takes no icon parameter; fixing the attribution requires shipping
    /// a real `.app` bundle, not a change in this file.
    MacOS,
    /// Kitty notification protocol (OSC 99) with ST terminator.
    /// Uses `ESC ] 99 ; params ST` — no audible beep, unlike BEL.
    Kitty,
    /// Ghostty notification protocol (OSC 777).
    /// Uses `ESC ] 777 ; notify ; title ; message BEL`.
    Ghostty,
    /// Suppress all notifications.
    Off,
}

/// Truthful result from one notification delivery attempt.
///
/// Callers that surface a receipt (notably the model-facing `notify` tool)
/// use this instead of claiming a notification was sent when user policy,
/// focus, or the configured delivery method suppressed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The notification was handed to the resolved transport.
    Delivered(Method),
    /// A background OS/audio worker was started; acceptance is unverified.
    Dispatched(Method),
    /// Banner bytes were sent, but the selected audio could not be dispatched.
    DeliveredWithoutSound(Method),
    /// A native dispatch was attempted, but selected audio was unavailable.
    DispatchedWithoutSound(Method),
    /// The bell-only transport has no authorized cue (off or rate limited).
    SuppressedBySound,
    /// The terminal is still in the foreground, or has only just lost focus.
    SuppressedByAttention,
    /// The event completed before the configured duration threshold.
    SuppressedByThreshold,
    /// Notification delivery is explicitly disabled.
    SuppressedByMethod,
    /// Quiet mode or the per-event allow-list suppressed this category.
    SuppressedByGate,
    /// The selected terminal protocol produced no transport bytes.
    UnsupportedTransport,
    /// The terminal transport could not be written.
    DeliveryFailed,
}

impl DeliveryOutcome {
    /// Short, stable receipt text for command/tool surfaces.
    #[must_use]
    pub fn receipt(self) -> &'static str {
        match self {
            Self::Delivered(_) => "notification sent",
            Self::Dispatched(_) => "notification dispatch attempted",
            Self::DeliveredWithoutSound(_) => "notification sent; sound unavailable",
            Self::DispatchedWithoutSound(_) => "notification dispatch attempted; sound unavailable",
            Self::SuppressedBySound => "notification not sent: sound is off or rate limited",
            Self::SuppressedByAttention => "notification not sent: attention policy blocked it",
            Self::SuppressedByThreshold => "notification not sent: below the duration threshold",
            Self::SuppressedByMethod => "notification not sent: notifications are off",
            Self::SuppressedByGate => {
                "notification not sent: quiet mode or event settings blocked it"
            }
            Self::UnsupportedTransport => {
                "notification not sent: terminal transport is unsupported"
            }
            Self::DeliveryFailed => "notification not sent: terminal delivery failed",
        }
    }
}

// ── Notification gate (#5041) ────────────────────────────────────────
//
// One policy switchboard between "an event happened" and "the user's
// desktop is interrupted". `[notifications].quiet` silences every
// category; `[notifications.events]` disables individual categories. The
// gate is installed from config by `tui::notifications::settings` and consulted by
// `tui::notifications::notify_done` ahead of every delivery mechanism, so a disabled
// category can never leak through one specific protocol.

/// Which notification categories may reach the user's desktop.
///
/// The category set mirrors [`NotificationKind`] one-to-one. Default:
/// everything enabled, quiet off — matching the pre-#5041 behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationGate {
    /// Suppress every category when `true` (`[notifications].quiet`).
    pub quiet: bool,
    pub turn_complete: bool,
    pub subagent_terminal: bool,
    pub approval_needed: bool,
    pub input_needed: bool,
    pub elevation_needed: bool,
    pub model_notify: bool,
}

impl Default for NotificationGate {
    fn default() -> Self {
        Self {
            quiet: false,
            turn_complete: true,
            subagent_terminal: true,
            approval_needed: true,
            input_needed: true,
            elevation_needed: true,
            model_notify: true,
        }
    }
}

impl NotificationGate {
    /// Project the `[notifications]` config block onto a gate.
    #[must_use]
    pub fn from_config(notif: &NotificationsConfig) -> Self {
        Self {
            quiet: notif.quiet,
            turn_complete: notif.events.turn_complete,
            subagent_terminal: notif.events.subagent_terminal,
            approval_needed: notif.events.approval_needed,
            input_needed: notif.events.input_needed,
            elevation_needed: notif.events.elevation_needed,
            model_notify: notif.events.model_notify,
        }
    }

    /// Whether an event of `kind` may be delivered under this gate.
    #[must_use]
    pub fn allows(self, kind: NotificationKind) -> bool {
        if self.quiet {
            return false;
        }
        match kind {
            NotificationKind::TurnComplete => self.turn_complete,
            NotificationKind::SubagentTerminal | NotificationKind::BackgroundTerminal => {
                self.subagent_terminal
            }
            NotificationKind::ApprovalNeeded => self.approval_needed,
            NotificationKind::InputNeeded => self.input_needed,
            NotificationKind::ElevationNeeded => self.elevation_needed,
            NotificationKind::ModelNotify => self.model_notify,
        }
    }

    const QUIET_BIT: u8 = 1 << 0;
    const TURN_COMPLETE_BIT: u8 = 1 << 1;
    const SUBAGENT_TERMINAL_BIT: u8 = 1 << 2;
    const APPROVAL_NEEDED_BIT: u8 = 1 << 3;
    const INPUT_NEEDED_BIT: u8 = 1 << 4;
    const ELEVATION_NEEDED_BIT: u8 = 1 << 5;
    const MODEL_NOTIFY_BIT: u8 = 1 << 6;

    pub const fn to_bits(self) -> u8 {
        (self.quiet as u8 * Self::QUIET_BIT)
            | (self.turn_complete as u8 * Self::TURN_COMPLETE_BIT)
            | (self.subagent_terminal as u8 * Self::SUBAGENT_TERMINAL_BIT)
            | (self.approval_needed as u8 * Self::APPROVAL_NEEDED_BIT)
            | (self.input_needed as u8 * Self::INPUT_NEEDED_BIT)
            | (self.elevation_needed as u8 * Self::ELEVATION_NEEDED_BIT)
            | (self.model_notify as u8 * Self::MODEL_NOTIFY_BIT)
    }

    pub const fn from_bits(bits: u8) -> Self {
        Self {
            quiet: bits & Self::QUIET_BIT != 0,
            turn_complete: bits & Self::TURN_COMPLETE_BIT != 0,
            subagent_terminal: bits & Self::SUBAGENT_TERMINAL_BIT != 0,
            approval_needed: bits & Self::APPROVAL_NEEDED_BIT != 0,
            input_needed: bits & Self::INPUT_NEEDED_BIT != 0,
            elevation_needed: bits & Self::ELEVATION_NEEDED_BIT != 0,
            model_notify: bits & Self::MODEL_NOTIFY_BIT != 0,
        }
    }
}

/// Attention delivery policy installed from the resolved notification config.
///
/// The default is background-only. A newly started TUI is treated as focused
/// until the terminal explicitly reports `FocusLost`, so the safe startup
/// behavior is silence rather than an unexpected banner or bell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionCondition {
    Always = 0,
    Unfocused = 1,
    Never = 2,
}

pub const DEFAULT_UNFOCUSED_GRACE: Duration = Duration::from_secs(2);
#[must_use]
pub fn attention_delivery_allowed_at(
    condition: AttentionCondition,
    focused: bool,
    unfocused_since_ms: u64,
    now_ms: u64,
) -> bool {
    match condition {
        AttentionCondition::Always => true,
        AttentionCondition::Never => false,
        AttentionCondition::Unfocused => {
            !focused
                && unfocused_since_ms > 0
                && now_ms.saturating_sub(unfocused_since_ms)
                    >= DEFAULT_UNFOCUSED_GRACE.as_millis() as u64
        }
    }
}

/// Native hosts provide focus observations; the same grace/condition rule
/// applies before either native sound or banner preparation.
pub fn native_attention_allowed(
    config: &NotificationsConfig,
    focused: bool,
    unfocused_for: Duration,
) -> bool {
    let condition = match config.condition.unwrap_or(NotificationCondition::Unfocused) {
        NotificationCondition::Always => AttentionCondition::Always,
        NotificationCondition::Unfocused => AttentionCondition::Unfocused,
        NotificationCondition::Never => AttentionCondition::Never,
    };
    let elapsed = unfocused_for.as_millis().min(u128::from(u64::MAX - 1)) as u64;
    attention_delivery_allowed_at(condition, focused, 1, elapsed + 1)
}

/// Resolve the effective notification method/threshold/include-summary tuple
/// for a completed turn, taking the high-level
/// `[tui].notification_condition` override into account on top of the
/// lower-level `[notifications]` block.
///
/// Returns `None` only when the high-level attention policy is `never`.
/// `Method::Off` remains a valid projection so the event gate can report
/// that both banner and sound are disabled.
#[must_use]
pub fn settings_projection(notif: &NotificationsConfig) -> Option<(Method, Duration, bool)> {
    let method = match notif.method {
        NotificationMethod::Auto => Method::Auto,
        NotificationMethod::Osc9 => Method::Osc9,
        NotificationMethod::Bel => Method::Bel,
        NotificationMethod::Kitty => Method::Kitty,
        NotificationMethod::Ghostty => Method::Ghostty,
        NotificationMethod::Off => Method::Off,
    };
    match notif.condition.unwrap_or(NotificationCondition::Unfocused) {
        NotificationCondition::Always => Some((method, Duration::ZERO, notif.include_summary)),
        NotificationCondition::Unfocused => Some((
            method,
            Duration::from_secs(notif.threshold_secs),
            notif.include_summary,
        )),
        NotificationCondition::Never => None,
    }
}
