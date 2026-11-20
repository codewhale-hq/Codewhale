//! Bounded supervision over the existing manager and persisted continuation map.
use super::*;

pub(super) const COMPACT_STATUS_BYTES: usize = 8192;
pub(super) const COMPACT_SPAWN_BYTES: usize = 4096;
pub(super) const COMPACT_CANCEL_BYTES: usize = 4096;
const DETAIL_STATUS_BYTES: usize = 8 * 1024 - r#","action":"status""#.len();
const PEEK_RECENT_EVENTS_LIMIT: usize = 8;

pub(super) fn compact_transcript_handle(mut handle: Value) -> Value {
    if let Some(preview) = handle.get("repr_preview").and_then(Value::as_str) {
        handle["repr_preview"] = json!(text_preview(preview, 160));
    }
    handle
}

fn compact_event_entry(event: &AgentWorkerEvent) -> Value {
    let mut entry = json!({
        "seq": event.seq,
        "status": agent_worker_status_name(event.status),
        "timestamp_ms": event.timestamp_ms,
    });
    let obj = entry.as_object_mut().expect("event object");
    if let Some(step) = event.step {
        obj.insert("step".into(), json!(step));
    }
    if let Some(tool_name) = event.tool_name.as_deref() {
        obj.insert("tool_name".into(), json!(text_preview(tool_name, 64)));
    }
    if let Some(message) = event.message.as_deref() {
        obj.insert("message".into(), json!(text_preview(message, 160)));
    }
    entry
}

fn compact_event_json(event: &Value) -> Value {
    let Some(source) = event.as_object() else {
        return json!(text_preview(&event.to_string(), 160));
    };
    let mut obj = serde_json::Map::new();
    for key in ["seq", "status", "timestamp_ms", "step"] {
        if let Some(val) = source.get(key) {
            obj.insert(key.to_string(), val.clone());
        }
    }
    if let Some(tool_name) = source.get("tool_name").and_then(Value::as_str) {
        obj.insert("tool_name".into(), json!(text_preview(tool_name, 64)));
    }
    if let Some(message) = source.get("message").and_then(Value::as_str) {
        obj.insert("message".into(), json!(text_preview(message, 160)));
    }
    Value::Object(obj)
}

pub(super) fn attach_peek_recent_events(row: &mut Value, record: Option<&AgentWorkerRecord>) {
    let Some(obj) = row.as_object_mut() else {
        return;
    };
    match record {
        Some(record) => {
            let total = record.events.len();
            let start = total.saturating_sub(PEEK_RECENT_EVENTS_LIMIT);
            let recent: Vec<Value> = record
                .events
                .iter()
                .skip(start)
                .map(compact_event_entry)
                .collect();
            obj.insert("recent_events".into(), Value::Array(recent));
            obj.insert("events_total".into(), json!(total));
        }
        None => {
            obj.insert("recent_events".into(), Value::Array(Vec::new()));
            obj.insert("events_total".into(), json!(0));
        }
    }
}

pub(super) fn compact_cancel_receipt(
    snapshot: &SubAgentResult,
    projection: &SubAgentSessionProjection,
    record: Option<&AgentWorkerRecord>,
) -> Value {
    let usage = record.map(|rec| &rec.usage).unwrap_or(&projection.usage);
    let handle = compact_transcript_handle(
        serde_json::to_value(&projection.transcript_handle).unwrap_or(Value::Null),
    );
    let mut receipt = json!({
        "action": "cancel",
        "agent_id": projection.agent_id,
        "name": text_preview(&projection.name, 64),
        "role": snapshot.agent_type.as_str(),
        "status": projection.status,
        "terminal": projection.terminal,
        "steps": snapshot.steps_taken,
        "steps_taken": snapshot.steps_taken,
        "duration_ms": snapshot.duration_ms,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
        },
        "transcript_handle": handle,
        "cancelled_by_parent": snapshot.status == SubAgentStatus::Cancelled,
    });
    if let Some(summary) = snapshot
        .result
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        receipt["summary"] = json!(text_preview(summary, 512));
    }
    if serde_json::to_vec(&receipt).map_or(true, |bytes| bytes.len() > COMPACT_CANCEL_BYTES)
        && let Some(summary) = receipt.get("summary").and_then(Value::as_str)
    {
        receipt["summary"] = json!(text_preview(summary, 160));
    }
    receipt
}

