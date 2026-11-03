//! Bundle rendering for `codewhale remote-setup`.
//!
//! Renders a self-contained deploy bundle to `--out`:
//! `runtime.env`, `<bridge>.env`, the runtime + bridge systemd units, and a
//! `RUNBOOK.md` with the exact remaining manual steps and first-pairing flow.
//!
//! Env files lead with `CODEWHALE_*` keys; `DEEPSEEK_*` are documented as legacy
//! aliases. The provider lives entirely in `runtime.env` (the bridge is pure
//! transport and never needs to know which provider is behind the runtime).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::registry::{BridgeSpec, CloudTarget, DeployInputs, InstallMethod, SecretStore};

/// Default runtime port the units and bundle use.
pub const DEFAULT_PORT: u16 = 7878;
/// Default worker count.
pub const DEFAULT_WORKERS: u32 = 2;
/// Default runtime URL the bridge talks to (loopback only).
pub const DEFAULT_RUNTIME_URL: &str = "http://127.0.0.1:7878";

/// Minimal provider facts the bundle needs, read from the existing
/// `codewhale_config::provider` registry (the single source of truth).
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Canonical provider slug, e.g. `"deepseek"`.
    pub slug: String,
    /// Human-readable display name, e.g. `"DeepSeek"`.
    pub display: String,
    /// The provider's own API-key env var, e.g. `"DEEPSEEK_API_KEY"` (`env_keys[0]`).
    pub key_var: String,
    /// Provider default model, used as a comment hint in the bundle.
    pub default_model: String,
}

impl ProviderInfo {
    /// Resolve a [`ProviderInfo`] from a slug against the config provider registry.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        let kind = codewhale_config::ProviderKind::parse(slug)?;
        let p = codewhale_config::provider::provider_for_kind(kind);
        let key_var = p.env_vars().first().copied().unwrap_or("CODEWHALE_API_KEY");
        Some(Self {
            slug: p.id().to_string(),
            display: p.display_name().to_string(),
            key_var: key_var.to_string(),
            default_model: p.default_model().to_string(),
        })
    }
}

/// Everything needed to render a bundle. Constructed by the wizard (or directly
/// in tests). Secret *values* are placeholders the RUNBOOK tells the user to
/// replace; the only generated secret is the runtime token.
#[derive(Debug, Clone)]
pub struct BundleInputs {
    pub cloud: &'static CloudTarget,
    pub bridge: &'static BridgeSpec,
    pub provider: ProviderInfo,
    /// Model id to write (default `"auto"`).
    pub model: String,
    /// Generated runtime token shared by `runtime.env` and `<bridge>.env`.
    pub runtime_token: String,
    /// Provider API-key value (placeholder unless the user supplied one).
    pub provider_key_value: String,
    /// Bridge secret values keyed by env var (placeholder unless supplied).
    pub bridge_secret_values: Vec<(String, String)>,
    /// Allowlist string (comma-separated chat ids); may be empty for first pairing.
    pub allowlist: String,
    /// Runtime port.
    pub port: u16,
    /// Runtime worker count.
    pub workers: u32,
    /// Workspace path on the host.
    pub workspace: String,
}

impl BundleInputs {
    /// Build the [`DeployInputs`] the cloud `plan()` consumes.
    #[must_use]
    pub fn deploy_inputs(&self) -> DeployInputs {
        DeployInputs {
            bridge_slug: self.bridge.slug.to_string(),
            provider_slug: self.provider.slug.to_string(),
            region: self.cloud.default_region.to_string(),
            instance_name: "codewhale-remote".to_string(),
            image: "ghcr.io/codewhale-hq/codewhale:latest".to_string(),
        }
    }
}

/// A single rendered file: relative path within the bundle + contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFile {
    pub relative_path: String,
    pub contents: String,
}

