import type { DocsRuntimeApiDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/runtime-api/page.tsx`
 * ("Automate with the Runtime API"). Checked against docs/RUNTIME_API.md
 * (entrypoints, defaults, auth, endpoints) and the handlers in
 * crates/app-server/src/runtime_api.rs (`POST /v1/threads` → 201 ThreadRecord).
 */
export const docsRuntimeApi: DocsRuntimeApiDict = {
  metaTitle: "Automate with the Runtime API · Codewhale Docs",
  metaDescription:
    "Drive Codewhale from your own scripts and apps: run one-shot prompts in CI, or start the local HTTP API and send turns, stream events, and answer approvals.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Automate with the Runtime API",
  lede:
    "Use `codewhale exec` for a one-shot job, or follow this tutorial to create a thread and stream a turn through the local Runtime API. The [complete reference](/docs/runtime-reference), generated from the repository's main branch, covers routes, request fields, events and compatibility limits.",
  sections: [
    {
      id: "exec",
      title: "Run one job from a script",
      blocks: [
        {
          code: `codewhale exec "Reply with exactly: pong"
codewhale exec --auto --sandbox workspace-write "fix the failing test and run it again"
codewhale exec --auto --sandbox workspace-write --output-format stream-json "update the changelog"`,
          lang: "Terminal",
        },
        {
          p: "Plain `exec` answers once without tools. By default, `--auto` enables Full Access: tools run without asking, shell commands have no Codewhale sandbox, and trust mode lets file tools write outside the workspace. Any `--sandbox` other than `danger-full-access` (or the equivalent `sandbox_mode` in config) keeps trust mode off. Add `--sandbox workspace-write` for the [shell sandbox, where available](/docs/sandbox), and file-tool workspace restrictions; `external-sandbox` relies on your own isolation. Put flags before the prompt, and use `--` before prompt text that starts with `-` or contains literal exec flags. Runs that offer tools are saved in every output format. `--output-format stream-json` prints one JSON event per line and also saves answer-only runs so `--continue` can pick them up. `--max-turns` and `--allowed-tools` put limits on a run.",
        },
      ],
    },
    {
      id: "start",
      title: "Start the Runtime API",
      blocks: [
        { p: "Use a POSIX shell with `curl`, `jq` and `openssl`, and a Codewhale build that supports `app-server --http`. Run all steps in the same terminal so the token, server process id and thread id remain available." },
        { note: "The commands on this page exist in the published release; check `codewhale --version` before comparing behavior. Connecting a provider is required for the turn below, and that request uses your provider's normal billing; starting the server and checking `/health` do not send a model request." },
        {
          code: `export CODEWHALE_RUNTIME_TOKEN="$(openssl rand -hex 32)"
codewhale app-server --http &
CODEWHALE_RUNTIME_PID=$!
curl --fail --silent --show-error --retry 10 --retry-connrefused \  --retry-delay 1 http://127.0.0.1:7878/health`,
          lang: "Terminal",
        },
        {
          p: "The health command must return successfully before you continue; inspect the server output if it fails or the port is occupied. Keep the generated token private and send it as `Authorization: Bearer <token>` on every `/v1/*` request.",
        },
      ],
    },
    {
      id: "turn",
      title: "Send a turn and watch it",
      blocks: [
        {
          code: `API=http://127.0.0.1:7878
AUTH="Authorization: Bearer $CODEWHALE_RUNTIME_TOKEN"

THREAD=$(curl -fsS -X POST "$API/v1/threads" -H "$AUTH" \\
  -H "Content-Type: application/json" -d '{}' | jq -er .id)

curl -fsS -X POST "$API/v1/threads/$THREAD/turns" -H "$AUTH" \\
  -H "Content-Type: application/json" -d '{"prompt": "Summarize README.md"}'

curl -fN "$API/v1/threads/$THREAD/events?since_seq=0" -H "$AUTH"`,
          lang: "Terminal",
        },
        {
          p: "Check that thread creation returns a nonempty id and the turn request returns an accepted turn; the events stream should then show the turn and its recorded result. Keep the last accepted `seq` for reconnects, handle the terminal `stream.end` frame, and follow the [replay contract](/docs/runtime-reference#ending-and-resuming-a-thread-stream) instead of assuming every disconnect is recoverable.",
        },
        {
          rows: [
            ["Stop a turn", "`POST /v1/threads/{id}/turns/{turn_id}/interrupt`"],
            ["Answer an approval", "`POST /v1/approvals/{approval_id}`"],
            ["Steer a running turn", "`POST /v1/threads/{id}/turns/{turn_id}/steer`"],
            ["List saved sessions", "`GET /v1/sessions`"],
          ],
        },
        {
          p: "The [Runtime API reference](/docs/runtime-reference) renders the contract from the repository's main branch, including endpoint tables, request fields, event envelopes and version limits.",
        },
      ],
    },
    {
      id: "stop",
      title: "Stop the tutorial server",
      blocks: [
        { p: "Press Ctrl+C to close the event stream, then stop the server you started in this terminal. Closing the stream alone does not stop the server or interrupt an active model turn; use the interrupt endpoint above if needed." },
        { code: 'kill "$CODEWHALE_RUNTIME_PID"\nunset CODEWHALE_RUNTIME_TOKEN CODEWHALE_RUNTIME_PID', lang: "Terminal" },
      ],
    },
    {
      id: "other",
      title: "Pick another connection",
      blocks: [
        {
          rows: [
            ["codewhale app-server --stdio", "JSON-RPC over standard input and output, with no network listener. Good for an SDK or a local probe."],
            ["codewhale serve --acp", "Agent Client Protocol for editors such as Zed."],
            ["codewhale serve --mcp", "Offer Codewhale's tools to another MCP client. See [Connect tools with MCP](/docs/mcp)."],
            ["codewhale web", "The built-in [browser client](/docs/web), on the same API."],
            ["codewhale doctor --json", "Health and capabilities as JSON, with no secrets."],
          ],
          codeTerms: true,
        },
      ],
    },
    {
      id: "security",
      title: "Keep it private",
      blocks: [
        {
          list: [
            "The server listens on `127.0.0.1` by default. The token is a local guard, not a replacement for TLS or a VPN; do not expose the port to a network.",
            "`--insecure-no-auth` is accepted only on a loopback address.",
            "The API never returns your provider keys. Health and capability reports carry only metadata — no secrets, file contents, or messages.",
          ],
        },
      ],
    },
  ],
  next: [
    { href: "/docs/runtime-reference", label: "Read the Runtime API reference", note: "Find exact routes, fields, event schemas and compatibility behavior." },
    {
      href: "/docs/web",
      label: "Open the browser client",
      note: "A ready-made client for the same API.",
    },
    {
      href: "/docs/hooks",
      label: "Run commands on events",
      note: "React to session events without writing a client.",
    },
    {
      href: "/docs/fleet",
      label: "Run a workflow",
      note: "Durable, multi-step runs you can check from any terminal.",
    },
  ],
  sourceNote: "Source document: docs/RUNTIME_API.md · Update docs-map.ts when changing.",
};
