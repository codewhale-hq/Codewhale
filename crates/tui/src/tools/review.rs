//! Tool for structured code reviews of files, diffs, or pull requests.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::CodewhaleClient;
#[cfg(test)]
use codewhale_dependencies::ExternalTool;
use codewhale_models::Usage;
use codewhale_util::utils::truncate_with_ellipsis;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str, optional_u64, required_str,
};

const DEFAULT_MAX_CHARS: usize = 200_000;
const MAX_MAX_CHARS: usize = 1_000_000;
pub(crate) const MAX_REVIEW_PASSES: usize = 64;
const FALLBACK_MAX_CHARS: usize = 4000;
const REVIEW_RECEIPT_SCHEMA_VERSION: u32 = 1;
const PR_COVERAGE_RECEIPT_SCHEMA_VERSION: u32 = 2;

/// Rank used to bound reasoning level without depending on `Ord`.
fn reasoning_effort_rank(effort: crate::reasoning_preference::ReasoningEffort) -> u8 {
    use crate::reasoning_preference::ReasoningEffort;
    match effort {
        ReasoningEffort::Off => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
        ReasoningEffort::Ultra => 6,
        ReasoningEffort::Max => 7,
        // `Auto` is resolved from the prompt before this bound is applied; if
        // it somehow arrives unresolved, treat it as the medium default rather
        // than silently unbounded.
        ReasoningEffort::Auto => 3,
    }
}

fn reasoning_effort_from_rank(rank: u8) -> crate::reasoning_preference::ReasoningEffort {
    use crate::reasoning_preference::ReasoningEffort;
    match rank {
        0 => ReasoningEffort::Off,
        1 => ReasoningEffort::Minimal,
        2 => ReasoningEffort::Low,
        3 => ReasoningEffort::Medium,
        4 => ReasoningEffort::High,
        5 => ReasoningEffort::XHigh,
        6 => ReasoningEffort::Ultra,
        _ => ReasoningEffort::Max,
    }
}

/// Highest reasoning level a review pass may request, given the visible-text
/// reserve this exact model needs (`route_budget::review_visible_text_reserve_percent`).
///
/// A review pass only has to rank findings, so unbounded reasoning buys little
/// while a shared `max_tokens` allowance lets it consume everything: #6285 saw
/// `reasoning_tokens == output_tokens == 65536`, stop reason `length`, zero
/// visible text, and a PR blocked with no findings shown. The cap scales with
/// the reserve the model actually needs and never raises the caller's request.
/// An allowance of 8K or less on a reasoning model caps at Low, because a
/// medium-or-higher pass can spend that whole allowance reasoning (#7067).
///
/// What this does not do: it cannot separate reasoning from text on a route
/// that exposes no effort knob, and it does not re-request a pass that already
/// exhausted its allowance — that stays a reported budget outcome.
#[must_use]
pub(crate) fn bounded_review_reasoning_effort(
    requested: crate::reasoning_preference::ReasoningEffort,
    reserve_percent: u32,
    allowance: u32,
) -> crate::reasoning_preference::ReasoningEffort {
    let mut ceiling = match reserve_percent {
        // Nothing reserved: the model does not reason, so nothing to bound.
        0 => u8::MAX,
        // A quarter of the allowance must survive as text.
        1..=25 => 3,
        // Half the allowance must survive as text.
        _ => 2,
    };
    if reserve_percent > 0 && allowance <= super::review_triage::REASONING_ROUTE_MIN_OUTPUT_TOKENS {
        ceiling = ceiling.min(2);
    }
    reasoning_effort_from_rank(reasoning_effort_rank(requested).min(ceiling))
}

/// Budget for how many lines a committable suggestion may replace. A
/// mechanical fix is small; anything larger is judgement wearing a
/// suggestion fence, so it must degrade to prose.
pub const MAX_COMMITTABLE_SUGGESTION_LINES: u32 = 25;
const REVIEW_CLIENT_UNAVAILABLE: &str = "Review tool requires an active Codewhale model client";

