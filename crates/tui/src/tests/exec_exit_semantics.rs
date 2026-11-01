use super::*;

#[test]
fn retryable_infra_categories_exit_with_ex_tempfail() {
    // Provider/transport failures after all in-session retries: the task
    // neither passed nor failed, and the harness may safely retry.
    assert_eq!(exec_failure_exit_code(Some("network")), 75);
    assert_eq!(exec_failure_exit_code(Some("timeout")), 75);
    assert_eq!(EXEC_EXIT_RETRYABLE_INFRA, 75, "EX_TEMPFAIL from sysexits.h");
}

#[test]
fn genuine_failures_keep_exit_1() {
    // Task-side failures and unknown categories keep the historical
    // exit-1 contract — no masking, no forced zero exits.
    assert_eq!(exec_failure_exit_code(Some("tool")), 1);
    assert_eq!(exec_failure_exit_code(Some("authentication")), 1);
    assert_eq!(exec_failure_exit_code(Some("invalid_input")), 1);
    assert_eq!(exec_failure_exit_code(None), 1);
    // rate_limit is deliberately exit 1: the same category also covers
    // quota exhaustion, which a blind harness retry would hammer.
    assert_eq!(exec_failure_exit_code(Some("rate_limit")), 1);
}

#[test]
fn recoverable_error_events_do_not_fail_the_run_summary() {
    // A recoverable warning (e.g. a stream-stall notice mid-turn) must
    // not force the exec summary into failure; the terminal TurnComplete
    // carries the authoritative outcome.
    let warning = codewhale_error_taxonomy::ErrorEnvelope::network(
        "Stream stalled: no data received for 120s, closing stream",
    );
    assert!(
        !exec_error_event_is_fatal(&warning),
        "recoverable envelopes must not poison the exec summary"
    );
    let fatal = codewhale_error_taxonomy::ErrorEnvelope::fatal("engine exploded");
    assert!(exec_error_event_is_fatal(&fatal));
}
