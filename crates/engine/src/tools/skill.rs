//! `load_skill` tool — fetch a `SKILL.md` body and its companion-file
//! list into the model's context (#434).
//!
//! Skills are discovered through this tool, not a prompt index: `name="list"`
//! lists project and Codewhale-native skills with a compact summary of
//! external-harness roots, `query` searches across all skills, and each full
//! body is loaded only by exact name.
//!
//! `load_skill name=<id>` is the canonical progressive-disclosure path. It
//! performs a name-based host lookup, so native global skills work without
//! widening the model's workspace file authority, and it enumerates companion
//! files without a separate `list_dir`. Reviewed plugin skills are exposed
//! only through this tool's content-bound in-memory snapshot; their mutable
//! source paths and companion files are deliberately not returned.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use serde_json::{Value, json};

use codewhale_skills::{
    DEFAULT_SKILL_DESCRIPTION_CHARS, Skill, SkillCatalogGroup, SkillDiscoveryMode,
    SkillRootCatalog, SkillSource, discover_for_workspace, skill_directories_for_workspace_and_dir,
    skill_root_catalog_for_workspace, skills_directories_for_mode,
};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

const MAX_QUERY_RESULTS: usize = 25;
const MAX_UNKNOWN_SKILL_HINTS: usize = 20;

pub struct LoadSkillTool;

#[async_trait]
impl ToolSpec for LoadSkillTool {
    fn name(&self) -> &'static str {
        "load_skill"
    }

    fn description(&self) -> &'static str {
        "Load a named skill's SKILL.md body and companion file list into this turn. There is \
         no skills index in the prompt: pass query=\"...\" to search names, aliases, and descriptions, \
         or name=\"list\" for project and Codewhale skills. Use include_external=true to list \
         global compatible skills too. Load the matching skill by name before \
         starting that work. Resolves global and plugin skills that `read` cannot reach."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill id to load. Omit or pass \"list\" for the compact catalogue, or \"list-all\" to include global compatible skills."
                },
                "query": {
                    "type": "string",
                    "description": "Words matched against skill names, aliases, and descriptions, including global compatible skills."
                },
                "include_external": {
                    "type": "boolean",
                    "description": "Include individual global compatible skills in the list instead of only their counts."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let context = context.clone();
        #[cfg(any(test, feature = "test-support"))]
        let env_ticket = crate::test_support::env_scope_ticket();
        tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            Self::execute_sync(input, &context)
        })
        .await
        .map_err(|error| ToolError::execution_failed(format!("load skill task failed: {error}")))?
    }
}

impl LoadSkillTool {
    fn execute_sync(input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();

        let discovery_mode =
            codewhale_skills::SkillDiscoveryMode::from_config(&context.skills_config);
        let registry = discover_for_workspace(
            &context.workspace,
            context.skills_dir.as_deref(),
            &context.skills_config,
            context.plugin_registry.as_deref(),
        )
        .into_enabled();

        let raw_query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let include_external = input
            .get("include_external")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let list_all_alias = name.eq_ignore_ascii_case("list-all") && registry.get(name).is_none();
        let is_list_request = name.is_empty() || name == "*" || name == "list" || list_all_alias;

        if !raw_query.is_empty() || is_list_request {
            let (catalog, home) = skill_root_catalog_for_workspace(
                &context.workspace,
                context.skills_dir.as_deref(),
                &context.skills_config,
            );
            let all: Vec<&Skill> = registry
                .list()
                .iter()
                .filter(|skill| skill.invocation.model_invocable())
                .collect();
            if !raw_query.is_empty() {
                return Ok(ToolResult::success(render_query_results(
                    raw_query,
                    &all,
                    &catalog,
                    &context.workspace,
                    home.as_deref(),
                )));
            }
            return Ok(ToolResult::success(render_skill_listing(
                &all,
                &catalog,
                &context.workspace,
                home.as_deref(),
                include_external || list_all_alias,
            )));
        }

        let Some(skill) = registry.get(name) else {
            let (catalog, home) = skill_root_catalog_for_workspace(
                &context.workspace,
                context.skills_dir.as_deref(),
                &context.skills_config,
            );
            let available: Vec<&Skill> = registry
                .list()
                .iter()
                .filter(|s| s.invocation.model_invocable())
                .collect();
            let hint = if available.is_empty() {
                let dirs: Vec<String> = context
                    .skills_dir
                    .as_deref()
                    .map(|skills_dir| {
                        skill_directories_for_workspace_and_dir(
                            &context.workspace,
                            skills_dir,
                            discovery_mode,
                        )
                    })
                    .unwrap_or_else(|| {
                        skills_directories_for_mode(&context.workspace, discovery_mode)
                    })
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                if dirs.is_empty() {
                    if discovery_mode == SkillDiscoveryMode::CodeWhaleOnly {
                        "no skills directories found; install skills under `<workspace>/.codewhale/skills/<name>/SKILL.md` or `~/.codewhale/skills/<name>/SKILL.md`"
                                .to_string()
                    } else {
                        "no skills directories found; install skills under `<workspace>/.agents/skills/<name>/SKILL.md`, `~/.codewhale/skills/<name>/SKILL.md`, or `~/.deepseek/skills/<name>/SKILL.md`"
                                .to_string()
                    }
                } else {
                    format!("no skills installed. Searched: {}", dirs.join(", "))
                }
            } else {
                format_unknown_skill_hint(
                    name,
                    &available,
                    &catalog,
                    &context.workspace,
                    home.as_deref(),
                )
            };
            return Err(ToolError::execution_failed(hint));
        };

        if !skill.invocation.model_invocable() {
            return Err(ToolError::execution_failed(format!(
                "Skill `{}` does not allow model invocation; ask the user to invoke an enabled skill explicitly",
                skill.name
            )));
        }
        ensure_reviewed_plugin_skill_is_current_for(
            skill,
            &context.workspace,
            context.plugin_registry.as_deref(),
        )?;
        ensure_native_skill_file_present(skill)?;
        let body = format_skill_body(skill);
        let (skill_path, skill_source) = match &skill.source {
            SkillSource::Native => (Some(skill.path.display().to_string()), "native".to_string()),
            SkillSource::Plugin {
                plugin_id,
                plugin_name,
                ..
            } => (
                None,
                format!("reviewed-plugin-snapshot:{plugin_name}:{plugin_id}"),
            ),
        };
        Ok(ToolResult::success(body).with_metadata(json!({
            "skill_name": skill.name,
            "skill_path": skill_path,
            "skill_source": skill_source,
            "companion_files": collect_companion_files(skill)
                .into_iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<String>>(),
        })))
    }
}

