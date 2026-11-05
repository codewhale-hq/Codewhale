/// Whether one path component names the repository metadata directory as the
/// filesystem resolves it, not just as spelled: `.git` in any letter case
/// (macOS and Windows default to case-insensitive names) and, on Windows,
/// with the trailing dots/spaces or `:stream` suffix it drops and the `GIT~N`
/// short-name alias. The one `.git` rule for workspace file routes, file
/// restore, displayed workspace paths, the write carve-out and sub-agent
/// deliverables.
pub fn is_git_metadata_name(name: &std::ffi::OsStr) -> bool {
    git_metadata_name(&name.to_string_lossy(), cfg!(windows))
}

pub fn git_metadata_name(name: &str, windows: bool) -> bool {
    let name = if windows {
        name.split(':')
            .next()
            .unwrap_or_default()
            .trim_end_matches(['.', ' '])
    } else {
        name
    };
    name.eq_ignore_ascii_case(".git")
        || (windows
            && name.len() > 4
            && name
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("git~"))
            && name[4..].bytes().all(|byte| byte.is_ascii_digit()))
}

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_GITDIR_POINTER_BYTES: u64 = 64 * 1024;
const MAX_HEAD_OR_CONFIG_BYTES: u64 = 1024 * 1024;
const ABBREVIATED_ID_LEN: usize = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHead {
    pub branch: Option<String>,
    pub id: Option<String>,
}

impl GitHead {
    pub fn short_id(&self) -> Option<&str> {
        self.id
            .as_deref()
            .map(|id| id.get(..ABBREVIATED_ID_LEN).unwrap_or(id))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GitReadError {
    #[error("not inside a git repository")]
    NotRepository,
    #[error("unsupported git repository: {0}")]
    Unsupported(String),
    #[error("corrupt git repository: {0}")]
    Corrupt(String),
    #[error("git read cancelled")]
    Cancelled,
}

pub fn read_head(workspace: &Path, cancel: &AtomicBool) -> Result<GitHead, GitReadError> {
    check_cancelled(cancel)?;
    precheck_metadata_sizes(workspace)?;
    let options = gix::open::Options::isolated()
        .bail_if_untrusted(true)
        .strict_config(true)
        .config_overrides(["core.useReplaceRefs=false", "core.fsmonitor=false"]);
    let repository = match gix::discover_opts(workspace, Default::default(), options) {
        Ok(repository) => repository,
        Err(error) => {
            return Err(
                match error.downcast_any_ref::<gix::discover::upwards::Error>() {
                    Some(
                        gix::discover::upwards::Error::NoGitRepository { .. }
                        | gix::discover::upwards::Error::NoGitRepositoryWithinCeiling { .. }
                        | gix::discover::upwards::Error::NoGitRepositoryWithinFs { .. },
                    ) => GitReadError::NotRepository,
                    _ => GitReadError::Unsupported(error.to_string()),
                },
            );
        }
    };
    if repository.workdir().is_none() {
        return Err(GitReadError::NotRepository);
    }
    check_cancelled(cancel)?;
    let head = repository
        .head()
        .map_err(|error| GitReadError::Corrupt(error.to_string()))?;
    let branch = match head.referent_name() {
        Some(name) => Some(
            name.as_bstr()
                .strip_prefix(b"refs/heads/")
                .and_then(|branch| std::str::from_utf8(branch).ok())
                .map(str::to_owned)
                .ok_or_else(|| {
                    GitReadError::Corrupt("HEAD does not name a local branch".to_string())
                })?,
        ),
        None => None,
    };
    let id = head.id().map(|id| id.to_string());
    check_cancelled(cancel)?;
    Ok(GitHead { branch, id })
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), GitReadError> {
    if cancel.load(Ordering::Relaxed) {
        Err(GitReadError::Cancelled)
    } else {
        Ok(())
    }
}

fn precheck_metadata_sizes(workspace: &Path) -> Result<(), GitReadError> {
    for directory in workspace.ancestors() {
        let marker = directory.join(".git");
        let Ok(metadata) = std::fs::metadata(&marker) else {
            continue;
        };
        if metadata.is_file() {
            if metadata.len() > MAX_GITDIR_POINTER_BYTES {
                return Err(GitReadError::Unsupported(
                    ".git pointer file is too large".to_string(),
                ));
            }
        } else if metadata.is_dir() {
            for name in ["HEAD", "config"] {
                if let Ok(metadata) = std::fs::metadata(marker.join(name))
                    && (!metadata.is_file() || metadata.len() > MAX_HEAD_OR_CONFIG_BYTES)
                {
                    return Err(GitReadError::Unsupported(format!(
                        ".git/{name} is not a regular file within the size bound"
                    )));
                }
            }
        }
        return Ok(());
    }
    Ok(())
}
