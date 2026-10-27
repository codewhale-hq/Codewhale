// Audit-write failure must route through `tracing::*`, not raw stderr —
// see `runtime_log` for the scroll-demon rationale.
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

//! Per-domain network policy for outbound network calls (#135).
//!
//! Three small pieces:
//!
//! 1. [`Decision`] — `Allow | Deny | Prompt`.
//! 2. [`NetworkPolicy`] — a list of allow/deny hostnames + a default decision,
//!    with **deny-wins precedence**: a host that matches an entry in `deny`
//!    is denied even if it also matches `allow`.
//! 3. [`NetworkAuditor`] — appends one plaintext line per outbound call to
//!    `~/.codewhale/audit.log` in the format described below.
//!
//! In addition, [`NetworkSessionCache`] holds in-process "approve once for
//! this session" state for the `Prompt` flow, and [`NetworkDenied`] is the
//! structured error surfaced to callers when a host is blocked.
//!
//! # Host-matching rules
//!
//! * **Exact match** — an entry like `api.deepseek.com` matches only the host
//!   `api.deepseek.com` (case-insensitive).
//! * **Subdomain match** — an entry that **starts with a leading dot**, e.g.
//!   `.example.com`, matches any subdomain (`api.example.com`, `a.b.example.com`)
//!   but **not** the apex `example.com`. To match both, list both.
//!
//! Matching is case-insensitive and trims a single trailing dot from the host
//! (so `example.com.` and `example.com` are equivalent).
//!
//! # Audit-log format
//!
//! ```text
//! <RFC3339-timestamp> network <host> <tool> <Allow|Deny|Prompt-Approved|Prompt-Denied|TrustedProxyFakeIp-Allow>
//! ```
//!
//! Plaintext, one line per call, appended to `<audit_path>` (defaults to
//! `~/.codewhale/audit.log`). Best-effort: write failures are logged but do
//! not block the call.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What the policy decided about an outbound network call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allow the call without prompting.
    Allow,
    /// Deny the call. Surfaced to callers as [`NetworkDenied`].
    Deny,
    /// Defer to the user via an approval prompt.
    Prompt,
}

impl Decision {
    /// String form used in audit-log lines.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::Deny => "Deny",
            Self::Prompt => "Prompt",
        }
    }

    /// Parse a decision from a TOML string. Unknown values fall back to
    /// `Prompt` so a typo never silently disables the policy.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "allow" => Self::Allow,
            "deny" | "block" => Self::Deny,
            _ => Self::Prompt,
        }
    }
}

/// Per-domain allow/deny list with a default fallback.
///
/// See the module docs for [host-matching rules](self#host-matching-rules)
/// and [deny-wins precedence](self#deny-wins-precedence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Decision for hosts that match neither `allow` nor `deny`.
    #[serde(default = "default_decision")]
    pub default: DecisionToml,
    /// Hosts that should be allowed without prompting.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Hosts that should always be denied.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Hostnames whose DNS may resolve to fake-IP/private proxy ranges in an
    /// explicitly trusted proxy setup. This does not affect literal IP URLs.
    #[serde(default)]
    pub proxy: Vec<String>,
    /// Explicit fake-IP placeholder CIDRs used by the trusted proxy setup.
    /// Only subnets contained by the IETF benchmark range `198.18.0.0/15`
    /// are eligible; loopback, RFC1918, link-local, metadata, and ULA ranges
    /// can never be trusted through this setting.
    #[serde(default)]
    pub proxy_fake_ip_cidrs: Vec<String>,
    /// Whether to record one audit-log line per network call. Defaults to true.
    #[serde(default = "default_audit")]
    pub audit: bool,
}

fn default_decision() -> DecisionToml {
    DecisionToml::Prompt
}

/// The stricter of two fallback decisions, ordering `Deny` < `Prompt` < `Allow`.
/// Used when a lower-precedence layer is folded onto a policy an authority set.
fn stricter_decision(left: DecisionToml, right: DecisionToml) -> DecisionToml {
    fn rank(decision: DecisionToml) -> u8 {
        match decision {
            DecisionToml::Deny => 0,
            DecisionToml::Prompt => 1,
            DecisionToml::Allow => 2,
        }
    }
    if rank(left) <= rank(right) {
        left
    } else {
        right
    }
}

fn default_audit() -> bool {
    true
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            default: DecisionToml::Prompt,
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: true,
        }
    }
}

/// Wire-format wrapper for [`Decision`] used in serde-derived TOML/JSON. The
/// runtime API exposes [`Decision`] directly; this type only exists so
/// `default = "prompt"` round-trips cleanly through TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionToml {
    Allow,
    Deny,
    Prompt,
}

impl From<DecisionToml> for Decision {
    fn from(value: DecisionToml) -> Self {
        match value {
            DecisionToml::Allow => Self::Allow,
            DecisionToml::Deny => Self::Deny,
            DecisionToml::Prompt => Self::Prompt,
        }
    }
}

impl From<Decision> for DecisionToml {
    fn from(value: Decision) -> Self {
        match value {
            Decision::Allow => Self::Allow,
            Decision::Deny => Self::Deny,
            Decision::Prompt => Self::Prompt,
        }
    }
}

impl From<codewhale_config::NetworkPolicyToml> for NetworkPolicy {
    fn from(toml: codewhale_config::NetworkPolicyToml) -> Self {
        Self {
            default: Decision::parse(&toml.default).into(),
            allow: toml.allow,
            deny: toml.deny,
            proxy: toml.proxy,
            proxy_fake_ip_cidrs: toml.proxy_fake_ip_cidrs,
            audit: toml.audit,
        }
    }
}

