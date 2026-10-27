//! The fleet as a list of models (design MODEL-ROUTING-CATALOG §10, F1).
//!
//! A person builds their fleet by adding models from the providers they have
//! configured; the operator model later picks sub-agent routes from that list
//! only. The store is the selected fleet file (`fleet/store.rs`): its operator
//! route and every member that pins an exact `provider` + `model`. Nothing
//! here invents a second member store. Shortlist rows carry no role; the
//! roles a model fills are the executable member rows that pin it.

use std::path::Path;

use super::store::{
    FleetFile, FleetMember, FleetScope, FleetStoreError, load_fleet_at, load_fleet_in_scope,
    member_pins, provider_ids_match, resolve_selected_fleet, save_fleet, set_selected, slugify,
};
use codewhale_agent_contract::role::public_role_label;
use codewhale_localization::{Locale, MessageId, tr};

/// Default name for the personal fleet created by the first `/fleet add` or
/// ⇧F on a row in `/model` when no fleet is selected yet.
pub const DEFAULT_FLEET_NAME: &str = "My fleet";

/// One model in the fleet: an exact route plus the roles that pin it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetModel {
    /// Exact provider id (a `[providers.<id>]` key or a built-in id).
    pub provider: String,
    /// Exact model id on that provider's route.
    pub model: String,
    /// Roles whose member rows pin this route; `operator` for the fleet's
    /// own route. Empty when the model was added without a role.
    pub roles: Vec<String>,
    /// The fleet this model belongs to.
    pub fleet: String,
}

impl FleetModel {
    /// Model wire IDs and custom provider keys are exact. Only known built-in
    /// provider aliases are interchangeable for membership and row markers.
    #[must_use]
    pub fn matches(&self, provider: &str, model: &str) -> bool {
        provider_ids_match(&self.provider, provider) && self.model == model.trim()
    }

    /// `roles` joined for a one-line label, or `member` when none.
    #[must_use]
    pub fn roles_label(&self) -> String {
        if self.roles.is_empty() {
            "member".to_string()
        } else {
            self.roles
                .iter()
                .map(|role| public_role_label(role))
                .collect::<Vec<_>>()
                .join(" · ")
        }
    }
}

/// Why the route's membership remains unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnchangedReason {
    /// The route is the fleet's operator route (your current model while
    /// this fleet is selected) and has no member row of its own; `⇧F` and a
    /// role-less `/fleet add` leave it alone instead of duplicating it.
    OperatorRoute,
    /// Every requested role already pins the route.
    AlreadyPresent,
}

/// What a membership change did, for the receipt line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetModelChange {
    Added {
        fleet: String,
        /// The default personal fleet was created for this add.
        created_fleet: bool,
        /// The fleet was not selected before this add and is now.
        selected_fleet: bool,
        roles: Vec<String>,
    },
    Removed {
        fleet: String,
        roles: Vec<String>,
    },
    /// Route membership is unchanged. A redundant shortlist row may have
    /// been removed while a saved role or operator still pins the route.
    Unchanged {
        fleet: String,
        reason: UnchangedReason,
    },
}

/// Why a membership change could not be made. Typed so the surface that
/// shows it picks the person's locale; `Display` is the English form for logs.
#[derive(Debug)]
pub enum FleetModelError {
    /// Provider id or model id was blank.
    NeedsRoute,
    /// No fleet is selected, so there is nothing to remove from.
    NoSelection,
    /// The route is the fleet's operator route, which `/fleet save` changes.
    OperatorRoute { route: String, fleet: String },
    /// No member row pins the route.
    NotInFleet { route: String, fleet: String },
    /// The selected fleet could not be resolved, read, parsed, or written.
    Store(FleetStoreError),
}

impl From<FleetStoreError> for FleetModelError {
    fn from(error: FleetStoreError) -> Self {
        Self::Store(error)
    }
}

impl FleetModelError {
    /// The person-facing explanation in `locale`.
    #[must_use]
    pub fn message(&self, locale: Locale) -> String {
        match self {
            Self::NeedsRoute => tr(locale, MessageId::FleetModelErrorNeedsRoute).into_owned(),
            Self::NoSelection => tr(locale, MessageId::FleetModelErrorNoSelection).into_owned(),
            Self::OperatorRoute { route, fleet } => {
                tr(locale, MessageId::FleetModelErrorOperatorRoute)
                    .replace("{route}", route)
                    .replace("{fleet}", fleet)
            }
            Self::NotInFleet { route, fleet } => tr(locale, MessageId::FleetModelErrorNotInFleet)
                .replace("{route}", route)
                .replace("{fleet}", fleet),
            Self::Store(error) => error.to_string(),
        }
    }
}

impl std::fmt::Display for FleetModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message(Locale::En))
    }
}

impl std::error::Error for FleetModelError {}

/// The selected fleet's models, operator first, then members in file order.
/// Members that inherit the session route (no pin) are not models of their
/// own: when the fleet has an operator their role is attributed to that
/// route — the route they actually resolve to (#6037) — and without an
/// operator they are skipped.
///
/// `Ok(empty)` means no fleet is selected (or the selected one has no pinned
/// route): the caller states "your fleet is the session model only". `Err`
/// means a fleet *is* selected but cannot be resolved or read — a broken
/// explicit selection, which must never be shown as "no selection".
pub fn fleet_models(workspace: &Path) -> Result<Vec<FleetModel>, FleetStoreError> {
    let Some(selected) = resolve_selected_fleet(workspace)? else {
        return Ok(Vec::new());
    };
    let (fleet, _scope) = load_fleet_at(&selected.path)?;
    Ok(models_of(&fleet))
}

