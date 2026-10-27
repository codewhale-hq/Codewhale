# HarmonyOS and OpenHarmony

> 阅读简体中文版：[zh_hans/HarmonyOS.md](zh_hans/HarmonyOS.md)。

This page covers Codewhale on HarmonyOS PC and OpenHarmony cross-build setups.

## Support Tier

| Target | Codewhale tier | Validation | Distribution |
| --- | --- | --- | --- |
| HarmonyOS PC, inside the Linux subsystem (融合开发引擎, openEuler) | Tier 1 Linux ARM64 runtime | Covered by the Linux ARM64 release build; no check runs inside the Linux subsystem | GitHub release binaries; npm secondary |
| Native HarmonyOS and OpenHarmony (`aarch64-unknown-linux-ohos`) | Tier 2 cross-build target | Manually dispatched SDK check for `codewhale-tui` and `codewhale-cli` | Build from source; no prebuilt release asset |

The Linux subsystem is a virtual machine running openEuler Linux, so it uses the
Linux ARM64 release. Native HarmonyOS and OpenHarmony are not Linux
distributions: their userspace uses OpenHarmony's musl-based C library and
system APIs, and Codewhale's native build for it uses the
`aarch64-unknown-linux-ohos` target. The Linux release binaries are not built or
tested for native HarmonyOS.

Tier 2 provides a source-build path and a manually dispatched SDK check; it does
not promise a check on every commit, a release binary, or full device-level
runtime testing. The OpenHarmony workflow uses the published OpenHarmony 6.1
native SDK; it deliberately fails if the SDK,
Clang, or sysroot is unavailable rather than substituting host headers or a stub
that could report false success.

## Running On HarmonyOS PC

On HarmonyOS PC, run Codewhale inside the Linux subsystem (融合开发引擎). Huawei
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
npm remains a secondary packaging route; inside the Linux subsystem it installs
the same Linux ARM64 binaries. In a native HarmonyOS terminal, Node reports the
platform as `openharmony`. There npm still installs the static Linux build as a
best effort, untested on native HarmonyOS. If the binary cannot start,
`codewhale` points you to the Linux subsystem or a source build. The install
script also prints a warning, which npm 7 and later show only with
`--foreground-scripts`. The `codewhale-tui-linux-arm64` filename is retained
only for legacy updater compatibility and is not a third command.

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

## Cross-Compiling To OpenHarmony

The repository does not check in machine-specific SDK paths. Set
`OHOS_NATIVE_SDK` to the OpenHarmony native SDK directory, the directory that
contains `llvm/bin`, `sysroot`, and `build/cmake/ohos.toolchain.cmake`.

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
the output directory. Copy it to the device and run `./codewhale --version`.
Build from a clean checkout when using the commit stamp above.

The setup scripts export Cargo's target-specific `linker`, `AR`, `CC`, `CXX`,
`CFLAGS`, `CXXFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CC_SHELL_ESCAPED_FLAGS`, and
CMake toolchain variables for `aarch64-unknown-linux-ohos`. They also point
`bindgen` at the SDK's `libclang` and sysroot so `rquickjs-sys` can generate
the OpenHarmony bindings that it does not ship pre-generated.

On Windows, `ohos-env.ps1` points Cargo at the repository's
`ohos-clang.cmd` launcher. The launcher delegates to `ohos-clang.ps1`, so the
final Rust link—not only C/C++ compilation and bindgen—always carries
`-target aarch64-linux-ohos`, the SDK sysroot, and `-D__MUSL__` while preserving
Cargo's linker arguments and exit status. The launcher re-quotes every
argument before forwarding, so an SDK path containing spaces (for example the
default `D:\DevEco Studio\...` install) keeps its `--sysroot` intact through
the final link.

## Identifying A Development Build

Current development is on `main`. A version such as `0.10.2 (dev)` means the
build did not supply a source stamp; it does not identify a development channel
or prove that the binary includes the latest fixes. The commands above embed
the first 12 characters of the source commit in `--version`. Record the full
`git rev-parse HEAD` alongside the binary. A local rebuild does not update an
older installed binary automatically; check the exact executable you copied.

Native OpenHarmony has no prebuilt release asset. On HarmonyOS PC, Linux ARM64
release binaries belong in the Linux subsystem; they are not native OHOS
binaries. GitHub build-only release-candidate artifacts, when available,
identify their source commit but do not establish native OHOS support or a
published release.

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
