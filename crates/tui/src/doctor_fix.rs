//! `codewhale doctor --fix` repair planning and application (#5552, v1).
//!
//! The doctor is a read-only diagnostic by default. `--fix` computes a
//! concrete repair plan first — pure detection, no mutation — shows it, and
//! applies it only after explicit consent (or `--yes`). Every v1 action is
//! narrowly scoped and reversible:
//!
//! - delete stale `.tmp*` files left behind by interrupted atomic writes in
//!   the Codewhale home, only when last modified more than an hour ago;
//! - tighten secret-store file permissions to `0600` on Unix when group or
//!   world bits are set (the secret store writes private files, but a file
//!   restored from backup or moved from another machine may have drifted);
//! - disable MCP entries whose structural check reports `Error` (no command
//!   and no URL, or an empty command) by writing `enabled: false` through the
//!   same atomic save path the MCP editor uses;
//! - scaffold the user-global `skills`, `tools`, and `plugins` directories
//!   when they are missing so discovery surfaces stop reporting them absent.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::McpServerDoctorStatus;
use codewhale_engine::mcp;

/// One concrete, reversible repair the doctor can apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DoctorFixAction {
    /// Delete an interrupted-atomic-write leftover. Only ever a regular file
    /// whose name starts with `.tmp` directly inside the Codewhale home and
    /// whose last modification was more than an hour ago.
    DeleteStaleTempFile {
        path: PathBuf,
    },
    /// Restrict a secret-store file to owner-only on Unix.
    #[cfg(unix)]
    TightenSecretPermissions {
        path: PathBuf,
        from_mode: u32,
    },
    /// Disable a structurally broken MCP entry by name in the given config.
    DisableMcpServer {
        config_path: PathBuf,
        server: String,
    },
    /// Create a missing user-global discovery directory.
    ScaffoldDirectory {
        path: PathBuf,
    },
    MoveInstallLeftover {
        path: PathBuf,
        bytes: u64,
    },
    AlignAlias {
        alias: PathBuf,
        primary: PathBuf,
        sha256: String,
    },
    AdoptInstall {
        dir: PathBuf,
        sha256: String,
    },
    PruneRecovery {
        stamps: Vec<String>,
        bytes: u64,
    },
    MigrateConfigApiKey {
        config_path: PathBuf,
        provider: codewhale_config::ProviderKind,
        table: String,
    },
}

/// The full repair plan for one doctor run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DoctorFixPlan {
    pub(crate) actions: Vec<DoctorFixAction>,
}

impl DoctorFixPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.actions.len()
    }

    /// One human-readable line per action, in a stable order.
    pub(crate) fn describe(&self) -> Vec<String> {
        self.actions
            .iter()
            .map(|action| match action {
                DoctorFixAction::DeleteStaleTempFile { path } => {
                    format!(
                        "delete stale temp file {}",
                        codewhale_util::utils::display_path(path)
                    )
                }
                #[cfg(unix)]
                DoctorFixAction::TightenSecretPermissions { path, from_mode } => format!(
                    "restrict {} to 0600 (currently {:o})",
                    codewhale_util::utils::display_path(path),
                    from_mode & 0o7777
                ),
                DoctorFixAction::DisableMcpServer {
                    config_path,
                    server,
                } => format!(
                    "disable broken MCP server entry '{server}' in {}",
                    codewhale_util::utils::display_path(config_path)
                ),
                DoctorFixAction::ScaffoldDirectory { path } => {
                    format!(
                        "create missing directory {}",
                        codewhale_util::utils::display_path(path)
                    )
                }
                DoctorFixAction::MoveInstallLeftover { path, bytes } => format!(
                    "move old Codewhale backup {} ({}) to the recovery folder (kept {} days)",
                    codewhale_util::utils::display_path(path),
                    codewhale_release::installs::human_bytes(*bytes),
                    codewhale_release::installs::RECOVERY_RETENTION_DAYS
                ),
                DoctorFixAction::AlignAlias { alias, primary, .. } => format!(
                    "make {} match {} (v{}); the old copy goes to the recovery folder",
                    codewhale_util::utils::display_path(alias),
                    codewhale_util::utils::display_path(primary),
                    env!("CARGO_PKG_VERSION")
                ),
                DoctorFixAction::AdoptInstall { dir, .. } => format!(
                    "record an install receipt in {} so `codewhale update` and `codewhale uninstall` own it",
                    codewhale_util::utils::display_path(dir)
                ),
                DoctorFixAction::PruneRecovery { stamps, bytes } => format!(
                    "delete {} recovery folder(s) older than {} days ({}): {}",
                    stamps.len(),
                    codewhale_release::installs::RECOVERY_RETENTION_DAYS,
                    codewhale_release::installs::human_bytes(*bytes),
                    stamps.join(", ")
                ),
                DoctorFixAction::MigrateConfigApiKey {
                    config_path, table, ..
                } => {
                    if table.is_empty() {
                        format!(
                            "move the plaintext `api_key` from {} into the secret store and remove it from the file",
                            codewhale_util::utils::display_path(config_path)
                        )
                    } else {
                        format!(
                            "move the plaintext `[providers.{table}].api_key` from {} into the secret store and remove it from the file",
                            codewhale_util::utils::display_path(config_path)
                        )
                    }
                }
            })
            .collect()
    }
}

fn is_stale_temp_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".tmp"))
        && std::fs::symlink_metadata(path).is_ok_and(|meta| {
            meta.is_file()
                && meta.modified().is_ok_and(|modified| {
                    modified
                        .elapsed()
                        .is_ok_and(|age| age > std::time::Duration::from_secs(60 * 60))
                })
        })
}

