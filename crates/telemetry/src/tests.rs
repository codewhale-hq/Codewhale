//! The tests are the contract.
//!
//! Two of them are load-bearing beyond ordinary coverage:
//! `every_payload_field_is_bounded` walks a fully-populated batch and asserts
//! that every string leaf is a member of a declared enum set or one of exactly
//! three regexed strings, and `every_string_leaf_survives_redaction_unchanged`
//! runs the workflow crate's disclosure redactor over each leaf **individually**
//! — never over the serialized document, which would be one whitespace-free
//! token and would report clean no matter what it contained.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codewhale_config::{
    CliRuntimeOverrides, ConfigToml, ResolvedRuntimeOptions, SetupState, TELEMETRY_NOTICE_VERSION,
};
use serde_json::Value;

use crate::buffer;
use crate::decision::{
    EndpointError, TelemetryDecision, decide_in_home, load_setup_state_for_decision_at,
    permission_still_enabled_in_home, re_decide_with_setup_path, validate_endpoint,
};
use crate::envelope;
use crate::event::*;

// ---------------------------------------------------------------- fixtures --

fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp home")
}

fn root_of(home: &tempfile::TempDir) -> PathBuf {
    home.path().join(crate::TELEMETRY_DIR)
}

/// A resolved-options value with everything but telemetry left at its default.
fn resolved(telemetry: bool, explicit_off: bool, endpoint: Option<&str>) -> ResolvedRuntimeOptions {
    let mut options =
        ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
    options.telemetry = telemetry;
    options.telemetry_explicit_off = explicit_off;
    options.telemetry_endpoint = endpoint.map(str::to_string);
    options
}

fn accepted_setup() -> SetupState {
    let mut setup = SetupState::default();
    setup.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, true);
    setup
}

fn declined_setup() -> SetupState {
    let mut setup = SetupState::default();
    setup.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, false);
    setup
}

fn stale_setup() -> SetupState {
    let mut setup = SetupState::default();
    setup.record_telemetry_notice("0", true);
    setup
}

#[test]
fn setup_state_loader_defaults_only_when_the_privacy_record_is_absent() {
    let home = temp_home();
    let path = home.path().join("setup_state.json");

    assert!(
        load_setup_state_for_decision_at(&path).is_some(),
        "a genuinely fresh install uses the documented default"
    );

    std::fs::write(&path, b"{not-json").expect("write corrupt setup state");
    assert!(
        load_setup_state_for_decision_at(&path).is_none(),
        "an existing unreadable privacy record must fail closed"
    );

    accepted_setup()
        .save_to(&path)
        .expect("write valid setup state");
    assert!(
        load_setup_state_for_decision_at(&path)
            .is_some_and(|setup| setup.telemetry_accepted(TELEMETRY_NOTICE_VERSION)),
        "a valid setup state remains usable"
    );
}

#[test]
fn flush_redecision_fails_closed_on_a_corrupt_setup_state() {
    let home = temp_home();
    let config_path = home.path().join("config.toml");
    let setup_path = home.path().join("setup_state.json");
    std::fs::write(&config_path, "telemetry = true\n").expect("write config");
    std::fs::write(&setup_path, b"{not-json").expect("write corrupt setup state");

    assert!(matches!(
        re_decide_with_setup_path(Some(&config_path), &setup_path, Surface::Exec),
        TelemetryDecision::ForcedOff
    ));
}

/// One instance of every event variant, populated with the most adversarial
/// values the schema permits.
///
/// **This list is hand-written, and the compiler cannot make you extend it.**
/// A new `Event` variant carrying a free-form `String` would be walked by none
/// of the red-line tests below — they all start here — and `golden_payload_v1`
/// would still pass, because it serializes this same fixture. Nothing closes
/// that hole from inside this file; enumerating an enum's variants needs
/// reflection this workspace deliberately does not depend on. What does bite is
/// [`Event::is_bounded`], whose `match self` is exhaustive: adding a variant
/// fails the build until its author states a bound. If you are that author,
/// add the variant here too.
fn every_event() -> Vec<Event> {
    vec![
        Event::InstallOrUpgrade {
            kind: InstallKind::Upgrade,
            previous_version: Some("0.9.3-rc.1".to_string()),
        },
        Event::SessionStart {
            source: SessionSource::Resume,
        },
        Event::SessionEnd {
            duration_bucket: DurationBucket::OneToTen,
            exit_class: ExitClass::Panic,
            cold_start_bucket: Some(ColdStartBucket::Mid),
            providers: vec!["custom".to_string(), "deepseek".to_string()],
            counters: Counters {
                turns: 14,
                tool_calls: 61,
                fleet_dispatch: 0,
                workflow_run: 0,
                subagent_spawn: 2,
                mcp_server_connected: 0,
                memory_search: 0,
                approval_modal_shown: 0,
                approval_auto_allowed: 0,
                command_palette_open: 3,
            },
            errors: Errors {
                auth_preflight_failed: 0,
                provider_http_4xx: 0,
                provider_http_5xx: 1,
                tool_denied_by_policy: 0,
                tool_timeout: 0,
                network_error: 0,
            },
            turn_wall: TurnWall {
                lt_5s: 9,
                five_to_thirty: 4,
                thirty_to_onetwenty: 1,
                gte_120s: 0,
            },
        },
        Event::Panic {
            site: "crates/tui/src/tui/ui.rs:8801:17".to_string(),
        },
        Event::ProductUsage {
            counters: ProductCounters {
                page_view: 1,
                ..ProductCounters::default()
            },
        },
        Event::OperationsSummary {
            requests: 10,
            errors: 1,
            duration_ms_total: 1500,
            duration_ms_max: 300,
            probes: 2,
            probes_failed: 0,
        },
        Event::ModelUsage {
            provider: "deepseek".to_string(),
            model: "other".to_string(),
            counters: crate::event::ModelCounters {
                turns_ok: 12,
                turns_failed: 1,
                turns_interrupted: 0,
                input_tokens: 418_230,
                output_tokens: 9_120,
                cache_read_tokens: 380_001,
                cache_write_tokens: 0,
            },
            turn_wall: TurnWall {
                lt_5s: 3,
                five_to_thirty: 8,
                thirty_to_onetwenty: 2,
                gte_120s: 0,
            },
        },
        Event::ToolUsage {
            tool: "read_file".to_string(),
            calls: 41,
            errors: 2,
        },
    ]
}

/// A fully-populated batch. This is the artifact the red-line tests walk.
pub(crate) fn every_field_batch() -> Batch {
    Batch {
        schema_version: SCHEMA_VERSION,
        notice_version: NOTICE_VERSION,
        sent_at: "2026-08-03T18:04:11Z".to_string(),
        install_id: "3f2a9c1e-0000-4000-8000-000000000001".to_string(),
        app_version: "0.9.4".to_string(),
        git_sha: Some("abcdef012345".to_string()),
        surface: Surface::ControlPlane,
        os: Os::Macos,
        arch: Arch::Aarch64,
        libc: Libc::None,
        tty: true,
        events: every_event(),
    }
}

// --------------------------------------------------------- schema red lines --

