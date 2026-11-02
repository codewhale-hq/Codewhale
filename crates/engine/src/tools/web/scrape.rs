//! Shared DuckDuckGo / Bing HTML SERP scrapers and spam filter.
//!
//! Used by both `web_search` and `web_run` so parser behavior (including the
//! #964 evidence-based spam filter) cannot drift between the two paths.

use base64::{Engine as _, engine::general_purpose};
use regex::Regex;
use std::sync::OnceLock;

/// Shared browser-like user agent for public SERP scraping and image search.
/// Both `web_search` and `web_run` send this so scrape behavior (including
/// bot-challenge rates) stays identical between the two surfaces.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

/// One parsed search hit: title, absolute URL, optional snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapedSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

// Cached regex patterns for HTML parsing
static TITLE_RE: OnceLock<Regex> = OnceLock::new();
static SNIPPET_RE: OnceLock<Regex> = OnceLock::new();
static TAG_RE: OnceLock<Regex> = OnceLock::new();
static BING_RESULT_RE: OnceLock<Regex> = OnceLock::new();
static BING_TITLE_RE: OnceLock<Regex> = OnceLock::new();
static BING_SNIPPET_RE: OnceLock<Regex> = OnceLock::new();

fn get_title_re() -> &'static Regex {
    TITLE_RE.get_or_init(|| {
        Regex::new(r#"<a[^>]*class=\"result__a\"[^>]*href=\"([^\"]+)\"[^>]*>(.*?)</a>"#)
            .expect("title regex pattern is valid")
    })
}

fn get_snippet_re() -> &'static Regex {
    SNIPPET_RE.get_or_init(|| {
        Regex::new(
            r#"<a[^>]*class=\"result__snippet\"[^>]*>(.*?)</a>|<div[^>]*class=\"result__snippet\"[^>]*>(.*?)</div>"#,
        )
        .expect("snippet regex pattern is valid")
    })
}

fn get_tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("tag regex pattern is valid"))
}

fn get_bing_result_re() -> &'static Regex {
    BING_RESULT_RE.get_or_init(|| {
        Regex::new(r#"(?is)<li[^>]*class=\"[^\"]*\bb_algo\b[^\"]*\"[^>]*>(.*?)</li>"#)
            .expect("bing result regex pattern is valid")
    })
}

fn get_bing_title_re() -> &'static Regex {
    BING_TITLE_RE.get_or_init(|| {
        Regex::new(r#"(?is)<h2[^>]*>.*?<a[^>]*href=\"([^\"]+)\"[^>]*>(.*?)</a>"#)
            .expect("bing title regex pattern is valid")
    })
}

fn get_bing_snippet_re() -> &'static Regex {
    BING_SNIPPET_RE.get_or_init(|| {
        Regex::new(r#"(?is)<div[^>]*class=\"[^\"]*\bb_caption\b[^\"]*\"[^>]*>.*?<p[^>]*>(.*?)</p>"#)
            .expect("bing snippet regex pattern is valid")
    })
}

/// Parse DuckDuckGo HTML SERP results. Known spam-domain hits are omitted.
pub fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<ScrapedSearchResult> {
    let title_re = get_title_re();
    let snippet_re = get_snippet_re();
    let snippets: Vec<String> = snippet_re
        .captures_iter(html)
        .filter_map(|cap| cap.get(1).or_else(|| cap.get(2)))
        .map(|m| normalize_text(m.as_str()))
        .collect();

    let mut results = Vec::new();
    for (idx, cap) in title_re.captures_iter(html).enumerate() {
        if results.len() >= max_results {
            break;
        }
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title_raw = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let title = normalize_text(title_raw);
        if title.is_empty() {
            continue;
        }
        let url = normalize_duckduckgo_url(href);
        if is_known_spam_url(&url) {
            continue;
        }
        let snippet = snippets
            .get(idx)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        results.push(ScrapedSearchResult {
            title,
            url,
            snippet,
        });
    }

    results
}

