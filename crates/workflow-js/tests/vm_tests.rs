//! End-to-end tests for the Workflow JS runtime against a fake driver.

use std::sync::Arc;
use std::time::Duration;

use codewhale_workflow_js::testing::{FakeDriver, FakeReply};
use codewhale_workflow_js::{ProgressEvent, WorkflowJsError, WorkflowRunCancel, WorkflowVm};
use serde_json::json;

async fn run(
    driver: &Arc<FakeDriver>,
    source: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, WorkflowJsError> {
    WorkflowVm::new()
        .run_script(
            source,
            args,
            driver.clone() as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
        )
        .await
}

fn script_message(result: Result<serde_json::Value, WorkflowJsError>) -> String {
    match result {
        Err(WorkflowJsError::Script(message)) => message,
        other => panic!("expected script error, got {other:?}"),
    }
}

#[tokio::test]
async fn plain_return_value_round_trips() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(&driver, "return 1 + 1;", json!(null)).await.unwrap();
    assert_eq!(value, json!(2));
}

#[tokio::test]
async fn undefined_return_becomes_null() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(&driver, "const x = 1;", json!(null)).await.unwrap();
    assert_eq!(value, json!(null));
}

#[tokio::test]
async fn args_global_is_the_invocation_input() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        "return { sum: args.x + 1, tag: args.tags[0] };",
        json!({"x": 41, "tags": ["release"]}),
    )
    .await
    .unwrap();
    assert_eq!(value, json!({"sum": 42, "tag": "release"}));
}

#[tokio::test]
async fn checked_in_best_of_n_search_recipe_runs_with_structured_receipts() {
    let driver = Arc::new(FakeDriver::new());
    for index in 1..=2 {
        driver.on(
            // Rules match the driver-visible `TaskRequest.description`, which
            // is the full instruction text (the VM's `prompt` alias wins over
            // a short label). Match the unique per-candidate suffix line.
            &format!("candidate_id=cand_{index:03} of 2."),
            FakeReply::Complete(
                json!({
                    "candidate_id": format!("cand_{index:03}"),
                    "hypothesis": "bounded fixture",
                    "modified_paths": ["src/lib.rs"],
                    "commands_run": ["cargo test --locked"],
                    "self_verdict": "pass",
                    "known_risks": [],
                    "artifact_refs": [format!("patch:cand_{index:03}")]
                })
                .to_string(),
            ),
        );
    }
    driver.on(
        "read-only tournament judge",
        FakeReply::Complete(
            json!({
                "winner_id": "cand_001",
                "ranking": ["cand_001", "cand_002"],
                "verification_required": true,
                "reasons": ["fixture score"]
            })
            .to_string(),
        ),
    );

    let value = run(
        &driver,
        include_str!("../../../workflows/operate_best_of_n.workflow.js"),
        json!({
            "brief": "Implement the fixture",
            "strategy": "search",
            "n": 2,
            "writeRoots": ["src"],
            "model": "deepseek-v4-flash",
            "thinking": "max"
        }),
    )
    .await
    .expect("checked-in search recipe should execute");

    assert_eq!(value["scenario"], "operate-search");
    assert_eq!(value["review"]["winner_id"], "cand_001");
    assert_eq!(driver.spawn_count(), 3);
    let requests = driver.requests();
    assert_eq!(requests[0].model.as_deref(), Some("deepseek-v4-flash"));
    assert_eq!(requests[0].thinking.as_deref(), Some("max"));
    assert_eq!(requests[0].write_roots, ["src"]);
    assert_eq!(requests[2].write_authority.as_deref(), Some("read_only"));
    // Regression: the driver-visible description is the full instruction text,
    // so reply rules must target text that actually reaches the driver. If a
    // future recipe reintroduces a separate short `description` next to a long
    // `prompt`, these needles stop matching, the FakeDriver falls back to its
    // non-JSON "done:..." reply, and the structured receipts fail loudly.
    assert!(
        requests[0]
            .description
            .starts_with("You are one independent candidate")
    );
    assert!(
        requests[0]
            .description
            .contains("CANDIDATE-SPECIFIC INSTRUCTION: candidate_id=cand_001 of 2.")
    );
    assert!(
        requests[2]
            .description
            .starts_with("You are the read-only tournament judge")
    );
}

#[tokio::test]
async fn task_prompt_wins_over_description_as_driver_visible_text() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        return await task({
            description: "short progress label",
            prompt: "the real instruction",
        });
        "#,
        json!(null),
    )
    .await
    .unwrap();

    // No rules were registered, so the FakeDriver fallback echoes the
    // driver-visible description. The reply text proves the driver received
    // the prompt, not the short label.
    assert_eq!(value, json!("done:the real instruction"));
    let requests = driver.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].description, "the real instruction");
    assert_ne!(requests[0].description, "short progress label");
}

#[tokio::test]
async fn task_round_trip_carries_all_options_and_normalizes_profile() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        return await task({
            description: "implement the bounded change",
            subagentType: "implementer",
            profile: "  ALpha-1  ",
            model: "deepseek-chat",
            modelStrength: "faster",
            thinking: "low",
            cwd: "repo-a",
            worktree: true,
            writeAuthority: "worktree_write",
            writeRoots: ["crates/tui/src"],
            exactFiles: ["Cargo.toml"],
            coordinationContracts: ["public-api"],
            dependencies: ["issue-4619"],
            acceptance: ["locked tests pass"],
            allowedTools: ["read", "grep"],
            maxDepth: 2,
            tokenBudget: 5000,
            maxSteps: 4,
            wallTimeSecs: 90,
            label: "L1",
            phase: "P1",
        });
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!("done:implement the bounded change"));

    let requests = driver.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.description, "implement the bounded change");
    assert_eq!(request.subagent_type.as_deref(), Some("implementer"));
    assert_eq!(request.profile.as_deref(), Some("alpha-1"));
    assert_eq!(request.model.as_deref(), Some("deepseek-chat"));
    assert_eq!(request.model_strength.as_deref(), Some("faster"));
    assert_eq!(request.thinking.as_deref(), Some("low"));
    assert_eq!(request.cwd.as_deref(), Some("repo-a"));
    assert!(request.worktree);
    assert_eq!(request.write_authority.as_deref(), Some("worktree_write"));
    assert_eq!(request.write_roots, ["crates/tui/src"]);
    assert_eq!(request.exact_files, ["Cargo.toml"]);
    assert_eq!(request.coordination_contracts, ["public-api"]);
    assert_eq!(request.dependencies, ["issue-4619"]);
    assert_eq!(request.acceptance, ["locked tests pass"]);
    assert_eq!(
        request.allowed_tools.as_deref(),
        Some(["read".to_string(), "grep".to_string()].as_slice())
    );
    assert_eq!(request.max_depth, Some(2));
    assert_eq!(request.token_budget, Some(5000));
    assert_eq!(request.max_steps, Some(4));
    assert_eq!(request.wall_time_secs, Some(90));
    assert_eq!(request.response_schema, None);
    assert_eq!(request.label.as_deref(), Some("L1"));
    assert_eq!(request.phase.as_deref(), Some("P1"));
}

#[tokio::test]
async fn scopeless_write_tasks_dispatch_and_leave_scope_to_the_spawn_boundary() {
    // A Workflow script starts a writing child the same way the Agent tool
    // does: no declared scope, so the spawn boundary claims the workspace
    // root. The VM must not refuse it before dispatch.
    let driver = Arc::new(FakeDriver::new());
    run(
        &driver,
        r#"
        await task({ prompt: "implement it", type: "implementer" });
        await task({ prompt: "build it", type: "builder" });
        await task({ prompt: "do it", type: "general" });
        await task({ prompt: "lead it", profile: "release-lead" });
        return await task({
            prompt: "edit without a claim",
            writeAuthority: "workspace_write",
        });
        "#,
        json!(null),
    )
    .await
    .expect("scopeless write tasks dispatch");
    let requests = driver.requests();
    assert_eq!(requests.len(), 5);
    for request in &requests {
        assert!(request.write_roots.is_empty());
        assert!(request.exact_files.is_empty());
        assert!(request.coordination_contracts.is_empty());
    }
    assert_eq!(requests[0].subagent_type.as_deref(), Some("implementer"));
    assert_eq!(
        requests[4].write_authority.as_deref(),
        Some("workspace_write")
    );

    // A read-only role still cannot claim write authority.
    let driver = Arc::new(FakeDriver::new());
    let error = run(
        &driver,
        r#"return await task({ prompt: "x", type: "reviewer", writeAuthority: "workspace_write" });"#,
        json!(null),
    )
    .await
    .expect_err("read-only role with write authority")
    .to_string();
    assert!(
        error.contains("read-only roles cannot declare write-capable authority"),
        "{error}"
    );
    assert!(driver.requests().is_empty());
}

#[tokio::test]
async fn task_coordination_lists_deduplicate_with_hard_count_bounds() {
    let driver = Arc::new(FakeDriver::new());
    run(
        &driver,
        r#"
        return await task({
            prompt: "bounded edit",
            type: "implementer",
            writeAuthority: "workspace_write",
            exactFiles: ["src/a.rs", "src/a.rs"],
            dependencies: ["A", "A"],
            acceptance: ["tests pass", "tests pass"],
        });
        "#,
        json!(null),
    )
    .await
    .expect("bounded unique coordination values");
    let request = driver.requests().pop().expect("request");
    assert_eq!(request.exact_files, ["src/a.rs"]);
    assert_eq!(request.dependencies, ["A"]);
    assert_eq!(request.acceptance, ["tests pass"]);
}

