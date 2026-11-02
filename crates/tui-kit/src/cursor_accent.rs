//! Capability-gated OSC 12 cursor accent support.
//!
//! OSC 12 changes the terminal cursor color and OSC 112 restores the terminal
//! default. The guard is deliberately conservative: an explicit supported
//! terminal marker is required, while `TERM=dumb`, `NO_COLOR` (monochrome
//! color depth), and reduced-motion policy suppress the decorative escape
//! entirely.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use codewhale_palette::{ColorDepth, WHALE_ACTION_RGB};
use ratatui::style::Color;

const OSC12_RESET: &[u8] = b"\x1b]112\x07";
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// RAII handle for one process-wide cursor accent installation.
pub struct CursorAccentGuard {
    active: bool,
}

impl CursorAccentGuard {
    /// Install the accent only when the resolved settings and environment make
    /// decorative terminal control safe and explicit.
    pub fn install(reduced_motion: bool, accent: Color) -> Self {
        if reduced_motion || !environment_allows_cursor_accent() {
            return Self { active: false };
        }

        let mut stdout = io::stdout();
        if write_cursor_accent(&mut stdout, color_rgb(accent)).is_ok() {
            ACTIVE.store(true, Ordering::SeqCst);
            Self { active: true }
        } else {
            Self { active: false }
        }
    }
}

fn color_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(red, green, blue) => (red, green, blue),
        _ => WHALE_ACTION_RGB,
    }
}

impl Drop for CursorAccentGuard {
    fn drop(&mut self) {
        if self.active {
            restore_cursor_accent();
        }
    }
}

/// Restore the terminal's default cursor color once. Safe from normal,
/// panic, and signal cleanup paths; repeated calls are no-ops.
pub fn restore_cursor_accent() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut stdout = io::stdout();
    let _ = stdout.write_all(OSC12_RESET).and_then(|()| stdout.flush());
}

fn write_cursor_accent<W: Write>(
    writer: &mut W,
    (red, green, blue): (u8, u8, u8),
) -> io::Result<()> {
    write!(writer, "\x1b]12;#{red:02X}{green:02X}{blue:02X}\x07")?;
    writer.flush()
}

fn environment_allows_cursor_accent() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let color_term = std::env::var("COLORTERM").unwrap_or_default();
    let reduced_motion = std::env::var("NO_ANIMATIONS")
        .ok()
        .is_some_and(|value| env_truthy(&value));
    let no_color = ColorDepth::detect() == ColorDepth::Monochrome;
    cursor_accent_supported(
        Some(&term_program),
        Some(&term),
        Some(&color_term),
        reduced_motion,
        no_color,
    )
}

fn cursor_accent_supported(
    term_program: Option<&str>,
    term: Option<&str>,
    color_term: Option<&str>,
    reduced_motion: bool,
    no_color: bool,
) -> bool {
    if reduced_motion || no_color || term == Some("dumb") {
        return false;
    }

    let program = term_program.unwrap_or_default().to_ascii_lowercase();
    let known_terminal = matches!(
        program.as_str(),
        "alacritty"
            | "apple_terminal"
            | "contour"
            | "ghostty"
            | "iterm.app"
            | "kitty"
            | "konsole"
            | "rio"
            | "vscode"
            | "wezterm"
            | "windows_terminal"
    );
    known_terminal && (color_term.is_some_and(|value| !value.is_empty()) || term.is_some())
}

fn env_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_terminals_are_explicitly_allowlisted() {
        assert!(cursor_accent_supported(
            Some("Ghostty"),
            Some("xterm-256color"),
            Some("truecolor"),
            false,
            false
        ));
        assert!(cursor_accent_supported(
            Some("kitty"),
            Some("xterm-kitty"),
            Some("truecolor"),
            false,
            false
        ));
        assert!(!cursor_accent_supported(
            Some("unknown-terminal"),
            Some("xterm-256color"),
            Some("truecolor"),
            false,
            false
        ));
    }

    #[test]
    fn plain_and_reduced_motion_terminals_are_suppressed() {
        assert!(!cursor_accent_supported(
            Some("Ghostty"),
            Some("dumb"),
            Some("truecolor"),
            false,
            false
        ));
        assert!(!cursor_accent_supported(
            Some("Ghostty"),
            Some("xterm-256color"),
            Some("truecolor"),
            true,
            false
        ));
    }

    #[test]
    fn no_color_suppresses_the_accent_on_supported_terminals() {
        assert!(!cursor_accent_supported(
            Some("Ghostty"),
            Some("xterm-256color"),
            Some("truecolor"),
            false,
            true
        ));
        assert!(!cursor_accent_supported(
            Some("kitty"),
            Some("xterm-kitty"),
            None,
            false,
            true
        ));
    }

    #[test]
    fn cursor_sequences_set_and_restore_the_default() {
        let mut output = Vec::new();
        write_cursor_accent(&mut output, (0x12, 0xab, 0xf0)).unwrap();
        assert_eq!(
            output, b"\x1b]12;#12ABF0\x07",
            "OSC 12 must use the existing accent as an RGB cursor color"
        );
        assert_eq!(OSC12_RESET, b"\x1b]112\x07");
    }

    #[test]
    fn non_rgb_themes_fall_back_to_the_existing_accent() {
        assert_eq!(color_rgb(Color::Blue), WHALE_ACTION_RGB);
        assert_eq!(color_rgb(Color::Rgb(1, 2, 3)), (1, 2, 3));
    }

    #[test]
    fn truthy_environment_values_are_conservative() {
        assert!(env_truthy("1"));
        assert!(env_truthy(" TRUE "));
        assert!(!env_truthy("0"));
        assert!(!env_truthy("false"));
    }
}
