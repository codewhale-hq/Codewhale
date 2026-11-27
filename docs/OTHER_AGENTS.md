# Run other agents in Codewhale

> [简体中文](zh_hans/OTHER_AGENTS.md)

An external-agent session runs on an agent CLI installed on the computer
hosting Codewhale. Codewhale keeps the thread, transcript and approval
history; the other agent runs the turn and uses its own account, model and
tools. The connection uses [Agent Client Protocol (ACP)](https://agentclientprotocol.com/).
This is not a Codewhale provider or sub-agent. To connect an editor **to**
Codewhale instead, see [Codewhale as an ACP agent](RUNTIME_API.md#acp-stdio-adapter-codewhale-serve---acp).

## Install and sign in

1. Install an agent on the computer that runs Codewhale, not just on the
   device you connect from. See the
   [agent registry](../crates/engine/src/fleet/agents.rs) for recognized agents,
   minimum versions and upstream install links.
2. Sign in or configure a model in the agent's own CLI. For example, run
   `opencode auth login` for OpenCode. A provider key saved in Codewhale is
   not passed to the other agent.
3. Make sure the agent executable is on the server's `PATH`. For Claude Code,
   Codex or Muse Code, also install the corresponding ACP adapter and put it
   on `PATH`. Codewhale does not download either executable.

For Google personal accounts, use Antigravity CLI. The Gemini CLI entry can
discover an existing installation with supported access. [Google account migration](https://developers.google.com/gemini-code-assist/docs/deprecations/code-assist-individuals)

| Agent | Install the adapter |
| --- | --- |
| Claude Code | `npm i -g @agentclientprotocol/claude-agent-acp` |
| Codex | `npm i -g @agentclientprotocol/codex-acp` |
| Muse Code | `npm i -g @bex-co/muse-code-acp` |

## Start a session

### From the terminal

Open `/model` in the TUI to choose either a Codewhale model or an installed external ACP agent under **Other agents (ACP)**. Agents that are installed but not ready show why (for example, a missing adapter or older version) alongside their sign-in command.

For scripts or direct launch, pass `--agent <id>`:

```bash
codewhale --agent opencode
codewhale exec --agent opencode "Summarize this project without changing files."
```

### From the Runtime API

You can also start the session directly through the Runtime API. In a Bash shell
inside the project folder, start the local server. Keep the token private.

```bash
export CODEWHALE_RUNTIME_TOKEN="$(openssl rand -hex 32)"
codewhale app-server --http --host 127.0.0.1 --port 7878 &
server_pid=$!

base=http://127.0.0.1:7878
api() { curl -sS --fail-with-body -H "Authorization: Bearer $CODEWHALE_RUNTIME_TOKEN" "$@"; }
# Run after the server has started.
api "$base/v1/agents/external"
```

To list another installed ACP agent, export `CODEWHALE_CUSTOM_ACP_AGENTS`
in the shell that starts Codewhale. Give it a JSON array of commands. Each
entry needs a unique `id`, a display `name` and a `binary` on PATH or at an
absolute path. Put command arguments in `args` as separate strings.
For example, `[{"id":"local-agent","name":"Local agent","binary":"local-agent","args":["acp"]}]`
adds a local command to discovery. Keep credentials in the agent's own login.

Pick an `id` whose row says `"runnable": true`. This confirms discovery, not
sign-in. The example below uses OpenCode and `jq` to build the JSON safely,
including when the project path contains spaces.

```bash
agent=opencode
thread=$(api "$base/v1/threads" -H 'Content-Type: application/json' \
  -d "$(jq -n --arg workspace "$PWD" --arg agent "$agent" \
    '{workspace:$workspace, permission_posture:"ask", executor:{kind:"acp", agent_id:$agent}}')")
thread_id=$(jq -er .id <<< "$thread")
api "$base/v1/threads/$thread_id/turns" -H 'Content-Type: application/json' \
  -d '{"prompt":"Summarize this project without changing files."}'
api -N "$base/v1/threads/$thread_id/events"
```

The event stream stays open for updates. Stop watching with Ctrl+C; this does
not cancel the turn. Inspect the turn and any pending approvals at
`GET /v1/threads/{id}`. To answer a pending approval, send `{"decision":"allow"}`
or `{"decision":"deny"}` to `POST /v1/approvals/{approval_id}`. To cancel a
running turn, use `POST /v1/threads/{id}/turns/{turn_id}/interrupt`.
Stop the server when done with `kill "$server_pid"`. To reopen a session,
restart the server with the same Codewhale and agent state and call
`POST /v1/threads/{id}/resume` with the saved thread ID.

Leave the model unset to use the agent's default. To pick a model when an
agent offers that choice, query `GET /v1/agents/external/{id}/options?cwd=...`
with the URL-encoded absolute project path and set `executor.config.model`
to the agent's offered value when creating the thread. It is not a Codewhale
model ID.

## What Codewhale controls

- **Approvals:** Ask displays requests the agent sends through ACP. If an
  agent has its own allow rules, it may act without sending a request.
  Codewhale does not sandbox the agent's tools. Configure permissions in the
  agent itself and only run agents you trust.
- **Resume:** Codewhale reopens the same agent session when the agent supports
  ACP resume or load. Otherwise it reports that it cannot reopen the session;
  it does not start a replacement.
- **Limits:** External sessions do not support steering, compaction, forks,
  image input or per-turn model changes through Codewhale. Usage is shown only
  when the agent reports it; Codewhale does not know the cost.

## If the agent won't start

| Code | What to do |
| --- | --- |
| `not_installed` | Install the CLI and check the server's `PATH`. |
| `adapter_missing` | Install the required ACP adapter and check the server's `PATH`. |
| `below_min_version` | Update the CLI to the version shown or newer. |
| `no_acp` | Check the CLI or adapter version and whether it can start in ACP mode. |
| `needs_sign_in` | Sign in through the agent's own CLI, then retry. |

For request fields, permission policies and events, see the
[Runtime API contract](RUNTIME_API.md#sessions-on-other-agents-external-agent-threads).
