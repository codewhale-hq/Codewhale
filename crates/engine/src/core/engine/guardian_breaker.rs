use std::collections::{HashMap, VecDeque};

pub(crate) const CONSECUTIVE_DENIAL_LIMIT: usize = 3;
pub(crate) const WINDOW_DENIAL_LIMIT: usize = 10;
pub(crate) const WINDOW_SIZE: usize = 50;

pub(crate) const CONSECUTIVE_TRIP_COPY: &str = "Auto-Review denied 3 actions in a row, so the turn was stopped. Review the denials with /approve, change the request, or switch to Ask.";
pub(crate) const WINDOW_TRIP_COPY: &str = "Auto-Review denied 10 actions in the last 50 reviews, so the turn was stopped. Review the denials with /approve, change the request, or switch to Ask.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardianBreakerTrip {
    Consecutive(usize),
    Window { denials: usize, window: usize },
}

impl GuardianBreakerTrip {
    #[must_use]
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Consecutive(_) => CONSECUTIVE_TRIP_COPY,
            Self::Window { .. } => WINDOW_TRIP_COPY,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct TurnBreakerState {
    consecutive_denials: usize,
    recent_outcomes: VecDeque<bool>,
    recent_denials: usize,
    tripped: bool,
    pending_interrupt: Option<GuardianBreakerTrip>,
    last_trip: Option<GuardianBreakerTrip>,
}

impl TurnBreakerState {
    fn push_window_outcome(&mut self, denied: bool) {
        if self.recent_outcomes.len() == WINDOW_SIZE
            && let Some(oldest_denied) = self.recent_outcomes.pop_front()
            && oldest_denied
        {
            self.recent_denials = self.recent_denials.saturating_sub(1);
        }
        self.recent_outcomes.push_back(denied);
        if denied {
            self.recent_denials = self.recent_denials.saturating_add(1);
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct GuardianBreaker {
    turns: HashMap<String, TurnBreakerState>,
}

impl GuardianBreaker {
    pub(crate) fn clear_turn(&mut self, turn_id: &str) {
        self.turns.remove(turn_id);
    }

    pub(crate) fn record_allow(&mut self, turn_id: &str) {
        let state = self.turns.entry(turn_id.to_string()).or_default();
        state.consecutive_denials = 0;
        state.push_window_outcome(false);
    }

    pub(crate) fn record_failure(&mut self, _turn_id: &str) {}

    pub(crate) fn record_person_approved(&mut self, _turn_id: &str) {}

    pub(crate) fn record_deny(&mut self, turn_id: &str) -> Option<GuardianBreakerTrip> {
        let state = self.turns.entry(turn_id.to_string()).or_default();
        state.consecutive_denials = state.consecutive_denials.saturating_add(1);
        state.push_window_outcome(true);

        if state.tripped {
            return None;
        }

        let trip = if state.consecutive_denials >= CONSECUTIVE_DENIAL_LIMIT {
            Some(GuardianBreakerTrip::Consecutive(state.consecutive_denials))
        } else if state.recent_denials >= WINDOW_DENIAL_LIMIT {
            Some(GuardianBreakerTrip::Window {
                denials: state.recent_denials,
                window: WINDOW_SIZE,
            })
        } else {
            None
        };

        if let Some(trip) = trip {
            state.tripped = true;
            state.pending_interrupt = Some(trip);
            state.last_trip = Some(trip);
        }

        trip
    }

    #[must_use]
    pub(crate) fn is_tripped(&self, turn_id: &str) -> bool {
        self.turns.get(turn_id).is_some_and(|state| state.tripped)
    }

    #[must_use]
    pub(crate) fn tripped_message(&self, turn_id: &str) -> Option<&'static str> {
        self.turns
            .get(turn_id)
            .and_then(|state| state.last_trip.map(GuardianBreakerTrip::message))
    }

    pub(crate) fn take_interrupt_message(&mut self, turn_id: &str) -> Option<&'static str> {
        self.turns.get_mut(turn_id).and_then(|state| {
            state
                .pending_interrupt
                .take()
                .map(GuardianBreakerTrip::message)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_trips_on_third_consecutive_denial() {
        let mut breaker = GuardianBreaker::default();
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert_eq!(breaker.record_deny("turn-1"), None);
        let trip = breaker.record_deny("turn-1");
        assert_eq!(
            trip,
            Some(GuardianBreakerTrip::Consecutive(CONSECUTIVE_DENIAL_LIMIT))
        );
        assert_eq!(trip.unwrap().message(), CONSECUTIVE_TRIP_COPY);
        assert!(breaker.is_tripped("turn-1"));
        assert!(!breaker.is_tripped("turn-2"));
    }

    #[test]
    fn allow_resets_consecutive() {
        let mut breaker = GuardianBreaker::default();
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert_eq!(breaker.record_deny("turn-1"), None);
        breaker.record_allow("turn-1");
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert!(!breaker.is_tripped("turn-1"));
        assert_eq!(
            breaker.record_deny("turn-1"),
            Some(GuardianBreakerTrip::Consecutive(CONSECUTIVE_DENIAL_LIMIT))
        );
    }

    #[test]
    fn ten_of_fifty_trips() {
        let mut breaker = GuardianBreaker::default();
        for _ in 0..9 {
            assert_eq!(breaker.record_deny("turn-1"), None);
            breaker.record_allow("turn-1");
            breaker.record_allow("turn-1");
        }
        assert!(!breaker.is_tripped("turn-1"));
        let trip = breaker.record_deny("turn-1");
        assert_eq!(
            trip,
            Some(GuardianBreakerTrip::Window {
                denials: WINDOW_DENIAL_LIMIT,
                window: WINDOW_SIZE,
            })
        );
        assert_eq!(trip.unwrap().message(), WINDOW_TRIP_COPY);

        breaker.clear_turn("turn-1");
        assert!(!breaker.is_tripped("turn-1"));
    }

    #[test]
    fn failures_do_not_count() {
        let mut breaker = GuardianBreaker::default();
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert_eq!(breaker.record_deny("turn-1"), None);
        for _ in 0..15 {
            breaker.record_failure("turn-1");
            breaker.record_person_approved("turn-1");
        }
        assert!(!breaker.is_tripped("turn-1"));
        assert_eq!(
            breaker.record_deny("turn-1"),
            Some(GuardianBreakerTrip::Consecutive(CONSECUTIVE_DENIAL_LIMIT))
        );
    }

    #[test]
    fn trips_once_per_turn() {
        let mut breaker = GuardianBreaker::default();
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert!(breaker.record_deny("turn-1").is_some());
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert_eq!(breaker.record_deny("turn-1"), None);
        assert_eq!(
            breaker.take_interrupt_message("turn-1"),
            Some(CONSECUTIVE_TRIP_COPY)
        );
        assert_eq!(breaker.take_interrupt_message("turn-1"), None);
    }
}
