//! Session-owned pet sidecar, using the existing confined artifact I/O. A
//! writer lock plus content revision rejects concurrent or external edits.
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[cfg(test)]
use codewhale_runtime::artifacts::open_session_relative;
use codewhale_runtime::artifacts::write_session_relative_immutable;
use codewhale_util::files::{WorkspaceFile, same_file};

const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;
#[cfg(test)]
const HABITAT: &str = "artifacts/pet/habitat.json";

pub struct Store {
    data: WorkspaceFile,
    lock: WorkspaceFile,
    original_lock: File,
    expected: Option<[u8; 32]>,
}

impl Store {
    /// Shared presentation-owner state. Legacy session habitats stay in place.
    pub fn at(root: &Path) -> io::Result<Self> {
        let data = WorkspaceFile::open_resolved_root(root, Path::new("habitat.json"), true)?;
        let lock = WorkspaceFile::open_resolved_root(root, Path::new("habitat.lock"), true)?;
        let original_lock = lock.open_update(true, false)?;
        Ok(Self {
            data,
            lock,
            original_lock,
            expected: None,
        })
    }
    #[cfg(test)]
    pub fn open(session: &str) -> io::Result<Self> {
        let data = open_session_relative(session, Path::new(HABITAT), true)?;
        let lock = open_session_relative(session, Path::new("artifacts/pet/habitat.lock"), true)?;
        let original_lock = lock.open_update(true, false)?;
        Ok(Self {
            data,
            lock,
            original_lock,
            expected: None,
        })
    }

    fn with_lock<T>(&self, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let file = self.lock.open_update(false, false)?;
        if !same_file(&file, &self.original_lock)? {
            return Err(io::Error::other("Pet habitat lock was replaced"));
        }
        let mut lock = fd_lock::RwLock::new(file);
        // Never wait behind another process in the world worker.
        let _guard = lock.try_write()?;
        action()
    }

    fn read(&self) -> io::Result<Option<Vec<u8>>> {
        let file = match self.data.open_file() {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(io::Error::other("Pet habitat exceeds 8 MiB"));
        }
        Ok(Some(bytes))
    }

    pub fn load(&mut self) -> io::Result<Option<String>> {
        let bytes = self.with_lock(|| self.read())?;
        let text = bytes
            .as_ref()
            .map(|b| {
                String::from_utf8(b.clone())
                    .map_err(|_| io::Error::other("Pet habitat is not UTF-8"))
            })
            .transpose()?;
        self.expected = bytes.map(|b| Sha256::digest(b).into());
        Ok(text)
    }

    #[cfg(test)]
    pub fn save(&mut self, text: &str) -> io::Result<()> {
        self.save_archived(text, None)
    }

    pub fn save_archived(&mut self, text: &str, archive: Option<(&[u8], u64)>) -> io::Result<()> {
        if text.len() > MAX_BYTES {
            return Err(io::Error::other("Pet habitat exceeds 8 MiB"));
        }
        self.with_lock(|| {
            let current = self.read()?.map(|b| <[u8; 32]>::from(Sha256::digest(b)));
            if current != self.expected {
                return Err(io::Error::other("Another writer changed the pet habitat"));
            }
            if let Some((bytes, tick)) = archive {
                if bytes.len() > MAX_EXPORT_BYTES {
                    return Err(io::Error::other("Pet recording exceeds 64 MiB"));
                }
                let hash: String = Sha256::digest(bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                let archived = self
                    .data
                    .sibling(&format!("segment-{tick:012}-{hash}.json"))?;
                if let Err(error) = archived.publish(bytes) {
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                    let mut existing = Vec::new();
                    archived
                        .open_file()?
                        .take(MAX_EXPORT_BYTES as u64 + 1)
                        .read_to_end(&mut existing)?;
                    if existing != bytes {
                        return Err(io::Error::other("An archived recording was changed"));
                    }
                }
            }
            self.data.replace(text.as_bytes())
        })?;
        self.expected = Some(Sha256::digest(text.as_bytes()).into());
        Ok(())
    }
}

pub fn export(session: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(io::Error::other("Pet recording exceeds 64 MiB"));
    }
    let relative = PathBuf::from(format!(
        "artifacts/pet/replay-{}.json",
        uuid::Uuid::new_v4()
    ));
    write_session_relative_immutable(session, &relative, bytes)
}

