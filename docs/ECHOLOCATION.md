# Echolocation

## Product definition and direction

**Echolocation is Codewhale's native, feedback-driven code navigation: use each
observation to narrow the next action, obtain sufficient source evidence, and
reach a correct result with less searching, rereading, and execution overhead.**
It is not merely a footer, a repository summary, or a cache-hit percentage.

This section defines the product target. It does not claim that all of the
adaptive behavior below is implemented; the current mechanisms and limits are
recorded separately below.

### The navigation loop

Borrow the useful principle of echolocation: broad orientation, a more focused
observation, then precise work near the target. More detail is useful when it
changes the next action, not simply because it is available.

1. **Orient cheaply.** Use the query, path, requested range, and existing evidence
   to identify plausible files or regions without dumping the repository.
2. **Focus.** Make observations actionable: qualified source windows and explicit
   relationships, rather than names that require another search to locate.
3. **Inspect enough to act.** Return the relevant body and necessary context, such
   as its imports or a directly involved helper. A tiny snippet that forces a
   whole-module reread is not a successful reduction.
4. **Follow useful relationships.** Increase detail around the selected target
   when a caller, callee, test, or configuration edge answers the task's question.
   Label lexical, syntax-derived, and semantic evidence distinctly. Unresolved
   relationships stay unresolved; same-directory proximity is not a call edge.
5. **Reobserve changes and stop expanding.** Changed source invalidates affected
   references. Once there is sufficient evidence for the requested action, avoid
   unrelated exploration. A host observation is not proof a model received it.

The model still selects actions through the existing Engine and tools. Use
available request and conversation signals before inventing another planning
call or control loop. The current visit-count buzz is a starting heuristic, not
an adequate definition of task relevance.

### Make both the navigation and its machinery efficient

- **Avoid unnecessary model round trips.** A search result should lead directly
  to useful evidence. Reuse existing tool composition for bounded dependent
  steps when the next step is mechanical, not a new reasoning decision.
- **Avoid repeated host work.** Where contracts permit, one admitted byte
  snapshot should serve range selection, source analysis, and echoes. Reuse
  derived facts for identical source rather than scanning, parsing, or
  serializing them again unnecessarily.
- **Pay only for useful observations.** Small output alone does not imply small
  execution cost. Bound analysis and relationship traversal, not just rendering;
  expensive optional detail must earn its cost in the current operation.
- **Keep reuse honest.** Freshness and current access checks remain mandatory.
  Do not replace current-byte validation with an mtime-only shortcut, or confuse
  host analysis reuse, provider prefix reuse, and model-visible delivery.
- **Stay native, index-free, and bounded.** Echolocation is the backend for the
  existing Codewhale tools. Extend their Engine, read/search/edit and LSP paths
  and bounded observations; do not build a repository index, indexing service,
  daemon, or parallel navigation system.

### First implementation direction and acceptance

Start with the existing Rust read-to-reference-to-focused-read path, not a new
repository-wide navigation framework. Profile cold, repeated, and post-edit
reads; distinguish source I/O and validation, line selection, syntax analysis,
neighbor traversal, serialization, and model wait time. Choose the first change
from measured avoidable work, and keep that change separate from new behavior.

The first backend optimization passes ready Rust syntax observations into
`sound_echolocation`, avoiding the legacy symbol scan whose results were previously
replaced during rendering. Unavailable syntax observations still use the legacy
scanner. Import roots are now collected during the existing link scan rather
than a separate whole-source traversal; reverse-link callers skip unused import
extraction. Link targets are still resolved against the filesystem on each call.
Repeated whole-source loading, hashing, and line scanning for narrow reads remain
investigation targets, not permission to weaken freshness.

Judge improvements by correct completed tasks first, then time to useful source
evidence, end-to-end completion time, model round trips, tool latency, source
bytes read, delivered context, and total token cost. Include cold-start,
invalidation, memory, and cancellation costs. Keep unsuccessful runs. A faster
read that causes more searches, wrong answers, or excessive retained state is not
a win. The existing fixed-task benchmark is a regression anchor, not the whole
product definition; each new capability needs a task that genuinely exercises it.

## Current implementation

Bounded navigation soundings. File tools echo their surroundings fresh
from admitted source bytes -- no repository-wide index, background work,
or new dependencies. Soundings are deterministic for the same source,
visibility gates, and buzz state: sorted, capped, byte-budgeted, and
`/`-separated on every OS. They ride tool results (append-only history),
never the session-pinned prefix. Repeating a result later in a transcript
does not itself earn a prefix-cache hit: reuse requires the same preceding
request bytes, tool order, and provider route.

## What each surface emits

- **read** appends a footer: `[Pod (N): …]` sibling mates (bare
  names — the dir is the file just read, so it goes unstated) plus
  `[Sound: N symbols: …; imports: …; links: …]` (line-numbered
  symbols, import roots, cross-pod file links; same-pod links render
  bare and import roots a link already names (file stem or package
  path) are dropped). Lowercase Rust `read` also publishes bounded syntax
  navigation as described below. Total footer budget remains 1200B.
- **Terminal buzz**: after two distinct files are read in one pod, the
  next read there adds `heard by …` — podmates whose resolved links
  include this file. Sparse clicks while searching, high-resolution
  echoes at close range.
- **read miss** (`NotFound` only — never on denylist/permission refusals)
  appends `[Miss echo: <dir>]`: podmates of the missing file's parent,
  so a typo still orients the next call.
- **grep / file_search** chart each matched dir: mates with `(match)`
  marks, under the calling search's own visibility gates (denylist,
  excludes, extensions, gitignore). Pods never enumerate names the
  calling search hides.
