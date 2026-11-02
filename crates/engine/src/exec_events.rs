use std::time::{Duration, Instant};

use anyhow::Result;

type ExecSettlementProbe = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<crate::core::ops::SubAgentSettlement>> + Send>,
>;

/// Read the existing Engine stream through the one-shot host's final boundary.
/// Successful parent receipts remain pending while admitted children or their
/// completion inbox can still produce another normal Engine turn. This owns
/// only the deferred output receipt, never child execution or a turn loop.
pub struct ExecAgentEvents {
    handle: crate::core::engine::EngineHandle,
    deadline: tokio::time::Instant,
    terminal: Option<crate::core::events::Event>,
    probe: Option<ExecSettlementProbe>,
    next_probe_at: tokio::time::Instant,
    in_flight_usage: codewhale_models::Usage,
}

impl ExecAgentEvents {
    pub fn new(handle: crate::core::engine::EngineHandle, deadline: Instant) -> Self {
        Self {
            handle,
            deadline: deadline.into(),
            terminal: None,
            probe: None,
            next_probe_at: tokio::time::Instant::now(),
            in_flight_usage: codewhale_models::Usage::default(),
        }
    }

    fn stop_settlement(
        &mut self,
        status: crate::core::events::TurnOutcomeStatus,
        error: String,
    ) -> crate::core::events::Event {
        use crate::core::events::Event;
        self.handle
            .cancel_with_reason(crate::core::engine::CancelReason::External);
        // Cancellation is out of band; shutdown remains in the existing
        // Engine mailbox so it also cancels detached session children.
        let _ = self.handle.try_send(crate::core::ops::Op::Shutdown);
        self.probe = None;
        let mut terminal = self.terminal.take().expect("pending parent receipt");
        if let Event::TurnComplete {
            status: terminal_status,
            error: terminal_error,
            usage,
            parent_route_usage,
            routed_usage_dropped_records,
            ..
        } = &mut terminal
        {
            *terminal_status = status;
            *terminal_error = Some(error);
            crate::core::turn::add_usage_to(usage, &self.in_flight_usage);
            crate::core::turn::add_usage_to(parent_route_usage, &self.in_flight_usage);
            // The host cannot prove usage settlement after abandoning the
            // inbox. Keep reported usage and explicitly mark coverage partial.
            *routed_usage_dropped_records = routed_usage_dropped_records.saturating_add(1);
        }
        self.in_flight_usage = codewhale_models::Usage::default();
        terminal
    }

    pub async fn next(&mut self) -> Option<crate::core::events::Event> {
        use crate::core::events::{Event, TurnOutcomeStatus};
        // Keep the streamed event on the stack instead of allocating another
        // box for every token merely to equalize the two small control arms.
        #[allow(clippy::large_enum_variant)]
        enum Input {
            Event(Option<Event>),
            Probe(Result<crate::core::ops::SubAgentSettlement>),
            Poll,
        }
        loop {
            if matches!(
                self.terminal,
                Some(Event::TurnComplete { status, .. }) if status != TurnOutcomeStatus::Completed
            ) {
                return self.terminal.take();
            }
            let settling = self.terminal.is_some();
            if settling && self.handle.is_cancelled() {
                return Some(self.stop_settlement(
                    TurnOutcomeStatus::Interrupted,
                    "Headless exec cancelled while settling children; recorded usage is partial."
                        .to_string(),
                ));
            }
            if settling && tokio::time::Instant::now() >= self.deadline {
                return Some(self.stop_settlement(
                    TurnOutcomeStatus::Failed,
                    "Headless exec wall-clock budget exhausted while settling children; recorded usage is partial."
                        .to_string(),
                ));
            }
            if settling && self.probe.is_none() && tokio::time::Instant::now() >= self.next_probe_at
            {
                let handle = self.handle.clone();
                self.probe = Some(Box::pin(
                    async move { handle.get_subagent_settlement().await },
                ));
            }
            let probing = self.probe.is_some();
            let wake_at = self.deadline.min(if probing {
                tokio::time::Instant::now() + Duration::from_millis(250)
            } else {
                self.next_probe_at
            });
            let input = {
                let mut events = self.handle.rx_event.write().await;
                tokio::select! {
                    biased;
                    // Drain queued SessionUpdated/TurnComplete events before
                    // accepting the later actor-owned idle receipt.
                    event = events.recv() => Input::Event(event),
                    result = async { self.probe.as_mut().expect("active probe").await }, if probing => Input::Probe(result),
                    () = tokio::time::sleep_until(wake_at), if settling => Input::Poll,
                }
            };
            match input {
                Input::Poll => {}
                Input::Probe(Ok(snapshot)) if snapshot.is_settled() => {
                    self.probe = None;
                    return self.terminal.take();
                }
                Input::Probe(Ok(_)) => {
                    self.probe = None;
                    self.next_probe_at = tokio::time::Instant::now() + Duration::from_millis(250);
                }
                Input::Probe(Err(error)) => {
                    return Some(self.stop_settlement(
                        TurnOutcomeStatus::Failed,
                        format!(
                            "Cannot verify child settlement: {error}; recorded usage is partial."
                        ),
                    ));
                }
                Input::Event(None) if settling => {
                    return Some(self.stop_settlement(
                        TurnOutcomeStatus::Failed,
                        "Engine event channel closed before child settlement; recorded usage is partial."
                            .to_string(),
                    ));
                }
                Input::Event(None) => return None,
                Input::Event(Some(mut event)) => {
                    match &mut event {
                        Event::TurnComplete {
                            usage,
                            parent_route_usage,
                            routed_usage_dropped_records,
                            status,
                            error,
                            ..
                        } => {
                            if let Some(Event::TurnComplete {
                                usage: prior_usage,
                                parent_route_usage: prior_parent_usage,
                                routed_usage_dropped_records: prior_dropped,
                                ..
                            }) = self.terminal.take()
                            {
                                crate::core::turn::add_usage_to(usage, &prior_usage);
                                crate::core::turn::add_usage_to(
                                    parent_route_usage,
                                    &prior_parent_usage,
                                );
                                *routed_usage_dropped_records =
                                    routed_usage_dropped_records.saturating_add(prior_dropped);
                            }
                            self.in_flight_usage = codewhale_models::Usage::default();
                            if *status == TurnOutcomeStatus::Completed && error.is_none() {
                                self.terminal = Some(event);
                                self.next_probe_at = tokio::time::Instant::now();
                                continue;
                            }
                        }
                        Event::TurnUsage { usage, .. } => {
                            crate::core::turn::add_usage_to(&mut self.in_flight_usage, usage);
                        }
                        Event::Error { envelope, .. }
                            if settling && exec_error_event_is_fatal(envelope) =>
                        {
                            let terminal = self.stop_settlement(
                                TurnOutcomeStatus::Failed,
                                format!(
                                    "{}; child settlement stopped and recorded usage is partial.",
                                    envelope.message
                                ),
                            );
                            self.terminal = Some(terminal);
                        }
                        _ => {}
                    }
                    return Some(event);
                }
            }
        }
    }
}

/// Should a mid-turn engine error event force the final exec summary into
/// failure? Only non-recoverable envelopes do. Recoverable warnings (stream
/// stall notices, transient retry noise) are emitted on the stream for
/// visibility, but the terminal `TurnComplete` event carries the
/// authoritative turn outcome — a warning must never fail a run whose turn
/// later completes.
pub fn exec_error_event_is_fatal(envelope: &codewhale_error_taxonomy::ErrorEnvelope) -> bool {
    !envelope.recoverable
}
