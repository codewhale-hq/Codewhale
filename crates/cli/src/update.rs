//! Self-update for the `codewhale` binary.
//!
//! The `update` subcommand fetches the latest release from
//! `github.com/codewhale-hq/CodeWhale/releases/latest`, downloads the
//! platform-correct binary, verifies its SHA256 checksum, and atomically
//! replaces the currently running binary.

use std::cmp::Ordering;
use std::collections::HashMap;
#[cfg(target_os = "android")]
use std::ffi::CStr;
#[cfg(any(target_os = "android", all(test, unix)))]
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use codewhale_release::install::GITHUB_MIGRATION_HELP;
use codewhale_release::{
    CHECKSUM_MANIFEST_ASSET, InstallMethod, ReleaseChannel, ReleaseQuery, UPDATE_USER_AGENT,
    cnb_mirror_override_active, cnb_mirror_supports_target, cnb_release_base_url,
    compare_release_versions, is_beta_tag, mirror_asset_url, resolve_release_query,
    update_is_needed, update_network_fallback_hint,
};
use reqwest::Proxy;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use crate::hashing::sha256_hex;
use codewhale_release::signing::{self, SIGNATURE_ASSET, SignatureCheck};
use codewhale_release::{autoupdate, installs};

mod compiled_host;
#[cfg(any(unix, windows))]
pub(crate) mod uninstall;

const GITHUB_LATEST_RELEASE_PAGE_URL: &str =
    "https://github.com/codewhale-hq/CodeWhale/releases/latest";
const GITHUB_RELEASE_DOWNLOAD_BASE_URL: &str =
    "https://github.com/codewhale-hq/CodeWhale/releases/download";
const UPDATE_HTTP_ATTEMPTS: usize = 3;
const UPDATE_HTTP_RETRY_DELAY_MS: u64 = 100;
/// Ceiling for one asset download. Generous, because release binaries are tens
/// of megabytes and some of the networks this exists for are slow.
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Ceiling for one checksum-manifest probe. The manifest is a few hundred
/// bytes, so this is only a backstop against a source that accepts the
/// connection and then stalls. GitHub gets the first attempt; an unavailable
/// manifest falls back to the supported mirror without waiting for a binary.
const MANIFEST_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const STAGED_MANIFEST: &str = "codewhale-artifacts-sha256.txt";
const STAGED_SIGNATURE: &str = "codewhale-artifacts-sha256.txt.sig";
#[cfg(target_os = "android")]
const ANDROID_PROC_SELF_MAPS: &str = "/proc/self/maps";

/// Run the self-update workflow.
///
/// OpenHarmony (HarmonyOS) won't compile this file, so no need to handle
pub fn run_update(beta: bool, check_only: bool, proxy_arg: Option<String>) -> Result<()> {
    let executable_identity = update_executable_identity()?;
    let current_exe = executable_identity.path.clone();
    let install_method = InstallMethod::detect(&current_exe);
    let protected_location = protected_update_path(&current_exe);
    let legacy_binary = is_legacy_binary(&current_exe);
    ensure_supported_release_target(std::env::consts::OS, std::env::consts::ARCH)?;

    let plan = update_plan_for_exe(&current_exe);
    let channel = ReleaseChannel::from_beta_flag(beta);
    let current_version = env!("CARGO_PKG_VERSION");
    let build_version = env!("CODEWHALE_BUILD_VERSION");
    let unreleased_checkout_build = build_version.ends_with(" (dev)");
    let proxy = proxy_arg
        .as_deref()
        .map(validate_and_build_proxy)
        .transpose()?;

    println!("Checking for {} updates...", channel.label());
    println!("Current binary: {}", current_exe.display());
    println!("Current version: v{build_version}");
    if legacy_binary {
        println!();
        println!("{}", legacy_binary_message(&current_exe));
    }
    if let Some(warning) = managed_install_warning(install_method) {
        println!();
        println!("{warning}");
        if !check_only {
            bail!("The package-managed executable was not changed.");
        }
    }
    if protected_location {
        println!(
            "System/package directory: in-place self-update is disabled.\n\n{GITHUB_MIGRATION_HELP}"
        );
        if !check_only {
            bail!("The system/package executable was not changed.");
        }
    }

    if check_only {
        let fetched = fetch_latest_release(channel, proxy.as_ref())
            .with_context(update_network_fallback_hint)?;
        let latest_tag = &fetched.release.tag_name;
        println!("Latest {} release: {latest_tag}", channel.label());
        let replace_same_version = unreleased_checkout_build
            && compare_release_versions(current_version, latest_tag)? == Ordering::Equal;
        if update_is_needed(channel, current_version, latest_tag)? || replace_same_version {
            if replace_same_version {
                println!(
                    "This build is not a published release ({build_version}); the {latest_tag} release artifact replaces it."
                );
            }
            if install_method.supports_self_update() && !protected_location {
                println!(
                    "Update available. Run `{} update` to install {latest_tag}.",
                    current_exe.display()
                );
            } else if !install_method.supports_self_update() {
                println!(
                    "Update available. Use the GitHub installation instructions above, or `{}` for this package-managed copy.",
                    install_method.update_command()
                );
            } else {
                println!("Update available. Use the GitHub installation instructions above.");
            }
            println!(
                "Release source: {}",
                describe_release_source_for_check(&fetched, &plan.asset_stem, proxy.as_ref())
            );
        } else {
            match compare_release_versions(current_version, latest_tag)? {
                Ordering::Greater => {
                    println!("Current build is newer than the latest published release.");
                }
                Ordering::Less | Ordering::Equal => {
                    println!("Already up to date.");
                }
            }
        }
        return Ok(());
    }

    // Step 1: Fetch latest release metadata
    let fetched =
        fetch_latest_release(channel, proxy.as_ref()).with_context(update_network_fallback_hint)?;
    let release = &fetched.release;
    let latest_tag = &release.tag_name;
    println!("Latest {} release: {latest_tag}", channel.label());

    if fetched.source.is_pinned_mirror() && channel == ReleaseChannel::Beta {
        println!(
            "Using {}; --beta does not select GitHub beta releases in mirror mode.",
            fetched.source.describe()
        );
    }
    let replace_same_version = unreleased_checkout_build
        && compare_release_versions(current_version, latest_tag)? == Ordering::Equal;
    if !update_is_needed(channel, current_version, latest_tag)? && !replace_same_version {
        if compare_release_versions(current_version, latest_tag)? == Ordering::Greater {
            println!(
                "Current build is newer than the latest published release; keeping v{current_version}. No downgrade or download performed."
            );
        } else {
            println!("Already up to date; no download needed.");
        }
        return Ok(());
    }
    if replace_same_version {
        println!(
            "This build is not a published release ({build_version}); installing the {latest_tag} release artifact over it."
        );
    }

    let divergent = divergent_aliases(&plan, &executable_identity);
    for target in &plan.target_paths {
        if !divergent.contains(target) {
            validate_update_target(target, &executable_identity)?;
        }
    }

    let download = resolve_download_plan(
        &fetched,
        &plan.asset_stem,
        proxy.as_ref(),
        compiled_host::required_for(&current_exe)?,
    )?;
    println!("Release source: {}", download.source.describe());
    let signature = fetch_signature(&download, latest_tag, proxy.as_ref())?;

    println!("Downloading {}...", download.binary_name);
    let bytes = download_pinned_binary(&download, proxy.as_ref())?;

    preflight_downloaded_binary(&download.binary_name, &bytes)?;

    println!(
        "SHA256 checksum verified against {CHECKSUM_MANIFEST_ASSET} from {}{}.",
        download.source.label(),
        if signature.is_verified() {
            "; release signature verified"
        } else {
            ""
        }
    );

    let mut host_update = compiled_host::prepare(
        &download,
        latest_tag,
        plan.asset_stem.trim_start_matches("codewhale-"),
        &current_exe,
        proxy.as_ref(),
    )?;
    let replaced =
        replace_command_paths(&executable_identity, &plan, &bytes, &divergent, &mut || {
            if let Some(host) = &mut host_update {
                host.publish()?;
            }
            Ok(())
        });
    if let Err(error) = replaced {
        if let Some(host) = &mut host_update {
            host.rollback()
                .context("compiled-host rollback failed; retained backup path is reported below")?;
        }
        return Err(error);
    }
    if let Some(host) = &host_update {
        println!(
            "Updated the qualified compiled image, notices and relink source beside this CLI; Node remains default."
        );
        for path in host.recovery_paths() {
            println!("Previous companion bytes retained at {}", path.display());
        }
    }
    if let Ok(home) = codewhale_config::codewhale_home() {
        autoupdate::discard_staged(&home);
        let mut state = autoupdate::load_state(&home);
        if state.skip_version.take().is_some() {
            let _ = autoupdate::store_state(&home, &state);
        }
    }

    println!(
        "\n✅ Successfully updated to {latest_tag}!\n\
         Release source: {source}\n\
         Updated binaries:\n{targets}\n\
         \n\
         Restart the application to use the new version.\n\
         What's new: {notes}\n\
         Undo: codewhale update --rollback",
        source = download.source.describe(),
        notes = autoupdate::changelog_url(latest_tag),
        targets = plan
            .target_paths
            .iter()
            .map(|path| format!("  - {} ({})", path.display(), download.binary_name))
            .collect::<Vec<_>>()
            .join("\n")
    );

    Ok(())
}

fn fetch_signature(
    download: &DownloadPlan,
    tag: &str,
    proxy: Option<&Proxy>,
) -> Result<SignatureCheck> {
    if signing::trusted_keys().is_empty() {
        return Ok(SignatureCheck::NotConfigured);
    }
    let signature = download_url_with_timeout(
        &format!("{}.sig", download.manifest_url),
        proxy,
        MANIFEST_PROBE_TIMEOUT,
    )
    .with_context(|| {
        format!(
            "{SIGNATURE_ASSET} could not be fetched from {}; this build requires signed releases, so nothing was installed",
            download.source.describe()
        )
    })?;
    match signing::verify_manifest(tag, &download.manifest_bytes, Some(&signature)) {
        SignatureCheck::Verified => Ok(SignatureCheck::Verified),
        other => bail!(
            "{SIGNATURE_ASSET} from {} does not verify for {tag} ({other:?}); nothing was installed",
            download.source.describe()
        ),
    }
}

fn divergent_aliases(plan: &UpdatePlan, identity: &UpdateExecutableIdentity) -> Vec<PathBuf> {
    plan.target_paths
        .iter()
        .filter(|target| **target != identity.path)
        .filter(|target| {
            let Ok(meta) = std::fs::symlink_metadata(target) else {
                return false;
            };
            meta.is_file()
                && same_owner(&meta, &identity.path)
                && InstallMethod::from_path(target).supports_self_update()
                && !protected_update_path(target)
                && std::fs::read(target).is_ok_and(|bytes| sha256_hex(&bytes) != identity.file_hash)
                && installs::is_codewhale_binary(target)
        })
        .cloned()
        .collect()
}

#[cfg(unix)]
fn same_owner(meta: &std::fs::Metadata, reference: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(reference).is_ok_and(|reference| reference.uid() == meta.uid())
}

#[cfg(not(unix))]
fn same_owner(_meta: &std::fs::Metadata, _reference: &Path) -> bool {
    true
}

fn save_previous(home: &Path, identity: &UpdateExecutableIdentity) -> Result<()> {
    let bytes = std::fs::read(&identity.path).context("failed to read the running executable")?;
    if sha256_hex(&bytes) != identity.file_hash {
        bail!("the running executable changed before it could be kept for rollback");
    }
    let dir = autoupdate::previous_dir(home);
    installs::write_file_atomically(&dir, &autoupdate::binary_name(), &bytes, true)?;
    autoupdate::store_json(
        &dir.join("previous.json"),
        &autoupdate::PreviousBinary {
            version: env!("CARGO_PKG_VERSION").to_string(),
            sha256: identity.file_hash.clone(),
        },
    )
}

fn replace_command_paths(
    identity: &UpdateExecutableIdentity,
    plan: &UpdatePlan,
    bytes: &[u8],
    divergent: &[PathBuf],
    before_replace: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let home = codewhale_config::codewhale_home().ok();
    #[cfg(unix)]
    let mut command_receipt = uninstall::CommandReceipt::capture(&identity.path)?;
    #[cfg(unix)]
    let had_receipt = command_receipt.is_some();
    #[cfg(windows)]
    let had_receipt = uninstall::CommandReceipt::capture(&identity.path)?.is_some();
    match &home {
        Some(home) => save_previous(home, identity)
            .context("could not keep the current version for `codewhale update --rollback`; nothing was replaced")?,
        None => println!("Codewhale home is unavailable; `codewhale update --rollback` will not have this version."),
    }
    if !divergent.is_empty() {
        let home = home
            .as_deref()
            .context("cannot resolve the Codewhale home for the recovery folder")?;
        let mut batch = installs::RecoveryBatch::open(home)?;
        for path in divergent {
            validate_primary_update_identity(identity)?;
            let moved = batch.move_in(path)?;
            println!(
                "Bringing {} in line with this release; the old copy is kept in {} for {} days. Undo: {}",
                path.display(),
                batch.dir.display(),
                installs::RECOVERY_RETENTION_DAYS,
                moved.undo_command()
            );
        }
        batch.finish();
    }
    let replaced = (|| {
        validate_primary_update_identity(identity)?;
        before_replace()?;
        replace_verified_downloads(&plan.target_paths, bytes, |target| {
            validate_primary_update_identity(identity)?;
            validate_update_target(target, identity)
        })
    })();
    #[cfg(unix)]
    let receipt_renewal = command_receipt
        .as_mut()
        .map(|receipt| receipt.renew(bytes))
        .transpose();
    #[cfg(windows)]
    let receipt_renewal = if had_receipt {
        uninstall::CommandReceipt::capture_accepting(&identity.path, Some(&sha256_hex(bytes)))
            .and_then(|receipt| {
                receipt
                    .context("command ownership receipt disappeared during update")?
                    .renew(bytes)
            })
            .map(Some)
    } else {
        Ok(None)
    };
    if let Err(error) = replaced {
        #[cfg(any(unix, windows))]
        if let Err(renewal) = receipt_renewal {
            return Err(error.context(format!(
                "command ownership receipt could not be renewed after partial update: {renewal:#}"
            )));
        }
        return Err(error);
    }
    #[cfg(any(unix, windows))]
    receipt_renewal.context("binaries updated, but command ownership receipt renewal failed; preserved receipt/recovery paths require attention")?;
    #[cfg(any(unix, windows))]
    if !had_receipt {
        adopt_receipt_for(&identity.path, bytes);
    }
    Ok(())
}

#[cfg(any(unix, windows))]
fn adopt_receipt_for(executable: &Path, bytes: &[u8]) {
    let Some(directory) = executable.parent() else {
        return;
    };
    let directory = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    let digest = sha256_hex(bytes);
    let entries: Vec<(String, String)> = uninstall::COMMANDS
        .iter()
        .filter(|name| {
            installs::file_sha256(&directory.join(name)).is_ok_and(|hash| hash == digest)
        })
        .map(|name| ((*name).to_string(), digest.clone()))
        .collect();
    if entries.is_empty() {
        return;
    }
    match installs::adopt_receipt(&directory, &entries) {
        Ok(true) => println!(
            "Recorded an install receipt in {}; `codewhale uninstall` can now remove this copy.",
            directory.display()
        ),
        Ok(false) => {}
        Err(error) => println!("Could not record an install receipt: {error:#}"),
    }
}

pub fn run_stage(beta: bool, proxy_arg: Option<String>) -> Result<()> {
    let identity = update_executable_identity()?;
    let exe = identity.path.clone();
    if !InstallMethod::detect(&exe).supports_self_update() || protected_update_path(&exe) {
        bail!("This copy is not a release-binary install; nothing was staged.");
    }
    if compiled_host::required_for(&exe)? {
        bail!(
            "Background updates skip installs with the optional compiled host; run `codewhale update`."
        );
    }
    ensure_supported_release_target(std::env::consts::OS, std::env::consts::ARCH)?;
    let home = codewhale_config::codewhale_home()?;
    let mut state = autoupdate::load_state(&home);
    state.last_stage_attempt_unix = codewhale_release::check::now_unix();
    let _ = autoupdate::store_state(&home, &state);
    let channel = ReleaseChannel::from_beta_flag(beta);
    let proxy = proxy_arg
        .as_deref()
        .map(validate_and_build_proxy)
        .transpose()?;
    let fetched =
        fetch_latest_release(channel, proxy.as_ref()).with_context(update_network_fallback_hint)?;
    let tag = fetched.release.tag_name.clone();
    let version = tag.trim_start_matches('v').to_string();
    state.last_stage_version = Some(version.clone());
    let _ = autoupdate::store_state(&home, &state);
    if !update_is_needed(channel, env!("CARGO_PKG_VERSION"), &tag)? {
        autoupdate::discard_staged(&home);
        println!("Already up to date; nothing staged.");
        return Ok(());
    }
    if state.skip_version.as_deref() == Some(version.as_str()) {
        println!("v{version} was rolled back on this machine; it is not staged again.");
        return Ok(());
    }
    if let Some(staged) = autoupdate::load_staged(&home)
        && staged.version == version
        && installs::file_sha256(&autoupdate::staged_dir(&home).join(autoupdate::binary_name()))
            .is_ok_and(|hash| hash == staged.sha256)
    {
        println!("v{version} is already staged.");
        return Ok(());
    }
    let plan = update_plan_for_exe(&exe);
    let download = resolve_download_plan(&fetched, &plan.asset_stem, proxy.as_ref(), false)?;
    let signature = fetch_signature(&download, &tag, proxy.as_ref())?;
    let bytes = download_pinned_binary(&download, proxy.as_ref())?;
    preflight_downloaded_binary(&download.binary_name, &bytes)?;
    autoupdate::discard_staged(&home);
    let dir = autoupdate::staged_dir(&home);
    installs::write_file_atomically(&dir, &autoupdate::binary_name(), &bytes, true)?;
    installs::write_file_atomically(&dir, STAGED_MANIFEST, &download.manifest_bytes, false)?;
    if signature.is_verified() {
        let signature_bytes = download_url_with_timeout(
            &format!("{}.sig", download.manifest_url),
            proxy.as_ref(),
            MANIFEST_PROBE_TIMEOUT,
        )?;
        installs::write_file_atomically(&dir, STAGED_SIGNATURE, &signature_bytes, false)?;
    }
    let target_dir = exe
        .parent()
        .context("executable has no parent")?
        .canonicalize()?;
    autoupdate::store_staged(
        &home,
        &autoupdate::StagedUpdate {
            version: version.clone(),
            sha256: sha256_hex(&bytes),
            signed: signature.is_verified(),
            source: download.source.describe(),
            target_dir,
            declined: false,
            staged_at_unix: codewhale_release::check::now_unix(),
        },
    )?;
    println!(
        "Staged v{version} from {} ({}). {}",
        download.source.describe(),
        if signature.is_verified() {
            "signature verified"
        } else {
            "checksum verified; releases are not signed yet"
        },
        if signature.is_verified() {
            "It is applied the next time Codewhale starts."
        } else {
            "The next start asks before applying it."
        }
    );
    Ok(())
}

