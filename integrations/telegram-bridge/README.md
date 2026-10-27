# Telegram Bridge

This bridge lets a Telegram chat control a local `codewhale serve --http`
runtime from a phone. It uses Telegram Bot API long polling, so the first
version does not need a public webhook URL or inbound port.

Security model:

- `codewhale serve --http` stays bound to `127.0.0.1`.
- `/v1/*` runtime calls use `CODEWHALE_RUNTIME_TOKEN`. Legacy
  `DEEPSEEK_RUNTIME_TOKEN` is accepted only as a compatibility fallback.
- Telegram chats must be allowlisted unless `TELEGRAM_ALLOW_UNLISTED=true` is
  set for first pairing.
- Direct messages are the intended MVP control surface. Group chat control is
  disabled unless `TELEGRAM_ALLOW_GROUPS=true`.
- Tool approvals are text commands: `/allow <approval_id>` or `/deny <approval_id>`.
- The bridge also sends inline button controls for common actions. Text
  commands remain the fallback.

## Setup

Create a bot with Telegram's `@BotFather`, then configure the bridge:

```bash
cd /opt/codewhale/telegram-bridge
npm install --omit=dev
cp .env.example /etc/codewhale/telegram-bridge.env
sudoedit /etc/codewhale/telegram-bridge.env
node src/index.mjs
```

Validate env files before starting the service:

```bash
npm run validate:config -- \
  --env /etc/codewhale/telegram-bridge.env \
  --runtime-env /etc/codewhale/runtime.env \
  --workspace-root /opt/whalebro \
  --check-filesystem
```

For first pairing, temporarily set `TELEGRAM_ALLOW_UNLISTED=true`, send the bot
`/status`, copy the returned `chat_id` or `user_id` into
`TELEGRAM_CHAT_ALLOWLIST`, then turn `TELEGRAM_ALLOW_UNLISTED=false`.

## Commands

- `/menu`
- `/status`
- `/threads`
- `/new`
- `/resume <thread_id>`
- `/model <name|default>`
- `/interrupt`
- `/answer <response>`
- `/compact`
- `/allow <approval_id> [remember]`
- `/deny <approval_id>`

Anything else is sent as a prompt. If group control is explicitly enabled,
messages must start with `/cw` by default, for example:

```text
/cw check git status and tell me what is dirty
```

The `/menu`, `/status`, `/threads`, active-turn, and approval messages include
tap targets for common actions. Approval buttons map to the same runtime API as
`/allow` and `/deny`; they do not enable blanket auto-approval unless you tap
the explicit "Allow + remember" button.


## Restart access checks

Recovered deliveries require a saved, previously admitted sender identity that
still matches the current allowlist and group policy. Removing access and
restarting the bridge also stops recovery messages to that chat. Legacy state
without this identity stays detached; it does not cancel the runtime turn.
Use the Runtime to control a turn without a recorded initiating human. A fresh admitted
message records identity for subsequent restart recovery. Keep allow-unlisted
mode off when operating with private workspace or runtime data.


## Durable admission and questions

The bridge saves each prompt, its Telegram message identity, initiating human,
and frozen Engine request in the existing thread map before advancing the
Telegram polling cursor. It uses Engine operation keys only when the Runtime
advertises both turn-operation idempotency and lookup. After an uncertain
submission, it looks up that original operation; a missing operation permits
retrying the same request and key, while an incomplete or unavailable receipt
stays retained. An Engine without these capabilities never receives an automatic
retry of an uncertain submission. `/status` reports retained admission state.
Changing the bot account, Runtime URL, allowlist, or group policy does not move
an existing pending request to another identity or Runtime.

A disconnected event stream reconnects to the accepted turn with its saved
cursor and accumulated reply. `/new`, `/resume`, and `/compact` cannot replace
a pending turn or retained final reply. Final replies are saved before sending.
An uncertain send is not automatically retried: the initiating human can retrieve
the saved reply with `/status`, which warns that the earlier delivery may have
succeeded. Telegram rate-limit refusals permit bounded retries. Progress updates
remain best effort; no send failure authorizes another Engine turn. This bridge does not provide a
cross-process lease for multiple instances sharing a state file. Run one bridge
process per bot token and state file.

When the Engine asks a question, reply with `/answer` followed by an option
number or text, or send the answer as a normal message. Answer multiple questions
on separate lines. The bridge checks the current Runtime question, turn, and
recorded initiating human before submitting an answer. Other group members
cannot answer, approve, or interrupt that person's turn. Group thread lists and
resume controls stay within that group's current conversation.
