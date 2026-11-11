/// Estimate output tokens from streamed text before provider usage arrives.
///
/// Provider-reported usage remains canonical at turn completion. During a live
/// stream, this gives the footer a stable approximation without inspecting
/// provider-specific tokenizer internals.
pub fn estimate_output_tokens_from_text(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    if chars == 0 {
        0
    } else {
        chars.saturating_add(3) / 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimated_streaming_tokens_round_up_from_text_chars() {
        assert_eq!(estimate_output_tokens_from_text(""), 0);
        assert_eq!(estimate_output_tokens_from_text("abc"), 1);
        assert_eq!(estimate_output_tokens_from_text("abcd"), 1);
        assert_eq!(estimate_output_tokens_from_text("abcde"), 2);
    }
}
