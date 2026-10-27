//! Read-only views over two snapshots: what changed between them.
//!
//! The engine is the only snapshot writer. These methods only read the side
//! repo (`diff-tree`, `ls-tree`, `cat-file`), so a host can learn what a turn
//! changed from the `pre-turn`/`post-turn` pair the engine already took,
//! without a second snapshot or a second store.

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use super::{SnapshotId, SnapshotRepo};
use codewhale_dependencies::ExternalTool;

/// Blobs above this size are reported by size only: hashing them for a
/// Preview list is not worth the I/O, and the read route refuses them anyway.
pub const DELTA_HASH_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// How one path changed between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaChange {
    Created,
    Updated,
    Deleted,
    Renamed,
}

/// One changed path. `size`/`sha256` describe the content in the *newer*
/// snapshot; both are `None` for a deletion, and `sha256` is `None` above
/// [`DELTA_HASH_MAX_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaEntry {
    /// Workspace-relative, `/`-separated.
    pub path: String,
    pub previous_path: Option<String>,
    pub change: DeltaChange,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

/// Every regular-file change between two snapshots, bounded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotDelta {
    pub entries: Vec<DeltaEntry>,
    /// Entries were cut at the limit.
    pub truncated: bool,
    /// Changes not listed: past the limit, a non-UTF-8 or unsafe path, or
    /// not a regular file (symlink, submodule).
    pub omitted: u64,
}

struct RawChange {
    dst_mode: String,
    dst_blob: String,
    status: u8,
    path: Vec<u8>,
    previous_path: Option<Vec<u8>>,
}

fn git(repo: &SnapshotRepo) -> io::Result<Command> {
    let mut command = codewhale_dependencies::Git::command()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git not found on PATH"))?;
    command
        .arg("--git-dir")
        .arg(repo.git_dir())
        .arg("--work-tree")
        .arg(repo.work_tree());
    Ok(command)
}

