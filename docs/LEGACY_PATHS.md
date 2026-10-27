# Legacy `.deepseek/` compatibility paths — audit & migration status (#3068)

> 阅读简体中文版：[zh_hans/LEGACY_PATHS.md](zh_hans/LEGACY_PATHS.md)。

## v0.9.10 cleanse ledger

This is the release-window decision record for residue that can otherwise look
interchangeable in a text search. A compatibility entry must name the contract
that still requires it; new production code must use a current path.

| Class | Surface | Decision / contract |
|---|---|---|
| DELETE | Retired underwater build-version test override | Removed in `1cc4d1d1b`; the owner test was already deleted and the production-only build version remains authoritative. |
| MIGRATE | Cross-task Agent Mail | `bd998495a` and managed-app `4ff7bc50d` replace guessed event aliases and same-session mailbox conflation with one durable typed runtime protocol. |
| MIGRATE | TUI `status_message` writes | Active residue; touched flows must move to the typed toast/current-state owner. No new writes are allowed. |
| COMPATIBILITY | `.deepseek/` state and stored provider aliases | Read fallback for upgraded installations and stored provider identity; writes remain Codewhale-only. Details are below. |
| COMPATIBILITY | `agent_message` transcript/protocol decoding | Persisted transcripts and tool wire values require decoding; it is not the cross-task Agent Mail transport. |
| COMPATIBILITY | Managed runtime envelope schema 1 | Previously persisted/enrolled runner replay; current local Codewhale Agent Mail producer is schema 2 and both normalize to the managed envelope. |
| CURRENT | DeepSeek provider support | A real selectable provider, not product branding; provider-scoped names and configuration remain. |
| CURRENT | Same-session subagent mailbox | Start/status/peek/message/followup/interrupt/wait/cancel stays scoped to a parent runtime session. |
| CURRENT | Web locale dictionaries | The application and documentation routes have no page-local `isZh` branches; new copy continues through dictionaries. |
| CURRENT | Safety, authorization, protocol, persistence, migration, and data-integrity tests | These protect external or durable contracts and are not copy/layout cleanup candidates. |

Snapshot on 2026-08-19: `10,954` Rust `#[test]` attributes under
`crates/tui`, `523` TUI `status_message` source lines, and zero page-local
`isZh` files under `apps/web/app` in the paired managed-app candidate. Counts
are inventory signals, not deletion targets.

Codewhale was renamed from DeepSeek-TUI. To avoid breaking existing installs, the runtime reads
state from the new `~/.codewhale/` location but **falls back** to the legacy `~/.deepseek/` location,
and always **writes** to `~/.codewhale/`. This doc audits each legacy reference and records a
keep / deprecate / remove decision so the migration is auditable.

## The canonical resolver (use this for new code)

State-dir resolution is consolidated in `crates/config/src/lib.rs`:

| Symbol | Line | Purpose |
|---|---|---|
| `CODEWHALE_APP_DIR` / `LEGACY_APP_DIR` | 5369 | re-exported from `crates/paths` (defined at `crates/paths/src/lib.rs` 13 / 16) |
| `codewhale_home()` | 5375 | `~/.codewhale` |
| `legacy_deepseek_home()` | 5392 | `~/.deepseek` (legacy) |
| `resolve_state_dir(subdir)` | 5434 | **read** path: `~/.codewhale/<subdir>`, falling back to `~/.deepseek/<subdir>` when only the legacy dir exists |
| `ensure_state_dir(subdir)` | 5458 | **write** path: always creates under `~/.codewhale/<subdir>` |

Migration contract: read-with-fallback, write-to-new. This preserves the v0.8.44 migration for
users who still have `~/.deepseek/` while steering all new writes to `~/.codewhale/`.

## Per-path decisions

**Decision for all legacy references below: keep-as-fallback.** Removing the `.deepseek` fallback
would strand users who upgraded in place and never re-ran onboarding. Revisit only after a release
that actively migrates `~/.deepseek/` → `~/.codewhale/` on first run and a deprecation window.

| Reference | Routed through `resolve_state_dir`? | Decision |
|---|---|---|
| `config::resolve_state_dir` / `ensure_state_dir` | n/a (the resolver itself) | keep — canonical |
| `crates/tui/src/skills/mod.rs` (`~/.deepseek/skills`) | no — hardcoded | keep-as-fallback; route through resolver in a follow-up refactor |
| `crates/tui/src/prompts.rs` (`LEGACY_HANDOFF_RELATIVE_PATH = ".deepseek/handoff.md"`) | no — explicit legacy const | keep — explicit legacy handoff fallback |
| `crates/tui/src/workspace_trust.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/tui/src/session_manager.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/runtime/src/skill_state.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/tui/src/tools/skill.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/snapshot/src/lib.rs` | no — hardcoded | keep-as-fallback; follow-up |
| `crates/runtime/src/workspace_discovery.rs` | no — hardcoded | keep-as-fallback; follow-up |

## Follow-up (separate, non-doc change — out of scope for #3068)

The optional consolidation the issue mentions — routing the hardcoded sites above through
`resolve_state_dir`/`ensure_state_dir` instead of joining `.deepseek`/`.codewhale` by hand — is a
small refactor that should land as its own PR with tests asserting read-fallback + write-to-new for
each migrated site. It is intentionally kept out of this audit so the documentation can land safely
on its own.
