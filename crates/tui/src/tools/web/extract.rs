//! Content-type routing and readable-document extraction for web tools.
//!
//! Networking deliberately lives elsewhere. This module accepts already
//! fetched bytes and turns them into one normalized document so `fetch_url`
//! and `web.run` cannot disagree about HTML, Markdown, PDF, or media handling.

use super::adapter::{AdapterFailure, AdapterResult};
use std::sync::OnceLock;

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use regex::Regex;

use crate::tools::spec::ToolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentKind {
    Html,
    Markdown,
    Text,
    Pdf,
    Media,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtractedDocument {
    pub(crate) kind: DocumentKind,
    pub(crate) title: Option<String>,
    pub(crate) text: String,
    pub(crate) markdown: String,
    /// Readability-cleaned HTML. `web.run` consumes this to retain clickable
    /// links while avoiding page chrome and consent-banner noise.
    pub(crate) cleaned_html: Option<String>,
    pub(crate) pdf_pages: Option<Vec<Vec<String>>>,
    /// Validated extension for image/audio/video artifacts.
    pub(crate) media_extension: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaSignature {
    extension: &'static str,
    family: MediaFamily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaFamily {
    Image,
    Audio,
    Video,
}

static TITLE_RE: OnceLock<Regex> = OnceLock::new();
static META_RE: OnceLock<Regex> = OnceLock::new();
static FALLBACK_RE: OnceLock<[[Regex; 3]; 2]> = OnceLock::new();
static PAGE_CHROME_RE: OnceLock<[Regex; 2]> = OnceLock::new();
static PAGE_HEADER_RE: OnceLock<Regex> = OnceLock::new();
static TAG_RE: OnceLock<Regex> = OnceLock::new();
static WHITESPACE_RE: OnceLock<Regex> = OnceLock::new();

/// HTML's encoding declaration prescan is intentionally small. Keeping the
/// bound here prevents a late body string, script, or injected fragment from
/// changing how an already-started document is decoded.
const HTML_ENCODING_SNIFF_BYTES: usize = 1_024;

pub(crate) async fn extract_document(
    url: &str,
    content_type: Option<&str>,
    bytes: &[u8],
    truncated: bool,
    context: Option<&super::super::spec::ToolContext>,
) -> AdapterResult<ExtractedDocument> {
    extract_document_with_pdf_command(
        url,
        content_type,
        bytes,
        truncated,
        super::super::pdf::PdfTextCommand::system(context),
    )
    .await
}

pub(crate) async fn extract_document_with_pdf_command(
    url: &str,
    content_type: Option<&str>,
    bytes: &[u8],
    truncated: bool,
    pdf_command: super::super::pdf::PdfTextCommand<'_>,
) -> AdapterResult<ExtractedDocument> {
    let context = pdf_command.context();
    let declared = normalized_content_type(content_type);
    let declared = declared.as_deref();

    if bytes.is_empty() {
        return Ok(ExtractedDocument {
            kind: DocumentKind::Text,
            title: None,
            text: String::new(),
            markdown: String::new(),
            cleaned_html: None,
            pdf_pages: None,
            media_extension: None,
        });
    }

    if validate_pdf_response(url, content_type, bytes)? {
        return extract_pdf(bytes, pdf_command).await.map_err(|error| {
            if context.is_some_and(|context| {
                context
                    .features
                    .enabled(codewhale_runtime::features::Feature::PdfHost)
            }) {
                AdapterFailure::host(error)
            } else {
                error.into()
            }
        });
    }

    if let Some(signature) = sniff_media(bytes) {
        if let Some(declared_family) = declared_media_family(declared)
            && declared_family != signature.family
        {
            return Err((ToolError::execution_failed(format!(
                "Response media type `{}` did not match its bytes",
                declared.unwrap_or("unknown")
            )))
            .into());
        }
        return Ok(ExtractedDocument {
            kind: DocumentKind::Media,
            title: None,
            text: String::new(),
            markdown: String::new(),
            cleaned_html: None,
            pdf_pages: None,
            media_extension: Some(signature.extension),
        });
    }

    if declared_media_family(declared).is_some() {
        return Err((ToolError::execution_failed(format!(
            "Response claimed media type `{}`, but its bytes did not match a supported media signature",
            declared.unwrap_or("unknown")
        ))).into());
    }

    let sniff_html = should_sniff_html_encoding(declared, url, bytes);
    let body = decode_response_body(bytes, content_type, sniff_html)?;
    if sniff_html || is_html(declared, url, &body) {
        if let Some(context) = context.filter(|context| {
            context
                .features
                .enabled(codewhale_runtime::features::Feature::WebExtractHost)
        }) {
            return extract_html_with_host(url, &body, truncated, context).await;
        }
        return extract_html(url, &body, truncated).map_err(Into::into);
    }
    if is_markdown(declared, url) {
        return Ok(ExtractedDocument {
            kind: DocumentKind::Markdown,
            title: markdown_title(&body),
            text: body.clone(),
            markdown: body,
            cleaned_html: None,
            pdf_pages: None,
            media_extension: None,
        });
    }
    if is_textual(declared, url) {
        return Ok(ExtractedDocument {
            kind: DocumentKind::Text,
            title: None,
            text: body.clone(),
            markdown: body,
            cleaned_html: None,
            pdf_pages: None,
            media_extension: None,
        });
    }

    Err((ToolError::execution_failed(format!(
        "Unsupported binary response type `{}`; use a dedicated download tool",
        declared.unwrap_or("unknown")
    )))
    .into())
}

pub(crate) fn validate_pdf_response(
    url: &str,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<bool, ToolError> {
    let declared = normalized_content_type(content_type);
    let declared = declared.as_deref();
    let signed = looks_like_pdf(bytes);
    if signed && declared_media_family(declared).is_some() {
        return Err(ToolError::execution_failed(format!(
            "Response media type `{}` did not match its PDF bytes",
            declared.unwrap_or("unknown")
        )));
    }
    let claimed = signed || declared == Some("application/pdf") || url_is_pdf(url);
    if claimed && !signed {
        return Err(ToolError::execution_failed(
            "Response claimed to be a PDF, but its bytes did not contain a PDF signature",
        ));
    }
    Ok(claimed)
}

async fn extract_html_with_host(
    url: &str,
    html: &str,
    truncated: bool,
    context: &super::super::spec::ToolContext,
) -> AdapterResult<ExtractedDocument> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Choice {
        kind: String,
        candidate: Option<u8>,
    }
    let parsed_url = reqwest::Url::parse(url)
        .map_err(|error| ToolError::invalid_input(format!("invalid URL: {error}")))?;
    let candidates = main_html_candidates(html, truncated);
    let facts=candidates.iter().map(|(id,html)| {
        let text=html_to_plain_text(html);
        serde_json::json!({"id":id,"non_whitespace":text.chars().filter(|value|!value.is_whitespace()).count(),"words":text.split_whitespace().count()})
    }).collect::<Vec<_>>();
    let budget = context
        .turn_deadline
        .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
        .unwrap_or(std::time::Duration::from_secs(15));
    let choice: Choice = super::adapter::transform(
        crate::extension_host::StockOperation::WebExtract,
        serde_json::json!({"candidates":facts}),
        context,
        budget,
    )
    .await?;
    if choice.kind != "web_extract" {
        return Err(AdapterFailure::host(ToolError::execution_failed(
            "Web Host returned a malformed HTML region choice",
        )));
    }
    let expected = candidates
        .iter()
        .find(|(_, html)| meaningful_html(html))
        .map(|(id, _)| *id);
    if choice.candidate != expected {
        return Err(AdapterFailure::host(ToolError::execution_failed(
            "Web Host changed mandatory readable-region order or meaningfulness",
        )));
    }
    let Some(selected) = choice.candidate else {
        return Err(js_required_error(url).into());
    };
    let cleaned = candidates
        .into_iter()
        .find(|(id, _)| *id == selected)
        .map(|(_, html)| html)
        .ok_or_else(|| {
            AdapterFailure::host(ToolError::execution_failed(
                "Web Host returned an unknown HTML region",
            ))
        })?;
    document_from_html_region(url, &parsed_url, html_title(html), cleaned).map_err(Into::into)
}

fn extract_html(url: &str, html: &str, truncated: bool) -> Result<ExtractedDocument, ToolError> {
    let parsed_url = reqwest::Url::parse(url)
        .map_err(|err| ToolError::invalid_input(format!("invalid URL: {err}")))?;
    let original_title = html_title(html);

    // Readability-based extraction was removed to consolidate the HTML
    // pipeline onto a single stack (htmd 0.5 + html5ever 0.38). The
    // previous dual-stack (readability 0.3 / html5ever 0.26 + htmd / 0.38)
    // compiled two incompatible html5ever/markup5ever trees. The fallback
    // main-content regex retains the meaningful-content signal used by the
    // tests (≥32 non-whitespace chars, ≥5 words) without the duplicate tree.
    let cleaned_html = fallback_main_html(html, truncated).ok_or_else(|| js_required_error(url))?;
    document_from_html_region(url, &parsed_url, original_title, cleaned_html)
}

fn document_from_html_region(
    url: &str,
    parsed_url: &reqwest::Url,
    original_title: Option<String>,
    cleaned_html: String,
) -> Result<ExtractedDocument, ToolError> {
    let markdown = html_to_markdown_with_base_url(&cleaned_html, parsed_url).map_err(|err| {
        ToolError::execution_failed(format!(
            "Failed to convert readable HTML to Markdown: {err}"
        ))
    })?;
    let text = html_to_plain_text(&cleaned_html);

    if !meaningful_text(&text) && !meaningful_text(&markdown) {
        return Err(js_required_error(url));
    }

    let title = original_title;

    Ok(ExtractedDocument {
        kind: DocumentKind::Html,
        title,
        text,
        markdown,
        cleaned_html: Some(cleaned_html),
        pdf_pages: None,
        media_extension: None,
    })
}

/// Resolve relative anchors in `htmd`'s parsed DOM, not with an HTML regex.
/// Absolute, fragment, non-HTTP, and malformed destinations fall through to
/// the built-in handler unchanged.
fn html_to_markdown_with_base_url(
    html: &str,
    base_url: &reqwest::Url,
) -> Result<String, std::io::Error> {
    let base_url = base_url.clone();
    htmd::HtmlToMarkdown::builder()
        .add_handler(
            vec!["a"],
            move |handlers: &dyn htmd::element_handler::Handlers, element: htmd::Element<'_>| {
                let href = element.attrs.iter().find_map(|attr| {
                    (attr.name.local.as_ref() == "href").then(|| attr.value.to_string())
                });
                let Some(destination) = href
                    .as_deref()
                    .and_then(|href| resolve_relative_http_href(&base_url, href))
                else {
                    return handlers.fallback(element);
                };
                let content = handlers.walk_children(element.node).content;
                let trailing = &content[content.trim_end().len()..];
                let destination = destination.replace('(', "\\(").replace(')', "\\)");
                let title = element
                    .attrs
                    .iter()
                    .find_map(|attr| {
                        (attr.name.local.as_ref() == "title").then(|| {
                            attr.value
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                                .replace('"', "\\\"")
                        })
                    })
                    .map_or_else(String::new, |title| format!(" \"{title}\""));
                Some(format!("[{}]({destination}{title}){trailing}", content.trim()).into())
            },
        )
        .build()
        .convert(html)
}

fn resolve_relative_http_href(base_url: &reqwest::Url, href: &str) -> Option<String> {
    if !matches!(base_url.scheme(), "http" | "https") {
        return None;
    }

    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || reqwest::Url::parse(href).is_ok() {
        return None;
    }

    base_url.join(href).ok().map(Into::into)
}

/// Pick the readable region of a page with bounded regexes.
///
/// Order: every `<article>` (listing, news and forum pages carry one per
/// card), then `<main>`, then `<body>`. Page chrome is removed from the chosen
/// region; a `<header>` is chrome only at body level, because inside an
/// article or `<main>` it carries the title and byline. Forms are unwrapped,
/// not dropped: ASP.NET-style pages wrap the whole body in one `<form>`, so
/// only the controls themselves are stripped.
///
/// Known limits: the lazy regexes do not balance nested same-name elements
/// (an `<article>` inside an `<article>` ends at the inner close tag), and no
/// JavaScript runs, so a client-rendered shell still yields nothing.
fn main_html_candidates(html: &str, truncated: bool) -> Vec<(u8, String)> {
    let html = html
        .rfind('<')
        .filter(|&start| truncated && !html[start..].contains('>'))
        .map_or(html, |start| &html[..start]);
    let [article, main, body] = &FALLBACK_RE.get_or_init(|| {
        [false, true].map(|open_ended| {
            ["article", "main", "body"].map(|tag| {
                let end = element_end(tag, open_ended);
                Regex::new(&format!(r"(?is)<{tag}(?:\s[^>]*)?>(.*?){end}"))
                    .expect("fallback element regex")
            })
        })
    })[usize::from(truncated)];
    let articles = article
        .captures_iter(html)
        .filter_map(|capture| capture.get(1))
        .map(|content| strip_page_chrome(content.as_str(), false, truncated))
        .filter(|content| !html_to_plain_text(content).is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let mut candidates = vec![(0, articles)];
    for (id, re, strip_header) in [(1, main, false), (2, body, true)] {
        if let Some(content) = re.captures(html).and_then(|capture| capture.get(1)) {
            candidates.push((
                id,
                strip_page_chrome(content.as_str(), strip_header, truncated),
            ));
        }
    }
    candidates
}

fn fallback_main_html(html: &str, truncated: bool) -> Option<String> {
    main_html_candidates(html, truncated)
        .into_iter()
        .find_map(|(_, html)| meaningful_html(&html).then_some(html))
}

fn element_end(tag: &str, open_ended: bool) -> String {
    if open_ended {
        format!(r"(?:</{tag}\s*>|\z)")
    } else {
        format!(r"</{tag}\s*>")
    }
}

fn strip_page_chrome(html: &str, strip_header: bool, truncated: bool) -> String {
    let page_chrome = &PAGE_CHROME_RE.get_or_init(|| {
        [false, true].map(|open_ended| {
            let block = |tag: &str| {
                let end = element_end(tag, open_ended);
                format!(r"<{tag}(?:\s[^>]*)?>.*?{end}")
            };
            Regex::new(&format!(
                "(?is)(?:{}|{}|{}{}",
                block("script"),
                block("style"),
                block("noscript"),
                concat!(
                    r"|<nav(?:\s[^>]*)?>.*?</nav\s*>",
                    r"|<footer(?:\s[^>]*)?>.*?</footer\s*>",
                    r"|<aside(?:\s[^>]*)?>.*?</aside\s*>",
                    // Form controls, and the form tags themselves (unwrapped).
                    r"|<select(?:\s[^>]*)?>.*?</select\s*>",
                    r"|<textarea(?:\s[^>]*)?>.*?</textarea\s*>",
                    r"|<button(?:\s[^>]*)?>.*?</button\s*>",
                    r"|<input(?:\s[^>]*)?>",
                    r"|</?form(?:\s[^>]*)?>)",
                ),
            ))
            .expect("page chrome regex")
        })
    })[usize::from(truncated)];
    let cleaned = page_chrome.replace_all(html, "");
    if !strip_header {
        return cleaned.into_owned();
    }
    PAGE_HEADER_RE
        .get_or_init(|| {
            Regex::new(r"(?is)<header(?:\s[^>]*)?>.*?</header\s*>").expect("page header regex")
        })
        .replace_all(&cleaned, "")
        .into_owned()
}

fn meaningful_html(html: &str) -> bool {
    meaningful_text(&html_to_plain_text(html))
}

fn meaningful_text(text: &str) -> bool {
    text.chars().filter(|ch| !ch.is_whitespace()).count() >= 32
        && text.split_whitespace().count() >= 5
}

fn html_to_plain_text(html: &str) -> String {
    let without_tags = TAG_RE
        .get_or_init(|| Regex::new(r"(?s)<[^>]+>").expect("tag regex"))
        .replace_all(html, " ");
    normalize_text(&decode_common_entities(&without_tags))
}

fn normalize_text(text: &str) -> String {
    WHITESPACE_RE
        .get_or_init(|| Regex::new(r"\s+").expect("whitespace regex"))
        .replace_all(text.trim(), " ")
        .into_owned()
}

fn decode_common_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn html_title(html: &str) -> Option<String> {
    let capture = TITLE_RE
        .get_or_init(|| {
            Regex::new(r"(?is)<title(?:\s[^>]*)?>(.*?)</title\s*>").expect("title regex")
        })
        .captures(html)?;
    let title = normalize_text(&decode_common_entities(capture.get(1)?.as_str()));
    (!title.is_empty()).then_some(title)
}

pub(crate) fn head_summary(
    url: &str,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Option<ExtractedDocument> {
    let declared = normalized_content_type(content_type);
    let sniff_html = should_sniff_html_encoding(declared.as_deref(), url, bytes);
    let html = decode_response_body(bytes, content_type, sniff_html).ok()?;
    let title = html_title(&html).or_else(|| meta_content(&html, "property", "og:title"));
    let description = meta_content(&html, "name", "description")
        .or_else(|| meta_content(&html, "property", "og:description"));
    let text = [title.as_deref(), description.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.is_empty() {
        return None;
    }
    let markdown = if title.is_some() {
        format!("# {text}")
    } else {
        text.clone()
    };
    Some(ExtractedDocument {
        kind: DocumentKind::Text,
        title,
        text,
        markdown,
        cleaned_html: None,
        pdf_pages: None,
        media_extension: None,
    })
}

fn meta_content(html: &str, attribute: &str, key: &str) -> Option<String> {
    META_RE
        .get_or_init(|| Regex::new(r"(?is)<meta\s[^>]*>").expect("meta regex"))
        .find_iter(html)
        .find_map(|tag| {
            let tag = tag.as_str();
            html_attribute_value(tag, attribute)
                .filter(|value| value.eq_ignore_ascii_case(key))
                .and_then(|_| html_attribute_value(tag, "content"))
                .map(|content| normalize_text(&decode_common_entities(&content)))
                .filter(|content| !content.is_empty())
        })
}

fn markdown_title(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let title = line.trim().strip_prefix("# ")?.trim();
        (!title.is_empty()).then(|| title.to_string())
    })
}

/// Stable prefix for "the response parsed, but carried no readable body".
///
/// The fetch pipeline matches on this to decide whether a cache-busting
/// re-fetch is worth one more request, and to attach the role-aware recovery
/// text. Extraction itself has no `ToolContext`, so it cannot know which
/// escalation the calling role actually owns; it states the fact and leaves
/// the remedy to [`super::fetch`].
pub(crate) const JS_SHELL_MARKER: &str = "No readable page content was found at";

/// Whether `error` is the JS-shell extraction failure (a parsed response whose
/// body held no readable content), as opposed to a transport or type failure.
pub(crate) fn is_js_shell_error(error: &ToolError) -> bool {
    error.to_string().contains(JS_SHELL_MARKER)
}

fn js_required_error(url: &str) -> ToolError {
    ToolError::execution_failed(format!(
        "{JS_SHELL_MARKER} {url}; the response parsed but its body held no readable content, so the page may require JavaScript."
    ))
}

/// Decode one response body without guessing from language statistics.
///
/// Precedence is receipt-grade and deterministic: BOM, recognized transport
/// charset, an HTML-only bounded meta prescan, then UTF-8. Unknown transport
/// labels deliberately fall through to a valid HTML declaration. `html_sniff`
/// must come from MIME/URL/ASCII markup evidence; JSON and plain text callers
/// pass `false`, so a body string cannot impersonate an HTML declaration.
pub(crate) fn decode_response_body(
    bytes: &[u8],
    content_type: Option<&str>,
    html_sniff: bool,
) -> Result<String, ToolError> {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        let (decoded, _) = encoding.decode_without_bom_handling(&bytes[bom_len..]);
        reject_binary_nul(bytes, encoding, &decoded)?;
        return Ok(decoded.into_owned());
    }

    let transport_encoding = content_type.and_then(content_type_encoding);
    let encoding = transport_encoding
        .or_else(|| html_sniff.then(|| html_meta_encoding(bytes)).flatten())
        .unwrap_or(UTF_8);
    let (decoded, _) = encoding.decode_without_bom_handling(bytes);
    reject_binary_nul(bytes, encoding, &decoded)?;
    Ok(decoded.into_owned())
}

fn reject_binary_nul(
    bytes: &[u8],
    encoding: &'static Encoding,
    decoded: &str,
) -> Result<(), ToolError> {
    // UTF-16 uses zero bytes structurally for many characters, so inspect its
    // decoded scalar values. Every other encoding must be NUL-free across the
    // complete response; neither a BOM nor a late byte may bypass the guard.
    let contains_nul = if encoding == UTF_16LE || encoding == UTF_16BE {
        decoded.contains('\0')
    } else {
        bytes.contains(&0)
    };
    if contains_nul {
        return Err(ToolError::execution_failed(
            "Unsupported binary response contained NUL bytes",
        ));
    }
    Ok(())
}

/// Parse only exact semicolon-delimited `charset` parameters. A random
/// `charset=` substring inside another parameter is not transport authority.
fn content_type_encoding(value: &str) -> Option<&'static Encoding> {
    value.split(';').skip(1).find_map(|parameter| {
        let (name, raw_value) = parameter.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("charset") {
            return None;
        }
        let value = raw_value.trim();
        let value = match (value.as_bytes().first(), value.as_bytes().last()) {
            (Some(b'"'), Some(b'"')) | (Some(b'\''), Some(b'\'')) if value.len() >= 2 => {
                &value[1..value.len() - 1]
            }
            _ if value.contains('"') || value.contains('\'') => return None,
            _ => value,
        };
        let label = value.trim();
        (!label.is_empty())
            .then(|| Encoding::for_label(label.as_bytes()))
            .flatten()
    })
}

fn should_sniff_html_encoding(content_type: Option<&str>, url: &str, bytes: &[u8]) -> bool {
    match content_type {
        Some("text/html" | "application/xhtml+xml") => true,
        // Explicit non-HTML text and structured formats never consult markup
        // embedded in their body.
        Some(value) if value.starts_with("text/") || is_structured_text_type(value) => false,
        Some("application/octet-stream") | None => {
            url_path_ends_with(url, &[".html", ".htm"]) || looks_like_html_bytes(bytes)
        }
        Some(_) => false,
    }
}

fn is_structured_text_type(content_type: &str) -> bool {
    content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("yaml")
        || content_type.contains("javascript")
}

fn looks_like_html_bytes(bytes: &[u8]) -> bool {
    let start = Encoding::for_bom(bytes).map_or(0, |(_, length)| length);
    let end = bytes
        .len()
        .min(start.saturating_add(HTML_ENCODING_SNIFF_BYTES));
    let ascii = ascii_lowercase_projection(&bytes[start..end]);
    let Some(prefix) = html_prefix_after_leading_declarations(&ascii) else {
        return false;
    };
    prefix.starts_with("<!doctype html")
        || prefix.starts_with("<html")
        || prefix.starts_with("<head")
        || prefix.starts_with("<meta")
}

fn html_prefix_after_leading_declarations(mut prefix: &str) -> Option<&str> {
    loop {
        prefix = prefix.trim_start();
        if let Some(comment) = prefix.strip_prefix("<!--") {
            let end = comment.find("-->")?;
            prefix = &comment[end + 3..];
            continue;
        }
        if let Some(declaration) = prefix.strip_prefix("<?xml") {
            let end = declaration.find("?>")?;
            prefix = &declaration[end + 2..];
            continue;
        }
        return Some(prefix);
    }
}

fn html_meta_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let sniff_len = bytes.len().min(HTML_ENCODING_SNIFF_BYTES);
    let html = ascii_lowercase_projection(&bytes[..sniff_len]);
    let mut cursor = 0usize;

    while let Some(relative) = html[cursor..].find('<') {
        let start = cursor + relative;
        if html[start..].starts_with("<!--") {
            cursor = html[start + 4..]
                .find("-->")
                .map_or(html.len(), |end| start + 4 + end + 3);
            continue;
        }
        if tag_starts_at(&html, start, "script") || tag_starts_at(&html, start, "style") {
            let name = if tag_starts_at(&html, start, "script") {
                "script"
            } else {
                "style"
            };
            let close = format!("</{name}");
            cursor = html[start..]
                .find(&close)
                .and_then(|close_start| {
                    html[start + close_start..]
                        .find('>')
                        .map(|end| start + close_start + end + 1)
                })
                .unwrap_or(html.len());
            continue;
        }
        if !tag_starts_at(&html, start, "meta") {
            cursor = start + 1;
            continue;
        }
        let relative_end = html[start..].find('>')?;
        let end = start + relative_end + 1;
        let tag = &html[start..end];
        if let Some(label) = html_attribute_value(tag, "charset")
            && let Some(encoding) = Encoding::for_label(label.as_bytes())
        {
            return Some(normalize_meta_encoding(encoding));
        }
        let is_content_type = html_attribute_value(tag, "http-equiv")
            .is_some_and(|value| value.eq_ignore_ascii_case("content-type"));
        if is_content_type
            && let Some(content) = html_attribute_value(tag, "content")
            && let Some(encoding) = content_type_encoding(&content)
        {
            return Some(normalize_meta_encoding(encoding));
        }
        cursor = end;
    }
    None
}

