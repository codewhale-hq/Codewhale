use super::*;

use crate::agent_roster::{RosterState, build_agent_roster};
use crate::tools::subagent::{
    AgentRunUsage, AgentWorkerEvent, AgentWorkerRecord, AgentWorkerSpec, AgentWorkerStatus,
    AgentWorkerToolProfile,
};
use codewhale_agent_contract::role::FleetRole;
use codewhale_agent_contract::worker_profile::WorkerRuntimeProfile;

fn usage_none() -> AgentRunUsage {
    AgentRunUsage {
        status: "unavailable".to_string(),
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cost_microusd: None,
        note: "no route audit for this worker".to_string(),
    }
}

fn usage_of(input: u64, output: u64) -> AgentRunUsage {
    AgentRunUsage {
        status: "reported".to_string(),
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
        cost_microusd: Some(1_234),
        ..usage_none()
    }
}

/// Minimal record; each test overrides only the fields it is about.
fn record(worker_id: &str, created_at_ms: u64) -> AgentWorkerRecord {
    let spec = AgentWorkerSpec {
        worker_id: worker_id.to_string(),
        run_id: format!("run-{worker_id}"),
        parent_run_id: None,
        workflow_run_id: None,
        session_name: Some(worker_id.to_string()),
        objective: "do the thing".to_string(),
        role: None,
        agent_type: FleetRole::Scout,
        model: "deepseek-v4-pro".to_string(),
        workspace: std::path::PathBuf::from("/tmp/ws"),
        git_branch: None,
        context_mode: "fresh".to_string(),
        fork_context: false,
        tool_profile: AgentWorkerToolProfile::Inherited,
        runtime_profile: WorkerRuntimeProfile::for_role(FleetRole::Scout),
        max_steps: 20,
        spawn_depth: 1,
        max_spawn_depth: 3,
        child_route: None,
        launch_manifest: None,
    };
    let mut rec = AgentWorkerRecord::new(spec, created_at_ms);
    rec.status = AgentWorkerStatus::Running;
    rec.started_at_ms = Some(created_at_ms);
    rec.usage = usage_none();
    rec
}

// === The truth rule: absent is not zero ===

#[test]
fn a_worker_without_a_usage_receipt_reports_no_tokens_not_zero() {
    let rows = build_agent_roster(&[record("scout", 1_000)], 5_000);
    let row = &rows[0];
    assert_eq!(
        row.input_tokens, None,
        "a missing route audit must stay missing"
    );
    assert_eq!(row.output_tokens, None);
    assert_eq!(row.cost_microusd, None);

    let text = render_agent_roster(&rows, "main");
    assert!(
        text.contains('—'),
        "an absent receipt renders as an em dash, never as 0:\n{text}"
    );
    assert!(
        !text.contains("↓ 0") && !text.contains("↑ 0"),
        "no receipt must never be shown as a zero token count:\n{text}"
    );
}

#[test]
fn reported_usage_is_passed_through_untouched() {
    let mut rec = record("builder", 1_000);
    rec.usage = usage_of(96_300, 4_120);
    let rows = build_agent_roster(&[rec], 5_000);
    assert_eq!(rows[0].input_tokens, Some(96_300));
    assert_eq!(rows[0].output_tokens, Some(4_120));

    let text = render_agent_roster(&rows, "main");
    assert!(text.contains("↓ 96.3k"), "{text}");
    assert!(text.contains("↑ 4.1k"), "{text}");
}

#[test]
fn totals_are_absent_when_nothing_reported_and_never_summed_from_zeros() {
    let rows = build_agent_roster(&[record("a", 1), record("b", 2)], 10);
    assert_eq!(
        roster_totals(&rows),
        (None, None),
        "summing absent receipts into 0 restates the same lie per-row rendering forbids"
    );
    assert!(!all_rows_have_usage(&rows));

    let mut reported = record("c", 3);
    reported.usage = usage_of(10, 20);
    let mixed = build_agent_roster(&[record("a", 1), reported], 10);
    assert_eq!(
        roster_totals(&mixed),
        (Some(10), Some(20)),
        "a partial total reports only what was actually reported"
    );
    assert!(
        !all_rows_have_usage(&mixed),
        "callers must be able to label a partial total as partial"
    );
}

// === Time: elapsed while live, frozen when finished ===

