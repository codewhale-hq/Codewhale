use crate::display_redaction::is_sensitive_config_key;
use crate::path_io::{
    normalize_config_file_path, read_checked_config_file, read_checked_toml_file,
    reject_path_symlink,
};
use crate::provider_toml::CONFIG_FILE_NAME;
use crate::{config_document, legacy_root, persistence};
use anyhow::Context as _;
use anyhow::Result;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub(crate) fn config_backup_file_name(path: &Path) -> OsString {
    let mut file_name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(CONFIG_FILE_NAME));
    file_name.push(".bak");
    file_name
}

pub(crate) fn config_sibling_path_unchecked(config_path: &Path, file_name: &OsStr) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(file_name)
}

pub(crate) fn checked_config_sibling_path(
    config_path: &Path,
    file_name: &OsStr,
) -> Result<PathBuf> {
    let config_path = normalize_config_file_path(config_path.to_path_buf())?;
    let parent = config_path
        .parent()
        .context("config path must include a parent directory")?;
    let path = parent.join(file_name);
    reject_path_symlink(&path)?;
    Ok(path)
}

#[cfg(test)]
pub(crate) fn config_backup_path(path: &Path) -> PathBuf {
    config_sibling_path_unchecked(path, &config_backup_file_name(path))
}

pub(crate) fn checked_config_backup_path(path: &Path) -> Result<PathBuf> {
    checked_config_sibling_path(path, &config_backup_file_name(path))
}

/// Remove plaintext `api_key` entries from the one-time config backup, if it
/// exists.
///
/// Credential migration deliberately preserves the rest of `config.toml.bak`
/// while ensuring that moving a key into the durable secret store does not
/// leave the same credential behind in an older backup.
pub fn scrub_plaintext_api_keys_from_config_backup(path: &Path) -> Result<()> {
    scrub_credentials_from_backup_file(&checked_config_backup_path(path)?)
}

