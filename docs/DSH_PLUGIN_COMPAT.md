# DeepSeek Harness Plugin Compatibility

Codewhale runs reviewed DeepSeek Harness (DSH) plugins in its experimental
Native extension host ([EXTENSIONS.md](EXTENSIONS.md)). The target is DSH
**0.2.2-rc.1** with Cordis **4.1.0**, pinned in `crates/engine/extension-host/src/dsh/compatibility.json`.
The Rust Engine keeps turns, sessions, approvals, credentials and receipts; a
DSH service is offered only where it maps onto one of those existing owners.
Everything else is refused by name when the plugin activates, never faked.

This page is the equivalence checklist. "Verified" names the check that was
run against the commit that landed the behavior. It is not a release
qualification, and hosted CI, Linux and Windows runs are listed separately.

For the separate launcher that runs your own installed `dsh`, see
[DeepSeek Harness integration](INTEGRATIONS_DSH.md).

## Packages, review and trust

| DSH behavior | In Codewhale | Status |
| --- | --- | --- |
| Install a bundle package (`dsh.bundle.patch`, Cordis patch YAML) | `/plugin import dsh`, `POST /v1/apps/plugins/import/dsh/preview`, then install with the previewed content hash | Verified on macOS over the Runtime API |
| A new plugin does nothing until reviewed | installed disabled and untrusted; trust needs the review token; enable is separate | Verified on macOS over the Runtime API |
| Trust covers the bytes and the compatibility table | changing either invalidates earlier trust | Verified by focused Engine tests |
| Bytes changed after review | install refused with a 409 conflict, nothing installed | Verified by focused Engine tests (`17ebeb5f61`) |
| Update and rollback walkthrough | existing update/revoke/remove paths | Not verified |
| Desktop app preview → install → trust → enable | App plugin settings | Built; not walked through live |

## What a plugin may inject, and what it may never provide

Compatibility table version 3 separates injectable services by their source
and keeps the authority refusal list separate:

- `provided_services` groups what a plugin may **inject** under `dsh`,
  `cordis` and `codewhale`. `HostRoot` in
  `crates/engine/extension-host/src/root.ts` admits the union of those groups
  and fails activation for any other injected name.
- `refused_services` is what a plugin may **not provide** — the Codewhale core
  owns that authority. The root reflection wrapper refuses a plugin that
  calls `provide` for one of those names. The single exception is the host's own
  shim of that name, provided once by the root itself.

So `tools` is injectable and unprovidable: a plugin uses the Engine's tool
registry through the host shim and can never install a second one. `events`,
`logger`, `reflect` and `registry` are Cordis framework built-ins supplied by
the host root rather than Codewhale shims. `loader` maps to the reviewed loader
and is injectable inside reviewed compositions without being advertised as
a root service. `prompt`, `avatars`, `mcp` and `shellHooks` are Codewhale
compatibility services, not services declared by DSH. This reclassification
keeps the same injectable names. Its changed table digest requires fresh
Native review and trust for previously reviewed plugins.

The rc.1 update reuses loader 1.1.0, include 1.1.0 and group 1.1.0 while
keeping reviewed module imports and non-writing composition: tree mutations
now report through `commit(change)`, which a reviewed composition ignores, and
bare specifiers keep the native import fallback instead of upstream's
writable `.cordis/resolve.mjs` helper. The `dsh-tools` definition excerpt is
taken from rc.1's split build with unchanged definition semantics. The
injectable and refused service lists are unchanged; the changed table digest
requires fresh Native review and trust. The legacy preset adapter remains
explicitly pinned to `0d1f5000`; its replacement upstream runtime packages
are not adopted. Node support remains `^22.19.0 || >=24.0.0`, and Bun remains
`>=1.4.0`. rc.1 is a prerelease. Earlier rc.5 receipts are not evidence for
this target, and local checks of this upgrade do not repeat the live
walkthroughs below.

## Headless services

