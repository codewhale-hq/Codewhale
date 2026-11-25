import type { DocsPetDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/pet/page.tsx`
 * ("Watch each session with the whale view"). Checked against docs/PET_MODE.md and
 * the `/whale` command in crates/tui/src/commands/groups/config/config.rs.
 */
export const docsPet: DocsPetDict = {
  metaTitle: "Watch each session with the whale view · Codewhale Docs",
  metaDescription:
    "/whale turns each terminal into a glanceable status view: a whale made of dots that shows working, waiting for you, done, or failed, with replies and agents one key away.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Watch each session with the whale view",
  lede:
    "`/whale` turns the terminal you already use into a glanceable status view. A whale made of dots fills the main view and shows whether the session is working, waiting for you, done, or failed, while the message box stays at the bottom. Each terminal draws its own whale from its own session; nothing else runs.",
  sections: [
    {
      id: "turn-on",
      title: "Turn it on",
      blocks: [
        { note: "This page describes Codewhale 0.10.5. The command is now `/whale`; `/pet` still runs it but is no longer listed. Earlier versions ran a shared companion process with sound, replay export, an appearance studio and other views. Those were removed in 0.10.5; see \"What was removed\" below." },
        {
          code: "/whale         # turn the whale view on or off\n/whale on      # on, without toggling\n/whale off     # back to the standard view",
          lang: "Codewhale",
        },
        {
          p: "The view being on or off is saved as the `pet_mode` setting, so the next session starts the way you left it. The whale view only presents the session you are already running. It does not start a turn, call a model, or change your draft, mode, or approvals.",
        },
      ],
    },
    {
      id: "states",
      title: "Read the whale",
      blocks: [
        {
          p: "The first line and the mark under the whale give the state as a mark and a word, so it reads without colour or motion.",
        },
        {
          rows: [
            ["○ idle", "Nothing is running."],
            ["● working", "A turn is running, including tools and verification."],
            ["needs you", "The session is waiting for an approval or an answer."],
            ["✓ Done", "The last turn completed."],
            ["✕ failed", "The last turn ended in failure."],
          ],
          codeTerms: false,
        },
        {
          p: "While a turn runs, the whale's dots become the thing the work is, such as a wrench for commands, a pencil for edits or a book for reading, then return to the whale. The status bar keeps its live readouts, such as the agent count.",
        },
      ],
    },
    {
      id: "inspect",
      title: "Open replies and agents when you want them",
      blocks: [
        {
          p: "The main view stays quiet. Press `F5` to open the current reply and the agents list over it. Press `Esc` to return to the message box; your draft and any running turn are untouched.",
        },
        {
          rows: [
            ["F5", "Open replies and agents."],
            ["Esc", "Back to the message box."],
            ["Tab", "Switch between the Reply and Agents panes."],
            ["Enter", "In the Agents pane, open the selected agent's transcript."],
            ["c", "Copy the last finished reply, the same as `/copy`."],
          ],
          codeTerms: true,
        },
      ],
    },
    {
      id: "terminals",
      title: "Use it in several terminals",
      blocks: [
        {
          p: "The whale view is per terminal. Run `/whale` in each one and every whale follows its own session, so one can show Done while another is still working. Sessions are never mixed, and the terminals do not talk to each other: there is no background process, no port and no shared state.",
        },
      ],
    },
    {
      id: "commands",
      title: "Commands",
      blocks: [
        {
          rows: [
            ["/whale, /whale on, /whale off", "Toggle, enable, or disable the whale view for this terminal."],
            ["/whale full", "The same as `/whale`; the word exists so the command reads the same as in the desktop app."],
            ["/whale still", "Hold the whale still, or let it move again."],
            ["/whale name <text>", "Name the whale, up to 32 characters. The name replaces Codewhale in the view's first line."],
            ["/whale color <#rrggbb>", "Colour the whale. `/whale color default` restores its own colours."],
          ],
          codeTerms: false,
        },
        {
          p: "The name, the colour and hold-still last until the terminal closes; they are not saved.",
        },
      ],
    },
    {
      id: "privacy",
      title: "What it can see",
      blocks: [
        {
          p: "The whale is drawn inside the terminal process. It reads a short list of event kinds, ids and outcomes from the session, such as \"a read started\" or \"turn completed\". Prompt text, reasoning, tool names, arguments, results and paths are not read, and nothing is written to disk or sent anywhere.",
        },
      ],
    },
    {
      id: "removed",
      title: "What was removed in 0.10.5",
      blocks: [
        {
          p: "The shared companion process (`codewhale pet serve`) is gone, with everything only it provided:",
        },
        {
          list: [
            "Sound (`/pet sound`, `F6`).",
            "Replay recording and `/pet export`.",
            "The browser appearance studio (`/pet appearance`, `F8`) and `/pet style`.",
            "Source selection (`/pet source`) and `/pet status`.",
            "The other views, view cycling, `/pet avatar`, `/pet action`, `/pet view` and avatar packs in the terminal.",
            "`/pet inspect`; use `F5`.",
          ],
        },
        {
          p: "Files an older version left in `~/.codewhale/pet-shared` are not read and are not deleted; you can remove that folder yourself.",
        },
      ],
    },
    {
      id: "limits",
      title: "Known limits",
      blocks: [
        {
          note: "There is no separate \"reply ready\" cue yet. A finished turn shows Done on the whale; press `F5` to read the reply.",
        },
        {
          note: "While agents run, the agents list in the inspector refreshes when the turn ends. The agent count in the status bar is live.",
        },
        {
          note: "The work bar is hidden while the whale view is showing. Turn the view off with `/whale` to use it.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/work",
      label: "Track progress",
      note: "Follow the goal, To-do list, and sub-agents in the workbar.",
    },
    {
      href: "/docs/subagents",
      label: "Run agents in parallel",
      note: "Hand independent parts of a task to sub-agents.",
    },
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Choose when a session waits for you.",
    },
  ],
  sourceNote:
    "Source documents: docs/PET_MODE.md · Update docs-map.ts when changing.",
};
