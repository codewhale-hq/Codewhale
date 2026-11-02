//! Admitted GitHub operations. Core keeps processes, policy, private drafts and receipts.
use super::{cli, report, shape, validate_evidence};
use crate::tools::spec::{
    ToolContext, ToolError, ToolResult, optional_bool, optional_str, required_str, required_u64,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
pub enum Request {
    Issue {
        number: u64,
        comments: bool,
    },
    Pr {
        number: u64,
        diff: bool,
    },
    Comment {
        target: String,
        number: u64,
        body: String,
        dry: bool,
    },
    Close {
        target: String,
        number: u64,
        comment: Option<String>,
        dry: bool,
        allow_dirty: bool,
    },
    Draft {
        input: Value,
    },
    Read {
        session: String,
        id: String,
        operator: bool,
    },
}
impl Request {
    pub(super) fn capture(
        action: &str,
        input: &Value,
        context: &ToolContext,
    ) -> Result<Self, ToolError> {
        if serde_json::to_vec(input)
            .map_err(|_| ToolError::invalid_input("Invalid GitHub input"))?
            .len()
            > 1024 * 1024
        {
            return Err(ToolError::invalid_input("GitHub input exceeds 1 MiB"));
        }
        Ok(match action {
            "issue_context" => Self::Issue {
                number: required_u64(input, "number")?,
                comments: optional_bool(input, "include_comments", true)?,
            },
            "pr_context" => Self::Pr {
                number: required_u64(input, "number")?,
                diff: optional_bool(input, "include_diff", false)?,
            },
            "comment" => {
                validate_evidence(input, false)?;
                let target = required_str(input, "target")?;
                if !matches!(target, "issue" | "pr") {
                    return Err(ToolError::invalid_input(
                        "github comment: target must be issue or pr; nothing was posted",
                    ));
                }
                Self::Comment {
                    target: target.into(),
                    number: required_u64(input, "number")?,
                    body: required_str(input, "body")?.into(),
                    dry: optional_bool(input, "dry_run", false)?,
                }
            }
            "close_issue" | "close_pr" => {
                validate_evidence(input, true)?;
                Self::Close {
                    target: if action == "close_issue" {
                        "issue"
                    } else {
                        "pr"
                    }
                    .into(),
                    number: required_u64(input, "number")?,
                    comment: optional_str(input, "comment")?.map(str::to_string),
                    dry: optional_bool(input, "dry_run", false)?,
                    allow_dirty: optional_bool(input, "allow_dirty", false)?,
                }
            }
            "report_draft" => {
                report::validate_host_draft(input, context)?;
                Self::Draft {
                    input: input.clone(),
                }
            }
            "report_read" => {
                report::validate_host_read(input)?;
                if context
                    .session_objects
                    .as_ref()
                    .is_none_or(|snapshot| snapshot.session_id != context.state_namespace)
                {
                    return Err(ToolError::not_available(
                        "Issue reading requires the active Engine session context.",
                    ));
                }
                Self::Read {
                    session: context.state_namespace.clone(),
                    id: required_str(input, "report_id")?.into(),
                    operator: false,
                }
            }
            _ => return Err(ToolError::invalid_input("Unknown GitHub action")),
        })
    }
}

/// Private completion facts remain in the admitted Core job; no protocol projection.
#[derive(Clone, Copy, Default)]
enum Mutation {
    #[default]
    None,
    CommentPending,
    CommentCompleted,
    ClosePending {
        comment_completed: bool,
    },
    CloseCompleted {
        comment_completed: bool,
    },
}
#[derive(Default)]
pub struct RunState {
    pub result: Option<Result<Outcome, ToolError>>,
    mutation: Mutation,
}
impl RunState {
    fn mark(state: &Mutex<Self>, mutation: Mutation) {
        state.lock().expect("GitHub result lock").mutation = mutation;
    }
    fn refused_before_write(state: &Mutex<Self>, error: &ToolError) {
        // These failures occur before the contained process can be spawned. A
        // previous successful comment remains a completed effect in a Close job.
        if matches!(
            error,
            ToolError::PermissionDenied { .. }
                | ToolError::InvalidInput { .. }
                | ToolError::MissingField { .. }
                | ToolError::NotAvailable { .. }
                | ToolError::PathEscape { .. }
        ) {
            let mut state = state.lock().expect("GitHub result lock");
            state.mutation = match state.mutation {
                Mutation::ClosePending {
                    comment_completed: true,
                } => Mutation::CommentCompleted,
                Mutation::CommentPending
                | Mutation::ClosePending {
                    comment_completed: false,
                } => Mutation::None,
                other => other,
            };
        }
    }
}
impl Mutation {
    fn failure(self, error: ToolError) -> ToolError {
        let fact = match self {
            Self::None => return error,
            Self::CommentPending => {
                "GitHub comment outcome is uncertain; inspect the thread before retrying; do not replay automatically"
            }
            Self::CommentCompleted => {
                "GitHub comment completed; inspect the thread and do not replay the comment"
            }
            Self::ClosePending {
                comment_completed: false,
            } => {
                "GitHub close outcome is uncertain; inspect the thread before retrying; do not replay automatically"
            }
            Self::ClosePending {
                comment_completed: true,
            } => {
                "GitHub comment completed; close outcome is uncertain; inspect the thread and do not replay the comment or close automatically"
            }
            Self::CloseCompleted {
                comment_completed: false,
            } => "GitHub close completed; inspect the thread and do not replay the close",
            Self::CloseCompleted {
                comment_completed: true,
            } => {
                "GitHub comment and close completed; inspect the thread and do not replay either write"
            }
        };
        // Only Core-derived effect and failure class survive; never interpolate
        // arbitrary Host presentation errors or private process diagnostics.
        match error {
            ToolError::PermissionDenied { .. } => ToolError::permission_denied(format!(
                "{fact}; current authority refused completion"
            )),
            ToolError::Cancelled { .. } => {
                ToolError::cancelled(format!("{fact}; completion was cancelled"))
            }
            ToolError::NotAvailable { .. } => {
                ToolError::not_available(format!("{fact}; completion is unavailable"))
            }
            ToolError::Timeout { seconds } => ToolError::execution_failed_with_metadata(
                format!("{fact}; completion timed out"),
                json!({"github_effect": fact,"status":"timeout","timeout_seconds":seconds}),
            ),
            _ => ToolError::execution_failed_with_metadata(
                format!("{fact}; completion or presentation failed"),
                json!({"github_effect":fact}),
            ),
        }
    }
}

/// The private Core fault wins over a generic peer failure. A Host refusal,
/// forged metadata, timeout or cancellation cannot erase a confirmed write.
pub fn finish_run(
    state: &Mutex<RunState>,
    host: Result<ToolResult, ToolError>,
) -> Result<ToolResult, ToolError> {
    let (owned, mutation) = {
        let mut state = state.lock().expect("GitHub result lock");
        (state.result.take(), state.mutation)
    };
    match owned {
        Some(Err(error)) => Err(error),
        Some(Ok(outcome)) => host
            .and_then(|host| outcome.finish(host))
            .map_err(|error| mutation.failure(error)),
        None => Err(mutation.failure(host.err().unwrap_or_else(|| {
            ToolError::execution_failed("GitHub driver produced no owned receipt")
        }))),
    }
}

pub struct Outcome {
    pub projection: Value,
    pub metadata: Option<Value>,
}
impl Outcome {
    pub fn finish(self, mut result: ToolResult) -> Result<ToolResult, ToolError> {
        let expected_success = self.projection.get("dirty").and_then(Value::as_bool) != Some(true);
        if result.success != expected_success {
            return Err(ToolError::execution_failed(
                "GitHub presenter status does not match the owned outcome",
            ));
        }
        // The Host owns presentation, never task/report paths, IDs or timestamps.
        if result
            .metadata
            .as_ref()
            .is_some_and(|value| !value.is_null())
        {
            return Err(ToolError::execution_failed(
                "GitHub presenter returned unowned metadata",
            ));
        }
        result.metadata = self.metadata;
        Ok(result)
    }
}
fn prefix(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}
fn context_outcome(
    context: &ToolContext,
    action: &str,
    number: u64,
    mut raw: Value,
    diff: Option<String>,
) -> Result<Outcome, ToolError> {
    let body = raw.get("body").and_then(Value::as_str).map(str::to_string);
    let large = body
        .as_ref()
        .is_some_and(|body| body.len() > shape::BODY_ARTIFACT_THRESHOLD);
    let body_artifact = if large {
        shape::write_artifact_if_needed(
            context,
            if action == "issue_context" {
                "issue_body"
            } else {
                "pr_body"
            },
            body.as_deref().unwrap_or(""),
            shape::BODY_ARTIFACT_THRESHOLD,
        )
        .map_err(|_| ToolError::execution_failed("GitHub artifact storage is unavailable"))?
    } else {
        None
    };
    if large {
        raw["body"] = json!(prefix(body.as_deref().unwrap_or(""), 1200));
    }
    let diff_artifact = diff
        .as_deref()
        .map(|diff| {
            shape::write_artifact_if_needed(
                context,
                "pr_diff",
                diff,
                shape::DIFF_ARTIFACT_THRESHOLD,
            )
        })
        .transpose()
        .map_err(|_| ToolError::execution_failed("GitHub artifact storage is unavailable"))?
        .flatten();
    let mut artifacts = Vec::new();
    for (path, label, text) in [
        (
            body_artifact.as_ref(),
            if action == "issue_context" {
                "github_issue_body"
            } else {
                "github_pr_body"
            },
            body.as_deref(),
        ),
        (diff_artifact.as_ref(), "github_pr_diff", diff.as_deref()),
    ] {
        if let Some(path) = path {
            artifacts.push(crate::task_manager::TaskArtifactRef {
                label: label.into(),
                path: path.clone(),
                summary: shape::summarize(text.unwrap_or(""), 900),
                created_at: chrono::Utc::now(),
            });
        }
    }
    Ok(Outcome {
        projection: json!({"action":action,"number":number.to_string(),"raw":raw,"large_body":large,"body_artifact":body_artifact,"diff":diff.map(|diff|prefix(&diff,900)),"diff_artifact":diff_artifact}),
        metadata: (!artifacts.is_empty()).then(|| json!({"task_updates":{"artifacts":artifacts}})),
    })
}
fn report_outcome(report: report::Report, operator: bool) -> Result<Outcome, ToolError> {
    Ok(Outcome {
        projection: json!({"action":if operator{"report_review"}else{"report_read"},"report":report.host_snapshot()}),
        metadata: if operator {
            None
        } else {
            Some(report.host_metadata()?)
        },
    })
}

pub async fn run<F, Fut>(
    request: Request,
    context: Option<ToolContext>,
    cancel: CancellationToken,
    check: F,
    state: Arc<Mutex<RunState>>,
) -> Result<Outcome, ToolError>
where
    F: Fn() -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<(), ToolError>> + Send,
{
    if cancel.is_cancelled() {
        return Err(ToolError::not_available("GitHub operation cancelled"));
    }
    check().await?;
    match request {
        Request::Issue { number, comments } => {
            let context = context
                .as_ref()
                .ok_or_else(|| ToolError::not_available("GitHub caller missing"))?;
            cli::host_git(context, &["rev-parse", "--is-inside-work-tree"], &cancel).await?;
            let fields = if comments {
                "number,title,state,author,labels,assignees,milestone,body,comments,url,createdAt,updatedAt"
            } else {
                "number,title,state,author,labels,assignees,milestone,body,url,createdAt,updatedAt"
            };
            let target = cli::host_target(context, &cancel).await?;
            check().await?;
            let raw = cli::host_gh(
                context,
                &target,
                &["issue", "view", &number.to_string(), "--json", fields],
                None,
                &cancel,
            )
            .await?;
            check().await?;
            context_outcome(
                context,
                "issue_context",
                number,
                serde_json::from_str(&raw)
                    .map_err(|_| ToolError::execution_failed("Invalid GitHub issue JSON"))?,
                None,
            )
        }
        Request::Pr { number, diff } => {
            let context = context
                .as_ref()
                .ok_or_else(|| ToolError::not_available("GitHub caller missing"))?;
            cli::host_git(context, &["rev-parse", "--is-inside-work-tree"], &cancel).await?;
            let target = cli::host_target(context, &cancel).await?;
            check().await?;
            let raw=cli::host_gh(context,&target,&["pr","view",&number.to_string(),"--json","number,title,state,author,body,comments,reviews,reviewDecision,statusCheckRollup,baseRefName,headRefName,headRefOid,baseRefOid,files,url,createdAt,updatedAt"],None,&cancel).await?;
            let diff = if diff {
                Some(
                    cli::host_gh(
                        context,
                        &target,
                        &["pr", "diff", &number.to_string(), "--patch"],
                        None,
                        &cancel,
                    )
                    .await?,
                )
            } else {
                None
            };
            check().await?;
            context_outcome(
                context,
                "pr_context",
                number,
                serde_json::from_str(&raw)
                    .map_err(|_| ToolError::execution_failed("Invalid GitHub PR JSON"))?,
                diff,
            )
        }
        Request::Comment {
            target,
            number,
            body,
            dry,
        } => {
            let context = context
                .as_ref()
                .ok_or_else(|| ToolError::not_available("GitHub caller missing"))?;
            if !dry {
                let selection = cli::host_target(context, &cancel).await?;
                check().await?;
                RunState::mark(&state, Mutation::CommentPending);
                cli::host_gh(
                    context,
                    &selection,
                    &[&target, "comment", &number.to_string(), "--body-file", "-"],
                    Some(body.as_bytes()),
                    &cancel,
                )
                .await
                .map_err(|error| {
                    RunState::refused_before_write(&state, &error);
                    write_error(
                        error,
                        "GitHub comment outcome is uncertain; inspect the thread before retrying",
                    )
                })?;
                RunState::mark(&state, Mutation::CommentCompleted);
            }
            check().await.map_err(|error| {
                if dry {
                    error
                } else {
                    Mutation::CommentCompleted.failure(error)
                }
            })?;
            let metadata = if dry {
                None
            } else {
                Some(shape::github_event_metadata(
                    "comment",
                    &target,
                    number,
                    shape::summarize(&body, 240),
                    None,
                    shape::write_artifact_if_needed(
                        context,
                        "github_comment",
                        &body,
                        shape::BODY_ARTIFACT_THRESHOLD,
                    ).map_err(|_|ToolError::execution_failed("GitHub comment completed, but its Core artifact receipt could not be saved; do not replay the comment"))?,
                ))
            };
            Ok(Outcome {
                projection: json!({"action":"comment","number":number.to_string(),"target":target,"dry_run":dry}),
                metadata,
            })
        }
        Request::Close {
            target,
            number,
            comment,
            dry,
            allow_dirty,
        } => {
            let context = context
                .as_ref()
                .ok_or_else(|| ToolError::not_available("GitHub caller missing"))?;
            if !allow_dirty {
                let status = cli::host_git(context, &["status", "--porcelain"], &cancel).await?;
                if !status.trim().is_empty() {
                    return Ok(Outcome {
                        projection: json!({"action":if target=="issue"{"close_issue"}else{"close_pr"},"number":number.to_string(),"target":target,"dirty":true}),
                        metadata: Some(json!({"dirty_status":status})),
                    });
                }
            }
            if !dry {
                let selection = cli::host_target(context, &cancel).await?;
                check().await?;
                if let Some(comment) = comment.as_ref() {
                    RunState::mark(&state, Mutation::CommentPending);
                    cli::host_gh(context,&selection,&[&target,"comment",&number.to_string(),"--body-file","-"],Some(comment.as_bytes()),&cancel).await.map_err(|error| {
                        RunState::refused_before_write(&state, &error);
                        write_error(error,"GitHub comment outcome is uncertain; inspect the thread before retrying; close was not attempted")
                    })?;
                    RunState::mark(&state, Mutation::CommentCompleted);
                    check().await.map_err(|error| match error {
                        ToolError::PermissionDenied { .. } => ToolError::permission_denied("GitHub comment completed, but caller authority changed; close was not attempted; do not replay the comment"),
                        ToolError::Cancelled { .. } => ToolError::cancelled("GitHub comment completed, but completion was cancelled; close was not attempted; do not replay the comment"),
                        _ => ToolError::not_available("GitHub comment completed, but caller authority changed; close was not attempted; do not replay the comment"),
                    })?;
                }
                let number_string = number.to_string();
                let args = if target == "issue" {
                    vec![
                        "issue",
                        "close",
                        number_string.as_str(),
                        "--reason",
                        "completed",
                    ]
                } else {
                    vec!["pr", "close", number_string.as_str()]
                };
                RunState::mark(
                    &state,
                    Mutation::ClosePending {
                        comment_completed: comment.is_some(),
                    },
                );
                if let Err(error) = cli::host_gh(context, &selection, &args, None, &cancel).await {
                    RunState::refused_before_write(&state, &error);
                    return Err(if comment.is_some() {
                        match error {
                            ToolError::PermissionDenied { .. } => ToolError::permission_denied(
                                "GitHub comment completed; close was blocked by current network authority; do not replay the comment",
                            ),
                            ToolError::NotAvailable { .. } => ToolError::not_available(
                                "GitHub comment completed; close was unavailable; do not replay the comment",
                            ),
                            ToolError::Cancelled { .. } => ToolError::cancelled(
                                "GitHub comment completed; close was cancelled and its outcome is uncertain; inspect before retrying and do not replay the comment",
                            ),
                            _ => ToolError::execution_failed(
                                "GitHub comment completed; close outcome is uncertain; inspect before retrying and do not replay the comment",
                            ),
                        }
                    } else {
                        write_error(
                            error,
                            "GitHub close outcome is uncertain; inspect the thread before retrying",
                        )
                    });
                }
                RunState::mark(
                    &state,
                    Mutation::CloseCompleted {
                        comment_completed: comment.is_some(),
                    },
                );
            }
            check().await.map_err(|error| {
                if dry {
                    error
                } else {
                    Mutation::CloseCompleted {
                        comment_completed: comment.is_some(),
                    }
                    .failure(error)
                }
            })?;
            let metadata = if dry {
                None
            } else {
                Some(shape::github_event_metadata(
                    "close",
                    &target,
                    number,
                    format!(
                        "{} closed as completed with structured evidence",
                        if target == "issue" { "Issue" } else { "PR" }
                    ),
                    None,
                    comment.as_deref().map(|comment|shape::write_artifact_if_needed(context,"github_close_comment",comment,shape::BODY_ARTIFACT_THRESHOLD)).transpose().map_err(|_|ToolError::execution_failed("GitHub close completed, but its Core artifact receipt could not be saved; inspect the thread and do not replay the write"))?.flatten(),
                ))
            };
            Ok(Outcome {
                projection: json!({"action":if target=="issue"{"close_issue"}else{"close_pr"},"number":number.to_string(),"target":target,"dry_run":dry}),
                metadata,
            })
        }
        Request::Draft { input } => {
            let context =
                context.ok_or_else(|| ToolError::not_available("Report caller missing"))?;
            let report = report_worker(check.clone(), move || {
                report::create_host_draft(input, &context)
            })
            .await?;
            check().await?;
            report_outcome(report, false)
        }
        Request::Read {
            session,
            id,
            operator,
        } => {
            let report = report_worker(check.clone(), move || report::load(&session, &id)).await?;
            check().await?;
            report_outcome(report, operator)
        }
    }
}

/// The admitted job/permit remains owned by the actual blocking worker after its
/// async waiter is cancelled. Aborting a waiter cannot release quota early.
pub async fn report_worker<A, T>(
    admission: A,
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError>
where
    A: Send + 'static,
    T: Send + 'static,
{
    #[cfg(any(test, feature = "test-support"))]
    let scope = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        let _admission = admission;
        #[cfg(any(test, feature = "test-support"))]
        let _scope = crate::test_support::join_env_scope(scope);
        work()
    })
    .await
    .map_err(|_| ToolError::execution_failed("Report worker lost"))?
}

