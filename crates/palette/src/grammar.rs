//! Closed color vocabulary for status-bar chrome.
//!
//! Five visual families: surface, neutral text, action, live/outcome, and
//! attention. Semantic roles stay explicit even when they share a hue;
//! modes and outcomes are named by their labels, not extra rainbow lanes.
//! Attention preserves each theme's permission, warning and danger shades.
//!
//! Contract: `docs/design/STATUS_BAR_COLOR_GRAMMAR.md`.

use ratatui::style::{Color, Style};

use super::themes::UiTheme;

/// The five visual families. Surface is the canvas, not a foreground ink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticFamily {
    Surface,
    Neutral,
    Action,
    Live,
    Attention,
}

impl SemanticFamily {
    #[cfg(test)]
    pub const ALL: [Self; 5] = [
        Self::Surface,
        Self::Neutral,
        Self::Action,
        Self::Live,
        Self::Attention,
    ];
}

/// Named status-bar inks. Each variant is an existing `UiTheme` slot, not a
/// new theme. Adding a variant requires assigning one of the five families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeInk {
    Outcome,
    PermissionAsk,
    PermissionAutoReview,
    PermissionFullAccess,
    Waiting,
    Attention,
    Active,
    PolicyAct,
    PolicyPlan,
    PolicyOperate,
    Identity,
    Info,
    MetadataValue,
    Metadata,
    MetadataHint,
    MetadataDim,
    Failure,
}

impl ChromeInk {
    #[cfg(test)]
    pub const ALL: [Self; 17] = [
        Self::Outcome,
        Self::PermissionAsk,
        Self::PermissionAutoReview,
        Self::PermissionFullAccess,
        Self::Waiting,
        Self::Attention,
        Self::Active,
        Self::PolicyAct,
        Self::PolicyPlan,
        Self::PolicyOperate,
        Self::Identity,
        Self::Info,
        Self::MetadataValue,
        Self::Metadata,
        Self::MetadataHint,
        Self::MetadataDim,
        Self::Failure,
    ];

    #[must_use]
    pub const fn family(self) -> SemanticFamily {
        match self {
            Self::Outcome | Self::Active => SemanticFamily::Live,
            Self::PermissionAsk
            | Self::PermissionAutoReview
            | Self::PermissionFullAccess
            | Self::Waiting
            | Self::Attention
            | Self::Failure => SemanticFamily::Attention,
            Self::PolicyAct
            | Self::PolicyPlan
            | Self::PolicyOperate
            | Self::Identity
            | Self::Info => SemanticFamily::Action,
            Self::MetadataValue | Self::Metadata | Self::MetadataHint | Self::MetadataDim => {
                SemanticFamily::Neutral
            }
        }
    }

    /// Resolve through the live theme. Ordinary navigation and mode share
    /// its action hue; active and completed work share its live hue. Safety
    /// retains the existing permission and warning/error distinctions.
    #[must_use]
    pub fn color(self, theme: &UiTheme) -> Color {
        match self {
            Self::Outcome => theme.status_working,
            Self::PermissionAsk => theme.permission_ask,
            Self::PermissionAutoReview => theme.permission_auto_review,
            Self::PermissionFullAccess => theme.permission_full_access,
            Self::Waiting => theme.accent_action,
            Self::Attention => theme.warning,
            Self::Active => theme.status_working,
            Self::PolicyAct => theme.accent_primary,
            Self::PolicyPlan => theme.accent_primary,
            Self::PolicyOperate => theme.accent_primary,
            Self::Identity => theme.accent_primary,
            Self::Info => theme.accent_primary,
            Self::MetadataValue => theme.text_soft,
            Self::Metadata => theme.text_muted,
            Self::MetadataHint => theme.text_hint,
            Self::MetadataDim => theme.text_dim,
            Self::Failure => theme.error_fg,
        }
    }
}

#[must_use]
pub fn chrome_style(theme: &UiTheme, ink: ChromeInk) -> Style {
    Style::default().fg(ink.color(theme))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SELECTABLE_THEMES;

    #[test]
    fn chrome_has_five_visual_families_with_explicit_safety_roles() {
        assert_eq!(SemanticFamily::ALL.len(), 5);
        for ink in ChromeInk::ALL {
            assert!(SemanticFamily::ALL.contains(&ink.family()));
        }
        assert_eq!(ChromeInk::Failure.family(), SemanticFamily::Attention);
        assert_eq!(
            ChromeInk::PermissionFullAccess.family(),
            SemanticFamily::Attention
        );
        assert_eq!(ChromeInk::PolicyOperate.family(), SemanticFamily::Action);
        assert_eq!(ChromeInk::Outcome.family(), SemanticFamily::Live);
    }

    #[test]
    fn every_selectable_theme_limits_ordinary_chrome_to_action_and_live() {
        for id in SELECTABLE_THEMES {
            let theme = id.ui_theme();
            // Changing a mode or finishing work must not add a third hue.
            for ink in [
                ChromeInk::Identity,
                ChromeInk::Info,
                ChromeInk::PolicyAct,
                ChromeInk::PolicyPlan,
                ChromeInk::PolicyOperate,
            ] {
                assert_eq!(
                    ink.color(&theme),
                    theme.accent_primary,
                    "{} {ink:?}",
                    id.name()
                );
            }
            for ink in [ChromeInk::Active, ChromeInk::Outcome] {
                assert_eq!(
                    ink.color(&theme),
                    theme.status_working,
                    "{} {ink:?}",
                    id.name()
                );
            }
            assert_eq!(ChromeInk::Metadata.color(&theme), theme.text_muted);
        }
    }

    #[test]
    fn every_selectable_theme_preserves_permission_and_failure_meaning() {
        for id in SELECTABLE_THEMES {
            let theme = id.ui_theme();
            let permissions = [
                ChromeInk::PermissionAsk.color(&theme),
                ChromeInk::PermissionAutoReview.color(&theme),
                ChromeInk::PermissionFullAccess.color(&theme),
            ];
            assert_eq!(
                permissions,
                [
                    theme.permission_ask,
                    theme.permission_auto_review,
                    theme.permission_full_access
                ],
                "{} permission authority",
                id.name()
            );
            assert_ne!(
                permissions[0],
                permissions[1],
                "{} Ask/Auto-Review",
                id.name()
            );
            assert_ne!(
                permissions[1],
                permissions[2],
                "{} Auto-Review/Full Access",
                id.name()
            );
            assert_ne!(
                permissions[0],
                permissions[2],
                "{} Ask/Full Access",
                id.name()
            );
            assert_eq!(ChromeInk::Attention.color(&theme), theme.warning);
            assert_eq!(ChromeInk::Failure.color(&theme), theme.error_fg);
            assert_eq!(
                chrome_style(&theme, ChromeInk::Failure).fg,
                Some(theme.error_fg)
            );
        }
    }
}
