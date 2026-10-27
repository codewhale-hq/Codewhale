//! The allowlisted safe-label boundary for `/preview-request` (#1004).
//!
//! Every free-form string that reaches a manifest surface — human table or
//! JSON — crosses this module first. Nothing here is a "scrubber" that tries
//! to find secrets in arbitrary text: a value either matches a narrow
//! allowlist and is published verbatim, or it is replaced by a stable
//! `sha256:<12 hex>` fingerprint. Two previews of the same route still
//! compare equal, and nothing that was not on the allowlist is ever printed.
//!
//! Why this exists at all: the obvious "identifier" fields are not safe by
//! construction. A custom `[providers.<name>]` key is user-authored text, and
//! a model id can be a filesystem path (`/models/llama-3.gguf`), a URL, a URL
//! path, or a deployment id that is itself a credential. Bounding the *shape*
//! of what may be printed is the only way to keep those out of a manifest a
//! user will paste into an issue tracker.
//!
//! Error strings get the same treatment through [`safe_error_text`], which is
//! path- and URL-path-safe: an MCP or request-preparation failure often
//! carries an absolute workspace path or an endpoint URL, and neither may
//! reach the transcript.

use serde::{Serialize, Serializer};

/// Longest identifier published verbatim. Real provider/model/route ids are
/// far shorter; anything longer is treated as opaque payload.
const MAX_IDENTIFIER_LEN: usize = 64;
/// Longest short phrase (labels with spaces, e.g. a billing presentation).
const MAX_PHRASE_LEN: usize = 80;
/// Longest error sentence published. Errors are truncated, never wrapped.
const MAX_ERROR_LEN: usize = 200;
/// A run of this many characters from a single "opaque" alphabet reads as a
/// key, token, or hash rather than as a name.
const OPAQUE_RUN_LEN: usize = 20;
/// Hex prefix length used when a value is replaced by its fingerprint.
const FINGERPRINT_HEX_LEN: usize = 12;

/// A string that is safe to publish on a manifest surface.
///
/// Construct with [`SafeLabel::identifier`], [`SafeLabel::catalog_model`], or
/// [`SafeLabel::phrase`]; all fall back to a fingerprint when the input is not
/// on the allowlist. There is deliberately no constructor that takes
/// arbitrary text verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeLabel {
    text: String,
    redacted: bool,
}

impl SafeLabel {
    /// A generic identifier-shaped value: provider id, route id, reasoning
    /// tier. Allows `A-Z a-z 0-9 . _ : - + @` and rejects every slash. Model
    /// ids with a slash must use [`Self::catalog_model`] instead.
    pub fn identifier(raw: &str) -> Self {
        let trimmed = raw.trim();
        if identifier_is_allowlisted(trimmed) {
            Self {
                text: trimmed.to_string(),
                redacted: false,
            }
        } else {
            Self::fingerprint(raw)
        }
    }

    /// A model label. Slash-bearing values are published only when the exact
    /// id exists in the active local model catalog; a vendor-looking prefix is
    /// never authority by itself. Non-slash ids retain the generic identifier
    /// boundary for custom compatible deployments.
    pub fn catalog_model(raw: &str) -> Self {
        let trimmed = raw.trim();
        if !trimmed.contains('/') {
            return Self::identifier(raw);
        }
        if catalog_model_identifier_is_allowlisted(trimmed) {
            Self {
                text: trimmed.to_string(),
                redacted: false,
            }
        } else {
            Self::fingerprint(raw)
        }
    }

    /// A short human phrase: the same allowlist plus spaces, parentheses, and
    /// commas, for host-supplied presentation labels.
    pub fn phrase(raw: &str) -> Self {
        let trimmed = raw.trim();
        if phrase_is_allowlisted(trimmed) {
            Self {
                text: trimmed.to_string(),
                redacted: false,
            }
        } else {
            Self::fingerprint(raw)
        }
    }

