use super::*;
use tempfile::tempdir;

pub(super) fn status_rows(payload: &Value) -> Vec<Value> {
    let columns = payload["columns"].as_array().expect("roster columns");
    let unique = columns
        .iter()
        .map(|column| column.as_str().expect("column name"))
        .collect::<HashSet<_>>();
    assert_eq!(columns.len(), unique.len(), "duplicate roster column");
    payload["agents"]
        .as_array()
        .expect("roster rows")
        .iter()
        .map(|row| {
            let values = row.as_array().expect("roster row array");
            assert_eq!(columns.len(), values.len(), "row must match its header");
            Value::Object(
                columns
                    .iter()
                    .zip(values)
                    .map(|(column, value)| (column.as_str().unwrap().to_string(), value.clone()))
                    .collect(),
            )
        })
        .collect()
}

fn prior_messages() -> Vec<Message> {
    vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "retained work".into(),
            cache_control: None,
        }],
    }]
}

#[tokio::test]
async fn lifecycle_bulk_followup_preserves_mappings_and_retries_without_duplicate_workers() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 12);
    let mut sources = Vec::new();
    {
        let mut guard = manager.write().await;
        for i in 0..6 {
            let (id, _) = guard.insert_test_interrupted_continuable_agent(
                &format!("parked-{i}"),
                dir.path(),
                prior_messages(),
            );
            let agent = guard.agents.get_mut(&id).unwrap();
            agent.checkpoint.as_mut().unwrap().parked_at_turn_end = true;
            agent.agent_type = FleetRole::Scout;
            agent.model = "deepseek-v4-flash".into();
            agent.allowed_tools = Some(Vec::new());
            let record = guard.worker_records.get_mut(&id).unwrap();
            record.parent_run_id = None;
            record.spec.parent_run_id = None;
            let spec = &mut record.spec;
            spec.model = "deepseek-v4-flash".into();
            spec.agent_type = FleetRole::Scout;
            spec.runtime_profile = WorkerRuntimeProfile::for_role(FleetRole::Scout);
            sources.push(id);
        }
    }
    let (client, _, _, fixture_config) =
        super::tests::delayed_chat_client(Duration::from_secs(30), "fixture result").await;
    let mut runtime = super::tests::stub_runtime();
    runtime.manager = Arc::clone(&manager);
    runtime.client = client;
    runtime.api_config = Some(Arc::new(fixture_config));
    runtime.context = ToolContext::new(dir.path());
    let tool = coord::AgentsFollowupTool::new(Arc::clone(&manager)).with_runtime(runtime);
    let input = json!({"agent_ids": sources, "message": "Continue the assignment."});
    let first = tool
        .execute(input.clone(), &ToolContext::new(dir.path()))
        .await
        .unwrap();
    let first: Value = serde_json::from_str(&first.content).unwrap();
    assert_eq!(first["results"].as_array().unwrap().len(), 6);
    assert_eq!(first["errors"], json!([]));
    let second = tool
        .execute(input, &ToolContext::new(dir.path()))
        .await
        .unwrap();
    let second: Value = serde_json::from_str(&second.content).unwrap();
    for (a, b) in first["results"]
        .as_array()
        .unwrap()
        .iter()
        .zip(second["results"].as_array().unwrap())
    {
        assert_eq!(a["from"], b["from"]);
        assert_eq!(a["to"], b["to"]);
        assert_ne!(a["from"], a["to"]);
    }
    let mut guard = manager.write().await;
    assert_eq!(guard.agents.len(), 12);
    for id in sources {
        let target = guard.continuation_target(&id).unwrap();
        assert_ne!(target, id);
        assert_eq!(
            guard.continuation_source(&target).as_deref(),
            Some(id.as_str())
        );
        let _ = guard.cancel_agent(&target);
    }
}

#[tokio::test]
async fn lifecycle_bulk_followup_reports_unknown_and_foreign_targets_without_hiding_success() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 4);
    let (owned, foreign) = {
        let mut guard = manager.write().await;
        let owned = guard.insert_test_running_agent("owned", dir.path());
        let foreign = guard.insert_test_running_agent("foreign", dir.path());
        guard.assign_test_session_owner(&foreign, "another-session");
        (owned, foreign)
    };
    let tool = coord::AgentsFollowupTool::new(Arc::clone(&manager));
    let result = tool
        .execute(
            json!({"agent_ids": [owned, "missing", foreign], "message": "Check progress"}),
            &ToolContext::new(dir.path()),
        )
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(payload["results"].as_array().unwrap().len(), 1);
    assert_eq!(payload["errors"].as_array().unwrap().len(), 2);
    assert!(!manager.read().await.child_was_woken(&foreign));
}

#[tokio::test]
async fn lifecycle_followup_rejects_ambiguous_and_invalid_batch_inputs_before_delivery() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 2);
    let id = manager
        .write()
        .await
        .insert_test_running_agent("owned", dir.path());
    let tool = coord::AgentsFollowupTool::new(Arc::clone(&manager));
    for input in [
        json!({"agent_id": id, "agent_ids": [id], "message": "x"}),
        json!({"agent_ids": [], "message": "x"}),
        json!({"agent_ids": [id, 7], "message": "x"}),
        json!({"all_parked": "true", "message": "x"}),
        json!({"agent_id": id, "message": "  "}),
    ] {
        assert!(
            tool.execute(input, &ToolContext::new(dir.path()))
                .await
                .is_err()
        );
    }
    assert!(!manager.read().await.child_was_woken(&id));
}

