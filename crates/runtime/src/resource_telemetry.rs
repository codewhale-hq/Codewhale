#![allow(dead_code)]

//! Resource-usage telemetry for long-running CodeWhale tasks.
//!
//! This module is a pure, side-effect-free foundation for surfacing how many
//! tokens and how much wall-clock time a task has consumed, optionally relative
//! to a budget. It performs no I/O and no rendering; consumers (status lines,
//! the cost panel, the goal/budget tooling) are wired up separately so the
//! formatting and pressure logic can be unit-tested in isolation.
//!
//! The shape intentionally mirrors the budget vocabulary already used by the
//! goal tooling (`token_budget: Option<_>`) so a consumer can adapt between the
//! two without inventing new concepts. We keep a local type rather than reusing
//! `tools::goal` here to avoid coupling a presentation-layer helper to the tool
//! domain model (whose budgets are `u32` and carry unrelated bookkeeping).

use std::{
    fmt::{self, Write as _},
    time::Duration,
};

/// A coarse, three-level read on how close a task is to exhausting its budget.
///
/// The level is derived from the *highest* pressure across all bounded
/// dimensions (tokens and time), so a task that is comfortable on tokens but
/// nearly out of time still reports [`PressureLevel::High`]. When nothing is
/// bounded, pressure is [`PressureLevel::Low`] by definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureLevel {
    /// Plenty of headroom (under ~75% of every bounded budget).
    Low,
    /// Getting close (at/over ~75% but under 100% of some budget).
    Medium,
    /// At or over budget on some bounded dimension.
    High,
}

impl PressureLevel {
    /// Fraction at/above which a dimension is considered medium pressure.
    const MEDIUM_THRESHOLD: f64 = 0.75;
    /// Fraction at/above which a dimension is considered high pressure.
    const HIGH_THRESHOLD: f64 = 1.0;

    /// Classify a single budget fraction (e.g. `0.41` for 41% used).
    ///
    /// Negative or non-finite input is treated as [`PressureLevel::Low`]; the
    /// telemetry helpers never produce such values, but classifying defensively
    /// keeps this usable for arbitrary callers.
    fn from_fraction(fraction: f64) -> Self {
        if !fraction.is_finite() || fraction < Self::MEDIUM_THRESHOLD {
            PressureLevel::Low
        } else if fraction < Self::HIGH_THRESHOLD {
            PressureLevel::Medium
        } else {
            PressureLevel::High
        }
    }

    /// A short lowercase label suitable for compact status output.
    pub fn label(self) -> &'static str {
        match self {
            PressureLevel::Low => "low",
            PressureLevel::Medium => "medium",
            PressureLevel::High => "high",
        }
    }
}

/// A snapshot of token and time usage for a single task, with optional budgets.
///
/// All fields are plain counters; this type owns no clock and reads no
/// environment. Construct it from whatever the caller is already tracking and
/// use the helpers below to render or classify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceTelemetry {
    /// Total tokens consumed so far.
    pub tokens_used: u64,
    /// Total wall-clock seconds elapsed so far.
    pub time_used_seconds: u64,
    /// Optional token ceiling for the task; `None` means unbounded.
    pub token_budget: Option<u64>,
    /// Optional time ceiling in seconds; `None` means unbounded.
    pub time_budget_seconds: Option<u64>,
}

impl ResourceTelemetry {
    /// Create a telemetry snapshot with no budgets (fully unbounded).
    pub fn new(tokens_used: u64, time_used_seconds: u64) -> Self {
        Self {
            tokens_used,
            time_used_seconds,
            token_budget: None,
            time_budget_seconds: None,
        }
    }

    /// Set the token budget, returning the updated snapshot (builder style).
    pub fn with_token_budget(mut self, budget: u64) -> Self {
        self.token_budget = Some(budget);
        self
    }

    /// Set the time budget in seconds, returning the updated snapshot.
    pub fn with_time_budget_seconds(mut self, seconds: u64) -> Self {
        self.time_budget_seconds = Some(seconds);
        self
    }

    /// Fraction of the token budget consumed, or `None` when unbounded.
    ///
    /// A zero budget yields `None` (a percentage of nothing is meaningless)
    /// rather than infinity, keeping every downstream consumer safe.
    pub fn token_fraction(&self) -> Option<f64> {
        fraction(self.tokens_used, self.token_budget)
    }

    /// Fraction of the time budget consumed, or `None` when unbounded.
    pub fn time_fraction(&self) -> Option<f64> {
        fraction(self.time_used_seconds, self.time_budget_seconds)
    }

