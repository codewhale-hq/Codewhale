//! REPL fence-extraction utilities.
//!
//! The agent's main loop scans assistant text for ` ```repl ` fenced blocks
//! and feeds them to a [`crate::runtime::PythonRuntime`]. Capturing
//! `FINAL(...)` and routing sub-LLM RPCs are handled inside the runtime via
//! a stdin/stdout protocol — no scraping required here.

const REPL_FENCE: &str = "```repl";

/// Byte offset of the first opening `` ```repl `` fence at or after `from`.
///
/// A fence must open its own line (up to three spaces of indent, as in
/// Markdown) and carry no other info string. Prose that mentions the fence
/// mid-line, or a `` ```repl-output `` block, is not code to execute.
fn next_repl_fence(text: &str, from: usize) -> Option<usize> {
    let mut line_start = if from == 0 || text.as_bytes().get(from - 1) == Some(&b'\n') {
        from
    } else {
        from + text[from..].find('\n')? + 1
    };
    loop {
        let line_end = text[line_start..]
            .find('\n')
            .map_or(text.len(), |offset| line_start + offset);
        let line = &text[line_start..line_end];
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent <= 3
            && line[indent..]
                .strip_prefix(REPL_FENCE)
                .is_some_and(|info| info.trim().is_empty())
        {
            return Some(line_start + indent);
        }
        if line_end >= text.len() {
            return None;
        }
        line_start = line_end + 1;
    }
}

/// Check if a string contains a `` ```repl `` fence that opens its own line.
pub fn has_repl_block(text: &str) -> bool {
    next_repl_fence(text, 0).is_some()
}

/// Extract every line-anchored `` ```repl `` block from `text` with byte offsets.
pub fn extract_repl_blocks(text: &str) -> Vec<ReplBlock> {
    let mut blocks = Vec::new();
    let mut search_from = 0;

    while let Some(start) = next_repl_fence(text, search_from) {
        let after_fence = &text[start..];
        let code_start = after_fence.find('\n').unwrap_or(after_fence.len());
        let code_region = &after_fence[code_start..];
        let Some(end_offset) = code_region.find("\n```") else {
            break;
        };
        blocks.push(ReplBlock {
            code: code_region[..end_offset].to_string(),
            start_offset: start,
            end_offset: start + code_start + end_offset + 3,
        });
        search_from = start + code_start + end_offset + 4;
    }

    blocks
}

/// A `` ```repl `` code block with byte-offset position info.
#[derive(Debug, Clone)]
pub struct ReplBlock {
    pub code: String,
    pub start_offset: usize,
    pub end_offset: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_repl_block_detects_fence() {
        assert!(has_repl_block("some text\n```repl\ncode\n``` more"));
        assert!(has_repl_block("  ```repl  \r\ncode\n```"));
        assert!(!has_repl_block("no repl here ```python\ncode\n```"));
        assert!(!has_repl_block("just text"));
    }

    #[test]
    fn extract_repl_blocks_single() {
        let text = "before\n```repl\nprint('hello')\n```\nafter";
        let blocks = extract_repl_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code.trim(), "print('hello')");
    }

    #[test]
    fn extract_repl_blocks_multiple() {
        let text = "```repl\ncode1\n```\nmid\n```repl\ncode2\n```\nend";
        let blocks = extract_repl_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].code.trim(), "code1");
        assert_eq!(blocks[1].code.trim(), "code2");
    }

    #[test]
    fn extract_repl_blocks_empty_when_none() {
        let blocks = extract_repl_blocks("no blocks here");
        assert!(blocks.is_empty());
    }

    #[test]
    fn mid_line_mention_is_not_a_fence() {
        let text = "a ```repl mention\n```\nx\n```";
        assert!(!has_repl_block(text));
        assert!(extract_repl_blocks(text).is_empty());
        assert!(!has_repl_block("```repl-output\nx\n```"));
        assert!(!has_repl_block("    ```repl\nindented code block\n```"));
    }

    #[test]
    fn mid_line_mention_does_not_hide_a_later_fence() {
        let text = "see ```repl here\n```repl\nprint(1)\n```";
        let blocks = extract_repl_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code.trim(), "print(1)");
        assert_eq!(&text[blocks[0].start_offset..][..7], "```repl");
    }
}