/// Parse Bing HTML SERP results. Known spam-domain hits are omitted.
pub fn parse_bing_results(html: &str, max_results: usize) -> Vec<ScrapedSearchResult> {
    let mut results = Vec::new();
    for cap in get_bing_result_re().captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let Some(block) = cap.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let Some(title_cap) = get_bing_title_re().captures(block) else {
            continue;
        };
        let href = title_cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title_raw = title_cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let title = normalize_text(title_raw);
        if title.is_empty() {
            continue;
        }
        let snippet = get_bing_snippet_re()
            .captures(block)
            .and_then(|snippet_cap| snippet_cap.get(1))
            .map(|m| normalize_text(m.as_str()))
            .filter(|s| !s.is_empty());

        let url = normalize_bing_url(href);
        if is_known_spam_url(&url) {
            continue;
        }
        results.push(ScrapedSearchResult {
            title,
            url,
            snippet,
        });
    }
    results
}

/// Detect DuckDuckGo bot-challenge interstitial HTML.
pub fn is_duckduckgo_challenge(html: &str) -> bool {
    html.contains("anomaly-modal") || html.contains("Unfortunately, bots use DuckDuckGo too")
}

/// Evidence-based filter for the domain family in #964. A broad same-domain
/// ratio is not a spam signal: site-scoped searches and documentation hosts
/// legitimately return many results from one registrable domain.
fn is_known_spam_url(url: &str) -> bool {
    const KNOWN_SPAM_DOMAINS: &[&str] = &["forumgratuit.org"];

    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    KNOWN_SPAM_DOMAINS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn normalize_duckduckgo_url(href: &str) -> String {
    if let Some(uddg) = extract_query_param(href, "uddg") {
        let decoded = percent_decode(&uddg);
        if !decoded.is_empty() {
            return decoded;
        }
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    if href.starts_with('/') {
        return format!("https://duckduckgo.com{href}");
    }
    href.to_string()
}

/// Normalize a Bing SERP result href, unwrapping `/ck/a?...&u=<base64>` redirects.
pub fn normalize_bing_url(href: &str) -> String {
    // Bing wraps every SERP result URL in a `/ck/a?...&u=<base64>` click-tracking
    // redirect, and in the raw HTML the separators are `&amp;` entities. Without
    // decoding entities first, `extract_query_param` looks for `u` but the actual
    // key is `amp;u`, so the real URL is never recovered and callers receive Bing's
    // tracking URL instead of the cited source. Decode entities before parsing.
    let href = decode_html_entities(href);
    let href = href.as_str();
    if let Some(encoded) = extract_query_param(href, "u") {
        let decoded = percent_decode(&encoded);
        let token = decoded.strip_prefix("a1").unwrap_or(&decoded);
        let mut padded = token.replace('-', "+").replace('_', "/");
        while !padded.len().is_multiple_of(4) {
            padded.push('=');
        }
        if let Ok(bytes) = general_purpose::STANDARD.decode(padded)
            && let Ok(url) = String::from_utf8(bytes)
            && (url.starts_with("http://") || url.starts_with("https://"))
        {
            return url;
        }
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    if href.starts_with('/') {
        return format!("https://www.bing.com{href}");
    }
    href.to_string()
}

fn normalize_text(text: &str) -> String {
    let stripped = strip_html_tags(text);
    let decoded = decode_html_entities(&stripped);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_html_tags(text: &str) -> String {
    get_tag_re().replace_all(text, "").to_string()
}

/// Decode common HTML named and numeric character references.
pub fn decode_html_entities(text: &str) -> String {
    static ENTITY_RE: OnceLock<Regex> = OnceLock::new();
    let re = ENTITY_RE.get_or_init(|| {
        Regex::new(r"&(?:#(\d+)|#x([0-9A-Fa-f]+)|([a-zA-Z]+));").expect("HTML entity regex")
    });

    re.replace_all(text, |caps: &regex::Captures| {
        if let Some(dec) = caps.get(1) {
            return dec
                .as_str()
                .parse::<u32>()
                .ok()
                .and_then(std::char::from_u32)
                .unwrap_or('\u{FFFD}')
                .to_string();
        }
        if let Some(hex) = caps.get(2) {
            return u32::from_str_radix(hex.as_str(), 16)
                .ok()
                .and_then(std::char::from_u32)
                .unwrap_or('\u{FFFD}')
                .to_string();
        }
        let named = caps.get(3).map(|m| m.as_str());
        match named {
            Some("amp") => "&",
            Some("lt") => "<",
            Some("gt") => ">",
            Some("quot") => "\"",
            Some("apos") => "'",
            Some("nbsp") => " ",
            Some("copy") => "\u{00A9}",
            Some("reg") => "\u{00AE}",
            Some("mdash") => "\u{2014}",
            Some("ndash") => "\u{2013}",
            Some("lsquo") => "\u{2018}",
            Some("rsquo") => "\u{2019}",
            Some("ldquo") => "\u{201C}",
            Some("rdquo") => "\u{201D}",
            Some("hellip") => "\u{2026}",
            _ => return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string(),
        }
        .to_string()
    })
    .to_string()
}

/// Percent-decode a URL component. `+` becomes space (query-string convention).
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &input[i + 1..i + 3];
                if let Ok(val) = u8::from_str_radix(hex, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
            }
            b'+' => out.push(b' '),
            _ => out.push(bytes[i]),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for part in query.split('&') {
        let mut iter = part.splitn(2, '=');
        let name = iter.next().unwrap_or("");
        if name == key {
            return iter.next().map(str::to_string);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression guard: Bing /ck/a redirect hrefs are HTML-entity-encoded
    // (`&amp;`). normalize_bing_url must decode entities before extracting the
    // `u=` base64 payload, otherwise the real URL is never recovered and the
    // result remains a Bing tracking URL instead of the cited source.
    #[test]
    fn bing_ckurl_with_html_entities_decodes_real_url() {
        let href = "https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1aHR0cHM6Ly9ydXN0LWxhbmcub3JnLw&amp;ntb=1";
        assert_eq!(normalize_bing_url(href), "https://rust-lang.org/");
    }

    #[test]
    fn parses_bing_results_and_decodes_redirect_url() {
        let html = r#"
            <ol>
              <li class="b_algo">
                <h2><a href="https://www.bing.com/ck/a?u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9wYXRoP3E9MQ">Example &amp; Result</a></h2>
                <div class="b_caption"><p>A <strong>useful</strong> snippet.</p></div>
              </li>
            </ol>
        "#;

        let results = parse_bing_results(html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example & Result");
        assert_eq!(results[0].url, "https://example.com/path?q=1");
        assert_eq!(results[0].snippet.as_deref(), Some("A useful snippet."));
    }

    #[test]
    fn parses_duckduckgo_results() {
        let html = r#"
            <a class="result__a" href="https://example.com/rust">Rust &amp; async</a>
            <a class="result__snippet">A <b>useful</b> snippet.</a>
            <a class="result__a" href="//docs.rs/tokio">Tokio</a>
            <div class="result__snippet">Runtime docs</div>
        "#;
        let results = parse_duckduckgo_results(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust & async");
        assert_eq!(results[0].url, "https://example.com/rust");
        assert_eq!(results[0].snippet.as_deref(), Some("A useful snippet."));
        assert_eq!(results[1].url, "https://docs.rs/tokio");
    }

    #[test]
    fn known_spam_filter_is_domain_specific() {
        assert!(is_known_spam_url("https://astralia.forumgratuit.org/page1"));
        assert!(!is_known_spam_url("https://forumgratuit.org.example/a"));
        assert!(!is_known_spam_url("https://docs.example.com/a"));
    }

    #[test]
    fn legitimate_same_domain_results_are_preserved() {
        let html = r#"
            <a class="result__a" href="https://docs.example.com/a">A</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.com/b">B</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.com/c">C</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.com/d">D</a>
            <a class="result__snippet">s</a>
        "#;
        assert_eq!(parse_duckduckgo_results(html, 10).len(), 4);
    }

    #[test]
    fn public_suffix_and_private_suffix_hosts_are_not_misgrouped() {
        let html = r#"
            <a class="result__a" href="https://alpha.example.co.uk/a">A</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://beta.example.co.uk/b">B</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://alice.github.io/c">C</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://bob.github.io/d">D</a>
            <a class="result__snippet">s</a>
        "#;
        assert_eq!(parse_duckduckgo_results(html, 10).len(), 4);
    }

    #[test]
    fn max_results_three_keeps_legitimate_same_domain_results() {
        let html = r#"
            <a class="result__a" href="https://docs.example.com/a">A</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.com/b">B</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.com/c">C</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.com/d">D</a>
            <a class="result__snippet">s</a>
        "#;
        assert_eq!(parse_duckduckgo_results(html, 3).len(), 3);
    }

    #[test]
    fn parse_duckduckgo_filters_known_spam_domain() {
        // Shared path used by web_run and web_search: known spam → empty.
        let html = r#"
            <a class="result__a" href="https://astralia.forumgratuit.org/a">A</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://russia.forumgratuit.org/b">B</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://other.forumgratuit.org/c">C</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://hello.forumgratuit.org/d">D</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://world.forumgratuit.org/e">E</a>
            <a class="result__snippet">s</a>
        "#;
        assert!(parse_duckduckgo_results(html, 10).is_empty());
    }

    #[test]
    fn parse_bing_filters_known_spam_domain() {
        let html = r#"
            <ol>
              <li class="b_algo">
                <h2><a href="https://astralia.forumgratuit.org/a">A</a></h2>
                <div class="b_caption"><p>s</p></div>
              </li>
              <li class="b_algo">
                <h2><a href="https://russia.forumgratuit.org/b">B</a></h2>
                <div class="b_caption"><p>s</p></div>
              </li>
              <li class="b_algo">
                <h2><a href="https://other.forumgratuit.org/c">C</a></h2>
                <div class="b_caption"><p>s</p></div>
              </li>
              <li class="b_algo">
                <h2><a href="https://hello.forumgratuit.org/d">D</a></h2>
                <div class="b_caption"><p>s</p></div>
              </li>
              <li class="b_algo">
                <h2><a href="https://world.forumgratuit.org/e">E</a></h2>
                <div class="b_caption"><p>s</p></div>
              </li>
            </ol>
        "#;
        assert!(parse_bing_results(html, 10).is_empty());
    }

    #[test]
    fn decode_html_entities_handles_named_entities() {
        assert_eq!(decode_html_entities("&amp;"), "&");
        assert_eq!(decode_html_entities("&lt;"), "<");
        assert_eq!(decode_html_entities("&gt;"), ">");
        assert_eq!(decode_html_entities("&quot;"), "\"");
        assert_eq!(decode_html_entities("&apos;"), "'");
        assert_eq!(decode_html_entities("&nbsp;"), " ");
        assert_eq!(decode_html_entities("&copy;"), "\u{00A9}");
        assert_eq!(decode_html_entities("&mdash;"), "\u{2014}");
    }

    #[test]
    fn decode_html_entities_handles_decimal_numeric_references() {
        assert_eq!(decode_html_entities("&#65;"), "A");
        assert_eq!(decode_html_entities("&#60;"), "<");
        assert_eq!(decode_html_entities("&#8211;"), "\u{2013}");
    }

    #[test]
    fn decode_html_entities_handles_hex_numeric_references() {
        assert_eq!(decode_html_entities("&#x41;"), "A");
        assert_eq!(decode_html_entities("&#x3C;"), "<");
        assert_eq!(decode_html_entities("&#x2014;"), "\u{2014}");
    }

    #[test]
    fn decode_html_entities_passthrough_unknown() {
        assert_eq!(decode_html_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn decode_html_entities_mixed_content() {
        let input = "Hello &amp; welcome to &quot;Rust&apos;s world&quot; &mdash; enjoy!";
        let expected = "Hello & welcome to \"Rust's world\" \u{2014} enjoy!";
        assert_eq!(decode_html_entities(input), expected);
    }

    #[test]
    fn percent_decode_handles_utf8_multibyte_sequences() {
        // Percent-encoded CJK: %E4%B8%AA%E4%BA%BA = 个人 (each glyph is 3 UTF-8 bytes).
        assert_eq!(percent_decode("Hello %E4%B8%AA%E4%BA%BA"), "Hello 个人");
        assert_eq!(percent_decode("%E7%B4%A0%E6%9D%90"), "素材");
        // Percent-encoded UTF-8 inside a URL path (DuckDuckGo `uddg=` redirect shape).
        assert_eq!(
            percent_decode("https://example.com/%E9%A1%B5%E9%9D%A2"),
            "https://example.com/页面"
        );
        // Raw UTF-8 in the input passes through unchanged.
        assert_eq!(percent_decode("查询 keyword"), "查询 keyword");
        // Query-string convention: `+` becomes space; `%20` becomes space.
        assert_eq!(percent_decode("foo+bar%20baz"), "foo bar baz");
    }

    #[test]
    fn is_duckduckgo_challenge_detects_interstitial() {
        assert!(is_duckduckgo_challenge(
            "Unfortunately, bots use DuckDuckGo too."
        ));
        assert!(is_duckduckgo_challenge(
            r#"<div class="anomaly-modal">challenge</div>"#
        ));
        assert!(!is_duckduckgo_challenge(
            r#"<a class="result__a" href="https://example.com">ok</a>"#
        ));
    }
}