| DSH service | In Codewhale | Status |
| --- | --- | --- |
| `tools.register` | Engine tool registry; input and output checked against the declared schema; every call goes through an approval card | Verified on macOS over the Runtime API |
| Plugin tools on the **first** turn | the first tool build waits up to 10 s for the plugin host to finish loading | Verified on macOS over the Runtime API (`25225ef051`); before the fix, `tool_search` could not find the tool on turn one |
| `commands.register` | thread-scoped Runtime commands with replay-safe operation keys; a command may submit one text prompt with plugin attribution. A collision with a built-in receives a plugin-specific name and does not replace the built-in. | Existing command path verified; namespaced collision fallback requires the focused current-source check |
| `sessions.flush(exec.agent.session)` | Engine captures the conversation so far and waits for the Runtime, terminal or `exec` store to confirm the save | Verified on macOS over the Runtime API, including reload after an Engine restart (`17ebeb5f61`); other `sessions` methods refused |
| `userQuestions.ask` | the Engine's own question flow; answers are typed (choice, your own text, or skipped) | Verified on macOS over the Runtime API (`fff5a476f9`); `wait` and timed asks refused |
| `storage`, `skills` | DSH-shaped adapters to Engine-owned services | Supported subset; full API inventory not qualified |
| `prompt`, `avatars`, `mcp`, `shellHooks` | Codewhale compatibility services | Not upstream DSH service declarations |
| `logger`, `events`, `reflect`, `registry` | Cordis framework built-ins | Supplied by the shared root |
| Tool definition `deferLoading` | boolean presentation hint accepted without changing Engine loading policy | `when` and execution/authority fields remain refused |
| `subagents.getProvider('spawn')`, `subagents.startActivation`, caller delivery, `result`, `dispose` | the existing sub-agent manager, through the same approval planning as the model's `agent` tool | Verified on Linux over the Runtime API: the child start raised its own approval card, the child's request carried the persona and lacked the denied tools, and the result came back to the plugin; see the mapping below |
| `subagents.start` (older DSH form) | refused | Not implemented |
| `tools.get(name, agent)` | read-only lookup in the calling tool's catalog | Verified with the child-agent probe |
| `tools.restrict` outside a child's `agent/created` handler | refused | Not implemented |
| `agents`, `systemPrompt` | injectable so plugins that name them activate; `agents` offers nothing and `systemPrompt` methods refuse outside a child's `agent/created` context | Verified with the child-agent probe |
| `llm`, `approval`, `credentials`, `fs`, `subprocess` | refused by design until each maps to Engine authority | Not implemented |
| Rich or media tool output | dropped | Not implemented |
| Git review scopes | Bounded `execFile` metadata requests use the Engine Git tool and its approval path; the Native host still cannot read the repository directly | Current-source qualification pending |

## Surfaces walked through live

The same DSH-format probe plugin (a tool that saves the session and asks one
question, plus two commands), run against a local fake model provider on
macOS with no account or spend:

