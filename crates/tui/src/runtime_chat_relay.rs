//! Isolated native Runtime execution for account-owned Chat relay commands.
//!
//! The managed control plane supplies only opaque tenant/thread/turn bindings
//! and an exact non-secret provider route. Provider credentials and local
//! paths stay inside the Runtime. Every Chat thread is a dedicated
//! [`RuntimeThreadManager`] thread with an empty model-visible tool allowlist;
//! the active interactive TUI thread is never reused.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use codewhale_plugins::PluginRegistry;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use codewhale_engine::{
    core::engine::ISOLATED_CHAT_SYSTEM_PROMPT,
    runtime_threads::{
        RuntimeEventRecord, RuntimeTurnStatus, manager::CreateThreadRequest,
        manager::RuntimeThreadManager, manager::RuntimeThreadManagerConfig,
        manager::StartTurnRequest,
    },
};
use codewhale_runtime::config::{Config, MemoryBackend, MemoryConfig, SkillsConfig};

#[cfg(test)]
use codewhale_runtime::config::ContextConfig;

const STATE_SCHEMA_VERSION: u32 = 2;
const MAX_RELAY_ID_BYTES: usize = 240;
const MAX_OPERATION_KEY_BYTES: usize = 128;
const STATE_FILE: &str = "runtime-chat-bindings.json";
const SCOPE_LOCK_FILE: &str = "runtime-chat.owner.lock";

