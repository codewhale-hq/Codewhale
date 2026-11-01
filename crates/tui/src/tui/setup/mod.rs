//! Progressive setup and repair guide.
//!
//! Bare `/setup` starts at the first missing decision and otherwise shows a
//! compact readiness summary. Optional power tools only join that journey when
//! they are already configured or need repair. Named `/setup <target>` routes
//! and the versioned Constitution checkpoint remain compatibility entrypoints,
//! but ordinary preferences belong to `/settings` and advanced keys belong to
//! `/config <key>`.

use std::borrow::Cow;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use crate::tui::app::{App, AppModeUi};
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
    render_panel_scroll_rail, render_underwater_surface,
};
use codewhale_engine::prompts::{
    BASE_PROMPT_OVERRIDE_OPT_IN_ENV, CONSTITUTION_OVERRIDE_FILE, base_prompt_override_opt_in,
};
use codewhale_engine::provider_readiness::has_api_key;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::Config;

use codewhale_config::{
    AutonomyPreference, ConstitutionAuthoring, ConstitutionChoice, ConstitutionSource,
    ConstitutionValidity, InheritedConfigFacts, RuntimePostureSource, SetupState, SetupStep,
    StepEntry, StepStatus, UserConstitution, UserConstitutionLoad,
    user_constitution::MAX_NOTES_LEN,
};

mod fleet_draft;
mod model_draft;
mod operate;
mod persistence;
mod provider;
mod remote;
mod tools_mcp;

pub(crate) use fleet_draft::{draft_fleet_profile_with_model, workspace_fingerprint};
pub(crate) use model_draft::draft_constitution_with_model;
use persistence::SetupPersistenceFacts;
pub(crate) use provider::record_configured_route;
use remote::SetupRemoteFacts;

use codewhale_engine::prompts::CONSTITUTION_CHECKPOINT_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupCommitKind {
    BundledConstitution,
    DeferredConstitution,
}