- **project_map** sounds each key file: one line of `name:line` symbols
  per file, so the map says what lives where.
- **write / edit** append `[Edit echo: touched …; heard by …]`: the
  symbols the mutation touched (nearest declaration at or above each
  changed span, via the same `similar` diff that builds the unified
  diff, under a 100ms timeout) and the podmates linking the file.
  The model-facing receipt is one line and the diff rides metadata
  for the TUI, so the echo is how the model learns what its edit hit.

## Languages

Symbol, import, and link sounding covers Rust, Python, JavaScript /
TypeScript, and Go via deliberately small line scanners (see
`crates/tui/src/tools/echolocation.rs` for the documented lookalike
classes). Anything unresolvable is omitted, never guessed; links
require an on-disk target.

## Revision-qualified Rust navigation

The lowercase `read` path uses the already-installed `syn` parser to observe
outer Rust functions and impl methods, including inline modules. A bounded
`metadata.navigation` object identifies its producer (`rust_syntax_v1`), the
workspace-relative path, source-byte SHA-256 revision, availability and omission
counts. Each symbol carries a `read` object: `path`, 1-based line `offset`, line
`limit`, and `expected_revision`. Pass that object directly to `read` to inspect
the captured function window. No separately named navigation tool is required.

Observations come from the same byte snapshot as the read, not another filesystem
read. Syntax is not semantic resolution: macros, active cfg, external modules,
type/name resolution and LSP coordinates are not claimed. Ranges start on the
`fn` keyword's line; preceding attribute/modifier lines and local items are not
included. Invalid UTF-8, parse errors and sources above
256 KiB report `unavailable`, never invented ranges. At most 24 symbols and 4 KiB
of serialized navigation are retained; counts identify omitted observations.

A `[Code navigation: ...]` footer renders whole reference objects under 600B,
sharing the existing 1200B budget with the pod/sounding sections. Long paths can
leave no room for an inline reference; the bounded metadata remains available to
`execute_tools`. Existing scanners still serve other languages and other echo
surfaces. The successful Rust read's symbol sounding consumes these syntax facts.

### Shared facts, separate delivery

`RuntimeToolServices` reuses the existing bounded `HandleStore` implementation in
a **separate, host-owned store**, not the writable program `var_handle` namespace.
Keys bind canonical workspace, relative path and producer; cached values are
reused only after normal path/authority admission and matching the **current byte
revision**. There is no watcher, persistent index, permission bypass or mtime-only
freshness claim. Child runtime clones share the physical store, not a parent's
delivery ledger. Cache eviction simply recomputes the observation on a later read.

An optional `expected_revision` guard rejects stale source before using old
offsets, including a same-size external edit. Without it, ordinary reads behave
normally. A reference is evidence of a range, never authority to access that path.

`WorkingSet` retains at most 32 delivered references **only after the final
projected request is accepted by the model client**. It inspects model-facing
tool-result text, not nested calls or the physical cache. A program can read source
internally and return only `{done: true}` without seeding this ledger; returning a
selected structured reference records just that reference. JSON strings inside
logs/argument previews are not decoded as deliveries. Inspection is conservative
and bounded to 2048 JSON nodes and 16 nesting levels per result.

The session's existing tool-result log retains the same projected reference
objects. Resume/compaction rebuilds use that conversation, requiring a subsequent
assistant message before classifying a historical result as delivered; a trailing
crash-boundary result alone is not proof. This ledger does not claim body readership,
current freshness, complete historical delivery or deduplication of future reads.
Fresh child seeds do not rebuild it from a parent's ledger.

**KV-cache effect:** navigation is tool-result history only, never a volatile
prefix contributor. The optional `read` schema field is static and session-pinned
with the tool catalog. No new system-prompt text is inserted.

## Tuning and ablation

`CODEWHALE_ECHO` gates sections for trials
(`off:<name>`, comma-separated; names: `mates`, `symbols`, `imports`,
`links`, `callers`, `pods`, `soundings`, `navigation`). Unset or malformed means
everything on. The gates are process-pinned and read-only; there is no
per-session or per-call control, by design. The squeeze follows the
same gates, so an `off:links` trial measures links alone.
`off:navigation` suppresses the inline navigation footer only; structured
metadata remains available to programs and still uses the syntax producer.

## Non-goals

- No tool description teaches the footer format: the brackets are
  self-describing, and every description byte on an eager tool taxes
  the session prefix. The diet stops where behavior would degrade.
- No cross-pod callers: reverse links scan same-dir mates only, so the
  cost of a sounding is bounded by one directory listing plus a
  bounded re-read of small mates.
- No `patch` (multi-file) echoes yet; single-file write/edit only.


## Source and scan limits

Callers are inspected from at most 32 sorted visible language-file candidates. Each accepted caller body is at most 256 KiB; one extra byte detects overflow. Unscanned candidates are reported explicitly. Key-file body sounding accepts at most 8 MiB and detects overflow the same way. Reads use the existing protected regular-file handle and reject unsafe file shapes before parsing. Directory name enumeration and platform/kernel read latency remain outside these body caps. Delete attribution uses the old-side range; replacement attribution uses both old and new ranges. These source rules do not qualify Windows behavior or provider cache savings.

Search pod enumeration reuses the depth-one ignore walker: parent ignore files, global rules, repository gitignore and info/exclude are honored when requested. Symlinks are not followed. Include/exclude/extension and deny guards remain applied, and the unlistable-directory fallback only repeats already matched non-dotfile names. Directed reads bypass ignore filtering while retaining denial and protected-read guards.
