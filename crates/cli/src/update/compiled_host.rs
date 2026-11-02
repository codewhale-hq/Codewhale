//! Optional companion delivery through the existing updater's locked source.
//! This is installation provenance, never a runtime or extension authority.
use super::*;
use codewhale_runtime::delivery_files::GuardedFile;
use std::collections::HashSet;

const CATALOG: &str = "codewhale-extension-hosts.json";
const RECEIPT: &str = "codewhale-extension-host.release.json";

#[derive(serde::Deserialize)]
struct Catalog {
    schema: u64,
    version: String,
    source_sha: String,
    bundle_sha256: String,
    hosts: Vec<Host>,
}

#[derive(serde::Deserialize)]
struct Host {
    target: String,
    asset: String,
    sha256: String,
    notices_asset: String,
    notices_sha256: String,
    source_asset: String,
    source_sha256: String,
    runtime_version: String,
    runtime_revision: String,
    runtime_sha256: String,
    webkit_revision: String,
    bundle_sha256: String,
    source_commit: String,
    native_platform: String,
    native_arch: String,
    libc: String,
    passed: u64,
    failed: u64,
    skipped: u64,
    test_log_sha256: String,
    native_passed: u64,
    native_failed: u64,
    native_skipped: u64,
    native_log_sha256: String,
    license_closure: String,
    relink_source: String,
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn version(value: &str) -> bool {
    let (core, suffix) = value
        .split_once('-')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && suffix.is_none_or(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        })
}

fn native_target(target: &str) -> Option<(&'static str, &'static str)> {
    Some(match target {
        "linux-x64" => ("linux", "x64"),
        "linux-arm64" => ("linux", "arm64"),
        "macos-x64" => ("darwin", "x64"),
        "macos-arm64" => ("darwin", "arm64"),
        "windows-x64" => ("win32", "x64"),
        "windows-arm64" => ("win32", "arm64"),
        _ => return None,
    })
}

fn parse(bytes: &[u8], expected_version: &str) -> Result<Catalog> {
    if bytes.len() > 64 * 1024 {
        bail!("compiled host catalog exceeds 64 KiB");
    }
    let catalog: Catalog =
        serde_json::from_slice(bytes).context("malformed compiled host catalog")?;
    if catalog.schema != 1
        || !version(&catalog.version)
        || catalog.version != expected_version.trim_start_matches('v')
        || !hex(&catalog.source_sha, 40)
        || !hex(&catalog.bundle_sha256, 64)
        || catalog.hosts.len() > 6
    {
        bail!("compiled host catalog does not identify this exact release");
    }
    let mut targets = HashSet::new();
    for host in &catalog.hosts {
        let (platform, arch) = native_target(&host.target)
            .context("unsupported compiled host target; Android is not a qualified Codewhale delivery target")?;
        let minimum_native = if platform == "win32" { 8 } else { 1 };
        let stem = format!("codewhale-extension-host-{}", host.target);
        let binary = format!("{stem}{}", if platform == "win32" { ".exe" } else { "" });
        if !targets.insert(&host.target)
            || host.asset != binary
            || host.notices_asset != format!("{stem}-LICENSES.txt")
            || host.source_asset != format!("{stem}-relink-source.tar.gz")
            || [
                &host.sha256,
                &host.notices_sha256,
                &host.source_sha256,
                &host.runtime_sha256,
                &host.test_log_sha256,
                &host.native_log_sha256,
            ]
            .iter()
            .any(|digest| !hex(digest, 64))
            || !version(&host.runtime_version)
            || !hex(&host.runtime_revision, 40)
            || !hex(&host.webkit_revision, 40)
            || host.bundle_sha256 != catalog.bundle_sha256
            || host.source_commit != catalog.source_sha
            || host.native_platform != platform
            || host.native_arch != arch
            || !(6..=9_007_199_254_740_991).contains(&host.passed)
            || host.failed != 0
            || host.skipped != 0
            || !(minimum_native..=9_007_199_254_740_991).contains(&host.native_passed)
            || host.native_failed != 0
            || host.native_skipped != 0
            || host.license_closure != "complete"
            || host.relink_source != "complete"
            || if platform == "linux" {
                !matches!(host.libc.as_str(), "glibc" | "musl")
            } else {
                host.libc != "none"
            }
        {
            bail!("compiled host is not a complete matching-native qualified payload");
        }
    }
    Ok(catalog)
}