pub trait SetupWizardStep {
    fn id(&self) -> SetupStep;
    fn title_id(&self) -> MessageId;
    fn why_id(&self) -> MessageId;
    fn required(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StaticSetupStep {
    id: SetupStep,
    title_id: MessageId,
    why_id: MessageId,
    required: bool,
}

impl SetupWizardStep for StaticSetupStep {
    fn id(&self) -> SetupStep {
        self.id
    }

    fn title_id(&self) -> MessageId {
        self.title_id
    }

    fn why_id(&self) -> MessageId {
        self.why_id
    }

    fn required(&self) -> bool {
        self.required
    }
}

const STEP_SPECS: [StaticSetupStep; 10] = [
    StaticSetupStep {
        id: SetupStep::Language,
        title_id: MessageId::SetupStepLanguageTitle,
        why_id: MessageId::SetupStepLanguageWhy,
        required: true,
    },
    StaticSetupStep {
        id: SetupStep::ProviderModel,
        title_id: MessageId::SetupStepProviderModelTitle,
        why_id: MessageId::SetupStepProviderModelWhy,
        required: true,
    },
    StaticSetupStep {
        id: SetupStep::TrustSandbox,
        title_id: MessageId::SetupStepTrustSandboxTitle,
        why_id: MessageId::SetupStepTrustSandboxWhy,
        required: true,
    },
    StaticSetupStep {
        id: SetupStep::Constitution,
        title_id: MessageId::SetupStepConstitutionTitle,
        why_id: MessageId::SetupStepConstitutionWhy,
        required: true,
    },
    StaticSetupStep {
        id: SetupStep::OperateFleet,
        title_id: MessageId::SetupStepOperateFleetTitle,
        why_id: MessageId::SetupStepOperateFleetWhy,
        required: false,
    },
    StaticSetupStep {
        id: SetupStep::Hotbar,
        title_id: MessageId::SetupStepHotbarTitle,
        why_id: MessageId::SetupStepHotbarWhy,
        required: false,
    },
    StaticSetupStep {
        id: SetupStep::ToolsMcp,
        title_id: MessageId::SetupStepToolsMcpTitle,
        why_id: MessageId::SetupStepToolsMcpWhy,
        required: false,
    },
    StaticSetupStep {
        id: SetupStep::RemoteRuntime,
        title_id: MessageId::SetupStepRemoteRuntimeTitle,
        why_id: MessageId::SetupStepRemoteRuntimeWhy,
        required: false,
    },
    StaticSetupStep {
        id: SetupStep::Persistence,
        title_id: MessageId::SetupStepPersistenceTitle,
        why_id: MessageId::SetupStepPersistenceWhy,
        required: false,
    },
    StaticSetupStep {
        id: SetupStep::Verification,
        title_id: MessageId::SetupStepVerificationTitle,
        why_id: MessageId::SetupStepVerificationWhy,
        required: false,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupWizardView {
    state: SetupState,
    selected: usize,
    locale: Locale,
    /// Bare `/setup` is a short runtime-derived repair journey. Explicit
    /// targets remain focused compatibility cards outside that journey.
    progressive_guide: bool,
    /// Technical inventory and preset details stay off the first paint.
    details_expanded: bool,
    facts: SetupRuntimeFacts,
    guided_draft: GuidedConstitutionDraft,
    /// First-run shows one plain-language initiative choice. The six-axis
    /// editor remains available explicitly, but never competes with the
    /// recommended path on first paint.
    constitution_advanced: bool,
    freeform_note: String,
    editing_freeform_note: bool,
    guided_preview_seen: bool,
    /// The keep-existing path mirrors the guided two-step: the first `K`
    /// opens the rendered preview of the existing file, the second completes
    /// the checkpoint without touching it.
    existing_preview_seen: bool,
    /// A model-drafted constitution awaiting ratification, installed by the
    /// host after a successful one-shot draft (already sanitized + bounded).
    /// Cleared whenever a guided answer changes so a stale draft can never be
    /// ratified against fresh answers.
    model_draft: Option<Box<UserConstitution>>,
    /// Display label of the model that authored `model_draft` (safe metadata,
    /// e.g. "GLM-5.2"), for provenance copy only.
    model_draft_label: Option<String>,
    /// The answers (and free-form note) the in-flight model draft was asked
    /// to write. A draft lands only while they are still the wizard's
    /// answers; one requested for answers since changed is stale (U08-06).
    model_draft_request: Option<(GuidedConstitutionDraft, Option<String>)>,
    runtime_preset: SetupRuntimePreset,
    runtime_preset_preview_seen: bool,
    body_scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupRuntimeFacts {
    provider: String,
    model: String,
    auth: String,
    health: String,
    provider_ready: bool,
    provider_status: StepStatus,
    provider_result: String,
    work_intent: String,
    approval: String,
    shell: String,
    allow_shell_enabled: bool,
    trust: String,
    sandbox: String,
    sandbox_mode_value: String,
    network: String,
    network_default_value: String,
    runtime_result: String,
    operate_runtime_ready: bool,
    operate_runtime_result: String,
    fleet_roster_ready: bool,
    fleet_roster_result: String,
    operate_concurrency_result: String,
    operate_result: String,
    hotbar_bindings_result: String,
    hotbar_actions_result: String,
    hotbar_result: String,
    tools_mcp_servers_result: String,
    tools_mcp_skills_result: String,
    tools_mcp_tools_result: String,
    tools_mcp_plugins_result: String,
    tools_mcp_dsh_result: String,
    tools_mcp_hotbar_result: String,
    tools_mcp_result: String,
    tools_mcp_needs_action: bool,
    tools_mcp_path_display: String,
    tools_mcp_skills_path_display: String,
    tools_mcp_plugins_path_display: String,
    remote_clouds_result: String,
    remote_bridges_result: String,
    remote_providers_result: String,
    remote_mode_result: String,
    remote_command_provider: String,
    remote_result: String,
    remote_control_result: String,
    /// The four observed remote modes (#3409). Empty only before facts load.
    remote_modes: Vec<remote::RemoteModeFact>,
    /// True when a mode is missing a token or config. Recorded as
    /// `NeedsAction`, which by contract never blocks the ready screen.
    remote_needs_action: bool,
    persistence: SetupPersistenceFacts,
    default_mode: String,
    approval_policy_value: String,
    project_override_warning: Option<String>,
    constitution_autonomy: String,
    constitution_file: SetupConstitutionFileState,
    expert_override: SetupExpertOverrideState,
}

impl Default for SetupRuntimeFacts {
    fn default() -> Self {
        Self {
            provider: "not loaded".to_string(),
            model: "not loaded".to_string(),
            auth: "not checked".to_string(),
            health: "not checked".to_string(),
            provider_ready: false,
            provider_status: StepStatus::NeedsAction,
            provider_result: "provider/model not loaded".to_string(),
            work_intent: "not loaded".to_string(),
            approval: "not loaded".to_string(),
            shell: "not loaded".to_string(),
            allow_shell_enabled: false,
            trust: "not loaded".to_string(),
            sandbox: "not configured".to_string(),
            sandbox_mode_value: "default".to_string(),
            network: "not configured".to_string(),
            network_default_value: "prompt".to_string(),
            runtime_result: "permissions not loaded".to_string(),
            operate_runtime_ready: false,
            operate_runtime_result: "agent runtime not loaded".to_string(),
            fleet_roster_ready: false,
            fleet_roster_result: "Fleet not loaded".to_string(),
            operate_concurrency_result: "concurrency not loaded".to_string(),
            operate_result: "operate readiness not loaded".to_string(),
            hotbar_bindings_result: "Hotbar config not loaded".to_string(),
            hotbar_actions_result: "Hotbar actions not loaded".to_string(),
            hotbar_result: "hotbar not loaded".to_string(),
            tools_mcp_servers_result: "MCP config not loaded".to_string(),
            tools_mcp_skills_result: "skills dir not loaded".to_string(),
            tools_mcp_tools_result: "tools dir not loaded".to_string(),
            tools_mcp_plugins_result: "plugins dir not loaded".to_string(),
            tools_mcp_dsh_result: "DeepSeek Harness not probed".to_string(),
            tools_mcp_hotbar_result: "hotbar source metadata not loaded".to_string(),
            tools_mcp_result: "tools/MCP not loaded".to_string(),
            tools_mcp_needs_action: false,
            tools_mcp_path_display: String::new(),
            tools_mcp_skills_path_display: String::new(),
            tools_mcp_plugins_path_display: String::new(),
            remote_clouds_result: "remote cloud registry not loaded".to_string(),
            remote_bridges_result: "remote bridge registry not loaded".to_string(),
            remote_providers_result: "provider registry not loaded".to_string(),
            remote_mode_result: "remote setup mode not loaded".to_string(),
            remote_command_provider: "deepseek".to_string(),
            remote_result: "remote runtime not loaded".to_string(),
            remote_control_result: "off".to_string(),
            remote_modes: Vec::new(),
            remote_needs_action: false,
            persistence: SetupPersistenceFacts::default(),
            default_mode: "agent".to_string(),
            approval_policy_value: "on-request".to_string(),
            project_override_warning: None,
            constitution_autonomy: "not loaded".to_string(),
            constitution_file: SetupConstitutionFileState::NotChecked,
            expert_override: SetupExpertOverrideState::NotChecked,
        }
    }
}

impl SetupRuntimeFacts {
    fn from_app_config(app: &App, config: &Config) -> Self {
        let expert_override = SetupExpertOverrideState::load();
        let readiness = app
            .admitted_provider_identity()
            .ok()
            .filter(|identity| config.verify_provider_identity(identity).is_ok())
            .map_or(
                codewhale_engine::provider_readiness::ResolvedProviderReadiness::InvalidRoute,
                |identity| {
                    codewhale_engine::provider_readiness::resolve_for_model(
                        config,
                        identity,
                        if app.auto_model { "auto" } else { &app.model },
                        &app.provider_health,
                    )
                },
            );
        // A failed observed check remains retryable in route pickers, but the
        // setup receipt must not certify it as healthy. Saved-unchecked and
        // local-unchecked are honest reviewed configuration states; an actual
        // session failure is NeedsAction until a later success replaces it.
        let provider_status = provider::step_status(&readiness);
        let provider_ready = matches!(
            provider_status,
            StepStatus::Configured | StepStatus::Verified
        );
        let model = app.model_display_label();
        let provider_name = app
            .admitted_provider_identity()
            .map_or("unavailable", |identity| {
                identity
                    .compatibility()
                    .map_or(identity.key.as_str(), |row| row.label)
            })
            .to_string();
        let context_window = app.effective_context_window();
        let context_window_source = app.effective_context_window_source().display_label();
        let provider =
            format!("{provider_name} · context {context_window} ({context_window_source})");
        let auth = readiness.label().into_owned();
        let health = if provider_ready {
            format!("{}; route can be attempted", readiness.label())
        } else if matches!(
            &readiness,
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedLastCheckFailed { .. }
        ) {
            format!("{}; retry or open /provider", readiness.label())
        } else if app.api_provider == codewhale_config::ProviderKind::OpenaiCodex {
            format!(
                "{}; Sign in with ChatGPT via `codewhale auth chatgpt` or /provider setup openai-codex (subscription billing). Codex CLI import remains an explicit alternative.",
                readiness.label()
            )
        } else if let Some(url) = codewhale_runtime::config::credential_help_for_provider_route(
            app.api_provider,
            &config.active_route_base_url(),
        )
        .credential_url
        {
            format!(
                "{}; credentials: {url}; open /provider to repair the route",
                readiness.label()
            )
        } else {
            format!(
                "{}; {}; open /provider to repair the route",
                readiness.label(),
                codewhale_runtime::config::credential_help_for_provider_route(
                    app.api_provider,
                    &config.active_route_base_url(),
                )
                .guidance
            )
        };
        let provider_result = format!(
            "provider={}, model={}, context_window={} ({}) auth={}, health={}",
            app.provider_identity_for_persistence(),
            model,
            context_window,
            context_window_source,
            readiness.label(),
            if provider_ready {
                "attemptable"
            } else {
                "needs action"
            }
        );
        let shell = if app.allow_shell { "enabled" } else { "hidden" }.to_string();
        let trust = if app.trust_mode {
            "trusted workspace / writes allowed by permissions"
        } else {
            "workspace trust not elevated"
        }
        .to_string();
        let sandbox = config
            .sandbox_mode
            .as_deref()
            .filter(|mode| !mode.trim().is_empty())
            .unwrap_or("default")
            .to_string();
        let sandbox_mode_value = sandbox.clone();
        let network_default_value = config
            .network
            .as_ref()
            .map_or("prompt".to_string(), |policy| policy.default.clone());
        let network = config
            .network
            .as_ref()
            .map_or("prompt by default".to_string(), |policy| {
                format!("default {}", policy.default)
            });
        let runtime_result = format!(
            "intent={}, approval={}, shell={}, trust={}, sandbox={}, network={}",
            app.mode.as_setting(),
            app.approval_mode
                .permission_chip_label()
                .to_ascii_lowercase(),
            if app.allow_shell { "enabled" } else { "hidden" },
            if app.trust_mode {
                "trusted"
            } else {
                "workspace"
            },
            sandbox,
            network
        );
        let operate = operate::SetupOperateFacts::from_app_config(app, config, provider_ready);
        let known_hotbar_action_ids = app
            .hotbar_actions
            .iter()
            .map(|action| action.id())
            .collect::<Vec<_>>();
        let hotbar_resolution = config.resolve_hotbar_bindings(&known_hotbar_action_ids);
        let configured_hotbar_slots = config.hotbar.as_ref().map_or(0, Vec::len);
        let hotbar_state = match config.hotbar.as_ref() {
            None => "hidden",
            Some(bindings) if bindings.is_empty() => "disabled",
            Some(_) => "customized",
        };
        let active_hotbar_slots = hotbar_resolution.bindings.len();
        let hotbar_warning_count = hotbar_resolution.warnings.len();
        let hotbar_bindings_result = format!(
            "{hotbar_state}; configured_slots={configured_hotbar_slots}; active_slots={active_hotbar_slots}; warnings={hotbar_warning_count}"
        );
        let hotbar_actions_result =
            format!("{} bindable actions registered", app.hotbar_actions.len());
        let hotbar_result = format!(
            "state={hotbar_state}, configured_slots={configured_hotbar_slots}, active_slots={active_hotbar_slots}, actions={}, warnings={hotbar_warning_count}",
            app.hotbar_actions.len()
        );
        let codewhale_home = setup_codewhale_home_dir();
        let persistence = SetupPersistenceFacts::from_app_config(app, config, &codewhale_home);
        let tools_mcp =
            tools_mcp::SetupToolsMcpFacts::from_app_config(app, config, &codewhale_home);
        let tools_mcp_servers_result = tools_mcp.servers_result;
        let tools_mcp_skills_result = tools_mcp.skills_result;
        let tools_mcp_tools_result = tools_mcp.tools_result;
        let tools_mcp_plugins_result = tools_mcp.plugins_result;
        let tools_mcp_hotbar_result = tools_mcp.hotbar_result;
        let tools_mcp_dsh_result = tools_mcp.dsh_result;
        let tools_mcp_result = tools_mcp.result;
        let tools_mcp_needs_action = tools_mcp.needs_action;
        let tools_mcp_path_display = tools_mcp.mcp_path_display;
        let tools_mcp_skills_path_display = tools_mcp.skills_path_display;
        let tools_mcp_plugins_path_display = tools_mcp.plugins_path_display;
        let remote = SetupRemoteFacts::from_app(app);
        let remote_needs_action = remote.needs_action();
        let constitution_autonomy = UserConstitution::load()
            .ok()
            .and_then(|load| {
                load.constitution().map(|constitution| {
                    autonomy_label(constitution.autonomy_preference, app.ui_locale).to_string()
                })
            })
            .unwrap_or_else(|| tr(app.ui_locale, MessageId::SetupAutonomyUnspecified).to_string());
        Self {
            provider,
            model,
            auth,
            health,
            provider_ready,
            provider_status,
            provider_result,
            work_intent: app.mode.display_name_localized(app.ui_locale).into_owned(),
            approval: app
                .approval_mode
                .permission_chip_label()
                .to_ascii_lowercase(),
            shell,
            allow_shell_enabled: app.allow_shell,
            trust,
            sandbox,
            sandbox_mode_value,
            network,
            network_default_value,
            runtime_result,
            operate_runtime_ready: operate.runtime_ready,
            operate_runtime_result: operate.runtime_result,
            fleet_roster_ready: operate.roster_ready,
            fleet_roster_result: operate.roster_result,
            operate_concurrency_result: operate.concurrency_result,
            operate_result: operate.result,
            hotbar_bindings_result,
            hotbar_actions_result,
            hotbar_result,
            tools_mcp_servers_result,
            tools_mcp_skills_result,
            tools_mcp_tools_result,
            tools_mcp_plugins_result,
            tools_mcp_hotbar_result,
            tools_mcp_dsh_result,
            tools_mcp_result,
            tools_mcp_needs_action,
            tools_mcp_path_display,
            tools_mcp_skills_path_display,
            tools_mcp_plugins_path_display,
            remote_clouds_result: remote.clouds_result,
            remote_bridges_result: remote.bridges_result,
            remote_providers_result: remote.providers_result,
            remote_mode_result: remote.mode_result,
            remote_command_provider: remote.command_provider,
            remote_result: remote.result,
            remote_control_result: {
                use crate::remote_control::Status;
                let message = match app.remote_control.status() {
                    Status::Connected => MessageId::SetupRemoteStatusReady,
                    Status::Connecting | Status::Stopping => MessageId::SetupStatusInProgress,
                    Status::Failed => MessageId::SetupRemoteStatusNeedsAction,
                    Status::Off => MessageId::SetupRemoteStatusDisabled,
                };
                tr(app.ui_locale, message).into_owned()
            },
            remote_needs_action,
            remote_modes: remote.modes,
            persistence,
            default_mode: app.mode.as_setting().to_string(),
            approval_policy_value: config
                .approval_policy
                .as_deref()
                .filter(|policy| !policy.trim().is_empty())
                .unwrap_or("on-request")
                .to_string(),
            project_override_warning: project_runtime_override_warning(
                &app.workspace,
                app.ui_locale,
            ),
            constitution_autonomy,
            constitution_file: SetupConstitutionFileState::load(),
            expert_override,
        }
    }
}

fn setup_codewhale_home_dir() -> std::path::PathBuf {
    codewhale_config::codewhale_home().unwrap_or_else(|_| {
        codewhale_paths::user_home().map_or_else(
            || std::path::PathBuf::from(".codewhale"),
            |home| home.join(".codewhale"),
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SetupRuntimePreset {
    AskFirst,
    #[default]
    NormalAgent,
    HighTrustLocal,
}

impl SetupRuntimePreset {
    const ALL: [Self; 3] = [Self::AskFirst, Self::NormalAgent, Self::HighTrustLocal];

    fn from_key(key: char) -> Option<Self> {
        match key {
            '1' => Some(Self::AskFirst),
            '2' => Some(Self::NormalAgent),
            '3' => Some(Self::HighTrustLocal),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::AskFirst => "ask-first",
            Self::NormalAgent => "normal-agent",
            Self::HighTrustLocal => "high-trust-local",
        }
    }

    fn title_id(self) -> MessageId {
        match self {
            Self::AskFirst => MessageId::SetupRuntimePresetAskFirstTitle,
            Self::NormalAgent => MessageId::SetupRuntimePresetNormalAgentTitle,
            Self::HighTrustLocal => MessageId::SetupRuntimePresetHighTrustTitle,
        }
    }

    fn description_id(self) -> MessageId {
        match self {
            Self::AskFirst => MessageId::SetupRuntimePresetAskFirstDescription,
            Self::NormalAgent => MessageId::SetupRuntimePresetNormalAgentDescription,
            Self::HighTrustLocal => MessageId::SetupRuntimePresetHighTrustDescription,
        }
    }

    pub fn default_mode(self) -> &'static str {
        match self {
            Self::AskFirst => "plan",
            Self::NormalAgent | Self::HighTrustLocal => "agent",
        }
    }

    pub fn permission_posture(self) -> &'static str {
        match self {
            Self::AskFirst | Self::NormalAgent => "ask",
            Self::HighTrustLocal => "full-access",
        }
    }

    pub fn approval_policy(self) -> Option<&'static str> {
        match self {
            Self::AskFirst | Self::NormalAgent => Some("on-request"),
            // Full Access lives in TUI settings; it is intentionally not a
            // top-level approval_policy value.
            Self::HighTrustLocal => None,
        }
    }

    pub fn allow_shell(self) -> bool {
        match self {
            Self::AskFirst => false,
            Self::NormalAgent | Self::HighTrustLocal => true,
        }
    }

    pub fn sandbox_mode(self) -> &'static str {
        match self {
            Self::AskFirst => "read-only",
            Self::NormalAgent => "workspace-write",
            Self::HighTrustLocal => "danger-full-access",
        }
    }

    pub fn result_summary(self) -> String {
        let approval = self
            .approval_policy()
            .unwrap_or("unset (Full Access saved in TUI settings)");
        format!(
            "preset={}, default_mode={}, permission_posture={}, approval_policy={}, allow_shell={}, sandbox_mode={}, network=unchanged, trust=unchanged",
            self.id(),
            self.display_mode(),
            self.permission_posture(),
            approval,
            self.allow_shell(),
            self.sandbox_mode()
        )
    }

    fn display_mode(self) -> &'static str {
        match self {
            Self::AskFirst => "plan",
            Self::NormalAgent => "act",
            Self::HighTrustLocal => "act + full-access",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupConstitutionFileState {
    NotChecked,
    Missing,
    Loaded,
    Empty,
    Invalid,
    Unreadable,
    PathError,
}

impl SetupConstitutionFileState {
    fn load() -> Self {
        match UserConstitution::path() {
            Ok(path) => Self::from_load(&UserConstitution::load_from(&path)),
            Err(_) => Self::PathError,
        }
    }

    fn from_load(load: &UserConstitutionLoad) -> Self {
        match load {
            UserConstitutionLoad::Missing => Self::Missing,
            UserConstitutionLoad::Empty => Self::Empty,
            UserConstitutionLoad::Invalid(_) => Self::Invalid,
            UserConstitutionLoad::Unreadable(_) => Self::Unreadable,
            UserConstitutionLoad::Loaded(_) => Self::Loaded,
        }
    }

    fn label(self, choice: ConstitutionChoice, locale: Locale) -> Cow<'static, str> {
        let id = match self {
            Self::NotChecked => MessageId::SetupConstitutionFileNotChecked,
            Self::Missing => MessageId::SetupConstitutionFileMissing,
            Self::Loaded if choice == ConstitutionChoice::GuidedCustom => {
                MessageId::SetupConstitutionFileLoadedSelected
            }
            Self::Loaded if choice.is_explicit() => MessageId::SetupConstitutionFileLoadedInactive,
            Self::Loaded => MessageId::SetupConstitutionFileLoadedUnselected,
            Self::Empty => MessageId::SetupConstitutionFileEmpty,
            Self::Invalid => MessageId::SetupConstitutionFileInvalid,
            Self::Unreadable => MessageId::SetupConstitutionFileUnreadable,
            Self::PathError => MessageId::SetupConstitutionFilePathError,
        };
        tr(locale, id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupExpertOverrideState {
    NotChecked,
    Missing,
    Active,
    Disabled,
    Empty,
    Unreadable,
    PathError,
}

impl SetupExpertOverrideState {
    fn load() -> Self {
        let Some(path) = expert_override_path() else {
            return Self::PathError;
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) if raw.trim().is_empty() => Self::Empty,
            Ok(_) if base_prompt_override_opt_in() => Self::Active,
            Ok(_) => Self::Disabled,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::Missing,
            Err(_) => Self::Unreadable,
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::NotChecked => tr(locale, MessageId::SetupExpertOverrideNotChecked),
            Self::Missing => tr(locale, MessageId::SetupExpertOverrideMissing),
            Self::Active => tr(locale, MessageId::SetupExpertOverrideActive),
            Self::Disabled => tr(locale, MessageId::SetupExpertOverrideDisabled)
                .replace("{env}", BASE_PROMPT_OVERRIDE_OPT_IN_ENV)
                .into(),
            Self::Empty => tr(locale, MessageId::SetupExpertOverrideEmpty),
            Self::Unreadable => tr(locale, MessageId::SetupExpertOverrideUnreadable),
            Self::PathError => tr(locale, MessageId::SetupExpertOverridePathError),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuidedConstitutionDraft {
    purpose: GuidedPurpose,
    autonomy: AutonomyPreference,
    evidence: GuidedEvidence,
    communication: GuidedCommunication,
    privacy: GuidedPrivacy,
    principles: GuidedPrinciples,
}

impl Default for GuidedConstitutionDraft {
    fn default() -> Self {
        Self {
            purpose: GuidedPurpose::Coding,
            autonomy: AutonomyPreference::Balanced,
            evidence: GuidedEvidence::TestsAndReceipts,
            communication: GuidedCommunication::Concise,
            privacy: GuidedPrivacy::StandardCare,
            principles: GuidedPrinciples::ScopedChanges,
        }
    }
}

impl GuidedConstitutionDraft {
    fn cycle(&mut self, key: char) -> bool {
        match key {
            '1' => self.purpose = self.purpose.next(),
            '2' => self.autonomy = next_guided_autonomy(self.autonomy),
            '3' => self.evidence = self.evidence.next(),
            '4' => self.communication = self.communication.next(),
            '5' => self.privacy = self.privacy.next(),
            '6' => self.principles = self.principles.next(),
            _ => return false,
        }
        true
    }

    fn to_constitution_with_freeform(
        self,
        locale: Locale,
        freeform_note: Option<&str>,
    ) -> UserConstitution {
        let mut notes = self.notes(locale);
        if let Some(note) = freeform_note.map(str::trim).filter(|note| !note.is_empty()) {
            let own_words = match locale {
                Locale::Ja => format!(
                    "\nユーザー自由原則：{}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::ZhHans => format!(
                    "\n用户自定义准则：{}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::ZhHant => format!(
                    "\n使用者自由原則：{}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::PtBr => format!(
                    "\nPrincípio livre do usuário: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Es419 => format!(
                    "\nPrincipio libre del usuario: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Vi => format!(
                    "\nNguyên tắc tự do của người dùng: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Ko => format!(
                    "\n사용자 자유 원칙: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Ca => format!(
                    "\nPrincipi lliure de l'usuari: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::De => format!(
                    "\nFreitext-Prinzip des Nutzers: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Fr => format!(
                    "\nPrincipe en texte libre de l'utilisateur : {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Id => format!(
                    "\nPrinsip bebas pengguna: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Hi => format!(
                    "\nउपयोगकर्ता मुक्त-पाठ सिद्धांत: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Ru => format!(
                    "\nСвободный принцип пользователя: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                Locale::Uk => format!(
                    "\nВільний принцип користувача: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
                _ => format!(
                    "\nUser freeform principle: {}",
                    bounded_freeform_note(note, FREEFORM_NOTE_MAX)
                ),
            };
            notes.push_str(&own_words);
        }
        UserConstitution {
            language: Some(locale.tag().to_string()),
            about: Some(self.purpose.about(locale).to_string()),
            working_style: vec![
                self.purpose.working_style(locale).to_string(),
                self.communication.working_style(locale).to_string(),
                self.evidence.working_style(locale).to_string(),
                self.privacy.working_style(locale).to_string(),
            ],
            priorities: vec![
                authority_priority(locale).to_string(),
                autonomy_priority(self.autonomy, locale).to_string(),
                self.privacy.escalation_rule(locale).to_string(),
            ],
            autonomy_preference: self.autonomy,
            notes: Some(notes),
            ..UserConstitution::default()
        }
    }

    fn notes(self, locale: Locale) -> String {
        let notes = tr(locale, MessageId::SetupGuidedNotes);
        notes
            .replace("{purpose}", &self.purpose.label(locale))
            .replace("{initiative}", autonomy_label(self.autonomy, locale))
            .replace("{evidence}", &self.evidence.label(locale))
            .replace("{communication}", self.communication.label(locale))
            .replace("{privacy}", self.privacy.label(locale))
            .replace("{principles}", self.principles.label(locale))
            .replace("{notes}", self.principles.note(locale))
            .to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuidedPurpose {
    Coding,
    Research,
    Operations,
    Mixed,
}

impl GuidedPurpose {
    fn next(self) -> Self {
        match self {
            Self::Coding => Self::Research,
            Self::Research => Self::Operations,
            Self::Operations => Self::Mixed,
            Self::Mixed => Self::Coding,
        }
    }

    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::Coding => tr(locale, MessageId::SetupGuidedPurposeCoding),
            Self::Research => tr(locale, MessageId::SetupGuidedPurposeResearch),
            Self::Operations => tr(locale, MessageId::SetupGuidedPurposeOperations),
            Self::Mixed => tr(locale, MessageId::SetupGuidedPurposeMixed),
        }
    }

    fn about(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::Coding => tr(locale, MessageId::SetupGuidedPurposeAboutCoding),
            Self::Research => tr(locale, MessageId::SetupGuidedPurposeAboutResearch),
            Self::Operations => tr(locale, MessageId::SetupGuidedPurposeAboutOperations),
            Self::Mixed => tr(locale, MessageId::SetupGuidedPurposeAboutMixed),
        }
    }

    fn working_style(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::Coding => tr(locale, MessageId::SetupGuidedStyleCoding),
            Self::Research => tr(locale, MessageId::SetupGuidedStyleResearch),
            Self::Operations => tr(locale, MessageId::SetupGuidedStyleOperations),
            Self::Mixed => tr(locale, MessageId::SetupGuidedStyleMixed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuidedEvidence {
    Assumptions,
    TestsAndReceipts,
    ReleaseReceipts,
}

impl GuidedEvidence {
    fn next(self) -> Self {
        match self {
            Self::Assumptions => Self::TestsAndReceipts,
            Self::TestsAndReceipts => Self::ReleaseReceipts,
            Self::ReleaseReceipts => Self::Assumptions,
        }
    }

    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::Assumptions => tr(locale, MessageId::SetupGuidedEvidenceAssumptions),
            Self::TestsAndReceipts => tr(locale, MessageId::SetupGuidedEvidenceTestsAndReceipts),
            Self::ReleaseReceipts => tr(locale, MessageId::SetupGuidedEvidenceReleaseReceipts),
        }
    }

    fn working_style(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::Assumptions) => {
                "完了を主張する前に、前提、不明点、残るリスクを要約する。"
            }
            (Locale::Ja, Self::TestsAndReceipts) => {
                "不確実性を減らせるときは、コマンド、テスト、スクリーンショット、引用で具体的に検証する。"
            }
            (Locale::Ja, Self::ReleaseReceipts) => {
                "重要な主張とリリース証拠には、ファイル、コマンド、スクリーンショット、CI、出典を示す。"
            }
            (Locale::ZhHans, Self::Assumptions) => "在宣称完成前总结假设、未知和剩余风险。",
            (Locale::ZhHans, Self::TestsAndReceipts) => {
                "在能降低不确定性时，用命令、测试、截图或引用给出具体验证。"
            }
            (Locale::ZhHans, Self::ReleaseReceipts) => {
                "对重要结论和发布证据标注文件、命令、截图、CI 或来源。"
            }
            (Locale::ZhHant, Self::Assumptions) => "在宣稱完成前總結假設、未知和剩餘風險。",
            (Locale::ZhHant, Self::TestsAndReceipts) => {
                "在能降低不確定性時，用命令、測試、截圖或引用給出具體驗證。"
            }
            (Locale::ZhHant, Self::ReleaseReceipts) => {
                "對重要結論和發布證據標註檔案、命令、截圖、CI 或來源。"
            }
            (Locale::PtBr, Self::Assumptions) => {
                "Resuma premissas, desconhecidos e risco restante antes de dizer que concluiu."
            }
            (Locale::PtBr, Self::TestsAndReceipts) => {
                "Use comandos, testes, screenshots ou citações quando reduzirem a incerteza."
            }
            (Locale::PtBr, Self::ReleaseReceipts) => {
                "Cite arquivos, comandos, screenshots, CI ou fontes para afirmações materiais e evidência de release."
            }
            (Locale::Es419, Self::Assumptions) => {
                "Resume supuestos, incógnitas y riesgo restante antes de afirmar que terminaste."
            }
            (Locale::Es419, Self::TestsAndReceipts) => {
                "Usa comandos, pruebas, capturas o citas cuando reduzcan materialmente la incertidumbre."
            }
            (Locale::Es419, Self::ReleaseReceipts) => {
                "Cita archivos, comandos, capturas, CI o fuentes para afirmaciones materiales y evidencia de release."
            }
            (Locale::Vi, Self::Assumptions) => {
                "Tóm tắt giả định, điều chưa biết và rủi ro còn lại trước khi tuyên bố hoàn tất."
            }
            (Locale::Vi, Self::TestsAndReceipts) => {
                "Dùng lệnh, kiểm thử, ảnh chụp hoặc trích dẫn khi chúng giảm đáng kể bất định."
            }
            (Locale::Vi, Self::ReleaseReceipts) => {
                "Trích dẫn tệp, lệnh, ảnh chụp, CI hoặc nguồn cho tuyên bố quan trọng và bằng chứng phát hành."
            }
            (Locale::Ko, Self::Assumptions) => {
                "완료를 주장하기 전에 가정, 불확실한 점, 남은 위험을 요약한다."
            }
            (Locale::Ko, Self::TestsAndReceipts) => {
                "불확실성을 실질적으로 줄일 수 있을 때는 명령어, 테스트, 스크린샷, 인용으로 구체적으로 검증한다."
            }
            (Locale::Ko, Self::ReleaseReceipts) => {
                "중요한 주장과 릴리스 근거에는 파일 경로, 명령어, 스크린샷, CI, 출처를 제시한다."
            }
            (Locale::Ca, Self::Assumptions) => {
                "Resumeix supòsits, incògnites i risc pendent abans de dir que has acabat."
            }
            (Locale::Ca, Self::TestsAndReceipts) => {
                "Fes servir ordres, tests, captures de pantalla o citacions quan redueixin materialment la incertesa."
            }
            (Locale::Ca, Self::ReleaseReceipts) => {
                "Cita rutes de fitxers, ordres, captures de pantalla, CI o fonts per a afirmacions materials i evidència de release."
            }
            (Locale::De, Self::Assumptions) => {
                "Fasse Annahmen, Unbekannte und Restrisiken zusammen, bevor du Fertigstellung behauptest."
            }
            (Locale::De, Self::TestsAndReceipts) => {
                "Nutze Befehle, Tests, Screenshots oder Zitate, wenn sie die Unsicherheit wesentlich verringern."
            }
            (Locale::De, Self::ReleaseReceipts) => {
                "Nenne Dateipfade, Befehle, Screenshots, CI oder Quellen für wesentliche Aussagen und Release-Nachweise."
            }
            (Locale::Fr, Self::Assumptions) => {
                "Résumez les hypothèses, les inconnues et le risque restant avant d'annoncer la fin du travail."
            }
            (Locale::Fr, Self::TestsAndReceipts) => {
                "Utilisez commandes, tests, captures d'écran ou citations quand ils réduisent sensiblement l'incertitude."
            }
            (Locale::Fr, Self::ReleaseReceipts) => {
                "Citez chemins de fichiers, commandes, captures d'écran, CI ou sources pour les affirmations importantes et les preuves de release."
            }
            (Locale::Id, Self::Assumptions) => {
                "Ringkas asumsi, hal yang belum diketahui, dan risiko tersisa sebelum mengklaim selesai."
            }
            (Locale::Id, Self::TestsAndReceipts) => {
                "Gunakan perintah, tes, tangkapan layar, atau kutipan bila secara nyata mengurangi ketidakpastian."
            }
            (Locale::Id, Self::ReleaseReceipts) => {
                "Kutip path file, perintah, tangkapan layar, CI, atau sumber untuk klaim material dan bukti rilis."
            }
            (Locale::Hi, Self::Assumptions) => {
                "पूर्णता का दावा करने से पहले धारणाएँ, अज्ञात बातें और शेष जोखिम सारांशित करें।"
            }
            (Locale::Hi, Self::TestsAndReceipts) => {
                "जब वे अनिश्चितता सार्थक रूप से घटाएँ तो कमांड, टेस्ट, स्क्रीनशॉट या उद्धरण उपयोग करें।"
            }
            (Locale::Hi, Self::ReleaseReceipts) => {
                "महत्वपूर्ण दावों और रिलीज़ साक्ष्य के लिए फ़ाइल पथ, कमांड, स्क्रीनशॉट, CI या स्रोत उद्धृत करें।"
            }
            (Locale::Ru, Self::Assumptions) => {
                "Прежде чем заявить о завершении, перечислите предположения, неизвестные и оставшиеся риски."
            }
            (Locale::Ru, Self::TestsAndReceipts) => {
                "Используйте команды, тесты, скриншоты или цитаты, когда они существенно снижают неопределённость."
            }
            (Locale::Ru, Self::ReleaseReceipts) => {
                "Указывайте пути файлов, команды, скриншоты, CI или источники для существенных утверждений и доказательств релиза."
            }
            (Locale::Uk, Self::Assumptions) => {
                "Перш ніж заявити про завершення, підсумуйте припущення, невідомі та залишкові ризики."
            }
            (Locale::Uk, Self::TestsAndReceipts) => {
                "Використовуйте команди, тести, скриншоти або цитати, коли вони суттєво зменшують невизначеність."
            }
            (Locale::Uk, Self::ReleaseReceipts) => {
                "Посилайтеся на шляхи файлів, команди, скриншоти, CI або джерела для суттєвих тверджень і доказів релізу."
            }
            (_, Self::Assumptions) => {
                "Summarize assumptions, unknowns, and remaining risk before claiming completion."
            }
            (_, Self::TestsAndReceipts) => {
                "Use commands, tests, screenshots, or citations when they materially reduce uncertainty."
            }
            (_, Self::ReleaseReceipts) => {
                "Cite file paths, commands, screenshots, CI, or sources for material claims and release evidence."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuidedCommunication {
    Concise,
    Teaching,
    Direct,
}

impl GuidedCommunication {
    fn next(self) -> Self {
        match self {
            Self::Concise => Self::Teaching,
            Self::Teaching => Self::Direct,
            Self::Direct => Self::Concise,
        }
    }

    fn label(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::Concise) => "簡潔",
            (Locale::Ja, Self::Teaching) => "説明重視",
            (Locale::Ja, Self::Direct) => "直接的",
            (Locale::ZhHans, Self::Concise) => "简洁",
            (Locale::ZhHans, Self::Teaching) => "教学式",
            (Locale::ZhHans, Self::Direct) => "直接",
            (Locale::ZhHant, Self::Concise) => "簡潔",
            (Locale::ZhHant, Self::Teaching) => "教學式",
            (Locale::ZhHant, Self::Direct) => "直接",
            (Locale::PtBr, Self::Concise) => "conciso",
            (Locale::PtBr, Self::Teaching) => "didático",
            (Locale::PtBr, Self::Direct) => "direto",
            (Locale::Es419, Self::Concise) => "conciso",
            (Locale::Es419, Self::Teaching) => "didáctico",
            (Locale::Es419, Self::Direct) => "directo",
            (Locale::Vi, Self::Concise) => "ngắn gọn",
            (Locale::Vi, Self::Teaching) => "giảng giải",
            (Locale::Vi, Self::Direct) => "trực tiếp",
            (Locale::Ko, Self::Concise) => "간결함",
            (Locale::Ko, Self::Teaching) => "설명 중심",
            (Locale::Ko, Self::Direct) => "직설적",
            (Locale::Ca, Self::Concise) => "concís",
            (Locale::Ca, Self::Teaching) => "didàctic",
            (Locale::Ca, Self::Direct) => "directe",
            (Locale::De, Self::Concise) => "prägnant",
            (Locale::De, Self::Teaching) => "lehrend",
            (Locale::De, Self::Direct) => "direkt",
            (Locale::Fr, Self::Concise) => "concis",
            (Locale::Fr, Self::Teaching) => "pédagogique",
            (Locale::Fr, Self::Direct) => "direct",
            (Locale::Id, Self::Concise) => "ringkas",
            (Locale::Id, Self::Teaching) => "mengajar",
            (Locale::Id, Self::Direct) => "langsung",
            (Locale::Hi, Self::Concise) => "संक्षिप्त",
            (Locale::Hi, Self::Teaching) => "शिक्षणपरक",
            (Locale::Hi, Self::Direct) => "सीधा",
            (Locale::Ru, Self::Concise) => "краткий",
            (Locale::Ru, Self::Teaching) => "обучающий",
            (Locale::Ru, Self::Direct) => "прямой",
            (Locale::Uk, Self::Concise) => "стислий",
            (Locale::Uk, Self::Teaching) => "навчальний",
            (Locale::Uk, Self::Direct) => "прямий",
            (_, Self::Concise) => "concise",
            (_, Self::Teaching) => "teaching",
            (_, Self::Direct) => "direct",
        }
    }

    fn working_style(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::Concise) => "更新は簡潔にし、重要なトレードオフだけ短く説明する。",
            (Locale::Ja, Self::Teaching) => {
                "重要な推論とトレードオフを、ユーザーが仕組みを理解できる程度に説明する。"
            }
            (Locale::Ja, Self::Direct) => {
                "阻塞、リスク、不確実性を直接述べ、装飾的な文案を避ける。"
            }
            (Locale::ZhHans, Self::Concise) => "保持更新简洁，并只解释重要取舍。",
            (Locale::ZhHans, Self::Teaching) => "解释关键推理和取舍，让用户能理解系统。",
            (Locale::ZhHans, Self::Direct) => "直接说明阻塞、风险和不确定性，避免装饰性文案。",
            (Locale::ZhHant, Self::Concise) => "保持更新簡潔，並只解釋重要取捨。",
            (Locale::ZhHant, Self::Teaching) => "解釋關鍵推理和取捨，讓使用者能理解系統。",
            (Locale::ZhHant, Self::Direct) => "直接說明阻塞、風險和不確定性，避免裝飾性文案。",
            (Locale::PtBr, Self::Concise) => {
                "Mantenha atualizações concisas e explique brevemente só os tradeoffs importantes."
            }
            (Locale::PtBr, Self::Teaching) => {
                "Explique raciocínio e tradeoffs principais o bastante para o usuário entender o sistema."
            }
            (Locale::PtBr, Self::Direct) => {
                "Seja direto sobre bloqueios, risco e incerteza; evite texto ornamental."
            }
            (Locale::Es419, Self::Concise) => {
                "Mantén las actualizaciones concisas y explica brevemente solo los tradeoffs importantes."
            }
            (Locale::Es419, Self::Teaching) => {
                "Explica el razonamiento y los tradeoffs clave lo suficiente para que el usuario entienda el sistema."
            }
            (Locale::Es419, Self::Direct) => {
                "Sé directo sobre bloqueos, riesgo e incertidumbre; evita texto ornamental."
            }
            (Locale::Vi, Self::Concise) => {
                "Giữ cập nhật ngắn gọn và chỉ giải thích ngắn các đánh đổi quan trọng."
            }
            (Locale::Vi, Self::Teaching) => {
                "Giải thích suy luận và đánh đổi chính đủ để người dùng hiểu hệ thống."
            }
            (Locale::Vi, Self::Direct) => {
                "Nói thẳng về điểm chặn, rủi ro và bất định; tránh câu chữ trang trí."
            }
            (Locale::Ko, Self::Concise) => {
                "업데이트는 간결하게 유지하고, 중요한 트레이드오프만 짧게 설명한다."
            }
            (Locale::Ko, Self::Teaching) => {
                "사용자가 시스템을 이해할 수 있을 만큼 핵심 추론과 트레이드오프를 설명한다."
            }
            (Locale::Ko, Self::Direct) => {
                "차단 요인, 위험, 불확실성을 직설적으로 말하고 장식적인 표현은 피한다."
            }
            (Locale::Ca, Self::Concise) => {
                "Mantén les actualitzacions concises i explica breument només els compromisos importants."
            }
            (Locale::Ca, Self::Teaching) => {
                "Explica el raonament i els compromisos clau prou perquè l'usuari pugui entendre el sistema."
            }
            (Locale::Ca, Self::Direct) => {
                "Sigues directe sobre bloquejos, risc i incertesa; evita el text ornamental."
            }
            (Locale::De, Self::Concise) => {
                "Halte Aktualisierungen knapp und erkläre wichtige Trade-offs nur kurz."
            }
            (Locale::De, Self::Teaching) => {
                "Erkläre zentrale Begründungen und Trade-offs so weit, dass der Nutzer das System verstehen kann."
            }
            (Locale::De, Self::Direct) => {
                "Sei direkt bei Blockern, Risiken und Unsicherheit; vermeide dekorative Formulierungen."
            }
            (Locale::Fr, Self::Concise) => {
                "Gardez les mises à jour concises et n'expliquez que brièvement les arbitrages importants."
            }
            (Locale::Fr, Self::Teaching) => {
                "Expliquez le raisonnement et les arbitrages clés assez pour que l'utilisateur comprenne le système."
            }
            (Locale::Fr, Self::Direct) => {
                "Soyez direct sur les blocages, les risques et l'incertitude ; évitez le texte ornemental."
            }
            (Locale::Id, Self::Concise) => {
                "Jaga pembaruan tetap ringkas dan jelaskan tradeoff penting secara singkat."
            }
            (Locale::Id, Self::Teaching) => {
                "Jelaskan penalaran dan tradeoff kunci secukupnya agar pengguna dapat memahami sistem."
            }
            (Locale::Id, Self::Direct) => {
                "Bicara langsung soal penghambat, risiko, dan ketidakpastian; hindari teks hiasan."
            }
            (Locale::Hi, Self::Concise) => "अपडेट संक्षिप्त रखें और महत्वपूर्ण ट्रेडऑफ़ संक्षेप में समझाएँ।",
            (Locale::Hi, Self::Teaching) => {
                "मुख्य तर्क और ट्रेडऑफ़ इतना समझाएँ कि उपयोगकर्ता सिस्टम समझ सके।"
            }
            (Locale::Hi, Self::Direct) => {
                "रुकावटों, जोखिम और अनिश्चितता के बारे में सीधे बोलें; सजावटी भाषा से बचें।"
            }
            (Locale::Ru, Self::Concise) => {
                "Держите обновления краткими и лишь коротко поясняйте важные компромиссы."
            }
            (Locale::Ru, Self::Teaching) => {
                "Объясняйте ключевые рассуждения и компромиссы настолько, чтобы пользователь мог понять систему."
            }
            (Locale::Ru, Self::Direct) => {
                "Говорите прямо о блокерах, рисках и неопределённости; избегайте декоративных формулировок."
            }
            (Locale::Uk, Self::Concise) => {
                "Тримайте оновлення стислими й лише коротко пояснюйте важливі компроміси."
            }
            (Locale::Uk, Self::Teaching) => {
                "Пояснюйте ключові міркування та компроміси настільки, щоб користувач міг зрозуміти систему."
            }
            (Locale::Uk, Self::Direct) => {
                "Говоріть прямо про блокери, ризики та невизначеність; уникайте декоративних формулювань."
            }
            (_, Self::Concise) => "Keep updates concise and explain important tradeoffs briefly.",
            (_, Self::Teaching) => {
                "Explain key reasoning and tradeoffs enough that the user can learn the system."
            }
            (_, Self::Direct) => {
                "Be direct about blockers, risk, and uncertainty; avoid ornamental copy."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuidedPrivacy {
    StandardCare,
    StrictBoundaries,
    ProjectLocal,
}

impl GuidedPrivacy {
    fn next(self) -> Self {
        match self {
            Self::StandardCare => Self::StrictBoundaries,
            Self::StrictBoundaries => Self::ProjectLocal,
            Self::ProjectLocal => Self::StandardCare,
        }
    }

    fn label(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::StandardCare) => "標準保護",
            (Locale::Ja, Self::StrictBoundaries) => "厳格な境界",
            (Locale::Ja, Self::ProjectLocal) => "プロジェクト内メモリ",
            (Locale::ZhHans, Self::StandardCare) => "标准保护",
            (Locale::ZhHans, Self::StrictBoundaries) => "严格边界",
            (Locale::ZhHans, Self::ProjectLocal) => "项目内记忆",
            (Locale::ZhHant, Self::StandardCare) => "標準保護",
            (Locale::ZhHant, Self::StrictBoundaries) => "嚴格邊界",
            (Locale::ZhHant, Self::ProjectLocal) => "專案內記憶",
            (Locale::PtBr, Self::StandardCare) => "cuidado padrão",
            (Locale::PtBr, Self::StrictBoundaries) => "limites estritos",
            (Locale::PtBr, Self::ProjectLocal) => "memória local do projeto",
            (Locale::Es419, Self::StandardCare) => "cuidado estándar",
            (Locale::Es419, Self::StrictBoundaries) => "límites estrictos",
            (Locale::Es419, Self::ProjectLocal) => "memoria local del proyecto",
            (Locale::Vi, Self::StandardCare) => "bảo vệ tiêu chuẩn",
            (Locale::Vi, Self::StrictBoundaries) => "ranh giới nghiêm ngặt",
            (Locale::Vi, Self::ProjectLocal) => "bộ nhớ trong dự án",
            (Locale::Ko, Self::StandardCare) => "표준 보호",
            (Locale::Ko, Self::StrictBoundaries) => "엄격한 경계",
            (Locale::Ko, Self::ProjectLocal) => "프로젝트 내 메모리",
            (Locale::Ca, Self::StandardCare) => "cura estàndard",
            (Locale::Ca, Self::StrictBoundaries) => "límits estrictes",
            (Locale::Ca, Self::ProjectLocal) => "memòria local del projecte",
            (Locale::De, Self::StandardCare) => "Standardvorsorge",
            (Locale::De, Self::StrictBoundaries) => "strenge Grenzen",
            (Locale::De, Self::ProjectLocal) => "projektlokaler Speicher",
            (Locale::Fr, Self::StandardCare) => "soin standard",
            (Locale::Fr, Self::StrictBoundaries) => "limites strictes",
            (Locale::Fr, Self::ProjectLocal) => "mémoire locale au projet",
            (Locale::Id, Self::StandardCare) => "perlindungan standar",
            (Locale::Id, Self::StrictBoundaries) => "batasan ketat",
            (Locale::Id, Self::ProjectLocal) => "memori lokal proyek",
            (Locale::Hi, Self::StandardCare) => "मानक सावधानी",
            (Locale::Hi, Self::StrictBoundaries) => "सख्त सीमाएँ",
            (Locale::Hi, Self::ProjectLocal) => "प्रोजेक्ट-स्थानीय मेमोरी",
            (Locale::Ru, Self::StandardCare) => "стандартная осторожность",
            (Locale::Ru, Self::StrictBoundaries) => "строгие границы",
            (Locale::Ru, Self::ProjectLocal) => "память внутри проекта",
            (Locale::Uk, Self::StandardCare) => "стандартна обережність",
            (Locale::Uk, Self::StrictBoundaries) => "суворі межі",
            (Locale::Uk, Self::ProjectLocal) => "пам'ять у межах проєкту",
            (_, Self::StandardCare) => "standard care",
            (_, Self::StrictBoundaries) => "strict boundaries",
            (_, Self::ProjectLocal) => "project-local memory",
        }
    }

    fn working_style(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::StandardCare) => {
                "秘密情報、ユーザーファイル、Git 履歴、本番システム、コスト、プライバシー、時間を保護する。"
            }
            (Locale::Ja, Self::StrictBoundaries) => {
                "秘密、個人データ、認証情報、本番状態、資金、公開操作は、先に確認する境界として扱う。"
            }
            (Locale::Ja, Self::ProjectLocal) => {
                "プロジェクト固有の文脈はプロジェクト内に留め、明示要求がない限りメモリへ書かない。"
            }
            (Locale::ZhHans, Self::StandardCare) => {
                "保护密钥、用户文件、Git 历史、生产系统、成本、隐私和时间。"
            }
            (Locale::ZhHans, Self::StrictBoundaries) => {
                "把密钥、个人数据、凭据、生产状态、资金和发布动作视为先确认边界。"
            }
            (Locale::ZhHans, Self::ProjectLocal) => {
                "项目特定上下文留在项目内，除非明确要求，否则不要写入记忆。"
            }
            (Locale::ZhHant, Self::StandardCare) => {
                "保護密鑰、使用者檔案、Git 歷史、生產系統、成本、隱私和時間。"
            }
            (Locale::ZhHant, Self::StrictBoundaries) => {
                "把密鑰、個人資料、憑據、生產狀態、資金和發布動作視為先確認邊界。"
            }
            (Locale::ZhHant, Self::ProjectLocal) => {
                "專案特定上下文留在專案內，除非明確要求，否則不要寫入記憶。"
            }
            (Locale::PtBr, Self::StandardCare) => {
                "Proteja segredos, arquivos do usuário, histórico git, produção, custo, privacidade e tempo."
            }
            (Locale::PtBr, Self::StrictBoundaries) => {
                "Trate segredos, dados pessoais, credenciais, estado de produção, dinheiro e publicações como limites de confirmação."
            }
            (Locale::PtBr, Self::ProjectLocal) => {
                "Mantenha contexto específico do projeto no projeto; evite gravar na memória sem pedido explícito."
            }
            (Locale::Es419, Self::StandardCare) => {
                "Protege secretos, archivos del usuario, historial git, producción, costo, privacidad y tiempo."
            }
            (Locale::Es419, Self::StrictBoundaries) => {
                "Trata secretos, datos personales, credenciales, estado de producción, dinero y publicaciones como límites de confirmación."
            }
            (Locale::Es419, Self::ProjectLocal) => {
                "Mantén el contexto específico del proyecto en el proyecto; evita llevarlo a memoria sin pedido explícito."
            }
            (Locale::Vi, Self::StandardCare) => {
                "Bảo vệ bí mật, tệp người dùng, lịch sử git, hệ thống sản xuất, chi phí, riêng tư và thời gian."
            }
            (Locale::Vi, Self::StrictBoundaries) => {
                "Xem bí mật, dữ liệu cá nhân, thông tin xác thực, trạng thái sản xuất, tiền và xuất bản là ranh giới cần xác nhận."
            }
            (Locale::Vi, Self::ProjectLocal) => {
                "Giữ ngữ cảnh riêng của dự án trong dự án; tránh ghi vào bộ nhớ nếu không được yêu cầu rõ."
            }
            (Locale::Ko, Self::StandardCare) => {
                "비밀 정보, 사용자 파일, Git 이력, 프로덕션 시스템, 비용, 프라이버시, 시간을 보호한다."
            }
            (Locale::Ko, Self::StrictBoundaries) => {
                "비밀 정보, 개인 데이터, 자격 증명, 프로덕션 상태, 자금, 게시 작업은 먼저 확인하는 경계로 취급한다."
            }
            (Locale::Ko, Self::ProjectLocal) => {
                "프로젝트 고유 맥락은 프로젝트 안에 두고, 명시적으로 요청받지 않는 한 메모리에 쓰지 않는다."
            }
            (Locale::Ca, Self::StandardCare) => {
                "Protegeix secrets, fitxers de l'usuari, historial de git, sistemes de producció, cost, privacitat i temps."
            }
            (Locale::Ca, Self::StrictBoundaries) => {
                "Tracta secrets, dades personals, credencials, estat de producció, diners i accions de publicació com a límits que cal confirmar primer."
            }
            (Locale::Ca, Self::ProjectLocal) => {
                "Mantén el context específic del projecte dins del projecte; evita portar-lo a la memòria si no se't demana explícitament."
            }
            (Locale::De, Self::StandardCare) => {
                "Schütze Geheimnisse, Nutzerdateien, Git-Verlauf, Produktionssysteme, Kosten, Privatsphäre und Zeit."
            }
            (Locale::De, Self::StrictBoundaries) => {
                "Behandle Geheimnisse, persönliche Daten, Zugangsdaten, Produktionszustand, Geld und Veröffentlichungen als Grenzen, die erst bestätigt werden."
            }
            (Locale::De, Self::ProjectLocal) => {
                "Halte projektspezifischen Kontext im Projekt; vermeide es, sensible Details ohne ausdrückliche Bitte in den Speicher zu übernehmen."
            }
            (Locale::Fr, Self::StandardCare) => {
                "Protégez secrets, fichiers utilisateur, historique git, systèmes de production, coût, vie privée et temps."
            }
            (Locale::Fr, Self::StrictBoundaries) => {
                "Traitez secrets, données personnelles, identifiants, état de production, argent et publications comme des limites exigeant confirmation."
            }
            (Locale::Fr, Self::ProjectLocal) => {
                "Gardez le contexte propre au projet dans le projet ; évitez de l'écrire en mémoire sans demande explicite."
            }
            (Locale::Id, Self::StandardCare) => {
                "Lindungi rahasia, file pengguna, riwayat git, sistem produksi, biaya, privasi, dan waktu."
            }
            (Locale::Id, Self::StrictBoundaries) => {
                "Perlakukan rahasia, data pribadi, kredensial, status produksi, uang, dan tindakan publikasi sebagai batas yang harus dikonfirmasi dulu."
            }
            (Locale::Id, Self::ProjectLocal) => {
                "Simpan konteks khusus proyek di dalam proyek; hindari membawanya ke memori kecuali diminta secara eksplisit."
            }
            (Locale::Hi, Self::StandardCare) => {
                "रहस्यों, उपयोगकर्ता फ़ाइलों, git इतिहास, प्रोडक्शन सिस्टम, लागत, गोपनीयता और समय की रक्षा करें।"
            }
            (Locale::Hi, Self::StrictBoundaries) => {
                "रहस्यों, व्यक्तिगत डेटा, क्रेडेंशियल, प्रोडक्शन स्थिति, धन और प्रकाशन क्रियाओं को पहले-पुष्टि सीमाओं की तरह मानें।"
            }
            (Locale::Hi, Self::ProjectLocal) => {
                "प्रोजेक्ट-विशिष्ट संदर्भ प्रोजेक्ट के भीतर रखें; स्पष्ट अनुरोध के बिना संवेदनशील विवरण मेमोरी में न ले जाएँ।"
            }
            (Locale::Ru, Self::StandardCare) => {
                "Защищайте секреты, файлы пользователя, историю git, production-системы, затраты, приватность и время."
            }
            (Locale::Ru, Self::StrictBoundaries) => {
                "Считайте секреты, персональные данные, учётные данные, production-состояние, деньги и публикации границами, требующими подтверждения."
            }
            (Locale::Ru, Self::ProjectLocal) => {
                "Держите контекст, специфичный для проекта, внутри проекта; не переносите чувствительные детали в память без явного запроса."
            }
            (Locale::Uk, Self::StandardCare) => {
                "Захищайте секрети, файли користувача, історію git, production-системи, витрати, приватність і час."
            }
            (Locale::Uk, Self::StrictBoundaries) => {
                "Вважайте секрети, персональні дані, облікові дані, production-стан, гроші та публікації межами, що потребують підтвердження."
            }
            (Locale::Uk, Self::ProjectLocal) => {
                "Тримайте контекст, специфічний для проєкту, всередині проєкту; не переносьте чутливі деталі в пам'ять без явного запиту."
            }
            (_, Self::StandardCare) => {
                "Protect secrets, user files, git history, production systems, cost, privacy, and time."
            }
            (_, Self::StrictBoundaries) => {
                "Treat secrets, personal data, credentials, production state, money, and publish actions as stop-and-confirm boundaries."
            }
            (_, Self::ProjectLocal) => {
                "Keep project-specific context local; avoid carrying sensitive details into memory unless explicitly asked."
            }
        }
    }

    fn escalation_rule(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::StandardCare) => {
                "破壊的、高コスト、認証情報、公開、法務、セキュリティリスクのある操作の前に尋ねる。"
            }
            (Locale::Ja, Self::StrictBoundaries) => {
                "機微情報の読み取りや拡散、本番システム操作、支出、公開の前に停止して尋ねる。"
            }
            (Locale::Ja, Self::ProjectLocal) => {
                "プロジェクト詳細をメモリ、ワークスペース、古い引き継ぎへ持ち出す前に確認する。"
            }
            (Locale::ZhHans, Self::StandardCare) => {
                "遇到破坏性、高成本、凭据、发布、法律或安全风险操作时先询问。"
            }
            (Locale::ZhHans, Self::StrictBoundaries) => {
                "在读取或传播敏感信息、触碰生产系统、花费资金或发布内容前停止并询问。"
            }
            (Locale::ZhHans, Self::ProjectLocal) => {
                "需要跨项目记忆、复制项目细节或引用旧交接时，先确认这些上下文仍适用。"
            }
            (Locale::ZhHant, Self::StandardCare) => {
                "遇到破壞性、高成本、憑據、發布、法律或安全風險操作時先詢問。"
            }
            (Locale::ZhHant, Self::StrictBoundaries) => {
                "在讀取或傳播敏感資訊、觸碰生產系統、花費資金或發布內容前停止並詢問。"
            }
            (Locale::ZhHant, Self::ProjectLocal) => {
                "需要跨專案記憶、複製專案細節或引用舊交接時，先確認這些上下文仍適用。"
            }
            (Locale::PtBr, Self::StandardCare) => {
                "Pergunte antes de ações destrutivas, caras, com credenciais, publicação, risco legal ou de segurança."
            }
            (Locale::PtBr, Self::StrictBoundaries) => {
                "Pare e pergunte antes de ler ou espalhar dados sensíveis, tocar produção, gastar dinheiro ou publicar."
            }
            (Locale::PtBr, Self::ProjectLocal) => {
                "Confirme antes de levar detalhes do projeto para memória, workspaces ou handoffs antigos."
            }
            (Locale::Es419, Self::StandardCare) => {
                "Pregunta antes de acciones destructivas, costosas, con credenciales, publicación o riesgo legal/de seguridad."
            }
            (Locale::Es419, Self::StrictBoundaries) => {
                "Detente y pregunta antes de leer o difundir datos sensibles, tocar producción, gastar dinero o publicar."
            }
            (Locale::Es419, Self::ProjectLocal) => {
                "Confirma antes de llevar detalles del proyecto a memoria, workspaces o handoffs viejos."
            }
            (Locale::Vi, Self::StandardCare) => {
                "Hỏi trước các thao tác phá hủy, tốn kém, liên quan thông tin xác thực, xuất bản, pháp lý hoặc bảo mật."
            }
            (Locale::Vi, Self::StrictBoundaries) => {
                "Dừng và hỏi trước khi đọc/phát tán dữ liệu nhạy cảm, chạm sản xuất, chi tiền hoặc xuất bản."
            }
            (Locale::Vi, Self::ProjectLocal) => {
                "Xác nhận trước khi mang chi tiết dự án sang bộ nhớ, workspace khác hoặc handoff cũ."
            }
            (Locale::Ko, Self::StandardCare) => {
                "파괴적이거나, 비용이 크거나, 자격 증명, 게시, 법적, 보안 위험이 있는 작업 전에 먼저 물어본다."
            }
            (Locale::Ko, Self::StrictBoundaries) => {
                "민감 정보를 읽거나 퍼뜨리기 전, 프로덕션 시스템을 건드리기 전, 자금을 쓰거나 게시하기 전에 멈추고 물어본다."
            }
            (Locale::Ko, Self::ProjectLocal) => {
                "프로젝트 세부 정보를 메모리, 다른 워크스페이스, 오래된 인계 자료로 옮기기 전에 확인한다."
            }
            (Locale::Ca, Self::StandardCare) => {
                "Pregunta abans d'accions destructives, costoses, amb credencials, de publicació o amb risc legal o de seguretat."
            }
            (Locale::Ca, Self::StrictBoundaries) => {
                "Atura't i pregunta abans de llegir o difondre dades sensibles, tocar sistemes de producció, gastar diners o publicar."
            }
            (Locale::Ca, Self::ProjectLocal) => {
                "Confirma abans de portar detalls del projecte a la memòria, a altres espais de treball o a traspasos antics."
            }
            (Locale::De, Self::StandardCare) => {
                "Frage vor destruktiven, kostspieligen, zugangsdatenbezogenen, veröffentlichenden, rechtlichen oder sicherheitskritischen Aktionen."
            }
            (Locale::De, Self::StrictBoundaries) => {
                "Halte an und frage, bevor du sensible Daten liest oder verbreitest, Produktionssysteme anfasst, Geld ausgibst oder veröffentlichst."
            }
            (Locale::De, Self::ProjectLocal) => {
                "Bestätige, bevor du Projektdetails in Speicher, Workspaces oder veraltete Übergaben überträgst."
            }
            (Locale::Fr, Self::StandardCare) => {
                "Demandez avant toute action destructive, coûteuse, impliquant des identifiants, une publication, ou un risque juridique ou de sécurité."
            }
            (Locale::Fr, Self::StrictBoundaries) => {
                "Arrêtez et demandez avant de lire ou diffuser des données sensibles, de toucher aux systèmes de production, de dépenser de l'argent ou de publier."
            }
            (Locale::Fr, Self::ProjectLocal) => {
                "Confirmez avant de transporter des détails du projet vers la mémoire, d'autres espaces de travail ou d'anciens transferts."
            }
            (Locale::Id, Self::StandardCare) => {
                "Tanya sebelum tindakan destruktif, mahal, terkait kredensial, publikasi, hukum, atau berisiko keamanan."
            }
            (Locale::Id, Self::StrictBoundaries) => {
                "Berhenti dan tanya sebelum membaca atau menyebarkan data sensitif, menyentuh sistem produksi, membelanjakan uang, atau mempublikasikan."
            }
            (Locale::Id, Self::ProjectLocal) => {
                "Konfirmasi sebelum membawa detail proyek ke memori, workspace lain, atau handoff lama."
            }
            (Locale::Hi, Self::StandardCare) => {
                "विनाशकारी, उच्च-लागत, क्रेडेंशियल, प्रकाशन, कानूनी या सुरक्षा-जोखिम कार्यों से पहले पूछें।"
            }
            (Locale::Hi, Self::StrictBoundaries) => {
                "संवेदनशील डेटा पढ़ने या फैलाने, प्रोडक्शन सिस्टम छूने, धन खर्च करने या प्रकाशित करने से पहले रुककर पूछें।"
            }
            (Locale::Hi, Self::ProjectLocal) => {
                "प्रोजेक्ट विवरण मेमोरी, अन्य कार्यक्षेत्रों या पुराने हैंडऑफ़ में ले जाने से पहले पुष्टि करें।"
            }
            (Locale::Ru, Self::StandardCare) => {
                "Спрашивайте перед деструктивными, дорогими, связанными с учётными данными, публикацией, юридическими или угрожающими безопасности действиями."
            }
            (Locale::Ru, Self::StrictBoundaries) => {
                "Остановитесь и спросите, прежде чем читать или распространять чувствительные данные, трогать production-системы, тратить деньги или публиковать."
            }
            (Locale::Ru, Self::ProjectLocal) => {
                "Подтвердите, прежде чем переносить детали проекта в память, другие рабочие области или устаревшие передаточные заметки."
            }
            (Locale::Uk, Self::StandardCare) => {
                "Питайте перед руйнівними, дорогими, пов'язаними з обліковими даними, публікацією, юридичними чи небезпечними для безпеки діями."
            }
            (Locale::Uk, Self::StrictBoundaries) => {
                "Зупиніться й запитайте, перш ніж читати чи поширювати чутливі дані, чіпати production-системи, витрачати гроші або публікувати."
            }
            (Locale::Uk, Self::ProjectLocal) => {
                "Підтвердьте, перш ніж переносити деталі проєкту в пам'ять, інші робочі простори чи застарілі передаточні нотатки."
            }
            (_, Self::StandardCare) => {
                "Ask before destructive, high-cost, credential, publishing, legal, or security-risk actions."
            }
            (_, Self::StrictBoundaries) => {
                "Stop and ask before reading or spreading sensitive data, touching production systems, spending money, or publishing."
            }
            (_, Self::ProjectLocal) => {
                "Confirm before carrying project details across memory, workspaces, or stale handoffs."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuidedPrinciples {
    ScopedChanges,
    UserVoice,
    ReversibleOps,
}

impl GuidedPrinciples {
    fn next(self) -> Self {
        match self {
            Self::ScopedChanges => Self::UserVoice,
            Self::UserVoice => Self::ReversibleOps,
            Self::ReversibleOps => Self::ScopedChanges,
        }
    }

    fn label(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::ScopedChanges) => "小さく絞った変更",
            (Locale::Ja, Self::UserVoice) => "ユーザーの声を保つ",
            (Locale::Ja, Self::ReversibleOps) => "可逆手順",
            (Locale::ZhHans, Self::ScopedChanges) => "小范围改动",
            (Locale::ZhHans, Self::UserVoice) => "保留用户语气",
            (Locale::ZhHans, Self::ReversibleOps) => "可逆步骤",
            (Locale::ZhHant, Self::ScopedChanges) => "小範圍改動",
            (Locale::ZhHant, Self::UserVoice) => "保留使用者語氣",
            (Locale::ZhHant, Self::ReversibleOps) => "可逆步驟",
            (Locale::PtBr, Self::ScopedChanges) => "mudanças focadas",
            (Locale::PtBr, Self::UserVoice) => "preservar voz do usuário",
            (Locale::PtBr, Self::ReversibleOps) => "passos reversíveis",
            (Locale::Es419, Self::ScopedChanges) => "cambios acotados",
            (Locale::Es419, Self::UserVoice) => "preservar voz del usuario",
            (Locale::Es419, Self::ReversibleOps) => "pasos reversibles",
            (Locale::Vi, Self::ScopedChanges) => "thay đổi có phạm vi",
            (Locale::Vi, Self::UserVoice) => "giữ giọng người dùng",
            (Locale::Vi, Self::ReversibleOps) => "bước có thể đảo ngược",
            (Locale::Ko, Self::ScopedChanges) => "범위가 명확한 변경",
            (Locale::Ko, Self::UserVoice) => "사용자의 어조 유지",
            (Locale::Ko, Self::ReversibleOps) => "되돌릴 수 있는 단계",
            (Locale::Ca, Self::ScopedChanges) => "canvis acotats",
            (Locale::Ca, Self::UserVoice) => "veu de l'usuari",
            (Locale::Ca, Self::ReversibleOps) => "passos reversibles",
            (Locale::De, Self::ScopedChanges) => "begrenzte Änderungen",
            (Locale::De, Self::UserVoice) => "Stimme des Nutzers",
            (Locale::De, Self::ReversibleOps) => "reversible Schritte",
            (Locale::Fr, Self::ScopedChanges) => "changements ciblés",
            (Locale::Fr, Self::UserVoice) => "voix de l'utilisateur",
            (Locale::Fr, Self::ReversibleOps) => "étapes réversibles",
            (Locale::Id, Self::ScopedChanges) => "perubahan terbatas",
            (Locale::Id, Self::UserVoice) => "suara pengguna",
            (Locale::Id, Self::ReversibleOps) => "langkah reversibel",
            (Locale::Hi, Self::ScopedChanges) => "सीमित बदलाव",
            (Locale::Hi, Self::UserVoice) => "उपयोगकर्ता की आवाज़",
            (Locale::Hi, Self::ReversibleOps) => "उत्क्रमणीय चरण",
            (Locale::Ru, Self::ScopedChanges) => "ограниченные изменения",
            (Locale::Ru, Self::UserVoice) => "голос пользователя",
            (Locale::Ru, Self::ReversibleOps) => "обратимые шаги",
            (Locale::Uk, Self::ScopedChanges) => "обмежені зміни",
            (Locale::Uk, Self::UserVoice) => "голос користувача",
            (Locale::Uk, Self::ReversibleOps) => "оборотні кроки",
            (_, Self::ScopedChanges) => "scoped changes",
            (_, Self::UserVoice) => "user voice",
            (_, Self::ReversibleOps) => "reversible steps",
        }
    }

    fn note(self, locale: Locale) -> &'static str {
        match (locale, self) {
            (Locale::Ja, Self::ScopedChanges) => {
                "自由原則：小さくレビューしやすい変更を優先し、明示要求がない限り無関係なリファクタを避ける。"
            }
            (Locale::Ja, Self::UserVoice) => {
                "自由原則：ユーザーの語調、ブランド、制約を保ち、好みを権限拡大として扱わない。"
            }
            (Locale::Ja, Self::ReversibleOps) => {
                "自由原則：影響の大きい操作の前に、可逆手順、チェックポイント、ロールバック説明を選ぶ。"
            }
            (Locale::ZhHans, Self::ScopedChanges) => {
                "自定义准则：优先采用小范围、可审查的改动；除非明确要求，不做无关重构。"
            }
            (Locale::ZhHans, Self::UserVoice) => {
                "自定义准则：保留用户的语气、品牌和约束；不把偏好推断成权限扩大。"
            }
            (Locale::ZhHans, Self::ReversibleOps) => {
                "自定义准则：先选择可逆步骤、检查点和回滚说明，再进行高影响操作。"
            }
            (Locale::ZhHant, Self::ScopedChanges) => {
                "自由原則：優先採用小範圍、可審查的改動；除非明確要求，不做無關重構。"
            }
            (Locale::ZhHant, Self::UserVoice) => {
                "自由原則：保留使用者的語氣、品牌和約束；不把偏好推斷成權限擴大。"
            }
            (Locale::ZhHant, Self::ReversibleOps) => {
                "自由原則：先選擇可逆步驟、檢查點和回復說明，再進行高影響操作。"
            }
            (Locale::PtBr, Self::ScopedChanges) => {
                "Princípio livre: prefira mudanças pequenas e revisáveis; evite refactors não relacionados sem pedido explícito."
            }
            (Locale::PtBr, Self::UserVoice) => {
                "Princípio livre: preserve a voz, marca e restrições do usuário sem tratar preferências como expansão de permissão."
            }
            (Locale::PtBr, Self::ReversibleOps) => {
                "Princípio livre: favoreça passos reversíveis, checkpoints e notas de rollback antes de ações de alto impacto."
            }
            (Locale::Es419, Self::ScopedChanges) => {
                "Principio libre: prefiere cambios pequeños y revisables; evita refactors no relacionados sin pedido explícito."
            }
            (Locale::Es419, Self::UserVoice) => {
                "Principio libre: preserva la voz, marca y restricciones del usuario sin tratar preferencias como expansión de permisos."
            }
            (Locale::Es419, Self::ReversibleOps) => {
                "Principio libre: favorece pasos reversibles, checkpoints y notas de rollback antes de acciones de alto impacto."
            }
            (Locale::Vi, Self::ScopedChanges) => {
                "Nguyên tắc tự do: ưu tiên thay đổi nhỏ, dễ review; tránh refactor không liên quan nếu không được yêu cầu rõ."
            }
            (Locale::Vi, Self::UserVoice) => {
                "Nguyên tắc tự do: giữ giọng, thương hiệu và ràng buộc của người dùng, không xem sở thích là mở rộng quyền."
            }
            (Locale::Vi, Self::ReversibleOps) => {
                "Nguyên tắc tự do: ưu tiên bước có thể đảo ngược, checkpoint và ghi chú rollback trước thao tác tác động cao."
            }
            (Locale::Ko, Self::ScopedChanges) => {
                "자유 원칙: 작고 리뷰하기 쉬운 변경을 우선하고, 명시적으로 요청받지 않는 한 관련 없는 리팩터링은 하지 않는다."
            }
            (Locale::Ko, Self::UserVoice) => {
                "자유 원칙: 사용자의 어조, 브랜드, 제약을 유지하고 선호를 권한 확대로 취급하지 않는다."
            }
            (Locale::Ko, Self::ReversibleOps) => {
                "자유 원칙: 영향이 큰 작업 전에 되돌릴 수 있는 단계, 체크포인트, 롤백 메모를 우선한다."
            }
            (Locale::Ca, Self::ScopedChanges) => {
                "Principi lliure: prefereix canvis petits i revisables i evita refactors no relacionats si no es demanen explícitament."
            }
            (Locale::Ca, Self::UserVoice) => {
                "Principi lliure: preserva la veu, la marca i les restriccions de l'usuari sense tractar les preferències com una ampliació de permisos."
            }
            (Locale::Ca, Self::ReversibleOps) => {
                "Principi lliure: priorita passos reversibles, punts de control i notes de marxa enrere abans d'operacions d'alt impacte."
            }
            (Locale::De, Self::ScopedChanges) => {
                "Freitext-Prinzip: Bevorzuge kleine, überprüfbare Änderungen und vermeide unzusammenhängende Refactorings, sofern nicht ausdrücklich gewünscht."
            }
            (Locale::De, Self::UserVoice) => {
                "Freitext-Prinzip: Bewahre Stimme, Marke und Vorgaben des Nutzers, ohne Präferenzen als Rechteausweitung zu behandeln."
            }
            (Locale::De, Self::ReversibleOps) => {
                "Freitext-Prinzip: Bevorzuge reversible Schritte, Checkpoints und Rollback-Notizen vor einschneidenden Operationen."
            }
            (Locale::Fr, Self::ScopedChanges) => {
                "Principe libre : préférez des changements petits et révisables et évitez les refactors sans rapport, sauf demande explicite."
            }
            (Locale::Fr, Self::UserVoice) => {
                "Principe libre : préservez la voix, la marque et les contraintes de l'utilisateur sans traiter ses préférences comme une extension de permissions."
            }
            (Locale::Fr, Self::ReversibleOps) => {
                "Principe libre : privilégiez étapes réversibles, points de contrôle et notes de rollback avant les opérations à fort impact."
            }
            (Locale::Id, Self::ScopedChanges) => {
                "Prinsip bebas: utamakan perubahan kecil yang mudah ditinjau dan hindari refactor tak terkait kecuali diminta secara eksplisit."
            }
            (Locale::Id, Self::UserVoice) => {
                "Prinsip bebas: jaga suara, merek, dan batasan pengguna tanpa memperlakukan preferensi sebagai perluasan izin."
            }
            (Locale::Id, Self::ReversibleOps) => {
                "Prinsip bebas: utamakan langkah reversibel, checkpoint, dan catatan rollback sebelum operasi berdampak besar."
            }
            (Locale::Hi, Self::ScopedChanges) => {
                "मुक्त-पाठ सिद्धांत: छोटे, समीक्षायोग्य बदलावों को प्राथमिकता दें और स्पष्ट अनुरोध के बिना असंबंधित रिफैक्टर से बचें।"
            }
            (Locale::Hi, Self::UserVoice) => {
                "मुक्त-पाठ सिद्धांत: उपयोगकर्ता की आवाज़, ब्रांड और बाधाएँ सुरक्षित रखें; प्राथमिकताओं को अनुमति-विस्तार न मानें।"
            }
            (Locale::Hi, Self::ReversibleOps) => {
                "मुक्त-पाठ सिद्धांत: उच्च-प्रभाव कार्यों से पहले उत्क्रमणीय चरणों, चेकपॉइंट और रोलबैक नोट्स को प्राथमिकता दें।"
            }
            (Locale::Ru, Self::ScopedChanges) => {
                "Свободный принцип: предпочитайте небольшие, проверяемые изменения и избегайте несвязанных рефакторингов без явного запроса."
            }
            (Locale::Ru, Self::UserVoice) => {
                "Свободный принцип: сохраняйте голос, бренд и ограничения пользователя, не трактуя предпочтения как расширение полномочий."
            }
            (Locale::Ru, Self::ReversibleOps) => {
                "Свободный принцип: отдавайте предпочтение обратимым шагам, контрольным точкам и заметкам об откате перед высокорисковыми операциями."
            }
            (Locale::Uk, Self::ScopedChanges) => {
                "Вільний принцип: надавайте перевагу невеликим, перевірюваним змінам і уникайте непов'язаних рефакторингів без явного запиту."
            }
            (Locale::Uk, Self::UserVoice) => {
                "Вільний принцип: зберігайте голос, бренд і обмеження користувача, не трактуючи вподобання як розширення повноважень."
            }
            (Locale::Uk, Self::ReversibleOps) => {
                "Вільний принцип: надавайте перевагу оборотним крокам, контрольним точкам і нотаткам про відкат перед високоризиковими операціями."
            }
            (_, Self::ScopedChanges) => {
                "Freeform principle: prefer small, reviewable changes and avoid unrelated refactors unless explicitly requested."
            }
            (_, Self::UserVoice) => {
                "Freeform principle: preserve the user's voice, brand, and constraints without treating preferences as permission expansion."
            }
            (_, Self::ReversibleOps) => {
                "Freeform principle: favor reversible steps, checkpoints, and rollback notes before high-impact operations."
            }
        }
    }
}

fn next_guided_autonomy(preference: AutonomyPreference) -> AutonomyPreference {
    match preference {
        AutonomyPreference::Unspecified | AutonomyPreference::Cautious => {
            AutonomyPreference::Balanced
        }
        AutonomyPreference::Balanced => AutonomyPreference::Autonomous,
        AutonomyPreference::Autonomous => AutonomyPreference::Cautious,
    }
}

fn autonomy_label(preference: AutonomyPreference, locale: Locale) -> &'static str {
    match (locale, preference) {
        (Locale::Ja, AutonomyPreference::Cautious) => "慎重",
        (Locale::Ja, AutonomyPreference::Balanced) => "バランス",
        (Locale::Ja, AutonomyPreference::Autonomous) => "積極的",
        (Locale::ZhHans, AutonomyPreference::Cautious) => "谨慎",
        (Locale::ZhHans, AutonomyPreference::Balanced) => "平衡",
        (Locale::ZhHans, AutonomyPreference::Autonomous) => "积极主动",
        (Locale::ZhHant, AutonomyPreference::Cautious) => "謹慎",
        (Locale::ZhHant, AutonomyPreference::Balanced) => "平衡",
        (Locale::ZhHant, AutonomyPreference::Autonomous) => "積極主動",
        (Locale::PtBr, AutonomyPreference::Cautious) => "cauteloso",
        (Locale::PtBr, AutonomyPreference::Balanced) => "equilibrado",
        (Locale::PtBr, AutonomyPreference::Autonomous) => "ambicioso",
        (Locale::Es419, AutonomyPreference::Cautious) => "cauteloso",
        (Locale::Es419, AutonomyPreference::Balanced) => "equilibrado",
        (Locale::Es419, AutonomyPreference::Autonomous) => "ambicioso",
        (Locale::Vi, AutonomyPreference::Cautious) => "thận trọng",
        (Locale::Vi, AutonomyPreference::Balanced) => "cân bằng",
        (Locale::Vi, AutonomyPreference::Autonomous) => "chủ động",
        (Locale::Ko, AutonomyPreference::Cautious) => "신중함",
        (Locale::Ko, AutonomyPreference::Balanced) => "균형",
        (Locale::Ko, AutonomyPreference::Autonomous) => "적극적",
        (Locale::Ca, AutonomyPreference::Cautious) => "cautelós",
        (Locale::Ca, AutonomyPreference::Balanced) => "equilibrat",
        (Locale::Ca, AutonomyPreference::Autonomous) => "ambiciós",
        (Locale::De, AutonomyPreference::Cautious) => "vorsichtig",
        (Locale::De, AutonomyPreference::Balanced) => "ausgewogen",
        (Locale::De, AutonomyPreference::Autonomous) => "ambitioniert",
        (Locale::Fr, AutonomyPreference::Cautious) => "prudent",
        (Locale::Fr, AutonomyPreference::Balanced) => "équilibré",
        (Locale::Fr, AutonomyPreference::Autonomous) => "ambitieux",
        (Locale::Id, AutonomyPreference::Cautious) => "hati-hati",
        (Locale::Id, AutonomyPreference::Balanced) => "seimbang",
        (Locale::Id, AutonomyPreference::Autonomous) => "ambisius",
        (Locale::Hi, AutonomyPreference::Cautious) => "सावधान",
        (Locale::Hi, AutonomyPreference::Balanced) => "संतुलित",
        (Locale::Hi, AutonomyPreference::Autonomous) => "महत्वाकांक्षी",
        (Locale::Ru, AutonomyPreference::Cautious) => "осторожный",
        (Locale::Ru, AutonomyPreference::Balanced) => "сбалансированный",
        (Locale::Ru, AutonomyPreference::Autonomous) => "самостоятельный",
        (Locale::Uk, AutonomyPreference::Cautious) => "обережний",
        (Locale::Uk, AutonomyPreference::Balanced) => "збалансований",
        (Locale::Uk, AutonomyPreference::Autonomous) => "самостійний",
        (_, AutonomyPreference::Cautious) => "cautious",
        (_, AutonomyPreference::Balanced) => "balanced",
        (_, AutonomyPreference::Autonomous) => "ambitious",
        (_, AutonomyPreference::Unspecified) => "unspecified",
    }
}

fn autonomy_priority(preference: AutonomyPreference, locale: Locale) -> &'static str {
    match (locale, preference) {
        (Locale::Ja, AutonomyPreference::Cautious) => {
            "ファイル編集、コマンド実行、あいまいな製品判断の前に停止して尋ねる。"
        }
        (Locale::Ja, AutonomyPreference::Balanced) => {
            "明確で低リスクな作業は直接進め、危険、破壊的、あいまいな操作では先に確認する。"
        }
        (Locale::Ja, AutonomyPreference::Autonomous) => {
            "安全な定型作業はまとめて進めるが、破壊的、認証情報、公開、高コスト、法務、セキュリティリスクでは停止して尋ねる。"
        }
        (Locale::ZhHans, AutonomyPreference::Cautious) => {
            "在编辑文件、运行命令或产品选择不明确前，倾向先停下询问。"
        }
        (Locale::ZhHans, AutonomyPreference::Balanced) => {
            "清晰低风险任务可直接行动；遇到风险、破坏性或歧义时先确认。"
        }
        (Locale::ZhHans, AutonomyPreference::Autonomous) => {
            "可批量处理安全的常规工作，但遇到破坏性、凭据、发布、高成本、法律或安全风险时停止询问。"
        }
        (Locale::ZhHant, AutonomyPreference::Cautious) => {
            "在編輯檔案、執行命令或產品選擇不明確前，傾向先停下詢問。"
        }
        (Locale::ZhHant, AutonomyPreference::Balanced) => {
            "清晰低風險任務可直接行動；遇到風險、破壞性或歧義時先確認。"
        }
        (Locale::ZhHant, AutonomyPreference::Autonomous) => {
            "可批量處理安全的常規工作，但遇到破壞性、憑據、發布、高成本、法律或安全風險時停止詢問。"
        }
        (Locale::PtBr, AutonomyPreference::Cautious) => {
            "Pare e pergunte antes de editar arquivos, rodar comandos ou escolher entre caminhos ambíguos de produto."
        }
        (Locale::PtBr, AutonomyPreference::Balanced) => {
            "Aja diretamente em tarefas claras e de baixo risco; confirme antes de ações arriscadas, destrutivas ou ambíguas."
        }
        (Locale::PtBr, AutonomyPreference::Autonomous) => {
            "Agrupe trabalho seguro de rotina, mas pare para ações destrutivas, credenciais, publicação, alto custo, legais ou de segurança."
        }
        (Locale::Es419, AutonomyPreference::Cautious) => {
            "Detente y pregunta antes de editar archivos, ejecutar comandos o elegir entre caminos ambiguos de producto."
        }
        (Locale::Es419, AutonomyPreference::Balanced) => {
            "Actúa directamente en tareas claras y de bajo riesgo; confirma antes de acciones riesgosas, destructivas o ambiguas."
        }
        (Locale::Es419, AutonomyPreference::Autonomous) => {
            "Agrupa trabajo seguro de rutina, pero detente ante acciones destructivas, credenciales, publicación, alto costo, legales o de seguridad."
        }
        (Locale::Vi, AutonomyPreference::Cautious) => {
            "Dừng và hỏi trước khi sửa tệp, chạy lệnh hoặc chọn giữa đường sản phẩm mơ hồ."
        }
        (Locale::Vi, AutonomyPreference::Balanced) => {
            "Hành động trực tiếp với việc rõ, rủi ro thấp; xác nhận trước việc rủi ro, phá hủy hoặc mơ hồ."
        }
        (Locale::Vi, AutonomyPreference::Autonomous) => {
            "Gộp việc thường lệ an toàn, nhưng dừng với thao tác phá hủy, thông tin xác thực, xuất bản, chi phí cao, pháp lý hoặc bảo mật."
        }
        (Locale::Ko, AutonomyPreference::Cautious) => {
            "파일 수정, 명령어 실행, 애매한 제품 선택 전에 멈추고 물어본다."
        }
        (Locale::Ko, AutonomyPreference::Balanced) => {
            "명확하고 위험이 낮은 작업은 바로 진행하고, 위험하거나 파괴적이거나 애매한 작업은 먼저 확인한다."
        }
        (Locale::Ko, AutonomyPreference::Autonomous) => {
            "안전한 정형 작업은 모아서 진행하되, 파괴적이거나 자격 증명, 게시, 고비용, 법적, 보안 위험이 있는 작업에서는 멈추고 물어본다."
        }
        (Locale::Ca, AutonomyPreference::Cautious) => {
            "Atura't i pregunta abans d'editar fitxers, executar ordres o triar entre camins de producte ambigus."
        }
        (Locale::Ca, AutonomyPreference::Balanced) => {
            "Actua directament en tasques clares i de baix risc; confirma abans d'accions arriscades, destructives o ambigües."
        }
        (Locale::Ca, AutonomyPreference::Autonomous) => {
            "Agrupa la feina rutinària segura, però atura't davant accions destructives, amb credencials, de publicació, d'alt cost o amb risc legal o de seguretat."
        }
        (Locale::De, AutonomyPreference::Cautious) => {
            "Halte an und frage, bevor du Dateien bearbeitest, Befehle ausführst oder zwischen mehrdeutigen Produktwegen wählst."
        }
        (Locale::De, AutonomyPreference::Balanced) => {
            "Handle direkt bei klaren, risikoarmen Aufgaben; bestätige vor riskanten, destruktiven oder mehrdeutigen Aktionen."
        }
        (Locale::De, AutonomyPreference::Autonomous) => {
            "Bündle sichere Routinearbeit, aber halte an bei destruktiven, zugangsdatenbezogenen, veröffentlichenden, kostspieligen, rechtlichen oder sicherheitskritischen Aktionen."
        }
        (Locale::Fr, AutonomyPreference::Cautious) => {
            "Arrêtez et demandez avant de modifier des fichiers, d'exécuter des commandes ou de choisir entre des voies produit ambiguës."
        }
        (Locale::Fr, AutonomyPreference::Balanced) => {
            "Agissez directement sur les tâches claires et à faible risque ; confirmez avant les actions risquées, destructives ou ambiguës."
        }
        (Locale::Fr, AutonomyPreference::Autonomous) => {
            "Regroupez le travail de routine sûr, mais arrêtez devant les actions destructives, impliquant des identifiants, des publications, coûteuses, juridiques ou à risque de sécurité."
        }
        (Locale::Id, AutonomyPreference::Cautious) => {
            "Berhenti dan tanya sebelum mengedit file, menjalankan perintah, atau memilih di antara jalur produk yang ambigu."
        }
        (Locale::Id, AutonomyPreference::Balanced) => {
            "Bertindak langsung pada tugas yang jelas dan berisiko rendah; konfirmasi sebelum tindakan berisiko, destruktif, atau ambigu."
        }
        (Locale::Id, AutonomyPreference::Autonomous) => {
            "Kelompokkan pekerjaan rutin yang aman, tetapi berhenti untuk tindakan destruktif, terkait kredensial, publikasi, mahal, hukum, atau berisiko keamanan."
        }
        (Locale::Hi, AutonomyPreference::Cautious) => {
            "फ़ाइलें संपादित करने, कमांड चलाने या अस्पष्ट उत्पाद मार्गों में चुनने से पहले रुककर पूछें।"
        }
        (Locale::Hi, AutonomyPreference::Balanced) => {
            "स्पष्ट, कम-जोखिम वाले कार्यों पर सीधे कार्य करें; जोखिमपूर्ण, विनाशकारी या अस्पष्ट कार्यों से पहले पुष्टि करें।"
        }
        (Locale::Hi, AutonomyPreference::Autonomous) => {
            "सुरक्षित नियमित काम एक साथ करें, लेकिन विनाशकारी, क्रेडेंशियल, प्रकाशन, उच्च-लागत, कानूनी या सुरक्षा-जोखिम कार्यों पर रुककर पूछें।"
        }
        (Locale::Ru, AutonomyPreference::Cautious) => {
            "Остановитесь и спросите перед редактированием файлов, запуском команд или выбором между неоднозначными продуктовыми путями."
        }
        (Locale::Ru, AutonomyPreference::Balanced) => {
            "Действуйте напрямую в ясных низкорисковых задачах; подтверждайте перед рискованными, деструктивными или неоднозначными действиями."
        }
        (Locale::Ru, AutonomyPreference::Autonomous) => {
            "Группируйте безопасную рутинную работу, но останавливайтесь перед деструктивными действиями, действиями с учётными данными, публикациями, дорогими, юридическими или угрожающими безопасности операциями."
        }
        (Locale::Uk, AutonomyPreference::Cautious) => {
            "Зупиніться й запитайте перед редагуванням файлів, запуском команд або вибором між неоднозначними продуктовими шляхами."
        }
        (Locale::Uk, AutonomyPreference::Balanced) => {
            "Дійте безпосередньо в чітких низькоризикових завданнях; підтверджуйте перед ризикованими, руйнівними чи неоднозначними діями."
        }
        (Locale::Uk, AutonomyPreference::Autonomous) => {
            "Групуйте безпечну рутинну роботу, але зупиняйтеся перед руйнівними діями, діями з обліковими даними, публікаціями, дорогими, юридичними чи небезпечними для безпеки операціями."
        }
        (_, AutonomyPreference::Cautious) => {
            "Stop and ask before editing files, running commands, or choosing between ambiguous product paths."
        }
        (_, AutonomyPreference::Balanced) => {
            "Do clear, low-risk work; ask before risky, destructive, or unclear work."
        }
        (_, AutonomyPreference::Autonomous) => {
            "Batch routine safe work, then stop for destructive, credential, publishing, high-cost, legal, or security-risk actions."
        }
        (_, AutonomyPreference::Unspecified) => "No standing initiative preference was selected.",
    }
}

fn authority_priority(locale: Locale) -> &'static str {
    match locale {
        Locale::Ja => {
            "現在のユーザー要求とライブツール証拠は、メモリ、古い引き継ぎ、推測より優先される。"
        }
        Locale::ZhHans => "当前用户请求和实时工具证据优先于记忆、陈旧交接和猜测。",
        Locale::ZhHant => "目前使用者請求和即時工具證據優先於記憶、陳舊交接和猜測。",
        Locale::PtBr => {
            "Pedidos atuais do usuário e evidência viva das ferramentas superam memória, handoffs antigos e palpites."
        }
        Locale::Es419 => {
            "Las solicitudes actuales del usuario y la evidencia viva de herramientas superan memoria, handoffs viejos y suposiciones."
        }
        Locale::Vi => {
            "Yêu cầu hiện tại của người dùng và bằng chứng trực tiếp từ công cụ ưu tiên hơn bộ nhớ, handoff cũ và phỏng đoán."
        }
        Locale::Ko => {
            "현재 사용자 요청과 실시간 도구 근거는 메모리, 오래된 인계 자료, 추측보다 우선한다."
        }
        Locale::Ca => {
            "Les peticions actuals de l'usuari i l'evidència en directe de les eines prevalen sobre la memòria, els traspasos antics i les conjectures."
        }
        Locale::De => {
            "Aktuelle Nutzeranfragen und Live-Werkzeugnachweise haben Vorrang vor Speicher, veralteten Übergaben und Vermutungen."
        }
        Locale::Fr => {
            "Les demandes actuelles de l'utilisateur et les preuves directes des outils priment sur la mémoire, les anciens transferts et les suppositions."
        }
        Locale::Id => {
            "Permintaan pengguna saat ini dan bukti langsung dari alat mengalahkan memori, handoff lama, dan tebakan."
        }
        Locale::Hi => "वर्तमान उपयोगकर्ता अनुरोध और लाइव टूल साक्ष्य मेमोरी, पुराने हैंडऑफ़ और अनुमानों से ऊपर हैं।",
        Locale::Ru => {
            "Текущие запросы пользователя и живые свидетельства инструментов важнее памяти, устаревших передаточных заметок и догадок."
        }
        Locale::Uk => {
            "Поточні запити користувача та живі свідчення інструментів важливіші за пам'ять, застарілі передаточні нотатки й здогадки."
        }
        _ => {
            "Current user requests and live tool evidence outrank memory, stale handoffs, and guesses."
        }
    }
}

const FREEFORM_NOTE_MAX: usize = MAX_NOTES_LEN - 1_000;

fn bounded_freeform_note(input: &str, max_chars: usize) -> String {
    input
        .chars()
        .filter_map(|ch| {
            if ch == '\t' {
                Some(' ')
            } else if ch == '\n' || !ch.is_control() {
                Some(ch)
            } else {
                None
            }
        })
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn compact_freeform_preview(note: &str) -> String {
    let compact = note.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = compact.chars().take(96).collect::<String>();
    if compact.chars().count() > 96 {
        preview.push_str("...");
    }
    preview
}

fn freeform_note_line(locale: Locale, note: &str, editing: bool) -> Line<'static> {
    let preview = compact_freeform_preview(note);
    let text = match (locale, editing, preview.is_empty()) {
        (Locale::Ja, true, true) => {
            "F 自由原則：編集中 - 有界の原則を入力または貼り付け、Enter で完了".to_string()
        }
        (Locale::Ja, true, false) => format!("F 自由原則：編集中 - {preview}"),
        (Locale::Ja, false, true) => "F 自由原則：F で有界の原則を入力または貼り付け".to_string(),
        (Locale::Ja, false, false) => format!("F 自由原則：{preview}"),
        (Locale::ZhHans, true, true) => {
            "F 自定义准则：正在编辑 - 输入或粘贴明确的准则，Enter 完成".to_string()
        }
        (Locale::ZhHans, true, false) => format!("F 自定义准则：正在编辑 - {preview}"),
        (Locale::ZhHans, false, true) => {
            "F 自定义准则：按 F 输入或粘贴自己的明确准则".to_string()
        }
        (Locale::ZhHans, false, false) => format!("F 自定义准则：{preview}"),
        (Locale::ZhHant, true, true) => {
            "F 自由原則：正在編輯 - 輸入或貼上有界原則，Enter 完成".to_string()
        }
        (Locale::ZhHant, true, false) => format!("F 自由原則：正在編輯 - {preview}"),
        (Locale::ZhHant, false, true) => "F 自由原則：按 F 輸入或貼上自己的有界原則".to_string(),
        (Locale::ZhHant, false, false) => format!("F 自由原則：{preview}"),
        (Locale::PtBr, true, true) => {
            "F Princípio livre: editando - digite ou cole um princípio limitado, Enter para concluir".to_string()
        }
        (Locale::PtBr, true, false) => format!("F Princípio livre: editando - {preview}"),
        (Locale::PtBr, false, true) => {
            "F Princípio livre: pressione F para digitar ou colar um princípio limitado".to_string()
        }
        (Locale::PtBr, false, false) => format!("F Princípio livre: {preview}"),
        (Locale::Es419, true, true) => {
            "F Principio libre: editando - escribe o pega un principio acotado, Enter para terminar".to_string()
        }
        (Locale::Es419, true, false) => format!("F Principio libre: editando - {preview}"),
        (Locale::Es419, false, true) => {
            "F Principio libre: presiona F para escribir o pegar un principio acotado".to_string()
        }
        (Locale::Es419, false, false) => format!("F Principio libre: {preview}"),
        (Locale::Vi, true, true) => {
            "F Nguyên tắc tự do: đang sửa - nhập hoặc dán nguyên tắc có giới hạn, Enter để xong".to_string()
        }
        (Locale::Vi, true, false) => format!("F Nguyên tắc tự do: đang sửa - {preview}"),
        (Locale::Vi, false, true) => {
            "F Nguyên tắc tự do: nhấn F để nhập hoặc dán nguyên tắc có giới hạn".to_string()
        }
        (Locale::Vi, false, false) => format!("F Nguyên tắc tự do: {preview}"),
        (Locale::Ko, true, true) => {
            "F 자유 원칙: 편집 중 - 제한된 원칙을 입력하거나 붙여넣고 Enter로 완료".to_string()
        }
        (Locale::Ko, true, false) => format!("F 자유 원칙: 편집 중 - {preview}"),
        (Locale::Ko, false, true) => "F 자유 원칙: F를 눌러 제한된 원칙을 입력하거나 붙여넣기".to_string(),
        (Locale::Ko, false, false) => format!("F 자유 원칙: {preview}"),
        (Locale::Ca, true, true) => {
            "F Paraules pròpies: editant - escriu o enganxa un principi acotat, Enter per acabar".to_string()
        }
        (Locale::Ca, true, false) => format!("F Paraules pròpies: editant - {preview}"),
        (Locale::Ca, false, true) => {
            "F Paraules pròpies: prem F per escriure o enganxar un principi acotat".to_string()
        }
        (Locale::Ca, false, false) => format!("F Paraules pròpies: {preview}"),
        (Locale::De, true, true) => {
            "F Eigene Worte: Bearbeitung - tippe oder füge ein begrenztes Prinzip ein, Enter zum Abschluss".to_string()
        }
        (Locale::De, true, false) => format!("F Eigene Worte: Bearbeitung - {preview}"),
        (Locale::De, false, true) => {
            "F Eigene Worte: F drücken, um ein begrenztes Prinzip zu tippen oder einzufügen".to_string()
        }
        (Locale::De, false, false) => format!("F Eigene Worte: {preview}"),
        (Locale::Fr, true, true) => {
            "F Vos mots : édition - tapez ou collez un principe borné, Entrée pour terminer".to_string()
        }
        (Locale::Fr, true, false) => format!("F Vos mots : édition - {preview}"),
        (Locale::Fr, false, true) => {
            "F Vos mots : appuyez sur F pour taper ou coller un principe borné".to_string()
        }
        (Locale::Fr, false, false) => format!("F Vos mots : {preview}"),
        (Locale::Id, true, true) => {
            "F Kata sendiri: mengedit - ketik atau tempel prinsip terbatas, Enter untuk selesai".to_string()
        }
        (Locale::Id, true, false) => format!("F Kata sendiri: mengedit - {preview}"),
        (Locale::Id, false, true) => {
            "F Kata sendiri: tekan F untuk mengetik atau menempel prinsip terbatas".to_string()
        }
        (Locale::Id, false, false) => format!("F Kata sendiri: {preview}"),
        (Locale::Hi, true, true) => {
            "F अपने शब्द: संपादन जारी - सीमित सिद्धांत टाइप या पेस्ट करें, Enter से समाप्त करें".to_string()
        }
        (Locale::Hi, true, false) => format!("F अपने शब्द: संपादन जारी - {preview}"),
        (Locale::Hi, false, true) => {
            "F अपने शब्द: सीमित सिद्धांत टाइप या पेस्ट करने के लिए F दबाएँ".to_string()
        }
        (Locale::Hi, false, false) => format!("F अपने शब्द: {preview}"),
        (Locale::Ru, true, true) => {
            "F Свои слова: редактирование - введите или вставьте ограниченный принцип, Enter для завершения".to_string()
        }
        (Locale::Ru, true, false) => format!("F Свои слова: редактирование - {preview}"),
        (Locale::Ru, false, true) => {
            "F Свои слова: нажмите F, чтобы ввести или вставить ограниченный принцип".to_string()
        }
        (Locale::Ru, false, false) => format!("F Свои слова: {preview}"),
        (Locale::Uk, true, true) => {
            "F Свої слова: редагування - введіть або вставте обмежений принцип, Enter для завершення".to_string()
        }
        (Locale::Uk, true, false) => format!("F Свої слова: редагування - {preview}"),
        (Locale::Uk, false, true) => {
            "F Свої слова: натисніть F, щоб ввести або вставити обмежений принцип".to_string()
        }
        (Locale::Uk, false, false) => format!("F Свої слова: {preview}"),
        (_, true, true) => {
            "F Own words: editing - type or paste a bounded principle, Enter to finish".to_string()
        }
        (_, true, false) => format!("F Own words: editing - {preview}"),
        (_, false, true) => "F Own words: press F to type or paste a bounded principle".to_string(),
        (_, false, false) => format!("F Own words: {preview}"),
    };
    let style = if editing || !preview.is_empty() {
        Style::default().fg(palette::WHALE_HUMAN)
    } else {
        Style::default().fg(palette::TEXT_MUTED)
    };
    Line::from(Span::styled(text, style))
}

impl SetupWizardView {
    #[must_use]
    pub fn new_for_app(app: &App, config: &Config) -> Self {
        Self::new_with_facts(
            load_setup_state_for_app(app, config),
            app.ui_locale,
            SetupRuntimeFacts::from_app_config(app, config),
        )
    }

    #[must_use]
    pub fn new_checkpoint_for_app(app: &App, config: &Config) -> Self {
        Self::new_checkpoint_with_facts(
            load_setup_state_for_app(app, config),
            app.ui_locale,
            SetupRuntimeFacts::from_app_config(app, config),
        )
    }

    #[must_use]
    pub fn new_for_app_at(app: &App, config: &Config, step: SetupStep) -> Self {
        Self::new_at_with_facts(
            load_setup_state_for_app(app, config),
            app.ui_locale,
            step,
            SetupRuntimeFacts::from_app_config(app, config),
        )
    }

    #[must_use]
    pub fn selected_step(&self) -> SetupStep {
        STEP_SPECS[self.selected].id()
    }

    fn selected_spec(&self) -> &'static dyn SetupWizardStep {
        &STEP_SPECS[self.selected]
    }

    fn new_with_facts(state: SetupState, locale: Locale, facts: SetupRuntimeFacts) -> Self {
        let selected = progressive_initial_step_index(&state, &facts);
        Self {
            state,
            selected,
            locale,
            progressive_guide: true,
            details_expanded: false,
            facts,
            guided_draft: GuidedConstitutionDraft::default(),
            constitution_advanced: false,
            freeform_note: String::new(),
            editing_freeform_note: false,
            guided_preview_seen: false,
            existing_preview_seen: false,
            model_draft: None,
            model_draft_label: None,
            model_draft_request: None,
            runtime_preset: SetupRuntimePreset::default(),
            runtime_preset_preview_seen: false,
            body_scroll: 0,
        }
    }

    fn new_at_with_facts(
        state: SetupState,
        locale: Locale,
        step: SetupStep,
        facts: SetupRuntimeFacts,
    ) -> Self {
        Self {
            state,
            selected: visible_step_index(step),
            locale,
            progressive_guide: false,
            details_expanded: false,
            facts,
            guided_draft: GuidedConstitutionDraft::default(),
            constitution_advanced: false,
            freeform_note: String::new(),
            editing_freeform_note: false,
            guided_preview_seen: false,
            existing_preview_seen: false,
            model_draft: None,
            model_draft_label: None,
            model_draft_request: None,
            runtime_preset: SetupRuntimePreset::default(),
            runtime_preset_preview_seen: false,
            body_scroll: 0,
        }
    }

    fn new_checkpoint_with_facts(
        state: SetupState,
        locale: Locale,
        facts: SetupRuntimeFacts,
    ) -> Self {
        Self::new_at_with_facts(state, locale, SetupStep::Constitution, facts)
    }

    fn surface_title(&self) -> String {
        tr(self.locale, MessageId::SetupWizardTitle).into_owned()
    }

    fn tools_relevant(&self) -> bool {
        self.facts.tools_mcp_needs_action || !self.facts.tools_mcp_result.contains("overall=off")
    }

    fn progressive_steps(&self) -> Vec<SetupStep> {
        let mut steps = vec![
            SetupStep::ProviderModel,
            SetupStep::TrustSandbox,
            SetupStep::RemoteRuntime,
        ];
        if self.tools_relevant() {
            steps.push(SetupStep::ToolsMcp);
        }
        steps.push(SetupStep::Verification);
        steps
    }

    fn move_next(&mut self) {
        if self.progressive_guide {
            let steps = self.progressive_steps();
            let position = steps
                .iter()
                .position(|step| *step == self.selected_step())
                .unwrap_or(0);
            let next = steps[(position + 1).min(steps.len().saturating_sub(1))];
            self.selected = visible_step_index(next);
        } else {
            self.selected = (self.selected + 1).min(STEP_SPECS.len().saturating_sub(1));
        }
        self.constitution_advanced = false;
        self.details_expanded = false;
        self.body_scroll = 0;
    }

    fn move_back(&mut self) {
        if self.progressive_guide {
            let steps = self.progressive_steps();
            let position = steps
                .iter()
                .position(|step| *step == self.selected_step())
                .unwrap_or(0);
            let previous = steps[position.saturating_sub(1)];
            self.selected = visible_step_index(previous);
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
        self.constitution_advanced = false;
        self.details_expanded = false;
        self.body_scroll = 0;
    }

    fn commit_selected_status(
        &mut self,
        status: StepStatus,
        message_id: MessageId,
        advance: bool,
    ) -> ViewAction {
        let spec = self.selected_spec();
        let result = match status {
            StepStatus::Skipped => Some("skipped by user"),
            StepStatus::NeedsAction => Some("retry requested; needs action"),
            _ => None,
        };
        let mut entry = StepEntry::new(status, spec.required(), CONSTITUTION_CHECKPOINT_VERSION);
        if let Some(result) = result {
            entry = entry.with_result(result);
        }
        let mut state = self.state.clone();
        state.set_step(spec.id(), entry);
        if spec.id() == SetupStep::Constitution && status == StepStatus::Skipped {
            // `S` is a durable response to the versioned checkpoint, just like
            // choosing the explicit defer action. It only skips this setup
            // checkpoint, though; it must not replace an already active
            // bundled or custom Constitution choice. A fresh state has no
            // active choice, so keep the bundled floor by recording Deferred.
            let choice = if state.constitution_choice.is_explicit() {
                state.constitution_choice
            } else {
                ConstitutionChoice::Deferred
            };
            state.complete_constitution_checkpoint(CONSTITUTION_CHECKPOINT_VERSION, choice);
        }
        self.state = state.clone();
        if advance {
            self.move_next();
        }
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, message_id).to_string(),
        })
    }

    fn commit_language_review(&mut self) -> ViewAction {
        let mut state = self.state.clone();
        state.constitution_language = Some(self.locale.tag().to_string());
        state.set_step(
            SetupStep::Language,
            StepEntry::new(StepStatus::Verified, true, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(format!("setup locale {}", self.locale.tag())),
        );
        self.state = state.clone();
        self.move_next();
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, MessageId::SetupLanguageReviewed).to_string(),
        })
    }

    fn commit_provider_model_review(&mut self) -> ViewAction {
        let status = self.facts.provider_status;
        let mut state = self.state.clone();
        state.set_step(
            SetupStep::ProviderModel,
            provider::step_entry(
                status,
                CONSTITUTION_CHECKPOINT_VERSION,
                self.facts.provider_result.clone(),
            ),
        );
        self.state = state.clone();
        self.move_next();
        let message_id = if self.facts.provider_ready {
            MessageId::SetupProviderModelReviewed
        } else {
            MessageId::SetupProviderModelNeedsActionSaved
        };
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, message_id).to_string(),
        })
    }

    fn commit_runtime_posture_review(&mut self) -> ViewAction {
        let mut state = self.state.clone();
        state.runtime_posture_source = RuntimePostureSource::Confirmed;
        state.set_step(
            SetupStep::TrustSandbox,
            StepEntry::new(StepStatus::Verified, true, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(self.facts.runtime_result.clone()),
        );
        self.state = state.clone();
        self.move_next();
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, MessageId::SetupRuntimePostureReviewed).to_string(),
        })
    }

    fn operate_fleet_facts_ready(&self) -> bool {
        // Provider, capacity, and roster facts are configuration snapshots,
        // not proof of dispatch and terminal receipts. This release must never
        // persist an Operate-ready claim from those facts alone.
        false
    }

    fn commit_operate_fleet_review(&mut self) -> ViewAction {
        let status = if self.operate_fleet_facts_ready() {
            StepStatus::Verified
        } else {
            StepStatus::NeedsAction
        };
        let mut state = self.state.clone();
        state.set_step(
            SetupStep::OperateFleet,
            StepEntry::new(status, false, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(self.facts.operate_result.clone()),
        );
        self.state = state.clone();
        self.move_next();
        let message_id = if status == StepStatus::Verified {
            MessageId::SetupOperateReviewed
        } else {
            MessageId::SetupOperateNeedsActionSaved
        };
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, message_id).to_string(),
        })
    }

    fn commit_hotbar_review(&mut self) -> ViewAction {
        let mut state = self.state.clone();
        state.set_step(
            SetupStep::Hotbar,
            StepEntry::new(StepStatus::Verified, false, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(self.facts.hotbar_result.clone()),
        );
        self.state = state.clone();
        self.move_next();
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, MessageId::SetupHotbarReviewed).to_string(),
        })
    }

    fn commit_tools_mcp_review(&mut self) -> ViewAction {
        // Optional step: empty/off inventories settle as Optional; broken
        // configured tools record NeedsAction without blocking first-run.
        let status = if self.facts.tools_mcp_needs_action {
            StepStatus::NeedsAction
        } else if self.facts.tools_mcp_result.contains("overall=off") {
            StepStatus::Optional
        } else {
            StepStatus::Verified
        };
        let mut state = self.state.clone();
        state.set_step(
            SetupStep::ToolsMcp,
            StepEntry::new(status, false, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(self.facts.tools_mcp_result.clone()),
        );
        self.state = state.clone();
        self.move_next();
        let message_id = if status == StepStatus::NeedsAction {
            MessageId::SetupToolsMcpNeedsActionSaved
        } else {
            MessageId::SetupToolsMcpReviewed
        };
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, message_id).to_string(),
        })
    }

    fn preview_tools_mcp_on_ramp(&self) -> ViewAction {
        ViewAction::Emit(ViewEvent::OpenTextPager {
            title: tr(self.locale, MessageId::SetupToolsMcpPreviewTitle).to_string(),
            content: tools_mcp_on_ramp_text(self.locale, &self.facts),
        })
    }

    fn preview_remote_runtime_on_ramp(&self) -> ViewAction {
        ViewAction::Emit(ViewEvent::OpenTextPager {
            title: tr(self.locale, MessageId::SetupRemotePreviewTitle).to_string(),
            content: remote_runtime_on_ramp_text(self.locale, &self.facts),
        })
    }

    /// Record the remote step honestly (#3409).
    ///
    /// Local-only always works, so Enter alone settles the step — a user who
    /// never wants remote access is finished in one key. When a *reachable*
    /// mode is missing a token or config the entry is `NeedsAction`, which the
    /// setup report and doctor inherit verbatim and which never blocks ready.
    fn commit_remote_runtime_review(&mut self) -> ViewAction {
        let mut state = self.state.clone();
        let status = if self.facts.remote_needs_action {
            StepStatus::NeedsAction
        } else {
            StepStatus::Verified
        };
        state.set_step(
            SetupStep::RemoteRuntime,
            StepEntry::new(status, false, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(self.facts.remote_result.clone()),
        );
        self.state = state.clone();
        self.move_next();
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, MessageId::SetupRemoteReviewed).to_string(),
        })
    }

    fn commit_persistence_review(&mut self) -> ViewAction {
        let mut state = self.state.clone();
        state.set_step(
            SetupStep::Persistence,
            StepEntry::new(StepStatus::Verified, false, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(self.facts.persistence.result.clone()),
        );
        self.state = state.clone();
        self.move_next();
        ViewAction::Emit(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, MessageId::SetupPersistenceReviewed).to_string(),
        })
    }

    fn select_runtime_preset(&mut self, key: char) -> ViewAction {
        if let Some(preset) = SetupRuntimePreset::from_key(key)
            && preset != self.runtime_preset
        {
            self.runtime_preset = preset;
            self.runtime_preset_preview_seen = false;
        }
        ViewAction::None
    }

    fn preview_runtime_preset(&mut self) -> ViewAction {
        self.runtime_preset_preview_seen = true;
        ViewAction::Emit(ViewEvent::OpenTextPager {
            title: tr(self.locale, MessageId::SetupRuntimePresetPreviewTitle).to_string(),
            content: runtime_preset_preview_text(self.locale, self.runtime_preset, &self.facts),
        })
    }

    fn commit_runtime_preset(&mut self) -> ViewAction {
        if !self.runtime_preset_preview_seen {
            return self.preview_runtime_preset();
        }

        let mut state = self.state.clone();
        state.runtime_posture_source = RuntimePostureSource::Confirmed;
        state.set_step(
            SetupStep::TrustSandbox,
            StepEntry::new(StepStatus::Verified, true, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(self.runtime_preset.result_summary()),
        );
        self.state = state.clone();
        self.move_next();
        ViewAction::Emit(ViewEvent::SetupRuntimePresetApplyRequested {
            preset: self.runtime_preset,
            state,
            message: tr(self.locale, MessageId::SetupRuntimePresetApplied).to_string(),
        })
    }

    fn commit_setup_report(&mut self) -> ViewAction {
        let mut state = self.state.clone();
        let status = if setup_report_ready(&state) {
            StepStatus::Verified
        } else {
            StepStatus::NeedsAction
        };
        state.set_step(
            SetupStep::Verification,
            StepEntry::new(status, false, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(setup_report_result(&state, &self.facts)),
        );
        self.state = state.clone();
        let event = ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, MessageId::SetupReportRecorded).to_string(),
        };
        if self.progressive_guide {
            ViewAction::EmitAndClose(event)
        } else {
            ViewAction::Emit(event)
        }
    }

    fn open_constitution_advanced(&mut self) -> ViewAction {
        self.constitution_advanced = true;
        self.body_scroll = 0;
        ViewAction::None
    }

    fn close_constitution_advanced(&mut self) -> ViewAction {
        self.constitution_advanced = false;
        self.editing_freeform_note = false;
        self.body_scroll = 0;
        ViewAction::None
    }

    /// The first-run path is intentionally one decision: how much initiative
    /// Codewhale should take. This saves guidance only. Runtime approval,
    /// sandbox, shell, network, trust, and MCP policy remain untouched.
    fn commit_simple_constitution(&mut self) -> ViewAction {
        match self.facts.constitution_file {
            // Existing law is the safest default on an update checkpoint.
            // Keep it byte-for-byte and only advance setup state.
            SetupConstitutionFileState::Loaded => {
                return self.commit_existing_constitution_unchanged();
            }
            // Do not overwrite a file the user attempted to provide when it
            // cannot be parsed or read. The bundled floor remains active and
            // Advanced exposes the explicit repair/regenerate choices.
            SetupConstitutionFileState::Empty
            | SetupConstitutionFileState::Invalid
            | SetupConstitutionFileState::Unreadable
            | SetupConstitutionFileState::PathError => {
                return self.commit_constitution(SetupCommitKind::BundledConstitution);
            }
            SetupConstitutionFileState::NotChecked | SetupConstitutionFileState::Missing => {}
        }

        // The compiled constitution already embodies the balanced posture:
        // act on clear reversible work, ask when ambiguity is costly, and
        // require express authorization for irreversible or external effects.
        // Accepting the recommendation therefore records Bundled rather than
        // pinning a generated user-global fork that would miss future bundled
        // law improvements. Only Customize writes a GuidedCustom file.
        self.commit_constitution(SetupCommitKind::BundledConstitution)
    }

    fn commit_custom_constitution(
        &mut self,
        constitution: UserConstitution,
        authoring: ConstitutionAuthoring,
        result_prefix: &str,
    ) -> ViewAction {
        let mut state = self.state.clone();
        state.complete_constitution_checkpoint(
            CONSTITUTION_CHECKPOINT_VERSION,
            ConstitutionChoice::GuidedCustom,
        );
        state.constitution_language = constitution.language.clone();
        state.constitution_source = ConstitutionSource::UserGlobal;
        state.constitution_validity = ConstitutionValidity::Valid;
        state.constitution_authoring = Some(authoring);
        state.constitution_preview_hash = Some(constitution.preview_hash());
        state.constitution_preview_version =
            state.constitution_preview_version.saturating_add(1).max(1);
        let hash = state
            .constitution_preview_hash
            .as_deref()
            .unwrap_or("unknown");
        state.set_step(
            SetupStep::Constitution,
            StepEntry::new(StepStatus::Verified, true, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(format!("{result_prefix} preview_hash={hash}")),
        );
        self.state = state.clone();
        ViewAction::EmitAndClose(ViewEvent::SetupConstitutionCommitRequested {
            constitution,
            state,
            message: tr(self.locale, MessageId::SetupCheckpointDoneGuided).to_string(),
        })
    }

    fn commit_guided_constitution(&mut self) -> ViewAction {
        if !self.guided_preview_seen {
            return self.preview_guided_constitution();
        }

        let (constitution, authoring) = match self.model_draft.as_deref() {
            // Model drafts arrive sanitized + bounded from the untrusted-JSON
            // gate; ratify exactly what was previewed.
            Some(draft) => (draft.clone(), ConstitutionAuthoring::ModelDrafted),
            None => (
                self.guided_draft
                    .to_constitution_with_freeform(self.locale, self.freeform_note_for_draft()),
                ConstitutionAuthoring::Guided,
            ),
        };
        let result_prefix = match authoring {
            ConstitutionAuthoring::ModelDrafted => format!(
                "model-drafted constitution ratified ({})",
                self.model_draft_label.as_deref().unwrap_or("model")
            ),
            ConstitutionAuthoring::Guided => "guided custom constitution".to_string(),
        };
        self.commit_custom_constitution(constitution, authoring, &result_prefix)
    }

    fn preview_guided_constitution(&mut self) -> ViewAction {
        self.guided_preview_seen = true;
        let (constitution, provenance) = match self.model_draft.as_deref() {
            Some(draft) => (
                draft.clone(),
                DraftProvenance::Model(
                    self.model_draft_label
                        .clone()
                        .unwrap_or_else(|| "model".to_string()),
                ),
            ),
            None => (
                self.guided_draft
                    .to_constitution_with_freeform(self.locale, self.freeform_note_for_draft()),
                DraftProvenance::Guided,
            ),
        };
        ViewAction::Emit(ViewEvent::OpenTextPager {
            title: ratification_preview_title(self.locale).to_string(),
            content: constitution_ratification_text(self.locale, &constitution, &provenance),
        })
    }

    fn cycle_guided_answer(&mut self, key: char) -> ViewAction {
        if self.guided_draft.cycle(key) {
            self.guided_preview_seen = false;
            // Answers changed under the draft: the model draft is stale law
            // and must be re-drafted or replaced by the guided rendering.
            self.model_draft = None;
            self.model_draft_label = None;
        }
        ViewAction::None
    }

    /// `A` on the constitution step: ask the first configured model to draft.
    /// Requires a ready provider route; otherwise the key is inert and the
    /// deterministic guided flow stands untouched.
    fn request_model_draft(&mut self) -> ViewAction {
        if !self.facts.provider_ready {
            return ViewAction::None;
        }
        self.model_draft_request = Some(self.current_model_draft_request());
        ViewAction::Emit(ViewEvent::SetupConstitutionModelDraftRequested {
            draft: self.guided_draft,
            freeform_note: self.freeform_note_for_draft().map(str::to_string),
            locale: self.locale,
        })
    }

    fn toggle_freeform_edit(&mut self) -> ViewAction {
        if self.selected_step() == SetupStep::Constitution {
            self.editing_freeform_note = !self.editing_freeform_note;
        }
        ViewAction::None
    }

    fn current_model_draft_request(&self) -> (GuidedConstitutionDraft, Option<String>) {
        (
            self.guided_draft,
            self.freeform_note_for_draft().map(str::to_string),
        )
    }

    fn freeform_note_for_draft(&self) -> Option<&str> {
        let note = self.freeform_note.trim();
        (!note.is_empty()).then_some(note)
    }

    fn append_freeform_note_text(&mut self, text: &str) {
        let mut next = self.freeform_note.clone();
        next.push_str(text);
        self.freeform_note = bounded_freeform_note(&next, FREEFORM_NOTE_MAX);
        self.guided_preview_seen = false;
        self.model_draft = None;
        self.model_draft_label = None;
    }

    fn handle_freeform_note_key(&mut self, key: KeyEvent) -> Option<ViewAction> {
        if self.selected_step() != SetupStep::Constitution || !self.editing_freeform_note {
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.editing_freeform_note = false;
                Some(ViewAction::None)
            }
            KeyCode::Backspace => {
                self.freeform_note.pop();
                self.guided_preview_seen = false;
                self.model_draft = None;
                self.model_draft_label = None;
                Some(ViewAction::None)
            }
            KeyCode::Char(c) if key.modifiers.is_empty() => {
                let mut buf = [0; 4];
                self.append_freeform_note_text(c.encode_utf8(&mut buf));
                Some(ViewAction::None)
            }
            _ => Some(ViewAction::None),
        }
    }

    /// Install a model-drafted constitution (already sanitized + bounded by
    /// the untrusted-JSON gate) and return the `(title, content)` of the
    /// ratification preview the host must open in the same breath — that is
    /// what satisfies the preview gate. Ratifying still takes the explicit
    /// `G` keypress afterwards.
    ///
    /// Returns `None`, installing nothing, when the answers or note changed
    /// after the draft was requested: that draft is law for answers the
    /// person no longer holds (U08-06).
    #[must_use]
    pub(crate) fn install_model_draft(
        &mut self,
        constitution: Box<UserConstitution>,
        model_label: String,
    ) -> Option<(String, String)> {
        if self.model_draft_request.as_ref() != Some(&self.current_model_draft_request()) {
            return None;
        }
        self.model_draft_request = None;
        let content = constitution_ratification_text(
            self.locale,
            &constitution,
            &DraftProvenance::Model(model_label.clone()),
        );
        self.model_draft = Some(constitution);
        self.model_draft_label = Some(model_label);
        self.guided_preview_seen = true;
        Some((ratification_preview_title(self.locale).to_string(), content))
    }

    fn commit_constitution(&self, kind: SetupCommitKind) -> ViewAction {
        let choice = match kind {
            SetupCommitKind::BundledConstitution => ConstitutionChoice::Bundled,
            SetupCommitKind::DeferredConstitution => ConstitutionChoice::Deferred,
        };
        let mut state = self.state.clone();
        state.complete_constitution_checkpoint(CONSTITUTION_CHECKPOINT_VERSION, choice);
        state.constitution_source = ConstitutionSource::Bundled;
        state.constitution_validity = ConstitutionValidity::Unknown;
        state.constitution_authoring = None;
        state.constitution_preview_hash = None;
        state.set_step(
            SetupStep::Constitution,
            StepEntry::new(StepStatus::Verified, true, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(match kind {
                    SetupCommitKind::BundledConstitution => "bundled/default constitution",
                    SetupCommitKind::DeferredConstitution => "checkpoint deferred; bundled applies",
                }),
        );
        let message_id = match kind {
            SetupCommitKind::BundledConstitution => MessageId::SetupCheckpointDoneBundled,
            SetupCommitKind::DeferredConstitution => MessageId::SetupCheckpointDeferred,
        };
        ViewAction::EmitAndClose(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, message_id).to_string(),
        })
    }

    fn load_existing_constitution(&self) -> Option<UserConstitution> {
        if self.facts.constitution_file != SetupConstitutionFileState::Loaded {
            return None;
        }
        // Re-read the live file so a stale card cannot ratify a file that
        // has since become invalid; any non-loaded state leaves the key inert.
        UserConstitution::load().ok()?.constitution().cloned()
    }

    fn commit_existing_constitution_unchanged(&mut self) -> ViewAction {
        let Some(constitution) = self.load_existing_constitution() else {
            return ViewAction::None;
        };
        let mut state = self.state.clone();
        state.complete_constitution_checkpoint(
            CONSTITUTION_CHECKPOINT_VERSION,
            ConstitutionChoice::GuidedCustom,
        );
        state.constitution_source = ConstitutionSource::UserGlobal;
        state.constitution_validity = ConstitutionValidity::Valid;
        state.constitution_preview_hash = Some(constitution.preview_hash());
        state.set_step(
            SetupStep::Constitution,
            StepEntry::new(StepStatus::Verified, true, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result("existing constitution kept unchanged"),
        );
        self.state = state.clone();
        ViewAction::EmitAndClose(ViewEvent::SetupStateCommitRequested {
            state,
            message: tr(self.locale, MessageId::SetupCheckpointDoneKept).to_string(),
        })
    }

    /// Complete the checkpoint by keeping the existing valid
    /// `constitution.json` exactly as it stands (#3794). First `K` previews
    /// the rendered law; second `K` records the choice. The file is never
    /// rewritten — only `setup_state.json` changes, through the same commit
    /// event as every other completion.
    fn commit_keep_existing_constitution(&mut self) -> ViewAction {
        let Some(constitution) = self.load_existing_constitution() else {
            return ViewAction::None;
        };
        if !self.existing_preview_seen {
            self.existing_preview_seen = true;
            let content = constitution_ratification_text(
                self.locale,
                &constitution,
                &DraftProvenance::Existing,
            );
            return ViewAction::Emit(ViewEvent::OpenTextPager {
                title: ratification_preview_title(self.locale).to_string(),
                content,
            });
        }
        self.commit_existing_constitution_unchanged()
    }

    fn status_label(&self, status: StepStatus) -> Cow<'static, str> {
        tr(
            self.locale,
            match status {
                StepStatus::NotStarted => MessageId::SetupStatusNotStarted,
                StepStatus::Recommended => MessageId::SetupStatusRecommended,
                StepStatus::Optional => MessageId::SetupStatusOptional,
                StepStatus::Deferred => MessageId::SetupStatusDeferred,
                StepStatus::InProgress => MessageId::SetupStatusInProgress,
                StepStatus::Configured => MessageId::PickerActionConfigured,
                StepStatus::NeedsAction => MessageId::SetupStatusNeedsAction,
                StepStatus::Verified => MessageId::SetupStatusVerified,
                StepStatus::Skipped => MessageId::SetupStatusSkipped,
                StepStatus::Failed => MessageId::SetupStatusFailed,
            },
        )
    }
}