#[tokio::test]
async fn task_write_paths_normalize_and_reject_escape_spellings() {
    let driver = Arc::new(FakeDriver::new());
    run(
        &driver,
        r#"return await task({
            prompt: "bounded edit",
            type: "implementer",
            writeRoots: ["./src//", "src"],
            exactFiles: ["src\\lib.rs"]
        });"#,
        json!(null),
    )
    .await
    .expect("normalized repo-relative paths");
    let request = driver.requests().pop().expect("request");
    assert_eq!(request.write_roots, ["src"]);
    assert_eq!(request.exact_files, ["src/lib.rs"]);

    for path in [
        "../outside",
        "/tmp/outside",
        "C:\\outside",
        "src/../../outside",
    ] {
        let driver = Arc::new(FakeDriver::new());
        let source = format!(
            "return await task({{ prompt: 'escape', type: 'implementer', writeRoots: [{}] }});",
            serde_json::to_string(path).expect("path json")
        );
        let message = script_message(run(&driver, &source, json!(null)).await);
        assert!(
            message.contains("repo-relative") || message.contains("traversal"),
            "{path}: {message}"
        );
        assert!(driver.requests().is_empty());
    }
}

#[tokio::test]
async fn task_read_only_roles_reject_write_escalation() {
    // Scopeless write-capable tasks dispatch (see
    // scopeless_write_tasks_dispatch_and_leave_scope_to_the_spawn_boundary);
    // a read-only role still cannot claim write authority, and contradictory
    // identities are still refused.
    for source in [
        r#"return await task({prompt: "wrong authority", type: "reviewer", writeAuthority: "workspace_write", writeRoots: ["src"]});"#,
        r#"return await task({prompt: "wrong authority", type: "scout", writeAuthority: "workspace_write", writeRoots: ["src"]});"#,
        r#"return await task({prompt: "role conflict", type: "implementer", role: "reviewer", writeRoots: ["src"]});"#,
    ] {
        let driver = Arc::new(FakeDriver::new());
        let message = script_message(run(&driver, source, json!(null)).await);
        assert!(
            message.contains("require")
                || message.contains("cannot")
                || message.contains("contradictory"),
            "{message}"
        );
        assert!(driver.requests().is_empty());
    }
}

#[tokio::test]
async fn task_implementer_identity_can_be_narrowed_to_read_only_authority() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"return await task({prompt: "verification-only plan", type: "implementer", writeAuthority: "read_only"});"#,
        json!(null),
    )
    .await
    .expect("read-only authority must safely narrow an implementer identity");
    assert_eq!(value, json!("done:verification-only plan"));
    let request = driver.requests().pop().expect("request");
    assert_eq!(request.subagent_type.as_deref(), Some("implementer"));
    assert_eq!(request.write_authority.as_deref(), Some("read_only"));
    assert!(request.write_roots.is_empty());
}

#[tokio::test]
async fn task_accepts_prompt_and_type_aliases() {
    let driver = Arc::new(FakeDriver::new());
    run(
        &driver,
        r#"return await task({ prompt: "aliased", type: "verifier" });"#,
        json!(null),
    )
    .await
    .unwrap();
    let request = &driver.requests()[0];
    assert_eq!(request.description, "aliased");
    assert_eq!(request.subagent_type.as_deref(), Some("verifier"));
}

#[tokio::test]
async fn task_title_alias_routes_to_description() {
    let driver = Arc::new(FakeDriver::new());
    run(
        &driver,
        r#"return await task({ title: "inspect the release candidate", type: "verifier" });"#,
        json!(null),
    )
    .await
    .expect("title is accepted as the task description");

    let request = &driver.requests()[0];
    assert_eq!(request.description, "inspect the release candidate");
    assert_eq!(request.subagent_type.as_deref(), Some("verifier"));
}

#[tokio::test]
async fn task_prompt_takes_precedence_over_short_description() {
    let driver = Arc::new(FakeDriver::new());
    run(
        &driver,
        r#"return await task({
            description: "Short progress summary",
            prompt: "Detailed child instructions",
            label: "fixture-compatible"
        });"#,
        json!(null),
    )
    .await
    .unwrap();
    let request = &driver.requests()[0];
    assert_eq!(request.description, "Detailed child instructions");
    assert_eq!(request.label.as_deref(), Some("fixture-compatible"));
}

#[tokio::test]
async fn task_rejects_invalid_profile_tokens() {
    for bad in ["two words", "a=b", "a\"b", "a`b", "   "] {
        let driver = Arc::new(FakeDriver::new());
        let source = format!(
            "return await task({{ description: \"x\", profile: {} }});",
            serde_json::Value::String(bad.to_string())
        );
        let message = script_message(run(&driver, &source, json!(null)).await);
        assert!(message.contains("profile"), "profile {bad:?}: {message}");
        assert_eq!(driver.spawn_count(), 0, "invalid profile must not spawn");
    }
}

#[tokio::test]
async fn task_requires_a_description() {
    let driver = Arc::new(FakeDriver::new());
    let message = script_message(run(&driver, "return await task({});", json!(null)).await);
    assert!(message.contains("description"), "{message}");
    assert_eq!(driver.spawn_count(), 0);
}

#[tokio::test]
async fn task_rejects_unknown_option_names() {
    let driver = Arc::new(FakeDriver::new());
    let message = script_message(
        run(
            &driver,
            r#"return await task({ description: "x", responseschema: {} });"#,
            json!(null),
        )
        .await,
    );
    assert!(message.contains("invalid options"), "{message}");
    assert_eq!(driver.spawn_count(), 0);
}

#[tokio::test]
async fn driver_rejection_is_catchable_in_script() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("bad", FakeReply::Reject("admission cap".to_string()));
    let value = run(
        &driver,
        r#"
        try {
            await task({ description: "bad idea" });
            return "no-throw";
        } catch (err) {
            return String(err);
        }
        "#,
        json!(null),
    )
    .await
    .unwrap();
    let text = value.as_str().unwrap();
    assert!(text.contains("admission cap"), "{text}");
}

#[tokio::test]
async fn parallel_fan_out_maps_one_failure_to_null_slot() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("beta", FakeReply::Fail("boom".to_string()));
    let value = run(
        &driver,
        r#"
        return await parallel([
            () => task({ description: "alpha" }),
            () => task({ description: "beta" }),
            () => task({ description: "gamma" }),
        ]);
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(["done:alpha", null, "done:gamma"]));
    assert_eq!(driver.spawn_count(), 3);
}

#[tokio::test]
async fn parallel_logs_a_breadcrumb_when_a_slot_is_dropped_to_null() {
    // #dogfood 0.8.67: a fan-out slot that fails for a non-schema reason still
    // resolves to null (documented resilience), but must leave a breadcrumb in
    // the run log so an operator can see why a slot came back null / nothing
    // spawned — instead of a silent "completed" with no explanation.
    let driver = Arc::new(FakeDriver::new());
    driver.on("beta", FakeReply::Fail("boom".to_string()));
    let value = run(
        &driver,
        r#"
        return await parallel([
            () => task({ description: "alpha" }),
            () => task({ description: "beta" }),
        ]);
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(["done:alpha", null]));
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::Log { message } if message.contains("dropped a failed slot")
        )),
        "a dropped parallel slot should leave a breadcrumb in the run log"
    );
}

#[tokio::test]
async fn parallel_surfaces_response_schema_errors_instead_of_null() {
    let driver = Arc::new(FakeDriver::new());
    driver.on(
        "bad schema",
        FakeReply::Complete(r#"{"refuted":"yes"}"#.to_string()),
    );

    let message = script_message(
        run(
            &driver,
            r#"
            return await parallel([
                () => task({
                    description: "bad schema",
                    responseSchema: {
                        type: "object",
                        properties: { refuted: { type: "boolean" } },
                        required: ["refuted"],
                    },
                }),
            ]);
            "#,
            json!(null),
        )
        .await,
    );

    // The default bounded repair (#5583) re-asks once — the fake's rule
    // matches the repair too, so it fails identically and the run still
    // fails loud instead of degrading to a null slot.
    assert!(message.contains("responseSchema validation"), "{message}");
    assert_eq!(
        driver.spawn_count(),
        2,
        "default repair re-asks exactly once"
    );
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::TaskSchemaRepairAttempted { attempt: 1, raw, .. }
                if raw.contains("yes")
        )),
        "the failed first attempt should be receipted before the repair"
    );
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::TaskSchemaValidationFailed { message, attempt: 2, .. }
                if message.contains("responseSchema validation")
        )),
        "schema validation error should be emitted as workflow progress"
    );
}

