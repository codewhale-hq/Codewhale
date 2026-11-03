# HarmonyOS and OpenHarmony

> 阅读简体中文版：[zh_hans/HarmonyOS.md](zh_hans/HarmonyOS.md)。

Codewhale runs on HarmonyOS PC two ways: the Linux ARM64 build inside the Linux
subsystem (融合开发引擎), or a native ARM64 build for OpenHarmony's own userspace.
Native HarmonyOS remains a Tier 2 cross-build target. The SDK check does not
establish device-level TUI, editor or model-turn qualification.

## Which One To Use

| Target | How to run it | Distribution |
| --- | --- | --- |
| HarmonyOS PC, Linux subsystem (openEuler) | The ordinary Linux ARM64 build — it is a Linux virtual machine | GitHub release binaries; `install.sh`; npm secondary |
| Native HarmonyOS and OpenHarmony | `aarch64-unknown-linux-ohos`, signed before it will execute | `codewhale-ohos-arm64.tar.gz` when a release lists it; otherwise build and sign from source |

Native HarmonyOS is not a Linux distribution: its userspace uses OpenHarmony's
musl-based C library and system APIs, so the Linux build does not run there.
Pick the subsystem if you want the shortest path; pick the native build when you
want no virtual machine between Codewhale and the device.

## Inside The Linux Subsystem

Open a terminal in the subsystem and use the official installer:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
"$HOME/.local/bin/codewhale" --version
```

`codewhale update` upgrades it. For an occupied directory or a package-managed
install, see [the fresh-directory migration](INSTALL.md#migrating-from-npm-cargo-or-another-installation).

Huawei's [support page](https://consumer.huawei.com/cn/support/content/zh-cn16091898/)
(Chinese; HarmonyOS 6.0, 6.1 and 7.0 desktops and laptops; checked 2026-10-21)
lists what the subsystem does not do:

- openEuler is the only supported distribution, and it cannot be replaced.
- No service managers: start `sshd` and friends by hand.
- The kernel is fixed, so `modinfo`, `modprobe`, `rmmod` and `insmod` fail.
- Disk images cannot be mounted. Share files through `/mnt/linux_share`, where
  new files are owned by root — `sudo git clone` into it if `git clone` refuses.
- No external USB devices or IPv6; `chsh` does not change the login shell.
- At most five snapshots; only the primary user can install the subsystem.
- Docker needs HarmonyOS 7.0 or later.

## Native Build

Set `OHOS_NATIVE_SDK` to the OpenHarmony native SDK — the directory holding
`llvm/bin`, `sysroot` and `build/cmake/ohos.toolchain.cmake`.

| Requirement | Notes |
| --- | --- |
| OpenHarmony native SDK (6.1 / API 26 used here) | DevEco Studio ships one at `<DevEco>/Contents/sdk/default/openharmony/native`; the public SDK download works too |
| Rust with the OHOS target | `rustup target add aarch64-unknown-linux-ohos` |
| A C toolchain and `cmake` | Both come from the SDK; `scripts/ohos-env.sh` puts them on `PATH` |

Linux or macOS:

```bash
export OHOS_NATIVE_SDK=/path/to/openharmony/native
. ./scripts/ohos-env.sh
rustup target add aarch64-unknown-linux-ohos
export CODEWHALE_BUILD_SHA="$(git rev-parse HEAD)"
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

Windows PowerShell:

```powershell
$env:OHOS_NATIVE_SDK="<path-to-openharmony-native-sdk>"
. .\scripts\ohos-env.ps1
rustup target add aarch64-unknown-linux-ohos
$env:CODEWHALE_BUILD_SHA = (git rev-parse HEAD).Trim()
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli
```

`ohos-env.sh` / `ohos-env.ps1` export the target's linker, `CC`/`CXX`/`AR`,
CMake toolchain and bindgen settings, and put the SDK's own `cmake` on `PATH`, so
nothing machine-specific is checked in. On Windows the final link goes through
`scripts/ohos/ohos-clang.cmd`, which keeps `-target aarch64-linux-ohos`, the
sysroot and `-D__MUSL__` even when the SDK path contains spaces.

