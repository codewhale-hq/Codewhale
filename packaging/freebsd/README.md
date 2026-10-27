# FreeBSD packaging for CodeWhale

Resolves #1097 — FreeBSD install via `cargo` today, native `pkg` port planned.

## Current: cargo install (supported)

FreeBSD has no prebuilt GitHub Release asset. The npm wrapper intentionally fails with
`Unsupported platform: freebsd` and points to the Cargo path. Build from source with
Rust 1.89 or newer (the workspace `rust-version`):

```bash
pkg install -y rust pkgconf git  # or: pkg install -y lang/rust devel/pkgconf devel/git
cargo install codewhale-cli --locked   # provides `codewhale`
codewhale --version
codewhale doctor
```

Release installers (v0.9.5+) expose only `codewhale` + `codew`. Cargo installs
`codewhale`; users who want the optional short name can add a `codew` symlink.
There is no separate `codewhale-tui` binary to install. If you previously
installed `codewhale-tui` from ports or Cargo, run
`cargo uninstall codewhale-tui`, then rebuild `codewhale-cli`.

Linux `libdbus-1` / `pkg-config` build deps are not needed on FreeBSD for the default
feature set; the `rquickjs` FreeBSD bindings are generated at build time via `bindgen`
(see the `target_os = "freebsd"` dependency in `crates/workflow-js/Cargo.toml`, added by
`1582ba965`/`5eb0385e8`).

## Target validation

- No CI job builds or checks the Rust tier-2 target `x86_64-unknown-freebsd`, so FreeBSD source builds are not verified before a release. To check one, run `cargo check --target x86_64-unknown-freebsd -p codewhale-cli --locked` on a FreeBSD host.
- `aarch64-unknown-freebsd` cross-check is tracked for future hardware.
- The release matrix in `.github/workflows/release-artifacts.yml` remains 7×1 (Linux musl x64, Linux musl arm64, Android arm64, macOS x64/arm64, Windows x64/arm64). FreeBSD is a source-build target — no prebuilt asset, no matrix bloat, but the row in `docs/INSTALL.md` is authoritative.

## Future: native pkg / port

A proper FreeBSD port (`ports-mgmt` / `pkg install codewhale`) is the long-game follow-up to #1097.
Desired shape (not yet submitted):

```
ports/sysutils/codewhale/
  Makefile     # USES=cargo, CARGO_CRATES via `cargo make-port` or `cargo-crates` helper
  distinfo
  pkg-descr
  pkg-plist   # bin/codewhale, bin/codew
```

The Makefile should `BUILD_DEPENDS` on `lang/rust` and `devel/pkgconf`, run `cargo build --locked --release -p codewhale-cli`,
and install only `codewhale` + `codew`. Tests can reuse `cargo test -p codewhale-cli --lib` if desired.

If you can test on FreeBSD 14.x (amd64), please run the smoke from `docs/INSTALL.md#7-build-from-source`
and report the `codewhale doctor` JSON in #1097.

## Verification

```bash
# on a FreeBSD 14.1 amd64 host or VM
rustc --version  # >= 1.89
cargo install --path crates/cli --locked
codewhale --version
codewhale exec --auto "run pwd"
```