fn walk_strings(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) => out.push((path.to_string(), text.clone())),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                walk_strings(item, &format!("{path}[{index}]"), out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                walk_strings(item, &child, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn string_leaves(value: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_strings(value, "", &mut out);
    out
}

// The version and panic-site rules are the shipped predicates, not local
// copies. A test that re-implements the rule it is checking passes against a
// binary that enforces nothing — which is exactly what
// `a_hostile_buffer_line_never_reaches_a_batch` found the first time.
use crate::event::{is_reduced_panic_site, is_release_version_string as is_app_version};

fn is_short_sha(value: &str) -> bool {
    value.len() == 12
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Every closed-enum string this schema may ever emit.
fn closed_enum_values() -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    values.extend(Surface::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(Os::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(Arch::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(Libc::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(InstallKind::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(SessionSource::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(DurationBucket::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(ExitClass::ALL.iter().map(|v| v.as_str().to_string()));
    values.extend(ColdStartBucket::ALL.iter().map(|v| v.as_str().to_string()));
    // The `event` tag values.
    values.extend(every_event().iter().map(|e| e.name().to_string()));
    // `providers` entries: `ProviderKind::as_str()` is a `&'static str` from a
    // closed enum, and `Custom` yields the literal "custom".
    values.extend(
        codewhale_config::ProviderKind::all()
            .iter()
            .map(|kind| kind.as_str().to_string()),
    );
    values
}

#[test]
fn every_payload_field_is_bounded() {
    let batch = every_field_batch();
    let json = serde_json::to_value(&batch).expect("serialize batch");
    let enums = closed_enum_values();
    let leaves = string_leaves(&json);
    assert!(
        leaves.len() >= 10,
        "the walk found suspiciously few string leaves: {leaves:?}"
    );

    for (path, value) in leaves {
        let ok = match path.as_str() {
            "app_version" => is_app_version(&value),
            "git_sha" => is_short_sha(&value),
            "sent_at" => value.ends_with('Z') && value.len() == 20,
            "install_id" => uuid::Uuid::parse_str(&value).is_ok(),
            p if p.ends_with(".site") => is_reduced_panic_site(&value),
            p if p.ends_with(".previous_version") => is_app_version(&value),
            p if p.ends_with(".tool") => is_reported_tool_name(&value),
            p if p.ends_with(".model") => is_reported_model_id(&value),
            _ => enums.contains(&value),
        };
        assert!(ok, "unbounded string leaf at {path}: {value:?}");
    }
}

#[test]
fn counters_and_errors_serialize_every_field_including_zeros() {
    let json = serde_json::to_value(Counters::default()).expect("serialize counters");
    let object = json.as_object().expect("counters is an object");
    assert_eq!(object.len(), Counters::FIELDS.len());
    for field in Counters::FIELDS {
        assert_eq!(object.get(*field), Some(&Value::from(0u32)), "{field}");
    }

    let json = serde_json::to_value(Errors::default()).expect("serialize errors");
    let object = json.as_object().expect("errors is an object");
    assert_eq!(object.len(), Errors::FIELDS.len());
    for field in Errors::FIELDS {
        assert_eq!(object.get(*field), Some(&Value::from(0u32)), "{field}");
    }

    let json = serde_json::to_value(TurnWall::default()).expect("serialize turn_wall");
    let object = json.as_object().expect("turn_wall is an object");
    assert_eq!(object.len(), TurnWall::FIELDS.len());
    for field in TurnWall::FIELDS {
        assert_eq!(object.get(*field), Some(&Value::from(0u32)), "{field}");
    }
}

#[test]
fn no_event_field_is_ever_omitted() {
    // Options serialize as `null` rather than being skipped, so the key set on
    // the wire is closed and the doc-match test can be exact.
    let event = Event::SessionEnd {
        duration_bucket: DurationBucket::Lt1m,
        exit_class: ExitClass::Clean,
        cold_start_bucket: None,
        providers: Vec::new(),
        counters: Counters::default(),
        errors: Errors::default(),
        turn_wall: TurnWall::default(),
    };
    let json = serde_json::to_value(&event).expect("serialize");
    assert_eq!(json.get("cold_start_bucket"), Some(&Value::Null));

    let event = Event::InstallOrUpgrade {
        kind: InstallKind::Install,
        previous_version: None,
    };
    let json = serde_json::to_value(&event).expect("serialize");
    assert_eq!(json.get("previous_version"), Some(&Value::Null));
}

// ------------------------------------------------------ scrubber assertions --

/// Run the workflow crate's disclosure redactor over **each string leaf**.
///
/// Never over the serialized document: `redact_for_disclosure` tokenizes with
/// `input.split(' ')`, and a compact `serde_json` batch has no spaces, so the
/// whole document would be one token and every classifier would fail on it. A
/// batch containing an absolute path, a live-looking key, and a whole prompt
/// would report clean. The gate would detect nothing while appearing to pass.
fn redaction_kinds_over_leaves(json: &Value) -> Vec<String> {
    let mut kinds = Vec::new();
    for (_, value) in string_leaves(json) {
        let redaction = codewhale_workflow::redaction::redact_for_disclosure(&value);
        if redaction.redacted() {
            kinds.extend(redaction.kinds());
        }
    }
    kinds
}

#[test]
fn every_string_leaf_survives_redaction_unchanged() {
    let json = serde_json::to_value(every_field_batch()).expect("serialize batch");
    let kinds = redaction_kinds_over_leaves(&json);
    assert!(
        kinds.is_empty(),
        "a real payload tripped the disclosure redactor: {kinds:?}"
    );
}

#[test]
fn redaction_catches_a_planted_absolute_path() {
    let mut json = serde_json::to_value(every_field_batch()).expect("serialize batch");
    json["app_version"] = Value::from("/Users/hunter/src/app/main.rs");
    let kinds = redaction_kinds_over_leaves(&json);
    assert!(
        kinds.iter().any(|k| k == "absolute_path"),
        "the negative control did not fire: {kinds:?}"
    );
}

#[test]
fn redaction_catches_a_planted_secret() {
    let mut json = serde_json::to_value(every_field_batch()).expect("serialize batch");
    // Deliberately low-entropy: a realistic token in a fixture trips secret
    // scanners at push time.
    json["app_version"] = Value::from("api_key=sk-live-abcdef0123456789abcdef");
    let kinds = redaction_kinds_over_leaves(&json);
    assert!(
        kinds.iter().any(|k| k == "secret"),
        "the negative control did not fire: {kinds:?}"
    );
}

#[test]
fn panic_site_is_the_only_field_that_may_carry_a_path() {
    // `panic_site` is a repo-relative path by design, so it is the one
    // documented exemption. Prove the redactor would flag such a value, and
    // that no other leaf in a real payload carries one.
    let planted = codewhale_workflow::redaction::redact_for_disclosure("crates/tui/src/main.rs");
    assert!(
        planted.kinds().iter().any(|k| k == "relative_path"),
        "the redactor no longer classifies a repo-relative path: {:?}",
        planted.kinds()
    );

    let json = serde_json::to_value(every_field_batch()).expect("serialize batch");
    for (path, value) in string_leaves(&json) {
        if path.ends_with(".site") {
            continue;
        }
        let redaction = codewhale_workflow::redaction::redact_for_disclosure(&value);
        assert!(
            !redaction.kinds().iter().any(|k| k.ends_with("path")),
            "a non-exempt leaf carries a path: {path} = {value:?}"
        );
    }
}

// ------------------------------------------- the drain path is a boundary --

/// The buffer is a **deserializer input**, not an internal channel.
///
/// Every bound above is a property of how this process *builds* an event.
/// `flush` re-reads `buffer.jsonl` and hands the lines to `serde`, and any
/// process running as the user can append to that file — including a `Bash`
/// tool call the session made on the model's behalf, since `$CODEWHALE_HOME`
/// is a predictable path. Before `Event::is_bounded` existed, an appended
/// `{"event":"panic","site":"…/Users/victim/secret-repo"}` was POSTed verbatim
/// to the configured endpoint under the user's install id; the process-level
/// proof of that is `a_hostile_buffer_line_never_reaches_a_batch` in
/// `crates/tui/tests/telemetry_contract.rs`.
#[test]
fn hostile_buffer_lines_are_dropped_before_they_reach_a_batch() {
    let hostile = [
        // A path, which is the class `panic_site` is the sole exemption for.
        r#"{"event":"panic","site":"/Users/victim/src/secret-repo/main.rs"}"#,
        // A whole prompt in the one field that is allowed to look like text.
        r#"{"event":"panic","site":"rewrite the auth module for acme-corp"}"#,
        // A frame outside the `crates/` allowlist, spelled to look inside it.
        r#"{"event":"panic","site":"../vendor/crates/foo/src/lib.rs:1:1"}"#,
        // `previous_version` is read back from `state.json`, never validated
        // at the point it is written.
        r#"{"event":"install_or_upgrade","kind":"upgrade","previous_version":"/Users/victim/.ssh/id_ed25519"}"#,
        // A customer's `[providers.<name>]` table key — the exact string
        // `record_provider` takes a `ProviderKind` by value to avoid.
        r#"{"event":"session_end","duration_bucket":"lt_1m","exit_class":"clean","cold_start_bucket":null,"providers":["acme_internal_gateway"],"counters":{"turns":0,"tool_calls":0,"fleet_dispatch":0,"workflow_run":0,"subagent_spawn":0,"mcp_server_connected":0,"memory_search":0,"approval_modal_shown":0,"approval_auto_allowed":0,"command_palette_open":0},"errors":{"auth_preflight_failed":0,"provider_http_4xx":0,"provider_http_5xx":0,"tool_denied_by_policy":0,"tool_timeout":0,"network_error":0},"turn_wall":{"lt_5s":0,"5_30s":0,"30_120s":0,"gte_120s":0}}"#,
    ];
    for line in hostile {
        let event = serde_json::from_str::<Event>(line)
            .unwrap_or_else(|error| panic!("the fixture must be parseable: {error}\n{line}"));
        assert!(
            !event.is_bounded(),
            "an out-of-bounds event passed the drain check: {line}"
        );
        let parsed = crate::actor::parse_events(&[line.to_string()]);
        assert!(
            parsed.is_empty(),
            "a hostile buffer line survived the drain: {line}"
        );
    }
}

/// The drain check must not delete real telemetry. Everything this process
/// legitimately records has to survive a round trip through the buffer.
#[test]
fn every_legitimately_recorded_event_survives_the_drain() {
    let lines: Vec<String> = every_event()
        .iter()
        .map(|event| serde_json::to_string(event).expect("serialize"))
        .collect();
    assert_eq!(
        crate::actor::parse_events(&lines).len(),
        lines.len(),
        "the drain check dropped an event this process builds itself"
    );

    // Dialect kinds (`deepseek-anthropic`, the Model Studio plan variants) are
    // absent from the selectable `ProviderKind::ALL` catalog subset,
    // but captured intrinsic kinds include them for real routes. Narrowing the
    // provider bound to the catalog would drop those users' `session_end`.
    for kind in [
        codewhale_config::ProviderKind::DeepseekAnthropic,
        codewhale_config::ProviderKind::MinimaxAnthropic,
        codewhale_config::ProviderKind::Custom,
    ] {
        assert!(
            crate::event::is_known_provider_id(kind.as_str()),
            "a real routed provider is not a legal `providers` entry: {}",
            kind.as_str()
        );
    }
}

/// `install_id` is the one envelope field read verbatim off disk into a batch.
#[test]
fn a_non_uuid_install_id_on_disk_is_replaced_rather_than_sent() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create telemetry root");
    std::fs::write(
        buffer::install_id_path(&root),
        serde_json::json!({
            "schema_version": 1,
            "install_id": "/Users/victim/src/secret-repo",
            "rotated_at": envelope::now_rfc3339(),
        })
        .to_string(),
    )
    .expect("plant a hostile install id");

    let record = envelope::read_or_create_install_id(&root).expect("read install id");
    assert!(
        uuid::Uuid::parse_str(&record.install_id).is_ok(),
        "a non-UUID install id was carried onto the wire: {:?}",
        record.install_id
    );
}

/// Audit R05-09: only a canonical v4 id with a past `rotated_at` survives a
/// read. A v1 (MAC + clock), nil, braced, uppercase, or padded id is replaced,
/// and so is a future timestamp that would otherwise never rotate.
#[test]
fn a_non_canonical_or_future_dated_install_id_is_rotated() {
    let now = envelope::now_rfc3339();
    let future = (chrono::Utc::now() + chrono::Duration::days(3650))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let canonical = "3f2a9c1e-0000-4000-8000-000000000001";
    for (install_id, rotated_at) in [
        ("6ba7b810-9dad-11d1-80b4-00c04fd430c8", now.as_str()),
        ("00000000-0000-0000-0000-000000000000", now.as_str()),
        ("{3f2a9c1e-0000-4000-8000-000000000001}", now.as_str()),
        ("3F2A9C1E-0000-4000-8000-000000000001", now.as_str()),
        (" 3f2a9c1e-0000-4000-8000-000000000001 ", now.as_str()),
        (canonical, future.as_str()),
    ] {
        let home = temp_home();
        let root = root_of(&home);
        buffer::ensure_dir(&root).expect("create telemetry root");
        std::fs::write(
            buffer::install_id_path(&root),
            serde_json::json!({
                "schema_version": 1,
                "install_id": install_id,
                "rotated_at": rotated_at,
            })
            .to_string(),
        )
        .expect("plant install id");

        let record = envelope::read_or_create_install_id(&root).expect("read install id");
        assert_ne!(record.install_id, install_id, "kept {install_id:?}");
        let minted = uuid::Uuid::parse_str(&record.install_id).expect("minted uuid");
        assert_eq!(minted.get_version(), Some(uuid::Version::Random));
    }

    // The canonical, past-dated form is kept.
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create telemetry root");
    let kept = envelope::InstallId {
        schema_version: 1,
        install_id: canonical.to_string(),
        rotated_at: now.clone(),
    };
    std::fs::write(
        buffer::install_id_path(&root),
        serde_json::to_string(&kept).unwrap(),
    )
    .expect("plant canonical id");
    let record = envelope::read_or_create_install_id(&root).expect("read install id");
    assert_eq!(record.install_id, canonical);
}

// ------------------------------------------------------------- panic sites --

#[test]
fn panic_site_reduces_dependency_frames() {
    assert_eq!(
        envelope::reduce_panic_site("crates/tui/src/x.rs", 9, 1),
        "crates/tui/src/x.rs:9:1"
    );
    assert_eq!(
        envelope::reduce_panic_site(
            "/Users/builder/.cargo/registry/src/index.crates.io-1949cf8c/ratatui-0.29.0/src/y.rs",
            1,
            1
        ),
        "<dep>"
    );
    assert_eq!(
        envelope::reduce_panic_site("/rustc/deadbeef/library/core/src/panicking.rs", 1, 1),
        "<dep>"
    );
    // A path that merely mentions `crates/` somewhere is not a `crates/` frame.
    assert_eq!(
        envelope::reduce_panic_site("../vendor/crates/foo/src/lib.rs", 1, 1),
        "<dep>"
    );
}

#[test]
fn git_sha_is_null_without_release_env() {
    // The build script emits `CODEWHALE_RELEASE_BUILD_SHA` only when
    // `CODEWHALE_BUILD_SHA` (or a build-only compatibility alias) was in the
    // build environment, so on a developer machine this is `None` and on
    // release CI it is twelve hex characters. Both are asserted, because the
    // test has to pass in both places and neither shape may ever be a path, a
    // version, or a full sha.
    // The rule that produces it lives in `codewhale-build-support` and is
    // tested there against an injected environment; what is asserted here is
    // that whatever reaches the payload is `null` or twelve lowercase hex
    // characters, and never a path, a version, or a full sha.
    if let Some(sha) = envelope::release_build_sha() {
        assert!(
            is_short_sha(&sha),
            "release sha has the wrong shape: {sha:?}"
        );
    }
    assert_eq!(
        envelope::short_hex_sha("ABCDEF0123456789abcdef0123456789abcdef01"),
        Some("abcdef012345".to_string())
    );
    assert_eq!(envelope::short_hex_sha("not-a-sha"), None);
    assert_eq!(envelope::short_hex_sha("abc123"), None);
}

// --------------------------------------------------------------- decisions --

#[test]
fn decision_matrix_is_exhaustive() {
    let home = temp_home();
    let path = home.path();

    // Row: nobody has said anything. No acceptance is inferred.
    assert!(matches!(
        decide_in_home(
            Some(path),
            &resolved(true, false, None),
            &SetupState::default(),
            Surface::Tui
        ),
        TelemetryDecision::Enabled(_)
    ));

    // Row: a human said off. That is an answer.
    assert!(matches!(
        decide_in_home(
            Some(path),
            &resolved(false, true, None),
            &accepted_setup(),
            Surface::Tui
        ),
        TelemetryDecision::OptedOut
    ));

    // Row: config on with no notice record also uses the default-on policy.
    assert!(matches!(
        decide_in_home(
            Some(path),
            &resolved(true, false, None),
            &SetupState::default(),
            Surface::Tui
        ),
        TelemetryDecision::Enabled(_)
    ));

    // Row: on, asked, declined.
    assert!(matches!(
        decide_in_home(
            Some(path),
            &resolved(true, false, None),
            &declined_setup(),
            Surface::Tui
        ),
        TelemetryDecision::OptedOut
    ));

    // Row: old explicit yes remains on under the current default-on policy.
    assert!(matches!(
        decide_in_home(
            Some(path),
            &resolved(true, false, None),
            &stale_setup(),
            Surface::Tui
        ),
        TelemetryDecision::Enabled(_)
    ));

    // Row: on and accepted, no home to keep state in.
    assert!(matches!(
        decide_in_home(
            None,
            &resolved(true, false, None),
            &accepted_setup(),
            Surface::Tui
        ),
        TelemetryDecision::ForcedOff
    ));

    // Row: on and accepted, plaintext endpoint to a public host.
    assert!(matches!(
        decide_in_home(
            Some(path),
            &resolved(true, false, Some("http://example.com/t")),
            &accepted_setup(),
            Surface::Tui
        ),
        TelemetryDecision::ForcedOff
    ));

    // Row: on and accepted, no endpoint — the dry-run sink, which resolution
    // reaches from an explicitly empty `telemetry_endpoint`. (The *shipped*
    // default is `DEFAULT_TELEMETRY_ENDPOINT`; this predicate never sees it,
    // because it reads an already-resolved value.)
    let decision = decide_in_home(
        Some(path),
        &resolved(true, false, None),
        &accepted_setup(),
        Surface::Exec,
    );
    let TelemetryDecision::Enabled(consent) = decision else {
        panic!("an accepted, endpoint-less machine must be Enabled");
    };
    assert_eq!(consent.endpoint(), None);
    assert_eq!(consent.surface(), Surface::Exec);
    assert_eq!(consent.root(), root_of(&home));

    // Row: on and accepted, https endpoint.
    assert!(
        decide_in_home(
            Some(path),
            &resolved(true, false, Some("https://example.com/t")),
            &accepted_setup(),
            Surface::Tui
        )
        .is_enabled()
    );

    // Row: every headless surface uses the same documented default and kill
    // switches.
    for surface in Surface::ALL {
        assert!(
            decide_in_home(
                Some(path),
                &resolved(true, false, None),
                &SetupState::default(),
                *surface
            )
            .is_enabled(),
            "{surface:?} uses default-on without inferring acceptance"
        );
    }
}

#[test]
fn every_surface_spelling_parses_back_to_its_variant() {
    // `Surface::parse` is the only place a name becomes a variant, and an
    // embedder declares itself by name, so the round trip has to be exact in
    // both directions. A value that can be spelled must parse; a variant must
    // not be reachable under a second spelling.
    for surface in Surface::ALL {
        assert_eq!(
            Surface::parse(surface.as_str()),
            Some(*surface),
            "{surface:?} does not parse back from its own wire spelling"
        );
    }
    assert_eq!(
        Surface::parse("TUI"),
        None,
        "parsing is exact, not case-folded"
    );
    assert_eq!(Surface::parse(" vscode-extension "), None, "no trimming");
    assert_eq!(Surface::parse("vscode"), None);
    assert_eq!(Surface::parse("ide"), None);
    assert_eq!(Surface::parse(""), None);
    assert_eq!(Surface::parse("desktop"), Some(Surface::Desktop));
}

#[test]
fn an_unparseable_env_value_forces_off_and_does_not_wipe() {
    // The floor in `codewhale-config` turns an unreadable `CODEWHALE_TELEMETRY`
    // into `telemetry == false` *without* setting `telemetry_explicit_off`. A
    // typo is not a user answer and must never destroy state.
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    let buffer_path = buffer::buffer_path(&root);
    buffer::append(
        &root,
        &buffer_path,
        "{\"event\":\"session_start\",\"source\":\"api\"}",
    )
    .expect("seed");

    let decision = decide_in_home(
        Some(home.path()),
        &resolved(false, false, None),
        &accepted_setup(),
        Surface::Tui,
    );
    assert!(matches!(decision, TelemetryDecision::ForcedOff));
    assert!(!buffer::tombstone_present(&root));
    assert_eq!(buffer::read_lines(&buffer_path).len(), 1);
}

#[test]
fn only_opt_out_touches_disk() {
    // Every ForcedOff row against a seeded, consenting home must leave it
    // byte-identical. This is the finding that a "wipe on resolved false" would
    // have broken: `false` is the *default*, so it fired on every ordinary run.
    let forced_off_rows: Vec<(ResolvedRuntimeOptions, SetupState)> = vec![
        (resolved(false, false, None), accepted_setup()),
        (
            resolved(true, false, Some("http://example.com/t")),
            accepted_setup(),
        ),
    ];

    for (options, setup) in forced_off_rows {
        let home = temp_home();
        let root = root_of(&home);
        buffer::ensure_dir(&root).expect("create root");
        let before = seed_consenting_home(&root);

        let decision = decide_in_home(Some(home.path()), &options, &setup, Surface::Tui);
        assert!(
            matches!(decision, TelemetryDecision::ForcedOff),
            "expected ForcedOff, got {}",
            decision.label()
        );
        assert_eq!(snapshot(&root), before, "a ForcedOff run touched disk");
    }

    // Every OptedOut row wipes: tombstone present, data truncated, lock file
    // still present, identity gone.
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    seed_consenting_home(&root);

    let decision = decide_in_home(
        Some(home.path()),
        &resolved(false, true, None),
        &accepted_setup(),
        Surface::Tui,
    );
    assert!(matches!(decision, TelemetryDecision::OptedOut));
    assert!(buffer::tombstone_present(&root));
    assert!(buffer::buffer_path(&root).exists());
    assert!(buffer::read_lines(&buffer::buffer_path(&root)).is_empty());
    assert!(buffer::read_lines(&buffer::dryrun_path(&root)).is_empty());
    assert!(
        buffer::lock_path(&root).exists(),
        "the lock file must survive a wipe: unlinking it leaves appenders on a dead inode"
    );
    assert!(!buffer::install_id_path(&root).exists());
    assert!(!buffer::state_path(&root).exists());
}

#[test]
fn default_on_does_not_hide_a_durable_sidecar_decline() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    let before = seed_consenting_home(&root);
    let mut options = resolved(false, false, None);

    // A run-scoped kill switch still preserves the preexisting ordering.
    options.telemetry_source = codewhale_config::TelemetrySource::Env;
    assert!(matches!(
        decide_in_home(Some(home.path()), &options, &declined_setup(), Surface::Tui),
        TelemetryDecision::ForcedOff
    ));
    assert_eq!(snapshot(&root), before);

    // Once that temporary switch is gone, the durable decline must wipe even
    // though the new shipped configuration preference is on.
    options.telemetry = true;
    options.telemetry_source = codewhale_config::TelemetrySource::Default;
    assert!(matches!(
        decide_in_home(Some(home.path()), &options, &declined_setup(), Surface::Tui),
        TelemetryDecision::OptedOut
    ));
    assert!(buffer::tombstone_present(&root));
    assert!(!buffer::install_id_path(&root).exists());
    assert!(buffer::read_lines(&buffer::buffer_path(&root)).is_empty());
}

#[test]
fn the_tombstone_outlives_every_run_the_opt_out_covers() {
    // `docs/TELEMETRY.md` says the opt-out's tombstone survives, and an
    // adversary showed it did not: one ordinary run afterwards called
    // `buffer::arm`, which removes the tombstone, and minted a fresh install
    // id. Both halves of that are now impossible, and for the same reason —
    // the opt-out is a *persisted* statement, so every later run re-reads it,
    // takes the OptedOut branch again, and never reaches arming at all.
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    seed_consenting_home(&root);

    // The user writes `telemetry = false`.
    let opted_out = resolved(false, true, None);
    assert!(matches!(
        decide_in_home(
            Some(home.path()),
            &opted_out,
            &accepted_setup(),
            Surface::Tui
        ),
        TelemetryDecision::OptedOut
    ));
    assert!(buffer::tombstone_present(&root));
    let after_wipe = snapshot(&root);

    // Three more launches of any surface, with the setting still in place.
    for surface in [Surface::Tui, Surface::Exec, Surface::AppServer] {
        let decision = decide_in_home(Some(home.path()), &opted_out, &accepted_setup(), surface);
        assert!(
            matches!(decision, TelemetryDecision::OptedOut),
            "{surface:?} re-read the opt-out as {}",
            decision.label()
        );
        assert!(
            buffer::tombstone_present(&root),
            "{surface:?} cleared the tombstone"
        );
        assert!(
            !buffer::install_id_path(&root).exists(),
            "{surface:?} minted a new identity for an opted-out machine"
        );
        assert!(!buffer::state_path(&root).exists());
        assert!(buffer::read_lines(&buffer::buffer_path(&root)).is_empty());
        assert!(buffer::read_lines(&buffer::dryrun_path(&root)).is_empty());
        assert_eq!(snapshot(&root), after_wipe, "{surface:?} touched disk");
    }

    // Only writing the setting back turns collection on again, and that is the
    // one path allowed to clear the tombstone.
    let TelemetryDecision::Enabled(consent) = decide_in_home(
        Some(home.path()),
        &resolved(true, false, None),
        &accepted_setup(),
        Surface::Tui,
    ) else {
        panic!("an explicit re-enable must produce consent");
    };
    buffer::arm(&root, consent.tombstone_generation(), || true).expect("re-consent arms");
    assert!(!buffer::tombstone_present(&root));
}

#[test]
fn a_run_scoped_kill_switch_costs_a_consenting_user_nothing() {
    // The documented one-command recipe — `CODEWHALE_TELEMETRY=0 codewhale` —
    // used to take the destructive opt-out branch, so it deleted the install
    // id and truncated the user's own dry-run records every time it was used.
    // The resolver now reports that as "off, but nobody revoked anything", and
    // this is the half of that contract the telemetry crate owns.
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    let before = seed_consenting_home(&root);
    let identity = std::fs::read(buffer::install_id_path(&root)).expect("seeded install id");

    for _ in 0..3 {
        let decision = decide_in_home(
            Some(home.path()),
            // `telemetry == false`, `telemetry_explicit_off == false`: the
            // shape a run-scoped kill switch resolves to.
            &resolved(false, false, None),
            &accepted_setup(),
            Surface::Exec,
        );
        assert!(matches!(decision, TelemetryDecision::ForcedOff));
    }

    assert_eq!(snapshot(&root), before, "a kill-switch run touched disk");
    assert!(!buffer::tombstone_present(&root));
    assert_eq!(
        std::fs::read(buffer::install_id_path(&root)).expect("install id"),
        identity,
        "the install id churned across a kill-switch run"
    );
}

#[test]
fn an_opt_out_on_a_fresh_home_creates_nothing() {
    let home = temp_home();
    let root = root_of(&home);
    let decision = decide_in_home(
        Some(home.path()),
        &resolved(false, true, None),
        &SetupState::default(),
        Surface::Tui,
    );
    assert!(matches!(decision, TelemetryDecision::OptedOut));
    assert!(
        !root.exists(),
        "a fresh user who opts out must not get a telemetry directory"
    );
}

fn seed_consenting_home(root: &Path) -> Vec<(String, Vec<u8>)> {
    buffer::append(
        root,
        &buffer::buffer_path(root),
        "{\"event\":\"session_start\",\"source\":\"interactive\"}",
    )
    .expect("seed buffer");
    buffer::append_locked(root, &buffer::dryrun_path(root), "{\"schema_version\":1}")
        .expect("seed dryrun");
    envelope::read_or_create_install_id(root).expect("seed install id");
    envelope::write_state(root, &envelope::TelemetryState::default()).expect("seed state");
    snapshot(root)
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<u8>)> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let body = std::fs::read(entry.path()).unwrap_or_default();
            (name, body)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn failed_wipe_fails_closed() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    seed_consenting_home(&root);

    // Make the buffer un-truncatable. The tombstone is written first, so even
    // when the rest of the wipe fails the buffer is permanently undrainable.
    let buffer_path = buffer::buffer_path(&root);
    let readonly_worked = make_read_only(&buffer_path);

    let result = buffer::wipe(&root);
    assert!(
        buffer::tombstone_present(&root),
        "the tombstone must survive"
    );
    if readonly_worked {
        assert!(result.is_err(), "a failed truncate must be reported");
    }
    assert!(
        buffer::drain(&root).is_empty(),
        "a tombstoned buffer must never drain, wipe failure or not"
    );
    assert!(
        buffer::append(
            &root,
            &buffer_path,
            "{\"event\":\"session_start\",\"source\":\"api\"}"
        )
        .is_none(),
        "a tombstoned buffer must never accept an append"
    );
}

#[cfg(unix)]
fn make_read_only(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    // Root ignores the mode bits, so this fixture cannot be relied on there.
    if geteuid_is_root() {
        return false;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).is_ok()
}

#[cfg(unix)]
fn geteuid_is_root() -> bool {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}

#[cfg(not(unix))]
fn make_read_only(_path: &Path) -> bool {
    false
}

// --------------------------------------------------------------- endpoints --

#[test]
fn plain_http_is_rejected_except_on_loopback() {
    assert!(validate_endpoint("https://example.com/t").is_ok());
    assert_eq!(
        validate_endpoint("http://example.com/t"),
        Err(EndpointError::InsecureScheme)
    );
    assert!(validate_endpoint("http://127.0.0.1:9/x").is_ok());
    assert!(validate_endpoint("http://localhost:9/x").is_ok());
    assert!(validate_endpoint("http://[::1]:9/x").is_ok());
    assert_eq!(
        validate_endpoint("ftp://example.com/t"),
        Err(EndpointError::UnsupportedScheme)
    );
    assert_eq!(
        validate_endpoint("example.com"),
        Err(EndpointError::Unparseable)
    );
}

#[test]
fn no_environment_variable_can_authorize_plaintext() {
    // `CODEWHALE_ALLOW_INSECURE_HTTP` is a *provider* trust decision — it
    // permits an insecure model base URL for harnesses that intercept model
    // traffic. Honouring it here would let that decision also authorize
    // telemetry POSTs to an arbitrary host. No override of any kind exists.
    unsafe { std::env::set_var("CODEWHALE_ALLOW_INSECURE_HTTP", "1") };
    let with_env = validate_endpoint("http://example.com/t");
    unsafe { std::env::remove_var("CODEWHALE_ALLOW_INSECURE_HTTP") };
    assert_eq!(with_env, Err(EndpointError::InsecureScheme));
}

// -------------------------------------------------------- install identity --

#[test]
fn install_id_is_random_and_rotates() {
    let first_home = temp_home();
    let second_home = temp_home();
    let first_root = root_of(&first_home);
    let second_root = root_of(&second_home);

    let first = envelope::read_or_create_install_id(&first_root).expect("mint");
    let second = envelope::read_or_create_install_id(&second_root).expect("mint");
    assert_ne!(
        first.install_id, second.install_id,
        "two fresh homes must not share an id"
    );
    assert!(uuid::Uuid::parse_str(&first.install_id).is_ok());

    // Stable across reads on the same home.
    let again = envelope::read_or_create_install_id(&first_root).expect("re-read");
    assert_eq!(first.install_id, again.install_id);

    // Not a function of hostname, user, or path: nothing derivable appears in
    // the value, and two homes under the same user differ.
    for derived in [
        std::env::var("USER").unwrap_or_default(),
        std::env::var("HOME").unwrap_or_default(),
        first_root.display().to_string(),
    ] {
        if derived.trim().is_empty() {
            continue;
        }
        assert!(!first.install_id.contains(derived.trim()));
    }

    // 91 days old rotates.
    let stale = envelope::InstallId {
        schema_version: 1,
        install_id: first.install_id.clone(),
        rotated_at: (chrono::Utc::now() - chrono::Duration::days(91))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    codewhale_config::persistence::atomic_write_json(&buffer::install_id_path(&first_root), &stale)
        .expect("write stale id");
    let rotated = envelope::read_or_create_install_id(&first_root).expect("rotate");
    assert_ne!(rotated.install_id, first.install_id);
    assert_ne!(rotated.rotated_at, stale.rotated_at);
}

// ------------------------------------------------------------------ buffer --

fn line(n: usize) -> String {
    serde_json::to_string(&Event::Panic {
        site: format!("crates/tui/src/x.rs:{n}:1"),
    })
    .expect("serialize")
}

#[test]
fn ring_buffer_drops_oldest_at_cap_for_both_sinks() {
    let home = temp_home();
    let root = root_of(&home);
    for path in [buffer::buffer_path(&root), buffer::dryrun_path(&root)] {
        for n in 0..600 {
            buffer::append(&root, &path, &line(n)).expect("append");
        }
        let kept = buffer::read_lines(&path);
        assert_eq!(kept.len(), buffer::MAX_EVENTS, "{}", path.display());
        assert_eq!(
            kept.first().expect("first"),
            &line(600 - buffer::MAX_EVENTS)
        );
        assert_eq!(kept.last().expect("last"), &line(599));
    }
}

#[test]
fn probe_threshold_cannot_hide_an_over_cap_buffer() {
    // The append path skips the count probe below a byte threshold. That is
    // only safe if `MAX_EVENTS` lines cannot fit under it.
    let shortest = serde_json::to_string(&Event::SessionStart {
        source: SessionSource::Api,
    })
    .expect("serialize");
    let floor = (shortest.len() as u64 + 1) * buffer::MAX_EVENTS as u64;
    assert!(
        floor > 4096,
        "the shortest event is now small enough that {} of them fit under the probe threshold",
        buffer::MAX_EVENTS
    );
}

#[test]
fn a_line_over_pipe_buf_is_dropped_not_split() {
    let home = temp_home();
    let root = root_of(&home);
    let path = buffer::buffer_path(&root);
    let huge = format!("{{\"pad\":\"{}\"}}", "x".repeat(buffer::MAX_LINE_BYTES));
    assert!(buffer::append(&root, &path, &huge).is_none());
    assert!(buffer::read_lines(&path).is_empty());
}

#[test]
fn drain_skips_a_torn_trailing_line() {
    let home = temp_home();
    let root = root_of(&home);
    let path = buffer::buffer_path(&root);
    buffer::append(&root, &path, &line(1)).expect("append");
    buffer::append(&root, &path, &line(2)).expect("append");
    // `std::process::exit` on the signal path can cut a concurrent write.
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        file.write_all(b"{\"event\":\"pan").expect("tear");
    }
    let drained = buffer::drain(&root);
    assert_eq!(drained.len(), 3, "the drain returns raw lines");
    let parsed: Vec<Event> = drained
        .iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert_eq!(parsed.len(), 2, "the torn line must not reach a batch");
    assert!(buffer::read_lines(&path).is_empty(), "drain truncates");
}

#[test]
fn append_drops_without_blocking_on_a_held_privacy_lock() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    let path = buffer::buffer_path(&root);

    let held = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let holder_root = root.clone();
    let holder_held = held.clone();
    let holder_release = release.clone();
    let holder = std::thread::spawn(move || {
        buffer::with_lock(&holder_root, || {
            holder_held.wait();
            holder_release.wait();
            Ok(())
        })
    });

    held.wait();
    let started = Instant::now();
    let outcome = buffer::append(&root, &path, &line(7));
    let elapsed = started.elapsed();
    release.wait();
    holder.join().expect("holder thread").expect("holder lock");

    assert!(outcome.is_none(), "a contended append must be dropped");
    assert!(
        elapsed < Duration::from_millis(250),
        "an append waited {elapsed:?} on the privacy lock"
    );
    assert!(
        buffer::read_lines(&path).is_empty(),
        "the panic-safe path bypassed the privacy lock"
    );
}

#[test]
fn a_tombstoned_buffer_never_appends_or_drains() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    buffer::append(&root, &buffer::buffer_path(&root), &line(1)).expect("append");
    buffer::wipe(&root).expect("wipe");

    assert!(buffer::append(&root, &buffer::buffer_path(&root), &line(2)).is_none());
    assert!(buffer::append_locked(&root, &buffer::dryrun_path(&root), "{}").is_none());
    assert!(buffer::drain(&root).is_empty());
}

#[test]
fn arming_truncates_a_pre_consent_buffer() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    buffer::append(&root, &buffer::buffer_path(&root), &line(1)).expect("append");
    buffer::wipe(&root).expect("wipe");

    let generation = buffer::tombstone_generation(&root).expect("read wipe generation");
    buffer::arm(&root, generation.as_ref(), || true).expect("arm");
    assert!(!buffer::tombstone_present(&root));
    assert!(buffer::read_lines(&buffer::buffer_path(&root)).is_empty());
}

#[test]
fn stale_consent_cannot_clear_a_newer_opt_out_but_fresh_reenable_can() {
    let home = temp_home();
    let root = root_of(&home);
    let config_path = home.path().join("config.toml");
    let setup_path = home.path().join("setup_state.json");
    accepted_setup()
        .save_to(&setup_path)
        .expect("write accepted setup state");

    // This process resolved the old enabled config before another process
    // persisted an opt-out and completed its wipe.
    let stale_resolved = resolved(true, false, None);
    let TelemetryDecision::Enabled(pre_wipe_consent) = decide_in_home(
        Some(home.path()),
        &stale_resolved,
        &accepted_setup(),
        Surface::Tui,
    ) else {
        panic!("pre-wipe enabled facts must produce consent");
    };
    std::fs::write(&config_path, "telemetry = false\n").expect("persist opt-out");
    buffer::ensure_dir(&root).expect("create telemetry root");
    buffer::wipe(&root).expect("complete newer wipe");
    assert!(
        buffer::arm(&root, pre_wipe_consent.tombstone_generation(), || true).is_err(),
        "an old consent token cleared a newer tombstone generation"
    );
    assert!(buffer::tombstone_present(&root));

    // Even the difficult ordering — stale config facts combined with the new
    // tombstone generation — cannot arm, because arm re-reads the durable
    // predicate while holding the wipe lock.
    let TelemetryDecision::Enabled(stale_consent) = decide_in_home(
        Some(home.path()),
        &stale_resolved,
        &accepted_setup(),
        Surface::Tui,
    ) else {
        panic!("fixture must carry stale enabled facts");
    };
    assert!(
        buffer::arm(&root, stale_consent.tombstone_generation(), || {
            permission_still_enabled_in_home(
                Some(&config_path),
                &setup_path,
                Some(home.path()),
                &root,
            )
        })
        .is_err(),
        "stale consent cleared a completed opt-out"
    );
    assert!(buffer::tombstone_present(&root));

    // The documented explicit re-enable updates the durable register first. A
    // fresh consent observes both that value and the current generation, so it
    // may clear exactly that tombstone.
    std::fs::write(&config_path, "telemetry = true\n").expect("persist re-enable");
    let TelemetryDecision::Enabled(fresh_consent) = decide_in_home(
        Some(home.path()),
        &resolved(true, false, None),
        &accepted_setup(),
        Surface::Tui,
    ) else {
        panic!("fresh re-enable must produce consent");
    };
    buffer::arm(&root, fresh_consent.tombstone_generation(), || {
        permission_still_enabled_in_home(Some(&config_path), &setup_path, Some(home.path()), &root)
    })
    .expect("fresh re-enable arms");
    assert!(!buffer::tombstone_present(&root));
}

#[test]
fn completed_wipe_blocks_identity_and_state_recreation() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create telemetry root");
    envelope::read_or_create_install_id(&root).expect("seed install id");
    envelope::write_state(&root, &envelope::TelemetryState::default()).expect("seed state");
    buffer::wipe(&root).expect("wipe telemetry home");

    assert!(
        envelope::read_or_create_install_id(&root).is_err(),
        "an in-flight flush recreated the deleted install id"
    );
    assert!(
        envelope::write_state(&root, &envelope::TelemetryState::default()).is_err(),
        "an in-flight flush recreated state after opt-out"
    );
    assert!(!buffer::install_id_path(&root).exists());
    assert!(!buffer::state_path(&root).exists());
}

// ------------------------------------------------------------ unarmed gate --

#[test]
fn record_blocking_is_a_noop_when_unarmed() {
    // The process panic hook is installed before the command line is parsed, so
    // this is the state the hook runs in for every user who never opted in.
    let home = temp_home();
    let root = root_of(&home);
    assert!(!crate::is_armed());
    crate::record_blocking(Event::Panic {
        site: "crates/tui/src/x.rs:1:1".to_string(),
    });
    crate::record(Event::SessionStart {
        source: SessionSource::Interactive,
    });
    crate::set_exit_class(ExitClass::Panic);
    assert_eq!(crate::exit_class(), ExitClass::Clean);
    assert!(
        !root.exists(),
        "an unarmed process must create no directory"
    );
}

// ------------------------------------------------------------------ client --

#[test]
fn endpoint_unset_writes_the_dry_run_sink() {
    let home = temp_home();
    let root = root_of(&home);
    let batch = every_field_batch();
    assert_eq!(
        crate::client::send(&root, None, &batch),
        crate::client::SendOutcome::DryRun
    );
    let written = buffer::read_lines(&buffer::dryrun_path(&root));
    assert_eq!(written.len(), 1);
    let round_tripped: Batch = serde_json::from_str(&written[0]).expect("parse dry-run batch");
    assert_eq!(round_tripped, batch);
    assert!(
        !buffer::buffer_path(&root).exists(),
        "the dry-run sink is a separate file from the pending buffer"
    );
}

#[test]
fn a_tombstoned_home_sends_nothing_even_with_an_endpoint() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create root");
    buffer::wipe(&root).expect("wipe");
    // The tombstone check fires before any client is constructed, so this
    // asserts on the sink rather than on network timing.
    assert_eq!(
        crate::client::send(&root, Some("http://127.0.0.1:1/t"), &every_field_batch()),
        crate::client::SendOutcome::Dropped
    );
    assert!(buffer::read_lines(&buffer::dryrun_path(&root)).is_empty());
}

#[test]
fn wipe_and_delivery_share_one_ordering_boundary() {
    let home = temp_home();
    let root = root_of(&home);
    let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));

    let send_root = root.clone();
    let send_entered = entered.clone();
    let send_release = release.clone();
    let send = std::thread::spawn(move || {
        crate::client::send_with_transport(
            &send_root,
            Some("https://telemetry.codewhale.ai/v1/batch"),
            &every_field_batch(),
            move |_, _, _| {
                send_entered.wait();
                send_release.wait();
                crate::client::SendOutcome::Accepted
            },
        )
    });
    entered.wait();

    // The real send path is paused inside its transport callback. A
    // non-blocking probe must observe the same lock that wipe takes; this
    // deterministically pins the entire delivery inside the boundary without
    // depending on loopback networking in a restricted test sandbox.
    assert!(
        buffer::try_with_lock(&root, || Ok(()))
            .expect("probe privacy lock")
            .is_none(),
        "network delivery did not hold the wipe lock"
    );

    // Start the real blocking wipe while the POST is still in flight. It can
    // only complete after the response releases the sender's privacy guard.
    let wipe_root = root.clone();
    let wipe = std::thread::spawn(move || buffer::wipe(&wipe_root));
    release.wait();
    assert_eq!(
        send.join().expect("send thread"),
        crate::client::SendOutcome::Accepted
    );

    wipe.join()
        .expect("wipe thread")
        .expect("wipe after delivery");
    assert!(buffer::tombstone_present(&root));
    assert_eq!(
        crate::client::send(&root, Some("http://127.0.0.1:1/t"), &every_field_batch()),
        crate::client::SendOutcome::Dropped,
        "a send crossed the completed wipe boundary"
    );
}