/// Rewrite an existing backup without credential-named keys; see
/// [`config_toml_without_plaintext_api_keys`].
pub(crate) fn scrub_credentials_from_backup_file(backup: &Path) -> Result<()> {
    if !backup.exists() {
        return Ok(());
    }

    let raw = read_checked_toml_file(backup, "config backup")?;
    let scrubbed = config_toml_without_plaintext_api_keys(&raw).with_context(|| {
        format!(
            "failed to scrub plaintext API keys from config backup {}",
            backup.display()
        )
    })?;
    if scrubbed != raw {
        persistence::atomic_write(backup, scrubbed.as_bytes()).with_context(|| {
            format!(
                "failed to write credential-free config backup {}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

/// Remove only retired Antigravity state from Codewhale's one-time config
/// backup. This never resolves, reads, writes, or revokes any external Google
/// or Antigravity session; it edits only the checked sibling `.bak` file owned
/// by Codewhale.
pub fn scrub_legacy_antigravity_from_config_backup(path: &Path) -> Result<()> {
    let backup = checked_config_backup_path(path)?;
    if !backup.exists() {
        return Ok(());
    }

    let raw = read_checked_toml_file(&backup, "config backup")?;
    let scrubbed = config_toml_without_legacy_antigravity(&raw).with_context(|| {
        format!(
            "failed to clear retired provider state from config backup {}",
            backup.display()
        )
    })?;
    if scrubbed != raw {
        persistence::atomic_write(&backup, scrubbed.as_bytes()).with_context(|| {
            format!(
                "failed to write retired-provider-free config backup {}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

pub(crate) fn config_toml_without_legacy_antigravity(raw: &str) -> Result<String> {
    let mut document = raw.parse::<toml_edit::DocumentMut>().map_err(|_| {
        anyhow::anyhow!(
            "failed to parse config TOML while clearing retired provider state; file contents were omitted"
        )
    })?;
    let root = document.as_table_mut();

    if root
        .get("provider")
        .and_then(toml_edit::Item::as_str)
        .is_some_and(is_legacy_antigravity_name)
    {
        root.remove("provider");
    }
    if let Some(fallbacks) = root
        .get_mut("fallback_providers")
        .and_then(toml_edit::Item::as_array_mut)
    {
        fallbacks.retain(|value| !value.as_str().is_some_and(is_legacy_antigravity_name));
        if fallbacks.is_empty() {
            root.remove("fallback_providers");
        }
    }
    if let Some(providers) = root
        .get_mut("providers")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        providers.remove("antigravity");
        providers.remove("agy");
    }

    Ok(document.to_string())
}

pub(crate) fn is_legacy_antigravity_name(value: &str) -> bool {
    value.eq_ignore_ascii_case("antigravity") || value.eq_ignore_ascii_case("agy")
}

/// Before a write moves legacy top-level keys (#6394), keep one credential-free
/// copy of the file as it was, and queue a one-line notice for the caller's
/// status channel. Does nothing when the original has nothing to move.
pub(crate) fn note_legacy_root_file_migration(path: &Path, original_raw: &str) -> Result<()> {
    let Ok(document) = original_raw.parse::<toml_edit::DocumentMut>() else {
        return Ok(());
    };
    let receipt = legacy_root::preview_document(&document, None);
    if !receipt.changes_file() {
        return Ok(());
    }
    let backup = write_legacy_root_backup(path, original_raw)?;
    legacy_root::queue_notice(&receipt, &backup);
    Ok(())
}

/// Keep one credential-free copy of the file as it was before legacy
/// top-level keys first moved. Later migrations never overwrite it.
pub(crate) fn write_legacy_root_backup(path: &Path, original_raw: &str) -> Result<PathBuf> {
    let backup = checked_config_sibling_path(path, &pre_migrate_backup_file_name(path))?;
    if backup.exists() {
        // Written once and kept; still repair one left by a release that
        // scrubbed only `api_key`. Best effort: an unreadable old backup must
        // not block the save that is moving keys now.
        if let Err(err) = scrub_credentials_from_backup_file(&backup) {
            tracing::warn!("{err:#}");
        }
    } else {
        let scrubbed = config_toml_without_plaintext_api_keys(original_raw)?;
        persistence::atomic_write(&backup, scrubbed.as_bytes()).with_context(|| {
            format!(
                "failed to create config backup {} before moving top-level keys",
                backup.display()
            )
        })?;
    }
    Ok(backup)
}

pub(crate) fn pre_migrate_backup_file_name(path: &Path) -> OsString {
    let mut file_name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(CONFIG_FILE_NAME));
    file_name.push(".pre-migrate.bak");
    file_name
}

/// Path of the one-time backup written before legacy top-level keys moved.
pub fn legacy_root_backup_path(path: &Path) -> Result<PathBuf> {
    checked_config_sibling_path(path, &pre_migrate_backup_file_name(path))
}

pub(crate) fn write_one_time_config_backup(path: &Path) -> Result<()> {
    let backup = checked_config_backup_path(path)?;
    if backup.exists() {
        return scrub_plaintext_api_keys_from_config_backup(path);
    }

    let raw = read_checked_config_file(path)?;
    let scrubbed = config_toml_without_plaintext_api_keys(&raw).with_context(|| {
        format!(
            "failed to scrub plaintext API keys while creating config backup {}",
            backup.display()
        )
    })?;
    persistence::atomic_write(&backup, scrubbed.as_bytes()).with_context(|| {
        format!(
            "failed to create credential-free config backup {} from {}",
            backup.display(),
            path.display()
        )
    })?;
    Ok(())
}

pub(crate) fn config_toml_without_plaintext_api_keys(raw: &str) -> Result<String> {
    let mut document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| {
            anyhow::anyhow!(
                "failed to parse config TOML while removing plaintext API keys; file contents were omitted"
            )
        })?;
    remove_plaintext_api_keys_recursive(document.as_table_mut());
    scrub_backup_decor(document.as_table_mut().decor_mut());
    if let Some(trailing) = document.trailing().as_str().map(scrub_backup_comments) {
        document.set_trailing(trailing);
    }
    Ok(document.to_string())
}

/// Drop every credential-named key ([`is_sensitive_config_key`]: `api_key`,
/// `webhook_token`, `Authorization` under `http_headers`, ...) at any depth,
/// including tables inside arrays. The backups are kept indefinitely and are
/// described as credential-free, so a rotated secret must not live on there.
pub(crate) fn remove_plaintext_api_keys_recursive(table: &mut dyn toml_edit::TableLike) {
    let sensitive: Vec<String> = table
        .iter()
        .filter(|(key, item)| {
            is_sensitive_config_key(key) || item.as_value().is_some_and(backup_value_carries_secret)
        })
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in sensitive {
        // Keep a comment written above the key (often the file header).
        config_document::remove_key_preserving_leading_decor(table, &key);
    }
    for (mut key, item) in table.iter_mut() {
        scrub_backup_decor(key.leaf_decor_mut());
        if let toml_edit::Item::Table(nested) = item {
            scrub_backup_decor(nested.decor_mut());
        }
        if let toml_edit::Item::Value(value) = item {
            scrub_backup_decor(value.decor_mut());
        }
        match item {
            toml_edit::Item::ArrayOfTables(tables) => {
                for nested in tables.iter_mut() {
                    scrub_backup_decor(nested.decor_mut());
                    remove_plaintext_api_keys_recursive(nested);
                }
            }
            toml_edit::Item::Value(toml_edit::Value::Array(array)) => {
                remove_plaintext_api_keys_in_array(array);
            }
            _ => {
                if let Some(nested) = item.as_table_like_mut() {
                    remove_plaintext_api_keys_recursive(nested);
                }
            }
        }
    }
}

pub(crate) fn remove_plaintext_api_keys_in_array(array: &mut toml_edit::Array) {
    array.retain(|value| !backup_value_carries_secret(value));
    for value in array.iter_mut() {
        scrub_backup_decor(value.decor_mut());
        match value {
            toml_edit::Value::InlineTable(table) => remove_plaintext_api_keys_recursive(table),
            toml_edit::Value::Array(nested) => remove_plaintext_api_keys_in_array(nested),
            _ => {}
        }
    }
}

pub(crate) fn backup_value_carries_secret(value: &toml_edit::Value) -> bool {
    value
        .as_str()
        .is_some_and(codewhale_secrets::sanitize::contains_secret)
}

pub(crate) fn scrub_backup_decor(decor: &mut toml_edit::Decor) {
    let prefix = decor
        .prefix()
        .and_then(|text| text.as_str())
        .map(scrub_backup_comments);
    let suffix = decor
        .suffix()
        .and_then(|text| text.as_str())
        .map(scrub_backup_comments);
    if let Some(prefix) = prefix {
        decor.set_prefix(prefix);
    }
    if let Some(suffix) = suffix {
        decor.set_suffix(suffix);
    }
}

pub(crate) fn scrub_backup_comments(text: &str) -> String {
    text.split_inclusive('\n')
        .filter(|line| {
            !line
                .trim_start()
                .strip_prefix('#')
                .map(str::trim)
                .is_some_and(|comment| {
                    comment
                        .split_once('=')
                        .is_some_and(|(key, _)| is_sensitive_config_key(key))
                        || codewhale_secrets::sanitize::contains_secret(comment)
                })
        })
        .collect()
}

/// Merge comments and formatting from an original TOML file into a
/// freshly serialized document so user annotations (comments, whitespace,
/// disabled keys) survive config rewrites.
///
/// `original_raw` is the raw text of the file before the change; the
/// function parses it internally with [`toml_edit`] so callers stay free
/// of that dependency.
pub fn merge_and_preserve_comments(serialized: &str, original_raw: &str) -> Result<String> {
    let original = original_raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| {
            anyhow::anyhow!(
                "failed to parse original config for comment merge; file contents were omitted"
            )
        })?;

    let mut new_doc = serialized.parse::<toml_edit::DocumentMut>().map_err(|_| {
        anyhow::anyhow!(
            "failed to parse serialized config for comment merge; file contents were omitted"
        )
    })?;

    // Reuse the original document’s trailing text (file-footer comments /
    // disabled keys) so they survive the rewrite.
    new_doc.set_trailing(original.trailing().clone());

    // Copy the top-level table's decor (document-header comments, whitespace
    // before the first key) which `toml_edit` stores on the root `Table` itself.
    *new_doc.as_table_mut().decor_mut() = original.as_table().decor().clone();

    merge_decor_table(new_doc.as_table_mut(), original.as_table());

    Ok(new_doc.to_string())
}

/// Recursively copy `decor` (prefix/suffix comments and whitespace) from
/// every key in `source` that also exists in `target`.
pub(crate) fn merge_decor_table(target: &mut toml_edit::Table, source: &toml_edit::Table) {
    // Collect keys first — the borrow checker won't let us hold
    // `get_key_value_mut` while iterating.
    let keys: Vec<String> = source.iter().map(|(k, _)| k.to_owned()).collect();
    for key in &keys {
        let Some((source_key, source_item)) = source.get_key_value(key) else {
            continue;
        };
        let Some((mut target_key_mut, target_item)) = target.get_key_value_mut(key) else {
            continue;
        };

        // Copy the key-level decor (comments before the key itself)
        *target_key_mut.leaf_decor_mut() = source_key.leaf_decor().clone();

        copy_item_decor(target_item, source_item);

        if let (Some(tt), Some(st)) = (target_item.as_table_mut(), source_item.as_table()) {
            merge_decor_table(tt, st);
        }

        if let (Some(ta), Some(sa)) = (
            target_item.as_array_of_tables_mut(),
            source_item.as_array_of_tables(),
        ) {
            for (i, source_table) in sa.iter().enumerate() {
                if let Some(target_table) = ta.get_mut(i) {
                    copy_item_decor_table(target_table, source_table);
                    merge_decor_table(target_table, source_table);
                }
            }
        }
    }
}

/// Copy the decor (comments and surrounding whitespace) from `source` to `target`,
/// respecting the concrete item type since [`toml_edit::Item`] has no uniform
/// `decor` accessor.
pub(crate) fn copy_item_decor(target: &mut toml_edit::Item, source: &toml_edit::Item) {
    match (target, source) {
        (toml_edit::Item::Table(tt), toml_edit::Item::Table(st)) => {
            *tt.decor_mut() = st.decor().clone();
        }
        (toml_edit::Item::Value(tv), toml_edit::Item::Value(sv)) => {
            *tv.decor_mut() = sv.decor().clone();
        }
        _ => {}
    }
}

pub(crate) fn copy_item_decor_table(target: &mut toml_edit::Table, source: &toml_edit::Table) {
    *target.decor_mut() = source.decor().clone();
}