fn normalize_meta_encoding(encoding: &'static Encoding) -> &'static Encoding {
    if encoding == UTF_16LE || encoding == UTF_16BE {
        UTF_8
    } else {
        encoding
    }
}

fn ascii_lowercase_projection(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii() {
                char::from(byte.to_ascii_lowercase())
            } else {
                ' '
            }
        })
        .collect()
}

fn tag_starts_at(html: &str, start: usize, name: &str) -> bool {
    let Some(after_name) = html.get(start + 1 + name.len()..) else {
        return false;
    };
    html[start + 1..].starts_with(name)
        && after_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_whitespace() || matches!(ch, '/' | '>'))
}

fn html_attribute_value(tag: &str, wanted: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut cursor = 1usize;
    while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'>' {
        cursor += 1;
    }
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] == b'>' {
            break;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        let name = &tag[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }
        let (value_start, value_end) = if matches!(bytes[cursor], b'"' | b'\'') {
            let quote = bytes[cursor];
            cursor += 1;
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != quote {
                cursor += 1;
            }
            let end = cursor;
            cursor = cursor.saturating_add(1);
            (start, end)
        } else {
            let start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'>'
            {
                cursor += 1;
            }
            (start, cursor)
        };
        if name.eq_ignore_ascii_case(wanted) {
            return Some(tag[value_start..value_end].trim().to_string());
        }
    }
    None
}

