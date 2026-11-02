use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::frontmatter::{
    SkillValidationMode, parse_frontmatter, parse_frontmatter_bool, validate_skill_frontmatter,
};

pub const MAX_SKILL_NAME_CHARS: usize = 64;

/// Maximum directory-traversal depth when discovering skills.
///
/// Defends against pathological configurations (e.g. a user pointing
/// `skills_dir` at `~`) without artificially limiting realistic
/// vendored layouts like `<root>/<org>/<repo>/<skill>/SKILL.md`.
pub const MAX_DISCOVERY_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInvocation {
    ModelAndUser,
    ExplicitOnly,
    ModelOnly,
    Disabled,
}

impl SkillInvocation {
    pub fn model_invocable(self) -> bool {
        matches!(self, Self::ModelAndUser | Self::ModelOnly)
    }

    pub fn user_invocable(self) -> bool {
        matches!(self, Self::ModelAndUser | Self::ExplicitOnly)
    }

    pub fn from_frontmatter(value: Option<&str>) -> Self {
        match value.map(str::trim).map(|value| value.to_ascii_lowercase()) {
            Some(value) if value == "explicit-only" || value == "explicit_only" => {
                Self::ExplicitOnly
            }
            _ => Self::ModelAndUser,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedSkill {
    pub name: String,
    pub legacy_activation_name: Option<String>,
    pub description: String,
    pub localized_descriptions: HashMap<String, String>,
    pub invocation: SkillInvocation,
    pub aliases: Vec<String>,
    pub argument_hint: Option<String>,
    pub body: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct NormalizedSkillName {
    pub name: String,
    pub legacy_activation_name: Option<String>,
    pub warning: Option<String>,
}

pub fn normalize_skill_name(name: &str, skill_path: &Path) -> NormalizedSkillName {
    let legacy = legacy_skill_name_for_lookup(name);
    let normalized = normalize_skill_name_for_lookup(name);
    let supported_unicode_name =
        !name.is_ascii() && !name.chars().any(char::is_control) && is_valid_skill_name(&normalized);
    let legacy_activation_name = (legacy != normalized).then_some(legacy);
    let mut resolved = name.to_string();
    let mut warning = None;
    if normalized != name || !is_valid_skill_name(name) {
        let original = std::mem::replace(&mut resolved, normalized);
        if !supported_unicode_name {
            warning = Some(format!(
                "Skill name `{original}` in {} is not a safe command name; using `{resolved}` instead.",
                skill_path.display(),
            ));
        }
    }
    NormalizedSkillName {
        name: resolved,
        legacy_activation_name,
        warning,
    }
}

pub fn parse_verified_content(
    path: &Path,
    content: &str,
) -> std::result::Result<(ParsedSkill, Vec<String>), String> {
    let (mut skill, mut warnings) = parse_skill_and_warnings(path, content)?;
    skill.path = path.to_path_buf();
    let normalized = normalize_skill_name(&skill.name, path);
    skill.legacy_activation_name = normalized.legacy_activation_name;
    skill.name = normalized.name;
    warnings.extend(normalized.warning);
    Ok((skill, warnings))
}

pub fn parse_skill_and_warnings(
    path: &Path,
    content: &str,
) -> std::result::Result<(ParsedSkill, Vec<String>), String> {
    // Try to parse frontmatter block first. If absent, fall back to
    // extracting the first `# Heading` as the skill name so that plain
    // Markdown files (no `---` fence) are accepted instead of rejected.
    let parsed = parse_frontmatter(content)?;
    let warnings = validate_skill_frontmatter(
        parsed.as_ref().map(|(metadata, _)| metadata),
        Some(path),
        SkillValidationMode::Lenient,
    )?;
    if let Some((metadata, body)) = parsed {
        let name = metadata
            .get("name")
            .filter(|name| !name.is_empty())
            .cloned()
            .ok_or_else(|| "missing required frontmatter field: name".to_string())?;

        let mut description = metadata.get("description").cloned().unwrap_or_default();
        if let Some(trigger) = metadata
            .get("when_to_use")
            .filter(|value| !value.trim().is_empty())
        {
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str("Use when: ");
            description.push_str(trigger.trim());
        }

        let explicit =
            SkillInvocation::from_frontmatter(metadata.get("invocation").map(String::as_str))
                == SkillInvocation::ExplicitOnly;
        let model_allowed = !explicit
            && metadata
                .get("disable-model-invocation")
                .is_none_or(|value| parse_frontmatter_bool(value) == Some(false));
        let user_allowed = metadata
            .get("user-invocable")
            .is_none_or(|value| parse_frontmatter_bool(value) == Some(true));
        let invocation = match (model_allowed, user_allowed) {
            (true, true) => SkillInvocation::ModelAndUser,
            (false, true) => SkillInvocation::ExplicitOnly,
            (true, false) => SkillInvocation::ModelOnly,
            (false, false) => SkillInvocation::Disabled,
        };
        let aliases = metadata
            .get("aliases-for")
            .into_iter()
            .flat_map(|value| value.split([',', ' ', '\t']))
            .map(str::trim)
            .filter(|alias| !alias.is_empty())
            .map(normalize_skill_name_for_lookup)
            .filter(|alias| is_valid_skill_name(alias))
            .collect();

        // Collect `description_<tag>:` frontmatter keys (already lowercased
        // above) into locale-specific descriptions, e.g. `description_zh`.
        let localized_descriptions = metadata
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix("description_")
                    .filter(|tag| !tag.is_empty())
                    .map(|tag| (tag.to_string(), value.clone()))
            })
            .collect();

        return Ok((
            ParsedSkill {
                name,
                legacy_activation_name: None,
                description,
                localized_descriptions,
                invocation,
                aliases,
                argument_hint: metadata
                    .get("argument-hint")
                    .filter(|value| !value.trim().is_empty())
                    .cloned(),
                body: body.trim().to_string(),
                // Filled in by `discover` after parse succeeds; default to an
                // empty path so direct constructors (e.g. tests) compile.
                path: PathBuf::new(),
            },
            warnings,
        ));
    }

    // Graceful degradation: no frontmatter fence found.
    // Extract the first `# Heading` as the skill name.
    let heading_re = regex::Regex::new(r"(?m)^#\s+(.+)$").expect("static regex is valid");
    let name = heading_re
        .captures(content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "no frontmatter and no `# Heading` found to use as skill name".to_string()
        })?;

    Ok((
        ParsedSkill {
            name,
            legacy_activation_name: None,
            description: String::new(),
            localized_descriptions: HashMap::new(),
            invocation: SkillInvocation::ModelAndUser,
            aliases: Vec::new(),
            argument_hint: None,
            body: content.trim().to_string(),
            path: PathBuf::new(),
        },
        warnings,
    ))
}

pub fn is_valid_skill_name(name: &str) -> bool {
    let char_count = name.chars().count();
    char_count > 0
        && char_count <= MAX_SKILL_NAME_CHARS
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

pub fn normalize_skill_name_for_lookup(name: &str) -> String {
    normalize_qualified_skill_name(name, normalize_skill_name_segment)
}

fn legacy_skill_name_for_lookup(name: &str) -> String {
    normalize_qualified_skill_name(name, legacy_skill_name_segment)
}

fn normalize_qualified_skill_name(name: &str, segment: fn(&str) -> String) -> String {
    if let Some((plugin, skill)) = name.trim().split_once(':')
        && !plugin.is_empty()
        && !skill.is_empty()
        && !skill.contains(':')
    {
        return format!("{}:{}", segment(plugin), segment(skill));
    }
    segment(name)
}

pub fn normalize_skill_name_segment(name: &str) -> String {
    let name = name.trim();
    let legacy = legacy_skill_name_segment(name);
    if name.is_ascii() {
        return legacy;
    }
    // Preserve ASCII identities, but distinguish UTF-8 source names the old
    // slug folded together. No transliteration or Unicode normalization.
    let digest = crate::install::sha256_hex(name.to_ascii_lowercase().as_bytes());
    let prefix = legacy[..legacy.len().min(31)].trim_end_matches('-');
    format!("{prefix}-{}", &digest[..32])
}

fn legacy_skill_name_segment(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;

    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() && out.len() < MAX_SKILL_NAME_CHARS {
                out.push('-');
            }
            pending_dash = false;
            if out.len() < MAX_SKILL_NAME_CHARS {
                out.push(ch.to_ascii_lowercase());
            }
        } else {
            pending_dash = true;
        }

        if out.len() >= MAX_SKILL_NAME_CHARS {
            break;
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        "skill".to_string()
    } else {
        out
    }
}