#[test]
fn a_finished_agent_keeps_the_duration_it_finished_with() {
    let mut rec = record("done-worker", 1_000);
    rec.status = AgentWorkerStatus::Completed;
    rec.completed_at_ms = Some(4_500);

    let early = build_agent_roster(&[rec.clone()], 5_000);
    let much_later = build_agent_roster(&[rec], 900_000);
    assert_eq!(early[0].millis, Some(3_500));
    assert_eq!(
        much_later[0].millis,
        Some(3_500),
        "a finished row must not keep ticking as the session goes on"
    );
}

#[test]
fn a_live_agent_reports_elapsed_against_the_supplied_instant() {
    let rows = build_agent_roster(&[record("live", 1_000)], 210_000);
    assert_eq!(rows[0].millis, Some(209_000));
    let text = render_agent_roster(&rows, "main");
    assert!(text.contains("3m 29s"), "{text}");
}

#[test]
fn a_worker_that_never_started_has_no_duration_rather_than_zero() {
    let mut rec = record("queued", 1_000);
    rec.status = AgentWorkerStatus::Queued;
    rec.started_at_ms = None;
    let rows = build_agent_roster(&[rec], 9_000);
    assert_eq!(
        rows[0].millis, None,
        "reporting 0s would imply the worker had started"
    );
}

// === Activity line ===

#[test]
fn activity_prefers_the_newest_tool_event_and_stays_one_line() {
    let mut rec = record("worker", 1_000);
    rec.events.push_back(AgentWorkerEvent {
        seq: 1,
        worker_id: "worker".to_string(),
        status: AgentWorkerStatus::Running,
        timestamp_ms: 1_100,
        message: Some("starting".to_string()),
        step: Some(1),
        tool_name: Some("Grep".to_string()),
    });
    rec.events.push_back(AgentWorkerEvent {
        seq: 2,
        worker_id: "worker".to_string(),
        status: AgentWorkerStatus::RunningTool,
        timestamp_ms: 1_200,
        message: None,
        step: Some(4),
        tool_name: Some("Bash".to_string()),
    });
    let rows = build_agent_roster(&[rec], 2_000);
    assert_eq!(rows[0].activity.as_deref(), Some("step 4 · Bash"));
}

#[test]
fn a_multiline_message_is_flattened_and_bounded_to_one_row() {
    let mut rec = record("chatty", 1_000);
    rec.latest_message = Some(format!("line one\nline two\n{}", "x".repeat(300)));
    let rows = build_agent_roster(&[rec], 2_000);
    let activity = rows[0].activity.clone().expect("activity");
    assert!(!activity.contains('\n'), "a rail row is one row");
    assert!(activity.chars().count() <= 72, "{}", activity.len());
    assert!(activity.ends_with('…'), "truncation is visible: {activity}");
}

#[test]
fn a_worker_with_no_events_reports_no_activity() {
    let rows = build_agent_roster(&[record("silent", 1_000)], 2_000);
    assert_eq!(rows[0].activity, None);
    assert!(render_agent_roster(&rows, "main").contains('—'));
}

// === Ordering and workflow aggregation ===

#[test]
fn rows_are_ordered_oldest_first_so_the_rail_reads_as_history() {
    let rows = build_agent_roster(
        &[
            record("third", 3_000),
            record("first", 1_000),
            record("second", 2_000),
        ],
        9_000,
    );
    let names: Vec<&str> = rows.iter().map(|row| row.display_name.as_str()).collect();
    assert_eq!(names, vec!["first", "second", "third"]);
}

#[test]
fn a_workflow_parent_aggregates_its_children_as_n_of_m_done() {
    let parent = record("workflow", 1_000);
    let parent_run = parent.spec.run_id.clone();
    let mut children = Vec::new();
    for index in 0..6 {
        let mut child = record(&format!("child-{index}"), 2_000 + index as u64);
        child.parent_run_id = Some(parent_run.clone());
        child.spec.parent_run_id = Some(parent_run.clone());
        if index < 5 {
            child.status = AgentWorkerStatus::Completed;
            child.completed_at_ms = Some(3_000);
        }
        children.push(child);
    }
    let mut records = vec![parent];
    records.extend(children);
    let rows = build_agent_roster(&records, 9_000);

    let text = render_agent_roster(&rows, "main");
    assert!(
        text.contains("5/6 agents done"),
        "a workflow collapses to one progress line:\n{text}"
    );
    // Children are listed under the parent, indented, and exactly once.
    assert_eq!(
        text.matches("child-0").count(),
        1,
        "a child must not also appear at the top level:\n{text}"
    );
    assert!(text.contains("    ○ child-0"), "{text}");
}