fn compact_skill_description(description: &str) -> String {
    let single_line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= DEFAULT_SKILL_DESCRIPTION_CHARS {
        return single_line;
    }
    let mut out: String = single_line
        .chars()
        .take(DEFAULT_SKILL_DESCRIPTION_CHARS.saturating_sub(1))
        .collect();
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('…');
    out
}

fn format_skill_list_line(skill: &Skill, external_source: Option<&str>) -> String {
    let description = compact_skill_description(&skill.description);
    match (description.is_empty(), external_source) {
        (true, None) => format!("  - {}\n", skill.name),
        (true, Some(source)) => format!("  - {} ({source})\n", skill.name),
        (false, None) => format!("  - {} — {description}\n", skill.name),
        (false, Some(source)) => format!("  - {} — {description} ({source})\n", skill.name),
    }
}

fn render_skill_listing(
    skills: &[&Skill],
    catalog: &SkillRootCatalog,
    workspace: &std::path::Path,
    home_dir: Option<&std::path::Path>,
    include_external: bool,
) -> String {
    if skills.is_empty() {
        return "No skills installed.".to_string();
    }
    let mut primary: Vec<&Skill> = Vec::new();
    let mut external: Vec<(&Skill, String)> = Vec::new();
    for skill in skills {
        match catalog.classify_skill(skill, workspace, home_dir) {
            SkillCatalogGroup::Primary => primary.push(skill),
            SkillCatalogGroup::External { source_label } => external.push((skill, source_label)),
        }
    }

    let mut listing = String::new();
    if !primary.is_empty() {
        listing.push_str(&format!("Available skills ({}):\n", primary.len()));
        for skill in &primary {
            listing.push_str(&format_skill_list_line(skill, None));
        }
    } else if !include_external {
        listing.push_str("No project or Codewhale-native skills installed.\n");
    }

    if !external.is_empty() {
        if !listing.is_empty() {
            listing.push('\n');
        }
        if include_external {
            listing.push_str(&format!("External harness skills ({}):\n", external.len()));
            for (skill, source_label) in &external {
                listing.push_str(&format_skill_list_line(skill, Some(source_label)));
            }
        } else {
            let mut by_source: BTreeMap<&str, usize> = BTreeMap::new();
            for (_, source_label) in &external {
                *by_source.entry(source_label.as_str()).or_insert(0) += 1;
            }
            let summary = by_source
                .into_iter()
                .map(|(source, count)| format!("{source} ({count})"))
                .collect::<Vec<_>>()
                .join(", ");
            listing.push_str(&format!(
                "External harness skills ({} omitted from default list: {summary}).\n\
                 Use `load_skill` with `query=\"...\"` to search all skills, `name=\"list-all\"` (or `include_external=true`) to list external skills, or `name=\"<skill>\"` to load any skill directly.\n",
                external.len()
            ));
        }
    }

    listing
}

fn tokenize_search_terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut tokens = Vec::new();
    for part in text.split(|ch: char| !ch.is_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        let lower = part.to_lowercase();
        if seen.insert(lower.clone()) {
            tokens.push(lower);
        }
    }
    if tokens.is_empty() {
        let trimmed = text.trim().to_lowercase();
        if !trimmed.is_empty() {
            tokens.push(trimmed);
        }
    }
    tokens
}

fn score_skill_query(
    skill: &Skill,
    query_lower: &str,
    query_tokens: &[String],
) -> Option<(usize, u32)> {
    let name_lower = skill.name.to_lowercase();
    let name_tokens: HashSet<String> = tokenize_search_terms(&name_lower).into_iter().collect();
    let aliases_lower: Vec<String> = skill.aliases.iter().map(|a| a.to_lowercase()).collect();
    let alias_tokens: HashSet<String> = aliases_lower
        .iter()
        .flat_map(|alias| tokenize_search_terms(alias))
        .collect();
    let desc_lower = skill.description.to_lowercase();
    let localized_lower: Vec<String> = skill
        .localized_descriptions
        .values()
        .map(|value| value.to_lowercase())
        .collect();
    let mut desc_tokens: HashSet<String> = tokenize_search_terms(&desc_lower).into_iter().collect();
    for localized in &localized_lower {
        desc_tokens.extend(tokenize_search_terms(localized));
    }

    let mut matched_tokens = 0usize;
    let mut score = 0u32;
    for token in query_tokens {
        let token_score = if name_lower == *token {
            100
        } else if name_tokens.contains(token) {
            60
        } else if alias_tokens.contains(token) {
            50
        } else if name_lower.contains(token) {
            40
        } else if aliases_lower.iter().any(|alias| alias.contains(token)) {
            30
        } else if desc_tokens.contains(token) {
            20
        } else if desc_lower.contains(token)
            || localized_lower.iter().any(|text| text.contains(token))
        {
            10
        } else {
            0
        };
        if token_score > 0 {
            matched_tokens += 1;
            score += token_score;
        }
    }
    if matched_tokens == 0 {
        return None;
    }
    if name_lower == query_lower {
        score += 200;
    } else if aliases_lower.iter().any(|alias| alias == query_lower) {
        score += 150;
    }
    Some((matched_tokens, score))
}

