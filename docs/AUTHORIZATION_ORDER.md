# Authorization order

> 阅读简体中文版：[zh_hans/AUTHORIZATION_ORDER.md](zh_hans/AUTHORIZATION_ORDER.md)。

Codewhale combines tool availability, hooks, typed permission rules, approval
posture, repository policy, and sandboxing. An approval from one layer is not a
universal bypass: a later safety layer can still require review or block the
call, and an approval is not an operating-system sandbox grant.

This page records the order implemented by the interactive engine's
model-requested tool path. Entrypoints that use only part of that path, such as
the core runtime's direct tool API, keep the relative order of the layers they
do use.

## Model tool-call pipeline

The interactive engine evaluates a model-requested tool call in this order:

| Order | Layer | Result |
|---:|---|---|
| 1 | Effective configuration and posture | User settings, command/runtime overrides, and the project overlay resolve before the turn. A project overlay may tighten `approval_policy`, `sandbox_mode`, or shell availability, but may not loosen them. |
| 2 | Mode and tool admission | Plan-mode restrictions, input parse errors, per-command tool deny/allow lists, caller restrictions, and missing execution registrations fail before policy rules are considered. A tool present in both command lists is denied. |
| 3 | Preparation, then `tool_call_before` hooks | Registry preparation is side-effect free. Foreground hooks then fold as `deny > ask > allow`, and a strict matching hook that produces no verdict fails closed. The last `updatedInput` wins; the engine prepares the rewritten input again before later gates inspect it. |
| 4 | Registered-tool baseline | The prepared tool's `ApprovalRequirement` establishes its ordinary approval need. A hook `ask` is applied after that assignment so the baseline cannot erase it. Non-bypassable registered holds remain forced in postures that can prompt; Full Access auto-approves them instead of opening a contradictory modal. Plan mode also blocks write-capable tools here. |
| 5 | Typed `permissions.toml` rule | A matching `deny` blocks. A matching `allow` may clear only ordinary registry approval; it cannot clear a hook `ask` or a non-bypassable registered hold. A matching `ask` forces review only in a posture that can prompt. Full Access/auto-approval is not downgraded into a prompt, while an explicit typed `deny` still blocks. |
| 6 | Auto-review policy and built-in safety floor | Configured block rules run before the built-in floor, then configured allow rules and the deterministic fallback. This layer runs after typed permissions and can add a prompt or block, but cannot remove an earlier hold. The built-in floor refuses a shell command, shell input or verifier containing a hidden Unicode character, and holds publish-like actions, destructive background/headless actions and `curl`/`wget` credential uploads with a prompt no grant or allow rule can answer (a block in Auto-Review and Never). Full Access skips the built-in floor. |
| 7 | Repository law | Protected path invariants can only add a prompt or block. A repo-law prompt becomes a hard block in Full Access, which has no contradictory approval modal. |
| 8 | Human approval | A remaining prompt is sent to the approval channel. Denial stops the call. Approval authorizes this planned call; session and persistent choices affect later matching calls but do not erase a later gate from this call. |
| 9 | Tool authority and execution sandbox | Worker authority envelopes, native tool path checks, and the selected OS or external sandbox still apply during execution. A sandbox denial remains a denial unless the user separately authorizes a supported elevation path. |

The ordering is intentionally monotonic after the typed permission layer:
auto-review and repository law can tighten a result, not turn a previous block
or prompt into an unreviewed execution. There are explicit posture choices
inside those layers—for example, Full Access does not create an interactive
publish prompt—but those choices do not let an earlier remembered grant erase
a hold that the layer actually produced.

## Typed permission-rule selection

`permissions.toml` is currently a sibling of the active user `config.toml`.
There is no project-local permission-rule source today. An optional `workspace`
field scopes one user rule to a repository; it does not create a project
overlay. `/permissions` reports that source, matcher, scope, and whether the
scope applies to the current workspace.

The execution-policy engine evaluates matching rules as follows:

1. Normalize the tool, command, workspace, and any workspace-relative path.
   Unsafe or external paths do not become matchable file rules.
2. Check denied command prefixes against the whole command and every chained
   segment. These hard prefix denies are merged across rulesets and always win.
3. Compute a trusted-prefix candidate for an unchained shell command. This is a
   candidate for the approval-mode fallback, not an immediate decision.
