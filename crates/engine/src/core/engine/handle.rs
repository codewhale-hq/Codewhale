//! Public `EngineHandle` methods.
//!
//! The struct itself lives next door in `engine.rs` because two
//! construction sites (`Engine::new` and the test-only
//! `mock_engine_handle`) need access to its private mpsc channels.
//! The method surface — `send`, `cancel*`, `is_cancelled`,
//! `approve_tool_call` / `deny_tool_call` / `retry_tool_with_policy`,
//! `submit_user_input` / `cancel_user_input`, and `steer` — moves here
//! so the agent loop's mailbox API is reviewable on its own.

use anyhow::Result;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;

use super::approval::{ApprovalDecision, SubmittedApproval, UserInputDecision};
use super::{
    CancelReason, EngineHandle, EngineRunCompletion, LiveRuntimeAuthority, Op,
    RuntimePermissionAuthority, UserInputResponse,
};
use crate::approval_log::ApprovalDecider;

// This bounds host delivery/acknowledgement, never the person's time to
// answer. No active waiter may consume a reply after its caller abandoned it.
const USER_INPUT_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl EngineHandle {
    /// Wait for this exact background run to return or terminate abnormally.
    /// Cancelling a wait does not stop the engine or consume another waiter's result.
    /// Directly constructed handles return `NotObserved`; completion does not prove
    /// persistence succeeded or detached children and blocking writers stopped.
    pub async fn wait_run_completion(&self) -> EngineRunCompletion {
        let Some(mut completion) = self.run_completion.clone() else {
            return EngineRunCompletion::NotObserved;
        };
        loop {
            let outcome = *completion.borrow_and_update();
            if let Some(outcome) = outcome {
                return outcome;
            }
            if completion.changed().await.is_err() {
                return EngineRunCompletion::AbnormalTermination;
            }
        }
    }
}

async fn await_user_input_acceptance(
    sender: &mpsc::Sender<UserInputDecision>,
    decision: UserInputDecision,
    mut accepted: oneshot::Receiver<bool>,
    timeout: std::time::Duration,
) -> Result<()> {
    let delivery = tokio::time::timeout(timeout, async {
        sender
            .send(decision)
            .await
            .map_err(|_| anyhow::anyhow!("Engine is not accepting user input"))?;
        (&mut accepted).await.map_err(|_| {
            anyhow::anyhow!("Engine ended the user input request before accepting this decision")
        })
    })
    .await;
    let accepted = match delivery {
        Ok(result) => result?,
        Err(_) => {
            // Close first: after this point Engine's verdict send cannot
            // succeed. A verdict already delivered before the bound wins
            // over the timeout, so a consumed answer is never called rejected.
            accepted.close();
            accepted.try_recv().unwrap_or(false)
        }
    };
    anyhow::ensure!(
        accepted,
        "User input request is no longer accepting this decision"
    );
    Ok(())
}

#[derive(Clone)]
pub(super) struct TurnControl {
    pub id: u64,
    pub narrowing: super::host_profile::TurnNarrowing,
    pub cancel: CancellationToken,
    pub reason: Arc<StdMutex<Option<CancelReason>>>,
}

#[derive(Default)]
pub(super) struct TurnControls {
    next_id: u64,
    pub active: Option<TurnControl>,
    pub pending: VecDeque<TurnControl>,
}

impl TurnControls {
    pub fn fresh(&mut self) -> TurnControl {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("turn control id exhausted");
        TurnControl {
            id: self.next_id,
            narrowing: super::host_profile::TurnNarrowing::Inherit,
            cancel: CancellationToken::new(),
            reason: Arc::new(StdMutex::new(None)),
        }
    }

    fn target(&self) -> Option<&TurnControl> {
        self.active.as_ref().or_else(|| self.pending.front())
    }
}

pub(super) struct TurnControlGuard {
    pub controls: Arc<StdMutex<TurnControls>>,
    pub id: u64,
    pub narrowing: super::host_profile::TurnNarrowing,
}

impl Drop for TurnControlGuard {
    fn drop(&mut self) {
        let mut controls = self
            .controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if controls
            .active
            .as_ref()
            .is_some_and(|active| active.id == self.id)
        {
            controls.active = None;
        }
    }
}

#[derive(Debug)]
pub struct SteerInput {
    pub(super) turn_id: Option<u64>,
    pub(super) replace_pending: bool,
    pub content: String,
    pub(super) outcome: Option<oneshot::Sender<SteerOutcome>>,
}

