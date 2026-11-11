import type { DocsHooksDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/hooks/page.tsx`
 * ("Run commands on events"). Checked against docs/HOOKS.md — the
 * authoritative event-by-event contract (15 events, steering events,
 * conditions, environment variables, project-hook approval).
 */
export const docsHooks: DocsHooksDict = {
  metaTitle: "Run commands on events · Codewhale Docs",
  metaDescription:
    "Run your own scripts when a session starts, before a tool call, when a turn ends, or when Codewhale is waiting for you — to add context, enforce a rule, or get notified.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Run commands on events",
  lede:
    "Hooks run a command of yours at set moments in a Codewhale session. Use them to block a risky command, add context to a message, log what happened, or get told when Codewhale is waiting for you.",
  sections: [
    {
      id: "first-hook",
      title: "Add your first hook",
      blocks: [
        { p: "Hooks live in `~/.codewhale/config.toml`. This one prints a line whenever a session starts:" },
        {
          code: `[hooks]
enabled = true

[[hooks.hooks]]
name = "announce"
event = "session_start"
command = "echo 'Codewhale session started'"`,
          lang: "config.toml",
        },
        {
          p: "Start a session, then run `/hooks` to see every configured hook, whether hooks are on, and any entry that was rejected. `/hooks events` lists the event names.",
        },
      ],
    },
    {
      id: "gate",
      title: "Block a command before it runs",
      blocks: [
        {
          p: "A `tool_call_before` hook sees each tool call before it executes and can allow it, deny it, or force an approval prompt. This POSIX-shell example denies every call named `bash`, rather than guessing what a shell command string will do.",
        },
        {
          code: `#!/bin/sh
printf '%s\\n' '{"decision":"deny","reason":"Shell calls are blocked by this hook."}'`,
          lang: "~/.codewhale/hooks/deny-shell.sh",
        },
        { p: "Create `~/.codewhale/hooks/`, save the script there as `deny-shell.sh`, and make it executable on macOS or Linux:" },
        { code: "chmod +x ~/.codewhale/hooks/deny-shell.sh", lang: "Terminal" },
        {
          code: `[[hooks.hooks]]
name = "deny-shell"
event = "tool_call_before"
command = "~/.codewhale/hooks/deny-shell.sh"
condition = { type = "tool_name", name = "bash" }
continue_on_error = false`,
          lang: "config.toml",
        },
        {
          p: "The hook reads the call from environment variables and answers with JSON on standard output: `allow`, `deny`, or `ask`, plus an optional `reason`, a rewritten input (`updatedInput`), or extra context for the model (`additionalContext`). Exit code 2 always denies. When several hooks answer, deny beats ask, and ask beats allow.",
        },
        { note: "This example covers only the exact `bash` tool; it does not block other tools, external clients or commands you run yourself. A substring match for force-push flags is not a shell security boundary; use the execution policy and OS sandbox for broader restrictions." },
        {
          note: "`ask` forces a prompt wherever the active permission posture asks or reviews. Full Access does not add approval prompts, so `ask` does not add one there.",
        },
      ],
    },
    {
      id: "events",
      title: "Choose the moment",
      blocks: [
        {
          p: "Three events can change what happens next. The rest only observe; their output is ignored and a failure is a warning.",
        },
        {
          rows: [
            ["message_submit", "Before your message reaches the model. Can replace the text or block it."],
            ["tool_call_before", "Before each tool call. Can allow, deny, ask, rewrite the input, or add context."],
            ["shell_env", "Before each shell command. Can add environment variables."],
            ["session_start / session_end", "When a session opens or closes cleanly."],
            ["turn_end", "After a turn finishes, with its status, duration, and token usage."],
            ["tool_call_after", "After each tool result, with its exit code when there is one."],
            ["waiting_for_user", "When Codewhale starts waiting for an approval, an answer, or a paused goal."],
            ["session_idle / session_busy", "When the session settles or starts working again."],
            ["session_error / on_error", "When a turn fails for good, or on any error or failed tool."],
            ["mode_change", "When you change the mode."],
            ["subagent_spawn / subagent_complete", "When a sub-agent starts or finishes."],
          ],
          codeTerms: true,
        },
        {
          p: "A `condition` narrows when a hook fires: by tool name (with `*` globs), tool category, mode, or exit code, and combinations with `all` and `any`. A condition that can never match its event is rejected when the config loads, so a gate you think is armed never sits silently inert.",
        },
      ],
    },
    {
      id: "options",
      title: "Set timeouts and failure behavior",
      blocks: [
        {
          rows: [
            ["timeout_secs", "How long the hook may run. Default 30."],
            ["continue_on_error", "`true` (default): a failing hook only warns. `false`: the failure blocks."],
            ["background", "`true` runs the hook as an observer only; it cannot block or rewrite."],
            ["working_dir", "Under `[hooks]`: where hooks run. Default: the session's workspace."],
          ],
          codeTerms: true,
        },
        {
          note: "`[hooks] default_timeout_secs` replaces every hook's own `timeout_secs`, not just the missing ones. Leave it unset if you want per-hook timeouts.",
        },
      ],
    },
    {
      id: "project",
      title: "Use hooks a repository ships",
      blocks: [
        {
          p: "A repository can include hooks in `.codewhale/hooks.toml`. Because they run commands on your machine, they load only after you trust the workspace and approve that exact file:",
        },
        { code: "/hooks review\n/hooks approve <digest>\n/hooks revoke", lang: "Codewhale" },
        {
          p: "`/hooks review` shows the commands and a digest of the file; approving that digest enables those exact bytes from the next session. Any change to the file needs a new approval. Review the scripts the commands call, too.",
        },
      ],
    },
    {
      id: "headless",
      title: "Use hooks in scripts and CI",
      blocks: [
        {
          p: "Hooks run in the interactive session. `codewhale exec` fires none by default; add `--hooks` to fire `tool_call_before` and `shell_env`. With no one to answer, an `ask` becomes a deny.",
        },
        { code: 'codewhale exec --auto --sandbox workspace-write --hooks "run the test suite and fix the first failure"', lang: "Terminal" },
      ],
    },
  ],
  next: [
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "See how hook decisions combine with your approval setting.",
    },
    {
      href: "/docs/mcp",
      label: "Connect tools with MCP",
      note: "Gate MCP tools with the same `tool_call_before` hooks.",
    },
    {
      href: "/docs/configuration",
      label: "Change settings",
      note: "Where `config.toml` lives and what a project may override.",
    },
  ],
  sourceNote:
    "Source documents: docs/HOOKS.md (authoritative), docs/CONFIGURATION.md · Update docs-map.ts when changing.",
};
