//! Injectable ambient-environment access for credential resolution.
//!
//! Ported from pi-mono `packages/ai/src/auth/context.ts` and the `AuthContext`
//! interface in `packages/ai/src/auth/types.ts` (MIT, Copyright (c) 2025 Mario
//! Zechner — full notice in the parent module).
//!
//! pi's motivation applies here unchanged: resolution that reads
//! `process.env` / `std::env` directly is untestable without mutating the real
//! process. Every ambient read the resolver performs itself goes through this
//! trait, so a test can state exactly which variables exist. Resolution never
//! stats the filesystem (#5772): external credential discovery is
//! metadata-only, so the trait deliberately has no file probe.

#[cfg(any(test, feature = "test-support"))]
use std::collections::BTreeMap;

/// Ambient environment access for credential resolution.
pub trait AuthContext: Send + Sync {
    /// Read an environment variable, treating blank values as unset — a blank
    /// `DEEPSEEK_API_KEY=` is a leftover export, not a credential.
    fn env(&self, name: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessAuthContext;

impl AuthContext for ProcessAuthContext {
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }
}

/// Test double: a fixed set of variables.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct MapAuthContext {
    env: BTreeMap<String, String>,
}

#[cfg(any(test, feature = "test-support"))]
impl MapAuthContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_env(mut self, name: &str, value: &str) -> Self {
        self.env.insert(name.to_string(), value.to_string());
        self
    }
}

#[cfg(any(test, feature = "test-support"))]
impl AuthContext for MapAuthContext {
    fn env(&self, name: &str) -> Option<String> {
        self.env
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
    }
}
