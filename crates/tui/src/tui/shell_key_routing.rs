//! Shell keyboard bindings for details / context / help.
//!
//! Footer hints, help catalog chords, and live handlers must agree on one
//! source. Outside exclusive consent gates, printable characters belong to
//! the composer: bare `v` types `v` in every ordinary focus state — work surface, transcript selection,
//! panel, or modal (TUI-DOG-002). Details/output fires only on
//! Option+V / Alt+V, and macOS renders the label as `⌥V`, never `Alt`/`Cmd`.
//! Help answers to `F1` and `Ctrl+/` (with `/help`); chrome advertises only
//! `Ctrl+/`, the chord every terminal delivers.
//! Provider/route is `F3` (with `/provider`); it is non-printable so it can
//! remain available while the composer owns ordinary text input.
//! `Alt+?` and `Alt+C` are still accepted where terminals deliver them but
//! are never advertised until proven in real terminals (TUI-DOG-003);
//! `/context` is the guaranteed context path.
//! Ambiguous macOS Option glyphs (`ç` / `¿`) remain text: terminals do not
//! identify whether they came from Option or from a user's keyboard layout.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::key_shortcuts;
use crate::tui::views::ModalKind;

/// Who owns the keyboard right now.
///
/// One value, derived in one place ([`crate::tui::app::App::focus`]), in
/// place of the `app.input.is_empty()` guesses that used composer *content*
/// as a stand-in for composer *focus*. Composer editing keys still ask about
/// the text itself; every shell binding asks this instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The model-bound redaction consent gate exclusively owns its decision.
    RedactionGate,
    /// The onboarding rail owns every key until it finishes.
    Onboarding,
    /// A modal view is on top of the stack and handles its own keys.
    Modal(ModalKind),
    /// The pre-session launch screen — its menu or its composer.
    Launch,
    /// A focused rail or workflow panel inside a live session.
    Panel,
    /// The live PTY dock owns terminal input, including control keys.
    TerminalPanel,
    /// The session composer: the default owner.
    Composer,
}

/// Which focus states a binding is live in — the `ShellBinding` focus rule
/// that used to be re-invented at every call site as
/// `&& app.view_stack.is_empty()`. The variants nest: each admits everything
/// the one above it does, plus one more surface. The redaction consent gate
/// is exclusive and sits outside that shell hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusScope {
    TerminalPanel,
    RoutePicker,
    PetHabitat,
    Pet,
    /// Only the sandbox-elevation decision card.
    Elevation,
    /// Only the model-bound redaction consent gate.
    RedactionGate,
    /// A live session: the composer, or a rail/workflow panel that has taken
    /// the keys from it.
    SessionShell,
    /// [`FocusScope::SessionShell`], plus the pre-session launch stage.
    AnyShell,
    /// [`FocusScope::AnyShell`], plus the Config modal, which displays the
    /// very setting the binding changes.
    AnyShellOrConfig,
    /// Every ordinary shell state, onboarding and modals included. Exclusive
    /// consent gates keep their own keys.
    Everywhere,
}

impl FocusScope {
    #[must_use]
    pub fn admits(self, focus: Focus) -> bool {
        match self {
            Self::TerminalPanel => focus == Focus::TerminalPanel,
            Self::RoutePicker => matches!(
                focus,
                Focus::Modal(ModalKind::ProviderPicker | ModalKind::ModelPicker)
            ),
            Self::PetHabitat => focus == Focus::Modal(ModalKind::PetHabitat),
            Self::Pet => matches!(focus, Focus::Composer | Focus::Modal(ModalKind::PetHabitat)),
            Self::Elevation => focus == Focus::Modal(ModalKind::Elevation),
            Self::RedactionGate => focus == Focus::RedactionGate,
            Self::SessionShell => {
                matches!(focus, Focus::Composer | Focus::Panel | Focus::TerminalPanel)
            }
            Self::AnyShell => matches!(
                focus,
                Focus::Composer | Focus::Panel | Focus::TerminalPanel | Focus::Launch
            ),
            Self::AnyShellOrConfig => matches!(
                focus,
                Focus::Composer
                    | Focus::Panel
                    | Focus::TerminalPanel
                    | Focus::Launch
                    | Focus::Modal(ModalKind::Config)
            ),
            Self::Everywhere => focus != Focus::RedactionGate,
        }
    }
}

/// Stable binding ids shared by handlers, footer hints, and help catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellBindingId {
    TerminalNew,
    TerminalNext,
    TerminalPrevious,
    TerminalDetach,
    ElevationUp,
    ElevationDown,
    ElevationConfirm,
    ElevationAbort,
    PetInspect,
    PetResultUp,
    PetResultDown,
    PetResultPageUp,
    PetResultPageDown,
    PetBack,
    PetResultStart,
    PetResultEnd,
    PetFocusAgents,
    PetOpenAgent,
    PetCopyReply,
    PetSound,
    PetBrowser,
    PetWindow,
    RedactionGateConfirm,
    RedactionGateKeepOrBack,
    RedactionGateQuit,
    RedactionGateScroll,
    ToolDetails,
    ContextInspector,
    ProviderRoute,
    ProviderOffer,
    Help,
    ExternalEditor,
    Settings,
    /// Tab: cycle the session mode.
    ModeCycle,
    /// Shift+Tab: cycle the permission posture.
    PermissionCycle,
    /// Ctrl+Tab / Ctrl+]: the next bottom-dock view, or the next pet view
    /// while the pet main view is showing.
    ViewCycle,
    /// Ctrl+Shift+Tab: the previous bottom-dock view, or the previous pet view.
    ViewCycleBack,
}