/// Stale `.tmp*` regular files directly inside the Codewhale home.
/// Recent files and files with unknown or future modification times are kept.
fn stale_temp_files() -> Vec<PathBuf> {
    let Ok(home) = codewhale_config::codewhale_home() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&home) else {
        return Vec::new();
    };
    let mut files = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_stale_temp_file(path))
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// Secret-store files whose Unix permissions are looser than 0600.
#[cfg(unix)]
fn secret_files_needing_tightening() -> Vec<(PathBuf, u32)> {
    use std::os::unix::fs::PermissionsExt;

    let Ok(paths) = codewhale_secrets::FileKeyringStore::default_paths_read_only() else {
        return Vec::new();
    };
    let candidates = std::iter::once(paths.0).chain(paths.1);
    candidates
        .filter_map(|path| {
            let metadata = std::fs::metadata(&path).ok()?;
            if !metadata.is_file() {
                return None;
            }
            let mode = metadata.permissions().mode();
            (mode & 0o077 != 0).then_some((path, mode))
        })
        .collect()
}

/// MCP entries whose structural check reports `Error`, by config file.
fn broken_mcp_entries(
    config: &codewhale_runtime::config::Config,
    workspace: &Path,
    plugins: &codewhale_plugins::PluginRegistry,
) -> Vec<(PathBuf, Vec<String>)> {
    let global_path = config.mcp_config_path();
    let project_path = mcp::workspace_mcp_config_path(workspace);
    let mut broken = Vec::new();
    for path in [global_path, project_path] {
        let Ok(cfg) = mcp::load_config_with_workspace_and_plugins(&path, workspace, plugins) else {
            continue;
        };
        let names = cfg
            .servers
            .iter()
            .filter(|(_, server)| {
                server.is_enabled()
                    && matches!(
                        crate::doctor_check_mcp_server(server),
                        McpServerDoctorStatus::Error(_)
                    )
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if !names.is_empty() {
            broken.push((path, names));
        }
    }
    broken
}

/// User-global discovery directories the product expects to exist.
fn missing_user_directories(config: &codewhale_runtime::config::Config) -> Vec<PathBuf> {
    let candidates = [
        config.skills_dir(),
        crate::default_tools_dir(),
        crate::default_plugins_dir(),
    ];
    let mut missing = candidates
        .into_iter()
        .filter(|dir| !dir.exists())
        .collect::<Vec<_>>();
    missing.sort();
    missing
}

/// Compute the repair plan. Pure: reads state, mutates nothing.
#[cfg(test)]
pub(crate) fn plan_fixes(
    config: &codewhale_runtime::config::Config,
    workspace: &Path,
    plugins: &codewhale_plugins::PluginRegistry,
) -> DoctorFixPlan {
    plan_fixes_for_config(config, workspace, None, plugins)
}

pub(crate) fn plan_fixes_for_config(
    config: &codewhale_runtime::config::Config,
    workspace: &Path,
    config_path_override: Option<&Path>,
    plugins: &codewhale_plugins::PluginRegistry,
) -> DoctorFixPlan {
    let mut actions = Vec::new();
    actions.extend(
        stale_temp_files()
            .into_iter()
            .map(|path| DoctorFixAction::DeleteStaleTempFile { path }),
    );
    #[cfg(unix)]
    actions.extend(
        secret_files_needing_tightening()
            .into_iter()
            .map(|(path, from_mode)| DoctorFixAction::TightenSecretPermissions { path, from_mode }),
    );
    for (config_path, servers) in broken_mcp_entries(config, workspace, plugins) {
        actions.extend(
            servers
                .into_iter()
                .map(|server| DoctorFixAction::DisableMcpServer {
                    config_path: config_path.clone(),
                    server,
                }),
        );
    }
    actions.extend(
        missing_user_directories(config)
            .into_iter()
            .map(|path| DoctorFixAction::ScaffoldDirectory { path }),
    );
    actions.extend(install_actions());
    actions.extend(config_api_key_migrations(config, config_path_override));
    DoctorFixPlan { actions }
}

fn literal_api_key_value(value: Option<&toml::Value>) -> Option<String> {
    value
        .and_then(toml::Value::as_str)
        .filter(|key| {
            codewhale_config::classify_config_api_key_value(key)
                == codewhale_config::ConfigApiKeyValueKind::Literal
        })
        .map(str::to_string)
}

fn read_toml_doc(config_path: &Path) -> Option<toml::Value> {
    let raw = std::fs::read_to_string(config_path).ok()?;
    toml::from_str(&raw).ok()
}

fn file_api_key(config_path: &Path, table: &str) -> Option<String> {
    let doc = read_toml_doc(config_path)?;
    if table.is_empty() {
        return literal_api_key_value(doc.get("api_key"));
    }
    literal_api_key_value(
        doc.get("providers")
            .and_then(|providers| providers.get(table))
            .and_then(|entry| entry.get("api_key")),
    )
}

fn entry_disallows_api_key_migration(
    entry: Option<&toml::Value>,
    disk_config: &codewhale_runtime::config::Config,
    identity: &codewhale_runtime::config::ProviderIdentity,
) -> bool {
    if disk_config.should_skip_secret_store_for_provider(identity) {
        return true;
    }
    if let Some(table) = entry.and_then(toml::Value::as_table) {
        if table.contains_key("api_key_env")
            || table.contains_key("oauth_credential_generation")
            || table
                .get("external_credentials")
                .and_then(toml::Value::as_bool)
                == Some(true)
        {
            return true;
        }
        if table
            .get("auth_mode")
            .and_then(toml::Value::as_str)
            .is_some_and(|mode| !mode.trim().eq_ignore_ascii_case("api_key"))
        {
            return true;
        }
    }
    if disk_config
        .auth_mode_for_provider(identity)
        .as_deref()
        .is_some_and(|mode| !mode.trim().eq_ignore_ascii_case("api_key"))
    {
        return true;
    }
    disk_config
        .provider_config_for(identity)
        .is_some_and(|cfg| {
            cfg.api_key_env.is_some()
                || cfg.external_credentials.is_some()
                || cfg.oauth_credential_generation.is_some()
        })
}

fn config_api_key_migrations(
    config: &codewhale_runtime::config::Config,
    config_path_override: Option<&Path>,
) -> Vec<DoctorFixAction> {
    let config_path = match config_path_override {
        Some(path) => path.to_path_buf(),
        None => match config.loaded_config_path.as_ref() {
            Some(path) => path.clone(),
            None => match codewhale_runtime::config::credential_config_path() {
                Ok(path) => path,
                Err(_) => return Vec::new(),
            },
        },
    };
    let Some(doc) = read_toml_doc(&config_path) else {
        return Vec::new();
    };
    let Ok(disk_config) = codewhale_runtime::config::Config::load_with(
        Some(config_path.clone()),
        None,
        codewhale_runtime::config::ConfigEnvironmentPolicy::Runtime,
        &mut |_| Ok(()),
    ) else {
        return Vec::new();
    };

    let conflicted_tables: std::collections::HashSet<&str> = disk_config
        .legacy_root
        .notes
        .iter()
        .filter_map(|note| match note {
            codewhale_config::legacy_root::LegacyRootNote::Conflict {
                scope: None,
                field: codewhale_config::legacy_root::LegacyRootField::ApiKey,
                table,
                ..
            } => Some(table.strip_prefix("providers.").unwrap_or(table.as_str())),
            _ => None,
        })
        .collect();

    struct Candidate {
        slot: &'static str,
        provider: codewhale_config::ProviderKind,
        table: String,
        key: String,
    }

    let mut candidates: Vec<Candidate> = Vec::new();

    if let Some(root_key) = literal_api_key_value(doc.get("api_key"))
        && conflicted_tables.is_empty()
    {
        let root_owner = disk_config
            .legacy_root
            .notes
            .iter()
            .find_map(|note| match note {
                codewhale_config::legacy_root::LegacyRootNote::Moved {
                    scope: None,
                    field: codewhale_config::legacy_root::LegacyRootField::ApiKey,
                    to,
                }
                | codewhale_config::legacy_root::LegacyRootNote::Merged {
                    scope: None,
                    field: codewhale_config::legacy_root::LegacyRootField::ApiKey,
                    to,
                } => Some(to.strip_prefix("providers.").unwrap_or(to.as_str())),
                _ => None,
            })
            .unwrap_or("deepseek");
        if let Ok(identity) = disk_config.resolve_provider_identity(root_owner)
            && !matches!(
                identity.provider,
                codewhale_config::ProviderKind::Custom
                    | codewhale_config::ProviderKind::OpenaiCodex
                    | codewhale_config::ProviderKind::Antigravity
            )
            && !entry_disallows_api_key_migration(None, &disk_config, &identity)
        {
            candidates.push(Candidate {
                slot: codewhale_runtime::config::provider_secret_store_slot(identity.provider),
                provider: identity.provider,
                table: String::new(),
                key: root_key,
            });
        }
    }

    if let Some(providers_table) = doc.get("providers").and_then(toml::Value::as_table) {
        for (raw_table, entry_val) in providers_table {
            let Some(literal) = literal_api_key_value(entry_val.get("api_key")) else {
                continue;
            };
            let Ok(identity) = disk_config.resolve_provider_identity(raw_table) else {
                continue;
            };
            if matches!(
                identity.provider,
                codewhale_config::ProviderKind::Custom
                    | codewhale_config::ProviderKind::OpenaiCodex
                    | codewhale_config::ProviderKind::Antigravity
            ) {
                continue;
            }
            let canonical_table = identity
                .config_table_key()
                .ok()
                .unwrap_or(raw_table.as_str());
            if conflicted_tables.contains(raw_table.as_str())
                || conflicted_tables.contains(canonical_table)
            {
                continue;
            }
            if entry_disallows_api_key_migration(Some(entry_val), &disk_config, &identity) {
                continue;
            }
            candidates.push(Candidate {
                slot: codewhale_runtime::config::provider_secret_store_slot(identity.provider),
                provider: identity.provider,
                table: raw_table.clone(),
                key: literal,
            });
        }
    }

    let mut slot_keys: std::collections::HashMap<&'static str, Option<String>> =
        std::collections::HashMap::new();
    for candidate in &candidates {
        slot_keys
            .entry(candidate.slot)
            .and_modify(|existing| {
                if existing.as_deref() != Some(candidate.key.as_str()) {
                    *existing = None;
                }
            })
            .or_insert_with(|| Some(candidate.key.clone()));
    }

    candidates
        .into_iter()
        .filter(|candidate| slot_keys.get(candidate.slot).is_some_and(Option::is_some))
        .map(|candidate| DoctorFixAction::MigrateConfigApiKey {
            config_path: config_path.clone(),
            provider: candidate.provider,
            table: candidate.table,
        })
        .collect()
}

fn migrate_config_api_key(
    _config: &codewhale_runtime::config::Config,
    config_path: &Path,
    provider: codewhale_config::ProviderKind,
    table: &str,
) -> Result<()> {
    let Some(api_key) = file_api_key(config_path, table) else {
        return Ok(());
    };
    let Some(secrets) = codewhale_runtime::config::credential_secret_store() else {
        anyhow::bail!(
            "no durable secret store is available, so the key stays in {}; check `codewhale doctor --all` under Secret Backend",
            codewhale_util::utils::display_path(config_path)
        );
    };
    let normalized = codewhale_secrets::normalize_api_key(&api_key);
    let api_key = normalized.as_str();
    anyhow::ensure!(!api_key.is_empty(), "refusing to migrate an empty API key");
    let slot = codewhale_runtime::config::provider_secret_store_slot(provider);
    let disk_config = codewhale_runtime::config::Config::load_with(
        Some(config_path.to_path_buf()),
        None,
        codewhale_runtime::config::ConfigEnvironmentPolicy::Runtime,
        &mut |_| Ok(()),
    )?;
    let identity = disk_config
        .resolve_provider_identity(if table.is_empty() {
            provider.as_str()
        } else {
            table
        })
        .map_err(anyhow::Error::msg)?;
    if disk_config.should_skip_secret_store_for_provider(&identity) {
        anyhow::bail!(
            "secret-store migration is skipped for {}; keeping plaintext in {}",
            identity.key,
            codewhale_util::utils::display_path(config_path)
        );
    }
    let active_provider = disk_config
        .active_provider_identity()
        .ok()
        .map(|id| id.provider);

    codewhale_runtime::credentials::store::with_provider_write_lock(slot, || {
        let prior = secrets
            .get(slot)
            .map_err(|err| anyhow::anyhow!("failed to read secret-store slot `{slot}`: {err}"))?;
        if let Some(existing) = prior.as_deref()
            && existing != api_key
        {
            anyhow::bail!(
                "secret-store slot `{slot}` already holds a different key for a sibling route; keeping plaintext in {}",
                codewhale_util::utils::display_path(config_path)
            );
        }
        let wrote_secret = prior.as_deref() != Some(api_key);
        if wrote_secret {
            secrets.set(slot, api_key).map_err(|err| {
                anyhow::anyhow!("failed to write secret-store slot `{slot}`: {err}")
            })?;
        }
        let dest_table = identity
            .config_table_key()
            .unwrap_or(provider.as_str())
            .to_string();
        let doc_result =
            codewhale_runtime::config_persistence::mutate_config_document(config_path, |doc| {
                if table.is_empty() {
                    if active_provider == Some(provider) {
                        codewhale_runtime::config_persistence::set_document_value(
                            doc,
                            &["auth_mode"],
                            "api_key",
                        )?;
                    }
                    codewhale_runtime::config_persistence::unset_document_value(doc, &["api_key"])?;
                    if doc
                        .get("providers")
                        .and_then(|item| item.as_table())
                        .and_then(|providers| providers.get(&dest_table))
                        .and_then(|entry| entry.as_table())
                        .and_then(|entry| entry.get("api_key"))
                        .and_then(|item| item.as_str())
                        .is_some_and(|val| codewhale_secrets::normalize_api_key(val) == api_key)
                    {
                        codewhale_runtime::config_persistence::unset_document_value(
                            doc,
                            &["providers", &dest_table, "api_key"],
                        )?;
                    }
                } else {
                    codewhale_runtime::config_persistence::set_document_value(
                        doc,
                        &["providers", table, "auth_mode"],
                        "api_key",
                    )?;
                    codewhale_runtime::config_persistence::unset_document_value(
                        doc,
                        &["providers", table, "api_key"],
                    )?;
                }
                Ok(())
            });
        if let Err(error) = doc_result {
            if wrote_secret {
                let _ = match prior.as_deref() {
                    Some(previous) => secrets.set(slot, previous),
                    None => secrets.delete(slot),
                };
            }
            return Err(error);
        }
        codewhale_config::scrub_plaintext_api_keys_from_config_backup(config_path)?;
        Ok(())
    })
}

fn install_actions() -> Vec<DoctorFixAction> {
    use codewhale_release::installs::{self, InstallClass};
    let running = std::env::current_exe().ok();
    let inventory = installs::inventory(running.as_deref());
    let mut actions: Vec<DoctorFixAction> = inventory
        .leftovers()
        .map(|entry| DoctorFixAction::MoveInstallLeftover {
            path: entry.path.clone(),
            bytes: entry.bytes,
        })
        .collect();
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(exe) = &running
        && codewhale_release::InstallMethod::detect(exe).supports_self_update()
        && let Some(dir) = exe.parent().map(canonical)
        && std::fs::symlink_metadata(dir.join(installs::RECEIPT_FILE)).is_err()
        && let Some(entry) = inventory
            .entries
            .iter()
            .find(|entry| canonical(&entry.path) == canonical(exe))
        && entry.class == InstallClass::OursUnreceipted
        && let Ok(sha256) = installs::file_sha256(exe)
    {
        for alias in inventory.entries.iter().filter(|other| {
            other.class == InstallClass::OursUnreceipted
                && !other.same_as_running
                && other.path.parent().map(canonical).as_deref() == Some(dir.as_path())
                && installs::RECEIPT_COMMANDS
                    .iter()
                    .any(|name| other.path.file_name().and_then(|n| n.to_str()) == Some(name))
        }) {
            actions.push(DoctorFixAction::AlignAlias {
                alias: alias.path.clone(),
                primary: exe.clone(),
                sha256: sha256.clone(),
            });
        }
        if installs::directory_is_private(&dir) {
            actions.push(DoctorFixAction::AdoptInstall { dir, sha256 });
        }
    }
    if let Ok(home) = codewhale_config::codewhale_home() {
        let now = codewhale_release::check::now_unix();
        let expired: Vec<_> = installs::recovery_folders(&home)
            .into_iter()
            .filter(|folder| folder.expires_in_days(now) == Some(0))
            .collect();
        if !expired.is_empty() {
            actions.push(DoctorFixAction::PruneRecovery {
                bytes: expired.iter().map(|folder| folder.bytes()).sum(),
                stamps: expired.into_iter().map(|folder| folder.stamp).collect(),
            });
        }
    }
    actions
}

/// Outcome of applying one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DoctorFixOutcome {
    Applied,
    Failed(String),
}

/// Apply a plan. Each action reports its own outcome; one failure never
/// blocks the rest. Actions are idempotent, so a re-run converges.
pub(crate) fn apply_fixes(
    plan: &DoctorFixPlan,
    config: &codewhale_runtime::config::Config,
) -> Vec<(DoctorFixAction, DoctorFixOutcome)> {
    let mut batch = None;
    let results = plan
        .actions
        .iter()
        .cloned()
        .map(|action| {
            let outcome = match apply_install_action(&action, &mut batch) {
                Some(Ok(())) => DoctorFixOutcome::Applied,
                Some(Err(error)) => DoctorFixOutcome::Failed(format!("{error:#}")),
                None => match &action {
                    DoctorFixAction::MigrateConfigApiKey {
                        config_path,
                        provider,
                        table,
                    } => match migrate_config_api_key(config, config_path, *provider, table) {
                        Ok(()) => DoctorFixOutcome::Applied,
                        Err(error) => DoctorFixOutcome::Failed(format!("{error:#}")),
                    },
                    _ => apply_one(&action),
                },
            };
            (action, outcome)
        })
        .collect();
    if let Some(batch) = batch {
        let batch: codewhale_release::installs::RecoveryBatch = batch;
        let stamp = batch.stamp.clone();
        let dir = batch.dir.clone();
        let moves = batch.finish();
        if !moves.is_empty() {
            println!(
                "Moved {} item(s) ({}) to {}. Undo all: codewhale doctor --restore {stamp}",
                moves.len(),
                codewhale_release::installs::human_bytes(moves.iter().map(|m| m.bytes).sum()),
                dir.display()
            );
        }
    }
    results
}

fn recovery_batch(
    batch: &mut Option<codewhale_release::installs::RecoveryBatch>,
) -> Result<&mut codewhale_release::installs::RecoveryBatch> {
    if batch.is_none() {
        let home = codewhale_config::codewhale_home()?;
        *batch = Some(codewhale_release::installs::RecoveryBatch::open(&home)?);
    }
    Ok(batch.as_mut().expect("recovery batch was just opened"))
}

fn apply_install_action(
    action: &DoctorFixAction,
    batch: &mut Option<codewhale_release::installs::RecoveryBatch>,
) -> Option<Result<()>> {
    use codewhale_release::installs;
    Some(match action {
        DoctorFixAction::MoveInstallLeftover { path, .. } => (|| {
            let meta = std::fs::symlink_metadata(path)?;
            anyhow::ensure!(!meta.is_symlink(), "it became a symlink; not moved");
            let moved = recovery_batch(batch)?.move_in(path)?;
            println!(
                "  moved {} -> {}\n    undo: {}",
                path.display(),
                moved.to.display(),
                moved.undo_command()
            );
            Ok(())
        })(),
        DoctorFixAction::AlignAlias {
            alias,
            primary,
            sha256,
        } => (|| {
            let bytes = std::fs::read(primary)?;
            anyhow::ensure!(
                &installs::sha256_hex(&bytes) == sha256,
                "{} changed since the plan; not aligned",
                primary.display()
            );
            let dir = alias.parent().context("alias has no parent")?;
            let name = alias
                .file_name()
                .and_then(|name| name.to_str())
                .context("alias name is not UTF-8")?;
            let moved = recovery_batch(batch)?.move_in(alias)?;
            if let Err(error) =
                codewhale_release::installs::write_file_atomically(dir, name, &bytes, true)
            {
                let _ = std::fs::rename(&moved.to, alias);
                return Err(error);
            }
            println!(
                "  replaced {} with v{}; old copy at {}\n    undo: rm {} && {}",
                alias.display(),
                env!("CARGO_PKG_VERSION"),
                moved.to.display(),
                alias.display(),
                moved.undo_command()
            );
            Ok(())
        })(),
        DoctorFixAction::AdoptInstall { dir, sha256 } => (|| {
            let entries: Vec<(String, String)> = installs::RECEIPT_COMMANDS
                .iter()
                .filter(|name| {
                    installs::file_sha256(&dir.join(name)).is_ok_and(|hash| &hash == sha256)
                })
                .map(|name| ((*name).to_string(), sha256.clone()))
                .collect();
            if installs::adopt_receipt(dir, &entries)? {
                println!(
                    "  recorded {}\n    undo: rm {}",
                    dir.join(installs::RECEIPT_FILE).display(),
                    dir.join(installs::RECEIPT_FILE).display()
                );
            }
            Ok(())
        })(),
        DoctorFixAction::PruneRecovery { .. } => (|| {
            let home = codewhale_config::codewhale_home()?;
            for (stamp, bytes) in installs::prune_expired(&home) {
                println!(
                    "  deleted expired recovery folder {stamp} ({})",
                    installs::human_bytes(bytes)
                );
            }
            Ok(())
        })(),
        _ => return None,
    })
}

fn apply_one(action: &DoctorFixAction) -> DoctorFixOutcome {
    match action {
        DoctorFixAction::DeleteStaleTempFile { path } => {
            // Re-verify the deletion guard at apply time: the plan was
            // computed before consent, so the file may have changed.
            if !is_stale_temp_file(path) {
                return DoctorFixOutcome::Applied;
            }
            match std::fs::remove_file(path) {
                Ok(()) => DoctorFixOutcome::Applied,
                Err(error) => DoctorFixOutcome::Failed(error.to_string()),
            }
        }
        #[cfg(unix)]
        DoctorFixAction::TightenSecretPermissions { path, .. } => {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => DoctorFixOutcome::Applied,
                Err(error) => DoctorFixOutcome::Failed(error.to_string()),
            }
        }
        DoctorFixAction::DisableMcpServer {
            config_path,
            server,
        } => match disable_mcp_server(config_path, server) {
            Ok(()) => DoctorFixOutcome::Applied,
            Err(error) => DoctorFixOutcome::Failed(format!("{error:#}")),
        },
        DoctorFixAction::ScaffoldDirectory { path } => match std::fs::create_dir_all(path) {
            Ok(()) => DoctorFixOutcome::Applied,
            Err(error) => DoctorFixOutcome::Failed(error.to_string()),
        },
        DoctorFixAction::MoveInstallLeftover { .. }
        | DoctorFixAction::AlignAlias { .. }
        | DoctorFixAction::AdoptInstall { .. }
        | DoctorFixAction::PruneRecovery { .. }
        | DoctorFixAction::MigrateConfigApiKey { .. } => DoctorFixOutcome::Applied,
    }
}