/// Render every bundle file in memory (no filesystem writes). Pure function —
/// used directly by tests so we never touch disk or run a command.
#[must_use]
pub fn render_bundle(inputs: &BundleInputs) -> Vec<RenderedFile> {
    vec![
        RenderedFile {
            relative_path: "runtime.env".to_string(),
            contents: render_runtime_env(inputs),
        },
        RenderedFile {
            relative_path: format!("{}.env", inputs.bridge.slug),
            contents: render_bridge_env(inputs),
        },
        RenderedFile {
            relative_path: "codewhale-runtime.service".to_string(),
            contents: render_runtime_unit(inputs),
        },
        RenderedFile {
            relative_path: inputs.bridge.service_unit.to_string(),
            contents: render_bridge_unit(inputs),
        },
        RenderedFile {
            relative_path: "RUNBOOK.md".to_string(),
            contents: render_runbook(inputs),
        },
    ]
}

/// Render the bundle to `out_dir`, creating it if needed. Returns the absolute
/// paths written, in render order.
pub fn write_bundle(inputs: &BundleInputs, out_dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating bundle dir {}", out_dir.display()))?;
    let mut written = Vec::new();
    for file in render_bundle(inputs) {
        let path = out_dir.join(&file.relative_path);
        std::fs::write(&path, file.contents)
            .with_context(|| format!("writing {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

// ---------------------------------------------------------------------------
// runtime.env — provider config lives here
// ---------------------------------------------------------------------------

fn render_runtime_env(i: &BundleInputs) -> String {
    let mut out = String::new();
    out.push_str("# Codewhale runtime config — generated by `codewhale remote-setup`.\n");
    out.push_str("# Provider configuration lives here; the bridge is pure transport.\n");
    out.push_str("# CODEWHALE_* keys are canonical. DEEPSEEK_* are read as legacy aliases.\n\n");

    out.push_str(&format!("CODEWHALE_PROVIDER={}\n", i.provider.slug));
    out.push_str(&format!(
        "# Provider API key ({}). Replace the placeholder with your real key.\n",
        i.provider.display
    ));
    out.push_str(&format!(
        "{}={}\n",
        i.provider.key_var, i.provider_key_value
    ));
    out.push_str(&format!(
        "CODEWHALE_MODEL={}   # provider default is {}\n",
        i.model, i.provider.default_model
    ));
    out.push('\n');
    out.push_str("# Shared auth token between the runtime and the bridge. Generated for you;\n");
    out.push_str("# rotate it any time (keep runtime.env and the bridge env in sync).\n");
    out.push_str(&format!("CODEWHALE_RUNTIME_TOKEN={}\n", i.runtime_token));
    out.push_str(&format!("CODEWHALE_RUNTIME_PORT={}\n", i.port));
    out.push_str(&format!("CODEWHALE_RUNTIME_WORKERS={}\n", i.workers));
    out.push_str("RUST_LOG=info\n\n");

    if i.provider.slug == "deepseek" {
        out.push_str(
            "# Legacy aliases (still honored): DEEPSEEK_RUNTIME_TOKEN, DEEPSEEK_API_KEY,\n",
        );
        out.push_str("# DEEPSEEK_RUNTIME_PORT, DEEPSEEK_RUNTIME_WORKERS.\n");
    } else {
        out.push_str("# Legacy aliases (still honored): DEEPSEEK_RUNTIME_TOKEN,\n");
        out.push_str("# DEEPSEEK_RUNTIME_PORT, DEEPSEEK_RUNTIME_WORKERS.\n");
    }
    out
}

// ---------------------------------------------------------------------------
// <bridge>.env — transport only
// ---------------------------------------------------------------------------

fn render_bridge_env(i: &BundleInputs) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Codewhale {} bridge config — generated by `codewhale remote-setup`.\n",
        i.bridge.display
    ));
    out.push_str("# Transport only: forwards chat <-> the local runtime. No provider keys here.\n");
    out.push_str("# CODEWHALE_* keys are canonical; DEEPSEEK_* are read as legacy aliases.\n\n");

    out.push_str("# --- bridge credentials (replace placeholders) ---\n");
    for (key, value) in &i.bridge_secret_values {
        out.push_str(&format!("{key}={value}\n"));
    }
    out.push('\n');

    out.push_str("# --- transport to the local runtime ---\n");
    out.push_str(&format!("CODEWHALE_RUNTIME_URL={DEFAULT_RUNTIME_URL}\n"));
    out.push_str("# Must match CODEWHALE_RUNTIME_TOKEN in runtime.env.\n");
    out.push_str(&format!("CODEWHALE_RUNTIME_TOKEN={}\n", i.runtime_token));
    out.push_str(&format!("CODEWHALE_WORKSPACE={}\n", i.workspace));
    out.push_str(&format!("CODEWHALE_MODEL={}\n", i.model));
    out.push_str("CODEWHALE_MODE=agent\n");
    out.push_str("CODEWHALE_ALLOW_SHELL=true\n");
    out.push_str("CODEWHALE_TRUST_MODE=false\n");
    out.push_str("CODEWHALE_AUTO_APPROVE=false\n\n");

    out.push_str("# --- pairing / allowlist ---\n");
    out.push_str(&format!("{}\n", allowlist_lines(i)));

    out.push_str("\n# --- bridge tuning ---\n");
    out.push_str(&format!(
        "{}_THREAD_MAP_PATH=/var/lib/codewhale-{}-bridge/thread-map.json\n",
        bridge_env_prefix(i.bridge),
        i.bridge.slug
    ));
    out.push_str(&format!(
        "{}_ALLOW_GROUPS=false\n",
        bridge_env_prefix(i.bridge)
    ));
    out.push_str(&format!(
        "{}_REQUIRE_PREFIX_IN_GROUP=true\n",
        bridge_env_prefix(i.bridge)
    ));
    out.push_str(&format!(
        "{}_GROUP_PREFIX=/cw\n",
        bridge_env_prefix(i.bridge)
    ));
    out.push_str(&format!(
        "{}_MAX_REPLY_CHARS=3500\n",
        bridge_env_prefix(i.bridge)
    ));
    if i.bridge.slug == "telegram" {
        out.push_str("TELEGRAM_POLL_TIMEOUT_SECONDS=50\n");
    }
    out.push_str("CODEWHALE_TURN_TIMEOUT_MS=900000\n");
    out
}

/// The chat allowlist uses a bridge-prefixed var (TELEGRAM_/FEISHU_); the deploy
/// examples key it per bridge, so mirror that.
fn allowlist_lines(i: &BundleInputs) -> String {
    let prefix = bridge_env_prefix(i.bridge);
    format!(
        "# Comma-separated chat/user IDs allowed to control the runtime.\n# Leave empty only during first pairing, with {prefix}_ALLOW_UNLISTED=true.\n{prefix}_CHAT_ALLOWLIST={}\n{prefix}_ALLOW_UNLISTED=false",
        i.allowlist
    )
}

fn bridge_env_prefix(bridge: &BridgeSpec) -> &'static str {
    match bridge.slug {
        "telegram" => "TELEGRAM",
        "feishu" => "FEISHU",
        _ => "CODEWHALE",
    }
}

