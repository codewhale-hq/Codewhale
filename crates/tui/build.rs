use std::path::PathBuf;

fn main() {
    // Shared target directories may reuse this executable in another worktree.
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
    );
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");
    codewhale_build_support::declare_rerun_conditions(&manifest_dir);
    codewhale_build_support::emit_build_version(&manifest_dir, env!("CARGO_PKG_VERSION"));
}
