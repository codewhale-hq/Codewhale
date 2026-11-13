# Runtime HTTP route inventory

This table is generated from literal route registrations in
`crates/app-server/src/runtime_api.rs`. It names the paths and methods that
the current source registers, including native-client and compatibility
surfaces. A registered path is not a grant of permission or a promise that
a feature is enabled in your installed version.

The [Runtime API contract](RUNTIME_API.md) explains request fields, events,
authorization and compatibility. This inventory is a navigation index, not
a replacement for those schemas; routes used by the native client can have
client-specific contracts. Provider calls, file mutations and connected-service
actions remain subject to their documented authority boundaries.

Run `node web/scripts/check-runtime-routes.mjs --write` after changing a
registration; `npm run check:docs` refuses a stale inventory. Dynamic nested
routers, fallback handlers and static-asset mounts are outside this literal
registration index.

| HTTP method | Path |
| --- | --- |
| GET | `/` |
| GET | `/__codewhale/bootstrap/{nonce}` |
| GET | `/__codewhale/mobile/bootstrap/{nonce}` |
| POST | `/__codewhale/mobile/session` |
| POST | `/__codewhale/mobile/stream-ticket` |
| POST | `/__codewhale/web/stream-ticket` |
| GET | `/assets/codewhale-192.png` |
| GET | `/assets/codewhale-web.css` |
| GET | `/assets/codewhale-web.js` |
| GET | `/health` |
| GET | `/mobile` |
| GET | `/mobile/` |
| DELETE / GET / PUT | `/v1/account/model-access` |
| POST | `/v1/agent-mail` |
| GET | `/v1/agent-runs` |
| GET | `/v1/agent-runs/{run_id}` |
| POST | `/v1/agent-runs/{run_id}/cancel` |
| GET | `/v1/agents/external` |
| GET | `/v1/agents/external/{id}/options` |
| GET | `/v1/agents/external/quota` |
| GET | `/v1/approvals` |
| POST | `/v1/approvals/{approval_id}` |
| GET | `/v1/apps/avatars` |
| GET | `/v1/apps/avatars/{handle}/atlases/{page}` |
| GET / POST | `/v1/apps/marketplaces` |
| DELETE / GET | `/v1/apps/marketplaces/{name}` |
| POST | `/v1/apps/marketplaces/{name}/install` |
| POST | `/v1/apps/mcp/desktop-browser` |
| GET / POST | `/v1/apps/mcp/imports` |
| GET / POST | `/v1/apps/mcp/servers` |
| DELETE / GET / PATCH | `/v1/apps/mcp/servers/{name}` |
| POST | `/v1/apps/mcp/servers/{name}/disable` |
| POST | `/v1/apps/mcp/servers/{name}/enable` |
| POST | `/v1/apps/mcp/servers/{name}/reconnect` |
| GET | `/v1/apps/mcp/tools` |
| GET | `/v1/apps/plugins` |
| DELETE / GET | `/v1/apps/plugins/{selector}` |
| POST | `/v1/apps/plugins/{selector}/disable` |
| POST | `/v1/apps/plugins/{selector}/enable` |
| POST | `/v1/apps/plugins/{selector}/restage` |
| POST | `/v1/apps/plugins/{selector}/revoke` |
| ANY | `/v1/apps/plugins/{selector}/routes/{*path}` |
| POST | `/v1/apps/plugins/{selector}/trust` |
| GET | `/v1/apps/plugins/{selector}/ui` |
| GET | `/v1/apps/plugins/{selector}/ui/{*path}` |
| POST | `/v1/apps/plugins/{selector}/update` |
| GET | `/v1/apps/plugins/catalog` |
| POST | `/v1/apps/plugins/catalog/{id}/disable` |
| POST | `/v1/apps/plugins/catalog/{id}/enable` |
| PUT | `/v1/apps/plugins/catalog/{id}/settings` |
| POST | `/v1/apps/plugins/import/dsh/preview` |
| POST | `/v1/apps/plugins/install` |
| GET / POST | `/v1/automations` |
| DELETE / GET / PATCH | `/v1/automations/{id}` |
| GET | `/v1/automations/{id}/missed` |
| PUT | `/v1/automations/{id}/missed/policy` |
| POST | `/v1/automations/{id}/missed/run` |
| POST | `/v1/automations/{id}/missed/skip` |
| POST | `/v1/automations/{id}/pause` |
| POST | `/v1/automations/{id}/resume` |
| POST | `/v1/automations/{id}/run` |
| GET | `/v1/automations/{id}/runs` |
| GET | `/v1/automations/missed` |
| GET | `/v1/changes` |
| GET / POST | `/v1/cloud` |
| POST | `/v1/cloud/attach` |
| GET | `/v1/commands` |
| GET / POST | `/v1/config` |
| POST | `/v1/config/reload` |
| GET | `/v1/constitution` |
| POST | `/v1/constitution/preview` |
| GET | `/v1/crashes` |
| GET | `/v1/crashes/{name}` |
| GET | `/v1/definition` |
| GET | `/v1/diagnostics` |
| GET | `/v1/diff` |
| POST | `/v1/fleet/placement` |
| GET | `/v1/fleet/profiles` |
| GET | `/v1/fleet/profiles/{id}/document` |
| GET / POST | `/v1/fleet/runs` |
| GET | `/v1/fleet/runs/{run_id}` |
| GET | `/v1/fleet/runs/{run_id}/events` |
| GET | `/v1/fleet/runs/{run_id}/events/replay` |
| GET | `/v1/fleet/runs/{run_id}/receipts` |
| GET | `/v1/fleet/runs/{run_id}/receipts/{task_id}` |
| GET | `/v1/fleet/runs/{run_id}/receipts/{task_id}/evidence` |
| POST | `/v1/fleet/runs/{run_id}/start` |
| POST | `/v1/fleet/runs/{run_id}/stop` |
| GET | `/v1/fleet/runs/{run_id}/workers` |
| GET | `/v1/fleet/workers/{worker_id}` |
| POST | `/v1/fleet/workers/{worker_id}/interrupt` |
| POST | `/v1/fleet/workers/{worker_id}/restart` |
| POST | `/v1/fleet/workers/{worker_id}/stop` |
| GET | `/v1/git` |
| POST | `/v1/git/branch` |
| POST | `/v1/git/commit` |
| POST | `/v1/git/discard` |
| GET | `/v1/git/graph` |
| POST | `/v1/git/push` |
| POST | `/v1/git/stage` |
| POST | `/v1/git/unstage` |
| GET | `/v1/goals` |
| GET | `/v1/hooks` |
| GET | `/v1/job-operations` |
| GET | `/v1/jobs` |
| GET | `/v1/logs` |
| GET | `/v1/logs/{name}` |
| GET | `/v1/lsp` |
| DELETE / GET / POST | `/v1/memory` |
| GET | `/v1/memory/{id}` |
| GET | `/v1/modes` |
| GET / PATCH / POST | `/v1/operate` |
| POST | `/v1/operate/auto-merge/check` |
| POST | `/v1/operate/cancel` |
| POST | `/v1/operate/keepalive` |
| PUT | `/v1/operate/plan` |
| POST | `/v1/operate/stop` |
| GET | `/v1/plan` |
| GET | `/v1/plans` |
| GET | `/v1/process` |
| GET | `/v1/providers` |
| POST | `/v1/providers/{id}/connect` |
| DELETE / GET | `/v1/providers/{id}/connect/{attempt_id}` |
| DELETE / PUT | `/v1/providers/{id}/key` |
| POST | `/v1/providers/{id}/key/check` |
| GET | `/v1/providers/{id}/models` |
| POST | `/v1/providers/{id}/models/refresh` |
| POST | `/v1/providers/{id}/switch` |
| GET | `/v1/references` |
| GET | `/v1/remote` |
| POST | `/v1/remote/connect` |
| GET | `/v1/runtime/info` |
| GET / POST / PUT | `/v1/sessions` |
| DELETE / GET / PATCH | `/v1/sessions/{id}` |
| GET | `/v1/sessions/{id}/artifacts` |
| GET | `/v1/sessions/{id}/artifacts/{artifact_id}` |
| POST | `/v1/sessions/{id}/resume-thread` |
| GET | `/v1/sessions/repair` |
| GET | `/v1/sessions/summary` |
| GET | `/v1/settings/schema` |
| GET | `/v1/skills` |
| DELETE / GET / POST | `/v1/skills/{name}` |
| GET | `/v1/skills/{name}/audit` |
| POST | `/v1/skills/{name}/trust` |
| POST | `/v1/skills/{name}/update` |
| POST | `/v1/skills/install` |
| GET | `/v1/snapshots` |
| POST | `/v1/snapshots/{id}/restore` |
| GET / POST | `/v1/ssh` |
| POST | `/v1/ssh/connect` |
| POST | `/v1/stream` |
| GET | `/v1/symbols` |
| GET | `/v1/tailnet` |
| GET / POST | `/v1/targets` |
| POST | `/v1/targets/switch` |
| GET / POST | `/v1/tasks` |
| GET | `/v1/tasks/{id}` |
| POST | `/v1/tasks/{id}/cancel` |
| ANY | `/v1/terminal/{*rest}` |
| POST | `/v1/thread-history/import` |
| POST | `/v1/thread-history/mutate` |
| POST | `/v1/thread-history/operations/lookup` |
| POST | `/v1/thread-history/operations/recover` |
| GET / POST | `/v1/threads` |
| DELETE / GET / PATCH | `/v1/threads/{id}` |
| GET | `/v1/threads/{id}/agent-mail` |
| POST | `/v1/threads/{id}/agent-mail/{message_id}/cancel` |
| POST | `/v1/threads/{id}/agent-mail/{message_id}/deliver` |
| POST | `/v1/threads/{id}/agent-mail/{message_id}/read` |
| DELETE | `/v1/threads/{id}/approval-grants/{grant_id}` |
| POST | `/v1/threads/{id}/commands` |
| DELETE / GET | `/v1/threads/{id}/commands/{operation_key}` |
| POST | `/v1/threads/{id}/commands/builtin` |
| POST | `/v1/threads/{id}/compact` |
| GET | `/v1/threads/{id}/context` |
| GET | `/v1/threads/{id}/events` |
| POST | `/v1/threads/{id}/file-revert` |
| POST | `/v1/threads/{id}/fork` |
| POST | `/v1/threads/{id}/fork-at-turn` |
| DELETE / GET / PUT | `/v1/threads/{id}/goal` |
| POST | `/v1/threads/{id}/goal/block` |
| POST | `/v1/threads/{id}/goal/complete` |
| GET | `/v1/threads/{id}/history` |
| GET | `/v1/threads/{id}/job-operations` |
| GET | `/v1/threads/{id}/job-operations/{operation_key}` |
| GET / POST | `/v1/threads/{id}/jobs` |
| GET | `/v1/threads/{id}/jobs/{job_id}` |
| POST | `/v1/threads/{id}/jobs/{job_id}/kill` |
| GET | `/v1/threads/{id}/jobs/{job_id}/output` |
| POST | `/v1/threads/{id}/jobs/{job_id}/resize` |
| POST | `/v1/threads/{id}/jobs/{job_id}/stdin` |
| POST | `/v1/threads/{id}/members` |
| GET | `/v1/threads/{id}/members/operations/{operation_key}` |
| GET | `/v1/threads/{id}/notices` |
| DELETE | `/v1/threads/{id}/notices/{notice_id}` |
| POST | `/v1/threads/{id}/notifications/prepare` |
| GET / POST | `/v1/threads/{id}/patch-undo` |
| GET | `/v1/threads/{id}/plan` |
| POST | `/v1/threads/{id}/read` |
| GET | `/v1/threads/{id}/receipt` |
| POST | `/v1/threads/{id}/resume` |
| POST | `/v1/threads/{id}/retry` |
| POST | `/v1/threads/{id}/reviews` |
| GET | `/v1/threads/{id}/terminals` |
| POST | `/v1/threads/{id}/terminals/{terminal_id}/input` |
| POST | `/v1/threads/{id}/terminals/{terminal_id}/kill` |
| GET | `/v1/threads/{id}/terminals/{terminal_id}/output` |
| POST | `/v1/threads/{id}/terminals/{terminal_id}/resize` |
| GET | `/v1/threads/{id}/todo` |
| GET | `/v1/threads/{id}/turn-operations/{operation_key}` |
| POST | `/v1/threads/{id}/turns` |
| GET | `/v1/threads/{id}/turns/{turn_id}/artifacts` |
| GET | `/v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}` |
| GET | `/v1/threads/{id}/turns/{turn_id}/calls/{tool_call_id}/changes` |
| POST | `/v1/threads/{id}/turns/{turn_id}/interrupt` |
| GET | `/v1/threads/{id}/turns/{turn_id}/receipt` |
| POST | `/v1/threads/{id}/turns/{turn_id}/steer` |
| POST | `/v1/threads/{id}/turns/{turn_id}/tool-calls/{call_id}/result` |
| POST | `/v1/threads/{id}/undo` |
| GET | `/v1/threads/{id}/usage` |
| POST | `/v1/threads/{id}/watch-turns` |
| GET | `/v1/threads/running` |
| GET | `/v1/threads/summary` |
| GET | `/v1/todo` |
| GET | `/v1/todos` |
| GET / POST | `/v1/triggers` |
| DELETE / GET | `/v1/triggers/{id}` |
| GET | `/v1/usage` |
| DELETE / POST | `/v1/user-input/{thread_id}/{input_id}` |
| GET | `/v1/voice` |
| POST | `/v1/voice/control` |
| POST | `/v1/voice/dictate` |
| POST | `/v1/voice/send` |
| GET / PUT | `/v1/whale/soul` |
| GET | `/v1/workspace/diff` |
| GET / PUT | `/v1/workspace/files` |
| GET | `/v1/workspace/files/read` |
| GET | `/v1/workspace/files/search` |
| GET | `/v1/workspace/instructions` |
| GET | `/v1/workspace/status` |
| POST | `/v1/workspace/trust` |
| GET / POST | `/v1/worktrees` |
| POST | `/v1/worktrees/remove` |

The source contains 243 distinct literal paths in this table.
