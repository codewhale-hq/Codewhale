# Writing an extension tool or command

The experimental TypeScript extension host runs reviewed plugin code in a
shared Node process (or, as an opt-in, Bun). Rust still owns sessions, tool admission, approval and
execution. Extensions currently contribute tools and slash commands; they
cannot provide an approval service, run a second agent loop or replace
built-in tools or commands.

## Quick start

Start with [hello-extension](examples/plugins/hello-extension/hello.mts) and its
[plugin.json](examples/plugins/hello-extension/plugin.json). The example is
executed unchanged by the host tests. It returns a greeting and does not read
files or use the network.

1. Explicitly enable `[features] extension_host = true` in your configuration,
   or start Codewhale with `codewhale --enable extension_host` (the flag goes
   before any subcommand).
2. Install the example's directory using `/plugin install <local-directory>`.
3. Run `/plugin validate hello-extension`, then `/plugin show hello-extension`.
4. Run `/plugin enable hello-extension` to open its content/capability review.
   Read the review and personally run its exact `/plugin trust <token>` command,
   then enable the plugin. Trust alone does not execute the code.
5. Ask Codewhale to use `hello_greet`. The tool follows the normal approval gate.
6. Run `/hello-greet Codewhale`. The command runs when you type it, and shows
   its answer in the transcript with its origin, `extension:hello-extension`.

An authoring agent should stop after installation and validation and present
the review to the person. Do not trust or enable a bundle automatically.

## Locations, installation and trust

User bundles live under `~/.codewhale/plugins/`; workspace bundles live under
`<workspace>/.codewhale/plugins/`. New bundles use Agent Plugins v1.0.0
`plugin.json`. Declare `extensions["net.codewhale"].native.path` as one regular
`.mjs`, `.js` or `.mts` file inside the bundle. Directories and other extensions
fail validation when the host is enabled. With the flag off, native entries
remain inventory-only.

A plugin may split its code across several entries with `native.paths` (up to
64, alone or beside `path`). They are activated in the order declared, as the
fibers of one plugin: they share one owner, so one disable, review change or
crash tears all of them down together. Each entry is a separate module with its
own `apply`, and tool and command names must be unique across them. If an entry
fails to activate (throws, requires a service the host does not provide, or has
a registration refused), only that entry is withdrawn: entries that already
activated stay registered, later entries still activate, and the plugin fails
only when no entry activates. Listing the same file twice is one entry.

## Runtime

`[extension_host] runtime` selects what runs the host. Node is the default.
Bun is an opt-in: it is qualified on macOS only, and becomes the default only
after an explicit, recorded cutover.

```toml
[extension_host]
runtime = "node"   # default: Node only
# runtime = "bun"  # Bun only; fails rather than falling back to Node
# runtime = "auto" # Bun >= 1.4.0 if one is found and starts, otherwise Node
# bun = "~/.bun/bin/bun"          # the only Bun tried when set
# node = "/opt/homebrew/bin/node" # the only Node tried when set
# mcp_backend = "rust" # temporary opt-out; default is "host"
```

The default `mcp_backend = "host"` uses the pinned MCP SDK independently of
`[features] extension_host`, which controls optional Native extensions. Existing
stdio and HTTP servers use modern discovery when supported; explicitly configured
legacy SSE servers use their legacy handshake. Rust retains process and network
authority, credential resolution, catalog admission, tool approval and cancellation.
The SDK receives opaque session selectors and exact operation grants. Modern
requests carry Rust-approved protocol metadata and headers without a session id.

A typed discovery refusal can admit a legacy handshake. Only an explicit HTTP
transport refusal during that handshake can admit legacy SSE with a fresh exact
Rust grant. Legacy HTTP retains its GET preflight and bounded OAuth refresh;
modern HTTP does not inherit those session or resend behaviors. Incomplete result
flows are reported without automatically repeating the operation. Credentials,
configured URLs, OAuth browser/login operations and Computer Use decision keys
stay in Rust.

Both backends share recorded semantic MCP goldens and retain separate transport
goldens, including discovery, authentication and session evidence. The Host default
requires those fixtures and the actual broker acceptance checks, plus the platform
isolation and measured gates in Ops CURRENT_DECISIONS §26. An explicit
`mcp_backend = "rust"` remains available for one release after the qualified default
flip, before the native protocol adapters retire. Missing or unsupported Node uses
the existing doctor runtime diagnostic. Host failures never select Rust automatically.

