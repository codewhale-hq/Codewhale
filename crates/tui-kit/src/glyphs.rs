//! Codewhale's terminal glyph charter.
//!
//! Renderers use semantic names from this module instead of choosing visual
//! punctuation ad hoc. The solid current marker (`●`) is the recurring
//! Codewhale anchor: it marks the active speaker or current human choice.
//! ASCII-safe terminals receive the semantic fallback from the same owner.

/// Current speaker or current human choice — the recurring identity anchor.
pub const CURRENT: &str = "●";
/// Available but not current.
pub const AVAILABLE: &str = "○";
/// Keyboard/list selection pointer.
pub const SELECTION: &str = "▸";
/// Finished user-authored message marker.
pub const USER: &str = "▎";
/// Transcript continuation rail, including its authored trailing space.
pub const TRANSCRIPT_RAIL: &str = "▏ ";
/// Settled successful state.
pub const DONE: &str = "✓";
/// Settled failed state.
pub const FAILED: &str = "✕";
/// State that needs human attention.
pub const ATTENTION: &str = "◆";
/// Ready but not active.
pub const READY: &str = "○";
/// Paused work.
pub const PAUSED: &str = "⏸";
/// Fleet role marks share the same charter while retaining distinct shapes.
pub const ROLE_MANAGER: &str = "◆";
pub const ROLE_BUILDER: &str = "■";
pub const ROLE_REVIEWER: &str = "◇";
pub const ROLE_VERIFIER: &str = CURRENT;
pub const ROLE_SYNTHESIZER: &str = "▲";
pub const NEUTRAL: &str = "·";

#[must_use]
pub const fn selection_marker(selected: bool) -> &'static str {
    if selected { SELECTION } else { " " }
}

/// Reduce a single Codewhale-authored decorative glyph to narrow ASCII.
/// Language text and model/user content are intentionally outside this map.
#[must_use]
pub fn ascii_fallback(symbol: &str) -> Option<&'static str> {
    match symbol {
        "─" | "━" | "═" | "╌" | "╍" | "┄" | "┅" | "┈" | "┉" | "—" | "–" => {
            Some("-")
        }
        "│" | "┃" | "║" | "╎" | "╏" | "▏" | "▎" | "▍" | "▌" | "▐" | "▕" => {
            Some("|")
        }
        "┌" | "┐" | "└" | "┘" | "╭" | "╮" | "╰" | "╯" | "├" | "┤" | "┬" | "┴" | "┼" => {
            Some("+")
        }
        "█" | "▉" | "▊" | "▋" | "▀" | "▄" | "▅" | "▆" | "▇" | "▙" | "▛" | "▜" | "▟" | "▰" => {
            Some("#")
        }
        "▁" | "▂" | "▃" => Some("_"),
        // Tideline action glyphs (spec §2): one cell each, no wide glyphs.
        "⌁" => Some("+"),
        "⚙" => Some("*"),
        "↺" => Some("<"),
        "▤" => Some("="),
        "◐" => Some("*"),
        "⑂" => Some("y"),
        "∼" | "∿" => Some("~"),
        "⋯" => Some("."),
        "▖" | "▗" | "▘" | "▝" => Some("."),
        "▚" => Some("\\"),
        "▞" => Some("/"),
        "░" | "▒" | "▓" => Some(":"),
        "▱" => Some("-"),
        "▶" | "▷" | "▸" | "›" | "❯" | "→" | "↗" | "↘" | "»" => Some(">"),
        "◀" | "◂" | "‹" | "❮" | "←" | "↖" | "↙" | "«" => Some("<"),
        "▼" | "▾" | "▽" | "↓" => Some("v"),
        "▲" | "△" | "↑" => Some("^"),
        "◆" | "◇" | "♦" | "✦" | "◍" | "◉" | "★" | "☆" => Some("*"),
        "■" | "□" | "▪" | "▫" | "◼" | "◻" => Some("#"),
        // Filled marks stay a dot; hollow ones become `o` so CURRENT and
        // AVAILABLE stay distinguishable on ASCII terminals.
        "●" | "∘" | "•" | "·" => Some("."),
        "○" | "☐" | "◌" | "˚" | "°" | "◦" => Some("o"),
        "✓" | "✔" | "☑" => Some("Y"),
        "✕" | "×" | "⊘" | "✗" | "✘" | "☒" => Some("X"),
        "⏸" => Some("="),
        // Schedule/timer (the activity band's automation slot) — cron's `@`.
        "⏱" => Some("@"),
        // The launch warning line's gate glyph ("no model connected").
        "⚠" => Some("!"),
        // The working screen's MCP chip marker (`⋮ MCP n/m`).
        "⋮" => Some("|"),
        "≈≈>" => Some("~>"),
        "≈" | "～" => Some("~"),
        "🐳" | "🐋" => Some("w"),
        "…" => Some("."),
        // Affordance and menu glyphs. Each fallback is one ASCII cell: the
        // backend adapter swaps a single cell's symbol, so a wider spelling
        // would shift every cell after it on the row.
        "✎" => Some("e"),
        "↳" | "⇒" => Some(">"),
        "↩" => Some("<"),
        "⎇" => Some("y"),
        "◔" | "◎" => Some("*"),
        "⎘" | "⧉" => Some("="),
        "▣" | "⌘" => Some("#"),
        "⚑" => Some("!"),
        // A joined line break in a command preview: `;` keeps it reading as
        // a separate command rather than a redirect.
        "⏎" => Some(";"),
        "⇧" => Some("^"),
        "⌥" => Some("~"),
        "ⓘ" => Some("i"),
        _ => None,
    }
}

