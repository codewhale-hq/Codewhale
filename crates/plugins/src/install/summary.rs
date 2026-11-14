//! One-line install summary shared by every install surface.
//!
//! A pure builder: everything it needs arrives in [`SummaryInput`], nothing is
//! read from disk, the registry or the environment. Each clause carries a
//! stable message id so a surface can localize the clause while the English
//! text here stays the fallback.

use std::collections::BTreeSet;

use super::dsh::DshConversion;
use crate::host_services::{HostService, HostServiceReview, PluginApprover, approver_label};
use crate::manifest::PluginInventory;

pub const MSG_HEAD: &str = "plugin.summary.head";
pub const MSG_STATE_ON: &str = "plugin.summary.state_on";
pub const MSG_STATE_OFF: &str = "plugin.summary.state_off";
pub const MSG_APPROVER: &str = "plugin.summary.approver";
pub const MSG_ADDS: &str = "plugin.summary.adds";
pub const MSG_ADDS_NOTHING_NEW: &str = "plugin.summary.adds_nothing_new";
pub const MSG_NOW_ALSO_ADDS: &str = "plugin.summary.now_also_adds";
pub const MSG_WONT_RUN: &str = "plugin.summary.wont_run";
pub const MSG_NEEDS_EXTENSION_HOST: &str = "plugin.summary.needs_extension_host";
pub const MSG_NEEDS_NODE: &str = "plugin.summary.needs_node";
pub const MSG_REPLACES: &str = "plugin.summary.replaces";
pub const MSG_DOWNGRADED: &str = "plugin.summary.downgraded";
pub const MSG_SERVICES: &str = "plugin.summary.services";
pub const MSG_SERVICES_NOW_ALSO: &str = "plugin.summary.services_now_also";
pub const MSG_SERVICES_WITHHELD: &str = "plugin.summary.services_withheld";
pub const MSG_SERVICES_UNOFFERED: &str = "plugin.summary.services_unoffered";
pub const MSG_SERVICES_PAUSE: &str = "plugin.summary.services_pause";
pub const MSG_PROVIDER_NEW_SESSION: &str = "plugin.summary.provider_new_session";

const HOST_LIST_LIMIT: usize = 3;
const VERSION_GATE_REASON: &str = "incompatible-version";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryClause {
    pub id: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSummary {
    pub clauses: Vec<SummaryClause>,
}

impl PluginSummary {
    #[must_use]
    pub fn line(&self) -> String {
        let mut clauses = self.clauses.iter();
        let Some(head) = clauses.next() else {
            return String::new();
        };
        let rest: Vec<&str> = clauses.map(|clause| clause.text.as_str()).collect();
        if rest.is_empty() {
            head.text.clone()
        } else {
            format!("{}: {}", head.text, rest.join("; "))
        }
    }

    #[must_use]
    pub fn ids(&self) -> Vec<&'static str> {
        self.clauses.iter().map(|clause| clause.id).collect()
    }
}

#[derive(Debug, Clone)]
pub struct PreviousInstall<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub source: Option<&'a str>,
    pub inventory: &'a PluginInventory,
    pub host_services: &'a [HostService],
}

#[derive(Debug, Clone)]
pub struct SummaryInput<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub source: Option<&'a str>,
    pub enabled: bool,
    pub approver: Option<&'a PluginApprover>,
    pub inventory: &'a PluginInventory,
    pub dsh: Option<&'a DshConversion>,
    pub host_services: Option<&'a HostServiceReview>,
    pub extension_host_on: bool,
    pub previous: Option<PreviousInstall<'a>>,
    pub downgrade_allowed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Capabilities {
    skills: usize,
    local_mcp: usize,
    remote_mcp: usize,
    commands: usize,
    agents: usize,
    hooks: usize,
    lsp: usize,
    native: usize,
    providers: usize,
    panels: usize,
    filesystem_roots: BTreeSet<String>,
    network_hosts: BTreeSet<String>,
    lifecycle_mutation: bool,
}

