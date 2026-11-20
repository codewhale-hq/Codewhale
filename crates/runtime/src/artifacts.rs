//! Session-scoped artifact metadata.
//!
//! Large tool outputs are written under the owning session directory and saved
//! sessions keep a durable metadata index for resume/listing flows.

use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const ARTIFACTS_DIR_NAME: &str = "artifacts";

#[cfg(any(test, feature = "test-support"))]
static TEST_ARTIFACT_SESSIONS_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
pub static TEST_ARTIFACT_SESSIONS_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    ToolOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub kind: ArtifactKind,
    #[serde(default)]
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub created_at: DateTime<Utc>,
    pub byte_size: u64,
    pub preview: String,
    pub storage_path: PathBuf,
}

fn sanitize_id_component(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[must_use]
pub fn artifact_id_for_tool_call(tool_call_id: &str) -> String {
    format!("art_{}", sanitize_id_component(tool_call_id))
}

#[must_use]
pub fn session_artifact_relative_path(artifact_id: &str) -> PathBuf {
    PathBuf::from(ARTIFACTS_DIR_NAME).join(format!("{artifact_id}.txt"))
}

fn session_artifact_relative_path_with_extension(
    artifact_id: &str,
    extension: &str,
) -> io::Result<PathBuf> {
    let artifact_id = sanitize_id_component(artifact_id);
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    if artifact_id.is_empty()
        || extension.is_empty()
        || !extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact id and extension must contain safe ASCII characters",
        ));
    }
    Ok(PathBuf::from(ARTIFACTS_DIR_NAME).join(format!("{artifact_id}.{extension}")))
}

/// The root every session artifact is written under. Readers that serve
/// artifacts by a recorded reference resolve through this same function, so
/// a configured Runtime `sessions_dir` can never point a read at a different
/// tree than the writer used.
pub fn artifact_sessions_root() -> Option<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = TEST_ARTIFACT_SESSIONS_ROOT
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
    {
        return Some(root);
    }
    // Artifacts live beside saved sessions, so an unsealed test gets the same
    // private directory `default_sessions_dir` hands it.
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = crate::test_support::unsealed_state_dir("sessions") {
        return Some(root);
    }

    // Use the same state-root authority as saved sessions, including an explicit
    // CODEWHALE_HOME and legacy read fallback.
    codewhale_config::resolve_state_dir("sessions").ok()
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_test_artifact_sessions_root(root: Option<PathBuf>) -> Option<PathBuf> {
    let mut guard = TEST_ARTIFACT_SESSIONS_ROOT
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    std::mem::replace(&mut *guard, root)
}

/// The recorded spelling of a session artifact path: for receipts, metadata
/// and display, not for opening. It is built from the state root as spelled
/// (a user may have relocated that root behind a link), so an opener must go
/// through `open_session_relative` instead of this value; readers that do
/// consume it compare canonicalized forms.
#[must_use]
pub fn session_artifact_absolute_path(session_id: &str, relative_path: &Path) -> Option<PathBuf> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(
        artifact_sessions_root()?
            .join(session_id)
            .join(relative_path),
    )
}

pub fn write_session_artifact(
    session_id: &str,
    artifact_id: &str,
    content: &str,
) -> io::Result<(PathBuf, PathBuf)> {
    let relative_path = session_artifact_relative_path(artifact_id);
    let absolute_path =
        session_artifact_absolute_path(session_id, &relative_path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve session artifact path (missing home directory)",
            )
        })?;
    open_session_relative(session_id, &relative_path, true)?.replace(content.as_bytes())?;
    Ok((absolute_path, relative_path))
}

/// Publish immutable session-owned bytes without replacing an earlier handle.
/// A duplicate replay with identical bytes is idempotent; a different payload
/// for the same relative path fails closed.
pub fn write_session_relative_immutable(
    session_id: &str,
    relative_path: &Path,
    content: &[u8],
) -> io::Result<PathBuf> {
    let absolute_path =
        session_artifact_absolute_path(session_id, relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid session artifact path")
        })?;
    let destination = open_session_relative(session_id, relative_path, true)?;
    match destination.publish(content) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            use std::io::Read;
            let mut existing = Vec::new();
            destination
                .open_file()?
                .take(content.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            if existing != content {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "immutable artifact handle already contains different bytes",
                ));
            }
        }
        Err(err) => return Err(err),
    }
    Ok(absolute_path)
}

