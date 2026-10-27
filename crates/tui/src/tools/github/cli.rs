//! Command construction: every `gh` and `git` process this tool runs.
//!
//! Nothing above this file assembles an argv or resolves a binary path, so
//! "what did the tool actually shell out to" has exactly one answer.

use std::process::Command;

use serde_json::Value;

use crate::tools::spec::{ToolContext, ToolError};
use codewhale_dependencies::ExternalTool;

const DEFAULT_GH: &str = "/opt/homebrew/bin/gh";
const FALLBACK_GH_PATHS: &[&str] = &[
    "/usr/bin/gh",                       // Linux system package manager
    "/usr/local/bin/gh",                 // macOS Intel Homebrew / manual install
    "/home/linuxbrew/.linuxbrew/bin/gh", // Linux Homebrew (official prefix)
    "/opt/homebrew/bin/gh",              // macOS Apple Silicon Homebrew
];

fn gh_bin() -> String {
    if let Ok(bin) = std::env::var("CODEWHALE_GH_BIN").or_else(|_| std::env::var("DEEPSEEK_GH_BIN"))
    {
        return bin;
    }
    for path in FALLBACK_GH_PATHS {
        if std::path::Path::new(path).is_file() {
            return path.to_string();
        }
    }
    DEFAULT_GH.to_string()
}

