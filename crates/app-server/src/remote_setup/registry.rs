//! Table-driven registries for `codewhale remote-setup`.
//!
//! Mirrors the `ProviderKind`/`provider::Provider` registry pattern in
//! `crates/config/src/lib.rs`: adding a cloud or a bridge is one row of data,
//! not a new control-flow branch. The wizard in [`super`] iterates these tables
//! rather than hard-coding clouds/bridges, so the matrix grows by data.
//!
//! - [`BridgeSpec`] — pure transport between a chat app and the local runtime.
//! - [`CloudTarget`] — where the agent runs and where its secrets live.
//! - The provider dimension is *not* duplicated here: it reads the existing
//!   `codewhale_config::provider` registry (see [`super::bundle::ProviderInfo`]).

/// Where a cloud target stores the runtime/provider secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStore {
    /// Secrets live in `/etc/codewhale/*.env` files on the host.
    EnvFile,
    /// Secrets live in a managed vault (e.g. Azure Key Vault), read at boot.
    KeyVault,
}

impl SecretStore {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SecretStore::EnvFile => "EnvFile (/etc/codewhale/*.env)",
            SecretStore::KeyVault => "Key Vault (managed identity at boot)",
        }
    }
}

/// How the runtime + bridge are installed on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// Native `cargo install` + systemd units (mirrors deploy/tencent-lighthouse).
    NativeSystemd,
    /// Container image pulled and run under systemd / a container runtime.
    Docker,
}

impl InstallMethod {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            InstallMethod::NativeSystemd => "native + systemd",
            InstallMethod::Docker => "Docker image",
        }
    }
}

/// A single provisioning step expressed as **data**, never a shell string.
///
/// Commands are returned as `(program, args)` so the confirmation gate can print
/// every command before running anything, secrets are fed via stdin/temp files
/// (never argv or shell history — `secret_args` lists arg indexes to redact when
/// printing), and `--apply` simply executes the already-printed plan. In the
/// generate-only MVP these steps are only *rendered into the RUNBOOK*; nothing
/// is executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionStep {
    /// Human-readable description shown in the plan / RUNBOOK.
    pub description: String,
    /// Program to run (e.g. `az`, `doctl`).
    pub program: String,
    /// Arguments, in order.
    pub args: Vec<String>,
    /// Indexes into `args` whose values are secret and must be redacted when
    /// the plan is printed. (Empty for the data-only RUNBOOK rows here.)
    pub secret_args: Vec<usize>,
}

impl ProvisionStep {
    pub fn new(description: impl Into<String>, program: impl Into<String>, args: &[&str]) -> Self {
        Self {
            description: description.into(),
            program: program.into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            secret_args: Vec::new(),
        }
    }

    /// Render the command for display, redacting any secret arg positions.
    #[must_use]
    pub fn display_command(&self) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(self.program.clone());
        for (idx, arg) in self.args.iter().enumerate() {
            if self.secret_args.contains(&idx) {
                parts.push("<redacted>".to_string());
            } else {
                parts.push(arg.clone());
            }
        }
        parts.join(" ")
    }
}

/// Inputs collected by the wizard that a cloud `plan()` reads.
///
/// Deliberately minimal and side-effect-free: a `plan()` turns these into an
/// ordered list of [`ProvisionStep`]s. Secret *values* are never placed here;
/// the plan references where they will be read from (env file / vault), so this
/// struct stays safe to print and to construct in tests.
#[derive(Debug, Clone)]
pub struct DeployInputs {
    /// Bridge slug, e.g. `"telegram"`.
    pub bridge_slug: String,
    /// Provider slug, e.g. `"deepseek"`.
    pub provider_slug: String,
    /// Cloud region / location (default per cloud).
    pub region: String,
    /// Logical instance / resource name.
    pub instance_name: String,
    /// Container image used by Docker installs.
    pub image: String,
}

impl Default for DeployInputs {
    fn default() -> Self {
        Self {
            bridge_slug: "telegram".to_string(),
            provider_slug: "deepseek".to_string(),
            region: String::new(),
            instance_name: "codewhale-remote".to_string(),
            image: "ghcr.io/codewhale-hq/codewhale:latest".to_string(),
        }
    }
}