/// One advertised binding with the portable catalog chord and focus rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellBinding {
    pub id: ShellBindingId,
    /// Chord shown in help / documentation (portable Alt form; macOS
    /// substitutes `⌥` at render time via [`display_chord`]).
    pub catalog_chord: &'static str,
    /// Compact footer chord when this binding is advertised.
    pub footer_chord: &'static str,
    /// The focus states this binding is live in. Never composer content:
    /// a shell binding does the same thing whether or not you have typed.
    pub focus: FocusScope,
}

impl ShellBinding {
    /// Does this key press this binding, ignoring focus?
    #[must_use]
    pub fn matches(&self, key: &KeyEvent) -> bool {
        match self.id {
            ShellBindingId::TerminalNew => {
                key.code == KeyCode::Char('n') && key.modifiers == KeyModifiers::CONTROL
            }
            ShellBindingId::TerminalNext => {
                key.code == KeyCode::Down && key.modifiers == KeyModifiers::ALT
            }
            ShellBindingId::TerminalPrevious => {
                key.code == KeyCode::Up && key.modifiers == KeyModifiers::ALT
            }
            ShellBindingId::TerminalDetach => key.code == KeyCode::Esc && key.modifiers.is_empty(),
            ShellBindingId::ElevationUp => key.code == KeyCode::Up && key.modifiers.is_empty(),
            ShellBindingId::ElevationDown => key.code == KeyCode::Down && key.modifiers.is_empty(),
            ShellBindingId::ElevationConfirm => {
                key.code == KeyCode::Enter && key.modifiers.is_empty()
            }
            ShellBindingId::ElevationAbort => key.code == KeyCode::Esc && key.modifiers.is_empty(),
            ShellBindingId::PetInspect => key.code == KeyCode::F(5) && key.modifiers.is_empty(),
            ShellBindingId::PetResultUp => key.code == KeyCode::Up && key.modifiers.is_empty(),
            ShellBindingId::PetResultDown => key.code == KeyCode::Down && key.modifiers.is_empty(),
            ShellBindingId::PetResultPageUp => {
                key.code == KeyCode::PageUp && key.modifiers.is_empty()
            }
            ShellBindingId::PetResultPageDown => {
                key.code == KeyCode::PageDown && key.modifiers.is_empty()
            }
            ShellBindingId::PetBack => key.code == KeyCode::Esc && key.modifiers.is_empty(),
            ShellBindingId::PetResultStart => key.code == KeyCode::Home && key.modifiers.is_empty(),
            ShellBindingId::PetResultEnd => key.code == KeyCode::End && key.modifiers.is_empty(),
            ShellBindingId::PetFocusAgents => key.code == KeyCode::Tab && key.modifiers.is_empty(),
            ShellBindingId::PetOpenAgent => key.code == KeyCode::Enter && key.modifiers.is_empty(),
            ShellBindingId::PetCopyReply => {
                key.code == KeyCode::Char('c') && key.modifiers.is_empty()
            }
            ShellBindingId::PetSound => key.code == KeyCode::F(6) && key.modifiers.is_empty(),
            ShellBindingId::PetBrowser => key.code == KeyCode::F(8) && key.modifiers.is_empty(),
            ShellBindingId::PetWindow => key.code == KeyCode::F(9) && key.modifiers.is_empty(),

            ShellBindingId::RedactionGateConfirm => is_redaction_gate_choice(key, '1', 'y'),
            ShellBindingId::RedactionGateKeepOrBack => is_redaction_gate_choice(key, '2', 'u'),
            ShellBindingId::RedactionGateQuit => is_redaction_gate_choice(key, '3', 'n'),
            ShellBindingId::RedactionGateScroll => {
                key.modifiers.is_empty()
                    && matches!(
                        key.code,
                        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                    )
            }
            ShellBindingId::ToolDetails => is_tool_details_shortcut(key),
            ShellBindingId::ContextInspector => is_context_inspector_shortcut(key),
            ShellBindingId::ProviderRoute => is_provider_route_shortcut(key),
            ShellBindingId::ProviderOffer => key.code == KeyCode::F(7) && key.modifiers.is_empty(),
            ShellBindingId::Help => is_help_shortcut(key),
            ShellBindingId::ExternalEditor => key_shortcuts::is_external_editor_shortcut(key),
            ShellBindingId::Settings => is_settings_shortcut(key),
            ShellBindingId::ModeCycle => is_mode_cycle_shortcut(key),
            ShellBindingId::PermissionCycle => is_permission_cycle_shortcut(key),
            ShellBindingId::ViewCycle => is_view_cycle_shortcut(key),
            ShellBindingId::ViewCycleBack => is_view_cycle_back_shortcut(key),
        }
    }
}

