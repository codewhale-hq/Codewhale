use tempfile::TempDir;

#[test]
fn frontmatter_handles_bom_exact_fences_and_plain_continuations() {
    let content = "\u{feff}---\r\nname: demo\r\ndescription: Deploy apps --- safely\r\n  for X or Y: really\r\ninvocation: user\r\n---\r\n# Body\r\n";
    let (metadata, body) = super::parse_frontmatter(content).unwrap().unwrap();
    assert_eq!(metadata["name"], "demo");
    assert_eq!(
        metadata["description"],
        "Deploy apps --- safely for X or Y: really"
    );
    assert_eq!(metadata.len(), 3, "continuation text is not a new key");
    assert_eq!(body.trim(), "# Body");
    assert!(
        super::parse_frontmatter("---not a fence\n# Body")
            .unwrap()
            .is_none()
    );
    assert!(super::parse_frontmatter("---\nname: demo\ndescription: inline --- only").is_err());
}

#[test]
fn frontmatter_block_indentation_is_character_safe_and_keeps_nested_fences() {
    for indicator in ["|", ">"] {
        let content = format!(
            "---\nname: demo\ndescription: {indicator}\n  first\n\u{3000}wide\n \u{a0}mixed\n  ---\n    nested\n---\nbody"
        );
        let (metadata, body) = super::parse_frontmatter(&content).unwrap().unwrap();
        let expected = if indicator == "|" {
            "first\nwide\nmixed\n---\n  nested"
        } else {
            "first wide mixed ---   nested"
        };
        assert_eq!(metadata["description"], expected);
        assert_eq!(body.trim(), "body");
    }
}

#[test]
fn unavailable_home_fallbacks_are_fresh_nonexistent_paths() {
    let first = super::unavailable_home_root();
    let second = super::unavailable_home_root();
    assert_ne!(first, second);
    assert!(!first.exists());
    assert!(!second.exists());
    assert!(first.starts_with(std::env::temp_dir()));
    assert_ne!(first, std::path::Path::new("/tmp/codewhale"));
}

fn create_skill_dir(tmpdir: &TempDir, skill_name: &str, skill_content: &str) {
    let skill_dir = tmpdir.path().join("skills").join(skill_name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), skill_content).unwrap();
}

#[test]
fn unicode_skill_identity_preserves_bodies_and_legacy_activation() {
    let tmp = TempDir::new().unwrap();
    let names = [
        "技能",
        "分析",
        "PDF阅读",
        "PDF签名",
        "skill",
        "pdf",
        "Café",
        "Cafe\u{301}",
    ];
    for (index, raw) in names.iter().enumerate() {
        create_skill_dir(
            &tmp,
            &format!("source-{index}"),
            &format!("---\nname: {raw}\ndescription: identity fixture\n---\nbody {index}"),
        );
    }
    let registry = super::SkillRegistry::discover(&tmp.path().join("skills"));
    assert_eq!(
        registry.len(),
        names.len(),
        "colliding old slugs must not hide bodies"
    );
    for (index, raw) in names.iter().enumerate() {
        let skill = registry.get(raw).unwrap();
        assert_eq!(skill.body, format!("body {index}"));
        assert!(super::is_valid_skill_name(&skill.name));
        assert_eq!(registry.get(&skill.name).unwrap().body, skill.body);
        assert_eq!(
            super::normalize_skill_name_for_lookup(&skill.name),
            skill.name
        );
        assert!(skill.path.ends_with(format!("source-{index}/SKILL.md")));
    }
    assert_eq!(registry.get("skill").unwrap().name, "skill");
    assert_eq!(registry.get("pdf阅读").unwrap().body, "body 2");
    assert_ne!(
        registry.get("Café").unwrap().name,
        registry.get("Cafe\u{301}").unwrap().name
    );
    let long = format!("{}技能", "a".repeat(100));
    assert_eq!(super::normalize_skill_name_for_lookup(&long).len(), 64);
    assert!(
        registry
            .warnings()
            .iter()
            .all(|warning| warning.contains("differs from directory")),
        "{:?}",
        registry.warnings()
    );
    let mut malformed = registry.get("技能").unwrap().clone();
    malformed.name = "bad\u{1}技能".to_string();
    let mut validation = super::SkillRegistry::default();
    validation.normalize_skill_name(&mut malformed, std::path::Path::new("SKILL.md"));
    assert_eq!(
        validation.warnings().len(),
        1,
        "control bytes remain invalid"
    );

    let path = tmp.path().join("skills_state.toml");
    let original = b"disabled = [\"skill\", \"pdf\"]\n";
    std::fs::write(&path, original).unwrap();
    let filtered = registry
        .clone()
        .into_enabled_with_state(crate::skill_state::SkillStateStore::load_from(path.clone()));
    assert_eq!(filtered.len(), 2);
    assert!(filtered.get("技能").is_none());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mut state = crate::skill_state::SkillStateStore::load_from(path.clone()).unwrap();
    state
        .set_enabled(&registry.get("技能").unwrap().name, true)
        .unwrap();
    state.set_enabled("skill", true).unwrap();
    let filtered = registry.into_enabled_with_state(Ok(state));
    assert_eq!(filtered.get("技能").unwrap().body, "body 0");
    assert_eq!(filtered.get("skill").unwrap().body, "body 4");
    for hidden in ["分析", "PDF阅读", "PDF签名", "pdf"] {
        assert!(filtered.get(hidden).is_none(), "must not revive {hidden}");
    }
    let rendered = super::render_skills_block(&filtered, "en", tmp.path()).unwrap();
    assert!(rendered.contains(&filtered.get("技能").unwrap().name));
    assert!(!rendered.contains("body 1"));
}

#[test]
fn unicode_plugin_identity_keeps_exact_namespaces_and_reviewed_metadata() {
    let tmp = TempDir::new().unwrap();
    let config = crate::plugins::discovery::DiscoveryConfig {
        workspace: tmp.path().join("workspace"),
        user_plugins_dir: tmp.path().join("plugins"),
        workspace_plugins_dir: tmp.path().join("workspace-plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: tmp.path().join("plugin-state/state.json"),
    };
    for namespace in ["team.plugin", "team-plugin"] {
        let root = config.user_plugins_dir.join(namespace);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "$schema": "https://agent-plugins.org/schemas/plugin.json",
                "name": namespace,
                "version": "1.0.0"
            })
            .to_string(),
        )
        .unwrap();
        for name in ["技能", "分析", "skill"] {
            write_skill(
                &root.join("skills"),
                name,
                "reviewed identity",
                &format!("{namespace} {name}"),
            );
        }
    }
    let mut plugins = crate::plugins::discovery::discover_with_config(&config);
    assert!(plugins.validation_is_clean(), "{:?}", plugins.diagnostics());
    let mut registry = super::SkillRegistry::default();
    super::merge_active_plugin_skills(&mut registry, &plugins);
    assert!(registry.is_empty());
    for namespace in ["team.plugin", "team-plugin"] {
        plugins.trust(namespace).unwrap();
        plugins.enable(namespace).unwrap_or_else(|error| {
            panic!("{error}: {:?}", plugins.get(namespace).unwrap().diagnostics)
        });
        let plugin = plugins.get(namespace).unwrap();
        let unicode = plugin
            .skill_snapshots
            .iter()
            .find(|s| s.body == format!("{namespace} 技能"))
            .unwrap();
        assert_eq!(unicode.legacy_activation_name.as_deref(), Some("skill"));
        assert!(!unicode.source_hash.is_empty());
    }
    super::merge_active_plugin_skills(&mut registry, &plugins);
    assert_eq!(registry.len(), 6);
    let dotted = registry.get("TEAM.PLUGIN:技能").unwrap();
    assert_eq!(dotted.body, "team.plugin 技能");
    assert_eq!(
        dotted.legacy_activation_name.as_deref(),
        Some("team.plugin:skill")
    );
    assert_eq!(
        registry.get("TEAM-PLUGIN:技能").unwrap().body,
        "team-plugin 技能"
    );
    assert!(registry.get("team_plugin:技能").is_none());
    let path = tmp.path().join("skills_state.toml");
    std::fs::write(&path, "disabled = [\"team.plugin:skill\"]\n").unwrap();
    let filtered = registry
        .clone()
        .into_enabled_with_state(crate::skill_state::SkillStateStore::load_from(path));
    assert!(filtered.get("team.plugin:技能").is_none());
    assert!(filtered.get("team-plugin:技能").is_some());
    registry
        .skills
        .retain(|skill| !skill.name.starts_with("team.plugin:"));
    assert!(
        registry.get("team.plugin:技能").is_none(),
        "absent dotted namespace must not select dashed namespace"
    );
}

#[test]
fn discovery_metrics_reset_and_snapshot_are_exact() {
    super::reset_discovery_metrics();
    assert_eq!(
        super::discovery_metrics_snapshot(),
        super::SkillDiscoveryMetrics::default()
    );

    let tmpdir = TempDir::new().unwrap();
    let skills_root = tmpdir.path().join("skills");
    let vendor_root = skills_root.join("vendor");
    write_skill(&vendor_root, "demo", "A demo skill", "Instructions");

    let registry = super::SkillRegistry::discover(&skills_root);
    assert_eq!(registry.len(), 1);
    assert_eq!(
        super::discovery_metrics_snapshot(),
        super::SkillDiscoveryMetrics {
            root_discovery_calls: 1,
            directories_visited: 2,
            skill_md_read_attempts: 2,
        }
    );

    super::reset_discovery_metrics();
    let missing_root = tmpdir.path().join("missing");
    let _registry = super::SkillRegistry::discover(&missing_root);
    assert_eq!(
        super::discovery_metrics_snapshot(),
        super::SkillDiscoveryMetrics {
            root_discovery_calls: 1,
            directories_visited: 0,
            skill_md_read_attempts: 0,
        }
    );

    super::reset_discovery_metrics();
    assert_eq!(
        super::discovery_metrics_snapshot(),
        super::SkillDiscoveryMetrics::default()
    );
}

#[test]
fn prompt_warning_sanitizer_scrubs_stale_conventional_home_roots() {
    let workspace = std::path::Path::new("/tmp/workspace");
    let warning = "Skill at /Users/private-name/.agents/skills/a/SKILL.md is shadowed by /home/other/.skills/a/SKILL.md";
    let sanitized = super::sanitize_prompt_path_text(warning, workspace, None);
    assert_eq!(
        sanitized,
        "Skill at ~/.agents/skills/a/SKILL.md is shadowed by ~/.skills/a/SKILL.md"
    );
}

#[test]
fn prompt_warning_sanitizer_normalizes_windows_separators() {
    let workspace = std::path::Path::new(r"C:\workspace");
    let configured_root = std::path::Path::new(r"C:\runtime\sessions\session-123\skills");
    let warning = r"Skill in C:\runtime\sessions\session-123\skills\visual-design\SKILL.md is not a safe command name";

    let sanitized = super::sanitize_prompt_path_text(warning, workspace, Some(configured_root));

    assert_eq!(
        sanitized,
        "Skill in <configured-skills>/visual-design/SKILL.md is not a safe command name"
    );
}

