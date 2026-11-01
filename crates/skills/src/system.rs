//! System-skill installer: bundles first-party skills and auto-installs them
//! on first launch.

use std::fs;
use std::path::Path;

use codewhale_runtime::config::{BundledSkillsConfig, BundledSkillsInstall, SkillsConfig};
use codewhale_skill_install::skill_parse::{self, normalize_skill_name_for_lookup};
use sha2::{Digest, Sha256};

/// Bundled catalog generation for the default CodeWhale skill pack (#4691).
///
/// Generation 7 adds the explicit-only `help` router (#4698 parity slice).
/// Generation 8 adds the explicit-only `contributor-onboarding` path
/// requested by @JayBeest (#4227).
/// Generation 9 adds the `handoff` workflow skill (baton-pass for
/// continuous operate-mode operations).
/// Generation 10 adds the bundled `mcp-discovery` skill (Registry-first
/// tool selection).
/// Generation 11 rewrites `mcp-discovery` from a Registry-first gate into a
/// missing-capability fallback, and corrects two stale facts in its body:
/// `registry_sync` requires a `query`, and the discovery tools are deferred
/// rather than always present in the active surface. Because the rewrite
/// changes an already-installed body, generation 10's exact body is retained in
/// `SUPERSEDED_BODIES` so unmodified copies upgrade and edited copies do not.
/// Generation 12 adds the everyday pack (comms, money, audio, health,
/// shopping/travel, media, github, goals, forget, feedback) and demotes
/// `contributor-onboarding` to a repo-local project skill: existing installed
/// copies are left in place, new installs do not receive it.
/// Generation 13 trims the pack: `social-media` and `health` ship to nobody
/// (phone-export workflows, not everyday), and `feedback` joins
/// `contributor-onboarding` as a repo-local project skill.
/// Generation 14 corrects account setup, Photos export, forgetting and plugin
/// lifecycle guidance; exact generation-13 bodies allow safe upgrades.
/// Generation 15 points `help` and `pdf` at the model-visible `read`/`bash`
/// tools instead of the hidden compatibility `File` tool; exact
/// generation-14 bodies allow safe upgrades.
/// Generation 16 has `plugin-creator` scaffold `plugin.json` and describe
/// `native` entries under the experimental extension host; the exact
/// generation-15 body allows a safe upgrade.
/// Generation 17 adds the tested native author loop and keeps trust with the
/// person; the exact generation-16 body allows a safe upgrade.
/// Generation 18 recognizes previously shipped bodies by SHA-256 as well as by
/// retained bytes, so an untouched copy from any earlier generation upgrades.
const BUNDLED_SKILL_VERSION: &str = "19";

// ── system & extension (meta) ───────────────────────────────────────────────
const SKILL_CREATOR_BODY: &str = include_str!("../assets/skills/skill-creator/SKILL.md");
const DELEGATE_BODY: &str = include_str!("../assets/skills/delegate/SKILL.md");
const PLUGIN_CREATOR_BODY: &str = include_str!("../assets/skills/plugin-creator/SKILL.md");
const SKILL_INSTALLER_BODY: &str = include_str!("../assets/skills/skill-installer/SKILL.md");
const MCP_BUILDER_BODY: &str = include_str!("../assets/skills/mcp-builder/SKILL.md");
const FLEET_MANAGER_BODY: &str = include_str!("../assets/skills/fleet-manager/SKILL.md");
const HELP_BODY: &str = include_str!("../assets/skills/help/SKILL.md");

