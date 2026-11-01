use super::*;
use codewhale_runtime::delivery_files::GuardedFile;
use std::collections::BTreeMap;

const RECEIPT: &str = codewhale_release::installs::RECEIPT_FILE;
const MAX_RECEIPT: usize = 512;
pub(super) const COMMANDS: &[&str] = codewhale_release::installs::RECEIPT_COMMANDS;

pub(super) struct CommandReceipt {
    directory: PathBuf,
    receipt: GuardedFile,
    hashes: BTreeMap<String, String>,
}

impl CommandReceipt {
    pub(super) fn capture(executable: &Path) -> Result<Option<Self>> {
        Self::capture_accepting(executable, None)
    }

    pub(super) fn capture_accepting(
        executable: &Path,
        updated: Option<&str>,
    ) -> Result<Option<Self>> {
        let directory = executable.parent().context("executable has no parent")?;
        #[cfg(not(windows))]
        let directory = directory.canonicalize()?;
        #[cfg(windows)]
        let directory = directory.to_path_buf();
        let Some(mut receipt) = GuardedFile::open_bounded(&directory, RECEIPT, MAX_RECEIPT)? else {
            return Ok(None);
        };
        receipt.verify_owned()?;
        let bytes = receipt.read_bounded(MAX_RECEIPT)?;
        let text = std::str::from_utf8(&bytes).context("command receipt is not UTF-8")?;
        let hashes = parse_checksum_manifest(text)?;
        if hashes.is_empty()
            || hashes.len() > COMMANDS.len()
            || text.lines().count() != hashes.len()
            || hashes.keys().any(|name| !COMMANDS.contains(&name.as_str()))
        {
            bail!(
                "Invalid command ownership receipt {}; no files were changed",
                directory.join(RECEIPT).display()
            );
        }
        for (name, digest) in &hashes {
            if let Some(mut file) = GuardedFile::open(&directory, name)? {
                file.verify_owned()?;
                if file.sha256() != digest.as_str() && Some(file.sha256()) != updated {
                    bail!(
                        "Preserved modified command {}; receipt no longer matches",
                        directory.join(name).display()
                    );
                }
            }
        }
        Ok(Some(Self {
            directory,
            receipt,
            hashes: hashes.into_iter().collect(),
        }))
    }

    pub(super) fn renew(&mut self, verified: &[u8]) -> Result<()> {
        self.receipt.verify_owned()?;
        let new_hash = sha256_hex(verified);
        let mut text = String::new();
        for (name, old_hash) in &self.hashes {
            if let Some(mut file) = GuardedFile::open(&self.directory, name)? {
                if file.sha256() != old_hash.as_str() && file.sha256() != new_hash.as_str() {
                    bail!(
                        "Cannot renew ownership of modified command {}",
                        self.directory.join(name).display()
                    );
                }
                file.verify_owned()?;
                text.push_str(&format!("{}  {name}\n", file.sha256()));
            }
        }
        if text.is_empty() {
            bail!("No receipt-owned commands remain; ownership receipt was preserved");
        }
        if self.receipt.read_bounded(MAX_RECEIPT)? == text.as_bytes() {
            return Ok(());
        }
        self.receipt.verify_owned()?;
        let mut staged = GuardedFile::stage(&self.directory, text.as_bytes(), false)?;
        staged.verify_owned()?;
        let backup = GuardedFile::recovery_name();
        self.receipt.move_to_vacant(&backup).with_context(|| {
            format!(
                "receipt retirement failed; recovery at {}",
                self.directory.join(self.receipt.name()).display()
            )
        })?;
        if let Err(error) = staged.move_to_vacant(RECEIPT) {
            let restored = self.receipt.move_to_vacant(RECEIPT);
            bail!(
                "receipt publication failed: {error}; restoration: {restored:?}; previous receipt {}, staged receipt {}",
                self.directory.join(self.receipt.name()).display(),
                self.directory.join(staged.name()).display()
            );
        }
        let retired = self.directory.join(&backup);
        match codewhale_config::codewhale_home()
            .and_then(|home| installs::RecoveryBatch::open(&home))
            .and_then(|mut batch| {
                let moved = batch.move_in(&retired)?;
                batch.finish();
                Ok(moved)
            }) {
            Ok(moved) => println!(
                "Renewed command ownership receipt; previous receipt kept at {}",
                moved.to.display()
            ),
            Err(_) => println!(
                "Renewed command ownership receipt; previous receipt retained at {}",
                retired.display()
            ),
        }
        self.receipt = staged;
        Ok(())
    }
}

