//! Z.AI and Zhipu structured Web Search API contracts.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::{
    ProviderNativeSearchRequest, ProviderNativeSearchResponse, citation_from_url, push_citation,
};

pub(super) fn build_body(request: &ProviderNativeSearchRequest, base_url: &str) -> Result<Value> {
    let normalized = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    let search_engine = match normalized.as_str() {
        "https://api.z.ai/api/paas/v4" => "search-prime",
        "https://open.bigmodel.cn/api/paas/v4" => "search_std",
        _ => bail!("unsupported Z.AI web-search endpoint: {base_url}"),
    };
    Ok(json!({
        "search_engine": search_engine,
        "search_query": request.query,
        "count": request.max_results,
    }))
}

pub(super) fn parse(payload: &Value) -> ProviderNativeSearchResponse {
    let mut citations = Vec::new();
    if let Some(results) = payload.get("search_result").and_then(Value::as_array) {
        for result in results {
            let Some(url) = result.get("link").and_then(Value::as_str) else {
                continue;
            };
            let title = result
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string);
            let snippet = result
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_string);
            let published = result
                .get("publish_date")
                .and_then(Value::as_str)
                .map(str::to_string);
            push_citation(
                &mut citations,
                citation_from_url(url, title, snippet, published),
            );
        }
    }
    ProviderNativeSearchResponse {
        answer: None,
        citations,
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ProviderNativeSearchRequest {
        ProviderNativeSearchRequest {
            query: "current release".to_string(),
            max_results: 3,
            domains: vec!["example.com".to_string()],
        }
    }

    #[test]
    fn request_uses_site_specific_search_engine() {
        for (base_url, expected_engine) in [
            ("https://api.z.ai/api/paas/v4", "search-prime"),
            ("https://open.bigmodel.cn/api/paas/v4/", "search_std"),
        ] {
            let body = build_body(&request(), base_url).expect("official endpoint");
            assert_eq!(body["search_engine"], expected_engine);
            assert_eq!(body["search_query"], "current release");
            assert_eq!(body["count"], 3);
            assert!(body.get("search_domain_filter").is_none());
        }
    }

    #[test]
    fn request_rejects_unproven_product_surface() {
        assert!(build_body(&request(), "https://api.z.ai/api/coding/paas/v4").is_err());
    }

    #[test]
    fn parses_structured_search_results() {
        let parsed = parse(&json!({
            "search_result": [{
                "title": "Release",
                "content": "Release notes",
                "link": "https://example.com/release",
                "publish_date": "2026-08-28"
            }]
        }));
        assert_eq!(parsed.citations.len(), 1);
        assert_eq!(parsed.citations[0].title, "Release");
        assert_eq!(parsed.citations[0].published.as_deref(), Some("2026-08-28"));
    }
}
