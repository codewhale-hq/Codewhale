import type { DocsMcpDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/mcp/page.tsx`
 * ("Connect tools with MCP"). Commands and flags are checked against
 * `McpCommand` in crates/tui/src/lib.rs and docs/MCP.md; the code-mode
 * section against crates/engine/src/tools/codemode.rs and
 * crates/runtime/src/features.rs (`code_mode`: Experimental stage, on by default).
 */
export const docsMcp: DocsMcpDict = {
  metaTitle: "Connect tools with MCP · Codewhale Docs",
  metaDescription:
    "Connect local and remote MCP servers, understand authentication and plugin compatibility, and troubleshoot missing tools.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Connect tools with MCP",
  lede:
    "Connect a local tool server or a remote service through Model Context Protocol. Codewhale discovers its tools and applies your tool permissions and approvals when you use them.",
  sections: [
    {
      id: "add",
      title: "Try the Cloudflare Docs server",
      blocks: [
        {
          p: "The public [Cloudflare Documentation server](https://developers.cloudflare.com/agents/model-context-protocol/cloudflare/servers-for-cloudflare/) provides documentation search and a Pages-to-Workers migration guide. [View its official source](https://github.com/cloudflare/mcp-server-cloudflare/tree/main/apps/docs-vectorize). Add it without credentials:",
        },
        {
          code: `codewhale mcp add cloudflare-docs --url "https://docs.mcp.cloudflare.com/mcp"
codewhale mcp tools cloudflare-docs`,
          lang: "Terminal",
        },
        {
          p: "The tool list should include `search_cloudflare_documentation` and `migrate_pages_to_workers_guide`. In an existing TUI session, run `/mcp reload` to load the new configuration into that session.",
        },
        {
          note: "Checked on November 7, 2026 with published CLI v0.10.2: both commands exited successfully and listed those two tools without credentials. This checked setup and discovery, not tool execution. The [released guide](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MCP.md) describes that version; the [main source guide](https://github.com/codewhale-hq/Codewhale/blob/main/docs/MCP.md) may include unreleased changes.",
        },
        {
          p: "Inside a session, `/mcp` opens the MCP manager: each server's state, transport, timeouts, errors, and discovered tools. The same actions are available there, for example `/mcp add stdio <name> <command>` and `/mcp add http <name> <url>`.",
        },
        {
          note: "Local MCP servers run with the permissions of the Codewhale process; remote calls use the access you grant that service. Add only servers you trust.",
        },
      ],
    },
    {
      id: "compatibility",
      title: "What can connect",
      blocks: [
        {
          rows: [
            ["Local stdio", "Codewhale starts the executable you configure with `--command`; repeat `--arg` for its arguments. Install the server and its required runtime first."],
            ["Remote HTTP / SSE", "Use `--url` for Streamable HTTP, with legacy SSE fallback. A legacy SSE endpoint can use `--transport sse`. A remote entry does not need `command`."],
            ["Direct remote authentication", "Your own MCP configuration can use environment-backed headers, bearer tokens or OAuth. OAuth needs compatible server metadata and client registration."],
            ["Plugin-contributed MCP", "A reviewed, enabled plugin can supply stdio or remote MCP servers. Plugin MCP supports environment-backed authentication, but plugin-contributed MCP OAuth is unsupported."],
            ["Tools, resources and prompts", "Codewhale discovers what the server actually advertises. Resources, templates and prompts are available when the server implements them."],
            ["Optional extensions and MCP Apps", "General MCP Apps UI support is not available. Basic MCP transport support does not guarantee support for every optional extension or server."],
            ["Server requirements", "Each server keeps its own OS, executable, browser, dependency and account requirements. A local server runs on the computer hosting Codewhale; Computer Use has its own platform limits."],
          ],
        },
        {
          p: "A direct MCP entry does not require a plugin. [Plugins](/plugins) can package MCP tools with instructions and other components; each component still needs compatible dependencies, review and permissions.",
        },
      ],
    },
    {
      id: "remote-auth",
      title: "Sign in to a remote server",
      blocks: [
        {
          p: "For a directly configured OAuth server, add its real URL, then log in using the name you gave it:",
        },
        {
          code: `codewhale mcp login <server>
codewhale mcp logout <server>`,
          lang: "Terminal",
        },
        {
          p: "For a bearer token, use `--bearer-token-env-var TOKEN_NAME` when adding the server, and set that variable in the environment launching Codewhale. `env_headers` maps header names to environment variable names. Literal `headers` values do not expand `${TOKEN_NAME}`. Plugin MCP entries cannot use OAuth; configure that server directly instead.",
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
          p: "Global servers live in `~/.codewhale/mcp.json`; the legacy `~/.deepseek/mcp.json` is read when the Codewhale file is absent. `codewhale mcp init` writes a starter file. Use an absolute `mcp_config_path` or `CODEWHALE_MCP_CONFIG` to override it. The environment also accepts `DEEPSEEK_MCP_CONFIG`; the Codewhale name wins when both are set.",
        },
        {
          code: `{
  "servers": {
    "cloudflare-docs": {
      "url": "https://docs.mcp.cloudflare.com/mcp"
    }
  }
}`,
          lang: "mcp.json",
        },
        {
          p: "Use either `servers` or its `mcpServers` alias, never both in one file. `env_http_headers` is an alias for `env_headers`. Adapt the server's documented command, arguments and auth fields rather than assuming every client's configuration format is interchangeable.",
        },
        {
          p: "A trusted workspace can add `.codewhale/mcp.json`. Its entries override same-named global servers; the repository cannot grant itself that trust or override your global config path. Explicit global or trusted-project entries win over a colliding plugin server identity.",
        },
        {
          p: "After editing, run `/mcp reload` in the TUI; no restart is needed. Enabled, allowed servers connect at session start by default. Set `\"lazy\": true` on a user-configured server to connect on demand; `\"required\": true` still connects it at startup. Plugin MCP servers have no lazy manifest option. Headless runtimes need a restart after config changes.",
        },
      ],
    },
    {
      id: "tool-names",
      title: "Find the tools",
      blocks: [
        {
          p: "Each tool appears to the model as `mcp_<server>_<tool>`. For this example, documentation search is `mcp_cloudflare-docs_search_cloudflare_documentation`. `codewhale mcp tools <server>` lists discovered tools in its own process; it does not attach those connections to a running session.",
        },
        {
          p: "MCP tools follow your [approval setting](/docs/modes), allow/deny rules and network policy. Whether a call asks first depends on that policy and posture. Full Access does not override repository rules or managed policy.",
        },
      ],
    },
    {
      id: "troubleshooting",
      title: "When tools do not appear",
      blocks: [
        {
          rows: [
            ["Check configuration", "Run `codewhale doctor` for static path and configuration diagnostics. Shell `codewhale mcp validate` connects enabled, allowed servers in a separate process; it is a live check."],
            ["Refresh this session", "TUI `/mcp validate` shows its last snapshot without reconnecting. Use `/mcp reload` after config changes, or `/mcp retry <name>` for one server."],
            ["Connected, but missing a tool", "Check `codewhale mcp tools <server>`, the server's advertised tools, `enabled_tools` / `disabled_tools`, and the session's allow/deny rules. A connected transport alone does not mean a tool is available or permitted."],
            ["Authentication required", "For a direct OAuth server, run `/mcp login <name>`. For token auth, check that the variable is present in Codewhale's launch environment. A successful login in one process does not attach its transport to another."],
            ["Local command fails", "Check the server's executable, arguments, working directory and runtime. On Windows, package runners may need their `.cmd` name. Follow the server vendor's OS and dependency instructions."],
            ["Plugin server is absent", "Check the owning plugin's review, trust and enabled state. MCP settings cannot activate an untrusted plugin or bypass a rejected manifest."],
          ],
        },
      ],
    },
    {
      id: "serve",
      title: "Run Codewhale as an MCP server",
      blocks: [
        {
          p: "A client supporting Codewhale's advertised MCP protocol revisions can launch it as a stdio tool server. By default, it exposes only read-only `file_read` and `search` tools.",
        },
        {
          code: `codewhale mcp add-self
codewhale mcp tools codewhale`,
          lang: "Terminal",
        },
        {
          p: "`add-self` registers `codewhale serve --mcp`; `codewhale mcp-server` is the equivalent entry point. This server reads its separate `[server]` settings from the legacy `~/.deepseek/mcp_server.toml`, not the client `mcp.json`. Write or shell tools require explicit exposure and `require_approval = false`; there is no interactive approval channel. [Runtime API](/docs/runtime-api) HTTP is separate from MCP.",
        },
      ],
    },
    {
      id: "code-mode",
      title: "Compose tool calls with code mode (experimental)",
      blocks: [
        {
          note: "This section follows the main candidate's source and can include behavior newer than published v0.10.2.",
        },
        {
          p: "Code mode lets the model write one short JavaScript program that calls several tools, loops, and filters results, instead of making each call as a separate step. Only the program's final value goes back to the model, which keeps long lookups compact. It is on by default — `execute_tools` is advertised from the first request — and MCP and plugin tools compose inside a program the same way built-in ones do. Turning it off does not remove code mode: `execute_tools` goes back to being deferred, and the model can still find it through `tool_search`. To turn it off for one session, or in config:",
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
    "Source documents: docs/MCP.md, crates/engine/src/tools/codemode.rs · Update docs-map.ts when changing.",
};