#[test]
fn a_grandchild_is_rendered_under_its_parent_not_dropped() {
    let mut parent = record("workflow", 1_000);
    parent.status = AgentWorkerStatus::Running;
    let parent_run = parent.spec.run_id.clone();
    let mut child = record("builder", 2_000);
    child.parent_run_id = Some(parent_run.clone());
    child.spec.parent_run_id = Some(parent_run);
    child.status = AgentWorkerStatus::Running;
    let child_run = child.spec.run_id.clone();
    let mut grandchild = record("scout", 3_000);
    grandchild.parent_run_id = Some(child_run);
    grandchild.spec.parent_run_id = Some(child.spec.run_id.clone());
    grandchild.status = AgentWorkerStatus::Running;

    let rows = build_agent_roster(&[parent, child, grandchild], 9_000);
    let text = render_agent_roster(&rows, "main");
    assert!(
        text.contains("scout"),
        "a third-level agent must appear in /agents list:\n{text}"
    );
    assert_eq!(text.matches("scout").count(), 1, "{text}");
    assert!(
        text.contains("      "),
        "a grandchild should be indented under its parent:\n{text}"
    );
}

#[test]
fn an_orphaned_child_still_appears_when_its_parent_is_not_in_the_roster() {
    // The parent's record can age out of the ledger before the child's does.
    let mut child = record("orphan", 2_000);
    child.parent_run_id = Some("run-that-aged-out".to_string());
    let rows = build_agent_roster(&[child], 9_000);
    let text = render_agent_roster(&rows, "main");
    assert!(
        text.contains("orphan"),
        "a child whose parent is gone must not vanish from the roster:\n{text}"
    );
}

// === Rendering basics ===

#[test]
fn an_empty_roster_says_so_instead_of_rendering_an_empty_frame() {
    let text = render_agent_roster(&[], "main");
    assert!(text.contains("● main"));
    assert!(text.contains("No agents have run in this session yet"));
}

#[test]
fn every_state_has_a_distinct_single_width_glyph() {
    let states = [
        RosterState::Running,
        RosterState::Waiting,
        RosterState::Done,
        RosterState::Failed,
        RosterState::Cancelled,
    ];
    let glyphs: Vec<&str> = states.iter().map(|state| state.glyph()).collect();
    let unique: std::collections::HashSet<&&str> = glyphs.iter().collect();
    assert_eq!(unique.len(), glyphs.len(), "glyphs must be distinguishable");
    for glyph in glyphs {
        assert_eq!(glyph.chars().count(), 1, "{glyph} must occupy one cell");
    }
}

#[test]
fn interrupted_and_cancelled_both_read_as_cancelled_not_failed() {
    for status in [AgentWorkerStatus::Cancelled, AgentWorkerStatus::Interrupted] {
        let mut rec = record("stopped", 1_000);
        rec.status = status;
        rec.completed_at_ms = Some(2_000);
        let rows = build_agent_roster(&[rec], 9_000);
        assert_eq!(
            rows[0].state,
            RosterState::Cancelled,
            "{status:?} is a stop, not a failure — a red ✗ would misreport it"
        );
    }
}

#[test]
fn formatters_never_round_a_real_count_down_to_nothing() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(1), "1");
    assert_eq!(format_tokens(999), "999");
    assert_eq!(format_tokens(1_000), "1.0k");
    assert_eq!(format_tokens(96_300), "96.3k");
    assert_eq!(format_tokens(1_200_000), "1.2M");

    assert_eq!(format_duration(450), "450ms");
    assert_eq!(format_duration(12_000), "12s");
    assert_eq!(format_duration(209_000), "3m 29s");
    assert_eq!(format_duration(3_600_000 + 120_000), "1h 2m");
}

#[test]
fn a_partial_totals_line_says_it_is_partial() {
    let mut reported = record("builder", 2_000);
    reported.usage = usage_of(96_300, 4_120);
    let rows = build_agent_roster(&[record("scout", 1_000), reported], 9_000);
    let text = render_agent_roster(&rows, "main");
    assert!(
        text.contains("receipts from 1 of 2 agents"),
        "a bare total silently implies it covers every agent listed:\n{text}"
    );

    let mut both = record("second", 3_000);
    both.usage = usage_of(1_000, 500);
    let mut first = record("first", 1_000);
    first.usage = usage_of(2_000, 500);
    let full = build_agent_roster(&[first, both], 9_000);
    let full_text = render_agent_roster(&full, "main");
    assert!(full_text.contains("↓ 3.0k"), "{full_text}");
    assert!(
        !full_text.contains("receipts from"),
        "a complete total needs no caveat:\n{full_text}"
    );
}

