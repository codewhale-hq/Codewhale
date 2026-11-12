# Installing plugins

> 阅读简体中文版：[zh_hans/PLUGINS.md](zh_hans/PLUGINS.md)。

To create a bundle, start with [Write your first Codewhale plugin](PLUGIN_AUTHORING.md)
and its runnable Skills example.

This is the walkthrough for the `/plugin install` on-ramp (v0.9.4, #5182).
[PLUGIN_BUNDLES.md](PLUGIN_BUNDLES.md) remains the contract for the bundle
format (`plugin.json`, compatible `kimi.plugin.json` or
`.claude-plugin/plugin.json`, or legacy `plugin.toml`), discovery, validation, and
the trust/enable lifecycle — this document covers how bits get onto disk in
the first place.

Everything you add to Codewhale is a plugin. A skill, an MCP connector and a hook
are each a plugin, or part of one, and each has a kind: Skill, Connector, Hook,
Command. A skill you set up by hand is a plugin of the kind Skill, a server you
configure is a plugin of the kind Connector, and a hook you configure is a plugin
of the kind Hook. They are still managed where they are today: Extensions →
Skills, MCP and Hooks in the terminal, and Customize › Skills, Connectors and
Hooks on the desktop. Their files, trust rules and commands (`/skills`, `/mcp`,
`/hooks`) do not change. One Plugins list that shows every kind together is
planned for 0.10.3 and is not in this build; today the kind labels appear on the
parts inside a plugin bundle's detail. "Connection" is the word for a model
provider or another agent; it is not a kind of plugin.

## Choose what to add

| What you need | Use | Starting point |
| --- | --- | --- |
| Reusable instructions that work through your existing tools | A plugin that adds a skill | `/skills`, then `/skill <name>`; [Skills](SKILLS.md) |
| A reviewed bundle of workflows, tools, commands or agent profiles | A plugin bundle | `/plugin marketplace list`; [first-party catalog](https://github.com/codewhale-hq/codewhale-plugin-marketplace) |
| Tools or context from a particular local process or remote service | A plugin that adds an MCP connector | `/mcp`; [MCP setup](MCP.md) |

A plugin can combine skills, Markdown commands, Fleet agent profiles, hooks,
MCP servers and AI-provider declarations. Tools come from an MCP server or an
experimental native extension; there is no generic application-UI component in
the bundle manifest. Computer Use's Mac helper and Chromewhale's browser
extension have their own setup after plugin review.

An MCP connector is one of two kinds: a local process over stdio, or a remote URL over
Streamable HTTP. Legacy SSE is available when the server still supports it.
User-configured remote servers can use environment-backed credentials or the
host's OAuth login. Plugin-contributed remote servers require explicit
environment-backed credentials when authentication is needed; their interactive
OAuth path is not supported. Installing a plugin does not connect an
account or establish that a service works.

A local stdio server runs with your operating-system user's access. Requested
filesystem roots in a plugin manifest do not sandbox that process.

For example, configure [Cloudflare's public documentation server](https://developers.cloudflare.com/changelog/post/2026-10-22-cloudflare-mcp-servers-retire-sse/)
directly, or install the `cloudflare-docs` plugin when you also want its skill:

```sh
codewhale mcp add cloudflare-docs --url https://docs.mcp.cloudflare.com/mcp
codewhale mcp connect cloudflare-docs
codewhale mcp tools cloudflare-docs
```

Choose one setup for that server. `connect` is a diagnostic connection; it does
not attach tools to an already running session. In an existing terminal session,
run `/mcp reload` after changing configuration.

## Bundle compatibility

| Format | Supported boundary |
| --- | --- |
| Native `plugin.json` or legacy `plugin.toml` | Reviewed declarative adapters above; [manifest contract](PLUGIN_BUNDLES.md) |
| Claude `.claude-plugin/plugin.json` | Skills, commands, agent profiles and MCP subset; hooks, LSP, custom MCP paths and plugin-root expansion are rejected; [compatibility details](CLAUDE_PLUGIN_COMPAT.md) |
| `kimi.plugin.json` | Compatible skills, commands, agents and MCP declarations; unsupported runtime fields fail closed |
| Local DSH package | Explicit preview and exact-hash import through `/plugin import dsh`; experimental native runtime, not blanket DSH compatibility or release qualification; [current evidence and limits](DSH_PLUGIN_COMPAT.md) |

Native TypeScript extensions require `extension_host = true` under `[features]`
and fresh capability review. LSP, requested filesystem roots and lifecycle mutation
remain inventory-only. Current DSH adapters include work beyond the published
v0.10.2 CLI; third-party DSH plugins' complete workflows are not established by
an import succeeding. An imported DSH plugin's UI is served only on the seats the Engine admits ([details](DSH_PLUGIN_COMPAT.md#plugin-ui-dshclient)). Other applications' plugin
directories and credentials are not borrowed.

`/plugin suggest <task>` is a local, read-only companion: it ranks already
installed bundles (name, keywords, description, bundled skill names, declared
hosts) and available marketplace catalogs, including the bundled snapshot and
catalogs you have added with `/plugin marketplace add`.
It explains the match and gives the next review, enable, or catalog-install
step, but never installs, trusts, or enables a bundle on its own.

## How Codewhale offers plugins

Codewhale is helpful about plugins, not pushy. The rules:

- **One proactive surface.** Sending a task can show one quiet toast when the
  prompt matches an installed-but-idle plugin or a catalog candidate you do
  not have yet, for example `/plugin trust supabase` or
  `/plugin marketplace install <catalog> supabase`. Nothing appears while you
  type, and nothing is appended to your message to advertise plugins.
- **One switch.** With `contextual_tips` off, no plugin guidance appears
  anywhere. Required notices and your own `/plugin` commands still work.
- **One budget.** Plugin offers share the per-session guidance budget with
  other tips. In the interactive TUI the model can call
  `request_plugin_install` once per session to ask you to review a plugin the
  task needs; a second call fails. Exec, ACP, and runtime-API sessions do not
  get the tool.
- **Built-ins are never advertised.** Inspect existing bundles in `/plugin list`
  and Extensions before installing another copy. The published v0.10.2 CLI
  includes Computer Use; its layout differs from the current source candidate.
  See [Computer Use across versions](PLUGIN_MARKETPLACE.md#computer-use-across-versions).
- **Specific terms only.** Generic words (accessibility, browser, chrome,
  docs, screenshot, web, wiki, …) and Codewhale's own vocabulary (agent, code,
  data, MCP, model, session, tool) never trigger an offer, so asking why an
  MCP server is stuck does not suggest a vendor's plugin. Distinctive names,
  phrases and domains still do. The matcher and the marketplace's
  `check-marketplace.mjs` share one stoplist.
- **Only what runs here.** Plugins whose `when.os` excludes this OS are not
  offered.
- **Review before acting.** A model-requested row offers Review. Its button
  opens the existing Extensions inventory: Plugins for an installed bundle,
  Marketplace for a suggestion that is not installed. Nothing is installed,
  trusted, or enabled by that click. Choose the inventory's explicit action;
  trust still reviews the exact installed content before activation.
- **Reversible dismissal.** Esc clears a non-empty draft first, then hides the
  row for this session only. "Don't suggest again" is the explicit,
  persisted choice. `/plugin dismissals` lists both kinds, and
  `/plugin dismissals reset [<name>]` lets suggestions offer a plugin again.
- **Discovery is passive.** Find new plugins in these docs,
  `/plugin marketplace list`, Extensions, and the browser guide below.

Codewhale does not invent a remote plugin URL; missing plugins are suggested
only from available catalogs. On-disk bundle changes still toast
`/plugin reload` on send and between turns.

## Browser: pick one

Several options can drive a browser. They differ in whose browser it is and
what it can see.

| Option | Whose browser | Good for |
| --- | --- | --- |
| `chrome-devtools` MCP (`/mcp recommendations`) | A Chrome it drives, which can include signed-in pages | DevTools-level inspection and performance work |
| Playwright MCP (`/mcp recommendations`) | A fresh, isolated profile with `--isolated` | Scripted flows and testing without your identity |
| Computer Use browser tools (off until reviewed) | One it launches, in a profile of its own | Browser steps inside a wider desktop task |
| Chromewhale (developer preview, `codewhale-hq/codewhale-plugin-marketplace`) | Yours, already open, in your own Chrome profile; load unpacked | Reading or acting on the tab you are looking at, one granted site at a time |

None of these is offered to you proactively. Add the one that fits the job.

## Sources

`codewhale install <spec>` and `/plugin install <spec>` use the same installer.
The shell entry does not start a model or require model credentials. Both keep
bundles disabled and untrusted until you explicitly review and enable them.

```sh
codewhale install git:github.com/example/plugin@v1.0.0
codewhale install npm:@example/plugin@1.0.0
codewhale install ./local-plugin
```

Git shorthand supports GitHub archives only; refs must be a single safe segment.
Use the existing HTTPS archive URL plus `#path=...` for a nested bundle.
Npm requires an exact version and a native plugin manifest in the tarball.
No git hooks, npm lifecycle scripts, dependency installation or Pi executable
extensions run. A ref names the requested revision, not an integrity signature;
review still binds the exact downloaded bytes. Tags can move: pin a commit for
immutable GitHub selection. Updates of version-pinned sources retain that version.
Network approval and archive safety checks are unchanged.

The original source forms remain available:

```text
/plugin install ./path/to/bundle            # local directory (copied)
/plugin install github:owner/repo           # GitHub archive of the default branch
/plugin install https://example.com/x.tar.gz  # direct tarball URL
```

There is no registry index and no `git clone` in v1 — tarball-only fetching
keeps the size cap and no-symlink guarantees of the installer. Downloads are
gated by the per-domain network policy: an unknown host returns a
"needs approval" error naming the host (`/network allow <host>`, then retry),
a denied host aborts without touching disk.

The fetched tree must contain **exactly one** bundle root — a directory
holding a `plugin.json`, compatible `kimi.plugin.json`,
`.claude-plugin/plugin.json`, or legacy `plugin.toml` manifest. Kimi bundles are accepted when they use Codewhale-compatible Skills,
commands, agents, and MCP declarations; unsupported Kimi runtime fields fail
closed instead of being silently ignored. Bundles land in
the user plugins root at `~/.codewhale/plugins/<name>/`, where `<name>` is the
manifest's plugin name.

Claude bundles keep their metadata in `.claude-plugin/plugin.json` and their
components at the bundle root. The importer supports skills, commands, agents,
and MCP servers declared inline or in root `.mcp.json` (flat server map or an
`mcpServers` wrapper). Claude `http` transport maps to Streamable HTTP. Relative
sources in a `.claude-plugin/marketplace.json` catalog resolve from the marketplace
repository root. The whole bundle remains subject to the same review hashes and
path checks as native plugins.

Remote MCP headers can name credentials without embedding them: exact
`Bearer ${ENV_NAME}` authorization values become `bearer_token_env_var`, and
exact `${ENV_NAME}` header values become `env_headers`. Import reads no credential
values. Literal credentials and compound templates are rejected.

This is a compatible subset: hooks, LSP declarations, custom MCP file paths, and
`${CLAUDE_PLUGIN_ROOT}` expansion are rejected with an explanation; no partial
plugin is installed. Installing a remote MCP declaration does not complete its
authentication. Plugin-contributed remote servers retain the existing explicit
credential requirements; this importer does not enable plugin OAuth.

## The guided flow

Installing never activates anything. The command places the bits, then drops
you straight into the standard capability review:

```text
/plugin install github:someone/neat-plugin
→ Installed plugin 'neat-plugin' to ~/.codewhale/plugins/neat-plugin.
  It is disabled and untrusted. Review its requested authority below…
  <full inventory, permissions, MCP authority render>
  /plugin trust neat-plugin <content-hash>.<capability-hash>

/plugin trust neat-plugin <paste the token>   # records the hash-bound receipt
/plugin enable neat-plugin                    # activates for this workspace
```

This is the same review render and confirmation token as `/plugin trust
<name>` — trust is the strict hash-bound receipt flow, not an advisory marker.
If the bundle's content or declared capabilities change, the receipt stops
matching and the plugin goes inactive until you review again.

## Update and uninstall

```text
/plugin update <name>      # re-download, byte-compare, atomic swap if changed
/plugin disable <name>     # required before uninstall
/plugin uninstall <name>   # deletes the bundle and prunes its state entry
```

- `update` re-downloads the recorded source. Identical bytes are a no-op; a
  changed bundle is swapped atomically and its trust receipt is automatically
  invalidated (the hash no longer matches), so re-review is forced before the
  plugin can activate again. For a local install, it stages a fresh copy of the
  recorded source directory; keep that authoring directory available. Editing
  the installed copy does not update its source. See the
  [local authoring loop](PLUGIN_AUTHORING.md#4-iterate-and-review-changes).
- `update` retains the recorded repository revision, npm version, archive URL
  and bundle selector. Refreshing a catalog does not retarget that source. To
  select a newer catalog pin, follow
  [Changing an installed pin](PLUGIN_MARKETPLACE.md#changing-an-installed-pin).
- `uninstall` refuses enabled plugins (disable first), deletes the bundle
  directory, and removes its persisted trust/enablement entry.

## Safety rules

- Every install carries an `.installed-from` provenance marker. The installer
  **refuses to overwrite or delete** a bundle that lacks it — hand-placed
  bundles under `~/.codewhale/plugins/` are never clobbered.
- Tarballs are size-capped and extracted into a private staging directory
  first; path traversal (`..`, absolute paths) and symlinks/hard links inside
  the bundle are rejected, and the destination only appears via an atomic
  rename after every check passes.
- Install pre-checks the name against builtin and workspace bundles so a
  higher-precedence bundle cannot silently shadow (or be shadowed by) the
  install.
- Newly installed bits are always **disabled and untrusted**; enablement only
  ever happens through the explicit trust review above.
