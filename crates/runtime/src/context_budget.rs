//! Unified context-budget math for the TUI.
//!
//! Given a model's context window, the current input token estimate, and a
//! configured output cap, [`ContextBudget`] derives the four numbers the rest
//! of the app needs to reason about a turn:
//!
//!   * **available input budget** — how many input tokens may still be spent
//!     after reserving room for the model's output;
//!   * **output token cap** — the output reservation actually used to compute
//!     that budget (clamped so it never starves the window);
//!   * **compaction trigger** — the input-token level at which compaction
//!     should be suggested (default: ~75% of the full window, clamped to the
//!     spendable input ceiling);
//!   * **[`PressureLevel`]** — a coarse Low/Medium/High/Critical signal the UI
//!     can render without re-deriving thresholds.
//!
//! This module is the budget-math *foundation*. It is intentionally pure (no
//! I/O, no clock, no engine/config types) so it can be unit-tested in isolation.
//! Its consumers live outside it and call in, never the reverse: `route_budget`
//! and `core::engine::context` for [`ContextBudget`], `context_report` for
//! [`PressureLevel`].
//!
//! ### Route ceilings stay independent
//!
//! A route can publish a total context window, an output ceiling, and a
//! separate input ceiling. [`ContextBudget::new_with_input_limit`] intersects
//! all three without treating one as a substitute for another: the output
//! reservation is the route-effective wire request (a quarter of the window, at
//! most 64K and never more than that request, on a route that shares one window
//! between input and output), the total-window
//! arithmetic remains saturating, and a concrete input limit clamps the final
//! spendable ceiling and compaction trigger.

pub const CONTEXT_WARNING_THRESHOLD_PERCENT: f64 = 85.0;
pub const CONTEXT_CRITICAL_THRESHOLD_PERCENT: f64 = 95.0;

// This module IS wired. `ContextBudget` is consumed by `route_budget.rs` and
// `core/engine/context.rs`; `PressureLevel` by `context_report.rs`. It sits on
// the do-not-delete list in AGENTS.md because a blanket `allow(dead_code)` here,
// plus a comment that used to claim the module was "not yet referenced," taught
// several dead-code audits to propose deleting a live file. The suppression is
// now `#[cfg_attr(not(test), expect(dead_code))]` on the three methods unused
// outside tests, so a suppression that stops matching the lint fails the
// build instead of hiding behind a module-wide waiver. Tests call them, so
// a bare `#[expect(dead_code)]` would be unfulfilled under `cfg(test)`.

/// Fraction of the window, expressed as a percentage, at or above which
/// compaction should be suggested. Mirrors the "high" pressure boundary the
/// existing context report uses for its diagnostic label, rounded up to the
/// conventional three-quarters-full trigger.
pub const DEFAULT_COMPACTION_TRIGGER_PERCENT: f64 = 75.0;

/// Percentage of the window at or above which pressure is [`PressureLevel::Critical`].
pub const CRITICAL_PRESSURE_PERCENT: f64 = 90.0;

/// Percentage of the window at or above which pressure is [`PressureLevel::High`].
/// Pressure and the requested compaction trigger are window-relative. The
/// trigger is clamped to the spendable input ceiling, so it can fire before
/// this UI boundary when output reservation consumes substantial window space.
pub const HIGH_PRESSURE_PERCENT: f64 = DEFAULT_COMPACTION_TRIGGER_PERCENT;

/// Percentage of the window at or above which pressure is [`PressureLevel::Medium`].
/// Matches the "moderate" boundary of the existing diagnostic report.
pub const MEDIUM_PRESSURE_PERCENT: f64 = 40.0;

/// Safety headroom (tokens) subtracted from the window in addition to the
/// reserved output, to avoid bumping a provider's hard limit. Matches the
/// engine's `CONTEXT_HEADROOM_TOKENS`.
pub const CONTEXT_HEADROOM_TOKENS: u64 = 1_024;

/// Smallest input budget (tokens) [`ContextBudget`] will report for any window
/// large enough to hold it. The output cap is clamped down as needed to
/// preserve this much input room, so a generous configured output cap can never
/// drive the available input budget to zero on a usable window.
pub const MIN_INPUT_BUDGET_TOKENS: u64 = 1_024;

