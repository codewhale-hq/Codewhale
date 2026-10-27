//! `verify` — agent-callable adversarial self-critique (#4196).
//!
//! This tool lets the agent DECIDE to spend extra test-time compute on a
//! self-review of its own recent work before claiming a change done. It runs
//! an INDEPENDENT adversarial critic pass at elevated reasoning (High/Max,
//! regardless of the session tier) whose job is to REFUTE the agent's claim —
//! surfacing correctness gaps, missed requirements, and edge cases as
//! structured findings the agent must then address.
//!
//! # Why elevated reasoning is the mechanism
//!
//! The critic request explicitly sets `reasoning_effort` to a high tier
//! ([`VerifyTool::critic_effort`], default [`ReasoningEffort::Max`]). Elevated
//! reasoning IS the test-time-compute lever, so the critic never inherits a low
//! session tier — [`build_critic_request`] threads the effort onto the outgoing
//! [`MessageRequest`], which the client forwards to the provider.
//!
//! # Bounded / no runaway (hard requirement)
//!
//! A verify call must not be able to trigger another verify. Two independent
//! guards enforce this:
//!
//! 1. **Structural (primary):** the critic is a single model call with
//!    `tools: None` (see [`build_critic_request`]). With no tools of any kind,
//!    the critic literally cannot invoke `verify` — recursion is impossible by
//!    construction, not by a denylist that could be forgotten.
//! 2. **Re-entry guard (defense in depth):** [`VerifyTool::execute`] refuses if
//!    it is entered while a critique is already in progress on the same task
//!    (tracked via the [`static@VERIFY_ACTIVE`] task-local). This protects any
//!    future path that might run the critic inside a tool loop.
//!
//! # Relationship to neighbouring tools
//!
//! - `review` critiques a specific target (file/diff/PR) as a code review.
//! - `run_verifiers` executes external test/build gates (pytest, cargo, …).
//! - `verify` (this tool) is an adversarial reasoning pass over a *claim* and
//!   its supporting evidence — "is what I just did actually correct and
//!   complete?" — not a linter and not a test runner.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::CodewhaleClient;
use crate::llm_client::LlmClient;
use crate::reasoning_preference::ReasoningEffort;
use codewhale_dependencies::ExternalTool;
use codewhale_models::{ContentBlock, Message, MessageRequest, SystemPrompt, Usage};
use codewhale_runtime::features::Feature;
use codewhale_util::utils::truncate_with_ellipsis;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, required_str,
};
use codewhale_models::Role;

/// Total evidence budget handed to the critic. Kept well under a turn so the
/// critic has room to reason. Large diffs/files are truncated with a marker.
const DEFAULT_MAX_EVIDENCE_CHARS: usize = 120_000;
/// Per-file evidence cap so a single huge file can't crowd out the rest.
const PER_FILE_MAX_CHARS: usize = 40_000;
/// Cap on the raw-text fallback summary when the critic returns non-JSON.
const FALLBACK_SUMMARY_MAX_CHARS: usize = 4_000;

// Task-local marker set for the duration of a critic pass. Presence means "a
// verify critique is already running on this task", which `VerifyTool::execute`
// treats as illegal re-entry. This is defense-in-depth on top of the structural
// `tools: None` guard in `build_critic_request`.
tokio::task_local! {
    static VERIFY_ACTIVE: ();
}

const CRITIC_SYSTEM_PROMPT: &str = "You are an adversarial critic performing a rigorous \
self-review of a code change on behalf of the engineer who wrote it. Your job is to REFUTE the \
claim, not to praise it. Assume the change is WRONG or INCOMPLETE until the evidence proves \
otherwise.\n\
\n\
Hunt specifically for: correctness bugs; requirements that are only partially met or silently \
dropped; unhandled edge cases (empty / huge / malformed input, concurrency and re-entrancy, \
error and failure paths, off-by-one, integer overflow, null/None); regressions in existing \
behaviour; and tests that pass but assert the wrong thing (green-CI-but-wrong). Prefer a small \
number of concrete, evidence-backed findings over vague concerns. Cite `path:line` from the \
evidence whenever you can. If, after a genuine effort to break it, you cannot refute the claim, \
say so honestly rather than inventing problems.\n\
\n\
Return ONLY valid JSON (no prose, no markdown fences) matching this schema:\n\
{\n\
  \"verdict\": \"refuted\" | \"upheld\" | \"uncertain\",\n\
  \"summary\": \"<= 3 sentence adversarial assessment\",\n\
  \"findings\": [\n\
    {\n\
      \"severity\": \"critical\" | \"high\" | \"medium\" | \"low\",\n\
      \"issue\": \"what is wrong or unproven\",\n\
      \"evidence\": \"where/why, path:line when possible\",\n\
      \"suggested_fix\": \"concrete, actionable fix\"\n\
    }\n\
  ],\n\
  \"unresolved_risk\": true | false\n\
}\n\
Set verdict=refuted if you found at least one critical or high finding; upheld only if you \
genuinely could not refute the claim; uncertain if the evidence was insufficient to decide. Set \
unresolved_risk=true whenever any unaddressed correctness risk remains.";

/// A single adversarial finding the agent should address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CritiqueFinding {
    /// Normalized to one of `critical` / `high` / `medium` / `low`.
    #[serde(default)]
    pub severity: String,
    /// What is wrong or unproven.
    #[serde(default)]
    pub issue: String,
    /// Where/why, ideally `path:line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Concrete, actionable fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

