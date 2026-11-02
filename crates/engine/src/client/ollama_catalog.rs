use std::time::Duration;

use futures_util::StreamExt;

use codewhale_config::catalog::{
    CatalogOffering, CatalogSource, ProviderCatalogDelta, base_url_fingerprint, now_unix,
};
use serde::Deserialize;

pub const TAGS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub const SHOW_PROBE_LIMIT: usize = 8;

/// What `/api/show` reports about one tag. Both fields are optional because
/// older daemons omit `capabilities` and some architectures omit a context
/// length; a missing fact is unknown, never a "no".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OllamaTagProfile {
    pub capabilities: Option<Vec<String>>,
    pub context_length: Option<u64>,
}

impl OllamaTagProfile {
    pub fn has_capability(&self, name: &str) -> Option<bool> {
        self.capabilities
            .as_ref()
            .map(|caps| caps.iter().any(|cap| cap.eq_ignore_ascii_case(name)))
    }
}

/// Name heuristic for tags that cannot chat: embedding and reranking models.
/// Used only when the daemon did not report capabilities.
pub fn looks_like_non_chat_tag(tag: &str) -> bool {
    let lower = tag.to_ascii_lowercase();
    ["embed", "bge", "rerank", "minilm"]
        .iter()
        .any(|needle| lower.contains(needle))
}