/// What kind of claim this is. `defect` is a wrong behaviour in the changed
/// code; `intent` is a mismatch between what the change says it does and what
/// it does; `contract` is a dependent outside the diff that the change broke.
/// The distinction exists because the three need different evidence and a
/// reader triages them differently.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewIssue {
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    /// `defect`, `intent` or `contract`. Absent on older receipts and model
    /// output written before this field existed, and read as `defect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// How the reviewer established the claim. `reproduced` requires a
    /// command; `traced` requires `path:line`; `suspected` is never posted
    /// inline and is moved to the summary's "worth a human look" list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ReviewEvidence>,
    /// For a `contract` claim: what the change made newly true or false, and
    /// which dependents were checked against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<ReviewContract>,
    /// Whether this change introduced the defect or merely exposed it.
    ///
    /// `introduced` or `preexisting`. Set **mechanically** from the diff by the
    /// review core, never taken from the model: a reviewer that can label its
    /// own accusation as someone else's problem is not a reviewer worth
    /// trusting. A `preexisting` claim is a real bug on code this change did
    /// not touch, reported without attributing it to the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship: Option<String>,
    /// False when the claim cannot land on a line of this diff. Such a claim is
    /// still reported — in the review body rather than inline — because losing
    /// a proven finding to a line-number disagreement is worse than reporting
    /// it in the wrong place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_anchorable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewEvidence {
    /// `reproduced`, `traced` or `suspected`.
    #[serde(default)]
    pub tier: String,
    /// The command and its output for `reproduced`, or the input and the
    /// `path:line` chain for `traced`. Free text, rendered verbatim.
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewContract {
    /// What became newly true or newly false at the changed line.
    #[serde(default)]
    pub became: String,
    /// Dependents of that contract the reviewer checked, as `path:line` or a
    /// path with a symbol. An empty list is honest: it means none were found.
    #[serde(default)]
    pub dependents_checked: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewSuggestion {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    /// First line of the replaced span (inclusive). `None` means the
    /// suggestion covers a single line, `line`.
    #[serde(default)]
    pub start_line: Option<u32>,
    /// Last line of the replaced span (inclusive). Defaults to `line`.
    #[serde(default)]
    pub end_line: Option<u32>,
    /// Prose: why the change is wanted.
    #[serde(default)]
    pub suggestion: String,
    /// Literal replacement source for `start_line..=end_line`, indentation
    /// included. `Some` only for mechanical, high-confidence fixes; when it
    /// is `None` the reviewer posts prose instead of a committable
    /// GitHub suggestion block.
    #[serde(default)]
    pub replacement: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewOutput {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
    #[serde(default)]
    pub suggestions: Vec<ReviewSuggestion>,
    #[serde(default)]
    pub overall_assessment: String,
    /// `low`, `medium` or `high`, with the reason, so the person merging gets
    /// the answer they actually want before reading a single finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_risk: Option<ReviewMergeRisk>,
    /// What the reviewer established rather than suspected: dependents read,
    /// commands run and their results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified: Vec<String>,
    /// What was not reviewed, and why. An honest gap, not a blank cheque.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_reviewed: Vec<String>,
    /// What the reviewer examined and found sound, with the reason. A negative
    /// result, not a claim: it never counts toward merge risk and never becomes
    /// an inline comment. This is what lets a reader stop checking something.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ruled_out: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewMergeRisk {
    /// `low`, `medium` or `high`.
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub reason: String,
}

impl ReviewOutput {
    pub(crate) fn note_binary_coverage(&mut self, diff: &str) {
        if diff.contains("\nGIT binary patch\n") || diff.contains("\nBinary files ") {
            self.summary.push_str("\nCoverage limitation: binary changes were represented by metadata; their contents were not semantically inspected.");
        }
    }

    #[must_use]
    pub fn from_str(raw: &str) -> Self {
        if let Some(parsed) = parse_review_output_json(raw) {
            return parsed.normalize();
        }
        if let Some(json_block) = extract_json_block(raw)
            && let Some(parsed) = parse_review_output_json(json_block)
        {
            return parsed.normalize();
        }
        ReviewOutput::fallback(raw)
    }

    /// Parse `raw` only when it is the structured review contract (all four
    /// top-level fields present), unlike [`Self::from_str`], which falls
    /// back to wrapping prose.
    pub(crate) fn from_structured_str(raw: &str) -> Option<Self> {
        let candidate = serde_json::from_str::<Value>(raw)
            .ok()
            .or_else(|| extract_json_block(raw).and_then(|json| serde_json::from_str(json).ok()))?;
        let object = candidate.as_object()?;
        (object.get("summary")?.is_string()
            && object.get("issues")?.is_array()
            && object.get("suggestions")?.is_array()
            && object.get("overall_assessment")?.is_string())
        .then(|| serde_json::from_value::<ReviewOutput>(candidate).ok())
        .flatten()
        .map(Self::normalize)
    }

    fn fallback(raw: &str) -> Self {
        let trimmed = raw.trim();
        let summary = if trimmed.is_empty() {
            "Review completed but no structured output was returned.".to_string()
        } else {
            truncate_with_ellipsis(trimmed, FALLBACK_MAX_CHARS, "\n...[truncated]\n")
        };
        Self {
            summary,
            ..Self::default()
        }
    }

    fn normalize(mut self) -> Self {
        self.summary = self.summary.trim().to_string();
        self.overall_assessment = self.overall_assessment.trim().to_string();
        for issue in &mut self.issues {
            issue.severity = normalize_severity(&issue.severity);
            issue.title = issue.title.trim().to_string();
            issue.description = issue.description.trim().to_string();
            issue.path = normalize_optional(issue.path.take());
            issue.kind = issue
                .kind
                .take()
                .map(|kind| normalize_claim_kind(&kind))
                .filter(|kind| !kind.is_empty());
            if let Some(evidence) = issue.evidence.as_mut() {
                evidence.tier = normalize_evidence_tier(&evidence.tier);
                evidence.detail = evidence.detail.trim().to_string();
            }
            issue.authorship = issue.authorship.take().map(|value| {
                match value.trim().to_ascii_lowercase().as_str() {
                    "preexisting" | "pre-existing" | "pre_existing" | "existing" => "preexisting",
                    _ => "introduced",
                }
                .to_string()
            });
            if let Some(contract) = issue.contract.as_mut() {
                contract.became = contract.became.trim().to_string();
                contract.dependents_checked = contract
                    .dependents_checked
                    .iter()
                    .map(|entry| entry.trim().to_string())
                    .filter(|entry| !entry.is_empty())
                    .collect();
            }
        }
        if let Some(risk) = self.merge_risk.as_mut() {
            risk.level = match risk.level.trim().to_ascii_lowercase().as_str() {
                "high" | "blocking" | "blocker" => "high",
                "medium" | "moderate" | "concern" => "medium",
                _ => "low",
            }
            .to_string();
            risk.reason = risk.reason.trim().to_string();
        }
        self.verified = self
            .verified
            .iter()
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
            .collect();
        self.not_reviewed = self
            .not_reviewed
            .iter()
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
            .collect();
        self.ruled_out = self
            .ruled_out
            .iter()
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
            .collect();
        for suggestion in &mut self.suggestions {
            suggestion.suggestion = suggestion.suggestion.trim().to_string();
            suggestion.path = normalize_optional(suggestion.path.take());
            // Leading whitespace in `replacement` is load-bearing indentation,
            // so only trailing newlines and all-whitespace payloads are
            // normalized away.
            suggestion.replacement = suggestion
                .replacement
                .take()
                .map(|replacement| replacement.trim_end_matches(['\n', '\r']).to_string())
                .filter(|replacement| !replacement.trim().is_empty());
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrReviewPassManifest {
    pub number: usize,
    pub diff_fingerprint: String,
    pub diff_chars: usize,
    /// Number of entries in `files`: whole file patches plus, for an
    /// oversized text file, its `(part k/n)` parts. Parts of one file never
    /// share a pass — their combined size exceeds the whole file, which
    /// already exceeded the per-pass limit — so this is also the distinct
    /// file count of the pass and parts cannot inflate coverage.
    pub file_count: usize,
    pub files: Vec<String>,
}

/// One file patch the plan never scheduled (#6285 AC3/AC4). `file` is the
/// patch label exactly as it would have appeared in a pass manifest
/// (`a/old b/new`, or `… (part k/n)` for a pass-budget cut); `chars` is the
/// budgeted `model_diff` size.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrReviewSkippedFile {
    pub file: String,
    pub reason: String,
    pub chars: usize,
}

/// Skip reasons are stable sentence fragments rendered into review
/// summaries, receipts, and failure notes; keep them greppable.
const SKIP_REASON_HUNK_EXCEEDS_PASS: &str = "a single hunk exceeds the per-pass limit";
const SKIP_REASON_NO_HUNK_BOUNDARIES: &str =
    "exceeds the per-pass limit with no hunk boundaries to split at";
const SKIP_REASON_BEYOND_MAX_PASSES: &str = "beyond the max_passes budget";

/// Render a skip list the way every consumer shows it: the entries are
/// self-describing, so no caller needs its own format.
pub(crate) fn format_skipped_files(skipped: &[PrReviewSkippedFile]) -> String {
    skipped
        .iter()
        .map(|skip| format!("{} ({} chars; {})", skip.file, skip.chars, skip.reason))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrReviewManifest {
    pub base_sha: String,
    pub head_sha: String,
    pub diff_fingerprint: String,
    pub diff_chars: usize,
    pub file_count: usize,
    pub binary_file_patches: usize,
    pub binary_contents_semantically_inspected: bool,
    pub max_chars_per_pass: usize,
    pub passes: Vec<PrReviewPassManifest>,
    /// Files the plan never scheduled, in diff order. Empty means complete
    /// coverage; every entry names a file the gate did not read and why.
    /// `#[serde(default)]` keeps pre-skip-list receipts readable — those
    /// plans were complete by construction.
    #[serde(default)]
    pub skipped_files: Vec<PrReviewSkippedFile>,
}

#[derive(Debug, Clone)]
pub struct PrReviewPass {
    pub manifest: PrReviewPassManifest,
    pub diff: String,
}

#[derive(Debug, Clone)]
pub struct PrReviewPlan {
    pub manifest: PrReviewManifest,
    pub passes: Vec<PrReviewPass>,
}

fn patch_label(patch: &str) -> String {
    patch
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("diff --git "))
        .unwrap_or("(unknown file)")
        .to_string()
}

fn pr_file_patches(diff: &str) -> Vec<&str> {
    let mut starts = diff
        .match_indices("diff --git ")
        .filter_map(|(offset, _)| {
            (offset == 0 || diff.as_bytes().get(offset.wrapping_sub(1)) == Some(&b'\n'))
                .then_some(offset)
        })
        .collect::<Vec<_>>();
    starts.push(diff.len());
    starts
        .windows(2)
        .map(|window| &diff[window[0]..window[1]])
        .collect()
}

/// Split one file patch into its full header (`diff --git` through the `+++`
/// line) and its complete unified-diff hunks, every slice byte-exact. A patch
/// without hunks (binary or metadata-only) is all header and cannot be split.
fn pr_file_hunks(patch: &str) -> (&str, Vec<&str>) {
    let mut starts = patch
        .match_indices("@@ ")
        .filter_map(|(offset, _)| {
            (offset == 0 || patch.as_bytes().get(offset.wrapping_sub(1)) == Some(&b'\n'))
                .then_some(offset)
        })
        .collect::<Vec<_>>();
    let header = starts.first().map_or(patch, |end| &patch[..*end]);
    starts.push(patch.len());
    let hunks = starts
        .windows(2)
        .map(|window| &patch[window[0]..window[1]])
        .collect();
    (header, hunks)
}

/// One unit of PR review pass packing: a whole file patch, or one part of an
/// oversized text file split at complete hunk boundaries. `header_bytes` is
/// nonzero only on continuation parts, where the full file header is
/// replayed; it is the exact byte prefix to strip when rebuilding the
/// original diff.
struct PrReviewPiece<'a> {
    diff: Cow<'a, str>,
    label: String,
    header_bytes: usize,
}

/// One diff-ordered unit of a (possibly degraded) plan: a reviewable piece
/// or a skipped original patch. The partition guard rebuilds the diff from
/// both, so every byte is either reviewed or named as skipped.
enum PrReviewAtom<'a> {
    Piece(PrReviewPiece<'a>),
    Skipped {
        patch: &'a str,
        label: String,
        chars: usize,
        reason: &'static str,
    },
}

/// Plan PR review passes over `diff`, degrading instead of failing closed
/// (#6285 AC3): files that fit no pass and passes beyond `max_passes` are
/// skipped in diff order and named in `manifest.skipped_files` (AC4). Only a
/// plan that covers nothing still errors.
///
/// Known limitations, beside the behaviour: skips are whole files — a file
/// with one oversized hunk is skipped entirely, never truncated — and files
/// stay in diff order rather than re-sorted by estimated risk.
pub(crate) fn plan_pr_review(
    diff: &str,
    view: &super::review_pr::GhPullRequest,
    max_chars: usize,
    max_passes: usize,
) -> anyhow::Result<PrReviewPlan> {
    anyhow::ensure!(max_chars > 0, "Review max_chars must be positive");
    anyhow::ensure!(
        (1..=MAX_REVIEW_PASSES).contains(&max_passes),
        "Review max_passes must be from 1 to {MAX_REVIEW_PASSES}"
    );
    let patches = pr_file_patches(diff);
    anyhow::ensure!(
        patches.len() == view.changed_files && !patches.is_empty(),
        "Complete PR review plan found {} file patches; expected {}",
        patches.len(),
        view.changed_files
    );

    // A whole file stays together whenever it fits. An oversized text file
    // splits only at complete hunk boundaries, with the full file header
    // replayed into every part so each part stays a self-describing patch;
    // no line is elided, shortened or reordered. Sizes use the model
    // representation, so a binary payload already omitted there can never
    // drive a split.
    let mut atoms: Vec<PrReviewAtom<'_>> = Vec::new();
    for patch in patches {
        let patch_chars = super::review_pr::model_diff(patch).chars().count();
        if patch_chars <= max_chars {
            atoms.push(PrReviewAtom::Piece(PrReviewPiece {
                diff: Cow::Borrowed(patch),
                label: patch_label(patch),
                header_bytes: 0,
            }));
            continue;
        }
        let (header, hunks) = pr_file_hunks(patch);
        let header_chars = header.chars().count();
        let largest_hunk_chars = hunks.iter().map(|hunk| hunk.chars().count()).max();
        // A file whose largest hunk cannot share a pass with its own header
        // can never be scheduled; it is skipped whole, never truncated, so a
        // finding can never rest on half a change.
        if !largest_hunk_chars.is_some_and(|hunk_chars| header_chars + hunk_chars <= max_chars) {
            atoms.push(PrReviewAtom::Skipped {
                patch,
                label: patch_label(patch),
                chars: patch_chars,
                reason: if hunks.is_empty() {
                    SKIP_REASON_NO_HUNK_BOUNDARIES
                } else {
                    SKIP_REASON_HUNK_EXCEEDS_PASS
                },
            });
            continue;
        }
        let label = patch_label(patch);
        let mut parts: Vec<String> = Vec::new();
        let mut part = String::from(header);
        let mut part_chars = header_chars;
        for hunk in hunks {
            let hunk_chars = hunk.chars().count();
            if part_chars > header_chars && part_chars + hunk_chars > max_chars {
                parts.push(std::mem::replace(&mut part, String::from(header)));
                part_chars = header_chars;
            }
            part.push_str(hunk);
            part_chars += hunk_chars;
        }
        parts.push(part);
        let total = parts.len();
        atoms.extend(parts.into_iter().enumerate().map(|(index, part)| {
            PrReviewAtom::Piece(PrReviewPiece {
                label: format!("{label} (part {}/{total})", index + 1),
                header_bytes: if index == 0 { 0 } else { header.len() },
                diff: Cow::Owned(part),
            })
        }));
    }

    // The partition guard, byte-for-byte: continuation parts replay the file
    // header, so exactly those repeated headers are stripped, skipped
    // originals are replayed whole, and the rebuilt plan must equal the
    // original diff — every byte is either reviewed or named as skipped.
    let mut pieces: Vec<PrReviewPiece<'_>> = Vec::new();
    let mut skipped: Vec<PrReviewSkippedFile> = Vec::new();
    let mut rebuilt = String::with_capacity(diff.len());
    for atom in atoms {
        match atom {
            PrReviewAtom::Piece(piece) => {
                rebuilt.push_str(&piece.diff[piece.header_bytes..]);
                pieces.push(piece);
            }
            PrReviewAtom::Skipped {
                patch,
                label,
                chars,
                reason,
            } => {
                rebuilt.push_str(patch);
                skipped.push(PrReviewSkippedFile {
                    file: label,
                    reason: reason.to_string(),
                    chars,
                });
            }
        }
    }
    anyhow::ensure!(
        rebuilt == diff,
        "PR review plan did not partition the complete diff byte-for-byte"
    );

    let mut grouped: Vec<Vec<PrReviewPiece<'_>>> = Vec::new();
    let mut current: Vec<PrReviewPiece<'_>> = Vec::new();
    let mut current_chars = 0;
    for piece in pieces {
        let piece_chars = super::review_pr::model_diff(&piece.diff).chars().count();
        if !current.is_empty() && current_chars + piece_chars > max_chars {
            grouped.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(piece);
        current_chars += piece_chars;
    }
    if !current.is_empty() {
        grouped.push(current);
    }
    // Passes beyond the budget are skipped in diff order, never fatal. The
    // plan reviews what fits and names the rest.
    for group in grouped.split_off(max_passes.min(grouped.len())) {
        for piece in group {
            let label = piece.label;
            let chars = super::review_pr::model_diff(&piece.diff).chars().count();
            skipped.push(PrReviewSkippedFile {
                file: label,
                reason: SKIP_REASON_BEYOND_MAX_PASSES.to_string(),
                chars,
            });
        }
    }

    // Only a plan that covers nothing still errors — and even then it
    // names every skipped file, so the failure reads as limits, not as a
    // verdict on the code.
    anyhow::ensure!(
        !grouped.is_empty(),
        "PR review plan covers 0 of {} file patches within {max_chars} characters per pass and {max_passes} pass(es); skipped: {}. No review was run or posted.",
        view.changed_files,
        format_skipped_files(&skipped)
    );

    let passes = grouped
        .into_iter()
        .enumerate()
        .map(|(index, pieces)| {
            let diff = pieces
                .iter()
                .map(|piece| -> &str { &piece.diff })
                .collect::<String>();
            let labels = pieces
                .iter()
                .map(|piece| piece.label.clone())
                .collect::<Vec<_>>();
            let manifest = PrReviewPassManifest {
                number: index + 1,
                diff_fingerprint: diff_fingerprint(&diff),
                diff_chars: super::review_pr::model_diff(&diff).chars().count(),
                file_count: pieces.len(),
                files: labels,
            };
            PrReviewPass { manifest, diff }
        })
        .collect::<Vec<_>>();
    let manifest = PrReviewManifest {
        base_sha: view.base_sha.clone(),
        head_sha: view.head_sha.clone(),
        diff_fingerprint: diff_fingerprint(diff),
        diff_chars: super::review_pr::model_diff(diff).chars().count(),
        file_count: view.changed_files,
        binary_file_patches: diff
            .lines()
            .filter(|line| *line == "GIT binary patch" || line.starts_with("Binary files "))
            .count(),
        binary_contents_semantically_inspected: false,
        max_chars_per_pass: max_chars,
        passes: passes.iter().map(|pass| pass.manifest.clone()).collect(),
        skipped_files: skipped,
    };
    Ok(PrReviewPlan { manifest, passes })
}

/// Keep bounded Git reads off the Engine/CLI async runtime. Both frontends
/// prepare the same immutable requests before resolving or billing a model.
pub(crate) async fn build_pr_review_prompts(
    number: u32,
    view: &super::review_pr::GhPullRequest,
    plan: &PrReviewPlan,
    workspace: &Path,
) -> anyhow::Result<Vec<String>> {
    let (view, plan, workspace) = (view.clone(), plan.clone(), workspace.to_path_buf());
    #[cfg(test)]
    let env_scope = crate::test_support::env_scope_ticket();
    Ok(tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        plan.passes
            .iter()
            .map(|pass| build_pr_pass_prompt(number, &view, &plan, pass, &workspace))
            .collect()
    })
    .await?)
}

pub(crate) fn build_pr_pass_prompt(
    number: u32,
    view: &super::review_pr::GhPullRequest,
    plan: &PrReviewPlan,
    pass: &PrReviewPass,
    workspace: &Path,
) -> String {
    let diff = super::review_pr::model_diff(&pass.diff);
    let context = super::review_pr::source_context(
        workspace,
        &view.head_sha,
        &pass.diff,
        plan.manifest
            .max_chars_per_pass
            .saturating_sub(pass.manifest.diff_chars),
    );
    // A degraded plan tells the model it is partial, so a pass summary can
    // never honestly claim full coverage; the manifest below carries the
    // same skip list for the record.
    let task = if plan.manifest.skipped_files.is_empty() {
        "Review only defects introduced in this pass. Use supplementary source to check surrounding guards and declarations; it does not expand the commentable diff. Binary contents and omitted callers are not inspected. No build or tests have been run.".to_string()
    } else {
        format!(
            "Review only defects introduced in this pass. This is a partial review (pass {} of {}): the gate did not read {}. Do not claim full coverage. Use supplementary source to check surrounding guards and declarations; it does not expand the commentable diff. Binary contents and omitted callers are not inspected. No build or tests have been run.",
            pass.manifest.number,
            plan.manifest.passes.len(),
            format_skipped_files(&plan.manifest.skipped_files)
        )
    };
    json!({
        "task": task,
        "untrusted_repository_data": true,
        "pull_request": { "number": number, "title": view.title, "description": view.body },
        "manifest": plan.manifest,
        "pass": pass.manifest,
        "diff": diff,
        "repository_context": context,
        "context_limit": "Context is bounded supplementary excerpts from the exact head. Null means no source context could fit. Missing files or omitted lines are not evidence of a defect."
    }).to_string()
}

/// Exact immutable facts from the same Core source-context collector and budget projection.
pub(crate) fn capture_pr_pass_snapshot(
    number: u32,
    view: &super::review_pr::GhPullRequest,
    plan: &PrReviewPlan,
    pass: &PrReviewPass,
    workspace: &Path,
) -> Value {
    let context = super::review_pr::source_context(
        workspace,
        &view.head_sha,
        &pass.diff,
        plan.manifest
            .max_chars_per_pass
            .saturating_sub(pass.manifest.diff_chars),
    );
    json!({"number":number,"view":super::review_host::view_snapshot(view),"manifest":plan.manifest,"pass":pass.manifest,"diff":super::review_pr::model_diff(&pass.diff),"context":context,"sort_keys":super::review_host::sort_json_keys()})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptPass {
    pub number: usize,
    pub response_content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptCoverage {
    pub manifest: PrReviewManifest,
    pub completed_passes: Vec<ReviewReceiptPass>,
}

pub(crate) struct PrReviewAccumulator {
    manifest: PrReviewManifest,
    outputs: Vec<ReviewOutput>,
    raw_outputs: Vec<String>,
}

impl PrReviewAccumulator {
    pub(crate) fn new(plan: &PrReviewPlan) -> Self {
        Self {
            manifest: plan.manifest.clone(),
            outputs: Vec::new(),
            raw_outputs: Vec::new(),
        }
    }

    pub(crate) fn accept(&mut self, pass: &PrReviewPass, raw: String) -> anyhow::Result<()> {
        let expected = self.outputs.len() + 1;
        anyhow::ensure!(
            pass.manifest.number == expected
                && self.manifest.passes.get(expected - 1) == Some(&pass.manifest),
            "Review pass arrived out of order or does not match the immutable manifest; expected pass {expected}"
        );
        let output = ReviewOutput::from_structured_str(&raw).ok_or_else(|| {
            anyhow::anyhow!(
                "Review pass {expected}/{} did not return valid structured JSON; the partial review was not accepted or posted.",
                self.manifest.passes.len()
            )
        })?;
        self.outputs.push(output);
        self.raw_outputs.push(raw);
        Ok(())
    }

    pub(crate) fn finish(
        self,
        complete_diff: &str,
    ) -> anyhow::Result<(ReviewOutput, String, ReviewReceiptCoverage)> {
        anyhow::ensure!(
            self.outputs.len() == self.manifest.passes.len(),
            "Only {}/{} review passes completed; the partial review was not accepted or posted.",
            self.outputs.len(),
            self.manifest.passes.len()
        );
        anyhow::ensure!(
            diff_fingerprint(complete_diff) == self.manifest.diff_fingerprint,
            "Complete PR diff fingerprint changed before review aggregation"
        );
        let mut issues = Vec::new();
        let mut suggestions = Vec::new();
        let mut summaries = Vec::new();
        let mut assessments = Vec::new();
        for (index, output) in self.outputs.into_iter().enumerate() {
            if !output.summary.is_empty() {
                summaries.push(format!("Pass {}: {}", index + 1, output.summary));
            }
            if !output.overall_assessment.is_empty() {
                assessments.push(format!("Pass {}: {}", index + 1, output.overall_assessment));
            }
            issues.extend(output.issues);
            suggestions.extend(output.suggestions);
        }
        let total = self.manifest.passes.len();
        let per_pass = if summaries.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", summaries.join("\n\n"))
        };
        // A degraded plan must never claim complete coverage: the summary
        // names every file the gate did not read.
        let summary = if self.manifest.skipped_files.is_empty() {
            format!(
                "Complete review coverage: {total}/{total} passes, {} file patches, {}.{per_pass}",
                self.manifest.file_count, self.manifest.diff_fingerprint,
            )
        } else {
            format!(
                "Partial review coverage: {total} pass(es) completed; the gate did not read: {}. Diff: {} file patches, {}.{per_pass}",
                format_skipped_files(&self.manifest.skipped_files),
                self.manifest.file_count,
                self.manifest.diff_fingerprint,
            )
        };
        let mut output = ReviewOutput {
            summary,
            issues,
            suggestions,
            merge_risk: None,
            verified: Vec::new(),
            not_reviewed: Vec::new(),
            overall_assessment: if assessments.is_empty() {
                if self.manifest.skipped_files.is_empty() {
                    format!("All {total} review passes completed with structured output.")
                } else {
                    format!(
                        "Partial review: {total} pass(es) completed with structured output; see the summary for files never read."
                    )
                }
            } else {
                assessments.join("\n")
            },
            ..Default::default()
        };
        output.note_binary_coverage(complete_diff);
        let completed_passes = self
            .raw_outputs
            .iter()
            .enumerate()
            .map(|(index, raw)| ReviewReceiptPass {
                number: index + 1,
                response_content_sha256: format!("sha256:{}", sha256_hex(raw.as_bytes())),
            })
            .collect();
        let content = self
            .raw_outputs
            .iter()
            .enumerate()
            .map(|(index, raw)| format!("PASS {}\n{raw}", index + 1))
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok((
            output,
            content,
            ReviewReceiptCoverage {
                manifest: self.manifest,
                completed_passes,
            },
        ))
    }
}

/// Resolve a model-supplied review path to the post-image form diff hunks
/// are keyed by: trimmed, with any `./` prefix removed. `None` means the
/// finding has no position at all.
#[must_use]
pub fn normalize_review_path(path: Option<&str>) -> Option<String> {
    let path = path?.trim().trim_start_matches("./");
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}

/// Where a suggestion can anchor in a diff, and whether its replacement may
/// be emitted as a one-click committable block.
///
/// This is the single source of truth for "is this committable": the PR
/// inline-comment path and the review receipt both derive from it, so a
/// receipt can never claim a suggestion was committable while the posted
/// comment degraded it to prose (or the reverse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionAnchor {
    /// No path or no line at all: only the summary body can carry it.
    NoPosition,
    /// Path and line exist but no hunk contains the line — a model-estimated
    /// position that missed the diff.
    Unanchorable { path: String },
    /// Anchored to RIGHT-side `path:start..=end`. `committable` is true only
    /// when the literal replacement passed every safety gate: non-empty,
    /// within the span-size budget, an explicitly bounded (or single-line)
    /// span, and fully covered by RIGHT-side hunk lines.
    Anchored {
        path: String,
        start: u32,
        end: u32,
        committable: bool,
    },
}

/// Resolve one suggestion against a diff's hunks.
#[must_use]
pub fn resolve_suggestion_anchor(
    suggestion: &ReviewSuggestion,
    hunks: &super::review_hunks::DiffHunks,
) -> SuggestionAnchor {
    let Some(path) = normalize_review_path(suggestion.path.as_deref()) else {
        return SuggestionAnchor::NoPosition;
    };
    let Some(end) = suggestion.end_line.or(suggestion.line) else {
        return SuggestionAnchor::NoPosition;
    };
    if !hunks.contains_line(&path, end) {
        return SuggestionAnchor::Unanchorable { path };
    }
    let start = suggestion.start_line.unwrap_or(end);
    // A model that gives neither start_line nor end_line has told us nothing
    // about how much code it means to replace. GitHub would happily *insert*
    // a multi-line replacement at a single-line anchor, duplicating the lines
    // the model meant to replace, so that shape is not committable.
    let explicit_span = suggestion.start_line.is_some() || suggestion.end_line.is_some();
    let committable = suggestion
        .replacement
        .as_deref()
        .is_some_and(|replacement| {
            !replacement.trim().is_empty()
                && start <= end
                && end.saturating_sub(start).saturating_add(1) <= MAX_COMMITTABLE_SUGGESTION_LINES
                && (explicit_span || start < end || !replacement.contains('\n'))
                && hunks.contains_span(&path, start, end)
        });
    SuggestionAnchor::Anchored {
        path,
        start,
        end,
        committable,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceipt {
    pub schema_version: u32,
    pub mode: String,
    pub generated_at: String,
    pub target: String,
    pub diff_fingerprint: String,
    pub diff_bytes: usize,
    pub diff_lines: usize,
    pub provider: String,
    pub model: String,
    pub checks_run: Vec<ReviewReceiptCheck>,
    pub findings: ReviewReceiptFindings,
    pub unresolved_risk: ReviewReceiptRisk,
    pub review_content_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<ReviewReceiptCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptCheck {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptFindings {
    pub summary: String,
    pub issue_count: usize,
    pub suggestion_count: usize,
    pub highest_severity: String,
    pub issues: Vec<ReviewReceiptIssue>,
    /// What the suggestion pipeline would do with each suggestion against
    /// this diff. `#[serde(default)]` keeps receipts written before the
    /// field existed readable, so the schema version does not move.
    #[serde(default)]
    pub suggestions: ReviewReceiptSuggestions,
}

/// One suggestion emitted as a one-click committable block: where it
/// anchored, nothing else. The replacement text is deliberately absent —
/// a receipt is an audit record, never a second channel for model-written
/// code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptSuggestion {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// Receipt provenance for the suggestion pipeline. The three counters use
/// the same [`SuggestionAnchor`] resolution as the posted inline comments,
/// so the numbers a receipt records are exactly what the PR path would
/// emit for the same diff.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptSuggestions {
    /// Suggestions emitted as committable blocks (one entry each, below).
    pub committable_count: usize,
    /// Anchor spans of the committable suggestions: path + line range, no
    /// replacement text.
    pub committable: Vec<ReviewReceiptSuggestion>,
    /// Suggestions whose anchor was valid but whose replacement failed a
    /// safety gate, so they posted as prose instead.
    pub degraded_to_prose: usize,
    /// Suggestions whose line missed every hunk (or whose file is not in
    /// the diff), so nothing was posted inline. Suggestions with no
    /// position at all are not counted — they never had an anchor to lose.
    pub dropped_unanchorable: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptIssue {
    pub severity: String,
    pub title: String,
    pub path: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptRisk {
    pub unresolved: bool,
    pub level: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewReceiptValidation {
    pub passed: bool,
    pub reason: String,
    pub diff_fingerprint: String,
    pub receipt_fingerprint: Option<String>,
    pub receipt_path: Option<PathBuf>,
    pub unresolved_risk: Option<ReviewReceiptRisk>,
}

/// Classify every suggestion in `output` against `diff` exactly as the PR
/// inline-comment path would, producing the receipt's provenance counts.
#[must_use]
pub fn suggestion_provenance(output: &ReviewOutput, diff: &str) -> ReviewReceiptSuggestions {
    let hunks = super::review_hunks::DiffHunks::parse(diff);
    let mut suggestions = ReviewReceiptSuggestions::default();
    for suggestion in &output.suggestions {
        match resolve_suggestion_anchor(suggestion, &hunks) {
            SuggestionAnchor::Anchored {
                path,
                start,
                end,
                committable: true,
            } => suggestions.committable.push(ReviewReceiptSuggestion {
                path,
                start_line: start,
                end_line: end,
            }),
            SuggestionAnchor::Anchored {
                committable: false, ..
            } => suggestions.degraded_to_prose += 1,
            SuggestionAnchor::Unanchorable { .. } => suggestions.dropped_unanchorable += 1,
            SuggestionAnchor::NoPosition => {}
        }
    }
    suggestions.committable_count = suggestions.committable.len();
    suggestions
}

#[must_use]
pub fn build_review_receipt(
    target: impl Into<String>,
    diff: &str,
    provider: impl Into<String>,
    model: impl Into<String>,
    output: &ReviewOutput,
    review_content: &str,
    checks_run: Vec<ReviewReceiptCheck>,
) -> ReviewReceipt {
    let highest_severity = highest_review_severity(output);
    let unresolved = !output.issues.is_empty();
    let risk_level = if unresolved {
        highest_severity.clone()
    } else {
        "none".to_string()
    };
    let risk_summary = if unresolved {
        format!(
            "{} unresolved review issue(s); highest severity: {highest_severity}",
            output.issues.len()
        )
    } else {
        "No structured unresolved issues reported by review output.".to_string()
    };

    ReviewReceipt {
        schema_version: REVIEW_RECEIPT_SCHEMA_VERSION,
        mode: "pre_push_review".to_string(),
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        target: target.into(),
        diff_fingerprint: diff_fingerprint(diff),
        diff_bytes: diff.len(),
        diff_lines: diff.lines().count(),
        provider: provider.into(),
        model: model.into(),
        checks_run,
        findings: ReviewReceiptFindings {
            summary: output.summary.clone(),
            issue_count: output.issues.len(),
            suggestion_count: output.suggestions.len(),
            highest_severity: highest_severity.clone(),
            suggestions: suggestion_provenance(output, diff),
            issues: output
                .issues
                .iter()
                .map(|issue| ReviewReceiptIssue {
                    severity: issue.severity.clone(),
                    title: issue.title.clone(),
                    path: issue.path.clone(),
                    line: issue.line,
                })
                .collect(),
        },
        unresolved_risk: ReviewReceiptRisk {
            unresolved,
            level: risk_level,
            summary: risk_summary,
        },
        review_content_sha256: sha256_hex(review_content.as_bytes()),
        coverage: None,
    }
}

pub(crate) fn attach_pr_review_coverage(
    receipt: &mut ReviewReceipt,
    coverage: ReviewReceiptCoverage,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        receipt.diff_fingerprint == coverage.manifest.diff_fingerprint,
        "Review receipt and PR coverage manifest fingerprints differ"
    );
    anyhow::ensure!(
        coverage.completed_passes.len() == coverage.manifest.passes.len()
            && coverage
                .completed_passes
                .iter()
                .enumerate()
                .all(|(index, pass)| pass.number == index + 1),
        "Review receipt does not cover every planned PR pass"
    );
    receipt.schema_version = PR_COVERAGE_RECEIPT_SCHEMA_VERSION;
    receipt.coverage = Some(coverage);
    Ok(())
}

pub fn write_review_receipt(
    receipt: &ReviewReceipt,
    path_override: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let path = if let Some(path) = path_override {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        path.to_path_buf()
    } else {
        let dir = codewhale_config::ensure_state_dir("review-receipts")?;
        let digest = receipt
            .diff_fingerprint
            .strip_prefix("sha256:")
            .unwrap_or(receipt.diff_fingerprint.as_str());
        let short = digest.chars().take(12).collect::<String>();
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        dir.join(format!("{stamp}-{short}.json"))
    };
    let encoded = serde_json::to_string_pretty(receipt)?;
    fs::write(&path, encoded)?;
    Ok(path)
}

pub fn read_review_receipt(path: &Path) -> anyhow::Result<ReviewReceipt> {
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn latest_review_receipt_for_diff(
    diff: &str,
) -> anyhow::Result<Option<(PathBuf, ReviewReceipt)>> {
    let dir = codewhale_config::resolve_state_dir("review-receipts")?;
    if !dir.is_dir() {
        return Ok(None);
    }

    let expected = diff_fingerprint(diff);
    let mut matches = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(receipt) = read_review_receipt(&path) else {
            continue;
        };
        if receipt.diff_fingerprint != expected {
            continue;
        }
        let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
        matches.push((modified, path, receipt));
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(matches.pop().map(|(_, path, receipt)| (path, receipt)))
}

#[must_use]
pub fn validate_review_receipt_for_diff(
    diff: &str,
    receipt: &ReviewReceipt,
    receipt_path: Option<PathBuf>,
) -> ReviewReceiptValidation {
    let expected = diff_fingerprint(diff);
    let mut validation = ReviewReceiptValidation {
        passed: false,
        reason: String::new(),
        diff_fingerprint: expected.clone(),
        receipt_fingerprint: Some(receipt.diff_fingerprint.clone()),
        receipt_path,
        unresolved_risk: Some(receipt.unresolved_risk.clone()),
    };

    if !matches!(
        (receipt.schema_version, receipt.coverage.is_some()),
        (REVIEW_RECEIPT_SCHEMA_VERSION, false) | (PR_COVERAGE_RECEIPT_SCHEMA_VERSION, true)
    ) {
        validation.reason = format!(
            "unsupported review receipt schema version {}",
            receipt.schema_version
        );
        return validation;
    }
    if receipt.diff_fingerprint != expected {
        validation.reason = "current diff fingerprint does not match receipt".to_string();
        return validation;
    }
    if let Some(coverage) = &receipt.coverage {
        if coverage.completed_passes.len() != coverage.manifest.passes.len()
            || coverage
                .completed_passes
                .iter()
                .enumerate()
                .any(|(index, pass)| {
                    pass.number != index + 1
                        || !valid_sha256_fingerprint(&pass.response_content_sha256)
                })
        {
            validation.reason = "review receipt has incomplete or unordered pass coverage".into();
            return validation;
        }
        let view = super::review_pr::GhPullRequest {
            base_sha: coverage.manifest.base_sha.clone(),
            head_sha: coverage.manifest.head_sha.clone(),
            changed_files: coverage.manifest.file_count,
            ..Default::default()
        };
        let Ok(plan) = plan_pr_review(
            diff,
            &view,
            coverage.manifest.max_chars_per_pass,
            coverage.manifest.passes.len(),
        ) else {
            validation.reason = "current diff cannot reproduce the receipt pass manifest".into();
            return validation;
        };
        if plan.manifest != coverage.manifest {
            validation.reason = "current diff pass manifest does not match receipt".into();
            return validation;
        }
        // A partial review is real findings, but it must never read as a
        // gate pass: the check fails, naming what the gate did not read.
        if !coverage.manifest.skipped_files.is_empty() {
            validation.reason = format!(
                "review receipt covers a partial review; the gate did not read: {}",
                format_skipped_files(&coverage.manifest.skipped_files)
            );
            return validation;
        }
    }
    if receipt.unresolved_risk.unresolved {
        validation.reason = receipt.unresolved_risk.summary.clone();
        return validation;
    }
    if let Some(check) = receipt
        .checks_run
        .iter()
        .find(|check| !review_receipt_check_status_passes(&check.status))
    {
        validation.reason = format!(
            "review receipt check '{}' did not pass: {}",
            check.name, check.status
        );
        return validation;
    }

    validation.passed = true;
    validation.reason = "receipt matches current diff and has no unresolved risk".to_string();
    validation
}

#[must_use]
pub(crate) fn receipt_matches_pr_revision(
    receipt: &ReviewReceipt,
    view: &super::review_pr::GhPullRequest,
) -> bool {
    receipt.coverage.as_ref().is_none_or(|coverage| {
        coverage.manifest.base_sha == view.base_sha
            && coverage.manifest.head_sha == view.head_sha
            && coverage.manifest.file_count == view.changed_files
    })
}

#[must_use]
pub fn diff_fingerprint(diff: &str) -> String {
    format!("sha256:{}", sha256_hex(diff.as_bytes()))
}

fn parse_review_output_json(raw: &str) -> Option<ReviewOutput> {
    if let Ok(parsed) = serde_json::from_str::<ReviewOutput>(raw) {
        return Some(parsed);
    }

    let Value::String(inner) = serde_json::from_str::<Value>(raw).ok()? else {
        return None;
    };
    if inner.trim().is_empty() || inner == raw {
        return None;
    }
    parse_review_output_json(&inner)
}

fn highest_review_severity(output: &ReviewOutput) -> String {
    let mut highest = "none";
    for issue in &output.issues {
        let severity = issue.severity.as_str();
        if severity_rank(severity) > severity_rank(highest) {
            highest = severity;
        }
    }
    highest.to_string()
}

fn severity_rank(severity: &str) -> u8 {
    super::review_core::review_severity_rank(severity)
}

fn review_receipt_check_status_passes(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "passed" | "pass" | "success" | "ok"
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    codewhale_runtime::hashing::sha256_hex(bytes)
}

fn valid_sha256_fingerprint(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub struct ReviewTool {
    client: Option<CodewhaleClient>,
    model: String,
}

impl ReviewTool {
    #[must_use]
    pub fn new(client: Option<CodewhaleClient>, model: String) -> Self {
        Self { client, model }
    }
}

#[async_trait]
impl ToolSpec for ReviewTool {
    fn name(&self) -> &'static str {
        "review"
    }

    fn description(&self) -> &'static str {
        "Run a structured code review for a file, git diff, or GitHub pull request."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "File path, PR URL, or the literal 'diff'/'staged' for git diff review."
                },
                "kind": {
                    "type": "string",
                    "description": "Optional explicit target type: file, diff, or pr."
                },
                "base": {
                    "type": "string",
                    "description": "Optional git base ref when using diff target (e.g. origin/main)."
                },
                "staged": {
                    "type": "boolean",
                    "description": "Review staged changes when using diff target (default: false)."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum source characters per pass (default: 200000). Input is never truncated."
                },
                "max_passes": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_REVIEW_PASSES,
                    "description": "Maximum complete PR review passes (default: 1, maximum: 64). Values above 1 explicitly authorize additional model requests for an oversized PR."
                }
            },
            "required": ["target"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Network]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match optional_u64(input, "max_passes", 1) {
            Ok(1) => ApprovalRequirement::Auto,
            _ => ApprovalRequirement::Required,
        }
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let Some(client) = self.client.clone() else {
            return Err(ToolError::not_available(REVIEW_CLIENT_UNAVAILABLE));
        };

        let target = required_str(&input, "target")?.trim();
        if target.is_empty() {
            return Err(ToolError::invalid_input("target cannot be empty"));
        }

        let kind = optional_str(&input, "kind")?.map(|s| s.trim().to_ascii_lowercase());
        let base = optional_str(&input, "base")?.map(|s| s.trim().to_string());
        let staged = optional_bool(&input, "staged", false)?;
        let max_chars =
            usize::try_from(optional_u64(&input, "max_chars", DEFAULT_MAX_CHARS as u64)?)
                .unwrap_or(DEFAULT_MAX_CHARS)
                .clamp(1, MAX_MAX_CHARS);
        let max_passes =
            usize::try_from(optional_u64(&input, "max_passes", 1)?).unwrap_or(usize::MAX);
        if !(1..=MAX_REVIEW_PASSES).contains(&max_passes) {
            return Err(ToolError::invalid_input(format!(
                "max_passes must be from 1 to {MAX_REVIEW_PASSES}"
            )));
        }

        let source =
            resolve_review_source(target, kind.as_deref(), staged, base.as_deref(), context)
                .await?;
        if !matches!(&source, ReviewSource::PullRequest { .. }) && max_passes != 1 {
            return Err(ToolError::invalid_input(
                "max_passes applies only to pull request reviews",
            ));
        }
        let plan = match &source {
            ReviewSource::PullRequest { diff, view, .. } => Some(
                plan_pr_review(diff, view, max_chars, max_passes)
                    .map_err(|error| ToolError::invalid_input(error.to_string()))?,
            ),
            _ => None,
        };
        let prompts = if let Some(plan) = &plan {
            let ReviewSource::PullRequest { pr, view, .. } = &source else {
                unreachable!("PR plan has PR source")
            };
            let number = pr
                .number
                .parse::<u32>()
                .map_err(|_| ToolError::invalid_input("Invalid pull request number"))?;
            super::review_host::pr_prompts(number, view, plan, context).await?
        } else {
            validate_review_source_size(&source, max_chars)?;
            vec![if context
                .features
                .enabled(codewhale_runtime::features::Feature::ReviewHost)
            {
                super::review_host::source_prompt(review_source_snapshot(&source), context).await?
            } else {
                build_review_prompt(&source)
            }]
        };

        let route = client.effective_route_envelope(&self.model, chrono::Utc::now());
        let review_diff = match &source {
            ReviewSource::PullRequest { diff, .. } => diff.clone(),
            ReviewSource::Diff { diff, .. } => diff.clone(),
            ReviewSource::File { .. } => String::new(),
        };
        let tool_workspace = match &source {
            ReviewSource::PullRequest { view, .. } => {
                super::review_core::repository_tool_workspace(
                    &context.workspace,
                    Some(view.head_sha.as_str()),
                )
            }
            _ => super::review_core::repository_tool_workspace(&context.workspace, None),
        };
        let triage = super::review_triage::triage_review(&review_diff);
        let reserve_percent =
            crate::route_budget::review_visible_text_reserve_percent(&route.model);
        let budget = super::review_triage::budget_for(
            &triage,
            client.effective_max_output_tokens(&route.model),
        );
        let allowance = budget.max_output_tokens;
        let reasoning_effort =
            (codewhale_models::model_reasoning_capability(&route.model) == Some(true)).then(|| {
                bounded_review_reasoning_effort(
                    crate::reasoning_preference::ReasoningEffort::Max,
                    reserve_percent,
                    allowance,
                )
                .normalize_for_route_identity(
                    client.api_provider(),
                    client.admitted_provider_identity().key.as_str(),
                    client.base_url(),
                    &route.model,
                )
                .as_setting()
                .to_string()
            });
        let mut usage = Usage::default();
        let mut accumulator = plan.as_ref().map(PrReviewAccumulator::new);
        let mut single_output = None;
        let mut anchors = super::review_core::ReviewAnchorReport::default();
        let mut tools_used: Vec<String> = Vec::new();
        let mut steps = 0;
        let mut tool_calls = 0;
        let mut changed_lines = 0;
        let mut outcome = super::review_core::ReviewCoreOutcome::Completed;
        let mut stop = None;
        let mut spend_usd = None;
        for (index, prompt) in prompts.into_iter().enumerate() {
            let mut request = super::review_core::ReviewCoreRequest::new(&review_diff, prompt);
            request.workspace = tool_workspace.clone();
            request.budget = budget.tool_budget();
            request.max_output_tokens = Some(allowance);
            request.reasoning_effort = reasoning_effort.clone();
            let result =
                match super::review_core::run_review_core(&client, &self.model, request).await {
                    Ok(result) => result,
                    Err(error) => {
                        return Ok(review_error_with_usage(
                            &route,
                            &usage,
                            format!(
                                "{}; no partial review was accepted.",
                                request_failure_message(
                                    index + 1,
                                    plan.as_ref().map_or(1, |plan| plan.passes.len()),
                                    &error
                                )
                            ),
                        ));
                    }
                };
            add_review_usage(&mut usage, &result.usage);
            for tool in &result.tools_used {
                if !tools_used.iter().any(|seen| seen == tool) {
                    tools_used.push(tool.clone());
                }
            }
            anchors.claims += result.anchors.claims;
            anchors.anchored += result.anchors.anchored;
            anchors.unanchored += result.anchors.unanchored;
            steps += result.steps;
            tool_calls += result.tool_calls;
            changed_lines += result.coverage.changed_lines;
            outcome = result.outcome;
            stop = result.stop.clone().or(stop);
            spend_usd = result.spend_usd.or(spend_usd);
            if codewhale_models::is_incomplete_stop_reason(result.stop_reason.as_deref()) {
                return Ok(review_error_with_usage(
                    &route,
                    &usage,
                    format!(
                        "Review pass {}/{} response incomplete: provider stop reason `{}`; the pass {}. The partial review was not accepted.",
                        index + 1,
                        plan.as_ref().map_or(1, |plan| plan.passes.len()),
                        codewhale_models::stop_reason_detail(result.stop_reason.as_deref()),
                        exhausted_pass_clause(&result.usage)
                    ),
                ));
            }
            if result.outcome == super::review_core::ReviewCoreOutcome::LeakedToolCall {
                return Ok(review_error_with_usage(
                    &route,
                    &usage,
                    leaked_tool_call_message(
                        index + 1,
                        plan.as_ref().map_or(1, |plan| plan.passes.len()),
                    ),
                ));
            }
            if let (Some(plan), Some(accumulator)) = (&plan, accumulator.as_mut()) {
                if let Err(error) = accumulator.accept(&plan.passes[index], result.raw) {
                    return Ok(review_error_with_usage(&route, &usage, error.to_string()));
                }
            } else {
                match accept_single_review(&result.raw) {
                    Ok(_) => single_output = Some(result.output),
                    Err(error) => {
                        return Ok(review_error_with_usage(&route, &usage, error));
                    }
                }
            }
        }
        if let Err(error) = ensure_pr_source_current(&source, &context.workspace).await {
            return Ok(review_error_with_usage(&route, &usage, error.to_string()));
        }
        let mut coverage = None;
        let output = if let (Some(accumulator), ReviewSource::PullRequest { diff, .. }) =
            (accumulator, &source)
        {
            let (output, _, completed) = match accumulator.finish(diff) {
                Ok(completed) => completed,
                Err(error) => {
                    return Ok(review_error_with_usage(&route, &usage, error.to_string()));
                }
            };
            coverage = Some(completed);
            output
        } else {
            single_output.expect("one non-PR review response")
        };
        let mut output = output;
        if let Some(note) = stop
            .as_ref()
            .map(|stop| super::review_triage::partial_review_note(stop, &triage, &budget))
        {
            if !output.summary.is_empty() {
                output.summary.push_str("\n\n");
            }
            output.summary.push_str(&note);
        }
        let mut metadata = review_usage_metadata(&route, &usage);
        metadata["review_anchors"] = json!(anchors);
        metadata["review_tools_used"] = json!(tools_used);
        metadata["review_repository_reads"] = json!(tool_workspace.is_some());
        metadata["review_steps"] = json!(steps);
        metadata["review_tool_calls"] = json!(tool_calls);
        metadata["review_changed_lines"] = json!(changed_lines);
        metadata["review_triage"] = json!(triage);
        metadata["review_budget"] = json!(budget);
        metadata["review_stop"] =
            json!(stop.as_ref().map(super::review_triage::ReviewStop::as_str));
        metadata["review_spend_usd"] = json!(spend_usd);
        metadata["review_outcome"] = json!(match outcome {
            super::review_core::ReviewCoreOutcome::Completed => "completed",
            super::review_core::ReviewCoreOutcome::NoFinalAnswer => "no_final_answer",
            super::review_core::ReviewCoreOutcome::StepBudgetExhausted => "step_budget_exhausted",
            super::review_core::ReviewCoreOutcome::LeakedToolCall => "leaked_tool_call",
        });
        if let Some(plan) = &plan {
            metadata["review_passes"] = json!(plan.passes.len());
            metadata["diff_fingerprint"] = json!(plan.manifest.diff_fingerprint.as_str());
            metadata["review_coverage"] = match serde_json::to_value(coverage) {
                Ok(coverage) => coverage,
                Err(error) => {
                    return Ok(review_error_with_usage(&route, &usage, error.to_string()));
                }
            };
        }
        let result = match ToolResult::json(&output) {
            Ok(result) => result,
            Err(error) => {
                return Ok(review_error_with_usage(&route, &usage, error.to_string()));
            }
        };
        Ok(result.with_metadata(metadata))
    }
}

pub(crate) fn leaked_tool_call_message(pass: usize, passes: usize) -> String {
    format!(
        "Review pass {pass}/{passes} returned tool-call markup instead of a review: the model tried to call tools this pass did not offer. This is not a review result, and the response was not accepted or posted."
    )
}

pub(crate) fn exhausted_pass_clause(usage: &Usage) -> String {
    let visible_tokens = usage
        .output_tokens
        .saturating_sub(usage.reasoning_tokens.unwrap_or(0));
    if visible_tokens > 0 {
        format!(
            "was cut off before the review finished ({visible_tokens} visible tokens came back)"
        )
    } else {
        "produced no review text".to_string()
    }
}

/// Accept one non-PR review reply (#6561 D03-m4). Prose that ignores the
/// JSON contract is still a review (see [`ReviewOutput::from_str`]), but an
/// empty reply, or JSON that carries no summary, issue, suggestion or
/// assessment (`{}`, an unrelated object), used to become an empty
/// successful review that read as clean.
fn accept_single_review(response_text: &str) -> Result<ReviewOutput, String> {
    let output = ReviewOutput::from_str(response_text);
    if response_text.trim().is_empty()
        || (output.summary.is_empty()
            && output.issues.is_empty()
            && output.suggestions.is_empty()
            && output.overall_assessment.is_empty())
    {
        return Err(
            "Review response carried no review content (empty, or JSON without summary, \
             issues, suggestions or overall_assessment); no review was accepted."
                .to_string(),
        );
    }
    Ok(output)
}

fn review_error_with_usage(
    route: &crate::cost_status::EffectiveRouteEnvelope,
    usage: &Usage,
    message: impl Into<String>,
) -> ToolResult {
    ToolResult::error(message.into()).with_metadata(review_usage_metadata(route, usage))
}

fn review_usage_metadata(
    route: &crate::cost_status::EffectiveRouteEnvelope,
    usage: &Usage,
) -> Value {
    let mut metadata = json!({
        "tool": "review",
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
    });
    // Every billable class, from the one shared producer, so a child turn can be
    // priced with the same completeness as a parent turn (#4318).
    crate::cost_status::attach_child_usage_metadata(&mut metadata, route, usage);
    metadata
}

/// Describe a failed review request with its whole error chain.
///
/// The client wraps the retry loop's `LlmError` in one outer context (the
/// bare "Responses API request failed" / "Chat API request failed"), and
/// `{error}` prints only that layer. The alternate format walks the chain,
/// so the class the `LlmError` names (quota, auth, rate limit, upstream 5xx,
/// network, timeout) and its sanitized provider body reach the log and the
/// review workflow's non-run classifier. Both the agent-callable `ReviewTool`
/// and the `codewhale review` CLI path go through here.
pub(crate) fn request_failure_message(
    pass: usize,
    planned: usize,
    error: &anyhow::Error,
) -> String {
    format!("Review pass {pass}/{planned} request failed: {error:#}")
}

pub(crate) fn add_review_usage(total: &mut Usage, next: &Usage) {
    crate::core::turn::add_usage_to(total, next);
}

enum ReviewSource {
    File {
        display: String,
        content: String,
    },
    Diff {
        label: String,
        diff: String,
    },
    PullRequest {
        label: String,
        diff: String,
        pr: PullRequestRef,
        view: Box<super::review_pr::GhPullRequest>,
    },
}

async fn resolve_review_source(
    target: &str,
    kind: Option<&str>,
    staged: bool,
    base: Option<&str>,
    context: &ToolContext,
) -> Result<ReviewSource, ToolError> {
    if let Some(kind) = kind {
        return match kind {
            "file" => resolve_file_target(target, context),
            "diff" => {
                let diff = resolve_diff_target(context.workspace.as_path(), staged, base).await?;
                Ok(ReviewSource::Diff {
                    label: "git diff".to_string(),
                    diff,
                })
            }
            "pr" | "pull" | "pull_request" => {
                let pr = parse_pr_url(target)
                    .ok_or_else(|| ToolError::invalid_input("Invalid pull request URL"))?;
                gh_pr_source(pr, &context.workspace).await
            }
            other => Err(ToolError::invalid_input(format!(
                "Unknown review kind '{other}'"
            ))),
        };
    }

    if let Some(pr) = parse_pr_url(target) {
        return gh_pr_source(pr, &context.workspace).await;
    }

    if let Some(staged_override) = diff_mode_from_target(target) {
        let staged = staged || staged_override;
        let diff = resolve_diff_target(context.workspace.as_path(), staged, base).await?;
        return Ok(ReviewSource::Diff {
            label: if staged {
                "git diff --cached"
            } else {
                "git diff"
            }
            .to_string(),
            diff,
        });
    }

    resolve_file_target(target, context)
}

fn resolve_file_target(target: &str, context: &ToolContext) -> Result<ReviewSource, ToolError> {
    let path = crate::tools::file::resolve_guarded_read_path(context, target, "review")?;
    if !path.is_file() {
        return Err(ToolError::invalid_input(format!(
            "Target is not a file: {}",
            path.display()
        )));
    }
    let content = fs::read_to_string(&path).map_err(|e| {
        ToolError::execution_failed(format!("Failed to read file {}: {e}", path.display()))
    })?;
    let display = path
        .strip_prefix(&context.workspace)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    Ok(ReviewSource::File { display, content })
}

async fn resolve_diff_target(
    workspace: &Path,
    staged: bool,
    base: Option<&str>,
) -> Result<String, ToolError> {
    let base = base.map(str::trim).filter(|base| !base.is_empty());
    let base_commit = if let Some(base) = base {
        Some(super::git::resolve_commit_ref(workspace, base).await?)
    } else {
        None
    };

    let mut args = vec!["diff".to_string()];
    args.extend(codewhale_dependencies::Git::REVIEW_DIFF_ARGS.map(String::from));
    if staged {
        args.push("--cached".to_string());
        if let Some(base_commit) = base_commit {
            // `git diff --cached <base>...HEAD` is invalid because the index
            // is already one side of this diff. Preserve triple-dot semantics
            // by resolving the merge base first, then compare that tree with
            // the index (committed branch work plus the staged snapshot).
            let output = run_review_git(
                workspace,
                vec!["merge-base".to_string(), base_commit, "HEAD".to_string()],
                "resolve staged review merge base",
            )
            .await?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(ToolError::execution_failed(format!(
                    "git merge-base failed: {}",
                    stderr.trim()
                )));
            }
            let merge_base = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if merge_base.is_empty() || !merge_base.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(ToolError::execution_failed(
                    "git merge-base returned an invalid commit id",
                ));
            }
            args.push(merge_base);
        }
    } else if let Some(base_commit) = base_commit {
        args.push(format!("{base_commit}...HEAD"));
    }
    args.push("--".to_string());

    let output = run_review_git(workspace, args, "generate review diff").await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::execution_failed(format!(
            "git diff failed: {}",
            stderr.trim()
        )));
    }
    let diff = String::from_utf8_lossy(&output.stdout).to_string();
    if diff.trim().is_empty() {
        return Err(ToolError::invalid_input("No diff to review"));
    }
    Ok(diff)
}

