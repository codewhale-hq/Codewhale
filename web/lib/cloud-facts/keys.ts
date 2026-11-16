/**
 * Ed25519 verifying keys for the CodeWhale cloud facts channel (facts/v1).
 *
 * Mirror of `crates/config/src/cloud_facts/keys.rs` — `check:facts` fails if
 * the two diverge. Keys are pinned here (and in the binary); the Supabase
 * `facts_key` table is informational and never a trust root.
 *
 * The pinned anchor below is the trust root for delivery. Tests supply their
 * own public fixture keys; those keys never belong in this table.
 */
export type KeyStatus = "active" | "retired";
export type KeyRole = "founder" | "automation";

export interface TrustedKey {
  keyId: string;
  /** Standard base64 of the raw 32-byte Ed25519 public key. */
  publicKey: string;
  status: KeyStatus;
  role: KeyRole;
}

export const AUTOMATION_CHANNEL_SUFFIX = "-models";
export const AUTOMATION_MAX_VALIDITY_MS = 7 * 24 * 60 * 60 * 1000;
export const AUTOMATION_MAX_MODELS = 256;

export const DOMAIN = "codewhale-facts/v1\0";
export const ENVELOPE_VERSION = 1;
export const SUPPORTED_SCHEMA_VERSION = 1;
export const MAX_PAYLOAD_BYTES = 512 * 1024;

export const TRUSTED_KEYS: readonly TrustedKey[] = [
  {
    // Approved 2026-09-10. Mirrors crates/config/src/cloud_facts/keys.rs.
    // Private half held by the founder outside any repository.
    keyId: "cwf-2026-09",
    publicKey: "5d1syIWzufnSTlVrfFVb7CePHL6B3Ol92IhS15QaBoU=",
    status: "active",
    role: "founder",
  },
];

export function trustedKey(keyId: string): TrustedKey | undefined {
  return TRUSTED_KEYS.find((key) => key.keyId === keyId);
}
