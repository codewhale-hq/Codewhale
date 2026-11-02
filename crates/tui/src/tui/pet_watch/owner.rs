//! Presentation-only companion. Replaces per-view live world ownership; the
//! existing Engine projection, PetNative world and score remain authoritative.
//! No agent, prompt, provider or execution API is available to this process.
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rquickjs::{Context, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{appearance::Appearance, persistence::Store};
use codewhale_util::files::{WorkspaceFile, same_file};

const LEASE: Duration = Duration::from_secs(2);
const OWNER_IDLE_EXIT: Duration = Duration::from_secs(600);
const MAX_CLIENTS: usize = 4096;
/// Request body limit for every owner route. Clients size batches to it.
pub(super) const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// How long a route waits for the world thread's reply before answering 503.
pub(super) const WORK_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub version: u8,
    pub port: u16,
    pub token: String,
    pub identity: String,
}

pub fn directory() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEWHALE_PET_HOME") {
        return Ok(path.into());
    }
    Ok(dirs::home_dir()
        .ok_or_else(|| io::Error::other("Home directory unavailable"))?
        .join(".codewhale/pet-shared"))
}

pub fn descriptor(root: &Path) -> io::Result<Descriptor> {
    let file = WorkspaceFile::open_resolved_root(root, Path::new("connection.json"), false)?
        .open_file()?;
    let mut text = String::new();
    file.take(4097).read_to_string(&mut text)?;
    if text.len() > 4096 {
        return Err(io::Error::other("Invalid pet connection"));
    }
    let d: Descriptor = serde_json::from_str(&text)?;
    if d.version != 1
        || d.port == 0
        || d.token.len() != 64
        || !d.token.bytes().all(|b| b.is_ascii_hexdigit())
        || uuid::Uuid::parse_str(&d.identity).is_err()
    {
        return Err(io::Error::other("Invalid pet connection"));
    }
    Ok(d)
}

#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    seq: u64,
    hash: String,
    cursor: u64,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u8,
    identity: String,
    token: String,
    port: u16,
    source: String,
    source_revision: u64,
    cursor: u64,
    clients: BTreeMap<String, Receipt>,
    recording: Value,
    #[serde(default)]
    appearance: Appearance,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub identity: String,
    pub client: String,
    pub seq: u64,
    pub source_revision: u64,
    pub action: Action,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Interact {
        food: bool,
        x: f64,
        y: f64,
    },
    Select {
        source: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        only_if_idle: bool,
    },
    Appearance {
        appearance: Appearance,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    pub identity: String,
    pub epoch: String,
    pub client: String,
    pub source: String,
    pub source_revision: u64,
    pub seq: u64,
    pub waiting: bool,
    #[serde(default)]
    pub locale: Option<String>,
    pub events: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioLease {
    pub client: String,
    pub enabled: bool,
}

enum Work {
    View,
    Action(Request, tokio::sync::oneshot::Sender<Result<Value, String>>),
    Producer(
        Producer,
        tokio::sync::oneshot::Sender<Result<Value, String>>,
    ),
    Audio(
        AudioLease,
        tokio::sync::oneshot::Sender<Result<Value, String>>,
    ),
    Export(tokio::sync::oneshot::Sender<Result<Value, String>>),
}

#[derive(Clone)]
struct Service {
    descriptor: Descriptor,
    frames: Arc<Mutex<VecDeque<Value>>>,
    tx: mpsc::SyncSender<Work>,
}

fn valid_client(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}
fn valid_source(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b))
}

fn authorized(headers: &HeaderMap, state: &Service) -> bool {
    let origin = format!("http://127.0.0.1:{}", state.descriptor.port);
    let host = format!("127.0.0.1:{}", state.descriptor.port);
    if headers.get("host").and_then(|v| v.to_str().ok()) != Some(host.as_str()) {
        return false;
    }
    if headers
        .get("origin")
        .is_some_and(|o| o.as_bytes() != origin.as_bytes())
    {
        return false;
    }
    let bearer = format!("Bearer {}", state.descriptor.token);
    let cookie = format!("cw_pet={}", state.descriptor.token);
    headers
        .get("authorization")
        .is_some_and(|v| v.as_bytes() == bearer.as_bytes())
        || headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').any(|v| v.trim() == cookie))
}

fn answer(result: Result<Value, String>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::CONFLICT, Json(json!({"error": error}))).into_response(),
    }
}

