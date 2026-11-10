//! Tests for `/v1/computer/*` (ARCHITECTURE §6 S2 acceptance).
//!
//! The fake Xvnc below speaks the server side of RFB 3.8 on a Unix socket
//! and records every client byte it receives after `ClientInit`. "Watcher
//! input produces 0 X events" is asserted as "Xvnc received 0 input bytes":
//! input that never reaches the X server cannot become an X event.

use super::*;

#[test]
fn parser_forwards_allowed_messages_split_across_frames() {
    let mut parser = ClientParser::default();
    let mut out = Vec::new();
    // FramebufferUpdateRequest (10 bytes) split 3 + 7.
    let fur = [3u8, 1, 0, 0, 0, 0, 5, 160, 3, 132];
    parser.feed(&fur[..3], false, &mut out).unwrap();
    assert!(out.is_empty());
    parser.feed(&fur[3..], false, &mut out).unwrap();
    assert_eq!(out, fur);
}

#[test]
fn parser_drops_input_without_lease_and_forwards_with_it() {
    let key = [4u8, 1, 0, 0, 0, 0, 0, 0x61];
    let pointer = [5u8, 1, 0, 10, 0, 20];
    let cut = [6u8, 0, 0, 0, 0, 0, 0, 2, b'h', b'i'];
    let mut resize = vec![251u8, 0, 5, 0, 3, 32, 1, 0];
    resize.extend_from_slice(&[0u8; 16]);
    let mut all = Vec::new();
    for m in [&key[..], &pointer[..], &cut[..], &resize[..]] {
        all.extend_from_slice(m);
    }

    let mut out = Vec::new();
    let stats = ClientParser::default().feed(&all, false, &mut out).unwrap();
    assert!(out.is_empty(), "watcher input must not be forwarded");
    assert_eq!(stats.input_dropped, 4);

    let mut out = Vec::new();
    let stats = ClientParser::default().feed(&all, true, &mut out).unwrap();
    assert_eq!(out, all);
    assert_eq!(stats.input_forwarded, 4);
}

#[test]
fn parser_closes_on_unknown_type_and_oversize() {
    let mut out = Vec::new();
    assert_eq!(
        ClientParser::default().feed(&[248, 0, 0, 0], true, &mut out),
        Err(ParseError::UnknownType(248))
    );
    // A negative (extended-clipboard) or huge cut length is refused.
    assert!(matches!(
        ClientParser::default().feed(&[6, 0, 0, 0, 0xff, 0xff, 0xff, 0xfc], true, &mut out),
        Err(ParseError::TooLarge {
            message_type: 6,
            ..
        })
    ));
    // Unknown type after a valid message still closes.
    let mut parser = ClientParser::default();
    let mut bytes = vec![3u8, 1, 0, 0, 0, 0, 0, 1, 0, 1];
    bytes.push(0xff);
    assert_eq!(
        parser.feed(&bytes, true, &mut out),
        Err(ParseError::UnknownType(0xff))
    );
}

#[test]
fn parser_strips_encodings_that_start_unparsed_subprotocols() {
    let encodings: [i32; 5] = [16, -312, 7, -258, -239];
    let mut msg = vec![2u8, 0];
    msg.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in encodings {
        msg.extend_from_slice(&e.to_be_bytes());
    }
    let mut out = Vec::new();
    ClientParser::default().feed(&msg, false, &mut out).unwrap();
    let mut expected = vec![2u8, 0, 0, 3];
    for e in [16i32, 7, -239] {
        expected.extend_from_slice(&e.to_be_bytes());
    }
    assert_eq!(out, expected);
}

#[test]
fn display_socket_path_must_be_absolute_and_plain() {
    assert_eq!(
        validated_socket_path(" /run/cw/vnc.sock "),
        Some(PathBuf::from("/run/cw/vnc.sock"))
    );
    assert_eq!(validated_socket_path(""), None);
    assert_eq!(validated_socket_path("vnc.sock"), None);
    assert_eq!(validated_socket_path("./vnc.sock"), None);
    assert_eq!(validated_socket_path("/run/cw/../../etc/passwd"), None);
    assert_eq!(validated_socket_path("/run/./cw/vnc.sock"), None);
    assert_eq!(validated_socket_path("/run/cw/vnc\0.sock"), None);
    assert_eq!(validated_socket_path("/"), None);
}

