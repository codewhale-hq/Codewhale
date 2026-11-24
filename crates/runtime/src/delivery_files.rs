//! Updater-facing I/O over the existing Fleet file fence. No installation store.
//!
//! Retained handles identify ownership; a digest alone cannot identify a file.
//! Moves never replace another entry. Unix cannot atomically compare a directory
//! entry and rename it, so callers must recover/preserve the moved entry when
//! post-move identity validation refuses it. Windows moves the opened object.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use codewhale_util::files::{WorkspaceFile, same_file};

#[derive(Debug, PartialEq, Eq)]
struct Version {
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(unix)]
    nlink: u64,
}

fn version_with_max_links(file: &File, max_links: u64) -> io::Result<Version> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("delivery file is not regular"));
    }
    #[cfg(unix)]
    let nlink = {
        use std::os::unix::fs::MetadataExt;
        let nlink = metadata.nlink();
        if nlink == 0 || nlink > max_links {
            return Err(invalid("delivery file has multiple links"));
        }
        nlink
    };
    #[cfg(windows)]
    {
        let _ = max_links;
        let identity = codewhale_util::path_identity::windows_file_identity(file)?;
        if identity.links != 1 || identity.attributes & 0x0000_0400 != 0 {
            return Err(invalid("delivery file is linked or a reparse point"));
        }
    }
    #[cfg(all(not(unix), not(windows)))]
    return Err(io::ErrorKind::Unsupported.into());
    Ok(Version {
        length: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        changed: {
            use std::os::unix::fs::MetadataExt;
            (metadata.ctime(), metadata.ctime_nsec())
        },
        #[cfg(unix)]
        nlink,
    })
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// A regular file may still grow on Unix after its version was captured. Read
// at most the captured length plus one byte; do not wait for a racing writer's EOF.
fn hash_exact_length(reader: &mut impl Read, length: u64) -> io::Result<String> {
    let limit = length
        .checked_add(1)
        .ok_or_else(|| invalid("delivery file length cannot be bounded"))?;
    let mut bounded = reader.take(limit);
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut total = 0;
    loop {
        let count = bounded.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > length {
            return Err(invalid("delivery file grew while reading"));
        }
        hash.update(&buffer[..count]);
    }
    if total != length {
        return Err(invalid("delivery file shortened while reading"));
    }
    Ok(crate::hashing::hex_bytes(hash.finalize()))
}

fn stable_hash(file: &mut File, maximum: Option<u64>) -> io::Result<(String, Version)> {
    stable_hash_with_max_links(file, maximum, 1)
}

fn stable_hash_with_max_links(
    file: &mut File,
    maximum: Option<u64>,
    max_links: u64,
) -> io::Result<(String, Version)> {
    let before = version_with_max_links(file, max_links)?;
    if maximum.is_some_and(|maximum| before.length > maximum) {
        return Err(invalid("delivery file exceeds captured size limit"));
    }
    file.rewind()?;
    let digest = hash_exact_length(file, before.length)?;
    let after = version_with_max_links(file, max_links)?;
    if before != after {
        return Err(invalid("delivery file changed while reading"));
    }
    Ok((digest, after))
}

/// Blocking I/O facade used by the synchronous CLI updater. Async callers
/// must run these methods on their existing blocking worker.
/// One opened regular, unlinked file and its pinned parent. Drop closes handles;
/// it never deletes a pathname another process could have replaced.
#[derive(Debug)]
pub struct GuardedFile {
    location: WorkspaceFile,
    directory: PathBuf,
    name: String,
    file: File,
    digest: String,
    version: Version,
}

impl GuardedFile {
    pub fn open(directory: &Path, name: &str) -> io::Result<Option<Self>> {
        Self::open_with_limit(directory, name, None)
    }

    pub fn open_bounded(directory: &Path, name: &str, maximum: usize) -> io::Result<Option<Self>> {
        Self::open_with_limit(directory, name, Some(maximum))
    }

