use super::*;
use tempfile::TempDir;

fn skill_file(tmp: &TempDir, name: &str) -> std::path::PathBuf {
    tmp.path().join(name).join("SKILL.md")
}

fn skill_dir(tmp: &TempDir, name: &str) -> std::path::PathBuf {
    tmp.path().join(name)
}

fn marker_file(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".system-installed-version")
}

// ── fresh install ─────────────────────────────────────────────────────────

#[test]
fn fresh_install_creates_bundled_skills_and_marker() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();

    for skill in BUNDLED_SKILLS {
        assert!(
            skill_file(&tmp, skill.name).exists(),
            "{} SKILL.md should be created",
            skill.name
        );
    }
    assert!(marker_file(&tmp).exists(), "marker should be created");

    let ver = fs::read_to_string(marker_file(&tmp)).unwrap();
    assert_eq!(ver.trim(), BUNDLED_SKILL_VERSION);
}

#[test]
fn bundled_integration_skills_use_current_codewhale_commands_and_paths() {
    for (name, body) in [("mcp-builder", MCP_BUILDER_BODY), ("feishu", FEISHU_BODY)] {
        assert!(
            body.contains("codewhale mcp"),
            "{name} must use the current CLI"
        );
        assert!(
            !body.contains("deepseek mcp"),
            "{name} must not recommend the retired CLI name"
        );
    }
    assert!(SKILL_CREATOR_BODY.contains("<workspace>/.codewhale/skills"));
    assert!(SKILL_CREATOR_BODY.contains("~/.codewhale/skills"));
    assert!(SKILL_INSTALLER_BODY.contains("~/.codewhale/skills"));
    // Bundled skills must name model-visible tools. `read_file` is retired and
    // `File`/`Bash` are hidden compatibility names absent from new catalogs.
    assert!(PDF_BODY.contains("through `bash`"));
    assert!(HELP_BODY.contains("the `read` tool"));
    for (name, body) in [
        ("pdf", PDF_BODY),
        ("help", HELP_BODY),
        ("delegate", DELEGATE_BODY),
        ("best-of-n", BEST_OF_N_BODY),
    ] {
        assert!(
            !body.contains("read_file") && !body.contains("exec_shell"),
            "{name} must not teach a retired tool name"
        );
        assert!(
            !body.contains("`File`") && !body.contains("`Bash`"),
            "{name} must not teach the hidden File/Bash tools"
        );
    }
}

/// #4227 (requested by @JayBeest): the contributor sync/gate/digest skill
/// shipped in generations 8–11 and moved repo-local in generation 12. Its two
/// load-bearing refusals — never move a contributor's HEAD, never touch a
/// dirty tree — must survive any later edit to the retained body.
#[test]
fn contributor_onboarding_is_repo_local_and_keeps_its_refusals() {
    assert!(
        !is_bundled_skill_name("contributor-onboarding"),
        "contributor-onboarding must not ship to every user anymore"
    );

    let body = contributor_onboarding_body();
    assert!(body.contains("invocation: explicit-only"));
    // Read-only by default: sync is proposed, never performed.
    assert!(body.contains("Do not run `git fetch`, `git pull`, `git rebase`"));
    assert!(body.contains("Do not stash, discard, reset, or commit a dirty tree"));
    // The gate is quoted from CI rather than paraphrased, and the digest is
    // built from files rather than generated.
    assert!(body.contains("cargo clippy --workspace --all-features --locked"));
    assert!(body.contains(".github/workflows/ci.yml"));
    assert!(body.contains("Do not call a model provider"));
    // Provider neutrality: the dogfood step sends nothing anywhere.
    assert!(body.contains("./target/release/codewhale exec --help"));
    assert!(body.contains("Never select a provider for them"));
    // Contributor credit is part of the skill's own contract.
    assert!(body.contains("@JayBeest"));
}

/// A generation-11 install carries a bundled `contributor-onboarding` copy.
/// Generation 12 stops shipping it but never deletes by name alone: an
/// installed copy — shipped or user-edited — survives the upgrade, while a
/// fresh generation-12 install never creates one.
#[test]
fn upgrade_to_generation_12_leaves_contributor_onboarding_in_place() {
    let tmp = TempDir::new().unwrap();
    fs::write(marker_file(&tmp), "11").unwrap();
    let dir = skill_dir(&tmp, "contributor-onboarding");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), contributor_onboarding_body()).unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(skill_file(&tmp, "contributor-onboarding")).unwrap(),
        contributor_onboarding_body(),
        "upgrade must leave the installed copy untouched"
    );
    assert_eq!(
        fs::read_to_string(marker_file(&tmp)).unwrap().trim(),
        BUNDLED_SKILL_VERSION
    );

    let fresh = TempDir::new().unwrap();
    install_system_skills(fresh.path()).unwrap();
    assert!(
        !skill_file(&fresh, "contributor-onboarding").exists(),
        "fresh installs must not receive the repo-local skill"
    );
}