pub(crate) fn run(yes: bool, purge_data: bool) -> Result<()> {
    let selected = std::env::current_exe().ok();
    if purge_data {
        purge_targets(selected.as_deref())?;
    }
    let result = remove(yes);
    report_path_copies(selected.as_deref());
    let removed = result?;
    if purge_data {
        if !yes {
            return purge(false, selected.as_deref());
        }
        if removed {
            return purge(true, selected.as_deref());
        }
        println!(
            "Data was not purged: this run did not remove a receipted installation. Data is shared by every install and is kept."
        );
        Ok(())
    } else {
        println!(
            "User data was not touched. To also delete Codewhale's own data directory, add --purge-data."
        );
        Ok(())
    }
}

fn remove(yes: bool) -> Result<bool> {
    let identity = update_executable_identity()?;
    #[cfg(not(windows))]
    let identity = UpdateExecutableIdentity {
        path: identity
            .path
            .canonicalize()
            .context("failed to resolve the selected executable path")?,
        ..identity
    };
    let method = InstallMethod::detect(&identity.path);
    println!("Selected executable: {}", identity.path.display());
    println!(
        "Program removal preserves user state, credentials, sessions, workspaces and services. For a receipted release copy it also removes this install's exact Codewhale PATH block from your shell profiles and Codewhale-generated completion files at the documented paths; nothing else in a profile is touched."
    );
    if !method.supports_self_update() {
        println!(
            "This copy belongs to {}. Remove it with: {}",
            method.label(),
            codewhale_release::installs::manager_command(method, &identity.path)
        );
        return Ok(false);
    }
    if let Some(uninstaller) = windows_installer_uninstaller(&identity.path) {
        println!(
            "This copy belongs to the Codewhale Windows installer. Remove it from Settings > Apps > CodeWhale, or run: \"{}\"",
            uninstaller.display()
        );
        return Ok(false);
    }
    if protected_update_path(&identity.path) {
        bail!(
            "System/package path was preserved. Identify its owning package manager and remove it through that manager."
        );
    }
    let primary_name = identity
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .context("executable basename is not UTF-8")?
        .to_string();
    if !COMMANDS.contains(&primary_name.as_str()) {
        bail!(
            "This command name has no installer ownership contract; preserved {}",
            identity.path.display()
        );
    }
    let ownership = match CommandReceipt::capture(&identity.path)? {
        Some(ownership) => ownership,
        None => {
            let (directory, entries) = curl_install_entries(&identity)?.with_context(|| format!(
                "No command ownership receipt beside {}. This legacy/manual copy is preserved. Inspect its exact path and provenance before removing it manually; reinstalling into a fresh directory creates a receipted install.", identity.path.display()))?;
            println!(
                "No ownership receipt yet: {} is a curl-installer location and these commands are byte-identical to this Codewhale {}.",
                directory.display(),
                env!("CARGO_PKG_VERSION")
            );
            if !yes {
                for (name, _) in &entries {
                    println!("Would record and remove {}", directory.join(name).display());
                }
                println!(
                    "No files were changed. Run this executable again with uninstall --yes to record a receipt for exactly these files and remove them."
                );
                return Ok(false);
            }
            let text: String = entries
                .iter()
                .map(|(name, digest)| format!("{digest}  {name}\n"))
                .collect();
            let mut staged = GuardedFile::stage(&directory, text.as_bytes(), false)?;
            staged.verify_owned()?;
            staged.move_to_vacant(RECEIPT).with_context(|| {
                format!(
                    "could not record a receipt in {}; staged bytes retained at {}",
                    directory.display(),
                    directory.join(staged.name()).display()
                )
            })?;
            println!("Recorded an install receipt in {}.", directory.display());
            CommandReceipt::capture(&identity.path)?
                .context("the adopted ownership receipt disappeared; nothing was removed")?
        }
    };
    if ownership.hashes.get(&primary_name) != Some(&identity.file_hash) {
        bail!("The receipt does not own the selected executable; nothing was removed");
    }
    let directory = ownership.directory.clone();
    let mut files = Vec::new();
    let mut primary = None;
    for name in ownership.hashes.keys() {
        if let Some(file) = GuardedFile::open(&directory, name)? {
            if ownership.hashes.get(name).map(String::as_str) != Some(file.sha256()) {
                bail!(
                    "Command changed during uninstall preflight; preserved {}",
                    directory.join(name).display()
                );
            }
            if name == &primary_name {
                primary = Some(file);
            } else {
                files.push(file);
            }
        }
    }
    let target =
        release_asset_stem_for_prefix("codewhale", std::env::consts::OS, std::env::consts::ARCH);
    let companions =
        compiled_host::owned_for_uninstall(&directory, target.trim_start_matches("codewhale-"))?;
    if companions.is_empty() {
        println!("Any unreceipted compiled-host files are preserved.");
    }
    files.extend(companions);
    let mut receipt = ownership.receipt;
    let receipt_bytes = receipt.read_bounded(MAX_RECEIPT)?;
    files.push(receipt);
    files.push(primary.context("selected executable disappeared during preflight")?);
    for file in &mut files {
        file.verify_owned()?;
        println!(
            "{} {}",
            if yes { "Remove" } else { "Would remove" },
            directory.join(file.name()).display()
        );
    }
    let shell = ShellIntegration::plan(&directory, &identity.path, yes);
    validate_primary_update_identity(&identity)?;
    if !yes {
        println!(
            "No files were changed. Run this executable again with uninstall --yes to remove only this plan."
        );
        return Ok(false);
    }
    #[cfg(unix)]
    {
        let preflight = tempfile::Builder::new()
            .prefix(".codewhale-uninstall-preflight-")
            .tempdir_in(&directory)?
            .keep();
        std::fs::remove_dir(&preflight).with_context(|| {
            format!(
                "uninstall directory preflight failed at {}",
                preflight.display()
            )
        })?;
    }
    let mut removed = Vec::new();
    for file in &mut files {
        let name = file.name().to_string();
        let mut unlinked = false;
        let outcome = validate_primary_update_identity(&identity)
            .and_then(|()| file.remove_checked(&mut unlinked).map_err(Into::into));
        #[cfg(windows)]
        let outcome = match outcome {
            Err(error) if unlinked && name == primary_name => {
                let retired = directory.join(file.name());
                delete_after_exit(&retired)
                    .map(|()| {
                        println!(
                            "The running executable was retired to {}; it is deleted after this process exits. Delete it by hand if it remains.",
                            retired.display()
                        )
                    })
                    .map_err(|schedule| {
                        error.context(format!(
                            "deferred removal of {} could not be scheduled: {schedule:#}",
                            retired.display()
                        ))
                    })
            }
            other => other,
        };
        if unlinked {
            removed.push(name.clone());
        }
        if let Err(error) = outcome {
            if removed.iter().any(|name| name == RECEIPT)
                && !removed.iter().any(|name| name == &primary_name)
            {
                let restore = (|| {
                    let mut staged = GuardedFile::stage(&directory, &receipt_bytes, false)?;
                    staged.verify_owned()?;
                    staged.move_to_vacant(RECEIPT).with_context(|| {
                        format!(
                            "receipt recovery bytes retained at {}",
                            directory.join(staged.name()).display()
                        )
                    })
                })();
                if let Err(restore) = restore {
                    eprintln!("Receipt recovery needs attention: {restore:#}");
                }
            }
            bail!(
                "Uninstall stopped at {name}: {error:#}. Already unlinked: {}. Any retained recovery locations are reported above; no unknown destination is overwritten.",
                removed.join(", ")
            );
        }
    }
    #[cfg(windows)]
    for entry in std::fs::read_dir(&directory)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("codewhale.old-") || name.starts_with("codew.old-") {
            println!(
                "Preserved a copy left by an earlier self-update; delete it by hand: {}",
                entry.path().display()
            );
        }
    }
    let shell_failures = shell.apply();
    println!(
        "Removed this receipted release installation. Other installations were not removed. {}",
        if cfg!(windows) {
            "Remove its directory from your user PATH if you added it, then open a new terminal."
        } else {
            "Run hash -r in your shell."
        }
    );
    if !shell_failures.is_empty() {
        bail!(
            "The program files were removed, but shell integration cleanup is incomplete: {}",
            shell_failures.join("; ")
        );
    }
    Ok(true)
}