impl ModalView for SetupWizardView {
    fn kind(&self) -> ModalKind {
        ModalKind::SetupWizard
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if let Some(action) = self.handle_freeform_note_key(key) {
            return action;
        }
        if self.selected_step() == SetupStep::Constitution && !self.constitution_advanced {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
                KeyCode::Left | KeyCode::Char('b') => {
                    self.move_back();
                    ViewAction::None
                }
                KeyCode::Char('c') => self.open_constitution_advanced(),
                KeyCode::Enter => self.commit_simple_constitution(),
                _ => ViewAction::None,
            };
        }
        if self.selected_step() == SetupStep::Constitution
            && self.constitution_advanced
            && key.code == KeyCode::Esc
        {
            return self.close_constitution_advanced();
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Char('i') | KeyCode::Char('?') if self.progressive_guide => {
                self.details_expanded = !self.details_expanded;
                self.body_scroll = 0;
                ViewAction::None
            }
            KeyCode::Left | KeyCode::Char('b') => {
                self.move_back();
                ViewAction::None
            }
            KeyCode::Right | KeyCode::Char('n') => {
                self.move_next();
                ViewAction::None
            }
            KeyCode::PageUp => {
                self.body_scroll = self.body_scroll.saturating_sub(8);
                ViewAction::None
            }
            KeyCode::PageDown => {
                self.body_scroll = self.body_scroll.saturating_add(8);
                ViewAction::None
            }
            KeyCode::Up if self.progressive_guide => {
                self.body_scroll = self.body_scroll.saturating_sub(1);
                ViewAction::None
            }
            KeyCode::Down if self.progressive_guide => {
                self.body_scroll = self.body_scroll.saturating_add(1);
                ViewAction::None
            }
            KeyCode::Up => {
                self.move_back();
                ViewAction::None
            }
            KeyCode::Down => {
                self.move_next();
                ViewAction::None
            }
            KeyCode::Char('s') if self.progressive_guide => {
                if self.selected_step() == SetupStep::Verification {
                    ViewAction::Close
                } else {
                    self.move_next();
                    ViewAction::None
                }
            }
            KeyCode::Char('s') => {
                self.commit_selected_status(StepStatus::Skipped, MessageId::SetupStepSkipped, true)
            }
            KeyCode::Char('r')
                if self.progressive_guide
                    && matches!(
                        self.selected_step(),
                        SetupStep::RemoteRuntime | SetupStep::Verification
                    ) =>
            {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenRemoteControlRequested)
            }
            KeyCode::Char('p')
                if self.progressive_guide && self.selected_step() == SetupStep::Verification =>
            {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenProviderRequested)
            }
            KeyCode::Char('c')
                if self.progressive_guide && self.selected_step() == SetupStep::Verification =>
            {
                self.selected = visible_step_index(SetupStep::TrustSandbox);
                self.details_expanded = false;
                self.body_scroll = 0;
                ViewAction::None
            }
            KeyCode::Char('r')
                if !self.progressive_guide && self.selected_step() == SetupStep::ToolsMcp =>
            {
                self.preview_tools_mcp_on_ramp()
            }
            KeyCode::Char('r') if self.selected_step() == SetupStep::RemoteRuntime => {
                self.preview_remote_runtime_on_ramp()
            }
            KeyCode::Char('r') => self.commit_selected_status(
                StepStatus::NeedsAction,
                MessageId::SetupStepRetryRecorded,
                false,
            ),
            KeyCode::Char('g') if self.selected_step() == SetupStep::Constitution => {
                self.commit_guided_constitution()
            }
            KeyCode::Char('p') if self.selected_step() == SetupStep::ProviderModel => {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenProviderRequested)
            }
            KeyCode::Char('m') if self.selected_step() == SetupStep::ProviderModel => {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenModelRequested)
            }
            KeyCode::Char('p') if self.selected_step() == SetupStep::OperateFleet => {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenProviderRequested)
            }
            KeyCode::Char('f') if self.selected_step() == SetupStep::OperateFleet => {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenFleetRequested)
            }
            KeyCode::Char('h') if self.selected_step() == SetupStep::Hotbar => {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenHotbarRequested)
            }
            KeyCode::Char('m') if self.selected_step() == SetupStep::TrustSandbox => {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenModeRequested)
            }
            KeyCode::Char('c') if self.selected_step() == SetupStep::TrustSandbox => {
                ViewAction::EmitAndClose(ViewEvent::SetupOpenConfigRequested)
            }
            KeyCode::Char(key @ ('1' | '2' | '3'))
                if self.selected_step() == SetupStep::TrustSandbox =>
            {
                self.select_runtime_preset(key)
            }
            KeyCode::Char('a') if self.selected_step() == SetupStep::TrustSandbox => {
                self.commit_runtime_preset()
            }
            KeyCode::Char(key @ ('1' | '2' | '3' | '4' | '5' | '6'))
                if self.selected_step() == SetupStep::Constitution =>
            {
                self.cycle_guided_answer(key)
            }
            KeyCode::Char('a') if self.selected_step() == SetupStep::Constitution => {
                self.request_model_draft()
            }
            KeyCode::Char('f') if self.selected_step() == SetupStep::Constitution => {
                self.toggle_freeform_edit()
            }
            KeyCode::Char('k') if self.selected_step() == SetupStep::Constitution => {
                self.commit_keep_existing_constitution()
            }
            KeyCode::Char('u') if self.selected_step() == SetupStep::Constitution => {
                self.commit_constitution(SetupCommitKind::BundledConstitution)
            }
            KeyCode::Char('d') if self.selected_step() == SetupStep::Constitution => {
                self.commit_constitution(SetupCommitKind::DeferredConstitution)
            }
            KeyCode::Enter if self.selected_step() == SetupStep::Constitution => {
                self.commit_constitution(SetupCommitKind::BundledConstitution)
            }
            KeyCode::Enter if self.selected_step() == SetupStep::Language => {
                self.commit_language_review()
            }
            KeyCode::Enter if self.selected_step() == SetupStep::ProviderModel => {
                if self.progressive_guide && !self.facts.provider_ready {
                    ViewAction::EmitAndClose(ViewEvent::SetupOpenProviderRequested)
                } else {
                    self.commit_provider_model_review()
                }
            }
            KeyCode::Enter if self.selected_step() == SetupStep::TrustSandbox => {
                self.commit_runtime_posture_review()
            }
            KeyCode::Enter if self.selected_step() == SetupStep::OperateFleet => {
                self.commit_operate_fleet_review()
            }
            KeyCode::Enter if self.selected_step() == SetupStep::Hotbar => {
                self.commit_hotbar_review()
            }
            KeyCode::Enter if self.selected_step() == SetupStep::ToolsMcp => {
                self.commit_tools_mcp_review()
            }
            KeyCode::Enter if self.selected_step() == SetupStep::RemoteRuntime => {
                self.commit_remote_runtime_review()
            }
            KeyCode::Enter if self.selected_step() == SetupStep::Persistence => {
                self.commit_persistence_review()
            }
            KeyCode::Enter if self.selected_step() == SetupStep::Verification => {
                self.commit_setup_report()
            }
            KeyCode::Enter => {
                self.move_next();
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn handle_paste(&mut self, text: &str) -> bool {
        if self.selected_step() != SetupStep::Constitution || !self.constitution_advanced {
            return false;
        }
        self.append_freeform_note_text(text);
        true
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let inner = render_underwater_surface(area, buf, self.surface_title());
        let simple_constitution =
            self.selected_step() == SetupStep::Constitution && !self.constitution_advanced;
        let hints = if self.progressive_guide {
            self.progressive_action_hints()
        } else if simple_constitution {
            vec![
                ActionHint::new(
                    "Enter",
                    tr(
                        self.locale,
                        if self.facts.constitution_file == SetupConstitutionFileState::Loaded {
                            MessageId::SetupActionKeepExisting
                        } else {
                            MessageId::SetupActionUseRecommended
                        },
                    )
                    .to_string(),
                ),
                ActionHint::new(
                    "C",
                    tr(self.locale, MessageId::SetupActionCustomize).to_string(),
                ),
                ActionHint::new("B", tr(self.locale, MessageId::SetupActionBack).to_string()),
                ActionHint::new(
                    "Esc",
                    tr(self.locale, MessageId::SetupActionCancel).to_string(),
                ),
            ]
        } else {
            let mut hints = vec![
                ActionHint::new(
                    "Enter",
                    tr(self.locale, MessageId::SetupActionContinue).to_string(),
                ),
                ActionHint::new("B", tr(self.locale, MessageId::SetupActionBack).to_string()),
                ActionHint::new("S", tr(self.locale, MessageId::SetupActionSkip).to_string()),
            ];
            self.extend_focused_action_hints(&mut hints);
            hints.push(ActionHint::new(
                "Esc",
                tr(self.locale, MessageId::SetupActionCancel).to_string(),
            ));
            hints
        };
        let content_area = render_modal_footer(inner, buf, &hints);
        let spec = self.selected_spec();
        let (title_text, question_text) = if self.progressive_guide {
            match self.selected_step() {
                SetupStep::Verification if self.facts.provider_ready => (
                    tr(self.locale, MessageId::OnboardReadyTitle).into_owned(),
                    tr(self.locale, MessageId::OnboardReadyLead).into_owned(),
                ),
                SetupStep::ProviderModel | SetupStep::Verification => (
                    tr(self.locale, MessageId::OnboardProviderTitle).into_owned(),
                    tr(self.locale, MessageId::OnboardProviderBlurb).into_owned(),
                ),
                SetupStep::TrustSandbox => (
                    tr(self.locale, MessageId::SetupStepTrustSandboxTitle).into_owned(),
                    tr(self.locale, MessageId::SetupRuntimePostureReviewHint).into_owned(),
                ),
                SetupStep::RemoteRuntime => (
                    "/rc".to_string(),
                    tr(self.locale, MessageId::CmdRemoteControlDescription).into_owned(),
                ),
                _ => (
                    tr(self.locale, spec.title_id()).into_owned(),
                    tr(self.locale, spec.why_id()).into_owned(),
                ),
            }
        } else {
            (
                tr(self.locale, spec.title_id()).into_owned(),
                tr(self.locale, spec.why_id()).into_owned(),
            )
        };
        let title = Line::from(Span::styled(
            title_text,
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        ));
        let why = Line::from(Span::raw(question_text));
        let mut lines = vec![title, why, Line::from("")];
        lines.extend(self.selected_step_detail_lines());
        let wrap_width = usize::from(content_area.width).max(1);
        let visual_rows: usize = lines
            .iter()
            .map(|line| line.width().div_ceil(wrap_width).max(1))
            .sum();
        let visible_rows = usize::from(content_area.height).max(1);
        let max_scroll = visual_rows.saturating_sub(visible_rows);
        let scroll = self.body_scroll.min(max_scroll);
        let content_area =
            render_panel_scroll_rail(content_area, buf, visual_rows, scroll, visible_rows, true);
        // Explicit base ink: same black-on-black hazard as the pager body;
        // value spans below carry no fg of their own.
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0))
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .render(content_area, buf);
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl SetupWizardView {
    fn progressive_action_hints(&self) -> Vec<ActionHint> {
        let back = || ActionHint::new("B", tr(self.locale, MessageId::SetupActionBack).to_string());
        let exit = || {
            ActionHint::new(
                "Esc",
                tr(self.locale, MessageId::SetupActionCancel).to_string(),
            )
        };
        let details = || {
            ActionHint::new(
                "I",
                tr(self.locale, MessageId::CtxMenuOpenDetails).to_string(),
            )
        };
        let skip = || ActionHint::new("S", tr(self.locale, MessageId::SetupActionSkip).to_string());
        let mut hints = match self.selected_step() {
            SetupStep::ProviderModel => vec![
                ActionHint::new(
                    "Enter",
                    tr(
                        self.locale,
                        if self.facts.provider_ready {
                            MessageId::SetupActionContinue
                        } else {
                            MessageId::SetupActionProvider
                        },
                    )
                    .to_string(),
                ),
                skip(),
                details(),
                exit(),
            ],
            SetupStep::TrustSandbox => vec![
                ActionHint::new(
                    "Enter",
                    tr(self.locale, MessageId::SetupActionKeepExisting).to_string(),
                ),
                skip(),
                details(),
                exit(),
            ],
            SetupStep::RemoteRuntime => vec![
                ActionHint::new(
                    "R",
                    tr(self.locale, MessageId::CmdRemoteControlDescription).to_string(),
                ),
                skip(),
                details(),
                exit(),
            ],
            SetupStep::ToolsMcp => vec![
                ActionHint::new(
                    "Enter",
                    tr(self.locale, MessageId::SetupActionContinue).to_string(),
                ),
                skip(),
                details(),
                exit(),
            ],
            SetupStep::Verification => {
                let mut hints = vec![
                    ActionHint::new(
                        "Enter",
                        tr(self.locale, MessageId::OnboardReadyStart).to_string(),
                    ),
                    ActionHint::new(
                        "C",
                        tr(self.locale, MessageId::SetupActionCustomize).to_string(),
                    ),
                    details(),
                    exit(),
                ];
                if !self.facts.provider_ready {
                    hints.insert(
                        0,
                        ActionHint::new(
                            "P",
                            tr(self.locale, MessageId::SetupActionProvider).to_string(),
                        ),
                    );
                }
                hints
            }
            _ => vec![exit()],
        };
        let position = self
            .progressive_steps()
            .iter()
            .position(|step| *step == self.selected_step())
            .unwrap_or(0);
        if position > 0 {
            hints.insert(hints.len().saturating_sub(1), back());
        }
        hints
    }

    fn extend_focused_action_hints(&self, hints: &mut Vec<ActionHint>) {
        match self.selected_step() {
            SetupStep::Constitution if self.constitution_advanced => {
                hints.push(ActionHint::new(
                    "1-6",
                    tr(self.locale, MessageId::SetupActionTuneGuided).to_string(),
                ));
                hints.push(ActionHint::new(
                    "G",
                    tr(self.locale, MessageId::SetupActionGuided).to_string(),
                ));
                hints.push(ActionHint::new(
                    "F",
                    tr(self.locale, MessageId::SetupActionFreeform).to_string(),
                ));
            }
            SetupStep::ProviderModel => {
                hints.push(ActionHint::new(
                    "P",
                    tr(self.locale, MessageId::SetupActionProvider).to_string(),
                ));
                hints.push(ActionHint::new(
                    "M",
                    tr(self.locale, MessageId::SetupActionModel).to_string(),
                ));
            }
            SetupStep::OperateFleet => hints.push(ActionHint::new(
                "F",
                tr(self.locale, MessageId::SetupActionFleet).to_string(),
            )),
            SetupStep::Hotbar => hints.push(ActionHint::new(
                "H",
                tr(self.locale, MessageId::SetupActionHotbar).to_string(),
            )),
            SetupStep::ToolsMcp | SetupStep::RemoteRuntime => hints.push(ActionHint::new(
                "R",
                tr(self.locale, MessageId::SetupActionRetry).to_string(),
            )),
            SetupStep::TrustSandbox => hints.push(ActionHint::new(
                "C",
                tr(self.locale, MessageId::SetupActionConfig).to_string(),
            )),
            _ => {}
        }
    }

    fn selected_step_detail_lines(&self) -> Vec<Line<'static>> {
        if self.progressive_guide {
            return if self.details_expanded {
                self.progressive_expanded_lines()
            } else {
                self.progressive_detail_lines()
            };
        }
        match self.selected_step() {
            SetupStep::ProviderModel => self.provider_model_detail_lines(),
            SetupStep::TrustSandbox => self.runtime_posture_detail_lines(),
            SetupStep::Constitution if self.constitution_advanced => {
                self.constitution_detail_lines()
            }
            SetupStep::Constitution => self.constitution_simple_lines(),
            SetupStep::OperateFleet => self.operate_fleet_detail_lines(),
            SetupStep::Hotbar => self.hotbar_detail_lines(),
            SetupStep::ToolsMcp => self.tools_mcp_detail_lines(),
            SetupStep::RemoteRuntime => self.remote_runtime_detail_lines(),
            SetupStep::Persistence => self.persistence_detail_lines(),
            SetupStep::Verification => self.verification_detail_lines(),
            _ => Vec::new(),
        }
    }

    fn progressive_detail_lines(&self) -> Vec<Line<'static>> {
        match self.selected_step() {
            SetupStep::ProviderModel => {
                let answer = format!(
                    "{} · {} · {}",
                    self.facts.provider, self.facts.model, self.facts.auth
                );
                vec![self.detail_row(MessageId::SetupCardRouteLabel, &answer)]
            }
            SetupStep::TrustSandbox => {
                let answer = format!(
                    "{} · {} · {}",
                    self.facts.approval, self.facts.trust, self.facts.sandbox
                );
                let mut lines = vec![self.detail_row(MessageId::SetupCardApprovalLabel, &answer)];
                if let Some(warning) = &self.facts.project_override_warning {
                    lines.push(
                        self.detail_row(MessageId::SetupRuntimeProjectOverrideLabel, warning),
                    );
                }
                lines
            }
            SetupStep::RemoteRuntime => vec![self.detail_row(
                MessageId::SetupRemoteModeLabel,
                &self.facts.remote_control_result,
            )],
            SetupStep::ToolsMcp => {
                let status = tr(
                    self.locale,
                    if self.facts.tools_mcp_needs_action {
                        MessageId::SetupStatusNeedsAction
                    } else {
                        MessageId::SetupStatusVerified
                    },
                )
                .into_owned();
                vec![self.detail_row(MessageId::SetupStepToolsMcpTitle, &status)]
            }
            SetupStep::Verification => self.progressive_summary_lines(),
            _ => self.selected_step_detail_lines_expanded(),
        }
    }

    fn selected_step_detail_lines_expanded(&self) -> Vec<Line<'static>> {
        match self.selected_step() {
            SetupStep::ProviderModel => self.provider_model_detail_lines(),
            SetupStep::TrustSandbox => self.runtime_posture_detail_lines(),
            SetupStep::ToolsMcp => self.tools_mcp_detail_lines(),
            SetupStep::RemoteRuntime => self.remote_runtime_detail_lines(),
            SetupStep::Verification => self.verification_detail_lines(),
            _ => Vec::new(),
        }
    }

    fn progressive_expanded_lines(&self) -> Vec<Line<'static>> {
        if self.selected_step() != SetupStep::Verification {
            return self.selected_step_detail_lines_expanded();
        }
        let mut lines = self.progressive_summary_lines();
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "/settings  ",
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(tr(self.locale, MessageId::CmdSettingsDescription).to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "/config <key>  ",
                Style::default()
                    .fg(palette::TEXT_MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(tr(self.locale, MessageId::SetupActionConfig).to_string()),
        ]));
        lines
    }

    fn progressive_summary_lines(&self) -> Vec<Line<'static>> {
        let route = format!("{} · {}", self.facts.provider, self.facts.model);
        let permissions = format!(
            "{} · {} · {}",
            self.facts.approval, self.facts.trust, self.facts.sandbox
        );
        let mut lines = vec![
            self.detail_row(MessageId::SetupStepProviderModelTitle, &route),
            self.detail_row(MessageId::SetupStepTrustSandboxTitle, &permissions),
            self.detail_row(
                MessageId::SetupStepRemoteRuntimeTitle,
                &self.facts.remote_control_result,
            ),
        ];
        if self.tools_relevant() {
            let tools_status = tr(
                self.locale,
                if self.facts.tools_mcp_needs_action {
                    MessageId::SetupStatusNeedsAction
                } else {
                    MessageId::SetupStatusVerified
                },
            )
            .into_owned();
            lines.push(self.detail_row(MessageId::SetupStepToolsMcpTitle, &tools_status));
        }
        lines
    }

    fn constitution_simple_lines(&self) -> Vec<Line<'static>> {
        if self.facts.constitution_file == SetupConstitutionFileState::Loaded {
            return vec![
                self.detail_row(
                    MessageId::SetupConstitutionExistingLabel,
                    &self
                        .facts
                        .constitution_file
                        .label(self.state.constitution_choice, self.locale),
                ),
                Line::from(Span::styled(
                    tr(
                        self.locale,
                        MessageId::SetupConstitutionExistingDefaultDetail,
                    )
                    .to_string(),
                    Style::default().fg(palette::TEXT_MUTED),
                )),
            ];
        }
        if !matches!(
            self.facts.constitution_file,
            SetupConstitutionFileState::NotChecked | SetupConstitutionFileState::Missing
        ) {
            return vec![
                self.detail_row(
                    MessageId::SetupConstitutionExistingLabel,
                    &self
                        .facts
                        .constitution_file
                        .label(self.state.constitution_choice, self.locale),
                ),
                Line::from(Span::styled(
                    tr(self.locale, MessageId::SetupConstitutionRepairDefaultDetail).to_string(),
                    Style::default().fg(palette::TEXT_MUTED),
                )),
            ];
        }

        let recommendation = format!(
            "{} · {}",
            tr(self.locale, MessageId::SetupStatusRecommended),
            autonomy_label(AutonomyPreference::Balanced, self.locale)
        );
        vec![
            Line::from(Span::styled(
                recommendation,
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                autonomy_priority(AutonomyPreference::Balanced, self.locale).to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
        ]
    }

    fn provider_model_detail_lines(&self) -> Vec<Line<'static>> {
        vec![
            self.detail_row(MessageId::SetupCardRouteLabel, &self.facts.provider),
            self.detail_row(MessageId::SetupCardModelLabel, &self.facts.model),
            self.detail_row(MessageId::SetupCardAuthLabel, &self.facts.auth),
            self.detail_row(MessageId::SetupCardHealthLabel, &self.facts.health),
            Line::from(Span::styled(
                tr(
                    self.locale,
                    if self.facts.provider_ready {
                        MessageId::SetupProviderModelReadyHint
                    } else {
                        MessageId::SetupProviderModelNeedsActionHint
                    },
                )
                .to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
        ]
    }

    fn constitution_detail_lines(&self) -> Vec<Line<'static>> {
        let choice = constitution_choice_label(self.state.constitution_choice);
        let source = constitution_source_label(self.state.constitution_source);
        let validity = constitution_validity_label(self.state.constitution_validity);
        let source_state = format!("{source}; validity {validity}");
        let existing_file = self
            .facts
            .constitution_file
            .label(self.state.constitution_choice, self.locale);
        let expert_override = self.facts.expert_override.label(self.locale);
        let preview = self
            .state
            .constitution_preview_hash
            .as_deref()
            .unwrap_or("not accepted yet")
            .to_string();
        let mut lines = vec![
            self.detail_row(MessageId::SetupConstitutionChoiceLabel, choice),
            self.detail_row(MessageId::SetupConstitutionSourceLabel, &source_state),
            self.detail_row(MessageId::SetupConstitutionPreviewLabel, &preview),
            self.detail_row(MessageId::SetupConstitutionExistingLabel, &existing_file),
            self.detail_row(
                MessageId::SetupConstitutionExpertOverrideLabel,
                &expert_override,
            ),
            Line::from(Span::styled(
                tr(self.locale, MessageId::SetupConstitutionGuidedAnswersHint).to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
            self.guided_answer_pair(
                (
                    "1",
                    MessageId::SetupConstitutionPurposeLabel,
                    &self.guided_draft.purpose.label(self.locale),
                ),
                (
                    "2",
                    MessageId::SetupConstitutionAutonomyLabel,
                    autonomy_label(self.guided_draft.autonomy, self.locale),
                ),
            ),
            self.guided_answer_pair(
                (
                    "3",
                    MessageId::SetupConstitutionEvidenceLabel,
                    &self.guided_draft.evidence.label(self.locale),
                ),
                (
                    "4",
                    MessageId::SetupConstitutionCommunicationLabel,
                    self.guided_draft.communication.label(self.locale),
                ),
            ),
            self.guided_answer_single(
                "5",
                MessageId::SetupConstitutionPrivacyLabel,
                self.guided_draft.privacy.label(self.locale),
            ),
            self.guided_answer_single(
                "6",
                MessageId::SetupConstitutionPrinciplesLabel,
                self.guided_draft.principles.label(self.locale),
            ),
            freeform_note_line(self.locale, &self.freeform_note, self.editing_freeform_note),
        ];
        if self.facts.constitution_file == SetupConstitutionFileState::Loaded {
            lines.push(Line::from(Span::styled(
                keep_existing_invitation_line(self.locale),
                Style::default().fg(palette::WHALE_HUMAN),
            )));
        }
        if let Some(label) = self
            .model_draft_label
            .as_deref()
            .filter(|_| self.model_draft.is_some())
        {
            lines.push(Line::from(Span::styled(
                model_draft_ready_line(self.locale, label),
                Style::default().fg(palette::STATUS_SUCCESS),
            )));
        } else if self.facts.provider_ready {
            lines.push(Line::from(Span::styled(
                model_draft_invitation_line(self.locale, &self.facts.model),
                Style::default().fg(palette::WHALE_HUMAN),
            )));
        }
        lines.push(Line::from(Span::styled(
            tr(self.locale, MessageId::SetupConstitutionGuidedHint).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        )));
        lines
    }

    fn runtime_posture_detail_lines(&self) -> Vec<Line<'static>> {
        let project_override = self
            .facts
            .project_override_warning
            .clone()
            .unwrap_or_else(|| {
                tr(self.locale, MessageId::SetupRuntimeProjectOverrideNone).to_string()
            });
        let mut lines = vec![
            self.detail_row(MessageId::SetupCardIntentLabel, &self.facts.work_intent),
            self.detail_row(MessageId::SetupCardApprovalLabel, &self.facts.approval),
            self.detail_row(MessageId::SetupCardShellLabel, &self.facts.shell),
            self.detail_row(MessageId::SetupCardTrustLabel, &self.facts.trust),
            self.detail_row(MessageId::SetupCardSandboxLabel, &self.facts.sandbox),
            self.detail_row(MessageId::SetupCardNetworkLabel, &self.facts.network),
            self.detail_row(
                MessageId::SetupRuntimePresetSelectedLabel,
                &runtime_preset_summary(self.locale, self.runtime_preset),
            ),
            self.detail_row(
                MessageId::SetupRuntimePresetDiffLabel,
                &runtime_preset_inline_diff(self.runtime_preset, &self.facts),
            ),
            self.detail_row(
                MessageId::SetupRuntimeProjectOverrideLabel,
                &project_override,
            ),
            Line::from(Span::styled(
                tr(self.locale, MessageId::SetupRuntimePostureBoundary).to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
            Line::from(Span::styled(
                tr(self.locale, MessageId::SetupRuntimePresetSafetyFloor).to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
            self.setup_review_hint_line(
                MessageId::SetupRuntimePostureReviewHint,
                Some("Press M for work mode or C for config."),
            ),
            Line::from(Span::styled(
                tr(self.locale, MessageId::SetupRuntimePresetApplyHint).to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
        ];
        for (idx, preset) in SetupRuntimePreset::ALL.iter().enumerate() {
            let marker = if *preset == self.runtime_preset {
                ">"
            } else {
                " "
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "{marker} {}. {}",
                    idx + 1,
                    runtime_preset_summary(self.locale, *preset)
                ),
                Style::default().fg(if *preset == self.runtime_preset {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }),
            )));
        }
        lines
    }

    fn operate_fleet_detail_lines(&self) -> Vec<Line<'static>> {
        let route = format!("{} / {}", self.facts.provider, self.facts.model);
        let readiness = self.ready_label(self.operate_fleet_facts_ready());
        vec![
            self.detail_row(MessageId::SetupCardRouteLabel, &route),
            self.detail_row(MessageId::SetupCardAuthLabel, &self.facts.auth),
            self.detail_row(
                MessageId::SetupOperateRuntimeLabel,
                &self.facts.operate_runtime_result,
            ),
            self.detail_row(
                MessageId::SetupOperateRosterLabel,
                &self.facts.fleet_roster_result,
            ),
            self.detail_row(
                MessageId::SetupOperateConcurrencyLabel,
                &self.facts.operate_concurrency_result,
            ),
            self.detail_row(MessageId::SetupOperateReadinessLabel, &readiness),
            self.setup_review_hint_line(MessageId::SetupOperateReviewHint, None),
        ]
    }

    fn hotbar_detail_lines(&self) -> Vec<Line<'static>> {
        vec![
            self.detail_row(
                MessageId::SetupHotbarBindingsLabel,
                &self.facts.hotbar_bindings_result,
            ),
            self.detail_row(
                MessageId::SetupHotbarActionsLabel,
                &self.facts.hotbar_actions_result,
            ),
            self.setup_review_hint_line(
                MessageId::SetupHotbarReviewHint,
                Some("Press H to customize slots."),
            ),
        ]
    }

    fn tools_mcp_detail_lines(&self) -> Vec<Line<'static>> {
        vec![
            self.detail_row(
                MessageId::SetupToolsMcpServersLabel,
                &self.facts.tools_mcp_servers_result,
            ),
            self.detail_row(
                MessageId::SetupToolsMcpSkillsLabel,
                &self.facts.tools_mcp_skills_result,
            ),
            self.detail_row(
                MessageId::SetupToolsMcpToolsLabel,
                &self.facts.tools_mcp_tools_result,
            ),
            self.detail_row(
                MessageId::SetupToolsMcpPluginsLabel,
                &self.facts.tools_mcp_plugins_result,
            ),
            self.detail_row(
                MessageId::SetupToolsMcpHotbarLabel,
                &self.facts.tools_mcp_hotbar_result,
            ),
            self.detail_row(
                MessageId::SetupToolsMcpDshLabel,
                &self.facts.tools_mcp_dsh_result,
            ),
            self.setup_review_hint_line(
                MessageId::SetupToolsMcpReviewHint,
                Some("Press R for safe on-ramps (no auto-run)."),
            ),
        ]
    }

    /// #3409: one row per mode, each carrying its own observed status. The
    /// registry counts stay available in the preview; the card itself answers
    /// "where can this be reached from?" in four plain lines.
    fn remote_runtime_detail_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for fact in &self.facts.remote_modes {
            lines.push(self.detail_row(
                fact.mode.label_id(),
                &format!(
                    "{} · {}",
                    tr(self.locale, fact.status.label_id()),
                    fact.detail
                ),
            ));
        }
        if lines.is_empty() {
            lines.push(self.detail_row(
                MessageId::SetupRemoteModeLabel,
                &self.facts.remote_mode_result,
            ));
        }
        lines.push(self.detail_row(
            MessageId::SetupRemoteProvidersLabel,
            &self.facts.remote_providers_result,
        ));
        lines.push(self.setup_review_hint_line(
            MessageId::SetupRemoteReviewHint,
            Some("Press R to preview (nothing is written). Enter keeps local-only."),
        ));
        lines
    }

    fn persistence_detail_lines(&self) -> Vec<Line<'static>> {
        vec![
            self.detail_row(
                MessageId::SetupPersistenceHomeLabel,
                &self.facts.persistence.home_result,
            ),
            self.detail_row(
                MessageId::SetupPersistenceConfigLabel,
                &self.facts.persistence.config_result,
            ),
            self.detail_row(
                MessageId::SetupPersistenceStateLabel,
                &self.facts.persistence.state_result,
            ),
            self.detail_row(
                MessageId::SetupPersistenceConstitutionLabel,
                &self.facts.persistence.constitution_result,
            ),
            self.detail_row(
                MessageId::SetupPersistenceMemoryLabel,
                &self.facts.persistence.memory_result,
            ),
            self.detail_row(
                MessageId::SetupPersistenceNotesLabel,
                &self.facts.persistence.notes_result,
            ),
            self.setup_review_hint_line(MessageId::SetupPersistenceReviewHint, None),
        ]
    }

    fn verification_detail_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            self.detail_row(
                MessageId::SetupReportFirstRunLabel,
                &self.ready_label(self.state.first_run_ready()),
            ),
            self.detail_row(
                MessageId::SetupReportUpdateLabel,
                &self.ready_label(self.state.update_ready(CONSTITUTION_CHECKPOINT_VERSION)),
            ),
            self.detail_row(
                MessageId::SetupReportOperateLabel,
                &self.ready_label(self.state.operate_ready()),
            ),
            self.detail_row(
                MessageId::SetupReportSourceLabel,
                &self.state_source_label(),
            ),
            self.detail_row(
                MessageId::SetupReportAutonomyLabel,
                &self.facts.constitution_autonomy,
            ),
            self.detail_row(
                MessageId::SetupReportRuntimePostureLabel,
                &self.facts.runtime_result,
            ),
            Line::from(""),
            Line::from(Span::styled(
                tr(self.locale, MessageId::SetupReportRowsLabel).to_string(),
                Style::default()
                    .fg(palette::TEXT_MUTED)
                    .add_modifier(Modifier::BOLD),
            )),
        ];

        for spec in STEP_SPECS {
            let step = spec.id();
            let entry = self.state.steps.get(&step);
            let required = entry.map_or(spec.required(), |entry| entry.required);
            let required_label = if required {
                tr(self.locale, MessageId::SetupReportRequired)
            } else {
                tr(self.locale, MessageId::SetupReportOptional)
            };
            let mut value = format!(
                "{} ({})",
                self.status_label(self.state.status(step)),
                required_label
            );
            if let Some(version) = entry.and_then(|entry| entry.version.as_deref()) {
                value.push_str(&format!(" · {version}"));
            }
            if let Some(result) = entry.and_then(|entry| entry.result.as_deref()) {
                value.push_str(&format!(" · {result}"));
            }
            lines.push(self.detail_row(spec.title_id(), &value));
        }

        lines.push(Line::from(""));
        let next_action = tr(self.locale, self.next_action_id()).to_string();
        lines.push(self.detail_row(MessageId::SetupReportNextActionLabel, &next_action));
        lines
    }

    fn setup_review_hint_line(
        &self,
        hint_id: MessageId,
        english_action: Option<&'static str>,
    ) -> Line<'static> {
        let hint = if self.locale == Locale::En {
            let mut hint = "Enter records this setup snapshot.".to_string();
            if let Some(action) = english_action {
                hint.push(' ');
                hint.push_str(action);
            }
            hint
        } else {
            tr(self.locale, hint_id).to_string()
        };
        Line::from(Span::styled(hint, Style::default().fg(palette::TEXT_MUTED)))
    }

    fn ready_label(&self, ready: bool) -> String {
        if ready {
            tr(self.locale, MessageId::SetupReportReady).to_string()
        } else {
            tr(self.locale, MessageId::SetupStatusNeedsAction).to_string()
        }
    }

    fn state_source_label(&self) -> String {
        if self.state.inherited {
            tr(self.locale, MessageId::SetupReportInherited).to_string()
        } else {
            tr(self.locale, MessageId::SetupReportPersisted).to_string()
        }
    }

    fn next_action_id(&self) -> MessageId {
        if !self.state.update_ready(CONSTITUTION_CHECKPOINT_VERSION) {
            return MessageId::SetupReportNextActionConstitution;
        }
        if !matches!(
            self.state.status(SetupStep::ProviderModel),
            StepStatus::Configured | StepStatus::Verified | StepStatus::NeedsAction
        ) {
            return MessageId::SetupReportNextActionProvider;
        }
        if !self.state.runtime_posture_source.is_reviewed() {
            return MessageId::SetupReportNextActionRuntime;
        }
        if !self.state.first_run_ready() {
            return MessageId::SetupReportNextActionRequired;
        }
        if !self.state.operate_ready() {
            return MessageId::SetupReportNextActionOperate;
        }
        MessageId::SetupReportNextActionNone
    }

    fn detail_row(&self, label: MessageId, value: &str) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("{} ", tr(self.locale, label)),
                Style::default()
                    .fg(palette::TEXT_MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(value.to_string()),
        ])
    }

    fn guided_answer_pair(
        &self,
        left: (&str, MessageId, &str),
        right: (&str, MessageId, &str),
    ) -> Line<'static> {
        let label_style = Style::default()
            .fg(palette::TEXT_MUTED)
            .add_modifier(Modifier::BOLD);
        Line::from(vec![
            Span::styled(
                format!("{} {} ", left.0, tr(self.locale, left.1)),
                label_style,
            ),
            Span::raw(left.2.to_string()),
            Span::styled("  ·  ", Style::default().fg(palette::TEXT_MUTED)),
            Span::styled(
                format!("{} {} ", right.0, tr(self.locale, right.1)),
                label_style,
            ),
            Span::raw(right.2.to_string()),
        ])
    }

    fn guided_answer_single(&self, key: &str, label: MessageId, value: &str) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("{key} {} ", tr(self.locale, label)),
                Style::default()
                    .fg(palette::TEXT_MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(value.to_string()),
        ])
    }
}

