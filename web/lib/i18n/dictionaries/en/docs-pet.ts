import type { DocsPetDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/pet/page.tsx`
 * ("Watch each session with pet mode"). Checked against docs/PET_MODE.md and
 * the `/pet` command in crates/tui/src/commands/groups/config/config.rs.
 */
export const docsPet: DocsPetDict = {
  metaTitle: "Watch each session with pet mode · Codewhale Docs",
  metaDescription:
    "Pet mode turns each terminal into a glanceable status view: a whale that shows working, waiting for you, done, or failed, with replies and agents one key away.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Watch each session with pet mode",
  lede:
    "Pet mode turns the terminal you already use into a glanceable status view. A whale fills the main view and shows whether the session is working, waiting for you, done, or failed, while the message box stays at the bottom. Run it in several terminals and each one shows its own session.",
  sections: [
    {
      id: "turn-on",
      title: "Turn it on",
      blocks: [
        {
          code: "/pet on        # pet mode becomes the main view\n/pet off       # back to the standard view\n/pet status    # on or off, and the companion's identity",
          lang: "Codewhale",
        },
        {
          p: "Your choice is saved as the `pet_mode` setting, so the next session starts the way you left it. Pet mode only presents the session you are already running. It does not start a turn, call a model, or change your draft, mode, or approvals.",
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
          p: "The status bar keeps its live readouts, such as the agent count and the work bar.",
        },
      ],
    },
    {
      id: "inspect",
      title: "Open replies and agents when you want them",
      blocks: [
        {
          p: "The main view stays quiet. Press `F5`, or run `/pet inspect`, to open the current reply and the agents list over it. Press `Esc` to return to the message box; your draft and any running turn are untouched.",
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
          p: "Pet mode is per terminal. Run `/pet on` in each one and every whale follows its own session, so one can show Done while another is still working. Sessions are never mixed.",
        },
        {
          p: "All terminals on a machine share one local companion that keeps the pet's identity, sound, and appearance. It follows one session at a time, called the source: the first saved terminal to attach, or the one where you run `/pet source`. Other terminals still show their own state; only the finer activity detail inside the whale follows the source.",
        },
      ],
    },
    {
      id: "commands",
      title: "Commands",
      blocks: [
        {
          rows: [
            ["/pet on, /pet off, /pet", "Enable, disable, or toggle pet mode for this terminal."],
            ["/pet inspect", "Open replies and agents."],
            ["/pet status", "Show the mode, the companion's identity, and the source session."],
            ["/pet source", "Make this terminal's session the source."],
            ["/pet appearance", "Open the appearance studio in your browser, on this machine only."],
            ["/pet window", "Open the native Codewhale Pet window, if the macOS app is installed."],
            ["/pet sound on, /pet sound off", "Turn the companion's sound on or off. Sound is off by default and needs ffplay."],
            ["/pet avatar", "List the avatars, or choose one."],
            ["/pet export", "Save a replay to this saved session's artifacts."],
          ],
          codeTerms: false,
        },
      ],
    },
    {
      id: "privacy",
      title: "What it can see",
      blocks: [
        {
          p: "The companion only presents. It has no agent, prompt, provider, or execution access, and it receives event kinds and outcomes rather than prompts, replies, tool arguments, results, or paths. It listens only on `127.0.0.1`, behind a private token, and nothing leaves your machine.",
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
          note: "While agents run, the agents list in the inspector refreshes when the turn ends. The agent count in the status bar and the work bar are live.",
        },
        {
          note: "`/pet window` needs the macOS Codewhale Pet app, and sound needs ffplay.",
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