/// Generation 13 trims `social-media` and `health` from the bundle and moves
/// `feedback` repo-local. None of the three may ship to new installs; a
/// generation-12 `feedback` copy is left in place, never deleted by name.
#[test]
fn generation_13_trims_pack_and_feedback_goes_repo_local() {
    for name in ["social-media", "health", "feedback"] {
        assert!(
            !is_bundled_skill_name(name),
            "{name} must not ship in generation 13"
        );
    }

    let tmp = TempDir::new().unwrap();
    fs::write(marker_file(&tmp), "12").unwrap();
    let dir = skill_dir(&tmp, "feedback");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), feedback_body()).unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(skill_file(&tmp, "feedback")).unwrap(),
        feedback_body(),
        "upgrade must leave the installed feedback copy untouched"
    );

    let fresh = TempDir::new().unwrap();
    install_system_skills(fresh.path()).unwrap();
    for name in ["social-media", "health", "feedback"] {
        assert!(
            !skill_file(&fresh, name).exists(),
            "fresh installs must not receive {name}"
        );
    }
}

#[test]
fn fresh_install_skills_parse_for_discovery() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();

    let registry = crate::skills::SkillRegistry::discover(tmp.path());
    assert!(
        registry.warnings().is_empty(),
        "bundled skills should parse cleanly: {:?}",
        registry.warnings()
    );

    for skill in BUNDLED_SKILLS {
        let parsed = registry
            .get(skill.name)
            .unwrap_or_else(|| panic!("{} should be discoverable", skill.name));
        assert!(
            !parsed.description.is_empty(),
            "{} should include model-visible description",
            skill.name
        );
    }
}

#[test]
fn corrupt_marker_is_repaired_without_overwriting_user_skill_body() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();

    let user_body = "user-edited body";
    fs::write(skill_file(&tmp, "delegate"), user_body).unwrap();
    fs::remove_dir_all(skill_dir(&tmp, "skill-creator")).unwrap();
    fs::write(marker_file(&tmp), "not-a-version").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(skill_file(&tmp, "delegate")).unwrap(),
        user_body
    );
    assert!(skill_file(&tmp, "skill-creator").exists());
    assert_eq!(
        fs::read_to_string(marker_file(&tmp)).unwrap().trim(),
        BUNDLED_SKILL_VERSION
    );
}

#[test]
fn directory_marker_is_replaced_and_user_skills_are_preserved() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();

    let user_body = "user-edited body";
    fs::write(skill_file(&tmp, "delegate"), user_body).unwrap();
    fs::remove_dir_all(skill_dir(&tmp, "skill-creator")).unwrap();
    fs::remove_file(marker_file(&tmp)).unwrap();
    fs::create_dir(marker_file(&tmp)).unwrap();
    fs::write(marker_file(&tmp).join("stale-entry"), "stale").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(skill_file(&tmp, "delegate")).unwrap(),
        user_body
    );
    assert!(skill_file(&tmp, "skill-creator").exists());
    assert!(marker_file(&tmp).is_file());
    assert_eq!(
        fs::read_to_string(marker_file(&tmp)).unwrap().trim(),
        BUNDLED_SKILL_VERSION
    );
}

#[test]
fn invalid_marker_is_repaired_even_when_no_skill_body_changes() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();
    fs::write(marker_file(&tmp), "").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(marker_file(&tmp)).unwrap().trim(),
        BUNDLED_SKILL_VERSION
    );
}

#[test]
fn bundled_catalog_has_two_complete_truthful_tiers() {
    for skill in BUNDLED_SKILLS {
        assert!(
            bundled_skill_tier(skill.name).is_some(),
            "{} must have a picker tier",
            skill.name
        );
    }
    assert_eq!(
        bundled_skill_tier("best-of-n"),
        Some(BundledSkillTier::CoreAgentic)
    );
    assert_eq!(
        bundled_skill_tier("pdf"),
        Some(BundledSkillTier::FormatTooling)
    );
    assert_eq!(bundled_skill_tier("user-created"), None);
    assert!(
        !is_bundled_skill_name("imagine"),
        "do not advertise image generation without an image-generation tool"
    );
}