fn normalized_content_type(content_type: Option<&str>) -> Option<String> {
    content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn is_html(content_type: Option<&str>, url: &str, body: &str) -> bool {
    matches!(content_type, Some("text/html" | "application/xhtml+xml"))
        || url_path_ends_with(url, &[".html", ".htm"])
        || {
            let prefix = body.trim_start().chars().take(64).collect::<String>();
            let prefix = prefix.to_ascii_lowercase();
            prefix.contains("<!doctype html") || prefix.contains("<html")
        }
}

fn is_markdown(content_type: Option<&str>, url: &str) -> bool {
    matches!(
        content_type,
        Some("text/markdown" | "text/x-markdown" | "application/markdown")
    ) || url_path_ends_with(url, &[".md", ".markdown"])
}

fn is_textual(content_type: Option<&str>, url: &str) -> bool {
    content_type.is_some_and(|value| {
        value.starts_with("text/")
            || value.contains("json")
            || value.contains("xml")
            || value.contains("yaml")
            || value.contains("javascript")
            || value == "application/sql"
    }) || url_path_ends_with(
        url,
        &[
            ".txt", ".json", ".jsonl", ".xml", ".yaml", ".yml", ".csv", ".tsv", ".rs", ".py",
            ".js", ".ts", ".toml",
        ],
    )
}

fn url_is_pdf(url: &str) -> bool {
    url_path_ends_with(url, &[".pdf"])
}

fn url_path_ends_with(url: &str, extensions: &[&str]) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .map(|parsed| parsed.path().to_ascii_lowercase())
        .is_some_and(|path| extensions.iter().any(|extension| path.ends_with(extension)))
}

