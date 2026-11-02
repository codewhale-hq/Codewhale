//! Truthful auto-resume of the last valid session (#2934).
//!
//! Plain `codewhale` has always started fresh. Issue #2934 asked for the
//! previous conversation to come back automatically; the review that followed
//! (see the 2026-07-16 comment) landed on a firm constraint: **no surprise
//! resume**. So this is an explicit, opt-in setting, and when it is on it
//! still refuses to guess.
//!
//! The rules, in the order they are applied:
//!
//! 1. An explicit `--resume <id>` or `--continue` always wins. Auto-resume
//!    never overrides, reorders, or second-guesses what the user typed.
//! 2. `--fresh` always wins the other way.
//! 3. With the setting off (the default), the decision is
//!    [`AutoResumeDecision::Disabled`] and startup is unchanged.
//! 4. With the setting on, the candidate is the newest non-archived session
//!    recorded against *this* workspace. If there is none, startup is fresh
//!    and says so.
//! 5. The candidate is then **verified**: it must load, and its recorded
//!    workspace must still match the workspace we are launching in. A session
//!    that fails to load (truncated, hand-edited, half-written) yields
//!    [`AutoResumeDecision::Unreadable`] — fresh start, with the reason
//!    surfaced rather than swallowed.
//!
//! Rule 5's workspace re-check is the one that matters most. The candidate
//! already came from a workspace-scoped query, but the metadata read there is
//! a 64 KB prefix extraction; re-checking against the fully loaded session is
//! what makes "we will never silently resume a different workspace" a property
//! of the code rather than a claim in a doc.
//!
//! Nothing here contacts a provider or the network. Deciding what to resume,
//! and resuming it, are pure disk operations.

use std::path::Path;

use crate::session_manager::{SessionManager, workspace_scope_matches};

/// How far down the candidate list auto-resume will walk before giving up.
///
/// Bounded on purpose: a sessions directory full of damaged files must not
/// turn startup into a long scan, and the receipt's skipped count stays a
/// small, honest number rather than an unbounded tally.
pub const MAX_AUTO_RESUME_CANDIDATES: usize = 10;

/// Why startup is (or is not) attaching to a previous session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoResumeDecision {
    /// The user asked for a specific session, or for `--continue`. Auto-resume
    /// stands down and the explicit request is carried through untouched.
    ExplicitRequest { session_id: String },
    /// `--fresh` was passed. Start clean, no lookup performed.
    ForcedFresh,
    /// The setting is off. This is the shipped default.
    Disabled,
    /// A verified session for this workspace. Safe to resume.
    ///
    /// `skipped_unreadable` counts candidates newer than this one that failed
    /// verification. It is reported rather than swallowed: silently landing on
    /// an older session while newer ones rot is exactly the kind of quiet
    /// degradation a user should be told about.
    Resume {
        session_id: String,
        title: String,
        skipped_unreadable: usize,
    },
    /// Auto-resume is on but this workspace has no eligible session yet.
    /// Start fresh; this is a normal first-run state, not an error.
    NoSession,
    /// Every candidate for this workspace failed verification. Start fresh and
    /// name the newest failure plus how many were tried.
    Unreadable {
        session_id: String,
        reason: String,
        skipped_unreadable: usize,
    },
    /// The newest candidate belongs to a different workspace than the one we
    /// are launching in. Start fresh — resuming would silently move the user's
    /// conversation to another project.
    WorkspaceMismatch { session_id: String },
}