// ── idempotence ───────────────────────────────────────────────────────────

#[test]
fn calling_twice_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();

    for skill in BUNDLED_SKILLS {
        fs::write(
            skill_file(&tmp, skill.name),
            format!("{}-sentinel", skill.name),
        )
        .unwrap();
    }

    install_system_skills(tmp.path()).unwrap();

    for skill in BUNDLED_SKILLS {
        let body = fs::read_to_string(skill_file(&tmp, skill.name)).unwrap();
        assert_eq!(
            body,
            format!("{}-sentinel", skill.name),
            "second install should not overwrite {}",
            skill.name
        );
    }
}

// ── user deleted a directory ──────────────────────────────────────────────

#[test]
fn user_deleted_dir_is_not_recreated() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();

    // Simulate user deliberately removing one skill directory.
    fs::remove_dir_all(skill_dir(&tmp, "delegate")).unwrap();

    // Re-launch must NOT recreate the deleted directory.
    install_system_skills(tmp.path()).unwrap();

    assert!(
        !skill_file(&tmp, "delegate").exists(),
        "delegate must not be recreated after user deleted it"
    );
    assert!(
        skill_file(&tmp, "skill-creator").exists(),
        "skill-creator should still be present (not deleted by user)"
    );
}

#[test]
fn user_deleted_all_dirs_are_not_recreated() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();

    for skill in BUNDLED_SKILLS {
        fs::remove_dir_all(skill_dir(&tmp, skill.name)).unwrap();
    }

    install_system_skills(tmp.path()).unwrap();

    for skill in BUNDLED_SKILLS {
        assert!(
            !skill_file(&tmp, skill.name).exists(),
            "{} must not be recreated after user deletion",
            skill.name
        );
    }
}

// ── version bump re-installs ──────────────────────────────────────────────

#[test]
fn outdated_marker_triggers_reinstall_of_existing_skills() {
    let tmp = TempDir::new().unwrap();
    // Exact shipped bodies present with old marker: refresh is allowed and
    // newer skills are added. Non-matching user content is preserved
    // elsewhere (see upgrade_preserves_user_modified_bundled_skill_body).
    for skill in BUNDLED_SKILLS.iter().filter(|s| s.introduced_in <= 4) {
        fs::create_dir_all(skill_dir(&tmp, skill.name)).unwrap();
        fs::write(skill_file(&tmp, skill.name), skill.body).unwrap();
    }
    fs::write(marker_file(&tmp), "0").unwrap();

    install_system_skills(tmp.path()).unwrap();

    for skill in BUNDLED_SKILLS {
        assert!(
            skill_file(&tmp, skill.name).exists(),
            "{} should be installed after marker upgrade",
            skill.name
        );
        let content = fs::read_to_string(skill_file(&tmp, skill.name)).unwrap();
        assert_eq!(
            content, skill.body,
            "{} body should match shipped",
            skill.name
        );
    }
    let ver = fs::read_to_string(marker_file(&tmp)).unwrap();
    assert_eq!(ver.trim(), BUNDLED_SKILL_VERSION);
}

// ── partial previous install ─────────────────────────────────────────────

#[test]
fn version_bump_adds_skills_introduced_after_marker() {
    let tmp = TempDir::new().unwrap();
    // Pre-v5 install: only skills introduced through v4, with exact bodies.
    for skill in BUNDLED_SKILLS.iter().filter(|s| s.introduced_in <= 4) {
        fs::create_dir_all(skill_dir(&tmp, skill.name)).unwrap();
        fs::write(skill_file(&tmp, skill.name), skill.body).unwrap();
    }
    fs::write(marker_file(&tmp), "4").unwrap();

    install_system_skills(tmp.path()).unwrap();

    for skill in BUNDLED_SKILLS.iter().filter(|s| s.introduced_in == 5) {
        assert!(
            skill_file(&tmp, skill.name).exists(),
            "v5 skill {} should be installed on upgrade",
            skill.name
        );
    }
    // Unchanged exact bodies remain current.
    for skill in BUNDLED_SKILLS.iter().filter(|s| s.introduced_in <= 4) {
        let content = fs::read_to_string(skill_file(&tmp, skill.name)).unwrap();
        assert_eq!(content, skill.body);
    }
    let ver = fs::read_to_string(marker_file(&tmp)).unwrap();
    assert_eq!(ver.trim(), BUNDLED_SKILL_VERSION);
}

