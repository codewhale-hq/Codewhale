//! Bounded, non-transcript presentation for delegated coordination receipts.
//!
//! Headless callers keep the machine-readable typed projection. The TUI uses
//! this single formatter for its Work inspector so compact rows and details do
//! not grow a second, string-parsed coordination model.

use std::fmt::Write as _;

use codewhale_engine::tools::subagent::CoordinationDetailProjection;
use codewhale_engine::tools::subagent::coord::{DecisionStatus, ReconciliationReceipt};
use codewhale_localization::{Locale, MessageId, tr};

#[must_use]
pub(crate) fn summary(locale: Locale, projection: &CoordinationDetailProjection) -> String {
    [
        tr(locale, MessageId::CoordinationSummaryDecisions)
            .replace("{count}", &projection.decisions.len().to_string()),
        tr(locale, MessageId::CoordinationSummaryContentions)
            .replace("{count}", &projection.contentions.len().to_string()),
        tr(locale, MessageId::CoordinationSummaryReconciled)
            .replace("{count}", &projection.reconciliations.len().to_string()),
    ]
    .join(" · ")
}

#[must_use]
pub(crate) fn needs_attention(projection: &CoordinationDetailProjection) -> bool {
    !projection.process_lock_held
        || projection
            .decisions
            .iter()
            .any(|decision| decision.status == DecisionStatus::Proposed)
        || projection
            .reconciliations
            .iter()
            .any(|receipt| receipt.verification_outcome != "verified")
        || projection
            .contentions
            .iter()
            .any(|contention| contention.disposition.blocks_admission())
}

