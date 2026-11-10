//! Prototype boundary values used by the command capability shapes.
//!
//! These types deliberately do not replace the current TUI-owned production
//! types in FEAT-014. During the in-place adoption stage, thin TUI adapters
//! convert between existing application values and these boundary values.

/// Stable provider identity at the command boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProviderId(pub String);

// Slice 4 removed `CommandReasoningEffort`: it was a verbatim 9-variant copy of
// `codewhale_tui::reasoning_preference::ReasoningEffort` with no independent
// behavior and no consumer, so it had no place on the command boundary.

/// Application mode visible to commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    Agent,
    Plan,
}

impl CommandMode {
    /// Parse a `/mode` argument, mirroring the config layer's `AppMode::parse`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "agent" | "act" | "work" | "auto" | "1" => Some(Self::Agent),
            "plan" | "2" => Some(Self::Plan),
            other if Self::is_retired_operate_alias(other) => Some(Self::Agent),
            other if Self::is_legacy_bypass_alias(other) => Some(Self::Agent),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_retired_operate_alias(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "operate" | "operation" | "ops" | "3"
        )
    }

    /// Legacy bypass spellings (`yolo`, `4`, `bypass`, …) that carry full access.
    #[must_use]
    pub fn is_legacy_bypass_alias(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "yolo" | "4" | "bypass" | "bypass-permissions" | "bypasspermissions"
        )
    }

    /// Short English UI label for the mode.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Agent => "Work",
            Self::Plan => "Plan",
        }
    }
}

/// Tool-approval posture visible to commands.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CommandApprovalMode {
    Auto,
    Bypass,
    #[default]
    Suggest,
    AcceptEdits,
    Never,
}

/// Cost currency used by command-facing accounting operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCurrency {
    Usd,
    Cny,
}

/// Terminal screen mode visible to `/fullscreen` and `/inline` (FEAT-028 D5).
/// Mirrors the TUI `ScreenMode`; the host adapter owns the actual viewport swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandScreenMode {
    /// Alternate screen buffer.
    Fullscreen,
    /// Full-height inline viewport with no alternate screen.
    Inline,
}

/// Workbar placement, mirroring the TUI `WorkSurfacePlacement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandWorkSurfacePlacement {
    Bottom,
    Top,
    Left,
    Right,
    Off,
}

impl CommandWorkSurfacePlacement {
    /// Config-token spelling used by the `/workbar` status line and persistence.
    #[must_use]
    pub const fn as_setting(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Left => "left",
            Self::Right => "right",
            Self::Off => "off",
        }
    }
}

/// Workbar panel, mirroring the TUI `RailPanel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRailPanel {
    Agents,
    Tasks,
    Background,
    Terminal,
    Files,
    Notepad,
    Context,
    Git,
    Price,
}
