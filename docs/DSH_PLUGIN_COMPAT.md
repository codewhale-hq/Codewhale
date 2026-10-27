# DeepSeek Harness Plugin Compatibility

Codewhale runs reviewed DeepSeek Harness (DSH) plugins in its experimental
Native extension host ([EXTENSIONS.md](EXTENSIONS.md)). The target is DSH
**0.2.1-rc.2**, pinned in `crates/tui/extension-host/src/dsh/compatibility.json`.
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

## Headless services

| DSH service | In Codewhale | Status |
| --- | --- | --- |
| `tools.register` | Engine tool registry; input and output checked against the declared schema; every call goes through an approval card | Verified on macOS over the Runtime API |
| Plugin tools on the **first** turn | the first tool build waits up to 10 s for the plugin host to finish loading | Verified on macOS over the Runtime API (`25225ef051`); before the fix, `tool_search` could not find the tool on turn one |
| `commands.register` | thread-scoped Runtime commands with replay-safe operation keys; a command may submit one text prompt with plugin attribution | Verified (command runs and replays) |
| `sessions.flush(exec.agent.session)` | Engine captures the conversation so far and waits for the Runtime, terminal or `exec` store to confirm the save | Verified on macOS over the Runtime API, including reload after an Engine restart (`17ebeb5f61`); other `sessions` methods refused |
| `userQuestions.ask` | the Engine's own question flow; answers are typed (choice, your own text, or skipped) | Verified on macOS over the Runtime API (`fff5a476f9`); `wait` and timed asks refused |
| `prompt`, `storage`, `skills`, `avatars`, `mcp`, `shellHooks`, `logger`, `events`, `reflect`, `registry` | provided | Supported subset; full API inventory not qualified |
| `subagents.startActivation`, `subagents.start`, caller delivery, disposal | refused | Not implemented; see the mapping below |
| `tools.get`, `tools.restrict` | refused | Not implemented |
| `agents`, `systemPrompt`, `llm`, `approval`, `credentials`, `fs`, `subprocess` | refused by design until each maps to Engine authority | Not implemented |
| Rich or media tool output | dropped | Not implemented |
| Direct `git` from a plugin | Native profile denies repository reads | Not implemented |

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

## Child agents: planned mapping

DSH child agents are not a second agent system. Each DSH call below is meant
to be a thin adapter onto Codewhale's existing sub-agent manager and the one
turn loop, inside the parent's permission and budget envelope. None of it is
available yet. The manager and Engine pieces build against current source,
but the plugin-facing bridge (the host `subagents` shim, its Engine protocol
methods and the compatibility entry) does not exist, so `subagents` stays
refused.

| DSH call | Codewhale owner | Boundary |
| --- | --- | --- |
| `subagents.getProvider('spawn')` | fixed descriptor for the manager's fresh-context spawn | reports `inheritsParentContext: false`; no other providers |
| `subagents.startActivation({ provider: 'spawn', childId, request: { parent, prompt, agentOptions }, signal, delivery })` | the sub-agent manager's prepare step, then commit, with the existing origin, depth, write-scope and route admission | `parent` must be the exact live invocation agent; a requested model must be a route the parent may already use |
| `subagents.start('spawn', { persona, toolFilter, prompt })` (older DSH) | the same prepare and commit path, with persona and filter supplied up front | same as above |
| `ctx.on('agent/created')` | a view of the prepared child, raised between prepare and commit | not a source of truth; anything a handler does there only adds to the commit configuration |
| `agent.ctx.systemPrompt.section({ name: PERSONA_PREFIX_SECTION, … })` | the child's frozen activation persona, inserted once into its own system prompt and recorded in the child's launch manifest | added to the child's prompt, never replacing core policy or another agent's prompt; other section names refused; at most 32 KiB |
| `agent.ctx.tools.restrict({ deny })`, `toolFilter.deny` | denied tools merged into the child's inherited tool limit | can only narrow |
| `ctx.tools.get(name, parent)` | lookup in the parent's effective tool catalog | read-only |
| `activation.result` | the manager's settled result and the child's full transcript output, checked against its recorded hash | fails when the output cannot be captured; never reads a truncated checkpoint as complete |
| `activation.dispose()`, `signal` | the manager's exact-run disposal and cancellation, including the run's own descendants | waits for the child to stop; reports uncertainty instead of success |
| `delivery: 'caller'` | caller delivery: the result goes to the plugin, not into the parent model's inbox | completion events and usage are still recorded |

Open before the bridge can land: a plugin-started child must pass the same
approval planning as the model's `agent` tool (the outer tool's approval is
not enough), and the `agents` service the code-review plugin injects needs a
defined, mostly refusing surface.

## Plugin UI (`dsh.client`)

Not supported. The DSH import receipt says so: the UI half of a package is not
converted and stays off until Codewhale has a renderer that can keep plugin UI
off the network. A macOS probe showed WebRTC traffic leaving a WebKit view
despite every available restriction (content rules, CSP, an opaque sandboxed
frame and a restrictive Permissions-Policy), so a process-level boundary is
required first.

## Containment

| Behavior | Status |
| --- | --- |
| Native tier runs only under an OS sandbox (Seatbelt, bubblewrap, LPAC) | Verified on macOS with the compiled host (`17ebeb5f61`) |
| Plugins in the Native tier are isolated from each other | No: they share one trusted host process |
| Detached descendants on macOS | Can outlive the host; they keep the sandbox |
| Windows, current source | Not verified |

## Published plugins

`@michengai/dsh-code-review` 1.0.3 injects `commands`, `subagents`, `agents`,
`tools`, `systemPrompt`, `sessions` and `userQuestions`. `subagents`, `agents`
and `systemPrompt` are still refused, so the unmodified plugin cannot activate
in Codewhale yet.
