//! /share command — publish a redacted copy of the session as a secret gist.
//!
//! The page is rendered through the same projection and renderer as
//! `/export`, so hidden instructions, internal reasoning, and reasoning
//! signatures are dropped and secret-like values are redacted before anything
//! leaves the machine. The raw `api_messages` JSON never reaches the page.
//!
//! # Usage
//!
//! - `/share` — preview only: what would be uploaded; no network call
//! - `/share confirm` — upload as a secret gist (unlisted, not private)
//! - `/share help` — show usage

use std::io::Write;
use std::path::Path;

use codewhale_command_contract::facets::{
    CommandSessionExportContext, ConversationExportProjection, ExportBlock, RestorePointProjection,
    TranscriptProjection,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;
use crate::commands::groups::session::export::render_conversation;
use crate::tui::app::AppAction;
use codewhale_dependencies::ExternalTool;

const VISIBILITY_NOTE: &str =
    "a secret GitHub gist (unlisted, not private: anyone with the link can read it)";

/// Preview or share the current session.
fn share(export: &dyn CommandSessionExportContext, arg: Option<&str>) -> CommandResult {
    let raw = arg.map(str::trim).unwrap_or("");

    match raw {
        "" => preview(export),
        "confirm" => confirm(export),
        "help" | "--help" | "-h" => CommandResult::message(format!(
            "/share — Share a redacted copy of this session.\n\
             \n\
             Usage:\n\
             /share          Preview what would be uploaded (nothing leaves the machine)\n\
             /share confirm  Upload it as {VISIBILITY_NOTE}\n\
             \n\
             The page uses the same redacted rendering as /export: hidden\n\
             instructions and reasoning are omitted and secret-like values are\n\
             masked. Uploading needs the `gh` CLI, installed and signed in."
        )),
        _ => CommandResult::error(format!(
            "Unknown /share argument `{raw}`. Use `/share`, `/share confirm`, or `/share help`."
        )),
    }
}

/// What `/share` would publish, measured on the exact page it would upload.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SharePlan {
    markdown: String,
    model: String,
    mode: String,
    message_count: usize,
    /// `None` when the page is the visible-history fallback, which may carry
    /// tool output it cannot count.
    tool_results: Option<usize>,
    redactions: usize,
}

/// Project the conversation the way `/export` does, minus the local
/// restore-point table (snapshot ids mean nothing to a reader of the link).
fn plan(export: &dyn CommandSessionExportContext) -> Option<SharePlan> {
    let mut projection: ConversationExportProjection = export.conversation_projection();
    if projection.metadata.message_count == 0 {
        return None;
    }
    projection.restore_points = RestorePointProjection::None;
    // The visible-history fallback cannot tell tool output apart from the
    // rest, so it is not counted as "none".
    let tool_results = match &projection.transcript {
        TranscriptProjection::Authoritative(messages) => Some(
            messages
                .iter()
                .flat_map(|message| &message.blocks)
                .filter(|block| matches!(block, ExportBlock::ToolResult { .. }))
                .count(),
        ),
        TranscriptProjection::HistoryFallback(_) => None,
    };
    let model = projection.metadata.model.clone();
    let mode = projection.metadata.mode.clone();
    let message_count = projection.metadata.message_count;
    let markdown = render_conversation(projection);
    let redactions = count_redactions(&markdown);
    Some(SharePlan {
        markdown,
        model,
        mode,
        message_count,
        tool_results,
        redactions,
    })
}

/// Count the redaction markers the shared sanitizer leaves behind: `[redacted…]`
/// for secrets and keys, `***:***@` for URL credentials, and an encoded `***`
/// for sensitive URL query values.
fn count_redactions(markdown: &str) -> usize {
    ["[redacted", "***:***@", "=%2A%2A%2A"]
        .iter()
        .map(|marker| markdown.matches(marker).count())
        .sum()
}

const NOTHING_TO_SHARE: &str = "Nothing to share. The current session is empty.";

/// The counts both steps print, measured on the page itself. `confirm`
/// re-renders, so it states what it actually uploads rather than repeating
/// the preview's numbers (messages that arrived in between are included).
fn plan_summary(plan: &SharePlan) -> String {
    let tool_output = match plan.tool_results {
        Some(0) => "none".to_string(),
        Some(count) => format!("included ({count} tool result(s), redacted)"),
        None => "may be included (visible history, redacted)".to_string(),
    };
    format!(
        "Messages: {}\n\
         Tool output: {tool_output}\n\
         Redacted: {} item(s)\n\
         Omitted: hidden instructions, internal reasoning, reasoning signatures",
        plan.message_count, plan.redactions
    )
}