/// The engine's verdict on one steer. A steer whose turn had already moved
/// on is discarded by `next_turn_steer`; the verdict tells the sender which
/// happened, because "the channel accepted the text" is not "the model saw
/// it" (#6276).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    /// The steer's text was committed into the session record inside the
    /// turn it was sent to; the model received it.
    Accepted,
    /// The turn had already moved on (or ended) before the steer reached a
    /// commit boundary. The model never saw the text.
    Dropped,
}

/// A steer the engine has taken ownership of. Committing it reports
/// [`SteerOutcome::Accepted`]; any other exit — interrupt, failure, early
/// return, silent drop of the pending queue — reports `Dropped` from `Drop`,
/// so no path can lose a verdict.
pub struct PendingSteer {
    pub replace_pending: bool,
    pub content: String,
    outcome: Option<oneshot::Sender<SteerOutcome>>,
}

impl PendingSteer {
    pub fn new(content: String, outcome: Option<oneshot::Sender<SteerOutcome>>) -> Self {
        Self {
            content,
            outcome,
            replace_pending: false,
        }
    }

    /// Commit the steer into the turn's record: report `Accepted`, then hand
    /// back the text. Consuming `self` without calling this reports
    /// `Dropped` via `Drop`.
    pub fn commit(mut self) -> String {
        if let Some(outcome) = self.outcome.take() {
            let _ = outcome.send(SteerOutcome::Accepted);
        }
        // `Drop` runs after this returns and finds `outcome` already taken,
        // so the verdict stays exactly one `Accepted`.
        std::mem::take(&mut self.content)
    }
}

impl Drop for PendingSteer {
    fn drop(&mut self) {
        if let Some(outcome) = self.outcome.take() {
            let _ = outcome.send(SteerOutcome::Dropped);
        }
    }
}

impl SteerInput {
    /// Take ownership of this steer as an unsettled [`PendingSteer`].
    ///
    /// This is the only way to claim a steer off the channel. Whatever the
    /// claimant then does — `commit()` or drop — settles it exactly once, so
    /// there is one settlement mechanism rather than two (#6276).
    pub fn into_pending(mut self) -> PendingSteer {
        // Both fields are taken, so the `Drop` below finds nothing left to
        // settle and the verdict travels with the `PendingSteer`.
        let mut pending = PendingSteer::new(std::mem::take(&mut self.content), self.outcome.take());
        pending.replace_pending = self.replace_pending;
        pending
    }
}

impl Drop for SteerInput {
    fn drop(&mut self) {
        if let Some(outcome) = self.outcome.take() {
            let _ = outcome.send(SteerOutcome::Dropped);
        }
    }
}

impl std::ops::Deref for SteerInput {
    type Target = str;
    fn deref(&self) -> &str {
        &self.content
    }
}

impl std::fmt::Display for SteerInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.content.fmt(f)
    }
}

pub struct SteerPermit {
    permit: mpsc::OwnedPermit<SteerInput>,
    turn_id: Option<u64>,
}

impl SteerPermit {
    pub fn send(self, content: String) {
        self.permit.send(SteerInput {
            turn_id: self.turn_id,
            replace_pending: false,
            content,
            outcome: None,
        });
    }

    /// Send a steer and receive the engine's verdict on it. The receiver
    /// resolves to [`SteerOutcome::Accepted`] when the turn commits the text
    /// into its record, [`SteerOutcome::Dropped`] when the turn moved on
    /// first, and closes without a verdict only if the engine itself is gone
    /// (#6276).
    pub fn send_with_outcome(self, content: String) -> oneshot::Receiver<SteerOutcome> {
        self.send_with_replacement_outcome(content, false)
    }

    pub fn send_replacing_with_outcome(self, content: String) -> oneshot::Receiver<SteerOutcome> {
        self.send_with_replacement_outcome(content, true)
    }

    fn send_with_replacement_outcome(
        self,
        content: String,
        replace_pending: bool,
    ) -> oneshot::Receiver<SteerOutcome> {
        let (outcome_tx, outcome_rx) = oneshot::channel();
        self.permit.send(SteerInput {
            turn_id: self.turn_id,
            replace_pending,
            content,
            outcome: Some(outcome_tx),
        });
        outcome_rx
    }
}

impl EngineHandle {
    /// The engine's turn-phase heartbeat (#6184). Hosts read it to tell a
    /// bounded model wait from a wedged turn without inferring liveness from
    /// the stream-chunk timeout.
    #[must_use]
    pub fn turn_heartbeat(&self) -> &Arc<super::turn_heartbeat::TurnHeartbeat> {
        &self.turn_heartbeat
    }

