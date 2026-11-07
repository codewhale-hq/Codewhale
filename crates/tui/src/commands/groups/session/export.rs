//! `/export` command — portable handler over the session-export facet
//! (FEAT-025 D1-D9).
//!
//! Parsing, document rendering, redaction, operation sequencing, and result
//! composition are portable: this module depends only on the external command
//! contract, `chrono`, the shared pure sanitizer in
//! [`codewhale_sanitize::sanitize`], and the shared session `CommandResult`.
//! Concrete `App`, clipboard, filesystem, snapshot, history, and turn-handoff
//! access stays behind `CommandSessionExportContext` (D1). Helpers, tests, and
//! this handler therefore carry no TUI, client, configuration, or filesystem
//! dependency, so the slice can move to `codewhale-commands` unchanged (D8/D10).
//!
//! A conversation renders to Markdown here, or to a self-contained HTML page
//! or a structured JSON document in the sibling `export_html` and
//! `export_json` modules, all from the same sanitized projection.
//!
//! Known limitations: HTML and JSON omit the workspace restore-point table,
//! which is local state with no meaning outside this machine; HTML shows
//! message text verbatim instead of rendering its Markdown; `--no-tools`
//! cannot filter the visible-history fallback, which carries no structured
//! tool blocks; turn handoffs and clipboard exports are Markdown only.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use codewhale_command_contract::facets::{
    CommandSessionExportContext, ConversationExportProjection, ExportBlock, ExportMessage,
    ExportMetadata, HistoryEntry, RestorePointProjection, RestoreSnapshot, TranscriptProjection,
    TurnHandoffProjection,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};
use codewhale_sanitize::sanitize::{
    inline_text, is_internal_role, redact_json, redact_url_for_display, sanitize_text,
};
use serde_json::Value;

use super::CommandResult;

pub(in crate::commands) struct ExportCmd;

pub(in crate::commands) const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "export",
    aliases: &["daochu"],
    usage: "/export [clipboard|file [path]|turn [clipboard|file [path]]] [--format md|html|json] [--no-tools] [--force]",
    description_key: "cmd_export_description",
};

impl ContractRegisterCommand<CommandResult> for ExportCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::SESSION_EXPORT,
            handler: export_contextual,
        }
    }
}

pub(in crate::commands) fn export_contextual(
    contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(export) = parts.export.as_deref() else {
        return CommandResult::error("Command capability unavailable: session_export".to_string());
    };
    export_portable(export, arg)
}

/// Portable `/export` composed entirely from contract-owned data and facet
/// operations. Parse first, render the selected scope second, then run the
/// destination-specific sequence (D6/D7).
pub(in crate::commands) fn export_portable(
    export: &dyn CommandSessionExportContext,
    arg: Option<&str>,
) -> CommandResult {
    let request = match parse_request(arg) {
        Ok(request) => request,
        Err(err) => return CommandResult::error(err),
    };
    let (label, stem, document) = match request.scope {
        ExportScope::Conversation => {
            let projection = export.conversation_projection();
            let stem = export_file_stem(&projection.metadata);
            let document = render_export(projection, request.format, request.include_tools);
            ("Conversation", stem, document)
        }
        ExportScope::Turn => (
            "Turn handoff",
            format!(
                "codewhale-turn-{}",
                file_timestamp(chrono::Utc::now().timestamp())
            ),
            sanitize_turn_handoff(&export.turn_handoff_projection()),
        ),
    };

    match request.destination {
        ExportDestination::Clipboard => copy_to_clipboard(export, label, &document),
        ExportDestination::File { path, force } => {
            let path = match path {
                Some(path) => export.resolve_export_path(&path),
                None => {
                    export.default_export_path(&format!("{stem}.{}", request.format.extension()))
                }
            };
            let path = match path {
                Ok(path) => path,
                Err(err) => return CommandResult::error(err),
            };
            match export.write_export_file(&path, document.as_bytes(), force) {
                Ok(()) => CommandResult::message(format!(
                    "{label} exported to {} ({}, {} lines{}){}",
                    path.display(),
                    request.format.name(),
                    document.lines().count(),
                    redaction_note(count_redactions(&document)),
                    if force {
                        " (overwrite explicitly allowed)"
                    } else {
                        ""
                    }
                )),
                Err(err) => CommandResult::error(format!(
                    "Failed to export {label} to {}: {err}",
                    path.display()
                )),
            }
        }
    }
}

fn render_export(
    projection: ConversationExportProjection,
    format: ExportFormat,
    include_tools: bool,
) -> String {
    let projection = if include_tools {
        projection
    } else {
        without_tool_blocks(projection)
    };
    match format {
        ExportFormat::Markdown => render_conversation(projection),
        ExportFormat::Html => super::export_html::render(projection),
        ExportFormat::Json => super::export_json::render(projection),
    }
}

fn is_tool_block(block: &ExportBlock) -> bool {
    matches!(
        block,
        ExportBlock::ToolCall { .. }
            | ExportBlock::ToolResult { .. }
            | ExportBlock::ServerToolCall { .. }
            | ExportBlock::ToolSearchResult { .. }
            | ExportBlock::CodeExecutionResult { .. }
    )
}

fn without_tool_blocks(
    mut projection: ConversationExportProjection,
) -> ConversationExportProjection {
    if let TranscriptProjection::Authoritative(messages) = &mut projection.transcript {
        messages.retain_mut(|message| {
            let before = message.blocks.len();
            message.blocks.retain(|block| !is_tool_block(block));
            before == message.blocks.len()
                || message
                    .blocks
                    .iter()
                    .any(|block| !matches!(block, ExportBlock::InternalReasoning))
        });
        projection.metadata.message_count = messages.len();
    }
    projection
}

fn redaction_note(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => ", 1 redaction".to_string(),
        count => format!(", {count} redactions"),
    }
}

/// Count the redaction markers the shared sanitizer leaves behind: `[redacted…]`
/// for secrets and keys, `***:***@` for URL credentials, and an encoded `***`
/// for sensitive URL query values.
pub(in crate::commands) fn count_redactions(document: &str) -> usize {
    ["[redacted", "***:***@", "=%2A%2A%2A"]
        .iter()
        .map(|marker| document.matches(marker).count())
        .sum()
}

const EXPORT_SLUG_MAX_LEN: usize = 40;

