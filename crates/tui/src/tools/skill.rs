//! `load_skill` tool — fetch a `SKILL.md` body and its companion-file
//! list into the model's context (#434).
//!
//! ## Why a tool when skills already surface in the system prompt?
//!
//! `prompts.rs::system_prompt_for_mode_with_context_and_skills` injects a
//! budgeted first page of routing metadata. The full catalogue is available
//! through `name="list"`, and each full body is loaded only by exact name.
//!
//! `load_skill name=<id>` is the canonical progressive-disclosure path. It
//! performs a name-based host lookup, so native global skills work without
//! widening the model's workspace file authority, and it enumerates companion
//! files without a separate `list_dir`. Reviewed plugin skills are exposed
//! only through this tool's content-bound in-memory snapshot; their mutable
//! source paths and companion files are deliberately not returned.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::skills::{
    MAX_SKILL_DESCRIPTION_CHARS, Skill, SkillDiscoveryMode, SkillSource,
    discover_for_workspace_and_dir_with_mode_and_plugins,
    discover_in_workspace_with_mode_and_plugins, skill_directories_for_workspace_and_dir,
    skills_directories_for_mode,
};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

pub struct LoadSkillTool;

#[async_trait]
impl ToolSpec for LoadSkillTool {
    fn name(&self) -> &'static str {
        "load_skill"
    }

    fn description(&self) -> &'static str {
        "Load a named skill's SKILL.md body and companion file list into this turn. Use when \
         the user names a skill, or when an entry in the system prompt's `## Skills` index \
         matches the task -- load it before starting the work, not after. Pass query=\"...\" to \
         search names and descriptions, or name=\"list\" for the whole catalogue. Resolves \
         global and plugin skills that `read` cannot reach."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill id to load. Omit or pass \"list\" to see all available skills."
                },
                "query": {
                    "type": "string",
                    "description": "Search term matched against skill names and descriptions. Use when the index was truncated or no name is known."
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
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();

        // #432: walk every candidate skill directory (workspace
        // .agents/skills, skills, .opencode/skills, .claude/skills,
        // .cursor/skills, ~/.agents/skills, global default), merging with
        // first-wins precedence. The
        // tool's lookup mirrors what the system-prompt skills block
        // already lists, so the model never asks for a name it
        // can't find.
        let discovery_mode = context.skills_discovery_mode;
        let registry = if let Some(skills_dir) = context.skills_dir.as_deref() {
            discover_for_workspace_and_dir_with_mode_and_plugins(
                &context.workspace,
                skills_dir,
                discovery_mode,
                context.plugin_registry.as_deref(),
            )
        } else {
            discover_in_workspace_with_mode_and_plugins(
                &context.workspace,
                discovery_mode,
                context.plugin_registry.as_deref(),
            )
        }
        .into_enabled();

        // Listing mode: empty name, "*", or "list" returns the full registry (#4651).
        // Cap descriptions on the first 100 rows; retain all names so the model
        // can discover every installed skill without guessing. A `query` filters
        // over the same routing metadata the ambient index carries.
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !query.is_empty() || name.is_empty() || name == "*" || name == "list" {
            let all = registry.list();
            let skills: Vec<&_> = all
                .iter()
                .filter(|skill| skill.invocation.model_invocable())
                .filter(|skill| {
                    query.is_empty()
                        || skill.name.to_lowercase().contains(&query)
                        || skill.description.to_lowercase().contains(&query)
                })
                .collect();
            if skills.is_empty() {
                return Ok(ToolResult::success(if query.is_empty() {
                    "No skills installed.".to_string()
                } else {
                    format!("No skill matches {query:?}. Pass name=\"list\" for the catalogue.")
                }));
            }
            let mut listing = if query.is_empty() {
                format!("Available skills ({}):\n", skills.len())
            } else {
                format!("Skills matching {:?} ({}):\n", query, skills.len())
            };
            for (index, skill) in skills.iter().enumerate() {
                let description = if index < 100 {
                    skill.description.trim()
                } else {
                    ""
                };
                if description.is_empty() {
                    listing.push_str(&format!("  - {}\n", skill.name));
                } else {
                    let capped: String = description
                        .chars()
                        .take(MAX_SKILL_DESCRIPTION_CHARS)
                        .collect();
                    let marker = if capped.len() < description.len() {
                        "…"
                    } else {
                        ""
                    };
                    listing.push_str(&format!("  - {} — {capped}{marker}\n", skill.name));
                }
            }
            return Ok(ToolResult::success(listing));
        }

        let Some(skill) = registry.get(name) else {
            let available: Vec<&str> = registry
                .list()
                .iter()
                .filter(|s| s.invocation.model_invocable())
                .map(|s| s.name.as_str())
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
                    if context.skills_discovery_mode == SkillDiscoveryMode::CodeWhaleOnly {
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
                format!(
                    "skill `{name}` not found. Available: {}",
                    available.join(", ")
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
    crate::logging::warn(&message);
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
    plugins: Option<&crate::plugins::PluginRegistry>,
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
fn format_skill_body(skill: &Skill) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Skill: {}\n\n", skill.name));
    if !skill.description.trim().is_empty() {
        out.push_str(&format!("> {}\n\n", skill.description.trim()));
    }
    let invocation = match skill.invocation {
        crate::skills::SkillInvocation::ModelAndUser => "model+user",
        crate::skills::SkillInvocation::ExplicitOnly => "explicit-only",
        crate::skills::SkillInvocation::ModelOnly => "model-only",
        crate::skills::SkillInvocation::Disabled => "disabled",
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
    use crate::skills::SkillRegistry;
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
            invocation: crate::skills::SkillInvocation::ModelAndUser,
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
            authority: Box::new(crate::plugins::types::PluginAuthority {
                plugin_id: crate::plugins::types::PluginId("workspace/1/demo".to_string()),
                plugin_name: "demo".to_string(),
                workspace: tmp.path().to_path_buf(),
                state_path: tmp.path().join("state.json"),
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
            invocation: crate::skills::SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: "reviewed body".to_string(),
            path: skill_path.clone(),
            source: SkillSource::Plugin {
                plugin_id: "workspace/123/demo".to_string(),
                plugin_name: "demo".to_string(),
                native_registration: None,
                authority: Box::new(crate::plugins::types::PluginAuthority {
                    plugin_id: crate::plugins::types::PluginId("workspace/123/demo".to_string()),
                    plugin_name: "demo".to_string(),
                    workspace: tmp.path().to_path_buf(),
                    state_path: tmp.path().join("state.json"),
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

        let discovery = crate::plugins::PluginDiscoveryContext::capture_pre_dotenv();
        let mut plugins = discovery.registry_for_workspace(tmp.path());
        std::sync::Arc::make_mut(&mut plugins)
            .trust("demo")
            .unwrap();
        std::sync::Arc::make_mut(&mut plugins)
            .enable("demo")
            .unwrap();
        let registry = crate::skills::discover_in_workspace_with_mode_and_plugins(
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

        let mut context = ToolContext::new(workspace);
        // The skill tool reads $HOME for the global default; pin it to a
        // tempdir so the test is hermetic regardless of the host's
        // ~/.deepseek/skills.
        context.workspace = tmp.path().to_path_buf();

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
            crate::skills::SkillDiscoveryMode::CodeWhaleOnly,
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
        // `crate::config::effective_home_dir()` cannot be redirected reliably after process start
        // on Windows. The injected-home discovery test in `skills::tests`
        // separately proves that ~/.codewhale/skills enters the default catalog.
        let context = ToolContext::new(&workspace).with_skills_config(
            global_skills.clone(),
            crate::skills::SkillDiscoveryMode::Compatible,
        );
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
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path().join("home"));
        let _cw_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("cw-home"));
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
        let context = ToolContext::new(&workspace)
            .with_skills_config(&root, SkillDiscoveryMode::CodeWhaleOnly);
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
        assert!(!listing.content.contains("explicit"));
        assert!(!listing.content.contains("disabled"));
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
    #[tokio::test]
    async fn execute_listing_caps_rows_and_descriptions_honestly() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path().join("home"));
        let _cw_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("cw-home"));
        let workspace = tmp.path().to_path_buf();
        crate::test_support::trust_workspace(&workspace);
        let skills_dir = workspace.join(".codewhale").join("skills");
        let long = "d".repeat(500);
        for index in 0..105 {
            let description = if index == 0 { long.as_str() } else { "Short." };
            write_skill(
                &skills_dir,
                &format!("skill-{index:03}"),
                description,
                "Body.",
            );
        }

        let context = ToolContext::new(workspace);
        let result = LoadSkillTool
            .execute(json!({"name": "list"}), &context)
            .await
            .expect("listing should succeed");
        assert!(
            result.content.contains("Available skills (105)"),
            "{}",
            result.content
        );
        for index in 0..105 {
            assert!(
                result
                    .content
                    .lines()
                    .any(|line| { line.starts_with(&format!("  - skill-{index:03}")) }),
                "missing skill-{index:03}: {}",
                result.content
            );
        }
        let capped: String = "d".repeat(400);
        assert!(
            result.content.contains(&format!("skill-000 — {capped}…")),
            "{}",
            result.content
        );
        assert!(!result.content.contains(&long), "description must shorten");
    }
}