/// One command owns the world until export completes. Keep the large buffer in
/// the host, outside QuickJS's 64 MiB heap, and retain the exact checkpoint.
pub(super) fn export_recording(
    ctx: &rquickjs::Ctx<'_>,
    completed: bool,
) -> rquickjs::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut index = 0usize;
    loop {
        let chunk: Option<String> = ctx.eval(format!("pet.recordingChunk({index},{completed})"))?;
        let Some(chunk) = chunk else { return Ok(bytes) };
        if bytes.len() + chunk.len() > MAX_EXPORT_BYTES {
            return Err(rquickjs::Error::Unknown);
        }
        bytes.extend_from_slice(chunk.as_bytes());
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &Path) -> Store {
        let data = WorkspaceFile::open(root, Path::new("habitat.json"), true).unwrap();
        let lock = WorkspaceFile::open(root, Path::new("habitat.lock"), true).unwrap();
        let original_lock = lock.open_update(true, false).unwrap();
        Store {
            data,
            lock,
            original_lock,
            expected: None,
        }
    }

    #[test]
    fn stale_writer_cannot_replace_a_newer_recording_or_external_edit() {
        let root = tempfile::tempdir().unwrap();
        let mut first = store(root.path());
        let mut second = store(root.path());
        assert!(first.load().unwrap().is_none());
        assert!(second.load().unwrap().is_none());
        first.save("first recording").unwrap();
        assert!(second.save("stale recording").is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("habitat.json")).unwrap(),
            "first recording"
        );
        std::fs::write(root.path().join("habitat.json"), "external edit").unwrap();
        assert!(first.save("lost edit").is_err());
        assert_eq!(first.load().unwrap().as_deref(), Some("external edit"));
    }

    #[test]
    fn invalid_and_oversized_habitats_remain_intact() {
        let root = tempfile::tempdir().unwrap();
        let mut files = store(root.path());
        let path = root.path().join("habitat.json");
        std::fs::write(&path, [0xff]).unwrap();
        assert!(files.load().is_err());
        assert!(files.save("replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff]);
        std::fs::write(&path, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(files.load().is_err());
        assert!(files.save("replacement").is_err());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            MAX_BYTES as u64 + 1
        );
    }

    #[test]
    fn segments_are_immutable_and_must_publish_before_the_habitat_advances() {
        let root = tempfile::tempdir().unwrap();
        let mut files = store(root.path());
        files.load().unwrap();
        files.save("before").unwrap();
        files
            .save_archived("after", Some((b"complete history", 123)))
            .unwrap();
        let name = "segment-000000000123-42fcd454bac01f468e693701bf88157cd7a540556f85d26be0009392e43ecbd4.json";
        let archive = root.path().join(name);
        assert_eq!(std::fs::read(&archive).unwrap(), b"complete history");
        std::fs::write(&archive, "damaged archive").unwrap();
        assert!(
            files
                .save_archived("lost", Some((b"complete history", 123)))
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("habitat.json")).unwrap(),
            "after"
        );
        assert_eq!(
            std::fs::read_to_string(&archive).unwrap(),
            "damaged archive"
        );
        std::fs::write(root.path().join("habitat.json"), "another writer").unwrap();
        assert!(
            files
                .save_archived("lost", Some((b"new history", 124)))
                .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn private_atomic_files_reject_links_and_replaced_locks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let mut files = store(root.path());
        files.save("private").unwrap();
        let path = root.path().join("habitat.json");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let outside = root.path().join("outside");
        std::fs::rename(&path, &outside).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(files.load().is_err());
        assert!(files.save("replacement").is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "private");
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&outside, &path).unwrap();
        assert!(files.load().is_err());
        std::fs::remove_file(root.path().join("habitat.lock")).unwrap();
        let _new_owner = store(root.path());
        assert!(files.save("split lock").is_err());
    }
}