#[test]
fn version_bump_from_v8_adds_handoff_without_recreating_deleted_skills() {
    let tmp = TempDir::new().unwrap();
    fs::write(marker_file(&tmp), "8").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert!(skill_file(&tmp, "handoff").is_file());
    assert!(
        !skill_file(&tmp, "delegate").exists(),
        "an intentionally absent older skill must stay absent"
    );
    assert_eq!(
        fs::read_to_string(marker_file(&tmp)).unwrap().trim(),
        BUNDLED_SKILL_VERSION
    );
}

#[test]
fn version_bump_from_v5_adds_best_of_n_without_recreating_deleted_skills() {
    let tmp = TempDir::new().unwrap();
    fs::write(marker_file(&tmp), "5").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert!(skill_file(&tmp, "best-of-n").is_file());
    assert!(
        !skill_file(&tmp, "delegate").exists(),
        "an intentionally absent older skill must stay absent"
    );
    assert_eq!(
        fs::read_to_string(marker_file(&tmp)).unwrap().trim(),
        BUNDLED_SKILL_VERSION
    );
}

#[test]
fn version_bump_respects_deleted_existing_skill_while_adding_new_skill() {
    let tmp = TempDir::new().unwrap();

    // Simulate v2 where older bundled skills had been deliberately removed
    // before later versions introduced more system skills.
    fs::write(marker_file(&tmp), "2").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert!(
        !skill_file(&tmp, "skill-creator").exists(),
        "version bump should not recreate deleted skill-creator"
    );
    assert!(
        !skill_file(&tmp, "delegate").exists(),
        "version bump should not recreate deleted delegate"
    );
    for skill in BUNDLED_SKILLS
        .iter()
        .filter(|skill| skill.introduced_in > 2)
    {
        assert!(
            skill_file(&tmp, skill.name).exists(),
            "version bump should install newly introduced {}",
            skill.name
        );
    }
    let ver = fs::read_to_string(marker_file(&tmp)).unwrap();
    assert_eq!(ver.trim(), BUNDLED_SKILL_VERSION);
}

// ── upgrade ───────────────────────────────────────────────────────────────

#[test]
fn upgrade_from_v4_installs_pack_and_retires_unchanged_v4_best_practices() {
    let tmp = TempDir::new().unwrap();
    // Simulate a v4 install: marker + legacy skill bodies.
    fs::create_dir_all(skill_dir(&tmp, "v4-best-practices")).unwrap();
    fs::write(
        skill_file(&tmp, "v4-best-practices"),
        V4_BEST_PRACTICES_BODY,
    )
    .unwrap();
    fs::write(marker_file(&tmp), "4").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert!(
        !skill_dir(&tmp, "v4-best-practices").exists(),
        "unchanged v4-best-practices must be retired"
    );
    assert!(skill_file(&tmp, "debug").exists());
    assert!(skill_file(&tmp, "docx").exists());
    assert!(skill_file(&tmp, "release").exists());
    // Feishu is optional — not auto-installed by the default pack.
    assert!(
        !skill_dir(&tmp, "feishu").exists(),
        "feishu must not be universally installed"
    );
    let ver = fs::read_to_string(marker_file(&tmp)).unwrap();
    assert_eq!(ver.trim(), BUNDLED_SKILL_VERSION);
}

#[test]
fn upgrade_preserves_modified_v4_best_practices() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(skill_dir(&tmp, "v4-best-practices")).unwrap();
    fs::write(
        skill_file(&tmp, "v4-best-practices"),
        "---\nname: v4-best-practices\ndescription: user-owned\n---\n\n# mine\n",
    )
    .unwrap();
    fs::write(marker_file(&tmp), "4").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert!(skill_dir(&tmp, "v4-best-practices").exists());
    let body = fs::read_to_string(skill_file(&tmp, "v4-best-practices")).unwrap();
    assert!(
        body.contains("user-owned"),
        "modified body must be preserved"
    );
}

