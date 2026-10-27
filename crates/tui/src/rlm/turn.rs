//! Pure RLM result and feedback facts. The canonical Engine owns every model
//! request, code round, cancellation, session message and permission settlement.
use codewhale_models::{ContentBlock, Message, Role, Usage};
use std::time::Duration;

pub(crate) const STDOUT_METADATA_PREVIEW_LEN: usize = 800;
const PROMPT_PREVIEW_LEN: usize = 500;

/// How an RLM turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RlmTermination {
    /// `FINAL(value)` was called inside the REPL or `FINAL(...)` appeared
    /// at the top of the model's response on its own line.
    Final,
    /// The model failed to emit a `repl` block for too many rounds in a
    /// row. The accumulated last response text is surfaced as the answer
    /// rather than being thrown away.
    NoCode,
    /// Iteration cap reached without `FINAL`. The last root response is
    /// surfaced as the answer alongside the error.
    Exhausted,
    /// Hard error — LLM call failed, REPL crashed, timeout.
    Error,
}

/// Per-round trace entry. Surfaced in the tool result so the user can see
/// exactly what the sub-agent did.
#[derive(Debug, Clone)]
pub struct RlmRoundTrace {
    pub round: u32,
    pub code_summary: String,
    pub stdout_preview: String,
    pub had_error: bool,
    pub rpc_count: u32,
    pub elapsed_ms: u64,
}

/// Result of an RLM turn.
#[derive(Debug, Clone)]
pub struct RlmTurnResult {
    pub answer: String,
    pub iterations: u32,
    pub duration: Duration,
    pub error: Option<String>,
    pub usage: Usage,
    /// One exact frozen route/quote receipt per admitted provider request.
    /// Distinct calls are never coalesced, even when they share a route.
    pub routed_usage: Vec<crate::cost_status::RuntimeUsageRecord>,
    /// Exact routes for provider-success responses that omitted authoritative
    /// usage metadata.
    pub routed_usage_drop_records: Vec<crate::cost_status::RuntimeUsageDropRecord>,
    pub routed_usage_dropped_records: u64,
    pub termination: RlmTermination,
    /// Per-round trace. Empty when the loop never reached the REPL.
    pub trace: Vec<RlmRoundTrace>,
    /// Total sub-LLM RPCs made by the sub-agent (sum of `rpc_count` across
    /// rounds). Useful for verifying that the model engaged with `context`
    /// rather than answering directly.
    pub total_rpcs: u32,
}

impl RlmTurnResult {
    pub(crate) fn failed(error: String, duration: Duration) -> Self {
        Self {
            answer: String::new(),
            iterations: 0,
            duration,
            error: Some(error),
            usage: Usage::default(),
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_dropped_records: 0,
            termination: RlmTermination::Error,
            trace: Vec::new(),
            total_rpcs: 0,
        }
    }
    pub(crate) fn require_answer_or_error(mut self) -> Self {
        if self.termination != RlmTermination::Final
            && self.answer.trim().is_empty()
            && self.error.is_none()
        {
            self.error = Some(format!(
                "RLM ended ({:?}) after {} iteration(s) with an empty answer",
                self.termination, self.iterations
            ));
        }
        self
    }
}

pub(crate) fn metadata_text(
    prompt: &str,
    iteration: u32,
    code: Option<&str>,
    stdout: Option<&str>,
) -> String {
    extract_text_blocks(&build_metadata_message(prompt, None, iteration, code, stdout).content)
}

