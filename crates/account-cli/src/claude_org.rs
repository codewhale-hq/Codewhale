use chrono::{DateTime, Utc};
use codewhale_config::credentials::CLAUDE_ORG_LINK_COMMAND;

use super::*;

const TOKEN_PATH: &str = "/api/claude-billing/token";
const LINK_PATH: &str = "/api/claude-billing/link";
const STATUS_PATH: &str = "/api/claude-billing";
const LINK_START_PATH: &str = "/api/claude-billing/start";
const TOKEN_REFRESH_MARGIN_SECS: i64 = 120;
const LINK_POLL_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Args)]
pub(crate) struct ClaudeOrgArgs {
    #[command(subcommand)]
    command: ClaudeOrgCommand,
}

#[derive(Debug, Subcommand)]
enum ClaudeOrgCommand {
    /// Link a Claude Platform organization so Anthropic bills it directly.
    Link(ClaudeOrgLinkArgs),
    /// Show whether a Claude Platform organization is linked.
    Status,
    /// Remove the Claude Platform organization link.
    Unlink,
}

#[derive(Debug, Args)]
struct ClaudeOrgLinkArgs {
    /// Print the authorization URL without trying to open a browser.
    #[arg(long, default_value_t = false)]
    no_open: bool,
    /// Maximum time to wait for the browser authorization to finish.
    #[arg(
        long = "timeout-seconds",
        default_value_t = DEFAULT_LOGIN_TIMEOUT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..=MAX_LOGIN_TIMEOUT_SECONDS)
    )]
    timeout_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeOrgErrorKind {
    NotSignedIn,
    NotLinked,
    RelinkRequired,
    Disabled,
    AlreadyLinkedElsewhere,
    Unavailable,
    Invalid,
}

#[derive(Debug)]
pub struct ClaudeOrgError {
    pub kind: ClaudeOrgErrorKind,
    message: String,
}

impl ClaudeOrgError {
    #[must_use]
    pub fn with_message(kind: ClaudeOrgErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn new(kind: ClaudeOrgErrorKind) -> Self {
        let message = match kind {
            ClaudeOrgErrorKind::NotSignedIn => {
                "Sign in to Codewhale to use your linked Claude organization: run `codewhale login`."
                    .to_string()
            }
            ClaudeOrgErrorKind::NotLinked => format!(
                "No Claude organization is linked to your Codewhale account. Link one with `{CLAUDE_ORG_LINK_COMMAND}`."
            ),
            ClaudeOrgErrorKind::RelinkRequired => format!(
                "Your Claude organization link ended. Link again with `{CLAUDE_ORG_LINK_COMMAND}`."
            ),
            ClaudeOrgErrorKind::Disabled => {
                "Claude organization billing is not available for this Codewhale account."
                    .to_string()
            }
            ClaudeOrgErrorKind::AlreadyLinkedElsewhere => {
                "This Claude organization is already linked to another Codewhale account."
                    .to_string()
            }
            ClaudeOrgErrorKind::Unavailable => {
                "Claude organization billing is temporarily unavailable at Codewhale. Try again shortly."
                    .to_string()
            }
            ClaudeOrgErrorKind::Invalid => {
                "The Codewhale service returned an invalid Claude organization token.".to_string()
            }
        };
        Self { kind, message }
    }
}

impl std::fmt::Display for ClaudeOrgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClaudeOrgError {}

#[derive(Clone)]
pub struct ClaudeOrgToken {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    pub base_url: String,
    pub anthropic_version: String,
    pub user_profile_id: Option<String>,
}

impl std::fmt::Debug for ClaudeOrgToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeOrgToken")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("base_url", &self.base_url)
            .field("anthropic_version", &self.anthropic_version)
            .field("user_profile_id", &self.user_profile_id)
            .finish()
    }
}