pub(super) fn text_preview(value: &str, bytes: usize) -> String {
    if value.len() <= bytes {
        return value.to_string();
    }
    let mut end = bytes.saturating_sub(3).min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

pub(super) fn reasoning_override_note(route: &ChildRouteReceipt) -> Option<String> {
    let requested = route.requested_reasoning.trim();
    if matches!(requested, "" | "inherit" | "auto") {
        return None;
    }
    let effective = route
        .effective_reasoning
        .as_deref()
        .map(str::trim)
        .filter(|effort| !effort.is_empty());
    if effective.is_some_and(|effort| effort.eq_ignore_ascii_case(requested)) {
        return None;
    }
    let route_context = match route.resolved_profile_id.as_deref() {
        Some(profile) => format!(" (route ceiling; profile '{profile}')"),
        None => " (route ceiling)".to_string(),
    };
    let no_tier_context = match route.resolved_profile_id.as_deref() {
        Some(profile) => format!(" (profile '{profile}')"),
        None => String::new(),
    };
    Some(text_preview(
        &match effective {
            Some(effort) => {
                format!(
                    "requested thinking '{requested}' runs as '{effort}' on {}/{}{route_context}",
                    route.provider_id, route.model_id
                )
            }
            None => {
                format!(
                    "requested thinking '{requested}' is not sent; {}/{}{no_tier_context} has no reasoning tier",
                    route.provider_id, route.model_id
                )
            }
        },
        240,
    ))
}

pub(super) fn page(input: &Value) -> Result<(usize, usize), ToolError> {
    let number = |key: &str, default| -> Result<usize, ToolError> {
        match input.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    ToolError::invalid_input(format!("{key} must be a nonnegative integer"))
                }),
        }
    };
    let offset = number("offset", 0)?;
    let limit = number("limit", 20)?;
    if !(1..=20).contains(&limit) {
        return Err(ToolError::invalid_input("limit must be 1..20"));
    }
    Ok((offset, limit))
}

// Keep the same typed route receipt on every surface. Exceptionally long
// profile/model labels are previews; detail inspection retains their source.
pub(super) fn compact_child_route(mut route: Value) -> Value {
    if serde_json::to_vec(&route).is_ok_and(|bytes| bytes.len() <= 1024) {
        return route;
    }
    let Some(object) = route.as_object_mut() else {
        return route;
    };
    for cap in [128, 64, 32, 16, 8] {
        let mut truncated = false;
        for value in object.values_mut() {
            if let Value::String(text) = value
                && text.len() > cap
            {
                *text = text_preview(text, cap);
                truncated = true;
            }
        }
        if truncated {
            object.insert("truncated".into(), json!(true));
        }
        if serde_json::to_vec(&object).is_ok_and(|bytes| bytes.len() <= 1024) {
            break;
        }
    }
    route
}

pub(super) fn status_result(mut payload: Value, peek: bool) -> Result<ToolResult, ToolError> {
    let action = if peek { "peek" } else { "status" };
    payload["action"] = json!(action);
    let mut metadata = if payload.get("agent_id").is_some() {
        json!({
            "action": action, "agent_id": payload["agent_id"],
            "status": payload["status"], "terminal": payload["terminal"],
            "child_route": payload["child_route"],
        })
    } else {
        json!({"action": action, "count": payload["count"]})
    };
    if let Some(unchanged) = payload.get("unchanged") {
        metadata["unchanged"] = unchanged.clone();
    }
    let mut result = ToolResult::json(&payload)
        .map_err(|error| ToolError::execution_failed(error.to_string()))?;
    result.metadata = Some(metadata);
    Ok(result)
}