async fn run_review_git(
    workspace: &Path,
    args: Vec<String>,
    operation: &'static str,
) -> Result<std::process::Output, ToolError> {
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = codewhale_dependencies::Git::review_command(&workspace)
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        cmd.args(args).output().map_err(|e| {
            ToolError::execution_failed(format!("Failed to {operation} with git: {e}"))
        })
    })
    .await
    .map_err(|e| ToolError::execution_failed(format!("git {operation} task panicked: {e}")))?
}

async fn gh_pr_source(pr: PullRequestRef, workspace: &Path) -> Result<ReviewSource, ToolError> {
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let number = pr
            .number
            .parse::<u32>()
            .map_err(|_| ToolError::invalid_input("Invalid pull request number"))?;
        let repo = format!("{}/{}", pr.owner, pr.repo);
        let view = super::review_pr::fetch_view(number, Some(&repo), &workspace)
            .map_err(|error| ToolError::execution_failed(format!("{error:#}")))?;
        let diff = super::review_pr::fetch_diff(number, Some(&repo), &workspace, &view)
            .map_err(|error| ToolError::execution_failed(format!("{error:#}")))?;
        Ok(ReviewSource::PullRequest {
            label: pr.label(),
            diff,
            pr,
            view: Box::new(view),
        })
    })
    .await
    .map_err(|error| ToolError::execution_failed(format!("PR input task failed: {error}")))?
}

