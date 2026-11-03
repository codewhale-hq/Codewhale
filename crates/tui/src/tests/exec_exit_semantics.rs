use codewhale_engine::exec_events::exec_error_event_is_fatal;

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
