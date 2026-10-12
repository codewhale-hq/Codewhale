<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**The open-source coding agent that works with any model.**

Codewhale reads your project, edits files, runs commands, and checks its own
work — in your terminal, with a hosted or local model you choose.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Website](https://codewhale.net) · [Documentation](docs/README.md) · [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md)

[简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="A Codewhale terminal session" width="760">

<sub>Real Codewhale terminal capture from a configured local demo session.</sub>

</div>

## Install

macOS and Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

The installer downloads checksum-verified binaries into `~/.local/bin`. If
`codewhale` then says "command not found", run the one PATH line the installer
prints, or see [Put it on your PATH](docs/INSTALL.md#put-it-on-your-path).
Upgrade any time with `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo, and other routes</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew on Linux, Android/Termux, manual downloads with checksum
verification, and the optional CNB mirror are covered in the
[installation guide](docs/INSTALL.md). Pick one route: several installs on one
machine end up fighting over `PATH`.

</details>

## Quickstart

**Your provider keys, managed in one place.** [Create a Codewhale account](https://app.codewhale.net/register)
or run `codewhale login` to sign in. Save and update your API keys in your
account, then use them through the Codewhale model route on your signed-in
devices. [Set up account keys](docs/CONFIGURATION.md#account-provider-keys).
Signing in does not upload existing local keys; local use remains available
without an account.

1. **Open your project.** Run `codewhale` in the folder you want to work on.
2. **Connect a model.** Run `/provider` (or press `F3`) to add a hosted key or
   pick a local runtime. If Ollama is already running with a chat model,
   Codewhale switches to it on its own. Use `/model` to change models.
3. **Give it a concrete task.**

```text
Fix the failing tests and explain what changed.
```

The same task runs headless from a script or CI job:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Run `/help` for commands and keyboard shortcuts.

## Ways to run it

Every client drives the same local [Codewhale Engine](docs/ARCHITECTURE.md), so
sessions, tools, and permissions behave the same everywhere.

| Command | What it does |
| --- | --- |
| `codewhale` | The interactive terminal interface |
| `codewhale exec "…"` | One headless turn from a script or CI; `--auto` allows file and shell tools, `--output-format stream-json` streams JSON |
| `codewhale web` | The bundled [local browser client](docs/WEB.md) on `127.0.0.1` |
| `codewhale review --pr N` | An advisory [pull request review](docs/GITHUB_ACTION.md); posting is opt-in |
| Runtime API | A [local HTTP API](docs/RUNTIME_API.md) for threads, events, and approvals |

A native desktop app (GPUI) is being built as the signed-in product client; see
the [product page](https://codewhale.net/en/product) for availability. The
community-maintained [VS Code extension](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
connects to the same Engine from a sidebar ([source](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## What it does

- **Any model, no lock-in.** Over 40 built-in provider routes — Anthropic,
  DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI and more — plus
  any OpenAI-compatible endpoint and local models through Ollama, vLLM, or
  SGLang. [Providers](docs/PROVIDERS.md)
- **You stay in control.** Plan mode explores without changing anything; Work
  and Operate make changes. Approval postures decide when a tool call needs your
  OK, `/undo` and `/restore` recover workspace changes, and `/receipts` lists
  every file, command, and approval in a session. [Modes](docs/MODES.md) ·
  [Receipts](docs/RECEIPTS.md)
- **Built for long jobs.** Set a durable `/goal`, delegate bounded work to
  [sub-agents](docs/SUBAGENTS.md), run supervised [agent teams](docs/FLEET.md)
  with a pre-spend check, or script them as checked-in
  [workflows](docs/WORKFLOW_AUTHORING.md).
- **Extend what you already use.** Connect [MCP servers](docs/MCP.md), install
  [skills](docs/SKILLS.md) and [plugins](docs/PLUGINS.md), run
  [hooks](docs/HOOKS.md) on session and tool events, and load existing
  [Claude Code plugins](docs/CLAUDE_PLUGIN_COMPAT.md).
- **Computer Use.** An included plugin adds tools for observing and operating
  other applications. Review its access and enable it before use.
  [Guide](crates/tui/plugins/computer-use/README.md)

## Modes and permissions

| | Choose with | Options |
| --- | --- | --- |
| **Mode** — what the agent is doing | `Tab` or `/mode` | Plan (explore, no changes) · Work (edit and run) · Operate (drive a goal through planned, verified steps) |
| **Posture** — when it asks first | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access still respects hard policy boundaries. The
[modes and permissions guide](docs/MODES.md) explains each option.

## Safety

Codewhale runs on your machine with the access you grant it. Approval postures
and repository rules limit what the agent may do, and commands run inside an OS
sandbox where supported (Seatbelt on macOS; bubblewrap on Linux when installed and working).
`/preview-request` shows the exact redacted request before anything is sent.
Unknown model prices stay unknown instead of being reported as free.

See [authorization order](docs/AUTHORIZATION_ORDER.md),
[sandboxing](docs/SANDBOX.md), and [telemetry](docs/TELEMETRY.md) — usage
counts are on by default and `codewhale config set telemetry false` turns them
off.

## Documentation

| Start here | Go deeper |
| --- | --- |
| [Installation](docs/INSTALL.md) | [Configuration](docs/CONFIGURATION.md) |
| [Providers and local models](docs/PROVIDERS.md) | [Architecture](docs/ARCHITECTURE.md) |
| [Modes and permissions](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Keybindings](docs/KEYBINDINGS.md) | [Plugin authoring](docs/PLUGIN_AUTHORING.md) |
| [GitHub PR review](docs/GITHUB_ACTION.md) | [All documentation](docs/README.md) |

## Community

Bug reports, feature ideas, and pull requests are welcome — whether you have
used Codewhale for months or are trying it for the first time. If a provider is
missing or a workflow is awkward,
[open an issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) or
[send a pull request](CONTRIBUTING.md). First contributions are welcome, and
contributors keep credit for the work that lands. The
[repository layout](CONTRIBUTING.md#project-structure) is a good place to start.

Join the [Discord](https://discord.gg/37gfS3ksug), or add Hunter on WeChat
(`hunterbown`) and ask to join the Whale Brothers group.

## History and license

Codewhale began as `deepseek-tui` and still reads that project's configuration
and sessions. It is now provider-neutral and independently maintained, and is
not affiliated with any model provider. Thanks to
[every contributor](docs/CONTRIBUTORS.md) and to the open-source communities
that helped it grow.

[MIT](LICENSE). Portions adapted from other open-source projects are recorded
in [third-party notices](docs/THIRD_PARTY_NOTICES.md).