/// Plain `/share`: describe the page, upload nothing.
fn preview(export: &dyn CommandSessionExportContext) -> CommandResult {
    let Some(plan) = plan(export) else {
        return CommandResult::error(NOTHING_TO_SHARE);
    };
    CommandResult::message(format!(
        "Share preview — nothing has been uploaded.\n\
         \n\
         {}\n\
         \n\
         Run `/share confirm` to upload this page as {VISIBILITY_NOTE}.\n\
         Use `/export file <path>` to read the exact text first.",
        plan_summary(&plan)
    ))
}

/// `/share confirm`: hand the rendered, redacted page to the host to upload.
fn confirm(export: &dyn CommandSessionExportContext) -> CommandResult {
    let Some(plan) = plan(export) else {
        return CommandResult::error(NOTHING_TO_SHARE);
    };
    let html = render_session_html(&plan.markdown, &plan.model, &plan.mode);
    CommandResult::with_message_and_action(
        format!(
            "Uploading this page as {VISIBILITY_NOTE}...\n\
             \n\
             {}",
            plan_summary(&plan)
        ),
        AppAction::ShareSession { html },
    )
}

/// Upload a rendered page, then delete the local temp copy.
///
/// Called from the UI loop for `AppAction::ShareSession`. The upload runs on a
/// blocking thread because it shells out to `gh`.
pub async fn perform_share(html: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || upload_via_temp_file(&html, upload_gist))
        .await
        .map_err(|join_err| format!("share upload panicked: {join_err}"))?
}

/// Write `html` to a private temp file, run `upload` on its path, and remove
/// the file whatever the upload returned.
fn upload_via_temp_file(
    html: &str,
    upload: impl FnOnce(&Path) -> Result<String, String>,
) -> Result<String, String> {
    let tmp = write_temp_html(html).map_err(|e| format!("Failed to write temp file: {e}"))?;
    let result = upload(tmp.path());
    let removed = tmp.close();
    let url = result?;
    removed
        .map_err(|e| format!("Shared at {url}, but the local temp copy was not removed: {e}"))?;
    Ok(url)
}

