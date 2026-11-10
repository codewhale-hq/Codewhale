# Codewhale operations runbook

This runbook covers the local CLI/TUI. Commands below describe current source;
check `codewhale --version` and compare a published build with the documentation
at its tag before assuming a candidate-only control exists.

## Quick Triage

Start with offline diagnostics in the affected workspace:

```sh
codewhale --version
codewhale config path
codewhale doctor --json
```

`doctor --json` reports structural health without probing provider credentials,
contacting a provider, or starting MCP servers. An offline report is not proof
that a key works or that a remote service is reachable. Do not attach a raw
config, secret store, session archive or environment dump to an issue.

If more detail is needed, reproduce with `RUST_LOG=codewhale_tui=debug codewhale`.
Logs and session content can contain project data, so inspect and redact the
specific excerpt before sharing it. The [troubleshooting guide](INSTALL.md#13-troubleshooting)
covers installation and network messages.

## Incident: Turn Hangs or Stream Stops

First read the transcript for a pending approval, clarification or failed tool.
If a shell command is still running, `Ctrl+B` backgrounds that command while
the turn continues; `/jobs` shows it. Ask Codewhale to stop the identified task
when needed; current source uses `task_shell_wait` with its `task_id` and
`cancel: true`, rather than treating a task id as an operating-system pid.

Press Esc to cancel a turn; close any modal first if it consumes Esc. Interrupting
a turn does not undo already completed file changes or external actions.

For a hosted route, explicitly opt into a connection test:

```sh
codewhale auth status --provider <provider-id>
codewhale doctor --probe-api
```

Replace the provider id with your selected provider; `--probe-api` tests the
active route, not necessarily the provider named in the previous command.
For a local model, use `codewhale doctor --probe-local` instead. These probes
contact the selected service and may create provider usage; they are not part
of the offline triage above.

Reconnect or repair credentials before retrying. A retry can repeat external
actions, so inspect the transcript and [receipts](RECEIPTS.md) first.

## Incident: Network Outage / Offline Behavior

Use `/queue list` to inspect unsent messages. Offline queue state is persisted
per session under `~/.codewhale/sessions/checkpoints/`; a legacy global
`offline_queue.json` may be adopted during migration.

After connectivity is restored, inspect an entry with `/queue edit <n>` and
press Enter to send it. Verify the resulting response and queue state rather
than assuming queued work ran automatically.

## Incident: Crash Recovery Needed

List saved sessions with `codewhale sessions`, then resume explicitly with
`codewhale resume <id>` or `codewhale --continue` for the workspace. Inside the
TUI, Ctrl+R opens the session picker. A fresh launch does not silently resume
an interrupted session.

If recovery fails, preserve the affected state before repair. Use a compatible
binary for a newer schema; do not delete a checkpoint to silence a schema error.
The [legacy-path guide](LEGACY_PATHS.md) covers migration from `~/.deepseek`.

## Incident: Persistent State Schema Errors

Record the exact error and binary version. Back up the affected state directory
before changing files, then use a binary that supports that schema or a documented
migration path. Do not rewrite schema version fields by hand.

Session files, Runtime thread records and Fleet ledgers are different stores;
identify the one named in the failure. Preserve the original so repair can be
reviewed or reversed.

## Incident: MCP/Tool Execution Failures

Review the server entry and exact command in your global or trusted project
`mcp.json`. Running a server manually or reloading it executes its code and can
contact its service, so review that authority first. Use `/mcp` diagnostics and
inspect policy or sandbox refusals rather than widening permissions to make an
unknown failure disappear.

Back up an existing config before repair. `codewhale mcp init --force` replaces
configuration; it is not a harmless validation command. After a deliberate
change, `/mcp reload` refreshes the running connections.

## Post-Incident Checklist

Record what you ran, the exact output, the version and the smallest reproducible
workspace. Include a redacted diagnostic excerpt, the observed result and the
expected result in a bug report. Distinguish a tested fix from a workaround and
from an unverified hypothesis.
