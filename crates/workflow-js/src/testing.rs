//! Test support: a scriptable in-memory [`WorkflowDriver`].
//!
//! [`FakeDriver`] records every [`TaskRequest`] and [`ProgressEvent`] it
//! receives, answers spawns from substring-matched reply rules (with optional
//! delays for ordering tests), and counts `cancel_all` calls. It exists so
//! this crate — and the tui wiring that implements the real driver — can be
//! exercised without spawning a single real subagent.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::driver::{
    BudgetSnapshot, ProgressEvent, SpawnedTask, TaskCompletion, TaskRequest, WorkflowDriver,
};
use crate::error::DriverError;

/// How the fake answers a matched spawn.
#[derive(Debug, Clone)]
pub enum FakeReply {
    /// Resolve with this full result text.
    Complete(String),
    /// Resolve as a failed subagent.
    Fail(String),
    /// Resolve as cancelled.
    Cancelled,
    /// Resolve as budget-exhausted mid-flight.
    BudgetExhausted(String),
    /// Refuse admission: `spawn_task` returns [`DriverError::Rejected`].
    Reject(String),
    /// The driver seam is gone: `spawn_task` returns
    /// [`DriverError::Unavailable`].
    Unavailable(String),
    /// Admit the task, then drop the completion sender without ever sending a
    /// terminal outcome — the "driver dropped the completion channel" path.
    DropCompletion,
    /// Admit the task but never complete it (for cancellation tests). The
    /// completion sender is held so the channel stays open.
    Never,
    /// Never finish admission: `spawn_task` itself stays pending, like a
    /// saturated concurrency gate (for cancellation tests). The request is
    /// still recorded, so `spawn_count` shows the task reached admission.
    HoldAdmission,
}

#[derive(Debug)]
struct ReplyRule {
    needle: String,
    delay: Option<Duration>,
    reply: FakeReply,
}

#[derive(Debug, Default)]
struct Inner {
    rules: Vec<ReplyRule>,
    requests: Vec<TaskRequest>,
    events: Vec<ProgressEvent>,
    budget: BudgetSnapshot,
    spend_per_task: u64,
    next_id: u64,
    held: Vec<(String, oneshot::Sender<TaskCompletion>)>,
    cancelled: Vec<String>,
}

/// In-memory [`WorkflowDriver`] with scripted replies.
///
/// Unmatched spawns complete immediately with `done:<description>`. Rules are
/// matched by substring against the request description, first match wins.
#[derive(Debug, Default)]
pub struct FakeDriver {
    inner: Mutex<Inner>,
    cancel_calls: AtomicUsize,
}

impl FakeDriver {
    /// A fake with no rules, no budget ceiling, and echo replies.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a reply rule: requests whose description contains `needle` get
    /// `reply` immediately.
    pub fn on(&self, needle: &str, reply: FakeReply) {
        self.on_with_delay_opt(needle, reply, None);
    }

    /// Like [`FakeDriver::on`], but the completion is delivered after `delay`
    /// (the spawn itself still returns immediately).
    pub fn on_with_delay(&self, needle: &str, reply: FakeReply, delay: Duration) {
        self.on_with_delay_opt(needle, reply, Some(delay));
    }

    fn on_with_delay_opt(&self, needle: &str, reply: FakeReply, delay: Option<Duration>) {
        self.lock().rules.push(ReplyRule {
            needle: needle.to_string(),
            delay,
            reply,
        });
    }

    /// Configure the budget pool: ceiling plus a fixed spend debited at each
    /// spawn (simulating the driver-side reservation of design §5.3).
    pub fn set_budget(&self, total: Option<u64>, spend_per_task: u64) {
        let mut inner = self.lock();
        inner.budget = BudgetSnapshot { total, spent: 0 };
        inner.spend_per_task = spend_per_task;
    }

    /// Every request received so far, in spawn order.
    pub fn requests(&self) -> Vec<TaskRequest> {
        self.lock().requests.clone()
    }

    /// Descriptions of every request, in spawn order.
    pub fn request_descriptions(&self) -> Vec<String> {
        self.lock()
            .requests
            .iter()
            .map(|request| request.description.clone())
            .collect()
    }

    /// Number of admitted spawn calls.
    pub fn spawn_count(&self) -> usize {
        self.lock().requests.len()
    }

    /// Every progress event received so far, in emit order.
    pub fn events(&self) -> Vec<ProgressEvent> {
        self.lock().events.clone()
    }

    pub fn cancelled_tasks(&self) -> Vec<String> {
        self.lock().cancelled.clone()
    }

    /// How many times `cancel_all` has been invoked.
    pub fn cancel_all_calls(&self) -> usize {
        self.cancel_calls.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("FakeDriver mutex poisoned")
    }
}

#[async_trait]
impl WorkflowDriver for FakeDriver {
    async fn spawn_task(&self, request: TaskRequest) -> Result<SpawnedTask, DriverError> {
        let (task_id, reply, delay) = {
            let mut inner = self.lock();
            let matched = inner
                .rules
                .iter()
                .find(|rule| request.description.contains(&rule.needle))
                .map(|rule| (rule.reply.clone(), rule.delay));
            let (reply, delay) = matched.unwrap_or_else(|| {
                (
                    FakeReply::Complete(format!("done:{}", request.description)),
                    None,
                )
            });
            match reply {
                FakeReply::Reject(message) => return Err(DriverError::Rejected(message)),
                FakeReply::Unavailable(message) => {
                    return Err(DriverError::Unavailable(message));
                }
                _ => {}
            }
            inner.requests.push(request);
            inner.budget.spent += inner.spend_per_task;
            inner.next_id += 1;
            (format!("agent_{:04}", inner.next_id), reply, delay)
        };
        if matches!(reply, FakeReply::HoldAdmission) {
            return std::future::pending().await;
        }

        let (tx, rx) = oneshot::channel();
        match reply {
            FakeReply::Never => self.lock().held.push((task_id.clone(), tx)),
            FakeReply::DropCompletion => drop(tx),
            reply => {
                let completion = match reply {
                    FakeReply::Complete(text) => TaskCompletion::Completed { text },
                    FakeReply::Fail(message) => TaskCompletion::Failed { message },
                    FakeReply::Cancelled => TaskCompletion::Cancelled,
                    FakeReply::BudgetExhausted(message) => {
                        TaskCompletion::BudgetExhausted { message }
                    }
                    FakeReply::Reject(_)
                    | FakeReply::Unavailable(_)
                    | FakeReply::Never
                    | FakeReply::HoldAdmission
                    | FakeReply::DropCompletion => unreachable!("handled above"),
                };
                match delay {
                    None => {
                        let _ = tx.send(completion);
                    }
                    Some(delay) => {
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = tx.send(completion);
                        });
                    }
                }
            }
        }
        Ok(SpawnedTask {
            task_id,
            completion: rx,
        })
    }

    fn cancel_all(&self) {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
    }

    fn cancel_task(&self, task_id: &str) {
        let mut inner = self.lock();
        inner.cancelled.push(task_id.to_string());
        if let Some(index) = inner.held.iter().position(|(id, _)| id == task_id) {
            let (_, tx) = inner.held.remove(index);
            let _ = tx.send(TaskCompletion::Cancelled);
        }
    }

    fn budget(&self) -> BudgetSnapshot {
        self.lock().budget
    }

    fn progress(&self, event: ProgressEvent) {
        self.lock().events.push(event);
    }
}
