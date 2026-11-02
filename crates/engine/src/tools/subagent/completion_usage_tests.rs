use super::*;
use tempfile::tempdir;

fn child(
    manager: &mut SubAgentManager,
    name: &str,
    parent: Option<&str>,
    units: Option<u64>,
) -> String {
    let workspace = manager.workspace.clone();
    let id = manager.insert_test_running_agent(name, &workspace);
    let record = manager.worker_records.get_mut(&id).unwrap();
    record.spec.parent_run_id = parent.map(str::to_string);
    record.parent_run_id = parent.map(str::to_string);
    record.usage.input_tokens = units.map(|units| units * 8);
    record.usage.output_tokens = units.map(|units| units * 2);
    record.usage.total_tokens = units.map(|units| units * 10);
    id
}

fn resume(manager: &mut SubAgentManager, id: &str, source: &str) {
    let record = manager.worker_records.get_mut(id).unwrap();
    record.spec.launch_manifest = Some(
        serde_json::from_value(json!({
            "owner_session": record.spec.parent_run_id.as_deref().unwrap_or("root"), "child_id": id,
            "profile": record.spec.runtime_profile, "prompt": "continue",
            "cwd": null, "worktree": false, "writable_roots": [],
            "writable_files": [], "coordination_contracts": [],
            "resume_from_agent_id": source, "generation": 1
        }))
        .unwrap(),
    );
    // The same edge is present in both persisted representations.
    manager.resume_targets.insert(source.into(), id.into());
}

fn sentinel(completion: &SubAgentCompletion) -> Value {
    let opening = "<codewhale:subagent.done>";
    let start = completion.payload.rfind(opening).unwrap() + opening.len();
    let end = completion
        .payload
        .rfind("</codewhale:subagent.done>")
        .unwrap();
    serde_json::from_str(&completion.payload[start..end]).unwrap()
}

fn terminal_result(manager: &SubAgentManager, id: &str) -> SubAgentResult {
    let mut result = manager.get_result(id).unwrap();
    result.status = SubAgentStatus::Completed;
    result.result = Some("Measured work is complete.".into());
    result
}

fn receipt(manager: &SubAgentManager, id: &str) -> Value {
    sentinel(&manager.completion_from_result_with_ref_for_session(
        "workspace",
        &terminal_result(manager, id),
        None,
    ))
}

fn family(manager: &mut SubAgentManager) -> (String, String, String, String) {
    let root = child(manager, "root", None, Some(1));
    let direct = child(manager, "direct", Some(&root), Some(2));
    let grandchild = child(manager, "grandchild", Some(&direct), Some(3));
    let continued = child(manager, "continued", Some(&direct), Some(4));
    resume(manager, &continued, &grandchild);
    let _sibling = child(manager, "outside", None, Some(90));
    for id in [&direct, &grandchild, &continued] {
        manager.worker_records.get_mut(id).unwrap().status = AgentWorkerStatus::Completed;
        manager.agents.get_mut(id).unwrap().status = SubAgentStatus::Completed;
    }
    (root, direct, grandchild, continued)
}