fn export_file_stem(metadata: &ExportMetadata) -> String {
    let slug = [
        metadata.title.as_deref(),
        Some(metadata.session_label.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(|text| slugify(&sanitize_text(text)))
    .find(|slug| !slug.is_empty())
    .unwrap_or_else(|| "session".to_string());
    format!(
        "codewhale-{slug}-{}",
        file_timestamp(metadata.exported_at_unix)
    )
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for ch in text.chars() {
        if slug.len() >= EXPORT_SLUG_MAX_LEN {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(slug.trim_end_matches('-').len());
    slug
}

fn file_timestamp(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|time| time.format("%Y%m%d-%H%M%S").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn tool_names(transcript: &TranscriptProjection) -> HashMap<String, String> {
    let mut names = HashMap::new();
    if let TranscriptProjection::Authoritative(messages) = transcript {
        for block in messages.iter().flat_map(|message| &message.blocks) {
            if let ExportBlock::ToolCall { id, name, .. }
            | ExportBlock::ServerToolCall { id, name, .. } = block
            {
                names.insert(id.clone(), name.clone());
            }
        }
    }
    names
}

pub(super) fn role_label(message: &ExportMessage) -> String {
    let carries_only_tool_results = message.is_user_role
        && !message.blocks.is_empty()
        && message.blocks.iter().all(|block| {
            matches!(
                block,
                ExportBlock::ToolResult { .. }
                    | ExportBlock::ToolSearchResult { .. }
                    | ExportBlock::CodeExecutionResult { .. }
            )
        });
    if carries_only_tool_results {
        "tool".to_string()
    } else {
        inline_text(&message.role)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportScope {
    Conversation,
    Turn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    Markdown,
    Html,
    Json,
}

impl ExportFormat {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    fn from_path(path: &str) -> Self {
        match Path::new(path).extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") => {
                Self::Html
            }
            Some(ext) if ext.eq_ignore_ascii_case("json") => Self::Json,
            _ => Self::Markdown,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Html => "html",
            Self::Json => "json",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Html => "HTML",
            Self::Json => "JSON",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExportDestination {
    Clipboard,
    File { path: Option<String>, force: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportRequest {
    scope: ExportScope,
    destination: ExportDestination,
    format: ExportFormat,
    include_tools: bool,
}

struct Token {
    text: String,
    quoted: bool,
}

impl Token {
    fn is_word(&self, word: &str) -> bool {
        !self.quoted && self.text.eq_ignore_ascii_case(word)
    }
}

fn tokenize(raw: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = raw.chars().peekable();
    while let Some(&first) = chars.peek() {
        if first.is_whitespace() {
            chars.next();
            continue;
        }
        let mut text = String::new();
        if first == '"' || first == '\'' {
            chars.next();
            let mut closed = false;
            for ch in chars.by_ref() {
                if ch == first {
                    closed = true;
                    break;
                }
                text.push(ch);
            }
            if !closed {
                return Err(export_usage("unterminated quote"));
            }
            if chars.peek().is_some_and(|next| !next.is_whitespace()) {
                return Err(export_usage("unexpected text after a closing quote"));
            }
            tokens.push(Token { text, quoted: true });
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                text.push(ch);
                chars.next();
            }
            tokens.push(Token {
                text,
                quoted: false,
            });
        }
    }
    Ok(tokens)
}

fn parse_request(arg: Option<&str>) -> Result<ExportRequest, String> {
    let mut tokens = tokenize(arg.unwrap_or(""))?.into_iter();
    let mut positionals: Vec<Token> = Vec::new();
    let mut force = false;
    let mut include_tools = true;
    let mut format: Option<ExportFormat> = None;
    while let Some(token) = tokens.next() {
        if token.quoted || !token.text.starts_with("--") {
            positionals.push(token);
            continue;
        }
        let option = token.text.to_ascii_lowercase();
        let value = match option.as_str() {
            "--force" => {
                force = true;
                continue;
            }
            "--no-tools" => {
                include_tools = false;
                continue;
            }
            "--format" => match tokens.next() {
                Some(next) if next.quoted || !next.text.starts_with("--") => next.text,
                _ => return Err(export_usage("--format needs a value: md, html, or json")),
            },
            _ if option.starts_with("--format=") => token.text["--format=".len()..].to_string(),
            _ => {
                return Err(export_usage(&format!(
                    "unknown option `{}`",
                    inline_text(&token.text)
                )));
            }
        };
        let parsed = ExportFormat::parse(&value).ok_or_else(|| {
            export_usage(&format!(
                "unknown format `{}`; use md, html, or json",
                inline_text(&value)
            ))
        })?;
        if format.replace(parsed).is_some() {
            return Err(export_usage("--format was given more than once"));
        }
    }

    let scope = if positionals
        .first()
        .is_some_and(|token| token.is_word("turn"))
    {
        positionals.remove(0);
        ExportScope::Turn
    } else {
        ExportScope::Conversation
    };

    let wants_file = scope == ExportScope::Conversation
        && format.is_some_and(|format| format != ExportFormat::Markdown);
    let destination = if positionals.is_empty() {
        if wants_file {
            ExportDestination::File { path: None, force }
        } else {
            ExportDestination::Clipboard
        }
    } else if positionals[0].is_word("clipboard") {
        if positionals.len() > 1 {
            return Err(export_usage("clipboard does not accept a path"));
        }
        ExportDestination::Clipboard
    } else {
        let path_tokens = if positionals[0].is_word("file") {
            &positionals[1..]
        } else {
            &positionals[..]
        };
        let path = (!path_tokens.is_empty()).then(|| {
            path_tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        });
        ExportDestination::File { path, force }
    };
    if force && destination == ExportDestination::Clipboard {
        return Err(export_usage("--force only applies to file exports"));
    }

    let format = match scope {
        ExportScope::Turn => {
            if format.is_some_and(|format| format != ExportFormat::Markdown) {
                return Err(export_usage("turn handoffs are Markdown only"));
            }
            if !include_tools {
                return Err(export_usage(
                    "--no-tools applies to conversation exports, not turn handoffs",
                ));
            }
            ExportFormat::Markdown
        }
        ExportScope::Conversation => match (&destination, format) {
            (ExportDestination::Clipboard, Some(format)) if format != ExportFormat::Markdown => {
                return Err(export_usage(
                    "HTML and JSON exports need a file destination",
                ));
            }
            (
                ExportDestination::File {
                    path: Some(path), ..
                },
                None,
            ) => ExportFormat::from_path(path),
            (_, format) => format.unwrap_or(ExportFormat::Markdown),
        },
    };

    Ok(ExportRequest {
        scope,
        destination,
        format,
        include_tools,
    })
}

fn export_usage(reason: &str) -> String {
    format!("{reason}. Usage: {}", CONTRACT_INFO.usage)
}

fn copy_to_clipboard(
    export: &dyn CommandSessionExportContext,
    label: &str,
    markdown: &str,
) -> CommandResult {
    let terminal_client = export.clipboard_requires_terminal_paste();
    let last_copy = export.write_recovery_copy(markdown);
    let copy_hint = |path: Option<PathBuf>| match path {
        Some(path) => {
            format!("; a copy is at {}", path.display())
        }
        None => String::new(),
    };
    match export.write_clipboard(markdown) {
        Ok(()) if terminal_client => CommandResult::message(format!(
            "{label} sent to the terminal-client clipboard over SSH via tmux/OSC 52 ({} lines){}; terminal support and settings determine whether the client accepts it",
            markdown.lines().count(),
            copy_hint(last_copy)
        )),
        Ok(()) => CommandResult::message(format!(
            "{label} copied to the local clipboard ({} lines; a terminal clipboard fallback may have been used){}",
            markdown.lines().count(),
            copy_hint(last_copy)
        )),
        Err(err) => match last_copy {
            Some(path) => CommandResult::error(format!(
                "Clipboard export failed: {err}. The full export was written to {}; /export file <path> writes it where you choose",
                path.display()
            )),
            None => CommandResult::error(format!(
                "Clipboard export failed: {err}. No file was written; use `/export file <path>` to choose an explicit destination"
            )),
        },
    }
}

/// Render the full-conversation export document from portable projection data.
///
/// Takes the projection by value so the render path can move each block's JSON
/// payload into `push_json` instead of cloning it a second time. The projection
/// itself was already copied once at the facet boundary (F3); cloning the
/// payloads again here would be an avoidable extra copy of every tool input and
/// structured result.
pub(in crate::commands) fn render_conversation(projection: ConversationExportProjection) -> String {
    let ConversationExportProjection {
        metadata,
        transcript,
        restore_points,
    } = projection;
    let names = tool_names(&transcript);
    let mut out = String::new();
    out.push_str("# Codewhale conversation export\n\n");
    let _ = writeln!(
        out,
        "- Exported: {}",
        format_export_time(metadata.exported_at_unix)
    );
    if let Some(title) = metadata.title.as_deref() {
        let _ = writeln!(out, "- Title: {}", inline_text(title));
    }
    let _ = writeln!(out, "- Session: {}", inline_text(&metadata.session_label));
    let _ = writeln!(out, "- Provider: {}", inline_text(&metadata.provider));
    let _ = writeln!(out, "- Model: {}", inline_text(&metadata.model));
    let _ = writeln!(out, "- Mode: {}", metadata.mode);
    let _ = writeln!(
        out,
        "- Workspace: {}",
        inline_text(&metadata.workspace_name)
    );
    let _ = writeln!(out, "- Messages: {}", metadata.message_count);
    let _ = write!(out, "\n> {OMISSION_NOTE}\n\n");

    render_restore_summary(&mut out, &restore_points);

    match transcript {
        TranscriptProjection::HistoryFallback(entries) => {
            render_history_fallback(&mut out, &entries)
        }
        TranscriptProjection::Authoritative(messages) => {
            for (index, message) in messages.into_iter().enumerate() {
                // Correlation is derived before `message` is consumed by the
                // renderer so the render path can take ownership of its payloads.
                let correlation = correlation_markdown(&restore_points, &message);
                render_message(&mut out, index + 1, message, &names);
                out.push_str(&correlation);
            }
        }
    }
    out
}

pub(super) const OMISSION_NOTE: &str = "Hidden instructions, internal reasoning, and reasoning signatures are omitted. Secret-like values and credential-bearing URLs are redacted as a defense in depth; review the export before sharing it.";

pub(super) fn format_export_time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Characters of the snapshot SHA shown as a restore-point id.
const RESTORE_POINT_ID_LEN: usize = 12;

fn render_restore_summary(out: &mut String, projection: &RestorePointProjection) {
    out.push_str("## Restore points\n\n");
    match projection {
        RestorePointProjection::None => {
            out.push_str(
                "No workspace restore points are recorded for this workspace, so nothing in this export can be correlated to a restorable workspace state. Snapshots may be disabled, or no turn has taken one yet.\n\n",
            );
        }
        RestorePointProjection::Unreadable { reason } => {
            let _ = writeln!(
                out,
                "Workspace restore points could not be read ({}). Treat the correlation below as unavailable rather than empty.\n",
                inline_text(reason)
            );
        }
        RestorePointProjection::Recorded { snapshots } if snapshots.is_empty() => {
            out.push_str(
                "A snapshot repository exists for this workspace but records no restore points yet.\n\n",
            );
        }
        RestorePointProjection::Recorded { snapshots } => {
            let _ = writeln!(
                out,
                "The {} most recent workspace restore points, newest first. `/restore <N>` restores by the index in this table and `/restore list` shows the live list.\n",
                snapshots.len()
            );
            out.push_str(
                "> The index is the position at export time. Every new turn records another restore point and shifts it, so re-check `/restore list` before restoring from an older export. The snapshot id does not shift.\n\n",
            );
            out.push_str("| N | Restore point | Recorded (UTC) | Label |\n");
            out.push_str("| --- | --- | --- | --- |\n");
            for (index, snapshot) in snapshots.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "| {} | `{}` | {} | {} |",
                    index + 1,
                    short_restore_id(&snapshot.id),
                    format_export_time(snapshot.timestamp_unix),
                    inline_text(&snapshot.label)
                );
            }
            out.push('\n');
        }
    }
}

/// Append the restore points correlated to a single user message.
///
/// Correlation is by the prompt snippet the host embedded in the snapshot
/// label, produced by the same function the snapshot writer uses. No
/// message-index-to-turn-sequence mapping is invented: a turn sequence and an
/// export message index are different counters, and asserting they line up
/// would be a guess presented as provenance.
fn correlation_markdown(projection: &RestorePointProjection, message: &ExportMessage) -> String {
    let mut out = String::new();
    // F6: exact `Role::User` identity, not the rendered role string. Comparing
    // textually against "user" would also match `Role::Unrecognized("user")`,
    // which the baseline deliberately did not correlate.
    if !message.is_user_role {
        return out;
    }
    let RestorePointProjection::Recorded { snapshots } = projection else {
        return out;
    };
    let Some(snippet) = message.prompt_snippet.as_deref() else {
        return out;
    };

    let matches: Vec<(usize, &RestoreSnapshot)> = snapshots
        .iter()
        .enumerate()
        .filter(|(_, snapshot)| {
            matches!(snapshot.kind.as_str(), "pre-turn" | "post-turn")
                && snapshot.prompt_snippet.as_deref() == Some(snippet)
        })
        .collect();

    if matches.is_empty() {
        out.push_str(
            "- Restore points: none recorded for this message within the listed window.\n\n",
        );
        return out;
    }

    let ambiguous = matches.len() > 1;
    let rendered: Vec<String> = matches
        .iter()
        .map(|(index, snapshot)| {
            let seq = snapshot
                .sequence
                .map(|seq| format!(" turn {seq}"))
                .unwrap_or_default();
            format!(
                "N{} `{}` ({}{})",
                index + 1,
                short_restore_id(&snapshot.id),
                snapshot.kind,
                seq
            )
        })
        .collect();
    let _ = writeln!(out, "- Restore points: {}", rendered.join(", "));
    if ambiguous {
        out.push_str(
            "  - More than one restore point carries this prompt snippet, so the match is ambiguous; compare the recorded times above before restoring.\n",
        );
    }
    out.push('\n');
    out
}

fn short_restore_id(id: &str) -> String {
    id.chars().take(RESTORE_POINT_ID_LEN).collect()
}

fn render_message(
    out: &mut String,
    index: usize,
    message: ExportMessage,
    names: &HashMap<String, String>,
) {
    let role = role_label(&message);
    let _ = writeln!(out, "## {index}. {role}\n");
    if is_internal_role(&message.role) {
        out.push_str("[internal context omitted]\n\n");
        return;
    }
    if message.blocks.is_empty() {
        out.push_str("[no content]\n\n");
        return;
    }
    let mut after_heading = false;
    for block in message.blocks {
        render_content_block(out, block, names, &mut after_heading);
    }
}

fn render_content_block(
    out: &mut String,
    block: ExportBlock,
    names: &HashMap<String, String>,
    after_heading: &mut bool,
) {
    let headed = !matches!(
        block,
        ExportBlock::Text { .. } | ExportBlock::InternalReasoning
    );
    match block {
        ExportBlock::Text { text } => {
            if std::mem::take(after_heading) {
                out.push_str("### Text\n\n");
            }
            push_sanitized_text(out, &text);
        }
        ExportBlock::ImageReference { url } => {
            out.push_str("### Image attachment\n\n");
            let _ = writeln!(
                out,
                "- Reference: {}\n",
                inline_text(&redact_url_for_display(&url))
            );
        }
        ExportBlock::ImageOmitted => {
            out.push_str("### Image attachment\n\n");
            out.push_str("- Reference omitted (inline or local image payload)\n\n");
        }
        ExportBlock::InternalReasoning => {
            out.push_str("[internal reasoning and signature omitted]\n\n");
        }
        ExportBlock::ToolCall {
            id,
            name,
            caller,
            input,
        } => {
            let _ = writeln!(out, "### Tool call: {}\n", inline_text(&name));
            let _ = writeln!(out, "- ID: {}", inline_text(&id));
            if let Some(caller) = caller {
                let _ = writeln!(out, "- Caller type: {}", inline_text(&caller.caller_type));
                if let Some(tool_id) = caller.tool_id.as_deref() {
                    let _ = writeln!(out, "- Caller tool ID: {}", inline_text(tool_id));
                }
            }
            out.push('\n');
            push_json(out, input);
        }
        ExportBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            structured,
        } => {
            match names.get(&tool_use_id) {
                Some(name) => {
                    let _ = writeln!(out, "### Tool result: {}\n", inline_text(name));
                }
                None => out.push_str("### Tool result\n\n"),
            }
            let _ = writeln!(out, "- Tool call ID: {}", inline_text(&tool_use_id));
            if is_error {
                out.push_str("- Error: true\n");
            }
            out.push('\n');
            push_fenced_text(out, &sanitize_text(&content));
            if let Some(blocks) = structured {
                out.push_str("Structured result blocks:\n\n");
                push_json(out, blocks);
            }
        }
        ExportBlock::ServerToolCall { id, name, input } => {
            let _ = writeln!(out, "### Server tool call: {}\n", inline_text(&name));
            let _ = writeln!(out, "- ID: {}\n", inline_text(&id));
            push_json(out, input);
        }
        ExportBlock::ToolSearchResult {
            tool_use_id,
            content,
        } => {
            out.push_str("### Tool-search result\n\n");
            let _ = writeln!(out, "- Tool call ID: {}\n", inline_text(&tool_use_id));
            push_json(out, content);
        }
        ExportBlock::CodeExecutionResult {
            tool_use_id,
            content,
        } => {
            out.push_str("### Code-execution result\n\n");
            let _ = writeln!(out, "- Tool call ID: {}\n", inline_text(&tool_use_id));
            push_json(out, content);
        }
    }
    if headed {
        *after_heading = true;
    }
}

fn render_history_fallback(out: &mut String, entries: &[HistoryEntry]) {
    if entries.is_empty() {
        out.push_str("## Conversation\n\n[empty conversation]\n");
        return;
    }
    out.push_str(
        "> Structured API messages were unavailable; the entries below are a sanitized visible-history fallback.\n\n",
    );
    for (index, entry) in entries.iter().enumerate() {
        let (role, body) = match entry {
            HistoryEntry::Sanitized { role, body } => (role.as_str(), sanitize_text(body)),
            HistoryEntry::Literal { role, body } => (role.as_str(), body.clone()),
        };
        let _ = writeln!(out, "## {}. {}\n", index + 1, inline_text(role));
        push_pre_sanitized_text(out, &body);
    }
}

fn push_sanitized_text(out: &mut String, text: &str) {
    push_pre_sanitized_text(out, &sanitize_text(text));
}

fn push_pre_sanitized_text(out: &mut String, text: &str) {
    if text.trim().is_empty() {
        out.push_str("[empty text]\n\n");
    } else {
        out.push_str(text.trim_end());
        out.push_str("\n\n");
    }
}

fn push_fenced_text(out: &mut String, text: &str) {
    let body = text.trim_end();
    if body.trim().is_empty() {
        out.push_str("[empty text]\n\n");
        return;
    }
    let fence = markdown_fence(body);
    let _ = writeln!(out, "{fence}text\n{body}\n{fence}\n");
}

pub(super) fn redacted_json_text(mut value: Value) -> String {
    // Redact in place: the caller hands over ownership, so there is no need to
    // clone the whole payload just to redact it (F3 follow-up).
    redact_json(&mut value, None);
    serde_json::to_string_pretty(&value)
        .unwrap_or_else(|_| "\"[structured content unavailable]\"".to_string())
}

fn push_json(out: &mut String, value: Value) {
    let json = redacted_json_text(value);
    let fence = markdown_fence(&json);
    let _ = writeln!(out, "{fence}json\n{json}\n{fence}\n");
}

fn markdown_fence(content: &str) -> String {
    let longest = content
        .split(|ch| ch != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.saturating_add(1).max(3))
}

fn sanitize_turn_handoff(projection: &TurnHandoffProjection) -> String {
    let sanitized = sanitize_text(&projection.markdown);
    if projection.workspace_path.is_empty() {
        sanitized
    } else {
        sanitized.replace(&projection.workspace_path, ".")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Minimal fake facet: every delegate is deterministic and records calls.
    struct FakeExport {
        conversation: ConversationExportProjection,
        turn: TurnHandoffProjection,
        terminal_paste: bool,
        recovery: Option<PathBuf>,
        clipboard: Result<(), String>,
        resolve: Result<PathBuf, String>,
        write: Result<(), String>,
        calls: RefCell<Vec<String>>,
    }

    impl Default for FakeExport {
        fn default() -> Self {
            Self {
                conversation: conversation_projection(vec![]),
                turn: TurnHandoffProjection {
                    markdown: String::new(),
                    workspace_path: String::new(),
                },
                terminal_paste: false,
                recovery: None,
                clipboard: Ok(()),
                resolve: Ok(PathBuf::from("/resolved/out.md")),
                write: Ok(()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandSessionExportContext for FakeExport {
        fn conversation_projection(&self) -> ConversationExportProjection {
            self.calls
                .borrow_mut()
                .push("conversation_projection".to_string());
            self.conversation.clone()
        }

        fn turn_handoff_projection(&self) -> TurnHandoffProjection {
            self.calls
                .borrow_mut()
                .push("turn_handoff_projection".to_string());
            self.turn.clone()
        }

        fn clipboard_requires_terminal_paste(&self) -> bool {
            self.calls
                .borrow_mut()
                .push("clipboard_requires_terminal_paste".to_string());
            self.terminal_paste
        }

        fn write_recovery_copy(&self, markdown: &str) -> Option<PathBuf> {
            self.calls
                .borrow_mut()
                .push(format!("write_recovery_copy({markdown})"));
            self.recovery.clone()
        }

        fn write_clipboard(&self, markdown: &str) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("write_clipboard({markdown})"));
            self.clipboard.clone()
        }

        fn resolve_export_path(&self, raw: &str) -> Result<PathBuf, String> {
            self.calls
                .borrow_mut()
                .push(format!("resolve_export_path({raw})"));
            self.resolve.clone()
        }

        fn default_export_path(&self, file_name: &str) -> Result<PathBuf, String> {
            self.calls
                .borrow_mut()
                .push(format!("default_export_path({file_name})"));
            self.resolve.clone()
        }

        fn write_export_file(
            &self,
            path: &Path,
            contents: &[u8],
            force: bool,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push(format!(
                "write_export_file({}, {}, {force})",
                path.display(),
                String::from_utf8_lossy(contents)
            ));
            self.write.clone()
        }
    }

    fn conversation_projection(messages: Vec<ExportMessage>) -> ConversationExportProjection {
        ConversationExportProjection {
            metadata: codewhale_command_contract::facets::ExportMetadata {
                session_label: "sess12345678".to_string(),
                title: None,
                provider: "deepseek".to_string(),
                model: "deepseek-v4".to_string(),
                mode: "agent".to_string(),
                workspace_name: "workspace".to_string(),
                message_count: messages.len(),
                exported_at_unix: 1_700_000_000,
            },
            transcript: TranscriptProjection::Authoritative(messages),
            restore_points: RestorePointProjection::None,
        }
    }

    fn user_message(text: &str) -> ExportMessage {
        ExportMessage {
            is_user_role: true,
            role: "user".to_string(),
            blocks: vec![ExportBlock::Text {
                text: text.to_string(),
            }],
            prompt_snippet: Some(text.to_string()),
        }
    }

    fn snapshot(
        id: &str,
        label: &str,
        timestamp: i64,
        kind: &str,
        sequence: Option<u64>,
    ) -> RestoreSnapshot {
        RestoreSnapshot {
            id: id.to_string(),
            label: label.to_string(),
            timestamp_unix: timestamp,
            kind: kind.to_string(),
            sequence,
            prompt_snippet: label
                .split_once(": ")
                .map(|(_, snippet)| snippet.to_string()),
        }
    }

    fn request(scope: ExportScope, destination: ExportDestination) -> ExportRequest {
        ExportRequest {
            scope,
            destination,
            format: ExportFormat::Markdown,
            include_tools: true,
        }
    }

    fn file_destination(path: Option<&str>, force: bool) -> ExportDestination {
        ExportDestination::File {
            path: path.map(str::to_string),
            force,
        }
    }

    #[test]
    fn parser_matrix_is_exact() {
        assert_eq!(
            parse_request(None).unwrap(),
            request(ExportScope::Conversation, ExportDestination::Clipboard)
        );
        assert_eq!(
            parse_request(Some("clipboard")).unwrap().scope,
            ExportScope::Conversation
        );
        assert_eq!(
            parse_request(Some("TURN")).unwrap().scope,
            ExportScope::Turn
        );
        assert_eq!(
            parse_request(Some("file --force reports/chat export.md")).unwrap(),
            request(
                ExportScope::Conversation,
                file_destination(Some("reports/chat export.md"), true)
            )
        );
        assert_eq!(
            parse_request(Some("legacy export.md")).unwrap(),
            request(
                ExportScope::Conversation,
                file_destination(Some("legacy export.md"), false)
            )
        );
        assert_eq!(
            parse_request(Some("turn file --force handoff.md")).unwrap(),
            request(
                ExportScope::Turn,
                file_destination(Some("handoff.md"), true)
            )
        );
        assert_eq!(
            parse_request(Some("turn legacy.md")).unwrap(),
            request(
                ExportScope::Turn,
                file_destination(Some("legacy.md"), false)
            )
        );
        assert_eq!(
            parse_request(Some("turn clipboard")).unwrap().destination,
            ExportDestination::Clipboard
        );
        assert_eq!(
            parse_request(Some("file")).unwrap().destination,
            file_destination(None, false)
        );

        for arg in ["clipboard extra.md", "turn clipboard extra.md"] {
            assert!(parse_request(Some(arg)).is_err(), "{arg}");
        }
        assert_eq!(
            parse_request(Some("clipboard extra.md")).unwrap_err(),
            export_usage("clipboard does not accept a path")
        );
    }

    #[test]
    fn missing_authority_fails_safely_without_effects() {
        let result = export_contextual(CommandContexts::empty(), Some("clipboard"));
        assert!(result.is_error);
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Command capability unavailable: session_export")
        );
    }

    #[test]
    fn conversation_clipboard_renders_full_document_and_sequences_operations() {
        let fake = FakeExport {
            conversation: conversation_projection(vec![
                ExportMessage {
                    is_user_role: false,
                    role: "system".to_string(),
                    blocks: vec![ExportBlock::Text {
                        text: "hidden policy must never export".to_string(),
                    }],
                    prompt_snippet: None,
                },
                user_message("Please inspect this"),
            ]),
            recovery: Some(PathBuf::from("/home/.codewhale/exports/last-copy.md")),
            ..FakeExport::default()
        };

        let result = export_portable(&fake, Some("clipboard"));

        assert!(!result.is_error, "{:?}", result.message);
        let message = result.message.as_deref().unwrap_or_default();
        assert!(
            message.starts_with("Conversation copied to the local clipboard ("),
            "{message}"
        );
        assert!(
            message.contains(" lines; a terminal clipboard fallback may have been used)"),
            "{message}"
        );
        assert!(
            message.ends_with("; a copy is at /home/.codewhale/exports/last-copy.md"),
            "{message}"
        );
        let read = fake.calls.borrow();
        assert_eq!(read[0], "conversation_projection");
        assert_eq!(read[1], "clipboard_requires_terminal_paste");
        assert!(read[2].starts_with("write_recovery_copy("), "{read:?}");
        assert!(read[3].starts_with("write_clipboard("), "{read:?}");
        assert_eq!(read.len(), 4, "exactly one recovery and one clipboard call");
        let recovery_payload = read[2]
            .trim_start_matches("write_recovery_copy(")
            .trim_end_matches(')');
        let markdown = read[3]
            .trim_start_matches("write_clipboard(")
            .trim_end_matches(')');
        assert_eq!(
            recovery_payload, markdown,
            "both writes receive identical Markdown"
        );
        assert!(markdown.starts_with("# Codewhale conversation export\n\n"));
        assert!(markdown.contains("## 1. system\n\n[internal context omitted]\n\n"));
        assert!(markdown.contains("## 2. user\n\nPlease inspect this\n\n"));
        assert!(!markdown.contains("hidden policy must never export"));
    }

    #[test]
    fn conversation_document_matches_full_golden_equality() {
        let projection = conversation_projection(vec![
            ExportMessage {
                is_user_role: false,
                role: "system".to_string(),
                blocks: vec![ExportBlock::Text {
                    text: "hidden policy must never export".to_string(),
                }],
                prompt_snippet: None,
            },
            user_message("Please inspect this"),
        ]);

        let expected = "# Codewhale conversation export\n\n\
- Exported: 2023-11-14T22:13:20Z\n\
- Session: sess12345678\n\
- Provider: deepseek\n\
- Model: deepseek-v4\n\
- Mode: agent\n\
- Workspace: workspace\n\
- Messages: 2\n\n\
> Hidden instructions, internal reasoning, and reasoning signatures are omitted. Secret-like values and credential-bearing URLs are redacted as a defense in depth; review the export before sharing it.\n\n\
## Restore points\n\n\
No workspace restore points are recorded for this workspace, so nothing in this export can be correlated to a restorable workspace state. Snapshots may be disabled, or no turn has taken one yet.\n\n\
## 1. system\n\n[internal context omitted]\n\n\
## 2. user\n\nPlease inspect this\n\n";

        assert_eq!(render_conversation(projection), expected);
    }

    #[test]
    fn ssh_clipboard_uses_terminal_client_wording() {
        let fake = FakeExport {
            terminal_paste: true,
            conversation: conversation_projection(vec![user_message("hi")]),
            ..FakeExport::default()
        };

        let result = export_portable(&fake, Some("clipboard"));

        assert!(!result.is_error);
        let message = result.message.as_deref().unwrap_or_default();
        assert!(
            message.contains("terminal-client clipboard over SSH via tmux/OSC 52"),
            "{message}"
        );
        assert!(
            !message.contains("a copy is at"),
            "no recovery path present: {message}"
        );
    }

    #[test]
    fn recovery_failure_still_attempts_clipboard() {
        let fake = FakeExport {
            conversation: conversation_projection(vec![user_message("hi")]),
            recovery: None,
            clipboard: Err("no clipboard".to_string()),
            ..FakeExport::default()
        };

        let result = export_portable(&fake, Some("clipboard"));

        assert!(result.is_error);
        assert_eq!(
            result.message.as_deref(),
            Some(
                "Error: Clipboard export failed: no clipboard. No file was written; use `/export file <path>` to choose an explicit destination"
            )
        );
        let read = fake.calls.borrow();
        assert!(
            read.iter()
                .any(|call| call.starts_with("write_recovery_copy("))
        );
        assert!(read.iter().any(|call| call.starts_with("write_clipboard(")));
    }

    #[test]
    fn file_export_renders_resolves_then_writes_once() {
        let fake = FakeExport {
            conversation: conversation_projection(vec![user_message("first export")]),
            resolve: Ok(PathBuf::from("/workspace/transcript.md")),
            write: Ok(()),
            ..FakeExport::default()
        };

        let lines = render_conversation(fake.conversation.clone())
            .lines()
            .count();

        let result = export_portable(&fake, Some("file transcript.md"));

        assert!(!result.is_error, "{:?}", result.message);
        assert_eq!(
            result.message.as_deref(),
            Some(
                format!(
                    "Conversation exported to /workspace/transcript.md (Markdown, {lines} lines)"
                )
                .as_str()
            )
        );
        let read = fake.calls.borrow();
        assert_eq!(read.len(), 3, "{read:?}");
        assert_eq!(read[0], "conversation_projection");
        assert_eq!(read[1], "resolve_export_path(transcript.md)");
        assert!(read[2].starts_with("write_export_file(/workspace/transcript.md,"));
        assert!(
            !read
                .iter()
                .any(|call| call.contains("clipboard") || call.contains("recovery")),
            "file export must not touch clipboard or recovery: {read:?}"
        );
    }

    #[test]
    fn resolution_failure_prevents_writing() {
        let fake = FakeExport {
            conversation: conversation_projection(vec![user_message("x")]),
            resolve: Err("export paths may not contain `..`".to_string()),
            ..FakeExport::default()
        };

        let result = export_portable(&fake, Some("file ../escape.md"));

        assert!(result.is_error);
        assert_eq!(
            result.message.as_deref(),
            Some("Error: export paths may not contain `..`")
        );
        let read = fake.calls.borrow();
        assert!(
            !read
                .iter()
                .any(|call| call.starts_with("write_export_file(")),
            "no write after resolution failure: {read:?}"
        );
    }

    #[test]
    fn write_failure_wraps_exact_baseline_text() {
        let fake = FakeExport {
            conversation: conversation_projection(vec![user_message("x")]),
            resolve: Ok(PathBuf::from("/workspace/out.md")),
            write: Err("destination already exists: /workspace/out.md".to_string()),
            ..FakeExport::default()
        };

        let result = export_portable(&fake, Some("file out.md"));

        assert!(result.is_error);
        assert_eq!(
            result.message.as_deref(),
            Some(
                "Error: Failed to export Conversation to /workspace/out.md: destination already exists: /workspace/out.md"
            )
        );
    }

    #[test]
    fn forced_file_export_reports_overwrite_suffix() {
        let fake = FakeExport {
            conversation: conversation_projection(vec![user_message("x")]),
            resolve: Ok(PathBuf::from("/workspace/out.md")),
            ..FakeExport::default()
        };

        let lines = render_conversation(fake.conversation.clone())
            .lines()
            .count();

        let result = export_portable(&fake, Some("file --force out.md"));

        assert_eq!(
            result.message.as_deref(),
            Some(
                format!(
                    "Conversation exported to /workspace/out.md (Markdown, {lines} lines) (overwrite explicitly allowed)"
                )
                .as_str()
            )
        );
        let read = fake.calls.borrow();
        assert!(read[2].ends_with(", true)"), "{:?}", read[2]);
    }

    #[test]
    fn turn_export_sanitizes_then_replaces_nonempty_workspace() {
        let fake = FakeExport {
            turn: TurnHandoffProjection {
                markdown: "# Turn handoff\n\n\u{1b}[31m/Users/me/repo\u{1b}[0m done".to_string(),
                workspace_path: "/Users/me/repo".to_string(),
            },
            ..FakeExport::default()
        };

        let result = export_portable(&fake, Some("turn"));

        assert!(!result.is_error);
        let read = fake.calls.borrow();
        let markdown = read
            .iter()
            .find_map(|call| {
                call.strip_prefix("write_clipboard(")
                    .map(|c| c.trim_end_matches(')'))
            })
            .expect("clipboard payload");
        assert_eq!(markdown, "# Turn handoff\n\n. done");
        assert!(
            !read
                .iter()
                .any(|call| call.starts_with("conversation_projection")),
            "turn-only export must not read conversation snapshots: {read:?}"
        );
    }

    #[test]
    fn turn_export_with_empty_workspace_path_skips_replacement() {
        let fake = FakeExport {
            turn: TurnHandoffProjection {
                markdown: "path stays".to_string(),
                workspace_path: String::new(),
            },
            ..FakeExport::default()
        };
        assert_eq!(sanitize_turn_handoff(&fake.turn), "path stays");
    }

    #[test]
    fn restore_summary_distinguishes_every_state() {
        let mut none = String::new();
        render_restore_summary(&mut none, &RestorePointProjection::None);
        assert!(
            none.contains("No workspace restore points are recorded"),
            "{none}"
        );

        let mut unreadable = String::new();
        render_restore_summary(
            &mut unreadable,
            &RestorePointProjection::Unreadable {
                reason: "permission denied".to_string(),
            },
        );
        assert!(unreadable.contains("could not be read"), "{unreadable}");
        assert!(
            unreadable.contains("unavailable rather than empty"),
            "{unreadable}"
        );

        let mut empty = String::new();
        render_restore_summary(
            &mut empty,
            &RestorePointProjection::Recorded {
                snapshots: Vec::new(),
            },
        );
        assert!(empty.contains("records no restore points yet"), "{empty}");

        let mut recorded = String::new();
        render_restore_summary(
            &mut recorded,
            &RestorePointProjection::Recorded {
                snapshots: vec![
                    snapshot(
                        &"a".repeat(40),
                        "pre-turn:2: second prompt",
                        1_700_000_100,
                        "pre-turn",
                        Some(2),
                    ),
                    snapshot(
                        &"b".repeat(40),
                        "pre-turn:1: first prompt",
                        1_700_000_000,
                        "pre-turn",
                        Some(1),
                    ),
                ],
            },
        );
        assert!(
            recorded.contains(
                "| 1 | `aaaaaaaaaaaa` | 2023-11-14T22:15:00Z | pre-turn:2: second prompt |"
            ),
            "{recorded}"
        );
        assert!(
            recorded.contains(
                "| 2 | `bbbbbbbbbbbb` | 2023-11-14T22:13:20Z | pre-turn:1: first prompt |"
            ),
            "{recorded}"
        );
    }

    #[test]
    fn correlation_matches_ambiguity_absence_and_role_rules() {
        let recorded = RestorePointProjection::Recorded {
            snapshots: vec![
                snapshot(
                    &"e".repeat(40),
                    "pre-turn:9: run the tests",
                    1_700_000_300,
                    "pre-turn",
                    Some(9),
                ),
                snapshot(
                    &"f".repeat(40),
                    "pre-turn:5: run the tests",
                    1_700_000_100,
                    "pre-turn",
                    Some(5),
                ),
                snapshot(
                    &"2".repeat(40),
                    "tool:call_abc: rename the widget",
                    1_700_000_000,
                    "tool",
                    None,
                ),
            ],
        };

        let ambiguous = correlation_markdown(&recorded, &user_message("run the tests"));
        assert!(
            ambiguous.contains("N1 `eeeeeeeeeeee` (pre-turn turn 9)"),
            "{ambiguous}"
        );
        assert!(
            ambiguous.contains("N2 `ffffffffffff` (pre-turn turn 5)"),
            "{ambiguous}"
        );
        assert!(ambiguous.contains("ambiguous"), "{ambiguous}");

        let none = correlation_markdown(&recorded, &user_message("never snapshotted"));
        assert!(none.contains("none recorded for this message"), "{none}");

        let tool_only = correlation_markdown(&recorded, &user_message("rename the widget"));
        assert!(
            tool_only.contains("none recorded for this message"),
            "{tool_only}"
        );

        let assistant = correlation_markdown(
            &recorded,
            &ExportMessage {
                is_user_role: false,
                role: "assistant".to_string(),
                blocks: vec![ExportBlock::Text {
                    text: "run the tests".to_string(),
                }],
                prompt_snippet: Some("run the tests".to_string()),
            },
        );
        assert!(assistant.is_empty(), "{assistant}");
    }

    #[test]
    fn correlation_requires_exact_user_identity_not_the_role_string() {
        let recorded = RestorePointProjection::Recorded {
            snapshots: vec![snapshot(
                "aaaaaaaaaaaa",
                "pre-turn:4: run the tests",
                5,
                "pre-turn",
                Some(4),
            )],
        };
        // Same rendered role string as a real user turn, but not `Role::User`.
        let unrecognized = ExportMessage {
            role: "user".to_string(),
            is_user_role: false,
            blocks: vec![ExportBlock::Text {
                text: "run the tests".to_string(),
            }],
            prompt_snippet: Some("run the tests".to_string()),
        };
        assert!(
            correlation_markdown(&recorded, &unrecognized).is_empty(),
            "a role that only renders as \"user\" must not correlate"
        );

        let real_user = ExportMessage {
            is_user_role: true,
            ..unrecognized
        };
        assert!(
            correlation_markdown(&recorded, &real_user).contains("N1 `aaaaaaaaaaaa`"),
            "an exact user turn still correlates"
        );
    }

    #[test]
    fn history_fallback_marks_literals_and_sanitizes_visible_bodies() {
        let mut out = String::new();
        render_history_fallback(
            &mut out,
            &[
                HistoryEntry::Sanitized {
                    role: "user".to_string(),
                    body: "hello\u{1b}[31m world".to_string(),
                },
                HistoryEntry::Literal {
                    role: "system".to_string(),
                    body: "[internal context omitted]".to_string(),
                },
            ],
        );
        assert!(out.contains("## 1. user\n\nhello world\n\n"), "{out}");
        assert!(
            out.contains("## 2. system\n\n[internal context omitted]\n\n"),
            "{out}"
        );

        let mut empty = String::new();
        render_history_fallback(&mut empty, &[]);
        assert_eq!(empty, "## Conversation\n\n[empty conversation]\n");
    }

    #[test]
    fn json_fence_tracks_longest_backtick_run_and_redacts_secrets() {
        let mut out = String::new();
        push_json(
            &mut out,
            serde_json::json!({"api_key": "literal-secret", "note": "``` inner"}),
        );
        assert!(!out.contains("literal-secret"), "{out}");
        assert!(out.starts_with("````json\n"), "{out}");
        assert!(out.contains("\"api_key\": \"[redacted]\""), "{out}");
    }

    #[test]
    fn empty_content_and_missing_blocks_use_baseline_markers() {
        let names = HashMap::new();
        let mut out = String::new();
        render_message(
            &mut out,
            1,
            ExportMessage {
                is_user_role: false,
                role: "assistant".to_string(),
                blocks: Vec::new(),
                prompt_snippet: None,
            },
            &names,
        );
        assert!(out.contains("## 1. assistant\n\n[no content]\n\n"), "{out}");

        let mut empty_text = String::new();
        render_content_block(
            &mut empty_text,
            ExportBlock::Text {
                text: "   ".to_string(),
            },
            &names,
            &mut false,
        );
        assert!(empty_text.ends_with("[empty text]\n\n"), "{empty_text}");
    }

    #[test]
    fn parser_handles_whitespace_case_and_flag_positions() {
        assert_eq!(
            parse_request(Some("  clipboard  ")).unwrap(),
            request(ExportScope::Conversation, ExportDestination::Clipboard)
        );
        assert_eq!(
            parse_request(Some("  TURN  ")).unwrap(),
            request(ExportScope::Turn, ExportDestination::Clipboard)
        );
        assert_eq!(
            parse_request(Some("  File   Report One.md  ")).unwrap(),
            request(
                ExportScope::Conversation,
                file_destination(Some("Report One.md"), false)
            )
        );
        assert_eq!(
            parse_request(Some("turn FILE --Force handoff.md")).unwrap(),
            request(
                ExportScope::Turn,
                file_destination(Some("handoff.md"), true)
            )
        );
        assert_eq!(
            parse_request(Some("file out.md --force")).unwrap(),
            request(
                ExportScope::Conversation,
                file_destination(Some("out.md"), true)
            )
        );
        assert_eq!(
            parse_request(Some("turn file")).unwrap().destination,
            file_destination(None, false)
        );
        assert_eq!(
            parse_request(Some("turn file --force"))
                .unwrap()
                .destination,
            file_destination(None, true)
        );
        assert_eq!(
            parse_request(Some("turn clipboard extra.md")).unwrap_err(),
            export_usage("clipboard does not accept a path")
        );
    }

    #[test]
    fn header_metadata_keeps_unsaved_and_workspace_fallbacks() {
        let projection = ConversationExportProjection {
            metadata: codewhale_command_contract::facets::ExportMetadata {
                session_label: "unsaved".to_string(),
                title: None,
                provider: "unknown".to_string(),
                model: "unknown".to_string(),
                mode: "agent".to_string(),
                workspace_name: "workspace".to_string(),
                message_count: 0,
                exported_at_unix: 0,
            },
            transcript: TranscriptProjection::Authoritative(Vec::new()),
            restore_points: RestorePointProjection::None,
        };

        let rendered = render_conversation(projection);

        assert!(
            rendered.contains("- Exported: 1970-01-01T00:00:00Z\n"),
            "{rendered}"
        );
        assert!(rendered.contains("- Session: unsaved\n"), "{rendered}");
        assert!(rendered.contains("- Provider: unknown\n"), "{rendered}");
        assert!(rendered.contains("- Model: unknown\n"), "{rendered}");
        assert!(rendered.contains("- Workspace: workspace\n"), "{rendered}");
        assert!(rendered.contains("- Messages: 0\n"), "{rendered}");
    }

    #[test]
    fn every_content_block_variant_renders_exactly() {
        let names = HashMap::from([("call-1".to_string(), "fetch_url".to_string())]);

        // Image reference: URL credentials and sensitive query values are masked.
        let mut image = String::new();
        render_content_block(
            &mut image,
            ExportBlock::ImageReference {
                url: "https://alice:pw@example.com/a.png?api_key=hidden&ok=1".to_string(),
            },
            &names,
            &mut false,
        );
        assert_eq!(
            image,
            "### Image attachment\n\n- Reference: https://***:***@example.com/a.png?api_key=***&ok=1\n\n"
        );

        // Inline/local image payloads became an omission marker at projection.
        let mut omitted = String::new();
        render_content_block(&mut omitted, ExportBlock::ImageOmitted, &names, &mut false);
        assert_eq!(
            omitted,
            "### Image attachment\n\n- Reference omitted (inline or local image payload)\n\n"
        );

        // Reasoning bodies and signatures are replaced by the baseline marker.
        let mut reasoning = String::new();
        render_content_block(
            &mut reasoning,
            ExportBlock::InternalReasoning,
            &names,
            &mut false,
        );
        assert_eq!(reasoning, "[internal reasoning and signature omitted]\n\n");

        // Tool call with caller metadata and redacted JSON input.
        let mut tool_call = String::new();
        render_content_block(
            &mut tool_call,
            ExportBlock::ToolCall {
                id: "call-1".to_string(),
                name: "fetch_url".to_string(),
                caller: Some(codewhale_command_contract::facets::ToolCallerProjection {
                    caller_type: "code_execution_20250825".to_string(),
                    tool_id: Some("server-tool-1".to_string()),
                }),
                input: serde_json::json!({"api_key": "literal-secret"}),
            },
            &names,
            &mut false,
        );
        assert_eq!(
            tool_call,
            "### Tool call: fetch_url\n\n- ID: call-1\n- Caller type: code_execution_20250825\n- Caller tool ID: server-tool-1\n\n```json\n{\n  \"api_key\": \"[redacted]\"\n}\n```\n\n"
        );

        // Tool call without caller metadata omits only the caller lines.
        let mut bare_tool_call = String::new();
        render_content_block(
            &mut bare_tool_call,
            ExportBlock::ToolCall {
                id: "call-2".to_string(),
                name: "read_file".to_string(),
                caller: None,
                input: serde_json::json!({}),
            },
            &names,
            &mut false,
        );
        assert_eq!(
            bare_tool_call,
            "### Tool call: read_file\n\n- ID: call-2\n\n```json\n{}\n```\n\n"
        );

        // Tool result with structured blocks: both the sanitized result text
        // and the redacted structured payload are rendered.
        let mut structured_result = String::new();
        render_content_block(
            &mut structured_result,
            ExportBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "tool output line".to_string(),
                is_error: false,
                structured: Some(serde_json::json!([{"session_token": "session-secret"}])),
            },
            &names,
            &mut false,
        );
        assert_eq!(
            structured_result,
            "### Tool result: fetch_url\n\n- Tool call ID: call-1\n\n```text\ntool output line\n```\n\nStructured result blocks:\n\n```json\n[\n  {\n    \"session_token\": \"[redacted]\"\n  }\n]\n```\n\n"
        );

        // Tool result without structured blocks ends after the result text, and
        // an empty body keeps the baseline empty marker.
        let mut plain_result = String::new();
        render_content_block(
            &mut plain_result,
            ExportBlock::ToolResult {
                tool_use_id: "call-2".to_string(),
                content: String::new(),
                is_error: true,
                structured: None,
            },
            &names,
            &mut false,
        );
        assert_eq!(
            plain_result,
            "### Tool result\n\n- Tool call ID: call-2\n- Error: true\n\n[empty text]\n\n"
        );

        let mut server_call = String::new();
        render_content_block(
            &mut server_call,
            ExportBlock::ServerToolCall {
                id: "srv-1".to_string(),
                name: "web_search".to_string(),
                input: serde_json::json!({"query": "rust"}),
            },
            &names,
            &mut false,
        );
        assert_eq!(
            server_call,
            "### Server tool call: web_search\n\n- ID: srv-1\n\n```json\n{\n  \"query\": \"rust\"\n}\n```\n\n"
        );

        let mut search_result = String::new();
        render_content_block(
            &mut search_result,
            ExportBlock::ToolSearchResult {
                tool_use_id: "search-1".to_string(),
                content: serde_json::json!({"results": []}),
            },
            &names,
            &mut false,
        );
        assert_eq!(
            search_result,
            "### Tool-search result\n\n- Tool call ID: search-1\n\n```json\n{\n  \"results\": []\n}\n```\n\n"
        );

        let mut execution_result = String::new();
        render_content_block(
            &mut execution_result,
            ExportBlock::CodeExecutionResult {
                tool_use_id: "exec-1".to_string(),
                content: serde_json::json!({"stdout": "ok"}),
            },
            &names,
            &mut false,
        );
        assert_eq!(
            execution_result,
            "### Code-execution result\n\n- Tool call ID: exec-1\n\n```json\n{\n  \"stdout\": \"ok\"\n}\n```\n\n"
        );
    }
}