    /// The largest bounded budget fraction across tokens and time.
    ///
    /// Returns `None` only when *neither* dimension is bounded. When at least
    /// one budget is present, the most-pressured bounded dimension wins.
    pub fn budget_fraction(&self) -> Option<f64> {
        match (self.token_fraction(), self.time_fraction()) {
            (Some(t), Some(s)) => Some(t.max(s)),
            (Some(t), None) => Some(t),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        }
    }

    /// Budget fraction expressed as a whole-number percent (rounded), or `None`
    /// when unbounded. This is the value surfaced in the human summary.
    pub fn budget_percent(&self) -> Option<u64> {
        self.budget_fraction().map(|f| (f * 100.0).round() as u64)
    }

    /// Coarse pressure level derived from [`Self::budget_fraction`].
    ///
    /// Unbounded tasks are always [`PressureLevel::Low`].
    pub fn pressure(&self) -> PressureLevel {
        match self.budget_fraction() {
            Some(fraction) => PressureLevel::from_fraction(fraction),
            None => PressureLevel::Low,
        }
    }

    /// A compact, human-readable one-liner, e.g. `12.3k tok · 4m12s · 41% budget`.
    ///
    /// Tokens are abbreviated with `k`/`M` suffixes, time is rendered as
    /// `Hh Mm Ss` (dropping leading zero units), and the budget segment is
    /// omitted entirely when the task is unbounded.
    pub fn human_summary(&self) -> String {
        let mut out = String::new();
        // `write!` into a String is infallible; ignore the Result.
        let _ = write!(
            out,
            "{} tok · {}",
            format_tokens(self.tokens_used),
            codewhale_command_contract::elapsed::format_elapsed_secs(self.time_used_seconds),
        );
        if let Some(percent) = self.budget_percent() {
            let _ = write!(out, " · {percent}% budget");
        }
        out
    }
}

impl fmt::Display for ResourceTelemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.human_summary())
    }
}

/// Output-token throughput for a live or completed turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenThroughput {
    pub output_tokens: u64,
    pub elapsed_seconds: f64,
}

impl TokenThroughput {
    pub fn new(output_tokens: u64, elapsed: Duration) -> Option<Self> {
        let elapsed_seconds = elapsed.as_secs_f64();
        if output_tokens == 0 || !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
            return None;
        }
        Some(Self {
            output_tokens,
            elapsed_seconds,
        })
    }

    pub fn from_estimated_text(text: &str, elapsed: Duration) -> Option<Self> {
        Self::new(estimate_output_tokens_from_text(text), elapsed)
    }

    pub fn tokens_per_second(self) -> f64 {
        self.output_tokens as f64 / self.elapsed_seconds
    }

    pub fn compact_rate(self) -> String {
        let rate = self.tokens_per_second();
        if rate < 10.0 {
            format!("{rate:.1}")
        } else {
            format!("{rate:.0}")
        }
    }
}

/// Estimate output tokens from streamed text before provider usage arrives.
///
/// Provider-reported usage remains canonical at turn completion. During a live
/// stream, this gives the footer a stable approximation without inspecting
/// provider-specific tokenizer internals.
pub fn estimate_output_tokens_from_text(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    if chars == 0 {
        0
    } else {
        chars.saturating_add(3) / 4
    }
}

/// Divide `used` by an optional budget, guarding against an absent or zero
/// budget. Returns `None` when the budget is `None` or `0`.
fn fraction(used: u64, budget: Option<u64>) -> Option<f64> {
    match budget {
        Some(budget) if budget > 0 => Some(used as f64 / budget as f64),
        _ => None,
    }
}

/// Format a token count with a `k`/`M` suffix once it crosses each threshold.
///
/// Values under 1_000 are printed verbatim. Thousands use one decimal place
/// (`12.3k`), trimming a trailing `.0` so round values read cleanly (`5k`).
/// Millions follow the same rule (`1.5M`, `2M`).
fn format_tokens(tokens: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000_000;
    if tokens >= M {
        format_scaled(tokens, M, 'M')
    } else if tokens >= K {
        format_scaled(tokens, K, 'k')
    } else {
        tokens.to_string()
    }
}