    /// Open a closed set of command siblings, rejecting any hard link outside the opened group.
    pub fn open_command_siblings(
        directory: &Path,
        names: &[&str],
    ) -> io::Result<BTreeMap<String, Self>> {
        let mut unique_names = BTreeSet::new();
        for &name in names {
            if Path::new(name).components().count() != 1 {
                return Err(invalid("delivery filename must be one basename"));
            }
            unique_names.insert(name);
        }
        #[cfg(unix)]
        let max_links = unique_names.len().max(1) as u64;
        #[cfg(not(unix))]
        let max_links = 1;
        let mut opened = BTreeMap::new();
        for name in unique_names {
            if let Some(handle) =
                Self::open_with_limit_and_max_links(directory, name, None, max_links)?
            {
                opened.insert(name.to_string(), handle);
            }
        }
        #[cfg(unix)]
        {
            for handle in opened.values() {
                let mut group_len = 0_u64;
                for candidate in opened.values() {
                    if same_file(&handle.file, &candidate.file)? {
                        group_len += 1;
                    }
                }
                if handle.version.nlink != group_len {
                    return Err(invalid("delivery file has an unowned hard link"));
                }
            }
            for handle in opened.values_mut() {
                handle.verify()?;
            }
        }
        Ok(opened)
    }

    fn open_with_limit(
        directory: &Path,
        name: &str,
        maximum: Option<usize>,
    ) -> io::Result<Option<Self>> {
        Self::open_with_limit_and_max_links(directory, name, maximum, 1)
    }

    fn open_with_limit_and_max_links(
        directory: &Path,
        name: &str,
        maximum: Option<usize>,
        max_links: u64,
    ) -> io::Result<Option<Self>> {
        if Path::new(name).components().count() != 1 {
            return Err(invalid("delivery filename must be one basename"));
        }
        #[cfg(windows)]
        let directory = if directory.is_absolute() {
            directory.to_path_buf()
        } else {
            std::env::current_dir()?.join(directory)
        };
        #[cfg(not(windows))]
        let directory = directory.canonicalize()?;
        let location = WorkspaceFile::open_delivery(&directory, Path::new(name))?;
        let mut file = match location.open_retained_with_max_links(max_links) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let initial = version_with_max_links(&file, max_links)?;
        if maximum.is_some_and(|maximum| initial.length > maximum as u64) {
            return Err(invalid("delivery receipt exceeds size limit"));
        }
        let (digest, version) = stable_hash_with_max_links(
            &mut file,
            maximum.map(|maximum| maximum as u64),
            max_links,
        )?;
        let result = Self {
            location,
            directory,
            name: name.into(),
            file,
            digest,
            version,
        };
        if !result.matches_path()? {
            return Err(invalid("delivery pathname changed while opening"));
        }
        Ok(Some(result))
    }

    pub fn stage(directory: &Path, bytes: &[u8], executable: bool) -> io::Result<Self> {
        let name = Self::recovery_name();
        #[cfg(windows)]
        let directory = if directory.is_absolute() {
            directory.to_path_buf()
        } else {
            std::env::current_dir()?.join(directory)
        };
        #[cfg(not(windows))]
        let directory = directory.canonicalize()?;
        let location = WorkspaceFile::open_delivery(&directory, Path::new(&name))?;
        let mut file = location.publish_retained(bytes, executable)?;
        let (digest, version) = stable_hash(&mut file, Some(bytes.len() as u64))?;
        let result = Self {
            location,
            directory,
            name,
            file,
            digest,
            version,
        };
        if !result.matches_path()? {
            return Err(invalid("delivery stage changed while opening"));
        }
        Ok(result)
    }

    fn expected_max_links(&self) -> u64 {
        #[cfg(unix)]
        {
            self.version.nlink
        }
        #[cfg(not(unix))]
        {
            1
        }
    }

    /// Return whether two guarded handles refer to the same underlying file object.
    pub fn same_inode(&self, other: &Self) -> io::Result<bool> {
        same_file(&self.file, &other.file)
    }

    #[cfg(unix)]
    pub fn verify_owned(&mut self) -> io::Result<()> {
        let max_links = self.expected_max_links();
        self.location
            .verify_delivery_owner_with_max_links(&self.file, max_links)?;
        self.verify()?;
        self.location
            .verify_delivery_owner_with_max_links(&self.file, max_links)
    }

