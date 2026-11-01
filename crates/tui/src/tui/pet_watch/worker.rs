// Legacy per-host replay fixtures only; live production consumers use the companion.
//! Sandboxed, read-only execution of the generated Whalesong world. This reuses
//! the workspace's existing QuickJS dependency; no JS filesystem/network APIs,
//! second Engine, async runtime, Node installation or external process is needed.
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rquickjs::{Context, Runtime};
use serde::Deserialize;

use super::audio::{self, Target};
use super::persistence::{self, Store};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Raster {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<u8>,
    #[serde(rename = "timeMs")]
    pub time_ms: f64,
    /// Host-relative receipt clock, separate from the restored creature clock.
    #[serde(skip)]
    pub host_time_ms: f64,
    pub channel: String,
    pub arch: String,
    pub hollow: bool,
    pub dozing: bool,
    pub lit: f64,
}

pub enum Command {
    Export,
    Observe {
        json: String,
        time_ms: f64,
    },
    Advance {
        time_ms: f64,
        motion: bool,
        waiting: bool,
        width: u16,
        height: u16,
        audio: Option<Target>,
    },
}

pub enum Notice {
    Restored,
    StorageUnavailable,
    Exported(std::path::PathBuf),
    ExportFailed,
}

pub struct Worker {
    pub tx: mpsc::SyncSender<Command>,
    pub latest: Arc<Mutex<Option<Result<Raster, ()>>>>,
    pub notices: mpsc::Receiver<Notice>,
}

use super::persistence::export_recording;

use super::audio_cursor::AudioCursor;

impl Worker {
    pub fn start(session: Option<String>) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(128);
        let (notices_tx, notices) = mpsc::sync_channel(16);
        let latest = Arc::new(Mutex::new(None));
        let output = Arc::clone(&latest);
        std::thread::Builder::new()
            .name("pet-world".into())
            .spawn(move || {
                if run(rx, &output, &notices_tx, session.as_deref()).is_err()
                    && let Ok(mut slot) = output.lock()
                {
                    *slot = Some(Err(()));
                }
            })?;
        Ok(Self {
            tx,
            latest,
            notices,
        })
    }
}