impl Capabilities {
    fn from_inventory(inventory: &PluginInventory) -> Self {
        let split = inventory.stdio_mcp_servers + inventory.remote_mcp_servers;
        let (local_mcp, remote_mcp) = if split == 0 {
            (inventory.mcp_servers, 0)
        } else {
            (
                inventory.stdio_mcp_servers + inventory.mcp_servers.saturating_sub(split),
                inventory.remote_mcp_servers,
            )
        };
        Self {
            skills: inventory.skills,
            local_mcp,
            remote_mcp,
            commands: inventory.commands,
            agents: inventory.agents,
            hooks: inventory.hooks,
            lsp: inventory.lsp,
            native: inventory.native,
            providers: inventory.providers,
            panels: 0,
            filesystem_roots: inventory.filesystem_roots.iter().cloned().collect(),
            network_hosts: inventory.network_hosts.iter().cloned().collect(),
            lifecycle_mutation: inventory.lifecycle_mutation,
        }
    }

    fn with_dsh(mut self, dsh: &DshConversion) -> Self {
        self.skills = self.skills.max(dsh.skills.len());
        self.local_mcp = self.local_mcp.max(dsh.local_servers.len());
        self.remote_mcp = self.remote_mcp.max(dsh.remote_servers.len());
        self.commands = self.commands.max(dsh.commands.len());
        self.agents = self.agents.max(dsh.agents.len());
        self.hooks = self.hooks.max(dsh.hooks.len());
        self.native = self.native.max(dsh.native_rows.len());
        if dsh.requires_native && self.native == 0 {
            self.native = 1;
        }
        self.panels = self.panels.max(dsh.client_closures.len());
        self.network_hosts.extend(dsh.network_hosts.iter().cloned());
        self
    }

    fn since(&self, old: &Self) -> Self {
        let more = |new: usize, old: usize| new.saturating_sub(old);
        Self {
            skills: more(self.skills, old.skills),
            local_mcp: more(self.local_mcp, old.local_mcp),
            remote_mcp: more(self.remote_mcp, old.remote_mcp),
            commands: more(self.commands, old.commands),
            agents: more(self.agents, old.agents),
            hooks: more(self.hooks, old.hooks),
            lsp: more(self.lsp, old.lsp),
            native: more(self.native, old.native),
            providers: more(self.providers, old.providers),
            panels: more(self.panels, old.panels),
            filesystem_roots: self
                .filesystem_roots
                .difference(&old.filesystem_roots)
                .cloned()
                .collect(),
            network_hosts: self
                .network_hosts
                .difference(&old.network_hosts)
                .cloned()
                .collect(),
            lifecycle_mutation: self.lifecycle_mutation && !old.lifecycle_mutation,
        }
    }

    fn phrases(&self) -> Vec<String> {
        let mut phrases = Vec::new();
        let mut count = |n: usize, one: &str, many: &str| {
            if n > 0 {
                phrases.push(format!("{n} {}", if n == 1 { one } else { many }));
            }
        };
        count(self.skills, "skill", "skills");
        count(self.local_mcp, "local MCP server", "local MCP servers");
        count(self.remote_mcp, "remote MCP server", "remote MCP servers");
        count(self.commands, "command", "commands");
        count(self.agents, "agent profile", "agent profiles");
        count(self.hooks, "hook", "hooks");
        count(self.lsp, "language server", "language servers");
        count(self.native, "native extension", "native extensions");
        count(self.providers, "provider", "providers");
        count(self.panels, "panel", "panels");
        if !self.filesystem_roots.is_empty() {
            phrases.push(format!("file access ({})", limited(&self.filesystem_roots)));
        }
        if !self.network_hosts.is_empty() {
            phrases.push(format!("network access ({})", limited(&self.network_hosts)));
        }
        if self.lifecycle_mutation {
            phrases.push("permission to change plugins".to_string());
        }
        phrases
    }
}