fn setup_report_ready(state: &SetupState) -> bool {
    state.first_run_ready() || state.update_ready(CONSTITUTION_CHECKPOINT_VERSION)
}

fn runtime_preset_summary(locale: Locale, preset: SetupRuntimePreset) -> String {
    format!(
        "{} - {}",
        tr(locale, preset.title_id()),
        tr(locale, preset.description_id())
    )
}

fn runtime_preset_inline_diff(preset: SetupRuntimePreset, facts: &SetupRuntimeFacts) -> String {
    runtime_preset_diff_rows(preset, facts).join("; ")
}

fn runtime_preset_preview_text(
    locale: Locale,
    preset: SetupRuntimePreset,
    facts: &SetupRuntimeFacts,
) -> String {
    let mut lines = vec![
        tr(locale, MessageId::SetupRuntimePresetPreviewTitle).to_string(),
        runtime_preset_summary(locale, preset),
        String::new(),
        tr(locale, MessageId::SetupRuntimePresetDiffLabel).to_string(),
    ];
    lines.extend(
        runtime_preset_diff_rows(preset, facts)
            .into_iter()
            .map(|row| format!("- {row}")),
    );
    lines.extend([
        String::new(),
        tr(locale, MessageId::SetupRuntimePostureBoundary).to_string(),
        tr(locale, MessageId::SetupRuntimePresetSafetyFloor).to_string(),
        tr(locale, MessageId::SetupRuntimePresetApplyHint).to_string(),
    ]);
    lines.join("\n")
}

