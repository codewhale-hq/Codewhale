# Installing Codewhale

> 阅读简体中文版：[zh_hans/INSTALL.md](zh_hans/INSTALL.md)

Codewhale is an open-source coding agent that runs in your terminal. You give
it a task ("fix the failing test", "add a CLI flag"). It reads your repository,
edits files and runs commands. In the default **Ask** posture it applies file
edits inside the workspace immediately (and shows you the diff), but asks before
running shell commands, so commit or stash anything you care about first. It
works with many model providers. **DeepSeek** is the default.

The command is `codewhale`. `codew` is a shorter alias for the same program.

The installation paths were first checked with **v0.10.0** (released 2026-09-22) on a
fresh **Ubuntu 24.04 x86_64** machine. The dated
[install receipts](https://github.com/codewhale-hq/Codewhale/blob/37ecdfcc49bc68a9b0d058b97c3946e62c34bd31/docs/install-report/v0.10.0-2026-09-23/RECEIPTS.md) record the commands and output checked at that time. Steps that
could not be run on that machine are marked **(untested on this VM: reason)**.
macOS, Windows and Android are out of scope, apart from a few notes. A second
pass re-ran the installer, manual-download, archive and npm paths, the no-key
checks and zsh completion on **macOS 26.1 (Apple silicon)**; see
[macOS notes](#macos-notes). Steps that need a model call were not re-run
there.

Install commands that use `latest` resolve to the latest **published** GitHub
Release or package. Between releases, `main` may already describe the next
version (for example a v0.10.2 source candidate before its tag). A
prebuilt candidate is available through the official installer only after its
tag, checksums and release assets exist. Contributors can build `main` now.

## One account for your provider keys

[Register](https://app.codewhale.net/register) or [sign in](https://app.codewhale.net/login)
to keep your model providers' API keys in one place. Add, replace or remove
keys in your account's [Providers settings](https://app.codewhale.net/providers),
or use the terminal commands below. On another device, sign in and select the
Codewhale model route to use the same account keys without pasting them again.

```bash
codewhale login
codewhale account keys set deepseek  # asks for the key without echoing it
codewhale account keys list         # shows status, never the key itself
codewhale --provider codewhale      # use your account's model route
```

The account setup here describes the 0.10.1 source candidate. Signing in does
not upload existing local keys. To copy one deliberately, use
`codewhale account keys set deepseek --from-local`. Account keys stay on the
service; they are used for account-routed requests, not downloaded into each
device's local key store. You can keep using local keys or a local model
without an account. See [account key settings](CONFIGURATION.md#account-provider-keys)
for updating keys and switching between these options.

## Test and contribute to current source

`latest` selects the latest published release. To test unreleased repairs,
build canonical GitHub `main`, or CNB `main` after verifying its commit matches.
The source version number alone does not establish a published package.

```bash
git clone --branch main https://github.com/codewhale-hq/CodeWhale.git
cd CodeWhale
cargo build --release --locked -p codewhale-cli
target/release/codewhale --version
```

See the Cargo section for build prerequisites; use the repository's stable Rust
selection (declared minimum 1.89). Windows uses `target/release/codewhale.exe`
and needs MSVC. A development build is separate from a signed release package.
For mainland-China downloads and source validation, see [CNB mirror](CNB_MIRROR.md).
The [Tencent Lighthouse bootstrap](../scripts/tencent-lighthouse/README.md) is
bilingual. An npm registry mirror serves the wrapper; native binary downloads
still need an available release asset source.

---

## 60-second quickstart (Linux or macOS)

```bash
# 1. Install. Downloads two checksum-verified binaries into ~/.local/bin (no sudo).
curl -fsSL https://codewhale.net/install.sh | sh

# 2. Make sure ~/.local/bin is on your PATH, now and in future terminals.
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc    # zsh: use ~/.zshrc
export PATH="$HOME/.local/bin:$PATH"
codewhale --version          # -> codewhale 0.10.0 (1be1a703b975)

# 3. Give it a DeepSeek API key (from https://platform.deepseek.com/api_keys).
codewhale auth set --provider deepseek    # prompts for the key; nothing is echoed
codewhale auth status --provider deepseek # "active source: secret store"

# 4. Run your first task inside a git repository.
cd ~/your-project
codewhale
```

In the TUI, type something concrete:

```text
create a Python file primes.py that prints the first 10 primes, run it, and show me the output
```

Codewhale writes the file, shows you a diff, then asks **APPROVAL: bash
python3 primes.py – Do you want to proceed?** Press `y` to allow it once. It
runs the command and reports the output. Press `Ctrl-D` (with an empty input
box) to quit. It prints the command to resume the session later.

> **If nothing happens after you send your first message,** you have no key
> configured. v0.10.0 doesn't warn you in that case. Press **F3**. If DeepSeek
> shows `missing key`, press Enter, paste the key, then pick a model and
> confirm.

---

## Contents

1. [Before you start](#1-before-you-start)
2. [Install: recommended installer](#2-recommended-installer-curl--sh)
3. [Install: manual download from GitHub Releases](#3-manual-download-from-github-releases)
4. [Install: npm](#4-npm)
5. [Install: Cargo / build from source](#5-cargo-and-building-from-source)
6. [Install: Homebrew (Linux) and Nix](#6-homebrew-on-linux-and-nix)
7. [Updating and rolling back](#7-updating-and-rolling-back)
8. [API keys and providers](#8-api-keys-and-providers)
9. [Shell completions](#9-shell-completions)
10. [Running it: TUI, headless, resume](#10-running-it)
11. [Terminal notes (Ghostty and others)](#11-terminal-notes)
12. [Uninstalling and what Codewhale leaves behind](#12-uninstalling)
13. [Troubleshooting](#13-troubleshooting)
14. [Appendix: other platforms (not re-tested in this revision)](#appendix-other-platforms-not-re-tested-in-this-revision)

---

## 1. Before you start

| You need | Why |
|---|---|
| Linux x86_64 or arm64, or macOS | Prebuilt binaries exist for these. The Linux binaries are **static** (musl), so they have no glibc or libdbus dependency and run on any distro. |
| `curl` (or `wget`) and `sha256sum` (or `shasum`) | The installer uses them to download and verify. |
| A model provider key, e.g. [DeepSeek](https://platform.deepseek.com/api_keys) | Codewhale does nothing useful without a model. |
| `git` (recommended) | Codewhale works best inside a git repository. |
| Optional: Python 3, Node.js 20+ | If present, Codewhale enables its Python and JS execution tools (`codewhale doctor` lists them). |

**Which install path?**

* **Most people:** the [recommended installer](#2-recommended-installer-curl--sh).
  It's the fastest (about 6 s here), verifies checksums, and supports
  `codewhale update`.
* **Air-gapped or security-reviewed machines:**
  [manual download](#3-manual-download-from-github-releases).
* **You already manage CLI tools with npm:** [npm](#4-npm).
* **No prebuilt binary for your platform, or you want to build it yourself:**
  [Cargo](#5-cargo-and-building-from-source).

Pick **one**. Several installs on one machine end up fighting over PATH (see
[Troubleshooting](#13-troubleshooting)).

**Privacy note:** Codewhale sends aggregate usage counts (PostHog) **by
default**. To turn this off permanently:
`codewhale config set telemetry false`, or export `CODEWHALE_TELEMETRY=0`,
which always wins. The TUI also checks GitHub for updates at startup
(`[update] check_for_updates` in `~/.codewhale/config.toml`).

---

<a id="recommended-official-github-releases"></a>

## 2. Recommended installer (`curl | sh`)

**Prerequisites:** curl, `sha256sum` (Linux) or the built-in `shasum` (macOS),
a writable home directory. No sudo, no Node, no Rust.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

What it does (verified):

* It detects your platform (`linux-x64`, `linux-arm64`, `macos-x64`,
  `macos-arm64`). It refuses Android/Termux and riscv64 with a clear message.
* It downloads `codewhale-<platform>`, `codew-<platform>` and
  `codewhale-artifacts-sha256.txt` from the latest GitHub Release, and verifies
  both binaries against the manifest. If either doesn't match, it stops before
  installing anything (`codewhale install: checksum mismatch for …`).
* It installs `~/.local/bin/codewhale` and `~/.local/bin/codew`: two identical
  78 MB files.
* It **never uses sudo and never edits your shell profile.** It refuses to
  install into system or package-manager directories (`/usr/bin`,
  `~/.cargo/bin`, Homebrew, `node_modules`, `/nix/store`…) and refuses to
  overwrite a *different* existing `codewhale`.

Expected output:

```
Installing Codewhale for linux-x64
Release assets: https://github.com/codewhale-hq/CodeWhale/releases/latest/download
Install dir: /home/you/.local/bin
Checksums verified
Installed checksummed release commands:
  /home/you/.local/bin/codewhale
  /home/you/.local/bin/codew
…
PATH selects no codewhale command; this install is /home/you/.local/bin/codewhale
…
Put /home/you/.local/bin first on PATH in future shells (run once; this installer does not edit shell profiles):
  echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
Then run: . ~/.bashrc   (or open a new terminal)
…
```

### macOS notes

Re-checked on macOS 26.1, Apple silicon (`macos-arm64`), with a fresh `HOME`:

* The installer printed `Installing Codewhale for macos-arm64`, verified
  checksums with the system tools, and installed `codewhale` and `codew`
  (64 MiB each, Mach-O arm64) in 4.3 s. Both report
  `codewhale 0.10.0 (1be1a703b975)`. They ran without a Gatekeeper prompt.
* When Node isn't on `PATH`, it also prints `Computer Use is included and needs
  Node.js 20 or newer on PATH.` The core TUI works without Node, but Computer Use
  and the JavaScript execution tool (`js_execution`) stay unavailable until Node
  is on `PATH`.
* `codewhale doctor` behaves as on Linux (exit 0, `All checks complete!` with no
  key, file-based secret store under `~/.codewhale/secrets/`), except that it
  reports `✓ sandbox available: macos-seatbelt`.

### Put it on your PATH

If the installer says `PATH selects no codewhale command`, `~/.local/bin` isn't
on your PATH **in this shell**. The `codewhale.net/install.sh` installer then
prints the matching line from the block below for your `$SHELL` (zsh, bash,
fish, or a POSIX `sh`; for any other shell, or a directory name with quotes,
`$`, backticks or backslashes, it tells you to add the directory yourself). It
never edits a shell profile itself. The `install.sh` inside a release archive
prints only the current-shell `export` line. On Ubuntu and Debian, `~/.profile` adds
`~/.local/bin`, but only if the directory existed when you *logged in*. So:

* a new SSH or login shell picks it up automatically;
* a new terminal **window** on a desktop (GNOME Terminal, Ghostty, …) usually
  doesn't, until you log out and back in. I hit
  `bash: codewhale: command not found` in Ghostty right after installing.

Fix it once:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc   # bash (macOS login bash: ~/.bash_profile, or ~/.profile if only that exists)
# echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc  # zsh
# fish_add_path ~/.local/bin                               # fish (untested on this VM)
export PATH="$HOME/.local/bin:$PATH"; hash -r
command -v codewhale codew
```

### Options

```bash
# Choose the directory (must be absolute; created if missing)
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_INSTALL_DIR="$HOME/.local/codewhale/bin" sh
# Install a specific release
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION=v0.9.13 sh
# Show help
curl -fsSL https://codewhale.net/install.sh | sh -s -- --help
```

### Verify

```bash
codewhale --version     # codewhale 0.10.0 (1be1a703b975)
codew --version         # same
codewhale doctor        # diagnostics; see the note in §8 about what it does NOT check
```

### Re-running the installer

* Same version already installed: harmless. It prints
  `Already installed: …` and exits 0.
* Different version already installed: it **refuses**
  (`codewhale install: refusing to replace existing …/codewhale`), exits 1 and
  changes nothing. It downloads ~160 MB before refusing. Use
  [`codewhale update`](#7-updating-and-rolling-back) instead.

### Upgrade / uninstall

* Upgrade: `codewhale update` (see §7).
* Uninstall: `rm ~/.local/bin/codewhale ~/.local/bin/codew`, then see §12 for
  data.

---

## 3. Manual download from GitHub Releases

Use this when you want to see and verify every byte yourself. Releases:
<https://github.com/codewhale-hq/CodeWhale/releases>. Each platform has **bare
binaries** (`codewhale-linux-x64`, `codew-linux-x64`, …) and an **archive**
(`codewhale-linux-x64.tar.gz`) that holds the same two binaries plus an
`install.sh`.

### 3a. Bare binaries

```bash
mkdir -p ~/codewhale-dl && cd ~/codewhale-dl
base=https://github.com/codewhale-hq/CodeWhale/releases/latest/download
curl -fsSLO "$base/codewhale-linux-x64"          # use linux-arm64 on ARM
curl -fsSLO "$base/codew-linux-x64"
curl -fsSLO "$base/codewhale-artifacts-sha256.txt"
sha256sum -c codewhale-artifacts-sha256.txt --ignore-missing
#   codew-linux-x64: OK
#   codewhale-linux-x64: OK
mkdir -p ~/.local/bin
install -m 755 codewhale-linux-x64 ~/.local/bin/codewhale
install -m 755 codew-linux-x64     ~/.local/bin/codew
```

Then [put `~/.local/bin` on PATH](#put-it-on-your-path) and run
`codewhale --version`. On macOS the assets are `codewhale-macos-arm64` and
`codew-macos-arm64` (`-macos-x64` on Intel), and the built-in `shasum` verifies
them (tested on macOS 26.1, Apple silicon):

```bash
/usr/bin/shasum -a 256 -c codewhale-artifacts-sha256.txt --ignore-missing
#   codew-macos-arm64: OK
#   codewhale-macos-arm64: OK
```

The `codewhale-macos-arm64.tar.gz` archive verifies the same way against
`codewhale-bundles-sha256.txt`, and its `./install.sh` installs into
`~/.local/bin` (tested).

To pin a release, replace `latest/download` with `download/vX.Y.Z`, and take
the manifest from the same tag.

### 3b. Archive

```bash
cd "$(mktemp -d)"
base=https://github.com/codewhale-hq/CodeWhale/releases/latest/download
curl -fsSLO "$base/codewhale-linux-x64.tar.gz"
curl -fsSLO "$base/codewhale-bundles-sha256.txt"     # note: *bundles*, not *artifacts*
sha256sum -c codewhale-bundles-sha256.txt --ignore-missing
#   codewhale-linux-x64.tar.gz: OK
tar -xzf codewhale-linux-x64.tar.gz
cd codewhale-linux-x64 && ./install.sh               # -> ~/.local/bin; PREFIX=/some/dir ./install.sh -> /some/dir/bin
```

The archive's `install.sh` behaves like the website installer: no sudo, it
leaves differing existing files alone, and it prints the same PATH hint.

**Upgrade:** `codewhale update` works for both 3a and 3b, because they're
"direct binary" installs. **Uninstall:** delete the two files (see §12).

---

## 4. npm

**Prerequisites:** Node.js 18+ and npm, with a **global prefix you can write
to**. npm installs the registry's latest published version, never an
unpublished source candidate.

```bash
npm install -g codewhale
codewhale --version
```

The package is a small wrapper. Its `postinstall` step downloads the same
`codewhale`/`codew` release binaries, checks them against the release's SHA-256
manifest, and links `codewhale` and `codew` into npm's global `bin`. The whole
thing took 6 s here.

**Windows npm sessions:** Node remains the native program's launcher for the
whole session. A process-name kill such as `taskkill /IM node.exe` or
`Get-Process node | Stop-Process -Force` can interrupt this and other npm
Codewhale sessions and prevent normal terminal cleanup. Stop only the server
PID you started or the process owning its port, or use Codewhale's task
cancellation. The Windows
native archive/installer avoids this npm-parent dependency; this does not
remove Node requirements for optional JavaScript tools. Codewhale's Windows
shell safety floor holds recognized image-wide Node kills even in Full Access.
An external hard kill or an arbitrary program that terminates the launcher
cannot be made graceful by this shell-command check.

### If you get `EACCES: permission denied`

That means Node is installed system-wide (apt, `/usr/local`, `/opt`), and your
user can't write to its global prefix:

```
npm error code EACCES
npm error Error: EACCES: permission denied, mkdir '/opt/node22/lib/node_modules/codewhale'
```

**Don't use `sudo npm`.** Either use a per-user Node (nvm, fnm, volta), or
point npm at a directory you own. I tested the second option:

```bash
npm config set prefix "$HOME/.npm-global"
echo 'export PATH="$HOME/.npm-global/bin:$PATH"' >> ~/.bashrc
export PATH="$HOME/.npm-global/bin:$PATH"
npm install -g codewhale
command -v codewhale codew     # ~/.npm-global/bin/codewhale, ~/.npm-global/bin/codew
```

### Notes

* npm hides the download progress. Add `--foreground-scripts` to see it
  (`codewhale: selected GitHub Releases for v0.10.0 … done.`). The chosen
  source is also written to
  `$(npm prefix -g)/lib/node_modules/codewhale/bin/downloads/codewhale.source`.
* The package uses 157 MB on disk.
* On macOS 26.1 (Apple silicon, Homebrew Node 25) an install into a user-owned
  prefix (`npm install -g --prefix <dir> codewhale`) took 3 s and linked
  `codewhale` and `codew`, both `codewhale 0.10.0 (1be1a703b975)`.
* **Upgrade:** `npm install -g codewhale@latest`. `codewhale update` refuses
  on npm installs. It prints migration instructions and exits 1 with
  `error: The package-managed executable was not changed.`
* **Specific version:** `npm install -g codewhale@0.9.13`.
* **Uninstall:** `npm uninstall -g codewhale`. This removes only the program,
  not your data (§12).

---

<a id="4-install-via-cargo-any-tier-1-rust-target"></a><a id="7-build-from-source"></a>

## 5. Cargo and building from source

Use this if there's no prebuilt binary for your platform, or you want to
compile it yourself. One Cargo package is required:
`codewhale-cli` installs the `codewhale` command. npm and prebuilt releases also
expose `codew` as a convenience name for the same compiled runtime; Cargo does
not create that alias, so add `alias codew=codewhale` to your shell rc if you
want the short name.

### Prerequisites (Debian/Ubuntu)

```bash
sudo apt-get install -y build-essential pkg-config libdbus-1-dev git
# Rust via rustup (the distro's cargo is too old for this edition-2024 workspace)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version            # the workspace declares rust-version = 1.89
```

`libdbus-1-dev` **is required**. Without it the build fails after about a
minute with:

```
error: failed to run custom build command for `libdbus-sys v0.2.7`
  The system library `dbus-1` required by crate `libdbus-sys` was not found.
```

Fedora/RHEL: `sudo dnf install -y gcc make pkgconf-pkg-config dbus-devel`
**(untested on this VM: Ubuntu only)**.

### 5a. From crates.io

```bash
cargo install codewhale-cli --locked
codewhale --version
```

Tested result: **works**, with current stable Rust (1.98.1).
* It took **25 min 31 s** on 4 vCPU and 15 GB RAM, and pulled about 470 MB
  into `~/.cargo/registry`.
* It installs one 122 MB file, `~/.cargo/bin/codewhale`. That's a normal
  glibc-linked binary, and it needs `libdbus-1` at runtime.
* `codewhale --version` prints `codewhale 0.10.0`, with no commit hash.
* Headless and TUI smoke tests passed.

> **v0.10.0 declared "Rust 1.88+", which was wrong.** The workspace now
> declares 1.89, the version CI's MSRV job builds. With 1.88.0 the v0.10.0
> install fails in seconds:
> `rustc 1.88.0 is not supported by the following package: serde-saphyr@1.3.0 requires rustc 1.89`.
> Use current stable (`rustup update stable`).

### 5b. From a git checkout

```bash
git clone --depth 1 --branch v0.10.0 https://github.com/codewhale-hq/CodeWhale.git
cd CodeWhale
cargo install --path crates/cli --locked      # installs ~/.cargo/bin/codewhale
```

Things to know (observed):

* The repo contains `rust-toolchain.toml` (`channel = "stable"`). The first
  `cargo` command inside the checkout **silently downloads the latest stable
  toolchain** (about 250 MB), whatever your default is.
* The workspace treats every compiler warning as an error. With Rust 1.89
  the build **fails** after about 11 minutes with 8
  `error: this lint expectation is unfulfilled` errors in `codewhale-tui`. Use
  the stable toolchain the repo selects. Don't pass an older `+toolchain`.
* The workspace release profile uses thin LTO. On a 4-vCPU / 15 GB VM, the
  `codewhale-tui` crate alone compiled for more than an hour, peaking at
  6–8 GB of RAM. Budget 16 GB or more, and expect this to be the slowest install
  path by far. `target/` grew past 1.2 GB.

Tested result: **works** on the repo-selected stable Rust (1.98.1).
* `Finished release profile … in 83m 12s`. A Nix build competed for CPU and
  memory for most of that time, so treat it as an upper bound.
* `target/` ended at **3.1 GB**. Delete it afterwards with `cargo clean`.
* The binary reports `codewhale 0.10.0 (dev)`, and `exec` worked.
* Cargo prints `warning: default toolchain implicitly overridden with
  stable-x86_64-unknown-linux-gnu by rustup toolchain file`, which is harmless.
* **Cargo builds provide no `codew`.**

**Upgrade:** re-run the same `cargo install … --force` (for crates.io, you can
add `--version X.Y.Z`). `codewhale update` refuses Cargo installs.
**Uninstall:** `cargo uninstall codewhale-cli`, then §12.

---

## 6. Homebrew on Linux and Nix

### Homebrew on Linux: works, but not with the command the old docs gave

Prerequisite: Homebrew itself. Its installer needs sudo **once**, to create
`/home/linuxbrew/.linuxbrew`. Without sudo rights it stops with
`Insufficient permissions to install Homebrew to "/home/linuxbrew/.linuxbrew"`.
Ask an admin to run
`sudo mkdir -p /home/linuxbrew/.linuxbrew && sudo chown $USER /home/linuxbrew/.linuxbrew`,
then re-run the installer. Afterwards, add
`eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv bash)"` to `~/.bashrc`,
as its "Next steps" say.

```bash
brew install Hmbown/deepseek-tui/codewhale      # full name: taps and trusts in one step
```

The two-step form (`brew tap Hmbown/deepseek-tui` then `brew install
codewhale`) **fails on Homebrew 7.x**:

```
Error: Refusing to load formula hmbown/deepseek-tui/codewhale from untrusted tap hmbown/deepseek-tui.
Run `brew trust --formula hmbown/deepseek-tui/codewhale` or `brew trust hmbown/deepseek-tui` to trust it.
```

Run `brew trust hmbown/deepseek-tui` first, or use the full name above.

Tested with Homebrew 7.0.6 against the v0.10.0 tap formula: the install took
73 s. The tap formula tracks the latest release and downloads the official
release binaries, so there's no compile. It provides **both** `codewhale` and
`codew`. The `Hmbown/deepseek-tui` tap formula also depends on `node`, which
pulled in 31 bottles (~560 MB) on Linux. That `node` dependency belongs to
this tap formula only. The core TUI runs without Node; Computer Use and the JS
execution tool use it when it is on PATH.

* **Upgrade:** `brew upgrade codewhale`. (`codewhale update` refuses, and
  suggests migrating.)
* **Uninstall:** `brew uninstall codewhale && brew untap Hmbown/deepseek-tui`.
  This also autoremoves the tap's node dependency and anything else it pulled
  in. Homebrew's
  download cache (`~/.cache/Homebrew`, ~330 MB) stays until
  `brew cleanup --prune=all`.

### Nix: partially tested

```bash
# flakes are still experimental; the tested setup enabled them once:
mkdir -p ~/.config/nix
echo 'experimental-features = nix-command flakes' >> ~/.config/nix/nix.conf
nix run github:codewhale-hq/CodeWhale -- --version
# one-off alternative (untested on this VM): nix --extra-experimental-features 'nix-command flakes' run github:codewhale-hq/CodeWhale -- --version
```

Nix 2.35 installed fine; single-user mode needs `/nix` created by root once.
Flakes resolved. What I learned before stopping:

* There's **no binary cache**, so this is a full source build of the
  **main branch**, not the v0.10.0 release. The binary reports
  `codewhale 0.10.0 (dev)`.
* The build step took 30 min on 4 cores. The package then runs its **test
  suite** (`doCheck`), which recompiles the workspace in test mode. That took
  over 70 minutes and more than 8 GB of RAM for one rustc process, and I
  stopped it at 105 minutes. So `nix run` completing, `nix build`, and
  `nix profile install/remove` are **(untested on this VM: build did not
  finish in the time budget; the VM's proxy also required an
  `--override-input fenix …` workaround)**.
* Nix provides only `codewhale`; there's no `codew` (it builds just the
  `codewhale-cli` package).

Unless you already live in Nix, use §2 instead.

---

## 7. Updating and rolling back

These work for installs from §2 and §3 (direct binaries). Package-manager
installs (npm, Cargo, Homebrew) must be updated with their own tool.

```bash
codewhale update --check
#   Current binary: /home/you/.local/bin/codewhale
#   Current version: v0.9.13
#   Latest stable release: v0.10.0
#   Update available. Run `/home/you/.local/bin/codewhale update` to install v0.10.0.
codewhale update
#   Downloading codewhale-linux-x64...
#   SHA256 checksum verified against codewhale-artifacts-sha256.txt from GitHub Releases.
#   ✅ Successfully updated to v0.10.0!
#   Updated binaries:
#     - /home/you/.local/bin/codewhale (codewhale-linux-x64)
#     - /home/you/.local/bin/codew (codewhale-linux-x64)
```

It updates `codewhale` **and** `codew` together. It took 10 s here. Run it
again and you get `Already up to date; no download needed.` Other options:
`--beta` and `--proxy <URL>`.

<a id="roll-back-to-a-previous-release"></a>

### Rolling back (e.g. to v0.9.13)

`codewhale update` never downgrades, and `CODEWHALE_VERSION=0.9.13 codewhale
update` just says "Already up to date". To roll back, replace the files:

```bash
dir="$(dirname "$(command -v codewhale)")"     # the install PATH actually selects
rm "$dir/codewhale" "$dir/codew"
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION=v0.9.13 CODEWHALE_INSTALL_DIR="$dir" sh
hash -r; codewhale --version      # codewhale 0.9.13 (a0b81f619b66)
```

Tested with both the default `~/.local/bin` and a custom
`CODEWHALE_INSTALL_DIR`. Use it only for installer, manual or archive
installs. Never point it at an npm, Cargo or Homebrew directory.

Or keep both versions side by side, and put the old one first on PATH:

```bash
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION=v0.9.13 CODEWHALE_INSTALL_DIR="$HOME/.local/codewhale-0.9.13" sh
export PATH="$HOME/.local/codewhale-0.9.13:$PATH"; hash -r
```

To return to the latest after an in-place rollback, run `codewhale update`.
(Tested: 0.9.13 → 0.10.0.)

npm: `npm install -g codewhale@0.9.13`. Cargo:
`cargo install codewhale-cli --version 0.9.13 --locked --force` **(untested on
this VM: only 0.10.0 was built)**.

---

## 8. API keys and providers

### Where Codewhale looks for a key (first match wins)

1. `--api-key <KEY>` on the command line
2. `api_key` in `~/.codewhale/config.toml`
3. the secret store written by `codewhale auth set`
4. the environment variable (`DEEPSEEK_API_KEY` for DeepSeek)

This order matters. **A key in config or the secret store beats
`DEEPSEEK_API_KEY`.** If you rotate your key by exporting a new env var, an
old stored key keeps being used. I tested this: a wrong key in `config.toml`
plus the correct env var gives `Authentication Fails … ****beef is invalid`.

### Ways to set a DeepSeek key (all tested)

**Environment variable.** Good for trying it out and for CI:
```bash
export DEEPSEEK_API_KEY=sk-...          # add to ~/.bashrc / ~/.zshenv to persist
```

**`auth set`.** Saves the key for every folder:
```bash
codewhale auth set --provider deepseek                         # prompts: "Enter API key for deepseek:"
printf '%s\n' "$KEY" | codewhale auth set --provider deepseek --api-key-stdin   # scripted
# -> saved API key for deepseek to file-based ("/home/you/.codewhale/secrets/secrets.json") (config contains metadata only)
```
The file-based message prints the resolved secret-store path; an explicit
`CODEWHALE_HOME` changes that location.

On Linux, the key is stored in **plaintext** in
`~/.codewhale/secrets/secrets.json`, with mode 0600. It is not in an OS
keyring. Note that in v0.10.0, `auth set` also writes
`default_text_model = "deepseek-v4-pro"` into your config, switching you from
the default `deepseek-flash` to the pricier Pro model. Change it back with
`/model` in the TUI, or edit `~/.codewhale/config.toml`.

**Inside the TUI.** Press **F3** (or type `/provider`), select DeepSeek, press
Enter, paste the key (masked), pick a model, and confirm. This also writes the
secret store, and keeps `deepseek-flash`.

**Config file.** `~/.codewhale/config.toml`:
```toml
[providers.deepseek]
api_key = "sk-..."
```

### Check which key is active

```bash
codewhale auth status --provider deepseek
#   active source: env (last4: ...xxxx)        # or: secret store / config / missing
#   lookup order: config -> secret store -> env
codewhale doctor --probe-api
#   · Testing connection...  ✓ API connection successful
```

Plain `codewhale doctor` names an environment key it can see
(`deepseek: env_source=set via DEEPSEEK_API_KEY (value not shown; not checked offline)`)
but does not read the secret store or check the key; `--probe-api` does both.
Doctor exits 0 even when no key is found, so scripts should use `auth status`.

### Remove a stored key

```bash
codewhale auth clear --provider deepseek
#   cleared API key for deepseek from config and secret store
```

It doesn't unset `DEEPSEEK_API_KEY` in your shell, and it leaves the
`default_text_model` line that `auth set` added.

### Other providers

`codewhale auth list` shows about 50 providers (OpenRouter, Anthropic, OpenAI,
Moonshot, Ollama, …). The pattern is the same:
`codewhale auth set --provider <name>`, or the provider's env var. Local models
(Ollama, vLLM, SGLang) need no key. Only DeepSeek was tested here.

---

<a id="8-shell-completions"></a>

## 9. Shell completions

```bash
# bash (needs the bash-completion package)
mkdir -p ~/.local/share/bash-completion/completions
codewhale completion bash > ~/.local/share/bash-completion/completions/codewhale

# zsh
mkdir -p ~/.zfunc
codewhale completion zsh > ~/.zfunc/_codewhale
# in ~/.zshrc, if not already there:
#   fpath=(~/.zfunc $fpath)
#   autoload -Uz compinit && compinit

# fish
mkdir -p ~/.config/fish/completions
codewhale completion fish > ~/.config/fish/completions/codewhale.fish
```

Each script registers both `codewhale` and `codew`. `codewhale completions` is
an alias. Open a new shell afterwards. Regenerate after upgrading.

How well they work in v0.10.0 (tested interactively):

* **bash:** fully works (`codewhale comp<Tab>`, `codew auth <Tab><Tab>`).
* **fish:** sub-commands complete with descriptions, but
  `codewhale completion <Tab>` offers files instead of shell names.
* **zsh:** only the first word completes. After a sub-command
  (`codewhale auth <Tab>`), zsh wrongly lists the top-level commands again
  (same on macOS zsh 5.9, where it offers all 126 top-level entries).

PowerShell and Elvish scripts are generated too **(untested on this VM: shells
not installed)**.

---

## 10. Running it

### The TUI

```bash
cd your-git-repo
codewhale
```

* The composer is at the bottom. The footer shows the permission posture
  (`ask`), the mode (`work`) and the model (`DeepSeek · deepseek-flash`).
* **Shift+Tab** cycles the permission posture: Ask → Auto-Review → Full Access.
  **Tab** (with an empty composer) cycles the mode: Plan → Work → Operate.
* In **Ask**, file edits in the workspace are applied and shown as a diff.
  Shell commands stop at an **APPROVAL** prompt: `y` allow once, `a` allow for
  this session, `n` deny, `Esc` abort the turn.
* Useful keys: **F1** help (or `/help`), **Ctrl-K** command palette, **F3**
  provider/model picker, **Ctrl-R** resume a past session, **Ctrl-U** clear the
  input (**Ctrl-Z** restores it), **Ctrl-C** cancel or quit, **Ctrl-D** quit
  with an empty input. Full list: [KEYBINDINGS.md](KEYBINDINGS.md).
* On exit it prints `To resume this session, run codewhale resume <id>`.

Codewhale creates a `.codewhale/` directory in your repo. Ignore its contents
but keep the committable `constitution.json` (these are the same patterns
`/init` writes):

```gitignore
**/.codewhale/*
!**/.codewhale/constitution.json
```

### Headless (scripts, CI)

```bash
codewhale exec "Reply with exactly: pong"               # one-shot answer, no tools
codewhale exec --auto "create primes.py that prints the first 10 primes and run it"   # tools, auto-approved
codewhale exec --json "…"                               # summary JSON (provider, model, usage, output)
codewhale exec --auto --output-format stream-json "…"   # one JSON event per line
```

`--auto` auto-approves shell commands, so use it only in a repo or sandbox you
trust.

Plain `exec` offers the model no tools. Only `--auto`, `--yolo`,
`--allowed-tools` or resuming a session opens a tool surface; limits such as
`--max-turns`, `--disallowed-tools`, `--sandbox` and the output format never
add tools (tool-only flags print a warning). If the provider stops a reply at
its output limit, the model is asked to continue and the printed answer is the
whole reply. A plain run takes at most 8 model steps unless `--max-turns` sets
another limit; a reply still cut off at that limit fails the run.

### Resuming

```bash
codewhale resume <session-id>     # or a unique prefix, e.g. e2525dfb
codewhale -c                      # continue the most recent session in this folder
codewhale sessions                # list saved sessions
codewhale exec --continue "…"     # headless follow-up to the latest session
codewhale exec --resume <id> "…"
```

In v0.10.0, only **TUI sessions** and **`--output-format stream-json`** exec
runs are saved. A plain `codewhale exec`/`exec --auto` run is *not* saved, so a
following `exec --continue` fails with `No saved sessions found for workspace`.

---

## 11. Terminal notes

### Ghostty (tested: Ghostty 1.3.1 on Linux/X11)

Everything I checked worked in Ghostty with its default config
(`TERM=xterm-ghostty`, `COLORTERM=truecolor`). Screenshots are kept with the
[install receipts](https://github.com/codewhale-hq/Codewhale/tree/37ecdfcc49bc68a9b0d058b97c3946e62c34bd31/docs/install-report/v0.10.0-2026-09-23/screenshots).

| Check | Result |
|---|---|
| Colours / truecolor gradient, box drawing, Unicode (✓ é 日本語) | ✅ |
| Window resize (1504×886 → 800×500 → back) reflows cleanly | ✅ |
| Mouse wheel scrolls the transcript, with a jump-to-bottom button | ✅ |
| Paste (`Ctrl+Shift+V`), multi-line: inserted, not sent | ✅ |
| F1, F3, Ctrl-K, Ctrl-R, Tab, Shift+Tab, Ctrl-U/Ctrl-Z, Ctrl-C, Ctrl-D | ✅ |
| Window title shows state (`waiting on you…`, `✓ done`) | ✅ |
| Exit restores the terminal (normal screen, cursor, no mouse-reporting garbage) | ✅ |

Ghostty on Linux starts a **non-login** shell, so it reads `~/.bashrc` and not
`~/.profile`. That's why you need the PATH line in `~/.bashrc` (§2).

You may notice small dots and a faint label (e.g. `other · drift`) drifting
across empty space after a turn. That's Codewhale's decorative "ambient life"
whale, not a rendering bug.

### Other terminals

tmux eats **F1**, so use `/help` there. Some key chords (Ctrl-Shift-…, Ctrl-Tab)
need a terminal with an enhanced keyboard protocol; [KEYBINDINGS.md](KEYBINDINGS.md) lists
portable alternatives. Windows users should use Windows Terminal
**(untested on this VM)**.

---

## 12. Uninstalling

### Step 1: forget stored keys (if you used `auth set` or F3)

```bash
codewhale auth clear --provider deepseek
```

### Step 2: remove the program

| Installed with | Remove with |
|---|---|
| installer (§2) or manual (§3) | `rm ~/.local/bin/codewhale ~/.local/bin/codew` (or your `CODEWHALE_INSTALL_DIR`) |
| npm | `npm uninstall -g codewhale` |
| Cargo | `cargo uninstall codewhale-cli` |
| Homebrew | `brew uninstall codewhale && brew untap Hmbown/deepseek-tui` (also removes its node dependency) |

### Step 3: remove data. No uninstaller does this for you.

| Path | What it is | Size seen |
|---|---|---|
| `~/.codewhale/` | config.toml, **secrets/secrets.json (plaintext keys)**, sessions/, logs/, catalog/ (model list, ~5 MB), skills/, builtin-plugins/, tasks/, automations/, crashes/, audit.log, composer history | 6–7 MB |
| `~/.deepseek/snapshots/` | v0.10.0 stores its per-turn **copies of your workspaces** here (a legacy path). Contains the contents of every repo you ran it in. | 0.2–0.6 MB here; grows with repo size |
| `<every repo you used>/.codewhale/` | per-workspace state/lock dir | tiny |
| completion files | `~/.local/share/bash-completion/completions/codewhale`, `~/.zfunc/_codewhale`, `~/.config/fish/completions/codewhale.fish` | – |
| PATH lines you added | `~/.bashrc`, `~/.zshrc`, `~/.profile` | – |

```bash
rm -rf ~/.codewhale ~/.deepseek/snapshots
rmdir ~/.deepseek 2>/dev/null   # removes the parent only if it is now empty
# per-repo dirs, e.g.:
find ~ -type d -name .codewhale -prune -print     # review, then delete the ones you want
```

Codewhale wrote nothing outside `$HOME` and the repos it was used in: no
system files, services or cron jobs. (I checked every file owned by the test
users outside their home directories.) The commands above delete only
`~/.deepseek/snapshots`. If you still use the older DeepSeek-TUI, the rest of
`~/.deepseek` (its config and sessions) is left alone.

---

## 13. Troubleshooting

Every error below was hit while writing this guide.

**`bash: codewhale: command not found` right after installing.**
`~/.local/bin` isn't on PATH in this terminal. See
[Put it on your PATH](#put-it-on-your-path).

**`npm error code EACCES … permission denied, mkdir '…/lib/node_modules/codewhale'`.**
Your Node is system-owned. See [§4](#if-you-get-eacces-permission-denied).
Don't use sudo.

**`error: DeepSeek API key not found.` (from `codewhale exec`)**
No key anywhere. Follow the printed steps, or see §8.

**The TUI shows your message but never answers.**
No key (v0.10.0 doesn't say so). Press F3 → DeepSeek → Enter → paste the key.

**`error: Responses API request failed … Authentication Fails, Your api key: ****dead is invalid`.**
The key is wrong or revoked. Run `codewhale auth status --provider deepseek`
to see *which* source is being used. Remember that config and the secret store
beat the env var. Fix with `codewhale auth set --provider deepseek`, or
`codewhale auth clear --provider deepseek` to fall back to the env var. In the
TUI, a bad key sends you to a "Choose your model provider" screen that marks
DeepSeek `last check failed (authentication)`.

**`error: Network error: SSE stream request failed after HTTP/1.1 fallback: Responses API request failed. … on Windows or proxy networks, try CODEWHALE_FORCE_HTTP1=1 …`.**
Despite the wording, on Linux this usually just means **no connection to
`api.deepseek.com`**. Check with `curl -sI https://api.deepseek.com` (a `401`
response is fine; it means the host is reachable). If you're behind a proxy,
make sure `HTTPS_PROXY` is exported. `codewhale doctor --probe-api` only says
`✗ API connection failed` for both bad keys and network problems.

**`codewhale install: refusing to replace existing ~/.local/bin/codewhale`.**
A different version is already installed there. Run `codewhale update`, or
delete the two files first (§7 rollback), or install into a fresh
`CODEWHALE_INSTALL_DIR`.

**`codewhale install: checksum mismatch for codew-linux-x64`.**
The download was corrupted or tampered with. Nothing was installed. Retry, and
if it repeats, don't use a mirror.

**`error: The package-managed executable was not changed.` (from `codewhale update`)**
You installed with npm, Cargo or Homebrew. Update with that tool instead.

**`error: failed to run custom build command for libdbus-sys` (Cargo).**
Run `sudo apt-get install -y libdbus-1-dev pkg-config`.

**`error: No saved sessions found for workspace …` (from `exec --continue`).**
The previous run was plain-text `exec`, which isn't saved. Use the TUI, or
`--output-format stream-json`.

**zsh completion suggests the wrong things after the first word.**
Known v0.10.0 bug. bash and fish are fine.

**Getting help:** `codewhale doctor --json` produces a diagnostics bundle
without secrets.

---

## Appendix: other platforms (not re-tested in this revision)

The sections below are carried over unchanged from the previous revision of
this page. They were **not re-run** for the v0.10.0 install test above
(out of scope: Windows, macOS, Android/Termux, FreeBSD, mainland-China
mirrors), apart from the macOS paths noted in [macOS notes](#macos-notes).
Known contradictions with the published v0.10.0 assets, found by inspecting
them ([details](https://github.com/codewhale-hq/Codewhale/blob/37ecdfcc49bc68a9b0d058b97c3946e62c34bd31/docs/install-report/v0.10.0-2026-09-23/DOC_DEFECTS.md), D15 and D16):

* The winget manifest kept in `packaging/winget/` is stale (0.9.6) and not the
  published package; winget serves `HunterBown.CodeWhale`, see
  [Windows winget](#windows-winget).
* v0.10.0 publishes both `codewhale-windows-x64.zip` (with an `install.bat`
  that copies to `%USERPROFILE%\bin`) and `codewhale-windows-x64-portable.zip`;
  the sections below mention only the first.
* The standalone `codewhale.bat` launcher works only next to the x64 exe.

### Supported platforms and assets

The [latest stable release](https://github.com/codewhale-hq/CodeWhale/releases/latest)
publishes Linux x64/arm64, macOS x64/arm64, Windows x64/arm64, and Android arm64
assets. Artifact presence is distinct from platform qualification.
The table below describes the current source tree's platform and secondary
packaging support; `latest` installation still selects the published release.
Android/Termux is preview pending real-device QA. Linux ARM64 is available from
v0.8.8 onward. Linux RISC-V prebuilts are temporarily paused because the locked
`rquickjs-sys` dependency does not ship `riscv64gc-unknown-linux-gnu` bindings.

| Platform | Architecture | GitHub release asset | npm install | `cargo install` |
| ------------ | ------------ | ----------------------------------------------------- | :---------: | :-------------: |
| Linux | x64 (x86_64) | `codewhale-linux-x64`, `codew-linux-x64` | ✅ | ✅ |
| Linux | arm64 | `codewhale-linux-arm64`, `codew-linux-arm64` | ✅ | ✅ |
| Android / Termux | arm64 (aarch64) | `codewhale-android-arm64.tar.gz` (published in v0.9.12; device support is preview) | ⚠️⁴ preview | ⚠️⁴ preview |
| Linux | riscv64 | temporarily unsupported until upstream bindings land | ❌¹ | ❌³ |
| macOS | x64 | `codewhale-macos-x64`, `codew-macos-x64` | ✅ | ✅ |
| macOS | arm64 (M-series) | `codewhale-macos-arm64`, `codew-macos-arm64` | ✅ | ✅ |
| Windows | x64 | `codewhale-windows-x64.exe`, `codew-windows-x64.exe` | ✅ | ✅ |
| Windows | arm64 | `codewhale-windows-arm64.exe`, `codew-windows-arm64.exe` | ✅ | ✅ |
| Linux x64 or arm64 on musl (Alpine) | native arch | matching static Linux asset | ✅ (static) | ✅ |
| Other Linux (musl on other arches) | — | build from source | ❌¹ | ✅² |
| FreeBSD 14+ / OpenBSD | x64, arm64 | `cargo install codewhale-cli --locked` (no prebuilt; see § FreeBSD) | ❌ | ✅² |

¹ The npm package will exit with a clear error and point you here.
² Provided your toolchain can compile a recent Rust workspace; see
  [Build from source](#5-cargo-and-building-from-source) below.
³ RISC-V source builds currently need upstream `rquickjs-sys` RISC-V bindings or
  a bindgen-enabled dependency build.
⁴ The current npm wrapper recognizes Android arm64 and resolves
  the matching `codewhale` and `codew` Android assets. npm
  installation works only for a package version whose GitHub Release publishes
  those matching assets. The Android/Termux path remains preview-only until the
  real-device compile, startup, approval, file-tool, and update checks tracked
  in #4236 and #4242 are complete.

Android / Termux is not the same target as Linux arm64. Do not install the
Linux `codewhale-linux-arm64` archive in Termux; use the Termux-specific
Android archive when a release or release candidate publishes one, or build
from source inside Termux.

The current Linux **x64 and arm64** assets are **static musl builds**.
The x64 release path has used musl since v0.8.65; v0.9.6 extends the same build
and static-launch check to arm64. These binaries have no glibc dependency and
run on their matching architecture across Ubuntu, Debian, RHEL/CentOS, and
Alpine/musl. SQLite is bundled through `rusqlite`, so no separate `libsqlite3`
runtime package is needed.

#### Linux ARM64 portability

Linux arm64 assets before v0.9.6 were GNU libc builds and could inherit the
Ubuntu 24.04 build host's `GLIBC_2.39` floor. Ubuntu 22.04 ships glibc 2.35, so
those older arm64 binaries can fail with errors such as:

```text
version `GLIBC_2.39' not found
```

The npm wrapper, `codewhale update`, and the Unix archive installer retain their
GNU-binary preflight for older releases. The current arm64 build instead uses
`aarch64-unknown-linux-musl`, so it has no `GLIBC_*` floor. If you are installing
an earlier release on an older arm64 distribution, use:

```bash
cargo install codewhale-cli --locked   # installs `codewhale`
```

> **Linux ARM64 note (v0.8.7 and earlier).** v0.8.7 and earlier do **not**
> publish a Linux ARM64 prebuilt; users on HarmonyOS thin-and-light, Asahi
> Linux, Raspberry Pi, AWS Graviton, etc. saw `Unsupported architecture: arm64`
> from `npm i -g codewhale`. v0.8.8 publishes `codewhale-linux-arm64`, so a plain `npm i -g codewhale` works
> on any glibc-based ARM64 Linux. If you're stuck on v0.8.7, jump to
> [Build from source](#5-cargo-and-building-from-source) — `cargo install` works fine.
> For HarmonyOS PC and OpenHarmony cross-build setup, see
> [HarmonyOS and OpenHarmony](HarmonyOS.md).

### Migrating from npm, Cargo, or another installation

#### Migrating from npm, Cargo, or another installation

Package managers continue to own their files. `codewhale update` gives migration
instructions for npm, Cargo, Homebrew, and Omarchy instead of overwriting them.
Known system/package directories are also protected. A `CODEWHALE_INSTALL_METHOD=binary`
override cannot bypass a recognized managed path.

Create a fresh destination when `~/.local/bin` is occupied or a sibling command
has different bytes. This leaves every existing installation in place:

```bash
mkdir -p "$HOME/.local"
codewhale_install_dir="$(mktemp -d "$HOME/.local/codewhale-release.XXXXXX")"
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_INSTALL_DIR="$codewhale_install_dir" sh
"$codewhale_install_dir/codewhale" --version
export PATH="$codewhale_install_dir:$PATH"
hash -r
command -v codewhale codew
"$codewhale_install_dir/codewhale" update --check
```

After verifying the version and command paths, keep that directory first in your
shell profile. In PowerShell, use `Get-Command codewhale, codew -All` to inspect
resolution; run the selected executable using its full path. A successful update
only changes its own install directory, so another earlier PATH entry can still
launch an older copy.

Modern matched `codewhale`, `codew`, and compatibility copies update from the
same verified bytes. Symlinks to the running binary are preserved. A different
or unrelated sibling is named in the error and left untouched; no sibling is
executed merely to guess its owner. Use the fresh-directory migration above
for older installs with separate dispatcher/TUI binaries.

To retain a secondary package-managed install, use its manager:

```bash
npm install -g codewhale@latest
# or
cargo install codewhale-cli --locked --force
```

Homebrew uses `brew upgrade codewhale`; Omarchy uses `omarchy update`. These
commands update their own copies, so verify PATH again afterward.

<a id="android--termux-arm64"></a>

### Android / Termux arm64 (preview)

Termux runs on Android's Bionic libc and uses `$PREFIX` as its Unix prefix, so
it needs a Termux-specific Android arm64 archive. The Linux arm64 release asset
targets standard Linux with musl; Android uses a distinct Rust target, so the
Linux asset should not be used there.

Install the minimum archive/runtime tools first:

```bash
pkg update
pkg install -y ca-certificates curl tar gzip coreutils
```

When the release includes `codewhale-android-arm64.tar.gz`, install it with the
archive's bundled installer. Passing `PREFIX="$PREFIX"` matters: the installer
defaults to `~/.local`, while Termux users normally expect commands under
`$PREFIX/bin`.

```bash
cd "$HOME"
curl -L -O https://github.com/codewhale-hq/CodeWhale/releases/latest/download/codewhale-android-arm64.tar.gz
curl -L -O https://github.com/codewhale-hq/CodeWhale/releases/latest/download/codewhale-bundles-sha256.txt
sha256sum -c codewhale-bundles-sha256.txt --ignore-missing

tar xzf codewhale-android-arm64.tar.gz
cd codewhale-android-arm64
PREFIX="$PREFIX" ./install.sh
hash -r
```

If you are validating from source or building a release candidate locally,
install the build packages before running Cargo:

```bash
pkg install -y rust clang pkg-config make git
cargo install codewhale-cli --locked   # installs `codewhale`
```

The normal first-run setup path is implemented, but its Android interaction is
still part of the preview QA above. Prefer provider environment variables for
temporary credentials. `codewhale auth set` is available, but the Termux build
has no supported OS keyring integration and falls back to file-backed secrets
by writing `~/.codewhale/config.toml` and mirroring keys to
`~/.codewhale/secrets/secrets.json`. Both are plaintext files protected by
`0600` permissions and are not encrypted at rest.

```bash
codewhale auth set --provider deepseek
codewhale auth status
codewhale doctor
```

Maintainers should use this repeatable smoke checklist for a Termux / Android
arm64 release candidate:

```bash
command -v codewhale codew
test -x "$PREFIX/bin/codewhale"
test -x "$PREFIX/bin/codew"

codewhale --version
codewhale doctor
codewhale exec --auto "run pwd"
```

Known limitations:

- Commands inherit Android's per-app UID, SELinux, and seccomp protections and
  any permissions granted to Termux. Codewhale's bubblewrap
  child-process sandbox is Linux-only and is not built on Android, so approved
  commands receive no Codewhale-specific filesystem narrowing.
- The Termux build has no supported Android Keystore or desktop Secret Service
  integration. Use `codewhale auth status` to confirm the active source and
  prefer provider environment variables when file-backed plaintext storage is
  not acceptable.
- Terminal rendering varies by Android terminal app. The TUI always owns the
  alternate screen. If a terminal app cannot render the full-screen TUI,
  use `codewhale exec` for headless runs instead.

### China / mirror-friendly install

When installing from mainland China, configure mirrors for both **rustup**
(the Rust toolchain installer) and **Cargo** (the package registry) to avoid
TLS timeouts and download failures.

**Step 1: Install Rust via a rustup mirror**

```bash
# PowerShell
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
(New-Object Net.WebClient).DownloadFile('https://win.rustup.rs/x86_64', 'rustup-init.exe')

# git-bash / msys2
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
./rustup-init.exe -y --default-toolchain stable

# Linux / macOS
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
```

If the TUNA mirror is slow from your network, `rsproxy.cn` is another
rustup mirror option for Linux/macOS:

```bash
export RUSTUP_DIST_SERVER=https://rsproxy.cn
export RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
```

The `RUSTUP_DIST_SERVER` and `RUSTUP_UPDATE_ROOT` environment variables must
be set **before** running rustup-init; the toolchain download otherwise hits
the same TLS handshake problem as the installer.

**Step 2: Configure Cargo registry mirror**

```toml
# ~/.cargo/config.toml
[source.crates-io]
replace-with = "tuna"

[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"
```

`rsproxy`, Tencent COS, and Aliyun OSS mirrors work the same way; pick whichever
is fastest from your network.

### Omarchy / AUR

On Omarchy, install the prebuilt AUR package:

```bash
omarchy pkg aur add codewhale-bin
codewhale --version
```

`codewhale-bin` packages the same checksum-pinned Linux release archives as the
other binary install paths and provides both `codewhale` and `codew`. It does
not carry a separate Codewhale version; the existing `codewhale-tui`
compatibility command remains an alias to the same runtime. Package updates
arrive through `omarchy update`; the in-app updater leaves the pacman-owned
binary to Omarchy.

The AUR update follows the matching Codewhale tag and release assets, so it may
appear after the GitHub release while its generated `PKGBUILD` and `.SRCINFO`
are validated. Release-maintainer instructions live in
[`packaging/aur/README.md`](../packaging/aur/README.md).

---

### Windows

#### Windows Scoop

The `codewhale` package is listed in Scoop's main bucket:

```powershell
scoop update
scoop install codewhale
codewhale --version
```

Scoop manifests are maintained outside this repository's release workflow and
can lag GitHub/npm/Cargo releases. Use npm or manual GitHub release downloads
when you need the newest version immediately.

#### Windows winget

The published winget package is **`HunterBown.CodeWhale`** (verified in
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs/tree/master/manifests/h/HunterBown/CodeWhale)
on 2026-10-04; latest published version 0.10.0). It is a portable x64 package:
winget downloads `codewhale-tui-windows-x64.exe`, installs it as the `codewhale`
command, and pulls in the Microsoft Visual C++ 2015+ x64 runtime.

```powershell
winget install HunterBown.CodeWhale
codewhale --version
```

Update with `winget upgrade HunterBown.CodeWhale` or `codewhale update`. Each
version goes through winget-pkgs review after the GitHub Release, so winget can
lag GitHub/npm/Cargo; use npm or the GitHub Release asset when you need the
newest version immediately.

Known limits of the winget route:

* **x64 only.** The published manifest has no ARM64 installer. On Windows ARM64,
  use `npm install -g codewhale` under native ARM64 Node.js, or download
  `codewhale-windows-arm64.zip` from GitHub Releases.
* **`codewhale` only.** The short `codew` alias is not installed by winget; use
  `codewhale`.
* The manifest in this repository (`packaging/winget/`) is not the one winget
  serves; see [`packaging/winget/README.md`](../packaging/winget/README.md).

#### Windows NSIS Installer

A standalone NSIS-based installer is available starting with v0.8.50 for
Windows users who prefer a traditional double-click setup (no npm, no Scoop, no
Cargo required).

The NSIS installer currently contains the Windows x64 binaries. Windows ARM64
users should install through npm running under native ARM64 Node.js or download
`codewhale-windows-arm64.zip` from the same release; both paths then use native
ARM64 binaries.

**Download** `CodeWhaleSetup.exe` from the
[Releases page](https://github.com/codewhale-hq/CodeWhale/releases/latest).

**Install** by double-clicking the setup executable. The installer:

- Installs `codewhale.exe` and `codew.exe` side-by-side (single binary, no `codewhale-tui.exe`) into
  `%LOCALAPPDATA%\Programs\CodeWhale\bin`
- Installs `codewhale.bat`, which prefers Windows Terminal (`wt.exe`) when it is on `PATH` and
  otherwise launches the exe directly
- Creates a current-user Start Menu shortcut that opens that launcher, not the raw `.exe`
- Adds the install directory to the **current user** `PATH`
- Registers in Windows **Apps & Features** for easy uninstall

Uninstall removes the binaries, `codewhale.bat`, the Start Menu shortcut, and the user `PATH` entry.

**Silent install** (for IT admins, SCCM, Intune):

```powershell
CodeWhaleSetup.exe /S
```

The installer is per-user and does not request elevation. Run silent installs in
the target user's context, or use a deployment tool that can run the installer
for each user profile that needs Codewhale.

The release-built installer is currently unsigned and may trigger Windows
SmartScreen. Verify the SHA-256 checksum from `codewhale-artifacts-sha256.txt`
before deploying, and sign the installer in your internal deployment pipeline if
your environment requires signed application packages.

**Build the installer yourself** (requires [NSIS](https://nsis.sourceforge.io)):

```powershell
cd scripts\installer
# Place codewhale.exe and codew.exe here (single binary, no codewhale-tui.exe), then:
makensis /DVERSION=<version> codewhale.nsi
```

**Manual fallback** — if the installer is blocked by group policy, see the
[CLASSROOM_INSTALL.md](CLASSROOM_INSTALL.md) guide for step-by-step PowerShell
commands.

> **Deploying to a classroom or lab?** See the full
> [Classroom Install Checklist](CLASSROOM_INSTALL.md) for silent install,
> API key provisioning, imaging notes, and troubleshooting.

<a id="freebsd"></a>

### FreeBSD, cross-compiling, Windows source builds

#### FreeBSD 14+ source-build workaround (#1097)

FreeBSD has no prebuilt GitHub Release asset — `npm install -g codewhale` intentionally
fails with `Unsupported platform: freebsd` and points to Cargo. Install from source:

```bash
pkg install -y rust pkgconf git
cargo install codewhale-cli --locked   # installs `codewhale`
codewhale --version
codewhale doctor
```

The `rquickjs` FreeBSD bindings are generated at build time via `bindgen` (see
`1582ba965`/`5eb0385e8`). No separate `pkg install codewhale` port exists yet —
a native port is tracked as the follow-up to #1097 under `packaging/freebsd/`
(contributions welcome). Validate with `cargo check --target x86_64-unknown-freebsd -p codewhale-cli --locked`
on the release branch; the 7×1 release matrix (Linux musl x64/arm64,
Android arm64, macOS x64/arm64, Windows x64/arm64) stays 7 targets — FreeBSD is a
source-build target, not a prebuilt asset.

#### Cross-compiling from x64 to ARM64 Linux

The release asset uses `aarch64-unknown-linux-musl` and is built on a native ARM
runner. If you want to build a GNU-linked ARM64 Linux binary on an x64 Linux
host (e.g. for a HarmonyOS / openEuler ARM64 thin-and-light), use
[`cross`](https://github.com/cross-rs/cross), which wraps the official Rust
cross-targets in a Docker container:

```bash
# Once
rustup target add aarch64-unknown-linux-gnu
cargo install cross --locked

# Per build
cross build --release --target aarch64-unknown-linux-gnu -p codewhale-cli   # single binary
```

The resulting binary lands in
`target/aarch64-unknown-linux-gnu/release/codewhale`. Copy it to the ARM64 host
(e.g. via `scp`) and make it executable. This local GNU build is distinct from
the portable musl release asset; either executable can be copied under the
`codew` convenience name.

If you don't have Docker available, install the cross-linker directly and let
Cargo do the work:

```bash
sudo apt-get install -y gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu

cat >> ~/.cargo/config.toml <<'EOF'
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"
EOF

cargo build --release --target aarch64-unknown-linux-gnu -p codewhale-cli   # single binary
```

Producing `aarch64-unknown-linux-musl` while cross-compiling requires an
appropriate musl cross-linker. The release workflow avoids that extra moving
part by building and launching the musl binary on GitHub's native ARM runner.

#### Windows build from source

Building on Windows requires the **MSVC C toolchain** from
[Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
(the free workload-selectable installer, not the full IDE).

**Prerequisites (Windows)**

1. Install Visual Studio 2022 Build Tools — select the **"Desktop development
   with C++"** workload.
2. Install [Rust](https://rustup.rs) 1.89+ (see the
   [China mirror instructions](#china--mirror-friendly-install) above if
   downloading from mainland China).
3. Install [Git for Windows](https://git-scm.com/download/win) (provides `git`
   and the `git-bash` terminal).

**Recommended terminals**: Windows Terminal, `git-bash`, or PowerShell.
`cmd.exe` works but has a small buffer and limited PATH behavior.

**Setting up the MSVC environment**

Visual Studio Build Tools install `cl.exe` to a versioned directory but do
**not** add it to `PATH` globally. You must set the environment manually or
use a Developer Command Prompt. The required variables are:

```powershell
# Adjust version numbers to match your installation
$msvc = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207"
$sdk   = "C:\Program Files (x86)\Windows Kits\10"
$sdkv  = "10.0.26100.0"

$env:INCLUDE  = "$msvc\include;$msvc\atlmfc\include;$sdk\Include\$sdkv\ucrt;$sdk\Include\$sdkv\um;$sdk\Include\$sdkv\shared"
$env:LIB      = "$msvc\lib\x64;$msvc\atlmfc\lib\x64;$sdk\Lib\$sdkv\ucrt\x64;$sdk\Lib\$sdkv\um\x64"
$env:LIBPATH  = "$msvc\lib\x64;$msvc\atlmfc\lib\x64"
$env:CC       = "$msvc\bin\Hostx64\x64\cl.exe"
$env:CXX      = "$msvc\bin\Hostx64\x64\cl.exe"
$env:PATH     = "$msvc\bin\Hostx64\x64;$env:PATH"
```

Alternatively, open a **"Developer Command Prompt for VS 2022"** (available
from the Start Menu after installing Build Tools), which runs `vcvars64.bat`
to configure all of the above automatically. Then add `cargo` to `PATH` inside
that session and run `cargo build` from the project root.

**Cargo registry mirror** — on Windows the mirror config goes to
`%USERPROFILE%\.cargo\config.toml`. See [Step 2 above](#china--mirror-friendly-install).

**Build**

```bash
git clone https://github.com/codewhale-hq/CodeWhale.git
cd CodeWhale
set CARGO_HTTP_CHECK_REVOKE=false   # may be needed behind some Chinese ISPs
cargo build --release
```

The Cargo-built binary appears at `target\release\codewhale.exe`. Release
packaging separately exposes the same executable as `codew.exe`.

> Prefer not to build? Install via npm, Cargo, GitHub Releases, or the CNB
> mirror — see the sections above.

### Older-release and regional troubleshooting

#### `Unsupported architecture: arm64 on platform linux`

You're on a release earlier than v0.8.8 that doesn't publish Linux ARM64
binaries. Use the GitHub installer in a fresh directory as described above, or use
`cargo install` per [Section 4](#5-cargo-and-building-from-source).

#### `MISSING_COMPANION_BINARY` after upgrading an older install

The current single binary runs the TUI in-process and does not require a
companion executable. This error identifies a stale pre-v0.9.5 dispatcher.
Use the fresh-directory GitHub migration above, then verify the selected
`codewhale` and `codew` paths. Do not download another separate runtime.

#### `codewhale update` reports `no asset found for platform codewhale-linux-aarch64`

Older updaters used Rust architecture names that did not match the published
asset names. Use the official installer in a fresh directory as described above,
then run the newly installed command by its full path.

#### npm download is slow or times out from mainland China

On Linux x64 the npm wrapper already probes GitHub Releases and the CNB
first-party checksum manifests in parallel and downloads binaries only from
the first source that validates. You do not need `CODEWHALE_USE_CNB_MIRROR=1`
for that automatic path.

If both first-party sources fail, set `CODEWHALE_RELEASE_BASE_URL` to a
mirrored release-asset directory (rsproxy, TUNA, Tencent COS, Aliyun OSS),
or skip npm entirely and use the Cargo mirror setup in
[Section 4](#5-cargo-and-building-from-source). The legacy
`DEEPSEEK_TUI_RELEASE_BASE_URL` name is still accepted. `CODEWHALE_USE_CNB_MIRROR=1`
still forces CNB only on Linux x64 / OpenHarmony x64.

#### `codewhale update` is blocked by GitHub from mainland China

`codewhale update` prefers GitHub Releases. On supported Linux x64 targets,
a failed GitHub manifest permits the matching CNB manifest and binary fallback.
If GitHub metadata is also unreachable, explicitly select a known published CNB
version (`CODEWHALE_USE_CNB_MIRROR=1 CODEWHALE_VERSION=X.Y.Z codewhale update`)
or a binary mirror below. Existing newer builds are kept.

Building from the CNB source mirror with Cargo is a secondary option. Cargo
installs its own `codewhale` command:

To check the latest release without downloading or replacing binaries, run
`codewhale update --check`.

```bash
cargo install --git https://cnb.cool/codewhale.net/codewhale --tag vX.Y.Z codewhale-cli --locked --force   # single binary
```

If you operate a binary asset mirror, `codewhale update` can use it directly:

```bash
CODEWHALE_RELEASE_BASE_URL=https://your-mirror.example.com/CodeWhale/vX.Y.Z/ \
CODEWHALE_VERSION=X.Y.Z \
codewhale update
```

The mirror directory must contain `codewhale-artifacts-sha256.txt` and the
platform binaries from the GitHub release. The legacy
`DEEPSEEK_TUI_RELEASE_BASE_URL` mirror variable remains supported as an alias.

`codewhale update` only talks HTTPS, and only to GitHub's release hosts, the CNB
mirror, and the host of the `CODEWHALE_RELEASE_BASE_URL` you set; every
redirect hop is held to the same rule, so a plain-`http://` mirror is refused.
A private mirror therefore works as soon as its base URL is HTTPS. If that
mirror redirects asset downloads to a separate download host (a CDN or an
object-store domain), name that host too:

```bash
CODEWHALE_UPDATE_ALLOWED_HOSTS=cdn.your-mirror.example.com,objects.example.net \
CODEWHALE_RELEASE_BASE_URL=https://your-mirror.example.com/CodeWhale/vX.Y.Z/ \
CODEWHALE_VERSION=X.Y.Z \
codewhale update
```

The error message for a refused host names it and this variable.

### Windows and npm-download troubleshooting

#### Windows: `TLS handshake eof` or `CRYPT_E_REVOCATION_OFFLINE` from `rustup-init`

The TLS handshake to `static.rust-lang.org` fails from behind the GFW or
certain Chinese ISPs. Set the rustup mirror environment variables **before**
running the installer:

```bash
# git-bash / msys2
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
./rustup-init.exe -y --default-toolchain stable
```

If you see `CRYPT_E_REVOCATION_OFFLINE` from Cargo after Rust is installed,
also set `CARGO_HTTP_CHECK_REVOKE=false` during `cargo build`.

#### Windows: MSVC compiler (`cl.exe`) not found during `cargo build`

Visual Studio Build Tools do not add `cl.exe` to the global `PATH`. Either:

1. Open **"Developer Command Prompt for VS 2022"** from the Start Menu, add
   `%USERPROFILE%\.cargo\bin` to `PATH` in that window, and run `cargo build`
   from there; or
2. Set the MSVC environment variables manually — see the
   [Windows build from source](#windows-build-from-source) section for the
   PowerShell snippet.

Verify the compiler is reachable: `cl.exe /?` should print help text.

#### Windows: `拒绝访问 (os error 5)` when Cargo executes build scripts

Third-party antivirus software (Huorong, 360, Kaspersky, etc.) may block
Cargo from executing freshly-compiled build-script binaries
(e.g. `libsqlite3-sys`, `aws-lc-sys`, `instability`). The error is
path-agnostic — moving `target-dir` does not help.

**Symptoms**: `could not execute process ... build-script-build (never executed)`

**Workarounds** (pick one):

1. **Add the project's `target/` directory to your AV exclusions list.**
2. **Close the antivirus software temporarily** during `cargo build`.
3. **Use the GitHub Release installer/archive instead** — the release assets
   ship prebuilt binaries and skip the Cargo build entirely
   ([Section 6](#3-manual-download-from-github-releases)).
4. **Use `cargo install codewhale-cli --locked`** from crates.io — this
   changes the binary path, which some AV tools treat differently.

To verify that the build-script binary itself is valid (not corrupted), locate
it under `target/debug/build/<crate>/build-script-build` and run it manually:

```bash
target/debug/build/libsqlite3-sys-*/build-script-build
# If this runs but panics with "NotPresent" (no C compiler), the binary is
# fine — the AV is blocking Cargo's process-spawning path specifically.
```

#### npm binary download times out

If `codewhale` waits several seconds and prints `connect ETIMEDOUT` or
`EAI_AGAIN` while fetching from `github.com`, the npm wrapper installed
successfully but the prebuilt binary download is blocked or unreliable on
your network. This download is separate from the npm registry package
download. On Linux x64 the wrapper first races the small GitHub and CNB
checksum manifests and does not wait for a full GitHub binary to time out
before using a valid CNB manifest.

Use one of these paths:

1. Set a proxy and retry:

   ```bash
   export HTTPS_PROXY=http://your-proxy:port
   codewhale
   ```

2. Mirror the release assets internally and set `CODEWHALE_RELEASE_BASE_URL`:

   ```bash
   export CODEWHALE_RELEASE_BASE_URL=https://your-mirror.example.com/CodeWhale/
   codewhale
   ```

   The directory must contain `codewhale-artifacts-sha256.txt` and the platform
   binaries from the GitHub release.

3. Install via Cargo, which builds locally and does not download GitHub release
   assets. See [Section 4](#5-cargo-and-building-from-source).

4. Download both matching `codewhale` and `codew`
   binaries from the [Releases page](https://github.com/codewhale-hq/CodeWhale/releases),
   place them in a directory on `PATH`, and make them executable. See
   [Section 6](#3-manual-download-from-github-releases).
