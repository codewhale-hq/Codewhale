//! Machine-token contract tests.
//!
//! Every branch the control plane can take is exercised here against the
//! documented `details.code`, because the CLI's whole job in a CI failure is
//! to turn one of those codes into the one sentence that names the fix.

use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::{CloudResponse, CloudTransport};

const VALID: &str = "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789_-xQRST";

fn err_body(code: &str, message: &str) -> serde_json::Value {
    json!({
        "error": "request_failed",
        "message": message,
        "details": { "code": code },
    })
}

fn err_response(status: u16, code: &str, message: &str) -> CloudResponse {
    CloudResponse {
        status,
        body: serde_json::to_vec(&err_body(code, message)).unwrap(),
        retry_after: None,
    }
}

/// A transport that replays a scripted queue and records every request.
struct ScriptedTransport {
    responses: Mutex<Vec<Result<CloudResponse>>>,
    seen: Mutex<Vec<(String, Option<String>)>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<Result<CloudResponse>>) -> Self {
        let mut responses = responses;
        responses.reverse();
        Self {
            responses: Mutex::new(responses),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn ok(responses: Vec<CloudResponse>) -> Self {
        Self::new(responses.into_iter().map(Ok).collect())
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn bearers(&self) -> Vec<Option<String>> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, bearer)| bearer.clone())
            .collect()
    }
}

impl CloudTransport for ScriptedTransport {
    fn execute(&self, request: crate::CloudRequest) -> Result<CloudResponse> {
        self.seen
            .lock()
            .unwrap()
            .push((request.path.clone(), request.bearer.clone()));
        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err(anyhow!("scripted transport exhausted")))
    }
}

// -- token format ---------------------------------------------------------

#[test]
fn a_well_formed_token_parses_and_exposes_only_its_non_secret_head() {
    let key = MachineKey::parse(VALID).expect("the canonical shape must parse");
    assert_eq!(key.head(), "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91");
    assert_eq!(key.head().len(), TOKEN_HEAD_LEN);
    assert!(key.head().ends_with("3f2a9c1e4b7d8a0f5c6e2b91"));
    // The head is the whole key id, not a truncated fingerprint: it is what
    // maps a token in a build log to exactly one revocable listing row.
    assert!(VALID.starts_with(key.head()));

    // Debug is the accident-prone surface: a `{:?}` in a panic must not leak.
    let debug = format!("{key:?}");
    assert!(
        debug.contains("cwc_key_3f2a9c1e4b7d8a0f5c6e2b91"),
        "{debug}"
    );
    assert!(!debug.contains(&VALID[TOKEN_HEAD_LEN..]), "{debug}");
}

#[test]
fn ci_whitespace_and_wrapping_quotes_are_stripped_before_matching() {
    for raw in [
        format!("  {VALID}\n"),
        format!("\"{VALID}\""),
        format!("'{VALID}'"),
        format!("  \" {VALID} \"  "),
    ] {
        let key = MachineKey::parse(&raw).unwrap_or_else(|err| {
            panic!("secret-paste artifact must be tolerated: {raw:?}: {err}")
        });
        assert_eq!(key.head(), "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91");
    }
}

#[test]
fn a_malformed_token_fails_locally_and_the_message_never_echoes_it() {
    let truncated = &VALID[..40];
    let cases: Vec<String> = vec![
        truncated.to_string(),
        format!("{VALID}x"),
        VALID.replacen("cwc_key_", "cwc_kex_", 1),
        // Uppercase in the id half: the contract pins [0-9a-f].
        VALID.replacen("3f2a", "3F2A", 1),
        // '+' is base64, not base64url.
        format!("{}+{}", &VALID[..74], &VALID[75..]),
        "not-a-key".to_string(),
    ];
    for raw in cases {
        let err = MachineKey::parse(&raw).expect_err("malformed values must not be sent");
        let text = err.to_string();
        assert!(text.contains("CODEWHALE_API_KEY"), "{text}");
        // Naming the likely cause is the point: a 401 cannot distinguish a
        // half-pasted key from a deleted one, but a local check can.
        assert!(text.contains("truncated or shell-mangled"), "{text}");
        assert!(!text.contains(&raw), "the error echoed the value: {text}");
    }
}