impl AutoResumeDecision {
    /// The session id startup should actually load, if any.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::ExplicitRequest { session_id } | Self::Resume { session_id, .. } => {
                Some(session_id.as_str())
            }
            Self::ForcedFresh
            | Self::Disabled
            | Self::NoSession
            | Self::Unreadable { .. }
            | Self::WorkspaceMismatch { .. } => None,
        }
    }

    /// A short, user-facing receipt, or `None` when there is nothing worth
    /// saying. Silence is correct for `Disabled` (the default posture) and for
    /// `ExplicitRequest` (the existing resume path prints its own receipt).
    #[must_use]
    pub fn status_message(&self) -> Option<String> {
        match self {
            Self::ExplicitRequest { .. } | Self::ForcedFresh | Self::Disabled => None,
            Self::Resume {
                title,
                skipped_unreadable: 0,
                ..
            } => Some(format!("Auto-resumed session: {title}")),
            Self::Resume {
                title,
                skipped_unreadable,
                ..
            } => Some(format!(
                "Auto-resumed session: {title} (skipped {skipped_unreadable} unreadable {} above it)",
                plural_sessions(*skipped_unreadable)
            )),
            Self::NoSession => {
                Some("Auto-resume: no previous session for this workspace — starting fresh".into())
            }
            Self::Unreadable {
                session_id,
                reason,
                skipped_unreadable,
            } => Some(format!(
                "Auto-resume found no readable session ({skipped_unreadable} unreadable {}); newest was {}: {reason} — starting fresh",
                plural_sessions(*skipped_unreadable),
                crate::session_manager::truncate_id(session_id)
            )),
            Self::WorkspaceMismatch { session_id } => Some(format!(
                "Auto-resume skipped session {}: it belongs to a different workspace — starting fresh",
                crate::session_manager::truncate_id(session_id)
            )),
        }
    }

    /// True when startup will begin with an empty transcript.
    ///
    /// Test-only: startup itself branches on [`Self::session_id`], and this is
    /// the same fact spelled the way the assertions read. Gating it keeps the
    /// shipped surface to what the shipped code calls.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn starts_fresh(&self) -> bool {
        self.session_id().is_none()
    }
}

/// What the CLI asked for, independent of the persisted setting.
#[derive(Debug, Clone, Default)]
pub struct ResumeRequest {
    /// `--resume <id>` (or a `--continue`-resolved id).
    pub explicit_session_id: Option<String>,
    /// `--fresh`.
    pub force_fresh: bool,
}

/// Decide what, if anything, to auto-resume.
///
/// `manager` is borrowed rather than opened here so tests and the `main`
/// startup path share one session directory, and so a caller that already has
/// a manager does not pay for a second directory resolution.
pub fn decide_auto_resume(
    enabled: bool,
    request: &ResumeRequest,
    workspace: &Path,
    manager: &SessionManager,
) -> AutoResumeDecision {
    if let Some(session_id) = request
        .explicit_session_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return AutoResumeDecision::ExplicitRequest {
            session_id: session_id.to_string(),
        };
    }
    if request.force_fresh {
        return AutoResumeDecision::ForcedFresh;
    }
    if !enabled {
        return AutoResumeDecision::Disabled;
    }

    let listed = match manager.list_sessions() {
        Ok(listed) => listed,
        // A sessions directory we cannot enumerate is not a reason to fail
        // startup; it is a reason not to resume.
        Err(err) => {
            return AutoResumeDecision::Unreadable {
                session_id: String::new(),
                reason: format!("sessions directory unreadable ({err})"),
                skipped_unreadable: 0,
            };
        }
    };

    // Candidates newest-first, scoped through the *same* matcher the picker,
    // the rail, and the API use. `list_sessions` already sorts by `updated_at`
    // descending.
    let candidates = candidate_ids(&listed, workspace);
    if candidates.is_empty() {
        return AutoResumeDecision::NoSession;
    }

    let mut skipped_unreadable = 0usize;
    let mut newest_failure: Option<(String, String)> = None;

    // One corrupt newest session must not cost the user every older one. Walk
    // down until something verifies, bounded so a directory full of damaged
    // files cannot turn startup into a long scan.
    for id in candidates.into_iter().take(MAX_AUTO_RESUME_CANDIDATES) {
        // Verify against the real file before trusting it: the listing above
        // only parsed each session's bounded metadata prefix. The probe only
        // needs durable metadata — repair belongs to the resume that follows,
        // so read the snapshot without running or logging it here.
        let saved = match manager.load_session_snapshot(&id) {
            Ok(saved) => saved,
            Err(err) => {
                skipped_unreadable += 1;
                if newest_failure.is_none() {
                    newest_failure = Some((id, describe_load_error(&err)));
                }
                continue;
            }
        };

        // A workspace mismatch is a different failure from corruption: it means
        // the candidate is fine but is not ours. Report it rather than counting
        // it as damage, and stop — resuming past it would be guessing.
        if !workspace_scope_matches(&saved.metadata.workspace, workspace) {
            return AutoResumeDecision::WorkspaceMismatch {
                session_id: saved.metadata.id,
            };
        }
        if saved.metadata.archived {
            continue;
        }

        return AutoResumeDecision::Resume {
            title: saved.metadata.title.clone(),
            session_id: saved.metadata.id,
            skipped_unreadable,
        };
    }

    match newest_failure {
        Some((session_id, reason)) => AutoResumeDecision::Unreadable {
            session_id,
            reason,
            skipped_unreadable,
        },
        None => AutoResumeDecision::NoSession,
    }
}