#[cfg(test)]
static TEST_STATE_PERSIST_FAILURES: std::sync::Mutex<Vec<(PathBuf, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn inject_state_persist_failures(path: &Path, count: usize) {
    assert!(count > 0);
    TEST_STATE_PERSIST_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((path.to_path_buf(), count));
}

#[cfg(test)]
fn take_state_persist_failure(path: &Path) -> bool {
    let mut failures = TEST_STATE_PERSIST_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(index) = failures.iter().position(|(target, _)| target == path) else {
        return false;
    };
    if failures[index].1 > 1 {
        failures[index].1 -= 1;
    } else {
        failures.remove(index);
    }
    true
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RuntimeChatPrompt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_review: Option<codewhale_protocol::op::ReviewOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_model_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_constitution:
        Option<codewhale_config::user_constitution::ProfileConstitutionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    #[serde(rename = "type")]
    pub command_type: String,
    pub run_id: String,
    pub turn_id: String,
    pub operation_key: String,
    pub runtime_binding_id: String,
    pub runtime_thread_id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<codewhale_protocol::runtime::RuntimeImageInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub model: String,
    pub model_provider: String,
    pub model_provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub allowed_tools: Vec<String>,
    pub mode: String,
    pub requested_mode: String,
    pub workspace: RuntimeChatWorkspace,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RuntimeChatWorkspace {
    pub id: String,
    pub target_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeChatControlScope {
    pub(crate) runtime_binding_id: String,
    pub(crate) runtime_thread_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeChatProjection {
    pub run_id: String,
    pub native_thread_id: String,
    pub native_seq: u64,
    pub source_event_id: String,
    pub virtual_thread_id: String,
    pub virtual_turn_id: String,
    pub event: &'static str,
    pub timestamp: String,
    pub payload: Value,
}

#[derive(Clone)]
pub(crate) struct RuntimeChatRelayHost {
    manager: Arc<RuntimeThreadManager>,
    config: Arc<Config>,
    state: Arc<Mutex<RelayState>>,
    state_path: Arc<PathBuf>,
    target_ref: Arc<String>,
    session_id: Arc<String>,
    _scope_lock: Arc<RelayScopeLock>,
    apply_lock: Arc<tokio::sync::Mutex<()>>,
    inference_ownership:
        Arc<Mutex<Option<codewhale_engine::client::RuntimeChatInferenceOwnership>>>,
    claimed_projections: Arc<Mutex<HashSet<(String, u64)>>>,
    authorized_run_id: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelayState {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_scope_fingerprint: Option<String>,
    #[serde(default)]
    bindings: Vec<RelayThreadBinding>,
}

impl Default for RelayState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            owner_scope_fingerprint: None,
            bindings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelayThreadBinding {
    run_id: String,
    runtime_binding_id: String,
    virtual_thread_id: String,
    native_thread_id: String,
    model: String,
    model_provider: String,
    model_provider_id: String,
    first_operation_fingerprint: String,
    system_prompt_fingerprint: Option<String>,
    #[serde(default)]
    turns: BTreeMap<String, RelayTurnBinding>,
    #[serde(default)]
    projected_native_seq: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelayTurnBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review_request_fingerprint: Option<String>,
    native_turn_id: String,
    operation_fingerprint: String,
    request_fingerprint: String,
    #[serde(default)]
    terminal_projected: bool,
    /// True only when the deterministic reservation was durably written but
    /// native start was proven to have rejected before accepting provider work.
    /// This distinguishes a retryable reservation from an already-projected
    /// terminal turn, whose exact replay must remain settled.
    #[serde(default)]
    start_rejected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnReservationDisposition {
    New,
    Reopened,
    ExistingUnsettled,
    ExistingTerminal,
}

impl RelayState {
    fn validate(&self) -> Result<()> {
        if self.schema_version != STATE_SCHEMA_VERSION {
            bail!("Runtime Chat binding state uses an unsupported schema");
        }
        if let Some(owner) = self.owner_scope_fingerprint.as_deref() {
            validate_fingerprint(owner)?;
        } else if !self.bindings.is_empty() {
            bail!("Runtime Chat binding state has no account owner");
        }
        let mut binding_ids = HashSet::new();
        let mut virtual_threads = HashSet::new();
        let mut native_threads = HashSet::new();
        for binding in &self.bindings {
            validate_relay_id(&binding.run_id, "run id")?;
            validate_relay_id(&binding.runtime_binding_id, "binding id")?;
            validate_virtual_thread_id(&binding.virtual_thread_id)?;
            validate_native_record_id(&binding.native_thread_id, "native thread id")?;
            validate_route_id(&binding.model_provider, "provider id")?;
            validate_route_id(&binding.model_provider_id, "model-provider id")?;
            validate_model_id(&binding.model)?;
            validate_fingerprint(&binding.first_operation_fingerprint)?;
            if let Some(fingerprint) = binding.system_prompt_fingerprint.as_deref() {
                validate_fingerprint(fingerprint)?;
            }
            if !binding_ids.insert(binding.runtime_binding_id.clone())
                || !virtual_threads.insert(binding.virtual_thread_id.clone())
                || !native_threads.insert(binding.native_thread_id.clone())
            {
                bail!("Runtime Chat binding state contains duplicate thread authority");
            }
            for (virtual_turn_id, turn) in &binding.turns {
                validate_virtual_turn_id(virtual_turn_id)?;
                validate_native_record_id(&turn.native_turn_id, "native turn id")?;
                if let Some(fingerprint) = &turn.review_request_fingerprint {
                    validate_fingerprint(fingerprint)?;
                }
                validate_fingerprint(&turn.operation_fingerprint)?;
                validate_fingerprint(&turn.request_fingerprint)?;
            }
        }
        Ok(())
    }
}

impl RuntimeChatRelayHost {
    pub(crate) async fn workspace_restore_guard(
        &self,
        workspace: &Path,
    ) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>, String> {
        self.manager
            .workspace_restore_guard(workspace)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) fn open(
        config: Config,
        _plugin_registry: Arc<PluginRegistry>,
        private_root: PathBuf,
        target_ref: String,
        session_id: String,
    ) -> Result<Self, String> {
        validate_owner_component(&target_ref, "target")?;
        validate_owner_component(&session_id, "session")?;
        let private_dir = scoped_private_dir(&private_root, &target_ref, &session_id);
        fs::create_dir_all(&private_dir)
            .map_err(|_| "Runtime Chat could not prepare private local state.".to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700))
                .map_err(|_| "Runtime Chat could not protect private local state.".to_string())?;
        }
        let scope_lock =
            RelayScopeLock::acquire(&private_dir.join(SCOPE_LOCK_FILE)).map_err(|error| {
                // Only WouldBlock is genuine contention. Any other lock
                // failure is a local IO fault, and misreporting it as
                // ownership hides the cause (#5735's flake evidence).
                let contention = error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::WouldBlock);
                if contention {
                    "Another Codewhale process already owns this Runtime Chat account session."
                        .to_string()
                } else {
                    format!("Runtime Chat could not take its owner lock: {error:#}")
                }
            })?;
        let state_path = private_dir.join(STATE_FILE);
        let state = load_state(&state_path).map_err(|_| {
            "The saved Runtime Chat binding state could not be trusted.".to_string()
        })?;
        state.validate().map_err(|_| {
            "The saved Runtime Chat binding state could not be trusted.".to_string()
        })?;

        // Reuse the native Runtime thread engine and durable records, but keep
        // account Chat in its own private store and empty workspace. The
        // dedicated system-prompt override below is the model-visible boundary;
        // this workspace/config hardening also prevents local project, memory,
        // instruction, and skill sources from becoming fallback context.
        let (execution_config, chat_workspace) =
            isolated_chat_execution_config(&config, &private_dir)?;
        let relay_plugin_registry = Arc::new(PluginRegistry::empty(&chat_workspace));
        let task_data_dir = private_dir.join("tasks");
        let mut manager_cfg = RuntimeThreadManagerConfig::from_task_data_dir(task_data_dir);
        manager_cfg.data_dir = private_dir.join("runtime");
        let manager = RuntimeThreadManager::open_with_plugin_registry(
            execution_config,
            chat_workspace,
            manager_cfg,
            relay_plugin_registry,
        )
        .map_err(|_| "Runtime Chat could not open its isolated native thread store.".to_string())?;

        Ok(Self {
            manager: Arc::new(manager),
            config: Arc::new(config),
            state: Arc::new(Mutex::new(state)),
            state_path: Arc::new(state_path),
            target_ref: Arc::new(target_ref),
            session_id: Arc::new(session_id),
            _scope_lock: Arc::new(scope_lock),
            apply_lock: Arc::new(tokio::sync::Mutex::new(())),
            inference_ownership: Arc::new(Mutex::new(None)),
            claimed_projections: Arc::new(Mutex::new(HashSet::new())),
            authorized_run_id: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn catalog(&self, challenge: &str) -> Result<Value, String> {
        self.ensure_account_bound()?;
        crate::runtime_api::runtime_chat_relay_catalog(&self.config, challenge)
    }

    pub(crate) fn catalog_payload_fingerprint(payload: &Value) -> Result<String, String> {
        serde_json::to_vec(&canonical_json_value(payload))
            .map(|bytes| hex_digest(Sha256::digest(bytes)))
            .map_err(|_| "Runtime Chat could not fingerprint its safe catalog.".to_string())
    }

    pub(crate) fn bind_account(&self, account_ref: &str, target_ref: &str) -> Result<(), String> {
        validate_owner_component(account_ref, "account")?;
        if target_ref != self.target_ref.as_str() {
            return Err("The Runtime Chat account owner does not match this target.".to_string());
        }
        let owner = owner_scope_fingerprint(account_ref, target_ref, &self.session_id);
        self.persist_state_update(
            "Runtime Chat could not persist its account ownership.",
            |state| bind_owner_scope(state, &owner),
        )
    }

    pub(crate) fn authorize_run(&self, run_id: &str) -> Result<(), String> {
        validate_relay_id(run_id, "run id")
            .map_err(|_| "The Runtime Chat attachment has an invalid run identity.".to_string())?;
        self.ensure_account_bound()?;
        let durable_other_run_is_unsettled = self.state.lock().bindings.iter().any(|binding| {
            binding.run_id != run_id && binding.turns.values().any(|turn| !turn.terminal_projected)
        });
        if durable_other_run_is_unsettled {
            return Err(
                "Finish or interrupt the active Runtime Chat turn before attaching another run."
                    .to_string(),
            );
        }
        if self.has_any_unsettled_turns() && self.inference_ownership.lock().is_none() {
            let ownership =
                codewhale_engine::client::try_acquire_runtime_chat_inference_ownership()
                    .ok_or_else(|| {
                        "Finish the active local turn before recovering Runtime Chat.".to_string()
                    })?;
            *self.inference_ownership.lock() = Some(ownership);
        }
        *self.authorized_run_id.lock() = Some(run_id.to_string());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn has_unsettled_authorized_turns(&self) -> bool {
        self.authorized_run_id
            .lock()
            .as_deref()
            .is_some_and(|run_id| self.has_unsettled_turns_for_run(run_id))
    }

    pub(crate) fn has_any_unsettled_turns(&self) -> bool {
        self.state
            .lock()
            .bindings
            .iter()
            .any(|binding| binding.turns.values().any(|turn| !turn.terminal_projected))
    }

    #[cfg(test)]
    async fn ensure_inference_ownership(&self) {
        if self.inference_ownership.lock().is_some() {
            return;
        }
        let ownership = codewhale_engine::client::acquire_runtime_chat_inference_ownership().await;
        let mut current = self.inference_ownership.lock();
        if current.is_none() {
            *current = Some(ownership);
        }
    }

    fn try_ensure_inference_ownership(&self) -> Result<(), String> {
        if self.inference_ownership.lock().is_some() {
            return Ok(());
        }
        let ownership = codewhale_engine::client::try_acquire_runtime_chat_inference_ownership()
            .ok_or_else(|| {
                "Finish the active local provider work before starting Runtime Chat.".to_string()
            })?;
        let mut current = self.inference_ownership.lock();
        if current.is_none() {
            *current = Some(ownership);
        }
        Ok(())
    }

    pub(crate) fn recover_inference_ownership_for_pending_delivery(&self) -> Result<(), String> {
        if self.inference_ownership.lock().is_some() {
            return Ok(());
        }
        let ownership = codewhale_engine::client::try_acquire_runtime_chat_inference_ownership()
            .ok_or_else(|| {
                "Finish the active local turn before recovering Runtime Chat delivery.".to_string()
            })?;
        *self.inference_ownership.lock() = Some(ownership);
        Ok(())
    }

    pub(crate) fn release_inference_ownership_if_settled(&self) {
        if !self.has_any_unsettled_turns() {
            self.inference_ownership.lock().take();
        }
    }

    pub(crate) fn is_exact_prompt_replay(
        &self,
        command: &RuntimeChatPrompt,
    ) -> Result<bool, String> {
        command.validate_shape()?;
        let operation_fingerprint = fingerprint(&command.operation_key);
        let request_fingerprint = runtime_chat_request_fingerprint(command)?;
        let state = self.state.lock();
        let by_binding = state
            .bindings
            .iter()
            .position(|binding| binding.runtime_binding_id == command.runtime_binding_id);
        let by_thread = state
            .bindings
            .iter()
            .position(|binding| binding.virtual_thread_id == command.runtime_thread_id);
        let binding = match (by_binding, by_thread) {
            (None, None) => return Ok(false),
            (Some(binding_index), Some(thread_index)) if binding_index == thread_index => {
                &state.bindings[binding_index]
            }
            _ => {
                return Err(
                    "The Runtime Chat thread authority does not match its binding.".to_string(),
                );
            }
        };
        if binding.run_id != command.run_id {
            return Err("The Runtime Chat replay belongs to another run.".to_string());
        }
        let Some(turn) = binding.turns.get(&command.turn_id) else {
            return Ok(false);
        };
        if turn.operation_fingerprint == operation_fingerprint
            && turn.request_fingerprint == request_fingerprint
        {
            return Ok(true);
        }
        Err("The Runtime Chat turn binding does not match its replay.".to_string())
    }

    pub(crate) fn scope_matches(&self, target_ref: &str, session_id: &str) -> bool {
        self.target_ref.as_str() == target_ref && self.session_id.as_str() == session_id
    }

    #[cfg(test)]
    pub(crate) fn configured_default_model_for_tests(&self) -> String {
        self.config.default_model()
    }

    #[cfg(test)]
    pub(crate) fn inference_ownership_is_held_for_tests(&self) -> bool {
        self.inference_ownership.lock().is_some()
    }

    #[cfg(test)]
    pub(crate) async fn acquire_inference_ownership_for_tests(&self) {
        self.ensure_inference_ownership().await;
    }

    #[cfg(test)]
    pub(crate) fn install_unsettled_turn_for_tests(
        &self,
        run_id: &str,
        native_thread_id: &str,
        virtual_thread_id: &str,
        virtual_turn_id: &str,
    ) -> Result<(), String> {
        self.insert_binding(RelayThreadBinding {
            run_id: run_id.to_string(),
            runtime_binding_id: "binding_test_fixture".to_string(),
            virtual_thread_id: virtual_thread_id.to_string(),
            native_thread_id: native_thread_id.to_string(),
            model: "model-1".to_string(),
            model_provider: "custom".to_string(),
            model_provider_id: "local-provider".to_string(),
            first_operation_fingerprint: fingerprint("operation-test-fixture"),
            system_prompt_fingerprint: None,
            turns: BTreeMap::from([(
                virtual_turn_id.to_string(),
                RelayTurnBinding {
                    review_request_fingerprint: None,
                    native_turn_id: "turn_test_fixture".to_string(),
                    operation_fingerprint: fingerprint("operation-test-fixture"),
                    request_fingerprint: fingerprint("request-test-fixture"),
                    terminal_projected: false,
                    start_rejected: false,
                },
            )]),
            projected_native_seq: 0,
        })
    }

    #[cfg(test)]
    pub(crate) async fn install_prompt_replay_for_tests(
        &self,
        command: &RuntimeChatPrompt,
        terminal_projected: bool,
    ) -> Result<(), String> {
        command.validate_shape()?;
        let operation_fingerprint = fingerprint(&command.operation_key);
        let request_fingerprint = runtime_chat_request_fingerprint(command)?;
        let native_thread_id = self
            .manager
            .create_thread(CreateThreadRequest {
                parent_thread_id: None,
                operation_key: None,
                model: Some(command.model.clone()),
                model_provider: Some(command.model_provider.clone()),
                model_provider_id: Some(command.model_provider_id.clone()),
                reasoning_effort: command.reasoning_effort.clone(),
                allowed_tools: Some(Vec::new()),
                workspace: None,
                mode: Some("agent".to_string()),
                permission_posture: Some("ask".to_string()),
                allow_shell: Some(false),
                trust_mode: Some(false),
                auto_approve: Some(false),
                archived: false,
                system_prompt: Some(dedicated_chat_system_prompt(
                    command.system_prompt.as_deref(),
                )),
                task_id: None,
                dynamic_tools: Vec::new(),
                environments: Vec::new(),
            })
            .await
            .map_err(|_| "Runtime Chat could not create its replay fixture.".to_string())?
            .id;
        let native_turn_id = reserved_native_turn_id(
            &native_thread_id,
            &command.runtime_binding_id,
            &command.turn_id,
            &operation_fingerprint,
        );
        self.insert_binding(RelayThreadBinding {
            run_id: command.run_id.clone(),
            runtime_binding_id: command.runtime_binding_id.clone(),
            virtual_thread_id: command.runtime_thread_id.clone(),
            native_thread_id,
            model: command.model.clone(),
            model_provider: command.model_provider.clone(),
            model_provider_id: command.model_provider_id.clone(),
            first_operation_fingerprint: operation_fingerprint.clone(),
            system_prompt_fingerprint: command.system_prompt.as_deref().map(fingerprint),
            turns: BTreeMap::from([(
                command.turn_id.clone(),
                RelayTurnBinding {
                    review_request_fingerprint: command
                        .structured_review
                        .as_ref()
                        .map(codewhale_engine::core::engine::review::fingerprint)
                        .transpose()
                        .map_err(|error| error.to_string())?,
                    native_turn_id,
                    operation_fingerprint,
                    request_fingerprint,
                    terminal_projected,
                    start_rejected: false,
                },
            )]),
            projected_native_seq: if terminal_projected { 1 } else { 0 },
        })
    }

    fn has_unsettled_turns_for_run(&self, run_id: &str) -> bool {
        self.state.lock().bindings.iter().any(|binding| {
            binding.run_id == run_id && binding.turns.values().any(|turn| !turn.terminal_projected)
        })
    }

    pub(crate) async fn apply_prompt(&self, command: &RuntimeChatPrompt) -> Result<(), String> {
        self.ensure_account_bound()?;
        if self.authorized_run_id.lock().as_deref() != Some(command.run_id.as_str()) {
            return Err("The Runtime Chat command is not authorized for this run.".to_string());
        }
        let _apply = self.apply_lock.lock().await;
        command.validate_shape()?;
        let operation_fingerprint = fingerprint(&command.operation_key);
        let request_fingerprint = runtime_chat_request_fingerprint(command)?;
        let system_prompt_fingerprint = command.system_prompt.as_deref().map(fingerprint);
        let review_request_fingerprint = command
            .structured_review
            .as_ref()
            .map(codewhale_engine::core::engine::review::fingerprint)
            .transpose()
            .map_err(|error| error.to_string())?;

        let existing = {
            let state = self.state.lock();
            let by_binding = state
                .bindings
                .iter()
                .position(|binding| binding.runtime_binding_id == command.runtime_binding_id);
            let by_thread = state
                .bindings
                .iter()
                .position(|binding| binding.virtual_thread_id == command.runtime_thread_id);
            match (by_binding, by_thread) {
                (None, None) => None,
                (Some(binding_index), Some(thread_index)) if binding_index == thread_index => {
                    Some(state.bindings[binding_index].clone())
                }
                _ => {
                    return Err(
                        "The Runtime Chat thread authority does not match its binding.".to_string(),
                    );
                }
            }
        };
        let exact_operation_replay = existing.as_ref().is_some_and(|binding| {
            binding.turns.get(&command.turn_id).is_some_and(|turn| {
                turn.operation_fingerprint == operation_fingerprint
                    && turn.request_fingerprint == request_fingerprint
            })
        });
        if !exact_operation_replay {
            self.validate_route(command)?;
        }
        if !exact_operation_replay && self.has_unsettled_turns_for_run(&command.run_id) {
            return Err(
                "Finish or interrupt the active Runtime Chat turn before starting another."
                    .to_string(),
            );
        }

        let binding = if let Some(binding) = existing {
            validate_existing_binding(
                &binding,
                command,
                &operation_fingerprint,
                system_prompt_fingerprint.as_deref(),
            )?;
            self.manager
                .get_thread(&binding.native_thread_id)
                .await
                .map_err(|_| "The isolated Runtime Chat thread is unavailable.".to_string())?;
            binding
        } else {
            let thread = self
                .manager
                .create_thread(CreateThreadRequest {
                    operation_key: None,
                    model: Some(command.model.clone()),
                    model_provider: Some(command.model_provider.clone()),
                    model_provider_id: Some(command.model_provider_id.clone()),
                    reasoning_effort: command.reasoning_effort.clone(),
                    allowed_tools: Some(Vec::new()),
                    workspace: None,
                    // The native engine's existing Act loop executes a pure
                    // chat turn once its model-visible tool catalog is empty.
                    // `chat` remains the account wire mode, not a second loop.
                    mode: Some("agent".to_string()),
                    permission_posture: Some("ask".to_string()),
                    allow_shell: Some(false),
                    trust_mode: Some(false),
                    auto_approve: Some(false),
                    archived: false,
                    system_prompt: Some(dedicated_chat_system_prompt(
                        command.system_prompt.as_deref(),
                    )),
                    task_id: None,
                    parent_thread_id: None,
                    dynamic_tools: Vec::new(),
                    environments: Vec::new(),
                })
                .await
                .map_err(|_| {
                    "Runtime Chat could not create an isolated native thread.".to_string()
                })?;
            let binding = RelayThreadBinding {
                runtime_binding_id: command.runtime_binding_id.clone(),
                run_id: command.run_id.clone(),
                virtual_thread_id: command.runtime_thread_id.clone(),
                native_thread_id: thread.id,
                model: command.model.clone(),
                model_provider: command.model_provider.clone(),
                model_provider_id: command.model_provider_id.clone(),
                first_operation_fingerprint: operation_fingerprint.clone(),
                system_prompt_fingerprint,
                turns: BTreeMap::new(),
                projected_native_seq: 0,
            };
            if let Err(error) = self.insert_binding(binding.clone()) {
                let _ = self
                    .manager
                    .discard_empty_thread(&binding.native_thread_id)
                    .await;
                return Err(error);
            }
            binding
        };

        // Reject virtual-turn and operation-key drift before asking the native
        // manager to start anything. RuntimeThreadManager independently
        // enforces the same operation-key idempotency for crash replay.
        if let Some(existing_turn) = binding.turns.get(&command.turn_id)
            && (existing_turn.operation_fingerprint != operation_fingerprint
                || existing_turn.request_fingerprint != request_fingerprint)
        {
            return Err("The Runtime Chat turn binding does not match its replay.".to_string());
        }
        if binding.turns.iter().any(|(virtual_turn_id, turn)| {
            virtual_turn_id != &command.turn_id
                && turn.operation_fingerprint == operation_fingerprint
        }) {
            return Err("The Runtime Chat operation key belongs to another turn.".to_string());
        }

        // Preallocate and persist the exact native id before the manager may
        // submit anything to a provider. RuntimeThreadManager binds the same id
        // inside its operation-key transaction, eliminating the crash window
        // between native acceptance and relay correlation.
        let reserved_native_turn_id = reserved_native_turn_id(
            &binding.native_thread_id,
            &command.runtime_binding_id,
            &command.turn_id,
            &operation_fingerprint,
        );
        let reservation = self.reserve_turn(
            &command.runtime_binding_id,
            &command.runtime_thread_id,
            &command.turn_id,
            &reserved_native_turn_id,
            &operation_fingerprint,
            &request_fingerprint,
            review_request_fingerprint.as_deref(),
        )?;
        if matches!(reservation, TurnReservationDisposition::ExistingTerminal) {
            return Ok(());
        }

        // The exclusive provider-request lease is acquired only after all
        // command/route/binding validation and deterministic reservation are
        // durable, but before native Runtime can resolve Auto or dispatch any
        // provider request. At the common client seam it also blocks advisor,
        // detached-subagent, compaction, purge, and interactive requests that
        // could otherwise feed this attached CWC run.
        if let Err(error) = self.try_ensure_inference_ownership() {
            if !matches!(reservation, TurnReservationDisposition::ExistingUnsettled) {
                self.finish_turn_reservation(
                    &command.runtime_binding_id,
                    &command.runtime_thread_id,
                    &command.turn_id,
                )?;
            }
            self.release_inference_ownership_if_settled();
            return Err(error);
        }

        let admission = if let Some(review) = &command.structured_review {
            self.manager
                .start_review_turn_with_reserved_id(
                    &binding.native_thread_id,
                    codewhale_engine::runtime_threads::manager::StartReviewRequest {
                        review: review.clone(),
                        operation_key: command.operation_key.clone(),
                        account_model_owner: command.account_model_owner.clone(),
                        model: command.model.clone(),
                        model_provider: None,
                        model_provider_id: None,
                        reasoning_effort: command.reasoning_effort.clone(),
                        max_output_tokens: command
                            .max_output_tokens
                            .ok_or_else(|| "Review output allowance is required.".to_string())?,
                    },
                    &reserved_native_turn_id,
                )
                .await
        } else {
            self.manager
                .start_turn_with_reserved_id(
                    &binding.native_thread_id,
                    StartTurnRequest {
                        account_model_owner: command.account_model_owner.clone(),
                        profile_constitution: command.profile_constitution.clone(),
                        expected_workspace: None,
                        max_output_tokens: command.max_output_tokens,
                        prompt: command.prompt.clone(),
                        images: command.images.clone(),
                        operation_key: Some(command.operation_key.clone()),
                        input_summary: None,
                        model: Some(command.model.clone()),
                        reasoning_effort: command.reasoning_effort.clone(),
                        allowed_tools: Some(Vec::new()),
                        mode: Some("agent".to_string()),
                        permission_posture: Some("ask".to_string()),
                        allow_shell: Some(false),
                        trust_mode: Some(false),
                        auto_approve: Some(false),
                        dynamic_tools: Vec::new(),
                        environment_id: None,
                        model_provider: None,
                        model_provider_id: None,
                    },
                    &reserved_native_turn_id,
                )
                .await
        };
        let turn = match admission {
            Ok(turn) => turn,
            Err(error) => {
                // A process may crash after the relay reservation is durable
                // but before Runtime persists its operation binding/turn. On
                // exact replay that appears as ExistingUnsettled. Settle it
                // only when the native thread snapshot positively proves the
                // deterministic turn id was never accepted; any unreadable or
                // present native record stays fail-closed and projectable.
                let native_turn_is_durable = self
                    .manager
                    .get_thread_detail(&binding.native_thread_id)
                    .await
                    .map(|detail| {
                        detail
                            .turns
                            .iter()
                            .any(|turn| turn.id == reserved_native_turn_id)
                    })
                    .unwrap_or(true);
                if !native_turn_is_durable {
                    self.finish_turn_reservation(
                        &command.runtime_binding_id,
                        &command.runtime_thread_id,
                        &command.turn_id,
                    )?;
                }
                self.release_inference_ownership_if_settled();
                return Err(sanitized_runtime_error("start", &error));
            }
        };
        debug_assert_eq!(turn.id, reserved_native_turn_id);
        Ok(())
    }

    pub(crate) async fn interrupt(
        &self,
        run_id: &str,
        scope: &RuntimeChatControlScope,
        virtual_turn_id: &str,
    ) -> Result<(), String> {
        self.ensure_account_bound()?;
        validate_relay_id(run_id, "run id")
            .map_err(|_| "The Runtime Chat interrupt run is invalid.".to_string())?;
        validate_relay_id(&scope.runtime_binding_id, "binding id")
            .map_err(|_| "The Runtime Chat interrupt binding is invalid.".to_string())?;
        validate_virtual_thread_id(&scope.runtime_thread_id)
            .map_err(|_| "The Runtime Chat interrupt thread is invalid.".to_string())?;
        validate_virtual_turn_id(virtual_turn_id)
            .map_err(|_| "The Runtime Chat interrupt turn is invalid.".to_string())?;
        let (native_thread_id, native_turn_id) = {
            let state = self.state.lock();
            resolve_interrupt_target(&state, run_id, scope, virtual_turn_id)?
        };

        let detail = self
            .manager
            .get_thread_detail(&native_thread_id)
            .await
            .map_err(|_| "The isolated Runtime Chat thread is unavailable.".to_string())?;
        let turn = detail
            .turns
            .iter()
            .find(|turn| turn.id == native_turn_id)
            .ok_or_else(|| "The isolated Runtime Chat turn is unavailable.".to_string())?;
        if !matches!(
            turn.status,
            RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress
        ) {
            // Exact replay after the terminal boundary is already applied.
            return Ok(());
        }
        self.manager
            .interrupt_turn(&native_thread_id, &native_turn_id)
            .await
            .map(|_| ())
            .map_err(|error| sanitized_runtime_error("interrupt", &error))
    }

    /// Return at most one not-yet-journaled projection per poll. The caller
    /// journals it before advancing `projected_native_seq`, so a crash can
    /// cause a safe replay but never silently lose an accepted native event.
    pub(crate) async fn pending_projections(&self) -> Result<Vec<RuntimeChatProjection>, String> {
        let Some(authorized_run_id) = self.authorized_run_id.lock().clone() else {
            return Ok(Vec::new());
        };
        let bindings = {
            let state = self.state.lock();
            if state.owner_scope_fingerprint.is_none() {
                return Ok(Vec::new());
            }
            state
                .bindings
                .iter()
                .filter(|binding| binding.run_id == authorized_run_id)
                .cloned()
                .collect::<Vec<_>>()
        };
        for binding in bindings {
            let events = self
                .manager
                .events_since_async(
                    &binding.native_thread_id,
                    Some(binding.projected_native_seq),
                )
                .await
                .map_err(|_| "Runtime Chat could not read its native event ledger.".to_string())?;
            for event in events {
                let key = (binding.native_thread_id.clone(), event.seq);
                if self.claimed_projections.lock().contains(&key) {
                    break;
                }
                match project_native_event(&binding, &event) {
                    ProjectionDecision::Project(projection) => {
                        self.claimed_projections.lock().insert(key);
                        // Return immediately after acquiring the claim. No
                        // later binding read can fail and accidentally drop a
                        // previously acquired in-process claim.
                        return Ok(vec![*projection]);
                    }
                    ProjectionDecision::Skip => {
                        self.advance_projection_cursor(&binding.native_thread_id, event.seq, None)?;
                    }
                    ProjectionDecision::WaitForTurnBinding => break,
                }
            }
        }
        Ok(Vec::new())
    }

    pub(crate) fn mark_projected(
        &self,
        native_thread_id: &str,
        native_seq: u64,
        virtual_turn_id: &str,
        event: &str,
    ) -> Result<(), String> {
        self.claimed_projections
            .lock()
            .remove(&(native_thread_id.to_string(), native_seq));
        self.advance_projection_cursor(
            native_thread_id,
            native_seq,
            (event == "turn.completed").then_some(virtual_turn_id),
        )
    }

    pub(crate) fn release_projection(&self, native_thread_id: &str, native_seq: u64) {
        self.claimed_projections
            .lock()
            .remove(&(native_thread_id.to_string(), native_seq));
    }

    pub(crate) fn release_all_projection_claims(&self) {
        self.claimed_projections.lock().clear();
    }

    #[cfg(test)]
    pub(crate) fn install_projection_claim_for_tests(
        &self,
        native_thread_id: &str,
        native_seq: u64,
    ) {
        self.claimed_projections
            .lock()
            .insert((native_thread_id.to_string(), native_seq));
    }

    #[cfg(test)]
    pub(crate) fn projection_is_claimed_for_tests(
        &self,
        native_thread_id: &str,
        native_seq: u64,
    ) -> bool {
        self.claimed_projections
            .lock()
            .contains(&(native_thread_id.to_string(), native_seq))
    }

    #[cfg(test)]
    async fn native_turn_count_for_binding_for_tests(&self, runtime_binding_id: &str) -> usize {
        let native_thread_id = self
            .state
            .lock()
            .bindings
            .iter()
            .find(|binding| binding.runtime_binding_id == runtime_binding_id)
            .map(|binding| binding.native_thread_id.clone())
            .expect("test binding exists");
        self.manager
            .get_thread_detail(&native_thread_id)
            .await
            .expect("test native thread detail")
            .turns
            .len()
    }

    fn validate_route(&self, command: &RuntimeChatPrompt) -> Result<(), String> {
        if let Some(owner) = &command.account_model_owner {
            let expected = owner_scope_fingerprint(owner, &self.target_ref, &self.session_id);
            if self.state.lock().owner_scope_fingerprint.as_deref() != Some(expected.as_str()) {
                return Err("The agent model belongs to another account.".to_string());
            }
            let mut config = (*self.config).clone();
            return config
                .bind_account_model_owner(owner)
                .map_err(|error| error.to_string());
        }
        let catalog = self.catalog("a2345678901234567890123456789012")?;
        let provider = catalog
            .get("providers")
            .and_then(Value::as_array)
            .and_then(|providers| providers.first())
            .ok_or_else(|| "The active Runtime Chat route is unavailable.".to_string())?;
        let route_matches = provider.get("id").and_then(Value::as_str)
            == Some(command.model_provider.as_str())
            && provider.get("modelProviderId").and_then(Value::as_str)
                == Some(command.model_provider_id.as_str())
            && provider
                .get("models")
                .and_then(Value::as_array)
                .is_some_and(|models| {
                    models.iter().any(|model| {
                        model.get("id").and_then(Value::as_str) == Some(command.model.as_str())
                            && (command.images.is_empty()
                                || model.get("imageInput").and_then(Value::as_str)
                                    == Some("supported"))
                    })
                });
        if command.max_output_tokens.is_some()
            && !provider
                .get("models")
                .and_then(Value::as_array)
                .is_some_and(|models| {
                    models.iter().any(|model| {
                        model.get("id").and_then(Value::as_str) == Some(command.model.as_str())
                            && model.get("outputTokenLimit").and_then(Value::as_str)
                                == Some("supported")
                    })
                })
        {
            return Err(
                "The selected Runtime Chat route does not support maxOutputTokens.".to_string(),
            );
        }
        if !route_matches {
            return Err(
                "The requested Runtime Chat route is not the active ready route.".to_string(),
            );
        }
        Ok(())
    }

    fn insert_binding(&self, binding: RelayThreadBinding) -> Result<(), String> {
        self.persist_state_update(
            "Runtime Chat could not persist its private thread binding.",
            |state| {
                if state.owner_scope_fingerprint.is_none() {
                    return Err("The Runtime Chat account owner is not established.".to_string());
                }
                if state.bindings.iter().any(|existing| {
                    existing.runtime_binding_id == binding.runtime_binding_id
                        || existing.virtual_thread_id == binding.virtual_thread_id
                }) {
                    return Err(
                        "The Runtime Chat thread binding changed while it was being created."
                            .to_string(),
                    );
                }
                state.bindings.push(binding);
                Ok(())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn reserve_turn(
        &self,
        runtime_binding_id: &str,
        virtual_thread_id: &str,
        virtual_turn_id: &str,
        native_turn_id: &str,
        operation_fingerprint: &str,
        request_fingerprint: &str,
        review_request_fingerprint: Option<&str>,
    ) -> Result<TurnReservationDisposition, String> {
        validate_native_record_id(native_turn_id, "native turn id")
            .map_err(public_validation_error)?;
        validate_fingerprint(request_fingerprint).map_err(public_validation_error)?;
        if let Some(fingerprint) = review_request_fingerprint {
            validate_fingerprint(fingerprint).map_err(public_validation_error)?;
        }
        self.persist_state_update(
            "Runtime Chat could not persist its private turn reservation.",
            |state| {
                let binding = state
                    .bindings
                    .iter_mut()
                    .find(|binding| {
                        binding.runtime_binding_id == runtime_binding_id
                            && binding.virtual_thread_id == virtual_thread_id
                    })
                    .ok_or_else(|| "The Runtime Chat thread binding disappeared.".to_string())?;
                if let Some(existing) = binding.turns.get_mut(virtual_turn_id) {
                    if existing.operation_fingerprint == operation_fingerprint
                        && existing.request_fingerprint == request_fingerprint
                        && existing.native_turn_id == native_turn_id
                        && existing.review_request_fingerprint.as_deref()
                            == review_request_fingerprint
                    {
                        // A retry after a proven pre-dispatch failure reopens
                        // the same deterministic reservation. Any accepted or
                        // replayed native turn is therefore unsettled again
                        // until its terminal event is durably projected.
                        let disposition = if existing.terminal_projected && existing.start_rejected
                        {
                            TurnReservationDisposition::Reopened
                        } else if existing.terminal_projected {
                            TurnReservationDisposition::ExistingTerminal
                        } else {
                            TurnReservationDisposition::ExistingUnsettled
                        };
                        if matches!(disposition, TurnReservationDisposition::Reopened) {
                            existing.terminal_projected = false;
                            existing.start_rejected = false;
                        }
                        return Ok(disposition);
                    }
                    return Err(
                        "The Runtime Chat turn binding does not match its replay.".to_string()
                    );
                }
                if binding
                    .turns
                    .values()
                    .any(|turn| turn.operation_fingerprint == operation_fingerprint)
                {
                    return Err(
                        "The Runtime Chat operation key belongs to another turn.".to_string()
                    );
                }
                binding.turns.insert(
                    virtual_turn_id.to_string(),
                    RelayTurnBinding {
                        review_request_fingerprint: review_request_fingerprint.map(str::to_string),
                        native_turn_id: native_turn_id.to_string(),
                        operation_fingerprint: operation_fingerprint.to_string(),
                        request_fingerprint: request_fingerprint.to_string(),
                        terminal_projected: false,
                        start_rejected: false,
                    },
                );
                Ok(TurnReservationDisposition::New)
            },
        )
    }

    fn finish_turn_reservation(
        &self,
        runtime_binding_id: &str,
        virtual_thread_id: &str,
        virtual_turn_id: &str,
    ) -> Result<(), String> {
        self.persist_state_update(
            "Runtime Chat could not settle its rejected turn reservation.",
            |state| {
                let turn = state
                    .bindings
                    .iter_mut()
                    .find(|binding| {
                        binding.runtime_binding_id == runtime_binding_id
                            && binding.virtual_thread_id == virtual_thread_id
                    })
                    .and_then(|binding| binding.turns.get_mut(virtual_turn_id))
                    .ok_or_else(|| "The Runtime Chat turn reservation disappeared.".to_string())?;
                turn.terminal_projected = true;
                turn.start_rejected = true;
                Ok(())
            },
        )
    }

    fn advance_projection_cursor(
        &self,
        native_thread_id: &str,
        native_seq: u64,
        terminal_virtual_turn_id: Option<&str>,
    ) -> Result<(), String> {
        self.persist_state_update(
            "Runtime Chat could not persist its event cursor.",
            |state| {
                let binding = state
                    .bindings
                    .iter_mut()
                    .find(|binding| binding.native_thread_id == native_thread_id)
                    .ok_or_else(|| "The Runtime Chat projection binding is unknown.".to_string())?;
                binding.projected_native_seq = binding.projected_native_seq.max(native_seq);
                if let Some(virtual_turn_id) = terminal_virtual_turn_id {
                    let turn = binding.turns.get_mut(virtual_turn_id).ok_or_else(|| {
                        "The Runtime Chat terminal projection has no turn binding.".to_string()
                    })?;
                    turn.terminal_projected = true;
                    turn.start_rejected = false;
                }
                Ok(())
            },
        )
    }

    fn persist_state_update<T>(
        &self,
        persistence_error: &'static str,
        update: impl FnOnce(&mut RelayState) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut current = self.state.lock();
        let mut candidate = current.clone();
        let output = update(&mut candidate)?;
        persist_state(&self.state_path, &candidate).map_err(|_| persistence_error.to_string())?;
        *current = candidate;
        Ok(output)
    }

    fn ensure_account_bound(&self) -> Result<(), String> {
        if self.state.lock().owner_scope_fingerprint.is_none() {
            return Err("The Runtime Chat account owner is not established.".to_string());
        }
        Ok(())
    }
}

impl RuntimeChatPrompt {
    pub(crate) fn validate_shape(&self) -> Result<(), String> {
        if let Some(review) = &self.structured_review {
            codewhale_engine::core::engine::review::validate(review)
                .map_err(|error| error.to_string())?;
            if self.account_model_owner.is_none()
                || !self.prompt.is_empty()
                || !self.images.is_empty()
                || self.profile_constitution.is_some()
                || self.system_prompt.is_some()
                || !self
                    .max_output_tokens
                    .is_some_and(|value| (256..=8192).contains(&value.get()))
            {
                return Err("A native review requires an account model and bounded output, with no Chat prompt, images or caller instructions.".to_string());
            }
        }
        if let Some(owner) = &self.account_model_owner {
            validate_owner_component(owner, "account")?;
            if self.model_provider != "codewhale"
                || self.model_provider_id != "codewhale"
                || !self.model.contains('/')
                || self.model.starts_with("codewhale/")
                || !self.images.is_empty()
                || self.reasoning_effort.is_some()
            {
                return Err(
                    "The agent model requires an exact account provider and model.".to_string(),
                );
            }
        }
        codewhale_engine::image_attach::prepare_runtime_images(&self.images)
            .map_err(|error| error.to_string())?;
        if !self.images.is_empty() && self.model.trim().eq_ignore_ascii_case("auto") {
            return Err(
                "Image inputs require an exact named model; Auto is unavailable for images."
                    .to_string(),
            );
        }
        if self.command_type != "prompt.request" {
            return Err("Codewhale sent an unsupported Runtime Chat command.".to_string());
        }
        validate_relay_id(&self.run_id, "run id").map_err(public_validation_error)?;
        validate_virtual_turn_id(&self.turn_id).map_err(public_validation_error)?;
        validate_operation_key(&self.operation_key).map_err(public_validation_error)?;
        validate_relay_id(&self.runtime_binding_id, "binding id")
            .map_err(public_validation_error)?;
        validate_virtual_thread_id(&self.runtime_thread_id).map_err(public_validation_error)?;
        validate_model_id(&self.model).map_err(public_validation_error)?;
        validate_route_id(&self.model_provider, "provider id").map_err(public_validation_error)?;
        validate_route_id(&self.model_provider_id, "model-provider id")
            .map_err(public_validation_error)?;
        if self.structured_review.is_none()
            && (self.prompt.trim().is_empty()
                || self.prompt.len() > 128 * 1024
                || self.prompt.contains('\0'))
        {
            return Err("The Runtime Chat prompt is empty or oversized.".to_string());
        }
        if let Some(snapshot) = &self.profile_constitution {
            snapshot.validate().map_err(|error| error.to_string())?;
            if self
                .account_model_owner
                .as_ref()
                .is_some_and(|owner| &snapshot.account_id != owner)
            {
                return Err(
                    "The profile constitution and agent model must belong to the same account."
                        .to_string(),
                );
            }
        }
        if let Some(system_prompt) = self.system_prompt.as_deref()
            && (system_prompt.trim().is_empty()
                || system_prompt.len() > 64_000
                || system_prompt.contains('\0'))
        {
            return Err("The Runtime Chat system instructions are invalid.".to_string());
        }
        if !self.allowed_tools.is_empty() {
            return Err("Runtime-backed Chat does not grant work-execution tools.".to_string());
        }
        if self.mode != "chat" || self.requested_mode != "chat" {
            return Err("Runtime relay turns must use Chat mode.".to_string());
        }
        if let Some(reasoning) = self.reasoning_effort.as_deref()
            && codewhale_engine::reasoning_preference::ReasoningEffort::parse_strict(reasoning)
                .is_err()
        {
            return Err("The Runtime Chat reasoning effort is invalid.".to_string());
        }
        validate_relay_id(&self.workspace.id, "workspace id").map_err(public_validation_error)?;
        validate_relay_id(&self.workspace.target_ref, "target reference")
            .map_err(public_validation_error)?;
        Ok(())
    }
}

impl RuntimeChatControlScope {
    pub(crate) fn validate_for_turn(&self, virtual_turn_id: &str) -> Result<(), String> {
        validate_relay_id(&self.runtime_binding_id, "binding id")
            .map_err(public_validation_error)?;
        validate_virtual_thread_id(&self.runtime_thread_id).map_err(public_validation_error)?;
        validate_virtual_turn_id(virtual_turn_id).map_err(public_validation_error)
    }
}

fn validate_existing_binding(
    binding: &RelayThreadBinding,
    command: &RuntimeChatPrompt,
    operation_fingerprint: &str,
    system_prompt_fingerprint: Option<&str>,
) -> Result<(), String> {
    if binding.model != command.model
        || binding.run_id != command.run_id
        || binding.model_provider != command.model_provider
        || binding.model_provider_id != command.model_provider_id
    {
        return Err("The Runtime Chat thread is pinned to a different provider route.".to_string());
    }
    if command.system_prompt.is_some()
        && (binding.first_operation_fingerprint != operation_fingerprint
            || binding.system_prompt_fingerprint.as_deref() != system_prompt_fingerprint)
    {
        return Err(
            "Runtime Chat system instructions are allowed only on the first turn.".to_string(),
        );
    }
    Ok(())
}

fn resolve_interrupt_target(
    state: &RelayState,
    run_id: &str,
    scope: &RuntimeChatControlScope,
    virtual_turn_id: &str,
) -> Result<(String, String), String> {
    let binding = state
        .bindings
        .iter()
        .find(|binding| {
            binding.runtime_binding_id == scope.runtime_binding_id
                && binding.virtual_thread_id == scope.runtime_thread_id
        })
        .ok_or_else(|| "The Runtime Chat interrupt binding is unknown.".to_string())?;
    if binding.run_id != run_id {
        return Err("The Runtime Chat interrupt binding belongs to another run.".to_string());
    }
    let turn = binding
        .turns
        .get(virtual_turn_id)
        .ok_or_else(|| "The Runtime Chat interrupt turn is unknown.".to_string())?;
    Ok((
        binding.native_thread_id.clone(),
        turn.native_turn_id.clone(),
    ))
}

pub(crate) fn dedicated_chat_system_prompt(account_instructions: Option<&str>) -> String {
    match account_instructions
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(instructions) => format!(
            "{ISOLATED_CHAT_SYSTEM_PROMPT}\n\n<account_chat_instructions>\n{instructions}\n</account_chat_instructions>"
        ),
        None => ISOLATED_CHAT_SYSTEM_PROMPT.to_string(),
    }
}

enum ProjectionDecision {
    Project(Box<RuntimeChatProjection>),
    Skip,
    WaitForTurnBinding,
}

fn project_native_event(
    binding: &RelayThreadBinding,
    event: &RuntimeEventRecord,
) -> ProjectionDecision {
    let Some(native_turn_id) = event.turn_id.as_deref() else {
        return ProjectionDecision::Skip;
    };
    let Some((virtual_turn_id, turn_binding)) = binding
        .turns
        .iter()
        .find(|(_, turn)| turn.native_turn_id == native_turn_id)
    else {
        return ProjectionDecision::WaitForTurnBinding;
    };
    let base = || RuntimeChatProjection {
        run_id: binding.run_id.clone(),
        native_thread_id: binding.native_thread_id.clone(),
        native_seq: event.seq,
        source_event_id: source_event_id(&binding.native_thread_id, event.seq),
        virtual_thread_id: binding.virtual_thread_id.clone(),
        virtual_turn_id: virtual_turn_id.clone(),
        event: "",
        timestamp: event.timestamp.to_rfc3339(),
        payload: Value::Null,
    };
    match event.event.as_str() {
        "turn.started" => {
            let mut projection = base();
            projection.event = "turn.started";
            projection.payload = json!({
                "turn": {
                    "model": event.payload.pointer("/turn/effective_model")
                        .and_then(Value::as_str)
                        .unwrap_or(binding.model.as_str()),
                    "mode": "chat",
                },
            });
            ProjectionDecision::Project(Box::new(projection))
        }
        "item.delta"
            if event.payload.get("kind").and_then(Value::as_str) == Some("agent_message") =>
        {
            let Some(delta) = event.payload.get("delta").and_then(Value::as_str) else {
                return ProjectionDecision::Skip;
            };
            let mut projection = base();
            projection.event = "item.delta";
            projection.payload = json!({ "kind": "agent_message", "delta": delta });
            ProjectionDecision::Project(Box::new(projection))
        }
        "item.completed" => {
            let Some(expected_fingerprint) = turn_binding.review_request_fingerprint.as_deref()
            else {
                return ProjectionDecision::Skip;
            };
            let Some(item) = event.payload.get("item") else {
                return ProjectionDecision::Skip;
            };
            let Some(metadata) = item.get("metadata") else {
                return ProjectionDecision::Skip;
            };
            let Some(operation) = metadata.get("review_operation") else {
                return ProjectionDecision::Skip;
            };
            if item.get("kind").and_then(Value::as_str) != Some("tool_call")
                || item.get("status").and_then(Value::as_str) != Some("completed")
                || metadata.get("tool_name").and_then(Value::as_str) != Some("review")
                || metadata.get("is_error").and_then(Value::as_bool) != Some(false)
                || !metadata
                    .get("tool_result_for")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
                || operation.get("version").and_then(Value::as_u64) != Some(1)
                || operation.get("source").and_then(Value::as_str) != Some("codewhale.review_core")
                || operation.get("request_fingerprint").and_then(Value::as_str)
                    != Some(expected_fingerprint)
                || operation.get("complete").and_then(Value::as_bool) != Some(true)
                || operation.get("outcome").and_then(Value::as_str) != Some("completed")
            {
                return ProjectionDecision::Skip;
            }
            let Some(detail) = item.get("detail").and_then(Value::as_str).filter(|value| {
                value.len() <= codewhale_engine::core::engine::review::MAX_RESULT_BYTES
            }) else {
                return ProjectionDecision::Skip;
            };
            let Ok(output) = serde_json::from_str::<Value>(detail) else {
                return ProjectionDecision::Skip;
            };
            let mut projection = base();
            projection.event = "review.completed";
            projection.payload = json!({ "review": { "operation": operation, "output": output } });
            ProjectionDecision::Project(Box::new(projection))
        }
        "turn.completed" => {
            let turn = event
                .payload
                .get("turn")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let status = turn
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("failed");
            let mut projection = base();
            projection.event = "turn.completed";
            projection.payload = json!({
                "turn": {
                    "status": status,
                    "usage": turn.get("usage").cloned().unwrap_or_else(|| json!({})),
                    "effective_model": turn.get("effective_model").and_then(Value::as_str)
                        .unwrap_or(binding.model.as_str()),
                    "effective_provider": turn.get("effective_provider").and_then(Value::as_str)
                        .unwrap_or(binding.model_provider.as_str()),
                    "effective_billing_surface": turn.get("effective_billing_surface")
                        .and_then(Value::as_str).unwrap_or("provider_byok"),
                    "effective_billing_mode": turn.get("effective_billing_mode")
                        .and_then(Value::as_str).unwrap_or("local"),
                },
            });
            ProjectionDecision::Project(Box::new(projection))
        }
        _ => ProjectionDecision::Skip,
    }
}

fn validate_owner_component(value: &str, label: &str) -> Result<(), String> {
    if value.trim() != value || value.is_empty() || value.len() > 512 || value.contains('\0') {
        return Err(format!("The Runtime Chat {label} owner is invalid."));
    }
    Ok(())
}

fn scoped_private_dir(root: &Path, target_ref: &str, session_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale.runtime-chat-scope.v1\0");
    hasher.update(target_ref.as_bytes());
    hasher.update(b"\0");
    hasher.update(session_id.as_bytes());
    root.join(format!("scope-{}", hex_digest(hasher.finalize())))
}

fn owner_scope_fingerprint(account_ref: &str, target_ref: &str, session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale.runtime-chat-owner.v1\0");
    hasher.update(account_ref.as_bytes());
    hasher.update(b"\0");
    hasher.update(target_ref.as_bytes());
    hasher.update(b"\0");
    hasher.update(session_id.as_bytes());
    hex_digest(hasher.finalize())
}

fn bind_owner_scope(state: &mut RelayState, owner: &str) -> Result<(), String> {
    validate_fingerprint(owner)
        .map_err(|_| "The Runtime Chat account owner is invalid.".to_string())?;
    match state.owner_scope_fingerprint.as_deref() {
        Some(existing) if existing == owner => Ok(()),
        Some(_) => Err("This Runtime Chat session belongs to another account.".to_string()),
        None if state.bindings.is_empty() => {
            state.owner_scope_fingerprint = Some(owner.to_string());
            Ok(())
        }
        None => Err("The saved Runtime Chat state has no trusted account owner.".to_string()),
    }
}

fn source_event_id(native_thread_id: &str, native_seq: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale.runtime-chat-source-event.v1\0");
    hasher.update(native_thread_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(native_seq.to_be_bytes());
    format!("native_event_{}", hex_digest(hasher.finalize()))
}

fn reserved_native_turn_id(
    native_thread_id: &str,
    runtime_binding_id: &str,
    virtual_turn_id: &str,
    operation_fingerprint: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale.runtime-chat-native-turn.v1\0");
    hasher.update(native_thread_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(runtime_binding_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(virtual_turn_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(operation_fingerprint.as_bytes());
    format!("turn_{}", hex_digest(hasher.finalize()))
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_json_value).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json_value(&values[key]));
            }
            Value::Object(canonical)
        }
        _ => value.clone(),
    }
}

fn isolated_chat_execution_config(
    config: &Config,
    private_dir: &Path,
) -> Result<(Config, PathBuf), String> {
    let workspace = private_dir.join("chat-workspace");
    let context_dir = private_dir.join("prompt-context");
    let skills_dir = context_dir.join("skills-empty");
    fs::create_dir_all(&workspace)
        .and_then(|()| fs::create_dir_all(&skills_dir))
        .map_err(|_| "Runtime Chat could not prepare its isolated Chat context.".to_string())?;
    #[cfg(unix)]
    {
        // A mount that assigns modes itself refuses every `chmod`; the context
        // directories are linked only from this session either way.
        for path in [&workspace, &context_dir, &skills_dir] {
            codewhale_config::private_directory::tighten_mode(path, 0o700).map_err(|_| {
                "Runtime Chat could not protect its isolated Chat context.".to_string()
            })?;
        }
    }

    let mut execution = config.clone();
    execution.runtime_chat_isolated = true;
    execution.skills_dir = Some(skills_dir.to_string_lossy().into_owned());
    execution.instructions = None;
    execution.notes_path = Some(
        context_dir
            .join("notes-disabled")
            .to_string_lossy()
            .into_owned(),
    );
    execution.mcp_config_path = Some(
        context_dir
            .join("mcp-disabled.json")
            .to_string_lossy()
            .into_owned(),
    );
    execution.memory = Some(MemoryConfig {
        enabled: Some(false),
        backend: Some(MemoryBackend::Off),
    });
    execution.memory_path = Some(
        context_dir
            .join("memory-disabled.md")
            .to_string_lossy()
            .into_owned(),
    );
    execution.context.project_pack = Some(false);
    execution
        .skills
        .get_or_insert_with(SkillsConfig::default)
        .scan_codewhale_only = Some(true);
    Ok((execution, workspace))
}

#[derive(Debug)]
struct RelayScopeLock {
    _file: File,
}

impl RelayScopeLock {
    fn acquire(path: &Path) -> Result<Self> {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            bail!("refusing a symlinked Runtime Chat owner lock");
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options.open(path).context("open Runtime Chat owner lock")?;
        if !file
            .metadata()
            .context("inspect Runtime Chat owner lock")?
            .file_type()
            .is_file()
        {
            bail!("Runtime Chat owner lock is not a regular file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .context("protect Runtime Chat owner lock")?;
        }
        // Same-process drop-then-reopen can observe WouldBlock for a brief
        // window while the previous fd is still closing (#5735). Retry only
        // that contention; a lock that stays held is still ownership.
        let deadline = Instant::now() + Duration::from_millis(25);
        loop {
            match Self::try_lock_exclusive(&file) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(error).context("acquire Runtime Chat owner lock");
                    }
                    std::thread::yield_now();
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => {
                    return Err(error).context("acquire Runtime Chat owner lock");
                }
            }
        }
        Ok(Self { _file: file })
    }

    fn try_lock_exclusive(file: &File) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: `file` owns a valid descriptor for the duration of the
            // call and remains alive in `Self` for the full lock lifetime.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Storage::FileSystem::LockFile;
            // SAFETY: `file` owns a valid handle that remains alive in `Self`.
            if unsafe { LockFile(file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            Ok(())
        }
    }
}

impl Drop for RelayScopeLock {
    fn drop(&mut self) {
        // close() also releases, but unlocking first lets a same-process
        // reopen proceed without racing the previous fd's teardown (#5735).
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: Drop runs only while `_file` still owns this descriptor.
            unsafe {
                libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Storage::FileSystem::UnlockFile;
            // SAFETY: Drop runs only while `_file` still owns this handle.
            unsafe {
                UnlockFile(self._file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX);
            }
        }
    }
}

fn load_state(path: &Path) -> Result<RelayState> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("decode Runtime Chat binding state"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RelayState::default()),
        Err(error) => Err(error).context("read Runtime Chat binding state"),
    }
}

fn persist_state(path: &Path, state: &RelayState) -> Result<()> {
    state.validate()?;
    #[cfg(test)]
    if take_state_persist_failure(path) {
        bail!("injected Runtime Chat state persistence failure");
    }
    let body = serde_json::to_vec(state).context("encode Runtime Chat binding state")?;
    codewhale_util::utils::write_atomic(path, &body).context("persist Runtime Chat binding state")
}

fn validate_relay_id(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_RELAY_ID_BYTES
        || value.contains("..")
        || value.contains("://")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'@' | b'/' | b'+' | b'~' | b'-')
        })
    {
        bail!("invalid Runtime Chat {label}");
    }
    Ok(())
}

fn validate_operation_key(value: &str) -> Result<()> {
    validate_relay_id(value, "operation key")?;
    if value.len() > MAX_OPERATION_KEY_BYTES {
        bail!("Runtime Chat operation key is too long");
    }
    Ok(())
}

fn validate_virtual_thread_id(value: &str) -> Result<()> {
    if value.len() != 37
        || !value.starts_with("local_thread_")
        || !value[13..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("invalid Runtime Chat virtual thread id");
    }
    Ok(())
}

fn validate_virtual_turn_id(value: &str) -> Result<()> {
    if value.len() != 35
        || !value.starts_with("local_turn_")
        || !value[11..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("invalid Runtime Chat virtual turn id");
    }
    Ok(())
}

fn validate_native_record_id(value: &str, label: &str) -> Result<()> {
    if value.len() < 5
        || value.len() > 80
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("invalid {label}");
    }
    Ok(())
}

fn validate_route_id(value: &str, label: &str) -> Result<()> {
    if !crate::runtime_api::runtime_chat_route_id_is_safe(value) {
        bail!("invalid Runtime Chat {label}");
    }
    Ok(())
}

fn validate_model_id(value: &str) -> Result<()> {
    if !crate::runtime_api::runtime_chat_model_id_is_safe(value) {
        bail!("invalid Runtime Chat model id");
    }
    Ok(())
}

fn validate_fingerprint(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid Runtime Chat fingerprint");
    }
    Ok(())
}