#[tokio::test]
async fn lifecycle_resume_lineage_survives_persist_and_rejects_cycles_and_foreign_hops() {
    let dir = tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let state_path = base.join(".codewhale/subagents/state.json");
    let mut manager = SubAgentManager::new(base.clone(), 6).with_state_path(state_path.clone());
    let (a, _) = manager.insert_test_interrupted_continuable_agent("old", &base, prior_messages());
    let (b, _) =
        manager.insert_test_interrupted_continuable_agent("continued", &base, prior_messages());
    let (c, _) =
        manager.insert_test_interrupted_continuable_agent("latest", &base, prior_messages());
    manager.resume_targets.insert(a.clone(), b.clone());
    manager.resume_targets.insert(b.clone(), c.clone());
    manager.persist_state().unwrap().join().unwrap();
    let mut loaded = SubAgentManager::new(base.clone(), 6).with_state_path(state_path);
    loaded.load_state().unwrap();
    assert_eq!(loaded.continuation_target(&a).unwrap(), c);
    loaded.resume_targets.insert(c.clone(), a.clone());
    assert!(
        loaded
            .continuation_target(&a)
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
    loaded.resume_targets.remove(&c);
    loaded.assign_test_session_owner(&c, "foreign");
    assert!(
        loaded
            .continuation_target(&a)
            .unwrap_err()
            .to_string()
            .contains("outside")
    );
}

#[tokio::test]
async fn lifecycle_compact_roster_bounds_every_state_and_pages_multibyte_names() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 50);
    let mut expected = HashSet::new();
    {
        let mut guard = manager.write().await;
        for i in 0..37 {
            let id = guard.insert_test_running_agent(&format!("bounded-{i}"), dir.path());
            expected.insert(id.clone());
            let agent = guard.agents.get_mut(&id).unwrap();
            agent.session_name = "🐋\"".repeat(4000);
            agent.prompt = "archive-only".repeat(10_000);
            agent.checkpoint = Some(build_subagent_checkpoint(
                &id,
                "resume",
                &prior_messages(),
                1,
                true,
            ));
            if i % 3 == 1 {
                agent.status = SubAgentStatus::Interrupted("reason".repeat(10_000));
            }
            if i % 3 == 2 {
                agent.status = SubAgentStatus::Failed("failure".repeat(10_000));
            }
            let record = guard.worker_records.get_mut(&id).unwrap();
            record.usage.total_tokens = Some(100);
            record.verification.summary = "\"🐋".repeat(10_000);
            if i > 0 {
                record.parent_run_id = Some("agent_bounded-0".into());
            }
        }
    }
    let mut offset = 0;
    let mut seen = HashSet::new();
    let mut header = None;
    loop {
        let result = inspect_agent_from_input(
            &json!({"action": "status", "verbose": true, "offset": offset}),
            Arc::clone(&manager),
            &ToolContext::new(dir.path()),
            false,
            None,
        )
        .await
        .unwrap();
        assert!(
            result.content.len() <= lifecycle::COMPACT_STATUS_BYTES,
            "{}",
            result.content.len()
        );
        let value: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(
            *header.get_or_insert_with(|| value["columns"].clone()),
            value["columns"]
        );
        assert_eq!(value["total_count"], 37);
        assert_eq!(
            value["usage"]["total_tokens"], 3700,
            "scope totals must not be counted per child"
        );
        let rows = status_rows(&value);
        assert!(!rows.is_empty());
        for row in rows {
            assert!(seen.insert(row["agent_id"].as_str().unwrap().to_string()));
            for key in [
                "snapshot",
                "worker_record",
                "checkpoint",
                "transcript_handle",
            ] {
                assert!(row.get(key).is_none(), "{key}");
            }
        }
        let Some(next) = value["next_offset"].as_u64() else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn lifecycle_compact_columns_keep_unknown_usage_and_pending_input_explicit() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 3);
    let zero = manager.insert_test_running_agent("zero", dir.path());
    manager
        .worker_records
        .get_mut(&zero)
        .unwrap()
        .usage
        .total_tokens = Some(0);
    let (pending, _) =
        manager.insert_test_interrupted_continuable_agent("pending", dir.path(), prior_messages());
    manager.agents.get_mut(&pending).unwrap().needs_input = Some(SubAgentNeedsInput {
        question: "Approve the next validation step?".into(),
    });
    let payload =
        lifecycle::compact_roster(&manager, &json!({}), "workspace", false, false).unwrap();
    let rows = status_rows(&payload);
    let zero = rows.iter().find(|row| row["agent_id"] == zero).unwrap();
    let pending = rows.iter().find(|row| row["agent_id"] == pending).unwrap();
    assert_eq!(zero["total_tokens"], 0);
    assert!(zero["needs_input"].is_null());
    assert!(pending["total_tokens"].is_null());
    assert_eq!(pending["needs_input"], "Approve the next validation step?");
    assert_eq!(pending["needs_continuation"], true);
    assert_eq!(payload["usage"]["total_tokens"], 0);
    assert_eq!(payload["usage"]["reported_workers"], 1);
    let empty = SubAgentManager::new(dir.path().join("empty"), 1);
    let empty = lifecycle::compact_roster(&empty, &json!({}), "workspace", false, false).unwrap();
    assert_eq!(empty["columns"], payload["columns"]);
    assert!(status_rows(&empty).is_empty());
}

#[tokio::test]
async fn lifecycle_addressed_status_follows_lineage_and_detail_is_bounded() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 3);
    let (old, latest) = {
        let mut guard = manager.write().await;
        guard
            .with_store_transaction(|guard| {
                let (old, _) = guard.insert_test_interrupted_continuable_agent(
                    "old",
                    dir.path(),
                    prior_messages(),
                );
                let latest = guard.insert_test_running_agent("latest", dir.path());
                guard.resume_targets.insert(old.clone(), latest.clone());
                guard.agents.get_mut(&latest).unwrap().result = Some("🐋".repeat(100_000));
                guard.persist_state_synchronously()?;
                Ok((old, latest))
            })
            .unwrap()
    };
    let context = ToolContext::new(dir.path());
    let roster = inspect_agent_from_input(
        &json!({"action": "status"}),
        Arc::clone(&manager),
        &context,
        false,
        None,
    )
    .await
    .unwrap();
    let roster: Value = serde_json::from_str(&roster.content).unwrap();
    let rows = status_rows(&roster);
    assert_eq!(
        rows.iter().find(|row| row["agent_id"] == old).unwrap()["resumed_as"],
        latest
    );
    assert_eq!(
        rows.iter().find(|row| row["agent_id"] == latest).unwrap()["resumed_from"],
        old
    );
    let result = inspect_agent_from_input(
        &json!({"agent_id": old}),
        Arc::clone(&manager),
        &context,
        false,
        None,
    )
    .await
    .unwrap();
    let row: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(row["agent_id"], latest);
    assert_eq!(row["addressed_agent_id"], old);
    assert_eq!(row["resumed_from"], old);
    assert!(row.get("snapshot").is_none());
    let detail = inspect_agent_from_input(
        &json!({"agent_id": old, "detail": true}),
        manager,
        &context,
        false,
        None,
    )
    .await
    .unwrap();
    assert!(detail.content.len() <= 32 * 1024);
    let row: Value = serde_json::from_str(&detail.content).unwrap();
    assert_eq!(row["detail_bounded"], true);
    assert!(row["transcript_handle"].is_object());
}