/// Disable one server entry in an MCP config file through the atomic save
/// path the MCP editor uses. A missing file or missing server is a no-op
/// (the plan may be stale after consent).
fn disable_mcp_server(config_path: &Path, server_name: &str) -> Result<()> {
    mcp::mutate_config(config_path, None, |cfg| {
        if let Some(server) = cfg.servers.get_mut(server_name) {
            server.enabled = false;
            server.disabled = true;
        }
        Ok(())
    })
    .map(|_| ())
}

/// Print the repair plan the way the human doctor report presents it.
pub(crate) fn print_fix_plan(plan: &DoctorFixPlan) {
    use colored::Colorize;

    let (sky_r, sky_g, sky_b) = codewhale_palette::WHALE_ACTION_RGB;
    println!("{}", "Repair plan (--fix):".bold());
    if plan.is_empty() {
        println!("  {} nothing to repair", "✓".truecolor(sky_r, sky_g, sky_b));
        return;
    }
    for line in plan.describe() {
        println!("  · {line}");
    }
    println!(
        "  {} pass --yes to apply without prompting",
        "!".truecolor(sky_r, sky_g, sky_b)
    );
}

/// Ask for consent on stdin. Only used by the human (non-JSON) doctor path.
pub(crate) fn confirm_fix(plan: &DoctorFixPlan) -> bool {
    use std::io::{BufRead, Write};

    println!();
    print!("Apply these {} repair(s) now? [Y/n] ", plan.len());
    let mut answer = String::new();
    let _ = std::io::stdout().flush();
    match std::io::stdin().lock().read_line(&mut answer) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "" | "y" | "yes"
        ),
    }
}