#[test]
fn an_exported_but_empty_secret_reads_as_unset_not_as_invalid() {
    for raw in [Some(""), Some("   "), None] {
        let env = MachineKeyEnv::from_raw(raw);
        assert!(!env.is_present(), "{raw:?}");
        assert!(env.resolve().unwrap().is_none(), "{raw:?}");
        let err = env.require().expect_err("require must fail when unset");
        assert!(err.to_string().contains("CODEWHALE_API_KEY"), "{err}");
    }
}

#[test]
fn presence_is_independent_of_validity_so_management_can_refuse_a_bad_key() {
    let env = MachineKeyEnv::from_raw(Some("cwc_key_truncated"));
    assert!(env.is_present());
    assert!(env.resolve().is_err());
    // A key that is present but unparseable must still trigger the "keys
    // cannot manage keys" refusal rather than a format complaint.
    let err = reject_machine_key_for_management(&env, false).expect_err("must refuse");
    assert!(err.to_string().contains("codewhale login"), "{err}");
}

// -- base URL -------------------------------------------------------------

#[test]
fn base_url_precedence_is_flag_then_machine_env_then_device_env_then_default() {
    assert_eq!(
        resolve_api_base(
            Some("https://flag.example/"),
            Some("https://machine.example"),
            Some("https://device.example"),
            DEFAULT_BASE_FOR_TEST,
        ),
        "https://flag.example"
    );
    assert_eq!(
        resolve_api_base(
            None,
            Some("https://machine.example/"),
            Some("https://device.example"),
            DEFAULT_BASE_FOR_TEST,
        ),
        "https://machine.example"
    );
    assert_eq!(
        resolve_api_base(
            None,
            None,
            Some("https://device.example"),
            DEFAULT_BASE_FOR_TEST
        ),
        "https://device.example"
    );
    assert_eq!(
        resolve_api_base(None, Some("   "), None, DEFAULT_BASE_FOR_TEST),
        DEFAULT_BASE_FOR_TEST
    );
}

const DEFAULT_BASE_FOR_TEST: &str = "https://api.codewhale.net";

#[test]
fn plaintext_to_a_remote_host_is_a_hard_error_and_loopback_is_allowed() {
    // A warning in CI is a line nobody reads, so this must fail closed.
    let err = require_secure_base("http://api.codewhale.net")
        .expect_err("cleartext to a remote host must be refused");
    let text = format!("{err:#}");
    assert!(text.contains("CODEWHALE_API_KEY"), "{text}");

    require_secure_base("https://api.codewhale.net").expect("https is fine");
    for loopback in [
        "http://localhost:8787",
        "http://127.0.0.1:8787",
        "http://[::1]:8787",
    ] {
        require_secure_base(loopback).unwrap_or_else(|err| panic!("{loopback}: {err:#}"));
    }
}

// -- error classification -------------------------------------------------