/// The shell's single key-admission authority: which binding, if any, this
/// key presses for this focus owner.
///
/// Callers keep their position in the event loop — that ordering is a real
/// statement about which surface sees a key first — but none of them decides
/// admission any more, and none of them may ask about composer content.
#[must_use]
pub fn route(focus: Focus, key: &KeyEvent) -> Option<ShellBindingId> {
    SHELL_BINDINGS
        .iter()
        .find(|binding| {
            binding.focus.admits(focus)
                && binding.matches(key)
                && !(focus == Focus::TerminalPanel
                    && matches!(
                        binding.id,
                        ShellBindingId::ModeCycle | ShellBindingId::PermissionCycle
                    ))
        })
        .map(|binding| binding.id)
}

/// Canonical shell bindings. Handlers and chrome read from here.
pub const SHELL_BINDINGS: &[ShellBinding] = &[
    ShellBinding {
        id: ShellBindingId::TerminalNew,
        catalog_chord: "Ctrl+N",
        footer_chord: "Ctrl+N",
        focus: FocusScope::TerminalPanel,
    },
    ShellBinding {
        id: ShellBindingId::TerminalNext,
        catalog_chord: "Alt+Down",
        footer_chord: "Alt+Down",
        focus: FocusScope::TerminalPanel,
    },
    ShellBinding {
        id: ShellBindingId::TerminalPrevious,
        catalog_chord: "Alt+Up",
        footer_chord: "Alt+Up",
        focus: FocusScope::TerminalPanel,
    },
    ShellBinding {
        id: ShellBindingId::TerminalDetach,
        catalog_chord: "Esc",
        footer_chord: "Esc",
        focus: FocusScope::TerminalPanel,
    },
    ShellBinding {
        id: ShellBindingId::ElevationUp,
        catalog_chord: "Up",
        footer_chord: "↑",
        focus: FocusScope::Elevation,
    },
    ShellBinding {
        id: ShellBindingId::ElevationDown,
        catalog_chord: "Down",
        footer_chord: "↓",
        focus: FocusScope::Elevation,
    },
    ShellBinding {
        id: ShellBindingId::ElevationConfirm,
        catalog_chord: "Enter",
        footer_chord: "Enter",
        focus: FocusScope::Elevation,
    },
    ShellBinding {
        id: ShellBindingId::ElevationAbort,
        catalog_chord: "Esc",
        footer_chord: "Esc",
        focus: FocusScope::Elevation,
    },
    ShellBinding {
        id: ShellBindingId::PetInspect,
        catalog_chord: "F5 / /pet inspect",
        footer_chord: "F5",
        focus: FocusScope::AnyShell,
    },
    ShellBinding {
        id: ShellBindingId::PetResultUp,
        catalog_chord: "Up",
        footer_chord: "↑",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetResultDown,
        catalog_chord: "Down",
        footer_chord: "↓",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetResultPageUp,
        catalog_chord: "PgUp",
        footer_chord: "PgUp",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetResultPageDown,
        catalog_chord: "PgDn",
        footer_chord: "PgDn",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetBack,
        catalog_chord: "Esc",
        footer_chord: "Esc",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetResultStart,
        catalog_chord: "Home",
        footer_chord: "Home",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetResultEnd,
        catalog_chord: "End",
        footer_chord: "End",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetFocusAgents,
        catalog_chord: "Tab",
        footer_chord: "Tab",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetOpenAgent,
        catalog_chord: "Enter",
        footer_chord: "Enter",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetCopyReply,
        catalog_chord: "c",
        footer_chord: "c",
        focus: FocusScope::PetHabitat,
    },
    ShellBinding {
        id: ShellBindingId::PetSound,
        catalog_chord: "F6",
        footer_chord: "F6",
        focus: FocusScope::Pet,
    },
    ShellBinding {
        id: ShellBindingId::PetBrowser,
        catalog_chord: "F8",
        footer_chord: "F8",
        focus: FocusScope::Pet,
    },
    ShellBinding {
        id: ShellBindingId::PetWindow,
        catalog_chord: "F9",
        footer_chord: "F9",
        focus: FocusScope::Pet,
    },
    ShellBinding {
        id: ShellBindingId::RedactionGateConfirm,
        catalog_chord: "1/Y",
        footer_chord: "1/Y",
        focus: FocusScope::RedactionGate,
    },
    ShellBinding {
        id: ShellBindingId::RedactionGateKeepOrBack,
        catalog_chord: "2/U",
        footer_chord: "2/U",
        focus: FocusScope::RedactionGate,
    },
    ShellBinding {
        id: ShellBindingId::RedactionGateQuit,
        catalog_chord: "3/N",
        footer_chord: "3/N",
        focus: FocusScope::RedactionGate,
    },
    ShellBinding {
        id: ShellBindingId::RedactionGateScroll,
        catalog_chord: "↑/↓",
        footer_chord: "↑/↓",
        focus: FocusScope::RedactionGate,
    },
    ShellBinding {
        id: ShellBindingId::ToolDetails,
        catalog_chord: "Alt+V",
        footer_chord: "Alt+V",
        // The rail claims the details chord for a selected row before the
        // transcript pager sees it; both are session surfaces.
        focus: FocusScope::SessionShell,
    },
    ShellBinding {
        id: ShellBindingId::ContextInspector,
        // `/context` is the guaranteed path; Alt+C stays an unadvertised
        // handler until proven in Cursor/Terminal.app/iTerm2/tmux/PTY.
        catalog_chord: "/context",
        footer_chord: "/context",
        focus: FocusScope::SessionShell,
    },
    ShellBinding {
        id: ShellBindingId::ProviderRoute,
        // `/provider` remains the portable, explicit command path.
        catalog_chord: "F3 / /provider",
        footer_chord: "F3",
        // The route is also pickable before a session exists.
        focus: FocusScope::AnyShell,
    },
    ShellBinding {
        id: ShellBindingId::ProviderOffer,
        catalog_chord: crate::keybinding_table::PROVIDER_OFFER_CHORD,
        footer_chord: crate::keybinding_table::PROVIDER_OFFER_CHORD,
        focus: FocusScope::RoutePicker,
    },
    ShellBinding {
        id: ShellBindingId::ExternalEditor,
        catalog_chord: crate::keybinding_table::EXTERNAL_EDITOR_CHORD,
        footer_chord: "F4",
        focus: FocusScope::AnyShell,
    },
    ShellBinding {
        id: ShellBindingId::Help,
        // `/help` also opens this. F1 is accepted but never advertised on
        // chrome: tmux and several emulators eat it (see
        // [`HELP_CHROME_CHORD`]).
        catalog_chord: "F1 / Ctrl+/",
        footer_chord: HELP_CHROME_CHORD,
        focus: FocusScope::Everywhere,
    },
    ShellBinding {
        id: ShellBindingId::Settings,
        catalog_chord: "F2",
        footer_chord: "F2",
        // Shell-global for the same reason as Help: a settings route that
        // disappears inside onboarding or a modal is not a route.
        focus: FocusScope::Everywhere,
    },
    ShellBinding {
        id: ShellBindingId::ModeCycle,
        catalog_chord: "Tab",
        footer_chord: "Tab",
        // Tab is the shell's mode cycle. The composer's own completions
        // get the key first, but *having typed* never disables it. Live on
        // the launch screen too: the card's rows are arrowed, the composer
        // is always focused, so Tab had no focus left to move and read as
        // dead (0.9.12 defect #5).
        focus: FocusScope::AnyShell,
    },
    ShellBinding {
        id: ShellBindingId::PermissionCycle,
        catalog_chord: "Shift+Tab",
        footer_chord: "Shift+Tab",
        // A shell-level permission control, live wherever the shell is —
        // including the launch screen, where it used to be dead — plus the
        // Config modal that displays the posture it changes.
        focus: FocusScope::AnyShellOrConfig,
    },
    ShellBinding {
        id: ShellBindingId::ViewCycle,
        // Ctrl+Tab only arrives under the kitty keyboard protocol (the loop
        // pushes DISAMBIGUATE_ESCAPE_CODES); Ctrl+] is the chord every
        // terminal delivers — unbound here, not eaten by VS Code/Cursor
        // (Ctrl+F is), tmux, iTerm2, Terminal.app, or Windows Terminal.
        catalog_chord: "Ctrl+Tab / Ctrl+]",
        footer_chord: "Ctrl+]",
        // The launch card advertises this same work-bar control.
        focus: FocusScope::AnyShell,
    },
    ShellBinding {
        id: ShellBindingId::ViewCycleBack,
        catalog_chord: "Ctrl+Shift+Tab",
        footer_chord: "Ctrl+Shift+Tab",
        focus: FocusScope::AnyShell,
    },
];

