use super::*;
use std::env;
use std::ffi::OsString;
use std::fs;

// The fixture resolves the exact declaration from its destination document.
// Persistence still re-reads and validates under the shared write lock.
pub fn selection_identity(path: &Path, id: &str) -> crate::config::ProviderIdentity {
    crate::config::parse_config_base(&fs::read_to_string(path).unwrap())
        .unwrap()
        .resolve_provider_pin_identity(id)
        .unwrap()
}

/// Clears every model override the dispatcher's env layer reads for the
/// providers exercised below, so the assertion is about config precedence
/// and cannot be flipped by an ambient variable on a developer machine.
pub struct ModelEnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl ModelEnvGuard {
    const VARS: &'static [&'static str] = &[
        "CODEWHALE_MODEL",
        "DEEPSEEK_MODEL",
        "DEEPSEEK_DEFAULT_TEXT_MODEL",
        "GLM_MODEL",
        "BIGMODEL_MODEL",
        "ZAI_MODEL",
        "XAI_MODEL",
        "GROK_MODEL",
        "OPENROUTER_MODEL",
        "OLLAMA_MODEL",
    ];

    pub fn new() -> Self {
        let saved = Self::VARS
            .iter()
            .map(|name| (*name, env::var_os(name)))
            .collect();
        // Safety: test-only environment mutation; the caller holds the
        // process-wide test-env lock via `EnvGuard`.
        unsafe {
            for name in Self::VARS {
                env::remove_var(name);
            }
        }
        Self { saved }
    }
}

impl Default for ModelEnvGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ModelEnvGuard {
    fn drop(&mut self) {
        // Safety: test-only environment restoration under the same lock.
        unsafe {
            for (name, value) in &self.saved {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }
}
