use crate::config_backup::{merge_and_preserve_comments, note_legacy_root_file_migration};
use crate::config_document::replace_config_document_if_unchanged;
use crate::config_toml::ConfigToml;
use crate::external_credentials::quote_os_path;
use crate::path_io::{checked_path_exists, normalize_config_file_path, read_checked_config_file};
use crate::permissions::{
    PermissionsToml, append_permission_rule, checked_permissions_path_for_config_path,
    load_sibling_permissions, parse_generated_permissions, parse_permissions_document,
    read_permissions_state, write_permissions_atomic,
};
use crate::state_dirs::resolve_config_path;
use crate::{catalog, config_document, legacy_root};
use anyhow::Context as _;
use anyhow::{Result, bail};
use codewhale_execpolicy::{ExecPolicyEngine, PermissionAction, ToolAskRule};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ConfigStore {
    pub(crate) path: PathBuf,
    pub config: ConfigToml,
    pub(crate) permissions: PermissionsToml,
    /// Original file text, retained so [`save`](Self::save) can merge
    /// comments back after serialisation.
    pub(crate) original_raw: Option<String>,
    /// What loading moved in memory from legacy top-level keys (#6394).
    pub(crate) legacy_root: legacy_root::LegacyRootMigration,
}

/// Parse a [`ConfigToml`] on a dedicated thread with an explicit stack size.
///
/// `ConfigToml` nests the per-provider tables, fleet trust policy, and every
/// typed sub-table in one struct, and the monomorphized toml/serde
/// deserializer frames for a struct this large overflow the 2 MiB default
/// stack of libtest and tokio worker threads in debug builds (the same
/// hazard the TUI fixed for its `ConfigFile`; reproduced as the #5585 stack
/// overflow through the guided-setup save path). Every production
/// `ConfigToml` parse goes through here so config-store loads stay safe
/// regardless of the calling thread's stack budget.
pub(crate) fn parse_config_toml_str(contents: &str) -> Result<ConfigToml, toml::de::Error> {
    parse_config_toml_with_receipt(contents).map(|(config, _)| config)
}

/// [`parse_config_toml_str`] plus the receipt of the legacy top-level keys it
/// moved in memory (#6394).
pub(crate) fn parse_config_toml_with_receipt(
    contents: &str,
) -> Result<(ConfigToml, legacy_root::LegacyRootMigration), toml::de::Error> {
    std::thread::scope(|scope| {
        match std::thread::Builder::new()
            .name("config-toml-parse".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, || parse_config_toml_canonical(contents))
        {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            // Spawning can only fail under resource exhaustion; parsing on
            // the caller's stack is still the best remaining option.
            Err(_) => parse_config_toml_canonical(contents),
        }
    })
}

pub(crate) fn parse_config_toml_canonical(
    contents: &str,
) -> Result<(ConfigToml, legacy_root::LegacyRootMigration), toml::de::Error> {
    let (text, receipt) = legacy_root::canonicalize_text(contents)?;
    let config = toml::from_str::<ConfigToml>(&text)?;
    Ok((config, receipt))
}

/// Where a TOML error sits: `line L, column C` in `source` (the exact text
/// that was parsed), plus the key path when the deserializer recorded one.
///
/// Never the error's message or source snippet: both can quote the offending
/// value (`invalid type: string "sk-…", expected a boolean`), and config and
/// permissions files hold credentials. Pass `None` for `source` when the
/// span refers to text the user did not write.
#[must_use]
pub fn toml_error_location(source: Option<&str>, err: &toml::de::Error) -> String {
    let position = source.zip(err.span()).map(|(source, span)| {
        let mut start = span.start.min(source.len());
        while !source.is_char_boundary(start) {
            start -= 1;
        }
        let before = &source[..start];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        format!("line {line}, column {column}")
    });
    // Without its input, `toml` renders exactly `{message}\n` followed by
    // ``in `{keys}`\n`` when it recorded a key path; take only the latter.
    let mut detached = err.clone();
    detached.set_input(None);
    let rendered = detached.to_string();
    let key_path = rendered
        .strip_prefix(err.message())
        .and_then(|rest| rest.strip_prefix("\nin `"))
        .and_then(|rest| rest.strip_suffix("`\n"))
        .filter(|keys| !keys.is_empty())
        .map(|keys| format!("in `{}`", codewhale_secrets::redact::redact_secrets(keys)));
    match (position, key_path) {
        (Some(position), Some(keys)) => format!("{position}, {keys}"),
        (Some(position), None) => position,
        (None, Some(keys)) => keys,
        (None, None) => "position unknown".to_string(),
    }
}

