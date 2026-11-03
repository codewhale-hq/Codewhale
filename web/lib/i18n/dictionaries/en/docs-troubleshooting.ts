import type { DocsTroubleshootingDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/troubleshooting/page.tsx`
 * ("Fix a problem"). Error texts and fixes come from docs/INSTALL.md §13
 * (every one was hit while writing that guide), docs/OPERATIONS_RUNBOOK.md,
 * docs/KEYBINDINGS.md (Ctrl-B), crates/runtime/src/runtime_log.rs (log path),
 * and docs/DOCKER.md.
 */
export const docsTroubleshooting: DocsTroubleshootingDict = {
  metaTitle: "Fix a problem · Codewhale Docs",
  metaDescription:
    "Diagnose Codewhale in one command, then fix the common problems: command not found, no reply, a rejected key, network errors, a stuck turn, a session that will not resume, and MCP servers.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Fix a problem",
  lede:
    "Start with one diagnostic command, then find your symptom below. Each fix names the exact message you will see.",
  sections: [
    {
      id: "diagnose",
      title: "Run the diagnostics",
      blocks: [
        {
          code: `codewhale --version
codewhale doctor
codewhale doctor --probe-api                  # one real test call to your provider
codewhale auth status --provider deepseek     # which key is in use`,
          lang: "Terminal",
        },
        {
          p: "`codewhale doctor --json` produces a diagnostics bundle without secrets, ready to attach to an issue. Plain `doctor` does not tell you which key is active and exits successfully even with no key; use `auth status` for that.",
        },
      ],
    },
    {
      id: "install",
      title: "Install and update",
      blocks: [
        {
          rows: [
            ["`codewhale: command not found`", "`~/.local/bin` is not on your PATH in this terminal. Add `export PATH=\"$HOME/.local/bin:$PATH\"` to your shell profile and open a new terminal."],
            ["`npm error code EACCES`", "Your Node install is owned by the system. Do not use sudo: point npm at a folder you own with `npm config set prefix \"$HOME/.npm-global\"`, add its `bin` to your PATH, and install again."],
            ["`refusing to replace existing ~/.local/bin/codewhale`", "A different version is already there. Run `codewhale update`, or remove the old binaries first."],
            ["`checksum mismatch`", "The download was corrupted or altered, and nothing was installed. Try again; if it repeats, do not use a mirror."],
            ["`The package-managed executable was not changed.`", "You installed with npm, Cargo, or Homebrew. Update with that tool, for example `npm install -g codewhale`."],
          ],
        },
      ],
    },
    {
      id: "model",
      title: "No reply, or the key is rejected",
      blocks: [
        {
          rows: [
            ["Your message appears but nothing answers", "No key is configured, and v0.10.0 does not warn you. Press F3, choose your provider, and paste the key."],
            ["`API key not found`", "No key anywhere. Save one with `codewhale auth set --provider <name>`."],
            ["`Authentication Fails … is invalid`", "The key is wrong or revoked. Run `auth status` to see which source is used — a saved key beats an environment variable — then save the right key or `codewhale auth clear --provider <name>`."],
            ["`Network error: SSE stream request failed …`", "Usually no connection to the provider. Check with `curl -sI https://api.deepseek.com` (a 401 means it is reachable). Behind a proxy, export `HTTPS_PROXY`. On Windows or strict proxies, try `CODEWHALE_FORCE_HTTP1=1`."],
          ],
        },
      ],
    },
    {
      id: "turn",
      title: "A turn is stuck",
      blocks: [
        {
          list: [
            "Press Esc to cancel the turn. Esc also closes menus first, so press it again if a menu was open.",
            "If a long shell command is holding the turn, press Ctrl-B to move it into the background. The turn continues, and `/jobs` shows the command.",
            "`/retry` sends the last request again.",
          ],
        },
        {
          p: "For a detailed record, start Codewhale with `RUST_LOG=codewhale_tui=debug` (or `RUST_LOG=codewhale_tui::client=debug` for connection retries). Logs are written to `~/.codewhale/logs/`.",
        },
      ],
    },
    {
      id: "sessions",
      title: "Resume a session",
      blocks: [
        {
          code: `codewhale sessions            # list saved sessions
codewhale resume <id>         # an id or a unique prefix
codewhale -c                  # the latest session in this folder`,
          lang: "Terminal",
        },
        {
          p: "Inside Codewhale, Ctrl-R opens the session picker. `No saved sessions found for workspace` after `codewhale exec --continue` means the earlier run was a plain `exec`, which is not saved; use `--output-format stream-json` for runs you want to continue.",
        },
        {
          p: "Messages you send while offline wait in a queue, saved with the session. `/queue list` shows them. When the connection is back, open one with `/queue edit <n>` and press Enter to send it.",
        },
      ],
    },
    {
      id: "mcp",
      title: "MCP tools are missing",
      blocks: [
        {
          list: [
            "After changing `mcp.json` or a server's credentials, run `/mcp reload`. `/mcp validate` only refreshes what you see.",
            "Run the server's command yourself in a shell to confirm it starts.",
            "If the config file is missing or broken, `codewhale mcp init --force` writes a fresh one.",
          ],
        },
      ],
    },
    {
      id: "docker",
      title: "Run in Docker",
      blocks: [
        {
          code: `docker volume create codewhale-home
docker run --rm -it \\
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \\
  -v codewhale-home:/home/codewhale/.codewhale \\
  -v "$PWD:/workspace" -w /workspace \\
  ghcr.io/codewhale-hq/codewhale:latest`,
          lang: "Terminal",
        },
        {
          p: "The image runs as a non-root user and keeps your settings and sessions in the named volume. Pin a release tag instead of `latest` for repeatable setups, use one volume per project, and never bake keys into an image.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "Connect a provider",
      note: "Save a key, check which one is used, or switch to a local model.",
    },
    {
      href: "/install",
      label: "Install Codewhale",
      note: "Every install method, with the output each step should print.",
    },
    {
      href: "/docs/review",
      label: "Review what changed",
      note: "Roll files back to the snapshot before a turn went wrong.",
    },
  ],
  sourceNote:
    "Source documents: docs/INSTALL.md §13, docs/OPERATIONS_RUNBOOK.md, docs/DOCKER.md · Update docs-map.ts when changing.",
};