fn runtime_preset_diff_rows(preset: SetupRuntimePreset, facts: &SetupRuntimeFacts) -> Vec<String> {
    let approval_target = preset.approval_policy().map_or_else(
        || "removed; Full Access comes from settings.permission_posture".to_string(),
        ToString::to_string,
    );
    let mut rows = vec![
        format!(
            "settings.default_mode: {} -> {}",
            facts.default_mode,
            preset.display_mode()
        ),
        format!(
            "settings.permission_posture: -> {}",
            preset.permission_posture()
        ),
        format!(
            "config.approval_policy: {} -> {}",
            facts.approval_policy_value, approval_target
        ),
        format!(
            "config.allow_shell: {} -> {}",
            facts.allow_shell_enabled,
            preset.allow_shell()
        ),
        format!(
            "config.sandbox_mode: {} -> {}",
            facts.sandbox_mode_value,
            preset.sandbox_mode()
        ),
        format!(
            "config.network.default: {} -> unchanged",
            facts.network_default_value
        ),
        format!("workspace trust: {} -> unchanged", facts.trust),
    ];
    if let Some(warning) = facts.project_override_warning.as_deref() {
        rows.push(format!("project override warning: {warning}"));
    }
    rows
}

fn project_runtime_override_warning(workspace: &Path, locale: Locale) -> Option<String> {
    let outcome = codewhale_config::load_project_config_outcome(workspace);
    // A project config that exists but can't be parsed is not the same as no
    // project config: its restrictions are silently not in effect, and the
    // workspace falls back to the user's baseline. Say so here rather than
    // only in a log line the TUI never shows.
    if let Some((path, reason)) = outcome.invalid() {
        return Some(
            tr(locale, MessageId::SetupProjectPermissionsInvalid)
                .replace("{path}", &path.display().to_string())
                .replace("{reason}", reason),
        );
    }
    let project = outcome.into_config()?;
    let mut fields = Vec::new();
    if let Some(policy) = project.approval_policy.as_deref() {
        fields.push(format!("approval_policy={policy}"));
    }
    if let Some(mode) = project.sandbox_mode.as_deref() {
        fields.push(format!("sandbox_mode={mode}"));
    }
    if fields.is_empty() {
        return None;
    }
    Some(
        tr(locale, MessageId::SetupProjectPermissionsOverride)
            .replace("{fields}", &fields.join(", ")),
    )
}

