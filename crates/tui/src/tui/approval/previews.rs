//! Shell, file-write, edit, and apply-patch approval previews.
//!
//! Preview generation stays separate from modal state so bounded formatting
//! and exact detail-pager rendering can be reviewed without changing approval
//! decisions or persistence semantics.

use std::borrow::Cow;

use serde_json::Value;

use crate::tools::apply_patch::{NormalizedApplyPatchInput, normalize_apply_patch_input};
use crate::tools::canonical_action::canonical_action_alias;
use codewhale_localization::{Locale, MessageId, tr};

pub(super) fn file_write_preview_lines(tool_name: &str, params: &Value) -> Option<Vec<String>> {
    match canonical_action_alias(tool_name, params) {
        "write_file" => {
            let content = param_text(params, &["content"])?;
            Some(prefixed_preview_lines(
                "proposed content",
                "+ ",
                &content,
                5,
            ))
        }
        "edit_file" => {
            // Keep the per-frame card preview bounded. The details pager builds the
            // complete version lazily when the reviewer asks for it.
            edit_file_preview_lines(params, 3)
        }
        "apply_patch" => match normalize_apply_patch_input(params) {
            Ok(NormalizedApplyPatchInput::Patch(patch)) => apply_patch_preview_lines(patch),
            Ok(NormalizedApplyPatchInput::Replacement { entries, .. }) => {
                changes_preview_lines(entries)
            }
            Err(_) => None,
        },
        _ => None,
    }
    .filter(|lines| !lines.is_empty())
}

fn edit_file_preview_lines(params: &Value, max_lines: usize) -> Option<Vec<String>> {
    if let Some(edits) = params.get("edits").and_then(Value::as_array) {
        // One edit gets the whole per-side budget. Several share the card, so
        // each shows its first changed line and a count of the rest.
        let numbered = edits.len() > 1;
        let per_side = if numbered { 1 } else { max_lines };
        let mut lines = Vec::new();
        for (index, edit) in edits.iter().take(max_lines).enumerate() {
            let old = param_text(edit, &["oldText"])?;
            let new = param_text(edit, &["newText"])?;
            if numbered {
                lines.push(format!("edit {}", index + 1));
            }
            push_changed_lines(&mut lines, &old, &new, per_side);
        }
        if edits.len() > max_lines {
            lines.push(format!("... (+{} more edits)", edits.len() - max_lines));
        }
        return (!lines.is_empty()).then_some(lines);
    }
    let search = param_text(params, &["search"])?;
    let replace = param_text(params, &["replace"])?;
    let mut lines = Vec::new();
    push_changed_lines(&mut lines, &search, &replace, max_lines);
    Some(lines)
}

/// Append what an edit removes (`- `) and adds (`+ `), bounded per side.
///
/// Lines both sides share at the start and end are left out, and the rows
/// carry no sub-labels: the card has room for only a few preview rows, so
/// they go to the change itself. The details pager keeps the exact full text.
fn push_changed_lines(lines: &mut Vec<String>, old: &str, new: &str, max_lines: usize) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let shared = old.len().min(new.len());
    let mut head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let mut tail = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(shared - head)
        .take_while(|(a, b)| a == b)
        .count();
    if old.len() == new.len() && head == old.len() {
        // Same lines on both sides (a line-ending-only edit): there is no
        // changed region to narrow to, so show both sides whole.
        head = 0;
        tail = 0;
    }
    for (prefix, side) in [
        ("- ", &old[head..old.len() - tail]),
        ("+ ", &new[head..new.len() - tail]),
    ] {
        lines.extend(
            side.iter()
                .take(max_lines)
                .map(|line| format!("{prefix}{line}")),
        );
        if side.len() > max_lines {
            lines.push(format!("... (+{} more lines)", side.len() - max_lines));
        }
    }
}