// ---------------------------------------------------------------------------
// systemd units
// ---------------------------------------------------------------------------

fn render_runtime_unit(i: &BundleInputs) -> String {
    format!(
        "[Unit]\n\
Description=Codewhale Runtime API\n\
Wants=network-online.target\n\
After=network-online.target\n\n\
[Service]\n\
Type=simple\n\
User=codewhale\n\
Group=codewhale\n\
WorkingDirectory={workspace}\n\
# Legacy /etc/deepseek is loaded first for old installs; /etc/codewhale wins.\n\
EnvironmentFile=-/etc/deepseek/runtime.env\n\
EnvironmentFile=-/etc/codewhale/runtime.env\n\
ExecStart=/bin/sh -lc 'exec /home/codewhale/.cargo/bin/codewhale serve --http --host 127.0.0.1 --port \"${{CODEWHALE_RUNTIME_PORT:-${{DEEPSEEK_RUNTIME_PORT:-{port}}}}}\" --workers \"${{CODEWHALE_RUNTIME_WORKERS:-${{DEEPSEEK_RUNTIME_WORKERS:-{workers}}}}}\" --auth-token \"${{CODEWHALE_RUNTIME_TOKEN:-${{DEEPSEEK_RUNTIME_TOKEN}}}}\"'\n\
Restart=on-failure\n\
RestartSec=5\n\
NoNewPrivileges=true\n\
PrivateTmp=true\n\
ProtectSystem=full\n\
ReadWritePaths=/home/codewhale/.codewhale /home/codewhale/.deepseek {workspace}\n\n\
[Install]\n\
WantedBy=multi-user.target\n",
        workspace = i.workspace,
        port = i.port,
        workers = i.workers,
    )
}

