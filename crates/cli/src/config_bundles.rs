//! Portable config bundles: `codewhale config import` / `config export --portable`.
//!
//! A bundle is a TOML or JSON document carrying a portable subset of a
//! CodeWhale configuration (preferences, harness profiles, provider
//! non-secret settings, project/global sections) between machines. The
//! envelope is versioned and strict (`deny_unknown_fields`), secrets are
//! rejected by key name and value shape (never echoed), parsing is bounded,
//! and application is transactional with a timestamped backup and rollback.
//!
//! Security contract:
//! - No secret ever round-trips: fields whose key matches
//!   [`codewhale_config::is_sensitive_config_key`] are rejected on import and
//!   dropped on export, and bare credential-shaped values are rejected by
//!   value shape. Rejection messages name the field, never the value.
//! - Input size is capped (5 MiB, matching the skill installer's cap).
//! - HTTPS only for remote fetch, except plain `http` on loopback; redirects
//!   are followed at most a bounded number of times within the same scheme.
//! - Bundle-declared file paths must resolve inside the target config
//!   directory; traversal and symlink escapes are refused.
//! - Project scope never mutates the user-global document and vice versa.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use codewhale_config::{ConfigToml, is_sensitive_config_key};

/// Maximum accepted bundle size, both for reads and remote fetches.
/// Matches the skill installer's 5 MiB cap.
pub const MAX_BUNDLE_BYTES: u64 = 5 * 1024 * 1024;

/// Envelope `kind` value required by every bundle.
pub const BUNDLE_KIND: &str = "codewhale.portable-config";

/// Envelope `schema_version` accepted by this build.
pub const BUNDLE_SCHEMA_VERSION: u64 = 1;

/// Maximum number of HTTP redirects followed during a remote fetch.
const MAX_REDIRECTS: usize = 5;

/// Timeout for the remote fetch, in seconds.
const FETCH_TIMEOUT_SECS: u64 = 30;

/// Credential-shaped value prefixes rejected even under a benign key name.
/// Conservative on purpose: only well-known provider token shapes.
const SECRET_VALUE_PREFIXES: [&str; 6] = ["sk-", "Bearer ", "ghp_", "xoxb-", "AKIA", "eyJ"];

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// Strict portable-bundle envelope. Unknown fields fail the parse: a bundle
/// written by a newer schema must not be silently half-applied.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBundle {
    pub schema_version: u64,
    pub kind: String,
    #[serde(default)]
    pub metadata: BundleMetadata,
    #[serde(default)]
    pub preferences: BundleTable,
    #[serde(default)]
    pub profiles: BundleTable,
    #[serde(default)]
    pub plugins: BundleTable,
    #[serde(default)]
    pub project: BundleTable,
    #[serde(default)]
    pub global: BundleTable,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleMetadata {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub generator: Option<String>,
}

/// One bundle section: a flat table of config keys to values. Keys inside a
/// section are data, not schema, so unknown keys parse here — credential
/// rejection happens at plan time by name and value shape.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BundleTable {
    #[serde(flatten)]
    pub entries: std::collections::BTreeMap<String, toml::Value>,
}

// ---------------------------------------------------------------------------
// Parsing (bounded)
// ---------------------------------------------------------------------------

/// Parse a bundle from raw bytes, rejecting oversize input before parse.
pub fn parse_bundle_bytes(raw: &[u8], source: &str) -> Result<PortableBundle> {
    if raw.len() as u64 > MAX_BUNDLE_BYTES {
        bail!(
            "bundle at {source} is {} bytes; the limit is {MAX_BUNDLE_BYTES} bytes",
            raw.len()
        );
    }
    let text = std::str::from_utf8(raw)
        .with_context(|| format!("bundle at {source} is not valid UTF-8"))?;
    parse_bundle_str(text, source)
}

/// Parse a bundle document: TOML by default, JSON when the source ends in
/// `.json` or the document starts with `{`.
pub fn parse_bundle_str(text: &str, source: &str) -> Result<PortableBundle> {
    let trimmed = text.trim_start();
    let bundle = if trimmed.starts_with('{') || source.ends_with(".json") {
        // serde_json keeps the last of two identical object keys. A bundle is
        // a reviewed plan, so a repeated key must fail before anything is
        // planned or written, exactly as TOML already refuses duplicates.
        reject_duplicate_json_keys(text)
            .with_context(|| format!("bundle at {source} is not valid JSON"))?;
        serde_json::from_str::<PortableBundle>(text)
            .with_context(|| format!("bundle at {source} is not valid JSON"))?
    } else {
        toml::from_str::<PortableBundle>(text)
            .with_context(|| format!("bundle at {source} is not valid TOML"))?
    };
    validate_bundle(&bundle, source)?;
    Ok(bundle)
}

/// Fail closed on a JSON document that repeats an object key at any depth.
///
/// The document is walked with a deserializer seed that never materializes
/// values, so the check costs one pass and reports the first offending key
/// path without echoing any value.
fn reject_duplicate_json_keys(text: &str) -> Result<()> {
    use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
    use std::fmt;

    struct NoDuplicates<'a> {
        path: &'a mut Vec<String>,
    }

    impl<'de> DeserializeSeed<'de> for NoDuplicates<'_> {
        type Value = ();

        fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }
    }

    impl<'de> Visitor<'de> for NoDuplicates<'_> {
        type Value = ();

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a JSON value without duplicate object keys")
        }

        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
            Ok(())
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<(), E> {
            Ok(())
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let mut index = 0usize;
            loop {
                self.path.push(format!("[{index}]"));
                let next = seq.next_element_seed(NoDuplicates { path: self.path });
                self.path.pop();
                if next?.is_none() {
                    return Ok(());
                }
                index += 1;
            }
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut seen = std::collections::BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !seen.insert(key.clone()) {
                    let mut path = self.path.clone();
                    path.push(key);
                    return Err(A::Error::custom(format!(
                        "duplicate key {:?}",
                        path.join(".")
                    )));
                }
                self.path.push(key);
                let nested = map.next_value_seed(NoDuplicates { path: self.path });
                self.path.pop();
                nested?;
            }
            Ok(())
        }
    }

    let mut deserializer = serde_json::Deserializer::from_str(text);
    let mut path = Vec::new();
    NoDuplicates { path: &mut path }
        .deserialize(&mut deserializer)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    deserializer
        .end()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(())
}

fn validate_bundle(bundle: &PortableBundle, source: &str) -> Result<()> {
    if bundle.kind != BUNDLE_KIND {
        bail!(
            "bundle at {source} has kind {:?}; expected {BUNDLE_KIND:?}",
            bundle.kind
        );
    }
    if bundle.schema_version != BUNDLE_SCHEMA_VERSION {
        bail!(
            "bundle at {source} has schema_version {}; this build understands {BUNDLE_SCHEMA_VERSION}",
            bundle.schema_version
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Secret rejection
// ---------------------------------------------------------------------------

/// One rejected entry: the dotted key path and the reason. Values are never
/// included — the reason and path are all a reviewer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedEntry {
    pub key: String,
    pub reason: String,
}

/// Scan every section of the bundle for non-portable entries. Import and
/// export use the same path predicate, so machine-local route/execution/trust
/// authority cannot be stripped in one direction but accepted in the other.
/// String leaves are additionally rejected by credential shape.
pub fn find_rejected_entries(bundle: &PortableBundle) -> Vec<RejectedEntry> {
    let mut rejected = Vec::new();
    for (section, table) in [
        ("preferences", &bundle.preferences),
        ("profiles", &bundle.profiles),
        ("plugins", &bundle.plugins),
        ("project", &bundle.project),
        ("global", &bundle.global),
    ] {
        for (key, value) in &table.entries {
            let dotted = format!("{section}.{key}");
            if let Some(reason) = nonportable_path_reason(key) {
                rejected.push(RejectedEntry {
                    key: dotted,
                    reason: reason.to_string(),
                });
                continue;
            }
            if let Some(reason) = value_rejection_reason(key, value) {
                rejected.push(RejectedEntry {
                    key: dotted,
                    reason,
                });
            }
        }
    }
    rejected
}

/// Maximum nesting depth the export walkers descend. Config files are
/// shallow; anything deeper is pathological and fails closed.
const MAX_EXPORT_WALK_DEPTH: usize = 64;

/// Why a value carries nested non-portable authority or looks like a bare
/// credential, or `None` when it is safe to move between machines.
fn value_rejection_reason(path: &str, value: &toml::Value) -> Option<String> {
    value_rejection_reason_at(path, value, 0)
}

fn value_rejection_reason_at(path: &str, value: &toml::Value, depth: usize) -> Option<String> {
    if depth > MAX_EXPORT_WALK_DEPTH {
        return Some(format!(
            "nested more than {MAX_EXPORT_WALK_DEPTH} levels deep"
        ));
    }
    if let Some(reason) = nonportable_value_reason(path, value) {
        return Some(reason.to_string());
    }
    match value {
        toml::Value::String(text) => string_secret_reason(text),
        toml::Value::Array(items) => items
            .iter()
            .find_map(|value| value_rejection_reason_at(path, value, depth + 1))
            .map(|reason| format!("array contains an entry where {reason}")),
        toml::Value::Table(map) => {
            for (key, nested_value) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if let Some(reason) = nonportable_path_reason(&child_path) {
                    return Some(format!("nested key {key:?} {reason}"));
                }
                if let Some(reason) =
                    value_rejection_reason_at(&child_path, nested_value, depth + 1)
                {
                    return Some(format!("nested under {key:?}, {reason}"));
                }
            }
            None
        }
        _ => None,
    }
}

fn string_secret_reason(text: &str) -> Option<String> {
    if let Some(prefix) = SECRET_VALUE_PREFIXES
        .iter()
        .find(|prefix| text.trim().starts_with(*prefix))
    {
        return Some(format!(
            "value has the shape of a credential (prefix {prefix:?} redacted)"
        ));
    }
    if text.contains(codewhale_config::persistence::REDACTED) {
        // A placeholder is the residue of redaction, never a real setting;
        // exporting it would carry nothing and importing it would write the
        // placeholder into the live document.
        return Some("value contains a redaction placeholder".to_string());
    }
    if codewhale_config::persistence::redact_secrets(text) != text {
        return Some("value contains credential-shaped text".to_string());
    }
    if url_carries_credential(text.trim()) {
        return Some("value is a URL that carries a credential".to_string());
    }
    None
}

/// Webhook endpoints whose path is itself the bearer credential.
const CREDENTIAL_PATH_WEBHOOKS: [(&str, &str); 4] = [
    ("hooks.slack.com", "/services/"),
    ("discord.com", "/api/webhooks/"),
    ("discordapp.com", "/api/webhooks/"),
    ("webhook.office.com", "/webhookb2/"),
];

/// True for a URL with userinfo, a credential-named query parameter, or a
/// known webhook host whose path is the secret.
fn url_carries_credential(text: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(text) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return true;
    }
    // The shared display redactor masks credential-named query values with
    // `***`; count masks rather than compare bytes, because it re-encodes the
    // query and would otherwise flag benign URLs.
    let masks = |value: &str| value.matches("***").count();
    if masks(&codewhale_secrets::sanitize::redact_url_for_display(text)) > masks(text) {
        return true;
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    CREDENTIAL_PATH_WEBHOOKS.iter().any(|(webhook_host, path)| {
        (host == *webhook_host || host.ends_with(&format!(".{webhook_host}")))
            && url.path().starts_with(path)
    })
}

fn is_sensitive_bundle_key(key: &str) -> bool {
    if is_sensitive_config_key(key) || is_credential_authority_key(key) {
        return true;
    }
    // Normalize the complete dotted path, not only its final component. A
    // quoted TOML key such as `"api.key"` reaches us without its quotes and
    // is otherwise indistinguishable from two structural components. Either
    // representation names credential material and must fail closed.
    let normalized = normalize_bundle_key(key);

    matches!(
        normalized.as_str(),
        "access_key"
            | "access_token"
            | "api_key"
            | "api_keys"
            | "apikey"
            | "authorization"
            | "bearer"
            | "client_secret"
            | "cookie"
            | "credential"
            | "credentials"
            | "id_token"
            | "password"
            | "passwords"
            | "passwd"
            | "private_key"
            | "proxy_authorization"
            | "refresh_token"
            | "secret"
            | "secrets"
            | "set_cookie"
            | "token"
            | "tokens"
    ) || normalized.ends_with("_access_key")
        || normalized.ends_with("_api_key")
        || normalized.ends_with("_authorization")
        || normalized.ends_with("_cookie")
        || normalized.ends_with("_password")
        || normalized.ends_with("_private_key")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_token")
}

fn normalize_bundle_key(key: &str) -> String {
    let segment = key.trim().trim_matches('"');
    let chars: Vec<char> = segment.chars().collect();
    let mut normalized = String::with_capacity(segment.len());
    for (index, character) in chars.iter().copied().enumerate() {
        if !character.is_ascii_alphanumeric() {
            if !normalized.ends_with('_') {
                normalized.push('_');
            }
            continue;
        }
        if character.is_ascii_uppercase() {
            let previous = index.checked_sub(1).and_then(|index| chars.get(index));
            let next = chars.get(index + 1);
            let starts_word = previous.is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            }) || (previous
                .is_some_and(|character| character.is_ascii_uppercase())
                && next.is_some_and(|character| character.is_ascii_lowercase()));
            if starts_word && !normalized.ends_with('_') {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
        } else {
            normalized.push(character.to_ascii_lowercase());
        }
    }
    normalized.trim_matches('_').to_string()
}

fn is_credential_authority_key(key: &str) -> bool {
    let normalized = normalize_bundle_key(key);
    if normalized == "external_credentials"
        || normalized.ends_with("_external_credentials")
        || normalized == "oauth_credential_generation"
        || normalized.ends_with("_oauth_credential_generation")
    {
        return true;
    }
    // `auth_mode` is a declarative protocol selection; an `auth` table is
    // executable or secret-store authority and is intentionally non-portable.
    key.split('.')
        .map(normalize_bundle_key)
        .any(|segment| segment == "auth")
}

fn is_machine_bound_top_level_key(key: &str) -> bool {
    key.split('.')
        .next()
        .map(normalize_bundle_key)
        .is_some_and(|root| {
            matches!(
                root.as_str(),
                // Trust posture: a bundle must not widen what the agent may
                // run or approve on the receiving machine.
                "allow_shell"
                    | "approval_policy"
                    | "sandbox_mode"
                    | "sandbox_network_access"
                    | "yolo"
                    // Machine-local read-denylist paths.
                    | "sandbox_denied_read_paths"
                    | "sandbox_read_denylist_defaults"
                    | "sandbox_read_denylist_exempt"
                    // Local executables, outbound event sinks, and control
                    // endpoints.
                    | "control_socket"
                    | "extension_host"
                    | "lifecycle_outbox"
                    | "auto_review"
                    | "hooks"
                    | "instructions"
                    | "managed_config_path"
                    | "project_instruction_imports"
                    | "projects"
                    | "requirements_path"
                    | "route_preferences_version"
                    | "route_preferences_migration"
                    | "runtime_api"
                    | "workspace"
            )
        })
}

fn is_nonportable_lsp_authority_key(key: &str) -> bool {
    let mut segments = key.split('.').map(normalize_bundle_key);
    matches!(segments.next().as_deref(), Some("lsp"))
        && matches!(segments.next().as_deref(), Some("custom" | "servers"))
}

fn is_nonportable_nested_authority_key(key: &str) -> bool {
    let segments = key.split('.').map(normalize_bundle_key).collect::<Vec<_>>();
    match segments.as_slice() {
        [root, field, ..]
            if root == "tools" && matches!(field.as_str(), "overrides" | "plugin_dir") =>
        {
            true
        }
        [root, field, ..] if root == "update" && field == "update_uri" => true,
        [root, field, ..] if root == "notifications" && field == "sound_file" => true,
        [root, field, ..] if root == "speech" && field == "output_dir" => true,
        [root, .., field] if root == "providers" && field == "api_key_env" => true,
        _ => false,
    }
}

fn is_machine_specific_config_path(path: &str) -> bool {
    let path = normalize_bundle_key(path);
    MACHINE_SPECIFIC_KEYS.iter().any(|key| {
        let key = normalize_bundle_key(key);
        path == key
            || path
                .strip_suffix(&key)
                .is_some_and(|prefix| prefix.ends_with('_'))
    })
}

