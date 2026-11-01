//! Lossless, serialized `config.toml` mutation.
//!
//! Every Codewhale config writer coordinates through the adjacent lock owned
//! here. Mutations re-read only after acquiring the lock, so a stale process
//! cannot resurrect revoked credential authority. Callers that still serialize
//! a full typed snapshot must supply the exact bytes they originally loaded and
//! fail on a concurrent change.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{
    checked_path_exists, normalize_config_file_path, persistence, read_checked_config_file,
    write_one_time_config_backup,
};

/// Parse the latest document under the shared write lock, apply `mutate`, and
/// atomically persist only the resulting delta.
pub fn mutate_config_document<T, F>(path: &Path, mutate: F) -> Result<T>
where
    F: FnOnce(&mut toml_edit::DocumentMut) -> Result<T>,
{
    mutate_config_document_with_migration(path, |doc, _| mutate(doc))
}

/// [`mutate_config_document`], also handing `mutate` the receipt of the legacy
/// top-level `base_url` / `api_key` move this same write made, so a caller can
/// tell a value it just moved from one that was already in its table.
pub fn mutate_config_document_with_migration<T, F>(path: &Path, mutate: F) -> Result<T>
where
    F: FnOnce(&mut toml_edit::DocumentMut, &crate::legacy_root::LegacyRootMigration) -> Result<T>,
{
    mutate_locked(path, mutate).map(|(result, _)| result)
}

/// [`mutate_config_document`], also returning what undoes exactly this write.
///
/// Both halves are taken under the lock: the bytes this call left on disk and
/// the document as it stood just before `mutate` ran (after this writer's own
/// one-way repairs), or the file's absence. A caller whose follow-up step is
/// refused can then take back only its own change with
/// [`ConfigDocumentUndo::undo`].
pub fn mutate_config_document_undoable<T, F>(
    path: &Path,
    mutate: F,
) -> Result<(T, ConfigDocumentUndo)>
where
    F: FnOnce(&mut toml_edit::DocumentMut) -> Result<T>,
{
    mutate_locked(path, |doc, _| mutate(doc))
}

/// The undoable write, also retaining the migration performed under this same
/// lock. The caller can validate a captured route against the freshly moved
/// document before writing its delta; undo still compares the exact written bytes.
pub fn mutate_config_document_undoable_with_migration<T, F>(
    path: &Path,
    mutate: F,
) -> Result<(T, ConfigDocumentUndo)>
where
    F: FnOnce(&mut toml_edit::DocumentMut, &crate::legacy_root::LegacyRootMigration) -> Result<T>,
{
    mutate_locked(path, mutate)
}

fn mutate_locked<T, F>(path: &Path, mutate: F) -> Result<(T, ConfigDocumentUndo)>
where
    F: FnOnce(&mut toml_edit::DocumentMut, &crate::legacy_root::LegacyRootMigration) -> Result<T>,
{
    let requested = path.to_path_buf();
    with_config_write_lock(path, |path| {
        let original = read_optional_config(path)?;
        let mut document = match original.as_deref() {
            Some(raw) if !raw.trim().is_empty() => {
                raw.parse::<toml_edit::DocumentMut>().map_err(|_| {
                    anyhow::anyhow!(
                        "failed to parse config at {}; file contents were omitted",
                        crate::quote_os_path(path)
                    )
                })?
            }
            _ => toml_edit::DocumentMut::new(),
        };
        heal_extras_nesting(&mut document);
        // Move legacy top-level `base_url` / `api_key` into their provider
        // tables as part of this write (#6394). Conflicting pairs stay put
        // unless this very write changes the table side.
        let moved = crate::legacy_root::apply_to_document(&mut document, None);
        let conflicts = crate::legacy_root::conflict_snapshot(&document);
        let restore = original.as_ref().map(|_| document.to_string());
        let result = mutate(&mut document, &moved)?;
        crate::legacy_root::settle_conflicts_after_write(&mut document, conflicts);
        let body = document.to_string();
        if original.as_deref() == Some(body.as_str()) || (original.is_none() && body.is_empty()) {
            let undo = ConfigDocumentUndo {
                path: requested,
                written: original,
                restore,
            };
            return Ok((result, undo));
        }
        if moved.changes_file()
            && let Some(original) = original.as_deref()
        {
            crate::note_legacy_root_file_migration(path, original)?;
        }
        persist_locked(path, original.as_deref(), body.as_bytes())?;
        let undo = ConfigDocumentUndo {
            path: requested,
            written: Some(body),
            restore,
        };
        Ok((result, undo))
    })
}

