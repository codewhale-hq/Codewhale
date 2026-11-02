//! How *this* binary was installed, and therefore which command updates it.
//!
//! `codewhale update` replaces the running executable in place. That is the
//! right thing for a binary the user downloaded from GitHub Releases, and the
//! wrong thing for one a package manager owns: overwriting Homebrew's Cellar
//! binary or npm's `node_modules` payload leaves the manager's metadata
//! describing a version that is no longer on disk, and the next
//! `brew upgrade` / `npm install -g` silently reverts the user.
//!
//! So before we tell anyone to run anything, we work out who owns the file.
//! Detection is path-based where managers use distinct prefixes. Omarchy's
//! AUR packages live in the ordinary `/usr/bin` prefix, so that case also
//! consults the local pacman ownership database. No network access is needed.

use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

/// Environment variable that overrides install-method detection.
///
/// Accepts `npm`, `homebrew` (or `brew`), `cargo`, `omarchy`, `scoop`,
/// `winget`, and `binary`.
/// Anything else is ignored and detection falls back to automatic detection.
/// Packagers who relocate the binary somewhere the heuristics cannot read —
/// and users debugging a wrong guess — set this. Recognized managed paths
/// take precedence; `binary` cannot authorize overwriting a package-owned file.
pub const INSTALL_METHOD_ENV: &str = "CODEWHALE_INSTALL_METHOD";

/// Shared migration instructions for every runtime update surface. The new
/// directory avoids guessing ownership or overwriting a mixed installation.
pub const GITHUB_MIGRATION_HELP: &str = r#"Install the official GitHub release into a fresh user directory (macOS/Linux):
  mkdir -p "$HOME/.local"
  codewhale_install_dir="$(mktemp -d "$HOME/.local/codewhale-release.XXXXXX")"
  curl -fsSL https://codewhale.net/install.sh | CODEWHALE_INSTALL_DIR="$codewhale_install_dir" sh
  "$codewhale_install_dir/codewhale" --version
  export PATH="$codewhale_install_dir:$PATH"
  hash -r
  command -v codewhale codew
Future updates: "$codewhale_install_dir/codewhale" update
Keep the chosen PATH directory in your shell profile after verifying it.
Windows: https://github.com/codewhale-hq/CodeWhale/releases/latest
PATH and migration: https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md"#;

pub const DESKTOP_APP_UPDATE: &str = "Codewhale > Check for Updates";

pub const DESKTOP_APP_UPDATE_HELP: &str = "The Codewhale app updates this copy together with the app. Use Codewhale > Check for Updates. `codewhale update` never writes inside the app.";

/// The package manager (if any) that owns the running executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallMethod {
    /// Global npm install — the `codewhale` package under `node_modules`.
    Npm,
    /// Homebrew — a binary under a `Cellar` or `linuxbrew` prefix.
    Homebrew,
    /// `cargo install` — a binary under `~/.cargo/bin`.
    Cargo,
    /// An AUR/pacman package installed on Omarchy.
    Omarchy,
    /// Scoop — a binary under a `scoop/apps` prefix.
    Scoop,
    /// A WinGet portable package under `Microsoft/WinGet/Packages`.
    WinGet,
    DesktopApp,
    /// A release binary the user placed on disk themselves. The default, and
    /// the only case where in-place self-update is correct.
    Binary,
}

impl InstallMethod {
    /// Detect from an executable path, honouring [`INSTALL_METHOD_ENV`].
    ///
    /// `std::env::current_exe()` follows symlinks on Linux but not on macOS,
    /// where a Homebrew launch reports `/opt/homebrew/bin/codewhale` instead
    /// of the `Cellar` file it points at. When the path as given looks
    /// self-updatable, the symlink target is checked too, and a package
    /// manager that owns the real file wins.
    #[must_use]
    pub fn detect(exe: &Path) -> Self {
        let mut detected = Self::detect_with_omarchy_probe(exe, omarchy_package_owns);
        if detected.supports_self_update()
            && let Ok(real) = std::fs::canonicalize(exe)
            && real != exe
        {
            let owner = Self::detect_with_omarchy_probe(&real, omarchy_package_owns);
            if !owner.supports_self_update() {
                detected = owner;
            }
        }
        // An override can identify a relocated package, but cannot authorize
        // replacing a file that a known package manager owns.
        if !detected.supports_self_update() {
            return detected;
        }
        if let Some(forced) = std::env::var(INSTALL_METHOD_ENV)
            .ok()
            .and_then(|raw| Self::from_token(&raw))
        {
            return forced;
        }
        detected
    }

