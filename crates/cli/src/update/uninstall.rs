use super::*;
use codewhale_tui::delivery_files::GuardedFile;
use std::collections::BTreeMap;

const RECEIPT: &str = ".codewhale-install.sha256";
const MAX_RECEIPT: usize = 512;
#[cfg(windows)]
const COMMANDS: &[&str] = &["codewhale.exe", "codew.exe", "codewhale.bat"];
#[cfg(not(windows))]
const COMMANDS: &[&str] = &["codewhale", "codew"];

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
        println!(
            "Renewed command ownership receipt; previous receipt retained at {}",
            self.directory.join(backup).display()
        );
        self.receipt = staged;
        Ok(())
    }
}

pub(crate) fn run(yes: bool, purge_data: bool) -> Result<()> {
    let selected = std::env::current_exe().ok();
    let result = remove(yes);
    report_path_copies(selected.as_deref());
    result?;
    if purge_data {
        purge(yes)
    } else {
        println!(
            "User data was not touched. To also delete Codewhale's own data directory, add --purge-data."
        );
        Ok(())
    }
}

fn remove(yes: bool) -> Result<()> {
    let identity = update_executable_identity()?;
    let method = InstallMethod::detect(&identity.path);
    println!("Selected executable: {}", identity.path.display());
    println!(
        "Program removal preserves user state, credentials, sessions, workspaces, shell profiles and services."
    );
    if !method.supports_self_update() {
        println!(
            "This copy belongs to {}. Remove it with: {}",
            method.label(),
            manager_command(method, &identity.path)
        );
        return Ok(());
    }
    if let Some(uninstaller) = windows_installer_uninstaller(&identity.path) {
        println!(
            "This copy belongs to the Codewhale Windows installer. Remove it from Settings > Apps > CodeWhale, or run: \"{}\"",
            uninstaller.display()
        );
        return Ok(());
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
    let ownership = CommandReceipt::capture(&identity.path)?.with_context(|| format!(
        "No command ownership receipt beside {}. This legacy/manual copy is preserved. Inspect its exact path and provenance before removing it manually; reinstalling into a fresh directory creates a receipted install.", identity.path.display()))?;
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
    validate_primary_update_identity(&identity)?;
    if !yes {
        println!(
            "No files were changed. Run this executable again with uninstall --yes to remove only this plan."
        );
        return Ok(());
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
    println!(
        "Removed this receipted release installation. Data and other installations are unchanged. {}",
        if cfg!(windows) {
            "Remove its directory from your user PATH if you added it, then open a new terminal."
        } else {
            "Run hash -r in your shell."
        }
    );
    Ok(())
}

fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

fn manager_command(method: InstallMethod, executable: &Path) -> String {
    let real = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    let names: Vec<_> = real.components().map(|part| part.as_os_str()).collect();
    let position = |marker: &str| {
        names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(marker))
    };
    let prefix = |count: usize| names[..count].iter().collect::<PathBuf>();
    match method {
        InstallMethod::Cargo => match real.parent().and_then(Path::parent) {
            Some(root) => format!("cargo uninstall codewhale-cli --root {}", quoted(root)),
            None => method.uninstall_command().to_string(),
        },
        InstallMethod::Homebrew => {
            match position("Cellar").and_then(|index| names.get(index + 1)) {
                Some(formula) => format!("brew uninstall {}", formula.to_string_lossy()),
                None => method.uninstall_command().to_string(),
            }
        }
        InstallMethod::Npm => match position("node_modules") {
            Some(index) if index > 0 => {
                let mut root = prefix(index);
                if !cfg!(windows) && root.file_name().is_some_and(|name| name == "lib") {
                    root.pop();
                }
                format!("npm uninstall -g codewhale --prefix {}", quoted(&root))
            }
            _ => method.uninstall_command().to_string(),
        },
        _ => method.uninstall_command().to_string(),
    }
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
    let literal = path.display().to_string().replace('\'', "''");
    std::process::Command::new(powershell)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "Wait-Process -Id {} -ErrorAction SilentlyContinue; Remove-Item -LiteralPath '{literal}' -Force",
            std::process::id()
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .context("failed to start the deferred deletion helper")?;
    Ok(())
}

fn purge(yes: bool) -> Result<()> {
    let home = codewhale_config::codewhale_home()?;
    let explicit = codewhale_config::codewhale_home_is_explicit();
    let mut targets = vec![home.clone()];
    if !explicit {
        targets.push(codewhale_config::legacy_deepseek_home()?.join("snapshots"));
    }
    let user_home = dirs::home_dir().context("failed to resolve home directory")?;
    println!(
        "Data purge scope: Codewhale's config, plaintext secrets file, sessions, logs, skills, plugins, tasks and caches in the directories below. Nothing else is searched for."
    );
    let mut present = Vec::new();
    for target in targets {
        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                println!("Already absent: {}", target.display());
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", target.display()));
            }
        };
        if !metadata.is_dir() || user_home.starts_with(&target) || target.parent().is_none() {
            bail!(
                "Preserved {}: it is not a real directory, or it is your home or one of its parents; no data was deleted",
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
        println!(
            "{} {}",
            if yes { "Delete" } else { "Would delete" },
            target.display()
        );
        present.push(target);
    }
    println!(
        "Not covered: OS keychain/keyring entries (run codewhale auth clear --provider <id> before removing the program), per-workspace .codewhale directories, shell profile lines and completion files."
    );
    if !yes {
        println!("No data was changed. Add --yes to delete exactly these directories.");
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
    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ".ps1"]
    } else {
        &[""]
    };
    let mut seen = std::collections::HashSet::new();
    let mut remaining = false;
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for name in [
            "codewhale",
            "codew",
            "codewhale-tui",
            "deepseek",
            "deepseek-tui",
        ] {
            for extension in extensions {
                let path = directory.join(format!("{name}{extension}"));
                if Some(path.as_path()) == selected {
                    continue;
                }
                if seen.insert(path.clone()) && std::fs::symlink_metadata(&path).is_ok() {
                    remaining = true;
                    println!(
                        "Another copy remains on PATH (preserved): {}. Run it with uninstall to see who owns it and how to remove it.",
                        path.display()
                    );
                }
            }
        }
    }
    if !remaining {
        println!("No other Codewhale command remains on PATH.");
    }
}