/// Coarse, UI-facing description of how full the context window is.
///
/// Ordered from least to most pressure so the variants can be compared
/// (`level >= PressureLevel::High`) and so the derived `Ord` matches intuition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PressureLevel {
    /// Plenty of room; nothing to surface.
    Low,
    /// Noticeably filling up; informational.
    Medium,
    /// At or past the compaction trigger; suggest compaction.
    High,
    /// Near the window limit; compaction/clear is urgent.
    Critical,
}

impl PressureLevel {
    /// Classify a window-usage percentage (0.0..=100.0) into a level.
    ///
    /// Inputs outside the range are clamped, so callers may pass a raw
    /// percentage without pre-validating it.
    #[must_use]
    pub fn from_usage_percent(percent: f64) -> Self {
        let percent = percent.clamp(0.0, 100.0);
        if percent >= CRITICAL_PRESSURE_PERCENT {
            PressureLevel::Critical
        } else if percent >= HIGH_PRESSURE_PERCENT {
            PressureLevel::High
        } else if percent >= MEDIUM_PRESSURE_PERCENT {
            PressureLevel::Medium
        } else {
            PressureLevel::Low
        }
    }

    /// Lowercase, stable label suitable for status lines and logs.
    ///
    /// Kept aligned with the existing context-report vocabulary
    /// (`low`/`moderate`/`high`/`critical`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PressureLevel::Low => "low",
            PressureLevel::Medium => "moderate",
            PressureLevel::High => "high",
            PressureLevel::Critical => "critical",
        }
    }

    /// Whether this level is at or past the point where compaction should be
    /// suggested to the user.
    ///
    /// Unused by the engine today; kept as the pressure-level counterpart of
    /// `ContextBudget::should_compact` so both live next to their thresholds.
    #[must_use]
    pub const fn suggests_compaction(self) -> bool {
        matches!(self, PressureLevel::High | PressureLevel::Critical)
    }
}

/// A computed snapshot of how a turn's input sits against a model's context
/// window, plus the derived output cap, compaction trigger, and pressure level.
///
/// Construct via [`ContextBudget::new`]. All fields are token counts unless the
/// name says otherwise. The struct is `Copy` and holds no borrowed data so it
/// can be cached on UI state cheaply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// Total context window for the active route (input + output), in tokens.
    pub window_tokens: u64,
    /// Current estimated input tokens already committed to the turn.
    pub input_tokens: u64,
    /// Output tokens reserved (and thus the effective output cap) for the turn.
    /// Derived from the configured cap, clamped to fit the window while leaving
    /// at least [`MIN_INPUT_BUDGET_TOKENS`] of input room.
    pub output_cap_tokens: u64,
    /// Spendable input ceiling for the turn (`window - output_cap - headroom`,
    /// saturating at 0). Compaction percentages use the full window and clamp
    /// their resulting token threshold to this ceiling.
    pub input_budget_ceiling: u64,
    /// Input tokens still available before hitting the reserved boundary
    /// (`input_budget_ceiling - input`, saturating at 0).
    pub available_input_tokens: u64,
    /// Input-token level at or above which compaction should be suggested
    /// (`DEFAULT_COMPACTION_TRIGGER_PERCENT` of the window, clamped to the
    /// input budget ceiling).
    pub compaction_trigger_tokens: u64,
    /// Coarse pressure level derived from window usage.
    pub pressure: PressureLevel,
}

impl ContextBudget {
    /// Build a budget snapshot for a route.
    ///
    /// * `window_tokens` — the route-effective context window (input + output).
    /// * `input_tokens` — current estimated input tokens for the turn.
    /// * `configured_output_cap` — the route-effective output request the
    ///   caller needs to reserve. It is clamped down so it never consumes the
    ///   headroom or the minimum input budget; on a window too small to hold
    ///   even the minimum input budget plus headroom, the cap collapses to
    ///   whatever is left (possibly zero).
    ///
    /// Never panics and never underflows: all arithmetic saturates.
    #[must_use]
    pub fn new(window_tokens: u64, input_tokens: u64, configured_output_cap: u64) -> Self {
        Self::new_with_input_limit(window_tokens, input_tokens, configured_output_cap, None)
    }