The executable is `target/aarch64-unknown-linux-ohos/release/codewhale` (no
`.exe`, including when built on Windows); `--profile dist`, which the release
workflow uses, writes to `target/aarch64-unknown-linux-ohos/dist/` instead. A cold
release build takes about eight minutes on an Apple silicon laptop. Build from a
clean checkout when you care about the commit stamp — an incremental rebuild will
not re-stamp a binary that did not need rebuilding. Check the output:

```bash
file target/aarch64-unknown-linux-ohos/release/codewhale
# ELF 64-bit LSB pie executable, ARM aarch64, … dynamically linked, stripped

"$OHOS_NATIVE_SDK/llvm/bin/llvm-readelf" -d \
  target/aarch64-unknown-linux-ohos/release/codewhale | grep NEEDED
# libtime_service_ndk.so and libc.so — the system libraries the device provides
```

## Signing And Installing

A native HarmonyOS device executes only signed code: an unsigned ELF is refused
at `execve` with `Permission denied`, whatever its mode bits say, and `chmod +x`
does not change that. Signing is part of the build, not optional hardening. An
emulator is more permissive than a retail device, so test on the hardware you
target.

One command builds, strips a copy, self-signs and verifies:

```bash
scripts/ohos-sign-binary.sh
```

It leaves `target/aarch64-unknown-linux-ohos/release/codewhale-signed` (`--out
PATH` to move it) and fails rather than hand back a file that did not sign. By
hand:

```bash
tools="$(dirname "$OHOS_NATIVE_SDK")/toolchains/lib"
java -jar "$tools/binary-sign-tool.jar" sign -selfSign 1 \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale \
  -outFile target/aarch64-unknown-linux-ohos/release/codewhale
java -jar "$tools/binary-sign-tool.jar" display-sign \
  -inFile target/aarch64-unknown-linux-ohos/release/codewhale   # … self-sign
```

The C++ build of the same tool (`<sdk>/toolchains/lib/binary-sign-tool`, no Java
needed) takes identical arguments, and the DevBox app from AppGallery carries it
on the device. To sign while linking instead, add
`rustflags = ["-Clink-arg=-Wl,--code-sign"]` under
`[target.aarch64-unknown-linux-ohos]` in `.cargo/config.toml`; the SDK's `lld`
implements `--code-sign` and leaves it off by default.

A self-signature is enough locally: it is not bound to a device and runs on any
HarmonyOS PC, but it grants no permissions. Do not sign with the certificate
DevEco Studio generates for app signing — those binaries are refused at run time
with `Operation not permitted` (errno 129). AppGallery Connect issues debugging
certificates if you need permissions; Codewhale needs none, and no JIT.

Copy the signed file to the device and give it its exec bits before running it.
`hdc file send` lands files at `0660` and the user directory ignores `chmod`, so
the binary gains its mode another way: copy it on the device from a file that
already has the exec bits, or write it over an executable file already in place,
then confirm with `ls -l`.

Run it from an application's terminal — the system terminal app (HiShell, bundle
`com.huawei.hmos.hishell`) or the integrated terminal in CodeArts — not from
`hdc shell`, which is a debugging channel that cannot execute files written under
`/data/local/tmp`. The first launch asks for permission once.

```bash
"$HOME/codewhale" --version
# codewhale 0.10.2 (859dc2ac7ac0)
```

## The VS Code Extension In CodeArts

