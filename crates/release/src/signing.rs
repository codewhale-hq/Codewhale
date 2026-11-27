//! Release-manifest signatures.
//!
//! A release publishes `codewhale-artifacts-sha256.txt` and
//! `codewhale-artifacts-sha256.txt.sig`: a base64 Ed25519 signature over
//! [`manifest_statement`]. The statement binds the exact
//! manifest bytes to one version, so a mirror or a compromised download host
//! can serve bytes but cannot make them signed, and an old signed manifest
//! cannot be replayed as a newer version.
//!
//! [`TRUSTED_RELEASE_KEYS`] holds the release public key, so a build refuses
//! a release whose signature is missing or does not verify. A build from
//! before the key existed trusts no key, reads every manifest as unsigned and
//! asks before installing.
//!
//! Known limits: there is no expiry inside the statement (the updater never
//! downgrades, so a replayed old release is refused by version, but a withheld
//! update is not detected).

use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};

pub const SIGNATURE_ASSET: &str = "codewhale-artifacts-sha256.txt.sig";
pub const STATEMENT_PREFIX: &str = "codewhale-release-manifest-v1";
pub const MAX_SIGNATURE_BYTES: usize = 1024;

pub const TRUSTED_RELEASE_KEYS: &[[u8; 32]] = &[[
    0xf0, 0x1f, 0x23, 0x55, 0x98, 0x0c, 0x74, 0x8a, 0x0f, 0x64, 0xea, 0x5d, 0xb8, 0x15, 0xc8, 0xab,
    0x1f, 0x4f, 0xbc, 0xe6, 0x7e, 0x39, 0x13, 0xba, 0x07, 0xda, 0x3f, 0xd1, 0xe0, 0xed, 0x51, 0xce,
]];

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

#[cfg(test)]
mod tests {
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    const MANIFEST: &[u8] =
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  codewhale-linux-x64\n";

    fn pair(seed: u8) -> (Ed25519KeyPair, [u8; 32]) {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap();
        let public = pair.public_key().as_ref().try_into().unwrap();
        (pair, public)
    }

    fn sign(pair: &Ed25519KeyPair, version: &str, manifest: &[u8]) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .encode(pair.sign(&manifest_statement(version, manifest)))
            .into_bytes()
    }

    fn hex(key: &[u8; 32]) -> String {
        key.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn the_release_key_is_trusted_and_an_unsigned_release_is_missing_its_signature() {
        assert_eq!(
            TRUSTED_RELEASE_KEYS.iter().map(hex).collect::<Vec<_>>(),
            ["f01f2355980c748a0f64ea5db815c8ab1f4fbce67e3913ba07da3fd1e0ed51ce"]
        );
        assert_eq!(
            parse_key(&hex(&TRUSTED_RELEASE_KEYS[0])),
            Some(TRUSTED_RELEASE_KEYS[0])
        );
        assert_eq!(
            verify_manifest_with(TRUSTED_RELEASE_KEYS, "0.10.6", MANIFEST, None),
            SignatureCheck::Missing
        );
        assert_eq!(
            verify_manifest_with(&[], "0.10.6", MANIFEST, None),
            SignatureCheck::NotConfigured
        );
    }

    #[test]
    fn the_statement_format_is_unchanged() {
        assert_eq!(STATEMENT_PREFIX, "codewhale-release-manifest-v1");
        assert_eq!(SIGNATURE_ASSET, "codewhale-artifacts-sha256.txt.sig");
        assert_eq!(
            manifest_statement("v0.10.6", b"manifest bytes\n"),
            b"codewhale-release-manifest-v1\n0.10.6\nmanifest bytes\n"
        );
        assert_eq!(
            manifest_statement(" 0.10.6 ", b""),
            b"codewhale-release-manifest-v1\n0.10.6\n"
        );
    }

    #[test]
    fn a_manifest_signed_by_another_key_is_rejected() {
        let (signer, trusted) = pair(7);
        let (other, _) = pair(9);
        assert_eq!(
            hex(&trusted),
            "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
        );
        let signature = sign(&signer, "9.9.9", MANIFEST);
        assert_eq!(
            signature,
            b"vX9KZ7maFenoVCheguRaHu1npjRjKTa09ZCVvF6pbL3Yad5kNHvjWBIGZZbUQM9sO5Mnp86CxWJu82jhKZDADg=="
        );
        let verify = |keys: &[[u8; 32]], version: &str, manifest: &[u8], signature: &[u8]| {
            verify_manifest_with(keys, version, manifest, Some(signature))
        };
        assert_eq!(
            verify(&[trusted], "v9.9.9", MANIFEST, &signature),
            SignatureCheck::Verified
        );
        assert_eq!(
            verify(
                &[trusted],
                "9.9.9",
                MANIFEST,
                &sign(&other, "9.9.9", MANIFEST)
            ),
            SignatureCheck::Invalid
        );
        assert_eq!(
            verify(TRUSTED_RELEASE_KEYS, "9.9.9", MANIFEST, &signature),
            SignatureCheck::Invalid,
            "a key the build does not trust cannot sign a release"
        );
        assert_eq!(
            verify(&[trusted], "9.9.10", MANIFEST, &signature),
            SignatureCheck::Invalid,
            "a signature is bound to its version"
        );
        assert_eq!(
            verify(&[trusted], "9.9.9", b"other manifest\n", &signature),
            SignatureCheck::Invalid
        );
        assert_eq!(
            verify(&[trusted], "9.9.9", MANIFEST, b"not base64"),
            SignatureCheck::Invalid
        );
    }
}
