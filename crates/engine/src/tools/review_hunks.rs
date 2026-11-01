//! Unified-diff hunk parsing for GitHub review anchors.
//!
//! GitHub's "create a review" API rejects the *entire* review with a 422 when
//! any single inline comment anchors to a line that is not part of the diff.
//! Before this module existed the reviewer only checked that the *file* was
//! touched, so one model-estimated line number could make the whole review
//! request fail after every other inline comment had already been prepared.
//!
//! [`DiffHunks::parse`] turns a unified diff into the exact set of RIGHT-side
//! (post-image) line numbers GitHub will accept per file, so a bad anchor
//! drops one comment instead of the whole review.

use std::collections::BTreeMap;

/// A contiguous run of RIGHT-side line numbers, inclusive on both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineRange {
    start: u32,
    end: u32,
}

/// The RIGHT-side lines a unified diff exposes, keyed by post-image path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffHunks {
    files: BTreeMap<String, FileHunks>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileHunks {
    /// Every RIGHT-side line inside a hunk: context lines plus added lines.
    /// These are exactly the lines GitHub accepts with `side: "RIGHT"`.
    commentable: Vec<LineRange>,
}

/// Cursor state while walking a hunk body.
#[derive(Debug, Clone, Copy, Default)]
struct HunkCursor {
    right_line: u32,
    remaining_old: u32,
    remaining_new: u32,
}

impl HunkCursor {
    fn exhausted(self) -> bool {
        self.remaining_old == 0 && self.remaining_new == 0
    }
}

impl DiffHunks {
    /// Parse a unified diff (`git diff`, `gh pr diff`) into per-file
    /// RIGHT-side line coverage.
    ///
    /// Handles multiple hunks per file, added-only and deleted files,
    /// renames (the post-image path wins), `\ No newline at end of file`
    /// markers, CRLF line endings, and hunk headers with omitted counts
    /// (`@@ -1 +1 @@`). Hunk bodies are bounded by the counts in the header
    /// so a file whose *content* contains `--- `, `+++ `, or `diff --git`
    /// lines cannot be mistaken for a new file header.
    #[must_use]
    pub fn parse(diff: &str) -> Self {
        let mut files: BTreeMap<String, FileHunks> = BTreeMap::new();
        let mut current: Option<String> = None;
        let mut cursor = HunkCursor::default();
        let mut in_hunk = false;

        for raw in diff.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);

            if in_hunk && !cursor.exhausted() {
                match consume_hunk_line(line, &mut cursor, current.as_deref(), &mut files) {
                    BodyStep::Consumed => continue,
                    BodyStep::NotABodyLine => in_hunk = false,
                }
            }

            if line.starts_with("diff --git ") {
                current = None;
                in_hunk = false;
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                in_hunk = false;
                current = post_image_path(rest);
                if let Some(path) = current.as_ref() {
                    files.entry(path.clone()).or_default();
                }
            } else if line.starts_with("--- ") {
                // Pre-image header; the `+++` that follows decides the path we
                // anchor against (which is what makes renames work).
                in_hunk = false;
            } else if let Some((right_start, old_count, new_count)) = parse_hunk_header(line) {
                cursor = HunkCursor {
                    right_line: right_start,
                    remaining_old: old_count,
                    remaining_new: new_count,
                };
                in_hunk = true;
            }
        }

        Self { files }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// True when the diff touches `path` at all (post-image path match).
    #[must_use]
    pub fn touches_path(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Post-image paths and hunk ranges from the same parser used to validate
    /// comments. Context collection must not invent a second diff parser.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn ranges(&self, path: &str) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.files.get(path).into_iter().flat_map(|file| {
            file.commentable
                .iter()
                .map(|range| (range.start, range.end))
        })
    }

    /// True when `line` is a RIGHT-side line GitHub will accept an inline
    /// comment on for `path` — a context line or an added line inside a hunk.
    #[must_use]
    pub fn contains_line(&self, path: &str, line: u32) -> bool {
        self.files
            .get(path)
            .is_some_and(|file| covers(&file.commentable, line))
    }

    /// True when every line in `start..=end` is a valid RIGHT-side anchor.
    /// Gates multi-line committable suggestions: GitHub rejects a
    /// `start_line`/`line` span that leaves the diff.
    #[must_use]
    pub fn contains_span(&self, path: &str, start: u32, end: u32) -> bool {
        if start > end {
            return false;
        }
        let Some(file) = self.files.get(path) else {
            return false;
        };
        (start..=end).all(|line| covers(&file.commentable, line))
    }
}

