#!/bin/sh
# Keep retired compatibility names out of customer-facing TUI documentation
# and messages. Parser aliases and internal enum names intentionally remain.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

docs='docs/MODES.md docs/KEYBINDINGS.md docs/GUIDE.md docs/CONFIGURATION.md docs/FLEET.md'
if grep -En 'YOLO|Multitask|/mode yolo|--yolo|Bypass' $docs; then
  printf '%s\n' 'retired TUI vocabulary remains in current product documentation' >&2
  exit 1
fi

# Locale message identifiers are internal compatibility names and may still
# contain retired terms (for example `PermissionsPostureBypass`). Inspect only
# the serialized user-facing value so the check enforces the contract above
# without flagging an allowed key.
if grep -En '": "[^"]*(YOLO|Multitask|Bypass)' crates/localization/locales/*.json; then
  printf '%s\n' 'retired TUI vocabulary remains in localized product copy' >&2
  exit 1
fi

# Founder decision 2026-09-01: `fleet` is the public product term for the
# assembled model team; `Pod` is retired from customer-facing copy. The
# English source locale is the gate; other locale packs migrate with their
# translation sweep. `/pod` and `codewhale pod` remain parser aliases.
if grep -En '": "[^"]*[Pp]od' crates/localization/locales/en.json; then
  printf '%s\n' 'retired Pod vocabulary remains in English product copy' >&2
  exit 1
fi

if grep -En \
  'YOLO mode is deprecated|/mode yolo|Bypass permissions|agent · yolo|Plan → Act → Multitask' \
  crates/tui/src/tui/app.rs \
  crates/runtime/src/config.rs \
  crates/tui/src/commands/groups/config/config.rs \
  crates/tui/src/commands/groups/skills/restore.rs \
  crates/tui/src/prompts/text.rs; then
  printf '%s\n' 'retired TUI vocabulary remains in current product messages' >&2
  exit 1
fi

if grep -Ein 'bypass approvals|Act \+ bypass' crates/tui/src/tui/ui.rs; then
  printf '%s\n' 'retired permission wording remains in live Plan UI' >&2
  exit 1
fi

if grep -En 'mode-derived-yolo-bypass|YOLO derives bypass|settings\.default_mode:.*yolo' \
  crates/tui/src/tui/setup/mod.rs; then
  printf '%s\n' 'retired permission wording remains in live Setup UI' >&2
  exit 1
fi

if grep -En 'Agent or Yolo|Plan/Agent/Yolo' \
  crates/tui/src/core/engine/tool_catalog.rs crates/tui/src/commands/groups/core/hooks.rs; then
  printf '%s\n' 'retired mode wording remains in tool or hooks help' >&2
  exit 1
fi