#[tokio::test]
async fn completion_usage_live_terminal_counts_grandchildren_and_continuations_once() {
    for status in [SubAgentStatus::Completed, SubAgentStatus::BudgetExhausted] {
        let dir = tempdir().unwrap();
        let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 8);
        let (root, _, _, _) = family(&mut manager);
        let (tx, mut rx) = mpsc::channel(16);
        let (event_tx, mut event_rx) = mpsc::channel(8);
        manager.agents.get_mut(&root).unwrap().terminal_delivery =
            Some(SubAgentTerminalDeliveryContext {
                spawn_depth: 1,
                parent_completion_tx: Some(tx),
                mailbox: None,
                event_tx: Some(event_tx),
                event_delivery: None,
                session_id: "workspace".into(),
            });
        let mut result = terminal_result(&manager, &root);
        result.status = status;
        // The locked ledger wins over an earlier result snapshot.
        result.usage.as_mut().unwrap().total_tokens = Some(123_456);
        assert!(manager.finish_terminal_result(&root, result, false, false));
        let completion = rx.try_recv().unwrap();
        let payload = sentinel(&completion);
        assert_eq!(payload["usage"]["own"]["total_tokens"], 10);
        assert_eq!(payload["usage"]["descendants"]["workers"], 3);
        assert_eq!(payload["usage"]["descendants"]["total_tokens"]["known"], 90);
        assert_eq!(payload["usage"]["subtree"]["workers"], 4);
        assert_eq!(payload["usage"]["subtree"]["active_workers"], 0);
        assert_eq!(payload["usage"]["subtree"]["input_tokens"]["known"], 80);
        assert_eq!(payload["usage"]["subtree"]["output_tokens"]["known"], 20);
        assert_eq!(payload["usage"]["subtree"]["total_tokens"]["known"], 100);
        assert_eq!(
            payload["usage"]["subtree"]["total_tokens"]["reported_workers"],
            4
        );
        assert!(payload.get("verification").is_some());
        if manager.get_result(&root).unwrap().status == SubAgentStatus::BudgetExhausted {
            assert_eq!(payload["event"], "subagent.failed");
        }
        let Event::AgentComplete {
            result: event_result,
            ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("expected a terminal UI event");
        };
        assert_eq!(event_result, completion.payload);
        assert!(
            rx.try_recv().is_err(),
            "terminal fan-in remains exactly once"
        );
        assert_eq!(manager.worker_records[&root].usage.total_tokens, Some(10));
        assert!(serde_json::to_vec(&payload["usage"]).unwrap().len() <= 1600);
    }
}

#[tokio::test]
async fn completion_usage_recovery_restores_measured_lineage_without_recounting() {
    let dir = tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let path = base.join(".codewhale/subagents/state.json");
    let mut manager = SubAgentManager::new(base.clone(), 8).with_state_path(path.clone());
    let (root, _, grandchild, continued) = family(&mut manager);
    let result = terminal_result(&manager, &root);
    assert!(manager.finish_terminal_result(&root, result, false, false));
    let before = receipt(&manager, &root);
    manager.persist_state_synchronously().unwrap();
    let mut loaded = SubAgentManager::new(base, 8).with_state_path(path);
    loaded.load_state().unwrap();
    let after = receipt(&loaded, &root);
    assert_eq!(after["usage"], before["usage"]);
    assert_eq!(after["usage"]["subtree"]["total_tokens"]["known"], 100);
    assert_eq!(loaded.continuation_target(&grandchild).unwrap(), continued);
    // A resumed child's receipt covers its own forward subtree, not its
    // predecessor's already-delivered spend or unrelated siblings.
    assert_eq!(
        receipt(&loaded, &continued)["usage"]["subtree"]["total_tokens"]["known"],
        40
    );
}

#[tokio::test]
async fn completion_usage_rejects_foreign_bridges_and_counts_cycles_once() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 8);
    let root = child(&mut manager, "root", None, Some(1));
    let direct = child(&mut manager, "direct", Some(&root), Some(2));
    manager
        .worker_records
        .get_mut(&root)
        .unwrap()
        .spec
        .parent_run_id = Some(direct.clone());
    let foreign = child(&mut manager, "foreign", Some(&root), Some(90));
    manager.assign_test_session_owner(&foreign, "another-owner");
    let bridged = child(&mut manager, "bridged", Some(&foreign), Some(80));
    resume(&mut manager, &bridged, &foreign);
    manager.resume_targets.insert(root.clone(), foreign.clone());
    // A forged manifest cannot create a same-owner edge from foreign authority.
    let manifest = manager
        .worker_records
        .get_mut(&bridged)
        .unwrap()
        .spec
        .launch_manifest
        .as_mut()
        .unwrap();
    manifest.owner_session = "another-owner".into();
    manifest.resume_from_agent_id = Some(root.clone());
    let payload = receipt(&manager, &root);
    assert_eq!(payload["usage"]["subtree"]["workers"], 2);
    assert_eq!(payload["usage"]["subtree"]["total_tokens"]["known"], 30);
    assert_eq!(payload["usage"]["descendants"]["active_workers"], 1);
    let foreign_projection = sentinel(&manager.completion_from_result_with_ref_for_session(
        "another-owner",
        &terminal_result(&manager, &root),
        None,
    ));
    assert_eq!(foreign_projection["usage"]["scope"], "unavailable");
    assert!(foreign_projection["usage"]["own"]["total_tokens"].is_null());
}