/// Structured result of a verify/critique pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CritiqueReport {
    /// `refuted` (found a real problem), `upheld` (could not refute), or
    /// `uncertain` (insufficient evidence / unstructured critic output).
    #[serde(default)]
    pub verdict: String,
    /// Short adversarial assessment.
    #[serde(default)]
    pub summary: String,
    /// Concrete findings the agent must address before claiming done.
    #[serde(default)]
    pub findings: Vec<CritiqueFinding>,
    /// True whenever an unaddressed correctness risk remains. Forced `true` by
    /// any finding at `medium` severity or above (only `low` nits are exempt),
    /// regardless of what the critic self-reported, so "green-but-wrong"
    /// changes are not waved through.
    #[serde(default)]
    pub unresolved_risk: bool,
}

impl CritiqueReport {
    /// Parse the critic's raw response text into a structured report, tolerating
    /// bare JSON, a fenced ```json block, or free-form prose (fallback).
    #[must_use]
    pub fn from_model_text(raw: &str) -> Self {
        if let Some(parsed) = parse_report_json(raw) {
            return parsed.normalize();
        }
        if let Some(block) = extract_json_block(raw)
            && let Some(parsed) = parse_report_json(block)
        {
            return parsed.normalize();
        }
        Self::fallback(raw).normalize()
    }

    /// The critic returned something we could not parse as JSON. Fail safe:
    /// treat it as unresolved risk rather than a clean bill of health.
    fn fallback(raw: &str) -> Self {
        let trimmed = raw.trim();
        let summary = if trimmed.is_empty() {
            "Critic returned no output; treat the change as unverified.".to_string()
        } else {
            format!(
                "Critic returned unstructured output (treated as unresolved risk):\n{}",
                truncate_with_ellipsis(trimmed, FALLBACK_SUMMARY_MAX_CHARS, "\n...[truncated]\n")
            )
        };
        Self {
            verdict: "uncertain".to_string(),
            summary,
            findings: Vec::new(),
            unresolved_risk: true,
        }
    }

    /// Canonicalize severities/verdict and derive a fail-safe `unresolved_risk`.
    fn normalize(mut self) -> Self {
        self.summary = self.summary.trim().to_string();
        for finding in &mut self.findings {
            finding.severity = normalize_severity(&finding.severity);
            finding.issue = finding.issue.trim().to_string();
            finding.evidence = normalize_optional(finding.evidence.take());
            finding.suggested_fix = normalize_optional(finding.suggested_fix.take());
        }

        // `has_serious` (critical/high) refutes the claim outright.
        let has_serious = self
            .findings
            .iter()
            .any(|f| matches!(f.severity.as_str(), "critical" | "high"));
        // `has_blocking` (medium-or-above, i.e. anything that is not a `low`
        // nit) is a real unresolved correctness concern: it must force
        // `unresolved_risk` and forbid an `upheld` verdict, even if the critic
        // self-reported `unresolved_risk = false`. This closes the green-but-
        // wrong gap where verdict=upheld + a MEDIUM finding + critic-set
        // `unresolved_risk=false` reported "no unresolved risk". `low`-only
        // findings are intentionally exempt so nits don't block a claim.
        let has_blocking = self
            .findings
            .iter()
            .any(|f| matches!(f.severity.as_str(), "critical" | "high" | "medium"));

        // Verdict: honour an explicit, recognized value; otherwise infer.
        // Precedence: any serious finding => refuted; else any blocking
        // (medium) finding => uncertain (never upheld); else honour the critic.
        self.verdict = match self.verdict.trim().to_ascii_lowercase().as_str() {
            "refuted" | "rejected" | "fail" | "failed" => "refuted".to_string(),
            "upheld" | "confirmed" | "pass" | "passed" | "ok" => {
                if has_serious {
                    "refuted".to_string()
                } else if has_blocking {
                    // A medium finding is a genuine open concern — the claim
                    // cannot be "upheld" while it stands.
                    "uncertain".to_string()
                } else {
                    "upheld".to_string()
                }
            }
            "" => {
                if has_serious {
                    "refuted".to_string()
                } else {
                    "uncertain".to_string()
                }
            }
            _ => "uncertain".to_string(),
        };

        // Fail safe: any medium-or-above finding => unresolved risk, regardless
        // of what the model set.
        self.unresolved_risk = self.unresolved_risk || has_blocking;
        self
    }

    /// Highest severity present, or "none".
    #[must_use]
    fn highest_severity(&self) -> &'static str {
        for level in ["critical", "high", "medium", "low"] {
            if self.findings.iter().any(|f| f.severity == level) {
                return level;
            }
        }
        "none"
    }
}

/// Evidence gathered by the tool and handed to the critic.
struct CritiqueInput {
    claim: String,
    requirement: Option<String>,
    focus: Option<String>,
    evidence: Vec<EvidenceBlock>,
    /// True when no diff or file contents could be gathered.
    no_code_evidence: bool,
}

struct EvidenceBlock {
    label: String,
    body: String,
}

/// Outcome of a single critic invocation, plus accounting for metadata.
struct CritiqueRun {
    report: CritiqueReport,
    usage: Usage,
    /// Provider stop reason when the critic response was incomplete; billed
    /// usage above still counts, but the partial report must not.
    incomplete_stop_reason: Option<String>,
}

/// Agent-callable adversarial self-critique tool.
pub struct VerifyTool {
    client: Option<CodewhaleClient>,
    model: String,
    /// Reasoning tier the critic runs at, independent of the session tier.
    critic_effort: ReasoningEffort,
}