/// Project a fleet file into its models with roles unioned per exact route.
#[must_use]
pub fn models_of(fleet: &FleetFile) -> Vec<FleetModel> {
    let mut models: Vec<FleetModel> = Vec::new();
    let mut push = |provider: &str, model: &str, role: Option<&str>| {
        let role = role.map(str::trim).filter(|r| !r.is_empty());
        if let Some(existing) = models.iter_mut().find(|m| m.matches(provider, model)) {
            if let Some(role) = role
                && !existing.roles.iter().any(|r| r.eq_ignore_ascii_case(role))
            {
                existing.roles.push(role.to_string());
            }
            return;
        }
        models.push(FleetModel {
            provider: provider.trim().to_string(),
            model: model.trim().to_string(),
            roles: role.map(|r| vec![r.to_string()]).unwrap_or_default(),
            fleet: fleet.name.clone(),
        });
    };
    if let Some(operator) = fleet.operator.as_ref() {
        push(&operator.provider, &operator.model, Some("operator"));
    }
    for member in &fleet.members {
        match (member.provider.as_deref(), member.model.as_deref()) {
            (Some(provider), Some(model)) => {
                push(provider, model, Some(member.role_label()));
            }
            // Unpinned = inherit the operator route; the member's role is
            // covered by that route and follows it when the operator moves.
            // With no operator the route is the live session's, which this
            // static projection cannot name — the member stays unlisted.
            (None, None) if !member.shortlist => {
                if let Some(operator) = fleet.operator.as_ref() {
                    push(
                        &operator.provider,
                        &operator.model,
                        Some(member.role_label()),
                    );
                }
            }
            _ => {}
        }
    }
    models
}

/// Add `provider/model` to the selected fleet, one member row per role, or
/// one explicitly marked shortlist row when no role was requested.
///
/// With no fleet selected, the personal [`DEFAULT_FLEET_NAME`] fleet is used —
/// loaded when it already exists, created otherwise — and selected only once
/// the add has been written. Roles are deduplicated; when every requested
/// role already pins the route nothing is rewritten and the change is
/// [`FleetModelChange::Unchanged`].
///
/// A role member asked to run the fleet's own operator route inherits that
/// route instead of pinning it (#6037): the pin would resolve identically
/// today and then hold the member on a retired id when the operator moves.
pub fn add_fleet_model(
    workspace: &Path,
    provider: &str,
    model: &str,
    roles: &[String],
) -> Result<FleetModelChange, FleetModelError> {
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return Err(FleetModelError::NeedsRoute);
    }
    let target = selected_or_default(workspace)?;
    let SelectedOrDefault {
        mut fleet,
        scope,
        created_fleet,
        needs_select,
    } = target;
    let mut unique: Vec<String> = Vec::with_capacity(roles.len());
    for role in roles.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        if !unique.iter().any(|seen| seen.eq_ignore_ascii_case(role)) {
            unique.push(role.to_string());
        }
    }
    let roles = unique;
    let shortlist = roles.is_empty();
    let select = |needs_select: bool| -> Result<(), FleetModelError> {
        if needs_select {
            set_selected(DEFAULT_FLEET_NAME, FleetScope::Personal, workspace)?;
        }
        Ok(())
    };
    let on_operator_route = is_operator_route(&fleet, provider, model);
    if shortlist && on_operator_route {
        select(needs_select)?;
        return Ok(FleetModelChange::Unchanged {
            fleet: fleet.name,
            reason: UnchangedReason::OperatorRoute,
        });
    }
    // #6037: the pin is the deliberate opt-out, not the default. A role
    // member asked to run the fleet's own operator route inherits instead —
    // it resolves to that route today and still follows when the operator
    // moves. A role asked to run a different route keeps the explicit pin;
    // a shortlist row always pins (the pin is its entire content).
    let pins_route = shortlist || !on_operator_route;
    let model_slug = slugify(model);
    let mut added_any = false;
    for role in roles
        .iter()
        .map(String::as_str)
        .chain(shortlist.then_some(""))
    {
        let already = fleet.members.iter().any(|m| {
            if shortlist {
                return member_pins(m, provider, model);
            }
            !m.shortlist
                && m.role_label().eq_ignore_ascii_case(role)
                && (member_pins(m, provider, model) || (!pins_route && m.model.is_none()))
        });
        if already {
            continue;
        }
        let base = if shortlist {
            model_slug.clone()
        } else {
            slugify(role)
        };
        let id = unique_member_id(&fleet, &base, &model_slug);
        fleet.members.push(FleetMember {
            id,
            display_name: None,
            shortlist,
            role: role.to_string(),
            model: pins_route.then(|| model.to_string()),
            provider: pins_route.then(|| provider.to_string()),
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        });
        added_any = true;
    }
    if !added_any {
        select(needs_select)?;
        return Ok(FleetModelChange::Unchanged {
            fleet: fleet.name,
            reason: UnchangedReason::AlreadyPresent,
        });
    }
    save_fleet(&fleet, scope, workspace)?;
    select(needs_select)?;
    Ok(FleetModelChange::Added {
        fleet: fleet.name,
        created_fleet,
        selected_fleet: needs_select,
        roles,
    })
}

/// Remove every member row pinning `provider/model` from the selected fleet.
/// The operator route is not a member; it is changed with `/fleet save`.
pub fn remove_fleet_model(
    workspace: &Path,
    provider: &str,
    model: &str,
) -> Result<FleetModelChange, FleetModelError> {
    let provider = provider.trim();
    let model = model.trim();
    let Some(selected) = resolve_selected_fleet(workspace)? else {
        return Err(FleetModelError::NoSelection);
    };
    let (mut fleet, scope) = load_fleet_at(&selected.path)?;
    let before = fleet.members.len();
    let mut roles = Vec::new();
    fleet.members.retain(|m| {
        let hit = member_pins(m, provider, model);
        if hit && !m.role.trim().is_empty() {
            roles.push(m.role.clone());
        }
        !hit
    });
    if fleet.members.len() == before {
        let route = format!("{provider}/{model}");
        return Err(if is_operator_route(&fleet, provider, model) {
            FleetModelError::OperatorRoute {
                route,
                fleet: fleet.name,
            }
        } else {
            FleetModelError::NotInFleet {
                route,
                fleet: fleet.name,
            }
        });
    }
    save_fleet(&fleet, scope, workspace)?;
    Ok(FleetModelChange::Removed {
        fleet: fleet.name,
        roles,
    })
}

