#!/usr/bin/env bash

# Crates published for each codewhale release, in dependency order.
release_crates=(
  codewhale-build-support
  codewhale-mcp
  codewhale-paths
  codewhale-protocol
  codewhale-release
  codewhale-sanitize
  codewhale-secrets
  codewhale-state
  codewhale-workflow
  codewhale-workflow-js
  codewhale-execpolicy
  codewhale-hooks
  codewhale-tools
  codewhale-config
  codewhale-agent-contract
  codewhale-cloud-facts
  # Path+version dependency of cli/tui — must publish before those crates.
  codewhale-telemetry
  codewhale-lane
  codewhale-agent
  codewhale-core
  # Shared command shapes depend on protocol, never on core/runtime services.
  codewhale-command-contract
  codewhale-commands
  # TUI support crates added in 0.9.13: localization (i18n), models (catalog
  # facade), palette (design tokens). Only tui consumes them, so they sit
  # after core/config/build-support and before tui.
  codewhale-localization
  codewhale-models
  codewhale-palette
  # Scoped memory store; tui's native memory backend. No workspace deps.
  codewhale-memory
  # Headless runtime split out of the TUI (docs/design/TUI_DECONSTRUCTION.md).
  # Depends on config, core, memory and models; the published tui depends on
  # it, so it publishes after those and before tui.
  codewhale-runtime
  codewhale-net
  codewhale-notify
  codewhale-app-server
  codewhale-sandbox
  codewhale-util
  codewhale-dependencies
  codewhale-snapshot
  codewhale-lsp
  codewhale-repl
  codewhale-skill-install
  codewhale-tui
  codewhale-cli
)