#[test]
fn prompt_warning_sanitizer_replaces_configured_roots_only_at_path_boundaries() {
    let workspace = std::path::Path::new("/tmp/workspace");
    let configured_root = std::path::Path::new("/tmp/work");
    let warning = "Skill in /tmp/workspace/.agents/skills/a/SKILL.md shadows /tmp/work/a/SKILL.md";

    let sanitized = super::sanitize_prompt_path_text(warning, workspace, Some(configured_root));

    assert_eq!(
        sanitized,
        "Skill in ./.agents/skills/a/SKILL.md shadows <configured-skills>/a/SKILL.md"
    );
}

#[cfg(unix)]
#[test]
fn prompt_warning_sanitizer_handles_non_utf8_configured_roots() {
    use std::os::unix::ffi::OsStringExt;

    let workspace = std::path::Path::new("/tmp/workspace");
    let configured_root = std::path::PathBuf::from(std::ffi::OsString::from_vec(
        b"/tmp/session-\xff/skills".to_vec(),
    ));
    let warning = format!(
        "Skill in {}/visual-design/SKILL.md is not a safe command name",
        configured_root.display()
    );

    let sanitized = super::sanitize_prompt_path_text(&warning, workspace, Some(&configured_root));

    assert_eq!(
        sanitized,
        "Skill in <configured-skills>/visual-design/SKILL.md is not a safe command name"
    );
}

#[test]
fn render_available_skills_context_lists_names_and_usage() {
    let tmpdir = TempDir::new().unwrap();
    create_skill_dir(
        &tmpdir,
        "test-skill",
        "---\nname: test-skill\ndescription: A test skill\n---\nDo something special",
    );

    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");

    // Token diet: names + descriptions only. `load_skill` resolves the
    // name through the registry, so no on-disk path is rendered at all
    // (#4632's surface is gone with the paths).
    assert!(rendered.contains("## Skills"));
    assert!(rendered.contains("- test-skill: A test skill"));
    assert!(rendered.contains("call `load_skill` with that exact name before starting the work"));
    assert!(rendered.contains("do not expand tool, approval, or trust authority"));
    assert!(
        !rendered.contains("test-skill/SKILL.md"),
        "no on-disk paths in the index"
    );
    assert!(!rendered.contains(tmpdir.path().to_str().unwrap_or("/nonexistent")));
    assert!(rendered.contains("### Usage"));
}

#[test]
fn workspace_prompt_omits_disabled_skills_without_configured_directory() {
    let _env_lock = crate::test_support::lock_test_env();
    let tmpdir = TempDir::new().unwrap();
    let home = tmpdir.path().join("home");
    let workspace = tmpdir.path().join("workspace");
    let skills_root = workspace.join(".agents").join("skills");
    std::fs::create_dir_all(&home).unwrap();
    write_skill(
        &skills_root,
        "enabled-skill",
        "Enabled skill",
        "Instructions",
    );
    write_skill(
        &skills_root,
        "disabled-skill",
        "Disabled skill",
        "Instructions",
    );
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let _codewhale_home =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));
    crate::test_support::trust_workspace(&workspace);

    let mut state = crate::skill_state::SkillStateStore::load_default().unwrap();
    state.set_enabled("disabled-skill", false).unwrap();
    super::clear_skill_discovery_cache();

    let rendered = super::render_available_skills_context_for_workspace_with_mode_and_plugins(
        &workspace,
        super::SkillDiscoveryMode::Compatible,
        "en",
        None,
        super::MAX_AVAILABLE_SKILLS_CHARS,
    )
    .expect("enabled skill context");

    assert!(rendered.contains("enabled-skill"));
    assert!(!rendered.contains("disabled-skill"));
}

#[test]
fn render_available_skills_context_resolves_by_name_without_invented_paths() {
    // Regression (pathless form): when a community-installed skill
    // lives in a directory whose name differs from its frontmatter
    // `name`, the row carries the registry name and no path at all —
    // `load_skill` resolves the name to the real on-disk file, so no
    // caller can reconstruct a stale <frontmatter>/SKILL.md path.
    let tmpdir = TempDir::new().unwrap();
    create_skill_dir(
        &tmpdir,
        "weird-dir-name",
        "---\nname: friendly-name\ndescription: drift case\n---\nbody",
    );

    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");

    assert!(
        rendered.contains("- friendly-name: drift case"),
        "registry-name row present, got:\n{rendered}"
    );
    let (index, warnings) = rendered
        .split_once("\n### Skill load warnings\n")
        .expect("the real name/directory mismatch must remain visible");
    assert!(
        !index.contains("weird-dir-name/SKILL.md"),
        "no dir-derived path in the index:\n{index}"
    );
    assert!(
        warnings.contains("weird-dir-name/SKILL.md: frontmatter name `friendly-name` differs from directory `weird-dir-name`"),
        "the warning must identify the real source:\n{warnings}"
    );
    let registry = super::SkillRegistry::discover(&tmpdir.path().join("skills"));
    assert_eq!(
        registry.get("friendly-name").expect("name lookup").path,
        tmpdir.path().join("skills/weird-dir-name/SKILL.md")
    );
    assert!(
        !rendered.contains("friendly-name/SKILL.md"),
        "no frontmatter-derived path:\n{rendered}"
    );
}

#[test]
fn render_available_skills_context_returns_none_when_empty() {
    let tmpdir = TempDir::new().unwrap();
    let empty = tmpdir.path().join("skills");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(crate::skills::render_available_skills_context(&empty).is_none());

    let missing = tmpdir.path().join("does-not-exist");
    assert!(crate::skills::render_available_skills_context(&missing).is_none());
}

#[test]
fn render_skills_block_surfaces_warnings_when_no_skill_loaded() {
    let tmpdir = TempDir::new().unwrap();
    let mut registry = super::SkillRegistry::default();
    registry
        .warnings
        .push("broken skill could not be parsed".to_string());

    let rendered =
        super::render_skills_block(&registry, "en", tmpdir.path()).expect("warning-only block");

    assert!(rendered.contains("### Skill load warnings"));
    assert!(rendered.contains("broken skill could not be parsed"));
    assert!(rendered.chars().count() <= super::MAX_AVAILABLE_SKILLS_CHARS);
}

#[test]
fn render_available_skills_context_truncates_long_descriptions() {
    let tmpdir = TempDir::new().unwrap();
    let long_desc = "x".repeat(2_000);
    let body = format!("---\nname: bigdesc\ndescription: {long_desc}\n---\nbody");
    create_skill_dir(&tmpdir, "bigdesc", &body);

    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");

    let max = super::MAX_SKILL_DESCRIPTION_CHARS;
    assert!(rendered.contains('…'), "expected truncation marker");
    assert!(
        !rendered.contains(&"x".repeat(max + 1)),
        "untruncated long run should not appear"
    );
}

#[test]
fn render_available_skills_context_collapses_internal_whitespace() {
    let tmpdir = TempDir::new().unwrap();
    create_skill_dir(
        &tmpdir,
        "spaced-skill",
        "---\nname: spaced-skill\ndescription: alpha  \t  beta   gamma\n---\nbody",
    );

    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");

    let line = rendered
        .lines()
        .find(|l| l.starts_with("- spaced-skill:"))
        .expect("skill line");
    assert!(line.contains("alpha beta gamma"), "got: {line:?}");
}

/// Three-tier fitting: when full descriptions overflow the budget the index
/// shortens them, then drops to names-only — a skill's name never vanishes
/// while the names themselves fit.
#[test]
fn render_available_skills_context_keeps_every_name_when_descriptions_overflow() {
    let tmpdir = TempDir::new().unwrap();
    let big_desc = "y".repeat(super::MAX_SKILL_DESCRIPTION_CHARS - 20);
    for i in 0..200 {
        let body = format!("---\nname: skill-{i:03}\ndescription: {big_desc}\n---\nbody");
        create_skill_dir(&tmpdir, &format!("skill-{i:03}"), &body);
    }

    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");

    // 200 × ~380 chars of description is ~76k, far over the default budget:
    // tier 1 cannot fit, so descriptions shrink or names stand alone.
    for i in 0..200 {
        let name = format!("- skill-{i:03}");
        assert!(rendered.contains(&name), "missing {name}:\n{rendered}");
    }
    assert!(
        !rendered.contains("additional skills omitted"),
        "names fit the budget; omission is the last resort, not the first"
    );
    assert!(
        !rendered.contains(&big_desc),
        "a full-length description must not survive an overflowing index"
    );
    assert!(
        rendered.chars().count() <= super::MAX_AVAILABLE_SKILLS_CHARS,
        "rendered length must stay within the complete block budget"
    );
}

/// `Use when:` triggers survive shortening ahead of the summary — they are
/// what the model routes on.
#[test]
fn render_skills_block_shortens_summary_before_trigger() {
    let tmpdir = TempDir::new().unwrap();
    let mut registry = super::SkillRegistry::default();
    let summary = "s".repeat(300);
    for i in 0..120 {
        registry.skills.push(super::Skill {
            legacy_activation_name: None,
            name: format!("skill-{i:03}"),
            description: format!("{summary} Use when: the user asks for widget {i}."),
            localized_descriptions: std::collections::HashMap::new(),
            invocation: super::SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: "body".to_string(),
            path: tmpdir.path().join(format!("skill-{i:03}/SKILL.md")),
            source: super::SkillSource::Native,
        });
    }
    let rendered =
        super::render_skills_block(&registry, "en", tmpdir.path()).expect("skill context");
    let line = rendered
        .lines()
        .find(|l| l.starts_with("- skill-007:"))
        .expect("row for skill-007");
    assert!(
        line.contains("Use when: the user asks for widget 7"),
        "trigger must survive shortening intact:\n{line}"
    );
    assert!(
        !line.contains(&summary),
        "summary must be the half that shrinks:\n{line}"
    );
    assert!(rendered.chars().count() <= super::MAX_AVAILABLE_SKILLS_CHARS);
}

/// The budget follows the route window: a 1M route sees a much larger index
/// than a small local window, both clamped to sane bounds.
#[test]
fn skills_prompt_budget_scales_with_context_window() {
    let small = super::skills_prompt_budget_chars(Some(8_000));
    let default = super::skills_prompt_budget_chars(None);
    let large = super::skills_prompt_budget_chars(Some(1_000_000));
    assert_eq!(small, 2_400, "floor holds for tiny windows");
    assert_eq!(default, 25_600, "128k window × 4 chars × 5%");
    assert_eq!(large, 40_000, "ceiling holds for 1M windows");
    assert_eq!(
        super::skills_prompt_budget_chars(Some(0)),
        default,
        "a zero window is treated as unknown"
    );
}

#[test]
fn render_skills_block_holds_budget_with_five_digit_omission_counts() {
    let tmpdir = TempDir::new().unwrap();
    let mut registry = super::SkillRegistry::default();
    for i in 0..15_000 {
        registry.skills.push(super::Skill {
            legacy_activation_name: None,
            name: format!("skill-{i:05}"),
            description: "x".to_string(),
            localized_descriptions: std::collections::HashMap::new(),
            invocation: super::SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: "body".to_string(),
            path: tmpdir.path().join(format!("skill-{i:05}/SKILL.md")),
            source: super::SkillSource::Native,
        });
        registry.warnings.push(format!("warning {i:05}"));
    }

    let rendered =
        super::render_skills_block(&registry, "en", tmpdir.path()).expect("skill context");
    let omitted_skills = rendered
        .lines()
        .find(|line| line.contains("additional skills omitted"))
        .and_then(|line| line.split_whitespace().nth(2))
        .and_then(|count| count.parse::<usize>().ok())
        .expect("skill omission count");
    let omitted_warnings = rendered
        .lines()
        .find(|line| line.contains("additional warnings omitted"))
        .and_then(|line| line.split_whitespace().nth(2))
        .and_then(|count| count.parse::<usize>().ok())
        .expect("warning omission count");

    assert!(omitted_skills > 9_999, "fixture must exercise five digits");
    assert!(
        omitted_warnings > 9_999,
        "fixture must exercise five digits"
    );
    assert!(rendered.chars().count() <= super::MAX_AVAILABLE_SKILLS_CHARS);
}

