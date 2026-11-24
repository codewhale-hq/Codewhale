#!/usr/bin/env bash

# Crates published for each codewhale release, in dependency order.
release_crates=(
  codewhale-build-support
  codewhale-mcp
  codewhale-paths
  codewhale-app-server-protocol
  codewhale-protocol
  codewhale-ratatui
  codewhale-sanitize
  codewhale-release
  codewhale-secrets
  codewhale-state
  codewhale-workflow
  codewhale-workflow-js
  codewhale-execpolicy
  codewhale-hooks
  codewhale-tools
  codewhale-error-taxonomy
  codewhale-config
  codewhale-agent-contract
  codewhale-cloud-facts
  # Path+version dependency of cli/tui — must publish before those crates.
  codewhale-telemetry
  codewhale-lane
  codewhale-core
  # Shared command shapes depend on protocol, never on core/runtime services.
  codewhale-command-contract
  codewhale-commands
  # TUI support crates added in 0.9.13: localization (i18n), models (catalog
  # facade), palette (design tokens). Their Engine, host and TUI consumers
  # publish after core/config/build-support and these prerequisites.
  codewhale-localization
  codewhale-models
  codewhale-palette
  # Scoped memory store; tui's native memory backend. No workspace deps.
  codewhale-memory
  # Shared helpers used by runtime, snapshot, plugins, skills, lsp, tui and
  # dependencies, so it must publish before all of them.
  codewhale-util
  # Headless runtime split out of the TUI (docs/design/TUI_DECONSTRUCTION.md).
  # Depends on config, core, memory, models and util; the published tui depends
  # on it, so it publishes after those and before tui.
  codewhale-runtime
  codewhale-net
  codewhale-notify
  codewhale-account-cli
  codewhale-sandbox
  codewhale-dependencies
  codewhale-snapshot
  codewhale-lsp
  codewhale-repl
  codewhale-skill-install
  codewhale-plugins
  codewhale-skills
  codewhale-tui-kit
  codewhale-engine
  codewhale-app-server
  codewhale-tui
  codewhale-cli
)

crate_index_path() {
  local name="$1"
  case ${#name} in
    1) echo "1/${name}" ;;
    2) echo "2/${name}" ;;
    3) echo "3/${name:0:1}/${name}" ;;
    *) echo "${name:0:2}/${name:2:2}/${name}" ;;
  esac
}

crate_index_has() {
  local body
  body="$(curl -fsSL -A "CodeWhale release (https://github.com/codewhale-hq/CodeWhale)" \
    "https://index.crates.io/$(crate_index_path "$1")" 2>/dev/null)" || return 1
  grep -qF "\"vers\":\"$2\"" <<<"${body}"
}

crates_io_retry_after() {
  python3 -c '
import email.utils, re, sys
match = re.search(r"try again after (\w{3}, \d{1,2} \w{3} \d{4} \d{2}:\d{2}:\d{2} GMT)", sys.stdin.read())
if not match:
    sys.exit(1)
try:
    print(int(email.utils.parsedate_to_datetime(match.group(1)).timestamp()))
except (TypeError, ValueError):
    sys.exit(1)
'
}
