//! Legacy-profile setup — a progressive "set up your agent team" flow.
//!
//! `/fleet setup` routes here only when no named v2 Fleet is selected. When a
//! v2 Fleet is selected, the host opens that Fleet's exact detail editor so a
//! save can never appear to update a member while writing an ignored legacy
//! `.codewhale/agents/*.toml` profile.
//!
//! Replaces the old six-column config matrix (#3791). Fleet is presented as an
//! agent team: the shortest valid path remains role → provider/model →
//! save/apply. From the Model step, `c` opens an optional, pure composition
//! advisory built only from configured routes; accept/edit/reject all return to
//! this same human-reviewed save path.
//! The review step shows resolved provider, model, auth/readiness, profile
//! availability, and overwrite consequences once before anything is written. Thinking defaults to
//! inherit and can be adjusted on the review step without an extra wizard
//! screen. "Save profile" persists the exact rendered TOML bytes.
//!
//! NOTE (audit #7 / #3167): the role/model taxonomy and copy below are
//! intentionally English for now; #3167 reworks this into an interactive
//! provider/model picker that will churn most of this text. The command entry
//! (`CmdFleetDescription`) is already localized.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use codewhale_workflow::fleet_composition::{
    CompositionError, CompositionRole, ConfiguredModel, FleetCompositionProposal,
    FleetCompositionRequest, RatificationState, RoleSuggestion,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget, Wrap},
};

use crate::tui::app::App;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, centered_modal_area,
    render_modal_footer_with_gutter, render_modal_surface, truncate_view_text,
};
use codewhale_agent_contract::role::public_role_label;
use codewhale_engine::fleet::profile::FleetProfileScope;
use codewhale_localization::{MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::Config;
use codewhale_tui_kit::menu_style;

const PROFILE_DIR: &str = ".codewhale/agents";

/// Rows one PageUp/PageDown travels on choice steps. Pages clamp at the ends
/// per the shared vocabulary instead of wrapping (#6290).
const SETUP_PAGE: usize = 10;
/// Lines one PageUp/PageDown scrolls on the Review step (unchanged).
const REVIEW_SCROLL_PAGE: usize = 8;

/// The only two truthful destinations for `/fleet setup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FleetSetupEditTarget {
    /// No named v2 Fleet is selected, so the legacy profile wizard remains
    /// the effective roster-authoring surface.
    LegacyProfiles,
    /// A named v2 Fleet is selected; edit that exact file and scope.
    SelectedFleet {
        name: String,
        scope: codewhale_engine::fleet::store::FleetScope,
    },
}

/// Resolve setup independently of project-profile trust. A broken explicit
/// selection fails closed instead of being mistaken for "no Fleet" and
/// silently opening the legacy profile writer.
pub(crate) fn resolve_fleet_setup_edit_target(
    workspace: &Path,
) -> Result<FleetSetupEditTarget, String> {
    match codewhale_engine::fleet::store::resolve_selected_fleet(workspace) {
        Ok(Some(selected)) => Ok(FleetSetupEditTarget::SelectedFleet {
            name: selected.name,
            scope: selected.scope,
        }),
        Ok(None) => Ok(FleetSetupEditTarget::LegacyProfiles),
        Err(_) => Err(
            "Selected Fleet is missing or unreadable; open /fleet teams to repair or clear the selection. Legacy profiles were not opened."
                .to_string(),
        ),
    }
}

/// A selectable choice in a wizard step: a short identifier `label`, a one-line
/// `summary`, and a longer `description` shown (wrapped) in the detail pane.
#[derive(Clone)]
struct Choice {
    label: Cow<'static, str>,
    summary: Cow<'static, str>,
    description: Cow<'static, str>,
}

const CHOICE_LIST_WIDTH: u16 = 22;
const CHOICE_DETAIL_MIN_WIDTH: u16 = 58;
const CHOICE_TWO_COLUMN_MIN_WIDTH: u16 = CHOICE_LIST_WIDTH + CHOICE_DETAIL_MIN_WIDTH;

/// Agent-team roles. `label` doubles as the profile `role_hint` and file stem,
/// so these strings are part of the generated-profile contract.
const ROLES: [Choice; 9] = [
    Choice {
        label: Cow::Borrowed("manager"),
        summary: Cow::Borrowed("Plan & split queued work"),
        description: Cow::Borrowed(
            "Coordinates the Fleet run: plans the work, splits it into bounded tasks, and dispatches agents.",
        ),
    },
    Choice {
        label: Cow::Borrowed("explore"),
        summary: Cow::Borrowed("Read-first research"),
        description: Cow::Borrowed(
            "Research and evidence gathering. Reads and summarizes before anything is written.",
        ),
    },
    Choice {
        label: Cow::Borrowed("implement"),
        summary: Cow::Borrowed("Implements bounded changes"),
        description: Cow::Borrowed(
            "Implements changes strictly inside its assigned task scope; writes only what the slice needs.",
        ),
    },
    Choice {
        label: Cow::Borrowed("reviewer"),
        summary: Cow::Borrowed("Read-only review"),
        description: Cow::Borrowed(
            "Checks regressions, tests, and diffs. Read-only — it never writes.",
        ),
    },
    Choice {
        label: Cow::Borrowed("test"),
        summary: Cow::Borrowed("Bounded validation"),
        description: Cow::Borrowed(
            "Runs bounded validation (test/check selections) and reports receipts back to the orchestrator. Never writes — patching is denied; unbounded shell forms are refused.",
        ),
    },
    Choice {
        label: Cow::Borrowed("advisor"),
        summary: Cow::Borrowed("Read-only second opinion"),
        description: Cow::Borrowed(
            "Short-lived, high-reasoning counsel for difficult decisions and overlooked risks. Read-only and shell-less.",
        ),
    },
    Choice {
        label: Cow::Borrowed("synthesizer"),
        summary: Cow::Borrowed("Reduce receipts to handoff"),
        description: Cow::Borrowed(
            "Turns agent receipts into bounded handoff state instead of raw transcript replay.",
        ),
    },
    Choice {
        label: Cow::Borrowed("general"),
        summary: Cow::Borrowed("General-purpose agent"),
        description: Cow::Borrowed(
            "A flexible agent with no specialized focus — use it when the task doesn't fit a named role.",
        ),
    },
    Choice {
        label: Cow::Borrowed("custom"),
        summary: Cow::Borrowed("Author a profile by hand"),
        description: Cow::Borrowed(
            "Define the role yourself in a workspace agent TOML profile under .codewhale/agents/.",
        ),
    },
];

/// The `inherit` row shown first in the Model step (#3167). Concrete provider
/// models follow it, built per-run from EVERY configured provider's catalog
/// (#4093), so the user picks a real route — including cross-provider ones —
/// instead of an abstract class or only the active provider's models.
const MODEL_INHERIT: Choice = Choice {
    label: Cow::Borrowed("same as session"),
    summary: Cow::Borrowed("Same model as now"),
    description: Cow::Borrowed(
        "Use your current model — provider and reasoning included. Recommended default.",
    ),
};

const THINKING_CHOICES: &[Choice] = &[
    Choice {
        label: Cow::Borrowed("inherit"),
        summary: Cow::Borrowed("Same thinking as now"),
        description: Cow::Borrowed(
            "Reuse the Coordinator's current Thinking setting for this agent. Recommended default.",
        ),
    },
    Choice {
        label: Cow::Borrowed("off"),
        summary: Cow::Borrowed("No extra thinking"),
        description: Cow::Borrowed(
            "Use for narrow lookups or mechanical work where speed matters.",
        ),
    },
    Choice {
        label: Cow::Borrowed("low"),
        summary: Cow::Borrowed("Small thinking budget"),
        description: Cow::Borrowed(
            "Use for bounded checks that still benefit from light reasoning.",
        ),
    },
    Choice {
        label: Cow::Borrowed("medium"),
        summary: Cow::Borrowed("Balanced thinking budget"),
        description: Cow::Borrowed("Use for normal implementation and review work."),
    },
    Choice {
        label: Cow::Borrowed("high"),
        summary: Cow::Borrowed("Deep thinking budget"),
        description: Cow::Borrowed("Use for harder design, debugging, and integration tasks."),
    },
    Choice {
        label: Cow::Borrowed("max"),
        summary: Cow::Borrowed("Maximum thinking budget"),
        description: Cow::Borrowed("Use for hard release, security, and root-cause work."),
    },
    Choice {
        label: Cow::Borrowed("auto"),
        summary: Cow::Borrowed("Let Codewhale choose"),
        description: Cow::Borrowed("Choose a Thinking level from the agent prompt at runtime."),
    },
];

#[derive(Debug, Clone)]
pub struct FleetSetupSnapshot {
    workspace: PathBuf,
    locale: codewhale_localization::Locale,
    /// Whether the active provider has a key or local runtime — gates the
    /// model-draft offer, mirroring the constitution card's `provider_ready`.
    provider_ready: bool,
    provider: String,
    model: String,
    reasoning: String,
    subagents_enabled: bool,
    max_subagents: usize,
    launch_concurrency: usize,
    max_admitted: usize,
    subagent_spawn_depth: u32,
    fleet_spawn_depth: u32,
    api_timeout_secs: u64,
    heartbeat_timeout_secs: u64,
    /// Lowercased roster member ids with their origin labels (built-in /
    /// config / project), so the wizard can say when a chosen role would
    /// override an existing roster member.
    roster_members: Vec<(String, String)>,
    /// Saved (file-backed) roster members keyed by lowercased id: where the
    /// file lives and the route it pins, so reopening a saved profile from
    /// `/fleet` starts from what is on disk instead of the wizard defaults.
    roster_details: Vec<RosterMemberDetail>,
    /// Whether project-scope profiles are enabled for this launch
    /// (`--no-project-config` disables them). When false, "This project" is
    /// offered disabled with that reason instead of writing a file nothing
    /// will load.
    project_profiles_enabled: bool,
    /// Resolved personal profile directory (`$CODEWHALE_HOME/agents`), or the
    /// reason it could not be resolved. Captured once at snapshot time so the
    /// wizard never re-reads the environment while painting and tests can
    /// point it at a temp dir.
    personal_profile_dir: Result<PathBuf, String>,
    /// `(exact provider id, model id, readiness label, selectable)` routes for a worker,
    /// drawn from ALL configured providers — not only the active one (#4093).
    /// Shown after `inherit` in the Model step so a Fleet worker can be pinned
    /// to a route independent of the parent/current provider. The provider id
    /// is a canonical built-in id or the exact named custom table key, not a
    /// display label — see [`cross_provider_model_routes`].
    available_models: Vec<(
        String,
        String,
        codewhale_engine::provider_readiness::ResolvedProviderReadiness,
    )>,
}

/// A file-backed roster member as it exists on disk (project or personal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterMemberDetail {
    id: String,
    scope: FleetProfileScope,
    source: PathBuf,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
}

impl FleetSetupSnapshot {
    #[must_use]
    pub fn from_app(app: &App, config: &Config) -> Self {
        let provider = app.effective_route_identity_display().0;
        let model = if app.auto_model {
            app.last_effective_model
                .as_deref()
                .map(|effective| format!("auto -> {effective}"))
                .unwrap_or_else(|| "auto".to_string())
        } else {
            app.model.clone()
        };
        let fleet_spawn_depth = config
            .fleet
            .as_ref()
            .map(|fleet| fleet.exec.max_spawn_depth)
            .unwrap_or_else(|| codewhale_config::FleetExecConfig::default().max_spawn_depth);
        let roster = codewhale_engine::fleet::roster::FleetRoster::load(
            &config.fleet_config(),
            &app.workspace,
        );
        let roster_members = roster
            .members()
            .iter()
            .map(|member| (member.id.to_lowercase(), member.origin.to_string()))
            .collect();
        let roster_details = roster
            .members()
            .iter()
            .filter_map(|member| {
                let scope = match member.origin {
                    codewhale_engine::fleet::roster::ProfileOrigin::Workspace => {
                        FleetProfileScope::Project
                    }
                    codewhale_engine::fleet::roster::ProfileOrigin::Personal => {
                        FleetProfileScope::Personal
                    }
                    _ => return None,
                };
                Some(RosterMemberDetail {
                    id: member.id.to_lowercase(),
                    scope,
                    source: member.source.clone(),
                    provider: member.profile.provider.clone(),
                    model: member.profile.model.clone(),
                    reasoning_effort: member.profile.reasoning_effort.clone(),
                })
            })
            .collect();
        let identity = app
            .provider_identity
            .as_ref()
            .filter(|identity| config.verify_provider_identity(identity).is_ok());
        let provider_ready = identity.is_some_and(|identity| {
            codewhale_engine::provider_readiness::resolve_for_model(
                config,
                identity,
                if app.auto_model { "auto" } else { &app.model },
                &app.provider_health,
            )
            .can_attempt()
        });

        Self {
            workspace: app.workspace.clone(),
            locale: app.ui_locale,
            provider_ready,
            provider,
            model,
            reasoning: app.reasoning_effort_display_label(),
            subagents_enabled: identity.map_or_else(
                || config.subagents_enabled(),
                |identity| config.subagents_enabled_for_provider(identity),
            ),
            max_subagents: identity.map_or_else(
                || config.max_subagents(),
                |identity| config.max_subagents_for_provider(identity),
            ),
            launch_concurrency: identity.map_or_else(
                || config.launch_concurrency(),
                |identity| config.launch_concurrency_for_provider(identity),
            ),
            max_admitted: identity.map_or_else(
                || config.max_admitted_subagents(),
                |identity| config.max_admitted_subagents_for_provider(identity),
            ),
            subagent_spawn_depth: identity.map_or_else(
                || config.subagent_max_spawn_depth(),
                |identity| config.subagent_max_spawn_depth_for_provider(identity),
            ),
            fleet_spawn_depth,
            api_timeout_secs: identity.map_or_else(
                || config.subagent_api_timeout_secs(),
                |identity| config.subagent_api_timeout_secs_for_provider(identity),
            ),
            heartbeat_timeout_secs: identity.map_or_else(
                || config.subagent_heartbeat_timeout_secs(),
                |identity| config.subagent_heartbeat_timeout_secs_for_provider(identity),
            ),
            roster_members,
            roster_details,
            project_profiles_enabled:
                codewhale_engine::fleet::roster::project_agent_profiles_enabled(),
            personal_profile_dir: codewhale_engine::fleet::profile::personal_agent_profile_dir()
                .map_err(|err| format!("{err:#}")),
            available_models: cross_provider_model_routes(config, identity, &app.provider_health),
        }
    }
}

/// Build the `(canonical provider id, model id)` pairs selectable for a worker
/// from EVERY configured provider — not only the active one (#4093). Fleet
/// workers can be pinned to a route independent of the parent/current provider,
/// so the Model step must offer the same cross-provider catalog the model
/// picker does, instead of the active provider's models alone.
///
/// The provider id here is the exact non-secret configured route key. Built-ins
/// use their canonical id; named custom routes keep their table key so saved
/// Fleet profiles can rebuild the same child client.
/// Callers derive a human-readable label from it for UI text.
pub(crate) fn cross_provider_model_routes(
    config: &Config,
    active: Option<&codewhale_runtime::config::ProviderIdentity>,
    health: &codewhale_engine::provider_readiness::ProviderReadinessSnapshot,
) -> Vec<(
    String,
    String,
    codewhale_engine::provider_readiness::ResolvedProviderReadiness,
)> {
    let Some(active) = active.filter(|identity| config.verify_provider_identity(identity).is_ok())
    else {
        return Vec::new();
    };
    let mut routes = Vec::new();
    for identity in config.provider_identities() {
        if identity.provider == codewhale_config::ProviderKind::Antigravity
            || !codewhale_engine::provider_readiness::provider_is_configured_for_active(
                config, &identity, active,
            )
        {
            continue;
        }
        append_provider_model_routes(&mut routes, config, active, &identity, health);
    }
    routes
}

fn append_provider_model_routes(
    routes: &mut Vec<(
        String,
        String,
        codewhale_engine::provider_readiness::ResolvedProviderReadiness,
    )>,
    config: &Config,
    active: &codewhale_runtime::config::ProviderIdentity,
    identity: &codewhale_runtime::config::ProviderIdentity,
    health: &codewhale_engine::provider_readiness::ProviderReadinessSnapshot,
) {
    let mut models = Vec::new();
    if let Some(model) = config
        .provider_config_for(identity)
        .and_then(|entry| entry.model.as_deref())
    {
        push_unique_model(&mut models, model);
    }
    if identity == active {
        let model = config.default_model();
        if !model.trim().eq_ignore_ascii_case("auto") {
            push_unique_model(&mut models, &model);
        }
    }
    for model in codewhale_engine::provider_lake::models_for_provider(config, identity) {
        push_unique_model(&mut models, &model);
    }
    for model in models {
        let readiness = codewhale_engine::provider_readiness::resolve_for_model(
            config, identity, &model, health,
        );
        routes.push((identity.key.to_string(), model, readiness));
    }
}

fn push_unique_model(models: &mut Vec<String>, model: &str) {
    let model = model.trim();
    if !model.is_empty() && !models.iter().any(|existing| existing == model) {
        models.push(model.to_string());
    }
}

/// Human-readable label for a built-in provider id, falling back to an exact
/// named custom id verbatim.
/// Does this provider/model route match a typed filter?
///
/// Substring over the model id, the provider id, and the provider's display
/// label, because a person types "sonnet", "anthropic", or "Claude" and means
/// the same row. The inherit row also answers to the words describing it.
/// Shared so the setup wizard and the Fleet editor cannot disagree about what
/// a query means — the editor had no filter at all, which made picking one
/// model out of every configured route an arrow-key errand.
pub(super) fn route_matches_query(
    query: &str,
    provider: &str,
    model: &str,
    is_inherit_row: bool,
) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    model.to_ascii_lowercase().contains(&query)
        || provider.to_ascii_lowercase().contains(&query)
        || provider_display_label(provider)
            .to_ascii_lowercase()
            .contains(&query)
        || (is_inherit_row && "inherit same as session current".contains(&query))
}

pub(super) fn provider_display_label(provider_id: &str) -> String {
    codewhale_config::ProviderKind::parse(provider_id)
        .filter(|provider| provider.as_str() == provider_id)
        .map(|provider| provider.provider().display_name().to_string())
        .unwrap_or_else(|| provider_id.to_string())
}

/// Which focused screen of the wizard is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Pick the team role.
    Role,
    /// Review an inert role-to-model suggestion built from configured routes.
    Composition,
    /// Pick the model-routing class.
    Model,
    /// Choose where the profile is saved (this project or personal).
    Destination,
    /// Review the full posture and save.
    Review,
}

/// The two save destinations, in the order the Destination step lists them.
const DESTINATION_ORDER: [FleetProfileScope; 2] =
    [FleetProfileScope::Project, FleetProfileScope::Personal];

/// Resolved facts about one save destination, computed off the paint path
/// (on entering the Destination/Review steps and when the role changes).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DestinationStatus {
    scope: FleetProfileScope,
    /// `None` when the destination can be written; otherwise the localized
    /// reason it is offered disabled.
    unavailable_reason: Option<String>,
    /// Exact file that saving would write.
    target: PathBuf,
    /// Whether `target` already exists (saving would replace it).
    target_exists: bool,
}

/// Which control on the Review step owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFocus {
    Save,
    ChangeDestination,
    Back,
}

impl ReviewFocus {
    const ORDER: [Self; 3] = [Self::Save, Self::ChangeDestination, Self::Back];

    fn next(self) -> Self {
        let idx = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(idx + 1) % Self::ORDER.len()]
    }

    fn prev(self) -> Self {
        let idx = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(idx + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }
}

/// The workflow-owned request and its validated, deliberately unratified
/// proposal. Keeping the request beside the proposal lets the UI re-run the
/// workflow validator at the exact point where a human accepts a suggestion.
#[derive(Debug, Clone)]
struct CompositionAdvisory {
    request: FleetCompositionRequest,
    proposal: FleetCompositionProposal,
}

