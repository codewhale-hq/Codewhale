use std::collections::{BTreeSet, HashMap};
use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use serde::Serialize;
use serde_json::Value;

use super::plugins::{self, PluginSummaryEntry};
use super::{ApiError, CommandsQuery, HooksQuery, RuntimeApiState};

const MAX_ADDED_NAMES: usize = 512;

#[derive(Debug, Serialize)]
pub(super) struct PluginIndexResponse {
    workspace: String,
    items: Vec<IndexItem>,
    unavailable: Vec<IndexNote>,
    problems: Vec<IndexNote>,
}

#[derive(Debug, Serialize)]
struct IndexNote {
    kind: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct IndexItem {
    id: String,
    kind: &'static str,
    name: String,
    description: Option<String>,
    source: &'static str,
    state: &'static str,
    labels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adds: Option<PluginAdds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<&'static str>,
    owner: &'static str,
    row: Value,
}

#[derive(Debug, Default, Serialize)]
struct PluginAdds {
    skills: Vec<String>,
    connectors: Vec<String>,
    hooks: Vec<String>,
    commands: Vec<String>,
    profiles: Vec<String>,
}

impl PluginAdds {
    fn labels(&self) -> Vec<&'static str> {
        [
            ("skill", !self.skills.is_empty()),
            ("connector", !self.connectors.is_empty()),
            ("hook", !self.hooks.is_empty()),
            ("command", !self.commands.is_empty()),
            ("profile", !self.profiles.is_empty()),
        ]
        .into_iter()
        .filter_map(|(label, present)| present.then_some(label))
        .collect()
    }
}

fn row_value<T: Serialize>(row: &T) -> Value {
    serde_json::to_value(row).unwrap_or(Value::Null)
}

fn note_failure(unavailable: &mut Vec<IndexNote>, kind: &'static str, error: &ApiError) {
    unavailable.push(IndexNote {
        kind,
        message: error.message.clone(),
    });
}

fn plugin_source(origin: &str) -> &'static str {
    match origin {
        "codewhale-builtin" => "built_in",
        "workspace-codewhale" => "project",
        _ => "user",
    }
}

fn plugin_state(entry: &PluginSummaryEntry) -> &'static str {
    if matches!(
        entry.trust_status,
        "content-changed" | "capabilities-changed"
    ) || entry.state == "enabled-untrusted"
    {
        return "needs_review";
    }
    match entry.state {
        "active" => "enabled",
        "disabled" => "disabled",
        _ => "unavailable",
    }
}

fn add_stem(names: &mut BTreeSet<String>, path: &FsPath, extension: Option<&str>, lowercase: bool) {
    if let Some(wanted) = extension
        && path.extension().and_then(|value| value.to_str()) != Some(wanted)
    {
        return;
    }
    if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
        names.insert(if lowercase {
            stem.to_lowercase()
        } else {
            stem.to_string()
        });
    }
}

async fn component_names(
    paths: &[PathBuf],
    extension: Option<&str>,
    lowercase: bool,
) -> Vec<String> {
    let mut names = BTreeSet::new();
    for path in paths {
        let Ok(meta) = tokio::fs::symlink_metadata(path).await else {
            continue;
        };
        if meta.is_file() {
            add_stem(&mut names, path, extension, lowercase);
        } else if meta.is_dir()
            && let Ok(mut entries) = tokio::fs::read_dir(path).await
        {
            let mut seen = 0;
            while let Ok(Some(entry)) = entries.next_entry().await {
                seen += 1;
                if seen > MAX_ADDED_NAMES {
                    break;
                }
                if entry.file_type().await.is_ok_and(|kind| kind.is_file()) {
                    add_stem(&mut names, &entry.path(), extension, lowercase);
                }
            }
        }
        if names.len() >= MAX_ADDED_NAMES {
            break;
        }
    }
    names.into_iter().take(MAX_ADDED_NAMES).collect()
}