fn is_redaction_gate_choice(key: &KeyEvent, digit: char, letter: char) -> bool {
    // Modifiers must not turn an unrelated shortcut into consent. Shift is
    // accepted for the uppercase letter advertised in the action rail.
    (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        && matches!(key.code, KeyCode::Char(ch) if ch == digit || ch.eq_ignore_ascii_case(&letter))
}

/// The chord the info line advertises for help.
///
/// Not `F1`: tmux, screen, and several terminal emulators claim it before the
/// shell ever sees the key, so an info line that printed `F1 help` would be
/// advertising a key that does nothing for many users. Not `?` either — bare
/// `?` is composer text in every focus state, and help only answers to
/// `Alt+?`, which stays unadvertised until it is proven in real terminals
/// (TUI-DOG-003). `Ctrl+/` remains accepted, together with its legacy
/// `Ctrl+7` / `Ctrl+_` encodings, but it is no longer what chrome advertises:
/// how a terminal encodes Ctrl+/ varies enough that the printed hint was a
/// promise the product could not keep on the founder's own machine. `/help`
/// is a slash command — it reaches the same view through the composer, it
/// works in every terminal, and typing `/` already reveals it.
pub const HELP_CHROME_CHORD: &str = "/help";

/// The info line's single right-hand key hint.
///
/// A slash command names itself, so it prints bare (`/help`); a key chord
/// still needs the word (`Ctrl+/ help`).
#[must_use]
pub fn info_help_hint(locale: codewhale_localization::Locale) -> String {
    let chord = binding(ShellBindingId::Help).footer_chord;
    if chord.starts_with('/') {
        return chord.to_string();
    }
    format!(
        "{} {}",
        chord,
        codewhale_localization::tr(locale, codewhale_localization::MessageId::InfoLineHelp)
    )
}

#[must_use]
pub fn binding(id: ShellBindingId) -> &'static ShellBinding {
    SHELL_BINDINGS
        .iter()
        .find(|binding| binding.id == id)
        .expect("shell binding catalog is exhaustive")
}

