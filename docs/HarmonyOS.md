# HarmonyOS and OpenHarmony

> 阅读简体中文版：[zh_hans/HarmonyOS.md](zh_hans/HarmonyOS.md)。

This page covers Codewhale on HarmonyOS PC and OpenHarmony: how to run it, how to
cross-compile it, and what the device will and will not let the result do.

## Support Tier

| Target | Codewhale tier | Validation | Distribution |
| --- | --- | --- | --- |
| HarmonyOS PC, inside the Linux subsystem (融合开发引擎, openEuler) | Tier 1 Linux ARM64 runtime | Covered by the Linux ARM64 release build; no check runs inside the Linux subsystem | GitHub release binaries; npm secondary |
| Native HarmonyOS and OpenHarmony (`aarch64-unknown-linux-ohos`) | Tier 2 cross-build target | Manually dispatched SDK check for `codewhale-tui` and `codewhale-cli`; the release build additionally verifies the self-signature it ships | `codewhale-ohos-arm64.tar.gz` when the release lists it, otherwise build from source |

The Linux subsystem is a virtual machine running openEuler Linux, so it uses the
Linux ARM64 release. Native HarmonyOS and OpenHarmony are not Linux
distributions: their userspace uses OpenHarmony's musl-based C library and
system APIs, and Codewhale's native build for it uses the
`aarch64-unknown-linux-ohos` target. The Linux release binaries are not built or
tested for native HarmonyOS.

Tier 2 promises a source-build path, a manually dispatched SDK check, and a
signed asset when a release publishes one. It does not promise a check on every
commit or device-level runtime testing in CI. The OpenHarmony workflow uses the
published OpenHarmony 6.1 native SDK and deliberately fails if the SDK, Clang, or
sysroot is unavailable rather than substituting host headers or a stub that could
report false success.

## Running On HarmonyOS PC

On HarmonyOS PC, the simplest path is the Linux subsystem (融合开发引擎). Huawei
describes it as a lightweight virtualized Linux development environment; the
current version supports openEuler only. For a new installation, open a terminal
in the Linux subsystem and use the official GitHub installer:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
"$HOME/.local/bin/codewhale" --version
```

The [published v0.9.11 release](https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.11)
includes `codewhale-linux-arm64` and `codew-linux-arm64`. See
[Linux ARM64 portability](INSTALL.md#linux-arm64-portability) for release-specific
requirements and the Cargo fallback. For an existing direct install, use
`codewhale update`. For an occupied directory or a package-managed install, use
[the fresh-directory migration](INSTALL.md#migrating-from-npm-cargo-or-another-installation).

Huawei's [Linux subsystem support page](https://consumer.huawei.com/cn/support/content/zh-cn16091898/)
(Chinese; HarmonyOS 6.0, 6.1 and 7.0 desktops and laptops; checked 2026-10-21)
documents these limits:

- openEuler is the only supported distribution.
- `systemctl` and other service managers are not supported; start services
  such as `sshd` by hand (`sudo /usr/sbin/sshd`).
- The kernel cannot be modified, so `modinfo`, `modprobe`, `rmmod` and `insmod`
  fail.
- Disk images cannot be mounted with `mount`. Exchange files through a shared
  folder, which appears at `/mnt/linux_share`. Files created there are owned by
  root, so `git clone` into it can fail for the default user; Huawei's
  workaround is `sudo git clone`.
- External USB devices, IPv6, and changing the login shell with `chsh` are not
  supported.
- At most five snapshots can be saved.
- Only the primary user (the account created at first boot) can install it.
  Huawei commercial HarmonyOS PCs also allow the personal space from HarmonyOS
  6.0.0.130.
- Docker requires HarmonyOS 7.0 or later.

## The Native Target Runs Signed Code Only

A native HarmonyOS/OpenHarmony device runs code only if it carries a valid code
signature. An unsigned ELF is refused at `execve` with `Permission denied`
whatever its mode bits say — `chmod +x` does not help. This is the platform's
mandatory code-signing mechanism, not a Codewhale bug, and it applies to `sh`,
`toybox` and third-party binaries alike.

An emulator image is more permissive than a retail device: it executes an
unsigned binary copied to `/data/local/tmp` from `hdc shell`, so a build that
works there can still be refused on hardware. Sign and test on the device you
intend to use.

Two consequences shape everything below:

1. **The binary must be signed.** `binary-sign-tool … -selfSign 1` is enough for
   local use: it proves the file is unmodified, and the result runs on any
   HarmonyOS PC (a self-signature is not bound to a device). It grants no
   permissions — for those you need a debugging certificate bound to the device
   UDID, or a binary certificate from Huawei.
2. **`hdc shell` is not a place to run it.** That shell is a debugging channel
   with its own security label (`u:r:sh:s0`), and it cannot execute files written
   under `/data/local/tmp` (they carry `u:object_r:data_local_tmp:s0`). A
   correctly signed binary reports `Permission denied` there even though the file
   is `-rwxr-xr-x`. Run it from the system terminal application (HiShell, bundle
   `com.huawei.hmos.hishell`) instead; the first launch asks for permission once.

## Cross-Compiling To OpenHarmony

The repository does not check in machine-specific SDK paths. Set
`OHOS_NATIVE_SDK` to the OpenHarmony native SDK directory, the directory that
contains `llvm/bin`, `sysroot`, and `build/cmake/ohos.toolchain.cmake`.

Prerequisites:

| Requirement | Notes |
| --- | --- |
| OpenHarmony native SDK (6.1 / API 26 used here) | DevEco Studio ships one at `<DevEco>/Contents/sdk/default/openharmony/native`; the public SDK download and the `setup-ohos-sdk` action CI uses also work |
| Rust with the OHOS target | `rustup target add aarch64-unknown-linux-ohos` — a Tier 2 target with a prebuilt standard library |
| A `cmake` and build tool | Provided by the SDK under `build-tools/cmake/bin`; `scripts/ohos-env.sh` puts them on `PATH` |

On Windows PowerShell:

```powershell
$env:OHOS_NATIVE_SDK="<path-to-openharmony-native-sdk>"
. .\scripts\ohos-env.ps1
rustup target add aarch64-unknown-linux-ohos
$env:CODEWHALE_BUILD_SHA = (git rev-parse HEAD).Trim()
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