// ----------------------------------------------------------------- buckets --

#[test]
fn buckets_are_half_open_at_every_boundary() {
    assert_eq!(DurationBucket::from_secs(0), DurationBucket::Lt1m);
    assert_eq!(DurationBucket::from_secs(59), DurationBucket::Lt1m);
    assert_eq!(DurationBucket::from_secs(60), DurationBucket::OneToTen);
    assert_eq!(DurationBucket::from_secs(599), DurationBucket::OneToTen);
    assert_eq!(DurationBucket::from_secs(600), DurationBucket::TenToSixty);
    assert_eq!(DurationBucket::from_secs(3599), DurationBucket::TenToSixty);
    assert_eq!(DurationBucket::from_secs(3600), DurationBucket::Gt60m);

    assert_eq!(ColdStartBucket::from_millis(249), ColdStartBucket::Lt250);
    assert_eq!(ColdStartBucket::from_millis(250), ColdStartBucket::Mid);
    assert_eq!(ColdStartBucket::from_millis(999), ColdStartBucket::Mid);
    assert_eq!(ColdStartBucket::from_millis(1000), ColdStartBucket::Slow);
    assert_eq!(ColdStartBucket::from_millis(2999), ColdStartBucket::Slow);
    assert_eq!(ColdStartBucket::from_millis(3000), ColdStartBucket::Gte3000);

    let mut wall = TurnWall::default();
    for secs in [0, 4, 5, 29, 30, 119, 120, 10_000] {
        wall.observe_secs(secs);
    }
    assert_eq!(wall.lt_5s, 2);
    assert_eq!(wall.five_to_thirty, 2);
    assert_eq!(wall.thirty_to_onetwenty, 2);
    assert_eq!(wall.gte_120s, 2);
}

