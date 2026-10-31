use super::*;

use super::context::COMPACTION_SUMMARY_MARKER;
use super::streaming::{TOOL_CALL_END_MARKERS, TOOL_CALL_MARKER_PAIRS};
use super::turn_loop::{
    auto_review_block_tool_error, call_forces_prompt, initial_stream_error_user_message,
    preview_request_error_user_message, registered_tool_approval_required,
    registered_tool_forces_prompt, replace_runtime_mcp_tools, repo_law_must_block_without_prompt,
    requested_sandbox_escalation, sandbox_escalation_denial, workspace_write_carve_out_applies,
};
use crate::prompts::{
    InstructionSource, PromptSessionContext, system_prompt_flat_text,
    system_prompt_for_mode_with_context_skills_and_session,
};
use crate::test_support::{EnvVarGuard, lock_test_env};
use codewhale_config::ProviderKind;
use codewhale_models::{SystemBlock, Usage};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[path = "tests/extension_hooks.rs"]
mod extension_hooks;
#[path = "tests/extension_prompts.rs"]
mod extension_prompts;
#[path = "tests/profile_constitution.rs"]
mod profile_constitution;

#[path = "tests/child_host.rs"]
mod child_host;

include!("tests/test_cases_01.rs");

mod compaction;
include!("tests/test_cases_02.rs");

include!("tests/test_cases_03.rs");

include!("tests/test_cases_04.rs");

include!("tests/test_cases_05.rs");

include!("tests/test_cases_06.rs");

include!("tests/test_cases_07.rs");

include!("tests/test_cases_08.rs");

include!("tests/test_cases_09.rs");

include!("tests/test_cases_10.rs");

include!("tests/test_cases_11.rs");

include!("tests/test_cases_12.rs");

include!("tests/test_cases_13.rs");

include!("tests/test_cases_14.rs");

include!("tests/test_cases_15.rs");

include!("tests/test_cases_16.rs");

include!("tests/test_cases_17.rs");

include!("tests/test_cases_18.rs");

include!("tests/test_cases_19.rs");

mod admission_gates;
mod redacted_thinking;
mod runtime_state;
mod sse_turn_recovery;
mod tool_cancellation;
include!("tests/test_cases_20.rs");

#[path = "tests/rlm_host.rs"]
pub(crate) mod rlm_host;

#[test]
fn run_completion_real_factories_observe_shutdown_and_direct_handles_do_not() {
    let _env = lock_test_env();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let config = Config::default();
        let engine_config = || {
            let mut config = deterministic_engine_config(home.path());
            config.features.disable(Feature::Mcp);
            config.memory_enabled = false;
            config
        };
        let (direct, handle) = Engine::new(engine_config(), &config);
        assert_eq!(
            handle.wait_run_completion().await,
            EngineRunCompletion::NotObserved
        );
        drop(direct);
        let handle = spawn_engine(engine_config(), &config, None);
        let mut completion = Box::pin(handle.wait_run_completion());
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(completion.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(completion);
        handle.send(Op::Shutdown).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), handle.wait_run_completion())
                .await
                .unwrap(),
            EngineRunCompletion::NormalReturn
        );
        let (handle, task) = spawn_engine_with_authoritative_route_config(
            engine_config(),
            &config,
            EngineHostProfile::Normal,
            Arc::new(parking_lot::RwLock::new(config.clone())),
            None,
            None,
            CancellationToken::new(),
        );
        handle.send(Op::Shutdown).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), handle.wait_run_completion())
                .await
                .unwrap(),
            EngineRunCompletion::NormalReturn
        );
        task.await.unwrap();
    });
}
