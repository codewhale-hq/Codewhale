import type { DocsMcpDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/mcp/page.tsx`
 * ("Connect tools with MCP"). Commands and flags are checked against
 * `McpCommand` in crates/tui/src/lib.rs and docs/MCP.md; the code-mode
 * section against crates/tui/src/tools/codemode.rs and
 * crates/runtime/src/features.rs (`code_mode`: Experimental stage, on by default).
 */
export const docsMcp: DocsMcpDict = {
  metaTitle: "Connect tools with MCP · Codewhale Docs",
  metaDescription:
    "Add Model Context Protocol servers so Codewhale can use more tools, sign in to remote servers, run Codewhale itself as an MCP server, and try code mode.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Connect tools with MCP",
  lede:
    "MCP servers give Codewhale more tools — a database, an issue tracker, a browser. Add a local server that Codewhale starts for you, or a remote server by URL. Its tools then go through the same approvals as built-in ones.",
  sections: [
    {
      id: "add",
      title: "Add a server",
      blocks: [
        {
          code: `codewhale mcp add git --command "uvx" --arg "mcp-server-git"
codewhale mcp add docs --url "https://example.com/mcp"
codewhale mcp list
codewhale mcp validate`,
          lang: "Terminal",
        },
        {
          p: "`--command` starts a local server over stdio; repeat `--arg` for each argument. `--url` connects to a remote server over Streamable HTTP, with legacy SSE as a fallback. `mcp validate` checks the config and the servers you require.",
        },
        {
          p: "Inside a session, `/mcp` opens the MCP manager: each server's state, transport, timeouts, errors, and discovered tools. The same actions are available there, for example `/mcp add stdio <name> <command>` and `/mcp add http <name> <url>`.",
        },
        {
          note: "An MCP server runs with your permissions. Add only servers you trust, as you would any program you install.",
        },
      ],
    },
    {
      id: "remote-auth",
      title: "Sign in to a remote server",
      blocks: [
        {
          p: "For a server that uses OAuth, add it by URL and log in. For a bearer token, keep the token in an environment variable instead of the config file:",
        },
        {
          code: `codewhale mcp login docs
codewhale mcp add tracker --url "https://example.com/mcp" --bearer-token-env-var TRACKER_TOKEN`,
          lang: "Terminal",
        },
        {
          p: "An explicit Authorization header always wins: headers from config apply first, then the bearer-token variable, then a stored OAuth login. `codewhale mcp logout <name>` removes the stored login on this machine; the provider may keep its own grant until you revoke it there.",
        },
      ],
    },
    {
      id: "config",
      title: "Edit the config file",
      blocks: [
        {
          p: "Servers live in `~/.codewhale/mcp.json`. `codewhale mcp init` writes a starter file. The `mcpServers` key used by other clients works too, so you can paste an existing entry.",
        },
        {
          code: `{
  "servers": {
    "example": {
      "command": "node",
      "args": ["./path/to/your-mcp-server.js"],
      "env": {},
      "disabled": false
    }
  }
}`,
          lang: "mcp.json",
        },
        {
          p: "After editing the file, run `/mcp reload` in the session; no restart is needed. A server starts only when a turn needs one of its tools, unless you mark it `\"required\": true` to connect at startup.",
        },
      ],
    },
    {
      id: "tool-names",
      title: "Find the tools",
      blocks: [
        {
          p: "Each tool appears to the model as `mcp_<server>_<tool>`: a server named `git` with a `status` tool becomes `mcp_git_status`. `codewhale mcp tools <server>` lists what a server offers. A server that fails to connect or is disabled never shows up as an available tool.",
        },
        {
          p: "MCP tools follow your [approval setting](/docs/modes): listing and reading a server's resources and prompts can run without a prompt when policy allows, and tools with side effects ask first. Full Access does not override repository rules or managed policy.",
        },
      ],
    },
    {
      id: "serve",
      title: "Run Codewhale as an MCP server",
      blocks: [
        {
          p: "Other MCP clients — including another Codewhale session — can use Codewhale's tools. Register it once:",
        },
        {
          code: `codewhale mcp add-self
codewhale mcp tools codewhale`,
          lang: "Terminal",
        },
        {
          p: "`add-self` writes an entry that runs `codewhale serve --mcp` over stdio. Each client starts its own process; no network port is opened. `codewhale serve --http` is a different thing — the [Runtime API](/docs/runtime-api) for apps.",
        },
      ],
    },
    {
      id: "code-mode",
      title: "Compose tool calls with code mode (experimental)",
      blocks: [
        {
          p: "Code mode lets the model write one short JavaScript program that calls several tools, loops, and filters results, instead of making each call as a separate step. Only the program's final value goes back to the model, which keeps long lookups compact. It is on by default — `execute_tools` is advertised from the first request — and MCP and plugin tools compose inside a program the same way built-in ones do. To turn it off for one session, or in config:",
        },
        {
          code: `codewhale --disable code_mode

# ~/.codewhale/config.toml
[features]
code_mode = false`,
          lang: "Terminal / config.toml",
        },
        {
          list: [
            "Nested native, MCP and plugin calls pass the same gate as a direct call: allow/deny lists, ask-rules, and review still apply. A call that needs approval raises the normal approval card and pauses the program; allow resumes it, deny fails only that call. Approving the program itself grants nothing.",
            "MCP calls run through the session's MCP pool, and plugin tools are callable inside a program just like built-in ones.",
            "Limits per program: 50 nested calls, 4 at a time, and 16 KiB returned. There is no fixed 30-second cap: a program runs on the turn's remaining wall clock, unbounded unless `[tui].turn_wall_clock_secs` is set.",
            "Code mode is not available in Plan mode.",
          ],
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/hooks",
      label: "Run commands on events",
      note: "Check or rewrite a tool call before it runs, including MCP tools.",
    },
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Decide which MCP calls stop for your approval.",
    },
    {
      href: "/docs/runtime-api",
      label: "Automate with the Runtime API",
      note: "Drive Codewhale from your own app or script over HTTP.",
    },
  ],
  sourceNote:
    "Source documents: docs/MCP.md, crates/tui/src/tools/codemode.rs · Update docs-map.ts when changing.",
};