/// The same confined session directory for mutable sidecars and immutable
/// artifacts. The saved-session owner remains responsible for session state.
pub fn open_session_relative(
    session_id: &str,
    relative_path: &Path,
    create: bool,
) -> io::Result<codewhale_util::files::WorkspaceFile> {
    session_artifact_absolute_path(session_id, relative_path).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "invalid session artifact path")
    })?;
    let root = artifact_sessions_root()
        .ok_or_else(|| io::Error::other("session artifact root unavailable"))?;
    if create {
        std::fs::create_dir_all(&root)?;
    }
    // The sessions root is the application's own state, which a user may have
    // relocated behind a link (on Windows, a junction to another volume).
    // Resolve the root once; everything below it stays link-refusing.
    codewhale_util::files::WorkspaceFile::open_resolved_root(
        &root,
        &PathBuf::from(session_id).join(relative_path),
        create,
    )
}

pub fn write_session_artifact_immutable(
    session_id: &str,
    artifact_id: &str,
    content: &[u8],
) -> io::Result<(PathBuf, PathBuf)> {
    let relative_path = session_artifact_relative_path(artifact_id);
    let absolute_path = write_session_relative_immutable(session_id, &relative_path, content)?;
    Ok((absolute_path, relative_path))
}

pub fn write_session_artifact_immutable_from_reader(
    session_id: &str,
    artifact_id: &str,
    reader: &mut (impl io::Read + io::Seek),
) -> io::Result<(PathBuf, PathBuf, u64, String)> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, SeekFrom};

    struct HashingReader<'a, R> {
        reader: &'a mut R,
        hash: Sha256,
        byte_size: u64,
    }
    impl<R: Read> Read for HashingReader<'_, R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.reader.read(buffer)?;
            self.hash.update(&buffer[..count]);
            self.byte_size += count as u64;
            Ok(count)
        }
    }

    let relative_path = session_artifact_relative_path(artifact_id);
    let absolute_path =
        session_artifact_absolute_path(session_id, &relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid session artifact path")
        })?;
    let destination = open_session_relative(session_id, &relative_path, true)?;
    let start = reader.stream_position()?;
    let mut hashing = HashingReader {
        reader,
        hash: Sha256::new(),
        byte_size: 0,
    };
    let result = destination.publish_from_reader(&mut hashing);
    let byte_size = hashing.byte_size;
    let digest = crate::hashing::hex_bytes(hashing.hash.finalize());
    match result {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            reader.seek(SeekFrom::Start(start))?;
            let mut existing = destination.open_file()?;
            let mut source_bytes = [0; 8192];
            let mut existing_bytes = [0; 8192];
            loop {
                let count = match reader.read(&mut source_bytes) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                let same_length = match existing.read_exact(&mut existing_bytes[..count]) {
                    Ok(()) => true,
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => false,
                    Err(error) => return Err(error),
                };
                if !same_length
                    || existing_bytes[..count] != source_bytes[..count]
                    || (count == 0 && existing.read(&mut existing_bytes[..1])? != 0)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "immutable artifact handle already contains different bytes",
                    ));
                }
                if count == 0 {
                    break;
                }
            }
        }
        Err(error) => return Err(error),
    }
    Ok((absolute_path, relative_path, byte_size, digest))
}

/// Write arbitrary fetched bytes into a session artifact with a validated
/// extension. Media fetches use this after magic-byte validation.
pub fn write_session_artifact_bytes(
    session_id: &str,
    artifact_id: &str,
    extension: &str,
    content: &[u8],
) -> io::Result<(PathBuf, PathBuf)> {
    let relative_path = session_artifact_relative_path_with_extension(artifact_id, extension)?;
    let absolute_path =
        session_artifact_absolute_path(session_id, &relative_path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve session artifact path (missing home directory)",
            )
        })?;
    open_session_relative(session_id, &relative_path, true)?.replace(content)?;
    Ok((absolute_path, relative_path))
}