fn render_query_results(
    raw_query: &str,
    skills: &[&Skill],
    catalog: &SkillRootCatalog,
    workspace: &std::path::Path,
    home_dir: Option<&std::path::Path>,
) -> String {
    let query_lower = raw_query.to_lowercase();
    let query_tokens = tokenize_search_terms(raw_query);

    let mut ranked: Vec<_> = skills
        .iter()
        .filter_map(|skill| {
            let score = score_skill_query(skill, &query_lower, &query_tokens)?;
            let (is_primary, external_source) =
                match catalog.classify_skill(skill, workspace, home_dir) {
                    SkillCatalogGroup::Primary => (true, None),
                    SkillCatalogGroup::External { source_label } => (false, Some(source_label)),
                };
            Some((score, is_primary, *skill, external_source))
        })
        .collect();

    if ranked.is_empty() {
        return format!(
            "No skill matches {query_lower:?}. Pass name=\"list\" for project/Codewhale skills or refine your query."
        );
    }

    ranked.sort_by_key(|(score, primary, skill, _)| {
        (Reverse(*score), Reverse(*primary), &skill.name)
    });

    let total_matches = ranked.len();
    let shown = ranked
        .into_iter()
        .take(MAX_QUERY_RESULTS)
        .collect::<Vec<_>>();
    let mut listing = if total_matches > shown.len() {
        format!(
            "Skills matching {query_lower:?} (showing {} of {total_matches}):\n",
            shown.len()
        )
    } else {
        format!("Skills matching {query_lower:?} ({}):\n", shown.len())
    };
    for (_, _, skill, external_source) in &shown {
        listing.push_str(&format_skill_list_line(skill, external_source.as_deref()));
    }
    if total_matches > shown.len() {
        listing.push_str(&format!(
            "  ... {} more match(es); refine `query` to narrow results.\n",
            total_matches - shown.len()
        ));
    }
    listing
}

fn format_unknown_skill_hint(
    name: &str,
    available: &[&Skill],
    catalog: &SkillRootCatalog,
    workspace: &std::path::Path,
    home_dir: Option<&std::path::Path>,
) -> String {
    let mut primary: Vec<&str> = Vec::new();
    let mut external: Vec<&str> = Vec::new();
    for skill in available {
        match catalog.classify_skill(skill, workspace, home_dir) {
            SkillCatalogGroup::Primary => primary.push(skill.name.as_str()),
            SkillCatalogGroup::External { .. } => external.push(skill.name.as_str()),
        }
    }
    if !primary.is_empty() {
        let mut shown = primary
            .iter()
            .take(MAX_UNKNOWN_SKILL_HINTS)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        if primary.len() > MAX_UNKNOWN_SKILL_HINTS {
            shown.push_str(&format!(
                " (+{} more)",
                primary.len() - MAX_UNKNOWN_SKILL_HINTS
            ));
        }
        if external.is_empty() {
            format!("skill `{name}` not found. Available: {shown}")
        } else {
            format!(
                "skill `{name}` not found. Available: {shown} (plus {} external harness skill(s); use `load_skill` with `query=\"...\"` to search)",
                external.len()
            )
        }
    } else {
        let mut shown = external
            .iter()
            .take(MAX_UNKNOWN_SKILL_HINTS)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        if external.len() > MAX_UNKNOWN_SKILL_HINTS {
            shown.push_str(&format!(
                " (+{} more; use `load_skill` with `query=\"...\"`)",
                external.len() - MAX_UNKNOWN_SKILL_HINTS
            ));
        }
        format!("skill `{name}` not found. Available: {shown}")
    }
}

/// A native registry entry whose SKILL.md vanished from disk after discovery
/// (deleted, or resolved under a wrong home directory) must fail loudly with
/// the exact path — never silently serve the stale cached body while the user
/// believes the skill loaded (§2.5).
fn ensure_native_skill_file_present(skill: &Skill) -> Result<(), ToolError> {
    if !matches!(skill.source, SkillSource::Native) || skill.path.is_file() {
        return Ok(());
    }
    let message = format!(
        "Skill `{}` is registered at {} but that file no longer exists on disk, \
         so the skill did not load. Restore the file, or fix the skills directory it \
         came from (`skills_dir` in config.toml, `$CODEWHALE_HOME`, or the OS home) — \
         the path above shows exactly where the runtime looked.",
        skill.name,
        skill.path.display()
    );
    codewhale_runtime::logging::warn(&message);
    Err(ToolError::execution_failed(message))
}