pub fn apply_staged_at_start(argv: &[std::ffi::OsString]) {
    use std::io::IsTerminal;
    let Ok(home) = codewhale_config::codewhale_home() else {
        return;
    };
    let Some(mut staged) = autoupdate::load_staged(&home) else {
        return;
    };
    let current = env!("CARGO_PKG_VERSION");
    if compare_release_versions(current, &staged.version).ok() != Some(Ordering::Less)
        || autoupdate::load_state(&home).skip_version.as_deref() == Some(staged.version.as_str())
    {
        autoupdate::discard_staged(&home);
        return;
    }
    if autoupdate::resolve_mode(None, true).mode != autoupdate::UpdateMode::Auto {
        return;
    }
    let Ok(identity) = update_executable_identity() else {
        return;
    };
    if !InstallMethod::detect(&identity.path).supports_self_update()
        || protected_update_path(&identity.path)
        || identity
            .path
            .parent()
            .and_then(|dir| dir.canonicalize().ok())
            .as_deref()
            != Some(staged.target_dir.as_path())
    {
        return;
    }
    let version = staged.version.clone();
    let fail = |reason: String| {
        eprintln!(
            "Couldn't apply v{version} ({reason}). Still on v{current}. Details: codewhale doctor"
        );
    };
    let dir = autoupdate::staged_dir(&home);
    let verified = (|| -> Result<(Vec<u8>, bool)> {
        let bytes = std::fs::read(dir.join(autoupdate::binary_name()))
            .context("the downloaded file is unreadable")?;
        let manifest = std::fs::read(dir.join(STAGED_MANIFEST))
            .context("the downloaded checksum manifest is missing")?;
        let signature = std::fs::read(dir.join(STAGED_SIGNATURE)).ok();
        let signed = match signing::verify_manifest(&version, &manifest, signature.as_deref()) {
            SignatureCheck::Verified => true,
            SignatureCheck::NotConfigured => false,
            other => bail!("the release signature does not verify ({other:?})"),
        };
        let asset = release_asset_name_for_prefix(
            "codewhale",
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
        let checksums = parse_checksum_manifest(
            std::str::from_utf8(&manifest).context("the checksum manifest is not UTF-8")?,
        )?;
        let expected = checksums
            .get(&asset)
            .with_context(|| format!("the checksum manifest does not list {asset}"))?;
        if !sha256_hex(&bytes).eq_ignore_ascii_case(expected) {
            bail!("the downloaded file does not match its checksum manifest");
        }
        Ok((bytes, signed))
    })();
    let (bytes, signed) = match verified {
        Ok(verified) => verified,
        Err(error) => {
            autoupdate::discard_staged(&home);
            return fail(format!("{error:#}"));
        }
    };
    if !signed {
        if staged.declined || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            return;
        }
        eprint!(
            "Codewhale v{version} is downloaded and its checksum verified. Releases are not signed yet, so it waits for your OK.\nInstall v{version} now? [y/N] "
        );
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let accepted = std::io::stdin().read_line(&mut answer).is_ok()
            && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
        if !accepted {
            staged.declined = true;
            let _ = autoupdate::store_staged(&home, &staged);
            eprintln!("Kept v{current}. Run `codewhale update` whenever you want v{version}.");
            return;
        }
    }
    let plan = update_plan_for_exe(&identity.path);
    let divergent = divergent_aliases(&plan, &identity);
    let applied = (|| {
        for target in &plan.target_paths {
            if !divergent.contains(target) {
                validate_update_target(target, &identity)?;
            }
        }
        replace_command_paths(&identity, &plan, &bytes, &divergent, &mut || Ok(()))
    })();
    if let Err(error) = applied {
        return fail(format!("{error:#}"));
    }
    autoupdate::discard_staged(&home);
    eprintln!(
        "Updated to v{version} · What's new: {} · Undo: codewhale update --rollback",
        autoupdate::changelog_url(&version)
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(&identity.path)
            .args(argv.iter().skip(1))
            .exec();
        eprintln!("Restart Codewhale to run v{version} ({error}).");
        std::process::exit(0);
    }
    #[cfg(not(unix))]
    {
        let _ = argv;
        eprintln!("v{version} runs from the next start; this session continues on v{current}.");
    }
}

pub fn run_rollback() -> Result<()> {
    let identity = update_executable_identity()?;
    let method = InstallMethod::detect(&identity.path);
    if let Some(warning) = managed_install_warning(method) {
        println!("{warning}");
        bail!("The package-managed executable was not changed.");
    }
    if protected_update_path(&identity.path) {
        bail!("System/package directory: rollback is disabled here.");
    }
    let home = codewhale_config::codewhale_home()?;
    let previous = autoupdate::load_previous(&home)
        .context("There is no previous version to roll back to on this machine.")?;
    let bytes = std::fs::read(autoupdate::previous_dir(&home).join(autoupdate::binary_name()))
        .context("The previous version's binary is missing; nothing was changed.")?;
    if sha256_hex(&bytes) != previous.sha256 {
        bail!(
            "The kept previous version no longer matches its recorded checksum; nothing was changed."
        );
    }
    if previous.sha256 == identity.file_hash {
        println!("v{} is already the running version.", previous.version);
        return Ok(());
    }
    preflight_downloaded_binary(&autoupdate::binary_name(), &bytes)?;
    let current = env!("CARGO_PKG_VERSION");
    let plan = update_plan_for_exe(&identity.path);
    for target in &plan.target_paths {
        validate_update_target(target, &identity)?;
    }
    println!(
        "Rolling back {} from v{current} to v{}...",
        identity.path.display(),
        previous.version
    );
    replace_command_paths(&identity, &plan, &bytes, &[], &mut || Ok(()))?;
    let mut state = autoupdate::load_state(&home);
    state.skip_version = Some(current.to_string());
    autoupdate::store_state(&home, &state)?;
    autoupdate::discard_staged(&home);
    println!(
        "Rolled back to v{}. v{current} will not be installed automatically again; the next newer release will. Undo: codewhale update --rollback",
        previous.version
    );
    Ok(())
}

/// Fail closed when the downloaded bytes do not match the manifest that came
/// from the same source. A mismatch is never a reason to install anyway, and
/// never a reason to retry against the source that lost the probe: the two
/// build their own artifacts, so their checksums are not interchangeable.
fn verify_downloaded_asset(download: &DownloadPlan, bytes: &[u8]) -> Result<()> {
    verify_manifest_asset(download, &download.binary_name, bytes)
}

fn verify_manifest_asset(download: &DownloadPlan, name: &str, bytes: &[u8]) -> Result<()> {
    let expected = download.checksums.get(name).with_context(|| {
        format!(
            "{CHECKSUM_MANIFEST_ASSET} from {} is missing {name}",
            download.source.describe()
        )
    })?;
    let actual = sha256_hex(bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        bail!(
            "SHA256 mismatch for {name} from {}!\n  expected: {expected}\n  actual:   {actual}",
            download.source.describe()
        );
    }
    Ok(())
}

/// Explain how to move to GitHub releases without overwriting managed files.
fn managed_install_warning(method: InstallMethod) -> Option<String> {
    if method.supports_self_update() {
        return None;
    }
    if method == InstallMethod::DesktopApp {
        return Some(codewhale_release::install::DESKTOP_APP_UPDATE_HELP.to_string());
    }
    Some(format!(
        "This executable is managed by {label}; in-place self-update is disabled.\n\n\
         {GITHUB_MIGRATION_HELP}\n\n\
         To retain this secondary {label} installation, run `{command}`.",
        label = method.label(),
        command = method.update_command()
    ))
}

/// Resolve the executable that the updater is allowed to replace.
///
/// Android's `std::env::current_exe()`, `AT_EXECFN`, and `/proc/self/exe` can
/// all identify Bionic's runtime linker rather than the launched program. On
/// Android, locate a marker compiled into this executable with `dladdr`, then
/// require the executable `/proc/self/maps` row containing that same address
/// to agree by canonical path, device, and inode.
#[derive(Debug, Clone)]
struct UpdateExecutableIdentity {
    path: PathBuf,
    file_hash: String,
    #[cfg(target_os = "android")]
    android_proof: AndroidExecutableProof,
}

#[cfg(not(target_os = "android"))]
fn update_executable_identity() -> Result<UpdateExecutableIdentity> {
    let path = std::env::current_exe().context("failed to determine current executable path")?;
    let file_hash = sha256_hex(&std::fs::read(&path).context("failed to identify updater binary")?);
    Ok(UpdateExecutableIdentity { path, file_hash })
}

#[cfg(target_os = "android")]
fn update_executable_identity() -> Result<UpdateExecutableIdentity> {
    let android_proof = android_loaded_executable_proof()?;
    Ok(UpdateExecutableIdentity {
        file_hash: sha256_hex(
            &std::fs::read(&android_proof.path).context("failed to identify updater binary")?,
        ),
        path: android_proof.path.clone(),
        android_proof,
    })
}

#[cfg(target_os = "android")]
#[inline(never)]
extern "C" fn android_update_image_marker() -> usize {
    android_update_image_marker as *const () as usize
}

#[cfg(target_os = "android")]
fn android_loaded_executable_proof() -> Result<AndroidExecutableProof> {
    let marker = android_update_image_marker as *const () as usize as u64;
    let dladdr_path = android_dladdr_path(android_update_image_marker as *const libc::c_void)?;
    let maps = std::fs::read_to_string(ANDROID_PROC_SELF_MAPS)
        .context("failed to read Android executable mappings from /proc/self/maps")?;
    android_loaded_executable_proof_report(&maps, marker, &dladdr_path)
}

#[cfg(target_os = "android")]
fn android_dladdr_path(marker: *const libc::c_void) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    // SAFETY: `marker` points to a function in this loaded image and `info`
    // points to writable storage for the duration of the call.
    let found = unsafe { libc::dladdr(marker, info.as_mut_ptr()) };
    if found == 0 {
        bail!("Android dladdr could not locate the updater's loaded image");
    }
    // SAFETY: A non-zero dladdr result initializes `info`.
    let info = unsafe { info.assume_init() };
    if info.dli_fname.is_null() {
        bail!("Android dladdr returned an empty loaded-image path");
    }
    // SAFETY: `dli_fname` is a NUL-terminated string owned by the dynamic
    // loader and remains valid while this image is loaded.
    let bytes = unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes();
    if bytes.is_empty() {
        bail!("Android dladdr returned an empty loaded-image path");
    }
    Ok(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(any(target_os = "android", all(test, unix)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AndroidImageMapping {
    start: u64,
    end: u64,
    device_major: u32,
    device_minor: u32,
    inode: u64,
    path: PathBuf,
}

#[cfg(any(target_os = "android", all(test, unix)))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AndroidExecutableProofKind {
    DladdrAndProcMaps,
}

#[cfg(any(target_os = "android", all(test, unix)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AndroidExecutableProof {
    path: PathBuf,
    device_major: u32,
    device_minor: u32,
    inode: u64,
    proof_kind: AndroidExecutableProofKind,
}

#[cfg(any(target_os = "android", all(test, unix)))]
fn parse_android_image_mapping(maps: &str, marker: u64) -> Result<AndroidImageMapping> {
    let mut matching = None;
    for (line_index, line) in maps.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let range = fields
            .next()
            .with_context(|| format!("malformed /proc/self/maps line {}", line_index + 1))?;
        let (start, end) = range
            .split_once('-')
            .with_context(|| format!("malformed mapping range `{range}`"))?;
        let start = u64::from_str_radix(start, 16)
            .with_context(|| format!("invalid mapping start `{start}`"))?;
        let end =
            u64::from_str_radix(end, 16).with_context(|| format!("invalid mapping end `{end}`"))?;
        if !(start <= marker && marker < end) {
            continue;
        }

        let permissions = fields
            .next()
            .context("loaded-image mapping is missing permissions")?;
        let _offset = fields
            .next()
            .context("loaded-image mapping is missing its file offset")?;
        let device = fields
            .next()
            .context("loaded-image mapping is missing its device")?;
        let inode = fields
            .next()
            .context("loaded-image mapping is missing its inode")?
            .parse::<u64>()
            .context("loaded-image mapping has an invalid inode")?;
        let path = fields.collect::<Vec<_>>().join(" ");

        if permissions.as_bytes().get(2) != Some(&b'x') {
            bail!("loaded-image mapping for updater marker is not executable");
        }
        if inode == 0 {
            bail!("loaded-image mapping for updater marker has no file inode");
        }
        let (device_major, device_minor) = device
            .split_once(':')
            .context("loaded-image mapping has an invalid device")?;
        let device_major = u32::from_str_radix(device_major, 16)
            .context("loaded-image mapping has an invalid device major number")?;
        let device_minor = u32::from_str_radix(device_minor, 16)
            .context("loaded-image mapping has an invalid device minor number")?;
        if path.is_empty() {
            bail!("loaded-image mapping for updater marker has no pathname");
        }

        let mapping = AndroidImageMapping {
            start,
            end,
            device_major,
            device_minor,
            inode,
            path: PathBuf::from(path),
        };
        if matching.replace(mapping).is_some() {
            bail!("multiple /proc/self/maps rows contain the updater marker");
        }
    }

    matching.ok_or_else(|| anyhow!("no /proc/self/maps row contains the updater marker"))
}

#[cfg(all(test, unix))]
fn resolve_android_loaded_executable_report(
    maps: &str,
    marker: u64,
    dladdr_path: &Path,
) -> Result<PathBuf> {
    Ok(android_loaded_executable_proof_report(maps, marker, dladdr_path)?.path)
}

#[cfg(any(target_os = "android", all(test, unix)))]
fn android_loaded_executable_proof_report(
    maps: &str,
    marker: u64,
    dladdr_path: &Path,
) -> Result<AndroidExecutableProof> {
    let mapping = parse_android_image_mapping(maps, marker)?;
    validate_android_reported_path("dladdr", dladdr_path)?;
    validate_android_reported_path("/proc/self/maps", &mapping.path)?;

    let resolved_dladdr = dladdr_path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize Android dladdr path {}",
            dladdr_path.display()
        )
    })?;
    let resolved_mapping = mapping.path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize Android loaded-image mapping {}",
            mapping.path.display()
        )
    })?;
    if resolved_dladdr != resolved_mapping {
        bail!(
            "Android loaded-image authorities disagree: dladdr resolved to {}, but /proc/self/maps resolved to {}",
            resolved_dladdr.display(),
            resolved_mapping.display()
        );
    }
    if is_android_linker_name(&resolved_mapping) {
        bail!(
            "Android loaded-image authorities resolved to runtime linker {}; refusing to use the linker as an update target",
            resolved_mapping.display()
        );
    }
    if !is_executable_file(&resolved_mapping) {
        bail!(
            "Android loaded image `{}` is not an executable regular file; refusing to select an update target",
            resolved_mapping.display()
        );
    }

    validate_android_mapping_identity(&mapping, &resolved_mapping)?;
    Ok(AndroidExecutableProof {
        path: resolved_mapping,
        device_major: mapping.device_major,
        device_minor: mapping.device_minor,
        inode: mapping.inode,
        proof_kind: AndroidExecutableProofKind::DladdrAndProcMaps,
    })
}

#[cfg(any(target_os = "android", all(test, unix)))]
fn validate_android_reported_path(authority: &str, path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!(
            "Android {authority} reported non-absolute loaded-image path `{}`",
            path.display()
        );
    }
    if path.to_string_lossy().ends_with(" (deleted)") {
        bail!(
            "Android {authority} reported deleted loaded image `{}`",
            path.display()
        );
    }
    if is_android_linker_name(path) {
        bail!(
            "Android {authority} identifies runtime linker `{}`; refusing to use the linker as an update target",
            path.display()
        );
    }
    Ok(())
}

#[cfg(any(target_os = "android", all(test, unix)))]
fn validate_android_mapping_identity(
    mapping: &AndroidImageMapping,
    candidate: &Path,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let candidate_metadata = std::fs::metadata(candidate).with_context(|| {
        format!(
            "failed to stat Android update target {}",
            candidate.display()
        )
    })?;
    let (candidate_major, candidate_minor) = android_device_parts(candidate_metadata.dev());
    let identity_matches = mapping.device_major == candidate_major
        && mapping.device_minor == candidate_minor
        && mapping.inode == candidate_metadata.ino();
    if !identity_matches {
        bail!(
            "Android loaded-image identity changed: /proc/self/maps has device/inode {:x}:{:x}:{}, but update target {} is {:x}:{:x}:{}; refusing to replace it",
            mapping.device_major,
            mapping.device_minor,
            mapping.inode,
            candidate.display(),
            candidate_major,
            candidate_minor,
            candidate_metadata.ino()
        );
    }
    Ok(())
}

#[cfg(any(target_os = "android", all(test, unix)))]
fn android_device_parts(device: u64) -> (u32, u32) {
    // Linux/Bionic's dev_t encoding, matching makedev(3), major(3), and
    // minor(3). `/proc/self/maps` renders these components in hexadecimal.
    let major = ((device >> 8) & 0xfff) as u32;
    let minor = ((device & 0xff) | ((device >> 12) & 0xfff00)) as u32;
    (major, minor)
}

fn validate_primary_update_identity(identity: &UpdateExecutableIdentity) -> Result<()> {
    #[cfg(target_os = "android")]
    {
        let fresh = android_loaded_executable_proof()?;
        if fresh != identity.android_proof {
            bail!(
                "Android loaded-image proof changed from {:?} to {:?}; refusing to replace the update target",
                identity.android_proof,
                fresh
            );
        }
    }
    let bytes = std::fs::read(&identity.path).context("failed to recheck updater binary")?;
    if sha256_hex(&bytes) != identity.file_hash {
        bail!(
            "The running executable path changed during the update; no further files were replaced. Run the intended executable again by its full path."
        );
    }
    Ok(())
}

/// Only the running binary and copies of those exact bytes are ours to update.
/// Command names alone do not establish ownership of an existing sibling.
fn validate_update_target(target: &Path, identity: &UpdateExecutableIdentity) -> Result<()> {
    if !InstallMethod::from_path(target).supports_self_update() || protected_update_path(target) {
        bail!(
            "Refusing to replace managed/system path {}.\n\n{GITHUB_MIGRATION_HELP}",
            target.display()
        );
    }
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && target != identity.path => {
            return Ok(());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect update target {}", target.display()));
        }
    };
    if !metadata.is_file() || metadata.is_symlink() {
        bail!(
            "Refusing to replace {}: the update target is not a regular file.\n\n{GITHUB_MIGRATION_HELP}",
            target.display()
        );
    }
    let bytes = std::fs::read(target)
        .with_context(|| format!("failed to identify update target {}", target.display()))?;
    if sha256_hex(&bytes) != identity.file_hash {
        bail!(
            "Refusing to replace {}: its bytes differ from the running executable. This may be another installation or an unrelated command. No command is removed automatically.\n\n{GITHUB_MIGRATION_HELP}",
            target.display()
        );
    }
    Ok(())
}

fn protected_update_path(path: &Path) -> bool {
    [
        "/usr/bin",
        "/usr/sbin",
        "/bin",
        "/sbin",
        "/nix/store",
        "/gnu/store",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
        || path.components().any(|component| {
            component.as_os_str().to_str().is_some_and(|name| {
                name.eq_ignore_ascii_case("Windows")
                    || name.eq_ignore_ascii_case("WindowsApps")
                    || name.eq_ignore_ascii_case("scoop")
                    || name.eq_ignore_ascii_case("chocolatey")
            })
        })
}

fn replace_verified_downloads<F>(
    target_paths: &[PathBuf],
    verified_bytes: &[u8],
    validate_target: F,
) -> Result<()>
where
    F: Fn(&Path) -> Result<()>,
{
    // Fail before mutating a sibling if the primary pathname no longer names
    // the process image that initiated this update.
    for path in target_paths {
        validate_target(path)?;
    }
    for path in target_paths.iter().rev() {
        replace_binary_with_validation(path, verified_bytes, || {
            // Re-check after each temp file is fully staged and immediately
            // before every destructive rename. The running command is first
            // in the plan and therefore replaced last, after its colocated
            // compatibility names have received the same verified bytes.
            validate_target(path)
        })?;
    }
    Ok(())
}

#[cfg(any(target_os = "android", all(test, unix)))]
fn is_android_linker_name(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            matches!(
                name,
                "linker"
                    | "linker64"
                    | "linker_asan"
                    | "linker_asan64"
                    | "linker_hwasan"
                    | "linker_hwasan64"
            )
        })
}

#[cfg(any(target_os = "android", all(test, unix)))]
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchedRelease {
    release: Release,
    source: UpdateReleaseSource,
}

/// Where a release's assets come from.
///
/// This names the *asset* origin, which is not always the origin of the release
/// metadata: without an override the tag is resolved from GitHub, and only then
/// is the asset source chosen between GitHub and the first-party CNB mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateReleaseSource {
    /// Canonical GitHub Releases.
    GitHub,
    /// The first-party CNB mirror release for this exact tag (Linux x64 only).
    Cnb { base_url: String },
    /// An operator-supplied asset directory (`CODEWHALE_RELEASE_BASE_URL`).
    Mirror { base_url: String },
}

impl UpdateReleaseSource {
    /// Short, stable name for status output.
    fn label(&self) -> &'static str {
        match self {
            Self::GitHub => "GitHub Releases",
            Self::Cnb { .. } => "CNB mirror",
            Self::Mirror { .. } => "release mirror",
        }
    }

    /// The asset directory this source serves from, when it has one.
    fn base_url(&self) -> Option<&str> {
        match self {
            Self::GitHub => None,
            Self::Cnb { base_url } | Self::Mirror { base_url } => Some(base_url),
        }
    }

    /// Label plus asset directory — what status lines and the final receipt
    /// print, so "which source did this binary come from?" is answerable
    /// without rerunning the updater.
    fn describe(&self) -> String {
        match self.base_url() {
            Some(base_url) => format!("{} ({base_url})", self.label()),
            None => self.label().to_string(),
        }
    }

    /// True when an environment override, not a probe, chose this source. Such
    /// a source also carries the pinned version. The same no-downgrade and
    /// already-current checks apply before downloading from any source.
    fn is_pinned_mirror(&self) -> bool {
        !matches!(self, Self::GitHub)
    }
}

/// One source that could serve this release, and the two URLs that must come
/// from it together: the checksum manifest, and the binary that manifest
/// covers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseSourceCandidate {
    source: UpdateReleaseSource,
    manifest_url: String,
    binary_name: String,
    binary_url: String,
}

/// A source locked in for this update, with its manifest already fetched,
/// parsed, and confirmed to cover the binary we are about to download.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DownloadPlan {
    source: UpdateReleaseSource,
    binary_name: String,
    binary_url: String,
    /// Parsed checksums from this same source, already confirmed to cover
    /// `binary_name`. A plan cannot exist without this proof.
    checksums: HashMap<String, String>,
    manifest_url: String,
    manifest_bytes: Vec<u8>,
    fallback_binaries: Vec<(UpdateReleaseSource, String)>,
}

