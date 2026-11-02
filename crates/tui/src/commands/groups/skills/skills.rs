//! Skills commands: skills, skill
//!
//! FEAT-022 Phase 4: portable contextual dispatch over
//! [`CommandSkillGroupContext`]; the legacy `RegisterCommand::execute` is a
//! transitional shell that builds the capability envelope and delegates (Phase
//! 6 replaces it with the contract bridge). The dispatcher-only
//! `run_skill_by_name` path and its shared host machinery
//! ([`discover_visible_skills`], [`activate_skill_with_task`]) stay
//! App-carrying and co-located for FEAT-042 extraction.

use std::fmt::Write;

use codewhale_command_contract::facets::{
    CommandSkillGroupContext, CommandSkillsContext, RemoteRegistryOutcome, SkillActivationError,
    SkillBundledTier, SkillEntry, SkillMutationOutcome, SkillMutationReceipt, SkillSourceKind,
    SkillSyncEntry, SkillSyncOutcome, SkillTargetScope,
};
use codewhale_command_contract::handler::{CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;
use crate::tui::app::AppAction;

// ---------------------------------------------------------------------------
// Host-side dispatcher machinery (FEAT-042 handoff — stays App-carrying)
// ---------------------------------------------------------------------------

/// Discover the enabled visible skills for the current App state. Shared by the
/// dispatcher fallback (`run_skill_by_name`) and the host activation helper;
/// kept co-located for FEAT-042.
fn discover_visible_skills(app: &crate::tui::app::App) -> codewhale_skills::SkillRegistry {
    codewhale_skills::discover_for_workspace(
        &app.workspace,
        Some(&app.skills_dir),
        &app.skills_config,
        Some(app.extension_plugin_view().as_ref()),
    )
    .into_enabled()
}

/// Run a specific skill — activates skill for next user message, or
/// dispatches a sub-command (`install`, `update`, `uninstall`, `trust`).
/// Try to run a skill by exact name (used for unified slash-command namespace, #435).
/// Returns None when no skill with that name exists, so the caller can try other sources.
pub(in crate::commands) fn run_skill_by_name(
    app: &mut crate::tui::app::App,
    name: &str,
    arg: Option<&str>,
) -> Option<CommandResult> {
    let registry = discover_visible_skills(app);
    let lookup_name = if name == "new" { "skill-creator" } else { name };
    if registry.get(lookup_name).is_some() {
        Some(activate_skill_with_task(app, name, arg))
    } else {
        None
    }
}

/// Host-side activation helper shared with the dispatcher fallback. The
/// portable `/skill` path uses the `CommandSkillGroupContext` delegate instead
/// (D2); this App-carrying copy is retained for `run_skill_by_name` (FEAT-042).
fn activate_skill_with_task(
    app: &mut crate::tui::app::App,
    name: &str,
    task: Option<&str>,
) -> CommandResult {
    let mut result = activate_skill(app, name);
    if !result.is_error
        && let Some(task) = task.map(str::trim).filter(|task| !task.is_empty())
    {
        result.action = Some(AppAction::SendMessage(task.to_string()));
    }
    result
}

/// Host-side `/skill <name>` activation (FEAT-042 dispatcher machinery).
fn activate_skill(app: &mut crate::tui::app::App, name: &str) -> CommandResult {
    // `/skill new` is a friendly alias for `/skill skill-creator`.
    let name = if name == "new" { "skill-creator" } else { name };

    let registry = discover_visible_skills(app);

    if let Some(skill) = registry.get(name) {
        if !skill.invocation.user_invocable() {
            return CommandResult::error(format!(
                "Skill '{}' does not allow user invocation",
                skill.name
            ));
        }
        let plugin_provenance = skill.source.provenance();
        if let Some(provenance) = &plugin_provenance
            && let Err(reason) =
                provenance.verify_for(&app.workspace, Some(app.extension_plugin_view().as_ref()))
        {
            return CommandResult::error(format!(
                "Plugin skill '{}' is no longer active: {reason}",
                skill.name
            ));
        }
        let instruction = format!(
            "You are now using a skill. Follow these instructions:\n\n# Skill: {}\n\n{}\n\n---\n\nNow respond to the user's request following the above skill instructions.",
            skill.name, skill.body
        );

        app.add_message(crate::tui::history::HistoryCell::System {
            content: format!("Activated skill: {}\n\n{}", skill.name, skill.description),
        });

        app.active_skill = Some(instruction);
        app.active_skill_provenance = plugin_provenance;

        CommandResult::message(format!(
            "Skill '{}' activated.\n\nDescription: {}\n\nType your request and the skill instructions will be applied.",
            skill.name, skill.description
        ))
    } else {
        let available: Vec<String> = registry.list().iter().map(|s| s.name.clone()).collect();
        let warnings = render_skill_warnings(registry.warnings());

        if available.is_empty() {
            CommandResult::error(format!(
                "Skill '{name}' not found. No skills installed.\n\nUse /skills to see how to add skills.{warnings}"
            ))
        } else {
            CommandResult::error(format!(
                "Skill '{}' not found.\n\nAvailable skills: {}{}",
                name,
                available.join(", "),
                warnings
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Portable rendering helpers for the shared skill registry projection
// ---------------------------------------------------------------------------

/// Render registry warnings as the baseline suffix block.
fn render_skill_warnings(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let _ = writeln!(out, "\nWarnings ({}):", warnings.len());
    for warning in warnings {
        let _ = writeln!(out, "  - {warning}");
    }
    out
}

/// Source label used by `/skills inspect` (baseline `skill_source_label`).
fn skill_source_label(source: &SkillSourceKind) -> String {
    match source {
        SkillSourceKind::Native => "native".to_string(),
        SkillSourceKind::Plugin {
            plugin_name,
            plugin_id,
        } => format!("reviewed plugin snapshot {plugin_name} ({plugin_id})"),
    }
}

/// Network-policy approval message (baseline `needs_approval_message`).
fn needs_approval_message(host: &str) -> String {
    format!(
        "Network policy requires approval for {host}.\n\
         Add it to your allow list with `/network allow {host}` (or set [network].default = \"allow\" in ~/.codewhale/config.toml), then retry."
    )
}

/// Network-policy denial message (baseline `network_denied_message`).
fn network_denied_message(host: &str) -> String {
    format!(
        "Network policy denied access to {host}.\n\
         Remove the deny entry from ~/.codewhale/config.toml under [network] or contact your administrator."
    )
}

/// Render a mutation receipt byte-identically (baseline `format_mutation_receipt`).
fn format_mutation_receipt(receipt: &SkillMutationReceipt) -> String {
    match &receipt.outcome {
        SkillMutationOutcome::Installed => format!(
            "Installed skill '{}'.\nLocation: {}\n\nManage skills with /skills.",
            receipt.name, receipt.safe_target_path
        ),
        SkillMutationOutcome::Updated => format!(
            "Skill '{}' updated.\nLocation: {}",
            receipt.name, receipt.safe_target_path
        ),
        SkillMutationOutcome::NoChange => {
            format!("Skill '{}': no upstream change.", receipt.name)
        }
        SkillMutationOutcome::Removed => format!("Removed skill '{}'.", receipt.name),
        SkillMutationOutcome::Trusted => format!(
            "Marked skill '{}' as trusted. The .trusted marker is advisory and digest-bound; it records your review intent but does not sandbox or auto-authorize scripts.",
            receipt.name
        ),
        SkillMutationOutcome::Imported => format!(
            "Imported skill '{}'.\nLocation: {}",
            receipt.name, receipt.safe_target_path
        ),
        SkillMutationOutcome::AlreadyPresent => format!(
            "Skill '{}' is already present at {} (exact duplicate).",
            receipt.name, receipt.safe_target_path
        ),
        SkillMutationOutcome::NeedsApproval(host) => needs_approval_message(host),
        SkillMutationOutcome::NetworkDenied(host) => network_denied_message(host),
    }
}

/// Parse an optional `--project` / `--global` scope prefix (baseline
/// `parse_scope_args`, portable scope enum).
fn parse_scope_args(args: &str) -> Result<(Option<SkillTargetScope>, &str), String> {
    let mut scope = None;
    let mut rest = args.trim();
    loop {
        if let Some(next) = rest.strip_prefix("--project") {
            if scope.is_some() {
                return Err("specify at most one of --project / --global".into());
            }
            scope = Some(SkillTargetScope::Project);
            rest = next.trim_start();
            continue;
        }
        if let Some(next) = rest.strip_prefix("--global") {
            if scope.is_some() {
                return Err("specify at most one of --project / --global".into());
            }
            scope = Some(SkillTargetScope::Global);
            rest = next.trim_start();
            continue;
        }
        break;
    }
    Ok((scope, rest.trim()))
}

// ---------------------------------------------------------------------------
// /skills — portable contextual dispatch
// ---------------------------------------------------------------------------

pub(in crate::commands) const SKILLS_INFO: CommandInfo = CommandInfo {
    name: "skills",
    aliases: &["jinengliebiao"],
    usage: "/skills [manage|--remote|sync|inspect|suggest <task>|<prefix>]  (bare opens Extensions)",
    description_key: "cmd_skills_description",
};

pub(in crate::commands) struct SkillsCmd;

impl RegisterCommand<CommandResult> for SkillsCmd {
    fn info() -> &'static CommandInfo {
        &SKILLS_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::SKILL_GROUP,
            handler: skills_contextual,
        }
    }
}

/// Contextual `/skills` dispatch (FEAT-022 D4): exactly the skill-group facet.
fn skills_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(skill_group) = parts.skill_group.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: skill_group");
    };
    list_skills(skill_group, arg)
}

/// Shared inventory entry, with the dedicated mutation manager kept at `/skills manage`.
fn list_skills(group: &mut dyn CommandSkillGroupContext, arg: Option<&str>) -> CommandResult {
    let mut prefix: Option<String> = None;
    if let Some(arg) = arg {
        let trimmed = arg.trim();
        if trimmed == "manage" {
            return CommandResult::action(AppAction::OpenSkillsManager);
        }
        if trimmed == "--remote" || trimmed == "remote" {
            return list_remote_skills(group);
        }
        if trimmed == "sync" || trimmed == "--sync" {
            return sync_skills(group);
        }
        if trimmed == "inspect" || trimmed == "--inspect" {
            return inspect_skills(group);
        }
        if trimmed == "suggest" || trimmed == "recommend" {
            return CommandResult::error("Usage: /skills suggest <task>");
        }
        if let Some(task) = trimmed
            .strip_prefix("suggest ")
            .or_else(|| trimmed.strip_prefix("recommend "))
        {
            return suggest_remote_skills(group, task);
        }
        if !trimmed.is_empty() {
            // Anything else is treated as a name-prefix filter (#1318).
            // Reject obviously malformed args (whitespace inside the
            // prefix, leading dash) so future flag additions don't
            // collide with skill names. Skill names that start with
            // `-` aren't allowed by the loader so this is safe.
            if trimmed.starts_with('-') || trimmed.split_whitespace().count() > 1 {
                return CommandResult::error(
                    "Usage: /skills [--remote|sync|inspect|suggest <task>|<name-prefix>]",
                );
            }
            prefix = Some(trimmed.to_ascii_lowercase());
        }
    } else {
        // Bare inventory is owned-only and performs no network requests.
        return CommandResult::action(AppAction::OpenExtensions {
            tab: crate::tui::views::extensions::ExtensionsTab::Skills,
        });
    }

    let projection = group.skill_registry_projection();
    let warnings = render_skill_warnings(&projection.warnings);
    let skills_dir = projection.skills_dir.clone();

    if projection.entries.is_empty() {
        let msg = format!(
            "No skills found.\n\n\
             Skills location: {}\n\n\
             To add skills, create directories with SKILL.md files:\n  \
             {}/my-skill/SKILL.md\n\n\
             Format:\n  \
             ---\n  \
             name: my-skill\n  \
             description: What this skill does\n  \
             ---\n\n  \
             <instructions here>{warnings}",
            skills_dir, skills_dir
        );
        return CommandResult::message(msg);
    }

    let filtered: Vec<&SkillEntry> = if let Some(p) = prefix.as_deref() {
        projection
            .entries
            .iter()
            .filter(|s| s.name.to_ascii_lowercase().starts_with(p))
            .collect()
    } else {
        projection.entries.iter().collect()
    };

    if filtered.is_empty() {
        // The user typed a prefix that matched nothing. Surface what
        // they typed plus the full count so they can decide whether
        // to adjust the prefix or run `/skills` for the whole list.
        let p = prefix.as_deref().unwrap_or("");
        return CommandResult::message(format!(
            "No skills match prefix `{p}` (out of {} available).\n\nRun /skills to see them all.{warnings}",
            projection.total
        ));
    }

    let mut output = if let Some(p) = prefix.as_deref() {
        format!(
            "Available skills matching `{p}` ({} of {}):\n",
            filtered.len(),
            projection.total
        )
    } else {
        format!("Available skills ({}):\n", projection.total)
    };
    output.push_str("─────────────────────────────\n");

    if prefix.is_some() {
        // Filtered view: keep the flat list — the user already narrowed.
        for (idx, skill) in filtered.iter().enumerate() {
            if idx > 0 {
                output.push('\n');
            }
            let _ = writeln!(output, "  /{} - {}", skill.name, skill.description);
        }
    } else {
        // Unfiltered view: keep user-created skills prominent, then split the
        // shipped catalog into its two curated product tiers. The tier
        // classification is resolved host-side into `bundled_tier` so the
        // canonical bundle-name list is never duplicated here.
        let (user_skills, bundled_skills): (Vec<&SkillEntry>, Vec<&SkillEntry>) =
            filtered.iter().partition(|s| s.bundled_tier.is_none());

        if !user_skills.is_empty() {
            let _ = writeln!(output, "Your skills ({}):", user_skills.len());
            for skill in &user_skills {
                let _ = writeln!(output, "  /{} - {}", skill.name, skill.description);
            }
            if !bundled_skills.is_empty() {
                output.push('\n');
            }
        }

        if !bundled_skills.is_empty() {
            let (core, tooling): (Vec<&SkillEntry>, Vec<&SkillEntry>) = bundled_skills
                .into_iter()
                .partition(|skill| skill.bundled_tier == Some(SkillBundledTier::CoreAgentic));
            for (group_idx, (tier, skills)) in [
                (SkillBundledTier::CoreAgentic, core),
                (SkillBundledTier::FormatTooling, tooling),
            ]
            .into_iter()
            .enumerate()
            {
                if skills.is_empty() {
                    continue;
                }
                if group_idx > 0 {
                    output.push('\n');
                }
                let _ = writeln!(output, "{} ({}):", tier.heading(), skills.len());
                if user_skills.is_empty() {
                    for skill in skills {
                        let _ = writeln!(output, "  /{} - {}", skill.name, skill.description);
                    }
                } else {
                    let names: Vec<String> = skills
                        .iter()
                        .map(|skill| format!("/{}", skill.name))
                        .collect();
                    let _ = writeln!(output, "  {}", names.join(", "));
                }
            }
            if !user_skills.is_empty() {
                output.push_str("  (run /skills <name> for details on a built-in)\n");
            }
        }
    }

    let _ = write!(
        output,
        "\nUse /skill <name> to run a skill\nSkills location: {}{}",
        skills_dir, warnings
    );

    CommandResult::message(output)
}

/// `/skills inspect` — byte-identical discovery diagnostics.
fn inspect_skills(group: &mut dyn CommandSkillGroupContext) -> CommandResult {
    let projection = group.skill_registry_projection();
    let warnings = render_skill_warnings(&projection.warnings);

    let mut output = String::from("Skills Inspect\n");
    output.push_str("─────────────────────────────\n");
    let _ = writeln!(output, "Discovery mode: {}", projection.mode_label);
    let _ = writeln!(output, "Workspace: {}", projection.workspace);
    let _ = writeln!(output, "Configured skills dir: {}", projection.skills_dir);
    for line in &projection.bundled_status {
        let _ = writeln!(output, "{line}");
    }

    if projection.dirs.is_empty() {
        output.push_str("\nSearched directories: none found\n");
    } else {
        let _ = writeln!(
            output,
            "\nSearched directories ({}):",
            projection.dirs.len()
        );
        for (idx, dir) in projection.dirs.iter().enumerate() {
            let _ = writeln!(output, "  {}. {}", idx + 1, dir);
        }
    }

    let _ = writeln!(output, "\nAvailable skills ({}):", projection.total);
    if projection.entries.is_empty() {
        output.push_str("  (none)\n");
    } else {
        for skill in &projection.entries {
            if skill.description.trim().is_empty() {
                let _ = writeln!(output, "  - {}", skill.name);
            } else {
                let _ = writeln!(output, "  - {} — {}", skill.name, skill.description);
            }
            let _ = writeln!(output, "    source: {}", skill_source_label(&skill.source));
            if let Some(path) = skill
                .path
                .as_ref()
                .filter(|_| matches!(skill.source, SkillSourceKind::Native))
            {
                let _ = writeln!(output, "    path: {}", path);
            }
            if let Some(note) = &skill.index_note {
                let _ = writeln!(output, "    {note}");
            }
        }
    }

    output.push_str(&warnings);
    CommandResult::message(output)
}

/// `/skills --remote` — curated registry listing.
fn list_remote_skills(group: &mut dyn CommandSkillGroupContext) -> CommandResult {
    match group.fetch_remote_registry() {
        Ok(RemoteRegistryOutcome::Loaded { entries }) => {
            if entries.is_empty() {
                return CommandResult::message("Registry is empty.");
            }
            let mut out = format!("Available remote skills ({}):\n", entries.len());
            out.push_str("─────────────────────────────\n");
            for entry in &entries {
                let _ = writeln!(
                    out,
                    "  {} — {} (source: {})",
                    entry.name,
                    entry.description.clone().unwrap_or_default(),
                    entry.source
                );
            }
            let _ = write!(out, "\nInstall with: /skill install <name>");
            CommandResult::message(out)
        }
        Ok(RemoteRegistryOutcome::NeedsApproval(host)) => {
            CommandResult::error(needs_approval_message(&host))
        }
        Ok(RemoteRegistryOutcome::Denied(host)) => {
            CommandResult::error(network_denied_message(&host))
        }
        Err(err) => CommandResult::error(err),
    }
}

/// `/skills suggest <task>` — ranked remote recommendations.
fn suggest_remote_skills(group: &mut dyn CommandSkillGroupContext, task: &str) -> CommandResult {
    let task = task.trim();
    if task.chars().count() < 3 {
        return CommandResult::error("Usage: /skills suggest <task of at least 3 characters>");
    }

    match group.recommend_skills(task) {
        Ok(recommendations) => {
            if recommendations.is_empty() {
                return CommandResult::message(format!(
                    "No curated remote skills matched `{task}`.\n\nBrowse the catalog with /skills --remote. Nothing was installed, trusted, or enabled."
                ));
            }

            let mut out = format!("Suggested remote skills for `{task}`:\n");
            out.push_str("─────────────────────────────\n");
            for recommendation in &recommendations {
                let description = recommendation
                    .description
                    .as_deref()
                    .filter(|description| !description.trim().is_empty())
                    .unwrap_or("No description provided.");
                let _ = writeln!(out, "  {} — {description}", recommendation.name);
                let _ = writeln!(out, "    Why: {}", recommendation.matched_terms.join(", "));
                let _ = writeln!(
                    out,
                    "    Install if you want it: /skill install {}",
                    recommendation.name
                );
            }
            out.push_str("\nNothing was installed, trusted, or enabled.");
            CommandResult::message(out)
        }
        Err(err) => CommandResult::error(err),
    }
}

/// `/skills sync` — registry sync report.
fn sync_skills(group: &mut dyn CommandSkillGroupContext) -> CommandResult {
    match group.sync_registry() {
        Ok(SkillSyncOutcome::Done {
            total,
            downloaded,
            fresh,
            failed,
            entries,
        }) => {
            let mut out = String::from("Registry sync complete.\n\n");

            for outcome in &entries {
                match outcome {
                    SkillSyncEntry::Downloaded { name, path } => {
                        let _ = writeln!(out, "  [+] {name} — downloaded to {path}");
                    }
                    SkillSyncEntry::Fresh { name } => {
                        let _ = writeln!(out, "  [=] {name} — already up to date");
                    }
                    SkillSyncEntry::Failed { name, reason } => {
                        let _ = writeln!(out, "  [!] {name} — failed: {reason}");
                    }
                    SkillSyncEntry::Denied { name, host } => {
                        let _ = writeln!(out, "  [!] {name} — network denied ({host})");
                    }
                    SkillSyncEntry::NeedsApproval { name, host } => {
                        let _ = writeln!(
                            out,
                            "  [?] {name} — needs approval for {host} (run `/network allow {host}` then retry)"
                        );
                    }
                }
            }

            let _ = write!(
                out,
                "\n{total} skill(s) processed: {downloaded} downloaded, {fresh} up-to-date, {failed} failed."
            );

            CommandResult::message(out)
        }
        Ok(SkillSyncOutcome::RegistryNeedsApproval(host)) => {
            CommandResult::error(needs_approval_message(&host))
        }
        Ok(SkillSyncOutcome::RegistryDenied(host)) => {
            CommandResult::error(network_denied_message(&host))
        }
        Err(err) => CommandResult::error(err),
    }
}

// ---------------------------------------------------------------------------
// /skill — portable contextual dispatch
// ---------------------------------------------------------------------------

pub(in crate::commands) const SKILL_INFO: CommandInfo = CommandInfo {
    name: "skill",
    aliases: &["jineng"],
    usage: "/skill <name|install <spec>|update <name>|uninstall <name>|trust <name>>",
    description_key: "cmd_skill_description",
};

pub(in crate::commands) struct SkillCmd;

impl RegisterCommand<CommandResult> for SkillCmd {
    fn info() -> &'static CommandInfo {
        &SKILL_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: codewhale_command_contract::handler::CommandCapabilities::SKILL_GROUP
                .union(codewhale_command_contract::handler::CommandCapabilities::SKILLS),
            handler: skill_contextual,
        }
    }
}