The stock finance, data and speech adapters have independent experimental flags:
`[features] finance_host = true`, `data_host = true` and `speech_host = true`.
Each defaults to Rust. Speech preparation is shared by CLI `speech`/`tts` and
the model tools; clone samples, provider requests and output writes stay in Rust.
Selecting either uses the pinned Builtin harness and its existing operation
broker, even when Native extensions are disabled. Rust retains network/file
access, parsing diagnostics and permissions. A selected Host failure is reported;
it never silently switches to the Rust adapter.

Node must satisfy `^22.19 || >=24`; Bun must be 1.4.0 or newer. Leaving
`runtime` unset means `node`, except that a table which sets only `bun` means
`bun`. A configured `node` or `bun` path is the only candidate for that
runtime: if it does not run or is below the floor, the host fails with that
reason instead of searching `PATH`. Without one, every `node` (or `bun`, then
`$BUN_INSTALL/bin`, default `~/.bun/bin`) on `PATH` is tried in order, and one
inside a `node_modules` directory or the working directory is skipped without
being run; set the path explicitly to use such a runtime. The working-directory
check does not apply when Codewhale starts in a directory that contains your
home directory, such as `~` or `/`.

Under `auto`, if the Bun it found fails to start the host, `/plugin` reports
why once and Node runs the host for the rest of the session. The runtime is
pinned once a host on it completes its handshake, and restarts reuse it. A
runtime binary replaced at that path mid-session is refused; restart Codewhale
to use it. `codewhale doctor` and `/plugin` show which runtime runs the host,
its version and path, how its memory cap is enforced, and, when `auto` used
Node, why Bun was not used.

When the host cannot take a call, `/plugin` and the failing extension tool
call give the same reason: not started, starting, unresponsive, restarting
after a crash (with how it exited), failed to start or out of crash budget
(with the reason; change or reload a plugin to retry), or disabled by config.
Every call into the host has a deadline, 120 s for a tool call; past it
Codewhale cancels the call, reports a timeout, and the host keeps serving
other calls. A plugin that ignores cancellation keeps running inside the host
until the host is torn down.