/// Platform-aware chord for opening complete tool or approval details.
#[must_use]
pub fn tool_details_chord() -> Cow<'static, str> {
    display_chord(binding(ShellBindingId::ToolDetails).footer_chord)
}

/// Render a portable `Alt+X` chord for the current platform. macOS normally
/// shows `⌥X` and `fn+F5`, because an Apple keyboard sends media keys from the
/// top row unless Fn is held; ASCII-safe terminals retain the portable spelling.
#[must_use]
pub fn display_chord(chord: &'static str) -> Cow<'static, str> {
    display_chord_for_platform_and_ascii(
        chord,
        cfg!(target_os = "macos"),
        crate::tui::color_compat::ascii_safe_enabled(),
    )
}

#[cfg(test)]
#[must_use]
pub fn display_chord_for_platform(chord: &'static str, is_macos: bool) -> Cow<'static, str> {
    display_chord_for_platform_and_ascii(chord, is_macos, false)
}

fn display_chord_for_platform_and_ascii(
    chord: &'static str,
    is_macos: bool,
    ascii_safe: bool,
) -> Cow<'static, str> {
    if !is_macos {
        return Cow::Borrowed(chord);
    }
    let mut rendered = String::with_capacity(chord.len() + 6);
    let mut previous = None;
    for (index, ch) in chord.char_indices() {
        if ch == 'F'
            && !previous.is_some_and(char::is_alphanumeric)
            && chord[index + 1..].starts_with(|next: char| next.is_ascii_digit())
        {
            rendered.push_str("fn+");
        }
        rendered.push(ch);
        previous = Some(ch);
    }
    let rendered = if ascii_safe {
        rendered
    } else {
        rendered.replace("Alt+", "⌥")
    };
    if rendered == chord {
        Cow::Borrowed(chord)
    } else {
        Cow::Owned(rendered)
    }
}

/// Details/output opens only on Option+V (macOS legacy `√`) or Alt+V.
/// Bare `v` always types `v` — never a shortcut, in any focus state.
#[must_use]
pub fn is_tool_details_shortcut(key: &KeyEvent) -> bool {
    if key_shortcuts::is_macos_option_v_legacy_key(key) {
        return true;
    }
    matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
        && key_shortcuts::alt_nav_modifiers(key.modifiers)
}

#[must_use]
pub fn is_context_inspector_shortcut(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && key_shortcuts::alt_nav_modifiers(key.modifiers)
}

/// Route entry stays on a non-printable function key so it never steals a
/// model/provider name from the composer. `/provider` remains available in
/// terminals that do not forward function keys.
#[must_use]
pub fn is_provider_route_shortcut(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(3)) && key.modifiers.is_empty()
}