/// Add when absent, remove when present — the picker's one-key toggle.
///
/// The toggle edits only explicitly marked shortlist rows. Saved role pins
/// and the operator route remain authoritative and are never removed here.
pub fn toggle_fleet_model(
    workspace: &Path,
    provider: &str,
    model: &str,
) -> Result<FleetModelChange, FleetModelError> {
    let provider = provider.trim();
    let model = model.trim();
    let Some(selected) = resolve_selected_fleet(workspace)? else {
        return add_fleet_model(workspace, provider, model, &[]);
    };
    let (mut fleet, scope) = load_fleet_at(&selected.path)?;
    let before = fleet.members.len();
    fleet
        .members
        .retain(|member| !member.shortlist || !member_pins(member, provider, model));
    let removed_shortlist = fleet.members.len() != before;
    if removed_shortlist {
        save_fleet(&fleet, scope, workspace)?;
    }
    let on_operator_route = is_operator_route(&fleet, provider, model);
    if fleet.members.iter().any(|m| {
        member_pins(m, provider, model)
            // #6037: an unpinned role member inherits the operator route, so
            // it still covers this route even though nothing pins it.
            || (on_operator_route && !m.shortlist && m.model.is_none())
    }) {
        return Ok(FleetModelChange::Unchanged {
            fleet: fleet.name,
            reason: UnchangedReason::AlreadyPresent,
        });
    }
    if on_operator_route {
        return Ok(FleetModelChange::Unchanged {
            fleet: fleet.name,
            reason: UnchangedReason::OperatorRoute,
        });
    }
    if removed_shortlist {
        return Ok(FleetModelChange::Removed {
            fleet: fleet.name,
            roles: Vec::new(),
        });
    }
    add_fleet_model(workspace, provider, model, &[])
}

fn is_operator_route(fleet: &FleetFile, provider: &str, model: &str) -> bool {
    fleet
        .operator
        .as_ref()
        .is_some_and(|op| provider_ids_match(&op.provider, provider) && op.model == model.trim())
}

/// One-line receipt for a membership change, shared by `/fleet add|remove`
/// and the picker's `⇧F`.
#[must_use]
pub fn change_receipt(
    locale: Locale,
    provider: &str,
    model: &str,
    change: &FleetModelChange,
) -> String {
    let route = format!("{}/{}", provider.trim(), model.trim());
    match change {
        FleetModelChange::Added {
            fleet,
            created_fleet,
            selected_fleet,
            roles,
        } => {
            let line = if roles.is_empty() {
                tr(locale, MessageId::FleetModelAdded).replace("{route}", &route)
            } else {
                tr(locale, MessageId::FleetModelAddedAs)
                    .replace("{route}", &route)
                    .replace("{roles}", &roles.join(", "))
            };
            let note = if *created_fleet {
                tr(locale, MessageId::FleetModelAddedCreatedNote)
            } else if *selected_fleet {
                tr(locale, MessageId::FleetModelAddedSelectedNote)
            } else {
                std::borrow::Cow::Borrowed("")
            };
            format!("{}{note}", line.replace("{fleet}", fleet))
        }
        FleetModelChange::Removed { fleet, roles } => {
            let line = if roles.is_empty() {
                tr(locale, MessageId::FleetModelRemoved).replace("{route}", &route)
            } else {
                tr(locale, MessageId::FleetModelRemovedRoles)
                    .replace("{route}", &route)
                    .replace("{roles}", &roles.join(", "))
            };
            line.replace("{fleet}", fleet)
        }
        FleetModelChange::Unchanged { fleet, reason } => {
            let reason = match reason {
                UnchangedReason::OperatorRoute => {
                    tr(locale, MessageId::FleetModelReasonOperatorRoute)
                }
                UnchangedReason::AlreadyPresent => {
                    tr(locale, MessageId::FleetModelReasonAlreadyPresent)
                }
            };
            tr(locale, MessageId::FleetModelUnchanged)
                .replace("{route}", &route)
                .replace("{fleet}", fleet)
                .replace("{reason}", &reason)
        }
    }
}

struct SelectedOrDefault {
    fleet: FleetFile,
    scope: FleetScope,
    /// The default personal fleet did not exist and was built in memory; it
    /// reaches disk only with the first successful add.
    created_fleet: bool,
    /// Select the default personal fleet once the add has succeeded.
    needs_select: bool,
}

/// The selected fleet, or — with nothing selected — the personal default
/// fleet: loaded when it already exists (never overwritten with an empty
/// one), built in memory otherwise. Nothing is written or selected here.
fn selected_or_default(workspace: &Path) -> Result<SelectedOrDefault, FleetStoreError> {
    if let Some(selected) = resolve_selected_fleet(workspace)? {
        let (fleet, scope) = load_fleet_at(&selected.path)?;
        return Ok(SelectedOrDefault {
            fleet,
            scope,
            created_fleet: false,
            needs_select: false,
        });
    }
    match load_fleet_in_scope(DEFAULT_FLEET_NAME, FleetScope::Personal, workspace) {
        Ok((fleet, _path)) => Ok(SelectedOrDefault {
            fleet,
            scope: FleetScope::Personal,
            created_fleet: false,
            needs_select: true,
        }),
        Err(FleetStoreError::NotFound(_)) => Ok(SelectedOrDefault {
            fleet: FleetFile::new(
                DEFAULT_FLEET_NAME.to_string(),
                Some("Models added from /models and /fleet add.".to_string()),
            )?,
            scope: FleetScope::Personal,
            created_fleet: true,
            needs_select: true,
        }),
        Err(error) => Err(error),
    }
}