    /// Replace a value with a stable fingerprint of its exact bytes.
    fn fingerprint(raw: &str) -> Self {
        let digest = crate::hashing::sha256_hex(raw.as_bytes());
        Self {
            text: format!("sha256:{}", &digest[..FINGERPRINT_HEX_LEN]),
            redacted: true,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// True when the original value failed the allowlist and only its
    /// fingerprint is being published.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }
}

impl std::fmt::Display for SafeLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for SafeLabel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

fn identifier_is_allowlisted(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_LEN {
        return false;
    }
    if value.starts_with('/') || value.starts_with('~') || value.starts_with('.') {
        return false;
    }
    if value.contains("//") || value.contains("..") || value.contains(':') && value.contains('/') {
        return false;
    }
    if !value.chars().all(is_identifier_char) {
        return false;
    }
    if value.contains('/') {
        return false;
    }
    !looks_opaque(value)
}

fn catalog_model_identifier_is_allowlisted(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_LEN
        || value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with('.')
        || value.contains("//")
        || value.contains("..")
        || value.contains(':')
        || !value.chars().all(is_identifier_char)
        || looks_opaque(value)
    {
        return false;
    }
    codewhale_config::catalog::reviewed::public_model_identifier(value)
}

fn phrase_is_allowlisted(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_PHRASE_LEN {
        return false;
    }
    if value.contains('/') || value.contains('\\') || value.contains('~') {
        return false;
    }
    if !value
        .chars()
        .all(|ch| is_identifier_char(ch) || matches!(ch, ' ' | '(' | ')' | ','))
    {
        return false;
    }
    !looks_opaque(value)
}

fn is_identifier_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+' | '@' | ':' | '/')
}

/// Whether a value carries a key-, token-, or hash-shaped run.
///
/// Deliberately shape-based rather than a keyword list: `sk-`-style prefixes
/// are only one of the ways a deployment id can be a credential.
fn looks_opaque(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    for marker in ["sk-", "api_key", "apikey", "secret", "password", "bearer"] {
        if lower.contains(marker) {
            return true;
        }
    }
    let mut run = 0usize;
    for ch in value.chars() {
        // A long unbroken alphanumeric run with no separator is what base64
        // and hex payloads look like; real ids use `-`, `.`, or `/`.
        if ch.is_ascii_alphanumeric() {
            run += 1;
            if run >= OPAQUE_RUN_LEN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Longest single word published verbatim inside an error sentence.
const MAX_ERROR_WORD_LEN: usize = 40;
/// Longest scheme published from a URL-shaped token.
const MAX_SCHEME_LEN: usize = 16;
/// Longest `host[:port]` published from a URL-shaped token.
const MAX_HOST_LEN: usize = 80;
/// Stand-in for a word that is not on the error allowlist.
const REDACTED_WORD: &str = "<redacted>";
/// Stand-in for anything path-shaped.
const REDACTED_PATH: &str = "<path-redacted>";

/// Bound an error string so it can be shown in the transcript.
///
/// This is an **allowlist**, not a scrubber. Host error text is arbitrary: it
/// can interpolate a route id, a model id, a deployment path, a quoted server
/// name, a URL with a secret in its path, or a raw credential. Rather than
/// hunting for the bad parts, every whitespace-separated token must earn its
/// place:
///
/// - the config crate's secret redaction runs first;
/// - a token containing a control character is dropped entirely;
/// - a URL-shaped token keeps only `scheme://host[:port]`, and only when both
///   are themselves allowlisted — the path, query, fragment, and userinfo are
///   never published, because a deployment path can *be* the credential;
/// - a path-shaped token (POSIX absolute, `~/`, Windows drive, or anything
///   containing a backslash) collapses to `REDACTED_PATH`;
/// - a token carrying a quote character (`"`, `'`, or a backtick) is replaced
///   wholesale: quoted spans are where hostile identifiers hide;
/// - anything else must be a short, ordinary word — ASCII alphanumerics plus
///   `-`, `_`, `.`, bounded by `MAX_ERROR_WORD_LEN` and rejected by
///   `looks_opaque` — with only a small set of sentence punctuation allowed
///   at its edges. Everything else becomes `REDACTED_WORD`.
///
/// The result therefore contains no filesystem path, no URL path, no quoted
/// span, no token-shaped run, and no control character, and is truncated to
/// `MAX_ERROR_LEN`.
pub fn safe_error_text(raw: &str) -> String {
    let redacted = codewhale_config::persistence::redact_secrets(raw);
    let mut out = String::with_capacity(redacted.len().min(MAX_ERROR_LEN));
    let mut last_was_redacted = false;
    for token in redacted.split_whitespace() {
        let safe = safe_error_token(token);
        if safe.is_empty() {
            continue;
        }
        // Collapse runs of redactions: `<redacted> <redacted> <redacted>` is
        // noise, and its length would leak the shape of what was removed.
        let is_redacted = safe == REDACTED_WORD;
        if is_redacted && last_was_redacted {
            continue;
        }
        last_was_redacted = is_redacted;
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&safe);
    }
    if out.is_empty() {
        out.push_str("<unavailable>");
    }
    if out.len() > MAX_ERROR_LEN {
        out.truncate(
            (0..=MAX_ERROR_LEN)
                .rev()
                .find(|index| out.is_char_boundary(*index))
                .unwrap_or(0),
        );
        out.push('…');
    }
    out
}

fn safe_error_token(token: &str) -> String {
    if token.chars().any(char::is_control) {
        return REDACTED_WORD.to_string();
    }
    // A URL keeps its scheme and host and loses everything after it — but only
    // when the scheme and host are themselves ordinary.
    if let Some(scheme_end) = token.find("://") {
        return safe_url_token(token, scheme_end);
    }
    // Absolute and home-relative paths, plus Windows drive paths, collapse
    // entirely: a workspace path names the user's machine and project.
    let looks_like_path = token.starts_with('/')
        || token.starts_with("~/")
        || token.contains('\\')
        || (token.len() > 2 && token.as_bytes()[1] == b':' && token.contains('\\'));
    if looks_like_path {
        return REDACTED_PATH.to_string();
    }
    // Quoted spans are the classic carrier for a hostile server, route, or
    // model id. Never republish one, even partially.
    if token.contains(['"', '\'', '`']) {
        return REDACTED_WORD.to_string();
    }

    let (lead, core, trail) = split_sentence_punctuation(token);
    if core.is_empty() {
        // Pure punctuation: keep it only if every character is on the small
        // sentence-punctuation allowlist, which `split` already guaranteed.
        return format!("{lead}{trail}");
    }
    if error_word_is_allowlisted(core) {
        format!("{lead}{core}{trail}")
    } else {
        REDACTED_WORD.to_string()
    }
}

/// Collapse a URL-shaped token to `scheme://host[:port]/<path-redacted>`.
///
/// Userinfo, path, query, and fragment are dropped unconditionally. A scheme
/// or host that is not itself ordinary makes the whole token opaque rather
/// than publishing a hostile "host".
fn safe_url_token(token: &str, scheme_end: usize) -> String {
    let scheme = &token[..scheme_end];
    let rest = &token[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..authority_end].rsplit('@').next().unwrap_or("");

    let scheme_ok = !scheme.is_empty()
        && scheme.len() <= MAX_SCHEME_LEN
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'));
    let host_ok = !host.is_empty()
        && host.len() <= MAX_HOST_LEN
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':'))
        && !looks_opaque(host);
    if scheme_ok && host_ok {
        format!("{scheme}://{host}/{REDACTED_PATH}")
    } else {
        REDACTED_WORD.to_string()
    }
}

/// Sentence punctuation that may bracket an allowlisted word. Deliberately
/// excludes every quote character.
fn is_edge_punctuation(ch: char) -> bool {
    matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | '(' | ')')
}

