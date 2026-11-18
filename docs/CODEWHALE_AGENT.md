# Codewhale Agent: one identity, several surfaces

"Codewhale Agent" is the name Codewhale acts under when it works somewhere
other than your terminal: a pull-request review on GitHub, a commit credit, a
chat bot. This page lists those surfaces and the rules they share.

## Surfaces

| Surface | Appears as | Set up |
| --- | --- | --- |
| Pull-request review | The GitHub App bot `codewhale-agent[bot]`, or the workflow's own token when no App is configured | [GitHub review bot](GITHUB_ACTION.md), [GitHub App setup](GITHUB_APP.md). `codewhale review --pr N` prints a review locally; it is published only with `--post`. |
| Commit co-author | The contributor named in `.github/AUTHOR_MAP` | Maintainers keep the map; harvested work is credited to its author. |
| Chat bots | A bot you register and pair with your Codewhale account | The open-source bridges under `integrations/` (Telegram, Feishu, DingTalk, WeCom, Weixin) connect a chat to a local `codewhale serve --http`. Account-paired Telegram and Slack bots depend on the deployed account service. |

Slack bot commands control Codewhale. They do not give an agent tools to
search or read a Slack workspace; for that, configure a Slack MCP server
([MCP](MCP.md)). The Slack bot needs a Slack app, credentials and webhook that
you manage, and a hosted Slack delivery has not been qualified, so treat it as
source, not as a ready integration.

Personal WeChat is supported only through Tencent's official ClawBot (iLink)
channel, never through unofficial account automation. The
`integrations/weixin-bridge` bridge uses it with a local
`codewhale serve --http` and stops when Tencent's risk control objects.
Official Account (公众号/服务号) APIs are not used.

## Rules every surface follows

- One engine. Every surface is input and output around the same turn loop
  (`crates/engine/src/core/engine/turn_loop.rs`). No surface has its own
  engine.
- One account. Account-backed surfaces authenticate to the Codewhale account
  you sign in to with `codewhale login`; see [Account](ACCOUNT.md).
- Approvals are decisions, not messages. An approval from any surface is
  routed back as a permission decision to the same engine. A plain chat
  message never runs an action that needs approval.
- Commands and approvals are recorded on every channel.

## Chat bot behaviour

- Commands are allowlisted per channel. The default set is read-only:
  `status`, `jobs`, `receipts`, `help`. `new task`, `approve` and `deny` are
  opt-in.
- Approve and deny buttons act as permission decisions.
- The bot can be set to message only when something changed, to observe quiet
  hours and to send a digest.
- A channel is paired with one code issued inside the signed-in app.
  Unpairing revokes it at once.

## Naming

- Product: Codewhale. Agent surface: "Codewhale Agent".
- Bot handle, wherever it can be chosen: `codewhale-agent`.
- In the cloud the agent introduces itself as "Codewhale cloud agent", not by
  a model provider's name.