async fn submit(
    state: &Service,
    make: impl FnOnce(tokio::sync::oneshot::Sender<Result<Value, String>>) -> Work,
) -> Response {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if state.tx.try_send(make(tx)).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match tokio::time::timeout(WORK_REPLY_TIMEOUT, rx).await {
        Ok(Ok(result)) => answer(result),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn frame(
    State(state): State<Service>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>,
) -> Response {
    if !authorized(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let _ = state.tx.try_send(Work::View);
    let Ok(frames) = state.frames.lock() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let found = if let Some(tick) = query.get("tick") {
        let wanted = tick.parse::<u64>().ok();
        frames
            .iter()
            .find(|f| wanted.is_some() && f["tick"].as_u64() == wanted)
    } else {
        frames.back()
    };
    found.map_or_else(
        || StatusCode::NOT_FOUND.into_response(),
        |v| Json(v.clone()).into_response(),
    )
}

async fn action(
    State(state): State<Service>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Response {
    if !authorized(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    submit(&state, |reply| Work::Action(request, reply)).await
}
async fn producer(
    State(state): State<Service>,
    headers: HeaderMap,
    Json(request): Json<Producer>,
) -> Response {
    if !authorized(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    submit(&state, |reply| Work::Producer(request, reply)).await
}
async fn audio(
    State(state): State<Service>,
    headers: HeaderMap,
    Json(request): Json<AudioLease>,
) -> Response {
    if !authorized(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    submit(&state, |reply| Work::Audio(request, reply)).await
}
async fn export(State(state): State<Service>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    submit(&state, Work::Export).await
}
async fn attach(State(state): State<Service>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    (
        [
            (
                "set-cookie",
                format!(
                    "cw_pet={}; HttpOnly; SameSite=Strict; Path=/",
                    state.descriptor.token
                ),
            ),
            ("cache-control", "no-store".into()),
        ],
        Json(json!({"identity":state.descriptor.identity})),
    )
        .into_response()
}

/// Starts only on an explicit pet command or when a view first attaches.
/// The lifetime lock is held across HTTP serving, ticks and all checkpoints.
// `pet serve` is its own console process, never inside the alt-screen: its
// startup line and stop reason are the operator's only output.
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub fn serve(root: PathBuf, requested_port: u16) -> anyhow::Result<()> {
    std::fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(&root)?;
        if !metadata.is_dir() {
            anyhow::bail!("Pet directory must be a directory and cannot be a link");
        }
        // A mount that reports an owner of its own (HarmonyOS hmdfs) makes the
        // uid comparison meaningless; the link refusal above still holds.
        let ownership_proven = metadata.uid() == unsafe { libc::geteuid() }
            || matches!(
                codewhale_config::private_directory::mount_footing(&root),
                codewhale_config::private_directory::PrivateFooting::Synthesised { .. }
            );
        if !ownership_proven {
            anyhow::bail!("Pet directory must belong to this user and cannot be a link");
        }
        // `tighten_mode` keeps the refusal for a filesystem that stores modes
        // and excuses only one that assigns its own.
        codewhale_config::private_directory::tighten_mode(&root, 0o700)?;
    }
    let lock_path = WorkspaceFile::open_resolved_root(&root, Path::new("owner.lock"), true)?;
    let original = lock_path.open_update(true, false)?;
    let mut lifetime = fd_lock::RwLock::new(original.try_clone()?);
    let _guard = lifetime
        .try_write()
        .map_err(|_| anyhow::anyhow!("Another pet owner is running"))?;
    let mut store = Store::at(&root)?;
    let previous = store.load()?;
    let mut saved = if let Some(text) = previous {
        let value: Saved = serde_json::from_str(&text)?;
        if value.version != 1
            || uuid::Uuid::parse_str(&value.identity).is_err()
            || value.token.len() != 64
            || !value.token.bytes().all(|b| b.is_ascii_hexdigit())
            || !valid_source(&value.source)
            || value.clients.len() > MAX_CLIENTS
            || !value.appearance.valid()
        {
            anyhow::bail!("Invalid shared habitat; the existing file was kept");
        }
        value
    } else {
        Saved {
            version: 1,
            identity: uuid::Uuid::new_v4().to_string(),
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            port: requested_port,
            source: "unattached".into(),
            source_revision: 0,
            cursor: 0,
            clients: BTreeMap::new(),
            recording: Value::Null,
            appearance: Appearance::default(),
        }
    };
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, saved.port))?;
    saved.port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let descriptor = Descriptor {
        version: 1,
        port: saved.port,
        token: saved.token.clone(),
        identity: saved.identity.clone(),
    };
    let connection = WorkspaceFile::open_resolved_root(&root, Path::new("connection.json"), true)?;
    let epoch = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::sync_channel(128);
    let frames = Arc::new(Mutex::new(VecDeque::new()));
    let service = Service {
        descriptor: descriptor.clone(),
        frames: frames.clone(),
        tx,
    };
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (ended_tx, ended_rx) = tokio::sync::oneshot::channel();
    let owner = std::thread::Builder::new()
        .name("pet-owner-world".into())
        .spawn(move || {
            let result = run_world(
                saved, store, rx, frames, &epoch, &lock_path, &original, ready_tx,
            );
            if let Err(error) = result {
                eprintln!("Shared pet stopped: {error}");
            }
            let _ = ended_tx.send(());
        })?;
    ready_rx
        .recv_timeout(Duration::from_secs(15))?
        .map_err(anyhow::Error::msg)?;
    connection.replace(&serde_json::to_vec(&descriptor)?)?;
    let router = Router::new()
        .route("/", get(|| async { Html(include_str!("shared.html")) }))
        .route("/pet-native.js", get(|| async { ([("content-type", "text/javascript")], include_str!("pet-native.js")) }))
        .route("/whale-points.tsv", get(|| async { include_str!("../ambient_life/whale-points.tsv") }))
        .route("/pixel-whale.webp", get(|| async { ([("content-type", "image/webp")], include_bytes!("pixel-whale.webp").as_slice()) }))
        .route("/v1/frame", get(frame)).route("/v1/action", post(action))
        .route("/v1/producer", post(producer)).route("/v1/audio", post(audio))
        .route("/v1/export", get(export)).route("/v1/attach", post(attach))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(axum::middleware::from_fn(|request: axum::extract::Request, next: axum::middleware::Next| async move {
            let mut response = next.run(request).await;
            for (key, value) in [("cache-control", "no-store"), ("referrer-policy", "no-referrer"), ("x-content-type-options", "nosniff"), ("content-security-policy", "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")] {
                response.headers_mut().insert(axum::http::HeaderName::from_static(key), axum::http::HeaderValue::from_static(value));
            }
            response
        })).with_state(service);
    println!(
        "Shared pet {} listening on 127.0.0.1:{}",
        descriptor.identity, descriptor.port
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async {
        axum::serve(tokio::net::TcpListener::from_std(listener)?, router)
            .with_graceful_shutdown(async {
                tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=ended_rx=>{} }
            })
            .await
    })?;
    let _ = owner.join();
    Ok(())
}

fn run_world(
    mut saved: Saved,
    mut store: Store,
    rx: mpsc::Receiver<Work>,
    frames: Arc<Mutex<VecDeque<Value>>>,
    epoch: &str,
    lock_path: &WorkspaceFile,
    original: &std::fs::File,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> anyhow::Result<()> {
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(2 * 1024 * 1024);
    let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(10)));
    let limit = deadline.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        limit.lock().map_or(true, |d| Instant::now() > *d)
    })));
    let context = Context::full(&runtime)?;
    context
        .with(|ctx| -> rquickjs::Result<()> {
            let points: Vec<Vec<f64>> = include_str!("../ambient_life/whale-points.tsv")
                .lines()
                .map(|line| {
                    line.split_whitespace()
                        .filter_map(|n| n.parse().ok())
                        .collect()
                })
                .collect();
            ctx.globals()
                .set("points", serde_json::to_string(&points).unwrap())?;
            ctx.eval::<(), _>(include_bytes!("pet-native.js").as_slice())?;
            ctx.eval::<(), _>("globalThis.pet = new PetNative(points, '', '[]', true)")?;
            if !saved.recording.is_null() {
                ctx.globals().set("saved", saved.recording.to_string())?;
                ctx.eval::<(), _>(
                    "pet.restoreRecording(saved); pet.resumeEngine(); delete globalThis.saved",
                )?;
            }
            Ok(())
        })
        .map_err(|_| anyhow::anyhow!("Shared habitat could not restore; its file was kept"))?;
    let mut producer: Option<(String, u64, Instant, String)> = None;
    let mut audio: Option<(String, Instant)> = None;
    let mut playback: Option<(super::audio::Output, super::audio_cursor::AudioCursor)> = None;
    let mut audio_error = false;
    let mut waiting = false;
    let mut last_save = Instant::now();
    let mut storage_error = false;
    let origin = Instant::now();
    let initial_time: f64 = context.with(|ctx| ctx.eval("JSON.parse(pet.snapshot()).timeMs"))?;
    let mut last = origin;
    let mut last_view = origin;
    let mut interface_language = "en".to_string();
    let mut was_active = true;
    let mut idle_since: Option<Instant> = None;
    let frame_interval = Duration::from_secs_f64(1.0 / 30.0);
    let mut ticks = 0u64;
    let mut measurements = VecDeque::<f64>::new();
    save(&context, &mut saved, &mut store)?;
    let _ = ready.send(Ok(()));
    loop {
        *deadline
            .lock()
            .map_err(|_| anyhow::anyhow!("Clock lock failed"))? =
            Instant::now() + Duration::from_secs(5);
        let now = Instant::now();
        if !same_file(&lock_path.open_update(false, false)?, original)? {
            anyhow::bail!("Owner lock was replaced");
        }
        if producer
            .as_ref()
            .is_some_and(|(_, _, seen, _)| now.duration_since(*seen) > LEASE)
        {
            producer = None;
            waiting = false;
            context.with(|ctx| ctx.eval::<(), _>("pet.disconnectEngine()"))?;
        }
        if audio
            .as_ref()
            .is_some_and(|(_, seen)| now.duration_since(*seen) > LEASE)
        {
            audio = None;
        }
        let viewed = now.duration_since(last_view) <= LEASE;
        let active = viewed || producer.is_some() || audio.is_some();
        if active {
            idle_since = None;
        } else if now.duration_since(*idle_since.get_or_insert(now)) > OWNER_IDLE_EXIT {
            save(&context, &mut saved, &mut store)?;
            return Ok(());
        }
        if !active || !was_active {
            last = now;
        }
        let elapsed = now.duration_since(last).as_secs_f64();
        if active && now.duration_since(last) >= frame_interval {
            // A suspended machine advances a bounded amount and marks a gap;
            // offline wall time never invents activity or historical sound.
            let count = (elapsed * 30.0).floor().min(3.0) as u64;
            if elapsed > 0.25 {
                // A delayed clock tick loses observation coverage, not the
                // producer's transport sequence. Keep its lease until LEASE
                // expires above; ordinary scheduler stalls must not reject
                // the next valid packet as an unknown producer.
                waiting = false;
                context.with(|ctx| ctx.eval::<(), _>("pet.disconnectEngine()"))?;
            }
            let started = Instant::now();
            context.with(|ctx| -> rquickjs::Result<()> {
                ctx.globals()
                    .set("at", initial_time + (ticks + count) as f64 * 1000.0 / 30.0)?;
                ctx.globals().set("waiting", waiting)?;
                ctx.eval::<(), _>("pet.advanceEngine(at,true,waiting)")
            })?;
            ticks += count;
            if audio.is_none() {
                playback = None;
            }
            if audio.is_some() && playback.is_none() {
                match super::audio::Output::start() {
                    Ok(output) => {
                        let cursor = super::audio_cursor::AudioCursor::new(
                            output.target(),
                            initial_time + ticks as f64 * 1000.0 / 30.0,
                        );
                        playback = Some((output, cursor));
                        audio_error = false;
                    }
                    Err(_) => {
                        audio = None;
                        audio_error = true;
                    }
                }
            }
            if let Some((output, cursor)) = &mut playback {
                let target = output.target();
                if output.failed()
                    || context
                        .with(|ctx| {
                            cursor.present(
                                &ctx,
                                &target,
                                initial_time + ticks as f64 * 1000.0 / 30.0,
                            )
                        })
                        .is_err()
                {
                    context.with(|ctx| {
                        let _ = ctx.catch();
                    });
                    playback = None;
                    audio = None;
                    audio_error = true;
                }
            }
            last = if elapsed > 0.25 {
                now
            } else {
                last + Duration::from_secs_f64(count as f64 / 30.0)
            };
            if viewed {
                let appearance_json = serde_json::to_string(&saved.appearance)?;
                let text: String = context.with(|ctx| -> rquickjs::Result<String> {
                    ctx.globals()
                        .set("appearanceJSON", appearance_json.as_str())?;
                    ctx.globals()
                        .set("interfaceLanguage", interface_language.as_str())?;
                    ctx.eval("pet.presentation(JSON.parse(appearanceJSON),interfaceLanguage)")
                })?;
                let mut frame: Value = serde_json::from_str(&text)?;
                frame["version"] = json!(1);
                frame["identity"] = json!(saved.identity);
                frame["epoch"] = json!(epoch);
                frame["tick"] =
                    json!((frame["timeMs"].as_f64().unwrap_or(0.0) * 30.0 / 1000.0).round() as u64);
                frame["cursor"] = json!(saved.cursor);
                frame["source"] = json!(saved.source);
                frame["sourceRevision"] = json!(saved.source_revision);
                bind_activity(&mut frame, &saved.source, saved.cursor);
                // Presentation material keeps missing coverage legible. The core
                // pigment, score, particle digest and recording are unchanged.
                for key in ["", "still"] {
                    let pose = if key.is_empty() {
                        &mut frame
                    } else {
                        &mut frame[key]
                    };
                    let hollow =
                        producer.is_none() || pose["style"]["hollow"].as_bool().unwrap_or(true);
                    if hollow {
                        pose["style"]["hollow"] = json!(true);
                        pose["style"]["r"] = json!(153);
                        pose["style"]["g"] = json!(176);
                        pose["style"]["b"] = json!(184);
                        pose["style"]["alpha"] =
                            json!(0.68 * pose["state"]["lit"].as_f64().unwrap_or(1.0).max(0.25));
                    }
                }
                for key in ["", "still"] {
                    let pose = if key.is_empty() {
                        &mut frame
                    } else {
                        &mut frame[key]
                    };
                    if !saved.appearance.event_colors {
                        for (key, value) in
                            ["r", "g", "b"].into_iter().zip(saved.appearance.particle)
                        {
                            pose["style"][key] = json!(value);
                        }
                    }
                    let hollow = pose["style"]["hollow"].as_bool().unwrap_or(true);
                    let lit = pose["state"]["lit"]
                        .as_f64()
                        .unwrap_or(1.0)
                        .clamp(0.18, 1.0);
                    let light = saved
                        .appearance
                        .background
                        .iter()
                        .map(|v| f64::from(*v))
                        .sum::<f64>()
                        > 510.0;
                    if let Some(materials) = pose["materials"].as_array_mut() {
                        for material in materials {
                            if let Some(channels) = material.as_array_mut().filter(|c| c.len() == 4)
                            {
                                for k in 0..3 {
                                    let value = if hollow {
                                        [153., 176., 184.][k]
                                    } else if !saved.appearance.event_colors {
                                        f64::from(saved.appearance.particle[k])
                                    } else {
                                        channels[k].as_f64().unwrap_or(160.0)
                                    };
                                    channels[k] = json!(if light { value * 0.48 } else { value });
                                }
                                let alpha = channels[3].as_f64().unwrap_or(0.5);
                                channels[3] = json!(
                                    (alpha * saved.appearance.brightness * lit).clamp(0.0, 1.0)
                                );
                            }
                        }
                    }
                    pose["style"]["alpha"] = json!(
                        (pose["style"]["alpha"].as_f64().unwrap_or(0.5)
                            * saved.appearance.brightness)
                            .clamp(0.0, 1.0)
                    );
                }
                frame["appearance"] = json!(saved.appearance);
                frame["producerConnected"] = json!(producer.is_some());
                frame["storageAvailable"] = json!(!storage_error);
                frame["audioOwner"] = json!(audio.as_ref().map(|(id, _)| id));
                frame["audioUnavailable"] = json!(audio_error);
                measurements.push_back(started.elapsed().as_secs_f64() * 1000.0);
                if measurements.len() > 300 {
                    measurements.pop_front();
                }
                frame["performance"] = json!({"worldHz":30,"frames":ticks,"uptimeSeconds":origin.elapsed().as_secs_f64(),"workMs":measurements.back()});
                let mut output = frames
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Frame lock failed"))?;
                output.push_back(frame);
                if output.len() > 16 {
                    output.pop_front();
                }
            }
        }
        if (active || was_active || storage_error) && last_save.elapsed() >= Duration::from_secs(1)
        {
            storage_error = save(&context, &mut saved, &mut store).is_err();
            last_save = Instant::now();
        }
        was_active = active;
        let wait = if active {
            frame_interval.saturating_sub(last.elapsed())
        } else {
            Duration::from_secs(1)
        };
        let work = match rx.recv_timeout(wait) {
            Ok(work) => work,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                save(&context, &mut saved, &mut store)?;
                return Ok(());
            }
        };
        last_view = Instant::now();
        match work {
            Work::View => {}
            Work::Export(reply) => {
                let result = context
                    .with(|ctx| super::persistence::export_recording(&ctx, true))
                    .map_err(|_| "Recording export failed".to_owned())
                    .and_then(|text| {
                        serde_json::from_slice(&text).map_err(|_| "Recording export failed".into())
                    });
                let _ = reply.send(result);
            }
            Work::Audio(request, reply) => {
                let result = if !valid_client(&request.client) {
                    Err("Invalid view identity".into())
                } else if !request.enabled {
                    if audio.as_ref().is_some_and(|(id, _)| *id == request.client) {
                        audio = None;
                    }
                    Ok(json!({"granted":false}))
                } else if audio.as_ref().is_none_or(|(id, _)| *id == request.client) {
                    audio = Some((request.client, Instant::now()));
                    Ok(json!({"granted":true}))
                } else {
                    Ok(json!({"granted":false}))
                };
                let _ = reply.send(result);
            }
            Work::Producer(request, reply) => {
                let result = (|| -> Result<Value, String> {
                    if request.identity != saved.identity
                        || request.epoch != epoch
                        || !valid_client(&request.client)
                        || request.source != saved.source
                        || request.source_revision != saved.source_revision
                        || request.events.len() > 64
                        || request.locale.as_ref().is_some_and(|locale| {
                            locale.len() > 35
                                || !locale
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        })
                    {
                        return Err(
                            "Source changed; attach at the current frame and discard stale input"
                                .into(),
                        );
                    }
                    use sha2::{Digest, Sha256};
                    let hash=Sha256::digest(serde_json::to_vec(&json!({"seq":request.seq,"events":request.events,"waiting":request.waiting,"locale":request.locale})).unwrap()).iter().map(|b|format!("{b:02x}")).collect::<String>();
                    if let Some((id, seq, _, prior_hash)) = &producer {
                        if *id != request.client {
                            return Err("This source already has a producer".into());
                        }
                        if request.seq == *seq && *prior_hash == hash {
                            return Ok(
                                json!({"seq":seq,"cursor":saved.cursor,"duplicate":true,"durable":false}),
                            );
                        }
                        if request.seq != seq + 1 {
                            producer = None;
                            waiting = false;
                            context
                                .with(|ctx| ctx.eval::<(), _>("pet.disconnectEngine()"))
                                .map_err(|_| "Coverage reset failed")?;
                            return Err("Producer gap; reconnect without historical input".into());
                        }
                    } else if request.seq != 0 || !request.events.is_empty() {
                        return Err(
                            "Begin a producer lease with sequence zero and no historical events"
                                .into(),
                        );
                    }
                    context
                        .with(|ctx| -> rquickjs::Result<()> {
                            ctx.globals()
                                .set("events", serde_json::to_string(&request.events).unwrap())?;
                            ctx.eval::<(), _>(
                                "pet.observeEngineBatch(events, JSON.parse(pet.snapshot()).timeMs)",
                            )
                        })
                        .map_err(|_| {
                            context.with(|ctx| {
                                let _ = ctx.catch();
                            });
                            "Invalid Engine metadata"
                        })?;
                    if !request.events.is_empty() || waiting != request.waiting {
                        saved.cursor += 1;
                    }
                    producer = Some((request.client, request.seq, Instant::now(), hash));
                    waiting = request.waiting;
                    if let Some(locale) = request.locale {
                        interface_language =
                            if matches!(locale.as_str(), "en" | "zh-Hans" | "zh-Hant" | "ja") {
                                locale
                            } else {
                                "en".into()
                            };
                    }
                    Ok(json!({"seq":request.seq,"cursor":saved.cursor,"durable":false}))
                })();
                let _ = reply.send(result);
            }
            Work::Action(request, reply) => {
                let result = (|| -> Result<Value, String> {
                    use sha2::{Digest, Sha256};
                    if request.identity != saved.identity
                        || !valid_client(&request.client)
                        || request.seq == 0
                    {
                        return Err("Invalid pet action identity".into());
                    }
                    let hash = Sha256::digest(serde_json::to_vec(&request).unwrap())
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>();
                    if let Some(receipt) = saved.clients.get(&request.client) {
                        if request.seq == receipt.seq && hash == receipt.hash {
                            return Ok(json!({"cursor":receipt.cursor,"duplicate":true}));
                        }
                        if request.seq != receipt.seq + 1 {
                            return Err("Action sequence is stale or has a gap".into());
                        }
                    } else if request.seq != 1 || saved.clients.len() >= MAX_CLIENTS {
                        return Err(
                            "Action client is unknown or the retained client limit was reached"
                                .into(),
                        );
                    }
                    if request.source_revision != saved.source_revision {
                        return Err(
                            "Source changed; review the current source before interacting".into(),
                        );
                    }
                    // Check storage before accepting an action. A failed commit
                    // is rolled back together with its idempotency receipt.
                    save(&context, &mut saved, &mut store)
                        .map_err(|_| "Pet storage unavailable; action was not accepted")?;
                    let before = serde_json::to_string(&saved).unwrap();
                    match request.action {
                        Action::Appearance { appearance } => {
                            if !appearance.valid() {
                                return Err("Invalid appearance range".into());
                            }
                            saved.appearance = appearance;
                        }
                        Action::Interact { food, x, y } => {
                            if !x.is_finite() || !y.is_finite() || x.abs() > 1.0 || y.abs() > 1.0 {
                                return Err("Invalid interaction coordinates".into());
                            }
                            context
                                .with(|ctx| -> rquickjs::Result<()> {
                                    ctx.globals().set("x", x)?;
                                    ctx.globals().set("y", y)?;
                                    ctx.globals()
                                        .set("kind", if food { "food" } else { "attention" })?;
                                    ctx.eval::<(), _>("pet.interact(kind,x,y)")
                                })
                                .map_err(|_| "Interaction failed")?;
                        }
                        Action::Select {
                            source,
                            only_if_idle,
                        } => {
                            if !valid_source(&source) {
                                return Err("Invalid source identity".into());
                            }
                            if only_if_idle && producer.is_some() {
                                return Err("The followed source has a live producer".into());
                            }
                            let next_revision = saved
                                .source_revision
                                .checked_add(1)
                                .ok_or("Source revision exhausted")?;
                            saved.source = source;
                            saved.source_revision = next_revision;
                            producer = None;
                            waiting = false;
                            context
                                .with(|ctx| ctx.eval::<(), _>("pet.disconnectEngine()"))
                                .map_err(|_| "Source disconnect failed")?;
                        }
                    }
                    saved.cursor += 1;
                    saved.clients.insert(
                        request.client,
                        Receipt {
                            seq: request.seq,
                            hash,
                            cursor: saved.cursor,
                        },
                    );
                    if save(&context, &mut saved, &mut store).is_err() {
                        saved = serde_json::from_str(&before).unwrap();
                        context.with(|ctx| -> rquickjs::Result<()> { ctx.globals().set("rollback",saved.recording.to_string())?;ctx.eval::<(),_>("pet.restoreRecording(rollback); pet.disconnectEngine(); delete globalThis.rollback") }).map_err(|_| "Storage rollback failed")?;
                        producer = None;
                        waiting = false;
                        storage_error = true;
                        return Err("Pet storage unavailable; action was not accepted".into());
                    }
                    storage_error = false;
                    last_save = Instant::now();
                    Ok(json!({"cursor":saved.cursor,"duplicate":false}))
                })();
                let _ = reply.send(result);
            }
        }
    }
}

