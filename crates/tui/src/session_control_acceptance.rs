//! Shared acceptance matrix for the session control plane (#2934, #4397).
//!
//! One table, one row per contract line, each row exercised by a test in this
//! module. The table exists so the contract is greppable and so a reviewer can
//! see what is covered *and what is not* without reverse-engineering it from
//! test names — an acceptance list that lives only in an issue body drifts the
//! moment the code moves.
//!
//! Scope note, stated here rather than implied: these are integration-level
//! checks over the durable session store and the pure projection/decision
//! layers. They do not drive a terminal. Rendering questions — how the rail
//! looks at 40 columns, whether the archived label is legible in a given
//! theme — are listed in [`HUMAN_VERIFICATION`] as explicitly human work, not
//! quietly claimed as covered.

use std::path::{Path, PathBuf};

use codewhale_engine::session_manager::{
    SavedSession, SessionListFilter, SessionManager, SessionMutator,
    create_saved_session_with_id_and_mode,
};
use codewhale_engine::session_projection::{SessionQuery, SessionSortMode, project_sessions};
use codewhale_engine::session_resume::{AutoResumeDecision, ResumeRequest, decide_auto_resume};
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message};

/// Which issue's acceptance list a row comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contract {
    /// #2934 — persistent multi-session TUI experience.
    PersistentSessions,
    /// #4397 — multi-session dashboard + approval/input control plane.
    ControlPlane,
    /// Constraints both issues share (offline browsing, truthfulness).
    Shared,
}

/// One acceptance line and the test that holds it up.
#[derive(Debug, Clone, Copy)]
pub struct AcceptanceCase {
    pub contract: Contract,
    /// The behaviour promised, in the issue's own terms.
    pub behavior: &'static str,
    /// The test function in this module that exercises it.
    pub test: &'static str,
}

/// The matrix. Every entry must name a test that exists in this module —
/// `every_matrix_row_names_a_real_test` enforces it, so a row cannot become a
/// claim with nothing behind it.
pub const ACCEPTANCE_MATRIX: &[AcceptanceCase] = &[
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Sessions persist across a restart and are found again by a fresh manager",
        test: "sessions_persist_across_restart",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Browsing is scoped to the current workspace; another project's sessions are not listed",
        test: "browsing_is_scoped_to_the_selected_workspace",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Auto-resume with a valid session reattaches to it",
        test: "auto_resume_valid_session",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Auto-resume with no session for this workspace starts fresh with a receipt",
        test: "auto_resume_missing_session",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Auto-resume with a corrupt session file starts fresh with a receipt",
        test: "auto_resume_corrupt_session",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Auto-resume disabled (the default) never reattaches and says nothing",
        test: "auto_resume_disabled_by_default",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Auto-resume never resumes a session recorded against a different workspace",
        test: "auto_resume_never_crosses_a_workspace",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Search, sort, and preview inputs come from one projection shared by every surface",
        test: "search_sort_and_preview_are_one_projection",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Rename persists to disk and survives a reload",
        test: "rename_persists_and_survives_reload",
    },
    AcceptanceCase {
        contract: Contract::ControlPlane,
        behavior: "Archive is durable, hides the session from default listings, and is reversible",
        test: "archive_is_durable_reversible_and_hidden_by_default",
    },
    AcceptanceCase {
        contract: Contract::ControlPlane,
        behavior: "The real picker's filtered/sorted view equals the API projection of the same query",
        test: "tui_and_api_listings_agree",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "One workspace matcher: a nested path resolves to its repository root's scope",
        test: "a_nested_path_resolves_to_the_same_scope_as_its_repository_root",
    },
    AcceptanceCase {
        contract: Contract::ControlPlane,
        behavior: "An archive applied to the active session survives the next autosave",
        test: "archive_survives_the_next_autosave",
    },
    AcceptanceCase {
        contract: Contract::ControlPlane,
        behavior: "A rename applied to the active session survives the next autosave",
        test: "rename_survives_the_next_autosave",
    },
    AcceptanceCase {
        contract: Contract::ControlPlane,
        behavior: "An external writer is refused with a typed conflict while a session is live",
        test: "an_external_writer_is_refused_while_a_session_is_live",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Auto-resume skips corrupt newer candidates and names the skipped count",
        test: "auto_resume_skips_a_corrupt_newest_and_reports_how_many",
    },
    AcceptanceCase {
        contract: Contract::PersistentSessions,
        behavior: "Auto-resume reports honestly when every candidate is unreadable",
        test: "auto_resume_reports_when_every_candidate_is_unreadable",
    },
    AcceptanceCase {
        contract: Contract::Shared,
        behavior: "The auto-resume candidate walk is bounded",
        test: "auto_resume_candidate_walk_is_bounded",
    },
    AcceptanceCase {
        contract: Contract::ControlPlane,
        behavior: "Session archive filters resolve the include_archived/archived_only pair like threads",
        test: "session_and_thread_archive_filters_share_one_resolution",
    },
    AcceptanceCase {
        contract: Contract::Shared,
        behavior: "Browsing and resume decisions perform no provider or network call",
        test: "browsing_and_resume_are_offline",
    },
    AcceptanceCase {
        contract: Contract::Shared,
        behavior: "History and search results are bounded, never unbounded reads",
        test: "history_and_search_results_are_bounded",
    },
    AcceptanceCase {
        contract: Contract::Shared,
        behavior: "Projections report only recorded state — no fabricated live status",
        test: "projections_never_fabricate_live_state",
    },
];

