//! Negative controls for the `mcp` family: each one changes a case (or the
//! dispatch) the way a regression would and shows the suite fails, for the
//! reason the case exists. A golden that no mutation can move pins nothing.
//!
//! Every control runs through [`run_case`] with `compare_only`, so a control
//! can never rewrite a golden, even under `CODEWHALE_CONFORMANCE_UPDATE=1`.
//! The existing stalled-server control
//! (`harness_timeout_rejects_a_real_unanswered_mcp_call`) lives with the
//! runner; these add the rest.
//!
//! Two controls swap the *dispatch* instead of the case: a dispatch that
//! replays a failed call, and one that refreshes its catalog behind the
//! model's back. They stand in for a replacement implementation getting
//! those behaviours wrong, and prove the `DISPATCHES` seam would catch it.

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    BEARER_ENV, CALL_DEADLINE, DispatchFactory, FAMILY, McpDispatchUnderTest, McpPoolDispatch,
    assert_no_secret, run_case,
};
use crate::conformance::golden;
use crate::tools::spec::{RichToolResult, ToolError};

/// Run `name` with `mutate` against both actual production backends.
fn mutated(name: &str, mutate: impl FnOnce(&mut Value)) -> Result<(), String> {
    let mut case = golden::read_case(FAMILY, name);
    mutate(&mut case);
    let mut errors = Vec::new();
    for (dispatch, factory) in super::DISPATCHES {
        match run_case(name, &case, dispatch, *factory, CALL_DEADLINE, true) {
            Ok(()) => return Ok(()), // one implementation missed the mutation
            Err(error) => errors.push(format!("{dispatch}: {error}")),
        }
    }
    Err(errors.join("\n"))
}

fn mutated_via(
    name: &str,
    factory: DispatchFactory,
    mutate: impl FnOnce(&mut Value),
) -> Result<(), String> {
    let mut case = golden::read_case(FAMILY, name);
    mutate(&mut case);
    run_case(name, &case, "mcp_pool", factory, CALL_DEADLINE, true)
}

fn set(case: &mut Value, pointer: &str, value: Value) {
    *case
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("case has no `{pointer}`")) = value;
}

#[track_caller]
fn drifts(result: Result<(), String>) -> String {
    let error = result.expect_err("the changed case must not match its golden");
    eprintln!("control drift: {error}");
    assert!(
        error.contains("golden drift"),
        "expected a golden drift, got: {error}"
    );
    error
}

#[track_caller]
fn harness_rejects(result: Result<(), String>, reason: &str) {
    let error = result.expect_err("the changed case must fail the harness");
    assert!(error.contains(reason), "expected `{reason}`, got: {error}");
}

// --- stdio ---

#[cfg(unix)]
#[test]
fn control_stdio_server_that_survives_its_call_is_noticed() {
    drifts(mutated("stdio_tools_and_exit", |case| {
        set(
            case,
            "/server/tools~1call/3",
            json!({"match": {"name": "dies"}, "result": {"content": [{"type": "text", "text": "survived"}]}}),
        );
    }));
}

#[cfg(unix)]
#[test]
fn control_stdio_changed_reply_bytes_are_noticed() {
    drifts(mutated("stdio_tools_and_exit", |case| {
        set(
            case,
            "/server/tools~1call/0/result/content/0/text",
            json!("echo from stdio, changed"),
        );
    }));
}

// --- sessions ---

#[test]
fn control_a_stale_session_that_never_happens_is_noticed() {
    drifts(mutated("http_session_lifecycle", |case| {
        case["server"]["tools/call"]
            .as_array_mut()
            .expect("candidates")
            .remove(1);
    }));
}

#[test]
fn control_the_negotiated_protocol_revision_is_pinned() {
    drifts(mutated("http_session_lifecycle", |case| {
        set(
            case,
            "/server/initialize/result/protocolVersion",
            json!("2024-11-05"),
        );
    }));
}

#[test]
fn control_a_dispatch_that_replays_a_failed_call_is_caught() {
    // `session_error` answers a JSON-RPC error that mentions the session; the
    // pool does not replay it (the server may have acted). A dispatch that
    // calls again after any error shows up as a second `session_error` in
    // `server_received`, and as a second `tools/call` request.
    drifts(mutated_via(
        "http_session_lifecycle",
        |setup| {
            Box::new(Deviant::new(
                McpPoolDispatch::boxed(setup),
                Deviation::ReplayOnError,
            ))
        },
        |_| {},
    ));
}

