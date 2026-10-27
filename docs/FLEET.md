# Agent fleet

> Baca terjemahan bahasa Indonesia: [id/FLEET.md](id/FLEET.md)

Agent fleet is the local-first roster and member-selection layer for durable
multi-worker runs. It does not execute or authorize work. After fleet resolves
who should participate, the delegated coordinator launches a headless
`codewhale exec` run and the Runtime tracks it durably. See
[AGENT_RUNTIME.md](AGENT_RUNTIME.md) for how sub-agents, `exec`, and
fleet-backed workers converge on one runtime. In product language, a user may
still "open a sub-agent"; in architecture language, durable nested work uses a
fleet member identity with delegated runtime execution.

## Naming and compatibility boundary

**Fleet** is the public product noun. The durable ledger, saved rosters, config
tables, and `--fleet` flag share that name:

| Surface | Canonical |
| --- | --- |
| CLI | `codewhale fleet …` |
| Slash command | `/fleet …` |

These shared names are load-bearing wherever changing them would break
existing workspaces, receipts, or scripts:

- the durable ledger `.codewhale/fleet.jsonl` and the log directories
  `.codewhale/fleet/` and `.codewhale/fleet-host/`;
- saved rosters `fleets/<name>.toml` and their `schema = "fleet"` header, under
  `$CODEWHALE_HOME/` or the workspace's `.codewhale/` (checked-in rosters at the
  workspace root's `fleets/` are still read);
- the `[fleet]` config table (inline `[fleets.*]` tables were removed in 0.9.14; named fleets live in `fleets/<name>.toml` files);
- the `codewhale workflow run --fleet <name>` flag;
- wire, receipt, and control-plane operation ids such as `fleet.status`.

The rest of this document uses fleet as the public product noun and retains
these literal paths, keys, flags, and ids.

Use a fleet roster rather than anonymous short-lived `agent` fanout whenever a
delegated run needs stable member identities across retries, sleep/restart,
remote execution, receipts, or a ledgered audit trail. The initial CLI surface
is:

For a guided start-to-monitor walkthrough that combines fleet task specs with
Workflow authoring, see [fleet + Workflow Tutorial](FLEET_WORKFLOW_TUTORIAL.md).

```sh
codewhale fleet init
codewhale fleet run tasks.json --check   # validate only; nothing is created or launched
codewhale fleet run tasks.json --max-workers 4
codewhale fleet status
codewhale fleet inspect <worker-id>
codewhale fleet logs <worker-id>
codewhale fleet artifacts <worker-id>
codewhale fleet interrupt <worker-id>
codewhale fleet restart <worker-id>
codewhale fleet resume <run-id>
codewhale fleet stop --all
```

`codewhale fleet resume <run-id>` is the restart-recovery verb: it replays the
ledger, reconciles any in-flight lease whose worker stopped heartbeating
(retrying within the task's budget, else failing and escalating per the alert
policy), and prints the post-resume status. It launches no new work and is
idempotent, so it is safe to run after a manager exit, laptop sleep, or runtime
restart.

Coordinator state for fleet-backed runs is stored under the workspace in
`.codewhale/fleet.jsonl`. Worker logs and adapter logs are stored under
`.codewhale/fleet/` and `.codewhale/fleet-host/`.

## Public contract: identity, membership, and selection

**Fleet** = the user's model inventory: who is in the roster and which member is selected.

A public fleet identity consists only of:

- a stable member id and an optional user-facing name;
- a semantic role, such as `explore`, `implement`, or `reviewer` (the legacy
  spellings `worker`, `scout`, `builder`, `verifier`, `consultant`, and `oracle`
  are still accepted on input and map to `general`, `explore`, `implement`,
  `test`, and `advisor`);
- an exact provider/model identity, or an explicit inherited route;
- visible roster state or origin.

Project or workspace trust, filesystem and network reach, secret access,
approval mode, sandboxing, tool authorization, and every other form of runtime
authority are separate delegated-coordination and Runtime policy inputs. They
are never fleet identity fields, and they never select or reroute a fleet
member. The Runtime applies and clamps those policies only after member
selection; if the selected member cannot run inside the effective envelope,
launch fails closed instead of choosing somebody else.

Natural-language member selection is deterministic. A caller may name:

- an exact member id, optionally as `member:<id>` or `id:<id>`;
- a unique user-facing member name, optionally as `name:<name>`;
- a unique semantic role, for example `explore` or `role:explore`;
- an exact pinned model id, for example `deepseek-v4-flash`, or its offline
  display name, for example `DeepSeek V4 Flash`; or
- an exact `route:<provider>/<model>`.

An unqualified exact member id wins. Every other match succeeds only when it
identifies one distinct roster member. Multiple matches produce an ambiguity
error that names the candidates and asks for `member:<id>`; Codewhale never
picks whichever match happened to be listed first. Users do not need to know
an internal role label such as `explore`: a unique member name, display model, or
exact model id is equally valid. Saved v2 fleets store that optional human name
as `display_name` (the input alias `name` is also accepted); it must be one
trimmed printable line of at most 80 characters.

### Your fleet as models