/// Contextual `/skill` dispatch (FEAT-022 D4): exactly the skill-group facet
/// plus the shared SKILLS facet (active-skill reads + cache refresh; D2).
fn skill_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(skill_group) = parts.skill_group.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: skill_group");
    };
    let Some(skills) = parts.skills.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: skills");
    };
    run_skill(skill_group, skills, arg)
}

/// Portable `/skill` dispatch — byte-identical to the baseline handler.
fn run_skill(
    group: &mut dyn CommandSkillGroupContext,
    skills: &mut dyn CommandSkillsContext,
    arg: Option<&str>,
) -> CommandResult {
    let raw = match arg {
        Some(n) => n.trim(),
        None => {
            return CommandResult::error(
                "Usage: /skill <name>\n\nSubcommands:\n  /skill install [--project|--global] <github:owner/repo|https://…|<registry-name>>\n  /skill update [--project|--global] <name>\n  /skill uninstall [--project|--global] <name>\n  /skill trust [--project|--global] <name>",
            );
        }
    };

    // Sub-command dispatch happens before the activation path so users can't
    // accidentally activate a skill literally named "install".
    let mut iter = raw.splitn(2, char::is_whitespace);
    let head = iter.next().unwrap_or("").trim();
    let rest = iter.next().unwrap_or("").trim();
    match head {
        "install" => return install_skill(group, skills, rest),
        "update" => return update_skill(group, skills, rest),
        "uninstall" => return uninstall_skill(group, skills, rest),
        "trust" => return trust_skill(group, rest),
        _ => {}
    }

    let task = (!rest.is_empty()).then_some(rest);
    activate_skill_portable(group, head, task)
}