// ── end-user workflows ──────────────────────────────────────────────────────
const HANDOFF_BODY: &str = include_str!("../assets/skills/handoff/SKILL.md");
const BEST_OF_N_BODY: &str = include_str!("../assets/skills/best-of-n/SKILL.md");
const INTERVIEW_BODY: &str = include_str!("../assets/skills/interview/SKILL.md");
const PLAN_BODY: &str = include_str!("../assets/skills/plan/SKILL.md");
const IMPLEMENT_BODY: &str = include_str!("../assets/skills/implement/SKILL.md");
const DEBUG_BODY: &str = include_str!("../assets/skills/debug/SKILL.md");
const TEST_BODY: &str = include_str!("../assets/skills/test/SKILL.md");
const REVIEW_BODY: &str = include_str!("../assets/skills/review/SKILL.md");
const SECURITY_REVIEW_BODY: &str = include_str!("../assets/skills/security-review/SKILL.md");
const SIMPLIFY_BODY: &str = include_str!("../assets/skills/simplify/SKILL.md");
const VERIFY_BODY: &str = include_str!("../assets/skills/verify/SKILL.md");
const RESEARCH_BODY: &str = include_str!("../assets/skills/research/SKILL.md");
const FRONTEND_DESIGN_BODY: &str = include_str!("../assets/skills/frontend-design/SKILL.md");
const WEBAPP_TESTING_BODY: &str = include_str!("../assets/skills/webapp-testing/SKILL.md");
const DOCUMENT_BODY: &str = include_str!("../assets/skills/document/SKILL.md");
const DATAVIZ_BODY: &str = include_str!("../assets/skills/dataviz/SKILL.md");
const DOCX_BODY: &str = include_str!("../assets/skills/docx/SKILL.md");
const PDF_BODY: &str = include_str!("../assets/skills/pdf/SKILL.md");
const PPTX_BODY: &str = include_str!("../assets/skills/pptx/SKILL.md");
const XLSX_BODY: &str = include_str!("../assets/skills/xlsx/SKILL.md");
const DOCUMENTS_ALIAS_BODY: &str = include_str!("../assets/skills/documents/SKILL.md");
const PRESENTATIONS_ALIAS_BODY: &str = include_str!("../assets/skills/presentations/SKILL.md");
const SPREADSHEETS_ALIAS_BODY: &str = include_str!("../assets/skills/spreadsheets/SKILL.md");

// ── everyday ────────────────────────────────────────────────────────────────
const GITHUB_BODY: &str = include_str!("../assets/skills/github/SKILL.md");
const GMAIL_BODY: &str = include_str!("../assets/skills/gmail/SKILL.md");
const GOOGLE_CALENDAR_BODY: &str = include_str!("../assets/skills/google-calendar/SKILL.md");
const MONEY_BODY: &str = include_str!("../assets/skills/money/SKILL.md");
const SPOTIFY_BODY: &str = include_str!("../assets/skills/spotify/SKILL.md");
const TTS_BODY: &str = include_str!("../assets/skills/tts/SKILL.md");
const PODCAST_BODY: &str = include_str!("../assets/skills/podcast/SKILL.md");
const SHOPPING_BODY: &str = include_str!("../assets/skills/shopping/SKILL.md");
const FLIGHTS_BODY: &str = include_str!("../assets/skills/flights/SKILL.md");
const PHOTOS_BODY: &str = include_str!("../assets/skills/photos/SKILL.md");
const IMAGE_SEARCH_BODY: &str = include_str!("../assets/skills/image-search/SKILL.md");
const GOALS_BODY: &str = include_str!("../assets/skills/goals/SKILL.md");
const FORGET_BODY: &str = include_str!("../assets/skills/forget/SKILL.md");
const FEEDBACK_BODY: &str = include_str!("../assets/skills/feedback/SKILL.md");

// ── power / explicit-only ───────────────────────────────────────────────────
const BATCH_BODY: &str = include_str!("../assets/skills/batch/SKILL.md");
const DEPENDENCY_UPDATE_BODY: &str = include_str!("../assets/skills/dependency-update/SKILL.md");
const RELEASE_BODY: &str = include_str!("../assets/skills/release/SKILL.md");
const CONTRIBUTOR_ONBOARDING_BODY: &str =
    include_str!("../assets/skills/contributor-onboarding/SKILL.md");

