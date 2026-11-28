use crate::config::CommandCompactionConfig;
use crate::outcome::CommandResult;
use crate::types::CommandProviderId;
pub use codewhale_protocol::setup::SetupStep;

pub enum ModelSelection<'a> {
    Auto,
    Named(&'a str),
}

pub struct ModelSelectionReceipt {
    pub message: String,
    pub compaction: CommandCompactionConfig,
}

pub struct ProviderSelection {
    pub id: CommandProviderId,
    pub model: Option<String>,
    pub already_active: bool,
}

pub struct ProviderFallback {
    pub current: String,
    pub entries: Vec<(usize, CommandProviderId, bool)>,
    pub last_reason: Option<String>,
}

pub trait CommandProviderRoutesContext {
    fn resolve_switch(
        &self,
        selector: &str,
        model: Option<&str>,
    ) -> Result<ProviderSelection, String>;
    fn fallback(&self) -> ProviderFallback;
}

pub trait CommandModelSelectionContext {
    fn select_session_model(
        &mut self,
        selection: ModelSelection<'_>,
    ) -> Result<ModelSelectionReceipt, String>;
    fn save_session_route_default(&mut self) -> String;
}

pub enum ModelAction<'a> {
    OpenPicker,
    RefreshCatalog,
    Router(&'a str),
    UpdateCompaction(CommandCompactionConfig),
}

pub enum ProviderAction<'a> {
    OpenPicker,
    Setup(Option<&'a str>),
    Switch {
        provider: CommandProviderId,
        model: Option<String>,
    },
}

pub enum SetupAction<'a> {
    OpenWizard(Option<SetupStep>),
    Provider(&'a str),
}

pub fn resolve_action<T, A: TryFrom<T, Error = String>>(
    intent: T,
    message: Option<String>,
) -> CommandResult<A> {
    match A::try_from(intent) {
        Ok(action) => CommandResult {
            message,
            action: Some(action),
            is_error: false,
        },
        Err(reason) => CommandResult::error(reason),
    }
}
