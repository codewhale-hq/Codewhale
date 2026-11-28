pub struct ClearedConversation {
    pub session_id: String,
    pub model: String,
    pub workspace: std::path::PathBuf,
    pub mode: crate::types::CommandMode,
    pub agent_preset: Option<String>,
}

pub enum ClearFailure {
    Busy(Vec<String>),
    Transition(String),
}

pub trait ClearContext {
    fn start_empty_conversation(&mut self) -> Result<ClearedConversation, ClearFailure>;
}

pub enum EffortFailure {
    Unknown,
    Unavailable { model: String, labels: Vec<String> },
}

pub trait EffortContext {
    fn available_labels(&self) -> Vec<String>;
    fn cycle(&mut self) -> (String, String);
    fn set_supported_effort(&mut self, value: &str) -> Result<String, EffortFailure>;
}

pub trait FeedbackContext {
    fn has_session(&self) -> bool;
    fn safe_text(&self, text: &str) -> Result<String, String>;
}

pub enum FeedbackAction {
    OpenPicker,
    Feature,
    Security,
    Draft(String),
    Review { id: String, change: Option<String> },
}
