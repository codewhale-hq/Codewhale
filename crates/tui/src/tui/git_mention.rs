//! `@git` and `@diff` composer mentions (#4067).
//!
//! The `@` mention system is otherwise path-centric: every token resolves to
//! a file or directory. These two tokens resolve to *curated git context*
//! instead, so a user can attach "what is going on in this working tree"
//! inline rather than making the model spend a round-trip on `git_diff` or a
//! shell command that may need approval.
//!
//! Two deliberate boundaries:
//!
//! * **Read-only and bounded.** Only `git status` and `git diff` run, always
//!   with an explicit byte budget and time limit. A repository with a huge
//!   working-tree diff truncates with a visible marker rather than flooding
//!   the turn, and a wedged git reports a timeout instead of hanging it.
//! * **Honest when unavailable.** No git binary, or a directory that is not a
//!   repository, produces an explicit `<git-unavailable>` block. A mention
//!   never silently contributes nothing.

use std::io;
use std::path::Path;
use std::time::Duration;

use codewhale_dependencies::run_bounded_git_with_timeout;
use codewhale_dependencies::{ExternalTool, Git};

const GIT_MENTION_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_COMMAND_FAILED: &str = "git command failed";
const GIT_COMMAND_TIMED_OUT: &str = "git command timed out";
const GIT_COMMAND_NOT_RUN: &str = "git could not run safely";

/// Byte ceiling for the inlined `@diff` payload. Documented here because the
/// context inspector reports the budget alongside actual size.
pub const MAX_GIT_DIFF_MENTION_BYTES: usize = 32 * 1024;
/// Byte ceiling for the inlined `@git` status summary. Status output is
/// bounded in practice, but an unignored `node_modules` can still produce
/// tens of thousands of lines.
pub const MAX_GIT_STATUS_MENTION_BYTES: usize = 8 * 1024;

/// Which curated git payload a mention token asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitMentionKind {
    /// `@git` — a bounded `git status` summary plus the current branch.
    Status,
    /// `@diff` — the working-tree diff, staged and unstaged.
    Diff,
}

impl GitMentionKind {
    /// The mention token that selects this payload, without the `@`.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Status => "git",
            Self::Diff => "diff",
        }
    }

    /// Byte budget for the inlined payload.
    #[must_use]
    pub fn byte_budget(self) -> usize {
        match self {
            Self::Status => MAX_GIT_STATUS_MENTION_BYTES,
            Self::Diff => MAX_GIT_DIFF_MENTION_BYTES,
        }
    }

    /// Every git mention kind, in completion-menu order.
    pub fn iter_all() -> impl Iterator<Item = Self> {
        GIT_MENTION_KINDS.into_iter()
    }

    /// Short label for composer previews and the context inspector.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Status => "git status",
            Self::Diff => "working-tree diff",
        }
    }
}

/// Every git mention token, in completion-menu order.
pub const GIT_MENTION_KINDS: [GitMentionKind; 2] = [GitMentionKind::Status, GitMentionKind::Diff];

/// Classify a raw mention token. Case-insensitive so `@Git` and `@Diff`
/// behave like the lowercase spellings; a path that merely *starts* with
/// `git` (`@git/config`, `@diff.txt`) stays a file mention.
#[must_use]
pub fn git_mention_kind(raw: &str) -> Option<GitMentionKind> {
    let token = raw.trim();
    GIT_MENTION_KINDS
        .into_iter()
        .find(|kind| token.eq_ignore_ascii_case(kind.token()))
}

/// Outcome of resolving a git mention against a working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitMentionPayload {
    /// The model-facing block, already wrapped in its tag.
    pub block: String,
    /// Byte size of the payload actually inlined (the tag itself excluded).
    pub bytes: usize,
    /// Whether the payload hit its budget and was cut.
    pub truncated: bool,
    /// Present when git could not produce the payload at all.
    pub unavailable_reason: Option<String>,
}

impl GitMentionPayload {
    fn unavailable(kind: GitMentionKind, reason: &str) -> Self {
        Self {
            block: format!(
                "<git-unavailable mention=\"@{token}\" reason=\"{reason}\" />",
                token = kind.token(),
            ),
            bytes: 0,
            truncated: false,
            unavailable_reason: Some(reason.to_string()),
        }
    }
}

/// Per-submit memo for resolved git mentions.
///
/// One message send resolves mentions twice — once to build the context
/// inspector references and once to build the model-facing payload. For
/// `@diff` each resolution makes git compute the *entire* working-tree diff
/// before the 32 KB budget applies, so a large repository paid for that twice
/// to attach it once.
///
/// Scoped deliberately: a cache lives for one submit and is then dropped, so a
/// second `@diff` in a later message always re-shells out and can never show a
/// stale working tree.
#[derive(Debug, Default)]
pub struct GitMentionCache {
    resolved: std::collections::HashMap<(GitMentionKind, std::path::PathBuf), GitMentionPayload>,
}