/// Takes back one [`mutate_config_document_undoable`] write, and only that.
#[derive(Debug, Clone)]
pub struct ConfigDocumentUndo {
    path: PathBuf,
    /// The bytes that write left on disk; `None` when the file stayed absent.
    written: Option<String>,
    /// What `undo` puts back; `None` removes the file that write created.
    restore: Option<String>,
}

impl ConfigDocumentUndo {
    /// Put the document back as it was before the write, but only while the
    /// file still holds exactly the bytes that write left: any newer save
    /// wins. Returns `false` when the file changed since and was left alone.
    pub fn undo(&self) -> Result<bool> {
        with_config_write_lock(&self.path, |path| {
            let current = read_optional_config(path)?;
            if current == self.restore {
                return Ok(true);
            }
            if current != self.written {
                return Ok(false);
            }
            match self.restore.as_deref() {
                Some(body) => persist_locked(path, current.as_deref(), body.as_bytes())?,
                None => fs::remove_file(path).with_context(|| {
                    format!("failed to remove config at {}", crate::quote_os_path(path))
                })?,
            }
            Ok(true)
        })
    }
}

/// Lift keys trapped under literal `[extras]` tables back to the top level.
///
/// The config structs flatten unknown keys into an `extras` map; a historic
/// writer serialized that map under a literal `extras` key, and every
/// subsequent buggy round-trip nested it one level deeper
/// (`[extras.extras.extras.projects."..."]`). That silently strips real
/// state — workspace trust records, profiles, saved tokens — from every
/// reader that looks at the canonical top-level tables (2026-07-23 user
/// report: saved permission/trust ignored on each new session).
///
/// Healing runs on every config mutation: entries move up one level per
/// pass (existing top-level values always win; shadowed duplicates are
/// dropped), until no literal `extras` table remains. Bounded passes keep a
/// pathological file from looping.
///
/// An `extras` key that is *not* table-like (a string, array, or number) has
/// nothing to lift, so it is left exactly where it is. Removing it would
/// delete user data this function cannot heal, on every subsequent write.
pub fn heal_extras_nesting(document: &mut toml_edit::DocumentMut) -> bool {
    let mut healed = false;
    for _ in 0..16 {
        if document
            .get("extras")
            .is_none_or(|item| !item.is_table_like())
        {
            break;
        }
        let Some(extras) = document
            .remove("extras")
            .and_then(|item| item.into_table().ok())
        else {
            break;
        };
        healed = true;
        for (key, value) in extras {
            if document.get(&key).is_none() {
                document.insert(&key, value);
            }
        }
    }
    healed
}

/// What `codewhale config migrate` would do to the file at `path`, without
/// writing it (#6394).
pub fn preview_legacy_root_config(
    path: &Path,
    prefer: Option<crate::legacy_root::LegacyRootPrefer>,
) -> Result<crate::legacy_root::LegacyRootMigration> {
    let Some(raw) = read_optional_config(path)? else {
        return Ok(crate::legacy_root::LegacyRootMigration::default());
    };
    let document = raw.parse::<toml_edit::DocumentMut>().map_err(|_| {
        anyhow::anyhow!(
            "failed to parse config at {}; file contents were omitted",
            crate::quote_os_path(path)
        )
    })?;
    Ok(crate::legacy_root::preview_document(&document, prefer))
}

