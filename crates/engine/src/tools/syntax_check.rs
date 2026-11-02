//! Post-edit syntax gate for the file-mutating tools (#6204, #6206).
//!
//! Every tool that rewrites a file — `File write`/`edit`/`patch` in
//! [`super::file`] and [`super::apply_patch`], plus [`super::fim`] — routes its
//! post-edit content through [`guard_edit`] *before* the bytes reach the disk.
//! When the edit would leave a file unparseable, the write never happens and
//! the model gets a `line:column` error it can act on this turn instead of a
//! compiler failure one or more turns later.
//!
//! Rust is parsed by `syn`, which implements the real language grammar and
//! reports grammar-exact errors (as opposed to an error-tolerant CST that
//! builds a tree *around* malformed input). TOML and JSON go through
//! `toml_edit` and `serde_json` — the parsers this crate already loads its own
//! config with. A malformed `Cargo.toml` fails the *entire* workspace build
//! rather than one file, so catching it at edit time is worth more there than
//! anywhere else.
//!
//! # The gate only rejects *newly* introduced breakage
//!
//! [`guard_edit`] fails open unless the file parsed **before** the edit and
//! fails to parse **after** it. That asymmetry is the whole safety argument:
//! repairing a file that is already broken — the single most common reason an
//! agent edits a source file at all — must never be blocked by a gate whose
//! job is to catch the edit that broke it. Creating a new file is likewise
//! ungated, since there is no "before" to have regressed.
//!
//! # Known limitations
//!
//! - **Grammar, not semantics.** A file that parses can still fail to compile;
//!   type errors stay the compiler's and the LSP hook's job
//!   (`core::engine::lsp_hooks`).
//! - **`syn` tracks the editions it knows.** Source using syntax newer than the
//!   pinned `syn` would be reported as a parse error. Because the gate requires
//!   the pre-edit file to have parsed, such a file is skipped entirely rather
//!   than becoming uneditable.
//! - **Extension-driven.** A Rust file that is not named `*.rs` is not checked;
//!   language detection by content is deliberately not attempted. `.jsonc`,
//!   `.json5`, and `.jsonl` are *not* treated as JSON: they are different
//!   grammars, and a strict parser would reject valid files.
//! - **A `.json` file that is really JSONC** — `tsconfig.json` with comments is
//!   the usual one — does not parse before the edit either, so the gate skips
//!   it rather than making it uneditable.
//! - **Bounded by [`MAX_CHECKED_BYTES`].** Larger files skip the check rather
//!   than spend edit-path latency on a multi-megabyte parse.

use std::fmt;
use std::path::Path;

use super::spec::ToolError;

/// Files larger than this skip the syntax gate.
///
/// Parsing is linear and fast, but the check sits on the interactive edit path
/// and runs twice on a rejection. 2 MiB covers every hand-written source file
/// in this workspace by a wide margin; past that the file is generated or
/// vendored, where a syntax verdict is worth less than the latency.
const MAX_CHECKED_BYTES: usize = 2 * 1024 * 1024;

/// A language the edit path can parse. Extensions outside this set fail open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyntaxLanguage {
    Rust,
    Toml,
    Json,
}

impl SyntaxLanguage {
    /// Pick a parser from the file extension, or `None` to skip the check.
    fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "rs" => Some(Self::Rust),
            // Covers `Cargo.toml`, `deny.toml`, `.cargo/config.toml` and every
            // ordinary `*.toml` uniformly — the extension is the whole rule.
            "toml" => Some(Self::Toml),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Toml => "TOML",
            Self::Json => "JSON",
        }
    }
}

/// A parse failure, located precisely enough for the model to fix it directly.
#[derive(Debug, Clone)]
pub(super) struct SyntaxIssue {
    language: SyntaxLanguage,
    /// 1-based line, as every editor and compiler reports it.
    line: usize,
    /// 1-based column, likewise.
    column: usize,
    message: String,
}

impl fmt::Display for SyntaxIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} syntax error at line {}, column {}: {}",
            self.language.label(),
            self.line,
            self.column,
            self.message
        )
    }
}

