//! Full-fidelity session archive export (`tar.xz`).
//!
//! The interactive `/export` command renders a sanitized, lossy Markdown
//! transcript for humans. This module is the machine-facing counterpart: it
//! packs the durable session record into a compressed tar archive so a
//! complete session log — system prompt, every user and assistant message
//! including thinking blocks, tool calls and tool results, the branch
//! journal, approval receipts, and the session's artifacts — can be saved
//! with one command and restored later or attached to a bug report.
//!
//! Archive layout (format version 1):
//!
//! - `session.json` — the full [`SavedSession`] serialization, the same shape
//!   as the on-disk session record. Extract it and open it with `/load` in
//!   the TUI for a full-fidelity restore (system prompt included); note that
//!   `/resume <file>` imports the conversation transcript only.
//! - `container.json` — the portable [`SessionImportContainer`] for
//!   version-tolerant resume across schema changes.
//! - `artifacts/<...>` — the session-owned artifact directory, when present
//!   and not excluded. Only regular files are archived; symlinks are skipped
//!   and opened through the existing confined-file reader. Extracted files
//!   remain separate; `/load` does not install them into the artifact store.
//! - `manifest.json` — archive format version, generator version, export
//!   timestamp, session metadata, and the index of the preceding members.
//!   Written last so its index covers everything above it.
//!
//! Contents are intentionally **not sanitized**: this is a full-fidelity log
//! for the session owner, unlike `/export`, which redacts secrets for
//! sharing. xz compression keeps verbose tool-heavy sessions small enough to
//! archive or attach.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use liblzma::write::XzEncoder;
use serde::Serialize;
use tar::Builder;

use crate::artifacts::{ARTIFACTS_DIR_NAME, is_valid_session_id};
use crate::session_manager::{SavedSession, SessionMetadata};
use codewhale_runtime::session_tree::SessionImportContainer;
use codewhale_util::files::WorkspaceFile;

/// Layout version of the exported archive. Bump when members change.
pub const SESSION_ARCHIVE_FORMAT_VERSION: u32 = 1;

/// Default xz compression preset (0–9), mirroring `xz -6`.
pub const DEFAULT_XZ_COMPRESSION_LEVEL: u32 = 6;

const SESSION_RECORD_MEMBER: &str = "session.json";
const SESSION_CONTAINER_MEMBER: &str = "container.json";
const ARCHIVE_MANIFEST_MEMBER: &str = "manifest.json";
const MAX_ARTIFACT_DEPTH: usize = 64;
const MAX_ARTIFACT_ENTRIES: usize = 100_000;

/// Options for [`write_session_archive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionArchiveOptions {
    /// Include the session-owned `artifacts/` directory when it exists.
    pub include_artifacts: bool,
    /// xz compression preset, 0 (fastest) through 9 (smallest).
    pub compression_level: u32,
    /// Replace the destination directory entry. Otherwise fail if it exists.
    pub overwrite: bool,
}

impl Default for SessionArchiveOptions {
    fn default() -> Self {
        Self {
            include_artifacts: true,
            compression_level: DEFAULT_XZ_COMPRESSION_LEVEL,
            overwrite: false,
        }
    }
}

/// One archive member reported in the manifest and [`SessionArchiveSummary`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionArchiveMember {
    /// Member path inside the archive, `/`-separated.
    pub name: String,
    /// Uncompressed member size in bytes.
    pub bytes: u64,
}

/// Outcome of a successful [`write_session_archive`] call.
#[derive(Debug, Clone, Serialize)]
pub struct SessionArchiveSummary {
    /// Path the `.tar.xz` archive was written to.
    pub output: PathBuf,
    /// Exported session id.
    pub session_id: String,
    /// [`SESSION_ARCHIVE_FORMAT_VERSION`] used for this archive.
    pub archive_format_version: u32,
    /// xz preset the archive was written with.
    pub compression_level: u32,
    /// Whether `artifacts/` members were included.
    pub includes_artifacts: bool,
    /// Member index in write order. `manifest.json` itself is excluded: it
    /// is written last and indexes everything before it.
    pub members: Vec<SessionArchiveMember>,
}

impl SessionArchiveSummary {
    /// Sum of uncompressed member sizes.
    pub fn total_member_bytes(&self) -> u64 {
        self.members.iter().map(|member| member.bytes).sum()
    }