const PATH_BLOCK_START: &str = "# >>> codewhale PATH >>>";
const PATH_BLOCK_END: &str = "# <<< codewhale PATH <<<";
const MAX_PROFILE: usize = 1024 * 1024;
const MAX_COMPLETION: usize = 1024 * 1024;

struct ProfileEdit {
    profile: PathBuf,
    file: GuardedFile,
    updated: Vec<u8>,
    #[cfg(unix)]
    mode: u32,
}

#[derive(Default)]
struct ShellIntegration {
    edits: Vec<ProfileEdit>,
    completions: Vec<(PathBuf, GuardedFile)>,
}

impl ShellIntegration {
    fn plan(directory: &Path, selected: &Path, yes: bool) -> Self {
        let Some(home) = dirs::home_dir() else {
            return Self::default();
        };
        let verb = if yes { "Remove" } else { "Would remove" };
        Self {
            edits: plan_path_blocks(&home, directory, verb),
            completions: plan_completions(&home, directory, selected, verb),
        }
    }

    fn apply(self) -> Vec<String> {
        let mut failures = Vec::new();
        for edit in self.edits {
            let profile = edit.profile.clone();
            match apply_profile_edit(edit) {
                Ok(()) => println!(
                    "Removed the Codewhale PATH block from {}",
                    profile.display()
                ),
                Err(error) => failures.push(format!(
                    "{} was not changed ({error:#}); delete its Codewhale PATH block by hand",
                    profile.display()
                )),
            }
        }
        for (path, mut file) in self.completions {
            let mut unlinked = false;
            match file.remove_checked(&mut unlinked) {
                Ok(()) => println!("Removed completion file {}", path.display()),
                Err(error) => failures.push(format!(
                    "{} was not removed ({error}); delete it by hand",
                    path.display()
                )),
            }
        }
        failures
    }
}

