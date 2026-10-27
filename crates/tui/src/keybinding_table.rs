//! Documentation-only catalog of every user-facing keybinding.
//!
//! This module is the *single source of truth* for what shortcuts the help
//! overlay renders. The actual key handlers live in `tui/ui.rs` (and a few
//! sibling modules); they read keys directly off the crossterm event stream
//! and intentionally do **not** consult this catalog. The catalog exists so
//! that:
//!
//! 1. The help overlay (`tui/views/help.rs`) does not have to maintain a
//!    parallel list that silently rots when a handler is added or moved.
//! 2. New contributors have one place to look when answering "which keys are
//!    bound, and where do they go?"
//!
//! When you add or change a binding in `ui.rs`, **add or update the matching
//! entry here**. The compile-only side-effect of forgetting is a stale help
//! screen; there is no runtime crash, so the discipline lives in code review.
//!
//! Entries are grouped by `KeybindingSection`. The `chord` field is a
//! human-readable string formatted exactly the way it should appear in help —
//! we avoid storing `KeyBinding` values directly because many shortcuts are
//! pairs (`↑/↓`) or families (`1-8`) that don't map cleanly to a single
//! chord.

use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingSection {
    Navigation,
    Editing,
    Submission,
    Modes,
    Sessions,
    Clipboard,
    Pointer,
    Help,
}

impl KeybindingSection {
    /// Every section, in declaration order. Callers that need to walk the set —
    /// the help overlay's fold defaults, its rank lookup — read this instead of
    /// restating the list, which is how `Pointer` came to be missing from one
    /// of them and not the other.
    pub const ALL: [Self; 8] = [
        Self::Navigation,
        Self::Editing,
        Self::Submission,
        Self::Modes,
        Self::Sessions,
        Self::Clipboard,
        Self::Pointer,
        Self::Help,
    ];