pub(super) fn compact_row(manager: &SubAgentManager, agent: &SubAgent) -> Value {
    let record = manager.worker_records.get(&agent.id);
    let current = manager.continuation_target(&agent.id);
    let continuable = matches!(agent.status, SubAgentStatus::Interrupted(_))
        && agent
            .checkpoint
            .as_ref()
            .is_some_and(|cp| cp.continuable && !cp.messages.is_empty());
    let status = record
        .map(|record| agent_worker_status_name(record.status))
        .unwrap_or_else(|| {
            if continuable {
                "waiting_for_user"
            } else {
                subagent_status_name(&agent.status)
            }
        });
    let mut row = json!({
        "agent_id": agent.id, "name": text_preview(&agent.session_name, 64),
        "role": agent.agent_type.as_str(),
        "status": status, "terminal": agent.status != SubAgentStatus::Running,
        "compact": true, "steps": agent.steps_taken, "steps_taken": agent.steps_taken,
        "duration_ms": u64::try_from(agent.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        "needs_continuation": continuable, "usage": {},
    });
    let object = row.as_object_mut().expect("row is an object");
    if let Some(record) = record {
        if let Some(parent) = record
            .parent_run_id
            .as_ref()
            .or(record.spec.parent_run_id.as_ref())
        {
            object.insert("parent_agent_id".into(), json!(parent));
        }
        object.insert("spawn_depth".into(), json!(record.spec.spawn_depth));
        object.insert("max_spawn_depth".into(), json!(record.spec.max_spawn_depth));
        if let Ok(profile) = serde_json::to_value(&record.spec.runtime_profile) {
            let mut limits = serde_json::Map::new();
            for key in ["max_steps", "wall_time_secs", "wall_deadline_ms"] {
                if let Some(value) = profile.get(key) {
                    limits.insert(key.to_string(), value.clone());
                }
            }
            object.insert("effective_limits".into(), Value::Object(limits));
        }
        let usage = &record.usage;
        // These are this worker's receipts. Shared scope expenditure must not
        // be added once per descendant when presenting a subtree total.
        object.insert(
            "usage".into(),
            json!({
                "input_tokens": usage.input_tokens, "output_tokens": usage.output_tokens,
                "total_tokens": usage.total_tokens,
            }),
        );
        object.insert("last_activity_ms".into(), json!(record.updated_at_ms));
        if let Some(message) = &record.latest_message {
            object.insert("activity".into(), json!(text_preview(message, 96)));
        }
        let mut verification = serde_json::to_value(&record.verification).unwrap_or(Value::Null);
        if let Some(verdicts) = verification
            .get_mut("deliverables")
            .and_then(Value::as_array_mut)
        {
            let mut counts = std::collections::BTreeMap::<String, usize>::new();
            for verdict in verdicts.iter() {
                *counts
                    .entry(
                        verdict
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                    )
                    .or_default() += 1;
            }
            verdicts.sort_by_key(|verdict| {
                matches!(
                    verdict.get("status").and_then(Value::as_str),
                    Some("present" | "pending")
                )
            });
            let total = verdicts.len();
            verification["deliverables_total"] = json!(total);
            verification["deliverables_omitted"] = json!(total.saturating_sub(4));
            verification["deliverable_counts"] = json!(counts);
        }
        bound_detail_value(&mut verification, 0, 4, &mut 1200);
        object.insert("verification".into(), verification);
        if let Some(route) = &record.spec.child_route {
            object.insert(
                "child_route".into(),
                compact_child_route(serde_json::to_value(route).unwrap_or(Value::Null)),
            );
        }
        // #6194 item 5: live declared-vs-observed write surfacing. The child
        // declares deliverables at spawn and the registry records every
        // successful scoped write; the parent sees the diff while the child
        // is still alive instead of only in the post-mortem receipt.
        if record.spec.runtime_profile.permissions.write {
            const MAX_LISTED_WRITES: usize = 4;
            let declared: Vec<String> = record
                .spec
                .launch_manifest
                .as_ref()
                .map(|manifest| {
                    manifest
                        .deliverables
                        .iter()
                        .take(MAX_LISTED_WRITES)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let declared_total = record
                .spec
                .launch_manifest
                .as_ref()
                .map(|manifest| manifest.deliverables.len())
                .unwrap_or(0);
            let observed: Vec<String> = record
                .delivery_evidence
                .observed_writes
                .iter()
                .take(MAX_LISTED_WRITES)
                .cloned()
                .collect();
            let observed_total = record.delivery_evidence.observed_writes.len();
            if declared_total > 0 || observed_total > 0 {
                object.insert(
                    "write_progress".into(),
                    json!({
                        "declared": declared,
                        "declared_total": declared_total,
                        "observed": observed,
                        "observed_total": observed_total,
                    }),
                );
            }
        }
    }
    if let Some(source) = manager.continuation_source(&agent.id) {
        object.insert("resumed_from".into(), json!(source));
    }
    match current {
        Ok(target) if target != agent.id => {
            object.insert("resumed_as".into(), json!(target));
        }
        Err(error) => {
            object.insert(
                "lineage_error".into(),
                json!(text_preview(&error.to_string(), 120)),
            );
        }
        _ => {}
    }
    if let Some(input) = &agent.needs_input {
        object.insert(
            "needs_input".into(),
            json!(text_preview(&input.question, 160)),
        );
    }
    let stop_reason = match &agent.status {
        SubAgentStatus::Failed(reason) | SubAgentStatus::Interrupted(reason) => Some(reason),
        _ => agent
            .result
            .as_ref()
            .filter(|_| agent.status != SubAgentStatus::Running),
    };
    if let Some(reason) = stop_reason {
        object.insert("summary".into(), json!(text_preview(reason, 160)));
    }
    if serde_json::to_vec(&row).is_ok_and(|bytes| bytes.len() > 3072) {
        // A verdict containing highly escaped prose can exceed its raw-byte
        // allowance. Keep the authoritative verdict status and name the
        // omitted detail, rather than returning a page that cannot advance.
        let status = row
            .pointer("/verification/status")
            .cloned()
            .unwrap_or(Value::Null);
        let counts = row.pointer("/verification/deliverable_counts").cloned();
        let total = row.pointer("/verification/deliverables_total").cloned();
        row["verification"] = json!({"status": status, "deliverable_counts": counts, "deliverables_total": total, "detail_required": true});
    }
    row
}

pub(super) fn compact_roster(
    manager: &SubAgentManager,
    input: &Value,
    session: &str,
    archived: bool,
    peek: bool,
) -> Result<Value, ToolError> {
    let (offset, limit) = page(input)?;
    let mut agents = manager
        .agents
        .values()
        .filter(|agent| manager.agent_is_owned_by_session(agent, session))
        .filter(|agent| archived || !manager.is_from_prior_session(agent))
        .collect::<Vec<_>>();
    // Live and waiting work comes first; tie-break with immutable ids, so a
    // page is reproducible for unchanged state even across process restarts.
    agents.sort_by(|a, b| {
        (a.status != SubAgentStatus::Running, &a.id)
            .cmp(&(b.status != SubAgentStatus::Running, &b.id))
    });
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    let mut total_tokens = 0_u64;
    let mut reported_workers = 0_usize;
    for agent in &agents {
        *counts
            .entry(subagent_status_name(&agent.status))
            .or_default() += 1;
        if let Some(tokens) = manager
            .worker_records
            .get(&agent.id)
            .and_then(|record| record.usage.total_tokens)
        {
            total_tokens = total_tokens.saturating_add(tokens);
            reported_workers += 1;
        }
    }
    let total = agents.len();
    // A stable header carries field names once. Null cells mean absent or
    // unreported values; a reported zero remains a numeric zero.
    let columns = [
        "agent_id",
        "parent_agent_id",
        "resumed_from",
        "resumed_as",
        "spawn_depth",
        "status",
        "duration_ms",
        "total_tokens",
        "last_activity_ms",
        "activity",
        "needs_input",
        "needs_continuation",
        "verification",
        "summary",
        "lineage_error",
    ];
    let mut rows = agents
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|agent| {
            let mut row = compact_row(manager, agent);
            let total_tokens = row
                .pointer("/usage/total_tokens")
                .cloned()
                .unwrap_or(Value::Null);
            row["total_tokens"] = total_tokens;
            if let Some(record) = manager.worker_records.get(&agent.id) {
                let mut verification =
                    json!({"status": text_preview(&record.verification.status, 64)});
                if !matches!(
                    record.verification.status.as_str(),
                    "self_report_only" | "deliverables_present"
                ) && !record.verification.summary.is_empty()
                {
                    verification["summary"] =
                        json!(text_preview(&record.verification.summary, 160));
                }
                if let Some(counts) = row
                    .pointer("/verification/deliverable_counts")
                    .filter(|value| value.as_object().is_some_and(|counts| !counts.is_empty()))
                {
                    verification["deliverable_counts"] = counts.clone();
                }
                row["verification"] = verification;
            }
            Value::Array(
                columns
                    .iter()
                    .map(|column| row.get(*column).cloned().unwrap_or(Value::Null))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    loop {
        let shown = rows.len();
        let next = offset.saturating_add(shown);
        let payload = json!({
            "action": if peek { "peek" } else { "status" }, "compact": true,
            "count": shown, "total_count": total, "status_counts": counts,
            "usage": {"total_tokens": total_tokens, "reported_workers": reported_workers, "workers": total},
            "columns": columns, "agents": rows, "offset": offset,
            "next_offset": (next < total).then_some(next),
            "omitted": total.saturating_sub(shown),
            "detail_hint": "Rows follow columns. Inspect with agent_id and detail=true.",
        });
        if serde_json::to_vec(&payload)
            .map_err(|error| ToolError::execution_failed(error.to_string()))?
            .len()
            <= COMPACT_STATUS_BYTES
        {
            return Ok(payload);
        }
        if rows.pop().is_none() {
            return Err(ToolError::execution_failed(
                "Status envelope exceeds its byte limit",
            ));
        }
    }
}

// Bound arbitrary message/tool-input JSON as it is copied into a diagnostic
// page. The transcript handle remains the authoritative unabridged source.
fn bound_detail_value(value: &mut Value, offset: usize, limit: usize, budget: &mut usize) {
    if *budget == 0 {
        *value = json!("[omitted]");
        return;
    }
    match value {
        Value::String(text) => {
            *text = text_preview(text, (*budget).min(1024));
            *budget = budget.saturating_sub(text.len());
        }
        Value::Array(items) => {
            *items = std::mem::take(items)
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect();
            for item in items {
                bound_detail_value(item, 0, limit, budget);
            }
        }
        Value::Object(object) => {
            object.retain(|key, _| key.len() <= 128);
            for child in object.values_mut() {
                bound_detail_value(child, 0, limit, budget);
            }
        }
        _ => {
            *budget = budget.saturating_sub(16);
        }
    }
}

fn detail_fits(page: &Value) -> bool {
    serde_json::to_vec(page).is_ok_and(|bytes| bytes.len() <= DETAIL_STATUS_BYTES)
}

fn truncate_event_page(detail: &mut Value, offset: usize, retained: usize) {
    if let Some(events) = detail
        .get_mut("recent_events")
        .and_then(Value::as_array_mut)
        && events.len() > retained
    {
        events.truncate(retained);
        detail["events_next_offset"] = json!(offset.saturating_add(retained));
        detail["events_omitted"] = json!(true);
    }
}

pub(super) fn bounded_detail(value: Value, compact: Value, offset: usize, limit: usize) -> Value {
    // The typed handle is the retrieval authority, not diagnostic prose.
    // Large earlier fields must never consume its lookup coordinates.
    let transcript_handle = value
        .get("transcript_handle")
        .filter(|handle| !handle.is_null())
        .cloned()
        .map(compact_transcript_handle);
    let mut verification = value.get("verification").cloned().unwrap_or(Value::Null);
    if let Some(verdicts) = verification
        .get_mut("deliverables")
        .and_then(Value::as_array_mut)
    {
        let total = verdicts.len();
        *verdicts = std::mem::take(verdicts)
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();
        verification["deliverables_total"] = json!(total);
        verification["deliverables_next_offset"] = (offset.saturating_add(limit) < total)
            .then_some(offset.saturating_add(limit))
            .map_or(Value::Null, |next| json!(next));
    }
    bound_detail_value(&mut verification, 0, limit, &mut 2048);

    let mut detail = serde_json::Map::new();
    if let Some(compact_obj) = compact.as_object() {
        for (key, val) in compact_obj {
            detail.insert(key.clone(), val.clone());
        }
    }
    if !detail.contains_key("role")
        && let Some(role) = value
            .pointer("/snapshot/agent_type")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .pointer("/child_route/canonical_role")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                value
                    .pointer("/worker_record/spec/agent_type")
                    .and_then(Value::as_str)
            })
    {
        detail.insert("role".into(), json!(role));
    }
    if let Some(steps) = detail
        .get("steps")
        .or_else(|| detail.get("steps_taken"))
        .or_else(|| value.pointer("/snapshot/steps_taken"))
        .cloned()
    {
        detail.insert("steps".into(), steps.clone());
        detail.insert("steps_taken".into(), steps);
    }
    if let Some(objective) = value
        .pointer("/snapshot/assignment/objective")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/worker_record/spec/objective")
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        detail.insert("objective".into(), json!(text_preview(objective, 320)));
    }
    if let Some(result_text) = value
        .pointer("/snapshot/result")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let bounded = text_preview(result_text, 2048);
        let truncated = bounded.len() < result_text.len();
        detail.insert("result".into(), json!(bounded));
        if truncated {
            detail.insert("result_truncated".into(), json!(true));
        }
    }
    if let Some(checkpoint) = value
        .get("checkpoint")
        .filter(|cp| cp.is_object())
        .or_else(|| {
            value
                .pointer("/snapshot/checkpoint")
                .filter(|cp| cp.is_object())
        })
    {
        let mut summary = serde_json::Map::new();
        if let Some(reason) = checkpoint.get("reason").and_then(Value::as_str) {
            summary.insert("reason".into(), json!(text_preview(reason, 160)));
        }
        if let Some(step) = checkpoint.get("step") {
            summary.insert("step".into(), step.clone());
        }
        if let Some(messages) = checkpoint.get("messages").and_then(Value::as_array) {
            summary.insert("message_count".into(), json!(messages.len()));
        }
        if let Some(continuable) = checkpoint.get("continuable") {
            summary.insert("continuable".into(), continuable.clone());
        }
        if !summary.is_empty() {
            detail.insert("checkpoint_summary".into(), Value::Object(summary));
        }
    }
    if let Some(events) = value
        .pointer("/worker_record/events")
        .and_then(Value::as_array)
    {
        let total = events.len();
        let page_events: Vec<Value> = events
            .iter()
            .skip(offset)
            .take(limit)
            .map(compact_event_json)
            .collect();
        let next = offset.saturating_add(page_events.len());
        detail.insert("recent_events".into(), Value::Array(page_events));
        detail.insert("events_total".into(), json!(total));
        if next < total {
            detail.insert("events_next_offset".into(), json!(next));
        }
    }
    if let Some(continuable) = value.get("continuable") {
        detail.insert("continuable".into(), continuable.clone());
    }
    detail.insert("verification".into(), verification);
    let has_handle = transcript_handle.is_some();
    if let Some(handle) = transcript_handle {
        detail.insert("transcript_handle".into(), handle);
    }
    detail.insert("compact".into(), json!(false));
    detail.insert("detail_bounded".into(), json!(true));
    detail.insert("detail_offset".into(), json!(offset));
    detail.insert("detail_limit".into(), json!(limit));
    detail.insert(
        "detail_hint".into(),
        json!(if has_handle {
            "Fields are bounded; use offset/limit for recent_events and deliverables, or transcript_handle with handle_read for the complete retained transcript."
        } else {
            "Fields are bounded; use offset/limit for recent_events and deliverables."
        }),
    );

    let mut out = Value::Object(detail);
    if !detail_fits(&out) {
        if let Some(result_text) = out
            .get("result")
            .and_then(Value::as_str)
            .map(|text| text_preview(text, 512))
        {
            out["result"] = json!(result_text);
            out["result_truncated"] = json!(true);
        }
        truncate_event_page(&mut out, offset, 4);
    }
    if !detail_fits(&out) {
        truncate_event_page(&mut out, offset, 1);
        let status = out
            .pointer("/verification/status")
            .cloned()
            .unwrap_or(Value::Null);
        let counts = out.pointer("/verification/deliverable_counts").cloned();
        let total = out.pointer("/verification/deliverables_total").cloned();
        let next = out
            .pointer("/verification/deliverables_next_offset")
            .cloned();
        out["verification"] = json!({
            "status": status,
            "deliverable_counts": counts,
            "deliverables_total": total,
            "deliverables_next_offset": next,
            "detail_required": true,
        });
    }
    if !detail_fits(&out) {
        let handle = out.get("transcript_handle").cloned();
        let mut minimal = json!({
            "agent_id": out["agent_id"],
            "role": out["role"],
            "status": out["status"],
            "steps": out["steps"],
            "steps_taken": out["steps_taken"],
            "duration_ms": out["duration_ms"],
            "activity": out["activity"],
            "needs_input": out["needs_input"],
            "summary": out["summary"],
            "usage": out["usage"],
            "verification": out["verification"],
            "detail_bounded": true,
            "omitted_detail": if handle.is_some() {
                "Diagnostic page exceeded 8 KiB; use transcript_handle with handle_read for retained detail."
            } else {
                "Diagnostic page exceeded 8 KiB."
            },
        });
        if let Some(handle) = handle {
            minimal["transcript_handle"] = handle;
        }
        for key in [
            "recent_events",
            "events_total",
            "events_next_offset",
            "events_omitted",
        ] {
            if let Some(value) = out.get_mut(key) {
                minimal[key] = value.take();
            }
        }
        if !detail_fits(&minimal) {
            let identity = minimal.get_mut("agent_id").map(Value::take);
            minimal.as_object_mut().unwrap().retain(|key, _| {
                matches!(
                    key.as_str(),
                    "recent_events"
                        | "events_total"
                        | "events_next_offset"
                        | "events_omitted"
                        | "transcript_handle"
                        | "detail_bounded"
                )
            });
            if !detail_fits(&minimal)
                && let Some(row) = minimal
                    .get_mut("recent_events")
                    .and_then(Value::as_array_mut)
                    .and_then(|events| events.first_mut())
            {
                *row = json!({"seq": row["seq"].as_u64(), "detail_omitted": true});
            }
            if !detail_fits(&minimal) {
                minimal.as_object_mut().unwrap().remove("transcript_handle");
                minimal["transcript_handle_omitted"] = json!(true);
            }
            if let Some(identity) = identity {
                minimal["agent_id"] = identity;
                if !detail_fits(&minimal) {
                    minimal.as_object_mut().unwrap().remove("agent_id");
                }
            }
        }
        out = minimal;
    }
    out
}