impl VerifyTool {
    /// Construct with the default critic effort ([`ReasoningEffort::Max`]).
    #[must_use]
    pub fn new(client: Option<CodewhaleClient>, model: String) -> Self {
        Self {
            client,
            model,
            critic_effort: ReasoningEffort::Max,
        }
    }

    /// Override the critic reasoning tier. Values below `High` are clamped up to
    /// `High` — elevated reasoning is the whole point of this tool. This is the
    /// seam for a future `[verify] critic_effort` config knob; production
    /// registration currently uses the `Max` default from [`Self::new`].
    #[must_use]
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn with_critic_effort(mut self, effort: ReasoningEffort) -> Self {
        self.critic_effort = clamp_to_elevated(effort);
        self
    }
}

#[async_trait]
impl ToolSpec for VerifyTool {
    fn name(&self) -> &'static str {
        "verify"
    }

    fn description(&self) -> &'static str {
        "Run an INDEPENDENT adversarial critic over your own recent work before you claim it is \
done. You state a claim (what you believe your change accomplishes) plus optional scope (the \
recent git diff, specific files, the original requirement); an independent critic runs at \
elevated reasoning and tries to REFUTE it, returning structured findings (issue, severity, \
suggested fix). Call this when it is worth spending extra thinking: before claiming a non-trivial \
change complete, after a risky or subtle edit, or when you are unsure the change fully satisfies \
the requirement and handles edge cases. Skip it for trivial or mechanical changes. This is not a \
test runner (use run_verifiers) or a code review of an arbitrary target (use review) — it is a \
self-check of whether what you just did is actually correct and complete."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "claim": {
                    "type": "string",
                    "description": "What you believe your recent change accomplishes and why it is correct and complete. State it as an assertion the critic will try to REFUTE."
                },
                "requirement": {
                    "type": "string",
                    "description": "Optional: the original requirement / task / acceptance criteria the change must satisfy. The critic checks the change against THIS, not against your restatement of it."
                },
                "scope": {
                    "type": "string",
                    "enum": ["diff", "staged", "none"],
                    "default": "diff",
                    "description": "Code evidence to gather for the critic. 'diff' = uncommitted working-tree changes; 'staged' = git staged changes; 'none' = rely only on `files` and the claim text."
                },
                "base": {
                    "type": "string",
                    "description": "Optional git base ref for the diff (e.g. origin/main). Defaults to the plain working-tree/staged diff."
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional explicit file paths (relative to the workspace) whose current contents to include as evidence."
                },
                "focus": {
                    "type": "string",
                    "description": "Optional: a specific risk to scrutinize (e.g. 'concurrency', 'the empty-input case', 'error handling on network failure')."
                }
            },
            "required": ["claim"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Read-only: it inspects the workspace (git diff, file reads) and calls
        // the model. It never mutates the workspace.
        vec![ToolCapability::ReadOnly, ToolCapability::Network]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        // Opt-out (defense in depth; the primary gate is registration-time in
        // `with_agent_runtime_surface`). Honours `[features] verify_tool = false`
        // and lets saved-transcript replays respect a disabled toggle.
        if !context.features.enabled(Feature::Verify) {
            return Err(ToolError::not_available(
                "verify tool is disabled ([features] verify_tool = false)".to_string(),
            ));
        }

        // Re-entry guard: refuse if a critique is already running on this task.
        // Checked BEFORE anything else so it cannot be bypassed via a missing
        // client or bad input.
        if VERIFY_ACTIVE.try_with(|_| ()).is_ok() {
            return Err(ToolError::not_available(
                "verify cannot run inside its own critic pass (recursion guard)".to_string(),
            ));
        }

        // Validate the request shape before checking client availability, so a
        // malformed call gets a precise input error rather than a generic
        // "no client" one.
        let claim = required_str(&input, "claim")?.trim().to_string();
        if claim.is_empty() {
            return Err(ToolError::invalid_input("claim cannot be empty"));
        }
        let requirement = optional_str(&input, "requirement")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let focus = optional_str(&input, "focus")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let base = optional_str(&input, "base")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let scope = optional_str(&input, "scope")?.unwrap_or("diff").trim();
        let staged = match scope {
            "diff" | "" => false,
            "staged" => true,
            "none" => {
                // handled below by skipping diff gathering
                false
            }
            other => {
                return Err(ToolError::invalid_input(format!(
                    "unknown scope '{other}' (expected diff | staged | none)"
                )));
            }
        };
        let gather_diff_scope = scope != "none";

        let files = extract_string_array(&input, "files");

        let Some(client) = self.client.clone() else {
            return Err(ToolError::not_available(
                "verify tool requires an active model client".to_string(),
            ));
        };

        // --- Deterministic evidence gathering ---
        let mut evidence: Vec<EvidenceBlock> = Vec::new();
        if gather_diff_scope {
            evidence.extend(
                gather_diff_evidence(context.workspace.as_path(), staged, base.as_deref()).await?,
            );
        }
        evidence.extend(gather_files(&files, context));

        let no_code_evidence = evidence.is_empty();

        let critique_input = CritiqueInput {
            claim,
            requirement,
            focus,
            evidence,
            no_code_evidence,
        };

        // Run the critic under the re-entry marker so any (future) nested tool
        // call to `verify` is refused.
        let route = client.effective_route_envelope(&self.model, chrono::Utc::now());
        // The critic runs at elevated reasoning, so its output allowance must
        // be the route's normal one: a small fixed cap is consumed by the
        // reasoning stream on thinking routes and truncates every critique.
        let max_tokens = client.effective_max_output_tokens(&route.model);
        let run = VERIFY_ACTIVE
            .scope(
                (),
                run_critique(
                    &client,
                    &self.model,
                    self.critic_effort,
                    max_tokens,
                    &critique_input,
                ),
            )
            .await?;

        if let Some(reason) = &run.incomplete_stop_reason {
            let mut metadata = json!({ "tool": "verify" });
            crate::cost_status::attach_child_usage_metadata(&mut metadata, &route, &run.usage);
            return Ok(ToolResult::error(format!(
                "Verify critic response incomplete: provider stop reason `{reason}`; the partial critique was not accepted."
            ))
            .with_metadata(metadata));
        }

        let mut metadata = json!({
            "tool": "verify",
            "verdict": run.report.verdict,
            "finding_count": run.report.findings.len(),
            "highest_severity": run.report.highest_severity(),
            "unresolved_risk": run.report.unresolved_risk,
            "critic_effort": self.critic_effort.as_setting(),
        });
        // Previously this reported only input/output, so a verify child's cache
        // reads *and* cache writes were invisible to the cost audit (#4318).
        crate::cost_status::attach_child_usage_metadata(&mut metadata, &route, &run.usage);

        let result = ToolResult::json(&run.report)
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        Ok(result.with_metadata(metadata))
    }
}