/// Fetches one candidate's checksum manifest. Injected so the selection logic
/// can be tested without a network.
type ManifestFetcher = dyn Fn(&ReleaseSourceCandidate) -> Result<Vec<u8>> + Send + Sync;

/// Build the candidate list for proactive source selection, or `None` when this
/// update keeps a single canonical source.
///
/// Selection applies only when the release metadata came from GitHub (an
/// explicit override already named the source) and the target is one the CNB
/// mirror actually publishes. Every other target is left exactly as it was.
fn proactive_source_candidates(
    fetched: &FetchedRelease,
    asset_stem: &str,
    os: &str,
    rust_arch: &str,
) -> Option<Vec<ReleaseSourceCandidate>> {
    if fetched.source != UpdateReleaseSource::GitHub || !cnb_mirror_supports_target(os, rust_arch) {
        return None;
    }
    let mut candidates = Vec::new();
    if let Some(github) = github_source_candidate(&fetched.release, asset_stem) {
        candidates.push(github);
    }
    candidates.push(cnb_source_candidate(
        &fetched.release.tag_name,
        os,
        rust_arch,
    ));
    Some(candidates)
}

/// The canonical GitHub candidate for a release the API already described.
///
/// Asset URLs come from the release payload when it advertises them. A release
/// that lists the platform binary but not the manifest still gets a candidate:
/// GitHub serves release assets from a stable per-tag path, so the manifest is
/// addressable even when the payload omits it.
fn github_source_candidate(release: &Release, asset_stem: &str) -> Option<ReleaseSourceCandidate> {
    let asset = select_platform_asset(release, asset_stem)?;
    let manifest_url = select_checksum_manifest_asset(release)
        .map(|manifest| manifest.browser_download_url.clone())
        .unwrap_or_else(|| {
            let tag_name = format!("v{}", release.tag_name.trim_start_matches('v'));
            mirror_asset_url(
                &format!("{GITHUB_RELEASE_DOWNLOAD_BASE_URL}/{tag_name}"),
                CHECKSUM_MANIFEST_ASSET,
            )
        });
    Some(ReleaseSourceCandidate {
        source: UpdateReleaseSource::GitHub,
        manifest_url,
        binary_name: asset.name.clone(),
        binary_url: asset.browser_download_url.clone(),
    })
}

/// The first-party CNB candidate for this exact tag.
///
/// CNB builds its own artifacts from the tagged source, so its manifest only
/// describes its own binaries — which is precisely why the manifest and the
/// binary have to be taken from the same source.
fn cnb_source_candidate(tag_name: &str, os: &str, rust_arch: &str) -> ReleaseSourceCandidate {
    let base_url = cnb_release_base_url(tag_name);
    let binary_name = release_asset_name_for_prefix("codewhale", os, rust_arch);
    ReleaseSourceCandidate {
        manifest_url: mirror_asset_url(&base_url, CHECKSUM_MANIFEST_ASSET),
        binary_url: mirror_asset_url(&base_url, &binary_name),
        binary_name,
        source: UpdateReleaseSource::Cnb { base_url },
    }
}

/// Decide where this update's bytes come from, and prove the choice before
/// committing to it.
fn resolve_download_plan(
    fetched: &FetchedRelease,
    asset_stem: &str,
    proxy: Option<&Proxy>,
    require_compiled_host: bool,
) -> Result<DownloadPlan> {
    match proactive_source_candidates(
        fetched,
        asset_stem,
        std::env::consts::OS,
        std::env::consts::ARCH,
    ) {
        Some(candidates) => {
            println!(
                "Probing {CHECKSUM_MANIFEST_ASSET} for {} from {}...",
                fetched.release.tag_name,
                candidate_labels(&candidates)
            );
            let fetch = manifest_probe_fetcher(proxy);
            let qualified: Arc<ManifestFetcher> = Arc::new(move |candidate| {
                let bytes = fetch(candidate)?;
                if require_compiled_host {
                    compiled_host::require_catalog_manifest(&bytes)?;
                }
                Ok(bytes)
            });
            select_release_source(candidates, qualified).with_context(update_network_fallback_hint)
        }
        None => {
            let plan = single_source_download_plan(fetched, asset_stem, proxy)?;
            if require_compiled_host
                && !plan
                    .checksums
                    .contains_key("codewhale-extension-hosts.json")
            {
                bail!(
                    "selected release source has no qualified compiled-host catalog; no files changed"
                );
            }
            Ok(plan)
        }
    }
}

fn candidate_labels(candidates: &[ReleaseSourceCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| candidate.source.label())
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Name the source `--check` would download from, without downloading anything
/// bigger than a manifest — and without contacting anything at all when an
/// override already fixed the answer.
fn describe_release_source_for_check(
    fetched: &FetchedRelease,
    asset_stem: &str,
    proxy: Option<&Proxy>,
) -> String {
    let Some(candidates) = proactive_source_candidates(
        fetched,
        asset_stem,
        std::env::consts::OS,
        std::env::consts::ARCH,
    ) else {
        return fetched.source.describe();
    };
    match select_release_source(candidates, manifest_probe_fetcher(proxy)) {
        Ok(plan) => plan.source.describe(),
        // A failed probe is a real answer for `--check` to report, not a reason
        // to fail a command whose whole job is to describe the release.
        Err(error) => format!("unresolved — {error:#}"),
    }
}

/// Resolve an explicit source or a platform without a supported fallback.
///
/// This path is still fail-closed: every platform and every explicit mirror
/// must publish a valid manifest from the same source that covers the selected
/// binary. The binary is not downloaded until that proof exists.
fn single_source_download_plan(
    fetched: &FetchedRelease,
    asset_stem: &str,
    proxy: Option<&Proxy>,
) -> Result<DownloadPlan> {
    let release = &fetched.release;
    let asset = select_platform_asset(release, asset_stem).with_context(|| {
        format!(
            "no asset found for platform {asset_stem} in release {}. \
             Available assets: {}",
            release.tag_name,
            release
                .assets
                .iter()
                .map(|asset| asset.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    let checksum_asset = select_checksum_manifest_asset(release).with_context(|| {
        format!(
            "release {} from {} does not publish required {CHECKSUM_MANIFEST_ASSET}; refusing to download {} without checksum verification",
            release.tag_name,
            fetched.source.describe(),
            asset.name
        )
    })?;
    println!("Downloading {}...", checksum_asset.name);
    let checksum_bytes = download_url_with_timeout(
        &checksum_asset.browser_download_url,
        proxy,
        MANIFEST_PROBE_TIMEOUT,
    )
    .with_context(|| {
        format!(
            "failed to download {} from {}\n{}",
            checksum_asset.name,
            fetched.source.describe(),
            update_network_fallback_hint()
        )
    })?;
    let checksum_text = std::str::from_utf8(&checksum_bytes)
        .with_context(|| format!("{} is not valid UTF-8", checksum_asset.name))?;
    let checksums = parse_checksum_manifest(checksum_text).with_context(|| {
        format!(
            "failed to parse {} from {}",
            checksum_asset.name,
            fetched.source.describe()
        )
    })?;
    if !checksums.contains_key(&asset.name) {
        bail!(
            "{} from {} does not list {}; refusing to download an unverified update",
            checksum_asset.name,
            fetched.source.describe(),
            asset.name
        );
    }

    Ok(DownloadPlan {
        source: fetched.source.clone(),
        binary_name: asset.name.clone(),
        binary_url: asset.browser_download_url.clone(),
        checksums,
        manifest_url: checksum_asset.browser_download_url.clone(),
        manifest_bytes: checksum_bytes,
        fallback_binaries: Vec::new(),
    })
}

fn manifest_probe_fetcher(proxy: Option<&Proxy>) -> Arc<ManifestFetcher> {
    let proxy = proxy.cloned();
    Arc::new(move |candidate: &ReleaseSourceCandidate| {
        download_url_with_timeout(
            &candidate.manifest_url,
            proxy.as_ref(),
            MANIFEST_PROBE_TIMEOUT,
        )
    })
}

/// Try the official GitHub manifest first. Only an unavailable or unusable
/// manifest admits the next configured source; a faster mirror never races
/// GitHub. Each network probe has its own bounded timeout and retry policy.
fn select_release_source(
    candidates: Vec<ReleaseSourceCandidate>,
    fetch_manifest: Arc<ManifestFetcher>,
) -> Result<DownloadPlan> {
    let mut candidates = candidates.into_iter();
    let primary = candidates
        .next()
        .context("no release source publishes an asset for this platform")?;
    let (checksums, manifest_bytes) = probe_release_source(&primary, &*fetch_manifest)
        .with_context(|| {
            format!(
                "{} has no usable {CHECKSUM_MANIFEST_ASSET}; mirrors may only serve binaries that this manifest pins, so nothing was downloaded",
                primary.source.describe()
            )
        })?;
    let fallback_binaries = candidates
        .filter(|candidate| candidate.binary_name == primary.binary_name)
        .map(|candidate| (candidate.source, candidate.binary_url))
        .collect();
    Ok(DownloadPlan {
        source: primary.source,
        binary_name: primary.binary_name,
        binary_url: primary.binary_url,
        checksums,
        manifest_url: primary.manifest_url,
        manifest_bytes,
        fallback_binaries,
    })
}

fn download_pinned_binary(download: &DownloadPlan, proxy: Option<&Proxy>) -> Result<Vec<u8>> {
    let mut failures = Vec::new();
    let sources = std::iter::once((&download.source, &download.binary_url)).chain(
        download
            .fallback_binaries
            .iter()
            .map(|(source, url)| (source, url)),
    );
    for (source, url) in sources {
        match download_url(url, proxy)
            .and_then(|bytes| verify_downloaded_asset(download, &bytes).map(|()| bytes))
        {
            Ok(bytes) => {
                if source != &download.source {
                    println!(
                        "Downloaded {} from {}; its SHA256 matches the {} manifest.",
                        download.binary_name,
                        source.describe(),
                        download.source.label()
                    );
                }
                return Ok(bytes);
            }
            Err(error) => failures.push(format!("  - {}: {error:#}", source.describe())),
        }
    }
    bail!(
        "failed to download a {} whose SHA256 matches {CHECKSUM_MANIFEST_ASSET} from {}:\n{}\n{}",
        download.binary_name,
        download.source.describe(),
        failures.join("\n"),
        update_network_fallback_hint()
    )
}

fn probe_release_source(
    candidate: &ReleaseSourceCandidate,
    fetch_manifest: &ManifestFetcher,
) -> Result<(HashMap<String, String>, Vec<u8>)> {
    let bytes = fetch_manifest(candidate)
        .with_context(|| format!("failed to fetch {}", candidate.manifest_url))?;
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("{} is not valid UTF-8", candidate.manifest_url))?;
    let checksums = parse_checksum_manifest(text)
        .with_context(|| format!("failed to parse {}", candidate.manifest_url))?;
    if !checksums.contains_key(&candidate.binary_name) {
        bail!(
            "{} does not list {}",
            candidate.manifest_url,
            candidate.binary_name
        );
    }
    Ok((checksums, bytes))
}

fn ensure_supported_release_target(os: &str, arch: &str) -> Result<()> {
    if os == "linux" && arch == "riscv64" {
        bail!(
            "Linux riscv64 release assets are temporarily unavailable because \
             rquickjs-sys 0.12.0 does not ship riscv64gc-unknown-linux-gnu bindings. \
             See docs/INSTALL.md for the current platform matrix."
        );
    }
    Ok(())
}

pub(crate) fn release_arch_for_rust_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

/// Returns true when the binary name belongs to the pre-rebrand `deepseek-tui` era.
pub(crate) fn is_legacy_binary(current_exe: &Path) -> bool {
    let exe_name = current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    exe_name.starts_with("deepseek")
}

fn legacy_binary_message(current_exe: &Path) -> String {
    format!(
        "\
this binary ({exe}) is using the legacy deepseek/deepseek-tui command name.

The package has been renamed to `codewhale`. A supported direct update can
install the canonical `codewhale` command beside this legacy command when a
newer verified release is available and the destination paths are safe to use.
DeepSeek provider support is unchanged.

{GITHUB_MIGRATION_HELP}

Existing npm, Cargo, Homebrew, or system-managed commands are left to their
package manager. See docs/INSTALL.md for secondary package routes.

Once `codewhale` is on your PATH, run `codewhale update` for future updates.",
        exe = current_exe.display(),
    )
}

fn command_name_for_exe(current_exe: &Path) -> String {
    let exe_name = current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("codewhale")
        .to_ascii_lowercase();
    exe_name
        .strip_suffix(".exe")
        .unwrap_or(&exe_name)
        .to_string()
}

fn command_path_beside(current_exe: &Path, command: &str) -> PathBuf {
    current_exe.with_file_name(format!("{command}{}", std::env::consts::EXE_SUFFIX))
}

fn installed_command_path(current_exe: &Path, command: &str) -> PathBuf {
    if command_name_for_exe(current_exe) == command {
        current_exe.to_path_buf()
    } else {
        command_path_beside(current_exe, command)
    }
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn push_update_path(paths: &mut Vec<PathBuf>, path: PathBuf, current_exe: &Path) {
    // Keep a same-target symlink as a symlink. Replacing its target refreshes
    // the alias too. Foreign/broken links stay in the plan and fail validation.
    // Compare canonical paths on both sides: `current_exe()` is not
    // canonicalized on macOS, so an install dir reached through a symlinked
    // directory would otherwise never match its own alias.
    let same_target_link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_symlink())
        && match [path.as_path(), current_exe].map(std::fs::canonicalize) {
            [Ok(resolved), exe] => resolved == current_exe || exe.is_ok_and(|exe| resolved == exe),
            _ => false,
        };
    if !same_target_link {
        push_unique_path(paths, path);
    }
}

fn legacy_tui_command_exists_beside(current_exe: &Path) -> bool {
    command_name_for_exe(current_exe) == "deepseek-tui"
        || command_path_beside(current_exe, "deepseek-tui").exists()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpdatePlan {
    target_paths: Vec<PathBuf>,
    asset_stem: String,
}

fn update_plan_for_exe(current_exe: &Path) -> UpdatePlan {
    let mut target_paths = Vec::new();

    // Keep the process image first so reverse-order replacement updates the
    // command currently running the updater last. Pre-rebrand command names
    // retain their historical migration behavior: install canonical commands
    // beside them instead of overwriting the legacy path.
    if !is_legacy_binary(current_exe) {
        push_unique_path(&mut target_paths, current_exe.to_path_buf());
    }

    let primary = installed_command_path(current_exe, "codewhale");
    push_update_path(&mut target_paths, primary, current_exe);

    for alias in ["codew", "codewhale-tui"] {
        let alias_path = installed_command_path(current_exe, alias);
        let migrate_legacy_tui = alias == "codewhale-tui"
            && is_legacy_binary(current_exe)
            && legacy_tui_command_exists_beside(current_exe);
        if std::fs::symlink_metadata(&alias_path).is_ok()
            || command_name_for_exe(current_exe) == alias
            || migrate_legacy_tui
        {
            push_update_path(&mut target_paths, alias_path, current_exe);
        }
    }

    UpdatePlan {
        target_paths,
        asset_stem: release_asset_stem_for_prefix(
            "codewhale",
            std::env::consts::OS,
            std::env::consts::ARCH,
        ),
    }
}

fn release_asset_stem_for_prefix(prefix: &str, os: &str, rust_arch: &str) -> String {
    let arch = release_arch_for_rust_arch(rust_arch);
    format!("{prefix}-{os}-{arch}")
}

fn release_asset_name_for_prefix(prefix: &str, os: &str, rust_arch: &str) -> String {
    let stem = release_asset_stem_for_prefix(prefix, os, rust_arch);
    if os == "windows" {
        format!("{stem}.exe")
    } else {
        stem
    }
}

#[cfg(test)]
fn release_asset_stem_for(current_exe: &Path, os: &str, rust_arch: &str) -> String {
    let _ = current_exe;
    release_asset_stem_for_prefix("codewhale", os, rust_arch)
}

fn asset_is_exact_platform_binary(asset_name: &str, binary_name: &str) -> bool {
    asset_name == binary_name || asset_name == format!("{binary_name}.exe")
}

/// The raw platform executable, and nothing else.
///
/// The updater writes the downloaded bytes straight over the running binary;
/// it never unpacks. An archive, signature, or sidecar that merely shares the
/// stem (`codewhale-macos-arm64.tar.gz`) would pass the checksum — the manifest
/// lists it too — and then replace the executable with a non-executable. A
/// release without the raw binary has no asset for this platform.
fn select_platform_asset<'a>(release: &'a Release, binary_name: &str) -> Option<&'a Asset> {
    release
        .assets
        .iter()
        .find(|asset| asset_is_exact_platform_binary(&asset.name, binary_name))
}

fn select_checksum_manifest_asset(release: &Release) -> Option<&Asset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == CHECKSUM_MANIFEST_ASSET)
}

fn parse_checksum_manifest(text: &str) -> Result<HashMap<String, String>> {
    let mut checksums = HashMap::new();

    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.len() < 66 {
            bail!("invalid SHA256 manifest line {}: {trimmed}", index + 1);
        }

        let (hash, rest) = trimmed
            .get(..64)
            .zip(trimmed.get(64..))
            .with_context(|| format!("invalid SHA256 manifest line {}: {trimmed}", index + 1))?;
        if !hash.chars().all(|ch| ch.is_ascii_hexdigit())
            || rest.is_empty()
            || !rest.chars().next().is_some_and(char::is_whitespace)
        {
            bail!("invalid SHA256 manifest line {}: {trimmed}", index + 1);
        }

        let mut asset_name = rest.trim_start();
        if let Some(stripped) = asset_name.strip_prefix('*') {
            asset_name = stripped;
        }
        if asset_name.is_empty() {
            bail!("invalid SHA256 manifest line {}: {trimmed}", index + 1);
        }

        checksums.insert(asset_name.to_string(), hash.to_ascii_lowercase());
    }

    Ok(checksums)
}

#[cfg(test)]
fn expected_sha256_from_manifest(text: &str, asset_name: &str) -> Result<String> {
    let checksums = parse_checksum_manifest(text)?;
    checksums
        .get(asset_name)
        .cloned()
        .with_context(|| format!("checksum manifest is missing {asset_name}"))
}

/// GitHub release metadata.
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
struct Release {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

/// A single release asset.
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Validate the proxy URL format and build a proxy for update HTTP requests.
pub(crate) fn validate_and_build_proxy(proxy_str: &str) -> Result<Proxy> {
    let proxy_url = reqwest::Url::parse(proxy_str).with_context(|| {
        format!(
            "invalid proxy URL: {proxy_str}\n\
             Expected format: http://host:port, https://host:port, or socks5://host:port"
        )
    })?;
    Proxy::all(proxy_url).context("failed to configure update proxy")
}

fn update_http_client_with_policy(
    proxy: Option<&Proxy>,
    timeout: Duration,
    policy: &UpdateTransportPolicy,
) -> Result<reqwest::blocking::Client> {
    let mut builder = codewhale_release::platform_blocking_http_client_builder();
    if let Some(proxy) = proxy {
        builder = builder.proxy(proxy.clone());
    }
    builder
        .user_agent(UPDATE_USER_AGENT)
        .timeout(timeout)
        .redirect(update_redirect_policy(policy.clone()))
        .build()
        .context("failed to build update HTTP client")
}

/// Most redirects an update request follows.
const UPDATE_MAX_REDIRECTS: usize = 10;

/// Largest update response held in memory. Release archives are tens of
/// megabytes; a server that keeps sending is cut off instead of exhausting
/// memory before the checksum is ever compared.
const UPDATE_MAX_RESPONSE_BYTES: u64 = 512 * 1024 * 1024;

/// Hosts an update may contact without any operator configuration: GitHub's
/// API and release pages, and the three hosts GitHub has served release
/// assets from.
const UPDATE_GITHUB_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

/// The first-party CNB mirror, which `CODEWHALE_USE_CNB_MIRROR=1` and the
/// proactive Linux x64 probe select.
const UPDATE_CNB_HOST: &str = "cnb.cool";

/// Extra hosts the operator trusts for updates, comma separated. A mirror set
/// with `CODEWHALE_RELEASE_BASE_URL` already allows its own host; this is for
/// a mirror that redirects asset downloads to a separate download host.
const UPDATE_ALLOWED_HOSTS_ENV: &str = "CODEWHALE_UPDATE_ALLOWED_HOSTS";

/// Where an update request may go. Every request and every redirect hop must
/// be HTTPS and must name an allowed host, so a redirect or a poisoned asset
/// URL cannot move a download to a host nobody chose.
#[derive(Debug, Clone)]
struct UpdateTransportPolicy {
    extra_hosts: Vec<String>,
    /// Unit tests serve fixtures from loopback over plain HTTP; the tests that
    /// prove the policy build `strict()` and never set this.
    #[cfg(test)]
    allow_loopback_http: bool,
}

impl UpdateTransportPolicy {
    /// The built-in hosts plus what the operator configured: the host of an
    /// explicit release mirror, and `CODEWHALE_UPDATE_ALLOWED_HOSTS`.
    fn from_env() -> Self {
        let mut policy = Self::strict();
        if let Some(base_url) = codewhale_release::explicit_release_base_url_from_env()
            && let Ok(url) = reqwest::Url::parse(&base_url)
            && let Some(host) = url.host_str()
        {
            policy.extra_hosts.push(host.to_ascii_lowercase());
        }
        if let Ok(hosts) = std::env::var(UPDATE_ALLOWED_HOSTS_ENV) {
            policy.extra_hosts.extend(
                hosts
                    .split(',')
                    .map(|host| host.trim().to_ascii_lowercase())
                    .filter(|host| !host.is_empty()),
            );
        }
        #[cfg(test)]
        {
            policy.allow_loopback_http = true;
        }
        policy
    }

