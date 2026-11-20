# CodeWhale for VS Code

Official Codewhale extension: an agentic chat sidebar over the local Engine
Runtime API, with editor context, streaming turns, approvals, and terminal
parity.

## What it does

**Chat sidebar** (primary view):

- create, switch, and resume Codewhale threads; every thread stays available
  from the terminal and the embedded browser client
- stream turns live over the runtime's replayable SSE contract
  (`GET /v1/threads/{id}/events?since_seq=…`) with automatic reconnection
- attach editor context as chips before sending: current selection,
  active file, or Problems-panel diagnostics
- resolve tool approvals (allow / deny / remember) and clarification
  questions inline, hydrated from the thread-detail snapshot so a reload
  never strands pending work
- steer a running turn or stop it
- render agent replies as a safe Markdown subset; every code block gets
  Copy and Insert-at-cursor actions
- open changed files from `file_change` items when the runtime includes a path

**Runtime view** (secondary): connection state, recent thread summaries,
restore points, and the original terminal launch helpers.

**Connection**: the extension attaches to `codewhale serve --http` on
`127.0.0.1:7878` by default, starts it in a visible terminal on request, and
never runs its own agent engine — the runtime is the single turn/event owner.

Editors that speak the Agent Client Protocol, such as Zed, do not need this
extension: see [Use Codewhale from your editor](https://github.com/codewhale-hq/Codewhale/blob/main/docs/ACP.md).

## Security posture

- Runtime bearer tokens are stored in VS Code SecretStorage via
  **CodeWhale: Set Runtime Token**; the legacy `codewhale.runtimeToken`
  setting still works and is migrated into secret storage on first use.
- The webview renders with a strict CSP (`default-src 'none'`), and all
  model output is HTML-escaped before any Markdown transform runs; links
  must be http(s).
- The chat webview script is a static string — no runtime data is
  interpolated into it.

## Local use

```bash
npm install
npm test                                # compile + unit tests
npm run package                         # -> codewhale-vscode-<version>.vsix
code --install-extension codewhale-vscode-<version>.vsix
```

Settings: `codewhale.commandPath`, `codewhale.runtimeHost`,
`codewhale.runtimePort`, `codewhale.agentViewRefreshIntervalSeconds`
(`0` disables automatic refresh). Commands: **CodeWhale: Ask Codewhale**
(`ctrl+alt+c` from the editor, also on the editor context menu),
**CodeWhale: New Chat**, **CodeWhale: Set Runtime Token**,
**CodeWhale: Start Local Runtime**.

Keep the runtime on `127.0.0.1` unless you deliberately front it with trusted
local networking controls.

## Not yet built

VS Code-native diff/merge review of agent file changes (blocked on the
runtime publishing a Files/Changes contract), provider/model switching from
the composer, retry/undo/restore buttons, and account sign-in surface. The
runtime's embedded browser client (`codewhale web`) remains the full-feature
fallback for those flows.