#[test]
fn redaction_hides_ticket_and_token_values() {
    let redacted = redact_query_secrets(
        "/v1/computer/display?ticket=cwdt_secret&mode=view&mobile_stream_ticket=abc&Token=x#frag",
    );
    assert_eq!(
        redacted,
        "/v1/computer/display?ticket=redacted&mode=view&mobile_stream_ticket=redacted&Token=redacted#redacted"
    );
    assert!(!redacted.contains("cwdt_secret"));
    assert_eq!(redact_query_secrets("/v1/computer"), "/v1/computer");
}

#[tokio::test]
async fn parser_panic_is_contained_to_its_task() {
    // Stand-in for an active turn running on the same runtime.
    let turn = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        "turn finished"
    });
    let parser = tokio::spawn(async {
        if std::hint::black_box(true) {
            panic!("parser bug");
        }
        ExitReason::ClientClosed
    });
    assert_eq!(parser_exit(parser.await), ExitReason::ParserPanic);
    assert_eq!(turn.await.unwrap(), "turn finished");
}

#[cfg(unix)]
mod live {
    use super::*;
    use tokio::net::UnixListener;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::Message as TMessage;

    const MASTER: &str = "master-token-for-tests";

    struct Harness {
        base: String,
        ws_base: String,
        computer: ComputerState,
        received: Arc<parking_lot::Mutex<Vec<u8>>>,
        _dir: tempfile::TempDir,
    }

