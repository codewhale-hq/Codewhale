---
name: plugin-creator
description: Scaffold a local Codewhale plugin bundle with a versioned manifest, namespaced Skills, and a trust review that goes through the session's approval.
---

# Plugin Creator

Use this skill when a user wants a local Codewhale plugin bundle. Trusted and
enabled bundles may add declarative Skills, commands, agents, hooks, and MCP
servers (stdio and remote) through the existing engines. LSP, filesystem
roots, and lifecycle mutation are inventory-only. Native extensions (host
code) are inventory-only unless the user has turned on the experimental
`[features] extension_host` flag.

## Workflow

1. Pick a Codewhale-owned location:
   - User bundle: `~/.codewhale/plugins/<plugin-name>/`
   - Workspace bundle: `<workspace>/.codewhale/plugins/<plugin-name>/`
2. Normalize the bundle name to lowercase hyphen-case.
3. Create `plugin.json` (Agent Plugins v1.0.0; a legacy `plugin.toml` stays
   readable, but new bundles use `plugin.json`):

```json
{
  "$schema": "https://agent-plugins.org/schemas/plugin.json",
  "name": "my-plugin",
  "version": "0.1.0",
  "description": "What this bundle provides"
}
```

4. Put each Skill under `skills/<skill-name>/SKILL.md`; Codewhale finds
   `skills/` automatically and exposes each as `my-plugin:<skill-name>`,
   never as an unqualified command.
5. Add MCP servers in a sibling `mcp.json` only when the bundle needs an
   existing MCP engine. Keep stdio commands and paths inside the bundle. Map local
   environment values only as exact `${SOURCE_ENV}` references. For remote MCP,
   use HTTPS (or loopback HTTP), forbid URL user information/query/fragment,
   use only environment-backed headers or bearer tokens, and declare the exact
   normalized endpoint host set in `capabilities.network_hosts` under
   `extensions["net.codewhale"]`. Never place credentials in the manifest.
6. Commands (`commands/*.md`), agents (`agents/*.toml`), and hooks
   (`hooks/*.toml`), declared under `extensions["net.codewhale"]`, activate
   under the current policy — workspace bundles win same-name collisions over
   user and built-in bundles. LSP, filesystem roots, and lifecycle mutation
   are inventory-only: declare them only when inventorying future work. A
   `native` entry runs only under the experimental extension host; there it
   must be one `.mjs`, `.js` or `.mts` ES module file, `/plugin validate` rejects
   anything else, and its tools always use `Required` approval, never a
   plugin's read-only hint. Full Access, Bypass, or an exact session grant
   for the reviewed build can satisfy that gate without a prompt. A bundle
   that declares only unsupported surfaces cannot be enabled.
7. Validate and review without executing bundle content:
   - `/plugin validate <plugin-name>`
   - `/plugin show <plugin-name>`
   - present these results; the person runs `/plugin enable <plugin-name>`
     to open the content/capability review, reviews it, runs the exact
     `/plugin trust ...` confirmation shown, then enables the bundle
   - exception: in Full Access, for a plugin you wrote yourself, you may
     trust and enable it without asking the person. Write it in an ordinary
     folder inside the current project (for example
     `<workspace>/plugins/<plugin-name>/`), not under `.codewhale/plugins/`
     as in step 1, because `codewhale install` copies it into place and
     refuses a name that is already in use. Then run, in this order:
     `codewhale install <folder>`, `codewhale plugins trust <name>` (it prints
     the review and a review token), `codewhale plugins trust <name>
     --review-token <token> --yes` with the token from your own run, and
     `codewhale plugins enable <name>`. These are shell commands, so the
     session's approval applies to them like any other (Ask asks the person,
     Auto-Review's reviewer judges, Full Access runs them). The trust is
     recorded as approved by the agent, for this project only. A bundle that
     was not installed this way from inside the project is refused. Never do
     this for a plugin you did not write.
8. Verify `/skills inspect` reports plugin provenance and `/plugin list`
   reports the expected trust and activation state. Trust stages the reviewed
   content but does not activate it. After enablement, follow the host's
   reload notice: use `/reload` or a new session to apply changes to a live
   session's pinned skills and tools.

Every user and workspace bundle starts untrusted and disabled. Reuse the
existing `/plugin marketplace`, install, update, review and reload surfaces;
do not add a parallel installer or registry. Catalog membership alone never
installs, trusts or enables a plugin.

## Plugins with code or a screen

For a plugin that runs code, adds tools of its own, or shows a screen, use the
`cordis-plugin-development` skill when it is offered: it teaches the format
that also loads in DeepSeek Harness. Host code runs only under the
experimental `[features] extension_host` flag; start from the tested
`hello-extension` example and `docs/EXTENSIONS.md` in the Codewhale repository,
add a `plugin.json` beside the package for Codewhale's own kinds, and never
change the shared process cwd. A person reviews and turns on a plugin as step 7
says. `/plugin show <name>` reports owner state, live tools and recent
attributed diagnostics. Explain the shared-process and current platform sandbox
limits without claiming isolation.