/// Format the durable coordination projection for the shared Work pager.
///
/// Deliberately omitted: decision constraints and general evidence handles.
/// Those fields inform delegated prompts and headless inspection, but they can
/// contain operator-authored detail that does not belong in ambient TUI chrome.
#[must_use]
pub(crate) fn format(locale: Locale, projection: &CoordinationDetailProjection) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} · {} · {}",
        tr(locale, MessageId::CoordinationSchema)
            .replace("{value}", &projection.schema_version.to_string()),
        tr(locale, MessageId::CoordinationSequence)
            .replace("{value}", &projection.sequence.to_string()),
        tr(locale, MessageId::CoordinationPerSectionLimit)
            .replace("{limit}", &projection.limit.to_string())
    );
    if !projection.process_lock_held {
        let note = projection
            .process_lock_note
            .as_deref()
            .unwrap_or("another Codewhale process owns this workspace lock");
        let _ = writeln!(out, "Coordination lock: unavailable — {note}");
    }

    section(
        &mut out,
        tr(locale, MessageId::CoordinationDecisionsHeading).as_ref(),
    );
    if projection.decisions.is_empty() {
        let _ = writeln!(out, "{}", tr(locale, MessageId::CoordinationNone));
    } else {
        for decision in &projection.decisions {
            let status = tr(locale, MessageId::CoordinationStatus).replace(
                "{status}",
                decision_status(locale, decision.status).as_ref(),
            );
            let owner =
                tr(locale, MessageId::CoordinationOwner).replace("{owner}", &decision.owner);
            let version = tr(locale, MessageId::CoordinationVersion)
                .replace("{version}", &decision.version.to_string());
            let _ = writeln!(
                out,
                "{} · {}\n  {} · {} · {}",
                decision.decision_id, decision.subject, status, owner, version
            );
        }
    }

    section(
        &mut out,
        tr(locale, MessageId::CoordinationWriteClaimsHeading).as_ref(),
    );
    if projection.write_claims.is_empty() {
        let _ = writeln!(out, "{}", tr(locale, MessageId::CoordinationNone));
    } else {
        for receipt in &projection.write_claims {
            let claim = &receipt.claim;
            let workspace = if receipt.isolated_worktree {
                tr(locale, MessageId::CoordinationIsolated)
            } else {
                tr(locale, MessageId::CoordinationSharedWorkspace)
            };
            let _ = writeln!(
                out,
                "{} · {}\n  {}\n  {}",
                claim.owner,
                workspace,
                tr(locale, MessageId::CoordinationPaths).replace(
                    "{paths}",
                    &joined_paths(locale, &claim.roots, &claim.exact_files)
                ),
                tr(locale, MessageId::CoordinationContracts)
                    .replace("{contracts}", &joined_or_none(locale, &claim.contracts))
            );
        }
    }

    section(
        &mut out,
        tr(locale, MessageId::CoordinationContentionsHeading).as_ref(),
    );
    if projection.contentions.is_empty() {
        let _ = writeln!(out, "{}", tr(locale, MessageId::CoordinationNone));
    } else {
        for receipt in &projection.contentions {
            let claimant = tr(locale, MessageId::CoordinationClaimant)
                .replace("{claimant}", &receipt.claimant);
            let owner = tr(locale, MessageId::CoordinationOwner)
                .replace("{owner}", &receipt.conflicting_owner);
            let _ = writeln!(
                out,
                "{} · {}\n  {}\n  {}\n  {}",
                claimant,
                owner,
                tr(locale, MessageId::CoordinationPaths).replace(
                    "{paths}",
                    &joined_paths(locale, &receipt.roots, &receipt.exact_files)
                ),
                tr(locale, MessageId::CoordinationContracts)
                    .replace("{contracts}", &joined_or_none(locale, &receipt.contracts)),
                tr(locale, MessageId::CoordinationDisposition)
                    .replace("{disposition}", receipt.disposition.as_str())
            );
        }
    }

    section(
        &mut out,
        tr(locale, MessageId::CoordinationNeutralReconciliationHeading).as_ref(),
    );
    if projection.reconciliations.is_empty() {
        let _ = writeln!(out, "{}", tr(locale, MessageId::CoordinationNone));
    } else {
        for receipt in &projection.reconciliations {
            format_reconciliation(&mut out, locale, receipt);
        }
    }

    section(
        &mut out,
        tr(locale, MessageId::CoordinationContextProjectionsHeading).as_ref(),
    );
    if projection.context_projections.is_empty() {
        let _ = writeln!(out, "{}", tr(locale, MessageId::CoordinationNone));
    } else {
        for receipt in &projection.context_projections {
            let decisions = tr(locale, MessageId::CoordinationContextDecisions).replace(
                "{decisions}",
                &joined_or_none(locale, &receipt.decision_ids),
            );
            let bytes = tr(locale, MessageId::CoordinationBytes)
                .replace("{count}", &receipt.projected_bytes.to_string());
            let deduplicated = tr(locale, MessageId::CoordinationDeduplicated)
                .replace("{count}", &receipt.deduplicated.to_string());
            let omitted = tr(locale, MessageId::CoordinationOmitted)
                .replace("{count}", &receipt.omitted.to_string());
            let _ = writeln!(
                out,
                "{} · {} · {} · {} · {}",
                receipt.child_id, decisions, bytes, deduplicated, omitted
            );
        }
    }

    section(
        &mut out,
        tr(locale, MessageId::CoordinationActiveHotPathsHeading).as_ref(),
    );
    if projection.metrics.hottest_paths.is_empty() {
        let _ = writeln!(out, "{}", tr(locale, MessageId::CoordinationNone));
    } else {
        for path in &projection.metrics.hottest_paths {
            let active_claims = tr(locale, MessageId::CoordinationActiveClaims)
                .replace("{count}", &path.active_claims.to_string());
            let _ = writeln!(out, "{} · {}", path.path, active_claims);
        }
    }
    section(
        &mut out,
        tr(locale, MessageId::CoordinationMetricsNoteHeading).as_ref(),
    );
    // The headless projection retains the exact typed metrics note. Ambient
    // TUI chrome owns a localized explanation of the same current invariant.
    let _ = writeln!(
        out,
        "{}",
        tr(locale, MessageId::CoordinationMetricsNoAuthoritativeSource)
    );

    out.trim_end().to_string()
}

fn section(out: &mut String, label: &str) {
    let _ = write!(out, "\n{label}\n");
}