    #[cfg(unix)]
    pub fn remove_checked(&mut self, unlinked: &mut bool) -> io::Result<()> {
        self.remove_sibling_checked(0, false, unlinked)
    }

    /// Remove a guarded sibling command while enforcing exact pre- and post-unlink link counts.
    #[cfg(unix)]
    pub fn remove_sibling_checked(
        &mut self,
        expected_remaining_links: u64,
        prior_sibling_unlinked: bool,
        unlinked: &mut bool,
    ) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        *unlinked = false;
        if prior_sibling_unlinked {
            let expected_current_links = expected_remaining_links
                .checked_add(1)
                .ok_or_else(|| invalid("invalid expected link count"))?;
            if !self.matches_path_with_max_links(expected_current_links)? {
                return Err(invalid("delivery pathname identity changed"));
            }
            self.location
                .verify_delivery_owner_with_max_links(&self.file, expected_current_links)?;
            let metadata = self.file.metadata()?;
            if !metadata.is_file()
                || metadata.len() != self.version.length
                || metadata.modified()? != self.version.modified
                || metadata.nlink() != expected_current_links
            {
                return Err(invalid("delivery file changed since capture"));
            }
            self.file.rewind()?;
            let digest = hash_exact_length(&mut self.file, self.version.length)?;
            if digest != self.digest {
                return Err(invalid("delivery file changed since capture"));
            }
            let reanchored = version_with_max_links(&self.file, expected_current_links)?;
            if reanchored.length != self.version.length
                || reanchored.modified != self.version.modified
                || reanchored.nlink != expected_current_links
            {
                return Err(invalid("delivery file changed since capture"));
            }
            self.version = reanchored;
        } else if self.version.nlink != expected_remaining_links.saturating_add(1) {
            return Err(invalid("delivery file link count changed since capture"));
        }
        self.verify_owned()?;
        let expected_links_before_unlink = self.version.nlink;
        let directory = tempfile::Builder::new()
            .prefix(".codewhale-uninstall-")
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir_in(&self.directory)?
            .keep();
        let destination = WorkspaceFile::open_delivery(&directory, Path::new(&self.name))?;
        self.location.move_opened_to_directory_with_max_links(
            &self.file,
            &destination,
            expected_links_before_unlink,
        )?;
        let previous_location = std::mem::replace(&mut self.location, destination);
        self.directory = directory.clone();
        let removed = (|| {
            previous_location.sync_parent()?;
            self.location.sync_parent()?;
            if !self.matches_path()? {
                return Err(invalid("retired uninstall entry has a different identity"));
            }
            let (digest, version) = stable_hash_with_max_links(
                &mut self.file,
                Some(self.version.length),
                expected_links_before_unlink,
            )?;
            if digest != self.digest
                || version.length != self.version.length
                || version.modified != self.version.modified
                || version.nlink != expected_links_before_unlink
            {
                return Err(invalid("delivery file changed during uninstall retirement"));
            }
            self.version = version;
            self.verify_owned()?;
            self.location
                .unlink_opened_with_max_links(&self.file, expected_links_before_unlink)?;
            *unlinked = true;
            if self.file.metadata()?.nlink() != expected_remaining_links {
                return Err(invalid("delivery file retained another link after unlink"));
            }
            self.location.sync_parent()
        })();
        removed.map_err(|error| {
            let detail = if *unlinked {
                format!("file entry was unlinked, but its final identity or durability check failed: {error}; recovery directory {}", directory.display())
            } else {
                format!("{error}; inspect retained recovery location {}", directory.join(&self.name).display())
            };
            io::Error::new(error.kind(), detail)
        })?;
        if let Err(error) = std::fs::remove_dir(&directory) {
            eprintln!(
                "Removed owned file; empty recovery directory retained at {}: {error}",
                directory.display()
            );
        }
        Ok(())
    }

    #[cfg(windows)]
    pub fn verify_owned(&mut self) -> io::Result<()> {
        self.verify()
    }

    #[cfg(windows)]
    pub fn remove_checked(&mut self, unlinked: &mut bool) -> io::Result<()> {
        self.remove_sibling_checked(0, false, unlinked)
    }

    /// Windows binaries stay single-link; reject hard-linked sibling removal parameters.
    #[cfg(windows)]
    pub fn remove_sibling_checked(
        &mut self,
        expected_remaining_links: u64,
        prior_sibling_unlinked: bool,
        unlinked: &mut bool,
    ) -> io::Result<()> {
        if expected_remaining_links != 0 || prior_sibling_unlinked {
            return Err(invalid(
                "hard-linked sibling removal is unsupported on Windows",
            ));
        }
        *unlinked = false;
        self.verify()?;
        self.move_to_vacant(&Self::recovery_name())?;
        *unlinked = true;
        codewhale_config::private_directory::mark_windows_file_handle_for_deletion(&self.file)
            .map_err(|error| {
                io::Error::other(format!(
                    "{error:#}; retired entry retained at {}",
                    self.directory.join(&self.name).display()
                ))
            })
    }

    pub fn recovery_name() -> String {
        format!(".codewhale-host-recovery-{}", uuid::Uuid::new_v4())
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn sha256(&self) -> &str {
        &self.digest
    }

    fn matches_path(&self) -> io::Result<bool> {
        self.matches_path_with_max_links(self.expected_max_links())
    }

    fn matches_path_with_max_links(&self, max_links: u64) -> io::Result<bool> {
        if !same_file(
            &self.file,
            &self.location.identity_probe_with_max_links(max_links)?,
        )? {
            return Ok(false);
        }
        // An anchored Unix fd survives a renamed parent. Rewalk the original
        // captured absolute root without canonicalizing new links, so a new
        // directory at that pathname cannot inherit the installation receipt.
        let current = WorkspaceFile::open_delivery(&self.directory, Path::new(&self.name))?;
        same_file(
            &self.file,
            &current.identity_probe_with_max_links(max_links)?,
        )
    }

    pub fn verify(&mut self) -> io::Result<()> {
        if !self.matches_path()? {
            return Err(invalid("delivery pathname identity changed"));
        }
        let max_links = self.expected_max_links();
        let (digest, version) =
            stable_hash_with_max_links(&mut self.file, Some(self.version.length), max_links)?;
        if digest != self.digest || version != self.version {
            return Err(invalid("delivery file changed since capture"));
        }
        if !self.matches_path()? {
            return Err(invalid("delivery pathname changed while verifying"));
        }
        Ok(())
    }

    /// Reads and validates the same handle used for its digest, without reopening.
    pub fn read_bounded(&mut self, maximum: usize) -> io::Result<Vec<u8>> {
        if self.version.length > maximum as u64 {
            return Err(invalid("delivery receipt exceeds size limit"));
        }
        self.verify()?;
        self.file.rewind()?;
        let mut bytes = Vec::new();
        (&mut self.file)
            .take(self.version.length.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > maximum || crate::hashing::sha256_hex(&bytes) != self.digest {
            return Err(invalid("delivery receipt changed while reading"));
        }
        self.verify()?;
        Ok(bytes)
    }

    /// Moves to a vacant sibling. On a post-move refusal `name()` still names
    /// the recovery entry; callers must retain/report it and recover without
    /// replacing any concurrent destination.
    pub fn move_to_vacant(&mut self, name: &str) -> io::Result<()> {
        self.verify()?;
        let destination = self.location.sibling(name)?;
        self.location
            .move_opened_to_sibling(&self.file, &destination)?;
        self.location = destination;
        self.name = name.into();
        // Journal the completed move before any fallible durability/validation.
        self.location.sync_parent()?;
        if !self.matches_path()? {
            return Err(invalid("retired delivery entry has a different identity"));
        }
        let (digest, version) = stable_hash(&mut self.file, Some(self.version.length))?;
        if digest != self.digest
            || version.length != self.version.length
            || version.modified != self.version.modified
        {
            return Err(invalid("delivery file changed during retirement"));
        }
        // A successful rename may update ctime; it does not change file bytes.
        self.version = version;
        self.verify()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_budget_refuses_a_growing_reader_without_waiting_for_eof() {
        struct AppendingReader {
            read_bytes: u64,
        }
        impl Read for AppendingReader {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                bytes.fill(b'x');
                self.read_bytes += bytes.len() as u64;
                Ok(bytes.len())
            }
        }
        for length in [0, 5, 64 * 1024] {
            let mut reader = AppendingReader { read_bytes: 0 };
            assert!(hash_exact_length(&mut reader, length).is_err());
            assert_eq!(reader.read_bytes, length + 1);
        }
        let mut shortened = &b"short"[..];
        assert!(hash_exact_length(&mut shortened, 6).is_err());
        let mut exact = &b"owned"[..];
        assert_eq!(
            hash_exact_length(&mut exact, 5).unwrap(),
            crate::hashing::sha256_hex(b"owned")
        );
    }

    #[cfg(unix)]
    #[test]
    fn grown_receipt_is_refused_by_captured_and_requested_limits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("receipt");
        std::fs::write(&path, b"owned").unwrap();
        let mut captured = GuardedFile::open_bounded(root.path(), "receipt", 5)
            .unwrap()
            .unwrap();
        std::fs::write(&path, vec![b'x'; 64 * 1024 + 1]).unwrap();
        assert!(captured.verify().is_err());
        assert!(captured.read_bounded(64 * 1024).is_err());
        assert!(GuardedFile::open_bounded(root.path(), "receipt", 64 * 1024).is_err());
        let mut opened = File::open(&path).unwrap();
        assert!(stable_hash(&mut opened, Some(64 * 1024)).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024 + 1);
    }

    #[test]
    fn bounded_receipt_and_no_clobber_move_keep_canonical_ownership() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("receipt"), b"owned").unwrap();
        assert!(GuardedFile::open_bounded(root.path(), "receipt", 4).is_err());
        let mut file = GuardedFile::open_bounded(root.path(), "receipt", 5)
            .unwrap()
            .unwrap();
        assert_eq!(file.read_bounded(5).unwrap(), b"owned");
        std::fs::write(root.path().join("taken"), b"foreign").unwrap();
        assert!(file.move_to_vacant("taken").is_err());
        assert_eq!(file.name(), "receipt");
        assert_eq!(
            std::fs::read(root.path().join("taken")).unwrap(),
            b"foreign"
        );
        file.move_to_vacant("retired").unwrap();
        assert_eq!(file.read_bounded(5).unwrap(), b"owned");
        assert!(!root.path().join("receipt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn nofollow_reader_refuses_symlinks_and_multiple_links() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("untouched");
        std::fs::write(&source, b"foreign").unwrap();
        symlink(&source, root.path().join("linked")).unwrap();
        std::fs::hard_link(&source, root.path().join("hardlinked")).unwrap();
        for name in ["linked", "hardlinked"] {
            assert!(GuardedFile::open(root.path(), name).is_err());
            assert!(GuardedFile::open_bounded(root.path(), name, 64 * 1024).is_err());
        }
        assert_eq!(std::fs::read(source).unwrap(), b"foreign");
    }

    #[cfg(unix)]
    #[test]
    fn captured_receipt_cannot_be_reopened_through_a_swapped_symlink() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("receipt"), b"owned").unwrap();
        std::fs::write(outside.path().join("foreign"), b"owned").unwrap();
        let mut file = GuardedFile::open_bounded(root.path(), "receipt", 64 * 1024)
            .unwrap()
            .unwrap();
        std::fs::rename(root.path().join("receipt"), root.path().join("original")).unwrap();
        symlink(outside.path().join("foreign"), root.path().join("receipt")).unwrap();
        assert!(file.read_bounded(64 * 1024).is_err());
        assert_eq!(
            std::fs::read(root.path().join("original")).unwrap(),
            b"owned"
        );
        assert_eq!(
            std::fs::read(outside.path().join("foreign")).unwrap(),
            b"owned"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaced_parent_cannot_inherit_a_retained_payload_receipt() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("prefix");
        std::fs::create_dir(&prefix).unwrap();
        std::fs::write(prefix.join("host"), b"old").unwrap();
        let mut file = GuardedFile::open(&prefix, "host").unwrap().unwrap();
        std::fs::rename(&prefix, root.path().join("original-prefix")).unwrap();
        std::fs::create_dir(&prefix).unwrap();
        std::fs::write(prefix.join("host"), b"old").unwrap();
        assert!(file.verify().is_err());
        assert!(file.move_to_vacant("backup").is_err());
        assert_eq!(std::fs::read(prefix.join("host")).unwrap(), b"old");
        assert_eq!(
            std::fs::read(root.path().join("original-prefix/host")).unwrap(),
            b"old"
        );
    }

    #[cfg(windows)]
    #[test]
    fn retained_windows_reader_denies_writer_and_parent_replacement() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("prefix");
        std::fs::create_dir(&prefix).unwrap();
        std::fs::write(prefix.join("host"), b"old").unwrap();
        let mut file = GuardedFile::open(&prefix, "host").unwrap().unwrap();
        assert!(std::fs::write(prefix.join("host"), b"changed").is_err());
        assert!(std::fs::rename(&prefix, root.path().join("swapped")).is_err());
        file.move_to_vacant("backup").unwrap();
        assert_eq!(file.read_bounded(3).unwrap(), b"old");
    }

    #[cfg(unix)]
    #[test]
    fn command_siblings_allow_owned_pair_and_reject_external_hard_links() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let primary = root.path().join("codewhale");
        let alias = root.path().join("codew");
        std::fs::write(&primary, b"release-bytes").unwrap();
        std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::hard_link(&primary, &alias).unwrap();

        assert!(GuardedFile::open(root.path(), "codewhale").is_err());
        assert!(GuardedFile::open_bounded(root.path(), "codewhale", 64).is_err());
        assert!(GuardedFile::open_command_siblings(root.path(), &["codewhale"]).is_err());

        let mut siblings =
            GuardedFile::open_command_siblings(root.path(), &["codewhale", "codew"]).unwrap();
        assert_eq!(siblings.len(), 2);
        assert!(
            siblings["codewhale"]
                .same_inode(&siblings["codew"])
                .unwrap()
        );

        let third = outside.path().join("external");
        std::fs::hard_link(&primary, &third).unwrap();
        assert!(GuardedFile::open_command_siblings(root.path(), &["codewhale", "codew"]).is_err());
        assert!(siblings.get_mut("codew").unwrap().verify().is_err());
        let mut unlinked = false;
        assert!(
            siblings
                .get_mut("codew")
                .unwrap()
                .remove_sibling_checked(1, false, &mut unlinked)
                .is_err()
        );
        assert!(!unlinked);
        assert!(primary.exists());
        assert!(alias.exists());

        std::fs::remove_file(&third).unwrap();
        let mut siblings =
            GuardedFile::open_command_siblings(root.path(), &["codewhale", "codew"]).unwrap();
        let mut unlinked_alias = false;
        siblings
            .get_mut("codew")
            .unwrap()
            .remove_sibling_checked(1, false, &mut unlinked_alias)
            .unwrap();
        assert!(unlinked_alias);
        assert!(!alias.exists());
        assert!(primary.exists());

        let mut unlinked_primary = false;
        siblings
            .get_mut("codewhale")
            .unwrap()
            .remove_sibling_checked(0, true, &mut unlinked_primary)
            .unwrap();
        assert!(unlinked_primary);
        assert!(!primary.exists());
    }

    #[cfg(unix)]
    #[test]
    fn reanchored_sibling_refuses_newly_added_hard_link_before_final_unlink() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let primary = root.path().join("codewhale");
        let alias = root.path().join("codew");
        std::fs::write(&primary, b"release-bytes").unwrap();
        std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::hard_link(&primary, &alias).unwrap();

        let mut siblings =
            GuardedFile::open_command_siblings(root.path(), &["codewhale", "codew"]).unwrap();
        let mut unlinked_alias = false;
        siblings
            .get_mut("codew")
            .unwrap()
            .remove_sibling_checked(1, false, &mut unlinked_alias)
            .unwrap();
        assert!(unlinked_alias);

        let third = outside.path().join("raced-link");
        std::fs::hard_link(&primary, &third).unwrap();
        let mut unlinked_primary = false;
        assert!(
            siblings
                .get_mut("codewhale")
                .unwrap()
                .remove_sibling_checked(0, true, &mut unlinked_primary)
                .is_err()
        );
        assert!(!unlinked_primary);
        assert!(primary.exists());
    }
}
