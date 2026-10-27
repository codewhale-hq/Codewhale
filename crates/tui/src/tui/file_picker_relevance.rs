//! Helpers that decide which workspace files to surface in the
//! `/files` picker.
//!
//! The picker ranks files by three signals harvested from the running
//! session:
//!
//! * `modified` — files git reports as staged/unstaged or untracked
//! * `mentioned` — files the user @-referenced in the composer
//! * `tool` — files that recent tool calls touched (input or output)
//!
//! [`build_relevance`] composes those signals into a
//! `FilePickerRelevance` that the picker view uses to order results.
//! The remaining helpers are deterministic string/path utilities that
//! make path discovery resilient to quoting, leading `./`, and
//! trailing `:line` markers.

use codewhale_dependencies::{ExternalTool, Git};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::tui::app::App;
use crate::tui::app::ToolDetailRecord;
use crate::tui::file_picker::FilePickerRelevance;
use crate::tui::file_picker::FilePickerView;
use codewhale_core::{ContextReferenceKind, ContextReferenceSource};

/// Push the `/files` picker onto the view stack, pre-populated with
/// per-session relevance ranks (modified, @-mentioned, tool-touched).
pub(super) fn open_file_picker(app: &mut App) {
    let relevance = build_relevance(app);
    // Honor the configured `mention_walk_depth` (0 = unlimited) so the picker
    // and `@`-mention completion agree, and files in deeply nested trees stay
    // discoverable (#2488).
    app.view_stack
        .push(FilePickerView::new_with_relevance_and_depth(
            &app.workspace,
            relevance,
            app.mention_walk_depth,
            app.ui_locale,
        ));
}

/// Compose the in-memory relevance signals (@-mentions, tool-touched paths).
///
/// The git-reported `modified` signal is deliberately *not* gathered here: it
/// costs a subprocess, so the picker folds it in from its background scan
/// (#3905).
pub(super) fn build_relevance(app: &App) -> FilePickerRelevance {
    let mut relevance = FilePickerRelevance::default();

    for record in app.session_context_references.iter().rev().take(64) {
        let reference = &record.reference;
        if reference.source != ContextReferenceSource::AtMention {
            continue;
        }
        if !matches!(reference.kind, ContextReferenceKind::File) {
            continue;
        }
        for raw in [&reference.target, &reference.label] {
            if let Some(path) = workspace_file_candidate(raw, &app.workspace) {
                relevance.mark_mentioned(path);
            }
        }
    }

    let mut seen_tool_paths = HashSet::new();
    for detail in app.active_tool_details.values() {
        mark_tool_detail_paths(detail, &app.workspace, &mut seen_tool_paths, &mut relevance);
    }
    let mut rows: Vec<_> = app.tool_details_by_cell.iter().collect();
    rows.sort_by_key(|(idx, _)| std::cmp::Reverse(**idx));
    for (_, detail) in rows.into_iter().take(48) {
        mark_tool_detail_paths(detail, &app.workspace, &mut seen_tool_paths, &mut relevance);
    }

    relevance
}

/// Paths git reports as staged/unstaged/untracked.
///
/// Blocking: spawns `git status` and waits. The picker runs this on a blocking
/// task rather than the event loop (#3905), so it lives here but is called
/// from `file_picker.rs`.
pub(super) fn modified_workspace_paths(workspace: &Path) -> Vec<String> {
    let Some(mut cmd) = Git::command() else {
        return Vec::new();
    };
    let Ok(output) = cmd
        .arg("-C")
        .arg(workspace)
        .args(["status", "--short", "--untracked-files=normal"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_git_status_path)
        .filter_map(|path| workspace_file_candidate(&path, workspace))
        .collect()
}

pub(super) fn parse_git_status_path(line: &str) -> Option<String> {
    if line.len() < 4 {
        return None;
    }
    let raw = line.get(3..)?.trim();
    let raw = raw.rsplit(" -> ").next().unwrap_or(raw).trim();
    let raw = raw.trim_matches('"');
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

fn mark_tool_detail_paths(
    detail: &ToolDetailRecord,
    workspace: &Path,
    seen: &mut HashSet<String>,
    relevance: &mut FilePickerRelevance,
) {
    let mut budget = 256usize;
    mark_tool_paths_from_value(&detail.input, workspace, seen, relevance, &mut budget);
    if let Some(output) = detail
        .output
        .as_deref()
        .filter(|output| output.len() <= 8_192)
    {
        mark_tool_paths_from_text(output, workspace, seen, relevance, &mut budget);
    }
}

fn mark_tool_paths_from_value(
    value: &serde_json::Value,
    workspace: &Path,
    seen: &mut HashSet<String>,
    relevance: &mut FilePickerRelevance,
    budget: &mut usize,
) {
    if *budget == 0 {
        return;
    }
    match value {
        serde_json::Value::String(text) => {
            mark_tool_paths_from_text(text, workspace, seen, relevance, budget);
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mark_tool_paths_from_value(item, workspace, seen, relevance, budget);
                if *budget == 0 {
                    break;
                }
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                mark_tool_paths_from_value(item, workspace, seen, relevance, budget);
                if *budget == 0 {
                    break;
                }
            }
        }
        _ => {}
    }
}

pub(super) fn mark_tool_paths_from_text(
    text: &str,
    workspace: &Path,
    seen: &mut HashSet<String>,
    relevance: &mut FilePickerRelevance,
    budget: &mut usize,
) {
    if *budget == 0 || text.len() > 8_192 {
        return;
    }
    if let Some(path) = workspace_file_candidate(text, workspace)
        && seen.insert(path.clone())
    {
        relevance.mark_tool(path);
        *budget = (*budget).saturating_sub(1);
    }
    for token in text.split_whitespace().take(128) {
        if *budget == 0 {
            break;
        }
        if let Some(path) = workspace_file_candidate(token, workspace)
            && seen.insert(path.clone())
        {
            relevance.mark_tool(path);
            *budget = (*budget).saturating_sub(1);
        }
    }
}

pub(super) fn workspace_file_candidate(raw: &str, workspace: &Path) -> Option<String> {
    let cleaned = clean_path_token(raw)?;
    let path = Path::new(&cleaned);
    let absolute = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    };
    if !absolute.is_file() {
        return None;
    }
    let rel = absolute.strip_prefix(workspace).ok()?;
    workspace_path_to_picker_string(rel)
}

fn clean_path_token(raw: &str) -> Option<String> {
    let mut trimmed = raw.trim().trim_matches(|ch: char| {
        ch.is_ascii_whitespace()
            || matches!(
                ch,
                '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
            )
    });
    if let Some(stripped) = trimmed.strip_prefix("./") {
        trimmed = stripped;
    }
    if let Some((before, after)) = trimmed.rsplit_once(':')
        && !before.is_empty()
        && after.chars().all(|ch| ch.is_ascii_digit())
    {
        trimmed = before;
    }
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn workspace_path_to_picker_string(path: &Path) -> Option<String> {
    let mut out = String::new();
    for (idx, component) in path.components().enumerate() {
        if matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        ) {
            return None;
        }
        if idx > 0 {
            out.push('/');
        }
        out.push_str(&component.as_os_str().to_string_lossy());
    }
    if out.is_empty() { None } else { Some(out) }
}