#[tokio::test]
async fn lifecycle_named_cancel_stops_grandchildren_and_preserves_sibling() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 4);
    let parent = manager.insert_test_running_agent("parent", dir.path());
    let child = manager.insert_test_running_agent("child", dir.path());
    let sibling = manager.insert_test_running_agent("sibling", dir.path());
    let record = manager.worker_records.get_mut(&child).unwrap();
    record.parent_run_id = Some(parent.clone());
    record.spec.parent_run_id = Some(parent.clone());
    manager
        .cancel_agent_for_session("workspace", &parent)
        .unwrap();
    assert_eq!(
        manager.get_result(&child).unwrap().status,
        SubAgentStatus::Cancelled
    );
    assert_eq!(
        manager.get_result(&parent).unwrap().status,
        SubAgentStatus::Cancelled
    );
    assert_eq!(
        manager.get_result(&sibling).unwrap().status,
        SubAgentStatus::Running
    );
}

#[test]
fn lifecycle_recovery_never_forks_and_byte_preview_preserves_utf8() {
    let instruction = subagent_followup_recovery("agent_parked");
    assert!(instruction.contains("action=\"followup\""));
    assert!(!instruction.contains("resume_from"));
    let preview = lifecycle::text_preview(&"🐋".repeat(10_000), 65);
    assert!(preview.len() <= 65);
    assert!(preview.ends_with("..."));
}

#[test]
fn lifecycle_park_event_sentinel_and_tool_share_recovery_instruction() {
    let agent_id = "agent_parked";
    let instruction = subagent_followup_recovery(agent_id);
    let parking = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (status, output, checkpoint, needs_input, _, _) =
        subagent_cancellation_projection(agent_id, &prior_messages(), 1, None, Some(&parking));
    assert!(output.as_deref().unwrap().contains(&instruction));
    assert_eq!(needs_input.as_ref().unwrap().question, instruction);

    let mut result = super::tests::make_snapshot(status);
    result.agent_id = agent_id.into();
    result.result = output;
    result.checkpoint = checkpoint;
    result.needs_input = needs_input;
    let sentinel = subagent_done_sentinel(agent_id, &result, false);
    let metadata: Value = serde_json::from_str(
        sentinel
            .strip_prefix("<codewhale:subagent.done>")
            .unwrap()
            .strip_suffix("</codewhale:subagent.done>")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["needs_input"]["question"], instruction);
    assert!(AGENT_TOOL_DESCRIPTION.contains(&subagent_followup_recovery("<agent_id>")));
}

#[tokio::test]
async fn lifecycle_followup_rechecks_actual_successor_authority() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 4);
    let (caller, old, sibling) = {
        let mut guard = manager.write().await;
        let caller = guard.insert_test_running_agent("caller", dir.path());
        let (old, _) = guard.insert_test_interrupted_continuable_agent(
            "own-child",
            dir.path(),
            prior_messages(),
        );
        let sibling = guard.insert_test_running_agent("sibling", dir.path());
        let record = guard.worker_records.get_mut(&old).unwrap();
        record.parent_run_id = Some(caller.clone());
        record.spec.parent_run_id = Some(caller.clone());
        guard.resume_targets.insert(old.clone(), sibling.clone());
        (caller, old, sibling)
    };
    let tool =
        coord::AgentsFollowupTool::new(Arc::clone(&manager)).with_optional_caller(Some(caller));
    assert!(
        tool.execute(
            json!({"agent_id": old, "message": "Try to wake sibling"}),
            &ToolContext::new(dir.path())
        )
        .await
        .is_err()
    );
    assert!(!manager.read().await.child_was_woken(&sibling));
}

#[tokio::test]
async fn lifecycle_deliverable_preview_reports_omissions_and_detail_pages_the_full_list() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 2);
    let id = manager.insert_test_running_agent("outputs", dir.path());
    manager
        .worker_records
        .get_mut(&id)
        .unwrap()
        .verification
        .deliverables = (0..9)
        .map(|index| DeliverableVerdict {
            path: format!("report-{index}.md"),
            status: if index == 8 {
                "missing".into()
            } else {
                "present".into()
            },
            bytes: (index != 8).then_some(20),
        })
        .collect();
    let compact = lifecycle::compact_row(&manager, &manager.agents[&id]);
    assert_eq!(compact["verification"]["deliverables_total"], 9);
    assert_eq!(compact["verification"]["deliverables_omitted"], 5);
    assert_eq!(compact["verification"]["deliverable_counts"]["missing"], 1);
    assert_eq!(
        compact["verification"]["deliverables"][0]["status"],
        "missing"
    );
    let detail = lifecycle::bounded_detail(
        json!({"verification": manager.worker_records[&id].verification}),
        compact,
        4,
        2,
    );
    assert_eq!(
        detail["verification"]["deliverables"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        detail["verification"]["deliverables"][0]["path"],
        "report-4.md"
    );
    assert_eq!(detail["verification"]["deliverables_next_offset"], 6);
}

#[test]
fn lifecycle_oversized_event_pages_return_every_sequence_once() {
    let expected: Vec<u64> = (1..=25).collect();
    let events: Vec<Value> = expected
        .iter()
        .map(|seq| {
            json!({
                "seq": seq,
                "status": "running_tool",
                "timestamp_ms": 1_700_000_000_000_u64,
                "message": "\u{0001}".repeat(160),
                "tool_name": "x".repeat(64),
            })
        })
        .collect();
    let projection = json!({"worker_record": {"events": events}});
    for (path_bytes, first_page_len) in [(0, 4), (6000, 1), (10000, 1)] {
        let compact = json!({
            "agent_id": "agent-test",
            "write_progress": {"declared": ["x".repeat(path_bytes)]},
        });
        let mut seen = Vec::new();
        loop {
            let offset = seen.len();
            let detail = lifecycle::bounded_detail(projection.clone(), compact.clone(), offset, 20);
            let result = lifecycle::status_result(detail, false).unwrap();
            assert!(result.content.len() <= 8192);
            let page: Value = serde_json::from_str(&result.content).unwrap();
            let rows = page["recent_events"].as_array().unwrap();
            assert!(!rows.is_empty());
            if offset == 0 {
                assert_eq!(rows.len(), first_page_len);
            }
            assert_eq!(page["events_total"], expected.len());
            seen.extend(rows.iter().map(|row| row["seq"].as_u64().unwrap()));
            let Some(next) = page["events_next_offset"].as_u64() else {
                break;
            };
            assert_eq!(next as usize, seen.len());
            assert!(seen.len() < expected.len());
        }
        assert_eq!(seen, expected);
    }

    let last_page = |bytes| {
        lifecycle::bounded_detail(
            projection.clone(),
            json!({"agent_id":"agent-test","write_progress":{"declared":["x".repeat(bytes)]}}),
            expected.len() - 1,
            20,
        )
    };
    let available = 8192 - serde_json::to_vec(&last_page(0)).unwrap().len();
    let result = lifecycle::status_result(last_page(available), false).unwrap();
    assert!(result.content.len() <= 8192);
    let page: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(page["recent_events"][0]["seq"], 25);
    assert!(page.get("events_next_offset").is_none());
}