#[test]
fn explicit_only_skills_do_not_reduce_ambient_index_capacity() {
    let tmpdir = TempDir::new().unwrap();
    let mut registry = super::SkillRegistry::default();
    for i in 0..6 {
        registry.skills.push(super::Skill {
            legacy_activation_name: None,
            name: format!("visible-{i:03}"),
            description: "x".repeat(246),
            localized_descriptions: std::collections::HashMap::new(),
            invocation: super::SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: "body".to_string(),
            path: tmpdir.path().join(format!("visible-{i:03}/SKILL.md")),
            source: super::SkillSource::Native,
        });
    }

    let baseline =
        super::render_skills_block(&registry, "en", tmpdir.path()).expect("skill context");
    assert!(!baseline.contains("additional skills omitted"));

    let mut with_explicit_only = registry.clone();
    for i in 0..10_000 {
        with_explicit_only.skills.push(super::Skill {
            legacy_activation_name: None,
            name: format!("explicit-{i:05}"),
            description: String::new(),
            localized_descriptions: std::collections::HashMap::new(),
            invocation: super::SkillInvocation::ExplicitOnly,
            aliases: Vec::new(),
            argument_hint: None,
            body: "body".to_string(),
            path: tmpdir.path().join(format!("explicit-{i:05}/SKILL.md")),
            source: super::SkillSource::Native,
        });
    }

    let rendered = super::render_skills_block(&with_explicit_only, "en", tmpdir.path())
        .expect("skill context");
    assert_eq!(rendered, baseline);
}

#[test]
fn render_skills_block_preserves_registry_precedence_under_prompt_budget() {
    let tmpdir = TempDir::new().unwrap();
    let mut registry = super::SkillRegistry::default();
    registry.skills.push(super::Skill {
        legacy_activation_name: None,
        name: "workspace-priority".to_string(),
        description: "must survive truncation".to_string(),
        localized_descriptions: std::collections::HashMap::new(),
        invocation: super::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: "body".to_string(),
        path: tmpdir
            .path()
            .join(".claude")
            .join("skills")
            .join("workspace-priority")
            .join("SKILL.md"),
        source: super::SkillSource::Native,
    });

    let big_desc = "y".repeat(super::MAX_SKILL_DESCRIPTION_CHARS - 20);
    for i in 0..200 {
        registry.skills.push(super::Skill {
            legacy_activation_name: None,
            name: format!("aaa-global-{i:03}"),
            description: big_desc.clone(),
            localized_descriptions: std::collections::HashMap::new(),
            invocation: super::SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: "body".to_string(),
            path: tmpdir
                .path()
                .join(".deepseek")
                .join("skills")
                .join(format!("aaa-global-{i:03}"))
                .join("SKILL.md"),
            source: super::SkillSource::Native,
        });
    }

    let rendered =
        super::render_skills_block(&registry, "en", tmpdir.path()).expect("skill context");
    assert!(
        rendered.contains("workspace-priority"),
        "higher-precedence workspace skills must not be reordered behind globals:\n{rendered}"
    );
    let first_row = rendered
        .lines()
        .find(|line| line.starts_with("- "))
        .expect("at least one row");
    assert!(
        first_row.starts_with("- workspace-priority"),
        "registry order is render order:\n{rendered}"
    );
}

// --- Localized skill descriptions (#3354) ------------------------------

#[test]
fn parse_skill_collects_localized_description_frontmatter() {
    let content = "---\n\
name: demo\n\
description: A demo skill\n\
description_zh: 一个演示技能\n\
description_zh-Hant: 一個示範技能\n\
---\n\
body";
    let skill = super::SkillRegistry::parse_skill(std::path::Path::new("SKILL.md"), content)
        .expect("parse should succeed");
    assert_eq!(skill.description, "A demo skill");
    assert_eq!(
        skill.localized_descriptions.get("zh").map(String::as_str),
        Some("一个演示技能")
    );
    // Frontmatter keys are lowercased, so zh-Hant is stored as zh-hant.
    assert_eq!(
        skill
            .localized_descriptions
            .get("zh-hant")
            .map(String::as_str),
        Some("一個示範技能")
    );
}

#[test]
fn parse_skill_exposes_invocation_and_alias_metadata() {
    let content = "---\n\
name: spreadsheets\n\
description: Spreadsheet workflows\n\
invocation: explicit-only\n\
aliases-for: xlsx, spreadsheet\n\
---\n\
body";
    let skill = super::SkillRegistry::parse_skill(std::path::Path::new("SKILL.md"), content)
        .expect("parse should succeed");

    assert_eq!(skill.invocation, super::SkillInvocation::ExplicitOnly);
    assert_eq!(
        skill.aliases,
        vec!["xlsx".to_string(), "spreadsheet".to_string()]
    );

    let mut registry = super::SkillRegistry::default();
    registry.skills.push(skill);
    assert_eq!(
        registry.get("spreadsheet").map(|s| s.name.as_str()),
        Some("spreadsheets")
    );
    assert_eq!(
        registry.get("xlsx").map(|s| s.name.as_str()),
        Some("spreadsheets")
    );

    let rendered = super::render_skills_block(&registry, "en", std::path::Path::new("/"));
    assert!(
        rendered.is_some(),
        "an explicit-only skill remains loadable"
    );
    assert!(
        !rendered.unwrap_or_default().contains("spreadsheets"),
        "explicit-only skills must not enter the model catalogue"
    );
}

#[test]
fn missing_or_unknown_invocation_keeps_model_and_user_compatibility() {
    for invocation in [None, Some("future-mode")] {
        let invocation_line =
            invocation.map_or(String::new(), |value| format!("invocation: {value}\n"));
        let content =
            format!("---\nname: compatible\ndescription: compatible\n{invocation_line}---\nbody");
        let skill = super::SkillRegistry::parse_skill(std::path::Path::new("SKILL.md"), &content)
            .expect("parse should succeed");
        assert_eq!(skill.invocation, super::SkillInvocation::ModelAndUser);
    }
}

#[test]
fn description_for_locale_matches_exact_then_primary_then_falls_back() {
    let mut localized = std::collections::HashMap::new();
    localized.insert("zh".to_string(), "中文描述".to_string());
    localized.insert("ja".to_string(), "日本語の説明".to_string());
    let skill = super::Skill {
        legacy_activation_name: None,
        name: "demo".to_string(),
        description: "English description".to_string(),
        localized_descriptions: localized,
        invocation: super::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: String::new(),
        path: std::path::PathBuf::new(),
        source: super::SkillSource::Native,
    };

    assert_eq!(skill.description_for_locale("zh"), "中文描述"); // exact
    assert_eq!(skill.description_for_locale("ZH"), "中文描述"); // case-insensitive
    assert_eq!(skill.description_for_locale("zh-CN"), "中文描述"); // Simplified region → zh
    assert_eq!(skill.description_for_locale("zh-Hans"), "中文描述"); // Simplified script → zh
    assert_eq!(skill.description_for_locale("ja"), "日本語の説明");
    assert_eq!(skill.description_for_locale("fr"), "English description"); // fallback
    assert_eq!(skill.description_for_locale("en"), "English description");

    // Traditional Chinese must NOT borrow the Simplified `zh` description:
    // with no exact zh-hant key authored, it falls back to the default.
    assert_eq!(
        skill.description_for_locale("zh-Hant"),
        "English description"
    );
    assert_eq!(skill.description_for_locale("zh-TW"), "English description");
    assert_eq!(skill.description_for_locale("zh-HK"), "English description");
}

#[test]
fn description_for_locale_uses_exact_traditional_key_when_authored() {
    let mut localized = std::collections::HashMap::new();
    localized.insert("zh".to_string(), "简体描述".to_string());
    localized.insert("zh-hant".to_string(), "繁體描述".to_string());
    let skill = super::Skill {
        legacy_activation_name: None,
        name: "demo".to_string(),
        description: "English".to_string(),
        localized_descriptions: localized,
        invocation: super::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: String::new(),
        path: std::path::PathBuf::new(),
        source: super::SkillSource::Native,
    };
    // Exact Traditional key wins for a Traditional session.
    assert_eq!(skill.description_for_locale("zh-Hant"), "繁體描述");
    // Simplified session still gets the Simplified description.
    assert_eq!(skill.description_for_locale("zh-Hans"), "简体描述");
    assert_eq!(skill.description_for_locale("zh"), "简体描述");
}

#[test]
fn description_for_locale_uses_default_when_no_localized_variants() {
    let skill = super::Skill {
        legacy_activation_name: None,
        name: "demo".to_string(),
        description: "only english".to_string(),
        localized_descriptions: std::collections::HashMap::new(),
        invocation: super::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: String::new(),
        path: std::path::PathBuf::new(),
        source: super::SkillSource::Native,
    };
    assert_eq!(skill.description_for_locale("zh"), "only english");
}

#[test]
fn render_skills_block_selects_description_by_locale() {
    let mut registry = super::SkillRegistry::default();
    let mut localized = std::collections::HashMap::new();
    localized.insert("zh".to_string(), "压缩日志的技能".to_string());
    registry.skills.push(super::Skill {
        legacy_activation_name: None,
        name: "compress".to_string(),
        description: "Compress logs to save space".to_string(),
        localized_descriptions: localized,
        invocation: super::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: "body".to_string(),
        path: std::path::PathBuf::from("/skills/compress/SKILL.md"),
        source: super::SkillSource::Native,
    });

    let zh = super::render_skills_block(&registry, "zh-Hans", std::path::Path::new("/"))
        .expect("zh block");
    assert!(
        zh.contains("压缩日志的技能"),
        "zh session should get the zh description:\n{zh}"
    );
    assert!(!zh.contains("Compress logs to save space"));

    let en =
        super::render_skills_block(&registry, "en", std::path::Path::new("/")).expect("en block");
    assert!(
        en.contains("Compress logs to save space"),
        "en session keeps default:\n{en}"
    );
}

fn write_skill(dir: &std::path::Path, name: &str, description: &str, body: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
    )
    .unwrap();
}

