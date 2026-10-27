//! Consolidated harness for plain `#[test]`/`#[tokio::test]` acceptance suites.
//!
//! See `crates/tui/tests/README.md` for why this exists: 17 small integration
//! binaries each re-linked the full `codewhale-tui` graph — one harness keeps the
//! same test names (`integration::adaptive_evidence_acceptance::...`) so
//! `cargo test -p codewhale-tui adaptive_evidence_acceptance` still filters.

#[path = "../support/binary.rs"]
mod binary;

// Production modules that are `#[path]`-included by the test files below and
// that themselves use `crate::`. They must exist at the harness crate root so
// `crate::config`, etc. resolve when the same
// files are compiled as `crate::integration::<test>::<module>`.

#[path = "../../src/eval.rs"]
mod eval;
#[path = "../support/llm_client.rs"]
mod llm_client;
// The legacy text tool-call parser lives in codewhale-core now; keep the
// `crate::tool_parser` path the suites use.
use codewhale_core::tool_parser;

mod adaptive_evidence_acceptance;
mod cache_guard;
mod coordination_acceptance;
mod diagnostic_read_only;
mod dotenv_authority;
mod eval_harness;
mod exec_persistent_service;
mod exec_stream_drop_acceptance;
mod exec_turn_usage;
mod integration_mock_llm;
mod issue_report_acceptance;
mod lifecycle_outbox_exec;
mod palette_audit;
mod protocol_recovery;
mod reasoning_content_replayed_after_tool_call;
mod shell_denial_acceptance;
mod skill_cli;
mod telemetry_contract;
mod verifiers_harness_contract;
mod workflow_tool_stream_acceptance;