/// Portable activation — the host performs lookup, authority verification, and
/// side effects; the handler composes the byte-identical messages/actions.
fn activate_skill_portable(
    group: &mut dyn CommandSkillGroupContext,
    name: &str,
    task: Option<&str>,
) -> CommandResult {
    // `/skill new` is a friendly alias for `/skill skill-creator`; the alias is
    // resolved here (parsing stays portable) so the not-found message uses the
    // mapped name exactly like the baseline.
    let name = if name == "new" { "skill-creator" } else { name };

    match group.activate_skill(name) {
        Ok(outcome) => {
            let mut result = CommandResult::message(format!(
                "Skill '{}' activated.\n\nDescription: {}\n\nType your request and the skill instructions will be applied.",
                outcome.name, outcome.description
            ));
            if let Some(task) = task.map(str::trim).filter(|task| !task.is_empty()) {
                result.action = Some(AppAction::SendMessage(task.to_string()));
            }
            result
        }
        Err(SkillActivationError::NotFound {
            requested,
            available,
            warnings,
        }) => {
            let warnings = render_skill_warnings(&warnings);
            if available.is_empty() {
                CommandResult::error(format!(
                    "Skill '{requested}' not found. No skills installed.\n\nUse /skills to see how to add skills.{warnings}"
                ))
            } else {
                CommandResult::error(format!(
                    "Skill '{}' not found.\n\nAvailable skills: {}{}",
                    requested,
                    available.join(", "),
                    warnings
                ))
            }
        }
        Err(SkillActivationError::InvocationRejected { name, reason }) => {
            CommandResult::error(format!("Skill '{}' could not be activated: {reason}", name))
        }
        Err(SkillActivationError::PluginRejected { name, reason }) => CommandResult::error(
            format!("Plugin skill '{}' is no longer active: {reason}", name),
        ),
    }
}

