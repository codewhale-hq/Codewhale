//! Shared, host-bound test support for the FEAT-025 session-export slice.
//!
//! Lives at the `commands` root — outside `groups/session`, which FEAT-043
//! moves into `codewhale-commands` — so the public-surface and host-regression
//! suites reuse one implementation instead of drifting copies.

use codewhale_command_contract::handler::ContextParts;

use std::sync::OnceLock;

use regex::Regex;

/// Replace the host-derived `- Exported:` timestamp value so two exports of
/// identical state compare byte-for-byte. Production timestamp semantics stay
/// untouched (D3/observable-behavior rule: host metadata derivation is
/// preserved; tests control comparison, not the adapter).
pub(crate) fn normalize_export_time(markdown: &str) -> String {
    let mut normalized = String::with_capacity(markdown.len());
    for line in markdown.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("- Exported: ") {
            let newline = if rest.ends_with('\n') { "\n" } else { "" };
            normalized.push_str("- Exported: <time>");
            normalized.push_str(newline);
        } else {
            normalized.push_str(line);
        }
    }
    normalized
}

/// Replace the turn-handoff wall-clock generation stamp so a captured golden
/// compares byte-for-byte.
///
/// `turn_handoff_markdown` stamps the header with `generated <now>`; the
/// renderer itself is TUI-owned and deliberately not migrated (D2), so tests
/// normalise the stamp instead of changing production timestamp semantics.
pub(crate) fn normalize_turn_generated_at(markdown: &str) -> String {
    let mut normalized = String::with_capacity(markdown.len());
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        let newline = &line[trimmed.len()..];
        if let Some(head) = trimmed.strip_prefix("_Status: ")
            && let Some((status, _)) = head.split_once(" \u{b7} generated ")
        {
            normalized.push_str("_Status: ");
            normalized.push_str(status);
            normalized.push_str(" \u{b7} generated <timestamp>_");
            normalized.push_str(newline);
            continue;
        }
        normalized.push_str(line);
    }
    normalized
}

/// Normalise the two volatile fields a recorded-restore-point export carries:
/// the snapshot id (a git SHA over a commit whose date is wall-clock) and the
/// `Recorded (UTC)` table cell.
///
/// Snapshot commits are created with `git commit-tree` and no pinned
/// author/committer date, so both the SHA and the timestamp change on every run.
/// Everything else in the document - the table structure, the 12-character id
/// truncation, the correlation wording, the ambiguity warning, and the
/// no-match line - is compared verbatim.
pub(crate) fn normalize_snapshot_identity(markdown: &str) -> String {
    fn snapshot_id_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"`[0-9a-f]{12}`").expect("snapshot-id regex"))
    }
    fn recorded_time_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"\| \d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z \|").expect("recorded-time regex")
        })
    }

    let with_ids = snapshot_id_regex().replace_all(markdown, "`<snapshot-id>`");
    recorded_time_regex()
        .replace_all(&with_ids, "| <recorded> |")
        .into_owned()
}

/// Normalisation for a document captured from the recorded-restore-point state:
/// the export stamp plus the snapshot identity.
pub(crate) fn normalize_recorded_export(markdown: &str) -> String {
    normalize_snapshot_identity(&normalize_export_time(markdown))
}

/// Assert that an envelope built from exactly `SESSION_EXPORT` exposes the
/// export facet and no other facet.
///
/// The exhaustive destructuring is deliberate. `ContextParts` is not
/// `#[non_exhaustive]`, so adding a facet fails to compile here until the new
/// slot is classified, and every slot is asserted absent in one place. That
/// turns the "exposure of no unrelated facet" acceptance criterion into a
/// structural guarantee rather than a spot-check of a chosen few fields.
pub(crate) fn assert_only_export_facet_exposed(parts: ContextParts<'_>) {
    let ContextParts {
        session,
        model,
        cost,
        mode_policy,
        system_prompt,
        skills,
        workspace,
        presentation,
        media,
        memory,
        project,
        skill_group,
        plugin,
        lifecycle,
        control,
        export,
        structcopy,
        debug_receipts,
        debug_change,
        debug_history,
        debug_diff,
        debug_undo,
        debug_diagnostics,
        permissions,
        config_status,
        config,
        pet,
        trust,
        login,
        work_surface,
        navigation,
        voice_input,
    } = parts;
    assert!(
        permissions.is_none()
            && config_status.is_none()
            && config.is_none()
            && pet.is_none()
            && trust.is_none()
            && login.is_none()
            && work_surface.is_none()
            && navigation.is_none()
            && voice_input.is_none()
    );

    assert!(export.is_some(), "the export facet must be exposed");

    let unrelated = [
        ("session", session.is_some()),
        ("model", model.is_some()),
        ("cost", cost.is_some()),
        ("mode_policy", mode_policy.is_some()),
        ("system_prompt", system_prompt.is_some()),
        ("skills", skills.is_some()),
        ("workspace", workspace.is_some()),
        ("presentation", presentation.is_some()),
        ("media", media.is_some()),
        ("memory", memory.is_some()),
        ("project", project.is_some()),
        ("skill_group", skill_group.is_some()),
        ("plugin", plugin.is_some()),
        ("lifecycle", lifecycle.is_some()),
        ("control", control.is_some()),
        ("structcopy", structcopy.is_some()),
        ("debug_receipts", debug_receipts.is_some()),
        ("debug_change", debug_change.is_some()),
        ("debug_history", debug_history.is_some()),
        ("debug_diff", debug_diff.is_some()),
        ("debug_undo", debug_undo.is_some()),
        ("debug_diagnostics", debug_diagnostics.is_some()),
    ];
    for (facet, present) in unrelated {
        assert!(
            !present,
            "{facet} must stay unavailable to a SESSION_EXPORT-only envelope"
        );
    }
}

/// The recorded-golden normaliser must replace *only* the wall-clock fields; a
/// normaliser that over-matched would turn the golden comparison into a
/// tautology, so pin its exact behaviour.
#[test]
fn recorded_golden_normalisers_replace_only_volatile_fields() {
    let doc = concat!(
        "- Exported: 2026-09-11T14:39:58Z\n",
        "\n",
        "| 1 | `3d0ad76f0222` | 2026-09-11T14:39:58Z | pre-turn:3: Fix the login test |\n",
        "- Restore points: N1 `3d0ad76f0222` (pre-turn turn 3)\n",
        "| 2 | `6a3bc0698866` | 2026-09-11T14:39:58Z | tool:call-1 |\n",
    );

    let out = normalize_recorded_export(doc);

    assert!(out.contains("- Exported: <time>\n"), "{out}");
    assert!(
        out.contains("| 1 | `<snapshot-id>` | <recorded> | pre-turn:3: Fix the login test |"),
        "{out}"
    );
    assert!(
        out.contains("- Restore points: N1 `<snapshot-id>` (pre-turn turn 3)"),
        "{out}"
    );
    assert!(
        out.contains("| 2 | `<snapshot-id>` | <recorded> | tool:call-1 |"),
        "{out}"
    );
    // Nothing volatile survives, and nothing static was touched.
    assert!(!out.contains("3d0ad76f0222"), "{out}");
    assert!(!out.contains("6a3bc0698866"), "{out}");
    assert!(!out.contains("2026-09-11T14:39:58Z"), "{out}");
    assert!(out.contains("pre-turn:3: Fix the login test"), "{out}");
    assert!(out.contains("(pre-turn turn 3)"), "{out}");
}