/// Preserve the working-bubble fill signal when Braille is unavailable.
#[must_use]
pub fn braille_ascii_fallback(ch: char) -> Option<&'static str> {
    if !(('\u{2800}'..='\u{28FF}').contains(&ch)) {
        return None;
    }
    let dots = ((ch as u32) - 0x2800).count_ones();
    Some(match dots {
        0 => " ",
        1..=2 => ".",
        3..=4 => ":",
        5..=6 => "+",
        _ => "#",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charter_has_narrow_semantic_fallbacks() {
        for (rich, safe) in [
            (SELECTION, ">"),
            ("▷", ">"),
            (CURRENT, "."),
            (AVAILABLE, "o"),
            (READY, "o"),
            ("☐", "o"),
            ("•", "."),
            (NEUTRAL, "."),
            (USER, "|"),
            (DONE, "Y"),
            (FAILED, "X"),
            (ATTENTION, "*"),
            ("≈≈>", "~>"),
            ("≈", "~"),
            ("～", "~"),
            ("⌁", "+"),
            ("↺", "<"),
            ("▤", "="),
            ("◐", "*"),
            ("⑂", "y"),
            ("∼", "~"),
            ("∿", "~"),
            ("⋯", "."),
            ("⏱", "@"),
            ("🐳", "w"),
            ("🐋", "w"),
            ("✎", "e"),
            ("↳", ">"),
            ("⎇", "y"),
            ("◔", "*"),
            ("⎘", "="),
            ("▣", "#"),
            ("⧉", "="),
            ("⚑", "!"),
            ("⏎", ";"),
            ("⇧", "^"),
            ("⌥", "~"),
            ("⌘", "#"),
            ("⇒", ">"),
            ("◎", "*"),
            ("↩", "<"),
            ("ⓘ", "i"),
        ] {
            assert_eq!(ascii_fallback(rich), Some(safe));
        }
        assert_ne!(
            ascii_fallback(CURRENT),
            ascii_fallback(AVAILABLE),
            "current and available must stay distinct in ASCII"
        );
        assert_eq!(braille_ascii_fallback('\u{2801}'), Some("."));
        // The backend adapter replaces one cell's symbol: every single-glyph
        // fallback must stay one ASCII cell or the rest of the row shifts.
        for (rich, safe) in [("…", "."), ("✎", "e"), ("ⓘ", "i")] {
            assert_eq!(ascii_fallback(rich), Some(safe));
            assert_eq!(unicode_width::UnicodeWidthStr::width(safe), 1);
        }
        assert_eq!(braille_ascii_fallback('A'), None);
    }
}