fn nonportable_path_reason(path: &str) -> Option<&'static str> {
    if is_machine_bound_top_level_key(path) {
        return Some("carries machine-bound execution or trust authority");
    }
    if is_nonportable_lsp_authority_key(path) {
        return Some("carries executable LSP authority");
    }
    if is_nonportable_nested_authority_key(path) {
        return Some("carries machine-local route or execution authority");
    }
    if is_machine_specific_config_path(path) {
        return Some("carries machine-local route or filesystem authority");
    }
    if is_credential_authority_key(path) {
        return Some("carries machine-local credential authority");
    }
    if is_sensitive_bundle_key(path) {
        return Some("names credential material");
    }
    None
}

/// Telemetry opt-out is safe to move between machines, but opt-in is durable
/// user consent coupled to SetupState. A portable bundle may tighten that
/// consent (`false`); it must never manufacture or transfer `true`.
fn nonportable_value_reason(path: &str, value: &toml::Value) -> Option<&'static str> {
    let top_level_telemetry = !path.contains('.') && normalize_bundle_key(path) == "telemetry";
    (top_level_telemetry && matches!(value, toml::Value::Boolean(true)))
        .then_some("would port telemetry opt-in consent between machines")
}

// ---------------------------------------------------------------------------
// Import plan
// ---------------------------------------------------------------------------

/// What applying the bundle would do, computed before anything is written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub skipped: Vec<String>,
    pub conflicting: Vec<String>,
    pub rejected: Vec<RejectedEntry>,
}

impl ImportPlan {
    #[must_use]
    pub fn is_no_op(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty()
    }
}

/// Compute the deterministic import plan for `bundle` against `config`.
///
/// `section` selects the target document mapping: bundle `project` entries
/// apply only to a project-scope document, `global` entries only to a
/// user-global one; `preferences`, `profiles`, and `plugins` apply to both.
/// Entries that would not touch the target document are `skipped`, so the
/// same bundle imports cleanly at either scope.
pub fn plan_import(bundle: &PortableBundle, config: &ConfigToml, scope: BundleScope) -> ImportPlan {
    let mut plan = ImportPlan {
        rejected: find_rejected_entries(bundle),
        ..ImportPlan::default()
    };
    let rejected_keys: std::collections::BTreeSet<&str> = plan
        .rejected
        .iter()
        .map(|entry| entry.key.as_str())
        .collect();
    // Sections are presentation and scope labels over one flat ConfigToml
    // keyspace. Two applicable sections naming the same key would otherwise
    // make apply order decide which value wins. Detect that ambiguity before
    // classifying or writing any entry.
    let mut applicable_key_counts = std::collections::BTreeMap::<&str, usize>::new();
    for (section, table) in [
        ("preferences", &bundle.preferences),
        ("profiles", &bundle.profiles),
        ("plugins", &bundle.plugins),
        ("project", &bundle.project),
        ("global", &bundle.global),
    ] {
        if section_applies(section, scope) {
            for key in table.entries.keys() {
                *applicable_key_counts.entry(key.as_str()).or_default() += 1;
            }
        }
    }
    let colliding_keys: std::collections::BTreeSet<&str> = applicable_key_counts
        .into_iter()
        .filter_map(|(key, count)| (count > 1).then_some(key))
        .collect();

    for (section, table) in [
        ("preferences", &bundle.preferences),
        ("profiles", &bundle.profiles),
        ("plugins", &bundle.plugins),
        ("project", &bundle.project),
        ("global", &bundle.global),
    ] {
        let dotted = |key: &str| format!("{section}.{key}");
        let applies = section_applies(section, scope);
        for (key, value) in &table.entries {
            let dotted = dotted(key);
            if rejected_keys.contains(dotted.as_str())
                || (applies && colliding_keys.contains(key.as_str()))
            {
                plan.conflicting.push(dotted);
                continue;
            }
            if !applies {
                plan.skipped.push(dotted);
                continue;
            }
            if config_value_matches(config, key, value) {
                plan.skipped.push(dotted);
            } else if config_has_value(config, key) {
                plan.changed.push(dotted);
            } else {
                plan.added.push(dotted);
            }
        }
    }
    plan
}

fn config_value_matches(config: &ConfigToml, key: &str, value: &toml::Value) -> bool {
    let semantically_equal = (|| {
        let current = config_document(config).ok()?;
        let mut candidate = config.clone();
        apply_config_value(&mut candidate, key, value).ok()?;
        Some(config_document(&candidate).ok()? == current)
    })()
    .unwrap_or(false);
    semantically_equal
        || config.get_value(key).is_some_and(|current| {
            render_toml_value(value).ok().as_deref() == Some(current.as_str())
        })
}

fn config_has_value(config: &ConfigToml, key: &str) -> bool {
    config_document(config)
        .ok()
        .is_some_and(|table| table.contains_key(key))
        || config.get_value(key).is_some()
}

