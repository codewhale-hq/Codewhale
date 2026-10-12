use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use codewhale_config::ConfigStore;

#[derive(Debug, Args)]
pub(crate) struct DispatchArgs {
    /// Task for the remote agent. Required unless inspecting or cancelling a job.
    #[arg(value_name = "PROMPT")]
    prompt: Vec<String>,
    /// Inspect one account job.
    #[arg(long, value_name = "ID")]
    show: Option<String>,
    /// Cancel one account job.
    #[arg(long, value_name = "ID")]
    cancel: Option<String>,
    /// Account dispatch: Agent name or ID (default: the repository-free default Agent).
    #[arg(long)]
    agent: Option<String>,
    /// Account dispatch: task time in seconds (default: what your offer allows).
    #[arg(long)]
    seconds: Option<u64>,
    /// Account dispatch: UTF-8 source context file.
    #[arg(long, value_name = "PATH")]
    context_file: Option<PathBuf>,
    /// Account dispatch: saved account file FILE_ID@VERSION; repeatable.
    #[arg(long = "file", value_name = "FILE_ID@VERSION")]
    file_refs: Vec<String>,
    /// Account dispatch: skip the question (needs --confirm-eu-compute).
    #[arg(long)]
    yes: bool,
    /// Account dispatch: agree that repository code and Work files run on EU compute.
    #[arg(long)]
    confirm_eu_compute: bool,
    /// Account dispatch: replay key for a retry after an uncertain response.
    #[arg(long)]
    operation_key: Option<String>,
    /// Account dispatch: message ID for a retry after an uncertain response.
    #[arg(long)]
    message_id: Option<String>,
}

fn unknown_job(id: &str) -> anyhow::Error {
    anyhow::anyhow!("Unknown job {id}: it is not a Codewhale account job ID.")
}

pub(crate) fn run(
    args: DispatchArgs,
    profile: Option<&str>,
    config: &mut ConfigStore,
) -> Result<()> {
    use crate::cloud::{AccountDispatch, run_dispatch};
    let modes = [
        args.show.is_some(),
        args.cancel.is_some(),
        !args.prompt.is_empty(),
    ];
    if modes.iter().filter(|mode| **mode).count() > 1 {
        bail!("Use one of: a prompt, --show <id>, or --cancel <id>.");
    }
    if let Some(id) = args.show.as_deref() {
        if id.starts_with("cloud_") {
            return Err(unknown_job(id));
        }
        return run_dispatch(AccountDispatch::Status(id.to_string()), profile, config);
    }
    if let Some(id) = args.cancel.as_deref() {
        if id.starts_with("cloud_") {
            return Err(unknown_job(id));
        }
        return run_dispatch(AccountDispatch::Cancel(id.to_string()), profile, config);
    }
    if args.prompt.is_empty() {
        bail!("Give a task to dispatch, or use --show <id> or --cancel <id>.");
    }
    run_dispatch(
        AccountDispatch::Run {
            objective: args.prompt.join(" "),
            agent: args.agent,
            seconds: args.seconds,
            context_file: args.context_file,
            file_refs: args.file_refs,
            operation_key: args.operation_key,
            message_id: args.message_id,
            yes: args.yes,
            confirm_eu_compute: args.confirm_eu_compute,
        },
        profile,
        config,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands};
    use clap::Parser;

    fn args(argv: &[&str]) -> DispatchArgs {
        let cli = Cli::try_parse_from(argv).unwrap();
        let Some(Commands::Dispatch(args)) = cli.command else {
            panic!("expected dispatch command");
        };
        args
    }

    #[test]
    fn parses_the_obvious_dispatch_command() {
        let parsed = args(&[
            "codewhale",
            "dispatch",
            "fix",
            "the",
            "flake",
            "--seconds",
            "600",
        ]);
        assert_eq!(parsed.prompt, ["fix", "the", "flake"]);
        assert_eq!(parsed.seconds, Some(600));
        assert!(Cli::try_parse_from(["codewhale", "dispatch", "--remote", "github"]).is_err());
    }

    #[test]
    fn rendered_help_carries_no_provider_brand() {
        use clap::CommandFactory;
        let help = Cli::command()
            .find_subcommand_mut("dispatch")
            .expect("dispatch subcommand exists")
            .render_help()
            .to_string();
        assert!(help.contains("--show"), "{help}");
        assert!(!help.contains("--own-sandbox"), "{help}");
        for banned in ["Daytona", "daytona"] {
            assert!(
                !help.contains(banned),
                "--help must not brand the operator: {banned}"
            );
        }
    }
}
