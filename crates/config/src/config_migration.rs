use crate::path_io::reject_path_symlink;
use crate::persistence;
use crate::provider_toml::CONFIG_FILE_NAME;
use crate::state_dirs::{codewhale_home, codewhale_home_is_explicit, legacy_deepseek_home};
use anyhow::Context as _;
use anyhow::Result;
use std::path::PathBuf;

pub fn default_config_path() -> Result<PathBuf> {
    // Prefer ~/.codewhale/config.toml when it exists (fresh install or
    // migrated), otherwise fall back to ~/.deepseek/config.toml.
    let primary = codewhale_home()?.join(CONFIG_FILE_NAME);
    if codewhale_home_is_explicit() || primary.exists() {
        return Ok(primary);
    }
    let legacy = legacy_deepseek_home()?.join(CONFIG_FILE_NAME);
    if legacy.exists() {
        return Ok(legacy);
    }
    // Neither exists — return primary so first write creates it there.
    Ok(primary)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMigration {
    pub legacy_path: PathBuf,
    pub primary_path: PathBuf,
}

impl ConfigMigration {
    pub fn user_notice(&self) -> String {
        format!(
            "Migrated legacy config from {} to {}. Use the .codewhale path for future edits; the .deepseek file remains only as a compatibility fallback.",
            self.legacy_path.display(),
            self.primary_path.display()
        )
    }
}

/// v0.8.44: one-time migration from `~/.deepseek/config.toml` to
/// `~/.codewhale/config.toml`. Called on first launch after the config
/// is loaded; copies the legacy file if the primary doesn't exist yet.
/// Never overwrites an existing primary config.
pub fn migrate_config_if_needed() -> Result<Option<ConfigMigration>> {
    if codewhale_home_is_explicit() {
        return Ok(None);
    }
    let primary = codewhale_home()?.join(CONFIG_FILE_NAME);
    // `exists()` follows links, so a dangling link would read as "absent" and
    // the copy below would create whatever it points at. A link at the primary
    // name is refused instead, dangling or not.
    reject_path_symlink(&primary)?;
    if primary.exists() {
        return Ok(None);
    }
    let legacy = legacy_deepseek_home()?.join(CONFIG_FILE_NAME);
    if !legacy.exists() {
        return Ok(None);
    }
    // Copy the config to the new home, owner-only, through a temporary file
    // renamed into place (the primary name is replaced, never written through).
    let contents = std::fs::read(&legacy).context("failed to read legacy deepseek config")?;
    persistence::atomic_write(&primary, &contents)
        .context("failed to migrate config from deepseek to codewhale home")?;
    tracing::info!(
        "Migrated config from {} to {}",
        legacy.display(),
        primary.display()
    );
    Ok(Some(ConfigMigration {
        legacy_path: legacy,
        primary_path: primary,
    }))
}