fn preview_text(content: &str, max_chars: usize) -> String {
    let mut preview: String = content.chars().take(max_chars).collect();
    if content.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

pub fn record_tool_output_artifact(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    storage_path: impl Into<PathBuf>,
    content: &str,
) -> ArtifactRecord {
    let storage_path = storage_path.into();
    let byte_size = std::fs::metadata(&storage_path)
        .map(|metadata| metadata.len())
        .unwrap_or_else(|_| content.len() as u64);
    record_tool_output_artifact_with_size(
        session_id,
        tool_call_id,
        tool_name,
        storage_path,
        byte_size,
        &preview_text(content, 200),
    )
}

pub fn record_tool_output_artifact_with_size(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    storage_path: impl Into<PathBuf>,
    byte_size: u64,
    preview: &str,
) -> ArtifactRecord {
    ArtifactRecord {
        id: artifact_id_for_tool_call(tool_call_id),
        kind: ArtifactKind::ToolOutput,
        session_id: session_id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        created_at: Utc::now(),
        byte_size,
        preview: preview_text(preview, 200),
        storage_path: storage_path.into(),
    }
}

#[must_use]
pub fn format_artifact_relative_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

pub use codewhale_protocol::display::format_byte_size;

#[cfg(test)]
mod tests {
    use super::*;

    struct TestArtifactSessionsRoot {
        prior: Option<PathBuf>,
    }

    impl Drop for TestArtifactSessionsRoot {
        fn drop(&mut self) {
            set_test_artifact_sessions_root(self.prior.take());
        }
    }

    fn set_test_sessions_root(root: PathBuf) -> TestArtifactSessionsRoot {
        TestArtifactSessionsRoot {
            prior: set_test_artifact_sessions_root(Some(root)),
        }
    }

    #[test]
    fn session_artifact_absolute_path_uses_test_sessions_root() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));

        let path = session_artifact_absolute_path(
            "session-123",
            &PathBuf::from("artifacts").join("art_call-big.txt"),
        )
        .expect("path");

        assert_eq!(
            path,
            tmp.path()
                .join("sessions")
                .join("session-123")
                .join("artifacts")
                .join("art_call-big.txt")
        );
    }

    #[test]
    fn binary_session_artifact_uses_validated_extension_and_exact_bytes() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));
        let bytes = b"\x89PNG\r\n\x1a\nfixture";

        let (absolute, relative) =
            write_session_artifact_bytes("session-123", "web/media", ".PNG", bytes)
                .expect("write binary artifact");

        assert_eq!(relative, PathBuf::from("artifacts/web_media.png"));
        assert_eq!(std::fs::read(absolute).unwrap(), bytes);
        assert!(write_session_artifact_bytes("session-123", "bad", "../png", bytes).is_err());
    }

    #[test]
    fn adaptive_evidence_publication_is_immutable_and_replay_safe() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));
        let relative = PathBuf::from("artifacts/art_call.txt");
        let bytes = b"first exact payload";

        let first = write_session_relative_immutable("session-a", &relative, bytes).unwrap();
        let replay = write_session_relative_immutable("session-a", &relative, bytes).unwrap();
        assert_eq!(first, replay);
        let err = write_session_relative_immutable("session-a", &relative, b"different")
            .expect_err("handle aliasing must fail");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(first).unwrap(), bytes);
    }

    #[test]
    fn streamed_artifact_is_bounded_exact_and_replay_safe() {
        use std::io::{Read, Seek, SeekFrom, Write};

        struct ChunkedFile(std::fs::File);
        impl Read for ChunkedFile {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                assert!(buffer.len() <= 64 * 1024);
                let limit = buffer.len().min(997);
                self.0.read(&mut buffer[..limit])
            }
        }
        impl Seek for ChunkedFile {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.0.seek(position)
            }
        }

        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));
        let mut fixture = tempfile::tempfile().unwrap();
        let chunk = [0xab; 8192];
        for _ in 0..1024 {
            fixture.write_all(&chunk).unwrap();
        }
        fixture.rewind().unwrap();
        let mut source = ChunkedFile(fixture);
        let first = write_session_artifact_immutable_from_reader(
            "session-stream",
            "art_large",
            &mut source,
        )
        .unwrap();
        assert_eq!(first.2, 8 * 1024 * 1024);
        let mut expected = sha2::Sha256::default();
        use sha2::Digest;
        for _ in 0..1024 {
            expected.update(chunk);
        }
        assert_eq!(first.3, crate::hashing::hex_bytes(expected.finalize()));
        source.rewind().unwrap();
        let replay = write_session_artifact_immutable_from_reader(
            "session-stream",
            "art_large",
            &mut source,
        )
        .unwrap();
        assert_eq!(first, replay);
        source.0.rewind().unwrap();
        source.0.write_all(b"changed").unwrap();
        source.rewind().unwrap();
        assert_eq!(
            write_session_artifact_immutable_from_reader(
                "session-stream",
                "art_large",
                &mut source
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut stored = std::fs::File::open(first.0).unwrap();
        let mut actual = [0; 8192];
        for _ in 0..1024 {
            stored.read_exact(&mut actual).unwrap();
            assert_eq!(actual, chunk);
        }
        assert_eq!(stored.read(&mut actual).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn immutable_session_artifact_rejects_symlinked_parent() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let _root = set_test_sessions_root(sessions.clone());
        std::fs::create_dir_all(&sessions).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), sessions.join("session-a")).unwrap();
        assert!(
            write_session_relative_immutable(
                "session-a",
                Path::new("artifacts/handoff.json"),
                b"private"
            )
            .is_err()
        );
        assert!(
            write_session_artifact_immutable_from_reader(
                "session-a",
                "art_stream",
                &mut io::Cursor::new(b"private")
            )
            .is_err()
        );
        assert!(!outside.path().join("artifacts").exists());
    }

    #[test]
    fn adaptive_evidence_failed_publication_creates_no_handle() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let _root = set_test_sessions_root(sessions.clone());
        std::fs::create_dir_all(sessions.join("session-a")).unwrap();
        std::fs::write(sessions.join("session-a/artifacts"), b"block directory").unwrap();
        let relative = PathBuf::from("artifacts/art_failed.txt");

        assert!(write_session_relative_immutable("session-a", &relative, b"payload").is_err());
        assert!(!sessions.join("session-a/artifacts/art_failed.txt").exists());
    }
    #[test]
    fn streamed_artifact_read_failure_publishes_nothing() {
        use std::io::{Read, Seek, SeekFrom};

        struct FailingReader(io::Cursor<Vec<u8>>);
        impl Read for FailingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.0.position() >= 4096 {
                    return Err(io::Error::other("fixture read failure"));
                }
                let limit = buffer.len().min(4096);
                self.0.read(&mut buffer[..limit])
            }
        }
        impl Seek for FailingReader {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.0.seek(position)
            }
        }

        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let _root = set_test_sessions_root(sessions.clone());
        let mut source = FailingReader(io::Cursor::new(vec![0xcd; 8192]));
        let error =
            write_session_artifact_immutable_from_reader("session-a", "art_failed", &mut source)
                .unwrap_err();
        assert_eq!(error.to_string(), "fixture read failure");
        assert_eq!(
            std::fs::read_dir(sessions.join("session-a/artifacts"))
                .unwrap()
                .count(),
            0
        );
    }

    #[cfg(unix)]
    fn check_mutable_parent_links(binary: bool) {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        for linked_session in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let sessions = temp.path().join("sessions");
            let _root = set_test_sessions_root(sessions.clone());
            std::fs::create_dir_all(&sessions).unwrap();
            let outside = temp.path().join("outside");
            let outside_artifacts = if linked_session {
                outside.join("artifacts")
            } else {
                outside.clone()
            };
            std::fs::create_dir_all(&outside_artifacts).unwrap();
            let extension = if binary { "bin" } else { "txt" };
            let canary = outside_artifacts.join(format!("mutable.{extension}"));
            std::fs::write(&canary, b"old-exact-artifact-canary").unwrap();
            if linked_session {
                std::os::unix::fs::symlink(&outside, sessions.join("session-a")).unwrap();
            } else {
                std::fs::create_dir(sessions.join("session-a")).unwrap();
                std::os::unix::fs::symlink(&outside, sessions.join("session-a/artifacts")).unwrap();
            }
            let result = if binary {
                write_session_artifact_bytes("session-a", "mutable", extension, b"new-byte-payload")
            } else {
                write_session_artifact("session-a", "mutable", "new-text-payload")
            };
            assert_eq!(
                std::fs::read(&canary).unwrap(),
                b"old-exact-artifact-canary",
                "mutable artifact overwrote a linked outside destination"
            );
            assert!(result.is_err(), "linked mutable artifact parent accepted");
        }
    }

    #[cfg(unix)]
    #[test]
    fn mutable_text_artifact_refuses_linked_session_and_artifacts_parents() {
        check_mutable_parent_links(false);
    }

    #[cfg(unix)]
    #[test]
    fn mutable_byte_artifact_refuses_linked_session_and_artifacts_parents() {
        check_mutable_parent_links(true);
    }

    #[cfg(unix)]
    #[test]
    fn mutable_artifacts_replace_exact_bytes_without_following_leaf_links() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let _root = set_test_sessions_root(sessions.clone());
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, b"outside-leaf-canary").unwrap();
        for binary in [false, true] {
            let extension = if binary { "bin" } else { "txt" };
            let relative = PathBuf::from(format!("artifacts/mutable.{extension}"));
            let directory = sessions.join("session-a/artifacts");
            std::fs::create_dir_all(&directory).unwrap();
            let path = sessions.join("session-a").join(&relative);
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            let (absolute, recorded) = if binary {
                write_session_artifact_bytes(
                    "session-a",
                    "mutable",
                    extension,
                    b"\0exact-byte-payload",
                )
            } else {
                write_session_artifact("session-a", "mutable", "exact-text-payload")
            }
            .unwrap();
            assert_eq!(absolute, path);
            assert_eq!(recorded, relative);
            let expected: &[u8] = if binary {
                b"\0exact-byte-payload"
            } else {
                b"exact-text-payload"
            };
            assert_eq!(std::fs::read(&absolute).unwrap(), expected);
            assert!(
                !std::fs::symlink_metadata(&absolute)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::metadata(&absolute).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(std::fs::read(&outside).unwrap(), b"outside-leaf-canary");
        }
    }
}
