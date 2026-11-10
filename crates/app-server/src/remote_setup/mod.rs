//! `codewhale remote-setup` — guided generation of a remote-agent deploy bundle.
//!
//! Generate-only MVP: the wizard collects a cloud target, a chat bridge, and a
//! model provider, then renders a deploy bundle (env files, systemd units,
//! RUNBOOK) to `--out`. Cloud auto-provisioning is not implemented: the
//! hidden `--apply` flag fails with a non-zero exit before anything is
//! prompted, written, or executed.
//!
//! Design mirrors the table-driven provider registry in
//! `crates/config/src/lib.rs`: the wizard iterates [`registry::CLOUD_TARGETS`],
//! [`registry::BRIDGES`], and the existing `codewhale_config::provider` registry
//! rather than hard-coding the matrix.

pub mod bundle;
pub mod registry;

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;

use bundle::{BundleInputs, DEFAULT_PORT, DEFAULT_WORKERS, ProviderInfo, write_bundle};
use registry::{BRIDGES, BridgeSpec, CLOUD_TARGETS, CloudTarget};

/// Flags for `codewhale remote-setup` (clap), per the RFC command surface.
#[derive(Args, Debug, Clone, Default)]
pub struct RemoteSetupArgs {
    /// Cloud target slug (lighthouse, azure, digitalocean). Skips the prompt.
    #[arg(long)]
    pub cloud: Option<String>,
    /// Chat bridge slug (feishu, telegram). Skips the prompt.
    #[arg(long)]
    pub bridge: Option<String>,
    /// Provider slug; validated against the provider registry. Skips the prompt.
    #[arg(long)]
    pub provider: Option<String>,
    /// Bundle output directory (default `./codewhale-deploy/<cloud>-<bridge>`).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Emit the bundle, do not provision (default).
    #[arg(long, default_value_t = false)]
    pub generate_only: bool,
    /// Reserved for cloud auto-provisioning, which is not implemented.
    /// Hidden from `--help`; passing it makes `remote-setup` fail.
    #[arg(
        long,
        default_value_t = false,
        conflicts_with = "generate_only",
        hide = true
    )]
    pub apply: bool,
    /// Skip the final confirmation gate (CI / non-interactive).
    #[arg(long, default_value_t = false)]
    pub yes: bool,
    /// Fail instead of prompting if any required value is missing.
    #[arg(long, default_value_t = false)]
    pub non_interactive: bool,
}

/// Entry point invoked by the TUI command dispatcher.
pub fn run_remote_setup(args: RemoteSetupArgs) -> Result<()> {
    if args.apply {
        bail!("{APPLY_NOT_IMPLEMENTED}");
    }

    print_header();

    let cloud = resolve_cloud(&args)?;
    let bridge = resolve_bridge(&args)?;
    let provider = resolve_provider(&args)?;

    println!();
    println!("Plan:");
    println!("  cloud    : {} ({})", cloud.display, cloud.slug);
    println!("  bridge   : {} ({})", bridge.display, bridge.slug);
    println!(
        "  provider : {} ({}) — key var {}",
        provider.display, provider.slug, provider.key_var
    );
    println!("  hint     : {}", bridge.setup_hint);

    // Generate the shared runtime token with the codebase's established CSPRNG
    // pattern (uuid v4, as in acp_server.rs) — never Math.random / time-based.
    let runtime_token = generate_runtime_token();

    let inputs = BundleInputs {
        cloud,
        bridge,
        provider: provider.clone(),
        model: "auto".to_string(),
        runtime_token,
        provider_key_value: format!("replace-with-{}-key", provider.slug),
        bridge_secret_values: bridge
            .secret_keys
            .iter()
            .map(|k| {
                (
                    (*k).to_string(),
                    format!("replace-with-{}", k.to_ascii_lowercase()),
                )
            })
            .collect(),
        allowlist: String::new(),
        port: DEFAULT_PORT,
        workers: DEFAULT_WORKERS,
        workspace: "/opt/whalebro".to_string(),
    };

    let out_dir = args.out.clone().unwrap_or_else(|| {
        PathBuf::from("codewhale-deploy").join(format!("{}-{}", cloud.slug, bridge.slug))
    });

    let written = write_bundle(&inputs, &out_dir)?;
    println!();
    println!("Generated bundle in {}:", out_dir.display());
    for path in &written {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!("  - {name}");
    }

    println!();
    println!(
        "Next: open {}/RUNBOOK.md and follow the steps.",
        out_dir.display()
    );

    Ok(())
}