impl CompositionAdvisory {
    fn validated_route_for_role(
        &self,
        role: &str,
    ) -> Result<Option<(String, String)>, CompositionError> {
        let proposal =
            FleetCompositionProposal::validate(&self.request, self.proposal.suggestions.clone())?;
        Ok(proposal
            .suggestions
            .iter()
            .find(|suggestion| suggestion.role.eq_ignore_ascii_case(role))
            .map(|suggestion| (suggestion.provider.clone(), suggestion.model.clone())))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompositionDecision {
    Pending,
    Accepted,
    Edited,
    Rejected,
}

/// Per-row Fleet Model step interaction state.
///
/// Replaces the old `model_selectable: Vec<bool>` so a dormant external-consent
/// route can require explicit activation (#v092-fleet-routes-fix) while
/// genuinely unconfigured routes stay blocked with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FleetModelRowState {
    Ready,
    NeedsActivation,
    Blocked { reason: String },
}

impl FleetModelRowState {
    fn from_readiness(
        readiness: &codewhale_engine::provider_readiness::ResolvedProviderReadiness,
    ) -> Self {
        if readiness.requires_explicit_activation() {
            return Self::NeedsActivation;
        }
        if let Some(reason) = readiness.blocked_reason() {
            return Self::Blocked {
                reason: reason.into_owned(),
            };
        }
        if readiness.can_attempt() {
            return Self::Ready;
        }
        Self::Blocked {
            reason: readiness
                .blocked_reason()
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_else(|| readiness.label().into_owned()),
        }
    }
}

/// Build the setup-time advisory from routes the wizard already resolved from
/// the operator's configured providers. The adapter is intentionally pure: it
/// sorts and de-duplicates the redacted provider/model pairs, assigns them to
/// the built-in roles in stable round-robin order, then asks the workflow
/// schema to validate every assignment against that exact pool.
fn deterministic_composition_advisory(
    available_models: &[(
        String,
        String,
        codewhale_engine::provider_readiness::ResolvedProviderReadiness,
    )],
) -> Option<CompositionAdvisory> {
    let mut seen = BTreeSet::new();
    let mut pool: Vec<ConfiguredModel> = available_models
        .iter()
        // Do not recommend a route the Model step would refuse or require the
        // operator to activate first. Such rows remain available for explicit
        // human selection in the existing picker.
        .filter(|(_, _, readiness)| {
            FleetModelRowState::from_readiness(readiness) == FleetModelRowState::Ready
        })
        .filter_map(|(provider, model, _)| {
            let key = (provider.clone(), model.clone());
            seen.insert(key.clone())
                .then(|| ConfiguredModel::new(key.0, key.1, None))
        })
        .collect();
    pool.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.model.cmp(&right.model))
    });

    let roles: Vec<CompositionRole> = ROLES
        .iter()
        // `custom` is an invitation to author a posture, not a semantic Fleet
        // role, so it stays on the manual Model path.
        .filter(|role| role.label != "custom")
        .map(|role| CompositionRole::new(role.label.to_string(), Some(&role.summary)))
        .collect();
    let request = FleetCompositionRequest::new(pool, roles).ok()?;
    let suggestions = request
        .roles
        .iter()
        .enumerate()
        .map(|(idx, role)| {
            let configured = &request.pool[idx % request.pool.len()];
            RoleSuggestion {
                role: role.role.clone(),
                provider: configured.provider.clone(),
                model: configured.model.clone(),
                reason: Some(
                    "Stable round-robin assignment from the configured model pool.".to_string(),
                ),
            }
        })
        .collect();
    let proposal = FleetCompositionProposal::validate(&request, suggestions).ok()?;
    Some(CompositionAdvisory { request, proposal })
}

/// A role assignment edits only route keys in the original document. The
/// source and possible destinations are captured before opening the picker.
struct RouteAssignment {
    editor_id: uuid::Uuid,
    id: String,
    template: toml::Table,
    original_member: codewhale_engine::fleet::profile::AgentProfile,
    source: Option<(PathBuf, String)>,
    source_scope: Option<FleetProfileScope>,
    destinations: Vec<(PathBuf, Option<String>)>,
    provider: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
}

/// The wizard answers a model draft was requested against (U09-03).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelDraftRequest {
    role: String,
    route: Option<(String, String)>,
    reasoning_effort: Option<String>,
}

fn assignment_source(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("Cannot read {}: {err}", path.display())),
    }
}

pub struct FleetSetupView {
    snapshot: FleetSetupSnapshot,
    assignment: Option<RouteAssignment>,
    step: Step,
    role_idx: usize,
    model_idx: usize,
    thinking_idx: usize,
    profile_scope: FleetProfileScope,
    /// Whether the user has explicitly chosen (or a saved profile supplied) the
    /// save destination. Until then the header says the choice is still ahead
    /// instead of silently presenting a default as a decision.
    scope_decided: bool,
    /// Highlighted row on the Destination step (index into DESTINATION_ORDER).
    destination_idx: usize,
    /// Resolved destination facts for both scopes. Recomputed on entry to the
    /// Destination/Review steps and when the role (file name) changes; the
    /// draw path never touches the filesystem (#3908).
    destinations: Option<[DestinationStatus; 2]>,
    /// Focused control on the Review step (Tab/Shift-Tab/←/→ move it).
    review_focus: ReviewFocus,
    /// Replacing an existing file needs a second Enter on the save control.
    replace_armed: bool,
    /// One-line inline notice (e.g. why a model row cannot be selected).
    /// Cleared on the next navigation key.
    notice: Option<String>,
    review_scroll: usize,
    /// A model-drafted profile awaiting save (already sanitized and
    /// bounded by the untrusted gate). Cleared when the selection changes so
    /// a stale draft can never be saved against fresh answers.
    model_draft: Option<Box<codewhale_engine::fleet::profile::FleetProfileDraft>>,
    /// Exact rendered TOML preview for `model_draft` (header comment + the
    /// deterministic bytes saving would persist). Rendered inline on the
    /// Review step — never in a separate pager (#4093): a standalone pager
    /// view owns its own `g`/`G` scroll bindings, which silently swallowed
    /// the save keypress and left users unable to save without first
    /// pressing Esc. Keeping the preview and the save control in the same
    /// view means the footer's `g`/Enter hints are never a lie.
    model_draft_preview: Option<String>,
    /// The answers the in-flight model draft (`m` on Review) was requested
    /// against. Cleared with the draft whenever an answer changes, so a
    /// draft that lands late is refused instead of being installed onto
    /// answers it was never written for.
    model_draft_request: Option<ModelDraftRequest>,
    /// Model-step rows: `inherit` followed by one row per concrete model from
    /// every configured provider (#4093).
    model_choices: Vec<Choice>,
    /// `(provider, model)` aligned with `model_choices`. Index 0 is `inherit`
    /// (the active route); later rows pin a concrete, possibly cross-provider
    /// route. Drives the review/copy so a pinned route names its own provider.
    model_routes: Vec<(String, String)>,
    /// Interaction state for each aligned Model row. Distinguishes ready rows,
    /// dormant external-consent rows that need explicit activation, and
    /// genuinely blocked rows with a short reason.
    model_row_states: Vec<FleetModelRowState>,
    /// Typed filter for the Model step (#4639): substring match over
    /// provider and model id, so provider-heavy catalogs (e.g. OpenRouter)
    /// stay navigable without a provider→model drill-down.
    model_query: String,
    /// Whether the Model step's filter input is capturing keystrokes (`/`
    /// toggles it; Enter keeps the filter, Esc clears it).
    model_filter_active: bool,
    /// Pure workflow-schema proposal shown before the Model picker. It has no
    /// save, spawn, launch, or snapshot capability.
    composition: Option<CompositionAdvisory>,
    composition_decision: CompositionDecision,
    /// Selectable rows registered by the latest render. Keeping mouse geometry
    /// in the view gives the Fleet walkthrough the same row ownership as its
    /// keyboard path without coupling the host to this modal's layout.
    row_hitboxes: RefCell<Vec<(Rect, usize)>>,
}

impl FleetSetupView {
    /// Refresh row states from a freshly built snapshot while preserving the
    /// user's current selection position and draft state. Used after the host
    /// validates a dormant external-consent route so the same row becomes
    /// Ready without closing and reopening the modal.
    pub fn refresh_from_snapshot(&mut self, snapshot: FleetSetupSnapshot) {
        let old_step = self.step;
        let old_role_idx = self.role_idx;
        let old_model_idx = self.model_idx;
        let old_thinking_idx = self.thinking_idx;
        let old_profile_scope = self.profile_scope;
        let old_scope_decided = self.scope_decided;
        let old_destination_idx = self.destination_idx;
        let old_review_focus = self.review_focus;
        let old_model_query = self.model_query.clone();
        let old_model_filter_active = self.model_filter_active;
        let old_review_scroll = self.review_scroll;
        let old_model_draft = self.model_draft.clone();
        let old_model_draft_preview = self.model_draft_preview.clone();
        let old_model_draft_request = self.model_draft_request.clone();

        *self = Self::from_snapshot(snapshot);

        self.step = old_step;
        self.role_idx = old_role_idx;
        self.model_idx = old_model_idx.min(self.filtered_model_indices().len().saturating_sub(1));
        self.thinking_idx = old_thinking_idx;
        self.profile_scope = old_profile_scope;
        self.scope_decided = old_scope_decided;
        self.destination_idx = old_destination_idx;
        self.review_focus = old_review_focus;
        self.model_query = old_model_query;
        self.model_filter_active = old_model_filter_active;
        self.review_scroll = old_review_scroll;
        self.model_draft = old_model_draft;
        self.model_draft_preview = old_model_draft_preview;
        self.model_draft_request = old_model_draft_request;
        if self.step == Step::Composition && !self.has_composition_for_selected_role() {
            self.step = Step::Model;
        }
        if matches!(self.step, Step::Destination | Step::Review) {
            self.refresh_destinations();
        }
    }

    #[must_use]
    pub fn new(app: &App, config: &Config) -> Self {
        Self::from_snapshot(FleetSetupSnapshot::from_app(app, config))
    }

    /// Open setup for a role the operator already selected in `/fleet`.
    /// Unknown/custom roster roles map to the explicit custom authoring row;
    /// Left or Esc still exposes Role so the carried choice is never sticky.
    #[must_use]
    pub fn new_for_role(app: &App, config: &Config, role: &str) -> Self {
        Self::from_snapshot_for_role(FleetSetupSnapshot::from_app(app, config), role)
    }

