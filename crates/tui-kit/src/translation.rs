//! Post-hoc translation interception layer.
//!
//! When output translation is enabled (`/translate`), this module provides
//! the interception logic that detects English model output and replaces it
//! with Chinese translations before display. The primary mechanism is the
//! system prompt instruction in `prompts.rs`; this module is the fallback
//! for model output that leaks English despite the instruction.
//!
//! ## Architecture
//!
//! - `needs_translation()` — heuristic to detect if text is predominantly
//!   English and should be translated.
//!
//! The event loop dispatches focused translation requests through the exact
//! provider/model client frozen for the originating turn. The dedicated agent
//! receives only the source text and returns only the translation — no tool
//! calls or conversation history.
//!

/// Heuristic threshold: if more than this fraction of alphabetic characters
/// are Latin (A-Z / a-z), the text is considered English.
const ENGLISH_LATIN_RATIO_THRESHOLD: f64 = 0.6;

/// Minimum number of alphabetic characters required before applying the
/// heuristic — avoids false positives on short mixed-language strings.
const MIN_ALPHA_CHARS_FOR_DETECTION: usize = 10;

/// How many Latin-letter "information units" each CJK character is worth.
/// A single CJK character carries roughly the information of a short English
/// word (2–4 letters), so we weight CJK at 3× for fair comparison.
const CJK_CHAR_WEIGHT: usize = 3;

/// Detect if text content is predominantly English and should be translated.
///
/// The heuristic compares CJK characters (weighted) against Latin letters.
/// CJK characters carry much more information per glyph, so a string with
/// even a modest number of Chinese characters among English words will not
/// be flagged.
#[must_use]
pub fn needs_translation(text: &str) -> bool {
    let mut latin_count = 0usize;
    let mut cjk_count = 0usize;

    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            latin_count += 1;
        } else if is_cjk(ch) {
            cjk_count += 1;
        }
    }

    let total_alpha = latin_count + (cjk_count * CJK_CHAR_WEIGHT);

    if total_alpha < MIN_ALPHA_CHARS_FOR_DETECTION {
        return false;
    }

    // If weighted CJK dominates, it's already Chinese — no translation needed.
    if (cjk_count * CJK_CHAR_WEIGHT) > latin_count {
        return false;
    }

    let ratio = latin_count as f64 / total_alpha as f64;
    ratio >= ENGLISH_LATIN_RATIO_THRESHOLD
}

/// Check if a character is in the CJK Unified Ideographs block or is a
/// common Chinese/Japanese/Korean character.
pub fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Unified Ideographs Extension A
        | '\u{2E80}'..='\u{2EFF}' // CJK Radicals Supplement
        | '\u{3000}'..='\u{303F}' // CJK Symbols and Punctuation
        | '\u{FF00}'..='\u{FFEF}' // Halfwidth and Fullwidth Forms
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_avoids_false_positive() {
        assert!(!needs_translation("hi"));
        assert!(!needs_translation("ok"));
    }

    #[test]
    fn english_text_detected() {
        assert!(needs_translation(
            "This is a message from the assistant explaining how the code works."
        ));
    }

    #[test]
    fn chinese_text_not_detected() {
        assert!(!needs_translation(
            "这是助手的一条中文回复，解释了代码的工作原理。"
        ));
    }

    #[test]
    fn mixed_mostly_english_detected() {
        assert!(needs_translation(
            "The function handle_request takes a Request param and returns a Response."
        ));
    }

    #[test]
    fn mixed_mostly_chinese_not_detected() {
        assert!(!needs_translation(
            "这个 handle_request 函数接收一个 Request 参数并返回 Response。"
        ));
    }

    #[test]
    fn code_with_short_labels_not_falsely_detected() {
        assert!(!needs_translation("let x = 1; let y = 2;"));
    }

    #[test]
    fn long_english_code_is_detected() {
        assert!(needs_translation(
            "function calculateTotalRevenueForQuarterlyReport() { return; }"
        ));
    }

    #[test]
    fn js_comments_in_english_detected() {
        assert!(needs_translation(
            "// This is a JavaScript function that handles user authentication\nfunction login() {}"
        ));
    }
}