fn section_applies(section: &str, scope: BundleScope) -> bool {
    match section {
        "project" => scope == BundleScope::Project,
        "global" => scope == BundleScope::Global,
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Which document an import/export targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleScope {
    /// The user-global config (`~/.codewhale/config.toml` by default).
    Global,
    /// The workspace-scoped config (`<repo>/.codewhale/config.toml`).
    Project,
}

impl BundleScope {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

fn validate_scope_target(scope: BundleScope, target: &Path) -> Result<()> {
    let workspace_scoped = codewhale_config::config_path_is_workspace_scoped(target);
    match (scope, workspace_scoped) {
        (BundleScope::Project, false) => bail!(
            "--project requires a workspace config ({} is the user-global document)",
            target.display()
        ),
        (BundleScope::Global, true) => bail!(
            "global bundle operations cannot target workspace config {}; rerun with --project or select the user-global config",
            target.display()
        ),
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

/// Resolve `candidate` inside `base_dir`, refusing traversal and symlink
/// escapes. Returns the resolved path or an error naming the refusal — the
/// candidate string itself is safe to echo (it is config data, not a secret).
/// Resolve `candidate` inside `base_dir`, refusing traversal and symlink
/// escapes. Returns the joined path or an error naming the refusal.
/// Reserved for path-carrying bundle sections (none shipped yet); exercised
/// by the traversal tests so the contract cannot silently rot.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "path-carrying sections land with the next schema")
)]
pub fn resolve_bounded_path(base_dir: &Path, candidate: &str) -> Result<PathBuf> {
    if candidate.contains('\0') {
        bail!("bundle path contains a NUL byte; refused");
    }
    let candidate_path = Path::new(candidate);
    if candidate_path.is_absolute() {
        bail!(
            "bundle path {candidate:?} is absolute; only paths inside the config directory are accepted"
        );
    }
    let canonical_base = base_dir
        .canonicalize()
        .with_context(|| format!("config directory {} is unavailable", base_dir.display()))?;
    let joined = base_dir.join(candidate_path);
    // Walk the joined path's ancestors from the deepest existing component up:
    // every existing component must canonicalize inside the base, so a symlink
    // pointing outside the config directory is refused even when the final
    // target does not exist yet.
    let deepest_existing = joined
        .ancestors()
        .find(|ancestor| ancestor.symlink_metadata().is_ok())
        .context("bundle path has no existing ancestor inside the config directory")?;
    let resolved = deepest_existing.canonicalize().with_context(|| {
        format!(
            "could not resolve bundle path component {}",
            deepest_existing.display()
        )
    })?;
    if !resolved.starts_with(&canonical_base) {
        bail!("bundle path {candidate:?} escapes the config directory via a symlink; refused");
    }
    Ok(joined)
}

// ---------------------------------------------------------------------------
// Remote fetch
// ---------------------------------------------------------------------------

/// Fetch a bundle over HTTPS (or plain http on loopback only) with a hard
/// size cap, a timeout, and bounded redirects. Mirrors the skill installer's
/// fetch bounds.
pub fn fetch_bundle(url: &str) -> Result<Vec<u8>> {
    let mut current_url = reqwest::Url::parse(url).map_err(|_| anyhow!("invalid bundle URL"))?;
    validate_bundle_url(&current_url)?;
    let initial_scheme = current_url.scheme().to_string();

    let client = codewhale_release::platform_blocking_http_client_builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        // Redirect targets must pass the same scheme/host policy as the
        // initial request, so redirects are followed explicitly below.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| anyhow!("building bundle fetch client failed"))?;
    let mut redirects = 0usize;
    let response = loop {
        let response = client
            .get(current_url.clone())
            .send()
            // reqwest errors can include the full URL (including its query or
            // userinfo), so keep transport failures deliberately URL-free.
            .map_err(|_| anyhow!("bundle fetch request failed"))?;

        if !response.status().is_redirection() {
            break response;
        }
        if redirects >= MAX_REDIRECTS {
            bail!("bundle fetch exceeded the five-redirect limit");
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or_else(|| anyhow!("bundle redirect is missing a valid Location header"))?
            .to_str()
            .map_err(|_| anyhow!("bundle redirect is missing a valid Location header"))?;
        let next_url = current_url
            .join(location)
            .map_err(|_| anyhow!("bundle redirect Location is invalid"))?;
        validate_bundle_redirect(&initial_scheme, &next_url)?;
        current_url = next_url;
        redirects += 1;
    };

    if !response.status().is_success() {
        bail!(
            "bundle fetch failed with HTTP status {}",
            response.status().as_u16()
        );
    }

    // Read at most MAX_BUNDLE_BYTES + 1 so an oversize body is detected
    // rather than silently truncated.
    let mut buffer = Vec::new();
    let body = response;
    body.take(MAX_BUNDLE_BYTES + 1)
        .read_to_end(&mut buffer)
        .map_err(|_| anyhow!("reading remote bundle failed"))?;
    if buffer.len() as u64 > MAX_BUNDLE_BYTES {
        bail!("remote bundle exceeds the {MAX_BUNDLE_BYTES} byte limit; refused");
    }
    Ok(buffer)
}

fn validate_bundle_url(url: &reqwest::Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported bundle URL scheme; use https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("bundle URLs may not include credentials");
    }
    let host = url.host_str().context("bundle URL must include a host")?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_bundle_host(host) => Ok(()),
        "http" => bail!("plain http is only allowed for loopback hosts; use https"),
        _ => unreachable!("scheme was validated above"),
    }
}

fn validate_bundle_redirect(initial_scheme: &str, next_url: &reqwest::Url) -> Result<()> {
    validate_bundle_url(next_url)?;
    if next_url.scheme() != initial_scheme {
        bail!("bundle redirects may not change URL scheme");
    }
    Ok(())
}

fn is_loopback_bundle_host(host: &str) -> bool {
    let normalized = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    normalized.eq_ignore_ascii_case("localhost")
        || normalized.to_ascii_lowercase().ends_with(".localhost")
        || normalized
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Build a deterministic, secret-free export from `config`.
///
/// Keys are sorted, machine-specific absolute paths and credential fields are
/// dropped, and the same section mapping as import is used so an exported
/// bundle re-imports at the same scope.
pub fn export_bundle(
    config: &ConfigToml,
    scope: BundleScope,
    metadata: BundleMetadata,
) -> Result<PortableBundle> {
    let mut preferences = BundleTable::default();
    let profiles = BundleTable::default();
    let mut global = BundleTable::default();
    let mut project = BundleTable::default();

    let mut document = config_document(config)?;
    if scope == BundleScope::Global {
        // Root model aliases are route-relative on import; reconcile them with
        // the canonical provider slots so the bundle never conflicts with
        // itself, without reparsing unrelated preserved extras as `Config`.
        codewhale_engine::route_preferences::scrub_root_model_aliases_for_export(&mut document)?;
    }
    for (key, value) in document {
        if let Some(value) = sanitize_export_value(&key, &value) {
            match export_section_for(&key, scope) {
                ExportSection::Preferences => {
                    preferences.entries.insert(key, value);
                }
                ExportSection::Global => {
                    global.entries.insert(key, value);
                }
                ExportSection::Project => {
                    project.entries.insert(key, value);
                }
                ExportSection::Drop => {}
            }
        }
    }

    let bundle = PortableBundle {
        schema_version: BUNDLE_SCHEMA_VERSION,
        kind: BUNDLE_KIND.to_string(),
        metadata,
        preferences,
        profiles,
        plugins: BundleTable::default(),
        project,
        global,
    };
    let rejected = find_rejected_entries(&bundle);
    if !rejected.is_empty() {
        bail!(
            "portable export refused credential-bearing config paths: {}",
            rejected
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(bundle)
}

/// Serialize a bundle deterministically (sorted keys, TOML).
pub fn serialize_bundle(bundle: &PortableBundle) -> Result<String> {
    toml::to_string_pretty(bundle).context("serializing portable bundle")
}

/// Config keys that name a machine-local location and must never be exported.
const MACHINE_SPECIFIC_KEYS: [&str; 14] = [
    "base_url",
    "bwrap_dev_roots",
    "bwrap_ro_roots",
    "hook_sinks.unix_socket_path",
    "mcp_config_path",
    "mcp_oauth_callback_port",
    "mcp_oauth_callback_url",
    "memory_path",
    "network.proxy",
    "notes_path",
    "sandbox_backend",
    "sandbox_url",
    "skills_dir",
    "telemetry_endpoint",
];

enum ExportSection {
    Preferences,
    Global,
    Project,
    Drop,
}

fn export_section_for(key: &str, scope: BundleScope) -> ExportSection {
    if key.starts_with("skills") || key.starts_with("tools") || key.starts_with("snapshots") {
        return ExportSection::Preferences;
    }
    if key.starts_with("auth.") {
        return ExportSection::Drop;
    }
    match scope {
        BundleScope::Global => ExportSection::Global,
        BundleScope::Project => ExportSection::Project,
    }
}

fn config_document(config: &ConfigToml) -> Result<toml::map::Map<String, toml::Value>> {
    // Serialize through TOML text before parsing to Value. Direct
    // `Value::try_from` double-encodes datetime values held inside flattened
    // `toml::Value` extras as the serializer's private marker table.
    let text = toml::to_string(config).context("serializing typed config for bundle")?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|_| anyhow!("serialized typed config was not valid TOML"))?;
    let toml::Value::Table(mut table) = value else {
        bail!("typed config did not serialize to a TOML table");
    };
    // `selected_provider_id` is runtime parse state and is skipped by serde;
    // restore the exact named-provider identity that ConfigStore writes.
    table.insert(
        "provider".to_string(),
        toml::Value::String(config.provider_id().to_string()),
    );
    Ok(table)
}

/// Return a recursively scrubbed export value. Secret-bearing leaves and
/// machine-local paths are omitted rather than replaced with a placeholder,
/// because a placeholder would become literal config on re-import.
fn sanitize_export_value(path: &str, value: &toml::Value) -> Option<toml::Value> {
    sanitize_export_value_at(path, value, 0)
}

fn sanitize_export_value_at(path: &str, value: &toml::Value, depth: usize) -> Option<toml::Value> {
    if depth > MAX_EXPORT_WALK_DEPTH {
        return None;
    }
    if nonportable_path_reason(path).is_some() || nonportable_value_reason(path, value).is_some() {
        return None;
    }
    match value {
        toml::Value::String(text) if string_secret_reason(text).is_some() => None,
        toml::Value::Array(values) => Some(toml::Value::Array(
            values
                .iter()
                .filter_map(|value| sanitize_export_value_at(path, value, depth + 1))
                .collect(),
        )),
        toml::Value::Table(table) => {
            let mut scrubbed = toml::map::Map::new();
            for (key, value) in table {
                let child_path = format!("{path}.{key}");
                if let Some(value) = sanitize_export_value_at(&child_path, value, depth + 1) {
                    scrubbed.insert(key.clone(), value);
                }
            }
            Some(toml::Value::Table(scrubbed))
        }
        _ => Some(value.clone()),
    }
}

// ---------------------------------------------------------------------------
// Transactional apply
// ---------------------------------------------------------------------------

/// Outcome of a committed import.
#[derive(Debug)]
pub struct ImportReceipt {
    pub plan: ImportPlan,
    pub backup_path: Option<PathBuf>,
    pub target: PathBuf,
}

/// Apply a validated bundle to `store` transactionally.
///
/// The current document is backed up to `<target>.bundle-backup-<timestamp>-<random>`,
/// the prepared candidate is committed through one `ConfigStore::save`, and any failure
/// restores the backup before returning the error. The receipt redacts by
/// construction: it carries only key paths and counts, never values.
#[cfg(test)]
pub fn apply_bundle(
    bundle: &PortableBundle,
    store: &mut codewhale_config::ConfigStore,
    scope: BundleScope,
    _workspace: &Path,
) -> Result<ImportReceipt> {
    let prepared = prepare_import(bundle, store, scope)?;
    apply_prepared_bundle(prepared, store, save_candidate)
}

struct PreparedImport {
    plan: ImportPlan,
    candidate: ConfigToml,
}

fn apply_prepared_bundle<F>(
    prepared: PreparedImport,
    store: &mut codewhale_config::ConfigStore,
    apply: F,
) -> Result<ImportReceipt>
where
    F: FnOnce(ConfigToml, &mut codewhale_config::ConfigStore, &mut bool) -> Result<()>,
{
    let PreparedImport { plan, candidate } = prepared;
    if !plan.conflicting.is_empty() {
        bail!(
            "bundle contains conflicting or rejected entries: {}; remove duplicate keys or credential-shaped entries and re-export",
            plan.conflicting.join(", ")
        );
    }
    if plan.is_no_op() {
        return Ok(ImportReceipt {
            plan,
            backup_path: None,
            target: store.path().to_path_buf(),
        });
    }

    let target = store.path().to_path_buf();
    let original_config = store.config.clone();
    let backup_path = if target
        .try_exists()
        .with_context(|| format!("checking config target {}", target.display()))?
    {
        Some(create_collision_safe_backup(&target)?)
    } else {
        None
    };

    let mut target_written = false;
    let apply_result = apply(candidate, store, &mut target_written);
    if let Err(error) = apply_result {
        store.config = original_config;
        let rollback = rollback_import_target(&target, backup_path.as_deref(), target_written)
            .and_then(|()| store.reload());
        match rollback {
            Ok(()) => bail!("{error:#}; rolled back to the pre-import document"),
            Err(_) => bail!(
                "{error:#}; ROLLBACK FAILED — the pre-import document is preserved at {}",
                backup_path
                    .as_deref()
                    .map(Path::display)
                    .map(|path| path.to_string())
                    .unwrap_or_else(
                        || "<no prior file; remove the new target manually>".to_string()
                    )
            ),
        }
    }

    Ok(ImportReceipt {
        plan,
        backup_path,
        target,
    })
}

fn rollback_import_target(
    target: &Path,
    backup_path: Option<&Path>,
    target_written: bool,
) -> Result<()> {
    // ConfigStore fails closed before replacing a stale target. If it did not
    // report a successful write, leave a concurrently-created or edited file
    // alone instead of mistaking somebody else's bytes for ours.
    if !target_written {
        return Ok(());
    }
    if let Some(backup_path) = backup_path {
        let bytes = std::fs::read(backup_path)
            .with_context(|| format!("reading pre-import backup {}", backup_path.display()))?;
        // Replace the name by rename (owner-only), never write through a link
        // that has appeared at it since the import wrote the file.
        codewhale_config::persistence::atomic_write(target, &bytes)
            .with_context(|| format!("restoring pre-import config {}", target.display()))?;
        return Ok(());
    }

    match std::fs::remove_file(target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("removing newly-created config {}", target.display())),
    }
}

/// Build the exact candidate used for both preview and commit. Legacy route
/// migration remains in memory until the existing ConfigStore CAS save.
fn prepare_import(
    bundle: &PortableBundle,
    store: &codewhale_config::ConfigStore,
    scope: BundleScope,
) -> Result<PreparedImport> {
    match scope {
        BundleScope::Global if !bundle.project.entries.is_empty() => bail!(
            "bundle carries [project] entries; import it with --project from the workspace instead"
        ),
        BundleScope::Project if !bundle.global.entries.is_empty() => bail!(
            "bundle carries [global] entries; importing them into a project document would leak machine state"
        ),
        _ => {}
    }
    validate_scope_target(scope, store.path())?;
    let mut plan = plan_import(bundle, &store.config, scope);
    if !plan.conflicting.is_empty() || (plan.is_no_op() && plan.skipped.is_empty()) {
        return Ok(PreparedImport {
            plan,
            candidate: store.config.clone(),
        });
    }
    let rendered;
    let original = if let Some(original) = store.original_body() {
        original
    } else {
        rendered = store.rendered_body()?;
        &rendered
    };
    let mut document =
        codewhale_engine::route_preferences::prepare_document(store.path(), original)?;
    let mut candidate = config_from_document(&document.to_string())?;
    let mut model_edits = Vec::<(String, String, String)>::new();
    let mut selected_provider = None;
    for (section, table) in [
        ("preferences", &bundle.preferences),
        ("profiles", &bundle.profiles),
        ("plugins", &bundle.plugins),
        ("project", &bundle.project),
        ("global", &bundle.global),
    ] {
        if !section_applies(section, scope) {
            continue;
        }
        for (key, value) in &table.entries {
            let dotted = format!("{section}.{key}");
            if nonportable_path_reason(key).is_some()
                || value_rejection_reason(key, value).is_some()
            {
                bail!("refusing to import non-portable config path {dotted}");
            }
            if key == "provider" {
                selected_provider = Some(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow!("config entry {dotted:?} must be a string"))?,
                );
                continue;
            }
            collect_model_edits(&dotted, key, value, &mut model_edits)?;
            if codewhale_engine::route_preferences::is_route_key(key) {
                continue;
            }
            apply_config_value(&mut candidate, key, value)?;
        }
    }
    document = toml::to_string(&toml::Value::Table(config_document(&candidate)?))?
        .parse()
        .map_err(|_| anyhow!("could not prepare imported configuration; contents omitted"))?;
    // Definitions precede the exact final selector. Root aliases then target
    // that selected route, so an old provider slot cannot mask an imported model.
    if let Some(provider) = selected_provider {
        codewhale_engine::route_preferences::set_document(
            store.path(),
            &mut document,
            "provider",
            provider,
        )?;
    }
    model_edits.sort_by_key(|(_, key, _)| !key.starts_with("providers."));
    for (_, key, value) in &model_edits {
        codewhale_engine::route_preferences::set_document(store.path(), &mut document, key, value)?;
    }
    let final_value: toml::Value = toml::from_str(&document.to_string())?;
    for (dotted, key, value) in &model_edits {
        let mut replay = document.clone();
        codewhale_engine::route_preferences::set_document(store.path(), &mut replay, key, value)?;
        if toml::from_str::<toml::Value>(&replay.to_string())? != final_value {
            plan.conflicting.push(dotted.clone());
        }
    }
    candidate = config_from_document(&document.to_string())?;
    // Run the same validation/serialization as the final save before consent
    // or backup creation. This clone never writes or replaces the CAS snapshot.
    let mut validation_store = store.clone();
    validation_store.config = candidate.clone();
    validation_store.rendered_body()?;
    if config_document(&candidate)? == config_document(&store.config)? {
        plan.skipped.append(&mut plan.added);
        plan.skipped.append(&mut plan.changed);
    } else {
        // A raw root alias may already match while its canonical provider
        // slot differs. Such an import is a real change, not a skipped write.
        if plan.is_no_op() {
            for (dotted, _, _) in &model_edits {
                plan.skipped.retain(|key| key != dotted);
                plan.changed.push(dotted.clone());
            }
        }
        let original_value: toml::Value = toml::from_str(original)?;
        if original_value.get("route_preferences_version").is_none()
            && final_value.get("route_preferences_version").is_some()
        {
            plan.added
                .push("global.route_preferences_version (local migration)".to_string());
        }
    }
    for keys in [
        &mut plan.added,
        &mut plan.changed,
        &mut plan.skipped,
        &mut plan.conflicting,
    ] {
        keys.sort();
        keys.dedup();
    }
    Ok(PreparedImport { plan, candidate })
}

fn collect_model_edits(
    dotted: &str,
    key: &str,
    value: &toml::Value,
    edits: &mut Vec<(String, String, String)>,
) -> Result<()> {
    if key != "provider" && codewhale_engine::route_preferences::is_route_key(key) {
        let value = value
            .as_str()
            .ok_or_else(|| anyhow!("config entry {dotted:?} must be a string"))?;
        edits.push((dotted.to_string(), key.to_string(), value.to_string()));
    } else if (key == "providers" || key.starts_with("providers."))
        && let Some(table) = value.as_table()
    {
        for (child, value) in table {
            collect_model_edits(
                &format!("{dotted}.{child}"),
                &format!("{key}.{child}"),
                value,
                edits,
            )?;
        }
    }
    Ok(())
}

fn config_from_document(body: &str) -> Result<ConfigToml> {
    let mut config = codewhale_config::parse_config_toml(body).map_err(|_| {
        anyhow!("imported configuration has an invalid TOML type; contents omitted")
    })?;
    let document: toml::Value = toml::from_str(body)?;
    if let Some(provider) = document.get("provider").and_then(toml::Value::as_str) {
        config.bind_persisted_provider_id(provider)?;
    }
    Ok(config)
}

fn save_candidate(
    candidate: ConfigToml,
    store: &mut codewhale_config::ConfigStore,
    target_written: &mut bool,
) -> Result<()> {
    store.config = candidate;
    store.save().context("saving imported bundle")?;
    *target_written = true;
    Ok(())
}

fn apply_config_value(config: &mut ConfigToml, key: &str, value: &toml::Value) -> Result<()> {
    if codewhale_config::config_toml_choices(key).is_some()
        || key == "auth.mode"
        || key == "hook_sinks.unix_socket_path"
        || key.starts_with("providers.")
    {
        return config.set_value(key, &render_toml_value(value)?);
    }

    let mut document = config_document(config)?;
    if let Some(current) = document.get_mut(key) {
        deep_merge_toml_value(current, value);
    } else {
        document.insert(key.to_string(), value.clone());
    }
    // As in `config_document`, round-trip through TOML text so datetimes in
    // flattened extras stay TOML datetimes instead of serde-private marker
    // tables or strings.
    let text = toml::to_string(&toml::Value::Table(document))
        .with_context(|| format!("config entry {key:?} could not be serialized"))?;
    *config = config_from_document(&text)?;
    Ok(())
}

/// Merge a portable value into the target document without treating omitted
/// table leaves as deletions. Tables recurse; arrays and scalars represent an
/// explicit portable choice and replace the corresponding target value.
fn deep_merge_toml_value(target: &mut toml::Value, incoming: &toml::Value) {
    match (target, incoming) {
        (toml::Value::Table(target), toml::Value::Table(incoming)) => {
            for (key, value) in incoming {
                if let Some(current) = target.get_mut(key) {
                    deep_merge_toml_value(current, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, incoming) => *target = incoming.clone(),
    }
}

/// Render a TOML value into the scalar text `config set` accepts.
fn render_toml_value(value: &toml::Value) -> Result<String> {
    Ok(match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Float(number) => number.to_string(),
        toml::Value::Boolean(flag) => flag.to_string(),
        toml::Value::Datetime(text) => text.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => {
            toml::to_string(value)?.trim_end().to_string()
        }
    })
}

fn create_collision_safe_backup(target: &Path) -> Result<PathBuf> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_string());
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let prefix = format!("{file_name}.bundle-backup-{timestamp}-");
    // NamedTempFile uses exclusive creation and restrictive initial
    // permissions, so concurrent same-second imports cannot clobber an older
    // receipt or expose config bytes before target permissions are applied.
    let mut backup = tempfile::Builder::new()
        .prefix(&prefix)
        .tempfile_in(parent)
        .with_context(|| {
            format!(
                "creating a collision-safe backup beside {}",
                target.display()
            )
        })?;
    let mut source = std::fs::File::open(target)
        .with_context(|| format!("opening {} for bundle backup", target.display()))?;
    std::io::copy(&mut source, backup.as_file_mut())
        .with_context(|| format!("copying {} into its bundle backup", target.display()))?;
    use std::io::Write as _;
    backup
        .as_file_mut()
        .flush()
        .context("flushing bundle backup")?;
    let permissions = source
        .metadata()
        .with_context(|| format!("reading permissions for {}", target.display()))?
        .permissions();
    std::fs::set_permissions(backup.path(), permissions)
        .context("preserving config permissions on bundle backup")?;
    backup
        .as_file()
        .sync_all()
        .context("syncing bundle backup")?;
    let (_file, path) = backup
        .keep()
        .map_err(|error| error.error)
        .context("persisting bundle backup")?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

/// Require explicit consent before mutating: interactive sessions get a
/// prompt; headless runs require `--yes`.
pub fn require_import_consent(yes: bool, plan: &ImportPlan) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "import refused: non-interactive use requires explicit --yes after reviewing the plan"
        );
    }
    print!(
        "Apply this bundle ({} added, {} changed)? Type 'yes': ",
        plan.added.len(),
        plan.changed.len()
    );
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("reading import consent")?;
    if answer.trim() != "yes" {
        bail!("import cancelled; no configuration was changed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

/// Arguments for `codewhale config import`.
#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    /// Bundle source: a file path, an HTTPS URL, or `-` for stdin.
    pub source: String,
    /// Print the deterministic import plan without writing anything.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
    /// Skip the interactive consent prompt (required for headless use).
    #[arg(long, default_value_t = false)]
    yes: bool,
    /// Target the project config instead of the user-global document.
    #[arg(long, default_value_t = false)]
    project: bool,
}

/// Arguments for `codewhale config export --portable`.
#[derive(Debug, clap::Args)]
pub struct ExportArgs {
    /// Emit a portable, secret-free bundle (required flag; plain `export`
    /// is reserved so a future non-portable format cannot silently change
    /// what the command writes).
    #[arg(long, default_value_t = false)]
    portable: bool,
    /// Export the project config instead of the user-global document.
    #[arg(long, default_value_t = false)]
    project: bool,
    /// Write to this path instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

/// Run `config import`.
pub fn run_import(
    args: &ImportArgs,
    store: &mut codewhale_config::ConfigStore,
    _workspace: &Path,
) -> Result<()> {
    let scope = if args.project {
        BundleScope::Project
    } else {
        BundleScope::Global
    };
    validate_scope_target(scope, store.path())?;
    let remote_source = args.source.starts_with("https://") || args.source.starts_with("http://");
    let source_label = if args.source == "-" {
        "stdin"
    } else if remote_source {
        "remote bundle"
    } else {
        args.source.as_str()
    };
    let raw = if args.source == "-" {
        let mut buffer = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAX_BUNDLE_BYTES + 1)
            .read_to_end(&mut buffer)
            .context("reading bundle from stdin")?;
        if buffer.len() as u64 > MAX_BUNDLE_BYTES {
            bail!("stdin bundle exceeds the {MAX_BUNDLE_BYTES} byte limit; refused");
        }
        buffer
    } else if remote_source {
        fetch_bundle(&args.source)?
    } else {
        let path = PathBuf::from(&args.source);
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("reading bundle at {}", path.display()))?;
        if metadata.len() > MAX_BUNDLE_BYTES {
            bail!(
                "bundle at {} is {} bytes; the limit is {MAX_BUNDLE_BYTES} bytes",
                path.display(),
                metadata.len()
            );
        }
        std::fs::read(&path).with_context(|| format!("reading bundle at {}", path.display()))?
    };

    let bundle = parse_bundle_bytes(&raw, source_label)?;
    let prepared = prepare_import(&bundle, store, scope)?;
    let plan = &prepared.plan;

    println!("import plan ({} scope, {source_label}):", scope.label());
    println!("  added:       {}", plan.added.len());
    println!("  changed:     {}", plan.changed.len());
    println!("  skipped:     {}", plan.skipped.len());
    println!("  conflicting: {}", plan.conflicting.len());
    println!("  rejected:    {}", plan.rejected.len());
    for entry in &plan.added {
        println!("  + {entry}");
    }
    for entry in &plan.changed {
        println!("  ~ {entry}");
    }
    for entry in &plan.rejected {
        println!("  ! {} — {}", entry.key, entry.reason);
    }
    for entry in &plan.conflicting {
        println!("  x {entry}");
    }

    if args.dry_run {
        println!("dry run: nothing was written");
        return Ok(());
    }

    require_import_consent(args.yes, plan)?;
    let receipt = apply_prepared_bundle(prepared, store, save_candidate)?;
    if receipt.plan.is_no_op() {
        println!("nothing to apply; config already matches the bundle (idempotent re-import)");
        return Ok(());
    }
    println!(
        "imported: {} added, {} changed into {}",
        receipt.plan.added.len(),
        receipt.plan.changed.len(),
        receipt.target.display()
    );
    if let Some(backup) = &receipt.backup_path {
        println!("pre-import backup: {}", backup.display());
    }
    Ok(())
}

/// Run `config export --portable`.
pub fn run_export(args: &ExportArgs, store: &codewhale_config::ConfigStore) -> Result<()> {
    if !args.portable {
        bail!("config export requires --portable; plain export is not defined yet");
    }
    let scope = if args.project {
        BundleScope::Project
    } else {
        BundleScope::Global
    };
    validate_scope_target(scope, store.path())?;
    let metadata = BundleMetadata {
        name: None,
        created_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        generator: Some(format!("codewhale {}", env!("CARGO_PKG_VERSION"))),
    };
    let bundle = export_bundle(&store.config, scope, metadata)?;
    let body = serialize_bundle(&bundle)?;
    match &args.out {
        Some(path) => {
            codewhale_config::persistence::atomic_write(path, body.as_bytes())
                .with_context(|| format!("writing bundle to {}", path.display()))?;
            println!("wrote portable bundle to {}", path.display());
        }
        None => {
            use std::io::Write;
            std::io::stdout().write_all(body.as_bytes())?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::ConfigStore;
    use std::io::Write;
    use std::net::{Ipv4Addr, TcpListener};

    const VALID_TOML: &str = r#"
schema_version = 1
kind = "codewhale.portable-config"

[metadata]
name = "team-baseline"

[preferences]
verbosity = "concise"
telemetry = false

[global]
log_level = "debug"
"#;

    fn sample_bundle() -> PortableBundle {
        parse_bundle_str(VALID_TOML, "test.toml").expect("valid bundle")
    }

    #[test]
    fn valid_bundle_parses_and_validates() {
        let bundle = sample_bundle();
        assert_eq!(bundle.schema_version, 1);
        assert_eq!(bundle.kind, "codewhale.portable-config");
        assert_eq!(bundle.metadata.name.as_deref(), Some("team-baseline"));
        assert_eq!(bundle.preferences.entries.len(), 2);
    }

    #[test]
    fn unknown_envelope_fields_fail_the_parse() {
        let text = r#"
schema_version = 1
kind = "codewhale.portable-config"
sneaky_extra = true
"#;
        let err = parse_bundle_str(text, "test.toml").expect_err("unknown field must fail");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("unknown field"), "{rendered}");
    }

    #[test]
    fn wrong_kind_or_schema_version_is_refused() {
        let bad_kind = "schema_version = 1
kind = \"something-else\"\n";
        let err = parse_bundle_str(bad_kind, "t.toml").expect_err("kind must match");
        assert!(err.to_string().contains("kind"), "{err:#}");

        let bad_version = "schema_version = 99\nkind = \"codewhale.portable-config\"\n";
        let err = parse_bundle_str(bad_version, "t.toml").expect_err("schema version must match");
        assert!(err.to_string().contains("schema_version"), "{err:#}");
    }

    #[test]
    fn json_bundles_parse_when_the_document_is_json() {
        let json = r#"{"schema_version": 1, "kind": "codewhale.portable-config",
            "preferences": {"verbosity": "quiet"}}"#;
        let bundle = parse_bundle_str(json, "bundle.json").expect("json bundle");
        assert_eq!(bundle.preferences.entries.len(), 1);
    }

    #[test]
    fn oversize_input_is_refused_before_parse() {
        let big = vec![b'#'; (MAX_BUNDLE_BYTES + 1) as usize];
        let err = parse_bundle_bytes(&big, "big.toml").expect_err("oversize must fail");
        assert!(err.to_string().contains("limit"), "{err:#}");
    }

    #[test]
    fn credential_keys_are_rejected_by_name() {
        let text = r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
api_key = "value-is-never-echoed"

[preferences]
openai_api_key = "also-secret"
"#;
        let bundle = parse_bundle_str(text, "t.toml").expect("parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 2, "{rejected:?}");
        assert!(rejected.iter().any(|r| r.key == "global.api_key"));
        assert!(
            rejected
                .iter()
                .all(|r| !r.reason.contains("value-is-never-echoed"))
        );
    }

    #[test]
    fn credential_shaped_values_are_rejected_under_benign_names() {
        let text = r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
note = "sk-abcdefghij0123456789"
"#;
        let bundle = parse_bundle_str(text, "t.toml").expect("parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert!(!rejected[0].reason.contains("sk-abcdefghij"));
    }

    #[test]
    fn nested_secret_keys_and_values_are_rejected_without_echoing_values() {
        let shaped_value = ["Bear", "er nested-token-must-not-leak"].concat();
        let text = format!(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences.with_key.nested]
password = "nested-password-must-not-leak"

[preferences.with_value.nested]
note = "{shaped_value}"
"#
        );
        let bundle = parse_bundle_str(&text, "nested.toml").expect("bundle parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 2, "{rejected:?}");
        assert!(
            rejected
                .iter()
                .any(|entry| entry.key == "preferences.with_key")
        );
        assert!(
            rejected
                .iter()
                .any(|entry| entry.key == "preferences.with_value")
        );
        let rendered = format!("{rejected:?}");
        assert!(!rendered.contains("nested-password-must-not-leak"));
        assert!(!rendered.contains("nested-token-must-not-leak"));
    }

    #[test]
    fn json_bundles_with_duplicate_keys_fail_before_parse() {
        let duplicate = r#"{"schema_version":1,"kind":"codewhale.portable-config","preferences":{"verbosity":"quiet","verbosity":"loud"}}"#;
        let error = parse_bundle_str(duplicate, "dup.json").expect_err("duplicate key must fail");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("duplicate key"), "{rendered}");
        assert!(rendered.contains("preferences.verbosity"), "{rendered}");
        assert!(!rendered.contains("loud"), "{rendered}");

        let nested_array = r#"{"schema_version":1,"kind":"codewhale.portable-config","preferences":{"list":[{"a":1,"a":2}]}}"#;
        let error = parse_bundle_str(nested_array, "dup-array.json")
            .expect_err("nested duplicate must fail");
        assert!(
            format!("{error:#}").contains("preferences.list.[0].a"),
            "{error:#}"
        );

        let clean = r#"{"schema_version":1,"kind":"codewhale.portable-config","preferences":{"verbosity":"quiet","profiles":{"verbosity":"loud"}}}"#;
        parse_bundle_str(clean, "clean.json").expect("same key under different parents is fine");
    }

    #[test]
    fn network_proxy_routes_are_rejected_on_import_and_scrubbed_on_export() {
        let proxy_url = ["http://proxy-user:proxy-", "pass@proxy.internal:3128"].concat();
        let text = format!(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global.network]
default = "prompt"
allow = ["registry.example"]
proxy = ["{proxy_url}"]
"#
        );
        let bundle = parse_bundle_str(&text, "network-proxy.toml").expect("bundle parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert_eq!(rejected[0].key, "global.network");
        let rendered = format!("{rejected:?}");
        assert!(!rendered.contains("proxy-pass"), "{rendered}");
        assert!(!rendered.contains("proxy.internal"), "{rendered}");

        let config: ConfigToml = toml::from_str(&format!(
            r#"
[network]
default = "prompt"
allow = ["registry.example"]
proxy = ["{proxy_url}"]
"#
        ))
        .expect("network config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("network proxy is scrubbed");
        let body = serialize_bundle(&exported).expect("serialize network export");
        assert!(!body.contains("proxy-user"), "{body}");
        assert!(!body.contains("proxy.internal"), "{body}");
        let reparsed: toml::Value = toml::from_str(&body).expect("export reparses");
        let network = reparsed
            .get("global")
            .and_then(|global| global.get("network"))
            .expect("portable network policy is kept");
        assert!(network.get("proxy").is_none(), "{body}");
        assert_eq!(
            network.get("default").and_then(toml::Value::as_str),
            Some("prompt")
        );
        assert!(body.contains("registry.example"), "{body}");
    }

    #[test]
    fn redaction_placeholders_are_rejected_on_import_and_export() {
        let placeholder = codewhale_config::persistence::REDACTED;
        let text = format!(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
verbosity = "quiet"
note = "prefix {placeholder} suffix"
"#
        );
        let bundle = parse_bundle_str(&text, "placeholder.toml").expect("bundle parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert_eq!(rejected[0].key, "preferences.note");
        assert!(
            rejected[0].reason.contains("redaction placeholder"),
            "{rejected:?}"
        );

        let config: ConfigToml = toml::from_str(&format!(
            "verbosity = \"quiet\"\nnote = \"prefix {placeholder} suffix\"\n"
        ))
        .expect("placeholder config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("placeholder is scrubbed");
        let body = serialize_bundle(&exported).expect("serialize placeholder export");
        assert!(!body.contains(placeholder), "{body}");
        assert!(body.contains("quiet"), "{body}");
    }

    #[test]
    fn camel_case_and_dotted_secret_keys_avoid_token_count_false_positives() {
        for key in [
            "accessToken",
            "refreshToken",
            "clientSecret",
            "apiKey",
            "api.key",
            "private.key",
            "accessKey",
            "aws_access_key",
            "awsSecretAccessKey",
            "Cookie",
            "Set-Cookie",
            "providers.xai.auth.command",
            "providers.xai.external_credentials",
            "providers.xai.oauth_credential_generation",
            "nested.service.accessToken",
            "nested.service.refreshToken",
        ] {
            assert!(is_sensitive_bundle_key(key), "must reject {key}");
        }
        for key in [
            "auth_mode",
            "maxTokens",
            "tokenizer",
            "tokenBudget",
            "max_tokens",
        ] {
            assert!(!is_sensitive_bundle_key(key), "must preserve {key}");
        }
    }

    #[test]
    fn compound_and_access_keys_are_rejected_before_import_without_mutation() {
        let api_dot = ["api", ".key"].concat();
        let private_dot = ["private", ".key"].concat();
        let access_camel = ["access", "Key"].concat();
        let aws_snake = ["aws", "_access_key"].concat();
        let aws_camel = ["aws", "SecretAccessKey"].concat();
        let cookie = ["Coo", "kie"].concat();
        let set_cookie = ["Set-", "Cookie"].concat();
        let text = format!(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
"{api_dot}" = "opaque-api-value"
"{private_dot}" = "opaque-private-value"
{access_camel} = "opaque-access-value"
{aws_snake} = "opaque-aws-access-value"
{aws_camel} = "opaque-aws-secret-access-value"
maxTokens = 8192
tokenizer = "bpe"

[preferences.http_headers]
{cookie} = "opaque-cookie-import-value"
{set_cookie} = "opaque-set-cookie-import-value"
"#
        );
        let bundle =
            parse_bundle_str(&text, "compound-secrets.toml").expect("compound-key bundle parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 6, "{rejected:?}");
        assert!(
            rejected
                .iter()
                .any(|entry| entry.key == "preferences.http_headers"),
            "{rejected:?}"
        );
        assert!(
            rejected
                .iter()
                .all(|entry| !entry.key.contains("maxTokens") && !entry.key.contains("tokenizer")),
            "{rejected:?}"
        );

        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"quiet\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before import");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        let error = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect_err("credential keys must refuse the entire import");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("conflicting or rejected"), "{rendered}");
        for secret in [
            "opaque-api-value",
            "opaque-private-value",
            "opaque-access-value",
            "opaque-aws-access-value",
            "opaque-aws-secret-access-value",
            "opaque-cookie-import-value",
            "opaque-set-cookie-import-value",
        ] {
            assert!(
                !rendered.contains(secret),
                "error leaked {secret}: {rendered}"
            );
        }
        assert_eq!(std::fs::read(path).expect("config after refusal"), before);
        assert_eq!(store.config.verbosity.as_deref(), Some("quiet"));
    }

    #[test]
    fn plan_reports_added_changed_skipped_deterministically() {
        let store = isolated_store();
        let bundle_text = r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
verbosity = "concise"
log_level = "debug"

[global]
telemetry = false
"#;
        let bundle = parse_bundle_str(bundle_text, "t.toml").expect("bundle");
        // verbosity already matches; log_level is new; telemetry is global-scope.
        let plan_global = plan_import(&bundle, &store.config, BundleScope::Global);
        assert!(
            plan_global
                .added
                .contains(&"preferences.log_level".to_string())
        );
        // `verbosity` resolves to a shipped default even when the file key is
        // unset, so an equal value reads as changed-or-skipped by resolution;
        // what matters for determinism is that every entry lands in exactly
        // one bucket and nothing is dropped silently.
        let all: std::collections::BTreeSet<&String> = plan_global
            .added
            .iter()
            .chain(plan_global.changed.iter())
            .chain(plan_global.skipped.iter())
            .collect();
        assert_eq!(all.len(), 3, "{plan_global:?}");
        // Project scope skips global-section entries.
        let plan_project = plan_import(&bundle, &store.config, BundleScope::Project);
        assert!(
            plan_project
                .skipped
                .contains(&"global.telemetry".to_string())
        );
    }

    #[test]
    fn rejected_entries_show_up_as_conflicting_in_the_plan() {
        let store = isolated_store();
        let text = r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
api_key = "never-echoed"
"#;
        let bundle = parse_bundle_str(text, "t.toml").expect("bundle");
        let plan = plan_import(&bundle, &store.config, BundleScope::Global);
        assert!(plan.conflicting.contains(&"global.api_key".to_string()));
        assert!(plan.added.is_empty());
    }

    #[test]
    fn duplicate_flat_keys_across_applicable_sections_fail_before_apply() {
        let mut store = isolated_store();
        let before = std::fs::read(store.path()).expect("config before import");
        let text = r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
verbosity = "concise"

[global]
verbosity = "normal"
"#;
        let bundle = parse_bundle_str(text, "collision.toml").expect("bundle parses");
        let plan = plan_import(&bundle, &store.config, BundleScope::Global);

        assert_eq!(
            plan.conflicting,
            ["preferences.verbosity", "global.verbosity"]
        );
        assert!(plan.added.is_empty(), "{plan:?}");
        assert!(plan.changed.is_empty(), "{plan:?}");
        assert!(plan.skipped.is_empty(), "{plan:?}");

        let workspace = tempfile::tempdir().expect("workspace");
        let error = apply_bundle(&bundle, &mut store, BundleScope::Global, workspace.path())
            .expect_err("ambiguous flat key must fail closed");
        let rendered = error.to_string();
        assert!(rendered.contains("conflicting"), "{error:#}");
        assert!(!rendered.contains("concise"), "{error:#}");
        assert!(!rendered.contains("normal"), "{error:#}");
        assert_eq!(
            std::fs::read(store.path()).expect("config after refused import"),
            before,
            "collision must be refused before any write"
        );
    }

    #[test]
    fn dry_run_semantics_plan_never_mutates() {
        let store = isolated_store();
        let before = std::fs::read_to_string(store.path()).expect("read config");
        let bundle = sample_bundle();
        let _prepared = prepare_import(&bundle, &store, BundleScope::Global)
            .expect("prepare import without writing");
        let after = std::fs::read_to_string(store.path()).expect("read config");
        assert_eq!(before, after, "planning must not write");
    }

    #[test]
    fn route_import_prepares_migration_once_and_overrides_a_masking_provider_slot() {
        use crate::tests::{ScopedEnvVar, env_lock};
        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.path().to_string_lossy());
        let _config_override = ScopedEnvVar::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_override = ScopedEnvVar::remove("DEEPSEEK_CONFIG_PATH");
        let path = home.path().join("config.toml");
        let original = "provider = 'zai'\ndefault_text_model = 'GLM-5.3'\n[providers.zai]\nmodel = 'GLM-5.2'\n";
        std::fs::write(&path, original).expect("seed config");
        let settings_path = home.path().join("settings.toml");
        let old_settings = "default_provider = 'deepseek'\n[provider_models]\ndeepseek = 'deepseek-v4-pro'\nzai = 'GLM-5.4'\n";
        std::fs::write(&settings_path, old_settings).expect("seed legacy choices");
        let bundle = parse_bundle_str(
            "schema_version = 1\nkind = 'codewhale.portable-config'\n[global]\nprovider = 'zai'\ndefault_text_model = 'GLM-5.3'\n",
            "route.toml",
        ).expect("route bundle");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store");
        let prepared = prepare_import(&bundle, &store, BundleScope::Global).expect("preview");
        assert!(
            prepared
                .plan
                .changed
                .iter()
                .any(|key| key == "global.default_text_model")
        );
        assert!(
            !prepared.plan.is_no_op(),
            "the old slot still masks the root value"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(&settings_path).unwrap(),
            old_settings
        );

        // Consent commits exactly the preview even if the archived input moves.
        let later_settings = "default_provider = 'openai'\n";
        std::fs::write(&settings_path, later_settings).unwrap();
        apply_prepared_bundle(prepared, &mut store, save_candidate).expect("commit preview");
        let saved: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["provider"].as_str(), Some("zai"));
        assert_eq!(saved["providers"]["zai"]["model"].as_str(), Some("GLM-5.3"));
        assert_eq!(
            saved["providers"]["deepseek"]["model"].as_str(),
            Some("deepseek-v4-pro")
        );
        assert_eq!(saved["route_preferences_version"].as_integer(), Some(1));
        assert_eq!(
            std::fs::read_to_string(settings_path).unwrap(),
            later_settings
        );
        assert_eq!(
            codewhale_engine::route_preferences::get(&path, "provider")
                .unwrap()
                .as_deref(),
            Some("zai")
        );
        assert_eq!(
            codewhale_engine::route_preferences::get(&path, "model")
                .unwrap()
                .as_deref(),
            Some("GLM-5.3")
        );
        let again = prepare_import(&bundle, &store, BundleScope::Global).expect("repeat preview");
        assert!(again.plan.is_no_op(), "{:?}", again.plan);
    }

    #[test]
    fn conflicting_import_model_aliases_fail_before_any_write() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        let original = "provider = 'zai'\n[providers.zai]\nmodel = 'GLM-5.2'\n";
        std::fs::write(&path, original).unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        let bundle = parse_bundle_str(
            "schema_version = 1\nkind = 'codewhale.portable-config'\n[global]\ndefault_text_model = 'GLM-5.3'\n[global.providers.zai]\nmodel = 'GLM-5.4'\n",
            "conflict.toml",
        ).unwrap();
        let prepared = prepare_import(&bundle, &store, BundleScope::Global).unwrap();
        assert!(
            prepared
                .plan
                .conflicting
                .iter()
                .any(|key| key == "global.providers.zai.model")
        );
        let error = apply_prepared_bundle(prepared, &mut store, save_candidate)
            .expect_err("conflicting aliases must be refused");
        assert!(error.to_string().contains("conflicting"));
        assert!(!error.to_string().contains("GLM-5.4"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no backup or staged write before validation"
        );
    }

    #[test]
    fn prepared_import_keeps_configstore_cas_against_concurrent_edits() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = 'deepseek'\n").unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        let prepared = prepare_import(&sample_bundle(), &store, BundleScope::Global).unwrap();
        let concurrent = "provider = 'openai'\n# concurrent writer\n";
        std::fs::write(&path, concurrent).unwrap();
        apply_prepared_bundle(prepared, &mut store, save_candidate)
            .expect_err("stale preview must fail closed");
        assert_eq!(std::fs::read_to_string(path).unwrap(), concurrent);
    }

    #[test]
    fn route_migration_receipts_are_local_and_not_portable() {
        let bundle = parse_bundle_str(
            "schema_version = 1\nkind = 'codewhale.portable-config'\n[global]\nroute_preferences_version = 1\n[global.route_preferences_migration]\nprevious_provider = 'zai'\n",
            "receipt.toml",
        ).unwrap();
        assert_eq!(find_rejected_entries(&bundle).len(), 2);
        let config: ConfigToml = toml::from_str(
            "route_preferences_version = 1\n[route_preferences_migration]\nprevious_provider = 'zai'\n",
        ).unwrap();
        let exported =
            export_bundle(&config, BundleScope::Global, BundleMetadata::default()).unwrap();
        assert!(
            !exported
                .global
                .entries
                .contains_key("route_preferences_version")
        );
        assert!(
            !exported
                .global
                .entries
                .contains_key("route_preferences_migration")
        );
    }

    #[test]
    fn canonical_route_export_omits_shadowed_roots_and_round_trips_provider_slots() {
        let config: ConfigToml = toml::from_str(
            "provider = 'zai'\ndefault_text_model = 'deepseek-v4-pro'\nmodel = 'old-root-model'\n[providers.zai]\nmodel = 'GLM-5.3'\n[providers.deepseek]\nmodel = 'deepseek-v4-flash'\n[providers.openai]\nmodel = 'gpt-4.1'\n",
        ).unwrap();
        let bundle =
            export_bundle(&config, BundleScope::Global, BundleMetadata::default()).unwrap();
        assert!(!bundle.global.entries.contains_key("model"));
        assert!(!bundle.global.entries.contains_key("default_text_model"));
        let dir = tempfile::tempdir().expect("config dir");
        let mut store = ConfigStore::load(Some(dir.path().join("config.toml"))).unwrap();
        let receipt = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect("canonical export must import without alias conflicts");
        assert!(receipt.plan.conflicting.is_empty());
        assert_eq!(store.config.provider_id(), "zai");
        assert_eq!(
            store.config.get_value("providers.zai.model").as_deref(),
            Some("GLM-5.3")
        );
        assert_eq!(
            store
                .config
                .get_value("providers.deepseek.model")
                .as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            store.config.get_value("providers.openai.model").as_deref(),
            Some("gpt-4.1")
        );
        let again = export_bundle(
            &store.config,
            BundleScope::Global,
            BundleMetadata::default(),
        )
        .unwrap();
        assert_eq!(
            serialize_bundle(&again).unwrap(),
            serialize_bundle(&bundle).unwrap()
        );

        let root_only: ConfigToml =
            toml::from_str("default_text_model = 'deepseek-v4-pro'\n").unwrap();
        let legacy =
            export_bundle(&root_only, BundleScope::Global, BundleMetadata::default()).unwrap();
        assert_eq!(
            legacy
                .global
                .entries
                .get("default_text_model")
                .and_then(toml::Value::as_str),
            Some("deepseek-v4-pro")
        );
        let literal_custom = config_from_document(
            "provider = 'custom'\nbase_url = 'https://literal.example.test/v1'\ndefault_text_model = 'LiteralRootModel'\n",
        ).unwrap();
        let literal_export = export_bundle(
            &literal_custom,
            BundleScope::Global,
            BundleMetadata::default(),
        )
        .unwrap();
        // The literal route's top-level fields became `[providers.custom]`
        // when parsed (#6394); its model survives the export.
        assert!(
            serialize_bundle(&literal_export)
                .unwrap()
                .contains("LiteralRootModel"),
            "{literal_export:#?}"
        );
    }

    #[test]
    fn export_reconciles_a_legacy_root_default_model_with_the_deepseek_slot() {
        // Migration writes the canonical slot but never removes a legacy root
        // `default_model`; on import that alias also targets the DeepSeek slot,
        // so a raw export would conflict with itself.
        let config: ConfigToml = toml::from_str(
            "provider = 'deepseek'\ndefault_model = 'deepseek-v4-flash'\n[providers.deepseek]\nmodel = 'deepseek-v4-pro'\n",
        )
        .unwrap();
        let bundle =
            export_bundle(&config, BundleScope::Global, BundleMetadata::default()).unwrap();
        assert!(!bundle.global.entries.contains_key("default_model"));
        let providers = bundle
            .global
            .entries
            .get("providers")
            .and_then(toml::Value::as_table)
            .expect("providers table");
        assert_eq!(
            providers["deepseek"]["model"].as_str(),
            Some("deepseek-v4-pro")
        );

        let dir = tempfile::tempdir().expect("config dir");
        let mut store = ConfigStore::load(Some(dir.path().join("config.toml"))).unwrap();
        let receipt = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect("export must import without alias conflicts");
        assert!(receipt.plan.conflicting.is_empty(), "{:?}", receipt.plan);
        assert_eq!(
            store
                .config
                .get_value("providers.deepseek.model")
                .as_deref(),
            Some("deepseek-v4-pro")
        );

        // Without a canonical slot the root alias folds into the DeepSeek slot.
        let config: ConfigToml = toml::from_str(
            "provider = 'zai'\ndefault_model = 'deepseek-v4-flash'\n[providers.zai]\nmodel = 'GLM-5.3'\n",
        )
        .unwrap();
        let bundle =
            export_bundle(&config, BundleScope::Global, BundleMetadata::default()).unwrap();
        assert!(!bundle.global.entries.contains_key("default_model"));
        let providers = bundle
            .global
            .entries
            .get("providers")
            .and_then(toml::Value::as_table)
            .expect("providers table");
        assert_eq!(
            providers["deepseek"]["model"].as_str(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(providers["zai"]["model"].as_str(), Some("GLM-5.3"));
    }

    #[test]
    fn export_preserves_the_deepseek_root_fallback_when_another_route_is_active() {
        // With Z.ai active, a DeepSeek-id root `default_text_model` is
        // DeepSeek's saved fallback, not shadowed state; the active route's
        // canonical slot must not cause it to be dropped.
        let config: ConfigToml = toml::from_str(
            "provider = 'zai'\ndefault_text_model = 'deepseek-v4-flash'\n[providers.zai]\nmodel = 'GLM-5.3'\n",
        )
        .unwrap();
        let bundle =
            export_bundle(&config, BundleScope::Global, BundleMetadata::default()).unwrap();
        assert!(!bundle.global.entries.contains_key("default_text_model"));
        let providers = bundle
            .global
            .entries
            .get("providers")
            .and_then(toml::Value::as_table)
            .expect("providers table");
        assert_eq!(
            providers["deepseek"]["model"].as_str(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(providers["zai"]["model"].as_str(), Some("GLM-5.3"));

        let dir = tempfile::tempdir().expect("config dir");
        let mut store = ConfigStore::load(Some(dir.path().join("config.toml"))).unwrap();
        let receipt = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect("export must import without alias conflicts");
        assert!(receipt.plan.conflicting.is_empty(), "{:?}", receipt.plan);
        assert_eq!(
            store
                .config
                .get_value("providers.deepseek.model")
                .as_deref(),
            Some("deepseek-v4-flash")
        );

        // A root alias the active route still consumes stays shadowed state and
        // is dropped; a canonical DeepSeek leaf wins over a duplicate root
        // fallback.
        let config: ConfigToml = toml::from_str(
            "provider = 'zai'\ndefault_text_model = 'deepseek-v4-pro'\nmodel = 'GLM-5.1'\n[providers.zai]\nmodel = 'GLM-5.3'\n[providers.deepseek]\nmodel = 'deepseek-v4-flash'\n",
        )
        .unwrap();
        let bundle =
            export_bundle(&config, BundleScope::Global, BundleMetadata::default()).unwrap();
        assert!(!bundle.global.entries.contains_key("model"));
        assert!(!bundle.global.entries.contains_key("default_text_model"));
        let providers = bundle
            .global
            .entries
            .get("providers")
            .and_then(toml::Value::as_table)
            .expect("providers table");
        assert_eq!(
            providers["deepseek"]["model"].as_str(),
            Some("deepseek-v4-flash")
        );
    }

    #[test]
    fn imports_preserve_exact_builtin_shadowing_custom_provider_identity() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = 'OpenAI'\n[providers.OpenAI]\nkind = 'openai-compatible'\nbase_url = 'https://custom.example.test/v1'\nmodel = 'LiteralOldModel'\n[providers.openai]\nmodel = 'gpt-4.1'\n",
        ).unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        for (entries, expected_model) in [
            ("verbosity = 'concise'\n", "LiteralOldModel"),
            (
                "provider = 'OpenAI'\nmodel = 'LiteralNewModel'\nlog_level = 'debug'\n",
                "LiteralNewModel",
            ),
        ] {
            let bundle = parse_bundle_str(
                &format!(
                    "schema_version = 1\nkind = 'codewhale.portable-config'\n[global]\n{entries}"
                ),
                "custom.toml",
            )
            .unwrap();
            apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path()).unwrap();
            let saved: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(saved["provider"].as_str(), Some("OpenAI"));
            assert_eq!(
                saved["providers"]["OpenAI"]["model"].as_str(),
                Some(expected_model)
            );
            assert_eq!(
                saved["providers"]["OpenAI"]["base_url"].as_str(),
                Some("https://custom.example.test/v1")
            );
            assert_eq!(
                saved["providers"]["openai"]["model"].as_str(),
                Some("gpt-4.1")
            );
            store.reload().unwrap();
            assert_eq!(
                store.config.provider,
                codewhale_config::ProviderKind::Custom
            );
            assert_eq!(store.config.provider_id(), "OpenAI");
            assert_eq!(
                codewhale_engine::route_preferences::get(&path, "provider")
                    .unwrap()
                    .as_deref(),
                Some("OpenAI")
            );
        }
    }

    #[test]
    fn imports_preserve_regional_selector_and_canonical_model_slot() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = 'deepseek-cn'\n[providers.deepseek_cn]\nmodel = 'deepseek-v4-pro'\n[providers.deepseek]\nmodel = 'deepseek-v4-pro'\n",
        ).unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        for (entries, expected_model) in [
            ("verbosity = 'concise'\n", "deepseek-v4-pro"),
            (
                "'providers.deepseek_cn.model' = 'deepseek-v4-flash'\n",
                "deepseek-v4-flash",
            ),
        ] {
            let bundle = parse_bundle_str(
                &format!(
                    "schema_version = 1\nkind = 'codewhale.portable-config'\n[global]\n{entries}"
                ),
                "regional.toml",
            )
            .unwrap();
            apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path()).unwrap();
            let saved: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(saved["provider"].as_str(), Some("deepseek-cn"));
            assert_eq!(
                saved["providers"]["deepseek_cn"]["model"].as_str(),
                Some(expected_model)
            );
            assert_eq!(
                saved["providers"]["deepseek"]["model"].as_str(),
                Some("deepseek-v4-pro")
            );
            store.reload().unwrap();
            assert_eq!(store.config.provider_id(), "deepseek-cn");
            assert_eq!(
                codewhale_engine::route_preferences::get(&path, "model")
                    .unwrap()
                    .as_deref(),
                Some(expected_model)
            );
            let mut export_config = store.config.clone();
            export_config.default_text_model = Some("stale-regional-root".to_string());
            let exported = export_bundle(
                &export_config,
                BundleScope::Global,
                BundleMetadata::default(),
            )
            .unwrap();
            assert!(!exported.global.entries.contains_key("default_text_model"));
            assert_eq!(
                exported
                    .global
                    .entries
                    .get("provider")
                    .and_then(toml::Value::as_str),
                Some("deepseek-cn")
            );
        }
    }

    #[test]
    fn apply_is_idempotent_on_reimport() {
        let mut store = isolated_store();
        let workspace = tempfile::tempdir().expect("workspace");
        let bundle = sample_bundle();

        let first = apply_bundle(&bundle, &mut store, BundleScope::Global, workspace.path())
            .expect("first import");
        assert!(first.plan.added.len() + first.plan.changed.len() > 0);

        let second = apply_bundle(&bundle, &mut store, BundleScope::Global, workspace.path())
            .expect("second import");
        assert!(
            second.plan.is_no_op(),
            "re-import must be a no-op: {:?}",
            second.plan
        );
        assert!(second.backup_path.is_none());
    }

    #[test]
    fn immediate_mutating_imports_create_distinct_no_clobber_backups() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        let original = b"verbosity = \"concise\"\n";
        std::fs::write(&path, original).expect("seed config");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("restrict target permissions");
        }
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        let first_bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
verbosity = "normal"
"#,
            "first.toml",
        )
        .expect("first bundle parses");
        let first = apply_bundle(&first_bundle, &mut store, BundleScope::Global, dir.path())
            .expect("first import");
        let first_backup = first.backup_path.expect("first backup receipt");
        assert_eq!(
            std::fs::read(&first_backup).expect("first backup"),
            original
        );
        let after_first = std::fs::read(&path).expect("target after first import");

        let second_bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