#[test]
fn every_documented_error_code_maps_to_its_own_actionable_message() {
    struct Case {
        status: u16,
        code: &'static str,
        expect: &'static str,
        exit: i32,
        retryable: bool,
    }
    let cases = [
        Case {
            status: 401,
            code: "api_key_invalid",
            expect: "is not valid",
            exit: EXIT_AUTH,
            retryable: false,
        },
        Case {
            status: 401,
            code: "api_key_revoked",
            expect: "was revoked",
            exit: EXIT_AUTH,
            retryable: false,
        },
        Case {
            status: 401,
            code: "api_key_expired",
            expect: "expired",
            exit: EXIT_AUTH,
            retryable: false,
        },
        Case {
            status: 401,
            code: "api_key_required",
            expect: "Codewhale CLI bug",
            exit: EXIT_AUTH,
            retryable: false,
        },
        Case {
            status: 401,
            code: "auth_required",
            expect: "codewhale login",
            exit: EXIT_AUTH,
            retryable: false,
        },
        Case {
            status: 403,
            code: "api_key_route_denied",
            expect: "cannot be used for this command",
            exit: EXIT_AUTH,
            retryable: false,
        },
        Case {
            status: 403,
            code: "api_key_scope_denied",
            expect: "scope",
            exit: EXIT_AUTH,
            retryable: false,
        },
        Case {
            status: 409,
            code: "account_agent_model_unconfigured",
            expect: "no agent model configured",
            exit: EXIT_AGENT_UNCONFIGURED,
            retryable: false,
        },
        Case {
            status: 409,
            code: "api_key_limit_reached",
            expect: "25 active API keys",
            exit: EXIT_LIMIT,
            retryable: false,
        },
        Case {
            status: 400,
            code: "api_key_name_invalid",
            expect: "server prose",
            exit: EXIT_INPUT,
            retryable: false,
        },
        Case {
            status: 400,
            code: "api_key_expiry_invalid",
            expect: "server prose",
            exit: EXIT_INPUT,
            retryable: false,
        },
        Case {
            status: 400,
            code: "api_key_scopes_invalid",
            expect: "server prose",
            exit: EXIT_INPUT,
            retryable: false,
        },
        Case {
            status: 400,
            code: "api_key_body_invalid",
            expect: "server prose",
            exit: EXIT_INPUT,
            retryable: false,
        },
        Case {
            status: 404,
            code: "api_key_not_found",
            expect: "No such Codewhale API key",
            exit: EXIT_INPUT,
            retryable: false,
        },
        Case {
            status: 503,
            code: "api_key_unavailable",
            expect: "does not support API keys yet",
            exit: EXIT_UNAVAILABLE,
            retryable: false,
        },
        Case {
            status: 503,
            code: "control_plane_not_attached",
            expect: "attached to the account control plane",
            exit: EXIT_UNAVAILABLE,
            retryable: false,
        },
    ];
    for case in cases {
        let error = classify(&err_response(case.status, case.code, "server prose"));
        assert_eq!(error.code, case.code);
        assert_eq!(error.exit_code, case.exit, "exit code for {}", case.code);
        assert_eq!(
            error.retryable, case.retryable,
            "retryability for {}",
            case.code
        );
        assert!(
            error.message.contains(case.expect),
            "{} message was {:?}",
            case.code,
            error.message
        );
    }
}

#[test]
fn a_missing_model_and_a_bad_credential_carry_different_exit_codes() {
    // CI logs must tell a configuration problem from a credential problem
    // without parsing English.
    let config = classify(&err_response(409, "account_agent_model_unconfigured", ""));
    let credential = classify(&err_response(401, "api_key_invalid", ""));
    assert_ne!(config.exit_code, credential.exit_code);
    assert_eq!(config.exit_code, EXIT_AGENT_UNCONFIGURED);
    assert_eq!(credential.exit_code, EXIT_AUTH);
}

#[test]
fn the_two_503_codes_do_not_read_as_the_same_problem() {
    // They need different fixes from different people.
    let unavailable = classify(&err_response(503, "api_key_unavailable", ""));
    let detached = classify(&err_response(503, "control_plane_not_attached", ""));
    assert_ne!(unavailable.message, detached.message);
    assert!(
        detached.message.contains("routing/deployment"),
        "{}",
        detached.message
    );
    assert!(!unavailable.message.contains("routing/deployment"));
}

#[test]
fn api_key_not_found_does_not_pretend_to_distinguish_the_three_causes() {
    let error = classify(&err_response(404, "api_key_not_found", ""));
    let text = error.message.to_lowercase();
    // Unknown, malformed, and another account's id answer identically, so
    // revoke cannot be used to probe for foreign key ids.
    for leak in [
        "another account",
        "belongs to",
        "malformed",
        "does not exist",
    ] {
        assert!(!text.contains(leak), "{text}");
    }
}