// Optional integration (not auto-installed for every user): Feishu body kept for
// digest/migration helpers only.
const FEISHU_BODY: &str = include_str!("../assets/skills/feishu/SKILL.md");
const MCP_DISCOVERY_BODY: &str = include_str!("../assets/skills/mcp-discovery/SKILL.md");

// Legacy v4 body retained for migration fixtures; installed copies stay on disk.
#[cfg(test)]
const V4_BEST_PRACTICES_BODY: &str = include_str!("../assets/skills/v4-best-practices/SKILL.md");

// Generation-10 `mcp-discovery` body, retained solely so an unmodified copy of
// it can be recognized and refreshed on upgrade. Only exact known bodies
// without a managed marker can be replaced; user edits remain in place.
const MCP_DISCOVERY_GENERATION_10_BODY: &str =
    include_str!("../assets/skills/mcp-discovery/SKILL.generation-10.md");

/// Exact bodies a bundled skill shipped in an earlier generation.
///
/// The installer refuses to overwrite an installed body it does not recognize,
/// which is what protects user edits. Without this table that same rule also
/// pins every unmodified older copy forever: the on-disk body no longer equals
/// the shipped one, so the skill never upgrades. Listing the previous body
/// restores the upgrade for exactly the copies CodeWhale wrote itself.
const SUPERSEDED_BODIES: &[(&str, &str)] = &[
    ("mcp-discovery", MCP_DISCOVERY_GENERATION_10_BODY),
    (
        "gmail",
        include_str!("../assets/skills/gmail/SKILL.generation-13.md"),
    ),
    (
        "google-calendar",
        include_str!("../assets/skills/google-calendar/SKILL.generation-13.md"),
    ),
    (
        "photos",
        include_str!("../assets/skills/photos/SKILL.generation-13.md"),
    ),
    (
        "forget",
        include_str!("../assets/skills/forget/SKILL.generation-13.md"),
    ),
    (
        "spotify",
        include_str!("../assets/skills/spotify/SKILL.generation-13.md"),
    ),
    (
        "plugin-creator",
        include_str!("../assets/skills/plugin-creator/SKILL.generation-13.md"),
    ),
    (
        "plugin-creator",
        include_str!("../assets/skills/plugin-creator/SKILL.generation-15.md"),
    ),
    (
        "plugin-creator",
        include_str!("../assets/skills/plugin-creator/SKILL.generation-16.md"),
    ),
    (
        "help",
        include_str!("../assets/skills/help/SKILL.generation-14.md"),
    ),
    (
        "pdf",
        include_str!("../assets/skills/pdf/SKILL.generation-14.md"),
    ),
];

/// `name sha256` lines for every body a bundled skill carried in canonical
/// history, plus bodies found installed in the field that no ref contains.
const SHIPPED_BODY_HASHES: &str = include_str!("system/shipped_hashes.txt");