#[must_use]
pub fn is_help_shortcut(key: &KeyEvent) -> bool {
    if matches!(key.code, KeyCode::F(1)) {
        return true;
    }
    // Windows delivers AltGr as Ctrl+Alt, so a layout-emitted glyph (e.g.
    // AltGr+Q typing '/' on ABNT2) would satisfy a bare CONTROL check.
    // AltGr chords are text, never shortcuts (#4723).
    let altgr = crate::tui::widgets::key_hint::is_altgr(key.modifiers);
    if matches!(key.code, KeyCode::Char('/'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !altgr
    {
        return true;
    }
    // Some legacy terminal stacks encode Ctrl+/ as the ASCII unit separator,
    // which crossterm reports as Ctrl+7 or Ctrl+_. Accept both portable
    // decodings so the documented fallback remains real.
    if matches!(key.code, KeyCode::Char('7') | KeyCode::Char('_'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !altgr
    {
        return true;
    }
    // Alt+? still opens help where the terminal delivers it, but it is not
    // advertised anywhere (TUI-DOG-003).
    matches!(key.code, KeyCode::Char('?')) && key_shortcuts::alt_nav_modifiers(key.modifiers)
}

#[must_use]
pub fn is_settings_shortcut(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(2)) && key.modifiers.is_empty()
}

/// Tab cycles the session mode. Terminal chords that mean something else to
/// the host (Ctrl/Alt/Cmd+Tab) are not ours, and Shift+Tab is the permission
/// cycle below.
#[must_use]
pub fn is_mode_cycle_shortcut(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Tab)
        && !key.modifiers.intersects(
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::SHIFT,
        )
}

/// Shift+Tab cycles the permission posture. Terminals encode the same chord
/// either as `BackTab` or as `Tab` + SHIFT; accept both.
#[must_use]
pub fn is_permission_cycle_shortcut(key: &KeyEvent) -> bool {
    let forbidden = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
    if key.modifiers.intersects(forbidden) {
        return false;
    }
    matches!(key.code, KeyCode::BackTab)
        || (matches!(key.code, KeyCode::Tab) && key.modifiers.contains(KeyModifiers::SHIFT))
}

/// Ctrl+Tab (kitty protocol: `Tab` + CONTROL) or Ctrl+] cycles the bottom
/// dock view forward. Legacy terminals send Ctrl+] as ASCII 0x1d, which
/// crossterm decodes as Ctrl+5. AltGr chords stay text (#4723).
#[must_use]
pub fn is_view_cycle_shortcut(key: &KeyEvent) -> bool {
    if crate::tui::widgets::key_hint::is_altgr(key.modifiers) {
        return false;
    }
    let ctrl_only = key.modifiers.contains(KeyModifiers::CONTROL)
        && !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::SHIFT);
    ctrl_only && matches!(key.code, KeyCode::Tab | KeyCode::Char(']' | '5'))
}

/// Ctrl+Shift+Tab (kitty protocol: `BackTab` or `Tab` with CONTROL|SHIFT)
/// cycles the bottom dock view backward.
#[must_use]
pub fn is_view_cycle_back_shortcut(key: &KeyEvent) -> bool {
    if key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
        || !key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return false;
    }
    matches!(key.code, KeyCode::BackTab)
        || (matches!(key.code, KeyCode::Tab) && key.modifiers.contains(KeyModifiers::SHIFT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_cycle_chords_are_ctrl_tab_and_ctrl_bracket() {
        let ctrl_tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL);
        let ctrl_bracket = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL);
        let ctrl_shift_tab = KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(
            route(Focus::Composer, &ctrl_tab),
            Some(ShellBindingId::ViewCycle)
        );
        assert_eq!(
            route(Focus::Panel, &ctrl_bracket),
            Some(ShellBindingId::ViewCycle)
        );
        assert_eq!(
            route(Focus::Composer, &ctrl_shift_tab),
            Some(ShellBindingId::ViewCycleBack)
        );
        // Plain Tab / Shift+Tab stay the mode and permission cycles.
        assert_eq!(
            route(
                Focus::Composer,
                &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
            ),
            Some(ShellBindingId::ModeCycle)
        );
        assert_eq!(
            route(
                Focus::Composer,
                &KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
            ),
            Some(ShellBindingId::PermissionCycle)
        );
        // Ctrl+T is reasoning effort, not ours; a bare `]` is composer text.
        assert_eq!(
            route(
                Focus::Composer,
                &KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)
            ),
            None
        );
        assert_eq!(
            route(
                Focus::Composer,
                &KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE)
            ),
            None
        );
        // Modals keep every key.
        assert_eq!(route(Focus::Modal(ModalKind::Pager), &ctrl_bracket), None);
    }

    #[test]
    fn work_bar_accepts_legacy_and_enhanced_keys_on_launch() {
        for code in [KeyCode::Tab, KeyCode::Char(']'), KeyCode::Char('5')] {
            let key = KeyEvent::new(code, KeyModifiers::CONTROL);
            assert_eq!(route(Focus::Launch, &key), Some(ShellBindingId::ViewCycle));
            assert_eq!(route(Focus::Onboarding, &key), None);
            assert_eq!(route(Focus::Modal(ModalKind::Pager), &key), None);
        }
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ] {
            assert!(!is_view_cycle_shortcut(&KeyEvent::new(
                KeyCode::Char('5'),
                modifiers
            )));
        }
        let backwards = KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(
            route(Focus::Launch, &backwards),
            Some(ShellBindingId::ViewCycleBack)
        );
    }

    #[test]
    fn bare_v_is_never_a_shortcut_in_any_state() {
        // TUI-DOG-002: bare `v` always types `v`; there is no focus state in
        // which it opens details, so the matcher takes no focus argument.
        let plain_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE);
        assert!(!is_tool_details_shortcut(&plain_v));
        let plain_upper_v = KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT);
        assert!(!is_tool_details_shortcut(&plain_upper_v));
    }

    #[test]
    fn alt_v_and_macos_option_v_open_details() {
        let alt_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
        assert!(is_tool_details_shortcut(&alt_v));
        let alt_upper_v = KeyEvent::new(KeyCode::Char('V'), KeyModifiers::ALT);
        assert!(is_tool_details_shortcut(&alt_upper_v));
    }

    #[test]
    fn details_label_is_option_glyph_on_macos_and_alt_elsewhere() {
        assert_eq!(display_chord_for_platform("Alt+V", true), "⌥V");
        assert_eq!(display_chord_for_platform("Alt+V", false), "Alt+V");
    }

    #[test]
    fn chrome_never_advertises_a_key_terminals_eat() {
        // F1 stays in the catalog (it works where delivered) but no chrome
        // hint may print it; the help hint is derived from the binding.
        // Ctrl+/ is still accepted, but chrome advertises the route that
        // works in every terminal.
        assert_eq!(binding(ShellBindingId::Help).footer_chord, "/help");
        let hint = info_help_hint(codewhale_localization::Locale::En);
        assert_eq!(hint, "/help", "a slash command names itself");
        assert!(
            is_help_shortcut(&KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL)),
            "Ctrl+/ must keep working for the terminals that deliver it"
        );
        for binding in SHELL_BINDINGS {
            assert!(!binding.footer_chord.contains("F1"), "{:?}", binding.id);
            assert!(!binding.footer_chord.contains("Alt+?"), "{:?}", binding.id);
        }
    }

    #[test]
    fn help_accepts_f1_ctrl_slash_and_unadvertised_fallbacks() {
        assert!(is_help_shortcut(&KeyEvent::new(
            KeyCode::F(1),
            KeyModifiers::NONE
        )));
        assert!(is_help_shortcut(&KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::CONTROL
        )));
        assert!(is_help_shortcut(&KeyEvent::new(
            KeyCode::Char('7'),
            KeyModifiers::CONTROL
        )));
        assert!(is_help_shortcut(&KeyEvent::new(
            KeyCode::Char('_'),
            KeyModifiers::CONTROL
        )));
        // Unadvertised but accepted where the terminal delivers them.
        assert!(is_help_shortcut(&KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::ALT
        )));
        let inverted_question = KeyEvent::new(KeyCode::Char('\u{00bf}'), KeyModifiers::NONE);
        assert!(!is_help_shortcut(&inverted_question));
    }

    #[test]
    fn altgr_slash_types_text_instead_of_opening_help() {
        // Windows encodes AltGr as Ctrl+Alt: AltGr+Q on ABNT2 delivers '/'
        // with CONTROL|ALT and must reach the composer as text (#4723).
        let altgr_slash = KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let altgr_seven = KeyEvent::new(
            KeyCode::Char('7'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        if cfg!(windows) {
            assert!(!is_help_shortcut(&altgr_slash));
            assert!(!is_help_shortcut(&altgr_seven));
        } else {
            // Elsewhere Ctrl+Alt is a deliberate chord and keeps working.
            assert!(is_help_shortcut(&altgr_slash));
            assert!(is_help_shortcut(&altgr_seven));
        }
        // Plain Ctrl+/ still opens help everywhere.
        assert!(is_help_shortcut(&KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn settings_accepts_only_plain_f2() {
        assert!(is_settings_shortcut(&KeyEvent::new(
            KeyCode::F(2),
            KeyModifiers::NONE
        )));
        assert!(!is_settings_shortcut(&KeyEvent::new(
            KeyCode::F(2),
            KeyModifiers::SHIFT
        )));
        assert!(!is_settings_shortcut(&KeyEvent::new(
            KeyCode::F(1),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn context_accepts_explicit_alt_c_without_stealing_layout_characters() {
        let alt_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT);
        assert!(is_context_inspector_shortcut(&alt_c));
        let cedilla = KeyEvent::new(KeyCode::Char('\u{00e7}'), KeyModifiers::NONE);
        assert!(!is_context_inspector_shortcut(&cedilla));
    }

    #[test]
    fn infoline_route_f3_requires_a_plain_function_key() {
        assert!(is_provider_route_shortcut(&KeyEvent::new(
            KeyCode::F(3),
            KeyModifiers::NONE
        )));
        assert!(!is_provider_route_shortcut(&KeyEvent::new(
            KeyCode::F(3),
            KeyModifiers::ALT
        )));
        assert!(!is_provider_route_shortcut(&KeyEvent::new(
            KeyCode::Char('3'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn chrome_only_advertises_chords_that_are_live_where_it_is_shown() {
        // The metrics line's help hint is built from this table, so it must
        // not be able to name a chord the same table refuses at any focus
        // the chrome is rendered in (the hint paints on every screen).
        let hint = info_help_hint(codewhale_localization::Locale::En);
        let help = binding(ShellBindingId::Help);
        assert!(hint.starts_with(help.footer_chord), "{hint}");
        assert_eq!(help.focus, FocusScope::Everywhere);
        assert!(help.matches(&KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn elevation_bindings_are_nontext_and_stay_in_the_elevation_card() {
        for (code, id) in [
            (KeyCode::Up, ShellBindingId::ElevationUp),
            (KeyCode::Down, ShellBindingId::ElevationDown),
            (KeyCode::Enter, ShellBindingId::ElevationConfirm),
            (KeyCode::Esc, ShellBindingId::ElevationAbort),
        ] {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            assert_eq!(route(Focus::Modal(ModalKind::Elevation), &key), Some(id));
            assert!(!binding(id).footer_chord.is_empty());
            for focus in [
                Focus::Composer,
                Focus::Launch,
                Focus::Modal(ModalKind::Approval),
            ] {
                assert_ne!(route(focus, &key), Some(id));
            }
            for modifiers in [
                KeyModifiers::ALT,
                KeyModifiers::CONTROL,
                KeyModifiers::SUPER,
                KeyModifiers::SHIFT,
            ] {
                assert_ne!(
                    route(
                        Focus::Modal(ModalKind::Elevation),
                        &KeyEvent::new(code, modifiers)
                    ),
                    Some(id)
                );
            }
        }
        for letter in "nwfajkN W F A J K123".chars() {
            assert_eq!(
                route(
                    Focus::Modal(ModalKind::Elevation),
                    &KeyEvent::new(KeyCode::Char(letter), KeyModifiers::NONE)
                ),
                None
            );
        }
    }

    #[test]
    fn focus_scopes_nest_from_the_session_outwards() {
        let ladder = [
            FocusScope::SessionShell,
            FocusScope::AnyShell,
            FocusScope::AnyShellOrConfig,
            FocusScope::Everywhere,
        ];
        let states = [
            Focus::RedactionGate,
            Focus::Composer,
            Focus::Panel,
            Focus::Launch,
            Focus::Modal(ModalKind::Config),
            Focus::Modal(ModalKind::Approval),
            Focus::Onboarding,
        ];
        for pair in ladder.windows(2) {
            for focus in states {
                assert!(
                    !pair[0].admits(focus) || pair[1].admits(focus),
                    "{:?} admits {focus:?} but the wider {:?} does not",
                    pair[0],
                    pair[1]
                );
            }
        }
        // Nothing but Help/Settings may reach a focused workflow.
        for binding in SHELL_BINDINGS {
            assert_eq!(
                binding.focus.admits(Focus::Modal(ModalKind::Approval)),
                binding.focus == FocusScope::Everywhere,
                "{:?} must not reach across an approval decision",
                binding.id
            );
        }
    }

    #[test]
    fn tab_and_shift_tab_are_distinct_bindings() {
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let shift_tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT);
        assert_eq!(
            route(Focus::Composer, &tab),
            Some(ShellBindingId::ModeCycle)
        );
        assert_eq!(
            route(Focus::Composer, &shift_tab),
            Some(ShellBindingId::PermissionCycle)
        );
        // Both shell controls are live on the launch stage as well.
        assert_eq!(route(Focus::Launch, &tab), Some(ShellBindingId::ModeCycle));
        assert_eq!(
            route(Focus::Launch, &shift_tab),
            Some(ShellBindingId::PermissionCycle)
        );
    }

    #[test]
    fn redaction_choices_require_the_gate_and_explicit_unmodified_keys() {
        for (id, keys) in [
            (ShellBindingId::RedactionGateConfirm, ['1', 'y', 'Y']),
            (ShellBindingId::RedactionGateKeepOrBack, ['2', 'u', 'U']),
            (ShellBindingId::RedactionGateQuit, ['3', 'n', 'N']),
        ] {
            for key in keys {
                for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                    let event = KeyEvent::new(KeyCode::Char(key), modifiers);
                    assert_eq!(route(Focus::RedactionGate, &event), Some(id));
                    for focus in [
                        Focus::Composer,
                        Focus::Panel,
                        Focus::Launch,
                        Focus::Onboarding,
                        Focus::Modal(ModalKind::Approval),
                    ] {
                        assert_eq!(route(focus, &event), None, "{focus:?}: {event:?}");
                    }
                }
                for modifiers in [
                    KeyModifiers::CONTROL,
                    KeyModifiers::ALT,
                    KeyModifiers::SUPER,
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ] {
                    assert_eq!(
                        route(
                            Focus::RedactionGate,
                            &KeyEvent::new(KeyCode::Char(key), modifiers)
                        ),
                        None
                    );
                }
            }
        }
        for code in [KeyCode::Enter, KeyCode::F(1), KeyCode::F(2), KeyCode::Tab] {
            assert_eq!(
                route(
                    Focus::RedactionGate,
                    &KeyEvent::new(code, KeyModifiers::NONE)
                ),
                None
            );
        }
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ] {
            assert_eq!(
                route(
                    Focus::RedactionGate,
                    &KeyEvent::new(code, KeyModifiers::NONE)
                ),
                Some(ShellBindingId::RedactionGateScroll)
            );
        }
    }

    #[test]
    fn catalog_chords_match_final_contract() {
        assert_eq!(binding(ShellBindingId::Help).catalog_chord, "F1 / Ctrl+/");
        assert_eq!(
            binding(ShellBindingId::ContextInspector).catalog_chord,
            "/context"
        );
        assert_eq!(binding(ShellBindingId::ToolDetails).catalog_chord, "Alt+V");
        assert_eq!(
            binding(ShellBindingId::ProviderRoute).catalog_chord,
            "F3 / /provider"
        );
        for binding in SHELL_BINDINGS {
            assert!(!binding.catalog_chord.contains("Alt+?"));
            assert_ne!(binding.catalog_chord, "v");
            assert!(!binding.catalog_chord.starts_with("v /"));
            assert!(!binding.footer_chord.contains("Alt+?"));
            assert_ne!(binding.footer_chord, "v");
        }
    }

    #[test]
    fn shell_binding_source_matches_help_catalog_chords() {
        assert_eq!(binding(ShellBindingId::ToolDetails).catalog_chord, "Alt+V");
        assert_eq!(
            binding(ShellBindingId::ContextInspector).catalog_chord,
            "/context"
        );
        assert_eq!(
            binding(ShellBindingId::ProviderRoute).catalog_chord,
            "F3 / /provider"
        );
        assert_eq!(binding(ShellBindingId::Help).catalog_chord, "F1 / Ctrl+/");
        // Every ordinary shell binding: exclusive redaction choices are
        // advertised only by their consent screen, never as global shortcuts.
        // The two that were
        // not covered had already drifted (the view-cycle chord read
        // "Ctrl+] / Ctrl+Tab" here and "Ctrl+Tab / Ctrl+]" at the source),
        // which is exactly the rot this module's doc comment warns about.
        for id in [
            ShellBindingId::ToolDetails,
            ShellBindingId::ContextInspector,
            ShellBindingId::ProviderRoute,
            ShellBindingId::Help,
            ShellBindingId::Settings,
            ShellBindingId::ModeCycle,
            ShellBindingId::PermissionCycle,
            ShellBindingId::ViewCycle,
            ShellBindingId::ViewCycleBack,
        ] {
            let chord = binding(id).catalog_chord;
            assert!(
                crate::keybinding_table::KEYBINDINGS
                    .iter()
                    .any(|entry| entry.chord == chord || entry.chord.contains(chord)),
                "shell binding {id:?} chord missing from help catalog: {chord}"
            );
        }
    }
}
