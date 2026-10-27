# Plugin bundles

> 阅读简体中文版：[zh_hans/PLUGIN_BUNDLES.md](zh_hans/PLUGIN_BUNDLES.md)。

Codewhale supports a deliberately small plugin-bundle boundary. The boundary
was drawn in v0.9.1 and is extended deliberately in v0.9.10: a bundle may
contribute declarative Skills, MCP configuration, Commands, Agent profiles,
and Hooks through Codewhale's existing engines. Unsupported declarations stay
inventoried instead of disabling a mixed bundle. Discovery alone never
executes, enables, trusts, downloads, updates, or installs anything.

This document owns the bundle formats, discovery,
validation, and the trust/enable/runtime contract. [PLUGINS.md](PLUGINS.md)
owns how bits get onto and off disk — the `/plugin install`, `update`,
`uninstall`, and `suggest` on-ramp added in v0.9.4 (#5182). Compatible Claude Code bundles use the same native adapters; their supported subset is
[CLAUDE_PLUGIN_COMPAT.md](CLAUDE_PLUGIN_COMPAT.md).
For a runnable native example and explicit OpenCode/DSH data conversion, see
[Write your first plugin](PLUGIN_AUTHORING.md).

## Discovery and precedence

Codewhale scans only its own roots, looking in each `<name>/` directory for a
manifest named `plugin.json` (the native Agent Plugins v1.0.0 format, since
v0.9.4), `kimi.plugin.json` (the compatible Kimi Skills/MCP subset, since
v0.9.8), `plugin.toml` (the legacy Codewhale format, still fully readable), or
`.claude-plugin/plugin.json` (the compatible Claude subset, since v0.9.13):

- User: `~/.codewhale/plugins/<name>/`
- Workspace: `<workspace>/.codewhale/plugins/<name>/`

A bundle that publishes multiple formats is read through `plugin.json` first,
then `kimi.plugin.json`, then the legacy `plugin.toml`, then
`.claude-plugin/plugin.json`.
Computer Use installs from the first-party catalog as a normal user plugin.
It updates independently of the Engine and requires review and enablement.
The native Mac helper is installed separately for permission setup and the
computer-control indicator. See the [Computer Use guide](https://github.com/codewhale-hq/codewhale-cu-plugin).

User plugins take precedence over workspace plugins. Symbolic-link roots,
manifests, component paths, and nested component files fail closed.
Historical built-in snapshots and receipts remain untouched for older binaries;
the current Engine neither creates nor discovers a built-in Computer Use copy.
Install and review the catalog plugin once when upgrading from that layout.

New user and workspace bundles are always untrusted and disabled. Discovery is
read-only and does not inspect any other application's extension or credential
directories: ambient roots such as `.claude/plugins` or `.cursor/plugins` are
never scanned.

Pre-v0.9.1 `overrides.json` enablement was intentionally not imported as
trust; every bundle activates only through the content-hash and
`codewhale-plugin-capabilities-v3` activation-policy review below.

## Manifest

All supported encodings parse into the same internal manifest, so validation, hashing,
review, and runtime behavior are identical downstream. On-disk auto-migration
between them is deliberately not performed; `/plugin export <name>
<target-dir>` publishes a loaded bundle as a spec-valid Agent Plugins v1.0.0
directory without modifying the installed one.

### `plugin.json` (Agent Plugins v1.0.0)

The standard's manifest root is closed: `$schema`, `name`, and the optional
well-known fields (`version`, `description`, `author`, `homepage`,
`repository`, `license`, `keywords`, `extensions`). An unknown root key is a
parse error. Client-specific data lives under `extensions`, keyed by
reverse-domain namespace: unknown vendor namespaces are ignored, never
rejected — that is what lets a bundle authored for another client load here —
while unknown keys inside Codewhale's own `extensions["net.codewhale"]`
namespace are rejected rather than silently dropped.

Names follow the standard's rule: 1–64 lowercase ASCII letters, digits, or
internal single `-`/`.`, starting and ending alphanumeric, never `--` or `..`.
A `skills/` directory in the bundle root is picked up automatically; other
component locations, `capabilities`, `when`, and `display_name` ride in
`extensions["net.codewhale"]`.

MCP servers cannot live in `plugin.json` (the root is closed); they live in a
sibling `mcp.json` under `mcpServers`, with `stdio`, `streamable-http`, or
`sse` transports (`type` may be omitted and is inferred from `command` vs
`url`). Codewhale-only server options — timeouts, tool filters, env-backed
credentials, enablement — ride per-server under `extensions["net.codewhale"]`.
The `env` names `PLUGIN_ROOT` and `PLUGIN_DATA` are reserved by the standard
for the host runtime and are rejected in plugin definitions.

### `plugin.toml` (legacy Codewhale format)

```toml
schema_version = 1

[plugin]
name = "example"
version = "0.1.0"
description = "Example instruction and MCP bundle"
author = "Example Author"

[skills]
path = "skills"

[commands]
path = "commands"

[agents]
path = "agents"

[hooks]
path = "hooks"

[mcp_servers.local]
command = "node"
args = ["server.js"]
cwd = "mcp"

[mcp_servers.remote]
url = "https://example.invalid/mcp"

[capabilities]
network_hosts = ["example.invalid"]

[when]
os = ["macos", "linux", "windows"]
binaries = ["node"]
```

Legacy TOML names are 1–64 lowercase ASCII letters, digits, or internal
hyphens. A pre-versioned manifest without `schema_version` still parses, with
a migration warning from `/plugin validate` (and `0.0.0` displayed when
`[plugin].version` is missing). An unknown top-level table or field is a
parse error (`deny_unknown_fields`), reported by byte offset without echoing
manifest values.

### Validation (both formats)

Component paths must be relative, contained, present, and free of symbolic
links or Windows reparse points (including junctions and mount points). The v1
schema rejects unknown MCP fields, ambiguous local/remote
transport combinations, unbounded lists/timeouts, and overlapping tool
filters.

Remote MCP URLs must use HTTPS, except for explicit loopback HTTP endpoints.
They cannot contain user information, a query, or a fragment. Literal headers
are rejected: authentication must name a source environment variable through
`env_headers` or `bearer_token_env_var`. A remote bundle must declare exactly
the normalized host set used by its endpoints in
`capabilities.network_hosts`; endpoint scheme, normalized host, port, and path
remain bound to the review. Redirects are limited and must retain that exact
normalized origin. Reviewed remote transports use an explicit no-proxy HTTP
client: plugin bundles never read or use ambient `HTTP_PROXY`, `HTTPS_PROXY`,
or `NO_PROXY` values, because proxy credentials and proxy observation are
outside the reviewed authority. User-authored MCP configuration keeps its
existing explicit proxy support.

Local stdio environment entries must use exact `${SOURCE_ENV}` references.
The review shows destination and source names, but never reads or prints their
values. Plugin children inherit only Codewhale's base secret-scrubbed child
environment plus those reviewed mappings; credential-capable proxy variables
and the broader compatibility environment used by user-authored MCP
configuration are not inherited ambiently. Absolute arguments and parent
traversal are rejected; contained bundle entrypoints are frozen to their
staged paths before spawn.

Every stdio argument is shown losslessly as a JSON string during review.
Common credential-bearing flags and known literal token shapes are rejected
from argv; credentials must instead use a reviewed environment mapping.
Plugin-contributed MCP OAuth has been disabled since v0.9.1 and remains
disabled as of v0.9.6, including discovery, login, refresh, and token storage;
a manifest declaring OAuth fields on a plugin MCP server fails validation.

### Active and inactive component surfaces

Codewhale 0.9.10 activates declarative `[skills]`, `[mcp_servers.*]`,
`[commands]`, `[agents]`, and `[hooks]` components from its content-addressed
runtime snapshot. Commands use markdown command files, Agents use Fleet TOML
profiles, and Hooks use `HooksConfig` TOML files. A component may name one file
or a directory of the corresponding files. Ordinary user/workspace commands
and Agent profiles keep precedence over plugin contributions; trusted project
hooks run after plugin hooks.

The manifest can additionally inventory the following inactive surfaces.
Those declarations stay hashed, reviewed, and displayed, but do not activate
and no longer disable the whole bundle:

```toml
[lsp]           # TOML alias: [lsp_servers]
path = "lsp"

[native]        # TOML alias: [native_extension]
path = "native/index.mjs"

[capabilities]
filesystem_roots = ["workspace"]
network_hosts = ["api.example.invalid"]
lifecycle_mutation = true
```

(In a `plugin.json` bundle the same tables ride under
`extensions["net.codewhale"]`.)

The accept/reject behavior is deliberately loud, never silent:

- Compatibility is per-component: `full` when every declared surface has an
  adapter (or the bundle is empty), `partial` when supported components can
  activate beside named inactive surfaces, and `unsupported` when the bundle
  only declares surfaces Codewhale cannot activate yet. The same versioned
  activation policy (v3) drives those labels, the runtime adapters, and the
  capability hash. Executing LSP or native code must change that policy,
  which changes the capability hash and forces re-review. v1 and v2 trust
  receipts fail closed as `capabilities-changed`.
- **`native` under the experimental extension host.** With
  `[features] extension_host` on, the policy becomes v4 and `native` is an
  active adapter: each entry is one `.mjs`, `.js` or `.mts` ES module file that the
  TypeScript extension host imports. A directory or any other file reports
  an error in `/plugin validate` and review and prevents activation. Its tools
  always use `Required` approval;
  Full Access, Bypass, or an exact session grant for the reviewed build can
  satisfy that gate without a prompt. Toggling the flag
  re-reviews every plugin. See
  [the design](design/TS_EXTENSION_HOST.md#as-built-phase-1-2026-09-25).
  The [extension author guide](EXTENSIONS.md) includes a tested typed example,
  diagnostic workflow and Node's erasable-TypeScript restrictions.
- A **recognized-but-inactive** declaration (`lsp`, `native`, a non-empty
  `capabilities.filesystem_roots`, or
  `capabilities.lifecycle_mutation = true`) parses and is validated like any
  component (contained, present, link-free). It is counted in the inventory,
  hashed into the capability receipt, shown in review and `/plugin show` as
  inactive, and never executed (for `native`, only while the extension host
  flag is off). A reviewed, trusted, applicable mixed bundle
  can still be enabled: supported declarative components become active, and
  the inactive surfaces stay named as inactive.
- An **all-unsupported** bundle can be reviewed and trusted, but `/plugin
  enable` fails closed and names the inactive surfaces. There is nothing
  Codewhale can honestly activate.
- An **unrecognized** section or field is a validation failure, not an
  inventory entry: unknown top-level TOML tables, unknown MCP server fields,
  unknown `plugin.json` root keys, and unknown keys inside
  `extensions["net.codewhale"]` are all rejected outright. The single
  ignore-without-error case is another vendor's `extensions` namespace in the
  Agent Plugins format, which the standard requires clients to skip.
- `capabilities.network_hosts` is not a future surface: it is enforced today,
  and must exactly match the normalized host set of the bundle's remote MCP
  endpoints (so it cannot be declared without them, or omitted with them).

A successful environment or health check is never treated as trust.

## Review, trust, and enablement

Use the in-session command surface:

```text
/plugin list
/plugin validate example
/plugin show example
/plugin enable example
```

The first `enable` opens a review showing source, component inventory,
requested permissions, sanitized MCP endpoints, full content and capability
hashes, and inactive declarations. It also prints an exact confirmation:

```text
/plugin trust example <full-content-sha256>.<full-capability-sha256>
```

Run that exact command only after reviewing the bundle. The confirmation token
uses both complete SHA-256 receipts rather than display prefixes. The
capability receipt is the v3 digest: it still hashes the complete inventory
and also binds this build's activation policy (which adapters are executable
versus inventoried-only). Trust first
copies the complete reviewed tree into a Codewhale-owned, content-addressed
runtime snapshot and records the matching receipt; it does not activate
anything.
Then run `/plugin enable example` again. Trust and enablement are separate:

- `/plugin disable example` stops contribution while preserving trust.
- `/plugin revoke example` removes trust while preserving the enablement bit;
  the bundle remains inactive until reviewed again.
- `/plugin reload` rebuilds the current workspace registry when files have
  changed on disk.

(`/plugin install`, `update`, and `uninstall` place, replace, and remove the
bits themselves and always drop into this same review — see
[PLUGINS.md](PLUGINS.md). `/plugin suggest` ranks installed bundles and
any locally added marketplace catalogs; sending a matching task can toast the
same next step without installing anything. Nothing is written into the
model's request to advertise plugins; the full offering policy is in
[PLUGINS.md](PLUGINS.md#how-codewhale-offers-plugins).)

Trust, enable, disable, revoke, and reload rebuild the current workspace's
Skills, MCP, Commands, Agent profiles, and Hooks immediately. Each persisted
transition advances a per-bundle generation under a stable cross-process lock.
A generation change cancels in-flight MCP work, removes cached catalog
entries, terminates an idle plugin stdio child, and denies persisted queued
Skills carrying the older authority receipt.

The review distinguishes remote MCP endpoints from local stdio MCP servers.
A local stdio server is a child process running with the Codewhale user's host
filesystem and network authority; plugin trust is not an OS sandbox. The
review therefore shows the command, argument count, working directory,
environment-variable names, and this host-authority warning without printing
environment or header values. MCP tool approval still applies after the
server starts.

Trust receipts live in `~/.codewhale/plugins/state.json`. Atomic owner-only
writes record the full content hash, capability hash, reviewed capability
inventory, generation, and review time, with the latest 32 reviews retained as
a bounded audit trail. Malformed or unsupported state is not overwritten: all
bundles fail closed until the state file is repaired or moved.

The content hash covers the manifest, complete bundle tree, and executable
shape in deterministic path order, including local MCP entrypoints and
companion assets. Staging is bounded, rejects symbolic links and unsupported
file kinds (plus every Windows reparse point and hard-linked files), uses an
atomic destination swap, and applies owner-only runtime permissions or ACLs
through validated object handles on Windows. The capability hash covers the
normalized component and permission inventory. A source or staged-content
edit, capability change, or unsafe runtime-root replacement invalidates the
receipt deterministically; an already-enabled bundle becomes inactive until
it is reviewed again. This is the same invalidation `/plugin update` relies
on: replaced bytes stop matching the receipt, forcing re-review.

## Runtime behavior

An active bundle must be enabled, trusted for its current hashes, applicable to
the host, and free of validation errors. A reviewed mixed bundle may be
active, but only supported components in the reviewed v3 activation mask are
consumable. Unsupported components remain listed, hashed, reviewed, and
inactive.

- Skills are exposed only as `<plugin>:<skill>`. The model-facing catalogue and
  `load_skill` use an in-memory snapshot bound to the reviewed staged tree,
  rather than reading a mutable source path at execution time. `load_skill`
  revalidates source, stage, receipt, workspace, and generation immediately
  before releasing content and fails closed on drift. Queued messages persist
  the same provenance and repeat that check at dispatch. `/skills inspect`
  identifies the reviewed bundle without exposing its mutable source path.
- MCP server names are exposed as
  `plugin-<plugin-name-byte-length>-<plugin>-<server>` so hyphens in either
  component cannot create an authority collision. Disabled or untrusted
  bundles are denied again at the headless MCP adapter. Authority is checked
  before connection, immediately before every lazy stdio spawn, after
  transport construction, and before each tool/resource/prompt operation.
  Persisted generation/enablement/trust state is also watched while an
  operation is in flight, so disable, revoke, or another cross-process state
  transition cancels the operation and terminates a plugin stdio child. Full
  source and staged-tree hashes are revalidated at dispatch/catalogue
  boundaries; the runtime does not continuously re-hash those trees during an
  already-running MCP call. Source or stage drift therefore fails the next
  boundary and drops the stale connection/catalogue entry, but is not claimed
  to interrupt a call already executing. Every failure includes instructions
  to reload, review, trust, and enable the bundle again.
- Commands load after ordinary user/workspace commands and saved workflows, so
  existing definitions keep precedence and collisions are visible. The
  palette hides a revoked command immediately; dispatch rechecks the full
  receipt before expanding its body and reports a visible denial on stale
  input.
- Agent profiles join the Fleet roster below explicit config, personal, and
  workspace profiles but above built-ins. Roster collisions retain the
  existing visible shadow record. Every Agent spawn rebuilds from the current
  registry and rechecks the selected plugin profile's authority before its
  prompt or route can be used.
- Hooks merge after global hooks and before trusted project hooks. Foreground
  Hooks recheck authority immediately before process spawn; background Hooks
  check before enqueue and again at dequeue so a queued, revoked Hook cannot
  start later.
- Plain launch, resume, fork, exec, and serve each construct an immutable
  workspace-scoped registry before constructing their plugin-backed catalogues.
- Constitution, repository instructions, permission rules, sandbox policy,
  and MCP tool approval continue to outrank plugin instructions.

`/plugin list`, `show`, `suggest`, and `validate` perform no network requests,
process launches, credential reads, or configuration writes. Reviews render
structural argv as lossless JSON strings and environment provenance without
values. Credential-bearing argv is rejected at manifest validation;
plugin-originated errors suppress URL query, authentication, argv, and
environment material. Legacy executable tools under `[tools].plugin_dir`
remain a distinct system and are listed under `/plugin tools`; they cannot
approve themselves or replace built-in tools (see
[CONFIGURATION.md](CONFIGURATION.md#script-tools-and-overrides)).

## Explicit non-goals as of v0.9.10

Federated marketplace catalogs (`/plugin marketplace add|list|show|remove|install`)
parse local Kimi-, Claude-, Codex-, and Codewhale-format catalog documents; see
the marketplace section below (`/plugin install` fetches
one reviewed source, and `/plugin suggest` ranks only what is already
installed), no ambient compatibility discovery, no automatic trust, no
plugin-contributed MCP OAuth, no LSP adapter or MCP subscription adapter, no
native extension runtime outside the experimental `extension_host` flag, no
foreign executable plugin runtime import, and no on-disk auto-migration of a
legacy `plugin.toml` to `plugin.json`. The explicit offline
[OpenCode/DSH converter](PLUGIN_AUTHORING.md#convert-an-existing-plugin) supports
selected portable Skills, static Streamable HTTP MCP declarations, and
explicitly packaged Node `.mjs`, `.js`, or `.cjs` MCP servers selected with `--stdio-root`.
Local source and dependencies are copied for the same native installation,
capability review, hash-bound trust and enable flow; conversion executes no
code or package manager. It does not migrate arbitrary bundles or reproduce
another client's runtime or policy.
The other capabilities above remain later work rather than implied support.

## Marketplace catalogs (#5311)

`/plugin marketplace` reads LOCAL catalog documents in the real published
schemas (Kimi, Claude, Codex, Codewhale native; Codex via its policy markers)
and renders every candidate with an honest install plan:

```text
/plugin marketplace add <name> <path>   # parse a local catalog file (no network)
/plugin marketplace list                # catalogs + candidates + diagnostics
/plugin marketplace show <name>          # one catalog in detail
/plugin marketplace remove <name>        # forget a catalog (plugins unaffected)
/plugin marketplace install <catalog> <candidate>
```

- `add` never fetches anything: it reads one local JSON file (≤4 MiB, regular
  files only, symlinks refused) and stores the parsed catalog next to the
  plugin state file.
- Catalog tiers and provenance (`official`, `curated`, …) are **display
  only** — they never grant trust, enablement, or installation.
- Foreign policies are visibly ignored: a Codex `INSTALLED_BY_DEFAULT` entry
  is listed with a `NO_AUTO_INSTALL` warning and nothing is installed until
  an operator runs the install verb.
- Sources Codewhale cannot fetch (npm packages, `command:` sources, non-tarball
  URLs) are listed as `not installable` with the reason.
- `install` routes through the same reviewed installer as `/plugin install`:
  the bundle lands disabled and untrusted, and enters the hash-bound trust
  review before anything activates.