#[tokio::test]
async fn completion_usage_distinguishes_unknown_zero_partial_and_overflow() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 4);
    let root = child(&mut manager, "unknown", None, None);
    let direct = child(&mut manager, "zero", Some(&root), Some(0));
    let payload = receipt(&manager, &root);
    assert!(payload["usage"]["own"]["total_tokens"].is_null());
    assert_eq!(payload["usage"]["subtree"]["total_tokens"]["known"], 0);
    assert_eq!(
        payload["usage"]["subtree"]["total_tokens"]["reported_workers"],
        1
    );
    assert_eq!(payload["usage"]["subtree"]["workers"], 2);
    manager
        .worker_records
        .get_mut(&direct)
        .unwrap()
        .usage
        .total_tokens = None;
    let payload = receipt(&manager, &root);
    assert!(payload["usage"]["subtree"]["total_tokens"]["known"].is_null());
    assert_eq!(
        payload["usage"]["subtree"]["total_tokens"]["reported_workers"],
        0
    );
    assert_eq!(
        receipt(&manager, &direct)["usage"]["descendants"]["total_tokens"]["known"],
        0
    );
    manager
        .worker_records
        .get_mut(&root)
        .unwrap()
        .usage
        .total_tokens = Some(u64::MAX);
    manager
        .worker_records
        .get_mut(&direct)
        .unwrap()
        .usage
        .total_tokens = Some(1);
    let payload = receipt(&manager, &root);
    assert!(payload["usage"]["subtree"]["total_tokens"]["known"].is_null());
    assert_eq!(
        payload["usage"]["subtree"]["total_tokens"]["overflow"],
        true
    );
    assert_eq!(
        payload["usage"]["subtree"]["total_tokens"]["reported_workers"],
        2
    );
}

#[tokio::test]
async fn completion_usage_keeps_unknown_then_known_response_subtotals_visible() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 8);
    let root = child(&mut manager, "partial-root", None, None);
    let direct = child(&mut manager, "partial-child", Some(&root), None);
    let zero = child(&mut manager, "known-zero", Some(&direct), None);
    // The parent, manifest and successor map describe the same descendant.
    resume(&mut manager, &zero, &direct);
    let foreign = child(&mut manager, "foreign-unknown", Some(&root), None);
    manager.assign_test_session_owner(&foreign, "another-owner");
    manager.record_worker_usage(&foreign, "foreign-unknown", &Usage::default(), None);

    for (id, input, output) in [(&root, 8, 2), (&direct, 16, 4)] {
        manager.record_worker_usage(id, "unreported-response", &Usage::default(), None);
        let before = receipt(&manager, id);
        assert!(before["usage"]["own"]["total_tokens"].is_null());
        assert_eq!(before["usage"]["own"]["has_unreported_usage"], true);
        manager.record_worker_usage(
            id,
            "reported-response",
            &Usage {
                input_tokens: input,
                output_tokens: output,
                ..Usage::default()
            },
            None,
        );
    }
    manager.record_worker_usage(
        &zero,
        "reported-zero-response",
        &Usage {
            prompt_cache_hit_tokens: Some(0),
            ..Usage::default()
        },
        None,
    );

    let payload = receipt(&manager, &root);
    let usage = &payload["usage"];
    assert_eq!(usage["own"]["input_tokens"], 8);
    assert_eq!(usage["own"]["output_tokens"], 2);
    assert_eq!(usage["own"]["total_tokens"], 10);
    assert_eq!(usage["own"]["has_unreported_usage"], true);
    assert_eq!(usage["descendants"]["workers"], 2);
    assert_eq!(usage["descendants"]["unreported_usage_workers"], 1);
    assert_eq!(usage["descendants"]["total_tokens"]["known"], 20);
    assert_eq!(usage["subtree"]["workers"], 3);
    assert_eq!(usage["subtree"]["unreported_usage_workers"], 2);
    assert_eq!(usage["subtree"]["input_tokens"]["known"], 24);
    assert_eq!(usage["subtree"]["output_tokens"]["known"], 6);
    assert_eq!(usage["subtree"]["total_tokens"]["known"], 30);
    assert_eq!(usage["subtree"]["total_tokens"]["reported_workers"], 3);
    assert!(serde_json::to_vec(usage).unwrap().len() <= 1600);

    let zero_receipt = receipt(&manager, &zero);
    assert_eq!(zero_receipt["usage"]["own"]["total_tokens"], 0);
    assert_eq!(zero_receipt["usage"]["own"]["has_unreported_usage"], false);
    assert_eq!(
        zero_receipt["usage"]["subtree"]["unreported_usage_workers"],
        0
    );
    assert_eq!(
        zero_receipt["usage"]["descendants"]["unreported_usage_workers"],
        0
    );
    assert_eq!(manager.worker_records[&root].usage.total_tokens, Some(10));
    assert_eq!(manager.worker_records[&direct].usage.total_tokens, Some(20));
}

