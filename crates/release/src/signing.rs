//! Release-manifest signatures.
//!
//! A release publishes `codewhale-artifacts-sha256.txt` and, once the release
//! key exists, `codewhale-artifacts-sha256.txt.sig`: a base64 Ed25519
//! signature over [`manifest_statement`]. The statement binds the exact
//! manifest bytes to one version, so a mirror or a compromised download host
//! can serve bytes but cannot make them signed, and an old signed manifest
//! cannot be replayed as a newer version. The same key signs the desktop
//! appcast under its own statement prefix.
//!
//! Known limits: there is no expiry inside the statement (the updater never
//! downgrades, so a replayed old release is refused by version, but a withheld
//! update is not detected). Until a key is configured every manifest reads as
//! unsigned, which keeps automatic updates on "ask first".

use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};

pub const SIGNATURE_ASSET: &str = "codewhale-artifacts-sha256.txt.sig";
pub const STATEMENT_PREFIX: &str = "codewhale-release-manifest-v1";
pub const MAX_SIGNATURE_BYTES: usize = 1024;

pub const TRUSTED_RELEASE_KEYS: &[[u8; 32]] = &[];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureCheck {
    NotConfigured,
    Missing,
    Verified,
    Invalid,
}

impl SignatureCheck {
    #[must_use]
    pub fn is_verified(self) -> bool {
        self == Self::Verified
    }
}

#[must_use]
pub fn manifest_statement(version: &str, manifest: &[u8]) -> Vec<u8> {
    let version = version.trim().trim_start_matches('v');
    let mut statement = format!("{STATEMENT_PREFIX}\n{version}\n").into_bytes();
    statement.extend_from_slice(manifest);
    statement
}

#[must_use]
pub fn trusted_keys() -> Vec<[u8; 32]> {
    let mut keys = TRUSTED_RELEASE_KEYS.to_vec();
    let mut add = |hex: &str| {
        if let Some(key) = parse_key(hex) {
            keys.push(key);
        }
    };
    if let Some(hex) = option_env!("CODEWHALE_RELEASE_PUBLIC_KEY") {
        add(hex);
    }
    if cfg!(debug_assertions)
        && let Ok(hex) = std::env::var("CODEWHALE_TEST_RELEASE_PUBLIC_KEY")
    {
        add(&hex);
    }
    keys
}

fn parse_key(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

#[must_use]
pub fn verify_manifest(version: &str, manifest: &[u8], signature: Option<&[u8]>) -> SignatureCheck {
    verify_manifest_with(&trusted_keys(), version, manifest, signature)
}

#[must_use]
pub fn verify_manifest_with(
    keys: &[[u8; 32]],
    version: &str,
    manifest: &[u8],
    signature: Option<&[u8]>,
) -> SignatureCheck {
    if keys.is_empty() {
        return SignatureCheck::NotConfigured;
    }
    let Some(signature) = signature else {
        return SignatureCheck::Missing;
    };
    if signature.len() > MAX_SIGNATURE_BYTES {
        return SignatureCheck::Invalid;
    }
    let Ok(text) = std::str::from_utf8(signature) else {
        return SignatureCheck::Invalid;
    };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(text.trim()) else {
        return SignatureCheck::Invalid;
    };
    let statement = manifest_statement(version, manifest);
    if raw.len() == 64
        && keys.iter().any(|key| {
            UnparsedPublicKey::new(&ED25519, key)
                .verify(&statement, &raw)
                .is_ok()
        })
    {
        SignatureCheck::Verified
    } else {
        SignatureCheck::Invalid
    }
}