#[cfg(unix)]
fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[test]
fn skills_directories_returns_existing_dirs_in_precedence_order() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path();
    crate::test_support::trust_workspace(workspace);

    // Create four of the five workspace candidate dirs (skip `.opencode`).
    std::fs::create_dir_all(workspace.join(".agents").join("skills")).unwrap();
    std::fs::create_dir_all(workspace.join("skills")).unwrap();
    std::fs::create_dir_all(workspace.join(".claude").join("skills")).unwrap();
    std::fs::create_dir_all(workspace.join(".cursor").join("skills")).unwrap();

    let dirs = super::skills_directories_for_mode(workspace, super::SkillDiscoveryMode::Compatible);
    // We don't assert on the global default position because it's
    // host-dependent (may not exist on the test machine).
    let mut idx = 0;
    let agents = workspace.join(".agents").join("skills");
    let local = workspace.join("skills");
    let claude = workspace.join(".claude").join("skills");
    let cursor = workspace.join(".cursor").join("skills");

    assert_eq!(dirs.get(idx), Some(&agents), "agents must come first");
    idx += 1;
    assert!(
        !dirs.contains(&local),
        "flat product content requires opt-in"
    );
    // .opencode/skills was not created — it must NOT appear.
    assert!(
        !dirs
            .iter()
            .any(|p| p == &workspace.join(".opencode").join("skills")),
        "missing dir must be omitted, got: {dirs:?}"
    );
    assert_eq!(
        dirs.get(idx),
        Some(&claude),
        "claude must come after agents"
    );
    idx += 1;
    assert_eq!(
        dirs.get(idx),
        Some(&cursor),
        "cursor must come after claude"
    );
}

#[test]
fn existing_skill_dirs_orders_globals_agents_then_claude_then_deepseek() {
    // Pins the precedence among the three global skill roots (#902).
    // Workspace candidates are tested separately above; here we only
    // exercise the global ordering at the existing_skill_dirs level
    // so the assertion is host-independent.
    let tmpdir = TempDir::new().unwrap();
    let agents_global = tmpdir.path().join(".agents").join("skills");
    let claude_global = tmpdir.path().join(".claude").join("skills");
    let deepseek_global = tmpdir.path().join(".deepseek").join("skills");
    std::fs::create_dir_all(&agents_global).unwrap();
    std::fs::create_dir_all(&claude_global).unwrap();
    std::fs::create_dir_all(&deepseek_global).unwrap();

    let dirs = super::existing_skill_dirs(vec![
        agents_global.clone(),
        claude_global.clone(),
        deepseek_global.clone(),
    ]);

    assert_eq!(dirs, vec![agents_global, claude_global, deepseek_global]);
}

#[test]
fn existing_skill_dirs_keeps_agents_global_before_deepseek_global() {
    let tmpdir = TempDir::new().unwrap();
    let agents_global = tmpdir.path().join(".agents").join("skills");
    let deepseek_global = tmpdir.path().join(".deepseek").join("skills");
    let missing = tmpdir.path().join("missing").join("skills");
    std::fs::create_dir_all(&agents_global).unwrap();
    std::fs::create_dir_all(&deepseek_global).unwrap();

    let dirs = super::existing_skill_dirs(vec![
        missing,
        agents_global.clone(),
        deepseek_global.clone(),
        agents_global.clone(),
    ]);

    assert_eq!(dirs, vec![agents_global, deepseek_global]);
}

#[test]
fn discover_in_workspace_merges_with_first_wins_precedence() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path();
    crate::test_support::trust_workspace(workspace);

    // Same skill name `shared` in two locations — the higher-precedence
    // dir's version should win.
    write_skill(
        &workspace.join(".agents").join("skills"),
        "shared",
        "agents wins",
        "from agents",
    );
    write_skill(
        &workspace.join(".claude").join("skills"),
        "shared",
        "claude loses",
        "from claude",
    );
    // Unique skill in claude — should still be discovered.
    write_skill(
        &workspace.join(".claude").join("skills"),
        "unique-claude",
        "only here",
        "claude-only",
    );

    let registry = super::discover_in_workspace(workspace);
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"shared"),
        "shared must be present: {names:?}"
    );
    assert!(names.contains(&"unique-claude"));

    let shared = registry.get("shared").expect("shared present");
    assert_eq!(
        shared.description, "agents wins",
        "first-wins precedence should keep .agents/skills version"
    );
    assert!(
        shared.path.starts_with(workspace.join(".agents")),
        "shared.path should be from .agents/skills, got {:?}",
        shared.path
    );
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("shared") && warning.contains("shadowed by")),
        "duplicate shadowing should warn, got {:?}",
        registry.warnings()
    );
}

#[test]
fn same_root_slug_collision_warns_and_keeps_one() {
    let tmpdir = TempDir::new().unwrap();
    let root = tmpdir.path();
    // Two sibling directories under one root whose frontmatter names
    // slugify to the same command name ("my-skill"). Only one can be
    // reachable by name; the other must warn rather than silently coexist
    // as an unreachable duplicate (#3919 same-root gap).
    write_skill(root, "My Skill", "first", "body");
    write_skill(root, "my_skill", "second", "body");

    let registry = super::SkillRegistry::discover(root);
    let claimants = registry
        .list()
        .iter()
        .filter(|s| s.name == "my-skill")
        .count();
    assert_eq!(
        claimants,
        1,
        "exactly one skill should claim `my-skill`, got {:?}",
        registry.list().iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    assert!(
        registry
            .warnings()
            .iter()
            .any(|w| w.contains("my-skill") && w.contains("shadowed by")),
        "same-root slug collision should warn, got {:?}",
        registry.warnings()
    );
}

#[test]
fn discover_in_workspace_pulls_skills_from_opencode_dir() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path();
    crate::test_support::trust_workspace(workspace);
    write_skill(
        &workspace.join(".opencode").join("skills"),
        "opencode-only",
        "for interop",
        "body",
    );

    let registry = super::discover_in_workspace(workspace);
    assert!(
        registry.get("opencode-only").is_some(),
        ".opencode/skills must be scanned (#432)"
    );
}

#[test]
fn discover_in_workspace_pulls_skills_from_cursor_dir() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path();
    crate::test_support::trust_workspace(workspace);
    write_skill(
        &workspace.join(".cursor").join("skills"),
        "cursor-only",
        "for cursor interop",
        "body",
    );

    let registry = super::discover_in_workspace(workspace);
    assert!(
        registry.get("cursor-only").is_some(),
        ".cursor/skills must be scanned"
    );
}

#[test]
fn discover_accepts_plain_markdown_heading_without_frontmatter() {
    let tmpdir = TempDir::new().unwrap();
    let skill_dir = tmpdir.path().join("plain-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# Plain Skill\n\nUse this skill without YAML frontmatter.\n",
    )
    .unwrap();

    let registry = super::SkillRegistry::discover(tmpdir.path());
    let skill = registry.get("plain-skill").expect("plain skill parsed");
    assert_eq!(skill.name, "plain-skill");
    assert_eq!(skill.description, "");
    assert!(skill.body.contains("Use this skill"));
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("using `plain-skill` instead")),
        "expected slug warning, got {:?}",
        registry.warnings()
    );
}

#[test]
fn discover_slugifies_invalid_frontmatter_names_and_lookup_normalizes() {
    let tmpdir = TempDir::new().unwrap();
    let root = tmpdir.path().join("skills");
    let skill_dir = root.join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: My Skill\ndescription: spaced name\n---\nbody",
    )
    .unwrap();

    let registry = super::SkillRegistry::discover(&root);
    let skill = registry.get("  MY   skill  ").expect("normalized lookup");
    assert_eq!(skill.name, "my-skill");
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("My Skill")
                && warning.contains("using `my-skill` instead")),
        "expected invalid-name warning, got {:?}",
        registry.warnings()
    );
}

#[test]
fn discover_warns_for_plain_markdown_without_heading() {
    let tmpdir = TempDir::new().unwrap();
    let skill_dir = tmpdir.path().join("plain-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "Use this skill without a heading or YAML frontmatter.\n",
    )
    .unwrap();

    let registry = super::SkillRegistry::discover(tmpdir.path());
    assert!(registry.is_empty());
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("no `# Heading` found")),
        "expected missing-heading warning, got {:?}",
        registry.warnings()
    );
}

#[test]
fn render_available_skills_context_for_workspace_picks_up_cross_tool_dirs() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path();
    crate::test_support::trust_workspace(workspace);
    write_skill(
        &workspace.join(".claude").join("skills"),
        "from-claude",
        "claude-style skill",
        "body",
    );
    let rendered =
        super::render_available_skills_context_for_workspace(workspace).expect("non-empty");
    assert!(rendered.contains("from-claude"));
}

#[test]
fn codewhale_only_mode_ignores_cross_tool_skill_dirs() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path().join("workspace");
    let home = tmpdir.path().join("home");
    let configured_dir = home.join(".codewhale").join("skills");
    std::fs::create_dir_all(&workspace).unwrap();
    crate::test_support::trust_workspace(&workspace);
    write_skill(
        &workspace.join(".claude").join("skills"),
        "from-claude",
        "claude-style skill",
        "body",
    );
    write_skill(
        &workspace.join(".codewhale").join("skills"),
        "from-codewhale",
        "codewhale skill",
        "body",
    );
    write_skill(
        &home.join(".agents").join("skills"),
        "from-agents",
        "agents skill",
        "body",
    );
    write_skill(
        &configured_dir,
        "configured-codewhale",
        "configured skill",
        "body",
    );

    let registry = super::discover_for_workspace_and_dir_with_home_and_mode(
        &workspace,
        &configured_dir,
        Some(&home),
        super::SkillDiscoveryMode::CodeWhaleOnly,
    );
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"from-codewhale"));
    assert!(names.contains(&"configured-codewhale"));
    assert!(
        !names.contains(&"from-claude") && !names.contains(&"from-agents"),
        "CodeWhale-only mode must not import cross-tool skills: {names:?}"
    );
}

#[test]
fn codewhale_only_mode_still_honors_explicit_configured_dir() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path().join("workspace");
    let home = tmpdir.path().join("home");
    let configured_dir = tmpdir.path().join("my-skills");
    std::fs::create_dir_all(&workspace).unwrap();
    write_skill(
        &configured_dir,
        "configured-skill",
        "explicit configured skill",
        "body",
    );

    let registry = super::discover_for_workspace_and_dir_with_home_and_mode(
        &workspace,
        &configured_dir,
        Some(&home),
        super::SkillDiscoveryMode::CodeWhaleOnly,
    );
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();

    assert_eq!(names, vec!["configured-skill"]);
}

#[test]
fn codewhale_only_mode_rejects_workspace_codewhale_symlink_escape() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path().join("workspace");
    let home = tmpdir.path().join("home");
    let escape_target = tmpdir.path().join("escape-target");
    std::fs::create_dir_all(workspace.join(".codewhale")).unwrap();
    write_skill(&escape_target, "escaped-skill", "escaped skill", "body");

    let link_path = workspace.join(".codewhale").join("skills");
    if let Err(err) = create_dir_symlink(&escape_target, &link_path) {
        eprintln!("skipping symlink escape assertion: {err}");
        return;
    }

    let registry = super::discover_for_workspace_and_dir_with_home_and_mode(
        &workspace,
        &tmpdir.path().join("missing-configured-skills"),
        Some(&home),
        super::SkillDiscoveryMode::CodeWhaleOnly,
    );

    assert!(
        registry.get("escaped-skill").is_none(),
        "CodeWhale-only mode must not follow workspace .codewhale/skills outside the workspace"
    );
}

#[test]
fn discover_for_workspace_and_dir_merges_workspace_and_configured_sources() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path().join("workspace");
    let home = tmpdir.path().join("home");
    let configured_dir = tmpdir.path().join("configured-skills");
    std::fs::create_dir_all(&workspace).unwrap();
    crate::test_support::trust_workspace(&workspace);
    write_skill(
        &workspace.join(".claude").join("skills"),
        "workspace-skill",
        "workspace visible skill",
        "body",
    );
    write_skill(
        &configured_dir,
        "configured-skill",
        "configured visible skill",
        "body",
    );

    let registry =
        super::discover_for_workspace_and_dir_with_home(&workspace, &configured_dir, Some(&home));
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"workspace-skill"));
    assert!(names.contains(&"configured-skill"));
}

