use serde::Deserialize;

/// Absolute per-scope default: 100k estimated tokens.
pub const DEFAULT_SCOPE_BUDGET_TOKENS: usize = 100_000;
/// Per-scope default as a share of the usable window.
pub const DEFAULT_SCOPE_BUDGET_PERCENT: u8 = 25;
/// Default ceiling for the whole static prefix, as a share of the usable window.
pub const DEFAULT_STATIC_PREFIX_GUARD_PERCENT: u8 = 80;
/// Hard memory-safety ceiling for one instruction file (8 MiB).
pub const DEFAULT_FILE_MAX_BYTES: u64 = 8 * 1024 * 1024;
const MIN_STATIC_RESIDUAL_PERCENT: u8 = 10;

/// What to do when an instruction scope does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InstructionBudgetPolicy {
    /// Trim the scope and continue with a warning (default).
    #[default]
    Trim,
    /// Stop the session instead of trimming or running an unsatisfiable guard.
    Refuse,
}

impl InstructionBudgetPolicy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trim => "trim",
            Self::Refuse => "refuse",
        }
    }
}

/// The `[instruction_budget]` config table.
///
/// Every field is optional so the table only states what the user changed and
/// the defaults stay in one place. `deny_unknown_fields` makes a typo inside
/// the table fail loudly — the top-level config is deliberately tolerant of
/// unknown *tables* (#6516), so silently accepting `global_budget_token`
/// would hide the mistake forever.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionBudgetConfig {
    /// Absolute ceiling for the global scope, in estimated tokens.
    pub global_budget_tokens: Option<usize>,
    /// Absolute ceiling for the project scope, in estimated tokens.
    pub project_budget_tokens: Option<usize>,
    /// Global scope ceiling as a share of the usable window (1–100).
    pub global_budget_percent: Option<u8>,
    /// Project scope ceiling as a share of the usable window (1–100).
    pub project_budget_percent: Option<u8>,
    /// Ceiling for the whole static prefix as a share of the usable window.
    pub static_prefix_guard_percent: Option<u8>,
    /// Hard per-file ceiling in bytes; a larger file is skipped, not trimmed.
    pub file_max_bytes: Option<u64>,
    /// `trim` (default) or `refuse`.
    pub policy: Option<InstructionBudgetPolicy>,
    /// Read `CLAUDE.md` when no `AGENTS.md`/codewhale file is present.
    pub claude_fallback: Option<bool>,
}

impl InstructionBudgetConfig {
    /// Reject values that cannot mean anything.
    ///
    /// Called while config is applied, so a broken table stops the launch
    /// instead of silently producing a nonsensical budget.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("global_budget_tokens", self.global_budget_tokens),
            ("project_budget_tokens", self.project_budget_tokens),
        ] {
            if value == Some(0) {
                return Err(format!(
                    "instruction_budget.{name} must be positive (0 is not an unlimited budget)"
                ));
            }
        }
        if self
            .file_max_bytes
            .is_some_and(|value| value == 0 || value > DEFAULT_FILE_MAX_BYTES)
        {
            return Err(
                "instruction_budget.file_max_bytes must be between 1 and 8388608 (the hard ceiling cannot be raised)"
                    .to_string(),
            );
        }
        for (name, value) in [
            ("global_budget_percent", self.global_budget_percent),
            ("project_budget_percent", self.project_budget_percent),
            (
                "static_prefix_guard_percent",
                self.static_prefix_guard_percent,
            ),
        ] {
            if let Some(value) = value
                && !(1..=100).contains(&value)
            {
                return Err(format!(
                    "instruction_budget.{name} must be between 1 and 100 (got {value})"
                ));
            }
        }
        let global_percent = self
            .global_budget_percent
            .unwrap_or(DEFAULT_SCOPE_BUDGET_PERCENT);
        let project_percent = self
            .project_budget_percent
            .unwrap_or(DEFAULT_SCOPE_BUDGET_PERCENT);
        let guard = self
            .static_prefix_guard_percent
            .unwrap_or(DEFAULT_STATIC_PREFIX_GUARD_PERCENT);
        if guard < global_percent.saturating_add(project_percent) {
            return Err(format!(
                "instruction_budget.static_prefix_guard_percent ({guard}) must be at least the sum of the scope percents ({global_percent} + {project_percent})"
            ));
        }
        if guard - global_percent.saturating_add(project_percent) < MIN_STATIC_RESIDUAL_PERCENT {
            return Err(format!(
                "instruction_budget.static_prefix_guard_percent ({guard}) must leave at least {MIN_STATIC_RESIDUAL_PERCENT} points above the scope percents ({global_percent} + {project_percent}) for non-instruction static content"
            ));
        }
        Ok(())
    }
}