fn save(context: &Context, saved: &mut Saved, store: &mut Store) -> anyhow::Result<()> {
    let (text, segment) = context.with(|ctx| -> rquickjs::Result<(String, bool)> {
        let segment: bool = ctx.eval("pet.needsSegment()")?;
        Ok((
            ctx.eval(if segment {
                "pet.prepareSegment()"
            } else {
                "pet.recording(true)"
            })?,
            segment,
        ))
    })?;
    saved.recording = serde_json::from_str(&text)?;
    let archive =
        if segment {
            Some(context.with(|ctx| {
                ctx.eval::<String, _>("JSON.stringify(JSON.parse(pet.recording(true)))")
            })?)
        } else {
            None
        };
    let tick = saved.recording["checkpoint"]["tick"].as_u64().unwrap_or(0);
    store.save_archived(
        &serde_json::to_string(saved)?,
        archive.as_ref().map(|a| (a.as_bytes(), tick)),
    )?;
    if segment {
        context.with(|ctx| ctx.eval::<(), _>("pet.commitSegment()"))?;
    }
    Ok(())
}

/// Bind the reducer's account-neutral owner projection to the session and
/// cursor this owner serves. The reducer never knows either; `Scene::valid`
/// rejects a frame whose projection disagrees with them.
fn bind_activity(frame: &mut Value, source: &str, cursor: u64) {
    if let Some(activity) = frame["activity"].as_object_mut() {
        let session = if source == "unattached" {
            Value::Null
        } else {
            json!(source)
        };
        activity.insert("sessionId".into(), session);
        activity.insert("cursor".into(), json!(cursor));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_protocol::engine_owner::{
        EngineOwnerProjection, OwnerActivityKind, OwnerFreshness, OwnerPresence,
    };

    /// Drive the committed `pet-native.js` bundle (the one the owner and the
    /// worker actually evaluate) and read back its projection the way the
    /// live client does: strict schema, then the contract's invariants.
    fn projection(ctx: &rquickjs::Ctx<'_>, source: &str, cursor: u64) -> EngineOwnerProjection {
        let text: String = ctx.eval("pet.presentation()").unwrap();
        let mut frame: Value = serde_json::from_str(&text).unwrap();
        bind_activity(&mut frame, source, cursor);
        let activity: EngineOwnerProjection =
            serde_json::from_value(frame["activity"].clone()).expect("bundle matches the contract");
        assert!(activity.is_valid(), "invalid projection: {activity:?}");
        assert_eq!(activity.cursor, cursor);
        activity
    }

    #[test]
    fn committed_bundle_projection_satisfies_the_owner_contract() {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            let points: Vec<Vec<f64>> = include_str!("../ambient_life/whale-points.tsv")
                .lines()
                .map(|line| line.split_whitespace().filter_map(|n| n.parse().ok()).collect())
                .collect();
            ctx.globals()
                .set("points", serde_json::to_string(&points).unwrap())
                .unwrap();
            ctx.eval::<(), _>(include_bytes!("pet-native.js").as_slice())
                .unwrap();
            ctx.eval::<(), _>("globalThis.pet = new PetNative(points, '', '[]', true)")
                .unwrap();

            let unattached = projection(&ctx, "unattached", 0);
            assert_eq!(unattached.freshness, OwnerFreshness::Missing);
            assert_eq!(unattached.session_id, None);

            let feed = |events: &str, at: f64| {
                ctx.globals().set("events", events).unwrap();
                ctx.globals().set("at", at).unwrap();
                ctx.eval::<(), _>("pet.observeEngineBatch(events, at); pet.advanceEngine(at + 50, true, false)")
                    .unwrap();
            };
            feed(
                r#"[{"event":"turn_started","turn_id":"turn-1"},
                    {"event":"operation_activity_started","span_id":"call-1","activity_kind":"computer"}]"#,
                0.0,
            );
            let working = projection(&ctx, "session-a", 3);
            assert_eq!(working.session_id.as_deref(), Some("session-a"));
            assert_eq!(working.freshness, OwnerFreshness::Fresh);
            assert_eq!(working.authoritative_presence, OwnerPresence::Working);
            assert_eq!(working.activity_kind, Some(OwnerActivityKind::Computer));

            // The old tool-name feed is refused at the boundary.
            ctx.globals()
                .set(
                    "events",
                    r#"[{"event":"tool_call_started","tool_call_id":"a","tool_name":"exec_shell"}]"#,
                )
                .unwrap();
            assert!(ctx.eval::<(), _>("pet.observeEngineBatch(events, 100)").is_err());
            let _ = ctx.catch();

            feed(
                r#"[{"event":"operation_activity_completed","span_id":"call-1","activity_kind":"computer","outcome":"succeeded"},
                    {"event":"turn_complete","turn_id":"turn-1","turn_outcome":"completed"}]"#,
                200.0,
            );
            let done = projection(&ctx, "session-a", 4);
            assert_eq!(done.authoritative_presence, OwnerPresence::Done);
            assert_eq!(done.done_effect_id.as_deref(), Some("turn-1"));

            feed(
                r#"[{"event":"turn_started","turn_id":"turn-2"},
                    {"event":"turn_complete","turn_id":"turn-2","turn_outcome":"interrupted"}]"#,
                400.0,
            );
            let interrupted = projection(&ctx, "session-a", 5);
            assert_ne!(interrupted.authoritative_presence, OwnerPresence::Done);
            assert_eq!(interrupted.done_effect_id, None);
        });
    }
}