fn run(
    rx: mpsc::Receiver<Command>,
    output: &Mutex<Option<Result<Raster, ()>>>,
    notices: &mpsc::SyncSender<Notice>,
    session: Option<&str>,
) -> Result<(), ()> {
    let runtime = Runtime::new().map_err(|_| ())?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(2 * 1024 * 1024);
    let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(2)));
    let check = Arc::clone(&deadline);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        check.lock().map_or(true, |d| Instant::now() > *d)
    })));
    let context = Context::full(&runtime).map_err(|_| ())?;
    context
        .with(|ctx| -> rquickjs::Result<()> {
            let points: Vec<Vec<f64>> = include_str!("../ambient_life/whale-points.tsv")
                .lines()
                .map(|line| {
                    line.split_whitespace()
                        .filter_map(|s| s.parse().ok())
                        .collect()
                })
                .collect();
            ctx.globals().set(
                "points",
                serde_json::to_string(&points).expect("finite points"),
            )?;
            ctx.eval::<(), _>(include_bytes!("pet-native.js").as_slice())?;
            ctx.eval::<(), _>("globalThis.pet = new PetNative(points, '', '[]', true)")?;
            Ok(())
        })
        .map_err(|_| ())?;
    let mut store = None;
    let mut offset_ms = 0.0;
    if let Some(session) = session {
        // Hydrating a bounded recording validates its whole accepted history.
        // It runs off the UI thread and can take longer than a frame command.
        *deadline.lock().map_err(|_| ())? = Instant::now() + Duration::from_secs(10);
        let loaded = (|| -> Result<Store, ()> {
            let mut files = Store::open(session).map_err(|_| ())?;
            if let Some(saved) = files.load().map_err(|_| ())? {
                offset_ms = context
                    .with(|ctx| -> rquickjs::Result<f64> {
                        ctx.globals().set("savedHabitat", saved)?;
                        ctx.eval("pet.restoreRecording(savedHabitat); delete globalThis.savedHabitat; pet.resumeEngine()")
                    })
                    .map_err(|_| ())?;
                let _ = notices.try_send(Notice::Restored);
            }
            Ok(files)
        })();
        match loaded {
            Ok(files) => store = Some(files),
            Err(()) => {
                // A damaged/concurrent file stays untouched; this run can still
                // show unknown/live telemetry and export its own accepted tape.
                context
                    .with(|ctx| {
                        ctx.eval::<(), _>("delete globalThis.savedHabitat; globalThis.pet = new PetNative(points, '', '[]', true)")
                    })
                    .map_err(|_| ())?;
                offset_ms = 0.0;
                let _ = notices.try_send(Notice::StorageUnavailable);
            }
        }
    }
    let mut saved_at = None;
    let mut audio_cursor: Option<AudioCursor> = None;
    for command in rx {
        // A delayed host can ask for up to 300 fixed ticks (ten seconds).
        // The worker remains interruptible without treating legitimate catch-up
        // as a telemetry failure on a busy machine. The UI never waits here.
        *deadline.lock().map_err(|_| ())? = Instant::now() + Duration::from_secs(5);
        let save_at = match &command {
            Command::Advance { time_ms, .. }
                if saved_at.is_none_or(|last| time_ms - last >= 5_000.0) =>
            {
                Some(*time_ms)
            }
            _ => None,
        };
        context
            .with(|ctx| -> rquickjs::Result<()> {
                match command {
                    Command::Export => {
                        let result = session
                            .ok_or_else(|| std::io::Error::other("No saved session"))
                            .and_then(|id| {
                                let bytes = export_recording(&ctx, false).map_err(|_| {
                                    let _ = ctx.catch();
                                    std::io::Error::other("Pet recording could not be exported")
                                })?;
                                persistence::export(id, &bytes)
                            });
                        let notice = match result {
                            Ok(path) => Notice::Exported(path),
                            Err(_) => Notice::ExportFailed,
                        };
                        let _ = notices.try_send(notice);
                    }
                    Command::Observe { json, time_ms } => {
                        ctx.globals().set("metadata", json)?;
                        ctx.globals().set("timeMs", time_ms + offset_ms)?;
                        ctx.eval::<(), _>("pet.observeEngine(metadata, timeMs)")?;
                    }
                    Command::Advance {
                        time_ms,
                        motion,
                        waiting,
                        width,
                        height,
                        audio,
                    } => {
                        ctx.globals().set("timeMs", time_ms + offset_ms)?;
                        ctx.globals().set("motion", motion)?;
                        ctx.globals().set("waiting", waiting)?;
                        ctx.globals().set("width", width)?;
                        ctx.globals().set("height", height)?;
                        let json: String = ctx.eval(
                            "pet.advanceEngine(timeMs,motion,waiting); pet.terminal(width,height)",
                        )?;
                        let mut frame: Raster =
                            serde_json::from_str(&json).map_err(|_| rquickjs::Error::Unknown)?;
                        if !frame.time_ms.is_finite() || frame.time_ms < offset_ms {
                            return Err(rquickjs::Error::Unknown);
                        }
                        frame.host_time_ms = time_ms;
                        if let Some(target) = audio.filter(Target::active) {
                            *deadline.lock().map_err(|_| rquickjs::Error::Unknown)? =
                                Instant::now() + Duration::from_millis(500);
                            if audio_cursor
                                .as_ref()
                                .is_none_or(|c| !c.target.same_stream(&target))
                            {
                                audio_cursor = Some(AudioCursor {
                                    target: target.clone(),
                                    sample: (frame.time_ms * audio::SAMPLE_RATE as f64 / 1000.0)
                                        .floor()
                                        as usize,
                                    voices: Vec::new(),
                                });
                            }
                            if audio_cursor
                                .as_mut()
                                .expect("initialized audio cursor")
                                .present(&ctx, &target, frame.time_ms)
                                .is_err()
                            {
                                // Sound failure cannot stop telemetry or its recording.
                                let _ = ctx.catch();
                                target.fail();
                                audio_cursor = None;
                            }
                            *deadline.lock().map_err(|_| rquickjs::Error::Unknown)? =
                                Instant::now() + Duration::from_secs(5);
                        } else {
                            audio_cursor = None;
                        }
                        if let Ok(mut slot) = output.lock() {
                            *slot = Some(Ok(frame));
                        }
                    }
                }
                Ok(())
            })
            .map_err(|_| ())?;
        if let Some(at) = save_at {
            save(&context, &mut store, notices)?;
            saved_at = Some(at);
        }
    }
    *deadline.lock().map_err(|_| ())? = Instant::now() + Duration::from_secs(5);
    save(&context, &mut store, notices)?;
    Ok(())
}

