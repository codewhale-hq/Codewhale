use std::collections::BTreeMap;
use std::io::Write;
use std::net::IpAddr;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TailnetNode {
    #[serde(rename(deserialize = "ID"))]
    pub id: String,
    #[serde(default, rename(deserialize = "DNSName"))]
    pub dns_name: String,
    #[serde(default, rename(deserialize = "HostName"))]
    pub host_name: String,
    #[serde(default, rename(deserialize = "OS"))]
    pub os: String,
    #[serde(default, rename(deserialize = "Online"))]
    pub online: bool,
    #[serde(default, rename(deserialize = "TailscaleIPs"))]
    pub addresses: Vec<IpAddr>,
    #[serde(default, rename(deserialize = "sshHostKeys"), skip_serializing)]
    ssh_host_keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TailnetStatus {
    #[serde(rename = "BackendState")]
    backend_state: String,
    #[serde(default, rename = "Peer")]
    peers: BTreeMap<String, TailnetNode>,
}

pub fn parse_inventory(bytes: &[u8]) -> Result<Vec<TailnetNode>> {
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Tailnet inventory exceeds 1 MiB"
    );
    let status: TailnetStatus =
        serde_json::from_slice(bytes).context("Invalid Tailscale status")?;
    ensure!(
        status.backend_state == "Running",
        "Tailscale is not connected"
    );
    ensure!(
        status.peers.len() <= 512,
        "Tailnet inventory exceeds 512 peers"
    );
    let mut nodes: Vec<_> = status.peers.into_values().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    ensure!(
        nodes.windows(2).all(|pair| pair[0].id != pair[1].id),
        "Ambiguous tailnet node identity"
    );
    for node in &nodes {
        ensure!(
            !node.id.is_empty() && node.id.len() <= 256 && !node.id.chars().any(char::is_control),
            "Invalid tailnet node identity"
        );
    }
    Ok(nodes)
}

pub fn inventory() -> Result<Vec<TailnetNode>> {
    let mut command = Command::new("tailscale");
    command.args(["status", "--json"]);
    codewhale_util::utils::suppress_console_window(&mut command);
    codewhale_runtime::child_env::apply_to_command(
        &mut command,
        std::iter::empty::<(&str, &str)>(),
    );
    let output = codewhale_dependencies::probe_output_bounded(
        &mut command,
        true,
        Duration::from_secs(5),
        1024 * 1024,
    )
    .context("Could not read the local Tailscale connection")?;
    ensure!(
        output.status.success(),
        "Tailscale status failed; connect Tailscale before selecting this environment"
    );
    parse_inventory(&output.stdout)
}

pub fn resolve_node<'a>(nodes: &'a [TailnetNode], node_id: &str) -> Result<&'a TailnetNode> {
    let node = nodes
        .iter()
        .find(|node| node.id == node_id)
        .context("Selected tailnet node is absent; refusing a hostname substitute")?;
    ensure!(node.online, "Selected tailnet node is offline");
    ensure!(
        !node.addresses.is_empty(),
        "Selected tailnet node has no address"
    );
    ensure!(
        !node.ssh_host_keys.is_empty(),
        "Selected node does not advertise Tailscale SSH host keys"
    );
    Ok(node)
}

pub fn ssh_destination(node: &TailnetNode) -> Result<(String, tempfile::NamedTempFile)> {
    let address = node
        .addresses
        .iter()
        .find(|address| address.is_ipv4())
        .or(node.addresses.first())
        .context("Selected tailnet node has no address")?
        .to_string();
    let mut file = tempfile::Builder::new()
        .prefix("codewhale-tailnet-")
        .tempfile()?;
    let mut written = 0;
    ensure!(
        node.ssh_host_keys.len() <= 64,
        "Too many tailnet SSH host keys"
    );
    for key in &node.ssh_host_keys {
        ensure!(
            key.len() <= 16 * 1024 && !key.contains(['\r', '\n']),
            "Invalid tailnet SSH host key"
        );
        let mut parts = key.split_whitespace();
        let algorithm = parts.next().context("Empty tailnet SSH host key")?;
        let encoded = parts.next().context("Missing tailnet SSH host key")?;
        ensure!(
            matches!(
                algorithm,
                "ssh-ed25519"
                    | "ssh-rsa"
                    | "ecdsa-sha2-nistp256"
                    | "ecdsa-sha2-nistp384"
                    | "ecdsa-sha2-nistp521"
            ),
            "Unsupported tailnet SSH host key"
        );
        ensure!(
            !encoded.is_empty()
                && encoded
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte)),
            "Invalid encoded tailnet SSH host key"
        );
        writeln!(file, "{address} {algorithm} {encoded}")?;
        written += 1;
    }
    ensure!(
        written > 0,
        "No valid SSH host keys for selected tailnet node"
    );
    file.flush()?;
    Ok((address, file))
}

#[derive(Debug)]
pub(crate) struct TailnetSnapshot {
    nodes: std::result::Result<Vec<TailnetNode>, String>,
    captured: std::time::Instant,
    pub(crate) observed_at: String,
}

impl TailnetSnapshot {
    pub(crate) fn new(nodes: std::result::Result<Vec<TailnetNode>, String>) -> Self {
        Self {
            nodes,
            captured: std::time::Instant::now(),
            observed_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub(crate) fn resolve(&self, node_id: &str) -> Result<&TailnetNode> {
        ensure!(
            self.captured.elapsed() <= Duration::from_secs(30),
            "Tailnet inventory expired before launch"
        );
        let nodes = self
            .nodes
            .as_ref()
            .map_err(|message| anyhow::anyhow!("{message}"))?;
        resolve_node(nodes, node_id)
    }
}
