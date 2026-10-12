# Codewhale TypeScript extension host: design

> **Repository copy.** This is the design as reviewed on 2026-09-25, copied from
> the private release plan (`codewhale-ops/releases/0.10.1/plans-20260925/`) so
> the code and its design live together. The "As built" sections below
> record where the implementation (`crates/tui/extension-host/`,
> `crates/tui/src/extension_host/`, behind `[features] extension_host`) differs
> from the text that follows. Where they disagree, the newest "As built"
> section and the code are current; the rest is the plan for later phases.

## As built: authored prompt sections, owner-local storage and invocation identity (2026-10-02)

`prompt` and `storage` are host-provided shim services whose replacement is
refused. `ctx.prompt.registerSection({id, text})` uses the existing owned
registration lifecycle and Rust's reviewed owner registry. Live snapshots are
scoped to one Engine attachment, validated against its desired plugin hashes
and current Native authority before and after collection, and sorted by
owner/id. Rust bounds raw text, section counts and the final attributed block;
the existing user-role runtime-message path delivers the complete bounded
snapshot at turn boundaries, including explicit withdrawal, without changing
the pinned system header or truncating it into workspace context deltas.
An author cannot replace the system prompt or choose another session. See
`docs/EXTENSIONS.md` for the exact byte/count limits and grammar.

`ctx.storage.get`, `set` and `delete` expose bounded plain JSON in the directory
Rust assigned to that owner. Atomic per-key replacement, corrupt-data refusal, symlink
refusal, serialized owner writes and live-owner checks preserve local state.
Storage has no session-history or credential access and introduces no protocol
method. It remains plugin-local state in the existing directory, rather than
another Codewhale session store.

`tool/call` and `command/run` add optional `session_id`, `agent_id` and
`origin_turn_id` wire strings. The host exposes only supplied fields as frozen
per-call `sessionId`, `agentId` and `originTurnId`; no identity is cached during
activation. A user slash command supplies its known session only. These are
labels, not handles or new authority. Older callers can omit every field.

## As built: programmable pre-execute mods (2026-10-02)

`ctx.on('tools/pre-execute', (exec, next) => ...)` now registers an owned `hook` handle through Cordis's `internal/listener` extension point. `hook/evaluate` is core-to-host only, carries a Rust-composed frozen call projection with DSH's `name`, `arguments`, `callId`, and cancellation `signal` spelling, and accepts monotonic `abstain`, `deny`, `ask`, `annotate`, and `revise` proposals. DSH `allow` maps to abstention; unsupported answers fail closed. No runtime, session, agent, invocation ticket, or approval handle is given to a listener.

`HostAttachment::tool_before_hooks` scopes dispatch to this engine's desired reviewed owners, validates Native authority and owner generation before and after dispatch, and uses the existing owner cancellation and host supervision. Native hook results followed by TypeScript proposals enter `fold_tool_call_before_results`; the existing `reprepare_tool_call_after_hook` and all later policy/approval gates consume the revised arguments. Strict no-verdict semantics cover malformed replies, errors, the bounded five-second listener batch, and revocation. Multiple listeners share the existing deny/ask/last-input/context fold. The real-host acceptance drives native reads and a read-to-write revision through a real Engine, alongside owner withdrawal and workspace isolation.

This is the bounded pre-execute part of phase 4. It does not implement DSH's around-execution waterfalls or replace its agent/token runtime; `next()` is an abstention rather than tool execution. `prepend`/`global`, post-result transformations, and lifecycle events remain absent. Standalone ACP has no TypeScript attachment; its native hook contract is preserved.

## As built: `core/call`, capability tickets and the gate for extension tools (2026-10-02)

Slice B of the tier plan, and the first thing the host can ask the core to do.
Still protocol v1 (a new method and an optional field); corpus `22`, `58`-`66`.
User-facing rules: `docs/EXTENSIONS.md`, "Asking the core to run a tool".
Nested shell and network calls force a fresh user prompt under the accepted
Slice B contract, including semantic action aliases. The separate routing hook
remains outstanding; programmable pre-execute proposals do not implement it.

- **Protocol.** `core/call` (host to core, request, both tiers)
  `{owner, ticket, name, input}`, answered with the existing `ToolResultWire`;
  `core_call::` below. New error codes `Refused` (-32002: policy, a limit, an
  invalid ticket) and `Denied` (-32003: the user declined the card).
  `tool/call` gains an optional `ticket`. `host_protocol_never_gains_core_authority`
  has its reviewed row for `core/call` and was not weakened (the method name
  mentions none of the core-only words); corpus `22` flipped from invalid to
  valid and was renamed.