    /// Called only while Runtime holds the idle turn admission claim. The
    /// following SendMessage refreshes the existing prompt/config projection.
    pub fn restore_runtime_goal(
        &self,
        goal: Option<&codewhale_protocol::ThreadGoal>,
    ) -> Result<()> {
        let mut state = self
            .goal_state
            .lock()
            .map_err(|_| anyhow::anyhow!("goal state lock poisoned"))?;
        let current = state.snapshot();
        if current.goal_id.as_deref() != goal.map(|goal| goal.goal_id.as_str()) {
            *state = goal.map_or_else(crate::tools::goal::GoalState::default, |goal| {
                crate::tools::goal::GoalState::from_snapshot(
                    &crate::tools::goal::GoalSnapshot::from_thread_goal(goal),
                )
            });
        }
        Ok(())
    }

    /// Apply the host's latest durable goal control to the live continuation
    /// gate. The mailbox drains only between turns, so it cannot carry stop
    /// controls. A replacement parks the old goal until normal turn admission
    /// restores the new revision; it never starts a second turn here.
    pub fn sync_runtime_goal_control(
        &self,
        goal: Option<&codewhale_protocol::ThreadGoal>,
    ) -> Result<()> {
        let mut state = self
            .goal_state
            .lock()
            .map_err(|_| anyhow::anyhow!("goal state lock poisoned"))?;
        let current = state.snapshot();
        let Some(goal) = goal else {
            state.clear();
            return Ok(());
        };
        if current.goal_id.as_deref() != Some(goal.goal_id.as_str()) {
            if state.is_active() {
                state.sync_from_host_status(
                    current.objective.as_deref(),
                    current.token_budget,
                    crate::tools::goal::GoalStatus::Paused,
                );
            }
        } else {
            let (status, pause_reason) = crate::tools::goal::thread_goal_status_projection(
                goal.status.clone(),
                goal.pause_reason,
            );
            if status != crate::tools::goal::GoalStatus::Active {
                state.sync_from_host_status_with_reason(
                    current.objective.as_deref(),
                    current.token_budget,
                    status,
                    pause_reason,
                );
            }
        }
        Ok(())
    }

    /// True when the caller must preflight a concrete provider client before
    /// committing UI/runtime turn state. Test and embedding handles with an
    /// injected model client return false because that client owns model I/O.
    #[must_use]
    pub fn client_preflight_required(&self) -> bool {
        self.client_preflight_required
    }

    pub fn retire_mcp_for_handoff(&self) -> Option<crate::mcp::McpHandoff> {
        let mut slot = self
            .mcp_pool_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.handed_off = true;
        if let Some(handoff) = slot.handoff.take() {
            return Some(handoff);
        }
        let pool = slot.pool.upgrade()?;
        let mut pool = pool.try_lock().ok()?;
        Some(pool.retire_for_handoff())
    }

    /// Send an operation to the engine
    ///
    /// This awaits channel capacity, and the engine drains `rx_op` only
    /// between turns — so on the UI event loop an awaited send into a
    /// saturated mailbox freezes input for the rest of the turn (#6150).
    /// Input-path callers instead either `try_send` a droppable op (report
    /// the rejection) or `try_reserve_owned` before committing UI state and
    /// hand off with `send_reserved_op`. An awaited `send` remains correct
    /// only where the operation is part of a committed, ordered transition
    /// (session/provider reload) whose drop would desync engine and UI.
    pub async fn send(&self, op: Op) -> Result<()> {
        let authority = Self::change_mode_authority(&op);
        let permit = self.tx_op.clone().reserve_owned().await?;
        if let Some(authority) = authority {
            self.publish_runtime_authority(authority);
        }
        self.send_reserved_op(permit, op);
        Ok(())
    }

    /// Try to send an operation without blocking.
    ///
    /// Returns `Err` if the channel is full or closed.  Use this for
    /// non-critical, refresh-type ops (e.g. `Op::ListSubAgents`) that can
    /// safely be dropped and re-requested on the next drain cycle.
    pub fn try_send(&self, op: Op) -> Result<()> {
        let authority = Self::change_mode_authority(&op);
        let result = self.tx_op.clone().try_reserve_owned();
        // A full channel already guarantees that the engine will wake and
        // drain an operation. Publish the typed authority anyway: the drain
        // applies pending authority before handling that queued operation, so
        // a posture edit never blocks behind refresh traffic. A closed
        // channel has no engine left to observe the update.
        if !matches!(&result, Err(mpsc::error::TrySendError::Closed(_)))
            && let Some(authority) = authority
        {
            self.publish_runtime_authority(authority);
        }
        // Keep the public error bound to the rejected operation. Callers use
        // TrySendError<Op> to distinguish a retryable full mailbox from a
        // stopped engine; reservation errors otherwise carry a Sender<Op>.
        match result {
            Ok(permit) => {
                self.send_reserved_op(permit, op);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                Err(mpsc::error::TrySendError::Full(op).into())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(mpsc::error::TrySendError::Closed(op).into())
            }
        }
    }