fn body_sha256(body: &str) -> String {
    Sha256::digest(body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hash_is_listed(table: &str, name: &str, body: &str) -> bool {
    let digest = body_sha256(body);
    table
        .lines()
        .filter_map(|line| line.split_once(' '))
        .any(|(skill, listed)| skill == name && listed == digest)
}

/// Whether `existing` is a body CodeWhale previously shipped for `name` (and
/// therefore safe to replace on upgrade): retained bytes or a listed hash.
fn is_superseded_shipped_body(name: &str, existing: &str) -> bool {
    SUPERSEDED_BODIES
        .iter()
        .any(|(skill, body)| *skill == name && *body == existing)
        || hash_is_listed(SHIPPED_BODY_HASHES, name, existing)
}

/// Bundled skills whose installed body is neither the current one nor any
/// shipped one: customized by the user or from a generation this build cannot
/// identify. The installer never refreshes these.
#[must_use]
pub fn unrecognized_system_skills(
    skills_dir: &Path,
    policy: &BundledSkillsConfig,
) -> Vec<&'static str> {
    BUNDLED_SKILLS
        .iter()
        .filter(|skill| bundled_enabled(policy, skill.name))
        .filter(|skill| {
            fs::symlink_metadata(skills_dir.join(skill.name).join(".installed-from")).is_err()
        })
        .filter(|skill| {
            codewhale_util::fs_confined::read_to_string(
                skills_dir,
                &skills_dir.join(skill.name).join("SKILL.md"),
            )
            .is_ok_and(|existing| {
                existing != skill.body && !is_superseded_shipped_body(skill.name, &existing)
            })
        })
        .map(|skill| skill.name)
        .collect()
}

struct BundledSkill {
    name: &'static str,
    body: &'static str,
    introduced_in: u32,
}

/// Skills auto-installed for every user on fresh install / upgrade.
const BUNDLED_SKILLS: &[BundledSkill] = &[
    // System & extension
    BundledSkill {
        name: "skill-creator",
        body: SKILL_CREATOR_BODY,
        introduced_in: 1,
    },
    BundledSkill {
        name: "delegate",
        body: DELEGATE_BODY,
        introduced_in: 2,
    },
    BundledSkill {
        name: "plugin-creator",
        body: PLUGIN_CREATOR_BODY,
        introduced_in: 3,
    },
    BundledSkill {
        name: "skill-installer",
        body: SKILL_INSTALLER_BODY,
        introduced_in: 3,
    },
    BundledSkill {
        name: "mcp-builder",
        body: MCP_BUILDER_BODY,
        introduced_in: 3,
    },
    BundledSkill {
        name: "fleet-manager",
        body: FLEET_MANAGER_BODY,
        introduced_in: 4,
    },
    BundledSkill {
        name: "help",
        body: HELP_BODY,
        introduced_in: 7,
    },
    // End-user workflows
    BundledSkill {
        name: "handoff",
        body: HANDOFF_BODY,
        introduced_in: 9,
    },
    BundledSkill {
        name: "best-of-n",
        body: BEST_OF_N_BODY,
        introduced_in: 6,
    },
    BundledSkill {
        name: "interview",
        body: INTERVIEW_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "plan",
        body: PLAN_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "implement",
        body: IMPLEMENT_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "debug",
        body: DEBUG_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "test",
        body: TEST_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "review",
        body: REVIEW_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "security-review",
        body: SECURITY_REVIEW_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "simplify",
        body: SIMPLIFY_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "verify",
        body: VERIFY_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "research",
        body: RESEARCH_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "frontend-design",
        body: FRONTEND_DESIGN_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "webapp-testing",
        body: WEBAPP_TESTING_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "document",
        body: DOCUMENT_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "dataviz",
        body: DATAVIZ_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "docx",
        body: DOCX_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "pdf",
        body: PDF_BODY,
        introduced_in: 3,
    },
    BundledSkill {
        name: "pptx",
        body: PPTX_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "xlsx",
        body: XLSX_BODY,
        introduced_in: 5,
    },
    // Compatibility aliases for pre-v5 artifact names
    BundledSkill {
        name: "documents",
        body: DOCUMENTS_ALIAS_BODY,
        introduced_in: 3,
    },
    BundledSkill {
        name: "presentations",
        body: PRESENTATIONS_ALIAS_BODY,
        introduced_in: 3,
    },
    BundledSkill {
        name: "spreadsheets",
        body: SPREADSHEETS_ALIAS_BODY,
        introduced_in: 3,
    },
    // Power / explicit-only
    BundledSkill {
        name: "batch",
        body: BATCH_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "dependency-update",
        body: DEPENDENCY_UPDATE_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "release",
        body: RELEASE_BODY,
        introduced_in: 5,
    },
    BundledSkill {
        name: "mcp-discovery",
        body: MCP_DISCOVERY_BODY,
        introduced_in: 10,
    },
    // Everyday (generation 12)
    BundledSkill {
        name: "github",
        body: GITHUB_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "gmail",
        body: GMAIL_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "google-calendar",
        body: GOOGLE_CALENDAR_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "money",
        body: MONEY_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "spotify",
        body: SPOTIFY_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "tts",
        body: TTS_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "podcast",
        body: PODCAST_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "shopping",
        body: SHOPPING_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "flights",
        body: FLIGHTS_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "photos",
        body: PHOTOS_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "image-search",
        body: IMAGE_SEARCH_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "goals",
        body: GOALS_BODY,
        introduced_in: 12,
    },
    BundledSkill {
        name: "forget",
        body: FORGET_BODY,
        introduced_in: 12,
    },
];

/// Product-facing grouping for the bundled catalog.
///
/// User and compatible skills remain outside these two buckets. The grouping
/// is deliberately attached to the shipped catalog instead of inferred from
/// arbitrary community metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BundledSkillTier {
    CoreAgentic,
    FormatTooling,
}

