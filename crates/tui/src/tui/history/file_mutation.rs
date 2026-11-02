//! Structured, success-only File mutation receipts.
//!
//! Tool execution owns the exact before/after evidence. This module shapes
//! that evidence for the calm transcript without depending on whether an
//! approval modal happened to run.

use std::path::{Component, Path};

use ratatui::style::Style;
use ratatui::text::Line;
use serde_json::Value;

use codewhale_engine::tools::spec::ToolResult;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::settings::InlineDiffMode;
use codewhale_tui_kit::diff_render;

use super::details_affordance_line;

const MAX_INLINE_DIFF_LINES: usize = 14;
const MAX_SUMMARY_CHARS: usize = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMutationOutcome {
    Created,
    Updated,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMutationFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub outcome: FileMutationOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMutationReceipt {
    /// Raw execution-owned evidence exposed only through the explicit detail
    /// route. It is never painted into the ambient Work/transcript surface.
    pub exact_diff: String,
    /// Header-redacted copy safe for inline presentation.
    pub display_diff: String,
    pub files: Vec<FileMutationFile>,
    pub added: usize,
    pub deleted: usize,
}

impl FileMutationReceipt {
    /// Build a receipt only from an authoritative successful tool result.
    /// Failed/cancelled calls never carry a success diff into the transcript.
    #[must_use]
    pub fn from_success(workspace: &Path, result: &ToolResult) -> Option<Self> {
        if !result.success {
            return None;
        }
        let mutation = result.metadata.as_ref()?.get("mutation")?;
        let raw_diff = mutation.get("diff")?.as_str().unwrap_or("");
        let exact_diff = raw_diff.to_string();
        let display_diff = redact_diff_headers(workspace, raw_diff);
        let mut files = Vec::new();

        if let Some(entries) = mutation.get("files").and_then(Value::as_array) {
            for entry in entries {
                let Some(raw_path) = entry.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let outcome = match entry.get("outcome").and_then(Value::as_str) {
                    Some("created") => FileMutationOutcome::Created,
                    Some("updated") => FileMutationOutcome::Updated,
                    Some("deleted") => FileMutationOutcome::Deleted,
                    _ => continue,
                };
                files.push(FileMutationFile {
                    path: privacy_safe_path(workspace, raw_path),
                    previous_path: None,
                    outcome,
                });
            }
        }
        if let Some(renames) = mutation.get("renames").and_then(Value::as_array) {
            for rename in renames {
                let Some(from) = rename.get("from").and_then(Value::as_str) else {
                    continue;
                };
                let Some(to) = rename.get("to").and_then(Value::as_str) else {
                    continue;
                };
                files.push(FileMutationFile {
                    path: privacy_safe_path(workspace, to),
                    previous_path: Some(privacy_safe_path(workspace, from)),
                    outcome: FileMutationOutcome::Renamed,
                });
            }
        }

        let totals = diff_render::diff_totals(&display_diff);
        let added = totals.added;
        let deleted = totals.deleted;
        if files.is_empty() && exact_diff.trim().is_empty() {
            return None;
        }
        Some(Self {
            exact_diff,
            display_diff,
            files,
            added,
            deleted,
        })
    }

    #[must_use]
    pub fn outcome_label(&self, locale: Locale) -> String {
        let created = self.count(FileMutationOutcome::Created);
        let updated = self.count(FileMutationOutcome::Updated);
        let deleted = self.count(FileMutationOutcome::Deleted);
        let renamed = self.count(FileMutationOutcome::Renamed);
        let mut outcome_parts = Vec::new();
        push_count(
            &mut outcome_parts,
            created,
            locale,
            MessageId::FileMutationCreatedCount,
        );
        push_count(
            &mut outcome_parts,
            updated,
            locale,
            MessageId::FileMutationUpdatedCount,
        );
        push_count(
            &mut outcome_parts,
            deleted,
            locale,
            MessageId::FileMutationDeletedCount,
        );
        push_count(
            &mut outcome_parts,
            renamed,
            locale,
            MessageId::FileMutationRenamedCount,
        );

        if self.files.is_empty() {
            tr(locale, MessageId::FileMutationChangedFiles).into_owned()
        } else if self.files.len() == 1 {
            let file = &self.files[0];
            match file.outcome {
                FileMutationOutcome::Created => {
                    tr(locale, MessageId::FileMutationCreated).replace("{path}", &file.path)
                }
                FileMutationOutcome::Updated => {
                    tr(locale, MessageId::FileMutationUpdated).replace("{path}", &file.path)
                }
                FileMutationOutcome::Deleted => {
                    tr(locale, MessageId::FileMutationDeleted).replace("{path}", &file.path)
                }
                FileMutationOutcome::Renamed => {
                    let fallback = tr(locale, MessageId::FileMutationMissingFile);
                    let paths = format!(
                        "{} → {}",
                        file.previous_path.as_deref().unwrap_or(fallback.as_ref()),
                        file.path
                    );
                    tr(locale, MessageId::FileMutationRenamed).replace("{path}", &paths)
                }
            }
        } else {
            let count = super::tool_run::counted(
                self.files.len(),
                locale,
                MessageId::ActivityFileOne,
                MessageId::ActivityFileMany,
            );
            format!("{count} · {}", outcome_parts.join(" · "))
        }
    }

    #[must_use]
    pub fn semantic_summary(&self, locale: Locale) -> String {
        let stats = format!("+{} -{}", self.added, self.deleted);
        let separator_chars = " · ".chars().count();
        let outcome_budget = MAX_SUMMARY_CHARS
            .saturating_sub(stats.chars().count())
            .saturating_sub(separator_chars);
        format!(
            "{} · {stats}",
            bounded_text(&self.outcome_label(locale), outcome_budget)
        )
    }

    #[must_use]
    pub fn inspect_text(&self, locale: Locale) -> String {
        let summary = self.semantic_summary(locale);
        if self.exact_diff.trim().is_empty() {
            format!("{summary}\n\n{}", tr(locale, MessageId::FileMutationNoText))
        } else {
            format!("{summary}\n\n{}", self.exact_diff)
        }
    }

    pub fn render_inline(
        &self,
        width: u16,
        mode: InlineDiffMode,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        match mode {
            InlineDiffMode::Off | InlineDiffMode::Summary => vec![exact_evidence_hint(locale)],
            InlineDiffMode::Full => {
                let mut lines = Vec::new();
                if !self.display_diff.trim().is_empty() {
                    let rendered = diff_render::render_diff_body_bounded(
                        &self.display_diff,
                        width,
                        MAX_INLINE_DIFF_LINES,
                    );
                    lines.extend(rendered.lines);
                    if rendered.omitted_rows > 0 {
                        let detail_hint =
                            crate::tui::key_shortcuts::tool_details_shortcut_action_hint(
                                tr(locale, MessageId::FileMutationChange).as_ref(),
                            );
                        lines.push(details_affordance_line(
                            &tr(locale, MessageId::FileMutationMoreDiff)
                                .replace("{count}", &rendered.omitted_rows.to_string())
                                .replace("{hint}", &detail_hint),
                            Style::default().fg(palette::TEXT_MUTED).italic(),
                        ));
                    } else {
                        lines.push(exact_evidence_hint(locale));
                    }
                } else {
                    lines.push(exact_evidence_hint(locale));
                }
                lines
            }
        }
    }

    fn count(&self, outcome: FileMutationOutcome) -> usize {
        self.files
            .iter()
            .filter(|file| file.outcome == outcome)
            .count()
    }
}

fn exact_evidence_hint(locale: Locale) -> Line<'static> {
    details_affordance_line(
        &crate::tui::key_shortcuts::tool_details_shortcut_action_hint(
            tr(locale, MessageId::FileMutationChange).as_ref(),
        ),
        Style::default().fg(palette::TEXT_MUTED).italic(),
    )
}

