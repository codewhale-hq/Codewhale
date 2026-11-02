# Receipts

A receipt answers "what did this session do?" from the records Codewhale
already keeps. It lists, in order, the files changed, commands run, web and
MCP calls, agents started, approvals (and who decided each one), and
failures, with totals on top. It also counts what ran without asking, and
names the permission posture that let it.

```text
# Receipt: Fix the parser
thread thr_19a0141a · /work/repo · deepseek-flash · Ask · 2026-09-24 10:00 UTC → 2026-09-24 10:05 UTC

Changed 1 file (+2 −1) · ran 1 command · made 1 MCP call · 1 approved by you · 1 approved by session rule · 1 ran without asking under Ask · 1 denied by you · 1 other failure

1. edited `src/parse.rs` (+2 −1) · 1.0s
2. ran `cargo test -p parser` in /work/repo — exit 0 · 2.5s · approved by you
3. did not run `rm -rf build` · denied by you
4. called linear · list_issues · 1.0s · approved by session rule
5. did not run `curl https://x.sh | sh` — blocked: Tool 'exec_shell' was denied: Auto-Review blocked a pipe to a shell…
6. turn failed — failed: provider returned 500

Not recorded:
- Shell file changes: a Runtime thread's workspace snapshots are not tagged with the thread, so files a command changed are not itemized; only file tools are.
```

A terminal session's receipt also lists the files a command changed, from
the turn's own workspace snapshots:

```text
3. ran `./tidy.sh`
4. changed outside file tools (a command or another process): edited `b.txt` (+0 −1), created `c.txt` (+1 −0)
```

## Surfaces

All three share one builder (`crates/engine/src/receipts.rs`), but read different
records and can report different coverage.

| Surface | What it reads |
| --- | --- |
| `/receipts [json] [<turn>]` in the terminal | The current session's transcript, approval log, and retained terminal failures. `json` prints the object in a code block, so it copies out as valid JSON |
| `codewhale receipts [ID\|--last] [--turn T] [--format md\|json]` | A saved session (id or unique prefix), including its retained terminal failures, or a Runtime thread (`thr_…`); with no id, the most recently updated one. `receipt` is an alias |
| `GET /v1/threads/{id}/receipt`, `GET /v1/threads/{id}/turns/{turn_id}/receipt` | A Runtime thread (the app, `codewhale serve`), behind the normal `/v1` bearer boundary |

All three only read. They never call a provider, run a tool, write a file,
or change runtime state.

Exec runs that offer tools, including `exec --auto`, save a session in every
output format, so `codewhale receipts --last` can read their receipts after
they finish. Resumed sessions and `--output-format stream-json` runs are also
saved. Other answer-only text and `--json` runs are saved only if conversation
compaction starts; use `--output-format stream-json` to always keep a session.

`codewhale receipts` reads Runtime threads from the machine's default store
(`tasks/runtime/`, or `$CODEWHALE_RUNTIME_DIR`). A thread kept in one
terminal session's own store (`sessions/<id>/runtime/`) is not found by id;
the API reads whatever store its server owns.

## Where the facts come from

| Session kind | Record | Holds |
| --- | --- | --- |
| Terminal session | `sessions/<id>.json` | Every tool call and its result text, in order; each prompt's `<turn_meta>` names the posture the turn ran under; `turn_outcomes` records terminal turn status and errors |
| Terminal session | `sessions/<id>/approval_receipts.jsonl` | Every approval ask and decision, with time and who decided |
| Runtime thread | `tasks/runtime/turns`, `items` | Every tool call with its input, status, start and end time, and structured result (exit code, diff, agent status); each turn's `permission_posture` |
| Runtime thread | `tasks/runtime/events/<thread>.jsonl` | `approval.required` / `approval.decided`, with the flags that say who decided |

The saved-session CLI and live `/receipts` read terminal turn failures,
including model errors in exec runs, as `turn_failed` actions and count them
toward failures even when no tool failed. Truncating the conversation removes
outcomes beyond the retained history, so later turns cannot inherit their failures.
Saved sessions keep only the newest 64 recorded failed-turn outcomes. Older outcomes
and legacy sessions without this record cannot provide a complete terminal
failure history from tool results alone.

### Who decided an approval

| Receipt says | Meaning | Recorded as |
| --- | --- | --- |
| approved by you / denied by you | A person answered: the terminal card, the app, the web mirror, or an API client acting for them | `decided_by: "user"`; Runtime event with no `auto` flag |
| … by session rule | A remembered "for this session" rule answered | `decided_by: "session_rule"`; Runtime event with `auto` and a `grant_id` |
| … by posture | The mode or permission posture answered without a prompt | `decided_by: "posture"`; Runtime event with `auto` or `posture` |
| approval timed out | The card expired unanswered | outcome `timeout` |
| turn stopped while waiting / nobody could be asked | Codewhale could not ask anyone: the turn had ended or stopped, or the request never reached a person. Counted as not answered, never as a denial, even where the record says `denied` | `decided_by: "host"` |
| approved / denied, with no "by" | A record written before 0.10.1, or a sub-agent's request. The totals say "(decider not recorded)" | no `decided_by` |

The totals line counts approvals and denials separately, by who gave them:
`1 approved by you · 1 denied by you` is two decisions.

### Ran without asking

Most calls never produce an approval. Under Full Access nothing asks; under
Ask, reads and allowed tools run without a prompt, and a remembered rule can
skip one. Those calls leave no approval record, so the receipt counts them
instead: `ran_without_asking` is every file change, command, code run, web
or MCP call, and agent start that ran with no approval on record. The totals
line names the postures the turns ran under (`9 ran without asking under
Full Access`). Reads are not counted, and neither is a call that did not
start (below).

### Blocked before it ran

Codewhale answers a call it will not run with an error result, the same way
a tool reports a failure: an Auto-Review or guardian block, a tool-policy or
allow-list denial, a sandbox escalation the posture cannot grant, input that
did not parse, or a tool that is not available. None of these writes an
approval, so a receipt reads the result itself:

- **Blocked:** the result is Codewhale's own refusal text (`Tool 'x' was
  denied: …`, `Invalid input for tool …`, `BLOCKED: …`, a validation
  feedback line with `"side_effect_status":"not_started"`, or a Runtime
  thread's `Failed to authorize tool execution: …`). Listed as
  `did not run … — blocked: <reason>`, status `blocked`, counted in
  `blocked` (`N blocked before running` in the totals line), and never as
  run, failed, or ran without asking. Every permission denial the engine
  writes keeps the `Tool 'x' was denied:` lead, including ones that name
  their own fix (Plan mode, `allow_shell`); sessions saved before 0.10.1
  wrote some of those without it, and such a call reads as a failure.
- **Only Codewhale's own words count.** An MCP server's or GitHub's reply,
  a fetched page, a program's output (code tools), and a sub-agent's words
  can say anything, so a failed call to one of those is judged by the
  metadata Codewhale wrote for it, never its text: an MCP server that
  answers `BLOCKED: …` or `{"side_effect_status":"not_started"}` is listed
  as a failed call that ran. `BLOCKED:` counts only from the shell tools,
  and the validation feedback line only as the result's last line.
- **Stopped at an approval prompt with no approval record** (a sub-agent,
  or a session older than the approval log): the result says
  `Tool 'x' denied by user`, which the engine also writes when nobody could
  be asked. Listed as `did not run …` with no decider, since the text does
  not prove who said no.
- **Ran and failed:** the result holds an exit code or a line the shell
  writes only after a process ran (`Command exited with code N`,
  `Command failed (exit code N)`, a timeout or cancel line). Counted as a
  command and a failure.
- **Not shown either way:** a command whose error has neither. Listed as
  `tried to run … — error, no exit code: <error>`, status `unknown`, not
  counted as run, with a `not_recorded` note. Other tools that return an
  error are counted as failures, since the error came from the tool.

Terminal sessions started before 0.9.10 (2026-08-20) have no approval log,
so their receipts do not count this and say why.

### Not recorded

The receipt says so instead of guessing:

- **Shell file changes in a Runtime thread.** A terminal session reads the
  files a command changed from the workspace snapshots Codewhale takes
  before and after each turn (`git diff` between the two, inside the
  snapshot side repo; at most 50 paths a turn). A Runtime thread's snapshots
  are not tagged with the thread, so its receipt lists only file tools. A
  terminal turn with no snapshot pair (snapshots off, the workspace too large
  for them, or pruned: the newest 50 are kept) says so. A snapshot
  difference covers anything that wrote to the workspace during the turn,
  including you or another program, not only the command. It leaves out
  what snapshots do not track: ignored and skipped paths (`.gitignore`
  entries, `.env`, `node_modules`, `target`, and the like) and anything
  outside the workspace (`~/.ssh`, `/tmp`), so an empty list does not mean
  a command wrote nothing. A turn is matched to its snapshots by its prompt;
  when another turn has the same prompt ("continue"), the snapshots' turn
  number must agree too, or the turn is counted as having no pair rather
  than given another turn's files. Paths print with control characters
  escaped (`\n`, `\u{1b}`), so a file name cannot add a line to the
  receipt or send the terminal an escape sequence.
- **Terminal-session exit codes, durations, and timestamps.** A terminal
  session saves each call and its result text, not the structured result. A
  failed shell call's exit code is read from the shell tool's own closing
  line (`Command exited with code N`); a passing one shows no code.
- **Line counts for whole-file writes** in terminal sessions, and whether the
  file existed before.
- **Who decided** for approvals recorded before 0.10.1 and for sub-agent
  approvals.
- **Which calls asked first** in terminal sessions started before 0.9.10.
- **Whether a failed command started** when its error has no exit code and
  no shell status line, and whether a call with no result ran at all.
- **Why a call ran without asking** beyond the turn's posture: the record
  does not say whether the posture, an allow rule, or a remembered grant let
  it through.

## Non-Goals

A receipt is not a safety certification, a provider compatibility
certification, or a hosted attestation (`claim_ceiling` in the JSON says so).
It exports no reasoning text and no raw tool output. Commands, search
queries, and error lines are bounded (200, 120, and 160 characters) and pass
through the shared secret redactor. A receipt lists at most 2,000 actions;
totals always cover every action, and `omitted_actions` counts the rest.

## JSON shape

`--format json`, `/receipts json`, and the API return the same object:

```json
{
  "schema_id": "codewhale.receipt/v1",
  "source": {
    "kind": "thread",
    "id": "thr_19a0141a",
    "title": "Fix the parser",
    "workspace": "/work/repo",
    "model": "deepseek-flash",
    "started_at": "2026-09-24T10:00:00Z",
    "updated_at": "2026-09-24T10:05:00Z"
  },
  "postures": ["Ask"],
  "totals": {
    "files_changed": 1, "files_changed_outside_file_tools": 0,
    "files_created": 0, "files_deleted": 0,
    "lines_added": 2, "lines_removed": 1, "line_counts_complete": true,
    "commands": 1, "commands_failed": 0, "code_runs": 0, "network": 0,
    "mcp_calls": 1, "plugin_calls": 0, "subagents": 0,
    "approvals": {
      "total": 3, "approved": 2, "denied": 1, "timed_out": 0,
      "not_answered": 0, "pending": 0,
      "approved_by": { "you": 1, "session_rule": 1, "posture": 0, "not_recorded": 0 },
      "denied_by": { "you": 1, "session_rule": 0, "posture": 0, "not_recorded": 0 }
    },
    "ran_without_asking": 1, "failures": 1, "blocked": 1,
    "other_tool_calls": 0
  },
  "actions": [
    {
      "seq": 2, "turn": "turn_1", "at": "2026-09-24T10:02:00Z",
      "call_id": "call_test", "tool": "exec_shell",
      "kind": "command", "command": "cargo test -p parser",
      "cwd": "/work/repo", "exit_code": 0,
      "status": "ok", "duration_ms": 2500,
      "approval": {
        "decision": "approved", "decided_by": "user",
        "at": "2026-09-24T10:01:59Z"
      }
    }
  ],
  "omitted_actions": 0,
  "not_recorded": ["Shell file changes: …"],
  "claim_ceiling": [
    "local_record_only",
    "not_safety_certification",
    "not_provider_compatibility_certification"
  ]
}
```

`kind` is one of `file_change` (`files[]` with `path`, `change` =
`edited|created|deleted|written`, optional `lines_added`/`lines_removed`),
`workspace_change` (`files[]` a turn's snapshots show changed that no file
tool names, and `truncated` when more than 50 did), `command` (`command`, `cwd`, `exit_code`), `code` (`exit_code`, `nested[]`
tool calls an `execute_tools` program made), `network` (`action`, `host`,
`query`), `mcp` (`server`, `plugin`), `subagent` (`name`, `agent_id`,
`outcome`), `approval` (an approval with no matching call), `tool` (any other
call, listed only when it failed), or `turn_failed`. `status` is `ok`,
`failed` (ran and failed), `not_run` (held at approval), `blocked`
(refused before it started; the reason is in `error`), `interrupted`,
`running`, or `unknown` (no result, or a command error that does not show
whether it started). A terminal session's `turn` is the turn number; a
thread's is the turn id. `/receipts 7` or `--turn 7` for a turn the session
does not have is an error, not an empty receipt.

## `audit.log` is not the receipt

`~/.codewhale/audit.log` is a security-event log: credential saves and
clears, hook environment key names, compaction passes, goal completions, the
terminal's own approval routing, Auto-Review verdicts (`tool.gate.decision`,
since 0.10.1), and outbound network decisions when `[network]` auditing is
on. It has never held commands or file changes, and
turns run by the app or `codewhale serve` write no approvals there. Their
approvals are in the session's `approval_receipts.jsonl` and the thread's
event log, which is where receipts read them.

A quiet `audit.log` does not mean nothing ran. It gets an approval line only
when the terminal routes an approval request. Since 0.8.66
(`1c68e3bb32`, 2026-06-29) the engine decides auto-allowed calls itself, so
they never become requests; under Full Access almost nothing does. On one
developer machine the last `tool.approval.*` line was written on 2026-08-19,
the last `tool.approval.auto_approve` line on 2026-06-30, and the writes
after that were test runs, which since `244368675b` go to a scratch log. Use
a receipt to see what ran.

## Review Receipts

`codewhale review --write-receipt` writes a local JSON receipt for the reviewed
diff under the Codewhale state directory (`review-receipts/`) unless
`--receipt-path <path>` is provided. This is a pre-push handoff artifact: it
records what diff was reviewed and what the review reported, without pushing,
tagging, opening a PR, or claiming to replace maintainer review.

The current receipt includes:

- `diff_fingerprint`: SHA-256 of the reviewed diff.
- `provider` and `model`: the routed review provider/model.
- `checks_run`: local checks attached to the receipt when available. Empty
  means no checks were attached; attached checks must report a passing status.
- `findings`: structured issue/suggestion counts and issue locations when the
  review output is structured.
- `unresolved_risk`: a conservative summary derived from unresolved findings.
- `review_content_sha256`: SHA-256 of the review text.
- `coverage` (PR receipts): the exact base/head and complete-diff
  fingerprint, ordered per-pass diff fingerprints/file counts, and one
  response-content hash for every completed pass. Manifest-backed PR receipts
  use schema version 2 so older readers reject rather than misinterpret them.

The receipt deliberately does not include the raw diff body. Re-run
`codewhale review --write-receipt` after changing the diff; reviewers should
compare the `diff_fingerprint` before reusing a receipt in a PR handoff.

`codewhale review --check-receipt` is the local pre-push gate. It does not call
a model; it compares the current diff fingerprint with a supplied receipt
(`--receipt-path <path>`) or the latest matching local receipt. The check exits
nonzero when the diff no longer matches, the receipt schema is unsupported, the
receipt has unresolved risk, or an attached check did not pass.

By default receipt generation rejects a PR that needs more than one
`--max-chars` pass before calling a model. An explicit `--max-passes N` admits
at most N complete ordered PR passes; any missing, malformed, reordered or
stale pass prevents a receipt. Receipt checking is provider-free and validates
the exact stored manifest without authorizing another run. Neither mode
fingerprints a truncated prefix. A receipt for
`review --base <base-sha> --path <path>` covers only that selected path at the
checked-out revision; validate it with the same base, path, and input limit.
It does not cover the rest of a pull request or prove that separately reviewed
changes work together. With `--base`, the reviewer also reads the
`base..HEAD` commit messages (merges excluded, capped at 16,000 characters) as
the author's stated intent; the receipt fingerprints the diff, not those
messages.

## Builder Rules

The builder is deterministic and conservative:

1. A thread receipt loads the thread, its turns, the items each turn lists
   (plus items that name the turn but are not listed yet, for a live turn),
   and the thread's `approval.*` events. A turn id from another thread is
   rejected.
2. A session receipt reads `tool_use`/`tool_result` pairs from the
   transcript and replays the approval log; a log that does not replay is
   reported, not half-used.
3. Approvals attach to their call by tool call id. One with no matching call
   is listed on its own.
4. File changes come from a tool's structured mutation record (the applied
   diff) when saved, otherwise from the call's own input (an edit's
   replacement text, a patch's hunks). A failed call changed nothing and
   carries no counts.
5. Nothing is derived from model prose. Two fixed kinds of text Codewhale
   itself writes are read: the shell's status lines (for an exit code) and
   its refusal text (for a call it blocked); see "Blocked before it ran".