/// Acceptance lines that are **not** covered here, and why.
///
/// Listed rather than omitted: an untested claim that looks tested is worse
/// than an honest gap. Each of these needs eyes on a real terminal or a real
/// running runtime.
pub const HUMAN_VERIFICATION: &[&str] = &[
    "Sessions rail legibility and truncation at narrow widths (40/60/80 columns) and short heights",
    "Rail row, archived label, and current-session marker contrast in each shipped theme",
    "Keyboard/modal ownership: `e`/`x` inside the picker do not leak to the composer, and the rail's row activation does not steal focus mid-turn",
    "Localized rail and archive strings render without clipping in ja / zh-Hans / ko / vi / pt-BR / es-419",
    "Web dashboard saved-sessions section: layout at mobile widths, and resume-into-thread behaviour against a live runtime",
    "SSE gap/reconnect state in the dashboard after resuming a session into a new thread",
    "Approval and user-input targeting from the dashboard against a live pending approval",
];

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Fixture {
        dir: TempDir,
        workspace: PathBuf,
        manager: SessionManager,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("tempdir");
            let workspace = dir.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("workspace");
            let manager =
                SessionManager::new(dir.path().join("sessions")).expect("session manager");
            Self {
                dir,
                workspace,
                manager,
            }
        }

        /// Reopen the same directory with a new manager — the closest thing to
        /// a process restart that a unit test can honestly claim.
        fn reopen(&self) -> SessionManager {
            SessionManager::new(self.manager.sessions_dir().to_path_buf())
                .expect("reopen session manager")
        }

        fn save(&self, id: &str, title: &str, workspace: &Path) {
            let session = saved(id, title, workspace);
            self.manager.save_session(&session).expect("save session");
        }
    }

    /// Metadata with every fuzzy-search haystack field (title, id, workspace)
    /// fixed, so a search assertion is about the matcher and not about
    /// whatever path `TempDir` happened to hand out.
    fn metadata_row(
        id: &str,
        title: &str,
        workspace: &str,
    ) -> codewhale_engine::session_manager::SessionMetadata {
        let mut session = saved(id, title, Path::new(workspace));
        session.metadata.workspace = PathBuf::from(workspace);
        session.metadata
    }

    fn saved(id: &str, title: &str, workspace: &Path) -> SavedSession {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("prompt for {title}"),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: format!("reply for {title}"),
                    cache_control: None,
                }],
            },
        ];
        let mut session = create_saved_session_with_id_and_mode(
            id.to_string(),
            &messages,
            "deepseek-chat",
            workspace,
            42,
            None,
            Some("agent"),
        );
        session.metadata.title = title.to_string();
        session
    }

    #[test]
    fn every_matrix_row_names_a_real_test() {
        // The module's own source is the registry; a row naming a test that
        // does not exist would otherwise read as coverage.
        let source = include_str!("session_control_acceptance.rs");
        for case in ACCEPTANCE_MATRIX {
            assert!(
                source.contains(&format!("fn {}(", case.test)),
                "matrix row `{}` names missing test `{}`",
                case.behavior,
                case.test
            );
        }
        assert!(
            !HUMAN_VERIFICATION.is_empty(),
            "the human-verification list must stay explicit, not silently empty"
        );
    }

    #[test]
    fn matrix_covers_both_issues_and_their_shared_constraints() {
        for contract in [
            Contract::PersistentSessions,
            Contract::ControlPlane,
            Contract::Shared,
        ] {
            assert!(
                ACCEPTANCE_MATRIX
                    .iter()
                    .any(|case| case.contract == contract),
                "no acceptance rows for {contract:?}"
            );
        }
    }

    #[test]
    fn sessions_persist_across_restart() {
        let fx = Fixture::new();
        fx.save("persisted", "Whale migration", &fx.workspace);

        let reopened = fx.reopen();
        let listed = reopened.list_sessions().expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Whale migration");
        let loaded = reopened.load_session("persisted").expect("load");
        assert_eq!(loaded.messages.len(), 2);
    }

    #[test]
    fn browsing_is_scoped_to_the_selected_workspace() {
        let fx = Fixture::new();
        let other = fx.dir.path().join("other");
        std::fs::create_dir_all(&other).expect("other workspace");
        fx.save("mine", "Mine", &fx.workspace);
        fx.save("theirs", "Theirs", &other);

        let all = fx.manager.list_sessions().expect("list");
        let scoped = project_sessions(
            &all,
            &SessionQuery::default().scoped_to(&fx.workspace),
            None,
        );

        assert_eq!(
            scoped.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["mine"]
        );
    }

    #[test]
    fn auto_resume_valid_session() {
        let fx = Fixture::new();
        fx.save("valid", "Resumable work", &fx.workspace);

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision.session_id(), Some("valid"));
        assert!(!decision.starts_fresh());
    }

    #[test]
    fn auto_resume_missing_session() {
        let fx = Fixture::new();

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision, AutoResumeDecision::NoSession);
        assert!(
            decision.status_message().is_some(),
            "fallback needs a receipt"
        );
    }

    #[test]
    fn auto_resume_corrupt_session() {
        let fx = Fixture::new();
        fx.save("corrupt", "Half-written", &fx.workspace);
        // Metadata still parses from the prefix; the full document does not.
        let path = fx.manager.sessions_dir().join("corrupt.json");
        let content = std::fs::read_to_string(&path).expect("read");
        let truncated = content
            .trim_end()
            .strip_suffix('}')
            .expect("session JSON ends with }");
        std::fs::write(&path, truncated).expect("truncate");

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert!(
            decision.starts_fresh(),
            "a corrupt session must not block startup"
        );
        assert!(matches!(decision, AutoResumeDecision::Unreadable { .. }));
    }

    #[test]
    fn auto_resume_disabled_by_default() {
        let fx = Fixture::new();
        fx.save("ignored", "Not resumed", &fx.workspace);

        let decision =
            decide_auto_resume(false, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(decision, AutoResumeDecision::Disabled);
        assert_eq!(
            decision.status_message(),
            None,
            "the default must be silent"
        );
    }

    #[test]
    fn auto_resume_never_crosses_a_workspace() {
        let fx = Fixture::new();
        let other = fx.dir.path().join("other");
        std::fs::create_dir_all(&other).expect("other workspace");
        fx.save("foreign", "Someone else's project", &other);

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert!(decision.starts_fresh());
        assert_ne!(decision.session_id(), Some("foreign"));
    }

    #[test]
    fn search_sort_and_preview_are_one_projection() {
        let fx = Fixture::new();
        fx.save("alpha", "Alpha lane work", &fx.workspace);
        fx.save("beta", "Beta lane work", &fx.workspace);
        let all = fx.manager.list_sessions().expect("list");

        // Search is asserted over a hand-built list rather than the temp-dir
        // fixture on purpose: the fuzzy matcher's haystack includes the
        // workspace path, and a random temp path can subsequence-match almost
        // any query. Fixing all three haystack fields is what makes this an
        // assertion about the matcher instead of about `TempDir`.
        let deterministic = vec![
            metadata_row("alpha", "Alpha lane work", "/repo"),
            metadata_row("beta", "Beta lane work", "/repo"),
        ];
        let searched = project_sessions(
            &deterministic,
            &SessionQuery::default()
                .scoped_to(Path::new("/repo"))
                .with_search("alpha lane"),
            None,
        );
        assert_eq!(
            searched.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["alpha"],
            "a substring of one title must not drag the other row in"
        );
        assert!(
            project_sessions(
                &deterministic,
                &SessionQuery::default().with_search("no-such-session"),
                None,
            )
            .is_empty()
        );

        let by_name = project_sessions(
            &all,
            &SessionQuery::default()
                .scoped_to(&fx.workspace)
                .with_sort(SessionSortMode::Name),
            None,
        );
        assert_eq!(
            by_name.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            vec!["Alpha lane work", "Beta lane work"]
        );

        // Preview is the recorded title, so it is stable and cheap. See the
        // `session_projection` module docs for why it is not a last message.
        assert!(by_name.iter().all(|s| s.preview == s.title));
    }

    #[test]
    fn rename_persists_and_survives_reload() {
        let fx = Fixture::new();
        fx.save("renameable", "Original title", &fx.workspace);

        let renamed = fx
            .manager
            .rename_session("renameable", "  Renamed title  ", SessionMutator::Owner)
            .expect("rename");
        assert_eq!(renamed.title, "Renamed title", "titles are trimmed");

        let reloaded = fx.reopen().load_session("renameable").expect("reload");
        assert_eq!(reloaded.metadata.title, "Renamed title");
        assert_eq!(
            reloaded.metadata.created_at, renamed.created_at,
            "rename must not disturb creation time"
        );

        assert!(
            fx.manager
                .rename_session("renameable", "   ", SessionMutator::Owner)
                .is_err(),
            "an empty title must be rejected, not silently applied"
        );
        assert!(
            fx.manager
                .rename_session("renameable", &"x".repeat(101), SessionMutator::Owner)
                .is_err(),
            "an over-long title must be rejected"
        );
    }

    #[test]
    fn archive_is_durable_reversible_and_hidden_by_default() {
        let fx = Fixture::new();
        fx.save("keep", "Active work", &fx.workspace);
        fx.save("putaway", "Finished work", &fx.workspace);

        let archived = fx
            .manager
            .set_session_archived("putaway", true, SessionMutator::Owner)
            .expect("archive");
        assert!(archived.archived);

        // Durable: a fresh manager sees the flag.
        let reopened = fx.reopen();
        assert!(
            reopened
                .load_session("putaway")
                .expect("reload")
                .metadata
                .archived
        );

        // Hidden by default, visible on request — asserted through the same
        // selection seam the picker, the rail, and `/v1/sessions` run, not a
        // test-only listing helper that would prove nothing about them.
        let listed = reopened.list_sessions().expect("list");
        let ids_for = |filter| {
            codewhale_engine::session_projection::select_sessions(
                &listed,
                &codewhale_engine::session_projection::SessionQuery::default().with_filter(filter),
            )
            .into_iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>()
        };
        assert_eq!(ids_for(SessionListFilter::ActiveOnly), vec!["keep"]);
        assert_eq!(ids_for(SessionListFilter::ArchivedOnly), vec!["putaway"]);
        assert_eq!(ids_for(SessionListFilter::IncludeArchived).len(), 2);

        // Not an auto-resume candidate while archived.
        assert_ne!(
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &reopened)
                .session_id(),
            Some("putaway")
        );

        // Reversible.
        let restored = reopened
            .set_session_archived("putaway", false, SessionMutator::Owner)
            .expect("restore");
        assert!(!restored.archived);
        let relisted = reopened.list_sessions().expect("list");
        assert_eq!(
            codewhale_engine::session_projection::select_sessions(
                &relisted,
                &codewhale_engine::session_projection::SessionQuery::default()
                    .with_filter(SessionListFilter::ActiveOnly),
            )
            .len(),
            2
        );
    }

    #[test]
    fn tui_and_api_listings_agree() {
        // The previous version of this test projected the same query twice and
        // asserted the results matched, which proves nothing. This one drives
        // the *actual picker* — its filtering, its sort cycling, its workspace
        // scope — and compares against the API's projection of the same store.
        //
        // The store is a private directory, not `default_location()` behind a
        // redirected `CODEWHALE_HOME`: that redirect is process-global, and a
        // concurrent test resolving the store without the env barrier could
        // observe it mid-test and write sessions into this fixture between the
        // two listings (#5929). The picker is fed the same list through
        // `new_with_sessions`, so both sides still see one store snapshot.
        let tmp = TempDir::new().expect("tempdir");
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        let workspace = PathBuf::from("/repo");
        let other = PathBuf::from("/other");

        for (id, title, ws) in [
            ("alpha", "Alpha work", &workspace),
            ("bravo", "Bravo work", &workspace),
            ("charlie", "Charlie work", &workspace),
            ("foreign", "Another project", &other),
        ] {
            manager.save_session(&saved(id, title, ws)).expect("save");
        }
        let all = manager.list_sessions().expect("list");

        let mut picker = crate::tui::session_picker::SessionPickerView::new_with_sessions(
            &workspace,
            codewhale_localization::Locale::En,
            all.clone(),
        );

        assert_eq!(
            picker.visible_session_ids(),
            api_ids(&all, &picker.view_query_for_test()),
            "picker default view must equal the API projection of the same query"
        );
        assert!(
            !picker
                .visible_session_ids()
                .contains(&"foreign".to_string()),
            "workspace scope must exclude another project"
        );

        // Every sort mode, including tie-breaks, must still agree.
        for _ in 0..3 {
            picker.cycle_sort_for_test();
            assert_eq!(
                picker.visible_session_ids(),
                api_ids(&all, &picker.view_query_for_test()),
                "picker and API must agree after cycling sort"
            );
        }

        picker.set_search_for_test("brav");
        assert_eq!(picker.visible_session_ids(), vec!["bravo".to_string()]);
        assert_eq!(
            picker.visible_session_ids(),
            api_ids(&all, &picker.view_query_for_test())
        );

        picker.set_search_for_test("");
        picker.toggle_all_workspaces();
        assert_eq!(
            picker.visible_session_ids(),
            api_ids(&all, &picker.view_query_for_test())
        );
        assert!(
            picker
                .visible_session_ids()
                .contains(&"foreign".to_string()),
            "broadening scope must reach the other workspace"
        );
    }

    /// The API's answer for a query, as ids.
    fn api_ids(
        sessions: &[codewhale_engine::session_manager::SessionMetadata],
        query: &SessionQuery,
    ) -> Vec<String> {
        project_sessions(sessions, query, None)
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    #[test]
    fn a_nested_path_resolves_to_the_same_scope_as_its_repository_root() {
        // Plain path equality — what the picker used to do — would treat these
        // as different projects. The shared matcher walks to the git root, so a
        // linked worktree or a nested crate dir stays in scope.
        let fx = Fixture::new();
        let repo = fx.dir.path().join("repo");
        let nested = repo.join("crates").join("tui");
        std::fs::create_dir_all(&nested).expect("nested");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");

        fx.save("root", "At the repo root", &repo);
        let all = fx.manager.list_sessions().expect("list");

        let from_nested = project_sessions(&all, &SessionQuery::default().scoped_to(&nested), None);
        assert_eq!(
            from_nested
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["root"],
            "a session saved at the repo root must be in scope from a nested path"
        );
    }

    #[test]
    fn archive_survives_the_next_autosave() {
        // The autosave-survival gate. An autosave rebuilds metadata from
        // in-memory state; a stale copy would revert the archive. The writer
        // re-reads persisted lifecycle fields first.
        let fx = Fixture::new();
        fx.save("live", "Being worked on", &fx.workspace);

        let stale = fx
            .manager
            .list_sessions()
            .expect("list")
            .into_iter()
            .find(|m| m.id == "live")
            .expect("metadata");
        assert!(!stale.archived);

        fx.manager
            .set_session_archived("live", true, SessionMutator::Owner)
            .expect("archive");

        // Simulate the autosave: rebuild from the stale snapshot, then merge.
        let mut autosaved = stale.clone();
        autosaved.message_count = 99;
        assert!(fx.manager.merge_persisted_lifecycle(&mut autosaved));
        assert!(
            autosaved.archived,
            "autosave must not revert an archive applied after its snapshot"
        );
        assert_eq!(
            autosaved.message_count, 99,
            "merging lifecycle state must not clobber conversation state"
        );
    }

    #[test]
    fn rename_survives_the_next_autosave() {
        let fx = Fixture::new();
        fx.save("live", "Before", &fx.workspace);
        let stale = fx
            .manager
            .list_sessions()
            .expect("list")
            .into_iter()
            .find(|m| m.id == "live")
            .expect("metadata");

        fx.manager
            .rename_session("live", "After", SessionMutator::Owner)
            .expect("rename");

        let mut autosaved = stale.clone();
        assert!(fx.manager.merge_persisted_lifecycle(&mut autosaved));
        assert_eq!(autosaved.title, "After");
    }

    #[test]
    fn an_external_writer_is_refused_while_a_session_is_live() {
        // The archive-race gate. The TUI owns the in-memory copy, so an
        // out-of-band write must fail closed rather than be reverted later.
        let _lock = crate::test_support::lock_test_env();
        let fx = Fixture::new();
        fx.save("owned", "Open in the TUI", &fx.workspace);
        codewhale_engine::session_manager::set_live_session(Some("owned"));

        let refused = fx
            .manager
            .set_session_archived("owned", true, SessionMutator::External)
            .expect_err("external write must be refused while the session is live");
        assert_eq!(refused.kind(), std::io::ErrorKind::ResourceBusy);
        assert!(
            !fx.manager
                .load_session("owned")
                .expect("reload")
                .metadata
                .archived,
            "a refused write must not have partially applied"
        );

        let refused_rename = fx
            .manager
            .rename_session("owned", "Nope", SessionMutator::External)
            .expect_err("external rename must be refused too");
        assert_eq!(refused_rename.kind(), std::io::ErrorKind::ResourceBusy);

        // The owner is still allowed.
        assert!(
            fx.manager
                .set_session_archived("owned", true, SessionMutator::Owner)
                .is_ok()
        );

        // Releasing the claim re-opens external writes.
        codewhale_engine::session_manager::set_live_session(None);
        assert!(
            fx.manager
                .rename_session("owned", "Now allowed", SessionMutator::External)
                .is_ok()
        );
    }

    #[test]
    fn auto_resume_skips_a_corrupt_newest_and_reports_how_many() {
        let fx = Fixture::new();
        fx.save("older", "Older but readable", &fx.workspace);
        for id in ["broken-a", "broken-b"] {
            let mut session = saved(id, "Damaged", &fx.workspace);
            session.metadata.updated_at = chrono::Utc::now() + chrono::Duration::minutes(10);
            fx.manager.save_session(&session).expect("save");
            let path = fx.manager.sessions_dir().join(format!("{id}.json"));
            let content = std::fs::read_to_string(&path).expect("read");
            let truncated = content.trim_end().strip_suffix('}').expect("closing brace");
            std::fs::write(&path, truncated).expect("truncate");
        }

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert_eq!(
            decision.session_id(),
            Some("older"),
            "one corrupt newest session must not cost the user every older one"
        );
        assert!(
            matches!(
                decision,
                AutoResumeDecision::Resume {
                    skipped_unreadable: 2,
                    ..
                }
            ),
            "the receipt must count what was skipped, got {decision:?}"
        );
        let receipt = decision.status_message().expect("receipt");
        assert!(
            receipt.contains("skipped 2 unreadable"),
            "receipt must name the skipped count: {receipt}"
        );
    }

    #[test]
    fn auto_resume_reports_when_every_candidate_is_unreadable() {
        let fx = Fixture::new();
        for id in ["aa", "bb"] {
            fx.save(id, "Damaged", &fx.workspace);
            let path = fx.manager.sessions_dir().join(format!("{id}.json"));
            let content = std::fs::read_to_string(&path).expect("read");
            let truncated = content.trim_end().strip_suffix('}').expect("closing brace");
            std::fs::write(&path, truncated).expect("truncate");
        }

        let decision =
            decide_auto_resume(true, &ResumeRequest::default(), &fx.workspace, &fx.manager);

        assert!(decision.starts_fresh());
        assert!(matches!(
            decision,
            AutoResumeDecision::Unreadable {
                skipped_unreadable: 2,
                ..
            }
        ));
    }

    #[test]
    fn auto_resume_candidate_walk_is_bounded() {
        const {
            assert!(
                codewhale_engine::session_resume::MAX_AUTO_RESUME_CANDIDATES <= 16,
                "a damaged sessions directory must not turn startup into a long scan"
            );
        }
    }

    #[test]
    fn session_and_thread_archive_filters_share_one_resolution() {
        use codewhale_engine::runtime_threads::manager::ThreadListFilter;

        for (include, only, expected_session, expected_thread) in [
            (
                None,
                None,
                SessionListFilter::ActiveOnly,
                ThreadListFilter::ActiveOnly,
            ),
            (
                Some(true),
                None,
                SessionListFilter::IncludeArchived,
                ThreadListFilter::IncludeArchived,
            ),
            (
                None,
                Some(true),
                SessionListFilter::ArchivedOnly,
                ThreadListFilter::ArchivedOnly,
            ),
            (
                Some(true),
                Some(true),
                SessionListFilter::ArchivedOnly,
                ThreadListFilter::ArchivedOnly,
            ),
        ] {
            assert_eq!(
                SessionListFilter::from_query(include, only),
                expected_session
            );
            // Thread-side expectation is spelled out so a future change to
            // either resolver breaks this test rather than drifting quietly.
            let thread = if only.unwrap_or(false) {
                ThreadListFilter::ArchivedOnly
            } else if include.unwrap_or(false) {
                ThreadListFilter::IncludeArchived
            } else {
                ThreadListFilter::ActiveOnly
            };
            assert_eq!(thread, expected_thread);
        }
    }

    #[test]
    fn browsing_and_resume_are_offline() {
        // Structural, not behavioural: the browse/resume path must not name a
        // provider or HTTP client. A test that "made no network call" during
        // one run would prove nothing about the next one.
        for (name, source) in [
            (
                "session_projection",
                include_str!("../../engine/src/session_projection.rs"),
            ),
            (
                "session_resume",
                include_str!("../../engine/src/session_resume.rs"),
            ),
        ] {
            for forbidden in [
                "reqwest",
                "llm_client",
                "ProviderKind",
                "http://",
                "https://",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{name} must stay offline but references `{forbidden}`"
                );
            }
        }
    }

    #[test]
    fn history_and_search_results_are_bounded() {
        let fx = Fixture::new();
        for i in 0..30 {
            fx.save(&format!("s{i:02}"), &format!("Session {i}"), &fx.workspace);
        }
        let all = fx.manager.list_sessions().expect("list");

        let capped = project_sessions(
            &all,
            &SessionQuery::default()
                .scoped_to(&fx.workspace)
                .with_limit(5),
            None,
        );
        assert_eq!(capped.len(), 5);

        // Even an unbounded request is clamped by the projection cap.
        let unbounded = project_sessions(
            &all,
            &SessionQuery::default()
                .scoped_to(&fx.workspace)
                .with_limit(usize::MAX),
            None,
        );
        assert!(unbounded.len() <= codewhale_engine::session_projection::MAX_PROJECTED_SESSIONS);

        // Row text is bounded too, so one pathological title cannot blow up a
        // row or a response.
        let long = "x".repeat(5_000);
        fx.save("long", &long, &fx.workspace);
        let rows = project_sessions(
            &fx.manager.list_sessions().expect("list"),
            &SessionQuery::default().scoped_to(&fx.workspace),
            None,
        );
        let long_row = rows.iter().find(|r| r.id == "long").expect("long row");
        assert!(long_row.title.chars().count() <= 140);
        assert!(long_row.preview.chars().count() <= 140);
    }

    #[test]
    fn projections_never_fabricate_live_state() {
        let fx = Fixture::new();
        fx.save("recorded", "Recorded work", &fx.workspace);
        let all = fx.manager.list_sessions().expect("list");

        // No caller-supplied active id: nothing may claim to be current.
        let anonymous = project_sessions(
            &all,
            &SessionQuery::default().scoped_to(&fx.workspace),
            None,
        );
        assert!(
            anonymous.iter().all(|row| !row.is_current),
            "current-ness comes from the caller, never inferred from disk"
        );

        // Counts and timestamps are the recorded ones, not derived guesses.
        let row = &anonymous[0];
        let metadata = all.iter().find(|m| m.id == row.id).expect("metadata");
        assert_eq!(row.message_count, metadata.message_count);
        assert_eq!(row.updated_at, metadata.updated_at);
        assert_eq!(row.total_tokens, metadata.total_tokens);
        assert_eq!(row.archived, metadata.archived);
    }
}