/// A chat bridge: pure transport between a chat app and `127.0.0.1:7878`.
#[derive(Debug, Clone, Copy)]
pub struct BridgeSpec {
    /// Stable slug used on the CLI and in paths, e.g. `"telegram"`.
    pub slug: &'static str,
    /// Human-readable label.
    pub display: &'static str,
    /// Package directory (relative to repo root), e.g. `"integrations/telegram-bridge"`.
    pub package_dir: &'static str,
    /// Systemd unit filename for the bridge.
    pub service_unit: &'static str,
    /// Repo-relative path of the reference env template shipped with deploy/.
    pub env_template: &'static str,
    /// Bridge-specific secret env keys the wizard prompts for (token(s), etc.).
    pub secret_keys: &'static [&'static str],
    /// One-liner shown before prompting (where to get the bridge credentials).
    pub setup_hint: &'static str,
    /// systemd `WorkingDirectory` the unit expects the bridge to be installed at.
    pub install_dir: &'static str,
}

/// A cloud target: where the agent runs and where secrets live.
#[derive(Debug, Clone, Copy)]
pub struct CloudTarget {
    /// Stable slug used on the CLI and in paths, e.g. `"azure"`.
    pub slug: &'static str,
    /// Human-readable label.
    pub display: &'static str,
    /// Where runtime/provider secrets are stored.
    pub secret_store: SecretStore,
    /// How the runtime + bridge are installed.
    pub install: InstallMethod,
    /// Default region/location for this cloud.
    pub default_region: &'static str,
    /// Cloud CLI used by the (stubbed) auto-provision path, e.g. `"az"`.
    pub cli_tool: &'static str,
    /// Builds the ordered provisioning plan as data. In the generate-only MVP
    /// this is only rendered into the RUNBOOK; `--apply` is not implemented.
    pub plan: fn(&DeployInputs) -> Vec<ProvisionStep>,
}

// ---------------------------------------------------------------------------
// Bridge registry
// ---------------------------------------------------------------------------

/// Telegram bridge — long-poll transport, secret is the BotFather token.
pub const TELEGRAM: BridgeSpec = BridgeSpec {
    slug: "telegram",
    display: "Telegram",
    package_dir: "integrations/telegram-bridge",
    service_unit: "codewhale-telegram-bridge.service",
    env_template: "deploy/tencent-lighthouse/examples/telegram-bridge.env.example",
    secret_keys: &["TELEGRAM_BOT_TOKEN"],
    setup_hint: "Create a bot with @BotFather in Telegram and copy the HTTP API token.",
    install_dir: "/opt/codewhale/telegram-bridge",
};

/// Feishu/Lark bridge — app id + secret are the bridge credentials.
pub const FEISHU: BridgeSpec = BridgeSpec {
    slug: "feishu",
    display: "Feishu/Lark",
    package_dir: "integrations/feishu-bridge",
    service_unit: "codewhale-feishu-bridge.service",
    env_template: "deploy/tencent-lighthouse/examples/feishu-bridge.env.example",
    secret_keys: &["FEISHU_APP_ID", "FEISHU_APP_SECRET"],
    setup_hint: "Create a custom app in the Feishu/Lark Open Platform; copy its App ID and App Secret.",
    install_dir: "/opt/codewhale/bridge",
};

/// All registered bridges. Adding a bridge is one row here.
pub const BRIDGES: &[BridgeSpec] = &[FEISHU, TELEGRAM];

/// Look up a bridge by slug.
#[must_use]
pub fn bridge_by_slug(slug: &str) -> Option<&'static BridgeSpec> {
    BRIDGES.iter().find(|b| b.slug.eq_ignore_ascii_case(slug))
}

// ---------------------------------------------------------------------------
// Cloud registry
// ---------------------------------------------------------------------------

/// Tencent Lighthouse — native systemd, env-file secrets, CNB-driven deploy.
pub const LIGHTHOUSE: CloudTarget = CloudTarget {
    slug: "lighthouse",
    display: "Tencent Lighthouse",
    secret_store: SecretStore::EnvFile,
    install: InstallMethod::NativeSystemd,
    default_region: "ap-hongkong",
    cli_tool: "cnb",
    plan: lighthouse_plan,
};