/// Newest-first eligible session ids for `workspace`.
///
/// Uses [`crate::session_projection::select_sessions`] so "which sessions
/// belong to this workspace" is answered by the same code the browse surfaces
/// use — auto-resume cannot pick something the rail would not have listed.
fn candidate_ids(
    sessions: &[crate::session_manager::SessionMetadata],
    workspace: &Path,
) -> Vec<String> {
    // Empty auto-created placeholders are excluded *before* the candidate
    // cap, the same definition `get_latest_session_for_workspace` applies:
    // a run of fresh placeholders must not push a real session out of reach.
    let query = crate::session_projection::SessionQuery::default()
        .with_filter(crate::session_manager::SessionListFilter::ActiveOnly)
        .with_sort(crate::session_projection::SessionSortMode::Recent)
        .scoped_to(workspace)
        .without_empty_auto_created()
        .with_limit(MAX_AUTO_RESUME_CANDIDATES);
    crate::session_projection::select_sessions(sessions, &query)
        .into_iter()
        .map(|metadata| metadata.id.clone())
        .collect()
}

fn plural_sessions(count: usize) -> &'static str {
    if count == 1 { "session" } else { "sessions" }
}

fn describe_load_error(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "session file is missing".to_string(),
        std::io::ErrorKind::InvalidData => "session file is corrupt".to_string(),
        std::io::ErrorKind::PermissionDenied => "session file is not readable".to_string(),
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::{SavedSession, create_saved_session_with_id_and_mode};
    use codewhale_models::Role;
    use codewhale_models::{ContentBlock, Message};
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        workspace: PathBuf,
        manager: SessionManager,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("tempdir");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let manager = SessionManager::new(dir.path().join("sessions")).expect("session manager");
        Fixture {
            _dir: dir,
            workspace,
            manager,
        }
    }

    fn saved(id: &str, workspace: &Path, title: &str) -> SavedSession {
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        }];
        let mut session = create_saved_session_with_id_and_mode(
            id.to_string(),
            &messages,
            "deepseek-chat",
            workspace,
            12,
            None,
            Some("agent"),
        );
        session.metadata.title = title.to_string();
        session
    }

    #[test]
    fn disabled_by_default_never_looks_at_disk() {
        let fx = fixture();
        fx.manager
            .save_session(&saved("s1", &fx.workspace, "Prior work"))
            .expect("save");

        let decision =
            decide_auto_resume(false, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision, AutoResumeDecision::Disabled);
        assert!(decision.starts_fresh());
        assert_eq!(decision.status_message(), None);
    }

    #[test]
    fn fresh_placeholders_do_not_push_a_real_session_past_the_candidate_cap() {
        let fx = fixture();
        let mut real = saved("real", &fx.workspace, "Real work");
        real.metadata.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
        fx.manager.save_session(&real).expect("save real session");
        for index in 0..MAX_AUTO_RESUME_CANDIDATES {
            let mut placeholder = create_saved_session_with_id_and_mode(
                format!("placeholder-{index}"),
                &[],
                "deepseek-chat",
                &fx.workspace,
                0,
                None,
                Some("agent"),
            );
            placeholder.metadata.title = crate::session_manager::DEFAULT_SESSION_TITLE.to_string();
            placeholder.metadata.updated_at =
                chrono::Utc::now() - chrono::Duration::seconds(index as i64);
            fx.manager
                .save_session(&placeholder)
                .expect("save placeholder");
        }
        let listed = fx.manager.list_sessions().expect("list");
        assert_eq!(
            listed
                .iter()
                .filter(|session| crate::session_manager::is_empty_auto_created_session(session))
                .count(),
            MAX_AUTO_RESUME_CANDIDATES,
            "fixture keeps a full cap of newer placeholders"
        );

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision.session_id(), Some("real"));
    }

    #[test]
    fn enabled_resumes_the_newest_valid_session_for_this_workspace() {
        let fx = fixture();
        fx.manager
            .save_session(&saved("older", &fx.workspace, "Older work"))
            .expect("save older");
        let mut newer = saved("newer", &fx.workspace, "Newer work");
        newer.metadata.updated_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        fx.manager.save_session(&newer).expect("save newer");

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision.session_id(), Some("newer"));
        assert_eq!(
            decision.status_message().as_deref(),
            Some("Auto-resumed session: Newer work")
        );
    }

    #[test]
    fn missing_session_starts_fresh_with_a_receipt() {
        let fx = fixture();

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision, AutoResumeDecision::NoSession);
        assert!(decision.starts_fresh());
        assert!(
            decision
                .status_message()
                .expect("receipt")
                .contains("starting fresh")
        );
    }

    #[test]
    fn corrupt_session_falls_back_to_fresh_instead_of_failing_startup() {
        let fx = fixture();
        fx.manager
            .save_session(&saved("broken", &fx.workspace, "Broken work"))
            .expect("save");
        // Drop the closing brace: the metadata block still parses from the
        // file prefix (so listing still surfaces the session), but the full
        // load fails. That is exactly the shape auto-resume must survive.
        let path = fx.manager.sessions_dir().join("broken.json");
        let content = std::fs::read_to_string(&path).expect("read session");
        let truncated = content
            .trim_end()
            .strip_suffix('}')
            .expect("session JSON ends with }");
        std::fs::write(&path, truncated).expect("truncate session");

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert!(decision.starts_fresh());
        assert!(
            matches!(&decision, AutoResumeDecision::Unreadable { session_id, .. } if session_id == "broken"),
            "expected an Unreadable decision, got {decision:?}"
        );
        assert!(
            decision
                .status_message()
                .expect("receipt")
                .contains("starting fresh")
        );
    }

    #[test]
    fn a_session_from_another_workspace_is_never_resumed() {
        let fx = fixture();
        let other = fx._dir.path().join("other-workspace");
        std::fs::create_dir_all(&other).expect("other workspace");
        fx.manager
            .save_session(&saved("elsewhere", &other, "Someone else's project"))
            .expect("save");

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert!(decision.starts_fresh());
        assert_eq!(decision, AutoResumeDecision::NoSession);
    }

    #[test]
    fn archived_sessions_are_not_auto_resume_candidates() {
        let fx = fixture();
        let mut session = saved("put-away", &fx.workspace, "Put away");
        session.metadata.archived = true;
        fx.manager.save_session(&session).expect("save");

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision, AutoResumeDecision::NoSession);
    }

    #[test]
    fn explicit_resume_wins_over_the_setting_in_both_directions() {
        let fx = fixture();
        fx.manager
            .save_session(&saved("auto", &fx.workspace, "Auto candidate"))
            .expect("save");

        let explicit = ResumeRequest {
            explicit_session_id: Some("chosen".to_string()),
            force_fresh: false,
        };
        // Setting off: the explicit id still resumes.
        assert_eq!(
            decide_auto_resume(false, &explicit, &fx.workspace, &fx.manager).session_id(),
            Some("chosen")
        );
        // Setting on: the explicit id is not replaced by the auto candidate.
        assert_eq!(
            decide_auto_resume(true, &explicit, &fx.workspace, &fx.manager).session_id(),
            Some("chosen")
        );
    }

    #[test]
    fn fresh_flag_suppresses_auto_resume() {
        let fx = fixture();
        fx.manager
            .save_session(&saved("auto", &fx.workspace, "Auto candidate"))
            .expect("save");

        let decision = decide_auto_resume(
            true,
            &ResumeRequest {
                explicit_session_id: None,
                force_fresh: true,
            },
            &fx.workspace,
            &fx.manager,
        );

        assert_eq!(decision, AutoResumeDecision::ForcedFresh);
        assert_eq!(decision.status_message(), None);
    }

    #[test]
    fn blank_explicit_id_is_treated_as_absent() {
        let fx = fixture();
        let decision = decide_auto_resume(
            false,
            &ResumeRequest {
                explicit_session_id: Some("   ".to_string()),
                force_fresh: false,
            },
            &fx.workspace,
            &fx.manager,
        );
        assert_eq!(decision, AutoResumeDecision::Disabled);
    }
}
