//! `/import-claude` host operations (FEAT-028).
//!
//! The command owns the flow and the messages; this host module builds the plan
//! from the Claude sources, persists the rendered artifacts and carries over the
//! standing instructions. The plan is the contract model, so the command
//! renders it without any host type.

use crate::tui::app::App;
use codewhale_command_contract::config::{
    CommandClaudeApplyOutcome, CommandClaudeArtifacts, CommandClaudePlan, CommandMcpCandidate,
};
use std::path::Path;

/// Build the import plan from the host's home/workspace; `None` when nothing
/// was found.
pub(in crate::commands) fn build_plan(app: &mut App) -> Option<CommandClaudePlan> {
    let home = codewhale_paths::user_home().unwrap_or_else(|| std::path::PathBuf::from("."));
    let (sources, claude, settings) = codewhale_net::import_claude::read_sources(&home);

    // MCP candidates come from the same discovery the `/mcp import` consent
    // flow uses, filtered to the Claude sources: provenance stays single-sourced.
    let markets = Vec::new();
    let mcp_candidates =
        crate::mcp::external_import::discover_external_sources(&home, &app.workspace, &markets)
            .into_iter()
            .filter(|candidate| {
                matches!(
                    candidate.source_kind,
                    crate::mcp::external_import::ExternalMcpSourceKind::ClaudeJson
                )
            })
            .map(|candidate| CommandMcpCandidate {
                summary: candidate.summary,
                hard_blocked: candidate.hard_blocked,
                name: candidate.name,
            })
            .collect::<Vec<_>>();

    let plan =
        codewhale_net::import_claude::build_plan(sources, claude, settings, &home, mcp_candidates);
    if plan.is_empty() { None } else { Some(plan) }
}

/// Persist the rendered report and optional bundle.
pub(in crate::commands) fn write_artifacts(
    report: &str,
    bundle: Option<&str>,
) -> CommandClaudeArtifacts {
    let imports_dir = codewhale_config::codewhale_home()
        .map(|home| home.join("imports"))
        .unwrap_or_else(|_| std::path::PathBuf::from(".codewhale/imports"));
    let report_path = imports_dir.join("claude-import-report.md");
    let bundle_path = imports_dir.join("claude-portable-bundle.json");
    let wrote_report =
        codewhale_config::persistence::atomic_write(&report_path, report.as_bytes()).is_ok();
    let wrote_bundle = match bundle {
        Some(json) => {
            codewhale_config::persistence::atomic_write(&bundle_path, json.as_bytes()).is_ok()
        }
        None => false,
    };
    CommandClaudeArtifacts {
        report_path,
        bundle_path,
        wrote_report,
        wrote_bundle,
    }
}