/// Azure VM — Docker image + Key Vault secrets via managed identity.
pub const AZURE: CloudTarget = CloudTarget {
    slug: "azure",
    display: "Azure VM",
    secret_store: SecretStore::KeyVault,
    install: InstallMethod::Docker,
    default_region: "eastus",
    cli_tool: "az",
    plan: azure_plan,
};

/// DigitalOcean Droplet — native systemd, env-file secrets, cloud-init + doctl.
///
/// Hunter-requested target. Modeled like Azure/Lighthouse: secrets in
/// `/etc/codewhale/*.env`, native+systemd install driven by a cloud-init
/// user-data file, and `doctl` for the create/destroy commands. The `plan()`
/// returns `doctl` `ProvisionStep` data, but since `--apply` is stubbed in the
/// MVP the plan is only printed inside the generated RUNBOOK.
pub const DIGITALOCEAN: CloudTarget = CloudTarget {
    slug: "digitalocean",
    display: "DigitalOcean Droplet",
    secret_store: SecretStore::EnvFile,
    install: InstallMethod::NativeSystemd,
    default_region: "sfo3",
    cli_tool: "doctl",
    plan: digitalocean_plan,
};

/// All registered cloud targets. Adding a cloud is one row here.
pub const CLOUD_TARGETS: &[CloudTarget] = &[LIGHTHOUSE, AZURE, DIGITALOCEAN];

/// Look up a cloud target by slug.
#[must_use]
pub fn cloud_by_slug(slug: &str) -> Option<&'static CloudTarget> {
    CLOUD_TARGETS
        .iter()
        .find(|c| c.slug.eq_ignore_ascii_case(slug))
}

// ---------------------------------------------------------------------------
// Cloud plans (data only — never executed in the MVP)
// ---------------------------------------------------------------------------

fn lighthouse_plan(inputs: &DeployInputs) -> Vec<ProvisionStep> {
    // Lighthouse provisioning is driven by the existing CNB pipeline
    // (deploy/tencent-lighthouse/cnb/*). The "plan" here is the CNB trigger plus
    // the host-side service install the RUNBOOK walks the user through.
    let restart_bridge = format!("codewhale-{}-bridge", inputs.bridge_slug);
    vec![
        ProvisionStep::new(
            "Render and commit the CNB pipeline (cnb.yml + tag_deploy.yml) for this deploy",
            "git",
            &["add", ".cnb.yml", ".cnb/tag_deploy.yml"],
        ),
        ProvisionStep::new(
            "Trigger the CNB `web_trigger_lighthouse` button to build + ship to the host",
            "cnb",
            &["trigger", "web_trigger_lighthouse"],
        ),
        ProvisionStep::new(
            "On the host: install both systemd units and start the runtime + bridge",
            "bash",
            &["scripts/tencent-lighthouse/install-services.sh"],
        ),
        ProvisionStep::new(
            format!("Restart the bridge service after the deploy ({restart_bridge})"),
            "systemctl",
            &["restart", &restart_bridge],
        ),
    ]
}