fn fingerprint(value: &str) -> String {
    hex_digest(Sha256::digest(value.as_bytes()))
}

fn runtime_chat_request_fingerprint(command: &RuntimeChatPrompt) -> Result<String, String> {
    serde_json::to_value(command)
        .map(|value| canonical_json_value(&value))
        .and_then(|value| serde_json::to_vec(&value))
        .map(|bytes| hex_digest(Sha256::digest(bytes)))
        .map_err(|_| "Runtime Chat could not fingerprint its validated request.".to_string())
}

fn public_validation_error(_error: anyhow::Error) -> String {
    "The Runtime Chat command contains an invalid opaque identity.".to_string()
}

fn sanitized_runtime_error(action: &str, error: &anyhow::Error) -> String {
    let text = error.to_string().to_ascii_lowercase();
    if text.contains("operation_key") || text.contains("operation key") {
        return "The Runtime Chat operation key does not match its original turn.".to_string();
    }
    format!("The isolated Runtime Chat turn could not {action}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::time::Duration;

    fn open_host(root: &Path) -> RuntimeChatRelayHost {
        RuntimeChatRelayHost::open(
            Config::default(),
            Arc::new(PluginRegistry::empty(root)),
            root.to_path_buf(),
            "target_fixture".to_string(),
            "session_fixture".to_string(),
        )
        .expect("open Runtime Chat host")
    }

    fn binding() -> RelayThreadBinding {
        RelayThreadBinding {
            run_id: "run_fixture".to_string(),
            runtime_binding_id: "binding_fixture".to_string(),
            virtual_thread_id: format!("local_thread_{}", "a".repeat(24)),
            native_thread_id: "thr_fixture".to_string(),
            model: "model-1".to_string(),
            model_provider: "custom".to_string(),
            model_provider_id: "local-provider".to_string(),
            first_operation_fingerprint: fingerprint("operation-1"),
            system_prompt_fingerprint: None,
            turns: BTreeMap::from([(
                format!("local_turn_{}", "b".repeat(24)),
                RelayTurnBinding {
                    review_request_fingerprint: None,
                    native_turn_id: "turn_native".to_string(),
                    operation_fingerprint: fingerprint("operation-1"),
                    request_fingerprint: fingerprint("request-1"),
                    terminal_projected: false,
                    start_rejected: false,
                },
            )]),
            projected_native_seq: 0,
        }
    }

    #[test]
    fn persisted_binding_state_rejects_duplicate_or_nonopaque_authority() {
        let one = binding();
        let state = RelayState {
            schema_version: STATE_SCHEMA_VERSION,
            owner_scope_fingerprint: Some(fingerprint("owner")),
            bindings: vec![one.clone()],
        };
        state.validate().unwrap();
        let duplicate = RelayState {
            schema_version: STATE_SCHEMA_VERSION,
            owner_scope_fingerprint: Some(fingerprint("owner")),
            bindings: vec![one.clone(), one],
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn projection_rewrites_native_ids_to_virtual_chat_ids_and_keeps_route_receipt() {
        let binding = binding();
        let event = RuntimeEventRecord {
            schema_version: 2,
            seq: 9,
            timestamp: Utc::now(),
            thread_id: binding.native_thread_id.clone(),
            turn_id: Some("turn_native".to_string()),
            item_id: None,
            event: "turn.completed".to_string(),
            payload: json!({
                "turn": {
                    "status": "completed",
                    "usage": { "input_tokens": 2, "output_tokens": 3 },
                    "effective_model": "model-1",
                    "effective_provider": "custom",
                    "effective_billing_surface": "provider_byok",
                    "effective_billing_mode": "local",
                }
            }),
        };
        let ProjectionDecision::Project(projected) = project_native_event(&binding, &event) else {
            panic!("terminal event must project");
        };
        assert_eq!(projected.virtual_thread_id, binding.virtual_thread_id);
        assert!(projected.virtual_turn_id.starts_with("local_turn_"));
        assert_eq!(projected.event, "turn.completed");
        assert_eq!(
            projected.source_event_id,
            source_event_id(&binding.native_thread_id, event.seq)
        );
        assert_eq!(projected.source_event_id.len(), 77);
        assert_eq!(projected.payload["turn"]["effective_billing_mode"], "local");
        assert!(!projected.payload.to_string().contains("thr_fixture"));
    }

    #[test]
    fn chat_command_shape_requires_empty_tools_and_exact_chat_modes() {
        let mut prompt = RuntimeChatPrompt {
            structured_review: None,
            account_model_owner: None,
            profile_constitution: None,
            images: Vec::new(),
            max_output_tokens: None,
            command_type: "prompt.request".to_string(),
            run_id: "run_fixture".to_string(),
            turn_id: format!("local_turn_{}", "b".repeat(24)),
            operation_key: "operation-1".to_string(),
            runtime_binding_id: "binding_fixture".to_string(),
            runtime_thread_id: format!("local_thread_{}", "a".repeat(24)),
            prompt: "hello".to_string(),
            system_prompt: None,
            model: "model-1".to_string(),
            model_provider: "custom".to_string(),
            model_provider_id: "local-provider".to_string(),
            reasoning_effort: Some("high".to_string()),
            allowed_tools: Vec::new(),
            mode: "chat".to_string(),
            requested_mode: "chat".to_string(),
            workspace: RuntimeChatWorkspace {
                id: "workspace_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
            },
        };
        prompt.validate_shape().unwrap();
        for reasoning in ["minimal", "ultra"] {
            prompt.reasoning_effort = Some(reasoning.into());
            prompt.validate_shape().unwrap();
        }
        prompt.reasoning_effort = Some("invented-effort".into());
        assert!(prompt.validate_shape().is_err());
        prompt.reasoning_effort = None;
        prompt.allowed_tools.push("bash".to_string());
        assert!(prompt.validate_shape().is_err());
        prompt.allowed_tools.clear();
        prompt.requested_mode = "work".to_string();
        assert!(prompt.validate_shape().is_err());
    }

    #[tokio::test]
    async fn active_participant_rejects_new_and_exact_replay_without_blocking_or_starting() {
        let root = tempfile::tempdir().unwrap();
        let config = {
            let __cw_config_init_0_0 = Some("ollama".to_string());
            let __cw_config_init_0_1 = Some("relay-local:fixture".to_string());
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.default_text_model = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        let host = RuntimeChatRelayHost::open(
            config,
            Arc::new(PluginRegistry::empty(root.path())),
            root.path().to_path_buf(),
            "target_fixture".to_string(),
            "session_fixture".to_string(),
        )
        .unwrap();
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();
        host.authorize_run("run_fixture").unwrap();
        let prompt = RuntimeChatPrompt {
            structured_review: None,
            account_model_owner: None,
            profile_constitution: None,
            images: Vec::new(),
            max_output_tokens: None,
            command_type: "prompt.request".to_string(),
            run_id: "run_fixture".to_string(),
            turn_id: format!("local_turn_{}", "e".repeat(24)),
            operation_key: "operation-gate-fixture".to_string(),
            runtime_binding_id: "binding-gate-fixture".to_string(),
            runtime_thread_id: format!("local_thread_{}", "f".repeat(24)),
            prompt: "hello".to_string(),
            system_prompt: None,
            model: "relay-local:fixture".to_string(),
            model_provider: "ollama".to_string(),
            model_provider_id: "ollama".to_string(),
            reasoning_effort: None,
            allowed_tools: Vec::new(),
            mode: "chat".to_string(),
            requested_mode: "chat".to_string(),
            workspace: RuntimeChatWorkspace {
                id: "workspace_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
            },
        };

        let participant =
            codewhale_engine::client::acquire_remote_control_inference_participant().await;
        let direct_error = host.try_ensure_inference_ownership().unwrap_err();
        assert!(
            direct_error.contains("active local provider work"),
            "{direct_error}"
        );
        for attempt in 0..2 {
            let error = tokio::time::timeout(Duration::from_secs(30), host.apply_prompt(&prompt))
                .await
                .expect("Runtime Chat admission must not deadlock behind the UI-owned writer")
                .unwrap_err();
            assert!(error.contains("active local provider work"), "{error}");
            assert!(
                !host.has_any_unsettled_turns(),
                "rejected attempt {attempt} must not leave a phantom reservation"
            );
        }
        assert_eq!(
            host.native_turn_count_for_binding_for_tests(&prompt.runtime_binding_id)
                .await,
            0,
            "neither the new command nor its exact replay may reach provider-backed native start"
        );
        drop(participant);
        host.try_ensure_inference_ownership().unwrap();
        host.release_inference_ownership_if_settled();
    }

    #[tokio::test]
    async fn recovered_reservation_without_native_turn_settles_after_definitive_start_rejection() {
        let root = tempfile::tempdir().unwrap();
        let host = open_host(root.path());
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();
        host.authorize_run("run_fixture").unwrap();
        let identity = host.config.active_provider_identity().unwrap();
        let provider = identity.provider;
        let model_provider_id = host
            .config
            .active_provider_identity()
            .unwrap()
            .persisted_id()
            .unwrap_or_else(|| provider.as_str())
            .to_string();
        let prompt = RuntimeChatPrompt {
            structured_review: None,
            account_model_owner: None,
            profile_constitution: None,
            images: Vec::new(),
            max_output_tokens: None,
            command_type: "prompt.request".to_string(),
            run_id: "run_fixture".to_string(),
            turn_id: format!("local_turn_{}", "8".repeat(24)),
            operation_key: "operation-crash-window".to_string(),
            runtime_binding_id: "binding-crash-window".to_string(),
            runtime_thread_id: format!("local_thread_{}", "9".repeat(24)),
            prompt: "hello".to_string(),
            system_prompt: None,
            model: host.config.default_model(),
            model_provider: provider.as_str().to_string(),
            model_provider_id,
            reasoning_effort: None,
            allowed_tools: Vec::new(),
            mode: "chat".to_string(),
            requested_mode: "chat".to_string(),
            workspace: RuntimeChatWorkspace {
                id: "workspace_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
            },
        };
        host.install_prompt_replay_for_tests(&prompt, false)
            .await
            .unwrap();
        assert!(host.has_any_unsettled_turns());
        assert_eq!(
            host.native_turn_count_for_binding_for_tests(&prompt.runtime_binding_id)
                .await,
            0
        );

        let error = host.apply_prompt(&prompt).await.unwrap_err();
        assert!(error.contains("could not start"), "{error}");
        assert!(
            !host.has_any_unsettled_turns(),
            "a proven pre-native crash reservation must not hold ownership forever"
        );
        assert!(!host.inference_ownership_is_held_for_tests());
        assert_eq!(
            host.native_turn_count_for_binding_for_tests(&prompt.runtime_binding_id)
                .await,
            0
        );
    }

    #[test]
    fn catalog_is_challenge_bound_active_ready_and_secret_free() {
        let mut config = {
            let __cw_config_init_1_0 = Some("ollama".to_string());
            let __cw_config_init_1_1 =
                Some(codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_MODEL.to_string());
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.default_text_model = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        config.set_legacy_root(Some("must-not-cross".to_string()), None);
        config.set_legacy_root(None, Some("http://127.0.0.1:11434/v1".to_string()));
        let challenge = "c".repeat(32);
        assert!(crate::runtime_api::runtime_chat_relay_catalog(&config, &challenge).is_err());
        config.default_text_model = Some("relay-local:fixture".to_string());
        let catalog = crate::runtime_api::runtime_chat_relay_catalog(&config, &challenge).unwrap();
        assert_eq!(catalog["protocol"], "codewhale.runtime-chat-relay.v1");
        assert_eq!(catalog["challenge"], challenge);
        assert_eq!(catalog["runtime"]["service"], "codewhale-runtime-api");
        assert_eq!(catalog["runtime"]["apiVersion"], "1.0");
        assert_eq!(catalog["runtime"]["capabilities"]["stable_event_ids"], true);
        assert_eq!(catalog["providers"].as_array().unwrap().len(), 1);
        assert_eq!(
            catalog["providers"][0]["models"][0]["imageInput"],
            "unknown"
        );
        assert_eq!(
            catalog["runtime"]["capabilities"]["turn_image_inputs"],
            true
        );
        let serialized = catalog.to_string();
        assert!(!serialized.contains("must-not-cross"));
        assert!(!serialized.contains("127.0.0.1"));
        assert!(!serialized.contains("baseUrl"));
        assert!(!serialized.contains("endpoint"));
    }

    #[test]
    fn interrupt_resolution_is_bound_to_the_current_run() {
        let binding = binding();
        let turn_id = binding.turns.keys().next().unwrap().clone();
        let scope = RuntimeChatControlScope {
            runtime_binding_id: binding.runtime_binding_id.clone(),
            runtime_thread_id: binding.virtual_thread_id.clone(),
        };
        let state = RelayState {
            schema_version: STATE_SCHEMA_VERSION,
            owner_scope_fingerprint: Some(fingerprint("owner")),
            bindings: vec![binding],
        };
        assert!(resolve_interrupt_target(&state, "run_fixture", &scope, &turn_id).is_ok());
        assert!(resolve_interrupt_target(&state, "run_other", &scope, &turn_id).is_err());
    }

    #[test]
    fn scoped_state_is_exclusive_account_bound_and_restart_stable() {
        let root = tempfile::tempdir().unwrap();
        let first_path = scoped_private_dir(root.path(), "target_fixture", "session_fixture");
        let same_path = scoped_private_dir(root.path(), "target_fixture", "session_fixture");
        let other_path = scoped_private_dir(root.path(), "target_fixture", "session_other");
        assert_eq!(first_path, same_path);
        assert_ne!(first_path, other_path);
        assert!(!first_path.to_string_lossy().contains("target_fixture"));

        fs::create_dir_all(&first_path).unwrap();
        let lock_path = first_path.join(SCOPE_LOCK_FILE);
        let first_lock = RelayScopeLock::acquire(&lock_path).unwrap();
        assert!(RelayScopeLock::acquire(&lock_path).is_err());

        let owner = owner_scope_fingerprint("account_one", "target_fixture", "session_fixture");
        let mut state = RelayState::default();
        bind_owner_scope(&mut state, &owner).unwrap();
        bind_owner_scope(&mut state, &owner).unwrap();
        assert!(
            bind_owner_scope(
                &mut state,
                &owner_scope_fingerprint("account_other", "target_fixture", "session_fixture")
            )
            .is_err()
        );
        state.bindings.push(binding());
        let state_path = first_path.join(STATE_FILE);
        persist_state(&state_path, &state).unwrap();
        assert_eq!(load_state(&state_path).unwrap(), state);
        assert!(
            fs::read_dir(&first_path)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
        );

        drop(first_lock);
        RelayScopeLock::acquire(&lock_path).unwrap();
    }

    #[test]
    fn isolated_chat_prompt_drops_local_project_memory_and_skill_context() {
        let root = tempfile::tempdir().unwrap();
        let config = {
            let __cw_config_init_2_0 = Some("/Users/alice/CANARY_SKILLS".to_string());
            let __cw_config_init_2_1 = Some(vec!["/Users/alice/CANARY_AGENTS.md".to_string()]);
            let __cw_config_init_2_2 = Some("/Users/alice/CANARY_MEMORY.md".to_string());
            let __cw_config_init_2_3 = Some(MemoryConfig {
                enabled: Some(true),
                backend: Some(MemoryBackend::Native),
            });
            let __cw_config_init_2_4 = ContextConfig {
                project_pack: Some(true),
            };
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.skills_dir = __cw_config_init_2_0;
            __cw_config_init_2_value.instructions = __cw_config_init_2_1;
            __cw_config_init_2_value.memory_path = __cw_config_init_2_2;
            __cw_config_init_2_value.memory = __cw_config_init_2_3;
            __cw_config_init_2_value.context = __cw_config_init_2_4;
            __cw_config_init_2_value
        };

        let (execution, workspace) = isolated_chat_execution_config(&config, root.path()).unwrap();
        assert!(workspace.starts_with(root.path()));
        assert_ne!(workspace, PathBuf::from("/Users/alice"));
        assert!(!execution.memory_enabled());
        assert!(execution.instructions_paths().is_empty());
        assert!(!execution.project_context_pack_enabled());
        assert!(execution.skills_dir().starts_with(root.path()));
        assert!(execution.memory_path().starts_with(root.path()));
        assert!(execution.mcp_config_path().starts_with(root.path()));
        assert!(execution.notes_path().starts_with(root.path()));
        assert!(execution.skills_config().scan_codewhale_only());

        let prompt = dedicated_chat_system_prompt(None);
        assert_eq!(prompt, ISOLATED_CHAT_SYSTEM_PROMPT);
        for canary in [
            "CANARY_SKILLS",
            "CANARY_AGENTS",
            "CANARY_MEMORY",
            "/Users/alice",
        ] {
            assert!(!prompt.contains(canary));
        }
        let account_prompt = dedicated_chat_system_prompt(Some("Reply in short paragraphs."));
        assert!(account_prompt.starts_with(ISOLATED_CHAT_SYSTEM_PROMPT));
        assert!(account_prompt.contains("<account_chat_instructions>"));
    }

    #[test]
    fn unsafe_model_ids_are_rejected_and_never_cross_the_catalog() {
        for model in [
            "/Users/alice/private-model",
            "~/.ssh/provider-model",
            "sk-live-secret",
            "../secrets/model",
            "redacted-local-path",
            "alice:hunter2@internal.example:443/model",
            "internal.example:443/model",
            "internal:443/model",
        ] {
            assert!(validate_model_id(model).is_err(), "accepted {model:?}");
            let config = {
                let __cw_config_init_3_0 = Some("ollama".to_string());
                let __cw_config_init_3_1 = Some(model.to_string());
                let mut __cw_config_init_3_value: Config = Config::default();
                __cw_config_init_3_value.provider = __cw_config_init_3_0;
                __cw_config_init_3_value.default_text_model = __cw_config_init_3_1;
                __cw_config_init_3_value
            };
            if let Ok(catalog) =
                crate::runtime_api::runtime_chat_relay_catalog(&config, &"c".repeat(32))
            {
                let ids = catalog["providers"][0]["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|entry| entry["id"].as_str())
                    .collect::<Vec<_>>();
                assert!(!ids.contains(&model), "catalog leaked {model:?}");
            }
        }
        validate_model_id("anthropic/claude-sonnet-5").unwrap();

        for provider_id in ["ghp_secret-provider", "hf_secret-provider", "glpat-secret"] {
            assert!(!crate::runtime_api::runtime_chat_route_id_is_safe(
                provider_id
            ));
            let config = {
                let __cw_config_init_4_0 = Some(provider_id.to_string());
                let __cw_config_init_4_1 = Some(codewhale_runtime::config::ProvidersConfig {
                    custom: std::collections::HashMap::from([(
                        provider_id.to_string(),
                        codewhale_runtime::config::ProviderConfig {
                            kind: Some("openai-compatible".to_string()),
                            api_key: Some("fixture-key".to_string()),
                            base_url: Some("https://example.test/v1".to_string()),
                            model: Some("safe-model".to_string()),
                            ..Default::default()
                        },
                    )]),
                    ..Default::default()
                });
                let mut __cw_config_init_4_value: Config = Config::default();
                __cw_config_init_4_value.provider = __cw_config_init_4_0;
                __cw_config_init_4_value.providers = __cw_config_init_4_1;
                __cw_config_init_4_value
            };
            let error = crate::runtime_api::runtime_chat_relay_catalog(&config, &"c".repeat(32))
                .unwrap_err();
            assert!(!error.contains(provider_id));
        }
    }

    #[test]
    fn native_source_event_id_is_stable_across_transport_replay() {
        let first = source_event_id("thr_fixture", 41);
        assert_eq!(first, source_event_id("thr_fixture", 41));
        assert_ne!(first, source_event_id("thr_fixture", 42));
        assert_ne!(first, source_event_id("thr_other", 41));
        assert!(first.starts_with("native_event_"));
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        );
    }

    #[test]
    fn failed_state_writes_never_become_in_memory_authority_and_exact_retry_reopens() {
        let root = tempfile::tempdir().unwrap();
        let host = open_host(root.path());

        inject_state_persist_failures(&host.state_path, 1);
        assert!(
            host.bind_account("account_fixture", "target_fixture")
                .is_err()
        );
        assert!(host.state.lock().owner_scope_fingerprint.is_none());
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();

        let mut relay_binding = binding();
        relay_binding.turns.clear();
        inject_state_persist_failures(&host.state_path, 1);
        assert!(host.insert_binding(relay_binding.clone()).is_err());
        assert!(host.state.lock().bindings.is_empty());
        host.insert_binding(relay_binding.clone()).unwrap();

        let virtual_turn_id = format!("local_turn_{}", "c".repeat(24));
        let operation_fingerprint = fingerprint("operation-state-fault");
        let request_fingerprint = fingerprint("request-state-fault");
        let native_turn_id = reserved_native_turn_id(
            &relay_binding.native_thread_id,
            &relay_binding.runtime_binding_id,
            &virtual_turn_id,
            &operation_fingerprint,
        );
        inject_state_persist_failures(&host.state_path, 1);
        assert!(
            host.reserve_turn(
                &relay_binding.runtime_binding_id,
                &relay_binding.virtual_thread_id,
                &virtual_turn_id,
                &native_turn_id,
                &operation_fingerprint,
                &request_fingerprint,
                None,
            )
            .is_err()
        );
        assert!(
            !host.state.lock().bindings[0]
                .turns
                .contains_key(&virtual_turn_id)
        );
        host.reserve_turn(
            &relay_binding.runtime_binding_id,
            &relay_binding.virtual_thread_id,
            &virtual_turn_id,
            &native_turn_id,
            &operation_fingerprint,
            &request_fingerprint,
            None,
        )
        .unwrap();

        inject_state_persist_failures(&host.state_path, 1);
        assert!(
            host.advance_projection_cursor(&relay_binding.native_thread_id, 7, None)
                .is_err()
        );
        assert_eq!(host.state.lock().bindings[0].projected_native_seq, 0);
        host.advance_projection_cursor(&relay_binding.native_thread_id, 7, None)
            .unwrap();

        drop(host);
        let reopened = open_host(root.path());
        let state = reopened.state.lock();
        assert!(state.owner_scope_fingerprint.is_some());
        assert_eq!(state.bindings.len(), 1);
        assert_eq!(state.bindings[0].projected_native_seq, 7);
        assert_eq!(
            state.bindings[0].turns[&virtual_turn_id].native_turn_id,
            native_turn_id
        );
    }

    #[test]
    fn conflicting_replay_never_settles_a_live_turn_reservation() {
        let root = tempfile::tempdir().unwrap();
        let host = open_host(root.path());
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();
        let mut relay_binding = binding();
        relay_binding.turns.clear();
        host.insert_binding(relay_binding.clone()).unwrap();

        let virtual_turn_id = format!("local_turn_{}", "d".repeat(24));
        let operation_fingerprint = fingerprint("same-operation");
        let original_request = fingerprint("original-request");
        let changed_request = fingerprint("changed-request");
        let native_turn_id = reserved_native_turn_id(
            &relay_binding.native_thread_id,
            &relay_binding.runtime_binding_id,
            &virtual_turn_id,
            &operation_fingerprint,
        );
        assert_eq!(
            host.reserve_turn(
                &relay_binding.runtime_binding_id,
                &relay_binding.virtual_thread_id,
                &virtual_turn_id,
                &native_turn_id,
                &operation_fingerprint,
                &original_request,
                None,
            )
            .unwrap(),
            TurnReservationDisposition::New
        );
        assert!(
            host.reserve_turn(
                &relay_binding.runtime_binding_id,
                &relay_binding.virtual_thread_id,
                &virtual_turn_id,
                &native_turn_id,
                &operation_fingerprint,
                &changed_request,
                None,
            )
            .is_err()
        );
        assert!(host.has_any_unsettled_turns());
        assert!(!host.state.lock().bindings[0].turns[&virtual_turn_id].terminal_projected);
    }

    #[test]
    fn rejected_start_retry_reopens_the_same_deterministic_reservation() {
        let root = tempfile::tempdir().unwrap();
        let host = open_host(root.path());
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();
        let mut relay_binding = binding();
        relay_binding.turns.clear();
        host.insert_binding(relay_binding.clone()).unwrap();

        let virtual_turn_id = format!("local_turn_{}", "e".repeat(24));
        let operation_fingerprint = fingerprint("retry-operation");
        let request_fingerprint = fingerprint("retry-request");
        let native_turn_id = reserved_native_turn_id(
            &relay_binding.native_thread_id,
            &relay_binding.runtime_binding_id,
            &virtual_turn_id,
            &operation_fingerprint,
        );
        assert_eq!(
            host.reserve_turn(
                &relay_binding.runtime_binding_id,
                &relay_binding.virtual_thread_id,
                &virtual_turn_id,
                &native_turn_id,
                &operation_fingerprint,
                &request_fingerprint,
                None,
            )
            .unwrap(),
            TurnReservationDisposition::New
        );
        host.finish_turn_reservation(
            &relay_binding.runtime_binding_id,
            &relay_binding.virtual_thread_id,
            &virtual_turn_id,
        )
        .unwrap();
        assert!(!host.has_any_unsettled_turns());
        assert_eq!(
            host.reserve_turn(
                &relay_binding.runtime_binding_id,
                &relay_binding.virtual_thread_id,
                &virtual_turn_id,
                &native_turn_id,
                &operation_fingerprint,
                &request_fingerprint,
                None,
            )
            .unwrap(),
            TurnReservationDisposition::Reopened
        );
        assert!(host.has_any_unsettled_turns());
    }

    #[test]
    fn exact_replay_of_a_projected_terminal_turn_stays_settled() {
        let root = tempfile::tempdir().unwrap();
        let host = open_host(root.path());
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();
        let mut relay_binding = binding();
        relay_binding.turns.clear();
        host.insert_binding(relay_binding.clone()).unwrap();

        let virtual_turn_id = format!("local_turn_{}", "f".repeat(24));
        let operation_fingerprint = fingerprint("terminal-replay-operation");
        let request_fingerprint = fingerprint("terminal-replay-request");
        let native_turn_id = reserved_native_turn_id(
            &relay_binding.native_thread_id,
            &relay_binding.runtime_binding_id,
            &virtual_turn_id,
            &operation_fingerprint,
        );
        assert_eq!(
            host.reserve_turn(
                &relay_binding.runtime_binding_id,
                &relay_binding.virtual_thread_id,
                &virtual_turn_id,
                &native_turn_id,
                &operation_fingerprint,
                &request_fingerprint,
                None,
            )
            .unwrap(),
            TurnReservationDisposition::New
        );
        host.advance_projection_cursor(&relay_binding.native_thread_id, 11, Some(&virtual_turn_id))
            .unwrap();
        assert!(!host.has_any_unsettled_turns());

        assert_eq!(
            host.reserve_turn(
                &relay_binding.runtime_binding_id,
                &relay_binding.virtual_thread_id,
                &virtual_turn_id,
                &native_turn_id,
                &operation_fingerprint,
                &request_fingerprint,
                None,
            )
            .unwrap(),
            TurnReservationDisposition::ExistingTerminal
        );
        assert!(
            !host.has_any_unsettled_turns(),
            "an idempotent replay of already-projected output cannot reopen provider work"
        );
    }

    #[tokio::test]
    async fn restart_refuses_a_new_run_until_the_durable_old_turn_is_projected() {
        let root = tempfile::tempdir().unwrap();
        {
            let host = open_host(root.path());
            host.bind_account("account_fixture", "target_fixture")
                .unwrap();
            host.insert_binding(binding()).unwrap();
            host.authorize_run("run_fixture").unwrap();
            assert!(host.has_unsettled_authorized_turns());
        }

        let reopened = open_host(root.path());
        reopened
            .bind_account("account_fixture", "target_fixture")
            .unwrap();
        assert!(reopened.authorize_run("run_other").is_err());
        reopened.authorize_run("run_fixture").unwrap();
        let relay_binding = reopened.state.lock().bindings[0].clone();
        let virtual_turn_id = relay_binding.turns.keys().next().unwrap().clone();
        reopened
            .mark_projected(
                &relay_binding.native_thread_id,
                9,
                &virtual_turn_id,
                "turn.completed",
            )
            .unwrap();
        assert!(!reopened.has_unsettled_authorized_turns());
        reopened.authorize_run("run_other").unwrap();
    }
    #[test]
    fn runtime_image_relay_hash_preserves_text_and_binds_order() {
        let legacy = json!({"type":"prompt.request","runId":"run_fixture","turnId":format!("local_turn_{}", "b".repeat(24)),"operationKey":"operation-1","runtimeBindingId":"binding_fixture","runtimeThreadId":format!("local_thread_{}", "a".repeat(24)),"prompt":"look","model":"deepseek-v4-flash-vision-exp","modelProvider":"deepseek","modelProviderId":"deepseek","allowedTools":[],"mode":"chat","requestedMode":"chat","workspace":{"id":"workspace_fixture","targetRef":"target_fixture"}});
        let mut command: RuntimeChatPrompt = serde_json::from_value(legacy.clone()).unwrap();
        command.validate_shape().unwrap();
        let expected = hex_digest(Sha256::digest(
            serde_json::to_vec(&canonical_json_value(&legacy)).unwrap(),
        ));
        assert_eq!(
            runtime_chat_request_fingerprint(&command).unwrap(),
            expected
        );
        let mut with_empty = legacy;
        with_empty["images"] = json!([]);
        let empty: RuntimeChatPrompt = serde_json::from_value(with_empty).unwrap();
        assert_eq!(runtime_chat_request_fingerprint(&empty).unwrap(), expected);
        command.images = vec![
            codewhale_engine::image_attach::test_support::runtime_image_fixture(1),
            codewhale_engine::image_attach::test_support::runtime_image_fixture(2),
        ];
        command.validate_shape().unwrap();
        let first = runtime_chat_request_fingerprint(&command).unwrap();
        command.images.reverse();
        assert_ne!(runtime_chat_request_fingerprint(&command).unwrap(), first);
        command.model = "auto".into();
        assert!(command.validate_shape().is_err());
    }
}
