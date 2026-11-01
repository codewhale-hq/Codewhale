# Pinned hook protocol and configuration parsers

Source: DeepSeek Harness rc.5 commit `b830ec950faba0a46dda564be3e2d854afed94f1`.
Packages: `@deepseek-ai/dsh-hook-protocol`, `@deepseek-ai/dsh-hooks-claude-code`, and
`@deepseek-ai/dsh-hooks-codex`, each version `0.2.1-rc.5`, MIT.

`UPSTREAM.json` records every original and adapted hash. Matcher, codec and merge
are verbatim. Types remove session-writer declaration merging; the two parsers
use local imports. These are six pure source files, not the upstream agent/session
runtime. The Codewhale bridge reuses the existing Rust hook catalog, per-call
receipts, Execution grants, Engine scheduler, process driver and final steering.
