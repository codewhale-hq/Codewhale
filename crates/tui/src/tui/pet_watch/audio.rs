//! A bounded presentation sink for the shared core's stereo PCM. No score,
//! event interpretation or simulation clock lives in the audio player.
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub const SAMPLE_RATE: usize = 48_000;
pub const MAX_FRAMES: usize = SAMPLE_RATE / 2;

pub(super) struct Packet {
    pub(super) bytes: Vec<u8>,
    created: Instant,
}

#[derive(Default)]
struct Control {
    child: Mutex<Option<Child>>,
    cancelled: AtomicBool,
    failed: AtomicBool,
}

#[derive(Clone)]
pub struct Target {
    tx: mpsc::SyncSender<Packet>,
    control: Arc<Control>,
    requested: Instant,
}

impl Target {
    pub fn same_stream(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.control, &other.control)
    }

    pub fn active(&self) -> bool {
        !self.control.cancelled.load(Ordering::Acquire)
            && !self.control.failed.load(Ordering::Acquire)
    }

    pub fn fail(&self) {
        self.control.failed.store(true, Ordering::Release);
    }

    pub fn current(&self) -> bool {
        self.requested.elapsed() <= Duration::from_millis(500)
    }

    /// `Ok` means the samples were valid and the stream is live, not that they
    /// were played: a full queue or a stale request drops them. Live audio is
    /// a bounded real-time sink — late samples never queue behind the world
    /// clock — so an overrun is a dropout, not a stream failure.
    pub fn send(&self, channels: [Vec<f32>; 2]) -> Result<(), ()> {
        let length = channels[0].len();
        if length > MAX_FRAMES
            || channels[1].len() != length
            || channels
                .iter()
                .flatten()
                .any(|s| !s.is_finite() || s.abs() > 1.0)
        {
            self.fail();
            return Err(());
        }
        if !self.active() {
            return Err(());
        }
        if !self.current() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(length * 8);
        for (left, right) in channels[0].iter().zip(&channels[1]) {
            bytes.extend_from_slice(&left.to_le_bytes());
            bytes.extend_from_slice(&right.to_le_bytes());
        }
        match self.tx.try_send(Packet {
            bytes,
            created: self.requested,
        }) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.fail();
                Err(())
            }
        }
    }
}

pub struct Output {
    target: Target,
}

impl Output {
    #[cfg(test)]
    pub(super) fn capture() -> (Self, mpsc::Receiver<Packet>) {
        let (tx, rx) = mpsc::sync_channel(4);
        (
            Self {
                target: Target {
                    tx,
                    control: Arc::new(Control::default()),
                    requested: Instant::now(),
                },
            },
            rx,
        )
    }

    #[cfg(not(test))]
    pub fn start() -> io::Result<Self> {
        Self::spawn(player_command()?)
    }

    #[cfg(test)]
    pub fn start() -> io::Result<Self> {
        // Product/library tests never open the user's audio output.
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "audio disabled in tests",
        ))
    }

    fn spawn(mut command: Command) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(4);
        let control = Arc::new(Control::default());
        let thread_control = Arc::clone(&control);
        std::thread::Builder::new()
            .name("pet-audio".into())
            .spawn(move || {
                let result = play(&mut command, rx, &thread_control);
                if result.is_err() && !thread_control.cancelled.load(Ordering::Acquire) {
                    thread_control.failed.store(true, Ordering::Release);
                }
                let child = thread_control
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take());
                if let Some(mut child) = child {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            })?;
        Ok(Self {
            target: Target {
                tx,
                control,
                requested: Instant::now(),
            },
        })
    }

    pub fn target(&self) -> Target {
        Target {
            requested: Instant::now(),
            ..self.target.clone()
        }
    }
    pub fn failed(&self) -> bool {
        self.target.control.failed.load(Ordering::Acquire)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.target.control.cancelled.store(true, Ordering::Release);
        // Closing or muting Watch interrupts even a blocked pipe write. Reaping
        // happens in the audio thread, never in the terminal event loop.
        if let Ok(mut slot) = self.target.control.child.lock()
            && let Some(child) = slot.as_mut()
        {
            let _ = child.kill();
        }
    }
}

