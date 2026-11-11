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
  Lowercase tools also return written-snapshot evidence below; the echo
  remains advisory change attribution, not a verified dependency graph.

### Written-snapshot evidence

Lowercase `write` and `edit` return a JSON line containing a `read` object
(`path`, `expected_revision`) and a `diff` status. The revision hashes the exact
bytes accepted by the atomic writer, including line endings and any Rust
formatting, not the model's requested replacement. The object can be passed
directly to lowercase `read`; a later change is refused by the existing guard.

For UTF-8 snapshots, `diff.status = "complete"` includes the same applied text
diff as `metadata.mutation.diff`, only when the **entire diff** fits 2048 bytes.
Larger diffs use `status = "omitted"`, `reason = "inline_byte_limit"` and a byte
count; non-UTF-8 preimages or written snapshots use `reason = "non_utf8_snapshot"`.
No partial hunk is shown. The full text diff remains in TUI metadata, and the
guarded read remains available in every case. The cap covers diff bytes, not
the acknowledgement, JSON evidence or existing edit echo.

Evidence is derived from the mutation's captured inputs and written bytes,
without rereading disk or creating another observation store. Its KV-cache
effect is append-only tool history; the pinned prompt and catalog do not change.
It establishes a written snapshot, not later filesystem freshness, behavioral
correctness or a measured reduction in agent reads.

## Languages

Symbol, import, and link sounding covers Rust, Python, JavaScript /
TypeScript, and Go via deliberately small line scanners (see
`crates/tui/src/tools/echolocation.rs` for the documented lookalike
classes). Anything unresolvable is omitted, never guessed; links
require an on-disk target.

## Revision-qualified Rust navigation

The lowercase `read` path uses the already-installed `syn` parser to observe
outer Rust functions, structs (named-field, tuple and unit), enums, traits, unions,
type aliases, named constants and statics, including declarations in inline
modules. Trait methods (required and default), associated types and constants,
and impl methods/types/constants have their own guarded declaration windows. A bounded
`metadata.navigation` object identifies its producer (`rust_syntax_v1`), the
workspace-relative path, source-byte SHA-256 revision, availability and omission
counts. Each symbol carries a `read` object: `path`, 1-based line `offset`, line
`limit`, and `expected_revision`. Pass that object directly to `read` to inspect
the captured declaration window. No separately named navigation tool is required.

Observations come from the same byte snapshot as the read, not another filesystem
read. Syntax is not semantic resolution: macros, active cfg, external modules,
type/name resolution and LSP coordinates are not claimed. Ranges begin at the
declaration's earliest outer attribute (including doc comments), visibility or
supported modifier, preserving multiline `async`, `const`, safety, ABI and
specialization prefixes. Free-standing preceding comments and local items do
not extend the range. Trait members use `module::Trait::member` syntax labels; impl
members retain `module::impl@LINE::member` labels. These identify syntax scopes,
not a resolved receiver type or a link between a trait and its implementations.
Whole traits remain available alongside their members. Foreign declarations are
not separately emitted, and unnamed `const _` items do not produce named targets.
Invalid UTF-8, parse errors and sources above
256 KiB report `unavailable`, never invented ranges. At most 24 symbols and 4 KiB
of serialized navigation are retained; counts identify omitted observations.

A `[Code navigation: ...]` footer renders whole reference objects under 600B,
sharing the existing 1200B budget with the pod/sounding sections. Long paths can
leave no room for an inline reference; the bounded metadata remains available to
`execute_tools`. Existing scanners still serve other languages and other echo
surfaces. The successful Rust read's symbol sounding consumes these syntax facts
and renders qualified `name:line` labels rather than tagging every declaration
as a function.

Inline references offer only declarations partially displayed after range selection
and byte truncation: a whole-declaration read completes missing source, rather than
repeating a body already shown or suggesting an unrelated early helper. A fully
shown member is not reoffered, though its enclosing trait may still offer missing
context. Gap, empty and whole-file reads offer no such continuation. This checks
only the current response, not bodies delivered in earlier reads.

References continuing beyond the last displayed line take priority over backward-only
boundary fragments. Within each group, completions are ordered by ending line,
then later starting line: an inner member precedes its enclosing trait, while
non-nested declarations retain file order. The same byte budget still applies;
a forward reference that does not fit cannot block a fitting backward completion.
This is geometric continuation, not semantic relevance.

When displayed lines extend beyond an incomplete cached symbol prefix, the
renderer reparses the same admitted source snapshot on the existing blocking
worker. It selects partially displayed declarations before the 24-symbol cap, so
late declarations can appear without widening the source, cache, metadata, or footer
budgets. Candidates that do not fit are counted as omitted without blocking later,
smaller references. Structured metadata and cached observations remain
range-independent; the per-read selection is never cached. Cache hits outside the
retained prefix can therefore incur another bounded parse. A fully displayed file
skips this extra parse: no declaration can extend outside the displayed window.
References still name whole declarations, including large enclosing functions or
traits; no semantic next-step or model-performance improvement is claimed. A read footer
cannot assist discovery before the first source read, and offering a reference
does not establish that an agent followed it.

### One-hop local helper candidates

Focused Rust reads can also offer same-module free functions named by direct
`helper(...)` or `self::helper(...)` call expressions in the displayed source.
Free functions and impl methods can be callers. Each offered symbol labels the
relationship with `call_candidate_from`, and carries the existing revision-guarded
`read`. A small complete helper body is included as `source` only if it fits.
These are syntax candidates, not resolved callees or a dependency graph.