/// Print apply results and return whether every action succeeded.
pub(crate) fn print_apply_results(results: &[(DoctorFixAction, DoctorFixOutcome)]) -> bool {
    use colored::Colorize;

    let (aqua_r, aqua_g, aqua_b) = codewhale_palette::WHALE_ACTION_RGB;
    let (red_r, red_g, red_b) = codewhale_palette::WHALE_ERROR_RGB;
    println!("{}", "Repair results:".bold());
    let mut all_applied = true;
    for (action, outcome) in results {
        match outcome {
            DoctorFixOutcome::Applied => println!(
                "  {} {}",
                "✓".truecolor(aqua_r, aqua_g, aqua_b),
                plan_action_line(action)
            ),
            DoctorFixOutcome::Failed(error) => {
                all_applied = false;
                println!(
                    "  {} {} — {error}",
                    "✗".truecolor(red_r, red_g, red_b),
                    plan_action_line(action)
                );
            }
        }
    }
    all_applied
}

fn plan_action_line(action: &DoctorFixAction) -> String {
    DoctorFixPlan {
        actions: vec![action.clone()],
    }
    .describe()
    .pop()
    .unwrap_or_else(|| "repair".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scoped `CODEWHALE_HOME` override under the process-wide env barrier.
    /// `EnvVarGuard` restores the prior value even on panic.
    struct ScratchHome {
        dir: tempfile::TempDir,
        _env: crate::test_support::EnvVarGuard,
        _lock: crate::test_support::TestEnvLock,
    }

    impl ScratchHome {
        fn new() -> (Self, codewhale_runtime::config::Config) {
            let lock = crate::test_support::lock_test_env();
            let dir = tempfile::tempdir().expect("home tempdir");
            let env = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
            (
                Self {
                    dir,
                    _env: env,
                    _lock: lock,
                },
                codewhale_runtime::config::Config::default(),
            )
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    fn adv_plan_and_apply(
        home: &ScratchHome,
        body: &str,
    ) -> (
        Vec<String>,
        Vec<(DoctorFixAction, DoctorFixOutcome)>,
        String,
    ) {
        let config_path = home.path().join("config.toml");
        std::fs::write(&config_path, body).expect("write config");
        let config = codewhale_runtime::config::Config::load(Some(config_path.clone()), None)
            .expect("load config");
        let workspace = tempfile::tempdir().expect("workspace");
        let registry = codewhale_plugins::PluginRegistry::default();
        let plan = DoctorFixPlan {
            actions: plan_fixes(&config, workspace.path(), &registry)
                .actions
                .into_iter()
                .filter(|action| matches!(action, DoctorFixAction::MigrateConfigApiKey { .. }))
                .collect(),
        };
        let described = plan.describe();
        let results = apply_fixes(&plan, &config);
        let raw = std::fs::read_to_string(&config_path).expect("read config");
        (described, results, raw)
    }

    #[test]
    fn adv_xai_oauth_route_is_not_flipped_to_api_key() {
        let (home, _) = ScratchHome::new();
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let body = "provider = \"xai\"\n[providers.xai]\nauth_mode = \"oauth\"\napi_key = \"xai-advreviewleftover0123456789\"\n";
        let (plan, _results, raw) = adv_plan_and_apply(&home, body);
        assert!(
            plan.is_empty(),
            "oauth provider must not be migrated: {plan:?}"
        );
        assert!(
            raw.contains("auth_mode = \"oauth\""),
            "xAI OAuth route flipped: {raw}"
        );
    }

    #[test]
    fn adv_anthropic_oauth_route_is_not_flipped_to_api_key() {
        let (home, _) = ScratchHome::new();
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let body = "provider = \"anthropic\"\n[providers.anthropic]\nauth_mode = \"oauth\"\noauth_credential_generation = \"gen-1\"\napi_key = \"sk-ant-advreviewleftover0123456789\"\n";
        let (plan, _results, raw) = adv_plan_and_apply(&home, body);
        assert!(
            plan.is_empty(),
            "oauth provider must not be migrated: {plan:?}"
        );
        assert!(
            raw.contains("auth_mode = \"oauth\""),
            "Anthropic OAuth route flipped: {raw}"
        );
    }

    #[test]
    fn adv_shared_slot_keys_are_not_lost() {
        let (home, _) = ScratchHome::new();
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let global = "sk-advglobalsiliconflow0123456789";
        let china = "sk-advchinasiliconflow0123456789";
        let body = format!(
            "provider = \"siliconflow\"\n[providers.siliconflow]\napi_key = \"{global}\"\n[providers.siliconflow_cn]\napi_key = \"{china}\"\n"
        );
        let (plan, _results, raw) = adv_plan_and_apply(&home, &body);
        let stored = codewhale_runtime::config::credential_secret_store()
            .and_then(|store| store.get("siliconflow").ok().flatten());
        let survives = |key: &str| raw.contains(key) || stored.as_deref() == Some(key);
        assert!(
            plan.is_empty(),
            "conflicting shared slot must not migrate: {plan:?}"
        );
        assert!(
            survives(global) && survives(china),
            "a key was lost: stored={stored:?} raw={raw}"
        );
    }

    #[test]
    fn adv_deepseek_root_key_side_effects() {
        let (home, _) = ScratchHome::new();
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let body = "provider = \"zai\"\napi_key = \"sk-advdeepseekroot0123456789abcd\"\n[providers.zai]\nmodel = \"glm-5\"\n";
        let (plan, results, raw) = adv_plan_and_apply(&home, body);
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(results[0].1, DoctorFixOutcome::Applied, "{results:?}");
        assert!(!raw.contains("sk-advdeepseekroot"), "{raw}");
        assert!(!raw.contains("auth_mode = \"api_key\""), "{raw}");
        assert!(!raw.contains("reasoning_effort = \"auto\""), "{raw}");
    }

    #[test]
    fn fix_honors_explicit_config_path_and_skips_legacy_root_conflict() {
        let (home, _) = ScratchHome::new();
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let default_path = home.path().join("config.toml");
        std::fs::write(
            &default_path,
            "[providers.openai]\napi_key = \"sk-defaultfile0123456789abcdef\"\n",
        )
        .expect("write default");

        let custom_dir = tempfile::tempdir().expect("custom dir");
        let custom_path = custom_dir.path().join("other.toml");
        std::fs::write(
            &custom_path,
            "[providers.stepfun]\napi_key = \"sk-otherfile0123456789abcdef\"\n",
        )
        .expect("write custom");

        let config = codewhale_runtime::config::Config::load(Some(custom_path.clone()), None)
            .expect("load custom");
        let workspace = tempfile::tempdir().expect("workspace");
        let registry = codewhale_plugins::PluginRegistry::default();
        let plan = DoctorFixPlan {
            actions: plan_fixes_for_config(
                &config,
                workspace.path(),
                Some(&custom_path),
                &registry,
            )
            .actions
            .into_iter()
            .filter(|action| matches!(action, DoctorFixAction::MigrateConfigApiKey { .. }))
            .collect(),
        };
        assert_eq!(plan.len(), 1, "{plan:?}");
        let results = apply_fixes(&plan, &config);
        assert_eq!(results[0].1, DoctorFixOutcome::Applied, "{results:?}");
        assert!(
            std::fs::read_to_string(&default_path)
                .expect("default")
                .contains("sk-defaultfile0123456789abcdef")
        );
        assert!(
            !std::fs::read_to_string(&custom_path)
                .expect("custom")
                .contains("sk-otherfile0123456789abcdef")
        );

        let conflict_body = "api_key = \"sk-rootconflict0123456789ab\"\n[providers.deepseek]\napi_key = \"sk-tableconflict0123456789a\"\n";
        let (conflict_plan, _, conflict_raw) = adv_plan_and_apply(&home, conflict_body);
        assert!(conflict_plan.is_empty(), "{conflict_plan:?}");
        assert!(conflict_raw.contains("sk-rootconflict"));
        assert!(conflict_raw.contains("sk-tableconflict"));
    }

    #[test]
    fn fix_moves_a_plaintext_provider_key_into_the_secret_store() {
        let (home, config) = ScratchHome::new();
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let config_path = home.path().join("config.toml");
        let key = "sk-doctorfixmigration0123456789";
        std::fs::write(
            &config_path,
            format!("[providers.stepfun]\nmodel = \"step-3\"\napi_key = \"{key}\"\n"),
        )
        .expect("write config");
        let workspace = tempfile::tempdir().expect("workspace");
        let registry = codewhale_plugins::PluginRegistry::default();
        let plan = DoctorFixPlan {
            actions: plan_fixes(&config, workspace.path(), &registry)
                .actions
                .into_iter()
                .filter(|action| matches!(action, DoctorFixAction::MigrateConfigApiKey { .. }))
                .collect(),
        };
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert!(!plan.describe()[0].contains(key));

        let results = apply_fixes(&plan, &config);

        assert_eq!(results[0].1, DoctorFixOutcome::Applied, "{results:?}");
        let raw = std::fs::read_to_string(&config_path).expect("read config");
        assert!(!raw.contains(key), "{raw}");
        assert!(raw.contains("step-3"), "{raw}");
        assert!(
            plan_fixes(&config, workspace.path(), &registry)
                .actions
                .iter()
                .all(|action| !matches!(action, DoctorFixAction::MigrateConfigApiKey { .. }))
        );
    }

    #[test]
    fn stale_temp_files_only_names_regular_dot_tmp_files_in_home() {
        let (home, config) = ScratchHome::new();
        std::fs::write(home.path().join(".tmpAbC123"), b"orphaned").expect("write stale");
        std::fs::write(home.path().join(".tmpOther"), b"orphaned 2").expect("write stale 2");
        let now = std::time::SystemTime::now();
        for name in [".tmpAbC123", ".tmpOther"] {
            std::fs::File::options()
                .write(true)
                .open(home.path().join(name))
                .unwrap()
                .set_modified(now - std::time::Duration::from_secs(2 * 60 * 60))
                .unwrap();
        }
        for (name, modified) in [
            (".tmpLive", now),
            (".tmpRecent", now - std::time::Duration::from_secs(30 * 60)),
            (".tmpFuture", now + std::time::Duration::from_secs(60 * 60)),
        ] {
            let file = std::fs::File::create(home.path().join(name)).expect("temp file");
            file.set_modified(modified).expect("mtime");
        }
        std::fs::write(home.path().join("keep.txt"), b"keep").expect("write keep");
        std::fs::create_dir_all(home.path().join(".tmpDir")).expect("mkdir .tmpDir");

        let workspace = tempfile::tempdir().expect("workspace");
        let plan = plan_fixes(
            &config,
            workspace.path(),
            &codewhale_plugins::PluginRegistry::default(),
        );
        let temp_deletions = plan
            .actions
            .iter()
            .filter(|a| matches!(a, DoctorFixAction::DeleteStaleTempFile { .. }))
            .count();
        assert_eq!(temp_deletions, 2, "{:?}", plan.actions);

        // A stale candidate may become active while waiting for consent.
        std::fs::File::options()
            .write(true)
            .open(home.path().join(".tmpOther"))
            .unwrap()
            .set_modified(now)
            .unwrap();
        let results = apply_fixes(
            &DoctorFixPlan {
                actions: plan
                    .actions
                    .iter()
                    .filter(|a| matches!(a, DoctorFixAction::DeleteStaleTempFile { .. }))
                    .cloned()
                    .collect(),
            },
            &config,
        );
        assert!(results.iter().all(|(_, o)| *o == DoctorFixOutcome::Applied));
        assert!(!home.path().join(".tmpAbC123").exists());
        for name in [
            ".tmpOther",
            ".tmpLive",
            ".tmpRecent",
            ".tmpFuture",
            ".tmpDir",
        ] {
            assert!(home.path().join(name).exists(), "must preserve {name}");
        }
        assert!(home.path().join("keep.txt").exists());
    }

    #[test]
    fn missing_user_directories_are_planned_and_scaffolding_is_idempotent() {
        let (home, config) = ScratchHome::new();
        let workspace = tempfile::tempdir().expect("workspace");
        let registry = codewhale_plugins::PluginRegistry::default();
        let plan = plan_fixes(&config, workspace.path(), &registry);
        assert!(
            plan.actions
                .iter()
                .any(|a| matches!(a, DoctorFixAction::ScaffoldDirectory { .. })),
            "scratch home plans scaffolding: {:?}",
            plan.actions
        );
        let results = apply_fixes(&plan, &config);
        assert!(
            results.iter().all(|(_, o)| *o == DoctorFixOutcome::Applied),
            "{results:?}"
        );
        assert!(home.path().join("skills").exists());
        assert!(home.path().join("tools").exists());
        assert!(home.path().join("plugins").exists());
        let replan = plan_fixes(&config, workspace.path(), &registry);
        assert!(
            !replan
                .actions
                .iter()
                .any(|a| matches!(a, DoctorFixAction::ScaffoldDirectory { .. })),
            "scaffolding converges"
        );
    }

    #[cfg(unix)]
    #[test]
    fn loose_secret_file_permissions_are_planned_and_tightened() {
        use std::os::unix::fs::PermissionsExt;

        let (home, config) = ScratchHome::new();
        let secrets_dir = home.path().join("secrets");
        std::fs::create_dir_all(&secrets_dir).expect("secrets dir");
        let secrets_file = secrets_dir.join("secrets.json");
        std::fs::write(&secrets_file, b"{}").expect("secrets file");
        std::fs::set_permissions(&secrets_file, std::fs::Permissions::from_mode(0o644))
            .expect("loosen");

        let workspace = tempfile::tempdir().expect("workspace");
        let registry = codewhale_plugins::PluginRegistry::default();
        let plan = plan_fixes(&config, workspace.path(), &registry);
        let tighten = plan
            .actions
            .iter()
            .find(|a| matches!(a, DoctorFixAction::TightenSecretPermissions { .. }))
            .expect("loose secret file is planned");
        let results = apply_fixes(
            &DoctorFixPlan {
                actions: vec![tighten.clone()],
            },
            &config,
        );
        assert_eq!(results[0].1, DoctorFixOutcome::Applied);
        let mode = std::fs::metadata(&secrets_file)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn fix_flags_parse_and_conflict_with_json_modes() {
        use clap::Parser;

        let cli = crate::Cli::try_parse_from(["codewhale", "doctor", "--fix", "--yes"])
            .expect("--fix --yes parses");
        let Some(crate::Commands::Doctor(args)) = cli.command else {
            panic!("expected doctor command");
        };
        assert!(args.fix);
        assert!(args.yes);

        crate::Cli::try_parse_from(["codewhale", "doctor", "--fix", "--json"])
            .expect_err("--fix conflicts with --json");
        crate::Cli::try_parse_from(["codewhale", "doctor", "--fix", "--context-json"])
            .expect_err("--fix conflicts with --context-json");
        crate::Cli::try_parse_from(["codewhale", "doctor", "--yes"])
            .expect_err("--yes requires --fix");
    }

    #[test]
    fn broken_mcp_entry_is_disabled_through_the_atomic_save_path() {
        let (home, config) = ScratchHome::new();
        let workspace = tempfile::tempdir().expect("workspace");
        let mcp_path = home.path().join("mcp.json");
        std::fs::write(
            &mcp_path,
            serde_json::json!({
                "mcpServers": {
                    "broken": { "command": "", "args": [] },
                    "healthy": { "command": "node", "args": ["server.js"] }
                }
            })
            .to_string(),
        )
        .expect("mcp config");

        let mut config = config;
        config.mcp_config_path = Some(mcp_path.display().to_string());

        let registry = codewhale_plugins::PluginRegistry::default();
        let plan = plan_fixes(&config, workspace.path(), &registry);
        let disable = plan
            .actions
            .iter()
            .find_map(|a| match a {
                DoctorFixAction::DisableMcpServer {
                    config_path,
                    server,
                } if server == "broken" => Some(config_path.clone()),
                _ => None,
            })
            .expect("broken entry is planned");
        assert_eq!(disable, mcp_path);

        let results = apply_fixes(
            &DoctorFixPlan {
                actions: plan
                    .actions
                    .iter()
                    .filter(|a| matches!(a, DoctorFixAction::DisableMcpServer { .. }))
                    .cloned()
                    .collect(),
            },
            &config,
        );
        assert!(
            results.iter().all(|(_, o)| *o == DoctorFixOutcome::Applied),
            "{results:?}"
        );
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).expect("reread"))
                .expect("json");
        assert!(
            raw.get("servers").is_none(),
            "preserve the original mcpServers spelling"
        );
        assert_eq!(raw["mcpServers"]["broken"]["enabled"], false);
        assert_eq!(raw["mcpServers"]["broken"]["disabled"], true);
        assert_eq!(
            raw["mcpServers"]["healthy"],
            serde_json::json!({"command":"node", "args":["server.js"]}),
            "healthy entry remains unchanged"
        );
        assert!(mcp::load_config(&mcp_path).unwrap().servers["healthy"].enabled);
    }
}