The same fleet file answers a third question: **which models has this person
put in their fleet?** Every exact `provider` + `model` pin in the selected
fleet — the operator route, each pinned member, and each explicitly marked
shortlist row — is a fleet model. Executable member rows supply the roles it
fills; a shortlist row has no role. All remain in the same fleet file. Legacy
members that omit `role` still use their id as the role identity.
Shortlist rows carry only model choices; reasoning, instructions, and capability
requirements belong on executable role members and are refused on shortlist rows.

- `/fleet models` prints the fleet: `provider/model · roles · price · context ·
  tools`, facts read from the model catalog. With no selected fleet the line
  reads "Your fleet is the session model only".
- `/fleet add <provider> <model> [role…]` adds a model (one member row per
  role, or one `shortlist = true` row for a role-less add). The provider must be one you configured
  and, when the catalog knows the provider, must serve that exact id.
  A role member asked to run the fleet's own operator route inherits it
  instead of pinning — the role follows when the operator moves; a pin on
  any other route is the deliberate opt-out. Files that already pin the
  operator route are read as inheritance.
  With no fleet selected, a user-global fleet named `My fleet` is created and
  selected first. `/fleet remove <provider> <model>` drops every row that pins
  the route; the operator route is changed with `/fleet save`, not removed.
- In `/model`, `⇧F` adds or removes an explicit shortlist row for that exact
  route. It preserves saved role pins and the operator route;
  fleet models are listed first, labelled `fleet · <roles>`, ahead of your
  own `⇧P` pins and the provider lists. `/models` prints the fleet before the
  provider's list.

The operator model reads this list when it assigns sub-agents (design
`MODEL-ROUTING-CATALOG-20260901.md` §10, slice F2).
The model-facing roster resolves roles through the same route admission code
as a start. Explicit profiles and manual role pins remain authoritative; a
unique saved role pin also applies to a start naming only its role. Task model
choices are available for unpinned roles, constrained to the selected models
plus the session route. Shortlist model rows disclose their own exact route
independently of any role pin.

### Interactive and persistent status

`/fleet status` and `codewhale fleet status` are the **same** command on two
surfaces. Both read the durable `.codewhale/fleet.jsonl` ledger for the
workspace, through one shared control-plane contract, and both report the same
verb id (`fleet.status`), read-vs-write authority, persistence scope, and
receipt. When the workspace has no ledger they say so with a typed reason
(`no_fleet_ledger`) instead of rendering an empty-looking "all clear" — and
neither creates the ledger as a side effect of reading it.

The current interactive session's sub-agents are a **different set**, and now
have their own name:

- `/fleet workers` (or `/subagents`, or Tab / `w` from the `/fleet` roster)
  shows sub-agents attached to the current TUI session. It does not read the persistent ledger.
- `/fleet list|status|interrupt|resume` and `codewhale fleet
  list|status|interrupt|resume` act on the durable ledger.
- `codewhale fleet restart <worker-id>` is CLI-only: it re-leases the task and
  then drives the manager loop to completion. `/fleet restart` does not
  silently do a smaller thing — it reports `surface_not_supported` and names
  the CLI command.

The contract behind this — descriptors, availability reasons, exact-identity
targets, receipts, typed unknowns, and bounds — is documented in
[`docs/COMMAND_CONTROL_PLANE.md`](COMMAND_CONTROL_PLANE.md).

## Authoring agent profiles (`/fleet setup`)

Agents: the durable artifact is the profile TOML described below; the
key-by-key walkthrough is the human interactive path.

`/fleet setup` (also `/fleet setup edit` / `new`) opens an in-TUI wizard for
authoring a reusable agent-team profile. Bare `/fleet` and the
`roster`/`roles`/`profiles`/`party` aliases open the selected fleet's member
roster. `/fleet saved` opens the named saved-fleet picker. `/fleet workers` opens the
current-session worker view; `/subagents` is a
compatibility shortcut for that view. For durable run history, use
`/fleet status` or the shell command `codewhale fleet status` described above —
they are the same command.

The wizard is progressive: you make one focused choice at a time — a **role**,
then a **model** (`inherit`, or a concrete model from *any configured
provider*, not only the one the parent session is currently using), then
**where the profile lives**, and finally a **review** of the member identity
and route. When the review also previews thinking, tools, approvals, or another
execution control, those rows summarize separate Runtime policy; they do not
become fleet identity or member selectors. The header shows "Saves to: …" on
every step — the choice you still have to make, or the exact resolved file
once you have made it. Nothing is written until you activate the save control
on the review step.

The **Destination** step is a focused two-option list:

- **This project** writes `<workspace>/.codewhale/agents/<role>.toml`. It
  applies to this project only and takes precedence over a Personal profile
  with the same id. When project profiles are disabled for the session
  (`--no-project-config`) or the workspace folder is unavailable, the option is
  shown disabled with that reason; the wizard never falls back to Personal on
  its own.
- **Personal** writes `$CODEWHALE_HOME/agents/<role>.toml` and is available in
  every project on this machine, except where a project has its own profile
  with the same id.