| Surface | What was seen | Status |
| --- | --- | --- |
| Runtime API | preview → exact-hash install → review → trust → enable; first-turn tool; approval; question answered through `POST /v1/user-input`; command result | Verified |
| `codewhale exec --auto` | first-turn tool runs and saves the session; the plugin's question is refused with `NO_PROVIDER` because no person is present to answer | Verified |
| Desktop app | the tool's approval card; the plugin's question rendered as a native card (option descriptions, your own answer, Skip, Send answers); the answer reaches the plugin; `/probe-count` in the command palette with plugin attribution | Verified (plugin installed through the Runtime API, not the app's own settings flow) |
| Terminal UI (interactive) | — | Not walked through; its question and approval code is covered by focused tests only |

## Child agents: mapping

DSH child agents are not a second agent system. Each DSH call below is a thin
adapter onto Codewhale's existing sub-agent manager and the one turn loop,
inside the parent's permission and budget envelope. The host `subagents` shim
calls three Engine methods (`subagent/start`, `subagent/result`,
`subagent/dispose`), each bound to the live tool invocation's ticket. The
start goes through the turn loop's nested-call gate as a typed `agent` start,
so it gets the same planning, hooks and approval as the model's own `agent`
call.

| DSH call | Codewhale owner | Boundary |
| --- | --- | --- |
| `subagents.getProvider('spawn')` | fixed descriptor for the manager's fresh-context spawn | reports `inheritsParentContext: false`; no other providers |
| `subagents.startActivation({ provider: 'spawn', childId, request: { parent, prompt, agentOptions }, signal, delivery })` | the sub-agent manager's prepare step, then commit, with the existing origin, depth, write-scope and route admission | `parent` must be the exact live invocation agent; a requested model must be a route the parent may already use |
| `subagents.start('spawn', { persona, toolFilter, prompt })` (older DSH) | not provided | refused |
| `ctx.on('agent/created')` | a view of the prepared child, raised between prepare and commit | not a source of truth; anything a handler does there only adds to the commit configuration |
| `agent.ctx.systemPrompt.section({ name: PERSONA_PREFIX_SECTION, … })` | the child's frozen activation persona, inserted once into its own system prompt and recorded in the child's launch manifest | added to the child's prompt, never replacing core policy or another agent's prompt; other section names refused; at most 32 KiB |
| `agent.ctx.tools.restrict({ deny })`, `toolFilter.deny` | denied tools merged into the child's inherited tool limit | can only narrow |
| `ctx.tools.get(name, parent)` | lookup in the parent's effective tool catalog | read-only |
| `activation.result` | the manager's settled result and the child's full transcript output, checked against its recorded hash | fails when the output cannot be captured; never reads a truncated checkpoint as complete |
| `activation.dispose()`, `signal` | the manager's exact-run disposal and cancellation, including the run's own descendants | waits for the child to stop; reports uncertainty instead of success |
| `delivery: 'caller'` | caller delivery: the result goes to the plugin, not into the parent model's inbox | completion events and usage are still recorded |

Known limits: only the `spawn` provider exists; the child always starts with a
fresh context; a requested model must be one the parent may already use; only
text output blocks reach the plugin (others are counted in a diagnostic); and
the `agent/created` hook fires only for children the same plugin starts.

## Plugin UI (`dsh.client`)

An imported DSH plugin's client UI is served, on the seats the Engine admits.
The importer retains the reviewed client closure; at serve time the Engine
re-verifies it byte for byte against the reviewed hashes, maps each declared
slot key to a seat, and loads one Engine-generated bootstrap built from those
bytes. A native client is admitted only on `customize.plugins.detail`. A slot
key with no seat answers `plugin_ui_seat_unsupported`; several keys on one seat
need the caller to name one (`plugin_ui_dsh_slot_required`). A closure that needs
a host module the frame does not provide, uses ES module syntax or is not valid
JSON answers `plugin_ui_unsupported`. Code Review 1.0.8's client decorates
global command candidates through DSH services that the current detail-slot
renderer does not provide, so it is not qualified.
The route that forwards a client's requests to a plugin web server answers
`plugin_route_not_served` (501): no plugin web server exists yet.

The selected Electron host's reviewed asset transport and isolated frame do
not establish compatibility with those services. Its real network containment
and revocation still need qualification. An earlier macOS WebKit probe found
WebRTC traffic despite content rules, CSP, an opaque sandboxed frame and a
restrictive Permissions-Policy; that historical result does not qualify the
current Electron/Chromium host.

## Host services and grants

DSH services that need the core to act for a plugin (`llm`, `credentials`,
`settings`, `terminal`, `remote`, the plugin catalog, workspace files) are
brokered: the Rust core serves them, and only for a plugin whose review granted
them. See [Host services](EXTENSIONS.md#host-services).

| Behavior | Status |
| --- | --- |
| A service is granted only by a review that showed it, recorded with who approved | Verified by focused plugin and Engine tests on Linux |
| A review recorded before grants existed, or an altered or unreadable record | Grants nothing; verified by focused tests |
| Plugins share one process: a service is given only while every loaded plugin holds the same grant | Verified by focused Engine tests |
| Turned off, revoked or reviewed again | Grant withdrawn at once, including a call in flight; verified by focused Engine tests |
| Services offered by this build | None. Each is switched on separately, after its own review |
| A real plugin using a granted service end to end | Not verified |

## Containment

| Behavior | Status |
| --- | --- |
| Native tier runs only under an OS sandbox (Seatbelt, bubblewrap, LPAC) | Verified on macOS with the compiled host (`17ebeb5f61`) |
| Plugins in the Native tier are isolated from each other | No: they share one trusted host process |
| Detached descendants on macOS | Can outlive the host; they keep the sandbox |
| Windows, current source | Not verified |

## Published plugins

`@michengai/dsh-code-review` 1.0.3 injects `commands`, `subagents`, `agents`,
`tools`, `systemPrompt`, `sessions` and `userQuestions`. Its `review` command
collides with Codewhale's built-in command; the plugin command is registered
under a plugin-specific name shown in the command catalog. Base-branch and commit selection use a bounded `execFile('git', ...)` adapter.
It translates known metadata queries to `Git` action `review_scope` through a
live `core/call` ticket and the existing approval path. The Engine selects the
workspace, disables hooks, filters, paging and lazy fetch, caps output, and
contains the process under a deadline and cancellation. Other Git commands and
subprocess APIs retain the Native sandbox restrictions. Native v8 requires
re-review before this additional compatibility path is admitted. The
published plugin's end-to-end review flow is not yet qualified.