/// Split leading/trailing sentence punctuation off a token.
///
/// Returns `("", token, "")` when the token carries punctuation that is not on
/// the edge allowlist, so the caller rejects it as a whole.
fn split_sentence_punctuation(token: &str) -> (&str, &str, &str) {
    let start = token
        .char_indices()
        .find(|(_, ch)| !is_edge_punctuation(*ch))
        .map_or(token.len(), |(index, _)| index);
    let end = token
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_edge_punctuation(*ch))
        .map_or(start, |(index, ch)| index + ch.len_utf8());
    (
        &token[..start],
        &token[start..end.max(start)],
        &token[end.max(start)..],
    )
}

/// Whether a bare word inside an error sentence may be published verbatim.
fn error_word_is_allowlisted(word: &str) -> bool {
    if word.is_empty() || word.len() > MAX_ERROR_WORD_LEN {
        return false;
    }
    if word.contains("..") {
        return false;
    }
    if !word
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return false;
    }
    !looks_opaque(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_identifiers_pass_through_verbatim() {
        for value in [
            "deepseek-chat",
            "claude-sonnet-4-5",
            "gpt-5-codex",
            "MiniMax-M3",
            "my-gateway",
            "kimi-k2-0905-preview",
        ] {
            let label = SafeLabel::identifier(value);
            assert_eq!(label.as_str(), value, "{value} must publish verbatim");
            assert!(!label.is_redacted(), "{value}");
        }
    }

    #[test]
    fn hostile_route_and_model_identifiers_never_reach_a_surface() {
        let hostile = [
            "/Users/someone/models/private-weights.gguf".to_string(),
            "~/.codewhale/config.toml".to_string(),
            "https://internal.example.com/v1/deployments/prod".to_string(),
            "C:\\Users\\someone\\models\\weights.bin".to_string(),
            ["sk", "-fixture-not-a-real-key-00000000"].concat(),
            "deployments/9f8e7d6c5b4a39281706abcdef012345".to_string(),
            "../../etc/passwd".to_string(),
            "model with spaces and a /path/inside".to_string(),
            ["api_key=sk", "-live-1234567890"].concat(),
            "src/lib.rs".to_string(),
            "config/prod".to_string(),
            "models/weights.gguf".to_string(),
            "foo/bar-baz".to_string(),
        ];
        for value in hostile {
            let label = SafeLabel::identifier(&value);
            assert!(label.is_redacted(), "`{value}` must not publish verbatim");
            assert!(label.as_str().starts_with("sha256:"), "{}", label.as_str());
            assert!(!label.as_str().contains('/'), "{}", label.as_str());
            assert!(!label.as_str().contains(' '), "{}", label.as_str());
        }
    }

    #[test]
    fn generic_identifiers_reject_all_slashes_and_catalog_models_require_exact_ids() {
        for path in [
            "src/lib.rs",
            "docs/PREVIEW_REQUEST.md",
            "config/prod",
            "models/llama-3.gguf",
            "foo/bar-baz",
        ] {
            assert!(
                SafeLabel::identifier(path).is_redacted(),
                "relative path `{path}` must not be published"
            );
        }

        let known = "qwen/qwen3.6-flash";
        assert!(SafeLabel::identifier(known).is_redacted());
        assert_eq!(SafeLabel::catalog_model(known).as_str(), known);
        for hostile in ["openai/secrets/config", "qwen/src/lib.rs"] {
            assert!(SafeLabel::identifier(hostile).is_redacted());
            assert!(SafeLabel::catalog_model(hostile).is_redacted());
        }
    }

    #[test]
    fn fingerprints_are_stable_and_distinguishing() {
        let first = SafeLabel::identifier("/models/a.gguf");
        let second = SafeLabel::identifier("/models/a.gguf");
        let other = SafeLabel::identifier("/models/b.gguf");
        assert_eq!(first, second);
        assert_ne!(first, other);
    }

    #[test]
    fn phrases_allow_spaces_but_not_paths() {
        assert_eq!(
            SafeLabel::phrase("Codex OAuth quota").as_str(),
            "Codex OAuth quota"
        );
        assert!(SafeLabel::phrase("/opt/quota/plan").is_redacted());
    }

    #[test]
    fn error_text_is_path_and_url_path_safe() {
        let raw = "MCP server 'x' failed: cannot spawn /Users/someone/work/repo/bin/server \
                   while calling https://gateway.internal.example.com/v1/secret-deployment/messages";
        let safe = safe_error_text(raw);
        assert!(!safe.contains("/Users/someone"), "{safe}");
        assert!(!safe.contains("/v1/secret-deployment"), "{safe}");
        assert!(safe.contains("<path-redacted>"), "{safe}");
        assert!(
            safe.contains("https://gateway.internal.example.com/<path-redacted>"),
            "{safe}"
        );
    }

    /// The error surface is where hostile text most easily reaches a
    /// transcript: preflight, MCP, and request-preparation failures all
    /// interpolate route ids, model ids, server names, and endpoints.
    #[test]
    fn hostile_error_text_never_publishes_the_hostile_part() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let hostile: Vec<String> = vec![
            "route 'prod-key-8f2a' rejected key sk-live-abcdef0123456789abcdef".to_string(),
            "cannot read C:\\Users\\someone\\.codewhale\\config.toml".to_string(),
            format!("cannot read {home}/.codewhale/config.toml"),
            "GET https://gw.example.com/v1/deployments/prod-key-8f2a?api_key=sk-1234567890abcdef failed".to_string(),
            "server \"my secret server\" refused: password=hunter2".to_string(),
            "model /Users/someone/models/private.gguf is unavailable".to_string(),
            "authorization: Bearer eyJhbGciFAKEFIXTUREnotasecret".to_string(),
            "endpoint http://10.0.0.5:8443/internal/deploy-9f8e7d6c5b4a3928 timed out".to_string(),
            format!("crash{}oops", '\u{7}'),
        ];
        for raw in &hostile {
            let safe = safe_error_text(raw);
            for forbidden in [
                "prod-key-8f2a",
                "sk-live-",
                "sk-1234567890",
                "/Users/someone",
                "C:\\Users",
                ".codewhale",
                "api_key=",
                "hunter2",
                "password=",
                "eyJhbGci",
                "/v1/deployments",
                "/internal/deploy",
                "private.gguf",
                "my secret server",
            ] {
                assert!(
                    !safe.contains(forbidden),
                    "`{forbidden}` leaked from `{raw}`:\n{safe}"
                );
            }
            assert!(!safe.contains(&home), "home leaked from `{raw}`:\n{safe}");
            assert!(!safe.contains('"'), "{safe}");
            assert!(!safe.contains('\''), "{safe}");
            assert!(!safe.contains('`'), "{safe}");
            assert!(!safe.chars().any(char::is_control), "{safe}");
        }
    }

    #[test]
    fn ordinary_error_words_survive_so_the_message_stays_useful() {
        let safe = safe_error_text("the shared route planner could not resolve this turn.");
        assert_eq!(
            safe, "the shared route planner could not resolve this turn.",
            "an allowlisted sentence must survive intact"
        );
    }

    #[test]
    fn a_url_with_a_hostile_authority_is_dropped_rather_than_half_published() {
        // The "host" here is a long opaque run — republishing it would be
        // republishing the secret the path redaction exists to remove.
        let safe = safe_error_text("calling https://9f8e7d6c5b4a39281706abcdef012345.example/x");
        assert!(!safe.contains("9f8e7d6c5b4a3928"), "{safe}");
        assert!(safe.contains("<redacted>"), "{safe}");
    }

    #[test]
    fn error_text_is_bounded_and_single_line() {
        let raw = format!("failure {}", "x".repeat(4_000));
        let safe = safe_error_text(&raw);
        assert!(safe.chars().count() <= MAX_ERROR_LEN + 1, "{}", safe.len());
        assert!(!safe.contains('\n'));
    }
}