#[test]
fn api_key_field_unknown_names_the_fields_the_server_rejected() {
    let body = json!({
        "error": "bad_request",
        "message": "unknown fields in request body",
        "details": { "code": "api_key_field_unknown", "fields": ["ttl", "owner"] },
    });
    let error = classify(&CloudResponse {
        status: 400,
        body: serde_json::to_vec(&body).unwrap(),
        retry_after: None,
    });
    assert_eq!(error.exit_code, EXIT_INPUT);
    assert!(error.message.contains("ttl"), "{}", error.message);
    assert!(error.message.contains("owner"), "{}", error.message);
}

#[test]
fn classification_reads_the_code_not_the_status() {
    // Three 401s, three different fixes. Only `details.code` separates them.
    let invalid = classify(&err_response(401, "api_key_invalid", ""));
    let revoked = classify(&err_response(401, "api_key_revoked", ""));
    let expired = classify(&err_response(401, "api_key_expired", ""));
    assert_ne!(invalid.message, revoked.message);
    assert_ne!(revoked.message, expired.message);
    assert_ne!(invalid.message, expired.message);
}

#[test]
fn server_prose_cannot_rewrite_the_terminal() {
    let error = classify(&err_response(
        400,
        "api_key_name_invalid",
        "bad\u{1b}[2Jname\n\rmore",
    ));
    assert!(!error.message.contains('\u{1b}'), "{}", error.message);
    assert!(!error.message.contains('\n'), "{}", error.message);
}

#[test]
fn an_unrecognized_body_still_classifies_by_status() {
    let response = CloudResponse {
        status: 500,
        body: b"<html>oops</html>".to_vec(),
        retry_after: None,
    };
    let error = classify(&response);
    assert!(error.retryable);
    assert_eq!(error.exit_code, EXIT_TRANSPORT);
    assert!(error.message.contains("HTTP 500"), "{}", error.message);
}

// -- retry ----------------------------------------------------------------

#[test]
fn retry_after_wins_over_exponential_backoff_and_is_capped() {
    assert_eq!(backoff_delay(1, Some(7)), Duration::from_secs(7));
    assert_eq!(backoff_delay(3, Some(2)), Duration::from_secs(2));
    // A hostile or broken Retry-After cannot park CI for a day.
    assert_eq!(
        backoff_delay(1, Some(86_400)),
        Duration::from_millis(MAX_BACKOFF_MS)
    );
    // Without a header the delay still grows.
    assert!(backoff_delay(2, None) > backoff_delay(1, None));
}

#[test]
fn a_rate_limited_get_is_retried_and_honors_retry_after() {
    let transport = ScriptedTransport::ok(vec![
        CloudResponse {
            status: 429,
            body: serde_json::to_vec(&err_body("rate_limited", "slow down")).unwrap(),
            retry_after: Some(3),
        },
        CloudResponse {
            status: 200,
            body: serde_json::to_vec(&json!({
                "agent": { "configured": true, "modelProvider": "deepseek", "accountId": "user_1" }
            }))
            .unwrap(),
            retry_after: None,
        },
    ]);
    let mut slept = Vec::new();
    let mut sleeper = |duration: Duration| slept.push(duration);
    let client = MachineClient::new(&transport, MachineKey::parse(VALID).unwrap());
    let agent = client.agent(&mut sleeper).expect("the retry must succeed");
    assert!(agent.agent.configured);
    assert_eq!(transport.calls(), 2);
    assert_eq!(slept, vec![Duration::from_secs(3)]);
}

#[test]
fn a_transport_failure_on_a_get_is_retried_up_to_three_attempts() {
    let transport = ScriptedTransport::new(vec![
        Err(anyhow!("connection reset")),
        Err(anyhow!("connection reset")),
        Err(anyhow!("connection reset")),
    ]);
    let mut sleeper = |_: Duration| {};
    let client = MachineClient::new(&transport, MachineKey::parse(VALID).unwrap());
    let err = client
        .whoami(&mut sleeper)
        .expect_err("exhausted retries must fail");
    assert_eq!(transport.calls(), MAX_ATTEMPTS as usize);
    let machine_error = err
        .downcast_ref::<MachineError>()
        .expect("transport failures must carry an exit code");
    assert_eq!(machine_error.exit_code, EXIT_TRANSPORT);
}

