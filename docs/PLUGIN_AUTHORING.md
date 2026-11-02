# Write your first Codewhale plugin

> 阅读简体中文版：[zh_hans/PLUGIN_AUTHORING.md](zh_hans/PLUGIN_AUTHORING.md)

Start with a skill: a Markdown instruction file inside a small plugin bundle.
The [hello-codewhale example](examples/plugins/hello-codewhale/plugin.json)
contains two files, declares no server or hook, and asks for no tool use.
This walkthrough takes it from source files to a reviewed, enabled skill.

For a custom AI provider with host-owned OAuth, see [Plugin providers](PLUGIN_PROVIDERS.md)
and the [provider example](examples/plugins/oauth-provider/plugin.json).

## 1. Create the bundle

Use the checked-in example, or create this directory outside an installed
plugins directory:

```text
hello-codewhale/
├── plugin.json
└── skills/
    └── hello/
        └── SKILL.md
```

`plugin.json`:

```json
{
  "$schema": "https://agent-plugins.org/schemas/plugin.json",
  "name": "hello-codewhale",
  "version": "0.1.0",
  "description": "A minimal, explicitly invoked greeting skill."
}
```

`skills/hello/SKILL.md`:

```markdown
---
name: hello
description: Greet the user when they explicitly try the hello-codewhale example.
invocation: explicit-only
---

Respond with one short greeting in the user's language. Include the exact text
`hello-codewhale:hello` so they can identify the example they invoked.

Use only the conversation. Do not call tools, run commands, read or write files,
or contact external services.
```

