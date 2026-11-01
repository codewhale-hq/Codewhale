use crate::config_backup::{checked_config_sibling_path, config_sibling_path_unchecked};
use crate::config_document;
use crate::config_store::toml_error_location;
use crate::external_credentials::quote_os_path;
use crate::path_io::{checked_path_exists, read_checked_permissions_file};
use crate::provider_toml::PERMISSIONS_FILE_NAME;
use crate::state_dirs::resolve_config_path;
use anyhow::Context as _;
use anyhow::{Result, bail};
use codewhale_execpolicy::{PermissionAction, Ruleset, ToolAskRule};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Sibling `permissions.toml` schema.
///
/// Each rule is a typed condition that can deny, allow, or ask before a tool
/// invocation. The approval card persists ask rules and narrowly scoped,
/// exact allow grants; deny rules remain manually authored.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermissionsToml {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ToolAskRule>,
}

/// On-disk state of the active sibling `permissions.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionsFileState {
    /// No sibling permission file exists.
    Missing,
    /// The sibling permission file exists but contains no TOML content.
    Empty,
    /// The sibling permission file contains a parsed TOML document.
    Present,
}

/// A parsed, read-only view of the active sibling `permissions.toml`.
///
/// Removal tokens bind a displayed rule index to the exact file bytes that
/// produced this snapshot. A later editor must present the rule again when
/// another process changed the file instead of deleting whichever rule moved
/// into the old index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionsSnapshot {
    path: PathBuf,
    file_state: PermissionsFileState,
    permissions: PermissionsToml,
    removal_tokens: Vec<String>,
}

impl PermissionsSnapshot {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn file_exists(&self) -> bool {
        self.file_state != PermissionsFileState::Missing
    }