async fn plugin_adds(plugin: &codewhale_plugins::types::LoadedPlugin) -> PluginAdds {
    let mut skills: Vec<String> = plugin
        .skill_snapshots
        .iter()
        .map(|skill| skill.name.clone())
        .collect();
    skills.sort();
    let mut connectors: Vec<String> = plugin
        .manifest
        .mcp_servers
        .as_ref()
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default();
    connectors.sort();
    PluginAdds {
        skills,
        connectors,
        hooks: component_names(&plugin.components.hooks, None, false).await,
        commands: component_names(&plugin.components.commands, Some("md"), true).await,
        profiles: component_names(&plugin.components.agents, Some("toml"), false).await,
    }
}

async fn plugin_adds_by_id(state: &RuntimeApiState) -> HashMap<String, PluginAdds> {
    let registry = state
        .plugin_discovery
        .registry_for_workspace(&state.workspace);
    let mut adds = HashMap::new();
    for plugin in registry.list() {
        adds.insert(plugin.id.as_str().to_string(), plugin_adds(plugin).await);
    }
    adds
}

fn skill_source(
    path: Option<&PathBuf>,
    is_bundled: bool,
    skills_dir: &FsPath,
    workspace: &FsPath,
) -> &'static str {
    if is_bundled {
        return "built_in";
    }
    match path {
        Some(path) if path.starts_with(skills_dir) => "user",
        Some(path) if path.starts_with(workspace) => "project",
        Some(_) => "other_app",
        None => "plugin",
    }
}

