# Claude Plugin Compatibility

> 阅读简体中文版：[zh_hans/CLAUDE_PLUGIN_COMPAT.md](zh_hans/CLAUDE_PLUGIN_COMPAT.md)

Codewhale treats Claude Code skill folders as instruction bundles when they are
plain `SKILL.md` directories. It does not run Claude Code plugin runtimes.

## Supported

- Workspace or global `.claude/skills/<name>/SKILL.md` directories discovered by
  the normal skill registry.
- GitHub or tarball installs that contain one selected skill directory such as
  `skills/<name>/SKILL.md`, `.agents/skills/<name>/SKILL.md`,
  `.claude/skills/<name>/SKILL.md`, or a nested package layout ending in
  `skills/<name>/SKILL.md`.
- Companion files inside the selected skill directory, such as `references/`,
  `examples/`, or scripts that are only used after the skill is explicitly
  loaded and trusted.

## Compatible plugin bundles

Since v0.9.13, `/plugin install` accepts the declarative subset described in
[Installing plugins](PLUGINS.md): nested `.claude-plugin/plugin.json` metadata,
root skills, commands and agent profiles, plus inline MCP declarations or
`.mcp.json`. Local sources in Claude marketplace catalogs resolve from the
repository root outside `.claude-plugin`.

Exact `${ENV_NAME}` MCP header references map to Codewhale's environment-backed
credentials, including `Authorization: Bearer ${ENV_NAME}`. Literal credential
values and compound header templates are rejected; importing never reads the
referenced environment variables.

The importer uses Codewhale's existing adapters, review hashes and enablement.
A Claude marketplace's labels or defaults grant no authority. Bundles are not
scanned from another application's plugin roots or activated automatically.

## Unsupported runtime features

The importer rejects hooks, LSP declarations, custom MCP file paths, and
`${CLAUDE_PLUGIN_ROOT}` expansion. It does not run plugin build steps,
TypeScript agents, dashboard servers, shared plugin state, or token-gated
service processes. Claude-specific frontmatter behavior such as `model: inherit`
is not an additional runtime contract. Remote MCP authentication still follows
Codewhale's existing plugin credential boundary; installation does not complete
OAuth or borrow another application's credentials.

`/skill install` remains a separate one-skill operation: it rejects multi-skill
plugin archives rather than silently selecting one directory and dropping other
components. Use `/plugin install` for supported bundles, or migrate one explicit
skill directory when the repository depends on unsupported runtime behavior.

See [PLUGIN_BUNDLES.md](PLUGIN_BUNDLES.md) for discovery, review and activation.