For the highlighted option the step shows the exact file, whether saving would
create a new file or **replace an existing one**, and the precedence
consequence for the roster. The review step repeats those facts under
"Saves to" and names the final action by its effect — **Save to this
project**, **Save as Personal profile**, or **Replace …**. Replacing an
existing file asks for a second confirmation on the save control. Reopening a
saved member from `/fleet` starts from what is on disk: its member identity,
route, and save scope. Thinking (`inherit`, `off`, `low`, `medium`, `high`,
`max`, or `auto`) is adjusted on the review step, but remains a route
execution setting rather than part of the member's fleet identity.

Profile scope controls where a role definition is reusable; it does not widen
the authority of a running operation and is not a project-trust setting. To
coordinate several nearby repositories, start Codewhale from their shared
parent directory so that parent is the workspace. Project/workspace trust,
external paths, filesystem and network reach, secrets, approvals, sandboxing,
and tool authorization come from delegated-coordination and Runtime policy.
For nested delegation, Runtime intersects the requested child posture with the
live parent. For standalone `codewhale fleet` execution, Runtime instead uses
the bounded tool-authority envelope minted from the task's explicit write
scope together with live config, sandbox, and platform enforcement. Neither
path reads authority from the profile's storage scope or identity selector.
A worker whose envelope grants read-only shell access runs the same read-only
command grammar as an in-session read-only agent, including pipelines, chains
and a leading `cd` (see "Read-only shell commands" in `docs/SUBAGENTS.md`);
Admitted `gh` reads also need the envelope's network grant. `npm view` remains
outside the read-only grammar because npm configuration can select executable
helpers; a network grant does not authorize those helpers.

Picking a concrete model pins its provider explicitly: the saved profile records both
`model` and `provider` fields, so the route it names doesn't depend on
whichever provider happens to be active when the profile is later loaded.
Pressing **Enter** ("start") on the review step previews the exact starter
profile TOML inline on that same screen; nothing is written until you save it.
The `provider` field may be a built-in provider id such as `openrouter` or a
user-named OpenAI-compatible provider configured under `[providers.<name>]`
such as `lm-studio`; the launch path preserves that id and fails closed if the
provider is not configured.

Profiles are also how the model-facing `agent` tool selects a route: a child
either runs as a `profile` (whose saved route and thinking tier it uses
exactly) or inherits the operator's model. Per-task `model`, `model_strength`,
and `thinking` remain advertised for unpinned roles; saved profile and manual
role pins refuse overrides. See docs/SUBAGENTS.md for the advertised field
list and the parse-accepted compat list.

When a provider is configured, the review step also offers model-assisted
drafting behind an explicit preview-before-save gate:

- Press **`m`** to have your first configured model draft the profile. The
  draft arrives sanitized and bounded. Separately, the Runtime keeps its
  conservative execution floor (no shell or trust escalation and approval
  required) regardless of what the model proposes.
- **Drafting is not saving.** The exact rendered TOML preview renders
  inline on the review step (not in a separate scrollable viewer), so nothing
  is saved until you press **`g`** or **Enter** to save (or press `m` again
  to redraft). Saving writes the profile to the project or personal scope
  shown in the preview.

## Naming: Modes, Workflow, and fleet

These names describe different layers, not competing systems. Plan and Act are
the everyday work modes. Operate accepts ordinary messages and keeps the
parent's normal tool surface under the same approval, sandbox, shell, ask-rule,
and repository protections as Act. It prefers background fleet workers for
independent, parallel, isolated, or long-running work, but does not require a
worker for every executable step. Workflow is an optional orchestration overlay
for work that needs ordering, gates, shared budgets, replay, or deterministic
fan-in.

The short public vocabulary is:

- **Fleet** is the durable roster and deterministic member-selection surface.
  It records member ids and names, semantic roles, provider/model identities,
  and roster state. Fleet is also the name used by storage and wire formats.
- **Workflow** = what order the work follows: phases, gates, budgets, replay,
  and fan-in.
- **Lane** = one running Workflow instance and its live progress.
- **Runtime** = where, how, and with what authority selected work executes.
  Runtime owns the local or remote process, provider route, project/workspace
  trust, filesystem, network, secrets, approvals, sandbox, tools, and API
  boundary.

- **Workflow** is the repeatable plan and user-facing orchestration
  overlay: a script/IR that decides which phases and agents run next, keeps
  intermediate results out of the main conversation, and can be inspected or
  rerun. A Workflow run should have a visible progress view and a clear active
  header state instead of feeling like a hidden background task.
- **Fleet** is the durable roster and deterministic member-selection surface:
  member ids and names, semantic roles, pinned or inherited provider/model
  identities, and roster state. The delegated
  coordinator and Runtime own launch concurrency, leases, heartbeats, logs,
  receipts, tools, sandboxing, approvals, and authority.
- **High fan-out** is a behavior of a Workflow run, not a separate system:
  when a phase needs many workers at once, Workflow dispatches them as a
  fleet-backed run (durable workers, receipts, goal re-dispatch) rather than
  reviving prompt-only sub-agent fanout.
- **Fan-in is explicit:** when the user needs one combined result, an owner
  aggregates, verifies, and synthesizes the worker receipts. Independent tasks
  may finish separately; dispatch is never presented as completion.