#[tokio::test]
async fn parallel_partial_mode_keeps_schema_failures_as_structured_slots() {
    let driver = Arc::new(FakeDriver::new());
    // Repair is disabled per-task so each slot fails terminally on its own
    // reply; the mixed fan-out then exercises partial mode directly.
    driver.on(
        "good slot",
        FakeReply::Complete(r#"{"refuted": true}"#.to_string()),
    );
    driver.on(
        "bad slot",
        FakeReply::Complete("not json at all".to_string()),
    );
    driver.on("dead slot", FakeReply::Fail("boom".to_string()));

    let value = run(
        &driver,
        r#"
        const results = await parallel([
            () => task({
                description: "good slot",
                responseSchema: { "type": "object" },
            }),
            () => task({
                description: "bad slot",
                schemaRepairAttempts: 0,
                responseSchema: { "type": "object" },
            }),
            () => task({ description: "dead slot" }),
        ], { mode: "partial" });
        return results.map((slot) =>
            slot && typeof slot === "object" && slot.__taskError !== undefined
                ? "error:" + slot.__taskError.kind
                : slot === null
                  ? "null"
                  : "value:" + JSON.stringify(slot)
        );
        "#,
        json!(null),
    )
    .await
    .expect("partial mode completes the fan-out");

    assert_eq!(
        value,
        json!([
            "value:{\"refuted\":true}",
            // The JS-level kind is the fatal "schema"; the finer decode kind
            // (json_parse) lives on the receipt events, asserted below.
            "error:schema",
            // R9 behavior change: partial mode used to drop a dead subagent
            // to `null` — indistinguishable from a slot that legitimately
            // returned nothing. It is now a typed, inspectable failure.
            "error:agent"
        ])
    );
    // Every failed slot still leaves its terminal receipt.
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::TaskSchemaValidationFailed { kind, .. } if kind == "json_parse"
        )),
        "partial mode must not swallow the schema-failure receipt"
    );
}

/// Audit R06-10: a cancel that lands while `task()` is still waiting on
/// admission ends the run instead of leaving it parked on the driver.
#[tokio::test]
async fn cancel_during_task_admission_ends_the_run() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("gate", FakeReply::HoldAdmission);
    let cancel = WorkflowRunCancel::new();
    let run_cancel = cancel.clone();
    let run_driver = driver.clone();
    let handle = tokio::spawn(async move {
        WorkflowVm::new()
            .run_script_with_cancel(
                r#"await task({ description: "gate" });"#,
                json!(null),
                run_driver as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
                run_cancel,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while driver.spawn_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task should reach admission");
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("the run must end once cancelled")
        .expect("VM task should join");
    assert!(
        matches!(result, Err(WorkflowJsError::Cancelled)),
        "{result:?}"
    );
}

#[tokio::test]
async fn parallel_partial_mode_still_fails_the_run_on_cancellation() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("hang", FakeReply::Never);
    let cancel = WorkflowRunCancel::new();
    let run_cancel = cancel.clone();
    let run_driver = driver.clone();
    let handle = tokio::spawn(async move {
        WorkflowVm::new()
            .run_script_with_cancel(
                r#"
                await parallel([
                    () => task({ description: "hang", responseSchema: { "type": "object" } }),
                ], { mode: "partial" });
                "#,
                json!(null),
                run_driver as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
                run_cancel,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while driver.spawn_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task should start");
    cancel.cancel();

    let result = handle.await.expect("VM task should join");
    assert!(
        matches!(result, Err(WorkflowJsError::Cancelled)),
        "partial mode must not downgrade cancellation into a slot value: {result:?}"
    );
}

#[tokio::test]
async fn pipeline_surfaces_response_schema_errors_instead_of_null() {
    let driver = Arc::new(FakeDriver::new());
    driver.on(
        "bad schema",
        FakeReply::Complete("not json at all".to_string()),
    );

    let message = script_message(
        run(
            &driver,
            r#"
            return await pipeline(
                ["bad schema"],
                (description) => task({
                    description,
                    schemaRepairAttempts: 0,
                    responseSchema: {
                        type: "object",
                        properties: { refuted: { type: "boolean" } },
                        required: ["refuted"],
                    },
                }),
            );
            "#,
            json!(null),
        )
        .await,
    );

    // Repair disabled: the first decode failure is terminal.
    assert!(message.contains("not valid JSON"), "{message}");
    assert_eq!(driver.spawn_count(), 1);
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::TaskSchemaValidationFailed { kind, attempt: 1, .. }
                if kind == "json_parse"
        )),
        "a disabled repair must fail terminally on attempt 1 with the parse kind"
    );
}

#[tokio::test]
async fn prose_wrapped_json_repairs_in_one_attempt() {
    let driver = Arc::new(FakeDriver::new());
    // First match wins: the repair spawn's description carries the
    // "[schema repair 2]" marker, the first attempt's does not.
    driver.on(
        "[schema repair",
        FakeReply::Complete(r#"{"refuted": true}"#.to_string()),
    );
    driver.on(
        "score the claim",
        FakeReply::Complete(
            "Sure! Happy to help. Here is my verdict:\n\
             ```json\n{\"refuted\": true}\n```\n\
             Let me know if you need anything else."
                .to_string(),
        ),
    );

    let value = run(
        &driver,
        r#"
        return await task({
            description: "score the claim",
            responseSchema: {
                type: "object",
                properties: { refuted: { type: "boolean" } },
                required: ["refuted"],
            },
        });
        "#,
        json!(null),
    )
    .await
    .expect("prose-wrapped JSON should repair in one attempt");

    assert_eq!(value, json!({ "refuted": true }));
    assert_eq!(driver.spawn_count(), 2);
    let requests = driver.requests();
    assert_eq!(requests[0].response_schema, requests[1].response_schema);
    assert!(
        requests[1].description.starts_with("[schema repair 2]"),
        "the repair spawn must identify itself: {}",
        requests[1].description
    );
    assert!(
        requests[1].description.contains("score the claim"),
        "the repair prompt must embed the original task"
    );
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::TaskSchemaRepairAttempted {
                kind, attempt: 1, raw, raw_truncated: false, ..
            } if kind == "json_parse" && raw.contains("Happy to help")
        )),
        "the prose failure should be receipted with the parse kind"
    );
    assert!(
        !driver
            .events()
            .iter()
            .any(|event| matches!(event, ProgressEvent::TaskSchemaValidationFailed { .. })),
        "a successful repair must not leave a terminal schema-failure receipt"
    );
}

#[tokio::test]
async fn schema_violation_receipt_names_the_validation_kind() {
    let driver = Arc::new(FakeDriver::new());
    driver.on(
        "[schema repair",
        FakeReply::Complete(r#"{"refuted": false}"#.to_string()),
    );
    driver.on(
        "check the gate",
        FakeReply::Complete(r#"{"refuted":"no"}"#.to_string()),
    );

    let value = run(
        &driver,
        r#"
        return await task({
            description: "check the gate",
            responseSchema: {
                type: "object",
                properties: { refuted: { type: "boolean" } },
                required: ["refuted"],
            },
        });
        "#,
        json!(null),
    )
    .await
    .expect("valid JSON of the wrong shape should repair");

    assert_eq!(value, json!({ "refuted": false }));
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::TaskSchemaRepairAttempted { kind, message, .. }
                if kind == "schema_validation"
                    && message.contains("responseSchema validation")
        )),
        "a parsed-but-invalid reply must receipt as schema_validation, not json_parse"
    );
}

#[tokio::test]
async fn schema_repair_attempts_is_bounded_at_the_parse_gate() {
    let driver = Arc::new(FakeDriver::new());
    let message = script_message(
        run(
            &driver,
            r#"
            return await task({
                description: "bound me",
                schemaRepairAttempts: 4,
                responseSchema: { "type": "object" },
            });
            "#,
            json!(null),
        )
        .await,
    );
    assert!(message.contains("bounded to 3"), "{message}");
    assert_eq!(
        driver.spawn_count(),
        0,
        "no child may spawn for a bad option"
    );
}

#[tokio::test]
async fn repair_is_refused_when_the_shared_budget_is_exhausted() {
    let driver = Arc::new(FakeDriver::new());
    // Attempt 1 is admitted with an empty pool and debits it fully at spawn.
    driver.set_budget(Some(100), 100);
    driver.on(
        "spend it all",
        FakeReply::Complete("sure thing, no JSON here".to_string()),
    );

    let message = script_message(
        run(
            &driver,
            r#"
            return await task({
                description: "spend it all",
                responseSchema: { "type": "object" },
            });
            "#,
            json!(null),
        )
        .await,
    );

    assert!(
        message.contains("repair skipped: budget exhausted"),
        "{message}"
    );
    assert_eq!(
        driver.spawn_count(),
        1,
        "the repair must not spawn on an empty pool"
    );
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::TaskSchemaValidationFailed { attempt: 1, message, .. }
                if message.contains("repair skipped: budget exhausted")
        )),
        "the refused repair must stay a schema failure with the reason named"
    );
}

#[tokio::test]
async fn legacy_task_wall_clock_does_not_prevent_schema_repair() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("[schema repair", FakeReply::Complete("{}".to_string()));
    driver.on_with_delay(
        "slow prose",
        FakeReply::Complete("eventually, still not json".to_string()),
        Duration::from_millis(1_100),
    );

    let result = run(
        &driver,
        r#"
            return await task({
                description: "slow prose",
                wallTimeSecs: 1,
                responseSchema: { "type": "object" },
            });
            "#,
        json!(null),
    )
    .await
    .expect("legacy task wall time cannot stop repair");

    assert_eq!(result, json!({}));
    assert_eq!(
        driver.spawn_count(),
        2,
        "schema repair follows the runtime's trusted limits"
    );
}