#[tokio::test]
async fn lifecycle_running_row_reports_declared_vs_observed_writes() {
    // #6194 item 5: the parent sees the declared/observed write diff while
    // the child is still alive, not only in the post-mortem receipt.
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 2);
    let id = manager.insert_test_running_agent("writer", dir.path());
    {
        let record = manager.worker_records.get_mut(&id).unwrap();
        record.spec.runtime_profile.permissions.write = true;
        record.spec.launch_manifest = Some(ChildLaunchManifest {
            activation: None,
            owner_session: "root".to_string(),
            child_id: id.clone(),
            profile: record.spec.runtime_profile.clone(),
            prompt: record.spec.objective.clone(),
            cwd: None,
            worktree: false,
            writable_roots: Vec::new(),
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
            expected_artifact: None,
            deliverables: vec!["a.rs".to_string(), "b.rs".to_string()],
            resume_identity: None,
            generation: 1,
            resume_from_agent_id: None,
        });
        record
            .delivery_evidence
            .observed_writes
            .insert("b.rs".to_string());
        record
            .delivery_evidence
            .observed_writes
            .insert("surprise.rs".to_string());
    }
    let row = lifecycle::compact_row(&manager, &manager.agents[&id]);
    assert_eq!(row["write_progress"]["declared_total"], 2);
    assert_eq!(row["write_progress"]["observed_total"], 2);
    assert_eq!(row["write_progress"]["declared"][0], "a.rs");
    assert_eq!(row["write_progress"]["observed"][1], "surprise.rs");
}

#[tokio::test]
async fn lifecycle_read_only_row_omits_write_progress() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 2);
    let id = manager.insert_test_running_agent("scout", dir.path());
    let row = lifecycle::compact_row(&manager, &manager.agents[&id]);
    assert!(row.get("write_progress").is_none());
}

#[tokio::test]
async fn lifecycle_continuation_link_is_durable_before_the_child_can_run() {
    let dir = tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let path = base.join(".codewhale/subagents/state.json");
    let manager = Arc::new(RwLock::new(
        SubAgentManager::new(base.clone(), 4).with_state_path(path.clone()),
    ));
    let (client, calls, _, fixture_config) =
        super::tests::delayed_chat_client(Duration::from_secs(30), "fixture").await;
    let mut runtime = super::tests::stub_runtime();
    runtime.client = client;
    runtime.api_config = Some(Arc::new(fixture_config));
    runtime.manager = Arc::clone(&manager);
    runtime.context = ToolContext::new(&base);
    let mut guard = manager.write().await;
    let (source, _) =
        guard.insert_test_interrupted_continuable_agent("durable-source", &base, prior_messages());
    guard.agents.get_mut(&source).unwrap().model = "deepseek-v4-flash".into();
    let record = guard.worker_records.get_mut(&source).unwrap();
    record.parent_run_id = None;
    record.spec.parent_run_id = None;
    guard
        .worker_records
        .get_mut(&source)
        .unwrap()
        .spec
        .runtime_profile = WorkerRuntimeProfile::for_role(FleetRole::Scout);
    assert!(
        !guard.worker_records[&source]
            .spec
            .runtime_profile
            .permissions
            .write
    );
    let successor = guard
        .resume_from_checkpoint(Arc::clone(&manager), runtime, &source, "Continue")
        .unwrap();
    // Holding the manager lock keeps run_subagent_task_inner at its first
    // await. This is the earliest published snapshot, before any child step.
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let persisted: DurableSubAgentStore =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let persisted = persisted.state;
    assert_eq!(
        persisted.resume_targets.get(&source),
        Some(&successor.agent_id)
    );
    assert!(
        persisted
            .agents
            .iter()
            .any(|agent| agent.id == successor.agent_id)
    );
    let _ = guard.cancel_agent(&successor.agent_id);
}

#[tokio::test]
async fn lifecycle_continuation_persist_failure_rolls_back_worker_and_link() {
    let dir = tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let path = base.join(".codewhale/subagents/state.json");
    let manager = Arc::new(RwLock::new(
        SubAgentManager::new(base.clone(), 4).with_state_path(path.clone()),
    ));
    let mut runtime = super::tests::stub_runtime();
    runtime.manager = Arc::clone(&manager);
    runtime.context = ToolContext::new(&base);
    let mut guard = manager.write().await;
    let (source, _) =
        guard.insert_test_interrupted_continuable_agent("failed-source", &base, prior_messages());
    guard.agents.get_mut(&source).unwrap().model = "deepseek-v4-flash".into();
    guard
        .worker_records
        .get_mut(&source)
        .unwrap()
        .spec
        .runtime_profile = WorkerRuntimeProfile::for_role(FleetRole::Scout);
    assert!(
        !guard.worker_records[&source]
            .spec
            .runtime_profile
            .permissions
            .write
    );
    guard.persist_state_synchronously().unwrap();
    let retained_path = path.with_extension("retained-before-failure");
    let result = guard.with_store_transaction(|held| {
        std::fs::rename(&path, &retained_path)?;
        std::fs::create_dir(&path)?;
        let resumed =
            held.resume_from_checkpoint(Arc::clone(&manager), runtime, &source, "Continue");
        std::fs::remove_dir(&path)?;
        std::fs::rename(&retained_path, &path)?;
        resumed
    });
    assert!(result.is_err());
    assert_eq!(guard.agents.len(), 1);
    assert_eq!(guard.worker_records.len(), 1);
    assert!(guard.resume_targets.is_empty());
    assert!(matches!(
        guard.get_result(&source).unwrap().status,
        SubAgentStatus::Interrupted(_)
    ));
}