fn render_bridge_unit(i: &BundleInputs) -> String {
    format!(
        "[Unit]\n\
Description=Codewhale {display} Phone Bridge\n\
Wants=network-online.target codewhale-runtime.service\n\
After=network-online.target codewhale-runtime.service\n\n\
[Service]\n\
Type=simple\n\
User=codewhale\n\
Group=codewhale\n\
WorkingDirectory={install_dir}\n\
# Legacy /etc/deepseek is loaded first for old installs; /etc/codewhale wins.\n\
EnvironmentFile=-/etc/deepseek/{slug}-bridge.env\n\
EnvironmentFile=-/etc/codewhale/{slug}-bridge.env\n\
ExecStart=/usr/bin/node {install_dir}/src/index.mjs\n\
Restart=on-failure\n\
RestartSec=5\n\
NoNewPrivileges=true\n\
PrivateTmp=true\n\
ProtectSystem=full\n\
ReadWritePaths=/var/lib/codewhale-{slug}-bridge\n\n\
[Install]\n\
WantedBy=multi-user.target\n",
        display = i.bridge.display,
        slug = i.bridge.slug,
        install_dir = i.bridge.install_dir,
    )
}

// ---------------------------------------------------------------------------
// RUNBOOK.md
// ---------------------------------------------------------------------------