/// Move legacy top-level `base_url` / `api_key` into their provider tables
/// on disk, keeping comments. A conflicting pair changes only when `prefer`
/// says which side to keep. Before the first write a one-time,
/// credential-free backup is kept; its path is returned when one exists.
pub fn migrate_legacy_root_config(
    path: &Path,
    prefer: Option<crate::legacy_root::LegacyRootPrefer>,
) -> Result<(crate::legacy_root::LegacyRootMigration, Option<PathBuf>)> {
    with_config_write_lock(path, |path| {
        let Some(original) = read_optional_config(path)? else {
            return Ok((crate::legacy_root::LegacyRootMigration::default(), None));
        };
        let mut document = original.parse::<toml_edit::DocumentMut>().map_err(|_| {
            anyhow::anyhow!(
                "failed to parse config at {}; file contents were omitted",
                crate::quote_os_path(path)
            )
        })?;
        let receipt = crate::legacy_root::apply_to_document(&mut document, prefer);
        if !receipt.changes_file() {
            return Ok((receipt, None));
        }
        let backup = crate::write_legacy_root_backup(path, &original)?;
        persist_locked(path, Some(&original), document.to_string().as_bytes())?;
        Ok((receipt, Some(backup)))
    })
}

/// Create a config file only if it is still absent when the shared lock is
/// acquired. This closes the `exists()`/create race in first-run writers.
pub fn create_config_document(path: &Path, body: &str) -> Result<()> {
    replace_config_document_if_unchanged(path, None, body)
}

/// Replace a full typed snapshot only when on-disk bytes still equal the
/// snapshot the caller originally loaded. `None` means the file was absent.
pub fn replace_config_document_if_unchanged(
    path: &Path,
    expected: Option<&str>,
    body: &str,
) -> Result<()> {
    with_config_write_lock(path, |path| {
        let current = read_optional_config(path)?;
        if current.as_deref() == Some(body) {
            return Ok(());
        }
        if current.as_deref() != expected {
            bail!(
                "config changed after it was loaded; reload {} and retry instead of overwriting concurrent changes",
                crate::quote_os_path(path)
            );
        }
        persist_locked(path, current.as_deref(), body.as_bytes())
    })
}

/// Set a value at `segments`, creating implicit parent tables while preserving
/// existing key/value decor.
pub fn set_config_document_value(
    doc: &mut toml_edit::DocumentMut,
    segments: &[&str],
    value: impl Into<toml_edit::Value>,
) -> Result<()> {
    let (key, parents) = segments
        .split_last()
        .context("config value path must not be empty")?;
    let table = table_like_at_path_mut(doc.as_table_mut(), parents, PathLookup::Create)?
        .expect("Create lookups always yield a table");
    match table.get_mut(key) {
        Some(item) => {
            let mut value = value.into();
            if let Some(existing) = item.as_value() {
                *value.decor_mut() = existing.decor().clone();
            }
            *item = toml_edit::Item::Value(value);
        }
        None => {
            table.insert(key, toml_edit::value(value));
        }
    }
    Ok(())
}

/// Remove a value at `segments` without disturbing unrelated tables or decor.
pub fn unset_config_document_value(
    doc: &mut toml_edit::DocumentMut,
    segments: &[&str],
) -> Result<bool> {
    let (key, parents) = segments
        .split_last()
        .context("config value path must not be empty")?;
    let orphaned_root_prefix = (parents.is_empty() && doc.as_table().len() == 1)
        .then(|| leading_prefix_for_key(doc.as_table(), key))
        .flatten();
    let removed = {
        let Some(table) =
            table_like_at_path_mut(doc.as_table_mut(), parents, PathLookup::Existing)?
        else {
            return Ok(false);
        };
        remove_key_preserving_leading_decor(table, key)
    };
    if removed
        && let Some(prefix) = orphaned_root_prefix
        && prefix.as_str().is_some_and(|prefix| !prefix.is_empty())
    {
        let trailing = format!(
            "{}{}",
            prefix.as_str().unwrap_or_default(),
            doc.trailing().as_str().unwrap_or_default()
        );
        doc.set_trailing(trailing);
    }
    Ok(removed)
}

