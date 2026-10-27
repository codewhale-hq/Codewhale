//! Where a credential came from, and — when there isn't one — where we looked.
//!
//! Ported from pi-mono's `AuthResult { auth, env, source }`
//! (`packages/ai/src/auth/types.ts`, MIT, Copyright (c) 2025 Mario Zechner;
//! full notice in the parent module). pi returns a human-readable `source`
//! such as `"ANTHROPIC_API_KEY"`, `"OAuth"`, or `"~/.aws/credentials"` from
//! every resolution so a status surface can say which place won.
//!
//! CodeWhale adds the negative half, because that is where its picker was
//! useless: a failed resolution carries the ordered list of places that were
//! actually probed, so "missing key" can name them and say what would fix it.
//!
//! Nothing here holds secret material — only labels.

use std::borrow::Cow;

/// One place credential resolution looked, and what would put a credential
/// there. Labels only; never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialProbe {
    /// Where we looked, e.g. `"env DEEPSEEK_API_KEY"` or `"secret store slot \"deepseek\""`.
    pub(crate) place: Cow<'static, str>,
    /// What the user would do to make this place answer, if there is a
    /// one-line answer. `None` when the place is informational only.
    pub fix: Option<Cow<'static, str>>,
}

impl CredentialProbe {
    pub fn with_fix(
        place: impl Into<Cow<'static, str>>,
        fix: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            place: place.into(),
            fix: Some(fix.into()),
        }
    }
}

/// The place a credential was resolved from, or the fact that no place had
/// one. This is the single value every readiness surface should render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// `auth_mode = "none"`: the route intentionally sends no credential.
    AuthModeNone,
    /// A keyless self-hosted or loopback route.
    KeylessRoute {
        base_url: String,
    },
    /// `--api-key` on the command line (or the dispatcher's source-marked
    /// forward of it).
    CliOverride,
    /// `[providers.<table>] api_key`.
    ProviderConfigApiKey {
        table: String,
    },
    /// `[providers.<table>] api_key_env = "<var>"`, resolved from `<var>`.
    ProviderConfigEnv {
        var: String,
    },
    /// An ambient provider environment variable.
    AmbientEnv {
        var: String,
    },
    /// CodeWhale's own durable secret store.
    SecretStore {
        slot: String,
    },
    /// A read-only, explicitly consented credential file owned by another CLI.
    ExternalGrant {
        cli: String,
        path: String,
    },
    /// A subscription OAuth sign-in: Codewhale-owned storage, or (xAI) a
    /// consented Grok CLI import. `account` is the display label (email,
    /// plan) of the account it signs in as, taken from the same read that
    /// proved the sign-in usable; never token material.
    OAuth {
        flow: String,
        account: Option<String>,
    },
    /// The user-global `~/.codewhale/config.toml`, consulted last so a key
    /// saved there survives loading a workspace config.
    UserGlobalConfig,
    /// An expiring, process-only Codewhale account auth transform.
    AccountSession,
    SessionKey,
    /// Nothing had a credential. `probed` is in precedence order.
    Missing {
        probed: Vec<CredentialProbe>,
    },
}

impl CredentialSource {
    pub fn is_present(&self) -> bool {
        !matches!(self, Self::Missing { .. })
    }

    /// Short human-readable label, in pi's spirit: the name of the place, not
    /// a sentence. Safe to render anywhere — never contains a secret.
    pub fn label(&self) -> Cow<'static, str> {
        match self {
            Self::AuthModeNone => Cow::Borrowed("auth_mode = \"none\""),
            Self::KeylessRoute { base_url } => Cow::Owned(format!(
                "keyless route {}",
                codewhale_config::structural_url_authority(base_url)
            )),
            Self::CliOverride => Cow::Borrowed("--api-key"),
            Self::ProviderConfigApiKey { table } => Cow::Owned(format!("[{table}] api_key")),
            Self::ProviderConfigEnv { var } => Cow::Owned(format!("api_key_env {var}")),
            Self::AmbientEnv { var } => Cow::Owned(var.clone()),
            Self::SecretStore { slot } => Cow::Owned(format!("secret store \"{slot}\"")),
            Self::ExternalGrant { cli, path } => {
                Cow::Owned(format!("{cli} credentials (read-only) {path}"))
            }
            Self::OAuth { flow, .. } => Cow::Owned(format!("{flow} OAuth")),
            Self::AccountSession => Cow::Borrowed("Codewhale account"),
            Self::SessionKey => Cow::Borrowed("session key"),
            Self::UserGlobalConfig => Cow::Borrowed("~/.codewhale/config.toml api_key"),
            Self::Missing { .. } => Cow::Borrowed("not found"),
        }
    }

    /// The ordered places that were probed, for a failed resolution.
    pub fn probed(&self) -> &[CredentialProbe] {
        match self {
            Self::Missing { probed } => probed,
            _ => &[],
        }
    }
}

/// A resolution plus its source. Deliberately does not carry the credential:
/// readiness surfaces need the source, and the request path already has its
/// own resolver that returns the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialResolution {
    pub source: CredentialSource,
}

impl CredentialResolution {
    pub fn found(source: CredentialSource) -> Self {
        debug_assert!(source.is_present());
        Self { source }
    }

    pub fn missing(probed: Vec<CredentialProbe>) -> Self {
        Self {
            source: CredentialSource::Missing { probed },
        }
    }

    pub fn is_present(&self) -> bool {
        self.source.is_present()
    }

    /// One line naming the places checked, for a status row. Empty when the
    /// resolution succeeded.
    pub fn checked_places(&self) -> String {
        self.source
            .probed()
            .iter()
            .map(|probe| probe.place.as_ref())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The first actionable fix among the probed places, if any.
    pub fn first_fix(&self) -> Option<&str> {
        self.source
            .probed()
            .iter()
            .find_map(|probe| probe.fix.as_deref())
    }
}

#[cfg(test)]
mod private_label_tests {
    use super::CredentialSource;
    #[test]
    fn keyless_label_omits_credentials_and_still_names_the_endpoint() {
        for url in [
            "https://user:label-s10-synthetic@gateway.invalid/v1",
            "https://label-s10-synthetic@gateway.invalid/v1",
            "https://gateway.invalid/key/label-s10-synthetic/v1",
            "https://gateway.invalid/v1?token=label-s10-synthetic",
            "https://gateway.invalid/v1#label-s10-synthetic",
        ] {
            let label = CredentialSource::KeylessRoute {
                base_url: url.into(),
            }
            .label()
            .into_owned();
            assert_eq!(label, "keyless route https://gateway.invalid");
        }
        let label = CredentialSource::KeylessRoute {
            base_url: "http://localhost:11434/v1".into(),
        }
        .label()
        .into_owned();
        assert_eq!(label, "keyless route http://localhost:11434");
        for url in ["not a url", "file:///etc/passwd", ""] {
            let label = CredentialSource::KeylessRoute {
                base_url: url.into(),
            }
            .label()
            .into_owned();
            assert!(label.contains("configured value omitted"));
            assert!(!label.contains("passwd"));
        }
    }
}
