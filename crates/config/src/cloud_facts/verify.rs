//! Envelope verification: size cap → shape → pinned key → Ed25519 → payload
//! parse → cross-checks → channel → schema → version scope → rollback →
//! expiry. Nothing in the payload is trusted before the signature verifies.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use super::keys::{
    AUTOMATION_CHANNEL_SUFFIX, AUTOMATION_MAX_MODELS, AUTOMATION_MAX_VALIDITY_SECS, DOMAIN,
    ENVELOPE_VERSION, KeyRole, KeyStatus, MAX_ENVELOPE_BYTES, MAX_PAYLOAD_BYTES,
    SUPPORTED_SCHEMA_VERSION, TrustedKey, trusted_key,
};
use super::types::{CloudFacts, ModelFact, ModelOp};

/// Grace after `not_after` before facts downgrade to `stale` (48 h).
pub const NOT_AFTER_GRACE_SECS: u64 = 48 * 60 * 60;
pub const FUTURE_CLOCK_TOLERANCE_SECS: u64 = 300;
const MAX_SIGNATURES: usize = 8;

/// The transport envelope as served by `/api/facts/v1/<channel>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub envelope: u64,
    pub channel: String,
    pub facts_version: u64,
    #[serde(default)]
    pub schema_version: Option<u32>,
    pub key_id: String,
    pub alg: String,
    #[serde(default)]
    pub applies_to: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
    pub payload_b64: String,
    pub sig_b64: String,
    #[serde(default)]
    pub sigs: Vec<ExtraSignature>,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Additional `(key_id, sig)` pairs carried during key rotation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraSignature {
    pub key_id: String,
    pub sig_b64: String,
}

/// Why an envelope was not accepted. Every variant leaves bundled facts in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsRejection {
    TooLarge { bytes: usize },
    BadEnvelope(String),
    UnknownKey { key_id: String },
    RetiredKey { key_id: String },
    BadSignature,
    BadPayload(String),
    Mismatch(String),
    WrongChannel { expected: String, got: String },
    SchemaTooNew { schema_version: u32 },
    BadVersionReq(String),
    NotApplicable { applies_to: String },
    Rollback { got: u64, highest_seen: u64 },
    OutOfScope(String),
    WrongSchema { got: String },
}

impl std::fmt::Display for FactsRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { bytes } => write!(f, "envelope too large ({bytes} bytes)"),
            Self::BadEnvelope(msg) => write!(f, "bad envelope: {msg}"),
            Self::UnknownKey { key_id } => write!(f, "unknown key {key_id}"),
            Self::RetiredKey { key_id } => write!(f, "retired key {key_id}"),
            Self::BadSignature => write!(f, "bad signature"),
            Self::BadPayload(msg) => write!(f, "bad payload: {msg}"),
            Self::Mismatch(msg) => write!(f, "envelope/payload mismatch: {msg}"),
            Self::WrongChannel { expected, got } => {
                write!(f, "wrong channel (expected {expected}, got {got})")
            }
            Self::SchemaTooNew { schema_version } => {
                write!(f, "schema_version {schema_version} newer than supported")
            }
            Self::BadVersionReq(req) => write!(f, "unparseable applies_to {req:?}"),
            Self::NotApplicable { applies_to } => {
                write!(f, "not applicable to this binary ({applies_to})")
            }
            Self::Rollback { got, highest_seen } => {
                write!(f, "rollback (v{got} < seen v{highest_seen})")
            }
            Self::OutOfScope(msg) => write!(f, "outside the automation key's scope: {msg}"),
            Self::WrongSchema { got } => write!(f, "unsupported registry schema {got:?}"),
        }
    }
}

/// A payload that passed every check.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedFacts {
    pub facts: CloudFacts,
    /// The pinned key that authenticated the payload.
    pub key_id: String,
    /// Hex SHA-256 of the exact signed bytes.
    pub sha256: String,
    pub raw_len: usize,
    /// `not_after` has passed by more than the grace window.
    pub stale: bool,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

pub(crate) fn sha256_hex(payload: &[u8]) -> String {
    use sha2::Digest as _;
    hex(&sha2::Sha256::digest(payload))
}

/// The exact bytes an Ed25519 signature covers.
#[must_use]
pub fn signing_message(key_id: &str, payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(DOMAIN.len() + key_id.len() + 1 + payload.len());
    msg.extend_from_slice(DOMAIN);
    msg.extend_from_slice(key_id.as_bytes());
    msg.push(0);
    msg.extend_from_slice(payload);
    msg
}