    #[must_use]
    pub fn file_state(&self) -> PermissionsFileState {
        self.file_state
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionsToml {
        &self.permissions
    }

    #[must_use]
    pub fn rules(&self) -> &[ToolAskRule] {
        &self.permissions.rules
    }

    /// Return the opaque confirmation token for a zero-based rule index.
    #[must_use]
    pub fn removal_token(&self, index: usize) -> Option<&str> {
        self.removal_tokens.get(index).map(String::as_str)
    }
}

impl PermissionsToml {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    #[must_use]
    pub fn ruleset(&self) -> Ruleset {
        let mut denied = Vec::new();
        let mut trusted = Vec::new();
        let mut ask_rules = Vec::new();

        for rule in &self.rules {
            match rule.action {
                PermissionAction::Deny => {
                    // Command-based deny rules are promoted to denied_prefixes
                    // so they are caught by execpolicy's deny-always-wins check.
                    if let Some(cmd) = &rule.command
                        && !rule.command_exact
                        && rule.workspace.is_none()
                    {
                        denied.push(cmd.clone());
                    }
                    // Always keep in ask_rules for path-based and tool-only matching.
                    ask_rules.push(rule.clone());
                }
                PermissionAction::Allow => {
                    // Command-based allow rules are promoted to trusted_prefixes
                    // for arity-aware matching.  Path-only allow rules are
                    // handled through ask_rules (they skip the approval prompt).
                    if let Some(cmd) = &rule.command
                        && !rule.command_exact
                        && rule.workspace.is_none()
                    {
                        trusted.push(cmd.clone());
                    }
                    // Keep in ask_rules so path-only allow rules also work.
                    ask_rules.push(rule.clone());
                }
                PermissionAction::Ask => {
                    ask_rules.push(rule.clone());
                }
            }
        }

        Ruleset::user(trusted, denied).with_ask_rules(ask_rules)
    }
}

#[must_use]
pub fn permissions_path_for_config_path(config_path: &Path) -> PathBuf {
    config_sibling_path_unchecked(config_path, OsStr::new(PERMISSIONS_FILE_NAME))
}

pub(crate) fn checked_permissions_path_for_config_path(config_path: &Path) -> Result<PathBuf> {
    checked_config_sibling_path(config_path, OsStr::new(PERMISSIONS_FILE_NAME))
}

pub fn resolve_permissions_path(config_path: Option<PathBuf>) -> Result<PathBuf> {
    checked_permissions_path_for_config_path(&resolve_config_path(config_path)?)
}

/// Load the active sibling permission rules with confirmation tokens suitable
/// for a later compare-and-remove operation.
pub fn load_permissions_snapshot(config_path: Option<PathBuf>) -> Result<PermissionsSnapshot> {
    let path = resolve_permissions_path(config_path)?;
    let (file_exists, raw, permissions) = read_permissions_state(&path)?;
    let file_state = if !file_exists {
        PermissionsFileState::Missing
    } else if raw.is_empty() {
        PermissionsFileState::Empty
    } else {
        PermissionsFileState::Present
    };
    let removal_tokens = (0..permissions.rules.len())
        .map(|index| permission_removal_token(&path, &raw, index))
        .collect();
    Ok(PermissionsSnapshot {
        path,
        file_state,
        permissions,
        removal_tokens,
    })
}

/// Remove one zero-based permission rule if `expected_token` still describes
/// that exact index in the current file.
///
/// The file is re-read only after acquiring the same adjacent lock used by
/// append operations. This makes the token check and atomic replacement one
/// transaction, preventing stale list views from deleting a different rule.
pub fn remove_permission_rule(
    config_path: Option<PathBuf>,
    index: usize,
    expected_token: &str,
) -> Result<ToolAskRule> {
    let path = resolve_permissions_path(config_path)?;
    config_document::with_config_write_lock(&path, |path| {
        let (file_exists, raw, permissions) = read_permissions_state(path)?;
        if !file_exists {
            bail!(
                "permissions changed after they were listed; reload {} and retry",
                quote_os_path(path)
            );
        }
        let rule = permissions.rules.get(index).cloned().with_context(|| {
            format!(
                "permission rule {} no longer exists in {}; list rules again",
                index + 1,
                quote_os_path(path)
            )
        })?;
        let current_token = permission_removal_token(path, &raw, index);
        if current_token != expected_token {
            bail!(
                "permissions changed after they were listed; reload {} and retry",
                quote_os_path(path)
            );
        }

        let mut document = parse_permissions_document(path, &raw)?;
        let rules_item = document.get_mut("rules").with_context(|| {
            format!(
                "permissions at {} no longer contain a rules array",
                quote_os_path(path)
            )
        })?;
        let orphaned_header = remove_permission_rule_item(rules_item, index)?;
        if let Some(header) = orphaned_header {
            let trailing = format!(
                "{header}{}",
                document.trailing().as_str().unwrap_or_default()
            );
            document.set_trailing(trailing);
        }
        let body = document.to_string();
        let persisted = parse_generated_permissions(path, &body)?;
        if persisted.rules.len() + 1 != permissions.rules.len() {
            bail!(
                "refusing inconsistent permission removal at {}",
                quote_os_path(path)
            );
        }
        write_permissions_atomic(path, body.as_bytes())?;
        Ok(rule)
    })
}

pub(crate) fn load_sibling_permissions(config_path: &Path) -> Result<PermissionsToml> {
    let permissions_path = checked_permissions_path_for_config_path(config_path)?;
    let (_, _, permissions) = read_permissions_state(&permissions_path)?;
    Ok(permissions)
}

pub(crate) fn read_permissions_state(path: &Path) -> Result<(bool, String, PermissionsToml)> {
    let file_exists = checked_path_exists(path)?;
    let raw = if file_exists {
        read_checked_permissions_file(path)?
    } else {
        String::new()
    };
    let permissions = if raw.trim().is_empty() {
        PermissionsToml::default()
    } else {
        toml::from_str(&raw).map_err(|err| {
            anyhow::anyhow!(
                "failed to parse permissions at {} ({}); file contents were omitted",
                quote_os_path(path),
                toml_error_location(Some(&raw), &err)
            )
        })?
    };
    Ok((file_exists, raw, permissions))
}

pub(crate) fn parse_permissions_document(path: &Path, raw: &str) -> Result<toml_edit::DocumentMut> {
    if raw.trim().is_empty() {
        Ok(toml_edit::DocumentMut::new())
    } else {
        raw.parse::<toml_edit::DocumentMut>().map_err(|_| {
            anyhow::anyhow!(
                "failed to edit permissions at {}; file contents were omitted",
                quote_os_path(path)
            )
        })
    }
}

pub(crate) fn parse_generated_permissions(path: &Path, body: &str) -> Result<PermissionsToml> {
    toml::from_str(body).map_err(|_| {
        anyhow::anyhow!(
            "generated invalid permissions document for {}; file contents were omitted",
            quote_os_path(path)
        )
    })
}

pub(crate) fn permission_removal_token(path: &Path, raw: &str, index: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale-permission-removal-v1\0");
    hasher.update(quote_os_path(path).as_bytes());
    hasher.update(b"\0");
    hasher.update(index.to_le_bytes());
    hasher.update(b"\0");
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    let mut token = String::with_capacity(24);
    for byte in &digest[..12] {
        use std::fmt::Write as _;
        let _ = write!(&mut token, "{byte:02x}");
    }
    token
}

pub(crate) fn append_permission_rule(item: &mut toml_edit::Item, rule: &ToolAskRule) -> Result<()> {
    match item {
        toml_edit::Item::ArrayOfTables(rules) => {
            rules.push(permission_rule_table(rule));
            Ok(())
        }
        toml_edit::Item::Value(value) => {
            let Some(rules) = value.as_array_mut() else {
                bail!("`rules` in permissions.toml must be an array");
            };
            rules.push(toml_edit::Value::InlineTable(permission_rule_inline_table(
                rule,
            )));
            Ok(())
        }
        _ => bail!("`rules` in permissions.toml must be an array"),
    }
}

pub(crate) fn remove_permission_rule_item(
    item: &mut toml_edit::Item,
    index: usize,
) -> Result<Option<String>> {
    match item {
        toml_edit::Item::ArrayOfTables(rules) => {
            if index >= rules.len() {
                bail!("permission rule index changed before removal");
            }
            let file_header = if index == 0 {
                rules
                    .get(index)
                    .and_then(|rule| rule.decor().prefix())
                    .and_then(toml_edit::RawString::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
            rules.remove(index);
            if let Some(header) = file_header.as_deref()
                && let Some(next_rule) = rules.get_mut(0)
            {
                let next_prefix = next_rule
                    .decor()
                    .prefix()
                    .and_then(toml_edit::RawString::as_str)
                    .unwrap_or_default()
                    .to_owned();
                next_rule
                    .decor_mut()
                    .set_prefix(format!("{header}{next_prefix}"));
                return Ok(None);
            }
            Ok(file_header)
        }
        toml_edit::Item::Value(value) => {
            let Some(rules) = value.as_array_mut() else {
                bail!("`rules` in permissions.toml must be an array");
            };
            if index >= rules.len() {
                bail!("permission rule index changed before removal");
            }
            rules.remove(index);
            Ok(None)
        }
        _ => bail!("`rules` in permissions.toml must be an array"),
    }
}

pub(crate) fn permission_rule_table(rule: &ToolAskRule) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    table["tool"] = toml_edit::value(rule.tool.clone());
    if let Some(command) = rule.command.as_deref() {
        table["command"] = toml_edit::value(command);
    }
    if rule.command_exact {
        table["command_exact"] = toml_edit::value(true);
    }
    if let Some(path) = rule.path.as_deref() {
        table["path"] = toml_edit::value(path);
    }
    if let Some(workspace) = rule.workspace.as_deref() {
        table["workspace"] = toml_edit::value(workspace);
    }
    if rule.action != PermissionAction::Ask {
        table["action"] = toml_edit::value(match rule.action {
            PermissionAction::Allow => "allow",
            PermissionAction::Ask => "ask",
            PermissionAction::Deny => "deny",
        });
    }
    table
}

pub(crate) fn permission_rule_inline_table(rule: &ToolAskRule) -> toml_edit::InlineTable {
    let mut table = toml_edit::InlineTable::new();
    table.insert("tool", toml_edit::Value::from(rule.tool.clone()));
    if let Some(command) = rule.command.as_deref() {
        table.insert("command", toml_edit::Value::from(command));
    }
    if rule.command_exact {
        table.insert("command_exact", toml_edit::Value::from(true));
    }
    if let Some(path) = rule.path.as_deref() {
        table.insert("path", toml_edit::Value::from(path));
    }
    if let Some(workspace) = rule.workspace.as_deref() {
        table.insert("workspace", toml_edit::Value::from(workspace));
    }
    if rule.action != PermissionAction::Ask {
        table.insert(
            "action",
            toml_edit::Value::from(match rule.action {
                PermissionAction::Allow => "allow",
                PermissionAction::Ask => "ask",
                PermissionAction::Deny => "deny",
            }),
        );
    }
    table
}

pub(crate) fn write_permissions_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path.parent().with_context(|| {
        format!(
            "permissions path has no parent directory: {}",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create permissions directory {}",
            parent.display()
        )
    })?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary permissions file in {}",
            parent.display()
        )
    })?;
    // A filesystem that assigns modes itself (HarmonyOS hmdfs) refuses the
    // call; `tighten_mode` tolerates only that case and reports every other
    // failure, so a permissions file that really is world-readable still
    // fails here.
    #[cfg(unix)]
    crate::private_directory::tighten_mode_of(temporary.as_file(), temporary.path(), 0o600)
        .with_context(|| {
            format!(
                "failed to secure temporary permissions file for {}",
                path.display()
            )
        })?;
    temporary
        .write_all(body)
        .with_context(|| format!("failed to write permissions at {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync permissions at {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace permissions at {}", path.display()))?;
    Ok(())
}