pub(super) fn exact_edit_file_preview_lines(params: &Value, locale: Locale) -> Option<Vec<String>> {
    if let Some(edits) = params.get("edits").and_then(Value::as_array) {
        let mut lines = Vec::new();
        for (index, edit) in edits.iter().enumerate() {
            let old = param_text(edit, &["oldText"])?;
            let new = param_text(edit, &["newText"])?;
            lines.push(format!("edit {}", index + 1));
            lines.push(tr(locale, MessageId::ApprovalLabelReplaceThis).into_owned());
            lines.extend(exact_preview_body_lines("- ", &old));
            lines.push(tr(locale, MessageId::ApprovalLabelWithThis).into_owned());
            lines.extend(exact_preview_body_lines("+ ", &new));
        }
        return (!lines.is_empty()).then_some(lines);
    }
    let search = param_text(params, &["search"])?;
    let replace = param_text(params, &["replace"])?;
    let mut lines = vec![tr(locale, MessageId::ApprovalLabelReplaceThis).into_owned()];
    lines.extend(exact_preview_body_lines("- ", &search));
    lines.push(tr(locale, MessageId::ApprovalLabelWithThis).into_owned());
    lines.extend(exact_preview_body_lines("+ ", &replace));
    Some(lines)
}

fn exact_preview_body_lines(prefix: &str, content: &str) -> Vec<String> {
    if content.is_empty() {
        return vec![format!("{prefix}\"\"")];
    }

    content
        .split_inclusive('\n')
        .map(|chunk| {
            let (body, ending) = if let Some(body) = chunk.strip_suffix("\r\n") {
                (body, "\\r\\n")
            } else if let Some(body) = chunk.strip_suffix('\n') {
                (body, "\\n")
            } else {
                (chunk, "")
            };
            exact_preview_body_line(prefix, body, ending)
        })
        .collect()
}

fn exact_preview_body_line(prefix: &str, body: &str, ending: &str) -> String {
    let mut line = String::with_capacity(prefix.len() + body.len() + ending.len() + 2);
    line.push_str(prefix);
    line.push('"');
    for ch in body.chars() {
        match ch {
            '\\' => line.push_str("\\\\"),
            '"' => line.push_str("\\\""),
            ' ' => line.push_str("\\x20"),
            '\t' => line.push_str("\\t"),
            '\r' => line.push_str("\\r"),
            ch if ch.is_whitespace() || ch.is_control() => line.extend(ch.escape_unicode()),
            ch => line.push(ch),
        }
    }
    line.push_str(ending);
    line.push('"');
    line
}

fn prefixed_preview_lines(
    header: &str,
    prefix: &str,
    content: &str,
    max_lines: usize,
) -> Vec<String> {
    let mut lines = vec![header.to_string()];
    if content.is_empty() {
        lines.push(format!("{prefix}<empty>"));
        return lines;
    }

    let total = content.lines().count();
    for line in content.lines().take(max_lines) {
        lines.push(format!("{prefix}{line}"));
    }
    if total > max_lines {
        lines.push(format!("... (+{} more lines)", total - max_lines));
    }
    lines
}

fn push_preview_line(lines: &mut Vec<String>, line: impl Into<String>, limit: usize) -> bool {
    if lines.len() >= limit {
        return false;
    }
    lines.push(line.into());
    true
}

fn append_preview_truncation(lines: &mut Vec<String>, line: String, limit: usize) {
    if push_preview_line(lines, line.clone(), limit) {
        return;
    }
    if let Some(last) = lines.last_mut() {
        *last = line;
    }
}