/// Error for `--apply`: provisioning is not implemented, so the command must
/// fail rather than exit 0 as though something was provisioned.
const APPLY_NOT_IMPLEMENTED: &str = "remote-setup --apply is not implemented: Codewhale does \
not provision cloud resources. Run `codewhale remote-setup` without --apply to generate the \
deploy bundle, then follow its RUNBOOK.md.";

fn print_header() {
    use codewhale_palette as palette;
    use colored::Colorize;
    let (r, g, b) = palette::WHALE_ACTION_RGB;
    println!("{}", "Codewhale Remote Setup".truecolor(r, g, b).bold());
    println!("{}", "======================".truecolor(r, g, b));
    println!("Generate a deploy bundle for a remote Codewhale agent (cloud + chat bridge).");
}

// ---------------------------------------------------------------------------
// Resolution: flag -> prompt (unless --non-interactive) -> validated value
// ---------------------------------------------------------------------------

fn resolve_cloud(args: &RemoteSetupArgs) -> Result<&'static CloudTarget> {
    if let Some(slug) = &args.cloud {
        return registry::cloud_by_slug(slug)
            .ok_or_else(|| anyhow::anyhow!("unknown cloud '{slug}'. {}", cloud_choices()));
    }
    if args.non_interactive {
        bail!(
            "--cloud is required in --non-interactive mode. {}",
            cloud_choices()
        );
    }
    let idx = prompt_choice(
        "Cloud target",
        &CLOUD_TARGETS
            .iter()
            .map(|c| format!("{} ({})", c.display, c.slug))
            .collect::<Vec<_>>(),
    )?;
    Ok(&CLOUD_TARGETS[idx])
}

fn resolve_bridge(args: &RemoteSetupArgs) -> Result<&'static BridgeSpec> {
    if let Some(slug) = &args.bridge {
        return registry::bridge_by_slug(slug)
            .ok_or_else(|| anyhow::anyhow!("unknown bridge '{slug}'. {}", bridge_choices()));
    }
    if args.non_interactive {
        bail!(
            "--bridge is required in --non-interactive mode. {}",
            bridge_choices()
        );
    }
    let idx = prompt_choice(
        "Chat bridge",
        &BRIDGES
            .iter()
            .map(|b| format!("{} ({})", b.display, b.slug))
            .collect::<Vec<_>>(),
    )?;
    Ok(&BRIDGES[idx])
}

fn resolve_provider(args: &RemoteSetupArgs) -> Result<ProviderInfo> {
    if let Some(slug) = &args.provider {
        return ProviderInfo::from_slug(slug).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown provider '{slug}'. Known: {}",
                codewhale_config::ProviderKind::names_hint()
            )
        });
    }
    if args.non_interactive {
        bail!(
            "--provider is required in --non-interactive mode. Known: {}",
            codewhale_config::ProviderKind::names_hint()
        );
    }
    // List providers by their canonical names from the existing registry.
    let providers: Vec<ProviderInfo> = codewhale_config::ProviderKind::all()
        .iter()
        .filter_map(|kind| ProviderInfo::from_slug(kind.as_str()))
        .collect();
    let labels: Vec<String> = providers
        .iter()
        .map(|p| format!("{} ({})", p.display, p.slug))
        .collect();
    let idx = prompt_choice("Model provider", &labels)?;
    Ok(providers[idx].clone())
}

