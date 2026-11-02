//! Rate-limit aware adaptive scheduling for sub-agent fan-out ("swarm mode").
//!
//! A swarm can launch an unbounded number of sub-agents against one shared
//! LLM provider, so parallel 429s are the steady state rather than an edge
//! case. This module gives the sub-agent module two cooperating pieces:
//!
//! 1. [`DynamicGate`] — a launch gate with a *dynamically adjustable
//!    capacity*. The previous gate was a `tokio::sync::Semaphore`, whose
//!    capacity is fixed at construction; the only way to "shrink" it was to
//!    replace the `Arc`, which silently fails while any child still holds a
//!    permit (that is exactly why `update_runtime_limits` only applied
//!    launch-concurrency changes when no sub-agent was running). A
//!    custom gate can drop its capacity below the number of active holders:
//!    existing children keep running to completion, while new admissions
//!    block until `active < capacity`.
//!
//! 2. [`RateLimitGovernor`] — a sliding-window observer fed by the sub-agent
//!    LLM call path. Every rate-limited attempt and every successful attempt
//!    is reported; when the recent failure rate crosses a threshold the
//!    governor shrinks the gate (multiplicative decrease), and under a
//!    sustained burst it pauses new admissions entirely. Sustained success
//!    recovers capacity additively (AIMD), which converges without the
//!    oscillation a symmetric controller would show.
//!
//! Retries themselves stay in the LLM call path (see
//! the canonical Engine model-step dispatch): the governor never
//! delays an in-flight call, it only decides whether *new* launches may be
//! admitted. `QuotaExhausted` is deliberately not reported — quota is a
//! billing condition, not a transient throttle, and must keep following the
//! existing fatal/checkpoint path.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

/// Observation window for rate-limit events. Events older than this are
/// pruned on every governor interaction.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// Rate-limit events inside [`RATE_LIMIT_WINDOW`] at which the governor
/// starts shrinking launch concurrency (AIMD multiplicative decrease).
const THROTTLE_EVENT_THRESHOLD: usize = 2;

/// Recent rate-limit *ratio* (limited attempts / attempts) at which the
/// governor also shrinks launch concurrency, even below the absolute count
/// threshold. With very few in-flight calls, two 429s may be 100% of traffic.
const THROTTLE_RATIO_THRESHOLD: f64 = 0.3;

/// Rate-limit events inside the window at which the governor pauses new
/// admissions entirely (gate capacity 0). Held permits are unaffected.
const PAUSE_EVENT_THRESHOLD: usize = 4;

/// Successful attempts required to add one unit of launch capacity back
/// (AIMD additive increase). Successes are counted per gate-holder, so a
/// shrunken fleet still recovers at a controlled pace.
const SUCCESS_PER_INCREASE_STEP: u32 = 3;

/// Full-jitter exponential backoff for a rate-limited sub-agent API attempt
/// (`retry_number` is 1-based): the raw backoff is
/// `initial * 2^(n-1)` capped at [`RATE_LIMIT_MAX_BACKOFF`], and the actual
/// delay is drawn uniformly from `[0, backoff)` (AWS "full jitter"). Full
/// jitter de-synchronizes a fan-out of children that were all 429'd by the
/// same provider response; the cap keeps a retrying child inside its
/// wall-time budget instead of giving up.
const RATE_LIMIT_MAX_BACKOFF: Duration = Duration::from_secs(120);
const RATE_LIMIT_BACKOFF_JITTER_FACTOR: f64 = 1.0; // full jitter

/// Uniformly random factor in `[0, 1)` derived from UUID v4 entropy, the
/// same idiom as `llm_client::RetryConfig::delay_for_attempt`.
fn random_unit_factor() -> f64 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let sample = u16::from_le_bytes([bytes[0], bytes[1]]);
    f64::from(sample) / f64::from(u16::MAX)
}

/// Raw (pre-jitter) exponential backoff for a rate-limited attempt.
fn rate_limit_backoff_base(retry_number: u32) -> Duration {
    let multiplier = 1u32
        .checked_shl(retry_number.saturating_sub(1))
        .unwrap_or(u32::MAX);
    Duration::from_millis(250)
        .saturating_mul(multiplier)
        .min(RATE_LIMIT_MAX_BACKOFF)
}