#[test]
fn upgrade_preserves_user_modified_bundled_skill_body() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();
    let path = skill_file(&tmp, "debug");
    fs::write(
        &path,
        "---\nname: debug\ndescription: customized\n---\n\n# custom\n",
    )
    .unwrap();
    // Force version bump attempt
    fs::write(marker_file(&tmp), "4").unwrap();
    install_system_skills(tmp.path()).unwrap();
    let body = fs::read_to_string(path).unwrap();
    assert!(
        body.contains("customized"),
        "user edit must not be overwritten by name alone"
    );
    assert_eq!(unrecognized_system_skills(tmp.path()), vec!["debug"]);
}

/// A generation-10 install carries the Registry-first `mcp-discovery` body.
/// The generation-11 rewrite is only real for existing users if that untouched
/// copy is actually replaced — the digest allowance is what makes the upgrade
/// reach them instead of stopping at "body differs, must be the user's".
#[test]
fn upgrade_from_generation_10_refreshes_untouched_mcp_discovery() {
    let tmp = TempDir::new().unwrap();
    let old = MCP_DISCOVERY_GENERATION_10_BODY;
    let path = skill_file(&tmp, "mcp-discovery");
    fs::create_dir_all(skill_dir(&tmp, "mcp-discovery")).unwrap();
    fs::write(&path, old).unwrap();
    fs::write(marker_file(&tmp), "10").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        MCP_DISCOVERY_BODY,
        "an unmodified generation-10 body must upgrade to the shipped body"
    );
    assert_eq!(
        fs::read_to_string(marker_file(&tmp)).unwrap().trim(),
        BUNDLED_SKILL_VERSION
    );
}

#[test]
fn upgrade_from_generation_10_preserves_user_edited_mcp_discovery() {
    let tmp = TempDir::new().unwrap();
    let edited = format!("{MCP_DISCOVERY_GENERATION_10_BODY}\n- my own house rule\n");
    let path = skill_file(&tmp, "mcp-discovery");
    fs::create_dir_all(skill_dir(&tmp, "mcp-discovery")).unwrap();
    fs::write(&path, &edited).unwrap();
    fs::write(marker_file(&tmp), "10").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        edited,
        "an edited copy stays the user's, even one derived from a shipped body"
    );
}

#[test]
fn upgrade_preserves_an_intentionally_empty_mcp_discovery() {
    let tmp = TempDir::new().unwrap();
    let path = skill_file(&tmp, "mcp-discovery");
    fs::create_dir_all(skill_dir(&tmp, "mcp-discovery")).unwrap();
    fs::write(&path, "").unwrap();
    fs::write(marker_file(&tmp), "10").unwrap();
    install_system_skills(tmp.path()).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "");
}

#[test]
fn mcp_discovery_deleted_at_generation_10_stays_deleted() {
    let tmp = TempDir::new().unwrap();
    fs::write(marker_file(&tmp), "10").unwrap();

    install_system_skills(tmp.path()).unwrap();

    assert!(
        !skill_file(&tmp, "mcp-discovery").exists(),
        "the digest allowance must not resurrect a skill the user removed"
    );
}

/// The retained body is evidence, not decoration: it must be the superseded
/// text (Registry-first, `registry_sync {}`, always-present tools), never a
/// stale copy of the current one, or the allowance would silently do nothing.
#[test]
fn retained_generation_10_body_is_the_superseded_one() {
    let old = MCP_DISCOVERY_GENERATION_10_BODY;
    assert_ne!(old, MCP_DISCOVERY_BODY);
    assert!(is_superseded_shipped_body("mcp-discovery", old));
    assert!(!is_superseded_shipped_body(
        "mcp-discovery",
        MCP_DISCOVERY_BODY
    ));
    assert!(!is_superseded_shipped_body("debug", old));
    // The stale generation-10 facts the rewrite exists to remove.
    assert!(old.contains("registry_sync {}"));
    assert!(old.contains("available in the active tool"));
    assert!(MCP_DISCOVERY_BODY.contains("registry_sync {query:"));
}

#[test]
fn end_user_pack_skills_parse_for_discovery() {
    let tmp = TempDir::new().unwrap();
    install_system_skills(tmp.path()).unwrap();
    let registry = crate::skills::SkillRegistry::discover(tmp.path());
    assert!(
        registry.warnings().is_empty(),
        "bundled skills should parse cleanly: {:?}",
        registry.warnings()
    );
    for name in [
        "debug", "test", "review", "document", "docx", "release", "plan", "verify",
    ] {
        assert!(registry.get(name).is_some(), "{name} must be discoverable");
    }
}