/// Serialize a complete read-modify-write operation against a canonical
/// configuration path. The callback must not acquire this same lock again.
pub fn with_config_write_lock<T>(
    path: &Path,
    operation: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let path = prepare_config_path(path)?;
    let lock_path = adjacent_lock_path(&path)?;
    super::reject_path_symlink(&lock_path)?;

    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let lock_file = options.open(&lock_path).with_context(|| {
        format!(
            "failed to open config lock at {}",
            crate::quote_os_path(&lock_path)
        )
    })?;
    // The lock is created at `0600`, so this only narrows a lock that an
    // older version or a different umask left wider. It is applied to the
    // handle that was just opened, not re-resolved by path: between the open
    // and the `chmod` the path could name a different object, and this is the
    // file the caller is about to lock. A mount that cannot apply modes is
    // excused by `tighten_mode_of`; a filesystem that stores them keeps the
    // error.
    #[cfg(unix)]
    crate::private_directory::tighten_mode_of(&lock_file, &lock_path, 0o600).with_context(
        || {
            format!(
                "failed to secure config lock at {}",
                crate::quote_os_path(&lock_path)
            )
        },
    )?;
    #[cfg(windows)]
    validate_windows_lock_handle(&lock_file, &lock_path)?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock.write().with_context(|| {
        format!(
            "failed to acquire config lock at {}",
            crate::quote_os_path(&lock_path)
        )
    })?;
    operation(&path)
}

#[cfg(windows)]
fn validate_windows_lock_handle(file: &fs::File, expected_path: &Path) -> Result<()> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::fs::MetadataExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW,
        VOLUME_NAME_DOS,
    };

    let metadata = file.metadata().with_context(|| {
        format!(
            "failed to inspect config lock at {}",
            crate::quote_os_path(expected_path)
        )
    })?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        bail!(
            "refusing non-regular or reparse-point config lock at {}",
            crate::quote_os_path(expected_path)
        );
    }

    let handle = file.as_raw_handle();
    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    // SAFETY: `handle` remains owned by `file`; a null output buffer asks for
    // the required UTF-16 length.
    let needed = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, flags) };
    if needed == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to resolve config lock at {}",
                crate::quote_os_path(expected_path)
            )
        });
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    // SAFETY: `buffer` is writable for its declared length and `handle` stays
    // valid through the call.
    let written = unsafe {
        GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to resolve config lock at {}",
                crate::quote_os_path(expected_path)
            )
        });
    }
    let actual = OsString::from_wide(&buffer[..written as usize]);
    if normalize_windows_path_for_comparison(Path::new(&actual))?
        != normalize_windows_path_for_comparison(expected_path)?
    {
        bail!(
            "config lock was redirected while opening {}",
            crate::quote_os_path(expected_path)
        );
    }
    Ok(())
}

#[cfg(windows)]
fn normalize_windows_path_for_comparison(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "config lock path {} contains invalid Unicode and cannot be compared safely",
            crate::quote_os_path(path)
        )
    })?;
    let without_device_prefix = text.strip_prefix(r"\\?\").unwrap_or(text);
    let normalized_prefix = without_device_prefix.strip_prefix("UNC\\").map_or_else(
        || without_device_prefix.to_string(),
        |rest| format!(r"\\{rest}"),
    );
    Ok(normalized_prefix
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase())
}

fn prepare_config_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for config path")?
            .join(path)
    };
    if let Some(parent) = absolute
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create config directory {}",
                crate::quote_os_path(parent)
            )
        })?;
    }
    normalize_config_file_path(absolute)
}

fn adjacent_lock_path(path: &Path) -> Result<PathBuf> {
    let mut file_name = path
        .file_name()
        .context("config path must include a file name")?
        .to_os_string();
    file_name.push(".lock");
    Ok(path
        .parent()
        .context("config path must include a parent directory")?
        .join(file_name))
}

fn read_optional_config(path: &Path) -> Result<Option<String>> {
    if checked_path_exists(path)? {
        read_checked_config_file(path).map(Some)
    } else {
        Ok(None)
    }
}

