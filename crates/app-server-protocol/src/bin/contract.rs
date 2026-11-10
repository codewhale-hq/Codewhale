#[path = "../codegen/mod.rs"]
mod codegen;

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if !matches!(arguments.as_slice(), [mode] if mode == "--write" || mode == "--check") {
        return Err("use --write or --check against the contract manifest outputs".into());
    }
    let write = arguments[0] == "--write";
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(package.join(codegen::MANIFEST_PATH))?;
    let pinned = std::fs::read_to_string(package.join(codegen::PINNED_SCHEMA_PATH))?;
    let outputs = codegen::generate(&manifest, &pinned)?;
    let mut drift = Vec::new();
    for (path, text) in outputs.files {
        let target = package.join(path);
        if write {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, text)?;
        } else if std::fs::read_to_string(&target).ok().as_deref() != Some(text.as_str()) {
            drift.push(path);
        }
    }
    if drift.is_empty() {
        Ok(())
    } else {
        Err(format!("generated contract drift: {}", drift.join(", ")).into())
    }
}