#[test]
fn control_a_dispatch_that_refreshes_on_list_changed_is_caught() {
    // The Rust pool ignores `notifications/tools/list_changed`; a replacement
    // that re-lists on its own (here: on every catalog read) must not match.
    drifts(mutated_via(
        "http_session_lifecycle",
        |setup| {
            Box::new(Deviant::new(
                McpPoolDispatch::boxed(setup),
                Deviation::RefreshCatalog,
            ))
        },
        |_| {},
    ));
}

#[test]
fn control_a_newer_accepted_protocol_revision_does_not_end_the_handshake() {
    // The boundary of `MCP_CLIENT_ACCEPTED_PROTOCOL_VERSIONS`: the same case
    // with a revision the client does implement connects, so the case fails
    // the harness (it expects boot to fail) instead of drifting.
    harness_rejects(
        mutated("http_protocol_version_unsupported", |case| {
            set(
                case,
                "/server/initialize/result/protocolVersion",
                json!("2025-11-25"),
            );
            case["server"]["tools/list"] = json!({"result": {"tools": []}});
        }),
        "expected boot to fail",
    );
}

// --- catalog caps ---

#[test]
fn control_a_catalog_exactly_on_the_item_cap_still_connects() {
    harness_rejects(
        mutated("catalog_cap_items", |case| {
            set(
                case,
                "/server/tools~1list/generate_tools/count",
                json!(4096),
            );
        }),
        "expected boot to fail",
    );
}

#[test]
fn control_a_catalog_exactly_on_the_page_cap_still_connects() {
    harness_rejects(
        mutated("catalog_cap_pages", |case| {
            set(case, "/server/tools~1list/generate_pages/count", json!(64));
        }),
        "expected boot to fail",
    );
}

#[test]
fn control_a_cursor_chain_that_ends_is_not_a_repeat() {
    harness_rejects(
        mutated("catalog_cap_cursor_repeat", |case| {
            case["server"]["tools/list"][0]["result"]
                .as_object_mut()
                .expect("loop page")
                .remove("nextCursor");
        }),
        "expected boot to fail",
    );
}

#[test]
fn control_a_dropped_page_is_noticed() {
    drifts(mutated("catalog_pagination", |case| {
        case["server"]["tools/list"][2]["result"]
            .as_object_mut()
            .expect("first page")
            .remove("nextCursor");
    }));
}

#[test]
fn control_a_changed_approval_annotation_is_noticed() {
    drifts(mutated("approval_hints", |case| {
        set(
            case,
            "/server/tools~1list/result/tools/2/annotations/destructiveHint",
            json!(false),
        );
    }));
}

// --- auth ---

/// A server that accepts the handshake: what the 401 and redirect cases must
/// not turn into.
fn open_server() -> Value {
    json!({
        "initialize": {"result": {
            "protocolVersion": "2025-06-18",
            "serverInfo": {"name": "open", "version": "1"},
            "capabilities": {"tools": {}},
        }},
        "tools/list": {"result": {"tools": []}},
    })
}

#[test]
fn control_a_server_that_accepts_the_login_is_not_needs_auth() {
    harness_rejects(
        mutated("auth_needs_login", |case| {
            set(case, "/server", open_server())
        }),
        "expected boot to fail",
    );
}

#[test]
fn control_a_client_that_stops_sending_its_bearer_is_noticed() {
    // Without the secret the harness exports no token and the server stops
    // requiring one; what moves is the recorded `authorization` shape.
    drifts(mutated("auth_bearer_never_recorded", |case| {
        case.as_object_mut().expect("case").remove("bearer_secret");
    }));
}

#[test]
fn control_a_followed_redirect_is_noticed() {
    harness_rejects(
        mutated("auth_redirect_cross_origin", |case| {
            set(case, "/server", open_server())
        }),
        "expected boot to fail",
    );
}

#[test]
fn control_a_dispatch_that_leaks_the_credential_is_caught() {
    let error = mutated_via(
        "auth_bearer_never_recorded",
        |setup| {
            Box::new(Deviant::new(
                McpPoolDispatch::boxed(setup),
                Deviation::LeakCredential,
            ))
        },
        |_| {},
    )
    .expect_err("a leaked credential must fail the case");
    assert!(error.contains("secret leak"), "{error}");
}

