<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**An open-source coding agent for your terminal that works with any model.**

Give Codewhale a task and it reads the repository, edits files, and runs
commands until the task is done or it needs input from you. It works with more
than 40 built-in providers, DeepSeek V4.1 Flash by default, and with models running on
your own machine. `/receipts` lists the files it changed, the commands it ran,
and the web and MCP calls it made.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Website](https://codewhale.net) · [Documentation](docs/README.md) · [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md)

[简体中文](docs/i18n/README.zh-CN.md) · [日本語](docs/i18n/README.ja-JP.md) · [Tiếng Việt](docs/i18n/README.vi.md) · [Bahasa Indonesia](docs/i18n/README.id.md) · [한국어](docs/i18n/README.ko-KR.md) · [Español](docs/i18n/README.es-419.md) · [Português](docs/i18n/README.pt-BR.md) · [Русский](docs/i18n/README.ru.md) · [Українська](docs/i18n/README.uk.md) · [Français](docs/i18n/README.fr.md) · [Deutsch](docs/i18n/README.de.md) · [繁體中文](docs/i18n/README.zh-TW.md) · [हिन्दी](docs/i18n/README.hi.md) · [Türkçe](docs/i18n/README.tr.md) · [Italiano](docs/i18n/README.it.md) · [Polski](docs/i18n/README.pl.md) · [العربية](docs/i18n/README.ar.md) · [Català](docs/i18n/README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="A Codewhale terminal session" width="760">

<sub>A 0.10.2 development build with a scripted test model, in Full Access inside a throwaway workspace. The edit, three tests and a sub-agent review ran for real.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — +30% DeepSeek v4.1 Flash usage" width="760">
  </a>
</p>

[View the Codewhale × OrcaRouter offer](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale earns a referral fee from OrcaRouter.

## Install

macOS and Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

The installer puts checksum-verified `codewhale` and its short alias `codew`
in `~/.local/bin`. If your shell then says `command not found`, run the PATH
line the installer prints ([details](docs/INSTALL.md#put-it-on-your-path)).
`codewhale update` upgrades it. These commands install the latest release.
This page follows `main`, so a feature marked with a version arrives in that
release; the [changelog](CHANGELOG.md) lists what each release adds.

<details>
<summary><b>Windows, npm, Cargo, and other ways to install</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Prebuilt binaries cover Linux, macOS, and Windows on x64 and arm64. Nix,
Homebrew on Linux, Android/Termux (preview), and manual downloads are in the
[installation guide](docs/INSTALL.md), and containers in
[Docker](docs/DOCKER.md). Stick to one install method per machine; otherwise
two copies end up on `PATH` and whichever comes first wins.

</details>

## Quickstart

1. Commit or stash your work, then run `codewhale` from the root of a git
   repository. By default it applies file edits there without asking and
   shows you the diff.
2. Run `/provider` (or press `F3`), pick a provider, paste its API key, and
   choose a model. DeepSeek is the default provider. With no key, pick a local
   runtime instead. If no provider is configured and Ollama is already serving
   a chat model, Codewhale uses it automatically; from 0.10.2 it offers it when
   you send your first message.
3. Type a task:

```text
Fix the failing tests and explain what changed.
```

Read-only commands run straight away. For any other shell command Codewhale
asks first: `y` runs it once, `a` allows commands like it for the rest of the
conversation, `n` refuses. `/undo` takes back your last request and its file
changes. `/help` lists commands and keys.

For scripts and CI, `codewhale exec` runs one task without the interface. By
default, `--auto` uses Full Access: tools run without asking, shell commands
have no Codewhale sandbox, and trust mode lets file tools write outside the
workspace. Add `--sandbox workspace-write` to keep the shell sandbox and
file-tool workspace restrictions, with trust mode off:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Modes and permissions

A fresh install starts in Work mode with the Ask permission.

| | Switch with | Options |
| --- | --- | --- |
| Mode | `Tab` (when the input is empty) or `/mode` | **Plan** reads and searches but changes nothing. **Work** edits files and runs commands. **Operate** has the same access as Work, treats your request as a goal, and keeps going until the goal is verified or it needs input from you. |
| Permission | `Shift+Tab` | **Ask** applies file edits in your repository without asking and asks before shell commands that are not read-only. **Auto-Review** lets rules and a reviewer model approve routine calls and refuses high-risk ones; switch to Ask to approve those yourself. **Full Access** never asks and turns off Codewhale's sandbox; repository rules and managed policy still apply. |

See the [modes and permissions guide](docs/MODES.md) for the details.

## What it does

### New in 0.10.2

- `/pet on` turns your terminal into an animated whale that shows what the
  agent is doing, while the message box stays live. Its 980 particles spell
  the current step in English, Chinese or Japanese.
- `/workbar terminal` shows the shells the agent is running as they run. Type
  into one, or press `Ctrl+N` for a shell of your own (macOS and Linux).
- `/undo` takes back your whole last request in one step, including every file
  it changed, and `/diff` works in folders without Git.
- When a plan is ready, Codewhale asks whether to start Work with Ask or
  Auto-Review, or keep planning.
- `/color` gives each chat its own composer color, saved with the session.
- `web_search` can use DeepSeek's new Search API: set
  `[search] provider = "deepseek"` with a DeepSeek key.
- Outside Full Access, a `curl` or `wget` that would upload a credential never
  runs without your approval, and a command that hides invisible Unicode is
  refused. `write` no longer replaces a file the agent has not read, or one
  that changed after it read it.

### Models

More than 40 providers are built in, including Anthropic, OpenAI, DeepSeek,
Google, Mistral, Moonshot, OpenRouter, and xAI. Any OpenAI-compatible endpoint
works, and so do local models through Ollama, vLLM, or SGLang. The default is
DeepSeek's `deepseek-flash`, which DeepSeek moved to DeepSeek-V4.1-Flash-1017
on 2026-10-17; 0.10.1 runs it without an update
([details](docs/PROVIDERS.md#deepseek-v41-flash)).

`/model` switches models between turns, and `/model auto` picks one for each
turn (`/router` sets up how). `/turn inspect` shows which model a turn used,
what it cost and how long it took. `codewhale auth chatgpt` lets you use a
ChatGPT plan instead of an API key (experimental). See
[Providers](docs/PROVIDERS.md).

### Receipts and undo

`/receipts`, or `codewhale receipts --last` after you quit, lists the files
changed, commands run, web and MCP calls, agents started, approvals and who
gave them, and failures. `/undo` takes back your last request and the files it
changed, in one step and without trust mode. `/restore`
rolls the whole workspace back to an earlier snapshot; because that overwrites
later edits, it needs `/trust on` or Full Access. See [Receipts](docs/RECEIPTS.md).

Exec runs that offer tools are saved in every output format, so
`codewhale receipts --last` works after `exec --auto`, including `--json`.
Answer-only text and `--json` runs are saved only when resuming or when
conversation compaction starts; use `--output-format stream-json` to always
save them for receipts or `exec --continue`.

### Longer work

`/goal` sets an objective that carries across turns. Codewhale can split work
across [sub-agents](docs/SUBAGENTS.md), run a [fleet](docs/FLEET.md) of agents,
or follow a [workflow](docs/WORKFLOW_AUTHORING.md) checked into the
repository.

### Coming from another agent

Codewhale reads `AGENTS.md` on its own. Instruction files written for other
tools, such as `CLAUDE.md`, `.cursorrules`, or
`.github/copilot-instructions.md`, load only when you opt in with
`project_instruction_imports = ["claude"]` (or `"cursor"`, `"cline"`,
`"windsurf"`, `"copilot"`, `"all"`) in your config; see
[config.example.toml](config.example.toml). Codewhale warns when it finds one
it did not load.

`/import-claude` prepares an import of your Claude Code settings and MCP
servers for you to review. `/plugin install` accepts Claude Code plugins
containing skills, commands, agent profiles, and MCP servers, and refuses
plugins that need hooks or LSP servers
([details](docs/CLAUDE_PLUGIN_COMPAT.md)).

### Extensions

Connect [MCP servers](docs/MCP.md), add [skills](docs/SKILLS.md) and
[plugins](docs/PLUGINS.md), and run [hooks](docs/HOOKS.md) on session and tool
events. Code mode (experimental, on by default) lets the model write one
`execute_tools` program that calls several MCP and plugin tools; each call
still meets your approval rules and keeps its receipt. The independently installed Computer
Use plugin (experimental, macOS only) can observe
and operate other apps. It stays off until you review its access and enable it
([guide](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Ways to run it

| Command | What it does |
| --- | --- |
| `codewhale` | The interactive terminal app |
| `codewhale exec "…"` | One task without the interface. On its own it answers without using tools; `--auto` enables tools with Full Access by default, `--sandbox workspace-write` restricts them, and `--output-format stream-json` streams events as JSON |
| `codewhale web` | A local web UI on `127.0.0.1` ([guide](docs/WEB.md)) |
| `codewhale review --pr N` | An advisory review of a GitHub pull request. The [GitHub bot setup](docs/GITHUB_ACTION.md#set-up-your-own-bot) generates a pinned workflow for automatic reviews and `@codewhale review`, using your own model key and optional App identity |
| `codewhale app-server --http` | The [Runtime API](docs/RUNTIME_API.md), a local HTTP API for threads, events, approvals, and each turn's file changes |
| `codewhale serve --acp` | The Agent Client Protocol over stdio, for editors such as Zed |

The terminal app, `exec`, `web`, and the Runtime API share one engine
([architecture](docs/ARCHITECTURE.md)). A community-maintained
[VS Code extension](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
connects to it from the sidebar ([source](https://github.com/HengQuWorld/CodeWhale-VSCode)).
A native desktop app built on the Runtime API is in development; it has no
public download yet.

## Cost and accounts

Codewhale is free and MIT licensed. With your own API key you pay the provider
directly; local models need no key. The footer and `/cost` show what a session
has cost when the price is known, and an unknown price is shown as unknown,
never as free.

You don't need an account. Keys you add with `/provider` stay on your machine.

## Safety and privacy

In Ask and Auto-Review, commands run in an OS sandbox where the system has
one: Seatbelt on macOS, and bubblewrap on Linux when it is installed.
Windows has no OS sandbox yet, and Full Access turns the sandbox off. Inside
the sandbox, shell commands cannot read credential stores such as `~/.ssh`,
`~/.aws` and `.env` files; from 0.10.2 the same rule covers terminal sessions
and more file tools. `/preview-request` shows a redacted preview of the next
request without sending it.

Telemetry is on by default. It sends aggregate counts (version and platform,
session length and outcome, which built-in providers you used, feature and
error counts) with a random install ID that rotates every 90 days, and never
conversations, code, prompts, model names, file or repository names, or
credentials. `codewhale config set telemetry false` turns
it off. At startup Codewhale also
checks GitHub for a new release; set `CODEWHALE_NO_UPDATE_CHECK=1` to skip
that. More in [authorization order](docs/AUTHORIZATION_ORDER.md),
[sandboxing](docs/SANDBOX.md), and [telemetry](docs/TELEMETRY.md).

## Documentation

| Start here | Reference |
| --- | --- |
| [Installation](docs/INSTALL.md) | [Configuration](docs/CONFIGURATION.md) |
| [Providers and local models](docs/PROVIDERS.md) | [Architecture](docs/ARCHITECTURE.md) |
| [Modes and permissions](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Keybindings](docs/KEYBINDINGS.md) | [Plugin authoring](docs/PLUGIN_AUTHORING.md) |
| [GitHub PR review](docs/GITHUB_ACTION.md) | [All documentation](docs/README.md) |

## Community

If a provider is missing or a workflow is awkward,
[open an issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) or
[send a pull request](CONTRIBUTING.md). Contributors keep credit for the work
that lands, and the [repository layout](CONTRIBUTING.md#project-structure)
shows where things live.

Chat on [Discord](https://discord.gg/37gfS3ksug), or add the maintainer,
Hunter, on WeChat (`hunterbown`) and ask to join the Whale Brothers group.

## History and license

Codewhale started as `deepseek-tui` and still reads that project's
configuration and sessions. It is independently maintained and not affiliated
with DeepSeek or any other model provider. Thanks to
[everyone who has contributed](docs/CONTRIBUTORS.md).

[![Codewhale contributors](docs/assets/contributors.png)](docs/CONTRIBUTORS.md#everyone)

[MIT](LICENSE). Code adapted from other open-source projects is listed in the
[third-party notices](docs/THIRD_PARTY_NOTICES.md).