UI guidance: keep the main transcript calm. A Workflow run should appear as a
compact progress card plus workbar rows (the strip under the composer, or
a side workbar) with phase names, worker counts, receipts, and nested
indentation for child workers. Use the whale mark sparingly as an active
header/status signal; avoid repeating emoji-heavy rows for every worker.

## Saved fleets and the Reasoning Router

A selected v2 fleet freezes each selected member's id, semantic role, provider,
and model identity into the durable run before a Workflow starts. Save the
fleet as `fleets/<name>.toml` under the workspace's `.codewhale/` (where the
fleet editor saves folder fleets) or under `$CODEWHALE_HOME`; a checked-in
`fleets/<name>.toml` at the workspace root is also read.
Models cannot replace those identity or route assignments at runtime:

```toml
schema = "fleet"
schema_revision = 2
name = "release"

[operator]
provider = "deepseek"
model = "deepseek-v4-pro"

[[members]]
id = "implementer"
display_name = "Release Builder"
role = "implement"
provider = "zai"
model = "glm-5.2"

[[members]]
id = "advice"
role = "advisor"
provider = "openai"
model = "gpt-5.6"
```

The workflow crate's older `schema = "exact"`, revision 1 files are migration
input only. Do not author revision-1 files; the selected roster and setup UI
read and write only `schema = "fleet"`, revision 2.

`workflow(fleet: "release")` runs a saved Fleet without selecting it. At
Workflow start, a member with no pin takes the Fleet's `[operator]` route or,
without one, the session route and reasoning tier; that frozen route is what
runs and what receipts name, and editing the file mid-run changes only the next
Workflow. If a saved Fleet and an older exact/legacy file share a name, the
Workflow refuses to guess; qualify the saved one as `user/<name>` or
`folder/<name>`. Members with `instructions` or `requires` cannot run in a
Workflow yet.

Reasoning is a separate route-execution decision, not fleet identity. The
optional Reasoning Router is a reusable Runtime service, not a fleet member.
Save one profile at `routers/<name>.toml` in any search root and reference it
from any number of fleets:

```toml
name = "luna-low"
schema = "reasoning_router"
schema_revision = 1
provider = "openai"
model = "gpt-5.6-luna"
call_reasoning = "low"
```