/// Carry over the standing instructions and report what remains manual.
///
/// `--apply` is the consent: this path never overwrites an existing file, never
/// imports MCP servers, and never touches hooks or permission rules — those run
/// code, so they stay a human decision.
pub(in crate::commands) fn apply_plan(
    _app: &mut App,
    plan: &CommandClaudePlan,
    report_path: &Path,
    wrote_report: bool,
) -> CommandClaudeApplyOutcome {
    use codewhale_command_contract::config::CommandImportFollowUp;
    let mut done: Vec<CommandImportFollowUp> = Vec::new();
    let mut manual: Vec<CommandImportFollowUp> = Vec::new();

    if plan.has_claude_md {
        match codewhale_config::codewhale_home() {
            Ok(home) => {
                let destination = home.join("instructions.md");
                let source = codewhale_paths::user_home()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".claude")
                    .join("CLAUDE.md");
                if destination.exists() {
                    manual.push(CommandImportFollowUp::InstructionsKeptExisting {
                        destination: codewhale_util::utils::display_path(&destination).to_string(),
                        source: codewhale_util::utils::display_path(&source).to_string(),
                    });
                } else {
                    let copy_source = source.clone();
                    let copy_home = home.clone();
                    #[cfg(test)]
                    let ticket = crate::test_support::env_scope_ticket();
                    let copied = match tokio::runtime::Handle::try_current() {
                        Ok(handle)
                            if handle.runtime_flavor()
                                == tokio::runtime::RuntimeFlavor::MultiThread =>
                        {
                            crate::commands::contract::run_async(async move {
                                tokio::task::spawn_blocking(move || {
                                    #[cfg(test)]
                                    let _membership = crate::test_support::join_env_scope(ticket);
                                    std::fs::read(&copy_source).is_ok_and(|bytes| {
                                        codewhale_config::private_directory::PrivateDirectory::open(
                                            &copy_home,
                                        )
                                        .and_then(|directory| {
                                            directory.write_owned_file(
                                                "instructions.md",
                                                &bytes,
                                                false,
                                            )
                                        })
                                        .is_ok()
                                    })
                                })
                                .await
                                .unwrap_or(false)
                            })
                        }
                        Err(_) => std::thread::spawn(move || {
                            #[cfg(test)]
                            let _membership = crate::test_support::join_env_scope(ticket);
                            std::fs::read(&copy_source).is_ok_and(|bytes| {
                                codewhale_config::private_directory::PrivateDirectory::open(
                                    &copy_home,
                                )
                                .and_then(|directory| {
                                    directory.write_owned_file("instructions.md", &bytes, false)
                                })
                                .is_ok()
                            })
                        })
                        .join()
                        .unwrap_or(false),
                        Ok(_) => false,
                    };
                    if copied {
                        done.push(CommandImportFollowUp::InstructionsCopied {
                            destination: codewhale_util::utils::display_path(&destination)
                                .to_string(),
                        });
                    } else {
                        manual.push(CommandImportFollowUp::InstructionsCopyFailed {
                            source: codewhale_util::utils::display_path(&source).to_string(),
                            destination: codewhale_util::utils::display_path(&destination)
                                .to_string(),
                        });
                    }
                }
            }
            Err(_) => manual.push(CommandImportFollowUp::HomeUnresolved),
        }
    }

    // MCP approval must bind the exact bytes and current managed-config
    // revision shown by the shared preview. Blanket migration cannot grant it.
    for candidate in &plan.mcp_candidates {
        manual.push(if candidate.hard_blocked {
            CommandImportFollowUp::McpBlocked {
                name: candidate.name.clone(),
            }
        } else {
            CommandImportFollowUp::McpNeedsReview {
                name: candidate.name.clone(),
            }
        });
    }

    if !plan.hook_events.is_empty() {
        manual.push(CommandImportFollowUp::HookEvents {
            count: plan.hook_events.len(),
        });
    }
    if !plan.permissions_allow.is_empty()
        || !plan.permissions_ask.is_empty()
        || !plan.permissions_deny.is_empty()
    {
        manual.push(CommandImportFollowUp::PermissionRules);
    }

    CommandClaudeApplyOutcome {
        done,
        manual,
        report_path: report_path.to_path_buf(),
        wrote_report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandResult;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use std::fs;
    use tempfile::TempDir;

    /// Run the portable `/import-claude` handler through the real CONFIG
    /// envelope (the flow and messages now live in the command).
    fn import_claude_command(app: &mut App, arg: Option<&str>) -> CommandResult {
        let result = crate::commands::groups::config::config::import_claude(
            app.command_contexts_with_config(None)
                .contexts(crate::commands::groups::config::config::CONFIG_ONLY_CAPS),
            arg,
        );
        CommandResult {
            message: result.message,
            action: result
                .action
                .map(crate::commands::config_host::config_action_to_app_action),
            is_error: result.is_error,
        }
    }

    /// A home with a Claude `CLAUDE.md`, plus a Codewhale home to import into.
    fn sandbox() -> (TempDir, App) {
        let temp = TempDir::new().expect("tempdir");
        let home = temp.path().join("home");
        let claude = home.join(".claude");
        fs::create_dir_all(&claude).expect("claude dir");
        fs::write(claude.join("CLAUDE.md"), b"be excellent\n").expect("CLAUDE.md");
        fs::write(claude.join("settings.json"), b"{}").expect("settings");
        fs::write(home.join(".claude.json"), b"{}").expect("claude.json");

        let workspace = temp.path().join("ws");
        fs::create_dir_all(&workspace).expect("workspace");
        let options = TuiOptions {
            config_path: Some(temp.path().join("config.toml")),
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory.md"),
            notes_path: temp.path().join("notes.txt"),
            mcp_config_path: temp.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(&workspace)
        };
        let app = App::new(options, &Config::default());
        (temp, app)
    }

    #[test]
    fn the_bare_command_applies_nothing_and_says_how_to_apply() {
        let _lock = crate::test_support::lock_test_env();
        let (temp, mut app) = sandbox();
        let home = temp.path().join("home");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _profile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
        let cw_home = temp.path().join("cw");
        let _cw = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &cw_home);

        let result = import_claude_command(&mut app, None);
        let message = result.message.expect("a plan");
        assert!(
            message.contains("/import-claude --apply"),
            "the plan must say how to apply it: {message}"
        );
        assert!(
            !cw_home.join("instructions.md").exists(),
            "the bare command must not write instructions.md"
        );
    }

    #[test]
    fn apply_carries_the_standing_instructions_and_never_clobbers_them() {
        let _lock = crate::test_support::lock_test_env();
        let (temp, mut app) = sandbox();
        let home = temp.path().join("home");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _profile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
        let cw_home = temp.path().join("cw");
        let _cw = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &cw_home);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("import runtime");
        let result = runtime.block_on(async { import_claude_command(&mut app, Some("--apply")) });
        let message = result.message.expect("an outcome");
        let destination = cw_home.join("instructions.md");
        assert!(destination.exists(), "instructions were copied: {message}");
        assert_eq!(
            fs::read_to_string(&destination).expect("read"),
            "be excellent\n"
        );

        // Running it again must not overwrite the operator's own file.
        fs::write(&destination, b"mine now\n").expect("overwrite");
        let again = import_claude_command(&mut app, Some("--apply"));
        let message = again.message.expect("an outcome");
        assert_eq!(
            fs::read_to_string(&destination).expect("read"),
            "mine now\n",
            "an existing instructions.md is never clobbered: {message}"
        );
        assert!(
            message.contains("already exists"),
            "and the reason is said out loud: {message}"
        );
    }
}
