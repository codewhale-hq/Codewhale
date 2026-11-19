# Use Codewhale from your editor

`codewhale serve --acp` runs Codewhale as an agent for editors that speak the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP), such as Zed.
The editor starts the command and talks to it over standard input and output;
no network port is opened. Conversations you start in the editor are ordinary
Codewhale conversations: they are saved in the same store and you can resume
them from the terminal.

For VS Code, use the [Codewhale extension](../extensions/vscode/README.md)
instead. It connects to the local Runtime API over HTTP, not ACP.

## Set up

1. Install Codewhale and connect a model in the terminal first
   ([Install](INSTALL.md), [Providers](PROVIDERS.md)). The editor uses the
   same configuration and credentials.
2. Add Codewhale as a custom agent in the editor. In Zed, in `settings.json`:

   ```json
   {
     "agent_servers": {
       "Codewhale": {
         "type": "custom",
         "command": "codewhale",
         "args": ["serve", "--acp"]
       }
     }
   }
   ```

   The exact shape of this entry belongs to the editor and can change between
   editor versions; check its ACP documentation if the entry is rejected. What
   Codewhale needs is the command `codewhale` with the arguments `serve --acp`.
3. Open the editor's agent panel and start a conversation with Codewhale.

If the editor cannot find `codewhale`, give the full path that
`command -v codewhale` prints.

## What works

- Start a conversation, list saved conversations and load one. Loading replays
  the saved user and agent text and each tool call with its final status.
- Send a prompt and cancel it.
- Choose the model from the models your configured provider offers.
- Switch between Work and Plan. Plan is read-only.
- Approve or deny a tool call when Codewhale asks.

The agent can read and search files, use Git, apply patches and, when the
editor provides a terminal and your configuration allows shell commands, run
foreground shell commands. From 0.10.3 it can also use reviewed plugin tools
and the MCP servers you configured in Codewhale.

## Permissions

The permission setting comes from your Codewhale configuration and the launch
command. The editor can see it but cannot widen it.

| You want | Do this |
| --- | --- |
| Ask before changes (default) | Nothing. A configured Accept Edits setting is also served as Ask here. |
| Auto-Review | Set it in your Codewhale configuration; see [Auto-Review](MODES.md). |
| Full Access | Start with `codewhale serve --acp --full-access`, or set `yolo = true` in config. |
| Read-only | Set `sandbox_mode = "read-only"`; only Plan is offered. |

Each approval is for that one call. The editor cannot grant a remembered
permission. Plan stays read-only even under Full Access. Your typed permission
rules, hooks and repository rules apply as they do in the terminal; see
[Modes and permissions](MODES.md) and
[Authorization order](AUTHORIZATION_ORDER.md).

At startup Codewhale writes one line to standard error naming the permission
and sandbox it will run with. Look for it in the editor's agent log when a
launch command does not behave as you expected.

`approval_policy = "never"` is not supported on this transport; the command
refuses to start with it.

## Limits

- MCP servers supplied by the editor are refused. Configure MCP servers in
  Codewhale ([MCP](MCP.md)) instead.
- There is no tool search on this transport, so every connected MCP tool is
  sent with every request, and a server marked `lazy = true` never starts. A
  large MCP set costs tokens on each turn and can exceed a provider's tool
  limit.
- Background shell tasks, the interpreter and sub-agents are not available
  from the editor.
- A prompt runs for at most 50 model steps, or fewer if you configured a lower
  limit. A goal does not continue on its own after the prompt ends.
- Reasoning and images are not replayed when you load a saved conversation.
  Text the editor shows may be shortened; the saved history is complete.

## When Codewhale is already running

If the desktop app or `codewhale serve --http` already owns the conversation
store, `serve --acp` attaches to it and the editor works from the same store.
If that owner exits, the editor's Codewhale process ends with an error naming
the exit; start a new conversation after restarting it. A second
`codewhale serve --acp` on a store another ACP server already owns refuses to
start.

## Reference

The wire behaviour (methods, frame limits, error codes, replay and
cancellation) is in
[Runtime API: ACP stdio adapter](RUNTIME_API.md#acp-stdio-adapter-codewhale-serve---acp).