At runtime it may choose only the reasoning tier for an already-frozen worker
route. It cannot change the member, provider, model, or semantic role. The
Router call itself is capped at `off` or `low`; more expensive values are
rejected. A manually selected worker reasoning tier makes no Router call. Route
and reasoning receipts name the worker model and, when used, the Router's exact
provider/model so the operator can see which model did which job. If the same
bare Router or fleet name exists in more than one root, qualify it as
`codewhale_home/<name>`, `workspace/<name>` (the workspace's `.codewhale/`), or
`workspace_root/<name>` (the workspace root) instead of relying on shadowing.

Compatibility schemas may serialize `reasoning`, `permissions`, tool hints, or
other execution settings beside a member. Those values are not fleet identity,
member selectors, or active authority. A valid legacy `schema = "exact"` roster snapshot
retains its old `permissions` bytes only while verifying and replaying that
snapshot's recorded content hash; a fresh capture emits the authority-free
member shape. New-run validation rejects legacy roster
`security_policy` and worker `trust_level` fields; configure execution authority
through Runtime policy. The delegated coordinator resolves and durably freezes
the member first. Runtime then applies either the delegating parent's effective
ceiling or, for standalone fleet CLI work, Runtime execution configuration plus
live sandbox/platform enforcement. That boundary may
reduce or refuse the selected worker's execution surface, but it must never
choose a different member or route. See
[`docs/MODES.md`](MODES.md), [`docs/SUBAGENTS.md`](SUBAGENTS.md), and
[`docs/AGENT_RUNTIME.md`](AGENT_RUNTIME.md) for the enforcement contract.

Reasoning receipts record the requested tier *and* the tier the provider was
actually asked for. Those differ whenever a route cannot express the requested
one — Codewhale's route normalizer sends `high` for a requested `low` on most
routes, and Z.AI's GLM routes express only thinking on/off — so the receipt
reports the real request rather than the label that was selected. The value a
call actually carries is spelled by that route's own normalizer, not by the tier
label: an OpenAI Codex route is asked for `xhigh`, not `max`, and cannot be
asked for `off` at all.

A durable fleet CLI receipt keeps the selected profile id in
`effective_permissions.profile_id`, the resolved semantic role in
`resolved_route.role`, and the effective Runtime surface in the permission,
shell, and tool-scope fields. An exact Workflow launch receipt records
`member_role` separately from an optional Runtime `posture_role`, plus the
fingerprint of the effective authority envelope checked at the spawn boundary.
A member named `auditor` can therefore retain that identity while Runtime
reports a `custom` posture and independently proves the narrower surface it
enforced.

A Workflow start fails closed on anything decidable locally: an unresolvable
provider or model, a missing credential, a client that cannot be built for a
member's route, or an `auto` member with no usable Reasoning Router. Per-task
validation that the spawn boundary would refuse anyway — notably a write-capable
member with no declared `write_roots`/`exact_files`/`coordination_contracts` —
is checked before the Router is called, so an invalid task never spends a
routing request. If a spawn fails *after* a Router decision, the receipt is
still recorded: the tokens were spent, and any cross-provider disclosure already
happened.

## Manager-owned Workflow fan-in

When parallel work must return one combined answer, prefer a manager-owned
Workflow over a flat `agent` fan-out. Default shape:

1. **Cast one manager** (operator or workflow orchestrator).
2. **Fan out** child tasks through `workflow` (`task()`, `parallel()`,
   `pipeline()`, `phase()`) or a single manager session that owns the children.
3. **Wait** for child receipts or completion events.
4. **Aggregate and verify** load-bearing claims before treating them as facts.
5. **Synthesize** one result the operator can depend on.

Raw `agent` fan-out fits independent work with no combined result. When
results must be merged, compared, or verified, route through `workflow` so
the manager owns fan-in — that is what the shape above is for, not a ban
on simpler patterns when nothing needs combining.

## Workflow on fleet

The intended high-capability path is agent-authored. When the main agent
decides a task needs more durable coordination than turn-by-turn sub-agent
calls, it drafts a Workflow script/IR, presents the run plan according to the
active permission mode, and the runtime compiles it into typed fleet work.

fleet remains the sub-agent roster and member-selection surface. It owns member
identity, membership, semantic roles, saved provider/model pins or inheritance,
and roster state. Workflow owns the orchestration plan:
branch, sequence, loop, expand, review, and reduce decisions. The delegated
coordinator and Runtime own slot admission, launch concurrency, the execution
ledger, and every authority decision. A workflow script receives no direct
shell, filesystem, network, provider-secret, cancellation, or TUI authority;
workers perform real work as `codewhale exec` processes under the effective
Runtime policy.

Default Workflow-to-fleet validation is intentionally bounded:

- 1,000 total worker agents per Workflow run;
- 16 live worker agents at once; larger populations queue (block) on the host's
  per-run concurrency gate until a live slot frees, then route through fleet;
- Workflow IR structural nesting no deeper than 5;
- Runtime child delegation defaults to 3 levels and has an opt-in hard ceiling
  of 8, independently of the Workflow document's structural depth;
- bounded loops only (`max_iterations` required);
- bounded dynamic expansion only (`max_children` plus a template required).

These are delegated-coordination population limits, not fleet identity and not
a demand to launch everything at once. A 1,000-agent Workflow should still
drain through the configured Runtime worker pool. They are also not model-step
budgets: omitted or zero `max_steps` remains unbounded. An explicit positive
`max_steps` may cap that task, while wall-clock timeouts, cancellation,
provider safeguards, heartbeats, and admission controls remain independent.

Recommended model layouts, such as a DeepSeek Pro orchestrator with Flash
workers in the first ring and cheaper workers farther out, are presets only.
Every slot can inherit the active model or carry an explicit model override.
Inheritance is literal: the model you select in `/model` is the **operator**
(the pinned first row in `/fleet roster`), and any worker whose task spec and
roster profile pin no model runs on that session model. Once a selected member
has an exact provider/model pin, the Runtime does not silently reroute that
identity because a policy input differs; it either runs that route inside the
effective envelope or fails closed. Route receipts record the requested and
resolved identity.

The setup UI should render this as an expanding grid: an orchestrator plus a
small number of visible sub-agent slots, with Right/Enter drilling into a slot's
next recursive ring rather than trying to show the whole tree at once.

## Task Spec

`codewhale fleet run` accepts JSON or TOML. `codewhale fleet run <spec> --check`
runs every validation a real run performs (spec shape, roster members, agent
profiles, model routes) and prints the same warnings, then stops: no ledger is
created, no run is written, no worker starts, and nothing is spent. A minimal
JSON spec:

```json
{
  "name": "local smoke",
  "tasks": [
    {
      "id": "lint",
      "name": "Lint",
      "instructions": "Run the lint check and report failures.",
      "expected_artifacts": ["log"]
    }
  ]
}
```

Workers are optional. If omitted, Codewhale creates local worker slots up to
`--max-workers`.

A spec file takes one of three shapes, chosen by its structure before any
field is read:

- a **document** — an object with `tasks` (and optionally `name`, `labels`,
  `workers`, `usage_ceiling`);
- a **task array** — a bare JSON array of task objects;
- a **single task** — one task object with `id` / `instructions` at the top
  level (JSON or TOML; a TOML file is never a task array).

Array and single-task files take their run name from the file name. Because
the shape is picked first, a malformed spec reports the real problem, for
example ``JSON spec document at tasks[1] (id "review"): missing field
`instructions` at line 7 column 5``. The checked-in
[`docs/examples/fleet-dogfood.toml`](examples/fleet-dogfood.toml) and the
tutorial's `tasks.json` are parsed by the test suite, so they stay valid.

Task specs are typed in Rust and keep verification data separate from worker
transcripts. Only the `worker` member/role reference participates in fleet
identity selection. The remaining execution fields are delegated-coordination
or Runtime inputs applied after the member is resolved. A task can declare:

- `id`, `name`, `description`, `objective`, and `instructions`
- `worker` role, tool profile, tools, and required capabilities
- `workspace` root, required files, writable paths, and environment allowlist
- `input_files`, extra `context`, `budget`, `timeout_seconds`, and `retry_policy`
- `expected_artifacts`, `scorer`, `tags`, and free-form `metadata`

None of those execution-policy fields becomes part of a fleet identity or an
alternate member selector. Omitted or zero `max_steps` means no model-step
ceiling; Codewhale must not synthesize a default step budget. (This is the
fleet file task-spec convention; model-facing `agent` calls differ — the tool
parser rejects an explicit zero. See `docs/SUBAGENTS.md`.) Explicit positive
step limits, timeouts, cancellation, provider safeguards, heartbeats, and
admission control are enforced independently by the delegated coordinator and
Runtime.

Workers write bounded artifact files under `.codewhale/fleet/` and ledger only
the artifact refs: kind, path, checksum, MIME type, and size. Receipts record
`pass`, `fail`, `partial`, `skip`, or `timeout`; failed receipts may also mark
the source as `transport`, `task`, or `verifier`. `codewhale fleet status`
surfaces those failure-source counts separately.

Deterministic built-in scorers are `exit_code`, `file_exists`, `regex_match`,
and `json_path`. Specs may also declare `command`,
`code_whale_verifier_prompt`, or `manual`; those record a partial receipt until
an explicit verifier pass completes.

### Using Role Presets

Tasks can reference a semantic role name to select one unique roster member.
Built-in role names (`smoke-runner`, `reviewer`, `builder`, `read-only`) remain
available for compatibility, and custom roles may be defined in
`[fleet.roles]`.

```json
{
  "name": "smoke check",
  "tasks": [
    {
      "id": "lint",
      "name": "Lint check",
      "instructions": "Run lint and report failures.",
      "worker": { "role": "smoke-runner" },
      "expected_artifacts": ["log"]
    }
  ]
}
```

After identity resolution, compatibility role presets may provide tool,
timeout, or retry defaults to the delegated coordinator. Those defaults do not
grant authority, do not change which member was selected, and remain subject to
Runtime clamping. A task spec may request its execution settings explicitly:

```json
{
  "id": "deep-review",
  "name": "Deep review",
  "instructions": "Review the entire crate for soundness issues.",
  "worker": {
    "role": "reviewer",
    "tools": ["cargo", "rg", "git"],
    "capabilities": ["rust"]
  },
  "input_files": ["crates/**/*.rs"],
  "budget": { "max_tokens": 32000 },
  "expected_artifacts": ["log", "report"],
  "scorer": { "kind": "regex_match", "path": ".codewhale/fleet/report.md", "pattern": "finding|all clear" }
}
```

### Multi-Task Run Example

A single fleet run can dispatch several independent tasks in parallel:

```json
{
  "name": "CI gate",
  "tasks": [
    {
      "id": "check",
      "name": "Compile check",
      "instructions": "Run cargo check --workspace and report errors.",
      "worker": { "role": "builder" },
      "expected_artifacts": ["log"],
      "scorer": { "kind": "exit_code" }
    },
    {
      "id": "clippy",
      "name": "Clippy lint",
      "instructions": "Run cargo clippy --workspace and report warnings.",
      "worker": { "role": "reviewer", "tools": ["cargo", "cargo-clippy"] },
      "expected_artifacts": ["log"],
      "scorer": { "kind": "exit_code" }
    },
    {
      "id": "security",
      "name": "Secret audit",
      "instructions": "Search for plaintext secrets and report any matches.",
      "worker": { "role": "read-only", "tools": ["rg"] },
      "input_files": ["crates/**/*.rs"],
      "expected_artifacts": ["log", "report"],
      "retry_policy": { "max_attempts": 1 }
    }
  ]
}
```

## Alerts

fleet alerting is disabled by default. A caller must supply an enabled alert
config before anything is sent. Routes match typed fleet event classes, not log
strings:

- `stale`
- `restart_exhausted`
- `needs_human`
- `budget_exceeded`
- `verifier_failed`
- `run_completed`

Adapter config stores environment variable names, not secret values. Send-time
code resolves those names from the environment or a future secrets provider.
Ledger records store only audit labels such as `slack`, `webhook`, or
`pagerduty`; task specs persisted in the ledger redact webhook URLs and routing
keys.

Example alert config shape:

```json
{
  "enabled": true,
  "dry_run": true,
  "routes": [
    {
      "events": ["stale", "restart_exhausted", "verifier_failed"],
      "adapter": "ops-slack"
    },
    {
      "events": ["restart_exhausted"],
      "adapter": "pager"
    }
  ],
  "adapters": {
    "ops-slack": {
      "kind": "slack",
      "webhook_env": "CODEWHALE_FLEET_SLACK_WEBHOOK",
      "channel": "#codewhale-fleet"
    },
    "pager": {
      "kind": "pager_duty",
      "routing_key_env": "CODEWHALE_FLEET_PAGERDUTY_ROUTING_KEY",
      "severity": "critical"
    }
  }
}
```

Use dry-run to inspect a redacted adapter payload without sending:

```sh
codewhale fleet alert-dry-run \
  --event stale \
  --run-id fleet-demo \
  --worker-id fleet-demo-local-1 \
  --task-id release-triage \
  --reason "worker heartbeat stale since 2026-06-13T02:00:00Z" \
  --adapter slack
```

The payload includes the run id, worker id, task id, status, short reason, and
safe inspection commands such as `codewhale fleet status` and
`codewhale fleet inspect <worker-id>`. Endpoints, webhook secrets, and
PagerDuty routing keys are shown as `<redacted:env:...>`.

## Status Surfaces

`codewhale fleet status` shows compact counts for queued, running, completed,
partial, failed, restarted, escalated, cancelled, stale, and verifier/transport
failure sources. `inspect` shows the worker state plus the current task
objective, role, host, heartbeat, latest event, artifact refs, latest error, and
alert state. `logs` prints bounded log artifact contents, and `artifacts` lists
artifact refs without embedding large payloads.

The Runtime API exposes the same ledger-backed projection behind the existing
runtime auth middleware:

```text
GET  /v1/fleet/runs
GET  /v1/fleet/runs/{run_id}
GET  /v1/fleet/runs/{run_id}/workers
GET  /v1/fleet/workers/{worker_id}
POST /v1/fleet/workers/{worker_id}/interrupt
POST /v1/fleet/workers/{worker_id}/restart
POST /v1/fleet/runs/{run_id}/stop
```

Action endpoints call the same manager controls as the CLI and record their
decisions in the fleet ledger.

## Manager-Agent Runbook

Manager agents should treat fleet operations as typed, ledgered control-plane
work. Start with `codewhale fleet status`, then inspect one run or worker with
`codewhale fleet inspect <worker-id>`, `logs`, and `artifacts`. Use direct
reads of `.codewhale/fleet.jsonl`, host logs, or remote files only when the
typed CLI/API surface cannot provide the required evidence.

Classify the worker before taking action:

- `transient failure`: stale heartbeat, host timeout, interrupted transport,
  retryable provider/network error, or an adapter status that can plausibly
  recover without changing the task.
- `task failure`: the worker completed but produced an incorrect result,
  domain failure, missing required artifact, or explicit task-level error.
- `verifier failure`: the worker result exists, but the scorer/verifier failed,
  timed out, or disagrees with the receipt.
- `needs-human`: missing authority, secret request, destructive operation,
  repeated restart exhaustion, ambiguous product decision, or conflicting
  evidence that the manager cannot resolve from typed artifacts.

Choose one typed action:

- Restart a worker only when the failure is transient, retry budget remains,
  the task is idempotent or retry-safe, and no permission or secret boundary is
  involved: `codewhale fleet restart <worker-id>`.
- Interrupt or stop only when the current task is unsafe to continue or the
  operator explicitly asks for cancellation: `codewhale fleet interrupt
  <worker-id>` or `codewhale fleet stop --all`.
- Do not restart pure task failures by default; preserve artifacts and hand the
  receipt to the task owner unless the task spec says retrying can produce new
  evidence.
- For verifier failures, inspect scorer inputs and artifact refs first. If the
  verifier cannot be corrected through typed fleet actions, escalate for human
  review.
- For `needs-human`, draft an escalation instead of sending it unless alert
  config explicitly authorizes sending.

Safe Slack or PagerDuty draft:

```text
Codewhale fleet needs attention
Run: <run-id>
Worker: <worker-id>
Task: <task-id or unknown>
Classification: <transient failure | task failure | verifier failure | needs-human>
Reason: <one sentence, no secrets>
Latest typed evidence: codewhale fleet inspect <worker-id>; codewhale fleet artifacts <worker-id>
Safe log excerpt: <3 lines max or "see artifact <ref>">
Requested decision: <restart approval | verifier review | task owner review | permission decision>
```

Post-run summaries should include the run id, workers checked, classification,
typed action taken or drafted, expected ledger effect, artifact refs reviewed,
and next owner. Keep summaries bounded; link artifact refs instead of copying
full logs or transcripts.

The bundled `fleet-manager` skill mirrors this runbook for manager agents. It
is a first-party system skill and should be discoverable through the normal
skill registry after system skills are installed or refreshed.

## Host Adapters

The Runtime host-adapter boundary supports local child processes and explicit
SSH workers. Host choice is Runtime placement on a worker spec, not fleet member
identity or a member selector. It does not authenticate the host or grant
access. Adapters expose the same operations: start, read status, read bounded
logs, interrupt, restart, stop, and cleanup.

Local workers run as child processes with stdin closed and stdout/stderr written
to bounded host-adapter logs. They inherit only a small safe base environment
such as `PATH` and explicitly allowlisted variables.

SSH workers run through the system `ssh` client with `BatchMode=yes` and a
bounded connect timeout. Remote environment variables are sent with OpenSSH
`SendEnv`; values are not embedded in the local ssh argv or fleet logs.

Host keys must already be trusted: connections use `StrictHostKeyChecking=yes`
and never accept a new key automatically. An explicit `known_hosts` file limits
trust to that file; when it is omitted, OpenSSH uses its normal known-host stores.
Verify the host key before adding it to either store. The legacy
`host_key_fingerprint` field is unsupported and is rejected; migrate it to a
verified `known_hosts` entry. The `identity` file selects the client login key;
it does not verify the remote host.

Example SSH worker spec:

```json
{
  "id": "builder-1",
  "name": "Builder 1",
  "host": {
    "kind": "ssh",
    "host": "builder.example.com",
    "user": "codewhale",
    "port": 22,
    "identity": "~/.ssh/codewhale_fleet",
    "known_hosts": "~/.ssh/codewhale_fleet_known_hosts",
    "working_directory": "/srv/codewhale/work",
    "env_allowlist": ["CODEWHALE_PROFILE"],
    "codewhale_binary": "/usr/local/bin/codewhale"
  },
  "capabilities": ["local", "linux", "tests"],
  "max_concurrent_tasks": 1
}
```

Defaults are intentionally conservative:

- no hosted control plane or cloud provisioning is enabled;
- SSH requires an explicit host, working directory, and Codewhale binary path;
- secret-like environment names such as `TOKEN`, `SECRET`, `PASSWORD`,
  `API_KEY`, and `PRIVATE_KEY` are rejected from adapter allowlists;
- secrets should remain in Codewhale config providers or remote host config,
  not in task instructions, argv, or fleet logs.

## Runtime policy and authority are not fleet identity

fleet does not define a project/workspace trust level, filesystem or network
reach, secret access, approval mode, sandbox, tool set, or execution authority.
Those belong to delegated-coordination and Runtime policy. This separation is
load-bearing:

- member resolution considers only member id/name, semantic role,
  provider/model identity, and roster state;
- the selected identity is frozen before any authority policy is evaluated;
- the Runtime applies the live parent ceiling when one exists; standalone
  fleet CLI launches instead carry an explicit bounded authority envelope,
  and both paths remain subject to live sandbox and platform enforcement;
- no trust, permission, capability, secret, sandbox, approval, or tool-policy
  value may select another member or silently change its provider/model route;
  and
- receipts report requested and effective Runtime posture separately from the
  fleet member identity.

Older persisted configuration and protocol shapes may still contain fields such as
`security_policy`, `trust_level`, `permissions`, `capability_grants`, secret
references, host authentication, environment allowlists, or tool profiles.
They remain deserializable for ledger replay, but new fleet run creation rejects
`security_policy` and worker `trust_level` rather than pretending they grant
authority. Their presence in old data does not make them fleet variables or
grants. The active Runtime remains the final authority and fails closed when a
requested operation cannot be enforced.

For current enforcement behavior, use [Modes](MODES.md),
[Sub-agents](SUBAGENTS.md), [Agent Runtime](AGENT_RUNTIME.md), and the
[Command Control Plane](COMMAND_CONTROL_PLANE.md). Keep secret values out of
task instructions, arguments, logs, and receipts; adapter and Runtime layers
must continue to redact or reject them independently of fleet selection.

## Child grants: 0.10.1 scope and the 0.11 rework (#6298)

A child's authority is one grant object, `ChildGrant`
(`crates/agent-contract/src/worker_profile.rs`). It shipped in v0.10.0 (966ef974e1,
#5633). It has `files` (none / read / write), `shell` (none / inspect /
verify / full), `network`, `desktop`, a tool `surface`, the caller's explicit
`scope`, and `spawn`. Roles are presets over it (`ChildGrant::for_role`), and
`ChildGrant::resolve` intersects the preset with the parent-derived profile
and the caller's scope. The child's tool catalog, its dispatch refusals, and
its capability envelope all read the same grant, so a tool the child can see
is a tool it can call. `desktop` is in no preset.

`ShellGrant::Verify` also shipped in v0.10.0: the Verifier preset gets the
bounded built-in verification surface (default workspace checks, pure test
selection, bounded Git fetch and merge-tree) instead of a shell grammar.

**Other fixes shipped by v0.10.0** (on top of the grant):

- Children never inherit desktop or computer-control tools (b5e48cd31, #6296).
- A bounded verify surface for Git: `fetch` against a configured remote name
  and a read-only `merge_tree` (b89349286).
- Refusals name the sanctioned alternative and tell a child to report a
  blocked probe to its parent instead of working around it (23747acea).
- One reasoning vocabulary (c2bc1244d). Token budgets are tracked but never
  enforced (a7a8bdb33).

**0.10.1 scope.** This release adds no new grant-model code. #6298 is
re-scoped to the remainder below, which lands in 0.11 as its own slices.

**0.11 remainder** (one slice at a time):

1. **A `verify` shell mode for builds.** `cargo test`/`check` run under an
   explicit, bounded write scope (`target/`, refs), so a verifier can run the
   builds it is handed without a full shell.
2. **Classified tool families that fail closed.** MCP and desktop tools form a
   labeled family. A child gets that family only when the spawn grants it with
   a reason, and an unclassified tool is not granted.
3. **Legible grants.** The role picker, roster, and receipts show the effective
   grant, model, and thinking tier in plain words.

Related work is tracked in #6015, #5633, #6194, and #6232.
