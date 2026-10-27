//! `notify` tool — model-callable desktop notification (#1322).
//!
//! Delivered by the terminal host through `codewhale_runtime::host_terminal` (OSC 9
//! for known capable terminals, BEL fallback on macOS / Linux, `MessageBeep`
//! on Windows when explicitly opted in). The model decides when to fire —
//! the tool is intended for "long task done, come back" beats and
//! sub-agent-completion pings, not chatter.
//!
//! Honors the user's `[notifications]` config: `method = "off"` silences
//! the tool entirely, and `quiet` / `events.model-notify = false` gate the
//! category through the process-wide notification gate. Output messages
//! are length-capped so a runaway model can't paint a paragraph into the
//! terminal title bar.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, required_str,
};

/// Maximum chars passed through for the title — keeps the OSC 9 escape
/// reasonable on terminals that wrap long titles awkwardly.
const NOTIFY_TITLE_CAP: usize = 80;
/// Maximum chars passed through for the body. Most receivers truncate
/// past ~120, so 200 leaves headroom while still bounded.
const NOTIFY_BODY_CAP: usize = 200;

/// Tool that fires a single desktop notification.
pub struct NotifyTool;

#[async_trait]
impl ToolSpec for NotifyTool {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn description(&self) -> &'static str {
        "Send a desktop notification only when the user must act: a long task \
         completed, a blocking error needs a decision, or progress cannot \
         continue without an answer. Never notify for routine progress, \
         acknowledgements, or liveness. Pass a short `title` and optional \
         `body`. Users can silence everything with \
         `[notifications].method = \"off\"` or `[notifications].quiet = true`, \
         or this category with `[notifications.events].model-notify = false`; \
         disabled notifications are silent no-ops."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short notification title (≤ 80 chars after truncation). Required."
                },
                "body": {
                    "type": "string",
                    "description": "Optional longer body (≤ 200 chars after truncation)."
                }
            },
            "required": ["title"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // No filesystem or shell side effects; the only output is one
        // notification the terminal host delivers (or declines to, when
        // none is installed). Mark as ReadOnly so the
        // approval-requirement default is `Auto` and the tool routes
        // through without prompting.
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let title_raw = required_str(&input, "title")?;
        let body_raw = optional_str(&input, "body")?.unwrap_or("");

        // Char-bounded truncation (not byte-bounded) so we don't slice
        // through a multi-byte sequence and emit invalid UTF-8 to the
        // terminal.
        let title: String = title_raw.chars().take(NOTIFY_TITLE_CAP).collect();
        let body: String = body_raw.chars().take(NOTIFY_BODY_CAP).collect();
        let title = title.trim();
        let body = body.trim();

        if title.is_empty() {
            return Err(ToolError::execution_failed("title must not be empty"));
        }

        // #4834: the host builds the typed payload from this text (bounded,
        // control-byte-stripped, redacted) and applies the user's method,
        // gate and attention policy. Threshold zero: the model has already
        // decided this is the moment.
        let receipt = codewhale_runtime::host_terminal::host()
            .notify_model(title, if body.is_empty() { None } else { Some(body) });

        Ok(ToolResult::success(format!("{receipt}: {title}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn ctx() -> ToolContext {
        ToolContext::new(Path::new("."))
    }

    #[tokio::test]
    async fn rejects_missing_title() {
        let err = NotifyTool.execute(json!({}), &ctx()).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("title"), "{err}");
    }

    #[tokio::test]
    async fn rejects_empty_title_after_trim() {
        let err = NotifyTool
            .execute(json!({"title": "   "}), &ctx())
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("must not be empty"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn truncates_title_to_cap() {
        let long = "x".repeat(500);
        let result = NotifyTool
            .execute(json!({"title": long}), &ctx())
            .await
            .expect("ok");
        // Confirmation message echoes the *truncated* title.
        let echo_x_count = result.content.matches('x').count();
        assert_eq!(echo_x_count, NOTIFY_TITLE_CAP);
    }

    #[tokio::test]
    async fn accepts_body_optional() {
        let result = NotifyTool
            .execute(json!({"title": "done", "body": "tests pass"}), &ctx())
            .await
            .expect("ok");
        assert!(result.success);
        assert!(result.content.contains("done"));
    }

    #[tokio::test]
    async fn safe_against_multibyte_truncation() {
        // Construct a title whose char-count is below the cap but whose
        // byte-count would be above a naive byte cap; assert no panic
        // and the success-content roundtrips the title intact.
        let title: String = "我".repeat(30); // 30 chars × 3 bytes = 90 bytes, < 80 chars cap (well, == 30 chars)
        let result = NotifyTool
            .execute(json!({"title": title.clone()}), &ctx())
            .await
            .expect("ok");
        assert!(result.content.contains(&title));
    }

    #[test]
    fn schema_exposes_title_and_body_fields() {
        let schema = NotifyTool.input_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("title").is_some());
        assert!(props.get("body").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("title")));
        assert!(!required.iter().any(|v| v.as_str() == Some("body")));
    }

    #[tokio::test]
    async fn the_model_sees_success_whatever_the_host_did() {
        // The description promises a *silent* no-op: whatever the host did
        // with the notification, the model sees success (nothing to retry).
        // No host is installed in this test binary, so this is the no-host
        // receipt; `method = "off"` through the TUI host is covered by
        // `tui::notifications` (`notify_model_honors_the_installed_off_method`)
        // and `tui::ui::terminal` (`tui_host_routes_the_notify_tool_through_the_installed_method`).

        let result = NotifyTool
            .execute(json!({"title": "done"}), &ctx())
            .await
            .expect("ok");
        assert!(result.success);
        // The receipt is the installed host's, not a hard-coded "sent": no
        // host is installed in this test binary, so it is the no-host one.
        assert_eq!(
            result.content,
            "notification not sent: no terminal host: done"
        );
    }
}