4. Select one matching typed rule by this lexicographic precedence:
   1. higher source layer: `User > Agent > BuiltinDefault`;
   2. stronger action inside that layer: `deny > ask > allow`;
   3. the more specific matcher when layer and action tie.
5. Apply the selected typed action. `deny` forbids the call, `allow` skips the
   execution-policy approval, and `ask` requires approval. A typed `ask`
   overrides a trusted prefix.
6. If no typed action decides the result, apply the approval-mode fallback
   using the trusted-prefix candidate.

Source layer is compared before typed action. Consequently, a user-layer typed
`allow` can override an agent-layer typed `deny`; inside the same layer, `deny`
still beats `ask`, which beats `allow`, regardless of file order or matcher
specificity. Hard denied prefixes are the exception: they are checked before
typed-layer selection and cannot be overridden by a typed allow.

Specificity is only a tie-breaker after source and action. A constrained
command, exact-command, path, or workspace matcher beats a tool-wide rule with
the same action in the same layer. Specificity never lets a narrow allow beat a
same-layer deny.

For chained shell commands, a trusted prefix never approves the whole chain. A
typed deny that wins for any individual segment blocks the full invocation.

## Approval posture and missing prompts

The execution-policy result is combined with the registered-tool baseline; the
two should not be interpreted independently.

- Ask and Auto-Review may surface tool safety approvals. Model-authored user
  questions (`request_user_input`) are a separate channel and reach the user in
  every interactive posture, Auto-Review included. Only headless `exec` runs,
  which have no responder, withhold the tool. Runtime threads (app
  conversations, background tasks and automations) keep it: a question parks
  the turn until someone answers or cancels it through the app or runtime API,
  or until a positive `tools.user_input_timeout_seconds` expires. An omitted or
  zero timeout waits indefinitely, the same way an Ask-posture approval parks.
- Full Access and YOLO-compatible auto-approval paths do not let a typed `ask`
  downgrade the session into prompting. Non-bypassable registered holds
  auto-approve in Full Access. Typed deny, catastrophic background/headless
  safety holds, and repository law still fail closed where their respective
  layers apply.
- With `approval_policy = "never"`, a matching typed `ask` is forbidden because
  the required prompt cannot be shown.

Runtime adapters may transport an approval decision differently from the
interactive modal. That transport and any continuation protocol are separate
from this ordering contract; see [Runtime API](RUNTIME_API.md).

## Project overlays

The project config overlay at `<workspace>/.codewhale/config.toml` is not a
permission-rule layer. It can only move approval and sandbox posture toward
more restrictive values:

- approval: `auto` → `on-request`/`untrusted` → `never`;
- sandbox: `danger-full-access` → `workspace-write` → `read-only`;
- shell availability: `true` may become `false`, never the reverse.

Project config cannot add credentials, hooks, provider authority, or a
project-local `permissions.toml`. See
[Configuration](CONFIGURATION.md#per-project-overlay-485) for the complete
overlay allow-list.

## Regression coverage

The contract is exercised by tests at the layers that own each decision:

- `authorization_order_contract_matches_documented_precedence` covers hard
  prefix denial and the typed `layer → action → specificity → approval-mode`
  sequence through the public execution-policy API.
- `hook_fold_deny_wins_over_ask_and_allow` covers foreground hook folding.
- `non_bypassable_registered_tools_auto_approve_in_full_access` covers
  registered holds.
- `full_access_permission_allow_cannot_bypass_background_catastrophic_floor`
  and `full_access_permission_allow_cannot_bypass_repo_law` cover later safety
  layers overriding a remembered allow.
- `project_merge_only_tightens_approval_and_sandbox_policy` covers project
  overlay monotonicity.

Focused commands:

```bash
cargo test -p codewhale-execpolicy --test authorization_order --locked
cargo test -p codewhale-tui --lib --locked full_access_permission_allow_cannot_bypass
cargo test -p codewhale-config --locked project_merge_only_tightens_approval_and_sandbox_policy
```

## Related references

- [Configuration](CONFIGURATION.md) — rule schema, `/permissions`, hooks, and
  project overlays
- [Modes](MODES.md) — Plan/Work and permission posture
- [Sandbox threat model](SANDBOX.md) — platform enforcement and fallbacks
- [Runtime API](RUNTIME_API.md) — approval events and remote resolution