fn setup_report_result(state: &SetupState, facts: &SetupRuntimeFacts) -> String {
    format!(
        "first_run={}, update={}, operate={}, constitution={:?}, autonomy={}, posture={:?}, runtime={}, operate_fleet={}",
        if state.first_run_ready() {
            "ready"
        } else {
            "needs_action"
        },
        if state.update_ready(CONSTITUTION_CHECKPOINT_VERSION) {
            "ready"
        } else {
            "needs_action"
        },
        if state.operate_ready() {
            "ready"
        } else {
            "needs_action"
        },
        state.constitution_choice,
        facts.constitution_autonomy,
        state.runtime_posture_source,
        facts.runtime_result,
        facts.operate_result
    )
}

fn remote_runtime_on_ramp_text(locale: Locale, facts: &SetupRuntimeFacts) -> String {
    remote::on_ramp_text(
        locale,
        &facts.remote_clouds_result,
        &facts.remote_bridges_result,
        &facts.remote_providers_result,
        &facts.remote_mode_result,
        &facts.remote_command_provider,
    )
}

fn tools_mcp_on_ramp_text(locale: Locale, facts: &SetupRuntimeFacts) -> String {
    let tools_facts = tools_mcp::SetupToolsMcpFacts {
        servers_result: facts.tools_mcp_servers_result.clone(),
        skills_result: facts.tools_mcp_skills_result.clone(),
        tools_result: facts.tools_mcp_tools_result.clone(),
        plugins_result: facts.tools_mcp_plugins_result.clone(),
        hotbar_result: facts.tools_mcp_hotbar_result.clone(),
        dsh_result: facts.tools_mcp_dsh_result.clone(),
        result: facts.tools_mcp_result.clone(),
        overall_status: if facts.tools_mcp_needs_action {
            tools_mcp::InventoryStatus::NeedsConfig
        } else if facts.tools_mcp_result.contains("overall=off") {
            tools_mcp::InventoryStatus::Off
        } else {
            tools_mcp::InventoryStatus::Healthy
        },
        needs_action: facts.tools_mcp_needs_action,
        mcp_path_display: facts.tools_mcp_path_display.clone(),
        skills_path_display: facts.tools_mcp_skills_path_display.clone(),
        plugins_path_display: facts.tools_mcp_plugins_path_display.clone(),
    };
    tools_mcp::on_ramp_text(locale, &tools_facts)
}

/// Who authored the draft being previewed for ratification.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DraftProvenance {
    /// Rendered deterministically from the guided answers.
    Guided,
    /// Drafted by the named model, then sanitized and bounded by Codewhale.
    Model(String),
    /// The user's existing `constitution.json`, shown unchanged for the
    /// keep-existing checkpoint completion (#3794).
    Existing,
}

fn ratification_preview_title(locale: Locale) -> &'static str {
    match locale {
        Locale::Ja => "ユーザー憲法 - 批准前の草案",
        Locale::ZhHans => "用户宪章 — 确认前草案",
        Locale::ZhHant => "使用者憲法 - 批准前草案",
        Locale::PtBr => "Constituição do Usuário - Rascunho para Ratificação",
        Locale::Es419 => "Constitución del Usuario - Borrador para Ratificación",
        Locale::Vi => "Hiến pháp Người dùng - Bản nháp để phê chuẩn",
        Locale::Ko => "사용자 헌법 - 승인 전 초안",
        Locale::Ca => "Constitució de l'Usuari - Esborrany per a Ratificació",
        Locale::De => "Nutzerverfassung - Entwurf zur Ratifizierung",
        Locale::Fr => "Constitution de l'Utilisateur - Brouillon pour Ratification",
        Locale::Id => "Konstitusi Pengguna - Draf untuk Ratifikasi",
        Locale::Hi => "उपयोगकर्ता संविधान - अंगीकार हेतु मसौदा",
        Locale::Ru => "Конституция пользователя - Проект для ратификации",
        Locale::Uk => "Конституція користувача - Проєкт для ратифікації",
        _ => "User Constitution — Draft for Ratification",
    }
}