    fn detect_with_omarchy_probe(exe: &Path, owns_package: impl FnOnce(&Path) -> bool) -> Self {
        let path_method = Self::from_path(exe);
        if path_method != Self::Binary {
            return path_method;
        }
        if exe.parent().is_some_and(|bin| {
            bin.file_name().is_some_and(|name| name == "bin")
                && bin.parent().is_some_and(|root| {
                    [".crates.toml", ".crates2.json"]
                        .iter()
                        .any(|name| std::fs::symlink_metadata(root.join(name)).is_ok())
                })
        }) {
            return Self::Cargo;
        }
        if owns_package(exe) {
            Self::Omarchy
        } else {
            Self::Binary
        }
    }

    /// Path-only detection, with no environment lookup. Split out from
    /// [`detect`](Self::detect) so tests can exercise the heuristics without
    /// mutating process-global state.
    #[must_use]
    pub fn from_path(exe: &Path) -> Self {
        let components: Vec<String> = exe
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .map(str::to_ascii_lowercase)
            .collect();

        let has = |name: &str| components.iter().any(|c| c == name);

        if components.windows(3).any(|parts| {
            parts[0].ends_with(".app") && parts[1] == "contents" && parts[2] == "macos"
        }) || components
            .windows(2)
            .any(|pair| pair[0] == "gpui" && pair[1] == "engine")
        {
            return Self::DesktopApp;
        }

        // npm is checked first: a `node_modules` install *inside* a Homebrew
        // or Termux prefix is still npm's to update.
        if has("node_modules") {
            return Self::Npm;
        }
        if has("cellar") || has(".linuxbrew") || has("linuxbrew") {
            return Self::Homebrew;
        }
        let pair = |first: &str, second: &str| {
            components
                .windows(2)
                .any(|pair| pair[0] == first && pair[1] == second)
        };
        if pair("scoop", "apps") {
            return Self::Scoop;
        }
        if pair("winget", "packages") || pair("winget", "links") {
            return Self::WinGet;
        }
        // `.cargo/bin/codewhale` — require the pair so an unrelated `bin`
        // directory does not read as a Cargo install.
        if components
            .windows(2)
            .any(|pair| pair[0] == ".cargo" && pair[1] == "bin")
        {
            return Self::Cargo;
        }
        Self::Binary
    }

    fn from_token(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "npm" => Some(Self::Npm),
            "homebrew" | "brew" => Some(Self::Homebrew),
            "cargo" => Some(Self::Cargo),
            "omarchy" => Some(Self::Omarchy),
            "scoop" => Some(Self::Scoop),
            "winget" => Some(Self::WinGet),
            "desktop" | "app" => Some(Self::DesktopApp),
            "binary" | "release" => Some(Self::Binary),
            _ => None,
        }
    }

    /// The exact shell command that updates this install.
    ///
    /// Homebrew's primary formula is `codewhale`. Existing Cellar paths
    /// under the legacy `deepseek-tui` name still detect as Homebrew; those
    /// installs can keep using `brew upgrade deepseek-tui` during the
    /// overlap window, but new notices name the Codewhale formula.
    #[must_use]
    pub fn update_command(self) -> &'static str {
        match self {
            Self::Npm => "npm install -g codewhale@latest",
            Self::Homebrew => "brew upgrade codewhale",
            Self::Cargo => "cargo install codewhale-cli --locked --force",
            Self::Omarchy => "omarchy update",
            Self::Scoop => "scoop update codewhale",
            Self::WinGet => "winget upgrade --id HunterBown.CodeWhale",
            Self::DesktopApp => DESKTOP_APP_UPDATE,
            Self::Binary => "codewhale update",
        }
    }

    #[must_use]
    pub fn uninstall_command(self) -> &'static str {
        match self {
            Self::Npm => "npm uninstall -g codewhale",
            Self::Homebrew => {
                "brew uninstall codewhale (use the owning formula name for a legacy formula)"
            }
            Self::Cargo => "cargo uninstall codewhale-cli --root <the owning Cargo install root>",
            Self::Omarchy => {
                "pacman -Qo <the executable path>, then remove that package with your package manager"
            }
            Self::Scoop => "scoop uninstall codewhale",
            Self::WinGet => "winget uninstall --id HunterBown.CodeWhale",
            Self::DesktopApp => "remove the Codewhale app; this Engine is part of it",
            Self::Binary => "codewhale uninstall --yes",
        }
    }

    /// Whether `codewhale update` may replace this binary in place.
    ///
    /// False for every package-managed install: see the module docs for why
    /// overwriting a managed binary is worse than doing nothing.
    #[must_use]
    pub fn supports_self_update(self) -> bool {
        matches!(self, Self::Binary)
    }

    /// Short human label, for messages that name the owner of the install.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Homebrew => "Homebrew",
            Self::Cargo => "cargo",
            Self::Omarchy => "Omarchy",
            Self::Scoop => "Scoop",
            Self::WinGet => "WinGet",
            Self::DesktopApp => "the Codewhale app",
            Self::Binary => "release binary",
        }
    }
}