#[test]
fn exit_class_round_trips_through_the_atomic_encoding() {
    for class in ExitClass::ALL {
        assert_eq!(ExitClass::from_u8(class.as_u8()), *class);
    }
    // An exit code is never the source: 130 is both a cancelled turn and SIGINT.
    assert_eq!(ExitClass::from_u8(130), ExitClass::Clean);
}

// ---------------------------------------------------------------- counters --

#[test]
fn custom_provider_emits_literal_custom() {
    let counters = crate::SessionCounters::default();
    counters.record_provider(codewhale_config::ProviderKind::Custom);
    counters.record_provider(codewhale_config::ProviderKind::Deepseek);
    counters.record_provider(codewhale_config::ProviderKind::Custom);
    let providers = counters.providers();
    assert_eq!(
        providers,
        vec!["custom".to_string(), "deepseek".to_string()]
    );
}

#[test]
fn counter_bumps_land_in_the_named_field() {
    let counters = crate::SessionCounters::default();
    counters.bump(crate::Counter::Turns);
    counters.bump(crate::Counter::Turns);
    counters.bump(crate::Counter::CommandPaletteOpen);
    counters.bump_error(crate::ErrorCounter::ProviderHttp5xx);
    counters.observe_turn_secs(3);

    let snapshot = counters.counters();
    assert_eq!(snapshot.turns, 2);
    assert_eq!(snapshot.command_palette_open, 1);
    assert_eq!(snapshot.tool_calls, 0);
    assert_eq!(counters.errors().provider_http_5xx, 1);
    assert_eq!(counters.turn_wall().lt_5s, 1);
}