/// Full-jitter retry delay for a rate-limited attempt.
pub fn rate_limit_retry_delay(retry_number: u32) -> Duration {
    let base = rate_limit_backoff_base(retry_number).as_secs_f64();
    // Full jitter: uniform in [0, base). Reaching exactly `base` is fine and
    // only sharpens de-synchronization; the draw can never exceed it.
    Duration::from_secs_f64(base * (1.0 - RATE_LIMIT_BACKOFF_JITTER_FACTOR * random_unit_factor()))
}

// === DynamicGate ===

#[derive(Debug)]
struct GateWaiter {
    sender: oneshot::Sender<DynamicGatePermit>,
}

#[derive(Debug)]
struct GateInner {
    capacity: usize,
    active: usize,
    waiters: VecDeque<GateWaiter>,
}

/// A launch gate with runtime-adjustable capacity (see module docs).
///
/// `acquire` returns a [`DynamicGatePermit`] whose `Drop` releases the slot
/// and wakes one waiter. Reducing capacity below `active` is allowed: the
/// surplus holders finish naturally and no new permit is granted until the
/// active count drops under the new capacity.
///
/// Waiters receive an *already granted* permit through a oneshot channel, so
/// a waiter future that is cancelled after the grant is dispatched simply
/// drops the permit, whose `Drop` hands the slot to the next waiter. (A
/// wake-and-recheck design would lose that wakeup — the cancelled waiter
/// never re-checks, and with no remaining holders there is no later release
/// to re-dispatch it.)
#[derive(Debug)]
pub struct DynamicGate {
    inner: Mutex<GateInner>,
}

impl DynamicGate {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(GateInner {
                capacity: capacity.max(1),
                active: 0,
                waiters: VecDeque::new(),
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.lock().expect("launch gate poisoned").capacity
    }

    /// Free admission slots right now (`capacity - active`). Diagnostics and
    /// tests only; racy by design.
    #[cfg(any(test, feature = "test-support"))]
    pub fn available_permits(&self) -> usize {
        let inner = self.inner.lock().expect("launch gate poisoned");
        inner.capacity.saturating_sub(inner.active)
    }

    /// Adjust the gate capacity. Raising it grants queued waiters the new
    /// headroom immediately; lowering it simply stops new admissions until
    /// the active count drains below the new capacity.
    pub fn set_capacity(self: &std::sync::Arc<Self>, capacity: usize) {
        let mut inner = self.inner.lock().expect("launch gate poisoned");
        inner.capacity = capacity;
        Self::wake_locked(self, &mut inner);
    }

    /// Grant queued waiters while there is headroom. Called with the lock
    /// held; each waiter receives an already-counted permit, so a cancelled
    /// receiver's permit is disarmed (never `Drop`ped under the lock) and the
    /// slot flows to the next waiter.
    fn wake_locked(gate: &std::sync::Arc<Self>, inner: &mut GateInner) {
        while inner.active < inner.capacity {
            let Some(waiter) = inner.waiters.pop_front() else {
                break;
            };
            let permit = DynamicGatePermit {
                gate: Some(std::sync::Arc::clone(gate)),
            };
            match waiter.sender.send(permit) {
                Ok(()) => inner.active += 1,
                Err(mut returned) => {
                    // The waiter future was cancelled before receiving the
                    // grant. Disarm instead of dropping: `Drop` would call
                    // `release()` and re-enter the lock we are holding.
                    let _ = returned.disarm();
                }
            }
        }
    }

    fn release(self: &std::sync::Arc<Self>) {
        let mut inner = self.inner.lock().expect("launch gate poisoned");
        inner.active = inner.active.saturating_sub(1);
        Self::wake_locked(self, &mut inner);
    }

    /// Try to acquire a permit without waiting.
    pub fn try_acquire(self: &std::sync::Arc<Self>) -> Option<DynamicGatePermit> {
        let mut inner = self.inner.lock().expect("launch gate poisoned");
        (inner.active < inner.capacity).then(|| {
            inner.active += 1;
            DynamicGatePermit {
                gate: Some(std::sync::Arc::clone(self)),
            }
        })
    }

    /// Acquire a permit, waiting until capacity is available. Cancellation
    /// safe: a dropped future either leaves a stale queue entry (skipped and
    /// disarmed by the granter) or drops an already-dispatched permit (whose
    /// `Drop` re-releases the slot).
    pub async fn acquire(self: &std::sync::Arc<Self>) -> DynamicGatePermit {
        loop {
            let rx = {
                let mut inner = self.inner.lock().expect("launch gate poisoned");
                if inner.active < inner.capacity {
                    inner.active += 1;
                    return DynamicGatePermit {
                        gate: Some(std::sync::Arc::clone(self)),
                    };
                }
                let (tx, rx) = oneshot::channel();
                inner.waiters.push_back(GateWaiter { sender: tx });
                rx
            };
            // Defensive: a failed receive requires the queued sender to be
            // dropped without sending — which requires the gate itself to be
            // dropped, impossible while this future holds an `Arc` to it.
            // Loop anyway so a future refactor that breaks that invariant
            // degrades to re-queueing instead of unwrapping.
            if let Ok(permit) = rx.await {
                return permit;
            }
        }
    }
}

/// One held launch slot. Released on drop.
///
/// The gate is an `Option` so the wake path can disarm a permit whose
/// receiver vanished without running `Drop` (which would re-enter the locked
/// `release()`).
pub struct DynamicGatePermit {
    gate: Option<std::sync::Arc<DynamicGate>>,
}

impl DynamicGatePermit {
    fn disarm(&mut self) -> Option<std::sync::Arc<DynamicGate>> {
        self.gate.take()
    }
}

impl std::fmt::Debug for DynamicGatePermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicGatePermit").finish()
    }
}

