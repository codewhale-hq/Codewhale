# Pinned hook protocol and configuration parsers

Source: DeepSeek Harness commit `0d1f50007f9bca3f52b06e1c3074fa14d5fb0720`.
Packages: `@deepseek-ai/dsh-hook-protocol`, `@deepseek-ai/dsh-hooks-claude-code`, and
`@deepseek-ai/dsh-hooks-codex`, each version `0.1.6-alpha.1`, MIT.

`UPSTREAM.json` records every original and adapted hash. Matcher, codec and merge
are verbatim. Types remove session-writer declaration merging; the two parsers
use local imports. These are six pure source files, not the upstream agent/session
runtime. The Codewhale bridge reuses the existing Rust hook catalog, per-call
receipts, Execution grants, Engine scheduler, process driver and final steering.