// ─── /skill install ────────────────────────────────────────────────────────

fn install_skill(
    group: &mut dyn CommandSkillGroupContext,
    skills: &mut dyn CommandSkillsContext,
    args: &str,
) -> CommandResult {
    let (scope, spec) = match parse_scope_args(args) {
        Ok(v) => v,
        Err(err) => return CommandResult::error(err),
    };
    if spec.is_empty() {
        return CommandResult::error(
            "Usage: /skill install [--project|--global] <github:owner/repo|https://…|<registry-name>>",
        );
    }
    match group.install_skill(scope, spec) {
        Ok(receipt) => {
            // Cache refresh is a D2 shared-SKILLS operation: the host returns
            // the receipt; the portable handler owns the refresh policy.
            if matches!(receipt.outcome, SkillMutationOutcome::Installed) {
                skills.refresh_skill_cache();
            }
            let message = format_mutation_receipt(&receipt);
            if matches!(
                receipt.outcome,
                SkillMutationOutcome::NeedsApproval(_) | SkillMutationOutcome::NetworkDenied(_)
            ) {
                CommandResult::error(message)
            } else {
                CommandResult::message(message)
            }
        }
        Err(err) => CommandResult::error(err),
    }
}

// ─── /skill update ─────────────────────────────────────────────────────────