async fn ensure_pr_source_current(
    source: &ReviewSource,
    workspace: &Path,
) -> Result<(), ToolError> {
    if let ReviewSource::PullRequest { pr, view, .. } = source {
        let pr = pr.clone();
        let view = view.clone();
        let workspace = workspace.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let number = pr
                .number
                .parse::<u32>()
                .map_err(|_| ToolError::invalid_input("Invalid pull request number"))?;
            super::review_pr::ensure_current(
                number,
                Some(&format!("{}/{}", pr.owner, pr.repo)),
                &workspace,
                &view,
            )
            .map_err(|error| ToolError::execution_failed(format!("{error:#}")))
        })
        .await
        .map_err(|error| {
            ToolError::execution_failed(format!("PR revision check failed: {error}"))
        })??;
    }
    Ok(())
}

/// Refuse the complete numbered source before either formatter/provider path.
fn validate_review_source_size(source: &ReviewSource, max_chars: usize) -> Result<(), ToolError> {
    let chars = match source {
        ReviewSource::File { content, .. } => {
            let mut count = 0usize;
            for (index, line) in content.lines().enumerate() {
                if index > 0 {
                    count = count.saturating_add(1);
                }
                let digits = (index + 1).ilog10() as usize + 1;
                count = count
                    .saturating_add(digits.max(4) + 3)
                    .saturating_add(line.chars().count());
            }
            count
        }
        ReviewSource::Diff { diff, .. } => diff.chars().count(),
        ReviewSource::PullRequest { .. } => return Ok(()), // Core pass planner owns PR bounds.
    };
    if chars > max_chars {
        return Err(ToolError::invalid_input(format!(
            "Complete review source has {chars} characters, exceeding max_chars={max_chars}; no source was truncated and no review was run"
        )));
    }
    Ok(())
}
fn review_source_snapshot(source: &ReviewSource) -> Value {
    match source {
        ReviewSource::File { display, content } => {
            json!({"kind":"file","display":display,"content":content})
        }
        ReviewSource::Diff { label, diff } => json!({"kind":"diff","label":label,"diff":diff}),
        ReviewSource::PullRequest {
            label, diff, view, ..
        } => {
            json!({"kind":"pr","label":label,"diff":super::review_pr::model_diff(diff),"head_sha":view.head_sha,"base_sha":view.base_sha})
        }
    }
}