/// Re-read the `[network]` table from a configuration document.
///
/// Deliberately narrower than `Config::load`: this runs on the tool-context
/// build path, where re-applying the environment, managed, and credential
/// layers would be both wasteful and wrong. Only the document's own table comes
/// back — the same table `/network allow <host>` edits.
///
/// `None` means there is nothing usable to adopt: the document is missing,
/// unreadable, unparseable, or carries no `[network]` table. Callers keep the
/// policy they already hold, which is the conservative direction for an
/// allow/deny gate.
///
/// Known limitation: `[profiles.<name>.network]` is not consulted. `/network`
/// does not write it either, so a live session and the command agree on this
/// base table.
#[must_use]
pub fn network_policy_from_document(path: &Path) -> Option<NetworkPolicy> {
    #[derive(Deserialize)]
    struct NetworkTable {
        network: Option<codewhale_config::NetworkPolicyToml>,
    }

    let contents = fs::read_to_string(path).ok()?;
    let parsed: NetworkTable = toml::from_str(&contents).ok()?;
    parsed.network.map(NetworkPolicy::from)
}

impl NetworkPolicy {
    /// Fold a lower-precedence layer (the user's config document) onto this
    /// policy without letting it widen what a higher authority set.
    ///
    /// The fold is deliberately asymmetric, because the two layers are not
    /// peers:
    ///
    /// * `deny` is the **union**. Either layer saying no is a no.
    /// * `default` keeps the **stricter** of the two.
    /// * When the authority's fallback is `Deny`, the lower layer contributes
    ///   **nothing that can allow**: `allow`, `proxy`, and the fake-IP CIDRs
    ///   stay the authority's own. `decide` checks `allow` *before* falling back
    ///   to `default`, so adopting a lower `allow` entry under a
    ///   deny-by-default authority would let the document lift exactly the
    ///   denial the authority set.
    /// * Otherwise the authority is prompt-by-default, and the lower layer may
    ///   do what `/network allow <host>` writes: name additional hosts. Those
    ///   merge with the authority's own rather than replacing them, so a
    ///   refresh cannot erase an administrator's allowances either.
    /// * `audit` is the one field that widens by `true`: either layer asking for
    ///   the audit log keeps it, and a document cannot silence it under an
    ///   authority.
    #[must_use]
    pub fn folded_with_lower_layer(&self, lower: Self) -> Self {
        fn union(mut base: Vec<String>, extra: Vec<String>) -> Vec<String> {
            for entry in extra {
                if !base.iter().any(|existing| existing == &entry) {
                    base.push(entry);
                }
            }
            base
        }

        let deny_by_default = self.default == DecisionToml::Deny;
        let (allow, proxy, proxy_fake_ip_cidrs) = if deny_by_default {
            (
                self.allow.clone(),
                self.proxy.clone(),
                self.proxy_fake_ip_cidrs.clone(),
            )
        } else {
            (
                union(self.allow.clone(), lower.allow),
                union(self.proxy.clone(), lower.proxy),
                union(self.proxy_fake_ip_cidrs.clone(), lower.proxy_fake_ip_cidrs),
            )
        };
        Self {
            default: stricter_decision(self.default, lower.default),
            allow,
            deny: union(self.deny.clone(), lower.deny),
            proxy,
            proxy_fake_ip_cidrs,
            audit: self.audit || lower.audit,
        }
    }

    /// Decide what to do for a single outbound call to `host`.
    ///
    /// **Deny-wins precedence**: if `host` matches any entry in `deny`, the
    /// answer is [`Decision::Deny`] regardless of `allow`. This makes deny
    /// lists safe to combine with broad allow rules.
    #[must_use]
    pub fn decide(&self, host: &str) -> Decision {
        let normalized = normalize_host(host);
        if normalized.is_empty() {
            // We don't pretend we can audit a malformed host; treat it as the
            // default (prompt or deny).
            return self.default.into();
        }
        if self
            .deny
            .iter()
            .any(|entry| host_matches(entry, &normalized))
        {
            return Decision::Deny;
        }
        if self
            .allow
            .iter()
            .any(|entry| host_matches(entry, &normalized))
        {
            return Decision::Allow;
        }
        self.default.into()
    }

    /// Append `host` to the allow list (de-duplicated, case-insensitive).
    /// Used by the prompt flow when the user picks "always for this host".
    pub fn add_allow(&mut self, host: &str) {
        let normalized = normalize_host(host);
        if normalized.is_empty() {
            return;
        }
        if !self
            .allow
            .iter()
            .any(|existing| normalize_host(existing) == normalized)
        {
            self.allow.push(normalized);
        }
    }

    /// Whether audit logging is enabled.
    #[must_use]
    pub fn audit_enabled(&self) -> bool {
        self.audit
    }

    /// Whether `host` is explicitly trusted to resolve through a local
    /// fake-IP proxy. Deny entries still win over this list.
    #[must_use]
    pub fn trusts_proxy_fakeip_host(&self, host: &str) -> bool {
        let normalized = normalize_host(host);
        if normalized.is_empty() {
            return false;
        }
        if self
            .deny
            .iter()
            .any(|entry| host_matches(entry, &normalized))
        {
            return false;
        }
        self.proxy
            .iter()
            .any(|entry| host_matches(entry, &normalized))
    }
}

