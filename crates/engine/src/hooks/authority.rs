//! User-owned approval of the exact project hooks file, separate from folder trust.
use codewhale_config::ProjectHookAuthority;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Read once for both the digest and parsing. Repository symlinks and special
/// files are not executable configuration, even if the workspace is trusted.
pub fn review_project_hooks(workspace: &Path) -> Result<(ProjectHookAuthority, String), String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|_| "Cannot resolve hooks workspace")?;
    let mut path = workspace.clone();
    for component in [".codewhale", "hooks.toml"] {
        path.push(component);
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| "Cannot read project hooks path")?;
        if metadata.file_type().is_symlink() {
            return Err("Project hooks path must not contain symlinks".into());
        }
        if (component == ".codewhale" && !metadata.is_dir())
            || (component == "hooks.toml" && !metadata.is_file())
        {
            return Err("Project hooks must be a regular file in .codewhale".into());
        }
    }
    let contents = super::config::read_project_hooks_file(&path)
        .map_err(|_| "Cannot read project hooks file (maximum 1 MiB)")?;
    let digest = Sha256::digest(contents.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((ProjectHookAuthority { workspace, digest }, contents))
}

pub fn approved_project_hooks(workspace: &Path) -> Result<(ProjectHookAuthority, String), String> {
    if !codewhale_runtime::config::is_workspace_trusted(workspace) {
        return Err("Project hooks require workspace trust and separate hook approval".into());
    }
    let (authority, contents) = review_project_hooks(workspace)?;
    if codewhale_runtime::config::hook_receipt_for_workspace(&authority.workspace).as_deref()
        != Some(&authority.digest)
    {
        return Err("Project hooks are unapproved or changed; use /hooks review, then /hooks approve <digest>".into());
    }
    Ok((authority, contents))
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
    if !codewhale_runtime::config::is_workspace_trusted(workspace) {
        return Err("Trust the workspace before approving its hooks".into());
    }
    let (authority, contents) = review_project_hooks(workspace)?;
    if reviewed_digest != authority.digest {
        return Err("Hooks do not match the reviewed digest; use /hooks review again".into());
    }
    let parsed = toml::from_str::<codewhale_config::hooks::HooksConfig>(&contents)
        .map_err(|_| "Invalid hooks TOML")?;
    if parsed.validate().iter().any(|problem| problem.rejected) {
        return Err("Hooks contain invalid entries; correct them before approval".into());
    }
    codewhale_runtime::config::save_workspace_hook_receipt(&authority.workspace, reviewed_digest)
        .map_err(|_| "Could not save hook approval in user config".to_string())?;
    Ok(())
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