pub(crate) fn unique_member_id(fleet: &FleetFile, base: &str, model_slug: &str) -> String {
    let taken = |id: &str| fleet.members.iter().any(|m| m.id.eq_ignore_ascii_case(id));
    if !taken(base) {
        return base.to_string();
    }
    let with_model = format!("{base}-{model_slug}");
    if !taken(&with_model) {
        return with_model;
    }
    (2..)
        .map(|n| format!("{with_model}-{n}"))
        .find(|candidate| !taken(candidate))
        .expect("an unbounded counter yields a free id")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::store::FleetOperator;
    use crate::fleet::store::{save_fleet, set_selected};

    fn fleet_with(operator: Option<(&str, &str)>, members: &[(&str, &str, &str)]) -> FleetFile {
        let mut fleet = FleetFile::new("Test".to_string(), None).expect("valid");
        fleet.operator = operator.map(|(p, m)| FleetOperator {
            provider: p.to_string(),
            model: m.to_string(),
            reasoning: None,
        });
        for (id, role, model) in members {
            fleet.members.push(FleetMember {
                id: (*id).to_string(),
                display_name: None,
                shortlist: false,
                role: (*role).to_string(),
                model: Some((*model).to_string()),
                provider: Some("openrouter".to_string()),
                reasoning: None,
                instructions: None,
                requires: Vec::new(),
            });
        }
        fleet
    }

    /// An isolated `CODEWHALE_HOME` and workspace; the guard must outlive
    /// the test body.
    fn isolated_workspace() -> (
        tempfile::TempDir,
        crate::test_support::EnvVarGuard,
        std::path::PathBuf,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let guard = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let workspace = temp.path().join("repo");
        std::fs::create_dir_all(&workspace).expect("workspace");
        (temp, guard, workspace)
    }

    fn selected_file(workspace: &Path) -> (FleetFile, std::path::PathBuf) {
        let selected = resolve_selected_fleet(workspace)
            .expect("ok")
            .expect("selected");
        let (fleet, _) = load_fleet_at(&selected.path).expect("load");
        (fleet, selected.path)
    }

    #[test]
    fn models_of_unions_roles_per_exact_route_and_puts_the_operator_first() {
        let fleet = fleet_with(
            Some(("openrouter", "z-ai/glm-5.3")),
            &[
                ("scout", "scout", "z-ai/glm-5.3-flash"),
                ("reviewer", "reviewer", "deepseek/deepseek-v4-flash"),
                ("verifier", "verifier", "z-ai/glm-5.3-flash"),
                ("planner", "planner", "z-ai/glm-5.3"),
            ],
        );
        let models = models_of(&fleet);
        let ids: Vec<_> = models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(
            ids,
            [
                "z-ai/glm-5.3",
                "z-ai/glm-5.3-flash",
                "deepseek/deepseek-v4-flash"
            ]
        );
        assert_eq!(models[0].roles, ["operator", "planner"]);
        assert_eq!(models[1].roles, ["scout", "verifier"]);
        assert_eq!(models[1].roles_label(), "explore · test");
    }

    #[test]
    fn case_distinct_custom_providers_with_one_model_remain_separate() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let model = "shared-wire-model";
        for provider in ["TeamA", "teama"] {
            for roles in [Vec::new(), vec!["reviewer".into()]] {
                assert!(matches!(
                    add_fleet_model(&workspace, provider, model, &roles).unwrap(),
                    FleetModelChange::Added { .. }
                ));
            }
        }
        let (before, path) = selected_file(&workspace);
        assert_eq!(before.members.len(), 4);
        let lower_rows = before
            .members
            .iter()
            .filter(|member| member.provider.as_deref() == Some("teama"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(lower_rows.len(), 2);
        let models = fleet_models(&workspace).unwrap();
        assert_eq!(
            models.len(),
            2,
            "both endpoints must remain available to routing"
        );
        assert_eq!(models[0].provider, "TeamA");
        assert_eq!(models[1].provider, "teama");
        for (index, provider) in ["TeamA", "teama"].into_iter().enumerate() {
            assert!(models[index].matches(provider, model));
            assert!(!models[1 - index].matches(provider, model));
            assert_eq!(models[index].roles, ["reviewer"]);
        }
        let bytes = std::fs::read(&path).unwrap();
        assert!(matches!(
            remove_fleet_model(&workspace, "TEAMA", model),
            Err(FleetModelError::NotInFleet { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        // Toggle only the upper provider's choice; its role and both lower
        // provider rows remain. Removal then removes only that upper role.
        assert!(matches!(
            toggle_fleet_model(&workspace, "TeamA", model).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::AlreadyPresent,
                ..
            }
        ));
        let after_toggle = selected_file(&workspace).0;
        assert_eq!(after_toggle.members.len(), 3);
        assert_eq!(
            after_toggle
                .members
                .iter()
                .filter(|member| member.provider.as_deref() == Some("teama"))
                .cloned()
                .collect::<Vec<_>>(),
            lower_rows
        );
        assert!(matches!(
            remove_fleet_model(&workspace, "TeamA", model).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0.members, lower_rows);
        assert!(matches!(
            toggle_fleet_model(&workspace, "TeamA", model).unwrap(),
            FleetModelChange::Added { .. }
        ));
        assert!(matches!(
            toggle_fleet_model(&workspace, "TeamA", model).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0.members, lower_rows);
    }

    #[test]
    fn custom_provider_case_does_not_alias_the_operator_with_the_same_model() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let model = "shared-wire-model";
        let fleet = fleet_with(Some(("TeamA", model)), &[]);
        save_fleet(&fleet, FleetScope::Workspace, &workspace).unwrap();
        set_selected(&fleet.name, FleetScope::Workspace, &workspace).unwrap();
        assert!(matches!(
            add_fleet_model(&workspace, "teama", model, &[]).unwrap(),
            FleetModelChange::Added { .. }
        ));
        let models = fleet_models(&workspace).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].roles, ["operator"]);
        assert!(models[1].roles.is_empty());
        assert!(matches!(
            toggle_fleet_model(&workspace, "teama", model).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0, fleet);
        assert!(matches!(
            add_fleet_model(&workspace, "TeamA", model, &[]).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::OperatorRoute,
                ..
            }
        ));
    }

    #[test]
    fn built_in_provider_aliases_share_membership_and_operator_pins() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let fleet = fleet_with(Some(("meta", "muse-spark-1.3")), &[]);
        save_fleet(&fleet, FleetScope::Workspace, &workspace).unwrap();
        set_selected(&fleet.name, FleetScope::Workspace, &workspace).unwrap();
        assert!(matches!(
            add_fleet_model(&workspace, "MUSE", "muse-spark-1.3", &[]).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::OperatorRoute,
                ..
            }
        ));
        assert!(matches!(
            add_fleet_model(&workspace, "meta", "muse-spark-1.2", &[]).unwrap(),
            FleetModelChange::Added { .. }
        ));
        let (_, path) = selected_file(&workspace);
        let bytes = std::fs::read(&path).unwrap();
        assert!(matches!(
            add_fleet_model(&workspace, "muse", "muse-spark-1.2", &[]).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::AlreadyPresent,
                ..
            }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let models = fleet_models(&workspace).unwrap();
        assert_eq!(models.len(), 2);
        assert!(models[1].matches("MUSE", "muse-spark-1.2"));
        assert!(matches!(
            toggle_fleet_model(&workspace, "MUSE", "muse-spark-1.2").unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0, fleet);
    }

    #[test]
    fn case_distinct_saved_models_survive_add_remove_and_toggle() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let upper = "Preview-fixture";
        let lower = "preview-fixture";
        for model in [upper, lower] {
            assert!(matches!(
                add_fleet_model(&workspace, "openrouter", model, &[]).unwrap(),
                FleetModelChange::Added { .. }
            ));
            assert!(matches!(
                add_fleet_model(&workspace, "openrouter", model, &["reviewer".into()]).unwrap(),
                FleetModelChange::Added { .. }
            ));
        }
        let (before, path) = selected_file(&workspace);
        assert_eq!(before.members.len(), 4);
        let upper_rows = before
            .members
            .iter()
            .filter(|member| member.model.as_deref() == Some(upper))
            .cloned()
            .collect::<Vec<_>>();
        let lower_rows = before
            .members
            .iter()
            .filter(|member| member.model.as_deref() == Some(lower))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(upper_rows.len(), 2);
        assert_eq!(lower_rows.len(), 2);
        let models = fleet_models(&workspace).unwrap();
        assert_eq!(models.len(), 2);
        for model in [upper, lower] {
            assert_eq!(
                models
                    .iter()
                    .filter(|row| row.matches("OPENROUTER", model))
                    .count(),
                1,
                "only the exact saved model gets the active marker"
            );
        }
        let bytes = std::fs::read(&path).unwrap();
        assert!(matches!(
            add_fleet_model(&workspace, "OPENROUTER", upper, &["reviewer".into()]).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::AlreadyPresent,
                ..
            }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(matches!(
            remove_fleet_model(&workspace, "openrouter", "PREVIEW-FIXTURE"),
            Err(FleetModelError::NotInFleet { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        // Toggle removes only the exact shortlist row and retains its role pin.
        assert!(matches!(
            toggle_fleet_model(&workspace, "OPENROUTER", lower).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::AlreadyPresent,
                ..
            }
        ));
        let (after_toggle, _) = selected_file(&workspace);
        assert_eq!(
            after_toggle
                .members
                .iter()
                .filter(|member| member.model.as_deref() == Some(upper))
                .cloned()
                .collect::<Vec<_>>(),
            upper_rows
        );
        assert_eq!(
            after_toggle
                .members
                .iter()
                .filter(|member| member.model.as_deref() == Some(lower))
                .cloned()
                .collect::<Vec<_>>(),
            lower_rows
                .into_iter()
                .filter(|member| !member.shortlist)
                .collect::<Vec<_>>()
        );
        assert!(matches!(
            remove_fleet_model(&workspace, "openrouter", lower).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0.members, upper_rows);

        // Toggling the absent lower spelling adds and removes only that choice.
        assert!(matches!(
            toggle_fleet_model(&workspace, "openrouter", lower).unwrap(),
            FleetModelChange::Added { .. }
        ));
        assert_eq!(
            selected_file(&workspace).0.members.len(),
            upper_rows.len() + 1
        );
        assert!(matches!(
            toggle_fleet_model(&workspace, "openrouter", lower).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0.members, upper_rows);
        assert!(matches!(
            remove_fleet_model(&workspace, "openrouter", upper).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert!(selected_file(&workspace).0.members.is_empty());
    }

    #[test]
    fn case_distinct_member_does_not_alias_the_saved_operator() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let upper = "Preview-fixture";
        let lower = "preview-fixture";
        let fleet = fleet_with(Some(("openrouter", upper)), &[]);
        save_fleet(&fleet, FleetScope::Workspace, &workspace).unwrap();
        set_selected(&fleet.name, FleetScope::Workspace, &workspace).unwrap();
        assert!(matches!(
            add_fleet_model(&workspace, "openrouter", lower, &[]).unwrap(),
            FleetModelChange::Added { .. }
        ));
        assert!(matches!(
            add_fleet_model(&workspace, "OPENROUTER", upper, &[]).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::OperatorRoute,
                ..
            }
        ));
        let models = fleet_models(&workspace).unwrap();
        assert_eq!(models.len(), 2);
        assert!(models[0].matches("OPENROUTER", upper));
        assert!(!models[1].matches("openrouter", upper));
        assert!(matches!(
            remove_fleet_model(&workspace, "openrouter", lower).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0.operator, fleet.operator);
        assert!(matches!(
            toggle_fleet_model(&workspace, "openrouter", lower).unwrap(),
            FleetModelChange::Added { .. }
        ));
        assert!(matches!(
            toggle_fleet_model(&workspace, "openrouter", lower).unwrap(),
            FleetModelChange::Removed { .. }
        ));
        assert_eq!(selected_file(&workspace).0, fleet);
        let (_, path) = selected_file(&workspace);
        let bytes = std::fs::read(&path).unwrap();
        assert!(matches!(
            toggle_fleet_model(&workspace, "openrouter", upper).unwrap(),
            FleetModelChange::Unchanged {
                reason: UnchangedReason::OperatorRoute,
                ..
            }
        ));
        assert!(matches!(
            remove_fleet_model(&workspace, "openrouter", upper),
            Err(FleetModelError::OperatorRoute { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn inheriting_members_are_not_models_of_their_own() {
        let mut fleet = fleet_with(None, &[]);
        fleet.members.push(FleetMember {
            id: "builder".to_string(),
            display_name: None,
            shortlist: false,
            role: "builder".to_string(),
            model: None,
            provider: None,
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        });
        assert!(models_of(&fleet).is_empty());
    }

    #[test]
    fn roleless_add_persists_then_toggle_preserves_explicit_role_pins() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();

        assert!(fleet_models(&workspace).expect("no selection").is_empty());
        let change =
            add_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3-flash", &[]).expect("add");
        assert_eq!(
            change,
            FleetModelChange::Added {
                fleet: DEFAULT_FLEET_NAME.to_string(),
                created_fleet: true,
                selected_fleet: true,
                roles: Vec::new(),
            }
        );
        let receipt = change_receipt(Locale::En, "openrouter", "z-ai/glm-5.3-flash", &change);
        assert!(
            receipt.contains("new personal team, now selected"),
            "{receipt}"
        );
        let models = fleet_models(&workspace).expect("fleet");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "z-ai/glm-5.3-flash");
        assert!(models[0].roles.is_empty());
        let (on_disk, _) = selected_file(&workspace);
        assert_eq!(on_disk.members.len(), 1);
        assert!(on_disk.members[0].shortlist);
        assert!(on_disk.members[0].role.is_empty());

        // A second add with another role attaches the role instead of
        // duplicating the model.
        add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3-flash",
            &["general".to_string(), "scout".to_string()],
        )
        .expect("add role");
        let models = fleet_models(&workspace).expect("fleet");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].roles, ["general", "scout"]);
        let (before, path) = selected_file(&workspace);
        let explicit_roles: Vec<_> = before
            .members
            .into_iter()
            .filter(|member| !member.shortlist)
            .collect();

        let change =
            toggle_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3-flash").expect("toggle");
        assert!(matches!(
            change,
            FleetModelChange::Unchanged {
                reason: UnchangedReason::AlreadyPresent,
                ..
            }
        ));
        let receipt = change_receipt(Locale::En, "openrouter", "z-ai/glm-5.3-flash", &change);
        assert!(receipt.contains("stays on the team"), "{receipt}");
        assert!(
            !receipt.contains("Removed"),
            "retained role pins must not report removal: {receipt}"
        );
        let (on_disk, _) = selected_file(&workspace);
        assert_eq!(
            on_disk.members, explicit_roles,
            "explicit pins survive unchanged"
        );
        let bytes = std::fs::read(&path).expect("saved roles");
        let change = toggle_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3-flash")
            .expect("pinned toggle");
        assert!(matches!(
            change,
            FleetModelChange::Unchanged {
                reason: UnchangedReason::AlreadyPresent,
                ..
            }
        ));
        assert_eq!(std::fs::read(&path).expect("unchanged roles"), bytes);

        // The explicitly named removal command still removes role assignments.
        let change = remove_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3-flash")
            .expect("explicit removal");
        assert!(
            matches!(change, FleetModelChange::Removed { ref roles, .. } if roles == &["general", "scout"])
        );
        assert!(fleet_models(&workspace).expect("fleet").is_empty());

        let err = remove_fleet_model(&workspace, "openrouter", "nope").expect_err("absent");
        assert!(
            matches!(err, FleetModelError::NotInFleet { ref route, .. } if route == "openrouter/nope"),
            "{err:?}"
        );
        assert!(
            err.message(Locale::En).contains("is not on the team"),
            "{err}"
        );
    }

    /// The picker's ⇧F creates only a persisted model choice, never a role.
    #[test]
    fn toggle_enrolls_without_a_role_or_executable_member_and_toggles_off() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();

        let change =
            toggle_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3-flash").expect("toggle on");
        assert!(
            matches!(change, FleetModelChange::Added { ref roles, .. } if roles.is_empty()),
            "{change:?}"
        );
        let (on_disk, _) = selected_file(&workspace);
        assert_eq!(on_disk.members.len(), 1);
        assert!(on_disk.members[0].shortlist);
        assert!(on_disk.members[0].role.is_empty());
        assert_eq!(on_disk.members[0].provider.as_deref(), Some("openrouter"));
        assert_eq!(
            on_disk.members[0].model.as_deref(),
            Some("z-ai/glm-5.3-flash")
        );
        assert!(
            fleet_models(&workspace).expect("reloaded models")[0]
                .roles
                .is_empty()
        );
        let roster = crate::fleet::identity::roster_from_fleet(
            &on_disk,
            FleetScope::Personal,
            Path::new("shortlist.toml"),
        );
        assert!(roster.members().is_empty(), "a model choice is not a role");
        let effective =
            crate::fleet::identity::load_effective_roster(&Default::default(), &workspace, None);
        assert!(!effective.is_exact_selection());
        assert!(
            effective
                .members()
                .iter()
                .all(|member| member.origin == crate::fleet::roster::ProfileOrigin::BuiltIn)
        );

        let change =
            toggle_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3-flash").expect("toggle off");
        assert!(
            matches!(change, FleetModelChange::Removed { ref roles, .. } if roles.is_empty()),
            "{change:?}"
        );
        let (on_disk, _) = selected_file(&workspace);
        assert!(on_disk.members.is_empty(), "{:?}", on_disk.members);
    }

    /// Review finding on #5815: with nothing selected, an existing personal
    /// `My fleet` must be loaded and extended, never overwritten with an
    /// empty file, and selected only once the add has landed.
    #[test]
    fn add_with_no_selection_reuses_an_existing_personal_default_fleet() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let mut existing = fleet_with(None, &[("scout", "scout", "z-ai/glm-5.3-flash")]);
        existing.name = DEFAULT_FLEET_NAME.to_string();
        save_fleet(&existing, FleetScope::Personal, &workspace).expect("save");
        assert!(resolve_selected_fleet(&workspace).expect("ok").is_none());

        // A rejected add selects nothing.
        let err = add_fleet_model(&workspace, "", "x", &[]).expect_err("blank provider");
        assert!(matches!(err, FleetModelError::NeedsRoute), "{err:?}");
        assert!(resolve_selected_fleet(&workspace).expect("ok").is_none());

        let change = add_fleet_model(
            &workspace,
            "openrouter",
            "deepseek/deepseek-v4-flash",
            &["reviewer".to_string()],
        )
        .expect("add");
        assert_eq!(
            change,
            FleetModelChange::Added {
                fleet: DEFAULT_FLEET_NAME.to_string(),
                created_fleet: false,
                selected_fleet: true,
                roles: vec!["reviewer".to_string()],
            }
        );
        let receipt = change_receipt(
            Locale::En,
            "openrouter",
            "deepseek/deepseek-v4-flash",
            &change,
        );
        assert!(receipt.ends_with("(now selected)"), "{receipt}");
        let (on_disk, _) = selected_file(&workspace);
        let ids: Vec<_> = on_disk.members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["scout", "reviewer"]);
    }

    /// Review finding on #5815: requested roles are deduplicated and a
    /// fully-present request rewrites nothing.
    #[test]
    fn add_dedupes_roles_and_is_a_no_op_when_every_role_is_present() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let change = add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3-flash",
            &[
                "scout".to_string(),
                "scout".to_string(),
                "Scout".to_string(),
            ],
        )
        .expect("add");
        assert!(
            matches!(change, FleetModelChange::Added { ref roles, .. } if roles == &["scout"]),
            "{change:?}"
        );
        let (on_disk, path) = selected_file(&workspace);
        assert_eq!(on_disk.members.len(), 1);
        let before = std::fs::read(&path).expect("read");
        let before_modified = std::fs::metadata(&path).expect("meta").modified().ok();

        let change = add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3-flash",
            &["scout".to_string()],
        )
        .expect("add again");
        assert_eq!(
            change,
            FleetModelChange::Unchanged {
                fleet: DEFAULT_FLEET_NAME.to_string(),
                reason: UnchangedReason::AlreadyPresent,
            }
        );
        assert_eq!(std::fs::read(&path).expect("read"), before);
        assert_eq!(
            std::fs::metadata(&path).expect("meta").modified().ok(),
            before_modified,
            "a no-op add must not rewrite the file"
        );
    }

    /// Review finding on #5815: a selected fleet that cannot be read is an
    /// error, not "no fleet selected".
    #[test]
    fn fleet_models_reports_a_broken_selection_as_an_error() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3-flash",
            &["scout".to_string()],
        )
        .expect("add");
        let (_, path) = selected_file(&workspace);

        std::fs::write(&path, "this is not = [toml").expect("corrupt");
        let err = fleet_models(&workspace).expect_err("parse error");
        assert!(matches!(err, FleetStoreError::Parse { .. }), "{err:?}");

        std::fs::remove_file(&path).expect("remove");
        let err = fleet_models(&workspace).expect_err("dangling selection");
        assert!(matches!(err, FleetStoreError::NotFound(_)), "{err:?}");
        // The picker's toggle carries the same error instead of adding to a
        // fleet nobody can read.
        let err = toggle_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3-flash")
            .expect_err("toggle on a broken selection");
        assert!(matches!(err, FleetModelError::Store(_)), "{err:?}");
    }

    #[test]
    fn legacy_bare_pins_migrate_without_reclassifying_id_only_roles() {
        // A 0.9.12 roster: role members plus model pins promoted to members.
        let text = r#"
schema = "fleet"
schema_revision = 2
name = "Default"

[[members]]
id = "scout"
role = "scout"
model = "deepseek-v4-flash"
provider = "deepseek"

[[members]]
id = "general"
model = "deepseek-v4-pro"
provider = "deepseek"

[[members]]
id = "audit-team"
model = "private-review-model"
provider = "custom-a"

[[members]]
id = "glm-52"
model = "GLM-5.2"
provider = "zai"

[[members]]
id = "model-a"
model = "model-a"
provider = "custom-a"
"#;
        let fleet = FleetFile::parse(text).expect("parse");
        let ids: Vec<&str> = fleet.members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["scout", "general", "audit-team"],
            "only legacy bare model pins are dropped"
        );
        assert!(fleet.members.iter().all(|member| !member.shortlist));
        assert_eq!(fleet.member("general").unwrap().role_label(), "general");
        assert_eq!(
            fleet.member("audit-team").unwrap().role_label(),
            "audit-team"
        );
        assert!(!fleet.render_toml().unwrap().contains("shortlist"));
    }

    #[test]
    fn operator_membership_survives_shortlist_toggle_and_explicit_role_pins() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let mut fleet = fleet_with(Some(("openrouter", "z-ai/glm-5.3")), &[]);
        fleet.name = "Ops".to_string();
        fleet.members.push(
            serde_json::from_value(serde_json::json!({
                "id": "choice", "shortlist": true,
                "provider": "openrouter", "model": "z-ai/glm-5.3",
            }))
            .unwrap(),
        );
        save_fleet(&fleet, FleetScope::Personal, &workspace).expect("save");
        set_selected("Ops", FleetScope::Personal, &workspace).expect("select");

        let change = toggle_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3").expect("toggle");
        assert_eq!(
            change,
            FleetModelChange::Unchanged {
                fleet: "Ops".to_string(),
                reason: UnchangedReason::OperatorRoute,
            }
        );
        let receipt = change_receipt(Locale::En, "openrouter", "z-ai/glm-5.3", &change);
        assert!(receipt.contains("stays on the team"), "{receipt}");
        assert!(receipt.contains("current model"), "{receipt}");
        let change = add_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3", &[])
            .expect("operator is already available");
        assert!(matches!(
            change,
            FleetModelChange::Unchanged {
                reason: UnchangedReason::OperatorRoute,
                ..
            }
        ));
        let (on_disk, _) = selected_file(&workspace);
        assert!(
            on_disk.members.is_empty(),
            "no duplicate member row: {:?}",
            on_disk.members
        );
        let err = remove_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3")
            .expect_err("operator route");
        assert!(
            matches!(err, FleetModelError::OperatorRoute { .. }),
            "{err:?}"
        );

        // With a role, the operator route may also fill that role; the
        // shortcut cannot delete either explicit assignment.
        add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3",
            &["planner".to_string()],
        )
        .expect("add role");
        let models = fleet_models(&workspace).expect("fleet");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].roles, ["operator", "planner"]);
        let change = toggle_fleet_model(&workspace, "openrouter", "z-ai/glm-5.3").expect("toggle");
        assert!(
            matches!(
                change,
                FleetModelChange::Unchanged {
                    reason: UnchangedReason::AlreadyPresent,
                    ..
                }
            ),
            "{change:?}"
        );
        assert_eq!(
            fleet_models(&workspace).expect("fleet")[0].roles,
            ["operator", "planner"]
        );
    }

    #[test]
    fn add_on_the_operator_route_inherits_and_follows_it_when_it_moves() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        let mut fleet = fleet_with(Some(("openrouter", "z-ai/glm-5.3")), &[]);
        fleet.name = "Ops".to_string();
        save_fleet(&fleet, FleetScope::Personal, &workspace).expect("save");
        set_selected("Ops", FleetScope::Personal, &workspace).expect("select");

        // #6037: a role asked to run the operator route inherits it — no pin.
        let change = add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3",
            &["planner".to_string()],
        )
        .expect("add role");
        assert!(
            matches!(change, FleetModelChange::Added { .. }),
            "{change:?}"
        );
        let (on_disk, _) = selected_file(&workspace);
        let planner = on_disk.member("planner").expect("planner member");
        assert_eq!(planner.provider, None, "inherited route writes no pin");
        assert_eq!(planner.model, None, "inherited route writes no pin");

        // Coverage still counts: a second add of the same role is a no-op.
        let change = add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3",
            &["planner".to_string()],
        )
        .expect("re-add");
        assert!(matches!(
            change,
            FleetModelChange::Unchanged {
                reason: UnchangedReason::AlreadyPresent,
                ..
            }
        ));

        // The inherited role follows the operator when it moves — the exact
        // property a pin would have defeated.
        let (mut moved, _) = selected_file(&workspace);
        moved.operator = Some(FleetOperator {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            reasoning: None,
        });
        let models = models_of(&moved);
        assert_eq!(models[0].model, "deepseek-v4-flash");
        assert_eq!(models[0].roles, ["operator", "planner"]);

        // A role asked to run a different route keeps the deliberate pin.
        add_fleet_model(
            &workspace,
            "openrouter",
            "z-ai/glm-5.3-flash",
            &["scout".to_string()],
        )
        .expect("add pinned role");
        let (on_disk, _) = selected_file(&workspace);
        let scout = on_disk.member("scout").expect("scout member");
        assert_eq!(scout.provider.as_deref(), Some("openrouter"));
        assert_eq!(scout.model.as_deref(), Some("z-ai/glm-5.3-flash"));
    }

    #[test]
    fn member_ids_stay_unique_when_a_role_is_reused_on_two_models() {
        let _lock = crate::test_support::lock_test_env();
        let (_temp, _home, workspace) = isolated_workspace();
        add_fleet_model(&workspace, "openrouter", "a/one", &["scout".to_string()]).expect("one");
        add_fleet_model(&workspace, "openrouter", "a/two", &["scout".to_string()]).expect("two");
        let (fleet, _) = selected_file(&workspace);
        let ids: Vec<_> = fleet.members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["scout", "scout-atwo"]);
    }
}