fn plan_path_blocks(home: &Path, directory: &Path, verb: &str) -> Vec<ProfileEdit> {
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    let mut profiles: Vec<PathBuf> = [
        ".zshrc",
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".profile",
    ]
    .iter()
    .map(|name| home.join(name))
    .chain(zdotdir.map(|dir| dir.join(".zshrc")))
    .chain(std::iter::once(
        home.join(".config")
            .join("fish")
            .join("conf.d")
            .join("codewhale.fish"),
    ))
    .collect();
    profiles.sort();
    profiles.dedup();
    let install_directory = directory.canonicalize().ok();
    let names_directory = |value: &str| {
        let expanded = match value.strip_prefix("$HOME/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(value),
        };
        expanded
            .canonicalize()
            .is_ok_and(|path| install_directory.as_ref() == Some(&path))
    };
    let ours = |line: &str| {
        line.strip_prefix("export PATH=\"")
            .and_then(|rest| rest.strip_suffix(":$PATH\""))
            .is_some_and(names_directory)
            || line
                .strip_prefix("contains -- \"")
                .and_then(|rest| rest.split_once("\" $PATH; or set -gx PATH \""))
                .is_some_and(|(value, rest)| {
                    rest == format!("{value}\" $PATH") && names_directory(value)
                })
    };
    let mut edits = Vec::new();
    for profile in profiles {
        let (Some(parent), Some(name)) = (
            profile.parent(),
            profile.file_name().and_then(|name| name.to_str()),
        ) else {
            continue;
        };
        let mut file = match GuardedFile::open_bounded(parent, name, MAX_PROFILE) {
            Ok(Some(file)) => file,
            Ok(None) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                println!(
                    "Not read: {} ({error}). If it holds a Codewhale PATH block, remove that block by hand.",
                    profile.display()
                );
                continue;
            }
        };
        let bytes = match file.read_bounded(MAX_PROFILE) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!(
                    "Not read: {} ({error}). If it holds a Codewhale PATH block, remove that block by hand.",
                    profile.display()
                );
                continue;
            }
        };
        let Ok(text) = String::from_utf8(bytes) else {
            println!(
                "Not read: {} is not a UTF-8 file within the size limit.",
                profile.display()
            );
            continue;
        };
        let segments: Vec<&str> = text.split_inclusive('\n').collect();
        let lines: Vec<&str> = segments.iter().map(|line| line.trim_end()).collect();
        let mut removed = vec![false; lines.len()];
        let mut index = 0;
        while index < lines.len() {
            if lines[index] == PATH_BLOCK_START {
                if lines.get(index + 2) == Some(&PATH_BLOCK_END) && ours(lines[index + 1]) {
                    println!(
                        "{verb} the Codewhale PATH block for this install: {} lines {}-{}",
                        profile.display(),
                        index + 1,
                        index + 3
                    );
                    removed[index..index + 3].fill(true);
                    index += 3;
                    continue;
                }
                println!(
                    "Codewhale PATH marker at {} line {} does not form a block for this install; left unchanged.",
                    profile.display(),
                    index + 1
                );
            } else if lines[index] == PATH_BLOCK_END {
                println!(
                    "Codewhale PATH end marker at {} line {} has no start marker; left unchanged.",
                    profile.display(),
                    index + 1
                );
            }
            index += 1;
        }
        if !removed.contains(&true) {
            continue;
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::metadata(&profile) {
                Ok(metadata) => metadata.permissions().mode() & 0o7777,
                Err(error) => {
                    println!(
                        "Not changed: {} ({error}); delete its Codewhale PATH block by hand.",
                        profile.display()
                    );
                    continue;
                }
            }
        };
        let updated = segments
            .iter()
            .zip(&removed)
            .filter(|(_, removed)| !**removed)
            .flat_map(|(segment, _)| segment.bytes())
            .collect();
        edits.push(ProfileEdit {
            profile,
            file,
            updated,
            #[cfg(unix)]
            mode,
        });
    }
    edits
}

