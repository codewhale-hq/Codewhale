# Auto-Review

Auto-Review is the permission setting where Codewhale reviews each tool call
itself instead of asking you. Safe calls run, dangerous calls are blocked with
a reason, and uncertain calls go to a second model that judges that one call.
You are asked only for the few things that need a person.

It is one stop on the permission dial described in
[Modes and permissions](MODES.md#permission-posture), and one layer of the
[authorization order](AUTHORIZATION_ORDER.md). It does not bypass tool
admission, repository rules or the [sandbox](SANDBOX.md).

## Turn it on

- Press `Shift+Tab` until the footer shows Auto-Review, or run `/auto`.
- Or run `/config` and set the `permission_posture` row to `auto-review`.
- From the shell, start with `--permission-mode auto-review`.

The internal value is `auto`; `approval_policy = "auto"` in config selects it.

## How a call is decided

Two layers decide. The first is fixed rules; the second is a model.

### Layer 1: the deterministic floor

The floor is your configured block rules plus a built-in safety floor. It is
never model-reviewed, and on unattended work it never waits for an approval
card. It does one of three things with a call.

**Allows** calls it can prove safe. This includes explicit routine workspace
test and build commands, also with a trailing `2>&1` or `>/dev/null` and a
single `head`, `tail` or literal `grep` filter. It does not include
`cargo run`, non-routine `make` targets, `git add`, `git commit`, discard
flags, or anything your own block rules name.

**Blocks** outright:

- Package and release publishing: `npm publish`, `cargo publish`,
  `gh release` and release tools.
- Destructive work in an unattended run (see
  [Attended and unattended runs](#attended-and-unattended-runs)).
- Any repository push or tag change in an unattended run.

**Holds** for the model reviewer, ahead of your `auto_review.allow` rules:

- A push to a repository remote in an interactive session.
- The named soft rules: `force-push`, `git-tag`, `deploy-branch`,
  `git-discard`, `infra-destroy`, `ci-or-pr-bypass`, `print-credentials`,
  `secret-commit` and `deploy-wrapper`. The `print-credentials` rule includes
  `.claude.json` and `.config/gh/hosts.yml`.
- Writes, edits, deletes or moves that target `AGENTS.md`, `CLAUDE.md` or a
  loaded instruction file.
- File or shell writes that target a protected repository, credential,
  IDE or agent, workflow, shell-startup or autostart path: `.git`,
  `.codewhale/`, `.vscode/`, `.claude/`, `.mcp.json`, `.claude.json`,
  `.github/workflows/`, `.gitconfig`, `.config/gh/hosts.yml`, shell rc and
  profile files, `Library/LaunchAgents/`, `.config/autostart/`,
  `.cargo/config.toml`, `.config/fish/`, `.config/systemd/user/` and the
  Windows `Startup` folder, including symlink and path-alias targets. These
  are held ahead of narrow allow rules and routine checks too.
- Outbound web requests, and any call the floor could not prove safe.

Three more rules belong to the floor:

- **Folders without git.** A bounded workspace write is treated as safe only
  when local snapshots are enabled (`snapshots_enabled`) and the workspace
  root is not `/`, a home folder or a top-level folder. Deleting a file or
  emptying a whole file in such a folder still goes to the reviewer. The
  Accept Edits rule for git work trees is unchanged.
- **First read outside the workspace.** The first shell read that resolves
  outside the workspace asks you once per Auto-Review session when an
  interactive owner can confirm it. Standard pseudo-devices such as
  `/dev/null` do not count. An unattended run sends it to the reviewer
  instead. The once-per-session state resets when you leave Auto-Review or
  rewind or reset the session.
- **Critical-path deletions.** Deleting the filesystem root, the home folder,
  the project root or any ancestor of it, or a `.git` folder asks you in an
  interactive session and is refused in an unattended or headless one.

### Layer 2: the model reviewer

A held call goes to a one-shot reviewer model. It returns a risk level, allow
or deny, and a reason.

- **Which model.** `auto_review.reviewer_model` when set. Otherwise a capable
  model on the session's active provider route when one matches, or the
  session model. It never switches provider or account.
- **What it has.** No tools and no memory of earlier rules. It makes a single
  request with a 90-second deadline. Incomplete or malformed output, a
  timeout, a cancellation or a provider failure all count as "unavailable",
  which blocks the call.
- **High or critical risk** cannot run automatically unless you approve a
  one-use retry of that exact call. An explicit denial by the reviewer still
  blocks even then.
- **Soft-rule holds** carry the rule name in the hold, the block reason and
  the receipt. They need an explicit request from you for that action, not a
  broad instruction.

The reviewer receives structured evidence, marked as untrusted, and nothing
else:

| It receives | Notes |
| --- | --- |
| The exact `proposed_tool_call`, including its `description` | Never truncated. |
| Deterministic observations | Includes `person_approved_retry` and any triggering `soft_rule`. |
| `recent_actions` | Earlier non-read actions from the current turn, redacted. |
| `approved_plan` | When there is one. |
| `environment` | Scope entries from user or managed config. A project `.codewhale/config.toml` cannot set or change `auto_review.environment`. |
| `project_instructions` | Captured at session start with separate user and repository labels and SHA-256 hashes; never re-read from disk during the session. |
| `person_messages` | What the conversation owner typed, newest first. On a sub-agent turn these come from the root conversation, not from the child's brief. Empty on an agent-created background thread. On runtime threads the owner composer is the signed-in web client or a runtime-token caller that names `"client": "desktop"` (or `tui`/`web`) on the turn or steer request; other Runtime API callers record nothing. |

It never receives attached `@file` bodies, skill instructions, expanded
context, tool outputs or assistant prose.

The context is capped at 64 KiB. The current request and the exact proposed
call are never truncated; if they alone exceed the cap the review fails closed
as unavailable. Under pressure, repository instructions are trimmed first,
then user instructions, then the oldest middle `person_messages`, keeping the
earliest typed limit and the latest request.

## When a call is blocked

Every block names the rule and the reason. Codewhale then either takes
another route or, in an interactive session with you present, offers a
one-use approval of that exact call:

- press `r` in an empty composer on the block receipt,
- run `/permissions blocked`, or
- call the Runtime API: `POST /v1/threads/{id}/auto-review/approve`.

A retry grant is bound to the exact `(tool_name, call_digest)`, is used up by
one retry, and still passes every other gate: hooks, typed permissions,
repository rules, the deterministic floor and the sandbox. It never extends to
sub-agents or extension calls, and it is cleared when you leave Auto-Review or
reset the conversation.

## Repeated blocks

| Run | Threshold | What happens |
| --- | --- | --- |
| You are present, and this is not a child worker | 3 blocks in a row, 20 blocks in the conversation, or 3 unavailable reviewer checks in a row | Permissions switch to Ask (`AutoReviewPaused`) and the work continues in Ask. Switching back to Auto-Review resets the counters. |
| Unattended | 20 blocks in total, or 10 unavailable reviewer checks in a row | The run never falls back to Ask. It continues past blocks and ends as interrupted with a summary of the blocked actions. |

An allowed review resets the in-a-row block and unavailable counts. An
explicit denial by the reviewer resets the in-a-row unavailable count.

## Attended and unattended runs

Each turn carries two facts minted by the engine. A host or a request field
cannot supply either.

- **Run origin.** `Interactive` only when the turn was started or steered
  directly from the owner's active terminal composer, or from a signed-in
  owner runtime composer with an owner surface attached. `Headless` for `exec`
  and ACP runs, including `approval_policy = "auto"`. `Background` for
  detached starts, detached children, tasks, automations and delayed
  `send_later` runs.
- **Owner presence** (`owner_present`).

"Unattended" on this page means `Background`, `Headless`, or any turn where
`owner_present` is false.

**When you are present.** Prompts that something else forces open the normal
approval card instead of being waived or failing closed: a typed
`permissions.toml` `ask` rule, a `tool_call_before` hook that answers `ask`,
prompt-only tools such as Computer Use consent and script cards, and
repository-rule holds that need a person. The exception is a hard block
(`hidden_unicode` or a configured `auto_review.block` rule). On a runtime
thread only the owner's surface (`desktop`, `tui` or `web`) may answer such a
card.

**When you are not present.** No approval card is ever opened. A forced
prompt is refused, naming the rule, without registering a pending approval,
and a repository-rule hold that needs a person is blocked.

**Headless `exec`** consults the model reviewer for non-destructive holds and
applies the deterministic unattended rules to destructive work.

**ACP** turns use the deterministic rules only and refuse a call that only
the reviewer could decide. See [Runtime API: ACP stdio adapter](RUNTIME_API.md#acp-stdio-adapter-codewhale-serve---acp).

**Extension calls** that are held go to the reviewer with their host-minted
plugin identity. They are never cleared by routine tiers or allow rules.
Independent typed rules, hook holds and safety-floor blocks still apply.

## Questions to you

Deliberate questions stay available in interactive sessions through
`request_user_input`. A question parks the turn until you answer or cancel it,
or its configured timeout expires. Headless `exec` withholds that tool because
nobody can answer. Tool safety holds are separate from these questions.

## Sub-agents and Fleet workers

A worker under an Auto-Review parent follows the same policy:

- Proven-safe calls run. Publish-like and destructive background work is
  blocked.
- A hold it cannot prove safe goes to the same one-shot reviewer, using the
  child's own session client.
- No prompt is ever opened for a child. An unavailable reviewer denies.

When a child returns to an Auto-Review parent after running one or more tool
calls, the reviewer checks the child's list of executed actions once. It sees
tool names and compacted, redacted call inputs only, never tool results or the
child's report text, and compares them with the root owner's messages
(`person_messages`), the approved plan and the pinned project instructions.
If it flags a concern, or cannot run (unavailable, timed out, cancelled or
over the context budget), the engine puts a sanitized
`[Auto-Review warning: ...]` banner at the top of the report delivered to the
parent.

Blocks from child calls and from return reviews are recorded in the parent's
blocked-actions list (`/permissions blocked` and
`GET /v1/threads/{id}/auto-review/blocks`) as `[sub-agent <id>]` entries. They
cannot be approved for retry (`approvable: false`).

How children behave under the other permission settings is in
[Modes and permissions](MODES.md#children-sub-agents-and-fleet-workers).

## Configure it

The `[auto_review]` table (allow and block rules, `reviewer_model`,
`environment`, `review_all_shell`, and the managed `disabled` switch) is
documented in [Configuration](CONFIGURATION.md). A project config file cannot
set it.

## Design references

The LLM reviewer is closest to OpenAI Codex's experimental Auto-Review at
commit [`6fc6b9d6d2580d62622fc9884b5f5707f6505a5e`](https://github.com/openai/codex/tree/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e).
Codex's [guardian entry point](https://github.com/openai/codex/blob/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e/codex-rs/core/src/guardian/mod.rs)
reconstructs full conversation transcripts and runs a dedicated review session.
Codewhale passes only Rust-recorded owner `person_messages`, optional approved
plan text, redacted earlier non-read actions, session-pinned instructions,
deterministic observations, and the exact `proposed_tool_call` within a 64 KiB
cap, paired with a 90-second deadline, fail-closed result, and per-turn denial
circuit breaker. It excludes assistant prose and tool outputs and does not copy
Codex's transcript reconstruction, user-authorization score, reviewer tools,
retries, persistent review session, or denial ledger.

Kimi Code at commit
[`1414d4602898f406e540b23342cb18db23ff9efc`](https://github.com/MoonshotAI/kimi-code/tree/1414d4602898f406e540b23342cb18db23ff9efc)
also has no LLM reviewer. Its ordered
[permission policy](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/permissionPolicyService.ts)
applies explicit deny rules and then its
[Auto policy](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/policies/auto-mode-approve.ts)
returns `approve` directly. Codewhale uses the deterministic floor and reviewer
described above, while keeping deliberate user questions available.

The sandbox and escalation baseline is grounded in DeepSeek Harness
`0.1.0-rc.5` at
commit [`47f943859bef60e4160492346772ded9b24f765a`](https://github.com/deepseek-ai/deepseek-harness/tree/47f943859bef60e4160492346772ded9b24f765a):
its [sandbox contract](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/sandbox.md)
defines per-call `read-only`, `workspace-write`, and `danger-full-access`
boundaries and forbids silent unconfined fallback; its
[approval contract](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/approval.md)
grants only `allowed-once` and fails closed on rejection, cancellation, or an
unavailable answerer; and its
[sandbox result contract](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/shell/bash-sandbox/README.md)
tells the model to retry a denied command exactly once with the narrowest wider
mode plus a justification. DeepSeek Harness does not add an LLM reviewer to
that path. Codewhale's autonomous posture adds only the single stateless
reviewer request described above; deterministic hard blocks remain
non-bypassable in the reviewing postures.
