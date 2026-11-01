//! Response shaping: what a `gh` payload looks like by the time the model
//! sees it.
//!
//! Long bodies and diffs spill to task artifacts and come back as summaries,
//! and every write action returns the task metadata that records what it did.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::task_manager::{TaskArtifactRef, TaskGithubEvent};
use crate::tools::spec::{ToolContext, ToolError};

pub(super) const BODY_ARTIFACT_THRESHOLD: usize = 4_000;
pub(super) const DIFF_ARTIFACT_THRESHOLD: usize = 8_000;

pub(super) fn shape_large_text(
    context: &ToolContext,
    mut value: Value,
    label: &str,
    threshold: usize,
) -> Result<Value, ToolError> {
    let body = value
        .get("body")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(body) = body
        && body.len() > threshold
    {
        let artifact = write_artifact_if_needed(context, label, &body, threshold)?;
        value["body_summary"] = json!(summarize(&body, 900));
        value["body_artifact"] = json!(artifact);
        value["body"] = json!(summarize(&body, 1200));
    }
    Ok(value)
}

pub(super) fn write_artifact_if_needed(
    context: &ToolContext,
    label: &str,
    content: &str,
    threshold: usize,
) -> Result<Option<PathBuf>, ToolError> {
    if content.len() <= threshold {
        return Ok(None);
    }
    let Some(task_id) = context.runtime.active_task_id.as_deref() else {
        return Ok(None);
    };
    if let Some(manager) = context.runtime.task_manager.as_ref() {
        return manager
            .write_task_artifact(task_id, label, content)
            .map(Some)
            .map_err(|e| ToolError::execution_failed(e.to_string()));
    }
    let Some(data_dir) = context.runtime.task_data_dir.as_ref() else {
        return Ok(None);
    };
    let dir = data_dir.join("artifacts").join(task_id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| ToolError::execution_failed(format!("create artifact dir: {e}")))?;
    let absolute = dir.join(format!(
        "{}_{}.txt",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        sanitize_filename(label)
    ));
    std::fs::write(&absolute, content)
        .map_err(|e| ToolError::execution_failed(format!("write artifact: {e}")))?;
    Ok(Some(
        absolute
            .strip_prefix(data_dir)
            .map(Path::to_path_buf)
            .unwrap_or(absolute),
    ))
}

pub(super) fn artifact_refs_from_context(content: &str, label: &str) -> Vec<TaskArtifactRef> {
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return Vec::new();
    };
    let (path_key, summary_key) = if label.ends_with("_diff") {
        ("diff_artifact", "diff_summary")
    } else {
        ("body_artifact", "body_summary")
    };
    let mut refs = Vec::new();
    collect_artifact_refs(&value, path_key, summary_key, label, &mut refs);
    refs
}

fn collect_artifact_refs(
    value: &Value,
    path_key: &str,
    summary_key: &str,
    label: &str,
    refs: &mut Vec<TaskArtifactRef>,
) {
    match value {
        Value::Object(map) => {
            if let Some(path) = map.get(path_key).and_then(Value::as_str) {
                let summary = map
                    .get(summary_key)
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| format!("GitHub {label} artifact"));
                refs.push(TaskArtifactRef {
                    label: label.to_string(),
                    path: PathBuf::from(path),
                    summary,
                    created_at: Utc::now(),
                });
            }
            for child in map.values() {
                collect_artifact_refs(child, path_key, summary_key, label, refs);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_artifact_refs(child, path_key, summary_key, label, refs);
            }
        }
        _ => {}
    }
}

pub(super) fn github_event_metadata(
    action: &str,
    target: &str,
    number: u64,
    summary: String,
    url: Option<String>,
    artifact: Option<PathBuf>,
) -> Value {
    let artifacts = artifact
        .map(|path| {
            json!([TaskArtifactRef {
                label: format!("github_{action}"),
                path,
                summary: summary.clone(),
                created_at: Utc::now(),
            }])
        })
        .unwrap_or_else(|| json!([]));
    json!({
        "task_updates": {
            "github_event": TaskGithubEvent {
                id: format!("gh_{}", &Uuid::new_v4().to_string()[..8]),
                action: action.to_string(),
                target: target.to_string(),
                number,
                summary,
                url,
                recorded_at: Utc::now(),
            },
            "artifacts": artifacts
        }
    })
}

pub(super) fn summarize(text: &str, limit: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= limit.saturating_sub(3) {
            out.push_str("...");
            return out;
        }
        if ch.is_control() && ch != '\n' && ch != '\t' {
            continue;
        }
        out.push(ch);
    }
    out
}

fn sanitize_filename(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "artifact".to_string()
    } else {
        out
    }
}