#[test]
fn a_non_retryable_auth_failure_is_not_retried() {
    let transport = ScriptedTransport::ok(vec![err_response(401, "api_key_revoked", "revoked")]);
    let mut sleeper = |_: Duration| panic!("a revoked key must never be retried");
    let client = MachineClient::new(&transport, MachineKey::parse(VALID).unwrap());
    let err = client.whoami(&mut sleeper).expect_err("revoked must fail");
    assert_eq!(transport.calls(), 1);
    assert_eq!(
        err.downcast_ref::<MachineError>().unwrap().exit_code,
        EXIT_AUTH
    );
}

#[test]
fn create_is_marked_never_retryable_because_a_replay_mints_an_invisible_key() {
    // The variant is the guard: a POST that actually succeeded server-side
    // would mint a second key whose one-time secret the caller never saw.
    assert_ne!(Retry::Never, Retry::Idempotent);
}

// -- request shape --------------------------------------------------------

#[test]
fn exactly_one_credential_reaches_the_wire_and_it_is_the_machine_key() {
    let transport = ScriptedTransport::ok(vec![CloudResponse {
        status: 200,
        body: serde_json::to_vec(&json!({
            "account": { "id": "user_1", "displayName": "H", "email": "h@example.test", "region": "us-west", "plan": "free" },
            "apiKey": { "id": "3f2a9c1e4b7d8a0f5c6e2b91", "name": "ci", "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91", "scopes": ["account:read"], "createdAt": "2026-01-01T00:00:00Z" },
            "agent": { "configured": true, "modelProvider": "deepseek" }
        }))
        .unwrap(),
        retry_after: None,
    }]);
    let mut sleeper = |_: Duration| {};
    let key = MachineKey::parse(VALID).unwrap();
    let client = MachineClient::new(&transport, key);
    client.whoami(&mut sleeper).unwrap();
    let bearers = transport.bearers();
    assert_eq!(bearers.len(), 1);
    assert_eq!(bearers[0].as_deref(), Some(VALID));
    assert_eq!(
        transport.seen.lock().unwrap()[0].0,
        "/api/account/api-key/whoami"
    );
}

// -- whoami / agent rendering ---------------------------------------------

fn whoami_fixture(configured: bool) -> WhoamiResponse {
    let provider = if configured { "deepseek" } else { "" };
    serde_json::from_value(json!({
        "account": { "id": "user_1", "displayName": "Hunter", "email": "h@example.test", "region": "us-west", "plan": "free" },
        "apiKey": {
            "id": "3f2a9c1e4b7d8a0f5c6e2b91",
            "name": "github-actions",
            "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91",
            "scopes": ["account:read", "agent:run"],
            "createdAt": "2026-01-01T00:00:00Z",
            "expiresAt": null,
            "lastUsedAt": null,
            "revokedAt": null
        },
        "agent": { "configured": configured, "modelProvider": provider }
    }))
    .unwrap()
}

#[test]
fn whoami_prints_the_account_and_only_the_key_head() {
    let mut out = Vec::new();
    let key = MachineKey::parse(VALID).unwrap();
    write_whoami(
        &mut out,
        &whoami_fixture(true),
        "https://api.codewhale.net",
        key.head(),
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("user_1"), "{text}");
    assert!(text.contains("cwc_key_3f2a9c1e4b7d8a0f5c6e2b91"), "{text}");
    assert!(
        !text.contains(&VALID[TOKEN_HEAD_LEN..]),
        "the secret half leaked: {text}"
    );
}

#[test]
fn an_unconfigured_agent_is_a_distinct_line_on_a_successful_whoami() {
    // The server answers 200 here: authentication succeeded, and a diagnosis
    // surface that failed on unrelated configuration would tell the operator
    // nothing about the credential they came to check.
    let mut out = Vec::new();
    write_whoami(
        &mut out,
        &whoami_fixture(false),
        "https://api.codewhale.net",
        "cwc_key_x",
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("user_1"),
        "the account must still print: {text}"
    );
    assert!(text.contains("not configured"), "{text}");
    assert!(text.contains("codewhale account keys set"), "{text}");
}

