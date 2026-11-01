# Compaction survival contract

Schema version 2.

The protected history means the current user instructions and a recent
conversation tail selected by token budget, including assistant output.
The tail receives one fifth of the compaction threshold remaining after the
fixed system and tool prefix, capped at 20,000 estimated tokens. Provider
usage calibration applies to both that allowance and the request guard.
One complete exchange can exceed the allowance; pairing takes precedence.
Older completed steps within a long uninterrupted task may be summarized.
Tool-call batches and their results are never split, and unresolved calls
prevent a split across them. Original history is saved before any rewrite.

Language-invariant schema for session-tree journal entry types. Compaction
may summarize older turns; it must not drop the fields marked **survive**
below. The B1 strategy (Rust `codewhale-tui` compaction path) enforces this
over the live journal / API transcript.

Failed compact must not replace live history. A second compact must replace
the prior summary instead of stacking it, and must not grow the cacheable
prefix by duplicating checkpoints. Internal protocol placeholders
(`(no summary available)`, refusal-only text) are never durable transcript
content.

Live repository, tool, and GitHub state is distinguished from summarized
facts: background-work handles and the exact branch/worktree identity
survive here or are re-read from live state before consequential work.

## Entry types

Journal kinds are the protocol `kind` strings (`header`, `user`,
`assistant`, `tool_result`, `compaction`, `branch_summary`). Session-tree
`message` / `system` entries carry the same payload on `SessionEntryKind`.

| kind | payload location | survive | summarized | pruned | not retained |
| --- | --- | --- | --- | --- | --- |
| `header` | `payload` identity, `id`, `created_at` | always | — | — | secrets |
| `user` | `payload` text / `SessionEntryKind::User.text` / `Message` text blocks | last-round verbatim (bounded token budget for older user text) | older user turns | obsolete narration | secrets, raw dumps |
| `assistant` | `payload` text / `SessionEntryKind::Assistant.text` / `Message` text + `tool_use` | protected last-round verbatim | older assistant text and reasoning | — | internal placeholders |
| `tool_result` | `payload.tool_use_id`, `content`, `is_error`, `content_blocks` | last-round bounded (8KiB) | older results | nested images, oversized blocks | secrets, raw large outputs |
| `compaction` | `payload.summary`, `tokens_before`, `tokens_after`, `model`, coverage receipt | latest checkpoint only | prior summaries replaced | stacked prior summaries | placeholder-only summaries |
| `branch_summary` | `payload.branch_id`, `summary`, `parent_branch_id` | always (not rewritten by compact) | — | — | — |
| `message` | flattened `Message.role` + `Message.content[]` | see content-block table | older turns | see prune rules | see never-durable |
| `system` | `SessionEntryKind::System.content` | constitution / repo-law identity when present | duplicated policy | — | unsupported completion claims |

## Content blocks on `message` entries

| block `type` | fields that must survive | bound / notes |
| --- | --- | --- |
| `text` | `text` on the last user round | older user text may truncate to the retained-user token budget |
| `tool_use` | `id`, `name`, `input`, `thought_signature` | last-round only; `id` is the join key for `tool_result` |
| `tool_result` | `tool_use_id`, `content`, `is_error` | last-round; `content` truncated at 8KiB, `content_blocks` dropped if truncated |
| `thinking` | `thinking`, `signature` (byte-for-byte) | all retained reasoning is replay protocol; older exchanges may be summarized as complete units |
| `image_url` | last-round images | older images may be pruned with tool results |
| `server_tool_use` / `*_tool_result` | last-round ids | older blocks may be summarized |

## Fields restated from live state (not the summary)

These must remain usable after compact. They live outside free-form summary
text; if the journal copy is missing they are re-read before consequential
work.

