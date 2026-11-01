//! Provider-free contract tests for the bundled starter pack (#4698).
//!
//! Scope, stated precisely so these assertions are not over-read:
//!
//! * They validate **deterministic Codewhale behavior** — which skills install,
//!   which parse, which become ambient catalogue entries, which stay explicitly
//!   loadable, how aliases resolve, and how much prompt budget the catalogue
//!   costs.
//! * They validate **nothing about semantic routing**. Whether a model picks
//!   `debug` for a stack trace is a live-provider question and is deliberately
//!   out of scope here (see `docs/LIVE_SMOKE.md` for the opt-in harness).
//!
//! The expectation table lives in `assets/skills-catalog-matrix.json` and is
//! authored, not generated. The bijection assertion means a shipped-catalog
//! change cannot land without an explicit fixture update.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use tempfile::TempDir;

use super::system::{bundled_skill_generation, bundled_skill_names, install_system_skills};
use super::{
    BundledSkillTier, DEFAULT_SKILL_DESCRIPTION_CHARS, MAX_AVAILABLE_SKILLS_CHARS, Skill,
    SkillInvocation, SkillRegistry, bundled_skill_tier, render_skills_block,
};

const MATRIX_JSON: &str = include_str!("../assets/skills-catalog-matrix.json");

#[derive(Debug, serde::Deserialize)]
struct MatrixFixture {
    generation: String,
    skills: Vec<MatrixEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct MatrixEntry {
    name: String,
    tier: String,
    invocation: String,
    #[serde(default)]
    aliases: Vec<String>,
    in_model_catalogue: bool,
    #[serde(default)]
    shadowed_aliases: Vec<String>,
}

fn fixture() -> MatrixFixture {
    serde_json::from_str(MATRIX_JSON).expect("skills-catalog-matrix.json must be valid JSON")
}

/// Install the shipped pack into a temp dir and discover it, exactly as a
/// fresh user install would. No network, no provider, no ambient home.
fn installed_registry() -> (TempDir, SkillRegistry) {
    let tmp = TempDir::new().expect("temp dir");
    install_system_skills(tmp.path(), &Default::default()).expect("bundled install");
    let registry = SkillRegistry::discover(tmp.path());
    assert!(
        registry.warnings().is_empty(),
        "bundled pack must parse without warnings: {:?}",
        registry.warnings()
    );
    (tmp, registry)
}

fn rendered_catalogue(registry: &SkillRegistry, locale: &str, workspace: &Path) -> String {
    render_skills_block(registry, locale, workspace).expect("non-empty registry renders a block")
}

/// Canonical names that appear as `- <name>: …` entries under
/// `### Available skills` (and only there — the warnings section also uses
/// `- ` bullets).
fn catalogue_entry_names(block: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in block.lines() {
        if line.starts_with("###") {
            inside = line.trim() == "### Available skills";
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ")
            && let Some((name, _)) = rest.split_once(':')
        {
            names.push(name.trim().to_string());
        }
    }
    names
}

fn by_name(registry: &SkillRegistry) -> HashMap<&str, &Skill> {
    registry
        .list()
        .iter()
        .map(|skill| (skill.name.as_str(), skill))
        .collect()
}

// ── fixture ↔ bundle bijection ──────────────────────────────────────────────

#[test]
fn fixture_matrix_covers_exactly_the_shipped_bundle() {
    let fixture = fixture();
    let fixture_names: BTreeSet<&str> = fixture.skills.iter().map(|e| e.name.as_str()).collect();
    let shipped_names: BTreeSet<&str> = bundled_skill_names().into_iter().collect();

    assert_eq!(
        fixture_names, shipped_names,
        "assets/skills-catalog-matrix.json must be updated whenever the bundled \
         starter pack changes; left = fixture, right = BUNDLED_SKILLS"
    );
    assert_eq!(
        fixture.skills.len(),
        fixture_names.len(),
        "fixture must not list the same skill twice"
    );
    assert_eq!(
        fixture.generation,
        bundled_skill_generation(),
        "fixture generation must track BUNDLED_SKILL_VERSION"
    );
}

#[test]
fn fixture_matrix_matches_parsed_frontmatter_for_every_bundled_skill() {
    let (_tmp, registry) = installed_registry();
    let skills = by_name(&registry);

    for entry in fixture().skills {
        let skill = skills
            .get(entry.name.as_str())
            .unwrap_or_else(|| panic!("{} must install and parse", entry.name));

        let expected_invocation = match entry.invocation.as_str() {
            "explicit-only" => SkillInvocation::ExplicitOnly,
            "model+user" => SkillInvocation::ModelAndUser,
            other => panic!("{}: unknown fixture invocation {other}", entry.name),
        };
        assert_eq!(
            skill.invocation, expected_invocation,
            "{} invocation drifted from the fixture",
            entry.name
        );

        let actual_aliases: BTreeSet<&str> = skill.aliases.iter().map(String::as_str).collect();
        let expected_aliases: BTreeSet<&str> = entry.aliases.iter().map(String::as_str).collect();
        assert_eq!(
            actual_aliases, expected_aliases,
            "{} aliases drifted from the fixture",
            entry.name
        );

        let expected_tier = match entry.tier.as_str() {
            "core" => BundledSkillTier::CoreAgentic,
            "tools" => BundledSkillTier::FormatTooling,
            other => panic!("{}: unknown fixture tier {other}", entry.name),
        };
        assert_eq!(
            bundled_skill_tier(&entry.name),
            Some(expected_tier),
            "{} tier drifted from the fixture",
            entry.name
        );

        assert!(
            !skill.description.trim().is_empty(),
            "{} must ship a routing description",
            entry.name
        );
    }
}

// ── positive: eligibility and explicit load ─────────────────────────────────

#[test]
fn every_bundled_skill_is_explicitly_loadable_including_explicit_only() {
    let (_tmp, registry) = installed_registry();
    for name in bundled_skill_names() {
        let resolved = registry
            .get(name)
            .unwrap_or_else(|| panic!("{name} must resolve by canonical name"));
        assert_eq!(resolved.name, name);
    }
}

#[test]
fn ambient_catalogue_is_a_progressive_subset_of_eligible_bundled_skills() {
    let (tmp, registry) = installed_registry();
    let block = rendered_catalogue(&registry, "en", tmp.path());
    let rendered: BTreeSet<String> = catalogue_entry_names(&block).into_iter().collect();

    let expected: BTreeSet<String> = fixture()
        .skills
        .into_iter()
        .filter(|entry| entry.in_model_catalogue)
        .map(|entry| entry.name)
        .collect();

    assert!(
        !rendered.is_empty(),
        "the ambient routing page must not be empty"
    );
    assert!(
        rendered.is_subset(&expected),
        "the ambient page may contain only model+user bundled skills"
    );
    if rendered.len() < expected.len() {
        assert!(
            block.contains("`load_skill` with `name=\"list\""),
            "a truncated ambient page must point to complete discovery"
        );
    }
}

// ── negative: non-activation and explicit-only exclusion ────────────────────

#[test]
fn explicit_only_skills_are_absent_from_the_ambient_catalogue() {
    let (tmp, registry) = installed_registry();
    let block = rendered_catalogue(&registry, "en", tmp.path());

    for entry in fixture()
        .skills
        .into_iter()
        .filter(|e| !e.in_model_catalogue)
    {
        assert!(
            !block.contains(&format!("- {}: ", entry.name)),
            "{} is explicit-only and must not become ambient context",
            entry.name
        );
        assert!(
            registry.get(&entry.name).is_some(),
            "{} must still be loadable by explicit name",
            entry.name
        );
    }
}

#[test]
fn unrelated_and_unshipped_names_do_not_activate_a_bundled_skill() {
    let (_tmp, registry) = installed_registry();
    for name in [
        "",
        "   ",
        "imagine",
        "image-generation",
        "codereview",
        "checkwork",
        "gh-file-issue",
        "codew-release-qa-sweep",
        "totally-unknown-skill",
    ] {
        assert!(
            registry.get(name).is_none(),
            "{name:?} must not resolve to a bundled skill"
        );
    }

    // Lookup normalization is punctuation-insensitive by design, so an alias
    // typed with a space still lands on its canonical skill. Pin that so the
    // negative cases above stay meaningful rather than accidental.
    assert_eq!(
        registry.get("check work").map(|skill| skill.name.as_str()),
        Some("verify")
    );
}

// ── alias behavior ──────────────────────────────────────────────────────────

#[test]
fn aliases_resolve_to_their_canonical_skill_and_add_no_catalogue_entries() {
    let (tmp, registry) = installed_registry();
    let block = rendered_catalogue(&registry, "en", tmp.path());
    let rendered = catalogue_entry_names(&block);

    for entry in fixture().skills {
        for alias in &entry.aliases {
            let resolved = registry
                .get(alias)
                .unwrap_or_else(|| panic!("alias {alias} must resolve"));
            let expected = if entry.shadowed_aliases.contains(alias) {
                // A canonical bundled name always wins over another skill's
                // alias, so `docx` resolves to `docx`, never to `documents`.
                alias.as_str()
            } else {
                entry.name.as_str()
            };
            assert_eq!(
                resolved.name, expected,
                "alias {alias} resolved to the wrong canonical skill"
            );

            if !entry.shadowed_aliases.contains(alias) {
                assert!(
                    !rendered.iter().any(|name| name == alias),
                    "alias {alias} must not become a second catalogue entry"
                );
            }
        }
    }
}

#[test]
fn grok_compatibility_aliases_map_to_shipped_codewhale_workflows() {
    let (_tmp, registry) = installed_registry();
    for (alias, canonical) in [
        ("check-work", "verify"),
        ("code-review", "review"),
        ("create-skill", "skill-creator"),
    ] {
        let resolved = registry
            .get(alias)
            .unwrap_or_else(|| panic!("{alias} must resolve"));
        assert_eq!(resolved.name, canonical, "{alias} must map to {canonical}");
    }
}

#[test]
fn no_two_bundled_skills_claim_the_same_alias() {
    let mut owners: HashMap<String, String> = HashMap::new();
    for entry in fixture().skills {
        for alias in entry.aliases {
            if let Some(previous) = owners.insert(alias.clone(), entry.name.clone()) {
                panic!(
                    "alias {alias} is claimed by both {previous} and {}",
                    entry.name
                );
            }
        }
    }
}

// ── prompt-budget invariants ────────────────────────────────────────────────

#[test]
fn catalogue_has_unique_entries_and_the_complete_block_fits_the_prompt_budget() {
    let (tmp, registry) = installed_registry();
    let block = rendered_catalogue(&registry, "en", tmp.path());
    let rendered = catalogue_entry_names(&block);

    let unique: BTreeSet<&String> = rendered.iter().collect();
    assert_eq!(
        rendered.len(),
        unique.len(),
        "metadata must never duplicate a prompt entry: {rendered:?}"
    );

    assert!(
        block.chars().count() <= MAX_AVAILABLE_SKILLS_CHARS,
        "complete ambient block is {} chars, over the {MAX_AVAILABLE_SKILLS_CHARS} budget",
        block.chars().count()
    );
    if block.contains("additional skills omitted") {
        assert!(
            block.contains("`load_skill` with `name=\"list\""),
            "budget overflow must advertise complete on-demand discovery"
        );
    }
    // Budget headroom (docs/SKILLS.md): the shipped pack alone must render
    // every eligible skill with no omission line, so user skills are never
    // silently displaced by the bundle.
    assert!(
        !block.contains("additional skills omitted"),
        "shipped pack alone must not truncate the ambient catalogue"
    );

    // No entry may smuggle newlines or an oversized description into the
    // prompt prefix — that is how a catalogue line would poison context.
    for line in block.lines() {
        assert!(
            line.chars().count() <= DEFAULT_SKILL_DESCRIPTION_CHARS + 200,
            "catalogue line is too long for a routing hint: {line}"
        );
    }
    for skill in registry.list() {
        assert!(
            !skill.description.contains('\n'),
            "{} description must stay single-line",
            skill.name
        );
    }
}

// ── locale-aware routing metadata ───────────────────────────────────────────

#[test]
fn every_shipped_locale_falls_back_to_canonical_english_routing_descriptions() {
    // The bundled pack ships no `description_<tag>` frontmatter. Rather than
    // fabricate translations, the contract is an explicit, tested fallback:
    // every shipped locale sees the canonical English routing description.
    let (_tmp, registry) = installed_registry();
    for skill in registry.list() {
        assert!(
            skill.localized_descriptions.is_empty(),
            "{} ships localized routing metadata; add source-backed coverage \
             to this test instead of relying on the English-fallback contract",
            skill.name
        );
        for locale in codewhale_localization::Locale::shipped() {
            assert_eq!(
                skill.description_for_locale(locale.tag()),
                skill.description,
                "{} must fall back to canonical English for {}",
                skill.name,
                locale.tag()
            );
        }
    }
}

#[test]
fn rendered_catalogue_is_identical_across_every_shipped_locale() {
    let (tmp, registry) = installed_registry();
    let english = rendered_catalogue(&registry, "en", tmp.path());
    for locale in codewhale_localization::Locale::shipped() {
        let localized = rendered_catalogue(&registry, locale.tag(), tmp.path());
        assert_eq!(
            localized,
            english,
            "{} catalogue must match English parity while no localized \
             routing descriptions are shipped",
            locale.tag()
        );
    }
}

#[test]
fn locale_resolution_covers_exact_primary_tag_and_english_fallback() {
    // Synthetic fixture: exercises the three resolution paths the bundled pack
    // does not currently reach, for every shipped locale tag.
    let content = "---\n\
name: locale-probe\n\
description: English routing description\n\
description_ja: 日本語のルーティング説明\n\
description_pt: Descrição de roteamento\n\
description_zh-hant: 繁體路由說明\n\
---\n\n# body\n";
    let skill = SkillRegistry::parse_skill(Path::new("SKILL.md"), content).expect("parses");

    // Exact tag match (lowercased key lookup).
    assert_eq!(
        skill.description_for_locale("ja"),
        "日本語のルーティング説明"
    );
    assert_eq!(skill.description_for_locale("zh-Hant"), "繁體路由說明");
    // Primary-subtag fallback: pt-BR → description_pt.
    assert_eq!(
        skill.description_for_locale("pt-BR"),
        "Descrição de roteamento"
    );
    // English fallback for shipped locales with no authored variant.
    for tag in ["en", "ko", "vi", "es-419", "zh-Hans"] {
        assert_eq!(
            skill.description_for_locale(tag),
            "English routing description",
            "{tag} must fall back to the canonical description"
        );
    }

    // Every shipped locale tag must resolve to *some* non-empty description.
    for locale in codewhale_localization::Locale::shipped() {
        assert!(
            !skill.description_for_locale(locale.tag()).is_empty(),
            "{} must resolve to a non-empty routing description",
            locale.tag()
        );
    }
}

// ── starter-pack boundaries ─────────────────────────────────────────────────

#[test]
fn repository_maintenance_helpers_stay_out_of_the_end_user_starter_pack() {
    // These live in `docs/skills/` for maintainers and must never be installed
    // for end users (#4698 item 5). Plugin delivery is #4836, out of scope.
    for name in [
        "gh-file-issue",
        "gh-compile-issues",
        "gh-assign-issues",
        "gh-find-prs",
        "gh-treasure-hunt",
        "gh-close-issues",
        "gh-credit-harvest",
        "codew-release-qa-sweep",
    ] {
        assert!(
            !bundled_skill_names().contains(&name),
            "{name} is a maintainer helper and must not ship in the starter pack"
        );
    }
}

#[test]
fn starter_pack_does_not_advertise_capabilities_the_runtime_lacks() {
    // `imagine` stays out of scope while no image-generation/edit tool exists.
    let names = bundled_skill_names();
    for name in ["imagine", "image", "image-gen"] {
        assert!(
            !names.contains(&name),
            "{name} must not ship without a real image-generation tool"
        );
    }
}

#[test]
fn help_is_a_bounded_explicit_only_router() {
    let (_tmp, registry) = installed_registry();
    let help = registry.get("help").expect("help must be installed");
    assert_eq!(help.invocation, SkillInvocation::ExplicitOnly);
    // Bounded: a routing card, not an embedded manual.
    assert!(
        help.body.lines().count() < 80,
        "help must stay a router, not a manual: {} lines",
        help.body.lines().count()
    );
    for surface in ["/help", "/skills", "/config", "doctor"] {
        assert!(
            help.body.contains(surface),
            "help must route to the installed {surface} surface"
        );
    }
}
