# Shared command / control-plane contract

> 阅读简体中文版：[zh_hans/COMMAND_CONTROL_PLANE.md](zh_hans/COMMAND_CONTROL_PLANE.md)。

Issues #1888 and #4022.

Codewhale exposes the same lifecycle operations on three surfaces: a slash
command typed into the composer, a bound hotbar slot, and a CLI entrypoint.
Before this contract those three could — and did — drift: `/fleet status`
showed the current session's sub-agents while `codewhale fleet status` read the
durable ledger, and the CLI's Lane verbs had no slash equivalent at all.

The contract is one typed descriptor table plus one executor per domain, in
[`crates/lane/src/control.rs`](../crates/lane/src/control.rs) and
[`crates/engine/src/fleet/control.rs`](../crates/engine/src/fleet/control.rs).
`codewhale-lane` is the lowest crate the thin CLI facade and the TUI both
already depend on, so there is exactly one place the contract can live without
forking.

## Vocabulary

Unchanged and load-bearing: **Fleet = who**, **Workflow = order**, **Lane = one
running Workflow**, **Runtime = where/how**. Auto-Review is a permission
posture, never a reviewer role. There is no "Operation" product noun; the
internal `ControlOperation` type names control-plane *verbs* and never appears
in user-facing copy.

## What a descriptor pins down

Every `(domain, verb)` pair has exactly one `OperationDescriptor`, keyed by a
stable id of the form `<domain>.<verb>`:

| Field | Meaning |
| --- | --- |
| `id` | `lane.status`, `fleet.interrupt`, … — the same string on every surface and in every receipt |
| `authority` | `read` or `write`. Not a permission posture: it says whether the verb observes durable state or mutates it |
| `persistence` | Which durable store the effect lands in (`lane_registry`, `fleet_ledger`) |
| `target` | What exact identity it acts on (`none`, `lane_run`, `fleet_worker`, `fleet_run`) |
| `retry` | `idempotent` or `unsafe` |
| `surfaces` | Which surfaces offer it |
| `backend` | `Implemented`, `NotImplemented { hint }`, or `SurfaceLimited { available_on, hint }` |
| `slash_command` / `cli_invocation` | The exact bindings; the hotbar action id is always `slash.<slash_command>` |

The verb table today:

| Verb | Lane | fleet |
| --- | --- | --- |
| `list` | read, whole registry | read, whole ledger |
| `status` | read, one Lane | read, whole ledger |
| `interrupt` | write, one Lane (idempotent) | write, one worker (idempotent) |
| `restart` | **no backend** — a Lane is re-created, not restarted | CLI-only (drives the manager loop) |
| `resume` | **no backend** — a stopped Lane's Runtime session is gone | write, one run (idempotent) |

## No surface advertises what it cannot do

`OperationDescriptor::availability(surface, ctx)` returns either `Available` or
a typed `UnavailableReason` with a sanitized hint:

- `backend_not_implemented` — nobody has built it. Every surface refuses.
- `surface_not_supported` — the backend exists but not here. The hint names the
  surface that works (`codewhale fleet restart <worker-id>`).
- `no_lane_registry` / `no_fleet_ledger` — the durable store does not exist yet.

Availability is probed **read-only**. `LaneRegistry::open_default` and
`FleetManager::open` both create their store as a side effect, so a status verb
probes `lane_registry_root()` / `fleet_ledger_path()` first. Otherwise "this
workspace has no fleet ledger" silently becomes "here is an empty fleet ledger
I just made".

## Exact run identity

`parse_target` is the single target parser for all three surfaces: exactly one
token, exact ids only (no prefix or fuzzy matching), ASCII alphanumerics plus
`-`, `_`, `.`, no path separators, and a hard reject when a targetless verb is
handed an argument.

A write may be **fenced** by appending `@<lifecycle-seq>`:

```
codewhale lane interrupt lane-a1b2c3d4@3
/lane interrupt lane-a1b2c3d4@3
```

If the durable record has moved past sequence 3, the verb is rejected with a
`conflict` failure and the observed sequence, instead of stopping whatever
happens to be there now.

## Receipts

Every invocation returns a `ControlReceipt` carrying the operation id, surface,
authority, persistence scope, availability, target, `LifecycleOutcome`
(`inspected`, `transitioned`, `no_change`, `rejected`, `failed`), the observed
lifecycle sequence, retryability, an optional bounded sanitized failure, and an
optional bounded run page. `ControlReceipt::render()` is the only renderer; the
CLI prints it and the slash command returns it as a message. `--json` on the
Lane verbs emits the same struct.

## Typed unknowns

Run DTOs never imply absence. `Known<T>` is either `Known(value)` or
`Unknown(reason)` where the reason is `not_recorded`, `not_applicable`, or
`redacted`, and renders as `<not_recorded>` rather than a blank or a plausible
default.

Concretely: the fleet receipt's `FleetResolvedRoute` records the **effective**
reasoning tier only, so `requested_reasoning` is `not_recorded` — it is not
back-filled from the effective value, and `reasoning_downgraded()` returns
`None` rather than guessing. The Lane registry records no route or usage at
all, so those fields are uniformly `not_recorded`. fleet runs are fenced per
task rather than per run, so a fleet run's `lifecycle_seq` is
`not_applicable`.

## Bounds and redaction

- Run lists are pages: `DEFAULT_RUN_LIST_LIMIT` (50) with a hard
  `MAX_RUN_LIST_LIMIT` (200) ceiling, and the page reports `total` and
  `truncated` so a bound is never mistaken for an empty result.
- Status worker rows and inspection artifact rows cap at 24 with an explicit
  omission notice.
- Receipt detail caps at `MAX_DETAIL_LINES` (40) lines of `MAX_DETAIL_LINE_CHARS`
  (240) characters.
- Every operator-visible string passes through `sanitize_line`: `$HOME`-rooted
  paths collapse to `~/…`, credential-shaped `key=value` pairs and known token
  prefixes (`sk-`, `ghp_`, `xoxb-`, `Bearer`, …) become `[redacted]`.

## Model-visible tool surface

Unchanged. This work adds no tool, no tool parameter, and no prompt text; the
model-facing sub-agent surface is still `agent` only. No tool-schema regression
measurement is required.

## Tests

- `crates/lane/src/control.rs` — descriptor-table integrity, the five-verb
  symmetry across both domains, authority/persistence/target agreement across
  surfaces, availability rules, target parsing and lifecycle fencing, receipt
  round-trips, bounding, and redaction; plus executor tests proving all three
  surfaces get byte-identical results for the same durable Lane and that
  interrupt is idempotent and fenced.
- `crates/engine/src/fleet/control.rs` — route/usage DTO projection with typed
  unknowns, bounded pages and rows, absent-ledger reporting without creation,
  CLI-only `fleet.restart`, and cross-surface identity of `fleet.status`.
- `crates/tui/src/commands/groups/core/lane.rs` and `…/fleet.rs` — slash verbs
  map onto the shared operations, `/fleet status` reads the durable ledger
  rather than session sub-agents, and bare dispatch (what the hotbar fires) is
  read-only.
- `crates/cli/src/lib.rs` — the CLI exposes exactly the declared Lane verbs
  under the same ids, and `lane stop` is a compatibility spelling of
  `lane interrupt`.
