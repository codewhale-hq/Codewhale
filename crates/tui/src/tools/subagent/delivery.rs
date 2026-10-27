//! Delivery evidence replacing the old prose-verb/git-status heuristic.
//! The worker ledger retains the spawn baseline; this module only reads files.

use super::{
    AgentRunVerificationSummary, AgentWorkerSpec, default_agent_run_verification,
    normalize_claim_path,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) const MAX_DELIVERABLES: usize = 16;
const MAX_BASELINE_PATHS: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliverableVerdict {
    pub path: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    baseline: Option<GitDeliveryBaseline>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) observed_writes: BTreeSet<String>,
    #[serde(default)]
    pub(super) checked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GitDeliveryBaseline {
    root: PathBuf,
    head: Option<String>,
    dirty: BTreeMap<String, String>,
}

fn git_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1");
    command
}

fn git_output(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    git_command(root).args(args).output().ok()
}

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = git_output(root, args)?;
    output.status.success().then_some(output.stdout)
}

/// `git` that reports stderr on failure, for checkpoint notes.
fn git_captured(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(root, args).ok_or_else(|| "git spawn failed".to_string())?;
    if !output.status.success() {
        return Err(first_line_lossy(&output.stderr, 200));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// [`git_captured`] with `paths` fed to git's stdin NUL-terminated, for
/// commands reading `-z --stdin` or `--pathspec-from-file=- --pathspec-file-nul`.
/// No argv length limit applies, so no path count ever needs a wider
/// command instead of the exact list.
fn git_captured_paths(root: &Path, args: &[&str], paths: &[&str]) -> Result<String, String> {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = git_command(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "git spawn failed".to_string())?;
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    if let Some(mut stdin) = child.stdin.take() {
        // A write error means git exited early; its stderr says why.
        let _ = stdin.write_all(&input);
    }
    let output = child
        .wait_with_output()
        .map_err(|_| "git wait failed".to_string())?;
    if !output.status.success() {
        return Err(first_line_lossy(&output.stderr, 200));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn first_line_lossy(bytes: &[u8], max_chars: usize) -> String {
    let line = String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if line.is_empty() {
        return "git failed with no message".to_string();
    }
    line.chars().take(max_chars).collect()
}

fn status_paths(root: &Path) -> Option<BTreeSet<String>> {
    let output = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut entries = output
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty());
    let mut paths = BTreeSet::new();
    while let Some(entry) = entries.next() {
        let path = std::str::from_utf8(entry.get(3..)?).ok()?;
        paths.insert(path.to_string());
        if entry[..2].iter().any(|byte| matches!(byte, b'R' | b'C')) {
            // Porcelain -z emits the destination first, then the source.
            if let Some(source) = entries.next() {
                paths.insert(std::str::from_utf8(source).ok()?.to_string());
            }
        }
    }
    (paths.len() <= MAX_BASELINE_PATHS).then_some(paths)
}

fn fingerprint(root: &Path, relative: &str) -> Option<String> {
    let mut parent = root.to_path_buf();
    let components = Path::new(relative).components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        parent.push(component);
        if fs::symlink_metadata(&parent).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Some("symlink_ancestor".into());
        }
    }
    let path = root.join(relative);

    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some("missing".into());
        }
        Err(_) => return None,
    };
    if metadata.file_type().is_symlink() {
        return Some(format!("symlink:{}", fs::read_link(&path).ok()?.display()));
    }
    if !metadata.is_file() {
        return Some("non_file".into());
    }
    let mut file = fs::File::open(&path).ok()?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Some(format!(
        "sha256:{}",
        codewhale_runtime::hashing::hex_bytes(hash.finalize())
    ))
}

impl DeliveryEvidence {
    pub(super) fn capture(spec: &AgentWorkerSpec) -> Self {
        Self::capture_for_handle(&spec.workspace, spec.runtime_profile.permissions.write)
    }

