use codewhale_command_contract::CommandDiscovery;
use codewhale_localization::MessageId;
use std::path::Path;
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone)]
pub struct BuiltinCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub usage: String,
    pub description_id: MessageId,
    pub subcommands: Vec<String>,
    pub takes_arguments: bool,
    pub requires_argument: bool,
    pub requires_required_argument: bool,
    pub composer_wants_trailing_space: bool,
    pub palette_runs_directly: bool,
    pub show_in_empty_discovery: bool,
    pub discovery: CommandDiscovery,
    pub unlisted: bool,
}

#[derive(Debug, Clone)]
pub struct UserCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub usage: Option<String>,
    pub takes_arguments: bool,
    pub hidden: bool,
    pub extension: bool,
    pub plugin_id: Option<String>,
    pub workspace_sourced: bool,
    pub builtin_bindings: Vec<String>,
}

/// The host’s read-only command metadata and collision checks, implemented
/// by the commands side.
pub trait CommandCatalog: Send + Sync {
    /// Whether a built-in command answers to `name` (lower case): a canonical
    /// name, an alias, or one of the mode aliases the dispatcher answers ahead
    /// of the registry.
    fn answers_to(&self, name: &str) -> bool;
    fn builtins(&self) -> Vec<BuiltinCommand>;
    fn user_commands(&self, workspace: &Path) -> Vec<UserCommand>;
    /// Whether this host executes `name` remotely at all. Checked before any
    /// thread state is assembled, so a name outside the remote surface is
    /// refused exactly like a name no host knows. `false` is the default: a
    /// host opts in one command at a time, and widening this must widen
    /// [`Self::execute_builtin`] too.
    fn serves_builtin(&self, name: &str) -> bool {
        let _ = name;
        false
    }
    /// Execute one built-in command against a host snapshot. `None` means
    /// this host does not implement remote execution, or does not answer to
    /// the name; `Some` carries the command's own answer.
    fn execute_builtin(
        &self,
        name: &str,
        args: Option<&str>,
        host: &BuiltinCommandHostSnapshot,
    ) -> Option<BuiltinCommandExecution> {
        let _ = (name, args, host);
        None
    }
}

/// The thread state a remote built-in execution reads.
///
/// Engine-side so the app-server can assemble it from thread state and the
/// terminal host can interpret it without either depending on the other. The
/// projections are the command contract's own data-only types: the host maps
/// them into the facet envelope, and the capability mask each portable
/// handler declares decides which commands this snapshot can honestly serve.
pub struct BuiltinCommandHostSnapshot {
    pub mode: codewhale_command_contract::types::CommandMode,
    /// The host's own label for `mode` ("WORK", "PLAN"), the same string the
    /// terminal footer shows.
    pub mode_label: String,
    pub locale: codewhale_localization::Locale,
    pub model: String,
    pub system_prompt: codewhale_command_contract::facets::DebugSystemPrompt,
    /// Published usage telemetry and the host's cost breakdown for the
    /// thread — the complete data source the portable `/tokens` and `/cost`
    /// renderers consume.
    pub tokens: codewhale_command_contract::facets::DebugTokenProjection,
}

/// One remote built-in execution's outcome.
pub struct BuiltinCommandExecution {
    pub name: String,
    pub text: String,
    pub is_error: bool,
    /// `false` when the command also asked the terminal host for an action —
    /// the text is still what the command answered, but no API client can
    /// perform that action (a balance fetch, a snapshot restore) on the
    /// host's behalf.
    pub text_only: bool,
}

static CATALOG: OnceLock<Arc<dyn CommandCatalog>> = OnceLock::new();

/// Install the process's catalog, once at startup. The first install wins and
/// returns `true`; a later one is ignored.
pub fn install(catalog: Arc<dyn CommandCatalog>) -> bool {
    CATALOG.set(catalog).is_ok()
}

/// The installed catalog. `None` means none was installed (or, in a test, the
/// thread asked for none): command registration is refused.
pub fn installed() -> Option<Arc<dyn CommandCatalog>> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(chosen) = TEST_CATALOG.with(|cell| cell.borrow().clone()) {
        return chosen;
    }
    CATALOG.get().cloned()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// `Some(None)` is a thread that asked for no catalog at all.
    static TEST_CATALOG: std::cell::RefCell<Option<Option<Arc<dyn CommandCatalog>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: route [`installed`] on this thread to a catalog (or to
/// none) until dropped, restoring what was there before, so tests never touch
/// the process-wide install.
#[cfg(any(test, feature = "test-support"))]
pub struct CatalogGuard(Option<Option<Arc<dyn CommandCatalog>>>);

#[cfg(any(test, feature = "test-support"))]
impl CatalogGuard {
    pub fn install(catalog: Arc<dyn CommandCatalog>) -> Self {
        Self(TEST_CATALOG.with(|cell| cell.replace(Some(Some(catalog)))))
    }

    /// This thread has no catalog, whatever the process installed.
    pub fn absent() -> Self {
        Self(TEST_CATALOG.with(|cell| cell.replace(Some(None))))
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for CatalogGuard {
    fn drop(&mut self) {
        TEST_CATALOG.with(|cell| *cell.borrow_mut() = self.0.take());
    }
}