/// Run one adversarial critic pass. Generic over [`LlmClient`] so tests can
/// drive it with `MockLlmClient` without a network call.
async fn run_critique<C: LlmClient>(
    client: &C,
    model: &str,
    effort: ReasoningEffort,
    max_tokens: u32,
    input: &CritiqueInput,
) -> Result<CritiqueRun, ToolError> {
    let prompt = build_critic_prompt(input, DEFAULT_MAX_EVIDENCE_CHARS);
    let request = build_critic_request(model, effort, max_tokens, prompt);
    let response = client
        .create_message(request)
        .await
        .map_err(|e| ToolError::execution_failed(format!("verify critic request failed: {e}")))?;
    let incomplete_stop_reason =
        codewhale_models::is_incomplete_stop_reason(response.stop_reason.as_deref()).then(|| {
            codewhale_models::stop_reason_detail(response.stop_reason.as_deref()).to_string()
        });
    let text = extract_text(&response.content);
    Ok(CritiqueRun {
        report: CritiqueReport::from_model_text(&text),
        usage: response.usage,
        incomplete_stop_reason,
    })
}

/// Build the critic's outgoing request. **The recursion guarantee lives here:**
/// `tools` is always `None`, so the critic cannot invoke `verify` (or any other
/// tool). `reasoning_effort` is set explicitly so the critic runs elevated
/// regardless of the session tier.
fn build_critic_request(
    model: &str,
    effort: ReasoningEffort,
    max_tokens: u32,
    prompt: String,
) -> MessageRequest {
    MessageRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt,
                cache_control: None,
            }],
        }],
        // Route-effective allowance: elevated reasoning shares the output
        // budget on thinking routes, so a small fixed cap here truncates the
        // critique before the JSON starts.
        max_tokens,
        system: Some(SystemPrompt::Text(CRITIC_SYSTEM_PROMPT.to_string())),
        // Hard bound: the critic gets NO tools, so it cannot recurse into verify.
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        // Test-time compute: elevated reasoning, independent of session tier.
        reasoning_effort: Some(clamp_to_elevated(effort).as_setting().to_string()),
        stream: Some(false),
        // Route parity with ordinary turns: no sampling params, so every
        // provider's own defaults apply and fixed-sampling routes don't reject.
        temperature: None,
        top_p: None,
    }
}

fn build_critic_prompt(input: &CritiqueInput, max_chars: usize) -> String {
    let mut out = String::new();
    out.push_str("CLAIM (to be refuted):\n");
    out.push_str(&input.claim);
    out.push('\n');

    if let Some(req) = &input.requirement {
        out.push_str("\nORIGINAL REQUIREMENT (verify the change against THIS):\n");
        out.push_str(req);
        out.push('\n');
    }
    if let Some(focus) = &input.focus {
        out.push_str("\nFOCUS (scrutinize this in particular):\n");
        out.push_str(focus);
        out.push('\n');
    }

    // Evidence gets its own budget so the claim/requirement always survive.
    let header_len = out.len();
    let evidence_budget = max_chars.saturating_sub(header_len).max(1_000);

    out.push_str("\n=== EVIDENCE ===\n");
    if input.no_code_evidence {
        out.push_str(
            "No code diff or file contents were available. Critique the claim on its own terms, \
and explicitly note in your summary that you could not inspect the actual change.\n",
        );
    } else {
        let mut evidence_text = String::new();
        for block in &input.evidence {
            evidence_text.push_str("--- ");
            evidence_text.push_str(&block.label);
            evidence_text.push_str(" ---\n");
            evidence_text.push_str(&block.body);
            if !block.body.ends_with('\n') {
                evidence_text.push('\n');
            }
            evidence_text.push('\n');
        }
        out.push_str(&truncate_with_ellipsis(
            &evidence_text,
            evidence_budget,
            "\n...[evidence truncated]...\n",
        ));
    }
    out.push_str("=== END EVIDENCE ===\n\nRefute the claim. Return ONLY the JSON object.");
    out
}