#[test]
fn a_roster_with_no_receipts_at_all_says_so_rather_than_showing_zeros() {
    let rows = build_agent_roster(&[record("a", 1_000), record("b", 2_000)], 9_000);
    let text = render_agent_roster(&rows, "main");
    assert!(
        text.contains("2 agents · no usage receipts recorded"),
        "{text}"
    );
    assert!(!text.contains("↓ 0"), "{text}");
}

/// Renders the whole thing once so the shape is reviewable in one place.
#[test]
fn rendered_roster_shape() {
    let parent = record("refactor-workflow", 1_000);
    let parent_run = parent.spec.run_id.clone();
    let mut scout = record("scout", 2_000);
    scout.parent_run_id = Some(parent_run.clone());
    scout.status = AgentWorkerStatus::Completed;
    scout.completed_at_ms = Some(120_000);
    scout.usage = usage_of(96_300, 4_120);
    let mut builder = record("builder", 3_000);
    builder.parent_run_id = Some(parent_run);
    builder.events.push_back(AgentWorkerEvent {
        seq: 1,
        worker_id: "builder".to_string(),
        status: AgentWorkerStatus::RunningTool,
        timestamp_ms: 4_000,
        message: None,
        step: Some(7),
        tool_name: Some("apply_patch".to_string()),
    });
    builder.usage = usage_of(210_400, 18_900);

    let rows = build_agent_roster(&[parent, scout, builder], 212_000);
    let text = render_agent_roster(&rows, "main");
    assert!(text.contains("1/2 agents done"), "{text}");
    assert!(text.contains("step 7 · apply_patch"), "{text}");
    assert!(text.contains("↓ 306.7k"), "{text}");
}

// === #5906: a parked husk is not an agent waiting for input ===

/// A child parked at the parent's turn end settles as `WaitingForUser` like a
/// child that really asked a question. Only `parked_at_turn_end` separates
/// them, and the rail has to read it or it prints the same row for both.
#[test]
fn a_parked_record_is_its_own_roster_state_not_waiting() {
    let mut parked = record("parked_child", 1_000);
    parked.status = AgentWorkerStatus::WaitingForUser;
    parked.parked_at_turn_end = true;

    let mut asked = record("asking_child", 2_000);
    asked.status = AgentWorkerStatus::WaitingForUser;

    let rows = build_agent_roster(&[parked, asked], 5_000);
    let parked_row = rows
        .iter()
        .find(|row| row.worker_id == "parked_child")
        .expect("parked row");
    let asked_row = rows
        .iter()
        .find(|row| row.worker_id == "asking_child")
        .expect("asking row");

    assert_eq!(parked_row.state, RosterState::Parked);
    assert_eq!(
        asked_row.state,
        RosterState::Waiting,
        "a child that genuinely asked must keep the answerable state"
    );
    assert_ne!(parked_row.state.glyph(), asked_row.state.glyph());
    assert!(
        !parked_row.state.is_terminal(),
        "parked work is unfinished, not settled-done"
    );
}

/// The rail is otherwise oldest-first, but a parked husk is exactly the row an
/// operator must not have to scan past to find live work.
#[test]
fn parked_husks_sort_below_live_and_waiting_agents() {
    let mut parked = record("aaa_parked", 1_000);
    parked.status = AgentWorkerStatus::WaitingForUser;
    parked.parked_at_turn_end = true;

    let running = record("bbb_running", 2_000);

    let mut waiting = record("ccc_waiting", 3_000);
    waiting.status = AgentWorkerStatus::WaitingForUser;

    let rows = build_agent_roster(&[parked, running, waiting], 9_000);
    let order: Vec<&str> = rows.iter().map(|row| row.worker_id.as_str()).collect();
    assert_eq!(
        order,
        vec!["bbb_running", "ccc_waiting", "aaa_parked"],
        "the parked husk sinks below live and answerable rows despite being oldest"
    );
}