fn azure_plan(inputs: &DeployInputs) -> Vec<ProvisionStep> {
    let rg = format!("{}-rg", inputs.instance_name);
    let vault = format!("{}-kv", inputs.instance_name);
    let nsg = format!("{}-nsg", inputs.instance_name);
    let provider_secret = format!("codewhale-{}-key", inputs.provider_slug);
    vec![
        ProvisionStep::new(
            "Create the resource group",
            "az",
            &[
                "group",
                "create",
                "--name",
                &rg,
                "--location",
                &inputs.region,
            ],
        ),
        ProvisionStep::new(
            "Create the Key Vault that holds the provider key + runtime token",
            "az",
            &[
                "keyvault",
                "create",
                "--name",
                &vault,
                "--resource-group",
                &rg,
                "--location",
                &inputs.region,
            ],
        ),
        ProvisionStep::new(
            format!(
                "Store the {} provider key in Key Vault (value piped via stdin, not argv)",
                inputs.provider_slug
            ),
            "az",
            &[
                "keyvault",
                "secret",
                "set",
                "--vault-name",
                &vault,
                "--name",
                &provider_secret,
            ],
        ),
        ProvisionStep::new(
            format!(
                "Create the VM from {} with cloud-init custom-data + a system-assigned identity",
                inputs.image
            ),
            "az",
            &[
                "vm",
                "create",
                "--resource-group",
                &rg,
                "--name",
                &inputs.instance_name,
                "--custom-data",
                "cloud-init.yaml",
                "--assign-identity",
                // A Linux `vm create` otherwise adds an NSG rule admitting SSH
                // from any address; the next step adds the caller-only rule.
                "--nsg",
                &nsg,
                "--nsg-rule",
                "NONE",
            ],
        ),
        ProvisionStep::new(
            "Allow SSH (22) from the caller IP only (replace <your-public-ip>); 7878 stays on 127.0.0.1",
            "az",
            &[
                "network",
                "nsg",
                "rule",
                "create",
                "--resource-group",
                &rg,
                "--nsg-name",
                &nsg,
                "--name",
                "allow-ssh-from-caller",
                "--priority",
                "1000",
                "--direction",
                "Inbound",
                "--access",
                "Allow",
                "--protocol",
                "Tcp",
                "--destination-port-ranges",
                "22",
                "--source-address-prefixes",
                "<your-public-ip>/32",
            ],
        ),
    ]
}

