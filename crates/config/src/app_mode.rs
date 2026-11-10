//! The TUI's user-facing operating mode. Lives in codewhale-config so
//! settings, receipts, and other crates can name it without depending on
//! the TUI; the TUI adds the localized picker strings through an extension
//! trait.

/// Supported application modes for the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Agent,
    Plan,
}

impl AppMode {
    pub const ALL: [Self; 2] = [Self::Plan, Self::Agent];

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "agent" | "act" | "work" | "auto" | "1" => Some(Self::Agent),
            "plan" | "2" => Some(Self::Plan),
            other if Self::is_retired_operate_alias(other) => Some(Self::Agent),
            // Invisible one-way permission shorthand only — never a visible
            // mode. These spellings resolve to Work; the bypass posture they
            // imply is carried by the permission surface (settings load,
            // CLI/runtime wire), not by a mode.
            other if Self::is_legacy_bypass_alias(other) => Some(Self::Agent),
            _ => None,
        }
    }

    /// Legacy mode spellings (`yolo`, `4`, `bypass`, `bypass-permissions`,
    /// `bypasspermissions`) that carried the Full Access posture. [`Self::parse`]
    /// folds them to Work; callers that must preserve the permission meaning
    /// (the `/mode` command, the runtime policy wire, persisted thread records)
    /// ask this one predicate instead of keeping their own copy of the list.
    #[must_use]
    pub fn is_legacy_bypass_alias(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "yolo" | "4" | "bypass" | "bypass-permissions" | "bypasspermissions"
        )
    }

    /// Operate was retired as a mode. Its spellings still parse, as Work, so a
    /// saved setting or an older client never errors; callers that want to say
    /// so once ask this predicate.
    #[must_use]
    pub fn is_retired_operate_alias(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "operate" | "operation" | "ops" | "3"
        )
    }

    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        Self::parse(value).unwrap_or(Self::Agent)
    }

    #[must_use]
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
        }
    }

    /// Short label used in the UI footer.
    pub fn label(self) -> &'static str {
        match self {
            AppMode::Agent => "WORK",
            AppMode::Plan => "PLAN",
        }
    }

    #[must_use]
    pub fn hook_label(self) -> &'static str {
        match self {
            AppMode::Agent => "ACT",
            AppMode::Plan => "PLAN",
        }
    }

    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            AppMode::Agent => "Work",
            AppMode::Plan => "Plan",
        }
    }

    #[must_use]
    pub fn number(self) -> char {
        match self {
            AppMode::Agent => '1',
            AppMode::Plan => '2',
        }
    }

    #[must_use]
    pub fn uses_agent_baseline(self) -> bool {
        matches!(self, Self::Agent)
    }

    /// Description shown in help or onboarding text.
    pub fn description(self) -> &'static str {
        match self {
            AppMode::Agent => "Work mode - direct work in the current session with tools",
            AppMode::Plan => "Plan mode - research and design before implementing",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_mode_helpers_centralize_parse_and_labels() {
        assert_eq!(AppMode::parse("agent"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("act"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("work"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("2"), Some(AppMode::Plan));
        assert_eq!(AppMode::parse("auto"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("3"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("operate"), Some(AppMode::Agent));
        assert!(AppMode::is_retired_operate_alias(" Operate "));
        // Legacy YOLO spellings resolve to Work; the bypass posture they imply
        // travels on the permission surface, not on a mode.
        assert_eq!(AppMode::parse("YOLO"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("4"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("bypass"), Some(AppMode::Agent));
        assert_eq!(AppMode::parse("bypass-permissions"), Some(AppMode::Agent));
        for alias in [
            "yolo",
            " YOLO ",
            "4",
            "bypass",
            "bypass-permissions",
            "BypassPermissions",
        ] {
            assert!(AppMode::is_legacy_bypass_alias(alias), "{alias}");
        }
        for mode in ["agent", "act", "plan", "operate", "1", "full-access", ""] {
            assert!(!AppMode::is_legacy_bypass_alias(mode), "{mode}");
        }
        assert_eq!(AppMode::parse("multitask"), None);
        assert_eq!(AppMode::parse("5"), None);
        assert_eq!(AppMode::parse("fast"), None);
        assert_eq!(AppMode::from_setting("multitask"), AppMode::Agent);
        assert_eq!(AppMode::from_setting("5"), AppMode::Agent);

        assert_eq!(AppMode::Agent.as_setting(), "agent");
        assert_eq!(AppMode::Plan.display_name(), "Plan");
        assert_eq!(AppMode::Agent.number(), '1');
        assert_eq!(AppMode::ALL, [AppMode::Plan, AppMode::Agent]);
    }

    /// The durable form of a mode is the `as_setting()` string persisted into
    /// settings and session records — `AppMode` derives no `Serialize`, so the
    /// round-trip that has to hold is string -> mode -> string.
    #[test]
    fn setting_strings_round_trip_for_every_mode() {
        for mode in AppMode::ALL {
            let setting = mode.as_setting();
            assert_eq!(
                AppMode::from_setting(setting),
                mode,
                "from_setting({setting})"
            );
            assert_eq!(AppMode::parse(setting), Some(mode), "parse({setting})");
        }

        assert_eq!(AppMode::Agent.as_setting(), "agent");
        assert_eq!(AppMode::Plan.as_setting(), "plan");
    }

    /// `from_setting` is the de-facto default: an absent, empty, or unreadable
    /// stored value must land on Work rather than panicking.
    #[test]
    fn from_setting_falls_back_to_act_for_unknown_values() {
        assert_eq!(AppMode::from_setting(""), AppMode::Agent);
        assert_eq!(AppMode::from_setting("   "), AppMode::Agent);
        assert_eq!(AppMode::from_setting("nonsense"), AppMode::Agent);
        assert_eq!(AppMode::from_setting("OPERATE"), AppMode::Agent);
    }

    #[test]
    fn labels_and_descriptions_cover_every_mode() {
        assert_eq!(AppMode::Agent.label(), "WORK");
        assert_eq!(AppMode::Plan.label(), "PLAN");

        assert_eq!(AppMode::Agent.display_name(), "Work");
        assert_eq!(AppMode::Plan.display_name(), "Plan");

        for mode in AppMode::ALL {
            assert!(!mode.description().is_empty());
        }
    }

    #[test]
    fn only_work_uses_the_agent_baseline() {
        assert!(AppMode::Agent.uses_agent_baseline());
        assert!(!AppMode::Plan.uses_agent_baseline());
    }
}