pub(super) fn apply_patch_preview_lines(patch: &str) -> Option<Vec<String>> {
    const PREVIEW_LIMIT: usize = 7;

    let mut lines = Vec::new();
    let mut omitted = 0usize;
    for line in patch.lines().filter(|line| !line.trim().is_empty()) {
        let is_diff_header = line.starts_with("diff --git ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("@@");
        let is_change_line = (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"));
        if is_diff_header || is_change_line {
            if !push_preview_line(&mut lines, line, PREVIEW_LIMIT) {
                omitted += 1;
            }
        } else {
            omitted += 1;
        }
    }

    if lines.is_empty() {
        omitted = 0;
        for line in patch.lines().filter(|line| !line.trim().is_empty()) {
            if !push_preview_line(&mut lines, line, PREVIEW_LIMIT) {
                omitted += 1;
            }
        }
    }

    if omitted > 0 {
        if lines.len() >= PREVIEW_LIMIT {
            omitted += 1;
        }
        append_preview_truncation(
            &mut lines,
            format!("... (+{omitted} more patch lines)"),
            PREVIEW_LIMIT,
        );
    }
    if lines.is_empty() { None } else { Some(lines) }
}

fn changes_preview_lines(changes: &[Value]) -> Option<Vec<String>> {
    const PREVIEW_LIMIT: usize = 7;

    let mut lines = Vec::new();
    let mut rendered_changes = 0usize;
    for (idx, change) in changes.iter().enumerate() {
        let path = change
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<file>");
        let content = change.get("content").and_then(Value::as_str).unwrap_or("");
        if idx > 0 && !push_preview_line(&mut lines, String::new(), PREVIEW_LIMIT) {
            break;
        }
        if !push_preview_line(&mut lines, format!("file: {path}"), PREVIEW_LIMIT) {
            break;
        }
        rendered_changes += 1;
        for line in prefixed_preview_lines("replacement content", "+ ", content, PREVIEW_LIMIT)
            .into_iter()
            .skip(1)
        {
            if !push_preview_line(&mut lines, line, PREVIEW_LIMIT) {
                break;
            }
        }
        if lines.len() >= PREVIEW_LIMIT {
            break;
        }
    }
    let skipped_changes = changes.len().saturating_sub(rendered_changes);
    if skipped_changes > 0 {
        append_preview_truncation(
            &mut lines,
            format!("... (+{skipped_changes} more files)"),
            PREVIEW_LIMIT,
        );
    }
    if lines.is_empty() { None } else { Some(lines) }
}

pub(super) fn param_text(params: &Value, keys: &[&str]) -> Option<String> {
    let Value::Object(map) = params else {
        return None;
    };

    for key in keys {
        let Some(value) = map.get(*key) else {
            continue;
        };
        match value {
            Value::String(text) => return Some(text.clone()),
            Value::Number(number) => return Some(number.to_string()),
            Value::Bool(flag) => return Some(flag.to_string()),
            other => return Some(other.to_string()),
        }
    }

    None
}

pub(super) fn localize_detail_label(label: &str, locale: Locale) -> Cow<'static, str> {
    match label {
        "Command" => tr(locale, MessageId::ApprovalLabelCommand),
        "Dir" => tr(locale, MessageId::ApprovalLabelDir),
        "File" => tr(locale, MessageId::ApprovalLabelFile),
        "Preview" => tr(locale, MessageId::ApprovalLabelPreview),
        "proposed content" => tr(locale, MessageId::ApprovalLabelProposedContent),
        "replace this" => tr(locale, MessageId::ApprovalLabelReplaceThis),
        "with this" => tr(locale, MessageId::ApprovalLabelWithThis),
        "replacement content" => tr(locale, MessageId::ApprovalLabelReplacementContent),
        "Path" => tr(locale, MessageId::ApprovalLabelPath),
        "Target" => tr(locale, MessageId::ApprovalLabelTarget),
        "Input" => tr(locale, MessageId::ApprovalLabelInput),
        "Action" => tr(locale, MessageId::ApprovalLabelAction),
        "Type" => tr(locale, MessageId::ApprovalLabelType),
        "Prompt" => tr(locale, MessageId::ApprovalLabelPrompt),
        _ if locale != Locale::ZhHans => label.to_string().into(),
        "Goal" => "目标".into(),
        "Children" => "子任务".into(),
        "Writes" => "写入".into(),
        "Shell" => "Shell".into(),
        "Network" => "网络".into(),
        "Budget" => "预算".into(),
        "Trust mode" => "信任模式".into(),
        "Auto-approve" => "自动批准".into(),
        "Mode" => "模式".into(),
        "Workspace" => "工作区".into(),
        _ => label.to_string().into(),
    }
}

pub(super) fn localize_preview_shell_line(
    tool_name: &str,
    line: &str,
    locale: Locale,
) -> Cow<'static, str> {
    match tool_name {
        "write_file" if line == "proposed content" => localize_detail_label(line, locale),
        _ => line.to_string().into(),
    }
}

pub(crate) fn format_shell_command_for_approval(command: &str) -> Vec<String> {
    if let Some(preview) = parse_printf_write_file_command(command) {
        return format_printf_write_file_preview(preview);
    }

    let mut out = Vec::new();
    for raw_line in command.lines() {
        split_shell_display_line(raw_line, &mut out);
    }
    if out.is_empty() && !command.trim().is_empty() {
        out.push(command.trim().to_string());
    }
    out
}