impl Drop for DynamicGatePermit {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
    }
}

// === RateLimitGovernor ===

#[derive(Debug)]
struct GovernorState {
    /// Ceiling additive increase may climb to (configured launch
    /// concurrency).
    max_capacity: usize,
    /// Timestamps of rate-limited attempts inside the window.
    limited: VecDeque<Instant>,
    /// Timestamps of all reported attempts inside the window (successes and
    /// rate limits) — the denominator of the recent rate-limit ratio.
    attempts: VecDeque<Instant>,
    consecutive_successes: u32,
    paused: bool,
}

/// Rate-limit aware scheduler over a [`DynamicGate`] (see module docs).
#[derive(Debug)]
pub struct RateLimitGovernor {
    gate: std::sync::Arc<DynamicGate>,
    state: Mutex<GovernorState>,
}

impl RateLimitGovernor {
    pub fn new(max_capacity: usize) -> (std::sync::Arc<Self>, std::sync::Arc<DynamicGate>) {
        let gate = std::sync::Arc::new(DynamicGate::new(max_capacity.max(1)));
        let governor = std::sync::Arc::new(Self {
            gate: std::sync::Arc::clone(&gate),
            state: Mutex::new(GovernorState {
                max_capacity: max_capacity.max(1),
                limited: VecDeque::new(),
                attempts: VecDeque::new(),
                consecutive_successes: 0,
                paused: false,
            }),
        });
        (governor, gate)
    }

    /// The governor's launch gate. `SubAgentManager` hands this to spawned
    /// tasks in place of the old fixed `Semaphore`. (Directly exercised by
    /// governor unit tests.)
    #[cfg(any(test, feature = "test-support"))]
    pub fn gate(&self) -> std::sync::Arc<DynamicGate> {
        std::sync::Arc::clone(&self.gate)
    }

    /// Apply a new configured launch capacity: the AIMD ceiling and the gate
    /// capacity while not throttled. Applies to the live gate immediately
    /// (raising and lowering alike) unless the governor is paused — a pause
    /// keeps capacity 0 until recovery, so an external limit change cannot
    /// silently lift a rate-limit pause.
    pub fn set_max_capacity(&self, max_capacity: usize) {
        let mut state = self.state.lock().expect("rate limit governor poisoned");
        state.max_capacity = max_capacity.max(1);
        if !state.paused {
            self.gate.set_capacity(state.max_capacity);
        }
    }