fn ed25519_ok(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(message, signature)
        .is_ok()
}

/// Parse an RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) to unix seconds.
///
/// Offsets other than `Z` are not accepted (returns `None`); the publisher
/// always writes UTC.
#[must_use]
pub fn parse_rfc3339_utc(value: &str) -> Option<u64> {
    let value = value.trim();
    if !value.ends_with('Z') {
        return None;
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    u64::try_from(parsed.timestamp()).ok()
}

pub(crate) struct Authenticated {
    pub envelope: Envelope,
    pub payload: Vec<u8>,
    pub key_id: String,
    pub key_role: KeyRole,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EnvelopeLimits {
    pub envelope_bytes: usize,
    pub payload_bytes: usize,
    pub facts_metadata: bool,
}

pub(crate) fn authenticate(
    bytes: &[u8],
    expected_channel: &str,
    keys: &[TrustedKey],
    limits: EnvelopeLimits,
) -> Result<Authenticated, FactsRejection> {
    if bytes.len() > limits.envelope_bytes {
        return Err(FactsRejection::TooLarge { bytes: bytes.len() });
    }
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|err| FactsRejection::BadEnvelope(err.to_string()))?;
    if envelope.envelope != ENVELOPE_VERSION {
        return Err(FactsRejection::BadEnvelope(format!(
            "envelope version {} (supported {ENVELOPE_VERSION})",
            envelope.envelope
        )));
    }
    if limits.facts_metadata
        && (envelope.schema_version.is_none()
            || envelope.applies_to.is_none()
            || envelope.published_at.is_none()
            || envelope.sha256.is_none())
    {
        return Err(FactsRejection::BadEnvelope(
            "missing signed-payload metadata".into(),
        ));
    }
    if envelope.alg != "ed25519" {
        return Err(FactsRejection::BadEnvelope("unsupported alg".into()));
    }
    let valid_key_id = |id: &str| {
        id.strip_prefix("cwf-").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 32
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    };
    if envelope.sigs.len() >= MAX_SIGNATURES
        || !valid_key_id(&envelope.key_id)
        || envelope
            .sigs
            .iter()
            .any(|signature| !valid_key_id(&signature.key_id))
        || envelope.sig_b64.len() > 88
        || envelope
            .sigs
            .iter()
            .any(|signature| signature.sig_b64.len() > 88)
    {
        return Err(FactsRejection::BadEnvelope(
            "invalid signature candidates".into(),
        ));
    }
    if envelope.channel.is_empty()
        || envelope.channel.len() > 32
        || expected_channel.len() > 32
        || !envelope
            .channel
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !envelope
            .channel
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || envelope
            .applies_to
            .as_ref()
            .is_some_and(|value| value.len() > 200)
        || envelope
            .published_at
            .as_ref()
            .is_some_and(|value| value.len() > 40)
        || envelope
            .not_after
            .as_ref()
            .is_some_and(|value| value.len() > 40)
        || envelope
            .sha256
            .as_ref()
            .is_some_and(|value| value.len() != 64)
    {
        return Err(FactsRejection::BadEnvelope(
            "invalid metadata bounds".into(),
        ));
    }
    if envelope.payload_b64.len() > limits.payload_bytes * 4 / 3 + 4 {
        return Err(FactsRejection::TooLarge {
            bytes: envelope.payload_b64.len(),
        });
    }
    let payload = BASE64
        .decode(envelope.payload_b64.as_bytes())
        .map_err(|err| FactsRejection::BadEnvelope(format!("payload_b64: {err}")))?;
    if payload.is_empty() {
        return Err(FactsRejection::BadEnvelope("empty payload".into()));
    }
    if payload.len() > limits.payload_bytes {
        return Err(FactsRejection::TooLarge {
            bytes: payload.len(),
        });
    }

    let mut candidates: Vec<(&str, &str)> = Vec::with_capacity(1 + envelope.sigs.len());
    candidates.push((envelope.key_id.as_str(), envelope.sig_b64.as_str()));
    for extra in &envelope.sigs {
        candidates.push((extra.key_id.as_str(), extra.sig_b64.as_str()));
    }
    let mut saw_known = false;
    let mut saw_retired: Option<String> = None;
    let mut verified_key: Option<(String, KeyRole)> = None;
    for (key_id, sig_b64) in candidates {
        let Some(key) = trusted_key(keys, key_id) else {
            continue;
        };
        if key.status == KeyStatus::Retired {
            saw_retired.get_or_insert_with(|| key_id.to_string());
            continue;
        }
        saw_known = true;
        let Ok(signature) = BASE64.decode(sig_b64.as_bytes()) else {
            continue;
        };
        if signature.len() != 64 {
            continue;
        }
        let message = signing_message(key_id, &payload);
        if ed25519_ok(&key.public_key, &message, &signature) {
            if key.role == KeyRole::Founder {
                verified_key = Some((key_id.to_string(), key.role));
                break;
            }
            verified_key.get_or_insert_with(|| (key_id.to_string(), key.role));
        }
    }
    let Some((key_id, key_role)) = verified_key else {
        if saw_known {
            return Err(FactsRejection::BadSignature);
        }
        if let Some(key_id) = saw_retired {
            return Err(FactsRejection::RetiredKey { key_id });
        }
        return Err(FactsRejection::UnknownKey {
            key_id: envelope.key_id,
        });
    };

    Ok(Authenticated {
        envelope,
        payload,
        key_id,
        key_role,
    })
}

/// Verify a raw envelope document.
///
/// * `expected_channel` — the channel the client asked for.
/// * `current` — the running binary's version (`CARGO_PKG_VERSION`).
/// * `highest_seen` — highest `facts_version` this client previously accepted
///   for the channel (rollback protection); equal is accepted.
/// * `keys` — the pinned trust anchors (normally [`super::keys::TRUSTED_KEYS`]).
/// * `now_unix` — wall clock, injected for deterministic tests.
pub fn verify_envelope(
    bytes: &[u8],
    expected_channel: &str,
    current: &semver::Version,
    highest_seen: Option<u64>,
    keys: &[TrustedKey],
    now_unix: u64,
) -> Result<VerifiedFacts, FactsRejection> {
    let Authenticated {
        envelope,
        payload,
        key_id,
        key_role,
    } = authenticate(
        bytes,
        expected_channel,
        keys,
        EnvelopeLimits {
            envelope_bytes: MAX_ENVELOPE_BYTES,
            payload_bytes: MAX_PAYLOAD_BYTES,
            facts_metadata: true,
        },
    )?;

    // Only now is the payload trusted enough to parse.
    let facts: CloudFacts = serde_json::from_slice(&payload)
        .map_err(|err| FactsRejection::BadPayload(err.to_string()))?;
    if facts.channel != envelope.channel {
        return Err(FactsRejection::Mismatch("channel".into()));
    }
    if facts.facts_version != envelope.facts_version {
        return Err(FactsRejection::Mismatch(format!(
            "facts_version {} vs {}",
            envelope.facts_version, facts.facts_version
        )));
    }
    if let Some(outer) = envelope.applies_to.as_deref()
        && outer != facts.applies_to
    {
        return Err(FactsRejection::Mismatch("applies_to".into()));
    }
    if let Some(outer) = envelope.schema_version
        && outer != facts.schema_version
    {
        return Err(FactsRejection::Mismatch(format!(
            "schema_version {outer} vs {}",
            facts.schema_version
        )));
    }
    if envelope
        .published_at
        .as_ref()
        .is_some_and(|outer| outer != &facts.published_at)
    {
        return Err(FactsRejection::Mismatch("published_at".into()));
    }
    if let Some(outer) = envelope.not_after.as_deref()
        && Some(outer) != facts.not_after.as_deref()
    {
        return Err(FactsRejection::Mismatch("not_after".into()));
    }
    if facts.schema_version == 0 || facts.facts_version == 0 {
        return Err(FactsRejection::BadPayload(
            "schema and facts versions must be positive".into(),
        ));
    }
    if facts.channel != expected_channel {
        return Err(FactsRejection::WrongChannel {
            expected: expected_channel.to_string(),
            got: facts.channel,
        });
    }
    if facts.schema_version > SUPPORTED_SCHEMA_VERSION {
        return Err(FactsRejection::SchemaTooNew {
            schema_version: facts.schema_version,
        });
    }
    if !version_req_matches(&facts.applies_to, current)? {
        return Err(FactsRejection::NotApplicable {
            applies_to: facts.applies_to,
        });
    }
    if let Some(highest) = highest_seen
        && facts.facts_version < highest
    {
        return Err(FactsRejection::Rollback {
            got: facts.facts_version,
            highest_seen: highest,
        });
    }
    let published_at = parse_rfc3339_utc(&facts.published_at)
        .ok_or_else(|| FactsRejection::BadPayload("invalid published_at timestamp".into()))?;
    if published_at > now_unix.saturating_add(FUTURE_CLOCK_TOLERANCE_SECS) {
        return Err(FactsRejection::BadPayload(
            "published_at is in the future".into(),
        ));
    }
    let not_after = facts
        .not_after
        .as_deref()
        .map(|value| {
            parse_rfc3339_utc(value)
                .ok_or_else(|| FactsRejection::BadPayload("invalid not_after timestamp".into()))
        })
        .transpose()?;
    if not_after.is_some_and(|expires| expires <= published_at) {
        return Err(FactsRejection::BadPayload(
            "not_after does not follow published_at".into(),
        ));
    }
    if key_role == KeyRole::Automation {
        let raw: serde_json::Value = serde_json::from_slice(&payload)
            .map_err(|err| FactsRejection::BadPayload(err.to_string()))?;
        if let Some(violation) =
            automation_scope_violation(&facts, &raw, expected_channel, published_at, not_after)
        {
            return Err(FactsRejection::OutOfScope(violation));
        }
    }
    let stale =
        not_after.is_some_and(|expires| now_unix > expires.saturating_add(NOT_AFTER_GRACE_SECS));

    let sha256 = sha256_hex(&payload);
    if envelope
        .sha256
        .as_ref()
        .is_some_and(|outer| outer != &sha256)
    {
        return Err(FactsRejection::Mismatch("sha256".into()));
    }
    Ok(VerifiedFacts {
        facts,
        key_id,
        sha256,
        raw_len: payload.len(),
        stale,
    })
}

const AUTOMATION_PAYLOAD_FIELDS: &[&str] = &[
    "schema_version",
    "channel",
    "facts_version",
    "published_at",
    "not_after",
    "applies_to",
    "models",
    "provider_defaults",
    "release",
    "announcements",
];
const AUTOMATION_MODEL_FIELDS: &[&str] = &[
    "provider",
    "id",
    "op",
    "context_window",
    "max_output",
    "reasoning",
    "display_name",
    "deprecated_at",
    "replacement",
    "note",
    "applies_to",
    "pricing_withheld",
];

fn plain_text(value: Option<&str>, max_chars: usize) -> bool {
    value
        .is_none_or(|text| text.chars().count() <= max_chars && !text.chars().any(char::is_control))
}

fn whole_second_utc(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

#[must_use]
pub fn automation_scope_violation(
    facts: &CloudFacts,
    raw: &serde_json::Value,
    expected_channel: &str,
    published_at: u64,
    not_after: Option<u64>,
) -> Option<String> {
    let CloudFacts {
        schema_version: _,
        channel: _,
        facts_version,
        published_at: published_text,
        not_after: not_after_text,
        applies_to,
        models,
        provider_defaults,
        release,
        announcements,
        unknown,
    } = facts;
    if !expected_channel
        .strip_suffix(AUTOMATION_CHANNEL_SUFFIX)
        .is_some_and(|prefix| {
            prefix
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return Some(format!(
            "channel {expected_channel:?} is not an automation channel"
        ));
    }
    if !plain_text(Some(applies_to), 200) {
        return Some("applies_to is too long or not plain".into());
    }
    if !provider_defaults.is_empty() {
        return Some("provider defaults".into());
    }
    if release.is_some() {
        return Some("release facts".into());
    }
    if !announcements.is_empty() {
        return Some("announcements".into());
    }
    let unknown_top = raw.as_object().and_then(|object| {
        object
            .keys()
            .find(|field| !AUTOMATION_PAYLOAD_FIELDS.contains(&field.as_str()))
    });
    if let Some(field) = unknown_top.or_else(|| unknown.keys().next()) {
        return Some(format!("field {field:?} is not allowed"));
    }
    if models.len() > AUTOMATION_MAX_MODELS {
        return Some(format!("more than {AUTOMATION_MAX_MODELS} models"));
    }
    let raw_models = raw.get("models").and_then(serde_json::Value::as_array);
    if raw_models.map_or(0, Vec::len) != models.len() {
        return Some("models do not match the signed bytes".into());
    }
    for (model, raw_model) in models.iter().zip(raw_models.into_iter().flatten()) {
        let ModelFact {
            provider,
            id,
            op,
            allow_unlisted,
            context_window,
            max_output,
            pricing,
            pricing_withheld,
            reasoning: _,
            reasoning_options,
            display_name,
            deprecated_at,
            replacement,
            note,
            applies_to,
        } = model;
        let name = format!("{provider}/{id}");
        if provider.is_empty() || id.is_empty() {
            return Some("malformed model entry".into());
        }
        let unknown_field = raw_model.as_object().and_then(|object| {
            object
                .keys()
                .find(|field| !AUTOMATION_MODEL_FIELDS.contains(&field.as_str()))
        });
        if let Some(field) = unknown_field {
            return Some(format!("{name}: field {field:?} is not allowed"));
        }
        let null_field = raw_model.as_object().and_then(|object| {
            object
                .iter()
                .find_map(|(field, value)| value.is_null().then_some(field.as_str()))
        });
        if let Some(field) = null_field {
            return Some(format!("{name}: field {field:?} is null"));
        }
        if *allow_unlisted || pricing.is_some() || reasoning_options.is_some() {
            return Some(format!("{name}: field outside the automation model fields"));
        }
        if !matches!(op, ModelOp::Upsert | ModelOp::Deprecate) {
            return Some(format!("{name} is not an upsert or deprecation"));
        }
        if !plain_text(Some(provider), 64)
            || !plain_text(Some(id), 200)
            || !plain_text(display_name.as_deref(), 120)
            || !plain_text(deprecated_at.as_deref(), 40)
            || !plain_text(replacement.as_deref(), 200)
            || !plain_text(note.as_deref(), 300)
            || !plain_text(applies_to.as_deref(), 200)
            || !plain_text(pricing_withheld.as_deref(), 300)
        {
            return Some(format!("{name}: text too long or not plain"));
        }
        let window_ok = context_window.is_none_or(|window| (1_024..=20_000_000).contains(&window));
        let output_ok = max_output.is_none_or(|output| {
            (1..=2_000_000).contains(&output)
                && context_window.is_none_or(|window| output <= window)
        });
        if !window_ok || !output_ok {
            return Some(format!("{name}: limit out of range"));
        }
    }
    if !whole_second_utc(published_text)
        || not_after_text
            .as_deref()
            .is_some_and(|text| !whole_second_utc(text))
    {
        return Some("timestamp is not whole-second UTC".into());
    }
    if *facts_version != published_at {
        return Some("facts_version is not the publication time".into());
    }
    automation_window_violation(published_at, not_after).map(str::to_string)
}

pub const MAX_AUTOMATION_WINDOW_SECS: u64 = AUTOMATION_MAX_VALIDITY_SECS;

#[must_use]
pub fn automation_window_violation(
    published_at: u64,
    not_after: Option<u64>,
) -> Option<&'static str> {
    match not_after {
        None => Some("no not_after"),
        Some(expires)
            if expires <= published_at || expires - published_at > MAX_AUTOMATION_WINDOW_SECS =>
        {
            Some("validity outside the automation limit")
        }
        Some(_) => None,
    }
}

pub fn check_automation_window(
    published_at: u64,
    not_after: Option<u64>,
    now_unix: u64,
) -> Result<u64, FactsRejection> {
    if let Some(violation) = automation_window_violation(published_at, not_after) {
        return Err(FactsRejection::OutOfScope(violation.into()));
    }
    let expires = not_after.expect("checked above");
    if now_unix >= expires {
        return Err(FactsRejection::OutOfScope("not_after has expired".into()));
    }
    Ok(expires)
}

/// Evaluate a Cargo-style semver requirement against the running version.
///
/// `*` matches everything; an empty requirement is invalid. Prerelease binaries (`0.9.12-beta.1`) only
/// match requirements that name a prerelease on the same `major.minor.patch`,
/// which is semver's rule; the beta channel must set `applies_to` explicitly.
pub fn version_req_matches(req: &str, current: &semver::Version) -> Result<bool, FactsRejection> {
    let trimmed = req.trim();
    if trimmed.is_empty() || trimmed.len() > 200 {
        return Err(FactsRejection::BadVersionReq(String::new()));
    }
    if trimmed == "*" {
        return Ok(true);
    }
    let parsed = semver::VersionReq::parse(trimmed)
        .map_err(|_| FactsRejection::BadVersionReq(trimmed.to_string()))?;
    Ok(parsed.matches(current))
}

/// Lenient per-item variant: unparseable requirements simply do not match.
#[must_use]
pub fn item_applies(req: Option<&str>, current: &semver::Version) -> bool {
    match req {
        None => true,
        Some(req) => version_req_matches(req, current).unwrap_or(false),
    }
}
