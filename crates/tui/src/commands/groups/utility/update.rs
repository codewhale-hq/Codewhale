//! `/update` — check for and install a new CodeWhale release without leaving
//! the TUI.
//!
//! The updater itself is not reimplemented here. `codewhale update` (see
//! `crates/cli/src/update.rs`) already resolves the release, downloads the
//! platform-correct asset, verifies its SHA256, and atomically replaces the
//! running binary; this command finds that binary and runs it. Duplicating any
//! of that logic would give us two updaters to keep honest.
//!
//! Two deliberate limits:
//!
//! * **Package-managed installs get instructions, not an updater run.**
//!   Overwriting a binary Homebrew, npm, or cargo owns leaves the manager's
//!   metadata describing a version that is no longer on disk, and the next
//!   upgrade silently reverts the user.
//! * **We do not relaunch.** This codebase has no self-exec/relaunch pattern,
//!   and inventing one under a TUI holding the terminal is not a small change.
//!   Telling the user to restart is the honest slice; that is also the only
//!   possible answer on Windows, where the replaced image is the one running.

use std::path::{Path, PathBuf};
use std::process::Command;

use codewhale_command_contract::handler::CommandHandler;
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_release::InstallMethod;

use crate::commands::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "update",
    aliases: &["upgrade"],
    usage: "/update [check|install]",
    description_key: "cmd_update_description",
};

pub(in crate::commands) struct UpdateCmd;

impl RegisterCommand<CommandResult> for UpdateCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Pure(update)
    }
}

/// What `/update` was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateMode {
    /// Explain what an install would do, and ask the updater whether one is
    /// available. The default: `/update` never installs without being told to.
    Check,
    /// Actually run the updater.
    Install,
}

fn parse_mode(arg: Option<&str>) -> Result<UpdateMode, String> {
    match arg
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "check" | "status" => Ok(UpdateMode::Check),
        "install" | "now" | "apply" | "yes" => Ok(UpdateMode::Install),
        other => Err(format!(
            "Unknown /update argument {other:?}. Usage: {}",
            COMMAND_INFO.usage
        )),
    }
}

/// How this install can be updated, resolved before anything runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpdaterPlan {
    /// Run this `codewhale` binary's `update` subcommand.
    Run(PathBuf),
    /// A package manager owns the binary; print its command instead.
    Managed(InstallMethod),
    /// Self-update would be correct, but no `codewhale` CLI is reachable —
    /// e.g. a bare `codewhale-tui` build with no sibling CLI.
    NoUpdater { exe: PathBuf },
}

/// The primary binary name. `codew` carries the same CLI and update subcommand.
const UPDATER_BIN_STEM: &str = "codewhale";

/// Resolve the update path without touching the process environment, so the
/// decision is testable. `exists` answers whether a candidate path is a file.
pub(super) fn resolve_updater(
    exe: Option<&Path>,
    method: InstallMethod,
    exists: &dyn Fn(&Path) -> bool,
) -> UpdaterPlan {
    if !method.supports_self_update() {
        return UpdaterPlan::Managed(method);
    }
    // No resolvable executable path is not a reason to guess at one: report it
    // as "no updater here" and let the user run the CLI themselves.
    let Some(exe) = exe else {
        return UpdaterPlan::NoUpdater {
            exe: PathBuf::from(UPDATER_BIN_STEM),
        };
    };
    if matches!(
        exe.file_stem().and_then(|stem| stem.to_str()),
        Some(UPDATER_BIN_STEM | "codew")
    ) {
        return UpdaterPlan::Run(exe.to_path_buf());
    }
    // A `codewhale-tui` build has no `update` subcommand, but the CLI that
    // does is normally installed right next to it.
    if let Some(dir) = exe.parent() {
        let extension = exe.extension().and_then(|ext| ext.to_str());
        let mut sibling = dir.join(UPDATER_BIN_STEM);
        if let Some(extension) = extension {
            sibling.set_extension(extension);
        }
        if exists(&sibling) {
            return UpdaterPlan::Run(sibling);
        }
    }
    UpdaterPlan::NoUpdater {
        exe: exe.to_path_buf(),
    }
}

/// Instructions for an install this command must not update in place.
pub(super) fn managed_install_message(method: InstallMethod) -> String {
    if method == InstallMethod::DesktopApp {
        return codewhale_release::install::DESKTOP_APP_UPDATE_HELP.to_string();
    }
    format!(
        "Codewhale was installed with {label}, which owns this binary.\n\
         {migration_help}\n\n\
         To keep this secondary {label} installation, run `{command}` in a shell.\n\
         /update will not replace a binary owned by {label}; restart Codewhale after changing installs.",
        label = method.label(),
        migration_help = codewhale_release::install::GITHUB_MIGRATION_HELP,
        command = method.update_command(),
    )
}

