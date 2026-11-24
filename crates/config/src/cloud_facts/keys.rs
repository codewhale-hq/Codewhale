//! Trust anchors for the cloud facts channel.
//!
//! Keys are pinned in the binary. The Supabase `facts_key` table and the
//! website mirror (`web/lib/cloud-facts/keys.ts`) are informational; a facts
//! envelope is accepted only when its signature verifies under an `Active` key
//! listed here. `web/scripts/check-cloud-facts.mjs` fails CI if this table and
//! the TypeScript mirror diverge.
//!
//! Rotation (two-release rule): pin the new key here → ship → sign with both
//! keys (`sigs`) → mark the old key `Retired` → ship → drop it. Compromise:
//! revoke every release signed by the key server-side, ship a binary without
//! the key. There is deliberately no in-band "distrust this key" message.

/// Domain separator prefixed to every signed message.
///
/// Message = `DOMAIN || key_id || 0x00 || payload_bytes`.
pub const DOMAIN: &[u8] = b"codewhale-facts/v1\0";

/// Transport envelope version this client understands.
pub const ENVELOPE_VERSION: u64 = 1;

/// Highest signed-payload `schema_version` this client understands. Newer
/// payloads are rejected as `SchemaTooNew` and the bundled facts stay in use.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Hard cap on the decoded payload; enforced before any crypto runs.
pub const MAX_PAYLOAD_BYTES: usize = 512 * 1024;

/// Hard cap on the raw envelope document (payload base64 + metadata).
pub const MAX_ENVELOPE_BYTES: usize = 768 * 1024;

/// Whether a pinned key may still authenticate new releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    /// Accepts signatures.
    Active,
    /// Still listed so `/status` can name it, but no longer accepted.
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRole {
    Founder,
    Automation,
}

pub const AUTOMATION_CHANNEL_SUFFIX: &str = "-models";
pub const AUTOMATION_MAX_VALIDITY_SECS: u64 = 7 * 24 * 60 * 60;
pub const AUTOMATION_MAX_MODELS: usize = 256;

/// One pinned Ed25519 verifying key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedKey {
    /// `cwf-<label>`; also part of the signed message.
    pub key_id: &'static str,
    /// Raw 32-byte Ed25519 public key.
    pub public_key: [u8; 32],
    pub status: KeyStatus,
    pub role: KeyRole,
}

/// Pinned production trust anchors.
///
/// A shipped binary can only trust a key it was compiled with — there is no
/// in-band command that installs or widens trust, and `CODEWHALE_CLOUD_FACTS_PATH`
/// verifies against this same table, so a local envelope is no escape hatch.
/// Adding a key here is therefore a release-gated decision, and the matching
/// entry in `web/lib/cloud-facts/keys.ts` must stay byte-identical:
/// `check:facts` fails when the two tables diverge.
///
/// Tests inject their own fixture keys; no fixture signature establishes
/// production trust.
pub const TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey {
    // Approved 2026-09-10. Private half held by the founder outside any
    // repository; only this public anchor is committed.
    key_id: "cwf-2026-09",
    public_key: [
        229, 221, 108, 200, 133, 179, 185, 249, 210, 78, 85, 107, 124, 85, 91, 236, 39, 143, 28,
        190, 129, 220, 233, 125, 216, 136, 82, 215, 148, 26, 6, 133,
    ],
    status: KeyStatus::Active,
    role: KeyRole::Founder,
}];

/// Look up a pinned key by id.
#[must_use]
pub fn trusted_key<'a>(keys: &'a [TrustedKey], key_id: &str) -> Option<&'a TrustedKey> {
    if keys.iter().enumerate().any(|(index, left)| {
        keys[index + 1..]
            .iter()
            .any(|right| left.key_id == right.key_id || left.public_key == right.public_key)
    }) {
        return None;
    }
    keys.iter().find(|key| key.key_id == key_id)
}
