//! Codewhale's terminal glyph charter.
//!
//! Renderers use semantic names from this module instead of choosing visual
//! punctuation ad hoc. The solid current marker (`●`) is the recurring
//! Codewhale anchor: it marks the active speaker or current human choice.
//! ASCII-safe terminals receive the semantic fallback from the same owner.
//!
//! Extracted from the Codewhale engine's `crates/tui/src/tui/glyphs.rs`
//! (`Hmbown/CodeWhale` `58b1dd3dd`, last changed in `d7712814e`). One meaning
//! per glyph: `●` current or running, `✓` done, `✕` failed, `◆` needs you,
//! `○` ready, `■` stopped, `▸` selection, `?` unknown.

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
/// Stopped by a person, or cancelled.
pub const STOPPED: &str = "■";
/// A state nobody reported. Never shown as a failure.
pub const UNKNOWN: &str = "?";
/// Blocked state waiting on a hard precondition or external gate.
pub const BLOCKED: &str = "⊘";
/// Resting state between active runs.
pub const RESTING: &str = "◡";
/// Asleep or suspended state.
pub const ASLEEP: &str = "☾";
/// The one ellipsis.
pub const ELLIPSIS: &str = "…";
/// Typographic minus sign for signed numbers and diffs.
pub const MINUS: &str = "\u{2212}";
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

/// `symbol`, or its ASCII fallback when `ascii` is set. Symbols without a
/// fallback are returned unchanged.
#[must_use]
pub fn pick(symbol: &'static str, ascii: bool) -> &'static str {
    if ascii {
        ascii_fallback(symbol).unwrap_or(symbol)
    } else {
        symbol
    }
}

/// Reduce a single Codewhale-authored decorative glyph to narrow ASCII.
/// Language text and model/user content are intentionally outside this map.
#[must_use]
pub fn ascii_fallback(symbol: &str) -> Option<&'static str> {
    match symbol {
        "─" | "━" | "═" | "╌" | "╍" | "┄" | "┅" | "┈" | "┉" | "—" | "–" | "\u{2212}" => {
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
        "✕" | "×" | "✗" | "✘" | "☒" => Some("X"),
        "⊘" => Some("!"),
        "◡" => Some("~"),
        "☾" => Some("z"),
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
        "▏ " => Some("| "),
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
            (STOPPED, "#"),
            (BLOCKED, "!"),
            (RESTING, "~"),
            (ASLEEP, "z"),
            (ELLIPSIS, "."),
            (MINUS, "-"),
        ] {
            assert_eq!(ascii_fallback(rich), Some(safe));
        }
        assert_ne!(
            ascii_fallback(CURRENT),
            ascii_fallback(AVAILABLE),
            "current and available must stay distinct in ASCII"
        );
        assert_eq!(braille_ascii_fallback('\u{2801}'), Some("."));
        assert_eq!(braille_ascii_fallback('A'), None);
    }

    #[test]
    fn every_public_mark_has_an_ascii_form() {
        for mark in [
            CURRENT,
            AVAILABLE,
            SELECTION,
            USER,
            TRANSCRIPT_RAIL,
            DONE,
            FAILED,
            ATTENTION,
            READY,
            PAUSED,
            STOPPED,
            UNKNOWN,
            BLOCKED,
            RESTING,
            ASLEEP,
            ELLIPSIS,
            MINUS,
            ROLE_MANAGER,
            ROLE_BUILDER,
            ROLE_REVIEWER,
            ROLE_VERIFIER,
            ROLE_SYNTHESIZER,
            NEUTRAL,
            selection_marker(true),
            selection_marker(false),
        ] {
            let safe = pick(mark, true);
            assert!(safe.is_ascii(), "{mark:?} -> {safe:?}");
            assert_eq!(safe.len(), mark.chars().count(), "{mark:?} keeps its width");
        }
    }
}
