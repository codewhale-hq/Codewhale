<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Open-source Rust coding agent for your terminal. Any model, multi-agent fleets, OS sandboxing, and verifiable receipts.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Website](https://codewhale.net) · [Documentation](docs/README.md) · [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md)

[简体中文](docs/i18n/README.zh-CN.md) · [日本語](docs/i18n/README.ja-JP.md) · [Tiếng Việt](docs/i18n/README.vi.md) · [Bahasa Indonesia](docs/i18n/README.id.md) · [한국어](docs/i18n/README.ko-KR.md) · [Español](docs/i18n/README.es-419.md) · [Português](docs/i18n/README.pt-BR.md) · [Русский](docs/i18n/README.ru.md) · [Українська](docs/i18n/README.uk.md) · [Français](docs/i18n/README.fr.md) · [Deutsch](docs/i18n/README.de.md) · [繁體中文](docs/i18n/README.zh-TW.md) · [हिन्दी](docs/i18n/README.hi.md) · [Türkçe](docs/i18n/README.tr.md) · [Italiano](docs/i18n/README.it.md) · [Polski](docs/i18n/README.pl.md) · [العربية](docs/i18n/README.ar.md) · [Català](docs/i18n/README.ca.md)

<img src="web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 in a terminal after a real task on local Ollama (qwen3:4b): it edited ls.py, waited for approval before running python3 -m unittest -v test_ls.py, ran 2 passing tests, and reported the result.">

<sub>Real session on published Codewhale v0.10.4 (48669ad108fa) running local Ollama (qwen3:4b) in Accept Edits mode.</sub>

</div>

## Install

The install script above puts checksum-verified `codewhale` and its short alias
`codew` in `~/.local/bin` on macOS and Linux. If your shell reports
`command not found`, add `~/.local/bin` to your `PATH`
([details](docs/INSTALL.md#put-it-on-your-path)). Upgrade in place with
`codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Prebuilt binaries ship for Linux, macOS, and Windows (x64 and arm64). Scoop,
Nix, Homebrew on Linux, Android/Termux (preview), and manual downloads are in
[docs/INSTALL.md](docs/INSTALL.md); container images are in
[docs/DOCKER.md](docs/DOCKER.md). Use one install method per machine so two
binaries do not shadow each other on `PATH`.

## Quickstart

1. Commit or stash your work, then run `codewhale` from a git repository root.
2. Run `/provider` (or press `F3`) to select a provider, paste an API key, and
   pick a model. DeepSeek is the default; if no key is configured and a local
   Ollama server responds, Codewhale offers its models on your first message.
3. Give it a task:

```text
Fix the failing tests and explain what changed.
```

Read-only commands run immediately. Mutating shell commands prompt for
approval: `y` runs once, `a` allows matching commands for the rest of the
session, and `n` denies. Run `/undo` to revert the last turn and its file
edits, or `/help` for commands and keybindings.

## Capabilities

- **50+ built-in providers and local models:** Anthropic, OpenAI, DeepSeek
  (`deepseek-flash` / V4.1 Flash by default;
  [details](docs/PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot,
  OpenRouter, xAI, any OpenAI-compatible endpoint, and local runtimes via
  Ollama, vLLM, or SGLang. Switch models mid-session with `/model`, route
  per-turn with `/model auto` (`/router`), and inspect latency and cost with
  `/turn inspect`. See [Providers](docs/PROVIDERS.md).
- **Receipts and rollback:** `/receipts` (or `codewhale receipts --last` after
  exiting) records changed files, commands, web/MCP calls, sub-agents,
  approvals, and failures. `/undo` reverts the last turn in one step;
  `/restore` rolls the workspace back to an earlier snapshot (requires
  `/trust on` or Full Access). See [Receipts](docs/RECEIPTS.md).
- **Sub-agents, Fleets, and workflows:** Carry multi-turn objectives with
  `/goal`, delegate work to [sub-agents](docs/SUBAGENTS.md), coordinate role-
  and model-specific [Fleets](docs/FLEET.md), or run repository-checked
  [workflows](docs/WORKFLOW_AUTHORING.md).
- **MCP, skills, hooks, and plugins:** Attach [MCP servers](docs/MCP.md),
  [skills](docs/SKILLS.md), [plugins](docs/PLUGINS.md), and lifecycle
  [hooks](docs/HOOKS.md). Experimental code mode (`execute_tools`) batches
  multiple MCP/plugin calls under your normal approval rules. Optional macOS
  [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) stays off
  until explicitly reviewed and enabled.
- **Drop-in compatibility:** Reads `AGENTS.md` automatically. Opt into
  `CLAUDE.md`, `.cursorrules`, or `.github/copilot-instructions.md` via
  `project_instruction_imports` in [config.example.toml](config.example.toml).
  Import Claude Code settings and MCP servers with `/import-claude`, or install
  Claude Code plugins (skills, commands, agent profiles, MCP) and DeepSeek
  Harness (DSH) plugins with `/plugin install`
  ([details](docs/CLAUDE_PLUGIN_COMPAT.md)).

## Entry points

| Command | Purpose |
| --- | --- |
| `codewhale` | Interactive terminal app |
| `codewhale exec "…"` | Headless single-task runner for scripts and CI ([guide](docs/INSTALL.md#headless-scripts-ci); `--auto` uses Full Access unless sandboxed) |
| `codewhale web` | Local web client on `127.0.0.1` ([guide](docs/WEB.md)) |
| `codewhale review --pr N` | Advisory GitHub PR review; [GitHub bot setup](docs/GITHUB_ACTION.md#set-up-your-own-bot) runs it on your own key |
| `codewhale app-server --http` | Local HTTP [Runtime API](docs/RUNTIME_API.md) for threads, events, approvals, and per-turn diffs |
| `codewhale serve --acp` | Agent Client Protocol over stdio for editors such as Zed |

All entry points share one Rust engine ([architecture](docs/ARCHITECTURE.md)).
A community-maintained
[VS Code extension](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
([source](https://github.com/HengQuWorld/CodeWhale-VSCode)) connects to the
same engine. See the [latest release notes](https://github.com/codewhale-hq/Codewhale/releases/latest)
and [roadmap](docs/ROADMAP.md) for current release and desktop/cloud status.

## Permissions, sandboxing, and privacy

`Shift+Tab` cycles permission modes:
**Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` only
completes). **Accept Edits** (default) applies edits inside the git repository
and asks before commands. **Ask** prompts on every edit and command. **Plan**
is read-only. **Auto-Review** uses rules and a reviewer model to approve
routine calls and block high-risk ones. **Full Access** skips prompts and
disables the OS sandbox (repository rules and managed policy still apply). See
[Modes](docs/MODES.md).

In Ask, Accept Edits, and Auto-Review, commands run inside an OS sandbox where
available: Seatbelt on macOS and bubblewrap on Linux (Windows has no OS sandbox
yet). Sandboxed commands and terminal sessions cannot read credential paths
such as `~/.ssh`, `~/.aws`, or `.env` files. Run `/preview-request` to inspect
a redacted outgoing request before sending it.

API keys added with `/provider` stay on your machine, and local models require
no key. `/cost` and the footer track session spend when pricing is known (an
unknown price is shown as unknown, never as free).

Aggregate telemetry is enabled by default (version, OS, session length/outcome,
built-in provider IDs, bundled public-catalog model IDs (custom, private, or
local models map to `other`), aggregate token totals, built-in tool call/error
counts, and feature/error counters; rotating 90-day random install ID; never
code, prompts, conversations, custom/private model names, file/repo/branch
names, credentials, or IP addresses). Disable it with:

```bash
codewhale config set telemetry false
```

At startup Codewhale also checks GitHub for a new release
(`CODEWHALE_NO_UPDATE_CHECK=1` to skip) and refreshes the public `models.dev`
model catalog every 24 hours (`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` or
`[catalog] auto_refresh = false` to skip; suppressed automatically in CI and
under `DO_NOT_TRACK`). Details: [authorization order](docs/AUTHORIZATION_ORDER.md),
[sandboxing](docs/SANDBOX.md), and [telemetry](docs/TELEMETRY.md).

## Documentation

Guides follow `main` and may describe changes not yet in the latest release.

| Start here | Reference |
| --- | --- |
| [User guide](docs/GUIDE.md) | [Configuration](docs/CONFIGURATION.md) |
| [Installation](docs/INSTALL.md) | [Architecture](docs/ARCHITECTURE.md) |
| [Providers and local models](docs/PROVIDERS.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Modes and permissions](docs/MODES.md) | [Plugin authoring](docs/PLUGIN_AUTHORING.md) |
| [Keybindings](docs/KEYBINDINGS.md) | [Receipts](docs/RECEIPTS.md) |
| [GitHub pull request review](docs/GITHUB_ACTION.md) | [All documentation](docs/README.md) |

## Contributing and license

<a href="docs/CONTRIBUTORS.md#everyone"><img src="docs/assets/contributors.png" width="760" alt="Codewhale contributor wall"></a>

The [contributor wall](docs/CONTRIBUTORS.md) (`scripts/contributor-wall.py`)
tracks code, co-authored commits, carried-forward PRs, and credited reports.
Bug reports and feature proposals go to
[Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose),
questions to [Discussions](https://github.com/codewhale-hq/Codewhale/discussions),
and PR guidelines plus layout are in [CONTRIBUTING.md](CONTRIBUTING.md)
([project structure](CONTRIBUTING.md#project-structure)). Join us on
[Discord](https://discord.gg/37gfS3ksug) or add `hunterbown` on WeChat for the
Whale Brothers group.

Codewhale started as `deepseek-tui` and remains backward-compatible with its
config and sessions. It is independently maintained and not affiliated with
DeepSeek or any other model provider.

[MIT](LICENSE). Third-party notices: [vendored sources](THIRD_PARTY_NOTICES.md)
and [adapted designs](docs/THIRD_PARTY_NOTICES.md).
