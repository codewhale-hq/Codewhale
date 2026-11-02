//! The control endpoint is a pure function of the selected runtime store.
//!
//! These cover the decision, not the transport: `daemon_socket`'s own tests
//! cover the path derivation, and the owner tests cover the socket being
//! bound. What is pinned here is which endpoint this `serve` run picks, and
//! that it picks the same one however the store was spelled.
use super::*;
use crate::test_support::{EnvVarGuard, lock_test_env};

fn inputs() -> codewhale_paths::daemon_socket::SocketPathInputs {
    codewhale_paths::daemon_socket::SocketPathInputs {
        explicit: None,
        codewhale_home_override: None,
        xdg_runtime_dir: None,
        user_home: Some(PathBuf::from("/home/whale")),
        macos: false,
    }
}

fn default_socket() -> PathBuf {
    PathBuf::from("/home/whale/.codewhale/run/daemon.sock")
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("cw-control-socket-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    root
}

fn derived_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

#[test]
fn an_explicit_socket_selection_always_wins() -> Result<()> {
    let resolved = store_selected_control_socket_with(
        Some(PathBuf::from("/tmp/explicit.sock")),
        Path::new("/tmp/opened-store"),
        Some(Path::new("/tmp/opened-store")),
        &inputs(),
    )?;
    assert_eq!(resolved, PathBuf::from("/tmp/explicit.sock"));
    Ok(())
}

#[test]
fn the_default_selection_keeps_the_well_known_socket() -> Result<()> {
    let resolved =
        store_selected_control_socket_with(None, Path::new("/tmp/opened-store"), None, &inputs())?;
    assert_eq!(resolved, default_socket());
    Ok(())
}

#[test]
fn a_selected_store_derives_its_own_sibling_endpoint() -> Result<()> {
    let root = temp_root("derived");
    let store = root.join("runtime-store");
    std::fs::create_dir_all(&store)?;
    let canonical = store.canonicalize()?;

    let resolved = store_selected_control_socket_with(None, &store, Some(&canonical), &inputs())?;
    assert_eq!(
        resolved.parent().unwrap(),
        default_socket().parent().unwrap()
    );
    let name = derived_name(&resolved);
    assert!(name.starts_with("store-"), "derived endpoint: {name}");
    assert!(name.ends_with(".sock"), "derived endpoint: {name}");

    // Same store, same endpoint — and a different store cannot share it.
    let again = store_selected_control_socket_with(None, &store, Some(&canonical), &inputs())?;
    assert_eq!(resolved, again);
    let other = root.join("other-store");
    std::fs::create_dir_all(&other)?;
    let other_canonical = other.canonicalize()?;
    let elsewhere =
        store_selected_control_socket_with(None, &other, Some(&other_canonical), &inputs())?;
    assert_ne!(resolved, elsewhere);
    Ok(())
}

#[test]
fn a_store_the_override_does_not_name_falls_back_to_the_default_endpoint() -> Result<()> {
    // The opened store is not the one the override selected (the selection
    // changed under us, or the store came from the fallback chain). The
    // default endpoint is the conservative answer: the strict receipt
    // comparison there still refuses a wrong-store attachment.
    let resolved = store_selected_control_socket_with(
        None,
        Path::new("/tmp/some-other-store"),
        Some(Path::new("/tmp/the-override-store")),
        &inputs(),
    )?;
    assert_eq!(resolved, default_socket());
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_aliased_store_selection_still_derives_its_own_endpoint() -> Result<()> {
    // The store chain resolves the override as written; the control plane
    // canonicalizes it. Comparing the two in different forms would let a
    // symlinked (or relative, or `/var`-shaped on macOS) override keep the
    // default endpoint, which is exactly the collision this derivation exists
    // to remove.
    let root = temp_root("alias");
    let real = root.join("real");
    std::fs::create_dir_all(real.join("store"))?;
    let link = root.join("link");
    std::os::unix::fs::symlink(&real, &link)?;
    let aliased_store = link.join("store");
    let canonical_override = aliased_store.canonicalize()?;
    assert_ne!(aliased_store, canonical_override, "fixture must alias");

    let resolved = store_selected_control_socket_with(
        None,
        &aliased_store,
        Some(&canonical_override),
        &inputs(),
    )?;
    let name = derived_name(&resolved);
    assert!(
        name.starts_with("store-"),
        "an aliased selection must still derive its own endpoint, got {name}"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn the_runtime_dir_override_reaches_the_control_endpoint() -> Result<()> {
    let _env = lock_test_env();
    // A short root on purpose: unix socket paths have a hard kernel limit, and
    // a fixture under the (long) macOS temp dir cannot hold a derived name.
    let root = std::env::var_os("CODEWHALE_TEST_SHORT_TMPDIR")
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
        .join(format!("cw-control-socket-env-{}", std::process::id()));
    let home = root.join("home");
    let store = root.join("runtime-store");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&store)?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", &home);
    let _runtime_dir = EnvVarGuard::set("CODEWHALE_RUNTIME_DIR", &store);

    let resolved = store_selected_control_socket(None, &store).await?;
    let name = derived_name(&resolved);
    assert!(
        name.starts_with("store-"),
        "a selected store must reach the derived endpoint, got {}",
        resolved.display()
    );
    assert!(
        resolved.starts_with(&home),
        "the derived endpoint stays beside the well-known one, got {}",
        resolved.display()
    );
    Ok(())
}
