use axum::Json;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use codewhale_core::secret_eq::constant_time_eq;
use serde_json::json;

use super::{RuntimeApiState, mobile, web};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedRuntimeAuth {
    pub(super) token: Option<String>,
    pub(super) generated: bool,
}

pub(super) fn resolve_runtime_auth(
    cli_token: Option<String>,
    env_token: Option<String>,
    insecure_no_auth: bool,
) -> ResolvedRuntimeAuth {
    if let Some(token) = first_nonblank_token(cli_token).or_else(|| first_nonblank_token(env_token))
    {
        return ResolvedRuntimeAuth {
            token: Some(token),
            generated: false,
        };
    }
    if insecure_no_auth {
        return ResolvedRuntimeAuth {
            token: None,
            generated: false,
        };
    }
    ResolvedRuntimeAuth {
        token: Some(generate_runtime_token()),
        generated: true,
    }
}

pub(super) fn runtime_auth_status_lines(auth: &ResolvedRuntimeAuth) -> Vec<String> {
    if auth.generated {
        return vec![
            "Runtime API auth: generated bearer token for this process (not printed).".to_string(),
            "  Set CODEWHALE_RUNTIME_TOKEN (or DEEPSEEK_RUNTIME_TOKEN as an alias) or pass --auth-token when another client needs to connect.".to_string(),
        ];
    }
    if auth.token.is_some() {
        return vec!["Runtime API auth: bearer token required for /v1/* routes.".to_string()];
    }
    vec!["Runtime API auth: disabled by explicit insecure mode.".to_string()]
}

fn first_nonblank_token(token: Option<String>) -> Option<String> {
    token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn generate_runtime_token() -> String {
    format!(
        "cwrt_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub(super) async fn require_host_token(
    State(state): State<RuntimeApiState>,
    req: Request,
    next: Next,
) -> Response {
    if state
        .runtime_token
        .as_deref()
        .is_some_and(|master| request_has_header_runtime_token(&req, master))
    {
        next.run(req).await
    } else {
        runtime_token_required_response()
    }
}

pub(super) async fn require_runtime_token(
    State(state): State<RuntimeApiState>,
    req: Request,
    next: Next,
) -> Response {
    if runtime_request_is_authorized(&req, &state) {
        next.run(req).await
    } else if request_bearer(&req)
        .is_some_and(|token| state.computer.client_principal(token).is_some())
    {
        (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "watch client tokens cannot change Runtime state"})),
        )
            .into_response()
    } else {
        runtime_token_required_response()
    }
}

pub(super) fn runtime_request_is_authorized(req: &Request, state: &RuntimeApiState) -> bool {
    let Some(expected) = state.runtime_token.as_deref() else {
        return true;
    };
    if request_has_header_runtime_token(req, expected) {
        return true;
    }
    // Device tokens carry their immutable mint intent. Watch permits HTTP
    // reads only; an upgraded GET could otherwise become a write channel.
    // Computer display/control routes enforce intent in their own handlers.
    if let Some(principal) =
        request_bearer(req).and_then(|token| state.computer.client_principal(token))
    {
        return client_request_is_authorized(&principal, req);
    }
    if state
        .web
        .as_ref()
        .is_some_and(|web| web_session_request_is_authorized(req, state, web))
    {
        return true;
    }
    state
        .mobile
        .as_ref()
        .is_some_and(|mobile| mobile_session_request_is_authorized(req, state, mobile))
}

fn client_request_is_authorized(
    principal: &super::computer_display::Principal,
    req: &Request,
) -> bool {
    principal.can_drive()
        || (matches!(*req.method(), Method::GET | Method::HEAD)
            && !req.headers().contains_key(header::UPGRADE)
            && !req.headers().contains_key("sec-websocket-key"))
}

fn request_bearer(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .or_else(|| {
            req.headers()
                .get("x-codewhale-runtime-token")
                .and_then(|value| value.to_str().ok())
        })
}