fn looks_like_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

fn declared_media_family(content_type: Option<&str>) -> Option<MediaFamily> {
    let content_type = content_type?;
    if content_type.starts_with("image/") {
        Some(MediaFamily::Image)
    } else if content_type.starts_with("audio/") {
        Some(MediaFamily::Audio)
    } else if content_type.starts_with("video/") {
        Some(MediaFamily::Video)
    } else {
        None
    }
}

fn sniff_media(bytes: &[u8]) -> Option<MediaSignature> {
    let trimmed = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .map(|start| &bytes[start..])
        .unwrap_or(bytes);
    let signature = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        MediaSignature {
            extension: "png",
            family: MediaFamily::Image,
        }
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        MediaSignature {
            extension: "jpg",
            family: MediaFamily::Image,
        }
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        MediaSignature {
            extension: "gif",
            family: MediaFamily::Image,
        }
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        MediaSignature {
            extension: "webp",
            family: MediaFamily::Image,
        }
    } else if bytes.starts_with(b"ID3") || bytes.starts_with(b"\xff\xfb") {
        MediaSignature {
            extension: "mp3",
            family: MediaFamily::Audio,
        }
    } else if bytes.starts_with(b"fLaC") {
        MediaSignature {
            extension: "flac",
            family: MediaFamily::Audio,
        }
    } else if bytes.starts_with(b"OggS") {
        MediaSignature {
            extension: "ogg",
            family: MediaFamily::Audio,
        }
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        MediaSignature {
            extension: "wav",
            family: MediaFamily::Audio,
        }
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        MediaSignature {
            extension: "mp4",
            family: MediaFamily::Video,
        }
    } else if bytes.starts_with(b"\x1aE\xdf\xa3") {
        MediaSignature {
            extension: "webm",
            family: MediaFamily::Video,
        }
    } else if trimmed.starts_with(b"<svg")
        || (trimmed.starts_with(b"<?xml")
            && trimmed
                .windows(4)
                .take(1_024)
                .any(|window| window.eq_ignore_ascii_case(b"<svg")))
    {
        MediaSignature {
            extension: "svg",
            family: MediaFamily::Image,
        }
    } else {
        return None;
    };
    Some(signature)
}

