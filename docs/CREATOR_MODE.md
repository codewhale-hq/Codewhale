# Creator mode

Creator mode is a conversation mode for building and testing plugins. A
Creator mode conversation starts with the plugin tools and the plugin-writing
skills already loaded, so you can describe the plugin you want and have
Codewhale write it, install it in this project and try it.

The everyday mode is called Standard. A conversation is one or the other, and
the choice is fixed after its first message.

To write a plugin by hand, start with
[Write your first plugin](PLUGIN_AUTHORING.md). What a plugin can contain is in
[Plugins](PLUGINS.md).

## Before you start

- Creator mode needs a plugin runtime: Node.js or Bun. Without one, the mode
  is listed as unavailable with that reason.
- The `plugin_creator` feature is beta and on by default. Setting
  `plugin_creator = false` under `[features]` turns the mode off; see
  [Configuration](CONFIGURATION.md).
- A session that runs another agent as its executor uses that agent's own
  tools and instructions, so conversation modes are unavailable there.

## Start a Creator mode conversation

| Where | How |
| --- | --- |
| A new conversation | `/new creator` (`--force` still combines) |
| A blank conversation | `/mode creator`, or pick Creator mode in the first group of the `/mode` picker |
| The shell | `codewhale --conversation-mode creator`, also on `codewhale exec` |

After the first message the mode is locked: the picker shows the group as
locked and `/mode creator` is refused with a note to start a new conversation.
`/new` alone starts a Standard conversation.

Resume, `/load`, `/fork` and `/clear` keep the conversation's saved mode. The
footer shows the mode name when it is not Standard. With `--resume`,
`--session-id` or `--continue`, `--conversation-mode` must match the session's
saved mode, or the run exits with code 2 before any model call.

## What it loads

A Creator mode conversation always loads three tools, `plugin_manager`,
`cordis_inspect_list` and `cordis_inspect_query`, together with the creator
skills. A Standard conversation does not.

## Permissions

A mode never changes a permission. Creator mode in Plan is still Plan, and
every plugin change passes the same gate as any other action.

Read-only `plugin_manager` queries run under every permission setting without
a prompt.

Calls that change plugin state (`install_bundle`, `remove_bundle`,
`set_bundle`, `set_plugin`) are prepared read-only and gated by the engine
before they run:

| Change | Decided by |
| --- | --- |
| A local change inside the workspace | The session's permission setting: an approval card in Ask and Accept Edits, the reviewer and allow rules in [Auto-Review](AUTO_REVIEW.md), and it runs in Full Access. The plugin state records that the agent made the change. |
| A path outside the project | You, on the approval card, even in Auto-Review and Full Access. |
| A remote spec, or a local bundle with unresolved npm dependencies | You. Nothing is fetched before approval, and the bundle lands disabled and untrusted. |
| A bundle that declares stdio MCP connectors or hooks while shell execution is disabled | You. |
| A plugin you approved yourself, or a built-in switch | You. |

Changing plugin calls are refused in Plan, with `sandbox_mode = "read-only"`,
under the `never` permission, in background runs, in sub-agents and inside
`execute_tools`.

## Related pages

- [Modes and permissions](MODES.md)
- [Plugin bundles](PLUGIN_BUNDLES.md) for review, trust and activation
- [Writing an extension tool](EXTENSIONS.md) for the plugin runtime