const MAX_RECEIPT_FIELD_CHARS: usize = 96;

pub fn safe_receipt_field(raw: &str) -> String {
    let sanitized = sanitize_persisted_route_label(raw);
    let mut out = String::with_capacity(raw.len().min(MAX_RECEIPT_FIELD_CHARS));
    let mut previous_separator = false;
    for ch in sanitized.chars() {
        if out.chars().count() >= MAX_RECEIPT_FIELD_CHARS {
            break;
        }
        let safe = if ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/' | ':' | '+') {
            ch
        } else {
            '_'
        };
        let separator = safe == '_';
        if separator && previous_separator {
            continue;
        }
        out.push(safe);
        previous_separator = separator;
    }
    if out.is_empty() { "-".to_string() } else { out }
}

pub fn sanitize_persisted_route_label(raw: &str) -> String {
    const MAX_PERSISTED_ROUTE_LABEL_CHARS: usize = 256;
    let value = raw.trim();
    let lower = value.to_ascii_lowercase();

    if value.is_empty() {
        return "-".to_string();
    }

    // URLs are not route labels. Endpoints have a dedicated, validated hash
    // field; persisting a URL here risks leaking userinfo, query credentials,
    // or fragments through a custom provider/model name.
    if value.contains("://") {
        return "redacted-url".to_string();
    }

    let authorization_value = ["bearer ", "basic ", "digest ", "token ", "apikey "]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        || lower.contains("authorization:")
        || lower.contains("proxy-authorization:");
    if authorization_value {
        return "redacted-credential".to_string();
    }

    // Reject credential assignments regardless of common casing or separator:
    // FOO_API_KEY=..., access-token:..., password = ....
    for (index, ch) in value.char_indices() {
        if !matches!(ch, '=' | ':') {
            continue;
        }
        let name = lower[..index]
            .trim()
            .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '{' | '[' | ','));
        let name = name.rsplit([' ', ',', ';']).next().unwrap_or(name);
        let normalized = name.replace('-', "_");
        if normalized.ends_with("api_key")
            || normalized.ends_with("token")
            || normalized.ends_with("secret")
            || normalized.ends_with("password")
            || normalized.ends_with("passwd")
        {
            return "redacted-credential".to_string();
        }
    }

    // Common credential token prefixes. These are intentionally checked at
    // word boundaries so model ids containing an incidental "sk" survive.
    let credential_prefix = lower
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '=' | ':' | ',' | ';' | '"' | '\''))
        .filter(|part| !part.is_empty())
        .any(|part| {
            [
                "sk-",
                "sk_",
                "rk-",
                "pk-",
                "ghp_",
                "gho_",
                "ghu_",
                "ghs_",
                "github_pat_",
                "hf_",
                "glpat-",
                "xoxb-",
                "xoxp-",
                "xoxa-",
                "akia",
                "aiza",
                "eyj",
            ]
            .iter()
            .any(|prefix| part.starts_with(prefix))
        });
    if credential_prefix {
        return "redacted-credential".to_string();
    }

    let windows_absolute = value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .get(2)
            .is_some_and(|separator| matches!(separator, b'/' | b'\\'));
    let contains_local_root = [
        "/users/",
        "/volumes/",
        "/home/",
        "/private/",
        "\\users\\",
        "file://",
        "/.ssh/",
        "\\.ssh\\",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let looks_like_relative_path = value.contains('\\')
        || lower.starts_with(".ssh/")
        || lower.starts_with(".ssh\\")
        || lower.split('/').any(|segment| {
            matches!(
                segment,
                "." | ".."
                    | ".ssh"
                    | ".config"
                    | "secrets"
                    | "secret"
                    | "credentials"
                    | "credential"
                    | "relative"
                    | "workspace"
                    | "tmp"
            )
        });
    if std::path::Path::new(value).is_absolute()
        || windows_absolute
        || value.starts_with("~/")
        || value.starts_with("./")
        || value.starts_with("../")
        || contains_local_root
        || looks_like_relative_path
    {
        return "redacted-local-path".to_string();
    }
    let bounded: String = value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_PERSISTED_ROUTE_LABEL_CHARS)
        .collect();
    if bounded.is_empty() {
        "-".to_string()
    } else {
        bounded
    }
}