    fn strict() -> Self {
        Self {
            extra_hosts: Vec::new(),
            #[cfg(test)]
            allow_loopback_http: false,
        }
    }

    fn host_is_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        UPDATE_GITHUB_HOSTS.contains(&host.as_str())
            || host == UPDATE_CNB_HOST
            || host
                .strip_suffix(UPDATE_CNB_HOST)
                .is_some_and(|prefix| prefix.ends_with('.'))
            || self.extra_hosts.contains(&host)
    }

    /// Refuse anything that is not HTTPS to an allowed host.
    fn check_url(&self, url: &reqwest::Url) -> std::result::Result<(), String> {
        if cfg!(debug_assertions)
            && std::env::var_os("CODEWHALE_UPDATE_ALLOW_LOOPBACK_HTTP").is_some()
            && url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        {
            return Ok(());
        }
        #[cfg(test)]
        if self.allow_loopback_http
            && url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        {
            return Ok(());
        }
        if url.scheme() != "https" {
            return Err(format!(
                "update URL must use HTTPS, not {}: {url}",
                url.scheme()
            ));
        }
        let host = url.host_str().unwrap_or_default();
        if !self.host_is_allowed(host) {
            return Err(format!(
                "update host {host:?} is not an allowed release host. A private mirror's host \
                 is allowed when it is the {} HTTPS base URL; any other host can be added with \
                 {UPDATE_ALLOWED_HOSTS_ENV}=host1,host2",
                codewhale_release::RELEASE_BASE_URL_ENV
            ));
        }
        Ok(())
    }

    fn check_str(&self, url: &str) -> Result<()> {
        let parsed =
            reqwest::Url::parse(url).with_context(|| format!("invalid update URL {url}"))?;
        self.check_url(&parsed).map_err(|message| anyhow!(message))
    }
}

/// Follow redirects, but only over HTTPS and only to an allowed host: a
/// request that started encrypted must not finish over a channel anyone on the
/// path can rewrite, or on a host nobody chose.
fn update_redirect_policy(policy: UpdateTransportPolicy) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > UPDATE_MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        match policy.check_url(attempt.url()) {
            Ok(()) => attempt.follow(),
            Err(message) => attempt.error(format!("update redirect refused: {message}")),
        }
    })
}

/// Fetch the latest release metadata from GitHub.
fn fetch_latest_release(channel: ReleaseChannel, proxy: Option<&Proxy>) -> Result<FetchedRelease> {
    match resolve_release_query(channel) {
        ReleaseQuery::Mirror { base_url, version } => {
            UpdateTransportPolicy::from_env()
                .check_str(&base_url)
                .with_context(|| format!("release mirror {base_url} cannot be used"))?;
            Ok(FetchedRelease {
                release: release_from_mirror_base_url(
                    &base_url,
                    &version,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                ),
                source: pinned_mirror_source(base_url),
            })
        }
        ReleaseQuery::GitHubLatest { url } => match fetch_latest_release_from_url(url, proxy) {
            Ok(release) => Ok(FetchedRelease {
                release,
                source: UpdateReleaseSource::GitHub,
            }),
            Err(api_error) => {
                eprintln!(
                    "GitHub API release lookup failed; trying github.com releases/latest fallback..."
                );
                Ok(FetchedRelease {
                    release: fetch_latest_stable_release_from_redirect(proxy).with_context(
                        || format!("GitHub API release lookup failed first: {api_error:#}"),
                    )?,
                    source: UpdateReleaseSource::GitHub,
                })
            }
        },
        ReleaseQuery::GitHubReleaseList { url } => Ok(FetchedRelease {
            release: fetch_latest_beta_release_from_url(url, proxy)?,
            source: UpdateReleaseSource::GitHub,
        }),
    }
}

/// Name the source an environment override selected.
///
/// `CODEWHALE_USE_CNB_MIRROR` and `CODEWHALE_RELEASE_BASE_URL` both resolve to
/// a base URL, but only the first is the first-party mirror — reporting them
/// alike would hide which one a user actually asked for.
fn pinned_mirror_source(base_url: String) -> UpdateReleaseSource {
    if cnb_mirror_override_active() {
        UpdateReleaseSource::Cnb { base_url }
    } else {
        UpdateReleaseSource::Mirror { base_url }
    }
}

fn release_from_mirror_base_url(
    base_url: &str,
    version: &str,
    os: &str,
    rust_arch: &str,
) -> Release {
    let tag_name = format!("v{}", version.trim_start_matches('v'));
    release_from_asset_base_url(&tag_name, base_url, os, rust_arch)
}

fn release_from_github_download_tag(tag_name: &str, os: &str, rust_arch: &str) -> Release {
    let tag_name = format!("v{}", tag_name.trim_start_matches('v'));
    let base_url = format!("{GITHUB_RELEASE_DOWNLOAD_BASE_URL}/{tag_name}");
    release_from_asset_base_url(&tag_name, &base_url, os, rust_arch)
}

fn release_from_asset_base_url(
    tag_name: &str,
    base_url: &str,
    os: &str,
    rust_arch: &str,
) -> Release {
    let mut assets = vec![Asset {
        name: CHECKSUM_MANIFEST_ASSET.to_string(),
        browser_download_url: mirror_asset_url(base_url, CHECKSUM_MANIFEST_ASSET),
    }];

    let name = release_asset_name_for_prefix("codewhale", os, rust_arch);
    assets.push(Asset {
        browser_download_url: mirror_asset_url(base_url, &name),
        name,
    });

    Release {
        tag_name: tag_name.to_string(),
        prerelease: false,
        assets,
    }
}

fn fetch_release_json_once(
    url: &str,
    description: &str,
    proxy: Option<&Proxy>,
) -> Result<(reqwest::StatusCode, String)> {
    let policy = UpdateTransportPolicy::from_env();
    policy.check_str(url)?;
    let client = update_http_client_with_policy(proxy, UPDATE_DOWNLOAD_TIMEOUT, &policy)?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .with_context(|| format!("failed to fetch {description} from {url}"))?;
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("failed to read {description} response body from {url}"))?;
    Ok((status, body))
}

fn fetch_release_json(url: &str, description: &str, proxy: Option<&Proxy>) -> Result<String> {
    let mut last_error = None;
    for attempt in 1..=UPDATE_HTTP_ATTEMPTS {
        match fetch_release_json_once(url, description, proxy) {
            Ok((status, body)) if status.is_success() => return Ok(body),
            Ok((status, body)) => {
                let error =
                    anyhow!("failed to fetch {description} from {url}: HTTP {status}\n{body}");
                if should_retry_http_status(status) && attempt < UPDATE_HTTP_ATTEMPTS {
                    last_error = Some(error);
                    sleep_before_update_retry(attempt);
                    continue;
                }
                return Err(error);
            }
            Err(error) if attempt < UPDATE_HTTP_ATTEMPTS => {
                last_error = Some(error);
                sleep_before_update_retry(attempt);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("failed to fetch {description} from {url}")))
}

fn should_retry_http_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

fn sleep_before_update_retry(attempt: usize) {
    std::thread::sleep(Duration::from_millis(
        UPDATE_HTTP_RETRY_DELAY_MS * attempt as u64,
    ));
}

fn fetch_latest_release_from_url(url: &str, proxy: Option<&Proxy>) -> Result<Release> {
    let body = fetch_release_json(url, "release info", proxy)?;
    let release: Release = serde_json::from_str(&body).with_context(|| {
        format!("failed to parse release JSON from GitHub API. Response: {body}")
    })?;

    Ok(release)
}

fn fetch_latest_stable_release_from_redirect(proxy: Option<&Proxy>) -> Result<Release> {
    let tag_name =
        fetch_latest_stable_tag_from_redirect_url(GITHUB_LATEST_RELEASE_PAGE_URL, proxy)?;
    Ok(release_from_github_download_tag(
        &tag_name,
        std::env::consts::OS,
        std::env::consts::ARCH,
    ))
}

fn fetch_latest_stable_tag_from_redirect_url(url: &str, proxy: Option<&Proxy>) -> Result<String> {
    let policy = UpdateTransportPolicy::from_env();
    policy.check_str(url)?;
    let client = update_http_client_with_policy(proxy, UPDATE_DOWNLOAD_TIMEOUT, &policy)?;
    let mut last_error = None;
    for attempt in 1..=UPDATE_HTTP_ATTEMPTS {
        match fetch_latest_stable_tag_from_redirect_url_once(&client, &policy, url) {
            Ok(tag_name) => return Ok(tag_name),
            Err(error) if attempt < UPDATE_HTTP_ATTEMPTS => {
                last_error = Some(error);
                sleep_before_update_retry(attempt);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("failed to resolve latest stable release from {url}")))
}

fn fetch_latest_stable_tag_from_redirect_url_once(
    client: &reqwest::blocking::Client,
    policy: &UpdateTransportPolicy,
    url: &str,
) -> Result<String> {
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to fetch release redirect from {url}"))?;
    let status = response.status();
    let final_url = response.url().clone();
    // Every hop was already checked; the page that names the tag must still be
    // an allowed host before its URL or body is trusted for a tag.
    policy
        .check_url(&final_url)
        .map_err(|message| anyhow!(message))?;
    if status.is_success() {
        if let Some(tag_name) = release_tag_from_github_release_url(&final_url) {
            return Ok(tag_name);
        }
        let body = response
            .text()
            .with_context(|| format!("failed to read release redirect response from {url}"))?;
        if let Some(tag_name) = release_tag_from_github_release_html(&body) {
            return Ok(tag_name);
        }
        bail!("release redirect did not resolve to a tag URL: {final_url}");
    }

    let body = response
        .text()
        .with_context(|| format!("failed to read release redirect response from {url}"))?;
    bail!("failed to fetch release redirect from {url}: HTTP {status}\n{body}");
}

fn release_tag_from_github_release_url(url: &reqwest::Url) -> Option<String> {
    let segments = url.path_segments()?.collect::<Vec<_>>();
    segments
        .windows(3)
        .find(|window| window[0] == "releases" && window[1] == "tag")
        .map(|window| window[2].to_string())
        .filter(|tag| is_plausible_release_tag(tag))
}

/// A release tag becomes a URL path segment; only version-tag characters pass.
fn is_plausible_release_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '+'))
}

fn release_tag_from_github_release_html(body: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "/codewhale-hq/CodeWhale/releases/tag/",
        "/codewhale-hq/CodeWhale/releases/tag/",
        "/releases/tag/",
    ];
    for marker in MARKERS {
        for rest in body.split(marker).skip(1) {
            let tag = rest
                .split(['"', '\'', '<', '>', '?', '#', '&'])
                .next()
                .unwrap_or("")
                .trim();
            if is_plausible_release_tag(tag) {
                return Some(tag.to_string());
            }
        }
    }
    None
}

fn fetch_latest_beta_release_from_url(url: &str, proxy: Option<&Proxy>) -> Result<Release> {
    let body = fetch_release_json(url, "release list", proxy)?;
    // GitHub caps this endpoint at 100 releases per page. Codewhale uses the
    // first page as the latest-beta search window, matching GitHub's ordering.
    let releases: Vec<Release> = serde_json::from_str(&body).with_context(|| {
        format!("failed to parse release list JSON from GitHub API. Response: {body}")
    })?;

    releases
        .into_iter()
        .find(|release| is_beta_tag(&release.tag_name))
        .context("no beta release found in GitHub releases")
}

/// Download a URL to bytes.
fn download_url(url: &str, proxy: Option<&Proxy>) -> Result<Vec<u8>> {
    download_url_with_timeout(url, proxy, UPDATE_DOWNLOAD_TIMEOUT)
}

fn download_url_with_timeout(
    url: &str,
    proxy: Option<&Proxy>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 1..=UPDATE_HTTP_ATTEMPTS {
        match download_url_once(url, proxy, timeout) {
            Ok((status, bytes)) if status.is_success() => return Ok(bytes),
            Ok((status, bytes)) => {
                let body = String::from_utf8_lossy(&bytes);
                let error = anyhow!("download failed with HTTP {status}: {body}");
                if should_retry_http_status(status) && attempt < UPDATE_HTTP_ATTEMPTS {
                    last_error = Some(error);
                    sleep_before_update_retry(attempt);
                    continue;
                }
                return Err(error);
            }
            Err(error) if attempt < UPDATE_HTTP_ATTEMPTS => {
                last_error = Some(error);
                sleep_before_update_retry(attempt);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("failed to download {url}")))
}

fn download_url_once(
    url: &str,
    proxy: Option<&Proxy>,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, Vec<u8>)> {
    download_url_once_with(
        &UpdateTransportPolicy::from_env(),
        UPDATE_MAX_RESPONSE_BYTES,
        url,
        proxy,
        timeout,
    )
}

fn download_url_once_with(
    policy: &UpdateTransportPolicy,
    max_bytes: u64,
    url: &str,
    proxy: Option<&Proxy>,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, Vec<u8>)> {
    policy.check_str(url)?;
    let client = update_http_client_with_policy(proxy, timeout, policy)?;
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to download {url}"))?;
    let status = response.status();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(response, max_bytes + 1),
        &mut bytes,
    )
    .with_context(|| format!("failed to read response body from {url}"))?;
    if bytes.len() as u64 > max_bytes {
        bail!("response from {url} exceeds the {max_bytes}-byte update limit");
    }

    Ok((status, bytes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct GlibcVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl GlibcVersion {
    fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    fn display(self) -> String {
        if self.patch == 0 {
            format!("{}.{}", self.major, self.minor)
        } else {
            format!("{}.{}.{}", self.major, self.minor, self.patch)
        }
    }
}

fn parse_glibc_version(text: &str) -> Option<GlibcVersion> {
    text.split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter(|part| part.contains('.'))
        .find_map(parse_glibc_version_token)
}

fn parse_glibc_version_token(token: &str) -> Option<GlibcVersion> {
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    Some(GlibcVersion::new(major, minor, patch))
}

fn highest_required_glibc(bytes: &[u8]) -> Option<GlibcVersion> {
    const MARKER: &[u8] = b"GLIBC_";
    let mut offset = 0;
    let mut highest = None;

    while let Some(found) = find_bytes(&bytes[offset..], MARKER) {
        let start = offset + found + MARKER.len();
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        if end > start
            && let Ok(token) = std::str::from_utf8(&bytes[start..end])
            && let Some(version) = parse_glibc_version_token(token)
            && highest.is_none_or(|current| version > current)
        {
            highest = Some(version);
        }
        offset = start;
    }

    highest
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn glibc_check_disabled() -> bool {
    [
        "CODEWHALE_SKIP_GLIBC_CHECK",
        "DEEPSEEK_TUI_SKIP_GLIBC_CHECK",
        "DEEPSEEK_SKIP_GLIBC_CHECK",
    ]
    .into_iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| value == std::ffi::OsStr::new("1")))
}

fn preflight_downloaded_binary(asset_name: &str, bytes: &[u8]) -> Result<()> {
    // glibc preflight is Linux-only (#4241). Rust treats `target_os = "android"`
    // as distinct from `"linux"`, so Termux/Android builds skip this check entirely
    // — Android uses Bionic libc, not glibc.
    if !cfg!(target_os = "linux") || glibc_check_disabled() {
        return Ok(());
    }

    let Some(required) = highest_required_glibc(bytes) else {
        return Ok(());
    };
    let host = detect_host_glibc();
    if host.is_some_and(|host| host >= required) {
        return Ok(());
    }

    bail!(
        "{}",
        glibc_compatibility_message(asset_name, required, host)
    );
}

fn detect_host_glibc() -> Option<GlibcVersion> {
    let getconf = std::process::Command::new("getconf")
        .arg("GNU_LIBC_VERSION")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| parse_glibc_version(&output));
    if getconf.is_some() {
        return getconf;
    }

    std::process::Command::new("ldd")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.trim().is_empty() {
                text = String::from_utf8_lossy(&output.stderr).to_string();
            }
            parse_glibc_version(&text)
        })
}

fn glibc_compatibility_message(
    asset_name: &str,
    required: GlibcVersion,
    host: Option<GlibcVersion>,
) -> String {
    let host_line = match host {
        Some(host) => format!(
            "this system has glibc {}, which is too old for that asset.",
            host.display()
        ),
        None => "this system does not appear to provide glibc.".to_string(),
    };
    format!(
        "\
Prebuilt Codewhale asset `{asset_name}` requires GLIBC_{required}, but {host_line}

Official Codewhale Linux release assets (x64 and arm64) are static musl builds
with no glibc dependency, so this binary is not an official release asset. Check
the download source, or install from source on this host instead:

  cargo install codewhale-cli --locked

Set CODEWHALE_SKIP_GLIBC_CHECK=1 to bypass this preflight at your own risk.",
        required = required.display(),
    )
}

/// Replace the running binary.
///
/// Writes the new binary to a secure temp file in the target directory, then
/// installs it in place. Unix can atomically replace the executable path. On
/// Windows an atomic replace is tried first; when that fails, as it does for a
/// running executable, the current file is renamed out of the way before the
/// new binary moves into the original path.
#[cfg(test)]
fn replace_binary(target: &Path, new_bytes: &[u8]) -> Result<()> {
    replace_binary_with_validation(target, new_bytes, || Ok(()))
}

fn replace_binary_with_validation<F>(
    target: &Path,
    new_bytes: &[u8],
    validate_before_replace: F,
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    replace_binary_with_validation_and_permission_setter(
        target,
        new_bytes,
        validate_before_replace,
        |path, permissions| std::fs::set_permissions(path, permissions),
    )
}

/// `apply_permissions` is a seam for `std::fs::set_permissions` so tests can
/// exercise permission-setup failures without host-specific filesystem state.
fn replace_binary_with_validation_and_permission_setter<F, P>(
    target: &Path,
    new_bytes: &[u8],
    validate_before_replace: F,
    apply_permissions: P,
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
    P: Fn(&Path, std::fs::Permissions) -> std::io::Result<()>,
{
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut tmp = tempfile::Builder::new()
        .prefix(".codewhale-update-")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temp file in {}", parent.display()))?;
    tmp.write_all(new_bytes)
        .with_context(|| format!("failed to write temp file at {}", tmp.path().display()))?;

    // Permission setup is part of pre-replacement validation: a staged binary
    // that cannot receive correct permissions must never replace a working
    // target, so every failure below aborts before any destructive rename.
    if target.exists() {
        // Preserve permissions from the original binary.
        let meta = std::fs::metadata(target).with_context(|| {
            format!(
                "failed to read permissions of update target {}",
                target.display()
            )
        })?;
        apply_permissions(tmp.path(), meta.permissions()).with_context(|| {
            format!(
                "failed to set permissions on staged update {} before replacing {}",
                tmp.path().display(),
                target.display()
            )
        })?;
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            apply_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).with_context(
                || {
                    format!(
                        "failed to set permissions on staged update {} before installing {}",
                        tmp.path().display(),
                        target.display()
                    )
                },
            )?;
        }
    }

    // Independently verify the staged binary is executable before it may
    // replace the target; a chmod that silently did not stick would otherwise
    // install a binary that cannot run.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let staged_mode = tmp
            .as_file()
            .metadata()
            .with_context(|| {
                format!(
                    "failed to inspect staged update at {}",
                    tmp.path().display()
                )
            })?
            .permissions()
            .mode();
        if staged_mode & 0o111 == 0 {
            bail!(
                "staged update {} is not executable (mode {:03o}); refusing to replace {}",
                tmp.path().display(),
                staged_mode & 0o7777,
                target.display()
            );
        }
    }

    tmp.as_file()
        .sync_all()
        .with_context(|| format!("failed to sync staged update at {}", tmp.path().display()))?;

    validate_before_replace()?;

    #[cfg(windows)]
    {
        let tmp = match tmp.persist(target) {
            Ok(_) => return Ok(()),
            Err(error) if !target.exists() => bail!(
                "failed to install new binary at {}: {}",
                target.display(),
                error.error
            ),
            Err(error) => error.file,
        };
        let backup = backup_path_for(target);
        std::fs::rename(target, &backup).with_context(|| {
            format!(
                "failed to move current executable {} to {}",
                target.display(),
                backup.display()
            )
        })?;
        if let Err(err) = tmp.persist(target) {
            if backup.exists()
                && let Err(restore_error) = std::fs::rename(&backup, target)
            {
                bail!(
                    "failed to install new binary at {}: {}; restoring the previous binary also failed: {}. It is at {} — move it back by hand.",
                    target.display(),
                    err.error,
                    restore_error,
                    backup.display()
                );
            }
            bail!(
                "failed to install new binary at {}: {}",
                target.display(),
                err.error
            );
        }
        let _ = std::fs::remove_file(&backup);
    }

    #[cfg(not(windows))]
    {
        tmp.persist(target)
            .map_err(|err| err.error)
            .with_context(|| format!("failed to rename temp file to {}", target.display()))?;
    }

    Ok(())
}