fn persist_locked(path: &Path, original: Option<&str>, body: &[u8]) -> Result<()> {
    if original.is_some() {
        write_one_time_config_backup(path)?;
    }
    persistence::atomic_write(path, body)
        .with_context(|| format!("failed to write config at {}", crate::quote_os_path(path)))
}

pub(crate) fn remove_key_preserving_leading_decor(
    table: &mut dyn toml_edit::TableLike,
    key: &str,
) -> bool {
    let mut found = false;
    let next_key = table.iter().find_map(|(candidate, _)| {
        if found {
            Some(candidate.to_owned())
        } else {
            found = candidate == key;
            None
        }
    });
    let leading_prefix = leading_prefix_for_key(table, key);
    if table.remove(key).is_none() {
        return false;
    }
    let Some(prefix) = leading_prefix else {
        return true;
    };
    let Some(next_key) = next_key else {
        return true;
    };
    if prefix.as_str() == Some("") {
        return true;
    }
    if let Some(mut next_key_decor) = table.key_mut(&next_key)
        && decor_prefix_is_empty(next_key_decor.leaf_decor())
    {
        next_key_decor.leaf_decor_mut().set_prefix(prefix);
    }
    true
}

fn decor_prefix_is_empty(decor: &toml_edit::Decor) -> bool {
    match decor.prefix() {
        Some(prefix) => prefix.as_str() == Some(""),
        None => true,
    }
}