log_level = "trace"
"#,
            "second.toml",
        )
        .expect("second bundle parses");
        let second = apply_bundle(&second_bundle, &mut store, BundleScope::Global, dir.path())
            .expect("second import");
        let second_backup = second.backup_path.expect("second backup receipt");
        assert_ne!(first_backup, second_backup, "backups must never collide");
        assert_eq!(
            std::fs::read(&second_backup).expect("second backup"),
            after_first,
            "second receipt must preserve its exact pre-import document"
        );
        assert_eq!(
            std::fs::read(&first_backup).expect("first backup remains"),
            original,
            "second import must not overwrite the first receipt"
        );
        for backup in [&first_backup, &second_backup] {
            assert!(
                backup
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".bundle-backup-")),
                "unexpected backup name: {}",
                backup.display()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    std::fs::metadata(backup)
                        .expect("backup metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600,
                    "backup must preserve restrictive target permissions"
                );
            }
        }
    }

    #[test]
    fn closed_choice_imports_refuse_before_backup_or_write() {
        for key in ["approval_policy", "sandbox_mode", "verbosity"] {
            for value in [
                toml::Value::String("misspelled-choice".into()),
                toml::Value::Boolean(true),
            ] {
                for existing in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("config.toml");
                    let original = b"# Preserve this document exactly\nverbosity = 'normal'\n";
                    if existing {
                        std::fs::write(&path, original).unwrap();
                    }
                    let mut store = ConfigStore::load(Some(path.clone())).unwrap();
                    let before = toml::to_string(&store.config).unwrap();
                    let mut bundle = sample_bundle();
                    bundle.preferences.entries.clear();
                    bundle.global.entries.clear();
                    bundle.preferences.entries.insert(key.into(), value.clone());
                    // Even an earlier valid candidate edit must not reach the store.
                    bundle
                        .preferences
                        .entries
                        .insert("log_level".into(), toml::Value::String("info".into()));
                    let error = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
                        .expect_err("closed choice must fail before the transaction");
                    assert!(error.to_string().contains(key), "{error:#}");
                    assert_eq!(toml::to_string(&store.config).unwrap(), before);
                    assert_eq!(path.exists(), existing);
                    if existing {
                        assert_eq!(std::fs::read(&path).unwrap(), original);
                    }
                    assert!(
                        std::fs::read_dir(dir.path())
                            .unwrap()
                            .all(|entry| { entry.unwrap().path() == path }),
                        "refusal must not create a backup or another file"
                    );
                }
            }
        }
    }

    #[test]
    fn closed_choice_imports_accept_reader_values_without_revalidating_old_fields() {
        // Trust posture is intentionally machine-bound and rejected by the
        // bundle boundary even when its value is otherwise valid. Exercise
        // the portable closed choice here; authority rejection has its own
        // no-write and export-scrubbing regressions below.
        for value in [" CONCISE ", "normal"] {
            let key = "verbosity";
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            // Loading and unrelated round trips still preserve legacy text; only
            // the value being written gets the shared closed-choice validation.
            let original =
                "# Preserve old fields\napproval_policy = 'legacy-unknown'\nverbosity = 'quiet'\n";
            std::fs::write(&path, original).unwrap();
            let mut store = ConfigStore::load(Some(path.clone())).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            let mut bundle = sample_bundle();
            bundle.preferences.entries.clear();
            bundle.global.entries.clear();
            bundle
                .preferences
                .entries
                .insert(key.into(), toml::Value::String(value.into()));
            let receipt =
                apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path()).unwrap();
            assert_eq!(
                std::fs::read_to_string(receipt.backup_path.unwrap()).unwrap(),
                original
            );
            let reloaded = ConfigStore::load(Some(path)).unwrap();
            assert_eq!(reloaded.config.get_value(key).as_deref(), Some(value));
            assert_eq!(
                reloaded.config.approval_policy.as_deref(),
                Some("legacy-unknown")
            );
        }
    }

    #[test]
    fn non_no_op_import_creates_a_missing_config_without_a_backup() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("missing config loads");
        assert!(!path.exists(), "load must not create the config");

        let receipt = apply_bundle(
            &sample_bundle(),
            &mut store,
            BundleScope::Global,
            dir.path(),
        )
        .expect("import creates config");

        assert!(path.is_file(), "non-no-op import must create the config");
        assert!(
            receipt.backup_path.is_none(),
            "no prior file means no backup"
        );
        let reloaded = ConfigStore::load(Some(path)).expect("created config reloads");
        assert_eq!(reloaded.config.verbosity.as_deref(), Some("concise"));
        assert_eq!(reloaded.config.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn failed_import_removes_a_config_created_during_the_transaction() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("missing config loads");

        let prepared =
            prepare_import(&sample_bundle(), &store, BundleScope::Global).expect("prepare import");
        let error =
            apply_prepared_bundle(prepared, &mut store, |candidate, store, target_written| {
                save_candidate(candidate, store, target_written)?;
                bail!("forced failure after the new document was saved")
            })
            .expect_err("forced post-save failure must roll back");

        assert!(error.to_string().contains("rolled back"), "{error:#}");
        assert!(
            !path.exists(),
            "rollback must remove the newly-created file"
        );
        assert_eq!(
            store.config.verbosity, None,
            "in-memory state also rolls back"
        );
    }

    #[test]
    fn project_scope_never_touches_the_global_document() {
        let mut store = isolated_store();
        let global_before = std::fs::read_to_string(store.path()).expect("global doc");

        let text = r#"
schema_version = 1
kind = "codewhale.portable-config"

[project]
verbosity = "quiet"
"#;
        let bundle = parse_bundle_str(text, "t.toml").expect("bundle");
        assert!(find_rejected_entries(&bundle).is_empty());
        let ws = tempfile::tempdir().expect("ws");
        apply_bundle(&bundle, &mut store, BundleScope::Project, ws.path())
            .expect_err("project entries cannot land in a global-scoped store");
        let global_after = std::fs::read_to_string(store.path()).expect("global doc");
        assert_eq!(global_before, global_after);
    }

    #[cfg(unix)]
    #[test]
    fn failed_apply_rolls_back_to_the_prior_document() {
        let mut store = isolated_store();
        let original = std::fs::read_to_string(store.path()).expect("config");

        // A bundle whose entry fails mid-apply: `providers.deepseek.wire` is a
        // real key path but an invalid value for it, so set_value errors after
        // earlier entries were applied.
        let text = r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
log_level = "debug"

[global]
providers_deepseek_wire = "not-a-real-key-so-this-errors"
"#;
        let _ = text;
        // Simpler deterministic failure: make the target file read-only.
        let text_ok = r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
log_level = "debug"
"#;
        let bundle = parse_bundle_str(text_ok, "t.toml").expect("bundle");
        let path = store.path().to_path_buf();
        // Atomic saves replace the file via rename, so the *directory* must
        // be made unwritable to force the write failure.
        use std::os::unix::fs::PermissionsExt;
        let dir = path.parent().expect("config dir").to_path_buf();
        let mut perms = std::fs::metadata(&dir).expect("dir meta").permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&dir, perms).expect("chmod dir");

        let result = apply_bundle(&bundle, &mut store, BundleScope::Global, Path::new("."));
        // Restore permissions so the tempdir can be cleaned up.
        let mut perms = std::fs::metadata(&dir).expect("dir meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dir, perms).expect("chmod restore");

        assert!(result.is_err(), "apply must fail on a read-only document");
        let restored = std::fs::read_to_string(&path).expect("config after rollback");
        assert_eq!(restored, original, "rollback must preserve the prior bytes");
    }

    #[test]
    fn export_is_deterministic_and_secret_free() {
        let mut store = isolated_store();
        store
            .config
            .set_value("verbosity", "concise")
            .expect("set verbosity");
        store
            .config
            .set_value("default_text_model", "deepseek-v4-pro")
            .expect("set model");
        store.save().expect("save");

        let metadata = BundleMetadata::default();
        let one = export_bundle(&store.config, BundleScope::Global, metadata.clone())
            .and_then(|b| serialize_bundle(&b))
            .expect("export one");
        let two = export_bundle(&store.config, BundleScope::Global, metadata)
            .and_then(|b| serialize_bundle(&b))
            .expect("export two");
        assert_eq!(one, two, "export must be deterministic");

        // No machine-specific absolute paths in the body.
        assert!(!one.contains("/Users/"), "{one}");
        assert!(!one.contains("/home/"), "{one}");
    }

    #[test]
    fn export_preserves_typed_structured_config_and_toml_value_kinds() {
        let config: ConfigToml = toml::from_str(
            r#"
provider = "deepseek"
telemetry = false
retry_count = 3
ratio = 1.25
started_at = 1979-05-27T07:32:00Z
labels = ["alpha", "beta"]

[skills]
registry_url = "https://registry.example/skills.json"
max_install_size_bytes = 12345

[snapshots]
enabled = false
max_age_days = 11

[portable_table]
enabled = true
count = 4
"#,
        )
        .expect("typed config parses");

        let bundle = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("typed export");
        assert!(matches!(
            bundle.preferences.entries.get("skills"),
            Some(toml::Value::Table(_))
        ));
        assert!(matches!(
            bundle.preferences.entries.get("snapshots"),
            Some(toml::Value::Table(_))
        ));
        assert!(matches!(
            bundle.global.entries.get("telemetry"),
            Some(toml::Value::Boolean(false))
        ));
        assert!(matches!(
            bundle.global.entries.get("retry_count"),
            Some(toml::Value::Integer(3))
        ));
        assert!(matches!(
            bundle.global.entries.get("ratio"),
            Some(toml::Value::Float(value)) if *value == 1.25
        ));
        assert!(
            matches!(
                bundle.global.entries.get("started_at"),
                Some(toml::Value::Datetime(_))
            ),
            "{bundle:#?}"
        );

        let dir = tempfile::tempdir().expect("round-trip dir");
        let path = dir.path().join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("fresh store");
        apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect("typed bundle imports");
        let reloaded = ConfigStore::load(Some(path)).expect("typed config reloads");
        let reexported = export_bundle(
            &reloaded.config,
            BundleScope::Global,
            BundleMetadata::default(),
        )
        .expect("round-trip export");
        assert_eq!(
            serialize_bundle(&reexported).expect("serialize round trip"),
            serialize_bundle(&bundle).expect("serialize original"),
            "typed portable config must round-trip without stringification or loss"
        );
        let plan = plan_import(&bundle, &reloaded.config, BundleScope::Global);
        assert!(
            plan.is_no_op(),
            "typed re-import must be idempotent: {plan:?}"
        );
    }

    #[test]
    fn typed_reimport_normalizes_omitted_serde_defaults_before_comparison() {
        let bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences.snapshots]