Write extensions for both runtimes. Use Node's APIs (Bun implements them) and
only erasable TypeScript in `.mts`. Enums, decorators and syntax that needs
transformation require a separate author build to JavaScript. Bun would accept
more, but Node would not. Node never reads a `tsconfig.json` for the host. Bun
does: a `tsconfig.json` next to or above an extension's files applies its
`paths`, `baseUrl` and JSX settings to that extension's imports, so an import
that resolves under Bun can fail under Node. Do not rely on it. Known limit:
that includes a `tsconfig.json` in a directory above the reviewed bundle,
which is not part of what you reviewed. See
[Node's TypeScript rules](https://nodejs.org/docs/latest-v22.x/api/typescript.html).
Under Bun the host runs with `--no-install`: a missing package fails the import;
it is never downloaded.

Reviewed DSH compositions admit every local module against its recorded path
and SHA-256 before importing it. Node checks runtime resolution; Bun prepares
the reviewed JavaScript syntax and checks computed imports before they run.
Unreviewed files and ambient package imports are refused. Bun currently cannot
preserve query/fragment module identity, and computed CommonJS resolution also
requires Node. These cases stop with a diagnostic: select
`[extension_host] runtime = "node"` for that composition. Ordinary UTF-8 source
and reviewed computed file/JSON imports work on both runtimes.

Extensions cannot run native code inside the host. On both runtimes
`process.dlopen`, `process.execve` and Worker threads are unavailable (a Worker
is a new JavaScript realm that would start without these restrictions). Under
Bun, `bun:ffi`, `Bun.FFI`, `bun:sqlite`, `node:sqlite` and `ShadowRealm` are
unavailable too; under Node, `node:sqlite` and `node:ffi` are switched off
(SQLite extensions and FFI load native libraries). The host refuses to start
when one of these restrictions does not hold on the installed runtime. Known
limit: these are the native-code entry points found so far (Bun 1.4, Node 22
and 26); one a newer runtime adds is not covered until it is added. A
process an extension starts is outside this policy; on macOS and Linux it runs
under the same sandbox as the host.
Include local imports in the bundle; trust stages reviewed content, and the
entry is rehashed before import. Changes to reviewed bytes or capabilities
require another review. See [bundle rules](PLUGIN_BUNDLES.md).

## Imports and services

Export a Cordis plugin function or an object with `apply`. The host supplies
one shared Cordis and the supported DSH compatibility services. The example's
`inject = ['tools', 'commands']` asks for the tool and command registries. The
supplied service names are `tools`, `commands`, `prompt`, `storage`, `skills`, `shellHooks`, `avatars`, `mcp`, `logger`,
`events`, `reflect` and `registry`. `ctx.avatars.registerPack({path})` registers a
reviewed bundle-relative avatar pack; see
[the whale-avatar example](examples/plugins/whale-avatar/README.md). A tool can ask the core to run a core tool through
`exec.core` (see [Asking the core to run a tool](#asking-the-core-to-run-a-tool));
Reviewed Native entries can propose MCP definitions through `ctx.mcp` as
described below. Programmable pre-execute listeners use `ctx.on` as described below. A required
service that is unavailable fails activation with a diagnostic.

Package runtime dependencies and local imports within the reviewed bundle.
Do not install packages or fetch code during activation. Register cleanup
through `ctx.effect`; asynchronous disposers are awaited with a deadline.

`ctx.prompt.registerSection({ id, text })` contributes an attributed plain-text
section and returns its disposer. Rust delivers the complete current snapshot
as a user-role runtime message; changes replace earlier snapshots and an empty
snapshot explicitly withdraws earlier sections. Rust admits
sections only for the caller's live reviewed owners, sorts them by owner/id,
and withdraws them on unregister, disable, revoke or host exit. Each section
is limited to 4 KiB UTF-8 text, each owner to 32 KiB and 128 sections, and the
host to 128 KiB and 1,024 sections. The final attributed prompt has the same
128 KiB limit. IDs use lower-case letters, digits, `_` and `-`, start with a
letter, and contain at most 64 characters. Duplicate owner-local IDs require
disposing the earlier section first. The author cannot replace the core
system prompt or select another session's sections.

To interpolate Core's accepted turn facts, register
`{id, text, interpolate: 'model-cwd'}`. Only `{{model}}` and `{{cwd}}` are
supported; Rust expands them once when it captures the turn's prompt. Literal
sections retain braces unchanged. Both source and expanded text must fit the
section and snapshot limits. Scoped contributions keep their exact selected
entry identity through capture and withdrawal. The fixed DSH persona bridge
uses this path for additive prefix/suffix text; complete prompt replacement and
runtime-context suppression are refused.

Reviewed DSH compositions can register Claude Code and Codex command hooks
through `shellHooks`. Codewhale's existing fifteen firepoints use the same
pinned Builtin runner and Rust-owned process driver when the host feature is
enabled. Rust retains hook approval, process environment and final verdicts;
ShellEnv values remain in Rust. Forced continuation, observer steering,
noncommand hooks and asynchronous dialect commands are unsupported and
reported explicitly. A Native hook's `allow` cannot approve a tool.

`ctx.storage.get(key)`, `set(key, json)` and `delete(key)` persist owner-local
JSON under the `dataDir` Rust assigned. The API refuses access once owner
disposal begins, symlinked storage, corrupt data and writes that exceed its
bounded key/value/owner limits. It does not expose session history or secrets.
Tool and command invocations also expose frozen `sessionId`, `agentId` and
`originTurnId` strings when Rust supplies them for that particular call. Tool invocations
with an authoritative session and workspace also expose frozen `exec.agent.id`
and `exec.agent.session.header.cwd`; root identity uses the session id and a
child uses its supplied agent id. The projection has no methods and retaining it
grants no operations after the invocation ends.

An entry injecting `sessions` can call `ctx.sessions.flush(exec.agent.session)`
while its directly invoked root tool holds an Engine checkpoint ticket. The
argument must be that invocation's exact session object. The Engine captures
its committed prefix and waits for the owning Runtime, terminal, or exec store
to acknowledge persistence. Runtime retains its existing receipt-secret
redaction policy; success describes that persisted projection. It does not
include the running tool's future result or the plugin's separate journal.
Runtime recovery retains later tool results without replaying the tool.
Checkpoint-bearing Runtime turns require schema version 5; older readers
refuse them rather than silently reconstructing incomplete history.

`false` is reserved for a deliberately ephemeral host with no persistence
backend. Missing authority, foreign or expired handles, child checkpoints,
superseded writes, storage failure, cancellation and unavailable consumers
reject. Cancellation can race a disk write, so a rejected call does not prove
nothing was written. Other `sessions` methods remain unsupported.

## MCP definitions

`ctx.mcp.registerServer({ serverName, server })` returns an idempotent disposer
and proposes a literal MCP definition to the existing Rust catalog. Include
`mcp` in the entry's `inject` list. Rust owns transport connections, tool
admission, permissions and authentication; this service does not expose
credentials or grant extensions direct process or network access.

Definitions retain their exact reviewed, selected Native entry receipt.
The limits are 64 servers per owner, 256 per host and 64 KiB per definition.
Literal stdio, streamable HTTP and SSE definitions are supported by the bridge;
credential values, endpoint query data and unsupported startup/reconnect
controls are refused. Disposing an entry, changing its caller selection,
disabling its plugin or changing reviewed bytes withdraws its definitions and
cancels affected calls. An uncertain write is never replayed.

The raw DSH skill-filesystem bridge uses the same reviewed skill-root service
below. Its configuration must explicitly set `includeDefaultRoots: false` and
`watch: false`, and list bundle-relative `customSkillDirs`. Ambient filesystem
roots and independent watchers remain unsupported.

## Skill roots

`ctx.skills.registerRoot({ path: 'profiles/review-skills' })` returns an
idempotent disposer and contributes reviewed instructions to Rust's existing
skill catalog. Include `skills` in the entry's `inject` list. For example:

```js
export const inject = ['skills']
export function apply(ctx) {
  ctx.skills.registerRoot({ path: 'profiles/review-skills' })
}
```

Place each skill in a child package directory such as
`profiles/review-skills/quick-check/SKILL.md`. Paths are bundle-relative,
at most 512 UTF-8 bytes, with normal slash-separated components. Absolute
paths, links, empty components, `.` and `..` are refused. Rust parses only
files covered by the reviewed bundle inventory, through the existing
[frontmatter and invocation contract](SKILLS.md#invocation-and-alias-metadata).
The existing nesting rules apply: hidden child directories are skipped, and
a package containing `SKILL.md` claims its nested examples. An invalid skill
or an empty root refuses admission. Native review covers this registration;
the entry does not need a separate declarative Skills component.

Pending and admitted roots count toward the host shim's limits: 8 roots per
owner and 64 per host. Rust stops each root proposal at 128 candidate
`SKILL.md` files or 4 MiB of raw input. Retained instruction fields are also
limited to 128 skills / 4 MiB per owner and 1,024 skills / 32 MiB per host.
These are logical catalog limits. Duplicate owner-local root paths require
disposing the earlier root first.

Disposal, disable, revocation and host exit remove the root from later
discovery. Loading a selected skill rechecks the reviewed Native receipt and
the live registration. Queued selections also recheck it; selections saved
before a process or host restart require selecting the skill again. Completed
session history remains the record of instructions already used. Roots have
no watcher, companion-file access or tool permission grant; core approval and
sandbox policy continue to apply.

## Tool rules

Register a unique name with `ctx.tools.register`, a description, a JSON object
input schema and an `execute(input, exec)` function. Names start with a letter,
contain only letters, digits, `_` and `-`, and are at most 64 characters. Use a
plugin-specific prefix, such as `hello_greet`. Core names, core approval-name
families, and `mcp_`/`ext_` prefixes are reserved. Refusals explain the rule.

Every extension tool is `Required` and never treated as read-only based on a
plugin's claim. Full Access, Bypass or an exact session grant for the reviewed
plugin receipt may satisfy that requirement without another prompt. Tools are
deferred by default; `[tools].always_load` can pin a tool through the normal
tool configuration. Registration never grants permission to execute it.

The core checks every call's input against that schema (JSON Schema, draft
2020-12 unless the schema names another) before it asks for approval and again
before anything is sent to the host, so `additionalProperties: false`,
`required`, types and bounds hold even when `execute` does not check them. A
call that fails is returned to the model as an invalid-input error naming what to
correct, and the host never sees it. A schema that cannot be compiled (an
invalid keyword value, or a `$ref` to anything outside the schema itself;
nothing is fetched) is refused at registration, which fails activation with the
reason. Declare the narrowest schema you can: it is both what the model sees
and what is enforced.

Return a JSON value or text; the host renders it into ordinary tool output.
Avoid secrets in descriptions, logs and results. An approval card's wording
and identity come from Rust, never from plugin-supplied labels.

For DSH definitions with `output`, declare both `output.schema` and
`output.render`. The host validates the canonical JSON result before rendering
it. This adapter currently accepts plain text blocks only; unsupported rich
content or extra block fields produce an explicit error instead of disappearing.
Nonempty registration options, `projectContent`, `finalizeContent`, `timeoutMs`
and deferred loading are not implemented and are refused. The shared
[`compatibility.json`](../crates/tui/extension-host/src/dsh/compatibility.json)
pins the definition API to DSH 0.2.1-rc.2 and records service, row-bridge and
module mappings. Its bytes participate in Native content and capability review,
so changing the compatibility contract invalidates prior trust.

Successful and soft-failure tool results carry Rust-stamped `extension`
metadata identifying the plugin, reviewed content, entry, generation, caller
selection, handle and process boot. Plugin-supplied output cannot choose this
identity.

## Command rules

Register a slash command with `ctx.commands.register`, which returns an
idempotent disposer (the registration is also removed when the plugin unloads):

```ts
ctx.commands.register({
  name: 'hello-greet',            // /hello-greet
  description: 'Greet someone.',  // one line, shown in the palette and /help
  argumentHint: '[name]',         // optional; a command with a hint waits in
                                  // the composer for arguments
  handler({ args, signal }) {
    return { kind: 'success', text: `Hello, ${args || 'world'}!` }
  },
})
```

Names are lower case, start with a letter and use only `a-z`, `0-9`, `_` and
`-` (at most 64 characters). A command can never take the name of a built-in
command (or one of its aliases) or of another plugin's command: the
registration is refused, activation fails, and the reason is in `/plugin`.
A user, workspace or plugin-manifest markdown command with the same name wins
the spelling and the extension command is left out of the registry. Descriptions
(1 KiB) and hints (256 bytes) are single-line text.

A handler runs only when the **user** types the command; invoking it is the
user's own action and needs no approval. It can only return an answer:

- a string, or `{ kind: 'success', text? }`: shown in the transcript;
- `{ kind: 'error', text }` or a thrown error: shown as a failure;
- `{ kind: 'submit', prompt, text? }`: `prompt` enters the ordinary turn with
  Runtime provenance and visible plugin attribution. Every model tool call
  is gated as usual. `text` is shown beside the prompt.

The handler cannot call the model, a tool or approval. Output is stripped of
terminal escape sequences and cut at 64 KiB; a prompt over 128 KiB is refused,
not truncated. A command has 30 seconds: past it Codewhale cancels the call
(`signal` aborts) and reports a timeout. If the host is down, the command
fails at once with why.

`args` is what follows the command name, trimmed. The invocation also carries
`signal`, a `commandId` and an empty `attachments`. DSH plugins using
`ctx.commands.register({ name, description, input: { hint }, handler })` and
returning `{ kind: 'success' | 'error', text }` run unchanged, including
`import { CommandDefinitionId } from '@deepseek-ai/dsh-commands/brand'` (the
only `dsh-commands` import the host resolves; `definitionId` is accepted and
ignored). DSH's `rawInput` is provided (it keeps the leading separator,
`' name'`). When Rust supplies the caller session, `invocation.agent` exposes
its read-only id and `followup(message)`. A handler may capture exactly one
JSON user message with a bounded id, a source containing only `kind`, and one
plain text content block. Successful completion converts it to the same Submit
result; an error result, cancellation or exception never submits it. Explicit
Submit plus followup, multiple messages, attachments, empty text and text above
128 KiB are refused. The handle closes when the invocation settles. Core owns
the resulting turn identity and provenance; this is not a general agent service
or persistence of the supplied DSH message identity. Missing caller context
refuses access.

Not provided: attachments (`input.attachments: true` fails activation), a `list`/`find`/
`execute` surface, and DSH command lifecycle events. The Runtime API records
Codewhale command admission, completion and cancellation in its existing event
store.

Thread snapshots also include bounded `command_receipts` for the most recent
100 terminal commands that did not submit a turn. Each receipt has a stable
identity, completion time, status, text and extension attribution. This is a
projection of the existing event log; reopening a native App conversation can
display the result without rerunning a command or adding it to model history.

### Runtime API and native App

Read `GET /v1/commands?thread_id=<id>` to discover commands for that thread's
workspace and plugin selection. Extension entries have `kind` and `binding`
set to `extension`, an `origin` and an opaque `registration_id`. A listing
without a thread has no invocable registration identity.

Submit `name`, `args`, `registration_id`, `operation_key` and
`expected_workspace` to `POST /v1/threads/<id>/commands`. Reuse the same
operation key only for the exact same request. A completed retry returns the
stored outcome; a pending or uncertain invocation is never executed again.
`GET /v1/threads/<id>/commands/<operation_key>` reads the result. If a recorded
invocation has lost its worker, lookup settles an explicit uncertain outcome
and recovers any admitted turn without rerunning the callback. `DELETE`
requests cancellation, including cancellation before admission and interruption
of an associated active turn. A stale
registration or changed workspace requires discovering and selecting again.

The Engine admits a returned prompt through its ordinary turn lifecycle with
Runtime provenance. TUI and App present it as attributed plugin input and exclude
it from ordinary human recall, edit and retry. Clients must not resubmit that prompt. Results include the
accepted turn when present and the exact extension receipt. The native App
uses these routes for its command palette and slash input, preserves edited
drafts and refreshes the catalog after plugin changes and reconnects.

## Execution context

A tool's `execute(input, exec)` gets, in `exec`:

- `signal`, the call's cancellation signal; check it and propagate it to
  asynchronous operations;
- `callId`, which identifies the call, and `args`, its input (also the first
  argument);
- `workspace`, the root path of the workspace of the session that made the
  call, and no other (it is absent only if that path is not valid UTF-8);
- `dataDir`, the plugin's own directory;
- `core`, only while the call runs under the turn's permission gate:
  [`exec.core.call`](#asking-the-core-to-run-a-tool).

A command's handler gets `workspace` (where the user ran it) and `dataDir` in
its invocation beside `args`, `signal` and `commandId`. All of these are
read-only strings (`exec` and the invocation are frozen). Nothing else about the
machine is passed: no home directory, no other workspace, no credential path.
A call id or plugin trust grants no new privileges.

The workspace is where the *call* comes from, so read it per call: one host
serves every session in the Codewhale process, and a tool called from another
workspace sees that one. The host process's working directory is its data
directory, not the caller's workspace; do not use `process.chdir` in this
shared process, and resolve relative paths against `exec.workspace` yourself.
Reading a workspace file is your code's own filesystem access, under the host's
sandbox, not something the core checks per file.

`dataDir` is `~/.codewhale/extension-host/data/plugins/<name>-<id>`, created by
Codewhale (mode 0700 on Unix) before the plugin activates, and the same
directory for every generation and every entry of that plugin, so it keeps
files across restarts and updates. It sits inside the host's one writable
root, which is where the sandbox allows writes. It is not a boundary between
plugins: they share a process, and one can write into another's directory.
Codewhale never deletes it, including on uninstall.

## Configuration

A user configures a plugin in their own `config.toml`, keyed by the plugin's
manifest name:

```toml
[plugins."hello-extension".config]
greeting = "Howdy"
```

That table is delivered as the second argument of `apply(ctx, config)`
(`{}` when there is none). If the entry module exports a `Config` schema
(`import Schema from '@deepseek-ai/schemastery'`; `export const Config =
Schema.object({ ... })`), the host validates the table against it before
`apply` runs, applies its defaults, and a mismatch fails activation with the
reason, so the plugin only ever sees a config its own schema accepts. Every
entry of a multi-entry plugin gets the same table and checks it against its own
`Config`. [hello.mts](examples/plugins/hello-extension/hello.mts) reads one
such setting.

- It is the user's data, not part of what was reviewed: the user can change
  it without a new trust review, which is why only the user's config can set it
  (a project's `.codewhale/config.toml` cannot).
- Plain TOML values only (a date-time is refused), at most 16 KiB serialized
  and 16 levels deep. A table over a limit fails that plugin's activation with
  the reason, in `/plugin show`; it is never delivered cut short.
- `/plugin show <name>` lists the configured keys (not the values) and says
  when the config is refused. Do not put a secret in it: the plugin's code
  reads every value, and so does any other plugin in the shared process.
- Codewhale reads `[plugins]` at start, and again at `/plugin reload` (or any
  plugin command that changes plugins). A plugin whose table changed is revoked
  and activated again as a new generation, with the new values; one whose table
  did not change is left alone. A file that cannot be read keeps the previous
  settings.

## Programmable tool admission

A reviewed native mod may register `ctx.on('tools/pre-execute', async (exec, next) => ...)`. The frozen call view carries `name`, `callId`, `arguments`, `signal`, `workspace`, `mode`, and `model`. It has no session, agent, tool, or approval handle. `next()` returns an abstention; Rust evaluates the remaining listeners and gates.

Return `{kind:'deny', reason}`, `{kind:'ask', reason?}`, `{kind:'revise', input}` (a JSON object), `{kind:'annotate', text}`, or `{kind:'abstain'}`. `undefined` also abstains. DSH-shaped `allow` is an abstention and logs a warning once per owner. A malformed answer, thrown error, timeout, or withdrawn owner fails the call closed. The listener batch has a five-second deadline and observes cancellation through `exec.signal`.

Native hooks run first, then mod listeners in core registration order. Denial always wins; the last accepted input revision wins. Rust re-prepares revised arguments and reruns the existing authority, policy, and approval checks. Input revisions use the existing 32 KiB hook limit; context and reasons use the existing sanitizers. Unloading a fiber removes its listener, and disabling or revoking an owner retires its admitted handles before host teardown. Other workspaces do not receive the call.

This bridge runs on Engine sessions, including model tools, gated code-mode calls, and `exec.core` calls. The standalone ACP execution path has no TypeScript host attachment. Prepend/global listener ordering, DSH runtime/agent handles, around-execution wrappers, and post-result rewriting are not provided. Native `tool_call_after` remains an observer contract.

## Asking the core to run a tool

A tool can ask the core to run one of the core's tools for it:

```ts
async execute({ path }, exec) {
  if (!exec.core) return { error: 'not run under the turn gate' }
  try {
    const { content, isError, structured } = await exec.core.call('read', { path })
    return { content, isError }
  } catch (error) {
    // error.name === 'CoreCallError'; error.code is 'refused' | 'denied' |
    // 'cancelled' | 'unavailable' | 'failed'
    return { error: error.code, message: error.message }
  }
}
```

The call is planned and approved exactly like a call the model makes: the
allow and deny lists, hooks, Auto-Review, repo law, the worker authority
envelope and the approval card all apply. **You never decide any of that.**
There is no way to approve, to supply a card's text, an argv, a URL or a
ticket. Rust composes the card ("Requested by `extension:<plugin>` from inside
its tool `<tool>`") and decides whether one is shown.

**When `exec.core` exists.** Only when the model called your tool directly and
the turn loop is serving its permission gate for that call. It is absent for a
command, a timer, activation code, and a tool run from inside
`execute_tools` (code mode gives nested tools no gate); the core refuses
a `core/call` from any of them. It also ends with the call: when your `execute`
returns, fails, times out or is cancelled, or your plugin is disabled, or the
host exits, pending core calls are cancelled and a waiting approval card is
withdrawn (recorded as cancelled; an answer given afterwards changes nothing).
Cancelling your `exec.signal` cancels them too, and `call(name, input, {signal})`
can cancel one.

**Refused outright** (`error.code === 'refused'`, nothing runs, no card): every
tool code mode refuses (`execute_tools`, the interpreters, `agent`, `workflow`,
`rlm`, `request_user_input`, interactive shells, sandbox escalation, Computer
Use consent and scripts, MCP sign-in); any extension tool, yours included (no
recursion); tool search and tool-result retrieval; the memory writer
(`remember`) and tools that change what the session may do or schedule work
(`request_plugin_install`, goals, automations, `send_later`, starting MCP
servers); and **every MCP tool, Computer Use included** (not in v1). Names match
case-insensitively and after the core resolves aliases and after any hook
rewrites the call.

**What prompts.** Every call needs approval except a read-only, workspace-local
tool from a short list (`read`, `read_file`, `list_dir`, `file_search`,
`grep_files`) when nothing else asks for one. Your plugin's own approvals are separate: the user approved your
tool, not what it asks the core to do. Approval keys are scoped to your plugin
build: a grant the user gave the model for a tool never covers your call of it,
and a grant for your call never covers the model's. **Shell and network calls
force a prompt**: a session grant is not consulted. Ask and Full Access
both ask the user every time; explicit session denials still refuse the call.
Auto-Review and Never refuse it without opening a card. In Full Access every
other call an extension makes is auto-approved, as it is for the model.
`error.code === 'denied'` is the user's "no": do not retry it.

**Limits**, per invocation: 50 core calls in all, 4 at once, and one approval
card at a time; per host, 256 requests in flight. A host that presents invalid
tickets in a burst is ended as a protocol violation. Your `tool/call` deadline
(120 s) stops while a core call waits on an approval card. The result is the
tool's text (`content`) and, when it was JSON, the parsed value
(`structured`); a long one is cut with a note, and images are dropped. What your
tool asked the core to run is recorded (bounded) in your tool result's
`core_calls` metadata, with each call's decision and outcome.

## Lifecycle, diagnostics and restarts

Use `/plugin show <name>` for that plugin's owner state, live tool names and
up to 20 recent retained diagnostic messages. The host retains a bounded 64-entry
shared diagnostic ring; this view is not a persistent per-plugin log. Warnings,
errors, activation/refusal/fault messages and teardown outcomes are attributed
to the plugin when known. Display text is escaped. `/plugin list` includes
overall host health and bounded diagnostics.

Do not block the event loop. Heartbeats mark an unanswered host Unresponsive
after 3 seconds and kill it after 10 seconds. Unexpected exits restart with a
short backoff; the third crash within 5 minutes stops automatic recovery.
Opening another engine does not reset that budget. Explicit plugin changes or
reload retry deliberately. Launch failures also allow a newly attached engine
to retry after a one-minute cooldown.

Recovery verifies current attachments and persisted trust again, then creates
fresh owner tokens and tool handles. Outstanding calls fail and are never
replayed; failed/faulted receipts remain suppressed until explicit retry or
changed authority. Two dirty teardowns within 10 minutes request maintenance
when active calls finish. This restart preserves the unexpected-crash budget.
Dispose within 2 seconds and avoid leaving background work behind.

## Sandbox

Trust is not a complete security boundary. Native plugins share one process
and data-access cohort and can interfere with each other. Owner labels and
JavaScript method locks do not isolate mutually untrusted plugins.

Native hosts use a positive filesystem-read policy on macOS Seatbelt and
Linux bubblewrap (`/usr/bin/bwrap`). Grants cover the selected runtime and
required system files, the host bundle, Core-managed reviewed snapshot stores,
and the Native data/private temporary directories. Ordinary workspace files,
including project `.env` files, are outside those grants. Child processes
inherit the same boundary. Direct network access is denied; writes stay in
the Native data and private temporary directories. The general shell profile's
Cargo, npm and Darwin cache write grants do not apply to this Native profile.

Snapshot-store grants include other reviewed snapshots in the shared Native
cohort, not just the currently selected entry. They do not grant repository
access. A plugin that directly runs `git` over the user's project therefore
needs a separately scoped operation bridge before it can work in this profile.

Native extensions require a verified OS wrapper at each launch. On Linux,
missing bwrap or a failed namespace probe refuses Native activation and reports
the concrete error. Windows uses the existing LPAC filesystem/network wrapper
and refuses Native activation when that wrapper cannot be established. Source
support is not evidence of platform or bundled-artifact qualification. The pinned Builtin tier retains
an explicitly diagnosed unsandboxed exception; every effect still requires
Rust operation tickets. That exception does not qualify Native extensions or
the mandatory-host/default-runtime cutover. Planning creates the sibling
Builtin data directory before masking it for Native launch. Review the
[current design limits](design/TS_EXTENSION_HOST.md) before enabling
third-party code.

## Limits

The host process has a 1 GiB memory cap. On Linux it is `RLIMIT_DATA` (or a
lower hard limit Codewhale itself inherited) and on Windows the Job Object's
per-process limit, so an allocation past it fails; both also apply to each
process an extension starts. Those two have been tested with a Node host only.
On macOS the Bun host applies a jetsam limit to itself before any extension
loads, and the kernel kills it past the cap; processes it starts are not
covered. A Node host on macOS has no kernel limit: Codewhale checks its
resident size at each 3-second heartbeat and kills it past the cap. Under Node
the JavaScript heap is also limited to 256 MiB. The host has a
32 MiB frame limit, 256 in-flight request
limit (plus a reserved heartbeat), 128 tools per owner and 1024 per host, and
64 commands per owner and 256 per host. Tool
descriptions are at most 4 KiB and schemas 64 KiB. Tool calls have a 120-second
deadline and commands a 30-second one. The feature stays Experimental and off by default; local fixture
tests do not establish sandbox parity, provider behavior or release readiness.