#[test]
fn explicit_configured_skills_dir_precedes_global_defaults() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path().join("workspace");
    let home = tmpdir.path().join("home");
    let configured_dir = tmpdir.path().join("configured-skills");
    std::fs::create_dir_all(&workspace).unwrap();
    write_skill(
        &home.join(".agents").join("skills"),
        "shared-skill",
        "global skill",
        "global body",
    );
    write_skill(
        &configured_dir,
        "shared-skill",
        "configured skill",
        "configured body",
    );

    let registry =
        super::discover_for_workspace_and_dir_with_home(&workspace, &configured_dir, Some(&home));
    let skill = registry
        .get("shared-skill")
        .expect("shared skill discovered");

    assert_eq!(skill.description, "configured skill");
}

/// Regression for the GitHub issue where users organize skills under
/// vendor / category subdirectories (e.g. cloned skill repos that
/// bundle several skills together). The old single-level `read_dir`
/// only ever surfaced `<root>/<skill>/SKILL.md` and silently ignored
/// `<root>/<vendor>/<skill>/SKILL.md`.
#[test]
fn discover_finds_skills_nested_under_vendor_subdirectory() {
    let tmpdir = TempDir::new().unwrap();
    let root = tmpdir.path().join("skills");

    // Two-level nesting: `<root>/<vendor>/<skill>/SKILL.md`. This
    // matches the `clawhub-skills/clawhub/SKILL.md` layout in the
    // bug report.
    write_skill(
        &root.join("clawhub-skills"),
        "clawhub",
        "claw search",
        "body",
    );
    write_skill(
        &root.join("clawhub-skills"),
        "github",
        "github helpers",
        "body",
    );
    // Three-level nesting: `<root>/<org>/<repo>/<skill>/SKILL.md`.
    write_skill(
        &root.join("pasky").join("chrome-cdp-skill"),
        "chrome-cdp",
        "browser automation",
        "body",
    );
    // Mixed-depth: a flat skill alongside the nested layout still
    // works (this is what the bundled `skill-creator` looks like).
    write_skill(&root, "skill-creator", "make skills", "body");

    let registry = super::SkillRegistry::discover(&root);
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"clawhub"), "vendor/skill missed: {names:?}");
    assert!(names.contains(&"github"), "vendor/skill missed: {names:?}");
    assert!(
        names.contains(&"chrome-cdp"),
        "deeply-nested skill missed: {names:?}"
    );
    assert!(
        names.contains(&"skill-creator"),
        "flat top-level skill must still load: {names:?}"
    );
    assert!(
        registry.warnings().is_empty(),
        "well-formed nested layout should not warn: {:?}",
        registry.warnings()
    );
}

#[cfg(any(unix, windows))]
#[test]
fn discover_follows_symlinked_skill_directories() {
    let tmpdir = TempDir::new().unwrap();
    let source_root = tmpdir.path().join("claude-skills");
    let skills_root = tmpdir.path().join(".deepseek").join("skills");
    write_skill(&source_root, "agent-browser", "browser automation", "body");
    std::fs::create_dir_all(&skills_root).unwrap();
    let link_path = skills_root.join("agent-browser");

    if let Err(err) = create_dir_symlink(&source_root.join("agent-browser"), &link_path) {
        eprintln!("skipping symlink discovery assertion: {err}");
        return;
    }

    let registry = super::SkillRegistry::discover(&skills_root);
    let skill = registry
        .get("agent-browser")
        .expect("symlinked skill directory should be discovered");
    assert_eq!(skill.description, "browser automation");
    assert_eq!(skill.path, link_path.join("SKILL.md"));
}

#[cfg(any(unix, windows))]
#[test]
fn discover_dedupes_symlink_cycles_by_canonical_directory() {
    let tmpdir = TempDir::new().unwrap();
    let root = tmpdir.path().join("skills");
    write_skill(&root, "real-skill", "ok", "body");
    let loop_parent = root.join("vendor");
    std::fs::create_dir_all(&loop_parent).unwrap();

    if let Err(err) = create_dir_symlink(&root, &loop_parent.join("loop")) {
        eprintln!("skipping symlink cycle assertion: {err}");
        return;
    }

    let registry = super::SkillRegistry::discover(&root);
    let matches = registry
        .list()
        .iter()
        .filter(|skill| skill.name == "real-skill")
        .count();
    assert_eq!(
        matches, 1,
        "symlink cycle should not rediscover the same canonical skill directory"
    );
}

/// Once a directory is identified as a skill (has `SKILL.md`), the
/// walker must NOT descend into it: any nested `SKILL.md` would be
/// a fixture / example bundled with the parent skill, not a
/// separately-installable one. This mirrors the contract that
/// `tools::skill::collect_companion_files` already documents
/// ("nested directory — skipped").
#[test]
fn discover_does_not_descend_into_a_skill_directory() {
    let tmpdir = TempDir::new().unwrap();
    let root = tmpdir.path().join("skills");

    // Parent skill: <root>/parent/SKILL.md.
    write_skill(&root, "parent", "outer skill", "outer body");
    // Fixture bundled inside the parent's directory:
    // <root>/parent/examples/inner-fixture/SKILL.md. The walker
    // must NOT descend into <root>/parent/ after finding its
    // SKILL.md, so `inner-fixture` must not be loaded.
    write_skill(
        &root.join("parent").join("examples"),
        "inner-fixture",
        "should not load",
        "fixture body",
    );

    let registry = super::SkillRegistry::discover(&root);
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"parent"));
    assert!(
        !names.contains(&"inner-fixture"),
        "nested SKILL.md inside an existing skill must be ignored: {names:?}"
    );
}

/// Hidden subdirectories below the root (e.g. `.git`, `.cache`) must
/// be skipped so a `skills_dir` that lives inside a checked-out repo
/// doesn't accidentally load random `SKILL.md`-named fixtures from
/// the VCS metadata. The root itself is exempt — the user explicitly
/// pointed `skills_dir` at it.
#[test]
fn discover_skips_hidden_subdirectories_below_root() {
    let tmpdir = TempDir::new().unwrap();
    let root = tmpdir.path().join("skills");

    write_skill(&root, "real-skill", "ok", "body");
    // A `<root>/.git/<junk>/SKILL.md` lookalike that mustn't load.
    // `.git` is a direct child of the user-provided root (depth 0
    // of the walk), which is exactly the case the old `depth > 0`
    // gate missed.
    write_skill(&root.join(".git"), "vcs-noise", "should not load", "body");

    let registry = super::SkillRegistry::discover(&root);
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"real-skill"));
    assert!(
        !names.contains(&"vcs-noise"),
        "skills under hidden subdirs must be skipped: {names:?}"
    );
}

/// The user explicitly chooses the root, so even a hidden path like
/// `~/.agents/skills` (the layout in the bug report) must work.
#[test]
fn discover_honors_a_hidden_root_directory() {
    let tmpdir = TempDir::new().unwrap();
    let root = tmpdir.path().join(".agents").join("skills");

    // Matches the bug report: skills_dir = "~/.agents/skills"
    // with a skill nested at <root>/custom-skills/git-conventions/SKILL.md.
    write_skill(
        &root.join("custom-skills"),
        "git-conventions",
        "conventions",
        "body",
    );

    let registry = super::SkillRegistry::discover(&root);
    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"git-conventions"),
        "hidden root must still be walked: {names:?}"
    );
}

/// Exercises the local/global skill inventory independent of terminal layout.
/// scenario without the PTY harness: a workspace-level skill in
/// `.agents/skills/` and a global skill in `~/.codewhale/skills/`
/// must both be discoverable.
#[test]
fn discover_finds_both_workspace_and_global_skills() {
    let tmpdir = TempDir::new().unwrap();
    let workspace = tmpdir.path().join("workspace");
    let home = tmpdir.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();

    write_skill(
        &workspace.join(".agents").join("skills"),
        "workspace-beta",
        "Workspace beta skill",
        "body",
    );
    write_skill(
        &home.join(".codewhale").join("skills"),
        "global-alpha",
        "Global alpha skill",
        "body",
    );

    crate::test_support::trust_workspace(&workspace);
    let skills_dir = workspace.join(".agents").join("skills");
    let registry =
        super::discover_for_workspace_and_dir_with_home(&workspace, &skills_dir, Some(&home));

    let names: Vec<&str> = registry.list().iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"workspace-beta"),
        "workspace-beta from .agents/skills must be discovered: {names:?}",
    );
    assert!(
        names.contains(&"global-alpha"),
        "global-alpha from ~/.codewhale/skills must be discovered: {names:?}",
    );
}

// ── Block scalar parsing (YAML `>` and `|`) ────────────────

/// `>` (folded block scalar): subsequent indented lines are folded
/// into a single line joined by spaces.
#[test]
fn parse_skill_folded_block_scalar() {
    let tmpdir = TempDir::new().unwrap();
    create_skill_dir(
        &tmpdir,
        "folded-skill",
        "---\nname: folded-skill\ndescription: >\n  line one chinese\n  line two chinese\n---\nbody",
    );
    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");
    assert!(
        rendered.contains("line one chinese line two chinese"),
        "folded block scalar should join lines with space, got:\n{rendered}"
    );
}

/// `|` (literal block scalar): subsequent indented lines preserve
/// newlines.
#[test]
fn parse_skill_literal_block_scalar() {
    let tmpdir = TempDir::new().unwrap();
    create_skill_dir(
        &tmpdir,
        "literal-skill",
        "---\nname: literal-skill\ndescription: |\n  line one\n  line two\n---\nbody",
    );
    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");
    // `truncate_for_prompt` collapses whitespace, so the newlines
    // become spaces. The key assertion is that the content is
    // captured (not just `|`).
    assert!(
        rendered.contains("line one line two"),
        "literal block scalar should preserve content, got:\n{rendered}"
    );
}

/// `>-` (folded with strip chomping): same as `>` but trailing
/// whitespace is stripped.
#[test]
fn parse_skill_folded_strip_block_scalar() {
    let tmpdir = TempDir::new().unwrap();
    create_skill_dir(
        &tmpdir,
        "strip-skill",
        "---\nname: strip-skill\ndescription: >-\n  alpha\n  beta\n\n---\nbody",
    );
    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");
    assert!(
        rendered.contains("alpha beta"),
        "strip-chomped folded block should join lines, got:\n{rendered}"
    );
}

/// Regression: a single-line description (no block scalar) must
/// still parse correctly after the parser rewrite.
#[test]
fn parse_skill_single_line_description_still_works() {
    let tmpdir = TempDir::new().unwrap();
    create_skill_dir(
        &tmpdir,
        "plain-skill",
        "---\nname: plain-skill\ndescription: A simple description\n---\nbody",
    );
    let rendered = crate::skills::render_available_skills_context(&tmpdir.path().join("skills"))
        .expect("skill context");
    assert!(
        rendered.contains("- plain-skill: A simple description"),
        "single-line description should still work, got:\n{rendered}"
    );
}

/// Direct unit test on the parsed Skill struct (not through rendering)
/// so we assert the exact description value.
#[test]
fn parse_skill_direct_folded_result() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: test\ndescription: >\n  this is a test\n  used to verify parsing\n---\nbody",
    )
    .expect("should parse");
    assert_eq!(skill.name, "test");
    assert_eq!(skill.description, "this is a test used to verify parsing");
}