fn digitalocean_plan(inputs: &DeployInputs) -> Vec<ProvisionStep> {
    // A Droplet stood up from a cloud-init user-data file, then the host-side
    // service install. doctl is the cloud CLI; commands are data only here.
    vec![
        ProvisionStep::new(
            "Create the Droplet from the generated cloud-init user-data (native + systemd)",
            "doctl",
            &[
                "compute",
                "droplet",
                "create",
                &inputs.instance_name,
                "--region",
                &inputs.region,
                "--image",
                "ubuntu-24-04-x64",
                "--size",
                "s-2vcpu-4gb",
                "--user-data-file",
                "cloud-init.yaml",
                "--ssh-keys",
                "<your-ssh-key-fingerprint>",
                "--wait",
            ],
        ),
        ProvisionStep::new(
            "Read the Droplet's public IPv4 for the SSH step below",
            "doctl",
            &[
                "compute",
                "droplet",
                "get",
                &inputs.instance_name,
                "--format",
                "PublicIPv4",
                "--no-header",
            ],
        ),
        ProvisionStep::new(
            "On the Droplet: write /etc/codewhale/*.env, install both systemd units, enable --now",
            "bash",
            &["scripts/tencent-lighthouse/install-services.sh"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// Repo root, resolved from this crate's manifest dir (`crates/app-server`).
    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/app-server has a two-level parent (repo root)")
            .to_path_buf()
    }

    #[test]
    fn bridge_slugs_are_unique() {
        let mut seen = HashSet::new();
        for b in BRIDGES {
            assert!(seen.insert(b.slug), "duplicate bridge slug: {}", b.slug);
        }
        assert_eq!(seen.len(), BRIDGES.len());
    }

    #[test]
    fn cloud_slugs_are_unique() {
        let mut seen = HashSet::new();
        for c in CLOUD_TARGETS {
            assert!(seen.insert(c.slug), "duplicate cloud slug: {}", c.slug);
        }
        assert_eq!(seen.len(), CLOUD_TARGETS.len());
    }

    #[test]
    fn digitalocean_is_registered() {
        // Hunter explicitly wants DigitalOcean in the matrix.
        assert!(
            cloud_by_slug("digitalocean").is_some(),
            "DigitalOcean must be a registered cloud target"
        );
        let r#do = cloud_by_slug("digitalocean").unwrap();
        assert_eq!(r#do.secret_store, SecretStore::EnvFile);
        assert_eq!(r#do.install, InstallMethod::NativeSystemd);
        assert_eq!(r#do.cli_tool, "doctl");
    }

    #[test]
    fn every_bridge_references_existing_files() {
        let root = repo_root();
        for b in BRIDGES {
            let pkg = root.join(b.package_dir);
            assert!(
                pkg.is_dir(),
                "bridge {} package_dir missing: {}",
                b.slug,
                pkg.display()
            );
            let unit = root
                .join("deploy/tencent-lighthouse/systemd")
                .join(b.service_unit);
            assert!(
                unit.is_file(),
                "bridge {} service_unit missing: {}",
                b.slug,
                unit.display()
            );
            let template = root.join(b.env_template);
            assert!(
                template.is_file(),
                "bridge {} env_template missing: {}",
                b.slug,
                template.display()
            );
            assert!(
                !b.secret_keys.is_empty(),
                "bridge {} must declare at least one secret key",
                b.slug
            );
        }
    }

    #[test]
    fn lookup_helpers_are_case_insensitive() {
        assert_eq!(bridge_by_slug("TELEGRAM").map(|b| b.slug), Some("telegram"));
        assert_eq!(cloud_by_slug("Azure").map(|c| c.slug), Some("azure"));
        assert!(bridge_by_slug("nope").is_none());
        assert!(cloud_by_slug("nope").is_none());
    }

    #[test]
    fn cloud_plans_return_ordered_steps_without_executing() {
        // Build (never run) a plan for each cloud and assert on program+args.
        let inputs = DeployInputs::default();
        for c in CLOUD_TARGETS {
            let steps = (c.plan)(&inputs);
            assert!(!steps.is_empty(), "cloud {} produced an empty plan", c.slug);
            // First step's program is the cloud's own tooling or a host script.
            assert!(
                steps
                    .iter()
                    .all(|s| !s.program.is_empty() && !s.description.is_empty()),
                "cloud {} has a malformed step",
                c.slug
            );
        }

        // DigitalOcean specifically drives doctl.
        let do_steps = (DIGITALOCEAN.plan)(&inputs);
        assert!(
            do_steps.iter().any(|s| s.program == "doctl"),
            "DigitalOcean plan must use doctl"
        );
        // Azure specifically drives az.
        let az_steps = (AZURE.plan)(&inputs);
        assert!(
            az_steps.iter().any(|s| s.program == "az"),
            "Azure plan must use az"
        );
    }

    #[test]
    fn azure_plan_admits_ssh_from_the_caller_only() {
        let steps = (AZURE.plan)(&DeployInputs::default());
        let arg_after = |step: &ProvisionStep, flag: &str| {
            let at = step.args.iter().position(|arg| arg == flag)?;
            step.args.get(at + 1).cloned()
        };
        // `open-port` has no source restriction: it admits the whole internet.
        assert!(
            steps
                .iter()
                .all(|s| !s.args.iter().any(|a| a == "open-port")),
            "Azure plan must not open a port to every source"
        );
        let create = steps
            .iter()
            .find(|s| {
                s.args
                    .starts_with(&["vm".to_string(), "create".to_string()])
            })
            .expect("vm create step");
        assert_eq!(arg_after(create, "--nsg-rule").as_deref(), Some("NONE"));
        let ssh_rules: Vec<_> = steps
            .iter()
            .filter(|s| arg_after(s, "--destination-port-ranges").as_deref() == Some("22"))
            .collect();
        assert_eq!(ssh_rules.len(), 1, "exactly one SSH rule");
        let source = arg_after(ssh_rules[0], "--source-address-prefixes").unwrap();
        assert!(
            source.ends_with("/32") && !source.starts_with('*') && !source.starts_with("0.0.0.0"),
            "SSH source must be a single caller address, got {source}"
        );
        assert_eq!(
            arg_after(ssh_rules[0], "--nsg-name"),
            arg_after(create, "--nsg"),
            "the rule must land on the NSG the VM was created with"
        );
    }

    #[test]
    fn display_command_redacts_secret_args() {
        let mut step =
            ProvisionStep::new("set secret", "az", &["keyvault", "secret", "set", "VALUE"]);
        step.secret_args = vec![3];
        let rendered = step.display_command();
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("VALUE"));
    }
}
