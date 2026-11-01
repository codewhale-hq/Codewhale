//! Local catalog document loading shared by the `/plugin marketplace add`
//! command and the Runtime API marketplace endpoints (#5311 surface).
//!
//! One loader so both entry points apply the same rules: LOCAL file only
//! (never network), bounded size, no symlink documents, strict per-format
//! parsing, and refusal to persist a document that parsed to nothing but
//! errors. Candidate→install-spec resolution also lives here so the TUI
//! command and the HTTP API can never disagree about what would be fetched.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::parsers::{self, MarketplaceDocument};
use super::store::StoredMarketplaceCatalog;
use super::types::{
    MarketplaceCandidate, MarketplaceCatalogId, MarketplaceFormat, MarketplaceInstallPlan,
    MarketplaceSourceSpec,
};

/// Catalog documents are JSON text; four megabytes is far beyond any real
/// published catalog and caps the parse cost of a user-supplied file.
const MAX_CATALOG_BYTES: u64 = 4 * 1024 * 1024;

/// A parsed catalog ready to store, plus the counts callers render back.
#[derive(Debug)]
pub struct LoadedCatalogDocument {
    pub entry: StoredMarketplaceCatalog,
    pub candidate_count: usize,
    pub warning_count: usize,
}

/// Conservative catalog name: it becomes a key, appears in candidate IDs,
/// and is rendered back to the operator.
#[must_use]
pub fn valid_marketplace_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Read and parse a LOCAL catalog document. Relative paths resolve against
/// `workspace`. No network is touched, here or anywhere in this module.
pub fn load_catalog_document(
    name: &str,
    workspace: &Path,
    raw_path: &str,
) -> Result<LoadedCatalogDocument, String> {
    if !valid_marketplace_name(name) {
        return Err(
            "Marketplace name must be 1-64 characters of letters, digits, `-`, `_`, or `.`"
                .to_string(),
        );
    }
    let path = PathBuf::from(raw_path.trim());
    let path = if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    };
    let path = canonical_document(&path)?;
    let body = read_bounded(&path)?;
    let root = serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|error| format!("Catalog at {} is not valid JSON: {error}", path.display()))?;

    let document = MarketplaceDocument {
        catalog_id: MarketplaceCatalogId::new(name),
        format: MarketplaceFormat::Auto,
        root,
        base: Some(path.display().to_string()),
    };
    let catalog = parsers::parse_catalog(document);

    // A document-level error (unknown/ambiguous format, not-an-object) means
    // nothing useful was parsed; do not persist it.
    if catalog.candidates.is_empty() && catalog.error_count() > 0 {
        return Err(format!(
            "Catalog `{name}` could not be parsed as any known marketplace format (kimi, claude, codex, codewhale):\n{}",
            render_diagnostics_inline(&catalog.diagnostics)
        ));
    }

    let entry = StoredMarketplaceCatalog {
        added_at: chrono::Utc::now().to_rfc3339(),
        source_path: path.display().to_string(),
        catalog,
    };
    Ok(LoadedCatalogDocument {
        candidate_count: entry.catalog.total_candidates(),
        warning_count: entry.catalog.warning_count(),
        entry,
    })
}

/// What installing a stored candidate would do, resolved once for every
/// caller. `Supported.spec` is exactly what the reviewed installer accepts.
pub enum CatalogInstallResolution<'a> {
    Supported {
        spec: String,
        source_kind: String,
    },
    /// A matching name is occupied; catalog metadata does not prove identity.
    AlreadyPresent {
        plugin: &'a crate::types::LoadedPlugin,
        reason: String,
    },
    Unsupported {
        reason: String,
    },
    HasErrors {
        diagnostics: String,
    },
}