/// [`toml_error_location`] for an error from [`parse_config_toml_with_receipt`].
/// A file with legacy top-level keys is parsed from a rewritten copy whose
/// offsets name nothing the user wrote, so only the key path is reported.
pub(crate) fn config_toml_error_location(raw: &str, err: &toml::de::Error) -> String {
    let rewritten = matches!(
        legacy_root::canonicalize_text(raw),
        Ok((std::borrow::Cow::Owned(_), _))
    );
    toml_error_location((!rewritten).then_some(raw), err)
}

/// Parse any `config.toml`-shaped document into [`ConfigToml`], moving legacy
/// top-level `base_url` / `api_key` into their provider tables first. Every
/// caller outside this crate (bundle import, tests) parses through here so a
/// top-level key can never land in `extras`.
pub fn parse_config_toml(contents: &str) -> Result<ConfigToml, toml::de::Error> {
    parse_config_toml_str(contents)
}

impl ConfigStore {
    /// The validated file snapshot captured by the last load or successful
    /// save. This read-only view preserves literal provider identities that a
    /// typed serialization may normalize; it excludes unsaved in-memory edits.
    #[must_use]
    pub fn original_body(&self) -> Option<&str> {
        self.original_raw.as_deref()
    }

    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = resolve_config_path(path)?;
        let (config, original_raw, legacy_root) = if checked_path_exists(&path)? {
            let raw = read_checked_config_file(&path)?;
            let (mut parsed, receipt) = parse_config_toml_with_receipt(&raw).map_err(|err| {
                anyhow::anyhow!(
                    "failed to parse config at {} ({}); file contents were omitted",
                    quote_os_path(&path),
                    config_toml_error_location(&raw, &err)
                )
            })?;
            let raw_document: toml::Value = toml::from_str(&raw).map_err(|err| {
                anyhow::anyhow!(
                    "failed to parse config at {} ({}); file contents were omitted",
                    quote_os_path(&path),
                    toml_error_location(Some(&raw), &err)
                )
            })?;
            if let Some(provider_id) = raw_document.get("provider").and_then(toml::Value::as_str) {
                parsed
                    .bind_persisted_provider_id(provider_id)
                    .with_context(|| {
                        format!("failed to parse config at {}", quote_os_path(&path))
                    })?;
            }
            (parsed, Some(raw), receipt)
        } else {
            (
                ConfigToml::default(),
                None,
                legacy_root::LegacyRootMigration::default(),
            )
        };
        let permissions = load_sibling_permissions(&path)?;