fn push_count(parts: &mut Vec<String>, count: usize, locale: Locale, label: MessageId) {
    if count > 0 {
        parts.push(tr(locale, label).replace("{count}", &count.to_string()));
    }
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut bounded = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    bounded.push('…');
    bounded
}

fn privacy_safe_path(workspace: &Path, raw: &str) -> String {
    let normalized = raw.replace('\\', "/");
    let workspace = workspace.to_string_lossy().replace('\\', "/");
    let relative = if Path::new(raw).is_absolute() || normalized.starts_with('/') {
        let prefix = workspace.trim_end_matches('/');
        if normalized == prefix {
            ""
        } else if let Some(relative) = normalized.strip_prefix(&format!("{prefix}/")) {
            relative
        } else {
            return "<external file>".to_string();
        }
    } else {
        normalized.as_str()
    };
    let path = Path::new(relative);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return "<external file>".to_string();
    }
    let display = path.to_string_lossy().replace('\\', "/");
    if display.is_empty() {
        "<workspace>".to_string()
    } else {
        display
    }
}

fn redact_diff_headers(workspace: &Path, diff: &str) -> String {
    let mut redacted = diff
        .lines()
        .map(|line| {
            for prefix in ["--- ", "+++ ", "rename from ", "rename to "] {
                if let Some(raw) = line.strip_prefix(prefix) {
                    if raw == "/dev/null" {
                        return line.to_string();
                    }
                    let side = if raw.starts_with("a/") {
                        "a/"
                    } else if raw.starts_with("b/") {
                        "b/"
                    } else {
                        ""
                    };
                    let path = raw.strip_prefix(side).unwrap_or(raw);
                    return format!("{prefix}{side}{}", privacy_safe_path(workspace, path));
                }
            }
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let mut paths = rest.split_whitespace();
                if let (Some(old), Some(new)) = (paths.next(), paths.next()) {
                    let old = old.strip_prefix("a/").unwrap_or(old);
                    let new = new.strip_prefix("b/").unwrap_or(new);
                    return format!(
                        "diff --git a/{} b/{}",
                        privacy_safe_path(workspace, old),
                        privacy_safe_path(workspace, new)
                    );
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if diff.ends_with('\n') {
        redacted.push('\n');
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn result(mutation: Value) -> ToolResult {
        ToolResult::success("ok").with_metadata(json!({ "mutation": mutation }))
    }

    #[test]
    fn creates_bounded_semantic_summary_and_redacts_external_headers() {
        let result = result(json!({
            "diff": "--- /Users/alice/private.rs\n+++ /Users/alice/private.rs\n@@ -0,0 +1 @@\n+secret\n",
            "files": [{ "path": "/Users/alice/private.rs", "outcome": "created" }],
            "renames": []
        }));
        let receipt =
            FileMutationReceipt::from_success(Path::new("/workspace"), &result).expect("receipt");
        assert_eq!(receipt.files[0].path, "<external file>");
        assert!(receipt.exact_diff.contains("alice"));
        assert!(!receipt.display_diff.contains("alice"));
        assert!(
            receipt
                .semantic_summary(Locale::En)
                .contains("Created <external file>")
        );
    }

    #[test]
    fn rename_and_multifile_outcomes_stay_semantic() {
        let result = result(json!({
            "diff": "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "files": [{ "path": "lib.rs", "outcome": "updated" }],
            "renames": [{ "from": "old.rs", "to": "new.rs" }]
        }));
        let receipt =
            FileMutationReceipt::from_success(Path::new("/workspace"), &result).expect("receipt");
        assert_eq!(receipt.files.len(), 2);
        assert_eq!(receipt.added, 1);
        assert_eq!(receipt.deleted, 1);
        assert_eq!(
            receipt.semantic_summary(Locale::En),
            "2 files · 1 updated · 1 renamed · +1 -1"
        );
        let renamed = FileMutationReceipt {
            exact_diff: String::new(),
            display_diff: String::new(),
            files: vec![FileMutationFile {
                path: "new{path}.rs".into(),
                previous_path: Some("old{path}.rs".into()),
                outcome: FileMutationOutcome::Renamed,
            }],
            added: 0,
            deleted: 0,
        };
        assert_eq!(
            renamed.outcome_label(Locale::En),
            "Renamed old{path}.rs → new{path}.rs"
        );
        for (locale, verb) in [
            (Locale::ZhHans, "已重命名"),
            (Locale::ZhHant, "已重新命名"),
            (Locale::Ja, "名前変更"),
        ] {
            let text = renamed.semantic_summary(locale);
            assert!(text.starts_with(verb), "{text}");
            assert!(text.contains("old{path}.rs → new{path}.rs"), "{text}");
            assert!(text.ends_with("+0 -0"), "{text}");
        }
    }

    #[test]
    fn failed_results_never_become_success_receipts() {
        let failed = ToolResult::error("cancelled").with_metadata(json!({
            "mutation": {
                "diff": "--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n",
                "files": [{ "path": "a", "outcome": "updated" }],
                "renames": []
            }
        }));
        assert!(FileMutationReceipt::from_success(Path::new("/workspace"), &failed).is_none());
    }

    #[test]
    fn inline_diff_modes_are_bounded_and_keep_the_exact_detail_route() {
        let additions = (0..30)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = result(json!({
            "diff": format!("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -0,0 +1,30 @@\n{additions}\n"),
            "files": [{ "path": "src/lib.rs", "outcome": "created" }],
            "renames": []
        }));
        let receipt =
            FileMutationReceipt::from_success(Path::new("/workspace"), &result).expect("receipt");

        let full = receipt.render_inline(80, InlineDiffMode::Full, Locale::En);
        let full_text = plain(&full);
        assert!(full.len() <= MAX_INLINE_DIFF_LINES + 2, "{full_text}");
        assert!(full_text.contains("line 0"), "{full_text}");
        assert!(full_text.contains("diff lines"), "{full_text}");
        assert!(full_text.contains(":change"), "{full_text}");
        assert!(!full_text.contains("summary:"), "{full_text}");

        let summary = plain(&receipt.render_inline(80, InlineDiffMode::Summary, Locale::En));
        assert!(!summary.contains("Created src/lib.rs"), "{summary}");
        assert!(!summary.contains("+30 -0"), "{summary}");
        assert!(!summary.contains("line 0"), "{summary}");
        assert!(summary.contains(":change"), "{summary}");

        let off = plain(&receipt.render_inline(80, InlineDiffMode::Off, Locale::En));
        assert!(!off.contains("line 0"), "{off}");
        assert!(!off.contains("+30 -0"), "{off}");
        assert!(off.contains(":change"), "{off}");
    }

    #[test]
    fn full_mode_spends_its_bound_on_red_green_evidence() {
        let result = result(json!({
            "diff": "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs\ndiff --git a/lib.rs b/lib.rs\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "files": [{ "path": "lib.rs", "outcome": "updated" }],
            "renames": [{ "from": "old.rs", "to": "new.rs" }]
        }));
        let receipt =
            FileMutationReceipt::from_success(Path::new("/workspace"), &result).expect("receipt");
        let full_text = plain(&receipt.render_inline(80, InlineDiffMode::Full, Locale::En));

        assert!(full_text.contains("- old"), "{full_text}");
        assert!(full_text.contains("+ new"), "{full_text}");
        assert!(!full_text.contains("summary:"), "{full_text}");
    }

    #[test]
    fn bounded_summary_never_truncates_semantic_stats() {
        let long_path = format!("src/{}.rs", "whale".repeat(80));
        let result = result(json!({
            "diff": format!("--- a/{long_path}\n+++ b/{long_path}\n@@ -1 +1 @@\n-old\n+new\n"),
            "files": [{ "path": long_path, "outcome": "updated" }],
            "renames": []
        }));
        let receipt =
            FileMutationReceipt::from_success(Path::new("/workspace"), &result).expect("receipt");
        let summary = receipt.semantic_summary(Locale::En);
        assert!(summary.ends_with(" · +1 -1"), "{summary}");
        assert!(summary.chars().count() <= MAX_SUMMARY_CHARS);
    }

    #[test]
    fn failed_file_cell_never_paints_a_forged_success_receipt() {
        let receipt = FileMutationReceipt::from_success(
            Path::new("/workspace"),
            &result(json!({
                "diff": "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+FORGED-SUCCESS\n",
                "files": [{ "path": "a.rs", "outcome": "updated" }],
                "renames": []
            })),
        )
        .expect("receipt");
        let cell = super::super::PatchSummaryCell {
            path: "a.rs".to_string(),
            summary: "editing".to_string(),
            status: super::super::ToolStatus::Failed,
            error: Some("cancelled".to_string()),
            receipt: Some(receipt),
        };

        let text = plain(&cell.render(
            80,
            true,
            super::super::RenderMode::Live,
            InlineDiffMode::Full,
            codewhale_localization::Locale::En,
        ));
        assert!(text.contains("cancelled"), "{text}");
        assert!(!text.contains("FORGED-SUCCESS"), "{text}");
    }
}