The [CodeWhale VS Code extension](https://github.com/HengQuWorld/CodeWhale-VSCode)
uses an HTTP webview to talk to the engine. The steps below describe the
CodeArts Agent integration route; an exact-build device run is still required
to qualify the editor workflow.

1. Put the signed engine on the `PATH` CodeArts gives its integrated terminal, or
   point `brotherwhale.enginePath` at the file.
2. In the extension panel, open the Open VSX page and install "CodeWhale"
   (publisher `hengquworld`). That is the registry CodeArts reads, so no VSIX has
   to be side-loaded.
3. Open a workspace folder and start a conversation. The extension starts the
   engine on demand and reports Ready in the status bar.

## What Works On The Device

The existing observations are from one HarmonyOS 7.0.0.110 / API 26 2in1
device with a self-signed build. They do not qualify later builds or the
workflows marked unverified below.

| Surface | State |
| --- | --- |
| `--version`, `--help`, `doctor` | Observed working |
| `auth set` / `auth status` | Local commands start; use the store smoke below to verify a complete write/read on the chosen state directory |
| The VS Code extension in CodeArts Agent | Integration route above; exact-build device workflow unverified |
| `exec_shell` | The device provides `/bin/sh`; child spawning was observed by a probe, not a model turn |
| Interactive TUI | Device validation pending; use HiShell or the CodeArts integrated terminal because `hdc shell` allocates no tty |
| Local sandbox | Absent: commands run best-effort, and the engine says so |
| Persistent PTY tools (`terminal/*`) | Not registered on this target |
| Clipboard | OSC 52 (tmux `load-buffer -w`); text only, no image paste |
| `codewhale update` | Compiled out; move a new signed binary into place instead |
| Anything needing `git` | Only if you provide `git`; snapshots and `/undo` report why |

A native HarmonyOS terminal is not a Linux shell: no `/bin/bash`, no package
manager. Codewhale reports a missing capability rather than substituting one.

## The User Directory

`/storage/Users/currentUser` is hmdfs, a distributed filesystem that reports the
logged-in user as the owner of every object and applies its own modes (`0660` for
files, `2771` for directories) instead of the requested ones. Codewhale keeps its
state and keys there like anywhere else, and the store logs one warning naming the
mode it saw: the mount's own access control is what protects those files.

Where you have a choice:

- **Strict permission checks** need a filesystem that stores owner and mode.
  Point `CODEWHALE_HOME` at the app sandbox to get them, keeping the path short:
  the daemon socket is `<CODEWHALE_HOME>/run/daemon.sock` and the kernel rejects
  paths longer than 107 bytes.

  ```sh
  CODEWHALE_HOME=/data/storage/el2/base/files/cw codewhale serve --http
  ```

  `CODEWHALE_HOME` moves the whole state root, config and sessions included.
- **Keys** can also come from the provider's environment variable, which is
  plaintext in the shell profile:

  ```bash
  export DEEPSEEK_API_KEY=sk-…
  codewhale
  ```

- **One driver per workspace.** An engine holds a lease per working tree, so a
  second one is refused with the holder's pid, client and age. That includes the
  terminal and the editor on the same folder: close one before opening the other.
  If the filesystem refuses advisory locks, the engine reports that exclusivity
  is unavailable; two drivers are not excluded on that mount.

`scripts/ohos-secrets-store-smoke.sh <directory>` checks the store end to end
against a real directory — it writes a key, reads it back through
`auth status`, and confirms `config.toml` holds no plaintext copy.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| `Required build dependency is missing. Halting build.` from `aws-lc-sys` | No `cmake` on `PATH` | Re-source `scripts/ohos-env.sh` (it adds the SDK's `cmake`), or install `cmake` |
| `Permission denied` when running the binary | Unsigned build, or no exec bits, or launched from `hdc shell` | Sign it, copy it in with its mode, run it from HiShell or CodeArts |
| `Operation not permitted` (errno 129) after signing | Signed with DevEco Studio's automatic app-signing certificate | Sign with the SDK's self-signature, or a debugging certificate from AppGallery Connect |

## Toolchain Notes

- **Compiler wrappers.** `scripts/ohos/ohos-clang.{sh,ps1}` and
  `ohos-clangxx.{sh,ps1}` read `OHOS_NATIVE_SDK` and contain no local paths;
  `chmod +x` the POSIX ones before running them directly.
- **No checked-in linker paths.** Cargo cannot expand environment variables
  inside `linker` or CMake toolchain values, so those come from `ohos-env`.
- **Dependency guard.** `./scripts/release/check-ohos-deps.sh` runs without an
  SDK and fails if a crate the target cannot carry — `portable-pty`, `nix`,
  `starlark`, `arboard`, `keyring` — re-enters the graph. The SDK check in
  `.github/workflows/ohos.yml` is the real build.
- **Identifying a build.** The commands above supply `CODEWHALE_BUILD_SHA`,
  which embeds the first 12 characters in `--version`. An unstamped local build
  reports `(dev)`; checking out `main` alone does not stamp it. A local rebuild
  does not replace an installed binary.
