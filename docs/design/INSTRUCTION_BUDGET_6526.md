# Standing instructions: selection, budgets, and visible truncation

Plan for issue #6526 (and the comment at
https://github.com/codewhale-hq/Codewhale/issues/6526#issuecomment-6393297659).

- **Status:** candidate implementation for contributor PR #7086; local verification and
  acceptance are recorded separately. This document does not establish a merge or release.
- **Contributor source:** `fix/instruction-budget-6526` (from `main` at `498dd5a87`,
  2026-10-24); maintainer integration uses canonical `main`.
- **Scope:** standing instruction files (`AGENTS.md`, `CLAUDE.md`, `instructions.md`,
  the top-level `instructions = [...]` array, `rules/`). Skills are a separate issue (#7073).
- **Orientation:** OpenCode's selection model (AGENTS.md wins, otherwise CLAUDE.md) with
  codewhale's own sources preserved; no content truncation while a source is within budget.

## 1. Historical problem

This section records the pre-change baseline used for the contributor proposal. Its file
locations and measurements are historical, not current source receipts.

Before this change, codewhale has four instruction sources, three ceilings, and two different trim
directions — none of them visible to the operator.

### 1.1 Global layer

`load_global_agents_context` tries six candidates in order, first existing wins, and falls
through to the next candidate on a read error (`crates/tui/src/project_context.rs:1000-1033`):

1. `~/.codewhale/AGENTS.md`
2. `~/.agents/AGENTS.md`
3. `~/.deepseek/AGENTS.md` (legacy)
4. `~/.codewhale/instructions.md`
5. `~/.agents/instructions.md`
6. `~/.deepseek/instructions.md`

`~/.claude/CLAUDE.md` is not read today.

### 1.2 Project layer

Per directory, root → workspace:

- `AGENTS.md` (canonical);
- `.agents/AGENTS.md` (always, as a cross-agent fragment, `:196-208`);
- `.codewhale/instructions.md`;
- `.deepseek/instructions.md` (legacy);
- `CLAUDE.md` and other foreign formats only when `project_instruction_imports` enables them
  (`crates/tui/src/config.rs:2346-2352`, empty by default);
- `WHALE.md` is ignored and reported as a migration diagnostic.

### 1.3 Rules

`.codewhale/rules/*.md` and `.claude/rules/*.md` (`project_context.rs:341`), plus foreign rules
directories when imports are enabled (`:254-265`). `MAX_RULES_FILES = 50` per directory
(`:390`) silently drops the rest with a `tracing::warn!` only (`:1144-1155`).

### 1.4 Configured extra files

The top-level `instructions = [...]` array (`crates/tui/src/config.rs:2327`, user config only;
the project overlay ignores it). Each entry is capped at 100 KiB and trimmed with
`[…truncated: N of M bytes omitted — …]` (`crates/tui/src/prompts.rs:113`, `:154-164`,
`:464-540`). This is not a `[context]` key: `ContextConfig` only has `project_pack`
(`config.rs:1675`) and deliberately ignores unknown fields (#6516).

### 1.5 Ceilings and trim directions

- Aggregate `MAX_PROJECT_INSTRUCTION_BYTES = 48 * 1024` (`project_context.rs:386`), shared by
  instructions and rules.
- Per-file `MAX_CONTEXT_SIZE = 100 * 1024` (`:368`): a larger file is skipped (`TooLarge`,
  `:1070-1075`); a whitespace-only file is skipped as `Empty` (`:1078-1082`).
- Instructions are trimmed with `keep_tail_within` (`:397-410`): keep the tail, drop the
  front (broadest scope first), marker `[…broader-scope project instructions dropped at the
  aggregate budget…]`.
- Rules are trimmed with `keep_head_within` (`:413-426`): keep the head, drop the tail (the
  last rule files), marker `[…rules block truncated at the aggregate budget…]` (`:456-470`).
- The global text is merged into the project text (`merge_global_and_project_instructions`,
  `:985-999`) before `enforce_project_instruction_budget` runs (`:552`, `:749`). Per-scope
  budgets require a pipeline change: keep the scopes separate until each is measured and
  trimmed, then merge.

### 1.6 Visibility

`ProjectContext.warnings` appears only in `/context report`
(`crates/tui/src/context_report.rs:560-571`) and the Runtime API source map. Nothing is shown
at session start; the model sees inline markers without sizes or file names.

### 1.7 Measured on a real setup (approximate; markers and labels excluded)

Global `~/.agents/AGENTS.md` = 71,522 B, project `AGENTS.md` = 37,571 B. Assembled 109,093 B;
the 48 KiB tail keeps the project and about 11.3 KiB (16%) of the global; about 58.5 KiB are
dropped from the top. A 26 KiB project leaves ~22.0 KiB (31%) of the global. A 120 KiB project
is skipped by the per-file ceiling and the global is still cut to 49,152 B.

### 1.8 Data model and cache today

`enforce_project_instruction_budget` sees one flattened `String` (`ctx.instructions`) plus
`ctx.rules_block`; the per-file segments (`ancestor_docs: Vec<(PathBuf, String)>`) are local to
the loader and gone by trim time. The project-context cache
(`crates/tui/src/project_context_cache.rs`, wrapper
`load_project_context_with_parents_cached_and_home`) is keyed on files, not on the context
window. Both facts constrain the design: segment-level trimming needs segments kept on
`ProjectContext`, and window-derived budgets must not be baked into a file-keyed cache.

## 2. Historical comparison

### Claude Code (`code.claude.com/docs/en/memory`)

- Load order, broadest → most specific: managed policy → `~/.claude/CLAUDE.md` → project
  (`./CLAUDE.md` or `./.claude/CLAUDE.md`, every ancestor) → `./CLAUDE.local.md`.
- All discovered files are concatenated, order root → cwd; **no size cap, no truncation**.
- `AGENTS.md` is read only when no `CLAUDE.md`/`.claude/CLAUDE.md`/`CLAUDE.local.md` exists in
  cwd or above; a setting can switch to both / CLAUDE-only / managed-only.
- Imports `@path` up to 4 hops; `.claude/rules/` and path-scoped rules.
- Only explicit limit: auto memory, first 200 lines or 25 KB.

### OpenCode (`opencode.ai/docs/rules`, source `anomalyco/opencode@dev`)

- Per category: local `AGENTS.md` beats `CLAUDE.md`; global
  `~/.config/opencode/AGENTS.md` beats `~/.claude/CLAUDE.md`.
- `instructions` in `opencode.json` adds paths, globs, and remote URLs (5 s timeout), combined
  with `AGENTS.md`.
- **No content truncation.** `config/plugin/instruction.ts` reads global + ancestor
  `AGENTS.md` + configured sources; `session/instructions.ts` injects each file whole as a
  synthetic message `Instructions from: <path>\n<content>`.
- Order: global → project (nearest → farthest) → configured.

### codewhale before this change

Concatenate global + project + rules, then trim to 48 KiB: instructions keep the tail, rules
keep the head, global is discarded first. The only one of the three that cuts content.

## 3. Decisions

### 3.1 Selection

- **Project, per directory:** `AGENTS.md` wins; if absent, `CLAUDE.md` is read only as an
  opt-in fallback (OpenCode parity), enabled with `claude_fallback = true` — the default is
  `false`, because importing another tool's instructions is never silent. `CLAUDE.md` is also
  read alongside `AGENTS.md` when `project_instruction_imports` includes `claude`.

  | AGENTS.md | CLAUDE.md | `claude` import | `claude_fallback` | read |
  | --- | --- | --- | --- | --- |
  | loaded | absent | any | any | AGENTS.md |
  | loaded | loaded | no | any | AGENTS.md |
  | loaded | loaded | yes | any | both |
  | absent/skipped | loaded | yes | any | CLAUDE.md (explicit import outranks the fallback flag) |
  | absent/skipped | loaded | no | true (opt-in) | CLAUDE.md |
  | absent/skipped | loaded | no | false (default) | nothing |
  | absent/skipped | absent/skipped | any | any | nothing |

  `CLAUDE.md` here means `CLAUDE.md` or `.claude/CLAUDE.md` in the directory;
  `CLAUDE.local.md` is Claude-specific and is not imported.

  **Skip counts as absent:** if the winning `AGENTS.md` is too large or empty, it is treated
  as absent and the next candidate (CLAUDE.md) is tried; the notice names both the skipped
  file and the fallback selected. The same rule applies to the global chain: a skipped
  candidate falls through to the next one. An **empty project scope after a symlink dedupe**
  is not "absent": the content is already loaded as global, so no fallback runs; the notice
  says `duplicate`.

- **Global:** the six existing candidates in order, then `~/.claude/CLAUDE.md` as the last
  resort when none of the six loads and either `claude_fallback = true` or the explicit
  `claude` import is enabled. Foreign files never outrank codewhale's own.
- **All ancestor directories** are read, root → workspace.
- **Symlinks:** project files are opened through `fs_confined::open_read`, which refuses a
  symlink below the scope root; global and configured files use `open_user_read`, which
  keeps intentional links and rejects anything that is not a regular file. Duplicates are
  detected by canonical path and loaded once. A project file symlinked to the global file is
  counted as global; the project duplicate is reported, not counted twice.
- `.agents/AGENTS.md` is always a fragment; it is not deduped against a same-directory
  `AGENTS.md` unless the canonical path is identical.

### 3.2 Budgets

- Budgets are in **tokens**. Files and the per-file ceiling are in bytes; conversion uses the
  engine's token estimator on the assembled text, the same estimator the context report uses.
  No `bytes/4` shortcut (it underestimates Cyrillic by 30–50%).
- **Usable window** = `context_window − planned_output_reservation − safety_reserve`
  (`safety_reserve` = 10% of the window, minimum 2,048 tokens). The reservation is the
  planned output cap the request will actually carry: effort normalized with
  `resolve_auto_effort`, then `effective_max_output_tokens_for_turn`, intersected with the
  client cap when the turn carries an explicit allowance. It is not the later,
  input-dependent admitted cap, and never a goal token budget. A route whose window is
  unknown has no usable window: relative ceilings and the guard are reported as unknown and
  only the absolute scope caps apply. All percentages are of the usable window, not the raw
  window.
- Per-scope budgets, independent:
  `effective = min(absolute_tokens, percent × usable_window)`.
  Defaults: **100,000 tokens** and **25%** per scope. All values are positive; `0` is invalid
  (a finite budget is the point). `percent` is 1–100.
- **Scope assignment:** global scope = the six global candidates + `~/.claude/CLAUDE.md` + the
  configured `instructions = [...]` entries (user-level authority). Project scope = the
  per-directory chain + `CLAUDE.md` + rules. Configured entries are therefore global,
  not project; rules are project. The separately rendered additional fragments retain
  their existing bounded-fragment cap and are outside these two scope budgets (§10).
- **Static-prefix guard:** the measured prefix is every system block (base prompt, tool-use
  policy, environment, instructions, skills index, memory, goals, inline authority) plus the
  tool schemas the request carries. It must stay under `static_prefix_guard_percent`
  (default 80% of the usable window). Only the two instruction scopes can be trimmed to
  satisfy it. The guard is the safety net for non-instruction static content; it is not a
  substitute for a total-instructions key.
- **No separate "total instructions" key.** A key equal to the sum of the two scope caps
  (50%) would be inert; a smaller one would contradict the per-scope decision. The guard
  covers the actual risk.
- Worked examples with a 64K (65,536) output reservation:

  | window | usable | per scope | both scopes | guard 80% |
  | --- | --- | --- | --- | --- |
  | 128,000 | 49,664 | 12,416 | 24,832 | 39,731 |
  | 1,000,000 | 834,464 | 100,000 (absolute cap) | 200,000 | 667,571 |

  On a large window the absolute cap wins: 100,000 tokens is 10% of a 1M window per scope.
  A small window is best-effort. The guard trims instructions only until the prefix fits; it
  is unsatisfiable when non-instruction content alone exceeds the guard, in which case both
  scopes reach zero and the request proceeds under `policy = trim` or stops under
  `policy = refuse`.

### 3.3 Per-file ceiling

- `file_max_bytes = 8 MiB` is a memory-safety limit, not truncation, and the configured
  value can only lower it (validation rejects anything above 8 MiB). The read is bounded on
  the open handle (`cap + 1` bytes, regular files only), not by trusting `metadata()`, so a
  FIFO, a device, or a file that grows mid-read cannot become an unbounded read. A larger
  file is `skipped`, with a notice naming the file and the limit. There is no
  `0`/unlimited value: a hard ceiling must always exist.
- A file that fits the ceiling still retains at most its first 100 KiB of text (a UTF-8
  boundary head) followed by a marker naming the bytes kept and dropped, with a structured
  `file_content` notice. This applies to instruction, rules, and configured files at
  assembly; the structured constitution JSON reader keeps its own complete-document read.
- A whitespace-only file is `skipped (empty)`.
- `MAX_RULES_FILES = 50` stays as a safety cap, but its overflow is reported as
  `skipped (rules cap)` in the notice and `/context`; it is no longer silent.
- Because retention is capped at 100 KiB per file, tokenization never sees an 8 MiB file;
  the scope budget trims what remains.

### 3.4 Trim semantics

- `ProjectContext` keeps `instruction_segments: Vec<(PathBuf, String)>` (scope, path, text) in
  addition to the joined string. Per-scope trimming operates on segments; the joined string is
  rebuilt after trimming.
- Each scope is trimmed only against its own budget; the other scope is untouched.
- Within the project scope, drop whole chain segments from the broadest end first (root-most
  ancestors go first, the workspace file last). If a single file alone exceeds the remaining
  budget, keep its head, drop its tail with `keep_head_within`, and insert a marker with
  bytes/tokens. This reverses today's single-file direction (today `keep_tail_within` keeps
  the tail); the change is listed in §9. `keep_tail_within` is no longer used for instructions.
- Within the global scope there is normally one file; the same single-file rule applies.
- Rules are charged to the project scope after project instructions; they keep the current
  head-first trim (rules are a flat filename-sorted list, with no nearest-scope hierarchy to
  preserve), and the marker and notice carry the numbers.
- A scope is trimmed once against its own ceiling. The static-prefix guard then makes one
  bounded pass over both scopes, largest first, lowering a scope's ceiling by the measured
  overage and re-measuring, with at most two attempts per scope. A ceiling of zero is
  installed like any other and removes the scope. There is no convergence loop; a prefix
  still over the guard afterwards is reported as unsatisfied.
- The marker is included in the budget (as today: `max − marker_len`). The cut point is found
  by binary search on UTF-8 boundaries so the result is deterministic.
- If trimming leaves zero source bytes (marker excluded), report `skipped (budget)`, not
  `trimmed`. This is reachable when the budget is smaller than the marker or when the static
  guard trims a scope to zero.
- If the static guard trips, the pass above narrows the installed instruction block in
  place from the captured raw segments; nothing is read from disk again. If both scopes
  reach zero and the guard is still exceeded, report the residual excess and continue
  (policy `trim`) or stop before dispatch (policy `refuse`).
- A per-turn `<context_update>` for an unchanged explicit context is composed under the
  installed (guard-narrowed) ceilings, so it can never re-append text the guard dropped. An
  explicit route, model, or header change starts again from the base budgets. A changed
  output reservation (including effort) reapplies budgets at the guard without declaring a
  header change by itself.

### 3.5 Policy

- Default: `trim` + a reported loss. `refuse` remains an opt-in value for operators who
  prefer a hard stop. `refuse` stops the request before dispatch on **trim** losses (scope
  budget, rules budget, per-file 100 KiB retention) and when the request prefix exceeds the
  guard; **skips** (file ceiling, empty file, rules cap) remain skips and are reported,
  because a single bad file must not make the project unopenable. The check covers the real
  system blocks and tool schemas of the request, including inline and dynamic authority.
- The `warn` value is removed; "warn" is what `trim` does.
- Validation rejects unknown policy values, non-positive budgets, percents outside 1–100, a
  guard outside 1–100, and any config where `guard − (global_percent + project_percent) < 10`;
  the remaining 10 points are the minimum room for non-instruction static content.

### 3.6 Notice contract

| Trigger | Action | Report | Model |
| --- | --- | --- | --- |
| file > `file_max_bytes` | skip | `/context report` | first-turn synthetic |
| empty file | skip | `/context report` | first-turn synthetic |
| rules cap 50 | skip files | `/context report` | first-turn synthetic |
| duplicate via symlink | skip duplicate | `/context report` | first-turn synthetic |
| scope > budget, `trim` | trim that scope | `/context report` + numbers | first-turn synthetic + marker |
| zero source bytes left | skip | "skipped", not "trimmed" | same |
| static guard, `trim` | trim largest scope by `over` | `/context report` | first-turn synthetic |
| static guard unsatisfiable | proceed (trim) / stop (refuse) | named row | first-turn synthetic |
| `refuse` policy on trim | stop before dispatch | named error | request not sent |
| all within budgets | no notice | no | no |

Fields: `path`, `scope` (`global`/`project`), `source` (`agents`/`claude`/`instructions`/
`configured`/`rules`), `action` (`kept`/`trimmed`/`skipped`/`refused`), `bytes_kept`,
`bytes_dropped`, `tokens_kept` (estimated), `tokens_dropped`, `limit_kind` (`scope_budget`/
`static_guard`/`file_ceiling`/`empty_file`/`rules_cap`/`rules_budget`/`duplicate_path`), and
`fallback_selected` when a skipped file was replaced by a lower-precedence candidate.

Channels:

- **Model and transcript:** a runtime-provenance synthetic message delivered once from turn
  preparation whenever the rendered loss list changes (see §10). It is part of the logged
  transcript, and its prose comes from the locale pack for the session locale; the scope,
  origin, action, and limit tokens stay machine keys.
- **`/context report`:** per-source action and budget from the installed receipt. The
  headless `doctor` report is labelled a preview: it is built from current files and
  config, carries no installed receipt, and does not claim the tool-schema guard accepted a
  request.
- **Session snapshot and protocol:** `receipt_json()` is a compact projection that omits
  block text; the typed receipt is held only by the Session and the App.

## 4. Config surface

Top-level `instructions = [...]` stays as is (array). New table **`[instruction_budget]`**
(not `[instructions]`, which would collide with the array; not `[context]`, which ignores
unknown keys and would hide typos):

```toml
[instruction_budget]
global_budget_tokens = 100000     # positive
project_budget_tokens = 100000    # positive
global_budget_percent = 25        # 1..=100
project_budget_percent = 25       # 1..=100
static_prefix_guard_percent = 80  # 1..=100
file_max_bytes = 8388608          # hard ceiling, positive
policy = "trim"                   # trim | refuse
claude_fallback = false           # opt in: AGENTS.md wins; otherwise CLAUDE.md
```

- `deny_unknown_fields` on this table so a typo inside it fails loud.
- The **project overlay ignores this table** (like `instructions`), so a cloned repository
  cannot shrink the user's global budget or force `refuse`. The overlay/base merge happens at
  `config.rs:10625-10628`; the new table follows the `instructions` rule there (user config
  only).
- Because the top-level config is tolerant of unknown tables (#6516), a misspelled table name
  (`[instruction_budjet]`) would be ignored; `codewhale doctor` inspects the raw TOML before
  deserialization and warns about unknown `instruction*` tables and keys.

## 5. Historical implementation plan

This is the contributor's original work breakdown, retained for provenance. The current
contract is §3 and the reconciled implementation is §10. Proposed test names and mechanisms
below are not evidence that a test exists, passed, or shipped.

1. **New assembly order** in `crates/tui/src/project_context.rs`:
   `assemble_instruction_scopes(raw: &ProjectContext, configured: &[PathBuf], budgets:
   &InstructionBudgets)` reads configured entries fresh (they are user config and are not in
   the cache key), joins them to the global scope, applies per-scope trims, then merges with
   labels. Imports are already baked into the raw cache; budgets are passed separately. The
   new function owns the split; the configured entries move out of
   `prompts.rs::render_instructions_block`, so there is exactly one owner and one charge
   (global).
2. **Cache**: `project_context_cache` keeps **raw segments** keyed by the existing file
   candidate set (`project_context_cache_candidate_paths`, `project_context.rs:754-800`).
   `apply_instruction_budgets` runs after cache lookup, per request/window; configured
   `instructions` entries are read after cache lookup, so a change there is picked up without
   cache invalidation; `InstructionSource::Inline` stays outside the cache. A window change
   must not invalidate the content cache, and a content change must not reuse trimmed text.
3. **`crates/tui/src/project_context.rs`**
   - `ProjectContext` gains `instruction_segments: Vec<InstructionSegment>` (scope, path,
     text); the joined string is rebuilt after trimming.
   - Delete both `enforce_project_instruction_budget` call sites (`:552`, `:749`) and replace
     them with `apply_instruction_budgets`, called by the assembly after cache lookup.
   - `load_global_agents_context`: add `~/.claude/CLAUDE.md` as the last candidate; keep the
     six existing candidates and fall-through; report the selected fallback.
   - `load_dir_instructions` / `active_fragment_candidates`: implement the truth table from
     §3.1 (skip-as-absent, explicit import co-reads, symlink dedupe).
   - `apply_instruction_budgets`: per-scope budgets; segments dropped whole; `keep_head_within`
     for a single oversized file and the rules block; `keep_tail_within` no longer used for
     instructions.
   - `MAX_RULES_FILES` overflow → structured warning, not only `tracing::warn!`.
   - Module-level known-limitations note (repo rule) describing the estimator and trim order.
4. **`crates/tui/src/prompts.rs`**
   - `INSTRUCTIONS_FILE_MAX_BYTES` from `file_max_bytes`; split the relay-artifact cap from
     the instruction cap so `policy = refuse` does not change relay behaviour.
     `render_instructions_block` renders only the assembled instruction block (no configured
     entries).
5. **Static guard**: `prompts::measure_static_prefix` counts every system block plus the
   tool schemas, and `prompts::guard_instruction_prefix` applies the bounded two-scope pass
   (§3.4) from turn preparation, once the pinned prefix and the real tool catalog are known
   together. Preview runs the same pure decision without installing a receipt.
6. **Startup notice**
   - Deliver the loss list to the model as one runtime-provenance message and show it in
     `/context report`; do not reuse `startup_notice` (auto-resume status line).
7. **`crates/tui/src/context_report.rs`**
   - Show per-source action, bytes/tokens, budget and limit kind.
8. **Docs and registry**
   - `docs/CONFIGURATION.md`, `docs/zh_hans/CONFIGURATION.md` (mirror, or an explicit deferral
     note with the sync date), `config.example.toml` (replace the fixed 48 KiB comment),
     `CHANGELOG.md`, and a `docs/features.toml` row with id/name/summary/status/surfaces/
     since/flag/docs/owner (the registry test fails on drift).
9. **Proposed tests** (historical names; existing tests made wrong by the final contract
   are updated or removed):
   - `agents_md_wins_over_claude_md`
   - `claude_md_is_default_fallback`
   - `claude_fallback_can_be_disabled`
   - `claude_import_reads_when_agents_absent_and_fallback_disabled`
   - `explicit_claude_import_reads_both`
   - `agents_md_skipped_falls_back_to_claude`
   - `global_claude_is_last_resort_after_instructions_md`
   - `symlinked_duplicate_is_loaded_once` (canonical path; no fallback)
   - `no_trim_within_scope_budget`
   - `per_scope_budgets_are_independent`
   - `trim_keeps_the_nearest_scope` (segments)
   - `single_oversized_file_keeps_the_head`
   - `zero_bytes_left_is_reported_as_skipped`
   - `rules_share_the_project_scope_budget`
   - `rules_block_is_head_trimmed_with_numbers`
   - `static_guard_trims_by_the_overage`
   - `static_guard_unsatisfied_terminates`
   - `guard_trim_begins_at_residual`
   - `unsatisfiable_only_above_guard`
   - `static_prefix_guard_measures_tool_schemas`
   - `stable_prefix_guard_ignores_memory_growth`
   - `cache_returns_untrimmed_segments_across_budgets`
   - `configured_instructions_are_read_fresh`
   - `inline_instructions_stay_outside_cache`
   - `relay_artifact_cap_unchanged`
   - `refuse_policy_stops_on_trim_but_not_on_skip`
   - `file_over_ceiling_is_skipped_not_trimmed`
   - `file_max_bytes_zero_is_rejected`
   - `empty_file_is_skipped`
   - `rules_cap_is_reported`
   - `unknown_window_uses_128k`
   - `percent_and_guard_bounds_are_validated`
   - `project_overlay_cannot_set_global_budget`
   - `instructions_array_counts_toward_global`
   - `startup_notice_is_a_system_cell_and_stderr`
   - `model_notice_is_a_session_event`
   - `resumed_session_reemits_notice`
   - `context_report_shows_actions_and_budgets`
   - update the existing 48 KiB tests to the new defaults.

## 6. Acceptance criteria

- Default behaviour: no content is trimmed while a source is within its scope budget; the
  only losses are scope-budget trims, static-guard trims, and skips (file ceiling, empty,
  rules cap) — each with a notice.
- `AGENTS.md` selection and the CLAUDE fallback match the truth table, including
  skip-as-absent and `claude_fallback = false`; `project_instruction_imports` still works for
  co-reading.
- Global and project budgets are independent; a huge project does not shrink the global; rules
  are charged to project; configured `instructions` entries are charged to global.
- Any loss (`trimmed`/`skipped`/`refused`) is reported to the model and in `/context`, with path, scope, source, bytes, tokens and limit kind.
- No silent drop remains: rules cap, empty files, fall-through substitution, symlink
  duplicates and per-file skips are all in the contract; the guard has a terminal
  unsatisfiable branch and trims by a defined overage.
- Focused existing checks must pass with positive test counts; measured fixtures and
  conformance goldens must match current behavior. Synthetic manual observations must cover
  the changed trust boundaries and Engine lifecycle. A test-name list is not acceptance.

## 7. Risks

- **Prompt-cache and cost:** a larger static prefix changes the cached prefix and increases
  cost per request; the PR must include a measured estimate.
- **Larger defaults:** the approved default is 100,000 tokens per scope, capped at 25% of
  usable context. The 100 KiB retention limit still applies to each file.
- **Foreign instructions:** a repository containing only `CLAUDE.md` contributes no foreign
  instructions by default. The operator must enable fallback or the explicit import.
- **Small windows:** the actual output reservation and safety reserve determine the residual;
  a fixed nominal window alone cannot establish the guard threshold. Measure the real
  assembled system blocks and tool catalog.
- **Estimator drift:** tokens are estimated, not counted; the notice must say "estimated"
  where it matters.
- **Refactor size:** moving configured `instructions` into the assembly and keeping segments
  on `ProjectContext` touches the loader, cache and prompt builder; the PR should be split
  into a behaviour-preserving refactor and the budget change if maintainers prefer.

## 8. Out of scope

- Skills index budgets and truncation (#7073).
- Skill installation/disable policy (#7066).
- Compaction engine changes.
- Moving instructions from the system prompt into a synthetic message (decided against for
  this PR).

## 9. PR framing

- Reference OpenCode's selection model with source links (`opencode.ai/docs/rules`,
  `anomalyco/opencode` `config/plugin/instruction.ts`, `session/instructions.ts`) and Claude
  Code's no-truncation model (`code.claude.com/docs/en/memory`).
- Include the measured evidence from §1.7, the effective-budget table from §3.2, and the
  cost/cache estimate.
- State the behaviour changes (CLAUDE fallback; 48 KiB → per-scope budgets; a single
  oversized file now keeps its head instead of its tail; configured `instructions` entries
  move from `prompts.rs` rendering into the instruction assembly; the guard covers every
  system block and the real tool schemas) and the opt-in migration path
  (`claude_fallback = true` or explicit import, `[instruction_budget]`).
- Use `Refs #6526`, not `fix`/`close`. Maintainers land verified slices on `main`; this branch
  is a contributor branch, not a maintainer PR branch.

## 10. Candidate implementation notes

What the candidate implements, and where it differs from the historical plan above. The differences are
places where the plan cited a tree that has since changed, or where the plan's
mechanism did not exist; none of them changes the behaviour contract in §3.

- **Where the code lives.** Per-scope budgets, segments, notices, trim
  markers, and the static-guard decision are in
  `crates/tui/src/project_context/budget.rs`. Loader selection and the
  assembly are in `crates/tui/src/project_context.rs`; `types.rs` carries the
  segments and notices on `ProjectContext`. Existing tests live in `project_context/instruction_budget_tests.rs` (selection, skips,
  trims, rules, configured entries, `refuse`), `budget.rs` (budget math and the
  guard decision), `prompts.rs` (stable-prefix measurement, configured-entry
  rendering), `lib.rs` (project overlay) and `config.rs`.
- **The "first-turn synthetic message" mechanism** named in §3.6 does not exist
  in this tree (`crates/tui/src/core/agent.rs` and `announce_active_skill` are
  gone). The equivalent is the runtime-provenance synthetic message the LSP
  diagnostics path already uses
  (`runtime_text_message_with_turn_metadata(..., UserInputProvenance::Runtime)`),
  delivered once from turn preparation. It is part of the transcript, so it is
  logged (model-visible ⇒ logged) and a resumed session already has it in
  history — that replaces the separate persisted event.
- **The static guard** runs in turn preparation rather than
  `core/engine/preview.rs`, because that is the first point where the pinned
  prefix and the real tool catalog are known together. It measures every
  system block plus the tool schemas (`prompts::measure_static_prefix`) and
  runs the bounded two-scope pass of §3.4, replacing only the verified byte
  range of the installed instruction block. Preview and RLM run the same pure
  decision; host-owned prompts (child, isolated, RLM) are measured and can be
  refused but are never trimmed.
- **`policy = "refuse"`** is enforced by the Engine before dispatch, using the
  actual output reservation and the actual tool catalog of the request, so it
  applies to the TUI, `exec`, ACP, and app-server alike.

- **Symlinks.** `fs_confined::open_read` refuses every symlink below the scope root, so a
  project file linked to the user's global file is now recognised as a
  **duplicate** before the read (the content is already loaded from the wider
  scope), and any other symlinked instruction file is *skipped with a notice*
  rather than followed. That refusal is pre-existing sandbox behaviour; this
  change only makes the duplicate case honest instead of an unreadable-file
  error.
- **Foreign fragments** (`.cursorrules`, `.clinerules`, …) keep their own
  bounded-fragment cap and are not charged to the project scope; they are
  reported in `/context` and named as a known limitation in `budget.rs`.
- **Engine-owned settings and receipt.** The budgets, foreign imports, and
  home directory are an explicit immutable `InstructionSettings` captured from
  the applied config when the Engine is built; there is no process-global
  budget or import setter. The Engine captures the raw instruction context,
  composes the block, and installs the typed `InstructionAssembly` receipt
  together with the prompt. The loader cache key includes the settings, and an
  entry is stored only when its pre- and post-load signatures agree. Signatures include
  canonical identity and symlink components as well as bounded content fingerprints.
  Configured entries are read when capturing raw context; guard passes and installed
  reports reuse that capture. Global labels are home-relative and configured absolute
  file labels use the basename; full diagnostic paths stay in receipts. The installed
  instruction range includes its leading separator, so removing the final scope removes
  the separator too.
- **Channels.** The loss list reaches the model as one runtime-provenance
  transcript message and is shown per source in `/context report`. The old
  TUI-cell and `exec` stderr startup lines were removed with the independent
  startup checks.
- **`/context report`** rows come from the installed receipt in the TUI. The
  headless report builds a preview from current files and config, resolves the
  window and output reservation through the same route helpers, and labels the
  budget row as a preview. Facts that are unknown print `unknown`, never a
  guessed window.

A history-only workspace update is assembled under the installed effective scope
ceilings and checked for instruction loss before dispatch. Its notice is logged
without replacing the installed prefix receipt. Preview observes this read-only;
when a workspace update or new notice is pending, the body is unavailable until
the turn establishes that history. It does not advance the delta baseline or
write prepared native-memory receipts. Host-owned prompts retain their authority;
residual static-guard diagnostics do not require a root instruction receipt.