enabled = false
"#,
            "defaults.toml",
        )
        .expect("bundle with omitted typed default");
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        let mut store = ConfigStore::load(Some(path)).expect("fresh store");
        apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect("first typed import");

        assert_eq!(
            store
                .config
                .snapshots
                .as_ref()
                .expect("snapshots configured")
                .max_age_days,
            7,
            "serde default must be materialized"
        );
        let plan = plan_import(&bundle, &store.config, BundleScope::Global);
        assert!(
            plan.is_no_op(),
            "normalized re-import must be a no-op: {plan:?}"
        );
    }

    #[test]
    fn telemetry_opt_out_round_trips_but_opt_in_consent_never_does() {
        let opted_in = ConfigToml {
            telemetry: Some(true),
            ..ConfigToml::default()
        };
        let exported = export_bundle(&opted_in, BundleScope::Global, BundleMetadata::default())
            .expect("opt-in export is safely omitted");
        assert!(
            !exported.global.entries.contains_key("telemetry"),
            "opt-in consent must not be portable: {exported:?}"
        );

        let opt_in_bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
telemetry = true
"#,
            "telemetry-opt-in.toml",
        )
        .expect("opt-in bundle parses before policy validation");
        let rejected = find_rejected_entries(&opt_in_bundle);
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert!(
            rejected[0].reason.contains("opt-in consent"),
            "{rejected:?}"
        );

        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"concise\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before refusal");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        apply_bundle(&opt_in_bundle, &mut store, BundleScope::Global, dir.path())
            .expect_err("portable opt-in consent must be refused");
        assert_eq!(std::fs::read(&path).expect("config after refusal"), before);
        assert_eq!(store.config.telemetry, None);

        let opt_out_bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
