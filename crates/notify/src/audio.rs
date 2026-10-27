//! Local audio dispatch. Policy decides the cue; this sink never substitutes a
//! bell for a missing/unsupported WAV. Tests inject a sink and never call an OS
//! player. A single in-flight WAV keeps repeated categories from stacking audio.
use super::sound_policy::SoundCue;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutcome {
    Emitted,
    Dispatched,
    Unsupported,
    Failed,
    Busy,
}

pub fn emit_terminal(cue: &SoundCue, out: &mut dyn Write) -> AudioOutcome {
    let bytes: &[u8] = match cue {
        SoundCue::Bell | SoundCue::Beep => b"\x07",
        SoundCue::DoubleBell => b"\x07\x07",
        SoundCue::Whale | SoundCue::File(_) => return AudioOutcome::Unsupported,
    };
    if out.write_all(bytes).and_then(|()| out.flush()).is_ok() {
        AudioOutcome::Emitted
    } else {
        AudioOutcome::Failed
    }
}

pub const WHALE_WAV: &[u8] = include_bytes!("../assets/audio/codewhale-whale-call.wav");

/// Resolve an external system helper (the audio players `aplay` and the
/// pet's `ffplay`, the Linux browser launcher `xdg-open`) from fixed install
/// prefixes. The ambient `PATH` is never consulted: an empty (`::`), relative
/// (`.`) or repository-local entry would let a checked-out workspace plant a
/// helper that Codewhale then runs with the user's authority.
///
/// Known limitation: a helper installed only outside these prefixes (a
/// custom `~/bin`, a version manager shim) is refused and the caller reports
/// the feature as unavailable. That is deliberate — there is no `PATH`
/// fallback.
pub fn trusted_system_executable(name: &str) -> io::Result<PathBuf> {
    trusted_player_in(name, &trusted_player_dirs())
}

fn trusted_player_in(name: &str, dirs: &[PathBuf]) -> io::Result<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    dirs.iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&file))
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("`{name}` is not installed in a trusted system location"),
            )
        })
}

fn trusted_player_dirs() -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        [
            "/usr/bin",
            "/bin",
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/home/linuxbrew/.linuxbrew/bin",
            "/run/current-system/sw/bin",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
    #[cfg(windows)]
    {
        // Package-manager shim directories from known folders, never PATH.
        let mut found = Vec::new();
        if let Some(local) = dirs::data_local_dir() {
            found.push(local.join("Microsoft").join("WinGet").join("Links"));
        }
        if let Some(home) = dirs::home_dir() {
            found.push(home.join("scoop").join("shims"));
        }
        if let Some(data) = std::env::var_os("ProgramData") {
            found.push(PathBuf::from(data).join("chocolatey").join("bin"));
        }
        found
    }
    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
pub fn dispatch(cue: &SoundCue, out: &mut dyn Write) -> AudioOutcome {
    #[cfg(target_os = "windows")]
    if matches!(cue, SoundCue::Bell | SoundCue::Beep | SoundCue::DoubleBell) {
        use windows::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE;
        let count = if *cue == SoundCue::DoubleBell { 2 } else { 1 };
        for _ in 0..count {
            if unsafe { MessageBeep(MESSAGEBOX_STYLE(0)) }.is_err() {
                return AudioOutcome::Failed;
            }
        }
        return AudioOutcome::Emitted;
    }
    if matches!(cue, SoundCue::Bell | SoundCue::Beep | SoundCue::DoubleBell) {
        return emit_terminal(cue, out);
    }
    dispatch_wav(cue)
}

#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
pub fn dispatch(_cue: &SoundCue, _out: &mut dyn Write) -> AudioOutcome {
    // Production entry points are also fail-closed in library and scoped debug tests.
    AudioOutcome::Unsupported
}

#[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
static PLAYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
fn dispatch_wav(cue: &SoundCue) -> AudioOutcome {
    use std::sync::atomic::Ordering;
    if !cfg!(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux"
    )) {
        return AudioOutcome::Unsupported;
    }
    if PLAYING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return AudioOutcome::Busy;
    }
    let cue = cue.clone();
    match std::thread::Builder::new()
        .name("notification-audio".into())
        .spawn(move || {
            struct Reset;
            impl Drop for Reset {
                fn drop(&mut self) {
                    PLAYING.store(false, Ordering::SeqCst);
                }
            }
            let _reset = Reset;
            if let Err(error) = play_wav(&cue) {
                // Do not log file names or external-player output (both may contain private data).
                tracing::warn!(kind = ?error.kind(), "notification audio playback failed");
            }
        }) {
        Ok(_) => AudioOutcome::Dispatched,
        Err(_) => {
            PLAYING.store(false, Ordering::SeqCst);
            AudioOutcome::Failed
        }
    }
}