/// Gather git-diff evidence for the requested scope. Returns zero or more
/// labelled blocks; an empty result is not an error (unlike `review`) — a claim
/// can be about reasoning, and `files` may carry the evidence instead.
///
/// The key correctness point (fix for the base-omits-worktree gap): when a
/// `base` ref is supplied for the working-tree scope, we emit BOTH the committed
/// changes since the merge-base (`git diff base...HEAD`) AND the uncommitted
/// working-tree changes (`git diff HEAD`). `git diff base...HEAD` alone captures
/// branch commits but drops the uncommitted edits the agent usually wants to
/// verify before claiming done.
async fn gather_diff_evidence(
    workspace: &Path,
    staged: bool,
    base: Option<&str>,
) -> Result<Vec<EvidenceBlock>, ToolError> {
    let base = match base.filter(|b| !b.trim().is_empty()) {
        Some(base) => Some(super::git::resolve_commit_ref(workspace, base).await?),
        None => None,
    };
    let mut blocks = Vec::new();

    if staged {
        // Staged scope: the index (optionally vs an explicit base).
        let mut args: Vec<String> = vec!["--cached".to_string()];
        if let Some(base) = &base {
            args.push(base.to_string());
        }
        if let Some(diff) = run_git_diff(workspace, &args).await? {
            blocks.push(EvidenceBlock {
                label: "git diff --cached (staged)".to_string(),
                body: diff,
            });
        }
        return Ok(blocks);
    }

    // Working-tree scope.
    if let Some(base) = base {
        // Committed changes on this branch since the merge-base with `base`.
        if let Some(diff) = run_git_diff(workspace, &[format!("{base}...HEAD")]).await? {
            blocks.push(EvidenceBlock {
                label: format!("committed changes since {base} (git diff {base}...HEAD)"),
                body: diff,
            });
        }
    }
    // Uncommitted changes (staged + unstaged) — what "before claiming done"
    // usually means. `git diff HEAD` captures both; fall back to a plain
    // working-tree diff on an unborn HEAD (empty repo / no commits yet).
    let worktree = match run_git_diff(workspace, &["HEAD".to_string()]).await {
        Ok(diff) => diff,
        Err(_) => run_git_diff(workspace, &[]).await?,
    };
    if let Some(diff) = worktree {
        blocks.push(EvidenceBlock {
            label: "uncommitted changes (git diff HEAD, working tree)".to_string(),
            body: diff,
        });
    }
    Ok(blocks)
}

/// Run `git diff <args>` in `workspace`. Returns `Ok(None)` for an empty diff or
/// when git is unavailable, and `Err` when git runs but reports failure.
async fn run_git_diff(workspace: &Path, args: &[String]) -> Result<Option<String>, ToolError> {
    if !codewhale_dependencies::Git::available() {
        // git not installed: degrade gracefully rather than failing the tool.
        return Ok(None);
    }
    let workspace = workspace.to_path_buf();
    let args = args.to_vec();
    let output = tokio::task::spawn_blocking(move || {
        // The review command keeps the workspace's fsmonitor, hooks and
        // filters from running during the diff.
        let mut cmd = super::git::read_only_git_command(&workspace)?;
        cmd.arg("diff")
            .args(codewhale_dependencies::Git::REVIEW_DIFF_ARGS)
            .args(&args)
            .arg("--");
        cmd.output()
            .map_err(|e| ToolError::execution_failed(format!("failed to run git diff: {e}")))
    })
    .await
    .map_err(|e| ToolError::execution_failed(format!("git diff task panicked: {e}")))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::execution_failed(format!(
            "git diff failed: {}",
            stderr.trim()
        )));
    }
    let diff = String::from_utf8_lossy(&output.stdout).to_string();
    if diff.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(diff))
    }
}

/// Read the requested files as evidence, recording read/path failures as inline
/// notes so the critic knows evidence was requested but unavailable.
fn gather_files(files: &[String], context: &ToolContext) -> Vec<EvidenceBlock> {
    let mut blocks = Vec::new();
    for raw in files {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match crate::tools::file::resolve_guarded_read_path(context, raw, "verify") {
            Ok(path) => match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let display = path
                        .strip_prefix(&context.workspace)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    let numbered = number_lines(&content);
                    blocks.push(EvidenceBlock {
                        label: format!("file: {display}"),
                        body: truncate_with_ellipsis(
                            &numbered,
                            PER_FILE_MAX_CHARS,
                            "\n...[file truncated]...\n",
                        ),
                    });
                }
                Err(e) => blocks.push(EvidenceBlock {
                    label: format!("file: {raw} (unreadable)"),
                    body: format!("<could not read file: {e}>"),
                }),
            },
            Err(e) => blocks.push(EvidenceBlock {
                label: format!("file: {raw} (rejected)"),
                body: format!("<path rejected: {e}>"),
            }),
        }
    }
    blocks
}