#[tokio::test]
async fn cancellation_during_repair_terminates_cleanly() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("[schema repair", FakeReply::Never);
    driver.on(
        "hang the repair",
        FakeReply::Complete("prose, no json".to_string()),
    );
    let cancel = WorkflowRunCancel::new();
    let run_cancel = cancel.clone();
    let run_driver = driver.clone();
    let handle = tokio::spawn(async move {
        WorkflowVm::new()
            .run_script_with_cancel(
                r#"
                return await task({
                    description: "hang the repair",
                    responseSchema: { "type": "object" },
                });
                "#,
                json!(null),
                run_driver as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
                run_cancel,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while driver.spawn_count() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("repair should start");
    cancel.cancel();

    let result = handle.await.expect("VM task should join");
    assert!(
        matches!(result, Err(WorkflowJsError::Cancelled)),
        "{result:?}"
    );
    assert!(
        !driver
            .events()
            .iter()
            .any(|event| matches!(event, ProgressEvent::TaskSchemaValidationFailed { .. })),
        "cancellation must not be rewritten into a schema failure"
    );
}

#[tokio::test]
async fn parallel_fail_fast_rejects_with_the_typed_slot_error() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("beta", FakeReply::Fail("boom".to_string()));
    let value = run(
        &driver,
        r#"
        try {
            await parallel([
                () => task({ description: "alpha" }),
                () => task({ description: "beta" }),
            ], { mode: "fail-fast" });
            return "no-error";
        } catch (err) {
            return (err && err.kind) + ":" + (err && err.message);
        }
        "#,
        json!(null),
    )
    .await
    .unwrap();
    let text = value.as_str().unwrap();
    assert!(
        // R9: a child that ran and failed is `agent`, distinct from the
        // `script` kind a plain `throw` in a thunk produces.
        text.starts_with("agent:") && text.contains("boom"),
        "fail-fast must reject with the typed slot error: {text}"
    );
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::Log { message } if message.contains("fail-fast slot error")
        )),
        "fail-fast must leave a breadcrumb with the slot error"
    );
}

#[tokio::test]
async fn task_errors_carry_typed_kinds() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("budget", FakeReply::BudgetExhausted("limit 10".to_string()));
    driver.on("cancelled", FakeReply::Cancelled);
    driver.on("admission", FakeReply::Reject("admission cap".to_string()));
    let value = run(
        &driver,
        r#"
        const kinds = {};
        for (const description of ["budget", "cancelled", "admission"]) {
            try {
                await task({ description });
                kinds[description] = "none";
            } catch (err) {
                kinds[description] = err && err.kind;
            }
        }
        return kinds;
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(
        value,
        json!({"budget": "budget", "cancelled": "cancelled", "admission": "admission"})
    );
}

#[tokio::test]
async fn pipeline_fail_fast_rejects_instead_of_nulling_the_item() {
    let value = run(
        &Arc::new(FakeDriver::new()),
        r#"
        const stage = async (value) => {
            if (value === 1) throw new Error("stage boom");
            return value * 2;
        };
        try {
            await pipeline([1, 2], { stages: [stage], mode: "fail-fast" });
            return "no-error";
        } catch (err) {
            return (err && err.kind) + ":" + (err && err.message);
        }
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!("script:stage boom"));
}

#[tokio::test]
async fn parallel_enforces_the_1000_item_cap_without_spawning() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        const thunks = new Array(1001).fill(() => task({ description: "x" }));
        try {
            await parallel(thunks);
            return "no-throw";
        } catch (err) {
            return String(err);
        }
        "#,
        json!(null),
    )
    .await
    .unwrap();
    let text = value.as_str().unwrap();
    assert!(text.contains("max 1000"), "{text}");
    assert_eq!(driver.spawn_count(), 0, "cap must reject before any spawn");
}

#[tokio::test]
async fn parallel_accepts_exactly_1000_items() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        const thunks = new Array(1000).fill(() => Promise.resolve(1));
        const results = await parallel(thunks);
        return results.length;
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(1000));
}

#[tokio::test]
async fn pipeline_has_no_barrier_between_stages() {
    let driver = Arc::new(FakeDriver::new());
    // Item A crawls through stage 1; item B sprints through both stages.
    driver.on_with_delay(
        "s1:A",
        FakeReply::Complete("A1".to_string()),
        Duration::from_millis(300),
    );
    driver.on_with_delay(
        "s1:B",
        FakeReply::Complete("B1".to_string()),
        Duration::from_millis(20),
    );
    driver.on_with_delay(
        "s2:B1",
        FakeReply::Complete("B2".to_string()),
        Duration::from_millis(20),
    );
    driver.on("s2:A1", FakeReply::Complete("A2".to_string()));

    let value = run(
        &driver,
        r#"
        return await pipeline(
            ["A", "B"],
            (v) => task({ description: "s1:" + v }),
            (v) => task({ description: "s2:" + v }),
        );
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(["A2", "B2"]));

    // B's stage 2 must have been requested while A was still in stage 1 —
    // per-item chains, no stage barrier.
    let descriptions = driver.request_descriptions();
    assert_eq!(descriptions[..2], ["s1:A".to_string(), "s1:B".to_string()]);
    assert_eq!(
        descriptions[2], "s2:B1",
        "expected B to reach stage 2 while A was still in stage 1: {descriptions:?}"
    );
    assert_eq!(descriptions[3], "s2:A1");
}

#[tokio::test]
async fn pipeline_stage_error_drops_only_that_item() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("s1:B", FakeReply::Fail("boom".to_string()));
    let value = run(
        &driver,
        r#"
        return await pipeline(
            ["A", "B"],
            (v) => task({ description: "s1:" + v }),
            (v) => v + "+2",
        );
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(["done:s1:A+2", null]));
}

#[tokio::test]
async fn task_throws_once_budget_spent_reaches_total() {
    let driver = Arc::new(FakeDriver::new());
    driver.set_budget(Some(100), 60);
    let value = run(
        &driver,
        r#"
        let completed = 0;
        try {
            while (true) {
                await task({ description: "chunk " + completed });
                completed++;
            }
        } catch (err) {
            return { completed, message: String(err) };
        }
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value["completed"], json!(2));
    let message = value["message"].as_str().unwrap();
    assert!(message.contains("budget exhausted"), "{message}");
    assert_eq!(driver.spawn_count(), 2);
}

#[tokio::test]
async fn budget_globals_reflect_live_driver_snapshots() {
    let driver = Arc::new(FakeDriver::new());
    driver.set_budget(Some(1000), 100);
    let value = run(
        &driver,
        r#"
        const before = budget.remaining();
        await task({ description: "one" });
        return {
            total: budget.total,
            before,
            spent: budget.spent(),
            after: budget.remaining(),
        };
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(
        value,
        json!({"total": 1000, "before": 1000, "spent": 100, "after": 900})
    );
}

#[tokio::test]
async fn unbounded_budget_reads_as_null_total_and_infinite_remaining() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        "return budget.total === null && budget.remaining() === Infinity;",
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(true));
}

#[tokio::test]
async fn sequential_work_continues_past_one_thousand_children() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        let completed = 0;
        for (let i = 0; i < 1001; i++) {
            await task({ description: "t" + i });
            completed++;
        }
        return completed;
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(1001));
    assert_eq!(driver.spawn_count(), 1001);
}