pub(super) fn request_has_header_runtime_token(req: &Request, expected: &str) -> bool {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
        || req
            .headers()
            .get("x-codewhale-runtime-token")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
        || req
            .headers()
            .get("x-deepseek-runtime-token")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
}

/// Web fetches require a cookie and an origin-scoped proof; streams consume
/// a single-use ticket instead. Origin metadata is an additional check only.
pub(super) fn web_session_request_is_authorized(
    req: &Request,
    state: &RuntimeApiState,
    web: &web::RuntimeWebState,
) -> bool {
    web_request_is_authorized(req, &runtime_http_origin(state), web)
}

fn web_request_is_authorized(
    req: &Request,
    expected_origin: &str,
    web: &web::RuntimeWebState,
) -> bool {
    if req
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|value| value != "same-origin")
        || req
            .headers()
            .get(header::ORIGIN)
            .is_some_and(|value| value != expected_origin)
    {
        return false;
    }
    let cookie = req
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok());
    if is_thread_stream_request(req) {
        return web.consume_stream_ticket(cookie, stream_ticket(req, web::WEB_STREAM_TICKET_QUERY));
    }
    web.matches_request(
        cookie,
        req.headers()
            .get(web::WEB_REQUEST_HEADER)
            .and_then(|value| value.to_str().ok()),
    )
}

/// Mobile cookies are host-scoped and can be attached to a sibling loopback
/// port. A cookie alone is therefore never Runtime authority: normal fetches
/// must also present an origin-scoped proof and EventSource requests must
/// consume a short-lived stream ticket. The origin/Fetch Metadata check keeps
/// a sibling port from replaying a captured value.
pub(super) fn mobile_session_request_is_authorized(
    req: &Request,
    state: &RuntimeApiState,
    mobile_state: &mobile::RuntimeMobileState,
) -> bool {
    if !mobile_cookie_request_is_same_origin(req, state) {
        return false;
    }
    let cookie_header = req
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok());
    if is_thread_stream_request(req) {
        return mobile_state.consume_stream_ticket(
            cookie_header,
            stream_ticket(req, mobile::MOBILE_STREAM_TICKET_QUERY),
        );
    }
    mobile_state.matches_request(
        cookie_header,
        req.headers()
            .get(mobile::MOBILE_REQUEST_HEADER)
            .and_then(|value| value.to_str().ok()),
    )
}

fn mobile_cookie_request_is_same_origin(req: &Request, state: &RuntimeApiState) -> bool {
    let fetch_metadata_is_same_origin = req
        .headers()
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|site| site.eq_ignore_ascii_case("same-origin"));
    if req
        .headers()
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|site| !site.eq_ignore_ascii_case("same-origin"))
    {
        return false;
    }

    match req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        Some(origin) => origin == runtime_http_origin(state),
        None => fetch_metadata_is_same_origin,
    }
}

fn runtime_http_origin(state: &RuntimeApiState) -> String {
    runtime_http_origin_for_bind(&state.bind_host, state.bind_port)
}

fn runtime_http_origin_for_bind(bind_host: &str, bind_port: u16) -> String {
    let host = match bind_host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => address.to_string(),
        Ok(std::net::IpAddr::V6(address)) => format!("[{address}]"),
        Err(_) => bind_host.to_string(),
    };
    if bind_port == 80 {
        format!("http://{host}")
    } else {
        format!("http://{host}:{bind_port}")
    }
}

fn is_thread_stream_request(req: &Request) -> bool {
    req.method() == Method::GET
        && req.uri().path().starts_with("/v1/threads/")
        && req.uri().path().ends_with("/events")
}

fn stream_ticket<'a>(req: &'a Request, query_key: &str) -> Option<&'a str> {
    let mut tickets = req
        .uri()
        .query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter_map(|(key, value)| (key == query_key).then_some(value));
    let ticket = tickets.next()?;
    tickets.next().is_none().then_some(ticket)
}