fn update_skill(
    group: &mut dyn CommandSkillGroupContext,
    skills: &mut dyn CommandSkillsContext,
    args: &str,
) -> CommandResult {
    let (scope, name) = match parse_scope_args(args) {
        Ok(v) => v,
        Err(err) => return CommandResult::error(err),
    };
    if name.is_empty() {
        return CommandResult::error("Usage: /skill update [--project|--global] <name>");
    }
    match group.update_skill(scope, name) {
        Ok(receipt) => {
            if matches!(receipt.outcome, SkillMutationOutcome::Updated) {
                skills.refresh_skill_cache();
            }
            let message = format_mutation_receipt(&receipt);
            if matches!(
                receipt.outcome,
                SkillMutationOutcome::NeedsApproval(_) | SkillMutationOutcome::NetworkDenied(_)
            ) {
                CommandResult::error(message)
            } else {
                CommandResult::message(message)
            }
        }
        Err(err) => CommandResult::error(err),
    }
}

// ─── /skill uninstall ──────────────────────────────────────────────────────

fn uninstall_skill(
    group: &mut dyn CommandSkillGroupContext,
    skills: &mut dyn CommandSkillsContext,
    args: &str,
) -> CommandResult {
    let (scope, name) = match parse_scope_args(args) {
        Ok(v) => v,
        Err(err) => return CommandResult::error(err),
    };
    if name.is_empty() {
        return CommandResult::error("Usage: /skill uninstall [--project|--global] <name>");
    }
    match group.uninstall_skill(scope, name) {
        Ok(receipt) => {
            skills.refresh_skill_cache();
            CommandResult::message(format_mutation_receipt(&receipt))
        }
        Err(err) => CommandResult::error(err),
    }
}

// ─── /skill trust ──────────────────────────────────────────────────────────