fn build_review_prompt(source: &ReviewSource) -> String {
    match source {
        ReviewSource::File {
            display, content, ..
        } => {
            let numbered = format_with_line_numbers(content);
            format!(
                "Review the following file and provide feedback.\n\
Path: {display}\n\n{numbered}\n\nEnd of file."
            )
        }
        ReviewSource::Diff { label, diff } => {
            format!("Review the following {label} and provide feedback.\n\n{diff}\n\nEnd of diff.")
        }
        ReviewSource::PullRequest {
            label, diff, view, ..
        } => {
            let diff = super::review_pr::model_diff(diff);
            format!(
                "Review the complete pull request diff ({label}) at head {} and base {}. Binary changes are represented by metadata; their contents are not semantically inspected. Exact binary object IDs remain in the review evidence.\n\n{diff}\n\nEnd of diff.",
                view.head_sha, view.base_sha,
            )
        }
    }
}

fn format_with_line_numbers(content: &str) -> String {
    content
        .lines()
        .enumerate()
        .map(|(idx, line)| format!("{:>4} | {}", idx + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn normalize_claim_kind(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "intent" | "intent_mismatch" | "unannounced" => "intent",
        "contract" | "dependent" | "impact" => "contract",
        "defect" | "bug" | "" => "defect",
        other if other.starts_with("intent") => "intent",
        other if other.starts_with("contract") || other.starts_with("depend") => "contract",
        _ => "defect",
    }
    .to_string()
}

fn normalize_evidence_tier(value: &str) -> String {
    let lower = value.trim().to_ascii_lowercase();
    if lower.starts_with("repro") || lower.starts_with("verif") {
        "reproduced".to_string()
    } else if lower.starts_with("trace") || lower.starts_with("proven") {
        "traced".to_string()
    } else {
        "suspected".to_string()
    }
}

fn normalize_severity(value: &str) -> String {
    super::review_core::normalize_review_severity(value)
}

fn extract_json_block(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        None
    } else {
        Some(&raw[start..=end])
    }
}

fn diff_mode_from_target(target: &str) -> Option<bool> {
    match target.trim().to_ascii_lowercase().as_str() {
        "diff" | "git diff" | "changes" | "working tree" | "working-tree" => Some(false),
        "staged" | "cached" | "git diff --cached" | "git diff --staged" => Some(true),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct PullRequestRef {
    owner: String,
    repo: String,
    number: String,
}

impl PullRequestRef {
    fn label(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }
}

fn parse_pr_url(url: &str) -> Option<PullRequestRef> {
    let trimmed = url.trim().trim_end_matches('/');
    if !trimmed.starts_with("http") {
        return None;
    }
    let parts: Vec<&str> = trimmed.split('/').collect();
    let pull_idx = parts.iter().position(|part| *part == "pull")?;
    if pull_idx < 2 || pull_idx + 1 >= parts.len() {
        return None;
    }
    let owner = parts.get(pull_idx.saturating_sub(2))?;
    let repo = parts.get(pull_idx.saturating_sub(1))?;
    let number = parts.get(pull_idx + 1)?;
    if owner.is_empty() || repo.is_empty() || number.is_empty() {
        return None;
    }
    Some(PullRequestRef {
        owner: (*owner).to_string(),
        repo: (*repo).to_string(),
        number: (*number).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_failure_message_keeps_the_provider_failure_beneath_the_context() {
        let error = anyhow::Error::new(crate::llm_client::LlmError::ServerError {
            status: 503,
            message: "upstream unavailable".into(),
        })
        .context("Responses API request failed");
        let message = request_failure_message(1, 1, &error);
        assert_eq!(
            message,
            "Review pass 1/1 request failed: Responses API request failed: Server error (503): upstream unavailable"
        );
    }

    #[test]
    fn complete_numbered_review_source_refuses_instead_of_truncating() {
        for content in ["", "漢字🐋e\u{301}\r\nlast\r", "\n", "x\n"] {
            let source = ReviewSource::File {
                display: "x".into(),
                content: content.into(),
            };
            let numbered = format_with_line_numbers(content);
            let chars = numbered.chars().count();
            assert!(validate_review_source_size(&source, chars).is_ok());
            if chars > 0 {
                assert!(matches!(
                    validate_review_source_size(&source, chars - 1),
                    Err(ToolError::InvalidInput { .. })
                ));
            }
            assert!(build_review_prompt(&source).contains(&numbered));
        }
        let source = ReviewSource::Diff {
            label: "working diff".into(),
            diff: "漢字🐋END".into(),
        };
        assert!(validate_review_source_size(&source, 6).is_ok());
        assert!(validate_review_source_size(&source, 5).is_err());
        assert!(build_review_prompt(&source).contains("漢字🐋END"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actual_review_tool_refuses_complete_source_before_provider_on_both_backends() {
        let _home = crate::test_support::SealedHome::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("source.rs"),
            "unreviewed tail must not disappear",
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let client = CodewhaleClient::new(&{
            let __cw_config_init_0_0 = Some("moonshot".into());
            let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
                moonshot: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("local-fixture-key".into()),
                    base_url: Some(base),
                    model: Some("kimi-k2.5".into()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        })
        .unwrap();
        let tool = ReviewTool::new(Some(client), "kimi-k2.5".into());
        for host in [false, true] {
            let mut features = codewhale_runtime::features::Features::with_defaults();
            if host {
                features.enable(codewhale_runtime::features::Feature::ReviewHost);
            }
            let context = ToolContext::new(root.path()).with_features(features);
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                tool.execute(
                    json!({"target":"source.rs","kind":"file","max_chars":8}),
                    &context,
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(matches!(error, ToolError::InvalidInput { .. }), "{error}");
            assert!(error.to_string().contains("no source was truncated"));
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "provider must not be called"
            );
        }
    }

    fn pr_view(files: usize) -> super::super::review_pr::GhPullRequest {
        super::super::review_pr::GhPullRequest {
            title: "Batch fixture".into(),
            body: "Review every pass".into(),
            base: "main".into(),
            head: "feature".into(),
            url: "https://github.com/example/repo/pull/1".into(),
            base_sha: "a".repeat(40),
            head_sha: "b".repeat(40),
            changed_files: files,
            additions: files,
            deletions: 0,
        }
    }

    fn pr_patch(name: &str, content: &str) -> String {
        format!(
            "diff --git a/{name} b/{name}\nnew file mode 100644\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+{content}\n"
        )
    }

    fn pr_multi_hunk_patch(name: &str, contents: &[&str]) -> String {
        let mut patch = format!(
            "diff --git a/{name} b/{name}\nindex {}..{} 100644\n--- a/{name}\n+++ b/{name}\n",
            "1".repeat(40),
            "2".repeat(40)
        );
        for (index, content) in contents.iter().enumerate() {
            patch.push_str(&format!(
                "@@ -{0},1 +{0},1 @@\n-old{0}\n+{content}\n",
                index + 1
            ));
        }
        patch
    }

    fn clean_pass(summary: &str) -> String {
        json!({
            "summary": summary,
            "issues": [],
            "suggestions": [],
            "overall_assessment": "No issue in this pass"
        })
        .to_string()
    }

    #[test]
    fn pr_batch_plan_degrades_to_first_pass_and_names_skipped_files() {
        let patches = [
            pr_patch("a.txt", "alpha"),
            pr_patch("b.txt", "🐋"),
            pr_patch("c.txt", "charlie"),
        ];
        let diff = patches.concat();
        let max_chars = patches
            .iter()
            .map(|patch| patch.chars().count())
            .max()
            .unwrap();
        // One pass of budget: the first file is reviewed, the rest are
        // skipped in diff order and named — never silently dropped.
        let degraded = plan_pr_review(&diff, &pr_view(3), max_chars, 1).unwrap();
        assert_eq!(degraded.passes.len(), 1);
        assert_eq!(degraded.passes[0].diff, patches[0]);
        assert_eq!(degraded.manifest.passes[0].files, ["a/a.txt b/a.txt"]);
        assert_eq!(degraded.manifest.skipped_files.len(), 2);
        assert_eq!(degraded.manifest.skipped_files[0].file, "a/b.txt b/b.txt");
        assert_eq!(degraded.manifest.skipped_files[1].file, "a/c.txt b/c.txt");
        assert!(
            degraded
                .manifest
                .skipped_files
                .iter()
                .all(|skip| skip.reason == SKIP_REASON_BEYOND_MAX_PASSES)
        );

        let plan = plan_pr_review(&diff, &pr_view(3), max_chars, 3).unwrap();
        assert_eq!(plan.passes.len(), 3);
        assert!(plan.manifest.skipped_files.is_empty());
        assert_eq!(
            plan.passes
                .iter()
                .map(|pass| pass.diff.as_str())
                .collect::<String>(),
            diff
        );
        assert_eq!(plan.manifest.diff_chars, diff.chars().count());
        assert_eq!(plan.manifest.passes[1].files, ["a/b.txt b/b.txt"]);
        assert!(plan.passes[1].diff.contains("🐋"));
    }

    #[test]
    fn pr_batch_plan_rejects_one_file_overflow_before_any_pass() {
        let diff = pr_patch("large.txt", &"x".repeat(200));
        let error = plan_pr_review(&diff, &pr_view(1), 100, MAX_REVIEW_PASSES).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("covers 0 of 1 file patches"), "{message}");
        assert!(message.contains("large.txt"), "{message}");
        assert!(message.contains(SKIP_REASON_HUNK_EXCEEDS_PASS), "{message}");
        assert!(message.contains("No review was run or posted"), "{message}");
    }

    #[test]
    fn pr_batch_plan_skips_oversized_file_and_reviews_the_rest() {
        let ok = pr_patch("ok.txt", "fine");
        let big = pr_multi_hunk_patch("big.txt", &["fine", &"x".repeat(500)]);
        let diff = format!("{ok}{big}");
        let max_chars = ok.chars().count();
        let plan = plan_pr_review(&diff, &pr_view(2), max_chars, MAX_REVIEW_PASSES).unwrap();
        assert_eq!(plan.passes.len(), 1);
        assert_eq!(plan.passes[0].diff, ok);
        assert_eq!(plan.manifest.skipped_files.len(), 1);
        assert_eq!(plan.manifest.skipped_files[0].file, "a/big.txt b/big.txt");
        assert_eq!(
            plan.manifest.skipped_files[0].reason,
            SKIP_REASON_HUNK_EXCEEDS_PASS
        );
        assert!(plan.manifest.skipped_files[0].chars > max_chars);
    }

    #[test]
    fn pr_batch_plan_splits_oversized_file_only_at_complete_hunk_boundaries() {
        let contents = ["alpha", "bravo", "charlie", "delta"];
        let patch = pr_multi_hunk_patch("big.txt", &contents);
        let (header, hunks) = pr_file_hunks(&patch);
        assert_eq!(hunks.len(), 4);
        assert!(hunks.iter().all(|hunk| hunk.starts_with("@@ ")));
        assert_eq!(format!("{header}{}", hunks.concat()), patch);

        // A file that fits is never split.
        let whole = plan_pr_review(&patch, &pr_view(1), patch.chars().count(), 1).unwrap();
        assert_eq!(whole.passes.len(), 1);
        assert_eq!(whole.passes[0].diff, patch);
        assert_eq!(whole.manifest.passes[0].files, ["a/big.txt b/big.txt"]);

        // Header plus the largest hunk fits, so header plus any two hunks does
        // not: exactly one hunk per part, four parts, four passes.
        let max_chars =
            header.chars().count() + hunks.iter().map(|hunk| hunk.chars().count()).max().unwrap();
        // Three passes of budget for four parts: the first three parts are
        // reviewed and the last part is skipped by name.
        let degraded = plan_pr_review(&patch, &pr_view(1), max_chars, 3).unwrap();
        assert_eq!(degraded.passes.len(), 3);
        assert_eq!(degraded.manifest.skipped_files.len(), 1);
        assert_eq!(
            degraded.manifest.skipped_files[0].file,
            "a/big.txt b/big.txt (part 4/4)"
        );
        assert_eq!(
            degraded.manifest.skipped_files[0].reason,
            SKIP_REASON_BEYOND_MAX_PASSES
        );

        let plan = plan_pr_review(&patch, &pr_view(1), max_chars, 4).unwrap();
        assert_eq!(plan.passes.len(), 4);
        assert_eq!(plan.manifest.file_count, 1);
        let mut rebuilt = String::new();
        for (index, pass) in plan.passes.iter().enumerate() {
            assert!(pass.diff.starts_with(header));
            assert!(pass.diff.contains(&format!("+{}", contents[index])));
            assert_eq!(pass.manifest.diff_chars, pass.diff.chars().count());
            assert!(pass.manifest.diff_chars <= max_chars);
            assert_eq!(
                pass.manifest.files,
                [format!("a/big.txt b/big.txt (part {}/4)", index + 1)]
            );
            assert_eq!(pass.manifest.file_count, 1);
            if index == 0 {
                rebuilt.push_str(&pass.diff);
            } else {
                rebuilt.push_str(
                    pass.diff
                        .strip_prefix(header)
                        .expect("continuation part replays the full file header"),
                );
            }
        }
        assert_eq!(rebuilt, patch);
    }

    #[test]
    fn pr_batch_plan_refuses_when_one_hunk_with_header_cannot_fit() {
        let patch = pr_multi_hunk_patch("mixed.txt", &["ok", &"x".repeat(500)]);
        let (header, hunks) = pr_file_hunks(&patch);
        // The small hunk fits with the header; the large one does not, so the
        // file cannot be split and the plan must fail before any pass.
        let max_chars = header.chars().count() + hunks[0].chars().count();
        let error = plan_pr_review(&patch, &pr_view(1), max_chars, MAX_REVIEW_PASSES).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("mixed.txt"), "{message}");
        assert!(message.contains("covers 0 of 1 file patches"), "{message}");
        assert!(message.contains(SKIP_REASON_HUNK_EXCEEDS_PASS), "{message}");
        assert!(message.contains("No review was run or posted"), "{message}");
    }

    #[test]
    fn pr_batch_plan_never_splits_a_binary_patch_for_its_omitted_payload() {
        let patch = format!(
            "diff --git a/blob.bin b/blob.bin\nindex {}..{} 100644\nGIT binary patch\nliteral 8\n{}\n",
            "1".repeat(40),
            "2".repeat(40),
            "z".repeat(10_000)
        );
        let model_chars = super::super::review_pr::model_diff(&patch).chars().count();
        assert!(model_chars < patch.chars().count());
        let plan = plan_pr_review(&patch, &pr_view(1), model_chars, 1).unwrap();
        assert_eq!(plan.passes.len(), 1);
        assert_eq!(plan.passes[0].diff, patch);
        assert_eq!(plan.manifest.passes[0].files, ["a/blob.bin b/blob.bin"]);
        assert_eq!(plan.manifest.binary_file_patches, 1);
    }

    #[test]
    fn pr_batch_plan_counts_distinct_files_when_a_split_shares_the_plan() {
        let hunk_content = "x".repeat(200);
        let big = pr_multi_hunk_patch(
            "big.txt",
            &[
                hunk_content.as_str(),
                hunk_content.as_str(),
                hunk_content.as_str(),
            ],
        );
        let small = pr_patch("small.txt", "tiny");
        let diff = format!("{big}{small}");
        let (header, hunks) = pr_file_hunks(&big);
        let hunk_chars = hunks[0].chars().count();
        let header_chars = header.chars().count();
        // Two parts for big.txt (header + two hunks, header + one hunk), then
        // small.txt packed after the second part.
        let max_chars = header_chars + 2 * hunk_chars + small.chars().count();
        assert!(big.chars().count() > max_chars);
        let plan = plan_pr_review(&diff, &pr_view(2), max_chars, 2).unwrap();
        assert_eq!(plan.passes.len(), 2);
        assert_eq!(plan.manifest.file_count, 2);
        assert_eq!(
            plan.manifest.passes[0].files,
            ["a/big.txt b/big.txt (part 1/2)"]
        );
        assert_eq!(plan.manifest.passes[0].file_count, 1);
        assert_eq!(
            plan.manifest.passes[1].files,
            [
                "a/big.txt b/big.txt (part 2/2)".to_string(),
                "a/small.txt b/small.txt".to_string()
            ]
        );
        assert_eq!(plan.manifest.passes[1].file_count, 2);
        // The second pass holds big.txt's continuation (header replayed),
        // then small.txt whole; stripping the one repeated header rebuilds
        // the original diff byte-for-byte.
        let mut rebuilt = plan.passes[0].diff.clone();
        rebuilt.push_str(
            plan.passes[1]
                .diff
                .strip_prefix(header)
                .expect("continuation part replays the full file header"),
        );
        assert_eq!(rebuilt, diff);
    }

    #[test]
    fn pr_batch_accumulator_rejects_missing_malformed_and_unordered_middle_passes() {
        let patches = [
            pr_patch("a.txt", "alpha"),
            pr_patch("b.txt", "bravo"),
            pr_patch("c.txt", "charlie"),
        ];
        let diff = patches.concat();
        let max_chars = patches
            .iter()
            .map(|patch| patch.chars().count())
            .max()
            .unwrap();
        let plan = plan_pr_review(&diff, &pr_view(3), max_chars, 3).unwrap();

        let mut missing = PrReviewAccumulator::new(&plan);
        missing
            .accept(&plan.passes[0], clean_pass("first"))
            .unwrap();
        assert!(
            missing
                .finish(&diff)
                .unwrap_err()
                .to_string()
                .contains("Only 1/3")
        );

        let mut malformed = PrReviewAccumulator::new(&plan);
        malformed
            .accept(&plan.passes[0], clean_pass("first"))
            .unwrap();
        assert!(
            malformed
                .accept(&plan.passes[1], "not JSON".into())
                .is_err()
        );
        assert!(malformed.accept(&plan.passes[1], "{}".into()).is_err());
        assert!(
            malformed
                .finish(&diff)
                .unwrap_err()
                .to_string()
                .contains("Only 1/3")
        );

        let mut unordered = PrReviewAccumulator::new(&plan);
        assert!(
            unordered
                .accept(&plan.passes[1], clean_pass("second"))
                .is_err()
        );
    }

    #[test]
    fn pr_batch_aggregate_binds_complete_diff_counts_coverage_and_revision() {
        let first = pr_patch("a.txt", "alpha");
        let second = pr_patch("b.txt", "bravo");
        let diff = format!("{first}{second}");
        let max_chars = first.chars().count().max(second.chars().count());
        let view = pr_view(2);
        let plan = plan_pr_review(&diff, &view, max_chars, 2).unwrap();
        let mut accumulator = PrReviewAccumulator::new(&plan);
        accumulator
            .accept(
                &plan.passes[0],
                json!({
                    "summary": "first",
                    "issues": [{"severity":"warning","title":"A","description":"a","path":"a.txt","line":1}],
                    "suggestions": [],
                    "overall_assessment": "first assessment"
                })
                .to_string(),
            )
            .unwrap();
        accumulator
            .accept(
                &plan.passes[1],
                json!({
                    "summary": "second",
                    "issues": [{"severity":"error","title":"B","description":"b","path":"b.txt","line":1}],
                    "suggestions": [{"path":"b.txt","line":1,"suggestion":"fix"}],
                    "overall_assessment": "second assessment"
                })
                .to_string(),
            )
            .unwrap();
        let (output, content, coverage) = accumulator.finish(&diff).unwrap();
        assert_eq!(output.issues.len(), 2);
        assert_eq!(output.suggestions.len(), 1);
        assert!(output.summary.contains("2/2 passes, 2 file patches"));
        assert_eq!(coverage.completed_passes.len(), 2);

        let mut receipt = build_review_receipt(
            "pr:1",
            &diff,
            "fixture",
            "fixture-model",
            &output,
            &content,
            Vec::new(),
        );
        attach_pr_review_coverage(&mut receipt, coverage).unwrap();
        assert_eq!(receipt.schema_version, PR_COVERAGE_RECEIPT_SCHEMA_VERSION);
        assert_eq!(receipt.findings.issue_count, 2);
        assert_eq!(receipt.findings.suggestion_count, 1);
        let unresolved = validate_review_receipt_for_diff(&diff, &receipt, None);
        assert!(!unresolved.passed);
        assert!(unresolved.reason.contains("unresolved review issue"));

        let mut clean_accumulator = PrReviewAccumulator::new(&plan);
        for (index, pass) in plan.passes.iter().enumerate() {
            clean_accumulator
                .accept(pass, clean_pass(&format!("clean pass {}", index + 1)))
                .unwrap();
        }
        let (clean_output, clean_content, clean_coverage) =
            clean_accumulator.finish(&diff).unwrap();
        let mut receipt = build_review_receipt(
            "pr:1",
            &diff,
            "fixture",
            "fixture-model",
            &clean_output,
            &clean_content,
            Vec::new(),
        );
        attach_pr_review_coverage(&mut receipt, clean_coverage).unwrap();
        assert!(validate_review_receipt_for_diff(&diff, &receipt, None).passed);
        let mut missing = receipt.clone();
        missing
            .coverage
            .as_mut()
            .unwrap()
            .completed_passes
            .remove(0);
        assert!(!validate_review_receipt_for_diff(&diff, &missing, None).passed);
        let mut tampered = receipt.clone();
        tampered
            .coverage
            .as_mut()
            .unwrap()
            .manifest
            .passes
            .swap(0, 1);
        assert!(!validate_review_receipt_for_diff(&diff, &tampered, None).passed);
        let mut drifted = view.clone();
        drifted.head_sha = "c".repeat(40);
        assert!(!receipt_matches_pr_revision(&receipt, &drifted));
        assert!(
            PrReviewAccumulator::new(&plan)
                .finish(&(diff.clone() + "drift"))
                .is_err()
        );
    }

    #[test]
    fn pr_batch_aggregate_reports_partial_coverage_and_receipt_check_names_skips() {
        let first = pr_patch("a.txt", "alpha");
        let second = pr_patch("b.txt", "bravo");
        let diff = format!("{first}{second}");
        let max_chars = first.chars().count().max(second.chars().count());
        let plan = plan_pr_review(&diff, &pr_view(2), max_chars, 1).unwrap();
        assert_eq!(plan.passes.len(), 1);
        assert_eq!(plan.manifest.skipped_files.len(), 1);
        let mut accumulator = PrReviewAccumulator::new(&plan);
        accumulator
            .accept(
                &plan.passes[0],
                json!({
                    "summary": "first",
                    "issues": [],
                    "suggestions": [],
                    "overall_assessment": ""
                })
                .to_string(),
            )
            .unwrap();
        let (output, content, coverage) = accumulator.finish(&diff).unwrap();
        assert!(
            output.summary.contains("Partial review coverage"),
            "{}",
            output.summary
        );
        assert!(
            output.summary.contains("a/b.txt b/b.txt"),
            "{}",
            output.summary
        );
        assert!(
            !output.summary.contains("Complete review coverage"),
            "{}",
            output.summary
        );
        assert!(
            output.overall_assessment.contains("Partial review"),
            "{}",
            output.overall_assessment
        );
        let mut receipt = build_review_receipt(
            "pr:1",
            &diff,
            "fixture",
            "fixture-model",
            &output,
            &content,
            Vec::new(),
        );
        attach_pr_review_coverage(&mut receipt, coverage).unwrap();
        let validation = validate_review_receipt_for_diff(&diff, &receipt, None);
        assert!(!validation.passed);
        assert!(
            validation.reason.contains("partial review"),
            "{}",
            validation.reason
        );
        assert!(
            validation.reason.contains("a/b.txt b/b.txt"),
            "{}",
            validation.reason
        );
    }

    #[test]
    fn pr_pass_prompt_marks_degraded_plans_partial_for_the_model() {
        let first = pr_patch("a.txt", "alpha");
        let second = pr_patch("b.txt", "bravo");
        let diff = format!("{first}{second}");
        let max_chars = first.chars().count().max(second.chars().count());
        let view = pr_view(2);
        let workspace = tempfile::tempdir().unwrap();
        let degraded = plan_pr_review(&diff, &view, max_chars, 1).unwrap();
        let prompt =
            build_pr_pass_prompt(1, &view, &degraded, &degraded.passes[0], workspace.path());
        let task = serde_json::from_str::<serde_json::Value>(&prompt).unwrap()["task"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(task.contains("partial review"), "{task}");
        assert!(task.contains("pass 1 of 1"), "{task}");
        assert!(task.contains("a/b.txt b/b.txt"), "{task}");
        let complete = plan_pr_review(&diff, &view, max_chars, 2).unwrap();
        let prompt =
            build_pr_pass_prompt(1, &view, &complete, &complete.passes[0], workspace.path());
        let task = serde_json::from_str::<serde_json::Value>(&prompt).unwrap()["task"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!task.contains("partial review"), "{task}");
    }

    #[test]
    fn review_usage_aggregates_every_billable_counter() {
        let mut total = Usage::default();
        let mut first = Usage {
            input_tokens: 10,
            output_tokens: 3,
            prompt_cache_hit_tokens: Some(2),
            reasoning_tokens: Some(4),
            input_audio_tokens: Some(3),
            output_audio_tokens: Some(1),
            ..Default::default()
        };
        first.server_tool_use = Some(codewhale_models::ServerToolUsage {
            code_execution_requests: Some(1),
            tool_search_requests: None,
        });
        let second = Usage {
            input_tokens: 20,
            output_tokens: 5,
            prompt_cache_hit_tokens: Some(7),
            reasoning_tokens: Some(6),
            input_audio_tokens: Some(4),
            output_audio_tokens: Some(1),
            server_tool_use: Some(codewhale_models::ServerToolUsage {
                code_execution_requests: Some(2),
                tool_search_requests: Some(3),
            }),
            ..Default::default()
        };
        add_review_usage(&mut total, &first);
        add_review_usage(&mut total, &second);
        assert_eq!(total.input_tokens, 30);
        assert_eq!(total.output_tokens, 8);
        assert_eq!(total.prompt_cache_hit_tokens, Some(9));
        assert_eq!(total.reasoning_tokens, Some(10));
        assert_eq!(total.input_audio_tokens, Some(7));
        assert_eq!(total.output_audio_tokens, Some(2));
        assert_eq!(total.request_usages, vec![first, second]);
        assert_eq!(
            total.server_tool_use.unwrap().code_execution_requests,
            Some(3)
        );
    }

    #[test]
    fn additional_review_passes_require_human_approval() {
        let tool = ReviewTool::new(None, "unused".to_string());
        assert_eq!(
            tool.approval_requirement_for(&json!({"target":"diff"})),
            ApprovalRequirement::Auto
        );
        assert_eq!(
            tool.approval_requirement_for(
                &json!({"target":"https://github.com/a/b/pull/1","max_passes":2})
            ),
            ApprovalRequirement::Required
        );
        assert_eq!(
            tool.approval_requirement_for(&json!({"target":"diff","max_passes":"invalid"})),
            ApprovalRequirement::Required
        );
    }

    #[test]
    fn malformed_second_pass_and_drift_return_all_prior_usage_without_coverage() {
        let first = pr_patch("a.txt", "alpha");
        let second = pr_patch("b.txt", "bravo");
        let diff = format!("{first}{second}");
        let plan = plan_pr_review(
            &diff,
            &pr_view(2),
            first.chars().count().max(second.chars().count()),
            2,
        )
        .unwrap();
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            codewhale_config::ProviderKind::Custom,
            "test",
            "test-model",
            None,
            chrono::Utc::now(),
        );
        let mut usage = Usage::default();
        for input_tokens in [11, 13] {
            add_review_usage(
                &mut usage,
                &Usage {
                    input_tokens,
                    output_tokens: 2,
                    ..Default::default()
                },
            );
        }

        let mut malformed = PrReviewAccumulator::new(&plan);
        malformed
            .accept(&plan.passes[0], clean_pass("first"))
            .unwrap();
        let error = malformed.accept(&plan.passes[1], "{}".into()).unwrap_err();
        let result = review_error_with_usage(&route, &usage, error.to_string());
        assert!(!result.success);
        let metadata = result.metadata.unwrap();
        assert_eq!(metadata["input_tokens"], 24);
        assert_eq!(metadata["output_tokens"], 4);
        assert_eq!(
            crate::cost_status::child_usage_from_metadata(&metadata),
            Some(usage.clone())
        );
        assert_eq!(usage.request_usages.len(), 2);
        assert!(metadata.get("review_coverage").is_none());

        let mut drift = PrReviewAccumulator::new(&plan);
        drift.accept(&plan.passes[0], clean_pass("first")).unwrap();
        drift.accept(&plan.passes[1], clean_pass("second")).unwrap();
        let error = drift.finish(&(diff + "drift")).unwrap_err();
        let result = review_error_with_usage(&route, &usage, error.to_string());
        assert!(!result.success);
        assert_eq!(result.metadata.unwrap()["input_tokens"], 24);
    }

    #[tokio::test]
    async fn missing_review_client_uses_codewhale_provider_neutral_language() {
        let tool = ReviewTool::new(None, "unused".to_string());
        let context = ToolContext::new(PathBuf::from("."));

        let error = tool
            .execute(json!({}), &context)
            .await
            .expect_err("review requires a configured model client")
            .to_string();

        assert_eq!(
            error,
            "Failed to locate tool: Review tool requires an active Codewhale model client"
        );
        assert!(!error.contains("DeepSeek"));
    }

    fn fixture_git(workspace: &Path, args: &[&str]) -> std::process::Output {
        let mut command = codewhale_dependencies::Git::command().expect("git test dependency");
        let output = command
            .args(args)
            .current_dir(workspace)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[tokio::test]
    async fn staged_diff_with_base_compares_merge_base_to_index() {
        let repo = tempfile::TempDir::new().expect("temp git repository");
        fixture_git(repo.path(), &["init"]);
        fixture_git(repo.path(), &["config", "user.name", "Codewhale Test"]);
        fixture_git(
            repo.path(),
            &["config", "user.email", "codewhale-test@example.invalid"],
        );

        let tracked = repo.path().join("tracked.txt");
        fs::write(&tracked, "base\n").expect("write base fixture");
        fixture_git(repo.path(), &["add", "tracked.txt"]);
        fixture_git(repo.path(), &["commit", "-m", "base"]);
        let base =
            String::from_utf8_lossy(&fixture_git(repo.path(), &["rev-parse", "HEAD"]).stdout)
                .trim()
                .to_string();

        fs::write(&tracked, "base\ncommitted\n").expect("write committed fixture");
        fixture_git(repo.path(), &["add", "tracked.txt"]);
        fixture_git(repo.path(), &["commit", "-m", "branch change"]);
        fs::write(&tracked, "base\ncommitted\nstaged\n").expect("write staged fixture");
        fixture_git(repo.path(), &["add", "tracked.txt"]);
        fs::write(&tracked, "base\ncommitted\nstaged\nunstaged\n").expect("write unstaged fixture");

        let diff = resolve_diff_target(repo.path(), true, Some(&base))
            .await
            .expect("staged review diff from base");
        assert!(diff.contains("+committed"), "{diff}");
        assert!(diff.contains("+staged"), "{diff}");
        assert!(!diff.contains("unstaged"), "{diff}");
    }

    #[test]
    fn binary_coverage_limit_is_part_of_the_returned_review_summary() {
        let mut review = ReviewOutput::from_str(r#"{"summary":"Review findings"}"#);
        review.note_binary_coverage("diff --git a/image b/image\nGIT binary patch\nliteral 4\n");
        assert!(review.summary.contains("not semantically inspected"));
        let mut metadata_review = ReviewOutput::from_str(r#"{"summary":"Review findings"}"#);
        metadata_review.note_binary_coverage(
            "diff --git a/image b/image\nBinary files a/image and b/image differ\n",
        );
        assert!(
            metadata_review
                .summary
                .contains("not semantically inspected")
        );
        let mut text_review = ReviewOutput::from_str(r#"{"summary":"Text review"}"#);
        text_review.note_binary_coverage("diff --git a/a b/a\n@@ -0,0 +1 @@\n+text\n");
        assert_eq!(text_review.summary, "Text review");
    }

    #[test]
    fn parses_pr_url() {
        let pr =
            parse_pr_url("https://github.com/deepseek-ai/deepseek-cli/pull/123").expect("parse pr");
        assert_eq!(pr.owner, "deepseek-ai");
        assert_eq!(pr.repo, "deepseek-cli");
        assert_eq!(pr.number, "123");
    }

    #[test]
    fn ignores_non_pr_url() {
        assert!(parse_pr_url("https://github.com/deepseek-ai/deepseek-cli").is_none());
        assert!(parse_pr_url("not-a-url").is_none());
    }

    #[test]
    fn extracts_json_block() {
        let raw = "prefix {\"summary\":\"ok\"} suffix";
        let block = extract_json_block(raw).expect("block");
        assert!(block.contains("\"summary\""));
    }

    #[test]
    fn review_output_parses_structured_json() {
        let raw = r#"{
            "summary": " Looks good overall ",
            "issues": [{
                "severity": "high",
                "title": " Missing test ",
                "description": " Add coverage ",
                "path": " src/lib.rs ",
                "line": 42
            }],
            "suggestions": [{
                "path": "",
                "line": 7,
                "suggestion": " Keep the helper small "
            }],
            "overall_assessment": " Safe after test "
        }"#;

        let output = ReviewOutput::from_str(raw);

        assert_eq!(output.summary, "Looks good overall");
        assert_eq!(output.issues.len(), 1);
        assert_eq!(output.issues[0].severity, "blocker");
        assert_eq!(output.issues[0].title, "Missing test");
        assert_eq!(output.issues[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(output.issues[0].line, Some(42));
        assert_eq!(output.suggestions.len(), 1);
        assert_eq!(output.suggestions[0].path, None);
        assert_eq!(output.suggestions[0].line, Some(7));
        assert_eq!(output.suggestions[0].suggestion, "Keep the helper small");
        assert_eq!(output.overall_assessment, "Safe after test");
    }

    #[test]
    fn review_output_parses_double_encoded_json_string() {
        let inner = serde_json::json!({
            "summary": "structured",
            "issues": [{
                "severity": "warning",
                "title": "Risk",
                "description": "The parser should not fall back to a raw JSON string.",
                "path": "src/main.rs",
                "line": 3
            }],
            "suggestions": [],
            "overall_assessment": "usable"
        })
        .to_string();
        let double_encoded = serde_json::to_string(&inner).expect("encode string");

        let output = ReviewOutput::from_str(&double_encoded);

        assert_eq!(output.summary, "structured");
        assert_eq!(output.issues.len(), 1);
        assert_eq!(output.issues[0].severity, "concern");
        assert_eq!(output.issues[0].path.as_deref(), Some("src/main.rs"));
        assert_eq!(output.overall_assessment, "usable");
    }

    #[test]
    fn single_review_refuses_blank_or_contractless_replies() {
        for raw in ["", "   \n", "{}", r#"{"verdict":"ok"}"#, "```json\n{}\n```"] {
            assert!(
                accept_single_review(raw).is_err(),
                "{raw:?} must not become a clean review"
            );
        }
        let prose = accept_single_review("Looks good; no findings.").expect("prose review");
        assert_eq!(prose.summary, "Looks good; no findings.");
        let structured = accept_single_review(
            r#"{"summary":"One risk","issues":[],"suggestions":[],"overall_assessment":"ok"}"#,
        )
        .expect("structured review");
        assert_eq!(structured.summary, "One risk");
    }

    #[test]
    fn review_output_fallback_keeps_summary() {
        let output = ReviewOutput::from_str("Not JSON");
        assert!(!output.summary.is_empty());
        assert!(output.issues.is_empty());
    }

    #[test]
    fn review_usage_metadata_reports_child_tokens_for_cost_accrual() {
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            codewhale_config::ProviderKind::Deepseek,
            "deepseek",
            "deepseek-v4-flash",
            Some("https://api.deepseek.com/v1"),
            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"),
        );
        let metadata = review_usage_metadata(
            &route,
            &Usage {
                input_tokens: 123,
                output_tokens: 45,
                prompt_cache_hit_tokens: Some(100),
                prompt_cache_miss_tokens: Some(23),
                reasoning_tokens: Some(7),
                ..Default::default()
            },
        );

        assert_eq!(metadata["tool"], "review");
        assert_eq!(metadata["child_model"], "deepseek-v4-flash");
        assert_eq!(metadata["child_input_tokens"], 123);
        assert_eq!(metadata["child_output_tokens"], 45);
        assert_eq!(metadata["child_prompt_cache_hit_tokens"], 100);
        assert_eq!(metadata["child_prompt_cache_miss_tokens"], 23);
        assert_eq!(metadata["child_reasoning_tokens"], 7);
    }

    #[test]
    fn pre_push_diff_review_receipt_includes_fingerprint_and_risk() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n+let risky = true;\n";
        let output = ReviewOutput {
            summary: "Found one issue".to_string(),
            issues: vec![ReviewIssue {
                severity: "concern".to_string(),
                title: "Missing test".to_string(),
                description: "Add coverage".to_string(),
                path: Some("src/lib.rs".to_string()),
                line: Some(12),
                ..Default::default()
            }],
            suggestions: vec![ReviewSuggestion {
                path: Some("src/lib.rs".to_string()),
                line: Some(12),
                start_line: None,
                end_line: None,
                suggestion: "Add a regression test".to_string(),
                replacement: None,
            }],
            overall_assessment: "Needs a test".to_string(),
            ..Default::default()
        };

        let receipt = build_review_receipt(
            "working-tree",
            diff,
            "deepseek",
            "deepseek-v4-pro",
            &output,
            "review body",
            vec![ReviewReceiptCheck {
                name: "cargo test -p codewhale-tui".to_string(),
                status: "passed".to_string(),
            }],
        );

        assert_eq!(receipt.schema_version, REVIEW_RECEIPT_SCHEMA_VERSION);
        assert_eq!(receipt.mode, "pre_push_review");
        assert_eq!(receipt.target, "working-tree");
        assert_eq!(receipt.diff_fingerprint, diff_fingerprint(diff));
        assert_eq!(receipt.diff_lines, 2);
        assert_eq!(receipt.provider, "deepseek");
        assert_eq!(receipt.model, "deepseek-v4-pro");
        assert_eq!(receipt.checks_run.len(), 1);
        assert_eq!(receipt.findings.issue_count, 1);
        assert_eq!(receipt.findings.suggestion_count, 1);
        assert_eq!(receipt.findings.highest_severity, "concern");
        assert!(receipt.unresolved_risk.unresolved);
        assert_eq!(receipt.unresolved_risk.level, "concern");
        assert_eq!(
            receipt.review_content_sha256,
            sha256_hex("review body".as_bytes())
        );
    }

    #[test]
    fn review_receipt_records_committable_suggestion_provenance() {
        // Built from a slice, not one string literal: a `\` continuation
        // strips the leading space that marks a context line.
        let diff = [
            "diff --git a/src/lib.rs b/src/lib.rs",
            "--- a/src/lib.rs",
            "+++ b/src/lib.rs",
            "@@ -10,2 +10,3 @@ fn head() {",
            " let a = 1;",
            "+let b = a.unwrap();",
            " let c = 2;",
            "",
        ]
        .join("\n");
        let output = ReviewOutput {
            summary: "One fix, one judgement call, one miss".to_string(),
            issues: Vec::new(),
            suggestions: vec![
                // Valid anchor, explicit in-hunk span, literal replacement:
                // the only shape that may become a committable block.
                ReviewSuggestion {
                    path: Some("src/lib.rs".to_string()),
                    line: Some(12),
                    start_line: Some(11),
                    end_line: Some(12),
                    suggestion: "Use the checked variant".to_string(),
                    replacement: Some("let b = a.unwrap_or_default();\nlet c = 2;".to_string()),
                },
                // Valid anchor, no literal replacement: degrades to prose.
                ReviewSuggestion {
                    path: Some("src/lib.rs".to_string()),
                    line: Some(11),
                    start_line: None,
                    end_line: None,
                    suggestion: "Add a test".to_string(),
                    replacement: None,
                },
                // Line 25 sits between hunks: unanchorable.
                ReviewSuggestion {
                    path: Some("src/lib.rs".to_string()),
                    line: Some(25),
                    start_line: None,
                    end_line: None,
                    suggestion: "Wrong line".to_string(),
                    replacement: Some("x = 1;".to_string()),
                },
                // No position at all: summary-body only, counted nowhere here.
                ReviewSuggestion {
                    path: None,
                    line: None,
                    start_line: None,
                    end_line: None,
                    suggestion: "Consider renaming".to_string(),
                    replacement: None,
                },
            ],
            overall_assessment: "Fix the unwrap".to_string(),
            ..Default::default()
        };

        let receipt = build_review_receipt(
            "working-tree",
            &diff,
            "deepseek",
            "deepseek-v4-pro",
            &output,
            "review body",
            Vec::new(),
        );

        let provenance = &receipt.findings.suggestions;
        assert_eq!(provenance.committable_count, 1);
        assert_eq!(
            provenance.committable,
            vec![ReviewReceiptSuggestion {
                path: "src/lib.rs".to_string(),
                start_line: 11,
                end_line: 12,
            }]
        );
        assert_eq!(provenance.degraded_to_prose, 1);
        assert_eq!(provenance.dropped_unanchorable, 1);
        assert_eq!(receipt.findings.suggestion_count, 4);

        // Provenance records anchors, never code: the replacement text must
        // not leak into the receipt.
        let serialized = serde_json::to_string(&receipt).expect("serialize receipt");
        assert!(!serialized.contains("unwrap_or_default"), "{serialized}");
        assert!(!serialized.contains("x = 1;"), "{serialized}");
    }

    #[test]
    fn review_receipt_without_suggestion_provenance_still_decodes() {
        // Receipts written before the provenance field existed are schema v1;
        // the field is additive and serde-defaulted, so they must keep
        // decoding and the schema version must not move.
        let output = ReviewOutput::from_str("Looks good");
        let receipt = build_review_receipt(
            "working-tree",
            "diff --git a/a b/a\n",
            "deepseek",
            "deepseek-v4-flash",
            &output,
            "Looks good",
            Vec::new(),
        );
        let mut value = serde_json::to_value(&receipt).expect("serialize");
        value
            .get_mut("findings")
            .expect("findings")
            .as_object_mut()
            .expect("findings object")
            .remove("suggestions");
        let legacy: ReviewReceipt = serde_json::from_value(value).expect("legacy receipt decodes");
        assert_eq!(legacy.schema_version, REVIEW_RECEIPT_SCHEMA_VERSION);
        assert_eq!(
            legacy.findings.suggestions,
            ReviewReceiptSuggestions::default()
        );
    }

    #[test]
    fn write_review_receipt_accepts_override_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("receipt.json");
        let output = ReviewOutput::from_str("Looks good");
        let receipt = build_review_receipt(
            "staged",
            "diff --git a/a b/a\n",
            "deepseek",
            "deepseek-v4-flash",
            &output,
            "Looks good",
            Vec::new(),
        );

        let written = write_review_receipt(&receipt, Some(&path)).expect("write receipt");

        assert_eq!(written, path);
        let raw = fs::read_to_string(&written).expect("read receipt");
        let decoded: ReviewReceipt = serde_json::from_str(&raw).expect("decode receipt");
        assert_eq!(decoded.diff_fingerprint, receipt.diff_fingerprint);
        assert_eq!(decoded.unresolved_risk.level, "none");
    }

    #[test]
    fn review_receipt_validation_passes_matching_clean_receipt() {
        let diff = "diff --git a/a b/a\n+ok\n";
        let output = ReviewOutput::from_str("Looks good");
        let receipt = build_review_receipt(
            "working-tree",
            diff,
            "deepseek",
            "deepseek-v4-flash",
            &output,
            "Looks good",
            vec![ReviewReceiptCheck {
                name: "cargo test".to_string(),
                status: "passed".to_string(),
            }],
        );

        let validation = validate_review_receipt_for_diff(diff, &receipt, None);

        assert!(validation.passed);
        assert_eq!(validation.diff_fingerprint, diff_fingerprint(diff));
        assert_eq!(
            validation.reason,
            "receipt matches current diff and has no unresolved risk"
        );
    }

    #[test]
    fn review_receipt_validation_rejects_changed_diff() {
        let output = ReviewOutput::from_str("Looks good");
        let receipt = build_review_receipt(
            "working-tree",
            "diff --git a/a b/a\n+old\n",
            "deepseek",
            "deepseek-v4-flash",
            &output,
            "Looks good",
            Vec::new(),
        );

        let validation =
            validate_review_receipt_for_diff("diff --git a/a b/a\n+new\n", &receipt, None);

        assert!(!validation.passed);
        assert_eq!(
            validation.reason,
            "current diff fingerprint does not match receipt"
        );
    }

    #[test]
    fn review_receipt_validation_rejects_unresolved_risk() {
        let diff = "diff --git a/a b/a\n+risk\n";
        let output = ReviewOutput {
            summary: "Risk found".to_string(),
            issues: vec![ReviewIssue {
                severity: "blocker".to_string(),
                title: "Unsafe change".to_string(),
                description: "Needs work".to_string(),
                path: Some("a".to_string()),
                line: Some(1),
                ..Default::default()
            }],
            suggestions: Vec::new(),
            overall_assessment: String::new(),
            ..Default::default()
        };
        let receipt = build_review_receipt(
            "working-tree",
            diff,
            "deepseek",
            "deepseek-v4-flash",
            &output,
            "Risk found",
            Vec::new(),
        );

        let validation = validate_review_receipt_for_diff(diff, &receipt, None);

        assert!(!validation.passed);
        assert_eq!(
            validation.unresolved_risk.as_ref().unwrap().level,
            "blocker"
        );
        assert!(validation.reason.contains("unresolved review issue"));
    }

    #[test]
    fn review_receipt_validation_rejects_failed_check() {
        let diff = "diff --git a/a b/a\n+ok\n";
        let output = ReviewOutput::from_str("Looks good");
        let receipt = build_review_receipt(
            "working-tree",
            diff,
            "deepseek",
            "deepseek-v4-flash",
            &output,
            "Looks good",
            vec![ReviewReceiptCheck {
                name: "cargo test".to_string(),
                status: "failed".to_string(),
            }],
        );

        let validation = validate_review_receipt_for_diff(diff, &receipt, None);

        assert!(!validation.passed);
        assert!(
            validation
                .reason
                .contains("review receipt check 'cargo test' did not pass")
        );
    }

    #[test]
    fn review_receipt_validation_rejects_attached_not_run_check() {
        let diff = "diff --git a/a b/a\n+ok\n";
        let output = ReviewOutput::from_str("Looks good");
        let receipt = build_review_receipt(
            "working-tree",
            diff,
            "deepseek",
            "deepseek-v4-flash",
            &output,
            "Looks good",
            vec![ReviewReceiptCheck {
                name: "cargo test".to_string(),
                status: "not_run".to_string(),
            }],
        );

        let validation = validate_review_receipt_for_diff(diff, &receipt, None);

        assert!(!validation.passed);
        assert!(
            validation
                .reason
                .contains("review receipt check 'cargo test' did not pass: not_run")
        );
    }

    #[test]
    fn bounded_review_effort_caps_reasoning_by_visible_text_reserve() {
        use crate::reasoning_preference::ReasoningEffort;

        // Half the allowance must survive as text: reasoning capped to Low.
        assert_eq!(
            bounded_review_reasoning_effort(ReasoningEffort::Max, 50, 65_536),
            ReasoningEffort::Low
        );
        // A quarter reserved: capped to Medium.
        assert_eq!(
            bounded_review_reasoning_effort(ReasoningEffort::Max, 25, 65_536),
            ReasoningEffort::Medium
        );
        // Nothing reserved (non-reasoning model): request untouched.
        assert_eq!(
            bounded_review_reasoning_effort(ReasoningEffort::Max, 0, 65_536),
            ReasoningEffort::Max
        );
        // The cap never raises a lower request.
        assert_eq!(
            bounded_review_reasoning_effort(ReasoningEffort::Low, 50, 65_536),
            ReasoningEffort::Low
        );
        assert_eq!(
            bounded_review_reasoning_effort(ReasoningEffort::Off, 50, 65_536),
            ReasoningEffort::Off
        );
        // Unresolved Auto must not survive as unbounded either.
        assert_eq!(
            bounded_review_reasoning_effort(ReasoningEffort::Auto, 50, 65_536),
            ReasoningEffort::Low
        );
    }
}