fn save(
    context: &Context,
    store: &mut Option<Store>,
    notices: &mpsc::SyncSender<Notice>,
) -> Result<(), ()> {
    if let Some(files) = store {
        let saved = context.with(|ctx| -> rquickjs::Result<()> {
            let segment: Option<String> =
                ctx.eval("pet.needsSegment() ? pet.prepareSegment() : null")?;
            if let Some(text) = segment {
                let archive = export_recording(&ctx, true)?;
                let tick: u64 =
                    ctx.eval("Math.round(JSON.parse(pet.snapshot()).timeMs * 30 / 1000)")?;
                files
                    .save_archived(&text, Some((&archive, tick)))
                    .map_err(|_| rquickjs::Error::Unknown)?;
                ctx.eval::<(), _>("pet.commitSegment()")?;
            } else {
                let text: String = ctx.eval("pet.recording(true)")?;
                files.save(&text).map_err(|_| rquickjs::Error::Unknown)?;
            }
            Ok(())
        });
        if saved.is_err() {
            context.with(|ctx| {
                let _ = ctx.catch();
            });
            *store = None;
            let _ = notices.try_send(Notice::StorageUnavailable);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ArtifactRoot(Option<std::path::PathBuf>);
    impl Drop for ArtifactRoot {
        fn drop(&mut self) {
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
        }
    }

    fn frame(worker: &Worker, at: f64) -> Raster {
        frame_with_audio(worker, at, None)
    }

    fn frame_with_audio(worker: &Worker, at: f64, audio: Option<Target>) -> Raster {
        worker
            .tx
            .send(Command::Advance {
                time_ms: at,
                motion: false,
                waiting: true,
                width: 78,
                height: 22,
                audio,
            })
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(value) = worker.latest.lock().unwrap().take() {
                return value.expect("world worker failed");
            }
            assert!(Instant::now() < until, "worker timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn finish(worker: Worker) {
        let output = Arc::clone(&worker.latest);
        drop(worker);
        let until = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&output) > 1 {
            assert!(Instant::now() < until, "worker did not finish saving");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!matches!(*output.lock().unwrap(), Some(Err(()))));
    }

    #[test]
    fn audio_cursor_preserves_the_shared_score_across_buffer_boundaries() {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        let (sink, _packets) = audio::Output::capture();
        context.with(|ctx| {
            let points: Vec<Vec<f64>> = include_str!("../ambient_life/whale-points.tsv")
                .lines()
                .map(|line| line.split_whitespace().map(|s| s.parse().unwrap()).collect())
                .collect();
            ctx.globals().set("points", serde_json::to_string(&points).unwrap()).unwrap();
            ctx.eval::<(), _>(include_bytes!("pet-native.js").as_slice()).unwrap();
            ctx.eval::<(), _>(r#"globalThis.pet = new PetNative(points, '', '[]', true);
                pet.observeEngine('{"event":"approval_required","id":"audio-test"}', 0);
                globalThis.allVoices = [];"#).unwrap();
            let mut cursor = AudioCursor { target: sink.target(), sample: 0, voices: Vec::new() };
            let mut received = Vec::new();
            // Compare the actual cursor's samples without a wall-clock delivery
            // deadline. The sink tests cover expiry, queue bounds and interleaving.
            for tick in 1..=48 {
                let at = f64::from(tick) * 1000.0 / 30.0;
                ctx.globals().set("timeMs", at).unwrap();
                ctx.eval::<(), _>("pet.advanceEngine(timeMs,false,true); allVoices.push(...JSON.parse(pet.snapshot()).voices)").unwrap();
                let time: f64 = ctx.eval("JSON.parse(pet.snapshot()).timeMs").unwrap();
                let [left, right] = cursor.render_samples(&ctx, time).unwrap().unwrap();
                received.extend(left.iter().zip(&right)
                    .flat_map(|(l, r)| [*l, *r]).flat_map(f32::to_le_bytes));
            }
            let expected: String = ctx.eval(format!("pet.pcm(JSON.stringify(allVoices),0,{},48000)", cursor.sample)).unwrap();
            let [left, right]: [Vec<f32>; 2] = serde_json::from_str(&expected).unwrap();
            let interleaved: Vec<_> = left.iter().zip(&right)
                .flat_map(|(l, r)| [*l, *r]).flat_map(f32::to_le_bytes).collect();
            assert!(left.iter().any(|s| s.abs() > 0.001), "fixture must exercise actual voices");
            assert_eq!(received, interleaved, "host chunking changed the shared PCM");
            // Reopening a stream at the current clock cannot replay its past.
            let (reopened, packets) = audio::Output::capture();
            assert!(!sink.target().same_stream(&reopened.target()));
            cursor.voices.clear();
            cursor.present(&ctx, &reopened.target(), cursor.sample as f64 / 48.0).unwrap();
            assert!(packets.try_recv().is_err());
        });
    }

    #[test]
    fn output_failures_do_not_stop_the_world() {
        let worker = Worker::start(None).unwrap();
        let (sink, packets) = audio::Output::capture();
        drop(packets);
        frame_with_audio(&worker, 0.0, Some(sink.target()));
        let before = frame_with_audio(&worker, 400.0, Some(sink.target()));
        // Provoke the disconnect directly rather than relying on the worker to
        // consume a target inside `Target::current()`'s 500 ms freshness
        // window. `send` returns `Ok(())` early for a stale target and never
        // reaches `try_send`, so on a host slow enough to miss that window the
        // failure is never recorded — which is why this was red on Windows and
        // green everywhere else. This is the same path the worker takes, minus
        // the wall clock: a fresh target, a live `active()`, and a `try_send`
        // that observes `Disconnected` because `packets` was dropped above.
        assert!(
            sink.target().send([vec![0.0], vec![0.0]]).is_err(),
            "a dropped receiver must fail a fresh send"
        );
        assert!(sink.failed());
        worker.tx.send(Command::Export).unwrap();
        assert!(matches!(
            worker
                .notices
                .recv_timeout(Duration::from_secs(10))
                .unwrap(),
            Notice::ExportFailed
        ));
        let after = frame_with_audio(&worker, 800.0, Some(sink.target()));
        assert!(after.time_ms > before.time_ms);
        assert!(after.hollow, "sound failure is not observed telemetry");
        finish(worker);
    }

    #[test]
    fn session_checkpoint_reopens_in_the_actual_worker_without_reviving_an_approval() {
        let _guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let _restore = ArtifactRoot(
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
                root.path().to_owned(),
            )),
        );
        let worker = Worker::start(Some("pet-worker-test".into())).unwrap();
        worker
            .tx
            .send(Command::Observe {
                json: r#"{"event":"approval_required","id":"old-approval"}"#.into(),
                time_ms: 0.0,
            })
            .unwrap();
        let before = frame(&worker, 5_000.0);
        assert_eq!(before.channel, "human");
        worker.tx.send(Command::Export).unwrap();
        let export = match worker
            .notices
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Notice::Exported(path) => path,
            _ => panic!("The live world did not export"),
        };
        let exported: serde_json::Value =
            serde_json::from_slice(&std::fs::read(export).unwrap()).unwrap();
        assert_eq!(exported["checkpoint"]["frame"]["timeMs"], before.time_ms);
        assert!(exported["checkpoint"]["sim"]["particles"].is_array());
        finish(worker);
        let path = root
            .path()
            .join("pet-worker-test/artifacts/pet/habitat.json");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            exported, saved,
            "Export omitted the current pose, score or history"
        );
        let worker = Worker::start(Some("pet-worker-test".into())).unwrap();
        let after = frame(&worker, 800.0);
        assert!(after.hollow);
        assert!(after.time_ms > before.time_ms);
        assert_eq!(after.host_time_ms, 800.0);
        assert!(
            worker
                .notices
                .try_iter()
                .any(|n| matches!(n, Notice::Restored))
        );
        finish(worker);
        let resumed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            &resumed["tape"].as_array().unwrap()[..saved["tape"].as_array().unwrap().len()],
            saved["tape"].as_array().unwrap()
        );
    }
}
