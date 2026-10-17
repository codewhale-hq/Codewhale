use std::path::PathBuf;

use crate::types::CommandMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationUserHelp {
    pub name: String,
    pub description: Option<String>,
    pub usage: Option<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationBuiltinHelp {
    pub name: String,
    pub usage: String,
    pub description: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationHelpTopic {
    pub user: Option<NavigationUserHelp>,
    pub builtin: Option<NavigationBuiltinHelp>,
    pub shadowed_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationSkill {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationOverview {
    pub model: String,
    pub mode: CommandMode,
    pub mode_label: String,
    pub workspace: PathBuf,
    pub history_count: usize,
    pub total_tokens: u32,
    pub queued_count: usize,
    pub subagent_count: usize,
    pub active_skill: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationProviderKind {
    Moonshot,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationProviderLinks {
    pub id: String,
    pub name: String,
    pub credential_url: Option<String>,
    pub docs_url: Option<String>,
    pub guidance: String,
    pub env_names: Vec<String>,
    pub kind: NavigationProviderKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationLinks {
    pub active_provider: String,
    pub locale_tag: String,
    pub providers: Vec<NavigationProviderLinks>,
    pub kimi_route: String,
    pub kimi_console: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTarget {
    Missing,
    NotDirectory,
    Directory { canonical_or_candidate: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorWriteError {
    Open(String),
    Write(String),
}

pub trait CommandNavigationContext {
    fn help_topic(&self, topic: &str) -> NavigationHelpTopic;
    fn cached_skills(&self) -> Vec<NavigationSkill>;
    fn help_is_open(&self) -> bool;
    fn open_help(&mut self);
    fn home_is_blocked(&self) -> bool;
    fn restore_home(&mut self);
    fn overview(&self) -> NavigationOverview;
    fn links(&self) -> NavigationLinks;
}

pub trait CommandVoiceInputContext {
    fn recorder_available(&self) -> bool;
    fn voice_enabled(&self) -> bool;
    fn set_voice_enabled(&mut self, enabled: bool);
    fn send_enabled(&self) -> bool;
    fn set_send_enabled(&mut self, enabled: bool);
    fn control_enabled(&self) -> bool;
    fn set_control_enabled(&mut self, enabled: bool);
}