fn apply_profile_edit(edit: ProfileEdit) -> Result<()> {
    let ProfileEdit {
        profile,
        mut file,
        updated,
        #[cfg(unix)]
        mode,
    } = edit;
    let parent = profile
        .parent()
        .context("profile has no parent directory")?;
    let name = profile
        .file_name()
        .and_then(|name| name.to_str())
        .context("profile name is not UTF-8")?
        .to_string();
    #[cfg(unix)]
    let mut staged = stage_profile(parent, &updated, mode)?;
    #[cfg(not(unix))]
    let mut staged = stage_profile(parent, &updated)?;
    // Rename the profile aside instead of unlinking it: a failure while the
    // replacement is installed can then put the user's own bytes back by name.
    let retired = GuardedFile::recovery_name();
    file.move_to_vacant(&retired).with_context(|| {
        format!(
            "{} could not be set aside for its replacement",
            profile.display()
        )
    })?;
    if let Err(error) = staged.move_to_vacant(&name) {
        let restore = file.move_to_vacant(&name).map_err(|restore| {
            format!(
                "the unchanged profile is retained at {} and could not be restored ({restore})",
                parent.join(&retired).display()
            )
        });
        let mut discarded = false;
        let _ = staged.remove_checked(&mut discarded);
        return match restore {
            Ok(()) => Err(error).with_context(|| {
                format!(
                    "{} was left unchanged because its replacement could not be installed",
                    profile.display()
                )
            }),
            Err(restore) => Err(anyhow!("{error}; {restore}")),
        };
    }
    let mut unlinked = false;
    if file.remove_checked(&mut unlinked).is_err() || !unlinked {
        println!(
            "Retired copy of {} is retained at {}; delete it by hand",
            profile.display(),
            parent.join(&retired).display()
        );
    }
    Ok(())
}

/// Stage the replacement bytes, then adopt the profile's mode before the guard
/// captures them. `GuardedFile` pins ctime, so a chmod after `stage` invalidates
/// the very file the caller is about to move into place.
fn stage_profile(parent: &Path, updated: &[u8], #[cfg(unix)] mode: u32) -> Result<GuardedFile> {
    let staged = GuardedFile::stage(parent, updated, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = parent.join(staged.name());
        let name = staged.name().to_string();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
        drop(staged);
        GuardedFile::open_bounded(parent, &name, MAX_PROFILE)?
            .context("the staged profile replacement disappeared before it could be verified")
    }
    #[cfg(not(unix))]
    Ok(staged)
}