#[tokio::test]
async fn lifecycle_cancel_original_stops_its_continuation_and_existing_descendants() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 5);
    let (source, _) =
        manager.insert_test_interrupted_continuable_agent("original", dir.path(), prior_messages());
    let current = manager.insert_test_running_agent("current", dir.path());
    let child = manager.insert_test_running_agent("existing-child", dir.path());
    let sibling = manager.insert_test_running_agent("separate-fork", dir.path());
    manager
        .resume_targets
        .insert(source.clone(), current.clone());
    let record = manager.worker_records.get_mut(&child).unwrap();
    record.parent_run_id = Some(source.clone());
    record.spec.parent_run_id = Some(source.clone());

    let cancelled = manager
        .cancel_agent_for_session("workspace", &source)
        .unwrap();
    assert_eq!(cancelled.agent_id, current);
    assert_eq!(cancelled.status, SubAgentStatus::Cancelled);
    assert_eq!(
        manager.get_result(&child).unwrap().status,
        SubAgentStatus::Cancelled
    );
    assert_eq!(
        manager.get_result(&sibling).unwrap().status,
        SubAgentStatus::Running
    );
    assert!(matches!(
        manager.get_result(&source).unwrap().status,
        SubAgentStatus::Interrupted(_)
    ));
}

#[tokio::test]
async fn lifecycle_resumed_parent_controls_existing_descendants_but_never_itself_or_siblings() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 5);
    let (source, _) =
        manager.insert_test_interrupted_continuable_agent("original", dir.path(), prior_messages());
    let current = manager.insert_test_running_agent("current", dir.path());
    let child = manager.insert_test_running_agent("existing-child", dir.path());
    let sibling = manager.insert_test_running_agent("sibling", dir.path());
    manager
        .resume_targets
        .insert(source.clone(), current.clone());
    let record = manager.worker_records.get_mut(&child).unwrap();
    record.parent_run_id = Some(source.clone());
    record.spec.parent_run_id = Some(source.clone());
    assert!(
        manager
            .continuation_target_for_caller("workspace", &child, Some(&current), "test")
            .is_ok()
    );
    for forbidden in [&source, &current, &sibling] {
        assert!(
            manager
                .continuation_target_for_caller("workspace", forbidden, Some(&current), "test")
                .is_err()
        );
    }

    // A corrupt source→sibling link must not turn source authority into
    // permission to cancel the unrelated actual successor.
    manager
        .resume_targets
        .insert(child.clone(), sibling.clone());
    let manager = Arc::new(RwLock::new(manager));
    assert!(
        cancel_agent_from_input(
            &json!({"agent_id": child}),
            Arc::clone(&manager),
            &ToolContext::new(dir.path()),
            Some(&current),
        )
        .await
        .is_err()
    );
    assert_eq!(
        manager.read().await.get_result(&sibling).unwrap().status,
        SubAgentStatus::Running
    );
}

#[tokio::test]
async fn lifecycle_detail_budget_keeps_the_transcript_handle_retrievable() {
    let dir = tempdir().unwrap();
    let context = ToolContext::new(dir.path());
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 2);
    let id = manager.insert_test_running_agent("large-checkpoint", dir.path());
    let messages = (0..20)
        .map(|_| Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "🐋".repeat(1024),
                cache_control: None,
            }],
        })
        .collect::<Vec<_>>();
    manager.agents.get_mut(&id).unwrap().checkpoint = Some(build_subagent_checkpoint(
        &id, "retained", &messages, 1, true,
    ));
    let projection = subagent_session_projection(
        &new_shared_subagent_manager(dir.path().to_path_buf(), 1),
        manager.get_result(&id).unwrap(),
        false,
        &context,
        manager.get_worker_record_for_session("workspace", &id),
    )
    .await;
    let expected = projection.transcript_handle.clone();
    let detail = lifecycle::bounded_detail(
        serde_json::to_value(projection).unwrap(),
        lifecycle::compact_row(&manager, &manager.agents[&id]),
        0,
        20,
    );
    assert!(serde_json::to_vec(&detail).unwrap().len() <= 32 * 1024);
    let handle: VarHandle = serde_json::from_value(detail["transcript_handle"].clone()).unwrap();
    assert_eq!(handle.session_id, expected.session_id);
    assert_eq!(handle.name, expected.name);
    assert_eq!(handle.sha256, expected.sha256);
    assert!(
        context
            .runtime
            .handle_store
            .lock()
            .await
            .get(&handle)
            .is_some()
    );
}

#[tokio::test]
async fn lifecycle_cleanup_keeps_source_identity_while_descendants_still_need_it() {
    let dir = tempdir().unwrap();
    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 5);
    let source = manager.insert_test_running_agent("original", dir.path());
    let current = manager.insert_test_running_agent("continuation", dir.path());
    let child = manager.insert_test_running_agent("existing-child", dir.path());
    manager
        .resume_targets
        .insert(source.clone(), current.clone());
    let record = manager.worker_records.get_mut(&child).unwrap();
    record.parent_run_id = Some(source.clone());
    record.spec.parent_run_id = Some(source.clone());
    let old = Instant::now() - Duration::from_secs(120);
    manager.agents.get_mut(&source).unwrap().status = SubAgentStatus::Completed;
    manager.agents.get_mut(&source).unwrap().started_at = old;
    let record = manager.worker_records.get_mut(&source).unwrap();
    record.status = AgentWorkerStatus::Completed;
    record.completed_at_ms = Some(epoch_millis_now().saturating_sub(120_000));

    manager
        .cleanup_for_session("workspace", Duration::from_secs(60))
        .expect("durable worker operation");
    assert_eq!(manager.continuation_target(&source).unwrap(), current);
    assert!(
        manager
            .continuation_target_for_caller("workspace", &child, Some(&current), "test")
            .is_ok()
    );
    manager
        .cancel_agent_for_session("workspace", &source)
        .unwrap();
    assert_eq!(
        manager.get_result(&child).unwrap().status,
        SubAgentStatus::Cancelled
    );

    // Once no live work uses either projection, normal expiry reclaims the
    // archived workers and their continuation edge together.
    for agent in manager.agents.values_mut() {
        agent.started_at = old;
    }
    for record in manager.worker_records.values_mut() {
        record.completed_at_ms = Some(epoch_millis_now().saturating_sub(120_000));
    }
    manager
        .cleanup_for_session("workspace", Duration::from_secs(60))
        .expect("durable worker operation");
    assert!(manager.agents.is_empty());
    assert!(manager.worker_records.is_empty());
    assert!(manager.resume_targets.is_empty());
}

