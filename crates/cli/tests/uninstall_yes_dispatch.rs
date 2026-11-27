#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use sha2::{Digest, Sha256};

fn codewhale_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_codewhale").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = option_env!("CARGO_BIN_EXE_codewhale") {
        return PathBuf::from(path);
    }
    let mut path = std::env::current_exe().expect("current test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("codewhale");
    path
}

#[test]
fn uninstall_yes_removes_a_receipted_install_and_keeps_user_data() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let bin = root_path.join("bin");
    let home = root_path.join("home");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    let installed = bin.join("codewhale");
    fs::copy(codewhale_binary(), &installed).unwrap();
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).unwrap();
    let digest: String = Sha256::digest(fs::read(&installed).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let receipt = bin.join(".codewhale-install.sha256");
    fs::write(&receipt, format!("{digest}  codewhale\n")).unwrap();
    fs::set_permissions(&receipt, fs::Permissions::from_mode(0o644)).unwrap();

    let data = home.join(".codewhale");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("config.toml"), "keep = true\n").unwrap();

    let xdg_data = root_path.join("xdg_data");
    let xdg_config = root_path.join("xdg_config");
    fs::create_dir(&xdg_data).unwrap();
    fs::create_dir(&xdg_config).unwrap();

    let output = Command::new(&installed)
        .env_clear()
        .args(["uninstall", "--yes"])
        .env("HOME", &home)
        .env("CODEWHALE_HOME", &data)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("PATH", &bin)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!installed.exists(), "stdout: {stdout}");
    assert!(!receipt.exists(), "stdout: {stdout}");
    assert!(data.join("config.toml").exists());
}