| field | where it lives | after compact |
| --- | --- | --- |
| `/anchor` pin | `.codewhale/anchors.md` (legacy `.deepseek/anchors.md`) | restated verbatim on the checkpoint user message |
| compaction path + coverage | `CompactionCoverage` on the result / `/context` inspector | named as `summary` or `prune-only` plus last-round counts |
| compaction receipt | checkpoint `user` message whose text contains the summary marker; TUI `HistoryCell::System` | latest receipt kept; failed compact does not write a replacement |
| billed parent prompt tokens | `TurnContext.latest_parent_input_tokens` / session-carried copy | survive the turn boundary; cleared on history rewrite |
| branch / worktree identity | live git / workspace | re-read from live state |
| background-work / WorkRef handles | session + worker registry | survive or re-read from live state |
| child usage / cumulative tool steps | turn billing totals | must not create false compaction pressure |

## Enforcement

Rust now:

- `last_round::build_replacement_history` keeps the bounded selected tail,
  current user instructions, older user text within its configured budget,
  and one checkpoint receipt. The summary request contains the older selected
  span plus previous checkpoints, so retained messages do not need to be
  paraphrased and prior state is carried forward.
- `validate_retained_coverage` uses the same selection and refuses the rewrite
  if its user texts, tool-call ids, tool-result ids, or assistant texts would vanish.
  Every user turn in the round is checked, not just the first: the round spans
  a tool-bearing turn plus the toolless tail after it, so checking one would
  let the latest turn -- the one this contract exists for -- be dropped. The
  assistant check compares text, because "some assistant message survived" is
  satisfied by the summary the rewrite itself just wrote.
- `validate_survival_contract` also refuses a missing checkpoint receipt,
  duplicated prior summaries, dropped `/anchor` text, or a placeholder-only
  checkpoint.
- Checkpoint recognition in the recompaction family is era-scoped. A history
  that already carries a provenance-stamped carrier (header block plus the
  provenance block) recognizes only stamped carriers as checkpoints, so a
  user turn that pastes a bare legacy summary header stays content: it
  anchors the round, is retained verbatim, and does not count as a duplicated
  summary. A history without a stamped carrier (a legacy session, or any
  session not yet compacted) keeps the loose legacy matching, so a pasted
  legacy header still reads as a checkpoint at a session's first compaction. The receipt count reads the
  replacement in its own era, except that a post-provenance original raises
  the bar to a stamped carrier: the engine stamps every replacement it
  builds, and the stamp is the only form request rewriting relocates.
- `compact_messages_safe` never mutates the caller's live history; the host
  commits only after `Ok`.

The language-invariant fixture matrix is
`crates/engine/src/compaction/fixtures/matrix.json`. Rust loads it in
`last_round` tests, with each fixture's recent token allowance. The Rust
validator is the single enforcement path.

`/context` names the compaction path and `/anchor` survival.

## Summary and budget integrity

Reasoning-only or placeholder replies cannot become checkpoints. An unusable reply gets one quality retry before the pass fails with its original history preserved. Incomplete replies and tool calls are rejected. Generated prose is explicitly unverified; retained tool receipts and live state remain the evidence for tests, commits and file state. The complete pre-compaction history is saved before a provider call. An automatic summary failure stops the turn instead of continuing from a full window or retrying the same broken summary through emergency recovery.

The parent turn learns its highest observed input-token ratio from actual request usage and applies it to subsequent request sizing, suffix growth, summary sizing and emergency recovery. Replacing history clears stale billed sizes while retaining that turn's calibration. After two compaction attempts, refilling within two model steps stops the turn with an explicit recovery message. Calibration is scoped to the current turn and cannot predict a tokenizer before a provider supplies usage. Summaries continue on the selected route; no implicit provider escalation or new spend is authorized.

Disabling automatic compaction also disables preflight and provider-overflow recovery before any summary request or replacement. The turn reports that refusal and keeps its original history. Explicit manual compaction remains available. Both overflow paths use the same typed failure reporter, so a rejected recovery request retains its own cause instead of falling back to the original overflow error.