    /// Compressed archive size in bytes, or 0 if the file is unreadable.
    pub fn compressed_bytes(&self) -> u64 {
        fs::metadata(&self.output).map_or(0, |meta| meta.len())
    }
}

#[derive(Serialize)]
struct ArchiveManifest {
    archive_format_version: u32,
    generator: &'static str,
    generator_version: &'static str,
    exported_at: String,
    session: SessionMetadata,
    members: Vec<SessionArchiveMember>,
}

/// Write one session as a full-fidelity `.tar.xz` archive.
///
/// `session` is typically loaded with
/// [`SessionManager::load_session_snapshot`](crate::session_manager::SessionManager::load_session_snapshot)
/// so the archive reflects the durable record without applying resume-time
/// repair. `sessions_dir` is the trusted session store root; pass `None` (or clear
/// [`SessionArchiveOptions::include_artifacts`]) to export the transcript
/// only.
///
/// The archive is streamed to a sibling temporary file and renamed into
/// place, so a failed export never leaves a truncated archive at `output`.
/// An existing `output` is replaced only when `options.overwrite` is set.
pub fn write_session_archive(
    session: &SavedSession,
    sessions_dir: Option<&Path>,
    output: &Path,
    options: SessionArchiveOptions,
) -> io::Result<SessionArchiveSummary> {
    if !is_valid_session_id(&session.metadata.id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session id",
        ));
    }
    if options.compression_level > 9 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "xz compression level {} is out of range 0-9",
                options.compression_level
            ),
        ));
    }
    let session_json = session_json(session)?;
    let container_json = container_json(session, sessions_dir)?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    fs::create_dir_all(&parent)?;
    let sessions_dir = sessions_dir.map(Path::canonicalize).transpose()?;
    if let Some(root) = &sessions_dir
        && parent.canonicalize()?.starts_with(root)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "export output must be outside the session store",
        ));
    }
    let artifact_files = match (options.include_artifacts, sessions_dir.as_deref()) {
        (true, Some(root)) => match session_artifacts_dir(root, &session.metadata.id)? {
            Some(dir) => collect_artifact_files(root, &dir)?,
            None => Vec::new(),
        },
        _ => Vec::new(),
    };
    let mut temp = tempfile::Builder::new()
        .prefix(".codewhale-session-export-")
        .tempfile_in(&parent)?;

    let mut summary = SessionArchiveSummary {
        output: output.to_path_buf(),
        session_id: session.metadata.id.clone(),
        archive_format_version: SESSION_ARCHIVE_FORMAT_VERSION,
        compression_level: options.compression_level,
        includes_artifacts: !artifact_files.is_empty(),
        members: Vec::new(),
    };

    {
        let file = temp.as_file_mut();
        let mut tar = Builder::new(XzEncoder::new(file, options.compression_level));
        append_member(
            &mut tar,
            SESSION_RECORD_MEMBER,
            MemberContents::Bytes(session_json.as_bytes()),
            &mut summary.members,
        )?;
        append_member(
            &mut tar,
            SESSION_CONTAINER_MEMBER,
            MemberContents::Bytes(container_json.as_bytes()),
            &mut summary.members,
        )?;
        for (name, relative) in &artifact_files {
            let root = sessions_dir
                .as_deref()
                .expect("artifact collection needs a store");
            // Parent directories and the leaf are opened without following links.
            // Stream this handle directly; never reopen a validated path.
            let mut file = WorkspaceFile::open_resolved_root(root, relative, false)?.open_file()?;
            append_member(
                &mut tar,
                name,
                MemberContents::File(&mut file),
                &mut summary.members,
            )?;
        }
        let manifest_json = serde_json::to_string_pretty(&ArchiveManifest {
            archive_format_version: SESSION_ARCHIVE_FORMAT_VERSION,
            generator: env!("CARGO_PKG_NAME"),
            generator_version: env!("CARGO_PKG_VERSION"),
            exported_at: Utc::now().to_rfc3339(),
            session: session.metadata.clone(),
            members: summary.members.clone(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        append_member(
            &mut tar,
            ARCHIVE_MANIFEST_MEMBER,
            MemberContents::Bytes(manifest_json.as_bytes()),
            &mut Vec::new(),
        )?;
        let encoder = tar.into_inner()?;
        let file = encoder.finish()?;
        file.flush()?;
        file.sync_all()?;
    }
    if options.overwrite {
        temp.persist(output)
    } else {
        temp.persist_noclobber(output)
    }
    .map_err(|error| {
        if error.error.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already exists; pass --force to overwrite it",
                    output.display()
                ),
            )
        } else {
            error.error
        }
    })?;

    Ok(summary)
}