#[test]
fn the_secret_scan_trips_on_every_spelling_of_the_token() {
    let secret = "conformance-secret-token-0123456789";
    assert_no_secret(
        secret,
        "{\"requests\": [{\"authorization\": \"<bearer>\"}]}",
    )
    .expect("a shape is not a secret");
    // Name the spelling that was missed, never print it: the failure message
    // must not carry the token (or a string built from it) into a log.
    for (spelling, leaked) in [
        (
            "an Authorization header",
            format!("Authorization: Bearer {secret}"),
        ),
        (
            "a JSON error detail",
            format!("{{\"detail\": \"rejected {secret}\"}}"),
        ),
        ("a URL query", format!("http://host/mcp?token={secret}")),
    ] {
        assert!(
            assert_no_secret(secret, &leaked).is_err(),
            "the scan missed the token in {spelling}"
        );
    }
}

#[test]
fn no_committed_golden_carries_a_case_secret() {
    let mut scanned = 0;
    for name in golden::case_names(FAMILY) {
        let case = golden::read_case(FAMILY, &name);
        let Some(secret) = case["bearer_secret"].as_str() else {
            continue;
        };
        let path = golden::family_dir(FAMILY).join(format!("{name}.golden.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_no_secret(secret, &text).unwrap_or_else(|error| panic!("{name}: {error}"));
        if case["record_requests"].is_string() {
            for (dispatch, _) in super::DISPATCHES {
                let path = golden::family_dir(FAMILY)
                    .join(format!("{name}.{dispatch}.transport.golden.json"));
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                assert_no_secret(secret, &text)
                    .unwrap_or_else(|error| panic!("{name} via {dispatch}: {error}"));
            }
        }
        scanned += 1;
    }
    assert!(scanned >= 2, "expected the auth cases to declare secrets");
}

// --- deny rules ---

#[test]
fn control_dropping_the_deny_rules_is_noticed() {
    drifts(mutated("deny_tool_rules", |case| {
        set(case, "/disallowed_tools", json!([]));
    }));
    drifts(mutated("deny_server_wildcard", |case| {
        set(case, "/disallowed_tools", json!([]));
    }));
}

// --- deviant dispatches ---

#[derive(Clone, Copy)]
enum Deviation {
    /// Call a failed tool a second time.
    ReplayOnError,
    /// Reconnect before every catalog read.
    RefreshCatalog,
    /// Put the bearer token into an error detail.
    LeakCredential,
}

struct Deviant {
    inner: Box<dyn McpDispatchUnderTest>,
    deviation: Deviation,
}

impl Deviant {
    fn new(inner: Box<dyn McpDispatchUnderTest>, deviation: Deviation) -> Self {
        Self { inner, deviation }
    }
}

#[async_trait::async_trait]
impl McpDispatchUnderTest for Deviant {
    async fn boot(&self) -> Result<(), String> {
        self.inner.boot().await
    }

    async fn catalog(&self) -> Vec<codewhale_models::Tool> {
        if matches!(self.deviation, Deviation::RefreshCatalog) {
            self.inner.shutdown().await;
            let _ = self.inner.boot().await;
        }
        self.inner.catalog().await
    }

    async fn call(
        &self,
        model_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<RichToolResult, ToolError> {
        let first = self
            .inner
            .call(model_name, input.clone(), cancel.clone())
            .await;
        match (self.deviation, first) {
            (Deviation::ReplayOnError, Err(error))
                if !matches!(error, ToolError::Cancelled { .. }) =>
            {
                self.inner.call(model_name, input, cancel).await
            }
            (Deviation::LeakCredential, Err(error)) => {
                let token = std::env::var(BEARER_ENV).unwrap_or_default();
                Err(ToolError::execution_failed(format!(
                    "{error} (sent Authorization: Bearer {token})"
                )))
            }
            (_, result) => result,
        }
    }

    async fn approval_hint(&self, model_name: &str) -> Option<&'static str> {
        self.inner.approval_hint(model_name).await
    }

    async fn shutdown(&self) {
        self.inner.shutdown().await;
    }
}
