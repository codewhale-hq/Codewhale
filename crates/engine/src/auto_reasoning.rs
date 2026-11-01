//! Declared reasoning-effort tier policy for `Auto` mode (#663).
//!
//! When the user sets `reasoning_effort = "auto"`, the engine calls
//! [`select`] before each turn-level request to pick the actual tier.
//!
//! The tier is a declared policy, not a content classification. Until the
//! #6290 rework it guessed from the user's wording (`debug`/`error` → `Max`,
//! `search`/`lookup` → `Low`, with CJK/JP keyword lists) — the host-side
//! semantic-determinism class that made cost and answer quality depend on
//! vocabulary rather than the task. `auto` now means the declared default
//! below; the user's explicit tier is the precise control. A model-declared
//! escalation hint is the intended follow-up — this function is the seam a
//! real signal would land in.
//!
//! Known limitation: there is no per-turn cost policy left here. A turn whose
//! wording would once have classified as `Low` now runs at `High` unless the
//! user or the caller sets a tier explicitly; `auto` is not a cost-saver by
//! itself.

use crate::reasoning_preference::ReasoningEffort;

/// Choose a concrete `ReasoningEffort` tier for an `auto` setting.
///
/// Declared policy: `High`. The message, the model, and the caller are never
/// inspected.
#[must_use]
pub fn select() -> ReasoningEffort {
    ReasoningEffort::High
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared default. Changing this value is a policy change: update the
    /// resolution tests in `core/engine/turn_loop.rs` and `lib.rs` with it.
    #[test]
    fn declared_default_is_high() {
        assert_eq!(select(), ReasoningEffort::High);
    }
}