#[test]
fn http_status_maps_to_the_class_counter_and_nothing_else() {
    assert_eq!(
        crate::counters::http_status_counter(404),
        Some(crate::ErrorCounter::ProviderHttp4xx)
    );
    assert_eq!(
        crate::counters::http_status_counter(503),
        Some(crate::ErrorCounter::ProviderHttp5xx)
    );
    assert_eq!(crate::counters::http_status_counter(200), None);
    assert_eq!(crate::counters::http_status_counter(302), None);
}

// --------------------------------------------------------------- API shape --

#[test]
fn no_public_api_accepts_a_bare_bool() {
    // `init` takes a `TelemetryConsent` **by value**, and `TelemetryConsent` has
    // no public constructor other than `decide`. This is a shape assertion: it
    // stops compiling if the signature is ever widened.
    let init: fn(crate::TelemetryConsent) = crate::init;
    let _ = init;

    // The only source of one is `decide`, which still applies every persistent
    // and run-scoped opt-out before constructing the capability.
    let home = temp_home();
    assert!(
        decide_in_home(
            Some(home.path()),
            &resolved(true, false, None),
            &accepted_setup(),
            Surface::Cli
        )
        .is_enabled()
    );
}

// ------------------------------------------------- docs and code are welded --

const TELEMETRY_DOC: &str = include_str!("../../../docs/TELEMETRY.md");
const GOLDEN_V4: &str = include_str!("../tests/golden/v4.json");

