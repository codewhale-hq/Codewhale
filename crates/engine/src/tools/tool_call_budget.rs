//! Hard per-turn tool-call admission budget (#4415).
//!
//! One counter per turn, built from the task's structured `max_tool_calls`
//! constraint. Every proposed tool call passes the same gate in proposal
//! order, so a batch larger than the remaining budget is truncated to
//! exactly the calls that still fit and the excess are rejected — never
//! executed — with a typed reason carrying the remaining-call count.
//!
//! The cap counts *admitted* calls: a call that debits a slot but is then
//! stopped by a later admission gate (deny-list, allow-list, sandbox,
//! hooks, missing tool) is refunded before it would have executed (#5170),
//! so blocked calls cannot burn the budget.

use codewhale_tools::ToolError;

/// Countdown of tool calls one turn may still admit.
///
/// This is the turn's admission state: created when the turn starts and
/// decremented at the admission gate. It deliberately does not live in the
/// tool catalog or surface policy, which only carry the declared limit.
/// `None` means unlimited — the default when a task declares no budget —
/// and leaves the gate inert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCallBudget {
    max: Option<u32>,
    remaining: Option<u32>,
}

/// Typed rejection for a call that exceeds the remaining budget.
///
/// Rendered into the same `PermissionDenied` error the neighboring admission
/// gates (deny-list, allow-list) produce, so the denial is visible in the
/// transcript and to the model with the remaining-call count spelled out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCallBudgetExceeded {
    max: u32,
}

impl ToolCallBudgetExceeded {
    pub fn into_tool_error(self, tool_name: &str) -> ToolError {
        ToolError::permission_denied(format!(
            "Tool '{tool_name}' rejected: per-turn tool-call budget of {} exhausted (remaining=0). \
             The call was not executed.",
            self.max
        ))
    }
}

impl ToolCallBudget {
    pub fn new(max_tool_calls: Option<u32>) -> Self {
        Self {
            max: max_tool_calls,
            remaining: max_tool_calls,
        }
    }

    /// Debit one proposed tool call at the admission gate. While budget
    /// remains, the call is admitted and the remaining count decrements;
    /// once exhausted, the call is rejected with [`ToolCallBudgetExceeded`].
    /// A call stopped by a later gate before execution must be handed back
    /// through [`refund`](Self::refund) so the cap counts admitted calls.
    pub fn admit(&mut self) -> Result<(), ToolCallBudgetExceeded> {
        let (Some(max), Some(remaining)) = (self.max, self.remaining.as_mut()) else {
            return Ok(());
        };
        if *remaining == 0 {
            return Err(ToolCallBudgetExceeded { max });
        }
        *remaining -= 1;
        Ok(())
    }

    /// Hand back one debited slot when the call that took it is blocked by
    /// a later admission gate and never executes (#5170). Clamped at the
    /// declared maximum so a refund can never grow the budget.
    pub fn refund(&mut self) {
        if let (Some(max), Some(remaining)) = (self.max, self.remaining.as_mut()) {
            *remaining = (*remaining + 1).min(max);
        }
    }
}