impl GitMentionCache {
    /// Number of distinct mentions resolved so far this submit. Used by tests
    /// to prove one submit shells out once per mention.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.resolved.len()
    }

    /// Resolve `kind` against `workspace`, reusing this submit's result.
    pub fn resolve(&mut self, kind: GitMentionKind, workspace: &Path) -> &GitMentionPayload {
        self.resolved
            .entry((kind, workspace.to_path_buf()))
            .or_insert_with(|| resolve_git_mention(kind, workspace))
    }
}

/// Run the git commands for `kind` in `cwd` and render the model-facing block.
///
/// Never returns an error: an unavailable git, a non-repository directory, or
/// a failing command all resolve to an explicit `<git-unavailable>` block so
/// the turn records why the mention contributed nothing.
#[must_use]
pub fn resolve_git_mention(kind: GitMentionKind, cwd: &Path) -> GitMentionPayload {
    if !Git::available() {
        return GitMentionPayload::unavailable(kind, "git not found on PATH");
    }
    match git_output(&["rev-parse", "--is-inside-work-tree"], cwd) {
        Ok(out) if out.trim() == "true" => {}
        Ok(_) | Err(GIT_COMMAND_FAILED) => {
            return GitMentionPayload::unavailable(kind, "not a git repository");
        }
        Err(reason) => return GitMentionPayload::unavailable(kind, reason),
    }

    let raw = match kind {
        GitMentionKind::Status => git_status_payload(cwd),
        GitMentionKind::Diff => git_output(
            &[&["diff"][..], &Git::REVIEW_DIFF_ARGS[..], &["HEAD"][..]].concat(),
            cwd,
        ),
    };
    let raw = match raw {
        Ok(raw) => raw,
        Err(reason) => return GitMentionPayload::unavailable(kind, reason),
    };

    if raw.trim().is_empty() {
        let reason = match kind {
            GitMentionKind::Status => "working tree clean",
            GitMentionKind::Diff => "no working-tree changes",
        };
        return GitMentionPayload::unavailable(kind, reason);
    }

    let (body, truncated) = truncate_on_char_boundary(&raw, kind.byte_budget());
    let tag = match kind {
        GitMentionKind::Status => "git-status",
        GitMentionKind::Diff => "git-diff",
    };
    let truncated_attr = if truncated {
        format!(
            " truncated=\"true\" budget-bytes=\"{}\"",
            kind.byte_budget()
        )
    } else {
        String::new()
    };
    let block = format!(
        "<{tag} mention=\"@{token}\" bytes=\"{bytes}\"{truncated_attr}>\n{body}\n</{tag}>",
        token = kind.token(),
        bytes = body.len(),
    );

    GitMentionPayload {
        block,
        bytes: body.len(),
        truncated,
        unavailable_reason: None,
    }
}

/// `git status` plus the branch line, so the model does not have to infer the
/// branch from a porcelain listing.
fn git_status_payload(cwd: &Path) -> Result<String, &'static str> {
    git_output(&["status", "--short", "--branch"], cwd)
}