fn limited(items: &BTreeSet<String>) -> String {
    let shown: Vec<&str> = items
        .iter()
        .take(HOST_LIST_LIMIT)
        .map(String::as_str)
        .collect();
    let mut text = shown.join(", ");
    if items.len() > HOST_LIST_LIMIT {
        text.push_str(&format!(" and {} more", items.len() - HOST_LIST_LIMIT));
    }
    text
}

fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn rows(n: usize) -> String {
    format!("{n} {}", if n == 1 { "row" } else { "rows" })
}

fn quoted(peers: &[crate::host_services::ReviewPeer]) -> String {
    peers
        .iter()
        .map(|peer| format!("`{}`", peer.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn push(clauses: &mut Vec<SummaryClause>, id: &'static str, text: String) {
    clauses.push(SummaryClause { id, text });
}

#[must_use]
pub fn build_summary(input: &SummaryInput<'_>) -> PluginSummary {
    let mut clauses = Vec::new();

    let mut head = input.name.to_string();
    if !input.version.is_empty() {
        head.push(' ');
        head.push_str(input.version);
    }
    if let Some(source) = input.source.filter(|source| !source.is_empty()) {
        head.push_str(&format!(" from {source}"));
    }
    push(&mut clauses, MSG_HEAD, head);

    if input.enabled {
        push(&mut clauses, MSG_STATE_ON, "on".to_string());
    } else {
        push(&mut clauses, MSG_STATE_OFF, "off".to_string());
    }
    push(
        &mut clauses,
        MSG_APPROVER,
        approver_label(input.approver).to_string(),
    );

    if let Some(previous) = &input.previous {
        if input.downgrade_allowed && previous.version != input.version {
            push(
                &mut clauses,
                MSG_DOWNGRADED,
                format!("downgraded from {}", previous.version),
            );
        }
        if previous.source != input.source {
            let from = previous
                .source
                .filter(|source| !source.is_empty())
                .map_or_else(|| "an unrecorded source".to_string(), str::to_string);
            push(
                &mut clauses,
                MSG_REPLACES,
                format!("replaces {} from {from}", previous.name),
            );
        }
    }

    let mut now = Capabilities::from_inventory(input.inventory);
    if let Some(dsh) = input.dsh {
        now = now.with_dsh(dsh);
    }
    match &input.previous {
        None => {
            let phrases = now.phrases();
            if !phrases.is_empty() {
                push(
                    &mut clauses,
                    MSG_ADDS,
                    format!("adds {}", and_list(&phrases)),
                );
            }
        }
        Some(previous) => {
            let added = now
                .since(&Capabilities::from_inventory(previous.inventory))
                .phrases();
            if added.is_empty() {
                push(
                    &mut clauses,
                    MSG_ADDS_NOTHING_NEW,
                    "adds nothing new".to_string(),
                );
            } else {
                push(
                    &mut clauses,
                    MSG_NOW_ALSO_ADDS,
                    format!("now also adds {}", and_list(&added)),
                );
            }
        }
    }

    if let Some(dsh) = input.dsh {
        let skipped: Vec<_> = dsh
            .outcomes
            .iter()
            .filter(|outcome| outcome.needs_manual_port())
            .collect();
        if !skipped.is_empty() {
            let gated = skipped
                .iter()
                .filter(|outcome| outcome.reason.starts_with(VERSION_GATE_REASON))
                .count();
            let manual = skipped.len() - gated;
            let reason = match (gated, manual) {
                (0, _) => "each needs a manual port".to_string(),
                (_, 0) => "refused by the host version gate".to_string(),
                _ => format!(
                    "{gated} refused by the host version gate, {manual} with no portable form"
                ),
            };
            push(
                &mut clauses,
                MSG_WONT_RUN,
                format!("won't run: {} ({reason})", rows(skipped.len())),
            );
        }
        if dsh.requires_node {
            push(
                &mut clauses,
                MSG_NEEDS_NODE,
                "needs Node.js for its local servers".to_string(),
            );
        }
    }

    if now.native > 0 && !input.extension_host_on {
        push(
            &mut clauses,
            MSG_NEEDS_EXTENSION_HOST,
            "needs the extension host (off)".to_string(),
        );
    }

    if let Some(review) = input.host_services {
        let granted = review.grants();
        let previously: &[HostService] = input
            .previous
            .as_ref()
            .map_or(&[], |previous| previous.host_services);
        let names = |services: Vec<HostService>| -> Vec<String> {
            services
                .into_iter()
                .map(|service| service.name().to_string())
                .collect()
        };
        match &input.previous {
            None => {
                if !granted.is_empty() {
                    let mut text =
                        format!("uses host services {}", and_list(&names(granted.clone())));
                    let mut peers: Vec<_> = review
                        .lines
                        .iter()
                        .filter(|line| line.offered)
                        .flat_map(|line| line.shared_with.iter())
                        .collect();
                    peers.sort_by(|a, b| a.id.cmp(&b.id));
                    peers.dedup_by(|a, b| a.id == b.id);
                    if !peers.is_empty() {
                        let peers: Vec<_> = peers.into_iter().cloned().collect();
                        text.push_str(&format!(" (shared with {})", quoted(&peers)));
                    }
                    push(&mut clauses, MSG_SERVICES, text);
                }
            }
            Some(_) => {
                let fresh: Vec<_> = granted
                    .iter()
                    .copied()
                    .filter(|service| !previously.contains(service))
                    .collect();
                if !fresh.is_empty() {
                    push(
                        &mut clauses,
                        MSG_SERVICES_NOW_ALSO,
                        format!("now also uses host services {}", and_list(&names(fresh))),
                    );
                }
            }
        }
        let withheld: Vec<String> = review
            .lines
            .iter()
            .filter(|line| line.offered && !line.withheld_by.is_empty())
            .map(|line| {
                format!(
                    "{} ({} is turned on without it)",
                    line.service.name(),
                    quoted(&line.withheld_by)
                )
            })
            .collect();
        if !withheld.is_empty() {
            push(
                &mut clauses,
                MSG_SERVICES_WITHHELD,
                format!("off for now: {}", withheld.join(", ")),
            );
        }
        let unoffered: Vec<String> = review
            .lines
            .iter()
            .filter(|line| !line.offered)
            .map(|line| line.service.name().to_string())
            .collect();
        if !unoffered.is_empty() {
            push(
                &mut clauses,
                MSG_SERVICES_UNOFFERED,
                format!(
                    "this Codewhale does not offer {} to plugins",
                    and_list(&unoffered)
                ),
            );
        }
        if !granted.is_empty() {
            push(
                &mut clauses,
                MSG_SERVICES_PAUSE,
                "services pause for every plugin in its host until the host is idle".to_string(),
            );
        }
    }

    if now.providers > 0 {
        push(
            &mut clauses,
            MSG_PROVIDER_NEW_SESSION,
            "provider changes apply in a new session".to_string(),
        );
    }

    PluginSummary { clauses }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_services::{
        AgentApproval, AgentPosture, HostServiceLine, PersonVia, PluginCohort, ReviewPeer,
    };
    use crate::install::dsh::DshOutcome;

    fn inventory() -> PluginInventory {
        PluginInventory::default()
    }

    fn input<'a>(inventory: &'a PluginInventory, approver: &'a PluginApprover) -> SummaryInput<'a> {
        SummaryInput {
            name: "notes",
            version: "1.2.0",
            source: Some("github:acme/notes"),
            enabled: true,
            approver: Some(approver),
            inventory,
            dsh: None,
            host_services: None,
            extension_host_on: true,
            previous: None,
            downgrade_allowed: false,
        }
    }

    fn person() -> PluginApprover {
        PluginApprover::person(PersonVia::Terminal)
    }

    fn conversion() -> DshConversion {
        DshConversion {
            plugin_name: "dsh-whale-widget".into(),
            source_package: Some("dsh-whale-widget".into()),
            source_version: Some("0.4.2".into()),
            manifest_sha256: "0".repeat(64),
            layers: Vec::new(),
            outcomes: Vec::new(),
            diagnostics: Vec::new(),
            skills: vec!["balance".into()],
            commands: Vec::new(),
            agents: Vec::new(),
            hooks: Vec::new(),
            remote_servers: vec!["account".into()],
            local_servers: vec!["bridge".into(), "helper".into()],
            network_hosts: vec!["api.example.com".into()],
            requires_node: false,
            requires_native: false,
            native_rows: Vec::new(),
            client_closures: Vec::new(),
        }
    }

    fn skipped(kind: &str, reason: &str) -> DshOutcome {
        DshOutcome {
            row: Some("row".into()),
            package: Some("pkg".into()),
            kind: kind.into(),
            outcome: "skipped".into(),
            reason: reason.into(),
            layer: None,
            patch: None,
        }
    }

    fn text_of(summary: &PluginSummary, id: &str) -> Option<String> {
        summary
            .clauses
            .iter()
            .find(|clause| clause.id == id)
            .map(|clause| clause.text.clone())
    }

    #[test]
    fn summary_plain_plugin_names_source_state_approver_and_adds() {
        let inventory = PluginInventory {
            skills: 2,
            mcp_servers: 1,
            stdio_mcp_servers: 1,
            hooks: 1,
            network_hosts: vec!["api.example.com".into()],
            ..inventory()
        };
        let approver = person();
        let summary = build_summary(&input(&inventory, &approver));
        assert_eq!(
            summary.line(),
            "notes 1.2.0 from github:acme/notes: on; approved by you; adds 2 skills, 1 local MCP server, 1 hook and network access (api.example.com)"
        );
        assert_eq!(
            summary.ids(),
            vec![MSG_HEAD, MSG_STATE_ON, MSG_APPROVER, MSG_ADDS]
        );
    }

    #[test]
    fn summary_off_plugin_and_each_approver_kind_read_distinctly() {
        let inventory = inventory();
        let agent = PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::AutoReview,
            workspace: "/work".into(),
            thread_id: None,
            turn_id: None,
            tool_call_id: None,
        });
        let mut request = input(&inventory, &agent);
        request.enabled = false;
        request.source = None;
        let summary = build_summary(&request);
        assert_eq!(
            summary.line(),
            "notes 1.2.0: off; approved by the agent in Auto-Review"
        );
        let mut request = input(&inventory, &agent);
        request.approver = None;
        assert!(
            build_summary(&request)
                .line()
                .contains("approver not recorded")
        );
    }

    #[test]
    fn summary_dsh_package_counts_conversion_fields_and_panels() {
        let mut dsh = conversion();
        dsh.requires_node = true;
        dsh.client_closures = vec![crate::install::dsh::DshClientClosure {
            package: "dsh-whale-widget".into(),
            root: String::new(),
            entry: "source/client.js".into(),
            platform: None,
            inject: Vec::new(),
            host_provided: Vec::new(),
            files: Default::default(),
            bytes: 10,
        }];
        let inventory = inventory();
        let approver = person();
        let mut request = input(&inventory, &approver);
        request.dsh = Some(&dsh);
        let summary = build_summary(&request);
        assert_eq!(
            text_of(&summary, MSG_ADDS).unwrap(),
            "adds 1 skill, 2 local MCP servers, 1 remote MCP server, 1 panel and network access (api.example.com)"
        );
        assert_eq!(
            text_of(&summary, MSG_NEEDS_NODE).unwrap(),
            "needs Node.js for its local servers"
        );
        assert!(text_of(&summary, MSG_WONT_RUN).is_none());
    }

    #[test]
    fn summary_refused_and_version_refused_rows_are_counted_with_reasons() {
        let mut dsh = conversion();
        dsh.outcomes = vec![
            skipped("foreign", "no portable representation"),
            skipped("mcp", "needs a manual port"),
            DshOutcome {
                outcome: "converted".into(),
                ..skipped("mcp", "")
            },
        ];
        let inventory = inventory();
        let approver = person();
        let mut request = input(&inventory, &approver);
        request.dsh = Some(&dsh);
        assert_eq!(
            text_of(&build_summary(&request), MSG_WONT_RUN).unwrap(),
            "won't run: 2 rows (each needs a manual port)"
        );

        dsh.outcomes = vec![skipped(
            "patch",
            "incompatible-version: this bundle requires DSH `^0.1.0-rc.6`",
        )];
        let mut request = input(&inventory, &approver);
        request.dsh = Some(&dsh);
        assert_eq!(
            text_of(&build_summary(&request), MSG_WONT_RUN).unwrap(),
            "won't run: 1 row (refused by the host version gate)"
        );

        dsh.outcomes
            .push(skipped("foreign", "no portable representation"));
        let mut request = input(&inventory, &approver);
        request.dsh = Some(&dsh);
        assert_eq!(
            text_of(&build_summary(&request), MSG_WONT_RUN).unwrap(),
            "won't run: 2 rows (1 refused by the host version gate, 1 with no portable form)"
        );
    }

    #[test]
    fn summary_native_rows_need_the_extension_host_only_while_it_is_off() {
        let mut dsh = conversion();
        dsh.requires_native = true;
        dsh.native_rows = vec!["worker".into()];
        let inventory = inventory();
        let approver = person();
        let mut request = input(&inventory, &approver);
        request.dsh = Some(&dsh);
        request.extension_host_on = false;
        let off = build_summary(&request);
        assert_eq!(
            text_of(&off, MSG_NEEDS_EXTENSION_HOST).unwrap(),
            "needs the extension host (off)"
        );
        assert!(
            text_of(&off, MSG_ADDS)
                .unwrap()
                .contains("1 native extension")
        );
        request.extension_host_on = true;
        assert!(text_of(&build_summary(&request), MSG_NEEDS_EXTENSION_HOST).is_none());
    }

    #[test]
    fn summary_update_names_only_what_changed() {
        let before = PluginInventory {
            skills: 2,
            ..inventory()
        };
        let after = PluginInventory {
            skills: 2,
            hooks: 1,
            network_hosts: vec!["api.example.com".into()],
            ..inventory()
        };
        let approver = person();
        let mut request = input(&after, &approver);
        request.previous = Some(PreviousInstall {
            name: "notes",
            version: "1.1.0",
            source: Some("github:acme/notes"),
            inventory: &before,
            host_services: &[],
        });
        let summary = build_summary(&request);
        assert_eq!(
            text_of(&summary, MSG_NOW_ALSO_ADDS).unwrap(),
            "now also adds 1 hook and network access (api.example.com)"
        );
        assert!(text_of(&summary, MSG_ADDS).is_none());
        assert!(text_of(&summary, MSG_REPLACES).is_none());
        assert!(text_of(&summary, MSG_DOWNGRADED).is_none());

        request.previous = Some(PreviousInstall {
            name: "notes",
            version: "1.1.0",
            source: Some("github:acme/notes"),
            inventory: &after,
            host_services: &[],
        });
        assert_eq!(
            text_of(&build_summary(&request), MSG_ADDS_NOTHING_NEW).unwrap(),
            "adds nothing new"
        );
    }

    #[test]
    fn summary_source_change_and_downgrade_are_stated() {
        let inventory = inventory();
        let approver = person();
        let mut request = input(&inventory, &approver);
        request.previous = Some(PreviousInstall {
            name: "notes",
            version: "1.4.0",
            source: Some("github:old/notes"),
            inventory: &inventory,
            host_services: &[],
        });
        request.downgrade_allowed = true;
        let summary = build_summary(&request);
        assert_eq!(
            text_of(&summary, MSG_REPLACES).unwrap(),
            "replaces notes from github:old/notes"
        );
        assert_eq!(
            text_of(&summary, MSG_DOWNGRADED).unwrap(),
            "downgraded from 1.4.0"
        );
        request.downgrade_allowed = false;
        assert!(text_of(&build_summary(&request), MSG_DOWNGRADED).is_none());
        request.previous.as_mut().unwrap().source = None;
        assert_eq!(
            text_of(&build_summary(&request), MSG_REPLACES).unwrap(),
            "replaces notes from an unrecorded source"
        );
    }

    fn review(lines: Vec<HostServiceLine>) -> HostServiceReview {
        HostServiceReview::new("user/0/notes", "c", "k", &PluginCohort::Reviewed, lines)
    }

    fn peer(name: &str) -> ReviewPeer {
        ReviewPeer {
            id: format!("user/0/{name}"),
            name: name.into(),
        }
    }

    #[test]
    fn summary_host_services_state_sharing_withheld_and_the_pause() {
        let inventory = inventory();
        let approver = person();
        let review = review(vec![
            HostServiceLine {
                service: HostService::Workspace,
                offered: true,
                shared_with: vec![peer("other")],
                withheld_by: Vec::new(),
            },
            HostServiceLine {
                service: HostService::Network,
                offered: true,
                shared_with: Vec::new(),
                withheld_by: vec![peer("legacy")],
            },
            HostServiceLine {
                service: HostService::Remote,
                offered: false,
                shared_with: Vec::new(),
                withheld_by: Vec::new(),
            },
        ]);
        let mut request = input(&inventory, &approver);
        request.host_services = Some(&review);
        let summary = build_summary(&request);
        assert_eq!(
            text_of(&summary, MSG_SERVICES).unwrap(),
            "uses host services workspace and network (shared with `other`)"
        );
        assert_eq!(
            text_of(&summary, MSG_SERVICES_WITHHELD).unwrap(),
            "off for now: network (`legacy` is turned on without it)"
        );
        assert_eq!(
            text_of(&summary, MSG_SERVICES_UNOFFERED).unwrap(),
            "this Codewhale does not offer remote to plugins"
        );
        assert_eq!(
            text_of(&summary, MSG_SERVICES_PAUSE).unwrap(),
            "services pause for every plugin in its host until the host is idle"
        );

        let before = [HostService::Workspace];
        let prior = PluginInventory::default();
        request.previous = Some(PreviousInstall {
            name: "notes",
            version: "1.1.0",
            source: Some("github:acme/notes"),
            inventory: &prior,
            host_services: &before,
        });
        let update = build_summary(&request);
        assert_eq!(
            text_of(&update, MSG_SERVICES_NOW_ALSO).unwrap(),
            "now also uses host services network"
        );
        assert!(text_of(&update, MSG_SERVICES).is_none());

        let empty = self::review(Vec::new());
        request.previous = None;
        request.host_services = Some(&empty);
        assert!(text_of(&build_summary(&request), MSG_SERVICES_PAUSE).is_none());
    }

    #[test]
    fn summary_provider_declarations_say_they_apply_in_a_new_session() {
        let inventory = PluginInventory {
            providers: 1,
            ..inventory()
        };
        let approver = person();
        let summary = build_summary(&input(&inventory, &approver));
        assert_eq!(
            text_of(&summary, MSG_PROVIDER_NEW_SESSION).unwrap(),
            "provider changes apply in a new session"
        );
        assert!(text_of(&summary, MSG_ADDS).unwrap().contains("1 provider"));
    }

    #[test]
    fn summary_long_host_lists_are_shortened() {
        let inventory = PluginInventory {
            network_hosts: ["a.io", "b.io", "c.io", "d.io", "e.io"]
                .map(String::from)
                .to_vec(),
            ..inventory()
        };
        let approver = person();
        assert_eq!(
            text_of(&build_summary(&input(&inventory, &approver)), MSG_ADDS).unwrap(),
            "adds network access (a.io, b.io, c.io and 2 more)"
        );
    }
}
