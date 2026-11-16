# Feishu / Lark Bridge

This bridge lets a Feishu or Lark chat control a local `codewhale serve --http`
runtime from a phone. It uses the official Lark/Feishu Node SDK long-connection
mode, so the first version does not need a public webhook URL.

Security model:

- `codewhale serve --http` stays bound to `127.0.0.1`.
- `/v1/*` runtime calls use `CODEWHALE_RUNTIME_TOKEN`.
- Feishu/Lark chats must be allowlisted in `CODEWHALE_CHAT_ALLOWLIST` unless
  `CODEWHALE_ALLOW_UNLISTED=true`
  is set for first pairing.
- Direct messages are the intended MVP control surface. Group chat control is
  disabled unless `FEISHU_ALLOW_GROUPS=true`.
- Tool approvals are text commands: `/allow <approval_id>` or `/deny <approval_id>`.
- Feishu/Lark only sees the prompts, status, thread summaries, and approval
  messages the bridge sends. The workspace, shell, and runtime HTTP listener
  stay local behind the Codewhale runtime token.

## Setup

```bash
cd /opt/codewhale/feishu-bridge
npm install --omit=dev
cp .env.example /etc/codewhale/feishu-bridge.env
sudoedit /etc/codewhale/feishu-bridge.env
node src/index.mjs
```

Validate the env files before starting the service:

```bash
npm run validate:config -- \
  --env /etc/codewhale/feishu-bridge.env \
  --runtime-env /etc/codewhale/runtime.env \
  --workspace-root /opt/whalebro \
  --check-filesystem
```

For first pairing, temporarily set `CODEWHALE_ALLOW_UNLISTED=true`, send the
bot `/status`, copy the returned `chat_id`, `open_id`, or `union_id` into
`CODEWHALE_CHAT_ALLOWLIST`, then turn `CODEWHALE_ALLOW_UNLISTED=false`.

For a Tencent Lighthouse deployment, use:

```bash
sudo systemctl enable --now codewhale-runtime codewhale-feishu-bridge
sudo journalctl -u codewhale-feishu-bridge -f
```

## Commands

- `/status`
- `/threads`
- `/new`
- `/resume <thread_id>`
- `/model <name|default>`
- `/interrupt`
- `/compact`
- `/allow <approval_id> [remember]`
- `/deny <approval_id>`

Anything else is sent as a prompt. If group control is explicitly enabled,
messages should start with the Codewhale prefix `/cw`, for example:

```text
/cw check git status and tell me what is dirty
```


## Thread ownership

A thread belongs to the chat that created it. The bridge records the owner in
the thread map when it creates a thread. `/threads` lists only the threads this
chat created, and `/resume` accepts only an ID this chat created. Any other ID,
including a thread started from the desktop app, the TUI or another chat, gets
one fixed reply and is never sent to the Runtime.

When an existing thread map is first loaded, each chat keeps only the thread it
is bound to, and only after the bridge asks the Runtime once whether that thread
was started by this bridge in the configured workspace. A thread started from
the desktop app, the TUI or another client is dropped from the chat, and so is a
thread that several chats point at. Those chats send `/new` to start a new
thread. If the Runtime cannot be reached at start, the bridge retries every 30
seconds and meanwhile gives the fixed reply. An existing thread map records only
the thread each chat is bound to, so threads a chat created earlier and has
since left with `/new` are no longer listed or resumable. Review the thread map
after upgrading.

## Restart access checks

Recovered deliveries require a saved, previously admitted sender identity that
still matches the current allowlist and group policy. Removing access and
restarting the bridge also stops recovery messages to that chat. Legacy state
without this identity stays detached; it does not cancel the runtime turn.
Use an authorized chat to inspect or interrupt that turn. A fresh admitted
message records identity for subsequent restart recovery. Keep allow-unlisted
mode off when operating with private workspace or runtime data.