struct Replacement {
    name: String,
    staged: GuardedFile,
    old: Option<GuardedFile>,
    backup_name: String,
    retired: bool,
    published: bool,
}

pub(super) struct Prepared {
    directory: PathBuf,
    replacements: Vec<Replacement>,
}

impl Prepared {
    fn verify_target(directory: &Path, replacement: &mut Replacement) -> Result<()> {
        if let Some(old) = &mut replacement.old {
            old.verify()?;
        } else if GuardedFile::open(directory, &replacement.name)?.is_some() {
            bail!(
                "compiled host destination appeared: {}",
                directory.join(&replacement.name).display()
            );
        }
        replacement.staged.verify()?;
        Ok(())
    }

    pub(super) fn recovery_paths(&self) -> Vec<PathBuf> {
        self.replacements
            .iter()
            .filter(|replacement| replacement.retired)
            .map(|replacement| self.directory.join(&replacement.backup_name))
            .collect()
    }

    pub(super) fn publish(&mut self) -> Result<()> {
        for replacement in &mut self.replacements {
            Self::verify_target(&self.directory, replacement)?;
        }
        for replacement in &mut self.replacements {
            Self::verify_target(&self.directory, replacement)?;
            if let Some(old) = &mut replacement.old {
                let result = old.move_to_vacant(&replacement.backup_name);
                // A Unix name can change between comparison and rename. Record
                // the move even when post-move identity validation rejects it.
                replacement.retired = old.name() == replacement.backup_name;
                result.with_context(|| {
                    format!(
                        "failed to retire {}; recovery at {}",
                        replacement.name,
                        self.directory.join(&replacement.backup_name).display()
                    )
                })?;
            }
            let result = replacement.staged.move_to_vacant(&replacement.name);
            replacement.published = replacement.staged.name() == replacement.name;
            result.with_context(|| {
                format!(
                    "failed to publish {}; no concurrent destination was overwritten",
                    replacement.name
                )
            })?;
        }
        Ok(())
    }

    pub(super) fn rollback(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        for replacement in self.replacements.iter_mut().rev() {
            let restored = (|| {
                if replacement.published {
                    // The retained staged handle, not equal bytes, proves this
                    // entry is the one we published. Preserve changed entries.
                    replacement.staged.verify()?;
                    let recovery = GuardedFile::recovery_name();
                    let result = replacement.staged.move_to_vacant(&recovery);
                    replacement.published = replacement.staged.name() == replacement.name;
                    result?;
                }
                if replacement.retired {
                    // Restore the entry actually moved, even if a racing Unix
                    // replacement made retirement refuse its identity. A fresh
                    // protected handle retains that recovery object's identity.
                    let original = replacement.old.as_mut().context("retired handle missing")?;
                    if original.verify().is_ok() {
                        original.move_to_vacant(&replacement.name)?;
                    } else {
                        // On Unix the name moved may be a racing replacement;
                        // on either platform an observed writer may have changed
                        // the recovery bytes. Capture that actual recovery entry
                        // and restore it only if the old destination is vacant.
                        let mut recovery =
                            GuardedFile::open(&self.directory, &replacement.backup_name)?
                                .context("retired entry disappeared")?;
                        recovery.move_to_vacant(&replacement.name)?;
                    }
                    replacement.retired = false;
                }
                Ok::<_, anyhow::Error>(())
            })();
            if let Err(error) = restored {
                failures.push(format!(
                    "{}: {} (retired entry {}; staged/published entry {})",
                    replacement.name,
                    error,
                    self.directory.join(&replacement.backup_name).display(),
                    self.directory.join(replacement.staged.name()).display()
                ));
            }
        }
        if !failures.is_empty() {
            bail!(
                "compiled host rollback needs attention; recovery bytes preserved: {}",
                failures.join("; ")
            );
        }
        Ok(())
    }
}