    /// Build a budget snapshot and intersect it with a provider's independent
    /// hard input ceiling when one is published by the resolved route.
    #[must_use]
    pub fn new_with_input_limit(
        window_tokens: u64,
        input_tokens: u64,
        configured_output_cap: u64,
        input_limit_tokens: Option<u64>,
    ) -> Self {
        let output_cap_tokens = clamp_output_cap(window_tokens, configured_output_cap);

        // Reserve output + safety headroom; whatever remains is spendable input.
        let reserved = output_cap_tokens.saturating_add(CONTEXT_HEADROOM_TOKENS);
        let window_input_ceiling = window_tokens.saturating_sub(reserved);
        let input_budget_ceiling = input_limit_tokens
            .filter(|limit| *limit > 0)
            .map_or(window_input_ceiling, |limit| {
                window_input_ceiling.min(limit)
            });
        let available_input_tokens = input_budget_ceiling.saturating_sub(input_tokens);

        let compaction_trigger_tokens =
            percent_of(window_tokens, DEFAULT_COMPACTION_TRIGGER_PERCENT).min(input_budget_ceiling);

        let pressure =
            PressureLevel::from_usage_percent(usage_percent(window_tokens, input_tokens));

        ContextBudget {
            window_tokens,
            input_tokens,
            output_cap_tokens,
            input_budget_ceiling,
            available_input_tokens,
            compaction_trigger_tokens,
            pressure,
        }
    }

    /// Derive a compaction trigger from a window percentage.
    ///
    /// The percentage means what the user-facing context meter says it means:
    /// a fraction of the full route window (`80` on a 1M window is 800K input
    /// tokens before the route ceiling clamp). One internal clamp applies:
    ///
    /// ```text
    /// trigger = min(window × percent, window − output reservation − headroom)
    /// ```
    ///
    /// so a late percentage can never push the trigger past the spendable
    /// input ceiling and overflow the provider window. Previously the
    /// percentage was applied to the ceiling itself, which silently pulled an
    /// "80%" setting down to ~60% of the window on routes with large output
    /// reservations; UI surfaces should disclose the clamp when it engages
    /// instead of redefining what the percentage means.
    #[must_use]
    pub fn compaction_trigger_for_percent(&self, percent: f64) -> u64 {
        percent_of(self.window_tokens, percent).min(self.input_budget_ceiling)
    }

    /// Fraction of the window currently consumed by input, as a percentage in
    /// `0.0..=100.0`. A zero window reports `0.0` rather than dividing by zero.
    #[must_use]
    pub fn usage_percent(&self) -> f64 {
        usage_percent(self.window_tokens, self.input_tokens)
    }

    /// Whether current input has reached the compaction trigger and compaction
    /// should be suggested.
    #[must_use]
    pub fn should_compact(&self) -> bool {
        self.window_tokens > 0 && self.input_tokens >= self.compaction_trigger_tokens
    }

    /// Whether another `additional_input_tokens` of input would fit within the
    /// available budget (i.e. not exceed the reserved boundary).
    #[must_use]
    pub fn fits_additional(&self, additional_input_tokens: u64) -> bool {
        additional_input_tokens <= self.available_input_tokens
    }
}

/// Clamp a desired output cap so it fits the window while preserving at least
/// [`MIN_INPUT_BUDGET_TOKENS`] of input room plus [`CONTEXT_HEADROOM_TOKENS`].
///
/// On a window too small to hold even that floor, returns whatever room is left
/// after the headroom (possibly zero) rather than underflowing.
fn clamp_output_cap(window_tokens: u64, configured_output_cap: u64) -> u64 {
    // The most output we can reserve and still keep the input floor + headroom.
    let reserved_floor = MIN_INPUT_BUDGET_TOKENS.saturating_add(CONTEXT_HEADROOM_TOKENS);
    let max_output = window_tokens.saturating_sub(reserved_floor);
    configured_output_cap.min(max_output)
}