#[cfg(test)]
fn ensure_reviewed_plugin_skill_is_current(
    skill: &Skill,
    workspace: &std::path::Path,
) -> Result<(), ToolError> {
    ensure_reviewed_plugin_skill_is_current_for(skill, workspace, None)
}
fn ensure_reviewed_plugin_skill_is_current_for(
    skill: &Skill,
    workspace: &std::path::Path,
    plugins: Option<&codewhale_plugins::PluginRegistry>,
) -> Result<(), ToolError> {
    let Some(provenance) = skill.source.provenance() else {
        return Ok(());
    };
    provenance.verify_for(workspace,plugins).map_err(|reason| ToolError::execution_failed(format!(
        "Plugin skill `{}` was denied: {reason}. Reload and select the skill again before retrying", skill.name)))
}

/// Render the skill body the model will see. Includes the description
/// up top so a single tool result is self-contained — no need to
/// cross-reference the system-prompt catalogue. Companion-file paths
/// land at the bottom under a clearly-named heading so the model can
/// open them with `read_file` if they're relevant to the task.
pub(crate) fn format_skill_body(skill: &Skill) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Skill: {}\n\n", skill.name));
    if !skill.description.trim().is_empty() {
        out.push_str(&format!("> {}\n\n", skill.description.trim()));
    }
    let invocation = match skill.invocation {
        codewhale_skill_install::skill_parse::SkillInvocation::ModelAndUser => "model+user",
        codewhale_skill_install::skill_parse::SkillInvocation::ExplicitOnly => "explicit-only",
        codewhale_skill_install::skill_parse::SkillInvocation::ModelOnly => "model-only",
        codewhale_skill_install::skill_parse::SkillInvocation::Disabled => "disabled",
    };
    out.push_str(&format!("Invocation: `{invocation}`\n"));
    if !skill.aliases.is_empty() {
        out.push_str(&format!("Aliases: `{}`\n", skill.aliases.join("`, `")));
    }
    out.push('\n');
    match &skill.source {
        SkillSource::Native => out.push_str(&format!("Source: `{}`\n\n", skill.path.display())),
        SkillSource::Plugin {
            plugin_id,
            plugin_name,
            ..
        } => out.push_str(&format!(
            "Source: reviewed in-memory plugin snapshot `{plugin_name}` ({plugin_id})\n\n"
        )),
    }
    out.push_str("## SKILL.md\n\n");
    out.push_str(skill.body.trim());
    out.push('\n');
    if matches!(skill.source, SkillSource::Native)
        && let Some(context) =
            codewhale_skills::bundled_skill_load_context(&skill.name, &skill.path)
    {
        out.push_str(&format!(
            "\nBase directory for this skill: `{}`\n",
            context.base_dir.display()
        ));
        if let Some(notes) = context.host_notes {
            out.push_str("\n## Host notes: Codewhale\n\n");
            out.push_str(notes.trim());
            out.push('\n');
        }
    }

    let companions = collect_companion_files(skill);
    if !companions.is_empty() {
        out.push_str("\n## Companion files\n\n");
        out.push_str(
            "Sibling files in the skill directory. Open one with `read` (path=...) when the task requires it; a skill stored outside the workspace has to be read through `bash` instead.\n\n",
        );
        for path in &companions {
            out.push_str(&format!("- `{}`\n", path.display()));
        }
    }
    out
}

pub(crate) fn bundled_creator_skill_body(name: &str) -> Option<String> {
    codewhale_skills::bundled_creator_skill(name).map(|skill| format_skill_body(&skill))
}