    /// Bind controls and enqueue under one lock, preserving the same FIFO as
    /// the operation mailbox even when several senders hold reserved slots.
    pub fn send_reserved_op(&self, permit: mpsc::OwnedPermit<Op>, op: Op) {
        self.send_reserved_narrowed_op(permit, op, super::host_profile::TurnNarrowing::Inherit);
    }

    /// Called only by the captured ACP Runtime admission. One lock retains the
    /// exact control/mailbox FIFO; no profile is added to the public operation.
    pub fn send_reserved_acp_op(&self, permit: mpsc::OwnedPermit<Op>, op: Op) {
        self.send_reserved_narrowed_op(permit, op, super::host_profile::TurnNarrowing::Acp);
    }

    fn send_reserved_narrowed_op(
        &self,
        permit: mpsc::OwnedPermit<Op>,
        op: Op,
        narrowing: super::host_profile::TurnNarrowing,
    ) {
        let mut controls = self
            .turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(&op, Op::SendMessage(_) | Op::Review { .. }) {
            let mut control = controls.fresh();
            control.narrowing = narrowing;
            controls.pending.push_back(control);
        }
        permit.send(op);
    }

    fn change_mode_authority(op: &Op) -> Option<LiveRuntimeAuthority> {
        let Op::ChangeMode {
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
            configured_sandbox_mode,
        } = op
        else {
            return None;
        };
        Some(LiveRuntimeAuthority::from_fields(
            *mode,
            *allow_shell,
            *trust_mode,
            *auto_approve,
            *approval_mode,
            configured_sandbox_mode.clone(),
        ))
    }

    fn publish_runtime_authority(&self, authority: LiveRuntimeAuthority) {
        let mut state = self
            .live_runtime_authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.revision = state.revision.wrapping_add(1).max(1);
        state.authority = authority;
    }

    pub fn publish_turn_authority(
        &self,
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
        configured_sandbox_mode: Option<String>,
    ) {
        self.publish_runtime_authority(LiveRuntimeAuthority::from_fields(
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
            configured_sandbox_mode,
        ));
    }

    /// Exact live permission authority for runtime approval and elevation
    /// gates. This is the same typed state the active engine turn drains.
    #[must_use]
    pub fn runtime_permission_authority(&self) -> RuntimePermissionAuthority {
        self.live_runtime_authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .authority
            .permission_snapshot()
    }

    /// Reserve capacity for a runtime steer before it mutates durable state.
    /// The owned permit lets the caller persist and dispatch synchronously,
    /// without a cancellation point between those two operations.
    pub async fn reserve_steer(&self) -> Result<SteerPermit> {
        let permit = self.tx_steer.clone().reserve_owned().await?;
        let turn_id = self
            .turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target()
            .map(|control| control.id);
        Ok(SteerPermit { permit, turn_id })
    }

    /// Cancel the current request (user-initiated path — keeps the
    /// public `cancel()` signature stable). Equivalent to
    /// `cancel_with_reason(CancelReason::User)`.
    pub fn cancel(&self) {
        self.cancel_with_reason(CancelReason::User);
    }