/// Parse `source` as the language implied by `path`.
///
/// `None` means "no objection": the content parses, the extension is not one
/// we parse, or the file is too large to be worth checking.
pub(super) fn syntax_check(path: &Path, source: &str) -> Option<SyntaxIssue> {
    if source.len() > MAX_CHECKED_BYTES {
        return None;
    }
    match SyntaxLanguage::from_path(path)? {
        SyntaxLanguage::Rust => check_rust(source),
        SyntaxLanguage::Toml => check_toml(source),
        SyntaxLanguage::Json => check_json(source),
    }
}

/// Refuse an edit that would take a parseable file to an unparseable one.
///
/// `before` is the pre-edit content, or `None` when the edit creates the file.
/// Callers invoke this before writing, so a rejection leaves the file on disk
/// byte-for-byte untouched and there is nothing to roll back.
pub(super) fn guard_edit(
    path: &Path,
    display_path: &str,
    before: Option<&str>,
    after: &str,
) -> Result<(), ToolError> {
    let Some(issue) = syntax_check(path, after) else {
        return Ok(());
    };
    // Fail open on a file that was already broken (or is brand new): the gate
    // exists to catch the edit that *introduces* a syntax error, never to
    // strand a model that is repairing one.
    let Some(before) = before else {
        return Ok(());
    };
    if syntax_check(path, before).is_some() {
        return Ok(());
    }
    Err(ToolError::execution_failed(format!(
        "Edit refused: it would leave {display_path} unparseable — {issue}. Nothing was written; \
         the file is unchanged. Recovery: re-read the file with `read` (path=...), check the \
         replacement for unbalanced delimiters or a truncated block, and retry."
    )))
}

fn check_rust(source: &str) -> Option<SyntaxIssue> {
    let error = syn::parse_file(source).err()?;
    // A `syn::Error` can carry several diagnostics; the first is the earliest
    // and the one worth showing.
    let first = error.into_iter().next()?;
    let start = first.span().start();
    Some(SyntaxIssue {
        language: SyntaxLanguage::Rust,
        line: start.line,
        // `proc-macro2` columns are 0-based; editors and rustc are not.
        column: start.column.saturating_add(1),
        message: first.to_string(),
    })
}

fn check_toml(source: &str) -> Option<SyntaxIssue> {
    let error = source.parse::<toml_edit::DocumentMut>().err()?;
    let (line, column) = error
        .span()
        .map_or((1, 1), |span| line_column(source, span.start));
    Some(SyntaxIssue {
        language: SyntaxLanguage::Toml,
        line,
        column,
        message: error.message().trim().to_string(),
    })
}

fn check_json(source: &str) -> Option<SyntaxIssue> {
    let error = serde_json::from_str::<serde_json::Value>(source).err()?;
    let rendered = error.to_string();
    // `serde_json` already appends " at line L column C" to its message; the
    // location is reported in its own fields, so drop the duplicate tail.
    let message = rendered
        .split_once(" at line ")
        .map_or(rendered.as_str(), |(head, _)| head);
    Some(SyntaxIssue {
        language: SyntaxLanguage::Json,
        // A zero means "position unknown" (e.g. an IO-shaped error); the
        // 1-based floor keeps the rendered location honest either way.
        line: error.line().max(1),
        column: error.column().max(1),
        message: message.to_string(),
    })
}

