//! Provider credentials: one type-tagged credential per provider, one write
//! path, and one resolution result that names its source.
//!
//! # Attribution
//!
//! The design of this module is ported from **pi-mono** by Mario Zechner
//! (<https://github.com/earendil-works/pi-mono>), MIT licensed, specifically:
//!
//! | pi-mono file                              | ported into                                    |
//! |-------------------------------------------|------------------------------------------------|
//! | `packages/ai/src/auth/types.ts`           | [`Credential`], [`CredentialStore`], [`AuthContext`] |
//! | `packages/ai/src/auth/credential-store.ts`| `store::InMemoryCredentialStore`             |
//! | `packages/ai/src/auth/context.ts`         | [`context::ProcessAuthContext`]                |
//! | `packages/ai/src/auth/resolve.ts`         | `codewhale_tui::config::credential_resolve`    |
//!
//! This is a **design port into idiomatic Rust, not a line-for-line copy**.
//! pi's module is async TypeScript over a `Provider` record with a single
//! `auth.json`; CodeWhale's is synchronous Rust over `ProviderKind` and the
//! several pre-existing on-disk stores (secret store, config file, ambient
//! environment, externally consented CLI credential files). The four ideas
//! taken verbatim in spirit are: one type-tagged credential per provider,
//! `modify` as the only serialized write path, one stated precedence rule in
//! one place, and every resolution carrying a human-readable source label.
//! Several doc comments are adapted closely enough that the MIT permission
//! notice travels with them, reproduced below.
//!
//! ```text
//! MIT License
//!
//! Copyright (c) 2025 Mario Zechner
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//! ```
//!
//! # Redaction
//!
//! Nothing in this module renders or logs secret material. [`Credential`]
//! deliberately has a hand-written [`std::fmt::Debug`] that prints only the
//! variant tag, so a credential cannot reach a log line through `{:?}` on a
//! surrounding struct.

pub mod context;
pub(crate) mod source;
pub mod store;

#[cfg(test)]
mod tests;

pub use context::AuthContext;
pub use source::{CredentialProbe, CredentialResolution, CredentialSource};
pub use store::{CredentialInfo, CredentialStore};

/// One type-tagged credential per provider — pi's `Credential` union.
///
/// CodeWhale stores API keys in the secret store and OAuth material in
/// provider-specific files that this type deliberately does **not** try to
/// unify; the OAuth variant carries only what a status surface needs, so
/// adopting this type never moves a token between stores.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// A bearer API key held in Codewhale's durable store or process memory.
    ApiKey { key: String },
    /// An OAuth access token plus its expiry, if the flow reported one.
    ///
    /// Constructed today only by this module's own tests: CodeWhale's OAuth
    /// stores (xAI's generation files, the read-only external grants) have not
    /// been moved behind [`CredentialStore`] in this change, so nothing in the
    /// production path mints one yet. The variant is kept because it is half
    /// of the ported contract and the store's serialization guarantee exists
    /// precisely for it.
    OAuth {
        access: String,
        expires_at_unix_secs: Option<i64>,
    },
}

impl Credential {
    /// Only `store::InMemoryCredentialStore::list` needs this today; the
    /// secret-store adapter knows every slot it holds is an api key.
    pub fn kind(&self) -> CredentialKind {
        match self {
            Self::ApiKey { .. } => CredentialKind::ApiKey,
            Self::OAuth { .. } => CredentialKind::OAuth,
        }
    }

    /// Borrow the secret. Callers must not log or render the result; this is
    /// the single narrow accessor so `rg "expose_secret"` finds every use.
    pub fn expose_secret(&self) -> &str {
        match self {
            Self::ApiKey { key } => key,
            Self::OAuth { access, .. } => access,
        }
    }
}

/// Never print secret material, even through a derived `Debug` on a
/// surrounding struct.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey { .. } => f.write_str("Credential::ApiKey(<redacted>)"),
            Self::OAuth {
                expires_at_unix_secs,
                ..
            } => f
                .debug_struct("Credential::OAuth")
                .field("access", &"<redacted>")
                .field("expires_at_unix_secs", expires_at_unix_secs)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    ApiKey,
    /// See the note on [`Credential::OAuth`]: no production store mints one
    /// yet.
    OAuth,
}
