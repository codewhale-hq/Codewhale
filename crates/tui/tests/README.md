# `crates/tui/tests/`

Integration tests for the TUI binary. Per `CONTRIBUTING.md`, each crate's
integration tests live in its own `tests/` directory; the repository-root
`tests/` directory is unused.

## Harness consolidation (build-time lane #5247)

`crates/tui/tests/` used to ship 26 root-level `*.rs` binaries, each linking
the full `codewhale-tui` graph plus `cucumber`/`wiremock`/`rio-vt`. That was
~26 large link jobs per `cargo test -p codewhale-tui` and a major share of the
30-minute suite in #4991.

Since #5247 the remaining files are consolidated into **2 directory harnesses** (plus
the `codewhale-tui` bin unit tests):

| harness | binary | what lives there | why it stays separate |
|---|---|---|---|
| `tests/integration/main.rs` | `integration` | 17 plain `#[test]`/`#[tokio::test]` suites: `adaptive_evidence_acceptance`, `cache_guard`, `coordination_acceptance`, `diagnostic_read_only`, `dotenv_authority`, `eval_harness`, `exec_persistent_service`, `exec_stream_drop_acceptance`, `exec_turn_usage`, `integration_mock_llm`, `palette_audit`, `protocol_recovery`, `reasoning_content_replayed_after_tool_call`, `skill_cli`, `telemetry_contract`, `verifiers_harness_contract`, `workflow_tool_stream_acceptance` | All are process-level but require no PTY or Gherkin runner; they share `wiremock`/`tempfile` and link the TUI once instead of 17 times. `crate::` for `eval`/`models`/`llm_client`/`network_policy`/`config`/`install` is satisfied by `integration/main.rs` re-exporting those `#[path]` modules at the harness crate root so `crate::config` etc. resolve. |
| `tests/cucumber/main.rs` | `cucumber` | 6 Gherkin runners: `core_session_command_extraction`, `directory_listing_acceptance`, `epic_acceptance_harness`, `eval_smoke_acceptance`, `plugin_e2e_acceptance`, `tool_lifecycle_acceptance` | Each defines a distinct `cucumber::World`; steps are registered per-World via inventory, so merging is safe and cuts 6 `cucumber` link jobs to 1. `plugin_e2e`’s PTY part is `#[cfg(all(unix, feature="long-running-tests"))]` and stays dormant in the default run. |
The former `tests/pty` harness was removed. It accumulated full-screen copy,
color, timing, and geometry assertions that were expensive to link and made the
implementation serve a simulated terminal. Visible UX is now accepted against
the actual binary in the terminal being changed.

`ls crates/tui/tests/*.rs | wc -l` is **0** (all `*.rs` live under
`integration/` and `cucumber/`). The surviving binaries are the 2 directory
harnesses above.

Filtering still works via the module path:

```sh
cargo test -p codewhale-tui --tests -- --list | grep adaptive_evidence
cargo test -p codewhale-tui --test integration adaptive_evidence_acceptance -- --nocapture
cargo test -p codewhale-tui --test cucumber tool_lifecycle -- --nocapture
```

The shared helpers in `crates/tui/tests/support/` (`qa_harness`, `llm_client`) and fixtures in `crates/tui/tests/fixtures/` are untouched — harnesses reach them via `../support`.

## Mock LLM client (`integration::integration_mock_llm`)

`crates/engine/src/llm_client/mock.rs` provides a `MockLlmClient` that implements
the `LlmClient` trait by replaying queue-driven canned responses and capturing
every outgoing `MessageRequest`. Tests mock at the **trait boundary** — never
at the `reqwest` HTTP layer — because the trait is the durable abstraction the
runtime is meant to depend on.

Coverage today exercises the trait surface end-to-end:

- streaming turn loop
- reasoning-content replay across tool-call rounds (V4 §5.1.1, the bug that
  broke v0.4.9-v0.5.1)
- tool-call round-trip with chunked input JSON
- multi-tool-call ordering inside a single turn
- compaction-style non-streaming `create_message`
- sub-agent style independent parent/child mocks
- capacity-gate observation of a captured request before stream drain

Full-engine journeys use `Engine::new_with_model_client` and the same mock.
A non-trivial model/protocol/user-visible behavior change needs one keyless
assembled journey at the nearest real entry path. Prefer semantic assertions
over large full-screen goldens; pin only the durable events, protocol text,
side effects, accounting, and next request whose exact shape is the feature.
Do not add a new snapshot framework, network call, provider key, timing sleep,
or platform shell merely to cover the journey. The Auto-Review guardian
journeys in `src/core/engine/tests.rs` are the first fixture (#5361); each
drives one mock-model tool turn through `Engine::run` and pins:

- `auto_review_guardian_allow_executes_once_and_accounts_usage_without_prompt_leak`
  — allow executes the tool exactly once, reviewer usage reaches `TurnUsage`
  and `TurnComplete`, and the reviewer rationale/audit fields never appear in
  the follow-up model request
- `auto_review_guardian_deny_returns_one_paired_failed_result` — deny yields
  one paired `is_error` tool result carrying the rationale, no orphaned call,
  no side effect
- `auto_review_guardian_parse_and_transport_failures_deny_closed` — reviewer
  parse or transport failure denies fail-closed with an `Unavailable` receipt
- `auto_review_cancellation_promptly_drops_the_guardian_request` — cancel
  drops the in-flight guardian future and interrupts the turn without a
  follow-up model request

## `--record` mode for `deepseek eval`

The offline `deepseek eval` harness now accepts `--record <DIR>`. When set,
each tool step appends one JSON Lines record to `<DIR>/<scenario>.jsonl`
(default scenario: `offline-tool-loop.jsonl`). Each line is a self-contained
JSON object with the schema:

```json
{ "request":  { "step": "list_dir", "kind": "List" },
  "response_events": [ { "type": "ok", "output": "…" } ] }
```

The mock LLM client (`crate::llm_client::mock`) replays these fixtures by
mapping each `response_events` array onto a canned `Vec<StreamEvent>`. Drop
generated fixtures into `crates/tui/tests/fixtures/` so they ride the repo and
feed the mock in CI.

Quick example:

```bash
cargo run --bin codewhale -- eval --record crates/tui/tests/fixtures
cat crates/tui/tests/fixtures/offline-tool-loop.jsonl | jq .
```

The scenario name is sanitized to `[A-Za-z0-9_-]` before forming the filename,
so unusual scenario strings stay portable across platforms.

## Canonical executable

The Engine crate is a library; its acceptance suites launch `codewhale` from
`codewhale-cli`. Build it before direct acceptance invocations:

```sh
cargo build -p codewhale-cli --bin codewhale --locked
cargo test -p codewhale-tui --test integration --locked
```

`scripts/dev-test.sh tui-integration` and `tui-cucumber` perform that build using
the same cache and hermetic test boundary. Both harnesses use one resolver:
`QA_TUI_BIN` selects an explicit QA binary, otherwise `CARGO_BIN_EXE_codewhale`
or the canonical executable beside the test harness is required. A stale
`codewhale-tui` executable is never an implicit fallback. Release aliases remain
byte-identical copies of the canonical program.