fn plan_completions(
    home: &Path,
    directory: &Path,
    selected: &Path,
    verb: &str,
) -> Vec<(PathBuf, GuardedFile)> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let data = absolute("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"));
    let config = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    let candidates = [
        (
            clap_complete::Shell::Bash,
            data.join("bash-completion").join("completions"),
            "codewhale",
        ),
        (clap_complete::Shell::Zsh, home.join(".zfunc"), "_codewhale"),
        (
            clap_complete::Shell::Fish,
            config.join("fish").join("completions"),
            "codewhale.fish",
        ),
    ];
    let present: Vec<_> = candidates
        .into_iter()
        .filter(|(_, directory, name)| directory.join(name).symlink_metadata().is_ok())
        .collect();
    if present.is_empty() {
        return Vec::new();
    }
    // The receipt's other commands (`codew`) and the selected executable are
    // this installation, not a second one. Only a copy outside the directory
    // being removed keeps the completions.
    let install_directory = std::fs::canonicalize(directory).ok();
    let others_remain = codewhale_release::installs::inventory(Some(selected))
        .entries
        .iter()
        .any(|entry| {
            std::fs::canonicalize(&entry.path).ok().as_deref() != Some(selected)
                && entry
                    .path
                    .parent()
                    .and_then(|parent| std::fs::canonicalize(parent).ok())
                    .is_none_or(|parent| Some(&parent) != install_directory.as_ref())
        });
    let mut files = Vec::new();
    for (shell, directory, name) in present {
        let path = directory.join(name);
        if others_remain {
            println!(
                "Kept completion file {}: another Codewhale copy remains on PATH.",
                path.display()
            );
            continue;
        }
        let mut file = match GuardedFile::open_bounded(&directory, name, MAX_COMPLETION) {
            Ok(Some(file)) => file,
            Ok(None) => continue,
            Err(error) => {
                println!(
                    "Not read: {} ({error}). If you generated it with codewhale completion, delete it by hand.",
                    path.display()
                );
                continue;
            }
        };
        let generated = file
            .read_bounded(MAX_COMPLETION)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .is_some_and(|text| is_generated_completion(shell, &text));
        if generated {
            println!("{verb} completion file {}", path.display());
            files.push((path, file));
        } else {
            println!(
                "Left {} unchanged: it is not a Codewhale-generated completion script.",
                path.display()
            );
        }
    }
    files
}

fn is_generated_completion(shell: clap_complete::Shell, text: &str) -> bool {
    let bin = crate::COMPLETION_BIN_NAME;
    match shell {
        clap_complete::Shell::Bash => {
            text.contains(&format!("_{bin}()")) && text.contains(&format!("complete -F _{bin}"))
        }
        clap_complete::Shell::Zsh => {
            text.starts_with(&format!("#compdef {bin}")) && text.contains(&format!("_{bin}()"))
        }
        clap_complete::Shell::Fish => text.contains(&format!("complete -c {bin}")),
        _ => false,
    }
}

type CurlInstallEntries = (PathBuf, Vec<(String, String)>);

fn curl_install_entries(identity: &UpdateExecutableIdentity) -> Result<Option<CurlInstallEntries>> {
    let Some(directory) = identity
        .path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
    else {
        return Ok(None);
    };
    let Some(home) = dirs::home_dir().and_then(|home| home.canonicalize().ok()) else {
        return Ok(None);
    };
    let local = home.join(".local");
    let known = directory == local.join("bin")
        || directory == local.join("codewhale").join("bin")
        || (cfg!(windows) && directory == home.join("bin"))
        || (directory.parent() == Some(local.as_path())
            && directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("codewhale-release.")));
    if !known {
        return Ok(None);
    }
    let mut entries = Vec::new();
    for name in COMMANDS.iter().copied() {
        let Some(mut file) = GuardedFile::open(&directory, name)? else {
            continue;
        };
        file.verify_owned()?;
        if file.sha256() == identity.file_hash.as_str() {
            entries.push((name.to_string(), identity.file_hash.clone()));
        }
    }
    let selected_listed = identity.path.file_name().is_some_and(|file_name| {
        entries
            .iter()
            .any(|(name, _)| file_name == std::ffi::OsStr::new(name))
    });
    Ok(selected_listed.then_some((directory, entries)))
}