pub(super) fn run_gh_text(context: &ToolContext, args: &[&str]) -> Result<String, ToolError> {
    let out = Command::new(gh_bin())
        .args(args)
        .current_dir(&context.workspace)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::not_available("gh CLI not found; install it or set DEEPSEEK_GH_BIN")
            } else {
                ToolError::execution_failed(format!("failed to run gh: {e}"))
            }
        })?;
    if !out.status.success() {
        return Err(ToolError::execution_failed(format!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub(super) fn run_gh_json(context: &ToolContext, args: &[&str]) -> Result<Value, ToolError> {
    let text = run_gh_text(context, args)?;
    serde_json::from_str(&text).map_err(|e| ToolError::execution_failed(e.to_string()))
}

pub(super) fn ensure_github_repo(context: &ToolContext) -> Result<(), ToolError> {
    let out = codewhale_dependencies::Git::output(
        &["rev-parse", "--is-inside-work-tree"],
        &context.workspace,
    )
    .map_err(|e| ToolError::execution_failed(format!("failed to run git: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(ToolError::not_available(
            "current workspace is not a git repository",
        ))
    }
}

pub(super) fn git_status_porcelain(context: &ToolContext) -> Result<String, ToolError> {
    let out = codewhale_dependencies::Git::output(&["status", "--porcelain"], &context.workspace)
        .map_err(|e| ToolError::execution_failed(format!("failed to run git status: {e}")))?;
    if !out.status.success() {
        return Err(ToolError::execution_failed(format!(
            "git status failed ({}); cannot verify that the worktree is clean",
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// One captured Core repo selection, reused for every step (including comment-before-close).
pub(super) struct HostTarget {
    repo: String,
    host: String,
}
pub(super) async fn host_target(
    context: &ToolContext,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<HostTarget, ToolError> {
    let override_repo = std::env::var("GH_REPO")
        .ok()
        .filter(|value| !value.is_empty());
    let (host, repo) = if let Some(repo) = override_repo {
        let parts = repo.split('/').collect::<Vec<_>>();
        match parts.as_slice() {
            [owner, name] => (
                std::env::var("GH_HOST").unwrap_or_else(|_| "github.com".into()),
                format!("{owner}/{name}"),
            ),
            [host, owner, name] => (host.to_string(), format!("{owner}/{name}")),
            _ => {
                return Err(ToolError::invalid_input(
                    "GitHub repository override is invalid",
                ));
            }
        }
    } else {
        let remote = host_git(context, &["remote", "get-url", "origin"], cancel).await?;
        let remote = remote.trim();
        let parsed = if remote.contains("://") {
            url::Url::parse(remote)
        } else if let Some((host, path)) = remote.split_once(':') {
            url::Url::parse(&format!("ssh://{host}/{path}"))
        } else {
            return Err(ToolError::not_available(
                "GitHub requires a network origin repository",
            ));
        };
        let url = parsed.map_err(|_| ToolError::invalid_input("GitHub origin is invalid"))?;
        if !matches!(url.scheme(), "https" | "http" | "ssh")
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ToolError::invalid_input("GitHub origin is invalid"));
        }
        let host = std::env::var("GH_HOST")
            .ok()
            .filter(|host| !host.is_empty())
            .or_else(|| url.host_str().map(str::to_string))
            .ok_or_else(|| ToolError::invalid_input("GitHub origin host missing"))?;
        (
            host,
            url.path()
                .trim_matches('/')
                .trim_end_matches(".git")
                .to_string(),
        )
    };
    if host.is_empty()
        || host
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '-'))
        || repo.split('/').count() != 2
        || repo.split('/').any(|part| {
            part.is_empty()
                || part
                    .chars()
                    .any(|c| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-'))
        })
    {
        return Err(ToolError::invalid_input(
            "GitHub repository selection is invalid",
        ));
    }
    Ok(HostTarget {
        repo: format!("{host}/{repo}"),
        host,
    })
}
pub(super) async fn host_gh(
    context: &ToolContext,
    target: &HostTarget,
    args: &[&str],
    input: Option<&[u8]>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String, ToolError> {
    use codewhale_net::network_policy::Decision;
    if context
        .tool_authority
        .as_ref()
        .is_some_and(|authority| authority.network_access == Some(false))
    {
        return Err(ToolError::permission_denied(
            "GitHub network access exceeds the current authority",
        ));
    }
    if context
        .network_policy
        .as_ref()
        .is_some_and(|policy| policy.evaluate(&target.host, "github") != Decision::Allow)
    {
        return Err(ToolError::permission_denied(
            "GitHub access is blocked or awaiting network approval",
        ));
    }
    let mut command = if let Ok(binary) =
        std::env::var("CODEWHALE_GH_BIN").or_else(|_| std::env::var("DEEPSEEK_GH_BIN"))
    {
        tokio::process::Command::new(binary)
    } else {
        codewhale_dependencies::Gh::tokio_command()
            .ok_or_else(|| ToolError::not_available("gh CLI is unavailable"))?
    };
    codewhale_util::utils::suppress_tokio_console_window(&mut command);
    command
        .args(args)
        .args(["--repo", &target.repo])
        .env("GH_HOST", &target.host)
        .env_remove("GH_REPO")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(&context.workspace);
    host_output(&mut command, input.unwrap_or_default(), cancel, "GitHub").await
}
pub(super) async fn host_git(
    context: &ToolContext,
    args: &[&str],
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String, ToolError> {
    let mut command = codewhale_dependencies::Git::tokio_command()
        .ok_or_else(|| ToolError::not_available("git is unavailable"))?;
    command.args(args).current_dir(&context.workspace);
    host_output(&mut command, &[], cancel, "Git repository inspection").await
}
async fn host_output(
    command: &mut tokio::process::Command,
    input: &[u8],
    cancel: &tokio_util::sync::CancellationToken,
    label: &str,
) -> Result<String, ToolError> {
    if cancel.is_cancelled() {
        return Err(ToolError::not_available("GitHub operation cancelled"));
    }
    let output = codewhale_runtime::process_tree::contained_output_with_input_bounded(
        command,
        input.to_vec(),
        1024 * 1024,
        64 * 1024,
        cancel.cancelled(),
    )
    .await
    .map_err(|_| {
        ToolError::execution_failed(format!(
            "{label} process failed or exceeded its output bound"
        ))
    })?;
    if output.stopped {
        return Err(ToolError::cancelled("GitHub operation cancelled"));
    }
    if !output.output.status.success() {
        return Err(ToolError::execution_failed(format!(
            "{label} process failed; private command arguments and diagnostics were withheld"
        )));
    }
    String::from_utf8(output.output.stdout)
        .map_err(|_| ToolError::execution_failed(format!("{label} output was not UTF-8")))
}

#[cfg(all(test, unix))]
mod host_tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn bounded_github_output_refuses_oversize_and_cancelled_children() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "head -c 1048584 /dev/zero"]);
        let error = host_output(
            &mut command,
            &[],
            &tokio_util::sync::CancellationToken::new(),
            "GitHub",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("bound"));
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let cancel = tokio_util::sync::CancellationToken::new();
        let stop = cancel.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            stop.cancel();
            done_tx.send(()).unwrap();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            host_output(&mut command, &[], &cancel, "GitHub"),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        done_rx.await.unwrap();
    }
}