fn format_reconciliation(out: &mut String, locale: Locale, receipt: &ReconciliationReceipt) {
    let candidates = tr(locale, MessageId::CoordinationCandidates)
        .replace("{count}", &receipt.candidate_handles.len().to_string());
    let retry = tr(locale, MessageId::CoordinationRetry)
        .replace("{count}", &receipt.retry_count.to_string())
        .replace("{limit}", &receipt.retry_limit.to_string());
    let _ = writeln!(
        out,
        "{} · {} · {}\n  {}\n  {}\n  {}\n  {}",
        receipt.subject,
        candidates,
        retry,
        tr(locale, MessageId::CoordinationOwner).replace("{owner}", &receipt.owner),
        tr(locale, MessageId::CoordinationReviewer).replace(
            "{reviewer}",
            &joined_or_none(locale, &receipt.reviewer_evidence_handles)
        ),
        tr(locale, MessageId::CoordinationVerifier).replace(
            "{verifier}",
            &joined_or_none(locale, &receipt.verifier_evidence_handles)
        ),
        tr(locale, MessageId::CoordinationVerification)
            .replace("{verification}", &receipt.verification_outcome)
    );
}

fn decision_status(locale: Locale, status: DecisionStatus) -> std::borrow::Cow<'static, str> {
    match status {
        DecisionStatus::Proposed => tr(locale, MessageId::CoordinationStatusProposed),
        DecisionStatus::Accepted => tr(locale, MessageId::CoordinationStatusAccepted),
        DecisionStatus::Superseded => tr(locale, MessageId::CoordinationStatusSuperseded),
    }
}

fn joined_paths(locale: Locale, roots: &[String], exact_files: &[String]) -> String {
    let values = roots
        .iter()
        .chain(exact_files)
        .map(String::as_str)
        .collect::<Vec<_>>();
    if values.is_empty() {
        tr(locale, MessageId::CoordinationNoneValue).into_owned()
    } else {
        values.join(", ")
    }
}