    pub(crate) fn new_for_route_assignment(
        app: &App,
        config: &Config,
        id: &str,
    ) -> Result<Self, String> {
        let roster = codewhale_engine::fleet::identity::load_effective_roster(
            &config.fleet_config(),
            &app.workspace,
            Some(app.plugin_registry.as_ref()),
        );
        let member = roster
            .members()
            .iter()
            .find(|member| member.id == id)
            .ok_or_else(|| "This role is no longer available. Reopen Fleet.".to_string())?;
        if matches!(
            member.origin,
            codewhale_engine::fleet::roster::ProfileOrigin::Plugin
                | codewhale_engine::fleet::roster::ProfileOrigin::Config
        ) {
            return Err(format!(
                "{} is managed by {} ({}). Change its model there; copying it into a profile would discard its original controls.",
                member.id,
                member.origin,
                member.source.display()
            ));
        }
        if member.origin == codewhale_engine::fleet::roster::ProfileOrigin::BuiltIn
            && (member.profile.permissions != Default::default()
                || member.profile.delegation != Default::default())
        {
            return Err("This role has controls that cannot be copied into a profile. Edit its defining configuration.".into());
        }
        let mut view = Self::new_for_role(app, config, id);
        let source_scope = match member.origin {
            codewhale_engine::fleet::roster::ProfileOrigin::Workspace => {
                Some(FleetProfileScope::Project)
            }
            codewhale_engine::fleet::roster::ProfileOrigin::Personal => {
                Some(FleetProfileScope::Personal)
            }
            _ => None,
        };
        let source = if source_scope.is_some() {
            Some((
                member.source.clone(),
                assignment_source(&member.source)?
                    .ok_or_else(|| "The saved role disappeared. Reopen Fleet.".to_string())?,
            ))
        } else {
            None
        };
        let template = if let Some((_, text)) = &source {
            toml::from_str::<toml::Table>(text).map_err(|err| err.to_string())?
        } else {
            let draft = codewhale_engine::fleet::profile::FleetProfileDraft {
                id: member.id.clone(),
                display_name: member.display_name.clone(),
                description: member.description.clone(),
                role_hint: member.profile.role.name.clone(),
                model_class_hint: Some(member.profile.loadout.as_str().to_string()),
                model: member.profile.model.clone(),
                provider: member.profile.provider.clone(),
                reasoning_effort: member.profile.reasoning_effort.clone(),
                instructions: member.profile.role.instructions.clone(),
            };
            toml::from_str::<toml::Table>(&draft.render_toml()).map_err(|err| err.to_string())?
        };
        view.assignment = Some(RouteAssignment {
            editor_id: uuid::Uuid::new_v4(),
            id: member.id.clone(),
            template,
            original_member: member.clone(),
            source,
            source_scope,
            destinations: Vec::new(),
            provider: member.profile.provider.clone(),
            model: member.profile.model.clone(),
            reasoning: member.profile.reasoning_effort.clone(),
        });
        view.refresh_destinations();
        let targets = view
            .destinations
            .as_ref()
            .into_iter()
            .flatten()
            .filter(|destination| destination.unavailable_reason.is_none())
            .map(|destination| {
                Ok((
                    destination.target.clone(),
                    assignment_source(&destination.target)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        view.assignment.as_mut().unwrap().destinations = targets;
        view.step = Step::Destination;
        Ok(view)
    }

    pub(crate) fn route_pick_request(&self) -> ViewAction {
        self.assignment
            .as_ref()
            .map_or(ViewAction::None, |assignment| {
                ViewAction::Emit(ViewEvent::FleetProfileRoutePickRequested {
                    editor_id: assignment.editor_id,
                })
            })
    }

    pub(crate) fn assignment_context(&self) -> (String, String) {
        (self.selected_role(), self.saves_to_line())
    }

    pub(crate) fn route_selection(
        &self,
        editor_id: uuid::Uuid,
    ) -> Option<super::fleet_detail::FleetRouteSelection> {
        let assignment = self
            .assignment
            .as_ref()
            .filter(|assignment| assignment.editor_id == editor_id)?;
        Some(super::fleet_detail::FleetRouteSelection {
            provider: assignment.provider.clone(),
            model: assignment.model.clone(),
            reasoning: assignment.reasoning.as_deref().and_then(|value| {
                codewhale_engine::reasoning_preference::ReasoningEffort::parse_strict(value).ok()
            }),
            allow_inherit: true,
        })
    }

    pub(crate) fn accept_route(
        &mut self,
        editor_id: uuid::Uuid,
        provider: String,
        model: String,
        reasoning: Option<codewhale_engine::reasoning_preference::ReasoningEffort>,
    ) -> bool {
        let Some(assignment) = self
            .assignment
            .as_mut()
            .filter(|assignment| assignment.editor_id == editor_id)
        else {
            return false;
        };
        assignment.provider = (model != "auto").then_some(provider);
        assignment.model = (model != "auto").then_some(model);
        assignment.reasoning = reasoning.map(|effort| effort.as_setting().to_string());
        self.step = if self.scope_decided {
            Step::Review
        } else {
            Step::Destination
        };
        self.refresh_destinations();
        true
    }

    pub(crate) fn commit_route_assignment(
        &self,
        editor_id: uuid::Uuid,
        app: &App,
        config: &Config,
    ) -> Result<String, String> {
        let assignment = self
            .assignment
            .as_ref()
            .filter(|assignment| assignment.editor_id == editor_id)
            .ok_or_else(|| "This assignment is no longer open.".to_string())?;
        if !matches!(
            resolve_fleet_setup_edit_target(&app.workspace),
            Ok(FleetSetupEditTarget::LegacyProfiles)
        ) {
            return Err("The selected team changed. Reopen Fleet before saving.".into());
        }
        let roster = codewhale_engine::fleet::identity::load_effective_roster(
            &config.fleet_config(),
            &app.workspace,
            Some(app.plugin_registry.as_ref()),
        );
        if !roster
            .members()
            .iter()
            .any(|member| member == &assignment.original_member)
        {
            return Err(
                "This role changed while you were choosing. Reopen Fleet before saving.".into(),
            );
        }
        if !self.scope_decided || !self.selected_destination_available() {
            return Err("Choose an available save destination first.".into());
        }
        if self.profile_scope == FleetProfileScope::Project
            && !codewhale_engine::fleet::roster::project_agent_profiles_enabled()
        {
            return Err("Project profiles are disabled for this launch.".into());
        }
        if let Some(provider) = assignment.provider.as_deref()
            && let Some(reason) = crate::commands::fleet_provider_rejection(app, config, provider)
        {
            return Err(reason);
        }
        if let Some((path, expected)) = &assignment.source
            && assignment_source(path)?.as_ref() != Some(expected)
        {
            return Err("This role changed on disk. Reopen Fleet before saving.".into());
        }
        let target = &self
            .destination_for(self.profile_scope)
            .ok_or("Save destination unavailable")?
            .target;
        let expected = assignment
            .destinations
            .iter()
            .find(|(path, _)| path == target)
            .ok_or("Save destination changed. Reopen Fleet.")?;
        if assignment_source(target)? != expected.1 {
            return Err("The destination changed on disk. Reopen Fleet before saving.".into());
        }
        let dir = target.parent().ok_or("Invalid profile destination")?;
        let identities = if self.profile_scope == FleetProfileScope::Project {
            codewhale_engine::fleet::profile::load_workspace_agent_profile_identities(
                &app.workspace,
            )
        } else {
            codewhale_engine::fleet::profile::load_agent_profile_identities_from_dir(dir)
        }
        .map_err(|err| err.to_string())?;
        if identities.iter().any(|profile| {
            profile.id.eq_ignore_ascii_case(&assignment.id) && profile.source != *target
        }) {
            return Err("Another file already defines this role. Reopen Fleet.".into());
        }
        let mut table = assignment.template.clone();
        for alias in [
            "model",
            "model_hint",
            "model_id",
            "provider",
            "reasoning_effort",
            "thinking",
            "reasoning",
        ] {
            table.remove(alias);
        }
        for (key, value) in [
            ("model", &assignment.model),
            ("provider", &assignment.provider),
            ("reasoning_effort", &assignment.reasoning),
        ] {
            if let Some(value) = value {
                table.insert(key.into(), toml::Value::String(value.clone()));
            }
        }
        table.insert("loadout".into(), toml::Value::String("inherit".into()));
        let text = toml::to_string_pretty(&table).map_err(|err| err.to_string())?;
        let mut transaction = codewhale_config::persistence::SetupTransaction::new();
        transaction.stage(target.clone(), text.into_bytes());
        transaction.commit().map_err(|err| err.to_string())?;
        Ok(format!(
            "{} model saved · {}",
            assignment.id,
            target.display()
        ))
    }

    fn from_snapshot_for_role(snapshot: FleetSetupSnapshot, role: &str) -> Self {
        let mut view = Self::from_snapshot(snapshot);
        let role = public_role_label(role);
        view.role_idx = ROLES
            .iter()
            .position(|choice| choice.label.eq_ignore_ascii_case(&role))
            .unwrap_or(ROLES.len() - 1);
        view.step = Step::Model;
        // Reopening a SAVED member edits what is on disk: preselect its route,
        // thinking tier, and — most importantly — the scope it was saved in,
        // so "edit" can never quietly land in the other destination.
        let role_id = role.trim().to_ascii_lowercase();
        let saved = view
            .snapshot
            .roster_details
            .iter()
            .find(|detail| detail.id == role_id || public_role_label(&detail.id) == role)
            .cloned();
        if let Some(saved) = saved {
            view.profile_scope = saved.scope;
            view.scope_decided = true;
            view.destination_idx = DESTINATION_ORDER
                .iter()
                .position(|scope| *scope == saved.scope)
                .unwrap_or(0);
            if let Some(model) = saved.model.as_deref() {
                let idx = view.model_routes.iter().position(|(provider, candidate)| {
                    candidate == model
                        && saved
                            .provider
                            .as_deref()
                            .is_none_or(|p| p.eq_ignore_ascii_case(provider))
                });
                if let Some(idx) = idx {
                    view.model_idx = idx;
                }
            }
            if let Some(effort) = saved.reasoning_effort.as_deref()
                && let Some(idx) = THINKING_CHOICES
                    .iter()
                    .position(|choice| choice.label.eq_ignore_ascii_case(effort))
            {
                view.thinking_idx = idx;
            }
        }
        view
    }

    fn from_snapshot(snapshot: FleetSetupSnapshot) -> Self {
        let mut model_choices = vec![MODEL_INHERIT];
        // `inherit` (index 0) maps to the active route; every later row pins a
        // concrete (provider, model) drawn from all configured providers.
        let mut model_routes = vec![(snapshot.provider.clone(), snapshot.model.clone())];
        let mut model_row_states = vec![FleetModelRowState::Ready];
        for (provider, model, readiness) in &snapshot.available_models {
            let provider_label = provider_display_label(provider);
            let readiness_summary = readiness.detail().map_or_else(
                || readiness.label().into_owned(),
                |detail| format!("{}: {detail}", readiness.label()),
            );
            // Capability badges from the existing catalog/registry owners
            // (#5038): shown in the word-wrapped detail pane so the picker
            // list stays narrow-terminal friendly. Unknown models honestly
            // omit the sentence instead of blocking selection.
            let capability_note =
                codewhale_engine::fleet::capability_badges::resolve_route_capability_badges(
                    Some(provider),
                    model,
                )
                .map(|badges| format!(" Capabilities: {}.", badges.summary()))
                .unwrap_or_default();
            model_choices.push(Choice {
                label: Cow::Owned(model.clone()),
                summary: Cow::Owned(format!(
                    "Pin this model ({provider_label}) · {readiness_summary}"
                )),
                description: Cow::Owned(format!(
                    "Route this agent to {model} on {provider_label} instead of inheriting the session route.{capability_note}"
                )),
            });
            // Canonical provider id (not the display label above) — this is
            // what gets persisted into the saved profile (#4093).
            model_routes.push((provider.clone(), model.clone()));
            model_row_states.push(FleetModelRowState::from_readiness(readiness));
        }
        let composition = deterministic_composition_advisory(&snapshot.available_models);
        Self {
            snapshot,
            assignment: None,
            step: Step::Role,
            role_idx: 0,
            model_idx: 0,
            thinking_idx: 0,
            // Profiles authored for a person should follow that person across
            // repositories by default. Project scope remains one `s` away and
            // keeps higher roster precedence when explicitly selected.
            profile_scope: FleetProfileScope::Personal,
            scope_decided: false,
            destination_idx: DESTINATION_ORDER.len() - 1,
            destinations: None,
            review_focus: ReviewFocus::Save,
            replace_armed: false,
            notice: None,
            review_scroll: 0,
            model_draft: None,
            model_draft_preview: None,
            model_draft_request: None,
            model_choices,
            model_routes,
            model_row_states,
            model_query: String::new(),
            model_filter_active: false,
            composition,
            composition_decision: CompositionDecision::Pending,
            row_hitboxes: RefCell::new(Vec::new()),
        }
    }

    /// Install a sanitized, bounded model draft. The exact TOML preview
    /// (returned here for the caller's status message) renders inline on the
    /// Review step — not in a separate pager — so the footer's `g`/Enter
    /// ratify hints stay true the instant the draft lands (#4093).
    ///
    /// `None` refuses a draft that no longer matches the wizard: an answer
    /// changed (or the draft was discarded) while it was in flight. The
    /// request generation only orders requests; it cannot see an edit made
    /// after the last `m`, so the wizard checks its own answers (U09-03).
    pub fn install_model_draft(
        &mut self,
        mut draft: Box<codewhale_engine::fleet::profile::FleetProfileDraft>,
        model_label: String,
        picked_route: Option<(String, String)>,
        reasoning_effort: Option<String>,
    ) -> Option<(String, String)> {
        let current = self.current_draft_request();
        if self.model_draft_request.as_ref() != Some(&current)
            || current.route != picked_route
            || current.reasoning_effort != reasoning_effort
        {
            return None;
        }
        self.model_draft_request = None;
        // Re-inject the route the operator picked at `m`-press time (#4093). A
        // model draft comes from `from_untrusted_json`, which hard-sets
        // `provider: None` and echoes whatever `model` the model happened to
        // emit — so ratifying it verbatim would drop a concrete cross-provider
        // pick and persist the ambiguous, provider-scoped profile #4093 exists
        // to prevent. Pinning BOTH fields from the CARRIED route keeps the route
        // the user actually chose (the model only authored the prose); the
        // check above already refused a draft whose answers changed in flight.
        // `inherit` (a `None` route) leaves `model`/`provider` untouched,
        // matching the deterministic Enter path.
        if let Some((provider, model)) = picked_route {
            draft.model = Some(model);
            draft.provider = Some(provider);
        }
        draft.reasoning_effort = reasoning_effort;
        let (title, header) = (
            tr(self.snapshot.locale, MessageId::FleetDraftTitle)
                .replace("{model_label}", &model_label),
            tr(self.snapshot.locale, MessageId::FleetDraftHeader)
                .replace("{name}", &draft.file_name())
                .replace("{model_label}", &model_label),
        );
        let content = format!(
            "{}{}",
            self.scope_preview_header(header),
            draft.render_toml()
        );
        self.model_draft = Some(draft);
        self.model_draft_preview = Some(content.clone());
        self.review_scroll = 0;
        Some((title, content))
    }

    /// The planner role chosen (drives the profile file name and `role_hint`).
    fn selected_role(&self) -> String {
        self.assignment
            .as_ref()
            .map(|assignment| assignment.id.clone())
            .unwrap_or_else(|| ROLES[self.role_idx.min(ROLES.len() - 1)].label.to_string())
    }

    fn has_composition_for_selected_role(&self) -> bool {
        let role = self.selected_role();
        self.composition.as_ref().is_some_and(|advisory| {
            advisory
                .proposal
                .suggestions
                .iter()
                .any(|suggestion| suggestion.role.eq_ignore_ascii_case(&role))
        })
    }

    /// Re-validate the entire proposal against its original explicit pool,
    /// then return the selected role's route. An out-of-pool proposal never
    /// reaches `model_idx`, even if the in-memory advisory were corrupted.
    fn validated_composition_route(&self) -> Option<(String, String)> {
        self.composition
            .as_ref()?
            .validated_route_for_role(&self.selected_role())
            .ok()?
    }

    fn select_model_route(&mut self, route: &(String, String)) -> bool {
        let Some(idx) = self
            .model_routes
            .iter()
            .position(|candidate| candidate == route)
        else {
            return false;
        };
        self.model_query.clear();
        self.model_filter_active = false;
        self.model_idx = idx;
        true
    }

    fn accept_composition(&mut self) -> ViewAction {
        let Some(route) = self.validated_composition_route() else {
            return ViewAction::None;
        };
        if !self.select_model_route(&route) {
            return ViewAction::None;
        }
        self.composition_decision = CompositionDecision::Accepted;
        // Accepting a suggestion still routes through the Destination step:
        // where the file lives is a human decision, not part of the advisory.
        self.step = Step::Destination;
        self.refresh_destinations();
        ViewAction::None
    }

    fn edit_composition(&mut self) -> ViewAction {
        let Some(route) = self.validated_composition_route() else {
            return ViewAction::None;
        };
        if !self.select_model_route(&route) {
            return ViewAction::None;
        }
        self.composition_decision = CompositionDecision::Edited;
        self.step = Step::Model;
        ViewAction::None
    }

    fn reject_composition(&mut self) -> ViewAction {
        self.composition_decision = CompositionDecision::Rejected;
        self.step = Step::Model;
        ViewAction::None
    }

    /// Copy note when the chosen role would override an existing roster
    /// member of the same id (e.g. "overrides built-in reviewer"). A saved
    /// profile shadows lower roster layers rather than adding a new member.
    fn roster_override_note(&self) -> Option<String> {
        self.override_note_for_scope(self.profile_scope)
    }

    /// Precedence consequence of saving the selected role into `scope`, given
    /// what the roster already contains for that id. Returns `None` when the
    /// id is new everywhere.
    fn override_note_for_scope(&self, scope: FleetProfileScope) -> Option<String> {
        let role = self.selected_role().to_lowercase();
        let locale = self.snapshot.locale;
        let (id, origin) = self
            .snapshot
            .roster_members
            .iter()
            .find(|(id, _)| *id == role)?;
        let has_project_copy = self
            .snapshot
            .roster_details
            .iter()
            .any(|d| d.id == role && d.scope == FleetProfileScope::Project);
        let has_personal_copy = self
            .snapshot
            .roster_details
            .iter()
            .any(|d| d.id == role && d.scope == FleetProfileScope::Personal);
        Some(match scope {
            FleetProfileScope::Personal if has_project_copy => {
                tr(locale, MessageId::FleetDestOverridesProject).replace("{id}", id)
            }
            FleetProfileScope::Project if has_personal_copy => {
                tr(locale, MessageId::FleetDestOverridesPersonal).replace("{id}", id)
            }
            _ => tr(locale, MessageId::FleetDestOverridesBuiltIn)
                .replace("{origin}", origin)
                .replace("{id}", id),
        })
    }

    /// Localized "This project" / "Personal" label for a scope.
    fn scope_label(&self, scope: FleetProfileScope) -> String {
        tr(
            self.snapshot.locale,
            match scope {
                FleetProfileScope::Project => MessageId::FleetDestProjectLabel,
                FleetProfileScope::Personal => MessageId::FleetDestPersonalLabel,
            },
        )
        .into_owned()
    }

    fn destination_for(&self, scope: FleetProfileScope) -> Option<&DestinationStatus> {
        self.destinations
            .as_ref()
            .and_then(|all| all.iter().find(|d| d.scope == scope))
    }

    /// The header chip: where the file will be written, or that the choice is
    /// still ahead. Visible on every step so the destination is never a
    /// surprise on the last screen.
    fn saves_to_line(&self) -> String {
        let locale = self.snapshot.locale;
        if !self.scope_decided {
            return tr(locale, MessageId::FleetSavesToUndecided).into_owned();
        }
        let path = self
            .destination_for(self.profile_scope)
            .map(|d| d.target.display().to_string())
            .unwrap_or_else(|| self.projected_target(self.profile_scope));
        tr(locale, MessageId::FleetSavesToChip)
            .replace("{scope}", &self.scope_label(self.profile_scope))
            .replace("{path}", &path)
    }

    /// Best-effort target path without touching the filesystem (used before
    /// `refresh_destinations` has run for the current role).
    fn projected_target(&self, scope: FleetProfileScope) -> String {
        let file = format!("{}.toml", profile_file_stem(&self.selected_role()));
        match scope {
            FleetProfileScope::Project => self
                .snapshot
                .workspace
                .join(codewhale_engine::fleet::profile::WORKSPACE_AGENT_PROFILE_DIR)
                .join(file)
                .display()
                .to_string(),
            FleetProfileScope::Personal => match &self.snapshot.personal_profile_dir {
                Ok(dir) => dir.join(file).display().to_string(),
                Err(_) => format!("{}/{file}", scope.display_dir()),
            },
        }
    }

    /// The label of the primary Review action — it names its effect.
    fn save_action_label(&self) -> String {
        let locale = self.snapshot.locale;
        let exists = self
            .destination_for(self.profile_scope)
            .is_some_and(|d| d.target_exists);
        if exists && self.replace_armed {
            let file = self
                .destination_for(self.profile_scope)
                .and_then(|d| {
                    d.target
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            return tr(locale, MessageId::FleetActionConfirmReplace).replace("{file}", &file);
        }
        tr(
            locale,
            match (self.profile_scope, exists) {
                (FleetProfileScope::Project, false) => MessageId::FleetActionSaveProject,
                (FleetProfileScope::Personal, false) => MessageId::FleetActionSavePersonal,
                (FleetProfileScope::Project, true) => MessageId::FleetActionReplaceProject,
                (FleetProfileScope::Personal, true) => MessageId::FleetActionReplacePersonal,
            },
        )
        .into_owned()
    }

    /// Whether the currently chosen destination can be written.
    fn selected_destination_available(&self) -> bool {
        self.destination_for(self.profile_scope)
            .is_none_or(|d| d.unavailable_reason.is_none())
    }

    /// The concrete model chosen for this worker, written to the profile
    /// `model` field. `None` means `inherit` (reuse the session route).
    fn selected_model(&self) -> Option<String> {
        self.selected_route().map(|(_, model)| model)
    }

    /// The concrete `(provider, model)` chosen for this worker — a pinned route
    /// independent of the parent/current provider (#4093) — or `None` when
    /// `inherit` is selected (reuse the session route).
    fn selected_route(&self) -> Option<(String, String)> {
        if let Some(assignment) = &self.assignment {
            return assignment.provider.clone().zip(assignment.model.clone());
        }
        let real_idx = self.real_model_idx();
        if real_idx == 0 {
            return None;
        }
        self.model_routes.get(real_idx).cloned()
    }

    /// Indices into `model_choices` visible under the current typed filter
    /// (#4639). Empty query shows every row; otherwise substring match over
    /// provider id/label and model id.
    fn filtered_model_indices(&self) -> Vec<usize> {
        (0..self.model_choices.len())
            .filter(|idx| {
                let (provider, model) = &self.model_routes[*idx];
                route_matches_query(&self.model_query, provider, model, *idx == 0)
            })
            .collect()
    }

    /// Map the filtered highlight position back to the real `model_choices`
    /// index. Selection, persistence, and hitboxes all use the real index.
    fn real_model_idx(&self) -> usize {
        let filtered = self.filtered_model_indices();
        if filtered.is_empty() {
            return 0;
        }
        filtered[self.model_idx.min(filtered.len() - 1)]
    }

    fn selected_reasoning_effort(&self) -> Option<String> {
        if let Some(assignment) = &self.assignment {
            return assignment.reasoning.clone();
        }
        if self.thinking_idx == 0 {
            return None;
        }
        THINKING_CHOICES
            .get(self.thinking_idx)
            .map(|choice| choice.label.to_string())
    }

    fn selected_thinking_label(&self) -> String {
        self.selected_reasoning_effort()
            .unwrap_or_else(|| format!("same as session ({})", self.snapshot.reasoning))
    }

    fn scope_preview_header(&self, header: String) -> String {
        header.replacen(PROFILE_DIR, self.profile_scope.display_dir(), 1)
    }

    /// Number of selectable rows on the current step (0 on the review step).
    fn step_len(&self) -> usize {
        match self.step {
            Step::Role => ROLES.len(),
            Step::Composition => 0,
            Step::Model => self.filtered_model_indices().len(),
            Step::Destination => DESTINATION_ORDER.len(),
            Step::Review => 0,
        }
    }

    fn move_up(&mut self) {
        match self.step {
            Step::Role => {
                self.role_idx =
                    codewhale_tui_kit::list_nav::wrap_index(self.role_idx, self.step_len(), -1);
                self.discard_model_draft();
                self.composition_decision = CompositionDecision::Pending;
            }
            Step::Composition => {}
            Step::Model => {
                self.model_idx =
                    codewhale_tui_kit::list_nav::wrap_index(self.model_idx, self.step_len(), -1);
                self.discard_model_draft();
                if self.composition_decision != CompositionDecision::Pending {
                    self.composition_decision = CompositionDecision::Edited;
                }
            }
            Step::Destination => {
                self.destination_idx = codewhale_tui_kit::list_nav::wrap_index(
                    self.destination_idx,
                    self.step_len(),
                    -1,
                );
            }
            Step::Review => self.review_scroll = self.review_scroll.saturating_sub(1),
        }
    }

    /// A draft is only valid for the answers it was requested against —
    /// including one still in flight.
    fn discard_model_draft(&mut self) {
        self.model_draft = None;
        self.model_draft_preview = None;
        self.model_draft_request = None;
    }

    fn current_draft_request(&self) -> ModelDraftRequest {
        ModelDraftRequest {
            role: self.selected_role(),
            route: self.selected_route(),
            reasoning_effort: self.selected_reasoning_effort(),
        }
    }

    fn move_down(&mut self) {
        match self.step {
            Step::Role => {
                self.role_idx =
                    codewhale_tui_kit::list_nav::wrap_index(self.role_idx, self.step_len(), 1);
                self.discard_model_draft();
                self.composition_decision = CompositionDecision::Pending;
            }
            Step::Composition => {}
            Step::Model => {
                self.model_idx =
                    codewhale_tui_kit::list_nav::wrap_index(self.model_idx, self.step_len(), 1);
                self.discard_model_draft();
                if self.composition_decision != CompositionDecision::Pending {
                    self.composition_decision = CompositionDecision::Edited;
                }
            }
            Step::Destination => {
                self.destination_idx = codewhale_tui_kit::list_nav::wrap_index(
                    self.destination_idx,
                    self.step_len(),
                    1,
                );
            }
            Step::Review => self.review_scroll = self.review_scroll.saturating_add(1),
        }
    }

    /// Apply one [`list_nav`](codewhale_tui_kit::list_nav) motion (#6290), returning
    /// whether it was consumed. Steps wrap; pages travel [`SETUP_PAGE`] rows
    /// and clamp. On the Review step the same keys scroll the proof pane
    /// instead — it has no row list — and render clamps the offset. The
    /// region axis is declined so Tab/Left/Right keep their explicit wizard
    /// arms below.
    fn apply_motion(&mut self, motion: codewhale_tui_kit::list_nav::Motion) -> bool {
        use codewhale_tui_kit::list_nav::Motion;
        match motion {
            Motion::Prev => {
                self.move_up();
                true
            }
            Motion::Next => {
                self.move_down();
                true
            }
            Motion::RegionPrev | Motion::RegionNext => false,
            _ => {
                if self.step == Step::Review {
                    match motion {
                        Motion::PagePrev => {
                            self.review_scroll =
                                self.review_scroll.saturating_sub(REVIEW_SCROLL_PAGE);
                        }
                        Motion::PageNext => {
                            self.review_scroll =
                                self.review_scroll.saturating_add(REVIEW_SCROLL_PAGE);
                        }
                        Motion::First => self.review_scroll = 0,
                        // Render clamps to the content height.
                        Motion::Last => self.review_scroll = usize::MAX,
                        _ => return false,
                    }
                    return true;
                }
                let len = self.step_len();
                if len == 0 {
                    return false;
                }
                let current = match self.step {
                    Step::Role => self.role_idx,
                    Step::Model => self.model_idx,
                    Step::Destination => self.destination_idx,
                    _ => return false,
                };
                let Some(next) =
                    codewhale_tui_kit::list_nav::apply(current, len, SETUP_PAGE, motion)
                else {
                    return false;
                };
                match self.step {
                    Step::Role => {
                        self.role_idx = next;
                        self.discard_model_draft();
                        self.composition_decision = CompositionDecision::Pending;
                    }
                    Step::Model => {
                        self.model_idx = next;
                        self.discard_model_draft();
                        if self.composition_decision != CompositionDecision::Pending {
                            self.composition_decision = CompositionDecision::Edited;
                        }
                    }
                    Step::Destination => {
                        self.destination_idx = next;
                    }
                    _ => {}
                }
                true
            }
        }
    }

    /// Re-stat the profile directory. Called on the two transitions that can
    /// change the answer — entering Review, and toggling project/user scope —
    /// so the Review step never touches the filesystem while painting.
    fn refresh_destinations(&mut self) {
        let file = format!("{}.toml", profile_file_stem(&self.selected_role()));
        let statuses = DESTINATION_ORDER.map(|scope| {
            let file = self
                .assignment
                .as_ref()
                .filter(|assignment| assignment.source_scope == Some(scope))
                .and_then(|assignment| assignment.source.as_ref())
                .and_then(|(path, _)| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or(&file);
            destination_status(
                scope,
                &self.snapshot.workspace,
                &self.snapshot.personal_profile_dir,
                file,
                self.snapshot.project_profiles_enabled,
                self.snapshot.locale,
            )
        });
        self.destinations = Some(statuses);
        self.replace_armed = false;
    }

    /// Choose a destination explicitly (Destination step or roster preload).
    fn choose_destination(&mut self, scope: FleetProfileScope) {
        if self.profile_scope != scope {
            self.discard_model_draft();
        }
        self.profile_scope = scope;
        self.scope_decided = true;
        self.destination_idx = DESTINATION_ORDER
            .iter()
            .position(|s| *s == scope)
            .unwrap_or(0);
        self.replace_armed = false;
    }

    /// starter profile TOML the next save keypress would persist.
    fn advance(&mut self) -> ViewAction {
        match self.step {
            Step::Role => {
                self.step = Step::Model;
                ViewAction::None
            }
            Step::Composition => self.accept_composition(),
            Step::Model => {
                let idx = self.real_model_idx();
                match self.model_row_states.get(idx) {
                    Some(FleetModelRowState::Ready) => {
                        // Path: role → model → destination → review/save.
                        // Thinking defaults to inherit; adjust on review with `t`.
                        self.notice = None;
                        self.step = Step::Destination;
                        self.refresh_destinations();
                    }
                    Some(FleetModelRowState::NeedsActivation) => {
                        // Dormant external-consent route: explicit human
                        // selection must mint the read capability and validate
                        // only this exact provider/model. Hand off to the host
                        // so rendering stays I/O-free.
                        if let Some((provider_id, model)) = self.model_routes.get(idx)
                            && let Some(provider) =
                                codewhale_config::ProviderKind::parse(provider_id)
                            && crate::tui::provider_picker::external_consent_target_for_provider(
                                provider,
                            )
                            .is_some()
                        {
                            return ViewAction::Emit(
                                ViewEvent::FleetSetupExternalConsentActivationRequested {
                                    provider_id: provider_id.clone(),
                                    model: model.clone(),
                                },
                            );
                        }
                    }
                    Some(FleetModelRowState::Blocked { reason }) => {
                        // Stay on the Model step, but say why Enter did nothing
                        // and where to fix it instead of failing silently.
                        self.notice = Some(
                            tr(self.snapshot.locale, MessageId::FleetModelRowBlockedNotice)
                                .replace("{reason}", reason),
                        );
                    }
                    None => {}
                }
                ViewAction::None
            }
            Step::Destination => {
                let scope =
                    DESTINATION_ORDER[self.destination_idx.min(DESTINATION_ORDER.len() - 1)];
                let available = self
                    .destination_for(scope)
                    .is_none_or(|d| d.unavailable_reason.is_none());
                if !available {
                    // A disabled destination never falls back to the other one.
                    return ViewAction::None;
                }
                self.choose_destination(scope);
                self.step = Step::Review;
                self.review_scroll = 0;
                self.review_focus = ReviewFocus::Save;
                self.refresh_destinations();
                ViewAction::None
            }
            Step::Review => self.activate_review_focus(),
        }
    }

    /// Enter on the Review step acts on the focused control.
    fn activate_review_focus(&mut self) -> ViewAction {
        match self.review_focus {
            ReviewFocus::Save => self.save_action(),
            ReviewFocus::ChangeDestination => {
                self.step = Step::Destination;
                self.refresh_destinations();
                ViewAction::None
            }
            ReviewFocus::Back => self.back(),
        }
    }

    /// The single save path for both the deterministic starter profile and a
    /// model-authored draft. Replacing an existing file requires a second
    /// press: the first arms the control and renames it; nothing is written
    /// until the second. An unavailable destination never saves.
    fn save_action(&mut self) -> ViewAction {
        if !self.scope_decided || !self.selected_destination_available() {
            return ViewAction::None;
        }
        let exists = self
            .destination_for(self.profile_scope)
            .is_some_and(|d| d.target_exists);
        if exists && !self.replace_armed {
            self.replace_armed = true;
            return ViewAction::None;
        }
        if let Some(assignment) = &self.assignment {
            return ViewAction::Emit(ViewEvent::FleetProfileRouteCommitRequested {
                editor_id: assignment.editor_id,
            });
        }
        match self.model_draft.clone() {
            Some(draft) => ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested {
                draft,
                scope: self.profile_scope,
            }),
            None => self.commit_starter_profile_action(),
        }
    }

    /// Step back toward the first screen. Returns `None` at the first step (the
    /// host closes the modal via Esc instead).
    fn back(&mut self) -> ViewAction {
        if self.assignment.is_some() {
            return self.route_pick_request();
        }
        match self.step {
            Step::Role => ViewAction::None,
            Step::Composition => {
                self.step = Step::Model;
                ViewAction::None
            }
            Step::Model => {
                self.notice = None;
                self.step = Step::Role;
                ViewAction::None
            }
            Step::Destination => {
                self.step = Step::Model;
                ViewAction::None
            }
            Step::Review => {
                self.replace_armed = false;
                self.step = Step::Destination;
                self.refresh_destinations();
                ViewAction::None
            }
        }
    }

    /// Persist the deterministic starter profile directly from the Review
    /// summary. Unlike a model-authored draft, every field is derived from the
    /// structured choices already visible on this screen, so a second TOML
    /// ratification state adds no trust boundary.
    fn commit_starter_profile_action(&self) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested {
            draft: self.starter_profile_draft(),
            scope: self.profile_scope,
        })
    }

    /// Build a deterministic starter profile for the current role/model
    /// selection. The same save event persists this as model-drafted profiles,
    /// so duplicate-id checks and atomic writes stay in one host path.
    ///
    /// `provider` is seeded from whatever the user actually picked in the
    /// Model step (#4093) — a concrete route names its own provider
    /// explicitly, so the saved profile is never ambiguously scoped to
    /// whatever provider happens to be active at launch time. `inherit`
    /// carries no provider, matching its `model: None`.
    fn starter_profile_draft(&self) -> Box<codewhale_engine::fleet::profile::FleetProfileDraft> {
        let role = &ROLES[self.role_idx.min(ROLES.len() - 1)];
        let route = self.selected_route();
        Box::new(codewhale_engine::fleet::profile::FleetProfileDraft {
            id: profile_file_stem(&role.label),
            display_name: Some(role.label.to_string()),
            description: Some(format!("{} - {}", role.summary, role.description)),
            role_hint: role.label.to_string(),
            model_class_hint: None,
            model: route.as_ref().map(|(_, model)| model.clone()),
            provider: route.map(|(provider, _)| provider),
            reasoning_effort: self.selected_reasoning_effort(),
            instructions: Some(format!(
                "Role: {}. Work only within the assigned Fleet slice. Report concise evidence and stop when the assignment is complete. Do not widen permissions, trust, route configuration, or topology.",
                role.label
            )),
        })
    }

    /// The action hints for the current step's footer (wrapped by the shared
    /// footer renderer so they can never run off the modal edge).
    fn footer_hints(&self) -> Vec<ActionHint> {
        let mut hints = Vec::new();
        match self.step {
            Step::Role => {
                hints.push(ActionHint::new("↑/↓", "choose"));
                hints.push(ActionHint::new("Enter", "next"));
            }
            Step::Composition => {
                hints.push(ActionHint::new("a/Enter", "accept"));
                hints.push(ActionHint::new("e", "edit"));
                hints.push(ActionHint::new("r", "reject"));
                hints.push(ActionHint::new("←", "back"));
            }
            Step::Model => {
                hints.push(ActionHint::new("↑/↓", "choose"));
                hints.push(ActionHint::new("/", "filter"));
                if self.has_composition_for_selected_role() {
                    hints.push(ActionHint::new("c", "suggest"));
                }
                hints.push(ActionHint::new("Enter", "next"));
                hints.push(ActionHint::new("←", "back"));
            }
            Step::Destination => {
                hints.push(ActionHint::new("↑/↓", "choose"));
                hints.push(ActionHint::new("Enter/Space", "next"));
                hints.push(ActionHint::new("←", "back"));
            }
            Step::Review => {
                hints.push(ActionHint::new("Tab", "focus"));
                hints.push(ActionHint::new("Enter", "activate"));
                hints.push(ActionHint::new("↑/↓", "scroll"));
                hints.push(ActionHint::new("t", "thinking"));
                if self.assignment.is_some() {
                    // Route changes do not regenerate role instructions.
                } else if self.model_draft.is_some() {
                    hints.push(ActionHint::new("m", "redraft"));
                } else if self.snapshot.provider_ready {
                    hints.push(ActionHint::new("m", "model draft"));
                }
                hints.push(ActionHint::new("←", "back"));
            }
        }
        // Esc is honest: it steps back everywhere except the first screen,
        // where it cancels the wizard.
        if self.step == Step::Role {
            hints.push(ActionHint::new("Esc", "cancel"));
        } else {
            hints.push(ActionHint::new("Esc", "back"));
        }
        hints
    }
}

impl ModalView for FleetSetupView {
    fn kind(&self) -> ModalKind {
        ModalKind::FleetSetup
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_up(),
            MouseEventKind::ScrollDown => self.move_down(),
            MouseEventKind::Down(MouseButton::Left) => {
                let row = self.row_hitboxes.borrow().iter().find_map(|(rect, row)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                        .then_some(*row)
                });
                if let Some(row) = row {
                    match self.step {
                        Step::Role => {
                            self.role_idx = row.min(ROLES.len().saturating_sub(1));
                            self.composition_decision = CompositionDecision::Pending;
                        }
                        Step::Composition => {}
                        Step::Model => {
                            self.model_idx = row.min(self.step_len().saturating_sub(1));
                            if self.composition_decision != CompositionDecision::Pending {
                                self.composition_decision = CompositionDecision::Edited;
                            }
                        }
                        Step::Destination => {
                            self.destination_idx = row.min(DESTINATION_ORDER.len() - 1);
                            return ViewAction::None;
                        }
                        Step::Review => {}
                    }
                    self.discard_model_draft();
                }
            }
            _ => {}
        }
        ViewAction::None
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // Model-step filter input captures keystrokes while active (#4639).
        if self.step == Step::Model && self.model_filter_active {
            // Typing-safe movement set: pages and edges work while filtering
            // without letter aliases eating the query (#6290).
            if let Some(motion) = codewhale_tui_kit::list_nav::motion_while_typing(&key)
                && self.apply_motion(motion)
            {
                return ViewAction::None;
            }
            match key.code {
                KeyCode::Enter => {
                    self.model_filter_active = false;
                }
                KeyCode::Esc => {
                    self.model_filter_active = false;
                    self.model_query.clear();
                    self.model_idx = 0;
                }
                KeyCode::Backspace => {
                    self.model_query.pop();
                    self.model_idx = 0;
                    if self.composition_decision != CompositionDecision::Pending {
                        self.composition_decision = CompositionDecision::Edited;
                    }
                }
                KeyCode::Char(ch)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.model_query.push(ch);
                    self.model_idx = 0;
                    if self.composition_decision != CompositionDecision::Pending {
                        self.composition_decision = CompositionDecision::Edited;
                    }
                }
                _ => {}
            }
            return ViewAction::None;
        }
        // Any navigation key clears a one-shot notice; the notice is re-set
        // below when the same blocked action is attempted again.
        if !matches!(key.code, KeyCode::Null) {
            self.notice = None;
        }
        // Movement keys come from the shared vocabulary (#6290), j/k aliases
        // included; the region axis is declined so Tab/Left/Right keep their
        // explicit wizard arms, and letter verbs below are unaffected.
        if let Some(motion) = codewhale_tui_kit::list_nav::motion(&key)
            && self.apply_motion(motion)
        {
            return ViewAction::None;
        }
        if self.assignment.is_some() && matches!(key.code, KeyCode::Char('t')) {
            return self.route_pick_request();
        }
        if self.assignment.is_some() && matches!(key.code, KeyCode::Char('m')) {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Esc if self.step != Step::Role => self.back(),
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Char('q') if self.step == Step::Role => ViewAction::Close,
            // Tab moves focus; it never changes where the file is written.
            KeyCode::Tab if self.step == Step::Review => {
                self.review_focus = self.review_focus.next();
                self.replace_armed = false;
                ViewAction::None
            }
            KeyCode::BackTab if self.step == Step::Review => {
                self.review_focus = self.review_focus.prev();
                self.replace_armed = false;
                ViewAction::None
            }
            KeyCode::Right | KeyCode::Char('l') if self.step == Step::Review => {
                self.review_focus = self.review_focus.next();
                self.replace_armed = false;
                ViewAction::None
            }
            KeyCode::Char(' ') if self.step == Step::Destination => self.advance(),
            KeyCode::Char(' ') if self.step == Step::Review => self.activate_review_focus(),
            KeyCode::Char('a') if self.step == Step::Composition => self.accept_composition(),
            KeyCode::Char('e') if self.step == Step::Composition => self.edit_composition(),
            KeyCode::Char('r') if self.step == Step::Composition => self.reject_composition(),
            KeyCode::Char('c')
                if self.step == Step::Model && self.has_composition_for_selected_role() =>
            {
                self.composition_decision = CompositionDecision::Pending;
                self.discard_model_draft();
                self.step = Step::Composition;
                ViewAction::None
            }
            KeyCode::Char('/') if self.step == Step::Model => {
                self.model_filter_active = true;
                ViewAction::None
            }
            // Secondary accelerator: jump to the Destination step. The primary
            // way to change the destination is the focused Review control.
            KeyCode::Char('s') if self.step == Step::Review => {
                self.replace_armed = false;
                self.step = Step::Destination;
                self.refresh_destinations();
                ViewAction::None
            }
            KeyCode::Char('t') if self.step == Step::Review => {
                self.thinking_idx = (self.thinking_idx + 1) % THINKING_CHOICES.len();
                self.discard_model_draft();
                ViewAction::None
            }
            KeyCode::Char('m') if self.step == Step::Review && self.snapshot.provider_ready => {
                let route = self.selected_route();
                self.model_draft_request = Some(self.current_draft_request());
                ViewAction::Emit(ViewEvent::FleetProfileModelDraftRequested {
                    role: self.selected_role(),
                    model: route
                        .as_ref()
                        .map(|(_, model)| model.clone())
                        .unwrap_or_else(|| "inherit".to_string()),
                    // Carry the picked provider so the redrafted profile keeps
                    // the cross-provider route (#4093). `install_model_draft`
                    // re-injects it and refuses a draft whose answers changed
                    // while it was in flight; the event stays self-describing.
                    provider: route.map(|(provider, _)| provider),
                    reasoning_effort: self.selected_reasoning_effort(),
                    locale: self.snapshot.locale,
                })
            }
            KeyCode::Char('g') if self.step == Step::Review => {
                self.review_focus = ReviewFocus::Save;
                self.save_action()
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.advance(),
            KeyCode::Left | KeyCode::Char('h') => self.back(),
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.row_hitboxes.borrow_mut().clear();
        // Choice steps have a bounded list/detail body and should not expand
        // into a tall empty card on roomy terminals. Review is proof-dense and
        // scrollable, so it keeps the extra row budgeted for the footer gutter.
        let preferred_height = match self.step {
            Step::Role => 22,
            Step::Composition => 26,
            Step::Model => 23,
            Step::Destination => 22,
            Step::Review => 32,
        };
        let popup_area = centered_modal_area(area, 96, preferred_height, 60, 16);
        render_modal_surface(area, popup_area, buf);

        let step_no = match self.step {
            Step::Role => 1,
            Step::Composition => 2,
            Step::Model => 2,
            Step::Destination => 3,
            Step::Review => 4,
        };
        let block = Block::default()
            .title(Line::from(Span::styled(
                " Fleet setup — your agent team ",
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )))
            .title_bottom(
                Line::from(Span::styled(
                    format!(" Step {step_no}/4 "),
                    Style::default().fg(palette::TEXT_MUTED),
                ))
                .alignment(ratatui::layout::Alignment::Right),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .bg(palette::WHALE_BG),
            )
            .padding(Padding::uniform(1));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let hints = self.footer_hints();
        let content = render_modal_footer_with_gutter(inner, buf, &hints);

        // Header (title + subtitle + "Saves to" chip) above the step body.
        // In the Compact tier the subtitle is dropped so the chip survives.
        let header_rows = if content.height < 4 {
            1
        } else if content.height < 12 {
            2
        } else {
            3
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_rows), Constraint::Min(1)])
            .split(content);
        self.render_header(chunks[0], buf);

        match self.step {
            Step::Role => {
                let mut context = vec![
                    "Fleet runs agents that delegate work. Pick the role this agent should play; the saved profile carries it as its role_hint.".to_string(),
                ];
                if let Some(note) = self.roster_override_note() {
                    context.push(note);
                }
                render_choice_step(chunks[1], buf, &ROLES, self.role_idx, &context);
                register_choice_hitboxes(chunks[1], ROLES.len(), self.role_idx, &self.row_hitboxes);
            }
            Step::Destination => {
                self.render_destination(chunks[1], buf);
                register_choice_hitboxes(
                    chunks[1],
                    DESTINATION_ORDER.len(),
                    self.destination_idx,
                    &self.row_hitboxes,
                );
            }
            Step::Composition => self.render_composition(chunks[1], buf),
            Step::Model => {
                let filtered = self.filtered_model_indices();
                // Compact tier: the row summary and any notice matter more
                // than the long route description, which would push them
                // below the fold.
                let compact = chunks[1].height < 12;
                let filtered_choices: Vec<Choice> = filtered
                    .iter()
                    .map(|idx| {
                        let mut choice = self.model_choices[*idx].clone();
                        if compact {
                            choice.description = Cow::Borrowed("");
                        }
                        choice
                    })
                    .collect();
                let selected = self.model_idx.min(filtered.len().saturating_sub(1));
                let filter_line = if self.model_filter_active {
                    format!("Filter: {}▏ (Enter keep · Esc clear)", self.model_query)
                } else if !self.model_query.trim().is_empty() {
                    format!(
                        "Filter: {} ({} of {} rows · / edit)",
                        self.model_query,
                        filtered.len(),
                        self.model_choices.len()
                    )
                } else {
                    format!(
                        "Type / to filter {} models by provider or name",
                        self.model_choices.len()
                    )
                };
                let mut context = Vec::new();
                if let Some(notice) = &self.notice {
                    context.push(notice.clone());
                }
                context.push(filter_line);
                context.push(format!(
                    "Current model: {} / {}  ·  reasoning {}",
                    self.snapshot.provider, self.snapshot.model, self.snapshot.reasoning
                ));
                context.push(match self.selected_model() {
                    Some(model) => format!("This member will run on {model}."),
                    None => "This member uses your current model.".to_string(),
                });
                if filtered_choices.is_empty() {
                    context.push(
                        "No routes match this filter. Keep typing, or press Esc to clear it."
                            .to_string(),
                    );
                }
                render_choice_step(chunks[1], buf, &filtered_choices, selected, &context);
                register_choice_hitboxes(
                    chunks[1],
                    filtered_choices.len(),
                    selected,
                    &self.row_hitboxes,
                );
            }
            Step::Review => self.render_review(chunks[1], buf),
        }
    }
}

impl FleetSetupView {
    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let (title, subtitle): (Cow<'static, str>, Cow<'static, str>) = match self.step {
            Step::Role => (
                Cow::Borrowed("Choose a team role"),
                Cow::Borrowed("Each Fleet member plays one role in the delegation."),
            ),
            Step::Composition => (
                Cow::Borrowed("Unratified composition suggestion"),
                Cow::Borrowed(
                    "Review the configured-pool assignments; nothing is saved or running.",
                ),
            ),
            Step::Model => (
                Cow::Borrowed("Choose a model"),
                Cow::Borrowed("Pick this agent's model, or inherit your current route."),
            ),
            Step::Destination => (
                Cow::Owned(tr(self.snapshot.locale, MessageId::FleetDestStepTitle).into_owned()),
                Cow::Owned(tr(self.snapshot.locale, MessageId::FleetDestStepSubtitle).into_owned()),
            ),
            Step::Review if self.assignment.is_some() => (
                Cow::Owned(format!("Review {} model", self.selected_role())),
                Cow::Borrowed(
                    "Save this role's model and thinking; the session model stays unchanged.",
                ),
            ),
            Step::Review if self.model_draft.is_some() => (
                Cow::Borrowed("Save profile"),
                Cow::Borrowed(
                    "Exact TOML shown below; nothing is written until you activate the save control.",
                ),
            ),
            Step::Review => (
                Cow::Borrowed("Review & save"),
                Cow::Borrowed("Nothing is written until you activate the save control."),
            ),
        };
        let chip_style = Style::default().fg(palette::TEXT_MUTED);
        let mut lines = vec![Line::from(Span::styled(
            title.into_owned(),
            Style::default().fg(palette::WHALE_ACTION).bold(),
        ))];
        if area.height >= 3 {
            lines.push(Line::from(Span::styled(
                subtitle.into_owned(),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }
        lines.push(Line::from(Span::styled(
            truncate_view_text(&self.saves_to_line(), usize::from(area.width)),
            chip_style,
        )));
        // No wrapping: each header row is one line, so the chip row is
        // always the last row and never pushed out by a long subtitle.
        Paragraph::new(lines).render(area, buf);
    }

    /// The Destination step: a focused two-option list (This project /
    /// Personal) with the exact resolved file, whether it will be replaced,
    /// and the precedence consequence, for the highlighted option.
    fn render_destination(&self, area: Rect, buf: &mut Buffer) {
        let locale = self.snapshot.locale;
        // Compact tier: keep the choice, the file, and the consequence; drop
        // the long explanation rather than clip the file line off-screen.
        let compact = area.height < 12;
        let choices: Vec<Choice> = DESTINATION_ORDER
            .iter()
            .map(|scope| {
                let unavailable = self
                    .destination_for(*scope)
                    .and_then(|d| d.unavailable_reason.clone());
                let (label, summary, description) = match scope {
                    FleetProfileScope::Project => (
                        MessageId::FleetDestProjectLabel,
                        MessageId::FleetDestProjectSummary,
                        MessageId::FleetDestProjectDescription,
                    ),
                    FleetProfileScope::Personal => (
                        MessageId::FleetDestPersonalLabel,
                        MessageId::FleetDestPersonalSummary,
                        MessageId::FleetDestPersonalDescription,
                    ),
                };
                let _ = unavailable;
                Choice {
                    label: Cow::Owned(tr(locale, label).into_owned()),
                    summary: Cow::Owned(tr(locale, summary).into_owned()),
                    description: if compact {
                        Cow::Borrowed("")
                    } else {
                        tr(locale, description)
                    },
                }
            })
            .collect();
        let selected = self.destination_idx.min(DESTINATION_ORDER.len() - 1);
        let scope = DESTINATION_ORDER[selected];
        let mut context = Vec::new();
        match self.destination_for(scope) {
            Some(status) => {
                if let Some(reason) = &status.unavailable_reason {
                    context.push(
                        tr(locale, MessageId::FleetDestUnavailable).replace("{reason}", reason),
                    );
                }
                context.push(
                    tr(locale, MessageId::FleetDestPathLine)
                        .replace("{path}", &status.target.display().to_string()),
                );
                if status.target_exists {
                    context.push(
                        tr(locale, MessageId::FleetDestWillReplace)
                            .replace("{path}", &status.target.display().to_string()),
                    );
                }
            }
            None => context.push(
                tr(locale, MessageId::FleetDestPathLine)
                    .replace("{path}", &self.projected_target(scope)),
            ),
        }
        if let Some(note) = self.override_note_for_scope(scope) {
            context.push(note);
        }
        render_choice_step(area, buf, &choices, selected, &context);
    }

    fn render_composition(&self, area: Rect, buf: &mut Buffer) {
        let Some(advisory) = self.composition.as_ref() else {
            Paragraph::new("No configured model pool is available. Press e to choose manually.")
                .wrap(Wrap { trim: true })
                .render(area, buf);
            return;
        };
        let selected_role = self.selected_role();
        let mut lines = vec![
            Line::from(Span::styled(
                format!(
                    "{} · {}",
                    advisory.proposal.ratification.as_str().to_ascii_uppercase(),
                    advisory.proposal.advisory
                ),
                Style::default().fg(palette::STATUS_WARNING).bold(),
            )),
            Line::from(""),
        ];
        for suggestion in &advisory.proposal.suggestions {
            let selected = suggestion.role.eq_ignore_ascii_case(&selected_role);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{} {}",
                        codewhale_tui_kit::glyphs::selection_marker(selected),
                        suggestion.role
                    ),
                    if selected {
                        menu_style::selected_row_style()
                    } else {
                        Style::default().fg(palette::TEXT_PRIMARY)
                    },
                ),
                Span::styled(
                    format!(
                        "  →  {}/{}",
                        provider_display_label(&suggestion.provider),
                        suggestion.model
                    ),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
            ]));
        }
        lines.extend([
            Line::from(""),
            Line::from(Span::styled(
                format!(
                    "Accept applies only the {selected_role} suggestion to this unsaved profile. Edit highlights it in the configured model picker; reject keeps your current selection."
                ),
                Style::default().fg(palette::TEXT_MUTED),
            )),
        ]);
        debug_assert_eq!(
            advisory.proposal.ratification,
            RatificationState::Unratified
        );
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .render(area, buf);
    }

    fn render_review(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // Row 1: the focused action controls. Row 2+: the scrollable summary.
        // Keeping the controls out of the scroll region means the save action
        // and its label are visible at every scroll offset and every size.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(area);
        self.render_review_actions(rows[0], buf);
        let body = rows[1];

        if let Some(assignment) = &self.assignment {
            let model = assignment
                .model
                .as_deref()
                .unwrap_or("Follow current session");
            let provider = assignment.provider.as_deref().unwrap_or("session provider");
            let thinking = assignment
                .reasoning
                .as_deref()
                .unwrap_or("Follow current session");
            let text = format!(
                "Role: {}\nModel: {model} · {provider}\nThinking: {thinking}\n{}\n\nOnly the model and thinking assignment changes. Existing name, description, instructions, tools and permissions are preserved. The current session model stays unchanged.",
                assignment.id,
                self.saves_to_line()
            );
            render_scrollable_text(body, buf, &text, self.review_scroll);
            return;
        }

        // A ratify-ready draft is on screen: show the exact TOML preview
        // inline, scrolled by the same `review_scroll` state, so the save
        // control in THIS view ratifies it directly — no separate pager in the
        // way to swallow the keypress (#4093).
        if let Some(preview) = self.model_draft_preview.as_deref() {
            render_scrollable_text(body, buf, preview, self.review_scroll);
            return;
        }

        let role = &ROLES[self.role_idx.min(ROLES.len() - 1)];
        let locale = self.snapshot.locale;
        let mut lines: Vec<Line> = Vec::new();
        let section = |lines: &mut Vec<Line>, label: &str, body: String| {
            lines.push(Line::from(Span::styled(
                label.to_string(),
                Style::default().fg(palette::WHALE_ACTION).bold(),
            )));
            lines.push(Line::from(Span::styled(
                body,
                Style::default().fg(palette::TEXT_PRIMARY),
            )));
            lines.push(Line::from(""));
        };

        // "Saves to" comes first: it is the decision this screen exists to
        // confirm. Exact file, replace/create, precedence consequence.
        let mut saves_to = vec![format!(
            "{} · {}",
            self.scope_label(self.profile_scope),
            self.destination_for(self.profile_scope)
                .map(|d| d.target.display().to_string())
                .unwrap_or_else(|| self.projected_target(self.profile_scope))
        )];
        if let Some(status) = self.destination_for(self.profile_scope) {
            if let Some(reason) = &status.unavailable_reason {
                saves_to
                    .push(tr(locale, MessageId::FleetDestUnavailable).replace("{reason}", reason));
            } else if status.target_exists {
                saves_to.push(
                    tr(locale, MessageId::FleetDestWillReplace)
                        .replace("{path}", &status.target.display().to_string()),
                );
            }
        }
        if let Some(note) = self.roster_override_note() {
            saves_to.push(note);
        }
        section(
            &mut lines,
            &tr(locale, MessageId::FleetReviewSavesTo),
            saves_to.join("  ·  "),
        );
        section(
            &mut lines,
            "Role",
            format!("{} — {}", role.label, role.summary),
        );
        section(
            &mut lines,
            "Model",
            // The picked route's OWN provider, not the parent/current
            // session's — a cross-provider pin must never be misreported as
            // running on the active provider (#4093).
            match self.selected_route() {
                Some((provider, model)) => {
                    let readiness = self
                        .snapshot
                        .available_models
                        .iter()
                        .find(|(candidate_provider, candidate_model, _)| {
                            candidate_provider == &provider && candidate_model == &model
                        })
                        .map(|(_, _, readiness)| readiness.label().into_owned())
                        .unwrap_or_else(|| {
                            if self.snapshot.provider_ready {
                                "ready".to_string()
                            } else {
                                "needs action".to_string()
                            }
                        });
                    format!(
                        "{model}  ·  provider {}  ·  {readiness}",
                        provider_display_label(&provider)
                    )
                }
                None => format!(
                    "inherit  ·  route {} / {}  ·  {}",
                    self.snapshot.provider,
                    self.snapshot.model,
                    if self.snapshot.provider_ready {
                        "ready"
                    } else {
                        "needs action"
                    }
                ),
            },
        );
        match self.composition_decision {
            CompositionDecision::Accepted => section(
                &mut lines,
                "Composition",
                "Accepted the configured-pool suggestion for this role. It remains unsaved until you save this profile; no Fleet was launched or changed.".to_string(),
            ),
            CompositionDecision::Edited => section(
                &mut lines,
                "Composition",
                "Edited the suggestion in the configured model picker. This review is the only save boundary.".to_string(),
            ),
            CompositionDecision::Rejected => section(
                &mut lines,
                "Composition",
                "Rejected the suggestion and kept the manually selected route. Nothing was saved or launched by the advisory.".to_string(),
            ),
            CompositionDecision::Pending => {}
        }
        section(&mut lines, "Thinking", self.selected_thinking_label());
        section(
            &mut lines,
            "Auth & readiness",
            if self.snapshot.provider_ready {
                "Active route can be attempted with the current credentials.".to_string()
            } else {
                "Active route is not ready — fix auth/readiness before relying on this profile at runtime.".to_string()
            },
        );
        section(
            &mut lines,
            "Permissions",
            "Access: members can only narrow what the session allows. They cannot widen approval, trust, or secrets, and required approvals stay on.".to_string(),
        );
        section(
            &mut lines,
            "Tools",
            "Read tools by default; write tools for builders within scope; shell stays policy-gated; artifacts and receipts stay inspectable.".to_string(),
        );
        section(
            &mut lines,
            "Workspace & org",
            tr(locale, MessageId::FleetReviewWorkspaceLimits)
                .replace("{concurrent}", &self.snapshot.max_subagents.to_string())
                .replace(
                    "{launch_slots}",
                    &self.snapshot.launch_concurrency.to_string(),
                )
                .replace("{admitted}", &self.snapshot.max_admitted.to_string())
                .replace(
                    "{agent_depth}",
                    &self.snapshot.subagent_spawn_depth.to_string(),
                )
                .replace(
                    "{fleet_depth}",
                    &self.snapshot.fleet_spawn_depth.to_string(),
                )
                .replace(
                    "{ceiling}",
                    &codewhale_config::MAX_SPAWN_DEPTH_CEILING.to_string(),
                )
                .replace(
                    "{enabled}",
                    &tr(
                        locale,
                        if self.snapshot.subagents_enabled {
                            MessageId::ExtensionsStateEnabled
                        } else {
                            MessageId::HotbarSetupStatusDisabled
                        },
                    ),
                )
                .replace(
                    "{workspace}",
                    &self.snapshot.workspace.display().to_string(),
                ),
        );
        section(&mut lines, "Review policy", self.review_policy_summary());

        // `scroll` offsets by *visual* (post-wrap) rows, so the bound must count
        // wrapped rows — not logical lines — or the bottom sections become
        // unreachable. Estimate each line's wrapped height from its display
        // width; an over-estimate is harmless (scroll clamps at the real end).
        let wrap_width = usize::from(body.width).max(1);
        let visual_rows: usize = lines
            .iter()
            .map(|line| line.width().div_ceil(wrap_width).max(1))
            .sum();
        let max_scroll = visual_rows.saturating_sub(usize::from(body.height).max(1));
        let scroll = self.review_scroll.min(max_scroll);
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .scroll((scroll as u16, 0))
            .render(body, buf);
    }

    /// The Review step's focused control row: [Save…] [Change destination]
    /// [Back]. The focused control is drawn with the canonical selection style
    /// and the `▸` marker; a disabled save control (unavailable destination)
    /// is dimmed and named with the reason on the "Saves to" line.
    fn render_review_actions(&self, area: Rect, buf: &mut Buffer) {
        let locale = self.snapshot.locale;
        let save_enabled = self.scope_decided && self.selected_destination_available();
        let controls: [(ReviewFocus, String, bool); 3] = [
            (ReviewFocus::Save, self.save_action_label(), save_enabled),
            (
                ReviewFocus::ChangeDestination,
                tr(locale, MessageId::FleetActionChangeDestination).into_owned(),
                true,
            ),
            (
                ReviewFocus::Back,
                tr(locale, MessageId::FleetActionBack).into_owned(),
                true,
            ),
        ];
        let mut spans: Vec<Span> = Vec::new();
        for (focus, label, enabled) in controls {
            let focused = focus == self.review_focus;
            let text = format!(
                "{} {} ",
                codewhale_tui_kit::glyphs::selection_marker(focused),
                label
            );
            let style = match (focused, enabled) {
                (true, true) => menu_style::selected_row_style(),
                (true, false) => menu_style::disabled_selected_row_style(),
                (false, true) => Style::default().fg(palette::TEXT_PRIMARY),
                (false, false) => Style::default().fg(palette::TEXT_MUTED).dim(),
            };
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" "));
        }
        Paragraph::new(vec![Line::from(spans), Line::from("")])
            .wrap(Wrap { trim: true })
            .render(area, buf);
    }

    fn review_policy_summary(&self) -> String {
        tr(self.snapshot.locale, MessageId::FleetReviewPolicy)
            .replace("{api_secs}", &self.snapshot.api_timeout_secs.to_string())
            .replace(
                "{heartbeat_secs}",
                &self.snapshot.heartbeat_timeout_secs.to_string(),
            )
    }
}