#[derive(Debug, Deserialize)]
struct OllamaShowResponse {
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    model_info: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Parse `POST /api/show` JSON into the facts adoption ranks on.
pub fn parse_ollama_show_response(payload: &str) -> anyhow::Result<OllamaTagProfile> {
    let parsed: OllamaShowResponse = serde_json::from_str(payload)
        .map_err(|err| anyhow::anyhow!("Failed to parse Ollama /api/show JSON: {err}"))?;
    let context_length = parsed.model_info.as_ref().and_then(|info| {
        info.iter()
            .filter(|(key, _)| key.ends_with(".context_length"))
            .find_map(|(_, value)| value.as_u64())
    });
    Ok(OllamaTagProfile {
        capabilities: parsed.capabilities,
        context_length,
    })
}

/// Strip a trailing `/v1` (with optional slash) so we can hit native `/api/tags`.
pub fn ollama_native_origin(v1_base: &str) -> String {
    let trimmed = v1_base.trim().trim_end_matches('/');
    if let Some(origin) = trimmed.strip_suffix("/v1") {
        origin.to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaTagModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagModel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    details: Option<serde_json::Value>,
    #[serde(default)]
    context_length: Option<u64>,
}

/// Parse native Ollama `GET /api/tags` JSON into sorted unique tag ids.
#[cfg(any(test, feature = "test-support"))]
pub fn parse_ollama_tags_response(payload: &str) -> anyhow::Result<Vec<String>> {
    parse_ollama_tags_with_profiles(payload).map(|(tags, _)| tags)
}

pub fn parse_ollama_tags_with_profiles(
    payload: &str,
) -> anyhow::Result<(
    Vec<String>,
    std::collections::HashMap<String, OllamaTagProfile>,
)> {
    let parsed: OllamaTagsResponse = serde_json::from_str(payload)
        .map_err(|err| anyhow::anyhow!("Failed to parse Ollama /api/tags JSON: {err}"))?;
    anyhow::ensure!(
        parsed.models.len() <= super::PROVIDER_CATALOG_MAX_ROWS,
        "Ollama catalog exceeds the model row limit"
    );
    let mut profiles = std::collections::HashMap::new();
    for row in parsed.models {
        let tag = if row.name.trim().is_empty() {
            row.model.trim()
        } else {
            row.name.trim()
        };
        if !codewhale_runtime::catalog_cache::valid_catalog_model_id(tag) {
            continue;
        }
        let profile = OllamaTagProfile {
            capabilities: row.capabilities,
            context_length: row
                .details
                .as_ref()
                .and_then(|details| details.get("context_length"))
                .and_then(serde_json::Value::as_u64),
        };
        if let Some(previous) = profiles.insert(tag.to_string(), profile.clone()) {
            anyhow::ensure!(
                previous == profile,
                "Ollama reported conflicting tag capabilities"
            );
        }
    }
    let mut tags: Vec<String> = profiles.keys().cloned().collect();
    tags.sort();
    Ok((tags, profiles))
}

pub fn ollama_catalog_delta(
    provider: &str,
    endpoint_v1: &str,
    tags: &[String],
    profiles: &std::collections::HashMap<String, OllamaTagProfile>,
    running_body: Option<&str>,
) -> ProviderCatalogDelta {
    let running: std::collections::HashMap<String, u64> = running_body
        .and_then(|body| serde_json::from_str::<OllamaTagsResponse>(body).ok())
        .filter(|response| response.models.len() <= super::PROVIDER_CATALOG_MAX_ROWS)
        .into_iter()
        .flat_map(|response| response.models)
        .filter_map(|row| {
            let context = row.context_length.filter(|value| *value > 0)?;
            let context = u32::try_from(context).ok().map(u64::from)?;
            let tag = if row.name.trim().is_empty() {
                row.model.trim()
            } else {
                row.name.trim()
            };
            (!tag.is_empty()).then(|| (tag.to_string(), context))
        })
        .collect();
    let chat_tags: Vec<String> = tags
        .iter()
        .filter(|tag| codewhale_runtime::catalog_cache::valid_catalog_model_id(tag))
        .filter(|tag| {
            profiles
                .get(*tag)
                .is_some_and(|profile| profile.has_capability("completion") == Some(true))
        })
        .cloned()
        .collect();
    let preferred = choose_chat_tag(&chat_tags, profiles);
    let fingerprint = base_url_fingerprint(endpoint_v1);
    let fetched_at = now_unix();
    let offerings = chat_tags
        .iter()
        .filter_map(|tag| {
            let profile = profiles.get(tag)?;
            Some(CatalogOffering {
                provider: provider.to_string(),
                wire_model_id: tag.clone(),
                endpoint_key: "chat".into(),
                default_for_provider: preferred.as_deref() == Some(tag.as_str()),
                tool_call: profile.has_capability("tools"),
                reasoning: profile.has_capability("thinking"),
                limit: running.get(tag).copied().map(|context| {
                    codewhale_config::models_dev::ModelsDevLimit {
                        context: Some(context),
                        ..Default::default()
                    }
                }),
                source: CatalogSource::Live {
                    base_url_fingerprint: fingerprint.clone(),
                    fetched_at,
                },
                ..Default::default()
            })
        })
        .collect();
    ProviderCatalogDelta {
        provider: provider.to_string(),
        base_url_fingerprint: fingerprint,
        fetched_at,
        offerings,
    }
}

pub fn probe_client() -> anyhow::Result<reqwest::Client> {
    // The first-run probe can run before any provider client has installed
    // the rustls crypto provider; the shared builder installs it (the bare
    // `reqwest::Client::builder()` panics under `rustls-no-provider`).
    Ok(codewhale_release::tls::reqwest_client_builder()
        .timeout(TAGS_PROBE_TIMEOUT)
        .build()?)
}

#[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
pub const MODEL_LOAD_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
pub async fn load_model(origin: &str, tag: &str) -> bool {
    let Ok(client) = codewhale_release::tls::reqwest_client_builder()
        .timeout(MODEL_LOAD_TIMEOUT)
        .build()
    else {
        return false;
    };
    client
        .post(format!("{origin}/api/generate"))
        .json(&serde_json::json!({ "model": tag }))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

pub async fn fetch_text(url: &str) -> anyhow::Result<String> {
    let response = probe_client()?.get(url).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }
    super::bounded_provider_catalog_text(response, super::PROVIDER_CATALOG_MAX_RESPONSE_BYTES)
        .await
        .map_err(|reason| anyhow::anyhow!("Ollama catalog response rejected: {reason:?}"))
}

pub async fn fetch_tag_profile(origin: &str, tag: &str) -> anyhow::Result<OllamaTagProfile> {
    let response = probe_client()?
        .post(format!("{origin}/api/show"))
        .json(&serde_json::json!({ "model": tag }))
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }
    let body =
        super::bounded_provider_catalog_text(response, super::PROVIDER_CATALOG_MAX_RESPONSE_BYTES)
            .await
            .map_err(|reason| anyhow::anyhow!("Ollama profile response rejected: {reason:?}"))?;
    parse_ollama_show_response(&body)
}

/// An unprofiled tag cannot become an executable catalog default.
pub async fn fetch_tag_profiles<F, Fut>(
    tags: &[String],
    profiles: &std::collections::HashMap<String, OllamaTagProfile>,
    mut fetch: F,
) -> std::collections::HashMap<String, OllamaTagProfile>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<OllamaTagProfile>>,
{
    let candidates: Vec<String> = tags
        .iter()
        .filter(|tag| {
            profiles
                .get(*tag)
                .is_none_or(|profile| profile.capabilities.is_none())
        })
        .cloned()
        .collect();
    let probes = futures_util::stream::iter(candidates.into_iter().map(|tag| {
        let future = fetch(tag.clone());
        async move {
            let result = future.await;
            (tag, result)
        }
    }));
    probes
        .buffer_unordered(SHOW_PROBE_LIMIT)
        .filter_map(|(tag, result)| futures_util::future::ready(match result {
            Ok(profile) => Some((tag, profile)),
            Err(err) => {
                tracing::debug!(target: "local_ollama", error = %err, tag = %tag, "POST /api/show probe failed");
                None
            }
        }))
        .collect()
        .await
}

fn looks_like_coder_tag(tag: &str) -> bool {
    let lower = tag.to_ascii_lowercase();
    lower.contains("coder") || lower.contains("code")
}

/// Pick the tag adoption should switch to.
///
/// A tag is a chat candidate when `/api/show` lists `completion`, or — when
/// the daemon reported no capabilities — when its name is not an embedding or
/// reranker. Among candidates: coder or tool-capable models first, then the
/// largest reported context, then alphabetical order for stability.
pub fn choose_chat_tag(
    tags: &[String],
    profiles: &std::collections::HashMap<String, OllamaTagProfile>,
) -> Option<String> {
    let unknown = OllamaTagProfile::default();
    tags.iter()
        .filter_map(|tag| {
            let profile = profiles.get(tag).unwrap_or(&unknown);
            let chat = match profile.has_capability("completion") {
                Some(known) => known,
                None => !looks_like_non_chat_tag(tag),
            };
            if !chat {
                return None;
            }
            let preferred =
                looks_like_coder_tag(tag) || profile.has_capability("tools").unwrap_or(false);
            Some((
                preferred,
                profile.context_length.unwrap_or(0),
                std::cmp::Reverse(tag.as_str()),
                tag,
            ))
        })
        .max_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)))
        .map(|(_, _, _, tag)| tag.clone())
}