#[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
fn play_wav(cue: &SoundCue) -> io::Result<()> {
    let mut bundled = None;
    let path = match cue {
        SoundCue::Whale => {
            let mut file = tempfile::Builder::new()
                .prefix("codewhale-call-")
                .suffix(".wav")
                .tempfile()?;
            file.write_all(WHALE_WAV)?;
            file.flush()?;
            let path = file.path().to_path_buf();
            bundled = Some(file);
            path
        }
        SoundCue::File(path) => std::fs::canonicalize(path)?,
        _ => return Err(io::Error::other("expected WAV cue")),
    };
    // Retain the private temporary file until the synchronous player exits.
    let result = play_file(&path);
    drop(bundled);
    result
}

#[cfg(all(
    not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))),
    target_os = "windows"
))]
fn play_file(path: &std::path::Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_NODEFAULT};
    use windows::core::PCWSTR;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // Synchronous in the worker: the bundled file must outlive playback.
    if unsafe { PlaySoundW(PCWSTR(wide.as_ptr()), None, SND_FILENAME | SND_NODEFAULT) }.as_bool() {
        Ok(())
    } else {
        Err(io::Error::other("audio player failed"))
    }
}

#[cfg(all(
    not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))),
    any(target_os = "macos", target_os = "linux")
))]
fn play_file(path: &std::path::Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let player = PathBuf::from("/usr/bin/afplay");
    #[cfg(target_os = "linux")]
    let player = trusted_system_executable("aplay")?;
    let status = std::process::Command::new(player)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("audio player failed"))
    }
}

#[cfg(all(
    not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))),
    not(any(target_os = "windows", target_os = "macos", target_os = "linux"))
))]
fn play_file(_path: &std::path::Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "WAV playback unsupported",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_whale_is_the_complete_pcm_wav_without_clipped_samples() {
        assert_eq!(&WHALE_WAV[..4], b"RIFF");
        assert_eq!(&WHALE_WAV[8..12], b"WAVE");
        assert_eq!(WHALE_WAV.len(), 136754);
        assert_eq!(u16::from_le_bytes(WHALE_WAV[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(WHALE_WAV[24..28].try_into().unwrap()),
            44100
        );
        assert_eq!(
            u16::from_le_bytes(WHALE_WAV[34..36].try_into().unwrap()),
            16
        );
        let samples: Vec<i16> = WHALE_WAV[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(i16::from_le_bytes)
            .collect();
        assert_eq!(samples.first(), Some(&0));
        assert_eq!(samples.last(), Some(&0));
        assert!(
            samples
                .iter()
                .all(|sample| *sample != i16::MIN && *sample != i16::MAX)
        );
    }

    #[test]
    fn terminal_sink_has_exact_bell_bytes_and_no_wav_fallback() {
        for (cue, bytes) in [
            (SoundCue::Bell, &b"\x07"[..]),
            (SoundCue::Beep, &b"\x07"[..]),
            (SoundCue::DoubleBell, &b"\x07\x07"[..]),
        ] {
            let mut out = Vec::new();
            assert_eq!(emit_terminal(&cue, &mut out), AudioOutcome::Emitted);
            assert_eq!(out, bytes);
        }
        for cue in [SoundCue::Whale, SoundCue::File("missing.wav".into())] {
            let mut out = Vec::new();
            assert_eq!(emit_terminal(&cue, &mut out), AudioOutcome::Unsupported);
            assert!(out.is_empty());
        }
    }

    #[test]
    fn trusted_player_needs_an_executable_file_under_an_absolute_prefix() {
        assert!(trusted_player_dirs().iter().all(|dir| dir.is_absolute()));
        let dir = tempfile::tempdir().unwrap();
        let name = "codewhale-test-player";
        let file = dir.path().join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        });
        let prefixes = [dir.path().to_path_buf()];
        assert!(trusted_player_in(name, &prefixes).is_err(), "absent");
        std::fs::write(&file, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert!(
                trusted_player_in(name, &prefixes).is_err(),
                "a non-executable file is not a player"
            );
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(trusted_player_in(name, &prefixes).unwrap(), file);
    }

    #[test]
    fn production_audio_entry_is_silent_in_library_tests() {
        let mut out = Vec::new();
        assert_eq!(
            dispatch(&SoundCue::Whale, &mut out),
            AudioOutcome::Unsupported
        );
        assert_eq!(
            dispatch(&SoundCue::Bell, &mut out),
            AudioOutcome::Unsupported
        );
        assert!(out.is_empty());
    }
}
