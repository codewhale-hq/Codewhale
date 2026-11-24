use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostService {
    Workspace,
    Llm,
    Network,
    Settings,
    Catalog,
    Terminal,
    Remote,
    Attachments,
}

impl HostService {
    pub const ALL: [Self; 8] = [
        Self::Workspace,
        Self::Llm,
        Self::Network,
        Self::Settings,
        Self::Catalog,
        Self::Terminal,
        Self::Remote,
        Self::Attachments,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Llm => "llm",
            Self::Network => "network",
            Self::Settings => "settings",
            Self::Catalog => "catalog",
            Self::Terminal => "terminal",
            Self::Remote => "remote",
            Self::Attachments => "attachments",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.name() == name)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace files and commands",
            Self::Llm => "Your model provider",
            Self::Network => "Network requests",
            Self::Settings => "Codewhale settings",
            Self::Catalog => "Plugins and provider routes",
            Self::Terminal => "Terminal sessions",
            Self::Remote => "Remote pairing",
            Self::Attachments => "Images in the conversation",
        }
    }

    #[must_use]
    pub fn plain(self) -> &'static str {
        match self {
            Self::Workspace => {
                "read and change files and run commands in the session's workspace, under Codewhale's approval rules"
            }
            Self::Llm => "send requests to the model provider you approved, on your account",
            Self::Network => {
                "send requests to the hosts in its review, with credentials it stores in Codewhale"
            }
            Self::Settings => "read and change the Codewhale settings in its review",
            Self::Catalog => {
                "list your provider routes, read your balance and plan plugin installs"
            }
            Self::Terminal => "start and drive terminal sessions in the session's workspace",
            Self::Remote => "pair another device with the session",
            Self::Attachments => {
                "read and add images in this conversation, within Codewhale's image limits"
            }
        }
    }
}

pub const HOST_SERVICE_INJECTS: &[(&str, HostService)] = &[
    ("workspace", HostService::Workspace),
    ("llm", HostService::Llm),
    ("credentials", HostService::Network),
    ("settings", HostService::Settings),
    ("pluginManager", HostService::Catalog),
    ("deepseekAccount", HostService::Catalog),
    ("terminal", HostService::Terminal),
    ("remote", HostService::Remote),
];

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_OFFER: std::cell::RefCell<Option<Vec<HostService>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestOffer(Option<Vec<HostService>>);