- **Tickets** (`extension_host/ticket.rs`). A Rust-minted opaque id (244 random
  bits) indexing a Rust-side row `{kind, tier, host_generation, owner, method,
  target, expires, uses_left}`. `TicketKind` has one variant, `Invocation`
  (multi-use, budgeted); a process launch, fetch or MCP grant join with their
  redeemers. `redeem` checks every field under one mutex (kind, tier, host
  generation, the whole `OwnerRef`, method, a stated target as parsed JSON,
  expiry, uses) and refuses an unknown, expired, replayed (exhausted), wrong-owner,
  wrong-tier, wrong-generation or wrong-method ticket. Which field mismatched is
  not told to the host. Eight invalid presentations within a minute from one
  host process are a protocol violation (`HostRequestContext::violation` ends
  the host through the reader's kill path); a valid ticket merely out of uses
  does not count. Tickets are never persisted or logged (`Ticket`'s `Debug` is
  redacted; nothing quotes one) and are revoked when the invocation ends (the
  guard's `Drop`, so also when the tool call's future is dropped), when the
  owner is revoked (reconcile, `ext/faulted`) and when the host exits.
  Backstop expiry is 24 h: an invocation's life is bounded by its own deadline,
  which pauses while a person decides, so a short ttl would be wrong.
- **Where one exists.** `HostToolSpec::execute` mints an `Invocation` ticket only
  when the tool's context carries a `NestedCallGate` that the turn loop built
  *for this tool* (`NestedCallGate::for_extension(caller, specs)`; the tool
  checks the caller matches its own). The turn loop attaches one when a
  registry spec says `extension_caller()` (a `ToolSpec` method, default
  `None`), in `execute_tools_with_nested_gate`, the same function that serves an
  `execute_tools` program's gate. A sub-agent, a test, and a tool nested in
  `execute_tools` (whose invoker takes the gate from the nested context) have
  none, nor does any command, timer or activation: the tool gets no `exec.core`
  and a forged request finds no ticket.
- **One gate, one executor.** The turn loop serves the request with
  `gate_nested_call`, now parameterised by who is asking
  (`ToolCallSource::Extension`), so planning is `plan_tool_calls` unchanged
  (budget, allow/deny lists, preparation, hooks, ask-rules, Auto-Review, repo
  law, the authority envelope, the fleet guard). The caller side is the same
  `CodemodeInvoker` code mode uses (`for_extension`; `invoke` is now `call` plus
  a mapping): the concurrency cap, the exclusive/shared order lock, the
  `PauseClock`, the receipts, the result bounding and spill. What this removed
  rather than copied: nothing about the executor was duplicated; the only new
  code in `codemode.rs` is the withdraw plumbing and the `NestedFailure` type
  that lets a caller see the decision (code mode maps it back to
  `DriverError`).
- **Refused outright** (`core_call::refusal`), before planning on the host's
  name and again by the turn loop on the name planning resolved and the final
  input (after a hook rewrite): everything `refusal_before_gate` refuses; any
  extension tool (`ToolSpec::extension_caller`, found by case-insensitive name
  and by canonical alias), the caller's own tool included; `mcp_*` and the MCP
  resource tools, Computer Use included (founder's default: not in v1); tool
  search and `retrieve_tool_result`; `remember`; and a name table for what
  changes or schedules beyond the session (`request_plugin_install`, goals,
  `automation*`, `send_later`, starting MCP servers). Names compare ASCII
  case-insensitively and by canonical alias.
- **Approval** (`core_call::origin_approval`, applied at the end of
  `plan_tool_calls` for `Extension` and only ever raising). `Required` unless the
  tool is in `EXT_AUTO_ELIGIBLE` (`read`, `read_file`, `list_dir`, `file_search`,
  `grep_files`; a test pins each as a registered, read-only, auto-approved tool;
  the action-based `Git` tool is left out until its read-only actions can be
  told apart by name) *and* planning found nothing
  that asks. Shell and network (the registered tool's capabilities and concrete
  execution-envelope classification, plus the authority categories and `web.run`,
  `git_fetch`, `finance`, `run_tests`, `verify`, `run_verifiers`, `harness`)
  set `approval_force_prompt`. The card text is composed in Rust ("Requested by
  `extension:<plugin>` from inside its tool `<tool>` (core/call): ...") and the
  audit events say `caller: extension` with the extension and tool. Keys are
  origin-scoped (`extension_origin_approval_keys`: `extcall:<ext:plugin@hash>:`
  plus the usual key), so a grant for the model's call never covers an
  extension's and the reverse.
  *How "forced" meets the existing postures* (a documented choice, tested in
  `extension_calls_resolve_against_every_posture_as_documented`): the engine
  always raises the card with `approval_force_prompt`; what answers it is
  `resolve_approval_request_disposition`. Ask and Full Access: a modal every
  time, no session grant consulted. The Engine-minted `extcall:ext:` approval
  namespace identifies extension-origin calls; host labels cannot select this
  policy. Explicit session denials still win. Other forced policy holds retain
  their Full Access refusal. Auto-Review and Never refuse these extension calls
  as for any hold. Every other call an
  extension makes (Required, not forced) is, in Full Access, auto-approved as the
  model's would be, and in Ask promptable and groupable under the extension's
  own keys.
- **Two fixes the plan named.** (1) `await_tool_approval` takes an optional
  withdraw token (`request_tool_approval_until`; the old name is a wrapper):
  when it fires the wait ends with a `Cancelled` outcome in the approval log, a
  status line and a cancelled error, and the call is never decided for the
  person. The token fires on the host's `$/cancel` of the request, the owner's
  revocation, the host's exit and the invocation's end (which includes the
  tool call's deadline and the turn being cancelled); a request withdrawn
  before the server starts it is dropped unplanned (`NestedCallRequest::is_stale`).
  (2) `HostProcess::call_with_clock` measures the `tool/call` deadline on the
  invocation's `PauseClock`, paused while any `core/call` waits on the gate
  (`call` is the same with a clock nothing pauses), so a person taking a minute
  on a card does not time the tool out; the deadline is otherwise as before.
- **Caps.** Per invocation 50 core calls in all (the ticket's uses), 4 at once
  (code mode's `MAX_CONCURRENT_CALLS`; a fifth waits for a slot), and one
  approval card at a time (the turn loop serves one gate request at a time; a
  second call needing approval waits behind the first); per host 256 requests in
  flight (A2's table). The call's result metadata gets `core_calls`: at most 50
  receipts (decision, status, bytes, a note cut at 256 bytes).
- **Host.** `exec.core.call(name, input?, {signal?})` (`src/shims/core.ts`),
  present on a tool's `exec` only when the call carried a ticket (so the frozen
  `exec`'s keys are unchanged otherwise); it answers `{content, isError,
  structured?}` or rejects with `CoreCallError` whose `code` is `refused`,
  `denied`, `cancelled`, `unavailable` or `failed`. The host refuses locally
  what it must not send (a bad name, input that is not plain JSON). The tool's
  `exec.signal` and an optional per-call signal send `$/cancel` for the pending
  `core/call`. The ticket is never exposed to plugin code. No DSH equivalent was
  found in the vendored DSH surface (DSH reaches tools through its own
  `ToolRuntime` service, which the host deliberately does not provide), so there
  is only the Codewhale-native API.
- **Tests.** Rust: the ticket table (every field, expiry, exhaustion, burst
  window, revocation, redaction); spoofing against `serve` (wrong owner, other
  token, other tier, generation bump, unknown, after revoke/exit/end); the real
  host through a stand-in gate (read plus receipts, every refusal in many
  spellings before the gate is asked, no ticket without this tool's extension
  gate, a command has no `core`, a held approval outliving a 1 s deadline,
  withdrawal on drop, owner revoke and host kill, 50 total and 4 concurrent);
  the real turn loop with a fake extension tool (forced prompt in Ask and Full
  Access with Rust-composed card and extension-scoped keys, read unprompted and
  write carded, refused calls raise no card, a withdrawn card recorded
  cancelled, one card at a time) and `await_tool_approval`'s withdraw token on
  its own; the refusal list and the approval table; the posture matrix; the key
  scoping in both directions; the corpus and the lint. JS: `exec.core` against a
  fake core (payload, absence without a ticket, every error code, local refusals,
  cancel, concurrency).

Withdrawn calls emit the typed `ApprovalWithdrawn` identity after committing the
cancelled outcome. Terminal and runtime clients retire the matching card and
continue draining events; a queued allow cannot win over a ready cancellation.
Headless Full Access also refuses these forced holds.

Not done: no per-plugin process (a
ticket narrows a frame, it does not isolate plugins sharing the host, section
4.4); images and rich content from a core tool are dropped; the refusal table
names tools (a new mode/permission tool must be added); no `core/call` from a
command, a timer or activation (by design: they return a proposal); `mcp_*`
stays out until the MCP move.

## As built: the tier in the handshake, method tiers and host requests (2026-10-02)

Slice A2 of the tier plan. It adds the three things A1 listed as not done that
need no new method. Still protocol v1; the TypeScript shapes are regenerated and
corpus fixtures `51`-`57` cover the new hello fields.

- **`host/hello` says what the host is.** It now carries `tier`
  (`plugin`|`builtin`, the `--tier=` the host was started with) and
  `builtin_modules`, one `{id, sha256}` row per built-in module source the host
  *build* embeds, in id order. `build.mjs` builds the built-in modules first and
  substitutes their digests into the host bundle, so the bundle states them
  itself and stays deterministic. The core checks both the way it checks the
  runtime name and version, in `supervisor::check_hello_identity`, and refuses
  a mismatch before `host/initialize`: a tier other than the one in the launch
  plan, or a module list that is not exactly the one the Rust table
  (`tier::BUILTIN_MODULES`, carried on the launch as `builtin_modules`) pins.
  Both fields are required (a host that omits one is a protocol violation, not
  a legacy host: the bundle is embedded and always the same build). Today the
  table and the list are empty. This is drift protection between the two halves
  of one build, not authentication: a substituted host reports whatever it
  likes, as `bundle_sha256` already allowed (§4.4, threat 1).
- **A tier allow-list per method.** `protocol::MethodSpec` has `tiers`, the
  trust tiers whose host may send (host to core) or be sent (core to host) the
  method; `row()` gives both and a reserved method is written
  `MethodSpec { tiers: &[HostTier::Builtin], ..row(..) }`. It is enforced in both
  directions on both sides: `admit_in` refuses a frame from a host of the wrong
  tier like an unknown method (a protocol violation), `HostProcess::start_request`
  refuses to send a method the host's tier may not receive (`MethodNotFound`,
  nothing is written), and the host's `validateMessage(.., tier)` refuses both
  what it would send and what it is sent. The generated `METHODS` table carries
  `tiers`. No method is reserved yet (the first are the process broker, the
  fetch proxy and the MCP client: `proc/*`, `net/*`, `mcp/*`; a test pins that
  any method with those prefixes can never allow the plugin tier); the mechanism
  is tested with test-only reserved rows in Rust and in the host's tests.
- **Host requests are tasks.** The reader no longer answers `registry/*`
  inline. Each host request is admitted into an id table
  (`supervisor::InboundRequests`, at most 256 in flight and no id reused in
  flight, the same bound the host holds itself to; either is a protocol
  violation that ends the host), runs as its own task through
  `HostEvents::host_request` (an async trait method whose default answers the
  registry requests exactly as before) and is answered when it finishes. It is
  cancelled by the host's `$/cancel {id}` (the answer, if the handler still
  produces one, is dropped; a cancel for an id not in flight is ignored), by
  its owner's revocation (`revoke_calls_of`: the host is answered `Cancelled`)
  and by the host's exit (nobody is answered). A handler gets a
  `CancellationToken` it should wait on; one that ignores it is abandoned
  500 ms (`CANCEL_GRACE`) after the cancel, so a cancelled request never holds
  its slot. The host half: `RpcPeer.request(method, params, signal)` sends
  `$/cancel` for its id and rejects as cancelled when the signal aborts.
- **Tests.** Rust: the hello identity rules (tier, missing, extra, changed and
  repeated modules), a real host refused for a launched/reported tier mismatch
  and for a pinned module its bundle lacks, the id table (cancel, late answer,
  abandoned handler, revoke, exit, cap, id reuse, default registry answers), the
  tier rule over a reserved test table, the corpus under both tiers, and the
  generated-TypeScript drift test. JS: the corpus under both tiers, a reserved
  test row refused to a plugin host in both directions, the hello fields, and
  `RpcPeer` cancellation.

Not done: any reserved method (nothing needs the builtin-only tier before the
MCP move). The first host request that can be cancelled, `core/call`, is the
next section.

## As built: trust tiers (2026-10-02)

Slice A1 of the tier plan (CURRENT_DECISIONS §26 D3, §1.5 above): the host is
now two processes' worth of machinery, one per trust tier, and nothing else
about it changed. No wire change, no new behaviour for a plugin author. What
exists:

- **Two tiers** (`extension_host/tier.rs`, `HostTier { Plugin, Builtin }`).
  *Plugin* hosts reviewed third-party plugins; its owner ids are the plugin ids
  discovery builds (`<scope>/<12 hex>/<name>`). *Builtin* (tier 0) is for
  Codewhale's own host code and its owner ids are `host:<module>`. The id
  spaces cannot meet: `OwnerRegistry::begin_owner` takes the tier and refuses a
  `host:` id on the plugin tier and any other id on the builtin tier, and an
  authority that does not fit (a plugin owner needs its reviewed plugin
  authority, a module has none); every owner entry, tool and command
  registration records its tier. Discovery cannot produce a `host:` id (a
  manifest name cannot hold `:`, and an id starts with a scope name), a test
  shows such a plugin failing validation, and `desired_owners` checks the id
  once more before it reaches the host.
- **An empty production table.** `BUILTIN_MODULES` (`BuiltinModule { id,
  source_sha256, tools: &[Tier0Tool { name, approval }] }`) is empty, so
  nothing asks for the builtin tier and **it never spawns**; plugins behave
  exactly as before. "Needed" currently means "has a row". A tier-0 tool's
  approval comes only from this Rust table (`Auto` or `Required`); an unlisted
  tool or module is `Required`, and the module's own say changes nothing. It is
  still never read-only for plan mode. The manager holds the table
  (`ManagerShared::builtin_modules`, the production const unless a test builds
  the manager with `with_builtin_modules`), so a test exercises tier 0 without
  a production row. A module's source is
  expected at `<bundle dir>/builtin/<module>.mjs` and is activated only if its
  SHA-256 is the pinned one (otherwise a failed owner with the reason, no host
  started); nothing materializes such a file yet because there is no module.
- **Per-tier supervision.** `ManagerShared` holds two `TierRuntime`s (host
  slot, generation, spawn count, crash and restart state), lazily spawned. The
  owner registry, the runtime pin, the attachments and plugin settings stay
  shared, so both hosts run on the same pinned runtime and one registry says
  which tier an owner is in. A host's exit, restart, heartbeat and dirty
  teardown touch only its own tier (`OwnerRegistry::host_exited(tier, ..)`).
  A host answers `registry/*`, `log` and the like only for owners of its own
  tier. Lock order, restated at `ManagerShared` and kept: `sync_lock`, then one
  tier's host slot, then that tier's supervision state, then the registry; no
  path holds both tiers' slots, and the other locks are leaves.
- **Supervisor.** `plan_launch` and the sandbox plan take a tier. The argv ends
  `<bundle> --tier=plugin|builtin`. The plugin tier's data directory, working
  directory and writable root are what they always were,
  `extension-host/data`, so the per-plugin directories under it
  (`data/plugins/<name>-<hex>`, unchanged and pinned by a test) keep their
  data. The builtin tier's is **`extension-host/data-builtin`**, a sibling and
  not `data/builtin`: a child would lie inside the plugin tier's writable root,
  and the host sandbox has no per-subpath write deny, so plugin code could
  then write tier-0 state. The plugin tier's read deny list names the builtin
  directory explicitly (unit test on the path function). A built-in module's
  own directory is `data-builtin/modules/<module>`. Under bubblewrap a builtin
  directory that did not exist when the plugin host started cannot be masked;
  Seatbelt denies it by name before it exists.
- **Host.** `--tier=` is parsed before anything else; an unknown value, a bare
  `--tier` or a tier named twice exits with 64 before `host/hello` (no
  `--tier` means `plugin`, the least-privileged). `ext/activate` for an owner
  of the other tier (decided by the `host:` prefix) is refused with
  `InvalidParams`, before anything is read or loaded. `dist/` was rebuilt.
- **Drift check.** `build.mjs` builds each `src/builtin/<id>.ts` to
  `dist/builtin/<id>.mjs` and writes the SHA-256 of each to
  `dist/builtin-modules.json` (`{"modules": {}}` today); a Rust test
  (`tier::tests::table_matches_the_host_build`) fails when `BUILTIN_MODULES` and
  that file disagree in either direction. A module that would bundle a
  `node_modules` package fails the build until its licence notice is handled.
- **Activation policy** is untouched: v4 and its pinned digest test pass
  without change, because tier-0 modules are not plugins and no installed
  plugin is re-reviewed.
- **Tests.** Rust: owner/tier refusals and per-tier crash isolation in the
  registry, a host answering only for its own tier, a `host:`-named manifest
  failing validation, launch plans (argv, data directories, deny list, the
  unchanged per-plugin path), the drift test, the table's approval lookup, a
  tampered or missing module refused with no host started, production never
  spawning the builtin tier, and (test table) a tier-0 host spawning apart from
  the plugin host, activating `host:tier0-module`, its tools' approval
  following the table, a call through it, and a plugin-host crash leaving it
  alone. JS: refusal of bad `--tier` values, each tier refusing the other's
  owners, `host:` activation on the builtin tier, and the dist digest file
  matching `dist/builtin/`.

Not done, and not claimed: any real tier-0 module (the MCP move, §5, is the
first consumer); `host/hello` reporting a tier or module digests and a
per-method tier allow-list (done in slice A2, above); capability
tickets (landed with their first redeemer, `core/call`, in the section above); embedding or
materializing a module's source; a bundled tier-0 executable (D1); a demand
predicate that defers the builtin spawn until something needs it; the builtin
tier's sandbox denying the plugin tier's data directory (it can read it today);
`/plugin` listing the builtin host's modules and tools beyond one status line;
tier-0 commands or tools being offered to the model (an engine installs plugin
owners' tools only).

## As built: plugin context, several entries, input validation, notices (2026-10-01)

A review of the commands slice found four defects and one gap in what an author
can do. This section is what changed; where it disagrees with the older "As
built" sections below, this one is current.

- **Licence notices.** The embedded bundle contains MIT code (cordis,
  schemastery, cosmokit, dsh-util-values, and verbatim `dsh-tools` excerpts),
  and its banner promised `LICENSES.txt` beside it, which nothing wrote.
  `build.mjs` now generates `dist/LICENSES.txt` from the bundler's metafile
  (every package that contributed an input file, each with its own LICENSE
  file; a bundled package with none fails the build; the `dsh-tools` excerpt is
  listed from a named table that the build checks against the source file). The
  list was hand-kept before and named `@standard-schema/spec`, which is not
  bundled. The existing `git diff --exit-code -- dist` check covers it. Rust
  embeds it (`NOTICES`) and `materialize_bundle` writes it beside the bundle in
  the same digest-named directory, by the same staging, `0400`, rename and
  read-back as the bundle (so a symlink at its name is replaced, never written
  through). `THIRD_PARTY_NOTICES.md` lists the packages. Tests: the directory
  holds exactly the two files with the embedded bytes; and every
  `node_modules/<pkg>` marker in the *embedded bundle* has a section in
  `LICENSES.txt` and an entry in `THIRD_PARTY_NOTICES.md`, so the check does not
  trust the generator.
- **Several `native` entries.** The manifest has always accepted
  `native.paths` (up to 64), and the core sent one `ext/activate` per entry
  under one owner token, which the host answered with "owner token already
  active" for the second. No protocol change was needed: the host now accepts a
  further `ext/activate` for a live owner token if the previous entry finished
  activating, the plugin matches, and the path is new; each entry is a fiber of
  the same owner, so deactivation, revocation and crash handling are unchanged.
  Any failing entry fails the owner and the host disposes the earlier entries'
  fibers (rolling back their registrations) before answering; the core already
  drops the owner's registrations. The core merges the cumulative tool and
  command names of the answers and skips a path the manifest lists twice. Tests
  (JS and Rust, with `two-entries` and `two-entries-failing` fixtures): both
  entries live under one owner, one tool callable from each, a repeated or
  foreign entry refused, one `ext/deactivate` tearing both down, a throwing
  second entry leaving nothing of the first.
- **Input is validated by the core.** `HostToolSpec` checked nothing against
  the schema a plugin registered, so `additionalProperties: false` in the
  example was advice (DSH's `defineTool` validates inside the plugin; a raw
  registration did not). `registry::InputValidator` compiles the schema at
  registration with the `jsonschema` crate (already linked for Workflow
  `responseSchema`; it moved from `[dev-dependencies]` to `[dependencies]` of
  `codewhale-tui`, no new crate and no lockfile change). No `$ref` resolver is
  enabled, so a reference outside the schema fails to compile and is refused
  at registration with the reason, as is any invalid schema. `prepare` (before
  an approval card exists) and `execute` check each call and return
  `ToolError::InvalidInput` naming the violations (at most five, bounded), so
  the model can correct itself and the host sees nothing. The deferred-tool
  first-call check (`deferred_first_call_matches_schema`) is a separate,
  hand-written shape check of required and known field names; it leaves types
  to the tool, so it was not the path to reuse. Known limit: the validator
  runs on the channel's callback thread under the registry lock at
  registration (a schema is at most 64 KiB); schema `format` assertions follow
  the draft's own default.
- **Plugin context.** Core to host, three optional fields (protocol still v1;
  an older host ignores them, the generated TypeScript was regenerated):
  `ext/activate.data_dir`, `tool/call.workspace`, `command/run.workspace`
  (corpus `46`-`50`). The workspace is `ToolContext::workspace` of the calling
  session, or the workspace whose user registry loaded the command
  (`ExtensionCommandRef::workspace`); a path that is not UTF-8 is omitted. A
  plugin's `exec` is now frozen `{signal, callId, args, workspace?, dataDir?}`
  and a command invocation gains the same two. `dataDir` is
  `<home>/extension-host/data/plugins/<name>-<12 hex of sha256(plugin id)>`,
  created by Rust (0700) before activation: inside the host's single writable
  root, stable across generations, never deleted. It is not isolation between
  plugins that share the process (§4.4). No home, credential or other
  workspace path is passed, though `dataDir` and the entry path do reveal the
  Codewhale home's location.
- **Plugin configuration.** There was no per-plugin config (`config` was always
  `{}`). `[plugins."<name>".config]` in the *user's* `config.toml`
  (`config::PluginSettings`, a new field on `Config`; project scope reads an
  explicit key list that excludes it) becomes the second argument of `apply`.
  Cordis already validates it against an exported `Config` schema (a
  schemastery `Schema`) and applies defaults, so a mismatch fails activation
  with the field named; no host code was needed for that. Rust bounds it
  (`plugin_config.rs`: objects of plain TOML values, 16 KiB, 16 levels, no
  date-times) and refuses an over-limit table by failing that plugin's
  activation with the reason, before the host is asked. Each owner records the
  digest of the config it was activated with; reconcile treats a changed
  digest like a changed plugin (revoke, new generation), so settings re-read
  at `/plugin reload` re-activate exactly the plugins whose table changed. A
  refused config is hashed from its reason, so it is neither retried every turn
  nor stuck once fixed. `/plugin show` lists the configured keys (never the
  values) or the refusal. The example plugin reads one setting through its own
  `Config` schema.
- **Tests.** Rust: settings parse, size/depth/date-time refusal and digest
  behaviour; the real host with `plugin-context` (settings with the plugin's
  default filled in, per-call workspace, frozen context, a write into `dataDir`
  inside the sandbox, a command's workspace, no churn on unchanged settings, a
  new generation on a change, a schema-refused config, an oversize config, and
  recovery); `/plugin show` rendering and escaping. JS: the same against the
  fake core, and the typed example with and without its setting.

Not done: a running session does not watch `config.toml` (only `/plugin reload`
re-reads it); `[plugins]` is not profile-scoped; the review screen shows the
config keys through `/plugin show`, not the trust review itself; the `Config`
schema is not exported into the plugin's registered tool schema; there is still
no storage service beyond the directory; the Linux and Windows runs of the new
Rust tests did not happen on this machine.

## As built: extension commands (2026-10-01)

Phase 2's `command/run` landed in `crates/tui/src/extension_host/command.rs`,
the registry, the protocol and the host's `src/shims/commands.ts`, mirroring
how a tool flows: `registry/register` → owned entry (plugin id + generation,
never-reused handle) → adapter → call → teardown.

- **Protocol (still v1).** `registry/register` takes `kind: "command"`.
  `RegisterSpecWire` (was `ToolSpecWire`) has `name`, `description`,
  `input_schema?` and `argument_hint?`; the generator cannot express a
  per-kind union, so `RegisterParams::check_spec` (a tool needs `input_schema`
  and takes no hint; a command takes no schema) runs in `parse_host_message`
  and is mirrored in the host's `validateMessage`, and the corpus holds both
  (`20`, `37`–`40`). New core→host request `command/run`
  `{handle, command_id, raw_input, deadline_ms}`; `raw_input` is the argument
  text after the name, trimmed (the core's slash parser trims; the design's
  `agent: AgentRef` was not built, the host has no agent or session). Its
  answer, `CommandResultWire`, is `{kind: "success", text?}`,
  `{kind: "error", text}` or `{kind: "submit", prompt, text?}` (corpus `41`–`45`).
  `ext/activate`'s `ok` also reports `commands` (defaulted). The protocol lint
  (`host_protocol_never_gains_core_authority`) passes unchanged by
  construction: `command/run` names no event, store, approval, secret,
  credential, token, auth, turn, loop, session or prompt authority, and has a
  reviewed row saying why. `protocol.generated.ts` is regenerated by the
  drift test.
- **What a command can return.** What the user-command machinery already does:
  text shown as a `System` transcript cell (labelled with `/name` and
  `extension:<plugin>`; escapes stripped, cut at 64 KiB) and/or a prompt sent
  as the user's next message through the same `SendMessage` path a markdown
  command's template takes (visible; refused over 128 KiB, never truncated;
  empty refused). Nothing the host returns makes the core call a tool or the
  model itself: after a `submit` the turn and tool approval are the ordinary
  ones.
- **Rust.** `OwnerRegistry` holds commands beside tools with the same owner
  rules; one never-reused handle counter; `remove_registrations_of`,
  `mark_failed`, `revoke_owner`, `forget_owner`, `host_exited` and
  `revoke_all` remove an owner's commands with its tools; a stale
  `unregister` is a no-op. Admission (`register_command`): DSH's grammar
  (`^[a-z][a-z0-9_-]*$`, ≤ 64), refused if a built-in command or alias (or the
  fixed `jihua`/`zidong`) answers to the name, or another plugin holds it;
  descriptions ≤ 1 KiB and hints ≤ 256 bytes, single-line; 64 per owner, 256
  per host. Commands and tools are separate namespaces. The commands of the
  owners an engine's workspace snapshot desires, bound to the reviewed hash,
  are loaded by `UserCommandRegistry::load_extension_commands` into the
  existing user registry, last, so a user, workspace or manifest markdown
  command (or a built-in) always wins the spelling, with a load error; a
  global `command::epoch()` bumped on every registry or attachment change
  makes the registry reload. Entries carry the owner's reviewed authority, so
  disabling or untrusting the plugin hides them at once, before reconcile.
  Dispatch (`try_dispatch`) returns the new `AppAction::RunExtensionCommand`;
  the UI loop awaits `extension_host::run_command` (like `/balance`).
  `run` re-checks, immediately before sending, the policy flag, a running
  host (so a dead host answers `extension host is down: <why>` at once), the
  exact registration (handle and generation), the reviewed receipt and the
  `Native` capability, through the same `live_host` the tool path now uses.
  The call is bounded by `SupervisionOptions::command_run_deadline` (30 s),
  sent as `deadline_ms`, then `$/cancel`, like every method. Capability: the
  existing `Native`; no new one and no policy bump (§4.6).
- **Host.** `ctx.commands.register(definition)` returns an idempotent
  disposer and is an effect of the calling fiber. It accepts the Codewhale
  shape (`argumentHint`, handler may return a string, `{kind: 'submit'}`) and
  DSH's (`input: {hint}`, `{kind: 'success' | 'error'}`, `rawInput` with its
  leading separator; `@deepseek-ai/dsh-commands/brand`, the one value import
  such plugins make, resolves to two identity functions in
  `src/dsh/resolve-hooks.ts`, and the rest of that package still fails
  loudly). `commands` moved from "not provided" to provided;
  plugins still cannot provide it (the root provides its own shim once, frozen
  like `tools`). `src/shims/owned.ts` holds the register/undo bookkeeping
  both shims now share; the tool path was moved onto it. `dist/` was rebuilt.
- **Tests.** Host: register, each answer kind, cancel, refusal, dispose of
  one handle versus deactivation, invalid definitions, no provide/rewrite.
  Rust: registry rules (shadowing, caps, exact undo, revocation, crash),
  wire shapes, end-to-end through the real host and `commands::execute`
  (hints, per-workspace visibility, trimmed arguments, DSH `rawInput`,
  escapes, disable), clash refusal (built-in, other plugin, markdown wins),
  host-down, deadline cancel, and a killed host with a stale reference after
  replay.

Not done: Esc or a keypress cancelling a running command before its deadline
(the UI loop awaits it); the Runtime API and GPUI cannot list or run
extension commands (`GET /v1/commands` omits them); a clash with a
markdown command is resolved at registry load, not refused at registration;
`/plugin show`'s localized owner line does not list commands (the `/plugin`
host section does); no `agent`/session handle, attachments, `list`/`find`
on the DSH surface or `command/run`/`command/done` session events; an
extension command does not reset the goal, todos or plan as a markdown command
does; a command's `submit` prompt is not marked as plugin-authored beyond the
transcript line printed before it; the Bun runtime and Windows/Linux were not
run for the Rust tests of this slice.

## As built: script tools lose self-approval and shadowing (2026-09-30)

CURRENT_DECISIONS §26 D4 landed in Rust ahead of phase 4, so R1 below and
D9 describe the old behaviour. In `crates/tui/src/tools/plugin.rs` and
`ToolRegistry::apply_overrides`: a script's `# approval: auto` is ignored and
the tool gets the default a script with no `approval:` line gets (`Suggest`),
reported in the runtime log and `/plugin tools`; a `[tools.overrides]`
`script` / `command` entry keyed by a built-in is refused, named once in a
status line and in the runtime log, and the built-in stays active (`disabled` still works). D9(a) planned
`Required`, rememberable per tool: `Suggest` and `Required` resolve the same
way in `resolve_tool_permission`, and per-tool remembering was not built.

## As built: Bun runtime (2026-09-30)

The host can run on Bun as an opt-in: `[extension_host] runtime = "node" |
"bun" | "auto"`, default `node`. It follows CURRENT_DECISIONS §26 (D1/D2 and
the 2026-09-29 update "go to bun asap") and the 2026-09-29 Bun-vs-Node spike.
Bun may become the default only after four gates are proven on every
platform, followed by an explicit, recorded cutover. The table below is the
record, measured on macOS 26.1 arm64 with Bun 1.4.0 and Node 22.20, 24.19 and
26.10; there is no Linux or Windows Bun proof, so Node stays the default and
the diagnosed fallback. The same embedded bundle runs on either runtime.

| Gate | How it is closed | Evidence |
| --- | --- | --- |
| 1. `--no-install` always | `supervisor::runtime_args` passes `--no-install --no-env-file --config=<null device> --no-addons` to Bun | host test against a local recording registry (a control run without the flag does contact it); Rust launch-plan test |
| 2. OS-enforced memory cap | Linux `RLIMIT_DATA`; Windows Job Object per-process limit; macOS + Bun: a fatal jetsam limit the host applies to itself | host test (Bun on macOS: `memory_limit_mib` reported, same pid, SIGKILL at 300 MiB); Rust `memory_cap_stops_a_{bun,node}_host` |
| 3. FFI policy in the loader | `src/runtime.ts` locks `bun:ffi`, `Bun.FFI`, SQLite, Workers and ShadowRealm; the host refuses to start if a lock does not hold | host test with 13 entry points; each one is reachable when the lockdown is removed |
| 4. `host/hello` reports the real runtime | `runtime: {name, version}` from `process.versions.bun` first; the handshake refuses a runtime or version mismatch | corpus 01/21/31–35; Rust handshake-mismatch test; host handshake test |

- **Selection** (`dependencies::resolve_extension_host_runtime`). Unset
  means `node`, or `bun` when the table sets only a `bun` path
  (`ExtensionHostConfig::effective_runtime`). A configured `node`/`bun` path is
  the only candidate for its runtime and fails resolution with its reason
  rather than falling through. Otherwise each `bun` on `PATH`, then
  `$BUN_INSTALL/bin` or `~/.bun/bin` (or each `node` on `PATH`) is run, and
  the first at or above the floor (Bun **1.4.0**, Node `^22.19 || >=24`) is
  taken. A searched candidate inside a `node_modules` directory or the working
  directory is skipped without being run (not applied when the working
  directory contains the user's home). `auto` tries Bun first and records why
  Bun was not used when it takes Node. `bun` and `node` try only that runtime
  and never fall back. For Node the resolver also probes which of
  `--no-experimental-sqlite` and `--no-experimental-ffi` the binary accepts
  (`node <flag> --version`).
- **Pinned for the process.** The runtime is pinned once a host on it
  completes the handshake. Every restart reuses it without resolving it
  again, so a session cannot switch runtime. Under `auto`, a Bun host that
  fails to launch or handshake before anything is pinned is reported once and
  Node is resolved for the rest of the session. The handshake refuses a host
  whose `host/hello` reports a different runtime, or a different version than
  the pinned probe saw (the binary was replaced mid-session). `codewhale doctor`
  prints the resolution (what, which version, where, the fallback reason and
  how the memory cap is enforced). `/plugin` shows `running · pid … · bun
  1.4.0 · …` and a `runtime:` line with the summary and memory posture.
- **Protocol.** `host/hello.node_version` is replaced by
  `runtime: {name: "bun" | "node", version}`, read from `process.versions.bun`
  first (Bun emulates `process.versions.node`), plus an optional
  `memory_limit_mib` (below). The protocol integer stays 1: the bundle and the
  core always ship together. Corpus: `01`, `21`, `31`–`35`.
- **Flags and environment** (`supervisor::runtime_args`, `runtime_env`). Node
  keeps `--max-old-space-size=256 --disable-proto=throw --no-addons` and gets
  whichever of `--no-experimental-sqlite --no-experimental-ffi` it accepts.
  Bun ignores Node's heap and `__proto__` flags, so it gets `--no-install
  --no-env-file --config=/dev/null --no-addons` and `BUN_JSC_useShadowRealm=0`.
  Without them Bun fetches a missing package from npm while plugin code runs,
  and loads `.env` and `bunfig.toml` (which can preload code) from the working
  directory, which is the host's writable data dir.
- **Module resolution** (`src/dsh/resolve-hooks.ts`). Bun has no
  `module.registerHooks`, and its runtime `onResolve` is not called for bare
  package names. So the Bun branch uses `Bun.plugin` in three pieces:
  `build.module` serves each singleton by exact name. `onResolve` classifies
  the subpaths Bun does pass it. `onLoad` refuses any file under a
  `node_modules` copy of a peer (a second Cordis, a shipped
  `@deepseek-ai/dsh-*`). A peer that is not installed at all is mapped from
  Bun's `Cannot find package` to the same ``requires `X` `` error
  (`explainImportError`).
- **Native code** (`src/runtime.ts`, `denyNativeCode`). The checkpoint only
  locked the `bun:ffi` export object. Probing found five ways around that:
  `Bun.FFI` is a separate object with `dlopen`, `linkSymbols` and raw pointer
  reads and writes; a Web Worker or `worker_threads` Worker is a new realm
  with an untouched `bun:ffi`; `ShadowRealm#importValue('bun:ffi')` loads a
  fresh copy, and a `node:vm` context hands out a working `ShadowRealm`
  constructor even after the global is deleted; `bun:sqlite`
  `setCustomSQLite` and SQLite extensions `dlopen` arbitrary libraries. Node
  had gaps of its own: Node 26.10 ships `node:ffi` enabled, `--no-addons` does
  not cover it or `node:sqlite` extension loading, and a Worker given its own
  `execArgv` drops `--no-experimental-*`. Now, before any plugin loads:
  `process.dlopen`, `process.execve` (which would drop the flags and the
  macOS limit) and Worker threads are refused on both runtimes; under Bun,
  the `bun:ffi`, `Bun.FFI`, `bun:sqlite` and `node:sqlite` exports are
  throwing getters (for `node:sqlite`, whose ESM namespace Bun builds early,
  every prototype method throws) and ShadowRealm is off engine-wide; under
  Node, the two builtins are switched off by flags. The host then verifies
  each Bun lock through a real `import()`, and checks ShadowRealm and the Node
  builtins, and exits with an error rather than run with a lock that does not
  hold. A process a plugin starts is outside this policy (as under Node);
  Seatbelt stays the outer boundary on macOS. Known limit: this is a list of
  the entry points found; one a newer runtime adds is not covered until it is
  added.
- **Memory cap** (1 GiB, `supervisor::MemoryEnforcement`). Linux:
  `RLIMIT_DATA` between fork and exec (clamped to a lower inherited hard
  limit), so the kernel fails an allocation past it; measured once in a Linux
  container (2026-09-29), Node 24 cannot create the watchdog Worker at
  512 MiB and Bun 1.4 aborts at startup at 256 MiB, and both run at 1 GiB.
  Windows: the Job Object's per-process limit (`JOB_OBJECT_LIMIT_PROCESS_MEMORY`),
  set just after spawn when the host joins its job. Both apply to every process
  a plugin starts as well. macOS: `setrlimit(RLIMIT_AS/RLIMIT_DATA)` below the
  current mapping size returns `EINVAL`, and `memorystatus_control` returns
  `EPERM`. A fatal jetsam limit set as a `posix_spawn` attribute
  (`posix_spawnattr_setjetsam_ext`, libSystem SPI) works unprivileged, but any
  later `exec` clears it, so the core cannot set it on `sandbox-exec`. The Bun
  host therefore re-executes itself in place (`POSIX_SPAWN_SETEXEC`: same pid,
  process group, stdio and Seatbelt sandbox) with the limit, before any plugin
  loads, using `bun:ffi` before the lockdown takes it away, and reports
  `memory_limit_mib` in `host/hello`. The core accepts only the value it asked
  for (`CODEWHALE_HOST_MEMORY_LIMIT_MIB`). Past the limit the kernel SIGKILLs
  the host; the exit reason reports the SIGKILL and the configured limit but
  not a cause, since any SIGKILL looks the same. Processes the host starts are not
  covered. A Bun host that cannot apply the requested kernel limit is refused
  before initialization, with the reason retained in its stderr and `/plugin`
  diagnostics. A Node host on macOS is checked at each heartbeat instead,
  which lags by up to one interval. Node's 256 MB heap
  flag still applies everywhere.
- **Tests.** The host JS suite runs under `node --test` and `bun test`
  (`npm run test:bun`); each run spawns the host on the runtime running the
  suite. CI adds a JS-suite Bun leg pinned to 1.4.0 on Linux and keeps the
  Node leg. Rust covers the selection matrix and flag probe, the default and
  config rule, the per-runtime launch flags and environment, the runtime and
  version mismatch refusals, the `auto` fallback when Bun fails to start, a
  Bun end-to-end run with a crash restart that stays on Bun, and the memory
  cap: CI runs it with Node on Linux, macOS and Windows; the Bun case has run
  on macOS only.

2026-10-02 source checkpoint: `compile-host.mjs` builds the same canonical host
with an explicitly supplied local Bun and no runtime download. An adjacent
compiled image is optional for Bun/Auto and must report this Engine's exact
source digest; Node remains the default. Six local compiled-image/fake-Core
cases pass, including both tiers, same-PID macOS jetsam, FFI/Worker refusal,
embedded no-install flags and zero registry traffic. This is separate from
Rust execution, code signing, release packaging and installed-binary proof.

Native launch now refuses an absent or failed verified OS wrapper. The pinned
Builtin exception remains diagnosed and ticket-bound; it does not close D9.
The CI source requires Node and Bun in the Rust three-OS matrix and adds
source/compiled-host suites, but these jobs have not run on this source. Linux
and Windows isolation/memory receipts, release assets and the four D2 gates
remain required before a Bun default cutover. The Linux `RLIMIT_DATA` path
clamps to a lower inherited hard limit and reports the configured cap.
Reviewed import closures diagnose Bun query/fragment and dynamic CommonJS
identity limitations explicitly rather than choosing a different module.

## As built: phase 2a supervision (2026-09-29)

The experimental host now has bounded lifecycle supervision. It uses the same
Rust manager, owner registry, attachment snapshots, and approval gate:

- `host/ping` runs every 3 seconds. A ping unanswered for 3 seconds marks the
  host **Unresponsive**; after 10 seconds the existing process-tree supervisor
  kills it. A pong restores Ready only for that generation. Deadlines use a
  monotonic clock; laptop suspend/resume behavior has not been qualified.
- Unexpected exits, protocol violations and hang kills share one crash budget.
  Below 3 crashes in 5 minutes, the manager waits 250 ms then revalidates current
  attachments and replays eligible owners with fresh generations and tokens.
  The third crash stops recovery and retains the failure/stderr diagnostic.
  Opening another engine and replaying a host never reset this budget.
- In-flight tool calls fail with the existing typed unavailable error; they
  are **never replayed**. Failed/faulted receipts remain suppressed. A crash
  during the sole activating owner's initialization is attributed to that
  receipt, so other valid plugins can recover.
- Explicit plugin changes/reload clear the crash budget and retry failed
  receipts through the existing `plugins_changed` path. Start failures also
  permit a new engine attachment to retry once the one-minute cooldown has
  elapsed. There is no automatic handshake/start-failure loop.
- Old-generation callbacks and recovery tickets cannot mutate a newer host.
  Planned shutdown is not a crash. Native-entry, staged-byte, persisted-state,
  approval-grant and platform sandbox rules remain unchanged.
- Two incomplete, leaking, malformed or failed teardowns in ten minutes request
  one planned restart. The existing monitor waits until reconciliation and
  all non-heartbeat requests are idle, then atomically closes request admission
  before retiring the process tree. Current valid owners replay with fresh
  tokens; calls are never replayed. This maintenance neither consumes nor
  resets the unexpected-crash budget. Late old-process outcomes cannot dirty
  the replacement.

The authoring follow-up adds an escaped `/plugin show` owner section with state,
live tools and up to 20 recent attributed messages from the bounded 64-entry
shared diagnostic ring. Regular `.mts` entries use the same reviewed-byte and
discovery rules as `.mjs`/`.js`; Node strips erasable types. The executable
[hello extension](../examples/plugins/hello-extension/hello.mts) and
[author guide](../EXTENSIONS.md) describe the actual services and trust loop.

`exec.cwd` remains deferred: the current execution context does not expose the
caller's workspace path. Hooks, MCP, `core/call`, and sandbox parity
also remain subsequent work (commands landed: see the 2026-10-01 section). The following phase-1 section is its historical
receipt, including the earlier lack of heartbeat/restart and `.mts` support.

## As built: phase 1 (2026-09-25)

**Scope matches §8:** tools only, one host per engine process, protocol v1,
no `core/call`, no commands, hooks, MCP, heartbeat or auto-restart. The
differences from the text below:

- **An OS sandbox arrived early (§4.5 said phase 5).** On macOS the host runs
  under Seatbelt with a workspace-write profile rooted at
  `$CODEWHALE_HOME/extension-host/data`: no direct network, writes only there
  and in the temp dirs, and no reads of
  - every entry of the Codewhale homes (runtime home, `~/.codewhale`, legacy
    `~/.deepseek`) except `extension-host/`, `plugins/` and `builtin-plugins/`.
    Existing entries are denied by enumeration at launch, and the known stores
    (`secrets`, `tokens`, `state`, `sessions`, `config.toml.bak`, …) are denied
    by name even before they exist;
  - the Codex home (`$CODEX_HOME` / `~/.codex`, holding `auth.json`) and the DSH
    home (`$DSH_HOME` / `~/.dsh`, holding `.credentials.yaml`);
  - the default credential-store deny-list (`sandbox::read_guard`).

  **What it does not do:** other user-readable files, including project `.env`
  files, stay readable (the `.env` filename rule has no Seatbelt subpath form),
  and Mach services and `exec` are not restricted, so this is defense in depth,
  not the §4.5 containment. On Linux the same policy runs under bubblewrap
  when a launch-time probe shows bwrap works; each Codewhale home is masked
  whole and its readable entries bound again, since bwrap cannot deny a path
  that does not exist yet (`extension_host::supervisor` lists what that does
  not cover). Current Native admission refuses missing or failed verified
  wrappers, including Windows while filesystem/network isolation is absent.
  Only the pinned Builtin exception can run unsandboxed with its diagnostic;
  §4.5's full containment and mandatory-host cutover remain open.
- **Extension tool names that the approval path keys by name are refused.**
  Approval keys (`approval_cache`), approval-card summaries and the approval /
  auto-review category are derived from the tool name. A plugin tool named
  `web_fetch` would otherwise share `fetch_url`'s `net:<host>` session grant.
  `registry::core_special_case` probes those classifiers and refuses any name
  they special-case (`web_fetch`, `exec_wait`, `task_shell_start`, `read_*`,
  `get_*`, `list_*`, …), in addition to the §3.1 native and prefix rules.
- **Teardown of the whole process tree.** The host leads its own process group
  on Unix (a Job Object on Windows). It kills that group itself at stdin EOF,
  and a watchdog thread kills it when the host's parent process changes, even
  while a plugin blocks the event loop. A plugin child that calls `setsid`
  escapes the group. There is no Rust-side `host/shutdown` in production: the
  host is shared by every engine in the process and ends with it.
- **Revocation on plugin changes is immediate.** `/plugin disable`, `revoke`,
  `enable` and the runtime API's plugin actions call
  `extension_host::plugins_changed`, which reconciles the host at once instead
  of waiting for the next turn.
- **Shared-process disclosure (§4.4, threat 3).** When other plugins share the
  host, the approval card and `/plugin` say so. The tools shim's prototype is
  frozen, which stops one plugin rewriting `register` for the others; shared
  globals remain, so the disclosure is the control.
- **Core-service refusal does not depend on the caller.** Providing a refused
  service name through `ctx.root` fails like providing it through `ctx`.
- **Node resolution** skips relative `PATH` entries and probes each candidate
  with an empty environment.
- **The host re-hashes only the `native` entry file** before importing it, not
  the whole package closure (§4.4, threat 2). The rest of the staged snapshot is
  covered by Rust's per-call receipt check.
- **Measured** (this machine, Node 22.20, debug build): spawn + handshake +
  activation of the DSH fixture 78–95 ms; host RSS 61–63 MB with the watchdog
  thread (53 MB without it). `hyperfine` was not run.
- **DSH references** are pinned to `refs/dsh` commit `00102833`
  (`0.1.7-alpha.2`); the local checkout's HEAD has since moved to `0d1f50007f`.

**Phase-1 fixes (2026-09-28).**

- **Engines attach; they do not own the host.** The host and its owner
  registry are process-wide, but each engine holds a `HostAttachment` with
  its own workspace plugin snapshot. Reconcile activates the union of what
  every attached snapshot desires, re-verifying each snapshot against
  persisted plugin state (so a disable or revoke through any registry
  revokes everywhere), and revokes only owners no attachment desires. An
  engine installs only the tools of owners its own snapshot desires. Engines
  without a snapshot of their own (isolated chats, the empty fallback) do not
  attach, and dropping an attachment detaches without revoking. Before this,
  every engine's `sync` revoked whatever *its* registry did not desire, so
  two workspaces, or one isolated chat, cancelled each other's in-flight
  calls. The native-name set is now additive across engines.
- **Session grants are bound to the reviewed build (§4.3).** Extension tools
  key both the exact and the session-grant approval key as
  `ext:<plugin_id>@<content_hash>:<name>:<hash(input)>`, through the
  `ToolSpec::approval_scope` hook, so an updated plugin, or another plugin
  that later takes the same tool name, is asked again. This only narrows
  grants; widening them is an open decision.
- **The native-entry rule is checked at review time.** "One `.mjs` or `.js`
  file" is one function (`plugins::runtime::native_entry_problem`). With the
  flag on, discovery reports a violating entry as an error diagnostic, so
  `/plugin validate` and the review screen fail it, and activation refuses it.
- **Code mode suspends extension tools for approval.** Lane 6562 landed
  (#6583) before phase 1 (#6600), so acceptance 2's code-mode assertion is
  "suspends for approval": an `execute_tools` call of an extension tool in a
  main-session turn raises `<call>.<seq>` approval attributed to
  `extension:<plugin>`, sends no `tool/call` before approval, returns the
  result on allow and fails only that call on deny. Direct
  `execute_tools_tool` with no gate still refuses it before any host call.
- **The handshake timeout is 30 s**, not the 2 s §1.4 and §8 state
  (`supervisor::HANDSHAKE_DEADLINE`). 5 s failed on loaded Windows CI with a
  silent host (a cold `node` start plus an antivirus scan of the freshly
  materialized bundle), and a miss fails the host for the whole session. The
  handshake is off the first-prompt path, so 30 s (the MCP stdio handshake
  budget) costs nothing when the host is healthy.
- **A failed host is retried by a new engine, not by `/plugin enable`
  itself.** In the TUI every plugin change respawns the engine, and
  `Engine::new` calls `begin_session()`, which resets a failed host. On the
  runtime-API path a plugin action retries a failed host only when it makes
  a plugin the process has not yet seen desired; otherwise the host stays
  failed until a new thread engine starts. Explicit retry is phase-2
  supervision work.
- **`hyperfine` was never run** for acceptance 7; the flag-off guarantees
  rest on the never-spawned test and the pinned v3 policy digest.


Status: **proposal**, 2026-09-25. Written for the founder direction of that date: move plugins, hooks, commands, custom tools, agent presets and MCP to TypeScript, using the same model as the DSH (DeepSeek Harness) plugin system, and make only that part of Codewhale extensible.

This stage was read-only. I changed no source, branch, tracker or checkout. The only file written is this one. **Revised the same day after an adversarial review.** §0.1 lists the 13 findings (R1–R13), and the exact phase-1 scope is in §8.

## 0. Evidence base and corrections

| Source | What I actually read |
|---|---|
| Engine | `/private/tmp/cw-wt-6446`. **HEAD is `8a835d7c4`, not `58b1dd3dd`** as the Rust inventory states. It is two commits past `58b1dd3dd` (#6571, #6483), and neither touches plugins or MCP. Line numbers below were re-checked against `8a835d7c4`. |
| Code-mode lane | `feat/code-mode-mcp-6562` (`fda77b64a`, `18241e3e8`, `ebda21f02`), read with `git show` |
| DSH | `/Volumes/VIXinSSD/CW/refs/dsh` at `00102833` (`0.1.7-alpha.2`), read with `git show` |
| Research inputs | The DSH, RUST and REFS memos embedded in the task. I re-verified the load-bearing claims listed below. |

**Checks I ran myself** (greps and `git show` only; nothing was built):

- **Lane code:**
  - `NestedCallGate { mcp_pool, … }` is at `codemode.rs:200`. `refusal_before_gate` is at :250 and `ungated_admission` at :502. The MCP branch in `execute` is at :533–556.
  - `gate_nested_call` plans through `plan_tool_calls(…, ToolCallSource::CodeMode)` with `<parent>.<seq>` ids (lane `turn_loop.rs`, around line 4706).
- **Feature flags:** `Feature` and `Stage::{Experimental, Beta, Stable}` live in `crates/runtime/src/features.rs`, with a `[features]` table in `config.example.toml:1228`.
- **Activation policy:** `ACTIVATION_POLICY_VERSION = 3` (`plugins/activation.rs:22`). Its capability set already has a **`Native` capability that is inventoried but inactive** (`activation.rs:93`), and a manifest field `native` with the alias `native_extension` (`manifest.rs:51`).
- **Built-in bundles:** `plugins/builtin.rs` embeds `crates/tui/plugins/computer-use` with `include_bytes!` and materializes it under `$CODEWHALE_HOME/builtin-plugins` as a digest-named snapshot. **This is the shipping path the host bundle will reuse.**
- **ToolCallSource:** `crates/tools/src/lib.rs:386` has `ToolCallSource { Direct, JsRepl }`. The lane adds a second, private `ToolCallSource { Model, CodeMode }` at `turn_loop.rs:39`.
- **MCP boot:** it is already lazy. `tool_selection_covers_server` drives the lazy boot and `tools_always_load` (`mcp.rs:2852`, `config.rs:4863`), `McpServerConfig.required` is at `mcp.rs:571`, and `McpPool::new` hashes the config.
- **DSH plugins:**
  - They import runtime helpers by package name. My grep counted 109 `from '@deepseek-ai/dsh-tools'` imports in `packages/**/src`.
  - `@deepseek-ai/dsh-mcp-client` spawns stdio servers **itself**, through `StdioClientTransport` from `@modelcontextprotocol/client` 2.0.0 (`transport.ts:10,34`).
  - The DSH root requires Node `^22.19.0 || >=24.0.0`.
- **Local Node:** the first `node` on `PATH` on this machine (`/opt/homebrew/bin/node`, 25.8.0) **fails to start** because `libllhttp.9.3.dylib` is missing. `/usr/local/bin/node` is v22.20.0. Spawning `node -e ''` six times took **96, 23, 22, 27, 31 and 22 ms**, measured with Python `perf_counter` around `subprocess.run`. That is the only measurement in this document.

**Two facts the plan depends on:**
- **Node 20 reached end-of-life on 2026-04-30.** This is from my knowledge, not checked in this session. Adding a *new* runtime dependency with a floor of 20 would ship on an unsupported runtime.
- **The local Homebrew breakage is a real failure mode.** The host must resolve Node by *running* it, not by finding it on `PATH`. **Correction (review round 2):** `resolve_node()` (`dependencies.rs:278`) is *not* reusable as is. It runs `node --version` once, only for the first `node` on `PATH`, caches the answer in a process-wide `OnceLock`, and never parses the version. On this machine it would return `None` (Homebrew's broken node comes first), and the host would be unavailable although a working 22.20 is installed. Phase 1 adds a candidate ladder with a version floor (§8).

### 0.1 Adversarial review, round 2 (2026-09-25)

Checked against `/private/tmp/cw-wt-6446` @ `8a835d7c4`, lane `feat/code-mode-mcp-6562` @ `ebda21f02`, and `refs/dsh` @ `00102833` (the checkout's own HEAD has moved on to `0d1f50007f`, so every DSH reference here is pinned to the commit, not to HEAD). Greps and `git show` only; nothing was built or run.

**Facts that changed the design:**

| # | Finding | Evidence | Consequence |
|---|---|---|---|
| R1 | **A second custom-tool system already ships, outside plugin trust.** Scripts in `~/.codewhale/tools/` become model-visible tools on every turn. Each declares its own approval in frontmatter (`# approval: auto`). `ToolRegistry::register` *overwrites* a built-in of the same name with only a `warn!`. `[tools.overrides]` `Script` / `Command` entries replace built-ins on purpose. | `tools/plugin.rs:1-20,111`; `tools/registry.rs:53-63,375-414`; `core/engine.rs:4910,7360-7385` | Today, a script already approves itself and shadows built-ins. The founder's "only the TS host is extensible" requires moving this, so it is added to the deletion plan (§7) and decision D9. The host must not be weaker than this path, and must not copy it either. |
| R2 | **Registry tools are the existing seam for extension tools; `ExternalToolDispatch` is not needed in phase 1.** The registry is rebuilt every turn (`build_turn_tool_registry_and_catalog`). Tools outside `DEFAULT_ACTIVE_NATIVE_TOOLS` are deferred by default (`tool_catalog.rs:136-149`). On main, code mode already sees registry tools and refuses the ones that need approval (`codemode.rs:233-238`). The lane gates "native, plugin, or MCP" nested calls with approval suspension (lane `codemode.rs:1-25`). | as cited | Phase-1 extension tools are `ToolSpec` adapters registered next to `configure_plugin_tools`. They get plan mode, the authority envelope, deferral, approval and code-mode gating from code that already exists. **Phase 1 does not depend on the unmerged lane.** `ExternalToolDispatch` is left as an MCP-only interface that the lane may adopt (§5.2). |
| R3 | **The lane is not merged.** `origin/main` has code-mode Phase 1 (`e23ce514c`, which runs Auto-only nested calls and refuses MCP). The lane is 3 commits ahead. | `git log origin/main..feat/code-mode-mcp-6562` | Phase-1 acceptance cannot require "gated identically from `execute_tools` with `<parent>.<seq>` ids". On main, the assertion is "refused as needs-approval". Once the lane lands, it is "suspends for approval". **Resolved:** the lane landed first (#6583, then #6600), and the phase-1 fixes assert "suspends for approval" (`execute_tools_gates_an_extension_tool_before_any_host_call`). |
| R4 | **The self-declared read-only hint is an auto-approve.** `approval_hint_for` → `TrustedReadOnly` → `ApprovalRequirement::Auto` (`mcp.rs:1265-1274`, `tool_preparation.rs:46-48`, test at `:537-540`). | as cited | If extension tools honoured `presentCall` / `kind: 'read'` (old §4.3), a plugin would switch off approval for its own tools, and those tools run arbitrary Node. **Removed.** Extension tools are always `Required` (§4.3). |
| R5 | **Secrets are on disk, readable by any same-user process.** The default secret backend is `~/.codewhale/secrets/` (`crates/secrets/src/lib.rs:64-69`). MCP OAuth tokens are re-read "from the on-disk credential" (`mcp/oauth.rs:749-752`). | as cited | "Tokens never enter Node" and "one gate *even if the host is compromised*" (old §4.4, §5.1) are false until the phase-5 sandbox denies those paths. They are restated as protocol properties, not containment (§4.1, §4.4). |
| R6 | **The protocol names a mechanism that does not exist.** No `change:tool_surface` exists anywhere in `crates/tui/src`. | grep | Removed. Per-turn rebuild plus a liveness check at dispatch time is enough (§3.3). |
| R7 | **Computer Use is an MCP stdio server (`mcp/server.mjs`), not host code.** `agent.mjs` is the one-shot or `--serve` *remote SSH agent* that runs on another computer. | `crates/tui/plugins/computer-use/{package.json:12, agent.mjs:1-4}` | "Computer Use runs in host #0" (old §1.5) and the phase-2 `agent.mjs` item were wrong, and both are removed. CU stays a separate process. In phase 3 it is spawned by the broker like any stdio server. |
| R8 | **The manifest is closed.** `CodewhalePluginExtension` is `deny_unknown_fields` (`agent_plugin.rs:627-653`, test `:1646-1652`). A new `extensions."net.codewhale".host` key makes every *older* build reject the whole plugin. The existing `native` field and `Native` capability are exactly "executable extension, hashed, inventoried, inactive" (`manifest.rs:51,254`, `activation.rs:93`). | as cited | Phase 1 **activates `native`** as the host entry instead of adding `host` and later retiring `Native` (§4.6). Older builds keep loading the plugin's other components and report native as inactive. Only one executable-extension concept ever exists. |
| R9 | **One host per "session runtime" multiplies memory.** Sub-agents share the parent's `McpPool` (`tools/subagent/mod.rs:2758,3204`), and the runtime API runs many threads per process. | as cited | With a fan-out of 8, a host per session would cost 8 × ≤80 MB. Changed to **one host per engine process per trust tier**, with registrations scoped per session (§1.5). |
| R10 | **CI does not run root `npm test`, and its JS jobs pin Node 20 or 22.** The web job runs `npm test` inside `web/`. The CU and bridge suites use explicit `cd … && npm test` steps (`ci.yml:280-307`). | as cited | "The gate runs it automatically" is true only for the local gate. Phase 1 adds an explicit CI step on Node 22 (§8). Without it, the real-bundle Rust test would skip on CI and nobody would notice. |
| R11 | **The phase-1 DSH plugin has no `dsh.bundle.patch`.** Its `schemastery` is a regular *dependency*, not a peer, and the repository has only `src/index.ts`. The installable artefact is the published `lib/index.js`. | `git show 00102833:packages/skill/tool-workspace-dependencies/package.json` | Phase 1 cannot rely on recognising `dsh.bundle.patch`. The fixture is a Codewhale bundle whose `native` entry is a Cordis plugin that loads the vendored DSH `lib/index.js` with literal config. The resolve hook maps `schemastery` whether it is declared as a peer or as a dependency (§6.2). |
| R12 | **The phase-3 grant check was a denylist.** It inspected only `tools/call`, `resources/read` and `prompts/get`, so newer MCP methods (tasks, subscriptions, completion) would pass ungated. | old §4.4 | Changed to a default-deny method allowlist, with `arguments` compared as parsed JSON values, not by hashing re-serialized bytes (§4.4). |
| R13 | **Phase 1 was too large for one slice.** It combined commands, heartbeat, auto-restart with replay, `dsh.bundle.patch` recognition, a 3-capability policy split, and a dependency on the lane. | old §8 | Cut to tools only. §8 lists the exact scope. |

**Claims re-verified and left unchanged:** `ACTIVATION_POLICY_VERSION = 3`, and the whole policy (supported plus inactive lists) is hashed into every receipt (`activation.rs:107-122`), so any policy change re-reviews every plugin. `builtin.rs` digest-named materialisation. `ReviewedStdioLaunch` (`mcp.rs:929`; note that on macOS a Node entry is "hash-checked here, then reopened by path"). `HookProcessTree` / `WindowsHookJob` (`hooks/executor.rs:908-990`, private to that module). `McpCatalogBudget` (`mcp.rs:1546`). `PendingAuthorityWatch` (`mcp.rs:1661`). `ToolCallDecision::Allow` no-op (`turn_loop.rs:6364`). `updated_input` re-plan (`turn_loop.rs:3238`). `execute_mcp_tool_with_pool` (`tool_execution.rs:232`). Lane `NestedCallGate` at `:200`, `refusal_before_gate` at `:250`, `ungated_admission` at `:502`, `execute` at `:530`. Lane `ToolCallSource { Model, CodeMode }` at `turn_loop.rs:39`. `crates/tools` `ToolCallSource { Direct, JsRepl }`: its only users are the tests in `crates/tools/tests/parity_tools.rs`. DSH `mcp-client` spawns through `StdioClientTransport` (`transport.ts:10,34`). DSH `engines` is `^22.19.0 || >=24.0.0`. `vendor/cordis` is `4.0.4` with `publishConfig.access: public` (publication itself is still unverified offline).

---

## 1. Architecture

### 1.1 Shape

```
             ┌──────────────────────────── Rust core (closed, authoritative) ─────────────────────────────┐
 model ─────▶│ turn loop ─ plan_tool_calls ─ hooks fold ─ approval ─ sandbox ─ store ─ prompt ─ credentials │
             │      ▲                 ▲                                                                    │
             │      │ ToolRegistry (HostToolSpec, ph.1) · ExternalToolDispatch (MCP, ph.3)                 │
             │  HostSupervisor ── OwnerRegistry(owner, generation, handle) ── ProcessBroker ── FetchProxy   │
             └──────┬──────────────────────────────────────────────────────────────────────────▲──────────┘
                    │ one framed channel (stdin/stdout of the child), typed, versioned            │ spawn/kill,
                    ▼                                                                             │ egress
             ┌──────────────── codewhale-extension-host (Node ≥22.19, one process) ───────────────┐  │
             │ Cordis root ── shim services (tools, commands, skills, systemPrompt*, mcp, logger) │──┘
             │   ├── plugin fiber A (DSH package)     ├── plugin fiber B (Codewhale TS plugin)    │
             │   └── builtin:mcp  (official MCP SDK clients, one fiber per server)                │
             └────────────────────────────────────────────────────────────────────────────────────┘
```

**The host has no turn loop, event store, prompt authority or approval.** Everything it does is one of three things:
- *registers* something, which Rust admits or refuses;
- *answers* a Rust request (`tool/call`, `command/run`, `hook/evaluate`, `mcp/*`);
- *asks* Rust to act (`core/call`, `proc/*`, `net/fetch`, `prompt/propose`), and Rust gates the action exactly as it would gate a model call.

### 1.2 Where it lives in the repo

The package goes at **`crates/tui/extension-host/`**, named `@codewhale/extension-host` with `private: true`. **It is not a root workspace member** (correction): npm ignores a member's own lockfile, and the host needs its own pinned lockfile, as `computer-use` has.

**Why not `npm/extension-host`?** The bundle has to be `include_bytes!`-embedded by `codewhale-tui`. `builtin.rs` already puts Computer Use under `crates/tui/plugins/` so that the embed path stays inside the crate, which is what `cargo package` / `cargo install` need. The host follows the same precedent.

**Root `package.json` change:** extend the `test` script the way `web` already is: `npm test --workspaces --if-present && npm --prefix crates/tui/extension-host test && npm --prefix web test`. The local gate then covers the host. **CI does not run root `npm test`** (R10). Phase 1 therefore adds an explicit step to the existing Node-22 JS job in `ci.yml`, next to the Computer Use suites: `(cd crates/tui/extension-host && npm ci && npm test && npm run build && git diff --exit-code dist)`.

**The tests run against the committed `dist/` bundle and `node:test` only, so `npm test` needs no install.** This keeps the same property the CU and bridge suites have ("npm test works without npm ci", `ci.yml:284-286`). Only the build and drift check need `npm ci`. The package keeps its own `package-lock.json` (as `computer-use` does), so the root lockfile, which carries `wrangler`, is not churned.

**Layout:**

```
crates/tui/extension-host/
  package.json          engines.node "^22.19.0 || >=24.0.0"; build + test scripts
  src/main.ts           boot: framing, console rebinding, parent watchdog, crash attribution
  src/protocol.generated.ts  GENERATED from the Rust protocol types (constants, method table, params shapes, types)
  src/protocol.ts       frame codec + envelope checks over the generated shapes
  src/root.ts           Cordis root, shim services, refusal list
  src/shims/{tools,commands,skills,system-prompt,mcp-resources,logger}.ts
  src/dsh/{resolve-hooks,profile,dsh-tools-compat}.ts
  src/mcp/{service,broker-stdio-transport,proxied-fetch}.ts        (phase 3)
  dist/codewhale-extension-host.mjs   committed esbuild output, single file, + LICENSES.txt
  test/*.test.mjs                     node --test, importing dist/ only (no type stripping, no install)
```

- **Committed build.** `dist/` is committed so that `cargo build` never needs Node or npm. CI runs `npm run build -w @codewhale/extension-host && git diff --exit-code crates/tui/extension-host/dist`. This is the same guarantee Computer Use gets today by shipping plain `.mjs`.
- **Dependencies are bundled** into the one file:
  - `@deepseek-ai/cordis` 4.0.x
  - `@deepseek-ai/schemastery`
  - `cosmokit`
  - the loader/include pieces we use
  - later, `@modelcontextprotocol/client` 2.0.0

  They are pinned by exact version plus lockfile integrity. MIT notices go into `dist/LICENSES.txt` and `THIRD_PARTY_NOTICES.md`.
- **Unverified: whether `@deepseek-ai/cordis` is published to npm.** DSH depends on it as `workspace:~`, and I had no network access. If it is not published, we vendor `refs/dsh/vendor/{cordis,loader,include}` at `00102833` under `extension-host/vendor/`, keeping the MIT notices and DSH's `vendor/README.md` modification list. Phase 1 has to settle this first.

### 1.3 How it ships in every distribution channel

The host bundle ships the same way as Computer Use. It rides **inside the Rust binary**, so the npm, tarball, `cargo install`, brew, AUR, winget, FreeBSD, nix and Docker channels need no changes.

At first use, `extension_host::materialize()`:
- writes the bundle to `$CODEWHALE_HOME/extension-host/<sha256>/codewhale-extension-host.mjs` (read-only, digest-named, and never overwritten across builds, as `builtin.rs` already does);
- launches it with the reviewed-launch discipline from `ReviewedStdioLaunch`: the digest is re-checked on the opened file before exec.

**Node is the only new external requirement**, and only for users who enable a host plugin or, after phase 3, any MCP server. Per channel:

| Channel | Node situation | What we do |
|---|---|---|
| npm (`npm/codewhale`) | Node is present, but its `engines` floor is `>=18` | The host checks its own floor; the npm package floor is unchanged |
| brew / tarball / cargo / AUR / winget / FreeBSD | Node is often absent | `codewhale doctor` and `/plugin` report "extension host needs Node ≥22.19 (found: none / 20.x / broken)". Setting `[extension_host] node = "/path/to/node"` overrides discovery |
| nix | `nix/package.nix` | Phases 1–2: nothing, because the host is opt-in. Phase 3: add `nodejs_22` to the wrapper's `PATH`, since MCP then needs it. Do not add a runtime closure dependency for an experimental flag |
| Docker (`packaging/docker`, `Dockerfile`) | — | Phase 3: install Node 22 in the image |
| `cargo install --git` / `--path` | Builds from a checkout | `include_bytes!` reads the committed `dist/`, so no Node is needed to *build*. `codewhale-tui` is not published to crates.io (no `cargo publish` in the release workflows), so there is no crate-size limit to plan around |
| Termux / Android | Node 22 is available as a Termux package; unverified on our targets | Doctor diagnostic only |

**The founder must decide (§9, D1):** whether MCP-only users get a bundled Node runtime or a diagnostic. My recommendation is a diagnostic in phases 1–3, then revisit with install telemetry before MCP is moved by default. That is phase 3's exit gate.

### 1.4 Process lifecycle and supervision

**Lazy spawn.** The host process starts only when one of these first happens:
- an enabled, reviewed plugin declares a `native` (host code) entry and the flag is on;
- (phase 3+) an MCP connect is needed.

This follows VS Code's `lazyCreateExtensionHostManager`. The host never blocks the first prompt:
- **Phase 1:** the host is spawned at session start, in the background, when an enabled plugin has a `native` entry. Tools it registers join the registry at the next turn's rebuild, deferred, so they are reachable through `tool_search`. The first prompt never waits, and nothing is cached.
- **Phase 3:** MCP tools are advertised from the Rust-side catalog cache (§5.4).
- Either way, tools that arrive late enter through the deferred catalog / `tool_search` or `execute_tools`, so the pinned prefix is never rebuilt.

**Node resolution** (new in phase 1, `dependencies.rs`). `resolve_node_at_least(22, 19)` tries candidates in order:
1. `[extension_host] node`;
2. every `node` / `node.exe` found by scanning `PATH` in order, not only the first;
3. stop at the first candidate whose `--version` both runs and parses at or above the floor.

The result is cached per process, together with the rejected candidates and why each was rejected, for `/plugin` and `codewhale doctor`. The existing `resolve_node()` keeps its contract for `js_execution`. Converging the two is a later cleanup, not phase 1.

**Spawn.** Rust spawns `node --max-old-space-size=256 --disable-proto=throw --no-addons <bundle>`. `--disable-proto=throw` stays only if the phase-1 fixtures run under it: DSH writes `__proto__` keys through `defineProperty` (`dsh-tools` `schema.ts:242`), which it allows, but other packages are unverified.
- `--no-addons` follows decision D6, which recommends refusing native modules in v1. If D6 goes the other way, the flag is dropped for the trust tier that allows them.
- The process is placed in its own process group (Unix) or job object (Windows), reusing the hooks' `HookProcessTree` / `WindowsHookJob`.
- The environment is scrubbed like MCP's `child_env`. **No credentials enter it.**
- `CODEWHALE_HOST_PARENT_PID` carries the parent PID.

**Handshake:** `host/hello` (host → core), then `host/initialize` (core → host), then `host/ready`, all within 2 s. A version mismatch makes the host exit with code 78 (EX_CONFIG) and puts a diagnostic in `/plugin`.

**Watchdogs:**

- **In the host:**
  - It exits on **EOF of stdin**, which is the protocol channel. The pipe closes when the core dies for any reason, so this is exact and immune to PID reuse. (Corrected: an earlier draft polled the parent PID every 1 s.) `CODEWHALE_HOST_PARENT_PID` is kept only for diagnostics.
  - Global `uncaughtException` / `unhandledRejection` handlers attribute the error to the owning fiber, using Cordis fiber context or an async-context tag set around each invocation. They report `ext/faulted {owner}` and dispose that fiber. The process survives.
  - `process.exit` and `process.abort` are replaced with a thrower. This guards against plugin *bugs* that would kill the host. It is not a security control, since `process.kill(process.pid)` still works.
- **In Rust (phase 2; phase 1 relies on per-call deadlines and process exit only):**
  - Heartbeat `host/ping` every 3 s. A missed 3 s window marks the host **unresponsive**, which the status line shows. After 10 s the host is killed and restarted.
  - Crash tracker: 3 crashes in 5 min stops restarts. The host is then shown as failed with its last stderr. Nothing restarts silently after that.
- **Phase 1 has no auto-restart.** When the host exits, the generation bump below runs, the host is marked *failed* with its stderr tail in `/plugin`, and it respawns only on the next session or on an explicit `/plugin enable`. That is the whole crash story for phase 1, and it cannot loop.

**Restart = generation bump.** When the host dies, Rust does the following, synchronously:
1. Drops every registration the host owned (§3).
2. Fails in-flight `tool/call` / `command/run` / `hook/evaluate` / `mcp/*` with a typed `ToolError::not_available("extension host restarted")`.
3. Kills brokered children.
4. Emits a core status event. The host itself never emits session events.

Rust then respawns the host and replays activations from its own record of which plugins are enabled. Replaying registrations is the plugin's job: its `apply` runs again. Rust keeps no replay log of the host's calls.

**Shutdown.** `host/shutdown` has a 2 s budget; this matches omp's `session_shutdown` cap, so Ctrl-C is never held hostage. After that comes SIGTERM, then SIGKILL of the process group at 3 s.

**Stdout discipline.** The protocol runs on the child's stdin/stdout.
- Before any plugin loads, `main.ts` rebinds `console.*` and replaces `process.stdout.write` with a writer to a log channel on stderr, which Rust tails into its tracing with owner attribution.
- A plugin that writes raw bytes to fd 1 corrupts framing. Rust detects this through bad magic or length, kills the host, and restarts it (counted as a crash).
- I rejected fd 3: Rust's `std::process` cannot pass extra handles on Windows, and the task prefers one transport everywhere.

### 1.5 Isolation units

- **One process means one trust domain.** Plugins in the same host share a V8 isolate and can monkey-patch each other's globals (§4.4).
- **Default: one host per engine *process* per trust tier**, shared by every session, thread and sub-agent in that process. Registrations carry `RegScope::{Global, Session(id), Agent(id)}`. (Corrected from "one host per session runtime": sub-agents already share the parent's `McpPool` (R9), and a host per session would multiply ≤80 MB by the fan-out.)
  - host #0, builtin: only `builtin:mcp` (phase 3) and other first-party host code. **Computer Use is not host code** (R7). It remains an MCP stdio server process that the broker spawns;
  - host #1: reviewed third-party plugins.
  - Phase 1 runs host #1 only, because no builtin host code exists yet.
  - **Phase-3 entry gate:** `builtin:mcp` never shares a process with third-party code. The tier split has to exist before MCP moves.
- **A later option is per-plugin `isolation: "process"`**, the DSH `isolate` analogue for processes, for plugins whose declared capabilities differ sharply from the rest. The supervisor already supports N hosts, because a host is keyed by `HostId`.
- **Cost:** about +40 MB RSS per extra host (estimated, not measured).

---

## 2. Protocol

### 2.1 Transport and framing

- **Framing.** Each frame is a 4-byte magic `CWX1`, then a u32 LE length, then UTF-8 JSON.
  - `MAX_FRAME = 32 MiB`, enough for Computer Use screenshots after base64. Anything larger is refused with a typed error, never truncated.
  - A length prefix is used rather than NDJSON. Framing errors are detectable, which matters because plugins share the process with the channel (§1.4). It also follows the precedent in Codex `code-mode-protocol/src/host/codec.rs`.
- **Envelope.** JSON-RPC 2.0 (`id`, `method`, `params` / `result` / `error`). It is familiar, and in phase 3 relayed MCP frames sit inside `params` as opaque strings.
- **Limits:**
  - At most 256 requests in flight per direction.
  - Outgoing queue bounded at 128. When it is full, the host awaits; Rust never blocks the turn loop on the host.
  - Request IDs are deduplicated over a window of 4096.

  These copy the Codex code-mode-host limits.

### 2.2 Types and versioning

- **Rust serde types are the source of truth.** They go in `crates/tui/src/extension_host/protocol.rs`, not `crates/protocol`, because the host protocol is private to the engine process and the app-server has no reason to see it.
  - Host → core types use `#[serde(deny_unknown_fields, tag = "kind")]`, because host output is untrusted input.
  - Core → host types are tolerant.
- **Generated TypeScript.** `schemars` (already a `crates/tui` dependency, derived on the wire types under `cfg(test)`) reads each type's serde shape, and a Rust test (`crates/tui/src/extension_host/protocol/tests.rs`) renders `src/protocol.generated.ts` from it and from the Rust method table (`protocol::METHODS`): constants, error codes, the method table, every params shape the host validates, and the wire types. The file is committed and the test fails on drift (re-record with `CODEWHALE_CONFORMANCE_UPDATE=1`, then rebuild `dist/`); no npm generator is involved. Both sides still parse and round-trip the shared JSON fixture corpus (`crates/tui/tests/fixtures/extension_host/protocol/*.json`).
- **Handshake:**

  ```
  host → core  host/hello      {protocol: {min: 1, max: 1}, host_version, bundle_sha256, runtime: {name, version},
                                tier, builtin_modules: [{id, sha256}], (memory_limit_mib)}
  core → host  host/initialize {protocol: 1, session_runtime_id, workspace_roots, caps_granted: [...],
                                limits: {max_frame, max_inflight, hook_deadline_ms, dispose_deadline_ms}}
  host → core  host/ready      {}
  ```

  - **The bundle digest must equal the one Rust materialized.** On a mismatch the host is refused. This catches a stale or corrupted bundle. It is **not** anti-substitution: a substituted host would simply report the expected digest. The real control is that Rust chooses what to exec (§4.4, threat 1).
  - **Capability versioning.** Adding a method means adding an optional capability, so the protocol integer does not change. Changing semantics means `protocol + 1`. The core supports `[n-1, n]` for one release.

### 2.3 Message catalogue

**Core → host (requests unless noted)**

| Method | Params → Result | Notes |
|---|---|---|
| `host/initialize` | above | |
| `ext/activate` | `{owner: OwnerRef, entry: StagedEntry, config, scope}` → `{ok} \| {failed, diagnostic}` | `OwnerRef = {plugin_id, generation, owner_token}`. `StagedEntry` is the reviewed, hash-bound path. The host re-hashes it before `import()` |
| `ext/deactivate` | `{owner}` → `{disposed, leaked: [..]}` | Returns only after the fiber is quiescent (Cordis `quiesceFiber`) or the deadline passes |
| `tool/call` | `{handle, call_id, parent_call_id, input, deadline_ms, origin}` → `ToolResultWire` | Only for handles Rust registered. The call has **already passed the gate** |
| `command/run` | `{handle, command_id, raw_input, agent: AgentRef}` → `CommandResultWire` | |
| `hook/evaluate` (phase 4) | `{handle, event, payload, deadline_ms}` → `abstain \| deny{reason} \| ask{reason} \| annotate{text} \| revise{input}` | **No `allow` in the type** |
| `mcp/connect`, `mcp/disconnect`, `mcp/list`, `mcp/call`, `mcp/readResource`, `mcp/getPrompt` (phase 3) | §5 | `mcp/call` carries a `grant` |
| `proc/data`, `proc/exit` (notifications) | `{proc, stream, bytes_b64 \| code}` | Broker → host bytes for relayed stdio |
| `$/cancel` (notification) | `{id}` | Cancels any request; the host fires the invocation's `AbortSignal` |
| `host/ping`, `host/shutdown` | | |

**Host → core**

| Method | Params → Result | Notes |
|---|---|---|
| `registry/register` | `{owner, kind, spec}` → `{handle} \| {refused, reason}` | `kind` ∈ `tool, command, skillRoot, promptSection, hook, preset, mcpServer`. Rust checks the owner's reviewed capabilities, name collisions and schema caps |
| `registry/unregister` | `{owner, handle}` → `{}` | Idempotent. A stale handle is a no-op (§3.2) |
| `core/call` | `{owner, invocation_id?, name, input}` → `ToolResultWire \| refused` | Runs a tool **through `plan_tool_calls` + approval**, with `ToolCallSource::Extension` (§4.2) |
| `proc/spawn`, `proc/write`, `proc/closeStdin`, `proc/kill` | | Process broker (phase 3). A spawn needs a Rust-issued `launch_ticket`; the host cannot name arbitrary argv |
| `net/fetch` | `{ticket, method, url, headers, body_b64}` → streamed response | Fetch proxy (phase 3). Rust applies network policy and injects auth |
| `prompt/propose` | `{owner, section_id, text}` → `{accepted \| deferred \| refused}` | Rust decides inclusion and timing: new sessions only, or through a recorded transition |
| `storage/get`, `storage/set` | owner-scoped key/value in the core store | Phase 2+ |
| `ext/faulted` (notification) | `{owner, error}` | |
| `log` (notification) | `{owner?, level, msg}` | |
| `$/progress` (notification) | `{call_id, progress, total?, message?}` | Streaming progress for `tool/call` and `mcp/call`. It resets the Rust-side idle deadline, like opencode's `resetTimeoutOnProgress` |

### 2.4 Streaming and cancellation

**Streaming.**
- Long results use `$/progress` notifications. The final result is a single frame.
- Streamed `net/fetch` responses (SSE and Streamable HTTP) arrive as `net/chunk {stream, bytes_b64}` notifications, then `net/end`.
- The host applies backpressure with `net/credit {stream, n}`, so Rust never buffers without limit.

**Cancellation is bidirectional and always a notification.**
- **Rust cancels a host call** (turn interrupt, deadline, or revocation within `PendingAuthorityWatch`'s ~50 ms) by sending `$/cancel {id}`. The host aborts the `AbortSignal` passed as `exec.signal` (DSH `ToolExecution.signal`) or `invocation.signal`.
- **Grace period.** Rust waits 500 ms for a response. After that it **resolves the call as cancelled on its own side regardless**, so the turn is never held by the host. A late response is dropped and logged.
- **The host cancels its own `core/call`** with `$/cancel`, and Rust cancels the planned call the way it cancels an interrupted tool.

---

## 3. Owned registrations and coordinated async teardown

### 3.1 Ownership model (Rust side is the authority)

```rust
struct OwnerKey { plugin_id: PluginId, generation: u64 }      // generation bumps on every (re)activation and host restart
struct Registration { handle: RegHandle /* u64, never reused */, owner: OwnerKey, kind: RegKind,
                      public_name: String, scope: RegScope /* Global | Session(id) | Agent(id) */ }
struct OwnerRegistry { by_handle: HashMap<RegHandle, Registration>, by_owner: HashMap<OwnerKey, BTreeSet<RegHandle>>,
                       by_name: HashMap<(RegKind, String, RegScope), RegHandle> }
```

- **Admission.** Each `registry/register` is checked against the owner's **reviewed capability receipt** (`verify_plugin_authority`, which is re-hashed on every dispatch per #6209).
  - Phase 1: any registration needs the owner's `Native` capability to be reviewed and supported, via `verify_plugin_component_authority(…, Native)`, and only `kind = "tool"` exists. Later phases add finer capabilities only as they start enforcing them (§4.6). A `mcpServer` registration always needs `McpStdio` / `McpRemote`.
  - An unknown capability is refused with a diagnostic, and the row FAILS in the host.
- **Names.**
  - A name that collides with a native tool, an MCP tool, another owner's tool or a reserved prefix (`mcp_`, `ext_`, `execute_tools`, `tool_search`, …) is **refused**, never shadowed. omp lets an extension replace built-ins; we do not.
  - **This is enforced twice.** Once at `registry/register`, against the static native set. Again at turn build, where the `HostToolSpec` adapters are added *after* natives and `~/.codewhale/tools` scripts, and any name already in the registry is skipped with a diagnostic. The second check is needed because `ToolRegistry::register` silently overwrites a same-name tool (R1).
  - Within one owner, re-registering the same name returns a new handle and retires the old one. This matches DSH's `NamedEntries` semantics.
- **Schema caps.** Tool input schemas must be ≤ 64 KiB and description text ≤ 4 KiB. An owner may hold at most 128 tools, and a host at most 1,024. These reuse `McpCatalogBudget` numbers where they exist.

### 3.2 Exact-entry undo (from DSH `NamedEntries` / `AnonymousEntries`)

- `unregister(handle)` removes exactly one entry. If a newer registration has taken the name, the old handle no longer matches `by_name`. Removing it only deletes its own `by_handle` row and never touches the newer entry.
- In the host, each shim returns a disposer `() => void` that is idempotent and closes over its handle. Cordis runs these disposers in reverse order, either when the fiber unloads or when the plugin calls the disposer early.

### 3.3 Teardown protocol (Rust revokes first, then asks the host to drain)

When a plugin is disabled, a trust receipt changes, the plugin is uninstalled or updated, or the session ends for session scope:

1. **Rust revokes synchronously.**
   - Remove every registration of `OwnerKey` from the catalog.
   - Future dispatches fail with `not_available`. `HostToolSpec::execute` re-checks that its handle and generation are still admitted before sending `tool/call`, which covers a revocation in the middle of a turn.
   - In-flight calls get `$/cancel` and resolve as cancelled after 500 ms.
   - The next turn's registry rebuild simply leaves the tool out. The tool was deferred, so the pinned prefix does not move. (Corrected: the earlier `change:tool_surface` event does not exist (R6), and none is needed.)

   **Revocation never waits for the host.**
2. **Rust sends `ext/deactivate {owner}`.** The host runs the fiber's memoised `dispose()`. Racing callers await the same promise; this is DSH `createScope().dispose` with `disposing ??= quiesceFiber(fiber)`. Disposers run in reverse order. Async disposers are awaited, e.g. `tool-workspace-dependencies` awaits its in-flight preparation (`index.ts` ~l.243).
3. **Bounded wait.** The deadline is 2 s, or 5 s for MCP servers so that a transport close can finish.
4. **The host acks `{disposed, leaked}`.** Leak detection counts the fiber's remaining effects, open managed timers and brokered processes after quiescence.
   - A leak, or a timeout, marks the host **dirty**.
   - Rust has already removed everything from its own view, so correctness does not depend on the ack.
   - Two dirty events in 10 minutes schedule a host restart at the next idle point (no turn in flight). The restart replays the remaining activations.
5. **Rust kills brokered processes** still owned by `OwnerKey`, as a whole process group.

**Activation is all-or-nothing.** If `apply` throws, or an async `apply` rejects, or activation exceeds 5 s, the host disposes the partial fiber, which rolls back every registration made so far. It then reports `{failed}`. Rust also sweeps `by_owner` for that generation, so nothing half-registered survives on either side. **MCP tool lists get the same rule:** a refresh replaces the whole generation or keeps the previous one (DSH `mcp-client` all-or-nothing sync).

**Timers and child processes.**
- The host exposes Cordis `ctx.setTimeout` / `ctx.setInterval`. They are `unref`'d, owned by the fiber and cleared at dispose (the omp `managed-timers` lesson).
- Raw `setInterval` still works, since this is Node, but it is detected as a leak at dispose.
- Child processes are expected to go through `ctx.subprocess`, which is the broker. A direct `child_process` call is outside our control until the host is OS-sandboxed (§4.5).

---

## 4. Trust and approvals

### 4.1 The invariant, stated narrowly

**The claim:** every action the host asks **the core** to perform (a tool call, shell, file write through the core, process spawn, network fetch, prompt section, store write) goes through the same Rust gate as a model-originated action, and the host has no protocol method that expresses approval.

**What the claim does not cover:** ambient Node code (`node:fs`, `node:net`, `child_process`). The RUST memo §1.1 says the same. Until §4.5 lands, a **trusted host plugin runs with the user's privileges**. The review UI must say exactly that. This is no weaker than today's MCP stdio servers (`mcp.rs:753-756`: "not an OS sandbox") or today's `~/.codewhale/tools` scripts (R1).

**Concretely, "user privileges" includes the secret store** (R5). The default backend is `~/.codewhale/secrets/`, which also holds MCP OAuth tokens, and provider keys may sit in `config.toml`. The host process gets a scrubbed environment and never receives a credential over the protocol. But an unsandboxed plugin can `readFile` those paths. So everywhere this document says that secrets or tokens "never enter Node", it means **never through the protocol**, which rules out accidental leaks, logs and crash dumps. It does **not** mean a malicious plugin is contained. Containment arrives with the §4.5 sandbox, whose profile must explicitly deny `$CODEWHALE_HOME/{secrets,config.toml,auth*}` and keychain access.

### 4.2 Extension-originated tool calls reuse code mode's gate

**Phase 1 needs none of this section.** A phase-1 extension tool is a `ToolSpec` adapter (`HostToolSpec`) in the per-turn registry (R2). When the model calls it, the call goes through `plan_tool_calls`, then hooks, then approval, then execution, exactly as a script tool's call does. When `execute_tools` calls it, main refuses it as needs-approval and the lane suspends it for approval. The host *cannot* ask the core to do anything in phase 1: `core/call` is not in the phase-1 protocol.

The rest of this section is phase 2, when `core/call` arrives. Lane 6562 already built what it needs. `gate_nested_call` plans one call through `plan_tool_calls(…, ToolCallSource::CodeMode)`, raises `ApprovalRequired` with a `<parent>.<seq>` id, suspends the program while it waits, and records receipts. The proposal:

- **Generalize `NestedCallGate` into a `CallGate`.** The turn loop serves it for any executor that runs *inside a tool call*: `execute_tools` programs and extension tool executions.
  - While `tool/call` for extension tool `T` is in flight, the host's `core/call` requests carrying that `invocation_id` go into `T`'s `CallGate`.
  - They are planned with `ToolCallSource::Extension { owner }` and receive ids `<T.call_id>.<seq>`.
  - Approval cards say "requested by extension `<plugin>` (inside `<T>`)". **Rust composes the card text.** The host cannot supply approval text.
  - **Attribution spoofing.** In a shared host, plugin B can read A's in-flight `invocation_id` and send `core/call` under it. The card would then blame A. Rust rejects a `core/call` whose `owner_token` differs from the invocation's owner, which catches bugs. Because the token is readable inside the process (§4.4, threat 3), the card for a multi-plugin host also shows "host with N plugins". **The gate itself still holds:** B's request still needs the user's approval, and a spoofed id buys a misleading label, not an approval.
- **Out-of-turn actions have no gate.** Command handlers, event listeners, timers and activation code get `ungated_admission` semantics, the same as code mode without a turn: only read-only, `ApprovalRequirement::Auto` tools pass `enforce_tool_authority`, and everything else is refused with "no session permission gate; return a proposal instead".
  - Commands return `{kind: 'submit', prompt}`. Rust submits that as a visible user turn, so anything the model then does is gated normally.
- **`ToolCallSource` hygiene.** Merge or rename before adding the third variant. `crates/tools` `ToolCallSource { Direct, JsRepl }` and the lane's private `turn_loop.rs` `ToolCallSource { Model, CodeMode }` are unrelated enums with the same name. Rename the tools-crate one to `ToolInvocationChannel`, then add `Extension { owner: OwnerKey }` to the engine one.

### 4.3 What an extension can and cannot express

| DSH surface | Codewhale host | Why |
|---|---|---|
| `ctx.approval.request`, `approval/request` answerers | **Refused.** The row FAILS with a diagnostic | No self-approval |
| `tools/pre-execute` returning `{kind: 'allow'}` | **Not expressible.** The shim maps allow to `abstain` and logs it once per owner | Matches DSH's own `hooks-claude-code` rule (a `PreToolUse` `allow` does not pre-approve) and Codewhale's `ToolCallDecision::Allow` no-op (`turn_loop.rs:6364`) |
| `tools/pre-execute` deny / ask / revise | `hook/evaluate` verdict, folded in Rust as `deny > ask > abstain` | A `revise{input}` is **re-planned through the gate**, as `updated_input` is today (`turn_loop.rs:3238`). A hook error or timeout counts as **deny** (fail closed, as in omp `wrapper.ts:240`) |
| `tools/execute` around-wrappers | **Refused** in v1 | They could rewrite results or arguments after the gate |
| Providing a core service name (`agents`, `sessions`, `approval`, `llm`, `sandboxPolicy`, `credentials`, `fs`, `subprocess`, `systemPrompt`, `tools`, `commands`, `skills`) | **Refused** at `ctx.reflect.provide` time. The row FAILS | One authority per concern |
| `presentCall` / annotations such as `kind: 'read'` | **Display only; never changes approval.** Extension tools are always `ApprovalRequirement::Required`, and the user can remember the approval per tool (bound to the plugin's receipt hash, so an update re-asks). They are never read-only for plan mode | (Corrected, R4.) Under `approval_hint_for`, a read-only hint becomes `Auto`, which would let a plugin turn off approval for its own tool, and that tool body runs arbitrary Node. That is self-approval. MCP servers keep today's rule unchanged; extension tools do not inherit it |

### 4.4 Anti-spoofing

The threats, from the outside in:

1. **Host substitution.** A different JS file is launched as the host. Rust launches only the digest-named bundle it materialized, and re-checks the digest on an open descriptor before exec (the `ReviewedStdioLaunch` pattern). **Scope:** on macOS, `ReviewedStdioLaunch` itself hash-checks a Node entry and then *reopens it by path* (`mcp.rs:937-939`). A same-user process could still swap the file in that window, and the bundle lives in the user's own home directory. This control defeats corruption and stale builds. It does not stop an attacker who already runs as the user, who would own the machine anyway. `host/hello.bundle_sha256` is self-reported and serves only as a consistency check.
2. **Plugin substitution after review.** `ext/activate.entry` is the staged, hash-bound copy from `plugins/install/stage.rs`. The host re-hashes the **whole package closure** before `import()`: every file under the package root, including any `node_modules` shipped inside the tarball. A mismatch fails activation and Rust marks the receipt `ContentChanged`.
3. **Stale or cross-owner calls (confused deputy).**
   - Every host→core frame carries `owner_token`. This is 128 random bits minted by Rust per `(plugin_id, generation)` and handed over only in `ext/activate`.
   - Rust rejects a frame whose token does not match its handle's owner or whose generation is stale.
   - This stops bugs and stale fibers. **It does not stop a malicious plugin inside the same process**, which can monkey-patch the shared transport and read other owners' tokens.
   - The honest statement: **the effective authority of a host process is the union of its plugins' reviewed capabilities.** That is why hosts are split by trust tier (§1.5), and why the review UI for a plugin in the shared third-party host says "runs alongside N other plugins".
4. **Impersonation toward the user or the model.**
   - Extension tool names cannot collide with or shadow existing names (§3.1).
   - `tool_search` output and approval cards always show the origin, `extension:<plugin>`.
   - Extension prompt sections are wrapped in an attributed, bounded envelope (`prompt/propose`), the same treatment MCP server instructions get. The host cannot write raw system-prompt text.
5. **Bypassing the gate to reach MCP servers directly** (phase 3). A plugin could grab the host's SDK `Client` objects and call `tools/call` without Rust.
   - Stdio bytes flow through the Rust broker, and HTTP flows through the Rust fetch proxy. So Rust **parses the top-level `method` of every outbound JSON-RPC message** and applies a **default-deny** method policy (corrected from a three-method denylist, R12):
     - **Ungated allowlist:** `initialize`, `notifications/initialized`, `ping`, `notifications/cancelled`, `notifications/progress`, `*/list` (with pagination), `logging/setLevel`, and responses to server-initiated requests. Responses are admitted only if Rust forwarded the matching server request.
     - **Needs a grant:** `tools/call`, `resources/read`, `prompts/get`.
     - **Everything else is dropped**, including `resources/subscribe`, `completion/complete`, `tasks/*` and any method added in a future spec revision, until someone classifies it on purpose.
   - A grant is single-use, minted by the gate when it plans the call, and bound to `(server, generation, params.name or uri, call_id)`. It also requires `params.arguments` to **equal the planned input as a parsed JSON value** (`serde_json::Value` equality). The SDK re-serializes arguments, so a hash over bytes would reject honest calls.
   - A request with no grant is dropped: the broker answers the server-side request with a JSON-RPC error. The event is logged as `extension_host.gate_bypass_attempt`.
   - **What this buys** (corrected, R5): a host bug, or a plugin that reaches the host's SDK `Client` objects, cannot call an MCP tool without the gate, *over the brokered channel*. It does **not** survive a malicious unsandboxed plugin, which can read the on-disk OAuth token and open its own connection, or `spawn` the stdio server itself. Until §4.5, "one gate" holds for every path Codewhale provides, not against same-user code. That is the same boundary as today's MCP stdio servers.

### 4.5 Sandboxing the host (phase 5, required before third-party host plugins leave experimental)

- Launch the host under `crates/tui/src/sandbox/` (seatbelt, bwrap or seccomp), with a profile that:
  - denies network except the loopback control socket;
  - denies writes outside `$CODEWHALE_HOME/extension-host/data/<plugin>/` and the OS temp dir;
  - allows reading the workspace and plugin roots;
  - **explicitly denies reading `$CODEWHALE_HOME/secrets/`, `config.toml`, auth and session stores, and keychain services**, even where the workspace allow rule would cover them (R5);
  - denies `exec` except `node` itself.
- Once MCP egress goes through `net/fetch` and stdio spawns go through `proc/spawn`, legitimate plugin paths need no ambient network or exec. The sandbox then turns §4.1's narrow claim into the broad one.
- DSH plugins that use `node:child_process` or `node:net` directly fail under the sandbox and are reported "needs unsandboxed host". That is decision D5.

### 4.6 Plugin trust and review

- **Phase 1 activates the existing `Native` capability; it does not add a new one** (corrected, R8). `native` (alias `native_extension`) is already a hashed, inventoried and *inactive* "executable extension" component, in both `plugin.toml` and `extensions."net.codewhale"`. Under the flag it moves from `inactive` to `supported`, and each `native` path is one host entry module (an ESM Cordis plugin).
  - **Why not a new `host` key?** `CodewhalePluginExtension` is `deny_unknown_fields`, so every older build would reject the whole plugin. With `native`, an older build loads the plugin's skills and MCP servers and reports native as inactive.
  - **Why not new capabilities?** A second executable concept would need retiring later.
  - **Display:** `native` is shown as "host code (JavaScript)".
- **Finer capabilities come later, and each is a policy bump.** Candidates are `HostCommands`, `HostHooks`, `HostNetwork` and `HostSubprocess`, the last two once the sandbox exists. Every policy change re-reviews every plugin, because the whole policy is hashed into each receipt (`activation.rs:107-122`). So add a capability only in the phase that first enforces it.
- **To avoid invalidating every user's receipts for an experiment**, the policy is selected by the flag:
  - With `extension_host` off, it returns v3 byte-for-byte. Existing receipts, including Computer Use's, stay valid.
  - With it on, it returns v4 (`Native` supported). Receipts fail closed as `CapabilitiesChanged`, which triggers re-review; that is intended. **Toggling the flag in either direction re-reviews everything.** Say so in the flag's description.
  - **Mechanics:** `current()` is a `const fn` called at 6 production sites with no `Features` in reach (`types.rs:239`, `registry.rs:2218`, `manifest.rs:268,284,292,1676`). It becomes a read of a process-wide `OnceLock<PluginActivationPolicy>`, set once at boot from config. A config reload never flips it mid-process, and it defaults to v3 when unset, which also covers tests.
  - When the flag graduates, v4 becomes the only policy and everyone re-reviews once. Put that in the release notes.
- **Review screen for a host plugin:**
  - package name and version, the full-closure content hash and file count;
  - declared capabilities (the `native` entries in `plugin.json`, or, from phase 5, inferred for DSH from `inject` plus the patch rows by the static Rust reviewer, §6.3);
  - whether it has install scripts (always refused in v1);
  - "Runs JavaScript on your computer with your user permissions".

  The last line is removed only when the sandbox is on.
- **Installation stays in Rust:** `plugins/install/{stage, place, tarball}` with exact-hash review. In v1, installs are from a path or a tarball only: **no npm registry fetch and no install scripts**. Dependencies must be bundled in the tarball or be host-provided peers (§6.2). A pnpm-based install with approval of build scripts is a Tier C item.

---

## 5. MCP on the host

### 5.1 Recommended split (founder's option B, hardened)

| Concern | Owner | Mechanism |
|---|---|---|
| Which servers exist, merge and scope (global, project, plugin), project-config trust, `/mcp add`, `required` / `enabled` / `enabled_tools` / `disabled_tools`, `execute_timeout` | **Rust** (unchanged: `mcp.rs` config half, 1–1300 and 5245–6468) | Sent to the host as `mcp/connect {server, generation, transport: {stdio: {launch_ticket}} \| {http: {fetch_ticket, url}}, timeouts}` |
| stdio **spawn / kill** | **Rust ProcessBroker** | Merges `ReviewedStdioLaunch` (fd-bound exec of the hashed entry), the `mcp/stdio.rs` spawn, `child_env` scrubbing plus credential injection, and hook `HookProcessTree` / `WindowsHookJob`. Bytes move over `proc/*`. Each whole process group is killed on disconnect |
| HTTP / SSE / Streamable HTTP egress | **Rust FetchProxy** | The SDK transports are given `fetch: proxiedFetch`. Rust applies `NetworkPolicyDecider`, `reviewed_redirect_matches_origin`, `configured_mcp_proxy` and **injects the OAuth bearer**. Tokens never cross the protocol into Node. They are still readable on disk by unsandboxed code (§4.1) |
| OAuth sign-in, token store, needs-auth state, synthetic `mcp_<s>_authenticate` | **Rust** (`mcp/oauth.rs` unchanged) | A 401 seen by FetchProxy sets needs-auth, which yields the existing `mcp_catalog_changed` metadata contract |
| JSON-RPC session: initialize and version negotiation (2026-07-28 with legacy fallback), pagination, `list_changed`, reconnect with backoff, stale-session retry, resources, prompts, progress, cancellation | **Host** (`builtin:mcp` fiber per server, official SDK `@modelcontextprotocol/client` 2.0.0) | One SDK `Client` per server generation. The code is forked or adapted from DSH `mcp-client` (MIT, ~1,209 lines, effect-scoped teardown, all-or-nothing tool generations) |
| Catalog admission | **Rust** | Host-reported lists are **untrusted**. They pass `McpCatalogBudget` page, item and byte caps plus name and schema validation |
| Model-facing names | **Rust** | `mcp_<server>_<tool>` is unchanged; it lives in session history and the cached prefix. The host reports `(server, raw_name, annotations)` |
| Approval hints, disallowed tools, ask rules | **Rust** | `McpToolApprovalHint` and `authorize_call` are unchanged. Annotations are trusted only for reviewed plugin servers |
| Status for TUI, GPUI and runtime API | **Rust** | A `HostMcpClient` produces the existing `McpManagerSnapshot`, `McpRecoveryKind` and `needs_auth_generation`, so `tui/views/extensions.rs`, `runtime_api.rs:1429-1458` and the GPUI app do not change |
| Server-initiated sampling and elicitation | **Rust** | The host forwards `mcp/serverRequest` to Rust, whose UI answers or refuses. Default: refuse, the same as today. **Never auto-resolved in the host** |

**Why not the VS Code split (option A, Rust keeps the JSON-RPC client)?** It is lower risk, but it keeps the hand-rolled Rust session code (~3,100 lines) that the founder wants gone, and it forgoes the SDK's protocol tracking (SHA-6628). Option B with brokered transports keeps every *authority* concern in Rust and moves only the protocol state machine.

**The cost of option B is IPC hops.** A stdio tool call crosses the channel four times: `mcp/call`, then `proc/write`, then `proc/data`, then the `mcp/call` result. Today it crosses twice. JSON results are also serialized twice.
- For Computer Use screenshots (hundreds of KB to MBs of base64) this is the risk to measure. Phase 3's gate: p95 added latency ≤ 5 ms for a 1 MB result on the CU `screenshot` tool.
- **Fallback if the gate fails:** a `direct_stdio` launch mode for **builtin, reviewed** servers only. The host spawns the process itself from a Rust-issued ticket that holds the exact argv and scrubbed env. That places secrets in host memory, so it is allowed only in the builtin host #0.
- **Computer Use in particular.** Its `screenshot` results are the largest payloads. Today they are Rust↔CU. In phase 3 they would be CU → broker → host → Rust, parsed twice. Code mode drops image blocks from nested results anyway (lane `codemode.rs` "Known limitations"), so the extra cost falls only on direct calls. Measure it before choosing.

### 5.2 Code mode reaches MCP through the one gate: the seam for lane 6562

**Scope (corrected, R2/R3).** This seam is for **MCP only**. Extension tools do not need it: they are registry `ToolSpec`s, which the lane already gates as "plugin" nested calls. The lane is unmerged and in flight, so this is an **offer** to that lane, not a phase-1 dependency:
- If the lane adopts it before merging, `codemode.rs` does not change again in phase 3.
- If it does not, phase 3 makes exactly the edits listed under "Lane changes" below. That is about 5 call sites in `codemode.rs` plus `tool_execution.rs:232`.

It merges the DSH and RUST memo proposals.

```rust
/// Tools served outside the native registry: MCP servers (Rust pool today, host in phase 3).
/// Extension tools are NOT served here; they are registry ToolSpecs (HostToolSpec).
/// Planning (plan_tool_calls → hooks fold → approval) happens BEFORE `call`; implementors never gate.
#[async_trait]
pub(crate) trait ExternalToolDispatch: Send + Sync {
    /// Catalog lookup by model-facing name. None = not an external tool.
    fn describe(&self, model_name: &str) -> Option<ExternalToolDescriptor>;
    async fn call(&self, model_name: &str, input: serde_json::Value, cx: ExternalCallCx)
        -> Result<RichToolResult, ToolError>;
}

pub(crate) struct ExternalToolDescriptor {
    pub origin: ExternalOrigin,        // Mcp { server, plugin: Option<PluginId> }
    pub generation: u64,               // catalog generation; call_tool rejects stale
    pub starts_sign_in: bool,          // replaces the `_authenticate` suffix test
    pub read_only_hint: bool, pub destructive_hint: bool,  // already filtered by approval_hint_for
}

pub(crate) struct ExternalCallCx {
    pub call_id: String, pub parent_call_id: Option<String>,
    pub source: ToolCallSource,        // Model | CodeMode | Extension{owner}
    pub tx_event: mpsc::Sender<Event>,
    pub disallowed_tools: Arc<[String]>,
    pub cancel: CancellationToken,     // turn interrupt + PendingAuthorityWatch revocation
    pub deadline: Option<Instant>,
    pub grant: GateGrant,              // minted by the gate; opaque to McpPool today, enforced by the broker in phase 3
}
```

**Lane changes (small; the lane's to take or leave, otherwise phase 3's):**
- `NestedCallGate.mcp_pool: Option<Arc<AsyncMutex<McpPool>>>` becomes `external: Option<Arc<dyn ExternalToolDispatch>>`.
- `CodemodeInvoker::execute` becomes `if let Some(d) = external.describe(name) { external.call(..) }`.
- `refusal_before_gate` becomes `describe(name).is_some_and(|d| d.starts_sign_in)`.
- `ungated_admission` becomes `describe(name).is_some()`, which yields "external tool; needs a session gate".
- The direct path's `execute_mcp_tool_with_pool` call sites (`tool_execution.rs:232` and its callers) go through the same trait, so direct and nested calls share one dispatcher.
- `McpPool::is_mcp_tool` (about 30 static-prefix call sites) becomes `external_tool_kind(name)` where the question is "is this external?", and stays name-based where it really is about the `mcp_` naming scheme.

**Implementations:**
1. `McpPoolDispatch`, whenever the lane or phase 3 wants it: a thin wrapper that locks the pool and calls `authorize_call` then `call_tool`.
2. `HostMcpDispatch`, phase 3: `mcp/call` with the grant.

There is no `CompositeDispatch` and no `HostExtensionDispatch` (removed). Only one implementation is live at a time, and it is swapped when the pool is deleted. The parity harness (§9.3) is the one place both exist.

### 5.3 Migrating pool, deferral, always_load and auth

| Today (`mcp.rs`) | After phase 3 |
|---|---|
| `McpPool::new(config)` + `hash_mcp_config` | `HostMcpClient::new(config)`. The same hash keys the Rust catalog cache |
| Lazy boot pass, `tool_selection_covers_server`, `tools_always_load`, per-turn explicit-connect wait (#6033) | **Unchanged Rust logic.** It now decides *when to send* `mcp/connect` rather than when to connect a Rust `McpConnection` |
| `required: true` servers block boot / report failure | Unchanged semantics. A required server forces host spawn at boot, in parallel with provider warmup |
| Deferred MCP tools via `tool_search` / `defer_loading` | Unchanged. The tool surface policy is Rust's |
| `McpConnection` / transport trait / `stdio.rs`, `sse.rs`, `streamable_http.rs`, `http.rs`, `wire.rs` | **Deleted.** The SDK plus brokered transports replace them |
| Reconnect / backoff (`mcp.rs:~2845`), stale-session retry | Host, per server fiber. Rust observes state via `mcp/state` notifications |
| OAuth (`mcp/oauth.rs`), needs-auth transition, `_authenticate` tool | Unchanged Rust. The trigger moves from the transport to FetchProxy's 401 |
| MCP prompts and resources | Host SDK, exposed through the existing Rust tools (`mcp_read_resource`, …) |
| `crates/mcp` `McpManager` / `ChildProcessMcpClient` | **Deleted independently in phase 0** (dead in production per the RUST memo §1.3) |
| `mcp_server.rs` (Codewhale *as* an MCP server) | Unchanged. It is a Rust server over the core gate |
| `runtime_mcp.rs`, `mcp_registry.rs` (model-started servers) | Unchanged authority (`ApprovalRequirement::Required`). They now end in `mcp/connect` |

### 5.4 Startup: advertise from cache, connect on demand

- **A persistent catalog cache in Rust.** Keyed by `(server, config_hash, server_version)`, it lives in the core store with a 30-day TTL. This follows VS Code's nonce cache, omp's SHA-256 tool cache and Codex's `LazyWhenCached`.
- **Advertising.** A server with a cached catalog is advertised without starting the host or the server. The first call starts both.
- **Uncached eager servers.** A server that must start eagerly and has no cache races a 250 ms window (omp's `STARTUP_TIMEOUT_MS`). Slower servers join through the deferred catalog.
- **When the host is late or has died,** the cache keeps the tool array, and therefore the KV-cache prefix, stable.

---

## 6. DSH compatibility level

### 6.1 Matrix

| DSH artefact | Status | Mechanism |
|---|---|---|
| `apply(ctx, config)` plugins and `Service` subclasses using `tools`, `commands`, `logger`, `timer`, `schemastery` `Config` | **Runs natively** (`tools`, `logger`, `timer`, `Config` in phase 1; `commands` in phase 2) | Real Cordis fibers under the host root. Shims provide `tools` (phase 1) and `commands` (phase 2). In phase 1, a plugin that injects `commands` FAILS activation with "requires `commands`, not yet provided" |
| `ctx.effect`, `ctx.on` teardown, async `apply`, `internal/plugin` unload during startup | **Native** | Real Cordis semantics, with the DSH-hardened fiber (vendor mod #6) |
| `skills` (roots or providers) | **Native** (phase 2) | `register.skillRoot`. Rust discovers and audits. Skills stay Markdown plus files |
| `systemPrompt` sections | **Shimmed** (phase 2) | `prompt/propose`. Rust decides inclusion and timing |
| `mcpResources` | **Shimmed** (phase 3) | Onto the host MCP service |
| `dsh-mcp-client` rows | **Native config, Codewhale runtime** (phase 3) | Rows are translated into Rust MCP server configs (Rust stays the authority for which servers exist) and run by `builtin:mcp`. The DSH `mcp-client` *code* does not run as-is, because it spawns `StdioClientTransport` itself (`transport.ts:34`), which would bypass the broker. Names map `mcp__s__t` ↔ `mcp_s_t` for imported permission rules |
| `dsh-skill-filesystem` rows | **Native config** | `register.skillRoot` |
| Bundle composition: `dsh.bundle.patch` (string or list), `insert`, override-by-id, groups, disabled ancestry | **Native** (phase 2 literal rows, phase 5 full) | DSH's own `applyEntryPatches` (`vendor/include`), so it cannot drift. **Phase 1 has no patch recognition** (R11). A DSH plugin is loaded by a Codewhale bundle whose `native` entry is a Cordis plugin calling `ctx.plugin(dshPlugin, literalConfig)`. That is the same semantics as a single literal patch row, with no new parser |
| `!!js` in config and `disabled` | **Native for trusted, enabled plugins** (phase 5) | Evaluated in the host against the row's ctx, as in DSH. For credential-shaped expressions (`process.env.X` in MCP `env` or `headers`), the static reviewer rewrites them to a Rust `CredentialRef(X)` so the secret never passes through Node |
| `inject`, `intercept`, `isolate` | **Native** within the host root. `isolate` gives a private realm per service | |
| `tools/pre-execute`, `tools/post-execute`, `agent/pre-step`, `agent/turn-stopping` listeners | **Shimmed, monotonic** (phase 4) | `hook/evaluate`. No allow. Revisions are re-gated |
| `hooks-claude-code` / `hooks-codex` rows | **Mapped** (phase 4) | Onto the one hook runtime, which in phase 4 is the host's shell-hook executor, folded in Rust |
| `agents`, `sessions`, `sessionProjections` | **Read-only projections** (phase 5) | Snapshots plus subscriptions to Rust core events |
| `fs`, `subprocess`, `sandboxPolicy` consumers | **Gated shim** (phase 5) | `core/call` or `proc/spawn` through the gate or the broker |
| Agent-preset rows (`dsh-agent-preset`) | **Translated** (phase 5) | Codewhale agent profiles at the roster's Plugin layer, capped to reviewed capabilities. The preset's `plugins` become per-agent host scopes with pinned revisions |
| `llm` consumers | **Skipped** in v1 | Provider routing and billing are core. Revisit with a budgeted `core/llm` |
| `approval` answerers, `allow` from pre-execute, `tools/execute` wrappers | **Refused by design** | §4.3 |
| Rows providing `agent-loop`, `session-*`, `system-prompt`, persistence, `tools` / `commands` services | **Refused by design** | One loop, one store, one prompt |
| `dsh.client` web modules, `slots` / `locale` / `ui*` / `remote` | **Skipped and reported** | The GPUI app is the product client |
| pnpm install with build-script approval | **Tier C** | v1 accepts path and tarball only, with no scripts |
| Rebuilding a retired preset revision after restart | Out (DSH does not do it either) | |

**Sizing** (from the DSH memo's approximate grep): about 13–21 of the ~90 parsed `inject`-declaring packages are headless and within reach by phase 5. About 69 need DSH UI or internal services and stay skipped. This is a planning estimate. Accepted Native source now reviews closed compositions and exact preset entries; unsupported service or prompt-replacement rows remain visibly broken. Complete stock-bundle acceptance must be established from actual installed Engine fixtures before claiming that every config bundle runs. UI and loop-replacing plugins remain outside the shared Engine boundary.

### 6.2 Module resolution: what "natively" requires

DSH packages declare `@deepseek-ai/cordis` and `@deepseek-ai/dsh-tools` as **peers**. `@deepseek-ai/schemastery` is sometimes a plain **dependency**: it is one in the phase-1 plugin (R11). There must be **one** Cordis instance and one schemastery instance, or `instanceof`, symbols and services break. So the resolve hook maps these specifiers **however the package declares them**, and ignores a bundled copy under the package's own `node_modules`. So:

- The host installs Node **synchronous module hooks** (`module.registerHooks`, available on Node ≥ 22.15 and within the 22.19 floor). They resolve these specifiers to host-provided singletons:
  - `@deepseek-ai/cordis`, `@deepseek-ai/schemastery`, `cosmokit` resolve to the bundled real packages.
  - `@deepseek-ai/dsh-tools` resolves to a **compat module**. It exports `defineTool` plus the error classes and constants that DSH plugins use at runtime. Its types match DSH `0.1.7-alpha.2`. **It does not export `ToolRuntime` as a provider.**
  - Any other `@deepseek-ai/dsh-*` peer fails the row with "requires `<pkg>`, which the Codewhale host does not provide". The failure is never silent.
- **Measure in phase 1:** whether bundling the real `@deepseek-ai/dsh-tools` helper subset is cheaper than maintaining the compat module. It has 109 import sites across DSH, so the compat surface should be generated from actual usage.

### 6.3 What `install/dsh.rs` becomes

It becomes the **static reviewer**: parse `package.json`, compose the patch rows without executing anything, infer declared capabilities from `inject` and rows, hash the whole closure, and produce the review screen. It no longer *converts* anything.

Its module doc ("arbitrary DSH TypeScript execution is outside this compatibility scope") and `DSH-PLUGIN-ADOPTION-20260922.md` §"Compatibility boundary" **must be updated in the phase that lands native loading.** They currently disagree with the founder's direction, and under AGENTS.md that disagreement is itself a defect.

---

## 7. What Rust gets deleted, in order

This follows "migrate the last consumer or do not start". Every phase's exit criterion includes its deletion, and a phase does not count as done while both paths exist outside the feature flag. The estimates are the RUST memo's reading-based numbers, not measured diffs.

| Order | Deleted | ≈ prod lines | Consumers that must migrate first |
|---|---|---|---|
| **0** | `crates/mcp` client pool, `InMemoryMcpClient`, `ChildProcessMcpClient` and legacy CLI aggregation proxy | Removed in the 0.10.1 completion source | The earlier "no production caller" premise was false: the CLI proxy spawned registered child clients. Under the recorded founder D5 decision, that proxy is removed and `mcp-server` delegates to existing native `serve --mcp`. The unused Core/App-server pool and `/mcp/startup` route/docs were removed together. The small shared bounded instruction sanitizer remains; saved legacy definitions are preserved without a new reader/writer. Hosted and package acceptance are separate pending receipts. |
| **1** (no deletion) | Nothing. Phase 1 adds the host behind the flag and deletes nothing, because no consumer has moved yet. The `ExternalToolDispatch` seam is the lane's option or phase 3's work (§5.2) | 0 | — |
| **3** (MCP move) | `McpConnection`, transport trait, discovery, pool connect / supervise / backoff / reconnect / stale retry / route (`mcp.rs` ~1466–2660 and most of 2709–5240); `mcp/{sse, streamable_http, http, http_client, wire, headers}.rs`; stdio framing (spawn moves to the broker); about half of `mcp/tests.rs` | ~4,700 | `core/engine.rs`, `turn_loop.rs`, `tool_execution.rs`, `tool_preparation.rs`, `dispatch.rs`, `runtime_api.rs`, `hooks/executor.rs`, `tools/subagent/mod.rs`, `tools/runtime_mcp.rs`, `tools/registry.rs`, `codemode.rs`, `lib.rs`, `tui/views/extensions.rs`, `tui/command_palette.rs`, `tui/setup/tools_mcp.rs`. Current adoption supersedes this estimate: callers retain `McpPool`/`McpConnection` as the sole Rust authority and use the selected SDK transport. Only native protocol/client orchestration is deleted after the one-release default window; shared ProcessBroker, guarded HTTP/OAuth, framing bounds and catalogue/session policy remain |
| **0** | Legacy `run_stdio_server` aggregating proxy | Removed with its last CLI consumer | Founder D5 drops the proxy. The CLI spelling remains an alias of the native server and introduces no SDK proxy or second client pool. |
| **4** (hooks) | `hooks/executor.rs` orchestration: matching, env building, sync and background runs, observers, message-submit transform; part of `hooks/config.rs` validation | ~2,450 | Turn-loop fire points (kept), `tui/ui/observer_hooks.rs`, `exec_agent`. **Kept in Rust:** the verdict fold, `authority.rs` project-hook receipts, output sanitizers, and the process tree (moved into the broker) |
| **4** (script tools, new row, R1) | `tools/plugin.rs` (`ScriptPluginTool`, `CommandPluginTool`, frontmatter parser; 893 lines incl. tests), `ToolRegistry::load_plugins`, the non-`Disabled` arms of `apply_overrides`, `configure_plugin_tools` (`core/engine.rs:7360-7400`) | ~700 | Users' `~/.codewhale/tools/*` scripts and `[tools.overrides]` `Script` / `Command` entries. They become one `builtin:script-tools` host plugin: each script is registered as an extension tool, spawned through the broker (the same executor as shell hooks, which is why this lands with phase 4). **Two user-visible changes are decision D9:** `# approval: auto` is no longer honoured (Required, rememberable), and a script can no longer replace a built-in. `[tools.overrides] X = "disabled"` stays in Rust: it is configuration, not extensibility |
| **5** (DSH native) | The conversion half of `install/dsh.rs` and `commands/groups/plugins/dsh_import.rs`, `dsh_tests.rs`; the DSH dialects of `scripts/convert-plugin.py` (the OpenCode dialect stays) | ~2,010 + ~200 py | `/plugin import dsh` and `POST /v1/apps/plugins/import/dsh/preview` become install-and-review of a host package |
| — | (Removed row.) `Native` is no longer retired. Phase 1 makes it the host capability (§4.6, R8) | 0 | — |
| optional | `integrations/dsh/*` external DSH launcher | ~2,850 | **Founder call** once DSH plugins run natively |

**Committed net: about −9,850 prod lines removed (including the script-tools row), +2,800 Rust added**
- The +2,800: supervisor and client (~600), protocol (~500), OwnerRegistry (~500), tool adapter and MCP dispatch (~300), broker glue (~300), FetchProxy (~400), catalog admission and cache (~200).
- That leaves **about −7,050 net**, plus about 5–8k lines of TypeScript. These remain reading-based estimates, not measured diffs.

**Stop rule.** The accepted anchor is the Phase 1 exit gate, not the September 26 host merge. If Phase 3 has not landed within six weeks of that gate, delete the host rather than retain another extension runtime behind a flag. Phase 1 exit is not yet achieved, so that clock has not started. The October 1 plan proposes a later anchor; no founder adoption of that change is recorded. `CURRENT_DECISIONS.md` §26 is the accepted decision.

---

## 8. Phase plan

### Phase 0: independent cleanups (can run in parallel with phase 1)

- Delete the dead `crates/mcp` client stack (§7, row 0), **together with its live readers** (`/mcp/startup`, the `crates/core` mapping and `docs/RUNTIME_API.md`).
- Re-scope **SHA-6628** (rmcp MCP 2026-07-28 client in Rust). It plans the Rust client this design removes. Its spec-convergence acceptance moves to phase 3, and the rmcp implementation is dropped. Done in Linear, not here.
- *Offer* the `ExternalToolDispatch` seam to lane 6562 (§5.2). This is optional, and phase 1 does not wait for it.

### Phase 1: host + protocol + extension tools + one real DSH plugin, behind a flag

**One PR, tools only** (cut from the earlier draft, R13). Commands, heartbeat, auto-restart, patch recognition and `core/call` all move to phase 2. What remains is the smallest slice that proves every load-bearing claim:
- a TS host starts lazily;
- a real DSH plugin runs unmodified;
- its tool goes through the one Rust gate;
- teardown is coordinated;
- flag-off is byte-identical.

**Flag.** `Feature::ExtensionHost`, `Stage::Experimental`, `default_enabled: false`. Config key `[features] extension_host = true`. With the flag off, the host is never spawned, the activation policy is v3 byte-for-byte, and no registry, catalog or turn-loop path changes.

**Why tools, not hooks or commands, first?**
- **Tools** land as `ToolSpec` adapters in the *existing* per-turn registry, beside `~/.codewhale/tools` scripts (R2). Every existing gate applies to them unchanged: plan mode, authority envelope, deferral, hooks, approval, and code mode. Nothing new is built on the core side of the gate.
- **Hooks** would give Codewhale two hook executors (Rust shell, host JS) until the whole executor moves. So hooks wait for phase 4, where they move in one piece.
- **Commands** need a new executable entry kind in `UserCommandRegistry` plus the `submit` round trip. Useful, but not load-bearing for any claim, so they are phase 2.

**Exact phase-1 scope: Rust** (`crates/tui/src/extension_host/`, ~900 lines estimated):

| File | Contents |
|---|---|
| `protocol.rs` | Frame codec (`CWX1` + u32 LE + JSON, 32 MiB max, typed refusal over that). Host→core types use `deny_unknown_fields`. Method subset: `host/{hello, initialize, ready, shutdown}`, `ext/{activate, deactivate, faulted}`, `registry/{register, unregister}` with `kind = "tool"` only, `tool/call`, `$/cancel`, `log`. Nothing else is accepted. |
| `supervisor.rs` | Lazy spawn when an enabled, reviewed plugin has a `native` entry and the flag is on. `resolve_node_at_least(22,19)` candidate ladder (§1.4) with the `[extension_host] node` override. Digest-named materialisation reusing `builtin.rs` helpers. Scrubbed env via `child_env`. Process group / job object: move `HookProcessTree` / `WindowsHookJob` out of `hooks/executor.rs` into a shared module; it is a move, not a copy. 2 s handshake. Bounded shutdown: 2 s, then SIGTERM, then SIGKILL at 3 s. **On exit:** revoke everything, fail in-flight calls with a typed error, mark the host failed with its stderr tail. **No auto-restart.** One host per engine process. |
| `registry.rs` | `OwnerRegistry` (§3.1–3.3): owner tokens, exact-handle undo, synchronous revocation, name refusal against the native set and reserved prefixes, schema caps (64 KiB schema, 4 KiB description, 128 tools per owner). |
| `tool.rs` | `HostToolSpec: ToolSpec`: always `ApprovalRequirement::Required`; `capabilities = [ExecutesCode, RequiresApproval]` (as script tools); not read-only; `registration_origin = "extension:<plugin>"`; deferred by default (not in `DEFAULT_ACTIVE_NATIVE_TOOLS`). `execute` re-checks liveness, sends `tool/call`, and on drop sends `$/cancel`. The result is resolved as cancelled 500 ms after cancel whatever the host does. |
| `core/engine.rs` | About 10 lines at `configure_plugin_tools`: after scripts and overrides, add the admitted `HostToolSpec`s, skipping any name already present (with a diagnostic). |
| `core/engine/dispatch.rs` | Approval-card description: "Extension tool `<name>` from plugin `<plugin>` (runs JavaScript with your permissions)". |
| `plugins/activation.rs` | Policy from a boot-time `OnceLock`. The v4 policy moves `Native` to `supported`, only with the flag on. |
| `plugins/*` display strings | `native` is shown as "host code (JavaScript)"; the review screen shows the closure hash and file count. No new manifest key (R8). |
| `features.rs`, `config.example.toml` | The flag, and the `[extension_host] node` key. |
| `dependencies.rs` | `resolve_node_at_least`. `resolve_node()` is left untouched. |

**Exact phase-1 scope: TypeScript** (`crates/tui/extension-host/`, ~600 lines estimated plus vendored deps):

| File | Contents |
|---|---|
| `src/main.ts` | Framing; console and `process.stdout.write` rebinding to stderr before any plugin loads; stdin-EOF exit; `process.exit` guard; per-owner fault attribution via `AsyncLocalStorage`. |
| `src/root.ts` | Cordis root; refusal list for core service names (`approval`, `agents`, `sessions`, `llm`, `sandboxPolicy`, `credentials`, `fs`, `subprocess`, `systemPrompt`, `tools`, `commands`, `skills`); a `tools` shim (`register` → `registry/register`, `tool/call` → `execute` with an `AbortSignal`, `output.render` applied host-side); a `logger` shim. A plugin injecting any service the root does not provide FAILS with the name of that service. |
| `src/dsh/resolve-hooks.ts` | `module.registerHooks` singletons for `@deepseek-ai/{cordis, schemastery}` and `cosmokit`, whether declared as peers or as dependencies. A `@deepseek-ai/dsh-tools` compat module exporting only what the fixture uses (`defineTool` and its error types). Any other `@deepseek-ai/dsh-*` fails loudly. |
| Vendored deps | `@deepseek-ai/{cordis, schemastery}` and `cosmokit`, from npm if published, otherwise vendored from `refs/dsh@00102833/vendor` with MIT notices. **This is the first task of the PR**, because everything else builds on it. |
| `dist/codewhale-extension-host.mjs` | Committed, plus `LICENSES.txt`. |
| `test/*.test.mjs` | Framing, the refusal list, the resolve hooks, owner teardown (reverse-order disposers, awaited async disposer). Tests run against `dist/`. |

**Fixtures:**
- `crates/tui/tests/fixtures/extension_host/dsh-workspace-deps/`: a `plugin.json` whose `extensions."net.codewhale".native` names `index.mjs`, a Cordis plugin that does `ctx.plugin(await import('./vendor/dsh-tool-workspace-dependencies/lib/index.js'), {source: './payload/runtime.json'})`. Alongside it, the published `lib/index.js` of `@deepseek-ai/dsh-tool-workspace-dependencies@0.1.7-alpha.2` (MIT) and a small payload. The plugin injects only `tools`, registers `load_workspace_dependencies`, and has an **async `ctx.effect` disposer** that awaits in-flight work (`src/index.ts:245-248`). With no `root` config it is read-only.
- `crates/tui/tests/fixtures/extension_host/refuses-approval/`: `ctx.plugin` providing `approval`. It must FAIL activation.
- The protocol conformance corpus (`*.json`), which both sides parse and round-trip.

**CI** (`.github/workflows/ci.yml`):
- In the existing Node-22 JS job: `cd crates/tui/extension-host && npm test`, then `npm ci && npm run build && git diff --exit-code dist`.
- In the Rust test job that runs the new integration test: `actions/setup-node` pinned to 22, so the real-bundle test **runs** on CI. It skips, with a printed reason, only when `CODEWHALE_EXT_HOST_TESTS` is unset *and* no suitable Node is found, which is the local case.

**Acceptance (all run, with pass/fail counts in the commit message):**
1. `npm test` passes (it now includes `npm --prefix crates/tui/extension-host test`), and `npm run check:web` passes.
2. A Rust integration test spawns the real bundle under Node ≥22.19, installs the DSH fixture through the existing reviewed installer (`plugins/install`), reviews it, enables it, and drives one model-path call of `load_workspace_dependencies`. It asserts:
   - the tool is **deferred** and reachable through `tool_search`;
   - the approval request is raised (`Required`), and its text names `extension:dsh-workspace-deps`;
   - after approval, the result JSON comes from the fixture payload;
   - the same call from `execute_tools` in a main-session turn suspends for approval with a `<parent>.<seq>` id attributed to `extension:<plugin>`, and no host `tool/call` is sent before approval; allow returns the result to the program and deny fails only that nested call. (Lane 6562 landed first, as #6583; the assertion was updated in the phase-1 fixes. Without a gate, `execute_tools` still refuses it as needs-approval.)
3. Disabling the plugin mid-call: Rust's registry drops the handle at once; the in-flight call resolves as cancelled within 500 ms; the host acks `disposed` only after the async disposer settles, and `leaked` is empty.
4. `kill -9` on the host: the in-flight call fails with the typed `not_available("extension host exited")`; `/plugin` shows *failed* with the stderr tail; nothing respawns until the next session or `/plugin enable`.
5. The `refuses-approval` fixture FAILS activation with a diagnostic, and no registration survives on either side.
6. An extension tool registered with a native name (`read_file`) is refused at `registry/register`. A second one named like an existing `~/.codewhale/tools` script is skipped at turn build with a diagnostic, and the script tool is unaffected.
7. With the flag off: the host is never spawned (asserted by test); `PluginActivationPolicy` hashes equal v3 (a unit test pins the digest); and `hyperfine 'codewhale --version'` shows no change. With the flag on and no `native` plugin enabled, the host is never spawned.
8. The protocol conformance corpus round-trips on both sides.

**Explicitly not in phase 1:** commands, `core/call` and the generalised `CallGate`, heartbeat and auto-restart, `dsh.bundle.patch` recognition, `!!js`, prompt sections, skills, MCP, hooks, script-tool migration, sandbox, registry installs, and multiple hosts or trust tiers. **Trust note for phase 1:** a single third-party host is acceptable only because the flag is Experimental and nothing builtin shares it.

### Phase 2: commands, restart, `core/call`, patch rows, skills, prompt sections

- `command/run`, with owner-bound entries in `UserCommandRegistry`. `{kind: 'submit'}` becomes a visible user turn. This needs a policy bump only if commands get their own capability (§4.6).
- Heartbeat, crash tracker, and auto-restart with activation replay (§1.4).
- `core/call` plus the generalised `CallGate` (§4.2), with `ToolCallSource::Extension`. This requires lane 6562 merged, because the suspension machinery is the lane's.
- `dsh.bundle.patch` with literal rows (§6.1).
- Generated protocol (`schemars` → TS, with a drift check).
- `register.skillRoot`, `prompt/propose` and `storage/*`.
- The trust-tier split: host #0 builtin, host #1 third-party. This is **required before phase 3**.
- (Removed: running Computer Use's `agent.mjs` as a host plugin. It is a remote SSH agent, not host code, R7.)

### Phase 3: MCP protocol orchestration moves to the host

**Entry gates:**
- the phase-2 tier split exists, so `builtin:mcp` runs in host #0 only;
- decision D1 is ratified in Ops CURRENT_DECISIONS §26: system Node with a doctor diagnostic;
- the `ExternalToolDispatch` seam exists, landed by the lane or at the start of this phase.

**Work:**
- ProcessBroker (merged spawn path) and FetchProxy with auth injection.
- `builtin:mcp`, the SDK client adapted from DSH `mcp-client`.
- The actual pinned SDK transport under the existing `McpConnection`/`McpPool`
  Rust authority facade: catalogue/session/permission/provenance and credential
  state stay in Rust. `mcp_backend = "host"` demand starts the Builtin tier
  independently of optional Native activation; harness/third-party activation
  continues to require the actual Native policy, including after restart.
- Grant enforcement at the broker and proxy with the default-deny method policy (§4.4, threat 5).

**Exit gates:**
- Every existing `mcp/tests.rs` behaviour test, not the transport internals, passes against the host through a Rust↔host conformance harness that reuses the MCP fixture servers.
- The CU screenshot latency gate (§5.1).
- Code mode's nested MCP calls pass the lane's MCP tests unchanged through `HostMcpDispatch`.
- The unchanged recorded corpus and real HTTP/OAuth/Computer Use/broker tests
  pass; platform isolation and the measured Phase 3 gates pass before a mandatory
  default. D9 forbids making tier 0 mandatory before Linux/Windows host sandboxes.
- Ops CURRENT_DECISIONS §26 orders the default flip for one release, then
  deletion of the native protocol/client adapters after their last selected
  consumers migrate. The Rust pool's authority/cache stays. Optional Native
  activation is not made mandatory by MCP. An MCP user without supported Node
  gets the existing doctor diagnostic, per ratified D1; no Rust fallback.

### Phase 4: hooks and script tools move to the host

- A shell-script executor is added as `builtin:hooks` in the host, spawning through the broker.
- DSH `tools/pre-execute` and related listeners, plus `hooks-claude-code` and `hooks-codex` rows.
- `builtin:script-tools` registers `~/.codewhale/tools` scripts and `[tools.overrides]` `Script` / `Command` entries as extension tools (R1, D9).
- The fold, sanitizers and authority stay in Rust.
- Deletion rows 4 (hooks) and 4 (script tools) land.

### Phase 5: full native DSH loading and host sandbox

- Complete patch composition, `!!js` for trusted plugins, `inject` / `intercept` / `isolate`, read-only `agents` and `sessions` projections, the gated `fs` and `subprocess` shims, and agent-preset translation.
- The OS sandbox for the host, including the secret-path denials (§4.5).
- `install/dsh.rs` shrinks to the reviewer.
- Deletion row 5 lands. The docs listed in §6.3 are reconciled.
- Decide `integrations/dsh`.

---

## 9. Risks, budgets, tests, open decisions

### 9.1 Risks

| Risk | Likelihood / impact | Mitigation |
|---|---|---|
| Node becomes mandatory for MCP (brew, cargo, Termux users) | High / high | D1. A doctor diagnostic with an exact fix. The catalog cache still advertises tools, which fail with a clear "needs Node" error. Phase 3 does not graduate without the decision |
| Broken or old Node on `PATH` (observed on this machine) | Medium / medium | `resolve_node_at_least`: try `[extension_host] node`, then every `node` on `PATH` in order, keep the first that runs and meets the version floor, and report the rejected candidates. (Not `resolve_node()`, which probes only the first `node` on `PATH`) |
| Same-process plugins can borrow each other's authority | Certain / medium | Trust-tier hosts (§1.5), union-of-capabilities disclosure, broker-level grants for MCP, and the sandbox in phase 5 |
| IPC cost for large MCP results | Medium / medium | Phase 3 latency gate; `direct_stdio` for builtins as a fallback |
| Cordis not published on npm; vendoring churn from DSH | Medium / low | Vendor at a recorded commit with a modification list, as DSH itself does |
| DSH peer-API drift (`dsh-tools` compat) | High / low | Generate the compat surface from DSH usage. Unknown exports fail loudly per row |
| Two hook executors or two MCP stacks living too long | Medium / high | Phase exit criteria include the deletions; stop rule (§7) |
| Trust-receipt churn when v4 becomes default | Certain / low | Flag-scoped policy until graduation; release note |
| A plugin writes to stdout and corrupts framing | Medium / low | Console rebinding. Framing detection leads to host exit (phase 1: marked failed) or a restart counted as a crash (phase 2) |
| Existing script tools self-approve (`# approval: auto`) and shadow built-ins (R1) | Certain (shipping today) / medium | Out of the host's scope until phase 4. The host never copies the behaviour (always `Required`; refuse or skip on collision). D9 decides the migration |
| Unsandboxed host plugin reads on-disk secrets (R5) | Possible / high | Honest review text; the flag stays Experimental; third-party host plugins do not leave Experimental before the §4.5 sandbox, whose profile denies the secret paths |
| Phase 1 couples to the unmerged code-mode lane | Was certain in the old draft / medium | Removed: phase 1 uses registry `ToolSpec`s, which work on main and under the lane (R2, R3) |
| Memory multiplies with sub-agents and runtime-API threads | Was likely in the old draft / medium | One host per engine process per trust tier (R9) |
| Out-of-date sources (SHA-6628; DSH-ADOPTION doc; `dsh.rs` module doc) | Certain / low | Reconcile in the phases named in §6.3 and §8, phase 0 |

### 9.2 Budgets

| Metric | Budget | Basis |
|---|---|---|
| Added startup with no host plugins or MCP | **0 ms**: the host is not spawned | Lazy spawn |
| Added startup with host plugins (phase 1) | **0 ms on the first-prompt path.** The spawn and handshake run in the background, and tools join at the next turn | Design rule; verify with the acceptance test |
| Added startup for MCP users (phase 3) | 0 ms with a cached catalog. With an uncached `required` server, the host spawn (≤400 ms cold) overlaps provider warmup | Target |
| Node process start | 22–31 ms warm, 96 ms first cold | Measured here: Node 22.20, `node -e ''`, 6 runs |
| `hello → ready` with a bundle ≤ 2 MB | ≤ 150 ms p50 warm, ≤ 400 ms cold | Target; measure in phase 1 with `hyperfine` |
| Per-plugin activation | ≤ 50 ms p50. The hard timeout is 5 s, after which the plugin FAILS | Target |
| First prompt | Never waits on the host. Required MCP servers are the only eager case, and they start in parallel with provider warmup | Design rule |
| Host RSS, idle, no MCP | ≤ 80 MB **per engine process per trust tier** (not per session or sub-agent). V8 heap capped with `--max-old-space-size=256` | Target; not measured |
| Per MCP server in the host | ≤ 5 MB beyond the server process itself | Target |
| Bundle size | ≤ 2 MB phase 1, ≤ 4 MB with the SDK | Embedded in the binary |
| Hook deadline (phase 4) | 2 s default, 30 s max. The clock pauses while waiting on the user | omp `runner.ts` precedent |
| Shutdown | 2 s drain, then SIGTERM, SIGKILL at 3 s | omp precedent |

### 9.3 Test strategy

This follows the evidence rules in AGENTS.md: match the evidence to the surface.
- **TS unit tests** (`node --test` against `dist/`, run by root `npm test` through `npm --prefix`, and by an explicit Node-22 CI step): framing, shims, refusal list, the `dsh-tools` compat module, fiber teardown and leak detection.
- **Protocol conformance corpus.** JSON fixtures that Rust `serde` and TS both parse and round-trip. Generated types plus the drift check arrive in phase 2.
- **Rust integration tests** that spawn the **real bundle** under a real Node. **CI installs Node 22 for this job, so they run there.** Locally they skip with a visible reason when no Node ≥22.19 is found. They cover admission, gating (direct, and code mode as refused on main or suspended once the lane lands), revocation, crash, and anti-spoofing: a stale token, a cross-owner handle, a name collision against natives and against scripts, and a refused `provide`. Restart and replay join in phase 2.
- **MCP parity harness** (phase 3): the existing MCP fixture servers are driven through both `McpPoolDispatch` and `HostMcpDispatch` with the same assertions, until the native protocol adapters retire after the one-release default window. The Rust pool's authority/cache remains. Transport-internal Rust tests are deleted with the code they test; security guards and behaviour tests are re-targeted to the actual SDK/broker consumers.
- **The DSH corpus** reuses the 41 `test_convert_plugin.py` cases as *install and review* cases (phase 5), plus the pinned real packages:
  - `tool-workspace-dependencies` in phase 1;
  - `dsh-mcp-client` rows in phase 3;
  - `hooks-claude-code` rows in phase 4.
- **Performance:** `hyperfine` on startup with the flag on and off, and the CU screenshot latency gate in phase 3.
- **Not claimed by any of the above:** hosted CI, a real provider call, or a customer run. Each is a separate level of evidence.

### 9.4 Decisions and remaining choices

Ops CURRENT_DECISIONS §26 is the current decision authority; older alternatives
below are historical proposals unless that table leaves the choice open.

- **D1. Node for MCP users — ratified.** System Node ≥22.19 with the existing
  doctor diagnostic for the initial 0.10.1 host. The September 29 founder
  follow-up makes Bun the target runtime and bundled executable after the four
  measured D2 gates; Node remains a diagnosed fallback while that work lands.
  Selected Host failures never silently fall back to the Rust adapter.
- **D2. Node floor.** `^22.19 || >=24` for the host, matching DSH. I recommend it: Node 20 is end-of-life, and `module.registerHooks` needs ≥22.15. Computer Use is a separate MCP server process (R7) and keeps its own `>=20` floor. Several CI jobs still pin Node 20 (`ci.yml` version-drift and conversion jobs, release workflows), and none of them run host code.
- **D3. Stdio secrets.** Relayed broker, which I recommend; or a `direct_stdio` ticket for builtin servers only if the phase-3 latency gate fails.
- **D4. Legacy stdio proxy:** resolved by Ops CURRENT_DECISIONS §26 D5: drop it. The native `serve --mcp` server remains.
- **D5. Third-party host plugins that need ambient network or exec** under the phase-5 sandbox: refuse them, or offer an explicit "unsandboxed host" tier with its own warning.
- **D6. Native addons (`.node`) in host packages.** Refuse them in v1, which I recommend: they defeat the closure hash and the sandbox story.
- **D7. `integrations/dsh` external launcher:** keep it or delete it once native loading lands.
- **D9. Script tools** (`~/.codewhale/tools`, `[tools.overrides]` `Script` / `Command`; R1). Moving them into the host in phase 4 is required by "only the host is extensible", and it changes two behaviours:
  - (a) `# approval: auto` is ignored and scripts become `Required`, rememberable per tool;
  - (b) a script can no longer replace a built-in.

  **Recommended: accept both, and ship them with a release note.** Today's behaviour is exactly the self-approval and shadowing the host refuses. The alternative, keeping script tools in Rust as a second extension path, contradicts the founder direction.
- **D8. Tracker.** Link SHA-6521, SHA-6628 and #6562 to the host epic. Filing goes to the Codewhale team, which is public. This design contains no inference or business details, so it is safe to file there.

## 10. File index

**Engine** (`/private/tmp/cw-wt-6446` @ `8a835d7c4`):
- `crates/tui/src/plugins/{activation.rs:22,26-37,80-100, manifest.rs:35-59,254, agent_plugin.rs:649, builtin.rs:1-30, install/{dsh.rs,stage.rs}}`
- `crates/runtime/src/features.rs`
- `config.example.toml:1228`
- `crates/tui/src/mcp.rs:536-575,753-756,2852-2865`
- `crates/tui/src/dependencies.rs:54-85,278-297` (`probe_executable`, single-probe `resolve_node`)
- `crates/tui/src/tools/{plugin.rs:1-20,57-172, registry.rs:53-63,360-440, spec.rs:1436-1550, codemode.rs:193-238}` (main)
- `crates/tui/src/core/engine.rs:4672,4895-4915,7360-7400`; `core/engine/{tool_catalog.rs:136-155, tool_preparation.rs:40-60, dispatch.rs:870-886}`
- `crates/tui/src/mcp.rs:929-1000` (`ReviewedStdioLaunch`), `:1265-1274` (`approval_hint_for`); `mcp/oauth.rs:700-760`
- `crates/tui/src/plugins/{agent_plugin.rs:627-672,1646-1652, registry.rs:2205-2225, types.rs:230-240, manifest.rs:160-180,245-262,1676-1690}`
- `crates/secrets/src/lib.rs:60-70`
- `crates/core/src/lib.rs:24,902,914,1278-1285,2460`; `crates/app-server/src/lib.rs:376,394`; `docs/RUNTIME_API.md:55`
- `crates/workflow-js/{Cargo.toml, src/lib.rs}` (code mode runs in-process QuickJS, which is untrusted model code and a separate runtime from the host by design)
- `.github/workflows/{ci.yml:186-307, web.yml:13-40}`; `crates/tui/plugins/computer-use/{package.json, agent.mjs}`; `crates/tui/src/tools/subagent/mod.rs:2758,3204`
- `crates/tui/src/hooks/{config.rs:27, executor.rs}`
- `crates/tui/src/commands/{user_commands.rs, user_registry.rs:97}`
- `crates/tools/src/lib.rs:386`
- `crates/tui/plugins/computer-use/{package.json, plugin.json}`
- `package.json` (workspaces)
- `npm/{codewhale, runtime-sdk}/package.json`

**Lane** (`feat/code-mode-mcp-6562`):
- `crates/tui/src/tools/codemode.rs:200-243,250-282,502-556,1175`
- `crates/tui/src/core/engine/turn_loop.rs:39,~4706-4790`

**DSH** (`refs/dsh` @ `00102833`):
- `packages/skill/tool-workspace-dependencies/{package.json, src/index.ts:237-277}`
- `packages/mcp/mcp-client/{package.json, src/transport.ts, src/index.ts}`
- `packages/core/tools/{package.json, src/index.ts}`
- `docs/subsystems/commands.md`
- `packages/feedback/command-feedback/src/index.ts`
- `packages/preset/agent-preset/skills/cordis-plugin-development/references/host-plugin.md`
- `package.json` (engines)
