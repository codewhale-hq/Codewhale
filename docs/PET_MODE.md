# The whale view

> 阅读简体中文版：[zh_hans/PET_MODE.md](zh_hans/PET_MODE.md)

`/whale` turns the terminal you already use into a glanceable status view. A
whale made of dots fills the main view and shows what the session is doing:
idle, working, waiting for you, done, or failed. The message box stays at the
bottom, so you can keep typing. Replies and agents are one key away.

Each terminal draws its own whale from its own session. Nothing else runs: there
is no background process, no port and no shared state. With several terminals
open, you can look across them and see which one needs you, which is still
working, and which has finished.

```text
Codewhale   ● working
                         (the whale)
                              ●
F5 replies & agents · /whale standard view
╭──────────────────────────────────────────────────────────╮
│❯ Type a follow-up…                                       │
╰──────────────────────────────────────────────────────────╯
 ● auto  Shift+Tab to change   work (Tab)   2 agents
```

## Commands

| Command | Effect |
| --- | --- |
| `/whale` | Turn the whale view on or off for this terminal. `/whale on` and `/whale off` set it without toggling. |
| `/whale full` | The same as `/whale`. In the terminal the whale view is already the full view; the word exists so the command reads the same as in the desktop app. |
| `/whale still` | Hold the whale still, or let it move again. |
| `/whale name <text>` | Name the whale. The name replaces `Codewhale` in the view's first line. Up to 32 characters. |
| `/whale color <#rrggbb>` | Colour the whale. Each dot keeps its own brightness. `/whale color default` restores the whale's own colours. |

`/pet` still runs `/whale`, with the same words after it, so old habits and
scripts keep working. It is not listed in help or completion.

The view being on or off is saved as the `pet_mode` setting (also in the
settings editor), so a new session starts the way you left it. The name, the
colour and hold-still last until this terminal closes; they are not saved yet.

The whale view never starts a turn, calls a model, or changes your draft, mode,
or approvals. It only presents the session you are already running.

## What the whale tells you

The first line of the view and the mark under the whale always carry the state
as a mark and a word, so it reads without colour or motion:

| State | Mark and word | Meaning |
| --- | --- | --- |
| Ready | `○ idle` | Nothing running. |
| Working | `● working` | A turn is running, including tools and verification. |
| Needs you | waiting or approval phase | The session is waiting for an approval or an answer. Motion is reduced while it waits. |
| Done | `✓ Done` | The last turn completed. |
| Failed | `✕ failed` | The last turn ended in failure. |

While a turn runs, the whale's dots leave it and become the thing the work is,
in motion, then return: a wrench for commands and tools, a pencil for edits, a
book for reading, a lens for searching, a flask for tests, a globe for
browsing, a pointer for computer use, stacked layers for memory, a bulb for
thinking, a speech bubble for replies, a pod of small whales for parallel
agents, a ringing bell when it needs you and a check mark when it is done.
Hold-still and reduced motion show one still pose of the form.

The status bar underneath keeps its usual live readouts, such as the agent
count (`2 agents`) and the work bar. Transient notices, such as a retry
(`Retry attempt: transport 1/3; network error; waiting 0.94s`), show on the
second line.

## Replies and agents on demand

The main view stays quiet. Press `F5` while the whale view is on, or click the
hint line, to open the inspector over it:

- the whale and an **Agents** list on top, with each agent's state and usage;
- a **Reply** pane below, which streams the current answer and shows terminal
  errors from the same session.

| Key | Effect |
| --- | --- |
| `F5` | Open replies and agents. |
| `Esc` | Back to the message box. Your draft, the transcript, and any running turn are untouched. |
| `Tab` | Switch between the Reply and Agents panes. |
| `Up` / `Down`, `PgUp` / `PgDn`, `Home` / `End` | Scroll the focused pane. |
| `Enter` (Agents pane) | Open the selected agent's transcript. |
| `c` | Copy the last finished reply, through the same path as `/copy`. |

On a Mac keyboard the top row sends media keys unless Fn is held, so F-key
hints throughout the TUI, including help, read `fn+F5`; press Fn with the key.

## Several terminals, one whale each

Start Codewhale in two terminals and run `/whale` in each: each whale follows
its own session, so one can show `✓ Done` while the other shows `● working`.
Nothing is mixed between sessions, and the terminals do not talk to each other.
The desktop app draws the same whale from the same code, and shows its own
sessions.

## When the dots cannot draw

- **`CODEWHALE_ASCII_SAFE=1`.** Marks, borders, and the pointer use ASCII
  (`o idle`, `|`, `>`). The whale is replaced by the state mark and word, and
  the line under the title says so.
- **Small terminals.** The dot whale needs an art area of at least 16x8 cells.
  Below that the mark, word, hint line, and message box remain.
- **The dot whale could not start.** The view draws the half-block whale
  instead, and the line under the title says so. The state mark and word keep
  working, because they come from your session.
- **`NO_COLOR`.** Colour is left to your terminal; the marks and words still
  carry the state.
- **Reduced motion.** Follows the shell's motion setting and shows a still pose.

## Privacy

The whale is drawn inside the terminal process. What it reads from the session
is a short allowlist of event kinds, ids, and outcomes (for example "a read
started", "turn completed"). Prompt text, reasoning, tool names, arguments,
results, and paths are not read, nothing is written to disk or sent anywhere,
and nothing about the session's model context changes.

## What was removed in 0.10.5

Earlier versions ran a shared companion process (`codewhale pet serve`) on a
loopback port. It is gone, with everything only it provided:

- sound (`/pet sound`, `F6`);
- replay recording and `/pet export`;
- the browser appearance studio (`/pet appearance`, `F8`) and `/pet style`;
- source selection (`/pet source`) and `/pet status`;
- the other views (`cove`, `braille`, `pixels`, `particles`, `whale-girl`),
  view cycling, `/pet avatar`, `/pet action`, `/pet view`, and avatar packs in
  the terminal;
- `/pet inspect`; use `F5`.

`CODEWHALE_PET_HOME`, `CODEWHALE_PET_PORT` and `CODEWHALE_PET_GRAPHICS` are no
longer read. Files an older version left in `~/.codewhale/pet-shared` are not
read and are not deleted; you can remove that folder yourself.

## Known limitations

- **No "reply ready" cue yet.** When a turn finishes, the whale shows `✓ Done`
  and the footer hint stays the same static line. There is no separate
  notification, badge, or sound that says a reply is waiting. Glance at the
  state word, or press `F5` to read the reply.
- **Agent rows can lag.** While agents run, the Agents list in the inspector is
  refreshed when the turn ends, not on every change. The agent count in the
  status bar and the work bar are live; treat the inspector's rows as a
  snapshot until the turn finishes.
- **The whale starts unobserved.** Events from before the first `/whale` in a
  Codewhale process are not shown; the whale rests until the next one arrives.
- **Name, colour and hold-still are not saved**, and are not shared with other
  terminals or the desktop app.
- **A colour replaces the activity hues.** The shape, the mark and the word
  still say what the session is doing.
- **The work bar is hidden while the whale view is showing.** Turn the view
  off with `/whale` to use it.

See also [Keybindings](KEYBINDINGS.md), [Modes](MODES.md), and the design notes
in [`design/DOT_WHALE_PET.md`](design/DOT_WHALE_PET.md).