fn cloud_choices() -> String {
    format!(
        "Choices: {}",
        CLOUD_TARGETS
            .iter()
            .map(|c| c.slug)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn bridge_choices() -> String {
    format!(
        "Choices: {}",
        BRIDGES
            .iter()
            .map(|b| b.slug)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

// ---------------------------------------------------------------------------
// Prompt helpers (reuse the stdin pattern from main.rs `pick_session_id`)
// ---------------------------------------------------------------------------

/// Print a numbered menu, read a 1-based selection from stdin, return the index.
fn prompt_choice(title: &str, options: &[String]) -> Result<usize> {
    println!();
    println!("{title}:");
    for (idx, opt) in options.iter().enumerate() {
        println!("  {:>2}. {}", idx + 1, opt);
    }
    print!("Enter a number: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if input.is_empty() {
        bail!("No selection made.");
    }
    let n: usize = input
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid input: {input}"))?;
    options
        .get(n.saturating_sub(1))
        .map(|_| n - 1)
        .ok_or_else(|| anyhow::anyhow!("Selection out of range"))
}

/// Generate a runtime token from two random v4 UUIDs (OS CSPRNG via uuid),
/// matching the existing token-generation pattern in this crate.
fn generate_runtime_token() -> String {
    let a = uuid::Uuid::new_v4().simple().to_string();
    let b = uuid::Uuid::new_v4().simple().to_string();
    format!("{a}{b}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_is_long_and_hex() {
        let t = generate_runtime_token();
        assert_eq!(t.len(), 64, "two simple uuids = 64 hex chars");
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // Two successive tokens differ (random, not fixed).
        assert_ne!(t, generate_runtime_token());
    }

    #[test]
    fn unknown_flags_fail_with_choices() {
        let args = RemoteSetupArgs {
            cloud: Some("nope".to_string()),
            non_interactive: true,
            ..Default::default()
        };
        let err = resolve_cloud(&args).unwrap_err().to_string();
        assert!(err.contains("unknown cloud"));
        assert!(err.contains("digitalocean"));

        let args = RemoteSetupArgs {
            bridge: Some("nope".to_string()),
            non_interactive: true,
            ..Default::default()
        };
        let err = resolve_bridge(&args).unwrap_err().to_string();
        assert!(err.contains("unknown bridge"));

        let args = RemoteSetupArgs {
            provider: Some("nope".to_string()),
            non_interactive: true,
            ..Default::default()
        };
        let err = resolve_provider(&args).unwrap_err().to_string();
        assert!(err.contains("unknown provider"));
    }

    #[test]
    fn non_interactive_requires_flags() {
        let args = RemoteSetupArgs {
            non_interactive: true,
            ..Default::default()
        };
        assert!(
            resolve_cloud(&args)
                .unwrap_err()
                .to_string()
                .contains("--cloud is required")
        );
    }

    #[test]
    fn flags_resolve_to_registry_rows() {
        let args = RemoteSetupArgs {
            cloud: Some("digitalocean".to_string()),
            bridge: Some("telegram".to_string()),
            provider: Some("deepseek".to_string()),
            non_interactive: true,
            ..Default::default()
        };
        assert_eq!(resolve_cloud(&args).unwrap().slug, "digitalocean");
        assert_eq!(resolve_bridge(&args).unwrap().slug, "telegram");
        assert_eq!(resolve_provider(&args).unwrap().slug, "deepseek");
    }

    #[test]
    fn apply_fails_before_writing_a_bundle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("bundle");
        let args = RemoteSetupArgs {
            cloud: Some("digitalocean".to_string()),
            bridge: Some("telegram".to_string()),
            provider: Some("deepseek".to_string()),
            out: Some(out.clone()),
            apply: true,
            yes: true,
            non_interactive: true,
            ..Default::default()
        };
        let err = run_remote_setup(args).unwrap_err().to_string();
        assert!(err.contains("--apply is not implemented"), "{err}");
        assert!(!out.exists(), "--apply must not render a bundle");
    }
}