#[cfg(any(unix, windows))]
pub(super) fn owned_for_uninstall(directory: &Path, target: &str) -> Result<Vec<GuardedFile>> {
    let Some(mut receipt) = GuardedFile::open_bounded(directory, RECEIPT, 64 * 1024)? else {
        return Ok(Vec::new());
    };
    receipt.verify_owned()?;
    let catalog = parse(&receipt.read_bounded(64 * 1024)?, env!("CARGO_PKG_VERSION"))?;
    let host = catalog
        .hosts
        .iter()
        .find(|host| host.target == target)
        .context("installed compiled-host receipt has no matching target")?;
    let mut files = Vec::new();
    let host_name = format!("codewhale-extension-host{}", std::env::consts::EXE_SUFFIX);
    for (name, expected) in [
        (host_name.as_str(), host.sha256.as_str()),
        (
            "codewhale-extension-host.LICENSES.txt",
            host.notices_sha256.as_str(),
        ),
        (
            "codewhale-extension-host.relink-source.tar.gz",
            host.source_sha256.as_str(),
        ),
    ] {
        if let Some(file) = GuardedFile::open(directory, name)? {
            if file.sha256() != expected {
                bail!(
                    "Preserved changed compiled-host file: {}",
                    directory.join(name).display()
                );
            }
            files.push(file);
        }
    }
    files.push(receipt);
    Ok(files)
}

pub(super) fn required_for(current_exe: &Path) -> Result<bool> {
    let directory = current_exe
        .parent()
        .context("updater executable has no parent")?;
    Ok(
        std::env::var("CODEWHALE_INSTALL_COMPILED_HOST").as_deref() == Ok("1")
            || GuardedFile::open_bounded(directory, RECEIPT, 64 * 1024)?.is_some(),
    )
}

pub(super) fn require_catalog_manifest(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).context("compiled host manifest is not UTF-8")?;
    if !parse_checksum_manifest(text)?.contains_key(CATALOG) {
        bail!("release source has no qualified compiled-host catalog");
    }
    Ok(())
}

