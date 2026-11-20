# Workspace crates

Generated from each `crates/*/Cargo.toml` by
`python3 scripts/docs/check-crates-reference.py --write`. Do not edit the
table by hand; change the crate's `description` and regenerate. How the crates
fit together is in [Architecture](../ARCHITECTURE.md).

| Crate | Path | What it is |
| --- | --- | --- |
| `codewhale-account-cli` | `crates/account-cli` | Codewhale account, cloud computer and dispatch commands for the CLI |
| `codewhale-agent-contract` | `crates/agent-contract` | Fleet role, child authority and worker runtime profile contract shared by the engine and its clients |
| `codewhale-app-server` | `crates/app-server` | App-server transport for Codewhale runtime integrations |
| `codewhale-app-server-protocol` | `crates/app-server-protocol` | Client wire contracts and schema exports for Codewhale App Server |
| `codewhale-build-support` | `crates/build-support` | Shared build-script helpers for embedding Codewhale build metadata |
| `codewhale-cli` | `crates/cli` | Agentic terminal facade for open-source and open-weight coding models |
| `codewhale-cloud-facts` | `crates/cloud-facts` | Fetch + verified disk cache for the Codewhale cloud facts channel (facts/v1) |
| `codewhale-command-contract` | `crates/command-contract` | Prototype command capability and dispatch shapes for the staged TUI command extraction |
| `codewhale-commands` | `crates/commands` | Portable command handlers and diagnostic reports for Codewhale |
| `codewhale-config` | `crates/config` | Config schema and precedence model for Codewhale |
| `codewhale-core` | `crates/core` | Core runtime boundaries for Codewhale |
| `codewhale-dependencies` | `crates/dependencies` | Codewhale external tool resolution and bounded subprocess probes |
| `codewhale-engine` | `crates/engine` | Codewhale engine: turn loop, tools, agents, context, sessions and model clients |
| `codewhale-error-taxonomy` | `crates/error-taxonomy` | Shared error categories, severities, and envelopes for Codewhale |
| `codewhale-execpolicy` | `crates/execpolicy` | Execution policy and approval model for Codewhale |
| `codewhale-hooks` | `crates/hooks` | Webhook and notification delivery support for Codewhale |
| `codewhale-lane` | `crates/lane` | Lane registry and Runtime backends for Codewhale workflow instances |
| `codewhale-localization` | `crates/localization` | Localization registry for Codewhale's user-facing strings. |
| `codewhale-lsp` | `crates/lsp` | Codewhale language server client and diagnostics |
| `codewhale-mcp` | `crates/mcp` | Bounded MCP guidance normalization for Codewhale |
| `codewhale-memory` | `crates/memory` | Local, scoped, provenance-bearing memory and resumable state for CodeWhale |
| `codewhale-models` | `crates/models` | Provider request/response models and intrinsic metadata projections for Codewhale. |
| `codewhale-net` | `crates/net` | Codewhale outbound network policy, guarded destinations and proxy resolution |
| `codewhale-notify` | `crates/notify` | Shared Codewhale notification policy and local audio dispatch |
| `codewhale-palette` | `crates/palette` | Colour tokens, themes, and contrast math for Codewhale's terminal UI. |
| `codewhale-paths` | `crates/paths` | User-scoped runtime path authority for Codewhale |
| `codewhale-plugins` | `crates/plugins` | Plugin manifests, discovery, review, install and marketplace for Codewhale |
| `codewhale-protocol` | `crates/protocol` | App-server protocol frames for Codewhale runtime integrations |
| `codewhale-release` | `crates/release` | Shared Codewhale release discovery and version comparison helpers |
| `codewhale-repl` | `crates/repl` | Codewhale Python REPL runtime; the caller supplies the sandboxed command |
| `codewhale-runtime` | `crates/runtime` | Headless Codewhale runtime, split out of the terminal UI one module at a time; no terminal UI dependency |
| `codewhale-sandbox` | `crates/sandbox` | Codewhale command sandbox preparation and platform enforcement |
| `codewhale-sanitize` | `crates/sanitize` | Pure redaction and output sanitization without credential storage |
| `codewhale-secrets` | `crates/secrets` | Secret storage backends for Codewhale, with OS keyring and file fallback, plus the shared pure secret-redaction and output-sanitization primitives |
| `codewhale-skill-install` | `crates/skill-install` | Codewhale community-skill installer, package digests and skill frontmatter |
| `codewhale-skills` | `crates/skills` | Skill discovery, registry, mutation and bundled skills for Codewhale |
| `codewhale-snapshot` | `crates/snapshot` | Codewhale pre/post-turn workspace snapshots in a side git repo |
| `codewhale-state` | `crates/state` | Session/thread persistence and recovery model for Codewhale |
| `codewhale-telemetry` | `crates/telemetry` | Anonymous, user-disableable product usage counting for Codewhale |
| `codewhale-tools` | `crates/tools` | Tool invocation lifecycle, schema validation, and scheduler parallelism for Codewhale |
| `codewhale-tui` | `crates/tui` | Terminal UI for open-source and open-weight coding models |
| `codewhale-tui-kit` | `crates/tui-kit` | Terminal rendering kit for Codewhale: markdown, diff, text, scrolling, paste and link helpers. |
| `codewhale-util` | `crates/util` | Codewhale shared utilities, confined workspace files, path identity and file locks |
| `codewhale-workflow` | `crates/workflow` | Typed Workflow IR and validation for Codewhale |
| `codewhale-workflow-js` | `crates/workflow-js` | Dynamic Workflow runtime: sandboxed rquickjs scripts that dispatch Codewhale subagents |