    fn prune(state: &mut GovernorState, now: Instant) {
        while state
            .limited
            .front()
            .is_some_and(|at| now.duration_since(*at) > RATE_LIMIT_WINDOW)
        {
            state.limited.pop_front();
        }
        while state
            .attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) > RATE_LIMIT_WINDOW)
        {
            state.attempts.pop_front();
        }
    }

    /// Report that a sub-agent LLM attempt is starting. Contributes to the
    /// recent-attempt denominator for the ratio heuristic.
    pub fn record_attempt(&self, now: Instant) {
        let mut state = self.state.lock().expect("rate limit governor poisoned");
        Self::prune(&mut state, now);
        state.attempts.push_back(now);
    }

    /// Lift a pause whose rate-limit events have all aged out of the window,
    /// resuming at a conservative quarter of the configured capacity so
    /// additive increase climbs the rest of the way. Callers must hold the
    /// state lock; `prune` first.
    fn unpause_if_window_drained(&self, state: &mut GovernorState) {
        if !state.paused || !state.limited.is_empty() {
            return;
        }
        state.paused = false;
        let capacity = (state.max_capacity / 4).max(1);
        self.gate.set_capacity(capacity);
        tracing::info!(
            target: "subagent",
            launch_capacity = capacity,
            max_capacity = state.max_capacity,
            "rate-limit governor resumed launches after window drained"
        );
    }

    /// Time-driven recovery probe for queued launches. A pause is normally
    /// lifted by a successful LLM attempt from an in-flight child, but if the
    /// entire in-flight fleet finishes while 429 events are still inside the
    /// window, no success ever arrives — without this probe the queue would
    /// freeze until each queued child hits its wall-time deadline. Once every
    /// limit event has aged out, the next probe resumes launches.
    pub fn recover_if_window_drained(&self, now: Instant) {
        let mut state = self.state.lock().expect("rate limit governor poisoned");
        Self::prune(&mut state, now);
        self.unpause_if_window_drained(&mut state);
    }

    /// Report a successful sub-agent LLM attempt. Drives AIMD additive
    /// increase and clears the pause once the window has drained.
    pub fn record_success(&self, now: Instant) {
        let mut state = self.state.lock().expect("rate limit governor poisoned");
        Self::prune(&mut state, now);
        state.consecutive_successes = state.consecutive_successes.saturating_add(1);

        self.unpause_if_window_drained(&mut state);

        if !state.paused
            && state.consecutive_successes >= SUCCESS_PER_INCREASE_STEP
            && self.gate.capacity() < state.max_capacity
        {
            state.consecutive_successes = 0;
            let capacity = (self.gate.capacity() + 1).min(state.max_capacity);
            self.gate.set_capacity(capacity);
            tracing::debug!(
                target: "subagent",
                launch_capacity = capacity,
                "rate-limit governor additively increased launch capacity"
            );
        }
    }

    /// Report a rate-limited (429) sub-agent LLM attempt. May shrink or pause
    /// the launch gate; never touches in-flight calls or retries.
    pub fn record_rate_limited(&self, now: Instant) {
        let mut state = self.state.lock().expect("rate limit governor poisoned");
        Self::prune(&mut state, now);
        state.limited.push_back(now);
        // The denominator (`attempts`) already contains this attempt — the
        // call path reports `record_attempt` before every LLM call, retries
        // included. Pushing again would double-count failures and skew the
        // ratio.
        state.consecutive_successes = 0;

        if state.paused {
            return;
        }

        let events = state.limited.len();
        let attempts = state.attempts.len().max(1);
        let ratio = f64::from(events as u32) / f64::from(attempts as u32);

        if events >= PAUSE_EVENT_THRESHOLD {
            state.paused = true;
            // Capacity 0 blocks all *new* admissions; children already holding
            // permits keep running to completion.
            self.gate.set_capacity(0);
            tracing::warn!(
                target: "subagent",
                window_events = events,
                window_attempts = attempts,
                "rate-limit governor paused new sub-agent launches (sustained provider 429s); \
                 queued children wait for the window to drain"
            );
            return;
        }

        // The ratio heuristic only fires once the window has real volume
        // (>= 2 observed attempts): with a single attempt every 429 is 100%
        // and would shrink the gate on the first blip, fighting the absolute
        // count threshold that is meant to own small-fleet behavior.
        if events >= THROTTLE_EVENT_THRESHOLD
            || (state.attempts.len() >= 2 && ratio > THROTTLE_RATIO_THRESHOLD)
        {
            let current = self.gate.capacity();
            if current > 1 {
                let capacity = (current / 2).max(1);
                self.gate.set_capacity(capacity);
                tracing::warn!(
                    target: "subagent",
                    window_events = events,
                    window_ratio = format!("{ratio:.2}"),
                    previous_capacity = current,
                    launch_capacity = capacity,
                    "rate-limit governor multiplicatively decreased launch capacity"
                );
            }
        }
    }

    /// Whether new launches are currently paused because of sustained 429s.
    pub fn is_paused(&self, now: Instant) -> bool {
        let mut state = self.state.lock().expect("rate limit governor poisoned");
        Self::prune(&mut state, now);
        state.paused
    }

    /// Observability snapshot: gate capacity, window limit events, paused.
    /// Feeds the fleet throttling line (addendum F5) and tests.
    pub fn snapshot(&self, now: Instant) -> GovernorSnapshot {
        let mut state = self.state.lock().expect("rate limit governor poisoned");
        Self::prune(&mut state, now);
        GovernorSnapshot {
            launch_capacity: self.gate.capacity(),
            max_capacity: state.max_capacity,
            window_limited: state.limited.len(),
            window_attempts: state.attempts.len(),
            paused: state.paused,
        }
    }
}