#[cfg(windows)]
fn backup_path_for(target: &Path) -> std::path::PathBuf {
    let pid = std::process::id();
    for index in 0..100 {
        let mut candidate = target.to_path_buf();
        let suffix = if index == 0 {
            format!("old-{pid}")
        } else {
            format!("old-{pid}-{index}")
        };
        candidate.set_extension(suffix);
        if !candidate.exists() {
            return candidate;
        }
    }
    target.with_extension(format!("old-{pid}-fallback"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::sync::{Mutex, MutexGuard};
    use std::thread;

    /// Release-source environment variables are process-wide, so the tests that
    /// exercise override precedence take this lock and restore what they found.
    static UPDATE_ENV_LOCK: Mutex<()> = Mutex::new(());
    const UPDATE_ENV_VARS: &[&str] = &[
        codewhale_release::RELEASE_BASE_URL_ENV,
        codewhale_release::LEGACY_RELEASE_BASE_URL_ENV,
        codewhale_release::DEEPSEEK_RELEASE_BASE_URL_ENV,
        codewhale_release::CNB_MIRROR_ENV,
        codewhale_release::UPDATE_VERSION_ENV,
        codewhale_release::LEGACY_TUI_UPDATE_VERSION_ENV,
        codewhale_release::LEGACY_UPDATE_VERSION_ENV,
        codewhale_release::install::INSTALL_METHOD_ENV,
        UPDATE_ALLOWED_HOSTS_ENV,
    ];

    struct UpdateEnvGuard {
        previous: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl UpdateEnvGuard {
        fn clear() -> Self {
            let lock = UPDATE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = UPDATE_ENV_VARS
                .iter()
                .map(|&name| (name, std::env::var_os(name)))
                .collect();
            for &name in UPDATE_ENV_VARS {
                // SAFETY: tests that mutate these process-wide vars hold UPDATE_ENV_LOCK.
                unsafe { std::env::remove_var(name) };
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for UpdateEnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.previous {
                // SAFETY: the guard still holds UPDATE_ENV_LOCK while restoring state.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn set_update_env(name: &str, value: &str) {
        // SAFETY: callers hold an UpdateEnvGuard, which serializes env mutation.
        unsafe { std::env::set_var(name, value) };
    }

    #[cfg(unix)]
    fn write_test_executable(path: &Path) {
        std::fs::write(path, b"test executable").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Write a stand-in installed binary: real update targets carry an
    /// executable mode on Unix, which the updater now preserves and verifies.
    fn write_installed_binary(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Verify the arch mapping used when constructing asset names.
    /// The mapping must use release-asset naming (arm64/x64), not Rust
    /// stdlib constants (aarch64/x86_64).
    #[test]
    fn test_arch_mapping() {
        assert_eq!(release_arch_for_rust_arch("aarch64"), "arm64");
        assert_eq!(release_arch_for_rust_arch("x86_64"), "x64");
        // Pass-through for unknown arches
        assert_eq!(release_arch_for_rust_arch("riscv64"), "riscv64");
        // The currently-compiled arch maps to a release asset name
        let compiled_arch = std::env::consts::ARCH;
        let asset_arch = release_arch_for_rust_arch(compiled_arch);
        // Must not contain the raw Rust constant names
        assert!(
            !asset_arch.contains("aarch64") && !asset_arch.contains("x86_64"),
            "asset arch '{asset_arch}' still uses raw Rust constant name"
        );
    }

    #[test]
    fn linux_riscv64_update_is_explicitly_unsupported() {
        let err = ensure_supported_release_target("linux", "riscv64")
            .expect_err("linux riscv64 should not claim a release asset");
        let message = err.to_string();
        assert!(message.contains("Linux riscv64 release assets are temporarily unavailable"));
        assert!(message.contains("rquickjs-sys 0.12.0"));
        ensure_supported_release_target("linux", "aarch64").unwrap();
        ensure_supported_release_target("macos", "aarch64").unwrap();
    }

    #[cfg(unix)]
    const TEST_ANDROID_MARKER: u64 = 0x1800;

    #[cfg(unix)]
    fn test_android_mapping_line(path: &Path, permissions: &str) -> String {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(path).unwrap();
        let (device_major, device_minor) = android_device_parts(metadata.dev());
        format!(
            "1000-2000 {permissions} 00000000 {:x}:{:x} {} {}\n",
            device_major,
            device_minor,
            metadata.ino(),
            path.display()
        )
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_resolves_agreed_mapping() {
        let dir = tempfile::TempDir::new().unwrap();
        let executable = dir.path().join("codewhale");
        write_test_executable(&executable);
        let maps = test_android_mapping_line(&executable, "r-xp");

        let resolved =
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &executable)
                .unwrap();

        assert_eq!(resolved, executable.canonicalize().unwrap());
        assert_eq!(update_plan_for_exe(&resolved).target_paths[0], resolved);
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_canonicalizes_symlink_and_sibling_policy() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::TempDir::new().unwrap();
        let canonical_dir = dir.path().join("canonical");
        let install_dir = dir.path().join("install");
        std::fs::create_dir(&canonical_dir).unwrap();
        std::fs::create_dir(&install_dir).unwrap();
        let canonical_dispatcher = canonical_dir.join("codewhale");
        let canonical_tui = canonical_dir.join("codewhale-tui");
        let invoked = install_dir.join("codewhale");
        write_test_executable(&canonical_dispatcher);
        write_test_executable(&canonical_tui);
        symlink(&canonical_dispatcher, &invoked).unwrap();
        let maps = test_android_mapping_line(&invoked, "r-xp");

        let resolved =
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &invoked).unwrap();
        let target_paths = update_plan_for_exe(&resolved).target_paths;

        assert_eq!(
            target_paths,
            vec![
                canonical_dispatcher.canonicalize().unwrap(),
                canonical_tui.canonicalize().unwrap()
            ]
        );
        assert!(!target_paths.contains(&invoked));
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_requires_marker_mapping() {
        let dir = tempfile::TempDir::new().unwrap();
        let executable = dir.path().join("codewhale");
        write_test_executable(&executable);
        let maps = test_android_mapping_line(&executable, "r-xp");

        let error = resolve_android_loaded_executable_report(&maps, 0x3000, &executable)
            .expect_err("a marker outside every mapping must fail closed");

        assert!(
            error.to_string().contains("no /proc/self/maps row"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_requires_executable_mapping() {
        let dir = tempfile::TempDir::new().unwrap();
        let executable = dir.path().join("codewhale");
        write_test_executable(&executable);
        let maps = test_android_mapping_line(&executable, "rw-p");

        let error =
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &executable)
                .expect_err("a non-executable marker mapping must fail closed");

        assert!(
            error
                .to_string()
                .contains("mapping for updater marker is not executable"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_rejects_anonymous_mapping() {
        let dir = tempfile::TempDir::new().unwrap();
        let executable = dir.path().join("codewhale");
        write_test_executable(&executable);
        let maps = "1000-2000 r-xp 00000000 00:00 0\n";

        let error =
            resolve_android_loaded_executable_report(maps, TEST_ANDROID_MARKER, &executable)
                .expect_err("an anonymous marker mapping must fail closed");

        assert!(
            error.to_string().contains("has no file inode"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_rejects_relative_or_deleted_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let executable = dir.path().join("codewhale");
        write_test_executable(&executable);
        let metadata = std::fs::metadata(&executable).unwrap();
        use std::os::unix::fs::MetadataExt;
        let (device_major, device_minor) = android_device_parts(metadata.dev());
        let relative_maps = format!(
            "1000-2000 r-xp 00000000 {:x}:{:x} {} codewhale\n",
            device_major,
            device_minor,
            metadata.ino()
        );
        let deleted = PathBuf::from(format!("{} (deleted)", executable.display()));

        let relative_error = resolve_android_loaded_executable_report(
            &relative_maps,
            TEST_ANDROID_MARKER,
            &executable,
        )
        .expect_err("a relative maps pathname must fail closed");
        let deleted_error = resolve_android_loaded_executable_report(
            &test_android_mapping_line(&executable, "r-xp"),
            TEST_ANDROID_MARKER,
            &deleted,
        )
        .expect_err("a deleted dladdr pathname must fail closed");

        assert!(relative_error.to_string().contains("non-absolute"));
        assert!(deleted_error.to_string().contains("deleted loaded image"));
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_rejects_linker_and_symlink_to_linker() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::TempDir::new().unwrap();
        let runtime_linker = dir.path().join("linker64");
        let invoked = dir.path().join("codewhale");
        write_test_executable(&runtime_linker);
        symlink(&runtime_linker, &invoked).unwrap();
        let maps = test_android_mapping_line(&invoked, "r-xp");

        let direct_error = resolve_android_loaded_executable_report(
            &maps,
            TEST_ANDROID_MARKER,
            Path::new("/system/bin/linker64"),
        )
        .expect_err("a directly reported Bionic linker must fail closed");
        let symlink_error =
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &invoked)
                .expect_err("a symlink to a linker must fail closed");

        assert!(
            direct_error
                .to_string()
                .contains("identifies runtime linker")
        );
        assert!(
            symlink_error
                .to_string()
                .contains("resolved to runtime linker")
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_linker_name_recognizes_bionic_loader_variants() {
        for name in [
            "linker",
            "linker64",
            "linker_asan",
            "linker_asan64",
            "linker_hwasan",
            "linker_hwasan64",
        ] {
            assert!(
                is_android_linker_name(
                    Path::new("/apex/com.android.runtime/bin")
                        .join(name)
                        .as_path()
                ),
                "{name} must never become an updater target"
            );
        }
        assert!(!is_android_linker_name(Path::new("codewhale")));
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_rejects_authority_disagreement() {
        let dir = tempfile::TempDir::new().unwrap();
        let mapped = dir.path().join("mapped-codewhale");
        let dladdr = dir.path().join("dladdr-codewhale");
        write_test_executable(&mapped);
        write_test_executable(&dladdr);
        let maps = test_android_mapping_line(&mapped, "r-xp");

        let error = resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &dladdr)
            .expect_err("dladdr and maps path disagreement must fail closed");

        assert!(
            error.to_string().contains("authorities disagree"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_rejects_non_executable_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let executable = dir.path().join("codewhale");
        std::fs::write(&executable, b"not executable").unwrap();
        let maps = test_android_mapping_line(&executable, "r-xp");

        let error =
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &executable)
                .expect_err("a non-executable target file must fail closed");

        assert!(
            error.to_string().contains("not an executable regular file"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_rejects_device_inode_mismatch() {
        let dir = tempfile::TempDir::new().unwrap();
        let executable = dir.path().join("codewhale");
        write_test_executable(&executable);
        let metadata = std::fs::metadata(&executable).unwrap();
        use std::os::unix::fs::MetadataExt;
        let (device_major, device_minor) = android_device_parts(metadata.dev());
        let maps = format!(
            "1000-2000 r-xp 00000000 {:x}:{:x} {} {}\n",
            device_major,
            device_minor,
            metadata.ino() + 1,
            executable.display()
        );

        let error =
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &executable)
                .expect_err("a different maps device/inode must fail closed");

        assert!(
            error.to_string().contains("loaded-image identity changed"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_loaded_image_recheck_detects_pre_replace_swap() {
        let dir = tempfile::TempDir::new().unwrap();
        let candidate = dir.path().join("codewhale");
        let replacement = dir.path().join("replacement");
        write_test_executable(&candidate);
        let maps = test_android_mapping_line(&candidate, "r-xp");

        write_test_executable(&replacement);
        std::fs::rename(&replacement, &candidate).unwrap();
        let error =
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &candidate)
                .expect_err("a path swap after download must fail before replacement");

        assert!(
            error.to_string().contains("loaded-image identity changed"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn android_identity_preflight_prevents_all_paired_replacements() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("codewhale");
        let sibling = dir.path().join("codewhale-tui");
        let swapped_primary = dir.path().join("swapped-primary");

        write_test_executable(&primary);
        std::fs::write(&primary, b"original running primary").unwrap();
        let maps = test_android_mapping_line(&primary, "r-xp");
        write_test_executable(&sibling);
        std::fs::write(&sibling, b"original sibling").unwrap();
        write_test_executable(&swapped_primary);
        std::fs::write(&swapped_primary, b"externally swapped primary").unwrap();
        std::fs::rename(&swapped_primary, &primary).unwrap();

        let target_paths = vec![primary.clone(), sibling.clone()];
        let error = replace_verified_downloads(&target_paths, b"downloaded binary", |_| {
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &primary)
                .map(|_| ())
        })
        .expect_err("identity mismatch must fail before either binary changes");

        assert!(
            error.to_string().contains("loaded-image identity changed"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            std::fs::read(&primary).unwrap(),
            b"externally swapped primary"
        );
        assert_eq!(std::fs::read(&sibling).unwrap(), b"original sibling");
    }

    #[cfg(unix)]
    #[test]
    fn android_identity_recheck_before_sibling_prevents_pair_split() {
        use std::cell::Cell;

        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("codewhale");
        let sibling = dir.path().join("codewhale-tui");
        let swapped_primary = dir.path().join("swapped-primary");
        write_test_executable(&primary);
        std::fs::write(&primary, b"original running primary").unwrap();
        let maps = test_android_mapping_line(&primary, "r-xp");
        write_test_executable(&sibling);
        std::fs::write(&sibling, b"original sibling").unwrap();
        write_test_executable(&swapped_primary);
        std::fs::write(&swapped_primary, b"externally swapped primary").unwrap();

        let target_paths = vec![primary.clone(), sibling.clone()];
        let validation_calls = Cell::new(0);
        let error = replace_verified_downloads(&target_paths, b"downloaded binary", |_| {
            let call = validation_calls.get() + 1;
            validation_calls.set(call);
            if call <= target_paths.len() {
                return Ok(());
            }
            std::fs::rename(&swapped_primary, &primary).unwrap();
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &primary)
                .map(|_| ())
        })
        .expect_err("identity mismatch must fail before the staged sibling persists");

        assert_eq!(validation_calls.get(), 3);
        assert!(
            error.to_string().contains("loaded-image identity changed"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            std::fs::read(&primary).unwrap(),
            b"externally swapped primary"
        );
        assert_eq!(std::fs::read(&sibling).unwrap(), b"original sibling");
    }

    #[cfg(unix)]
    #[test]
    fn android_identity_jit_recheck_runs_after_staging_before_persist() {
        use std::cell::Cell;

        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("codewhale");
        let swapped_primary = dir.path().join("swapped-primary");
        write_test_executable(&primary);
        std::fs::write(&primary, b"original running primary").unwrap();
        let maps = test_android_mapping_line(&primary, "r-xp");
        write_test_executable(&swapped_primary);
        std::fs::write(&swapped_primary, b"externally swapped primary").unwrap();

        let target_paths = vec![primary.clone()];
        let validation_calls = Cell::new(0);
        let error = replace_verified_downloads(&target_paths, b"downloaded binary", |_| {
            let call = validation_calls.get() + 1;
            validation_calls.set(call);
            if call == 1 {
                return Ok(());
            }
            std::fs::rename(&swapped_primary, &primary).unwrap();
            resolve_android_loaded_executable_report(&maps, TEST_ANDROID_MARKER, &primary)
                .map(|_| ())
        })
        .expect_err("the post-staging identity swap must fail before persist");

        assert_eq!(validation_calls.get(), 2);
        assert!(
            error.to_string().contains("loaded-image identity changed"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            std::fs::read(&primary).unwrap(),
            b"externally swapped primary"
        );
        assert!(
            std::fs::read_dir(dir.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".codewhale-update-")
            }),
            "failed validation must clean the staged temp file"
        );
    }

    /// Every command name resolves to the sole implementation asset.
    #[test]
    fn every_invocation_name_uses_codewhale_release_asset() {
        for command in [
            "codewhale",
            "codewhale.exe",
            "codew",
            "codew.exe",
            "codewhale-tui",
            "CodeWhale-TUI.exe",
            "deepseek",
            "deepseek-tui",
            "other-binary",
        ] {
            assert_eq!(
                release_asset_stem_for(Path::new(command), "macos", "aarch64"),
                "codewhale-macos-arm64"
            );
        }
    }

    #[test]
    fn test_is_legacy_binary_detection() {
        assert!(is_legacy_binary(Path::new("deepseek")));
        assert!(is_legacy_binary(Path::new("deepseek-tui")));
        assert!(is_legacy_binary(Path::new("/usr/local/bin/deepseek")));
        assert!(is_legacy_binary(Path::new("/usr/local/bin/deepseek-tui")));
        assert!(is_legacy_binary(Path::new("DeepSeek.exe")));
        assert!(is_legacy_binary(Path::new("DeepSeek-TUI.exe")));
        assert!(!is_legacy_binary(Path::new("codewhale")));
        assert!(!is_legacy_binary(Path::new("codewhale-tui")));
        assert!(!is_legacy_binary(Path::new("codew")));
    }

    #[test]
    fn managed_installs_offer_github_migration_and_secondary_manager_command() {
        let npm = managed_install_warning(InstallMethod::Npm).expect("npm is package-managed");
        assert!(npm.contains("npm install -g codewhale@latest"));
        assert!(npm.contains("in-place self-update is disabled"));
        assert!(npm.contains("https://codewhale.net/install.sh"));
        assert!(npm.contains("command -v codewhale codew"));

        let brew =
            managed_install_warning(InstallMethod::Homebrew).expect("brew is package-managed");
        assert!(brew.contains("brew upgrade codewhale"));

        assert!(managed_install_warning(InstallMethod::Cargo).is_some());

        let omarchy =
            managed_install_warning(InstallMethod::Omarchy).expect("Omarchy is package-managed");
        assert!(omarchy.contains("omarchy update"));

        // A plain release binary is exactly what this updater is for.
        assert!(managed_install_warning(InstallMethod::Binary).is_none());
    }

    #[test]
    fn binary_override_cannot_authorize_a_known_package_install() {
        let _env = UpdateEnvGuard::clear();
        set_update_env(codewhale_release::install::INSTALL_METHOD_ENV, "binary");
        for (path, expected) in [
            (
                "/usr/local/lib/node_modules/codewhale/bin/codewhale",
                InstallMethod::Npm,
            ),
            (
                "/opt/homebrew/Cellar/codewhale/0.9.11/bin/codewhale",
                InstallMethod::Homebrew,
            ),
            ("/home/u/.cargo/bin/codewhale", InstallMethod::Cargo),
        ] {
            assert_eq!(InstallMethod::detect(Path::new(path)), expected);
        }
    }

    #[test]
    fn explicit_mirror_cannot_downgrade_or_download_an_older_release() {
        let _env = UpdateEnvGuard::clear();
        set_update_env(
            codewhale_release::RELEASE_BASE_URL_ENV,
            "http://127.0.0.1:0",
        );
        set_update_env(codewhale_release::UPDATE_VERSION_ENV, "0.0.1");
        for beta in [false, true] {
            run_update(beta, false, None)
                .expect("an older pinned version must return before any download");
        }
    }

    #[test]
    fn system_paths_are_protected_but_user_release_paths_are_allowed() {
        for path in [
            "/usr/bin/codewhale",
            "/usr/sbin/codewhale",
            "/bin/codewhale",
            "/nix/store/pkg/bin/codewhale",
            "/gnu/store/pkg/bin/codewhale",
            "/Users/u/scoop/apps/codewhale/codewhale.exe",
            "/Windows/System32/codewhale.exe",
        ] {
            assert!(protected_update_path(Path::new(path)), "{path}");
        }
        for path in [
            "/usr/local/bin/codewhale",
            "/home/u/.local/bin/codewhale",
            "/data/data/com.termux/files/usr/bin/codewhale",
        ] {
            assert!(!protected_update_path(Path::new(path)), "{path}");
        }
    }

    #[cfg(not(target_os = "android"))]
    fn test_update_identity(path: &Path) -> UpdateExecutableIdentity {
        UpdateExecutableIdentity {
            path: path.to_path_buf(),
            file_hash: sha256_hex(&std::fs::read(path).unwrap()),
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn unrelated_alias_fails_before_any_command_is_replaced() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("codewhale");
        let alias = dir.path().join("codew");
        std::fs::write(&primary, b"running bytes").unwrap();
        std::fs::write(&alias, b"unrelated executable").unwrap();
        let identity = test_update_identity(&primary);
        let error =
            replace_verified_downloads(&[primary.clone(), alias.clone()], b"new bytes", |target| {
                validate_primary_update_identity(&identity)?;
                validate_update_target(target, &identity)
            })
            .unwrap_err();
        assert!(error.to_string().contains("bytes differ"), "{error:#}");
        assert_eq!(std::fs::read(primary).unwrap(), b"running bytes");
        assert_eq!(std::fs::read(alias).unwrap(), b"unrelated executable");
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_primary_swap_is_detected_before_siblings_change() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("codewhale");
        let alias = dir.path().join("codew");
        for path in [&primary, &alias] {
            std::fs::write(path, b"running bytes").unwrap();
        }
        let identity = test_update_identity(&primary);
        std::fs::write(&primary, b"a different build").unwrap();
        let error =
            replace_verified_downloads(&[primary.clone(), alias.clone()], b"new bytes", |target| {
                validate_primary_update_identity(&identity)?;
                validate_update_target(target, &identity)
            })
            .unwrap_err();
        assert!(error.to_string().contains("path changed"), "{error:#}");
        assert_eq!(std::fs::read(primary).unwrap(), b"a different build");
        assert_eq!(std::fs::read(alias).unwrap(), b"running bytes");
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn same_target_symlink_survives_and_foreign_or_broken_links_are_refused() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().canonicalize().unwrap().join("codewhale");
        let alias = primary.with_file_name("codew");
        write_installed_binary(&primary, b"running bytes");
        symlink("codewhale", &alias).unwrap();
        let identity = test_update_identity(&primary);
        let plan = update_plan_for_exe(&primary);
        assert_eq!(plan.target_paths.as_slice(), std::slice::from_ref(&primary));
        replace_verified_downloads(&plan.target_paths, b"new bytes", |target| {
            validate_primary_update_identity(&identity)?;
            validate_update_target(target, &identity)
        })
        .unwrap();
        assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
        assert_eq!(std::fs::read(&alias).unwrap(), b"new bytes");
        std::fs::remove_file(&alias).unwrap();
        symlink("foreign", &alias).unwrap();
        for exists in [false, true] {
            if exists {
                std::fs::write(primary.with_file_name("foreign"), b"other").unwrap();
            }
            let plan = update_plan_for_exe(&primary);
            assert!(plan.target_paths.contains(&alias));
            let identity = test_update_identity(&primary);
            assert!(validate_update_target(&alias, &identity).is_err());
            assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
        }
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn same_target_symlink_is_kept_when_the_install_dir_is_reached_through_a_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let real_dir = root.join("real-bin");
        std::fs::create_dir(&real_dir).unwrap();
        let linked_dir = root.join("linked-bin");
        symlink(&real_dir, &linked_dir).unwrap();
        // `current_exe()` on macOS reports the path as reached, not resolved.
        let primary = linked_dir.join("codewhale");
        write_installed_binary(&primary, b"running bytes");
        symlink("codewhale", linked_dir.join("codew")).unwrap();
        let plan = update_plan_for_exe(&primary);
        assert_eq!(plan.target_paths.as_slice(), std::slice::from_ref(&primary));
    }

    #[test]
    fn legacy_binary_message_gives_copy_pasteable_migration_steps() {
        let message = legacy_binary_message(Path::new("/usr/local/bin/deepseek-tui"));

        assert!(message.contains("legacy deepseek/deepseek-tui command name"));
        assert!(message.contains("canonical `codewhale` command"));
        assert!(message.contains("DeepSeek provider support"));
        assert!(message.contains("is unchanged"));
        assert!(message.contains(GITHUB_MIGRATION_HELP));
        assert!(!message.contains("This update will install"));
        assert!(message.contains("command -v codewhale codew"));
        assert!(message.contains("package manager"));
        assert!(!message.contains("uninstall"));
        assert!(message.contains("https://github.com/codewhale-hq/CodeWhale/releases/latest"));
    }

    #[test]
    fn legacy_dispatcher_update_targets_canonical_compatibility_commands() {
        let dir = tempfile::TempDir::new().unwrap();
        let dispatcher = dir
            .path()
            .join(format!("deepseek{}", std::env::consts::EXE_SUFFIX));
        let tui = dir
            .path()
            .join(format!("deepseek-tui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&dispatcher, b"legacy dispatcher").unwrap();
        std::fs::write(&tui, b"legacy tui").unwrap();

        let plan = update_plan_for_exe(&dispatcher);

        assert_eq!(
            plan.target_paths,
            vec![
                dir.path()
                    .join(format!("codewhale{}", std::env::consts::EXE_SUFFIX)),
                dir.path()
                    .join(format!("codewhale-tui{}", std::env::consts::EXE_SUFFIX))
            ]
        );
        assert!(plan.asset_stem.starts_with("codewhale-"));
        assert!(!plan.asset_stem.starts_with("codewhale-tui-"));
    }

    #[test]
    fn legacy_tui_update_targets_canonical_compatibility_commands() {
        let dir = tempfile::TempDir::new().unwrap();
        let dispatcher = dir
            .path()
            .join(format!("deepseek{}", std::env::consts::EXE_SUFFIX));
        let tui = dir
            .path()
            .join(format!("deepseek-tui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&dispatcher, b"legacy dispatcher").unwrap();
        std::fs::write(&tui, b"legacy tui").unwrap();

        let plan = update_plan_for_exe(&tui);

        assert_eq!(
            plan.target_paths,
            vec![
                dir.path()
                    .join(format!("codewhale{}", std::env::consts::EXE_SUFFIX)),
                dir.path()
                    .join(format!("codewhale-tui{}", std::env::consts::EXE_SUFFIX))
            ]
        );
        assert!(plan.asset_stem.starts_with("codewhale-"));
        assert!(!plan.asset_stem.starts_with("codewhale-tui-"));
    }

    #[test]
    fn test_release_asset_stem_for_supported_platforms() {
        let cases = [
            ("codewhale", "macos", "aarch64", "codewhale-macos-arm64"),
            ("codewhale", "macos", "x86_64", "codewhale-macos-x64"),
            ("codewhale", "linux", "x86_64", "codewhale-linux-x64"),
            ("codewhale", "windows", "x86_64", "codewhale-windows-x64"),
            ("codewhale", "windows", "aarch64", "codewhale-windows-arm64"),
            ("codew", "macos", "aarch64", "codewhale-macos-arm64"),
            ("codewhale-tui", "linux", "x86_64", "codewhale-linux-x64"),
        ];

        for (exe, os, arch, expected) in cases {
            assert_eq!(release_asset_stem_for(Path::new(exe), os, arch), expected);
        }
    }

    #[test]
    fn update_plan_includes_existing_compatibility_tui_for_primary() {
        let dir = tempfile::TempDir::new().unwrap();
        let dispatcher = dir
            .path()
            .join(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
        let tui = dir
            .path()
            .join(format!("codewhale-tui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&dispatcher, b"dispatcher").unwrap();
        std::fs::write(&tui, b"tui").unwrap();

        let plan = update_plan_for_exe(&dispatcher);
        let paths = plan
            .target_paths
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();

        assert_eq!(paths, vec![dispatcher.as_path(), tui.as_path()]);
        assert!(plan.asset_stem.starts_with("codewhale-"));
        assert!(!plan.asset_stem.starts_with("codewhale-tui-"));
    }

    #[test]
    fn update_plan_skips_missing_compatibility_commands() {
        let dir = tempfile::TempDir::new().unwrap();
        let dispatcher = dir
            .path()
            .join(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&dispatcher, b"dispatcher").unwrap();

        let plan = update_plan_for_exe(&dispatcher);

        assert_eq!(plan.target_paths, vec![dispatcher]);
        assert!(plan.asset_stem.starts_with("codewhale-"));
    }

    #[test]
    fn v094_three_command_install_updates_every_path_from_primary_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir
            .path()
            .join(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
        let codew = dir
            .path()
            .join(format!("codew{}", std::env::consts::EXE_SUFFIX));
        let legacy_tui = dir
            .path()
            .join(format!("codewhale-tui{}", std::env::consts::EXE_SUFFIX));
        for path in [&primary, &codew, &legacy_tui] {
            write_installed_binary(path, b"v0.9.4 old bytes");
        }

        let plan = update_plan_for_exe(&primary);
        assert_eq!(
            plan.target_paths,
            vec![primary.clone(), codew.clone(), legacy_tui.clone()]
        );
        assert!(plan.asset_stem.starts_with("codewhale-"));
        assert!(!plan.asset_stem.contains("codewhale-tui"));

        replace_verified_downloads(&plan.target_paths, b"v0.9.5 primary bytes", |_| Ok(()))
            .unwrap();

        for path in [&primary, &codew, &legacy_tui] {
            assert_eq!(std::fs::read(path).unwrap(), b"v0.9.5 primary bytes");
        }
        assert_ne!(std::fs::read(codew).unwrap(), b"v0.9.4 old bytes");
    }

    #[test]
    fn direct_alias_invocation_keeps_running_path_first_and_updates_primary() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir
            .path()
            .join(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
        let codew = dir
            .path()
            .join(format!("codew{}", std::env::consts::EXE_SUFFIX));
        let legacy_tui = dir
            .path()
            .join(format!("codewhale-tui{}", std::env::consts::EXE_SUFFIX));
        for invoked in [&codew, &legacy_tui] {
            for path in [&primary, &codew, &legacy_tui] {
                write_installed_binary(path, b"old");
            }
            let plan = update_plan_for_exe(invoked);
            assert_eq!(plan.target_paths.first(), Some(invoked));
            assert!(plan.target_paths.contains(&primary));
            assert!(plan.target_paths.contains(&codew));
            assert!(plan.target_paths.contains(&legacy_tui));
            assert!(plan.asset_stem.starts_with("codewhale-"));
            assert!(!plan.asset_stem.starts_with("codewhale-tui-"));

            replace_verified_downloads(&plan.target_paths, b"new primary bytes", |_| Ok(()))
                .unwrap();
            for path in [&primary, &codew, &legacy_tui] {
                assert_eq!(std::fs::read(path).unwrap(), b"new primary bytes");
            }
        }
    }

    #[test]
    fn test_asset_matching_accepts_only_the_raw_binary() {
        for (asset, binary, expected) in [
            ("codewhale-macos-arm64", "codewhale-macos-arm64", true),
            (
                "codewhale-tui-windows-x64.exe",
                "codewhale-tui-windows-x64",
                true,
            ),
            (
                "codewhale-macos-arm64.tar.gz",
                "codewhale-macos-arm64",
                false,
            ),
            ("codewhale-macos-arm64.zip", "codewhale-macos-arm64", false),
            ("codewhale-macos-arm64.sig", "codewhale-macos-arm64", false),
            (
                "codewhale-tui-windows-x64.exe.sha256",
                "codewhale-tui-windows-x64",
                false,
            ),
            ("codewhale-macos-aarch64", "codewhale-macos-arm64", false),
        ] {
            assert_eq!(
                asset_is_exact_platform_binary(asset, binary),
                expected,
                "{asset} vs {binary}"
            );
        }
    }

    #[test]
    fn select_platform_asset_prefers_bare_binary_over_archive() {
        let release = Release {
            tag_name: "v0.8.8".to_string(),
            prerelease: false,
            assets: vec![
                Asset {
                    name: "codewhale-macos-arm64.tar.gz".to_string(),
                    browser_download_url: "https://example.invalid/codewhale-macos-arm64.tar.gz"
                        .to_string(),
                },
                Asset {
                    name: "codewhale-macos-arm64".to_string(),
                    browser_download_url: "https://example.invalid/codewhale-macos-arm64"
                        .to_string(),
                },
            ],
        };

        let asset =
            select_platform_asset(&release, "codewhale-macos-arm64").expect("platform asset");

        assert_eq!(asset.name, "codewhale-macos-arm64");
    }

    /// Audit R02-04: the updater installs the downloaded bytes verbatim, so a
    /// release that ships only an archive/signature/sidecar for this platform
    /// has no installable asset rather than one that bricks the binary.
    #[test]
    fn select_platform_asset_never_substitutes_an_archive_or_sidecar() {
        let release = Release {
            tag_name: "v0.8.8".to_string(),
            prerelease: false,
            assets: ["tar.gz", "zip", "sig", "sbom.json"]
                .into_iter()
                .map(|ext| Asset {
                    name: format!("codewhale-macos-arm64.{ext}"),
                    browser_download_url: format!(
                        "https://example.invalid/codewhale-macos-arm64.{ext}"
                    ),
                })
                .collect(),
        };

        assert!(select_platform_asset(&release, "codewhale-macos-arm64").is_none());
    }

    #[test]
    fn test_sha256_hex_known_value() {
        let data = b"hello";
        let hash = sha256_hex(data);
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_sha256_hex_empty() {
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn glibc_version_parser_reads_getconf_and_symbol_text() {
        assert_eq!(
            parse_glibc_version("glibc 2.35\n"),
            Some(GlibcVersion::new(2, 35, 0))
        );
        assert_eq!(
            parse_glibc_version("requires GLIBC_2.39"),
            Some(GlibcVersion::new(2, 39, 0))
        );
        assert_eq!(parse_glibc_version("not glibc"), None);
    }

    #[test]
    fn highest_required_glibc_finds_highest_binary_symbol() {
        let bytes = b"\0GLIBC_2.17\0other\0GLIBC_2.39\0GLIBC_2.35";

        assert_eq!(
            highest_required_glibc(bytes),
            Some(GlibcVersion::new(2, 39, 0))
        );
    }

    #[test]
    fn glibc_compatibility_message_is_codewhale_branded_and_actionable() {
        let message = glibc_compatibility_message(
            "codewhale-linux-x64",
            GlibcVersion::new(2, 39, 0),
            Some(GlibcVersion::new(2, 35, 0)),
        );

        assert!(message.contains("Prebuilt Codewhale asset `codewhale-linux-x64`"));
        assert!(message.contains("requires GLIBC_2.39"));
        assert!(message.contains("this system has glibc 2.35"));
        assert!(message.contains("cargo install codewhale-cli --locked"));
        assert!(message.contains("(x64 and arm64) are static musl builds"));
        assert!(!message.contains("GNU "), "no stale GNU-build claim");
    }

    #[test]
    fn parse_checksum_manifest_accepts_sha256sum_format() {
        let manifest = "\
2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  codewhale-macos-arm64
E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855  *codewhale-windows-x64.exe
";
        let checksums = parse_checksum_manifest(manifest).expect("valid manifest");

        assert_eq!(
            checksums.get("codewhale-macos-arm64").map(String::as_str),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        assert_eq!(
            checksums
                .get("codewhale-windows-x64.exe")
                .map(String::as_str),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    #[test]
    fn parse_checksum_manifest_rejects_malformed_lines() {
        let err = parse_checksum_manifest("not-a-hash  codewhale-macos-arm64")
            .expect_err("invalid manifest line should fail");
        assert!(
            err.to_string().contains("invalid SHA256 manifest line"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn expected_sha256_from_manifest_requires_matching_asset() {
        let manifest =
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  other-asset\n";
        let err = expected_sha256_from_manifest(manifest, "codewhale-macos-arm64")
            .expect_err("missing asset should fail");
        assert!(
            err.to_string()
                .contains("checksum manifest is missing codewhale-macos-arm64"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn test_replace_binary_creates_and_replaces() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("codewhale-test");
        // Write initial content
        write_installed_binary(&target, b"old binary");

        replace_binary(&target, b"new binary content").unwrap();
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "new binary content");
    }

    #[test]
    fn test_replace_binary_creates_new_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("codewhale-new-test");

        replace_binary(&target, b"fresh binary").unwrap();
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "fresh binary");
    }

    fn assert_no_staged_temp_files(dir: &Path) {
        assert!(
            std::fs::read_dir(dir).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".codewhale-update-")
            }),
            "a failed permission setup must clean the staged temp file"
        );
    }

    /// Regression test for #5727: a permission-setup failure on the staged
    /// binary must abort the update before the existing target is replaced.
    #[test]
    fn permission_failure_on_existing_target_aborts_before_replacement() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("codewhale-test");
        write_installed_binary(&target, b"old binary");

        let error = replace_binary_with_validation_and_permission_setter(
            &target,
            b"new binary content",
            || Ok(()),
            |_, _| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        )
        .expect_err("a chmod failure must fail the update");

        assert!(
            error
                .to_string()
                .contains("failed to set permissions on staged update"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old binary",
            "the working binary must survive a permission-setup failure"
        );
        assert_no_staged_temp_files(dir.path());
    }

    /// Regression test for #5727, new-target path: when no binary exists yet
    /// the staged file still needs its 0o755 mode, and a chmod failure must
    /// abort instead of installing a non-executable file.
    #[cfg(unix)]
    #[test]
    fn permission_failure_on_new_target_aborts_install() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("codewhale-new-test");

        let error = replace_binary_with_validation_and_permission_setter(
            &target,
            b"fresh binary",
            || Ok(()),
            |_, _| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        )
        .expect_err("a chmod failure must fail a fresh install");

        assert!(
            error
                .to_string()
                .contains("failed to set permissions on staged update"),
            "unexpected error: {error:#}"
        );
        assert!(
            !target.exists(),
            "a failed fresh install must not leave a target behind"
        );
        assert_no_staged_temp_files(dir.path());
    }

    /// Regression test for #5727: even when permission setup reports success,
    /// a staged binary without an executable mode must never replace the
    /// working target.
    #[cfg(unix)]
    #[test]
    fn non_executable_staged_update_aborts_before_replacement() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("codewhale-test");
        write_test_executable(&target);
        std::fs::write(&target, b"old binary").unwrap();

        // A no-op setter models a chmod that claims success without sticking,
        // leaving the staged temp file at its default non-executable 0o600.
        let error = replace_binary_with_validation_and_permission_setter(
            &target,
            b"new binary content",
            || Ok(()),
            |_, _| Ok(()),
        )
        .expect_err("a non-executable staged binary must fail the update");

        assert!(
            error.to_string().contains("is not executable"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old binary",
            "the working binary must survive a non-executable staged update"
        );
        assert_no_staged_temp_files(dir.path());
    }

    /// Mocked GitHub release payload covering the sole implementation binary
    /// across the published platform/arch matrix, plus a checksum sibling that
    /// must never be picked as the binary.
    fn mocked_release() -> Release {
        let json = r#"{
          "tag_name": "v0.8.8",
          "assets": [
            { "name": "codewhale-linux-x64",          "browser_download_url": "https://example.invalid/codewhale-linux-x64" },
            { "name": "codewhale-macos-x64",          "browser_download_url": "https://example.invalid/codewhale-macos-x64" },
            { "name": "codewhale-macos-arm64",        "browser_download_url": "https://example.invalid/codewhale-macos-arm64" },
            { "name": "codewhale-windows-x64.exe",    "browser_download_url": "https://example.invalid/codewhale-windows-x64.exe" },
            { "name": "codewhale-windows-x64.exe.sha256", "browser_download_url": "https://example.invalid/codewhale-windows-x64.exe.sha256" },
            { "name": "codewhale-windows-arm64.exe",  "browser_download_url": "https://example.invalid/codewhale-windows-arm64.exe" }
          ]
        }"#;
        serde_json::from_str(json).expect("mock release JSON")
    }

    #[test]
    fn mocked_release_selects_dispatcher_asset_for_supported_platforms() {
        let release = mocked_release();
        let cases = [
            ("macos", "aarch64", "codewhale-macos-arm64"),
            ("macos", "x86_64", "codewhale-macos-x64"),
            ("linux", "x86_64", "codewhale-linux-x64"),
            ("windows", "x86_64", "codewhale-windows-x64.exe"),
            ("windows", "aarch64", "codewhale-windows-arm64.exe"),
        ];

        for (os, arch, expected) in cases {
            let stem = release_asset_stem_for(Path::new("/usr/local/bin/codewhale"), os, arch);
            let asset = select_platform_asset(&release, &stem)
                .unwrap_or_else(|| panic!("no asset for {os}/{arch} (stem {stem})"));
            assert_eq!(asset.name, expected, "{os}/{arch}");
        }
    }

    #[test]
    fn mocked_release_selects_primary_asset_when_compatibility_alias_invokes_update() {
        let release = mocked_release();
        let stem = release_asset_stem_for(
            Path::new("/usr/local/bin/codewhale-tui"),
            "macos",
            "aarch64",
        );
        let asset = select_platform_asset(&release, &stem).expect("primary platform asset");
        assert_eq!(asset.name, "codewhale-macos-arm64");

        let windows_stem = release_asset_stem_for(Path::new("C:\\codew.exe"), "windows", "aarch64");
        let windows_asset =
            select_platform_asset(&release, &windows_stem).expect("Windows ARM64 primary asset");
        assert_eq!(windows_asset.name, "codewhale-windows-arm64.exe");
    }

    #[test]
    fn android_arm64_maps_to_android_release_assets() {
        // The generic format!("{prefix}-{os}-{arch}") path naturally produces
        // Android asset stems. Verify every supported command name resolves to
        // the primary Android asset, never Linux or a removed TUI asset (#4241).
        assert_eq!(
            release_asset_stem_for_prefix("codewhale", "android", "aarch64"),
            "codewhale-android-arm64"
        );
        assert_eq!(
            release_asset_stem_for(Path::new("codewhale-tui"), "android", "aarch64"),
            "codewhale-android-arm64"
        );
        assert_eq!(
            release_asset_stem_for(Path::new("codew"), "android", "aarch64"),
            "codewhale-android-arm64"
        );
    }

    #[test]
    fn ensure_supported_release_target_accepts_android() {
        // Android/Termux is a supported release target (#4241).
        assert!(ensure_supported_release_target("android", "aarch64").is_ok());
    }

    #[test]
    fn android_release_assets_never_select_linux_arm64() {
        // Sanity: the stem formatter must never produce a linux-* stem for android.
        let stem = release_asset_stem_for_prefix("codewhale", "android", "aarch64");
        assert!(
            !stem.contains("linux"),
            "android stem must not contain linux: {stem}"
        );
    }

    #[test]
    fn mirror_release_uses_base_url_and_platform_assets() {
        let release = release_from_mirror_base_url(
            "https://mirror.example/releases/v0.8.36/",
            "0.8.36",
            "linux",
            "x86_64",
        );

        assert_eq!(release.tag_name, "v0.8.36");
        assert_eq!(release.assets[0].name, CHECKSUM_MANIFEST_ASSET);
        assert_eq!(
            release.assets[0].browser_download_url,
            "https://mirror.example/releases/v0.8.36/codewhale-artifacts-sha256.txt"
        );

        let dispatcher =
            select_platform_asset(&release, "codewhale-linux-x64").expect("dispatcher asset");
        assert_eq!(
            dispatcher.browser_download_url,
            "https://mirror.example/releases/v0.8.36/codewhale-linux-x64"
        );
        assert_eq!(release.assets.len(), 2);
        assert!(
            select_platform_asset(&release, "codewhale-tui-linux-x64").is_none(),
            "mirror fallback must not synthesize a removed TUI asset"
        );
    }

    #[test]
    fn mirror_release_uses_windows_exe_asset_names() {
        let release = release_from_mirror_base_url(
            "https://mirror.example/releases/v0.8.36",
            "v0.8.36",
            "windows",
            "x86_64",
        );

        assert_eq!(release.tag_name, "v0.8.36");
        assert!(
            select_platform_asset(&release, "codewhale-windows-x64")
                .is_some_and(|asset| asset.name == "codewhale-windows-x64.exe")
        );
        assert!(select_platform_asset(&release, "codewhale-tui-windows-x64").is_none());

        let arm_release = release_from_mirror_base_url(
            "https://mirror.example/releases/v0.9.1",
            "v0.9.1",
            "windows",
            "aarch64",
        );
        assert!(
            select_platform_asset(&arm_release, "codewhale-windows-arm64")
                .is_some_and(|asset| asset.name == "codewhale-windows-arm64.exe")
        );
    }

    #[test]
    fn github_release_url_parser_extracts_tag() {
        let url =
            reqwest::Url::parse("https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.8.61")
                .unwrap();

        assert_eq!(
            release_tag_from_github_release_url(&url).as_deref(),
            Some("v0.8.61")
        );
    }

    #[test]
    fn github_release_download_fallback_uses_deterministic_asset_urls() {
        let release = release_from_github_download_tag("0.8.61", "macos", "aarch64");

        assert_eq!(release.tag_name, "v0.8.61");
        assert_eq!(
            release.assets[0].browser_download_url,
            "https://github.com/codewhale-hq/CodeWhale/releases/download/v0.8.61/codewhale-artifacts-sha256.txt"
        );
        let dispatcher =
            select_platform_asset(&release, "codewhale-macos-arm64").expect("dispatcher asset");
        assert_eq!(
            dispatcher.browser_download_url,
            "https://github.com/codewhale-hq/CodeWhale/releases/download/v0.8.61/codewhale-macos-arm64"
        );
        assert_eq!(release.assets.len(), 2);
        assert!(select_platform_asset(&release, "codewhale-tui-macos-arm64").is_none());
    }

    #[test]
    fn latest_stable_redirect_fallback_reads_tag_url() {
        let (url, request_rx, handle) = serve_http_once("200 OK", "text/html", b"<html></html>");
        let tag_url = url.replace("/release", "/codewhale-hq/CodeWhale/releases/tag/v9.9.9");

        let tag = fetch_latest_stable_tag_from_redirect_url(&tag_url, None)
            .expect("tag should parse from final URL");

        assert_eq!(tag, "v9.9.9");
        let request = request_rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /codewhale-hq/CodeWhale/releases/tag/v9.9.9 "),
            "got {request:?}"
        );
        handle.join().expect("test server thread");
    }

    #[test]
    fn github_release_html_parser_skips_empty_first_marker() {
        let body = r#"
            <a href="/codewhale-hq/CodeWhale/releases/tag/?expanded=true">generic</a>
            <a href="/codewhale-hq/CodeWhale/releases/tag/v9.9.9">latest</a>
        "#;

        assert_eq!(
            release_tag_from_github_release_html(body).as_deref(),
            Some("v9.9.9")
        );
    }

    #[test]
    fn cnb_release_base_url_includes_tag_directory() {
        assert_eq!(
            codewhale_release::cnb_release_base_url("0.8.47"),
            "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v0.8.47"
        );
        assert_eq!(
            codewhale_release::cnb_release_base_url("v0.8.47"),
            "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v0.8.47"
        );
    }

    #[test]
    fn stable_update_is_needed_only_when_latest_is_newer() {
        assert!(update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.8.46").unwrap());
        assert!(update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.9.0-beta.1").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Stable, "0.8.45", "v0.8.45").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Stable, "0.9.0", "v0.9.0-beta.1").unwrap());
        assert!(
            !update_is_needed(ReleaseChannel::Stable, "0.9.0-beta.2", "v0.9.0-beta.1").unwrap()
        );
    }

    #[test]
    fn beta_update_allows_switching_from_same_stable_to_beta() {
        assert!(update_is_needed(ReleaseChannel::Beta, "1.0.0", "v1.0.0-beta.2").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.2", "v1.0.0-beta.2").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.3", "v1.0.0-beta.2").unwrap());
        assert!(update_is_needed(ReleaseChannel::Beta, "1.0.0-beta.2", "v1.0.0-beta.3").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "2.0.0", "v1.0.0-beta.3").unwrap());
        assert!(!update_is_needed(ReleaseChannel::Beta, "1.0.0-rc.1", "v1.0.0-beta.3").unwrap());
    }

    #[test]
    fn parse_release_version_accepts_tags_and_build_suffixes() {
        assert_eq!(
            codewhale_release::parse_release_version("v0.9.0-beta.1").unwrap(),
            semver::Version::parse("0.9.0-beta.1").unwrap()
        );
        assert_eq!(
            codewhale_release::parse_release_version("0.8.45 (abcdef123456)").unwrap(),
            semver::Version::parse("0.8.45").unwrap()
        );
    }

    #[test]
    fn beta_release_detection_requires_beta_tag() {
        let rc_prerelease = Release {
            tag_name: "v0.9.0-rc.1".to_string(),
            prerelease: true,
            assets: vec![],
        };
        let beta_tag = Release {
            tag_name: "v0.9.0-beta.1".to_string(),
            prerelease: false,
            assets: vec![],
        };
        let stable = Release {
            tag_name: "v0.9.0".to_string(),
            prerelease: false,
            assets: vec![],
        };

        assert!(!is_beta_tag(&rc_prerelease.tag_name));
        assert!(is_beta_tag(&beta_tag.tag_name));
        assert!(!is_beta_tag(&stable.tag_name));
    }

    #[test]
    fn update_fallback_hint_points_china_users_to_cnb_and_asset_mirrors() {
        let hint = update_network_fallback_hint();

        assert!(hint.contains(codewhale_release::CNB_REPO_URL), "{hint}");
        assert!(
            hint.contains(codewhale_release::RELEASE_BASE_URL_ENV),
            "{hint}"
        );
        assert!(
            hint.contains(codewhale_release::UPDATE_VERSION_ENV),
            "{hint}"
        );
        assert!(hint.contains("codewhale-cli"), "{hint}");
        assert!(!hint.contains("codewhale-tui --locked"), "{hint}");
    }

    fn serve_http_responses(
        responses: Vec<(&'static str, &'static str, &'static [u8])>,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        serve_http_owned_responses(
            responses
                .into_iter()
                .map(|(status, content_type, body)| (status, content_type, body.to_vec()))
                .collect(),
        )
    }

    fn serve_http_owned_responses(
        responses: Vec<(&'static str, &'static str, Vec<u8>)>,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let (request_tx, request_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            for (status, content_type, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept test request");
                let mut buf = [0_u8; 4096];
                let n = stream.read(&mut buf).expect("read test request");
                request_tx
                    .send(String::from_utf8_lossy(&buf[..n]).to_string())
                    .expect("send captured request");

                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .expect("write test response headers");
                stream.write_all(&body).expect("write test response body");
            }
        });

        (format!("http://{addr}/release"), request_rx, handle)
    }

    fn serve_http_once(
        status: &'static str,
        content_type: &'static str,
        body: &'static [u8],
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        serve_http_responses(vec![(status, content_type, body)])
    }

    // Ordered source selection is deterministic and offline. A successful
    // GitHub manifest must not cause any mirror request.
    fn scripted_manifest_fetcher(
        script: Vec<(&'static str, Result<String, String>)>,
    ) -> Arc<ManifestFetcher> {
        let script: HashMap<_, _> = script.into_iter().collect();
        Arc::new(move |candidate: &ReleaseSourceCandidate| {
            script
                .get(candidate.source.label())
                .expect("unexpected source request")
                .clone()
                .map(String::into_bytes)
                .map_err(|message| anyhow!(message))
        })
    }

    fn manifest_covering_linux_x64() -> String {
        format!(
            "{}  codewhale-linux-x64\n{}  codew-linux-x64\n",
            "a".repeat(64),
            "b".repeat(64)
        )
    }

    fn manifest_missing_linux_x64() -> String {
        format!("{}  codewhale-macos-arm64\n", "c".repeat(64))
    }

    fn github_fetched_release(tag_name: &str) -> FetchedRelease {
        FetchedRelease {
            release: Release {
                tag_name: tag_name.to_string(),
                prerelease: is_beta_tag(tag_name),
                assets: vec![
                    Asset {
                        name: "codewhale-linux-x64".to_string(),
                        browser_download_url: format!(
                            "https://github.com/codewhale-hq/CodeWhale/releases/download/{tag_name}/codewhale-linux-x64"
                        ),
                    },
                    Asset {
                        name: CHECKSUM_MANIFEST_ASSET.to_string(),
                        browser_download_url: format!(
                            "https://github.com/codewhale-hq/CodeWhale/releases/download/{tag_name}/{CHECKSUM_MANIFEST_ASSET}"
                        ),
                    },
                ],
            },
            source: UpdateReleaseSource::GitHub,
        }
    }

    fn candidates_for(
        fetched: &FetchedRelease,
        os: &str,
        arch: &str,
    ) -> Option<Vec<ReleaseSourceCandidate>> {
        proactive_source_candidates(fetched, "codewhale-linux-x64", os, arch)
    }

    fn linux_x64_candidates(tag_name: &str) -> Vec<ReleaseSourceCandidate> {
        candidates_for(&github_fetched_release(tag_name), "linux", "x86_64")
            .expect("linux x64 must try GitHub before the CNB mirror")
    }

    #[test]
    fn github_is_preferred_without_contacting_a_healthy_mirror() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requested);
        let fetch: Arc<ManifestFetcher> = Arc::new(move |candidate| {
            captured.lock().unwrap().push(candidate.source.label());
            Ok(manifest_covering_linux_x64().into_bytes())
        });
        let plan = select_release_source(linux_x64_candidates("v9.9.9"), fetch).unwrap();
        assert_eq!(plan.source, UpdateReleaseSource::GitHub);
        assert_eq!(*requested.lock().unwrap(), ["GitHub Releases"]);
        assert_eq!(
            plan.binary_url,
            "https://github.com/codewhale-hq/CodeWhale/releases/download/v9.9.9/codewhale-linux-x64"
        );
    }

    #[test]
    fn cnb_never_supplies_the_manifest_after_a_github_manifest_failure() {
        for first_answer in [
            Err("connection timed out".to_string()),
            Ok(manifest_missing_linux_x64()),
            Ok("not a checksum manifest".to_string()),
            Ok(String::new()),
        ] {
            let requested = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requested);
            let fetch: Arc<ManifestFetcher> = Arc::new(move |candidate| {
                captured.lock().unwrap().push(candidate.source.label());
                if candidate.source == UpdateReleaseSource::GitHub {
                    first_answer
                        .clone()
                        .map(String::into_bytes)
                        .map_err(|err| anyhow!(err))
                } else {
                    Ok(manifest_covering_linux_x64().into_bytes())
                }
            });
            let err = select_release_source(linux_x64_candidates("v9.9.9"), fetch)
                .expect_err("a mirror must never vouch for its own bytes");
            assert!(format!("{err:#}").contains("GitHub Releases"), "{err:#}");
            assert_eq!(*requested.lock().unwrap(), ["GitHub Releases"]);
        }
    }

    #[test]
    fn the_mirror_is_only_a_fallback_for_bytes_github_pins() {
        let fetch: Arc<ManifestFetcher> =
            Arc::new(|_| Ok(manifest_covering_linux_x64().into_bytes()));
        let plan = select_release_source(linux_x64_candidates("v9.9.9"), fetch).unwrap();
        assert_eq!(plan.source, UpdateReleaseSource::GitHub);
        assert_eq!(
            plan.fallback_binaries,
            vec![(
                UpdateReleaseSource::Cnb {
                    base_url: cnb_release_base_url("v9.9.9"),
                },
                "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v9.9.9/codewhale-linux-x64"
                    .to_string()
            )]
        );
    }

    #[test]
    fn selection_fails_closed_when_no_source_is_usable() {
        let fetch = scripted_manifest_fetcher(vec![
            ("GitHub Releases", Err("dns failure".to_string())),
            ("CNB mirror", Ok(manifest_missing_linux_x64())),
        ]);

        let err = select_release_source(linux_x64_candidates("v9.9.9"), fetch)
            .expect_err("no usable source must fail rather than download unverified bytes");
        let message = format!("{err:#}");

        assert!(
            message.contains("dns failure") && message.contains("GitHub Releases"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn only_supported_targets_have_cnb_as_a_fallback() {
        let fetched = github_fetched_release("v9.9.9");

        for (os, arch) in [
            ("linux", "aarch64"),
            ("linux", "riscv64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
            ("android", "aarch64"),
        ] {
            assert!(
                candidates_for(&fetched, os, arch).is_none(),
                "{os}/{arch} must keep its single canonical source"
            );
        }

        let raced = candidates_for(&fetched, "linux", "x86_64").expect("linux x64 fallback");
        assert_eq!(raced.len(), 2);
    }

    #[test]
    fn cnb_candidate_targets_the_exact_tag_and_platform_asset() {
        let candidate = cnb_source_candidate("v0.9.0-beta.2", "linux", "x86_64");

        assert_eq!(
            candidate.source,
            UpdateReleaseSource::Cnb {
                base_url: cnb_release_base_url("v0.9.0-beta.2"),
            }
        );
        assert_eq!(
            candidate.manifest_url,
            "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v0.9.0-beta.2/codewhale-artifacts-sha256.txt"
        );
        assert_eq!(
            candidate.binary_url,
            "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v0.9.0-beta.2/codewhale-linux-x64"
        );
        assert_eq!(candidate.binary_name, "codewhale-linux-x64");
    }

    #[test]
    fn github_candidate_addresses_the_manifest_even_when_the_payload_omits_it() {
        let release = Release {
            tag_name: "0.9.9".to_string(),
            prerelease: false,
            assets: vec![Asset {
                name: "codewhale-linux-x64".to_string(),
                browser_download_url: "https://cdn.example/codewhale-linux-x64".to_string(),
            }],
        };

        let candidate =
            github_source_candidate(&release, "codewhale-linux-x64").expect("github candidate");

        assert_eq!(
            candidate.manifest_url,
            "https://github.com/codewhale-hq/CodeWhale/releases/download/v0.9.9/codewhale-artifacts-sha256.txt"
        );
        assert_eq!(
            candidate.binary_url,
            "https://cdn.example/codewhale-linux-x64"
        );
    }

    #[test]
    fn every_single_source_platform_requires_a_checksum_manifest() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
            ("android", "aarch64"),
        ] {
            let asset_stem = release_asset_stem_for_prefix("codewhale", os, arch);
            let asset_name = release_asset_name_for_prefix("codewhale", os, arch);
            let fetched = FetchedRelease {
                release: Release {
                    tag_name: "v9.9.9".to_string(),
                    prerelease: false,
                    assets: vec![Asset {
                        name: asset_name.clone(),
                        browser_download_url: format!("https://cdn.example/{asset_name}"),
                    }],
                },
                source: UpdateReleaseSource::GitHub,
            };

            let err = single_source_download_plan(&fetched, &asset_stem, None)
                .expect_err("a missing manifest must fail before the binary download");
            let message = format!("{err:#}");
            assert!(
                message.contains("does not publish required codewhale-artifacts-sha256.txt"),
                "{os}/{arch} unexpectedly allowed an unverifiable plan: {message}"
            );
            assert!(message.contains(&asset_name), "{os}/{arch}: {message}");
        }
    }

    #[test]
    fn a_malformed_single_source_manifest_fails_before_binary_download() {
        let (manifest_url, request_rx, handle) =
            serve_http_once("200 OK", "text/plain", b"not a checksum manifest\n");
        let fetched = FetchedRelease {
            release: Release {
                tag_name: "v9.9.9".to_string(),
                prerelease: false,
                assets: vec![
                    Asset {
                        name: CHECKSUM_MANIFEST_ASSET.to_string(),
                        browser_download_url: manifest_url,
                    },
                    Asset {
                        name: "codewhale-macos-arm64".to_string(),
                        browser_download_url: "https://cdn.example/should-not-download".to_string(),
                    },
                ],
            },
            source: UpdateReleaseSource::GitHub,
        };

        let err = single_source_download_plan(&fetched, "codewhale-macos-arm64", None)
            .expect_err("a malformed manifest must fail closed");
        let message = format!("{err:#}");
        assert!(message.contains("failed to parse"), "{message}");
        assert!(
            message.contains("invalid SHA256 manifest line"),
            "{message}"
        );
        let request = request_rx.recv().expect("manifest request");
        assert!(request.starts_with("GET /release "), "got {request:?}");
        handle.join().expect("test server thread");
    }

    #[test]
    fn a_single_source_manifest_must_cover_the_exact_platform_binary() {
        let manifest = format!("{}  codewhale-linux-x64\n", "a".repeat(64));
        let (manifest_url, request_rx, handle) =
            serve_http_owned_responses(vec![("200 OK", "text/plain", manifest.into_bytes())]);
        let fetched = FetchedRelease {
            release: Release {
                tag_name: "v9.9.9".to_string(),
                prerelease: false,
                assets: vec![
                    Asset {
                        name: CHECKSUM_MANIFEST_ASSET.to_string(),
                        browser_download_url: manifest_url,
                    },
                    Asset {
                        name: "codewhale-windows-x64.exe".to_string(),
                        browser_download_url: "https://cdn.example/should-not-download.exe"
                            .to_string(),
                    },
                ],
            },
            source: UpdateReleaseSource::GitHub,
        };

        let err = single_source_download_plan(&fetched, "codewhale-windows-x64", None)
            .expect_err("a manifest for another platform must fail closed");
        let message = format!("{err:#}");
        assert!(
            message.contains("does not list codewhale-windows-x64.exe"),
            "{message}"
        );
        let request = request_rx.recv().expect("manifest request");
        assert!(request.starts_with("GET /release "), "got {request:?}");
        handle.join().expect("test server thread");
    }

    #[test]
    fn an_explicit_mirror_remains_pinned_and_verified_from_that_mirror() {
        let bytes = b"verified mirror bytes";
        let manifest = format!("{}  codewhale-macos-arm64\n", sha256_hex(bytes));
        let (url, request_rx, handle) =
            serve_http_owned_responses(vec![("200 OK", "text/plain", manifest.into_bytes())]);
        let base_url = url.trim_end_matches("/release").to_string();
        let fetched = FetchedRelease {
            release: release_from_mirror_base_url(&base_url, "9.9.9", "macos", "aarch64"),
            source: UpdateReleaseSource::Mirror {
                base_url: base_url.clone(),
            },
        };

        let plan = single_source_download_plan(&fetched, "codewhale-macos-arm64", None)
            .expect("the explicit mirror's valid manifest should produce a plan");
        assert_eq!(
            plan.source,
            UpdateReleaseSource::Mirror {
                base_url: base_url.clone(),
            }
        );
        assert_eq!(
            plan.binary_url,
            mirror_asset_url(&base_url, "codewhale-macos-arm64")
        );
        verify_downloaded_asset(&plan, bytes)
            .expect("the pinned mirror's checksum must verify its bytes");
        let request = request_rx.recv().expect("manifest request");
        assert!(
            request.starts_with("GET /codewhale-artifacts-sha256.txt "),
            "got {request:?}"
        );
        handle.join().expect("test server thread");
    }

    #[test]
    fn a_github_release_without_this_platform_leaves_cnb_as_the_only_candidate() {
        let fetched = FetchedRelease {
            release: Release {
                tag_name: "v9.9.9".to_string(),
                prerelease: false,
                assets: vec![Asset {
                    name: "codewhale-macos-arm64".to_string(),
                    browser_download_url: "https://cdn.example/codewhale-macos-arm64".to_string(),
                }],
            },
            source: UpdateReleaseSource::GitHub,
        };

        let candidates = candidates_for(&fetched, "linux", "x86_64").expect("linux x64 fallback");

        assert_eq!(candidates.len(), 1);
        assert!(matches!(
            candidates[0].source,
            UpdateReleaseSource::Cnb { .. }
        ));
    }

    #[test]
    fn beta_tags_use_the_same_ordered_sources_as_stable_tags() {
        let candidates = linux_x64_candidates("v0.9.0-beta.2");

        assert_eq!(candidates[0].source, UpdateReleaseSource::GitHub);
        assert_eq!(
            candidates[1].source,
            UpdateReleaseSource::Cnb {
                base_url: cnb_release_base_url("v0.9.0-beta.2"),
            },
            "the beta tag must be carried into the CNB URL verbatim"
        );
    }

    #[test]
    fn explicit_overrides_take_precedence_over_probing() {
        {
            let _env = UpdateEnvGuard::clear();
            set_update_env(
                codewhale_release::RELEASE_BASE_URL_ENV,
                "https://mirror.example/assets",
            );
            set_update_env(codewhale_release::UPDATE_VERSION_ENV, "9.9.9");

            let fetched = fetch_latest_release(ReleaseChannel::Stable, None)
                .expect("a pinned mirror resolves without a network");

            assert_eq!(
                fetched.source,
                UpdateReleaseSource::Mirror {
                    base_url: "https://mirror.example/assets".to_string(),
                }
            );
            assert!(fetched.source.is_pinned_mirror());
            assert!(
                candidates_for(&fetched, "linux", "x86_64").is_none(),
                "an explicit base URL must never fall back to CNB"
            );
            assert_eq!(
                describe_release_source_for_check(&fetched, "codewhale-linux-x64", None),
                "release mirror (https://mirror.example/assets)"
            );
        }

        {
            let _env = UpdateEnvGuard::clear();
            set_update_env(codewhale_release::CNB_MIRROR_ENV, "1");
            set_update_env(codewhale_release::UPDATE_VERSION_ENV, "9.9.9");

            let fetched = fetch_latest_release(ReleaseChannel::Stable, None)
                .expect("the CNB override resolves without a network");

            assert_eq!(
                fetched.source,
                UpdateReleaseSource::Cnb {
                    base_url: cnb_release_base_url("9.9.9"),
                },
                "an explicit CNB request must be reported as CNB, not as a generic mirror"
            );
            assert!(
                candidates_for(&fetched, "linux", "x86_64").is_none(),
                "an explicit CNB request must not fall back to GitHub"
            );
        }

        {
            let _env = UpdateEnvGuard::clear();
            set_update_env(codewhale_release::CNB_MIRROR_ENV, "1");
            set_update_env(
                codewhale_release::RELEASE_BASE_URL_ENV,
                "https://mirror.example/assets",
            );

            let fetched = fetch_latest_release(ReleaseChannel::Stable, None)
                .expect("a pinned mirror resolves without a network");

            assert_eq!(
                fetched.source,
                UpdateReleaseSource::Mirror {
                    base_url: "https://mirror.example/assets".to_string(),
                },
                "an explicit base URL outranks the CNB flag"
            );
        }
    }

    #[test]
    fn a_locked_source_serves_both_the_manifest_and_the_binary() {
        const BINARY: &[u8] = b"\x7fELF codewhale linux x64 payload";
        let manifest = format!("{}  codewhale-linux-x64\n", sha256_hex(BINARY));
        let (url, request_rx, handle) = serve_http_owned_responses(vec![
            ("200 OK", "text/plain", manifest.into_bytes()),
            ("200 OK", "application/octet-stream", BINARY.to_vec()),
        ]);
        let origin = url.trim_end_matches("/release").to_string();
        let candidate = ReleaseSourceCandidate {
            source: UpdateReleaseSource::Cnb {
                base_url: origin.clone(),
            },
            manifest_url: mirror_asset_url(&origin, CHECKSUM_MANIFEST_ASSET),
            binary_name: "codewhale-linux-x64".to_string(),
            binary_url: mirror_asset_url(&origin, "codewhale-linux-x64"),
        };

        let plan = select_release_source(vec![candidate], manifest_probe_fetcher(None))
            .expect("the only reachable source must win");
        let bytes = download_url(&plan.binary_url, None).expect("binary download");
        verify_downloaded_asset(&plan, &bytes)
            .expect("bytes from the locked source must match its own manifest");

        assert_eq!(bytes, BINARY);
        let manifest_request = request_rx.recv().expect("manifest request");
        let binary_request = request_rx.recv().expect("binary request");
        assert!(
            manifest_request.starts_with("GET /codewhale-artifacts-sha256.txt "),
            "got {manifest_request:?}"
        );
        assert!(
            binary_request.starts_with("GET /codewhale-linux-x64 "),
            "got {binary_request:?}"
        );
        handle.join().expect("test server thread");
    }

    #[test]
    fn a_checksum_mismatch_fails_closed_and_names_the_source() {
        let mut plan = DownloadPlan {
            source: UpdateReleaseSource::Cnb {
                base_url: cnb_release_base_url("v9.9.9"),
            },
            binary_name: "codewhale-linux-x64".to_string(),
            binary_url: "https://cnb.example/codewhale-linux-x64".to_string(),
            checksums: HashMap::from([("codewhale-linux-x64".to_string(), "a".repeat(64))]),
            manifest_url: String::new(),
            manifest_bytes: Vec::new(),
            fallback_binaries: Vec::new(),
        };

        let err = verify_downloaded_asset(&plan, b"tampered bytes")
            .expect_err("a mismatch must never install");
        let message = format!("{err:#}");
        assert!(message.contains("SHA256 mismatch"), "{message}");
        assert!(message.contains("CNB mirror"), "{message}");

        plan.checksums = HashMap::from([("codew-linux-x64".to_string(), "a".repeat(64))]);
        let err = verify_downloaded_asset(&plan, b"bytes")
            .expect_err("an uncovered asset must never install");
        let message = format!("{err:#}");
        assert!(
            message.contains("is missing codewhale-linux-x64"),
            "{message}"
        );
    }

    #[test]
    fn release_sources_describe_themselves_for_status_and_receipts() {
        assert_eq!(UpdateReleaseSource::GitHub.describe(), "GitHub Releases");
        assert!(!UpdateReleaseSource::GitHub.is_pinned_mirror());

        let cnb = UpdateReleaseSource::Cnb {
            base_url: cnb_release_base_url("v9.9.9"),
        };
        assert_eq!(
            cnb.describe(),
            "CNB mirror (https://cnb.cool/codewhale.net/codewhale/-/releases/download/v9.9.9)"
        );
        assert!(cnb.is_pinned_mirror());

        let mirror = UpdateReleaseSource::Mirror {
            base_url: "https://mirror.example/assets".to_string(),
        };
        assert_eq!(
            mirror.describe(),
            "release mirror (https://mirror.example/assets)"
        );
        assert!(mirror.is_pinned_mirror());
    }

    #[test]
    fn validate_and_build_proxy_accepts_supported_proxy_urls() {
        validate_and_build_proxy("http://localhost:7897").expect("http proxy");
        validate_and_build_proxy("https://proxy.example.com:8080").expect("https proxy");
        validate_and_build_proxy("socks5://127.0.0.1:1080").expect("socks proxy");
    }

    #[test]
    fn validate_and_build_proxy_rejects_malformed_urls() {
        let err = validate_and_build_proxy("not a valid url").expect_err("malformed URL");
        assert!(err.to_string().contains("invalid proxy URL"));
    }

    #[test]
    fn fetch_latest_release_from_url_reads_mocked_release_json() {
        let body = br#"{
          "tag_name": "v9.9.9",
          "assets": [
            { "name": "codewhale-linux-x64", "browser_download_url": "http://example.invalid/codewhale-linux-x64" },
            { "name": "codewhale-artifacts-sha256.txt", "browser_download_url": "http://example.invalid/codewhale-artifacts-sha256.txt" }
          ]
        }"#;
        let (url, request_rx, handle) = serve_http_once("200 OK", "application/json", body);
        let release = fetch_latest_release_from_url(&url, None).expect("release JSON should parse");

        assert_eq!(release.tag_name, "v9.9.9");
        assert_eq!(release.assets.len(), 2);

        let request = request_rx.recv().expect("captured request");
        let request_lower = request.to_ascii_lowercase();
        assert!(request.starts_with("GET /release "), "got {request:?}");
        assert!(
            request_lower.contains("accept: application/vnd.github+json"),
            "got {request:?}"
        );
        assert!(
            request_lower.contains("user-agent: codewhale-updater"),
            "got {request:?}"
        );
        handle.join().expect("test server thread");
    }

    #[test]
    fn fetch_latest_release_from_url_retries_transient_gateway_error() {
        let body = br#"{
          "tag_name": "v9.9.9",
          "assets": [
            { "name": "codewhale-linux-x64", "browser_download_url": "http://example.invalid/codewhale-linux-x64" }
          ]
        }"#;
        let (url, request_rx, handle) = serve_http_responses(vec![
            ("504 Gateway Timeout", "text/plain", b"gateway timeout"),
            ("200 OK", "application/json", body),
        ]);
        let release = fetch_latest_release_from_url(&url, None)
            .expect("release JSON should parse after retry");

        assert_eq!(release.tag_name, "v9.9.9");
        let first = request_rx.recv().expect("first request");
        let second = request_rx.recv().expect("second request");
        assert!(first.starts_with("GET /release "), "got {first:?}");
        assert!(second.starts_with("GET /release "), "got {second:?}");
        handle.join().expect("test server thread");
    }

    #[test]
    fn fetch_latest_release_from_url_reports_http_errors() {
        let (url, _request_rx, handle) = serve_http_responses(vec![
            ("500 Internal Server Error", "text/plain", b"server broke"),
            ("500 Internal Server Error", "text/plain", b"server broke"),
            ("500 Internal Server Error", "text/plain", b"server broke"),
        ]);
        let err = fetch_latest_release_from_url(&url, None).expect_err("HTTP 500 should fail");

        assert!(
            err.to_string().contains("HTTP 500"),
            "unexpected error: {err:#}"
        );
        handle.join().expect("test server thread");
    }

    #[test]
    fn fetch_latest_beta_release_from_url_selects_first_beta_release() {
        let body = br#"[
          { "tag_name": "v0.9.0", "prerelease": false, "assets": [] },
          { "tag_name": "v0.9.0-rc.1", "prerelease": true, "assets": [] },
          { "tag_name": "v0.9.0-beta.2", "prerelease": true, "assets": [
            { "name": "codewhale-linux-x64", "browser_download_url": "http://example.invalid/codewhale-linux-x64" }
          ] },
          { "tag_name": "v0.9.0-beta.1", "prerelease": true, "assets": [] }
        ]"#;
        let (url, request_rx, handle) = serve_http_once("200 OK", "application/json", body);
        let release =
            fetch_latest_beta_release_from_url(&url, None).expect("beta release JSON should parse");

        assert_eq!(release.tag_name, "v0.9.0-beta.2");
        assert!(release.prerelease);

        let request = request_rx.recv().expect("captured request");
        let request_lower = request.to_ascii_lowercase();
        assert!(request.starts_with("GET /release "), "got {request:?}");
        assert!(
            request_lower.contains("accept: application/vnd.github+json"),
            "got {request:?}"
        );
        handle.join().expect("test server thread");
    }

    #[test]
    fn fetch_latest_beta_release_from_url_reports_missing_beta() {
        let body = br#"[
          { "tag_name": "v0.9.0", "prerelease": false, "assets": [] }
        ]"#;
        let (url, _request_rx, handle) = serve_http_once("200 OK", "application/json", body);
        let err =
            fetch_latest_beta_release_from_url(&url, None).expect_err("missing beta should fail");

        assert!(
            err.to_string().contains("no beta release found"),
            "unexpected error: {err:#}"
        );
        handle.join().expect("test server thread");
    }

    #[test]
    fn download_url_retries_transient_gateway_error() {
        let (url, request_rx, handle) = serve_http_responses(vec![
            ("503 Service Unavailable", "text/plain", b"try again"),
            ("200 OK", "application/octet-stream", b"\0binary bytes"),
        ]);
        let bytes = download_url(&url, None).expect("binary download should retry and succeed");

        assert_eq!(bytes, b"\0binary bytes");
        let first = request_rx.recv().expect("first request");
        let second = request_rx.recv().expect("second request");
        assert!(first.starts_with("GET /release "), "got {first:?}");
        assert!(second.starts_with("GET /release "), "got {second:?}");
        handle.join().expect("test server thread");
    }

    #[test]
    fn download_url_reads_binary_body_with_updater_user_agent() {
        let (url, request_rx, handle) =
            serve_http_once("200 OK", "application/octet-stream", b"\0binary bytes");
        let bytes = download_url(&url, None).expect("binary download should succeed");

        assert_eq!(bytes, b"\0binary bytes");

        let request = request_rx.recv().expect("captured request");
        let request_lower = request.to_ascii_lowercase();
        assert!(request.starts_with("GET /release "), "got {request:?}");
        assert!(
            request_lower.contains("user-agent: codewhale-updater"),
            "got {request:?}"
        );
        handle.join().expect("test server thread");
    }

    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).expect("url")
    }

    #[test]
    fn update_hosts_are_an_allow_list_over_https() {
        let strict = UpdateTransportPolicy::strict();
        for allowed in [
            "https://github.com/codewhale-hq/CodeWhale/releases/latest",
            "https://api.github.com/repos/codewhale-hq/CodeWhale/releases/latest",
            "https://release-assets.githubusercontent.com/x",
            "https://objects.githubusercontent.com/x",
            "https://cnb.cool/codewhale.net/codewhale/-/releases/download/v1/a",
        ] {
            strict.check_url(&url(allowed)).expect(allowed);
        }
        for refused in [
            "http://github.com/codewhale-hq/CodeWhale",
            "https://github.com.evil.example/a",
            "https://evilgithub.com/a",
            "https://notcnb.cool/a",
            "https://raw.githubusercontent.com/a",
            "https://203.0.113.9/a",
            "ftp://github.com/a",
            "http://127.0.0.1:9/a",
        ] {
            assert!(strict.check_url(&url(refused)).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_configured_mirror_host_is_allowed_and_must_be_https() {
        let _guard = UpdateEnvGuard::clear();
        set_update_env(
            codewhale_release::RELEASE_BASE_URL_ENV,
            "https://Mirror.Internal.example:8443/CodeWhale/",
        );
        let policy = UpdateTransportPolicy::from_env();
        policy
            .check_url(&url("https://mirror.internal.example:8443/CodeWhale/v1/a"))
            .expect("the configured mirror host is allowed");
        assert!(
            policy
                .check_url(&url("https://other.internal.example/a"))
                .is_err()
        );
        assert!(
            policy
                .check_url(&url("http://mirror.internal.example/a"))
                .is_err(),
            "a mirror is never reached over plain HTTP"
        );
    }

    #[test]
    fn extra_update_hosts_come_only_from_the_operator_environment() {
        let _guard = UpdateEnvGuard::clear();
        set_update_env(
            UPDATE_ALLOWED_HOSTS_ENV,
            "cdn.one.example, CDN.two.example ,",
        );
        let policy = UpdateTransportPolicy::from_env();
        for host in ["cdn.one.example", "cdn.two.example"] {
            policy
                .check_url(&url(&format!("https://{host}/a")))
                .expect(host);
        }
        assert!(
            policy
                .check_url(&url("https://cdn.three.example/a"))
                .is_err()
        );
        assert!(policy.check_url(&url("http://cdn.one.example/a")).is_err());
    }

    #[test]
    fn a_plain_http_mirror_is_refused_before_any_request() {
        let _guard = UpdateEnvGuard::clear();
        set_update_env(
            codewhale_release::RELEASE_BASE_URL_ENV,
            "http://mirror.example/assets",
        );
        let error = fetch_latest_release(ReleaseChannel::Stable, None)
            .expect_err("an HTTP mirror must be refused");
        assert!(format!("{error:#}").contains("HTTPS"), "{error:#}");
    }

    /// A listener that records whether anything connected to it.
    fn silent_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        (listener, format!("http://{addr}/asset"))
    }

    #[test]
    fn the_request_path_refuses_plain_http_and_unlisted_hosts_without_connecting() {
        let (listener, plain_url) = silent_listener();
        let strict = UpdateTransportPolicy::strict();
        let error = download_url_once_with(
            &strict,
            UPDATE_MAX_RESPONSE_BYTES,
            &plain_url,
            None,
            Duration::from_secs(5),
        )
        .expect_err("plain HTTP is refused");
        assert!(format!("{error:#}").contains("HTTPS"), "{error:#}");
        assert!(
            listener.accept().is_err(),
            "no connection may be opened for a refused URL"
        );
        let error = download_url_once_with(
            &strict,
            UPDATE_MAX_RESPONSE_BYTES,
            "https://download.evil.example/codewhale",
            None,
            Duration::from_secs(5),
        )
        .expect_err("an unlisted host is refused");
        assert!(
            format!("{error:#}").contains("not an allowed release host"),
            "{error:#}"
        );
    }

    /// The redirect policy is exercised through the real built client against
    /// a live fixture. The fixture cannot speak TLS, so the request starts on
    /// plain HTTP (a start URL is the caller's check, covered above); the point
    /// is that the client then refuses to follow a hop that is not HTTPS to an
    /// allowed host, and never contacts it.
    #[test]
    fn the_built_client_refuses_to_follow_a_redirect_off_https_or_off_the_allow_list() {
        let (target, target_listener) = {
            let (listener, url) = silent_listener();
            (url, listener)
        };
        for location in [target.as_str(), "https://download.evil.example/next"] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let location = location.to_string();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf).expect("read");
                write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .expect("write");
            });
            let client = update_http_client_with_policy(
                None,
                Duration::from_secs(5),
                &UpdateTransportPolicy::strict(),
            )
            .expect("client");
            let error = client
                .get(format!("http://{addr}/start"))
                .send()
                .expect_err("the redirect must not be followed");
            // reqwest keeps the policy's message in the error's source chain.
            let mut text = error.to_string();
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                text.push_str(&format!(": {cause}"));
                source = cause.source();
            }
            assert!(text.contains("update redirect refused"), "{text}");
            server.join().expect("fixture thread");
        }
        assert!(
            target_listener.accept().is_err(),
            "the refused redirect target must never be contacted"
        );
    }

    #[test]
    fn a_response_past_the_size_cap_is_refused_and_one_at_the_cap_is_kept() {
        let mut policy = UpdateTransportPolicy::strict();
        policy.allow_loopback_http = true;
        let (url, _rx, handle) = serve_http_once("200 OK", "application/octet-stream", &[7_u8; 64]);
        let error = download_url_once_with(&policy, 63, &url, None, Duration::from_secs(5))
            .expect_err("one byte over the cap is refused");
        assert!(format!("{error:#}").contains("update limit"), "{error:#}");
        handle.join().expect("fixture thread");

        let (url, _rx, handle) = serve_http_once("200 OK", "application/octet-stream", &[7_u8; 64]);
        let (_, bytes) = download_url_once_with(&policy, 64, &url, None, Duration::from_secs(5))
            .expect("a body exactly at the cap is kept");
        assert_eq!(bytes.len(), 64);
        handle.join().expect("fixture thread");
    }

    #[test]
    fn release_tags_taken_from_a_page_must_look_like_tags() {
        let page = |tag: &str| {
            url(&format!(
                "https://github.com/codewhale-hq/CodeWhale/releases/tag/{tag}"
            ))
        };
        assert_eq!(
            release_tag_from_github_release_url(&page("v0.10.1")).as_deref(),
            Some("v0.10.1")
        );
        assert_eq!(release_tag_from_github_release_url(&page("%2e%2e")), None);
        assert_eq!(
            release_tag_from_github_release_html("<a href=\"/releases/tag/v1.2.3\">"),
            Some("v1.2.3".to_string())
        );
        assert_eq!(
            release_tag_from_github_release_html("<a href=\"/releases/tag/a%2Fb\">"),
            None
        );
    }
}
