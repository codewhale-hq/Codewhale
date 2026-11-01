#![allow(dead_code)] // Receipt types land incrementally; stream JSON is the first consumer.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Terminal reasons shared by TUI, JSONL, Fleet, and benchmark receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTerminationReason {
    Resolved,
    Unresolved,
    Canceled,
    Stuck,
    Timeout,
    BudgetExhausted,
    ApprovalRequired,
    ModelError,
    ToolError,
    InfrastructureError,
    EvidenceMissing,
}

impl RunTerminationReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
            Self::Canceled => "canceled",
            Self::Stuck => "stuck",
            Self::Timeout => "timeout",
            Self::BudgetExhausted => "budget_exhausted",
            Self::ApprovalRequired => "approval_required",
            Self::ModelError => "model_error",
            Self::ToolError => "tool_error",
            Self::InfrastructureError => "infrastructure_error",
            Self::EvidenceMissing => "evidence_missing",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Resolved)
    }

    #[must_use]
    pub const fn process_exit_code(self) -> i32 {
        match self {
            Self::Resolved => 0,
            Self::Unresolved | Self::EvidenceMissing => 2,
            Self::ApprovalRequired => 3,
            Self::Canceled => 130,
            Self::Stuck
            | Self::Timeout
            | Self::BudgetExhausted
            | Self::ModelError
            | Self::ToolError
            | Self::InfrastructureError => 1,
        }
    }
}

/// Reduce the existing Engine outcome plus typed subsystem evidence into the
/// terminal status shared by machine-facing projections. Successful and
/// canceled turns are unambiguous. For failed turns, the terminal error
/// category wins over historical tool failures from earlier, recovered steps;
/// approval/tool evidence is the fallback when the terminal event has no
/// typed cause.
#[must_use]
pub fn classify_turn_termination(
    status: crate::core::events::TurnOutcomeStatus,
    error_category: Option<codewhale_error_taxonomy::ErrorCategory>,
    tool_error_seen: bool,
    approval_required: bool,
) -> RunTerminationReason {
    use crate::core::events::TurnOutcomeStatus;
    use codewhale_error_taxonomy::ErrorCategory;

    match status {
        TurnOutcomeStatus::Completed => RunTerminationReason::Resolved,
        TurnOutcomeStatus::Interrupted => RunTerminationReason::Canceled,
        // A provider-declared incomplete response is the terminal cause even
        // if an earlier tool in the same run needed approval. Historical
        // approval evidence must not relabel model truncation.
        TurnOutcomeStatus::Failed
            if matches!(error_category, Some(ErrorCategory::InvalidInput)) =>
        {
            RunTerminationReason::ModelError
        }
        TurnOutcomeStatus::Failed if approval_required => RunTerminationReason::ApprovalRequired,
        TurnOutcomeStatus::Failed => match error_category {
            Some(ErrorCategory::Budget) => RunTerminationReason::BudgetExhausted,
            Some(ErrorCategory::Timeout) => RunTerminationReason::Timeout,
            Some(
                ErrorCategory::Network
                | ErrorCategory::Authentication
                | ErrorCategory::Authorization
                | ErrorCategory::RateLimit
                | ErrorCategory::InvalidInput
                | ErrorCategory::Parse,
            ) => RunTerminationReason::ModelError,
            Some(ErrorCategory::Tool) => RunTerminationReason::ToolError,
            Some(ErrorCategory::State | ErrorCategory::Internal) => {
                RunTerminationReason::InfrastructureError
            }
            None if tool_error_seen => RunTerminationReason::ToolError,
            None => RunTerminationReason::Unresolved,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    NotRun,
    PassedFirstAttempt,
    PassedAfterRepair,
    Failed,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReceipt {
    pub command: String,
    pub outcome: VerificationOutcome,
    pub attempts: u32,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReceipt {
    pub reason: RunTerminationReason,
    pub summary: String,
    #[serde(default)]
    pub files_changed: Vec<PathBuf>,
    #[serde(default)]
    pub verification: Vec<VerificationReceipt>,
    #[serde(default)]
    pub remaining_risks: Vec<String>,
}

impl RunReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() {
            return Err("run receipt summary cannot be empty".to_string());
        }
        if self.reason == RunTerminationReason::Resolved
            && self
                .verification
                .iter()
                .any(|receipt| receipt.outcome == VerificationOutcome::Failed)
        {
            return Err("resolved run cannot contain a failed verification receipt".to_string());
        }
        Ok(())
    }

    #[must_use]
    pub fn pass_at_one(&self) -> bool {
        !self.verification.is_empty()
            && self.verification.iter().all(|receipt| {
                receipt.outcome == VerificationOutcome::PassedFirstAttempt && receipt.attempts == 1
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_required_has_machine_distinct_exit() {
        assert_eq!(
            RunTerminationReason::ApprovalRequired.process_exit_code(),
            3
        );
        assert!(!RunTerminationReason::ApprovalRequired.is_success());
    }

    #[test]
    fn repair_success_is_not_pass_at_one() {
        let receipt = RunReceipt {
            reason: RunTerminationReason::Resolved,
            summary: "fixed".to_string(),
            files_changed: vec![PathBuf::from("src/lib.rs")],
            verification: vec![VerificationReceipt {
                command: "cargo test".to_string(),
                outcome: VerificationOutcome::PassedAfterRepair,
                attempts: 2,
                duration_ms: 10,
                artifact: None,
            }],
            remaining_risks: Vec::new(),
        };
        assert!(!receipt.pass_at_one());
        assert!(receipt.validate().is_ok());
    }

    #[test]
    fn failed_turns_keep_model_tool_and_infrastructure_distinct() {
        use crate::core::events::TurnOutcomeStatus;
        use codewhale_error_taxonomy::ErrorCategory;

        assert_eq!(
            classify_turn_termination(
                TurnOutcomeStatus::Failed,
                Some(ErrorCategory::Network),
                false,
                false,
            ),
            RunTerminationReason::ModelError
        );
        assert_eq!(
            classify_turn_termination(
                TurnOutcomeStatus::Failed,
                Some(ErrorCategory::InvalidInput),
                false,
                false,
            ),
            RunTerminationReason::ModelError
        );
        assert_eq!(
            classify_turn_termination(
                TurnOutcomeStatus::Failed,
                Some(ErrorCategory::Internal),
                false,
                false,
            ),
            RunTerminationReason::InfrastructureError
        );
        assert_eq!(
            classify_turn_termination(TurnOutcomeStatus::Failed, None, true, false),
            RunTerminationReason::ToolError
        );
        assert_eq!(
            classify_turn_termination(
                TurnOutcomeStatus::Failed,
                Some(ErrorCategory::InvalidInput),
                true,
                false,
            ),
            RunTerminationReason::ModelError,
            "a recovered tool error must not hide the terminal model failure"
        );
        assert_eq!(
            classify_turn_termination(
                TurnOutcomeStatus::Failed,
                Some(ErrorCategory::InvalidInput),
                true,
                true,
            ),
            RunTerminationReason::ModelError,
            "historical approval evidence must not hide terminal truncation"
        );
    }
}