fn joined_or_none(locale: Locale, values: &[String]) -> String {
    if values.is_empty() {
        tr(locale, MessageId::CoordinationNoneValue).into_owned()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use codewhale_engine::tools::subagent::coord::{
        ContextProjectionReceipt, CoordinationDetailMetrics, CoordinationHotPath, DecisionRecord,
        PersistedWriteClaim, WriteContentionDisposition, WriteContentionReceipt, WriteScopeClaim,
    };

    fn projection() -> CoordinationDetailProjection {
        CoordinationDetailProjection {
            schema_version: 1,
            sequence: 9,
            decisions: vec![DecisionRecord {
                decision_id: "decision-ui".to_string(),
                subject: "composer edges".to_string(),
                status: DecisionStatus::Accepted,
                owner: "planner".to_string(),
                scope: vec!["path:crates/tui".to_string()],
                constraints: vec!["PRIVATE-TRANSCRIPT-MARKER".to_string()],
                evidence_handles: vec!["artifact:hidden-evidence".to_string()],
                version: 3,
                sequence: 1,
            }],
            write_claims: vec![PersistedWriteClaim {
                claim: WriteScopeClaim {
                    owner: "worker-a".to_string(),
                    roots: vec!["crates/tui".to_string()],
                    exact_files: vec!["Cargo.toml".to_string()],
                    contracts: vec!["ui-contract".to_string()],
                },
                sequence: 2,
                isolated_worktree: false,
                present_at_claim: Vec::new(),
            }],
            reconciliations: vec![ReconciliationReceipt {
                reconciliation_id: "reconcile-ui".to_string(),
                subject: "composer edges".to_string(),
                owner: "release-owner".to_string(),
                input_decisions: vec!["decision-a".to_string(), "decision-b".to_string()],
                outcome: "candidate-a".to_string(),
                evidence_handles: Vec::new(),
                candidate_handles: vec!["branch:a".to_string(), "branch:b".to_string()],
                retry_count: 1,
                retry_limit: 3,
                reviewer_evidence_handles: vec!["agent:reviewer".to_string()],
                verifier_evidence_handles: vec!["agent:verifier".to_string()],
                verification_outcome: "verified".to_string(),
                sequence: 3,
            }],
            context_projections: vec![ContextProjectionReceipt {
                child_id: "worker-a".to_string(),
                decision_ids: vec!["decision-ui".to_string()],
                projected_bytes: 128,
                deduplicated: 2,
                omitted: 1,
                sequence: 4,
            }],
            contentions: vec![WriteContentionReceipt {
                claimant: "worker-b".to_string(),
                conflicting_owner: "worker-a".to_string(),
                roots: vec!["crates/tui".to_string()],
                exact_files: vec!["Cargo.toml".to_string()],
                contracts: vec!["ui-contract".to_string()],
                disposition: WriteContentionDisposition::BlockedPendingIsolationOrSerialization,
                resolution_sequence: None,
                sequence: 5,
            }],
            metrics: CoordinationDetailMetrics {
                hottest_paths: vec![CoordinationHotPath {
                    path: "crates/tui".to_string(),
                    active_claims: 2,
                }],
                package_or_module_growth: Some(json!({"ignored": true})),
                route_or_cost: None,
                note: "Only active owners contribute to hot paths".to_string(),
            },
            bounded: true,
            limit: 24,
            process_lock_held: true,
            process_lock_note: None,
        }
    }

    #[test]
    fn formatter_uses_typed_receipts_without_transcript_shaped_fields() {
        let text = format(Locale::En, &projection());
        for required in [
            "Schema 1 · sequence 9 · up to 24 per section",
            "decision-ui · composer edges",
            "status accepted · owner planner · version 3",
            "claimant worker-b · owner worker-a",
            "paths crates/tui, Cargo.toml",
            "contracts ui-contract",
            "disposition blocked_pending_isolation_or_serialization",
            "composer edges · 2 candidates · retry 1/3",
            "reviewer agent:reviewer",
            "verifier agent:verifier",
            "verification verified",
            "worker-a · decisions decision-ui · 128 bytes · 2 deduplicated · 1 omitted",
            "crates/tui · 2 active claims",
        ] {
            assert!(text.contains(required), "missing {required}:\n{text}");
        }
        assert!(!text.contains("PRIVATE-TRANSCRIPT-MARKER"), "{text}");
        assert!(!text.contains("bounded to 24 records"), "{text}");
        assert!(!text.contains("hidden-evidence"), "{text}");
        assert!(!text.contains("ignored"), "{text}");
    }

    #[test]
    fn complete_locale_packs_translate_chrome_and_preserve_receipt_values() {
        let value = projection();
        let english = format(Locale::En, &value);
        for locale in Locale::shipped_complete() {
            let text = format(*locale, &value);
            let summary = summary(*locale, &value);
            for literal in [
                "decision-ui",
                "composer edges",
                "worker-a",
                "blocked_pending_isolation_or_serialization",
                "agent:reviewer",
                "agent:verifier",
                "crates/tui",
            ] {
                assert!(text.contains(literal), "{locale:?} lost {literal}:\n{text}");
            }
            assert!(
                !text.contains('{'),
                "{locale:?} has a raw placeholder:\n{text}"
            );
            assert!(
                !summary.contains('{'),
                "{locale:?} summary has a raw placeholder: {summary}"
            );
            if *locale != Locale::En {
                assert_ne!(text, english, "{locale:?} fell back to English");
            }
        }
    }

    #[test]
    fn proposed_decisions_and_unverified_reconciliation_need_attention() {
        let mut value = projection();
        value.contentions.clear();
        assert!(!needs_attention(&value));
        value.decisions[0].status = DecisionStatus::Proposed;
        assert!(needs_attention(&value));
        value.decisions[0].status = DecisionStatus::Accepted;
        value.reconciliations[0].verification_outcome = "blocked".to_string();
        assert!(needs_attention(&value));
    }

    #[test]
    fn blocked_contention_attention_uses_durable_resolution_state() {
        let mut value = projection();
        assert!(needs_attention(&value));

        value.contentions[0].disposition = WriteContentionDisposition::ResolvedBySuccessfulClaim;
        value.contentions[0].resolution_sequence = Some(6);
        assert!(!needs_attention(&value));

        let text = format(Locale::En, &value);
        assert!(
            text.contains("disposition resolved_by_successful_claim"),
            "{text}"
        );
        assert!(!text.contains("disposition blocked_pending"), "{text}");
    }
}