/// The pet's PCM player: `ffplay` from a fixed install prefix, never from the
/// ambient `PATH` (see [`codewhale_notify::audio::trusted_system_executable`]). A player
/// that is not installed there fails the start and Watch reports audio as
/// unavailable.
fn player_command() -> io::Result<Command> {
    let mut command = Command::new(codewhale_notify::audio::trusted_system_executable(
        "ffplay",
    )?);
    command.args([
        "-nodisp",
        "-autoexit",
        "-loglevel",
        "error",
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-f",
        "f32le",
        "-sample_rate",
        "48000",
        "-ch_layout",
        "stereo",
        "-i",
        "pipe:0",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    Ok(command)
}

fn play(command: &mut Command, rx: mpsc::Receiver<Packet>, control: &Control) -> io::Result<()> {
    if control.cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let Some(mut input) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("audio pipe unavailable"));
    };
    match control.child.lock() {
        Ok(mut slot) => *slot = Some(child),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("audio lock failed"));
        }
    }
    while !control.cancelled.load(Ordering::Acquire) && !control.failed.load(Ordering::Acquire) {
        if control
            .child
            .lock()
            .map_err(|_| io::Error::other("audio lock failed"))?
            .as_mut()
            .is_none_or(|c| c.try_wait().map_or(true, |status| status.is_some()))
        {
            return Err(io::Error::other("audio player exited"));
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(packet) => {
                if packet.created.elapsed() > Duration::from_millis(500) {
                    continue;
                }
                input.write_all(&packet.bytes)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < until, "audio process did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn receiver() -> (Target, mpsc::Receiver<Packet>) {
        let (tx, rx) = mpsc::sync_channel(4);
        (
            Target {
                tx,
                control: Arc::new(Control::default()),
                requested: Instant::now(),
            },
            rx,
        )
    }

    #[test]
    fn pcm_boundary_preserves_samples_rejects_invalid_input_and_discards_backlog() {
        let (target, rx) = receiver();
        target.send([vec![0.125, -0.5], vec![0.25, 0.75]]).unwrap();
        let bytes: Vec<_> = [0.125_f32, 0.25, -0.5, 0.75]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(rx.recv().unwrap().bytes, bytes);
        for channels in [
            [vec![0.0], vec![]],
            [vec![f32::NAN], vec![0.0]],
            [vec![1.01], vec![0.0]],
            [vec![0.0; MAX_FRAMES + 1], vec![0.0; MAX_FRAMES + 1]],
        ] {
            let (target, rx) = receiver();
            assert!(target.send(channels).is_err());
            assert!(!target.active());
            assert!(rx.try_recv().is_err());
        }
        let (mut target, rx) = receiver();
        target.requested = Instant::now() - Duration::from_secs(1);
        assert!(target.send([vec![0.0], vec![0.0]]).is_ok());
        assert!(target.active());
        assert!(rx.try_recv().is_err());
        let (target, rx) = receiver();
        for _ in 0..4 {
            target.send([vec![0.0], vec![0.0]]).unwrap();
        }
        assert!(target.send([vec![0.0], vec![0.0]]).is_ok());
        assert!(target.active());
        assert_eq!(rx.try_iter().count(), 4);
    }

    /// Only a test subprocess with this explicit environment enters the sink.
    /// Ordinary library/nextest runs return without launching or playing audio.
    #[test]
    fn pet_audio_fake_player() {
        let Some(path) = std::env::var_os("CODEWHALE_TEST_PET_AUDIO_OUTPUT") else {
            return;
        };
        let mut file = std::fs::File::create(path).unwrap();
        if std::env::var_os("CODEWHALE_TEST_PET_AUDIO_HOLD").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        } else {
            std::io::copy(&mut std::io::stdin().lock(), &mut file).unwrap();
        }
    }

    fn fake_player(path: &std::path::Path, hold: bool) -> Command {
        let module = module_path!().split_once("::").unwrap().1;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("{module}::pet_audio_fake_player"),
                "--nocapture",
            ])
            .env("CODEWHALE_TEST_PET_AUDIO_OUTPUT", path);
        if hold {
            command.env("CODEWHALE_TEST_PET_AUDIO_HOLD", "1");
        }
        command
    }

    #[test]
    fn muting_reaps_the_player_even_with_a_retained_target_and_blocked_pipe() {
        for hold in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("received.f32");
            let output = Output::spawn(fake_player(&path, hold)).unwrap();
            wait_until(|| path.exists());
            let target = output.target();
            let control = Arc::clone(&target.control);
            target
                .send([vec![0.125; MAX_FRAMES], vec![-0.25; MAX_FRAMES]])
                .unwrap();
            if !hold {
                wait_until(|| std::fs::metadata(&path).unwrap().len() == (MAX_FRAMES * 8) as u64);
            }
            drop(output);
            assert!(!target.active());
            assert!(target.send([vec![0.0], vec![0.0]]).is_err());
            // Only this test and the deliberately retained target remain. The
            // player thread must have returned after killing and reaping it.
            wait_until(|| Arc::strong_count(&control) == 2);
            assert!(control.child.lock().unwrap().is_none());
        }
    }

    /// U06-06: the player is an absolute path from a trusted prefix or it is
    /// refused — never a bare name the ambient `PATH` resolves, where an empty
    /// or repository-local entry would run a workspace-planted `ffplay`.
    #[test]
    fn pet_player_is_never_resolved_through_path() {
        match player_command() {
            Ok(command) => assert!(
                std::path::Path::new(command.get_program()).is_absolute(),
                "player resolved through PATH: {:?}",
                command.get_program()
            ),
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
        }
    }

    #[test]
    fn missing_player_fails_without_opening_a_device_or_blocking_the_caller() {
        assert!(Output::start().is_err());
        let dir = tempfile::tempdir().unwrap();
        let output = Output::spawn(Command::new(dir.path().join("missing-player"))).unwrap();
        wait_until(|| output.failed());
        assert!(!output.target().active());
    }
}