telemetry = false
"#,
            "telemetry-opt-out.toml",
        )
        .expect("opt-out bundle parses");
        assert!(find_rejected_entries(&opt_out_bundle).is_empty());
        apply_bundle(&opt_out_bundle, &mut store, BundleScope::Global, dir.path())
            .expect("portable opt-out applies");
        assert_eq!(store.config.telemetry, Some(false));
        let reloaded = ConfigStore::load(Some(path)).expect("opt-out config reloads");
        let plan = plan_import(&opt_out_bundle, &reloaded.config, BundleScope::Global);
        assert!(
            plan.is_no_op(),
            "opt-out re-import must be idempotent: {plan:?}"
        );
        let reexported = export_bundle(
            &reloaded.config,
            BundleScope::Global,
            BundleMetadata::default(),
        )
        .expect("opt-out re-exports");
        assert_eq!(
            reexported.global.entries.get("telemetry"),
            Some(&toml::Value::Boolean(false))
        );
    }

    #[test]
    fn structured_import_deep_merges_without_erasing_local_authority() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        let api_key_name = ["api", "_key"].concat();
        let api_key_env_name = ["api", "_key_env"].concat();
        let target = format!(
            r#"
provider = "acme_gateway"

[providers.acme_gateway]
kind = "openai-compatible"
base_url = "https://local-only.invalid/v1"
model = "old-model"
{api_key_name} = "opaque-local-api-value"
{api_key_env_name} = "LOCAL_ACME_GATEWAY_KEY"

[providers.acme_gateway.auth]
source = "command"
command = ["/synthetic/local-credential-helper"]

[lsp]
enabled = true
include_warnings = false

[lsp.servers]
rust = ["/synthetic/local-rust-analyzer", "--stdio"]

[lsp.custom.foo]
language_id = "foo-language"
command = "/synthetic/local-foo-lsp"
args = ["--stdio"]

[hook_sinks]
unix_socket_path = "/synthetic/local-codewhale.sock"
"#
        );
        std::fs::write(&path, target).expect("seed local-authority config");
        let mut store = ConfigStore::load(Some(path.clone())).expect("target config loads");
        let bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
provider = "acme_gateway"

[global.providers.acme_gateway]
kind = "openai-compatible"
model = "new-portable-model"

