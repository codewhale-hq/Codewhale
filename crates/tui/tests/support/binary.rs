//! Canonical executable lookup shared by both mounted acceptance harnesses.
//!
//! The Engine crate is a library. Build `codewhale-cli --bin codewhale` before
//! acceptance tests, or supply QA_TUI_BIN for a deliberately selected QA build.

use std::path::{Path, PathBuf};

pub fn codewhale() -> PathBuf {
    let qa = std::env::var_os("QA_TUI_BIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let cargo = std::env::var_os("CARGO_BIN_EXE_codewhale")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_codewhale").map(PathBuf::from));
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("Engine workspace root");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let target = if target.is_absolute() {
        target
    } else {
        workspace.join(target)
    };
    resolve(
        qa,
        cargo,
        &std::env::current_exe().expect("current test executable"),
        &target,
    )
    .unwrap_or_else(|message| panic!("{message}"))
}

fn resolve(
    qa: Option<PathBuf>,
    cargo: Option<PathBuf>,
    test_executable: &Path,
    target_directory: &Path,
) -> Result<PathBuf, String> {
    if let Some(path) = qa.or(cargo) {
        return require_file(path);
    }
    let directory = test_executable
        .parent()
        .ok_or("test executable has no parent")?;
    let directory = if directory.ends_with("deps") {
        directory
            .parent()
            .ok_or("test deps directory has no parent")?
    } else if directory.ends_with("out") {
        directory
            .ancestors()
            .skip(1)
            .find(|ancestor| ancestor.ends_with("build"))
            .and_then(Path::parent)
            .unwrap_or(directory)
    } else {
        directory
    };
    let name = format!("codewhale{}", std::env::consts::EXE_SUFFIX);
    let adjacent = directory.join(&name);
    if adjacent.is_file() {
        return Ok(adjacent);
    }
    // Cargo's split build-dir stores test deps separately from final binaries.
    // Only inspect the known workspace/explicit target directory; never scan
    // PATH, another checkout, or a legacy executable name.
    let profile = directory
        .file_name()
        .ok_or("test profile directory has no name")?;
    // Explicit --target builds add one triple directory to both layouts.
    // Prefer that exact projection over a coexisting host executable.
    if let Some(triple) = directory.parent().and_then(Path::file_name) {
        let cross_target = target_directory.join(triple).join(profile).join(&name);
        if cross_target.is_file() {
            return Ok(cross_target);
        }
    }
    require_file(target_directory.join(profile).join(&name))
}

// This synchronous resolver belongs only to Cargo acceptance test binaries.
#[cfg(test)]
fn require_file(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_file() {
        if path.is_absolute() {
            Ok(path)
        } else {
            path.canonicalize().map_err(|error| {
                format!(
                    "cannot resolve selected Codewhale executable {} before changing workspace: {error}",
                    path.display()
                )
            })
        }
    } else {
        Err(format!(
            "canonical Codewhale executable is missing at {}: build `cargo build -p codewhale-cli --bin codewhale --locked` before Engine acceptance tests, or set QA_TUI_BIN to the intended QA binary",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_explicit_selection_survives_an_acceptance_workspace_change() {
        let current = std::env::current_dir().unwrap();
        let root = tempfile::TempDir::new_in(&current).unwrap();
        let binary = root.path().join("selected QA build");
        std::fs::write(&binary, b"canonical").unwrap();
        let relative = binary.strip_prefix(&current).unwrap().to_path_buf();
        for (qa, cargo) in [
            (Some(relative.clone()), None),
            (None, Some(relative.clone())),
        ] {
            let selected = resolve(qa, cargo, &binary, root.path()).unwrap();
            assert!(selected.is_absolute());
            assert_eq!(selected, binary.canonicalize().unwrap());
            assert!(
                root.path()
                    .join("another workspace")
                    .join(selected)
                    .is_file()
            );
        }
    }

    #[test]
    fn canonical_lookup_uses_the_test_target_directory_and_never_legacy_binary() {
        let root = tempfile::TempDir::new().unwrap();
        let debug = root.path().join("target with spaces").join("debug");
        std::fs::create_dir_all(debug.join("deps")).unwrap();
        let canonical = debug.join(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
        let legacy = debug.join(format!("codewhale-tui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&legacy, b"legacy").unwrap();
        let executable = debug.join("deps").join("integration-hash");
        assert!(
            resolve(
                None,
                None,
                &executable,
                &root.path().join("target with spaces")
            )
            .is_err()
        );
        std::fs::write(&canonical, b"canonical").unwrap();
        assert_eq!(
            resolve(
                None,
                None,
                &executable,
                &root.path().join("target with spaces")
            )
            .unwrap(),
            canonical
        );
    }

    #[test]
    fn explicit_qa_selection_wins_and_a_missing_selection_never_falls_back() {
        let root = tempfile::TempDir::new().unwrap();
        let qa = root.path().join("selected release");
        let cargo = root.path().join("codewhale");
        std::fs::write(&qa, b"qa").unwrap();
        std::fs::write(&cargo, b"cargo").unwrap();
        assert_eq!(
            resolve(Some(qa.clone()), Some(cargo.clone()), &cargo, root.path()).unwrap(),
            qa
        );
        assert!(
            resolve(
                Some(root.path().join("missing")),
                Some(cargo),
                &qa,
                root.path()
            )
            .is_err()
        );
    }

    #[test]
    fn split_build_cache_resolves_the_known_final_target_and_cross_profile() {
        let root = tempfile::TempDir::new().unwrap();
        let target = root.path().join("final target");
        let cache = root.path().join("build cache");
        for triple in [None, Some("aarch64-unknown-linux-gnu")] {
            let final_profile = match triple {
                Some(triple) => target.join(triple).join("debug"),
                None => target.join("debug"),
            };
            std::fs::create_dir_all(&final_profile).unwrap();
            let canonical =
                final_profile.join(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&canonical, b"canonical").unwrap();
            let test = match triple {
                Some(triple) => cache.join(triple).join("debug/deps/integration-hash"),
                None => cache.join("debug/deps/integration-hash"),
            };
            // The requested cross target must not select coexisting host output.
            assert_eq!(resolve(None, None, &test, &target).unwrap(), canonical);
        }
    }

    #[test]
    fn per_unit_build_layout_resolves_the_final_target_and_cross_profile() {
        let root = tempfile::TempDir::new().unwrap();
        let target = root.path().join("final target");
        for triple in [None, Some("aarch64-unknown-linux-gnu")] {
            let final_profile = match triple {
                Some(triple) => target.join(triple).join("debug"),
                None => target.join("debug"),
            };
            std::fs::create_dir_all(&final_profile).unwrap();
            let canonical =
                final_profile.join(format!("codewhale{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&canonical, b"canonical").unwrap();
            let test = final_profile
                .join("build")
                .join("codewhale-tui")
                .join("0123456789abcdef")
                .join("out")
                .join("integration-hash");
            assert_eq!(resolve(None, None, &test, &target).unwrap(), canonical);
        }
    }
}