On Linux or macOS:

```bash
export OHOS_NATIVE_SDK=/path/to/openharmony/native
. ./scripts/ohos-env.sh
rustup target add aarch64-unknown-linux-ohos
export CODEWHALE_BUILD_SHA="$(git rev-parse HEAD)"
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

The executable is `target/aarch64-unknown-linux-ohos/release/codewhale` (no
`.exe`, including when built on Windows), unless `CARGO_TARGET_DIR` overrides
the output directory. A cold release build takes roughly eight minutes on an
Apple silicon laptop. For the shipping profile use `--profile dist` instead of
`--release`: that is what the release workflow builds, with fat LTO.

Build from a clean checkout when using the commit stamp above. A version string
such as `0.10.2 (859dc2ac7ac0)` comes from `CODEWHALE_BUILD_SHA`, and an
incremental rebuild will not re-stamp a binary that did not need rebuilding.

Verify the output before copying it anywhere:

```bash
file target/aarch64-unknown-linux-ohos/release/codewhale
# ELF 64-bit LSB pie executable, ARM aarch64, … dynamically linked, stripped

"$OHOS_NATIVE_SDK/llvm/bin/llvm-readelf" -d \
  target/aarch64-unknown-linux-ohos/release/codewhale | grep NEEDED
# libtime_service_ndk.so and libc.so — the system libraries the device provides
```

The setup scripts export Cargo's target-specific `linker`, `AR`, `CC`, `CXX`,
`CFLAGS`, `CXXFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CC_SHELL_ESCAPED_FLAGS`, and
CMake toolchain variables for `aarch64-unknown-linux-ohos`. They also point
`bindgen` at the SDK's `libclang` and sysroot so `rquickjs-sys` can generate the
OpenHarmony bindings that it does not ship pre-generated, and they put the SDK's
own `cmake` on `PATH` so crates with a CMake build script (`aws-lc-sys`, which
the TLS stack pulls in) do not depend on the host having one.

On Windows, `ohos-env.ps1` points Cargo at the repository's
`ohos-clang.cmd` launcher. The launcher delegates to `ohos-clang.ps1`, so the
final Rust link—not only C/C++ compilation and bindgen—always carries
`-target aarch64-linux-ohos`, the SDK sysroot, and `-D__MUSL__` while preserving
Cargo's linker arguments and exit status. The launcher re-quotes every
argument before forwarding, so an SDK path containing spaces (for example the
default `D:\DevEco Studio\...` install) keeps its `--sysroot` intact through
the final link.

## Signing The Build

Sign the binary with the SDK's `binary-sign-tool` before running it on a device:

```bash
SDK_TOOLCHAINS="$(dirname "$OHOS_NATIVE_SDK")/toolchains/lib"
java -jar "$SDK_TOOLCHAINS/binary-sign-tool.jar" sign \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale \
  -outFile target/aarch64-unknown-linux-ohos/release/codewhale \
  -selfSign 1
