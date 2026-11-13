This skill was written for DeepSeek Harness. In Codewhale these points replace the matching sentences above:

- The tool that loads skills is `load_skill`, not `skill`. Read the template and reference files from the base directory above with the file-read tool.
- There is no profile folder and no pnpm. Never write under `~/.codewhale/plugins`; write the package in the workspace and call `plugin_manager` `install_bundle` with the folder. `DSH_PROFILE` and `DSH_PROFILE_DIR` are absent: use inspection instead of `dsh --dump-config`.
- Codewhale's built-in plugins are switches the person owns. Enabling one by writing an overriding bundle does not work: `list_bundles` shows them as not changeable, and `set_bundle` or `set_plugin` on one answers `management-required`.
- There is no package directory for DSH packages bridged by Codewhale. `Config.listConfigs` returns the accepted config schema and notes inline; do not look for a `README.md` or `lib/` to read.
- A patch row that needs a Loader `!!js` expression is refused at install, with the row named.
- `configEditor` and `credentials` do not exist here. Plugin settings are the person's `[plugins."<name>".config]` entries.
- Replacing an installed package needs no restart: `remove_bundle` then `install_bundle` loads the new code.
- `approvedBuilds` does not apply. Codewhale never runs a package manager or an install script; a package whose rows name a module that is not inside it is refused whole.
- Version exemptions are not available: the list is empty and a grant is refused.
- Connector tools from a plugin are named `mcp_plugin-...`; the install result's `warnings` list the exact new tool names to call.
- A visual plugin appears in the desktop app's plugin seats. `Slots.listSubTree` lists each seat and whether it is drawn; in the terminal it says no client is connected. The plugin frame has no `ctx.remote.commands.execute()` channel to the Engine.
