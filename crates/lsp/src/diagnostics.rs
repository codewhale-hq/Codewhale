//! Diagnostic shape returned by the LSP transport, plus the renderer that
//! produces the `<diagnostics file="…">` block injected into the model
//! context after a file edit.
//!
//! Format (matches the spec given in issue #136):
//!
//! ```text
//! <diagnostics file="crates/tui/src/foo.rs">
//!   ERROR [12:8] missing semicolon
//!   ERROR [13:1] expected `,`, found `}`
//! </diagnostics>
//! ```
//!
//! Lines are 1-based. Columns are 1-based. We trim each diagnostic message
//! to a single line so the block stays compact.

use std::path::PathBuf;

/// Severity bucket used in the rendered block. Mirrors the LSP severity
/// codes (1 = Error, 2 = Warning, 3 = Information, 4 = Hint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

impl Severity {
    /// Decode the LSP integer severity. Returns `None` when the integer is
    /// missing or unrecognized — callers default to `Error` to err on the
    /// side of surfacing the issue.
    #[must_use]
    pub fn from_lsp(code: Option<i64>) -> Option<Self> {
        match code? {
            1 => Some(Severity::Error),
            2 => Some(Severity::Warning),
            3 => Some(Severity::Information),
            4 => Some(Severity::Hint),
            _ => None,
        }
    }

    /// Uppercase label used in the rendered block.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warning => "WARNING",
            Severity::Information => "INFO",
            Severity::Hint => "HINT",
        }
    }
}

/// One LSP diagnostic, normalized to 1-based line/col so we can render it
/// directly. The transport layer is responsible for the `0-based -> 1-based`
/// conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: u32,
    pub column: u32,
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    /// Trim the message to a single line for compact rendering.
    fn render_message(&self) -> String {
        let first_line = self.message.lines().next().unwrap_or("").trim();
        first_line.to_string()
    }
}

/// One file's worth of diagnostics, ready to render. The renderer caps the
/// list to `max_per_file` items.
#[derive(Debug, Clone)]
pub struct DiagnosticBlock {
    /// Path used inside the `file="…"` attribute. Should be relative to the
    /// workspace root when possible (we use `path.file_name()` if relativizing
    /// fails, per the issue's hard rule).
    pub file: PathBuf,
    pub items: Vec<Diagnostic>,
}

impl DiagnosticBlock {
    /// Render the block in the format pasted in the module docs. Returns the
    /// empty string when `self.items` is empty so callers can `if !text.is_empty()`
    /// before injecting.
    #[must_use]
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let file_attr = self.file.display();
        let mut out = format!("<diagnostics file=\"{file_attr}\">\n");
        for item in &self.items {
            out.push_str(&format!(
                "  {} [{}:{}] {}\n",
                item.severity.label(),
                item.line,
                item.column,
                item.render_message(),
            ));
        }
        out.push_str("</diagnostics>");
        out
    }

    /// Truncate to at most `max_per_file` items, preserving order. The LSP
    /// manager is responsible for sorting by severity before calling this so
    /// errors are kept ahead of warnings when truncation happens.
    pub fn truncate(&mut self, max_per_file: usize) {
        if self.items.len() > max_per_file {
            self.items.truncate(max_per_file);
        }
    }
}

/// Format a list of [`DiagnosticBlock`]s as a single bundle. Used by the
/// engine when one turn touched several files. Empty blocks are skipped.
#[must_use]
pub fn render_blocks(blocks: &[DiagnosticBlock]) -> String {
    let mut chunks = Vec::new();
    for block in blocks {
        let rendered = block.render();
        if !rendered.is_empty() {
            chunks.push(rendered);
        }
    }
    chunks.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_decodes_lsp_codes() {
        assert_eq!(Severity::from_lsp(Some(1)), Some(Severity::Error));
        assert_eq!(Severity::from_lsp(Some(2)), Some(Severity::Warning));
        assert_eq!(Severity::from_lsp(Some(3)), Some(Severity::Information));
        assert_eq!(Severity::from_lsp(Some(4)), Some(Severity::Hint));
        assert_eq!(Severity::from_lsp(Some(99)), None);
        assert_eq!(Severity::from_lsp(None), None);
    }

    #[test]
    fn renders_block_in_required_format() {
        let block = DiagnosticBlock {
            file: PathBuf::from("crates/tui/src/foo.rs"),
            items: vec![
                Diagnostic {
                    line: 12,
                    column: 8,
                    severity: Severity::Error,
                    message: "missing semicolon".to_string(),
                },
                Diagnostic {
                    line: 13,
                    column: 1,
                    severity: Severity::Error,
                    message: "expected `,`, found `}`".to_string(),
                },
            ],
        };
        let rendered = block.render();
        assert!(rendered.contains("<diagnostics file=\"crates/tui/src/foo.rs\">"));
        assert!(rendered.contains("ERROR [12:8] missing semicolon"));
        assert!(rendered.contains("ERROR [13:1] expected `,`, found `}`"));
        assert!(rendered.ends_with("</diagnostics>"));
    }

    #[test]
    fn empty_block_renders_to_empty_string() {
        let block = DiagnosticBlock {
            file: PathBuf::from("foo.rs"),
            items: Vec::new(),
        };
        assert!(block.render().is_empty());
    }

    #[test]
    fn truncate_caps_to_max() {
        let mut block = DiagnosticBlock {
            file: PathBuf::from("foo.rs"),
            items: (0..30)
                .map(|i| Diagnostic {
                    line: i,
                    column: 1,
                    severity: Severity::Error,
                    message: format!("err {i}"),
                })
                .collect(),
        };
        block.truncate(20);
        assert_eq!(block.items.len(), 20);
    }

    #[test]
    fn renders_only_first_line_of_message() {
        let block = DiagnosticBlock {
            file: PathBuf::from("foo.rs"),
            items: vec![Diagnostic {
                line: 1,
                column: 1,
                severity: Severity::Error,
                message: "first line\nsecond line\nthird".to_string(),
            }],
        };
        let rendered = block.render();
        assert!(rendered.contains("first line"));
        assert!(!rendered.contains("second line"));
        assert!(!rendered.contains("third"));
    }
}