#[tokio::test]
async fn completion_usage_counts_real_manifest_only_root_fork() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 4);
    let mut runtime = tests::stub_runtime()
        .with_max_spawn_depth(3)
        .child_runtime();
    runtime.context = ToolContext::new(dir.path());
    runtime.manager = Arc::clone(&manager);
    // Exercise real registration without allowing any provider request.
    runtime.cancel_token.cancel();
    let mut guard = manager.write().await;
    let (canonical, directory) =
        codewhale_util::workspace_dir::open_workspace_directory(dir.path()).unwrap();
    guard
        .admit_coordination_workspace(dir.path().to_path_buf(), canonical, Arc::new(directory))
        .unwrap();
    let source = child(&mut guard, "source", None, Some(1));
    let completed = terminal_result(&guard, &source);
    assert!(guard.finish_terminal_result(&source, completed, true, false));
    let fork = guard
        .spawn_background_with_assignment_options(
            Arc::clone(&manager),
            runtime,
            FleetRole::Scout,
            "Read the prior work.".into(),
            SubAgentAssignment::new("Read the prior work.".into(), None),
            Some(vec![]),
            SubAgentSpawnOptions {
                resume_from_agent_id: Some(source.clone()),
                checkpoint_continuation: false,
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let record = guard.worker_records.get_mut(&fork.agent_id).unwrap();
    assert_eq!(record.owner_session_id, "workspace");
    assert!(record.spec.parent_run_id.is_none());
    let manifest = record.spec.launch_manifest.as_ref().unwrap();
    assert_eq!(manifest.owner_session, "root");
    assert_eq!(
        manifest.resume_from_agent_id.as_deref(),
        Some(source.as_str())
    );
    record.usage.input_tokens = Some(16);
    record.usage.output_tokens = Some(4);
    record.usage.total_tokens = Some(20);
    assert!(!guard.resume_targets.contains_key(&source));
    let payload = receipt(&guard, &source);
    assert_eq!(payload["usage"]["descendants"]["workers"], 1);
    assert_eq!(payload["usage"]["descendants"]["total_tokens"]["known"], 20);
    assert_eq!(payload["usage"]["subtree"]["total_tokens"]["known"], 30);
}

#[tokio::test]
#[expect(
    clippy::print_stderr,
    reason = "libtest-only measurement; never the TUI"
)]
async fn completion_usage_dozen_child_status_measures_bytes_and_keeps_descendant_rows() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 16);
    let mut ids: Vec<String> = Vec::new();
    {
        let mut guard = manager.write().await;
        for (index, name) in [
            "a2f4095d", "846bd172", "ce819730", "918deb4a", "716a54bf", "f291ac63", "b5701e92",
            "294e7cab", "ac967f81", "670d2ea9", "edf94328", "547ab013",
        ]
        .into_iter()
        .enumerate()
        {
            let parent = (index > 0).then(|| ids[(index - 1) / 2].clone());
            let depth = parent.as_ref().map_or(1, |parent| {
                guard.worker_records[parent].spec.spawn_depth + 1
            });
            let id = child(&mut guard, name, parent.as_deref(), Some(index as u64 + 1));
            let record = guard.worker_records.get_mut(&id).unwrap();
            record.spec.spawn_depth = depth;
            record.spec.max_spawn_depth = 4;
            record.spec.runtime_profile.spawn_depth = depth;
            record.spec.runtime_profile.max_spawn_depth = 4;
            record.latest_message = Some("starting".into());
            record.spec.child_route = Some(ChildRouteReceipt {
                requested_type: "explore".into(),
                requested_profile: Some("scout".into()),
                resolved_profile_id: Some("scout".into()),
                profile_origin: Some("workspace".into()),
                canonical_role: "explore".into(),
                provider_id: "deepseek".into(),
                model_id: "deepseek-v4-flash".into(),
                route_source: "profile.model".into(),
                fallback_note: None,
                requested_reasoning: "inherit".into(),
                effective_reasoning: Some("medium".into()),
                runtime_version: "0.9.13".into(),
                runtime_build_sha: "a".repeat(40),
            });
            record.spec.runtime_profile.max_steps = 12;
            record.spec.runtime_profile.wall_time_secs = Some(600);
            record.spec.runtime_profile.wall_deadline_ms = Some(record.updated_at_ms + 600_000);
            if index == 11 {
                record.verification.status = "deliverable_missing".into();
                record.verification.summary = "The claimed report.md is missing.".into();
                record.verification.deliverables = vec![DeliverableVerdict {
                    path: "report.md".into(),
                    status: "missing".into(),
                    bytes: None,
                }];
            }
            ids.push(id);
        }
    }
    let mut offset = 0;
    let mut bytes = 0;
    let mut pages = 0;
    let mut seen = HashSet::new();
    loop {
        let output = inspect_agent_from_input(
            &json!({"action":"status", "offset":offset}),
            Arc::clone(&manager),
            &ToolContext::new(dir.path()),
            false,
            None,
        )
        .await
        .unwrap();
        bytes += output.content.len();
        pages += 1;
        eprintln!("DOZEN_CHILD_STATUS_JSON {}", output.content);
        assert!(output.content.len() <= lifecycle::COMPACT_STATUS_BYTES);
        let payload: Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(payload["usage"]["total_tokens"], 780);
        for row in lifecycle_tests::status_rows(&payload) {
            let id = row["agent_id"].as_str().unwrap();
            assert!(seen.insert(id.to_string()));
            let index = ids.iter().position(|expected| expected == id).unwrap();
            assert_eq!(row["total_tokens"], (index + 1) * 10);
            for key in [
                "compact",
                "terminal",
                "name",
                "steps_taken",
                "child_route",
                "effective_limits",
                "usage",
                "max_spawn_depth",
            ] {
                assert!(row.get(key).is_none(), "{key}: {row}");
            }
            assert_eq!(row["needs_continuation"], false, "{row}");
            assert_eq!(row["activity"], "starting");
            for key in ["duration_ms", "last_activity_ms", "spawn_depth"] {
                assert!(row[key].is_u64(), "{key}: {row}");
            }
            if index == 11 {
                assert_eq!(row["verification"]["status"], "deliverable_missing");
                assert_eq!(row["verification"]["deliverable_counts"]["missing"], 1);
                assert_eq!(
                    row["verification"]["summary"],
                    "The claimed report.md is missing."
                );
            } else {
                assert_eq!(row["verification"], json!({"status": "self_report_only"}));
            }
            if index > 0 {
                assert_eq!(row["parent_agent_id"], ids[(index - 1) / 2]);
            }
        }
        let Some(next) = payload["next_offset"].as_u64() else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(seen.len(), 12);
    assert_eq!(pages, 1, "ordinary twelve-worker roster must fit one page");
    assert!(bytes <= 3072, "twelve-worker roster used {bytes} bytes");
    let addressed = inspect_agent_from_input(
        &json!({"action":"status", "agent_id":ids[0]}),
        Arc::clone(&manager),
        &ToolContext::new(dir.path()),
        false,
        None,
    )
    .await
    .unwrap();
    let addressed: Value = serde_json::from_str(&addressed.content).unwrap();
    assert_eq!(addressed["compact"], true);
    assert_eq!(addressed["child_route"]["model_id"], "deepseek-v4-flash");
    assert_eq!(addressed["effective_limits"]["max_steps"], 12);
    assert_eq!(addressed["effective_limits"]["wall_time_secs"], 600);
    assert_eq!(addressed["max_spawn_depth"], 4);
    assert_eq!(addressed["usage"]["input_tokens"], 8);
    assert_eq!(addressed["usage"]["output_tokens"], 2);
    eprintln!(
        "DOZEN_CHILD_STATUS_MEASUREMENT children=12 pages={pages} serialized_bytes={bytes}; token_count=unmeasured"
    );
}

