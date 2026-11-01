# Rebrand: DeepSeek TUI → Codewhale

> 阅读简体中文版：[zh_hans/REBRAND.md](zh_hans/REBRAND.md)。

Starting with **v0.8.41**, this project ships under a new name: `codewhale`.

This document explains what changed, what didn't, and how to migrate. None of the
DeepSeek provider integration changed — only the local CLI / TUI brand.

## TL;DR

```bash
# 1. Uninstall the old wrapper or binaries.
npm uninstall -g deepseek-tui      # or:
cargo uninstall deepseek-tui-cli 2>/dev/null || true
cargo uninstall deepseek-tui 2>/dev/null || true
                                    # Homebrew:
                                    # brew upgrade codewhale

# 2. Install under the new name.
npm install -g codewhale            # or:
cargo install codewhale-cli --locked
                                    # Homebrew:
                                    # brew tap Hmbown/deepseek-tui
                                    # brew install codewhale

# 3. Run with the new command.
codewhale doctor
codewhale
```

Your existing `~/.deepseek/config.toml`, `~/.deepseek/sessions/`,
`~/.deepseek/skills/`, `~/.deepseek/tasks/`, and `~/.deepseek/mcp.json` are
not deleted. New Codewhale installs prefer `~/.codewhale/`, and legacy
`~/.deepseek/` state remains a read fallback while you migrate. Existing
`DEEPSEEK_*` environment variables continue to work.

## What got renamed

| Surface | Before | After |
|---|---|---|
| Installed commands | `deepseek` / `deepseek-tui` | `codewhale` / `codew` |
| npm wrapper package | `deepseek-tui` | `codewhale` |
| Crates.io crates | `deepseek-tui-cli` / `deepseek-tui` / `deepseek-*` | `codewhale-cli` / `codewhale-tui` / `codewhale-*` |
| Release assets | `deepseek-<platform>` / `deepseek-tui-<platform>` | `codewhale-<platform>` / `codew-<platform>`; `codewhale-tui-<platform>` remains a compatibility-only filename |
| Checksum manifest | `deepseek-artifacts-sha256.txt` | `codewhale-artifacts-sha256.txt` |

## What changed for local state

New installs write product-owned state under `~/.codewhale/`. Existing
`~/.deepseek/` config, sessions, skills, tasks, MCP config, memory, and notes
remain readable as legacy fallbacks while you migrate. Codewhale never deletes
the legacy directory automatically.

## What did NOT change

Anything that targets the DeepSeek provider API stays exactly as it was:

- **Environment variables**: `DEEPSEEK_API_KEY`, `DEEPSEEK_BASE_URL`,
  `DEEPSEEK_MODEL`, `DEEPSEEK_PROVIDER`, `DEEPSEEK_PROFILE`,
  `DEEPSEEK_LOG_LEVEL`, plus the existing `DEEPSEEK_TUI_*` runtime knobs
  (`DEEPSEEK_TUI_BIN`, `DEEPSEEK_TUI_RELEASE_BASE_URL`, etc.). They're kept
  for backward compatibility; renaming them would break every shell rc on
  the planet.