    /// Cancel the current request and latch the reason so downstream
    /// "request cancelled" error messages can name a cause.
    pub fn cancel_with_reason(&self, reason: CancelReason) {
        // Keep turn activation excluded until both the admitted control and
        // the legacy shared token have been canceled.
        let controls = self
            .turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(control) = controls.target() {
            *control
                .reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason);
            control.cancel.cancel();
        }
        match self.cancel_reason.lock() {
            Ok(mut slot) => *slot = Some(reason),
            Err(poisoned) => *poisoned.into_inner() = Some(reason),
        }
        match self.cancel_token.lock() {
            Ok(token) => token.cancel(),
            Err(poisoned) => poisoned.into_inner().cancel(),
        }
        codewhale_runtime::retry_status::clear();
        #[cfg(any(test, feature = "test-support"))]
        if let Some(sources) = self.mock_approval_publications.as_ref() {
            let pending = sources.lock().drain().collect::<Vec<_>>();
            let runtime = tokio::runtime::Handle::try_current().ok();
            for (id, (source, producer)) in pending {
                let receipt = crate::approval_log::ApprovalReceipt::decided_with(
                    id,
                    crate::approval_log::ApprovalOutcome::Cancelled,
                    Some(ApprovalDecider::Host),
                );
                let committed = source.commit_mock_outcome(receipt).unwrap_or(false);
                if let Some(runtime) = runtime.as_ref() {
                    runtime.spawn(async move {
                        let _producer = producer;
                        if committed {
                            let _ = source.published().await;
                        }
                    });
                }
            }
        }
    }

    /// Snapshot the exact existing target control for one captured transport
    /// wait. It cannot follow a later turn or reset the admitted token.
    pub fn captured_turn_cancel(&self) -> CancellationToken {
        self.turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target()
            .map(|control| control.cancel.clone())
            .unwrap_or_else(|| {
                self.cancel_token
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
    }

    /// Check if a request is currently cancelled
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        if let Some(control) = self
            .turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target()
        {
            return control.cancel.is_cancelled();
        }
        match self.cancel_token.lock() {
            Ok(token) => token.is_cancelled(),
            Err(poisoned) => poisoned.into_inner().is_cancelled(),
        }
    }

    /// Pause or resume the current pausable command.
    pub fn set_paused(&self, paused: bool) {
        match self.shared_paused.lock() {
            Ok(mut slot) => *slot = paused,
            Err(poisoned) => *poisoned.into_inner() = paused,
        }
    }

    /// Check whether the engine pause gate is set.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn is_paused(&self) -> bool {
        match self.shared_paused.lock() {
            Ok(slot) => *slot,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    async fn send_approval(&self, decision: ApprovalDecision) -> Result<()> {
        let current = self
            .current_approval
            .lock()
            .as_ref()
            .filter(|source| source.is_for(decision.id()))
            .cloned();
        if let Some(source) = current {
            return self.deliver_source_approval(decision, source).await;
        }
        if crate::tools::subagent::SubAgentManager::is_child_approval_id(decision.id()) {
            use crate::tools::subagent::ChildApprovalOutcome;
            let (outcome, grant) = match &decision {
                ApprovalDecision::Approved { by, grant, .. } => {
                    (ChildApprovalOutcome::Approved { by: *by }, grant.clone())
                }
                ApprovalDecision::Denied { by, grant, .. } => {
                    (ChildApprovalOutcome::Denied { by: *by }, grant.clone())
                }
                ApprovalDecision::TimedOut { .. } => (ChildApprovalOutcome::TimedOut, None),
                ApprovalDecision::Unavailable { .. } => (ChildApprovalOutcome::Unavailable, None),
                ApprovalDecision::RetryWithPolicy { .. } => {
                    anyhow::bail!("child approval does not offer a sandbox retry")
                }
            };
            let submission = self.subagent_manager.write().await.submit_child_approval(
                decision.id(),
                outcome,
                grant,
            );
            if submission.wait().await? {
                return Ok(());
            }
            #[cfg(any(test, feature = "test-support"))]
            anyhow::ensure!(
                self.mock_approval_publications.is_some(),
                "child approval is no longer pending or resolved differently"
            );
            #[cfg(not(any(test, feature = "test-support")))]
            anyhow::bail!("child approval is no longer pending or resolved differently");
        }
        #[cfg(any(test, feature = "test-support"))]
        if let Some(sources) = self.mock_approval_publications.as_ref() {
            let mock = sources
                .lock()
                .get(decision.id())
                .map(|(source, _)| source.clone());
            if let Some(source) = mock {
                let receipt = decision.receipt();
                if source.commit_mock_outcome(receipt)? {
                    let actual = source.actual().await?;
                    let observed = if decision.require_actual(&actual).is_ok() {
                        Some(decision.clone())
                    } else if matches!(
                        actual,
                        crate::approval_log::ApprovalReceipt::Decided {
                            outcome: crate::approval_log::ApprovalOutcome::Timeout,
                            ..
                        }
                    ) {
                        Some(ApprovalDecision::TimedOut {
                            id: decision.id().to_owned(),
                        })
                    } else {
                        None
                    };
                    if let Some(observed) = observed {
                        self.tx_approval
                            .send(SubmittedApproval {
                                decision: observed,
                                publication: Some(source.clone()),
                            })
                            .await?;
                    }
                }
                return decision.require_actual(&source.published().await?);
            }
            let (source, _producer) = super::NativeApprovalPublication::new(decision.id());
            source.set_mock_controls(self.captured_turn_cancel(), None);
            source.commit_mock_outcome(decision.receipt())?;
            decision.require_actual(&source.published().await?)?;
            self.tx_approval
                .send(SubmittedApproval {
                    decision,
                    publication: Some(source),
                })
                .await?;
            return Ok(());
        }
        anyhow::bail!("approval has no current held Core request")
    }

    async fn deliver_source_approval(
        &self,
        decision: ApprovalDecision,
        publication: super::NativeApprovalPublication,
    ) -> Result<()> {
        anyhow::ensure!(
            publication.is_for(decision.id()),
            "approval source belongs to another Core request"
        );
        let current = self
            .current_approval
            .lock()
            .as_ref()
            .is_some_and(|source| source.same_request(&publication));
        if !current {
            return decision.require_actual(&publication.published().await?);
        }
        if matches!(decision, ApprovalDecision::TimedOut { .. }) {
            anyhow::ensure!(
                publication.timeout_seconds().is_some(),
                "Core approval has no configured deadline"
            );
            return decision.require_actual(&publication.published().await?);
        }
        let submitted = SubmittedApproval {
            decision: decision.clone(),
            publication: Some(publication.clone()),
        };
        let receipt = tokio::select! {
            biased;
            receipt = publication.published() => receipt?,
            sent = self.tx_approval.send(submitted) => {
                sent?;
                publication.published().await?
            }
        };
        decision.require_actual(&receipt)
    }

    pub(crate) async fn accept_child_approval(
        &self,
        id: String,
        reply: crate::tools::subagent::ChildApprovalReply,
    ) -> Result<()> {
        use crate::tools::subagent::ChildApprovalOutcome;
        let decision = match reply.outcome {
            ChildApprovalOutcome::Approved { by } => ApprovalDecision::Approved {
                id,
                by,
                grant: reply.grant,
            },
            ChildApprovalOutcome::Denied { by } => ApprovalDecision::Denied {
                id,
                by,
                grant: reply.grant,
            },
            ChildApprovalOutcome::TimedOut => ApprovalDecision::TimedOut { id },
            ChildApprovalOutcome::Unavailable => ApprovalDecision::Unavailable { id },
        };
        if let Some(publication) = reply.publication {
            self.deliver_source_approval(decision, publication).await
        } else {
            anyhow::bail!("child approval reply has no captured Core source")
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn has_mock_approval_publications(&self) -> bool {
        self.mock_approval_publications.is_some()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn mock_approval_publication(
        &self,
        tool_id: &str,
        timeout: Option<std::time::Duration>,
    ) -> Option<super::NativeApprovalPublication> {
        let sources = self.mock_approval_publications.as_ref()?.clone();
        let (source, producer) = super::NativeApprovalPublication::new(tool_id);
        source.set_mock_controls(self.captured_turn_cancel(), timeout);
        {
            let mut pending = sources.lock();
            if pending.contains_key(tool_id) {
                return None;
            }
            pending.insert(tool_id.to_owned(), (source.clone(), producer));
        }
        if let Some(timeout) = timeout {
            let source = source.clone();
            let tool_id = tool_id.to_owned();
            let approvals = self.tx_approval.clone();
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                let receipt = crate::approval_log::ApprovalReceipt::decided_with(
                    &tool_id,
                    crate::approval_log::ApprovalOutcome::Timeout,
                    None,
                );
                if source.commit_mock_outcome(receipt).unwrap_or(false) {
                    let _ = source.published().await;
                    if matches!(
                        source.actual().await,
                        Ok(crate::approval_log::ApprovalReceipt::Decided {
                            outcome: crate::approval_log::ApprovalOutcome::Timeout,
                            ..
                        })
                    ) {
                        let _ = approvals
                            .send(SubmittedApproval {
                                decision: ApprovalDecision::TimedOut {
                                    id: tool_id.clone(),
                                },
                                publication: Some(source.clone()),
                            })
                            .await;
                    }
                    let mut pending = sources.lock();
                    if pending
                        .get(&tool_id)
                        .is_some_and(|(current, _)| current.same_request(&source))
                    {
                        pending.remove(&tool_id);
                    }
                }
            });
        }
        Some(source)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn retire_mock_approval_publication(
        &self,
        tool_id: &str,
        source: &super::NativeApprovalPublication,
    ) {
        if let Some(sources) = self.mock_approval_publications.as_ref() {
            let mut pending = sources.lock();
            if pending
                .get(tool_id)
                .is_some_and(|(current, _)| current.same_request(source))
            {
                pending.remove(tool_id);
            }
        }
    }

    /// Approve a pending tool call because a person said yes.
    pub async fn approve_tool_call(&self, id: impl Into<String>) -> Result<()> {
        self.approve_tool_call_by(id, ApprovalDecider::User).await
    }

    /// Approve a pending tool call, recording who answered: a person, a
    /// session rule, or the active posture. The approval receipt keeps it.
    pub async fn approve_tool_call_by(
        &self,
        id: impl Into<String>,
        by: ApprovalDecider,
    ) -> Result<()> {
        self.send_approval(ApprovalDecision::Approved {
            id: id.into(),
            by,
            grant: None,
        })
        .await
    }

    /// Approve a pending tool call because a standing grant matched; the
    /// approval receipt names the grant.
    pub async fn approve_tool_call_by_grant(
        &self,
        id: impl Into<String>,
        by: ApprovalDecider,
        grant: crate::core::authority::grants::GrantRef,
    ) -> Result<()> {
        self.send_approval(ApprovalDecision::Approved {
            id: id.into(),
            by,
            grant: Some(grant),
        })
        .await
    }

    /// Deny a pending tool call because a person said no.
    pub async fn deny_tool_call(&self, id: impl Into<String>) -> Result<()> {
        self.deny_tool_call_by(id, ApprovalDecider::User).await
    }

    /// Deny a pending tool call, recording who answered.
    pub async fn deny_tool_call_by(
        &self,
        id: impl Into<String>,
        by: ApprovalDecider,
    ) -> Result<()> {
        self.send_approval(ApprovalDecision::Denied {
            id: id.into(),
            by,
            grant: None,
        })
        .await
    }

    /// Deny a pending tool call because a standing deny grant matched.
    pub async fn deny_tool_call_by_grant(
        &self,
        id: impl Into<String>,
        by: ApprovalDecider,
        grant: crate::core::authority::grants::GrantRef,
    ) -> Result<()> {
        self.send_approval(ApprovalDecision::Denied {
            id: id.into(),
            by,
            grant: Some(grant),
        })
        .await
    }

    /// Deny a pending tool call because its interactive approval card
    /// expired (#6101). Kept distinct from [`Self::deny_tool_call`] so the
    /// receipt records a timeout instead of an operator denial.
    pub async fn deny_tool_call_timed_out(&self, id: impl Into<String>) -> Result<()> {
        self.send_approval(ApprovalDecision::TimedOut { id: id.into() })
            .await
    }

    /// Resolve a pending tool call whose request could not be put in front
    /// of a person (a stale turn's request, or a child from another
    /// conversation). Kept distinct from [`Self::deny_tool_call`] so neither
    /// the receipt nor the model message claims the person denied it.
    pub async fn deny_tool_call_unavailable(&self, id: impl Into<String>) -> Result<()> {
        self.send_approval(ApprovalDecision::Unavailable { id: id.into() })
            .await
    }

    /// Retry a tool call with an elevated sandbox policy a person chose.
    pub async fn retry_tool_with_policy(
        &self,
        id: impl Into<String>,
        policy: codewhale_sandbox::SandboxPolicy,
    ) -> Result<()> {
        self.retry_tool_with_policy_by(id, policy, ApprovalDecider::User)
            .await
    }

    /// Retry a tool call with an elevated sandbox policy, recording who chose it.
    pub async fn retry_tool_with_policy_by(
        &self,
        id: impl Into<String>,
        policy: codewhale_sandbox::SandboxPolicy,
        by: ApprovalDecider,
    ) -> Result<()> {
        self.send_approval(ApprovalDecision::RetryWithPolicy {
            id: id.into(),
            policy,
            by,
        })
        .await
    }

    /// Submit a response for request_user_input. Success means Engine accepted
    /// it for the live exact request, rather than merely queuing the response.
    pub async fn submit_user_input(
        &self,
        id: impl Into<String>,
        response: UserInputResponse,
    ) -> Result<()> {
        let (accepted_tx, accepted_rx) = oneshot::channel();
        await_user_input_acceptance(
            &self.tx_user_input,
            UserInputDecision::Submitted {
                id: id.into(),
                response,
                accepted: accepted_tx,
            },
            accepted_rx,
            USER_INPUT_ACK_TIMEOUT,
        )
        .await
    }

    /// Cancel a request_user_input prompt.
    pub async fn cancel_user_input(&self, id: impl Into<String>) -> Result<()> {
        let (accepted_tx, accepted_rx) = oneshot::channel();
        await_user_input_acceptance(
            &self.tx_user_input,
            UserInputDecision::Cancelled {
                id: id.into(),
                accepted: accepted_tx,
            },
            accepted_rx,
            USER_INPUT_ACK_TIMEOUT,
        )
        .await
    }

    /// Steer an in-flight turn with additional user input.
    pub async fn steer(&self, content: impl Into<String>) -> Result<()> {
        self.reserve_steer().await?.send(content.into());
        Ok(())
    }

    /// Request the live context-window budget for this session's route.
    /// `None` means the route cannot express a bounded window (e.g. an
    /// unknown model with no catalog or configured limits) — callers should
    /// surface "unavailable" rather than inventing a number.
    pub async fn get_context_budget(
        &self,
    ) -> Result<Option<crate::core::ops::SessionContextBudget>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        self.send(Op::GetContextBudget { tx }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Engine dropped context budget oneshot"))
    }

    /// Request a snapshot of the current session state.
    /// Returns the snapshot directly via a oneshot channel, avoiding
    /// competition with the SSE event stream on the mpsc receiver.
    pub async fn get_session_snapshot(&self) -> Result<crate::core::ops::SessionSnapshot> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        self.send(Op::GetSessionSnapshot { tx }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Engine dropped session snapshot oneshot"))
    }

    /// Query after the active turn settles, without competing with events.
    /// The caller must keep draining events and bound this future: an active
    /// turn can be awaiting provider/tool work or a full event channel.
    pub async fn get_subagent_settlement(&self) -> Result<crate::core::ops::SubAgentSettlement> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Arc::new(StdMutex::new(Some(tx)));
        self.send(Op::GetSubAgentSettlement { tx }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Engine dropped child settlement receipt"))
    }

    /// Request active provider request concurrency state.
    pub async fn get_provider_runtime_status(
        &self,
    ) -> Result<crate::core::ops::ProviderRuntimeStatus> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        self.send(Op::GetProviderRuntimeStatus { tx }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Engine dropped provider runtime status oneshot"))
    }

    /// Run the bounded initial connection pass on the engine-owned MCP pool.
    ///
    /// The returned manager snapshot and every later tool call therefore see
    /// the same connections and catalog generation. Unlike `reload_mcp`, this
    /// does not force a config re-read or drop ready transports. Optional
    /// servers are connected in the background at engine spawn; this waits
    /// only if the caller explicitly asked for the settled receipt.
    pub async fn bootstrap_mcp(&self) -> Result<crate::core::ops::McpManagerUpdate> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        self.send(Op::BootstrapMcp { tx }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Engine dropped MCP bootstrap oneshot"))?
            .map_err(anyhow::Error::msg)
    }

    /// Retry one failed server through the existing engine-owned pool.
    pub async fn retry_mcp_server(
        &self,
        name: impl Into<String>,
    ) -> Result<crate::core::ops::McpManagerUpdate> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        self.send(Op::RetryMcpServer {
            name: name.into(),
            tx,
        })
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Engine dropped MCP retry oneshot"))?
            .map_err(anyhow::Error::msg)
    }

    /// Force the engine-owned MCP pool to reload and reconnect, returning a
    /// snapshot from the exact live pool that supplies the next model turn.
    pub async fn reload_mcp(
        &self,
        config_path: std::path::PathBuf,
    ) -> Result<crate::core::ops::McpManagerUpdate> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        self.send(Op::ReloadMcp { config_path, tx }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Engine dropped MCP reload oneshot"))?
            .map_err(anyhow::Error::msg)
    }
}

#[cfg(test)]
mod user_input_ack_tests {
    use super::*;

    #[tokio::test]
    async fn user_input_delivery_bound_rejects_abandoned_reply_and_full_mailbox() {
        for full in [false, true] {
            let (sender, mut mailbox) = mpsc::channel(1);
            if full {
                let (accepted, receiver) = oneshot::channel();
                drop(receiver);
                sender
                    .send(UserInputDecision::Cancelled {
                        id: "blocker".into(),
                        accepted,
                    })
                    .await
                    .unwrap();
            }
            let (accepted, verdict) = oneshot::channel();
            let result = await_user_input_acceptance(
                &sender,
                UserInputDecision::Submitted {
                    id: "late-reply".into(),
                    response: UserInputResponse {
                        answers: Vec::new(),
                    },
                    accepted,
                },
                verdict,
                std::time::Duration::from_millis(10),
            )
            .await;
            assert!(result.is_err());
            match mailbox.try_recv().unwrap() {
                UserInputDecision::Submitted { id, accepted, .. } => {
                    assert!(!full);
                    assert_eq!(id, "late-reply");
                    assert!(
                        accepted.send(true).is_err(),
                        "timed-out reply cannot be accepted later"
                    );
                }
                UserInputDecision::Cancelled { id, .. } => {
                    assert!(full);
                    assert_eq!(id, "blocker");
                }
            }
            assert!(
                mailbox.try_recv().is_err(),
                "a full mailbox must not receive the abandoned reply"
            );
        }
    }
}
