use crate::hooks::authority::{
    MCP_FILE, ReviewedProjectFile, approve_project_file, approved_project_file, review_project_file,
};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectMcpStatus {
    Absent,
    Untrusted,
    Unreadable(String),
    Unapproved { digest: String },
    Approved { digest: String },
}

pub fn review_project_mcp(workspace: &Path) -> Result<ReviewedProjectFile, String> {
    review_project_file(workspace, MCP_FILE)
}

pub fn approved_project_mcp(workspace: &Path) -> Result<ReviewedProjectFile, String> {
    approved_project_file(workspace, MCP_FILE)
}

pub fn approve_project_mcp(workspace: &Path, reviewed_digest: &str) -> Result<(), String> {
    approve_project_file(workspace, MCP_FILE, reviewed_digest, |contents| {
        let workspace =
            super::checked_workspace_path(workspace).map_err(|error| error.to_string())?;
        let config: codewhale_mcp::config::McpConfig = serde_json::from_str(contents)
            .map_err(|_| "Invalid MCP JSON; correct it before approval".to_string())?;
        for server in config.servers.values() {
            if server.command.is_some() && server.url.is_none() {
                super::resolve_project_mcp_cwd(&workspace, server.cwd.as_deref())
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    })
}

pub fn revoke_project_mcp(workspace: &Path) -> Result<(), String> {
    codewhale_runtime::config::save_workspace_project_receipt(
        workspace,
        codewhale_runtime::config::MCP_RECEIPT_KEY,
        "",
    )
    .map(|_| ())
    .map_err(|_| "Could not revoke project MCP approval".to_string())
}

#[must_use]
pub fn project_mcp_status(workspace: &Path) -> ProjectMcpStatus {
    let Ok(workspace) = super::checked_workspace_path(workspace) else {
        return ProjectMcpStatus::Absent;
    };
    let path = workspace.join(".codewhale").join("mcp.json");
    if path
        .symlink_metadata()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return ProjectMcpStatus::Absent;
    }
    if !codewhale_runtime::config::is_workspace_trusted(&workspace) {
        return ProjectMcpStatus::Untrusted;
    }
    match review_project_mcp(&workspace) {
        Err(error) => ProjectMcpStatus::Unreadable(error),
        Ok(reviewed) => {
            let approved = codewhale_runtime::config::project_receipt_for_workspace(
                &reviewed.workspace,
                codewhale_runtime::config::MCP_RECEIPT_KEY,
            )
            .as_deref()
                == Some(&reviewed.digest);
            if approved {
                ProjectMcpStatus::Approved {
                    digest: reviewed.digest,
                }
            } else {
                ProjectMcpStatus::Unapproved {
                    digest: reviewed.digest,
                }
            }
        }
    }
}

pub(super) fn carry_approval_across_product_edit(
    path: &Path,
    original: Option<&str>,
    written: &str,
) {
    let Some(original) = original else { return };
    if path.file_name() != Some(std::ffi::OsStr::new("mcp.json")) {
        return;
    }
    let Some(dir) = path.parent() else { return };
    if dir.file_name() != Some(std::ffi::OsStr::new(".codewhale")) {
        return;
    }
    let Some(workspace) = dir.parent().map(super::normalize_workspace_path) else {
        return;
    };
    let digest = |text: &str| -> String {
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    let receipt = codewhale_runtime::config::project_receipt_for_workspace(
        &workspace,
        codewhale_runtime::config::MCP_RECEIPT_KEY,
    );
    if receipt.as_deref() != Some(digest(original).as_str())
        || !codewhale_runtime::config::is_workspace_trusted(&workspace)
    {
        return;
    }
    if codewhale_runtime::config::save_workspace_project_receipt(
        &workspace,
        codewhale_runtime::config::MCP_RECEIPT_KEY,
        &digest(written),
    )
    .is_err()
    {
        tracing::warn!(target: "mcp", "Could not carry project MCP approval across an edit");
    }
}
