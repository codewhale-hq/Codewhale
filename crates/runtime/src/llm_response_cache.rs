//! Small in-process cache for deterministic non-streaming chat responses.

use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};

use lru::LruCache;
use sha2::{Digest, Sha256};

use codewhale_models::{MessageRequest, MessageResponse, Usage};

const DEFAULT_CAPACITY: usize = 256;

static RESPONSE_CACHE: OnceLock<ResponseCache> = OnceLock::new();

pub fn response_cache() -> &'static ResponseCache {
    RESPONSE_CACHE.get_or_init(ResponseCache::new)
}

pub fn request_is_cacheable(request: &MessageRequest) -> bool {
    request.stream != Some(true)
        && request.tools.as_ref().is_none_or(Vec::is_empty)
        && request.tool_choice.is_none()
        && request.temperature == Some(0.0)
        && request.top_p.is_none_or(|top_p| top_p == 1.0)
}

pub struct ResponseCache {
    inner: Mutex<LruCache<[u8; 32], MessageResponse>>,
}

impl ResponseCache {
    fn new() -> Self {
        Self::with_capacity(NonZeroUsize::new(DEFAULT_CAPACITY).expect("non-zero capacity"))
    }

    fn with_capacity(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    pub fn make_key(
        provider: &str,
        base_url: &str,
        path_suffix: Option<&str>,
        api_key: &str,
        wire_body: &[u8],
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        update_field(&mut hasher, provider.as_bytes());
        update_field(&mut hasher, base_url.as_bytes());
        update_field(&mut hasher, path_suffix.unwrap_or("").as_bytes());
        update_field(&mut hasher, &Sha256::digest(api_key.as_bytes()));
        update_field(&mut hasher, wire_body);
        hasher.finalize().into()
    }

    pub fn get(&self, key: &[u8; 32]) -> Option<MessageResponse> {
        let mut cache = self.inner.lock().ok()?;
        cache.get(key).cloned().map(|mut response| {
            response.usage = Usage::default();
            response
        })
    }

    pub fn put(&self, key: [u8; 32], value: MessageResponse) {
        if let Ok(mut cache) = self.inner.lock() {
            cache.put(key, value);
        }
    }
}

fn update_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with_usage(id: &str) -> MessageResponse {
        MessageResponse {
            id: id.to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: Vec::new(),
            model: "test-model".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            title: None,
            stop_details: None,
            refusal: None,
            usage: Usage {
                input_tokens: 42,
                output_tokens: 7,
                prompt_cache_hit_tokens: Some(3),
                prompt_cache_miss_tokens: Some(39),
                prompt_cache_write_tokens: None,
                reasoning_tokens: Some(5),
                reasoning_replay_tokens: Some(2),
                server_tool_use: None,

                input_audio_tokens: None,
                output_audio_tokens: None,

                request_usages: Vec::new(),
                safety_tokens: None,
            },
        }
    }

    fn request() -> MessageRequest {
        MessageRequest {
            model: "test-model".to_string(),
            messages: Vec::new(),
            max_tokens: 16,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: Some(0.0),
            top_p: None,
            safety_check: None,
            title: None,
        }
    }

    #[test]
    fn cache_key_separates_provider_route_account_and_wire_body() {
        let base = ResponseCache::make_key(
            "deepseek",
            "https://api.example.com/v1",
            None,
            "key-a",
            br#"{"model":"m","messages":[]}"#,
        );

        assert_ne!(
            base,
            ResponseCache::make_key(
                "openai",
                "https://api.example.com/v1",
                None,
                "key-a",
                br#"{"model":"m","messages":[]}"#
            )
        );
        assert_ne!(
            base,
            ResponseCache::make_key(
                "deepseek",
                "https://proxy.example.com/v1",
                None,
                "key-a",
                br#"{"model":"m","messages":[]}"#
            )
        );
        assert_ne!(
            base,
            ResponseCache::make_key(
                "deepseek",
                "https://api.example.com/v1",
                Some("responses"),
                "key-a",
                br#"{"model":"m","messages":[]}"#
            )
        );
        assert_ne!(
            base,
            ResponseCache::make_key(
                "deepseek",
                "https://api.example.com/v1",
                None,
                "key-b",
                br#"{"model":"m","messages":[]}"#
            )
        );
        assert_ne!(
            base,
            ResponseCache::make_key(
                "deepseek",
                "https://api.example.com/v1",
                None,
                "key-a",
                br#"{"model":"m","messages":[],"reasoning_effort":"high"}"#
            )
        );
    }

    #[test]
    fn cache_hit_zeroes_usage_to_avoid_fake_spend() {
        let cache = ResponseCache::with_capacity(NonZeroUsize::new(2).unwrap());
        let key =
            ResponseCache::make_key("deepseek", "https://api.example.com", None, "key", b"{}");

        cache.put(key, response_with_usage("cached"));

        let hit = cache.get(&key).expect("cache hit");
        assert_eq!(hit.id, "cached");
        assert_eq!(hit.usage, Usage::default());
    }

    #[test]
    fn capacity_evicts_oldest_entry() {
        let cache = ResponseCache::with_capacity(NonZeroUsize::new(2).unwrap());
        let key1 =
            ResponseCache::make_key("deepseek", "https://api.example.com", None, "key", b"one");
        let key2 =
            ResponseCache::make_key("deepseek", "https://api.example.com", None, "key", b"two");
        let key3 =
            ResponseCache::make_key("deepseek", "https://api.example.com", None, "key", b"three");

        cache.put(key1, response_with_usage("one"));
        cache.put(key2, response_with_usage("two"));
        cache.put(key3, response_with_usage("three"));

        assert!(cache.get(&key1).is_none());
        assert!(cache.get(&key2).is_some());
        assert!(cache.get(&key3).is_some());
    }

    #[test]
    fn cacheability_requires_deterministic_tool_free_non_streaming_request() {
        let mut req = request();
        assert!(request_is_cacheable(&req));

        req.temperature = None;
        assert!(!request_is_cacheable(&req));

        req = request();
        req.temperature = Some(0.2);
        assert!(!request_is_cacheable(&req));

        req = request();
        req.stream = Some(true);
        assert!(!request_is_cacheable(&req));

        req = request();
        req.top_p = Some(0.5);
        assert!(!request_is_cacheable(&req));

        req = request();
        req.tool_choice = Some(serde_json::json!("auto"));
        assert!(!request_is_cacheable(&req));
    }
}