/// Resolve a stored catalog candidate to its install spec. Relative local
/// paths resolve against the catalog document's own directory, not the
/// caller's working directory.
pub fn resolve_candidate_install<'a>(
    entry: &StoredMarketplaceCatalog,
    candidate: &MarketplaceCandidate,
    registry: &'a crate::PluginRegistry,
) -> CatalogInstallResolution<'a> {
    if candidate.has_errors() {
        return CatalogInstallResolution::HasErrors {
            diagnostics: render_diagnostics_inline(&candidate.diagnostics),
        };
    }
    if let Some(plugin) = registry.get(&candidate.name) {
        if plugin.scope == crate::types::PluginScope::Builtin {
            // A shipped bundle (Computer Use): the published "install"
            // command must say so, not read as a failure (B5). The catalog
            // entry still never replaces it.
            return CatalogInstallResolution::AlreadyPresent {
                plugin,
                reason: format!(
                    "'{}' is built into Codewhale, so there is nothing to install; this catalog entry does not replace it. Review it with /plugin show {}.",
                    plugin.name(),
                    plugin.id.as_str()
                ),
            };
        }
        return CatalogInstallResolution::AlreadyPresent {
            plugin,
            reason: format!(
                "A {} plugin named '{}' already exists. Review the existing bundle with /plugin show {}. Catalog metadata does not establish that it is the same bundle.",
                plugin.scope.as_str(),
                plugin.name(),
                plugin.id.as_str()
            ),
        };
    }
    match &candidate.install_plan {
        MarketplaceInstallPlan::Supported { spec, source_kind } => match resolve_spec(
            &entry.source_path,
            entry.catalog.format,
            &candidate.source,
            spec,
        ) {
            Ok(spec) => CatalogInstallResolution::Supported {
                spec,
                source_kind: source_kind.clone(),
            },
            Err(reason) => CatalogInstallResolution::Unsupported { reason },
        },
        MarketplaceInstallPlan::Unsupported { reason, .. } => {
            CatalogInstallResolution::Unsupported {
                reason: reason.clone(),
            }
        }
    }
}

/// A catalog's local source must name a directory inside the catalog's own
/// tree. An absolute path, a root or drive prefix, or any `..` component would
/// let catalog metadata point the installer at an arbitrary directory on this
/// machine, so such an entry is not installable from the catalog; installing a
/// directory outside it stays an explicit `/plugin install path:...`.
///
/// Known limitation: the check is textual. It inspects the source's path
/// components and never resolves the filesystem, so a symlink inside the
/// catalog tree (`plugins/foo -> /elsewhere`) still passes, and the install
/// follows it out of the catalog directory.
fn resolve_spec(
    source_path: &str,
    format: MarketplaceFormat,
    source: &MarketplaceSourceSpec,
    spec: &str,
) -> Result<String, String> {
    if let MarketplaceSourceSpec::LocalPath { path } = source
        && !path
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "Local catalog source `{}` leaves the catalog directory; only paths inside it (no absolute paths or `..`) are installable from a catalog",
            path.display()
        ));
    }
    if let MarketplaceSourceSpec::LocalPath { path } = source
        && let Some(dir) = Path::new(source_path).parent()
    {
        // Claude keeps its catalog in a manifest-only metadata directory;
        // relative sources are rooted at the marketplace repository.
        let dir = if format == MarketplaceFormat::Claude
            && dir.file_name().is_some_and(|name| name == ".claude-plugin")
        {
            dir.parent().unwrap_or(dir)
        } else {
            dir
        };
        return Ok(format!("path:{}", dir.join(path).display()));
    }
    // Every other source kind (GitHub, archive/tarball URL) is remote. The
    // installer parses the spec again, and any value it does not recognise
    // as remote is a local directory there — so an archive `url` of `/etc`
    // or `../..` would bypass the containment check above. Only a spec the
    // installer's own parser reads as remote is installable from a catalog.
    match crate::install::PluginInstallSource::parse(spec) {
        Ok(crate::install::PluginInstallSource::Remote(_)) => Ok(spec.to_string()),
        _ => Err(format!(
            "Catalog source `{spec}` is not a remote (github: or http(s)://) source; a catalog may name a local directory only as a path inside the catalog"
        )),
    }
}

