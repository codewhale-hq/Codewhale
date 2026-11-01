//! Session-scoped web citation registry shared by every web-tool surface.
//!
//! Ref IDs are opaque, deterministic within one session, and useless in a
//! foreign session. The registry stores only a normalized HTTP(S) URL, an
//! optional title, and the retrieval timestamp needed by Work Graph evidence.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

const CITATION_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_CITATIONS: usize = 4_096;

static CITATIONS: OnceLock<Mutex<HashMap<CitationKey, CitationEntry>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CitationKey {
    namespace: String,
    ref_id: String,
}

#[derive(Debug, Clone)]
struct CitationEntry {
    citation: WebCitation,
    touched_at: Instant,
}

/// Inspectable, secret-free web evidence metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebCitation {
    pub ref_id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub retrieved_at: String,
}

impl WebCitation {
    pub fn evidence_ref(&self) -> Result<crate::work_graph::EvidenceRef, String> {
        crate::work_graph::EvidenceRef::new(
            crate::work_graph::EvidenceKind::WebCitation {
                ref_id: self.ref_id.clone(),
                url: self.url.clone(),
                retrieved_at: self.retrieved_at.clone(),
            },
            self.ref_id.clone(),
            None,
            false,
        )
        .map_err(|error| error.to_string())
    }
}

/// Register a URL under a deterministic, session-scoped ref ID.
pub fn register(namespace: &str, url: &str, title: Option<&str>) -> Option<WebCitation> {
    let normalized = normalize_http_url(url)?;
    let ref_id = ref_id_for(namespace, &normalized);
    register_with_ref(namespace, &ref_id, &normalized, title)
}

/// Register a URL under a surface-owned ref ID (for example a `web.run` view).
pub fn register_with_ref(
    namespace: &str,
    ref_id: &str,
    url: &str,
    title: Option<&str>,
) -> Option<WebCitation> {
    if namespace.trim().is_empty()
        || ref_id.trim().is_empty()
        || ref_id
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return None;
    }
    let url = normalize_http_url(url)?;
    let key = CitationKey {
        namespace: namespace.to_string(),
        ref_id: ref_id.to_string(),
    };
    let now = Instant::now();
    let mut registry = CITATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cleanup(&mut registry, now);
    if let Some(existing) = registry.get_mut(&key) {
        if existing.citation.url != url {
            return None;
        }
        existing.touched_at = now;
        if existing.citation.title.is_none() {
            existing.citation.title = normalized_title(title);
        }
        return Some(existing.citation.clone());
    }
    if registry.len() >= MAX_CITATIONS
        && let Some(oldest) = registry
            .iter()
            .min_by_key(|(_, entry)| entry.touched_at)
            .map(|(key, _)| key.clone())
    {
        registry.remove(&oldest);
    }
    let citation = WebCitation {
        ref_id: ref_id.to_string(),
        url,
        title: normalized_title(title),
        retrieved_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    };
    citation.evidence_ref().ok()?;
    registry.insert(
        key,
        CitationEntry {
            citation: citation.clone(),
            touched_at: now,
        },
    );
    Some(citation)
}

/// Resolve a ref only inside the session that minted it.
pub fn resolve(namespace: &str, ref_id: &str) -> Option<WebCitation> {
    let key = CitationKey {
        namespace: namespace.to_string(),
        ref_id: ref_id.to_string(),
    };
    let now = Instant::now();
    let mut registry = CITATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cleanup(&mut registry, now);
    let entry = registry.get_mut(&key)?;
    entry.touched_at = now;
    Some(entry.citation.clone())
}

fn ref_id_for(namespace: &str, url: &str) -> String {
    let identity = format!("{namespace}\0{url}");
    let digest = codewhale_runtime::hashing::sha256_hex(identity.as_bytes());
    format!("web_{}", &digest[..16])
}

pub fn normalize_http_url(url: &str) -> Option<String> {
    let mut parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    let query_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(name, _)| !is_sensitive_query_name(name))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    let query_was_sanitized = parsed.query_pairs().count() != query_pairs.len();
    if query_was_sanitized {
        parsed.set_query(None);
        if !query_pairs.is_empty() {
            parsed.query_pairs_mut().extend_pairs(query_pairs);
        }
    }
    parsed.set_fragment(None);
    if !parsed.username().is_empty() {
        parsed.set_username("").ok()?;
    }
    if parsed.password().is_some() {
        parsed.set_password(None).ok()?;
    }
    Some(parsed.to_string())
}

fn is_sensitive_query_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "access_token"
            | "api_key"
            | "authorization"
            | "auth"
            | "credential"
            | "key"
            | "session"
            | "session_id"
            | "sig"
            | "signature"
            | "token"
            | "x-amz-credential"
            | "x-amz-signature"
            | "x-goog-credential"
            | "x-goog-signature"
    ) || name.ends_with("_token")
        || name.ends_with("_key")
}

fn normalized_title(title: Option<&str>) -> Option<String> {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| title.chars().take(240).collect())
}

fn cleanup(registry: &mut HashMap<CitationKey, CitationEntry>, now: Instant) {
    registry.retain(|_, entry| now.duration_since(entry.touched_at) <= CITATION_TTL);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_are_stable_within_a_session_and_scoped_between_sessions() {
        let first = register(
            "session-a",
            "https://example.com/page#section",
            Some("Page"),
        )
        .expect("valid citation");
        let repeated =
            register("session-a", "https://example.com/page", None).expect("same citation");
        let foreign =
            register("session-b", "https://example.com/page", None).expect("foreign citation");

        assert_eq!(first.ref_id, repeated.ref_id);
        assert_eq!(first.retrieved_at, repeated.retrieved_at);
        assert_ne!(first.ref_id, foreign.ref_id);
        assert_eq!(first.url, "https://example.com/page");
        assert!(resolve("session-b", &first.ref_id).is_none());
        let evidence = first.evidence_ref().expect("citation evidence");
        assert_eq!(evidence.reference(), first.ref_id);
    }

    #[test]
    fn registry_rejects_non_web_urls_and_strips_url_credentials() {
        assert!(register("session", "javascript:alert(1)", None).is_none());
        let citation = register(
            "session",
            "https://user:password@example.com/path#private",
            None,
        )
        .expect("http citation");
        assert_eq!(citation.url, "https://example.com/path");
        assert!(!citation.url.contains("password"));
        let signed = register(
            "session",
            "https://example.com/path?access_token=sensitive&view=full",
            None,
        )
        .expect("credential values are removed from citation metadata");
        assert_eq!(signed.url, "https://example.com/path?view=full");
        assert!(!signed.url.contains("sensitive"));
    }

    #[test]
    fn explicit_refs_are_validated_and_session_scoped() {
        assert!(register_with_ref("session", "two words", "https://example.com", None).is_none());
        let citation = register_with_ref(
            "session",
            "s1_turn0view1",
            "https://example.com",
            Some("Example"),
        )
        .expect("explicit ref");
        assert_eq!(citation.ref_id, "s1_turn0view1");
        assert!(resolve("other", "s1_turn0view1").is_none());
        assert!(
            register_with_ref(
                "session",
                "s1_turn0view1",
                "https://other.example.com",
                None
            )
            .is_none(),
            "an existing ref must never be rebound to another URL"
        );
    }
}