#[tokio::test]
async fn response_schema_returns_the_parsed_validated_object() {
    let driver = Arc::new(FakeDriver::new());
    driver.on(
        "check",
        FakeReply::Complete(r#"{"refuted": true, "confidence": 0.9}"#.to_string()),
    );
    let value = run(
        &driver,
        r#"
        const verdict = await task({
            description: "check the claim",
            responseSchema: {
                type: "object",
                properties: { refuted: { type: "boolean" } },
                required: ["refuted"],
            },
        });
        return verdict.refuted === true ? "refuted" : "upheld";
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!("refuted"));
    assert!(driver.requests()[0].response_schema.is_some());
}

#[tokio::test]
async fn response_schema_rejects_non_json_replies() {
    let driver = Arc::new(FakeDriver::new());
    driver.on(
        "check",
        FakeReply::Complete("definitely not json".to_string()),
    );
    let message = script_message(
        run(
            &driver,
            r#"
            return await task({
                description: "check",
                responseSchema: { type: "object" },
            });
            "#,
            json!(null),
        )
        .await,
    );
    assert!(message.contains("not valid JSON"), "{message}");
}

#[tokio::test]
async fn response_schema_rejects_schema_violations() {
    let driver = Arc::new(FakeDriver::new());
    driver.on(
        "check",
        FakeReply::Complete(r#"{"refuted": "yes"}"#.to_string()),
    );
    let message = script_message(
        run(
            &driver,
            r#"
            return await task({
                description: "check",
                responseSchema: {
                    type: "object",
                    properties: { refuted: { type: "boolean" } },
                    required: ["refuted"],
                },
            });
            "#,
            json!(null),
        )
        .await,
    );
    assert!(message.contains("responseSchema validation"), "{message}");
}

#[tokio::test]
async fn determinism_ban_date_now() {
    let driver = Arc::new(FakeDriver::new());
    let message = script_message(run(&driver, "return Date.now();", json!(null)).await);
    assert!(message.contains("Date.now()"), "{message}");
}

#[tokio::test]
async fn determinism_ban_math_random() {
    let driver = Arc::new(FakeDriver::new());
    let message = script_message(run(&driver, "return Math.random();", json!(null)).await);
    assert!(message.contains("Math.random()"), "{message}");
}

#[tokio::test]
async fn determinism_ban_new_date() {
    let driver = Arc::new(FakeDriver::new());
    let message = script_message(run(&driver, "return new Date();", json!(null)).await);
    assert!(message.contains("unavailable"), "{message}");
}

/// Explicit product surface for the sandboxed Workflow VM (#4129).
///
/// Only these Workflow-owned calls may exist on `globalThis` beyond standard
/// ECMAScript intrinsics. If a new host global is intentionally added, update
/// this list in the same PR — the fail-closed inventory test below will break
/// until the allowlist is extended deliberately.
const WORKFLOW_ALLOWED_GLOBALS: &[&str] = &[
    "task", "parallel", "pipeline", "phase", "log", "budget", "args",
];

/// Host / Node / Deno / browser surfaces that must never leak into the VM.
///
/// Standard ECMAScript intrinsics (`Object`, `Function`, `eval`, `Promise`, …)
/// remain available; this list is only host escape hatches.
const SANDBOX_BANNED_GLOBALS: &[&str] = &[
    "process",
    "require",
    "module",
    "exports",
    "__dirname",
    "__filename",
    "Buffer",
    "fs",
    "child_process",
    "os",
    "path",
    "net",
    "http",
    "https",
    "fetch",
    "XMLHttpRequest",
    "WebSocket",
    "Deno",
    "Bun",
    "Worker",
];

#[tokio::test]
async fn sandbox_exposes_only_the_documented_workflow_calls() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        return {
            task: typeof task,
            parallel: typeof parallel,
            pipeline: typeof pipeline,
            phase: typeof phase,
            log: typeof log,
            budget: typeof budget,
            args: typeof args,
        };
        "#,
        json!({"ok": true}),
    )
    .await
    .unwrap();
    assert_eq!(
        value,
        json!({
            "task": "function",
            "parallel": "function",
            "pipeline": "function",
            "phase": "function",
            "log": "function",
            "budget": "object",
            "args": "object",
        })
    );
    // Keep the constant and the live typeof probe in lockstep.
    assert_eq!(
        WORKFLOW_ALLOWED_GLOBALS,
        &[
            "task", "parallel", "pipeline", "phase", "log", "budget", "args"
        ]
    );
}

#[tokio::test]
async fn sandbox_blocks_host_filesystem_shell_network_and_env_surfaces() {
    // Each probe must either throw / reject or resolve to a clearly absent
    // binding. We never allow a successful host escape.
    let probes: &[(&str, &str)] = &[
        (
            "process.env",
            r#"
            if (typeof process !== "undefined") {
                return process.env;
            }
            throw new Error("process is unavailable");
            "#,
        ),
        (
            "require('fs')",
            r#"
            if (typeof require === "function") {
                return require("fs");
            }
            throw new Error("require is unavailable");
            "#,
        ),
        (
            "import",
            r#"
            // Dynamic import is a module-loader surface; the VM has no loader.
            return await import("fs");
            "#,
        ),
        (
            "fetch",
            r#"
            if (typeof fetch === "function") {
                return await fetch("https://example.invalid/");
            }
            throw new Error("fetch is unavailable");
            "#,
        ),
        (
            "child_process",
            r#"
            if (typeof require === "function") {
                return require("child_process");
            }
            if (typeof child_process !== "undefined") {
                return child_process;
            }
            throw new Error("child_process is unavailable");
            "#,
        ),
        (
            "Deno.env",
            r#"
            if (typeof Deno !== "undefined") {
                return Deno.env.toObject();
            }
            throw new Error("Deno is unavailable");
            "#,
        ),
    ];

    for (label, source) in probes {
        let driver = Arc::new(FakeDriver::new());
        let result = run(&driver, source, json!(null)).await;
        assert!(
            result.is_err(),
            "sandbox probe `{label}` must fail closed, got {result:?}"
        );
        // No driver side-effect is expected from a sandbox probe.
        assert_eq!(
            driver.spawn_count(),
            0,
            "probe `{label}` must not spawn tasks"
        );
    }
}

#[tokio::test]
async fn sandbox_global_inventory_fails_closed_on_new_host_leaks() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        // Own enumerable + non-enumerable names on the global object.
        // Anything beyond standard ECMAScript + the Workflow allowlist is a
        // regression that must break this test so new leaks cannot land quietly.
        const names = Reflect.ownKeys(globalThis)
            .map((k) => String(k))
            .sort();
        return names;
        "#,
        json!(null),
    )
    .await
    .unwrap();
    let names: Vec<String> = serde_json::from_value(value).expect("name list is a JSON array");

    // Fail closed: none of the banned host surfaces may appear.
    for banned in SANDBOX_BANNED_GLOBALS {
        assert!(
            !names.iter().any(|n| n == *banned),
            "banned global `{banned}` leaked into the Workflow VM: {names:?}"
        );
    }

    // Every Workflow-owned call must still be present.
    for allowed in WORKFLOW_ALLOWED_GLOBALS {
        assert!(
            names.iter().any(|n| n == *allowed),
            "expected Workflow global `{allowed}` missing from inventory: {names:?}"
        );
    }

    // Internal host helpers must not be script-visible.
    for internal in [
        "__workflow_task",
        "__workflow_log",
        "__workflow_every_slot_failed",
        "__workflow_phase",
        "__workflow_budget_total",
        "__workflow_budget_spent",
        "__workflow_budget_remaining",
    ] {
        assert!(
            !names.iter().any(|n| n == internal),
            "internal host binding `{internal}` must stay hidden: {names:?}"
        );
    }
}

#[tokio::test]
async fn sandbox_rejects_commonjs_module_loader_and_eval_style_constructors() {
    let driver = Arc::new(FakeDriver::new());
    // `eval` / `Function` are standard ES, but if they are present they must
    // still be unable to reach host modules. The banned-global inventory above
    // already fails closed if Node-style loaders appear; this probe documents
    // the intended product message for module load attempts.
    let message = script_message(
        run(
            &driver,
            r#"
            if (typeof require === "function") {
                return require("node:fs");
            }
            throw new Error("require is unavailable");
            "#,
            json!(null),
        )
        .await,
    );
    assert!(
        message.contains("unavailable") || message.contains("require"),
        "{message}"
    );
}

#[tokio::test]
async fn dropping_the_run_future_cancels_outstanding_tasks() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("hang", FakeReply::Never);
    let vm = WorkflowVm::new();
    {
        let fut = vm.run_script(
            "await task({ description: 'hang forever' }); return 'unreachable';",
            json!(null),
            driver.clone() as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
        );
        let outcome = tokio::time::timeout(Duration::from_millis(400), fut).await;
        assert!(outcome.is_err(), "run should still be pending at timeout");
        // The timed-out future is dropped here.
    }
    assert!(
        driver.cancel_all_calls() >= 1,
        "dropping the run future must cancel outstanding driver tasks"
    );
    assert_eq!(driver.spawn_count(), 1);
}

#[tokio::test]
async fn parallel_does_not_continue_after_external_run_cancellation() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("hang", FakeReply::Never);
    let cancel = WorkflowRunCancel::new();
    let run_cancel = cancel.clone();
    let run_driver = driver.clone();
    let handle = tokio::spawn(async move {
        WorkflowVm::new()
            .run_script_with_cancel(
                r#"
                await parallel([() => task({ description: "hang" })]);
                phase("unreachable after cancellation");
                return "wrong";
                "#,
                json!(null),
                run_driver as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
                run_cancel,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while driver.spawn_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task should start");
    cancel.cancel();

    let result = handle.await.expect("VM task should join");
    assert!(
        matches!(result, Err(WorkflowJsError::Cancelled)),
        "{result:?}"
    );
    assert!(
        !driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::Phase { title } if title == "unreachable after cancellation"
        )),
        "parallel() must not downgrade run cancellation into a null slot"
    );
}

#[tokio::test]
async fn script_error_rejects_cleanly_and_cancels_children() {
    let driver = Arc::new(FakeDriver::new());
    let result = run(
        &driver,
        r#"await task({ description: "quick" }); throw new Error("boom");"#,
        json!(null),
    )
    .await;
    let message = script_message(result);
    assert!(message.contains("boom"), "{message}");
    assert!(
        driver.cancel_all_calls() >= 1,
        "a failed run must cancel its cascade"
    );
}