/// Directory holding this session's artifacts, when it exists. `session_id`
/// is re-checked against path traversal here because this helper is also the
/// boundary for callers that build the path from user-supplied ids.
pub fn session_artifacts_dir(sessions_dir: &Path, session_id: &str) -> io::Result<Option<PathBuf>> {
    if !is_valid_session_id(session_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session id",
        ));
    }
    let relative = Path::new(session_id).join(ARTIFACTS_DIR_NAME);
    // Opening a confined file reference pins and validates its parent directory;
    // this checks the artifact root without creating or opening the dummy leaf.
    match WorkspaceFile::open_resolved_root(
        sessions_dir,
        &relative.join(".export-root-check"),
        false,
    ) {
        Ok(_) => Ok(Some(sessions_dir.join(relative))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn export_recovery_hint(session_id: &str) -> String {
    format!(
        "To move it to a provider you still have, run `codewhale --provider <name> resume {session_id}`. To keep its history without that provider, run `codewhale sessions export {session_id}`."
    )
}

/// Default archive file name for a session:
/// `codewhale-session-<short-id>.tar.xz`.
pub fn default_archive_file_name(metadata: &SessionMetadata) -> String {
    format!(
        "codewhale-session-{}.tar.xz",
        crate::session_manager::truncate_id(&metadata.id)
    )
}

fn session_json(session: &SavedSession) -> io::Result<String> {
    serde_json::to_string_pretty(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The portable journal container. With a session store it also carries the
/// entries bounded saves moved into the journal archive (#6842), so an export
/// holds the full history, not only what the document still keeps.
fn container_json(session: &SavedSession, sessions_dir: Option<&Path>) -> io::Result<String> {
    let mut container: SessionImportContainer = session.export_container("session-archive");
    if let Some(root) = sessions_dir {
        let archived = crate::session_manager::load_journal_archive(root, &session.metadata.id)?;
        if !archived.is_empty() {
            let present: std::collections::HashSet<String> =
                container.entries.iter().map(|e| e.id.clone()).collect();
            let mut entries: Vec<_> = archived
                .into_iter()
                .filter(|e| !present.contains(&e.id))
                .collect();
            entries.append(&mut container.entries);
            container.entries = entries;
        }
    }
    serde_json::to_string_pretty(&container)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Collect regular artifact files as `(archive member name, source path)`,
/// sorted by member name for stable ordering. Symlinks and other
/// non-regular entries are skipped so an export cannot reach outside the
/// session directory through a link.
fn collect_artifact_files(
    sessions_dir: &Path,
    artifacts_dir: &Path,
) -> io::Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    let mut entries = 0;
    collect_artifact_files_recursive(
        sessions_dir,
        artifacts_dir,
        ARTIFACTS_DIR_NAME,
        0,
        &mut entries,
        &mut files,
    )?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn collect_artifact_files_recursive(
    sessions_dir: &Path,
    dir: &Path,
    prefix: &str,
    depth: usize,
    entries: &mut usize,
    files: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    if depth > MAX_ARTIFACT_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact tree exceeds export depth limit",
        ));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        *entries += 1;
        if *entries > MAX_ARTIFACT_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact tree exceeds export entry limit",
            ));
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let child = entry.file_name();
        let child = child
            .to_str()
            .filter(|name| !name.contains(['\\', ':']))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact name is not portable UTF-8",
                )
            })?;
        let member = format!("{prefix}/{child}");
        let path = entry.path();
        let relative = path.strip_prefix(sessions_dir).map_err(io::Error::other)?;
        if file_type.is_dir() {
            // Reject substituted parent directories before descending. The file
            // read repeats confinement checks, so directory races cannot leak bytes.
            WorkspaceFile::open_resolved_root(
                sessions_dir,
                &relative.join(".export-root-check"),
                false,
            )?;
            collect_artifact_files_recursive(
                sessions_dir,
                &path,
                &member,
                depth + 1,
                entries,
                files,
            )?;
        } else if file_type.is_file() {
            files.push((member, relative.to_path_buf()));
        }
    }
    Ok(())
}

