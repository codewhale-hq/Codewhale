//! Live view transport replacing the app-local QuickJS Worker. Only immutable
//! projections cross back to Ratatui; network, raster and encoding stay here.
use super::{graphics, owner};
use codewhale_protocol::engine_owner::EngineOwnerProjection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize, Serialize)]
pub struct Style {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub alpha: f64,
    pub hollow: bool,
    pub channel: String,
    pub arch: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Pose {
    pub points: Vec<[f64; 2]>,
    #[serde(default)]
    pub materials: Option<Vec<[f64; 4]>>,
    pub style: Style,
    pub state: Value,
}
pub type Activity = EngineOwnerProjection;
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub version: u8,
    pub identity: String,
    pub epoch: String,
    pub tick: u64,
    pub cursor: u64,
    pub source: String,
    pub source_revision: u64,
    pub time_ms: f64,
    pub digest: String,
    pub behaviour: String,
    pub producer_connected: bool,
    pub storage_available: bool,
    pub audio_owner: Option<String>,
    pub audio_unavailable: bool,
    pub points: Vec<[f64; 2]>,
    #[serde(default)]
    pub materials: Option<Vec<[f64; 4]>>,
    pub style: Style,
    pub state: Value,
    pub still: Pose,
    #[serde(default)]
    pub appearance: super::appearance::Appearance,
    #[serde(default)]
    pub display_language: Option<String>,
    /// Owner activity is an overlay on the body, never a reason to drop the
    /// frame. A long-lived `pet serve` owner can predate this binary (the
    /// activity shape changed without a frame version bump), so an activity
    /// this client cannot parse reads as none and the pet keeps rendering.
    #[serde(default, deserialize_with = "lenient_activity")]
    pub activity: Option<Activity>,
}
fn lenient_activity<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Activity>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?
        .and_then(|value| serde_json::from_value(value).ok()))
}
impl Scene {
    /// Drop an activity that is invalid or bound to another cursor or
    /// session. Rejecting the whole frame instead would clear queued events,
    /// drop the producer lease and loop every shared view on reconnect.
    fn settle_activity(&mut self) {
        let source = (self.source != "unattached").then_some(self.source.as_str());
        if self.activity.as_ref().is_some_and(|activity| {
            !activity.is_valid()
                || activity.cursor != self.cursor
                || activity.session_id.as_deref() != source
        }) {
            self.activity = None;
        }
    }
    fn valid(&self) -> bool {
        self.version == 1
            && self.time_ms.is_finite()
            && self.points.len() == 980
            && self.still.points.len() == 980
            && [&self.materials, &self.still.materials]
                .into_iter()
                .all(|materials| {
                    materials.as_ref().is_none_or(|values| {
                        values.len() == 980
                            && values.iter().all(|m| {
                                m.iter().all(|v| v.is_finite())
                                    && m[..3].iter().all(|v| (0.0..=255.0).contains(v))
                                    && (0.0..=1.0).contains(&m[3])
                            })
                    })
                })
            && self
                .points
                .iter()
                .chain(&self.still.points)
                .flatten()
                .all(|p| p.is_finite() && p.abs() <= 8.0)
    }
}
#[derive(Clone)]
pub struct Presentation {
    pub client: String,
    pub scene: Scene,
    pub cells: Vec<u8>,
    pub colors: Vec<[u8; 3]>,
    pub image: Option<Vec<u8>>,
    pub width: u16,
    pub height: u16,
    pub created: Instant,
    pub frame_changed: Instant,
    pub render_ms: f64,
    pub bytes: usize,
}
#[derive(Clone, Default)]
pub struct View {
    pub locale: String,
    pub width: u16,
    pub height: u16,
    pub cell_width: f64,
    pub cell_height: f64,
    pub motion: bool,
    pub pixels: bool,
    pub visible: bool,
    pub waiting: bool,
    pub sound: bool,
}
#[derive(Clone)]
pub enum Command {
    Appearance {
        body: Option<super::BodyStyle>,
        language: Option<super::TextLanguage>,
    },
    Observe(String),
    Select,
    Browser,
    Window,
    Export,
}
pub enum Notice {
    AppearanceLegacy,
    Exported(PathBuf),
    Message(String),
    /// The companion cannot be reached: the view thread stopped, or a frame
    /// fetch failed and it is reconnecting. Only this marks the pet offline.
    Unreachable(String),
}
/// Producer events queued for the next `/v1/producer` post. The owner caps a
/// batch at 64 events *and* its request body at [`owner::MAX_REQUEST_BYTES`];
/// a batch that exceeds either is a coverage gap, never a post the owner must
/// refuse (a 64-event batch of 16 KiB events is far over the body limit).
#[derive(Default)]
struct Batch {
    events: Vec<Value>,
    bytes: usize,
}