#[tokio::test]
async fn log_and_phase_events_reach_the_driver_in_order() {
    let driver = Arc::new(FakeDriver::new());
    run(
        &driver,
        r#"
        phase("scan");
        log("a");
        log({ found: 2 });
        phase("verify");
        log("b");
        return null;
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(
        driver.events(),
        vec![
            ProgressEvent::Phase {
                title: "scan".to_string()
            },
            ProgressEvent::Log {
                message: "a".to_string()
            },
            ProgressEvent::Log {
                message: r#"{"found":2}"#.to_string()
            },
            ProgressEvent::Phase {
                title: "verify".to_string()
            },
            ProgressEvent::Log {
                message: "b".to_string()
            },
        ]
    );
}

#[tokio::test]
async fn promise_all_of_tasks_resolves_concurrently() {
    let driver = Arc::new(FakeDriver::new());
    driver.on_with_delay(
        "left",
        FakeReply::Complete("L".to_string()),
        Duration::from_millis(50),
    );
    driver.on_with_delay(
        "right",
        FakeReply::Complete("R".to_string()),
        Duration::from_millis(50),
    );
    let started = std::time::Instant::now();
    let value = run(
        &driver,
        r#"
        const [a, b] = await Promise.all([
            task({ description: "left" }),
            task({ description: "right" }),
        ]);
        return a + "/" + b;
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!("L/R"));
    // Two 50ms tasks awaited concurrently should not take ~100ms serially.
    // Generous bound to stay green on slow CI.
    assert!(
        started.elapsed() < Duration::from_millis(3000),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(driver.spawn_count(), 2);
}

#[tokio::test]
async fn export_default_async_function_runs_with_args() {
    let driver = Arc::new(FakeDriver::new());
    let source = r#"
export default async function (args) {
  return { doubled: args.n * 2 };
}
"#;
    let value = run(&driver, source, json!({ "n": 21 })).await.unwrap();
    assert_eq!(value, json!({ "doubled": 42 }));
}

#[tokio::test]
async fn export_default_function_result_becomes_run_result() {
    let driver = Arc::new(FakeDriver::new());
    let source = r#"
function helper() {
  return "from-helper";
}
export default function () {
  return helper();
}
"#;
    let value = run(&driver, source, json!(null)).await.unwrap();
    assert_eq!(value, json!("from-helper"));
}

#[tokio::test]
async fn export_default_non_function_value_is_returned() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(&driver, "export default 7;", json!(null))
        .await
        .unwrap();
    assert_eq!(value, json!(7));
}

#[tokio::test]
async fn plain_scripts_are_untouched_by_export_desugaring() {
    let driver = Arc::new(FakeDriver::new());
    // A string literal mentioning `export default` must not trigger the
    // module desugaring path.
    let value = run(
        &driver,
        "const note = \"export default docs\";\nreturn note.length;",
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(19));
}

#[tokio::test]
async fn export_default_examples_inside_multiline_text_are_not_desugared() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
const template = `
export default async function (args) {
  return args;
}
`;
/*
export default function () {
  return "comment example";
}
*/
return template.includes("export default async function");
"#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!(true));
}

#[tokio::test]
async fn task_accepts_agent_tool_spellings() {
    // The `agent` tool and `task()` are written by the same authors; a schema
    // that runs on one surface must not be an unknown-field error on the
    // other. snake_case spellings and `workspace_policy` are aliases.
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        return await task({
            prompt: "cross-surface schema",
            subagent_type: "implementer",
            workspace_policy: "worktree",
            write_authority: "worktree_write",
            write_roots: ["crates/tui/src"],
            token_budget: 5000,
            max_steps: 4,
        });
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!("done:cross-surface schema"));
    let requests = driver.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].worktree,
        "workspace_policy worktree maps to worktree isolation"
    );
    assert_eq!(
        requests[0].write_authority.as_deref(),
        Some("worktree_write")
    );
    assert_eq!(requests[0].token_budget, Some(5000));

    // "shared" is accepted and stays non-worktree; contradictions and unknown
    // values still fail loudly.
    let error = run(
        &driver,
        r#"return await task({ prompt: "x", workspacePolicy: "shared", worktree: true });"#,
        json!(null),
    )
    .await
    .unwrap_err();
    assert!(script_message(Err(error)).contains("conflicts with worktree"));
    let error = run(
        &driver,
        r#"return await task({ prompt: "x", workspacePolicy: "solo" });"#,
        json!(null),
    )
    .await
    .unwrap_err();
    assert!(script_message(Err(error)).contains("must be shared or worktree"));
}

#[tokio::test]
async fn vm_rejected_task_options_notify_the_driver() {
    // A task() whose options fail VM validation throws before spawn_task, and
    // inside parallel() that throw collapses to a null slot. The driver must
    // still receive a TaskRejected event so the run record can refuse to call
    // the run a plain success (morning-report issue #2).
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        return await parallel([
            () => task({ prompt: "bad slot", label: "L-bad", phase: "P1", cwd: "/absolute/path" }),
        ]);
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!([null]));
    assert!(
        driver.requests().is_empty(),
        "no dispatch reached the driver"
    );
    let rejected: Vec<_> = driver
        .events()
        .into_iter()
        .filter_map(|event| match event {
            ProgressEvent::TaskRejected {
                label,
                phase,
                message,
            } => Some((label, phase, message)),
            _ => None,
        })
        .collect();
    assert_eq!(rejected.len(), 1, "one rejection event per refused slot");
    let (label, phase, message) = &rejected[0];
    assert_eq!(label.as_deref(), Some("L-bad"));
    assert_eq!(phase.as_deref(), Some("P1"));
    assert!(message.contains("bounded repo-relative paths"), "{message}");
}

// ---------------------------------------------------------------------------
// R9: typed slot errors, inspectable settled failures, explicit modes.
// ---------------------------------------------------------------------------

/// Every way a `task()` can die gets its own kind, assigned by the host where
/// the failure happened. Before R9 all six collapsed into two buckets
/// ("budget"/"cancelled" if the message happened to say so, "task" otherwise),
/// so a dead subagent and a typo'd script throw were the same thing.
#[tokio::test]
async fn every_task_failure_mode_carries_its_own_kind() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("agent case", FakeReply::Fail("boom".to_string()));
    driver.on(
        "budget case",
        FakeReply::BudgetExhausted("limit 10".to_string()),
    );
    driver.on("cancelled case", FakeReply::Cancelled);
    driver.on(
        "admission case",
        FakeReply::Reject("admission cap".to_string()),
    );
    driver.on(
        "driver case",
        FakeReply::Unavailable("driver gone".to_string()),
    );
    driver.on("dropped case", FakeReply::DropCompletion);
    driver.on(
        "schema case",
        FakeReply::Complete("not json at all".to_string()),
    );

    let value = run(
        &driver,
        r#"
        const kinds = {};
        const probe = async (name, opts) => {
            try {
                await task(opts);
                kinds[name] = "none";
            } catch (err) {
                kinds[name] = err && err.kind;
            }
        };
        await probe("agent", { description: "agent case" });
        await probe("budget", { description: "budget case" });
        await probe("cancelled", { description: "cancelled case" });
        await probe("admission", { description: "admission case" });
        await probe("driver", { description: "driver case" });
        await probe("dropped", { description: "dropped case" });
        await probe("schema", {
            description: "schema case",
            schemaRepairAttempts: 0,
            responseSchema: { type: "object" },
        });
        // A malformed options object never reaches a child either.
        await probe("bad-options", { description: "x", nosuchoption: 1 });
        try {
            await task("not an object");
            kinds["not-an-object"] = "none";
        } catch (err) {
            kinds["not-an-object"] = String(err && err.kind);
        }
        return kinds;
        "#,
        json!(null),
    )
    .await
    .unwrap();

    assert_eq!(
        value,
        json!({
            "agent": "agent",
            "budget": "budget",
            "cancelled": "cancelled",
            "admission": "admission",
            "driver": "driver",
            "dropped": "driver",
            "schema": "schema",
            "bad-options": "admission",
            // A TypeError raised by the prelude's own argument check never
            // came from the host, so it is a script error, not a task kind.
            "not-an-object": "undefined",
        })
    );
}

/// The classifier reads `Error.kind`, never the message text. A child is free
/// to say "budget exhausted" or "responseSchema" in its own failure prose;
/// under the old substring classifier that forged a fatal kind and aborted an
/// otherwise healthy fan-out.
#[tokio::test]
async fn slot_kinds_cannot_be_forged_from_child_failure_text() {
    let driver = Arc::new(FakeDriver::new());
    driver.on(
        "liar",
        FakeReply::Fail(
            "the reviewer said the run cancelled because budget exhausted and responseSchema \
             validation failed"
                .to_string(),
        ),
    );

    let value = run(
        &driver,
        r#"
        const results = await parallel([
            () => task({ description: "liar" }),
            () => task({ description: "honest" }),
        ]);
        return {
            slots: results,
            kinds: results.errors.map((entry) => entry.kind),
        };
        "#,
        json!(null),
    )
    .await
    .expect("a child's prose must not cancel the run");

    assert_eq!(
        value,
        json!({
            "slots": [null, "done:honest"],
            "kinds": ["agent"],
        })
    );
}

