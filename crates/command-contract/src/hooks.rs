#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEntrySummary {
    pub event_label: String,
    pub label: String,
    pub background: bool,
    pub honors_background: bool,
    pub timeout_display: String,
    pub condition_summary: Option<String>,
    pub command_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HooksListSnapshot {
    pub enabled: bool,
    pub hooks: Vec<HookEntrySummary>,
    pub problem_summaries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHooksReview {
    pub digest: String,
    pub sanitized_contents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HooksAction {
    OpenExtensionsTab,
    EditProjectHooks,
}

pub trait CommandHooksContext {
    fn list_hooks(&self) -> HooksListSnapshot;
    fn review_project_hooks(&self) -> Result<ProjectHooksReview, String>;
    fn approve_project_hooks(&mut self, digest: &str) -> Result<(), String>;
    fn revoke_project_hooks(&mut self) -> Result<(), String>;
}