/// The ratification artifact shown in the pager: provenance, what a
/// constitution is, the exact block that will be injected (byte-identical to
/// prompt assembly's rendering), its authority boundaries, and how to ratify
/// or amend. Only the scaffold differs between guided and model drafts — the
/// law itself always comes from the same renderer.
fn constitution_ratification_text(
    locale: Locale,
    constitution: &UserConstitution,
    provenance: &DraftProvenance,
) -> String {
    const RULE: &str = "──────────────────────────────────────────────────────";
    let rendered = constitution
        .render_block(None)
        .unwrap_or_else(|| match locale {
            Locale::Ja => "構造化された憲法は空です。".to_string(),
            Locale::ZhHans => "结构化宪章为空。".to_string(),
            Locale::ZhHant => "結構化憲法為空。".to_string(),
            Locale::PtBr => "A constituição estruturada está vazia.".to_string(),
            Locale::Es419 => "La constitución estructurada está vacía.".to_string(),
            Locale::Vi => "Hiến pháp có cấu trúc đang trống.".to_string(),
            Locale::Ko => "구조화된 헌법이 비어 있습니다.".to_string(),
            Locale::Ca => "La constitució estructurada és buida.".to_string(),
            Locale::De => "Die strukturierte Verfassung ist leer.".to_string(),
            Locale::Fr => "La constitution structurée est vide.".to_string(),
            Locale::Id => "Konstitusi terstruktur kosong.".to_string(),
            Locale::Hi => "संरचित संविधान खाली है।".to_string(),
            Locale::Ru => "Структурированная конституция пуста.".to_string(),
            Locale::Uk => "Структурована конституція порожня.".to_string(),
            _ => "The structured constitution is empty.".to_string(),
        });

    match locale {
        Locale::Ja => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "{label} があなたのガイド回答から起草し、Codewhale が構造検証と境界制限を適用しました。"
                ),
                DraftProvenance::Guided => {
                    "あなたのガイド回答から決定的に生成されました。".to_string()
                }
                DraftProvenance::Existing => {
                    "既存の憲法を constitution.json から読み込み、変更せずに表示しています。"
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "これはすでに有効な基準です。プレビューを閉じて K を押すと、このまま保持してチェックポイントを完了します。\
                     ファイルは変更されません。/constitution または /setup でいつでも修正できます。"
                }
                _ => {
                    "確認するまで、どの内容も基準にはなりません。プレビューを閉じて G を押すと批准して保存します。\
                     /constitution または /setup でいつでも修正できます。"
                }
            };
            format!(
                "CODEWHALE · ユーザー憲法\n{RULE}\n\n{drafted_by}\n\n\
                 これは Codewhale があなたと協働するための常設の基準です。優れた憲法のように、使えるほど短く、\
                 網羅的な規則ではなく持続する原則で構成され、あなたの変化に合わせて修正できます。\
                 すべての個別判断を裁くのではなく権限と境界を定め、セッションを越えて協働を継続させます。\
                 ただしこれは記憶ではありません。履歴ではなく原則を保持します。\n\n\
                 {rendered}\n\n\
                 権限の階層\nあなたの直接の指示は常にこの文書より優先されます。\n\n\
                 これができないこと\n\
                 これは行動を導くものです。承認ポリシー、サンドボックス、Shell、ネットワーク、信頼、MCP 権限、\
                 既定モード、公開、支出の権限を付与または変更することはできません。これらは実行時にあなたが管理します。\n\n\
                 縮小コアと任意モジュール\n\
                 組み込みのコアは引き続き有効です。この草案はユーザーグローバルの長期設定だけを保存します。\
                 実行とオーケストレーションの機能は、ランタイムポリシー、現在のツールカタログ、または将来の任意モジュールから提供されます。このプレビューはモジュールを有効化せず、設定も変更しません。\n\n\
                 批准\n{ratify_how}"
            )
        }
        Locale::ZhHans => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "由 {label} 根据你的引导式答案起草，并已由 Codewhale 完成结构校验与边界限制。"
                ),
                DraftProvenance::Guided => "由你的引导式答案确定性生成。".to_string(),
                DraftProvenance::Existing => {
                    "你现有的宪章，读取自 constitution.json——原样展示，未做任何修改。".to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "这已是你当前使用的宪章。关闭此预览后按 K 保留并完成检查点——文件不会被修改。\
                     之后可随时用 /constitution 或 /setup 修改。"
                }
                _ => {
                    "未经你确认，任何内容都不会成为宪章。关闭此预览后按 G 确认并保存；\
                     之后可随时用 /constitution 或 /setup 修改。"
                }
            };
            format!(
                "CODEWHALE · 用户宪章\n{RULE}\n\n{drafted_by}\n\n\
                 这是 Codewhale 与你协作时长期遵循的偏好和规则。内容应保持简短、便于执行，以持久原则为主，并可随时调整。\
                 它界定协作方式与行为边界，而不是替你决定每一种情况；它让协作跨会话延续——但它不是记忆，只保留原则，不保留历史。\n\n\
                 {rendered}\n\n\
                 权限层级\n你的直接指令始终高于本文件。\n\n\
                 它不能做什么\n\
                 它只提供行为指导，不能授予或更改审批策略、沙箱、Shell、网络、信任、MCP 权限、默认模式、发布或支出权限——这些始终由你在运行时掌控。\n\n\
                 精简核心与可选策略\n\
                 内置核心始终生效。本草案只保存你的用户全局长期偏好。执行与编排能力来自运行时策略、当前工具目录或未来的可选规则包；此预览不会启用任何策略或更改配置。\n\n\
                 确认\n{ratify_how}"
            )
        }
        Locale::ZhHant => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "由 {label} 根據你的引導式答案起草，並已由 Codewhale 完成結構驗證與邊界限制。"
                ),
                DraftProvenance::Guided => "由你的引導式答案確定性生成。".to_string(),
                DraftProvenance::Existing => {
                    "你現有的憲法，讀取自 constitution.json；原樣展示，未做任何修改。".to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "這已是你現行的準則。關閉此預覽後按 K 保留並完成檢查點；\
                     檔案不會被修改。之後可隨時用 /constitution 或 /setup 修訂。"
                }
                _ => {
                    "未經你確認，任何內容都不會成為準則。關閉此預覽後按 G 批准並保存；\
                     之後可隨時用 /constitution 或 /setup 修訂。"
                }
            };
            format!(
                "CODEWHALE · 使用者憲法\n{RULE}\n\n{drafted_by}\n\n\
                 這是 Codewhale 與你協作的長期準則。像優秀的憲法一樣：足夠簡短因而可用，由持久原則而非詳盡規則構成，並且可以隨你修訂。\
                 它界定權力與邊界，而非裁決每個具體決定；它讓協作跨會話延續，但它不是記憶，它承載的是原則，而非歷史。\n\n\
                 {rendered}\n\n\
                 權限層級\n你的直接指令始終高於本文件。\n\n\
                 它不能做什麼\n\
                 它只提供行為指導，不能授予或更改審批策略、沙箱、Shell、網路、信任、MCP 權限、預設模式、發布或支出權限；這些始終由你在執行時掌控。\n\n\
                 精簡核心與可選模組\n\
                 內建核心始終生效。本草案只保存你的使用者全域長期偏好。執行與編排能力由執行時政策、即時工具目錄或未來的可選模組提供；此預覽不會啟用模組或更改其配置。\n\n\
                 批准\n{ratify_how}"
            )
        }
        Locale::PtBr => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Rascunhado por {label} a partir das suas respostas guiadas, depois validado por schema e limitado pelo Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Renderizado deterministicamente a partir das suas respostas guiadas.".to_string()
                }
                DraftProvenance::Existing => {
                    "Sua constituição existente, carregada de constitution.json, é exibida sem alterações."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Esta já é sua regra vigente. Feche a prévia e pressione K para mantê-la e concluir o checkpoint; \
                     o arquivo não será modificado. Edite quando quiser com /constitution ou /setup."
                }
                _ => {
                    "Nada vira regra até você confirmar. Feche a prévia e pressione G para ratificar e salvar. \
                     Edite quando quiser com /constitution ou /setup."
                }
            };
            format!(
                "CODEWHALE · CONSTITUIÇÃO DO USUÁRIO\n{RULE}\n\n{drafted_by}\n\n\
                 Esta é a regra permanente de como o Codewhale trabalha com você. Como boas constituições, \
                 ela é curta o bastante para ser usada, formada por princípios duráveis em vez de regras exaustivas, \
                 e pode ser emendada conforme você muda. Ela define poderes e limites em vez de decidir cada caso, \
                 e dá continuidade à colaboração entre sessões. Mas ela não é memória: carrega princípios, não histórico.\n\n\
                 {rendered}\n\n\
                 HIERARQUIA DE AUTORIDADE\nSeus pedidos diretos sempre superam este documento.\n\n\
                 O QUE ISTO NÃO PODE FAZER\n\
                 Isto orienta comportamento. Não pode conceder nem alterar política de aprovação, sandbox, shell, rede, \
                 confiança, permissões MCP, modo padrão, publicação ou autoridade para gastos; isso continua sob seu controle em tempo de execução.\n\n\
                 NÚCLEO REDUZIDO E MÓDULOS OPT-IN\n\
                 O núcleo embutido continua ativo. Este rascunho só salva suas preferências permanentes globais de usuário. \
                 Capacidades de execução e orquestração vêm da política de execução, do catálogo de ferramentas ativo ou de módulos opt-in futuros; esta prévia não ativa módulos nem muda sua configuração.\n\n\
                 RATIFICAÇÃO\n{ratify_how}"
            )
        }
        Locale::Es419 => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Redactado por {label} desde tus respuestas guiadas, luego validado por schema y acotado por Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Renderizado de forma determinística desde tus respuestas guiadas.".to_string()
                }
                DraftProvenance::Existing => {
                    "Tu constitución existente, cargada desde constitution.json, se muestra sin cambios."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Esta ya es tu regla vigente. Cierra la vista previa y presiona K para conservarla y completar el checkpoint; \
                     el archivo no se modifica. Puedes enmendarla cuando quieras con /constitution o /setup."
                }
                _ => {
                    "Nada se vuelve regla hasta que confirmes. Cierra la vista previa y presiona G para ratificar y guardar. \
                     Puedes enmendarla cuando quieras con /constitution o /setup."
                }
            };
            format!(
                "CODEWHALE · CONSTITUCIÓN DEL USUARIO\n{RULE}\n\n{drafted_by}\n\n\
                 Esta es la regla permanente de cómo Codewhale trabaja contigo. Como las buenas constituciones, \
                 es lo bastante breve para usarse, hecha de principios duraderos en vez de reglas exhaustivas, \
                 y enmendable a medida que cambias. Define poderes y límites en vez de decidir cada caso, \
                 y da continuidad a la colaboración entre sesiones. Pero no es memoria: lleva principios, no historial.\n\n\
                 {rendered}\n\n\
                 JERARQUÍA DE AUTORIDAD\nTus pedidos directos siempre superan este documento.\n\n\
                 LO QUE ESTO NO PUEDE HACER\n\
                 Orienta comportamiento. No puede conceder ni cambiar política de aprobación, sandbox, shell, red, \
                 confianza, permisos MCP, modo predeterminado, publicación o autoridad de gasto; eso sigue bajo tu control en tiempo de ejecución.\n\n\
                 NÚCLEO REDUCIDO Y MÓDULOS OPT-IN\n\
                 El núcleo integrado sigue activo. Este borrador solo guarda tus preferencias permanentes globales de usuario. \
                 Las capacidades de ejecución y orquestación provienen de la política de ejecución, el catálogo activo de herramientas o módulos opt-in futuros; esta vista previa no activa módulos ni cambia su configuración.\n\n\
                 RATIFICACIÓN\n{ratify_how}"
            )
        }
        Locale::Vi => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Được {label} soạn từ câu trả lời hướng dẫn của bạn, rồi được Codewhale kiểm tra schema và giới hạn biên."
                ),
                DraftProvenance::Guided => {
                    "Được kết xuất xác định từ câu trả lời hướng dẫn của bạn.".to_string()
                }
                DraftProvenance::Existing => {
                    "Hiến pháp hiện có của bạn, tải từ constitution.json, được hiển thị nguyên trạng."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Đây đã là luật hiện hành của bạn. Đóng bản xem trước rồi nhấn K để giữ nguyên và hoàn tất checkpoint; \
                     tệp không bị sửa. Có thể chỉnh bất cứ lúc nào bằng /constitution hoặc /setup."
                }
                _ => {
                    "Không có gì trở thành luật cho đến khi bạn xác nhận. Đóng bản xem trước rồi nhấn G để phê chuẩn và lưu. \
                     Có thể chỉnh bất cứ lúc nào bằng /constitution hoặc /setup."
                }
            };
            format!(
                "CODEWHALE · HIẾN PHÁP NGƯỜI DÙNG\n{RULE}\n\n{drafted_by}\n\n\
                 Đây là luật thường trực cho cách Codewhale làm việc với bạn. Giống các hiến pháp tốt, \
                 nó đủ ngắn để dùng, gồm các nguyên tắc bền vững thay vì luật lệ cạn kiệt, \
                 và có thể sửa khi bạn thay đổi. Nó định khung quyền hạn và giới hạn thay vì quyết định từng trường hợp, \
                 đồng thời giữ sự liên tục giữa các phiên. Nhưng nó không phải bộ nhớ: nó mang nguyên tắc, không mang lịch sử.\n\n\
                 {rendered}\n\n\
                 THỨ BẬC THẨM QUYỀN\nYêu cầu trực tiếp của bạn luôn cao hơn tài liệu này.\n\n\
                 ĐIỀU NÀY KHÔNG THỂ LÀM\n\
                 Nó hướng dẫn hành vi. Nó không thể cấp hoặc đổi chính sách phê duyệt, sandbox, shell, mạng, \
                 độ tin cậy, quyền MCP, chế độ mặc định, xuất bản hoặc quyền chi tiêu; những thứ đó vẫn do bạn kiểm soát lúc chạy.\n\n\
                 LÕI RÚT GỌN VÀ MÔ-ĐUN OPT-IN\n\
                 Lõi tích hợp vẫn hoạt động. Bản nháp này chỉ lưu tùy chọn thường trực toàn cục của người dùng. \
                 Khả năng thực thi và điều phối đến từ chính sách thời gian chạy, danh mục công cụ đang hoạt động hoặc mô-đun opt-in trong tương lai; bản xem trước này không bật mô-đun hoặc đổi cấu hình của chúng.\n\n\
                 PHÊ CHUẨN\n{ratify_how}"
            )
        }
        Locale::Ko => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "{label}이(가) 당신의 가이드 답변을 바탕으로 초안을 작성했고, Codewhale이 구조를 검증하고 범위를 제한했습니다."
                ),
                DraftProvenance::Guided => {
                    "당신의 가이드 답변으로부터 결정적으로 생성되었습니다.".to_string()
                }
                DraftProvenance::Existing => {
                    "constitution.json에서 불러온 기존 헌법이며, 변경 없이 그대로 표시됩니다."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "이것은 이미 당신의 상시 규칙입니다. 미리보기를 닫고 K를 눌러 그대로 유지하며 체크포인트를 완료하세요; \
                     파일은 수정되지 않습니다. /constitution 또는 /setup으로 언제든지 수정할 수 있습니다."
                }
                _ => {
                    "확인하기 전까지는 아무것도 규칙이 되지 않습니다. 미리보기를 닫고 G를 눌러 승인하고 저장하세요. \
                     /constitution 또는 /setup으로 언제든지 수정할 수 있습니다."
                }
            };
            format!(
                "CODEWHALE · 사용자 헌법\n{RULE}\n\n{drafted_by}\n\n\
                 이것은 Codewhale이 당신과 함께 일하는 방식에 대한 상시 규칙입니다. 훌륭한 헌법이 그렇듯, \
                 사용할 수 있을 만큼 짧고, 소모적인 규칙이 아닌 지속적인 원칙으로 이루어져 있으며, 당신이 변화함에 따라 수정할 수 있습니다. \
                 이는 모든 개별 사례를 판단하는 대신 권한과 한계를 규정하며, 세션을 넘어 협업의 연속성을 부여합니다. \
                 다만 이것은 기억이 아닙니다: 이력이 아니라 원칙을 담습니다.\n\n\
                 {rendered}\n\n\
                 권한 계층\n당신의 직접적인 요청은 언제나 이 문서보다 우선합니다.\n\n\
                 이것이 할 수 없는 일\n\
                 이것은 행동을 안내할 뿐입니다. 승인 정책, 샌드박스, 셸, 네트워크, 신뢰, MCP 권한, 기본 모드, 게시, 지출 권한을 \
                 부여하거나 바꿀 수 없습니다; 이는 여전히 런타임에서 당신이 직접 관리합니다.\n\n\
                 축소된 코어와 옵트인 모듈\n\
                 내장된 코어는 계속 활성 상태입니다. 이 초안은 사용자 전역의 상시 선호만 저장합니다. \
                 실행 및 오케스트레이션 기능은 런타임 정책, 현재 도구 카탈로그 또는 향후 옵트인 모듈에서 제공됩니다. 이 미리보기는 모듈을 활성화하지 않으며 그 설정도 바꾸지 않습니다.\n\n\
                 승인\n{ratify_how}"
            )
        }
        Locale::Ca => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Redactat per {label} a partir de les teves respostes guiades, després validat per esquema i acotat per Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Generat determinísticament a partir de les teves respostes guiades.".to_string()
                }
                DraftProvenance::Existing => {
                    "La teva constitució existent, carregada de constitution.json, es mostra sense canvis."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Aquesta ja és la teva llei vigent. Tanca la previsualització i prem K per conservar-la i completar el punt de control; \
                     el fitxer no es modifica. Esmena-la en qualsevol moment amb /constitution o /setup."
                }
                _ => {
                    "Res no esdevé llei fins que ho confirmis. Tanca la previsualització i prem G per ratificar i desar. \
                     Esmena-la en qualsevol moment amb /constitution o /setup."
                }
            };
            format!(
                "CODEWHALE · CONSTITUCIÓ DE L'USUARI\n{RULE}\n\n{drafted_by}\n\n\
                 Aquesta és la llei permanent de com Codewhale treballa amb tu. Com les bones constitucions, \
                 és prou curta per usar-se, feta de principis duradors en lloc de regles exhaustives, \
                 i esmenable a mesura que canvies. Defineix poders i límits en lloc de decidir cada cas, \
                 i dona continuïtat a la col·laboració entre sessions — però no és memòria: porta principis, no història.\n\n\
                 {rendered}\n\n\
                 JERARQUIA D'AUTORITAT\nLes teves peticions directes sempre prevalen sobre aquest document.\n\n\
                 EL QUE AIXÒ NO POT FER\n\
                 Orienta el comportament. No pot concedir ni canviar la política d'aprovació, sandbox, shell, xarxa, \
                 confiança, permisos MCP, mode per defecte, publicació o autoritat de despesa; això queda sota el teu control en temps d'execució.\n\n\
                 NUCLI REDUÏT I MÒDULS OPT-IN\n\
                 El nucli inclòs continua actiu. Aquest esborrany només desa les teves preferències permanents globals d'usuari. \
                 Les capacitats d'execució i orquestració provenen de la política d'execució, el catàleg d'eines actiu o futurs mòduls opt-in; aquesta previsualització no activa mòduls ni canvia la seva configuració.\n\n\
                 RATIFICACIÓ\n{ratify_how}"
            )
        }
        Locale::De => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Entworfen von {label} aus deinen geführten Antworten, dann schema-geprüft und begrenzt durch Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Deterministisch aus deinen geführten Antworten erzeugt.".to_string()
                }
                DraftProvenance::Existing => {
                    "Deine bestehende Verfassung, geladen aus constitution.json — unverändert gezeigt."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Dies ist bereits dein geltendes Recht. Schließe die Vorschau und drücke K, um sie zu behalten und den Checkpoint abzuschließen — \
                     die Datei wird nicht verändert. Jederzeit mit /constitution oder /setup änderbar."
                }
                _ => {
                    "Nichts wird Recht, bevor du bestätigst. Schließe die Vorschau und drücke G, um zu ratifizieren und zu speichern. \
                     Jederzeit mit /constitution oder /setup änderbar."
                }
            };
            format!(
                "CODEWHALE · NUTZERVERFASSUNG\n{RULE}\n\n{drafted_by}\n\n\
                 Dies ist das geltende Gesetz dafür, wie Codewhale mit dir arbeitet. Wie die besten Verfassungen \
                 ist sie kurz genug, um genutzt zu werden, besteht aus dauerhaften Prinzipien statt erschöpfender Regeln \
                 und lässt sich ändern, wenn du dich änderst. Sie rahmt Befugnisse und Grenzen, statt jeden Einzelfall zu entscheiden, \
                 und gibt deiner Zusammenarbeit Kontinuität über Sitzungen hinweg — aber sie ist kein Gedächtnis: Sie trägt Prinzipien, nicht Geschichte.\n\n\
                 {rendered}\n\n\
                 HIERARCHIE DER AUTORITÄT\nDeine direkten Anweisungen stehen immer über diesem Dokument.\n\n\
                 WAS DIES NICHT KANN\n\
                 Sie leitet Verhalten. Sie kann keine Freigaberichtlinie, Sandbox, Shell, Netzwerk, \
                 Vertrauen, MCP-Berechtigungen, Standardmodus, Veröffentlichung oder Ausgabenbefugnis gewähren oder ändern — die bleiben zur Laufzeit in deiner Hand.\n\n\
                 REDUZIERTER KERN UND OPT-IN-MODULE\n\
                 Der mitgelieferte Kern bleibt aktiv. Dieser Entwurf speichert nur deine benutzer-globalen Dauerpräferenzen. \
                 Ausführungs- und Orchestrierungsfähigkeiten kommen aus der Laufzeitrichtlinie, dem aktuellen Werkzeugkatalog oder künftigen Opt-in-Modulen; diese Vorschau aktiviert keine Module und ändert nicht ihre Konfiguration.\n\n\
                 RATIFIZIERUNG\n{ratify_how}"
            )
        }
        Locale::Fr => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Rédigé par {label} à partir de vos réponses guidées, puis validé par schéma et borné par Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Généré de façon déterministe à partir de vos réponses guidées.".to_string()
                }
                DraftProvenance::Existing => {
                    "Votre constitution existante, chargée depuis constitution.json — affichée sans modification."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "C'est déjà votre loi permanente. Fermez cet aperçu, puis appuyez sur K pour la conserver et terminer le point de contrôle — \
                     le fichier n'est pas modifié. Amendez-la à tout moment avec /constitution ou /setup."
                }
                _ => {
                    "Rien ne devient loi avant votre confirmation. Fermez cet aperçu, puis appuyez sur G pour ratifier et enregistrer. \
                     Amendez-la à tout moment avec /constitution ou /setup."
                }
            };
            format!(
                "CODEWHALE · CONSTITUTION DE L'UTILISATEUR\n{RULE}\n\n{drafted_by}\n\n\
                 Voici la loi permanente qui régit la façon dont Codewhale travaille avec vous. Comme les meilleures constitutions, \
                 elle est assez courte pour être utilisée, faite de principes durables plutôt que de règles exhaustives, \
                 et amendable à mesure que vous changez. Elle encadre les pouvoirs et les limites plutôt que de trancher chaque cas, \
                 et donne à votre collaboration une continuité entre les sessions — mais elle n'est pas une mémoire : elle porte des principes, pas un historique.\n\n\
                 {rendered}\n\n\
                 HIÉRARCHIE D'AUTORITÉ\nVos demandes directes priment toujours sur ce document.\n\n\
                 CE QU'ELLE NE PEUT PAS FAIRE\n\
                 Elle guide le comportement. Elle ne peut ni accorder ni modifier la politique d'approbation, le sandbox, le shell, le réseau, \
                 la confiance, les permissions MCP, le mode par défaut, la publication ou le pouvoir de dépense — ceux-ci restent entre vos mains à l'exécution.\n\n\
                 NOYAU RÉDUIT ET MODULES OPT-IN\n\
                 Le noyau intégré reste actif. Ce brouillon n'enregistre que vos préférences permanentes globales. \
                 Les capacités d'exécution et d'orchestration proviennent de la politique d'exécution, du catalogue d'outils actif ou de futurs modules opt-in ; cet aperçu n'active pas de modules et ne change pas leur configuration.\n\n\
                 RATIFICATION\n{ratify_how}"
            )
        }
        Locale::Id => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Disusun oleh {label} dari jawaban terpandu Anda, lalu diperiksa skemanya dan dibatasi oleh Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Dihasilkan secara deterministik dari jawaban terpandu Anda.".to_string()
                }
                DraftProvenance::Existing => {
                    "Konstitusi Anda yang ada, dimuat dari constitution.json — ditampilkan tanpa perubahan."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Ini sudah menjadi hukum tetap Anda. Tutup pratinjau ini, lalu tekan K untuk mempertahankannya dan menyelesaikan checkpoint — \
                     file tidak diubah. Amendemen kapan saja dengan /constitution atau /setup."
                }
                _ => {
                    "Tidak ada yang menjadi hukum sampai Anda mengonfirmasi. Tutup pratinjau ini, lalu tekan G untuk meratifikasi dan menyimpan. \
                     Amendemen kapan saja dengan /constitution atau /setup."
                }
            };
            format!(
                "CODEWHALE · KONSTITUSI PENGGUNA\n{RULE}\n\n{drafted_by}\n\n\
                 Ini adalah hukum tetap tentang cara Codewhale bekerja dengan Anda. Seperti konstitusi terbaik, \
                 ia cukup singkat untuk dipakai, tersusun dari prinsip yang awet alih-alih aturan yang menyeluruh, \
                 dan dapat diamendemen seiring Anda berubah. Ia membingkai wewenang dan batasan alih-alih memutuskan setiap kasus, \
                 dan memberi kolaborasi Anda kesinambungan lintas sesi — tetapi ia bukan memori: ia membawa prinsip, bukan riwayat.\n\n\
                 {rendered}\n\n\
                 HIERARKI OTORITAS\nPermintaan langsung Anda selalu mengungguli dokumen ini.\n\n\
                 APA YANG TIDAK BISA DILAKUKANNYA\n\
                 Ia memandu perilaku. Ia tidak dapat memberi atau mengubah kebijakan persetujuan, sandbox, shell, jaringan, \
                 kepercayaan, izin MCP, mode default, publikasi, atau wewenang belanja — semua itu tetap di tangan Anda saat runtime.\n\n\
                 INTI RINGKAS DAN MODUL OPT-IN\n\
                 Inti bawaan tetap aktif. Draf ini hanya menyimpan preferensi tetap global pengguna Anda. \
                 Kemampuan eksekusi dan orkestrasi berasal dari kebijakan runtime, katalog alat aktif, atau modul opt-in mendatang; pratinjau ini tidak mengaktifkan modul atau mengubah konfigurasinya.\n\n\
                 RATIFIKASI\n{ratify_how}"
            )
        }
        Locale::Hi => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "{label} द्वारा आपके गाइडेड उत्तरों से तैयार, फिर Codewhale द्वारा स्कीमा-जाँचा और सीमित किया गया।"
                ),
                DraftProvenance::Guided => "आपके गाइडेड उत्तरों से नियत रूप से तैयार किया गया।".to_string(),
                DraftProvenance::Existing => {
                    "आपका मौजूदा संविधान, constitution.json से लोड किया गया — अपरिवर्तित दिखाया गया।"
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "यह पहले से ही आपका स्थायी कानून है। यह पूर्वावलोकन बंद करें, फिर इसे बनाए रखने और चेकपॉइंट पूरा करने के लिए K दबाएँ — \
                     फ़ाइल संशोधित नहीं होती। /constitution या /setup से कभी भी संशोधित करें।"
                }
                _ => {
                    "जब तक आप पुष्टि नहीं करते, कुछ भी कानून नहीं बनता। यह पूर्वावलोकन बंद करें, फिर अंगीकार और सहेजने के लिए G दबाएँ। \
                     /constitution या /setup से कभी भी संशोधित करें।"
                }
            };
            format!(
                "CODEWHALE · उपयोगकर्ता संविधान\n{RULE}\n\n{drafted_by}\n\n\
                 यह Codewhale आपके साथ कैसे काम करे, इसका स्थायी कानून है। सर्वोत्तम संविधानों की तरह, \
                 यह उपयोग के लिए पर्याप्त छोटा है, संपूर्ण नियमों के बजाय टिकाऊ सिद्धांतों से बना है, \
                 और आपके बदलने के साथ संशोधनीय है। यह हर मामले का फ़ैसला करने के बजाय शक्तियों और सीमाओं का ढाँचा देता है, \
                 और आपके सहयोग को सत्रों के पार निरंतरता देता है — लेकिन यह मेमोरी नहीं है: यह इतिहास नहीं, सिद्धांत रखता है।\n\n\
                 {rendered}\n\n\
                 अधिकार पदानुक्रम\nआपके प्रत्यक्ष अनुरोध हमेशा इस दस्तावेज़ से ऊपर हैं।\n\n\
                 यह क्या नहीं कर सकता\n\
                 यह व्यवहार का मार्गदर्शन करता है। यह अनुमति नीति, सैंडबॉक्स, शेल, नेटवर्क, \
                 ट्रस्ट, MCP अनुमतियाँ, डिफ़ॉल्ट मोड, प्रकाशन या खर्च का अधिकार प्रदान या परिवर्तित नहीं कर सकता — वे रनटाइम पर आपके हाथ में रहते हैं।\n\n\
                 संक्षिप्त कोर और ऑप्ट-इन मॉड्यूल\n\
                 Bundled कोर सक्रिय रहता है। यह मसौदा केवल आपकी उपयोगकर्ता-वैश्विक स्थायी प्राथमिकताएँ सहेजता है। \
                 भारी निष्पादन या ऑर्केस्ट्रेशन सिद्धांत मोड प्रॉम्प्ट या भविष्य के ऑप्ट-इन मॉड्यूल में रहते हैं; यह पूर्वावलोकन मॉड्यूल सक्षम नहीं करता और न ही उनकी कॉन्फ़िगरेशन बदलता है।\n\n\
                 अंगीकार\n{ratify_how}"
            )
        }
        Locale::Ru => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Подготовлено {label} на основе ваших ответов на наводящие вопросы, затем проверено по схеме и ограничено Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Детерминированно построено из ваших ответов на наводящие вопросы.".to_string()
                }
                DraftProvenance::Existing => {
                    "Ваша существующая конституция, загруженная из constitution.json, — показана без изменений."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Это уже ваш действующий закон. Закройте это превью, затем нажмите K, чтобы сохранить её и завершить контрольную точку — \
                     файл не изменяется. Изменить можно в любое время через /constitution или /setup."
                }
                _ => {
                    "Ничто не становится законом, пока вы не подтвердите. Закройте это превью, затем нажмите G, чтобы ратифицировать и сохранить. \
                     Изменить можно в любое время через /constitution или /setup."
                }
            };
            format!(
                "CODEWHALE · КОНСТИТУЦИЯ ПОЛЬЗОВАТЕЛЯ\n{RULE}\n\n{drafted_by}\n\n\
                 Это постоянный закон о том, как Codewhale работает с вами. Как лучшие конституции, \
                 она достаточно коротка, чтобы ей пользоваться, состоит из долговечных принципов, а не исчерпывающих правил, \
                 и может изменяться вместе с вами. Она очерчивает полномочия и границы, а не решает каждый случай, \
                 и придаёт вашему сотрудничеству непрерывность между сессиями — но она не память: она хранит принципы, а не историю.\n\n\
                 {rendered}\n\n\
                 ИЕРАРХИЯ ПОЛНОМОЧИЙ\nВаши прямые указания всегда важнее этого документа.\n\n\
                 ЧЕГО ОНА НЕ МОЖЕТ\n\
                 Она направляет поведение. Она не может предоставить или изменить политику одобрения, sandbox, shell, сеть, \
                 доверие, разрешения MCP, режим по умолчанию, публикацию или право тратить — они остаются в ваших руках во время выполнения.\n\n\
                 СОКРАЩЁННОЕ ЯДРО И ОПЦИОНАЛЬНЫЕ МОДУЛИ\n\
                 Встроенное ядро остаётся активным. Этот проект сохраняет только ваши глобальные постоянные предпочтения. \
                 Тяжёлая доктрина исполнения или оркестрации принадлежит промптам режимов или будущим опциональным модулям; это превью не включает модули и не меняет их конфигурацию.\n\n\
                 РАТИФИКАЦИЯ\n{ratify_how}"
            )
        }
        Locale::Uk => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Підготовлено {label} на основі ваших відповідей на навідні запитання, потім перевірено за схемою та обмежено Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Детерміновано побудовано з ваших відповідей на навідні запитання.".to_string()
                }
                DraftProvenance::Existing => {
                    "Ваша чинна конституція, завантажена з constitution.json, — показана без змін."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "Це вже ваш чинний закон. Закрийте це прев'ю, потім натисніть K, щоб зберегти її та завершити контрольну точку — \
                     файл не змінюється. Змінити можна будь-коли через /constitution або /setup."
                }
                _ => {
                    "Ніщо не стає законом, доки ви не підтвердите. Закрийте це прев'ю, потім натисніть G, щоб ратифікувати та зберегти. \
                     Змінити можна будь-коли через /constitution або /setup."
                }
            };
            format!(
                "CODEWHALE · КОНСТИТУЦІЯ КОРИСТУВАЧА\n{RULE}\n\n{drafted_by}\n\n\
                 Це постійний закон про те, як Codewhale працює з вами. Як найкращі конституції, \
                 вона достатньо коротка, щоб нею користуватися, складається з довговічних принципів, а не вичерпних правил, \
                 і може змінюватися разом із вами. Вона окреслює повноваження та межі, а не вирішує кожен випадок, \
                 і надає вашій співпраці неперервність між сесіями — але вона не пам'ять: вона зберігає принципи, а не історію.\n\n\
                 {rendered}\n\n\
                 ІЄРАРХІЯ ПОВНОВАЖЕНЬ\nВаші прямі вказівки завжди важливіші за цей документ.\n\n\
                 ЧОГО ВОНА НЕ МОЖЕ\n\
                 Вона спрямовує поведінку. Вона не може надати або змінити політику схвалення, sandbox, shell, мережу, \
                 довіру, дозволи MCP, режим за замовчуванням, публікацію чи право витрачати — вони залишаються у ваших руках під час виконання.\n\n\
                 СКОРОЧЕНЕ ЯДРО Й ОПЦІЙНІ МОДУЛІ\n\
                 Вбудоване ядро залишається активним. Цей проєкт зберігає лише ваші глобальні постійні вподобання. \
                 Важка доктрина виконання чи оркестрації належить промптам режимів або майбутнім опційним модулям; це прев'ю не вмикає модулі й не змінює їхню конфігурацію.\n\n\
                 РАТИФІКАЦІЯ\n{ratify_how}"
            )
        }
        _ => {
            let drafted_by = match provenance {
                DraftProvenance::Model(label) => format!(
                    "Drafted by {label} from your guided answers, then schema-checked and bounded by Codewhale."
                ),
                DraftProvenance::Guided => {
                    "Rendered deterministically from your guided answers.".to_string()
                }
                DraftProvenance::Existing => {
                    "Your existing constitution, loaded from constitution.json — shown unchanged."
                        .to_string()
                }
            };
            let ratify_how = match provenance {
                DraftProvenance::Existing => {
                    "This is already your standing law. Close this preview, then press K to \
                     keep it and complete the checkpoint — the file is not modified. Amend \
                     anytime with /constitution or /setup."
                }
                _ => {
                    "Nothing becomes law until you confirm. Close this preview, then press G to \
                     ratify and save. Amend anytime with /constitution or /setup."
                }
            };
            format!(
                "CODEWHALE · USER CONSTITUTION\n{RULE}\n\n{drafted_by}\n\n\
                 This is the standing law for how Codewhale works with you. Like the best \
                 constitutions, it is short enough to use, made of durable principles rather \
                 than exhaustive rules, and amendable as you change. It frames powers and \
                 limits rather than deciding every case, and it gives your collaboration \
                 continuity across sessions — but it is not memory: it carries principles, \
                 not history.\n\n\
                 {rendered}\n\n\
                 HIERARCHY OF AUTHORITY\nYour direct requests always outrank this document.\n\n\
                 WHAT THIS CANNOT DO\n\
                 It guides behavior. It cannot grant or change approval policy, sandbox, shell, \
                 network, trust, MCP permissions, default mode, publishing, or spending \
                 authority — those stay under your hand at runtime.\n\n\
                 REDUCED CORE AND OPT-IN MODULES\n\
                 The bundled core stays active. This draft only saves your user-global \
                 standing preferences. Execution and orchestration capabilities come from runtime \
                 policy, the live tool catalog, or future opt-in modules; this preview does not enable modules or change \
                 their configuration.\n\n\
                 RATIFICATION\n{ratify_how}"
            )
        }
    }
}