fn build_metadata_message(
    prompt: &str,
    root_prompt: Option<&str>,
    iteration: u32,
    previous_code: Option<&str>,
    previous_stdout: Option<&str>,
) -> Message {
    let prompt_len = prompt.chars().count();
    let prompt_preview = truncate_text(prompt, PROMPT_PREVIEW_LEN);

    let mut parts = Vec::new();
    parts.push(format!("## REPL state (round {iteration})"));
    parts.push(String::new());
    if let Some(rp) = root_prompt
        && !rp.trim().is_empty()
    {
        parts.push("**Original task** (re-shown every round)".to_string());
        parts.push(format!("> {}", truncate_text(rp.trim(), 600)));
        parts.push(String::new());
    }
    parts.push("**`_context`** — the long input lives in the REPL only".to_string());
    parts.push(format!("- Length: {prompt_len} chars"));
    parts.push(format!("- Preview: \"{prompt_preview}\""));
    parts.push(String::new());

    parts.push("**REPL helpers** (use inside ```repl blocks)".to_string());
    parts.push(
        "- `_context` / `_ctx` / `content`                       — the full input string"
            .to_string(),
    );
    parts.push(
        "- `len(_context)` / `_context[a:b]` / `_context.splitlines()` — slice it".to_string(),
    );
    parts.push(
        "- `chunk(max_chars=20000, overlap=0)` — full-coverage chunks with index/start/end/text"
            .to_string(),
    );
    parts.push(
        "- `chunk_coverage(chunks)`              — coverage report for chunk output".to_string(),
    );
    parts.push(
        "- `llm_query(prompt, model=None)`        — one-shot child LLM; `model` is ignored and child calls keep the captured Core route"
            .to_string(),
    );
    parts.push(
        "- `llm_query_batched([p1, p2, ...], dependency_mode=\"independent\")` — concurrent fan-out for independent prompts only; `model` is ignored"
            .to_string(),
    );
    parts.push(
        "- `rlm_query(prompt, model=None)`        — recursive sub-RLM; `model` is ignored"
            .to_string(),
    );
    parts.push(
        "- `rlm_query_batched([p1, p2, ...], dependency_mode=\"independent\")` — concurrent recursive sub-RLMs for independent prompts only; `model` is ignored"
            .to_string(),
    );
    parts.push(
        "- `sub_query_sequence(prompt, slices)`   — sequential child calls for A->B dependencies and rollback-sensitive work"
            .to_string(),
    );
    parts.push(
        "- Batch safety: never batch dependent steps, global-state refactors, schema migrations, or rollback-sensitive tasks"
            .to_string(),
    );
    parts.push("- `SHOW_VARS()`                          — list user variables".to_string());
    parts.push("- `repl_set(name, value)` / `repl_get(name)` — explicit store".to_string());
    parts.push(
        "- `FINAL(value)`                         — end the loop with this answer".to_string(),
    );
    parts.push(
        "- `FINAL_VAR(name)`                      — end the loop with a variable's value"
            .to_string(),
    );
    parts.push(String::new());

    if iteration > 0 {
        parts.push("**Previous round**".to_string());
        if let Some(code) = previous_code {
            parts.push(format!("- Code: {}", summarize_code(code)));
        }
        if let Some(stdout) = previous_stdout {
            let stdout_clean = stdout.trim();
            if !stdout_clean.is_empty() {
                parts.push(format!("- Stdout preview: \"{stdout_clean}\""));
            } else {
                parts.push("- Stdout: (empty)".to_string());
            }
        }
    }

    let text = parts.join("\n");

    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text,
            cache_control: None,
        }],
    }
}

pub(crate) fn summarize_code(code: &str) -> String {
    let lines: Vec<&str> = code.lines().collect();
    if lines.len() <= 8 {
        return code.to_string();
    }
    let head = lines[..4].join("\n");
    let tail = lines[lines.len() - 4..].join("\n");
    format!("{} lines:\n{head}\n…\n{tail}", lines.len())
}

fn extract_text_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the first ` ```repl ` block from `text`. Falls back to
/// ` ```python `/`` ```py `` for compatibility with prompts that learned
/// the older fence style.
///
/// The opening fence must start its own line (up to three spaces of indent,
/// as in Markdown) with no other info string, so prose that mentions a fence
/// mid-line is never code to run.
pub(crate) fn extract_repl_code(text: &str) -> Option<String> {
    let mut lines = text.split_inclusive('\n');
    let mut offset = 0;
    let code_start = loop {
        let line = lines.next()?;
        offset += line.len();
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent <= 3
            && line[indent..].strip_prefix("```").is_some_and(|info| {
                matches!(info.trim(), "repl" | "python" | "py") && line.ends_with('\n')
            })
        {
            break offset;
        }
    };
    let after_fence = &text[code_start..];
    let end_idx = if after_fence.starts_with("```") {
        0
    } else {
        after_fence.find("\n```")?
    };

    let code = after_fence[..end_idx].trim().to_string();
    if code.is_empty() {
        return None;
    }
    Some(code)
}