fn git_output(repo: &SnapshotRepo, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = git(repo)?.args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// A workspace-relative display path, or `None` when the bytes are not
/// UTF-8, the path is not plainly relative, or it names `.git`.
fn safe_display(path: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(path).ok()?;
    let mut parts = Vec::new();
    for component in Path::new(text).components() {
        match component {
            Component::Normal(name)
                if !codewhale_util::git_metadata::is_git_metadata_name(name) =>
            {
                parts.push(name.to_str()?)
            }
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Parse `diff-tree -r -z --raw -M` output: `:<src mode> <dst mode> <src oid>
/// <dst oid> <status>\0<path>\0` and, for a rename or copy, a second path.
fn parse_raw(bytes: &[u8]) -> io::Result<Vec<RawChange>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "unexpected diff-tree output");
    let mut fields = bytes.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(header) = fields.next() {
        let header = std::str::from_utf8(header).map_err(|_| invalid())?;
        let header = header.strip_prefix(':').ok_or_else(invalid)?;
        let parts: Vec<&str> = header.split(' ').collect();
        let [_, dst_mode, _, dst_blob, status] = parts.as_slice() else {
            return Err(invalid());
        };
        let status = status.bytes().next().ok_or_else(invalid)?;
        let first = fields.next().ok_or_else(invalid)?.to_vec();
        let (path, previous_path) = if matches!(status, b'R' | b'C') {
            let second = fields.next().ok_or_else(invalid)?.to_vec();
            (second, Some(first))
        } else {
            (first, None)
        };
        changes.push(RawChange {
            dst_mode: (*dst_mode).to_string(),
            dst_blob: (*dst_blob).to_string(),
            status,
            path,
            previous_path,
        });
    }
    Ok(changes)
}

impl SnapshotRepo {
    /// Every regular-file change from snapshot `from` to snapshot `to`,
    /// with the size and SHA-256 of each new blob, bounded to `limit`
    /// entries. Renames are detected (`-M`). Reads the side repo only.
    ///
    /// What the snapshots cannot see is not here either: paths excluded by
    /// the built-in excludes or the workspace's `.gitignore` never enter a
    /// snapshot, so a change to them never appears in a delta.
    pub fn diff_snapshots(
        &self,
        from: &SnapshotId,
        to: &SnapshotId,
        limit: usize,
    ) -> io::Result<SnapshotDelta> {
        let raw = git_output(
            self,
            &[
                "diff-tree",
                "-r",
                "-z",
                "-M",
                "--raw",
                "--no-commit-id",
                "--end-of-options",
                from.as_str(),
                to.as_str(),
            ],
        )?;
        let mut delta = SnapshotDelta::default();
        let mut pending_blobs = Vec::new();
        for change in parse_raw(&raw)? {
            let kind = match change.status {
                b'A' | b'C' => DeltaChange::Created,
                b'D' => DeltaChange::Deleted,
                b'M' | b'T' => DeltaChange::Updated,
                b'R' => DeltaChange::Renamed,
                _ => {
                    delta.omitted += 1;
                    continue;
                }
            };
            let regular = matches!(change.dst_mode.as_str(), "100644" | "100755");
            if kind != DeltaChange::Deleted && !regular {
                delta.omitted += 1;
                continue;
            }
            let Some(path) = safe_display(&change.path) else {
                delta.omitted += 1;
                continue;
            };
            let previous_path = match change.previous_path.as_deref() {
                Some(previous) => match safe_display(previous) {
                    Some(previous) => Some(previous),
                    None => {
                        delta.omitted += 1;
                        continue;
                    }
                },
                None => None,
            };
            if delta.entries.len() >= limit {
                delta.truncated = true;
                delta.omitted += 1;
                continue;
            }
            if kind != DeltaChange::Deleted {
                pending_blobs.push((delta.entries.len(), change.dst_blob));
            }
            delta.entries.push(DeltaEntry {
                path,
                previous_path,
                // A copy (`C`) only appears with `-C`; treat it as a creation
                // so its source is never reported as touched.
                change: kind,
                size: None,
                sha256: None,
            });
        }
        for (index, size, sha256) in self.blob_facts(&pending_blobs)? {
            let entry = &mut delta.entries[index];
            entry.size = Some(size);
            entry.sha256 = sha256;
        }
        Ok(delta)
    }

    /// Size and SHA-256 of each blob in one `cat-file --batch` pass. Blobs
    /// above [`DELTA_HASH_MAX_BYTES`] are streamed past without hashing.
    fn blob_facts(
        &self,
        blobs: &[(usize, String)],
    ) -> io::Result<Vec<(usize, u64, Option<String>)>> {
        if blobs.is_empty() {
            return Ok(Vec::new());
        }
        let mut child = git(self)?
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("cat-file stdin unavailable"))?;
        let request: String = blobs.iter().map(|(_, oid)| format!("{oid}\n")).collect();
        // Feed requests from another thread: git blocks on a full stdout
        // pipe, and stops reading stdin until it drains.
        let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("cat-file stdout unavailable"))?;
        let mut reader = BufReader::new(stdout);
        let mut facts = Vec::with_capacity(blobs.len());
        let mut header = String::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        for (index, _) in blobs {
            header.clear();
            reader.read_line(&mut header)?;
            let mut parts = header.split_whitespace();
            let (Some(_), Some("blob"), Some(size)) = (parts.next(), parts.next(), parts.next())
            else {
                let _ = child.kill();
                return Err(io::Error::other(format!(
                    "cat-file returned an unexpected header: {}",
                    header.trim()
                )));
            };
            let size: u64 = size
                .parse()
                .map_err(|_| io::Error::other("cat-file returned an invalid size"))?;
            let mut hasher = (size <= DELTA_HASH_MAX_BYTES).then(Sha256::new);
            let mut remaining = size;
            while remaining > 0 {
                let want = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(0);
                let read = reader.read(&mut buffer[..want])?;
                if read == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "cat-file ended inside a blob",
                    ));
                }
                if let Some(hasher) = hasher.as_mut() {
                    hasher.update(&buffer[..read]);
                }
                remaining -= read as u64;
            }
            let mut newline = [0_u8; 1];
            reader.read_exact(&mut newline)?;
            facts.push((
                *index,
                size,
                hasher.map(|hasher| codewhale_runtime::hashing::hex_bytes(hasher.finalize())),
            ));
        }
        writer
            .join()
            .map_err(|_| io::Error::other("cat-file writer panicked"))??;
        child.wait()?;
        Ok(facts)
    }

    /// The bytes of regular file `rel` in snapshot `id`, or `None` when the
    /// snapshot does not hold a regular file there. A blob larger than
    /// `max_bytes` is refused with `FileTooLarge` before it is read.
    pub fn read_blob(
        &self,
        id: &SnapshotId,
        rel: &str,
        max_bytes: u64,
    ) -> io::Result<Option<Vec<u8>>> {
        if safe_display(rel.as_bytes()).as_deref() != Some(rel) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot path must be a plain workspace-relative path",
            ));
        }
        let entry = git_output(
            self,
            &[
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "--long",
                "--end-of-options",
                id.as_str(),
                "--",
                rel,
            ],
        )?;
        // `<mode> blob <oid> <size>\t<path>\0`
        let Some(line) = entry.split(|byte| *byte == 0).find(|line| !line.is_empty()) else {
            return Ok(None);
        };
        let header = std::str::from_utf8(line)
            .ok()
            .and_then(|line| line.split('\t').next())
            .unwrap_or_default();
        let fields: Vec<&str> = header.split_whitespace().collect();
        let [mode, "blob", oid, size] = fields.as_slice() else {
            return Ok(None);
        };
        if !matches!(*mode, "100644" | "100755") {
            return Ok(None);
        }
        let size: u64 = size
            .parse()
            .map_err(|_| io::Error::other("ls-tree returned an invalid size"))?;
        if size > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "snapshot blob exceeds the read limit",
            ));
        }
        git_output(self, &["cat-file", "blob", oid]).map(Some)
    }

    /// Which of `paths` snapshot `id` contains. A path absent from both
    /// snapshots of a pair is one the snapshots cannot see (excluded or
    /// ignored), which is different from one that did not change.
    pub fn tracked_paths(&self, id: &SnapshotId, paths: &[String]) -> io::Result<HashSet<String>> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let mut args = vec![
            "--literal-pathspecs",
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            "--end-of-options",
            id.as_str(),
            "--",
        ];
        args.extend(paths.iter().map(String::as_str));
        let listed = git_output(self, &args)?;
        Ok(listed
            .split(|byte| *byte == 0)
            .filter_map(safe_display)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::test_support::{EnvVarGuard, TestEnvLock, lock_test_env};
    use std::fs;

    struct Home {
        _vars: Vec<EnvVarGuard>,
        _lock: TestEnvLock,
    }

    fn repo(root: &Path) -> (SnapshotRepo, Home) {
        let lock = lock_test_env();
        let vars = vec![
            EnvVarGuard::set("HOME", root),
            EnvVarGuard::set("USERPROFILE", root),
            EnvVarGuard::set("CODEWHALE_HOME", root.join(".codewhale")),
        ];
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let repo = SnapshotRepo::open_or_init_with_cap(&workspace, 0).expect("snapshot repo");
        (
            repo,
            Home {
                _vars: vars,
                _lock: lock,
            },
        )
    }

    fn sha(bytes: &[u8]) -> Option<String> {
        Some(codewhale_runtime::hashing::sha256_hex(bytes))
    }

    #[test]
    fn delta_reports_created_updated_deleted_and_renamed_with_facts() {
        let root = tempfile::tempdir().unwrap();
        let (repo, _home) = repo(root.path());
        let work = repo.work_tree().to_path_buf();
        fs::write(work.join("keep.txt"), "same\n").unwrap();
        fs::write(work.join("edit.txt"), "before\n").unwrap();
        fs::write(work.join("gone.txt"), "bye\n").unwrap();
        fs::write(
            work.join("move-me.txt"),
            "a long enough body to be detected as a rename\n",
        )
        .unwrap();
        let pre = repo.snapshot("pre-turn:1").unwrap();

        // What a shell command does: plain filesystem writes, no receipts.
        fs::create_dir(work.join("site")).unwrap();
        fs::write(work.join("site/index.html"), "<h1>hi</h1>\n").unwrap();
        fs::write(work.join("edit.txt"), "after\n").unwrap();
        fs::remove_file(work.join("gone.txt")).unwrap();
        fs::rename(work.join("move-me.txt"), work.join("moved.txt")).unwrap();
        let post = repo.snapshot("post-turn:1").unwrap();

        let delta = repo.diff_snapshots(&pre, &post, 100).unwrap();
        assert!(!delta.truncated);
        assert_eq!(delta.omitted, 0);
        let mut entries = delta.entries.clone();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            entries,
            vec![
                DeltaEntry {
                    path: "edit.txt".into(),
                    previous_path: None,
                    change: DeltaChange::Updated,
                    size: Some(6),
                    sha256: sha(b"after\n"),
                },
                DeltaEntry {
                    path: "gone.txt".into(),
                    previous_path: None,
                    change: DeltaChange::Deleted,
                    size: None,
                    sha256: None,
                },
                DeltaEntry {
                    path: "moved.txt".into(),
                    previous_path: Some("move-me.txt".into()),
                    change: DeltaChange::Renamed,
                    size: Some(46),
                    sha256: sha(b"a long enough body to be detected as a rename\n"),
                },
                DeltaEntry {
                    path: "site/index.html".into(),
                    previous_path: None,
                    change: DeltaChange::Created,
                    size: Some(12),
                    sha256: sha(b"<h1>hi</h1>\n"),
                },
            ]
        );

        // The pair is diffable in either direction and bounded.
        let bounded = repo.diff_snapshots(&pre, &post, 2).unwrap();
        assert_eq!(bounded.entries.len(), 2);
        assert!(bounded.truncated);
        assert_eq!(bounded.omitted, 2);

        // Tracked probes distinguish "unchanged" from "invisible".
        let probes = ["keep.txt", "gone.txt", "never.txt"].map(String::from);
        assert_eq!(
            repo.tracked_paths(&pre, &probes).unwrap(),
            HashSet::from(["keep.txt".to_string(), "gone.txt".to_string()])
        );
    }

    #[test]
    fn oversized_blobs_report_size_without_a_revision() {
        let root = tempfile::tempdir().unwrap();
        let (repo, _home) = repo(root.path());
        let work = repo.work_tree().to_path_buf();
        fs::write(work.join("seed.txt"), "seed\n").unwrap();
        let pre = repo.snapshot("pre-turn:1").unwrap();
        let big = vec![b'z'; (DELTA_HASH_MAX_BYTES + 1) as usize];
        fs::write(work.join("big.txt"), &big).unwrap();
        fs::write(work.join("small.txt"), "small\n").unwrap();
        let post = repo.snapshot("post-turn:1").unwrap();
        let delta = repo.diff_snapshots(&pre, &post, 100).unwrap();
        let big_entry = delta.entries.iter().find(|e| e.path == "big.txt").unwrap();
        assert_eq!(big_entry.size, Some(DELTA_HASH_MAX_BYTES + 1));
        assert_eq!(big_entry.sha256, None);
        // A later blob in the same batch is still read correctly.
        let small = delta
            .entries
            .iter()
            .find(|e| e.path == "small.txt")
            .unwrap();
        assert_eq!(small.sha256, sha(b"small\n"));
    }

    #[test]
    fn read_blob_returns_exact_snapshot_bytes_and_refuses_unsafe_paths() {
        let root = tempfile::tempdir().unwrap();
        let (repo, _home) = repo(root.path());
        let work = repo.work_tree().to_path_buf();
        fs::create_dir(work.join("dir")).unwrap();
        fs::write(work.join("dir/a.txt"), b"exact \x00 bytes").unwrap();
        let id = repo.snapshot("post-turn:1").unwrap();
        fs::write(work.join("dir/a.txt"), b"changed later").unwrap();
        assert_eq!(
            repo.read_blob(&id, "dir/a.txt", 1024).unwrap().as_deref(),
            Some(&b"exact \x00 bytes"[..])
        );
        assert_eq!(repo.read_blob(&id, "dir/missing.txt", 1024).unwrap(), None);
        assert_eq!(
            repo.read_blob(&id, "dir", 1024).unwrap(),
            None,
            "a tree is not a file"
        );
        assert_eq!(
            repo.read_blob(&id, "dir/a.txt", 3).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        for unsafe_path in [
            "../escape",
            "/etc/passwd",
            "dir/../dir/a.txt",
            ".git/config",
            "",
        ] {
            assert_eq!(
                repo.read_blob(&id, unsafe_path, 1024).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{unsafe_path:?}"
            );
        }
    }

    #[test]
    fn unsafe_and_non_utf8_paths_are_not_displayed() {
        assert_eq!(safe_display(b"a/b.txt").as_deref(), Some("a/b.txt"));
        assert_eq!(safe_display(b"../x"), None);
        assert_eq!(safe_display(b"/abs"), None);
        assert_eq!(safe_display(b".git/config"), None);
        assert_eq!(safe_display(b"bad\xff.txt"), None);
        assert_eq!(safe_display(b""), None);
    }
}
