//! Shared regular-file I/O, replacing project_context's private no-follow open.
//! Parent containment uses the existing Fleet `WorkspaceFile` primitive.
//!
//! The caller chooses the root; links below it are refused. The root itself
//! may resolve through a user-selected link. Writes are not transactions:
//! callers needing atomic replacement must use `WorkspaceFile::replace`.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use crate::files::WorkspaceFile;

fn target(root: &Path, path: &Path, create: bool) -> io::Result<WorkspaceFile> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "File must stay within its root",
        )
    })?;
    WorkspaceFile::open(root, relative, create)
}

pub fn open_read(root: &Path, path: &Path) -> io::Result<File> {
    open_regular(root, path, false)
}

/// [`open_read`] for a file another process may hold open for writing, such as
/// a running worker's log. Windows denies those writers to [`open_read`] (it
/// is the protected reader), so this takes the sharing reader; the links and
/// regular-file policy is the same.
pub fn open_read_shared(root: &Path, path: &Path) -> io::Result<File> {
    open_regular(root, path, true)
}

fn open_regular(root: &Path, path: &Path, shared: bool) -> io::Result<File> {
    let target = target(root, path, false)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Refusing symlinked file",
        ));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path is not a regular file",
        ));
    }
    // The pinned parent and no-follow open enforce the same policy even if
    // the path changes after the metadata check. The handle is checked too.
    if shared {
        target.open_file_shared()
    } else {
        target.open_file()
    }
}

/// [`open_read`] for a workspace file that may be a link to another file in the
/// same root, such as a shared `AGENTS.md`. Every link is resolved first and
/// the final target must stay below the resolved root; the resolved path is
/// then opened with the same no-follow policy, so a component swapped for a
/// link after resolution is refused.
pub fn open_read_linked_within(root: &Path, path: &Path) -> io::Result<File> {
    let root = fs::canonicalize(root)?;
    let resolved = fs::canonicalize(path)?;
    if !resolved.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Refusing symlinked file that resolves outside the workspace",
        ));
    }
    open_regular(&root, &resolved, false)
}

pub fn read_to_string(root: &Path, path: &Path) -> io::Result<String> {
    let mut contents = String::new();
    open_read(root, path)?.read_to_string(&mut contents)?;
    Ok(contents)
}

/// Operator-owned global instructions may intentionally share a linked file.
/// This is not a workspace reader: it resolves links without a containment root.
pub fn open_user_read(path: &Path) -> io::Result<File> {
    let resolved = fs::canonicalize(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(resolved)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || crate::path_identity::metadata_is_link_or_reparse(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path is not a regular file",
        ));
    }
    Ok(file)
}

pub fn open_append(root: &Path, path: &Path) -> io::Result<File> {
    target(root, path, true)?.open_write(true)
}

