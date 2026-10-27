//! Fleet artifact I/O. Both verifier publication and HTTP evidence reads use
//! the same workspace-relative, opened-directory authority. This replaces the
//! ordinary path-based writes in task_spec and reads in runtime_api.

#[cfg(test)]
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use codewhale_util::files::WorkspaceFile;
pub(crate) use codewhale_util::files::path_is_confined;

use anyhow::{Context, Result, ensure};
use codewhale_protocol::fleet::FleetArtifactRef;
use sha2::{Digest, Sha256};

// The HTTP preview stays small; verifying a larger artifact streams its digest
// without retaining all bytes. The writer shares the ceiling so it cannot
// publish an artifact the evidence reader is unable to verify.
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) fn write(workspace: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() as u64 <= MAX_ARTIFACT_BYTES,
        "Fleet artifact exceeds the 16 MiB limit"
    );
    let parent = WorkspaceFile::open(workspace, relative, true)?;
    match parent.publish(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = parent.open_file()?;
            let mut saved = Vec::new();
            existing
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut saved)?;
            ensure!(
                saved == bytes,
                "An existing Fleet artifact contains different bytes"
            );
            Ok(())
        }
        Err(error) => Err(error).context("Publishing Fleet artifact"),
    }
}

pub(crate) fn read_verified(
    workspace: &Path,
    artifact: &FleetArtifactRef,
    preview_limit: u64,
) -> Result<(Vec<u8>, u64)> {
    let parent = WorkspaceFile::open(workspace, &artifact.path, false)?;
    let file = parent.open_file()?;
    let size = file.metadata()?.len();
    ensure!(
        size <= MAX_ARTIFACT_BYTES,
        "Fleet artifact exceeds the 16 MiB verification limit"
    );
    ensure!(
        artifact.size_bytes.is_none_or(|expected| expected == size),
        "Fleet artifact size changed"
    );
    let checksum = artifact
        .checksum
        .as_deref()
        .context("Fleet artifact has no recorded checksum")?;
    let mut hasher = Sha256::new();
    let mut preview = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut total = 0_u64;
    // The digest and returned preview consume exactly the same bytes from the
    // same opened file. A changed/replaced pathname is never reopened for data.
    let mut reader = (&file).take(size + 1);
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(total <= size, "Fleet artifact grew while being read");
        hasher.update(&buffer[..count]);
        let remaining = preview_limit.saturating_sub(preview.len() as u64) as usize;
        preview.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    ensure!(
        total == size && file.metadata()?.len() == size,
        "Fleet artifact size changed while being read"
    );
    ensure!(
        format!(
            "sha256:{}",
            codewhale_runtime::hashing::hex_bytes(hasher.finalize())
        ) == checksum,
        "Fleet artifact checksum does not match the recorded receipt"
    );
    Ok((preview, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_protocol::fleet::FleetArtifactKind;

    fn reference(path: &str, bytes: &[u8]) -> FleetArtifactRef {
        FleetArtifactRef {
            kind: FleetArtifactKind::Receipt,
            path: path.into(),
            checksum: Some(format!(
                "sha256:{}",
                codewhale_runtime::hashing::sha256_hex(bytes)
            )),
            mime_type: None,
            size_bytes: Some(bytes.len() as u64),
        }
    }

    #[test]
    fn publication_is_immutable_and_verifies_beyond_the_preview() {
        let workspace = tempfile::tempdir().unwrap();
        let bytes = vec![b'a'; 128 * 1024];
        let artifact = reference(".codewhale/fleet/receipt.json", &bytes);
        write(workspace.path(), &artifact.path, &bytes).unwrap();
        write(workspace.path(), &artifact.path, &bytes).unwrap();
        assert!(write(workspace.path(), &artifact.path, b"replacement").is_err());
        let (preview, size) = read_verified(workspace.path(), &artifact, 65_536).unwrap();
        assert_eq!(preview, bytes[..65_536]);
        assert_eq!(size, bytes.len() as u64);

        // Changing bytes outside the returned preview must still fail the
        // complete digest check, even when size/metadata remain unchanged.
        let mut changed = bytes;
        *changed.last_mut().unwrap() = b'b';
        std::fs::write(workspace.path().join(&artifact.path), changed).unwrap();
        let error = read_verified(workspace.path(), &artifact, 65_536).unwrap_err();
        assert!(error.to_string().contains("checksum"));
    }

    #[test]
    fn missing_digest_size_mismatch_and_oversized_files_fail_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let mut artifact = reference("receipt.json", b"receipt");
        write(workspace.path(), &artifact.path, b"receipt").unwrap();
        artifact.checksum = None;
        assert!(read_verified(workspace.path(), &artifact, 64).is_err());
        artifact = reference("receipt.json", b"receipt");
        artifact.size_bytes = Some(999);
        assert!(read_verified(workspace.path(), &artifact, 64).is_err());
        File::options()
            .write(true)
            .open(workspace.path().join(&artifact.path))
            .unwrap()
            .set_len(MAX_ARTIFACT_BYTES + 1)
            .unwrap();
        assert!(
            read_verified(workspace.path(), &artifact, 64)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        assert!(
            write(
                workspace.path(),
                Path::new("huge.json"),
                &vec![0; MAX_ARTIFACT_BYTES as usize + 1]
            )
            .is_err()
        );
        assert!(!workspace.path().join("huge.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn parent_and_final_symlinks_and_hard_links_never_escape() {
        use std::os::unix::fs::symlink;
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = b"OUTSIDE_SYNTHETIC_RECEIPT_CANARY";
        let outside_file = outside.path().join("private.txt");
        std::fs::write(&outside_file, secret).unwrap();
        std::fs::create_dir_all(workspace.path().join(".codewhale/fleet")).unwrap();

        let final_link = reference(".codewhale/fleet/final.json", secret);
        symlink(&outside_file, workspace.path().join(&final_link.path)).unwrap();
        assert!(read_verified(workspace.path(), &final_link, 64).is_err());
        assert!(write(workspace.path(), &final_link.path, b"overwrite").is_err());

        symlink(
            outside.path(),
            workspace.path().join(".codewhale/fleet/parent"),
        )
        .unwrap();
        let parent_link = reference(".codewhale/fleet/parent/private.txt", secret);
        assert!(read_verified(workspace.path(), &parent_link, 64).is_err());
        assert!(write(workspace.path(), &parent_link.path, b"overwrite").is_err());
        assert!(
            write(
                workspace.path(),
                Path::new(".codewhale/fleet/parent/new.json"),
                b"new"
            )
            .is_err()
        );
        assert!(!outside.path().join("new.json").exists());

        let hard_link = reference(".codewhale/fleet/hard.json", secret);
        std::fs::hard_link(&outside_file, workspace.path().join(&hard_link.path)).unwrap();
        assert!(read_verified(workspace.path(), &hard_link, 64).is_err());
        assert!(write(workspace.path(), &hard_link.path, b"overwrite").is_err());
        assert_eq!(std::fs::read(outside_file).unwrap(), secret);
    }

    #[cfg(unix)]
    #[test]
    fn publication_uses_the_open_parent_after_a_path_swap() {
        use std::os::unix::fs::symlink;
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent =
            WorkspaceFile::open(workspace.path(), Path::new("receipts/item.json"), true).unwrap();
        std::fs::rename(
            workspace.path().join("receipts"),
            workspace.path().join("pinned"),
        )
        .unwrap();
        symlink(outside.path(), workspace.path().join("receipts")).unwrap();
        parent.publish(b"complete receipt").unwrap();
        assert_eq!(
            std::fs::read(workspace.path().join("pinned/item.json")).unwrap(),
            b"complete receipt"
        );
        assert!(!outside.path().join("item.json").exists());
        assert!(write(workspace.path(), Path::new("receipts/next.json"), b"next").is_err());
    }
}