/// Instructions when self-update is right but no updater binary is reachable.
pub(super) fn no_updater_message(exe: &Path) -> String {
    format!(
        "No `{UPDATER_BIN_STEM}` updater was found for this install ({exe}).\n\
         The updater ships in the `{UPDATER_BIN_STEM}` CLI. Install or locate it, run \
         `{UPDATER_BIN_STEM} update` in a shell, then restart Codewhale.",
        exe = exe.display(),
    )
}

/// What an install would do, stated before it is done.
fn install_preamble(updater: &Path) -> String {
    format!(
        "`/update install` will run `{updater} update`: it resolves the latest release, \
         downloads the binary for this platform, verifies its SHA256 checksum, and atomically \
         replaces {updater}.\n\
         It does not restart Codewhale — you will need to do that yourself once it finishes. \
         The UI is paused while the updater runs.",
        updater = updater.display(),
    )
}

fn update(arg: Option<&str>) -> CommandResult {
    let mode = match parse_mode(arg) {
        Ok(mode) => mode,
        Err(message) => return CommandResult::error(message),
    };

    let exe = std::env::current_exe().ok();
    let method = match exe.as_deref() {
        Some(path) => InstallMethod::detect(path),
        None => InstallMethod::Binary,
    };

    match resolve_updater(exe.as_deref(), method, &|path: &Path| path.is_file()) {
        UpdaterPlan::Managed(method) => CommandResult::message(managed_install_message(method)),
        UpdaterPlan::NoUpdater { exe } => CommandResult::message(no_updater_message(&exe)),
        UpdaterPlan::Run(updater) => run_updater(&updater, mode),
    }
}

fn run_updater(updater: &Path, mode: UpdateMode) -> CommandResult {
    let mut command = Command::new(updater);
    command.arg("update");
    if mode == UpdateMode::Check {
        command.arg("--check");
    }

    let output = match command.output() {
        Ok(output) => output,
        Err(err) => {
            return CommandResult::error(format!(
                "Failed to run `{} update`: {err}\nRun it in a shell instead, then restart Codewhale.",
                updater.display()
            ));
        }
    };

    let transcript = updater_transcript(&output.stdout, &output.stderr);
    if !output.status.success() {
        return CommandResult::error(format!(
            "`{updater} update` failed.\n{transcript}",
            updater = updater.display()
        ));
    }

    match mode {
        UpdateMode::Check => {
            CommandResult::message(format!("{}\n\n{transcript}", install_preamble(updater)))
        }
        UpdateMode::Install => CommandResult::message(format!(
            "{transcript}\n\nRestart Codewhale to run the updated binary."
        )),
    }
}