[global.lsp]
enabled = false
include_warnings = true
"#,
            "deep-merge.toml",
        )
        .expect("portable update parses");
        assert!(find_rejected_entries(&bundle).is_empty(), "{bundle:?}");

        let receipt = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect("portable values merge into target");
        assert_eq!(
            receipt.plan.changed,
            ["global.lsp", "global.providers"],
            "{:?}",
            receipt.plan
        );
        assert_eq!(
            receipt.plan.skipped,
            ["global.provider"],
            "{:?}",
            receipt.plan
        );
        assert_eq!(store.config.provider_id(), "acme_gateway");
        let document = config_document(&store.config).expect("merged typed document");
        let acme = document
            .get("providers")
            .and_then(toml::Value::as_table)
            .and_then(|providers| providers.get("acme_gateway"))
            .and_then(toml::Value::as_table)
            .expect("custom provider survives");
        assert_eq!(
            acme.get("model").and_then(toml::Value::as_str),
            Some("new-portable-model")
        );
        assert_eq!(
            acme.get("base_url").and_then(toml::Value::as_str),
            Some("https://local-only.invalid/v1")
        );
        assert_eq!(
            acme.get("api_key").and_then(toml::Value::as_str),
            Some("opaque-local-api-value")
        );
        assert_eq!(
            acme.get("api_key_env").and_then(toml::Value::as_str),
            Some("LOCAL_ACME_GATEWAY_KEY")
        );
        assert_eq!(
            acme.get("auth")
                .and_then(toml::Value::as_table)
                .and_then(|auth| auth.get("command"))
                .and_then(toml::Value::as_array)
                .and_then(|command| command.first())
                .and_then(toml::Value::as_str),
            Some("/synthetic/local-credential-helper")
        );
        let lsp = store.config.lsp.as_ref().expect("LSP config survives");
        assert_eq!(lsp.enabled, Some(false));
        assert_eq!(lsp.include_warnings, Some(true));
        assert!(lsp.servers.as_ref().is_some_and(|servers| {
            servers
                .get("rust")
                .is_some_and(|command| command.first().is_some_and(|part| part.contains("rust")))
        }));
        assert!(
            lsp.custom
                .as_ref()
                .is_some_and(|custom| custom.contains_key("foo"))
        );
        assert_eq!(
            store
                .config
                .hook_sinks
                .as_ref()
                .and_then(|sinks| sinks.unix_socket_path.as_deref()),
            Some(Path::new("/synthetic/local-codewhale.sock"))
        );

        let reloaded = ConfigStore::load(Some(path)).expect("merged config reloads");
        assert_eq!(reloaded.config.provider_id(), "acme_gateway");
        let plan = plan_import(&bundle, &reloaded.config, BundleScope::Global);
        assert!(
            plan.is_no_op(),
            "deep-merged re-import must be idempotent: {plan:?}"
        );
    }

    #[test]
    fn export_recursively_drops_nested_secrets_but_keeps_safe_typed_siblings() {
        let provider_prefix = ["s", "k-"].concat();
        let bearer_prefix = ["Bear", "er "].concat();
        let access_key = ["access", "Token"].concat();
        let dotted_refresh_key = ["service.refresh", "Token"].concat();
        let refresh_key = ["refresh", "Token"].concat();
        let fixture = format!(
            r#"
[tools]
always_load = ["read_file", "{provider_prefix}nested-tool-value-must-not-leak", "write_file"]

[portable]
safe_count = 7
note = "{bearer_prefix}nested-export-value-must-not-leak"
values = ["plain", "{provider_prefix}nested-array-value-must-not-leak"]

[portable.nested]
{access_key} = "nested-export-key-must-not-leak"
"{dotted_refresh_key}" = "nested-dotted-value-must-not-leak"
label = "keep-me"

[[portable.records]]
{refresh_key} = "nested-record-value-must-not-leak"
count = 2

[[portable.records]]
label = "safe-record"
"#
        );
        let config: ConfigToml = toml::from_str(&fixture).expect("secret-bearing config parses");
        let bundle = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("safe export");
        let body = serialize_bundle(&bundle).expect("serialize export");
        for secret in [
            "nested-export-value-must-not-leak",
            "nested-array-value-must-not-leak",
            "nested-tool-value-must-not-leak",
            "nested-export-key-must-not-leak",
            "nested-dotted-value-must-not-leak",
            "nested-record-value-must-not-leak",
        ] {
            assert!(!body.contains(secret), "export leaked {secret}: {body}");
        }
        assert!(body.contains("safe_count = 7"), "{body}");
        assert!(body.contains("label = \"keep-me\""), "{body}");
        assert!(body.contains("label = \"safe-record\""), "{body}");
        assert!(body.contains("values = [\"plain\"]"), "{body}");
        assert!(find_rejected_entries(&bundle).is_empty(), "{bundle:?}");

        let dir = tempfile::tempdir().expect("sanitized import dir");
        let path = dir.path().join("config.toml");
        let mut store = ConfigStore::load(Some(path)).expect("fresh store");
        apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect("sanitized typed arrays re-import");
        assert_eq!(
            store
                .config
                .tools
                .as_ref()
                .expect("tools preserved")
                .always_load
                .as_slice(),
            ["read_file", "write_file"],
            "dropping a secret array element must preserve a valid typed array"
        );
        let plan = plan_import(&bundle, &store.config, BundleScope::Global);
        assert!(
            plan.is_no_op(),
            "sanitized re-import must be idempotent: {plan:?}"
        );
    }

    #[test]
    fn compound_access_and_cookie_fields_are_scrubbed_on_export() {
        let cookie = ["Coo", "kie"].concat();
        let set_cookie = ["Set-", "Cookie"].concat();
        let api_dot = ["api", ".key"].concat();
        let private_dot = ["private", ".key"].concat();
        let access_camel = ["access", "Key"].concat();
        let aws_snake = ["aws", "_access_key"].concat();
        let aws_camel = ["aws", "SecretAccessKey"].concat();
        let fixture = format!(
            r#"
[http_headers]
{cookie} = "opaque-cookie-value"
{set_cookie} = "opaque-set-cookie-value"
X-Safe = "portable-header"

[portable]
"{api_dot}" = "opaque-api-value"
"{private_dot}" = "opaque-private-value"
{access_camel} = "opaque-access-value"
{aws_snake} = "opaque-aws-access-value"
{aws_camel} = "opaque-aws-secret-access-value"
maxTokens = 8192
tokenizer = "bpe"
"#
        );
        let config: ConfigToml = toml::from_str(&fixture).expect("credential-key config parses");
        let bundle = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("credential fields are scrubbed");
        let body = serialize_bundle(&bundle).expect("serialize scrubbed export");
        for forbidden in [
            "opaque-cookie-value",
            "opaque-set-cookie-value",
            "opaque-api-value",
            "opaque-private-value",
            "opaque-access-value",
            "opaque-aws-access-value",
            "opaque-aws-secret-access-value",
            "api.key",
            "private.key",
            "accessKey",
            "aws_access_key",
            "awsSecretAccessKey",
            "Cookie",
            "Set-Cookie",
        ] {
            assert!(
                !body.contains(forbidden),
                "export retained {forbidden}: {body}"
            );
        }
        assert!(body.contains("X-Safe = \"portable-header\""), "{body}");
        assert!(body.contains("maxTokens = 8192"), "{body}");
        assert!(body.contains("tokenizer = \"bpe\""), "{body}");
        assert!(find_rejected_entries(&bundle).is_empty(), "{bundle:?}");
    }

    #[test]
    fn provider_credential_authority_is_rejected_on_import_and_scrubbed_on_export() {
        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"quiet\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before imports");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        let api_key_env_name = ["api", "_key_env"].concat();

        for (name, body) in [
            (
                "auth",
                r#"
[global.providers.xai.auth]
source = "command"
command = ["synthetic-credential-helper"]
"#
                .to_string(),
            ),
            (
                "external",
                r#"
[global.providers.xai.external_credentials]
access = "read_only"
provider = "xai"
source = "grok_cli"
path = "/synthetic/external/auth.json"
consent_version = 1
"#
                .to_string(),
            ),
            (
                "oauth-generation",
                r#"
[global.providers.xai]
oauth_credential_generation = "synthetic-owned-generation.toml"
"#
                .to_string(),
            ),
            (
                "api-key-env",
                format!(
                    r#"
[global.providers.xai]
{api_key_env_name} = "SYNTHETIC_RANDOM_PROVIDER_KEY"
"#
                ),
            ),
        ] {
            let text = format!("schema_version = 1\nkind = \"codewhale.portable-config\"\n{body}");
            let bundle = parse_bundle_str(&text, name).expect("authority bundle parses");
            assert_eq!(find_rejected_entries(&bundle).len(), 1, "{name}");
            let error = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
                .expect_err("authority-bearing import must fail");
            assert!(
                error.to_string().contains("conflicting or rejected"),
                "{name}: {error:#}"
            );
            assert_eq!(std::fs::read(&path).expect("config after refusal"), before);
            assert_eq!(store.config.verbosity.as_deref(), Some("quiet"));
        }

        let fixture = format!(
            r#"
provider = "xai"

[providers.xai]
model = "grok-safe-model"
oauth_credential_generation = "synthetic-owned-generation.toml"
{api_key_env_name} = "SYNTHETIC_RANDOM_PROVIDER_KEY"

[providers.xai.auth]
source = "command"
command = ["synthetic-credential-helper"]

[providers.xai.external_credentials]
access = "read_only"
provider = "xai"
source = "grok_cli"
path = "/synthetic/external/auth.json"
consent_version = 1
"#
        );
        let config: ConfigToml =
            toml::from_str(&fixture).expect("provider authority config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("provider authority is scrubbed");
        let body = serialize_bundle(&exported).expect("serialize provider export");
        for forbidden in [
            "external_credentials",
            "oauth_credential_generation",
            "synthetic-credential-helper",
            "synthetic-owned-generation.toml",
            "SYNTHETIC_RANDOM_PROVIDER_KEY",
            api_key_env_name.as_str(),
            "/synthetic/external/auth.json",
        ] {
            assert!(
                !body.contains(forbidden),
                "export retained {forbidden}: {body}"
            );
        }
        assert!(body.contains("model = \"grok-safe-model\""), "{body}");
        assert!(find_rejected_entries(&exported).is_empty(), "{exported:?}");
    }

    #[test]
    fn machine_local_route_and_path_fields_are_rejected_and_scrubbed_symmetrically() {
        let bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
telemetry_endpoint = "https://synthetic.invalid/telemetry"
mcpConfigPath = "/synthetic/import-mcp.json"

[global.providers.deepseek]
baseUrl = "https://synthetic.invalid/provider/v1"
model = "safe-model"

[global.hook_sinks]
unix_socket_path = "/synthetic/import-codewhale.sock"
"#,
            "machine-local-paths.toml",
        )
        .expect("machine-local bundle parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 4, "{rejected:?}");
        for key in [
            "global.telemetry_endpoint",
            "global.mcpConfigPath",
            "global.providers",
            "global.hook_sinks",
        ] {
            assert!(
                rejected.iter().any(|entry| entry.key == key),
                "{rejected:?}"
            );
        }

        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"quiet\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before import");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        let error = apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect_err("machine-local paths must refuse the entire import");
        assert!(
            error.to_string().contains("conflicting or rejected"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&path).expect("config after refusal"), before);
        assert_eq!(store.config.verbosity.as_deref(), Some("quiet"));

        let config: ConfigToml = toml::from_str(
            r#"
telemetry_endpoint = "https://synthetic.invalid/telemetry"
mcp_config_path = "/synthetic/export-mcp.json"

[providers.deepseek]
base_url = "https://synthetic.invalid/provider/v1"
model = "safe-model"

[hook_sinks]
unix_socket_path = "/synthetic/export-codewhale.sock"
"#,
        )
        .expect("machine-local config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("machine-local paths are scrubbed");
        let body = serialize_bundle(&exported).expect("serialize machine-local export");
        for forbidden in [
            "synthetic.invalid",
            "/synthetic/export-mcp.json",
            "/synthetic/export-codewhale.sock",
            "telemetry_endpoint",
            "mcp_config_path",
            "unix_socket_path",
            "base_url",
        ] {
            assert!(
                !body.contains(forbidden),
                "export retained {forbidden}: {body}"
            );
        }
        assert!(body.contains("model = \"safe-model\""), "{body}");
        assert!(find_rejected_entries(&exported).is_empty(), "{exported:?}");
    }

    #[test]
    fn remaining_local_authority_is_rejected_while_safe_policy_stays_portable() {
        for safe in [
            "databaseUrl",
            "baseUrlTemplate",
            "memoryPathology",
            "sandboxUrlTemplate",
            "skills.registry_url",
            "network.allow",
            "workflow.automatic",
            "fleet.exec.allowed_tools",
        ] {
            assert_eq!(nonportable_path_reason(safe), None, "must preserve {safe}");
        }

        let bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
instructions = "/synthetic/import-instructions.md"
project_instruction_imports = "all"
projectInstructionImports = "all"
sandbox_backend = "synthetic-local-backend"
sandboxUrl = "http://127.0.0.1:47891"
bwrapRoRoots = ["/synthetic/import-ro"]
bwrap_dev_roots = ["/synthetic/import-dev"]
skills_dir = "/synthetic/import-skills"
memoryPath = "/synthetic/import-memory.md"
mcpOauthCallbackUrl = "http://127.0.0.1:47892/callback"
mcp_oauth_callback_port = 47892
notes_path = "/synthetic/import-notes.md"

[global.runtime_api]
bind = "127.0.0.1:47893"

[global.auto_review]
allow = ["synthetic-shell-action"]

[global.tools]
always_load = ["read_file"]
plugin_dir = "/synthetic/import-plugins"

[global.tools.overrides.shell]
command = "/synthetic/import-tool-override"

[global.update]
channel = "stable"
update_uri = "file:///synthetic/import-update"

[global.notifications]
enabled = true
sound_file = "/synthetic/import-sound.wav"

[global.speech]
enabled = true
output_dir = "/synthetic/import-speech"

[global.skills]
registry_url = "https://registry.example/skills.json"

[global.network]
default = "prompt"
allow = ["registry.example"]

[global.workflow]
automatic = false

[global.fleet.exec]
allowed_tools = ["read_file"]
"#,
            "remaining-local-authority.toml",
        )
        .expect("remaining authority bundle parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 18, "{rejected:?}");
        for safe in [
            "global.skills",
            "global.network",
            "global.workflow",
            "global.fleet",
        ] {
            assert!(
                rejected.iter().all(|entry| entry.key != safe),
                "safe entry {safe} was rejected: {rejected:?}"
            );
        }

        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"concise\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before import");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect_err("remaining local authority import must fail");
        assert_eq!(std::fs::read(&path).expect("config after refusal"), before);
        assert_eq!(store.config.verbosity.as_deref(), Some("concise"));

        let config: ConfigToml = toml::from_str(
            r#"
instructions = "/synthetic/export-instructions.md"
project_instruction_imports = "all"
projectInstructionImports = "all"
sandbox_backend = "synthetic-local-backend"
sandboxUrl = "http://127.0.0.1:47894"
bwrapRoRoots = ["/synthetic/export-ro"]
bwrap_dev_roots = ["/synthetic/export-dev"]
skills_dir = "/synthetic/export-skills"
memoryPath = "/synthetic/export-memory.md"
mcpOauthCallbackUrl = "http://127.0.0.1:47895/callback"
mcp_oauth_callback_port = 47895
notes_path = "/synthetic/export-notes.md"

[runtime_api]
bind = "127.0.0.1:47896"

[auto_review]
allow = ["synthetic-shell-action"]

[tools]
always_load = ["read_file"]
plugin_dir = "/synthetic/export-plugins"

[tools.overrides.shell]
command = "/synthetic/export-tool-override"

[update]
channel = "stable"
update_uri = "file:///synthetic/export-update"

[notifications]
enabled = true
sound_file = "/synthetic/export-sound.wav"

[speech]
enabled = true
output_dir = "/synthetic/export-speech"

[skills]
registry_url = "https://registry.example/skills.json"
max_install_size_bytes = 12345

[network]
default = "prompt"
allow = ["registry.example"]

[workflow]
automatic = false

[fleet.exec]
allowed_tools = ["read_file"]
"#,
        )
        .expect("remaining authority config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("remaining authority is scrubbed");
        for key in [
            "instructions",
            "project_instruction_imports",
            "projectInstructionImports",
            "sandbox_backend",
            "sandboxUrl",
            "bwrapRoRoots",
            "bwrap_dev_roots",
            "skills_dir",
            "memoryPath",
            "mcpOauthCallbackUrl",
            "mcp_oauth_callback_port",
            "notes_path",
            "runtime_api",
            "auto_review",
        ] {
            assert!(!exported.global.entries.contains_key(key), "retained {key}");
        }
        let body = serialize_bundle(&exported).expect("serialize safe policy export");
        for forbidden in [
            "/synthetic/export-instructions.md",
            "/synthetic/export-ro",
            "/synthetic/export-dev",
            "/synthetic/export-skills",
            "/synthetic/export-memory.md",
            "/synthetic/export-notes.md",
            "/synthetic/export-sound.wav",
            "/synthetic/export-speech",
            "file:///synthetic/export-update",
            "127.0.0.1:47896",
        ] {
            assert!(
                !body.contains(forbidden),
                "export retained {forbidden}: {body}"
            );
        }
        for safe in [
            "https://registry.example/skills.json",
            "registry.example",
            "automatic = false",
            "allowed_tools = [\"read_file\"]",
            "channel = \"stable\"",
            "enabled = true",
        ] {
            assert!(body.contains(safe), "export lost {safe}: {body}");
        }

        // ToolsToml currently ignores these legacy fields while parsing, so
        // exercise the recursive export sanitizer directly as defense in depth.
        let raw: toml::Value = toml::from_str(
            r#"
[tools]
always_load = ["read_file"]
plugin_dir = "/synthetic/direct-plugin-dir"

[tools.overrides.shell]
command = "/synthetic/direct-tool-override"
"#,
        )
        .expect("raw tools table parses");
        let scrubbed =
            sanitize_export_value("tools", raw.get("tools").expect("raw tools table exists"))
                .expect("safe tools sibling remains");
        let scrubbed = scrubbed.to_string();
        assert!(scrubbed.contains("read_file"), "{scrubbed}");
        assert!(!scrubbed.contains("plugin_dir"), "{scrubbed}");
        assert!(!scrubbed.contains("overrides"), "{scrubbed}");
        assert!(find_rejected_entries(&exported).is_empty(), "{exported:?}");
    }

    #[test]
    fn deep_nesting_fails_closed_for_rejection_and_sanitize() {
        fn deep_toml(depth: usize) -> toml::Value {
            let mut value = toml::Value::String("leaf".to_string());
            for _ in 0..depth {
                let mut map = toml::map::Map::new();
                map.insert("t".to_string(), value);
                value = toml::Value::Table(map);
            }
            value
        }

        let deep = deep_toml(70);
        let reason = value_rejection_reason("t", &deep).expect("over-deep value must be rejected");
        assert!(reason.contains("levels deep"), "{reason}");
        assert!(
            sanitize_export_value("t", &deep)
                .is_some_and(|scrubbed| !scrubbed.to_string().contains("leaf")),
            "over-deep branch must be omitted, not exported"
        );
    }

    #[test]
    fn lsp_executable_authority_is_rejected_while_inert_settings_remain_portable() {
        let config: ConfigToml = toml::from_str(
            r#"
[lsp]
enabled = true
poll_after_edit_ms = 250
max_diagnostics_per_file = 12
include_warnings = true

[lsp.servers]
rust = ["/synthetic/rust-analyzer", "--stdio"]

[lsp.custom.foo]
language_id = "foo-language"
command = "/synthetic/foo-language-server"
args = ["--stdio", "--synthetic"]
"#,
        )
        .expect("LSP config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("LSP executable authority is scrubbed");
        let body = serialize_bundle(&exported).expect("serialize LSP export");
        for forbidden in [
            "/synthetic/rust-analyzer",
            "/synthetic/foo-language-server",
            "foo-language",
            "--stdio",
            "--synthetic",
        ] {
            assert!(
                !body.contains(forbidden),
                "export retained {forbidden}: {body}"
            );
        }
        for inert in [
            "enabled = true",
            "poll_after_edit_ms = 250",
            "max_diagnostics_per_file = 12",
            "include_warnings = true",
        ] {
            assert!(body.contains(inert), "export lost {inert}: {body}");
        }
        assert!(find_rejected_entries(&exported).is_empty(), "{exported:?}");

        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"concise\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before imports");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        for (name, body) in [
            (
                "servers",
                r#"
[global.lsp]
enabled = true

[global.lsp.servers]
rust = ["/synthetic/import-rust-analyzer", "--stdio"]
"#,
            ),
            (
                "custom",
                r#"
[global.lsp]
include_warnings = true

[global.lsp.custom.foo]
language_id = "foo-language"
command = "/synthetic/import-foo-server"
args = ["--stdio"]
"#,
            ),
        ] {
            let text = format!("schema_version = 1\nkind = \"codewhale.portable-config\"\n{body}");
            let bundle = parse_bundle_str(&text, name).expect("LSP bundle parses");
            assert_eq!(find_rejected_entries(&bundle).len(), 1, "{name}");
            apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
                .expect_err("LSP executable authority import must fail");
            assert_eq!(std::fs::read(&path).expect("config after refusal"), before);
            assert_eq!(store.config.verbosity.as_deref(), Some("concise"));
        }
    }

    #[test]
    fn trust_posture_and_local_endpoint_keys_are_rejected_on_import() {
        let bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[preferences]
yolo = true
allow_shell = true
sandbox_mode = "danger-full-access"
approval_policy = "never"
sandboxNetworkAccess = true
log_level = "debug"

[preferences.lifecycle_outbox]
webhook_url = "https://collector.example/collect"

[preferences.extension_host]
node = "/bin/echo"

[preferences.control_socket]
enabled = true

[global]
sandbox_read_denylist_exempt = ["/synthetic/exempt"]
"#,
            "posture.toml",
        )
        .expect("posture bundle parses");
        let rejected = find_rejected_entries(&bundle);
        let keys: Vec<&str> = rejected.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(rejected.len(), 9, "{rejected:?}");
        assert!(!keys.contains(&"preferences.log_level"), "{keys:?}");
        let rendered = format!("{rejected:?}");
        assert!(!rendered.contains("collector.example"), "{rendered}");
        assert!(!rendered.contains("/bin/echo"), "{rendered}");

        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"quiet\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before import");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect_err("trust posture import must fail");
        assert_eq!(std::fs::read(&path).expect("config after refusal"), before);
    }

    #[test]
    fn portable_export_drops_webhooks_local_executables_and_denylist_paths() {
        let config: ConfigToml = toml::from_str(
            r#"
model = "safe-model"
approval_policy = "never"
sandbox_mode = "danger-full-access"
yolo = true
sandbox_denied_read_paths = ["/synthetic/private"]
sandbox_read_denylist_exempt = ["/synthetic/exempt"]

[lifecycle_outbox]
path = "/synthetic/outbox.jsonl"
webhook_url = "https://hooks.slack.com/services/T0SYNTH/B0SYNTH/synthetichookvalue"

[extension_host]
node = "/synthetic/bin/node"

[control_socket]
enabled = true
"#,
        )
        .expect("config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("export scrubs posture");
        let body = serialize_bundle(&exported).expect("serialize");
        for forbidden in [
            "hooks.slack.com",
            "synthetichookvalue",
            "/synthetic/outbox.jsonl",
            "/synthetic/bin/node",
            "/synthetic/private",
            "/synthetic/exempt",
            "danger-full-access",
            "approval_policy",
            "yolo",
            "control_socket",
        ] {
            assert!(
                !body.contains(forbidden),
                "export retained {forbidden}: {body}"
            );
        }
        assert!(body.contains("model = \"safe-model\""), "{body}");
    }

    #[test]
    fn urls_carrying_credentials_are_credential_shaped() {
        for url in [
            "https://user:pass@registry.example/index.json",
            "https://registry.example/index.json?token=synthetic",
            "https://registry.example/index.json?access_token=synthetic",
            "https://hooks.slack.com/services/T0SYNTH/B0SYNTH/synthetic",
            "https://discord.com/api/webhooks/1/synthetic",
            "https://tenant.webhook.office.com/webhookb2/synthetic",
        ] {
            assert!(string_secret_reason(url).is_some(), "must reject {url}");
        }
        for url in [
            "https://registry.example/skills.json",
            "https://registry.example/search?q=codewhale&page=2",
            "https://registry.example/search?q=a+b%20c",
            "https://hooks.slack.com/",
        ] {
            assert_eq!(string_secret_reason(url), None, "must preserve {url}");
        }
    }

    #[test]
    fn machine_bound_authority_subtrees_are_rejected_and_never_exported() {
        let config: ConfigToml = toml::from_str(
            r#"
managed_config_path = "/synthetic/managed-config.toml"
requirements_path = "/synthetic/requirements.md"

[workspace]
root = "/synthetic/workspace-root"
trust = "trusted"
allow_shell = true

[projects."/synthetic/project-root"]
trust = "trusted"
allow_shell = true

[hooks.session_start]
command = "/synthetic/session-start"

[portable.workspace]
label = "safe-nested-workspace-label"

[portable.projects]
label = "safe-nested-projects-label"

[portable.hooks]
label = "safe-nested-hooks-label"
"#,
        )
        .expect("machine-bound authority config parses");
        let exported = export_bundle(&config, BundleScope::Global, BundleMetadata::default())
            .expect("machine-bound authority is scrubbed");
        assert!(!exported.global.entries.contains_key("workspace"));
        assert!(!exported.global.entries.contains_key("projects"));
        assert!(!exported.global.entries.contains_key("hooks"));
        assert!(!exported.global.entries.contains_key("managed_config_path"));
        assert!(!exported.global.entries.contains_key("requirements_path"));
        let body = serialize_bundle(&exported).expect("serialize machine-bound export");
        for path in [
            "/synthetic/workspace-root",
            "/synthetic/project-root",
            "/synthetic/session-start",
            "/synthetic/managed-config.toml",
            "/synthetic/requirements.md",
        ] {
            assert!(!body.contains(path), "export retained {path}: {body}");
        }
        assert!(body.contains("safe-nested-workspace-label"), "{body}");
        assert!(body.contains("safe-nested-projects-label"), "{body}");
        assert!(body.contains("safe-nested-hooks-label"), "{body}");

        let bundle = parse_bundle_str(
            r#"
schema_version = 1
kind = "codewhale.portable-config"

[global]
managed_config_path = "/synthetic/import-managed-config.toml"
requirements_path = "/synthetic/import-requirements.md"

[global.workspace]
root = "/synthetic/import-workspace"
trust = "trusted"
allow_shell = true

[global.projects."/synthetic/import-project"]
trust = "trusted"
allow_shell = true

[global.hooks.session_start]
command = "/synthetic/import-session-start"

[global.portable.workspace]
label = "safe-nested-workspace-label"
"#,
            "machine-bound-authority.toml",
        )
        .expect("machine-bound authority bundle parses");
        let rejected = find_rejected_entries(&bundle);
        assert_eq!(rejected.len(), 5, "{rejected:?}");
        assert!(rejected.iter().any(|entry| entry.key == "global.workspace"));
        assert!(rejected.iter().any(|entry| entry.key == "global.projects"));
        assert!(rejected.iter().any(|entry| entry.key == "global.hooks"));
        assert!(
            rejected
                .iter()
                .any(|entry| entry.key == "global.managed_config_path")
        );
        assert!(
            rejected
                .iter()
                .any(|entry| entry.key == "global.requirements_path")
        );

        let dir = tempfile::tempdir().expect("config dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "verbosity = \"concise\"\n").expect("seed config");
        let before = std::fs::read(&path).expect("config before import");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store loads");
        apply_bundle(&bundle, &mut store, BundleScope::Global, dir.path())
            .expect_err("machine-bound authority import must fail");
        assert_eq!(std::fs::read(path).expect("config after refusal"), before);
        assert_eq!(store.config.verbosity.as_deref(), Some("concise"));
    }

    #[test]
    fn bundle_scope_must_match_the_target_for_import_and_export() {
        let dir = tempfile::tempdir().expect("config dir");
        let global_path = dir.path().join("config.toml");
        let error = validate_scope_target(BundleScope::Project, &global_path)
            .expect_err("global path cannot masquerade as project scope");
        assert!(error.to_string().contains("workspace config"), "{error:#}");
        validate_scope_target(BundleScope::Global, &global_path)
            .expect("global config accepts global scope");

        let project_path = dir.path().join(".codewhale").join("config.toml");
        std::fs::create_dir(dir.path().join(".git")).expect("checkout marker");
        validate_scope_target(BundleScope::Project, &project_path)
            .expect("workspace config accepts project scope");
        let error = validate_scope_target(BundleScope::Global, &project_path)
            .expect_err("workspace path cannot masquerade as global scope");
        assert!(error.to_string().contains("--project"), "{error:#}");

        let mut store = ConfigStore::load(Some(project_path)).expect("workspace store loads");
        let import = ImportArgs {
            source: dir
                .path()
                .join("must-not-be-read.toml")
                .display()
                .to_string(),
            dry_run: true,
            yes: true,
            project: false,
        };
        let error = run_import(&import, &mut store, dir.path())
            .expect_err("global import must refuse a workspace config before reading input");
        assert!(error.to_string().contains("--project"), "{error:#}");

        let output = dir.path().join("must-not-be-written.toml");
        let export = ExportArgs {
            portable: true,
            project: false,
            out: Some(output.clone()),
        };
        let error = run_export(&export, &store)
            .expect_err("global export must refuse a workspace config before writing output");
        assert!(error.to_string().contains("--project"), "{error:#}");
        assert!(!output.exists(), "refused export must not create an output");
    }

    #[test]
    fn exported_bundle_reimports_cleanly() {
        let mut store = isolated_store();
        store.config.set_value("verbosity", "concise").expect("set");
        store.save().expect("save");

        let bundle = export_bundle(
            &store.config,
            BundleScope::Global,
            BundleMetadata::default(),
        )
        .expect("export");
        let rejected = find_rejected_entries(&bundle);
        assert!(
            rejected.is_empty(),
            "export must be secret-free: {rejected:?}"
        );

        let plan = plan_import(&bundle, &store.config, BundleScope::Global);
        assert!(
            plan.rejected.is_empty() && plan.conflicting.is_empty(),
            "own export must not trip rejection: {plan:?}"
        );
    }

    #[test]
    fn http_non_loopback_fetch_is_refused_without_network_access() {
        let err = fetch_bundle("http://example.com/bundle.toml")
            .expect_err("plain http to a public host must be refused");
        assert!(err.to_string().contains("loopback"), "{err:#}");
        assert!(!err.to_string().contains("example.com"), "{err:#}");
    }

    #[test]
    fn redirect_to_non_loopback_http_is_refused_without_leaking_location() {
        let secret = "location-secret-must-not-leak";
        let location = format!("http://example.com/internal?token={secret}");
        let response = http_response("302 Found", &[("Location", location.as_str())], b"");
        let (url, server) = spawn_bundle_http_server(vec![response]);

        let error = fetch_bundle(&url).expect_err("redirect target must be revalidated");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("loopback"), "{rendered}");
        assert!(!rendered.contains("example.com"), "{rendered}");
        assert!(!rendered.contains(secret), "{rendered}");
        assert_eq!(server.join().expect("server joins"), 1);
    }

    #[test]
    fn https_redirect_cannot_downgrade_to_loopback_http() {
        let secret = "downgrade-secret-must-not-leak";
        let target = reqwest::Url::parse(&format!("http://127.0.0.1/internal?token={secret}"))
            .expect("test target URL");

        let error = validate_bundle_redirect("https", &target)
            .expect_err("HTTPS redirect must not downgrade to loopback HTTP");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("scheme"), "{rendered}");
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(!rendered.contains("127.0.0.1"), "{rendered}");
    }

    #[test]
    fn relative_loopback_redirect_fetches_bundle() {
        let redirect = http_response("302 Found", &[("Location", "/bundle.toml")], b"");
        let body = VALID_TOML.as_bytes();
        let success = http_response("200 OK", &[("Content-Type", "text/plain")], body);
        let (url, server) = spawn_bundle_http_server(vec![redirect, success]);

        let fetched = fetch_bundle(&url).expect("relative redirect remains allowed");
        assert_eq!(fetched, body);
        assert_eq!(server.join().expect("server joins"), 2);
    }

    #[test]
    fn redirect_limit_is_enforced_before_a_sixth_hop() {
        let responses = (0..=MAX_REDIRECTS)
            .map(|hop| {
                let location = format!("/hop-{}", hop + 1);
                http_response("302 Found", &[("Location", location.as_str())], b"")
            })
            .collect();
        let (url, server) = spawn_bundle_http_server(responses);

        let error = fetch_bundle(&url).expect_err("sixth redirect must be refused");
        assert!(error.to_string().contains("five-redirect"), "{error:#}");
        assert_eq!(server.join().expect("server joins"), MAX_REDIRECTS + 1);
    }

    #[test]
    fn bundle_url_credentials_are_rejected_without_echoing_them() {
        let secret = "credential-secret-must-not-leak";
        let error = fetch_bundle(&format!("https://user:{secret}@example.com/bundle.toml"))
            .expect_err("URL userinfo must be refused");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("credentials"), "{rendered}");
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(!rendered.contains("example.com"), "{rendered}");
    }

    #[test]
    fn unsupported_schemes_are_refused() {
        let err = fetch_bundle("file:///etc/passwd").expect_err("file scheme refused");
        assert!(err.to_string().contains("scheme"), "{err:#}");
    }

    #[test]
    fn headless_import_requires_yes() {
        let plan = ImportPlan {
            added: vec!["preferences.x".to_string()],
            ..ImportPlan::default()
        };
        // The test harness runs headless (no tty), so consent without --yes
        // must refuse before any prompt.
        let err = require_import_consent(false, &plan).expect_err("headless needs --yes");
        assert!(err.to_string().contains("--yes"), "{err:#}");
        require_import_consent(true, &plan).expect("--yes short-circuits consent");
    }

    #[test]
    fn bounded_paths_refuse_traversal_and_absolute_escapes() {
        let base = tempfile::tempdir().expect("base");
        let err =
            resolve_bounded_path(base.path(), "../escape.toml").expect_err("traversal refused");
        assert!(
            err.to_string().contains("escapes") || err.to_string().contains("absolute"),
            "{err:#}"
        );
        let absolute = base.path().join("absolute.toml");
        let err = resolve_bounded_path(base.path(), absolute.to_string_lossy().as_ref())
            .expect_err("absolute refused");
        assert!(err.to_string().contains("absolute"), "{err:#}");
        let ok = resolve_bounded_path(base.path(), "nested/thing.toml").expect("inside ok");
        assert!(ok.starts_with(base.path()));
    }

    // -- helpers ------------------------------------------------------------

    fn http_response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )
        .into_bytes();
        for (name, value) in headers {
            response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        response.extend_from_slice(b"\r\n");
        response.extend_from_slice(body);
        response
    }

    fn spawn_bundle_http_server(
        responses: Vec<Vec<u8>>,
    ) -> (String, std::thread::JoinHandle<usize>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind HTTP fixture");
        let address = listener.local_addr().expect("fixture address");
        let handle = std::thread::spawn(move || {
            let mut served = 0usize;
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept fixture request");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .expect("fixture read timeout");
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).expect("read fixture request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                stream.write_all(&response).expect("write fixture response");
                served += 1;
            }
            served
        });
        (format!("http://{address}/start"), handle)
    }

    /// A store over a config file that outlives the helper: the tempdir is
    /// leaked deliberately (tests are short-lived; explicit cleanup would need
    /// to thread the guard through every call site).
    fn isolated_store() -> ConfigStore {
        // Serialize with every other env-mutating test in this crate: a private
        // lock here would still race `ScopedEnvVar` users (observed as a flaky
        // credentials-dir failure in `api_key_config_failure_restores_*`).
        let _guard = crate::tests::env_lock();

        let dir = {
            // TempDir::keep() is the non-deprecated ownership transfer.
            let temp = tempfile::TempDir::new().expect("tempdir");
            temp.keep()
        };
        let unique = dir.join("home").join(std::process::id().to_string());
        std::fs::create_dir_all(&unique).expect("unique home");
        // SAFETY: test-only env mutation, serialized by the lock above.
        unsafe { std::env::set_var("CODEWHALE_HOME", &unique) };
        let path = unique.join("config.toml");
        std::fs::write(&path, "# test config\n").expect("seed config file");
        ConfigStore::load(Some(path)).expect("store loads")
    }
}
