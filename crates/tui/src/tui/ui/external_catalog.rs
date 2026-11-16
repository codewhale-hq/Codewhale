use super::*;
use crate::tui::plugin_suggestions::ExternalCatalogChange;

fn skill_delta(before: &[(String, String)], after: &[(String, String)]) -> String {
    let names = |skills: &[(String, String)]| {
        skills
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let (before, after) = (names(before), names(after));
    let added = after.difference(&before).cloned().collect::<Vec<_>>();
    let removed = before.difference(&after).cloned().collect::<Vec<_>>();
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("skills added: {}", added.join(", ")));
    }
    if !removed.is_empty() {
        parts.push(format!("skills removed: {}", removed.join(", ")));
    }
    parts.join("; ")
}

pub(crate) async fn apply_external_catalog_change(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    config: &mut Config,
    change: ExternalCatalogChange,
) -> Result<()> {
    app.mark_external_catalog_applied();
    if change.plugins {
        let before = app.plugin_registry.clone();
        let skills_before = app.cached_skills.clone();
        let workspace = app.workspace.clone();
        app.plugin_registry = before.rediscover_for_workspace(&workspace);
        app.refresh_skill_cache();
        let mut message = String::from("Plugins changed outside this session and were applied");
        let delta = skill_delta(&skills_before, &app.cached_skills);
        if !delta.is_empty() {
            message.push_str(&format!(" ({delta})"));
        }
        message.push('.');
        if codewhale_plugins::provider_declarations_changed(
            before.as_ref(),
            app.plugin_registry.as_ref(),
        ) {
            message.push(' ');
            message.push_str(codewhale_plugins::PLUGIN_PROVIDER_NEW_SESSION_NOTICE);
        }
        let result = commands::CommandResult::with_message_and_action(
            message,
            AppAction::PluginRegistryChanged,
        );
        apply_command_result(terminal, app, engine_handle, task_manager, config, result).await?;
    }
    if change.mcp {
        let extensions_was_open = app.view_stack.extensions_is_top();
        handle_mcp_ui_action(
            app,
            engine_handle,
            config,
            crate::tui::app::McpUiAction::Reload,
        )
        .await;
        if !extensions_was_open && app.view_stack.extensions_is_top() {
            app.view_stack.pop();
        }
    }
    app.needs_redraw = true;
    Ok(())
}