/// Parse a top-level `FINAL(...)` directive from the model's raw text.
/// Mirrors the reference RLM's `find_final_answer`: directive must appear
/// at the start of a line, *outside* any code fence.
pub(crate) fn parse_text_final(text: &str) -> Option<String> {
    let outside_fence = strip_code_fences(text);

    for line in outside_fence.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("FINAL_VAR(") {
            // FINAL_VAR can't be resolved from text alone — defer to REPL.
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("FINAL(") {
            let inner = rest.trim_end();
            if let Some(end) = inner.rfind(')') {
                let value = inner[..end].trim();
                if !value.is_empty() {
                    return Some(strip_quotes(value));
                }
            }
        }
    }
    None
}

fn strip_code_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let take = max_chars.saturating_sub(3);
    let mut result: String = text.chars().take(take).collect();
    result.push_str("...");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::tests::rlm_host::{Replies, run_admitted_fixture, run_fixture};
    use crate::core::events::Event;
    use crate::llm_client::mock::MockLlmClient;
    use crate::rlm::bridge::RlmUsageAccumulator;
    use codewhale_models::{MessageRequest, MessageResponse};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// One model round whose code writes a marker file, run under `gate`.
    async fn marker_round(
        gate: Option<crate::tools::codemode::NestedCallGate>,
    ) -> (RlmTurnResult, bool) {
        let workspace = tempfile::tempdir().expect("tempdir");
        let marker = workspace.path().join("rlm-round-executed.txt");
        let marker_literal = serde_json::to_string(&marker.to_string_lossy()).expect("literal");
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(MessageResponse {
            id: "mock_gated_rlm".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: format!(
                    "```repl\nfrom pathlib import Path\nPath({marker_literal}).write_text('executed')\nFINAL('ran')\n```"
                ),
                cache_control: None,
            }],
            model: "mock-model".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        });
        let client: Arc<dyn Replies> = mock;
        let (tx, _rx) = mpsc::channel(8);
        let result = run_admitted_fixture(
            client,
            "root-model".to_string(),
            "long context".to_string(),
            None,
            "child-model".to_string(),
            tx,
            0,
            RlmUsageAccumulator::new(),
            tokio::time::Instant::now() + Duration::from_secs(60),
            gate,
        )
        .await;
        (result, marker.exists())
    }

    #[tokio::test]
    async fn code_round_runs_only_when_the_gate_admits_exactly_that_code() {
        use crate::tools::codemode::{NestedCallGate, NestedCallVerdict, NestedDecision};

        let (result, ran) = marker_round(Some(NestedCallGate::admitting_for_test())).await;
        assert!(ran, "an admitted round runs: {:?}", result.error);
        assert_eq!(result.termination, RlmTermination::Final);

        let (result, ran) = marker_round(None).await;
        assert!(!ran, "no gate, no code");
        assert_eq!(result.termination, RlmTermination::Error);
        let error = result.error.expect("refusal is reported");
        assert!(error.contains("no permission gate"), "{error}");

        let refusing = NestedCallGate::answering_for_test(|_, _| NestedCallVerdict::Refused {
            error: crate::tools::spec::ToolError::permission_denied("not approved"),
            decision: NestedDecision::Denied,
        });
        let (result, ran) = marker_round(Some(refusing)).await;
        assert!(!ran, "a refused round does not run");
        let error = result.error.expect("refusal is reported");
        assert!(error.contains("not approved"), "{error}");

        // A hook-rewritten or otherwise different admitted input is not
        // permission for the code the model wrote.
        let rewriting = NestedCallGate::answering_for_test(|name, _| NestedCallVerdict::Run {
            name: name.to_string(),
            input: serde_json::json!({ "code": "print('something else')" }),
            supports_parallel: false,
            decision: NestedDecision::Auto,
            hook_context: None,
        });
        let (result, ran) = marker_round(Some(rewriting)).await;
        assert!(!ran, "a different admitted input does not run this code");
        assert!(
            result
                .error
                .is_some_and(|error| error.contains("was not this code"))
        );
    }

    #[tokio::test]
    async fn max_tokens_complete_repl_is_not_executed_or_accepted() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let marker = workspace.path().join("truncated-repl-executed.txt");
        let marker_literal = serde_json::to_string(&marker.to_string_lossy())
            .expect("marker path should serialize as a Python string literal");
        let partial = format!(
            "```repl\nfrom pathlib import Path\nPath({marker_literal}).write_text('executed')\nFINAL('partial answer')\n```"
        );
        let usage = Usage {
            input_tokens: 17,
            output_tokens: 4096,
            ..Usage::default()
        };
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(MessageResponse {
            id: "mock_truncated_rlm".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: partial,
                cache_control: None,
            }],
            model: "mock-model".to_string(),
            stop_reason: Some("max_tokens".to_string()),
            stop_sequence: None,
            container: None,
            usage: usage.clone(),
        });
        let client: Arc<dyn Replies> = mock.clone();
        let (tx, _rx) = mpsc::channel(8);

        let result = run_fixture(
            client,
            "root-model".to_string(),
            "long context".to_string(),
            None,
            "child-model".to_string(),
            tx,
            0,
        )
        .await;

        assert_eq!(result.termination, RlmTermination::Error);
        assert!(
            result.answer.is_empty(),
            "partial FINAL must not be accepted"
        );
        let error = result.error.expect("truncation must fail the RLM turn");
        assert!(error.contains("incomplete"), "{error}");
        assert!(error.contains("max_tokens"), "{error}");
        let mut expected_usage = usage.clone();
        expected_usage.request_usages = vec![usage.clone()];
        assert_eq!(
            result.usage, expected_usage,
            "billed usage must still be charged"
        );
        assert_eq!(result.routed_usage.len(), 1);
        assert_eq!(result.routed_usage[0].usage.usage, expected_usage);
        assert_eq!(result.routed_usage_dropped_records, 0);
        assert_eq!(mock.call_count(), 1, "truncation must not retry");
        assert!(
            !marker.exists(),
            "complete-looking code from a truncated response must not execute"
        );
    }

    #[tokio::test]
    async fn root_provider_success_without_usage_retains_exact_missing_receipt() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(MessageResponse {
            id: "mock_missing_rlm_usage".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "partial".to_string(),
                cache_control: None,
            }],
            model: "mock-model".to_string(),
            stop_reason: Some("max_tokens".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        });
        let client: Arc<dyn Replies> = mock;
        let (tx, _rx) = mpsc::channel(8);

        let result = run_fixture(
            client,
            "root-model".to_string(),
            "long context".to_string(),
            None,
            "child-model".to_string(),
            tx,
            0,
        )
        .await;

        assert_eq!(result.termination, RlmTermination::Error);
        assert_eq!(result.usage, Usage::default());
        assert!(result.routed_usage.is_empty());
        assert_eq!(result.routed_usage_drop_records.len(), 1);
        assert_eq!(result.routed_usage_dropped_records, 1);
        assert_eq!(
            result.routed_usage_drop_records[0].route.model,
            "root-model"
        );
    }

    fn text_response(text: &str) -> MessageResponse {
        MessageResponse {
            id: "mock_rlm_round".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            model: "mock-model".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                ..Usage::default()
            },
        }
    }

    #[tokio::test]
    async fn recursive_work_continues_past_twenty_five_rounds_and_keeps_history() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        let rounds = 26;
        let preparation_rounds = 4;
        for _ in 0..preparation_rounds {
            mock.push_message_response(text_response("I will inspect the computation next."));
        }
        for round in 0..rounds {
            let code = if round < 4 {
                format!("```repl\nx = {round}\n```")
            } else {
                format!("```repl\nprint('round {round}')\n```")
            };
            mock.push_message_response(text_response(&code));
        }
        mock.push_message_response(text_response("FINAL('complete')"));
        let client: Arc<dyn Replies> = mock.clone();
        let (tx, mut rx) = mpsc::channel(1024);
        let result = run_fixture(
            client,
            "root-model".to_string(),
            "long context".to_string(),
            None,
            "child-model".to_string(),
            tx,
            0,
        )
        .await;
        assert_eq!(
            result.termination,
            RlmTermination::Final,
            "{:?}",
            result.error
        );
        assert_eq!(result.iterations, rounds + preparation_rounds + 1);
        assert_eq!(result.answer, "complete");
        assert!(result.error.is_none(), "{:?}", result.error);
        let requests = mock.captured_requests();
        assert_eq!(requests.len(), (rounds + preparation_rounds + 1) as usize);
        assert_eq!(
            requests
                .last()
                .expect("last request")
                .messages
                .iter()
                .filter(|message| message.role == Role::Assistant)
                .count(),
            (rounds + preparation_rounds) as usize
        );
        let mut statuses = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let Event::Status { message } = event {
                statuses.push(message);
            }
        }
        assert!(
            statuses
                .iter()
                .any(|line| line.contains("RLM finished: Final")),
            "{statuses:#?}"
        );
    }

    struct PendingAfterResponses(MockLlmClient, usize, tokio::sync::Notify);

    impl Replies for PendingAfterResponses {
        fn effective_route_envelope(
            &self,
            model: &str,
            dispatched_at: chrono::DateTime<chrono::Utc>,
        ) -> crate::cost_status::EffectiveRouteEnvelope {
            self.0.effective_route_envelope(model, dispatched_at)
        }

        fn effective_max_output_tokens(&self, model: &str) -> u32 {
            self.0.effective_max_output_tokens(model)
        }

        fn create_message_boxed(
            &self,
            request: MessageRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<MessageResponse>> + Send + '_>,
        > {
            if self.0.call_count() < self.1 {
                self.0.create_message_boxed(request)
            } else {
                self.2.notify_one();
                Box::pin(std::future::pending())
            }
        }
    }

    /// Checking time only between rounds cannot stop a pending root
    /// request, a Python block, or an event send within the current round.
    #[tokio::test]
    async fn wall_clock_deadline_interrupts_pending_work_and_keeps_partial_result() {
        let _home = crate::test_support::SealedHome::new();
        use codewhale_dependencies::ExternalTool as _;
        assert!(codewhale_dependencies::Python::resolve().is_some());
        for pending_model in [true, false] {
            let partial = if pending_model {
                "```repl\nprint(_os.environ['RLM_CONTEXT_FILE'])\n```"
            } else {
                "```repl\nimport time\ntime.sleep(60)\nFINAL('too late')\n```"
            };
            let mock = MockLlmClient::new(Vec::new());
            mock.push_message_response(text_response(partial));
            let client = Arc::new(PendingAfterResponses(mock, 1, tokio::sync::Notify::new()));
            let (tx, mut rx) = mpsc::channel(32);
            let usage = RlmUsageAccumulator::new();
            // Admission reads configuration and constructs the captured route.
            // It is fixture setup, before this invocation's five-second budget.
            let caller =
                crate::core::engine::tests::rlm_host::caller_for(client.clone(), "root-model");

            let call = caller.dispatch(crate::core::engine::rlm_host::RlmInvocation {
                prompt: "long context".to_string(),
                mode: crate::core::engine::rlm_host::RlmMode::Recursive { depth_remaining: 0 },
                max_tokens: None,
                task_instructions: None,
                deadline: Some(tokio::time::Instant::now() + Duration::from_secs(5)),
                gate: Some(crate::tools::codemode::NestedCallGate::admitting_for_test()),
                events: Some(tx),
                usage: usage.clone(),
            });
            tokio::pin!(call);
            let result = tokio::time::timeout(Duration::from_secs(15), async {
                if pending_model {
                    tokio::select! {
                        biased;
                        () = client.2.notified() => {},
                        result = &mut call => panic!(
                            "pending model request was not reached: error={:?}, iterations={}, calls={}, trace={:?}",
                            result.error,
                            result.iterations,
                            client.0.call_count(),
                            result.trace
                        ),
                    }
                }
                call.await
            })
            .await
            .expect("the turn deadline must interrupt in-flight work");

            assert_eq!(result.termination, RlmTermination::Error);
            assert_eq!(
                result.answer,
                partial,
                "error={:?}, iterations={}, calls={}, trace={:?}",
                result.error,
                result.iterations,
                client.0.call_count(),
                result.trace
            );
            assert_eq!(
                result.iterations,
                if pending_model { 2 } else { 1 },
                "error={:?}, calls={}, trace={:?}",
                result.error,
                client.0.call_count(),
                result.trace
            );
            assert!(
                result
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("wall-clock deadline")
            );
            let mut expected_usage = text_response(partial).usage;
            expected_usage.request_usages = vec![text_response(partial).usage];
            assert_eq!(result.usage, expected_usage);
            assert_eq!(client.0.call_count(), 1);
            let snapshot = usage.snapshot().await;
            assert_eq!(snapshot.records.len(), 1, "keep completed usage");
            assert_eq!(snapshot.dropped_records, u64::from(pending_model));
            if pending_model {
                assert_eq!(result.trace.len(), 1, "keep completed rounds");
                assert!(!result.trace[0].had_error);
                let context_path = PathBuf::from(result.trace[0].stdout_preview.trim());
                assert!(
                    context_path.is_absolute(),
                    "REPL must report its context path"
                );
                assert!(!context_path.exists(), "timeout must clean up context");
            }
            let mut saw_timeout = false;
            while let Ok(event) = rx.try_recv() {
                if let Event::Status { message } = event {
                    saw_timeout |= message.contains("RLM finished: Error")
                        && message.contains("wall-clock deadline");
                }
            }
            assert!(saw_timeout, "timeout must record how the turn ended");
            let terminal_receipts: Vec<_> = snapshot
                .nested_events
                .iter()
                .filter(|event| {
                    event["kind"] == "status"
                        && event["content"].as_str().is_some_and(|content| {
                            content.contains("RLM finished: Error")
                                && content.contains("wall-clock deadline")
                        })
                })
                .collect();
            assert_eq!(terminal_receipts.len(), 1, "one canonical terminal receipt");
        }
    }

    #[tokio::test]
    async fn wall_clock_deadline_returns_when_event_stream_is_full() {
        let _home = crate::test_support::SealedHome::new();
        // An empty response queue fails immediately and cannot test the deadline.
        // Observe the admitted provider future before filling the host channel.
        let client = Arc::new(PendingAfterResponses(
            MockLlmClient::new(Vec::new()),
            0,
            tokio::sync::Notify::new(),
        ));
        let caller = crate::core::engine::tests::rlm_host::caller_for(client.clone(), "root-model");
        let usage = RlmUsageAccumulator::new();
        let (tx, mut rx) = mpsc::channel(1);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            async {
                // Completion uses the same host/deadline/Drop authority without
                // making Python startup part of this event-channel fixture.
                let call = caller.dispatch(crate::core::engine::rlm_host::RlmInvocation {
                    prompt: "long context".to_string(),
                    mode: crate::core::engine::rlm_host::RlmMode::Completion,
                    max_tokens: None,
                    task_instructions: None,
                    deadline: Some(tokio::time::Instant::now() + Duration::from_secs(1)),
                    gate: None,
                    events: Some(tx.clone()),
                    usage: usage.clone(),
                });
                tokio::pin!(call);
                loop {
                    tokio::select! {
                        biased;
                        () = client.2.notified() => break,
                        result = &mut call => panic!("provider request was not reached: {:?}", result.error),
                        event = rx.recv() => assert!(event.is_some()),
                    }
                }
                let _ = tx.try_send(Event::status("fixture occupies the caller event channel"));
                assert_eq!(tx.capacity(), 0, "caller event channel must be full");
                call.await
            },
        )
        .await
        .expect("deadline hand-back must not wait on a full event stream");

        assert_eq!(result.termination, RlmTermination::Error);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("wall-clock deadline"),
            "{:?}",
            result.error
        );
        assert_eq!(client.0.call_count(), 0, "no scripted response completed");
        let snapshot = usage.snapshot().await;
        assert_eq!(snapshot.dropped_records, 1, "one pending admitted request");
        assert_eq!(
            snapshot
                .nested_events
                .iter()
                .filter(|event| {
                    event["kind"] == "status"
                        && event["content"].as_str().is_some_and(|content| {
                            content.contains("RLM finished: Error")
                                && content.contains("wall-clock deadline")
                        })
                })
                .count(),
            1,
            "full host channel must retain one canonical terminal receipt"
        );
    }

    #[test]
    fn empty_answer_is_never_returned_without_an_error() {
        let empty = |termination| RlmTurnResult {
            answer: "  ".to_string(),
            iterations: 2,
            duration: Duration::ZERO,
            error: None,
            usage: Usage::default(),
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_dropped_records: 0,
            termination,
            trace: Vec::new(),
            total_rpcs: 0,
        };
        let error = empty(RlmTermination::NoCode)
            .require_answer_or_error()
            .error
            .expect("empty answer needs a reason");
        assert!(error.contains("empty answer"), "{error}");
        assert!(error.contains("NoCode"), "{error}");

        // `FINAL("")` is an answer the model chose; callers keep getting "".
        let deliberate = empty(RlmTermination::Final).require_answer_or_error();
        assert!(deliberate.error.is_none(), "{:?}", deliberate.error);
    }

    #[test]
    fn extract_repl_code_finds_simple_block() {
        let text = "Here:\n```repl\nprint('hi')\n```\nEnd.";
        let code = extract_repl_code(text).unwrap();
        assert_eq!(code, "print('hi')");
    }

    #[test]
    fn extract_repl_code_falls_back_to_python_marker() {
        let text = "Code:\n```python\nx = 1 + 2\n```";
        let code = extract_repl_code(text).unwrap();
        assert_eq!(code, "x = 1 + 2");
    }

    #[test]
    fn extract_repl_code_returns_none_when_missing() {
        assert!(extract_repl_code("Just text.").is_none());
    }

    #[test]
    fn extract_repl_code_returns_none_on_empty_block() {
        assert!(extract_repl_code("```repl\n\n```").is_none());
    }

    #[test]
    fn extract_repl_code_handles_multiple_blocks() {
        let text = "```repl\na=1\n```\n```repl\nb=2\n```";
        let code = extract_repl_code(text).unwrap();
        assert_eq!(code, "a=1");
    }

    #[test]
    fn extract_repl_code_ignores_other_fences() {
        let text = "```\nfoo\n```\n```repl\nreal_code()\n```";
        let code = extract_repl_code(text).unwrap();
        assert_eq!(code, "real_code()");
    }

    #[test]
    fn extract_repl_code_requires_a_line_anchored_fence() {
        assert!(extract_repl_code("see ```python\nx = 1\n```").is_none());
        assert!(extract_repl_code("a ```repl mention\nx = 1\n```").is_none());
        assert!(extract_repl_code("```python-output\nx = 1\n```").is_none());
        let text = "prose ```py inline\n```py\nreal()\n```";
        assert_eq!(extract_repl_code(text).as_deref(), Some("real()"));
    }

    #[test]
    fn parse_text_final_extracts_simple_value() {
        let text = "OK.\nFINAL(42)\nThanks.";
        assert_eq!(parse_text_final(text).as_deref(), Some("42"));
    }

    #[test]
    fn parse_text_final_strips_quotes() {
        let text = "FINAL(\"the answer is yes\")";
        assert_eq!(parse_text_final(text).as_deref(), Some("the answer is yes"));
    }

    #[test]
    fn parse_text_final_ignores_inside_code_fence() {
        let text =
            "Some prose.\n```repl\n# Note: when ready, call FINAL(value)\nx = 1\n```\nMore prose.";
        assert!(parse_text_final(text).is_none());
    }

    #[test]
    fn parse_text_final_returns_none_when_absent() {
        assert!(parse_text_final("just talking, no final.").is_none());
    }

    #[test]
    fn build_metadata_contains_key_information() {
        let msg = build_metadata_message("Hello, world!", None, 0, None, None);
        let text = extract_text_blocks(&msg.content);
        assert!(text.contains("context"));
        assert!(text.contains("Hello, world!"));
        assert!(text.contains("round 0"));
        assert!(text.contains("llm_query"));
        assert!(text.contains("rlm_query"));
        assert!(text.contains("FINAL"));
    }

    #[test]
    fn build_metadata_truncates_long_context_without_leaking_tail() {
        let secret_tail = "DO_NOT_LEAK_CONTEXT_TAIL";
        let prompt = format!("{}{}", "a".repeat(PROMPT_PREVIEW_LEN + 100), secret_tail);
        let msg = build_metadata_message(&prompt, None, 0, None, None);
        let text = extract_text_blocks(&msg.content);

        assert!(text.contains(&format!("- Length: {} chars", prompt.chars().count())));
        assert!(text.contains("- Preview: \""));
        assert!(text.contains("..."));
        assert!(
            !text.contains(secret_tail),
            "metadata leaked the non-preview tail of context"
        );
    }

    #[tokio::test]
    async fn build_root_request_keeps_context_tail_out_of_root_payload() {
        let secret_tail = "DO_NOT_LEAK_ROOT_REQUEST";
        let prompt = format!("{}{}", "a".repeat(PROMPT_PREVIEW_LEN + 100), secret_tail);
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(MessageResponse {
            id: "context-metadata".into(),
            r#type: "message".into(),
            role: "assistant".into(),
            content: vec![ContentBlock::Text {
                text: "```repl\nFINAL('metadata only')\n```".into(),
                cache_control: None,
            }],
            model: "root-model".into(),
            stop_reason: Some("end_turn".into()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        });
        let (tx, _rx) = mpsc::channel(64);
        let result = run_fixture(
            mock.clone(),
            "root-model".into(),
            prompt.clone(),
            Some("answer from the long context".into()),
            "child-model".into(),
            tx,
            0,
        )
        .await;
        assert_eq!(
            result.termination,
            RlmTermination::Final,
            "{:?}",
            result.error
        );
        let requests = mock.captured_requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        let payload = serde_json::to_string(request).expect("actual Core request serializes");
        assert_eq!(request.temperature, None);
        assert_eq!(request.top_p, None);
        assert!(payload.contains(&format!("- Length: {} chars", prompt.chars().count())));
        assert!(
            !payload.contains(secret_tail),
            "Core request leaked the non-preview context tail"
        );
        assert!(payload.contains("Captured operator Core policy"));
        assert!(payload.contains("answer from the long context"));
    }

    #[test]
    fn build_metadata_with_iteration_shows_previous_code() {
        let msg = build_metadata_message("Test prompt", None, 3, Some("print('hi')"), Some("hi"));
        let text = extract_text_blocks(&msg.content);
        assert!(text.contains("round 3"));
        assert!(text.contains("print('hi')"));
        assert!(text.contains("hi"));
    }

    #[test]
    fn build_metadata_includes_root_prompt() {
        let msg = build_metadata_message(
            "long context",
            Some("Summarize the security model"),
            1,
            Some("# noop"),
            Some("ok"),
        );
        let text = extract_text_blocks(&msg.content);
        assert!(text.contains("Original task"));
        assert!(text.contains("Summarize the security model"));
    }

    #[test]
    fn truncate_text_leaves_short_alone() {
        assert_eq!(truncate_text("hello", 100), "hello");
    }

    #[test]
    fn truncate_text_shortens_long_text() {
        let long = "a".repeat(1000);
        let truncated = truncate_text(&long, 10);
        assert_eq!(truncated.chars().count(), 10);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn truncate_text_is_unicode_safe() {
        let s = "日本語テスト";
        let out = truncate_text(s, 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with("..."));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn extract_text_blocks_joins_text() {
        let blocks = vec![
            ContentBlock::Text {
                text: "first".to_string(),
                cache_control: None,
            },
            ContentBlock::Thinking {
                signature: None,
                state: None,
                redacted_data: None,
                thinking: "skip".to_string(),
            },
            ContentBlock::Text {
                text: "second".to_string(),
                cache_control: None,
            },
        ];
        assert_eq!(extract_text_blocks(&blocks), "first\nsecond");
    }

    #[test]
    fn metadata_msg_role_is_user() {
        let msg = build_metadata_message("test", None, 0, None, None);
        assert_eq!(msg.role, "user");
    }

    #[test]
    fn summarize_code_keeps_short() {
        assert_eq!(summarize_code("a\nb\nc"), "a\nb\nc");
    }

    #[test]
    fn summarize_code_compresses_long() {
        let lines: Vec<String> = (0..20).map(|i| format!("line{i}")).collect();
        let code = lines.join("\n");
        let s = summarize_code(&code);
        assert!(s.starts_with("20 lines:"));
        assert!(s.contains("line0"));
        assert!(s.contains("line19"));
        assert!(s.contains("…"));
    }
}
