use std::path::{Path, PathBuf};

fn retain(path: &Path, bytes: &[u8], write: bool) -> std::io::Result<()> {
    if write {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)
    } else if std::fs::read(path)? == bytes {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "generated contract drift: {}",
            path.display()
        )))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if !matches!(arguments.as_slice(), [mode] if mode == "--write" || mode == "--check") {
        return Err("use --write or --check against the declared package and SDK outputs".into());
    }
    let write = arguments[0] == "--write";
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository = package
        .parent()
        .and_then(Path::parent)
        .ok_or("package repository root")?;
    let schema = codewhale_app_server_protocol::export::runtime_schema();
    let typescript = codewhale_app_server_protocol::export::runtime_typescript(&schema);
    let json = serde_json::to_string_pretty(&schema)? + "\n";
    retain(
        &package.join("schema/json/runtime.schema.json"),
        json.as_bytes(),
        write,
    )?;
    retain(
        &package.join("schema/typescript/runtime.d.ts"),
        typescript.as_bytes(),
        write,
    )?;
    retain(
        &repository.join("npm/runtime-sdk/generated/runtime.d.ts"),
        typescript.as_bytes(),
        write,
    )?;
    Ok(())
}