fn trust_skill(group: &mut dyn CommandSkillGroupContext, args: &str) -> CommandResult {
    let (scope, name) = match parse_scope_args(args) {
        Ok(v) => v,
        Err(err) => return CommandResult::error(err),
    };
    if name.is_empty() {
        return CommandResult::error("Usage: /skill trust [--project|--global] <name>");
    }
    match group.trust_skill(scope, name) {
        Ok(receipt) => CommandResult::message(format_mutation_receipt(&receipt)),
        Err(err) => CommandResult::error(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::facets::{
        CommandApprovalState, RemoteRegistryOutcome, RemoteSkillEntry, ReviewOutcome,
        SkillActivationError, SkillActivationOutcome, SkillRecommendation, SkillRegistryProjection,
        SkillSourceKind, SnapshotEntry,
    };

    /// Shared SKILLS fake: read-only getters + cache refresh (D2 surface).
    struct FakeSkills {
        refreshed: bool,
    }
    impl CommandSkillsContext for FakeSkills {
        fn active_skill(&self) -> Option<String> {
            None
        }
        fn active_skill_provenance(&self) -> Option<String> {
            None
        }
        fn refresh_skill_cache(&mut self) {
            self.refreshed = true;
        }
    }

    /// Counting fake for preserving the baseline's exact cache-refresh policy.
    #[derive(Default)]
    struct CountingSkills {
        refresh_count: usize,
    }
    impl CommandSkillsContext for CountingSkills {
        fn active_skill(&self) -> Option<String> {
            None
        }
        fn active_skill_provenance(&self) -> Option<String> {
            None
        }
        fn refresh_skill_cache(&mut self) {
            self.refresh_count += 1;
        }
    }

    /// Deterministic fake skill-group facet over portable values only.
    struct FakeSkillGroup {
        projection: SkillRegistryProjection,
        activation: Result<SkillActivationOutcome, SkillActivationError>,
        install: Result<SkillMutationReceipt, String>,
        update: Result<SkillMutationReceipt, String>,
        uninstall: Result<SkillMutationReceipt, String>,
        trust: Result<SkillMutationReceipt, String>,
        remote: Result<RemoteRegistryOutcome, String>,
        recommend: Result<Vec<SkillRecommendation>, String>,
        sync: Result<SkillSyncOutcome, String>,
        review: Result<ReviewOutcome, String>,
        snapshots: Result<Vec<SnapshotEntry>, String>,
        restore: Result<(), String>,
        approval: CommandApprovalState,
    }

    impl FakeSkillGroup {
        fn new(entries: Vec<SkillEntry>) -> Self {
            let total = entries.len();
            Self {
                projection: SkillRegistryProjection {
                    workspace: "/ws".to_string(),
                    skills_dir: "/ws/.codewhale/skills".to_string(),
                    mode_label: "compatible".to_string(),
                    bundled_status: vec![],
                    dirs: vec!["/ws/.codewhale/skills".to_string()],
                    entries,
                    warnings: vec![],
                    total,
                },
                activation: Ok(SkillActivationOutcome {
                    name: "demo".to_string(),
                    description: "Demo skill".to_string(),
                }),
                install: Ok(SkillMutationReceipt {
                    name: "demo".to_string(),
                    safe_target_path: "/ws/.codewhale/skills/demo".to_string(),
                    outcome: SkillMutationOutcome::Installed,
                }),
                update: Ok(SkillMutationReceipt {
                    name: "demo".to_string(),
                    safe_target_path: "/ws/.codewhale/skills/demo".to_string(),
                    outcome: SkillMutationOutcome::Updated,
                }),
                uninstall: Ok(SkillMutationReceipt {
                    name: "demo".to_string(),
                    safe_target_path: "/ws/.codewhale/skills/demo".to_string(),
                    outcome: SkillMutationOutcome::Removed,
                }),
                trust: Ok(SkillMutationReceipt {
                    name: "demo".to_string(),
                    safe_target_path: "/ws/.codewhale/skills/demo".to_string(),
                    outcome: SkillMutationOutcome::Trusted,
                }),
                remote: Ok(RemoteRegistryOutcome::Loaded {
                    entries: vec![RemoteSkillEntry {
                        name: "remote-demo".to_string(),
                        description: Some("Remote demo".to_string()),
                        source: "github.com/acme/skills".to_string(),
                    }],
                }),
                recommend: Ok(vec![SkillRecommendation {
                    name: "remote-demo".to_string(),
                    description: Some("Remote demo".to_string()),
                    matched_terms: vec!["demo".to_string()],
                }]),
                sync: Ok(SkillSyncOutcome::Done {
                    total: 1,
                    downloaded: 1,
                    fresh: 0,
                    failed: 0,
                    entries: vec![SkillSyncEntry::Downloaded {
                        name: "demo".to_string(),
                        path: "/cache/demo".to_string(),
                    }],
                }),
                review: Ok(ReviewOutcome::Ready),
                snapshots: Ok(vec![SnapshotEntry {
                    id: "abcdef123456".to_string(),
                    label: "pre-turn:1".to_string(),
                    timestamp: 1_700_000_000,
                }]),
                restore: Ok(()),
                approval: CommandApprovalState {
                    yolo: true,
                    trust_mode: false,
                },
            }
        }
    }

    impl CommandSkillGroupContext for FakeSkillGroup {
        fn skill_registry_projection(&self) -> SkillRegistryProjection {
            self.projection.clone()
        }
        fn activate_skill(
            &mut self,
            _name: &str,
        ) -> Result<SkillActivationOutcome, SkillActivationError> {
            self.activation.clone()
        }
        fn install_skill(
            &mut self,
            _scope: Option<SkillTargetScope>,
            _spec: &str,
        ) -> Result<SkillMutationReceipt, String> {
            self.install.clone()
        }
        fn update_skill(
            &mut self,
            _scope: Option<SkillTargetScope>,
            _name: &str,
        ) -> Result<SkillMutationReceipt, String> {
            self.update.clone()
        }
        fn uninstall_skill(
            &mut self,
            _scope: Option<SkillTargetScope>,
            _name: &str,
        ) -> Result<SkillMutationReceipt, String> {
            self.uninstall.clone()
        }
        fn trust_skill(
            &mut self,
            _scope: Option<SkillTargetScope>,
            _name: &str,
        ) -> Result<SkillMutationReceipt, String> {
            self.trust.clone()
        }
        fn fetch_remote_registry(&mut self) -> Result<RemoteRegistryOutcome, String> {
            self.remote.clone()
        }
        fn recommend_skills(&mut self, _task: &str) -> Result<Vec<SkillRecommendation>, String> {
            self.recommend.clone()
        }
        fn sync_registry(&mut self) -> Result<SkillSyncOutcome, String> {
            self.sync.clone()
        }
        fn run_review(&mut self) -> Result<ReviewOutcome, String> {
            self.review.clone()
        }
        fn snapshot_list(&mut self, _limit: usize) -> Result<Vec<SnapshotEntry>, String> {
            self.snapshots.clone()
        }
        fn restore_snapshot(&mut self, _id: &str) -> Result<(), String> {
            self.restore.clone()
        }
        fn approval_state(&self) -> CommandApprovalState {
            self.approval
        }
    }

    fn demo_entry() -> SkillEntry {
        SkillEntry {
            index_note: None,
            name: "demo".to_string(),
            description: "Demo skill".to_string(),
            source: SkillSourceKind::Native,
            path: Some("/ws/.codewhale/skills/demo".to_string()),
            bundled_tier: None,
        }
    }

    fn bundled_entry(name: &str, tier: SkillBundledTier) -> SkillEntry {
        SkillEntry {
            index_note: None,
            name: name.to_string(),
            description: format!("{name} skill"),
            source: SkillSourceKind::Native,
            path: None,
            bundled_tier: Some(tier),
        }
    }

    // ── /skills parity ────────────────────────────────────────────────────

    #[test]
    fn bare_skills_opens_extensions_and_manage_keeps_mutation_manager() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, None);
        assert!(result.message.is_none());
        assert!(matches!(
            result.action,
            Some(AppAction::OpenExtensions {
                tab: crate::tui::views::extensions::ExtensionsTab::Skills
            })
        ));
        assert!(matches!(
            list_skills(&mut group, Some("manage")).action,
            Some(AppAction::OpenSkillsManager)
        ));
    }

    #[test]
    fn skills_empty_registry_message_is_exact() {
        let mut group = FakeSkillGroup::new(vec![]);
        let result = list_skills(&mut group, Some(""));
        let msg = result.message.expect("expected message");
        assert!(
            msg.starts_with("No skills found.\n\nSkills location: /ws/.codewhale/skills\n"),
            "{msg}"
        );
        assert!(msg.contains("/ws/.codewhale/skills/my-skill/SKILL.md"));
    }

    #[test]
    fn skills_prefix_listing_flat_format_is_exact() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, Some("de"));
        let msg = result.message.expect("expected message");
        assert!(
            msg.starts_with("Available skills matching `de` (1 of 1):\n"),
            "{msg}"
        );
        assert!(msg.contains("  /demo - Demo skill"));
    }

    #[test]
    fn skills_no_match_reports_prefix_and_total() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, Some("zzz"));
        let msg = result.message.expect("expected message");
        assert!(
            msg.starts_with("No skills match prefix `zzz` (out of 1 available)."),
            "{msg}"
        );
    }

    #[test]
    fn skills_unfiltered_splits_user_and_bundled_tiers() {
        let mut group = FakeSkillGroup::new(vec![
            demo_entry(),
            bundled_entry("skill-creator", SkillBundledTier::FormatTooling),
            bundled_entry("help", SkillBundledTier::CoreAgentic),
        ]);
        let result = list_skills(&mut group, Some(""));
        let msg = result.message.expect("expected message");
        assert!(msg.contains("Your skills (1):"), "{msg}");
        assert!(msg.contains("Core agentic (1):"), "{msg}");
        assert!(msg.contains("  /help"), "{msg}");
        assert!(msg.contains("Format & tooling (1):"), "{msg}");
        assert!(msg.contains("  /skill-creator"), "{msg}");
        assert!(
            msg.contains("(run /skills <name> for details on a built-in)"),
            "{msg}"
        );
    }

    #[test]
    fn skills_rejects_flag_like_and_multiword_prefixes() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, Some("-x"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("Usage: /skills [--remote|sync|inspect|suggest <task>|<name-prefix>]")
        );
        let result = list_skills(&mut group, Some("two words"));
        assert!(result.is_error);
    }

    #[test]
    fn skills_suggest_requires_meaningful_task() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, Some("suggest"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("Usage: /skills suggest <task>")
        );
        let result = list_skills(&mut group, Some("suggest ab"));
        assert!(result.is_error);
        assert!(result.message.unwrap().contains("at least 3 characters"));
    }

    #[test]
    fn skills_inspect_reports_discovery_details() {
        let mut entry = demo_entry();
        entry.index_note = Some("Indice limité à 80 caractères".to_string());
        let mut group = FakeSkillGroup::new(vec![entry]);
        group.projection.bundled_status = vec![
            "Bundled skills: install=none".into(),
            "pdf: explicit-only by config (/owned/pdf/SKILL.md)".into(),
        ];
        let result = list_skills(&mut group, Some("inspect"));
        let msg = result.message.expect("expected message");
        assert!(msg.starts_with("Skills Inspect\n"), "{msg}");
        assert!(msg.contains("Discovery mode: compatible"));
        assert!(msg.contains("Workspace: /ws"));
        assert!(msg.contains("Configured skills dir: /ws/.codewhale/skills"));
        assert!(msg.contains("Searched directories (1):"));
        assert!(msg.contains("Available skills (1):"));
        assert!(msg.contains("source: native"));
        assert!(msg.contains("path: /ws/.codewhale/skills/demo"));
        assert!(msg.contains("Indice limité à 80 caractères"));
        assert!(msg.contains("Bundled skills: install=none"));
        assert!(msg.contains("pdf: explicit-only by config (/owned/pdf/SKILL.md)"));
    }

    #[test]
    fn skills_remote_lists_entries_and_policy_errors() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, Some("--remote"));
        let msg = result.message.expect("expected message");
        assert!(msg.contains("Available remote skills (1):"), "{msg}");
        assert!(msg.contains("remote-demo — Remote demo (source: github.com/acme/skills)"));
        assert!(msg.contains("\nInstall with: /skill install <name>"));

        group.remote = Ok(RemoteRegistryOutcome::NeedsApproval("acme.com".to_string()));
        let result = list_skills(&mut group, Some("remote"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("Network policy requires approval for acme.com")
        );

        group.remote = Ok(RemoteRegistryOutcome::Denied("acme.com".to_string()));
        let result = list_skills(&mut group, Some("remote"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("Network policy denied access to acme.com")
        );

        group.remote = Err("Failed to fetch registry: boom".to_string());
        let result = list_skills(&mut group, Some("--remote"));
        assert!(result.is_error);
        assert_eq!(
            result.message.unwrap(),
            "Error: Failed to fetch registry: boom"
        );
    }

    #[test]
    fn skills_suggest_renders_recommendations() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, Some("suggest demo"));
        let msg = result.message.expect("expected message");
        assert!(msg.contains("Suggested remote skills for `demo`:"), "{msg}");
        assert!(msg.contains("  remote-demo — Remote demo"));
        assert!(msg.contains("    Why: demo"));
        assert!(msg.contains("    Install if you want it: /skill install remote-demo"));
        assert!(msg.contains("\nNothing was installed, trusted, or enabled."));
    }

    #[test]
    fn skills_sync_renders_per_skill_report() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let result = list_skills(&mut group, Some("sync"));
        let msg = result.message.expect("expected message");
        assert!(msg.starts_with("Registry sync complete.\n"), "{msg}");
        assert!(msg.contains("  [+] demo — downloaded to /cache/demo"));
        assert!(msg.contains("\n1 skill(s) processed: 1 downloaded, 0 up-to-date, 0 failed."));

        group.sync = Ok(SkillSyncOutcome::RegistryNeedsApproval(
            "acme.com".to_string(),
        ));
        let result = list_skills(&mut group, Some("sync"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("requires approval for acme.com")
        );
    }

    // ── /skill parity ─────────────────────────────────────────────────────

    #[test]
    fn skill_without_arg_prints_usage() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, None);
        assert!(result.is_error);
        assert!(result.message.unwrap().contains("Usage: /skill <name>"));
    }

    #[test]
    fn skill_activation_success_composes_message_and_task_action() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, Some("demo"));
        assert!(!result.is_error);
        let msg = result.message.expect("expected message");
        assert!(
            msg.starts_with("Skill 'demo' activated.\n\nDescription: Demo skill"),
            "{msg}"
        );
        assert!(result.action.is_none());

        let result = run_skill(&mut group, &mut skills, Some("demo do the thing"));
        assert!(
            matches!(result.action, Some(AppAction::SendMessage(ref t)) if t == "do the thing")
        );
    }

    #[test]
    fn skill_new_aliases_skill_creator_in_not_found_message() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        group.activation = Err(SkillActivationError::NotFound {
            requested: "skill-creator".to_string(),
            available: vec!["demo".to_string()],
            warnings: vec![],
        });
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, Some("new"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("Skill 'skill-creator' not found.")
        );
    }

    #[test]
    fn skill_not_found_lists_available_and_warnings() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        group.activation = Err(SkillActivationError::NotFound {
            requested: "missing".to_string(),
            available: vec!["demo".to_string()],
            warnings: vec!["one warning".to_string()],
        });
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, Some("missing"));
        assert!(result.is_error);
        let msg = result.message.unwrap();
        assert!(msg.contains("Skill 'missing' not found."), "{msg}");
        assert!(msg.contains("Available skills: demo"), "{msg}");
        assert!(msg.contains("Warnings (1):"), "{msg}");
        assert!(msg.contains("  - one warning"), "{msg}");
    }

    #[test]
    fn skill_not_found_with_no_skills_uses_install_hint() {
        let mut group = FakeSkillGroup::new(vec![]);
        group.activation = Err(SkillActivationError::NotFound {
            requested: "missing".to_string(),
            available: vec![],
            warnings: vec![],
        });
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, Some("missing"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("No skills installed.\n\nUse /skills to see how to add skills.")
        );
    }

    #[test]
    fn skill_invocation_rejected_is_honest_and_preserves_plugin_denial() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        group.activation = Err(SkillActivationError::InvocationRejected {
            name: "demo".into(),
            reason: "frontmatter does not allow user invocation".into(),
        });
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, Some("demo"));
        assert!(result.is_error);
        assert_eq!(
            result.message.as_deref(),
            Some(
                "Error: Skill 'demo' could not be activated: frontmatter does not allow user invocation"
            )
        );
    }

    #[test]
    fn skill_plugin_rejected_renders_exact_error() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        group.activation = Err(SkillActivationError::PluginRejected {
            name: "plug".to_string(),
            reason: "authority revoked".to_string(),
        });
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, Some("plug"));
        assert!(result.is_error);
        assert_eq!(
            result.message.unwrap(),
            "Error: Plugin skill 'plug' is no longer active: authority revoked"
        );
    }

    #[test]
    fn skill_install_receipt_refreshes_cache_exactly_once() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let mut skills = CountingSkills::default();
        let result = run_skill(&mut group, &mut skills, Some("install github:acme/demo"));
        assert!(!result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .starts_with("Installed skill 'demo'.\nLocation: /ws/.codewhale/skills/demo"),
        );
        assert_eq!(
            skills.refresh_count, 1,
            "Installed receipt must refresh the skill cache exactly once"
        );
    }

    #[test]
    fn skill_update_and_uninstall_refresh_cache_exactly_once_each() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let mut skills = CountingSkills::default();
        let result = run_skill(&mut group, &mut skills, Some("update demo"));
        assert!(!result.is_error);
        assert_eq!(skills.refresh_count, 1, "update refresh count");

        skills.refresh_count = 0;
        let result = run_skill(&mut group, &mut skills, Some("uninstall --global demo"));
        assert!(!result.is_error);
        assert_eq!(skills.refresh_count, 1, "uninstall refresh count");
        assert!(result.message.unwrap().contains("Removed skill 'demo'."));
    }

    #[test]
    fn skill_trust_does_not_refresh_cache() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let mut skills = CountingSkills::default();
        let result = run_skill(&mut group, &mut skills, Some("trust demo"));
        assert!(!result.is_error);
        assert_eq!(skills.refresh_count, 0, "trust must not refresh the cache");
        assert!(
            result
                .message
                .unwrap()
                .contains("Marked skill 'demo' as trusted.")
        );
    }

    #[test]
    fn skill_install_empty_spec_prints_usage() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(&mut group, &mut skills, Some("install"));
        assert!(result.is_error);
        assert!(result.message.unwrap().contains("Usage: /skill install"));
    }

    #[test]
    fn skill_scope_conflict_errors() {
        let mut group = FakeSkillGroup::new(vec![demo_entry()]);
        let mut skills = FakeSkills { refreshed: false };
        let result = run_skill(
            &mut group,
            &mut skills,
            Some("install --project --global x"),
        );
        assert!(result.is_error);
        assert!(
            result
                .message
                .unwrap()
                .contains("specify at most one of --project / --global")
        );
    }

    #[test]
    fn skill_missing_facet_errors_are_safe() {
        let result = skills_contextual(CommandContexts::empty(), Some("demo"));
        assert!(result.is_error);
        assert_eq!(
            result.message.unwrap(),
            "Error: Command capability unavailable: skill_group"
        );
    }
}