pub(super) fn runtime_token_required_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": "runtime API bearer token required",
                "status": StatusCode::UNAUTHORIZED.as_u16(),
            }
        })),
    )
        .into_response()
}

pub(super) async fn require_runtime_master_bearer(
    State(state): State<RuntimeApiState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.runtime_token.as_deref() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "Watch turns require configured Runtime bearer authentication"})),
        )
            .into_response();
    };
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|presented| constant_time_eq(presented.as_bytes(), expected.as_bytes()));
    if authorized {
        next.run(req).await
    } else {
        (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Watch turns require the master Authorization bearer"})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::runtime_http_origin_for_bind;

    #[test]
    fn runtime_surface_hardening_web_requests_require_proof() {
        use super::*;
        use axum::body::Body;
        let (web, nonce) = web::RuntimeWebState::new();
        let (token, proof) = web.consume(&nonce, "127.0.0.1".parse().unwrap()).unwrap();
        let cookie = format!("codewhale_web_session={token}");
        let origin = "http://127.0.0.1:7878";
        let request = |method: Method, path: &str, proof: Option<&str>, metadata: bool| {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header(header::COOKIE, &cookie);
            if metadata {
                builder = builder
                    .header(header::ORIGIN, origin)
                    .header("sec-fetch-site", "same-origin");
            }
            if let Some(proof) = proof {
                builder = builder.header(web::WEB_REQUEST_HEADER, proof);
            }
            builder.body(Body::empty()).unwrap()
        };
        for method in [Method::GET, Method::POST, Method::HEAD, Method::OPTIONS] {
            for metadata in [false, true] {
                for presented in [None, Some("wrong-proof")] {
                    assert!(!web_request_is_authorized(
                        &request(method.clone(), "/v1/threads", presented, metadata),
                        origin,
                        &web
                    ));
                }
                assert!(web_request_is_authorized(
                    &request(method.clone(), "/v1/threads", Some(&proof), metadata),
                    origin,
                    &web
                ));
            }
        }
        let mut wrong_origin = request(Method::POST, "/v1/threads", Some(&proof), true);
        wrong_origin
            .headers_mut()
            .insert(header::ORIGIN, "http://127.0.0.1:3000".parse().unwrap());
        assert!(!web_request_is_authorized(&wrong_origin, origin, &web));
        let mut cross_site = request(Method::GET, "/v1/threads", Some(&proof), true);
        cross_site
            .headers_mut()
            .insert("sec-fetch-site", "same-site".parse().unwrap());
        assert!(!web_request_is_authorized(&cross_site, origin, &web));

        assert!(web.refresh_stream_ticket(Some(&cookie), None).is_none());
        let ticket = web
            .refresh_stream_ticket(Some(&cookie), Some(&proof))
            .unwrap();
        let path = format!("/v1/threads/thread-1/events?web_stream_ticket={ticket}");
        assert!(!web_request_is_authorized(
            &request(
                Method::GET,
                "/v1/threads/thread-1/events",
                Some(&proof),
                true
            ),
            origin,
            &web
        ));
        assert!(!web_request_is_authorized(
            &request(
                Method::GET,
                &format!("{path}&web_stream_ticket=extra"),
                None,
                true
            ),
            origin,
            &web
        ));
        assert!(web_request_is_authorized(
            &request(Method::GET, &path, None, true),
            origin,
            &web
        ));
        assert!(!web_request_is_authorized(
            &request(Method::GET, &path, None, true),
            origin,
            &web
        ));
    }

    #[test]
    fn expected_origin_canonicalizes_ipv6_loopback_literals() {
        assert_eq!(
            runtime_http_origin_for_bind("0:0:0:0:0:0:0:1", 7878),
            "http://[::1]:7878"
        );
        assert_eq!(runtime_http_origin_for_bind("::1", 80), "http://[::1]");
    }
}
