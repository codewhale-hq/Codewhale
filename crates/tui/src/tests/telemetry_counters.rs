use codewhale_telemetry::counters::http_status_counter;
use codewhale_telemetry::{ErrorCounter, session_counters};

#[test]
fn a_custom_provider_is_recorded_as_the_literal_custom() {
    // This is the single most likely leak in the feature: the persistence
    // identity, the exec stream meta, and the planned route's effective
    // label all return the customer's own `[providers.<name>]` table key
    // when the route is custom, and `/status` already prints it. The
    // recording API takes a `ProviderKind` by value so none of them fit.
    crate::client::record_provider_response(codewhale_config::ProviderKind::Custom, 200);
    let providers = session_counters().providers();
    assert!(
        providers.iter().any(|name| name == "custom"),
        "expected the literal `custom`, got {providers:?}"
    );

    let closed: std::collections::BTreeSet<&str> = codewhale_config::ProviderKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect();
    for name in &providers {
        assert!(
            closed.contains(name.as_str()),
            "`{name}` is not a ProviderKind; a table name reached the provider set"
        );
    }
}

#[test]
fn http_status_classes_land_in_the_right_error_counter() {
    // Captured from the response, before any `LlmError` is built: every
    // variant of that error carries the raw provider body verbatim.
    assert_eq!(http_status_counter(200), None);
    assert_eq!(http_status_counter(304), None);
    assert_eq!(
        http_status_counter(429),
        Some(ErrorCounter::ProviderHttp4xx)
    );
    assert_eq!(
        http_status_counter(503),
        Some(ErrorCounter::ProviderHttp5xx)
    );
}

#[test]
fn the_turn_wall_histogram_buckets_by_wall_clock_not_per_turn_events() {
    // A histogram, never a timestamped series: a stream of per-turn
    // durations reconstructs a session's working rhythm.
    let mut wall = codewhale_telemetry::TurnWall::default();
    wall.observe_secs(0);
    wall.observe_secs(4);
    wall.observe_secs(5);
    wall.observe_secs(119);
    wall.observe_secs(120);
    assert_eq!(wall.lt_5s, 2);
    assert_eq!(wall.five_to_thirty, 1);
    assert_eq!(wall.thirty_to_onetwenty, 1);
    assert_eq!(wall.gte_120s, 1);
}
