# DeepSeek Harness creator files, pinned

Byte-for-byte copies of the files DeepSeek Harness 0.2.2-rc.1 uses for its
Creator mode, kept so Codewhale hosts the same tool and skill text rather than
a rewrite. `UPSTREAM.json` records the package, version, upstream path and
SHA-256 of every file; `test/creator-pin.test.mjs` fails when a file drifts.

- `plugin-manager/tools.js`: `@deepseek-ai/dsh-plugin-manager` `lib/types/tools.js`
- `tool-cordis/index.js`: `@deepseek-ai/dsh-tool-cordis` `lib/index.js`
- `sandbox/escalation.js`: not yet pinned (see `UPSTREAM.json`, `pending`)

The four authoring skills of `@deepseek-ai/dsh-agent-preset` are pinned beside
the skills crate at `crates/skills/assets/dsh-creator/`.

The files are MIT licensed by DeepSeek; `LICENSE` is the licence text shipped
in each of the four packages. Nothing here is edited: change the pin by
replacing the file and its recorded hash together, at a new DSH version.
