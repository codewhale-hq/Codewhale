//! Esc-Esc backtrack state machine (issue #133).
//!
//! Lets the user rewind the active conversation to a previous user message.
//! The chord is intentionally two-step so a single stray `Esc` after a popup
//! close cannot accidentally rewind a turn:
//!
//! 1. **First Esc** (no popup, no streaming, nothing to clear) — moves
//!    `Inactive` → `Primed`. The composer surfaces a transient hint
//!    ("Press Esc again to backtrack"). A second Esc within the prime
//!    window opens the overlay. Any other key path can later cancel the
//!    prime.
//! 2. **Second Esc** — moves `Primed` → `Selecting { selected_idx: 0 }`.
//!    The live-transcript overlay opens with the most recent user message
//!    highlighted. Left/Right step through prior user messages.
//! 3. **Enter** — commits the selection: yields the chosen `selected_idx`
//!    (a depth-from-tail offset, where `0` = newest user turn). Resets the
//!    machine to `Inactive`. The caller then forks the thread, populates
//!    the composer with the rolled-back text, and trims the transcript.
//!
//! The state machine knows nothing about the rest of the app — it stores
//! only the small bookkeeping required to pick the right user turn. UI
//! routing (popup detection, streaming guard, fork side effects) lives in
//! `tui::ui`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BacktrackPhase {
    /// No prime in flight; Esc behaves normally.
    #[default]
    Inactive,
    /// First Esc captured. The next Esc transitions into `Selecting`; any
    /// other Esc-equivalent dismissal cancels back to `Inactive`.
    Primed,
    /// Overlay open. `selected_idx` is the depth-from-tail of the user
    /// message currently highlighted (`0` = most recent). `total` is the
    /// number of user messages available to step through, captured at
    /// entry so bounds checks stay stable even if the transcript mutates
    /// underneath the overlay (which it will, because the engine never
    /// pauses).
    Selecting { selected_idx: usize, total: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Step toward older user messages (increases `selected_idx`).
    Left,
    /// Step toward newer user messages (decreases `selected_idx`).
    Right,
}

/// What the caller should do in response to a single `Esc` press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscEffect {
    /// No backtrack action — the caller should run its normal Esc path.
    None,
    /// Move from `Inactive` to `Primed`. The caller should surface the
    /// transient prime hint.
    Prime,
    /// Cancel a Primed state without entering Selecting. The caller should
    /// clear the prime hint.
    Cancel,
    /// Open the backtrack overlay (we transitioned `Primed` → `Selecting`).
    /// The caller should push the live-transcript overlay in
    /// `BacktrackPreview` mode.
    OpenOverlay,
}

/// Small bookkeeping struct hung off `App`. Owns only the state machine —
/// no transcript snapshots, no UI handles. The caller is responsible for
/// telling the state machine how many user messages exist when entering
/// `Selecting`, which avoids tying this module to any particular
/// transcript representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BacktrackState {
    pub phase: BacktrackPhase,
}