enum BodyStep {
    Consumed,
    NotABodyLine,
}

fn consume_hunk_line(
    line: &str,
    cursor: &mut HunkCursor,
    current: Option<&str>,
    files: &mut BTreeMap<String, FileHunks>,
) -> BodyStep {
    // `\ No newline at end of file` annotates the previous line and consumes
    // no line number on either side.
    if line.starts_with('\\') {
        return BodyStep::Consumed;
    }
    match line.as_bytes().first() {
        Some(b'+') => {
            if let Some(path) = current {
                let entry = files.entry(path.to_string()).or_default();
                push_line(&mut entry.commentable, cursor.right_line);
            }
            cursor.right_line = cursor.right_line.saturating_add(1);
            cursor.remaining_new = cursor.remaining_new.saturating_sub(1);
            BodyStep::Consumed
        }
        Some(b'-') => {
            // Deleted: LEFT side only, never a valid RIGHT anchor.
            cursor.remaining_old = cursor.remaining_old.saturating_sub(1);
            BodyStep::Consumed
        }
        // A context line is " text"; some transports strip the trailing space
        // from an empty context line, leaving a bare empty line.
        Some(b' ') | None => {
            if let Some(path) = current {
                let entry = files.entry(path.to_string()).or_default();
                push_line(&mut entry.commentable, cursor.right_line);
            }
            cursor.right_line = cursor.right_line.saturating_add(1);
            cursor.remaining_old = cursor.remaining_old.saturating_sub(1);
            cursor.remaining_new = cursor.remaining_new.saturating_sub(1);
            BodyStep::Consumed
        }
        Some(_) => BodyStep::NotABodyLine,
    }
}

fn covers(ranges: &[LineRange], line: u32) -> bool {
    ranges
        .iter()
        .any(|range| line >= range.start && line <= range.end)
}

fn push_line(ranges: &mut Vec<LineRange>, line: u32) {
    if let Some(last) = ranges.last_mut()
        && last.end.saturating_add(1) == line
    {
        last.end = line;
        return;
    }
    ranges.push(LineRange {
        start: line,
        end: line,
    });
}

/// Extract the post-image path from a `+++ ` header body. Returns `None` for
/// `/dev/null` (a deleted file has no RIGHT side to comment on).
fn post_image_path(rest: &str) -> Option<String> {
    // Some diff formats append a tab plus timestamp.
    let rest = rest.split('\t').next().unwrap_or(rest).trim_end();
    if rest.is_empty() || rest == "/dev/null" {
        return None;
    }
    // Quoted paths (`"b/we\tird.rs"`) carry C-style escapes; anchoring on a
    // mangled path would 422 the review, so decline rather than guess.
    if rest.starts_with('"') {
        return None;
    }
    let path = rest.strip_prefix("b/").unwrap_or(rest);
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    Some(path.to_string())
}

/// Parse `@@ -a,b +c,d @@ optional context` into
/// `(right_start, old_count, new_count)`. Counts may be omitted
/// (`@@ -1 +1 @@` means one line on each side).
pub(super) fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let ranges = &rest[..end];
    let mut old_count = None;
    let mut new_count = None;
    let mut right_start = None;
    for part in ranges.split(' ') {
        if let Some(spec) = part.strip_prefix('-') {
            old_count = Some(parse_range_count(spec)?);
        } else if let Some(spec) = part.strip_prefix('+') {
            let (start, count) = parse_range(spec)?;
            right_start = Some(start);
            new_count = Some(count);
        }
    }
    Some((right_start?, old_count?, new_count?))
}

fn parse_range(spec: &str) -> Option<(u32, u32)> {
    let mut parts = spec.split(',');
    let start = parts.next()?.parse::<u32>().ok()?;
    let count = match parts.next() {
        Some(count) => count.parse::<u32>().ok()?,
        None => 1,
    };
    Some((start, count))
}