// ── Chomping behaviour ────────────────────────────────────

/// `>-` (strip): trailing empty lines are stripped. Paragraph
/// breaks (empty line between text lines) are still folded to a
/// single space in a block-scalar join (no newline — the simplified
/// parser treats intra-block empty lines as paragraph breaks that
/// become a single space in the folded output).
#[test]
fn parse_skill_strip_chomp_strips_trailing_empties() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: >-\n  hello\n  world\n\n\n---\nbody",
    )
    .expect("should parse");
    // Trailing empty lines stripped: no whitespace at end, just folded text.
    assert_eq!(skill.description, "hello world");
}

/// `>+` (keep): trailing empty lines are preserved. Each trailing
/// empty line in the block becomes a newline in the description.
#[test]
fn parse_skill_keep_chomp_preserves_trailing_empties() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: >+\n  hello\n  world\n\n\n---\nbody",
    )
    .expect("should parse");
    // Two trailing empty lines should become two newlines.
    assert_eq!(skill.description, "hello world\n\n");
}

/// `>` (clip): trailing empty lines exceeding one are clipped.
/// The result should have at most one trailing newline.
#[test]
fn parse_skill_clip_chomp_clips_excess_trailing_empties() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: >\n  hello\n  world\n\n\n---\nbody",
    )
    .expect("should parse");
    // clip: 3 trailing empty lines → at most 1 trailing newline.
    assert_eq!(skill.description, "hello world\n");
}

/// `>` with no trailing empty lines: clip should not add anything.
#[test]
fn parse_skill_clip_chomp_no_trailing_empties() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: >\n  hello\n  world\n---\nbody",
    )
    .expect("should parse");
    assert_eq!(skill.description, "hello world");
}

/// `>` with exactly one trailing empty line: clip keeps it.
#[test]
fn parse_skill_clip_chomp_one_trailing_empty() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: >\n  hello\n  world\n\n---\nbody",
    )
    .expect("should parse");
    assert_eq!(skill.description, "hello world\n");
}

/// `>-` strip vs `>+` keep: same block content, different
/// trailing newline handling.
#[test]
fn parse_skill_strip_vs_keep_trailing() {
    let content = "---\nname: s\ndescription: >{}\n  hello\n  world\n\n\n---\nbody";
    let strip_skill =
        super::SkillRegistry::parse_skill(std::path::Path::new(""), &content.replace("{}", "-"))
            .expect("strip parse");
    let keep_skill =
        super::SkillRegistry::parse_skill(std::path::Path::new(""), &content.replace("{}", "+"))
            .expect("keep parse");
    // strip drops trailing empties; keep preserves them.
    assert_eq!(strip_skill.description, "hello world");
    assert_eq!(keep_skill.description, "hello world\n\n");
}

/// `|-` literal strip: trailing newlines are stripped.
#[test]
fn parse_skill_literal_strip_strips_trailing_newlines() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: |-\n  line one\n  line two\n\n\n---\nbody",
    )
    .expect("should parse");
    // literal: newlines preserved between non-empty lines.
    // strip: trailing empty lines removed.
    assert_eq!(skill.description, "line one\nline two");
}

/// `|+` literal keep: trailing newlines are preserved.
#[test]
fn parse_skill_literal_keep_preserves_trailing_newlines() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: |+\n  line one\n  line two\n\n\n---\nbody",
    )
    .expect("should parse");
    // literal: newlines preserved between non-empty lines.
    // keep: trailing empty lines are preserved as newlines.
    assert_eq!(skill.description, "line one\nline two\n\n");
}

/// Nested relative indentation is preserved in literal (`|`) block
/// scalars: only the content-level indent (from the first non-empty
/// line) is stripped, and any deeper indent stays as-is.
#[test]
fn parse_skill_literal_preserves_relative_indentation() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: |\n  Usage:\n    $ deepseek --model auto\n    $ deepseek doctor\n---\nbody",
    )
    .expect("should parse");
    assert_eq!(
        skill.description,
        "Usage:\n  $ deepseek --model auto\n  $ deepseek doctor"
    );
}

/// Folded (`>`) block scalars also preserve relative indentation
/// within lines (the extra spaces survive the fold).
#[test]
fn parse_skill_folded_preserves_relative_indentation() {
    let skill = super::SkillRegistry::parse_skill(
        std::path::Path::new(""),
        "---\nname: s\ndescription: >\n  See also:\n    the config file\n    the env var\n---\nbody",
    )
    .expect("should parse");
    assert_eq!(
        skill.description,
        "See also:   the config file   the env var"
    );
}

#[test]
fn plugin_skills_are_qualified_and_denied_until_trusted_and_enabled() {
    let tmp = TempDir::new().unwrap();
    let plugin_root = tmp.path().join("plugins/demo");
    std::fs::create_dir_all(plugin_root.join("skills/hello-world")).unwrap();
    std::fs::write(
        plugin_root.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    )
    .unwrap();
    std::fs::write(
        plugin_root.join("skills/hello-world/SKILL.md"),
        "---\nname: hello-world\ndescription: hello\n---\nbody\n",
    )
    .unwrap();
    let config = crate::plugins::discovery::DiscoveryConfig {
        workspace: tmp.path().join("workspace"),
        user_plugins_dir: tmp.path().join("plugins"),
        workspace_plugins_dir: tmp.path().join("workspace-plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: tmp.path().join("plugin-state/state.json"),
    };
    let mut plugins = crate::plugins::discovery::discover_with_config(&config);

    let mut registry = super::SkillRegistry::default();
    super::merge_active_plugin_skills(&mut registry, &plugins);
    assert!(registry.get("demo:hello-world").is_none());

    plugins.trust("demo").unwrap();
    super::merge_active_plugin_skills(&mut registry, &plugins);
    assert!(registry.get("demo:hello-world").is_none());

    plugins.enable("demo").unwrap();
    super::merge_active_plugin_skills(&mut registry, &plugins);
    let skill = registry
        .get("Demo:Hello_World")
        .expect("qualified lookup should normalize each namespace segment");
    assert_eq!(skill.name, "demo:hello-world");
    assert!(matches!(
        skill.source,
        super::SkillSource::Plugin { ref plugin_name, .. } if plugin_name == "demo"
    ));
    let rendered = super::render_skills_block(&registry, "en", tmp.path()).unwrap();
    assert!(rendered.contains("reviewed plugin snapshot: demo"));
    assert!(rendered.contains("use load_skill"));
    assert!(
        rendered.contains("hello"),
        "plugin skill descriptions must reach the model catalogue like native skills: {rendered}"
    );
    assert!(
        !rendered.contains(&plugin_root.display().to_string()),
        "model prompt must not expose mutable plugin files after snapshot review"
    );

    let mut fail_closed_input = registry.clone();
    fail_closed_input.skills.push(super::Skill {
        legacy_activation_name: None,
        name: "native-recovery".to_string(),
        description: "native recovery skill".to_string(),
        localized_descriptions: std::collections::HashMap::new(),
        invocation: super::SkillInvocation::ModelAndUser,
        aliases: Vec::new(),
        argument_hint: None,
        body: "recovery".to_string(),
        path: tmp.path().join("native/SKILL.md"),
        source: super::SkillSource::Native,
    });
    let fail_closed = fail_closed_input.into_enabled_with_state(Err(anyhow::anyhow!(
        "injected activation-state read failure"
    )));
    assert!(fail_closed.get("native-recovery").is_some());
    assert!(
        fail_closed.get("demo:hello-world").is_none(),
        "reviewed plugin Skills must not fail open when activation state is unreadable"
    );
    assert!(
        fail_closed
            .warnings()
            .iter()
            .any(|warning| warning.contains("hidden fail-closed"))
    );

    std::fs::remove_file(config.state_path.with_file_name("state.json.lock")).unwrap();
    let mut denied = super::SkillRegistry::default();
    super::merge_active_plugin_skills(&mut denied, &plugins);
    assert!(
        denied.get("demo:hello-world").is_none(),
        "a missing authority lock must remove plugin instructions from the prompt catalogue"
    );
}

// --- #3921 merged discovery cache -----------------------------------------

fn discovery_delta_since(earlier: super::SkillDiscoveryMetrics) -> super::SkillDiscoveryMetrics {
    super::discovery_metrics_snapshot().delta_since(earlier)
}

/// Run a cache-hit measurement on a cache no other test emptied meanwhile.
///
/// The discovery cache is process-wide. Under `cargo test` every test shares
/// it, and other tests (and the app code they drive) empty it at any moment,
/// which turns a genuine hit into a re-walk. Each attempt starts from an
/// empty cache and is kept only if nothing else dropped the cache while it
/// ran.
fn measure_on_undisturbed_cache<T>(mut measure: impl FnMut() -> T) -> T {
    for _ in 0..200 {
        super::clear_skill_discovery_cache();
        let drops = super::discovery_cache_drops();
        let measured = measure();
        if super::discovery_cache_drops() == drops {
            return measured;
        }
    }
    panic!("other tests emptied the shared skill discovery cache during every attempt");
}

#[test]
fn cached_discovery_reuses_unchanged_registry_without_rewalking() {
    let tmpdir = TempDir::new().unwrap();
    let skills_root = tmpdir.path().join("skills");
    write_skill(&skills_root, "demo", "A demo skill", "Instructions");
    let dirs = vec![skills_root];

    let (first, walked, second, rewalked) = measure_on_undisturbed_cache(|| {
        super::reset_discovery_metrics();
        let first = super::discover_from_directories_with_plugins(dirs.clone(), None);
        let walked = discovery_delta_since(super::SkillDiscoveryMetrics::default());
        let second = super::discover_from_directories_with_plugins(dirs.clone(), None);
        let rewalked = discovery_delta_since(walked);
        (first, walked, second, rewalked)
    });

    assert_eq!(walked.root_discovery_calls, 1);
    assert_eq!(rewalked, super::SkillDiscoveryMetrics::default());
    assert_eq!(first.len(), second.len());
    assert_eq!(first.list()[0].description, second.list()[0].description);
}

#[test]
fn cached_discovery_picks_up_added_skill_on_next_call() {
    super::clear_skill_discovery_cache();
    let tmpdir = TempDir::new().unwrap();
    let skills_root = tmpdir.path().join("skills");
    write_skill(&skills_root, "demo", "A demo skill", "Instructions");
    let dirs = vec![skills_root.clone()];

    let first = super::discover_from_directories_with_plugins(dirs.clone(), None);
    assert_eq!(first.len(), 1);

    write_skill(&skills_root, "added", "A later skill", "More");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let second = super::discover_from_directories_with_plugins(dirs, None);
    assert_eq!(second.len(), 2);
    assert!(second.get("added").is_some());
}

#[test]
fn cached_discovery_picks_up_skill_content_edits() {
    super::clear_skill_discovery_cache();
    let tmpdir = TempDir::new().unwrap();
    let skills_root = tmpdir.path().join("skills");
    write_skill(&skills_root, "demo", "Original description", "Instructions");
    let dirs = vec![skills_root.clone()];

    let first = super::discover_from_directories_with_plugins(dirs.clone(), None);
    assert_eq!(first.list()[0].description, "Original description");

    write_skill(&skills_root, "demo", "Edited description", "Instructions");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let second = super::discover_from_directories_with_plugins(dirs, None);
    assert_eq!(second.list()[0].description, "Edited description");
}

#[test]
fn cached_discovery_drops_removed_skills() {
    super::clear_skill_discovery_cache();
    let tmpdir = TempDir::new().unwrap();
    let skills_root = tmpdir.path().join("skills");
    write_skill(&skills_root, "keep", "Keep me", "Instructions");
    write_skill(&skills_root, "drop", "Drop me", "Instructions");
    let dirs = vec![skills_root.clone()];

    let first = super::discover_from_directories_with_plugins(dirs.clone(), None);
    assert_eq!(first.len(), 2);

    std::fs::remove_dir_all(skills_root.join("drop")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let second = super::discover_from_directories_with_plugins(dirs, None);
    assert_eq!(second.len(), 1);
    assert!(second.get("drop").is_none());
}

#[test]
fn clear_skill_discovery_cache_forces_a_fresh_walk() {
    super::clear_skill_discovery_cache();
    let tmpdir = TempDir::new().unwrap();
    let skills_root = tmpdir.path().join("skills");
    write_skill(&skills_root, "demo", "A demo skill", "Instructions");
    let dirs = vec![skills_root];

    let _ = super::discover_from_directories_with_plugins(dirs.clone(), None);
    super::clear_skill_discovery_cache();

    super::reset_discovery_metrics();
    let _ = super::discover_from_directories_with_plugins(dirs, None);
    let rewalked = discovery_delta_since(super::SkillDiscoveryMetrics::default());
    assert_eq!(rewalked.root_discovery_calls, 1);
}

#[test]
fn workspace_and_dir_entry_point_shares_the_same_cache() {
    let _env_lock = crate::test_support::lock_test_env();
    super::clear_skill_discovery_cache();
    let tmpdir = TempDir::new().unwrap();
    let home = tmpdir.path().join("home");
    let workspace = tmpdir.path().join("workspace");
    let skills_dir = tmpdir.path().join("configured-skills");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    write_skill(
        &skills_dir,
        "configured",
        "Configured skill",
        "Instructions",
    );
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let _codewhale_home =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));

    let (first, walked, second, rewalked) = measure_on_undisturbed_cache(|| {
        super::reset_discovery_metrics();
        let first = super::discover_for_workspace_and_dir_with_mode_and_plugins(
            &workspace,
            &skills_dir,
            super::SkillDiscoveryMode::Compatible,
            None,
        );
        let walked = discovery_delta_since(super::SkillDiscoveryMetrics::default());
        let second = super::discover_for_workspace_and_dir_with_mode_and_plugins(
            &workspace,
            &skills_dir,
            super::SkillDiscoveryMode::Compatible,
            None,
        );
        let rewalked = discovery_delta_since(walked);
        (first, walked, second, rewalked)
    });

    assert!(walked.root_discovery_calls >= 1);
    assert_eq!(rewalked, super::SkillDiscoveryMetrics::default());
    assert_eq!(first.len(), second.len());
    assert!(second.get("configured").is_some());
}