#[test]
fn worker_unknown_usage_promotes_exact_route_once_and_survives_serialization() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 4);
    let id = child(&mut manager, "late-exact", None, None);
    let route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        codewhale_config::ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some("https://api.deepseek.com/v1"),
        chrono::Utc::now(),
    );
    manager.record_worker_missing_usage(
        &id,
        "late-response",
        crate::cost_status::MissingUsageCoverage::for_route(
            &route,
            crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
        ),
    );
    let encoded = serde_json::to_vec(manager.worker_records.get(&id).unwrap()).unwrap();
    let restored: AgentWorkerRecord = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored.missing_usage_sources.len(), 1);
    assert!(restored.has_unreported_usage);
    manager.worker_records.insert(id.clone(), restored);
    let usage = Usage {
        input_tokens: 17,
        output_tokens: 3,
        ..Default::default()
    };
    let mut changed = route.clone();
    changed.model = "different-model".into();
    manager.record_worker_routed_usage(&id, "late-response", &changed, &usage, Some(20));
    assert!(manager.worker_records[&id].usage.total_tokens.is_none());
    for _ in 0..2 {
        manager.record_worker_routed_usage(&id, "late-response", &route, &usage, Some(20));
    }
    let record = &manager.worker_records[&id];
    assert_eq!(record.usage.total_tokens, Some(20));
    assert_eq!(record.usage.cost_microusd, Some(20));
    assert!(record.missing_usage_sources.is_empty());
    assert!(!record.has_unreported_usage);
    assert_eq!(record.usage_source_fingerprints.len(), 1);
}