#[cfg(target_os = "linux")]
fn omarchy_package_owns(exe: &Path) -> bool {
    if !Path::new("/usr/share/omarchy/version").is_file() {
        return false;
    }

    Command::new("pacman")
        .args(["-Qo"])
        .arg(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(target_os = "linux"))]
fn omarchy_package_owns(_exe: &Path) -> bool {
    false
}

/// Detect the install method for the currently running executable.
///
/// Returns [`InstallMethod::Binary`] when the executable path cannot be
/// resolved — the conservative answer, because it is the one that tells the
/// user to run our own updater rather than a package manager command that may
/// not apply to them.
#[must_use]
pub fn current_install_method() -> InstallMethod {
    match std::env::current_exe() {
        Ok(exe) => InstallMethod::detect(&exe),
        Err(_) => InstallMethod::Binary,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn npm_global_install_is_detected_from_node_modules() {
        let exe = PathBuf::from("/usr/local/lib/node_modules/codewhale/bin/codewhale");
        assert_eq!(InstallMethod::from_path(&exe), InstallMethod::Npm);
        assert_eq!(
            InstallMethod::Npm.update_command(),
            "npm install -g codewhale@latest"
        );
        assert!(!InstallMethod::Npm.supports_self_update());
    }

    #[test]
    fn homebrew_install_is_detected_from_cellar_on_both_prefixes() {
        for exe in [
            "/opt/homebrew/Cellar/codewhale/0.9.8/bin/codewhale",
            "/usr/local/Cellar/codewhale/0.9.8/bin/codewhale",
            "/home/linuxbrew/.linuxbrew/Cellar/codewhale/0.9.8/bin/codewhale",
            "/opt/homebrew/Cellar/deepseek-tui/0.9.4/bin/codewhale",
            "/usr/local/Cellar/deepseek-tui/0.9.4/bin/codewhale",
            "/home/linuxbrew/.linuxbrew/Cellar/deepseek-tui/0.9.4/bin/codewhale",
        ] {
            assert_eq!(
                InstallMethod::from_path(&PathBuf::from(exe)),
                InstallMethod::Homebrew,
                "{exe} should read as Homebrew"
            );
        }
        assert_eq!(
            InstallMethod::Homebrew.update_command(),
            "brew upgrade codewhale"
        );
        assert!(!InstallMethod::Homebrew.supports_self_update());
    }

    /// macOS reports the launcher symlink from `current_exe()`, so detection
    /// must follow it into `Cellar` before allowing an in-place update.
    #[cfg(unix)]
    #[test]
    fn homebrew_symlink_launcher_is_detected_through_its_target() {
        let root = tempfile::tempdir().unwrap();
        let cellar_bin = root.path().join("Cellar/codewhale/0.10.0/bin");
        std::fs::create_dir_all(&cellar_bin).unwrap();
        let real = cellar_bin.join("codewhale");
        std::fs::write(&real, b"binary").unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let launcher = bin.join("codewhale");
        std::os::unix::fs::symlink(&real, &launcher).unwrap();

        assert_eq!(InstallMethod::from_path(&launcher), InstallMethod::Binary);
        assert_eq!(InstallMethod::detect(&launcher), InstallMethod::Homebrew);

        // A plain release binary with no package owner stays self-updatable.
        let plain = bin.join("codew");
        std::fs::write(&plain, b"binary").unwrap();
        assert!(InstallMethod::detect(&plain).supports_self_update());
    }

    #[test]
    fn cargo_install_requires_the_cargo_bin_pair() {
        assert_eq!(
            InstallMethod::from_path(&PathBuf::from("/home/u/.cargo/bin/codewhale")),
            InstallMethod::Cargo
        );
        // A bare `bin` directory is not a Cargo install.
        assert_eq!(
            InstallMethod::from_path(&PathBuf::from("/home/u/bin/codewhale")),
            InstallMethod::Binary
        );
        assert!(!InstallMethod::Cargo.supports_self_update());
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(root.path().join(".crates2.json"), b"{}").unwrap();
        assert_eq!(
            InstallMethod::detect(&bin.join("codewhale")),
            InstallMethod::Cargo
        );
    }

    #[test]
    fn npm_wins_over_an_enclosing_manager_prefix() {
        // npm installed under a Homebrew-managed node prefix is still npm's.
        let exe = PathBuf::from("/opt/homebrew/lib/node_modules/codewhale/bin/codewhale");
        assert_eq!(InstallMethod::from_path(&exe), InstallMethod::Npm);
    }

    #[test]
    fn omarchy_probe_only_claims_package_owned_plain_paths() {
        let managed = PathBuf::from("/usr/bin/codewhale");
        assert_eq!(
            InstallMethod::detect_with_omarchy_probe(&managed, |_| true),
            InstallMethod::Omarchy
        );
        assert_eq!(
            InstallMethod::detect_with_omarchy_probe(&managed, |_| false),
            InstallMethod::Binary
        );

        let npm = PathBuf::from("/usr/lib/node_modules/codewhale/bin/codewhale");
        assert_eq!(
            InstallMethod::detect_with_omarchy_probe(&npm, |_| {
                panic!("a path-owned install must not query pacman")
            }),
            InstallMethod::Npm
        );
    }

    #[test]
    fn termux_and_plain_release_binaries_self_update() {
        for exe in [
            "/data/data/com.termux/files/usr/bin/codewhale",
            "/usr/local/bin/codewhale",
            "/home/u/Downloads/codewhale",
        ] {
            let method = InstallMethod::from_path(&PathBuf::from(exe));
            assert_eq!(method, InstallMethod::Binary, "{exe} should self-update");
            assert!(method.supports_self_update());
            assert_eq!(method.update_command(), "codewhale update");
        }
    }

    #[test]
    fn env_tokens_map_to_methods_and_junk_is_ignored() {
        assert_eq!(InstallMethod::from_token("npm"), Some(InstallMethod::Npm));
        assert_eq!(
            InstallMethod::from_token("  BREW "),
            Some(InstallMethod::Homebrew)
        );
        assert_eq!(
            InstallMethod::from_token("homebrew"),
            Some(InstallMethod::Homebrew)
        );
        assert_eq!(
            InstallMethod::from_token("cargo"),
            Some(InstallMethod::Cargo)
        );
        assert_eq!(
            InstallMethod::from_token("omarchy"),
            Some(InstallMethod::Omarchy)
        );
        assert_eq!(
            InstallMethod::from_token("binary"),
            Some(InstallMethod::Binary)
        );
        assert_eq!(InstallMethod::from_token("apt"), None);
    }

    #[test]
    fn omarchy_install_is_package_managed() {
        assert_eq!(InstallMethod::Omarchy.update_command(), "omarchy update");
        assert_eq!(InstallMethod::Omarchy.label(), "Omarchy");
        assert!(!InstallMethod::Omarchy.supports_self_update());
    }
}
