//! Deterministic, explainable suggestions for curated remote skills.
//!
//! This module deliberately has no network or install side effects. The slash
//! command fetches the configured registry under the existing network policy,
//! then this matcher ranks its metadata. A suggestion is never an install,
//! trust decision, or activation.

use crate::install::{RegistryDocument, RegistryEntry};

const MIN_QUERY_CHARS: usize = 3;
const MAX_EXPLANATIONS: usize = 3;

/// One remote registry entry ranked for a user's task description.
#[derive(Debug)]
pub struct RemoteSkillRecommendation<'a> {
    pub name: &'a str,
    pub entry: &'a RegistryEntry,
    /// The strongest, human-readable matching evidence, in rank order.
    pub matched_terms: Vec<String>,
    score: usize,
}

impl RemoteSkillRecommendation<'_> {
    #[must_use]
    pub fn score(&self) -> usize {
        self.score
    }
}

/// Rank up to `limit` remote skills for `query`.
///
/// Explicit registry keywords and domains outrank name and description
/// fallback matches. Ties are resolved by the registry key, which is a
/// `BTreeMap`, making results stable across runs. Matching uses ASCII word
/// boundaries so `box` does not accidentally match `boxing`.
pub fn recommend_remote_skills<'a>(
    query: &str,
    registry: &'a RegistryDocument,
    limit: usize,
) -> Vec<RemoteSkillRecommendation<'a>> {
    if limit == 0 || query.chars().count() < MIN_QUERY_CHARS {
        return Vec::new();
    }

    let query = query.to_ascii_lowercase();
    let mut recommendations = registry
        .skills
        .iter()
        .filter_map(|(name, entry)| recommend_one(&query, name, entry))
        .collect::<Vec<_>>();

    recommendations.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.name.cmp(right.name))
    });
    recommendations.truncate(limit);
    recommendations
}

fn recommend_one<'a>(
    query: &str,
    name: &'a str,
    entry: &'a RegistryEntry,
) -> Option<RemoteSkillRecommendation<'a>> {
    let mut matches = Vec::new();

    for keyword in &entry.keywords {
        add_phrase_match(query, keyword, "keyword", 900, &mut matches);
    }
    for domain in &entry.domains {
        if let Some(domain) = normalize_domain(domain) {
            add_phrase_match(query, &domain, "domain", 850, &mut matches);
        }
    }

    add_phrase_match(query, name, "name", 800, &mut matches);
    for term in word_terms(name) {
        add_phrase_match(query, &term, "name", 700, &mut matches);
    }

    if let Some(description) = entry.description.as_deref() {
        for term in word_terms(description) {
            if !is_generic_description_term(&term) {
                add_phrase_match(query, &term, "description", 120, &mut matches);
            }
        }
    }

    if matches.is_empty() {
        return None;
    }

    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.reason.cmp(&right.reason))
    });

    let primary_score = matches[0].score;
    let mut matched_terms = Vec::new();
    let mut bonus = 0usize;
    for found in matches {
        if matched_terms
            .iter()
            .any(|existing| existing == &found.reason)
        {
            continue;
        }
        if !matched_terms.is_empty() {
            // Extra evidence helps break close calls without allowing a long
            // generic description to outrank an explicit keyword.
            bonus += found.score.min(40);
        }
        matched_terms.push(found.reason);
        if matched_terms.len() == MAX_EXPLANATIONS {
            break;
        }
    }

    Some(RemoteSkillRecommendation {
        name,
        entry,
        matched_terms,
        score: primary_score + bonus,
    })
}

#[derive(Debug)]
struct Match {
    score: usize,
    reason: String,
}

fn add_phrase_match(
    query: &str,
    raw_term: &str,
    label: &str,
    base_score: usize,
    out: &mut Vec<Match>,
) {
    let term = raw_term.trim().to_ascii_lowercase();
    if term.chars().count() < MIN_QUERY_CHARS || !keyword_matches(query.as_bytes(), term.as_bytes())
    {
        return;
    }

    out.push(Match {
        score: base_score + term.len(),
        reason: format!("{label} `{term}`"),
    });
}

