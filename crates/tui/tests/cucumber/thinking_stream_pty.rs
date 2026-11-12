use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::qa_harness::{
    harness::{Harness, SealedWorkspace, make_sealed_workspace},
    keys,
};

const WAIT: Duration = Duration::from_secs(20);
const MODEL: &str = "deepseek-v4-pro";
const PROMPTS: [&str; 2] = ["first slow fixture prompt", "second slow fixture prompt"];
const ANSWER: &str = "Fixture answer after thinking.";
const REASONING_LINES: usize = 6;
const NOTICE: &str = "Account not loaded. Run codewhale login.";
const RETIRED_TRANSCRIPT_NOTICE: &str = "could not be loaded";

fn reasoning_line(index: usize) -> String {
    format!("thinking fixture line {index} of {REASONING_LINES}")
}

struct PacedProvider {
    address: SocketAddr,
    gate: mpsc::Sender<()>,
    stop: Arc<AtomicBool>,
    turns: Arc<AtomicUsize>,
    profile_requests: Arc<AtomicUsize>,
    acceptor: Option<std::thread::JoinHandle<()>>,
}

struct Served {
    gates: Mutex<mpsc::Receiver<()>>,
    stop: Arc<AtomicBool>,
    turns: Arc<AtomicUsize>,
    profile_requests: Arc<AtomicUsize>,
}

impl PacedProvider {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback provider");
        let address = listener.local_addr().expect("loopback address");
        let (gate, gates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let turns = Arc::new(AtomicUsize::new(0));
        let profile_requests = Arc::new(AtomicUsize::new(0));
        let served = Arc::new(Served {
            gates: Mutex::new(gates),
            stop: Arc::clone(&stop),
            turns: Arc::clone(&turns),
            profile_requests: Arc::clone(&profile_requests),
        });
        let acceptor = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if served.stop.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let served = Arc::clone(&served);
                std::thread::spawn(move || {
                    let _ = serve(stream, &served);
                });
            }
        });
        Self {
            address,
            gate,
            stop,
            turns,
            profile_requests,
            acceptor: Some(acceptor),
        }
    }

    fn model_base(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn account_base(&self) -> String {
        format!("http://{}", self.address)
    }

    fn release(&self, gates: usize) {
        for _ in 0..gates {
            self.gate.send(()).expect("provider gate");
        }
    }

    fn turns(&self) -> usize {
        self.turns.load(Ordering::Acquire)
    }
}

impl Drop for PacedProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

fn serve(stream: TcpStream, served: &Served) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length.min(8 * 1024 * 1024)];
    reader.read_exact(&mut body)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method == "GET" && path.ends_with("/models") {
        return respond_json(
            &mut stream,
            "200 OK",
            &serde_json::json!({"object":"list","data":[{"id":MODEL,"object":"model"}]}),
        );
    }
    if path.starts_with("/api/") {
        served.profile_requests.fetch_add(1, Ordering::AcqRel);
        return respond_json(
            &mut stream,
            "401 Unauthorized",
            &serde_json::json!({"error":"loopback fixture account is signed out"}),
        );
    }
    if method != "POST" {
        return respond_json(
            &mut stream,
            "404 Not Found",
            &serde_json::json!({"error":"not served by the loopback fixture"}),
        );
    }
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let streaming = request["stream"].as_bool().unwrap_or(false);
    let with_tools = request["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty());
    let text = String::from_utf8_lossy(&body);
    let turn = PROMPTS
        .iter()
        .rposition(|prompt| text.contains(prompt))
        .map(|index| index + 1);
    let paced = streaming
        && with_tools
        && turn.is_some_and(|turn| {
            served
                .turns
                .compare_exchange(turn - 1, turn, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        });
    if !streaming {
        return respond_json(
            &mut stream,
            "200 OK",
            &serde_json::json!({
                "id":"loopback","object":"chat.completion","model":MODEL,
                "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            }),
        );
    }
    stream.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
    )?;
    stream.flush()?;
    if paced {
        wait_for_gate(served)?;
        for index in 1..=REASONING_LINES {
            send_delta(
                &mut stream,
                serde_json::json!({"reasoning_content": format!("{}\n", reasoning_line(index))}),
            )?;
        }
        wait_for_gate(served)?;
        send_delta(&mut stream, serde_json::json!({"content": ANSWER}))?;
        wait_for_gate(served)?;
    } else {
        send_delta(&mut stream, serde_json::json!({"content": "ok"}))?;
    }
    send_chunk(
        &mut stream,
        &serde_json::json!({
            "id":"loopback","object":"chat.completion.chunk","model":MODEL,
            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}
        }),
    )?;
    stream.write_all(b"data: [DONE]\n\n")?;
    stream.flush()
}

fn wait_for_gate(served: &Served) -> std::io::Result<()> {
    let gates = served.gates.lock().expect("provider gates");
    loop {
        if served.stop.load(Ordering::Acquire) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        match gates.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
        }
    }
}

