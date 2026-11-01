# TUI agent guidance

Scope: the terminal UI, its embedded runtime engine, and user-visible behavior.
Read the repository guidance first.

## UI contracts

- One owner per fact: mode, permission and live counts in the posture bar
  (`phase_strip.rs`, row 1 under the composer); model, context and the
  session metrics — cost, ttft, tok/s, output tokens — in the metrics line
  (`infoline.rs`, row 2); the roster and to-do in the work surface; receipts,
  the active row and the phase in the transcript. Key hints come from the
  `shell_key_routing` binding table, never from a string literal.
- Status-bar ink goes through `codewhale_palette::grammar` (`docs/design/STATUS_BAR_COLOR_GRAMMAR.md`).
  Do not invent an eighth semantic or spend Failure red on non-failure chrome.
- Derive state from typed enums such as `ShellPhase` and `OceanTreatment`.
  Renderers must not infer state from English strings or invent lifecycle state.
- Keep settled output still. Motion is semantic, bounded, and fully disabled by
  reduced-motion settings.
- Route notices through the toast system, with typed level and lifetime; do not
  add new writes to the legacy `status_message` sink.
- Compact layouts remove chrome before content. Selectable rows need recorded
  hitboxes, visible focus, keyboard/mouse parity, and confirmation for
  destructive actions.
- User-visible prose uses `tr(locale, MessageId::...)`. Commands, key names, and
  glyphs are composed in code. Follow `locales/AGENTS.md` for string changes.

## Runtime API has a consumer outside this repo

`runtime_api.rs` is the only API of the community VS Code extension
(`HengQuWorld/CodeWhale-VSCode`): it spawns `codewhale serve --http` and drives
every feature through `/v1/*` (`src/api/api-client.ts`). No test here exercises
that client, so an incompatible change lands silently. Additive routes and
optional fields are safe; renaming, removing, or changing a shipped route
(path, method, status, required fields, enum spelling) needs the extension's
client updated in the same change, or both shapes kept.

- The extension feature-detects by probing a route with the wrong method and
  reading anything but `404` as present. Do not add a fallback that answers
  `404` for an unrouted method; it disables those features rather than failing.
- It reads the listener port from `RUNTIME_LISTENING_PREFIX` on stdout and
  rejects any other port, then requires `401` anonymously and `200` with the
  bearer token.
- Advertise a `/v1/runtime/info` capability flag only while all its routes
  exist; the extension cannot probe a family whose detail routes answer `404`.

`docs/RUNTIME_API.md` is the written contract. That repository also holds the
TUI/GUI parity rules for endpoint changes.

## Verification

Select the smallest evidence that answers the actual risk. Direct PTY or
terminal behavior is stronger evidence for visible UX than an assertion over
render internals. Use a focused existing test for safety, data integrity,
protocol, or a reproduced regression when useful. Do not add tests by default,
and do not require both full suites or workspace Clippy for an unrelated leaf
change. These are available release/cross-cutting gates, not per-edit ritual:

```sh
cargo test -p codewhale-tui --lib --locked
cargo test -p codewhale-tui --tests --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

`--lib` and `--tests` are disjoint; choose the target that owns the behavior.
Use both or the workspace gate only when the risk genuinely spans both.
`scripts/dev-test.sh <area|path> [filter]` prints and runs the fastest
targeted invocation for a source path (for example
`scripts/dev-test.sh crates/command-contract/src/elapsed.rs`). It uses `cargo nextest
run` when nextest is installed (`CODEWHALE_DEV_NEXTEST=0` forces libtest)
and applies `scripts/dev-cache.sh` to the canonical checkout's Cargo build-dir,
respecting existing cache settings. Keep the same checkout on `main`; do not
create a worktree for parallel edits or a build. For PTY
failures, reproduce the behavior directly before changing it. Script one input
at a time and capture after the UI settles. Choose the terminal sizes relevant
to the change from 40x12, 60x16, 80x24, 100x32, and 140x40; judge motion from
repeated frames, not a single screenshot. Remove inherited `NO_COLOR`,
`TERM=dumb`, and tmux motion overrides when they would invalidate the
observation.