/// Translate a byte offset into a 1-based line and column.
///
/// `toml_edit` reports a byte span; every human-facing tool reports line and
/// column. Counting is over `char`s rather than bytes so a column lands where
/// the reader's cursor does in a file with non-ASCII content.
fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let head = &source[..offset];
    let line = head.matches('\n').count() + 1;
    let column = head
        .rfind('\n')
        .map_or(head, |index| &head[index + 1..])
        .chars()
        .count()
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rust_path() -> PathBuf {
        PathBuf::from("src/lib.rs")
    }

    #[test]
    fn valid_rust_passes() {
        assert!(syntax_check(&rust_path(), "fn main() {}\n").is_none());
    }

    #[test]
    fn missing_brace_reports_line_and_column() {
        let issue = syntax_check(&rust_path(), "fn main() {\n    let x = 1;\n")
            .expect("unbalanced brace must be reported");
        assert_eq!(issue.language, SyntaxLanguage::Rust);
        assert!(issue.line >= 1, "{issue}");
        assert!(issue.column >= 1, "{issue}");
        let rendered = issue.to_string();
        assert!(rendered.contains("Rust syntax error at line"), "{rendered}");
    }

    #[test]
    fn valid_toml_passes() {
        assert!(syntax_check(Path::new("Cargo.toml"), "[package]\nname = \"x\"\n").is_none());
    }

    #[test]
    fn broken_toml_reports_line_and_column() {
        let issue = syntax_check(Path::new("Cargo.toml"), "[package]\nname = \n")
            .expect("a value-less key must be reported");
        assert_eq!(issue.language, SyntaxLanguage::Toml);
        assert_eq!(issue.line, 2, "{issue}");
        let rendered = issue.to_string();
        assert!(
            rendered.contains("TOML syntax error at line 2"),
            "{rendered}"
        );
    }

    #[test]
    fn valid_json_passes() {
        assert!(syntax_check(Path::new("data.json"), "{\"a\": [1, 2]}").is_none());
    }

    #[test]
    fn broken_json_reports_line_and_column() {
        let issue = syntax_check(Path::new("data.json"), "{\n  \"a\": [1, 2,\n}\n")
            .expect("a trailing comma must be reported");
        assert_eq!(issue.language, SyntaxLanguage::Json);
        assert_eq!(issue.line, 3, "{issue}");
        let rendered = issue.to_string();
        assert!(
            rendered.contains("JSON syntax error at line 3"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("at line 3 column"),
            "serde_json's duplicate location tail must be stripped: {rendered}"
        );
    }

    #[test]
    fn jsonc_with_comments_is_not_parsed_as_json_before_the_edit() {
        // A `.json` file that is really JSONC does not parse either way, so
        // the before/after rule skips it instead of making it uneditable.
        let commented = "{\n  // note\n  \"a\": 1\n}\n";
        assert!(syntax_check(Path::new("tsconfig.json"), commented).is_some());
        guard_edit(
            Path::new("tsconfig.json"),
            "tsconfig.json",
            Some(commented),
            "{\n  // note\n  \"a\": 2\n}\n",
        )
        .expect("a JSONC file must stay editable");
    }

    #[test]
    fn guard_rejects_an_edit_that_breaks_a_manifest() {
        let error = guard_edit(
            Path::new("Cargo.toml"),
            "Cargo.toml",
            Some("[package]\nname = \"x\"\n"),
            "[package\nname = \"x\"\n",
        )
        .expect_err("an unparseable manifest must be refused at edit time");
        let message = error.to_string();
        assert!(message.contains("TOML syntax error at line"), "{message}");
        assert!(message.contains("Nothing was written"), "{message}");
    }

    #[test]
    fn unknown_extension_is_skipped() {
        assert!(syntax_check(Path::new("notes.txt"), "fn main() {").is_none());
    }

    #[test]
    fn oversized_source_is_skipped() {
        let huge = format!("fn main() {{{}", " ".repeat(MAX_CHECKED_BYTES));
        assert!(syntax_check(&rust_path(), &huge).is_none());
    }

    #[test]
    fn guard_rejects_newly_broken_rust() {
        let error = guard_edit(
            &rust_path(),
            "src/lib.rs",
            Some("fn main() {}\n"),
            "fn main() {\n",
        )
        .expect_err("an edit that breaks a parseable file must be refused");
        let message = error.to_string();
        assert!(message.contains("src/lib.rs"), "{message}");
        assert!(message.contains("Rust syntax error at line"), "{message}");
        assert!(message.contains("Nothing was written"), "{message}");
        // Recovery must point at the model-visible `read` tool, never the
        // hidden compatibility `File` tool.
        assert!(message.contains("`read` (path=...)"), "{message}");
        assert!(!message.contains("File action="), "{message}");
    }

    #[test]
    fn guard_allows_repairing_an_already_broken_file() {
        // Still broken after the edit, but it was broken before: a model
        // mid-repair must not be locked out.
        guard_edit(
            &rust_path(),
            "src/lib.rs",
            Some("fn main() {\n"),
            "fn main() {\n    let x = 1;\n",
        )
        .expect("pre-existing breakage must fail open");
    }

    #[test]
    fn guard_allows_creating_a_new_file() {
        guard_edit(&rust_path(), "src/lib.rs", None, "fn main() {\n")
            .expect("file creation has no prior state to regress");
    }

    #[test]
    fn guard_allows_a_valid_edit() {
        guard_edit(
            &rust_path(),
            "src/lib.rs",
            Some("fn main() {}\n"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .expect("a syntactically valid edit must pass");
    }
}