fn send_delta(stream: &mut TcpStream, delta: serde_json::Value) -> std::io::Result<()> {
    send_chunk(
        stream,
        &serde_json::json!({
            "id":"loopback","object":"chat.completion.chunk","model":MODEL,
            "choices":[{"index":0,"delta":delta,"finish_reason":null}]
        }),
    )
}

fn send_chunk(stream: &mut TcpStream, chunk: &serde_json::Value) -> std::io::Result<()> {
    stream.write_all(format!("data: {chunk}\n\n").as_bytes())?;
    stream.flush()
}

fn respond_json(
    stream: &mut TcpStream,
    status: &str,
    body: &serde_json::Value,
) -> std::io::Result<()> {
    let body = body.to_string();
    stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.flush()
}

fn prepare(workspace: &SealedWorkspace, provider: &PacedProvider, settings: Option<&str>) {
    let state = workspace.home().join(".codewhale");
    std::fs::write(state.join(".onboarded"), "").unwrap();
    std::fs::write(
        state.join("config.toml"),
        format!(
            "provider = \"deepseek\"\n[providers.deepseek]\nbase_url = {:?}\napi_key = \"loopback-only\"\nmodel = {MODEL:?}\n[notifications]\nmethod = \"off\"\ncompletion_sound = \"off\"\n",
            provider.model_base()
        ),
    )
    .unwrap();
    if let Some(settings) = settings {
        std::fs::write(state.join("settings.toml"), settings).unwrap();
    }
    let trust = workspace.workspace().join(".deepseek");
    std::fs::create_dir_all(&trust).unwrap();
    std::fs::write(trust.join("trusted"), "").unwrap();
}

fn launch(
    workspace: &SealedWorkspace,
    provider: &PacedProvider,
    (rows, cols): (u16, u16),
    session: &str,
    animated: bool,
) -> Harness {
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("SHELL", "/bin/sh")
        .env("CODEWHALE_CLOUD_API_BASE", provider.account_base())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("NO_ANIMATIONS", if animated { "0" } else { "1" })
        .args([
            "--workspace",
            workspace.workspace().to_str().unwrap(),
            "--no-project-config",
            "--skip-onboarding",
            session,
        ])
        .size(rows, cols)
        .spawn()
        .unwrap();
    tui.wait_for_text("accept edits", WAIT)
        .unwrap_or_else(|error| panic!("the dial chip never painted: {error}"));
    tui
}

fn expect(tui: &mut Harness, what: &str, predicate: impl FnMut(&super::qa_harness::Frame) -> bool) {
    if let Err(error) = tui.wait_for(predicate, WAIT) {
        panic!("waiting for {what}: {error}");
    }
}

