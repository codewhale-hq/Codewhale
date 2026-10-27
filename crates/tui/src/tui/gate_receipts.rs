//! Transcript receipts for permission decisions nobody was prompted for.
//!
//! Auto-Review makes decisions a person would otherwise never see: the model
//! guardian allows or denies a held call, the deterministic policy blocks one,
//! or a hold that needs a person is denied instead of opening a modal. The
//! audit log keeps the full record; these one-line notes make the decision
//! and its stated reason visible in the transcript, the way a permission
//! prompt would have been.
//!
//! Proven-safe deterministic allows are deliberately silent (a routine read
//! is not news) — the same convention other harnesses use for rule-based
//! auto-approvals.

use std::borrow::Cow;

use crate::core::events::{ToolGate, ToolGateVerdict, bounded_gate_reason};
use codewhale_localization::{Locale, MessageId, tr};

/// Longest tool name echoed into a receipt. Tool names are model-authored
/// text on some wire dialects, so they are bounded like every other field.
const MAX_TOOL_NAME_CHARS: usize = 64;

/// One transcript line for a [`crate::core::events::Event::ToolGateDecision`].
#[must_use]
pub fn tool_gate_receipt(
    locale: Locale,
    tool_name: &str,
    gate: ToolGate,
    decision: ToolGateVerdict,
    risk: Option<&str>,
    reason: &str,
) -> String {
    let id = match (gate, decision) {
        (ToolGate::AutoReviewGuardian, ToolGateVerdict::Allowed) => {
            MessageId::AutoReviewReceiptGuardianAllowed
        }
        (ToolGate::AutoReviewGuardian, ToolGateVerdict::Denied) => {
            MessageId::AutoReviewReceiptGuardianDenied
        }
        (ToolGate::AutoReviewGuardian, ToolGateVerdict::Unavailable) => {
            MessageId::AutoReviewReceiptGuardianUnavailable
        }
        // The deterministic engine only surfaces blocks; an allow it proved
        // safe stays silent and an unavailable deterministic verdict does not
        // exist (the engine always answers).
        (
            ToolGate::AutoReviewDeterministic,
            ToolGateVerdict::Denied | ToolGateVerdict::Allowed | ToolGateVerdict::Unavailable,
        ) => MessageId::AutoReviewReceiptDeterministicBlocked,
    };
    fill(
        tr(locale, id),
        tool_name,
        risk.unwrap_or("unknown"),
        &bounded_gate_reason(reason),
    )
}

/// The `tool.gate.decision` record `audit.log` keeps for the same decision.
/// The reason goes through the secret redactor: a reviewer's rationale can
/// quote the command it judged. The caller adds `session_id`.
#[must_use]
pub fn tool_gate_audit_record(
    agent_id: Option<&str>,
    tool_id: &str,
    tool_name: &str,
    gate: ToolGate,
    decision: ToolGateVerdict,
    risk: Option<&str>,
    reason: &str,
) -> serde_json::Value {
    serde_json::json!({
        "agent_id": agent_id,
        "tool_id": tool_id,
        "tool_name": tool_name,
        "gate": gate.as_str(),
        "decision": decision.as_str(),
        "risk": risk,
        "reason": codewhale_secrets::redact::redact_secrets(reason),
    })
}

/// The receipt for a safety-floor hold that Auto-Review denied without
/// pausing (the posture never opens a prompt).
#[must_use]
pub fn auto_review_held_receipt(locale: Locale, tool_name: &str) -> String {
    fill(
        tr(locale, MessageId::AutoReviewReceiptHeld),
        tool_name,
        "",
        "",
    )
}

fn fill(template: Cow<'static, str>, tool_name: &str, risk: &str, reason: &str) -> String {
    template
        .replace("{tool}", &bounded_tool_name(tool_name))
        .replace("{risk}", risk)
        .replace("{reason}", reason)
}