    pub fn label(self, locale: codewhale_localization::Locale) -> Cow<'static, str> {
        use codewhale_localization::{MessageId, tr};
        let id = match self {
            Self::Navigation => MessageId::HelpSectionNavigation,
            Self::Editing => MessageId::HelpSectionEditing,
            Self::Submission => MessageId::HelpSectionActions,
            Self::Modes => MessageId::HelpSectionModes,
            Self::Sessions => MessageId::HelpSectionSessions,
            Self::Clipboard => MessageId::HelpSectionClipboard,
            Self::Pointer => MessageId::HelpSectionPointer,
            Self::Help => MessageId::HelpSectionHelp,
        };
        tr(locale, id)
    }

    /// Stable ordering for help rendering — matches the variant declaration
    /// order; explicit so adding a section forces a deliberate placement.
    pub fn rank(self) -> u8 {
        match self {
            Self::Navigation => 0,
            Self::Editing => 1,
            Self::Submission => 2,
            Self::Modes => 3,
            Self::Sessions => 4,
            Self::Clipboard => 5,
            Self::Pointer => 6,
            Self::Help => 7,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KeybindingEntry {
    pub chord: &'static str,
    pub description_id: codewhale_localization::MessageId,
    pub section: KeybindingSection,
}

pub const PROVIDER_OFFER_CHORD: &str = "F7";
pub const EXTERNAL_EDITOR_CHORD: &str = "Ctrl+Shift+O / F4";

/// Canonical list of keybindings shown in the help overlay.
///
/// Strings are written in the same notation the existing help screen uses so
/// readers can cross-reference with documentation: `Ctrl+X`, `Alt+X`,
/// `Shift+X`, `↑/↓`, `PgUp/PgDn`, etc. Help renderers may apply per-platform
/// substitutions (e.g. `⌥` for Alt on macOS) at render time, but the catalog
/// itself stores the portable form.
pub const KEYBINDINGS: &[KeybindingEntry] = &[
    // --- Navigation ---
    KeybindingEntry {
        chord: "↑ / ↓",
        description_id: codewhale_localization::MessageId::KbScrollTranscript,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "Alt+↑ / Alt+↓",
        description_id: codewhale_localization::MessageId::KbScrollTranscriptAlt,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "Shift+↑ / Shift+↓",
        description_id: codewhale_localization::MessageId::KbBrowseHistory,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "PgUp / PgDn",
        description_id: codewhale_localization::MessageId::KbScrollPage,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "Ctrl+Home / Ctrl+End",
        description_id: codewhale_localization::MessageId::KbJumpTopBottom,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "Alt+G / Alt+Shift+G",
        description_id: codewhale_localization::MessageId::KbJumpTopBottomEmpty,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "Alt+[ / Alt+]",
        description_id: codewhale_localization::MessageId::KbJumpToolBlocks,
        section: KeybindingSection::Navigation,
    },
    // --- Editing ---
    KeybindingEntry {
        chord: "← / → / Ctrl+←/→ / Alt+←/→",
        description_id: codewhale_localization::MessageId::KbMoveCursor,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Home / End",
        description_id: codewhale_localization::MessageId::KbJumpLineStartEnd,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Ctrl+A / Ctrl+E",
        description_id: codewhale_localization::MessageId::KbJumpLineStartEnd,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Backspace / Delete",
        description_id: codewhale_localization::MessageId::KbDeleteChar,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Ctrl+W / Ctrl+Backspace / Alt+Backspace",
        description_id: codewhale_localization::MessageId::KbDeleteWord,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Ctrl+Y",
        description_id: codewhale_localization::MessageId::KbYank,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Ctrl+Shift+E / Cmd+Shift+E",
        description_id: codewhale_localization::MessageId::KbToggleFileTree,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "Shift+←/→ / Shift+Home/End / Ctrl+Shift+←/→ / Alt+Shift+←/→ / Ctrl+Shift+Home/End",
        description_id: codewhale_localization::MessageId::KbSelectText,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        // Ctrl+A keeps its readline meaning (start of input); select-all is
        // the shifted chord, plus native Cmd+A on terminals that forward Cmd.
        chord: "Ctrl+Shift+A / Cmd+A",
        description_id: codewhale_localization::MessageId::KbSelectAllDraft,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Ctrl+U",
        description_id: codewhale_localization::MessageId::KbClearDraft,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Ctrl+Z",
        description_id: codewhale_localization::MessageId::KbRestoreClearedDraft,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Ctrl+G / Ctrl+S",
        description_id: codewhale_localization::MessageId::KbStashDraft,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        chord: "Alt+R",
        description_id: codewhale_localization::MessageId::KbSearchHistory,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        // Ctrl+J leads because it is a plain control byte every terminal
        // sends. Shift+Enter only arrives where the kitty keyboard protocol
        // is live — see `composer_ui::terminal_can_report_shift_enter` — so
        // it is listed last rather than taught first.
        chord: "Ctrl+J / Alt+Enter / Shift+Enter (enhanced terminals)",
        description_id: codewhale_localization::MessageId::KbInsertNewline,
        section: KeybindingSection::Editing,
    },
    // --- Submission / actions ---
    KeybindingEntry {
        chord: "Enter",
        description_id: codewhale_localization::MessageId::KbSendDraft,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Esc",
        description_id: codewhale_localization::MessageId::KbCloseMenu,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+C",
        description_id: codewhale_localization::MessageId::KbCancelOrExit,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+B",
        description_id: codewhale_localization::MessageId::KbShellControls,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+D",
        description_id: codewhale_localization::MessageId::KbExitEmpty,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+K",
        description_id: codewhale_localization::MessageId::KbCommandPalette,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "F2",
        description_id: codewhale_localization::MessageId::KbSettings,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: PROVIDER_OFFER_CHORD,
        description_id: codewhale_localization::MessageId::OrcaRouterOfferAction,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+X (Activity workbar)",
        description_id: codewhale_localization::MessageId::KbCancelBackgroundShellJobs,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+P",
        description_id: codewhale_localization::MessageId::KbFuzzyFilePicker,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        // `/context` is the guaranteed path; Alt+C is an unadvertised
        // handler until proven in real terminals (TUI-DOG-003).
        chord: "/context",
        description_id: codewhale_localization::MessageId::KbCompactInspector,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        // `/provider` remains the portable command route; F3 mirrors the
        // clickable topbar route segment without consuming composer text.
        chord: "F3 / /provider",
        description_id: codewhale_localization::MessageId::CmdProviderDescription,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Alt+L",
        description_id: codewhale_localization::MessageId::KbLastMessagePager,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        // Bare `v` always types `v`; details is Alt+V only (⌥V on macOS).
        chord: "Alt+V",
        description_id: codewhale_localization::MessageId::KbSelectedDetails,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+O",
        description_id: codewhale_localization::MessageId::KbReasoningDetail,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+Alt+O",
        description_id: codewhale_localization::MessageId::KbTurnInspector,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: EXTERNAL_EDITOR_CHORD,
        description_id: codewhale_localization::MessageId::KbExternalEditor,
        section: KeybindingSection::Editing,
    },
    KeybindingEntry {
        // `/transcript` is the reliable fallback when a terminal cannot
        // distinguish Ctrl+Shift+T from Ctrl+T.
        chord: "/transcript / Ctrl+Shift+T",
        description_id: codewhale_localization::MessageId::KbLiveTranscript,
        section: KeybindingSection::Submission,
    },
    KeybindingEntry {
        chord: "Ctrl+T",
        description_id: codewhale_localization::MessageId::KbCycleThinking,
        section: KeybindingSection::Modes,
    },
    KeybindingEntry {
        chord: "Esc Esc",
        description_id: codewhale_localization::MessageId::KbBacktrackMessage,
        section: KeybindingSection::Submission,
    },
    // --- Modes ---
    KeybindingEntry {
        chord: "Tab",
        description_id: codewhale_localization::MessageId::KbCompleteCycleModes,
        section: KeybindingSection::Modes,
    },
    KeybindingEntry {
        chord: "Shift+Tab",
        description_id: codewhale_localization::MessageId::KbCyclePermissions,
        section: KeybindingSection::Modes,
    },
    KeybindingEntry {
        chord: "Alt+1-8",
        description_id: codewhale_localization::MessageId::KbJumpPlanAgentYolo,
        section: KeybindingSection::Modes,
    },
    KeybindingEntry {
        chord: "Alt+P / Alt+A / Alt+Y",
        description_id: codewhale_localization::MessageId::KbAltJumpPlanAgentYolo,
        section: KeybindingSection::Modes,
    },
    KeybindingEntry {
        chord: "Alt+! / Alt+@ / Alt+# / Alt+$ / Alt+0 / Ctrl+Alt+0",
        description_id: codewhale_localization::MessageId::KbFocusSidebar,
        section: KeybindingSection::Modes,
    },
    // --- Sessions ---
    KeybindingEntry {
        chord: "Ctrl+R",
        description_id: codewhale_localization::MessageId::KbSessionPicker,
        section: KeybindingSection::Sessions,
    },
    KeybindingEntry {
        chord: "Ctrl+L",
        description_id: codewhale_localization::MessageId::KbCompactContext,
        section: KeybindingSection::Sessions,
    },
    KeybindingEntry {
        // Same shifted-Ctrl family as Ctrl+Shift+A/E/O; routes through the
        // `/update install` command, so managed installs keep their gate.
        chord: "Ctrl+Shift+U",
        description_id: codewhale_localization::MessageId::KbUpdateInstall,
        section: KeybindingSection::Sessions,
    },
    // --- Clipboard ---
    KeybindingEntry {
        // Keep both terminal-client families visible: the TUI may be running
        // on Linux while the user's SSH terminal is on macOS (or vice versa).
        chord: "Cmd+V / Ctrl+Shift+V",
        description_id: codewhale_localization::MessageId::KbTerminalPaste,
        section: KeybindingSection::Clipboard,
    },
    KeybindingEntry {
        chord: "Ctrl+V",
        description_id: codewhale_localization::MessageId::KbPasteAttach,
        section: KeybindingSection::Clipboard,
    },
    KeybindingEntry {
        // Terminal-native copy chords are normally consumed by the local
        // terminal and never become Codewhale key events. Ctrl+C is the
        // reliable in-app copy path when a Codewhale selection is active.
        chord: "Ctrl+C (selection)",
        description_id: codewhale_localization::MessageId::KbCopySelection,
        section: KeybindingSection::Clipboard,
    },
    KeybindingEntry {
        chord: "@path",
        description_id: codewhale_localization::MessageId::KbAttachPath,
        section: KeybindingSection::Clipboard,
    },
    // --- Pointer ---
    // The mouse is a first-class input here: every one of these is wired in
    // `tui/mouse_ui.rs`. A pointer that does nothing where a user expects it
    // to act reads as a broken app, so what responds is documented.
    KeybindingEntry {
        chord: "Wheel up / down",
        description_id: codewhale_localization::MessageId::KbPointerScroll,
        section: KeybindingSection::Pointer,
    },
    KeybindingEntry {
        chord: "Click",
        description_id: codewhale_localization::MessageId::KbPointerClick,
        section: KeybindingSection::Pointer,
    },
    KeybindingEntry {
        chord: "Drag",
        description_id: codewhale_localization::MessageId::KbPointerDrag,
        section: KeybindingSection::Pointer,
    },
    KeybindingEntry {
        chord: "Right click",
        description_id: codewhale_localization::MessageId::KbContextMenu,
        section: KeybindingSection::Pointer,
    },
    // --- Help ---
    KeybindingEntry {
        // `/help` leads because it is the one route that works in every
        // terminal. F1 is eaten by tmux and several emulators, and how a
        // terminal encodes Ctrl+/ varies; both still open help where they
        // arrive. Alt+? stays an unadvertised handler (TUI-DOG-003).
        chord: "/help / F1 / Ctrl+/",
        description_id: codewhale_localization::MessageId::KbHelpOverlay,
        section: KeybindingSection::Help,
    },
    KeybindingEntry {
        // The work dock was reachable but never listed here, so the only way
        // to learn the chord was the footer's one-time hint.
        chord: "Ctrl+Tab / Ctrl+]",
        description_id: codewhale_localization::MessageId::KbCycleWorkDock,
        section: KeybindingSection::Navigation,
    },
    KeybindingEntry {
        chord: "Ctrl+Shift+Tab",
        description_id: codewhale_localization::MessageId::KbCycleWorkDockBack,
        section: KeybindingSection::Navigation,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_non_empty_and_sections_have_entries() {
        assert!(KEYBINDINGS.iter().any(|entry| !entry.chord.is_empty()));
        // Every declared section should appear in the catalog at least once,
        // otherwise the help overlay would render an empty heading.
        let sections = [
            KeybindingSection::Navigation,
            KeybindingSection::Editing,
            KeybindingSection::Submission,
            KeybindingSection::Modes,
            KeybindingSection::Sessions,
            KeybindingSection::Clipboard,
            KeybindingSection::Help,
        ];
        for section in sections {
            assert!(
                KEYBINDINGS.iter().any(|entry| entry.section == section),
                "no entries for section {section:?}"
            );
        }
    }

    #[test]
    fn help_advertises_f1_and_ctrl_slash_never_alt_question() {
        // TUI-DOG-003: Alt+? is not advertised anywhere; F1 (with /help) is
        // primary and Ctrl+/ is the secondary fallback.
        assert!(
            KEYBINDINGS.iter().any(|entry| {
                entry.section == KeybindingSection::Help
                    && entry.chord.contains("F1")
                    && entry.chord.contains("Ctrl+/")
            }),
            "help must document F1 with the Ctrl+/ fallback"
        );
        assert!(
            KEYBINDINGS
                .iter()
                .all(|entry| !entry.chord.contains("Alt+?")),
            "Alt+? must not be advertised in the help catalog"
        );
    }

    #[test]
    fn composer_catalog_assigns_one_stable_role_to_each_chord() {
        let chord_for = |id| {
            KEYBINDINGS
                .iter()
                .find(|entry| entry.description_id == id)
                .expect("composer binding should be documented")
                .chord
        };

        assert_eq!(
            chord_for(codewhale_localization::MessageId::KbInsertNewline),
            "Ctrl+J / Alt+Enter / Shift+Enter (enhanced terminals)"
        );
        assert!(
            KEYBINDINGS
                .iter()
                .all(|entry| !entry.chord.contains("Ctrl+Enter")
                    && !entry.chord.contains("Cmd+Enter"))
        );
        assert_eq!(
            chord_for(codewhale_localization::MessageId::KbStashDraft),
            "Ctrl+G / Ctrl+S"
        );
        assert_eq!(
            chord_for(codewhale_localization::MessageId::KbSendDraft),
            "Enter"
        );

        let tab_copy = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::KbCompleteCycleModes,
        );
        assert!(!tab_copy.to_ascii_lowercase().contains("queue"));
        let stash_copy = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::KbStashDraft,
        );
        assert!(!stash_copy.to_ascii_lowercase().contains("send"));
    }

    #[test]
    fn clipboard_help_distinguishes_terminal_text_graphical_image_and_in_app_copy() {
        let terminal_paste = KEYBINDINGS
            .iter()
            .find(|entry| {
                entry.description_id == codewhale_localization::MessageId::KbTerminalPaste
            })
            .expect("terminal paste binding should be documented");
        let graphical_paste = KEYBINDINGS
            .iter()
            .find(|entry| entry.description_id == codewhale_localization::MessageId::KbPasteAttach)
            .expect("graphical paste binding should be documented");
        let copy = KEYBINDINGS
            .iter()
            .find(|entry| {
                entry.description_id == codewhale_localization::MessageId::KbCopySelection
            })
            .expect("copy binding should be documented");

        assert!(terminal_paste.chord.contains("Cmd+V"));
        assert!(terminal_paste.chord.contains("Ctrl+Shift+V"));
        assert_eq!(graphical_paste.chord, "Ctrl+V");
        let terminal_description = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::KbTerminalPaste,
        );
        let graphical_description = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::KbPasteAttach,
        );
        assert!(!terminal_description.to_ascii_lowercase().contains("image"));
        assert!(graphical_description.to_ascii_lowercase().contains("image"));
        assert_eq!(copy.chord, "Ctrl+C (selection)");
        assert!(!copy.chord.contains("Cmd+C"));
        assert!(!copy.chord.contains("Ctrl+Shift+C"));
    }

    #[test]
    fn transcript_navigation_catalog_does_not_advertise_bare_typing_keys() {
        for stale in [
            "g / G",
            "[ / ]",
            "l",
            "?",
            "Ctrl+↑ / Ctrl+↓",
            "v",
            "v / Alt+V",
        ] {
            assert!(
                KEYBINDINGS.iter().all(|entry| entry.chord != stale),
                "stale handler-free chord remains documented: {stale}"
            );
        }
        for wired in ["Alt+G / Alt+Shift+G", "Alt+[ / Alt+]", "Alt+L", "Alt+V"] {
            assert!(
                KEYBINDINGS.iter().any(|entry| entry.chord == wired),
                "wired transcript shortcut missing from help: {wired}"
            );
        }
    }

    #[test]
    fn live_transcript_documents_command_before_shaky_chord() {
        let transcript = KEYBINDINGS
            .iter()
            .find(|entry| {
                entry.description_id == codewhale_localization::MessageId::KbLiveTranscript
            })
            .expect("live transcript entry should be documented");

        assert_eq!(transcript.chord, "/transcript / Ctrl+Shift+T");
    }

    #[test]
    fn ctrl_o_and_ctrl_alt_o_help_copy_match_split_surfaces() {
        let ctrl_o = KEYBINDINGS
            .iter()
            .find(|entry| entry.chord == "Ctrl+O")
            .expect("Ctrl+O keybinding should be documented");

        // Ctrl+O now opens the full recorded Reasoning Detail; the whole-turn
        // Turn Inspector moved to Ctrl+Alt+O.
        assert_eq!(
            ctrl_o.description_id,
            codewhale_localization::MessageId::KbReasoningDetail
        );
        assert_eq!(
            codewhale_localization::tr(codewhale_localization::Locale::En, ctrl_o.description_id,),
            "Open reasoning detail for the selected or current turn"
        );

        let ctrl_alt_o = KEYBINDINGS
            .iter()
            .find(|entry| entry.chord == "Ctrl+Alt+O")
            .expect("Ctrl+Alt+O keybinding should be documented");
        assert_eq!(
            ctrl_alt_o.description_id,
            codewhale_localization::MessageId::KbTurnInspector
        );
        assert_eq!(
            codewhale_localization::tr(
                codewhale_localization::Locale::En,
                ctrl_alt_o.description_id,
            ),
            "Open Turn Inspector"
        );

        let editor = KEYBINDINGS
            .iter()
            .find(|entry| entry.chord == "Ctrl+Shift+O / F4")
            .expect("external-editor keybinding should be documented");
        assert_eq!(
            codewhale_localization::tr(codewhale_localization::Locale::En, editor.description_id,),
            "Edit the draft externally"
        );
    }

    #[test]
    fn ctrl_shift_u_update_install_is_documented_in_the_sessions_section() {
        let entry = KEYBINDINGS
            .iter()
            .find(|entry| entry.chord == "Ctrl+Shift+U")
            .expect("Ctrl+Shift+U keybinding should be documented");
        assert_eq!(
            entry.description_id,
            codewhale_localization::MessageId::KbUpdateInstall
        );
        assert_eq!(entry.section, KeybindingSection::Sessions);
        assert_eq!(
            codewhale_localization::tr(codewhale_localization::Locale::En, entry.description_id),
            "Check for and install the latest Codewhale update (`/update install`)"
        );
    }

    #[test]
    fn ctrl_x_activity_sidebar_cancel_all_is_documented() {
        let ctrl_x_activity = KEYBINDINGS
            .iter()
            .find(|entry| entry.chord == "Ctrl+X (Activity workbar)")
            .expect("Ctrl+X Activity workbar keybinding should be documented");

        assert_eq!(
            ctrl_x_activity.description_id,
            codewhale_localization::MessageId::KbCancelBackgroundShellJobs
        );
    }

    #[test]
    fn tool_details_documents_alt_v_only_never_bare_v() {
        let selected_details = KEYBINDINGS
            .iter()
            .filter(|entry| {
                entry.description_id == codewhale_localization::MessageId::KbSelectedDetails
            })
            .map(|entry| entry.chord)
            .collect::<Vec<_>>();

        // TUI-DOG-002: bare `v` always types `v`; details is Alt+V only.
        assert_eq!(selected_details, vec!["Alt+V"]);
        assert!(
            KEYBINDINGS
                .iter()
                .all(|entry| entry.chord != "v" && !entry.chord.starts_with("v /")),
            "bare `v` must not be advertised — composer typing owns it"
        );
    }

    /// #3758: a user who reads the help overlay must be able to answer "what
    /// does this key do?" with one answer. A key may appear twice only when
    /// every occurrence but one names its context in parentheses — the way
    /// `Ctrl+C` and `Ctrl+C (selection)` do — so the reader is told which
    /// reading applies. Two unqualified entries for the same key is the
    /// ambiguity this guard exists to reject.
    #[test]
    fn every_advertised_key_names_exactly_one_canonical_action() {
        struct Use {
            chord: &'static str,
            alternative: String,
            description_id: codewhale_localization::MessageId,
        }

        let mut uses_by_key: std::collections::BTreeMap<String, Vec<Use>> =
            std::collections::BTreeMap::new();
        for entry in KEYBINDINGS {
            for alternative in entry.chord.split(" / ") {
                let alternative = alternative.trim();
                // `Ctrl+C (selection)` → base key `Ctrl+C`, qualifier retained
                // on the alternative so the check below can see it.
                let base = alternative
                    .split_once(" (")
                    .map(|(head, _)| head)
                    .unwrap_or(alternative)
                    .trim()
                    .to_string();
                uses_by_key.entry(base).or_default().push(Use {
                    chord: entry.chord,
                    alternative: alternative.to_string(),
                    description_id: entry.description_id,
                });
            }
        }

        for (key, uses) in &uses_by_key {
            if uses.len() == 1 {
                continue;
            }
            let single_action = uses
                .iter()
                .all(|entry| entry.description_id == uses[0].description_id);
            if single_action {
                // The same action documented from two spellings is fine —
                // `Home / End` and `Ctrl+A / Ctrl+E` both jump to line edges.
                continue;
            }
            let unqualified: Vec<&str> = uses
                .iter()
                .filter(|entry| !entry.alternative.contains('('))
                .map(|entry| entry.chord)
                .collect();
            assert!(
                unqualified.len() <= 1,
                "{key} is advertised for more than one action without naming the \
                 context that selects between them: {unqualified:?}"
            );
        }
    }

    /// #440 / #3758: `Ctrl+G` and `Ctrl+S` stash the draft. They are not a
    /// send, a queue, a steer, or a file save, and a real-terminal report that
    /// says otherwise is reading ambiguous copy, not misusing the key.
    #[test]
    fn stash_chords_advertise_stashing_and_nothing_else() {
        let stash_entries: Vec<&KeybindingEntry> = KEYBINDINGS
            .iter()
            .filter(|entry| {
                entry
                    .chord
                    .split(" / ")
                    .map(str::trim)
                    .any(|chord| matches!(chord, "Ctrl+G" | "Ctrl+S"))
            })
            .collect();

        assert_eq!(
            stash_entries.len(),
            1,
            "Ctrl+G / Ctrl+S must be documented exactly once, together"
        );
        assert_eq!(stash_entries[0].chord, "Ctrl+G / Ctrl+S");
        assert_eq!(
            stash_entries[0].description_id,
            codewhale_localization::MessageId::KbStashDraft
        );

        let copy = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::KbStashDraft,
        )
        .to_ascii_lowercase();
        for forbidden in ["send", "queue", "steer", "submit", "save"] {
            assert!(
                !copy.contains(forbidden),
                "stash copy must not read as {forbidden:?}: {copy:?}"
            );
        }
        assert!(
            copy.contains("stash"),
            "stash copy must name the action it performs: {copy:?}"
        );
    }

    /// Only chords distinguishable by the baseline terminal protocol may be
    /// advertised. Enter sends or queues (then sends a queued message now),
    /// while the newline chords stay newlines.
    #[test]
    fn running_turn_verbs_belong_to_one_chord_each() {
        let entry_for = |id| {
            KEYBINDINGS
                .iter()
                .find(|entry| entry.description_id == id)
                .expect("running-turn binding should be documented")
        };

        assert_eq!(
            entry_for(codewhale_localization::MessageId::KbSendDraft).chord,
            "Enter"
        );
        assert!(
            KEYBINDINGS
                .iter()
                .all(|entry| !entry.chord.contains("Ctrl+Enter")
                    && !entry.chord.contains("Cmd+Enter"))
        );
        assert_eq!(
            entry_for(codewhale_localization::MessageId::KbInsertNewline).chord,
            "Ctrl+J / Alt+Enter / Shift+Enter (enhanced terminals)"
        );

        let newline_copy = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::KbInsertNewline,
        )
        .to_ascii_lowercase();
        for forbidden in ["send", "steer", "queue"] {
            assert!(
                !newline_copy.contains(forbidden),
                "newline chords must not read as {forbidden:?}: {newline_copy:?}"
            );
        }
    }

    #[test]
    fn section_rank_is_a_total_order() {
        let sections = [
            KeybindingSection::Navigation,
            KeybindingSection::Editing,
            KeybindingSection::Submission,
            KeybindingSection::Modes,
            KeybindingSection::Sessions,
            KeybindingSection::Clipboard,
            KeybindingSection::Help,
        ];
        let mut ranks: Vec<u8> = sections.iter().map(|s| s.rank()).collect();
        ranks.sort_unstable();
        ranks.dedup();
        assert_eq!(ranks.len(), sections.len(), "ranks must be unique");
    }

    // ------------------------------------------------------------------
    // docs/KEYBINDINGS.md <-> KEYBINDINGS bidirectional drift gate
    // ------------------------------------------------------------------

    const KEYBINDINGS_DOC: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/KEYBINDINGS.md"
    ));

    /// Doc table chords that are deliberately NOT in the help catalog.
    /// Every entry needs a justification; adding one is a reviewed decision.
    const DOC_CHORD_ALLOWLIST: &[&str] = &[
        // Ctrl+Enter / Cmd+Enter: works, but deliberately unadvertised —
        // several terminals encode it exactly like bare Enter (handler
        // comment at ui.rs is_forced_submit_key call site; absence pinned by
        // composer_catalog_assigns_one_stable_role_to_each_chord).
        "ctrlenter",
        // Ctrl+click / Cmd+click opens OSC 8 links — terminal-owned.
        "ctrlclick",
        // Ctrl+N navigates the slash-command menu; menu-local chords are
        // documented in the md but intentionally not help-catalog entries.
        "ctrln",
    ];

    /// Normalize a chord for comparison: lowercase, macOS aliases folded
    /// (Option -> Alt, Cmd -> Ctrl), all separators stripped, so `Ctrl-Home`,
    /// `Ctrl+Home`, and `Ctrl + Home` compare equal.
    fn normalize_chord(raw: &str) -> String {
        raw.to_lowercase()
            .replace("option", "alt")
            .replace("cmd", "ctrl")
            .chars()
            .filter(|c| !matches!(c, '-' | '+' | ' ' | '`'))
            .collect()
    }

    /// `Alt+1-8`-style digit family -> one normalized atom per digit.
    fn expand_digit_family(lowered: &str) -> Option<Vec<String>> {
        let (head, tail) = lowered.rsplit_once('-')?;
        let end: u32 = tail.parse().ok()?;
        let start: u32 = head.chars().next_back()?.to_digit(10)?;
        let prefix = &head[..head.len() - 1];
        if !matches!(prefix, "alt+" | "ctrl+" | "shift+") || start > end {
            return None;
        }
        Some(
            (start..=end)
                .map(|digit| normalize_chord(&format!("{prefix}{digit}")))
                .collect(),
        )
    }

    /// Expand one chord segment (no ` / ` separators) into normalized atoms.
    /// Handles suffix families: `Ctrl+Home/End` -> ctrlhome + ctrlend,
    /// `Ctrl+Shift+←/→` -> ctrlshift← + ctrlshift→.
    fn segment_atoms(segment: &str, out: &mut Vec<String>) {
        let lowered = segment
            .trim()
            .to_lowercase()
            .replace("option", "alt")
            .replace("cmd", "ctrl");
        if lowered.is_empty() {
            return;
        }
        if let Some(family) = expand_digit_family(&lowered) {
            out.extend(family);
            return;
        }
        let mut prefix = String::new();
        let mut rest = lowered.as_str();
        loop {
            let mut consumed = false;
            for modifier in ["ctrl+", "alt+", "shift+", "ctrl-", "alt-", "shift-"] {
                if let Some(stripped) = rest.strip_prefix(modifier) {
                    prefix.push_str(&modifier[..modifier.len() - 1]);
                    prefix.push('+');
                    rest = stripped;
                    consumed = true;
                    break;
                }
            }
            if !consumed {
                break;
            }
        }
        let parts: Vec<&str> = rest.split('/').collect();
        if !prefix.is_empty() && parts.len() > 1 && parts.iter().all(|p| !p.is_empty()) {
            for part in parts {
                out.push(normalize_chord(&format!("{prefix}{part}")));
            }
        } else {
            out.push(normalize_chord(&lowered));
        }
    }

    /// Normalized chord atoms advertised by the help catalog. Qualified
    /// entries (`/context`, `@path`, `Right click`) are commands or mouse
    /// gestures, not chords, and are skipped by design.
    fn catalog_chord_atoms() -> Vec<String> {
        let mut out = Vec::new();
        for entry in KEYBINDINGS {
            let chord = entry.chord.split(" (").next().unwrap_or(entry.chord);
            for segment in chord.split(" / ") {
                let segment = segment.trim();
                if segment.starts_with('/') || segment.starts_with('@') || segment.contains("click")
                {
                    continue;
                }
                segment_atoms(segment, &mut out);
            }
        }
        out
    }

    /// Normalized chord atoms from backticked tokens in docs/KEYBINDINGS.md
    /// table rows. Prose backticks (terminal-local notes) are excluded by
    /// only reading `| ... |` lines; non-chord tokens (slash commands,
    /// mentions, mouse drags, single letters) are filtered out.
    fn doc_table_chord_atoms() -> Vec<String> {
        const NAMED: &[&str] = &[
            "tab",
            "esc",
            "enter",
            "backspace",
            "delete",
            "home",
            "end",
            "pgup",
            "pgdn",
            "↑",
            "↓",
            "←",
            "→",
        ];
        let mut out = Vec::new();
        for line in KEYBINDINGS_DOC.lines() {
            if !line.trim_start().starts_with('|') {
                continue;
            }
            for token in line.split('`').skip(1).step_by(2) {
                let lowered = token.to_lowercase();
                if lowered.starts_with('/') || lowered.starts_with('@') || lowered.starts_with('!')
                {
                    continue;
                }
                let has_modifier = ["ctrl", "alt", "shift", "option", "cmd"]
                    .iter()
                    .any(|m| lowered.contains(m));
                let is_named = NAMED.contains(&lowered.as_str())
                    || (lowered.len() == 2
                        && lowered.starts_with('f')
                        && lowered[1..].parse::<u8>().is_ok());
                let is_named_combo = lowered.contains(' ')
                    && lowered.split(' ').all(|part| {
                        let part = part.trim();
                        NAMED.contains(&part)
                            || ["ctrl", "alt", "shift", "option", "cmd"]
                                .iter()
                                .any(|m| part.contains(m))
                    });
                if has_modifier || is_named || is_named_combo {
                    segment_atoms(token, &mut out);
                }
            }
        }
        out
    }

    #[test]
    fn keybindings_md_and_help_catalog_do_not_drift() {
        use std::collections::BTreeSet;

        let catalog: BTreeSet<String> = catalog_chord_atoms().into_iter().collect();
        let doc_atoms: BTreeSet<String> = doc_table_chord_atoms().into_iter().collect();
        let allowlist: BTreeSet<&str> = DOC_CHORD_ALLOWLIST.iter().copied().collect();

        // Direction 1: every chord a docs table advertises must be in the
        // help catalog (or an explicitly justified exception).
        let undocumented: Vec<&String> = doc_atoms
            .iter()
            .filter(|atom| !catalog.contains(*atom) && !allowlist.contains(atom.as_str()))
            .collect();
        assert!(
            undocumented.is_empty(),
            "docs/KEYBINDINGS.md advertises chords missing from the KEYBINDINGS \
             catalog — add the binding, fix the docs, or justify an allowlist entry: \
             {undocumented:?}"
        );

        // Direction 2: every catalog chord must be documented in the md —
        // either as an expanded table token (doc_atoms) or anywhere in the
        // normalized prose (e.g. Backspace/Delete in the selection notes).
        let normalized_doc = normalize_chord(KEYBINDINGS_DOC);
        let undocumented: Vec<&String> = catalog
            .iter()
            .filter(|atom| !doc_atoms.contains(*atom) && !normalized_doc.contains(atom.as_str()))
            .collect();
        assert!(
            undocumented.is_empty(),
            "KEYBINDINGS catalog advertises chords absent from docs/KEYBINDINGS.md \
             — document the chord or remove it from the catalog: {undocumented:?}"
        );
    }
}