impl BundledSkillTier {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CoreAgentic => "core",
            Self::FormatTooling => "tools",
        }
    }
}

/// Return the curated tier for a bundled skill name.
#[must_use]
pub fn bundled_skill_tier(name: &str) -> Option<BundledSkillTier> {
    if !is_bundled_skill_name(name) {
        return None;
    }
    let tier = match name {
        "skill-creator" | "plugin-creator" | "skill-installer" | "mcp-builder" | "help"
        | "frontend-design" | "webapp-testing" | "document" | "dataviz" | "docx" | "pdf"
        | "pptx" | "xlsx" | "documents" | "presentations" | "spreadsheets" => {
            BundledSkillTier::FormatTooling
        }
        _ => BundledSkillTier::CoreAgentic,
    };
    Some(tier)
}

/// Canonical names of every skill in the shipped starter pack, in bundle order.
///
/// Exposed so the catalog fixture matrix (#4698) can assert a *bijection*
/// between the checked-in fixture and the real bundle: a skill added or removed
/// without updating the fixture fails the build rather than silently changing
/// what every user gets installed.
#[must_use]
#[cfg(test)]
pub fn bundled_skill_names() -> Vec<&'static str> {
    BUNDLED_SKILLS.iter().map(|skill| skill.name).collect()
}

/// The shipped generation marker written to `.system-installed-version`.
#[must_use]
#[cfg(test)]
pub fn bundled_skill_generation() -> &'static str {
    BUNDLED_SKILL_VERSION
}

fn feishu_body() -> &'static str {
    FEISHU_BODY
}

/// Last shipped `contributor-onboarding` body (removed from the bundle in
/// generation 12; now a repo-local project skill). Retained so tests can pin
/// its load-bearing refusals and so an installed copy stays recognizable.
fn contributor_onboarding_body() -> &'static str {
    CONTRIBUTOR_ONBOARDING_BODY
}

/// Last shipped `feedback` body (removed from the bundle in generation 13;
/// now a repo-local project skill). Retained so an installed copy stays
/// recognizable and is left in place, never deleted by name.
fn feedback_body() -> &'static str {
    FEEDBACK_BODY
}

/// Whether a skill name matches one of the bundled first-party skills.
///
/// Used by `/skills` to distinguish user-created skills (which should be
/// surfaced prominently) from the always-installed bundle (which can be
/// rendered compactly when many skills are present).
///
/// Prefer [`is_exact_bundled_skill`] when classifying audit rows — name-only
/// matches can collide with user overrides of the same command name.
#[must_use]
pub fn is_bundled_skill_name(name: &str) -> bool {
    BUNDLED_SKILLS.iter().any(|s| s.name == name)
}

/// True when `name` is a bundled skill **and** `skill_md_content` exactly
/// matches the shipped asset body (byte-for-byte).
///
/// Used by the skill audit inventory so a user-edited copy of a bundled name
/// is not misclassified as built-in.
#[must_use]
pub fn is_exact_bundled_skill(name: &str, skill_md_content: &str) -> bool {
    BUNDLED_SKILLS
        .iter()
        .any(|s| s.name == name && s.body == skill_md_content)
}