/// Render the (already redacted) conversation as a standalone HTML page.
fn render_session_html(markdown: &str, model: &str, mode: &str) -> String {
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let escaped_model = html_escape(model);
    let escaped_mode = html_escape(mode);
    let escaped_body = html_escape(markdown);

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>codewhale Session Export</title>
<style>
  body {{
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    max-width: 800px; margin: 2rem auto; padding: 0 1rem;
    background: #0d1117; color: #c9d1d9;
  }}
  h1 {{ color: #58a6ff; border-bottom: 1px solid #30363d; padding-bottom: 0.5rem; }}
  .meta {{ color: #8b949e; font-size: 0.9rem; margin-bottom: 2rem; }}
  pre {{ white-space: pre-wrap; word-wrap: break-word; margin: 0; }}
  .footer {{ margin-top: 2rem; padding-top: 1rem; border-top: 1px solid #30363d; color: #8b949e; font-size: 0.8rem; }}
</style>
</head>
<body>
<h1>codewhale Session</h1>
<div class="meta">
  <strong>Model:</strong> {escaped_model} · <strong>Mode:</strong> {escaped_mode}<br>
  <strong>Exported:</strong> {timestamp}
</div>
<pre>{escaped_body}</pre>
<div class="footer">
  Generated by codewhale · https://github.com/codewhale-hq/CodeWhale
</div>
</body>
</html>"#,
    )
}

/// HTML-escape special characters.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Write HTML to a secure temp file and keep it alive for upload.
fn write_temp_html(html: &str) -> Result<tempfile::NamedTempFile, String> {
    let mut tmp = tempfile::Builder::new()
        .prefix("codewhale-share-")
        .suffix(".html")
        .tempfile()
        .map_err(|e| format!("{e}"))?;
    tmp.write_all(html.as_bytes()).map_err(|e| format!("{e}"))?;
    Ok(tmp)
}

/// Upload a file as a secret GitHub gist using the `gh` CLI. `gh gist
/// create` makes secret gists unless `--public` is passed; it never is here.
fn upload_gist(path: &Path) -> Result<String, String> {
    let mut cmd = codewhale_dependencies::Gh::command()
        .ok_or_else(|| "the `gh` CLI was not found".to_string())?;
    let output = cmd
        .args(gist_create_args(path))
        .output()
        .map_err(|e| format!("Failed to run `gh gist create`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`gh gist create` failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("`gh gist create` returned no output".to_string());
    }

    Ok(stdout)
}

fn gist_create_args(path: &Path) -> Vec<String> {
    vec![
        "gist".to_string(),
        "create".to_string(),
        path.to_string_lossy().into_owned(),
        "--filename".to_string(),
        "session-export.html".to_string(),
        "--desc".to_string(),
        "codewhale Session Export".to_string(),
    ]
}

pub(in crate::commands) const SHARE_INFO: CommandInfo = CommandInfo {
    name: "share",
    aliases: &[],
    usage: "/share [confirm]",
    description_key: "cmd_share_description",
};

pub(in crate::commands) struct ShareCmd;

impl RegisterCommand<CommandResult> for ShareCmd {
    fn info() -> &'static CommandInfo {
        &SHARE_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::SESSION_EXPORT,
            handler: share_contextual,
        }
    }
}

/// Contextual `/share` dispatch. `/share` reads the conversation through the
/// session-export facet, the same authority `/export` uses.
fn share_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(export) = parts.export.as_deref() else {
        return CommandResult::error("Command capability unavailable: session_export");
    };
    share(export, arg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::facets::{
        ExportMessage, ExportMetadata, TurnHandoffProjection,
    };
    use std::cell::Cell;
    use std::path::PathBuf;

    const FAKE_KEY: &str = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCD";

    /// Fake export facet: serves one projection and fails loudly if `/share`
    /// ever reaches for clipboard or filesystem effects.
    struct FakeExport {
        messages: Vec<ExportMessage>,
        projections: Cell<usize>,
    }

    impl FakeExport {
        fn new(messages: Vec<ExportMessage>) -> Self {
            Self {
                messages,
                projections: Cell::new(0),
            }
        }
    }

    impl CommandSessionExportContext for FakeExport {
        fn conversation_projection(&self) -> ConversationExportProjection {
            self.projections.set(self.projections.get() + 1);
            ConversationExportProjection {
                metadata: ExportMetadata {
                    session_label: "sess1234".to_string(),
                    provider: "deepseek".to_string(),
                    model: "deepseek-v4-pro".to_string(),
                    mode: "agent".to_string(),
                    workspace_name: "workspace".to_string(),
                    message_count: self.messages.len(),
                    exported_at_unix: 1_700_000_000,
                },
                transcript: TranscriptProjection::Authoritative(self.messages.clone()),
                restore_points: RestorePointProjection::Unreadable {
                    reason: "local-only snapshot detail".to_string(),
                },
            }
        }

        fn turn_handoff_projection(&self) -> TurnHandoffProjection {
            panic!("/share must not read the turn handoff");
        }

        fn clipboard_requires_terminal_paste(&self) -> bool {
            panic!("/share must not touch the clipboard");
        }

        fn write_recovery_copy(&self, _markdown: &str) -> Option<PathBuf> {
            panic!("/share must not write a recovery copy");
        }

        fn write_clipboard(&self, _markdown: &str) -> Result<(), String> {
            panic!("/share must not touch the clipboard");
        }

        fn resolve_export_path(&self, _raw: &str) -> Result<PathBuf, String> {
            panic!("/share must not resolve export paths");
        }

        fn write_export_file(
            &self,
            _path: &Path,
            _contents: &[u8],
            _force: bool,
        ) -> Result<(), String> {
            panic!("/share must not write export files");
        }
    }

    fn message(role: &str, blocks: Vec<ExportBlock>) -> ExportMessage {
        ExportMessage {
            role: role.to_string(),
            is_user_role: role == "user",
            blocks,
            prompt_snippet: None,
        }
    }

    fn session_with_secret_tool_result() -> FakeExport {
        FakeExport::new(vec![
            message(
                "system",
                vec![ExportBlock::Text {
                    text: "HIDDEN SYSTEM PROMPT".to_string(),
                }],
            ),
            message(
                "user",
                vec![ExportBlock::Text {
                    text: "print my env".to_string(),
                }],
            ),
            message(
                "assistant",
                vec![
                    ExportBlock::InternalReasoning,
                    ExportBlock::ToolCall {
                        id: "call_1".to_string(),
                        name: "exec_shell".to_string(),
                        caller: None,
                        input: serde_json::json!({"command": "env"}),
                    },
                ],
            ),
            message(
                "user",
                vec![ExportBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: format!("PATH=/usr/bin\nOPENAI_API_KEY={FAKE_KEY}\nleaked {FAKE_KEY}"),
                    is_error: false,
                    structured: None,
                }],
            ),
        ])
    }

    fn share_html(result: &CommandResult) -> &str {
        match result.action.as_ref() {
            Some(AppAction::ShareSession { html }) => html,
            other => panic!("expected ShareSession, got {other:?}"),
        }
    }

    #[test]
    fn plain_share_previews_without_an_upload_action() {
        let export = session_with_secret_tool_result();
        let result = share(&export, Some(""));
        assert!(!result.is_error, "{result:?}");
        assert!(
            result.action.is_none(),
            "plain /share must not start an upload: {:?}",
            result.action
        );
        let msg = result.message.unwrap();
        assert!(msg.contains("nothing has been uploaded"), "{msg}");
        assert!(msg.contains("Messages: 4"), "{msg}");
        assert!(
            msg.contains("Tool output: included (1 tool result(s), redacted)"),
            "{msg}"
        );
        assert!(msg.contains("Redacted: 2 item(s)"), "{msg}");
        assert!(msg.contains("/share confirm"), "{msg}");
        assert!(msg.contains("unlisted, not private"), "{msg}");
        assert!(!msg.contains(FAKE_KEY), "{msg}");
    }

    #[test]
    fn confirm_renders_the_redacted_export_projection_not_raw_messages() {
        let export = session_with_secret_tool_result();
        let result = share(&export, Some("confirm"));
        assert!(!result.is_error, "{result:?}");
        assert!(
            result
                .message
                .as_deref()
                .unwrap()
                .contains("unlisted, not private"),
            "{result:?}"
        );
        let msg = result.message.as_deref().unwrap();
        assert!(msg.contains("Messages: 4"), "{msg}");
        assert!(
            msg.contains("Tool output: included (1 tool result(s), redacted)"),
            "{msg}"
        );
        assert!(msg.contains("Redacted: 2 item(s)"), "{msg}");
        let html = share_html(&result);
        assert!(!html.contains(FAKE_KEY), "secret leaked into the page");
        assert!(
            !html.contains("sk-proj-"),
            "secret prefix leaked into the page"
        );
        assert!(!html.contains("HIDDEN SYSTEM PROMPT"), "system text leaked");
        assert!(html.contains("[internal context omitted]"));
        assert!(html.contains("[internal reasoning and signature omitted]"));
        assert!(
            html.contains("PATH=/usr/bin"),
            "ordinary tool output is kept"
        );
        assert!(
            !html.contains("local-only snapshot detail"),
            "restore-point table is local-only"
        );
        assert!(html.contains("deepseek-v4-pro"));
        assert_eq!(export.projections.get(), 1);
    }

    #[test]
    fn empty_session_errors_for_preview_and_confirm() {
        let export = FakeExport::new(Vec::new());
        for arg in ["", "confirm"] {
            let result = share(&export, Some(arg));
            assert!(result.is_error, "{arg}: {result:?}");
            assert!(result.message.unwrap().contains("Nothing to share"));
            assert!(result.action.is_none());
        }
    }

    #[test]
    fn help_and_unknown_routes() {
        let export = session_with_secret_tool_result();
        for arg in ["help", "--help", "-h"] {
            let result = share(&export, Some(arg));
            assert!(!result.is_error);
            assert!(result.action.is_none());
            let msg = result.message.unwrap();
            assert!(msg.contains("/share confirm"), "{msg}");
            assert!(msg.contains("unlisted, not private"), "{msg}");
        }
        let result = share(&export, Some("bogus"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("Unknown /share argument `bogus`")
        );
        assert_eq!(export.projections.get(), 0, "help/unknown read nothing");
    }

    #[test]
    fn missing_export_facet_fails_safely() {
        let result = share_contextual(CommandContexts::empty(), Some("confirm"));
        assert!(result.is_error);
        assert!(result.action.is_none());
        assert!(
            result
                .message
                .unwrap()
                .contains("Command capability unavailable: session_export")
        );
    }

    #[test]
    fn gist_is_created_without_public_flag() {
        let args = gist_create_args(Path::new("/tmp/page.html"));
        assert!(!args.iter().any(|arg| arg == "--public"), "{args:?}");
        assert_eq!(&args[..3], ["gist", "create", "/tmp/page.html"]);
    }

    #[test]
    fn temp_page_is_removed_after_upload_success_and_failure() {
        let mut seen = None;
        let url = upload_via_temp_file("<html>ok</html>", |path| {
            assert_eq!(std::fs::read_to_string(path).unwrap(), "<html>ok</html>");
            seen = Some(path.to_path_buf());
            Ok("https://gist.github.com/x".to_string())
        })
        .unwrap();
        assert_eq!(url, "https://gist.github.com/x");
        assert!(
            !seen.unwrap().exists(),
            "temp page must be deleted after upload"
        );

        let mut seen = None;
        let err = upload_via_temp_file("<html>no</html>", |path| {
            seen = Some(path.to_path_buf());
            Err("gh failed".to_string())
        })
        .unwrap_err();
        assert_eq!(err, "gh failed");
        assert!(
            !seen.unwrap().exists(),
            "temp page must be deleted on failure"
        );
    }

    #[test]
    fn html_escapes_the_rendered_markdown() {
        let html = render_session_html("<script>x</script> & \"q\"", "m<1>", "agent");
        assert!(html.contains("&lt;script&gt;x&lt;/script&gt; &amp; &quot;q&quot;"));
        assert!(html.contains("m&lt;1&gt;"));
        assert!(!html.contains("<script>"));
    }
}