/// Extract the fenced ```jsonc blocks from the schema doc, in order.
fn jsonc_blocks(doc: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for raw in doc.lines() {
        let line = raw.trim_end();
        match current.as_mut() {
            None => {
                if line.trim() == "```jsonc" {
                    current = Some(String::new());
                }
            }
            Some(body) => {
                if line.trim() == "```" {
                    blocks.push(std::mem::take(body));
                    current = None;
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            }
        }
    }
    blocks
}

/// Every `"name":` key in a jsonc block, including nested objects. Values are
/// never matched: a key is an identifier-shaped string followed by a colon, and
/// no value in these blocks has that shape.
fn documented_keys(block: &str) -> std::collections::BTreeSet<String> {
    let bytes: Vec<char> = block.chars().collect();
    let mut keys = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != '"' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end] != '"' {
            end += 1;
        }
        if end >= bytes.len() {
            break;
        }
        let candidate: String = bytes[start..end].iter().collect();
        let mut after = end + 1;
        while after < bytes.len() && bytes[after] == ' ' {
            after += 1;
        }
        let is_key = after < bytes.len() && bytes[after] == ':';
        let identifier_shaped = !candidate.is_empty()
            && candidate
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if is_key && identifier_shaped {
            keys.insert(candidate);
        }
        index = end + 1;
    }
    keys
}