#[test]
fn configured_skill_prompt_uses_a_stable_root_in_entries_and_warnings() {
    let _env_lock = crate::test_support::lock_test_env();
    super::clear_skill_discovery_cache();
    let tmpdir = TempDir::new().unwrap();
    let home = tmpdir.path().join("home");
    let workspace = home.join("workspace");
    let skills_dir = home
        .join("runtime")
        .join("sessions")
        .join("session-123")
        .join("skills");
    std::fs::create_dir_all(&workspace).unwrap();
    write_skill(
        &workspace.join(".claude").join("skills"),
        "workspace-skill",
        "Workspace skill",
        "Instructions",
    );
    let configured_skill = skills_dir.join("visual-design");
    std::fs::create_dir_all(&configured_skill).unwrap();
    std::fs::write(
        configured_skill.join("SKILL.md"),
        "---\nname: Visual Design\ndescription: Design assets\n---\nInstructions",
    )
    .unwrap();
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let _codewhale_home =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));
    crate::test_support::trust_workspace(&workspace);

    let rendered =
        super::render_available_skills_context_for_workspace_and_dir_with_mode_and_plugins(
            &workspace,
            &skills_dir,
            super::SkillDiscoveryMode::Compatible,
            "en",
            None,
            super::MAX_AVAILABLE_SKILLS_CHARS,
        )
        .expect("configured skill context");

    assert!(rendered.contains("- visual-design: Design assets\n"));
    assert!(rendered.contains("- workspace-skill: Workspace skill\n"));
    assert!(
        rendered
            .contains("in <configured-skills>/visual-design/SKILL.md is not a safe command name")
    );
    assert!(!rendered.contains("session-123"), "{rendered}");
    assert!(!rendered.contains(home.to_str().unwrap()), "{rendered}");
}

#[test]
fn default_workspace_skill_prompt_omits_paths_in_favour_of_load_skill() {
    let _env_lock = crate::test_support::lock_test_env();
    super::clear_skill_discovery_cache();
    let tmpdir = TempDir::new().unwrap();
    let home = tmpdir.path().join("home");
    let workspace = home.join("workspace");
    let skills_dir = workspace.join(".agents").join("skills");
    std::fs::create_dir_all(&home).unwrap();
    write_skill(
        &skills_dir,
        "workspace-skill",
        "Workspace skill",
        "Instructions",
    );
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let _codewhale_home =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));
    crate::test_support::trust_workspace(&workspace);

    let rendered =
        super::render_available_skills_context_for_workspace_and_dir_with_mode_and_plugins(
            &workspace,
            &skills_dir,
            super::SkillDiscoveryMode::Compatible,
            "en",
            None,
            super::MAX_AVAILABLE_SKILLS_CHARS,
        )
        .expect("workspace skill context");

    assert!(rendered.contains("- workspace-skill: Workspace skill\n"));
}

#[test]
fn global_skill_roots_come_from_the_os_home_only() {
    // §2.5: global skill roots resolve under the OS user's home (or an
    // explicit `$CODEWHALE_HOME`), never an account/GitHub handle. A wrong
    // home once produced `Failed to read /Users/<handle>/.codewhale/skills/
    // delegate/SKILL.md`; pin the source so every global root is provably
    // under the faked OS home.
    let _env_lock = crate::test_support::lock_test_env();
    let tmpdir = TempDir::new().unwrap();
    let home = tmpdir.path().join("os-home");
    let workspace = tmpdir.path().join("workspace");
    std::fs::create_dir_all(home.join(".codewhale").join("skills")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let _home = crate::test_support::EnvVarGuard::set("HOME", &home);
    let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);
    let _codewhale_home = crate::test_support::EnvVarGuard::remove("CODEWHALE_HOME");

    let dirs =
        super::skills_directories_for_mode(&workspace, super::SkillDiscoveryMode::Compatible);

    assert!(
        dirs.iter().any(|dir| dir.starts_with(&home)),
        "expected at least one global root under the OS home: {dirs:?}"
    );
    assert!(
        dirs.iter()
            .all(|dir| dir.starts_with(&home) || dir.starts_with(&workspace)),
        "every runtime root is under the OS home or the workspace: {dirs:?}"
    );
}

/// Passing a workspace skills dir as the session's `skills_dir` (as
/// `resolve_skills_dir` and the runtime API did) must not re-admit what the
/// root catalog's trust gate filtered out, and the warning must name the dir.
fn assert_skills_dir_held_to_workspace_trust(
    workspace: &std::path::Path,
    skills_dir: &std::path::Path,
    mode: super::SkillDiscoveryMode,
) {
    write_skill(
        skills_dir,
        "repo-skill",
        "from the repository",
        "do repo things",
    );

    let registry = super::discover_for_workspace_and_dir_with_mode_and_plugins(
        workspace, skills_dir, mode, None,
    );
    assert!(
        registry.get("repo-skill").is_none(),
        "untrusted workspace skill loaded via skills_dir {}",
        skills_dir.display()
    );
    assert!(
        !super::skill_directories_for_workspace_and_dir(workspace, skills_dir, mode)
            .iter()
            .any(|dir| super::roots::paths_refer_to_same_dir(dir, skills_dir)),
        "untrusted workspace skills_dir must not be searched"
    );
    let dir_name = skills_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("not trusted") && warning.contains(&dir_name)),
        "{:?}",
        registry.warnings()
    );

    crate::test_support::trust_workspace(workspace);
    let registry = super::discover_for_workspace_and_dir_with_mode_and_plugins(
        workspace, skills_dir, mode, None,
    );
    assert!(registry.get("repo-skill").is_some());
    assert!(
        !registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("not trusted")),
        "{:?}",
        registry.warnings()
    );
}

#[test]
fn untrusted_workspace_agents_skills_via_skills_dir_not_loaded() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("ws");
    assert_skills_dir_held_to_workspace_trust(
        &workspace,
        &workspace.join(".agents").join("skills"),
        super::SkillDiscoveryMode::Compatible,
    );
}

#[test]
fn untrusted_workspace_flat_skills_via_skills_dir_not_loaded() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("ws");
    assert_skills_dir_held_to_workspace_trust(
        &workspace,
        &workspace.join("skills"),
        super::SkillDiscoveryMode::Compatible,
    );
}

#[test]
fn untrusted_workspace_codewhale_only_skills_dir_not_loaded() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("ws");
    assert_skills_dir_held_to_workspace_trust(
        &workspace,
        &workspace.join(".codewhale").join("skills"),
        super::SkillDiscoveryMode::CodeWhaleOnly,
    );
}

/// An explicitly configured dir that lives inside the repository is still
/// repository content: it waits for trust like the built-in project roots.
#[test]
fn untrusted_workspace_custom_configured_dir_inside_workspace_not_loaded() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("ws");
    assert_skills_dir_held_to_workspace_trust(
        &workspace,
        &workspace.join("my-skills"),
        super::SkillDiscoveryMode::Compatible,
    );
}

/// Repository-supplied skills are instructions nobody reviewed: they stay out
/// of runtime discovery until the workspace is trusted, and discovery says so
/// rather than dropping them silently.
#[test]
fn project_skills_require_workspace_trust() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path();
    write_skill(
        &workspace.join(".claude").join("skills"),
        "repo-skill",
        "from the repository",
        "do repo things",
    );
    write_skill(
        &workspace.join(".codewhale").join("skills"),
        "owned-project-skill",
        "project owned",
        "owned",
    );

    let registry = super::discover_in_workspace(workspace);
    assert!(registry.get("repo-skill").is_none());
    assert!(registry.get("owned-project-skill").is_none());
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("not trusted") && warning.contains("/trust")),
        "{:?}",
        registry.warnings()
    );

    crate::config::save_workspace_trust(workspace).expect("trust workspace");
    let registry = super::discover_in_workspace(workspace);
    assert!(registry.get("repo-skill").is_some());
    assert!(registry.get("owned-project-skill").is_some());
    assert!(
        !registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("not trusted")),
        "{:?}",
        registry.warnings()
    );
}