fn settle(tui: &mut Harness) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut quiet_since = Instant::now();
    while Instant::now() < deadline && quiet_since.elapsed() < Duration::from_millis(300) {
        if tui.pump() {
            quiet_since = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn report(tui: &mut Harness, label: &str) {
    let frame = tui.frame();
    println!(
        "--- {label} at {}x{} ---\n{}",
        frame.cols(),
        frame.rows(),
        frame.debug_dump()
    );
}

fn footer_row(tui: &mut Harness) -> (u16, String) {
    let frame = tui.frame();
    (0..frame.rows())
        .rev()
        .map(|row| (row, frame.row(row)))
        .find(|(_, text)| text.contains("accept edits"))
        .unwrap_or_else(|| panic!("no dial chip on screen:\n{}", frame.debug_dump()))
}

fn assert_status_and_water(tui: &mut Harness, label: &str) {
    let (footer, status) = footer_row(tui);
    let frame = tui.frame();
    for retired in ["operate", "Operate", "Work bar"] {
        assert!(
            !frame.contains(retired),
            "{label}: `{retired}` is on screen:\n{}",
            frame.debug_dump()
        );
    }
    assert!(status.contains("accept edits"), "{label}: {status}");
    for row in 0..footer.saturating_sub(3) {
        let text = frame.row(row);
        let cells: Vec<char> = text.chars().collect();
        let school = cells.windows(2).any(|pair| {
            pair.iter()
                .all(|cell| ('\u{2801}'..='\u{28FF}').contains(cell))
        });
        assert!(
            !school && !text.contains(['˚', '°', '≈', '～']),
            "{label}: ambient art on transcript row {row} `{text}`:\n{}",
            frame.debug_dump()
        );
    }
}

fn thinking_rows(tui: &mut Harness) -> usize {
    let frame = tui.frame();
    (0..frame.rows())
        .filter(|&row| frame.row(row).contains("Thinking…"))
        .count()
}

fn stream_first_turn(tui: &mut Harness, provider: &PacedProvider, thinking_shown: bool) {
    tui.type_line(PROMPTS[0]).unwrap();
    expect(tui, "the first request and its interrupt hint", |frame| {
        provider.turns() == 1 && frame.contains("Esc to interrupt")
    });
    std::thread::sleep(Duration::from_millis(900));
    report(tui, "loading");
    assert_status_and_water(tui, "loading");

    provider.release(1);
    let tail = reasoning_line(REASONING_LINES);
    if thinking_shown {
        expect(tui, "the live thinking block", |frame| {
            frame.contains("Thinking…") && frame.contains(&tail)
        });
    } else {
        expect(tui, "the thinking activity line", |frame| {
            frame.contains("Thinking…")
        });
    }
    std::thread::sleep(Duration::from_millis(1300));
    report(tui, "thinking");
    assert_status_and_water(tui, "thinking");
    assert_eq!(thinking_rows(tui), 1, "one Thinking line");
    let (_, status) = footer_row(tui);
    assert!(
        status.contains("thinking ") && !status.contains("reasoning"),
        "the clock names the phase: {status}"
    );
    let frame = tui.frame();
    assert!(!frame.contains("reasoning..."), "{}", frame.debug_dump());
    assert_eq!(frame.contains(&tail), thinking_shown);
    assert!(
        !frame.contains(&reasoning_line(1)),
        "a long trace stays inside its window:\n{}",
        frame.debug_dump()
    );

    provider.release(1);
    expect(tui, "the streamed answer", |frame| frame.contains(ANSWER));
    report(tui, "answer");
    let frame = tui.frame();
    assert!(!frame.contains("Thinking…"), "{}", frame.debug_dump());
    assert_eq!(frame.contains("Thought for "), thinking_shown);
    assert!(!frame.contains(&tail), "{}", frame.debug_dump());

    provider.release(1);
    expect(tui, "the settled turn", |frame| {
        !frame.contains("Esc to interrupt")
    });
    settle(tui);
    report(tui, "settled");
    assert_status_and_water(tui, "settled");
}

#[test]
fn thinking_streams_stops_and_restores_at_the_shipped_widths() {
    for size in [(24, 80), (32, 100), (32, 120), (40, 140)] {
        let workspace = make_sealed_workspace().unwrap();
        let provider = PacedProvider::start();
        prepare(&workspace, &provider, None);
        let mut tui = launch(&workspace, &provider, size, "--fresh", true);
        stream_first_turn(&mut tui, &provider, true);

        tui.type_line(PROMPTS[1]).unwrap();
        expect(&mut tui, "the second request", |_| provider.turns() == 2);
        provider.release(1);
        let tail = reasoning_line(REASONING_LINES);
        expect(&mut tui, "thinking before the stop", |frame| {
            frame.contains("Thinking…") && frame.contains(&tail)
        });
        tui.send(keys::key::esc()).unwrap();
        expect(&mut tui, "the stopped turn", |frame| {
            !frame.contains("Esc to interrupt") && !frame.contains("Thinking…")
        });
        settle(&mut tui);
        report(&mut tui, "stopped");
        assert_status_and_water(&mut tui, "stopped");
        provider.release(2);

        tui.send(keys::key::ctrl('u')).unwrap();
        settle(&mut tui);
        tui.send(keys::key::ctrl_d()).unwrap();
        tui.wait_for_exit(Duration::from_secs(10));
        tui.shutdown();

        let mut tui = launch(&workspace, &provider, size, "--continue", false);
        expect(&mut tui, "the restored conversation", |frame| {
            frame.contains(ANSWER)
        });
        settle(&mut tui);
        report(&mut tui, "restored");
        assert_status_and_water(&mut tui, "restored");
        let frame = tui.frame();
        assert!(!frame.contains("Thinking…"), "{}", frame.debug_dump());
        tui.shutdown();
    }
}

#[test]
fn a_saved_choice_to_hide_thinking_survives_the_new_default() {
    let workspace = make_sealed_workspace().unwrap();
    let provider = PacedProvider::start();
    prepare(&workspace, &provider, Some("show_thinking = false\n"));
    let mut tui = launch(&workspace, &provider, (24, 80), "--fresh", false);
    stream_first_turn(&mut tui, &provider, false);
    let frame = tui.frame();
    assert!(!frame.contains("Thought"), "{}", frame.debug_dump());
    tui.shutdown();
    let saved: toml::Value = toml::from_str(
        &std::fs::read_to_string(workspace.home().join(".codewhale/settings.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["show_thinking"].as_bool(), Some(false));
}

fn sign_in(workspace: &SealedWorkspace, provider: &PacedProvider, session: &str) {
    let path = workspace.home().join(".codewhale/secrets/secrets.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let secrets =
        codewhale_secrets::Secrets::new(Arc::new(codewhale_secrets::FileKeyringStore::new(path)));
    let bundle = serde_json::from_value(serde_json::json!({
        "tokenType": "Bearer",
        "accessToken": "loopback-access",
        "refreshToken": "loopback-refresh",
        "session": {"id": session},
        "user": {"id": "loopback-account"}
    }))
    .unwrap();
    codewhale_secrets::account::AccountSessionStore::new(secrets, None, &provider.account_base())
        .save(bundle)
        .unwrap();
}

fn run_ungated_turn(tui: &mut Harness, provider: &PacedProvider) {
    provider.release(3);
    tui.type_line(PROMPTS[0]).unwrap();
    expect(tui, "the answer", |frame| frame.contains(ANSWER));
    expect(tui, "the settled turn", |frame| {
        !frame.contains("Esc to interrupt")
    });
}

#[test]
fn account_preferences_notice_is_shown_once_per_sign_in_and_esc_dismisses_it() {
    let workspace = make_sealed_workspace().unwrap();
    let provider = PacedProvider::start();
    prepare(&workspace, &provider, None);
    sign_in(&workspace, &provider, "first-sign-in");

    for cols in [80, 120] {
        let workspace = make_sealed_workspace().unwrap();
        let provider = PacedProvider::start();
        prepare(&workspace, &provider, None);
        sign_in(&workspace, &provider, "first-sign-in");
        let mut tui = launch(&workspace, &provider, (32, cols), "--fresh", false);
        run_ungated_turn(&mut tui, &provider);
        expect(&mut tui, "the standing notice", |frame| {
            frame.contains(NOTICE)
        });
        report(&mut tui, "account notice");
        assert!(provider.profile_requests.load(Ordering::Acquire) > 0);
        let (_, status) = footer_row(&mut tui);
        assert!(
            status.contains(NOTICE),
            "the notice is footer chrome: {status}"
        );
        assert!(!tui.frame().contains(RETIRED_TRANSCRIPT_NOTICE));
        tui.send(keys::key::esc()).unwrap();
        expect(&mut tui, "the dismissed notice", |frame| {
            !frame.contains(NOTICE)
        });
        report(&mut tui, "account notice dismissed");
        tui.shutdown();
    }

    let mut tui = launch(&workspace, &provider, (32, 120), "--fresh", false);
    run_ungated_turn(&mut tui, &provider);
    expect(&mut tui, "the first notice", |frame| frame.contains(NOTICE));
    tui.shutdown();

    let mut tui = launch(&workspace, &provider, (32, 120), "--fresh", false);
    provider.turns.store(0, Ordering::Release);
    run_ungated_turn(&mut tui, &provider);
    tui.wait_for_idle(Duration::from_secs(6), Duration::from_secs(30))
        .unwrap();
    report(&mut tui, "same sign-in, later session");
    assert!(
        !tui.frame().contains(NOTICE),
        "the same sign-in is told once"
    );
    assert!(!tui.frame().contains(RETIRED_TRANSCRIPT_NOTICE));
    tui.shutdown();

    sign_in(&workspace, &provider, "second-sign-in");
    let mut tui = launch(&workspace, &provider, (32, 120), "--fresh", false);
    provider.turns.store(0, Ordering::Release);
    run_ungated_turn(&mut tui, &provider);
    expect(&mut tui, "the notice for a later sign-in", |frame| {
        frame.contains(NOTICE)
    });
    report(&mut tui, "later sign-in");
    tui.shutdown();
}
