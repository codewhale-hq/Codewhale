This skill was written for DeepSeek Harness. In Codewhale:

- Only five DSH packages can be named in a patch row: the MCP client, the skill filesystem, the two hook bridges and the persona. Rows that name other packages are skipped, and the install result lists them. `references/packages.md` lists packages this host does not load.
- A row that needs a Loader `!!js` expression is refused at install, with the row named.