fn bounded_tool_name(tool_name: &str) -> String {
    let cleaned = bounded_gate_reason(tool_name);
    if cleaned.chars().count() <= MAX_TOOL_NAME_CHARS {
        return cleaned;
    }
    let mut out: String = cleaned.chars().take(MAX_TOOL_NAME_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_audit_record_names_the_gate_and_redacts_the_reason() {
        let record = tool_gate_audit_record(
            None,
            "call-1",
            "bash",
            ToolGate::AutoReviewDeterministic,
            ToolGateVerdict::Denied,
            None,
            "blocked `curl -H 'Authorization: Bearer sk-live-abcdefghijklmnopqrstuv' x`",
        );
        assert_eq!(record["gate"], "auto_review_deterministic");
        assert_eq!(record["decision"], "denied");
        assert_eq!(record["tool_id"], "call-1");
        let reason = record["reason"].as_str().expect("reason");
        assert!(
            !reason.contains("sk-live-abcdefghijklmnopqrstuv"),
            "{reason}"
        );

        // Written where every other audit event goes.
        let home = tempfile::tempdir().expect("tempdir");
        codewhale_runtime::audit::log_sensitive_event_in(home.path(), "tool.gate.decision", record);
        let log = std::fs::read_to_string(home.path().join("audit.log")).expect("audit.log");
        assert!(log.contains("\"event\":\"tool.gate.decision\""), "{log}");
        assert!(
            log.contains("\"gate\":\"auto_review_deterministic\""),
            "{log}"
        );
    }

    #[test]
    fn guardian_allow_names_tool_risk_and_reason() {
        let line = tool_gate_receipt(
            Locale::En,
            "bash",
            ToolGate::AutoReviewGuardian,
            ToolGateVerdict::Allowed,
            Some("low"),
            "reads a log file inside the workspace",
        );
        assert_eq!(
            line,
            "Auto-Review allowed 'bash' (low risk, model guardian): reads a log file inside the workspace"
        );
    }

    #[test]
    fn guardian_deny_and_unavailable_are_distinct_receipts() {
        let denied = tool_gate_receipt(
            Locale::En,
            "bash",
            ToolGate::AutoReviewGuardian,
            ToolGateVerdict::Denied,
            Some("high"),
            "would push to a remote",
        );
        assert!(denied.starts_with("Auto-Review denied 'bash' (high risk, model guardian): "));
        let unavailable = tool_gate_receipt(
            Locale::En,
            "File",
            ToolGate::AutoReviewGuardian,
            ToolGateVerdict::Unavailable,
            None,
            "guardian request timed out",
        );
        assert!(unavailable.contains("could not review 'File' (guardian request timed out)"));
        assert!(
            unavailable.ends_with("denied, fail closed"),
            "{unavailable}"
        );
        assert!(!unavailable.contains("unknown risk"), "{unavailable}");
    }

    #[test]
    fn deterministic_block_receipt_names_the_policy() {
        let line = tool_gate_receipt(
            Locale::En,
            "bash",
            ToolGate::AutoReviewDeterministic,
            ToolGateVerdict::Denied,
            None,
            "publish-like command",
        );
        assert_eq!(
            line,
            "Auto-Review blocked 'bash' (deterministic policy): publish-like command"
        );
    }

    #[test]
    fn receipts_bound_and_defang_untrusted_reason_and_tool_text() {
        let long = "x".repeat(600);
        let line = tool_gate_receipt(
            Locale::En,
            "ba\u{1b}[31msh\n\u{202E}",
            ToolGate::AutoReviewGuardian,
            ToolGateVerdict::Denied,
            Some("medium"),
            &format!("evil\u{1b}]0;title\u{7}  {long}"),
        );
        assert!(!line.contains('\u{1b}'));
        assert!(!line.contains('\n'));
        assert!(!line.contains('\u{202E}'));
        assert!(!line.contains('\u{7}'));
        assert!(line.chars().count() < 340, "{}", line.chars().count());
    }

    #[test]
    fn held_receipt_is_localized_and_names_the_tool() {
        let en = auto_review_held_receipt(Locale::En, "write");
        assert!(en.starts_with("Auto-Review held 'write' without pausing"));
        let ja = auto_review_held_receipt(Locale::Ja, "write");
        assert!(ja.contains("'write'"));
        assert_ne!(ja, en);
    }

    #[test]
    fn every_shipped_pack_keeps_receipt_placeholders() {
        for locale in Locale::shipped_complete() {
            let line = tool_gate_receipt(
                *locale,
                "bash",
                ToolGate::AutoReviewGuardian,
                ToolGateVerdict::Allowed,
                Some("low"),
                "REASON-SENTINEL",
            );
            assert!(line.contains("'bash'"), "{locale:?}: {line}");
            assert!(line.contains("REASON-SENTINEL"), "{locale:?}: {line}");
            assert!(!line.contains('{'), "{locale:?}: {line}");
        }
    }
}