/// Window usage as a percentage in `0.0..=100.0`. Zero window -> `0.0`.
pub fn usage_percent(window_tokens: u64, input_tokens: u64) -> f64 {
    if window_tokens == 0 {
        return 0.0;
    }
    ((input_tokens as f64 / window_tokens as f64) * 100.0).clamp(0.0, 100.0)
}

/// `percent`% of `window_tokens`, rounded to the nearest token. Saturates at
/// `u64::MAX` and treats out-of-range percentages by clamping to `0.0..=100.0`.
fn percent_of(window_tokens: u64, percent: f64) -> u64 {
    let percent = percent.clamp(0.0, 100.0);
    let value = (window_tokens as f64) * (percent / 100.0);
    // `as u64` saturates on overflow and floors; add 0.5 to round to nearest.
    (value + 0.5) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative spread of real-world windows: a tight self-hosted
    /// deployment, common provider sizes, and a V4-class 1M window.
    const WINDOWS: &[u64] = &[8_192, 32_768, 131_072, 262_144, 1_048_576];

    // -- PressureLevel boundaries ------------------------------------------

    #[test]
    fn pressure_level_boundaries_are_inclusive_lower_bounds() {
        assert_eq!(PressureLevel::from_usage_percent(0.0), PressureLevel::Low);
        assert_eq!(PressureLevel::from_usage_percent(39.9), PressureLevel::Low);
        // 40% is the moderate boundary.
        assert_eq!(
            PressureLevel::from_usage_percent(40.0),
            PressureLevel::Medium
        );
        assert_eq!(
            PressureLevel::from_usage_percent(74.9),
            PressureLevel::Medium
        );
        // 75% is the high / compaction boundary.
        assert_eq!(PressureLevel::from_usage_percent(75.0), PressureLevel::High);
        assert_eq!(PressureLevel::from_usage_percent(89.9), PressureLevel::High);
        // 90% is the critical boundary.
        assert_eq!(
            PressureLevel::from_usage_percent(90.0),
            PressureLevel::Critical
        );
        assert_eq!(
            PressureLevel::from_usage_percent(100.0),
            PressureLevel::Critical
        );
    }

    #[test]
    fn pressure_level_clamps_out_of_range_inputs() {
        assert_eq!(PressureLevel::from_usage_percent(-10.0), PressureLevel::Low);
        assert_eq!(
            PressureLevel::from_usage_percent(150.0),
            PressureLevel::Critical
        );
        assert_eq!(
            PressureLevel::from_usage_percent(f64::INFINITY),
            PressureLevel::Critical
        );
    }

    #[test]
    fn pressure_level_ordering_and_helpers() {
        assert!(PressureLevel::Low < PressureLevel::Medium);
        assert!(PressureLevel::Medium < PressureLevel::High);
        assert!(PressureLevel::High < PressureLevel::Critical);

        assert!(!PressureLevel::Low.suggests_compaction());
        assert!(!PressureLevel::Medium.suggests_compaction());
        assert!(PressureLevel::High.suggests_compaction());
        assert!(PressureLevel::Critical.suggests_compaction());

        assert_eq!(PressureLevel::Low.label(), "low");
        assert_eq!(PressureLevel::Medium.label(), "moderate");
        assert_eq!(PressureLevel::High.label(), "high");
        assert_eq!(PressureLevel::Critical.label(), "critical");
    }

    // -- Compaction trigger -------------------------------------------------

    #[test]
    fn compaction_trigger_is_window_percent_clamped_to_input_ceiling() {
        for &window in WINDOWS {
            let budget = ContextBudget::new(window, 0, 64_000);
            let expected = percent_of(window, DEFAULT_COMPACTION_TRIGGER_PERCENT)
                .min(budget.input_budget_ceiling);
            assert_eq!(
                budget.compaction_trigger_tokens, expected,
                "window {window}: trigger should be 75% of the window, clamped to the ceiling"
            );
            assert!(budget.compaction_trigger_tokens <= budget.input_budget_ceiling);
        }
    }

    #[test]
    fn should_compact_flips_at_the_trigger() {
        let window = 1_048_576;
        let cap = 262_144;
        let trigger = ContextBudget::new(window, 0, cap).compaction_trigger_tokens;
        assert!(trigger > 0);

        let below = ContextBudget::new(window, trigger - 1, cap);
        assert!(!below.should_compact());

        let at = ContextBudget::new(window, trigger, cap);
        assert!(at.should_compact());

        let above = ContextBudget::new(window, trigger + 1, cap);
        assert!(above.should_compact());
    }

    #[test]
    fn trigger_never_exceeds_input_ceiling() {
        const WINDOWS: &[u64] = &[
            0, 512, 8_192, 65_536, 131_072, 262_144, 500_000, 1_048_576, 2_000_000,
        ];
        const CAPS: &[u64] = &[0, 1_024, 64_000, 131_072, 262_144, 1_000_000];
        const INPUTS: &[u64] = &[0, 1_024, 200_000, 2_000_000];
        const PERCENTS: &[f64] = &[10.0, 50.0, 75.0, 80.0, 95.0, 100.0];

        for &window in WINDOWS {
            for &cap in CAPS {
                for &input in INPUTS {
                    let budget = ContextBudget::new(window, input, cap);
                    assert!(budget.compaction_trigger_tokens <= budget.input_budget_ceiling);
                    for &percent in PERCENTS {
                        assert!(
                            budget.compaction_trigger_for_percent(percent)
                                <= budget.input_budget_ceiling,
                            "window={window} cap={cap} input={input} percent={percent}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn zero_window_never_suggests_compaction() {
        let budget = ContextBudget::new(0, 0, 64_000);
        assert_eq!(budget.compaction_trigger_tokens, 0);
        assert_eq!(budget.input_budget_ceiling, 0);
        assert!(!budget.should_compact());
        assert_eq!(budget.pressure, PressureLevel::Low);
        assert_eq!(budget.available_input_tokens, 0);
        assert_eq!(budget.usage_percent(), 0.0);
    }

    // -- Output cap clamping & available budget ----------------------------

    #[test]
    fn output_cap_is_preserved_when_window_is_roomy() {
        // 1M window, 64K configured cap: cap fits comfortably.
        let budget = ContextBudget::new(1_048_576, 0, 64_000);
        assert_eq!(budget.output_cap_tokens, 64_000);
        // available = window - cap - headroom - input
        let expected = 1_048_576 - 64_000 - CONTEXT_HEADROOM_TOKENS;
        assert_eq!(budget.available_input_tokens, expected);
    }

    #[test]
    fn output_cap_is_clamped_to_protect_input_floor_on_small_window() {
        // This is the engine's hard-won lesson: a generous output reservation
        // on a small window must not underflow the input budget. An 8,192-token
        // window with a 262,144-token desired cap must still leave the input
        // floor available rather than collapsing to zero or wrapping.
        let window = 8_192u64;
        let budget = ContextBudget::new(window, 0, 262_144);

        let reserved_floor = MIN_INPUT_BUDGET_TOKENS + CONTEXT_HEADROOM_TOKENS;
        let expected_cap = window - reserved_floor;
        assert_eq!(budget.output_cap_tokens, expected_cap);
        // With zero input committed, the whole remaining budget is available
        // and is at least the protected floor.
        assert!(budget.available_input_tokens >= MIN_INPUT_BUDGET_TOKENS);
        assert_eq!(
            budget.available_input_tokens,
            window - budget.output_cap_tokens - CONTEXT_HEADROOM_TOKENS
        );
    }

    #[test]
    fn tiny_window_below_floor_saturates_without_panic() {
        // Window smaller than the protected floor: cap collapses to 0 and the
        // available budget saturates at 0 instead of underflowing.
        let window = 512u64; // < MIN_INPUT_BUDGET_TOKENS + headroom
        let budget = ContextBudget::new(window, 100, 262_144);
        assert_eq!(budget.output_cap_tokens, 0);
        assert_eq!(budget.available_input_tokens, 0);
        // Usage still computes a sane percentage.
        assert!((budget.usage_percent() - (100.0 / 512.0 * 100.0)).abs() < 1e-9);
    }

    #[test]
    fn available_budget_saturates_when_input_exceeds_ceiling() {
        let window = 131_072u64;
        let cap = 32_000u64;
        // Commit far more input than the window holds.
        let budget = ContextBudget::new(window, window * 2, cap);
        assert_eq!(budget.available_input_tokens, 0);
        assert!(!budget.fits_additional(1));
        assert!(budget.fits_additional(0));
        // Usage is clamped to 100%.
        assert_eq!(budget.usage_percent(), 100.0);
        assert_eq!(budget.pressure, PressureLevel::Critical);
    }

    #[test]
    fn fits_additional_respects_the_reserved_boundary() {
        let window = 262_144u64;
        let cap = 64_000u64;
        let budget = ContextBudget::new(window, 100_000, cap);
        let room = budget.available_input_tokens;
        assert!(budget.fits_additional(room));
        assert!(!budget.fits_additional(room + 1));
    }

    // -- Usage percent & pressure across window sizes ----------------------

    #[test]
    fn usage_percent_is_proportional_across_window_sizes() {
        for &window in WINDOWS {
            // Half-full should read ~50% and classify as Medium regardless of
            // absolute window size.
            let half = window / 2;
            let budget = ContextBudget::new(window, half, 64_000);
            let pct = budget.usage_percent();
            assert!(
                (pct - 50.0).abs() < 0.5,
                "window {window}: half-full should be ~50%, got {pct}"
            );
            assert_eq!(budget.pressure, PressureLevel::Medium);
        }
    }

    #[test]
    fn pressure_tracks_input_growth_on_a_1m_window() {
        let window = 1_048_576u64;
        let cap = 262_144u64;

        let low = ContextBudget::new(window, percent_of(window, 10.0), cap);
        assert_eq!(low.pressure, PressureLevel::Low);

        let medium = ContextBudget::new(window, percent_of(window, 50.0), cap);
        assert_eq!(medium.pressure, PressureLevel::Medium);

        let high = ContextBudget::new(window, percent_of(window, 80.0), cap);
        assert_eq!(high.pressure, PressureLevel::High);
        assert!(high.should_compact());

        let critical = ContextBudget::new(window, percent_of(window, 95.0), cap);
        assert_eq!(critical.pressure, PressureLevel::Critical);
    }

    #[test]
    fn snapshot_fields_are_internally_consistent() {
        for &window in WINDOWS {
            for &input in &[0u64, window / 4, window / 2, window] {
                let budget = ContextBudget::new(window, input, 64_000);
                // Field mirrors the constructor arguments.
                assert_eq!(budget.window_tokens, window);
                assert_eq!(budget.input_tokens, input);
                // available + input never claims more than the window minus
                // reserved output and headroom.
                let ceiling = window
                    .saturating_sub(budget.output_cap_tokens)
                    .saturating_sub(CONTEXT_HEADROOM_TOKENS);
                assert!(budget.available_input_tokens <= ceiling);
                // Pressure agrees with the standalone usage percentage.
                assert_eq!(
                    budget.pressure,
                    PressureLevel::from_usage_percent(budget.usage_percent())
                );
            }
        }
    }

    #[test]
    fn percent_of_rounds_to_nearest_token() {
        assert_eq!(percent_of(0, 75.0), 0);
        assert_eq!(percent_of(100, 0.0), 0);
        assert_eq!(percent_of(100, 100.0), 100);
        assert_eq!(percent_of(100, 75.0), 75);
        // 3 * 0.75 = 2.25 -> rounds to 2.
        assert_eq!(percent_of(3, 75.0), 2);
        // 2 * 0.75 = 1.5 -> rounds to 2.
        assert_eq!(percent_of(2, 75.0), 2);
    }

    #[test]
    fn budget_is_copy_and_comparable() {
        let a = ContextBudget::new(131_072, 1_000, 32_000);
        let b = a; // Copy, not move.
        assert_eq!(a, b);
        assert_eq!(a.window_tokens, b.window_tokens);
    }
}
