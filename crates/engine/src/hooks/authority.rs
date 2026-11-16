//! User-owned approval of the exact project hooks file, separate from folder trust.
use codewhale_config::ProjectHookAuthority;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Copy)]
pub(crate) struct ProjectFileKind {
    label: &'static str,
    file_name: &'static str,
    receipt_key: &'static str,
    review_hint: &'static str,
}

pub(crate) const HOOKS_FILE: ProjectFileKind = ProjectFileKind {
    label: "hooks",
    file_name: "hooks.toml",
    receipt_key: codewhale_runtime::config::HOOKS_RECEIPT_KEY,
    review_hint: "/hooks review, then /hooks approve <digest>",
};

pub(crate) const MCP_FILE: ProjectFileKind = ProjectFileKind {
    label: "MCP servers",
    file_name: "mcp.json",
    receipt_key: codewhale_runtime::config::MCP_RECEIPT_KEY,
    review_hint: "/mcp review, then /mcp approve <digest>",
};

pub struct ReviewedProjectFile {
    pub workspace: std::path::PathBuf,
    pub digest: String,
    pub contents: String,
}

/// Read once for both the digest and parsing. Repository symlinks and special
/// files are not executable configuration, even if the workspace is trusted.
pub(crate) fn review_project_file(
    workspace: &Path,
    kind: ProjectFileKind,
) -> Result<ReviewedProjectFile, String> {
    let label = kind.label;
    let workspace = workspace
        .canonicalize()
        .map_err(|_| format!("Cannot resolve {label} workspace"))?;
    let mut path = workspace.clone();
    for component in [".codewhale", kind.file_name] {
        path.push(component);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| format!("Cannot read project {label} path"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("Project {label} path must not contain symlinks"));
        }
        if (component == ".codewhale" && !metadata.is_dir())
            || (component == kind.file_name && !metadata.is_file())
        {
            return Err(format!(
                "Project {label} must be a regular file in .codewhale"
            ));
        }
    }
    let contents = super::config::read_project_hooks_file(&path)
        .map_err(|_| format!("Cannot read project {label} file (maximum 1 MiB)"))?;
    let digest = Sha256::digest(contents.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(ReviewedProjectFile {
        workspace,
        digest,
        contents,
    })
}

pub(crate) fn approved_project_file(
    workspace: &Path,
    kind: ProjectFileKind,
) -> Result<ReviewedProjectFile, String> {
    let label = kind.label;
    if !codewhale_runtime::config::is_workspace_trusted(workspace) {
        return Err(format!(
            "Project {label} require workspace trust and separate approval"
        ));
    }
    let reviewed = review_project_file(workspace, kind)?;
    if codewhale_runtime::config::project_receipt_for_workspace(
        &reviewed.workspace,
        kind.receipt_key,
    )
    .as_deref()
        != Some(&reviewed.digest)
    {
        return Err(format!(
            "Project {label} are unapproved or changed; use {}",
            kind.review_hint
        ));
    }
    Ok(reviewed)
}

pub(crate) fn approve_project_file(
    workspace: &Path,
    kind: ProjectFileKind,
    reviewed_digest: &str,
    validate: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let label = kind.label;
    if !codewhale_runtime::config::is_workspace_trusted(workspace) {
        return Err(format!("Trust the workspace before approving its {label}"));
    }
    let reviewed = review_project_file(workspace, kind)?;
    if reviewed_digest != reviewed.digest {
        return Err(format!(
            "The {label} file does not match the reviewed digest; review it again"
        ));
    }
    validate(&reviewed.contents)?;
    codewhale_runtime::config::save_workspace_project_receipt(
        &reviewed.workspace,
        kind.receipt_key,
        reviewed_digest,
    )
    .map_err(|_| format!("Could not save {label} approval in user config"))?;
    Ok(())
}

pub fn review_project_hooks(workspace: &Path) -> Result<(ProjectHookAuthority, String), String> {
    let reviewed = review_project_file(workspace, HOOKS_FILE)?;
    Ok((
        ProjectHookAuthority {
            workspace: reviewed.workspace,
            digest: reviewed.digest,
        },
        reviewed.contents,
    ))
}

pub fn approved_project_hooks(workspace: &Path) -> Result<(ProjectHookAuthority, String), String> {
    let reviewed = approved_project_file(workspace, HOOKS_FILE)?;
    Ok((
        ProjectHookAuthority {
            workspace: reviewed.workspace,
            digest: reviewed.digest,
        },
        reviewed.contents,
    ))
}

pub fn verify_hook_authorities(
    plugin: Option<&codewhale_config::PluginAuthority>,
    project: Option<&ProjectHookAuthority>,
) -> Result<(), String> {
    if let Some(authority) = plugin {
        codewhale_plugins::registry::verify_plugin_component_authority(
            authority,
            codewhale_plugins::activation::PluginActivationCapability::Hooks,
        )?;
    }
    if let Some(authority) = project {
        let (current, _) = approved_project_hooks(&authority.workspace)?;
        if &current != authority {
            return Err("Project hooks changed after loading; review and approve again".into());
        }
    }
    Ok(())
}

pub fn approve_project_hooks(workspace: &Path, reviewed_digest: &str) -> Result<(), String> {
    approve_project_file(workspace, HOOKS_FILE, reviewed_digest, |contents| {
        let parsed = toml::from_str::<codewhale_config::hooks::HooksConfig>(contents)
            .map_err(|_| "Invalid hooks TOML")?;
        if parsed.validate().iter().any(|problem| problem.rejected) {
            return Err("Hooks contain invalid entries; correct them before approval".into());
        }
        Ok(())
    })
}

/// The Native floor is sufficient for Native-authored shell contributions; it is not a legacy Hooks grant.
pub fn verify_hook(hook: &codewhale_config::hooks::Hook) -> Result<(), String> {
    if hook.native_shell.is_some() {
        let authority = hook
            .plugin_authority
            .as_ref()
            .ok_or("Native hook has no authority")?;
        codewhale_plugins::registry::verify_plugin_component_authority(
            authority,
            codewhale_plugins::activation::PluginActivationCapability::Native,
        )?;
        Ok(())
    } else {
        verify_hook_authorities(
            hook.plugin_authority.as_ref(),
            hook.project_authority.as_ref(),
        )
    }
}