/// Render wrapped, line-scrolled plain text (the ratify-ready draft TOML
/// preview) into `area`, clamping `scroll` to the real wrapped-row bound the
/// same way [`FleetSetupView::render_review`]'s summary does — an
/// over-estimate of wrapped height is harmless (scroll clamps at the end).
fn render_scrollable_text(area: Rect, buf: &mut Buffer, text: &str, scroll: usize) {
    let lines: Vec<Line> = text
        .lines()
        .map(|line| Line::from(line.to_string()))
        .collect();
    let wrap_width = usize::from(area.width).max(1);
    let visual_rows: usize = lines
        .iter()
        .map(|line| line.width().div_ceil(wrap_width).max(1))
        .sum();
    let max_scroll = visual_rows.saturating_sub(usize::from(area.height).max(1));
    let scroll = scroll.min(max_scroll);
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .scroll((scroll as u16, 0))
        .render(area, buf);
}

/// Render a wizard choice step: a list of selectable identifiers on the left and
/// a wrapped detail pane (summary + description + context) on the right. Stacks
/// vertically when the body is too narrow for two columns so nothing truncates.
fn render_choice_step(
    area: Rect,
    buf: &mut Buffer,
    choices: &[Choice],
    selected: usize,
    context: &[String],
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let (list_area, detail_area) = if area.width >= CHOICE_TWO_COLUMN_MIN_WIDTH {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(CHOICE_LIST_WIDTH),
                Constraint::Min(CHOICE_DETAIL_MIN_WIDTH),
            ])
            .split(area);
        (cols[0], cols[1])
    } else {
        let list_height = (choices.len() as u16).min(area.height);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_height), Constraint::Min(0)])
            .split(area);
        (rows[0], rows[1])
    };

    // No choices (a type-to-filter query that matches nothing): there is no
    // row to point at and no detail to show, so paint the context — the
    // caller adds the "no matches" hint — and stop before indexing (#5953).
    if choices.is_empty() {
        let lines: Vec<Line> = context
            .iter()
            .map(|entry| {
                Line::from(Span::styled(
                    entry.clone(),
                    Style::default().fg(palette::TEXT_MUTED),
                ))
            })
            .collect();
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .render(detail_area, buf);
        return;
    }

    // List: labels are identifiers, so a `▸`-marked single line each is safe.
    let list_width = usize::from(list_area.width);
    let visible = choices.len().min(usize::from(list_area.height));
    let row_start = choice_window_start(choices.len(), selected, visible);
    let mut list_lines: Vec<Line> = Vec::with_capacity(visible);
    for (idx, choice) in choices.iter().enumerate().skip(row_start).take(visible) {
        let is_selected = idx == selected;
        let pointer = format!(
            "{} ",
            codewhale_tui_kit::glyphs::selection_marker(is_selected)
        );
        let style = if is_selected {
            menu_style::selected_row_style()
        } else {
            Style::default().fg(palette::TEXT_PRIMARY)
        };
        list_lines.push(Line::from(Span::styled(
            truncate_view_text(&format!("{pointer}{}", choice.label), list_width),
            style,
        )));
    }
    Paragraph::new(list_lines).render(list_area, buf);

    // Detail: summary + wrapped description + wrapped context, all word-wrapped.
    let choice = &choices[selected.min(choices.len().saturating_sub(1))];
    let mut detail_lines: Vec<Line> = vec![Line::from(Span::styled(
        choice.summary.clone(),
        Style::default().fg(palette::WHALE_ACTION).bold(),
    ))];
    // An empty description (compact tiers drop the long explanation so the
    // decisive facts stay on screen) leaves no orphan blank rows behind.
    if !choice.description.is_empty() {
        detail_lines.push(Line::from(""));
        detail_lines.push(Line::from(Span::styled(
            choice.description.clone(),
            Style::default().fg(palette::TEXT_PRIMARY),
        )));
    }
    if !context.is_empty() {
        detail_lines.push(Line::from(""));
        for entry in context {
            detail_lines.push(Line::from(Span::styled(
                entry.clone(),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }
    }
    Paragraph::new(detail_lines)
        .wrap(Wrap { trim: true })
        .render(detail_area, buf);
}

/// Register exactly the list column/stack rows painted by
/// [`render_choice_step`]. The detail pane intentionally owns no hitboxes.
fn register_choice_hitboxes(
    area: Rect,
    choice_count: usize,
    selected: usize,
    hitboxes: &RefCell<Vec<(Rect, usize)>>,
) {
    if area.width == 0 || area.height == 0 || choice_count == 0 {
        return;
    }
    let list_area = if area.width >= CHOICE_TWO_COLUMN_MIN_WIDTH {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(CHOICE_LIST_WIDTH),
                Constraint::Min(CHOICE_DETAIL_MIN_WIDTH),
            ])
            .split(area)[0]
    } else {
        let list_height = (choice_count as u16).min(area.height);
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_height), Constraint::Min(0)])
            .split(area)[0]
    };
    let visible = choice_count.min(usize::from(list_area.height));
    let row_start = choice_window_start(choice_count, selected, visible);
    let mut rows = hitboxes.borrow_mut();
    rows.extend((0..visible).map(|visible_idx| {
        let choice_idx = row_start + visible_idx;
        (
            Rect::new(
                list_area.x,
                list_area.y.saturating_add(visible_idx as u16),
                list_area.width,
                1,
            ),
            choice_idx,
        )
    }));
}