/// Merge the updater's streams into one readable block, bounded so a pathological
/// run cannot flood the transcript.
fn updater_transcript(stdout: &[u8], stderr: &[u8]) -> String {
    const MAX_CHARS: usize = 4_000;
    let mut merged = String::new();
    for stream in [stdout, stderr] {
        let text = String::from_utf8_lossy(stream);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if !merged.is_empty() {
            merged.push('\n');
        }
        merged.push_str(text);
    }
    if merged.is_empty() {
        return "(the updater printed nothing)".to_string();
    }
    if merged.chars().count() > MAX_CHARS {
        let kept: String = merged.chars().take(MAX_CHARS).collect();
        return format!("{kept}\n… output truncated.");
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_and_explicit_modes_parse() {
        assert_eq!(parse_mode(None), Ok(UpdateMode::Check));
        assert_eq!(parse_mode(Some("  ")), Ok(UpdateMode::Check));
        assert_eq!(parse_mode(Some("Check")), Ok(UpdateMode::Check));
        assert_eq!(parse_mode(Some("install")), Ok(UpdateMode::Install));
        assert_eq!(parse_mode(Some("NOW")), Ok(UpdateMode::Install));
        assert!(parse_mode(Some("--force")).is_err());
    }

    #[test]
    fn a_package_managed_install_gets_its_managers_command_not_an_updater_run() {
        for method in [
            InstallMethod::Npm,
            InstallMethod::Homebrew,
            InstallMethod::Cargo,
            InstallMethod::Omarchy,
        ] {
            let plan = resolve_updater(
                Some(Path::new("/opt/whatever/codewhale")),
                method,
                &|_: &Path| true,
            );
            assert_eq!(plan, UpdaterPlan::Managed(method), "{method:?}");

            let message = managed_install_message(method);
            assert!(message.contains(codewhale_release::install::GITHUB_MIGRATION_HELP));
            assert!(message.contains("mktemp -d"));
            assert!(
                message.contains(method.update_command()),
                "{method:?} message must name its own update command: {message}"
            );
            assert!(
                message.contains("restart Codewhale"),
                "{method:?} message must tell the user to restart: {message}"
            );
        }
    }

    #[test]
    fn a_tui_only_build_without_a_sibling_cli_falls_back_to_instructions() {
        let exe = Path::new("/usr/local/bin/codewhale-tui");
        let plan = resolve_updater(Some(exe), InstallMethod::Binary, &|_: &Path| false);
        assert_eq!(
            plan,
            UpdaterPlan::NoUpdater {
                exe: exe.to_path_buf()
            }
        );

        let message = no_updater_message(exe);
        assert!(message.contains("codewhale update"), "{message}");
        assert!(message.contains("restart Codewhale"), "{message}");
        assert!(message.contains("codewhale-tui"), "{message}");
    }

    #[test]
    fn a_tui_build_next_to_the_cli_runs_the_sibling_updater() {
        let plan = resolve_updater(
            Some(Path::new("/usr/local/bin/codewhale-tui")),
            InstallMethod::Binary,
            &|path: &Path| path == Path::new("/usr/local/bin/codewhale"),
        );
        assert_eq!(
            plan,
            UpdaterPlan::Run(PathBuf::from("/usr/local/bin/codewhale"))
        );
    }

    #[test]
    fn a_cli_install_runs_itself_including_the_windows_extension() {
        assert_eq!(
            resolve_updater(
                Some(Path::new("/usr/local/bin/codewhale")),
                InstallMethod::Binary,
                &|_: &Path| false
            ),
            UpdaterPlan::Run(PathBuf::from("/usr/local/bin/codewhale"))
        );
        // Forward slashes so the case is meaningful on the host running the
        // test: `\` is a plain filename character to a Unix `Path`, which
        // would make this assert about nothing.
        assert_eq!(
            resolve_updater(
                Some(Path::new("C:/tools/codewhale.exe")),
                InstallMethod::Binary,
                &|_: &Path| false
            ),
            UpdaterPlan::Run(PathBuf::from("C:/tools/codewhale.exe"))
        );
    }

    #[test]
    fn standalone_codew_uses_its_own_updater_without_a_companion() {
        for exe in ["/usr/local/bin/codew", "C:/tools/codew.exe"] {
            let exe = Path::new(exe);
            assert_eq!(
                resolve_updater(Some(exe), InstallMethod::Binary, &|_| {
                    panic!("the full codew CLI must not look for another installation")
                }),
                UpdaterPlan::Run(exe.to_path_buf())
            );
        }
    }

    #[test]
    fn a_windows_tui_build_finds_the_sibling_cli_with_its_extension() {
        assert_eq!(
            resolve_updater(
                Some(Path::new("C:/tools/codewhale-tui.exe")),
                InstallMethod::Binary,
                &|path: &Path| path == Path::new("C:/tools/codewhale.exe"),
            ),
            UpdaterPlan::Run(PathBuf::from("C:/tools/codewhale.exe"))
        );
    }

    #[test]
    fn an_unresolvable_executable_reports_no_updater_instead_of_guessing() {
        assert_eq!(
            resolve_updater(None, InstallMethod::Binary, &|_: &Path| true),
            UpdaterPlan::NoUpdater {
                exe: PathBuf::from("codewhale")
            }
        );
    }

    #[test]
    fn the_check_preamble_states_what_install_would_do() {
        let preamble = install_preamble(Path::new("/usr/local/bin/codewhale"));
        assert!(
            preamble.contains("/usr/local/bin/codewhale update"),
            "{preamble}"
        );
        assert!(preamble.contains("SHA256"), "{preamble}");
        assert!(preamble.contains("does not restart"), "{preamble}");
    }

    #[test]
    fn updater_output_is_merged_and_bounded() {
        assert_eq!(updater_transcript(b" out ", b""), "out");
        assert_eq!(updater_transcript(b"out", b"err"), "out\nerr");
        assert_eq!(
            updater_transcript(b"", b"  "),
            "(the updater printed nothing)"
        );

        let flood = "x".repeat(9_000);
        let bounded = updater_transcript(flood.as_bytes(), b"");
        assert!(bounded.ends_with("… output truncated."));
        assert!(bounded.chars().count() < flood.chars().count());
    }

    #[test]
    fn handler_is_pure_and_argument_only() {
        assert!(matches!(UpdateCmd::handler(), CommandHandler::Pure(_)));
        assert_eq!(UpdateCmd::info().description_key, "cmd_update_description");
        assert_eq!(UpdateCmd::info().aliases, &["upgrade"]);
    }
}