#[test]
fn procedural_skill_homes_remain_bundled_and_lazy() {
    for name in ["debug", "best-of-n", "simplify", "verify", "test", "review"] {
        assert!(
            is_bundled_skill_name(name),
            "procedural skill home must remain available on demand: {name}"
        );
    }
}

#[test]
fn generation_14_refreshes_known_bodies_and_preserves_customizations_and_deletions() {
    for line in SHIPPED_BODY_HASHES.lines() {
        let (name, digest) = line.split_once(' ').expect("name and digest");
        assert!(is_bundled_skill_name(name), "{name} is not bundled");
        assert_eq!(digest.len(), 64, "{name}");
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
    }
    for (name, old) in SUPERSEDED_BODIES
        .iter()
        .filter(|(name, _)| *name != "mcp-discovery")
    {
        let skill = BUNDLED_SKILLS
            .iter()
            .find(|skill| skill.name == *name)
            .unwrap();
        assert_ne!(*old, skill.body);
        assert!(hash_is_listed(SHIPPED_BODY_HASHES, name, old), "{name}");
        assert!(!hash_is_listed(
            SHIPPED_BODY_HASHES,
            name,
            &format!("{old}\nMy instructions.\n")
        ));
        for customized in [false, true] {
            let tmp = TempDir::new().unwrap();
            let file = skill_file(&tmp, name);
            fs::create_dir_all(skill_dir(&tmp, name)).unwrap();
            let body = if customized {
                format!("{old}\nMy instructions.\n")
            } else {
                old.to_string()
            };
            fs::write(&file, &body).unwrap();
            fs::write(marker_file(&tmp), "13").unwrap();
            install_system_skills(tmp.path()).unwrap();
            assert_eq!(
                fs::read_to_string(file).unwrap(),
                if customized {
                    body
                } else {
                    skill.body.to_string()
                },
                "{name}"
            );
        }
        let tmp = TempDir::new().unwrap();
        fs::write(marker_file(&tmp), "13").unwrap();
        install_system_skills(tmp.path()).unwrap();
        assert!(!skill_file(&tmp, name).exists(), "{name} must stay deleted");
    }
}

#[test]
fn generation_15_refreshes_help_and_pdf_from_generation_14() {
    for name in ["help", "pdf"] {
        let old = SUPERSEDED_BODIES
            .iter()
            .find(|(entry, _)| *entry == name)
            .map(|(_, body)| *body)
            .expect("generation-14 body retained");
        assert!(
            old.contains("`File`"),
            "{name} retained body is the old one"
        );
        let skill = BUNDLED_SKILLS
            .iter()
            .find(|skill| skill.name == name)
            .unwrap();
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(skill_dir(&tmp, name)).unwrap();
        fs::write(skill_file(&tmp, name), old).unwrap();
        fs::write(marker_file(&tmp), "14").unwrap();
        install_system_skills(tmp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(skill_file(&tmp, name)).unwrap(),
            skill.body,
            "{name}"
        );
    }
}

#[test]
fn generation_17_refreshes_previous_plugin_creator_bodies() {
    let skill = BUNDLED_SKILLS
        .iter()
        .find(|skill| skill.name == "plugin-creator")
        .unwrap();
    assert!(skill.body.contains("Create `plugin.json`"));
    assert!(skill.body.contains("## Experimental host code"));
    for (generation, old) in [
        (
            "15",
            include_str!("../../../assets/skills/plugin-creator/SKILL.generation-15.md"),
        ),
        (
            "16",
            include_str!("../../../assets/skills/plugin-creator/SKILL.generation-16.md"),
        ),
    ] {
        assert!(is_superseded_shipped_body("plugin-creator", old));
        assert!(!old.contains("## Experimental host code"));
        for customized in [false, true] {
            let tmp = TempDir::new().unwrap();
            fs::create_dir_all(skill_dir(&tmp, "plugin-creator")).unwrap();
            let body = if customized {
                format!("{old}\nMy instructions.\n")
            } else {
                old.to_string()
            };
            fs::write(skill_file(&tmp, "plugin-creator"), &body).unwrap();
            fs::write(marker_file(&tmp), generation).unwrap();
            install_system_skills(tmp.path()).unwrap();
            assert_eq!(
                fs::read_to_string(skill_file(&tmp, "plugin-creator")).unwrap(),
                if customized {
                    body
                } else {
                    skill.body.to_string()
                },
            );
        }
    }
}