fn normalize_domain(domain: &str) -> Option<String> {
    let trimmed = domain.trim();
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme)
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    (!host.is_empty()).then(|| host.to_string())
}

fn word_terms(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .map(str::trim)
        .filter(|term| term.chars().count() >= MIN_QUERY_CHARS)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn is_generic_description_term(term: &str) -> bool {
    matches!(
        term,
        "about"
            | "agent"
            | "agents"
            | "build"
            | "create"
            | "from"
            | "help"
            | "helps"
            | "make"
            | "skill"
            | "skills"
            | "task"
            | "tasks"
            | "that"
            | "this"
            | "tool"
            | "tools"
            | "using"
            | "with"
            | "work"
            | "workflow"
            | "workflows"
            | "your"
    )
}

fn keyword_matches(haystack: &[u8], keyword: &[u8]) -> bool {
    if keyword.is_empty() || keyword.len() > haystack.len() {
        return false;
    }

    haystack
        .windows(keyword.len())
        .enumerate()
        .any(|(start, window)| {
            if window != keyword {
                return false;
            }
            let end = start + keyword.len();
            let start_ok = start == 0 || is_word(haystack[start - 1]) != is_word(haystack[start]);
            let end_ok =
                end == haystack.len() || is_word(haystack[end - 1]) != is_word(haystack[end]);
            start_ok && end_ok
        })
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn entry(description: &str, keywords: &[&str], domains: &[&str]) -> RegistryEntry {
        RegistryEntry {
            source: "github:example/skill".to_string(),
            description: Some(description.to_string()),
            keywords: keywords.iter().map(|value| (*value).to_string()).collect(),
            domains: domains.iter().map(|value| (*value).to_string()).collect(),
        }
    }

    fn registry(entries: &[(&str, RegistryEntry)]) -> RegistryDocument {
        RegistryDocument {
            skills: entries
                .iter()
                .map(|(name, entry)| ((*name).to_string(), entry.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn explicit_keywords_outrank_description_fallbacks() {
        let registry = registry(&[
            (
                "notes",
                entry("Create and organize spreadsheet notes", &[], &[]),
            ),
            (
                "table-tools",
                entry("Work with data files", &["spreadsheet"], &[]),
            ),
        ]);

        let matches = recommend_remote_skills("clean up this spreadsheet", &registry, 3);

        assert_eq!(matches[0].name, "table-tools");
        assert_eq!(matches[0].matched_terms[0], "keyword `spreadsheet`");
    }

    #[test]
    fn normalized_domains_match_pasted_urls() {
        let registry = registry(&[(
            "design",
            entry(
                "Design review workflow",
                &[],
                &["https://www.figma.com/files"],
            ),
        )]);

        let matches = recommend_remote_skills(
            "review https://www.figma.com/file/abc with me",
            &registry,
            3,
        );

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_terms[0], "domain `figma.com`");
    }

    #[test]
    fn short_queries_and_substrings_do_not_match() {
        let registry = registry(&[("box", entry("Box workflow", &["box"], &[]))]);

        assert!(recommend_remote_skills("go", &registry, 3).is_empty());
        assert!(recommend_remote_skills("boxing", &registry, 3).is_empty());
    }

    #[test]
    fn ties_are_stable_by_skill_name() {
        let registry = registry(&[
            ("beta", entry("", &["review"], &[])),
            ("alpha", entry("", &["review"], &[])),
        ]);

        let matches = recommend_remote_skills("review this change", &registry, 3);

        assert_eq!(
            matches.iter().map(|item| item.name).collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn name_and_description_remain_backward_compatible_fallbacks() {
        let registry = registry(&[("slide-deck", entry("Prepare presentation slides", &[], &[]))]);

        let matches = recommend_remote_skills("prepare presentation slides", &registry, 3);

        assert_eq!(matches.len(), 1);
        assert!(
            matches[0]
                .matched_terms
                .iter()
                .any(|reason| reason == "description `presentation`"),
            "expected explanation to include description fallback: {matches:#?}"
        );
    }
}