Local helper references take priority over partial-declaration completions. Callers
whose complete declarations are displayed rank first; within each group, the caller
midpoint nearest the read-window midpoint wins, with file order breaking ties.
Ranking precedes the 24-helper candidate cap and shared-helper deduplication, while
supported calls within a caller retain syntax order. This positional heuristic does
not infer query relevance. Optional helper bodies are considered only after
references, so they cannot displace another reference. A candidate that does not fit
is skipped without blocking smaller ones.
Helpers overlapping the read window are not repeated;
whole-file, empty and unavailable-syntax reads add no helper candidates. Collection
is one hop, within the same admitted byte snapshot, with at most 24 distinct
supported call paths per caller and 16 inline-module levels. Duplicate same-name
definitions are omitted. Unqualified calls are conservatively suppressed for
parameter/local pattern bindings or local items;
method calls, other qualified paths, nested items and closure bodies are not
followed. Macro expansion, cfg evaluation and name/type resolution remain outside
this contract.

Selection reparses the bounded snapshot on the existing blocking read worker,
sharing that parse with late partial-declaration selection when needed. It does
not reread disk, retain a relationship index, change cached metadata, or widen
the 600-byte navigation footer. References and optional source enter append-only
tool history, not the pinned prefix. `off:call_candidates` disables this extra
selection for ablation; `off:navigation` continues to hide the whole footer.
This capability does not itself establish agent adoption or a speed improvement.

### Focused Rust search-to-read

An untruncated `grep_files` result with at most 16 hits can now include
`navigation` before its ordinary `matches`. The existing syntax producer selects
only supported declarations intersecting the matched lines, before its symbol cap.
Each symbol names the enclosing scope and supplies the same
`{path, offset, limit, expected_revision}` object accepted by lowercase `read`.
This works before the first read, including for definitions beyond the cached
file-prefix cap. Comment lookalikes do not manufacture declarations. A match
confined to an outer attribute or declaration prefix also offers its owning
declaration, without treating that match as an identifier hit.
Nonempty regex matches overlapping a declaration's identifier token take
precedence over matches elsewhere in its signature or body, both within a file
and across the bounded candidate set. This priority applies while retaining symbols,
not just when sorting them: a later identifier hit can replace the last
non-identifier scope without widening the 24-symbol cap. Equal-priority scopes
retain source order. A call or type use inside a one-line
function therefore does not promote that function's unrelated name. Matching
uses the original admitted line and the existing compiled regex, including its
case and anchor behavior; BOM/shebang prefixes and UTF-8 byte coordinates are
accounted for. Zero-width matches do not establish identifier overlap. This
orders only the added observations, not the raw grep matches. Broad regexes can
still overlap several identifiers; query intent and semantic resolution are
not claimed.

Small complete declaration bodies can also appear as `source` beside their reference:
at most 2048 source bytes per body, only when their JSON fits without removing
references. Larger declarations still offer the guarded read rather than an arbitrary
context window. Raw matches, context, search coverage, and pod visibility are
unchanged. Broad or truncated searches keep their ordinary presentation.

Source is retained from the existing streaming search, not reread. At most 16
matched Rust snapshots, each at most 256 KiB, survive until the walk ends; broad
results discard them without parsing. Only snapshots scanned through EOF can
produce a whole-file revision. Syntax observations, bodies and references together
fit a 4 KiB JSON array; `navigation_files_omitted` reports qualifying matched files
without an observation. Invalid syntax reports `unavailable`; invalid UTF-8 still
follows the search's existing file-skip behavior. This does not resolve types,
macro/cfg expansion, callees or repository-wide definitions, and does not establish
that a model used the evidence or completed its task faster.

These are ordinary logged tool results, not a new tool, repository index,
cache entry, planning call, or prefix contributor. Following a reference still
performs the reader's current access and byte-revision checks.

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

When a source snapshot revision is available, native paging carries it from the
first page, even when the initial read omitted `expected_revision`. Byte-budget
and line-range continuations use the revision already computed from admitted
source bytes. The read defaults to at most 2,000 lines and 50 KiB; only the
operator's `[workshop] read_result_max_bytes` can raise the byte cap, up to 2 MiB.
The model has no `max_bytes` parameter. Lines over 2,000 characters are marked
truncated rather than retried with a model-selected byte budget.

Continuation offsets advance only over complete source lines. A complete blank
line permits advancing; an empty selection or character-truncated line does not
advertise skipping the unread range. Character-truncated windows do not supply
inline navigation bodies or call candidates. Declarations longer than the
reader's 2,000-line maximum are counted as omitted rather than advertising a
reference the reader would refuse.

The read's model-facing budget includes these bounded presentation bytes in
addition to the enforced source-byte budget, so paging and navigation footers do
not cause a second context cut. Whole results preserve source whitespace. The
source-byte maximum is unchanged; independent artifact spilling and explicit
child-output caps can still narrow delivery.

Reads without an available snapshot revision retain their existing paging and
fallback behavior. The guard remains optional on read requests; following a
paging hint does not grant read authority. These hints are appended tool-result
history, not additions to the session-pinned prefix.

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
`links`, `callers`, `pods`, `soundings`, `navigation`, `grep_navigation`,
`call_candidates`). Unset or malformed means
everything on. The gates are process-pinned and read-only; there is no
per-session or per-call control, by design. The squeeze follows the
same gates, so an `off:links` trial measures links alone.
`off:navigation` suppresses the inline navigation footer only; structured
metadata remains available to programs and still uses the syntax producer.
`off:grep_navigation` disables search snapshot retention, syntax analysis, bodies,
and references together, leaving the ordinary grep result for paired trials.


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