- **`DEEPSEEK_YOLO`**: now deprecated, but still read as an alias of
  `CODEWHALE_YOLO` through 0.9.x so existing scripts keep working (when both
  are set, `CODEWHALE_YOLO` wins). It is removed in 0.10 (#5443); use
  `CODEWHALE_YOLO` in new scripts.
- **Model IDs**: `deepseek-v4-pro`, `deepseek-v4-flash`, and the legacy
  aliases `deepseek-chat` and `deepseek-reasoner`.
- **Hosts**: `api.deepseek.com` (global). The legacy typo host
  `api.deepseeki.com` is not an official DeepSeek endpoint; it is only
  still accepted in URL heuristics for existing configs and is not
  offered as a fallback (#1079).
- **GitHub repository URL**: `https://github.com/codewhale-hq/CodeWhale`.
  The old `Hmbown/DeepSeek-TUI` URL redirects there during the transition.
- **Homebrew tap and formula**: the formula is `codewhale`. The tap GitHub
  repo is still `Hmbown/homebrew-deepseek-tui` until it is renamed;
  `brew tap Hmbown/deepseek-tui && brew install codewhale` is the current
  path. The legacy `deepseek-tui` formula remains a deprecated alias for
  one overlap release.
- **Docker image**: `ghcr.io/codewhale-hq/codewhale`.

## Deprecation shims (removed in v0.9.0)

To keep existing shell aliases, scripts, and CI working through the rename,
v0.8.41 and later v0.8.x releases shipped **deprecation shims**:

- A `deepseek` binary that prints a one-line warning to stderr and forwards
  argv to `codewhale`.
- A `deepseek-tui` binary that does the same for `codewhale-tui`.
- The legacy `deepseek-tui` npm package is deprecated and no longer receives
  new releases. Install the `codewhale` npm package instead.

These binary shims are removed in **v0.9.0**. DeepSeek provider support, model
IDs, `DEEPSEEK_*` environment variables, and legacy `~/.deepseek/` state
fallbacks remain supported.

A `deepseek` or `deepseek-tui` shim from those releases can still be on a
machine (its warning still names v0.9.0). `codewhale doctor --installs` lists
it with its owner's removal command, and `codewhale doctor --fix` moves stray
backup copies of it to the recovery folder.

## Migrating in practice

### npm

```bash
npm uninstall -g deepseek-tui
npm install -g codewhale
```

### Cargo

```bash
cargo uninstall deepseek-tui-cli 2>/dev/null || true
cargo uninstall deepseek-tui 2>/dev/null || true
cargo install codewhale-cli --locked
```

Or in a checkout:

```bash
cargo install --path crates/cli --locked --force
```

Cargo installs the canonical `codewhale` command. Release/npm/Homebrew
installers also provide the byte-identical `codew` short name; Cargo users can
add an optional `codew` symlink beside `codewhale`.

### Legacy `deepseek update`

Current v0.8.x compatibility binaries recognize when they are running under a
legacy `deepseek` or `deepseek-tui` filename. In that case, `deepseek update`
or `deepseek-tui update` downloads the canonical Codewhale release assets and
installs them beside the legacy binary as `codewhale` and `codewhale-tui` when
the install directory is writable. That describes the historical v0.8
compatibility updater, not the current install surface; after upgrading, use
`codewhale` or `codew`.

If that update path cannot write to the install directory, use the npm, Cargo,
Homebrew, or manual reinstall commands above. The legacy npm package
`deepseek-tui` remains deprecated and is not republished; npm users should move
to `npm install -g codewhale`.

### Homebrew

**Historical migration state as of v0.9.13 (published 2026-09-14):** The
formula is `codewhale`. New installs:

```bash
brew tap Hmbown/deepseek-tui
brew install codewhale
brew upgrade codewhale
```

The tap GitHub repo is still `Hmbown/homebrew-deepseek-tui` until it is
renamed to `Hmbown/homebrew-codewhale` (then `brew tap codewhale-hq/codewhale`
works; the old tap name keeps working through GitHub's redirect). The
legacy `deepseek-tui` formula remains a deprecated alias for this overlap
release so existing `brew upgrade deepseek-tui` crontabs keep working.

**Remaining rollout:**

1. Rename the tap repo to `Hmbown/homebrew-codewhale` when adding
   `HOMEBREW_TAP_PAT`, then tell Codewhalebot.
2. After one more minor release, remove the `deepseek-tui` alias.

### Manual / GitHub Releases

`v0.8.41` through `v0.8.x` Releases attached the canonical `codewhale-*` /
`codewhale-tui-*` assets (plus `codew-*` from v0.8.66 onward) and
compatibility-only `deepseek-*` / `deepseek-tui-*` shim assets. Starting in
v0.9.0, Releases attach the current `codewhale-*` / `codew-*` assets, the
`codewhale-artifacts-sha256.txt` checksum manifest, and byte-identical
`codewhale-tui-*` compatibility filenames required by legacy update clients.
Those compatibility filenames are not a third installed command. Install or
update through `codewhale` before moving to v0.9.0.

### Sessions, skills, and manual workspaces

Renaming the binary does not require starting over:

- **Config**: on first launch, Codewhale copies `~/.deepseek/config.toml` to
  `~/.codewhale/config.toml` if the Codewhale file does not already exist.
  It never overwrites a newer Codewhale config. You can inspect the active path
  with `codewhale doctor`.
- **Sessions and tasks**: managed state is read from `~/.codewhale/...` when
  present, with `~/.deepseek/...` used as the legacy fallback when only the old
  directory exists. Existing saved sessions still appear in `codewhale sessions`
  and the TUI resume picker.
- **Skills**: Codewhale discovers workspace skills first, then global skills,
  including both `~/.codewhale/skills` and legacy `~/.deepseek/skills`. Existing
  skill directories with `SKILL.md` do not need to be rewritten.
- **MCP config**: the default path is `~/.codewhale/mcp.json`. If that file is
  absent, Codewhale still reads legacy `~/.deepseek/mcp.json`. To use a custom
  MCP config file, set `mcp_config_path` in `config.toml` or
  `DEEPSEEK_MCP_CONFIG`.
- **Manual binary installs**: keep the two current command files together on
  your `PATH`: `codewhale` and `codew`. On Windows, the
  recommended user-local location is `%LOCALAPPDATA%\Programs\CodeWhale\bin`.
  On Unix-like systems, any user-writable `PATH` directory is fine as long as
  both commands are present. Do not install a compatibility-only
  `codewhale-tui-*` release filename as a third command.
- **Specified work directories**: running `codewhale` from a project directory,
  or launching it with a specific workspace path, does not move project files.
  Codewhale reads `<workspace>/.codewhale/config.toml` first and falls back to
  legacy `<workspace>/.deepseek/config.toml` when the new path is absent.

If both `~/.codewhale/...` and `~/.deepseek/...` copies exist, the Codewhale
path wins. Keep the legacy directory until you have confirmed `codewhale
doctor`, `codewhale sessions`, and your expected skills all show the same state.

### If sessions appear missing after an upgrade

Run `codewhale doctor` before copying or deleting anything. Doctor compares
top-level session JSON **filenames and filesystem metadata only** between
`~/.deepseek/sessions/` and `~/.codewhale/sessions/`. It does not read chat
contents, traverse `checkpoints/`, or modify either directory. The JSON form
exposes the same result at `legacy_state.session_recovery`.

If doctor lists recoverable filenames:

1. Back up both session directories (if present) and close other Codewhale
   processes.
2. Run `codewhale sessions`. This invokes the existing additive migration,
   which creates only missing destination files, never overwrites a file that
   already exists under `~/.codewhale/sessions/`, skips checkpoint internals,
   and leaves every legacy original in place.
3. Rerun `codewhale doctor`, then confirm the sessions appear with `codewhale
   sessions`. If any filenames remain listed, keep both backups and report the
   listed source/destination filenames without sharing chat contents.

An explicit `CODEWHALE_HOME` intentionally isolates that home and disables the
ambient `~/.deepseek` fallback. Doctor will not inspect the ambient legacy home
in that mode. To diagnose the default home without changing the isolated one,
use a separate shell with `CODEWHALE_HOME` unset and rerun `codewhale doctor`.

## Why the name change

Codewhale is a shorter, terminal-friendlier handle for the same terminal
coding agent and the longer-term product direction: an agentic terminal for
open source and open-weight coding models, with DeepSeek — the provider the
project started with — remaining first-class alongside every other provider. The project name,
command names, package names, release assets, Docker image, and CNB mirror move
to Codewhale; the official DeepSeek provider, model IDs, env vars, and
`~/.deepseek/` config surface remain first-class.

## Reporting issues with the rename

If your install broke during the migration, please open an issue at
<https://github.com/codewhale-hq/CodeWhale/issues> and include:

- The output of `codewhale --version` (or `deepseek --version` if you're
  still on the shim).
- Which install path you used (npm, cargo, brew, manual).
- The exact command you ran and the full error output.

We'll prioritize migration regressions.
