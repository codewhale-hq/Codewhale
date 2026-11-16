use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use super::{ApiError, RuntimeApiState, map_thread_err};

/// Live context-window posture for one thread — the facts a usage
/// panel cannot reconstruct from turn receipts. `input_tokens` is the same
/// conservative estimate the in-app context meter shows; `billed_input_tokens`
/// is the last provider-counted prompt size when one exists.
///
/// All numeric fields are nullable: a route that cannot express a bounded
/// window reports `null`. Static windows retain their resolution source,
/// including a named unverified fallback. `live: false` marks responses where
/// the engine could not be loaded and only the stored route resolved.
#[derive(Debug, Serialize)]
pub(super) struct ThreadContextResponse {
    thread_id: String,
    model: String,
    provider: Option<String>,
    model_provider_id: Option<String>,
    /// `true` when the numbers came from the loaded engine (live estimate +
    /// route limits); `false` when only the store-recorded route resolved.
    live: bool,
    window_tokens: Option<u64>,
    window_source: Option<&'static str>,
    input_tokens: Option<u64>,
    billed_input_tokens: Option<u64>,
    output_cap_tokens: Option<u64>,
    input_budget_ceiling: Option<u64>,
    available_input_tokens: Option<u64>,
    compaction_trigger_tokens: Option<u64>,
    usage_percent: Option<f64>,
    pressure: Option<&'static str>,
}

pub(super) async fn get_thread_context(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
) -> Result<Json<ThreadContextResponse>, ApiError> {
    let thread = state
        .runtime_threads
        .get_thread(&thread_id)
        .await
        .map_err(map_thread_err)?;

    if let Ok(engine) = state.runtime_threads.get_engine(&thread_id).await
        && let Ok(Some(snapshot)) = engine.get_context_budget().await
    {
        return Ok(Json(ThreadContextResponse {
            thread_id,
            model: snapshot.model,
            provider: Some(snapshot.provider),
            model_provider_id: snapshot.model_provider_id,
            live: true,
            window_tokens: Some(snapshot.window_tokens),
            window_source: Some("engine_active_route"),
            input_tokens: Some(snapshot.input_tokens),
            billed_input_tokens: snapshot.billed_input_tokens,
            output_cap_tokens: Some(snapshot.output_cap_tokens),
            input_budget_ceiling: Some(snapshot.input_budget_ceiling),
            available_input_tokens: Some(snapshot.available_input_tokens),
            compaction_trigger_tokens: Some(snapshot.compaction_trigger_tokens),
            usage_percent: Some(snapshot.usage_percent),
            pressure: Some(snapshot.pressure),
        }));
    }

    // Engine unavailable or the route cannot bound a window: still answer
    // with whatever the thread record and the static route catalog can prove.
    let config = state.config.read().clone();
    let route = thread
        .model_provider
        .as_deref()
        .and_then(|provider| {
            config
                .resolve_persisted_provider_identity(
                    Some(provider),
                    thread.model_provider_id.as_deref(),
                )
                .ok()
        })
        .and_then(|identity| {
            codewhale_engine::route_runtime::resolve_runtime_route_for_identity(
                &config,
                &identity,
                Some(&thread.model),
            )
            .ok()
        });
    let window_tokens = route
        .as_ref()
        .map(|route| u64::from(route.context_window.tokens));
    Ok(Json(ThreadContextResponse {
        thread_id,
        model: thread.model,
        provider: thread.model_provider,
        model_provider_id: thread.model_provider_id,
        live: false,
        window_tokens,
        window_source: route
            .as_ref()
            .map(|route| route.context_window.source.label()),
        input_tokens: None,
        billed_input_tokens: None,
        output_cap_tokens: None,
        input_budget_ceiling: None,
        available_input_tokens: None,
        compaction_trigger_tokens: None,
        usage_percent: None,
        pressure: None,
    }))
}

const STANDING_SCHEMA_VERSION: u32 = 1;
const STANDING_ESTIMATOR: &str = "conservative_max_chars_div3_bytes_div4";
const STANDING_NOT_MEASURED: [&str; 4] = ["skills", "agents", "mcp_tool_schemas", "memory"];
pub(super) const STANDING_MAX_ITEMS: usize = 200;
const STANDING_LABEL_MAX_CHARS: usize = 80;
const STANDING_DETAIL_KEYS: [&str; 4] = ["kind", "budget_tokens", "max_bytes", "max_files"];

fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn inert_text(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                '?'
            } else {
                ch
            }
        })
        .collect()
}

