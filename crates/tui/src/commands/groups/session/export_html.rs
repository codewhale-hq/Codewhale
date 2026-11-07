//! Self-contained HTML rendering of a conversation export.
//!
//! The page is built from the same sanitized projection as the Markdown
//! document and carries no script and no external resource; its
//! content-security-policy forbids both. Message text is shown verbatim rather
//! than rendered as Markdown, tool calls and results sit in collapsed
//! sections, and the local restore-point table is omitted because snapshot ids
//! mean nothing to a reader of the file.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;

use codewhale_command_contract::facets::{
    ConversationExportProjection, ExportBlock, ExportMessage, HistoryEntry, TranscriptProjection,
};
use codewhale_sanitize::sanitize::{
    inline_text, is_internal_role, redact_url_for_display, sanitize_text,
};

use super::export::{
    OMISSION_NOTE, format_export_time, redacted_json_text, role_label, tool_names,
};

const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'";

const STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#fff;--fg:#1f2328;--muted:#656d76;--line:#d0d7de;--card:#f6f8fa;--user:#ddf4ff;--tool:#fff8c5}
@media (prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--muted:#8d96a0;--line:#30363d;--card:#161b22;--user:#0c2d6b;--tool:#3b2e00}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif}
main{max-width:860px;margin:0 auto;padding:2rem 1rem 4rem}
h1{font-size:1.6rem;margin:0 0 .75rem;overflow-wrap:anywhere}
dl{display:grid;grid-template-columns:max-content 1fr;gap:.2rem 1rem;margin:0 0 1rem;color:var(--muted);font-size:.9rem}
dt{font-weight:600}
dd{margin:0;overflow-wrap:anywhere}
.note{border-left:3px solid var(--line);padding:.25rem .75rem;color:var(--muted);font-size:.9rem;margin:0 0 2rem}
.message{border:1px solid var(--line);border-radius:8px;margin:0 0 1rem;overflow:hidden}
.message>h2{margin:0;padding:.4rem .8rem;font-size:.8rem;text-transform:uppercase;letter-spacing:.05em;background:var(--card);border-bottom:1px solid var(--line)}
.message.user>h2{background:var(--user)}
.message.tool>h2{background:var(--tool)}
.body{padding:.6rem .8rem}
.text{white-space:pre-wrap;overflow-wrap:anywhere}
.omitted{margin:.25rem 0;color:var(--muted);font-style:italic}
.meta{margin:.25rem 0;color:var(--muted);font-size:.8rem}
details{border:1px solid var(--line);border-radius:6px;margin:.5rem 0;background:var(--card)}
summary{cursor:pointer;padding:.35rem .7rem;font-size:.9rem}
.inner{padding:0 .7rem .6rem}
pre{margin:.25rem 0;padding:.6rem;border:1px solid var(--line);border-radius:6px;background:var(--bg);overflow-x:auto;white-space:pre-wrap;overflow-wrap:anywhere;font:12.5px/1.45 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
footer{margin-top:2rem;padding-top:1rem;border-top:1px solid var(--line);color:var(--muted);font-size:.8rem}
"#;

pub(in crate::commands) fn render(projection: ConversationExportProjection) -> String {
    let ConversationExportProjection {
        metadata,
        transcript,
        restore_points: _,
    } = projection;
    let names = tool_names(&transcript);
    let heading = metadata
        .title
        .as_deref()
        .map(inline_text)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Codewhale conversation".to_string());
    let version = env!("CARGO_PKG_VERSION");

    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{CONTENT_SECURITY_POLICY}\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"generator\" content=\"Codewhale {version}\">\n\
         <title>{}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n<main>\n<header>\n<h1>{}</h1>\n<dl>\n",
        escape_html(&heading),
        escape_html(&heading),
    );
    for (name, value) in [
        ("Exported", format_export_time(metadata.exported_at_unix)),
        ("Session", inline_text(&metadata.session_label)),
        ("Provider", inline_text(&metadata.provider)),
        ("Model", inline_text(&metadata.model)),
        ("Mode", inline_text(&metadata.mode)),
        ("Workspace", inline_text(&metadata.workspace_name)),
        ("Messages", metadata.message_count.to_string()),
    ] {
        let _ = writeln!(out, "<dt>{name}</dt><dd>{}</dd>", escape_html(&value));
    }
    let _ = writeln!(
        out,
        "</dl>\n<p class=\"note\">{OMISSION_NOTE}</p>\n</header>"
    );

    match transcript {
        TranscriptProjection::HistoryFallback(entries) => render_history(&mut out, &entries),
        TranscriptProjection::Authoritative(messages) => {
            for (index, message) in messages.into_iter().enumerate() {
                render_message(&mut out, index + 1, message, &names);
            }
        }
    }

    let _ = writeln!(
        out,
        "<footer>Generated by Codewhale {version}</footer>\n</main>\n</body>\n</html>"
    );
    out
}

fn render_history(out: &mut String, entries: &[HistoryEntry]) {
    if entries.is_empty() {
        out.push_str("<p class=\"omitted\">[empty conversation]</p>\n");
        return;
    }
    out.push_str(
        "<p class=\"note\">Structured API messages were unavailable; the entries below are a sanitized visible-history fallback.</p>\n",
    );
    for (index, entry) in entries.iter().enumerate() {
        let (role, body) = match entry {
            HistoryEntry::Sanitized { role, body } => (role.as_str(), sanitize_text(body)),
            HistoryEntry::Literal { role, body } => (role.as_str(), body.clone()),
        };
        open_section(out, index + 1, &inline_text(role));
        push_text(out, &body);
        out.push_str("</div>\n</section>\n");
    }
}

fn open_section(out: &mut String, index: usize, role: &str) {
    let class = match role {
        "user" => "user",
        "tool" => "tool",
        "assistant" => "assistant",
        _ => "other",
    };
    let _ = writeln!(
        out,
        "<section class=\"message {class}\" id=\"m{index}\">\n<h2>{index}. {}</h2>\n<div class=\"body\">",
        escape_html(role)
    );
}

fn render_message(
    out: &mut String,
    index: usize,
    message: ExportMessage,
    names: &HashMap<String, String>,
) {
    open_section(out, index, &role_label(&message));
    if is_internal_role(&message.role) {
        out.push_str("<p class=\"omitted\">[internal context omitted]</p>\n");
    } else if message.blocks.is_empty() {
        out.push_str("<p class=\"omitted\">[no content]</p>\n");
    } else {
        for block in message.blocks {
            render_block(out, block, names);
        }
    }
    out.push_str("</div>\n</section>\n");
}

fn render_block(out: &mut String, block: ExportBlock, names: &HashMap<String, String>) {
    match block {
        ExportBlock::Text { text } => push_text(out, &sanitize_text(&text)),
        ExportBlock::ImageReference { url } => {
            let _ = writeln!(
                out,
                "<p class=\"meta\">Image attachment · {}</p>",
                escape_html(&inline_text(&redact_url_for_display(&url)))
            );
        }
        ExportBlock::ImageOmitted => out.push_str(
            "<p class=\"meta\">Image attachment · reference omitted (inline or local image payload)</p>\n",
        ),
        ExportBlock::InternalReasoning => {
            out.push_str("<p class=\"omitted\">[internal reasoning and signature omitted]</p>\n");
        }
        ExportBlock::ToolCall {
            id,
            name,
            caller,
            input,
        } => {
            let mut meta = format!("ID {}", inline_text(&id));
            if let Some(caller) = caller {
                let _ = write!(meta, " · caller {}", inline_text(&caller.caller_type));
                if let Some(tool_id) = caller.tool_id.as_deref() {
                    let _ = write!(meta, " ({})", inline_text(tool_id));
                }
            }
            push_details(
                out,
                &format!("Tool call · {}", inline_text(&name)),
                &(caption(&meta) + &pre(&redacted_json_text(input))),
            );
        }
        ExportBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            structured,
        } => {
            let mut summary = "Tool result".to_string();
            if let Some(name) = names.get(&tool_use_id) {
                let _ = write!(summary, " · {}", inline_text(name));
            }
            if is_error {
                summary.push_str(" · error");
            }
            let mut inner = caption(&format!("Tool call ID {}", inline_text(&tool_use_id)));
            inner.push_str(&pre(&sanitize_text(&content)));
            if let Some(blocks) = structured {
                inner.push_str(&caption("Structured result blocks"));
                inner.push_str(&pre(&redacted_json_text(blocks)));
            }
            push_details(out, &summary, &inner);
        }
        ExportBlock::ServerToolCall { id, name, input } => push_details(
            out,
            &format!("Server tool call · {}", inline_text(&name)),
            &(caption(&format!("ID {}", inline_text(&id))) + &pre(&redacted_json_text(input))),
        ),
        ExportBlock::ToolSearchResult {
            tool_use_id,
            content,
        } => push_details(
            out,
            "Tool-search result",
            &(caption(&format!("Tool call ID {}", inline_text(&tool_use_id)))
                + &pre(&redacted_json_text(content))),
        ),
        ExportBlock::CodeExecutionResult {
            tool_use_id,
            content,
        } => push_details(
            out,
            "Code-execution result",
            &(caption(&format!("Tool call ID {}", inline_text(&tool_use_id)))
                + &pre(&redacted_json_text(content))),
        ),
    }
}

fn push_text(out: &mut String, text: &str) {
    let text = text.trim_end();
    if text.trim().is_empty() {
        out.push_str("<p class=\"omitted\">[empty text]</p>\n");
    } else {
        let _ = writeln!(out, "<div class=\"text\">{}</div>", escape_html(text));
    }
}

fn caption(text: &str) -> String {
    format!("<p class=\"meta\">{}</p>\n", escape_html(text))
}

fn pre(text: &str) -> String {
    format!("<pre>{}</pre>\n", escape_html(text))
}

fn push_details(out: &mut String, summary: &str, inner: &str) {
    let _ = writeln!(
        out,
        "<details>\n<summary>{}</summary>\n<div class=\"inner\">\n{inner}</div>\n</details>",
        escape_html(summary)
    );
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