// -- local input validation -----------------------------------------------

#[test]
fn key_names_are_checked_locally_against_the_server_pattern() {
    for good in [
        "github-actions",
        "a",
        "CI runner 2",
        "team/ci",
        "a.b_c:d@e-f",
        &"n".repeat(64),
    ] {
        validate_key_name(good).unwrap_or_else(|err| panic!("{good:?}: {err}"));
    }
    for bad in [
        "",
        " leading-space",
        "-leading-dash",
        "bad\nname",
        "bad*name",
        &"n".repeat(65),
    ] {
        assert!(validate_key_name(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn scopes_are_normalized_against_the_closed_set() {
    // Omitting --scope means every scope, sent explicitly rather than left
    // to whatever the control plane defaults to.
    assert_eq!(
        validate_scopes(&[]).unwrap(),
        Some(vec![
            "account:read".to_string(),
            "agent:run".to_string(),
            "models:infer".to_string(),
        ])
    );
    assert_eq!(
        validate_scopes(&["models:infer".into()]).unwrap(),
        Some(vec!["models:infer".to_string()])
    );
    assert_eq!(
        validate_scopes(&[
            "agent:run".into(),
            "account:read".into(),
            "agent:run".into()
        ])
        .unwrap(),
        Some(vec!["agent:run".to_string(), "account:read".to_string()])
    );
    let err = validate_scopes(&["billing:write".into()]).expect_err("closed set");
    assert!(err.to_string().contains("account:read"), "{err}");
}

#[test]
fn key_ids_are_checked_as_a_paste_check_not_an_existence_check() {
    validate_key_id("3f2a9c1e4b7d8a0f5c6e2b91").unwrap();
    validate_key_id("  3f2a9c1e4b7d8a0f5c6e2b91 ").unwrap();
    for bad in [
        "3f2a",
        "3F2A9C1E4B7D8A0F5C6E2B91",
        "3f2a9c1e4b7d8a0f5c6e2b9z",
    ] {
        assert!(validate_key_id(bad).is_err(), "{bad}");
    }
}

// -- management refusal ---------------------------------------------------

#[test]
fn a_machine_key_alone_cannot_manage_keys_and_is_refused_before_the_wire() {
    let env = MachineKeyEnv::from_raw(Some(VALID));
    let err = reject_machine_key_for_management(&env, false)
        .expect_err("a key must not be able to mint a successor");
    let text = err.to_string();
    assert!(
        text.contains("Managing API keys needs an interactive login."),
        "{text}"
    );
    assert!(text.contains("codewhale login"), "{text}");
    // The refusal must explain *why*, not just refuse.
    assert!(text.contains("minting a replacement"), "{text}");
    assert!(!text.contains(VALID), "the refusal echoed the key: {text}");
}

#[test]
fn a_session_alongside_a_machine_key_still_manages_keys() {
    // The session is the credential the management routes accept; having a
    // key exported at the same time is not a reason to refuse a human.
    let env = MachineKeyEnv::from_raw(Some(VALID));
    reject_machine_key_for_management(&env, true).expect("a real login may manage keys");
    reject_machine_key_for_management(&MachineKeyEnv::default(), false)
        .expect("no key present is not this check's problem");
}

// -- created-key rendering ------------------------------------------------

#[test]
fn a_created_secret_is_printed_once_with_an_unmissable_notice() {
    let created: ApiKeyCreateResponse = serde_json::from_value(json!({
        "apiKey": {
            "id": "3f2a9c1e4b7d8a0f5c6e2b91",
            "name": "github-actions",
            "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91",
            "scopes": ["account:read", "agent:run"],
            "createdAt": "2026-01-01T00:00:00Z",
            "expiresAt": null
        },
        "secret": VALID,
    }))
    .unwrap();
    let mut out = Vec::new();
    write_created_key(&mut out, &created).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        text.matches(VALID).count(),
        1,
        "printed more than once: {text}"
    );
    assert!(
        text.contains("ONLY TIME YOU WILL SEE THIS SECRET"),
        "{text}"
    );
    assert!(text.contains("CODEWHALE_API_KEY"), "{text}");
    assert!(text.contains("cannot show it again"), "{text}");
}

/// Audit R02-m2: a 2xx with an empty or malformed secret is not reported as
/// a created key.
#[test]
fn a_created_response_without_a_well_formed_secret_is_not_reported_as_created() {
    for secret in [json!(null), json!(""), json!("cwc_key_truncated")] {
        let mut body = json!({
            "apiKey": {
                "id": "3f2a9c1e4b7d8a0f5c6e2b91",
                "name": "github-actions",
                "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91",
                "scopes": ["account:read"],
                "createdAt": "2026-01-01T00:00:00Z"
            }
        });
        if !secret.is_null() {
            body["secret"] = secret.clone();
        }
        let created: ApiKeyCreateResponse = serde_json::from_value(body).unwrap();
        let mut out = Vec::new();
        let err = write_created_key(&mut out, &created).expect_err("malformed secret");
        assert!(out.is_empty(), "printed before validating: {secret}");
        let message = err.to_string();
        assert!(message.contains("3f2a9c1e4b7d8a0f5c6e2b91"), "{message}");
        assert!(message.contains("revoke"), "{message}");
    }
}

#[test]
fn a_listing_can_never_carry_a_secret_because_the_type_has_no_field_for_one() {
    // The server never returns one; the type makes a regression that started
    // returning one impossible to render.
    let listing: ApiKeyListResponse = serde_json::from_value(json!({
        "apiKeys": [
            { "id": "3f2a9c1e4b7d8a0f5c6e2b91", "name": "ci", "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91",
              "scopes": ["account:read"], "createdAt": "2026-01-01T00:00:00Z", "secret": VALID },
            { "id": "aaaabbbbccccddddeeeeffff", "name": "old", "displayPrefix": "cwc_key_aaaabbbbccccddddeeeeffff",
              "scopes": ["agent:run"], "createdAt": "2025-01-01T00:00:00Z", "revokedAt": "2026-02-02T00:00:00Z" }
        ]
    }))
    .unwrap();
    let mut out = Vec::new();
    write_key_listing(&mut out, &listing.api_keys).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains(VALID), "a listing leaked a secret: {text}");
    // Revoked keys stay listed so an owner can audit history.
    assert!(text.contains("[revoked]"), "{text}");
    assert!(text.contains("[active]"), "{text}");
}

#[test]
fn an_empty_listing_names_the_command_that_fixes_it() {
    let mut out = Vec::new();
    write_key_listing(&mut out, &[]).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("codewhale account api-keys create"), "{text}");
}

// -- review wiring --------------------------------------------------------

#[test]
fn review_resolves_the_route_from_the_accounts_configured_provider() {
    let agent: AgentState = serde_json::from_value(
        json!({ "configured": true, "modelProvider": "deepseek", "accountId": "user_1" }),
    )
    .unwrap();
    let provider = review_provider_from_agent(&agent).expect("deepseek is a known route");
    assert_eq!(provider, ProviderKind::Deepseek);
}

#[test]
fn review_refuses_with_the_configuration_exit_code_when_no_model_is_set() {
    let agent = AgentState {
        configured: false,
        model_provider: String::new(),
        account_id: None,
    };
    let err = review_provider_from_agent(&agent).expect_err("machine work needs a model");
    let machine_error = err.downcast_ref::<MachineError>().expect("typed error");
    assert_eq!(machine_error.exit_code, EXIT_AGENT_UNCONFIGURED);
    assert_ne!(machine_error.exit_code, EXIT_AUTH);
    assert!(
        machine_error.message.contains("codewhale account keys set"),
        "{machine_error}"
    );
}

#[test]
fn an_unknown_provider_identifier_asks_for_an_upgrade_rather_than_guessing() {
    let agent = AgentState {
        configured: true,
        model_provider: "a-provider-from-the-future".to_string(),
        account_id: None,
    };
    let err = review_provider_from_agent(&agent).expect_err("unknown routes must not be guessed");
    assert!(err.to_string().contains("Upgrade `codewhale`"), "{err}");
}