```

The C++ build of the same tool (`<sdk>/toolchains/lib/binary-sign-tool`, no Java
required) takes identical arguments, and HarmonyOS PC users can install the
DevBox app from AppGallery to get the command on the device itself. Confirm the
signature landed:

```bash
java -jar "$SDK_TOOLCHAINS/binary-sign-tool.jar" display-sign \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale
# … code signature is self-sign
```

To sign at link time instead, add the linker flag — the SDK's `lld` implements
`--code-sign` and does not enable it by default:

```toml
# .cargo/config.toml
[target.aarch64-unknown-linux-ohos]
rustflags = ["-Clink-arg=-Wl,--code-sign"]
```

Two limits worth knowing:

- **Self-signing grants no permissions.** `display-sign` reports
  `permission is not found` regardless of what a `module.json` declares. Do not
  reuse the certificate DevEco Studio generates for app signing: binaries signed
  with it are refused at run time with `Operation not permitted` (errno 129), so
  request a debugging certificate in AppGallery Connect instead.
- **Self-signing grants no JIT.** Anything that maps executable pages at run time
  (an emulator, a JavaScript engine with a JIT) has to be packaged as a public
  HNP inside a HAP. Codewhale does not need this.

Copy the signed file to the device and run it from the system terminal:

```bash
"$HOME/codewhale" --version
# codewhale 0.10.2 (859dc2ac7ac0)
```

## What Works On The Device, And What Does Not

The two columns separate what has been observed on a device from what the
target's `cfg` gates make unreachable by construction. The observations are from
one HarmonyOS 7.0.0.110 / API 26 2in1 device with a self-signed build; treat them
as a starting point, not a compatibility matrix.

| Surface | State | Basis |
| --- | --- | --- |
| `--version`, `--help`, `doctor` | Works | Observed |
| Subcommands that only read local state (`auth status`, `auth set`'s prompt) | Run | Observed |
| `exec_shell` (`/bin/sh -c …` through the non-PTY process path) | Blocked on the device providing `/bin/sh`, which it does; a probe spawning children on the same API succeeded | Observed by probe, not through a model turn |
| Interactive TUI | Not verified on a device | `hdc shell` allocates no tty, and no run has yet drawn the TUI from HiShell |
| Local sandbox | Absent (`sandbox not available (commands run best-effort)`) | Code: `all(target_os = "linux", not(target_env = "ohos"))` |
| Persistent PTY tools (`terminal/*`) | Not registered | Code: `portable-pty` excluded from the graph |
| Clipboard | OSC 52 (or tmux `load-buffer -w`); no image paste | Code |
| `codewhale update` and self-update | Compiled out | Code |
| Anything needing `git` | Absent unless the user provides it; snapshots and `/undo` report why | Code plus one device warning |

A native HarmonyOS terminal is not a Linux shell: no `/bin/bash`, no `git`, no
package manager. Codewhale degrades rather than pretends — a missing capability is
reported, not silently substituted.

### Credential storage on the user directory

The 2in1 user directory (`/storage/Users/currentUser`) is a distributed
filesystem (hmdfs) that does not behave like a local one:

- it reports the logged-in user's uid as the owner of every object, whatever uid
  created it (a fixed `20001006` on the device this was measured on),
- it applies a base mode of `0660` to new files and `2771` to new directories
  instead of the requested mode,
- and because the caller is therefore never the reported owner, a later `chmod`
  on such an object answers `EPERM` — it is not the caller's file to change,
  whatever the mode says.

The file-backed secret store wants an owner-only file and a cross-process lock,
so `codewhale auth set` used to fail there with `insecure permissions`. The store
now decides from evidence instead of from the mode alone: it asks the
filesystem to narrow a probe file and reads the mode back, and only then treats
the mode as uninformative. Pointing `CODEWHALE_HOME` at a directory on `/data`
(hmfs, whose filesystem stores owner and mode normally) restores the strict
check; supplying the key through the provider's environment variable also works
and is read when the store cannot answer:

```bash
export DEEPSEEK_API_KEY=sk-…
codewhale
```

The environment variable is plaintext in the shell profile; choose it knowing
that. A mode read back in a shell does not answer whether the store can work
there — on this mount the file is reported at `0640` and the `chmod` that would
narrow it is refused because the shell is not the reported owner. What the store
does instead is ask the filesystem and then say which answer it got: run
`codewhale auth set` and read the warning it logs for the store path.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| `Required build dependency is missing. Halting build.` from `aws-lc-sys` | No `cmake` on `PATH` | Re-source `scripts/ohos-env.sh` (it adds the SDK's `cmake`), or install `cmake` |
| `function sha256_hex is never used` while checking `codewhale-cli` | A module only the updater uses was compiled on a target without the updater | Fixed in the tree, by gating `mod hashing` exactly as `mod update` is gated; an older checkout needs the same `#[cfg(not(target_env = "ohos"))]` on both |
| `Permission denied` running the binary from `hdc shell` | The debugging shell cannot execute files under `/data/local/tmp`, and only signed code runs at all | Sign it, then run it from HiShell |
| `Operation not permitted` (errno 129) after signing | Signed with DevEco Studio's automatic app-signing certificate | Request a debugging certificate in AppGallery Connect and sign with it |
| `Secret storage write failed … insecure permissions` | The mount cannot hold `0600` | See “Credential storage on the user directory” |
| Sign-in fails with `opening filesystem root` | Credential-directory preparation, before browser sign-in | Report with `--version`, the full build commit, device/OS and terminal version; do not make system directories writable |

## Identifying A Development Build

Current development is on `main`. A version such as `0.10.2 (dev)` means the
build did not supply a source stamp; it does not identify a development channel
or prove that the binary includes the latest fixes. The commands above embed the
first 12 characters of the source commit in `--version`. Record the full
`git rev-parse HEAD` alongside the binary. A local rebuild does not update an
older installed binary automatically; check the exact executable you copied.

A GitHub build-only release-candidate artifact identifies its source commit but
does not establish device-level support.

## Sign-In Filesystem Errors

`ChatGPT sign-in failed: opening filesystem root` occurs while preparing local
credential storage, before browser sign-in or callback setup. It does not by
itself identify a read-only `/tmp`. Current source uses traversal-only handles
for Linux/OpenHarmony ancestors and keeps a normal, protected handle for the
private credential directory. Root-open failures include the OS error.

For a report, include the exact `--version`, full build commit, device/OS and
terminal version, and complete error text. Keep tokens, authorization URLs and
credential files out of the report. Do not make system directories writable or
disable credential checks. A successful build or Linux permission reproduction
still needs a sign-in check on the affected device.

## Workspace Exclusivity On The User Directory

A driving Engine takes one lease per working tree, so a second driver is refused
instead of two Engines writing the same tree, snapshot side-repo and git index.
The lease lives under the state root — `$CODEWHALE_HOME/workspace-leases`, else
`~/.codewhale/workspace-leases` — which on a 2in1 sits on hmdfs.

hmdfs cannot prove that directory is current-user-owned: every object there is
reported as belonging to the logged-in user, whatever uid created it, so the
calling process is never the reported owner. That is a limit on the *proof*, not
on the lock. The two are separate capabilities, and the lease is admitted with
the ownership guard recorded as unverifiable and still taken, so exclusivity
holds: two engine scopes inside one process share a single slot as before, and a
second app that opens the same workspace is refused with the holder's pid,
client, store and age.

Only a filesystem that refuses advisory locks themselves — `EPERM`, `EACCES`,
`ENOTSUP`, `EINVAL` or `ENOSYS` from `flock`/`fcntl`, which some network and
FUSE mounts answer — leaves the Engine without exclusivity, and it says so once
per admitted workspace scope.

Earlier builds turned the same condition into a failed startup:

```
Engine exited before becoming ready: error: workspace drive lease check failed
for /storage/Users/currentUser/<workspace>: Codewhale credentials directory must
be owned by the current user
```

Neither half of that sentence named the real directory (the `workspace-leases`
state root) or the real cause (hmdfs synthesising ownership). The "credentials"
wording is inherited from the private-directory mechanism, which originated in
xAI OAuth storage.

For a real cross-client guarantee on HarmonyOS PC, put the Engine's private state
on the app sandbox, whose filesystem stores owner and mode normally:

```sh
CODEWHALE_HOME=/data/storage/el2/base/files/cw codewhale serve --http
```

`CODEWHALE_HOME` moves the whole state root, config and sessions included, so
choose whether that is what you want. The short path matters: the daemon socket
is `<CODEWHALE_HOME>/run/daemon.sock` and the kernel rejects a socket path longer
than 107 bytes.

Do not try to fix the refusal with `chmod`, and do not make the user directory
group-writable: the mount ignores the mode, and the next run reaches the same
conclusion.

## Compiler Wrappers

For ad-hoc compiler calls, use the wrappers in `scripts/ohos/`. They read the same
`OHOS_NATIVE_SDK` variable and do not contain local paths.

Windows PowerShell:

```powershell
.\scripts\ohos\ohos-clang.ps1 --version
.\scripts\ohos\ohos-clangxx.ps1 --version
```

Linux or macOS:

```bash
sh ./scripts/ohos/ohos-clang.sh --version
sh ./scripts/ohos/ohos-clangxx.sh --version
```

If you want to run the POSIX wrappers directly as `./scripts/ohos/ohos-clang.sh`, make them
executable first:

```bash
chmod +x ./scripts/ohos/ohos-clang.sh ./scripts/ohos/ohos-clangxx.sh
```

## Linker And Toolchain Paths

The repository does not check in a Cargo linker path or CMake toolchain path.
Cargo cannot expand environment variables inside `linker` or CMake toolchain
path values, so those values are exported by `scripts/ohos-env.ps1` and
`scripts/ohos-env.sh` instead.

## Dependency Guard

Release prep runs a no-SDK dependency check:

```bash
./scripts/release/check-ohos-deps.sh
```

The guard asserts the Windows final-link wrapper contract, proves that OHOS
activates the `rquickjs-sys` bindgen feature, resolves the `codewhale-tui`
dependency graph for `aarch64-unknown-linux-ohos`, and fails if unsupported
host/UI crates re-enter that graph: `nix` 0.28/0.29, `portable-pty`, `starlark`,
`arboard`, or `keyring`. This no-SDK check does not replace a real SDK/sysroot
build, but it catches the known linker, bindgen, `starlark -> rustyline -> nix`,
and PTY/keyring regressions before release.

Because `portable-pty` is intentionally absent from the OpenHarmony graph, the
persistent `terminal/*` PTY tools are not registered on that target. The
ordinary `exec_shell` tools remain available through their non-PTY process
implementation.

Linux-only sandbox implementations (bubblewrap and `prctl` process
hardening) are compiled only for
`all(target_os = "linux", not(target_env = "ohos"))`. OpenHarmony therefore
reports no local OS sandbox instead of probing Linux kernel paths or syscalls it
does not support. External OpenSandbox execution remains separately available
when configured.

Native desktop clipboard libraries and Wayland helpers are also excluded from
the OpenHarmony graph. Text copy degrades to the terminal-client path (OSC 52,
or tmux `load-buffer -w` when inside tmux); paste is supplied by the terminal as
normal/bracketed input. Image clipboard reads are unavailable on this target.
If the terminal cannot accept OSC 52, copy returns a clear "Clipboard
unavailable" error rather than panicking or claiming success.