fn leading_prefix_for_key(
    table: &dyn toml_edit::TableLike,
    key: &str,
) -> Option<toml_edit::RawString> {
    table
        .key(key)
        .and_then(|key| key.leaf_decor().prefix().cloned())
        .or_else(|| {
            table
                .get(key)
                .and_then(|item| item.as_value())
                .and_then(|value| value.decor().prefix().cloned())
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathLookup {
    Create,
    Existing,
}

fn table_like_at_path_mut<'a>(
    root: &'a mut toml_edit::Table,
    segments: &[&str],
    lookup: PathLookup,
) -> Result<Option<&'a mut dyn toml_edit::TableLike>> {
    let mut current: &mut dyn toml_edit::TableLike = root;
    for segment in segments {
        if current.get(segment).is_none() {
            match lookup {
                PathLookup::Create => {
                    let mut table = toml_edit::Table::new();
                    table.set_implicit(true);
                    current.insert(segment, toml_edit::Item::Table(table));
                }
                PathLookup::Existing => return Ok(None),
            }
        }
        let item = current
            .get_mut(segment)
            .expect("segment exists or was inserted above");
        match item.as_table_like_mut() {
            Some(table) => current = table,
            None => match lookup {
                PathLookup::Create => bail!("`{segment}` in config.toml must be a table"),
                PathLookup::Existing => return Ok(None),
            },
        }
    }
    Ok(Some(current))
}

#[cfg(test)]
mod tests {
    fn set_model(doc: &mut toml_edit::DocumentMut, model: &str) -> anyhow::Result<()> {
        super::set_config_document_value(doc, &["model"], model)
    }

    #[test]
    fn undo_restores_the_document_and_removes_a_file_it_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let before = "# keep me\nmodel = \"a\"\n";
        std::fs::write(&path, before).expect("write fixture");
        let ((), undo) = super::mutate_config_document_undoable(&path, |doc| set_model(doc, "b"))
            .expect("write");
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("\"b\"")
        );
        assert!(undo.undo().expect("undo"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), before);

        // A file this write created goes away again, not left behind empty.
        let absent = tmp.path().join("fresh.toml");
        let ((), undo) = super::mutate_config_document_undoable(&absent, |doc| set_model(doc, "b"))
            .expect("write");
        assert!(absent.exists());
        assert!(undo.undo().expect("undo"));
        assert!(!absent.exists(), "undo must not leave an empty config file");
    }

    #[test]
    fn undo_never_overwrites_a_newer_save() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "model = \"a\"\n").expect("write fixture");
        let ((), undo) = super::mutate_config_document_undoable(&path, |doc| set_model(doc, "b"))
            .expect("write");
        super::mutate_config_document(&path, |doc| set_model(doc, "c")).expect("newer save");
        assert!(!undo.undo().expect("undo"), "a changed file is reported");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "model = \"c\"\n"
        );

        // The same holds when the newer save removed a file the write created.
        let absent = tmp.path().join("fresh.toml");
        let ((), undo) = super::mutate_config_document_undoable(&absent, |doc| set_model(doc, "b"))
            .expect("write");
        std::fs::write(&absent, "model = \"c\"\n").expect("newer save");
        assert!(!undo.undo().expect("undo"));
        assert_eq!(
            std::fs::read_to_string(&absent).expect("read"),
            "model = \"c\"\n"
        );
    }

    #[test]
    fn healing_keeps_a_non_table_extras_key_it_cannot_lift() {
        // `extras` is where the config structs flatten unknown keys, so a
        // scalar or array under that exact name round-trips through the typed
        // path as ordinary user data. Healing used to `remove()` it before
        // discovering it was not a table, dropping it on the very next
        // `codewhale config set` — and reporting `healed == false` while doing
        // so.
        for body in [
            "extras = \"opaque\"\nmodel = \"m\"\n",
            "extras = [1, 2]\nmodel = \"m\"\n",
            "model = \"m\"\nextras = 7\n",
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("config.toml");
            std::fs::write(&path, body).expect("write fixture");

            super::mutate_config_document(&path, |doc| {
                super::set_config_document_value(doc, &["tui", "low_motion"], true)
            })
            .expect("mutate");

            let saved = std::fs::read_to_string(&path).expect("read");
            let parsed: toml::Value = toml::from_str(&saved).expect("parse");
            assert!(
                parsed.get("extras").is_some(),
                "non-table `extras` was deleted by an unrelated write: {saved}"
            );
            assert!(saved.contains("low_motion = true"), "{saved}");
        }
    }

    #[test]
    fn healing_still_lifts_an_inline_extras_table() {
        // The preservation guard above must not stop the real healing path:
        // an inline table is table-like and still gets lifted.
        let mut doc = "extras = { trust = true }\nmodel = \"m\"\n"
            .parse::<toml_edit::DocumentMut>()
            .expect("parse");
        assert!(super::heal_extras_nesting(&mut doc));
        let rendered = doc.to_string();
        assert!(rendered.contains("trust = true"), "{rendered}");
        assert!(!rendered.contains("extras"), "{rendered}");
    }

    #[test]
    fn healing_lifts_nested_extras_towers_to_the_top_level() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            concat!(
                "reasoning_effort = \"high\"\n\n",
                "[projects.\"/live\"]\n",
                "trust_level = \"trusted\"\n\n",
                "[extras.extras]\n",
                "chatgpt_access_token = \"tok\"\n",
                "reasoning_effort = \"low\"\n\n",
                "[extras.extras.projects.\"/old\"]\n",
                "trust_level = \"trusted\"\n",
            ),
        )
        .expect("write fixture");

        super::mutate_config_document(&path, |_| anyhow::Ok(())).expect("mutate heals");

        let healed: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        assert!(
            healed.get("extras").is_none(),
            "tower must be gone: {healed}"
        );
        assert_eq!(
            healed["chatgpt_access_token"].as_str(),
            Some("tok"),
            "trapped scalar lifted to the root"
        );
        assert_eq!(
            healed["reasoning_effort"].as_str(),
            Some("high"),
            "existing top-level values win over shadowed duplicates"
        );
        assert_eq!(
            healed["projects"]["/live"]["trust_level"].as_str(),
            Some("trusted"),
            "live records untouched"
        );
        // The nested projects table was shadowed by the live one at the
        // first lift; healing never merges table contents, only lifts whole
        // missing keys, so the shadowed duplicate is dropped.
    }

    #[test]
    fn healing_recovers_project_tables_when_no_top_level_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            concat!(
                "[extras.extras.extras.projects.\"/old\"]\n",
                "trust_level = \"trusted\"\n",
            ),
        )
        .expect("write fixture");

        super::mutate_config_document(&path, |_| anyhow::Ok(())).expect("mutate heals");

        let healed: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        assert!(healed.get("extras").is_none(), "{healed}");
        assert_eq!(
            healed["projects"]["/old"]["trust_level"].as_str(),
            Some("trusted"),
            "trapped trust record restored: {healed}"
        );
    }

    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    #[test]
    fn malformed_config_diagnostics_never_echo_secret_contents_or_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let secret = "sentinel";
        fs::write(
            &path,
            format!("[providers.xai]\napi_key = \"{secret}\" trailing-junk\n"),
        )
        .expect("seed malformed config");

        let error = mutate_config_document(&path, |_| Ok(())).expect_err("must reject malformed");
        let diagnostic = format!("{error:#}");
        assert!(!diagnostic.contains(secret), "{diagnostic}");
        assert!(!diagnostic.contains("api_key"), "{diagnostic}");
        assert!(
            diagnostic.contains("file contents were omitted"),
            "{diagnostic}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_lock_path_comparison_rejects_unpaired_utf16() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt as _;

        let invalid = PathBuf::from(OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xd800,
        ]));
        assert!(normalize_windows_path_for_comparison(&invalid).is_err());
        assert_eq!(
            normalize_windows_path_for_comparison(Path::new(r"C:\Config\A\config.toml.lock"))
                .unwrap(),
            normalize_windows_path_for_comparison(Path::new(r"C:\Config\a\config.toml.lock"))
                .unwrap(),
            "Windows lock identity must compare case-insensitively"
        );
    }

    #[test]
    fn targeted_mutation_preserves_unknown_provider_data_and_comments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let original = "# operator\n[providers.xai]\nreasoning_stream_style = \"structured\" # keep\nmax_concurrency = 7\ncustom_future = { preserve = true }\n\n[providers.my_private]\nkind = \"openai-compatible\"\napi_key_env = \"PRIVATE_KEY\"\n";
        fs::write(&path, original).expect("seed");

        mutate_config_document(&path, |doc| {
            set_config_document_value(
                doc,
                &["providers", "xai", "external_credentials", "access"],
                "read_only",
            )
        })
        .expect("mutate");

        let saved = fs::read_to_string(path).expect("read");
        for expected in [
            "# operator",
            "reasoning_stream_style = \"structured\" # keep",
            "max_concurrency = 7",
            "custom_future = { preserve = true }",
            "[providers.my_private]",
            "api_key_env = \"PRIVATE_KEY\"",
        ] {
            assert!(saved.contains(expected), "missing {expected:?}:\n{saved}");
        }
    }

    #[test]
    fn shared_lock_makes_revoke_win_without_losing_unrelated_update() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "[providers.xai.external_credentials]\naccess = \"read_only\"\nprovider = \"xai\"\nsource = \"grok_cli\"\npath = \"/external/auth.json\"\nconsent_version = 1\n",
        )
        .expect("seed");
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let revoke_path = path.clone();
        let entered_revoke = Arc::clone(&entered);
        let release_revoke = Arc::clone(&release);
        let revoke = thread::spawn(move || {
            mutate_config_document(&revoke_path, |doc| {
                entered_revoke.wait();
                release_revoke.wait();
                unset_config_document_value(doc, &["providers", "xai", "external_credentials"])?;
                Ok(())
            })
        });
        entered.wait();
        let update_path = path.clone();
        let update = thread::spawn(move || {
            mutate_config_document(&update_path, |doc| {
                set_config_document_value(doc, &["tui", "low_motion"], true)
            })
        });
        release.wait();
        revoke.join().expect("revoke thread").expect("revoke");
        update.join().expect("update thread").expect("update");

        let saved = fs::read_to_string(path).expect("read");
        assert!(!saved.contains("external_credentials"), "{saved}");
        assert!(saved.contains("low_motion = true"), "{saved}");
    }
}
