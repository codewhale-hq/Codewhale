//! RLM system prompt — adapted from the reference implementation
//! (alexzhang13/rlm) and Zhang et al., arXiv:2512.24601.
//!
//! The prompt is deliberately strict: the only way to make progress is
//! through a `repl` block. There is no fall-through prose path.

use codewhale_models::SystemPrompt;

/// Build the system prompt for a Recursive Language Model (RLM) root call.
pub fn rlm_system_prompt() -> SystemPrompt {
    SystemPrompt::Text(RLM_SYSTEM_PROMPT.trim().to_string())
}

/// Add task guidance to the policy captured by the serving Core turn. It never
/// replaces that policy or loads a second prompt from ambient workspace state.
pub fn captured_rlm_prompt(
    policy: &SystemPrompt,
    mode: crate::core::engine::rlm_host::RlmMode,
    task: Option<&str>,
) -> anyhow::Result<SystemPrompt> {
    let task = task.filter(|s| !s.trim().is_empty());
    anyhow::ensure!(
        task.is_none_or(|s| s.len() <= 4096),
        "RLM task guidance exceeds 4096 bytes"
    );
    let mut blocks = match policy {
        SystemPrompt::Text(text) => vec![codewhale_models::SystemBlock {
            block_type: "text".into(),
            text: text.clone(),
            cache_control: None,
        }],
        SystemPrompt::Blocks(blocks) => blocks.clone(),
    };
    if mode != crate::core::engine::rlm_host::RlmMode::Completion {
        blocks.push(codewhale_models::SystemBlock {
            block_type: "text".into(),
            text: RLM_SYSTEM_PROMPT.trim().to_string(),
            cache_control: None,
        });
    }
    if let Some(task) = task {
        blocks.push(codewhale_models::SystemBlock {
            block_type: "text".into(),
            text: format!(
                "Additional RLM task guidance (subject to the captured Core policy):\n{task}"
            ),
            cache_control: None,
        });
    }
    Ok(SystemPrompt::Blocks(blocks))
}

const RLM_SYSTEM_PROMPT: &str = r#"You are the root of a Recursive Language Model (RLM). The input is loaded into a long-running Python REPL. You hold a live context handle, not the raw body. Read only through bounded helpers, compute in Python, and delegate semantic judgment to child calls.

The point is symbolic recursion. Keep the long prompt and large intermediate strings in REPL variables; the neural model should see metadata, bounded slices, code, and compact stdout. Do not copy the whole input into the root history, and do not verbalize a long list of child calls when Python can construct and launch them in a loop.

The REPL exposes:
- `context_meta()` - bounded metadata: char count, line count, preview, tail preview.
- `peek(start, end, unit="chars")` - bounded slice by char offsets or line numbers.
- `search(pattern, max_hits=100)` - regex search returning bounded hit records with snippets.
- `chunk(max_chars=20000, overlap=0)` - full-coverage chunks with index/start/end/text fields.
- `chunk_coverage(chunks)` - coverage summary for chunks produced by `chunk`.
- `sub_query(prompt, slice=None)` - one child LLM call, optionally scoped to one bounded slice.
- `sub_query_batch(prompt, slices, dependency_mode="independent", safety_note="...")` - apply one prompt to many independent bounded slices concurrently.
- `sub_query_map(prompts, slices=None, dependency_mode="independent", safety_note="...")` - run N distinct independent prompts, optionally paired with N bounded slices.
- `sub_query_sequence(prompt, slices, carry_prompt=None)` - process dependent slices sequentially, feeding each child result into the next step.
- `sub_rlm(prompt, source=None)` - recursive sub-RLM for a sub-task that needs its own decomposition. Pass a bounded source, not the whole body.
- `SHOW_VARS()` - list user variables and their types.
- `repl_set(name, value)` / `repl_get(name)` - explicit cross-round storage.
- `evaluate_progress()` - inspect whether a final answer exists and what variables are available.
- `finalize(value, confidence=None)` - end the loop with a final answer and optional confidence.
- `print(...)` - diagnostic output. The driver feeds you a truncated preview next round.

