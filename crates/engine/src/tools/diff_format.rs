//! Build unified-diff strings for tool results.
//!
//! `edit_file` and `write_file` capture the file contents before and after
//! the mutation and emit a unified diff at the head of their `ToolResult`
//! output. The TUI's `output_looks_like_diff` detector then routes the
//! payload through `diff_render::render_diff`, which renders it with line
//! numbers and coloured `+`/`-` gutters (#505).
//!
//! The diff is also a strict UX upgrade for the model — it sees exactly
//! which lines changed instead of a one-line summary.

use similar::TextDiff;

/// Build a unified diff between `old` and `new` keyed at `path`.
///
/// Returns an empty string when the inputs are byte-identical so callers
/// can skip the "no changes" header. The output uses git-style `--- a/...`
/// / `+++ b/...` headers and three lines of context — matching the format
/// the TUI's `diff_render::render_diff` already understands.
#[must_use]
pub fn make_unified_diff(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a = format!("a/{path}");
    let b = format!("b/{path}");
    let diff = TextDiff::from_lines(old, new);
    diff.unified_diff()
        .context_radius(3)
        .header(&a, &b)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_emit_empty_diff() {
        let s = "hello\nworld\n";
        assert!(make_unified_diff("foo.txt", s, s).is_empty());
    }

    #[test]
    fn replacement_emits_minus_plus_pair() {
        let old = "alpha\nbeta\ngamma\n";
        let new = "alpha\nBETA\ngamma\n";
        let diff = make_unified_diff("foo.txt", old, new);
        assert!(diff.contains("--- a/foo.txt"), "{diff}");
        assert!(diff.contains("+++ b/foo.txt"), "{diff}");
        assert!(diff.contains("-beta"), "{diff}");
        assert!(diff.contains("+BETA"), "{diff}");
    }

    #[test]
    fn new_file_renders_against_empty_old() {
        let new = "first line\nsecond line\n";
        let diff = make_unified_diff("new.txt", "", new);
        assert!(diff.contains("--- a/new.txt"), "{diff}");
        assert!(diff.contains("+++ b/new.txt"), "{diff}");
        assert!(diff.contains("+first line"), "{diff}");
        assert!(diff.contains("+second line"), "{diff}");
    }

    #[test]
    fn diff_contains_hunk_header_so_tui_renders_it() {
        // The TUI detector scans the first 5 lines for `@@`. Make sure the
        // unified diff puts a hunk header within that window so the
        // diff-aware renderer kicks in (#505).
        let diff = make_unified_diff("foo.txt", "a\n", "b\n");
        let head: Vec<&str> = diff.lines().take(5).collect();
        assert!(
            head.iter().any(|line| line.starts_with("@@")),
            "expected hunk header in first 5 lines; got {head:?}"
        );
    }
}
