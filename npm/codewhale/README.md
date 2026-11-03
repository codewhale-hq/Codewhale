# codewhale

> The terminal coding agent for supported hosted and local models — open models first.

Codewhale is a Rust TUI and CLI for many model providers — DeepSeek,
OpenRouter, Hugging Face, and local vLLM/SGLang/Ollama are supported routes,
and it speaks natively to Anthropic Claude and OpenAI when that's what you have
— with approval-gated tools, OS sandboxing, side-git snapshots, and `/restore`
rollback.

This npm package is a small launcher: it downloads the matching native
Codewhale binaries for your platform, verifies them against the release
SHA-256 manifest, and installs `codewhale` plus the `codew` convenience name.
Both names run the same compiled runtime. The application state and credentials
still live in Codewhale's normal config files, not inside `node_modules`.

> Previously published as `deepseek-tui`. See
> [docs/REBRAND.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/REBRAND.md)
> for the migration notes; the legacy `deepseek-tui` npm package is deprecated
> and receives no further releases.

## Install

```bash
npm install -g codewhale
# or
pnpm add -g codewhale
```

On Windows, the Node launcher stays alive while the native program runs.
Killing Node by process name can also interrupt other Codewhale npm sessions
and prevent normal terminal cleanup. Stop dev servers by their owned PID or
port, or use Codewhale's task cancellation. The Windows native archive or
installer avoids this launcher dependency. See the [Windows npm note](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md#4-npm).

For project-local usage:

```bash
npm install codewhale
npx codewhale --help
```

`postinstall` tries to download platform binaries into `bin/downloads/`. On
Linux x64 it concurrently probes the GitHub Releases and CNB first-party
checksum manifests for this package version, locks the first source that
validates, and downloads binaries only from that source. If GitHub release
assets are temporarily unreachable, install continues and the wrapper retries
the download on first run.

## First run

```bash
codewhale auth set --provider deepseek
codewhale auth status
codewhale doctor
codewhale
```

Every provider is the same one-line shape — `--provider openrouter`,
`--provider huggingface`, `--provider ollama`, or `--provider anthropic` for a
Claude key; the full registry lives in
[docs/PROVIDERS.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/PROVIDERS.md).

The single runtime reads `~/.codewhale/config.toml` for auth and default model
settings. Legacy `~/.deepseek/config.toml` installs are still read as a
compatibility fallback. Common commands are available directly, including
`codewhale doctor`, `codewhale models`, `codewhale sessions`, and
`codewhale resume --last`.

## Supported platforms

Prebuilt binaries for the GitHub release are downloaded automatically:

- Linux x64
- Linux arm64
- macOS x64 / arm64
- Windows x64 / arm64
- Android arm64 / Termux (preview; requires matching Android assets in the
  selected GitHub Release)

The wrapper recognizes Android arm64 and resolves the
Termux-native `codewhale` and `codew` assets. That path works only for package
versions whose matching GitHub Release publishes both assets, and remains
preview support pending real-device QA. See the support table in
[docs/INSTALL.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md).

On a HarmonyOS PC, install inside the Linux subsystem (融合开发引擎, openEuler),
where the wrapper sees `linux` and installs the Linux arm64 binaries. In a native
HarmonyOS or OpenHarmony terminal (`openharmony`), the wrapper still installs the
static Linux build matching your CPU architecture (x64 or arm64) as a best
effort. That build is untested on native HarmonyOS. If the binary cannot start,
`codewhale` prints directions to use the Linux subsystem or build
`aarch64-unknown-linux-ohos` from source. The install script also prints a
warning, which npm 7 and later show only with `--foreground-scripts`. See
[docs/HarmonyOS.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/HarmonyOS.md).

Linux riscv64 prebuilts are temporarily paused while the locked `rquickjs-sys`
dependency lacks `riscv64gc-unknown-linux-gnu` bindings. Other
platform/architecture combinations (FreeBSD, Linux riscv64, …) aren't shipped
as prebuilts. Unsupported platforms, checksum failures, and glibc compatibility
problems still fail with a clear error pointing you at the full
[docs/INSTALL.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md)
guide.

## Wrapper configuration

| Setting | What it does |
| --- | --- |
| `codewhaleBinaryVersion` in `package.json` | Default native binary version. `deepseekBinaryVersion` is still read as a backward-compat fallback. |
| `CODEWHALE_RELEASE_BASE_URL` | Canonical override: use an internal or mirrored release-asset directory and skip the Linux x64 GitHub/CNB race. The directory must contain `codewhale-artifacts-sha256.txt` and the platform binaries. `DEEPSEEK_TUI_RELEASE_BASE_URL` and `DEEPSEEK_RELEASE_BASE_URL` are the implemented legacy fallbacks. |
| `CODEWHALE_USE_CNB_MIRROR=1` | Force the CNB (China-friendly) first-party mirror on Linux x64, skipping the automatic race. Other targets, native HarmonyOS included, fail with a clear unsupported-mirror error; use GitHub or a complete `CODEWHALE_RELEASE_BASE_URL` mirror there. Without this variable, Linux x64 still probes CNB and GitHub together and uses the first valid checksum manifest. |
| `CODEWHALE_VERSION` | Override the release version to download. |
| `CODEWHALE_GITHUB_REPO` | Override the source repo. Defaults to `codewhale-hq/CodeWhale`. |
| `CODEWHALE_FORCE_DOWNLOAD=1` | Force download even when the cached binary is already present. |
| `CODEWHALE_DISABLE_INSTALL=1` | Skip install-time download. |
| `CODEWHALE_OPTIONAL_INSTALL=1` | Make install-time retryable download failures warn and exit `0` instead of failing `npm install`. |
| `CODEWHALE_QUIET_INSTALL=1` | Suppress installer progress messages. |
| `CODEWHALE_DOWNLOAD_TIMEOUT_MS` | Override the total download budget. |
| `CODEWHALE_DOWNLOAD_STALL_MS` | Override the no-progress stall budget. |
| `CODEWHALE_SKIP_GLIBC_CHECK=1` | Bypass the Linux glibc preflight check at your own risk. |

The corresponding `DEEPSEEK_TUI_*` and `DEEPSEEK_*` names remain accepted as
legacy aliases, after the canonical Codewhale names.

### Proxies

Downloads respect `HTTPS_PROXY` / `HTTP_PROXY` (CONNECT tunneling included)
and `NO_PROXY`, so the wrapper works behind corporate proxies. For fully
offline installs, set `CODEWHALE_DISABLE_INSTALL=1` or point
`CODEWHALE_RELEASE_BASE_URL` at a local mirror.

## Release integrity

- `npm publish` runs a release-asset check to ensure the required binaries,
  archives, Windows installer, and checksum manifests exist for the target
  GitHub release before publishing.
- For the default GitHub Release source, `npm run release:check` also verifies
  that those release assets were updated by a successful `release.yml` run for
  the tag commit. When `CODEWHALE_RELEASE_BASE_URL` or a legacy mirror override
  is set, it checks the mirror asset URLs and checksum manifests instead.
- Install-time downloads are verified against the release checksum manifest before
  the wrapper marks them executable.

## Links

- Repository: <https://github.com/codewhale-hq/CodeWhale>
- Website: <https://codewhale.net/>
- Provider registry: [docs/PROVIDERS.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/PROVIDERS.md)
- Changelog: [CHANGELOG.md](https://github.com/codewhale-hq/CodeWhale/blob/main/CHANGELOG.md)