/// The settled default is unchanged on the wire — same slots, same length,
/// same JSON — but the run can now ask why a slot is null instead of guessing.
#[tokio::test]
async fn settled_parallel_keeps_null_slots_and_attaches_an_inspectable_ledger() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("beta", FakeReply::Fail("boom".to_string()));
    driver.on(
        "delta",
        FakeReply::BudgetExhausted("pool drained".to_string()),
    );

    let value = run(
        &driver,
        r#"
        const results = await parallel([
            () => task({ description: "alpha" }),
            () => task({ description: "beta" }),
            () => task({ description: "gamma" }),
            () => task({ description: "delta" }),
        ]);
        return {
            slots: results,
            length: results.length,
            // Non-enumerable: the array still serializes as a plain array.
            encoded: JSON.stringify(results),
            errors: results.errors.map((entry) => ({
                index: entry.index,
                kind: entry.kind,
                says: entry.message.indexOf("boom") !== -1
                    || entry.message.indexOf("pool drained") !== -1,
            })),
        };
        "#,
        json!(null),
    )
    .await
    .unwrap();

    assert_eq!(
        value,
        json!({
            "slots": ["done:alpha", null, "done:gamma", null],
            "length": 4,
            "encoded": "[\"done:alpha\",null,\"done:gamma\",null]",
            "errors": [
                {"index": 1, "kind": "agent", "says": true},
                {"index": 3, "kind": "budget", "says": true},
            ],
        })
    );
}

/// A clean fan-out still gets the ledger, empty and frozen — a script can read
/// `results.errors.length` unconditionally.
#[tokio::test]
async fn a_clean_fan_out_still_reports_an_empty_frozen_error_ledger() {
    let value = run(
        &Arc::new(FakeDriver::new()),
        r#"
        const results = await parallel([() => task({ description: "alpha" })]);
        let mutated = false;
        try {
            results.errors = ["forged"];
            mutated = true;
        } catch (_) {
            mutated = false;
        }
        return {
            count: results.errors.length,
            frozen: Object.isFrozen(results.errors),
            mutated: mutated,
            stillEmpty: results.errors.length,
        };
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(
        value,
        json!({"count": 0, "frozen": true, "mutated": false, "stillEmpty": 0})
    );
}

/// `settled` is the spelling of today's default; naming it explicitly changes
/// nothing.
#[tokio::test]
async fn explicit_settled_mode_matches_the_default() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("beta", FakeReply::Fail("boom".to_string()));
    let value = run(
        &driver,
        r#"
        const thunks = () => [
            () => task({ description: "alpha" }),
            () => task({ description: "beta" }),
        ];
        const implicit = await parallel(thunks());
        const explicit = await parallel(thunks(), { mode: "settled" });
        return {
            implicit: implicit,
            explicit: explicit,
            sameKinds: JSON.stringify(implicit.errors.map((e) => e.kind))
                === JSON.stringify(explicit.errors.map((e) => e.kind)),
        };
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(
        value,
        json!({
            "implicit": ["done:alpha", null],
            "explicit": ["done:alpha", null],
            "sameKinds": true,
        })
    );
}

/// A typo'd mode used to read as `settled`: the author believed slots were
/// now fatal while they kept silently dropping. It throws instead.
#[tokio::test]
async fn an_unknown_mode_is_refused_rather_than_silently_settled() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(
        &driver,
        r#"
        const errs = [];
        for (const mode of ["failfast", "all-settled", 7]) {
            try {
                await parallel([() => task({ description: "alpha" })], { mode });
                errs.push("no-error");
            } catch (err) {
                errs.push(err.message);
            }
        }
        try {
            await pipeline([1], { stages: [(v) => v], mode: "failfast" });
            errs.push("no-error");
        } catch (err) {
            errs.push(err.message);
        }
        return errs;
        "#,
        json!(null),
    )
    .await
    .unwrap();
    let messages = value.as_array().unwrap();
    assert_eq!(messages.len(), 4, "{value}");
    for message in messages {
        let text = message.as_str().unwrap();
        assert!(
            text.contains("unknown mode") && text.contains("settled, fail-fast, partial"),
            "{text}"
        );
    }
    assert_eq!(
        driver.spawn_count(),
        0,
        "a refused mode must not spawn anything"
    );
}

/// Partial mode is the "inspect every outcome" contract: no failure is erased,
/// and none of them can be mistaken for a value.
#[tokio::test]
async fn partial_mode_types_every_non_cancellation_failure() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("dead", FakeReply::Fail("boom".to_string()));
    driver.on("broke", FakeReply::BudgetExhausted("drained".to_string()));
    driver.on("refused", FakeReply::Reject("admission cap".to_string()));

    let value = run(
        &driver,
        r#"
        const results = await parallel([
            () => task({ description: "alive" }),
            () => task({ description: "dead" }),
            () => task({ description: "broke" }),
            () => task({ description: "refused" }),
        ], { mode: "partial" });
        return {
            shapes: results.map((slot) =>
                slot && typeof slot === "object" && slot.__taskError
                    ? slot.__taskError.kind + "@" + slot.__taskError.index
                    : String(slot)
            ),
            ledger: results.errors.map((entry) => entry.kind),
            noNulls: results.every((slot) => slot !== null),
        };
        "#,
        json!(null),
    )
    .await
    .expect("partial mode completes the fan-out");

    assert_eq!(
        value,
        json!({
            "shapes": ["done:alive", "agent@1", "budget@2", "admission@3"],
            "ledger": ["agent", "budget", "admission"],
            "noNulls": true,
        })
    );
}

/// `pipeline` speaks the same three modes and keeps the same ledger.
#[tokio::test]
async fn pipeline_supports_settled_fail_fast_and_partial_with_a_ledger() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("bad-1", FakeReply::Fail("boom".to_string()));

    let value = run(
        &driver,
        r#"
        const stage = (item) => task({ description: item });
        const settled = await pipeline(["ok-0", "bad-1", "ok-2"], stage);
        const partial = await pipeline(["ok-0", "bad-1"], {
            stages: [stage],
            mode: "partial",
        });
        let failFast = "no-error";
        try {
            await pipeline(["ok-0", "bad-1"], { stages: [stage], mode: "fail-fast" });
        } catch (err) {
            failFast = err.kind + ":" + (err.message.indexOf("boom") !== -1);
        }
        return {
            settled: settled,
            settledLedger: settled.errors.map((e) => e.index + ":" + e.kind),
            partial: partial.map((slot) =>
                slot && typeof slot === "object" && slot.__taskError
                    ? slot.__taskError.kind
                    : slot
            ),
            failFast: failFast,
        };
        "#,
        json!(null),
    )
    .await
    .unwrap();

    assert_eq!(
        value,
        json!({
            "settled": ["done:ok-0", null, "done:ok-2"],
            "settledLedger": ["1:agent"],
            "partial": ["done:ok-0", "agent"],
            "failFast": "agent:true",
        })
    );
}

/// A fan-out where nothing survived is a dead fan-out. The default still
/// resolves (existing scripts keep working) but the run log says so in a line
/// the host status classifier and an operator can both find.
#[tokio::test]
async fn a_fan_out_where_every_slot_failed_says_so_in_the_run_log() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("doomed", FakeReply::Fail("boom".to_string()));
    let value = run(
        &driver,
        r#"
        const results = await parallel([
            () => task({ description: "doomed a" }),
            () => task({ description: "doomed b" }),
        ]);
        return { slots: results, failed: results.errors.length };
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(value, json!({"slots": [null, null], "failed": 2}));
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::Log { message }
                if message.contains("every slot failed (2 of 2)")
                    && message.contains("no work survived")
        )),
        "a fully-failed fan-out must be named in the run log: {:?}",
        driver.events()
    );
}