/// Attempt to install a single bundled skill into `skills_dir`.
///
/// Returns `true` if installation occurred (fresh install or version bump).
fn install_one(
    skills_dir: &Path,
    skill: &BundledSkill,
    installed_version: Option<&str>,
    policy: &BundledSkillsConfig,
) -> std::io::Result<bool> {
    if !bundled_enabled(policy, skill.name) {
        return Ok(false);
    }
    let target_dir = skills_dir.join(skill.name);
    let target_file = target_dir.join("SKILL.md");
    let dir_exists = target_dir.exists();
    let installed_number = installed_version.and_then(|value| value.parse::<u32>().ok());

    let should_install = policy.install == Some(BundledSkillsInstall::All)
        || match (installed_version, installed_number, dir_exists) {
            // Fresh install: neither marker nor directory.
            (None, _, false) => true,
            // Newly bundled skill: add it for older system-skill installs.
            (Some(_), Some(version), _) if version < skill.introduced_in => true,
            // Version bump for an existing skill: refresh only if the user has not
            // intentionally deleted that skill directory.
            (Some(version), _, true) if version != BUNDLED_SKILL_VERSION => true,
            // Every other case: current install, user-deleted dir, or pre-existing
            // user-owned skill without our marker.
            _ => false,
        };

    if should_install {
        if fs::symlink_metadata(target_dir.join(".installed-from")).is_ok() {
            return Ok(false);
        }
        match codewhale_util::fs_confined::read_to_string(skills_dir, &target_file) {
            Ok(existing)
                if existing != skill.body && !is_superseded_shipped_body(skill.name, &existing) =>
            {
                return Ok(false);
            }
            Ok(existing) if existing == skill.body => {
                return Ok(installed_version != Some(BUNDLED_SKILL_VERSION));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::create_dir_all(skills_dir)?;
        codewhale_util::files::WorkspaceFile::open(
            skills_dir,
            &Path::new(skill.name).join("SKILL.md"),
            true,
        )?
        .replace(skill.body.as_bytes())?;
    }
    Ok(should_install)
}

/// Install bundled system skills into `skills_dir`.
///
/// Behaviour:
/// - Fresh install (no marker, no dir): installs every bundled skill, then
///   writes the version marker.
/// - Version bump (marker present with older version): re-installs any existing
///   bundled skill whose body is still one CodeWhale shipped (current or a
///   `SUPERSEDED_BODIES` entry) and installs newly introduced bundled skills.
///   A user-edited body is never replaced.
/// - User deleted a skill dir while marker still present at same version: leaves
///   it gone.
/// - Idempotent: calling twice with no changes is a no-op.
///
/// Errors are I/O errors from the filesystem; the caller should log them but not
/// abort startup.
pub fn install_system_skills(
    skills_dir: &Path,
    policy: &BundledSkillsConfig,
) -> std::io::Result<()> {
    if policy.install == Some(BundledSkillsInstall::None) {
        return Ok(());
    }
    let marker = skills_dir.join(".system-installed-version");

    let (installed_version, repair_marker) =
        match codewhale_util::fs_confined::read_to_string(skills_dir, &marker) {
            Ok(contents) => match contents.trim().parse::<u32>() {
                Ok(_) => (Some(contents.trim().to_string()), false),
                Err(_) => (None, true),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, false),
            Err(error) => return Err(error),
        };

    let mut changed = false;
    for skill in BUNDLED_SKILLS {
        changed |= install_one(skills_dir, skill, installed_version.as_deref(), policy)?;
    }

    // Feishu is optional: do not install for every user. If an older bundle
    // installed an exact shipped copy, leave it; never delete by name alone.
    let _ = feishu_body();

    // Contributor-onboarding is repo-local since generation 12: do not install
    // for new users. An older bundle's installed copy is left in place, same as
    // Feishu above — never delete by name alone.
    let _ = contributor_onboarding_body();

    // Feedback is repo-local since generation 13: same leave-in-place rule.
    let _ = feedback_body();

    if changed || repair_marker {
        fs::create_dir_all(skills_dir)?;
        codewhale_util::files::WorkspaceFile::open(
            skills_dir,
            Path::new(".system-installed-version"),
            true,
        )?
        .replace(BUNDLED_SKILL_VERSION.as_bytes())?;
    }
    Ok(())
}

pub fn canonical_bundled_skill_name(name: &str) -> Option<&'static str> {
    static CANONICAL: std::sync::LazyLock<std::collections::HashMap<&'static str, &'static str>> =
        std::sync::LazyLock::new(|| {
            BUNDLED_SKILLS
                .iter()
                .map(|skill| {
                    let canonical =
                        skill_parse::parse_verified_content(Path::new("SKILL.md"), skill.body)
                            .ok()
                            .and_then(|(parsed, _)| {
                                parsed.aliases.into_iter().find_map(|alias| {
                                    BUNDLED_SKILLS
                                        .iter()
                                        .find(|entry| entry.name == alias)
                                        .map(|entry| entry.name)
                                })
                            })
                            .unwrap_or(skill.name);
                    (skill.name, canonical)
                })
                .collect()
        });
    CANONICAL
        .get(normalize_skill_name_for_lookup(name).as_str())
        .copied()
}

fn listed(names: &Option<Vec<String>>, name: &str) -> bool {
    let canonical = canonical_bundled_skill_name(name);
    canonical.is_some()
        && names
            .iter()
            .flatten()
            .any(|entry| canonical_bundled_skill_name(entry) == canonical)
}

pub(super) fn bundled_enabled(policy: &BundledSkillsConfig, name: &str) -> bool {
    !listed(&policy.disabled, name)
        && (policy.selected.as_ref().is_none_or(Vec::is_empty) || listed(&policy.selected, name))
}

pub(super) fn bundled_explicit_only(policy: &BundledSkillsConfig, name: &str) -> bool {
    bundled_enabled(policy, name) && listed(&policy.explicit_only, name)
}

pub fn is_bundled_copy(path: &Path, name: &str, owned: &Path) -> bool {
    let expected = owned.join(name).join("SKILL.md");
    if !is_bundled_skill_name(name)
        || !super::roots::paths_refer_to_same_dir(path, &expected)
        || fs::symlink_metadata(owned.join(name).join(".installed-from")).is_ok()
    {
        return false;
    }
    codewhale_util::fs_confined::read_to_string(owned, &expected).is_ok_and(|body| {
        is_exact_bundled_skill(name, &body) || is_superseded_shipped_body(name, &body)
    })
}

#[derive(Debug, serde::Serialize)]
pub struct BundledSkillStatus {
    pub name: &'static str,
    pub path: std::path::PathBuf,
    pub reason: String,
}

#[derive(Debug, serde::Serialize)]
pub struct BundledSkillsStatus {
    pub install: BundledSkillsInstall,
    pub marker_generation: Option<String>,
    pub installed: usize,
    pub selected_missing: Vec<&'static str>,
    pub disabled: Vec<&'static str>,
    pub explicit_only: Vec<&'static str>,
    pub shadowed: Vec<&'static str>,
    pub unknown: Vec<String>,
    pub ignored: Vec<String>,
    pub skills: Vec<BundledSkillStatus>,
}

impl BundledSkillsStatus {
    pub fn summary(&self) -> String {
        format!(
            "Bundled skills: install={}, generation={}, installed={}, selected missing={}, disabled={}, explicit-only={}, shadowed={}",
            match self.install {
                BundledSkillsInstall::Auto => "auto",
                BundledSkillsInstall::All => "all",
                BundledSkillsInstall::None => "none",
            },
            self.marker_generation
                .as_deref()
                .unwrap_or("missing or invalid"),
            self.installed,
            self.selected_missing.len(),
            self.disabled.len(),
            self.explicit_only.len(),
            self.shadowed.len()
        )
    }

    pub fn inspection(&self) -> Vec<String> {
        let mut lines = vec![self.summary()];
        if !self.unknown.is_empty() {
            lines.push(format!(
                "Unknown bundled names: {}",
                self.unknown.join(", ")
            ));
        }
        if !self.ignored.is_empty() {
            lines.push(format!(
                "Ignored explicit_only entries: {}",
                self.ignored.join(", ")
            ));
        }
        lines.extend(
            self.skills
                .iter()
                .map(|row| format!("{}: {} ({})", row.name, row.reason, row.path.display())),
        );
        lines
    }
}

pub fn bundled_skills_status(
    config: &SkillsConfig,
    registry: &super::SkillRegistry,
) -> Option<BundledSkillsStatus> {
    let owned = codewhale_runtime::config::owned_skills_dir()?;
    let policy = &config.bundled;
    let marker_generation = codewhale_util::fs_confined::read_to_string(
        &owned,
        &owned.join(".system-installed-version"),
    )
    .ok()
    .and_then(|value| {
        value
            .trim()
            .parse::<u32>()
            .ok()
            .map(|generation| generation.to_string())
    });
    let mut status = BundledSkillsStatus {
        install: policy.install.unwrap_or_default(),
        marker_generation,
        installed: 0,
        selected_missing: vec![],
        disabled: vec![],
        explicit_only: vec![],
        shadowed: vec![],
        unknown: vec![],
        ignored: vec![],
        skills: vec![],
    };
    for (field, names) in [
        ("selected", &policy.selected),
        ("disabled", &policy.disabled),
        ("explicit_only", &policy.explicit_only),
    ] {
        for name in names.iter().flatten() {
            if canonical_bundled_skill_name(name).is_none() {
                status
                    .unknown
                    .push(format!("{field}={}", name.escape_debug()));
            } else if field == "explicit_only" && !bundled_enabled(policy, name) {
                status.ignored.push(name.escape_debug().to_string());
            }
        }
    }
    for skill in BUNDLED_SKILLS {
        let path = owned.join(skill.name).join("SKILL.md");
        let installed = codewhale_util::fs_confined::read_to_string(&owned, &path).is_ok();
        status.installed += usize::from(installed);
        let selected = registry
            .skills
            .iter()
            .find(|entry| entry.name == skill.name);
        let shadowed =
            selected.filter(|entry| !super::roots::paths_refer_to_same_dir(&entry.path, &path));
        let reason = if !bundled_enabled(policy, skill.name) {
            status.disabled.push(skill.name);
            let reason = if listed(&policy.disabled, skill.name) {
                "disabled by config"
            } else {
                "not selected by config"
            };
            if installed && !is_bundled_copy(&path, skill.name, &owned) {
                format!("manual or managed copy unaffected; bundled name {reason}")
            } else if let Some(selected) = shadowed {
                format!("{reason}; active copy at {}", selected.path.display())
            } else {
                reason.to_string()
            }
        } else if let Some(selected) = shadowed {
            status.shadowed.push(skill.name);
            format!("shadowed by {}", selected.path.display())
        } else if !installed {
            status.selected_missing.push(skill.name);
            "selected but not installed".to_string()
        } else if !is_bundled_copy(&path, skill.name, &owned) {
            "manual or managed copy preserved".to_string()
        } else if bundled_explicit_only(policy, skill.name) {
            status.explicit_only.push(skill.name);
            "explicit-only by config".to_string()
        } else {
            "installed".to_string()
        };
        status.skills.push(BundledSkillStatus {
            name: skill.name,
            path,
            reason,
        });
    }
    Some(status)
}

#[cfg(test)]
mod tests;