#[test]
fn reasoning_override_note_names_requested_and_effective_tiers() {
    let mut route = ChildRouteReceipt {
        requested_type: "explore".into(),
        requested_profile: Some("scout".into()),
        resolved_profile_id: Some("scout".into()),
        profile_origin: Some("workspace".into()),
        canonical_role: "explore".into(),
        provider_id: "gemini".into(),
        model_id: "gemini-4".into(),
        route_source: "profile.model".into(),
        fallback_note: None,
        requested_reasoning: "xhigh".into(),
        effective_reasoning: Some("high".into()),
        runtime_version: "0.10.3".into(),
        runtime_build_sha: "unknown".into(),
    };
    assert_eq!(
        lifecycle::reasoning_override_note(&route).as_deref(),
        Some(
            "requested thinking 'xhigh' runs as 'high' on gemini/gemini-4 (route ceiling; profile 'scout')"
        )
    );
    route.resolved_profile_id = None;
    assert_eq!(
        lifecycle::reasoning_override_note(&route).as_deref(),
        Some("requested thinking 'xhigh' runs as 'high' on gemini/gemini-4 (route ceiling)")
    );
    route.effective_reasoning = None;
    assert_eq!(
        lifecycle::reasoning_override_note(&route).as_deref(),
        Some("requested thinking 'xhigh' is not sent; gemini/gemini-4 has no reasoning tier")
    );
    route.effective_reasoning = Some("xhigh".into());
    assert_eq!(lifecycle::reasoning_override_note(&route), None);
    route.requested_reasoning = "inherit".into();
    route.effective_reasoning = Some("low".into());
    assert_eq!(lifecycle::reasoning_override_note(&route), None);
}

#[tokio::test]
async fn lifecycle_start_peek_and_status_detail_preserve_fields_and_strip_checkpoint_bloat() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 4);
    let (client, _, _, fixture_config) =
        super::tests::delayed_chat_client(Duration::from_secs(30), "done").await;
    let context = ToolContext::new(dir.path()).with_state_namespace("lifecycle-view-session");
    let mut runtime = super::tests::stub_runtime();
    runtime.client = client;
    runtime.api_config = Some(Arc::new(fixture_config));
    runtime.manager = Arc::clone(&manager);
    runtime.context = context.clone();
    let _cancel_on_drop = runtime.cancel_token.clone().drop_guard();

    let tool = AgentTool::new(Arc::clone(&manager), runtime);
    let started = tool
        .execute(
            json!({
                "action": "start",
                "type": "explore",
                "name": "scout-view",
                "prompt": "inspect the harness"
            }),
            &context,
        )
        .await
        .expect("start succeeds");
    let start_view = crate::core::engine::tool_result_context_view(
        codewhale_config::ProviderKind::Deepseek,
        "deepseek-v3.2-128k",
        None,
        "agent",
        &started,
    );
    assert!(!start_view.needs_full_output_artifact);
    assert!(!start_view.text.contains("Child results are self-reports"));
    assert!(
        !start_view
            .text
            .contains("[sub-agent result summarized for parent context]")
    );
    let start_json: Value = serde_json::from_str(&start_view.text).unwrap();
    let agent_id = start_json["agent_id"].as_str().unwrap().to_string();
    assert_eq!(start_json["action"], "start");
    assert_eq!(start_json["role"], "explore");
    assert_eq!(start_json["name"], "scout-view");
    assert_eq!(start_json["status"], "running");
    assert_eq!(
        start_json["effective_limits"]["max_steps"],
        WorkerRuntimeProfile::EXPLORE_MAX_STEPS
    );
    assert!(start_json["transcript_handle"].is_object());

    let child = manager
        .write()
        .await
        .agents
        .get_mut(&agent_id)
        .and_then(|agent| agent.task_handle.take())
        .expect("started child has a task");
    child.abort();
    if let Err(error) = child.await {
        assert!(error.is_cancelled(), "started child panicked: {error}");
    }

    let giant_sig = "SIG_BASE64_".repeat(500);
    {
        let mut guard = manager.write().await;
        let agent = guard.agents.get_mut(&agent_id).unwrap();
        agent.status = SubAgentStatus::Running;
        agent.steps_taken = 4;
        agent.checkpoint = Some(build_subagent_checkpoint(
            &agent_id,
            "checkpointed",
            &[Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "deep thoughts".into(),
                        signature: Some(giant_sig.clone()),
                        state: None,
                        redacted_data: None,
                    },
                    ContentBlock::ToolUse {
                        id: "call-1".into(),
                        execution_id: None,
                        name: "read".into(),
                        input: json!({"path": "Cargo.toml"}),
                        caller: None,
                        thought_signature: Some(giant_sig.clone()),
                    },
                ],
            }],
            4,
            true,
        ));
        let record = guard.worker_records.get_mut(&agent_id).unwrap();
        record.usage.input_tokens = Some(1200);
        record.usage.output_tokens = Some(250);
        record.usage.total_tokens = Some(1450);
        record.latest_message = Some("reading Cargo.toml".into());
        record.events.clear();
        for i in 0..120_u64 {
            record.events.push_back(AgentWorkerEvent {
                seq: i + 1,
                worker_id: agent_id.clone(),
                status: AgentWorkerStatus::RunningTool,
                timestamp_ms: 1_700_000_000_000 + i,
                message: Some(format!("step event {i}")),
                step: Some(4),
                tool_name: Some("read".into()),
            });
        }
        guard.persist_state_synchronously().unwrap();
    }

    let peek = tool
        .execute(json!({"action": "peek", "agent_id": agent_id}), &context)
        .await
        .expect("peek succeeds");
    let peek_view = crate::core::engine::tool_result_context_view(
        codewhale_config::ProviderKind::Deepseek,
        "deepseek-v3.2-128k",
        None,
        "agent",
        &peek,
    );
    assert!(!peek_view.needs_full_output_artifact);
    assert!(!peek_view.text.contains("Child results are self-reports"));
    let peek_json: Value = serde_json::from_str(&peek_view.text).unwrap();
    assert_eq!(peek_json["agent_id"], agent_id);
    assert_eq!(peek_json["role"], "explore");
    assert_eq!(peek_json["steps"], 4);
    assert!(peek_json["duration_ms"].is_u64());
    assert_eq!(peek_json["activity"], "reading Cargo.toml");
    assert_eq!(peek_json["usage"]["total_tokens"], 1450);
    assert!(peek_json["transcript_handle"].is_object());
    assert_eq!(peek_json["events_total"], 120);
    assert_eq!(peek_json["recent_events"].as_array().unwrap().len(), 8);
    assert_eq!(peek_json["recent_events"][7]["seq"], 120);

    let detail = tool
        .execute(
            json!({"action": "status", "agent_id": agent_id, "detail": true}),
            &context,
        )
        .await
        .expect("status detail succeeds");
    assert!(detail.content.len() <= 8 * 1024, "{}", detail.content.len());
    assert!(!detail.content.contains("SIG_BASE64_"));
    assert!(!detail.content.contains("thought_signature"));
    let detail_view = crate::core::engine::tool_result_context_view(
        codewhale_config::ProviderKind::Deepseek,
        "deepseek-v3.2-128k",
        None,
        "agent",
        &detail,
    );
    assert!(!detail_view.needs_full_output_artifact);
    assert!(!detail_view.text.contains("Child results are self-reports"));
    let detail_json: Value = serde_json::from_str(&detail_view.text).unwrap();
    assert_eq!(detail_json["agent_id"], agent_id);
    assert_eq!(detail_json["role"], "explore");
    assert_eq!(detail_json["steps"], 4);
    assert_eq!(detail_json["activity"], "reading Cargo.toml");
    assert_eq!(detail_json["usage"]["total_tokens"], 1450);
    assert!(detail_json.get("checkpoint").is_none());
    assert!(detail_json.get("snapshot").is_none());
    assert!(detail_json.get("worker_record").is_none());
    assert_eq!(detail_json["checkpoint_summary"]["message_count"], 1);
    assert_eq!(detail_json["events_total"], 120);
    let handle: VarHandle =
        serde_json::from_value(detail_json["transcript_handle"].clone()).unwrap();
    assert!(
        context
            .runtime
            .handle_store
            .lock()
            .await
            .get(&handle)
            .is_some()
    );
}