/// List sibling files of `SKILL.md` in the skill's own directory.
/// Skips the `SKILL.md` itself and any nested directories so the
/// listing stays focused on at-hand resources. Sorted lexically for
/// deterministic output (matters for transcript diffing in tests).
fn collect_companion_files(skill: &Skill) -> Vec<std::path::PathBuf> {
    if matches!(&skill.source, SkillSource::Plugin { .. }) {
        // Companion files remain hashed, but exposing their mutable on-disk
        // paths would let content change after review and bypass the snapshot.
        return Vec::new();
    }
    let Some(dir) = skill.path.parent() else {
        return Vec::new();
    };
    let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let is_file = entry.file_type().is_ok_and(|ft| ft.is_file());
                let is_skill_md = path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md");
                if is_file && !is_skill_md {
                    Some(path)
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_skills::SkillRegistry;
    use std::fs;
    use tempfile::tempdir;

    fn write_skill(dir: &std::path::Path, name: &str, description: &str, body: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn load_skill_returns_skill_body_with_description_header() {
        let tmp = tempdir().unwrap();
        write_skill(
            tmp.path(),
            "review-pr",
            "Run a focused PR review",
            "# Steps\n1. Read the diff.\n2. Comment.\n",
        );
        let skill = SkillRegistry::discover(tmp.path())
            .get("review-pr")
            .unwrap()
            .clone();
        let body = format_skill_body(&skill);
        assert!(body.contains("# Skill: review-pr"));
        assert!(body.contains("Run a focused PR review"));
        assert!(body.contains("# Steps"));
        assert!(body.contains("Read the diff."));
    }

    #[test]
    fn collect_companion_files_lists_siblings_excluding_skill_md() {
        let tmp = tempdir().unwrap();
        let skill_dir = tmp.path().join("rich-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: rich-skill\ndescription: x\n---\nbody\n",
        )
        .unwrap();
        fs::write(skill_dir.join("script.py"), "print('hi')").unwrap();
        fs::write(skill_dir.join("data.json"), "{}").unwrap();
        // Nested directory — skipped by collect_companion_files.
        fs::create_dir_all(skill_dir.join("subdir")).unwrap();

        let registry = SkillRegistry::discover(tmp.path());
        let skill = registry.get("rich-skill").unwrap();
        let files = collect_companion_files(skill);
        let names: Vec<String> = files
            .iter()
            .filter_map(|p| p.file_name().and_then(|s| s.to_str().map(str::to_string)))
            .collect();
        assert_eq!(
            names,
            vec!["data.json".to_string(), "script.py".to_string()]
        );
    }

    #[test]
    fn native_skill_with_vanished_file_fails_loudly_with_the_path() {
        // §2.5: a registry entry pointing at a SKILL.md that no longer exists
        // must surface the exact path instead of silently serving the stale
        // cached body — this is the "delegate skill silently never loads"
        // symptom class.
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("delegate").join("SKILL.md");
        let skill = Skill {
            legacy_activation_name: None,
            name: "delegate".to_string(),
            description: "delegate work".to_string(),
            localized_descriptions: std::collections::HashMap::new(),
            invocation: codewhale_skill_install::skill_parse::SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: "cached body".to_string(),
            path: missing.clone(),
            source: SkillSource::Native,
        };
        let err = ensure_native_skill_file_present(&skill)
            .expect_err("a vanished SKILL.md must fail loudly");
        let message = err.to_string();
        assert!(
            message.contains(&missing.display().to_string()),
            "error names the exact path: {message}"
        );
        assert!(
            message.contains("did not load"),
            "error says the skill did not load: {message}"
        );

        // An existing file passes, and plugin skills are untouched (their
        // content-bound snapshot never consults the mutable path).
        let present_dir = tempdir().unwrap();
        let present = present_dir.path().join("SKILL.md");
        fs::write(&present, "body").unwrap();
        let mut on_disk = skill.clone();
        on_disk.path = present;
        ensure_native_skill_file_present(&on_disk).expect("present file loads");
        let mut plugin = skill;
        plugin.source = SkillSource::Plugin {
            plugin_id: "workspace/1/demo".to_string(),
            plugin_name: "demo".to_string(),
            native_registration: None,
            authority: Box::new(codewhale_config::PluginAuthority {
                plugin_id: codewhale_config::PluginId("workspace/1/demo".to_string()),
                plugin_name: "demo".to_string(),
                workspace: tmp.path().to_path_buf(),
                state_path: tmp.path().join("state.json"),
                managed_policy_path: Some(tmp.path().join("managed-policy.json")),
                managed_policy_source: Some(tmp.path().join("managed-policy.json")),
                source_manifest: tmp.path().join("plugin.toml"),
                staged_manifest: tmp.path().join("staged/plugin.toml"),
                content_hash: "0".repeat(64),
                capability_hash: "0".repeat(64),
                state_generation: 0,
            }),
        };
        ensure_native_skill_file_present(&plugin).expect("plugin snapshot skips the disk check");
    }

    #[test]
    fn plugin_skill_body_uses_reviewed_snapshot_without_mutable_file_paths() {
        let tmp = tempdir().unwrap();
        let skill_path = tmp.path().join("SKILL.md");
        fs::write(&skill_path, "changed on disk").unwrap();
        fs::write(tmp.path().join("companion.txt"), "changed companion").unwrap();
        let skill = Skill {
            legacy_activation_name: None,
            name: "demo:hello".to_string(),
            description: "hello".to_string(),
            localized_descriptions: std::collections::HashMap::new(),
            invocation: codewhale_skill_install::skill_parse::SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: "reviewed body".to_string(),
            path: skill_path.clone(),
            source: SkillSource::Plugin {
                plugin_id: "workspace/123/demo".to_string(),
                plugin_name: "demo".to_string(),
                native_registration: None,
                authority: Box::new(codewhale_config::PluginAuthority {
                    plugin_id: codewhale_config::PluginId("workspace/123/demo".to_string()),
                    plugin_name: "demo".to_string(),
                    workspace: tmp.path().to_path_buf(),
                    state_path: tmp.path().join("state.json"),
                    managed_policy_path: Some(tmp.path().join("managed-policy.json")),
                    managed_policy_source: Some(tmp.path().join("managed-policy.json")),
                    source_manifest: tmp.path().join("plugin.toml"),
                    staged_manifest: tmp.path().join("staged/plugin.toml"),
                    content_hash: "0".repeat(64),
                    capability_hash: "0".repeat(64),
                    state_generation: 0,
                }),
            },
        };

        let rendered = format_skill_body(&skill);
        assert!(rendered.contains("reviewed body"));
        assert!(rendered.contains("reviewed in-memory plugin snapshot"));
        assert!(!rendered.contains(&skill_path.display().to_string()));
        assert!(collect_companion_files(&skill).is_empty());
    }

    #[test]
    fn plugin_skill_load_fails_closed_when_reviewed_bundle_drifts() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
        let bundle = tmp.path().join(".codewhale/plugins/demo");
        let skill_dir = bundle.join("skills/hello");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            bundle.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: hello\ndescription: hello\n---\nreviewed body\n",
        )
        .unwrap();
        fs::write(skill_dir.join("companion.txt"), "reviewed companion").unwrap();

        let discovery = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv();
        let mut plugins = discovery.registry_for_workspace(tmp.path());
        std::sync::Arc::make_mut(&mut plugins)
            .trust("demo")
            .unwrap();
        std::sync::Arc::make_mut(&mut plugins)
            .enable("demo")
            .unwrap();
        let registry = codewhale_skills::discover_in_workspace_with_mode_and_plugins(
            tmp.path(),
            SkillDiscoveryMode::CodeWhaleOnly,
            Some(plugins.as_ref()),
        );
        let skill = registry.get("demo:hello").expect("active plugin skill");
        ensure_reviewed_plugin_skill_is_current(skill, tmp.path())
            .expect("stable reviewed snapshot");

        fs::write(skill_dir.join("companion.txt"), "changed after review").unwrap();
        let error = ensure_reviewed_plugin_skill_is_current(skill, tmp.path())
            .expect_err("bundle drift must deny the reviewed skill snapshot");
        assert!(error.to_string().contains("changed after review"));
    }

    #[test]
    fn collect_companion_files_returns_empty_for_solo_skill() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "solo", "Just a skill", "body");
        let registry = SkillRegistry::discover(tmp.path());
        let skill = registry.get("solo").unwrap();
        assert!(collect_companion_files(skill).is_empty());
    }

    #[test]
    fn format_skill_body_emits_companion_files_section_when_present() {
        let tmp = tempdir().unwrap();
        let skill_dir = tmp.path().join("skill-with-friends");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: skill-with-friends\ndescription: x\n---\nbody\n",
        )
        .unwrap();
        fs::write(skill_dir.join("helper.sh"), "#!/bin/sh\necho hi").unwrap();

        let registry = SkillRegistry::discover(tmp.path());
        let skill = registry.get("skill-with-friends").unwrap();
        let body = format_skill_body(skill);
        assert!(body.contains("## Companion files"));
        assert!(body.contains("helper.sh"));
        // Companion guidance names the model-visible tools only.
        assert!(body.contains("`read` (path=...)"), "{body}");
        assert!(body.contains("`bash`"), "{body}");
        assert!(
            !body.contains("File action=") && !body.contains("through Bash"),
            "{body}"
        );
    }

    #[test]
    fn format_skill_body_skips_companion_section_when_solo() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "solo", "x", "body");
        let registry = SkillRegistry::discover(tmp.path());
        let skill = registry.get("solo").unwrap();
        let body = format_skill_body(skill);
        assert!(
            !body.contains("## Companion files"),
            "solo skills shouldn't emit an empty Companion files section"
        );
    }

    #[tokio::test]
    async fn execute_lists_available_skills_for_empty_star_and_list_names() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().unwrap();
        // Pin home-based global skill roots to the tempdir so host skills
        // never leak into the listing count.
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path().join("home"));
        let _cw_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("cw-home"));
        let workspace = tmp.path().to_path_buf();
        crate::test_support::trust_workspace(&workspace);
        let skills_dir = workspace.join(".codewhale").join("skills");
        write_skill(&skills_dir, "alpha-skill", "First demo skill", "Body A.");
        write_skill(&skills_dir, "beta-skill", "", "Body B.");

        let context = ToolContext::new(workspace);
        let tool = LoadSkillTool;

        // #4651: listing is an action inside the single load_skill tool —
        // empty name, "*", and "list" all enumerate the reviewed registry.
        for listing_name in [json!({}), json!({"name": "*"}), json!({"name": "list"})] {
            let result = tool
                .execute(listing_name.clone(), &context)
                .await
                .expect("listing should succeed");
            assert!(result.success);
            assert!(
                result.content.contains("Available skills (2)"),
                "listing for {listing_name} should count skills: {}",
                result.content
            );
            assert!(
                result.content.contains("alpha-skill — First demo skill"),
                "listing should include name and description: {}",
                result.content
            );
            assert!(
                result.content.contains("- beta-skill"),
                "listing should include description-less skills: {}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn execute_listing_reports_empty_registry_plainly() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path().join("home"));
        let _cw_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("cw-home"));
        let context = ToolContext::new(tmp.path().to_path_buf());
        let result = LoadSkillTool
            .execute(json!({"name": "list"}), &context)
            .await
            .expect("empty listing should still succeed");
        assert!(result.success);
        assert!(
            result.content.contains("No skills installed."),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn execute_finds_skills_in_opencode_dir_via_workspace_discovery() {
        let tmp = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(&tmp.path().join("home"));
        let workspace = tmp.path().to_path_buf();
        crate::test_support::trust_workspace(&workspace);
        // Skill installed under workspace `.opencode/skills` (#432).
        let opencode_dir = workspace.join(".opencode").join("skills");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        write_skill(
            &opencode_dir,
            "from-opencode",
            "Skill installed under .opencode/skills",
            "Body content marker.",
        );

        let context = ToolContext::new(workspace);

        let tool = LoadSkillTool;
        let result = tool
            .execute(json!({"name": "from-opencode"}), &context)
            .await
            .expect("load_skill should succeed");
        assert!(result.success);
        assert!(
            result.content.contains("# Skill: from-opencode"),
            "body header missing: {}",
            result.content
        );
        assert!(result.content.contains("Body content marker."));

        let metadata = result.metadata.expect("metadata stamped");
        assert_eq!(
            metadata
                .get("skill_name")
                .and_then(serde_json::Value::as_str),
            Some("from-opencode")
        );
        let path_str = metadata
            .get("skill_path")
            .and_then(serde_json::Value::as_str)
            .expect("skill_path stamped");
        assert!(
            path_str.contains(".opencode"),
            "skill_path should point at the .opencode dir: {path_str}"
        );
    }

    #[tokio::test]
    async fn execute_respects_codewhale_only_skill_discovery() {
        let tmp = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(&tmp.path().join("home"));
        let workspace = tmp.path().to_path_buf();
        crate::test_support::trust_workspace(&workspace);
        write_skill(
            &workspace.join(".claude").join("skills"),
            "claude-only",
            "Claude skill",
            "Body content marker.",
        );
        let codewhale_dir = workspace.join(".codewhale").join("skills");
        write_skill(
            &codewhale_dir,
            "codewhale-only",
            "CodeWhale skill",
            "Body content marker.",
        );

        let context = ToolContext::new(workspace).with_skills_config(
            codewhale_dir,
            std::sync::Arc::new(codewhale_runtime::config::SkillsConfig {
                scan_codewhale_only: Some(true),
                ..Default::default()
            }),
        );
        let tool = LoadSkillTool;

        let result = tool
            .execute(json!({"name": "codewhale-only"}), &context)
            .await
            .expect("CodeWhale skill should load");
        assert!(result.success);

        let err = tool
            .execute(json!({"name": "claude-only"}), &context)
            .await
            .expect_err("Claude skill should be hidden in CodeWhale-only mode");
        let msg = err.to_string();
        assert!(
            msg.contains("claude-only") && msg.contains("codewhale-only"),
            "error should name the missing skill and available strict catalog: {msg}"
        );
    }

    #[tokio::test]
    async fn execute_loads_configured_external_skill_without_workspace_trust() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let home = tmp.path().join("home");
        let global_skills = home.join(".codewhale/skills");
        fs::create_dir_all(&workspace).unwrap();
        write_skill(
            &global_skills,
            "global-helper",
            "Global helper",
            "Global body marker.",
        );

        // Keep this test independent of the process-native home directory:
        // `codewhale_paths::user_home()` cannot be redirected reliably after process start
        // on Windows. The injected-home discovery test in `codewhale_skills::tests`
        // separately proves that ~/.codewhale/skills enters the default catalog.
        let context = ToolContext::new(&workspace)
            .with_skills_config(global_skills.clone(), Default::default());
        assert!(!context.trust_mode);
        assert!(
            context
                .resolve_path(
                    global_skills
                        .join("global-helper/SKILL.md")
                        .to_str()
                        .unwrap()
                )
                .is_err(),
            "ordinary file tools must retain the workspace boundary"
        );

        let result = LoadSkillTool
            .execute(json!({"name": "global-helper"}), &context)
            .await
            .expect("load_skill host lookup should open a configured external skill root");
        assert!(result.success);
        assert!(result.content.contains("Global body marker."));
    }

    #[tokio::test]
    async fn execute_returns_helpful_error_for_unknown_skill() {
        let tmp = tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(&tmp.path().join("home"));
        let workspace = tmp.path().to_path_buf();
        crate::test_support::trust_workspace(&workspace);
        // One real skill so the available list is non-empty.
        write_skill(
            &workspace.join(".agents").join("skills"),
            "real-one",
            "x",
            "body",
        );

        let context = ToolContext::new(workspace);
        let tool = LoadSkillTool;
        let err = tool
            .execute(json!({"name": "imaginary"}), &context)
            .await
            .expect_err("unknown skill should error");
        let msg = err.to_string();
        assert!(
            msg.contains("imaginary") && msg.contains("real-one"),
            "error must name the missing skill and list available ones: {msg}"
        );
    }
    #[tokio::test]
    async fn model_load_and_listing_obey_independent_invocation_gates() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::test_support::SealedHome::at(&tmp.path().join("home"));
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let root = workspace.join(".codewhale/skills");
        for (name, policy) in [
            ("explicit", "disable-model-invocation: true"),
            (
                "disabled",
                "disable-model-invocation: true\nuser-invocable: false",
            ),
            ("model", "user-invocable: false"),
        ] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: routing\n{policy}\n---\nsecret steps for {name}")).unwrap();
        }
        let context = ToolContext::new(&workspace).with_skills_config(
            &root,
            std::sync::Arc::new(codewhale_runtime::config::SkillsConfig {
                scan_codewhale_only: Some(true),
                ..Default::default()
            }),
        );
        let tool = LoadSkillTool;
        for name in ["explicit", "disabled"] {
            let error = tool
                .execute(json!({"name":name}), &context)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("does not allow model invocation")
            );
        }
        let listing = tool
            .execute(json!({"name":"list"}), &context)
            .await
            .unwrap();
        assert!(listing.content.contains("model"));
        assert!(!listing.content.contains("  - explicit"));
        assert!(!listing.content.contains("  - disabled"));
        let query = tool
            .execute(json!({"query":"explicit"}), &context)
            .await
            .unwrap();
        assert!(!query.content.contains("secret steps"));
        assert!(!query.content.contains("  - explicit"));
        assert!(
            tool.execute(json!({"name":"model"}), &context)
                .await
                .unwrap()
                .content
                .contains("secret steps for model")
        );
    }

    #[test]
    fn creator_skill_body_is_followed_by_base_directory_and_host_notes() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().unwrap();
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("home"));
        let owned = codewhale_runtime::config::owned_skills_dir().unwrap();
        codewhale_skills::install_system_skills(&owned, &Default::default()).unwrap();

        let registry = SkillRegistry::discover(&owned);
        let skill = registry.get("cordis-plugin-development").unwrap();
        let rendered = format_skill_body(skill);
        let body_end = rendered.find(skill.body.trim()).unwrap() + skill.body.trim().len();
        let tail = &rendered[body_end..];
        let base = format!(
            "\nBase directory for this skill: `{}`\n",
            owned.join("cordis-plugin-development").display()
        );
        let tail = tail.strip_prefix('\n').unwrap();
        assert!(tail.starts_with(&base), "{tail}");
        assert!(tail[base.len()..].starts_with("\n## Host notes: Codewhale\n\n"));

        let plain = registry.get("agent-experience").unwrap();
        let plain = format_skill_body(plain);
        assert!(plain.contains("Base directory for this skill"));
        assert!(!plain.contains("Host notes"));
    }

    #[tokio::test]
    async fn execute_list_omits_external_harness_skills_by_default_and_allows_direct_load() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let cw_home = tmp.path().join("cw-home");
        let workspace = tmp.path().join("workspace");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _cw_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &cw_home);
        fs::create_dir_all(&workspace).unwrap();
        crate::test_support::trust_workspace(&workspace);

        let long_description = format!(
            "Deploy project release artifacts safely. {}",
            "Extra verbose detail. ".repeat(10)
        );
        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "proj-deploy",
            &long_description,
            "Project body.",
        );
        write_skill(
            &cw_home.join("skills"),
            "cw-helper",
            "Codewhale native helper",
            "Native body.",
        );
        write_skill(
            &home.join(".claude").join("skills"),
            "claude-figma",
            "External Claude skill",
            "Claude external body.",
        );
        write_skill(
            &home.join(".agents").join("skills"),
            "agents-social",
            "External Agents skill",
            "Agents external body.",
        );

        let context = ToolContext::new(workspace);
        let tool = LoadSkillTool;
        assert_eq!(
            tool.input_schema()["properties"]["include_external"]["type"],
            "boolean"
        );

        let default_list = tool
            .execute(json!({"name": "list"}), &context)
            .await
            .expect("default list");
        assert!(default_list.success);
        assert!(default_list.content.contains("Available skills (2)"));
        assert!(default_list.content.contains("proj-deploy —"));
        assert!(
            default_list
                .content
                .contains("cw-helper — Codewhale native helper")
        );
        assert!(!default_list.content.contains("  - claude-figma"));
        assert!(!default_list.content.contains("  - agents-social"));
        assert!(
            default_list
                .content
                .contains("External harness skills (2 omitted")
        );
        assert!(default_list.content.contains("~/.claude/skills (1)"));
        assert!(default_list.content.contains("~/.agents/skills (1)"));
        assert!(default_list.content.contains("query="));
        assert!(default_list.content.contains("include_external=true"));
        let proj_line = default_list
            .content
            .lines()
            .find(|line| line.contains("proj-deploy"))
            .expect("proj-deploy line");
        assert!(proj_line.ends_with('…'), "{proj_line}");

        let with_external = tool
            .execute(json!({"name": "list", "include_external": true}), &context)
            .await
            .expect("list with include_external");
        assert!(with_external.content.contains("  - claude-figma"));
        assert!(with_external.content.contains("  - agents-social"));

        let list_all = tool
            .execute(json!({"name": "list-all"}), &context)
            .await
            .expect("list-all");
        assert!(list_all.content.contains("  - claude-figma"));
        assert!(list_all.content.contains("  - agents-social"));

        let direct = tool
            .execute(json!({"name": "claude-figma"}), &context)
            .await
            .expect("direct load of external skill must still work");
        assert!(direct.content.contains("Claude external body."));
    }

    #[tokio::test]
    async fn execute_query_ranks_multi_word_tokenized_matches() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let cw_home = tmp.path().join("cw-home");
        let workspace = tmp.path().join("workspace");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _cw_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &cw_home);
        fs::create_dir_all(&workspace).unwrap();
        crate::test_support::trust_workspace(&workspace);

        let project_skills = workspace.join(".codewhale").join("skills");
        let gates_dir = project_skills.join("cw-gates");
        fs::create_dir_all(&gates_dir).unwrap();
        fs::write(
            gates_dir.join("SKILL.md"),
            "---\nname: cw-gates\ndescription: Verify Rust compilation, clippy, and test gates.\naliases-for: [build-checks]\n---\nGate steps.\n",
        )
        .unwrap();
        write_skill(
            &project_skills,
            "release-gates",
            "Final release build checklists and gates",
            "Release body.",
        );
        write_skill(
            &project_skills,
            "rust-tips",
            "General Rust ownership notes",
            "Tips body.",
        );
        write_skill(
            &project_skills,
            "moda-video",
            "Generate marketing videos",
            "Video body.",
        );
        write_skill(
            &home.join(".claude").join("skills"),
            "rust-build-runner",
            "Run rust build and gates in external harness",
            "External runner.",
        );

        let context = ToolContext::new(workspace);
        let result = LoadSkillTool
            .execute(json!({"query": "rust build gates"}), &context)
            .await
            .expect("multi-word query");
        assert!(result.success);
        let content = &result.content;
        let gates_pos = content.find("  - cw-gates").expect("cw-gates matched");
        let ext_pos = content
            .find("  - rust-build-runner")
            .expect("external skill searchable via query");
        let partial_two_pos = content
            .find("  - release-gates")
            .expect("2-of-3 partial match included");
        let partial_one_pos = content
            .find("  - rust-tips")
            .expect("1-of-3 partial match included");
        assert!(gates_pos < partial_two_pos, "{content}");
        assert!(ext_pos < partial_two_pos, "{content}");
        assert!(partial_two_pos < partial_one_pos, "{content}");
        assert!(content.contains("(~/.claude/skills)"), "{content}");
        assert!(!content.contains("moda-video"), "{content}");

        let punctuated = LoadSkillTool
            .execute(json!({"query": "rust/build, gates!"}), &context)
            .await
            .expect("punctuated multi-word query");
        assert!(punctuated.content.contains("  - cw-gates"));

        let alias = LoadSkillTool
            .execute(json!({"query": "build-checks"}), &context)
            .await
            .expect("alias query");
        assert!(
            alias
                .content
                .lines()
                .nth(1)
                .unwrap()
                .contains("  - cw-gates")
        );
    }
}