#[test]
fn worker_missing_overflow_and_legacy_gap_remain_after_exact_promotions() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 4);
    let id = child(&mut manager, "bounded-unknown", None, None);
    manager.mark_worker_unreported_usage(&id);
    let route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        codewhale_config::ProviderKind::Deepseek,
        "deepseek",
        "deepseek-v4-flash",
        Some("https://api.deepseek.com/v1"),
        chrono::Utc::now(),
    );
    for index in 0..65 {
        manager.record_worker_missing_usage(
            &id,
            &format!("request-{index}"),
            crate::cost_status::MissingUsageCoverage::for_route(
                &route,
                crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
            ),
        );
    }
    assert_eq!(manager.worker_records[&id].missing_usage_sources.len(), 64);
    let encoded = serde_json::to_vec(&manager.worker_records[&id]).unwrap();
    let restored: AgentWorkerRecord = serde_json::from_slice(&encoded).unwrap();
    assert!(restored.missing_usage_overflowed);
    assert!(restored.legacy_unreported_usage);
    manager.worker_records.insert(id.clone(), restored);
    let usage = Usage {
        input_tokens: 1,
        output_tokens: 1,
        ..Default::default()
    };
    for index in 0..65 {
        for _ in 0..2 {
            manager.record_worker_routed_usage(
                &id,
                &format!("request-{index}"),
                &route,
                &usage,
                None,
            );
        }
    }
    let record = &manager.worker_records[&id];
    assert_eq!(record.usage.total_tokens, Some(130));
    assert!(record.missing_usage_sources.is_empty());
    assert!(record.has_unreported_usage);
    assert!(record.missing_usage_overflowed && record.legacy_unreported_usage);
}