        Ok(Self {
            path,
            config,
            permissions,
            original_raw,
            legacy_root,
        })
    }

    /// What loading moved in memory from legacy top-level keys. Loading never
    /// rewrites the file; the next save does, keeping any conflict in place.
    #[must_use]
    pub fn legacy_root_migration(&self) -> &legacy_root::LegacyRootMigration {
        &self.legacy_root
    }

    /// Render the exact body [`save`](Self::save) would write: the serialized
    /// config with comments and disabled keys from the originally-loaded file
    /// merged back in. Exposed so setup flows can stage this body into a
    /// `persistence::SetupTransaction` alongside sibling files and keep the
    /// comment-preserving write atomic with the rest of the transaction.
    pub fn rendered_body(&self) -> Result<String> {
        catalog::configured::validate_configured_models(
            self.config.custom_models.as_deref().unwrap_or_default(),
        )?;
        let mut serialized =
            toml::to_string_pretty(&self.config).context("failed to serialize config")?;
        let provider_id = self.config.provider_id();
        if provider_id != self.config.provider.as_str() {
            let mut document = serialized
                .parse::<toml_edit::DocumentMut>()
                .context("failed to edit serialized config")?;
            document["provider"] = toml_edit::value(provider_id);
            serialized = document.to_string();
        }
        if let Some(ref original_raw) = self.original_raw {
            // Comments come from the original with legacy top-level keys
            // already moved (#6394): a comment above a moved key stays in
            // place instead of vanishing with the key.
            let decor_source = legacy_root::migrated_document_text(original_raw);
            let merged = merge_and_preserve_comments(
                &serialized,
                decor_source.as_deref().unwrap_or(original_raw),
            )
            .with_context(|| {
                format!(
                    "cannot safely preserve config at {}; reload it and retry instead of replacing an unmergeable snapshot",
                    quote_os_path(&self.path)
                )
            })?;
            // The typed body has no top-level keys. A conflicting pair the
            // user never touched goes back exactly as it was on disk.
            let mut document = merged
                .parse::<toml_edit::DocumentMut>()
                .context("failed to edit serialized config")?;
            if legacy_root::restore_conflicts(&mut document, original_raw) {
                return Ok(document.to_string());
            }
            Ok(merged)
        } else {
            Ok(serialized)
        }
    }

    pub fn save(&mut self) -> Result<()> {
        let path = normalize_config_file_path(self.path.clone())?;
        let body = self.rendered_body()?;
        if let Some(original_raw) = self.original_raw.as_deref() {
            note_legacy_root_file_migration(&path, original_raw)?;
        }
        replace_config_document_if_unchanged(&path, self.original_raw.as_deref(), &body)?;
        self.original_raw = Some(body);
        Ok(())
    }

    /// Refresh the typed value and byte snapshot after a targeted writer used
    /// the shared config lock. This keeps a long-lived command process from
    /// treating its own successful mutation as an external stale conflict.
    pub fn reload(&mut self) -> Result<()> {
        *self = Self::load(Some(self.path.clone()))?;
        Ok(())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionsToml {
        &self.permissions
    }

    #[must_use]
    pub fn permissions_path(&self) -> PathBuf {
        checked_permissions_path_for_config_path(&self.path)
            .expect("ConfigStore path is validated before construction")
    }

    #[must_use]
    pub fn exec_policy_engine(&self) -> ExecPolicyEngine {
        if self.permissions.is_empty() {
            ExecPolicyEngine::new(Vec::new(), Vec::new())
        } else {
            ExecPolicyEngine::with_rulesets(vec![self.permissions.ruleset()])
        }
    }

    /// Atomically append ask-only permission rules to the sibling
    /// `permissions.toml` file.
    ///
    /// Existing comments and formatting are preserved. Exact duplicate rules
    /// are ignored, and the in-memory permissions snapshot is refreshed after
    /// a successful write.
    pub fn append_ask_rules(&mut self, rules: &[ToolAskRule]) -> Result<usize> {
        self.append_permission_rules(rules, PermissionAction::Ask)
    }

    /// Atomically append exact, repo-scoped allow rules to the sibling
    /// `permissions.toml` file.
    ///
    /// The caller is responsible for deciding which tool calls are eligible;
    /// this boundary rejects broad or incorrectly typed records so a UI bug
    /// cannot persist an unscoped allow grant.
    pub fn append_allow_rules(&mut self, rules: &[ToolAskRule]) -> Result<usize> {
        for rule in rules {
            if rule.action != PermissionAction::Allow {
                bail!("append_allow_rules only accepts action = \"allow\"");
            }
            let Some(workspace) = rule
                .workspace
                .as_deref()
                .and_then(codewhale_execpolicy::normalize_workspace_scope)
            else {
                bail!("persistent allow rules must be scoped to a workspace");
            };
            if rule.command.is_some() && !rule.command_exact {
                bail!("persistent command allow rules must use exact matching");
            }
            if rule.command.is_none() && rule.path.is_none() {
                bail!("persistent allow rules must match an exact command or path");
            }
            if let Some(command) = rule.command.as_deref()
                && command.trim().is_empty()
            {
                bail!("persistent command allow rules must not be empty");
            }
            if let Some(path) = rule.path.as_deref()
                && codewhale_execpolicy::normalize_workspace_relative_path(path, &workspace)
                    .is_none_or(|path| path.is_empty())
            {
                bail!("persistent path allow rules must stay within the workspace");
            }
        }
        self.append_permission_rules(rules, PermissionAction::Allow)
    }

    fn append_permission_rules(
        &mut self,
        rules: &[ToolAskRule],
        expected_action: PermissionAction,
    ) -> Result<usize> {
        if rules.is_empty() {
            return Ok(0);
        }
        if rules.iter().any(|rule| rule.action != expected_action) {
            bail!(
                "permission rule action does not match requested {:?} persistence",
                expected_action
            );
        }

        let path = checked_permissions_path_for_config_path(&self.path)?;
        let (added, persisted) = config_document::with_config_write_lock(&path, |path| {
            let (_, raw, mut permissions) = read_permissions_state(path)?;
            let mut document = parse_permissions_document(path, &raw)?;

            if !document.contains_key("rules") {
                document["rules"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
            }
            let rules_item = document
                .get_mut("rules")
                .expect("rules entry was inserted above");

            let mut added = 0;
            for rule in rules {
                if permissions.rules.contains(rule) {
                    continue;
                }
                append_permission_rule(rules_item, rule)?;
                permissions.rules.push(rule.clone());
                added += 1;
            }
            if added == 0 {
                return Ok((0, permissions));
            }

            let body = document.to_string();
            let persisted = parse_generated_permissions(path, &body)?;
            write_permissions_atomic(path, body.as_bytes())?;
            Ok((added, persisted))
        })?;
        self.permissions = persisted;
        Ok(added)
    }
}