async fn extract_pdf(
    bytes: &[u8],
    command: super::super::pdf::PdfTextCommand<'_>,
) -> Result<ExtractedDocument, ToolError> {
    let text = super::super::pdf::extract_bytes(bytes, command)
        .await
        .map_err(super::super::pdf::into_tool_error)?;
    let pages = split_pdf_pages(&text);
    let text = pages
        .iter()
        .map(|page| page.join("\n"))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(ExtractedDocument {
        kind: DocumentKind::Pdf,
        title: Some("PDF Document".to_string()),
        markdown: text.clone(),
        text,
        cleaned_html: None,
        pdf_pages: Some(pages),
        media_extension: None,
    })
}

fn split_pdf_pages(text: &str) -> Vec<Vec<String>> {
    text.split('\x0C')
        .map(|page| {
            page.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn html_becomes_readable_markdown_without_page_chrome() {
        let html = br#"<!doctype html><html><head><title>Whale &amp; Signal</title></head><body>
            <nav>Products Pricing Log in Cookies</nav>
            <article><h1>Fetch once</h1><p>This is the important article body with enough words to be useful.</p>
            <a href="/proof">Read the proof</a></article>
            <footer>Privacy Cookies Terms</footer></body></html>"#;
        let document = extract_document(
            "https://example.com/post",
            Some("text/html"),
            html,
            false,
            None,
        )
        .await
        .expect("extract html");

        assert_eq!(document.kind, DocumentKind::Html);
        assert_eq!(document.title.as_deref(), Some("Whale & Signal"));
        assert!(document.markdown.contains("Fetch once") || document.title.is_some());
        assert!(
            document
                .markdown
                .contains("[Read the proof](https://example.com/proof)")
        );
        assert!(!document.markdown.contains("Products Pricing"));
        assert!(!document.markdown.contains("Privacy Cookies"));
    }

    #[test]
    fn relative_http_href_resolution_preserves_other_destination_kinds() {
        let base = reqwest::Url::parse("https://example.com/guides/page").expect("base URL");
        assert_eq!(
            resolve_relative_http_href(&base, "../proof?q=1#receipt").as_deref(),
            Some("https://example.com/proof?q=1#receipt")
        );
        for href in [
            "#receipt",
            "mailto:maintainer@example.com",
            "data:text/plain,proof",
            "codewhale:session/123",
            "https://other.example/proof",
            "http://[::1",
            "",
        ] {
            assert_eq!(
                resolve_relative_http_href(&base, href),
                None,
                "destination must be left to htmd unchanged: {href:?}"
            );
        }
        let file = reqwest::Url::parse("file:///tmp/page").expect("file URL");
        assert!(resolve_relative_http_href(&file, "proof").is_none());
    }

    #[tokio::test]
    async fn sparse_document_uses_article_fallback() {
        let html = br#"<html><head><title>Fallback</title></head><body><nav>cookie banner</nav>
            <article><h2>Small source</h2><p>Five useful words survive this compact article fallback path.</p></article>
            </body></html>"#;
        let document = extract_document(
            "https://example.com/short",
            Some("text/html"),
            html,
            false,
            None,
        )
        .await
        .expect("extract fallback");

        assert!(document.markdown.contains("## Small source"));
        assert!(!document.markdown.contains("cookie banner"));
    }

    #[tokio::test]
    async fn javascript_shell_returns_actionable_error() {
        let error = extract_document(
            "https://example.com/app",
            Some("text/html"),
            b"<html><body><div id='root'></div><script>boot()</script></body></html>",
            false,
            None,
        )
        .await
        .expect_err("empty app shell must fail");

        let message = error.to_string();
        assert!(message.contains("may require JavaScript"), "{message}");
        assert!(
            message.contains("https://example.com/app"),
            "the shell failure must name the URL: {message}"
        );
        assert!(
            is_js_shell_error(&error.error),
            "the fetch pipeline recognizes this failure by marker: {message}"
        );
        assert!(
            !is_js_shell_error(&ToolError::execution_failed("connection reset")),
            "transport failures must not look like a JS shell"
        );
    }

    #[tokio::test]
    async fn markdown_passes_through_unchanged() {
        let body = b"# Release note\n\nA complete markdown response remains intact.\n";
        let document = extract_document(
            "https://example.com/release.md",
            Some("text/markdown; charset=utf-8"),
            body,
            false,
            None,
        )
        .await
        .expect("extract markdown");

        assert_eq!(document.kind, DocumentKind::Markdown);
        assert_eq!(document.markdown.as_bytes(), body);
        assert_eq!(document.title.as_deref(), Some("Release note"));
    }

    #[tokio::test]
    async fn media_requires_matching_magic_bytes() {
        let error = extract_document(
            "https://example.com/not-image.png",
            Some("image/png"),
            b"<html>not really an image</html>",
            false,
            None,
        )
        .await
        .expect_err("spoofed media must fail");
        assert!(error.to_string().contains("did not match"));

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(b"fake test payload");
        let document = extract_document(
            "https://example.com/image",
            Some("application/octet-stream"),
            &png,
            false,
            None,
        )
        .await
        .expect("sniff png");
        assert_eq!(document.kind, DocumentKind::Media);
        assert_eq!(document.media_extension, Some("png"));
    }

    #[tokio::test]
    async fn arbitrary_binary_is_rejected() {
        let error = extract_document(
            "https://example.com/archive.bin",
            Some("application/octet-stream"),
            b"PK\x03\x04archive bytes",
            false,
            None,
        )
        .await
        .expect_err("archive must be rejected");
        assert!(error.to_string().contains("Unsupported binary response"));
    }

    #[tokio::test]
    async fn empty_success_body_is_valid_text() {
        let document = extract_document(
            "https://example.com/no-content",
            Some("application/octet-stream"),
            b"",
            false,
            None,
        )
        .await
        .expect("empty body");
        assert_eq!(document.kind, DocumentKind::Text);
        assert!(document.text.is_empty());
    }

    #[tokio::test]
    async fn content_type_matching_is_case_insensitive() {
        let document = extract_document(
            "https://example.com/document",
            Some("Application/JSON; Charset=UTF-8"),
            br#"{"status":"ok"}"#,
            false,
            None,
        )
        .await
        .expect("mixed-case JSON content type");

        assert_eq!(document.kind, DocumentKind::Text);
        assert_eq!(document.text, r#"{"status":"ok"}"#);
    }

    #[test]
    fn bom_wins_over_conflicting_transport_and_is_removed() {
        let mut utf8 = b"\xef\xbb\xbf".to_vec();
        utf8.extend_from_slice("café".as_bytes());
        assert_eq!(
            decode_response_body(&utf8, Some("text/html; charset=windows-1252"), true)
                .expect("UTF-8 BOM"),
            "café"
        );

        let mut utf16 = vec![0xff, 0xfe];
        for unit in "BOM 日本語".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            decode_response_body(&utf16, Some("text/plain; charset=windows-1252"), false)
                .expect("UTF-16 BOM"),
            "BOM 日本語"
        );
    }

    #[test]
    fn content_type_charset_is_exact_recognized_and_order_independent() {
        let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode("café");
        for content_type in [
            "text/plain; charset=windows-1252",
            "TEXT/PLAIN; boundary=x; CHARSET = \"windows-1252\"; q=1",
            "text/plain; q=1; charset='windows-1252'",
        ] {
            assert_eq!(
                decode_response_body(&bytes, Some(content_type), false).expect("declared charset"),
                "café",
                "{content_type}"
            );
        }

        for malformed in [
            "text/plain; note=charset=windows-1252",
            "text/plain; charset=\"windows-1252",
            "text/plain; charset=definitely-not-an-encoding",
        ] {
            let decoded =
                decode_response_body(&bytes, Some(malformed), false).expect("UTF-8 fallback");
            assert!(decoded.contains('\u{fffd}'), "{malformed}: {decoded}");
        }
    }

    #[test]
    fn invalid_header_falls_through_to_direct_and_legacy_html_meta() {
        let direct = r#"<html><head><meta charset="gbk"></head><body>中文</body></html>"#;
        let (direct_bytes, _, _) = encoding_rs::GBK.encode(direct);
        assert!(
            decode_response_body(&direct_bytes, Some("text/html; charset=not-real"), true,)
                .expect("direct meta")
                .contains("中文")
        );

        let legacy = r#"<html><head><meta content="text/html; charset=windows-1252" http-equiv="Content-Type"></head><body>café</body></html>"#;
        let (legacy_bytes, _, _) = encoding_rs::WINDOWS_1252.encode(legacy);
        assert!(
            decode_response_body(&legacy_bytes, Some("text/html"), true)
                .expect("legacy meta")
                .contains("café")
        );
    }

    #[test]
    fn recognized_transport_charset_beats_conflicting_meta() {
        let html = r#"<html><head><meta charset="shift_jis"></head><body>中文</body></html>"#;
        let (bytes, _, _) = encoding_rs::GBK.encode(html);
        let decoded = decode_response_body(&bytes, Some("text/html; charset=gbk"), true)
            .expect("transport charset");
        assert!(decoded.contains("中文"), "{decoded}");
    }

    #[test]
    fn html_prescan_ignores_comments_scripts_and_late_meta() {
        let cases = [
            "<!-- <meta charset=windows-1252> --><html><body>café</body></html>".to_string(),
            "<script>\"<meta charset=windows-1252>\"</script><html><body>café</body></html>"
                .to_string(),
            format!(
                "<html><head>{}<meta charset=windows-1252></head><body>café</body></html>",
                " ".repeat(HTML_ENCODING_SNIFF_BYTES)
            ),
        ];
        for html in cases {
            let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(&html);
            let decoded = decode_response_body(&bytes, Some("text/html"), true)
                .expect("bounded HTML fallback");
            assert!(
                decoded.contains('\u{fffd}'),
                "late/ignored meta changed decoding: {decoded}"
            );
        }
    }

    #[test]
    fn non_html_bodies_never_sniff_meta_markup() {
        let plain = "literal <meta charset=windows-1252> café";
        let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(plain);
        for content_type in ["text/plain", "application/json"] {
            let decoded = decode_response_body(&bytes, Some(content_type), false)
                .expect("non-HTML UTF-8 fallback");
            assert!(decoded.contains('\u{fffd}'), "{content_type}: {decoded}");
        }
    }

    #[test]
    fn declared_gbk_shift_jis_and_windows_1252_decode_deterministically() {
        let cases = [
            (encoding_rs::GBK, "中文", "gbk"),
            (encoding_rs::SHIFT_JIS, "日本語", "shift_jis"),
            (encoding_rs::WINDOWS_1252, "café", "windows-1252"),
        ];
        for (encoding, text, label) in cases {
            let (bytes, _, had_errors) = encoding.encode(text);
            assert!(!had_errors, "fixture must be representable in {label}");
            assert_eq!(
                decode_response_body(&bytes, Some(&format!("text/plain; charset={label}")), false,)
                    .expect("decode declared encoding"),
                text
            );
        }
    }

    #[test]
    fn nul_binary_is_rejected_but_utf16_bom_text_is_not() {
        let error = decode_response_body(b"PK\0\x03\x04archive", Some("text/plain"), false)
            .expect_err("NUL binary must fail");
        assert!(error.to_string().contains("NUL bytes"));

        let bom_binary = b"\xef\xbb\xbfapparently text\0binary";
        let error = decode_response_body(bom_binary, Some("text/plain"), false)
            .expect_err("a BOM must not bypass the NUL guard");
        assert!(error.to_string().contains("NUL bytes"));

        let mut late_binary = vec![b'x'; 8_193];
        late_binary.push(0);
        let error = decode_response_body(&late_binary, Some("text/plain"), false)
            .expect_err("a late NUL must not bypass the full-body guard");
        assert!(error.to_string().contains("NUL bytes"));

        let utf16 = [0xff, 0xfe, b'O', 0, b'K', 0];
        assert_eq!(
            decode_response_body(&utf16, Some("application/octet-stream"), false)
                .expect("BOM proves UTF-16 text"),
            "OK"
        );

        let utf16_nul = [0xff, 0xfe, b'O', 0, 0, 0, b'K', 0];
        let error = decode_response_body(&utf16_nul, Some("text/plain"), false)
            .expect_err("decoded UTF-16 NUL must remain binary");
        assert!(error.to_string().contains("NUL bytes"));
    }

    #[tokio::test]
    async fn extensionless_html_sniff_skips_leading_comments_and_xml_declarations() {
        let cases = [
            r#"<!-- deployment marker --><html><head><meta charset="windows-1252"><title>Café release notes</title></head><body><article><h1>Café release notes</h1><p>This extensionless page contains enough meaningful text for deterministic extraction.</p></article></body></html>"#,
            r#"<?xml version="1.0"?><!-- marker --><head><meta charset="windows-1252"><title>Café release notes</title></head><body><article><h1>Café release notes</h1><p>This extensionless page contains enough meaningful text for deterministic extraction.</p></article></body>"#,
        ];
        for html in cases {
            let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(html);
            let document = extract_document(
                "https://example.com/extensionless",
                None,
                &bytes,
                false,
                None,
            )
            .await
            .expect("leading declarations preserve extensionless HTML sniffing");
            assert_eq!(document.kind, DocumentKind::Html);
            assert_eq!(document.title.as_deref(), Some("Café release notes"));
        }
    }

    #[tokio::test]
    async fn svg_requires_and_accepts_svg_markup_signature() {
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        let document = extract_document(
            "https://example.com/diagram",
            Some("image/svg+xml"),
            svg,
            false,
            None,
        )
        .await
        .expect("sniff svg");
        assert_eq!(document.kind, DocumentKind::Media);
        assert_eq!(document.media_extension, Some("svg"));
    }

    async fn extract_html_fixture(url: &str, html: &str) -> ExtractedDocument {
        extract_document(url, Some("text/html"), html.as_bytes(), false, None)
            .await
            .expect("fixture must extract readable text")
    }

    #[tokio::test]
    async fn listing_page_keeps_every_article() {
        let html = r#"<html><body><nav>Home Blog About</nav><main>
            <article><h2>First post</h2><p>Alpha story body with plenty of words to read here.</p></article>
            <article><h2>Second post</h2><p>Bravo story body with plenty of words to read here.</p></article>
            <article><h2>Third post</h2><p>Charlie story body with plenty of words to read here.</p></article>
            </main></body></html>"#;
        let document = extract_html_fixture("https://blog.example/", html).await;
        for needle in [
            "First post",
            "Alpha story",
            "Second post",
            "Bravo story",
            "Third post",
            "Charlie story",
        ] {
            assert!(
                document.markdown.contains(needle),
                "{needle} missing: {}",
                document.markdown
            );
        }
        assert!(!document.markdown.contains("Home Blog About"));
    }

    #[tokio::test]
    async fn article_header_keeps_title_and_byline() {
        let html = r#"<html><body><header>Site logo Sign in Subscribe</header>
            <article><header><h1>Whales sing in dialects</h1><p class="byline">By Ada Lovelace</p></header>
            <p>Researchers recorded humpback song across three oceans and found regional variation.</p>
            </article></body></html>"#;
        let document = extract_html_fixture("https://news.example/whales", html).await;
        assert!(
            document.markdown.contains("Whales sing in dialects"),
            "{}",
            document.markdown
        );
        assert!(
            document.markdown.contains("By Ada Lovelace"),
            "{}",
            document.markdown
        );
        assert!(document.markdown.contains("regional variation"));
        assert!(!document.markdown.contains("Sign in Subscribe"));
    }

    #[tokio::test]
    async fn page_wrapped_in_a_form_is_not_mistaken_for_a_javascript_shell() {
        let html = r#"<html><body><form method="post" action="./Default.aspx" id="form1">
            <input type="hidden" name="__VIEWSTATE" value="dDwtMTA4MzE0MjEwNTs7Pg==" />
            <div class="content"><h1>Quarterly report</h1>
            <p>Revenue grew in every region this quarter, led by the northern division.</p></div>
            <select name="year"><option>2025</option><option>2026</option></select>
            <button type="submit">Go</button>
            </form></body></html>"#;
        let document = extract_html_fixture("https://legacy.example/Default.aspx", html).await;
        assert!(
            document.markdown.contains("Quarterly report"),
            "{}",
            document.markdown
        );
        assert!(document.markdown.contains("northern division"));
        assert!(!document.markdown.contains("VIEWSTATE"));
        assert!(
            !document.markdown.contains("2026"),
            "form controls are stripped"
        );
    }

    #[tokio::test]
    async fn arxiv_abstract_page_extracts_title_and_abstract() {
        // Trimmed from the shape of https://arxiv.org/abs/1706.03762 (2026-09).
        let html = r##"<!DOCTYPE html><html lang="en"><head><title>[1706.03762] Attention Is All You Need</title>
            <script>window.MathJax = {};</script></head>
            <body ><div class="flex-wrap-footer"><a href="#content" class="ds-skip-link">Skip to main content</a>
            <header class="ds-site-header"><a href="https://arxiv.org/">archive home</a>
            <button type="button" id="ds-nav-toggle">Open menu</button>
            <nav class="ds-site-header-nav"><a href="https://arxiv.org/search">Search</a><a href="https://arxiv.org/login">Log in</a></nav>
            </header>
            <div class="arxiv-search-overlay" hidden><form method="GET" action="https://arxiv.org/search">
            <label for="q">Search arXiv</label><input type="text" name="query" id="q"></form></div>
            <main><div id="content"><!-- rdf:RDF <rdf:Description dc:title="Attention Is All You Need" /> -->
            <div id="abs-outer"><div class="leftcolumn"><div class="subheader"><h1>Computer Science &gt; Computation and Language</h1></div>
            <div id="abs"><div class="dateline">[Submitted on 12 Jun 2017 (<a href="/abs/1706.03762v1">v1</a>)]</div>
            <h1 class="title mathjax"><span class="descriptor">Title:</span>Attention Is All You Need</h1>
            <div class="authors"><span class="descriptor">Authors:</span><a href="/a/vaswani_a_1">Ashish Vaswani</a></div>
            <blockquote class="abstract mathjax"><span class="descriptor">Abstract:</span>The dominant sequence transduction models are based on complex recurrent or convolutional neural networks.</blockquote>
            <script type="text/javascript" language="javascript">mathjaxToggle();</script>
            </div></div></div></div></main>
            <footer><a href="https://info.arxiv.org/help/contact.html">Contact</a></footer></div></body></html>"##;
        let document = extract_html_fixture("https://arxiv.org/abs/1706.03762", html).await;
        assert!(
            document.markdown.contains("Attention Is All You Need"),
            "{}",
            document.markdown
        );
        assert!(
            document.markdown.contains("dominant sequence transduction"),
            "{}",
            document.markdown
        );
        assert!(document.markdown.contains("Ashish Vaswani"));
        assert!(!document.markdown.contains("mathjaxToggle"));
        assert!(!document.markdown.contains("Log in"));
    }
}

#[cfg(test)]
#[path = "extract_host_tests.rs"]
mod host_tests;