/// `GET /v1/apps/plugin-index`: one list across every kind of plugin. Each
/// row embeds the row its owner's own route returns, so a field an owner adds
/// reaches this route without a second edit; an owner that fails is named
/// under `unavailable` and the rest still list.
pub(super) async fn get_plugin_index(
    State(state): State<RuntimeApiState>,
) -> Result<Json<PluginIndexResponse>, ApiError> {
    let mut items = Vec::new();
    let mut unavailable = Vec::new();
    let mut problems = Vec::new();

    match plugins::list_plugins(State(state.clone())).await {
        Ok(Json(response)) => {
            let mut adds_by_id = plugin_adds_by_id(&state).await;
            for diagnostic in &response.registry_diagnostics {
                problems.push(IndexNote {
                    kind: "plugin",
                    message: diagnostic.message.clone(),
                });
            }
            for entry in &response.plugins {
                let adds = adds_by_id.remove(&entry.id);
                items.push(IndexItem {
                    id: format!("plugin:{}", entry.id),
                    kind: "plugin",
                    name: entry
                        .display_name
                        .clone()
                        .unwrap_or_else(|| entry.name.clone()),
                    description: entry.description.clone(),
                    source: plugin_source(entry.origin),
                    state: plugin_state(entry),
                    labels: adds.as_ref().map(PluginAdds::labels).unwrap_or_default(),
                    plugin: None,
                    adds,
                    tier: None,
                    owner: "/v1/apps/plugins",
                    row: row_value(entry),
                });
            }
        }
        Err(error) => note_failure(&mut unavailable, "plugin", &error),
    }

    match plugins::list_plugin_catalog(State(state.clone())).await {
        Ok(Json(catalog)) => {
            for (tier, rows) in [
                ("builtin", &catalog.builtin),
                ("experimental", &catalog.experimental),
                ("more", &catalog.more),
            ] {
                for row in rows {
                    items.push(IndexItem {
                        id: format!("plugin:catalog:{}", row.id),
                        kind: "plugin",
                        name: row.title.clone(),
                        description: row.description.clone(),
                        source: "built_in",
                        state: if !row.available {
                            "unavailable"
                        } else if row.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        labels: Vec::new(),
                        plugin: None,
                        adds: None,
                        tier: Some(tier),
                        owner: "/v1/apps/plugins/catalog",
                        row: row_value(row),
                    });
                }
            }
        }
        Err(error) => note_failure(&mut unavailable, "plugin", &error),
    }

    match super::list_skills(State(state.clone())).await {
        Ok(Json(response)) => {
            for warning in &response.warnings {
                problems.push(IndexNote {
                    kind: "skill",
                    message: warning.clone(),
                });
            }
            for skill in &response.skills {
                if skill.plugin_id.is_some() {
                    continue;
                }
                items.push(IndexItem {
                    id: format!("skill:{}", skill.name),
                    kind: "skill",
                    name: skill.name.clone(),
                    description: Some(skill.description.clone()),
                    source: skill_source(
                        skill.path.as_ref(),
                        skill.is_bundled,
                        &response.directory,
                        &state.workspace,
                    ),
                    state: if skill.enabled { "enabled" } else { "disabled" },
                    labels: vec!["skill"],
                    plugin: None,
                    adds: None,
                    tier: None,
                    owner: "/v1/skills",
                    row: row_value(skill),
                });
            }
        }
        Err(error) => note_failure(&mut unavailable, "skill", &error),
    }

    match super::list_mcp_servers(State(state.clone())).await {
        Ok(Json(response)) => {
            for server in &response.servers {
                let source = match server.origin {
                    "plugin" => continue,
                    "project" => "project",
                    "runtime" => "runtime",
                    _ => "user",
                };
                items.push(IndexItem {
                    id: format!("connector:{}", server.name),
                    kind: "connector",
                    name: server.name.clone(),
                    description: None,
                    source,
                    state: if !server.enabled {
                        "disabled"
                    } else if server.auth_required {
                        "needs_sign_in"
                    } else {
                        "enabled"
                    },
                    labels: vec!["connector"],
                    plugin: None,
                    adds: None,
                    tier: None,
                    owner: "/v1/apps/mcp/servers",
                    row: row_value(server),
                });
            }
        }
        Err(error) => note_failure(&mut unavailable, "connector", &error),
    }

    match super::list_hooks(State(state.clone()), Query(HooksQuery { thread_id: None })).await {
        Ok(Json(response)) => {
            for problem in &response.problems {
                problems.push(IndexNote {
                    kind: "hook",
                    message: problem.clone(),
                });
            }
            let mut ordinals: HashMap<&'static str, usize> = HashMap::new();
            for hook in &response.hooks {
                let ordinal = ordinals.entry(hook.event).or_default();
                *ordinal += 1;
                let source = match hook.source {
                    "plugin" => continue,
                    "project" => "project",
                    _ => "user",
                };
                let name = hook
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{} {}", hook.event, ordinal));
                items.push(IndexItem {
                    id: format!("hook:{source}:{}:{name}", hook.event),
                    kind: "hook",
                    name,
                    description: None,
                    source,
                    state: if response.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    labels: vec!["hook"],
                    plugin: None,
                    adds: None,
                    tier: None,
                    owner: "/v1/hooks",
                    row: row_value(hook),
                });
            }
        }
        Err(error) => note_failure(&mut unavailable, "hook", &error),
    }

    match super::list_commands(
        State(state.clone()),
        Query(CommandsQuery { thread_id: None }),
    )
    .await
    {
        Ok(Json(response)) => {
            for command in &response.commands {
                let (source, plugin) = match (command.kind, command.plugin.as_ref()) {
                    ("builtin", _) => continue,
                    (_, Some(_)) if command.kind == "user" => continue,
                    ("extension", _) => ("plugin", command.origin.clone()),
                    _ => ("user", None),
                };
                items.push(IndexItem {
                    id: format!("command:{}", command.name),
                    kind: "command",
                    name: command.name.clone(),
                    description: command.summary.clone(),
                    source,
                    state: "enabled",
                    labels: vec!["command"],
                    plugin,
                    adds: None,
                    tier: None,
                    owner: "/v1/commands",
                    row: row_value(command),
                });
            }
        }
        Err(error) => note_failure(&mut unavailable, "command", &error),
    }

    Ok(Json(PluginIndexResponse {
        workspace: state.workspace.display().to_string(),
        items,
        unavailable,
        problems,
    }))
}