pub(super) fn prepare(
    download: &DownloadPlan,
    release_version: &str,
    target: &str,
    current_exe: &Path,
    proxy: Option<&Proxy>,
) -> Result<Option<Prepared>> {
    let directory = current_exe
        .parent()
        .context("updater executable has no parent")?;
    let host_path = directory.join(format!(
        "codewhale-extension-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    let receipt_path = directory.join(RECEIPT);
    let mut receipt_file = GuardedFile::open_bounded(directory, RECEIPT, 64 * 1024)?;
    let requested = std::env::var("CODEWHALE_INSTALL_COMPILED_HOST").as_deref() == Ok("1");
    if receipt_file.is_none() && !requested {
        return Ok(None);
    }
    let mut ownership = HashMap::new();
    if let Some(mut receipt) = receipt_file.take() {
        let old = parse(&receipt.read_bounded(64 * 1024)?, env!("CARGO_PKG_VERSION"))?;
        let entry = old
            .hosts
            .iter()
            .find(|host| host.target == target)
            .context("installed host receipt has no matching target")?;
        for (name, expected) in [
            (
                host_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("invalid host basename")?,
                entry.sha256.as_str(),
            ),
            (
                "codewhale-extension-host.LICENSES.txt",
                entry.notices_sha256.as_str(),
            ),
            (
                "codewhale-extension-host.relink-source.tar.gz",
                entry.source_sha256.as_str(),
            ),
        ] {
            let file = GuardedFile::open(directory, name)?
                .with_context(|| format!("owned compiled host file missing: {name}"))?;
            if file.sha256() != expected {
                bail!("compiled host ownership changed: {name}");
            }
            ownership.insert(directory.join(name), file);
        }
        ownership.insert(receipt_path.clone(), receipt);
    } else if GuardedFile::open(
        directory,
        host_path
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid host basename")?,
    )?
    .is_some()
    {
        bail!(
            "compiled host beside this CLI has no installation receipt; it was not replaced. Install into a fresh prefix or use Node"
        );
    }
    let fetch = |name: &str| -> Result<Vec<u8>> {
        let url = reqwest::Url::parse(&download.binary_url)?.join(name)?;
        let bytes = download_url(url.as_str(), proxy)?;
        verify_manifest_asset(download, name, &bytes)?;
        Ok(bytes)
    };
    if !download.checksums.contains_key(CATALOG) {
        bail!(
            "compiled host requested/installed but the selected release source has no qualified catalog; no files were changed. Node remains available"
        );
    }
    let catalog_bytes = fetch(CATALOG)?;
    let catalog = parse(&catalog_bytes, release_version)?;
    let host = catalog
        .hosts
        .iter()
        .find(|host| host.target == target)
        .context("release has no qualified compiled image for this target; use Node")?;
    let payloads = [
        (host.asset.as_str(), host_path, host.sha256.as_str(), true),
        (
            host.notices_asset.as_str(),
            directory.join("codewhale-extension-host.LICENSES.txt"),
            host.notices_sha256.as_str(),
            false,
        ),
        (
            host.source_asset.as_str(),
            directory.join("codewhale-extension-host.relink-source.tar.gz"),
            host.source_sha256.as_str(),
            false,
        ),
        (
            CATALOG,
            receipt_path,
            download
                .checksums
                .get(CATALOG)
                .context("catalog checksum missing")?
                .as_str(),
            false,
        ),
    ];
    let mut replacements = Vec::new();
    for (name, path, expected, executable) in payloads {
        let bytes = if name == CATALOG {
            catalog_bytes.clone()
        } else {
            fetch(name)?
        };
        if sha256_hex(&bytes) != expected {
            bail!("compiled host catalog and manifest disagree for {name}");
        }
        if executable && cfg!(target_os = "linux") {
            let required = highest_required_glibc(&bytes);
            let available = detect_host_glibc();
            if (host.libc == "glibc" && available.is_none())
                || required
                    .is_some_and(|required| available.is_none_or(|available| available < required))
                || (host.libc == "musl" && required.is_some())
            {
                bail!(
                    "optional compiled Bun image has an incompatible libc floor; the CLI remains static musl. Use Node"
                );
            }
        }
        let old = ownership.remove(&path);
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid compiled payload basename")?
            .to_owned();
        if old.is_none() && GuardedFile::open(directory, &filename)?.is_some() {
            bail!("refusing unowned compiled host file {}", path.display());
        }
        let staged = GuardedFile::stage(directory, &bytes, executable)?;
        if staged.sha256() != expected {
            bail!("compiled host stage changed: {}", staged.name());
        }
        replacements.push(Replacement {
            name: filename,
            staged,
            old,
            backup_name: GuardedFile::recovery_name(),
            retired: false,
            published: false,
        });
    }
    Ok(Some(Prepared {
        directory: directory.to_path_buf(),
        replacements,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> serde_json::Value {
        serde_json::json!({
            "schema":1,"version":"0.10.1","source_sha":"b".repeat(40),"bundle_sha256":"a".repeat(64),
            "hosts":[{
                "target":"linux-x64","asset":"codewhale-extension-host-linux-x64","sha256":"c".repeat(64),
                "notices_asset":"codewhale-extension-host-linux-x64-LICENSES.txt","notices_sha256":"d".repeat(64),
                "source_asset":"codewhale-extension-host-linux-x64-relink-source.tar.gz","source_sha256":"e".repeat(64),
                "runtime_version":"1.4.0","runtime_revision":"f".repeat(40),"runtime_sha256":"1".repeat(64),"webkit_revision":"2".repeat(40),
                "bundle_sha256":"a".repeat(64),"source_commit":"b".repeat(40),"native_platform":"linux","native_arch":"x64","libc":"glibc",
                "passed":6,"failed":0,"skipped":0,"test_log_sha256":"3".repeat(64),"native_passed":1,"native_failed":0,"native_skipped":0,"native_log_sha256":"4".repeat(64),"license_closure":"complete","relink_source":"complete"
            }]
        })
    }

    #[test]
    fn catalog_refuses_cross_native_or_skipped_containment_and_mixed_release() {
        let valid = receipt();
        assert!(parse(&serde_json::to_vec(&valid).unwrap(), "v0.10.1").is_ok());
        for (key, value) in [
            ("native_skipped", serde_json::json!(1)),
            ("native_arch", serde_json::json!("arm64")),
            ("native_passed", serde_json::json!(0)),
            ("source_asset", serde_json::json!("../source.tar.gz")),
            ("relink_source", serde_json::json!("pending")),
        ] {
            let mut invalid = valid.clone();
            invalid["hosts"][0][key] = value;
            assert!(
                parse(&serde_json::to_vec(&invalid).unwrap(), "0.10.1").is_err(),
                "{key}"
            );
        }
        assert!(parse(&serde_json::to_vec(&valid).unwrap(), "0.10.2").is_err());
    }

    #[test]
    fn catalog_requires_all_eight_current_windows_native_cases() {
        let mut value = receipt();
        let host = &mut value["hosts"][0];
        for (key, field) in [
            ("target", "windows-x64"),
            ("asset", "codewhale-extension-host-windows-x64.exe"),
            (
                "notices_asset",
                "codewhale-extension-host-windows-x64-LICENSES.txt",
            ),
            (
                "source_asset",
                "codewhale-extension-host-windows-x64-relink-source.tar.gz",
            ),
            ("native_platform", "win32"),
            ("libc", "none"),
        ] {
            host[key] = serde_json::json!(field);
        }
        host["native_passed"] = serde_json::json!(7);
        assert!(parse(&serde_json::to_vec(&value).unwrap(), "0.10.1").is_err());
        value["hosts"][0]["native_passed"] = serde_json::json!(8);
        assert!(parse(&serde_json::to_vec(&value).unwrap(), "0.10.1").is_ok());
    }

    fn prepared(directory: &Path, name: &str, old: Option<&[u8]>) -> Prepared {
        if let Some(bytes) = old {
            std::fs::write(directory.join(name), bytes).unwrap();
        }
        Prepared {
            directory: directory.to_path_buf(),
            replacements: vec![Replacement {
                name: name.into(),
                staged: GuardedFile::stage(directory, b"qualified", true).unwrap(),
                old: GuardedFile::open(directory, name).unwrap(),
                backup_name: GuardedFile::recovery_name(),
                retired: false,
                published: false,
            }],
        }
    }

    #[test]
    fn publish_retires_no_companion_if_a_destination_changed() {
        let directory = tempfile::tempdir().unwrap();
        let mut prepared = prepared(directory.path(), "host", None);
        std::fs::write(directory.path().join("host"), b"foreign").unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(
            std::fs::read(directory.path().join("host")).unwrap(),
            b"foreign"
        );
    }

    #[cfg(unix)]
    #[test]
    fn same_byte_inode_swap_cannot_establish_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let mut prepared = prepared(directory.path(), "host", Some(b"old"));
        std::fs::rename(
            directory.path().join("host"),
            directory.path().join("original"),
        )
        .unwrap();
        std::fs::write(directory.path().join("host"), b"old").unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(
            std::fs::read(directory.path().join("host")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(directory.path().join("original")).unwrap(),
            b"old"
        );
    }

    #[cfg(unix)]
    #[test]
    fn changed_writer_refuses_retirement_and_preserves_original() {
        let directory = tempfile::tempdir().unwrap();
        let mut prepared = prepared(directory.path(), "host", Some(b"old"));
        std::fs::write(directory.path().join("host"), b"concurrent").unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(
            std::fs::read(directory.path().join("host")).unwrap(),
            b"concurrent"
        );
    }

    #[test]
    fn successful_publication_and_rollback_preserve_both_versions() {
        let directory = tempfile::tempdir().unwrap();
        let mut prepared = prepared(directory.path(), "host", Some(b"old"));
        prepared.publish().unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("host")).unwrap(),
            b"qualified"
        );
        prepared.rollback().unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("host")).unwrap(),
            b"old"
        );
        let recovered = directory
            .path()
            .join(prepared.replacements[0].staged.name());
        assert_eq!(std::fs::read(recovered).unwrap(), b"qualified");
    }

    #[cfg(unix)]
    #[test]
    fn partial_rollback_preserves_changed_published_entry_and_recovers_other_slot() {
        let directory = tempfile::tempdir().unwrap();
        let mut first = prepared(directory.path(), "host", Some(b"old-host"));
        let mut second = prepared(directory.path(), "notices", Some(b"old-notices"));
        first.replacements.append(&mut second.replacements);
        first.publish().unwrap();
        let backup = directory.path().join(&first.replacements[0].backup_name);
        std::fs::rename(
            directory.path().join("host"),
            directory.path().join("published-original"),
        )
        .unwrap();
        // Same bytes must still fail identity; a digest-only rollback deletes it.
        std::fs::write(directory.path().join("host"), b"qualified").unwrap();
        let error = first.rollback().unwrap_err().to_string();
        assert!(error.contains("recovery bytes preserved"));
        assert_eq!(
            std::fs::read(directory.path().join("host")).unwrap(),
            b"qualified"
        );
        assert_eq!(std::fs::read(backup).unwrap(), b"old-host");
        assert_eq!(
            std::fs::read(directory.path().join("notices")).unwrap(),
            b"old-notices"
        );
    }
}
