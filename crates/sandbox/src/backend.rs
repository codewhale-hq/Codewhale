//! Pluggable sandbox backend abstraction.
//!
//! External sandbox backends route shell command execution to a remote service
//! (e.g. Alibaba OpenSandbox) instead of spawning a local process. This is
//! complementary to the OS-level sandbox module (Seatbelt / opt-in bubblewrap)
//! — the external backend *replaces* local execution entirely when configured.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;

/// Output from a sandbox backend execution.
#[derive(Debug, Clone)]
pub struct SandboxOutput {
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error from the command.
    pub stderr: String,
    /// Exit code (0 for success).
    pub exit_code: i32,
}

/// The kind of external sandbox backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxKind {
    /// No external sandbox — execute commands locally.
    None,
    /// Alibaba OpenSandbox remote execution.
    OpenSandbox,
    /// Configured backend is unavailable; execution is refused.
    Unsupported,
}

impl SandboxKind {
    /// Parse a sandbox backend name from config (case-insensitive).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "" => Some(Self::None),
            "opensandbox" | "open-sandbox" | "open_sandbox" => Some(Self::OpenSandbox),
            _ => None,
        }
    }

    /// Human-readable label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OpenSandbox => "opensandbox",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Abstract interface for an external sandbox backend.
///
/// Implementations send commands to a remote execution environment and return
/// structured output. The trait is `Send + Sync` so it can be stored in an
/// `Arc` and shared across async tasks.
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Backend identity used by tool receipts.
    fn kind(&self) -> SandboxKind;
    /// Execute a shell command and return its output.
    ///
    /// `cmd` is the full shell command string (e.g. `"ls -la"`).
    /// `env` contains additional environment variables to set.
    async fn exec(&self, cmd: &str, env: &HashMap<String, String>) -> Result<SandboxOutput>;
}

use codewhale_runtime::config::Config;

/// Create the configured sandbox backend from config.
///
/// Returns `None` when no external sandbox backend is configured (i.e. the
/// `sandbox_backend` key is absent, empty, or `"none"`). When `"opensandbox"`
/// is set, constructs an [`OpenSandboxBackend`](super::opensandbox::OpenSandboxBackend) using `sandbox_url` and
/// `sandbox_api_key`.
pub fn create_backend(config: &Config) -> Result<Option<Box<dyn SandboxBackend>>> {
    let Some(kind) = SandboxKind::parse(config.sandbox_backend.as_deref().unwrap_or("none")) else {
        // Old or misspelled remote settings must never select local execution.
        return Ok(Some(Box::new(UnsupportedBackend)));
    };

    match kind {
        SandboxKind::None => Ok(None),
        SandboxKind::Unsupported => Ok(Some(Box::new(UnsupportedBackend))),
        SandboxKind::OpenSandbox => {
            let base_url = config
                .sandbox_url
                .clone()
                .unwrap_or_else(|| "http://localhost:8080".to_string());
            let api_key = config.sandbox_api_key.clone();
            let backend = super::opensandbox::OpenSandboxBackend::new(base_url, api_key, 30)?;
            Ok(Some(Box::new(backend)))
        }
    }
}

/// A configured execution boundary that is no longer supported. Keep it present
/// in the tool context so every shell call is refused instead of running locally.
struct UnsupportedBackend;

#[async_trait]
impl SandboxBackend for UnsupportedBackend {
    fn kind(&self) -> SandboxKind {
        SandboxKind::Unsupported
    }
    async fn exec(&self, _cmd: &str, _env: &HashMap<String, String>) -> Result<SandboxOutput> {
        anyhow::bail!(
            "Unsupported sandbox_backend setting. Choose opensandbox, or explicitly set none for local execution."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsupported_backend_refuses_execution_instead_of_falling_back_to_local() {
        for name in ["shannon", "shannonnet", "shannon-net", "levee", "unknown"] {
            let config = {
                let __cw_config_init_0_0 = Some(name.into());
                let mut __cw_config_init_0_value: Config = Config::default();
                __cw_config_init_0_value.sandbox_backend = __cw_config_init_0_0;
                __cw_config_init_0_value
            };
            let backend = create_backend(&config)
                .unwrap()
                .expect("retain execution boundary");
            let error = backend
                .exec("echo must-not-run", &HashMap::new())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("Unsupported sandbox_backend"));
        }
        for name in [None, Some("none"), Some("")] {
            let config = {
                let __cw_config_init_1_0 = name.map(str::to_owned);
                let mut __cw_config_init_1_value: Config = Config::default();
                __cw_config_init_1_value.sandbox_backend = __cw_config_init_1_0;
                __cw_config_init_1_value
            };
            assert!(create_backend(&config).unwrap().is_none());
        }
    }
}