fn choice_window_start(total: usize, selected: usize, visible: usize) -> usize {
    if total <= visible || visible == 0 {
        return 0;
    }
    selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(total.saturating_sub(visible))
}

/// Resolve one save destination off the paint path: the exact target file,
/// whether it already exists, and — when it cannot be written — the localized
/// reason. A disabled destination is never silently swapped for the other.
fn destination_status(
    scope: FleetProfileScope,
    workspace: &Path,
    personal_dir: &Result<PathBuf, String>,
    file_name: &str,
    project_profiles_enabled: bool,
    locale: codewhale_localization::Locale,
) -> DestinationStatus {
    let dir: Result<PathBuf, String> = match scope {
        FleetProfileScope::Project => {
            Ok(workspace.join(codewhale_engine::fleet::profile::WORKSPACE_AGENT_PROFILE_DIR))
        }
        FleetProfileScope::Personal => personal_dir.clone(),
    };
    let (target, mut unavailable_reason) = match dir {
        Ok(dir) => (dir.join(file_name), None),
        Err(err) => (
            PathBuf::from(scope.display_dir()).join(file_name),
            Some(tr(locale, MessageId::FleetDestReasonHomeUnavailable).replace("{error}", &err)),
        ),
    };
    if unavailable_reason.is_none() {
        match scope {
            FleetProfileScope::Project => {
                if !project_profiles_enabled {
                    unavailable_reason =
                        Some(tr(locale, MessageId::FleetDestReasonNoProjectConfig).into_owned());
                } else if !workspace.is_dir() {
                    unavailable_reason = Some(
                        tr(locale, MessageId::FleetDestReasonWorkspaceMissing)
                            .replace("{path}", &workspace.display().to_string()),
                    );
                }
            }
            FleetProfileScope::Personal => {}
        }
    }
    if unavailable_reason.is_none()
        && let Some(parent) = target.parent()
        && parent.exists()
        && !parent.is_dir()
    {
        unavailable_reason = Some(
            tr(locale, MessageId::FleetDestReasonWorkspaceMissing)
                .replace("{path}", &parent.display().to_string()),
        );
    }
    let target_exists = unavailable_reason.is_none() && target.is_file();
    DestinationStatus {
        scope,
        unavailable_reason,
        target,
        target_exists,
    }
}