/// Every key of a serialized value, including nested objects.
fn serialized_keys(value: &Value) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    fn walk(value: &Value, out: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    out.insert(key.clone());
                    walk(item, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut keys);
    keys
}

/// First-column entries of the markdown table that follows `heading`.
fn table_first_column(doc: &str, heading: &str) -> Vec<String> {
    let mut lines = doc.lines().skip_while(|line| line.trim() != heading);
    let mut rows = Vec::new();
    let mut in_table = false;
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            if in_table {
                break;
            }
            continue;
        }
        in_table = true;
        let first = trimmed
            .trim_matches('|')
            .split('|')
            .next()
            .unwrap_or("")
            .trim();
        if first.is_empty() || first.chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        let name = first.trim_matches('`').to_string();
        if name.eq_ignore_ascii_case("field") || name.eq_ignore_ascii_case("file") {
            continue;
        }
        rows.push(name);
    }
    rows
}

#[test]
fn event_field_names_match_documented_schema() {
    let blocks = jsonc_blocks(TELEMETRY_DOC);
    assert_eq!(
        blocks.len(),
        9,
        "expected one jsonc block for the envelope and one per event variant; \
         a parse miss must fail rather than silently pass"
    );

    // The envelope.
    let documented = documented_keys(&blocks[0]);
    let declared: std::collections::BTreeSet<String> =
        Batch::FIELDS.iter().map(|f| (*f).to_string()).collect();
    assert_eq!(documented.len(), Batch::FIELDS.len());
    assert_eq!(
        documented, declared,
        "the batch envelope drifted from the doc"
    );

    // One block per event variant, in the order the doc presents them.
    let events = every_event();
    assert_eq!(events.len(), blocks.len() - 1);
    for (index, event) in events.iter().enumerate() {
        let block = &blocks[index + 1];
        let documented = documented_keys(block);
        let serialized = serialized_keys(&serde_json::to_value(event).expect("serialize"));
        assert!(
            !documented.is_empty(),
            "no keys parsed out of the {} block",
            event.name()
        );
        assert_eq!(
            documented,
            serialized,
            "the `{}` event drifted from the doc",
            event.name()
        );
    }

    // The envelope table, row for row.
    let envelope_rows =
        table_first_column(TELEMETRY_DOC, "### Batch envelope — sent on every POST");
    assert_eq!(
        envelope_rows.len(),
        Batch::FIELDS.len(),
        "the envelope table lost or gained a row: {envelope_rows:?}"
    );
    assert_eq!(
        envelope_rows,
        Batch::FIELDS
            .iter()
            .map(|f| (*f).to_string())
            .collect::<Vec<_>>()
    );

    // The counters and errors tables, which are the two closed field sets a
    // contributor is most likely to extend without touching the doc.
    let counter_rows = table_first_column(
        TELEMETRY_DOC,
        "**`counters`** — closed field set. Every bump happens at the **call site**, never inside a conditionally-entered handler:",
    );
    assert_eq!(
        counter_rows,
        Counters::FIELDS
            .iter()
            .map(|f| (*f).to_string())
            .collect::<Vec<_>>(),
        "the counters table drifted from `Counters`"
    );
    let error_rows = table_first_column(
        TELEMETRY_DOC,
        "**`errors`** — closed field set. Every value is a **variant discriminant**, never `err.to_string()`:",
    );
    assert_eq!(
        error_rows,
        Errors::FIELDS
            .iter()
            .map(|f| (*f).to_string())
            .collect::<Vec<_>>(),
        "the errors table drifted from `Errors`"
    );
}