fn write_error(error: ToolError, uncertain: &'static str) -> ToolError {
    match error {
        ToolError::PermissionDenied { .. }
        | ToolError::InvalidInput { .. }
        | ToolError::MissingField { .. }
        | ToolError::NotAvailable { .. }
        | ToolError::PathEscape { .. } => error,
        ToolError::Cancelled { .. } => ToolError::cancelled(uncertain),
        _ => ToolError::execution_failed(uncertain),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn capture_rejects_invalid_effects_before_admission() {
        let temp = tempfile::tempdir().unwrap();
        let context = ToolContext::new(temp.path());
        let evidence =
            json!({"files_changed":["src/lib.rs"],"tests_run":["fixture"],"final_status":"green"});
        for input in [
            json!({"action":"comment","number":1,"target":"issue","body":"text","dry_run":"true","evidence":evidence}),
            json!({"action":"comment","number":1,"target":"other","body":"text","evidence":evidence}),
            json!({"action":"close_issue","number":1,"evidence":evidence,"acceptance_criteria":[]}),
            json!({"action":"report_read","report_id":"../../other"}),
            json!({"action":"issue_context","number":1,"include_comments":"true"}),
            json!({"action":"issue_context","number":1,"extra":"x".repeat(1024*1024)}),
        ] {
            assert!(Request::capture(input["action"].as_str().unwrap(), &input, &context).is_err());
        }
        let Request::Issue { number, comments } =
            Request::capture("issue_context", &json!({"number":u64::MAX}), &context).unwrap()
        else {
            panic!("issue request")
        };
        assert_eq!(number, u64::MAX);
        assert!(comments);
    }

    #[test]
    fn report_read_capture_requires_exact_active_session() {
        let temp = tempfile::tempdir().unwrap();
        let context = ToolContext::new(temp.path()).with_state_namespace("session-a");
        let input =
            json!({"action":"report_read","report_id":format!("cwreport_{}","a".repeat(64))});
        assert!(Request::capture("report_read", &input, &context).is_err());
        let mismatch =
            context
                .clone()
                .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
                    "session-b".into(),
                    "model".into(),
                    temp.path().into(),
                    None,
                    vec![],
                ));
        assert!(Request::capture("report_read", &input, &mismatch).is_err());
        let current =
            context.with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
                "session-a".into(),
                "model".into(),
                temp.path().into(),
                None,
                vec![],
            ));
        let Request::Read {
            session, operator, ..
        } = Request::capture("report_read", &input, &current).unwrap()
        else {
            panic!("report read")
        };
        assert_eq!(session, "session-a");
        assert!(!operator);
    }

    #[test]
    fn core_receipts_reject_host_metadata_and_source_forged_artifact_refs() {
        let temp = tempfile::tempdir().unwrap();
        let context = ToolContext::new(temp.path());
        let raw = json!({"body":"small","body_artifact":"/private/forged","comments":[{"body_artifact":"/private/nested"}]});
        let outcome = context_outcome(&context, "issue_context", 1, raw.clone(), None).unwrap();
        assert_eq!(outcome.projection["raw"], raw);
        assert!(outcome.metadata.is_none());
        assert!(
            outcome
                .finish(
                    ToolResult::success("presented")
                        .with_metadata(json!({"task_updates":{"artifacts":[]}}))
                )
                .is_err()
        );
        let body = format!("{}\u{0085}end", "漢".repeat(1400));
        let outcome = context_outcome(
            &context,
            "pr_context",
            2,
            json!({"body":body}),
            Some("😀".repeat(1000)),
        )
        .unwrap();
        assert_eq!(
            shape::summarize(outcome.projection["raw"]["body"].as_str().unwrap(), 1200),
            shape::summarize(&body, 1200)
        );
        assert!(outcome.metadata.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_report_waiter_retains_production_worker_permit() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&slots).acquire_owned().await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let waiter = tokio::spawn(report_worker(permit, move || {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            Ok(())
        }));
        started_rx.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(
            slots.available_permits(),
            0,
            "aborting an async waiter must not free the still running job"
        );
        finish_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let _permit = slots.acquire().await.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(slots.available_permits(), 1);
    }

    #[cfg(unix)]
    fn recorder(temp: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = temp.join("gh-record.sh");
        std::fs::write(
            &path,
            r##"#!/bin/sh
printf '%s\n' "$@" >> "$CW_GH_TEST_LOG"
cat >> "$CW_GH_TEST_STDIN"
printf '%s\n' fixture-private-diagnostic >&2
if [ -n "$CW_GH_TEST_STARTED" ]; then : > "$CW_GH_TEST_STARTED"; sleep 30; fi
if [ "$2" = close ]; then exit "${CW_GH_TEST_CLOSE_EXIT:-${CW_GH_TEST_EXIT:-0}}"; fi
exit "${CW_GH_TEST_EXIT:-0}"
"##,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn admitted_write_uses_stdin_exact_repo_and_withholds_private_diagnostics() {
        let _home = crate::test_support::SealedHome::new();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args");
        let input = temp.path().join("stdin");
        let binary = recorder(temp.path());
        let _bin = crate::test_support::EnvVarGuard::set("CODEWHALE_GH_BIN", &binary);
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "approved.example/owner/name");
        let _log = crate::test_support::EnvVarGuard::set("CW_GH_TEST_LOG", &log);
        let _input = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STDIN", &input);
        let context = ToolContext::new(temp.path());
        let body = "private body 漢字";
        let outcome = run(
            Request::Comment {
                target: "pr".into(),
                number: u64::MAX,
                body: body.into(),
                dry: false,
            },
            Some(context.clone()),
            CancellationToken::new(),
            || async { Ok(()) },
            Arc::default(),
        )
        .await
        .unwrap();
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(args.contains("--body-file\n-\n"));
        assert!(args.contains("--repo\napproved.example/owner/name\n"));
        assert!(!args.contains(body));
        assert_eq!(std::fs::read_to_string(&input).unwrap(), body);
        assert!(outcome.metadata.is_some());
        let mut broken = context.clone();
        let blocked = temp.path().join("private-receipt-location");
        std::fs::write(&blocked, "not a directory").unwrap();
        broken.runtime.active_task_id = Some("task".into());
        broken.runtime.task_data_dir = Some(blocked);
        let error = run(
            Request::Comment {
                target: "pr".into(),
                number: 1,
                body: "x".repeat(4001),
                dry: false,
            },
            Some(broken),
            CancellationToken::new(),
            || async { Ok(()) },
            Arc::default(),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("comment completed"));
        assert!(error.contains("receipt could not be saved"));
        assert!(!error.contains("private-receipt-location"));
        let _exit = crate::test_support::EnvVarGuard::set("CW_GH_TEST_EXIT", "7");
        let error = run(
            Request::Comment {
                target: "issue".into(),
                number: 1,
                body: body.into(),
                dry: false,
            },
            Some(context),
            CancellationToken::new(),
            || async { Ok(()) },
            Arc::default(),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("uncertain"));
        assert!(!error.contains(body));
        assert!(!error.contains("fixture-private-diagnostic"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn changed_authority_after_comment_refuses_close_without_replaying_comment() {
        let _home = crate::test_support::SealedHome::new();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args");
        let input = temp.path().join("stdin");
        let _bin = crate::test_support::EnvVarGuard::set("CODEWHALE_GH_BIN", recorder(temp.path()));
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "owner/name");
        let _log = crate::test_support::EnvVarGuard::set("CW_GH_TEST_LOG", &log);
        let _input = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STDIN", &input);
        let checks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&checks);
        let error = run(
            Request::Close {
                target: "issue".into(),
                number: 1,
                comment: Some("confirmed evidence".into()),
                dry: false,
                allow_dirty: true,
            },
            Some(ToolContext::new(temp.path())),
            CancellationToken::new(),
            move || {
                let revoked = counter.fetch_add(1, Ordering::SeqCst) >= 2;
                async move {
                    if revoked {
                        Err(ToolError::not_available("revoked"))
                    } else {
                        Ok(())
                    }
                }
            },
            Arc::default(),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("comment completed"));
        assert!(error.contains("close was not attempted"));
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(args.contains("comment"));
        assert!(!args.contains("close"));
        assert_eq!(
            std::fs::read_to_string(input).unwrap(),
            "confirmed evidence"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn deny_or_prompt_policy_and_dry_run_never_invoke_gh() {
        use codewhale_net::network_policy::{DecisionToml, NetworkPolicy, NetworkPolicyDecider};
        let _home = crate::test_support::SealedHome::new();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args");
        let input = temp.path().join("stdin");
        let _bin = crate::test_support::EnvVarGuard::set("CODEWHALE_GH_BIN", recorder(temp.path()));
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "owner/name");
        let _log = crate::test_support::EnvVarGuard::set("CW_GH_TEST_LOG", &log);
        let _input = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STDIN", &input);
        for default in [DecisionToml::Deny, DecisionToml::Prompt] {
            let context =
                ToolContext::new(temp.path()).with_network_policy(NetworkPolicyDecider::new(
                    NetworkPolicy {
                        default,
                        ..Default::default()
                    },
                    None,
                ));
            let result = run(
                Request::Comment {
                    target: "issue".into(),
                    number: 1,
                    body: "no".into(),
                    dry: false,
                },
                Some(context),
                CancellationToken::new(),
                || async { Ok(()) },
                Arc::default(),
            )
            .await;
            assert!(matches!(result, Err(ToolError::PermissionDenied { .. })));
        }
        assert!(!log.exists());
        run(
            Request::Close {
                target: "pr".into(),
                number: 1,
                comment: None,
                dry: true,
                allow_dirty: true,
            },
            Some(ToolContext::new(temp.path())),
            CancellationToken::new(),
            || async { Ok(()) },
            Arc::default(),
        )
        .await
        .unwrap();
        assert!(!log.exists());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            run(
                Request::Comment {
                    target: "issue".into(),
                    number: 1,
                    body: "no".into(),
                    dry: false
                },
                Some(ToolContext::new(temp.path())),
                cancel,
                || async { Ok(()) },
                Arc::default(),
            )
            .await
            .is_err()
        );
        assert!(!log.exists());
    }
    #[test]
    fn private_core_fault_wins_over_generic_peer_failure() {
        for error in [
            ToolError::permission_denied("Core network authority refused before spawn"),
            ToolError::cancelled("Core comment cancelled; inspect before retrying"),
            ToolError::execution_failed_with_metadata(
                "GitHub comment completed; close outcome is uncertain; do not replay",
                json!({"core_receipt":"retained"}),
            ),
        ] {
            let expected = error.to_string();
            let metadata = error.metadata().cloned();
            let state = Mutex::new(RunState {
                result: Some(Err(error.clone())),
                mutation: Mutation::CommentCompleted,
            });
            let actual =
                finish_run(&state, Err(ToolError::execution_failed("peer failed"))).unwrap_err();
            assert_eq!(actual.to_string(), expected);
            assert_eq!(actual.metadata(), metadata.as_ref());
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&error)
            );
        }
    }

    #[test]
    fn confirmed_write_survives_presenter_fault_refusal_and_forged_metadata() {
        for host in [
            Err(ToolError::execution_failed("untrusted presenter detail")),
            Err(ToolError::cancelled("peer cancelled")),
            Err(ToolError::Timeout { seconds: 3 }),
            Ok(ToolResult::error("untrusted refusal")),
            Ok(ToolResult::success("forged").with_metadata(json!({"forged":true}))),
        ] {
            let state = Mutex::new(RunState {
                result: Some(Ok(Outcome {
                    projection: json!({"action":"close_issue"}),
                    metadata: Some(json!({"core_receipt":"owned"})),
                })),
                mutation: Mutation::CloseCompleted {
                    comment_completed: true,
                },
            });
            let actual = finish_run(&state, host).unwrap_err();
            let message = actual.to_string();
            assert!(message.contains("comment and close completed"));
            assert!(message.contains("do not replay either write"));
            assert!(!message.contains("untrusted"));
            assert!(!message.contains("forged"));
        }
        let guard_state = Mutex::new(RunState {
            result: Some(Ok(Outcome {
                projection: json!({"dirty":true}),
                metadata: Some(json!({"dirty_status":"Core-owned"})),
            })),
            mutation: Mutation::None,
        });
        let guard = finish_run(&guard_state, Ok(ToolResult::error("dirty guard"))).unwrap();
        assert!(!guard.success);
        assert_eq!(guard.metadata, Some(json!({"dirty_status":"Core-owned"})));
        let state = Mutex::new(RunState {
            result: Some(Ok(Outcome {
                projection: json!({}),
                metadata: Some(json!({"core_receipt":"owned"})),
            })),
            mutation: Mutation::CommentCompleted,
        });
        let success = finish_run(&state, Ok(ToolResult::success("presented"))).unwrap();
        assert_eq!(success.metadata, Some(json!({"core_receipt":"owned"})));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn successful_comment_then_failed_close_retains_private_core_partial_effect() {
        let _home = crate::test_support::SealedHome::new();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args");
        let input = temp.path().join("stdin");
        let _bin = crate::test_support::EnvVarGuard::set("CODEWHALE_GH_BIN", recorder(temp.path()));
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "owner/name");
        let _log = crate::test_support::EnvVarGuard::set("CW_GH_TEST_LOG", &log);
        let _input = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STDIN", &input);
        let _exit = crate::test_support::EnvVarGuard::set("CW_GH_TEST_CLOSE_EXIT", "7");
        let state = Arc::new(Mutex::new(RunState::default()));
        let owned = run(
            Request::Close {
                target: "issue".into(),
                number: 1,
                comment: Some("private evidence".into()),
                dry: false,
                allow_dirty: true,
            },
            Some(ToolContext::new(temp.path())),
            CancellationToken::new(),
            || async { Ok(()) },
            Arc::clone(&state),
        )
        .await;
        state.lock().unwrap().result = Some(owned);
        let actual = finish_run(
            &state,
            Err(ToolError::execution_failed("generic Builtin refusal")),
        )
        .unwrap_err();
        assert!(matches!(actual, ToolError::ExecutionFailed { .. }));
        let message = actual.to_string();
        assert!(message.contains("comment completed"));
        assert!(message.contains("close outcome is uncertain"));
        assert!(message.contains("do not replay"));
        assert!(!message.contains("private evidence"));
        assert!(!message.contains("fixture-private-diagnostic"));
        let args = std::fs::read_to_string(log).unwrap();
        assert_eq!(args.lines().filter(|line| *line == "comment").count(), 1);
        assert_eq!(args.lines().filter(|line| *line == "close").count(), 1);
        assert_eq!(std::fs::read_to_string(input).unwrap(), "private evidence");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_close_late_authority_failure_retains_completed_effect_and_class() {
        let _home = crate::test_support::SealedHome::new();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args");
        let input = temp.path().join("stdin");
        let _bin = crate::test_support::EnvVarGuard::set("CODEWHALE_GH_BIN", recorder(temp.path()));
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "owner/name");
        let _log = crate::test_support::EnvVarGuard::set("CW_GH_TEST_LOG", &log);
        let _input = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STDIN", &input);
        let checks = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(Mutex::new(RunState::default()));
        let owned = run(
            Request::Close {
                target: "pr".into(),
                number: 2,
                comment: None,
                dry: false,
                allow_dirty: true,
            },
            Some(ToolContext::new(temp.path())),
            CancellationToken::new(),
            move || {
                let late = checks.fetch_add(1, Ordering::SeqCst) >= 2;
                async move {
                    if late {
                        Err(ToolError::permission_denied("Core authority changed"))
                    } else {
                        Ok(())
                    }
                }
            },
            Arc::clone(&state),
        )
        .await;
        state.lock().unwrap().result = Some(owned);
        let actual = finish_run(
            &state,
            Err(ToolError::execution_failed("generic peer failure")),
        )
        .unwrap_err();
        assert!(matches!(actual, ToolError::PermissionDenied { .. }));
        assert!(actual.to_string().contains("close completed"));
        assert!(actual.to_string().contains("do not replay the close"));
        let args = std::fs::read_to_string(log).unwrap();
        assert_eq!(args.lines().filter(|line| *line == "close").count(), 1);
        assert!(!args.lines().any(|line| line == "comment"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_write_is_retained_while_post_write_authority_await_is_pending() {
        let _home = crate::test_support::SealedHome::new();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args");
        let input = temp.path().join("stdin");
        let _bin = crate::test_support::EnvVarGuard::set("CODEWHALE_GH_BIN", recorder(temp.path()));
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "owner/name");
        let _log = crate::test_support::EnvVarGuard::set("CW_GH_TEST_LOG", &log);
        let _input = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STDIN", &input);
        let checks = Arc::new(AtomicUsize::new(0));
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&reached);
        let resume = Arc::clone(&release);
        let state = Arc::new(Mutex::new(RunState::default()));
        let work_state = Arc::clone(&state);
        let context = ToolContext::new(temp.path());
        let worker = tokio::spawn(run(
            Request::Close {
                target: "issue".into(),
                number: 3,
                comment: None,
                dry: false,
                allow_dirty: true,
            },
            Some(context),
            CancellationToken::new(),
            move || {
                let late = checks.fetch_add(1, Ordering::SeqCst) >= 2;
                let reached = Arc::clone(&started);
                let release = Arc::clone(&resume);
                async move {
                    if late {
                        reached.notify_one();
                        release.notified().await;
                    }
                    Ok(())
                }
            },
            work_state,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), reached.notified())
            .await
            .unwrap();
        assert!(state.lock().unwrap().result.is_none());
        let error = finish_run(
            &state,
            Err(ToolError::cancelled(
                "peer cancelled before final Core check",
            )),
        )
        .unwrap_err();
        assert!(matches!(error, ToolError::Cancelled { .. }));
        assert!(error.to_string().contains("close completed"));
        assert!(error.to_string().contains("do not replay the close"));
        release.notify_one();
        worker.await.unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(log)
                .unwrap()
                .lines()
                .filter(|line| *line == "close")
                .count(),
            1
        );
    }
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_during_write_retains_uncertain_effect_and_cancelled_type() {
        let _home = crate::test_support::SealedHome::new();
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("args");
        let input = temp.path().join("stdin");
        let started = temp.path().join("started");
        let _bin = crate::test_support::EnvVarGuard::set("CODEWHALE_GH_BIN", recorder(temp.path()));
        let _repo = crate::test_support::EnvVarGuard::set("GH_REPO", "owner/name");
        let _log = crate::test_support::EnvVarGuard::set("CW_GH_TEST_LOG", &log);
        let _input = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STDIN", &input);
        let _started = crate::test_support::EnvVarGuard::set("CW_GH_TEST_STARTED", &started);
        let state = Arc::new(Mutex::new(RunState::default()));
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(run(
            Request::Comment {
                target: "issue".into(),
                number: 4,
                body: "private payload".into(),
                dry: false,
            },
            Some(ToolContext::new(temp.path())),
            cancel.clone(),
            || async { Ok(()) },
            Arc::clone(&state),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let pending =
            finish_run(&state, Err(ToolError::cancelled("Host waiter stopped"))).unwrap_err();
        assert!(matches!(pending, ToolError::Cancelled { .. }));
        assert!(pending.to_string().contains("comment outcome is uncertain"));
        assert!(!pending.to_string().contains("comment completed"));
        cancel.cancel();
        let owned = tokio::time::timeout(std::time::Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(&owned, Err(ToolError::Cancelled { .. })));
        state.lock().unwrap().result = Some(owned);
        let actual = finish_run(
            &state,
            Err(ToolError::execution_failed("generic peer fault")),
        )
        .unwrap_err();
        assert!(matches!(actual, ToolError::Cancelled { .. }));
        assert!(actual.to_string().contains("uncertain"));
        assert!(!actual.to_string().contains("private payload"));
        assert_eq!(
            std::fs::read_to_string(log)
                .unwrap()
                .lines()
                .filter(|line| *line == "comment")
                .count(),
            1
        );
    }
}