fn parse_range_count(spec: &str) -> Option<u32> {
    parse_range(spec).map(|(_, count)| count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Join diff lines verbatim. Written as a slice rather than one string
    /// literal because a `\` continuation would strip the leading space that
    /// marks a context line — which is exactly the byte this parser reads.
    fn diff(lines: &[&str]) -> String {
        let mut joined = lines.join("\n");
        joined.push('\n');
        joined
    }

    #[test]
    fn parses_multiple_hunks_in_one_file() {
        let diff = diff(&[
            "diff --git a/src/lib.rs b/src/lib.rs",
            "index 1111111..2222222 100644",
            "--- a/src/lib.rs",
            "+++ b/src/lib.rs",
            "@@ -10,3 +10,4 @@ fn one() {",
            " context_a",
            "-removed",
            "+added_one",
            "+added_two",
            " context_b",
            "@@ -80,2 +81,2 @@ fn two() {",
            " ctx",
            "-old",
            "+new",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.touches_path("src/lib.rs"));
        // First hunk covers right lines 10..=13 (ctx, +, +, ctx).
        for line in 10..=13 {
            assert!(hunks.contains_line("src/lib.rs", line), "line {line}");
        }
        assert!(!hunks.contains_line("src/lib.rs", 9));
        assert!(!hunks.contains_line("src/lib.rs", 14));
        // Second hunk: ctx at 81, `-old` consumes no right line, `+new` at 82.
        assert!(hunks.contains_line("src/lib.rs", 81));
        assert!(hunks.contains_line("src/lib.rs", 82));
        assert!(!hunks.contains_line("src/lib.rs", 83));
    }

    #[test]
    fn parses_added_only_file() {
        let diff = diff(&[
            "diff --git a/new.rs b/new.rs",
            "new file mode 100644",
            "index 0000000..3333333",
            "--- /dev/null",
            "+++ b/new.rs",
            "@@ -0,0 +1,3 @@",
            "+fn a() {}",
            "+fn b() {}",
            "+fn c() {}",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.contains_span("new.rs", 1, 3));
        assert!(!hunks.contains_line("new.rs", 4));
    }

    #[test]
    fn deleted_file_has_no_right_side_anchor() {
        let diff = diff(&[
            "diff --git a/gone.rs b/gone.rs",
            "deleted file mode 100644",
            "index 3333333..0000000",
            "--- a/gone.rs",
            "+++ /dev/null",
            "@@ -1,2 +0,0 @@",
            "-fn a() {}",
            "-fn b() {}",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(!hunks.touches_path("gone.rs"));
        assert!(!hunks.contains_line("gone.rs", 1));
    }

    #[test]
    fn pure_deletion_hunk_exposes_no_right_lines() {
        let diff = diff(&[
            "diff --git a/src/a.rs b/src/a.rs",
            "--- a/src/a.rs",
            "+++ b/src/a.rs",
            "@@ -5,3 +4,0 @@",
            "-one",
            "-two",
            "-three",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.touches_path("src/a.rs"));
        assert!(!hunks.contains_line("src/a.rs", 4));
        assert!(!hunks.contains_line("src/a.rs", 5));
    }

    #[test]
    fn rename_anchors_on_the_post_image_path() {
        let diff = diff(&[
            "diff --git a/old/name.rs b/new/name.rs",
            "similarity index 92%",
            "rename from old/name.rs",
            "rename to new/name.rs",
            "--- a/old/name.rs",
            "+++ b/new/name.rs",
            "@@ -1,2 +1,2 @@",
            " kept",
            "-old",
            "+new",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.touches_path("new/name.rs"));
        assert!(!hunks.touches_path("old/name.rs"));
        assert!(hunks.contains_span("new/name.rs", 1, 2));
        assert!(!hunks.contains_line("new/name.rs", 3));
    }

    #[test]
    fn rename_without_content_change_has_no_anchors() {
        let diff = diff(&[
            "diff --git a/old.rs b/new.rs",
            "similarity index 100%",
            "rename from old.rs",
            "rename to new.rs",
        ]);
        let hunks = DiffHunks::parse(&diff);
        // No `+++` header and no hunk: nothing is commentable, and we must not
        // invent an anchor.
        assert!(!hunks.touches_path("new.rs"));
        assert!(!hunks.contains_line("new.rs", 1));
    }

    #[test]
    fn no_newline_marker_consumes_no_line_number() {
        let diff = diff(&[
            "diff --git a/eof.rs b/eof.rs",
            "--- a/eof.rs",
            "+++ b/eof.rs",
            "@@ -1,2 +1,2 @@",
            " first",
            "-last",
            "\\ No newline at end of file",
            "+last!",
            "\\ No newline at end of file",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.contains_span("eof.rs", 1, 2));
        assert!(!hunks.contains_line("eof.rs", 3));
    }

    #[test]
    fn crlf_diff_parses_paths_and_lines() {
        let diff = [
            "diff --git a/crlf.rs b/crlf.rs",
            "index 1111111..2222222 100644",
            "--- a/crlf.rs",
            "+++ b/crlf.rs",
            "@@ -1,2 +1,3 @@",
            " ctx",
            "+added",
            " tail",
        ]
        .join("\r\n")
            + "\r\n";
        let hunks = DiffHunks::parse(&diff);
        assert!(
            hunks.touches_path("crlf.rs"),
            "CRLF path must not keep the carriage return"
        );
        assert!(hunks.contains_span("crlf.rs", 1, 3));
        assert!(!hunks.contains_line("crlf.rs", 4));
    }

    #[test]
    fn hunk_header_without_counts_is_supported() {
        let diff = diff(&[
            "diff --git a/one.rs b/one.rs",
            "--- a/one.rs",
            "+++ b/one.rs",
            "@@ -7 +9 @@",
            "-old",
            "+new",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.contains_line("one.rs", 9));
        assert!(!hunks.contains_line("one.rs", 8));
    }

    #[test]
    fn multi_file_diff_keeps_files_independent() {
        let diff = diff(&[
            "diff --git a/a.rs b/a.rs",
            "--- a/a.rs",
            "+++ b/a.rs",
            "@@ -1,0 +1,1 @@",
            "+alpha",
            "diff --git a/b.rs b/b.rs",
            "--- a/b.rs",
            "+++ b/b.rs",
            "@@ -50,0 +50,1 @@",
            "+beta",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.contains_line("a.rs", 1));
        assert!(!hunks.contains_line("a.rs", 50));
        assert!(hunks.contains_line("b.rs", 50));
        assert!(!hunks.contains_line("b.rs", 1));
    }

    #[test]
    fn file_content_that_looks_like_a_diff_header_stays_in_the_hunk() {
        // A patch that itself edits a stored diff fixture. Naive parsers read
        // the ` --- a/inner.rs` / ` +++ b/inner.rs` context lines as a new file
        // header and mis-anchor every later comment.
        let diff = diff(&[
            "diff --git a/fixtures/sample.diff b/fixtures/sample.diff",
            "--- a/fixtures/sample.diff",
            "+++ b/fixtures/sample.diff",
            "@@ -1,4 +1,4 @@",
            " --- a/inner.rs",
            " +++ b/inner.rs",
            "-@@ -1 +1 @@",
            "+@@ -2 +2 @@",
            " tail",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.contains_span("fixtures/sample.diff", 1, 4));
        assert!(!hunks.touches_path("inner.rs"));
    }

    #[test]
    fn quoted_path_is_declined_rather_than_mangled() {
        let diff = diff(&[
            "diff --git \"a/we\\tird.rs\" \"b/we\\tird.rs\"",
            "--- \"a/we\\tird.rs\"",
            "+++ \"b/we\\tird.rs\"",
            "@@ -1,0 +1,1 @@",
            "+x",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.files.is_empty());
    }

    #[test]
    fn empty_context_line_still_advances_the_cursor() {
        // Some transports strip the trailing space from an empty context line.
        let diff = diff(&[
            "diff --git a/s.rs b/s.rs",
            "--- a/s.rs",
            "+++ b/s.rs",
            "@@ -1,3 +1,4 @@",
            " one",
            "",
            "+three",
            " four",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.contains_span("s.rs", 1, 4));
    }

    #[test]
    fn contains_span_rejects_partial_and_inverted_ranges() {
        let diff = diff(&[
            "diff --git a/x.rs b/x.rs",
            "--- a/x.rs",
            "+++ b/x.rs",
            "@@ -1,1 +1,2 @@",
            " a",
            "+b",
        ]);
        let hunks = DiffHunks::parse(&diff);
        assert!(hunks.contains_span("x.rs", 1, 2));
        assert!(!hunks.contains_span("x.rs", 1, 3));
        assert!(!hunks.contains_span("x.rs", 2, 1));
        assert!(!hunks.contains_span("missing.rs", 1, 1));
    }

    #[test]
    fn empty_diff_yields_no_anchors() {
        let hunks = DiffHunks::parse("");
        assert!(!hunks.touches_path("anything.rs"));
        assert!(!hunks.contains_line("anything.rs", 1));
    }
}