    /// Baseline capture that needs only the workspace and write permission —
    /// the two spec fields the baseline actually reads. The async spawn path
    /// calls this in `spawn_blocking` BEFORE the manager write lock (#6210)
    /// and threads the evidence through registration, so git + file
    /// fingerprints never run under the lock.
    pub(super) fn capture_for_handle(workspace: &Path, write: bool) -> Self {
        let baseline = write
            .then(|| {
                let root =
                    String::from_utf8(git(workspace, &["rev-parse", "--show-toplevel"])?).ok()?;
                let root = PathBuf::from(root.trim());
                let head = git(&root, &["rev-parse", "--verify", "HEAD"])
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .map(|head| head.trim().to_string());
                let dirty = status_paths(&root)?
                    .into_iter()
                    .map(|path| {
                        let path = normalize_claim_path(&path).ok()?;
                        fingerprint(&root, &path).map(|hash| (path, hash))
                    })
                    .collect::<Option<BTreeMap<_, _>>>()?;
                Some(GitDeliveryBaseline { root, head, dirty })
            })
            .flatten();
        Self {
            baseline,
            ..Self::default()
        }
    }

    pub(super) fn changed_paths(&self, workspace: &Path) -> Option<BTreeSet<String>> {
        let baseline = self.baseline.as_ref()?;
        // Persisted evidence is data, never authority to inspect another tree
        // or pass caller-controlled options to git after a restart.
        let current_root =
            String::from_utf8(git(workspace, &["rev-parse", "--show-toplevel"])?).ok()?;
        let current_root = PathBuf::from(current_root.trim());
        if !same_path_identity(&baseline.root, &current_root)
            || baseline.dirty.len() > MAX_BASELINE_PATHS
            || baseline
                .dirty
                .keys()
                .any(|path| normalize_claim_path(path).as_ref() != Ok(path))
            || baseline.head.as_ref().is_some_and(|head| {
                !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return None;
        }
        let mut candidates = status_paths(&baseline.root)?;
        candidates.extend(baseline.dirty.keys().cloned());
        if let Some(head) = baseline.head.as_deref() {
            let mut args = vec!["diff"];
            args.extend(codewhale_dependencies::Git::REVIEW_DIFF_ARGS);
            args.extend(["--name-only", "-z", head, "HEAD", "--"]);
            let output = git(&baseline.root, &args)?;
            for path in output
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
            {
                candidates.insert(std::str::from_utf8(path).ok()?.to_string());
            }
        }
        // Git status paths are already repo-relative. Prefer them when the
        // worker workspace is the git toplevel by identity; otherwise project
        // through a shared canonicalize spelling. Always normalize separators
        // so Windows `src\lib.rs` matches claim paths like `src/lib.rs`.
        let workspace_root = workspace.canonicalize().ok()?;
        let baseline_root = baseline.root.canonicalize().ok()?;
        let workspace_is_repo_root = same_path_identity(&workspace_root, &baseline_root);
        let mut changed = BTreeSet::new();
        for path in candidates {
            let Ok(path) = normalize_claim_path(&path) else {
                continue;
            };
            if let Some(before) = baseline.dirty.get(&path)
                && fingerprint(&baseline.root, &path).as_ref() == Some(before)
            {
                continue;
            }
            let relative = if workspace_is_repo_root {
                path
            } else {
                let absolute = baseline_root.join(Path::new(&path));
                let Ok(relative) = absolute.strip_prefix(&workspace_root) else {
                    continue;
                };
                let Ok(normalized) = normalize_claim_path(&relative.to_string_lossy()) else {
                    continue;
                };
                normalized
            };
            changed.insert(relative);
        }
        Some(changed)
    }

    /// Commit the worker's uncommitted changes as labeled salvage, and only
    /// on an isolated worktree (#6194 item 4, #5529). Synchronous git reads
    /// and writes: call under `spawn_blocking`, never under the manager
    /// lock. `changed` is the `changed_paths` inventory the caller already
    /// computed; the commit is skipped (not forced) when none of it is still
    /// uncommitted, and every failure degrades to a note — never an error.
    ///
    /// Ownership is exact at every size (#6557 D02-11): only paths that are
    /// both in `changed` and dirty now are staged and committed. A
    /// pre-existing dirty file the worker never touched stays dirty, and a
    /// file staged by someone else stays staged but outside the commit
    /// (`git commit --only` with the owned pathspecs). Paths travel on stdin
    /// as literal pathspecs, so a name like `*.rs` is never a glob and no
    /// inventory size widens staging to the whole tree. A dirty tree larger
    /// than the baseline bound is refused rather than guessed at.
    pub(super) fn checkpoint_uncommitted(
        &self,
        changed: &BTreeSet<String>,
        agent_id: &str,
        cause: &str,
        isolated_worktree: bool,
    ) -> BudgetCheckpointOutcome {
        use BudgetCheckpointOutcome::*;
        if !isolated_worktree {
            // A shared checkout may hold the parent's or a sibling's dirty
            // files; auto-commit would sweep them into the checkpoint.
            return SkippedNonIsolated;
        }
        if changed.is_empty() {
            return Clean;
        }
        let Some(baseline) = self.baseline.as_ref() else {
            return Failed {
                reason: "no delivery baseline".to_string(),
            };
        };
        let Some(dirty) = status_paths(&baseline.root) else {
            return Failed {
                reason: format!(
                    "git status failed or listed more than {MAX_BASELINE_PATHS} paths; nothing was staged"
                ),
            };
        };
        // Status spelling is what git matches; `changed` is normalized.
        let owned: Vec<&str> = dirty
            .iter()
            .filter(|path| normalize_claim_path(path).is_ok_and(|path| changed.contains(&path)))
            .map(String::as_str)
            .collect();
        if owned.is_empty() {
            return Clean;
        }
        // `update-index` takes literal paths and stages every status shape:
        // edits, new files (`--add`), and deletions or rename sources whose
        // file is gone (`--remove`), where `git add <path>` would fail.
        if let Err(reason) = git_captured_paths(
            &baseline.root,
            &["update-index", "--add", "--remove", "-z", "--stdin"],
            &owned,
        ) {
            return Failed { reason };
        }
        let cause_short: String = cause
            .lines()
            .next()
            .unwrap_or(cause)
            .chars()
            .take(120)
            .collect();
        let message = format!(
            "checkpoint: {agent_id} ({cause_short}) - {} uncommitted file(s) at budget death; unreviewed salvage",
            owned.len()
        );
        if let Err(reason) = git_captured_paths(
            &baseline.root,
            &[
                "--literal-pathspecs",
                "-c",
                "user.name=Codewhale Subagent",
                "-c",
                "user.email=subagent@codewhale.invalid",
                "commit",
                "--quiet",
                "--only",
                "-m",
                &message,
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &owned,
        ) {
            return Failed { reason };
        }
        match git_captured(&baseline.root, &["rev-parse", "--short", "HEAD"]) {
            Ok(sha) => Committed {
                sha: sha.trim().to_string(),
            },
            Err(reason) => Failed { reason },
        }
    }
}

/// Outcome of the budget-death checkpoint commit.
pub(super) enum BudgetCheckpointOutcome {
    /// Uncommitted work is now commit `sha` on the worker branch.
    Committed { sha: String },
    /// Nothing attributable to commit (clean tree, or the worker committed).
    Clean,
    /// Shared checkout: auto-commit would sweep up other writers' work.
    SkippedNonIsolated,
    /// Nothing was committed; files survive on disk.
    Failed { reason: String },
}

fn same_path_identity(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub(super) fn declared_paths(
    paths: &[String],
    legacy: Option<&str>,
) -> Result<Vec<String>, String> {
    if paths.len() > MAX_DELIVERABLES {
        return Err(format!(
            "deliverables accepts at most {MAX_DELIVERABLES} paths"
        ));
    }
    let mut paths = paths.to_vec();
    if paths.is_empty()
        && let Some(legacy) = legacy
        && !legacy.chars().any(char::is_whitespace)
        && (legacy.contains('/') || legacy.contains('.'))
    {
        paths.push(legacy.to_string());
    }
    let mut normalized = Vec::new();
    for path in paths {
        let path = normalize_claim_path(&path)?;
        if path == "."
            || path.split('/').any(|part| {
                codewhale_util::git_metadata::is_git_metadata_name(std::ffi::OsStr::new(part))
            })
        {
            return Err("deliverables must name files outside git metadata".into());
        }
        if !normalized.contains(&path) {
            normalized.push(path);
        }
    }
    Ok(normalized)
}

pub(super) fn safe_deliverable_path(workspace: &Path, path: &str) -> Result<PathBuf, String> {
    let path = declared_paths(&[path.to_string()], None)?
        .pop()
        .ok_or_else(|| "deliverable must name a file".to_string())?;
    let root = workspace
        .canonicalize()
        .map_err(|_| "deliverable workspace is unavailable".to_string())?;
    let mut resolved = root.clone();
    for component in Path::new(&path).components() {
        resolved.push(component);
        match fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("deliverable path traverses a symlink".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("deliverable path cannot be inspected".into()),
        }
    }
    if resolved == root || !resolved.starts_with(root) {
        return Err("deliverable must be a file inside the worker workspace".into());
    }
    Ok(resolved)
}

pub(super) fn check_deliverable(workspace: &Path, path: &str, allowed: bool) -> DeliverableVerdict {
    let mut verdict = DeliverableVerdict {
        path: path.into(),
        status: "out_of_scope".into(),
        bytes: None,
    };
    if !allowed {
        return verdict;
    }
    let resolved = match safe_deliverable_path(workspace, path) {
        Ok(path) => path,
        Err(_) => {
            verdict.status = "invalid_path".into();
            return verdict;
        }
    };
    match fs::symlink_metadata(resolved) {
        Ok(metadata) if metadata.is_file() => {
            verdict.bytes = Some(metadata.len());
            verdict.status = if metadata.len() == 0 {
                "empty"
            } else {
                "present"
            }
            .into();
        }
        Ok(_) => verdict.status = "not_file".into(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            verdict.status = "missing".into()
        }
        Err(_) => verdict.status = "unreadable".into(),
    }
    verdict
}

fn citation(token: &str) -> bool {
    // A citation may be sentence-final or either side of a Markdown link.
    // Normalize punctuation only for citation detection; never rewrite a path.
    token.split("](").any(|part| {
        let part = part.trim_end_matches(|ch: char| {
            matches!(
                ch,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"' | '`'
            )
        });
        let Some((_, line)) = part.rsplit_once(':') else {
            return false;
        };
        let mut numbers = line.split('-');
        let numeric =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        numbers.next().is_some_and(numeric)
            && numbers.next().is_none_or(numeric)
            && numbers.next().is_none()
    })
}

pub(super) fn explicit_change_paths(summary: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let mut in_changes = false;
    for line in summary.lines() {
        let line = line.trim();
        let is_heading = line.starts_with('#');
        let line = line.trim_start_matches('#').trim();
        let lower = line.to_ascii_lowercase();
        // SUBAGENT_OUTPUT_FORMAT uses a bare Markdown heading, with ordinary
        // blank lines before its file bullets. That is an explicit declaration
        // boundary just like the compatibility CHANGES: label.
        if is_heading && lower == "changes" {
            in_changes = true;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let declaration = ["changes:", "changed files:", "files changed:"]
            .into_iter()
            .find(|prefix| lower.starts_with(prefix));
        let content = if let Some(prefix) = declaration {
            in_changes = true;
            &line[prefix.len()..]
        } else if in_changes && (line.starts_with('-') || line.starts_with('*')) {
            line
        } else {
            in_changes = false;
            continue;
        };
        if matches!(
            content
                .trim()
                .trim_end_matches('.')
                .to_ascii_lowercase()
                .as_str(),
            "none" | "no files changed" | "no changes"
        ) {
            in_changes = false;
            continue;
        }
        let content = content.trim();
        let mut remaining = content
            .strip_prefix("- ")
            .or_else(|| content.strip_prefix("* "))
            .unwrap_or(content);
        // File declarations lead with paths. Stop when their description
        // starts instead of interpreting sentence-final prose ("parsing.")
        // or a later reference ("see notes.md") as another claimed edit.
        while !remaining.is_empty() {
            remaining = remaining
                .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';'));
            let Some(first) = remaining.chars().next() else {
                break;
            };
            let quoted = matches!(first, '`' | '"' | '\'');
            let (token, rest) = if quoted {
                let Some((token, rest)) = remaining[1..].split_once(first) else {
                    break;
                };
                (token, rest)
            } else {
                let end = remaining
                    .find(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';'))
                    .unwrap_or(remaining.len());
                remaining.split_at(end)
            };
            remaining = rest;
            if citation(token) || token.contains("://") {
                break;
            }
            let token = if quoted {
                token
            } else {
                token
                    .trim_matches(|ch: char| matches!(ch, '(' | ')' | '[' | ']' | '*' | '-'))
                    .trim_end_matches(['.', ':', '!', '?'])
            };
            let token = token.split_once("](").map_or(token, |(label, _)| label);
            if !token.contains('/') && !token.contains('.') {
                break;
            }
            let Ok(path) = normalize_claim_path(token) else {
                break;
            };
            if path == "." {
                break;
            }
            paths.insert(path);
        }
    }
    paths
}

pub(super) fn verify_changes(
    summary: &str,
    write_capable: bool,
    evidence: &DeliveryEvidence,
    changed: Option<&BTreeSet<String>>,
    declared_outputs: &BTreeSet<String>,
) -> Option<AgentRunVerificationSummary> {
    if !write_capable {
        return None;
    }
    let claimed = explicit_change_paths(summary);
    let missing = changed
        .map(|changed| claimed.difference(changed).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    // A path changing inside a writable scope proves neither the actor nor a
    // child write. External tools and people can edit the same checkout, so only
    // successful bounded write receipts can support an undeclared-write claim.
    let undeclared = evidence
        .observed_writes
        .iter()
        .filter(|path| {
            changed.is_none_or(|changed| changed.contains(*path))
                && !claimed.contains(*path)
                && !declared_outputs.contains(*path)
        })
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() && undeclared.is_empty() {
        return None;
    }
    Some(AgentRunVerificationSummary {
        status: "claim_mismatch".into(),
        summary: format!(
            "Compared with the workspace at spawn: declared but unchanged: {missing:?}; observed changes without a change declaration: {undeclared:?}. Inspect the worker receipt."
        ),
        deliverables: Vec::new(),
    })
}

/// Everything delivery verification needs, snapshotted under a read lock.
/// `allowed[i]` is the write-scope verdict for `deliverables[i]`. The compute
/// half runs in `spawn_blocking` with no manager lock held (#6210).
#[derive(Debug, Clone)]
pub(super) struct DeliveryVerificationInputs {
    pub evidence: DeliveryEvidence,
    pub workspace: PathBuf,
    pub result_text: String,
    pub write_perm: bool,
    pub deliverables: Vec<String>,
    pub allowed: Vec<bool>,
    /// The worker ran in its own isolated worktree and settled in a state that
    /// cannot be resumed there; remove the worktree if it changed nothing.
    pub remove_worktree_if_unchanged: bool,
}

/// Pure compute half of worker delivery verification: the git trio +
/// fingerprints (`changed_paths`), claim comparison, and per-deliverable
/// presence checks. Runs off the manager lock; the caller stores the summary.
pub(super) fn compute_delivery_verification(
    inputs: &DeliveryVerificationInputs,
) -> AgentRunVerificationSummary {
    let changed = inputs.evidence.changed_paths(&inputs.workspace);
    let mut verification = verify_changes(
        &inputs.result_text,
        inputs.write_perm,
        &inputs.evidence,
        changed.as_ref(),
        &inputs.deliverables.iter().cloned().collect(),
    )
    .unwrap_or_else(default_agent_run_verification);
    verification.deliverables = inputs
        .deliverables
        .iter()
        .zip(inputs.allowed.iter())
        .map(|(path, allowed)| check_deliverable(&inputs.workspace, path, *allowed))
        .collect();
    let missing = verification
        .deliverables
        .iter()
        .filter(|verdict| verdict.status != "present")
        .map(|verdict| format!("{} ({})", verdict.path, verdict.status))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let prior = if verification.status == "claim_mismatch" {
            format!(" {}", verification.summary)
        } else {
            String::new()
        };
        verification.status = "deliverable_missing".to_string();
        verification.summary = format!(
            "Declared deliverables not produced as non-empty files in the worker write scope: {}.{prior}",
            missing.join(", ")
        );
    } else if !inputs.deliverables.is_empty() && verification.status == "self_report_only" {
        verification.status = "deliverables_present".to_string();
        verification.summary = "Declared files exist and are non-empty inside the worker write scope; their contents remain a worker self-report.".to_string();
    }
    verification
}