/// Point-in-time view of the governor for status surfaces and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernorSnapshot {
    pub launch_capacity: usize,
    pub max_capacity: usize,
    pub window_limited: usize,
    pub window_attempts: usize,
    pub paused: bool,
}

impl GovernorSnapshot {
    /// One line for the fleet header and queued rows (addendum F5), or `None`
    /// while launches run at the full configured concurrency.
    #[must_use]
    pub fn status_line(&self) -> Option<String> {
        let window = RATE_LIMIT_WINDOW.as_secs();
        if self.paused {
            return Some(format!(
                "launches paused after {} provider rate limit(s) in the last {window}s",
                self.window_limited
            ));
        }
        (self.launch_capacity < self.max_capacity).then(|| {
            format!(
                "launch slots throttled to {}/{} after {} provider rate limit(s) in the last {window}s",
                self.launch_capacity, self.max_capacity, self.window_limited
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn window_counts_and_prunes_events() {
        let (governor, _gate) = RateLimitGovernor::new(4);
        let t0 = Instant::now();
        for i in 0..5 {
            governor.record_attempt(t0 + ms(i * 10));
            governor.record_rate_limited(t0 + ms(i * 10));
        }
        let snap = governor.snapshot(t0 + ms(60));
        assert_eq!(snap.window_limited, 5);
        assert_eq!(snap.window_attempts, 5);

        // Events older than the 60s window drop out (strictly past the
        // window edge: the newest event is at t0+40ms).
        let snap = governor.snapshot(t0 + RATE_LIMIT_WINDOW + ms(50));
        assert_eq!(snap.window_limited, 0);
        assert_eq!(snap.window_attempts, 0);
    }

    #[test]
    fn multiplicative_decrease_halves_capacity_on_threshold() {
        let (governor, _gate) = RateLimitGovernor::new(8);
        let t0 = Instant::now();
        // First event: below both thresholds, no change.
        governor.record_attempt(t0);
        governor.record_rate_limited(t0);
        assert_eq!(governor.snapshot(t0).launch_capacity, 8);
        // Second event: hits the count threshold, halve.
        governor.record_attempt(t0 + ms(1));
        governor.record_rate_limited(t0 + ms(1));
        assert_eq!(governor.snapshot(t0).launch_capacity, 4);
        // Third: halve again.
        governor.record_attempt(t0 + ms(2));
        governor.record_rate_limited(t0 + ms(2));
        assert_eq!(governor.snapshot(t0).launch_capacity, 2);
        // Fourth: hits the pause threshold.
        governor.record_attempt(t0 + ms(3));
        governor.record_rate_limited(t0 + ms(3));
        let snap = governor.snapshot(t0);
        assert!(snap.paused);
    }

    #[test]
    fn status_line_names_throttle_and_pause_only_when_active() {
        let (governor, _gate) = RateLimitGovernor::new(8);
        let t0 = Instant::now();
        assert_eq!(governor.snapshot(t0).status_line(), None);
        for i in 0..2 {
            governor.record_attempt(t0 + ms(i));
            governor.record_rate_limited(t0 + ms(i));
        }
        let throttled = governor
            .snapshot(t0 + ms(2))
            .status_line()
            .expect("throttled");
        assert!(throttled.contains("throttled to 4/8"), "{throttled}");
        for i in 2..4 {
            governor.record_attempt(t0 + ms(i));
            governor.record_rate_limited(t0 + ms(i));
        }
        let paused = governor.snapshot(t0 + ms(4)).status_line().expect("paused");
        assert!(paused.starts_with("launches paused"), "{paused}");
    }

    #[test]
    fn ratio_threshold_triggers_decrease_even_with_few_events() {
        let (governor, _gate) = RateLimitGovernor::new(8);
        let t0 = Instant::now();
        // One success then one 429: the absolute event count is below the
        // threshold, but the 50% limit ratio must still shrink the gate.
        governor.record_attempt(t0);
        governor.record_success(t0);
        governor.record_attempt(t0 + ms(1));
        governor.record_rate_limited(t0 + ms(1));
        assert!(
            governor.snapshot(t0 + ms(2)).launch_capacity < 8,
            "50% limit ratio should trigger a decrease"
        );
    }

    #[test]
    fn additive_increase_recovers_capacity_gradually() {
        let (governor, _gate) = RateLimitGovernor::new(8);
        let t0 = Instant::now();
        // Drive capacity down to 4 via two events.
        governor.record_attempt(t0);
        governor.record_rate_limited(t0);
        governor.record_attempt(t0 + ms(1));
        governor.record_rate_limited(t0 + ms(1));
        assert_eq!(governor.snapshot(t0).launch_capacity, 4);

        // Three consecutive successes add exactly one unit of capacity.
        for i in 0..3u32 {
            governor.record_attempt(t0 + ms(10 + u64::from(i)));
            governor.record_success(t0 + ms(10 + u64::from(i)));
        }
        assert_eq!(governor.snapshot(t0 + ms(20)).launch_capacity, 5);
        for i in 0..3u32 {
            governor.record_attempt(t0 + ms(30 + u64::from(i)));
            governor.record_success(t0 + ms(30 + u64::from(i)));
        }
        assert_eq!(governor.snapshot(t0 + ms(40)).launch_capacity, 6);

        // A rate limit resets the success streak.
        governor.record_attempt(t0 + ms(50));
        governor.record_rate_limited(t0 + ms(50));
        for i in 0..2u32 {
            governor.record_attempt(t0 + ms(60 + u64::from(i)));
            governor.record_success(t0 + ms(60 + u64::from(i)));
        }
        governor.record_attempt(t0 + ms(80));
        governor.record_success(t0 + ms(80));
        // 2 successes before the limit + 1 after = 3 successes, but the limit
        // reset the streak, and the third event in the window halved again
        // (6 -> 3) before successes could climb.
        assert!(governor.snapshot(t0 + ms(90)).launch_capacity <= 6);
    }

    #[test]
    fn pause_releases_only_after_window_drains() {
        let (governor, gate) = RateLimitGovernor::new(8);
        let t0 = Instant::now();
        for i in 0..4 {
            governor.record_attempt(t0 + ms(i));
            governor.record_rate_limited(t0 + ms(i));
        }
        assert!(governor.is_paused(t0 + ms(10)));
        assert_eq!(governor.snapshot(t0 + ms(10)).launch_capacity, 0);

        // Successes before the window drains do NOT unpause.
        governor.record_success(t0 + ms(20));
        assert!(governor.is_paused(t0 + ms(30)));

        // Once every limit event ages out, the next success resumes at a
        // quarter of capacity.
        let late = t0 + RATE_LIMIT_WINDOW + ms(10);
        governor.record_success(late);
        assert!(!governor.is_paused(late));
        assert_eq!(governor.snapshot(late).launch_capacity, 2);
        assert_eq!(gate.capacity(), 2);
    }

    #[test]
    fn capacity_increase_is_capped_at_max() {
        let (governor, _gate) = RateLimitGovernor::new(2);
        let t0 = Instant::now();
        for i in 0..12u32 {
            governor.record_attempt(t0 + ms(u64::from(i)));
            governor.record_success(t0 + ms(u64::from(i)));
        }
        assert_eq!(governor.snapshot(t0).launch_capacity, 2);
    }

    #[test]
    fn gate_blocks_when_full_and_releases_on_drop() {
        let (governor, gate) = RateLimitGovernor::new(1);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        rt.block_on(async move {
            let first = governor.gate().try_acquire().expect("first permit");
            assert!(gate.try_acquire().is_none(), "capacity 1 must be full");

            let g2 = std::sync::Arc::clone(&gate);
            let waiter = tokio::spawn(async move { g2.acquire().await });

            // Waiter stays blocked while the first permit is held.
            tokio::time::sleep(ms(20)).await;
            assert!(!waiter.is_finished());

            drop(first);
            let _second = waiter.await.expect("waiter task");
        });
    }

    #[test]
    fn gate_set_capacity_shrinks_below_active_and_re_admits_later() {
        let (governor, gate) = RateLimitGovernor::new(4);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        rt.block_on(async move {
            let mut held: Vec<_> = (0..4)
                .map(|_| gate.try_acquire().expect("permit within capacity"))
                .collect();
            assert_eq!(gate.capacity(), 4);

            // Shrink below the active count: no new permit is granted.
            governor.gate().set_capacity(1);
            assert_eq!(gate.capacity(), 1);
            assert!(gate.try_acquire().is_none());

            let g2 = std::sync::Arc::clone(&gate);
            let waiter = tokio::spawn(async move { g2.acquire().await });
            tokio::time::sleep(ms(20)).await;
            assert!(!waiter.is_finished(), "must wait while active >= capacity");

            // Releasing holders drains `active` toward the new capacity; the
            // waiter is admitted only once every held permit is released
            // (active 4 -> 0 < capacity 1).
            drop(held.swap_remove(0));
            drop(held.swap_remove(0));
            drop(held.swap_remove(0));
            drop(held);
            let _permit = waiter.await.expect("waiter admitted after drain");
            assert!(gate.try_acquire().is_none(), "capacity 1 is now full");
            drop(_permit);
        });
    }

    #[test]
    fn rate_limit_retry_delay_is_full_jitter_within_base() {
        for retry in 1..=12u32 {
            let base = rate_limit_backoff_base(retry);
            for _ in 0..64 {
                let delay = rate_limit_retry_delay(retry);
                assert!(delay <= base, "full jitter must not exceed the base");
            }
        }
        // The cap holds for absurd retry numbers.
        assert_eq!(rate_limit_backoff_base(40), RATE_LIMIT_MAX_BACKOFF);
    }

    /// A pause must lift via the time-driven probe even when no in-flight
    /// child ever reports another success (the in-flight fleet drained before
    /// the window did): otherwise queued children freeze until their
    /// wall-time deadline.
    #[test]
    fn forkguard_rate_limit_governor_pauses_and_time_recovers_after_window_drains() {
        let (governor, _gate) = RateLimitGovernor::new(8);
        let t0 = Instant::now();
        for i in 0..4 {
            governor.record_attempt(t0 + ms(i));
            governor.record_rate_limited(t0 + ms(i));
        }
        assert!(governor.is_paused(t0 + ms(10)));

        // Probe while 429 events are still inside the window: stays paused.
        governor.recover_if_window_drained(t0 + ms(20));
        assert!(governor.is_paused(t0 + ms(30)));

        // Once every limit event has aged out, the probe resumes launches at
        // a quarter of the configured capacity — no success event required.
        let late = t0 + RATE_LIMIT_WINDOW + ms(10);
        governor.recover_if_window_drained(late);
        assert!(!governor.is_paused(late));
        assert_eq!(governor.snapshot(late).launch_capacity, 2);
    }

    /// A runtime launch-concurrency change must not silently lift a pause:
    /// the gate stays at capacity 0 until the window drains, then resumes at
    /// a quarter of the *new* configured capacity.
    #[test]
    fn forkguard_rate_limit_governor_limit_change_keeps_pause_capacity_zero() {
        let (governor, gate) = RateLimitGovernor::new(8);
        let t0 = Instant::now();
        for i in 0..4 {
            governor.record_attempt(t0 + ms(i));
            governor.record_rate_limited(t0 + ms(i));
        }
        assert!(governor.is_paused(t0 + ms(1)));

        governor.set_max_capacity(4);
        assert_eq!(gate.capacity(), 0, "pause must keep capacity 0");

        let late = t0 + RATE_LIMIT_WINDOW + ms(10);
        governor.recover_if_window_drained(late);
        assert_eq!(
            gate.capacity(),
            1,
            "resume at a quarter of the new capacity"
        );
    }

    /// A waiter cancelled *after* its grant was dispatched must not swallow
    /// the slot: the permit is dropped with the cancelled future and its
    /// `Drop` re-releases it for the next waiter.
    #[test]
    fn forkguard_dynamic_gate_redispatches_grant_of_cancelled_waiter() {
        let (_governor, gate) = RateLimitGovernor::new(1);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        rt.block_on(async move {
            let holder = gate.try_acquire().expect("holder");
            let g2 = std::sync::Arc::clone(&gate);
            let waiter = tokio::spawn(async move { g2.acquire().await });
            tokio::time::sleep(ms(20)).await;
            assert!(!waiter.is_finished(), "waiter must be queued");

            // Releasing the holder dispatches the grant into the waiter's
            // channel; on a current-thread runtime the waiter has not polled
            // yet when we abort it, so the permit is dropped mid-flight.
            drop(holder);
            waiter.abort();
            tokio::time::sleep(ms(20)).await;

            assert!(
                gate.try_acquire().is_some(),
                "grant of cancelled waiter must be re-released, not leaked"
            );
        });
    }

    /// The mirror case of the redispatch test: a waiter cancelled *before*
    /// its grant was dispatched leaves a stale queue entry with a dead
    /// receiver. The granter must skip that entry — disarming the already
    /// built permit instead of dropping it, which would re-enter the gate
    /// lock held by `wake_locked` — and the slot must stay usable.
    #[test]
    fn forkguard_dynamic_gate_skips_stale_queued_waiter_without_leaking_slot() {
        let (_governor, gate) = RateLimitGovernor::new(1);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        rt.block_on(async move {
            let holder = gate.try_acquire().expect("holder");
            let g2 = std::sync::Arc::clone(&gate);
            let waiter = tokio::spawn(async move { g2.acquire().await });
            tokio::time::sleep(ms(20)).await;
            assert!(
                !waiter.is_finished(),
                "waiter must be queued behind the holder"
            );

            // Cancel while the gate is full: no grant was ever dispatched,
            // so the stale entry stays queued with a dead receiver.
            waiter.abort();
            tokio::time::sleep(ms(20)).await;

            // Releasing the holder runs the granter over the stale entry.
            drop(holder);
            assert_eq!(
                gate.available_permits(),
                1,
                "cancelled queued waiter must neither swallow nor leak the slot"
            );
            let permit = gate
                .try_acquire()
                .expect("slot usable after the stale entry is skipped");
            drop(permit);
        });
    }

    /// Stress: concurrent acquire/release with aborts and capacity
    /// oscillation through 0 (a pause). Whatever the interleaving, every
    /// slot must come home — a lost wakeup or a leaked (never released)
    /// permit leaves the gate short of full capacity at the end of a round,
    /// and an over-granted permit keeps a slot alive after all owners are
    /// gone. Both fail the drain assertion.
    #[test]
    fn forkguard_dynamic_gate_stress_drains_to_full_capacity_despite_aborts() {
        let (_governor, gate) = RateLimitGovernor::new(4);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("test runtime");
        rt.block_on(async move {
            for round in 0..24usize {
                // Start every round with live headroom, then briefly drop to
                // 0 mid-round on every third round: the pause case keeps a
                // full queue parked while nothing holds a permit. Capacity 0
                // is never left in place while joining — with nobody holding
                // a permit a permanent 0 would deadlock the round by design,
                // so the restore below is part of the scenario.
                gate.set_capacity(1 + (round % 2));
                let mut handles = Vec::new();
                for i in 0..16u32 {
                    let g = std::sync::Arc::clone(&gate);
                    handles.push(tokio::spawn(async move {
                        let _permit = g.acquire().await;
                        tokio::time::sleep(ms(u64::from(i % 4))).await;
                    }));
                }
                // Abort every third task: some while still queued (stale
                // queue entries), some already holding a permit (the
                // drop-releases-and-rewakes path).
                for handle in handles.iter().step_by(3) {
                    handle.abort();
                }
                if round % 3 == 0 {
                    gate.set_capacity(0);
                    tokio::time::sleep(ms(2)).await;
                }
                gate.set_capacity(4);
                for handle in handles {
                    // A task that cannot finish inside the budget means a
                    // lost wakeup, a leaked permit, or a slot swallowed by a
                    // stale entry — fail the round instead of hanging.
                    tokio::time::timeout(ms(2000), handle)
                        .await
                        .expect("task must finish: stuck rounds mean lost wakeups or leaked slots")
                        .ok();
                }
                // Let straggler permit drops (cancelled waiter re-release)
                // run before asserting the drain.
                tokio::time::sleep(ms(5)).await;
                assert_eq!(
                    gate.available_permits(),
                    4,
                    "round {round}: gate must drain to full capacity despite aborts and pauses"
                );
            }
        });
    }
}
