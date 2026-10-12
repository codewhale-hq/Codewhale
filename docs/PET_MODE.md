# Pet mode

> 阅读简体中文版：[zh_hans/PET_MODE.md](zh_hans/PET_MODE.md)

Pet mode turns the terminal you already use into a glanceable status view. A
whale fills the main view and shows what the session is doing: idle, working,
waiting for you, done, or failed. The message box stays at the bottom, so you
can keep typing. Replies and agents are one key away.

Each terminal runs its own pet mode for its own session. With several terminals
open, you can look across them and see which one needs you, which is still
working, and which has finished.

```text
Codewhale   ● working
                         (the whale)
                              ●
F5 replies & agents · /pet off standard view
╭──────────────────────────────────────────────────────────╮
│❯ Type a follow-up…                                       │
╰──────────────────────────────────────────────────────────╯
 ● auto  Shift+Tab to change   work (Tab)   2 agents
```

## Turn it on

```text
/pet on        # pet mode becomes the main view
/pet off       # back to the standard view
/pet           # toggle
/pet status    # on or off, whether the habitat is open, and companion identity
```

`/pet on` and `/pet off` are saved as the `pet_mode` setting (also in the
settings editor), so a new session starts the way you left it. Pet mode never
starts a turn, calls a model, or changes your draft, mode, or approvals; it only
presents the session you are already running.

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

The status bar underneath keeps its usual live readouts, such as the agent
count (`2 agents`) and the work bar. Transient notices, such as a retry
(`Retry attempt: transport 1/3; network error; waiting 0.94s`), show on the
second line.

## Replies and agents on demand

The main view stays quiet. Press `F5`, or run `/pet inspect`, to open the
inspector over it:

- the whale and an **Agents** list on top, with each agent's state and usage;
- a **Reply** pane below, which streams the current answer and shows terminal
  errors from the same session.

| Key | Effect |
| --- | --- |
| `F5` or `/pet inspect` | Open replies and agents. |
| `Esc` | Back to the message box. Your draft, the transcript, and any running turn are untouched. |
| `Tab` | Switch between the Reply and Agents panes. |
| `Up` / `Down`, `PgUp` / `PgDn`, `Home` / `End` | Scroll the focused pane. |
| `Enter` (Agents pane) | Open the selected agent's transcript. |
| `c` | Copy the last finished reply, through the same path as `/copy`. |

## Several terminals, one pet each

Pet mode is per terminal. Start Codewhale in two terminals and run `/pet on` in
each: each whale follows its own session, so one can show `✓ Done` while the
other shows `● working`. Nothing is mixed between sessions.

Behind the scenes, all terminals on a machine share one local companion
(`codewhale pet serve`, started automatically when a view needs it). The
companion keeps the pet's identity, animation clock, saved recording, optional
sound, and the appearance and window views. It follows one session at a time,
called the source:

- The first saved terminal session to attach becomes the source.
- Later terminals are views of the same companion. They still show their own
  state word and mark, but the companion's finer activity detail inside the
  whale body (reading, editing, searching and so on) appears only in the
  terminal whose session is the source.
- Run `/pet source` in a terminal to make its session the source.

The companion exits after ten minutes with no view, producer, or audio client.
The next view starts it again.

## Commands

| Command | Effect |
| --- | --- |
| `/pet`, `/pet on`, `/pet off` | Toggle, enable, or disable pet mode for this terminal. |
| `/pet inspect` | Open replies and agents (same as `F5`). |
| `/pet status` | Show on or off, habitat open or closed, the companion's identity and tick, the source session, and measured output counters. |
| `/pet source` | Make this terminal's session the companion's source. Needs a saved session. |
| `/pet appearance` | Open the appearance studio (palette, brightness, dot size, glow) in your browser, on loopback. Same as `F8`. |
| `/pet window` | Open the native Codewhale Pet window, if the macOS app is installed. Same as `F9`. If it is not installed, Codewhale says to build or install it. |
| `/pet sound on`, `/pet sound off` | Turn the companion's sound on or off. Same toggle as `F6`. |
| `/pet avatar [key]` | List avatars, or choose one: `whale`, `particles`, `whale-girl`, and any avatar packs you have installed. |
| `/pet action <name>`, `/pet view <name>`, `/pet action live` | Preview a named pose or view of the chosen avatar, or return to live. |
| `/pet export` | Save a replay of the pet's recording to this saved session's artifacts. |

Appearance settings are shared by every view of the companion and never change
the session.

## Sound

Sound is off by default. `/pet sound on` (or `F6`) asks the companion for its
single audio output, played through FFmpeg's `ffplay`. The status reads
`Sound on`, `Sound off`, or `Sound paused` when another view holds the output.
If `ffplay` or the audio device is unavailable, Codewhale says so and sound
stays off.

## Fallbacks

- **Small terminals.** The whale art is drawn only when it has room; otherwise
  the mark, word, hint line, and message box remain.
- **`NO_COLOR`.** Colour is left to your terminal; the marks and words still
  carry the state.
- **`CODEWHALE_ASCII_SAFE=1`.** Marks, borders, and the pointer use ASCII
  (`o idle`, `|`, `>`), and the animated art is replaced by the mark and word.
- **Reduced motion.** Follows the shell's motion setting and shows a still pose.
- **`CODEWHALE_PET_GRAPHICS=braille`.** Forces text rendering where the
  terminal view would otherwise use pixel graphics.
- **No companion.** If the companion cannot be reached, the view says so and the
  state word and mark continue to work, because they come from your session.

## Privacy

The companion is presentation only. It has no agent, prompt, provider, or
execution API.

- What reaches it from a session is a short allowlist of event kinds, ids, and
  outcomes (for example "a read started", "turn completed"). Prompt text,
  reasoning, tool arguments, results, and paths are not sent, and nothing about
  the session's model context changes.
- It listens only on `127.0.0.1`. Every request needs a random token kept in a
  private (`0600`) file in `~/.codewhale/pet-shared`; browser access also checks
  `Host` and `Origin`. Set `CODEWHALE_PET_HOME` to keep it elsewhere.
- Nothing is uploaded. Your recording and saved world stay on your machine.

`CODEWHALE_PET_PORT` chooses the starting loopback port (default 4633).

## Known limitations

- **No "reply ready" cue yet.** When a turn finishes, the whale shows `✓ Done`
  and the footer hint stays the same static line. There is no separate
  notification, badge, or sound that says a reply is waiting. Glance at the
  state word, or press `F5` to read the reply.
- **Agent rows can lag.** While agents run, the Agents list in the inspector is
  refreshed when the turn ends, not on every change. The agent count in the
  status bar and the work bar are live; treat the inspector's rows as a
  snapshot until the turn finishes.
- **One source at a time.** The companion's activity detail follows one session.
  Other terminals show their own state word and mark, not the detail.
- **`/pet window` is macOS only** and needs the Codewhale Pet app.
- **Sound needs `ffplay`** and one terminal holds the output at a time.

See also [Keybindings](KEYBINDINGS.md), [Modes](MODES.md), and the design notes
in [`design/DOT_WHALE_PET.md`](design/DOT_WHALE_PET.md) and
[`../pet/SHARED.md`](../pet/SHARED.md).