impl ClaudeOrgToken {
    #[must_use]
    pub fn is_fresh_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.timestamp() - now.timestamp() > TOKEN_REFRESH_MARGIN_SECS
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenWire {
    token_type: String,
    access_token: String,
    expires_at: String,
    base_url: String,
    anthropic_version: String,
    user_profile_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkStart {
    authorization_url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeOrgLinkStatus {
    pub available: bool,
    pub linked: bool,
    pub linked_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnlinkResult {
    revoked: bool,
    message: Option<String>,
}

pub(crate) fn claude_org_token_from_response(
    response: &CloudResponse,
) -> std::result::Result<ClaudeOrgToken, ClaudeOrgError> {
    if response.status != 200 {
        return Err(claude_org_error(response));
    }
    let invalid = || ClaudeOrgError::new(ClaudeOrgErrorKind::Invalid);
    let wire: TokenWire = serde_json::from_slice(&response.body).map_err(|_| invalid())?;
    let header_safe = |value: &str| {
        !value.is_empty()
            && value.len() <= 8192
            && value.bytes().all(|byte| byte.is_ascii_graphic())
    };
    if !wire.token_type.eq_ignore_ascii_case("bearer")
        || !header_safe(&wire.access_token)
        || !header_safe(&wire.anthropic_version)
        || wire.anthropic_version.len() > 64
    {
        return Err(invalid());
    }
    let base = Url::parse(&wire.base_url).map_err(|_| invalid())?;
    if base.scheme() != "https" || base.host_str().is_none_or(str::is_empty) {
        return Err(invalid());
    }
    if wire
        .user_profile_id
        .as_deref()
        .is_some_and(|id| !valid_user_profile_id(id))
    {
        return Err(invalid());
    }
    let expires_at = DateTime::parse_from_rfc3339(&wire.expires_at)
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    Ok(ClaudeOrgToken {
        access_token: wire.access_token,
        expires_at,
        base_url: wire.base_url,
        anthropic_version: wire.anthropic_version,
        user_profile_id: wire.user_profile_id,
    })
}

fn valid_user_profile_id(id: &str) -> bool {
    id.strip_prefix("uprof_").is_some_and(|rest| {
        (1..=96).contains(&rest.len())
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

fn validate_link_start_url(value: &str, api_base: &str) -> Result<Url> {
    let unsafe_url = || anyhow!("The Codewhale service returned an unsafe Claude link URL");
    let url = Url::parse(value).map_err(|_| unsafe_url())?;
    let api = Url::parse(api_base).context("invalid Codewhale account API base URL")?;
    let loopback_test_api = api.scheme() == "http" && api.host_str().is_some_and(is_loopback_host);
    let scheme_ok = url.scheme() == "https" || (loopback_test_api && url.scheme() == "http");
    if value != url.as_str()
        || !scheme_ok
        || url.host_str().is_none()
        || url.host_str() != api.host_str()
        || url.port_or_known_default() != api.port_or_known_default()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != LINK_START_PATH
    {
        return Err(unsafe_url());
    }
    Ok(url)
}

fn response_code(response: &CloudResponse) -> Option<String> {
    response_error(response)
        .downcast_ref::<CloudHttpError>()
        .and_then(|error| error.code().map(str::to_string))
}

fn claude_org_error(response: &CloudResponse) -> ClaudeOrgError {
    let kind = match (response.status, response_code(response).as_deref()) {
        (401, _) => ClaudeOrgErrorKind::NotSignedIn,
        (_, Some("claude_not_linked")) => ClaudeOrgErrorKind::NotLinked,
        (_, Some("claude_relink_required")) => ClaudeOrgErrorKind::RelinkRequired,
        (_, Some("claude_billing_disabled")) | (404, None) => ClaudeOrgErrorKind::Disabled,
        (_, Some("claude_already_linked")) => ClaudeOrgErrorKind::AlreadyLinkedElsewhere,
        (_, Some("claude_refresh_busy")) => ClaudeOrgErrorKind::Unavailable,
        (500..=599, _) => ClaudeOrgErrorKind::Unavailable,
        _ => ClaudeOrgErrorKind::Invalid,
    };
    ClaudeOrgError::new(kind)
}

fn expect_claude_org_json<T: DeserializeOwned>(response: CloudResponse) -> Result<T> {
    if !(200..300).contains(&response.status) {
        let error = claude_org_error(&response);
        if error.kind == ClaudeOrgErrorKind::Invalid {
            return Err(response_error(&response));
        }
        return Err(error.into());
    }
    parse_json_body(&response.body)
}

impl AccountClient {
    pub fn claude_org_token(&self) -> std::result::Result<ClaudeOrgToken, ClaudeOrgError> {
        let mut rejected: Option<String> = None;
        loop {
            let lease = self
                .fresh_auth(rejected.as_deref())
                .map_err(|_| ClaudeOrgError::new(ClaudeOrgErrorKind::Unavailable))?
                .ok_or_else(|| ClaudeOrgError::new(ClaudeOrgErrorKind::NotSignedIn))?;
            let bearer = lease.bundle.access_token.clone();
            let response = self
                .transport
                .execute(CloudRequest {
                    method: HttpMethod::Post,
                    path: TOKEN_PATH.into(),
                    bearer: Some(bearer.clone()),
                    body: Some(b"{}".to_vec()),
                })
                .map_err(|_| ClaudeOrgError::new(ClaudeOrgErrorKind::Unavailable))?;
            if response.status == 401 && rejected.is_none() {
                rejected = Some(bearer);
                continue;
            }
            return claude_org_token_from_response(&response);
        }
    }

    pub fn claude_org_link_status(
        &self,
    ) -> std::result::Result<ClaudeOrgLinkStatus, ClaudeOrgError> {
        self.signed_in()?;
        link_status(&self.client()).map_err(|error| self.classify(error))
    }

    pub fn claude_org_start_link(&self) -> std::result::Result<String, ClaudeOrgError> {
        self.signed_in()?;
        start_link(&self.client(), &self.api_base)
            .map(|url| url.to_string())
            .map_err(|error| self.classify(error))
    }

    fn signed_in(&self) -> std::result::Result<(), ClaudeOrgError> {
        self.fresh_auth(None)
            .map_err(|_| ClaudeOrgError::new(ClaudeOrgErrorKind::Unavailable))?
            .map(|_| ())
            .ok_or_else(|| ClaudeOrgError::new(ClaudeOrgErrorKind::NotSignedIn))
    }

    fn classify(&self, error: anyhow::Error) -> ClaudeOrgError {
        if outcome_unknown(&error) {
            return ClaudeOrgError::new(ClaudeOrgErrorKind::Unavailable);
        }
        match error.downcast::<ClaudeOrgError>() {
            Ok(error) => error,
            Err(_) if self.signed_in().is_err() => {
                ClaudeOrgError::new(ClaudeOrgErrorKind::NotSignedIn)
            }
            Err(error) => {
                ClaudeOrgError::with_message(ClaudeOrgErrorKind::Invalid, error.to_string())
            }
        }
    }
}

fn link_status<T: CloudTransport>(client: &CloudClient<'_, T>) -> Result<ClaudeOrgLinkStatus> {
    expect_claude_org_json(client.execute_authenticated(HttpMethod::Get, STATUS_PATH, None)?)
}

fn start_link<T: CloudTransport>(client: &CloudClient<'_, T>, api_base: &str) -> Result<Url> {
    let start: LinkStart = expect_claude_org_json(client.execute_authenticated(
        HttpMethod::Post,
        LINK_PATH,
        Some(b"{}".to_vec()),
    )?)?;
    validate_link_start_url(&start.authorization_url, api_base)
}

fn transient_poll_error(error: &anyhow::Error) -> bool {
    outcome_unknown(error)
        || error
            .downcast_ref::<ClaudeOrgError>()
            .is_some_and(|error| error.kind == ClaudeOrgErrorKind::Unavailable)
}

pub(crate) fn run_claude_org<T: CloudTransport, W: Write>(
    args: ClaudeOrgArgs,
    client: &CloudClient<'_, T>,
    api_base: &str,
    out: &mut W,
    opener: &mut dyn FnMut(String) -> bool,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<()> {
    let unavailable = || anyhow::Error::new(ClaudeOrgError::new(ClaudeOrgErrorKind::Disabled));
    match args.command {
        ClaudeOrgCommand::Link(link) => {
            let before = link_status(client)?;
            if !before.available {
                return Err(unavailable());
            }
            if before.linked {
                writeln!(
                    out,
                    "A Claude organization is already linked to your Codewhale account."
                )?;
                writeln!(
                    out,
                    "Run `codewhale account claude-org unlink` first to link a different one."
                )?;
                return Ok(());
            }
            let url = start_link(client, api_base)?;
            writeln!(out, "Link a Claude Platform organization")?;
            writeln!(out, "Open: {url}")?;
            if !link.no_open && !opener(url.to_string()) {
                writeln!(out, "Browser could not be opened; use the URL above.")?;
            }
            writeln!(out, "Waiting for the link to finish in your browser...")?;
            let deadline = std::time::Instant::now() + Duration::from_secs(link.timeout_seconds);
            let mut tolerated_error = false;
            loop {
                sleeper(LINK_POLL_INTERVAL);
                match link_status(client) {
                    Ok(status) if !status.available => return Err(unavailable()),
                    Ok(status) if status.linked => {
                        writeln!(
                            out,
                            "Linked. Anthropic now bills your Claude organization directly for requests Codewhale sends with it."
                        )?;
                        writeln!(
                            out,
                            "To use it, set `auth_mode = \"claude-org\"` under `[providers.anthropic]` in your Codewhale config."
                        )?;
                        return Ok(());
                    }
                    Ok(_) => tolerated_error = false,
                    Err(error) if !tolerated_error && transient_poll_error(&error) => {
                        tolerated_error = true;
                    }
                    Err(error) => return Err(error),
                }
                if std::time::Instant::now() >= deadline {
                    bail!(
                        "The Claude organization link did not finish in time. Run `{CLAUDE_ORG_LINK_COMMAND}` to try again."
                    );
                }
            }
        }
        ClaudeOrgCommand::Status => {
            let status = link_status(client)?;
            if !status.available {
                writeln!(
                    out,
                    "Claude organization billing is not available for this Codewhale account."
                )?;
            } else if status.linked {
                match status.linked_at.as_deref().map(printable) {
                    Some(at) if !at.is_empty() => {
                        writeln!(out, "Claude organization linked (since {at}).")?
                    }
                    _ => writeln!(out, "Claude organization linked.")?,
                }
            } else {
                writeln!(out, "No Claude organization is linked.")?;
                writeln!(out, "Run `{CLAUDE_ORG_LINK_COMMAND}` to link one.")?;
            }
            Ok(())
        }
        ClaudeOrgCommand::Unlink => {
            let result: UnlinkResult = expect_claude_org_json(client.execute_authenticated(
                HttpMethod::Delete,
                STATUS_PATH,
                None,
            )?)?;
            writeln!(
                out,
                "Claude organization unlinked from your Codewhale account."
            )?;
            if !result.revoked {
                writeln!(
                    out,
                    "Anthropic did not confirm the revocation; remove Codewhale under Linked platforms in the Claude Console if it is still listed."
                )?;
            }
            if let Some(message) = result.message.as_deref().map(printable)
                && !message.is_empty()
            {
                writeln!(out, "{message}")?;
            }
            Ok(())
        }
    }
}
