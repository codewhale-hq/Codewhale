//! FEAT-023 Phase 4/6: portable lifecycle handler tests (Tasks 4.2/4.4/4.6).
//!
//! Deterministic composition tests: canned lifecycle outcomes drive each
//! portable handler and the exact baseline messages/actions are asserted
//! byte-for-byte. The public dispatch seam integration (real bundle) is
//! exercised in Phase 6 and the end-to-end parity matrix in Phase 7.

use codewhale_command_contract::facets::{
    SessionArchiveReceipt, SessionBranchOutcome, SessionForkFromReceipt, SessionForkReceipt,
    SessionNewReceipt, SessionSaveReceipt, TreeBodyProjection,
};
use codewhale_command_contract::handler::CommandContexts;
use std::path::PathBuf;

use codewhale_command_contract::outcome::SessionAction;

use super::lifecycle_test_support::{CannedLifecycle, sync_payload};

fn missing_lifecycle() -> CommandContexts<'static> {
    CommandContexts::empty()
}

// ---- /branch (Task 4.5) ----

#[test]
fn every_contextual_lifecycle_handler_fails_safely_without_its_facet() {
    type Handler = fn(CommandContexts<'_>, Option<&str>) -> super::CommandResult;
    let handlers: [(&str, Handler, Option<&str>); 7] = [
        ("branch", super::branch::branch_contextual, Some("entry-1")),
        ("fork", super::fork::fork_contextual, Some("session-1")),
        ("load", super::load::load_contextual, Some("session.json")),
        ("new", super::new::new_contextual, None),
        ("save", super::save::save_contextual, None),
        ("sessions", super::sessions::sessions_contextual, None),
        ("tree", super::tree::tree_contextual, None),
    ];

    for (name, handler, arg) in handlers {
        let result = handler(missing_lifecycle(), arg);
        assert!(result.is_error, "/{name}: {result:?}");
        assert_eq!(
            result.message.as_deref(),
            Some("Error: Command capability unavailable: session_lifecycle"),
            "/{name}"
        );
        assert!(result.action.is_none(), "/{name}");
    }
}

#[test]
fn branch_composes_exact_baseline_messages() {
    // Blocked transition first.
    let mut canned = CannedLifecycle {
        blocked: true,
        blockers: vec!["shell_a3f2  running  5h 18m  cw-leftovers.ps1".to_string()],
        ..CannedLifecycle::default()
    };
    let result = super::branch::branch_portable(&mut canned, Some("entry-1"));
    assert_eq!(
        result.message.as_deref(),
        Some(
            "Error: Cannot branch while runtime work is active:\n  • shell_a3f2  running  5h 18m  cw-leftovers.ps1\n\nWait for the work to finish, or stop what you can: Ctrl+C stops a running turn, and /jobs cancel-all cancels running shell jobs."
        )
    );
    assert_eq!(canned.transition_checks.get(), 1);
    assert!(canned.branch_entries.is_empty());

    // No-arg with an active leaf hint.
    let mut canned = CannedLifecycle {
        leaf_hint: Some("entry-7".to_string()),
        ..CannedLifecycle::default()
    };
    let result = super::branch::branch_portable(&mut canned, None);
    assert_eq!(
        result.message.as_deref(),
        Some(
            "Current leaf: entry-7\nUse `/branch <entry_id>` to move the leaf (history is never rewritten).\nUse `/tree` to list entry ids."
        )
    );

    // No-arg without a leaf -> usage fallback.
    let mut canned = CannedLifecycle::default();
    let result = super::branch::branch_portable(&mut canned, None);
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("Usage: /branch <entry_id>")),
        "{result:?}"
    );

    // Success message uses deterministic receipt fields.
    let mut canned = CannedLifecycle {
        branch: Ok(SessionBranchOutcome {
            leaf_display: "entry-3".to_string(),
            journal_entries_before: 5,
            sync: sync_payload("branched-session"),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::branch::branch_portable(&mut canned, Some("entry-3"));
    assert_eq!(
        result.message.as_deref(),
        Some(
            "Branched to entry entry-3 (leaf now entry-3); journal entries 5 (history preserved, leaf moved only)"
        )
    );
    assert!(
        matches!(result.action, Some(SessionAction::SyncSession(codewhale_command_contract::facets::SessionSyncPayload { session_id: Some(ref id), .. })) if id == "branched-session")
    );
    assert_eq!(canned.branch_entries, ["entry-3"]);

    // Host stage error passes through unchanged.
    let mut canned = CannedLifecycle {
        branch: Err("could not load session x: boom".to_string()),
        ..CannedLifecycle::default()
    };
    let result = super::branch::branch_portable(&mut canned, Some("x"));
    assert!(result.is_error);
    assert_eq!(
        result.message.as_deref(),
        Some("Error: could not load session x: boom")
    );
}

// ---- /fork (Task 4.3) ----

#[test]
fn fork_composes_exact_baseline_messages_and_actions() {
    // Picker aliases push the picker and return the baseline message.
    let mut canned = CannedLifecycle::default();
    let result = super::fork::fork_portable(&mut canned, Some("picker"));
    assert_eq!(
        result.message.as_deref(),
        Some("Fork picker: select a session and then run `/fork <id>` to fork it.")
    );
    assert_eq!(
        canned.picker_calls,
        [None],
        "bare picker must be opened without preselection"
    );
    assert_eq!(
        canned.transition_checks.get(),
        0,
        "picker aliases bypass the transition gate"
    );

    // Blocked active fork.
    let mut canned = CannedLifecycle {
        blocked: true,
        ..CannedLifecycle::default()
    };
    let result = super::fork::fork_portable(&mut canned, None);
    assert!(result.is_error);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("runtime work is active"),
        "{result:?}"
    );
    assert_eq!(canned.transition_checks.get(), 1);

    // Active fork success -> message + SyncSession action from the receipt.
    let mut canned = CannedLifecycle {
        fork_active: Ok(SessionForkReceipt {
            parent_label: "parent1".to_string(),
            fork_label: "child2".to_string(),
            sync: sync_payload("child2"),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::fork::fork_portable(&mut canned, None);
    assert_eq!(
        result.message.as_deref(),
        Some("Forked session parent1 -> child2")
    );
    assert!(matches!(
        result.action,
        Some(SessionAction::SyncSession(codewhale_command_contract::facets::SessionSyncPayload { session_id: Some(ref id), .. })) if id == "child2"
    ));
    assert_eq!(canned.transition_checks.get(), 1);

    // Explicit fork success appends spawn_depth.
    let mut canned = CannedLifecycle {
        fork_from: Ok(SessionForkFromReceipt {
            parent_label: "aaaa".to_string(),
            fork_label: "bbbb".to_string(),
            spawn_depth: 2,
            sync: sync_payload("bbbb"),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::fork::fork_portable(&mut canned, Some("aaaa"));
    assert_eq!(
        result.message.as_deref(),
        Some("Forked session aaaa -> bbbb (spawn_depth 2)")
    );
    assert_eq!(canned.fork_sources, ["aaaa"]);
    assert_eq!(canned.transition_checks.get(), 1);
}

// ---- /load (Task 4.3) ----

#[test]
fn load_composes_exact_baseline_outcomes() {
    let mut canned = CannedLifecycle {
        blocked: true,
        ..CannedLifecycle::default()
    };
    let result = super::load::load_portable(&mut canned, Some("x.json"));
    assert!(result.is_error);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("runtime work is active")
    );
    assert_eq!(canned.transition_checks.get(), 1);
    assert!(canned.load_paths.is_empty());

    let mut canned = CannedLifecycle::default();
    let result = super::load::load_portable(&mut canned, None);
    assert_eq!(
        result.message.as_deref(),
        Some("Error: Usage: /load <path>")
    );
    assert_eq!(canned.transition_checks.get(), 1);
    assert!(canned.load_paths.is_empty());

    let mut canned = CannedLifecycle {
        load: Ok(PathBuf::from("/tmp/loaded.json")),
        ..CannedLifecycle::default()
    };
    let result = super::load::load_portable(&mut canned, Some("/tmp/loaded.json"));
    assert!(result.message.is_none(), "no premature receipt: {result:?}");
    assert!(matches!(
        result.action,
        Some(SessionAction::LoadSession(ref p)) if p == &PathBuf::from("/tmp/loaded.json")
    ));
    assert_eq!(canned.load_paths, ["/tmp/loaded.json"]);
    assert_eq!(canned.transition_checks.get(), 1);

    let mut canned = CannedLifecycle {
        load: Err("Failed to read session file: nope".to_string()),
        ..CannedLifecycle::default()
    };
    let result = super::load::load_portable(&mut canned, Some("missing.json"));
    assert_eq!(
        result.message.as_deref(),
        Some("Error: Failed to read session file: nope")
    );
}

// ---- /new (Task 4.3) ----

#[test]
fn new_composes_exact_baseline_outcomes() {
    // Unknown argument usage.
    let mut canned = CannedLifecycle::default();
    let result = super::new::new_portable(&mut canned, Some("bogus"));
    assert!(result.is_error);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("Unknown argument: bogus"),
        "{result:?}"
    );
    assert_eq!(
        canned.transition_checks.get(),
        0,
        "argument validation precedes the transition gate"
    );

    // Blocked.
    let mut canned = CannedLifecycle {
        blocked: true,
        blockers: vec!["shell_a3f2  running  5h 18m  cw-leftovers.ps1".to_string()],
        ..CannedLifecycle::default()
    };
    let result = super::new::new_portable(&mut canned, None);
    assert!(result.is_error);
    let message = result.message.as_deref().unwrap_or_default();
    assert!(
        message.contains("Cannot start a new session while runtime work is active"),
        "{message}"
    );
    assert!(message.contains("shell_a3f2"), "{message}");
    assert!(message.contains("/jobs cancel-all"), "{message}");
    assert!(
        message.contains("`/new --force` only discards draft or queued input"),
        "the flag's real scope stays stated where the flag exists: {message}"
    );
    assert_eq!(canned.transition_checks.get(), 1);
    assert!(canned.fresh_forces.is_empty());

    // Success -> message + empty SyncSession action.
    let mut canned = CannedLifecycle {
        fresh: Ok(SessionNewReceipt {
            truncated_id: "new-123".to_string(),
            sync: sync_payload("new-123"),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::new::new_portable(&mut canned, Some("--force"));
    assert_eq!(
        result.message.as_deref(),
        Some(
            "Started new session new-123 (New Session). Previous sessions remain available via /resume."
        )
    );
    assert!(matches!(
        result.action,
        Some(SessionAction::SyncSession(codewhale_command_contract::facets::SessionSyncPayload { session_id: Some(ref id), .. })) if id == "new-123"
    ));
    assert_eq!(canned.fresh_forces, [true]);
    assert_eq!(canned.transition_checks.get(), 1);

    // Host blocker error passes through.
    let mut canned = CannedLifecycle {
        fresh: Err("Cannot start a new session while the composer has unsent text. Run `/new --force` to discard pending work and start a fresh session.".to_string()),
        ..CannedLifecycle::default()
    };
    let result = super::new::new_portable(&mut canned, None);
    assert!(result.is_error);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("/new --force")
    );
}

// ---- /save (Task 4.3) ----

#[test]
fn save_composes_exact_baseline_receipt() {
    let mut canned = CannedLifecycle {
        save: Ok(SessionSaveReceipt {
            display_path: "/tmp/abc.json".to_string(),
            truncated_id: "abc123".to_string(),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::save::save_portable(&mut canned, Some("/tmp/abc.json"));
    assert_eq!(
        result.message.as_deref(),
        Some("Session saved to /tmp/abc.json (ID: abc123)")
    );
    assert!(result.action.is_none());
    assert_eq!(canned.save_paths, [Some("/tmp/abc.json".to_string())]);

    let mut canned = CannedLifecycle {
        save: Err("Failed to save session: boom".to_string()),
        ..CannedLifecycle::default()
    };
    let result = super::save::save_portable(&mut canned, None);
    assert_eq!(
        result.message.as_deref(),
        Some("Error: Failed to save session: boom")
    );
}

// ---- /sessions (Task 4.5) ----

#[test]
fn sessions_composes_exact_baseline_outcomes() {
    // Bare -> picker push, no message/action.
    let mut canned = CannedLifecycle::default();
    let result = super::sessions::sessions_portable(&mut canned, None);
    assert_eq!(result.message, None);
    assert_eq!(result.action, None);
    assert_eq!(canned.picker_calls, [None]);

    // show/list/picker aliases.
    for alias in ["show", "list", "picker"] {
        let mut canned = CannedLifecycle::default();
        let result = super::sessions::sessions_portable(&mut canned, Some(alias));
        assert_eq!(result.message, None, "{alias}");
        assert_eq!(canned.picker_calls, [None], "{alias}");
    }

    // open with preselection.
    let mut canned = CannedLifecycle::default();
    let _result = super::sessions::sessions_portable(&mut canned, Some("open abc123"));
    assert_eq!(canned.picker_calls, [Some("abc123".to_string())]);

    // open without id -> usage.
    let result = super::sessions::sessions_portable(&mut canned, Some("open"));
    assert_eq!(
        result.message.as_deref(),
        Some("Error: usage: /sessions open <session-id>")
    );

    // archive/unarchive messages.
    let mut canned = CannedLifecycle {
        archived: Ok(SessionArchiveReceipt {
            truncated_id: "zzz".to_string(),
            title: "My Session".to_string(),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::sessions::sessions_portable(&mut canned, Some("archive zzz"));
    assert_eq!(
        result.message.as_deref(),
        Some("Archived session zzz (My Session)")
    );
    assert_eq!(canned.archive_calls, [("zzz".to_string(), true)]);
    let mut canned = CannedLifecycle {
        archived: Ok(SessionArchiveReceipt {
            truncated_id: "zzz".to_string(),
            title: "My Session".to_string(),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::sessions::sessions_portable(&mut canned, Some("restore zzz"));
    assert_eq!(
        result.message.as_deref(),
        Some("Restored session zzz (My Session)")
    );
    assert_eq!(canned.archive_calls, [("zzz".to_string(), false)]);

    // prune parsing + messages.
    let result = super::sessions::sessions_portable(&mut canned, Some("prune"));
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("usage: /sessions prune <days>")
    );
    let result = super::sessions::sessions_portable(&mut canned, Some("prune abc"));
    assert_eq!(
        result.message.as_deref(),
        Some("Error: expected a positive integer number of days, got `abc`")
    );
    let mut canned = CannedLifecycle {
        prune: Ok(0),
        ..CannedLifecycle::default()
    };
    let result = super::sessions::sessions_portable(&mut canned, Some("prune 30"));
    assert_eq!(
        result.message.as_deref(),
        Some("no sessions older than 30d to prune")
    );
    assert_eq!(canned.prune_days, [30]);
    let mut canned = CannedLifecycle {
        prune: Ok(2),
        ..CannedLifecycle::default()
    };
    let result = super::sessions::sessions_portable(&mut canned, Some("prune 30"));
    assert_eq!(
        result.message.as_deref(),
        Some("pruned 2 sessions older than 30d")
    );
    assert_eq!(canned.prune_days, [30]);

    // Unknown subcommand.
    let result = super::sessions::sessions_portable(&mut canned, Some("teleport"));
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("unknown subcommand `teleport`")
    );
}

// ---- /tree (Task 4.5) ----

#[test]
fn tree_composes_exact_baseline_messages() {
    let mut canned = CannedLifecycle {
        tree: Ok(TreeBodyProjection::Journal {
            rendered: "journal body".to_string(),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::tree::tree_portable(&mut canned, None);
    assert_eq!(
        result.message.as_deref(),
        Some(
            "journal body\nUse `/branch <entry_id>` to branch (moves leaf only, never rewrites history).\nUse `/fork [session_id]` to fork this session at any node.\n"
        )
    );

    let mut canned = CannedLifecycle {
        tree: Ok(TreeBodyProjection::Linear {
            rendered: "linear body".to_string(),
        }),
        ..CannedLifecycle::default()
    };
    let result = super::tree::tree_portable(&mut canned, None);
    assert_eq!(
        result.message.as_deref(),
        Some("linear body\nUse `/branch <n>` with entry id after journal is saved.\n")
    );

    let mut canned = CannedLifecycle {
        tree: Ok(TreeBodyProjection::EmptySession),
        ..CannedLifecycle::default()
    };
    let result = super::tree::tree_portable(&mut canned, None);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("(empty session — no entries yet)")
    );

    let mut canned = CannedLifecycle {
        tree: Ok(TreeBodyProjection::NoSession),
        ..CannedLifecycle::default()
    };
    let result = super::tree::tree_portable(&mut canned, None);
    assert!(
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("No active session")
    );

    let mut canned = CannedLifecycle {
        tree: Err("could not open sessions directory: boom".to_string()),
        ..CannedLifecycle::default()
    };
    let result = super::tree::tree_portable(&mut canned, None);
    assert_eq!(
        result.message.as_deref(),
        Some("Error: could not open sessions directory: boom")
    );
}