/// Cancellation stays fatal in every mode — it is the run's deadline, not a
/// per-slot outcome — and partial mode does not get to keep it as a value.
#[tokio::test]
async fn cancellation_is_fatal_in_partial_pipeline_mode() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("hang", FakeReply::Never);
    let cancel = WorkflowRunCancel::new();
    let run_cancel = cancel.clone();
    let run_driver = driver.clone();
    let handle = tokio::spawn(async move {
        WorkflowVm::new()
            .run_script_with_cancel(
                r#"
                await pipeline(["hang"], {
                    stages: [(item) => task({ description: item })],
                    mode: "partial",
                });
                "#,
                json!(null),
                run_driver as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
                run_cancel,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while driver.spawn_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task should start");
    cancel.cancel();

    let result = handle.await.expect("VM task should join");
    assert!(
        matches!(result, Err(WorkflowJsError::Cancelled)),
        "pipeline partial mode must not downgrade cancellation into a slot value: {result:?}"
    );
}

/// The dropped-slot breadcrumb now names the kind, so the run log alone
/// distinguishes "the agent failed" from "we never got to spawn it".
#[tokio::test]
async fn the_dropped_slot_breadcrumb_names_the_kind_and_the_slot() {
    let driver = Arc::new(FakeDriver::new());
    driver.on("beta", FakeReply::Fail("boom".to_string()));
    run(
        &driver,
        r#"
        return await parallel([
            () => task({ description: "alpha" }),
            () => task({ description: "beta" }),
        ]);
        "#,
        json!(null),
    )
    .await
    .unwrap();
    assert!(
        driver.events().iter().any(|event| matches!(
            event,
            ProgressEvent::Log { message }
                if message.contains("dropped a failed slot as null")
                    && message.contains("kind=agent")
                    && message.contains("slot 1")
        )),
        "the breadcrumb must name the kind and the slot: {:?}",
        driver.events()
    );
}

struct EchoInvoker;

#[async_trait::async_trait]
impl codewhale_workflow_js::ToolInvoker for EchoInvoker {
    async fn invoke(
        &self,
        request: codewhale_workflow_js::ToolCallRequest,
    ) -> Result<codewhale_workflow_js::ToolCallResponse, codewhale_workflow_js::DriverError> {
        use codewhale_workflow_js::{DriverError, ToolCallResponse};
        if request.tool == "boom" {
            return Ok(ToolCallResponse {
                ok: false,
                result: json!("kaput"),
            });
        }
        if request.tool == "deny" {
            return Err(DriverError::Rejected("nope".to_string()));
        }
        Ok(ToolCallResponse {
            ok: true,
            result: json!({ "echo": request.input }),
        })
    }
}

async fn run_tools(source: &str) -> Result<serde_json::Value, WorkflowJsError> {
    let driver = Arc::new(FakeDriver::new());
    WorkflowVm::new()
        .run_tools_script(
            source,
            json!(null),
            driver.clone() as Arc<dyn codewhale_workflow_js::WorkflowDriver>,
            Arc::new(EchoInvoker) as Arc<dyn codewhale_workflow_js::ToolInvoker>,
            WorkflowRunCancel::new(),
        )
        .await
}

#[tokio::test]
async fn tools_surface_is_absent_without_invoker() {
    let driver = Arc::new(FakeDriver::new());
    let value = run(&driver, "return typeof tools;", json!(null))
        .await
        .unwrap();
    assert_eq!(value, json!("undefined"));
}

#[tokio::test]
async fn tools_call_round_trips() {
    let value =
        run_tools(r#"const r = await tools.call("read", { path: "x" }); return r.echo.path;"#)
            .await
            .unwrap();
    assert_eq!(value, json!("x"));
}

#[tokio::test]
async fn native_callbacks_stay_on_the_originating_host_runtime() {
    use codewhale_workflow_js::{
        BudgetSnapshot, DriverError, SpawnedTask, TaskRequest, ToolCallRequest, ToolCallResponse,
        ToolInvoker, WorkflowDriver,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct OriginCallbacks {
        origin: std::thread::ThreadId,
        calls: AtomicUsize,
        driver: FakeDriver,
    }

    #[async_trait::async_trait]
    impl WorkflowDriver for OriginCallbacks {
        async fn spawn_task(&self, request: TaskRequest) -> Result<SpawnedTask, DriverError> {
            assert_eq!(std::thread::current().id(), self.origin);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.driver.spawn_task(request).await
        }

        fn cancel_all(&self) {
            self.driver.cancel_all();
        }

        fn budget(&self) -> BudgetSnapshot {
            self.driver.budget()
        }

        fn progress(&self, event: ProgressEvent) {
            self.driver.progress(event);
        }
    }

    #[async_trait::async_trait]
    impl ToolInvoker for OriginCallbacks {
        async fn invoke(&self, request: ToolCallRequest) -> Result<ToolCallResponse, DriverError> {
            assert_eq!(std::thread::current().id(), self.origin);
            self.calls.fetch_add(1, Ordering::SeqCst);
            EchoInvoker.invoke(request).await
        }
    }

    let callbacks = Arc::new(OriginCallbacks {
        origin: std::thread::current().id(),
        calls: AtomicUsize::new(0),
        driver: FakeDriver::new(),
    });
    let value = WorkflowVm::new()
        .run_tools_script(
            r#"const child = await task({ description: "origin-thread" });
               const tool = await tools.call("read", { path: "original" });
               return { child, path: tool.echo.path };"#,
            json!(null),
            callbacks.clone(),
            callbacks.clone(),
            WorkflowRunCancel::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        value,
        json!({"child": "done:origin-thread", "path": "original"})
    );
    assert_eq!(callbacks.calls.load(Ordering::SeqCst), 2);
    assert_eq!(callbacks.driver.spawn_count(), 1);
}

#[tokio::test]
async fn tools_call_refusal_throws_admission_kind() {
    let value = run_tools(
        r#"try { await tools.call("deny", {}); return "no-throw"; } catch (e) { return e.kind; }"#,
    )
    .await
    .unwrap();
    assert_eq!(value, json!("admission"));
}

#[tokio::test]
async fn tools_call_failure_throws_agent_kind() {
    let value = run_tools(
        r#"try { await tools.call("boom", {}); return "no-throw"; } catch (e) { return e.kind; }"#,
    )
    .await
    .unwrap();
    assert_eq!(value, json!("agent"));
}

#[tokio::test]
async fn tools_calls_continue_past_fifty_while_the_host_admits_them() {
    let result = run_tools(
        r#"let completed = 0; for (let i = 0; i < 55; i++) { await tools.call("read", {}); completed++; } return completed;"#,
    )
    .await
    .expect("admitted calls continue until the script completes");
    assert_eq!(result, json!(55));
}

/// A platform-native absolute path for `unix_path` (`/a/b`): unchanged on
/// Unix, `C:\a\b` on Windows, where `/a/b` has no drive and is not absolute.
fn native_abs(unix_path: &str) -> String {
    if cfg!(windows) {
        format!("C:{}", unix_path.replace('/', "\\"))
    } else {
        unix_path.to_string()
    }
}

/// An absolute `cwd` inside the run's workspace normalizes to the
/// repo-relative form; outside it, through `..`, or with no known workspace
/// it is still refused (the trust boundary is unchanged).
#[test]
fn absolute_cwd_inside_the_workspace_normalizes_and_outside_is_refused() {
    use codewhale_workflow_js::{normalize_task_cwd, normalize_task_cwd_in};
    use std::path::PathBuf;
    let workspace = PathBuf::from(native_abs("/Volumes/VIXinSSD/CW"));
    let workspace = Some(workspace.as_path());
    assert_eq!(
        normalize_task_cwd_in(&native_abs("/Volumes/VIXinSSD/CW/codewhale"), workspace).unwrap(),
        "codewhale"
    );
    assert_eq!(
        normalize_task_cwd_in(&native_abs("/Volumes/VIXinSSD/CW/./crates/tui"), workspace).unwrap(),
        "crates/tui"
    );
    assert_eq!(
        normalize_task_cwd_in(&native_abs("/Volumes/VIXinSSD/CW/"), workspace).unwrap(),
        "."
    );
    assert_eq!(
        normalize_task_cwd_in("crates/tui", workspace).unwrap(),
        normalize_task_cwd("crates/tui").unwrap()
    );
    for outside in [
        native_abs("/Volumes/VIXinSSD/CW-other/codewhale"),
        native_abs("/Volumes/VIXinSSD"),
        native_abs("/etc"),
        native_abs("/Volumes/VIXinSSD/CW/../secrets"),
        native_abs("/Volumes/VIXinSSD/CW/codewhale/../../secrets"),
        native_abs("/Volumes/VIXinSSD/X/../CW/codewhale"),
    ] {
        let error = normalize_task_cwd_in(&outside, workspace).unwrap_err();
        assert!(
            error.contains("outside the workspace") || error.contains("parent traversal"),
            "{outside}: {error}"
        );
    }
    let error =
        normalize_task_cwd_in(&native_abs("/Volumes/VIXinSSD/CW/codewhale"), None).unwrap_err();
    assert!(error.contains("bounded repo-relative paths"), "{error}");
}

/// Windows spellings of the same workspace path: a verbatim `\\?\` prefix on
/// either side, forward slashes, and a different case all match; another
/// drive or a sibling directory does not.
#[cfg(windows)]
#[test]
fn absolute_cwd_matches_windows_spellings_of_the_workspace() {
    use codewhale_workflow_js::normalize_task_cwd_in;
    use std::path::Path;
    let plain = Path::new(r"C:\Users\dev\CW");
    let verbatim = Path::new(r"\\?\C:\Users\dev\CW");
    for workspace in [plain, verbatim] {
        for inside in [
            r"C:\Users\dev\CW\codewhale",
            "C:/Users/dev/CW/codewhale",
            r"c:\users\DEV\cw\codewhale",
            r"\\?\C:\Users\dev\CW\codewhale",
        ] {
            assert_eq!(
                normalize_task_cwd_in(inside, Some(workspace)).unwrap(),
                "codewhale",
                "{inside} in {}",
                workspace.display()
            );
        }
        for outside in [
            r"D:\Users\dev\CW\codewhale",
            r"C:\Users\dev\CW-other",
            r"C:\Users\dev\CW\..\secrets",
            r"\\server\share\Users\dev\CW",
        ] {
            let error = normalize_task_cwd_in(outside, Some(workspace)).unwrap_err();
            assert!(
                error.contains("outside the workspace") || error.contains("parent traversal"),
                "{outside}: {error}"
            );
        }
    }
    // A UNC workspace matches its own share, case-insensitively.
    let unc = Path::new(r"\\Server\Share\CW");
    assert_eq!(
        normalize_task_cwd_in(r"\\server\share\cw\codewhale", Some(unc)).unwrap(),
        "codewhale"
    );
}
