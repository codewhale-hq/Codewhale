---
name: help
description: Route a "how do I use Codewhale" question to the installed help, config, and doctor surfaces instead of reciting a manual from memory. Explicit-only.
invocation: explicit-only
---

# Help

## Invocation
Explicit-only. This skill is a router, not a manual. It is deliberately kept
out of the ambient model catalogue so it never spends prompt budget, and it
never restates documentation that the running build already exposes.

## When to use
Load it only when the user explicitly asks how to use Codewhale itself —
a command, a setting, a keybinding, or where a feature lives.

## Non-goals
- Do not paste a manual, a command list, or a settings table into context.
- Do not answer from memory of another harness; Codewhale's surfaces differ.
- Do not guess at flags, config keys, or paths. Read them, or say you did not.

## Routing table
Answer from the surface that owns the fact, in this order:

1. **Slash commands** — `/help` lists the commands this build registers;
   `/help <command>` prints that command's usage line. This is the only
   authoritative command list, because it is generated from the registry.
2. **Skills** — `/skills` opens the manager, `/skills inspect` prints the
   discovery mode, searched directories, and source paths. `/skill <name>`
   activates one. See `docs/SKILLS.md` in a Codewhale checkout.
3. **Configuration** — `/config` is the live settings surface. Config file
   keys are documented in `docs/CONFIGURATION.md`; provider/model routing in
   `docs/PROVIDERS.md`.
4. **Keybindings** — `docs/KEYBINDINGS.md` in a checkout. There is no
   keybinding slash command; do not invent one.
5. **Environment problems** — `codewhale-tui doctor` reports the resolved
   config path, provider credential presence (never values), and workspace
   state. Prefer its output over inference.

## Working in a Codewhale checkout
When the workspace *is* a Codewhale checkout, `docs/` is present on disk and
`File` with `action: "read"` is the right tool. Read the single most relevant file and quote
the specific lines. Outside a checkout, `docs/` is usually absent — in that
case rely on `/help`, `/config`, and `doctor`, and say plainly that the
reference docs are not installed locally.

## Bounds
- One surface per question. Do not sweep `docs/` looking for context.
- If a surface disagrees with your recollection, the surface wins.
- If nothing local answers it, say so and stop; do not invent a flag.