/// Normalize a host for matching: lowercase, trim whitespace, strip a single
/// trailing dot (FQDN form), and strip a leading `*.` or `.` for entries that
/// are written that way in config (we treat both as subdomain wildcards on
/// the *match* side, but on input normalization we keep the leading dot so
/// `host_matches` can detect the wildcard intent).
fn normalize_host(host: &str) -> String {
    let trimmed = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if let Some(rest) = trimmed.strip_prefix("*.") {
        format!(".{rest}")
    } else {
        trimmed
    }
}

/// Match a single allow/deny entry against an already-normalized host.
fn host_matches(entry: &str, normalized_host: &str) -> bool {
    let entry_norm = normalize_host(entry);
    if let Some(suffix) = entry_norm.strip_prefix('.') {
        // Wildcard subdomain rule. Match any host ending in `.suffix`, but
        // *not* the bare `suffix` itself (per spec).
        if suffix.is_empty() {
            return false;
        }
        normalized_host.ends_with(&format!(".{suffix}"))
    } else {
        entry_norm == normalized_host
    }
}

/// Parse an IPv4 CIDR string such as `"198.18.0.0/15"` into `(base, prefix)`.
/// Returns `None` for malformed input or a prefix length above 32.
fn parse_ipv4_cidr(cidr: &str) -> Option<(Ipv4Addr, u8)> {
    let (addr, prefix) = cidr.split_once('/')?;
    let base: Ipv4Addr = addr.trim().parse().ok()?;
    let prefix: u8 = prefix.trim().parse().ok()?;
    if prefix > 32 {
        return None;
    }
    Some((base, prefix))
}

/// Parse only fake-IP networks that are fully contained by the IETF benchmark
/// block. This keeps an overly broad or mistaken config entry from weakening
/// the unconditional loopback/private/link-local/metadata protections.
fn parse_trusted_fakeip_cidr(cidr: &str) -> Option<(Ipv4Addr, u8)> {
    let (base, prefix) = parse_ipv4_cidr(cidr)?;
    let octets = base.octets();
    (prefix >= 15 && octets[0] == 198 && matches!(octets[1], 18..=19)).then_some((base, prefix))
}

/// Whether `ip` is contained in the `base/prefix` IPv4 CIDR block.
fn ipv4_in_cidr(ip: Ipv4Addr, base: Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask: u32 = u32::MAX << (32 - prefix);
    (u32::from(ip) & mask) == (u32::from(base) & mask)
}

/// Best-effort writer for the network audit log.
#[derive(Debug, Clone)]
pub struct NetworkAuditor {
    path: PathBuf,
    enabled: bool,
}

impl NetworkAuditor {
    /// New auditor that writes to `path`. `enabled = false` turns it into a no-op.
    #[must_use]
    pub fn new(path: PathBuf, enabled: bool) -> Self {
        Self { path, enabled }
    }

    /// Auditor pointing at the same `audit.log` every other audit event uses:
    /// `$CODEWHALE_HOME/audit.log`, else `~/.codewhale/audit.log`. It used to
    /// join `$HOME/.codewhale` itself, which ignored `CODEWHALE_HOME` and let
    /// test processes append to the developer's real log. Returns `None` if
    /// no Codewhale home resolves.
    #[must_use]
    pub fn default_path(enabled: bool) -> Option<Self> {
        Some(Self::new(
            codewhale_runtime::audit::audit_log_path()?,
            enabled,
        ))
    }

    /// Append one line. Best-effort: errors are logged via `tracing` but
    /// never bubble back to the caller.
    pub fn record(&self, host: &str, tool: &str, decision_label: &str) {
        if !self.enabled {
            return;
        }
        if let Err(err) = self.try_record(host, tool, decision_label) {
            // Routed through tracing so it lands in
            // `~/.codewhale/logs/tui-YYYY-MM-DD.log` rather than the
            // alt-screen — see `runtime_log` for the scroll-demon
            // rationale.
            tracing::warn!(target: "network_policy", ?err, host, tool, "network audit write failed");
        }
    }

    fn try_record(&self, host: &str, tool: &str, decision_label: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(
            file,
            "{ts} network {host} {tool} {decision}",
            ts = Utc::now().to_rfc3339(),
            host = sanitize_field(host),
            tool = sanitize_field(tool),
            decision = decision_label,
        )
    }

