use serde::{Deserialize, Serialize};

use super::review_core::ReviewToolBudget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewRisk {
    Low,
    Medium,
    High,
}

impl ReviewRisk {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewTriage {
    pub risk: ReviewRisk,
    /// Why this tier was chosen, in the reader's words. Every entry names the
    /// signal that raised it, so a receipt can be audited later.
    pub reasons: Vec<String>,
    pub changed_files: usize,
    pub changed_lines: usize,
    pub touches_tests: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReviewBudget {
    pub risk: ReviewRisk,
    pub max_steps: u32,
    pub max_tool_calls: u32,
    pub max_output_tokens: u32,
    /// `Some(0.0)` means a cap was requested but no price is known, which makes
    /// it unenforceable; that is reported rather than treated as unlimited.
    pub money_cap_usd: Option<f64>,
    pub money_limit_supported: bool,
}

const HIGH_RISK_PATH: &[(&str, &str)] = &[
    ("auth", "authentication or authorization code"),
    ("billing", "billing or payment code"),
    ("payment", "billing or payment code"),
    ("stripe", "billing or payment code"),
    ("migration", "a database migration"),
    ("crypto", "cryptography or key handling"),
    ("secret", "secret handling"),
    ("permission", "permission or policy code"),
    ("policy", "permission or policy code"),
    ("sandbox", "a sandbox or isolation boundary"),
    ("concurren", "concurrency or locking"),
    ("lock", "concurrency or locking"),
    ("delete", "a deletion or destructive path"),
    ("schema", "a persisted schema"),
];

const MEDIUM_RISK_PATH: &[(&str, &str)] = &[
    ("api", "a public or internal API surface"),
    ("route", "a request route"),
    ("server", "server behaviour"),
    ("config", "configuration"),
    ("db", "database access"),
    ("store", "persistence"),
    ("proto", "a protocol definition"),
    ("contract", "a published contract"),
];

const TEST_PATH: &[&str] = &[
    "/test/",
    "/tests/",
    "_test.",
    ".test.",
    "/spec/",
    "/__tests__/",
];

fn path_is_test(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    TEST_PATH.iter().any(|marker| lower.contains(marker))
}

/// Triage reads only the diff's paths and size. It is deliberately cheap: no
/// model call, no repository walk, and no claim about whether the change is
/// correct. It exists to decide how much review to buy, so it must be
/// explainable from the diff alone.
#[must_use]
pub fn triage_review(diff: &str) -> ReviewTriage {
    let mut paths: Vec<String> = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in diff.lines() {
        let header = line
            .strip_prefix("+++ ")
            .map(|rest| rest.trim().trim_start_matches("b/"))
            .or_else(|| {
                line.strip_prefix("--- ")
                    .map(|rest| rest.trim().trim_start_matches("a/"))
            });
        if let Some(path) = header {
            if path != "/dev/null" && !paths.iter().any(|seen| seen == path) {
                paths.push(path.to_string());
            }
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    let changed_files = paths.len();
    let changed_lines = added + removed;
    let touches_tests = paths.iter().any(|path| path_is_test(path));
    let mut reasons = Vec::new();
    let mut high = false;
    let mut medium = false;
    for (marker, reason) in HIGH_RISK_PATH {
        if paths
            .iter()
            .any(|path| path.to_ascii_lowercase().contains(marker))
        {
            high = true;
            if !reasons.iter().any(|seen| seen == reason) {
                reasons.push((*reason).to_string());
            }
        }
    }
    for (marker, reason) in MEDIUM_RISK_PATH {
        if paths
            .iter()
            .any(|path| path.to_ascii_lowercase().contains(marker))
        {
            medium = true;
            if !reasons.iter().any(|seen| seen == reason) {
                reasons.push((*reason).to_string());
            }
        }
    }
    if changed_lines >= 400 || changed_files > 20 {
        high = true;
        reasons.push(format!(
            "a large change ({changed_files} files, {changed_lines} changed lines)"
        ));
    } else if changed_lines >= 80 || changed_files > 6 {
        medium = true;
        reasons.push(format!(
            "a medium-size change ({changed_files} files, {changed_lines} changed lines)"
        ));
    }
    let risk = if high {
        ReviewRisk::High
    } else if medium {
        ReviewRisk::Medium
    } else {
        ReviewRisk::Low
    };
    if reasons.is_empty() {
        reasons.push(format!(
            "a small change ({changed_files} files, {changed_lines} changed lines) with no sensitive path"
        ));
    }
    if touches_tests {
        reasons.push("the change includes tests".to_string());
    }
    ReviewTriage {
        risk,
        reasons,
        changed_files,
        changed_lines,
        touches_tests,
    }
}

pub const REASONING_ROUTE_MIN_OUTPUT_TOKENS: u32 = 8_192;

/// The budget a tier buys. Low risk still gets a real review of the diff, just
/// without repository tools; high risk gets dependent tracing and room to
/// reproduce. Output is never tiered: every review may use the route's whole
/// output limit, so spend is bounded by skipping steps, not by truncation.
#[must_use]
pub fn budget_for(triage: &ReviewTriage, route_output_limit: u32) -> ReviewBudget {
    let (max_steps, max_tool_calls) = match triage.risk {
        ReviewRisk::Low => (1, 0),
        ReviewRisk::Medium => (4, 6),
        ReviewRisk::High => (6, 12),
    };
    ReviewBudget {
        risk: triage.risk,
        max_steps,
        max_tool_calls,
        max_output_tokens: route_output_limit,
        money_cap_usd: None,
        money_limit_supported: false,
    }
}

impl ReviewBudget {
    #[must_use]
    pub fn tool_budget(self) -> ReviewToolBudget {
        ReviewToolBudget {
            max_steps: self.max_steps,
            max_tool_calls: self.max_tool_calls,
            max_tool_result_chars: 40_000,
        }
    }

    /// Attach a per-review money cap. A cap without a price is not enforceable,
    /// and saying so is the point: `money_limit_supported` stays false until a
    /// rate is supplied, so a caller can never report a cap it did not apply.
    #[must_use]
    pub fn with_money_cap(mut self, cap_usd: Option<f64>, rates: Option<(f64, f64)>) -> Self {
        let Some(cap) = cap_usd.filter(|cap| *cap > 0.0 && cap.is_finite()) else {
            return self;
        };
        self.money_cap_usd = Some(cap);
        self.money_limit_supported = rates.is_some_and(|(input, output)| {
            input.is_finite() && output.is_finite() && input >= 0.0 && output >= 0.0
        });
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpendRates {
    pub input_usd_per_million: f64,
    pub output_usd_per_million: f64,
}

impl SpendRates {
    #[must_use]
    pub fn cost_usd(self, usage: &codewhale_models::Usage) -> f64 {
        let input = u64::from(usage.input_tokens)
            + u64::from(usage.prompt_cache_hit_tokens.unwrap_or(0))
            + u64::from(usage.prompt_cache_write_tokens.unwrap_or(0));
        let output = u64::from(usage.output_tokens);
        (input as f64 / 1_000_000.0) * self.input_usd_per_million
            + (output as f64 / 1_000_000.0) * self.output_usd_per_million
    }
}

/// Why a review stopped early. A stop is always reported with what was and was
/// not covered; it is never a silent truncation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStop {
    MoneyCap,
    StepBudget,
    ToolBudget,
    OutputLimit,
    ModelFailure,
}

impl ReviewStop {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MoneyCap => "money_cap",
            Self::StepBudget => "step_budget",
            Self::ToolBudget => "tool_budget",
            Self::OutputLimit => "output_limit",
            Self::ModelFailure => "model_failure",
        }
    }
}

#[must_use]
pub fn partial_review_note(
    stop: &ReviewStop,
    triage: &ReviewTriage,
    budget: &ReviewBudget,
) -> String {
    let what = match stop {
        ReviewStop::MoneyCap => format!(
            "the per-review money cap of ${:.4} was reached",
            budget.money_cap_usd.unwrap_or(0.0)
        ),
        ReviewStop::StepBudget => format!(
            "the {} model step(s) for a {} risk change were used",
            budget.max_steps,
            triage.risk.as_str()
        ),
        ReviewStop::ToolBudget => format!(
            "the {} tool call(s) for a {} risk change were used",
            budget.max_tool_calls,
            triage.risk.as_str()
        ),
        ReviewStop::OutputLimit => "the model exhausted its output allowance".to_string(),
        ReviewStop::ModelFailure => "the model call failed".to_string(),
    };
    format!(
        "This is a named partial review: {what}. Findings below were produced before the stop; files and dependents the reviewer did not reach were not reviewed, and their absence is not evidence that they are correct."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_for(paths: &[&str]) -> String {
        let mut out = String::new();
        for path in paths {
            out.push_str(&format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,2 @@\n context\n+added\n"
            ));
        }
        out
    }

    #[test]
    fn a_docs_change_is_low_risk_and_buys_no_tools() {
        let triage = triage_review(
            "diff --git a/docs/README.md b/docs/README.md\n--- a/docs/README.md\n+++ b/docs/README.md\n@@ -1 +1 @@\n-old\n+new\n",
        );
        assert_eq!(triage.risk, ReviewRisk::Low);
        let budget = budget_for(&triage, 65_536);
        assert_eq!(budget.max_tool_calls, 0);
        assert_eq!(budget.max_steps, 1);
        assert!(!budget.money_limit_supported);
    }

    #[test]
    fn a_billing_migration_is_high_risk_and_buys_repository_tools() {
        let triage = triage_review(&diff_for(&["services/billing/stripe-migration.sql"]));
        assert_eq!(triage.risk, ReviewRisk::High);
        assert!(
            triage
                .reasons
                .iter()
                .any(|reason| reason.contains("billing"))
        );
        assert!(
            triage
                .reasons
                .iter()
                .any(|reason| reason.contains("migration"))
        );
        let budget = budget_for(&triage, 65_536);
        assert!(budget.max_tool_calls >= 12);
        assert!(budget.max_steps >= 6);
    }

    #[test]
    fn size_and_sensitive_paths_raise_the_tier_independently() {
        let wide = diff_for(
            &(0..25)
                .map(|index| format!("src/part{index}.rs"))
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        assert_eq!(triage_review(&wide).risk, ReviewRisk::High);
        let tall: String = format!(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,401 @@\n context\n{}",
            "+line\n".repeat(400)
        );
        assert_eq!(triage_review(&tall).risk, ReviewRisk::High);
        assert_eq!(
            triage_review(&diff_for(&["src/tools/shell.rs"])).risk,
            ReviewRisk::Low
        );
    }

    #[test]
    fn a_money_cap_without_a_price_is_reported_as_unsupported() {
        let triage = triage_review(&diff_for(&["src/auth.rs"]));
        let without_rates = budget_for(&triage, 65_536).with_money_cap(Some(0.25), None);
        assert_eq!(without_rates.money_cap_usd, Some(0.25));
        assert!(!without_rates.money_limit_supported);
        let with_rates = budget_for(&triage, 65_536).with_money_cap(Some(0.25), Some((0.15, 0.60)));
        assert!(with_rates.money_limit_supported);
        let no_cap = budget_for(&triage, 65_536).with_money_cap(None, Some((0.15, 0.60)));
        assert_eq!(no_cap.money_cap_usd, None);
        assert!(!no_cap.money_limit_supported);
    }

    #[test]
    fn cost_uses_every_billable_counter() {
        let usage = codewhale_models::Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            prompt_cache_hit_tokens: Some(1_000_000),
            ..Default::default()
        };
        let cost = SpendRates {
            input_usd_per_million: 0.15,
            output_usd_per_million: 0.60,
        }
        .cost_usd(&usage);
        assert!((cost - (0.30 + 0.60)).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn a_partial_review_names_the_stop_and_the_limit() {
        let triage = triage_review(&diff_for(&["src/auth.rs"]));
        let budget = budget_for(&triage, 65_536).with_money_cap(Some(0.25), Some((0.15, 0.60)));
        let note = partial_review_note(&ReviewStop::MoneyCap, &triage, &budget);
        assert!(note.contains("named partial review"));
        assert!(note.contains("0.2500"));
        assert!(note.contains("not evidence that they are correct"));
    }
}