/// Render `value / divisor` to one decimal place with `suffix`, dropping a
/// trailing `.0`. The divisor is always one of the constants above (non-zero).
fn format_scaled(value: u64, divisor: u64, suffix: char) -> String {
    let scaled = value as f64 / divisor as f64;
    // Round to one decimal before deciding whether the fraction is ".0", so a
    // value like 1_999_999 reads as "2M" rather than "1.9...M".
    let rounded = (scaled * 10.0).round() / 10.0;
    if (rounded.fract()).abs() < f64::EPSILON {
        format!("{}{}", rounded as u64, suffix)
    } else {
        format!("{rounded:.1}{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- token formatting -------------------------------------------------

    #[test]
    fn format_tokens_under_a_thousand_is_verbatim() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(1), "1");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_uses_k_suffix_with_trimmed_decimal() {
        assert_eq!(format_tokens(1_000), "1k");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(12_345), "12.3k");
        // Exactly on a round thousand trims the ".0".
        assert_eq!(format_tokens(5_000), "5k");
        // Just under the millions boundary stays in k.
        assert_eq!(format_tokens(999_400), "999.4k");
    }

    #[test]
    fn format_tokens_uses_m_suffix_for_millions() {
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        assert_eq!(format_tokens(2_340_000), "2.3M");
    }

    #[test]
    fn format_tokens_rounds_up_across_a_unit_boundary() {
        // 1_999_999 rounds to 2.0M -> "2M", not "1.9M" or "2.0M".
        assert_eq!(format_tokens(1_999_999), "2M");
        // 999_950 rounds to 1000.0k; still within the k branch and trims ".0".
        assert_eq!(format_tokens(999_950), "1000k");
    }

    #[test]
    fn format_tokens_handles_very_large_values() {
        assert_eq!(format_tokens(u64::MAX), "18446744073709.6M");
    }

    // ---- throughput -------------------------------------------------------

    #[test]
    fn token_throughput_formats_compact_rates() {
        let throughput = TokenThroughput::new(120, Duration::from_secs(6)).expect("throughput");
        assert_eq!(throughput.tokens_per_second(), 20.0);
        assert_eq!(throughput.compact_rate(), "20");

        let slow = TokenThroughput::new(15, Duration::from_secs(4)).expect("throughput");
        assert_eq!(slow.compact_rate(), "3.8");
    }

    #[test]
    fn token_throughput_rejects_empty_or_zero_elapsed_samples() {
        assert!(TokenThroughput::new(0, Duration::from_secs(5)).is_none());
        assert!(TokenThroughput::new(5, Duration::ZERO).is_none());
    }

    #[test]
    fn estimated_streaming_tokens_round_up_from_text_chars() {
        assert_eq!(estimate_output_tokens_from_text(""), 0);
        assert_eq!(estimate_output_tokens_from_text("abc"), 1);
        assert_eq!(estimate_output_tokens_from_text("abcd"), 1);
        assert_eq!(estimate_output_tokens_from_text("abcde"), 2);

        let throughput =
            TokenThroughput::from_estimated_text(&"x".repeat(400), Duration::from_secs(10))
                .expect("estimated throughput");
        assert_eq!(throughput.output_tokens, 100);
        assert_eq!(throughput.compact_rate(), "10");
    }

    // ---- fraction / percent ----------------------------------------------

    #[test]
    fn fractions_are_none_when_unbounded() {
        let t = ResourceTelemetry::new(5_000, 120);
        assert_eq!(t.token_fraction(), None);
        assert_eq!(t.time_fraction(), None);
        assert_eq!(t.budget_fraction(), None);
        assert_eq!(t.budget_percent(), None);
    }

    #[test]
    fn zero_budget_yields_none_not_infinity() {
        let t = ResourceTelemetry {
            tokens_used: 100,
            time_used_seconds: 0,
            token_budget: Some(0),
            time_budget_seconds: Some(0),
        };
        assert_eq!(t.token_fraction(), None);
        assert_eq!(t.time_fraction(), None);
        assert_eq!(t.budget_fraction(), None);
        assert_eq!(t.pressure(), PressureLevel::Low);
    }

    #[test]
    fn token_fraction_is_computed_when_bounded() {
        let t = ResourceTelemetry::new(4_100, 0).with_token_budget(10_000);
        let frac = t.token_fraction().expect("bounded");
        assert!((frac - 0.41).abs() < 1e-9, "got {frac}");
        assert_eq!(t.budget_percent(), Some(41));
    }

    #[test]
    fn budget_fraction_takes_the_max_across_dimensions() {
        // Tokens at 10%, time at 80% -> the time pressure dominates.
        let t = ResourceTelemetry {
            tokens_used: 1_000,
            time_used_seconds: 80,
            token_budget: Some(10_000),
            time_budget_seconds: Some(100),
        };
        let frac = t.budget_fraction().expect("bounded");
        assert!((frac - 0.80).abs() < 1e-9, "got {frac}");
        assert_eq!(t.budget_percent(), Some(80));
    }

    #[test]
    fn budget_fraction_present_when_only_one_dimension_bounded() {
        let only_time = ResourceTelemetry::new(9_999, 50).with_time_budget_seconds(200);
        assert_eq!(only_time.budget_percent(), Some(25));

        let only_tokens = ResourceTelemetry::new(2_500, 9_999).with_token_budget(10_000);
        assert_eq!(only_tokens.budget_percent(), Some(25));
    }

    #[test]
    fn budget_percent_rounds_to_nearest_whole() {
        // 333 / 1000 = 33.3% -> 33
        let down = ResourceTelemetry::new(333, 0).with_token_budget(1_000);
        assert_eq!(down.budget_percent(), Some(33));
        // 336 / 1000 = 33.6% -> 34
        let up = ResourceTelemetry::new(336, 0).with_token_budget(1_000);
        assert_eq!(up.budget_percent(), Some(34));
    }

    // ---- pressure levels --------------------------------------------------

    #[test]
    fn pressure_low_when_unbounded_regardless_of_usage() {
        let t = ResourceTelemetry::new(u64::MAX, u64::MAX);
        assert_eq!(t.pressure(), PressureLevel::Low);
    }

    #[test]
    fn pressure_thresholds_just_under_and_over() {
        // 74% -> Low (just under the medium threshold).
        let low = ResourceTelemetry::new(7_400, 0).with_token_budget(10_000);
        assert_eq!(low.pressure(), PressureLevel::Low);

        // Exactly 75% -> Medium (inclusive lower bound).
        let medium_edge = ResourceTelemetry::new(7_500, 0).with_token_budget(10_000);
        assert_eq!(medium_edge.pressure(), PressureLevel::Medium);

        // 99% -> Medium (just under the high threshold).
        let medium = ResourceTelemetry::new(9_900, 0).with_token_budget(10_000);
        assert_eq!(medium.pressure(), PressureLevel::Medium);

        // Exactly 100% -> High (at budget).
        let high_edge = ResourceTelemetry::new(10_000, 0).with_token_budget(10_000);
        assert_eq!(high_edge.pressure(), PressureLevel::High);

        // Over budget -> High.
        let over = ResourceTelemetry::new(12_500, 0).with_token_budget(10_000);
        assert_eq!(over.pressure(), PressureLevel::High);
    }

    #[test]
    fn pressure_level_labels_and_ordering() {
        assert_eq!(PressureLevel::Low.label(), "low");
        assert_eq!(PressureLevel::Medium.label(), "medium");
        assert_eq!(PressureLevel::High.label(), "high");
        // Ord derive: Low < Medium < High.
        assert!(PressureLevel::Low < PressureLevel::Medium);
        assert!(PressureLevel::Medium < PressureLevel::High);
    }

    #[test]
    fn pressure_from_fraction_ignores_non_finite() {
        assert_eq!(PressureLevel::from_fraction(f64::NAN), PressureLevel::Low);
        assert_eq!(
            PressureLevel::from_fraction(f64::INFINITY),
            PressureLevel::Low
        );
        assert_eq!(PressureLevel::from_fraction(-0.5), PressureLevel::Low);
    }

    // ---- human summary ----------------------------------------------------

    #[test]
    fn human_summary_bounded_matches_example_shape() {
        let t = ResourceTelemetry::new(12_345, 252).with_token_budget(30_000);
        // 12_345 -> "12.3k", 252s -> "4m 12s", 12345/30000 = 41.15% -> 41%.
        assert_eq!(t.human_summary(), "12.3k tok · 4m 12s · 41% budget");
    }

    #[test]
    fn human_summary_unbounded_omits_budget_segment() {
        let t = ResourceTelemetry::new(500, 5);
        assert_eq!(t.human_summary(), "500 tok · 5s");
        // Display delegates to human_summary.
        assert_eq!(t.to_string(), "500 tok · 5s");
    }

    #[test]
    fn human_summary_zero_everything() {
        let t = ResourceTelemetry::default();
        assert_eq!(t.human_summary(), "0 tok · 0s");
    }

    #[test]
    fn human_summary_over_budget_can_exceed_one_hundred_percent() {
        let t = ResourceTelemetry::new(15_000, 7_320).with_token_budget(10_000);
        // 15000/10000 = 150%, 7320s -> 122m 00s.
        assert_eq!(t.human_summary(), "15k tok · 122m 00s · 150% budget");
        assert_eq!(t.pressure(), PressureLevel::High);
    }

    #[test]
    fn human_summary_with_only_time_budget() {
        let t = ResourceTelemetry::new(2_000_000, 300).with_time_budget_seconds(600);
        // 2M tokens, 5m 00s, 300/600 = 50% budget.
        assert_eq!(t.human_summary(), "2M tok · 5m 00s · 50% budget");
        assert_eq!(t.pressure(), PressureLevel::Low);
    }
}