    /// Path the auditor would write to. Mostly useful for tests.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Replace whitespace in a token so the line stays parseable.
fn sanitize_field(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// In-process cache of "approve once for this session" decisions. Keyed by
/// normalized host. Thread-safe.
#[derive(Debug, Default, Clone)]
pub struct NetworkSessionCache {
    inner: Arc<Mutex<NetworkSessionCacheInner>>,
}

#[derive(Debug, Default)]
struct NetworkSessionCacheInner {
    approved: std::collections::HashSet<String>,
    denied: std::collections::HashSet<String>,
}

impl NetworkSessionCache {
    /// New empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if the host was previously approved this session.
    #[must_use]
    pub fn is_approved(&self, host: &str) -> bool {
        let normalized = normalize_host(host);
        self.inner
            .lock()
            .map(|guard| guard.approved.contains(&normalized))
            .unwrap_or(false)
    }

    /// `true` if the host was previously denied this session.
    #[must_use]
    pub fn is_denied(&self, host: &str) -> bool {
        let normalized = normalize_host(host);
        self.inner
            .lock()
            .map(|guard| guard.denied.contains(&normalized))
            .unwrap_or(false)
    }

    /// Mark the host as approved for the rest of this session.
    pub fn approve(&self, host: &str) {
        let normalized = normalize_host(host);
        if let Ok(mut guard) = self.inner.lock() {
            guard.denied.remove(&normalized);
            guard.approved.insert(normalized);
        }
    }

    /// Mark the host as denied for the rest of this session.
    pub fn deny(&self, host: &str) {
        let normalized = normalize_host(host);
        if let Ok(mut guard) = self.inner.lock() {
            guard.approved.remove(&normalized);
            guard.denied.insert(normalized);
        }
    }
}

/// Structured error surfaced to callers when an outbound call is blocked.
#[derive(Debug, Clone, Error)]
#[error("network call to '{0}' blocked by network policy")]
pub struct NetworkDenied(pub String);

impl NetworkDenied {
    /// The host that was denied.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.0
    }
}

/// Glue type that bundles a [`NetworkPolicy`] with a session cache and an
/// auditor. Tools call [`NetworkPolicyDecider::evaluate`] before any HTTP
/// transport is constructed; the result decides whether to proceed, deny,
/// or prompt the user.
#[derive(Debug, Clone)]
pub struct NetworkPolicyDecider {
    policy: NetworkPolicy,
    cache: NetworkSessionCache,
    auditor: Option<NetworkAuditor>,
    /// IPv4 CIDR ranges that are treated as benign fake-IP placeholders (e.g.
    /// a transparent-proxy / TUN setup running in `fake-ip` mode, where DNS
    /// resolves every hostname into a reserved range like `198.18.0.0/15`).
    /// A resolved IP inside one of these ranges bypasses the restricted-IP SSRF
    /// block; real private/loopback/link-local/metadata IPs are unaffected.
    trusted_fakeip_cidrs: Vec<(Ipv4Addr, u8)>,
    /// The policy was produced by an authority the user's config document may
    /// not override — a managed layer, or a Fleet denial the user is not
    /// allowed to widen. A mid-session re-read of that document then folds onto
    /// this policy instead of replacing it. See
    /// [`NetworkPolicy::folded_with_lower_layer`].
    authoritative: bool,
}

impl NetworkPolicyDecider {
    /// Build a decider from a policy. The session cache starts empty.
    #[must_use]
    pub fn new(policy: NetworkPolicy, auditor: Option<NetworkAuditor>) -> Self {
        let trusted_fakeip_cidrs = policy
            .proxy_fake_ip_cidrs
            .iter()
            .filter_map(|cidr| parse_trusted_fakeip_cidr(cidr))
            .collect();
        Self {
            policy,
            cache: NetworkSessionCache::new(),
            auditor,
            trusted_fakeip_cidrs,
            authoritative: false,
        }
    }

    /// Mark this policy as set by an authority the user document may not
    /// override. See the `authoritative` field.
    #[must_use]
    pub fn with_authoritative(mut self) -> Self {
        self.authoritative = true;
        self
    }

    /// Whether an authority set this policy rather than the user document.
    #[must_use]
    pub fn is_authoritative(&self) -> bool {
        self.authoritative
    }

    /// Register IPv4 CIDR ranges to treat as benign fake-IP placeholders.
    /// Invalid CIDR strings are skipped. See [`Self::is_trusted_fakeip_addr`].
    #[must_use]
    pub fn with_trusted_fakeip_cidrs(mut self, cidrs: &[&str]) -> Self {
        for cidr in cidrs {
            if let Some(parsed) = parse_trusted_fakeip_cidr(cidr) {
                self.trusted_fakeip_cidrs.push(parsed);
            }
        }
        self
    }

    /// Whether `ip` falls inside a configured fake-IP placeholder range.
    ///
    /// In `fake-ip` proxy/TUN setups the local resolver maps every hostname to
    /// a reserved range (commonly `198.18.0.0/15`), so the DNS-resolution SSRF
    /// check would otherwise reject every request. This narrowly trusts only
    /// those placeholder addresses — real private/loopback/link-local/cloud-
    /// metadata IPs are *not* matched and stay blocked.
    #[must_use]
    pub fn is_trusted_fakeip_addr(&self, ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self
                .trusted_fakeip_cidrs
                .iter()
                .any(|(base, prefix)| ipv4_in_cidr(*v4, *base, *prefix)),
            // fake-ip placeholders are IPv4-only in practice.
            IpAddr::V6(_) => false,
        }
    }

    /// Convenience: build a decider with default audit logging at
    /// `~/.codewhale/audit.log`, if `policy.audit` is true.
    #[must_use]
    pub fn with_default_audit(policy: NetworkPolicy) -> Self {
        let audit_enabled = policy.audit_enabled();
        let auditor = if audit_enabled {
            NetworkAuditor::default_path(true)
        } else {
            None
        };
        Self::new(policy, auditor)
    }