fn windows_installer_uninstaller(executable: &Path) -> Option<PathBuf> {
    let bin = executable.parent()?;
    if !bin
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        return None;
    }
    let root = bin.parent()?;
    let uninstaller = root.join("Uninstall.exe");
    [&uninstaller, &root.join("update-user-path.ps1")]
        .iter()
        .all(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()))
        .then_some(uninstaller)
}

#[cfg(windows)]
fn delete_after_exit(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let system = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
    let powershell = Path::new(&system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    std::process::Command::new(powershell)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "Wait-Process -Id {} -ErrorAction SilentlyContinue; Remove-Item -LiteralPath $env:CODEWHALE_UNINSTALL_RETIRED_PATH -Force",
            std::process::id()
        ))
        .env("CODEWHALE_UNINSTALL_RETIRED_PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .context("failed to start the deferred deletion helper")?;
    Ok(())
}

fn purge_targets(selected: Option<&Path>) -> Result<Vec<PathBuf>> {
    let home = codewhale_config::codewhale_home()?;
    let explicit = codewhale_config::codewhale_home_is_explicit();
    let mut targets = vec![home.clone()];
    if !explicit {
        targets.push(codewhale_config::legacy_deepseek_home()?.join("snapshots"));
    }
    let user_home = dirs::home_dir()
        .context("failed to resolve home directory")?
        .canonicalize()
        .context("failed to resolve the real home directory")?;
    let install_directory = selected
        .and_then(Path::parent)
        .map(std::fs::canonicalize)
        .transpose()
        .context("failed to resolve the install directory before data purge")?;
    let mut present = Vec::new();
    for target in targets {
        let target: PathBuf = target.components().collect();
        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", target.display()));
            }
        };
        if !metadata.is_dir() {
            bail!(
                "Preserved {}: it is not a real directory; no data was deleted",
                target.display()
            );
        }
        if explicit && target == home && target.file_name().is_none_or(|name| name != ".codewhale")
        {
            bail!(
                "Preserved explicit CODEWHALE_HOME {}: review it and delete it by hand; no data was deleted",
                target.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid()
                != codewhale_config::private_directory::PrivateDirectory::current_user_id()
            {
                bail!(
                    "Preserved {}: it belongs to another user; no data was deleted",
                    target.display()
                );
            }
        }
        let target = target
            .canonicalize()
            .with_context(|| format!("failed to resolve data directory {}", target.display()))?;
        if user_home.starts_with(&target)
            || target.parent().is_none()
            || install_directory
                .as_ref()
                .is_some_and(|install| install.starts_with(&target))
        {
            bail!(
                "Preserved {}: it contains the home or install directory; no data was deleted",
                target.display()
            );
        }
        present.push(target);
    }
    Ok(present)
}

fn purge(yes: bool, selected: Option<&Path>) -> Result<()> {
    let present = purge_targets(selected)?;
    println!(
        "Data purge scope: Codewhale's config, plaintext secrets file, sessions, logs, skills, plugins, tasks, caches, recovery records and snapshots in the directories below. Nothing else is searched for. Data can be shared by other installations; purging it affects those copies too."
    );
    if present.is_empty() {
        println!("Codewhale data directories are already absent.");
    }
    for target in &present {
        println!(
            "{} {}",
            if yes { "Delete" } else { "Would delete" },
            target.display()
        );
    }
    println!(
        "Not covered: OS keychain/keyring entries (run codewhale auth clear --provider <id> before removing the program) and per-workspace .codewhale directories."
    );
    if !yes {
        println!(
            "No data was changed. Adding --yes deletes these directories only after a receipted installation is removed in the same run."
        );
        return Ok(());
    }
    for target in present {
        std::fs::remove_dir_all(&target).with_context(|| {
            format!(
                "data purge stopped at {}; inspect what remains",
                target.display()
            )
        })?;
        println!("Deleted {}", target.display());
    }
    Ok(())
}

fn report_path_copies(selected: Option<&Path>) {
    let inventory = codewhale_release::installs::inventory(selected);
    let lines = inventory.summary_lines(false, env!("CARGO_PKG_VERSION"));
    if inventory
        .entries
        .iter()
        .all(|entry| Some(entry.path.as_path()) == selected)
    {
        println!("No other Codewhale command remains on PATH.");
        return;
    }
    println!("Other Codewhale copies on this machine (not changed by uninstall):");
    for line in lines {
        println!("  {line}");
    }
}
