---
name: plugin-creator
description: Scaffold a local Codewhale plugin bundle with a versioned manifest, namespaced Skills, and an explicit trust review.
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
   must be one `.mjs` or `.js` ES module file, `/plugin validate` rejects
   anything else, and its tools always use `Required` approval, never a
   plugin's read-only hint. Full Access, Bypass, or an exact session grant
   for the reviewed build can satisfy that gate without a prompt. A bundle
   that declares only unsupported surfaces cannot be enabled.
7. Validate and review without executing bundle content:
   - `/plugin validate <plugin-name>`
   - `/plugin show <plugin-name>`
   - `/plugin enable <plugin-name>` to open the content/capability review
   - run the exact `/plugin trust ...` confirmation shown, then enable again
8. Verify `/skills inspect` reports plugin provenance and `/plugin list`
   reports the expected trust and activation state. Trust stages the reviewed
   content but does not activate it. After enablement, follow the host's
   reload notice: use `/reload` or a new session to apply changes to a live
   session's pinned skills and tools.

Every user and workspace bundle starts untrusted and disabled. Reuse the
existing `/plugin marketplace`, install, update, review and reload surfaces;
do not add a parallel installer, registry or automatic trust flow. Catalog
membership alone never installs, trusts or enables a plugin.