    async fn fake_xvnc(listener: UnixListener, received: Arc<parking_lot::Mutex<Vec<u8>>>) {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let received = received.clone();
            tokio::spawn(async move {
                s.write_all(RFB_VERSION_38).await.unwrap();
                let mut v = [0u8; 12];
                s.read_exact(&mut v).await.unwrap();
                s.write_all(&[1, 1]).await.unwrap();
                let mut one = [0u8; 1];
                s.read_exact(&mut one).await.unwrap();
                s.write_all(&[0, 0, 0, 0]).await.unwrap();
                s.read_exact(&mut one).await.unwrap(); // ClientInit
                let mut init = Vec::new();
                init.extend_from_slice(&1440u16.to_be_bytes());
                init.extend_from_slice(&900u16.to_be_bytes());
                init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
                init.extend_from_slice(&4u32.to_be_bytes());
                init.extend_from_slice(b"twin");
                s.write_all(&init).await.unwrap();
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => received.lock().extend_from_slice(&buf[..n]),
                    }
                }
            });
        }
    }

    async fn harness() -> Harness {
        // The workspace builds reqwest with `rustls-no-provider`; the binary
        // installs ring at startup, so tests that build a Client must too.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("vnc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let received = Arc::new(parking_lot::Mutex::new(Vec::new()));
        tokio::spawn(fake_xvnc(listener, received.clone()));
        let computer = ComputerState::new(sock, DEFAULT_IDLE);
        let app: Router = router(computer.clone(), Some(MASTER.to_string()));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
        Harness {
            base: format!("http://{addr}"),
            ws_base: format!("ws://{addr}"),
            computer,
            received,
            _dir: dir,
        }
    }

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn connect(h: &Harness, bearer: Option<&str>, query: &str) -> Result<Ws, u16> {
        let mut req = format!("{}/v1/computer/display{query}", h.ws_base)
            .into_client_request()
            .unwrap();
        if let Some(token) = bearer {
            req.headers_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(ws),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
            Err(other) => panic!("unexpected connect error: {other}"),
        }
    }

    struct RfbClient {
        ws: Ws,
        buf: Vec<u8>,
    }

    impl RfbClient {
        async fn read(&mut self, n: usize) -> Vec<u8> {
            while self.buf.len() < n {
                match tokio::time::timeout(Duration::from_secs(5), self.ws.next())
                    .await
                    .expect("frame within 5 s")
                {
                    Some(Ok(TMessage::Binary(b))) => self.buf.extend_from_slice(&b),
                    other => panic!("expected binary frame, got {other:?}"),
                }
            }
            let rest = self.buf.split_off(n);
            std::mem::replace(&mut self.buf, rest)
        }

        async fn send(&mut self, bytes: &[u8]) {
            self.ws
                .send(TMessage::Binary(bytes.to_vec().into()))
                .await
                .unwrap();
        }

        /// Handshake and return the ServerInit width/height.
        async fn handshake(ws: Ws) -> (Self, u16, u16) {
            let mut c = RfbClient {
                ws,
                buf: Vec::new(),
            };
            assert_eq!(c.read(12).await, RFB_VERSION_38);
            c.send(RFB_VERSION_38).await;
            assert_eq!(c.read(2).await, [1, 1], "only security None offered");
            c.send(&[1]).await;
            assert_eq!(c.read(4).await, [0, 0, 0, 0]);
            c.send(&[0]).await; // ClientInit
            let init = c.read(24).await;
            let name_len = u32::from_be_bytes([init[20], init[21], init[22], init[23]]) as usize;
            assert_eq!(c.read(name_len).await, b"twin");
            let w = u16::from_be_bytes([init[0], init[1]]);
            let h = u16::from_be_bytes([init[2], init[3]]);
            (c, w, h)
        }
    }

    async fn wait_for_received(h: &Harness, len: usize) -> Vec<u8> {
        for _ in 0..100 {
            if h.received.lock().len() >= len {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        h.received.lock().clone()
    }

    fn event_kinds(h: &Harness) -> Vec<(String, Value)> {
        h.computer
            .events_since(0)
            .0
            .into_iter()
            .map(|e| (e.kind, e.data))
            .collect()
    }

    const FUR: [u8; 10] = [3, 1, 0, 0, 0, 0, 5, 160, 3, 132];
    const KEY: [u8; 8] = [4, 1, 0, 0, 0, 0, 0, 0x61];
    const POINTER: [u8; 6] = [5, 1, 0, 10, 0, 20];

    #[tokio::test]
    async fn display_requires_a_token_or_a_single_use_ticket() {
        let h = harness().await;
        assert_eq!(connect(&h, None, "").await.err(), Some(401));
        assert_eq!(connect(&h, Some("wrong"), "").await.err(), Some(401));
        assert_eq!(
            connect(&h, None, "?ticket=cwdt_forged").await.err(),
            Some(401)
        );

        let http = codewhale_release::tls::reqwest_client();
        let resp = http
            .post(format!("{}/v1/computer/display/tickets", h.base))
            .bearer_auth(MASTER)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 201);
        let body: Value = resp.json().await.unwrap();
        let ticket = body["ticket"].as_str().unwrap().to_string();

        let ws = connect(&h, None, &format!("?ticket={ticket}"))
            .await
            .expect("ticket works once");
        let (_client, w, hgt) = RfbClient::handshake(ws).await;
        assert_eq!((w, hgt), (1440, 900), "valid ticket reaches ServerInit");
        assert_eq!(
            connect(&h, None, &format!("?ticket={ticket}")).await.err(),
            Some(401),
            "reused ticket is refused"
        );
    }

    #[tokio::test]
    async fn watcher_input_never_reaches_xvnc_and_lease_holder_input_does() {
        let h = harness().await;
        let ws = connect(&h, Some(MASTER), "").await.unwrap();
        let (mut c, _, _) = RfbClient::handshake(ws).await;

        // Watching: key + pointer are dropped, the update request passes.
        let mut burst = Vec::new();
        burst.extend_from_slice(&KEY);
        burst.extend_from_slice(&POINTER);
        burst.extend_from_slice(&FUR);
        c.send(&burst).await;
        let got = wait_for_received(&h, FUR.len()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(got, FUR, "watcher input produced bytes at Xvnc");
        assert_eq!(h.received.lock().len(), FUR.len());

        // Driving: after acquiring the lease the same input is forwarded.
        let resp = codewhale_release::tls::reqwest_client()
            .post(format!("{}/v1/computer/control/acquire", h.base))
            .bearer_auth(MASTER)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        c.send(&KEY).await;
        let got = wait_for_received(&h, FUR.len() + KEY.len()).await;
        assert_eq!(&got[FUR.len()..], KEY);

        let released = codewhale_release::tls::reqwest_client()
            .post(format!("{}/v1/computer/control/release", h.base))
            .bearer_auth(MASTER)
            .send()
            .await
            .unwrap();
        assert_eq!(released.status().as_u16(), 200);
        let kinds: Vec<String> = event_kinds(&h).into_iter().map(|(k, _)| k).collect();
        assert!(kinds.contains(&"computer.display.attached".to_string()));
        assert!(kinds.contains(&"computer.control.acquired".to_string()));
        assert!(kinds.contains(&"computer.control.released".to_string()));
        // Events carry counts, never key values.
        let released = event_kinds(&h)
            .into_iter()
            .find(|(k, _)| k == "computer.control.released")
            .unwrap()
            .1;
        assert_eq!(released["input_events"], 1);
    }

    #[tokio::test]
    async fn unknown_client_message_closes_the_stream_with_an_event() {
        let h = harness().await;
        let ws = connect(&h, Some(MASTER), "").await.unwrap();
        let (mut c, _, _) = RfbClient::handshake(ws).await;
        c.send(&[200, 0, 0, 0]).await;
        let close = loop {
            match tokio::time::timeout(Duration::from_secs(5), c.ws.next())
                .await
                .expect("close within 5 s")
            {
                Some(Ok(TMessage::Close(frame))) => break frame,
                Some(Ok(_)) => continue,
                other => panic!("expected close, got {other:?}"),
            }
        };
        assert_eq!(u16::from(close.unwrap().code), 1008);
        for _ in 0..50 {
            if event_kinds(&h)
                .iter()
                .any(|(k, _)| k == "computer.display.detached")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let detached = event_kinds(&h)
            .into_iter()
            .find(|(k, _)| k == "computer.display.detached")
            .expect("detached event")
            .1;
        assert!(
            detached["reason"]
                .as_str()
                .unwrap()
                .contains("unknown client message type 200")
        );
        assert!(h.received.lock().is_empty());
    }

    #[tokio::test]
    async fn watch_client_token_cannot_acquire_control_or_send_input_through_a_ticket() {
        let h = harness().await;
        let http = codewhale_release::tls::reqwest_client();
        let minted: Value = http
            .post(format!("{}/v1/auth/client-tokens", h.base))
            .bearer_auth(MASTER)
            // A legacy label cannot broaden the omitted, default-watch intent.
            .json(&json!({ "device_id": "same-device", "label": "cwc-seat:drive" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(minted["intent"], "watch");
        let watch = minted["token"].as_str().unwrap();
        let drive: Value = http
            .post(format!("{}/v1/auth/client-tokens", h.base))
            .bearer_auth(MASTER)
            .json(&json!({ "device_id": "same-device", "intent": "drive" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let lease = http
            .post(format!("{}/v1/computer/control/acquire", h.base))
            .bearer_auth(drive["token"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(lease.status(), StatusCode::OK);
        for (token, expected) in [(watch, false), (drive["token"].as_str().unwrap(), true)] {
            let status = http
                .get(format!("{}/v1/computer", h.base))
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            assert_eq!(status.status(), StatusCode::OK);
            let status: Value = status.json().await.unwrap();
            assert_eq!(status["control"]["you_hold_lease"], expected);
        }
        for (action, body) in [
            ("acquire", json!({})),
            ("acquire", json!({ "force": true })),
            ("release", json!({})),
        ] {
            let refused = http
                .post(format!("{}/v1/computer/control/{action}", h.base))
                .bearer_auth(watch)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(
            h.computer.sweep_lease().unwrap().holder,
            "device:same-device"
        );
        let ticket: Value = http
            .post(format!("{}/v1/computer/display/tickets", h.base))
            .bearer_auth(watch)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let ws = connect(
            &h,
            None,
            &format!("?ticket={}", ticket["ticket"].as_str().unwrap()),
        )
        .await
        .unwrap();
        let (mut watcher, _, _) = RfbClient::handshake(ws).await;
        let mut burst = Vec::from(KEY);
        burst.extend_from_slice(&POINTER);
        burst.extend_from_slice(&FUR);
        watcher.send(&burst).await;
        assert_eq!(wait_for_received(&h, FUR.len()).await, FUR);
        assert_eq!(h.received.lock().len(), FUR.len());
    }

    #[tokio::test]
    async fn client_tokens_are_owner_minted_scoped_and_revocable() {
        let h = harness().await;
        let http = codewhale_release::tls::reqwest_client();
        let resp = http
            .post(format!("{}/v1/auth/client-tokens", h.base))
            .bearer_auth(MASTER)
            .json(&json!({ "device_id": "mac-1", "ttl_seconds": 999999, "intent": "drive" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 201);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["intent"], "drive");
        let token = body["token"].as_str().unwrap().to_string();
        let id = body["id"].as_str().unwrap().to_string();
        let expires: DateTime<Utc> = body["expires_at"].as_str().unwrap().parse().unwrap();
        assert!(
            expires <= Utc::now() + chrono::Duration::seconds(3601),
            "ttl clamps to 1 h"
        );

        // The client token works on the computer surface...
        let status = http
            .get(format!("{}/v1/computer", h.base))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(status.status().as_u16(), 200);
        // ...and on the display, whose lease is per device.
        let ws = connect(&h, Some(&token), "").await.unwrap();
        let (_c, _, _) = RfbClient::handshake(ws).await;
        let lease: Value = http
            .post(format!("{}/v1/computer/control/acquire", h.base))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(lease["lease"]["holder"], "device:mac-1");
        // The owner is refused without force, and takes over with it.
        let conflict = http
            .post(format!("{}/v1/computer/control/acquire", h.base))
            .bearer_auth(MASTER)
            .send()
            .await
            .unwrap();
        assert_eq!(conflict.status().as_u16(), 409);

        // A client token cannot mint or list client tokens.
        let forbidden = http
            .post(format!("{}/v1/auth/client-tokens", h.base))
            .bearer_auth(&token)
            .json(&json!({ "device_id": "evil" }))
            .send()
            .await
            .unwrap();
        assert_eq!(forbidden.status().as_u16(), 403);
        let bad_device = http
            .post(format!("{}/v1/auth/client-tokens", h.base))
            .bearer_auth(MASTER)
            .json(&json!({ "device_id": "has space" }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_device.status().as_u16(), 400);

        // Revoke: the token stops working and its lease expires.
        let revoked = http
            .delete(format!("{}/v1/auth/client-tokens/{id}", h.base))
            .bearer_auth(MASTER)
            .send()
            .await
            .unwrap();
        assert_eq!(revoked.status().as_u16(), 204);
        let after = http
            .get(format!("{}/v1/computer", h.base))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(after.status().as_u16(), 401);
        let status: Value = http
            .get(format!("{}/v1/computer", h.base))
            .bearer_auth(MASTER)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(status["control"]["lease"].is_null());
        assert!(
            event_kinds(&h)
                .iter()
                .any(|(k, _)| k == "computer.control.expired")
        );
    }

    #[tokio::test]
    async fn missing_display_socket_is_503_not_a_hang() {
        let dir = tempfile::tempdir().unwrap();
        let computer = ComputerState::new(dir.path().join("absent.sock"), DEFAULT_IDLE);
        let app: Router = router(computer, Some(MASTER.to_string()));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
        let mut req = format!("ws://{addr}/v1/computer/display")
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {MASTER}").parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status().as_u16(), 503)
            }
            other => panic!("expected 503, got {other:?}"),
        }
    }
}