fn split_shell_display_line(line: &str, out: &mut Vec<String>) {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut current = String::new();
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }

        if matches!(ch, '"' | '\'') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
            current.push(ch);
            continue;
        }

        if quote.is_none() {
            match ch {
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    push_shell_clause(out, &mut current, Some("&&"));
                    continue;
                }
                '|' if chars.peek() == Some(&'|') => {
                    chars.next();
                    push_shell_clause(out, &mut current, Some("||"));
                    continue;
                }
                '|' => {
                    push_shell_clause(out, &mut current, Some("|"));
                    continue;
                }
                ';' => {
                    push_shell_clause(out, &mut current, Some(";"));
                    continue;
                }
                _ => {}
            }
        }

        current.push(ch);
    }

    push_shell_clause(out, &mut current, None);
}

fn push_shell_clause(out: &mut Vec<String>, current: &mut String, operator: Option<&str>) {
    let trimmed = current.trim();
    if trimmed.is_empty() {
        if let Some(operator) = operator {
            out.push(operator.to_string());
        }
    } else if let Some(operator) = operator {
        out.push(format!("{trimmed} {operator}"));
    } else {
        out.push(trimmed.to_string());
    }
    current.clear();
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PrintfWriteFilePreview {
    target: String,
    lines: Vec<String>,
}

fn parse_printf_write_file_command(command: &str) -> Option<PrintfWriteFilePreview> {
    // A chained or substituted command is not a plain file write: collapsing
    // it into `printf > target` would hide everything after the operator
    // from the approval card. Scan the whole command (not the halves around
    // the redirect) so quote state is never read from a mid-string split.
    // shlex and the scanners below only understand POSIX quoting. Bash's
    // ANSI-C and locale quotes need their original command shown in full.
    if command.contains("$'") || command.contains("$\"") || has_unquoted_shell_control(command) {
        return None;
    }
    let (before_redirect, after_redirect) = split_unquoted_redirect(command)?;
    let before_redirect = before_redirect.trim();
    if !before_redirect.starts_with("printf") {
        return None;
    }

    let tokens = shlex::split(before_redirect)?;
    if tokens.first()?.as_str() != "printf" {
        return None;
    }
    let target_parts = shlex::split(after_redirect.trim())?;
    if target_parts.len() != 1 {
        return None;
    }
    let target = target_parts
        .into_iter()
        .next()?
        .trim_matches(|ch| ch == '"' || ch == '\'')
        .to_string();
    if target.is_empty() {
        return None;
    }

    let args = &tokens[1..];
    if args.is_empty() {
        return None;
    }
    let values = if args.len() >= 2 && args[0].contains('%') {
        &args[1..]
    } else {
        args
    };
    let mut lines = Vec::new();
    for value in values {
        let normalized = value.replace("\\n", "\n");
        for line in normalized.lines() {
            lines.push(line.to_string());
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }

    Some(PrintfWriteFilePreview { target, lines })
}

fn format_printf_write_file_preview(preview: PrintfWriteFilePreview) -> Vec<String> {
    const MAX_PREVIEW_LINES: usize = 12;
    let mut out = vec![format!("printf > {}", preview.target)];
    let total = preview.lines.len();
    for line in preview.lines.into_iter().take(MAX_PREVIEW_LINES) {
        out.push(format!("  {line}"));
    }
    if total > MAX_PREVIEW_LINES {
        out.push(format!("  ... (+{} more lines)", total - MAX_PREVIEW_LINES));
    }
    out
}

/// Whether `text` holds shell syntax that runs or chains another command:
/// an unquoted `;`, `&`, `|`, newline, `(` (subshell, `$(`, `<(`), or a
/// command substitution (`$(` / backtick) even inside double quotes.
fn has_unquoted_shell_control(text: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut prev: Option<char> = None;
    for ch in text.chars() {
        if escaped {
            escaped = false;
            prev = None;
            continue;
        }
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some(_) => match ch {
                '\\' => escaped = true,
                '"' => quote = None,
                '`' => return true,
                '(' if prev == Some('$') => return true,
                _ => {}
            },
            None => match ch {
                '\\' => escaped = true,
                '\'' | '"' => quote = Some(ch),
                ';' | '&' | '|' | '`' | '(' | '\n' | '\r' => return true,
                _ => {}
            },
        }
        prev = Some(ch);
    }
    false
}

fn split_unquoted_redirect(command: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in command.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(ch, '"' | '\'') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
            continue;
        }
        if quote.is_none() && ch == '>' {
            return Some((&command[..idx], &command[idx + ch.len_utf8()..]));
        }
    }
    None
}
