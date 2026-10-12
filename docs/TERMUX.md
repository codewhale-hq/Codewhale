# Termux / Android arm64 Support

> 阅读简体中文版：[zh_hans/TERMUX.md](zh_hans/TERMUX.md)。

Codewhale provides an Android arm64 build and archive path for
[Termux](https://termux.dev). Treat Termux support as a preview until the
real-device runtime QA tracked in #4236 and #4242 is complete. This document
covers the install path and the platform-specific behavior differences you
should know about.

## Installation

Use the Android-specific GitHub release archive. The
[v0.9.11 release](https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.11)
includes `codewhale-android-arm64.tar.gz`; device support remains **preview**.
Follow [the Android / Termux installation steps](INSTALL.md#android--termux-arm64-preview)
to verify the archive against the matching `codewhale-bundles-sha256.txt`, then
run the bundled installer with `PREFIX="$PREFIX"` so commands go into
`$PREFIX/bin`. Use `codewhale update` for an existing direct installation;
keep package-managed files under their package manager's control.

If a release has no compatible Android archive or you are validating a source
build, Cargo remains a preview fallback inside Termux:

```sh
pkg install -y rust clang pkg-config make git
cargo install codewhale-cli --locked
```

The general macOS/Linux web installer is not the Android installation route.
Do not install `codewhale-linux-arm64` in Termux: Android uses Bionic libc and
a separate build target. A Linux release asset is not an Android binary.

## Platform behavior on Android

Codewhale's security model has three distinct layers on Android:

1. **Android's app sandbox** — Android assigns Termux its own app UID and
   applies the platform's SELinux and seccomp protections. Commands started by
   Codewhale inherit that app boundary and any storage or other permissions the
   user has granted to Termux. See the
   [Android application sandbox](https://source.android.com/docs/security/app-sandbox)
   and [Termux filesystem layout](https://github.com/termux/termux-packages/wiki/Termux-file-system-layout).
2. **Codewhale's per-command sandbox backend** — Seatbelt (macOS) or the
   bubblewrap wrapper (Linux) can further narrow what a child command
   may access. Codewhale does not currently provide that additional layer on
   Android.
3. **Codewhale's own gates** — workspace trust, approval prompts,
   `allow_shell`/`disallowed-tools`, and the file-tool permission system.
   These share the cross-platform application code path; their Android
   behavior still needs the real-device QA tracked below.

### Codewhale sandbox backend: none

Codewhale's existing Seatbelt and Linux bubblewrap integrations do not target
Android. Consequently, `codewhale doctor --json` reports the sandbox as
`{"available": false, "kind": null}` on Android. That status describes the
absence of an additional Codewhale child-process sandbox; it does not mean
Android or Termux provides no OS isolation.

- `get_platform_sandbox()` returns `None` on Android.
- No Linux-only bubblewrap wrapper is compiled into the Android build — it is
  `#[cfg(target_os = "linux")]`-gated and Rust
  treats `android` as a distinct target from `linux`.
- Shell commands retain Termux's Android app boundary but receive no
  Codewhale-specific filesystem narrowing. Treat every location available to
  Termux, including user-granted shared storage, as potentially available to a
  command that you approve.

### Approvals: still apply

Codewhale's approval system (interactive prompts for risky actions,
`allow_shell`, `--disallowed-tools`) is implemented at the application layer,
independently of the OS sandbox. The Android code path is present, but its
interactive behavior still needs the real-device QA tracked in #4242.

### Secret storage: file-backed

Codewhale's Termux/native build has no supported OS keyring backend (the
desktop Secret Service/dbus integration is unavailable, and Codewhale does not
yet integrate [Android Keystore](https://developer.android.com/privacy-and-security/keystore)).
It therefore falls back to **file-backed secret storage**: plaintext JSON files under
`~/.codewhale/secrets/` (Termux home directory), protected only by `0600`
file permissions — they are **not encrypted at rest**. On single-user
Termux this uses the same Unix permission mode as `~/.ssh` private keys; it is
not encrypted at rest.

- Keys saved through setup, `/provider`, or `codewhale auth set` are written to
  `~/.codewhale/config.toml` and mirrored to
  `~/.codewhale/secrets/secrets.json`. Treat both as plaintext sensitive
  files.
- `codewhale auth status --provider <id>` reports which secret backend is
  active for a provider.

### Self-update

`codewhale update` on Android requests the `codewhale-android-arm64`
release asset — never the Linux arm64
assets. The GNU libc (glibc) compatibility preflight is Linux-only and is
skipped entirely on Android (Bionic libc).

## Known limitations (first Termux release)

| Feature | Status | Notes |
|---------|--------|-------|
| Android app sandbox | ✅ inherited | Per-app UID plus Android platform protections |
| Codewhale command sandbox | ❌ unavailable | No bubblewrap/Seatbelt backend on Android |
| Codewhale keyring backend | ❌ unavailable | Falls back to file-backed secrets |
| Approvals / gates | ⚠️ implemented | Device QA pending |
| File tools | ⚠️ implemented | Device QA pending |
| Self-update | ⚠️ asset selection implemented | Published-asset and device QA pending |
| Shell execution | ⚠️ app boundary only | No Codewhale-specific narrowing; runtime QA pending |

## Related issues

- #4236 — Epic: official Termux / Android arm64 support
- #4238 — Make Android sandbox and secret-store behavior explicit
- #4240 — Build and bundle Android arm64 release assets
- #4241 — Teach updater to select Android assets on Termux
- #4242 — Run Termux runtime QA