impl BacktrackState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: BacktrackPhase::Inactive,
        }
    }

    /// `true` whenever the user has armed or opened backtrack. The UI uses
    /// this to skip the prime hint once the overlay is up and to know
    /// whether arrow keys should drive selection.
    #[cfg(test)]
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, BacktrackPhase::Inactive)
    }

    /// `true` only when the overlay is open and Left/Right should step
    /// through prior user messages. `Primed` is intentionally excluded —
    /// during the prime window arrows still scroll the transcript.
    #[cfg(test)]
    #[must_use]
    pub fn is_selecting(&self) -> bool {
        matches!(self.phase, BacktrackPhase::Selecting { .. })
    }

    /// Current depth-from-tail offset, if any. Convenient for renderers
    /// that need the highlight index without matching the enum.
    #[must_use]
    pub fn selected_idx(&self) -> Option<usize> {
        match self.phase {
            BacktrackPhase::Selecting { selected_idx, .. } => Some(selected_idx),
            _ => None,
        }
    }

    /// Process an Esc press.
    ///
    /// `total_user_messages` is the count of user turns in the live
    /// transcript right now. It's only consulted on the `Primed` → `Selecting`
    /// transition; a value of `0` short-circuits and cancels the prime
    /// (nothing to backtrack to).
    pub fn handle_esc(&mut self, total_user_messages: usize) -> EscEffect {
        match self.phase {
            BacktrackPhase::Inactive => {
                if total_user_messages == 0 {
                    // Nothing to backtrack to — do not even prime.
                    return EscEffect::None;
                }
                self.phase = BacktrackPhase::Primed;
                EscEffect::Prime
            }
            BacktrackPhase::Primed => {
                if total_user_messages == 0 {
                    self.phase = BacktrackPhase::Inactive;
                    return EscEffect::Cancel;
                }
                self.phase = BacktrackPhase::Selecting {
                    selected_idx: 0,
                    total: total_user_messages,
                };
                EscEffect::OpenOverlay
            }
            BacktrackPhase::Selecting { .. } => {
                // Esc while Selecting closes the overlay via the modal's own
                // handler; it should not be routed back through here. Defend
                // against accidental routing by canceling.
                self.phase = BacktrackPhase::Inactive;
                EscEffect::Cancel
            }
        }
    }

    /// Step the selection while in `Selecting`. No-op in any other phase.
    /// `Left` walks backward in time (older), `Right` walks forward (newer).
    /// Bounds-checked: `selected_idx` is clamped to `[0, total - 1]`.
    pub fn step(&mut self, dir: Direction) {
        if let BacktrackPhase::Selecting {
            selected_idx,
            total,
        } = self.phase
        {
            if total == 0 {
                return;
            }
            let last = total.saturating_sub(1);
            let new_idx = match dir {
                Direction::Left => selected_idx.saturating_add(1).min(last),
                Direction::Right => selected_idx.saturating_sub(1),
            };
            self.phase = BacktrackPhase::Selecting {
                selected_idx: new_idx,
                total,
            };
        }
    }

    /// Commit the current selection. Returns the depth-from-tail offset
    /// (0 = newest user turn) on success and resets to `Inactive`.
    /// Returns `None` if not currently selecting — the caller should treat
    /// it as a no-op.
    pub fn confirm(&mut self) -> Option<usize> {
        match self.phase {
            BacktrackPhase::Selecting { selected_idx, .. } => {
                self.phase = BacktrackPhase::Inactive;
                Some(selected_idx)
            }
            _ => None,
        }
    }

    /// Force the state machine back to `Inactive`. Used by the UI when a
    /// popup steals focus, when streaming starts, when the overlay closes
    /// without a confirm, and when any non-arrow / non-Enter key arrives
    /// during `Primed`.
    pub fn reset(&mut self) {
        self.phase = BacktrackPhase::Inactive;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_is_inactive() {
        let s = BacktrackState::new();
        assert!(!s.is_active());
        assert!(!s.is_selecting());
        assert_eq!(s.selected_idx(), None);
    }

    #[test]
    fn first_esc_primes() {
        let mut s = BacktrackState::new();
        let effect = s.handle_esc(3);
        assert_eq!(effect, EscEffect::Prime);
        assert!(matches!(s.phase, BacktrackPhase::Primed));
        assert!(s.is_active());
        assert!(!s.is_selecting());
    }

    #[test]
    fn first_esc_with_no_user_messages_is_noop() {
        let mut s = BacktrackState::new();
        let effect = s.handle_esc(0);
        assert_eq!(effect, EscEffect::None);
        assert!(matches!(s.phase, BacktrackPhase::Inactive));
    }

    #[test]
    fn double_esc_enters_selecting() {
        let mut s = BacktrackState::new();
        assert_eq!(s.handle_esc(5), EscEffect::Prime);
        let effect = s.handle_esc(5);
        assert_eq!(effect, EscEffect::OpenOverlay);
        assert_eq!(
            s.phase,
            BacktrackPhase::Selecting {
                selected_idx: 0,
                total: 5,
            }
        );
        assert!(s.is_selecting());
    }

    #[test]
    fn primed_with_zero_messages_cancels() {
        // If the transcript empties between the first and second Esc (e.g.
        // /clear ran in another path), the second Esc must cancel rather
        // than open an empty overlay.
        let mut s = BacktrackState::new();
        s.phase = BacktrackPhase::Primed;
        let effect = s.handle_esc(0);
        assert_eq!(effect, EscEffect::Cancel);
        assert!(matches!(s.phase, BacktrackPhase::Inactive));
    }

    #[test]
    fn step_left_walks_back_in_time() {
        let mut s = BacktrackState::new();
        s.phase = BacktrackPhase::Selecting {
            selected_idx: 0,
            total: 3,
        };
        s.step(Direction::Left);
        assert_eq!(s.selected_idx(), Some(1));
        s.step(Direction::Left);
        assert_eq!(s.selected_idx(), Some(2));
        // Bounds: cannot go past `total - 1`.
        s.step(Direction::Left);
        assert_eq!(s.selected_idx(), Some(2));
    }

    #[test]
    fn step_right_walks_forward_in_time() {
        let mut s = BacktrackState::new();
        s.phase = BacktrackPhase::Selecting {
            selected_idx: 2,
            total: 3,
        };
        s.step(Direction::Right);
        assert_eq!(s.selected_idx(), Some(1));
        s.step(Direction::Right);
        assert_eq!(s.selected_idx(), Some(0));
        // Bounds: saturating_sub keeps the floor at 0.
        s.step(Direction::Right);
        assert_eq!(s.selected_idx(), Some(0));
    }

    #[test]
    fn step_in_inactive_or_primed_is_noop() {
        let mut s = BacktrackState::new();
        s.step(Direction::Left);
        assert!(matches!(s.phase, BacktrackPhase::Inactive));
        s.phase = BacktrackPhase::Primed;
        s.step(Direction::Right);
        assert!(matches!(s.phase, BacktrackPhase::Primed));
    }

    #[test]
    fn step_with_total_one_clamps_at_zero() {
        let mut s = BacktrackState::new();
        s.phase = BacktrackPhase::Selecting {
            selected_idx: 0,
            total: 1,
        };
        s.step(Direction::Left);
        assert_eq!(s.selected_idx(), Some(0));
        s.step(Direction::Right);
        assert_eq!(s.selected_idx(), Some(0));
    }

    #[test]
    fn confirm_yields_index_and_resets() {
        let mut s = BacktrackState::new();
        s.phase = BacktrackPhase::Selecting {
            selected_idx: 2,
            total: 5,
        };
        let idx = s.confirm();
        assert_eq!(idx, Some(2));
        assert!(matches!(s.phase, BacktrackPhase::Inactive));
    }

    #[test]
    fn confirm_outside_selecting_returns_none() {
        let mut s = BacktrackState::new();
        assert_eq!(s.confirm(), None);
        s.phase = BacktrackPhase::Primed;
        assert_eq!(s.confirm(), None);
        assert!(matches!(s.phase, BacktrackPhase::Primed));
    }

    #[test]
    fn reset_returns_to_inactive_from_any_phase() {
        let mut s = BacktrackState::new();
        s.phase = BacktrackPhase::Primed;
        s.reset();
        assert!(matches!(s.phase, BacktrackPhase::Inactive));

        s.phase = BacktrackPhase::Selecting {
            selected_idx: 1,
            total: 3,
        };
        s.reset();
        assert!(matches!(s.phase, BacktrackPhase::Inactive));
    }

    #[test]
    fn esc_during_selecting_resets_defensively() {
        // Routing Esc through the state machine while already selecting
        // should not enter a fourth state — it cancels. The overlay's own
        // Esc handler is the canonical close path, but we defend against
        // a callsite that misroutes.
        let mut s = BacktrackState::new();
        s.phase = BacktrackPhase::Selecting {
            selected_idx: 1,
            total: 3,
        };
        let effect = s.handle_esc(3);
        assert_eq!(effect, EscEffect::Cancel);
        assert!(matches!(s.phase, BacktrackPhase::Inactive));
    }

    #[test]
    fn primed_then_step_then_second_esc_reaches_selecting() {
        // Steps that arrive while Primed should be no-ops on phase, so a
        // subsequent Esc still completes the chord. (Practically this
        // matters for the case where the user, for instance, pressed an
        // arrow key while the prime hint was visible.)
        let mut s = BacktrackState::new();
        assert_eq!(s.handle_esc(2), EscEffect::Prime);
        s.step(Direction::Left); // no-op
        assert!(matches!(s.phase, BacktrackPhase::Primed));
        assert_eq!(s.handle_esc(2), EscEffect::OpenOverlay);
        assert_eq!(s.selected_idx(), Some(0));
    }

    #[test]
    fn full_walk_then_confirm_returns_chosen_index() {
        let mut s = BacktrackState::new();
        assert_eq!(s.handle_esc(4), EscEffect::Prime);
        assert_eq!(s.handle_esc(4), EscEffect::OpenOverlay);
        s.step(Direction::Left); // 0 -> 1
        s.step(Direction::Left); // 1 -> 2
        assert_eq!(s.confirm(), Some(2));
        assert!(matches!(s.phase, BacktrackPhase::Inactive));
    }
}