fn render_runbook(i: &BundleInputs) -> String {
    let mut out = String::new();
    let plan = (i.cloud.plan)(&i.deploy_inputs());

    out.push_str(&format!(
        "# Codewhale remote-setup runbook — {} + {}\n\n",
        i.cloud.display, i.bridge.display
    ));
    out.push_str("Generated by `codewhale remote-setup` (generate-only). Nothing was run on\n");
    out.push_str("your behalf. Follow the steps below to stand the agent up.\n\n");

    out.push_str("## What was generated\n\n");
    out.push_str("| File | Purpose |\n|---|---|\n");
    out.push_str(
        "| `runtime.env` | Provider + runtime config (the only place the provider is set). |\n",
    );
    out.push_str(&format!(
        "| `{}.env` | {} bridge transport config (token, allowlist, runtime URL). |\n",
        i.bridge.slug, i.bridge.display
    ));
    out.push_str("| `codewhale-runtime.service` | systemd unit for the runtime API. |\n");
    out.push_str(&format!(
        "| `{}` | systemd unit for the {} bridge. |\n\n",
        i.bridge.service_unit, i.bridge.display
    ));

    out.push_str("## 1. Fill in the secrets\n\n");
    out.push_str(&format!(
        "- In `runtime.env`, set `{}` to your real {} API key.\n",
        i.provider.key_var, i.provider.display
    ));
    out.push_str(&format!("- {}\n", i.bridge.setup_hint));
    out.push_str(&format!(
        "  Then set {} in `{}.env`.\n",
        i.bridge
            .secret_keys
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(" and "),
        i.bridge.slug
    ));
    out.push_str(&format!(
        "- A random `CODEWHALE_RUNTIME_TOKEN` was generated (`{}`). It is identical in\n",
        i.runtime_token
    ));
    out.push_str("  both files; rotate it any time, keeping both files in sync.\n");
    out.push_str(&format!(
        "- Reference env template (every supported key, with comments): `{}`.\n\n",
        i.bridge.env_template
    ));

    out.push_str("## 2. Provision the host\n\n");
    out.push_str(&format!(
        "Cloud: **{}** — install: {}, secrets: {}.\n\n",
        i.cloud.display,
        i.cloud.install.label(),
        i.cloud.secret_store.label()
    ));
    out.push_str(&format!(
        "Auto-provision (`--apply`) is **not yet implemented**. Run these `{}` steps\n",
        i.cloud.cli_tool
    ));
    out.push_str("yourself (commands shown as data — review before running):\n\n");
    for (n, step) in plan.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", n + 1, step.description));
        out.push_str(&format!(
            "   ```sh\n   {}\n   ```\n",
            step.display_command()
        ));
    }
    out.push('\n');
    if i.cloud.secret_store == SecretStore::KeyVault {
        out.push_str("> The VM reads the provider key + runtime token from Key Vault via its\n");
        out.push_str("> managed identity at boot — they are not baked into the image.\n\n");
    }

    out.push_str("## 3. Install the env files + units on the host\n\n");
    out.push_str("```sh\nsudo install -d -m 750 /etc/codewhale\n");
    out.push_str(&format!(
        "sudo install -m 600 runtime.env /etc/codewhale/runtime.env\n\
sudo install -m 600 {slug}.env /etc/codewhale/{slug}-bridge.env\n\
sudo install -m 644 codewhale-runtime.service /etc/systemd/system/codewhale-runtime.service\n\
sudo install -m 644 {unit} /etc/systemd/system/{unit}\n\
sudo systemctl daemon-reload\n\
sudo systemctl enable --now codewhale-runtime {unit}\n```\n\n",
        slug = i.bridge.slug,
        unit = i.bridge.service_unit,
    ));
    if matches!(i.cloud.install, InstallMethod::NativeSystemd) {
        out.push_str(&format!(
            "The {} bridge is a zero-dep Node service; install it at `{}` (its\n",
            i.bridge.display, i.bridge.install_dir
        ));
        out.push_str(&format!(
            "`WorkingDirectory`) by copying `{}` there and running `npm install` if needed.\n\n",
            i.bridge.package_dir
        ));
    }

    out.push_str("## 4. First pairing\n\n");
    match i.bridge.slug {
        "telegram" => {
            out.push_str("1. With `TELEGRAM_CHAT_ALLOWLIST` empty, temporarily set\n");
            out.push_str(
                "   `TELEGRAM_ALLOW_UNLISTED=true`, restart the bridge, and DM your bot once.\n",
            );
            out.push_str(
                "2. Read the chat id the bridge logs, add it to `TELEGRAM_CHAT_ALLOWLIST`,\n",
            );
            out.push_str("   set `TELEGRAM_ALLOW_UNLISTED=false`, and restart the bridge.\n");
        }
        "feishu" => {
            out.push_str("1. With `FEISHU_CHAT_ALLOWLIST` empty, temporarily set\n");
            out.push_str(
                "   `FEISHU_ALLOW_UNLISTED=true`, restart the bridge, and message the app once.\n",
            );
            out.push_str(
                "2. Read the open id the bridge logs, add it to `FEISHU_CHAT_ALLOWLIST`,\n",
            );
            out.push_str("   set `FEISHU_ALLOW_UNLISTED=false`, and restart the bridge.\n");
        }
        _ => {
            out.push_str("Pair by adding your chat id to the bridge allowlist, then disable\n");
            out.push_str("unlisted access and restart the bridge.\n");
        }
    }
    out.push('\n');

    out.push_str("## 5. Verify\n\n");
    out.push_str("```sh\nsudo systemctl status codewhale-runtime --no-pager\n");
    out.push_str(&format!(
        "sudo systemctl status {} --no-pager\n```\n\n",
        i.bridge.service_unit
    ));
    out.push_str(
        "Port 7878 stays bound to 127.0.0.1. To reach `/status` from a laptop, SSH-tunnel\n",
    );
    out.push_str("it (`ssh -L 7878:127.0.0.1:7878 <host>`) rather than opening the port.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_setup::registry::{AZURE, DIGITALOCEAN, FEISHU, LIGHTHOUSE, TELEGRAM};

    fn sample_inputs(
        cloud: &'static CloudTarget,
        bridge: &'static BridgeSpec,
        provider_slug: &str,
    ) -> BundleInputs {
        let provider = ProviderInfo::from_slug(provider_slug)
            .unwrap_or_else(|| panic!("provider {provider_slug} not in registry"));
        let bridge_secret_values = bridge
            .secret_keys
            .iter()
            .map(|k| {
                (
                    (*k).to_string(),
                    format!("replace-{}", k.to_ascii_lowercase()),
                )
            })
            .collect();
        BundleInputs {
            cloud,
            bridge,
            provider: provider.clone(),
            model: "auto".to_string(),
            // Fixed, clearly-fake token for deterministic tests (never executed).
            runtime_token: "test-runtime-token-0000".to_string(),
            provider_key_value: format!("replace-{}", provider.key_var.to_ascii_lowercase()),
            bridge_secret_values,
            allowlist: String::new(),
            port: DEFAULT_PORT,
            workers: DEFAULT_WORKERS,
            workspace: "/opt/whalebro".to_string(),
        }
    }

    #[test]
    fn provider_info_reads_registry() {
        let ds = ProviderInfo::from_slug("deepseek").unwrap();
        assert_eq!(ds.slug, "deepseek");
        assert_eq!(ds.key_var, "DEEPSEEK_API_KEY");
        let oai = ProviderInfo::from_slug("openai").unwrap();
        assert_eq!(oai.key_var, "OPENAI_API_KEY");
        // Provider-registry aliases resolve to the canonical slug.
        assert_eq!(
            ProviderInfo::from_slug("nvidia").unwrap().slug,
            "nvidia-nim"
        );
        assert_eq!(ProviderInfo::from_slug("kimi").unwrap().slug, "moonshot");
        assert!(ProviderInfo::from_slug("not-a-provider").is_none());
    }

    #[test]
    fn bundle_renders_expected_file_set() {
        let inputs = sample_inputs(&LIGHTHOUSE, &FEISHU, "deepseek");
        let files = render_bundle(&inputs);
        let names: Vec<_> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert!(names.contains(&"runtime.env"));
        assert!(names.contains(&"feishu.env"));
        assert!(names.contains(&"codewhale-runtime.service"));
        assert!(names.contains(&"codewhale-feishu-bridge.service"));
        assert!(names.contains(&"RUNBOOK.md"));
        assert_eq!(files.len(), 5);
    }

    #[test]
    fn runtime_and_bridge_share_the_token() {
        let inputs = sample_inputs(&AZURE, &TELEGRAM, "openai");
        let files = render_bundle(&inputs);
        let runtime = &files
            .iter()
            .find(|f| f.relative_path == "runtime.env")
            .unwrap()
            .contents;
        let bridge = &files
            .iter()
            .find(|f| f.relative_path == "telegram.env")
            .unwrap()
            .contents;
        let token_line = format!("CODEWHALE_RUNTIME_TOKEN={}", inputs.runtime_token);
        assert!(runtime.contains(&token_line), "runtime.env missing token");
        assert!(bridge.contains(&token_line), "bridge env missing token");
    }

    #[test]
    fn env_files_lead_with_codewhale_keys() {
        let inputs = sample_inputs(&DIGITALOCEAN, &TELEGRAM, "deepseek");
        let files = render_bundle(&inputs);
        let runtime = &files
            .iter()
            .find(|f| f.relative_path == "runtime.env")
            .unwrap()
            .contents;
        assert!(runtime.contains("CODEWHALE_PROVIDER=deepseek"));
        assert!(runtime.contains("CODEWHALE_RUNTIME_TOKEN="));
        assert!(runtime.contains("CODEWHALE_RUNTIME_PORT="));
        // Provider key var present (DeepSeek doubles as canonical + legacy alias).
        assert!(runtime.contains("DEEPSEEK_API_KEY="));
        // Documents the legacy alias convention.
        assert!(runtime.to_lowercase().contains("legacy alias"));

        let bridge = &files
            .iter()
            .find(|f| f.relative_path == "telegram.env")
            .unwrap()
            .contents;
        assert!(bridge.contains("CODEWHALE_RUNTIME_URL="));
        assert!(bridge.contains("TELEGRAM_BOT_TOKEN="));
    }

    #[test]
    fn runbook_is_non_empty_and_lists_the_plan() {
        // DigitalOcean specifically: the RUNBOOK should carry the doctl plan.
        let inputs = sample_inputs(&DIGITALOCEAN, &TELEGRAM, "deepseek");
        let files = render_bundle(&inputs);
        let runbook = &files
            .iter()
            .find(|f| f.relative_path == "RUNBOOK.md")
            .unwrap()
            .contents;
        assert!(runbook.len() > 400, "RUNBOOK should be substantial");
        assert!(runbook.contains("not yet implemented"));
        assert!(runbook.contains("doctl"));
        assert!(runbook.to_lowercase().contains("first pairing"));
    }

    #[test]
    fn every_cloud_bridge_provider_triple_renders() {
        // Cover the matrix per the RFC §Tests; assert CODEWHALE_* + matching token
        // + non-empty RUNBOOK. No command is ever executed.
        for cloud in &[LIGHTHOUSE, AZURE, DIGITALOCEAN] {
            for bridge in &[FEISHU, TELEGRAM] {
                for provider_slug in &["deepseek", "openai", "moonshot"] {
                    let inputs = sample_inputs(cloud, bridge, provider_slug);
                    let files = render_bundle(&inputs);
                    assert_eq!(files.len(), 5, "{}-{} file count", cloud.slug, bridge.slug);

                    let runtime = &files
                        .iter()
                        .find(|f| f.relative_path == "runtime.env")
                        .unwrap()
                        .contents;
                    assert!(runtime.contains(&format!("CODEWHALE_PROVIDER={provider_slug}")));
                    let token_line = format!("CODEWHALE_RUNTIME_TOKEN={}", inputs.runtime_token);
                    assert!(runtime.contains(&token_line));

                    let bridge_env = &files
                        .iter()
                        .find(|f| f.relative_path == format!("{}.env", bridge.slug))
                        .unwrap()
                        .contents;
                    assert!(bridge_env.contains(&token_line));

                    let runbook = &files
                        .iter()
                        .find(|f| f.relative_path == "RUNBOOK.md")
                        .unwrap()
                        .contents;
                    assert!(!runbook.is_empty());
                }
            }
        }
    }

    #[test]
    fn systemd_units_reference_codewhale_paths() {
        let inputs = sample_inputs(&LIGHTHOUSE, &FEISHU, "deepseek");
        let files = render_bundle(&inputs);
        let unit = &files
            .iter()
            .find(|f| f.relative_path == "codewhale-runtime.service")
            .unwrap()
            .contents;
        assert!(unit.contains("/etc/codewhale/runtime.env"));
        assert!(unit.contains("CODEWHALE_RUNTIME_TOKEN"));
        // Legacy path still loaded first.
        assert!(unit.contains("/etc/deepseek/runtime.env"));

        let bridge_unit = &files
            .iter()
            .find(|f| f.relative_path == "codewhale-feishu-bridge.service")
            .unwrap()
            .contents;
        assert!(bridge_unit.contains("/etc/codewhale/feishu-bridge.env"));
    }
}