fn archive_header(size: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o600);
    header.set_mtime(Utc::now().timestamp().max(0) as u64);
    header.set_cksum();
    header
}

/// Payload for one archive member: in-memory bytes or a filesystem file
/// streamed straight into the tar.
enum MemberContents<'a> {
    Bytes(&'a [u8]),
    File(&'a mut fs::File),
}

/// Append exactly `expected` bytes of `reader` under `name`. The bounded read
/// keeps the tar header and the member payload consistent when the source
/// file changes size mid-export: growth is capped at the snapshot size
/// (valid archive, prefix content), while shrinkage — which would otherwise
/// silently shift every following header and corrupt the archive — fails the
/// export instead.
fn append_sized<W: Write, R: Read>(
    tar: &mut Builder<W>,
    header: &mut tar::Header,
    name: &str,
    reader: R,
    expected: u64,
) -> io::Result<()> {
    let mut limited = reader.take(expected);
    tar.append_data(header, name, &mut limited)?;
    if limited.limit() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "member {name} ended before its recorded size of {expected} bytes; the source changed during export"
            ),
        ));
    }
    Ok(())
}

fn append_member<W: Write>(
    tar: &mut Builder<W>,
    name: &str,
    contents: MemberContents<'_>,
    members: &mut Vec<SessionArchiveMember>,
) -> io::Result<()> {
    match contents {
        MemberContents::Bytes(bytes) => {
            let mut header = archive_header(bytes.len() as u64);
            tar.append_data(&mut header, name, bytes)?;
            members.push(SessionArchiveMember {
                name: name.to_string(),
                bytes: bytes.len() as u64,
            });
        }
        MemberContents::File(file) => {
            let size = file.metadata()?.len();
            let mut header = archive_header(size);
            append_sized(tar, &mut header, name, file, size)?;
            members.push(SessionArchiveMember {
                name: name.to_string(),
                bytes: size,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::create_saved_session;
    use codewhale_models::{ContentBlock, Message, Role};
    use codewhale_runtime::session_tree::SessionJournal;
    use std::io::Read;

    fn fixture_session() -> SavedSession {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "list the files in src".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::thinking("I should run ls first"),
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "toolu_01".to_string(),
                        name: "shell".to_string(),
                        input: serde_json::json!({ "command": "ls src" }),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "toolu_01".to_string(),
                    content: "main.rs\nlib.rs".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let mut session = create_saved_session(
            &messages,
            "test-model",
            Path::new("/tmp/archive-fixture"),
            128,
            None,
        );
        session.system_prompt = Some("You are a careful coding agent.".to_string());
        session.journal = Some(SessionJournal::from_messages(messages, 0));
        session
    }

    fn read_archive_members(path: &Path) -> Vec<(String, Vec<u8>)> {
        let file = fs::File::open(path).expect("archive opens");
        let mut archive = tar::Archive::new(liblzma::read::XzDecoder::new(file));
        archive
            .entries()
            .expect("archive entries")
            .map(|entry| {
                let mut entry = entry.expect("entry");
                let name = entry.path().expect("path").to_string_lossy().into_owned();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("member bytes");
                (name, bytes)
            })
            .collect()
    }

    #[test]
    fn forkguard_session_archive_export_roundtrips_full_context() {
        let session = fixture_session();
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("session.tar.xz");

        let summary =
            write_session_archive(&session, None, &output, SessionArchiveOptions::default())
                .expect("archive export");

        assert_eq!(summary.session_id, session.metadata.id);
        let mut members = read_archive_members(&output);
        members.sort_by(|left, right| left.0.cmp(&right.0));
        let names: Vec<&str> = members.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["container.json", "manifest.json", "session.json"]
        );

        // The raw session record restores the complete context: system
        // prompt, thinking, tool call, and tool result.
        let (_, session_bytes) = members
            .iter()
            .find(|(name, _)| name == SESSION_RECORD_MEMBER)
            .expect("session.json member");
        let restored: SavedSession =
            serde_json::from_slice(session_bytes).expect("session.json parses");
        assert_eq!(
            restored.system_prompt.as_deref(),
            Some("You are a careful coding agent.")
        );
        let mut saw_tool_use = false;
        let mut saw_tool_result = false;
        let mut saw_thinking = false;
        for message in &restored.messages {
            for block in &message.content {
                match block {
                    ContentBlock::ToolUse { id, .. } => {
                        saw_tool_use = true;
                        assert_eq!(id, "toolu_01");
                    }
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        saw_tool_result = true;
                        assert_eq!(tool_use_id, "toolu_01");
                    }
                    ContentBlock::Thinking { .. } => saw_thinking = true,
                    _ => {}
                }
            }
        }
        assert!(saw_tool_use && saw_tool_result && saw_thinking);

        // The portable container feeds the exact import path `/resume` uses
        // for exported session JSON.
        let (_, container_bytes) = members
            .iter()
            .find(|(name, _)| name == SESSION_CONTAINER_MEMBER)
            .expect("container.json member");
        let container = SessionImportContainer::from_json(
            std::str::from_utf8(container_bytes).expect("container utf8"),
        )
        .expect("container parses");
        let imported = SavedSession::import_foreign(
            container,
            PathBuf::from("/tmp/archive-fixture"),
            "test-model".to_string(),
        )
        .expect("container imports");
        assert!(
            imported
                .messages
                .iter()
                .any(|message| message.content.iter().any(
                    |block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "toolu_01")
                )),
            "imported session keeps the tool call"
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &members
                .iter()
                .find(|(name, _)| name == ARCHIVE_MANIFEST_MEMBER)
                .expect("manifest member")
                .1,
        )
        .expect("manifest parses");
        assert_eq!(
            manifest["archive_format_version"],
            SESSION_ARCHIVE_FORMAT_VERSION
        );
        assert_eq!(manifest["session"]["id"], session.metadata.id);
    }

    #[test]
    fn forkguard_session_archive_includes_artifacts_and_respects_skip() {
        let session = fixture_session();
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = dir.path().join("sessions");
        let artifacts_dir = sessions_dir
            .join(&session.metadata.id)
            .join(ARTIFACTS_DIR_NAME);
        fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
        fs::write(artifacts_dir.join("art_call-1.txt"), b"artifact body").expect("artifact");
        assert!(
            session_artifacts_dir(&sessions_dir, &session.metadata.id)
                .unwrap()
                .is_some(),
            "artifacts dir is discovered for a valid session id"
        );
        assert!(
            session_artifacts_dir(&sessions_dir, "../escape").is_err(),
            "path traversal ids never resolve to an artifacts dir"
        );

        let with_artifacts = write_session_archive(
            &session,
            Some(&sessions_dir),
            &dir.path().join("full.tar.xz"),
            SessionArchiveOptions::default(),
        )
        .expect("archive with artifacts");
        assert!(with_artifacts.includes_artifacts);
        let names: Vec<&str> = with_artifacts
            .members
            .iter()
            .map(|member| member.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["session.json", "container.json", "artifacts/art_call-1.txt"]
        );
        let members = read_archive_members(&dir.path().join("full.tar.xz"));
        let (_, artifact_bytes) = members
            .iter()
            .find(|(name, _)| name == "artifacts/art_call-1.txt")
            .expect("artifact member");
        assert_eq!(artifact_bytes, b"artifact body");

        let transcript_only = write_session_archive(
            &session,
            Some(&sessions_dir),
            &dir.path().join("lean.tar.xz"),
            SessionArchiveOptions {
                include_artifacts: false,
                ..SessionArchiveOptions::default()
            },
        )
        .expect("archive without artifacts");
        assert!(!transcript_only.includes_artifacts);
        assert!(
            !transcript_only
                .members
                .iter()
                .any(|member| member.name.starts_with(ARTIFACTS_DIR_NAME))
        );
    }

    #[test]
    fn forkguard_session_archive_rejects_artifact_shorter_than_recorded_size() {
        // A member whose source shrank between stat and copy must fail the
        // export instead of being zero-padded into a silently shifted
        // archive (the growth direction is capped to the snapshot size).
        let mut tar = Builder::new(Vec::new());
        let mut header = archive_header(16);
        let error = append_sized(
            &mut tar,
            &mut header,
            "artifacts/shrinking.bin",
            &b"only-nine"[..],
            16,
        )
        .expect_err("short member must fail the export");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

        let mut tar = Builder::new(Vec::new());
        let mut header = archive_header(9);
        append_sized(
            &mut tar,
            &mut header,
            "artifacts/stable.bin",
            &b"only-nine"[..],
            9,
        )
        .expect("exact-size member succeeds");
    }

    #[test]
    fn session_archive_rejects_out_of_range_compression_level() {
        let session = fixture_session();
        let dir = tempfile::tempdir().expect("tempdir");
        let error = write_session_archive(
            &session,
            None,
            &dir.path().join("out.tar.xz"),
            SessionArchiveOptions {
                compression_level: 10,
                ..SessionArchiveOptions::default()
            },
        )
        .expect_err("level 10 must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn session_archive_replaces_existing_output_atomically() {
        let session = fixture_session();
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("out.tar.xz");
        write_session_archive(&session, None, &output, SessionArchiveOptions::default())
            .expect("first export");
        write_session_archive(
            &session,
            None,
            &output,
            SessionArchiveOptions {
                overwrite: true,
                ..SessionArchiveOptions::default()
            },
        )
        .expect("second export replaces");
        let members = read_archive_members(&output);
        assert_eq!(members.len(), 3);
        assert!(default_archive_file_name(&session.metadata).ends_with(".tar.xz"));
    }

    #[test]
    fn session_archive_preserves_existing_output_without_force() {
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.tar.xz");
        fs::write(&output, b"existing archive").unwrap();
        let error =
            write_session_archive(&session, None, &output, SessionArchiveOptions::default())
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&output).unwrap(), b"existing archive");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temporary file cleaned up"
        );
    }

    #[test]
    fn session_archive_rejects_output_inside_store_even_with_force() {
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join(format!("{}.json", session.metadata.id));
        fs::write(&output, b"durable session").unwrap();
        let error = write_session_archive(
            &session,
            Some(dir.path()),
            &output,
            SessionArchiveOptions {
                overwrite: true,
                ..SessionArchiveOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&output).unwrap(), b"durable session");
    }

    #[test]
    fn session_archive_prefix_snapshot_keeps_unfinished_tool_call_and_journal() {
        use crate::session_manager::SessionManager;
        let mut session = fixture_session();
        session.messages.pop();
        session.journal = Some(SessionJournal::from_messages(session.messages.clone(), 0));
        let dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(dir.path().join("sessions")).unwrap();
        manager.save_session(&session).unwrap();
        let durable_path = manager
            .sessions_dir()
            .join(format!("{}.json", session.metadata.id));
        let before = fs::read(&durable_path).unwrap();
        let id = manager
            .resolve_session_id_prefix(&session.metadata.id[..8])
            .unwrap();
        let snapshot = manager.load_session_snapshot(&id).unwrap();
        let output = dir.path().join("out.tar.xz");
        write_session_archive(
            &snapshot,
            Some(manager.sessions_dir()),
            &output,
            SessionArchiveOptions::default(),
        )
        .unwrap();
        let members = read_archive_members(&output);
        let bytes = &members
            .iter()
            .find(|(name, _)| name == SESSION_RECORD_MEMBER)
            .unwrap()
            .1;
        let restored: SavedSession = serde_json::from_slice(bytes).unwrap();
        assert_eq!(restored.messages, session.messages);
        assert_eq!(
            serde_json::to_value(restored.journal).unwrap(),
            serde_json::to_value(session.journal).unwrap()
        );
        assert_eq!(
            fs::read(durable_path).unwrap(),
            before,
            "export must not rewrite the durable record"
        );
    }

    #[test]
    fn session_archive_hard_link_fails_without_replacing_output() {
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let artifacts = sessions.join(&session.metadata.id).join(ARTIFACTS_DIR_NAME);
        fs::create_dir_all(&artifacts).unwrap();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"outside content").unwrap();
        fs::hard_link(&outside, artifacts.join("linked.txt")).unwrap();
        let output = dir.path().join("out.tar.xz");
        fs::write(&output, b"original").unwrap();
        assert!(
            write_session_archive(
                &session,
                Some(&sessions),
                &output,
                SessionArchiveOptions {
                    overwrite: true,
                    ..SessionArchiveOptions::default()
                }
            )
            .is_err()
        );
        assert_eq!(fs::read(output).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn session_archive_rejects_linked_roots_and_skips_linked_members() {
        use std::os::unix::fs::symlink;
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let artifacts = sessions.join(&session.metadata.id).join(ARTIFACTS_DIR_NAME);
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"outside content").unwrap();
        fs::create_dir_all(artifacts.parent().unwrap()).unwrap();
        symlink(&outside, &artifacts).unwrap();
        let output = dir.path().join("out.tar.xz");
        assert!(
            write_session_archive(
                &session,
                Some(&sessions),
                &output,
                SessionArchiveOptions::default()
            )
            .is_err()
        );
        assert!(!output.exists());
        fs::remove_file(&artifacts).unwrap();
        fs::create_dir(&artifacts).unwrap();
        symlink(&outside, artifacts.join("directory-link")).unwrap();
        symlink(outside.join("secret.txt"), artifacts.join("file-link")).unwrap();
        fs::write(artifacts.join("regular.txt"), b"owned content").unwrap();
        let summary = write_session_archive(
            &session,
            Some(&sessions),
            &output,
            SessionArchiveOptions::default(),
        )
        .unwrap();
        assert_eq!(summary.members.len(), 3);
        assert_eq!(summary.members[2].name, "artifacts/regular.txt");
        assert!(
            !read_archive_members(&output)
                .iter()
                .any(|(_, bytes)| bytes == b"outside content")
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_archive_dangling_output_link_does_not_bypass_no_clobber() {
        use std::os::unix::fs::symlink;
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.tar.xz");
        let target = dir.path().join("missing");
        symlink(&target, &output).unwrap();
        assert!(!output.exists());
        let error =
            write_session_archive(&session, None, &output, SessionArchiveOptions::default())
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_link(&output).unwrap(), target);
        write_session_archive(
            &session,
            None,
            &output,
            SessionArchiveOptions {
                overwrite: true,
                ..SessionArchiveOptions::default()
            },
        )
        .unwrap();
        assert!(!target.exists());
        assert_eq!(read_archive_members(&output).len(), 3);
    }

    // APFS rejects invalid UTF-8 at file creation; Linux filesystems admit it.
    #[cfg(target_os = "linux")]
    #[test]
    fn session_archive_rejects_non_utf8_names_without_lossy_rename() {
        use std::os::unix::ffi::OsStringExt;
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let artifacts = sessions.join(&session.metadata.id).join(ARTIFACTS_DIR_NAME);
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(
            artifacts.join(std::ffi::OsString::from_vec(vec![0xff])),
            b"owned content",
        )
        .unwrap();
        let output = dir.path().join("out.tar.xz");
        let error = write_session_archive(
            &session,
            Some(&sessions),
            &output,
            SessionArchiveOptions::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[test]
    fn session_archive_rejects_nonportable_member_names() {
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let artifacts = sessions.join(&session.metadata.id).join(ARTIFACTS_DIR_NAME);
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("report:private"), b"owned content").unwrap();
        let output = dir.path().join("out.tar.xz");
        let error = write_session_archive(
            &session,
            Some(&sessions),
            &output,
            SessionArchiveOptions::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[test]
    fn session_archive_and_extracted_members_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let session = fixture_session();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.tar.xz");
        write_session_archive(&session, None, &output, SessionArchiveOptions::default()).unwrap();
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut archive = tar::Archive::new(liblzma::read::XzDecoder::new(
            fs::File::open(&output).unwrap(),
        ));
        let extracted = dir.path().join("extracted");
        archive.unpack(&extracted).unwrap();
        for name in [
            SESSION_RECORD_MEMBER,
            SESSION_CONTAINER_MEMBER,
            ARCHIVE_MANIFEST_MEMBER,
        ] {
            assert_eq!(
                fs::metadata(extracted.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    /// #6842: entries a bounded save archived travel with the export.
    #[test]
    fn container_includes_journal_archive_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let session = fixture_session();
        let journal = session.journal.clone().expect("journal");
        let root = journal.entries[0].id.clone();
        let mut archived = journal.clone();
        archived.branch_to(&root).expect("branch");
        let extra = archived.append_message(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "archived alternative".to_string(),
                cache_control: None,
            }],
        });
        let entry = archived.entries.last().expect("entry").clone();
        assert_eq!(entry.id, extra);
        let archive_dir = sessions.join(crate::session_manager::JOURNAL_ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir).expect("archive dir");
        let path = archive_dir.join(format!("{}.jsonl", session.metadata.id));
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let without: SessionImportContainer =
            serde_json::from_str(&container_json(&session, None).unwrap()).unwrap();
        let with: SessionImportContainer =
            serde_json::from_str(&container_json(&session, Some(&sessions)).unwrap()).unwrap();
        assert_eq!(with.entries.len(), without.entries.len() + 1);
        assert!(with.entries.iter().any(|e| e.id == extra));
        with.into_journal().expect("merged container validates");
    }
}