/// Sanitize a planner role label into a safe TOML file stem.
fn profile_file_stem(role: &str) -> String {
    let stem: String = role
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let stem = stem.trim_matches('-').to_ascii_lowercase();
    if stem.is_empty() {
        "custom".to_string()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::views::ViewStack;
    use crossterm::event::KeyModifiers;
    use unicode_width::UnicodeWidthStr;

    const BLOCKER_SIZES: [(u16, u16); 5] = [(80, 24), (89, 50), (100, 30), (120, 32), (160, 40)];

    #[test]
    fn role_assignment_preserves_profile_fields_and_rejects_stale_source() {
        let _env = crate::test_support::lock_test_env();
        let workspace = tempfile::tempdir().unwrap();
        let directory = workspace
            .path()
            .join(codewhale_engine::fleet::profile::WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("different-file-name.toml");
        let original = r#"id = "custom-reviewer"
display_name = "My reviewer"
description = "Keep this description"
role_hint = "reviewer"
loadout = "inherit"
model = "previous-model"
provider = "deepseek"
[instructions]
text = "Keep these precise instructions"
[tools]
posture = "read-only"
[permissions]
allow_shell = false
trust = false
approval_required = true
"#;
        std::fs::write(&path, original).unwrap();
        let config = Config::default();
        let app = App::new(
            crate::tui::test_support::test_tui_options(workspace.path()),
            &config,
        );
        let mut view =
            FleetSetupView::new_for_route_assignment(&app, &config, "custom-reviewer").unwrap();
        let editor_id = view.assignment.as_ref().unwrap().editor_id;
        assert_eq!(view.selected_role(), "custom-reviewer");
        assert_eq!(
            view.destination_for(FleetProfileScope::Project)
                .unwrap()
                .target,
            path
        );
        assert!(view.accept_route(
            editor_id,
            "deepseek".into(),
            "auto".into(),
            Some(codewhale_engine::reasoning_preference::ReasoningEffort::High)
        ));
        assert_eq!(view.step, Step::Review);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "picker/review must not write"
        );
        view.commit_route_assignment(editor_id, &app, &config)
            .unwrap();
        let before: toml::Table = toml::from_str(original).unwrap();
        let after: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for name in [
            "id",
            "display_name",
            "description",
            "role_hint",
            "instructions",
            "tools",
            "permissions",
        ] {
            assert_eq!(
                after.get(name),
                before.get(name),
                "{name} changed during route assignment"
            );
        }
        assert!(!after.contains_key("model") && !after.contains_key("provider"));
        assert_eq!(after["reasoning_effort"].as_str(), Some("high"));
        let mut stale =
            FleetSetupView::new_for_route_assignment(&app, &config, "custom-reviewer").unwrap();
        let stale_id = stale.assignment.as_ref().unwrap().editor_id;
        stale.accept_route(stale_id, "deepseek".into(), "auto".into(), None);
        let changed = format!(
            "{}\n# Another editor changed this file\n",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &changed).unwrap();
        assert!(
            stale
                .commit_route_assignment(stale_id, &app, &config)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
    }

    #[test]
    fn builtin_role_assignment_requires_destination_and_rejects_new_override() {
        let _env = crate::test_support::lock_test_env();
        let workspace = tempfile::tempdir().unwrap();
        let config = Config::default();
        let app = App::new(
            crate::tui::test_support::test_tui_options(workspace.path()),
            &config,
        );
        let mut view = FleetSetupView::new_for_route_assignment(&app, &config, "manager").unwrap();
        let id = view.assignment.as_ref().unwrap().editor_id;
        view.accept_route(id, "deepseek".into(), "auto".into(), None);
        assert_eq!(view.step, Step::Destination);
        assert!(!view.scope_decided);
        assert!(view.commit_route_assignment(id, &app, &config).is_err());
        let directory = workspace
            .path()
            .join(codewhale_engine::fleet::profile::WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("manager.toml"), "id = \"manager\"\n").unwrap();
        view.choose_destination(FleetProfileScope::Personal);
        view.refresh_destinations();
        assert!(view.commit_route_assignment(id, &app, &config).is_err());
    }

    fn snapshot() -> FleetSetupSnapshot {
        FleetSetupSnapshot {
            workspace: PathBuf::from("/tmp/codewhale-test-workspace"),
            locale: codewhale_localization::Locale::En,
            provider_ready: true,
            provider: "DeepSeek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning: "Auto".to_string(),
            subagents_enabled: true,
            max_subagents: 8,
            launch_concurrency: 3,
            max_admitted: 20,
            subagent_spawn_depth: 3,
            fleet_spawn_depth: 3,
            api_timeout_secs: 120,
            heartbeat_timeout_secs: 300,
            roster_members: codewhale_engine::fleet::roster::FleetRoster::built_ins_only()
                .members()
                .iter()
                .map(|member| (member.id.to_lowercase(), member.origin.to_string()))
                .collect(),
            roster_details: Vec::new(),
            project_profiles_enabled: true,
            personal_profile_dir: Ok(test_personal_dir()),
            available_models: vec![
                (
                    "deepseek".to_string(),
                    "deepseek-v4-pro".to_string(),
                    codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedUnchecked,
                ),
                (
                    "deepseek".to_string(),
                    "deepseek-v4-flash".to_string(),
                    codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedUnchecked,
                ),
            ],
        }
    }

    #[test]
    fn setup_target_routes_selected_v2_and_fails_closed_for_stale_selection() {
        let _lock = crate::test_support::lock_test_env();
        let workspace = tempfile::TempDir::new().expect("workspace");
        let personal_home = workspace.path().join("personal-home");
        std::fs::create_dir_all(&personal_home).expect("personal home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &personal_home);
        assert_eq!(
            resolve_fleet_setup_edit_target(workspace.path()).expect("no selection"),
            FleetSetupEditTarget::LegacyProfiles
        );

        let fleet = codewhale_engine::fleet::store::FleetFile::new("Launch".to_string(), None)
            .expect("valid Fleet");
        let fleet_path = codewhale_engine::fleet::store::save_fleet(
            &fleet,
            codewhale_engine::fleet::store::FleetScope::Workspace,
            workspace.path(),
        )
        .expect("save Fleet");
        codewhale_engine::fleet::store::set_selected(
            "Launch",
            codewhale_engine::fleet::store::FleetScope::Workspace,
            workspace.path(),
        )
        .expect("select Fleet");

        assert_eq!(
            resolve_fleet_setup_edit_target(workspace.path()).expect("selected Fleet"),
            FleetSetupEditTarget::SelectedFleet {
                name: "Launch".to_string(),
                scope: codewhale_engine::fleet::store::FleetScope::Workspace,
            }
        );

        std::fs::remove_file(fleet_path).expect("make selection stale");
        let error = resolve_fleet_setup_edit_target(workspace.path())
            .expect_err("a stale selection must not open legacy setup");
        assert!(error.contains("Legacy profiles were not opened"), "{error}");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A hermetic personal profile dir shared by the fixture snapshot, so no
    /// test reads the developer's real `$CODEWHALE_HOME/agents`.
    fn test_personal_dir() -> PathBuf {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        DIR.get_or_init(|| tempfile::tempdir().expect("personal dir"))
            .path()
            .join("agents")
    }

    fn sample_draft() -> Box<codewhale_engine::fleet::profile::FleetProfileDraft> {
        let codewhale_engine::fleet::profile::UntrustedProfileParse::Drafted(draft) =
            codewhale_engine::fleet::profile::FleetProfileDraft::from_untrusted_json(
                r#"{"id":"reviewer","role_hint":"reviewer","description":"Reviews diffs.","instructions":"Read. Report. Stop."}"#,
            )
        else {
            panic!("sample draft should parse");
        };
        draft
    }

    /// #5038: the Model step's detail pane carries capability badges for
    /// known catalog models and honestly omits them for unknown models, so
    /// stale/absent data never blocks selection.
    #[test]
    fn model_step_detail_shows_capability_badges_for_known_models_only() {
        let mut snap = snapshot();
        snap.available_models.push((
            "deepseek".to_string(),
            "totally-made-up-model-xyz".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedUnchecked,
        ));
        let view = FleetSetupView::from_snapshot(snap);

        let known = view
            .model_choices
            .iter()
            .find(|choice| choice.label == "deepseek-v4-pro")
            .expect("known catalog model row");
        assert!(
            known.description.contains("Capabilities:"),
            "{}",
            known.description
        );
        assert!(
            known.description.contains("1.05M ctx"),
            "{}",
            known.description
        );
        assert!(
            known.description.contains("catalog"),
            "catalog-backed rows must name catalog provenance: {}",
            known.description
        );

        let unknown = view.model_choices.last().expect("appended unknown row");
        assert_eq!(unknown.label, "totally-made-up-model-xyz");
        assert!(
            !unknown.description.contains("Capabilities:"),
            "{}",
            unknown.description
        );
        // The unknown row stays selectable; absence of data is not a block.
        assert_eq!(
            view.model_row_states.last(),
            Some(&FleetModelRowState::Ready)
        );
    }

    #[test]
    fn provider_display_label_preserves_case_colliding_custom_ids() {
        assert_eq!(provider_display_label("deepseek"), "DeepSeek");
        assert_eq!(provider_display_label("CUSTOM"), "CUSTOM");
        assert_eq!(provider_display_label("OPENAI"), "OPENAI");
    }

    fn to_review(view: &mut FleetSetupView) {
        view.handle_key(key(KeyCode::Enter)); // Role -> Model
        view.handle_key(key(KeyCode::Enter)); // Model -> Destination
        assert_eq!(view.step, Step::Destination);
        view.handle_key(key(KeyCode::Enter)); // Destination (Personal) -> Review
        assert_eq!(view.step, Step::Review);
    }

    /// Rendered text with all whitespace and box borders removed, so a phrase
    /// or path that wrapped across rows (temp-dir paths vary in length per
    /// platform and CI runner) still compares as one token.
    fn squashed(text: &str) -> String {
        text.chars()
            .filter(|c| !c.is_whitespace() && !matches!(c, '│' | '┃' | '┆' | '┊' | '|'))
            .collect()
    }

    fn contains_wrapped(text: &str, needle: &str) -> bool {
        squashed(text).contains(&squashed(needle))
    }

    fn rendered_text(view: &FleetSetupView, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn workspace_snapshot(workspace: &Path) -> FleetSetupSnapshot {
        FleetSetupSnapshot {
            workspace: workspace.to_path_buf(),
            ..snapshot()
        }
    }

    // ------------------------------------------------------------------
    // Save-scope redesign: destination step, review actions, no silent writes.
    // ------------------------------------------------------------------

    #[test]
    fn destination_step_sits_between_model_and_review_and_names_the_exact_file() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let mut view = FleetSetupView::from_snapshot(workspace_snapshot(temp.path()));
        view.handle_key(key(KeyCode::Down)); // explore
        view.handle_key(key(KeyCode::Enter)); // -> Model
        view.handle_key(key(KeyCode::Enter)); // inherit -> Destination
        assert_eq!(view.step, Step::Destination);
        assert!(
            !view.scope_decided,
            "nothing is decided until the user picks"
        );
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("Where should this profile live?"), "{text}");
        assert!(text.contains("This project"), "{text}");
        assert!(text.contains("Personal"), "{text}");
        assert!(text.contains("Step 3/4"), "{text}");
        // The highlighted (Personal) row shows its resolved file.
        let personal = test_personal_dir().join("explore.toml");
        assert!(
            text.contains("File:") && text.contains("agents"),
            "resolved file must be visible: {text}"
        );
        assert_eq!(view.destinations.as_ref().unwrap()[1].target, personal);
        // Up -> This project shows the workspace file.
        view.handle_key(key(KeyCode::Up));
        let text = rendered_text(&view, 120, 32);
        let project = temp.path().join(PROFILE_DIR).join("explore.toml");
        assert!(!text.contains("Will replace"), "{text}");
        assert_eq!(view.destinations.as_ref().unwrap()[0].target, project);
    }

    #[test]
    fn header_chip_says_where_it_saves_on_every_step_once_decided() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let mut view = FleetSetupView::from_snapshot(workspace_snapshot(temp.path()));
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("Saves to: choose in step 3"), "{text}");
        view.handle_key(key(KeyCode::Enter));
        view.handle_key(key(KeyCode::Enter));
        view.handle_key(key(KeyCode::Up)); // This project
        view.handle_key(key(KeyCode::Enter)); // -> Review
        assert_eq!(view.step, Step::Review);
        assert!(view.scope_decided);
        assert_eq!(view.profile_scope, FleetProfileScope::Project);
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("Saves to: This project"), "{text}");
        assert!(text.contains("Save to this project"), "{text}");
        // Going back keeps the decided destination visible while revising.
        view.handle_key(key(KeyCode::Esc)); // -> Destination
        view.handle_key(key(KeyCode::Esc)); // -> Model
        assert_eq!(view.step, Step::Model);
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("Saves to: This project"), "{text}");
    }

    #[test]
    fn switching_destination_preserves_role_model_and_thinking() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let mut view = FleetSetupView::from_snapshot(workspace_snapshot(temp.path()));
        view.handle_key(key(KeyCode::Down));
        view.handle_key(key(KeyCode::Down)); // builder
        view.handle_key(key(KeyCode::Enter));
        view.handle_key(key(KeyCode::Down)); // deepseek-v4-pro
        view.handle_key(key(KeyCode::Enter)); // -> Destination
        view.handle_key(key(KeyCode::Up)); // This project
        view.handle_key(key(KeyCode::Enter)); // -> Review
        view.handle_key(key(KeyCode::Char('t'))); // thinking: off
        let role = view.selected_role();
        let route = view.selected_route();
        let thinking = view.thinking_idx;
        assert_eq!(view.profile_scope, FleetProfileScope::Project);
        // Change destination via the focused control, pick Personal.
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.review_focus, ReviewFocus::ChangeDestination);
        assert_eq!(
            view.profile_scope,
            FleetProfileScope::Project,
            "Tab never changes scope"
        );
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, Step::Destination);
        view.handle_key(key(KeyCode::Down));
        view.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(view.step, Step::Review);
        assert_eq!(view.profile_scope, FleetProfileScope::Personal);
        assert_eq!(view.selected_role(), role);
        assert_eq!(view.selected_route(), route);
        assert_eq!(view.thinking_idx, thinking);
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("Save as Personal profile"), "{text}");
    }

    #[test]
    fn existing_target_is_announced_and_needs_a_second_enter_to_replace() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let dir = temp.path().join(PROFILE_DIR);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("manager.toml"), "id = \"manager\"\n").expect("existing");
        let mut view = FleetSetupView::from_snapshot(workspace_snapshot(temp.path()));
        view.handle_key(key(KeyCode::Enter)); // manager
        view.handle_key(key(KeyCode::Enter)); // inherit -> Destination
        view.handle_key(key(KeyCode::Up)); // This project
        let text = rendered_text(&view, 120, 32);
        assert!(
            contains_wrapped(&text, "Will replace the existing file"),
            "{text}"
        );
        view.handle_key(key(KeyCode::Enter)); // -> Review
        let text = rendered_text(&view, 120, 32);
        assert!(contains_wrapped(&text, "Replace in this project"), "{text}");
        assert!(
            contains_wrapped(&text, "Will replace the existing file"),
            "{text}"
        );
        // First Enter arms; nothing is emitted.
        let action = view.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ViewAction::None),
            "first Enter must not save"
        );
        assert!(view.replace_armed);
        let text = rendered_text(&view, 120, 32);
        assert!(
            text.contains("Press Enter again to replace manager.toml"),
            "{text}"
        );
        // Moving focus disarms.
        view.handle_key(key(KeyCode::Tab));
        assert!(!view.replace_armed);
        view.handle_key(key(KeyCode::BackTab));
        view.handle_key(key(KeyCode::Enter)); // arm again
        let action = view.handle_key(key(KeyCode::Enter)); // confirm
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, scope }) =
            action
        else {
            panic!("second Enter saves");
        };
        assert_eq!(scope, FleetProfileScope::Project);
        assert_eq!(draft.id, "manager");
    }

    #[test]
    fn project_destination_is_disabled_with_a_reason_and_never_falls_back() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let mut view = FleetSetupView::from_snapshot(FleetSetupSnapshot {
            project_profiles_enabled: false,
            ..workspace_snapshot(temp.path())
        });
        view.handle_key(key(KeyCode::Enter));
        view.handle_key(key(KeyCode::Enter)); // -> Destination
        view.handle_key(key(KeyCode::Up)); // This project (disabled)
        let text = rendered_text(&view, 120, 32);
        assert!(
            text.contains("Not available: project profiles are disabled"),
            "{text}"
        );
        let action = view.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(
            view.step,
            Step::Destination,
            "disabled destination does not advance"
        );
        assert!(!view.scope_decided);
        assert_eq!(
            view.profile_scope,
            FleetProfileScope::Personal,
            "no silent fallback either way: the scope is untouched"
        );
        // Personal still works.
        view.handle_key(key(KeyCode::Down));
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, Step::Review);
        assert_eq!(view.profile_scope, FleetProfileScope::Personal);
    }

    #[test]
    fn precedence_consequences_are_stated_for_both_destinations() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let project_source = temp.path().join(PROFILE_DIR).join("explore.toml");
        let mut snap = workspace_snapshot(temp.path());
        snap.roster_members.retain(|(id, _)| id != "explore");
        snap.roster_members
            .push(("explore".to_string(), "project".to_string()));
        snap.roster_details.push(RosterMemberDetail {
            id: "explore".to_string(),
            scope: FleetProfileScope::Project,
            source: project_source,
            provider: Some("deepseek".to_string()),
            model: Some("deepseek-v4-flash".to_string()),
            reasoning_effort: None,
        });
        let mut view = FleetSetupView::from_snapshot(snap);
        view.handle_key(key(KeyCode::Down)); // explore
        view.handle_key(key(KeyCode::Enter));
        view.handle_key(key(KeyCode::Enter)); // -> Destination (Personal highlighted)
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("already has a"), "{text}");
        view.handle_key(key(KeyCode::Up)); // This project
        let text = rendered_text(&view, 120, 32);
        assert!(!text.contains("already has a"), "{text}");
        assert!(text.contains("Replaces the project"), "{text}");
    }

    #[test]
    fn reopening_a_saved_member_preloads_its_scope_and_route() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let mut snap = workspace_snapshot(temp.path());
        snap.roster_members.retain(|(id, _)| id != "scout");
        snap.roster_members
            .push(("scout".to_string(), "project".to_string()));
        snap.roster_details.push(RosterMemberDetail {
            id: "scout".to_string(),
            scope: FleetProfileScope::Project,
            source: temp.path().join(PROFILE_DIR).join("scout.toml"),
            provider: Some("deepseek".to_string()),
            model: Some("deepseek-v4-flash".to_string()),
            reasoning_effort: Some("high".to_string()),
        });
        let view = FleetSetupView::from_snapshot_for_role(snap, "scout");
        assert_eq!(view.step, Step::Model);
        assert!(view.scope_decided);
        assert_eq!(view.profile_scope, FleetProfileScope::Project);
        assert_eq!(
            view.selected_route(),
            Some(("deepseek".to_string(), "deepseek-v4-flash".to_string()))
        );
        assert_eq!(view.selected_reasoning_effort().as_deref(), Some("high"));
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("Saves to: This project"), "{text}");
    }

    #[test]
    fn blocked_model_row_explains_why_enter_did_nothing() {
        let mut snap = snapshot();
        snap.available_models = vec![(
            "xai".to_string(),
            "grok-4.5".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::MissingKey,
        )];
        let mut view = FleetSetupView::from_snapshot(snap);
        view.handle_key(key(KeyCode::Enter)); // -> Model
        view.model_idx = 1;
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, Step::Model);
        assert!(
            view.notice
                .as_deref()
                .is_some_and(|n| n.contains("Not selectable"))
        );
        let text = rendered_text(&view, 120, 32);
        assert!(text.contains("Not selectable"), "{text}");
        view.handle_key(key(KeyCode::Down));
        assert!(view.notice.is_none(), "navigation clears the notice");
    }

    #[test]
    fn q_only_cancels_from_the_first_step_and_esc_is_back_elsewhere() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        view.handle_key(key(KeyCode::Enter)); // -> Model
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('q'))),
            ViewAction::None
        ));
        assert_eq!(view.step, Step::Model);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            ViewAction::None
        ));
        assert_eq!(view.step, Step::Role);
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('q'))),
            ViewAction::Close
        ));
        let hints = view.footer_hints();
        assert!(hints.iter().any(|h| h.key == "Esc" && h.label == "cancel"));
    }

    #[test]
    fn destination_and_review_stay_readable_at_60x16_80x24_and_120x32() {
        let temp = tempfile::tempdir().expect("temp workspace");
        for (w, h) in [(60u16, 16u16), (80, 24), (120, 32)] {
            let mut view = FleetSetupView::from_snapshot(workspace_snapshot(temp.path()));
            view.handle_key(key(KeyCode::Enter));
            view.handle_key(key(KeyCode::Enter)); // -> Destination
            let text = rendered_text(&view, w, h);
            assert!(text.contains("This project"), "{w}x{h}: {text}");
            assert!(text.contains("Personal"), "{w}x{h}: {text}");
            assert!(text.contains("File:"), "{w}x{h}: {text}");
            assert!(text.contains("Saves to:"), "{w}x{h}: {text}");
            view.handle_key(key(KeyCode::Up));
            view.handle_key(key(KeyCode::Enter)); // -> Review
            let text = rendered_text(&view, w, h);
            assert!(text.contains("Save to this project"), "{w}x{h}: {text}");
            assert!(text.contains("Saves to: This project"), "{w}x{h}: {text}");
            for line in text.lines() {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line) <= usize::from(w),
                    "{w}x{h}: overflow: {line}"
                );
            }
        }
    }

    fn open_composition(view: &mut FleetSetupView) {
        view.handle_key(key(KeyCode::Enter)); // Role -> Model
        assert_eq!(view.step, Step::Model);
        view.handle_key(key(KeyCode::Char('c')));
        assert_eq!(view.step, Step::Composition);
    }

    #[test]
    fn composition_is_deterministic_unratified_and_pool_bounded() {
        let first = FleetSetupView::from_snapshot(snapshot());
        let second = FleetSetupView::from_snapshot(snapshot());
        let first = first.composition.expect("configured pool advisory");
        let second = second.composition.expect("configured pool advisory");

        assert_eq!(first.proposal, second.proposal);
        assert_eq!(first.proposal.ratification, RatificationState::Unratified);
        assert!(!first.proposal.is_actionable());
        assert_eq!(
            first.request.pool_keys(),
            vec![
                "deepseek/deepseek-v4-flash".to_string(),
                "deepseek/deepseek-v4-pro".to_string(),
            ]
        );
        for suggestion in &first.proposal.suggestions {
            assert!(
                first
                    .request
                    .pool_contains(&suggestion.provider, &suggestion.model),
                "{suggestion:?} escaped the configured pool"
            );
        }

        let rendered = render_through_stack(
            || {
                let mut view = FleetSetupView::from_snapshot(snapshot());
                open_composition(&mut view);
                view
            },
            120,
            40,
        )
        .join("\n");
        assert!(rendered.contains("UNRATIFIED"), "{rendered}");
        assert!(rendered.contains("Suggestion only"), "{rendered}");
        assert!(rendered.contains("a/Enter accept"), "{rendered}");
        assert!(rendered.contains("e edit"), "{rendered}");
        assert!(rendered.contains("r reject"), "{rendered}");
    }

    #[test]
    fn composition_accept_edit_and_reject_keep_the_existing_save_boundary() {
        let mut accepted = FleetSetupView::from_snapshot(snapshot());
        open_composition(&mut accepted);
        let expected = accepted
            .validated_composition_route()
            .expect("selected role suggestion");
        assert!(matches!(
            accepted.handle_key(key(KeyCode::Char('a'))),
            ViewAction::None
        ));
        assert_eq!(accepted.step, Step::Destination);
        accepted.handle_key(key(KeyCode::Enter)); // Destination -> Review
        assert_eq!(accepted.step, Step::Review);
        assert_eq!(accepted.composition_decision, CompositionDecision::Accepted);
        assert_eq!(accepted.selected_route().as_ref(), Some(&expected));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, .. }) =
            accepted.handle_key(key(KeyCode::Enter))
        else {
            panic!("only the existing review save path may persist an accepted suggestion");
        };
        assert_eq!(
            (draft.provider.as_deref(), draft.model.as_deref()),
            (Some(expected.0.as_str()), Some(expected.1.as_str()))
        );

        let mut edited = FleetSetupView::from_snapshot(snapshot());
        open_composition(&mut edited);
        let suggested = edited
            .validated_composition_route()
            .expect("selected role suggestion");
        assert!(matches!(
            edited.handle_key(key(KeyCode::Char('e'))),
            ViewAction::None
        ));
        assert_eq!(edited.step, Step::Model);
        assert_eq!(edited.composition_decision, CompositionDecision::Edited);
        assert_eq!(edited.selected_route().as_ref(), Some(&suggested));

        let mut rejected = FleetSetupView::from_snapshot(snapshot());
        open_composition(&mut rejected);
        assert!(rejected.selected_route().is_none());
        assert!(matches!(
            rejected.handle_key(key(KeyCode::Char('r'))),
            ViewAction::None
        ));
        assert_eq!(rejected.step, Step::Model);
        assert_eq!(rejected.composition_decision, CompositionDecision::Rejected);
        assert!(rejected.selected_route().is_none());
    }

    #[test]
    fn composition_acceptance_revalidates_and_rejects_an_out_of_pool_route() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        open_composition(&mut view);
        let advisory = view.composition.as_mut().expect("advisory");
        let manager = advisory
            .proposal
            .suggestions
            .iter_mut()
            .find(|suggestion| suggestion.role == "manager")
            .expect("manager suggestion");
        manager.provider = "unconfigured".to_string();
        manager.model = "outside-pool".to_string();
        assert!(matches!(
            advisory.validated_route_for_role("manager"),
            Err(CompositionError::ModelOutsidePool { .. })
        ));

        assert!(matches!(
            view.handle_key(key(KeyCode::Char('a'))),
            ViewAction::None
        ));
        assert_eq!(view.step, Step::Composition);
        assert_eq!(view.composition_decision, CompositionDecision::Pending);
        assert!(view.selected_route().is_none());
    }

    #[test]
    fn composition_does_not_suggest_a_blocked_configured_route() {
        let mut snap = snapshot();
        snap.available_models.push((
            "anthropic".to_string(),
            "blocked-model".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedLastCheckFailed {
                category: codewhale_error_taxonomy::ErrorCategory::Authentication,
                message: "auth failed".to_string(),
            },
        ));
        let view = FleetSetupView::from_snapshot(snap);
        let advisory = view.composition.expect("ready pool still composes");
        assert!(!advisory.request.pool_contains("anthropic", "blocked-model"));
        assert!(
            advisory
                .proposal
                .suggestions
                .iter()
                .all(|suggestion| suggestion.model != "blocked-model")
        );
    }

    #[test]
    fn review_step_m_requests_model_draft_with_current_answers() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        to_review(&mut view);

        let action = view.handle_key(key(KeyCode::Char('m')));
        let ViewAction::Emit(ViewEvent::FleetProfileModelDraftRequested {
            role,
            model,
            provider,
            reasoning_effort,
            locale,
        }) = action
        else {
            panic!("expected model draft request");
        };
        assert!(!role.is_empty());
        assert!(!model.is_empty());
        // Default selection is `inherit` (model_idx 0), which carries no
        // concrete provider route.
        assert_eq!(provider, None);
        assert_eq!(reasoning_effort, None);
        assert_eq!(locale, codewhale_localization::Locale::En);
    }

    #[test]
    fn m_redraft_preserves_a_cross_provider_pick_regression_4093() {
        // #4093 BLOCKER 2 regression: a cross-provider route pick followed by an
        // `m` model-assisted redraft must STILL persist the picked provider. A
        // model draft comes from `from_untrusted_json`, which hard-sets
        // `provider: None` (and can echo any model). Without re-injection the
        // ratified profile would carry `model` with no `provider` — the exact
        // ambiguous, provider-scoped profile #4093 removes.
        //
        // The active/session provider is DeepSeek; the picked route is a
        // GLM model on Zai — a genuinely different provider than the parent.
        let mut snap = snapshot();
        snap.provider = "DeepSeek".to_string();
        snap.model = "deepseek-v4-pro".to_string();
        snap.available_models = vec![(
            "zai".to_string(),
            "glm-5.2".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedUnchecked,
        )];
        let mut view = FleetSetupView::from_snapshot(snap);

        // Role step: keep the first role. Model step: inherit(0), then the one
        // cross-provider row (1) -> pick it. Then advance to Review.
        view.handle_key(key(KeyCode::Enter)); // Role -> Model
        view.handle_key(key(KeyCode::Down)); // -> the zai/glm-5.2 row
        assert_eq!(
            view.selected_route(),
            Some(("zai".to_string(), "glm-5.2".to_string()))
        );
        view.handle_key(key(KeyCode::Enter)); // Model -> Destination
        view.handle_key(key(KeyCode::Enter)); // Destination -> Review
        assert_eq!(view.step, Step::Review);
        while view.selected_reasoning_effort().as_deref() != Some("max") {
            view.handle_key(key(KeyCode::Char('t')));
        }

        // `m` requests a draft and carries the picked cross-provider route.
        let action = view.handle_key(key(KeyCode::Char('m')));
        let ViewAction::Emit(ViewEvent::FleetProfileModelDraftRequested {
            model,
            provider,
            reasoning_effort,
            ..
        }) = action
        else {
            panic!("expected model draft request");
        };
        assert_eq!(model, "glm-5.2");
        assert_eq!(provider.as_deref(), Some("zai"));
        assert_eq!(reasoning_effort.as_deref(), Some("max"));

        // The host reconstructs the picked route from the event exactly as
        // `handle_fleet_profile_model_draft` does, and carries it to
        // `install_model_draft`, which refuses it if the answers changed.
        let picked_route = provider.map(|provider| (provider, model.clone()));

        // The model returns a draft that (as always) has provider: None — the
        // untrusted gate strips any provider a model tries to smuggle.
        let drafted = sample_draft();
        assert_eq!(drafted.provider, None);

        // Installing it re-injects the picked route, so the ratified draft keeps
        // BOTH the provider and the model the user actually chose, plus the
        // captured thinking tier.
        let (_title, content) = view
            .install_model_draft(
                drafted,
                "GLM-5.2".to_string(),
                picked_route,
                reasoning_effort,
            )
            .expect("answers unchanged since `m`");
        let ratified = view.model_draft.as_deref().expect("draft installed");
        assert_eq!(ratified.provider.as_deref(), Some("zai"));
        assert_eq!(ratified.model.as_deref(), Some("glm-5.2"));
        assert_eq!(ratified.reasoning_effort.as_deref(), Some("max"));

        // The rendered TOML the ratify keypress would persist names the provider
        // explicitly — never a provider-scoped ambiguity.
        assert!(content.contains("provider = \"zai\""), "{content}");
        assert!(content.contains("model = \"glm-5.2\""), "{content}");
        assert!(content.contains("reasoning_effort = \"max\""), "{content}");

        // And ratifying commits exactly that route.
        let action = view.handle_key(key(KeyCode::Char('g')));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, scope }) =
            action
        else {
            panic!("expected ratify commit event");
        };
        assert_eq!(scope, FleetProfileScope::Personal);
        assert_eq!(draft.provider.as_deref(), Some("zai"));
        assert_eq!(draft.model.as_deref(), Some("glm-5.2"));
        assert_eq!(draft.reasoning_effort.as_deref(), Some("max"));
    }

    #[test]
    fn model_step_filter_with_no_matches_renders_a_hint_instead_of_panicking() {
        // #5953: a type-to-filter query that matches nothing left
        // `render_choice_step` indexing an empty slice.
        let snap = snapshot();
        let mut view = FleetSetupView::from_snapshot(snap);
        view.handle_key(key(KeyCode::Enter));
        view.handle_key(key(KeyCode::Char('/')));
        for ch in "minimax ".chars() {
            view.handle_key(key(KeyCode::Char(ch)));
        }
        assert!(view.model_filter_active);
        assert_eq!(
            view.step_len(),
            0,
            "the query must match nothing for this test"
        );
        // Every size must render without panicking; the sizes with a detail
        // pane must also explain the empty list.
        for (w, h, expect_hint) in [(120u16, 40u16, true), (80, 24, true), (60, 12, false)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
            let text: String = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if expect_hint {
                assert!(
                    text.contains("No routes match"),
                    "{w}x{h} must explain the empty list:\n{text}"
                );
            }
        }
        // Esc clears the filter and the full catalog comes back.
        view.handle_key(key(KeyCode::Esc));
        assert!(view.step_len() > 0);
    }

    #[test]
    fn model_step_filter_narrows_large_catalogs_by_provider_and_model() {
        let mut snap = snapshot();
        // Simulate an OpenRouter-scale catalog: many rows from one provider.
        for i in 0..120 {
            snap.available_models.push((
                "openrouter".to_string(),
                format!("vendor/model-{i:03}"),
                codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedUnchecked,
            ));
        }
        snap.available_models.push((
            "openrouter".to_string(),
            "z-ai/glm-5-turbo".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedUnchecked,
        ));
        let mut view = FleetSetupView::from_snapshot(snap);
        // Role → Model.
        view.handle_key(key(KeyCode::Enter));
        let full_len = view.step_len();
        assert!(full_len > 120, "unfiltered shows the whole catalog");

        // `/` opens the filter; typing narrows by model id substring.
        view.handle_key(key(KeyCode::Char('/')));
        for ch in "glm".chars() {
            view.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(view.step_len(), 1, "only the glm row survives the filter");
        let route = view.selected_route().expect("filtered selection resolves");
        assert_eq!(
            route,
            ("openrouter".to_string(), "z-ai/glm-5-turbo".to_string())
        );

        // Provider substring filters too.
        view.handle_key(key(KeyCode::Esc));
        view.handle_key(key(KeyCode::Char('/')));
        for ch in "deepseek".chars() {
            view.handle_key(key(KeyCode::Char(ch)));
        }
        // inherit's route IS the active DeepSeek route, so it matches too.
        assert_eq!(
            view.step_len(),
            3,
            "deepseek rows plus the inherit (active deepseek route) match"
        );

        // Enter keeps the filter but releases the input; Esc in filter clears.
        view.handle_key(key(KeyCode::Enter));
        assert!(!view.model_filter_active);
        assert_eq!(view.step_len(), 3);
        view.handle_key(key(KeyCode::Char('/')));
        view.handle_key(key(KeyCode::Esc));
        assert_eq!(
            view.step_len(),
            full_len,
            "clearing restores the full catalog"
        );
    }

    #[test]
    fn review_saves_starter_or_ratifies_installed_model_draft() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        to_review(&mut view);

        // A structured starter draft is save-ready from the summary.
        let action = view.handle_key(key(KeyCode::Char('g')));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, scope }) =
            action
        else {
            panic!("expected starter commit event");
        };
        assert_eq!(scope, FleetProfileScope::Personal);
        assert_eq!(draft.id, "manager");

        let mut view = FleetSetupView::from_snapshot(snapshot());
        to_review(&mut view);
        view.handle_key(key(KeyCode::Char('m')));
        let (title, content) = view
            .install_model_draft(sample_draft(), "GLM-5.2".to_string(), None, None)
            .expect("answers unchanged since `m`");
        assert!(title.contains("GLM-5.2"));
        assert!(content.contains("id = \"reviewer\""), "{content}");
        assert!(content.contains("Nothing is saved until"), "{content}");

        let action = view.handle_key(key(KeyCode::Char('g')));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, scope }) =
            action
        else {
            panic!("expected ratify commit event");
        };
        assert_eq!(scope, FleetProfileScope::Personal);
        assert_eq!(draft.id, "reviewer");
    }

    #[test]
    fn changing_answers_discards_a_stale_draft() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        to_review(&mut view);
        view.handle_key(key(KeyCode::Char('m')));
        assert!(
            view.install_model_draft(sample_draft(), "GLM-5.2".to_string(), None, None)
                .is_some()
        );
        assert!(view.model_draft.is_some());

        // Back to the role step and change the selection: the draft no
        // longer matches the answers and must not survive to ratification.
        view.handle_key(key(KeyCode::Left)); // Review -> Destination
        view.handle_key(key(KeyCode::Left)); // Destination -> Model
        view.handle_key(key(KeyCode::Left)); // Model -> Role
        assert_eq!(view.step, Step::Role);
        view.handle_key(key(KeyCode::Down));
        assert!(view.model_draft.is_none());

        to_review(&mut view);
        let action = view.handle_key(key(KeyCode::Char('g')));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, .. }) =
            action
        else {
            panic!("expected fresh deterministic starter");
        };
        assert_eq!(draft.id, "explore");
    }

    /// U09-03: the request generation orders `m` presses but cannot see an
    /// answer changed after the last one. A draft that lands after the user
    /// changed the thinking tier (or role, or route) mid-flight is refused,
    /// never installed onto answers it was not written for.
    #[test]
    fn late_model_draft_is_refused_after_answers_change_in_flight() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        to_review(&mut view);
        let ViewAction::Emit(ViewEvent::FleetProfileModelDraftRequested {
            reasoning_effort, ..
        }) = view.handle_key(key(KeyCode::Char('m')))
        else {
            panic!("expected model draft request");
        };
        // While the draft is in flight the user cycles the thinking tier.
        view.handle_key(key(KeyCode::Char('t')));
        assert!(
            view.install_model_draft(
                sample_draft(),
                "GLM-5.2".to_string(),
                None,
                reasoning_effort.clone()
            )
            .is_none()
        );
        assert!(view.model_draft.is_none());
        let action = view.handle_key(key(KeyCode::Char('g')));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, .. }) =
            action
        else {
            panic!("expected the deterministic starter for the current answers");
        };
        assert_eq!(draft.id, "manager");

        // A draft nobody requested (no `m`) is refused too.
        let mut view = FleetSetupView::from_snapshot(snapshot());
        to_review(&mut view);
        assert!(
            view.install_model_draft(sample_draft(), "GLM-5.2".to_string(), None, None)
                .is_none()
        );
    }

    #[test]
    fn arrows_move_within_step_and_enter_advances() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        assert_eq!(view.step, Step::Role);

        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.role_idx, 1);

        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, Step::Model);

        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.model_idx, 1);

        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, Step::Destination);
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, Step::Review);

        // `t` cycles thinking on the review step without an extra wizard screen.
        view.handle_key(key(KeyCode::Char('t')));
        assert_eq!(view.thinking_idx, 1);

        // Left steps back through the wizard.
        view.handle_key(key(KeyCode::Left));
        assert_eq!(view.step, Step::Destination);
        view.handle_key(key(KeyCode::Left));
        assert_eq!(view.step, Step::Model);
        view.handle_key(key(KeyCode::Left));
        assert_eq!(view.step, Step::Role);
    }

    #[test]
    fn roster_role_handoff_starts_at_model_and_can_return_to_role() {
        let mut via_left = FleetSetupView::from_snapshot_for_role(snapshot(), "consultant");
        assert_eq!(via_left.step, Step::Model);
        assert_eq!(via_left.selected_role(), "advisor");
        assert!(matches!(
            via_left.handle_key(key(KeyCode::Left)),
            ViewAction::None
        ));
        assert_eq!(via_left.step, Step::Role);
        assert_eq!(via_left.selected_role(), "advisor");

        let mut via_esc = FleetSetupView::from_snapshot_for_role(snapshot(), "reviewer");
        assert_eq!(via_esc.step, Step::Model);
        assert_eq!(via_esc.selected_role(), "reviewer");
        assert!(matches!(
            via_esc.handle_key(key(KeyCode::Esc)),
            ViewAction::None
        ));
        assert_eq!(via_esc.step, Step::Role);
        assert_eq!(via_esc.selected_role(), "reviewer");

        let custom = FleetSetupView::from_snapshot_for_role(snapshot(), "domain-expert");
        assert_eq!(custom.step, Step::Model);
        assert_eq!(custom.selected_role(), "custom");
    }

    #[test]
    fn esc_steps_back_then_cancels_from_role() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        view.handle_key(key(KeyCode::Enter)); // -> Model
        let action = view.handle_key(key(KeyCode::Esc));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.step, Step::Role);
        let action = view.handle_key(key(KeyCode::Esc));
        assert!(matches!(action, ViewAction::Close));
    }

    #[test]
    fn mouse_selects_rows_and_wheel_matches_keyboard_navigation() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let (rect, row) = view.row_hitboxes.borrow()[2];

        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(row, 2);
        assert_eq!(view.role_idx, 2);

        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(view.role_idx, 3);
        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(view.role_idx, 2);
    }

    #[test]
    fn compact_choice_window_keeps_deep_selection_visible_and_clickable() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        view.role_idx = ROLES.len() - 1;
        let area = Rect::new(0, 0, 80, 16);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("▸ custom"), "{rendered}");
        assert!(
            view.row_hitboxes
                .borrow()
                .iter()
                .any(|(_, idx)| *idx == ROLES.len() - 1),
            "selected row needs an aligned mouse hitbox"
        );
    }

    /// #3908: destination facts (exists/is_dir) are computed on the
    /// transitions that can change them — never per paint.
    #[test]
    fn review_destinations_are_cached_on_transitions_not_recomputed_per_paint() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        assert!(
            view.destinations.is_none(),
            "nothing is stat-ed before the user reaches the Destination step"
        );

        view.advance(); // Role -> Model
        view.advance(); // Model -> Destination
        assert_eq!(view.step, Step::Destination);
        let on_entry = view
            .destinations
            .clone()
            .expect("entering Destination must populate the cached statuses");
        view.advance(); // Destination -> Review
        assert_eq!(view.step, Step::Review);

        // Painting repeatedly must not change the cached value — that is the
        // whole point — and must not panic on the cached-read path.
        let area = Rect::new(0, 0, 80, 24);
        for _ in 0..3 {
            let mut buf = Buffer::empty(area);
            view.render(area, &mut buf);
        }
        assert_eq!(view.destinations.as_ref(), Some(&on_entry));
    }

    #[test]
    fn destination_status_reports_new_file_replace_and_disabled_reasons() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let personal = Ok(temp.path().join("home-agents"));
        let fresh = destination_status(
            FleetProfileScope::Project,
            temp.path(),
            &personal,
            "reviewer.toml",
            true,
            codewhale_localization::Locale::En,
        );
        assert_eq!(
            fresh.target,
            temp.path().join(PROFILE_DIR).join("reviewer.toml")
        );
        assert!(!fresh.target_exists);
        assert!(fresh.unavailable_reason.is_none());

        let profile_dir = temp.path().join(PROFILE_DIR);
        std::fs::create_dir_all(&profile_dir).expect("profile dir");
        std::fs::write(profile_dir.join("reviewer.toml"), "id = \"reviewer\"\n")
            .expect("existing profile");
        let existing = destination_status(
            FleetProfileScope::Project,
            temp.path(),
            &personal,
            "reviewer.toml",
            true,
            codewhale_localization::Locale::En,
        );
        assert!(
            existing.target_exists,
            "the exact target file is detected, not a dir count"
        );

        let disabled = destination_status(
            FleetProfileScope::Project,
            temp.path(),
            &personal,
            "reviewer.toml",
            false,
            codewhale_localization::Locale::En,
        );
        assert!(
            disabled
                .unavailable_reason
                .as_deref()
                .is_some_and(|r| r.contains("--no-project-config")),
            "{disabled:?}"
        );

        let missing = destination_status(
            FleetProfileScope::Project,
            &temp.path().join("does-not-exist"),
            &personal,
            "reviewer.toml",
            true,
            codewhale_localization::Locale::En,
        );
        assert!(missing.unavailable_reason.is_some(), "{missing:?}");
    }

    #[test]
    fn one_enter_from_review_saves_starter_profile_for_selection() {
        let mut view = FleetSetupView::from_snapshot(snapshot());
        // Role: manager(0) scout(1) builder(2) -> builder.
        view.handle_key(key(KeyCode::Down));
        view.handle_key(key(KeyCode::Down));
        view.handle_key(key(KeyCode::Enter)); // -> Model
        // Model: inherit(0) deepseek-v4-pro(1) -> deepseek-v4-pro.
        view.handle_key(key(KeyCode::Down));
        view.handle_key(key(KeyCode::Enter)); // Model -> Destination
        view.handle_key(key(KeyCode::Enter)); // Destination -> Review
        assert_eq!(view.step, Step::Review);
        while view.selected_reasoning_effort().as_deref() != Some("max") {
            view.handle_key(key(KeyCode::Char('t')));
        }

        // The Review summary is already the structured confirmation surface;
        // one Enter saves the deterministic starter without another state.
        let action = view.handle_key(key(KeyCode::Enter));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, scope }) =
            action
        else {
            panic!("expected one-Enter starter save");
        };
        let content = draft.render_toml();
        assert!(content.contains("id = \"implement\""));
        assert!(content.contains("role_hint = \"implement\""));
        assert!(content.contains("model = \"deepseek-v4-pro\""));
        assert!(content.contains("reasoning_effort = \"max\""));
        // A concrete cross-provider route pin names its own provider
        // explicitly (#4093) — the saved profile must not be ambiguously
        // scoped to whatever provider happens to be active at launch time.
        assert!(content.contains("provider = \"deepseek\""), "{content}");
        for forbidden in ["base_url", "api_key"] {
            assert!(
                !content.contains(forbidden),
                "starter profile must not carry {forbidden}: {content}"
            );
        }

        assert_eq!(scope, FleetProfileScope::Personal);
        assert_eq!(draft.id, "implement");
        assert_eq!(draft.role_hint, "implement");
        assert_eq!(draft.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(draft.provider.as_deref(), Some("deepseek"));
        assert_eq!(draft.reasoning_effort.as_deref(), Some("max"));
    }

    #[test]
    fn review_defaults_to_personal_and_can_switch_to_project() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let mut view = FleetSetupView::from_snapshot(workspace_snapshot(temp.path()));
        to_review(&mut view);

        assert_eq!(view.profile_scope, FleetProfileScope::Personal);
        // `s` is a secondary accelerator back to the Destination step; the
        // destination itself is chosen with a focused control, never toggled
        // silently.
        view.handle_key(key(KeyCode::Char('s')));
        assert_eq!(view.step, Step::Destination);
        assert_eq!(
            view.profile_scope,
            FleetProfileScope::Personal,
            "s alone changes nothing"
        );
        view.handle_key(key(KeyCode::Up)); // This project
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, Step::Review);
        assert_eq!(view.profile_scope, FleetProfileScope::Project);

        let action = view.handle_key(key(KeyCode::Enter));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, scope }) =
            action
        else {
            panic!("expected project profile save event");
        };
        assert_eq!(scope, FleetProfileScope::Project);
        let rendered = draft.render_toml();
        assert!(rendered.contains("id = \"manager\""), "{rendered}");
    }

    #[test]
    fn inherit_selection_starter_draft_carries_no_provider() {
        // `inherit` (no concrete route pin) must never carry a provider —
        // there's no explicit route to name (#4093).
        let mut view = FleetSetupView::from_snapshot(snapshot());
        to_review(&mut view);
        let action = view.handle_key(key(KeyCode::Enter));
        let ViewAction::EmitAndClose(ViewEvent::FleetProfileDraftCommitRequested { draft, .. }) =
            action
        else {
            panic!("expected inherit starter save");
        };
        assert_eq!(draft.model, None);
        assert_eq!(draft.provider, None);
        assert_eq!(draft.reasoning_effort, None);
        let content = draft.render_toml();
        assert!(!content.contains("provider"), "{content}");
        assert!(!content.contains("reasoning_effort"), "{content}");
    }

    #[test]
    fn role_and_review_steps_note_roster_overrides() {
        // "reviewer" collides with the built-in roster member; the
        // role step context and review Role section must both say so.
        let mut view = FleetSetupView::from_snapshot(snapshot());
        for _ in 0..3 {
            view.handle_key(key(KeyCode::Down));
        }
        assert_eq!(view.selected_role(), "reviewer");
        assert_eq!(
            view.roster_override_note().as_deref(),
            Some("Replaces the built-in 'reviewer' role in the Fleet.")
        );

        let role_step = render_through_stack(
            || {
                let mut v = FleetSetupView::from_snapshot(snapshot());
                for _ in 0..3 {
                    v.handle_key(key(KeyCode::Down));
                }
                v
            },
            120,
            40,
        )
        .join("\n");
        assert!(
            contains_wrapped(&role_step, "Replaces the built-in 'reviewer'"),
            "{role_step}"
        );

        let review = render_through_stack(
            || {
                let mut v = FleetSetupView::from_snapshot(snapshot());
                for _ in 0..3 {
                    v.handle_key(key(KeyCode::Down));
                }
                v.step = Step::Review;
                v
            },
            120,
            40,
        )
        .join("\n");
        assert!(
            contains_wrapped(&review, "Replaces the built-in 'reviewer'"),
            "{review}"
        );

        // "custom" also matches a built-in roster member.
        let mut custom_view = FleetSetupView::from_snapshot(snapshot());
        for _ in 0..8 {
            custom_view.handle_key(key(KeyCode::Down));
        }
        assert_eq!(custom_view.selected_role(), "custom");
        assert_eq!(
            custom_view.roster_override_note().as_deref(),
            Some("Replaces the built-in 'custom' role in the Fleet.")
        );
    }

    #[test]
    fn default_selection_targets_manager_inherit() {
        let view = FleetSetupView::from_snapshot(snapshot());
        let draft = view.starter_profile_draft();
        assert_eq!(draft.file_name(), "manager.toml");
        assert_eq!(draft.role_hint, "manager");
        assert!(draft.model.is_none());
        assert!(draft.model_class_hint.is_none());
        assert!(
            draft
                .instructions
                .as_deref()
                .is_some_and(|text| text.contains("assigned Fleet slice"))
        );
    }

    #[test]
    fn fleet_model_rows_keep_failed_provider_visible_with_reason() {
        let mut snap = snapshot();
        snap.available_models = vec![(
            "zai".to_string(),
            "glm-5.2".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedLastCheckFailed {
                category: codewhale_error_taxonomy::ErrorCategory::Authentication,
                message: "auth failed".to_string(),
            },
        )];
        let mut view = FleetSetupView::from_snapshot(snap);
        assert_eq!(view.model_choices.len(), 2);
        assert!(
            view.model_choices[1]
                .summary
                .contains("last check failed (authentication)")
        );
        assert!(view.model_choices[1].summary.contains("auth failed"));
        assert_eq!(
            view.model_routes[1],
            ("zai".to_string(), "glm-5.2".to_string())
        );
        assert!(matches!(
            &view.model_row_states[1],
            FleetModelRowState::Blocked { reason } if reason == "auth failed"
        ));
        view.step = Step::Model;
        view.model_idx = 1;
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::None
        ));
        assert_eq!(view.step, Step::Model);
    }

    #[test]
    fn fleet_invalid_route_stays_visible_but_cannot_advance() {
        let mut snap = snapshot();
        snap.available_models = vec![(
            "zai".to_string(),
            "broken-model".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::InvalidRoute,
        )];
        let mut view = FleetSetupView::from_snapshot(snap);
        view.step = Step::Model;
        view.model_idx = 1;

        assert!(view.model_choices[1].summary.contains("invalid route"));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::None
        ));
        assert_eq!(view.step, Step::Model);
    }

    #[test]
    fn fleet_includes_saved_model_outside_bundled_catalog() {
        let providers = codewhale_runtime::config::ProvidersConfig {
            openrouter: codewhale_runtime::config::ProviderConfig {
                api_key: Some("openrouter-test-key".to_string()),
                model: Some("acme/private-preview".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let config = {
            let __cw_config_init_0_0 = Some("openrouter".to_string());
            let __cw_config_init_0_1 = Some(providers);
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };

        let routes = cross_provider_model_routes(
            &config,
            Some(&(config).test_identity_for_kind(codewhale_config::ProviderKind::Openrouter)),
            &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
        );

        assert!(routes.iter().any(|(provider, model, readiness)| {
            provider == "openrouter" && model == "acme/private-preview" && readiness.can_attempt()
        }));
        assert_eq!(
            routes
                .iter()
                .filter(|(provider, model, _)| {
                    provider == "openrouter" && model == "acme/private-preview"
                })
                .count(),
            1,
            "saved models must not be duplicated when the catalog later learns them"
        );
    }

    #[test]
    fn fleet_routes_and_saved_draft_keep_exact_named_custom_provider() {
        let mut custom = std::collections::HashMap::new();
        for (name, base_url, model) in [
            ("custom-a", "http://127.0.0.1:18181/v1", "model-a"),
            ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
        ] {
            custom.insert(
                name.to_string(),
                codewhale_runtime::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    base_url: Some(base_url.to_string()),
                    model: Some(model.to_string()),
                    api_key: Some("local-test-key".to_string()),
                    ..Default::default()
                },
            );
        }
        let config = {
            let __cw_config_init_1_0 = Some("custom-a".to_string());
            let __cw_config_init_1_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom,
                ..Default::default()
            });
            let mut __cw_config_init_1_value: Config = Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        let routes = cross_provider_model_routes(
            &config,
            Some(&(config).test_identity_for_kind(codewhale_config::ProviderKind::Custom)),
            &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
        );
        assert!(
            routes
                .iter()
                .any(|(provider, model, _)| { provider == "custom-a" && model == "model-a" })
        );
        assert!(
            routes
                .iter()
                .any(|(provider, model, _)| { provider == "custom-b" && model == "model-b" })
        );
        assert!(!routes.iter().any(|(provider, _, _)| provider == "custom"));

        let mut view = FleetSetupView::from_snapshot(FleetSetupSnapshot {
            available_models: routes,
            provider: "custom-a".to_string(),
            model: "model-a".to_string(),
            ..snapshot()
        });
        let route_idx = view
            .model_routes
            .iter()
            .position(|(provider, model)| provider == "custom-b" && model == "model-b")
            .expect("custom B route selectable while A is active");
        let route = view.model_routes[route_idx].clone();
        // Request the draft against the custom B answer, as `m` on Review does.
        view.model_idx = route_idx;
        view.step = Step::Review;
        view.handle_key(key(KeyCode::Char('m')));
        let draft = sample_draft();
        let (_, rendered) = view
            .install_model_draft(draft, "model-b".to_string(), Some(route), None)
            .expect("answers unchanged since `m`");
        assert!(rendered.contains("provider = \"custom-b\""), "{rendered}");
    }

    #[test]
    fn fleet_routes_keep_legacy_literal_custom_without_named_tables() {
        let config = {
            let __cw_config_init_2_0 = Some("custom".to_string());
            let __cw_config_init_2_1 = Some("legacy-custom-model".to_string());
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.default_text_model = __cw_config_init_2_1;
            __cw_config_init_2_value
        }
        .with_legacy_root(
            Some("local-test-key".to_string()),
            Some("http://127.0.0.1:18080/v1".to_string()),
        );

        let routes = cross_provider_model_routes(
            &config,
            Some(&(config).test_identity_for_kind(codewhale_config::ProviderKind::Custom)),
            &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
        );

        assert!(
            routes.iter().any(|(provider, model, readiness)| {
                provider == "custom"
                    && model == "legacy-custom-model"
                    && matches!(
                        readiness,
                        codewhale_engine::provider_readiness::ResolvedProviderReadiness::LocalUnchecked
                    )
                    && readiness.can_attempt()
            }),
            "{routes:?}"
        );
    }

    #[test]
    fn role_step_keeps_list_and_detail_separate_at_80_columns() {
        let rows = render_through_stack(|| FleetSetupView::from_snapshot(snapshot()), 80, 24);
        let text = rows.join("\n");

        let manager_row = rows
            .iter()
            .position(|row| row.contains("▸ manager"))
            .expect("manager row should render");
        let custom_row = rows
            .iter()
            .position(|row| row.contains("  custom"))
            .expect("custom row should render");
        let summary_row = rows
            .iter()
            .position(|row| row.contains("Plan & split queued work"))
            .expect("selected role summary should render");
        let description_row = rows
            .iter()
            .position(|row| row.contains("Coordinates the Fleet run"))
            .expect("selected role description should render");

        assert!(
            manager_row < custom_row,
            "expected the full role list before details:\n{text}"
        );
        assert!(
            custom_row < summary_row,
            "selected summary must not share a row with role names:\n{text}"
        );
        assert!(
            custom_row < description_row,
            "selected description must render below the list:\n{text}"
        );
        for row in &rows[manager_row..=custom_row] {
            assert!(
                !row.contains("Plan & split queued work")
                    && !row.contains("Coordinates the Fleet run")
                    && !row.contains("Fleet runs agents"),
                "role list row contains detail copy at 80 columns: {row:?}\n{text}"
            );
        }
    }

    const BLEED_FILL: &str = "\u{e000}";

    fn render_through_stack(view_at: impl Fn() -> FleetSetupView, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        for y in 0..h {
            for x in 0..w {
                // A private-use glyph that no rendered copy or temp path can
                // contain, so bleed-through detection cannot false-positive
                // on a path like `/Volumes/VIXinSSD/...`.
                buf[(x, y)].set_symbol(BLEED_FILL);
            }
        }
        let mut stack = ViewStack::new();
        stack.push(view_at());
        stack.render(area, &mut buf);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn fleet_setup_is_usable_and_opaque_at_blocker_sizes() {
        // Exercise each step so all three screens are validated at every size.
        type Builder = (&'static str, fn() -> FleetSetupView);
        let builders: [Builder; 3] = [
            ("role", || FleetSetupView::from_snapshot(snapshot())),
            ("model", || {
                let mut v = FleetSetupView::from_snapshot(snapshot());
                v.step = Step::Model;
                v
            }),
            ("review", || {
                let mut v = FleetSetupView::from_snapshot(snapshot());
                v.step = Step::Review;
                v
            }),
        ];

        for (label, make) in builders {
            for (w, h) in BLOCKER_SIZES {
                let rows = render_through_stack(make, w, h);
                let text = rows.join("\n");

                // No bleed-through anywhere in the composited frame.
                assert!(
                    !text.contains(BLEED_FILL),
                    "{label} {w}x{h}: background bleed-through"
                );
                // Some action label is always visible.
                assert!(text.contains("Esc"), "{label} {w}x{h}: missing footer");
                // The first impression communicates Fleet = agent team.
                assert!(
                    text.contains("agent team"),
                    "{label} {w}x{h}: missing framing"
                );
                // No row overflows the frame width.
                for (y, row) in rows.iter().enumerate() {
                    assert!(
                        UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                        "{label} {w}x{h}: row {y} overflows: {row:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn review_at_cursor_size_keeps_content_and_actions_apart() {
        let rows = render_through_stack(
            || {
                let mut view = FleetSetupView::from_snapshot(snapshot());
                view.step = Step::Review;
                view
            },
            89,
            50,
        );
        let popup = centered_modal_area(Rect::new(0, 0, 89, 50), 96, 31, 60, 16);
        let review_row = rows
            .iter()
            .position(|row| row.contains("Review & save"))
            .expect("review heading");
        let review_col = rows[review_row]
            .chars()
            .position(|ch| ch == 'R')
            .expect("review heading column") as u16;
        assert!(
            review_col >= popup.x.saturating_add(2),
            "body copy must not touch the popup border: {:?}",
            rows[review_row]
        );

        let action_row = rows
            .iter()
            .rposition(|row| row.contains("Esc"))
            .expect("footer Esc action");
        let footer_row = rows[..=action_row]
            .iter()
            .rposition(|row| row.contains("scroll"))
            .expect("footer shortcut row");
        assert!(footer_row > 0);
        let gutter = rows[footer_row - 1]
            .chars()
            .skip(usize::from(popup.x.saturating_add(1)))
            .take(usize::from(popup.width.saturating_sub(2)))
            .collect::<String>();
        assert!(
            gutter.trim().is_empty(),
            "review body needs a quiet row before the action rail: {gutter:?}"
        );
    }

    #[test]
    fn choice_steps_at_cursor_size_stay_content_sized() {
        for (step, expected_height) in [(Step::Role, 22usize), (Step::Model, 23usize)] {
            let rows = render_through_stack(
                || {
                    let mut view = FleetSetupView::from_snapshot(snapshot());
                    view.step = step;
                    view
                },
                89,
                50,
            );
            let top = rows
                .iter()
                .position(|row| row.contains("Fleet setup — your agent team"))
                .expect("fleet setup title");
            let bottom = rows
                .iter()
                .rposition(|row| row.contains("Step "))
                .expect("fleet setup step receipt");
            assert_eq!(
                bottom - top + 1,
                expected_height,
                "choice card should follow its content instead of filling the 89x50 frame"
            );
        }
    }

    #[test]
    fn review_lists_model_permissions_tools_and_profile_availability() {
        // Top of the review: the leading sections are visible without scrolling.
        let top = render_through_stack(
            || {
                let mut v = FleetSetupView::from_snapshot(snapshot());
                v.step = Step::Review;
                v
            },
            120,
            40,
        )
        .join("\n");
        for section in [
            "Saves to",
            "Role",
            "Model",
            "Auth & readiness",
            "Permissions",
        ] {
            assert!(top.contains(section), "review missing section: {section}");
        }
        // The destination line names the scope and the exact file; the
        // permission posture stays governed by the sections below it.
        assert!(top.contains("Personal · "), "{top}");
        assert!(top.contains("agents"), "{top}");
        assert!(
            top.contains("can only narrow what the session allows"),
            "{top}"
        );

        // The review is intentionally scrollable; scrolling to the bottom reveals
        // the workspace/org execution policy, review policy, and honest save note.
        let bottom = render_through_stack(
            || {
                let mut v = FleetSetupView::from_snapshot(snapshot());
                v.step = Step::Review;
                v.review_scroll = 999; // clamps to max in render
                v
            },
            120,
            40,
        )
        .join("\n");
        for needle in [
            "Tools",
            "Workspace",
            "Review policy",
            "Save as Personal profile",
        ] {
            assert!(bottom.contains(needle), "scrolled review missing: {needle}");
        }

        let policy = FleetSetupView::from_snapshot(snapshot()).review_policy_summary();
        for truth in [
            "agents in this session",
            "codewhale fleet status",
            ".codewhale/fleet.jsonl",
        ] {
            assert!(policy.contains(truth), "review policy missing: {truth}");
        }
        assert!(
            !policy.contains("inspects the ledger"),
            "the interactive status command must not claim to inspect the durable ledger: {policy}"
        );
    }

    #[test]
    fn dormant_external_consent_row_requires_activation() {
        let mut snap = snapshot();
        snap.available_models = vec![(
            "openai-codex".to_string(),
            "gpt-5.6-sol".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::ExternalConsentPendingSelection,
        )];
        let view = FleetSetupView::from_snapshot(snap);
        assert!(
            view.model_choices[1]
                .summary
                .contains("external consent · select to check")
        );
        assert!(matches!(
            view.model_row_states[1],
            FleetModelRowState::NeedsActivation
        ));
    }

    #[test]
    fn enter_on_dormant_external_consent_emits_activation_event() {
        let mut snap = snapshot();
        snap.available_models = vec![(
            "openai-codex".to_string(),
            "gpt-5.6-terra".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::ExternalConsentPendingSelection,
        )];
        let mut view = FleetSetupView::from_snapshot(snap);
        view.handle_key(key(KeyCode::Enter)); // Role -> Model
        view.handle_key(key(KeyCode::Down)); // inherit -> codex row
        assert_eq!(
            view.selected_route(),
            Some(("openai-codex".to_string(), "gpt-5.6-terra".to_string()))
        );
        let action = view.handle_key(key(KeyCode::Enter));
        let ViewAction::Emit(ViewEvent::FleetSetupExternalConsentActivationRequested {
            provider_id,
            model,
        }) = action
        else {
            panic!("expected external-consent activation request, got {action:?}");
        };
        assert_eq!(provider_id, "openai-codex");
        assert_eq!(model, "gpt-5.6-terra");
        assert_eq!(
            view.step,
            Step::Model,
            "stays on Model step until host validates"
        );
    }

    #[test]
    fn refresh_from_snapshot_makes_activated_row_ready() {
        let mut snap = snapshot();
        snap.available_models = vec![(
            "xai".to_string(),
            "grok-4.5".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::ExternalConsentPendingSelection,
        )];
        let mut view = FleetSetupView::from_snapshot(snap);
        view.handle_key(key(KeyCode::Enter)); // Role -> Model
        view.handle_key(key(KeyCode::Down)); // xai row
        assert!(matches!(
            view.model_row_states[1],
            FleetModelRowState::NeedsActivation
        ));

        // Simulate the host validating the route and rebuilding the snapshot:
        // the same row is now Ready.
        let mut refreshed = snapshot();
        refreshed.available_models = vec![(
            "xai".to_string(),
            "grok-4.5".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::Ready,
        )];
        view.refresh_from_snapshot(refreshed);

        assert!(matches!(
            view.model_row_states[1],
            FleetModelRowState::Ready
        ));
        // Selection and step are preserved.
        assert_eq!(view.step, Step::Model);
        assert_eq!(
            view.selected_route(),
            Some(("xai".to_string(), "grok-4.5".to_string()))
        );
    }

    #[test]
    fn blocked_row_cannot_advance() {
        let mut snap = snapshot();
        snap.available_models = vec![(
            "xai".to_string(),
            "grok-4.5".to_string(),
            codewhale_engine::provider_readiness::ResolvedProviderReadiness::MissingKey,
        )];
        let mut view = FleetSetupView::from_snapshot(snap);
        view.step = Step::Model;
        view.model_idx = 1;
        assert!(matches!(
            &view.model_row_states[1],
            FleetModelRowState::Blocked { reason } if reason == "missing API key"
        ));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::None
        ));
        assert_eq!(view.step, Step::Model);
    }

    #[test]
    fn fleet_setup_includes_openai_codex_account_roster_with_dormant_consent() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("owned ChatGPT home");
        let canonical_home = home
            .path()
            .canonicalize()
            .expect("canonical private fixture home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &canonical_home);
        let mut config = codewhale_runtime::config::Config::default();
        codewhale_runtime::oauth::install_test_chatgpt_registration(&mut config)
            .expect("owned ChatGPT registration");
        codewhale_runtime::codex_model_roster::install_test_chatgpt_roster(
            &config,
            &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"],
        )
        .expect("account-scoped roster");

        let routes = cross_provider_model_routes(
            &config,
            Some(&(config).test_identity_for_kind(codewhale_config::ProviderKind::Moonshot)),
            &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
        );

        for model in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            assert!(
                routes.iter().any(|(provider, m, readiness)| {
                    provider == "openai-codex"
                        && m == model
                        && matches!(
                            readiness,
                            codewhale_engine::provider_readiness::ResolvedProviderReadiness::SavedUnchecked
                        )
                }),
                "missing unchecked owned ChatGPT route for {model}: {routes:?}"
            );
        }
    }

    #[test]
    fn fleet_setup_includes_xai_grok_routes_with_dormant_consent() {
        let _env = crate::test_support::lock_test_env();
        let grok_home = tempfile::tempdir().expect("Grok home");
        let mut config = codewhale_runtime::config::Config::default();
        config.providers = Some(codewhale_runtime::config::ProvidersConfig {
            xai: codewhale_runtime::config::ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                external_credentials: Some(
                    codewhale_config::ExternalCredentialConsentToml::read_only(
                        codewhale_config::ProviderKind::Xai,
                        codewhale_config::ExternalCredentialSource::GrokCli,
                        grok_home.path().join("grok-auth.json"),
                    ),
                ),
                ..Default::default()
            },
            ..Default::default()
        });

        let routes = cross_provider_model_routes(
            &config,
            Some(&(config).test_identity_for_kind(codewhale_config::ProviderKind::Moonshot)),
            &codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default(),
        );

        let xai_rows: Vec<_> = routes
            .iter()
            .filter(|(provider, _, _)| provider == "xai")
            .collect();
        assert!(
            !xai_rows.is_empty(),
            "xAI routes must be offered when Grok CLI consent is configured: {routes:?}"
        );
        assert!(
            xai_rows.iter().all(|(_, _, readiness)| {
                matches!(
                    readiness,
                    codewhale_engine::provider_readiness::ResolvedProviderReadiness::ExternalConsentPendingSelection
                )
            }),
            "every xAI row must require explicit activation: {xai_rows:?}"
        );
    }
}
