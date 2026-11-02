use anyhow::Context as _;
use anyhow::{Result, bail};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

pub(crate) fn normalize_config_file_path(path: PathBuf) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("config path cannot be empty");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("config path cannot contain '..' components");
    }
    if path.file_name().is_none() {
        bail!("config path must include a file name");
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for config path")?
            .join(path)
    };
    let file_name = absolute
        .file_name()
        .map(OsString::from)
        .context("config path must include a file name")?;
    let parent = absolute
        .parent()
        .context("config path must include a parent directory")?;
    let parent = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => parent.to_path_buf(),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to resolve config directory {}", parent.display())
            });
        }
    };
    let normalized = parent.join(file_name);
    reject_path_symlink(&normalized)?;
    Ok(normalized)
}

pub(crate) fn normalize_project_workspace(workspace: &Path) -> Result<PathBuf> {
    if workspace.as_os_str().is_empty() {
        bail!("project workspace path cannot be empty");
    }
    if workspace
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("project workspace path cannot contain '..' components");
    }
    let absolute = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for project workspace")?
            .join(workspace)
    };
    match absolute.canonicalize() {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(normalize_path_components(&absolute))
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to resolve project workspace {}",
                workspace.display()
            )
        }),
    }
}

pub(crate) fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

pub(crate) fn checked_path_exists(path: &Path) -> Result<bool> {
    let path = normalize_config_file_path(path.to_path_buf())?;
    path.try_exists()
        .with_context(|| format!("failed to inspect config path {}", path.display()))
}

pub(crate) fn read_checked_config_file(path: &Path) -> Result<String> {
    read_checked_toml_file(path, "config")
}

pub(crate) fn read_checked_permissions_file(path: &Path) -> Result<String> {
    read_checked_toml_file(path, "permissions")
}

pub(crate) fn read_checked_toml_file(path: &Path, label: &str) -> Result<String> {
    let path = normalize_config_file_path(path.to_path_buf())?;
    read_string_no_follow(&path)
        .with_context(|| format!("failed to read {label} at {}", path.display()))
}

/// Maximum bytes read from a config file. Configs are kilobytes; anything
/// larger is not a config file.
pub(crate) const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

pub(crate) fn reject_path_symlink(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        bail!("config path must not be a symlink: {}", path.display());
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn read_string_no_follow(path: &Path) -> std::io::Result<String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut raw = String::new();
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("config file {} exceeds the 1 MiB limit", path.display()),
        ));
    }
    Ok(raw)
}

#[cfg(not(unix))]
pub(crate) fn read_string_no_follow(path: &Path) -> std::io::Result<String> {
    let file = fs::File::open(path)?;
    let mut raw = String::new();
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("config file {} exceeds the 1 MiB limit", path.display()),
        ));
    }
    Ok(raw)
}