fn number_lines(content: &str) -> String {
    content
        .lines()
        .enumerate()
        .map(|(idx, line)| format!("{:>4} | {line}", idx + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reasoning tiers below `High` defeat the purpose; clamp them up.
fn clamp_to_elevated(effort: ReasoningEffort) -> ReasoningEffort {
    match effort {
        ReasoningEffort::High | ReasoningEffort::Max => effort,
        // Off / Low / Medium / Auto → High (still elevated, provider-normalized
        // at the client boundary).
        _ => ReasoningEffort::High,
    }
}

fn extract_string_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn extract_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        if let ContentBlock::Text { text, .. } = block {
            out.push_str(text);
        }
    }
    out
}

fn parse_report_json(raw: &str) -> Option<CritiqueReport> {
    serde_json::from_str::<CritiqueReport>(raw.trim()).ok()
}

/// Extract a JSON object from prose: prefer a fenced ```json block, else the
/// span from the first `{` to the last `}`.
fn extract_json_block(raw: &str) -> Option<&str> {
    if let Some(start) = raw.find("```json") {
        let after = &raw[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim());
        }
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end > start {
        Some(raw[start..=end].trim())
    } else {
        None
    }
}

fn normalize_severity(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "critical" | "crit" | "blocker" | "severe" => "critical",
        "high" | "major" | "important" => "high",
        "low" | "minor" | "nit" | "trivial" => "low",
        // Default unknown/empty to medium so a finding is never dropped, but is
        // also not over-escalated to serious.
        _ => "medium",
    }
    .to_string()
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::mock::MockLlmClient;
    use serde_json::json;
    use std::path::Path;

    fn ctx() -> ToolContext {
        ToolContext::new(Path::new("."))
    }

    fn text_response(model: &str, body: &str) -> codewhale_models::MessageResponse {
        codewhale_models::MessageResponse {
            id: "msg_test".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: body.to_string(),
                cache_control: None,
            }],
            model: model.to_string(),
            stop_reason: Some("stop".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        }
    }

    fn planted_bug_input() -> CritiqueInput {
        // A change that claims to handle all inputs but has an obvious
        // divide-by-zero / empty-slice defect in the diff.
        CritiqueInput {
            claim: "average() now correctly computes the mean for any input slice".to_string(),
            requirement: Some("Must not panic on empty input.".to_string()),
            focus: None,
            evidence: vec![EvidenceBlock {
                label: "git diff (working tree)".to_string(),
                body: "+fn average(xs: &[f64]) -> f64 {\n+    xs.iter().sum::<f64>() / xs.len() as f64\n+}\n"
                    .to_string(),
            }],
            no_code_evidence: false,
        }
    }

    // === Contract tests ===

    #[test]
    fn tool_contract_name_and_schema() {
        let tool = VerifyTool::new(None, "test-model".to_string());
        assert_eq!(tool.name(), "verify");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["claim"].is_object());
        assert!(schema["properties"]["scope"]["enum"].is_array());
        let required = schema["required"].as_array().expect("required array");
        assert!(required.iter().any(|v| v == "claim"));
        // Read-only + network; approval auto.
        assert!(tool.capabilities().contains(&ToolCapability::ReadOnly));
        assert!(tool.is_read_only());
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Auto);
        assert!(tool.model_visible());
    }

    // === Elevated-reasoning + no-recursion structural guard ===

    #[test]
    fn critic_request_is_elevated_and_toolless() {
        // Even if someone constructs the tool at Low, the critic must run
        // elevated and carry NO tools (so it cannot recurse into verify).
        let req = build_critic_request("m", ReasoningEffort::Low, 65_536, "prompt".to_string());
        assert_eq!(
            req.reasoning_effort.as_deref(),
            Some("high"),
            "Low must clamp up to elevated reasoning"
        );
        assert!(
            req.tools.is_none(),
            "critic must be given NO tools — this is the structural recursion guard"
        );
        assert_eq!(
            req.max_tokens, 65_536,
            "critic must use the route-effective allowance it was handed"
        );
        assert_eq!(req.temperature, None, "no sampling params on the wire");
        assert_eq!(req.top_p, None, "no sampling params on the wire");

        let req_max = build_critic_request("m", ReasoningEffort::Max, 65_536, "prompt".to_string());
        assert_eq!(req_max.reasoning_effort.as_deref(), Some("max"));
        assert!(req_max.tools.is_none());
    }

    #[test]
    fn with_critic_effort_clamps_below_high() {
        let tool = VerifyTool::new(None, "m".to_string()).with_critic_effort(ReasoningEffort::Low);
        assert_eq!(tool.critic_effort, ReasoningEffort::High);
        let tool = VerifyTool::new(None, "m".to_string()).with_critic_effort(ReasoningEffort::Max);
        assert_eq!(tool.critic_effort, ReasoningEffort::Max);
    }

    // === Critic-finds-a-planted-bug (mocked model) ===

    #[tokio::test]
    async fn critic_surfaces_planted_bug() {
        let mock = MockLlmClient::new(vec![]);
        // Canonical adversarial JSON the critic would return for the defect.
        mock.push_message_response(text_response(
            "mock-critic",
            r#"{
              "verdict": "refuted",
              "summary": "average() divides by len() with no empty-slice guard.",
              "findings": [
                {
                  "severity": "critical",
                  "issue": "Divide-by-zero / NaN when xs is empty; violates the no-panic requirement.",
                  "evidence": "average(): xs.len() as f64 is 0 for empty input",
                  "suggested_fix": "Return 0.0 or Option::None when xs.is_empty()."
                }
              ],
              "unresolved_risk": true
            }"#,
        ));

        let run = run_critique(
            &mock,
            "mock-critic",
            ReasoningEffort::Max,
            65_536,
            &planted_bug_input(),
        )
        .await
        .expect("critique runs");

        assert_eq!(run.report.verdict, "refuted");
        assert!(run.report.unresolved_risk);
        assert_eq!(run.report.findings.len(), 1);
        assert_eq!(run.report.findings[0].severity, "critical");
        assert!(
            run.report.findings[0]
                .issue
                .to_lowercase()
                .contains("empty"),
            "finding should name the empty-input defect"
        );
        assert_eq!(run.report.highest_severity(), "critical");

        // The outgoing critic request carried elevated reasoning and NO tools.
        let sent = mock.last_request().expect("request captured");
        assert_eq!(sent.reasoning_effort.as_deref(), Some("max"));
        assert!(sent.tools.is_none());
        // Evidence and requirement were threaded into the prompt.
        let prompt = match &sent.messages[0].content[0] {
            ContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(prompt.contains("CLAIM"));
        assert!(prompt.contains("Must not panic on empty input"));
        assert!(prompt.contains("average("));
    }

    #[tokio::test]
    async fn unstructured_critic_output_is_unresolved_risk() {
        let mock = MockLlmClient::new(vec![]);
        mock.push_message_response(text_response("m", "I think it looks fine, ship it."));
        let run = run_critique(
            &mock,
            "m",
            ReasoningEffort::High,
            65_536,
            &planted_bug_input(),
        )
        .await
        .expect("runs");
        // Fail safe: non-JSON critic output must not read as a clean pass.
        assert_eq!(run.report.verdict, "uncertain");
        assert!(run.report.unresolved_risk);
    }

    #[test]
    fn upheld_with_serious_finding_is_downgraded_to_refuted() {
        // Guards the green-but-wrong trap: a critic can't declare "upheld" while
        // simultaneously reporting a high-severity finding.
        let report = CritiqueReport::from_model_text(
            r#"{"verdict":"upheld","summary":"looks ok","findings":[{"severity":"high","issue":"missing null check"}],"unresolved_risk":false}"#,
        );
        assert_eq!(report.verdict, "refuted");
        assert!(report.unresolved_risk);
    }

    #[test]
    fn upheld_with_medium_finding_flags_unresolved_risk() {
        // The exact green-but-wrong gap: verdict=upheld + a MEDIUM finding +
        // critic-set unresolved_risk=false must still surface as unresolved
        // risk, and the verdict must not stay "upheld".
        let report = CritiqueReport::from_model_text(
            r#"{"verdict":"upheld","summary":"seems fine","findings":[{"severity":"medium","issue":"unhandled empty-input case"}],"unresolved_risk":false}"#,
        );
        assert!(
            report.unresolved_risk,
            "a medium finding must force unresolved_risk=true"
        );
        assert_ne!(
            report.verdict, "upheld",
            "cannot remain 'upheld' with an open medium finding"
        );
        assert_eq!(
            report.verdict, "uncertain",
            "medium (not serious) downgrades upheld to uncertain, not refuted"
        );
    }

    #[test]
    fn upheld_with_only_low_finding_stays_upheld() {
        // Intentional carve-out: low-severity nits do not block a claim, so an
        // otherwise-clean "upheld" with only a low finding stays upheld and
        // does not raise unresolved_risk.
        let report = CritiqueReport::from_model_text(
            r#"{"verdict":"upheld","summary":"clean","findings":[{"severity":"low","issue":"nit: rename variable"}],"unresolved_risk":false}"#,
        );
        assert_eq!(report.verdict, "upheld");
        assert!(!report.unresolved_risk);
    }

    // === Recursion / re-entry guard ===

    #[tokio::test]
    async fn execute_refuses_reentry() {
        // Simulate being inside a critic pass; execute must refuse before it
        // even looks at the (absent) client or input.
        let tool = VerifyTool::new(None, "m".to_string());
        let err = VERIFY_ACTIVE
            .scope((), async {
                tool.execute(json!({ "claim": "x" }), &ctx()).await
            })
            .await
            .expect_err("re-entry must be refused");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("recursion") || msg.contains("inside its own"),
            "expected a recursion-guard error, got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_without_client_is_not_available() {
        let tool = VerifyTool::new(None, "m".to_string());
        let err = tool
            .execute(json!({ "claim": "did the thing" }), &ctx())
            .await
            .expect_err("no client");
        assert!(err.to_string().to_lowercase().contains("client"));
    }

    #[tokio::test]
    async fn execute_rejects_empty_and_unknown_scope() {
        let tool = VerifyTool::new(None, "m".to_string());
        // Empty claim is rejected before the (absent) client is consulted.
        let err = tool
            .execute(json!({ "claim": "   " }), &ctx())
            .await
            .expect_err("empty claim");
        assert!(err.to_string().to_lowercase().contains("claim"), "{err}");

        // Unknown scope is a precise input error, not a generic client error.
        let err = tool
            .execute(json!({ "claim": "ok", "scope": "everything" }), &ctx())
            .await
            .expect_err("unknown scope");
        assert!(err.to_string().to_lowercase().contains("scope"), "{err}");
    }

    #[test]
    fn parses_fenced_json_block() {
        let raw = "Here is my critique:\n```json\n{\"verdict\":\"refuted\",\"summary\":\"s\",\"findings\":[],\"unresolved_risk\":true}\n```\nDone.";
        let report = CritiqueReport::from_model_text(raw);
        assert_eq!(report.verdict, "refuted");
        assert!(report.unresolved_risk);
    }

    #[test]
    fn severity_normalization() {
        assert_eq!(normalize_severity("BLOCKER"), "critical");
        assert_eq!(normalize_severity("Major"), "high");
        assert_eq!(normalize_severity("nit"), "low");
        assert_eq!(normalize_severity("weird"), "medium");
        assert_eq!(normalize_severity(""), "medium");
    }

    // === git diff evidence gathering ===

    fn run_git(dir: &Path, args: &[&str]) {
        let mut cmd = codewhale_dependencies::Git::command().expect("git available");
        cmd.args(args).current_dir(dir);
        let out = cmd.output().expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[tokio::test]
    async fn diff_base_cannot_inject_options_or_run_diff_helpers() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let marker = outside.path().join("sentinel");
        std::fs::write(&marker, "untouched").unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]);
        run_git(repo, &["config", "user.email", "t@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        run_git(repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("f.txt"), "original\n").unwrap();
        std::fs::write(repo.join(".gitattributes"), "*.txt diff=hostile\n").unwrap();
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-q", "-m", "base"]);
        run_git(
            repo,
            &[
                "config",
                "diff.hostile.textconv",
                "definitely-missing-helper",
            ],
        );
        run_git(
            repo,
            &["config", "diff.external", "definitely-missing-helper"],
        );
        std::fs::write(repo.join("f.txt"), "changed-marker\n").unwrap();
        run_git(repo, &["add", "f.txt"]);
        for staged in [false, true] {
            for base in [
                format!("--output={}", marker.display()),
                format!("--out={}", marker.display()),
                "--ext-diff".into(),
                "HEAD --output=bad".into(),
                "HEAD\n--output=bad".into(),
            ] {
                assert!(
                    gather_diff_evidence(repo, staged, Some(&base))
                        .await
                        .is_err(),
                    "{staged}: {base}"
                );
                assert_eq!(std::fs::read_to_string(&marker).unwrap(), "untouched");
            }
            let blocks = gather_diff_evidence(repo, staged, Some("HEAD"))
                .await
                .unwrap();
            assert!(
                blocks
                    .iter()
                    .any(|block| block.body.contains("changed-marker"))
            );
        }
    }

    /// The working-tree evidence read must not run the repository's clean
    /// filter, which `--no-ext-diff`/`--no-textconv` do not cover.
    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_diff_evidence_runs_no_clean_filter() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let marker = outside.path().join("marker");
        let clean = outside.path().join("clean.sh");
        std::fs::write(
            &clean,
            format!("#!/bin/sh\necho clean >> '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&clean, std::fs::Permissions::from_mode(0o755)).unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]);
        run_git(repo, &["config", "user.email", "t@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        run_git(repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("f.txt"), "original\n").unwrap();
        std::fs::write(repo.join(".gitattributes"), "f.txt filter=x\n").unwrap();
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-q", "-m", "base"]);
        run_git(
            repo,
            &["config", "filter.x.clean", &clean.display().to_string()],
        );
        std::fs::write(repo.join("f.txt"), "changed-marker\n").unwrap();

        let blocks = gather_diff_evidence(repo, false, None).await.unwrap();
        assert!(
            !marker.exists(),
            "the evidence read ran the clean filter: {}",
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
        assert!(
            blocks
                .iter()
                .any(|block| block.body.contains("+changed-marker")),
            "{:?}",
            blocks.iter().map(|block| &block.body).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn diff_without_base_remains_usable_on_unborn_head() {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        assert!(
            gather_diff_evidence(tmp.path(), false, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            gather_diff_evidence(tmp.path(), true, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn diff_scope_with_base_includes_uncommitted_worktree_changes() {
        // Regression for the base-omits-worktree gap: `git diff base...HEAD`
        // captures branch commits but drops the uncommitted edits the agent
        // usually wants to verify before claiming done. With a base set, the
        // gatherer must include BOTH the committed-since-base changes AND the
        // uncommitted working-tree changes.
        if codewhale_dependencies::Git::command().is_none() {
            return; // no git in this environment — nothing to exercise.
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]);
        run_git(repo, &["config", "user.email", "t@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        run_git(repo, &["config", "commit.gpgsign", "false"]);

        // Base commit.
        std::fs::write(repo.join("f.txt"), "line1\n").expect("write");
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-q", "-m", "base"]);

        // A committed change on top of the base.
        std::fs::write(repo.join("f.txt"), "line1\nCOMMITTED_MARKER\n").expect("write");
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-q", "-m", "second"]);

        // An UNCOMMITTED working-tree change.
        std::fs::write(
            repo.join("f.txt"),
            "line1\nCOMMITTED_MARKER\nUNCOMMITTED_MARKER\n",
        )
        .expect("write");

        let blocks = gather_diff_evidence(repo, false, Some("HEAD~1"))
            .await
            .expect("gather diff");
        let joined = blocks
            .iter()
            .map(|b| format!("[{}]\n{}", b.label, b.body))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            joined.contains("UNCOMMITTED_MARKER"),
            "uncommitted working-tree change must appear in evidence with a base set:\n{joined}"
        );
        assert!(
            joined.contains("COMMITTED_MARKER"),
            "committed-since-base change must also appear:\n{joined}"
        );
        // Two distinct labelled blocks: committed-since-base and uncommitted.
        assert!(
            blocks
                .iter()
                .any(|b| b.label.contains("committed changes since")),
            "expected a committed-since-base block: {:?}",
            blocks.iter().map(|b| &b.label).collect::<Vec<_>>()
        );
        assert!(
            blocks
                .iter()
                .any(|b| b.label.contains("uncommitted changes")),
            "expected an uncommitted working-tree block: {:?}",
            blocks.iter().map(|b| &b.label).collect::<Vec<_>>()
        );
    }
}