    /// Rebuild this decider against a policy re-read from disk, keeping the
    /// session cache so an approval already granted in this session survives
    /// the refresh.
    ///
    /// The engine snapshots its policy when it is spawned, and `/network allow
    /// <host>` only edits the configuration document — it cannot reach that
    /// snapshot. Callers hand the re-read table here instead of waiting for a
    /// restart. The auditor keeps its log path and follows the new policy's
    /// `audit` switch.
    ///
    /// Extra CIDRs registered through [`Self::with_trusted_fakeip_cidrs`] are
    /// not carried over; the refresh re-derives them from `policy`. Configured
    /// fake-IP ranges already ride in the policy, and no production caller
    /// registers extras.
    #[must_use]
    pub fn with_policy_refreshed(&self, policy: NetworkPolicy) -> Self {
        let auditor = match self.auditor.as_ref() {
            Some(existing) => Some(NetworkAuditor::new(
                existing.path().to_path_buf(),
                policy.audit_enabled(),
            )),
            None if policy.audit_enabled() => NetworkAuditor::default_path(true),
            None => None,
        };
        Self {
            trusted_fakeip_cidrs: policy
                .proxy_fake_ip_cidrs
                .iter()
                .filter_map(|cidr| parse_trusted_fakeip_cidr(cidr))
                .collect(),
            policy,
            cache: self.cache.clone(),
            auditor,
            authoritative: self.authoritative,
        }
    }

    /// Inspect the policy.
    #[must_use]
    pub fn policy(&self) -> &NetworkPolicy {
        &self.policy
    }

    #[must_use]
    pub fn denies_by_default(&self) -> bool {
        self.policy.default == DecisionToml::Deny
    }

    /// Inspect the session cache.
    #[must_use]
    pub fn cache(&self) -> &NetworkSessionCache {
        &self.cache
    }

    /// Decide for `host`, consulting the session cache first.
    ///
    /// Audit logging happens **only** for terminal decisions (Allow / Deny).
    /// `Prompt` is intentionally not logged here — the caller is responsible
    /// for recording the user's eventual answer with `record_prompt_outcome`.
    #[must_use]
    pub fn evaluate(&self, host: &str, tool: &str) -> Decision {
        let normalized = normalize_host(host);
        if normalized.is_empty() {
            return self.policy.default.into();
        }
        if self.cache.is_denied(&normalized) {
            self.audit_record(&normalized, tool, "Deny");
            return Decision::Deny;
        }
        if self.cache.is_approved(&normalized) {
            self.audit_record(&normalized, tool, "Allow");
            return Decision::Allow;
        }
        let decision = self.policy.decide(&normalized);
        match decision {
            Decision::Allow => self.audit_record(&normalized, tool, "Allow"),
            Decision::Deny => self.audit_record(&normalized, tool, "Deny"),
            Decision::Prompt => {}
        }
        decision
    }

    /// Approve `host` for the rest of the session (one-shot). Audit log gets
    /// `Prompt-Approved`.
    pub fn approve_session(&self, host: &str, tool: &str) {
        self.cache.approve(host);
        self.audit_record(host, tool, "Prompt-Approved");
    }

    /// Deny `host` for the rest of the session. Audit log gets `Prompt-Denied`.
    pub fn deny_session(&self, host: &str, tool: &str) {
        self.cache.deny(host);
        self.audit_record(host, tool, "Prompt-Denied");
    }

    /// Persist `host` into the policy's allow list (so it survives the session)
    /// **and** approve it in-session. Returns the updated policy so callers can
    /// write it back to disk.
    pub fn approve_persistent(&mut self, host: &str, tool: &str) -> &NetworkPolicy {
        self.policy.add_allow(host);
        self.cache.approve(host);
        self.audit_record(host, tool, "Prompt-Approved");
        &self.policy
    }

    /// Whether this host is explicitly configured for trusted proxy fake-IP
    /// DNS handling.
    #[must_use]
    pub fn trusts_proxy_fakeip_host(&self, host: &str) -> bool {
        self.policy.trusts_proxy_fakeip_host(host)
    }

    /// Record that a restricted DNS result was allowed because the host is in
    /// the trusted proxy fake-IP list.
    pub fn record_trusted_proxy_fakeip_allow(&self, host: &str, tool: &str) {
        self.audit_record(host, tool, "TrustedProxyFakeIp-Allow");
    }

    fn audit_record(&self, host: &str, tool: &str, label: &str) {
        if let Some(auditor) = self.auditor.as_ref() {
            auditor.record(host, tool, label);
        }
    }
}

