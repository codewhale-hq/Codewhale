# Cloud computer dispatch (preview)

Cloud conversations run on a computer in your Codewhale account. The terminal
and desktop open the same conversation ID; closing either view leaves the
task running. Availability depends on your account and the deployed service.

## Start a conversation

Sign in ([Account](ACCOUNT.md)), configure your model in Codewhale, and
dispatch:

```sh
codewhale login
codewhale dispatch "your task"
printf 'Review the failing checks' | codewhale dispatch
```

Dispatch selects your only running account computer and the configured model.
When several computers are running, use `--computer COMPUTER_ID`. List them with
`codewhale account computers list`. Start and create operations use the account's existing
compute allowance, consent and admission checks. To create a computer, use
`codewhale account computers create --help` or Manage cloud computers in desktop.
Dispatch does not allocate or start a computer implicitly.

## Continue, inspect and stop

The returned conversation appears in desktop history for the same account.
Desktop's **Continue in terminal** control shows the matching command:

```sh
codewhale dispatch --thread CONVERSATION_ID
codewhale dispatch --thread CONVERSATION_ID "continue the task"
codewhale dispatch --thread CONVERSATION_ID --stop
codewhale dispatch --thread CONVERSATION_ID --json
```

Inspecting prints messages and pending approvals. Use `--approve APPROVAL_ID`
or `--deny APPROVAL_ID` after reviewing the request; approvals apply once.
Answer pending questions in desktop. `--stop` interrupts the turn and leaves
the computer running. Use `account computers pause COMPUTER_ID` to stop compute.

## How the model key reaches the computer

Dispatch resolves your configured model through Codewhale's shared config and
credential resolver. A portable API key is supplied to the selected Engine in
memory, bound to the same provider endpoint. Restarting the Engine clears it.
The receiving Engine must support endpoint-bound session credentials; an older
Engine refuses this handoff. The desktop uses the same CLI resolver through its
native bridge, so credentials never pass through the webview.

Device-bound sign-ins, custom header authentication and local-only inference
cannot currently be transferred this way. Select a portable HTTPS API-key
connection, or configure the computer's existing connection explicitly.
`--key-stdin` remains available for explicit memory-only credential entry.
Do not put keys in command arguments. This does not enable the account key vault.

`--cloud-provider PROVIDER --cloud-model MODEL` explicitly selects the computer's
existing connection. Without those overrides, a follow-up checks that your
configured model still matches the conversation before transferring a key.
`--configured-model` reports the selected model and whether it is portable,
without returning a credential. `--prepare-model --thread ID` reconnects that
model without starting a turn.

## Named agents (whales)

A whale is a named account collaborator with a saved provider and model.
`codewhale account agents create NAME --provider PROVIDER --model MODEL
--operation-key KEY` saves one without starting work or a computer, and
`codewhale account agents list` shows each whale's saved model.
`codewhale dispatch --whale AGENT_ID "your task"` starts a cloud conversation
with that whale: the service takes the route from the whale's saved
configuration, and your local configuration for that provider supplies the
memory-only key. Later `--thread` follow-ups keep using the whale's current
saved model; a changed whale applies to the next turn, not to a running one.
In a group conversation, a send or `--prepare-model` also connects the provider
of each active member whose saved provider differs from the lead's, from your
local configuration for that provider, memory-only like the lead's key. A
member whose provider is not configured locally stops the send with that
member's name; it is never skipped.

## Retry after an uncertain response

Every send prints a replay key before submission. After an uncertain response,
repeat the original command and prompt with `--operation-key KEY`. Keep the
original `--computer` target when retrying creation, or the original `--thread`
when retrying continuation. Reusing the key with changed content is refused.
Do not start another conversation to recover an uncertain send.

## Related commands and limits

`codewhale account session` exposes the same computer conversation commands.
The earlier `dispatch` Agent Work flow, including its repository and file
context options, remains available when an Agent Work option such as `--agent`
or `--context-file` is supplied.
Work-only flags cannot be mixed with computer conversations. This preview does
not move local folders or repository checkouts into a VM.

Conversations on one computer share that computer's files. Select a separate
computer when you need filesystem isolation.
