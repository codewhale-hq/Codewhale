use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

fn pin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/dsh-creator")
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn manifest() -> Value {
    serde_json::from_slice(&fs::read(pin_dir().join("UPSTREAM.json")).unwrap()).unwrap()
}

#[test]
fn every_pinned_skill_file_equals_its_recorded_hash_and_keeps_lf() {
    let manifest = manifest();
    assert_eq!(manifest["version"], "0.2.2-rc.1");
    let files = manifest["files"].as_array().unwrap();
    assert_eq!(files.len(), 33);
    for entry in files {
        let path = entry["path"].as_str().unwrap();
        assert_eq!(entry["verbatim"], true, "{path}");
        let bytes = fs::read(pin_dir().join(path)).unwrap();
        assert_eq!(
            bytes.len() as u64,
            entry["bytes"].as_u64().unwrap(),
            "{path}"
        );
        assert_eq!(sha256(&bytes), entry["sha256"].as_str().unwrap(), "{path}");
        assert!(!bytes.contains(&b'\r'), "{path} must keep LF line endings");
    }
}

#[test]
fn the_pin_directory_holds_exactly_the_listed_files() {
    let manifest = manifest();
    let mut listed: BTreeSet<String> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap().to_string())
        .collect();
    listed.extend(["UPSTREAM.json".to_string(), "LICENSE".to_string()]);
    let mut found = Vec::new();
    walk(&pin_dir(), &mut found);
    let present: BTreeSet<String> = found
        .iter()
        .map(|path| {
            path.strip_prefix(pin_dir())
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .filter(|path| !path.starts_with("host-notes/"))
        .collect();
    assert_eq!(present, listed);
}

#[test]
fn the_licence_matches_the_recorded_hash() {
    let manifest = manifest();
    let license = fs::read(pin_dir().join("LICENSE")).unwrap();
    assert_eq!(
        sha256(&license),
        manifest["license_sha256"].as_str().unwrap()
    );
    assert!(license.starts_with(b"MIT License\n\nCopyright (c) 2026 DeepSeek\n"));
}
