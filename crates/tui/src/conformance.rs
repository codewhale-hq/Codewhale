//! Conformance harness for the Rust → TypeScript edge migration.
//!
//! `codewhale-ops/CURRENT_DECISIONS.md` §26 moves MCP, hook orchestration,
//! script tools, tool adapters and slash commands to the TypeScript extension
//! host one subsystem at a time, each behind a flag that defaults to Rust. A
//! subsystem may flip only after it meets the same expectations the Rust
//! implementation meets today, on recorded fixtures. This module is that
//! gate's Rust half.
//!
//! Every family replays committed, language-neutral fixtures from
//! `crates/tui/tests/fixtures/conformance/<family>/` through the *current*
//! production path and compares a normalized result with a committed golden:
//!
//! | family | production path under test | proves |
//! |---|---|---|
//! | `sse` | `CodewhaleClient::create_message_stream` over a loopback HTTP server | wire bytes → normalized stream events |
//! | `responses` | route resolver, `prepare_outbound_request`, and the same loopback stream | profile id, request body, and Responses termination |
//! | `events` | `Engine::run` → `run_turn` with a scripted provider | one turn → protocol `EventMsg` sequence |
//! | `mcp` | `Engine::execute_mcp_tool_with_pool` + `McpPool::to_api_tools` against a transcript server | catalog and call results for one dispatch |
//! | `prompt` | the first model request of a real turn | model-visible prefix bytes (system prompt + tool catalog) |
//! | `hooks` | `run_tool_call_before_hooks` / `HookExecutor::execute` | hook verdict fold, env contract and schema-1 stdin |
//!
//! Default is compare-only. `CODEWHALE_CONFORMANCE_UPDATE=1` rewrites goldens
//! from the current source for review in the diff; it refuses to run under CI.
//! The fixture README states the normalized formats a second implementation
//! must reproduce.
//!
//! Known limits, stated so nobody assumes them: the harness pins what the
//! Rust side does today. A provider error after a collected tool call must
//! settle it without executing it (C02-05); the invariant is mandatory. It runs
//! no real provider, MCP server binary, or Node host. The `events`, `prompt`
//! and `hooks` goldens were recorded on Unix and are compiled only there:
//! hook fixtures are POSIX shell, and a Windows turn differs in shell and
//! path facts that no recorded golden covers yet. `sse`, `responses`, and `mcp` run
//! everywhere.
#![cfg(test)]

#[cfg(unix)]
#[cfg(test)]
mod events;
#[cfg(test)]
pub(crate) mod golden;
#[cfg(unix)]
#[cfg(test)]
mod hooks;
#[cfg(test)]
mod mcp;
#[cfg(unix)]
#[cfg(test)]
mod prompt;
#[cfg(test)]
mod responses;
#[cfg(test)]
mod sse;
#[cfg(test)]
mod stream_json;
