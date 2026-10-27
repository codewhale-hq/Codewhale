//! The user-facing approval posture (`Ask` / `Auto-Review` / `Full Access` /
//! `Never`). Lives beside `AskForApproval` so policy code and the TUI share
//! one definition; the TUI adds only presentation on top.

/// Determines when tool executions require user approval
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Automatically review risky tool calls before deciding whether to ask.
    Auto,
    /// Bypass approvals entirely (YOLO mode / --yolo flag).
    Bypass,
    /// Suggest approval for non-safe tools (non-YOLO modes)
    #[default]
    Suggest,
    /// Never execute tools requiring approval
    Never,
}

impl ApprovalMode {
    pub fn from_auto_approve(auto_approve: bool) -> Self {
        if auto_approve {
            Self::Bypass
        } else {
            Self::Suggest
        }
    }

    /// Shift+Tab permission cycle order (#0.8.68 M2).
    pub const PERMISSION_CYCLE: [Self; 3] = [Self::Suggest, Self::Auto, Self::Bypass];

    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Auto => "AUTO",
            ApprovalMode::Bypass => "BYPASS",
            ApprovalMode::Suggest => "SUGGEST",
            ApprovalMode::Never => "NEVER",
        }
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "auto-review" | "auto_review" => Some(ApprovalMode::Auto),
            "bypass" | "yolo" | "dontask" | "dont_ask" | "bypass-permissions"
            | "bypasspermissions" | "full-access" | "full_access" | "full" => {
                Some(ApprovalMode::Bypass)
            }
            "suggest" | "suggested" | "on-request" | "untrusted" | "ask" => {
                Some(ApprovalMode::Suggest)
            }
            "never" | "deny" | "denied" => Some(ApprovalMode::Never),
            _ => None,
        }
    }

    #[must_use]
    pub fn agent_baseline_sandbox_mode(self) -> &'static str {
        match self {
            Self::Bypass => "danger-full-access",
            Self::Auto | Self::Suggest | Self::Never => "workspace-write",
        }
    }

    #[must_use]
    pub fn cycle_permission_next(self) -> Self {
        let Some(index) = Self::PERMISSION_CYCLE.iter().position(|mode| *mode == self) else {
            return Self::Suggest;
        };
        Self::PERMISSION_CYCLE[(index + 1) % Self::PERMISSION_CYCLE.len()]
    }

    #[must_use]
    pub fn permission_chip_label(self) -> &'static str {
        match self {
            Self::Suggest => "Ask",
            Self::Auto => "Auto-Review",
            Self::Bypass => "Full Access",
            Self::Never => "Never",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_approval_mode_labels() {
        assert_eq!(ApprovalMode::Auto.label(), "AUTO");
        assert_eq!(ApprovalMode::Suggest.label(), "SUGGEST");
        assert_eq!(ApprovalMode::Never.label(), "NEVER");
    }

    #[test]
    fn test_approval_mode_from_config_value_accepts_aliases() {
        assert_eq!(
            ApprovalMode::from_config_value("auto"),
            Some(ApprovalMode::Auto)
        );
        assert_eq!(
            ApprovalMode::from_config_value("on-request"),
            Some(ApprovalMode::Suggest)
        );
        assert_eq!(
            ApprovalMode::from_config_value("full_access"),
            Some(ApprovalMode::Bypass)
        );
        assert_eq!(
            ApprovalMode::from_config_value("deny"),
            Some(ApprovalMode::Never)
        );
        assert_eq!(ApprovalMode::from_config_value("unknown"), None);
    }

    #[test]
    fn permission_cycle_is_a_closed_loop_and_never_is_off_cycle() {
        assert_eq!(
            ApprovalMode::PERMISSION_CYCLE,
            [
                ApprovalMode::Suggest,
                ApprovalMode::Auto,
                ApprovalMode::Bypass
            ]
        );
        assert_eq!(
            ApprovalMode::Suggest.cycle_permission_next(),
            ApprovalMode::Auto
        );
        assert_eq!(
            ApprovalMode::Auto.cycle_permission_next(),
            ApprovalMode::Bypass
        );
        assert_eq!(
            ApprovalMode::Bypass.cycle_permission_next(),
            ApprovalMode::Suggest
        );
        // Never is deliberately outside the Shift+Tab cycle; stepping from it
        // must land on the safest posture rather than panicking or wrapping.
        assert_eq!(
            ApprovalMode::Never.cycle_permission_next(),
            ApprovalMode::Suggest
        );
    }

    #[test]
    fn default_posture_is_suggest() {
        assert_eq!(ApprovalMode::default(), ApprovalMode::Suggest);
    }
}