#[cfg(any(test, feature = "test-support"))]
impl TestOffer {
    pub fn only(services: &[HostService]) -> Self {
        Self(TEST_OFFER.with(|offer| offer.replace(Some(services.to_vec()))))
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestOffer {
    fn drop(&mut self) {
        let previous = self.0.take();
        TEST_OFFER.with(|offer| *offer.borrow_mut() = previous);
    }
}

#[must_use]
pub fn offered(service: HostService) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(offer) = TEST_OFFER.with(|offer| offer.borrow().clone()) {
        return offer.contains(&service);
    }
    HOST_SERVICE_INJECTS
        .iter()
        .any(|(_, candidate)| *candidate == service)
}

#[must_use]
pub fn declared_injects(source: &str) -> BTreeSet<String> {
    static DECLARATION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static NAME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let declaration = DECLARATION.get_or_init(|| {
        regex::Regex::new(r"\binject\b\s*[:=]\s*(\[[^\]]*\]|\{[^;]*?\}\s*(?:[;,\n)]|$))")
            .expect("inject declaration pattern")
    });
    let name = NAME.get_or_init(|| {
        regex::Regex::new(
            r#""([A-Za-z_$][\w$]{0,63})"|'([A-Za-z_$][\w$]{0,63})'|([A-Za-z_$][\w$]{0,63})\s*:"#,
        )
        .expect("inject name pattern")
    });
    let mut names = BTreeSet::new();
    for found in declaration.captures_iter(source) {
        let body = found.get(1).map_or("", |body| body.as_str());
        for capture in name.captures_iter(body) {
            if let Some(matched) = capture.get(1).or(capture.get(2)).or(capture.get(3)) {
                names.insert(matched.as_str().to_string());
            }
        }
    }
    names
}

#[must_use]
pub fn services_for_injects<'a>(
    injects: impl IntoIterator<Item = &'a str>,
    table: &[(&str, HostService)],
) -> Vec<HostService> {
    let services: BTreeSet<HostService> = injects
        .into_iter()
        .flat_map(|inject| {
            table
                .iter()
                .filter(move |(name, _)| *name == inject)
                .map(|(_, service)| *service)
        })
        .collect();
    services.into_iter().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonVia {
    Terminal,
    App,
    ApprovalCard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPosture {
    FullAccess,
    AutoReview,
    CommandLine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentApproval {
    pub posture: AgentPosture,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginApprover {
    Person { via: PersonVia },
    Agent(AgentApproval),
    CommandLine,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginCohort {
    Reviewed,
    Agent { workspace: String },
}

impl PluginCohort {
    #[must_use]
    pub fn agent(workspace: impl AsRef<std::path::Path>) -> Self {
        let normalized = codewhale_config::normalize_project_workspace(workspace.as_ref())
            .unwrap_or_else(|_| workspace.as_ref().components().collect());
        Self::Agent {
            workspace: normalized.to_string_lossy().into_owned(),
        }
    }
}

impl PartialEq for PluginCohort {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Reviewed, Self::Reviewed) => true,
            (Self::Agent { workspace: a }, Self::Agent { workspace: b }) => {
                let norm_a = codewhale_config::normalize_project_workspace(std::path::Path::new(a))
                    .unwrap_or_else(|_| std::path::Path::new(a).components().collect());
                let norm_b = codewhale_config::normalize_project_workspace(std::path::Path::new(b))
                    .unwrap_or_else(|_| std::path::Path::new(b).components().collect());
                norm_a == norm_b
            }
            _ => false,
        }
    }
}

impl Eq for PluginCohort {}

impl PartialOrd for PluginCohort {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PluginCohort {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let key = |cohort: &Self| match cohort {
            Self::Reviewed => (0u8, None),
            Self::Agent { workspace } => {
                let norm =
                    codewhale_config::normalize_project_workspace(std::path::Path::new(workspace))
                        .unwrap_or_else(|_| std::path::Path::new(workspace).components().collect());
                (1u8, Some(norm))
            }
        };
        key(self).cmp(&key(other))
    }
}

impl std::hash::Hash for PluginCohort {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Reviewed => 0u8.hash(state),
            Self::Agent { workspace } => {
                1u8.hash(state);
                let norm =
                    codewhale_config::normalize_project_workspace(std::path::Path::new(workspace))
                        .unwrap_or_else(|_| std::path::Path::new(workspace).components().collect());
                norm.hash(state);
            }
        }
    }
}

impl PluginApprover {
    #[must_use]
    pub fn person(via: PersonVia) -> Self {
        Self::Person { via }
    }

    #[must_use]
    pub fn command_line(agent_workspace: Option<&std::path::Path>) -> Self {
        match agent_workspace {
            Some(workspace) => Self::Agent(AgentApproval {
                posture: AgentPosture::CommandLine,
                workspace: workspace.to_string_lossy().into_owned(),
                thread_id: None,
                turn_id: None,
                tool_call_id: None,
            }),
            None => Self::CommandLine,
        }
    }

    #[must_use]
    pub fn auto_review(
        agent_workspace: &std::path::Path,
        thread_id: Option<String>,
        turn_id: Option<String>,
        tool_call_id: Option<String>,
    ) -> Self {
        Self::Agent(AgentApproval {
            posture: AgentPosture::AutoReview,
            workspace: agent_workspace.to_string_lossy().into_owned(),
            thread_id,
            turn_id,
            tool_call_id,
        })
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Person { .. } => "person",
            Self::Agent(_) => "agent",
            Self::CommandLine => "command_line",
        }
    }

    #[must_use]
    pub fn cohort(&self) -> PluginCohort {
        match self {
            Self::Agent(agent) => PluginCohort::agent(&agent.workspace),
            Self::Person { .. } | Self::CommandLine => PluginCohort::Reviewed,
        }
    }

    #[must_use]
    pub fn agent_workspace(&self) -> Option<&str> {
        match self {
            Self::Agent(agent) => Some(agent.workspace.as_str()),
            Self::Person { .. } | Self::CommandLine => None,
        }
    }
}

#[must_use]
pub fn cohort_of(approver: Option<&PluginApprover>) -> PluginCohort {
    approver.map_or(PluginCohort::Reviewed, PluginApprover::cohort)
}

#[must_use]
pub fn approver_label(approver: Option<&PluginApprover>) -> &'static str {
    match approver {
        Some(PluginApprover::Person { .. }) => "approved by you",
        Some(PluginApprover::Agent(agent)) => match agent.posture {
            AgentPosture::FullAccess => "approved by the agent in Full Access",
            AgentPosture::AutoReview => "approved by the agent in Auto-Review",
            AgentPosture::CommandLine => "approved by the agent from a shell command",
        },
        Some(PluginApprover::CommandLine) => {
            "trusted from the command line with --yes; no review was shown there"
        }
        None => "approver not recorded (reviewed before 0.10.3)",
    }
}