fn slash_joined(path: &std::path::Path) -> String {
    path.components()
        .map(|part| inert_text(&part.as_os_str().to_string_lossy()))
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn standing_display_path(
    path: &std::path::Path,
    global: bool,
    workspace_roots: &[std::path::PathBuf],
    home_roots: &[std::path::PathBuf],
) -> String {
    let name = path
        .file_name()
        .map(|name| inert_text(&name.to_string_lossy()))
        .unwrap_or_default();
    if global {
        return home_roots
            .iter()
            .filter(|home| home.parent().is_some())
            .find_map(|home| path.strip_prefix(home).ok())
            .map_or(name, |relative| format!("~/{}", slash_joined(relative)));
    }
    workspace_roots
        .iter()
        .filter_map(|root| {
            let shared = root
                .ancestors()
                .filter(|ancestor| ancestor.parent().is_some())
                .find(|ancestor| path.starts_with(ancestor))?;
            let up = root.strip_prefix(shared).ok()?.components().count();
            Some((up, path.strip_prefix(shared).ok()?))
        })
        .min_by_key(|(up, _)| *up)
        .map_or(name, |(up, down)| {
            format!("{}{}", "../".repeat(up), slash_joined(down))
        })
}

fn standing_item(
    notice: &serde_json::Value,
    workspace_roots: &[std::path::PathBuf],
    home_roots: &[std::path::PathBuf],
) -> serde_json::Value {
    let key = |field: &str| {
        notice
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(snake_case)
    };
    let global = key("scope").as_deref() == Some("global");
    let path = notice
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(std::path::Path::new)
        .map(|path| standing_display_path(path, global, workspace_roots, home_roots));
    let label = path
        .as_deref()
        .and_then(|path| path.rsplit('/').next())
        .map(|name| name.chars().take(STANDING_LABEL_MAX_CHARS).collect())
        .unwrap_or_else(|| format!("{} instructions", key("scope").unwrap_or_default()));
    let detail = notice
        .get("detail")
        .and_then(serde_json::Value::as_object)
        .map(|detail| {
            serde_json::Value::Object(
                detail
                    .iter()
                    .filter(|(field, _)| STANDING_DETAIL_KEYS.contains(&field.as_str()))
                    .map(|(field, value)| (field.clone(), value.clone()))
                    .collect(),
            )
        })
        .unwrap_or_default();
    serde_json::json!({
        "label": label,
        "path": path,
        "scope": key("scope"),
        "origin": key("origin"),
        "action": key("action"),
        "limit": key("limit"),
        "tokens_kept": notice.get("tokens_kept"),
        "tokens_dropped": notice.get("tokens_dropped"),
        "bytes_kept": notice.get("bytes_kept"),
        "bytes_dropped": notice.get("bytes_dropped"),
        "fallback_selected": notice.get("fallback_selected"),
        "detail": detail,
    })
}

pub(super) fn standing_items(
    notices: &[serde_json::Value],
    workspace_roots: &[std::path::PathBuf],
    home_roots: &[std::path::PathBuf],
) -> (Vec<serde_json::Value>, usize) {
    let items = notices
        .iter()
        .take(STANDING_MAX_ITEMS)
        .map(|notice| standing_item(notice, workspace_roots, home_roots))
        .collect();
    (items, notices.len().saturating_sub(STANDING_MAX_ITEMS))
}

fn standing_response(
    thread_id: &str,
    live: bool,
    basis: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "schema_version": STANDING_SCHEMA_VERSION,
        "thread_id": thread_id,
        "live": live,
        "basis": basis,
        "estimator": STANDING_ESTIMATOR,
        "estimated": true,
        "not_measured": STANDING_NOT_MEASURED,
        "items": [],
    });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    body
}

pub(super) async fn get_thread_standing_context(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .runtime_threads
        .get_thread(&thread_id)
        .await
        .map_err(map_thread_err)?;

    let snapshot = match state.runtime_threads.get_engine(&thread_id).await {
        Ok(engine) => engine.get_session_snapshot().await.ok(),
        Err(_) => None,
    };
    let Some(snapshot) = snapshot else {
        return Ok(Json(standing_response(
            &thread_id,
            false,
            "unavailable",
            serde_json::Value::Null,
        )));
    };
    let Some(receipt) = snapshot.instruction_assembly else {
        return Ok(Json(standing_response(
            &thread_id,
            true,
            "no_receipt",
            serde_json::Value::Null,
        )));
    };

    let workspace = snapshot.workspace;
    let (workspace_roots, home_roots) = tokio::task::spawn_blocking(move || {
        let with_canonical = |path: std::path::PathBuf| {
            let mut roots = vec![path.clone()];
            if let Ok(canonical) = std::fs::canonicalize(&path)
                && canonical != path
            {
                roots.push(canonical);
            }
            roots
        };
        (
            with_canonical(workspace),
            codewhale_paths::user_home()
                .map(with_canonical)
                .unwrap_or_default(),
        )
    })
    .await
    .map_err(|_| ApiError::internal("instruction receipt path resolution failed"))?;

    let (items, items_omitted) = standing_items(
        receipt
            .get("notices")
            .and_then(serde_json::Value::as_array)
            .map_or(&[][..], Vec::as_slice),
        &workspace_roots,
        &home_roots,
    );
    let delivery = codewhale_engine::runtime_handoff::project_instructions_delivery(
        &snapshot.messages,
    )
    .map(|delivery| {
        serde_json::json!({
            "message_bytes": delivery.message_bytes,
            "message_tokens": delivery.message_tokens,
            "truncated": delivery.truncated,
        })
    });
    Ok(Json(standing_response(
        &thread_id,
        true,
        "installed_receipt",
        serde_json::json!({
            "global_tokens": receipt.get("global_tokens"),
            "project_tokens": receipt.get("project_tokens"),
            "constitution_tokens": receipt.get("constitution_tokens"),
            "retained_block_tokens": receipt.get("retained_block_tokens"),
            "budgets": receipt.get("budgets"),
            "items": items,
            "items_omitted": items_omitted,
            "delivery": {
                "cap_bytes": codewhale_engine::runtime_handoff::PROJECT_INSTRUCTIONS_MAX_BYTES,
                "delivered": delivery,
            },
        }),
    )))
}
