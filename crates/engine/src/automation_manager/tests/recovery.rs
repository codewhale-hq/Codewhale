use super::*;

/// A schedule that went five owed slots behind produces one catch-up run for
/// the oldest owed slot and resumes on the next future grid slot — it must
/// not replay one stale slot per tick.
#[tokio::test]
async fn overdue_recurring_schedule_coalesces_to_one_catch_up_run() -> Result<()> {
    let root = tempfile::tempdir()?;
    let receipts = root.path().join("executions");
    let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
    let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
    let mut automation = automation_record_with_settings(None, None, None, None);
    automation.id = "overdue".into();
    let first_due = Utc::now() - Duration::hours(5) - Duration::minutes(3);
    automation.next_run_at = Some(first_due);
    manager.save_automation(&automation)?;
    let shared = Arc::new(Mutex::new(manager));

    scheduler_tick_shared(&shared, &tasks).await?;
    let bound_id = {
        let manager = shared.lock().await;
        let runs = manager.list_runs(&automation.id, None)?;
        assert_eq!(runs.len(), 1, "one catch-up run, not one per missed slot");
        assert_eq!(
            runs[0].scheduled_for, first_due,
            "the receipt owns the oldest owed slot"
        );
        let updated = manager.get_automation(&automation.id)?;
        let next = updated
            .next_run_at
            .context("an hourly schedule keeps a future slot")?;
        let now = Utc::now();
        assert!(
            next > now && next <= now + Duration::hours(1),
            "advance must land on the *first* future grid slot, got {next}"
        );
        // Unanchored HOURLY keeps its established grid — minute-truncated
        // owed slot plus exact interval multiples — never re-phased to the
        // recovery instant.
        let grid = first_due
            .with_second(0)
            .and_then(|t| t.with_nanosecond(0))
            .context("minute truncation")?;
        assert_eq!(
            (next - grid).num_seconds() % Duration::hours(1).num_seconds(),
            0,
            "resume slot left the established hourly grid"
        );
        runs[0].task_id.clone().context("bound task id")?
    };
    let task = crate::task_manager::wait_for_terminal_state(
        &tasks,
        &bound_id,
        std::time::Duration::from_secs(10),
    )
    .await?;
    assert_eq!(task.status, TaskStatus::Completed);
    reconcile_run_statuses_shared(&shared, &tasks).await?;
    scheduler_tick_shared(&shared, &tasks).await?;
    assert_eq!(
        fixture_executions(&receipts),
        vec![bound_id],
        "the backlog produced exactly one execution"
    );
    assert_eq!(
        shared.lock().await.list_runs(&automation.id, None)?.len(),
        1
    );
    tasks.shutdown();
    Ok(())
}

/// A weekly slot owed three weeks over collapses to the upcoming calendar
/// slot — same weekday and wall time — rather than replaying each missed
/// Monday.
#[test]
fn overdue_weekly_slots_coalesce_to_the_next_calendar_slot() {
    let schedule = AutomationSchedule::parse_rrule("FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=30")
        .expect("weekly schedule parses");
    let anchor = Utc::now() - Duration::days(40);
    let slot = schedule
        .next_after_with_anchor(anchor, anchor)
        .expect("first weekly slot");
    assert!(slot < Utc::now() - Duration::days(30));
    let now = Utc::now();
    let next = schedule
        .next_unskipped_slot(slot, now, anchor)
        .expect("weekly grid advance")
        .expect("weekly schedule always has a future slot");
    assert!(next > now, "coalesced slot must be in the future");
    assert!(
        next <= now + Duration::days(7),
        "coalesce must land on the *next* owed slot, not a later one"
    );
    let local = next.with_timezone(&Local);
    assert_eq!(local.weekday(), Weekday::Mon);
    assert_eq!((local.hour(), local.minute()), (9, 30));
}

/// While an occurrence is still queued or running, the next due slot is
/// retained as owed work. One catch-up run is admitted after the earlier run
/// settles, with no canceled receipt or lost one-shot occurrence.
#[tokio::test]
async fn scheduled_occurrences_wait_while_a_prior_run_is_in_flight() -> Result<()> {
    let root = tempfile::tempdir()?;
    let receipts = root.path().join("executions");
    let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
    let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
    let automation = fixture_due_automation(&manager, "overlap", 1);

    // A bound occurrence durably in flight: queued, owning a task identity,
    // not yet admitted.
    let mut held = queued_run_for(&automation);
    held.scheduled_for = Utc::now() - Duration::hours(2);
    bind_run_dispatch(&mut held, &automation, &tasks.data_dir(), true)?;
    manager.save_run(&held)?;

    // The next slot comes due while that run is still queued. The claim must
    // not stack a second task or consume the still-owed slot.
    let (observed, proposed) = manager.collect_due_runs(Utc::now())?.remove(0);
    assert!(
        manager
            .claim_scheduled_run(&observed, proposed, &tasks.data_dir())?
            .is_none(),
        "an in-flight occurrence blocks the next scheduled claim"
    );
    let runs = manager.list_runs(&automation.id, None)?;
    assert_eq!(runs.len(), 1, "only the already-admitted run has a receipt");
    let stored = manager.get_automation(&automation.id)?;
    assert_eq!(
        stored.next_run_at, automation.next_run_at,
        "owed slot is retained"
    );
    assert!(
        fixture_executions(&receipts).is_empty(),
        "the waiting slot produced no task"
    );

    // Once the earlier occurrence settles, the original owed slot admits.
    held.status = AutomationRunStatus::Completed;
    held.ended_at = Some(Utc::now());
    manager.save_run(&held)?;
    let (observed, proposed) = manager.collect_due_runs(Utc::now())?.remove(0);
    let admitted = manager
        .claim_scheduled_run(&observed, proposed, &tasks.data_dir())?
        .context("a settled history no longer gates admission")?;
    assert_eq!(admitted.status, AutomationRunStatus::Queued);
    assert_eq!(Some(admitted.scheduled_for), automation.next_run_at);
    assert!(manager.get_automation(&automation.id)?.next_run_at.unwrap() > Utc::now());
    assert!(admitted.task_id.is_some());
    tasks.shutdown_and_wait().await?;
    Ok(())
}