/// Re-dispatch clears the flag on the record (#5921); the rail must follow it
/// back rather than latching a row as parked forever.
#[test]
fn a_re_dispatched_record_stops_reading_as_parked() {
    let mut rec = record("resumed", 1_000);
    rec.status = AgentWorkerStatus::WaitingForUser;
    rec.parked_at_turn_end = true;
    assert_eq!(
        build_agent_roster(&[rec.clone()], 5_000)[0].state,
        RosterState::Parked
    );

    rec.parked_at_turn_end = false;
    rec.status = AgentWorkerStatus::RunningTool;
    assert_eq!(
        build_agent_roster(&[rec], 5_000)[0].state,
        RosterState::Running
    );
}

#[test]
fn result_headline_skips_scaffolding_and_returns_the_first_sentence() {
    use crate::agent_roster::result_headline;
    // #6565: the first line of a report is usually a heading or a rule, and
    // showing it hid the answer.
    let report = "\n## Summary\n\n---\n```text\nThe docs audit found 3 stale links. Two are in README.md.\n```\n<codewhale:subagent.done>{\"event\":\"subagent.completed\"}</codewhale:subagent.done>";
    assert_eq!(
        result_headline(report).as_deref(),
        Some("The docs audit found 3 stale links.")
    );
    // A dotted version or file name inside the sentence does not end it.
    assert_eq!(
        result_headline("Bumped to v0.10.1 in Cargo.toml and tagged it. Done.").as_deref(),
        Some("Bumped to v0.10.1 in Cargo.toml and tagged it.")
    );
    assert_eq!(
        result_headline("修复了两个问题。其余正常。").as_deref(),
        Some("修复了两个问题。")
    );
    // Only headings: the first one, without its marks.
    assert_eq!(
        result_headline("# Only a heading\n## Another").as_deref(),
        Some("Only a heading")
    );
    assert_eq!(result_headline(""), None);
    assert_eq!(
        result_headline("  \n***\n<codewhale:subagent.done>{}</codewhale:subagent.done>"),
        None
    );
}

#[test]
fn a_finished_worker_shows_its_result_not_its_last_tool() {
    // #6565: the roster said `step 6 · read_file` for an agent that had
    // already finished and reported. A settled row leads with the outcome.
    let mut rec = record("auditor", 1_000);
    rec.events.push_back(AgentWorkerEvent {
        seq: 1,
        worker_id: "auditor".to_string(),
        status: AgentWorkerStatus::RunningTool,
        timestamp_ms: 1_200,
        message: None,
        step: Some(6),
        tool_name: Some("read_file".to_string()),
    });
    let long_tail = "detail ".repeat(400);
    rec.status = AgentWorkerStatus::Completed;
    rec.completed_at_ms = Some(1_900);
    rec.result_summary = Some(format!(
        "## Summary\n\nThe docs audit found 3 stale links.\n\n{long_tail}\n<codewhale:subagent.done>{{}}</codewhale:subagent.done>"
    ));
    let rows = build_agent_roster(&[rec.clone()], 2_000);
    assert_eq!(
        rows[0].activity.as_deref(),
        Some("The docs audit found 3 stale links.")
    );
    // The row keeps the whole answer for the focused view.
    assert!(
        rows[0]
            .outcome
            .as_deref()
            .is_some_and(|outcome| outcome.contains(long_tail.trim_end()))
    );

    // Failed leads with the error; a live worker still shows its step and
    // carries no outcome.
    rec.status = AgentWorkerStatus::Failed;
    rec.error = Some("provider returned 429".to_string());
    let rows = build_agent_roster(&[rec.clone()], 2_000);
    assert_eq!(rows[0].activity.as_deref(), Some("provider returned 429"));
    assert_eq!(rows[0].outcome.as_deref(), Some("provider returned 429"));

    rec.status = AgentWorkerStatus::RunningTool;
    let rows = build_agent_roster(&[rec], 2_000);
    assert_eq!(rows[0].activity.as_deref(), Some("step 6 · read_file"));
    assert_eq!(rows[0].outcome, None);
}

#[test]
fn a_finished_worker_without_a_result_falls_back_to_its_last_step() {
    let mut rec = record("quiet", 1_000);
    rec.events.push_back(AgentWorkerEvent {
        seq: 1,
        worker_id: "quiet".to_string(),
        status: AgentWorkerStatus::RunningTool,
        timestamp_ms: 1_200,
        message: None,
        step: Some(2),
        tool_name: Some("Grep".to_string()),
    });
    rec.status = AgentWorkerStatus::Completed;
    rec.result_summary = Some("   \n".to_string());
    let rows = build_agent_roster(&[rec], 2_000);
    assert_eq!(rows[0].activity.as_deref(), Some("step 2 · Grep"));
    assert_eq!(rows[0].outcome, None);
}