pub fn write(root: &Path, path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = target(root, path, true)?.open_write(false)?;
    // Validate the opened file before truncating it.
    file.set_len(0)?;
    file.write_all(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confined_files_support_read_append_and_truncate() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes/file.md");
        open_append(root.path(), &path)
            .unwrap()
            .write_all(b"first")
            .unwrap();
        open_append(root.path(), &path)
            .unwrap()
            .write_all(b" second")
            .unwrap();
        assert_eq!(read_to_string(root.path(), &path).unwrap(), "first second");
        write(root.path(), &path, b"next").unwrap();
        assert_eq!(read_to_string(root.path(), &path).unwrap(), "next");
    }

    /// A running worker holds its log open for writing while the host reads
    /// it (Windows denied that to the protected reader).
    #[test]
    fn a_shared_read_sees_a_file_a_writer_still_holds_open() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("logs/worker.log");
        let mut writer = open_append(root.path(), &path).unwrap();
        writer.write_all(b"started").unwrap();
        writer.flush().unwrap();
        let mut text = String::new();
        open_read_shared(root.path(), &path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "started");
        writer.write_all(b" and running").unwrap();
        drop(writer);
        assert_eq!(
            read_to_string(root.path(), &path).unwrap(),
            "started and running"
        );
        assert!(open_read_shared(root.path(), root.path()).is_err());
        assert!(open_read_shared(root.path(), &root.path().join("../worker.log")).is_err());
    }

    #[test]
    fn confined_files_refuse_paths_outside_the_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        for path in [
            outside.path().join("file.md"),
            root.path().join("../file.md"),
        ] {
            assert!(open_read(root.path(), &path).is_err());
            assert!(open_append(root.path(), &path).is_err());
            assert!(write(root.path(), &path, b"next").is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn confined_files_validate_handles_before_truncation() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let original = outside.path().join("original.md");
        fs::write(&original, "original").unwrap();
        let path = root.path().join("linked.md");
        fs::hard_link(&original, &path).unwrap();
        assert!(write(root.path(), &path, b"next").is_err());
        assert_eq!(fs::read_to_string(original).unwrap(), "original");
        assert!(open_read(root.path(), root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn confined_files_refuse_links_even_within_the_root() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("real");
        fs::create_dir(&directory).unwrap();
        let original = directory.join("file.md");
        fs::write(&original, "original").unwrap();
        symlink(&directory, root.path().join("linked-dir")).unwrap();
        symlink(&original, root.path().join("linked.md")).unwrap();
        for path in [
            root.path().join("linked-dir/file.md"),
            root.path().join("linked.md"),
        ] {
            assert!(open_read(root.path(), &path).is_err());
            assert!(open_read_shared(root.path(), &path).is_err());
            assert!(open_append(root.path(), &path).is_err());
            assert!(write(root.path(), &path, b"next").is_err());
            assert_eq!(fs::read_to_string(&original).unwrap(), "original");
        }
    }

    #[cfg(unix)]
    #[test]
    fn linked_reads_follow_links_that_stay_within_the_root() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let shared = root.path().join("ops/operator/AGENTS.md");
        fs::create_dir_all(shared.parent().unwrap()).unwrap();
        fs::write(&shared, "shared").unwrap();
        fs::write(outside.path().join("AGENTS.md"), "outside").unwrap();
        symlink("ops/operator/AGENTS.md", root.path().join("AGENTS.md")).unwrap();
        symlink(&shared, root.path().join("absolute.md")).unwrap();
        symlink("AGENTS.md", root.path().join("chain.md")).unwrap();
        symlink(root.path().join("ops"), root.path().join("linked-dir")).unwrap();
        for name in [
            "AGENTS.md",
            "absolute.md",
            "chain.md",
            "linked-dir/operator/AGENTS.md",
        ] {
            let mut text = String::new();
            open_read_linked_within(root.path(), &root.path().join(name))
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            assert_eq!(text, "shared", "{name}");
        }
        let outside_agents = outside.path().join("AGENTS.md");
        let relative_escape = Path::new("..")
            .join(outside.path().file_name().unwrap())
            .join("AGENTS.md");
        symlink(&outside_agents, root.path().join("escape.md")).unwrap();
        symlink(&relative_escape, root.path().join("dotdot.md")).unwrap();
        symlink(&outside_agents, root.path().join("ops/out.md")).unwrap();
        symlink("ops/out.md", root.path().join("chain-out.md")).unwrap();
        symlink(outside.path(), root.path().join("outside-dir")).unwrap();
        symlink(root.path().join("ops/operator"), root.path().join("dir.md")).unwrap();
        for name in [
            "escape.md",
            "dotdot.md",
            "chain-out.md",
            "outside-dir/AGENTS.md",
            "dir.md",
        ] {
            let error = open_read_linked_within(root.path(), &root.path().join(name)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name}: {error}");
        }
        assert!(
            open_read_linked_within(root.path(), &root.path().join("escape.md"))
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
    }

    #[cfg(windows)]
    #[test]
    fn linked_reads_follow_junctions_within_the_root_and_refuse_escapes() {
        fn junction(link: &Path, target: &Path) {
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("ops")).unwrap();
        fs::write(root.path().join("ops/AGENTS.md"), "shared").unwrap();
        fs::write(outside.path().join("AGENTS.md"), "outside").unwrap();
        junction(&root.path().join("inside"), &root.path().join("ops"));
        junction(&root.path().join("escape"), outside.path());
        let mut text = String::new();
        open_read_linked_within(root.path(), &root.path().join("inside/AGENTS.md"))
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "shared");
        assert!(
            open_read_linked_within(root.path(), &root.path().join("escape/AGENTS.md")).is_err()
        );
        assert!(open_read(root.path(), &root.path().join("inside/AGENTS.md")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn confined_writes_do_not_require_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.md");
        fs::write(&path, "first").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
        let mut file = open_append(root.path(), &path).unwrap();
        assert!(file.read(&mut [0]).is_err());
        file.write_all(b" second").unwrap();
        drop(file);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first second");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
        write(root.path(), &path, b"next").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "next");
    }
}