impl Batch {
    const MAX_EVENTS: usize = 64;
    /// Room left for the post's envelope (identity, epoch, source, sequence).
    const MAX_EVENT_BYTES: usize = owner::MAX_REQUEST_BYTES - 4 * 1024;

    /// Queue one event. `Ok(false)` means it does not fit: the caller drops
    /// the batch and restarts the lease rather than posting a partial one.
    fn push(&mut self, text: &str) -> io::Result<bool> {
        // `+ 1` counts the separating comma in the posted JSON array.
        let bytes = self.bytes + text.len() + 1;
        if self.events.len() >= Self::MAX_EVENTS || bytes > Self::MAX_EVENT_BYTES {
            return Ok(false);
        }
        self.events.push(serde_json::from_str(text)?);
        self.bytes = bytes;
        Ok(true)
    }

    fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
    }
}

pub struct Worker {
    pub tx: mpsc::SyncSender<Command>,
    pub latest: Arc<Mutex<Option<Presentation>>>,
    pub view: Arc<Mutex<View>>,
    pub notices: mpsc::Receiver<Notice>,
}

pub struct Client {
    pub descriptor: owner::Descriptor,
    http: reqwest::blocking::Client,
}
impl Client {
    pub fn connect() -> io::Result<Self> {
        let root = owner::directory()?;
        let http = crate::tls::reqwest_blocking_client_builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(io::Error::other)?;
        let attempt = || -> io::Result<Self> {
            Ok(Self {
                descriptor: owner::descriptor(&root)?,
                http: http.clone(),
            })
        };
        if let Ok(client) = attempt()
            && client.get("/v1/frame").is_ok()
        {
            return Ok(client);
        }
        #[cfg(not(test))]
        {
            use std::process::{Command as Process, Stdio};
            let mut process = Process::new(std::env::current_exe()?);
            process
                .args(["pet", "serve"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    process.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                process.creation_flags(0x08000000 | 0x00000008);
            }
            let mut child = process.spawn()?;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(5) {
            if let Ok(client) = attempt()
                && client.get("/v1/frame").is_ok()
            {
                return Ok(client);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(io::Error::other(
            "Shared pet unavailable. Run codewhale pet serve; existing recordings are preserved.",
        ))
    }
    pub fn get(&self, path: &str) -> io::Result<Value> {
        self.request(path, None)
    }
    pub fn post(&self, path: &str, body: &Value) -> io::Result<Value> {
        self.request(path, Some(body))
    }
    fn request(&self, path: &str, body: Option<&Value>) -> io::Result<Value> {
        let url = format!("http://127.0.0.1:{}{path}", self.descriptor.port);
        let mut request = if let Some(body) = body {
            self.http.post(url).json(body)
        } else {
            self.http.get(url)
        };
        if path == "/v1/export" {
            // The owner serializes the recording on its world thread for up
            // to its own work bound; giving up sooner abandons an export the
            // owner still completes. Other calls keep the 2 s client bound.
            //
            // Known limitation (U06-m4): export runs synchronously in the
            // owner's single QuickJS world, so the world answers no other work
            // until it finishes. Keeping it responsive needs an incremental
            // export in pet-native.js; aligning this wait is only the client
            // half.
            request = request.timeout(owner::WORK_REPLY_TIMEOUT + Duration::from_secs(1));
        }
        let response = request
            .bearer_auth(&self.descriptor.token)
            .send()
            .map_err(io::Error::other)?;
        let status = response.status();
        let success = status.is_success();
        let bound = if path == "/v1/export" {
            super::persistence::MAX_EXPORT_BYTES
        } else {
            8 * 1024 * 1024
        };
        let mut bytes = Vec::new();
        response.take(bound as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > bound {
            return Err(io::Error::other("Pet response exceeds its bound"));
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        if !success {
            let message = value["error"]
                .as_str()
                .unwrap_or("Shared pet connection failed");
            return Err(io::Error::new(
                if status == reqwest::StatusCode::CONFLICT && !message.contains("storage") {
                    io::ErrorKind::InvalidInput
                } else {
                    io::ErrorKind::Other
                },
                message,
            ));
        }
        Ok(value)
    }
    pub fn open_browser(&self) -> io::Result<()> {
        let url = format!(
            "http://127.0.0.1:{}/#{}",
            self.descriptor.port, self.descriptor.token
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use std::process::{Command as Process, Stdio};
            // `webbrowser` resolves `$BROWSER` and `xdg-open` through the
            // environment and `PATH`, where a workspace could shadow the
            // launcher and receive this URL's owner token. Use the launcher
            // from a trusted system prefix only (U06-06).
            let launcher = crate::notify::audio::trusted_system_executable("xdg-open")?;
            let mut child = Process::new(launcher)
                .arg(&url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            // Some launchers stay in the foreground with the browser; reap it
            // off this worker thread so the pet view keeps painting.
            std::thread::Builder::new()
                .name("pet-browser-launcher".into())
                .spawn(move || {
                    let _ = child.wait();
                })?;
            Ok(())
        }
        // macOS (LaunchServices) and Windows (ShellExecute) resolve the
        // default browser through the OS, not `PATH`.
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            webbrowser::open(&url).map_err(io::Error::other)
        }
    }
    pub fn open_window(&self) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            use std::process::{Command as Process, Stdio};
            // The system launcher by absolute path: a bare `open` resolves
            // through `PATH`, where a workspace entry could shadow it.
            let mut process = Process::new("/usr/bin/open");
            if let Some(path) = std::env::var_os("CODEWHALE_PET_APP") {
                process.arg(path);
            } else {
                process.args(["-a", "Codewhale Pet"]);
            }
            process
                .args(["--args", "--companion"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if process.status()?.success() {
                return Ok(());
            }
            Err(io::Error::other(
                "Build or install the Codewhale Pet app to open its companion window",
            ))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(io::Error::other(
                "The native companion window is currently available on macOS",
            ))
        }
    }
}
impl Worker {
    pub fn start(session: Option<String>) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(128);
        let (latest, (notices_tx, notices)) = (Arc::new(Mutex::new(None)), mpsc::sync_channel(16));
        let output = latest.clone();
        let view = Arc::new(Mutex::new(View {
            width: 40,
            height: 8,
            ..View::default()
        }));
        let settings = view.clone();
        std::thread::Builder::new()
            .name("pet-view".into())
            .spawn(move || {
                if let Err(e) = run(rx, &output, &notices_tx, &settings, session) {
                    let _ = notices_tx.try_send(Notice::Unreachable(e.to_string()));
                }
            })?;
        Ok(Self {
            tx,
            latest,
            notices,
            view,
        })
    }
}
pub(super) fn session_source(session: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "session:{}",
        Sha256::digest(session.as_bytes())
            .iter()
            .take(10)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn run(
    rx: mpsc::Receiver<Command>,
    output: &Mutex<Option<Presentation>>,
    notices: &mpsc::SyncSender<Notice>,
    settings: &Mutex<View>,
    session: Option<String>,
) -> io::Result<()> {
    let source = session.as_deref().map(session_source);
    let mut renderer = graphics::Renderer::default();
    let id = uuid::Uuid::new_v4().to_string();
    let mut client = Client::connect()?;

    let mut scene: Option<Scene> = None;
    let mut previous = None;
    let mut changed = Instant::now();
    let mut fetched = Instant::now() - Duration::from_secs(1);
    let mut encoded = Instant::now() - Duration::from_secs(1);
    let mut events = Batch::default();
    let mut producer_seq = None;
    let mut sequence = 0u64;
    let mut action: Option<Value> = None;
    let mut appearance_patch: Option<(Option<super::BodyStyle>, Option<super::TextLanguage>)> =
        None;
    let mut last_action = Instant::now() - Duration::from_secs(1);
    let mut last_produce = Instant::now();
    let mut last_audio = Instant::now();
    let mut last_failure = Instant::now() - Duration::from_secs(10);
    loop {
        let view = settings
            .lock()
            .map_err(|_| io::Error::other("Pet view lock failed"))?
            .clone();
        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(command) => match command {
                Command::Appearance { body, language } => {
                    let patch = appearance_patch.get_or_insert((None, None));
                    patch.0 = body.or(patch.0);
                    patch.1 = language.or(patch.1);
                }
                Command::Observe(text) => {
                    if !events.push(&text)? {
                        // Overflow is a coverage gap, never a truncated batch:
                        // restart the lease from sequence zero.
                        events.clear();
                        producer_seq = None;
                    }
                }
                Command::Select => {
                    if let (Some(s), Some(source)) = (&scene, &source)
                        && action.is_none()
                    {
                        action = Some(
                            json!({"identity":s.identity,"client":id,"seq":sequence+1,"source_revision":s.source_revision,"action":{"kind":"select","source":source}}),
                        );
                    } else {
                        let _ = notices.try_send(Notice::Message(
                            "Save this session and wait for the pet connection before selecting its source.".into(),
                        ));
                    }
                }
                Command::Browser => {
                    if let Err(e) = client.open_browser() {
                        let _ = notices.try_send(Notice::Message(e.to_string()));
                    }
                }
                Command::Window => {
                    if let Err(e) = client.open_window() {
                        let _ = notices.try_send(Notice::Message(e.to_string()));
                    }
                }
                Command::Export => {
                    let result = client.get("/v1/export").and_then(|r| {
                        super::persistence::export(
                            session.as_deref().ok_or_else(|| {
                                io::Error::other("Save the terminal session before exporting")
                            })?,
                            &serde_json::to_vec(&r)?,
                        )
                    });
                    let _ = notices.try_send(match result {
                        Ok(path) => Notice::Exported(path),
                        Err(e) => Notice::Message(e.to_string()),
                    });
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = client.post("/v1/audio", &json!({"client":id,"enabled":false}));
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if fetched.elapsed() >= Duration::from_millis(if view.visible { 30 } else { 400 }) {
            match client
                .get("/v1/frame")
                .and_then(|value| serde_json::from_value::<Scene>(value).map_err(io::Error::other))
                .map(|mut next| {
                    next.settle_activity();
                    next
                }) {
                Ok(next) if next.valid() => {
                    if scene
                        .as_ref()
                        .is_none_or(|s| s.epoch != next.epoch || s.identity != next.identity)
                    {
                        previous = None;
                        changed = Instant::now();
                        producer_seq = None;
                        events.clear();
                    } else if scene.as_ref().is_some_and(|s| s.tick != next.tick) {
                        previous = scene.clone();
                        changed = Instant::now();
                    }
                    if next.source == "unattached"
                        && let Some(source) = &source
                        && action.is_none()
                    {
                        action = Some(
                            json!({"identity":next.identity,"client":id,"seq":sequence+1,"source_revision":next.source_revision,"action":{"kind":"select","source":source}}),
                        );
                    }
                    scene = Some(next);
                    fetched = Instant::now();
                }
                _ => {
                    fetched = Instant::now();
                    events.clear();
                    producer_seq = None;
                    if last_failure.elapsed() > Duration::from_secs(3) {
                        last_failure = Instant::now();
                        let _ =
                            notices.try_send(Notice::Unreachable("Shared pet reconnecting".into()));
                        if let Ok(next) = Client::connect() {
                            client = next;
                        }
                    }
                    continue;
                }
            }
        }
        let Some(s) = &scene else { continue };
        if action.is_none()
            && let Some((body, language)) = appearance_patch.take()
        {
            if s.display_language.is_none() {
                let _ = notices.try_send(Notice::AppearanceLegacy);
            } else {
                let mut appearance = s.appearance.clone();
                if let Some(body) = body {
                    appearance.body_style = body;
                }
                if let Some(language) = language {
                    appearance.text_language = language;
                }
                action = Some(
                    json!({"identity":s.identity,"client":id,"seq":sequence+1,"source_revision":s.source_revision,"action":{"kind":"appearance","appearance":appearance}}),
                );
            }
        }
        if last_action.elapsed() >= Duration::from_millis(250)
            && let Some(pending) = &action
        {
            last_action = Instant::now();
            match client.post("/v1/action", pending) {
                Ok(_) => {
                    let selected = pending["action"]["kind"] == "select";
                    sequence += 1;
                    action = None;
                    if selected {
                        fetched = Instant::now() - Duration::from_secs(1);
                        producer_seq = None;
                        events.clear();
                    }
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::InvalidInput {
                        action = None;
                    }
                    if last_failure.elapsed() > Duration::from_secs(3) {
                        last_failure = Instant::now();
                        let _ = notices.try_send(Notice::Message(e.to_string()));
                    }
                }
            }
        }
        if source.as_deref() == Some(s.source.as_str())
            && last_produce.elapsed() >= Duration::from_millis(100)
        {
            let seq = producer_seq.map_or(0, |seq| seq + 1);
            if seq == 0 {
                events.clear();
            }
            let mut body = json!({"identity":s.identity,"epoch":s.epoch,"client":id,"source":s.source,"source_revision":s.source_revision,"seq":seq,"waiting":view.waiting,"events":events.events});
            if s.display_language.is_some() {
                body["locale"] = json!(view.locale);
            }
            producer_seq = client.post("/v1/producer", &body).ok().map(|_| seq);
            events.clear();
            last_produce = Instant::now();
        } else if source.as_deref() != Some(s.source.as_str()) {
            events.clear();
            producer_seq = None;
        }
        if last_audio.elapsed() >= Duration::from_millis(500) {
            let _=client.post("/v1/audio",&json!({"client":id,"enabled":view.sound&&view.visible&&changed.elapsed()<Duration::from_millis(500)}));
            last_audio = Instant::now();
        }
        if view.visible
            && encoded.elapsed()
                >= Duration::from_millis(if view.motion && view.pixels {
                    16
                } else if view.motion {
                    33
                } else {
                    200
                })
        {
            let began = Instant::now();
            let mut pose = if view.motion {
                Pose {
                    points: s.points.clone(),
                    materials: s.materials.clone(),
                    style: s.style.clone(),
                    state: s.state.clone(),
                }
            } else {
                s.still.clone()
            };
            if !s.producer_connected {
                pose.style.hollow = true;
            }
            let fraction = if view.motion {
                (changed.elapsed().as_secs_f64() * 30.0).clamp(0.0, 1.0)
            } else {
                1.0
            };
            renderer.set_appearance(&s.appearance);
            let (cells, image) = renderer.render(
                &pose,
                previous.as_ref().filter(|_| view.motion),
                fraction,
                &view,
                if view.motion { s.time_ms / 1000.0 } else { 0.0 },
            )?;
            let bytes = image.as_ref().map_or(0, Vec::len);
            if let Ok(mut slot) = output.lock() {
                *slot = Some(Presentation {
                    client: id.clone(),
                    scene: s.clone(),
                    cells,
                    colors: renderer.colors.clone(),
                    image,
                    width: view.width,
                    height: view.height,
                    created: Instant::now(),
                    frame_changed: changed,
                    render_ms: began.elapsed().as_secs_f64() * 1000.0,
                    bytes,
                });
            }
            encoded = began;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// U06-03: a producer batch is bounded by the owner's request body limit,
    /// not only by its event count, so the worker never posts a body the
    /// owner must refuse (64 events of 16 KiB each is ~1 MiB).
    #[test]
    fn producer_batch_fits_the_owner_body_limit() {
        let mut batch = Batch::default();
        let large = format!("{{\"event\":\"x\",\"id\":\"{}\"}}", "a".repeat(16_000));
        let mut accepted = 0;
        while batch.push(&large).unwrap() {
            accepted += 1;
        }
        assert!(accepted > 0 && accepted < Batch::MAX_EVENTS, "{accepted}");
        let posted = serde_json::to_vec(&json!({"seq": u64::MAX, "events": batch.events})).unwrap();
        assert!(posted.len() <= owner::MAX_REQUEST_BYTES, "{}", posted.len());

        batch.clear();
        let small = "{\"event\":\"tool_call_heartbeat\"}";
        for _ in 0..Batch::MAX_EVENTS {
            assert!(batch.push(small).unwrap());
        }
        assert!(!batch.push(small).unwrap(), "the event count still bounds");
    }
}