/// One damaged definition — or one outright unparseable file — quarantines to
/// a diagnostic and leaves its bytes alone; every healthy owned automation
/// still collects and dispatches.
#[tokio::test]
async fn damaged_definitions_quarantine_without_starving_owned_work() -> Result<()> {
    let root = tempfile::tempdir()?;
    let receipts = root.path().join("executions");
    let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
    let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
    fixture_due_automation(&manager, "healthy", 1);

    let mut broken = automation_record_with_settings(None, None, None, None);
    broken.id = "broken".into();
    broken.rrule = "FREQ=HOURLY;INTERVAL=nope".into();
    broken.next_run_at = Some(Utc::now() - Duration::minutes(1));
    manager.save_automation(&broken)?;

    let garbage = manager.automations_dir.join("garbage.json");
    fs::write(&garbage, b"{not json")?;
    let garbage_before = fs::read(&garbage)?;

    let shared = Arc::new(Mutex::new(manager));
    scheduler_tick_shared(&shared, &tasks).await?;

    // The dispatched occurrence runs asynchronously; settle it before the
    // receipt assertions.
    let bound_id = {
        let manager = shared.lock().await;
        let runs = manager.list_runs("healthy", None)?;
        assert_eq!(runs.len(), 1, "the healthy owned automation still claimed");
        assert!(
            manager.list_runs("broken", None)?.is_empty(),
            "the unevaluable schedule never claimed a run"
        );
        runs[0].task_id.clone().context("bound task id")?
    };
    let task = crate::task_manager::wait_for_terminal_state(
        &tasks,
        &bound_id,
        std::time::Duration::from_secs(10),
    )
    .await?;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(
        fixture_executions(&receipts).len(),
        1,
        "the healthy owned automation still dispatched"
    );
    assert_eq!(
        fs::read(&garbage)?,
        garbage_before,
        "unreadable definitions are preserved, never rewritten or deleted"
    );
    let manager = shared.lock().await;
    let stored = manager.get_automation("broken")?;
    assert_eq!(
        stored.rrule, broken.rrule,
        "damaged schedule left untouched"
    );
    assert_eq!(stored.status, AutomationStatus::Active);
    tasks.shutdown_and_wait().await?;
    Ok(())
}

/// A corrupt receipt quarantines only its own automation: the strict history
/// read inside the claim refuses to risk re-admitting that slot, while the
/// sibling automation's due work still dispatches.
#[tokio::test]
async fn damaged_run_receipt_quarantines_only_its_own_automation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let receipts = root.path().join("executions");
    let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
    let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
    let healthy = fixture_due_automation(&manager, "healthy", 1);
    let damaged = fixture_due_automation(&manager, "damaged", 2);

    let runs_dir = manager.runs_dir_for(&damaged.id)?;
    fs::create_dir_all(&runs_dir)?;
    let corrupt = runs_dir.join("corrupt.json");
    fs::write(&corrupt, b"{}")?;

    let shared = Arc::new(Mutex::new(manager));
    scheduler_tick_shared(&shared, &tasks).await?;

    // The dispatched occurrence runs asynchronously; settle it before the
    // receipt assertions.
    let bound_id = {
        let manager = shared.lock().await;
        let runs = manager.list_runs(&healthy.id, None)?;
        assert_eq!(runs.len(), 1, "healthy automation claimed its run");
        runs[0].task_id.clone().context("bound task id")?
    };
    let task = crate::task_manager::wait_for_terminal_state(
        &tasks,
        &bound_id,
        std::time::Duration::from_secs(10),
    )
    .await?;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(
        fixture_executions(&receipts).len(),
        1,
        "healthy work dispatched despite the sibling's corrupt receipt"
    );
    assert_eq!(fs::read(&corrupt)?, b"{}", "corrupt receipt is preserved");
    let manager = shared.lock().await;
    assert!(
        manager.list_runs(&healthy.id, None)?.len() == 1,
        "healthy automation recorded its run"
    );
    // The damaged automation is quarantined: still due, never advanced by a
    // claim it cannot verify, and never silently re-admitted.
    let stored = manager.get_automation(&damaged.id)?;
    assert_eq!(stored.status, AutomationStatus::Active);
    tasks.shutdown_and_wait().await?;
    Ok(())
}