#[test]
fn golden_payload_v4() {
    assert_eq!(NOTICE_VERSION.to_string(), TELEMETRY_NOTICE_VERSION);
    // `crates/telemetry/tests/golden/v4.json` is one fully-populated instance of
    // the envelope and every event. Any field add, remove, or retype fails here
    // until the developer re-blesses it under a bumped `SCHEMA_VERSION` — and it
    // is also the artifact a future receiver author reads to know exactly what
    // v1 was.
    //
    // Re-bless with: `CODEWHALE_BLESS_TELEMETRY_GOLDEN=1 cargo test -p codewhale-telemetry`
    let batch = every_field_batch();
    assert_eq!(
        batch.schema_version, SCHEMA_VERSION,
        "the fixture must be built at the current schema version"
    );
    let mut rendered = serde_json::to_string_pretty(&batch).expect("serialize");
    rendered.push('\n');

    if std::env::var("CODEWHALE_BLESS_TELEMETRY_GOLDEN").is_ok() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/v4.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create golden dir");
        std::fs::write(&path, &rendered).expect("write golden");
        return;
    }

    assert_eq!(
        rendered, GOLDEN_V4,
        "the v4 payload changed; bump SCHEMA_VERSION and re-bless the golden file"
    );
}

#[test]
fn version_comparison_names_install_upgrade_and_downgrade() {
    assert!(crate::version_is_older("0.9.3", "0.9.4"));
    assert!(crate::version_is_older("0.9", "0.9.4"));
    assert!(crate::version_is_older("0.10.0", "1.0.0"));
    assert!(!crate::version_is_older("0.9.4", "0.9.4"));
    assert!(!crate::version_is_older("0.9.5", "0.9.4"));
    // A pre-release suffix is not part of the ordering question being asked.
    assert!(!crate::version_is_older("0.9.4-rc.1", "0.9.4"));
    // Unparseable segments read as zero, so an unknown version never invents an
    // upgrade that did not happen.
    assert!(!crate::version_is_older("nightly", "0.0.0"));
}

#[test]
fn an_install_or_upgrade_is_reported_once_per_version() {
    let home = temp_home();
    let root = root_of(&home);
    buffer::ensure_dir(&root).expect("create telemetry dir");

    // No prior record on this machine.
    let mut state = envelope::read_state(&root);
    assert_eq!(state.last_version, None);

    // The state file is written before the event is queued, so the second
    // launch at the same version has nothing left to report.
    state.last_version = Some(env!("CARGO_PKG_VERSION").to_string());
    envelope::write_state(&root, &state).expect("write state");
    assert_eq!(
        envelope::read_state(&root).last_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    // The previous version is read from this file and from nowhere else —
    // never from session history or config mtimes, which answer the same
    // question under a different privacy contract.
    let entries: Vec<String> = std::fs::read_dir(&root)
        .expect("read dir")
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    assert!(
        entries.iter().any(|name| name == "state.json"),
        "expected state.json in {entries:?}"
    );
}

#[test]
fn the_notice_summarizes_what_the_schema_collects_and_states_every_red_line() {
    use crate::notice;

    let body = notice::NOTICE_BODY;

    // The modal names the useful product categories and links the exact
    // field-by-field schema. `install_id` is "a random ID stored on this
    // machine"; transport metadata remains in the linked document. The body
    // wraps at 72 columns, so multi-word claims are matched across the
    // reflowed whitespace.
    let flat: String = body.split_whitespace().collect();
    for claim in [
        "version",
        "OS and CPU family",
        "session duration and outcome",
        "aggregate feature and error counters",
        "random ID stored on this machine",
        "every 90 days",
        "on by default",
        "PostHog",
    ] {
        assert!(
            flat.contains(&claim.split_whitespace().collect::<String>()),
            "the notice does not describe: {claim}"
        );
    }

    // And every red line has to be stated as *not collected*, not as
    // anonymized or sampled — two promises this client does not make.
    for red_line in [
        "conversations",
        "code",
        "prompts",
        "files",
        "repo or branch names",
        "model content",
        "credentials",
        "per-turn or per-tool timeline",
    ] {
        assert!(
            flat.contains(&red_line.split_whitespace().collect::<String>()),
            "the notice does not disclaim: {red_line}"
        );
    }
    assert!(!body.to_ascii_lowercase().contains("anonymized"));

    // The modal names the persistent opt-out because that is the switch that
    // also fulfils its deletion promise. Run-only kill switches stay in the
    // linked schema document, which explains that they erase nothing.
    assert!(body.contains("codewhale config set telemetry false"));
    assert!(!body.contains("CODEWHALE_TELEMETRY=0"));
    assert!(body.contains("docs/TELEMETRY.md"));
}