/// Resolve a user-supplied document path to an existing regular file without
/// following a final symlink (the document is untrusted input).
fn canonical_document(path: &Path) -> Result<PathBuf, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot read catalog at {}: {e}", path.display()))?;
    if metadata.is_symlink() {
        return Err(format!(
            "Catalog path {} is a symlink; marketplace documents must be regular files",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "Catalog path {} is not a regular file",
            path.display()
        ));
    }
    Ok(path.to_path_buf())
}

fn read_bounded(path: &Path) -> Result<String, String> {
    // Validate the opened handle, not just the path checked before opening.
    let file = crate::registry::open_existing_regular_file(path, false)?
        .ok_or_else(|| format!("Cannot read catalog at {}: file is missing", path.display()))?;
    let mut text = String::new();
    let mut limited = file.take(MAX_CATALOG_BYTES + 1);
    limited
        .read_to_string(&mut text)
        .map_err(|e| format!("Cannot read catalog at {}: {e}", path.display()))?;
    // Check bytes actually read: the file can grow after its metadata is read.
    if text.len() as u64 > MAX_CATALOG_BYTES {
        return Err(format!(
            "Catalog at {} exceeds the {} byte limit",
            path.display(),
            MAX_CATALOG_BYTES
        ));
    }
    Ok(text)
}

fn render_diagnostics_inline(diagnostics: &[super::types::MarketplaceDiagnostic]) -> String {
    use crate::types::PluginDiagnosticLevel;
    diagnostics
        .iter()
        .map(|d| {
            format!(
                "{} {}: {}",
                match d.level {
                    PluginDiagnosticLevel::Error => "error",
                    PluginDiagnosticLevel::Warning => "warning",
                },
                d.code,
                d.message
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_names_are_conservative() {
        assert!(valid_marketplace_name("official"));
        assert!(valid_marketplace_name("My-Catalog_2.beta"));
        assert!(!valid_marketplace_name(""));
        assert!(!valid_marketplace_name("has space"));
        assert!(!valid_marketplace_name("a".repeat(65).as_str()));
    }

    #[test]
    fn catalog_read_enforces_actual_byte_limit() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let body = " ".repeat(MAX_CATALOG_BYTES as usize);
        std::fs::write(&path, &body).unwrap();
        assert_eq!(read_bounded(&path).unwrap(), body);
        let checked = canonical_document(&path).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b" ").unwrap();
        assert!(read_bounded(&checked).unwrap_err().contains("byte limit"));
    }

    #[cfg(unix)]
    #[test]
    fn catalog_read_refuses_symlink_substituted_after_path_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let other = dir.path().join("other.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::write(&other, "synthetic unrelated content").unwrap();
        let checked = canonical_document(&path).unwrap();
        std::fs::rename(&path, dir.path().join("original.json")).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(read_bounded(&checked).is_err());
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            "synthetic unrelated content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn catalog_read_refuses_fifo_without_waiting_for_a_writer() {
        const CHILD_PATH: &str = "CODEWHALE_TEST_CATALOG_FIFO";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            assert!(read_bounded(Path::new(&path)).is_err());
            return;
        }
        // Isolate a regressed blocking open so the test can stop it safely.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        std::fs::write(&path, "{}").unwrap();
        let checked = canonical_document(&path).unwrap();
        std::fs::rename(&path, dir.path().join("original.json")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "plugins::marketplace::document::tests::catalog_read_refuses_fifo_without_waiting_for_a_writer"])
            .env(CHILD_PATH, checked)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("catalog read waited for a FIFO writer");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_symlink_documents() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        std::fs::write(&real, "{}").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let error = load_catalog_document("test", dir.path(), link.to_str().unwrap())
            .expect_err("symlink document must be refused");
        assert!(error.contains("symlink"), "{error}");
    }

    #[test]
    fn load_refuses_unknown_format_documents() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("catalog.json");
        std::fs::write(&doc, r#"{"totally":"unknown"}"#).unwrap();

        let error = load_catalog_document("test", dir.path(), doc.to_str().unwrap())
            .expect_err("unknown format must be refused");
        assert!(error.contains("could not be parsed"), "{error}");
    }
}