#[test]
fn hidden_and_backup_payload_changes_stale_the_trust_receipt() {
    use super::audit::{self, SkillAuditMode, TrustState};
    use super::install::{INSTALLED_FROM_MARKER, TRUSTED_MARKER, write_trust_v2};
    use super::package_digest::compute_package_digest;
    use std::fs;
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join(".codewhale/skills/demo");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("SKILL.md"),
        "---\nname: demo\ndescription: test\n---\nbody",
    )
    .unwrap();
    let payloads = [
        ".hidden",
        ".hidden-dir/run.sh",
        "script.bak",
        "script.tmp",
        "nested/.trusted",
    ];
    for relative in payloads {
        let path = package.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "safe").unwrap();
    }
    let digest = compute_package_digest(&package).unwrap();
    write_trust_v2(&package, &digest).unwrap();
    let initial = audit::scan(tmp.path(), None, SkillAuditMode::OwnedOnly, None);
    assert_eq!(initial.skills.len(), 1);
    assert_eq!(
        initial.skills[0].trust,
        TrustState::TrustedForDigest(digest.clone())
    );
    for relative in payloads {
        fs::write(package.join(relative), "evil").unwrap();
        assert_ne!(
            compute_package_digest(&package).unwrap(),
            digest,
            "{relative}"
        );
        let changed = audit::scan(tmp.path(), None, SkillAuditMode::OwnedOnly, None);
        assert_eq!(
            changed.skills[0].trust,
            TrustState::TrustStale,
            "{relative}"
        );
        fs::write(package.join(relative), "safe").unwrap();
    }
    // These root files are local bookkeeping, not executable payload.
    for marker in [
        INSTALLED_FROM_MARKER,
        TRUSTED_MARKER,
        ".system-installed-version",
    ] {
        fs::write(package.join(marker), "local metadata").unwrap();
        assert_eq!(compute_package_digest(&package).unwrap(), digest);
    }
}

#[test]
fn owned_global_root_shadows_claude_compat() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("ws");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    write_skill(
        &home.join(".codewhale/skills"),
        "shared",
        "owned",
        "owned body",
    );
    write_skill(
        &home.join(".agents/skills"),
        "shared",
        "agents",
        "agents body",
    );
    write_skill(
        &home.join(".claude/skills"),
        "shared",
        "claude",
        "claude body",
    );
    let registry = super::discover_for_workspace_and_dir_with_home(
        &workspace,
        &home.join(".codewhale/skills"),
        Some(&home),
    );
    assert_eq!(registry.get("shared").unwrap().body, "owned body");
    assert_eq!(
        registry
            .warnings()
            .iter()
            .filter(|warning| warning.contains("shadowed by"))
            .count(),
        2
    );
}

#[test]
fn installed_skill_wins_over_agents_copy() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("ws");
    crate::test_support::trust_workspace(&workspace);
    write_skill(
        &workspace.join(".codewhale/skills"),
        "shared",
        "installed",
        "installed body",
    );
    write_skill(
        &workspace.join(".agents/skills"),
        "shared",
        "compat",
        "compat body",
    );
    let registry = super::discover_for_workspace_and_dir_with_home(
        &workspace,
        &workspace.join(".codewhale/skills"),
        None,
    );
    assert_eq!(registry.get("shared").unwrap().body, "installed body");
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains(".agents")
                && warning.contains(".codewhale")
                && warning.contains("shadowed by"))
    );
}

#[test]
fn flat_skills_root_opt_in() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("ws");
    crate::test_support::trust_workspace(&workspace);
    write_skill(
        &workspace.join("skills"),
        "flat",
        "product content",
        "flat body",
    );
    let catalog = super::roots::SkillRootCatalog::build(&workspace, None, None);
    assert!(
        catalog
            .audit_compatible_directories()
            .iter()
            .any(|root| root.path == workspace.join("skills") && !root.active_for_runtime)
    );
    let default = super::discover_for_workspace_and_dir_with_home_and_mode(
        &workspace,
        &workspace.join(".codewhale/skills"),
        None,
        super::SkillDiscoveryMode::Compatible,
    );
    assert!(default.get("flat").is_none());
    let config: crate::config::Config =
        toml::from_str("[skills]\nflat_workspace_root = true\n").unwrap();
    let mode = super::SkillDiscoveryMode::from_config(&config.skills_config());
    assert_eq!(mode, super::SkillDiscoveryMode::CompatibleWithFlatWorkspace);
    let enabled = super::discover_for_workspace_and_dir_with_home_and_mode(
        &workspace,
        &workspace.join(".codewhale/skills"),
        None,
        mode,
    );
    assert_eq!(enabled.get("flat").unwrap().body, "flat body");
    let owned_only: crate::config::Config =
        toml::from_str("[skills]\nflat_workspace_root = true\nscan_codewhale_only = true\n")
            .unwrap();
    assert_eq!(
        super::SkillDiscoveryMode::from_config(&owned_only.skills_config()),
        super::SkillDiscoveryMode::CodeWhaleOnly
    );
    let explicit = super::discover_for_workspace_and_dir_with_home_and_mode(
        &workspace,
        &workspace.join("skills"),
        None,
        super::SkillDiscoveryMode::Compatible,
    );
    assert!(
        explicit.get("flat").is_some(),
        "explicit skills_dir remains supported"
    );
}

#[test]
fn disable_model_invocation_maps_to_explicit_only() {
    for value in ["true", "yes", "on", "1"] {
        let skill = super::SkillRegistry::parse_skill(std::path::Path::new("demo/SKILL.md"), &format!("---\nname: demo\ndescription: routing\ndisable-model-invocation: {value}\n---\nbody")).unwrap();
        assert_eq!(skill.invocation, super::SkillInvocation::ExplicitOnly);
        assert!(!skill.invocation.model_invocable());
        assert!(skill.invocation.user_invocable());
    }
}

#[test]
fn user_invocable_false_hidden_from_slash_menu() {
    let content = "---\nname: demo\ndescription: routing\nuser-invocable: false\nargument-hint: '[query] [path]'\n---\nbody";
    let skill =
        super::SkillRegistry::parse_skill(std::path::Path::new("demo/SKILL.md"), content).unwrap();
    assert_eq!(skill.invocation, super::SkillInvocation::ModelOnly);
    assert!(skill.invocation.model_invocable());
    assert!(!skill.invocation.user_invocable());
    assert_eq!(skill.argument_hint.as_deref(), Some("[query] [path]"));
    assert!(skill.user_menu_description().contains("[query] [path]"));
    let disabled = super::SkillRegistry::parse_skill(
        std::path::Path::new("demo/SKILL.md"),
        &content.replace(
            "user-invocable: false",
            "user-invocable: false\ndisable-model-invocation: true",
        ),
    )
    .unwrap();
    assert_eq!(disabled.invocation, super::SkillInvocation::Disabled);
    assert!(!disabled.invocation.model_invocable());
    assert!(!disabled.invocation.user_invocable());
    let invalid = super::SkillRegistry::parse_skill(
        std::path::Path::new("demo/SKILL.md"),
        &content.replace(
            "user-invocable: false",
            "user-invocable: mystery\ndisable-model-invocation: mystery",
        ),
    )
    .unwrap();
    assert_eq!(invalid.invocation, super::SkillInvocation::Disabled);
}

#[test]
fn when_to_use_merged_into_trigger() {
    let content = "---\nname: demo\ndescription: Short summary.\nwhen_to_use: fixing a failing build\n---\nbody";
    let skill =
        super::SkillRegistry::parse_skill(std::path::Path::new("demo/SKILL.md"), content).unwrap();
    assert_eq!(
        skill.description,
        "Short summary. Use when: fixing a failing build"
    );
    let (summary, trigger) = super::split_trigger(&skill.description);
    assert_eq!(summary.trim(), "Short summary");
    assert!(trigger.unwrap().contains("fixing a failing build"));
}

#[test]
fn frontmatter_warnings_reach_registry_once() {
    let tmp = TempDir::new().unwrap();
    create_skill_dir(
        &tmp,
        "directory",
        "---\nname: different\nallowed-tools: [Read]\nmodel: example\ncontext: fork\nmystery: ignored\n---\nbody",
    );
    let registry = super::SkillRegistry::discover(&tmp.path().join("skills"));
    assert!(
        registry.get("different").is_some(),
        "runtime remains lenient"
    );
    assert_eq!(registry.warnings().len(), 6, "{:?}", registry.warnings());
    for key in [
        "allowed-tools",
        "model",
        "context",
        "mystery",
        "missing description",
        "differs from directory",
    ] {
        assert_eq!(
            registry
                .warnings()
                .iter()
                .filter(|warning| warning.contains(key))
                .count(),
            1
        );
    }
}

/// A repository must not make skill discovery read outside itself: a linked
/// skill directory or `SKILL.md` that leaves the workspace and a linked skills
/// root under the workspace are each refused with a warning, a linked skill
/// directory whose target stays inside the workspace is followed, and the same
/// layout under an operator-owned root (no workspace) follows links on purpose.
#[cfg(unix)]
#[test]
fn workspace_skill_discovery_does_not_follow_links_out_of_the_workspace() {
    use std::os::unix::fs::symlink;
    super::clear_skill_discovery_cache();
    let workspace = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write_skill(
        outside.path(),
        "smuggled-dir",
        "outside dir",
        "Outside body",
    );
    let linked_file_skill = outside.path().join("linked-file-source");
    write_skill(
        &linked_file_skill,
        "smuggled-file",
        "outside file",
        "Outside body",
    );

    let root = workspace.path().join(".agents").join("skills");
    write_skill(&root, "inside", "inside the workspace", "Inside body");
    symlink(outside.path().join("smuggled-dir"), root.join("linked-dir")).unwrap();
    write_skill(
        &workspace.path().join("docs").join("skills"),
        "linked-inside",
        "inside via link",
        "Inside body",
    );
    symlink(
        workspace
            .path()
            .join("docs")
            .join("skills")
            .join("linked-inside"),
        root.join("linked-inside"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("linked-file")).unwrap();
    symlink(
        linked_file_skill.join("smuggled-file").join("SKILL.md"),
        root.join("linked-file").join("SKILL.md"),
    )
    .unwrap();

    let registry = super::discover_from_directories_in_workspace(
        vec![root.clone()],
        Some(workspace.path()),
        None,
    );
    let names = registry
        .list()
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["inside", "linked-inside"],
        "{:?}",
        registry.warnings()
    );
    let warnings = registry.warnings().join("\n");
    assert!(
        warnings.contains("Refusing symlinked skill entry"),
        "{warnings}"
    );
    assert!(warnings.contains("linked-dir"), "{warnings}");
    assert!(!warnings.contains("linked-inside"), "{warnings}");
    assert!(warnings.contains("linked-file"), "{warnings}");

    // The same tree read as an operator-owned root still follows links.
    super::clear_skill_discovery_cache();
    let operator = super::discover_from_directories_with_plugins(vec![root.clone()], None);
    assert!(
        operator
            .list()
            .iter()
            .any(|skill| skill.name == "smuggled-dir"),
        "operator-owned roots keep following links"
    );

    // A linked skills root under the workspace is refused as a whole.
    super::clear_skill_discovery_cache();
    let other = TempDir::new().unwrap();
    std::fs::create_dir_all(other.path().join(".agents")).unwrap();
    symlink(outside.path(), other.path().join(".agents").join("skills")).unwrap();
    let registry = super::discover_from_directories_in_workspace(
        vec![other.path().join(".agents").join("skills")],
        Some(other.path()),
        None,
    );
    assert!(registry.list().is_empty());
    assert!(
        registry
            .warnings()
            .join("\n")
            .contains("Refusing symlinked path"),
        "{:?}",
        registry.warnings()
    );
}