#[tokio::test]
async fn lifecycle_cancel_receipt_is_compact_and_suppresses_duplicate_parent_completion() {
    let dir = tempdir().unwrap();
    let manager = new_shared_subagent_manager(dir.path().to_path_buf(), 4);
    let context = ToolContext::new(dir.path()).with_state_namespace("cancel-suppress-session");
    let (completion_tx, mut completion_rx) = mpsc::channel::<SubAgentCompletion>(16);
    let (event_tx, mut event_rx) = mpsc::channel(16);
    let (mailbox, mut mailbox_rx) = Mailbox::new(CancellationToken::new());

    let giant_sig = "SIG_BASE64_".repeat(500);
    let (cancelled_id, async_done_id) = {
        let mut guard = manager.write().await;
        let cancelled_id = "agent_cancel_compact".to_string();
        let async_done_id = "agent_async_finish".to_string();
        for id in [&cancelled_id, &async_done_id] {
            let (input_tx, _input_rx) = mpsc::unbounded_channel();
            let mut agent = SubAgent::new(
                id.clone(),
                FleetRole::Scout,
                "explore repo".into(),
                SubAgentAssignment::new("explore repo".into(), None),
                "deepseek-v4-flash".into(),
                None,
                None,
                input_tx,
                dir.path().to_path_buf(),
                guard.current_session_boot_id.clone(),
            );
            agent.owner_session_id = "cancel-suppress-session".into();
            agent.steps_taken = 9;
            agent.checkpoint = Some(build_subagent_checkpoint(
                id,
                "in-progress",
                &[Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "c1".into(),
                        execution_id: None,
                        name: "read".into(),
                        input: json!({"path": "README.md"}),
                        caller: None,
                        thought_signature: Some(giant_sig.clone()),
                    }],
                }],
                9,
                true,
            ));
            let mut delivery_runtime = super::tests::stub_runtime().child_runtime();
            delivery_runtime.context = context.clone();
            delivery_runtime.parent_completion_tx = Some(completion_tx.clone());
            delivery_runtime.event_tx = Some(event_tx.clone());
            delivery_runtime.mailbox = Some(mailbox.clone());
            agent.terminal_delivery = Some(SubAgentTerminalDeliveryContext::from_runtime(
                &delivery_runtime,
            ));
            guard.agents.insert(id.clone(), agent);
            let mut spec = super::tests::make_worker_spec(id, dir.path().to_path_buf());
            spec.agent_type = FleetRole::Scout;
            guard.register_worker_for_session(spec, "cancel-suppress-session", None);
            let record = guard.worker_records.get_mut(id).unwrap();
            record.usage.input_tokens = Some(5000);
            record.usage.output_tokens = Some(800);
            record.usage.total_tokens = Some(5800);
            if id == &cancelled_id {
                for seq in 1..=6000_u64 {
                    record.events.push_back(AgentWorkerEvent {
                        seq,
                        worker_id: id.clone(),
                        status: AgentWorkerStatus::RunningTool,
                        timestamp_ms: 1_700_000_000_000 + seq,
                        message: Some(format!("event-{seq}")),
                        step: Some(9),
                        tool_name: Some("read".into()),
                    });
                }
            }
        }
        (cancelled_id, async_done_id)
    };

    let tool = AgentTool::new(Arc::clone(&manager), super::tests::stub_runtime());
    let cancel_result = tool
        .execute(
            json!({"action": "cancel", "agent_id": cancelled_id}),
            &context,
        )
        .await
        .expect("cancel succeeds");
    assert!(
        cancel_result.content.len() < lifecycle::COMPACT_CANCEL_BYTES,
        "cancel receipt must stay under 4 KiB, got {}B",
        cancel_result.content.len()
    );
    assert!(!cancel_result.content.contains("SIG_BASE64_"));
    assert!(!cancel_result.content.contains("thought_signature"));
    let cancel_json: Value = serde_json::from_str(&cancel_result.content).unwrap();
    assert_eq!(cancel_json["action"], "cancel");
    assert_eq!(cancel_json["agent_id"], cancelled_id);
    assert_eq!(cancel_json["role"], "explore");
    assert_eq!(cancel_json["status"], "cancelled");
    assert_eq!(cancel_json["steps"], 9);
    assert_eq!(cancel_json["usage"]["total_tokens"], 5800);
    assert_eq!(cancel_json["cancelled_by_parent"], true);
    assert!(cancel_json["transcript_handle"].is_object());

    assert!(
        completion_rx.try_recv().is_err(),
        "explicit parent cancel must not emit duplicate subagent_completion"
    );
    let synthesized = manager
        .read()
        .await
        .terminal_results_excluding_for_session("cancel-suppress-session", &HashSet::new());
    assert!(
        synthesized.is_empty(),
        "explicit parent cancel must not be synthesized into a duplicate completion turn"
    );
    let host_event = event_rx.try_recv().expect("AgentComplete still emitted");
    assert!(matches!(
        host_event,
        Event::AgentComplete { id, outcome: Some(SubAgentStatus::Cancelled), .. }
            if id == cancelled_id
    ));
    assert!(mailbox_rx.drain_available().iter().any(|env| {
        matches!(&env.message, MailboxMessage::Cancelled { agent_id, .. } if agent_id == &cancelled_id)
    }));

    let mut finished = manager.read().await.get_result(&async_done_id).unwrap();
    finished.status = SubAgentStatus::Completed;
    finished.result = Some("finished async work".into());
    finish_terminal_result_owned(Arc::clone(&manager), finished)
        .await
        .expect("async completion commits");
    let delivered = completion_rx
        .try_recv()
        .expect("async completion must still deliver subagent_completion");
    assert_eq!(delivered.agent_id, async_done_id);
}

