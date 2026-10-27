//! Deterministic identifiers for the work graph.
//!
//! Every ID derives from `(session_id, discriminator)` via SHA-256 — there is
//! no RNG anywhere in this module tree — so replaying the same change
//! sequence (or re-importing the same legacy state) yields byte-identical
//! graphs, IDs included. That determinism is what makes import idempotent and
//! snapshots comparable across processes.
//!
//! Format: `<prefix>` + first 12 hex chars of
//! `sha256(prefix U+001F session_id U+001F discriminator)`. The unit
//! separator keeps `("ab", "c")` and `("a", "bc")` from colliding, and the
//! prefix participates in the hash so distinct ID types never share digests
//! for the same discriminator.

use serde::{Deserialize, Serialize};

use codewhale_runtime::hashing::sha256_hex;

fn derive_raw(prefix: &str, session_id: &str, discriminator: &str) -> String {
    let digest = sha256_hex(format!("{prefix}\u{1f}{session_id}\u{1f}{discriminator}"));
    format!("{prefix}{}", &digest[..12])
}

macro_rules! graph_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            /// Deterministically derive an ID from the owning session and a
            /// caller-chosen discriminator (e.g. `"plan:3"`, `"objective"`).
            #[must_use]
            pub fn derive(session_id: &str, discriminator: &str) -> Self {
                Self(derive_raw($prefix, session_id, discriminator))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

graph_id!(
    /// Identity of a node in the work graph (`"wn:" + sha256(...)[..12]`).
    WorkNodeId,
    "wn:"
);
graph_id!(
    /// Identity of an edge in the work graph.
    WorkEdgeId,
    "we:"
);
graph_id!(
    /// Identity of an applied change (recorded on its [`ChangeReceipt`](super::ChangeReceipt)).
    ChangeId,
    "ch:"
);
graph_id!(
    /// Identity of a proposed plan diff awaiting review.
    ProposalId,
    "pp:"
);
graph_id!(
    /// Identity of an operation binding; used with an owner-reported sequence
    /// number as the reducer idempotency key.
    BindingId,
    "bd:"
);