#[must_use]
pub fn recorded_approver_label(
    approver: Option<&str>,
    posture: Option<&str>,
) -> Option<&'static str> {
    match (approver, posture) {
        (Some("person"), _) => Some("approved by you"),
        (Some("agent"), Some("full_access")) => Some("approved by the agent in Full Access"),
        (Some("agent"), Some("auto_review")) => Some("approved by the agent in Auto-Review"),
        (Some("agent"), Some("command_line")) => Some("approved by the agent from a shell command"),
        (Some("command_line"), _) => {
            Some("trusted from the command line with --yes; no review was shown there")
        }
        (Some("none"), _) => Some("approver not recorded (reviewed before 0.10.3)"),
        _ => None,
    }
}

pub const AGENT_PROCESS_NOTE: &str = "runs apart from your reviewed plugins until you review it";

#[must_use]
pub fn short_review_date(timestamp: &str) -> &str {
    let bytes = timestamp.as_bytes();
    if bytes.len() >= 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
        && timestamp.is_char_boundary(10)
    {
        &timestamp[..10]
    } else {
        timestamp
    }
}

#[must_use]
pub fn workspace_folder_name(workspace: &str) -> &str {
    std::path::Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostServiceWireState {
    CanUse,
    NotOffered,
}

#[must_use]
pub fn host_service_wire_state(
    granted: &[HostService],
    line: &HostServiceLine,
) -> HostServiceWireState {
    if line.offered && granted.contains(&line.service) {
        HostServiceWireState::CanUse
    } else {
        HostServiceWireState::NotOffered
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewApproval {
    pub approved_by: PluginApprover,
    pub host_services: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewPeer {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostServiceLine {
    pub service: HostService,
    pub offered: bool,
    pub shared_with: Vec<ReviewPeer>,
    pub withheld_by: Vec<ReviewPeer>,
}

pub const REVIEW_LEAD: &str = "This plugin asks to use services of the Codewhale host. Plugins share one process: a service works only while every plugin that is turned on holds it, and each of those plugins can use it while another's tool runs.";

fn quoted(peers: &[ReviewPeer]) -> String {
    peers
        .iter()
        .map(|peer| format!("`{}`", peer.name))
        .collect::<Vec<_>>()
        .join(", ")
}

impl HostServiceLine {
    #[must_use]
    pub fn sentence(&self) -> String {
        let label = self.service.label();
        if !self.offered {
            return format!(
                "{label}: this version of Codewhale does not offer it to plugins. Approving grants nothing for it."
            );
        }
        let mut sentence = format!("{label}: {}.", self.service.plain());
        if self.shared_with.is_empty() {
            sentence.push_str(" No other reviewed plugin holds it.");
        } else {
            sentence.push_str(&format!(
                " Shared with {}: each can use it as this plugin while this plugin's tool runs, and this plugin can use it as them.",
                quoted(&self.shared_with)
            ));
        }
        if !self.withheld_by.is_empty() {
            sentence.push_str(&format!(
                " Off for now: {} {} turned on and {} review does not include it.",
                quoted(&self.withheld_by),
                if self.withheld_by.len() == 1 {
                    "is"
                } else {
                    "are"
                },
                if self.withheld_by.len() == 1 {
                    "its"
                } else {
                    "their"
                },
            ));
        }
        sentence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostServiceReview {
    pub lines: Vec<HostServiceLine>,
    pub token: String,
}

impl HostServiceReview {
    #[must_use]
    pub fn new(
        plugin_id: &str,
        content_hash: &str,
        capability_hash: &str,
        cohort: &PluginCohort,
        lines: Vec<HostServiceLine>,
    ) -> Self {
        let mut hasher = Sha256::new();
        let mut field = |label: &str, value: &str| {
            hasher.update(label.as_bytes());
            hasher.update(b"\0");
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        };
        field("codewhale-host-service-review-v1", plugin_id);
        field("content", content_hash);
        field("capability", capability_hash);
        match cohort {
            PluginCohort::Reviewed => field("cohort", "reviewed"),
            PluginCohort::Agent { workspace } => field("cohort-agent", workspace),
        }
        for line in &lines {
            field("service", line.service.name());
            field("offered", if line.offered { "yes" } else { "no" });
            for peer in &line.shared_with {
                field("shared", &peer.id);
            }
            for peer in &line.withheld_by {
                field("withheld", &peer.id);
            }
        }
        let token = crate::manifest::hex_digest(hasher.finalize());
        Self { lines, token }
    }

    #[must_use]
    pub fn grants(&self) -> Vec<HostService> {
        self.lines
            .iter()
            .filter(|line| line.offered)
            .map(|line| line.service)
            .collect()
    }

    #[must_use]
    pub fn text(&self) -> Vec<String> {
        if self.lines.is_empty() {
            return Vec::new();
        }
        std::iter::once(REVIEW_LEAD.to_string())
            .chain(self.lines.iter().map(HostServiceLine::sentence))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> ReviewPeer {
        ReviewPeer {
            id: format!("user/0/{name}"),
            name: name.to_string(),
        }
    }

    fn line(shared: &[&str], withheld: &[&str], offered: bool) -> HostServiceLine {
        HostServiceLine {
            service: HostService::Network,
            offered,
            shared_with: shared.iter().map(|name| peer(name)).collect(),
            withheld_by: withheld.iter().map(|name| peer(name)).collect(),
        }
    }

    #[test]
    fn every_host_service_is_offered_and_the_table_names_only_advertised_injects() {
        assert!(!HOST_SERVICE_INJECTS.is_empty());
        assert!(
            HostService::ALL
                .into_iter()
                .all(|service| (service == HostService::Attachments) ^ offered(service))
        );
        let table: serde_json::Value =
            serde_json::from_slice(crate::activation::DSH_COMPATIBILITY).unwrap();
        let advertised: BTreeSet<&str> = table["provided_services"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|group| group.as_array().unwrap())
            .map(|name| name.as_str().unwrap())
            .collect();
        for (inject, _) in HOST_SERVICE_INJECTS {
            assert!(advertised.contains(inject), "{inject} is not advertised");
        }
        for service in HostService::ALL {
            assert_eq!(HostService::parse(service.name()), Some(service));
            assert_eq!(
                serde_json::to_value(service).unwrap(),
                serde_json::Value::String(service.name().into())
            );
        }
        let _offer = TestOffer::only(&[HostService::Llm]);
        assert!(offered(HostService::Llm));
        assert!(!offered(HostService::Network));
    }

    #[test]
    fn the_review_says_whether_a_service_is_held_alone_shared_withheld_or_not_offered() {
        assert_eq!(
            line(&[], &[], true).sentence(),
            "Network requests: send requests to the hosts in its review, with credentials it stores in Codewhale. No other reviewed plugin holds it."
        );
        assert_eq!(
            line(&["alpha", "beta"], &[], true).sentence(),
            "Network requests: send requests to the hosts in its review, with credentials it stores in Codewhale. Shared with `alpha`, `beta`: each can use it as this plugin while this plugin's tool runs, and this plugin can use it as them."
        );
        assert_eq!(
            line(&[], &["gamma"], true).sentence(),
            "Network requests: send requests to the hosts in its review, with credentials it stores in Codewhale. No other reviewed plugin holds it. Off for now: `gamma` is turned on and its review does not include it."
        );
        assert!(
            line(&["alpha"], &["gamma", "delta"], true)
                .sentence()
                .ends_with("Off for now: `gamma`, `delta` are turned on and their review does not include it.")
        );
        assert_eq!(
            line(&["alpha"], &["gamma"], false).sentence(),
            "Network requests: this version of Codewhale does not offer it to plugins. Approving grants nothing for it."
        );
        let review = HostServiceReview::new(
            "id",
            "c",
            "k",
            &PluginCohort::Reviewed,
            vec![line(&["alpha"], &[], true)],
        );
        assert_eq!(review.text()[0], REVIEW_LEAD);
        assert_eq!(review.text().len(), 2);
        assert_eq!(review.grants(), vec![HostService::Network]);
        assert!(
            HostServiceReview::new(
                "id",
                "c",
                "k",
                &PluginCohort::Reviewed,
                vec![line(&[], &[], false)]
            )
            .grants()
            .is_empty()
        );
        let token =
            |lines| HostServiceReview::new("id", "c", "k", &PluginCohort::Reviewed, lines).token;
        let base = token(vec![line(&["alpha"], &[], true)]);
        assert_eq!(base.len(), 64);
        assert_ne!(base, token(vec![line(&["beta"], &[], true)]));
        assert_ne!(base, token(vec![line(&[], &["alpha"], true)]));
        assert_ne!(base, token(vec![line(&["alpha"], &[], false)]));
        assert_ne!(
            base,
            HostServiceReview::new(
                "other",
                "c",
                "k",
                &PluginCohort::Reviewed,
                vec![line(&["alpha"], &[], true)]
            )
            .token
        );
        assert_ne!(
            base,
            HostServiceReview::new(
                "id",
                "c2",
                "k",
                &PluginCohort::Reviewed,
                vec![line(&["alpha"], &[], true)]
            )
            .token
        );
        assert_ne!(
            base,
            HostServiceReview::new(
                "id",
                "c",
                "k",
                &PluginCohort::Agent {
                    workspace: "/w".into()
                },
                vec![line(&["alpha"], &[], true)]
            )
            .token
        );
        assert!(
            HostServiceReview::new("id", "c", "k", &PluginCohort::Reviewed, Vec::new())
                .text()
                .is_empty()
        );
    }

    #[test]
    fn the_record_says_who_approved_in_one_shape_and_the_label_says_it_in_words() {
        let agent = PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::AutoReview,
            workspace: "/w".into(),
            thread_id: Some("t".into()),
            turn_id: Some("u".into()),
            tool_call_id: Some("c".into()),
        });
        let cases = [
            (
                PluginApprover::person(PersonVia::ApprovalCard),
                serde_json::json!({"kind": "person", "via": "approval_card"}),
                "approved by you",
            ),
            (
                agent.clone(),
                serde_json::json!({"kind": "agent", "posture": "auto_review", "workspace": "/w", "thread_id": "t", "turn_id": "u", "tool_call_id": "c"}),
                "approved by the agent in Auto-Review",
            ),
            (
                PluginApprover::command_line(None),
                serde_json::json!({"kind": "command_line"}),
                "trusted from the command line with --yes; no review was shown there",
            ),
            (
                PluginApprover::command_line(Some(std::path::Path::new("/w"))),
                serde_json::json!({"kind": "agent", "posture": "command_line", "workspace": "/w"}),
                "approved by the agent from a shell command",
            ),
        ];
        for (approver, json, label) in cases {
            assert_eq!(serde_json::to_value(&approver).unwrap(), json);
            assert_eq!(
                serde_json::from_value::<PluginApprover>(json).unwrap(),
                approver
            );
            assert_eq!(approver_label(Some(&approver)), label);
        }
        assert_eq!(
            approver_label(None),
            "approver not recorded (reviewed before 0.10.3)"
        );
        assert_eq!(cohort_of(None), PluginCohort::Reviewed);
        assert_eq!(
            PluginApprover::command_line(None).cohort(),
            PluginCohort::Reviewed
        );
        assert_eq!(
            agent.cohort(),
            PluginCohort::Agent {
                workspace: "/w".into()
            }
        );
        assert!(serde_json::from_value::<PluginApprover>(serde_json::json!("person")).is_err());
        assert!(
            serde_json::from_value::<PluginApprover>(serde_json::json!({"kind": "agent"})).is_err()
        );
    }

    #[test]
    fn inject_declarations_map_to_host_services_through_one_table() {
        let table = [
            ("llm", HostService::Llm),
            ("credentials", HostService::Network),
            ("pluginManager", HostService::Catalog),
            ("deepseekAccount", HostService::Catalog),
        ];
        let array =
            "export const inject = ['tools', 'llm', \"subagents\"]\nconst other = ['credentials']";
        assert_eq!(
            declared_injects(array),
            ["llm", "subagents", "tools"].map(String::from).into()
        );
        assert_eq!(
            services_for_injects(declared_injects(array).iter().map(String::as_str), &table),
            vec![HostService::Llm]
        );
        let property =
            "var plugin={name:'w',inject:['webServer','credentials','connection'],apply(){}}";
        assert_eq!(
            services_for_injects(
                declared_injects(property).iter().map(String::as_str),
                &table
            ),
            vec![HostService::Network]
        );
        let object = "class P { static inject = { required: ['pluginManager'], optional: [\"llm\"] }; }\nPlugin.inject = { deepseekAccount: { required: false } };";
        assert_eq!(
            services_for_injects(declared_injects(object).iter().map(String::as_str), &table),
            vec![HostService::Llm, HostService::Catalog]
        );
        assert!(declared_injects("const injected = ['llm']; reinject('llm')").is_empty());
        assert!(
            services_for_injects(declared_injects(array).iter().map(String::as_str), &[])
                .is_empty()
        );
    }
}
