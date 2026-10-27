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
//!
//! Explicitly out of scope for v1: completion registration, launch-record
//! repair, and config.toml credential scrubbing (each needs its own consent
//! story; the doctor still *reports* the credential-shaped keys).

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::McpServerDoctorStatus;
use crate::mcp;

/// One concrete, reversible repair the doctor can apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DoctorFixAction {
    /// Delete an interrupted-atomic-write leftover. Only ever a regular file
    /// whose name starts with `.tmp` directly inside the Codewhale home and
    /// whose last modification was more than an hour ago.
    DeleteStaleTempFile { path: PathBuf },
    /// Restrict a secret-store file to owner-only on Unix.
    #[cfg(unix)]
    TightenSecretPermissions { path: PathBuf, from_mode: u32 },
    /// Disable a structurally broken MCP entry by name in the given config.
    DisableMcpServer {
        config_path: PathBuf,
        server: String,
    },
    /// Create a missing user-global discovery directory.
    ScaffoldDirectory { path: PathBuf },
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
    plugins: &crate::plugins::PluginRegistry,
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
pub(crate) fn plan_fixes(
    config: &codewhale_runtime::config::Config,
    workspace: &Path,
    plugins: &crate::plugins::PluginRegistry,
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
    DoctorFixPlan { actions }
}

/// Outcome of applying one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DoctorFixOutcome {
    Applied,
    Failed(String),
}

/// Apply a plan. Each action reports its own outcome; one failure never
/// blocks the rest. Actions are idempotent, so a re-run converges.
pub(crate) fn apply_fixes(plan: &DoctorFixPlan) -> Vec<(DoctorFixAction, DoctorFixOutcome)> {
    plan.actions
        .iter()
        .cloned()
        .map(|action| {
            let outcome = apply_one(&action);
            (action, outcome)
        })
        .collect()
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
    println!("Apply these {} repair(s) now? [y/N] ", plan.len());
    let mut answer = String::new();
    let _ = std::io::stdout().flush();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
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
            &crate::plugins::PluginRegistry::default(),
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
        let results = apply_fixes(&DoctorFixPlan {
            actions: plan
                .actions
                .iter()
                .filter(|a| matches!(a, DoctorFixAction::DeleteStaleTempFile { .. }))
                .cloned()
                .collect(),
        });
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
        let registry = crate::plugins::PluginRegistry::default();
        let plan = plan_fixes(&config, workspace.path(), &registry);
        assert!(
            plan.actions
                .iter()
                .any(|a| matches!(a, DoctorFixAction::ScaffoldDirectory { .. })),
            "scratch home plans scaffolding: {:?}",
            plan.actions
        );
        let results = apply_fixes(&plan);
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
        let registry = crate::plugins::PluginRegistry::default();
        let plan = plan_fixes(&config, workspace.path(), &registry);
        let tighten = plan
            .actions
            .iter()
            .find(|a| matches!(a, DoctorFixAction::TightenSecretPermissions { .. }))
            .expect("loose secret file is planned");
        let results = apply_fixes(&DoctorFixPlan {
            actions: vec![tighten.clone()],
        });
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

        let registry = crate::plugins::PluginRegistry::default();
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

        let results = apply_fixes(&DoctorFixPlan {
            actions: plan
                .actions
                .iter()
                .filter(|a| matches!(a, DoctorFixAction::DisableMcpServer { .. }))
                .cloned()
                .collect(),
        });
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