#[tokio::test]
async fn lifecycle_explore_caps_reasoning_and_strips_ungranted_mcp_server_instructions() {
    let home = tempdir().unwrap();
    let owned_home = home.path().canonicalize().unwrap();
    let _home = crate::test_support::SealedHome::at(&owned_home);
    let mut codex = super::tests::stub_runtime_for_provider("openai-codex");
    codex.model = "gpt-5.5".to_string();

    for parent_effort in [None, Some("high"), Some("xhigh"), Some("max")] {
        let mut rt = codex.clone();
        rt.reasoning_effort = parent_effort.map(str::to_string);
        let route = worker_profile_subagent_assignment_route(
            &rt,
            &ModelRoute::Inherit,
            SubAgentThinking::Inherit,
            &FleetRole::Scout,
        );
        assert_eq!(
            route.reasoning_effort.as_deref(),
            Some("medium"),
            "parent={parent_effort:?}"
        );
    }

    let mut low_parent = codex.clone();
    low_parent.reasoning_effort = Some("low".to_string());
    let low_route = worker_profile_subagent_assignment_route(
        &low_parent,
        &ModelRoute::Inherit,
        SubAgentThinking::Inherit,
        &FleetRole::Scout,
    );
    assert_eq!(low_route.reasoning_effort.as_deref(), Some("low"));

    let mut high_parent = codex.clone();
    high_parent.reasoning_effort = Some("high".to_string());
    let explicit_override = worker_profile_subagent_assignment_route(
        &high_parent,
        &ModelRoute::Inherit,
        SubAgentThinking::Effort(ReasoningEffort::High),
        &FleetRole::Scout,
    );
    assert_eq!(explicit_override.reasoning_effort.as_deref(), Some("high"));

    let worker_route = worker_profile_subagent_assignment_route(
        &high_parent,
        &ModelRoute::Inherit,
        SubAgentThinking::Inherit,
        &FleetRole::Worker,
    );
    assert_eq!(worker_route.reasoning_effort.as_deref(), Some("high"));

    let mut pool = crate::mcp::McpPool::new(codewhale_mcp::config::McpConfig::default());
    pool.insert_test_connection("github", &["list_prs"], Some("GitHub guidance"));
    pool.insert_test_connection("linear", &["list_issues"], Some("Linear guidance"));
    pool.insert_test_connection(
        "computer-use",
        &["screenshot"],
        Some("Computer-use guidance"),
    );
    pool.insert_test_connection("docs", &["search"], Some("Docs guidance"));
    let pool = Arc::new(tokio::sync::Mutex::new(pool));

    let mut explore_runtime = super::tests::stub_runtime()
        .child_runtime()
        .with_mcp_pool(Some(Arc::clone(&pool)));
    explore_runtime.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Scout);
    let explore_auth = engine::ChildAuthority::capture(
        explore_runtime.clone(),
        FleetRole::Scout,
        "agent_explore_mcp".into(),
        "explore".into(),
        None,
    );
    let explore_servers = explore_auth.admitted_mcp_servers(&explore_runtime).await;
    assert!(explore_servers.is_empty());

    let mut worker_runtime = super::tests::stub_runtime()
        .child_runtime()
        .with_mcp_pool(Some(Arc::clone(&pool)));
    worker_runtime.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Worker);
    let scoped_worker_auth = engine::ChildAuthority::capture(
        worker_runtime.clone(),
        FleetRole::Worker,
        "agent_docs_worker".into(),
        "docs-worker".into(),
        Some(vec!["read".into(), "mcp_docs_search".into()]),
    );
    let scoped_servers = scoped_worker_auth
        .admitted_mcp_servers(&worker_runtime)
        .await;
    assert_eq!(scoped_servers, HashSet::from(["docs".to_string()]));

    let full_worker_auth = engine::ChildAuthority::capture(
        worker_runtime.clone(),
        FleetRole::Worker,
        "agent_full_worker".into(),
        "full-worker".into(),
        None,
    );
    let full_servers = full_worker_auth.admitted_mcp_servers(&worker_runtime).await;
    assert!(!full_servers.contains("computer-use"));
    assert!(full_servers.contains("docs"));

    let parent_fork = SubAgentForkContext {
        messages: vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "investigate subagent lifecycle".into(),
                    cache_control: None,
                }],
            },
            crate::runtime_handoff::mcp_server_instructions_runtime_message(&[
                ("github".into(), "GitHub guidance".into()),
                ("linear".into(), "Linear guidance".into()),
                ("computer-use".into(), "Computer-use guidance".into()),
                ("docs".into(), "Docs guidance".into()),
            ]),
        ],
        structured_state_block: None,
        work_source: None,
    };
    let explore_seed = build_initial_subagent_messages(
        "map the lifecycle",
        &SubAgentAssignment::new("map the lifecycle".into(), None),
        &FleetRole::Scout,
        Some(&parent_fork),
    );
    assert!(
        !explore_seed
            .iter()
            .any(crate::runtime_handoff::is_mcp_server_instructions_message)
    );
}