/// Run git and return stdout, or the `<git-unavailable>` reason when the
/// binary is missing, the command exits non-zero, or it outlives
/// `GIT_MENTION_TIMEOUT` and is killed.
///
/// Runs under [`Git::review_command`], so repository-configured filters,
/// fsmonitor and hooks do not execute while reading the working tree.
fn git_output(args: &[&str], cwd: &Path) -> Result<String, &'static str> {
    let mut command = Git::review_command(cwd).map_err(|err| {
        if err
            .chain()
            .filter_map(|cause| cause.downcast_ref::<io::Error>())
            .any(|cause| cause.kind() == io::ErrorKind::TimedOut)
        {
            GIT_COMMAND_TIMED_OUT
        } else {
            GIT_COMMAND_NOT_RUN
        }
    })?;
    command.args(args);
    let subcommand = args.first().copied().unwrap_or("git");
    let output = run_bounded_git_with_timeout(&mut command, subcommand, GIT_MENTION_TIMEOUT)
        .map_err(|err| match err.kind() {
            io::ErrorKind::TimedOut => GIT_COMMAND_TIMED_OUT,
            _ => GIT_COMMAND_NOT_RUN,
        })?;
    if !output.status.success() {
        return Err(GIT_COMMAND_FAILED);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Cut `text` to at most `budget` bytes without splitting a UTF-8 scalar.
/// Returns the slice and whether anything was dropped.
fn truncate_on_char_boundary(text: &str, budget: usize) -> (&str, bool) {
    if text.len() <= budget {
        return (text, false);
    }
    let mut end = budget;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .expect("git available in tests");
            assert!(status.status.success(), "git {args:?} failed");
        }
    }

    fn commit_all(dir: &Path, message: &str) {
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn only_exact_tokens_are_git_mentions() {
        assert_eq!(git_mention_kind("git"), Some(GitMentionKind::Status));
        assert_eq!(git_mention_kind("Diff"), Some(GitMentionKind::Diff));
        // Paths that merely start with the token stay file mentions.
        assert_eq!(git_mention_kind("git/config"), None);
        assert_eq!(git_mention_kind("diff.txt"), None);
        assert_eq!(git_mention_kind("gitignore"), None);
    }

    #[test]
    fn non_repository_directory_is_explicitly_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let payload = resolve_git_mention(GitMentionKind::Diff, dir.path());
        assert_eq!(payload.bytes, 0);
        assert!(payload.block.contains("git-unavailable"));
        assert!(
            payload
                .unavailable_reason
                .as_deref()
                .is_some_and(|r| r.contains("not a git repository")),
            "unexpected reason: {:?}",
            payload.unavailable_reason
        );
    }

    #[test]
    fn empty_repository_reports_clean_rather_than_an_empty_block() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");

        let payload = resolve_git_mention(GitMentionKind::Diff, dir.path());
        assert_eq!(payload.bytes, 0);
        assert!(payload.block.contains("no working-tree changes"));
    }

    /// `@diff` and `@git` read the working tree without running the
    /// repository's external diff, textconv or clean filter.
    #[cfg(unix)]
    #[test]
    fn mentions_run_no_repository_configured_commands() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let marker = outside.path().join("marker");
        let script = outside.path().join("helper.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho ran >> '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script = script.display().to_string();
        init_repo(dir.path());
        std::fs::write(
            dir.path().join(".gitattributes"),
            "a.txt diff=conv filter=x\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        for (key, value) in [
            ("filter.x.clean", &script),
            ("diff.conv.textconv", &script),
            ("diff.external", &script),
        ] {
            let status = Command::new("git")
                .args(["config", key, value])
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success());
        }
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();

        let diff = resolve_git_mention(GitMentionKind::Diff, dir.path());
        let status = resolve_git_mention(GitMentionKind::Status, dir.path());
        assert!(
            !marker.exists(),
            "a mention ran a repository-configured command"
        );
        assert!(diff.block.contains("+changed"), "{}", diff.block);
        assert!(status.block.contains("a.txt"), "{}", status.block);
    }

    #[test]
    fn status_reports_branch_and_dirty_paths() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        std::fs::write(dir.path().join("b.txt"), "new\n").unwrap();

        let payload = resolve_git_mention(GitMentionKind::Status, dir.path());
        assert!(payload.unavailable_reason.is_none());
        assert!(payload.block.starts_with("<git-status mention=\"@git\""));
        assert!(payload.block.contains("b.txt"), "{}", payload.block);
        assert!(!payload.truncated);
    }

    #[test]
    fn diff_covers_staged_and_unstaged_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "one\n").unwrap();
        commit_all(dir.path(), "initial");

        std::fs::write(dir.path().join("a.txt"), "staged\n").unwrap();
        Command::new("git")
            .args(["add", "a.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("b.txt"), "unstaged\n").unwrap();

        let payload = resolve_git_mention(GitMentionKind::Diff, dir.path());
        assert!(payload.unavailable_reason.is_none());
        assert!(payload.block.contains("staged"), "{}", payload.block);
        assert!(payload.block.contains("unstaged"), "{}", payload.block);
    }

    #[test]
    fn large_diff_truncates_at_the_documented_budget() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("big.txt"), "seed\n").unwrap();
        commit_all(dir.path(), "initial");

        let bulk: String = (0..40_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), bulk).unwrap();

        let payload = resolve_git_mention(GitMentionKind::Diff, dir.path());
        assert!(payload.truncated, "expected truncation");
        assert!(payload.bytes <= MAX_GIT_DIFF_MENTION_BYTES);
        assert!(payload.block.contains("truncated=\"true\""));
        assert!(payload.block.contains("budget-bytes=\"32768\""));
    }

    #[test]
    fn truncation_never_splits_a_utf8_scalar() {
        // Budget lands mid-scalar: "é" is two bytes starting at index 1.
        let (cut, truncated) = truncate_on_char_boundary("aéb", 2);
        assert!(truncated);
        assert_eq!(cut, "a");
    }
}