/// Card line inviting the user to let their configured model draft the law.
fn model_draft_invitation_line(locale: Locale, model_label: &str) -> String {
    match locale {
        Locale::Ja => {
            format!("A {model_label} が起草し、あなたが批准します。確認するまで保存しません。")
        }
        Locale::ZhHans => {
            format!("A {model_label} 生成草案，由你确认。未经确认不会保存。")
        }
        Locale::ZhHant => {
            format!("A {model_label} 起草，你批准。未經確認不會保存。")
        }
        Locale::PtBr => {
            format!("A {model_label} pode rascunhar. Você ratifica. Nada salva sem você.")
        }
        Locale::Es419 => {
            format!("A {model_label} puede redactarla. Tú ratificas. Nada se guarda sin ti.")
        }
        Locale::Vi => {
            format!("A {model_label} có thể soạn. Bạn phê chuẩn. Không lưu gì nếu chưa có bạn.")
        }
        Locale::Ko => {
            format!(
                "A {model_label}이(가) 초안을 작성할 수 있습니다. 승인은 당신이 합니다. 당신 없이는 아무것도 저장되지 않습니다."
            )
        }
        Locale::Ca => {
            format!("A {model_label} la pot redactar. Tu la ratifiques. Res no es desa sense tu.")
        }
        Locale::De => {
            format!(
                "A {model_label} kann sie entwerfen. Du ratifizierst sie. Ohne dich wird nichts gespeichert."
            )
        }
        Locale::Fr => {
            format!(
                "A {model_label} peut la rédiger. Vous la ratifiez. Rien ne s'enregistre sans vous."
            )
        }
        Locale::Id => {
            format!(
                "A {model_label} dapat menyusunnya. Anda yang meratifikasi. Tidak ada yang tersimpan tanpa Anda."
            )
        }
        Locale::Hi => {
            format!(
                "A {model_label} इसका मसौदा बना सकता है। अंगीकार आप करते हैं। आपके बिना कुछ भी सहेजा नहीं जाता।"
            )
        }
        Locale::Ru => {
            format!(
                "A {model_label} может подготовить проект. Ратифицируете вы. Без вас ничего не сохраняется."
            )
        }
        Locale::Uk => {
            format!(
                "A {model_label} може підготувати проєкт. Ратифікуєте ви. Без вас нічого не зберігається."
            )
        }
        _ => format!("A {model_label} can draft it. You ratify it. Nothing saves without you."),
    }
}

/// Card line offering to keep an existing valid constitution unchanged.
fn keep_existing_invitation_line(locale: Locale) -> &'static str {
    match locale {
        Locale::Ja => "K 既存の憲法を保持 - 確認して保持、ファイルは変更しません。",
        Locale::ZhHans => "K 保留现有宪章——先查看，再保留，文件不变。",
        Locale::ZhHant => "K 保留現有憲法 - 先查看，再保留，檔案不變。",
        Locale::PtBr => "K Manter constituição existente - revise, mantenha, arquivo inalterado.",
        Locale::Es419 => {
            "K Conservar constitución existente - revisa, conserva, archivo sin cambios."
        }
        Locale::Vi => "K Giữ hiến pháp hiện có - xem lại, giữ nguyên, tệp không đổi.",
        Locale::Ko => "K 기존 헌법 유지 - 검토 후 유지, 파일은 변경되지 않음.",
        Locale::Ca => {
            "K Mantén la constitució existent - revisa-la, conserva-la, fitxer sense canvis."
        }
        Locale::De => "K Bestehende Verfassung behalten - prüfen, behalten, Datei unverändert.",
        Locale::Fr => {
            "K Garder votre constitution existante - révisez-la, gardez-la, fichier inchangé."
        }
        Locale::Id => {
            "K Pertahankan konstitusi Anda yang ada - tinjau, pertahankan, file tidak berubah."
        }
        Locale::Hi => "K अपना मौजूदा संविधान रखें - समीक्षा करें, बनाए रखें, फ़ाइल अपरिवर्तित।",
        Locale::Ru => {
            "K Сохранить существующую конституцию - просмотрите, сохраните, файл не изменяется."
        }
        Locale::Uk => "K Зберегти чинну конституцію - перегляньте, збережіть, файл без змін.",
        _ => "K Keep your existing constitution — review it, keep it, file unchanged.",
    }
}

/// Card line shown while a model draft awaits ratification.
fn model_draft_ready_line(locale: Locale, model_label: &str) -> String {
    match locale {
        Locale::Ja => {
            format!(
                "{model_label} の草案が批准待ちです - G で確認して批准、1-6 で草案を破棄します。"
            )
        }
        Locale::ZhHans => {
            format!("{model_label} 的草案待确认——按 G 查看并确认；按 1-6 会丢弃草案。")
        }
        Locale::ZhHant => {
            format!("{model_label} 的草案待批准 - 按 G 查看並批准；按 1-6 會丟棄草案。")
        }
        Locale::PtBr => {
            format!(
                "Rascunho de {model_label} aguarda ratificação - G para revisar e ratificar; 1-6 descarta."
            )
        }
        Locale::Es419 => {
            format!(
                "El borrador de {model_label} espera ratificación - G para revisar y ratificar; 1-6 lo descarta."
            )
        }
        Locale::Vi => {
            format!(
                "Bản nháp của {model_label} chờ phê chuẩn - G để xem và phê chuẩn; 1-6 sẽ bỏ bản nháp."
            )
        }
        Locale::Ko => {
            format!(
                "{model_label}의 초안이 승인을 기다리고 있습니다 - G로 확인하고 승인, 1-6은 초안을 버립니다."
            )
        }
        Locale::Ca => {
            format!(
                "L'esborrany de {model_label} espera ratificació - G per revisar i ratificar; 1-6 el descarta."
            )
        }
        Locale::De => {
            format!(
                "Entwurf von {model_label} wartet auf Ratifizierung - G zum Prüfen und Ratifizieren; 1-6 verwirft ihn."
            )
        }
        Locale::Fr => {
            format!(
                "Le brouillon de {model_label} attend ratification - G pour réviser et ratifier ; 1-6 l'écarte."
            )
        }
        Locale::Id => {
            format!(
                "Draf oleh {model_label} menunggu ratifikasi - G untuk meninjau dan meratifikasi; 1-6 membuangnya."
            )
        }
        Locale::Hi => {
            format!(
                "{model_label} का मसौदा अंगीकार की प्रतीक्षा में है - समीक्षा और अंगीकार के लिए G; 1-6 उसे खारिज करता है।"
            )
        }
        Locale::Ru => {
            format!(
                "Проект от {model_label} ожидает ратификации - G для просмотра и ратификации; 1-6 отклоняет его."
            )
        }
        Locale::Uk => {
            format!(
                "Проєкт від {model_label} очікує ратифікації - G для перегляду та ратифікації; 1-6 відхиляє його."
            )
        }
        _ => format!(
            "Draft by {model_label} awaits ratification — G to review and ratify; 1-6 discards it."
        ),
    }
}

/// Host-facing status line after a successful model draft.
pub(crate) fn model_draft_ready_message(locale: Locale, model_label: &str) -> String {
    match locale {
        Locale::Ja => format!(
            "{model_label} があなたの憲法を起草しました。プレビューを確認してから G で批准してください。"
        ),
        Locale::ZhHans => {
            format!("{model_label} 已生成你的宪章草案。请查看预览，然后按 G 确认。")
        }
        Locale::ZhHant => format!("{model_label} 已起草你的憲法。請查看預覽，然後按 G 批准。"),
        Locale::PtBr => format!(
            "{model_label} rascunhou sua constituição. Revise a prévia e pressione G para ratificar."
        ),
        Locale::Es419 => format!(
            "{model_label} redactó tu constitución. Revisa la vista previa y presiona G para ratificar."
        ),
        Locale::Vi => format!(
            "{model_label} đã soạn hiến pháp của bạn. Xem bản xem trước rồi nhấn G để phê chuẩn."
        ),
        Locale::Ko => format!(
            "{model_label}이(가) 당신의 헌법 초안을 작성했습니다. 미리보기를 확인한 뒤 G를 눌러 승인하세요."
        ),
        Locale::Ca => format!(
            "{model_label} ha redactat la teva constitució. Revisa la previsualització i prem G per ratificar."
        ),
        Locale::De => format!(
            "{model_label} hat deine Verfassung entworfen. Prüfe die Vorschau und drücke G zum Ratifizieren."
        ),
        Locale::Fr => format!(
            "{model_label} a rédigé votre constitution. Révisez l'aperçu, puis appuyez sur G pour ratifier."
        ),
        Locale::Id => format!(
            "{model_label} menyusun konstitusi Anda. Tinjau pratinjaunya, lalu tekan G untuk meratifikasi."
        ),
        Locale::Hi => format!(
            "{model_label} ने आपके संविधान का मसौदा तैयार किया। पूर्वावलोकन देखें, फिर अंगीकार के लिए G दबाएँ।"
        ),
        Locale::Ru => format!(
            "{model_label} подготовил проект вашей конституции. Просмотрите превью, затем нажмите G для ратификации."
        ),
        Locale::Uk => format!(
            "{model_label} підготував проєкт вашої конституції. Перегляньте прев'ю, потім натисніть G для ратифікації."
        ),
        _ => format!(
            "{model_label} drafted your constitution. Review the preview, then press G to ratify."
        ),
    }
}

/// Host-facing status line when model drafting fails or is unavailable. The
/// guided deterministic draft always remains the standing fallback.
pub(crate) fn model_draft_failed_message(
    locale: Locale,
    model_label: &str,
    reason: &str,
) -> String {
    match locale {
        Locale::Ja => {
            format!(
                "{model_label} は起草を完了できませんでした（{reason}）。ガイド草案は有効です。G でプレビューして批准できます。"
            )
        }
        Locale::ZhHans => {
            format!("{model_label} 未能生成草案（{reason}）。引导式草案仍可使用——按 G 预览并确认。")
        }
        Locale::ZhHant => {
            format!("{model_label} 未能完成起草（{reason}）。引導式草案仍然有效；按 G 預覽並批准。")
        }
        Locale::PtBr => {
            format!(
                "{model_label} não conseguiu rascunhar sua constituição ({reason}). O rascunho guiado continua válido; pressione G para pré-visualizar e ratificar."
            )
        }
        Locale::Es419 => {
            format!(
                "{model_label} no pudo redactar tu constitución ({reason}). El borrador guiado sigue válido; presiona G para previsualizar y ratificar."
            )
        }
        Locale::Vi => {
            format!(
                "{model_label} không thể soạn hiến pháp của bạn ({reason}). Bản nháp hướng dẫn vẫn hợp lệ; nhấn G để xem trước và phê chuẩn."
            )
        }
        Locale::Ko => {
            format!(
                "{model_label}이(가) 당신의 헌법 초안을 작성하지 못했습니다 ({reason}). 가이드 초안은 여전히 유효합니다. G를 눌러 미리보고 승인하세요."
            )
        }
        Locale::Ca => {
            format!(
                "{model_label} no ha pogut redactar la teva constitució ({reason}). L'esborrany guiat continua vigent; prem G per previsualitzar i ratificar."
            )
        }
        Locale::De => {
            format!(
                "{model_label} konnte deine Verfassung nicht entwerfen ({reason}). Dein geführter Entwurf bleibt gültig; drücke G für Vorschau und Ratifizierung."
            )
        }
        Locale::Fr => {
            format!(
                "{model_label} n'a pas pu rédiger votre constitution ({reason}). Votre brouillon guidé reste valide ; appuyez sur G pour l'aperçu et la ratification."
            )
        }
        Locale::Id => {
            format!(
                "{model_label} tidak dapat menyusun konstitusi Anda ({reason}). Draf terpandu Anda tetap berlaku; tekan G untuk pratinjau dan ratifikasi."
            )
        }
        Locale::Hi => {
            format!(
                "{model_label} आपके संविधान का मसौदा नहीं बना सका ({reason})। आपका गाइडेड मसौदा अब भी मान्य है; पूर्वावलोकन और अंगीकार के लिए G दबाएँ।"
            )
        }
        Locale::Ru => {
            format!(
                "{model_label} не смог подготовить вашу конституцию ({reason}). Ваш управляемый проект остаётся в силе — нажмите G для просмотра и ратификации."
            )
        }
        Locale::Uk => {
            format!(
                "{model_label} не зміг підготувати вашу конституцію ({reason}). Ваш керований проєкт залишається чинним — натисніть G для перегляду та ратифікації."
            )
        }
        _ => format!(
            "{model_label} could not draft your constitution ({reason}). Your guided draft still \
             stands — press G to preview and ratify."
        ),
    }
}

fn constitution_choice_label(choice: ConstitutionChoice) -> &'static str {
    match choice {
        ConstitutionChoice::Unset => "unset",
        ConstitutionChoice::Bundled => "bundled/default",
        ConstitutionChoice::GuidedCustom => "guided custom",
        ConstitutionChoice::ExpertOverride => "expert override",
        ConstitutionChoice::Deferred => "deferred",
    }
}

fn constitution_source_label(source: ConstitutionSource) -> &'static str {
    match source {
        ConstitutionSource::Bundled => "bundled",
        ConstitutionSource::UserGlobal => "user-global constitution.json",
        ConstitutionSource::ExpertOverride => "expert full Markdown override",
    }
}

fn constitution_validity_label(validity: ConstitutionValidity) -> &'static str {
    match validity {
        ConstitutionValidity::Unknown => "unknown",
        ConstitutionValidity::Valid => "valid",
        ConstitutionValidity::Invalid => "invalid",
        ConstitutionValidity::Empty => "empty",
        ConstitutionValidity::Unreadable => "unreadable",
    }
}

pub fn persist_user_constitution_choice(
    constitution: &UserConstitution,
    state: &SetupState,
) -> anyhow::Result<()> {
    let constitution_path = UserConstitution::path()?;
    let setup_state_path = SetupState::path()?;
    let mut transaction = codewhale_config::persistence::SetupTransaction::new();
    transaction.stage_json(constitution_path, &constitution.bounded())?;
    transaction.stage_json(setup_state_path, state)?;
    transaction.commit()
}

#[must_use]
pub fn should_open_update_checkpoint(app: &App, config: &Config) -> bool {
    let state = load_setup_state_for_app(app, config);
    state.needs_constitution_checkpoint(CONSTITUTION_CHECKPOINT_VERSION)
}

pub fn defer_update_checkpoint_for_app(app: &App, config: &Config) -> anyhow::Result<SetupState> {
    let mut state = load_setup_state_for_app(app, config);
    if !state.needs_constitution_checkpoint(CONSTITUTION_CHECKPOINT_VERSION) {
        return Ok(state);
    }
    state.complete_constitution_checkpoint(
        CONSTITUTION_CHECKPOINT_VERSION,
        ConstitutionChoice::Deferred,
    );
    state.constitution_source = ConstitutionSource::Bundled;
    state.constitution_validity = ConstitutionValidity::Unknown;
    state.constitution_authoring = None;
    state.constitution_preview_hash = None;
    state.set_step(
        SetupStep::Constitution,
        StepEntry::new(StepStatus::Deferred, true, CONSTITUTION_CHECKPOINT_VERSION)
            .with_result("checkpoint deferred; bundled applies"),
    );
    state.save()?;
    Ok(state)
}

#[must_use]
pub fn load_setup_state_for_app(app: &App, config: &Config) -> SetupState {
    if let Ok(Some(state)) = SetupState::load() {
        return state;
    }
    SetupState::derive_inherited(&inherited_facts_for_app(app, config))
}

pub(crate) fn record_provider_model_setup_state_for_app(
    app: &App,
    config: &Config,
) -> anyhow::Result<SetupState> {
    let facts = SetupRuntimeFacts::from_app_config(app, config);
    let mut state = load_setup_state_for_app(app, config);
    state.set_step(
        SetupStep::ProviderModel,
        provider::step_entry(
            facts.provider_status,
            CONSTITUTION_CHECKPOINT_VERSION,
            facts.provider_result,
        ),
    );
    state.save()?;
    Ok(state)
}

#[must_use]
fn inherited_facts_for_app(app: &App, config: &Config) -> InheritedConfigFacts {
    let user_constitution = UserConstitution::load().ok();
    let user_constitution_validity = user_constitution.as_ref().map_or(
        ConstitutionValidity::Unknown,
        UserConstitutionLoad::validity,
    );
    let has_user_constitution = user_constitution
        .as_ref()
        .is_some_and(|loaded| !matches!(loaded, UserConstitutionLoad::Missing));
    let expert_override = SetupExpertOverrideState::load();
    InheritedConfigFacts {
        language: Some(app.ui_locale.tag().to_string()),
        has_provider_route: !config.default_model().trim().is_empty(),
        has_credentials_or_local_runtime: has_api_key(config),
        trust_chosen: app.trust_mode || !codewhale_runtime::config::needs_trust(&app.workspace),
        has_expert_override: expert_override.is_active(),
        has_user_constitution,
        user_constitution_validity,
    }
}

fn expert_override_path() -> Option<std::path::PathBuf> {
    codewhale_config::codewhale_home()
        .ok()
        .map(|home| home.join(Path::new(CONSTITUTION_OVERRIDE_FILE)))
}

#[must_use]
fn progressive_initial_step_index(state: &SetupState, facts: &SetupRuntimeFacts) -> usize {
    if !facts.provider_ready {
        return step_index(SetupStep::ProviderModel);
    }
    let runtime_current = if state.inherited {
        matches!(state.status(SetupStep::TrustSandbox), StepStatus::Verified)
            && state.runtime_posture_source.is_reviewed()
    } else {
        state
            .steps
            .get(&SetupStep::TrustSandbox)
            .is_some_and(|entry| {
                entry.status == StepStatus::Verified
                    && entry.result.as_deref() == Some(facts.runtime_result.as_str())
            })
            && state.runtime_posture_source.is_reviewed()
    };
    if !runtime_current {
        return step_index(SetupStep::TrustSandbox);
    }
    if facts.tools_mcp_needs_action {
        return step_index(SetupStep::ToolsMcp);
    }
    step_index(SetupStep::Verification)
}

#[must_use]
fn step_index(step: SetupStep) -> usize {
    STEP_SPECS
        .iter()
        .position(|spec| spec.id() == step)
        .expect("all setup-state steps should have wizard specs")
}

fn visible_step_index(step: SetupStep) -> usize {
    STEP_SPECS
        .iter()
        .position(|spec| spec.id() == step)
        .unwrap_or_else(|| step_index(SetupStep::Constitution))
}

#[cfg(test)]
mod progressive_tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn facts(provider_ready: bool) -> SetupRuntimeFacts {
        SetupRuntimeFacts {
            provider: "local".to_string(),
            model: "stub-model".to_string(),
            auth: if provider_ready { "ready" } else { "missing" }.to_string(),
            provider_ready,
            provider_status: if provider_ready {
                StepStatus::Configured
            } else {
                StepStatus::NeedsAction
            },
            runtime_result: "approval=ask; sandbox=workspace; network=prompt".to_string(),
            tools_mcp_result: "mcp=off, skills=off, tools=off, plugins=off, overall=off"
                .to_string(),
            remote_control_result: tr(Locale::En, MessageId::SetupRemoteStatusDisabled)
                .into_owned(),
            ..SetupRuntimeFacts::default()
        }
    }

    fn complete_state(runtime_result: &str) -> SetupState {
        let mut state = SetupState {
            runtime_posture_source: RuntimePostureSource::Confirmed,
            ..SetupState::default()
        };
        state.set_step(
            SetupStep::TrustSandbox,
            StepEntry::new(StepStatus::Verified, true, CONSTITUTION_CHECKPOINT_VERSION)
                .with_result(runtime_result),
        );
        state
    }

    fn render_text(view: &SetupWizardView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        ModalView::render(view, area, &mut buffer);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn wizard_body_cells_carry_explicit_ink_on_the_dark_surface() {
        // The setup surface paints WHALE_BG while the blurb span carries no
        // fg of its own, so without the base paragraph style it inherits the
        // terminal default: black-on-black on light-profile terminals. Every
        // blurb cell must pin to the body ink; the explicitly styled title
        // must patch over the base unchanged.
        let view = SetupWizardView::new_with_facts(SetupState::default(), Locale::En, facts(false));
        let blurb_head: String = tr(Locale::En, MessageId::OnboardProviderBlurb)
            .chars()
            .take(16)
            .collect();
        let title_head: String = tr(Locale::En, MessageId::OnboardProviderTitle)
            .chars()
            .take(16)
            .collect();
        assert!(
            !blurb_head.is_empty() && !title_head.is_empty(),
            "test needs non-empty title and blurb heads to locate rows"
        );
        let area = Rect::new(0, 0, 100, 24);
        let mut buffer = Buffer::empty(area);
        ModalView::render(&view, area, &mut buffer);
        let mut blurb_hit = false;
        let mut title_hit = false;
        let mut checked = 0;
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
            let expected = if row.contains(blurb_head.as_str()) {
                blurb_hit = true;
                Some(palette::TEXT_PRIMARY)
            } else if row.contains(title_head.as_str()) {
                title_hit = true;
                Some(palette::WHALE_ACTION)
            } else {
                None
            };
            let Some(fg) = expected else {
                continue;
            };
            for x in 0..area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol().trim().is_empty() {
                    continue;
                }
                assert_eq!(
                    cell.style().fg,
                    Some(fg),
                    "setup body cell ({x}, {y}) must carry explicit ink"
                );
                checked += 1;
            }
        }
        assert!(
            blurb_hit && title_hit && checked > 0,
            "expected title and blurb rows in the rendered wizard"
        );
    }

    #[test]
    fn fresh_setup_starts_at_the_missing_provider_decision() {
        let view = SetupWizardView::new_with_facts(SetupState::default(), Locale::En, facts(false));

        assert_eq!(view.selected_step(), SetupStep::ProviderModel);
        assert!(matches!(
            ModalView::handle_key(
                &mut view.clone(),
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            ),
            ViewAction::EmitAndClose(ViewEvent::SetupOpenProviderRequested)
        ));
    }

    #[test]
    fn partial_setup_skips_the_ready_provider_and_asks_for_permissions() {
        let mut view =
            SetupWizardView::new_with_facts(SetupState::default(), Locale::En, facts(true));

        assert_eq!(view.selected_step(), SetupStep::TrustSandbox);
        let action =
            ModalView::handle_key(&mut view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let ViewAction::Emit(ViewEvent::SetupStateCommitRequested { state, message, .. }) = action
        else {
            panic!("reviewing the current permissions posture should persist a receipt");
        };
        assert_eq!(
            state.runtime_posture_source,
            RuntimePostureSource::Confirmed
        );
        assert_eq!(state.status(SetupStep::TrustSandbox), StepStatus::Verified);
        assert!(!message.is_empty());
        assert_eq!(view.selected_step(), SetupStep::RemoteRuntime);
    }

    #[test]
    fn fully_configured_setup_opens_the_compact_summary() {
        let facts = facts(true);
        let state = complete_state(&facts.runtime_result);
        let mut view = SetupWizardView::new_with_facts(state, Locale::En, facts);

        assert_eq!(view.selected_step(), SetupStep::Verification);
        let action =
            ModalView::handle_key(&mut view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let ViewAction::EmitAndClose(ViewEvent::SetupStateCommitRequested { state, .. }) = action
        else {
            panic!("starting from Ready should persist the report before closing");
        };
        assert!(state.steps.contains_key(&SetupStep::Verification));
    }

    #[test]
    fn stale_complete_receipts_reopen_the_real_broken_surface() {
        let mut provider_broken = facts(false);
        provider_broken.runtime_result = "current".to_string();
        let state = complete_state("old");
        assert_eq!(
            SetupWizardView::new_with_facts(state, Locale::En, provider_broken).selected_step(),
            SetupStep::ProviderModel
        );

        let runtime_current = facts(true);
        let stale_state = complete_state("old runtime snapshot");
        assert_eq!(
            SetupWizardView::new_with_facts(stale_state, Locale::En, runtime_current)
                .selected_step(),
            SetupStep::TrustSandbox
        );
    }

    #[test]
    fn configured_broken_tools_join_the_journey_but_empty_tools_do_not() {
        let mut broken = facts(true);
        let state = complete_state(&broken.runtime_result);
        broken.tools_mcp_needs_action = true;
        broken.tools_mcp_result = "overall=needs_config".to_string();
        let mut broken_view = SetupWizardView::new_with_facts(state.clone(), Locale::En, broken);
        assert_eq!(broken_view.selected_step(), SetupStep::ToolsMcp);
        let action = ModalView::handle_key(
            &mut broken_view,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        let ViewAction::Emit(ViewEvent::SetupStateCommitRequested { state, .. }) = action else {
            panic!("continuing past a broken optional tool should save its visible issue");
        };
        assert_eq!(state.status(SetupStep::ToolsMcp), StepStatus::NeedsAction);
        assert_eq!(broken_view.selected_step(), SetupStep::Verification);

        let empty = facts(true);
        assert_eq!(
            SetupWizardView::new_with_facts(state, Locale::En, empty).selected_step(),
            SetupStep::Verification
        );
    }

    #[test]
    fn account_action_uses_the_real_remote_control_handoff() {
        let facts = facts(true);
        let state = complete_state(&facts.runtime_result);
        let mut view = SetupWizardView::new_with_facts(state, Locale::En, facts);
        view.selected = visible_step_index(SetupStep::RemoteRuntime);

        assert!(matches!(
            ModalView::handle_key(
                &mut view,
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)
            ),
            ViewAction::EmitAndClose(ViewEvent::SetupOpenRemoteControlRequested)
        ));
    }

    #[test]
    fn progressive_arrow_keys_scroll_details_without_changing_the_decision() {
        let mut view =
            SetupWizardView::new_with_facts(SetupState::default(), Locale::En, facts(false));
        view.details_expanded = true;
        view.body_scroll = 4;
        let step = view.selected_step();

        assert!(matches!(
            ModalView::handle_key(&mut view, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            ViewAction::None
        ));
        assert_eq!(view.selected_step(), step);
        assert_eq!(view.body_scroll, 3);

        assert!(matches!(
            ModalView::handle_key(&mut view, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            ViewAction::None
        ));
        assert_eq!(view.selected_step(), step);
        assert_eq!(view.body_scroll, 4);
    }

    #[test]
    fn fresh_and_complete_guides_remain_reachable_at_compact_and_normal_sizes() {
        let fresh =
            SetupWizardView::new_with_facts(SetupState::default(), Locale::En, facts(false));
        let fresh_text = render_text(&fresh, 40, 12);
        assert!(fresh_text.contains(tr(Locale::En, MessageId::OnboardProviderTitle).as_ref()));
        let normal_text = render_text(&fresh, 100, 28);
        assert!(normal_text.contains(tr(Locale::En, MessageId::OnboardProviderTitle).as_ref()));
        assert!(normal_text.contains("local · stub-model"));

        let facts = facts(true);
        let complete = SetupWizardView::new_with_facts(
            complete_state(&facts.runtime_result),
            Locale::En,
            facts,
        );
        let complete_text = render_text(&complete, 40, 12);
        assert!(complete_text.contains(tr(Locale::En, MessageId::OnboardReadyTitle).as_ref()));
    }
}