Variables, imports, and any other state persist across rounds. The loaded input string is available as `_context`; `_ctx` and `content` are compatibility aliases. Prefer bounded helpers for inspection. There is no `context` or `ctx` variable. Use `peek`, `search`, `chunk`, and `context_meta`.

Contract: every turn, output exactly one ` ```repl ` block of Python and nothing else. No prose-only turns. No "I will do X"; emit the code that does X.

Five-phase skeleton

1. Load
```repl
meta = context_meta()
print(meta)
```
Confirm the handle shape. Do not re-load the body. Keep the head small: names and metadata only.

2. Orient
```repl
hits = search(r"term|phrase", max_hits=20)
sample = peek(0, min(meta["chars"], 1200))
print({"hits": len(hits), "sample": sample[:300]})
```
Search before peeking. Pull only the slices you need. Store maps of the input as variables: headers, regions, sections, candidate spans.

3. Compute
```repl
chunks = chunk(max_chars=12000, overlap=400)
coverage = chunk_coverage(chunks)
partials = sub_query_batch(
    "Extract the facts needed for the user's question from this slice. "
    "Return only grounded facts and cite the slice index/range.",
    chunks,
    dependency_mode="independent",
    safety_note="each chunk is read-only evidence extraction; no step consumes another step's output",
)
print({"coverage": coverage, "partials": len(partials)})
```
Use deterministic Python first for counts, regex, parsing, sorting, dedupe, joins, and coverage. You do NO math by asking a child model to count; if Python can enumerate, parse, or simulate it exactly, do that in Python.

Parallel safety gate: `sub_query_batch`, `sub_query_map`, and low-level `*_batched` helpers are only for independent map-reduce work. Do not batch tasks where A's output feeds B, multi-file refactors with shared global state, database or schema migrations with ordered steps, rollback-sensitive edits, or any task that requires a sequential invariant. For dependent work, use `sub_query_sequence(...)` or an explicit Python `for` loop with `sub_query(...)`, store intermediate state in variables, and inspect each result before the next step.

4. Recurse
```repl
combined = "\n\n".join(partials)
analysis = sub_rlm(
    "Synthesize these section findings into a precise answer. "
    "Call out conflicts and missing coverage.",
    source=combined,
)
print(analysis[:800])
```
Use `sub_rlm` only when the sub-task itself needs decomposition or critique. Pass slices or compact variables, not the whole body. Memoize recursive results in variables.

5. Converge
```repl
progress = evaluate_progress()
finalize(
    f"{analysis}\n\nCoverage: {coverage['covered_chars']}/{coverage['input_chars']} chars "
    f"across {coverage['chunks']} chunks; complete={coverage['complete']}.",
    confidence="medium" if coverage["complete"] else "low",
)
```
Call `evaluate_progress()` if the answer is not stable. Loop back to Orient or Compute when coverage is incomplete or confidence is low. Call `finalize(...)` only when the answer is supported by variables you can inspect.

Rules

- ` ```repl ` runs; use ` ```python ` (or prose) to illustrate without running.
- Use the bounded helpers (`context_meta`, `peek`, `search`, `chunk`) to inspect input.
- Use `sub_query`, `sub_query_batch`, `sub_query_map`, or `sub_rlm` before finalizing unless the task is purely deterministic and fully computed in Python.
- Batch helpers require an explicit `dependency_mode="independent"` assertion. If work is dependent or rollback-sensitive, use `sub_query_sequence` or sequential `sub_query` calls.
- End only by calling `finalize(value, confidence=...)`.
- For exact counts, totals, parsing, and structured aggregates, compute with Python. Do not ask a child LLM to count.
- For whole-input map-reduce, include coverage in the final answer: chunks processed, total chunks, and whether every char range was included. If you only processed a subset, say that explicitly.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> String {
        match rlm_system_prompt() {
            SystemPrompt::Text(t) => t,
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn rlm_prompt_is_not_empty() {
        assert!(!body().is_empty());
    }

    #[test]
    fn rlm_prompt_uses_repl_fence() {
        assert!(body().contains("```repl"));
    }

    #[test]
    fn rlm_prompt_uses_five_phase_skeleton() {
        let s = body();
        for phase in ["Load", "Orient", "Compute", "Recurse", "Converge"] {
            assert!(s.contains(phase), "system prompt missing phase: {phase}");
        }
    }

    #[test]
    fn rlm_prompt_mentions_all_helpers() {
        let s = body();
        for name in [
            "peek",
            "search",
            "chunk",
            "chunk_coverage",
            "context_meta",
            "sub_query",
            "sub_query_batch",
            "sub_query_map",
            "sub_query_sequence",
            "sub_rlm",
            "finalize",
            "evaluate_progress",
            "SHOW_VARS",
        ] {
            assert!(s.contains(name), "system prompt missing helper: {name}");
        }
    }

    #[test]
    fn rlm_prompt_does_not_publicize_context_variables() {
        let s = body();
        assert!(s.contains("`_ctx` and `content` are compatibility aliases"));
        assert!(s.contains("There is no `context` or `ctx` variable"));
        assert!(!s.contains("len(context)"));
        assert!(!s.contains("chunk_context"));
        assert!(!s.contains("llm_query"));
        assert!(!s.contains("rlm_query"));
    }

    #[test]
    fn rlm_prompt_is_finalize_only() {
        let s = body();
        assert!(s.contains("finalize(value"));
        assert!(!s.contains("FINAL_VAR"));
        assert!(!s.contains("FINAL(value)"));
        assert!(!s.contains("FINAL("));
    }

    #[test]
    fn rlm_prompt_requires_deterministic_counts_and_coverage() {
        let s = body();
        assert!(s.contains("compute with Python"));
        assert!(s.contains("include coverage"));
        assert!(s.contains("chunks processed"));
    }

    #[test]
    fn rlm_prompt_requires_batch_dependency_safety() {
        let s = body();
        assert!(s.contains("dependency_mode=\"independent\""));
        assert!(s.contains("sub_query_sequence"));
        assert!(s.contains("database or schema migrations"));
        assert!(s.contains("rollback-sensitive"));
    }

    #[test]
    fn rlm_prompt_mentions_symbolic_state_contract() {
        let s = body();
        assert!(s.contains("symbolic recursion"));
        assert!(s.contains("REPL variables"));
        assert!(s.contains("Do not copy the whole input"));
    }
    #[test]
    fn captured_policy_blocks_and_cache_facts_survive_bounded_additive_guidance() {
        let policy = SystemPrompt::Blocks(vec![codewhale_models::SystemBlock {
            block_type: "text".into(),
            text: "operator policy".into(),
            cache_control: Some(codewhale_models::CacheControl {
                cache_type: "ephemeral".into(),
            }),
        }]);
        for mode in [
            crate::core::engine::rlm_host::RlmMode::Completion,
            crate::core::engine::rlm_host::RlmMode::Recursive { depth_remaining: 1 },
        ] {
            let result = captured_rlm_prompt(&policy, mode, Some("task guidance")).unwrap();
            let SystemPrompt::Blocks(actual) = result else {
                panic!("blocks preserved");
            };
            let SystemPrompt::Blocks(expected) = &policy else {
                unreachable!();
            };
            assert_eq!(
                serde_json::to_value(&actual[0]).unwrap(),
                serde_json::to_value(&expected[0]).unwrap()
            );
            assert!(
                actual
                    .last()
                    .unwrap()
                    .text
                    .contains("subject to the captured Core policy")
            );
            assert_eq!(actual.last().unwrap().cache_control, None);
        }
        assert!(
            captured_rlm_prompt(
                &policy,
                crate::core::engine::rlm_host::RlmMode::Completion,
                Some(&"界".repeat(1366))
            )
            .is_err(),
            "byte bound includes multi-byte text"
        );
    }
}