/// Extract the host portion of a URL, lowercased. Returns `None` if the URL
/// can't be parsed or has no host.
#[must_use]
pub fn host_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    parsed.host_str().map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn mk(default: Decision, allow: &[&str], deny: &[&str]) -> NetworkPolicy {
        NetworkPolicy {
            default: default.into(),
            allow: allow.iter().map(|s| (*s).to_string()).collect(),
            deny: deny.iter().map(|s| (*s).to_string()).collect(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        }
    }

    /// The network auditor writes to the one `audit.log` (which follows
    /// `CODEWHALE_HOME`); it used to join `$HOME/.codewhale` itself.
    #[test]
    fn default_auditor_writes_to_the_shared_audit_log() {
        assert_eq!(
            NetworkAuditor::default_path(true).map(|auditor| auditor.path),
            codewhale_runtime::audit::audit_log_path()
        );
    }

    #[test]
    fn exact_match_in_allow_returns_allow() {
        let p = mk(Decision::Deny, &["api.deepseek.com"], &[]);
        assert_eq!(p.decide("api.deepseek.com"), Decision::Allow);
    }

    #[test]
    fn unknown_host_returns_default() {
        let p = mk(Decision::Deny, &["api.deepseek.com"], &[]);
        assert_eq!(p.decide("evil.example.com"), Decision::Deny);

        let p2 = mk(Decision::Prompt, &[], &[]);
        assert_eq!(p2.decide("anything.example"), Decision::Prompt);
    }

    #[test]
    fn deny_wins_precedence() {
        // Acceptance criterion: a host in both allow and deny is denied.
        let p = mk(Decision::Prompt, &["api.example.com"], &["api.example.com"]);
        assert_eq!(p.decide("api.example.com"), Decision::Deny);
    }

    #[test]
    fn deny_wins_with_subdomain_rules() {
        // Deny-wins applies even when the deny is a wildcard and the allow is exact.
        let p = mk(Decision::Allow, &["api.example.com"], &[".example.com"]);
        assert_eq!(p.decide("api.example.com"), Decision::Deny);
    }

    #[test]
    fn subdomain_wildcard_matches_subdomain_only() {
        let p = mk(Decision::Deny, &[".example.com"], &[]);
        assert_eq!(p.decide("api.example.com"), Decision::Allow);
        assert_eq!(p.decide("a.b.example.com"), Decision::Allow);
        // The bare apex is *not* matched by `.example.com` per the rule.
        assert_eq!(p.decide("example.com"), Decision::Deny);
    }

    #[test]
    fn star_dot_subdomain_alias_is_accepted() {
        let p = mk(Decision::Deny, &["*.example.com"], &[]);
        assert_eq!(p.decide("api.example.com"), Decision::Allow);
        assert_eq!(p.decide("example.com"), Decision::Deny);
    }

    #[test]
    fn host_match_is_case_insensitive() {
        let p = mk(Decision::Deny, &["API.DeepSeek.com"], &[]);
        assert_eq!(p.decide("api.deepseek.com"), Decision::Allow);
    }

    #[test]
    fn trailing_dot_is_ignored() {
        let p = mk(Decision::Deny, &["api.deepseek.com"], &[]);
        assert_eq!(p.decide("api.deepseek.com."), Decision::Allow);
    }

    #[test]
    fn empty_host_uses_default() {
        let p = mk(Decision::Deny, &["api.example.com"], &[]);
        assert_eq!(p.decide(""), Decision::Deny);
        assert_eq!(p.decide("   "), Decision::Deny);
    }

    #[test]
    fn add_allow_dedupes_case_insensitively() {
        let mut p = mk(Decision::Deny, &[], &[]);
        p.add_allow("Example.COM");
        p.add_allow("example.com");
        assert_eq!(p.allow.len(), 1);
        assert_eq!(p.allow[0], "example.com");
    }

    #[test]
    fn trusted_proxy_fakeip_hosts_match_exact_and_subdomains() {
        let mut p = mk(Decision::Deny, &[], &[]);
        p.proxy = vec![
            "github.com".to_string(),
            ".githubusercontent.com".to_string(),
        ];

        assert!(p.trusts_proxy_fakeip_host("github.com"));
        assert!(p.trusts_proxy_fakeip_host("raw.githubusercontent.com"));
        assert!(!p.trusts_proxy_fakeip_host("githubusercontent.com"));
        assert!(!p.trusts_proxy_fakeip_host("example.com"));
    }

    #[test]
    fn trusted_proxy_fakeip_hosts_respect_deny_precedence() {
        let mut p = mk(Decision::Allow, &[], &["raw.githubusercontent.com"]);
        p.proxy = vec![".githubusercontent.com".to_string()];

        assert!(!p.trusts_proxy_fakeip_host("raw.githubusercontent.com"));
        assert!(p.trusts_proxy_fakeip_host("avatars.githubusercontent.com"));
    }

    #[test]
    fn trusted_fakeip_cidr_allows_placeholder_but_not_real_private() {
        let decider = NetworkPolicyDecider::new(NetworkPolicy::default(), None)
            .with_trusted_fakeip_cidrs(&[
                "198.18.0.0/15",
                "127.0.0.0/8",
                "10.0.0.0/8",
                "169.254.0.0/16",
            ]);

        // fake-ip placeholder range (clash default / IETF benchmark) is trusted
        assert!(decider.is_trusted_fakeip_addr(&"198.18.0.5".parse::<std::net::IpAddr>().unwrap()));
        assert!(
            decider.is_trusted_fakeip_addr(&"198.19.255.255".parse::<std::net::IpAddr>().unwrap())
        );

        // real private / loopback / link-local / cloud-metadata are NOT trusted
        for ip in ["192.168.1.1", "10.0.0.1", "127.0.0.1", "169.254.169.254"] {
            assert!(
                !decider.is_trusted_fakeip_addr(&ip.parse::<std::net::IpAddr>().unwrap()),
                "{ip} must not be treated as a fake-ip placeholder"
            );
        }

        // no ranges configured → nothing trusted
        let bare = NetworkPolicyDecider::new(NetworkPolicy::default(), None);
        assert!(!bare.is_trusted_fakeip_addr(&"198.18.0.5".parse::<std::net::IpAddr>().unwrap()));
    }

    #[test]
    fn configured_fakeip_cidrs_are_loaded_but_unsafe_ranges_are_ignored() {
        let policy = NetworkPolicy {
            proxy_fake_ip_cidrs: vec![
                "198.18.0.0/15".to_string(),
                "127.0.0.0/8".to_string(),
                "10.0.0.0/8".to_string(),
            ],
            ..NetworkPolicy::default()
        };
        let decider = NetworkPolicyDecider::new(policy, None);

        assert!(decider.is_trusted_fakeip_addr(&"198.19.0.5".parse().unwrap()));
        assert!(!decider.is_trusted_fakeip_addr(&"127.0.0.1".parse().unwrap()));
        assert!(!decider.is_trusted_fakeip_addr(&"10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn host_from_url_extracts_host() {
        assert_eq!(
            host_from_url("https://api.deepseek.com/health"),
            Some("api.deepseek.com".to_string())
        );
        assert_eq!(
            host_from_url("http://Example.COM:8080/x"),
            Some("example.com".to_string())
        );
        assert_eq!(host_from_url("not a url"), None);
    }

    #[test]
    fn auditor_writes_one_line_per_call() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        let auditor = NetworkAuditor::new(path.clone(), true);
        auditor.record("api.example.com", "fetch_url", "Allow");
        auditor.record("evil.example.com", "fetch_url", "Deny");
        let body = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            // <ts> network <host> <tool> <decision>
            let parts: Vec<&str> = line.split_whitespace().collect();
            assert!(parts.len() >= 5, "line shape: {line}");
            assert_eq!(parts[1], "network");
        }
        assert!(lines[0].contains("api.example.com"));
        assert!(lines[0].ends_with("Allow"));
        assert!(lines[1].contains("evil.example.com"));
        assert!(lines[1].ends_with("Deny"));
    }

    #[test]
    fn auditor_disabled_writes_nothing() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        let auditor = NetworkAuditor::new(path.clone(), false);
        auditor.record("api.example.com", "fetch_url", "Allow");
        assert!(!path.exists() || std::fs::read_to_string(&path).unwrap().is_empty());
    }

    #[test]
    fn session_cache_short_circuits_evaluate() {
        let policy = mk(Decision::Prompt, &[], &[]);
        let decider = NetworkPolicyDecider::new(policy, None);
        // First call returns Prompt.
        assert_eq!(
            decider.evaluate("api.example.com", "fetch_url"),
            Decision::Prompt
        );
        decider.approve_session("api.example.com", "fetch_url");
        // After approve_session, the same host returns Allow without prompting.
        assert_eq!(
            decider.evaluate("api.example.com", "fetch_url"),
            Decision::Allow
        );
    }

    #[test]
    fn refreshed_policy_adopts_new_table_without_retracting_session_approvals() {
        let decider = NetworkPolicyDecider::new(mk(Decision::Prompt, &[], &[]), None);
        decider.approve_session("approved.example.com", "fetch_url");

        // Mid-session `/network allow api.github.com` reaches the session as a
        // re-read table rather than a restart.
        let refreshed =
            decider.with_policy_refreshed(mk(Decision::Prompt, &["api.github.com"], &[]));

        assert_eq!(
            refreshed.evaluate("api.github.com", "Bash"),
            Decision::Allow,
            "the re-read table is in force"
        );
        assert_eq!(
            refreshed.evaluate("approved.example.com", "fetch_url"),
            Decision::Allow,
            "an approval already granted this session survives the refresh"
        );
        assert_eq!(
            refreshed.evaluate("unlisted.example.com", "Bash"),
            Decision::Prompt,
            "hosts the table does not name keep the default"
        );
    }

    #[test]
    fn refreshed_policy_honours_a_new_deny_list() {
        let decider = NetworkPolicyDecider::new(mk(Decision::Allow, &[], &[]), None);
        assert_eq!(
            decider.evaluate("evil.example.com", "fetch_url"),
            Decision::Allow
        );

        let refreshed =
            decider.with_policy_refreshed(mk(Decision::Allow, &[], &["evil.example.com"]));

        assert_eq!(
            refreshed.evaluate("evil.example.com", "fetch_url"),
            Decision::Deny,
            "a deny entry written after spawn is enforced without a restart"
        );
    }

    #[test]
    fn refreshed_policy_keeps_the_authority_marking() {
        let decider =
            NetworkPolicyDecider::new(mk(Decision::Deny, &[], &[]), None).with_authoritative();
        assert!(decider.is_authoritative());
        let refreshed = decider.with_policy_refreshed(mk(Decision::Allow, &[], &[]));
        assert!(
            refreshed.is_authoritative(),
            "a refreshed authority is still an authority"
        );
    }

    #[test]
    fn folding_a_document_never_widens_an_authority() {
        // Assert what the folded policy *decides*, not the shape of its fields:
        // `decide` checks `allow` before falling back to `default`, so a field
        // assertion can look safe while the verdict is `Allow`.

        // What a Fleet denial or a managed overlay resolved to: deny by default.
        let authority = NetworkPolicy {
            default: DecisionToml::Deny,
            deny: vec!["blocked.example.com".to_string()],
            ..NetworkPolicy::default()
        };
        // What the user's document asks for on a mid-session re-read.
        let document = NetworkPolicy {
            default: DecisionToml::Allow,
            allow: vec!["api.github.com".to_string()],
            proxy: vec!["proxy.example.com".to_string()],
            proxy_fake_ip_cidrs: vec!["198.18.0.0/15".to_string()],
            audit: false,
            ..NetworkPolicy::default()
        };

        let folded = authority.folded_with_lower_layer(document);

        assert_eq!(
            folded.decide("api.github.com"),
            Decision::Deny,
            "a document cannot lift a deny-by-default authority's denial"
        );
        assert_eq!(
            folded.decide("blocked.example.com"),
            Decision::Deny,
            "a denial the authority set survives the fold"
        );
        assert_eq!(
            folded.decide("unlisted.example.com"),
            Decision::Deny,
            "an unlisted host still falls back to the authority's denial"
        );
        assert!(
            folded.proxy.is_empty() && folded.proxy_fake_ip_cidrs.is_empty(),
            "a document cannot hand itself a fake-IP SSRF exception under a denial"
        );
        assert!(
            folded.audit,
            "a document cannot silence the audit log under an authority"
        );

        // A prompt-by-default authority is the case `/network allow` exists for:
        // the document may name hosts, and it may not erase the authority's own.
        let prompt_authority = NetworkPolicy {
            default: DecisionToml::Prompt,
            allow: vec!["internal.example.com".to_string()],
            deny: vec!["blocked.example.com".to_string()],
            ..NetworkPolicy::default()
        };
        let folded = prompt_authority.folded_with_lower_layer(NetworkPolicy {
            default: DecisionToml::Allow,
            allow: vec!["api.github.com".to_string()],
            ..NetworkPolicy::default()
        });
        assert_eq!(
            folded.decide("api.github.com"),
            Decision::Allow,
            "the document may still name hosts when the authority prompts"
        );
        assert_eq!(
            folded.decide("internal.example.com"),
            Decision::Allow,
            "the authority's own allowance survives the fold"
        );
        assert_eq!(
            folded.decide("unlisted.example.com"),
            Decision::Prompt,
            "the document cannot widen the authority's fallback to allow"
        );
        assert_eq!(
            folded.decide("blocked.example.com"),
            Decision::Deny,
            "either layer's denial wins"
        );

        // The plain user path replaces the policy outright, so
        // `/network default allow` still lands there.
        let plain = NetworkPolicy {
            default: DecisionToml::Prompt,
            ..NetworkPolicy::default()
        };
        assert_eq!(
            plain
                .folded_with_lower_layer(NetworkPolicy {
                    default: DecisionToml::Allow,
                    ..NetworkPolicy::default()
                })
                .default,
            DecisionToml::Prompt
        );
    }

    /// A managed overlay may legitimately resolve `default = "allow"`. Its fold
    /// takes the union branch, and the document's own fallback can only narrow
    /// it — nothing there is more permissive than the authority's own policy,
    /// because that policy already allowed everything.
    #[test]
    fn folding_an_allow_by_default_authority_only_narrows() {
        let authority = NetworkPolicy {
            default: DecisionToml::Allow,
            ..NetworkPolicy::default()
        };
        let folded = authority.folded_with_lower_layer(NetworkPolicy {
            default: DecisionToml::Deny,
            allow: vec!["named.example.com".to_string()],
            ..NetworkPolicy::default()
        });

        assert_eq!(
            folded.decide("named.example.com"),
            Decision::Allow,
            "the authority allowed everything already; naming a host is not a widening"
        );
        assert_eq!(
            folded.decide("other.example.com"),
            Decision::Deny,
            "the document narrowed the fallback, which is the direction it may move"
        );
    }

    #[test]
    fn approve_persistent_writes_back_to_policy() {
        let policy = mk(Decision::Prompt, &[], &[]);
        let mut decider = NetworkPolicyDecider::new(policy, None);
        decider.approve_persistent("api.example.com", "fetch_url");
        assert!(
            decider
                .policy()
                .allow
                .iter()
                .any(|h| h == "api.example.com")
        );
        // And the session cache also got updated, so fresh evaluate returns Allow.
        assert_eq!(
            decider.evaluate("api.example.com", "fetch_url"),
            Decision::Allow
        );
    }

    #[test]
    fn deny_session_blocks_subsequent_evaluate() {
        let policy = mk(Decision::Allow, &[], &[]);
        let decider = NetworkPolicyDecider::new(policy, None);
        decider.deny_session("evil.example.com", "fetch_url");
        assert_eq!(
            decider.evaluate("evil.example.com", "fetch_url"),
            Decision::Deny
        );
    }

    #[test]
    fn audit_records_terminal_decisions_through_decider() {
        let dir = tempdir().expect("tempdir");
        let auditor = NetworkAuditor::new(dir.path().join("audit.log"), true);
        let policy = mk(Decision::Deny, &["api.deepseek.com"], &[]);
        let decider = NetworkPolicyDecider::new(policy, Some(auditor));

        let allow = decider.evaluate("api.deepseek.com", "fetch_url");
        let deny = decider.evaluate("evil.example.com", "fetch_url");
        assert_eq!(allow, Decision::Allow);
        assert_eq!(deny, Decision::Deny);

        let body = std::fs::read_to_string(dir.path().join("audit.log")).expect("read");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("Allow"));
        assert!(lines[1].ends_with("Deny"));
    }

    #[test]
    fn decision_parse_unknown_falls_back_to_prompt() {
        assert_eq!(Decision::parse("allow"), Decision::Allow);
        assert_eq!(Decision::parse("Deny"), Decision::Deny);
        assert_eq!(Decision::parse("BLOCK"), Decision::Deny);
        assert_eq!(Decision::parse("prompt"), Decision::Prompt);
        assert_eq!(Decision::parse("garbage"), Decision::Prompt);
    }

    #[test]
    fn network_denied_carries_host() {
        let err = NetworkDenied("api.example.com".to_string());
        assert_eq!(err.host(), "api.example.com");
        assert!(format!("{err}").contains("api.example.com"));
    }
}