Codewhale finds `skills/` automatically. `explicit-only` keeps this example out
of the model's automatic skill catalogue; you load it by name. See
[Skills](SKILLS.md#invocation-and-alias-metadata) for invocation metadata and
[Plugin bundles](PLUGIN_BUNDLES.md#manifest) for the authoritative manifest
contract. Keep new native bundles in `plugin.json`; no second manifest is
needed.

## 2. Install, inspect, and trust

Start Codewhale in the repository root. Enter these commands **inside the
Codewhale session**, one at a time:

```text
/plugin install ./docs/examples/plugins/hello-codewhale
/plugin validate hello-codewhale
/plugin show hello-codewhale
```

For your own bundle, replace the install path with its directory. Installation
copies it to `~/.codewhale/plugins/hello-codewhale/`, disabled and untrusted.
Review the installed source, skill inventory, and permissions. This example
should declare only Skills, with no MCP server, hook, or requested network host.

The install review prints a command containing two full hashes:

```text
/plugin trust hello-codewhale <full-content-sha256>.<full-capability-sha256>
```

Run the exact command printed by your review; the angle-bracket text above is
a placeholder. If you need a fresh review, use `/plugin trust hello-codewhale`
without a token. Trust records the reviewed content and capability hashes and
creates a runtime snapshot. It does not enable the plugin.

```text
/plugin enable hello-codewhale
/skills hello-codewhale:
/skills inspect
```

The skill is named `hello-codewhale:hello`: the bundle name qualifies the skill
name. `/skills inspect` identifies its reviewed plugin snapshot.

## 3. Invoke it and turn it off

```text
/skill hello-codewhale:hello
```

Codewhale confirms activation. Then send `Say hello.` as a normal message.
The reply should be a short greeting containing `hello-codewhale:hello`.
The example contributes instructions only; the reply still uses your selected
model and its normal provider connection. The local install, review, and
activation steps do not need a model call.

```text
/plugin disable hello-codewhale
```

Disabling removes the plugin's contributions while preserving its trust
receipt. A subsequent `/skill hello-codewhale:hello` must not activate it.
Enable it again when needed, provided its reviewed hashes still match.

## 4. Iterate and review changes

The installed bundle is a copy. Editing the example's original source does
not update that copy. To try a changed local source, disable and uninstall the
installed example, then install the source directory again:

```text
/plugin disable hello-codewhale
/plugin uninstall hello-codewhale
/plugin install ./docs/examples/plugins/hello-codewhale
/plugin validate hello-codewhale
```

Uninstall removes the installed copy; it leaves the original example source
alone. Review the new token, trust it, and enable it again. For bundles
installed from a remote source, use `/plugin update <name>`; see
[Installing plugins](PLUGINS.md#update-and-uninstall).

When files in a discovered bundle change directly, `/plugin reload` refreshes
the registry. Changed content invalidates the old receipt, even if you leave
the version unchanged. Reload does not grant trust. Use `/plugin revoke <name>`
to remove trust explicitly.

## Add only the components you need

All components use the same bundle review and existing Codewhale runtime:

| Component | Authoring surface |
| --- | --- |
| Skills | `skills/<name>/SKILL.md`; [instruction and invocation contract](SKILLS.md). |
| MCP | A sibling `mcp.json`; [bundle transport and credential rules](PLUGIN_BUNDLES.md#validation-both-formats). |
| Commands | Markdown command files; [command metadata](architecture/command-dispatch.md#user-commands). |
| Agent profiles | Fleet TOML profiles; [Fleet authoring](FLEET.md#authoring-agent-profiles-fleet-setup). |
| Hooks | `HooksConfig` TOML files; [events and process behavior](HOOKS.md). |
| Native mods (experimental) | Reviewed ESM entries contributing tools, commands, pre-execute listeners, prompt sections and owner-local JSON state; [extension contract](EXTENSIONS.md). |

Declare Commands, Agents, and Hooks paths under
`extensions["net.codewhale"]` in `plugin.json`, as specified in
[Plugin bundles](PLUGIN_BUNDLES.md#active-and-inactive-component-surfaces).
Do not place MCP server fields or arbitrary runtime entrypoints at the manifest
root. LSP can be inventoried but has no executable adapter. A `native`
extension is inventory-only by default; with the experimental
`[features] extension_host` flag on, it names one `.mjs`, `.js` or `.mts` ES module
file that the TypeScript extension host runs, and `/plugin validate` rejects
any other entry. Its tools always use `Required` approval, never a plugin's
read-only hint. Full Access, Bypass, or an exact session grant for the
reviewed build can satisfy that gate without a prompt
([design](design/TS_EXTENSION_HOST.md#as-built-phase-1-2026-09-25)).
For a tested typed example, lifecycle rules and per-plugin diagnostics, read
[Writing an extension tool](EXTENSIONS.md). `.mts` supports Node's erasable
types without a separate compiler; syntax needing transformation is not supported.

A plugin whose tools act on the person's own machine needs more than an
approval posture: what it may ever attempt, what the person has granted, and
how often to interrupt are three different questions. The Computer Use bundle
is the worked example, and
[its permission model](https://github.com/codewhale-hq/codewhale-cu-plugin/blob/main/docs/PERMISSIONS_MODEL.md)
proposes the separation the engine should adopt.

## Write a scoped native mod

The [mod-extension example](examples/plugins/mod-extension/README.md) is a
runnable ESM bundle with no package installation or compiler step. It registers
`mod_counter`, `/mod-count`, one prompt section, and a pre-execute listener
limited to its own tool. The tool returns a structured JSON counter value;
`ctx.storage` keeps that value in the owner directory Rust assigned. Its
idempotent disposers remove registrations and wait for queued work. Disable
withdraws the prompt section and listener while retaining the stored counter.

Enable the experimental `extension_host` feature explicitly, install the
example directory, validate and inspect its Native capability, then personally
review and trust its exact content/capability hashes before enabling it. With
the feature off, native code remains inventory-only. Source changes require
another review; `/plugin reload` is explicit, not a hot-reload watcher.

The available author services are `tools`, `commands`, `prompt`, `storage`, `skills`,
`logger`, and Cordis lifecycle facilities. `ctx.on('tools/pre-execute', ...)`
may abstain, deny, ask, revise object input or annotate context. Rust folds those
proposals and repeats planning and admission checks for revised input. `allow`
does not approve anything; `next()` abstains. Errors, malformed answers,
timeouts and withdrawn owners fail closed. This is a pre-execute proposal
contract, with no around-execution middleware or post-result rewriting.

`ctx.prompt.registerSection({id, text})` proposes bounded, attributed
instructions delivered through the existing Engine runtime-message path.
`ctx.storage.get/set/delete` handles bounded owner-local JSON, including state
across generations. Neither API replaces the system prompt, session store or
credentials. Tool and command invocations expose optional frozen `sessionId`,
`agentId` and `originTurnId` labels supplied for that call, not runtime handles.
The public author SDK is not published; the example uses the documented shims.

`ctx.skills.registerRoot({path: 'profiles/review-skills'})` contributes child
`SKILL.md` packages from the reviewed bundle to the existing Rust skill
catalog. Its disposer retires the root; disable, revoke and host exit do the
same. Rust checks the Native receipt, file hashes and current registration
when discovering or loading a skill, including queued user selections. A
process or host restart invalidates a saved Native selection. Bundle-relative
paths, parser rules and count/byte limits are documented in
[the skill-root contract](EXTENSIONS.md#skill-roots). This API adds instructions;
tool permissions remain with the shared engine.

The DSH importer described below can prepare a reviewed Native composition
from a closed package, as well as convert portable declarative components.
Custom Ratatui/GPUI widgets, DSH browser UI slots and DSH's agent runtime
are not provided.
Compatible Claude bundles still use the existing declarative component adapters;
this Native API does not load Claude's agent loop or automatically adapt Pi's
extension API. Port executable mod behavior against the documented host contract.
An extension tool can use `exec.core.call` only during its direct model
invocation under the shared turn gate. Commands and activation have no core
handle. Nested shell and network calls force a user prompt; a mode that cannot
open one refuses them. See [the exact core-call contract](EXTENSIONS.md#asking-the-core-to-run-a-tool)
for refused tools, cancellation and call limits.

Plugin trust is **not an OS sandbox**. A local MCP server or hook can launch a
process; review its code and authority before enabling it. Skills do not grant
permissions: repository instructions, permission rules, sandbox policy, and
tool approval still apply. Keep credentials out of bundles and command
arguments. Use the reviewed environment references documented in the
[bundle validation contract](PLUGIN_BUNDLES.md#validation-both-formats) for MCP;
read the separate [hook environment contract](HOOKS.md#the-hook-process-environment)
before adding a hook.

## Convert an existing plugin

[`scripts/convert-plugin.py`](../scripts/convert-plugin.py) converts explicitly
selected remote MCP declarations, packaged local Node MCP servers, and portable
Skills into a native bundle.
It requires Python 3.10+ and PyYAML 6+; install those separately if absent.
The converter installs no dependencies, scans no ambient configuration or
credentials, makes no network requests, and executes no source code.

### OpenCode

Save this plain JSON as `opencode-mcp.json`:

```json
{
  "mcp": {
    "docs": {
      "type": "remote",
      "url": "https://example.invalid/mcp",
      "oauth": false,
      "enabled": false
    }
  }
}
```

From the Codewhale repository root, run this in your shell:

```sh
python3 scripts/convert-plugin.py --format opencode-v1 \
  --config ./opencode-mcp.json --name migrated-tools --output ./migrated-opencode
```

Choose `--format opencode-v2` for the `mcp.servers.<name>` layout, whose server
flag is `disabled` instead of `enabled`. Select the format from the data;
filenames and upstream branch names do not determine its version. Both formats
require explicit `oauth: false` for remote servers. Remote MCP output uses **Streamable HTTP only**;
OpenCode's fallback to legacy SSE is not reproduced. For an SSE-only endpoint,
author native `mcp.json` with `type: "sse"` and use the same review flow.

When MCP servers are selected, configurations containing `tools`,
`permission`/`permissions`, `agent`/`agents`, legacy `mode`, or `default_agent`
are refused. These settings can restrict tool access beyond server enablement.
Manually preserve those restrictions in Codewhale before supplying an MCP-only
input; simply deleting the settings can widen access.

JSONC comments and trailing commas are not
accepted: provide a plain JSON copy containing the declarations you intend
to port.

### DeepSeek Harness (DSH)

Save this static Cordis entry list as `dsh-mcp.yml`:

```yaml
- name: '@deepseek-ai/dsh-mcp-client'
  disabled: true
  config:
    serverName: docs
    transport: streamable-http
    url: https://example.invalid/mcp
```

```sh
python3 scripts/convert-plugin.py --format dsh \
  --config ./dsh-mcp.yml --name migrated-dsh --output ./migrated-dsh
```

The DSH input may also be JSON, but must be the plain entry list, not a full
profile or patch composition. Each row must name `@deepseek-ai/dsh-mcp-client`.

A real DeepSeek Harness bundle package — an npm package whose `package.json`
declares `dsh.bundle.patch` — is imported natively by Codewhale, not by this
script (`--bundle` is retired):

```text
/plugin import dsh ./node_modules/@demo/tools-dsh
/plugin import dsh approve <package-dir> <content-hash>
```

The first command converts the package into scratch and shows what converts,
what is skipped, the network hosts and local processes the bundle will request,
and its content hash; nothing is installed. `approve` installs exactly that
converted bundle through the ordinary reviewed installer: it lands disabled and
untrusted, and a package edited after review installs nothing. Installing the
package directory as a plain `/plugin install <dir>` routes to the same importer.
`/plugin update <name>` re-converts the recorded package; changed output
replaces the bundle and invalidates its trust receipt. The Runtime API exposes
the same review as `POST /v1/apps/plugins/import/dsh/preview`, whose
`install_source` and `content_hash` go to `POST /v1/apps/plugins/install`.
Those two are the only import surfaces today: the TUI slash command
(`/plugin import dsh <dir>` and `approve`) and the Runtime API. There is no
`codewhale plugin` CLI subcommand for DSH import. This bundle import is also
not the external-launcher integration `codewhale integrations dsh`, which runs
the user's installed `dsh` (see [INTEGRATIONS_DSH.md](INTEGRATIONS_DSH.md)).

`dsh.bundle.patch` may name one patch file or an ordered list of files. The
importer reads only contained, non-linked package files (at most 64 files and
1 MiB of patch data combined), then applies `insert` and keyed overrides over
one empty profile. As in upstream `applyEntryPatches`, an override replaces a
whole field: `config` is **not** deep-merged. This is not a complete profile
resolver: other bundles, user overlays and deployment configuration are absent.
Plain YAML scalars follow the YAML 1.2 core schema, as DSH's own parser does.

Packages containing supported local modules, contained ESM package exports or
the supported DSH service bridges can produce a Native composition. The importer
seals the contained source files and module hashes, then generates the Native
entry or entries. A Native graph with an unresolved or unsupported row is
refused as a whole. It does not silently install the parts that happened to
resolve. Reviewed agent-preset packages can produce separate Native entries;
the selected default or child preset scopes their contributions.

Import and review never execute plugin code. A generated Native composition
remains inactive until `extension_host` is explicitly enabled and the installed
bundle is reviewed, trusted and enabled. The host verifies its source closure
before loading it, and Rust retains tool approval and session authority. The
supported services, runtime limits and teardown behavior are documented in
[EXTENSIONS.md](EXTENSIONS.md). DSH UI code and the upstream agent loop are
outside this contract.

For portable declarative conversion, disabled groups propagate their state to
descendants. Disabled MCP declarations
stay disabled. Disabled skills are omitted with an explicit receipt because the
native skill format has no disabled state. A conditional/non-boolean `disabled`
value on a group or portable row refuses the import rather than assuming it is
enabled. Unsupported entry policy/dependency fields, including `inject`,
`intercept` and `isolate`, also refuse the import on those rows or their groups.
Preserve their activation and authority rules in a manual port.

Unrepresentable portable components are reported in the bundle's `CONVERSION.md` and
structured `CONVERSION.json`, with source package/version, manifest and
ordered-layer SHA-256 hashes, converter version, per-row outcomes and required
manual ports. Unapplied patch operations also appear in the structured
manual-port list, with their source layer and one-based operation index.

In portable conversion, the only lowered `!!js` expressions are
`process.execPath` (becomes `node`) and
simple quoted/template literals without escapes or interpolation. Environment
expressions—including fallbacks—are never resolved against this machine.
Packaged Node MCP converts only when its relative entry resolves inside the
package (and its declared relative working directory); a server outside the
package is skipped, and host paths are never copied. Rows of
`@deepseek-ai/dsh-skill-filesystem` contribute their literal `customSkillDirs`
children only when those directories live inside the package. Default user and
project skill roots, watchers and foreign service dependencies are not imported.
DSH modules run through the experimental TypeScript extension host
(`[features] extension_host`, off by default), using either an explicitly
authored Native entry or a generated reviewed composition. Tools, commands,
prompt sections, skills, hooks and owner-local storage use Codewhale's existing
consumers and approval rules. This is a supported compatibility subset; a
successful import is not evidence that every upstream DSH service is available.
See [EXTENSIONS.md](EXTENSIONS.md) and
[design/TS_EXTENSION_HOST.md](design/TS_EXTENSION_HOST.md).

### Local Node MCP servers

For an already packaged Node MCP server, select its original process working
directory explicitly. The converter copies that directory into `mcp/<server>`
and sets the native server's working directory to the reviewed copy. Relative
entrypoint imports and read-only resources keep the same layout.

```json
{
  "mcp": {
    "localdocs": {
      "type": "local",
      "command": ["node", "server.mjs"],
      "environment": {"API_TOKEN": "{env:LOCALDOCS_TOKEN}"},
      "enabled": false
    }
  }
}
```

```sh
python3 scripts/convert-plugin.py --format opencode-v1 \
  --config ./local-mcp.json --stdio-root localdocs=./packaged-localdocs \
  --name local-tools --output ./migrated-local
```

Repeat `--stdio-root SERVER=DIRECTORY` for every local server in the selected
configuration. OpenCode v2 uses `mcp.servers` and `disabled`. Static DSH entries
use `transport: stdio`, `command: node`, and `args: [server.mjs]`; DSH `env`
must be absent or empty because its literals/expressions are not OpenCode
environment references. Optional DSH/v2 `cwd` must be absent, empty, or `.`;
the selected root explicitly supplies the original working directory.

Use `node` plus one relative `.mjs`, `.js`, or `.cjs` entry. Package module
type and sibling imports are preserved by the native launch adapter. Compile
TypeScript to JavaScript before packaging; the converter does not run a compiler.
Package dependencies and read-only resources first, inside the selected root.
No package manager, install script, module loader or server runs during
conversion. Links/reparse points, hard-linked files, hidden files/directories
(including `.gitignore`, `.env*`, `.npmrc` and `node_modules/.bin`), common
credential filenames, and private-key containers are refused. Prepare a clean
package directory; ignore rules are not used to silently omit files. Inspect
every selected file for embedded credentials before conversion. The existing
4,096-file / 64 MiB aggregate bundle limit applies.

The converter rejects shell launchers, Node flags, extra arguments, non-Node
interpreters, literal environment values, and loader-changing environment
names. Stateful servers that write into their working directory, depend on the
live workspace, or import files outside the package need a manual native port.
Copying files does not statically verify JavaScript import closure or sandbox
arbitrary code. Local MCP processes run with host-user authority; their network
and filesystem access are not restricted by the remote endpoint host list.
The same native install, capability review, hash-bound trust, and enable steps
are required before Codewhale launches the server. This adds a packaged Node
MCP subset; it does not execute DSH/Cordis plugin modules.

### Review the result

Both examples preserve disabled servers and use a placeholder endpoint. Replace
the endpoint and change the source's enablement flag before reconverting when
you are ready to connect. The output directory must be new, with an existing
parent. Existing output is refused; rejected input leaves no output bundle.

Add `--skill ./my-skill` for an explicitly selected directory containing
`SKILL.md`, or `--skill ./my-skill.md` for a single file; repeat the option for
more skills. `--config` is optional for a skills-only conversion. Skills require
`name` and `description` frontmatter. `disable-model-invocation: true` becomes
native `invocation: explicit-only`. Informational `license`, `compatibility`,
and `metadata` fields are retained in `SOURCE_SKILL_METADATA.json` companion
data. Companion files from selected skill directories are copied as data;
review them and the instructions before loading the skill.

Only exact OpenCode header references such as `{env:MCP_TOKEN}` become native
`env_headers`; the converter never reads the variable's value. Literal headers,
DSH header expressions, and URL file/environment substitution are refused.
Configured timeouts must be whole seconds expressed in milliseconds, from
`1000` through `3600000`. Omitted timeouts use Codewhale's defaults.

Executable foreign plugins and hooks, other stdio launchers, automatic OAuth,
configuration JavaScript,
YAML aliases/tags, `__jsExpr`, and unsupported skill runtime fields (including
`user-invocable: false`) require a manual port. Conversion does not reproduce
another client's runtime or bypass Codewhale's credential and sandbox rules.

Read the generated `CONVERSION.md`, `CONVERSION.json` for bundles, `plugin.json`, `mcp.json` when present, and
all selected skill and MCP source files. Then use `/plugin install ./migrated-opencode` (or the
DSH output path), `/plugin validate <name>`, and the same hash-bound trust and
enable flow above. Conversion alone proves neither connectivity nor runtime
compatibility; the output is not installed, trusted, or enabled.

Source audit, 2026-09-08: OpenCode's [v1 MCP documentation](https://github.com/anomalyco/opencode/blob/d6855b6b47a8433462ac6aeeba882ccf734cb7f1/packages/web/src/content/docs/mcp-servers.mdx)
and [v2 MCP schema](https://github.com/anomalyco/opencode/blob/d6855b6b47a8433462ac6aeeba882ccf734cb7f1/packages/core/src/config/mcp.ts)
at `d6855b6b47`, and DSH's [MCP client reference](https://github.com/deepseek-ai/deepseek-harness/blob/c389f96bf3a9b6807cb71ed6bdad5849be0df6d8/packages/mcp/mcp-client/README.md)
at `c389f96bf3`. Bundle patch semantics were rechecked against DSH
[`00102833df`](https://github.com/deepseek-ai/deepseek-harness/tree/00102833dfaee1da9f48a3a8eae9d34005a75218)
(`0.1.7-alpha.2`); `scripts/fixtures/dsh-web-app` retains its real five-file package
as pinned test data, not as an importable native plugin. The Rust tests in
`crates/tui/src/plugins_dsh_tests.rs` load that package through the native
importer and assert that no row is promoted to a converted component; they run in
the workspace `Test` job. The CI `Plugin conversion` job runs only
`scripts/test_convert_plugin.py` (the OpenCode and static DSH MCP-entry converter)
with Python/PyYAML and synthetic Node fixtures, and the script's `--bundle` path
refuses and points to `/plugin import dsh`. Upstream supports more than this
deliberately bounded converter.

## Community context

This guide responds to [giancarlocp's request for plugin authoring guidance
and OpenCode conversion in discussion #5827](https://github.com/codewhale-hq/Codewhale/discussions/5827).
The Chinese companion follows the documentation work requested by
[SparkofSpike in issue #5482](https://github.com/codewhale-hq/Codewhale/issues/5482).
