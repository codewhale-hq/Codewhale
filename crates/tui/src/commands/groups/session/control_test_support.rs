//! FEAT-024 Phase 4: deterministic fake session-control facet for portable
//! handler tests. Canned returns plus a call log prove exact strings, actions,
//! call arguments, operation counts, and check order without touching the host.

#![cfg(test)]

use codewhale_command_contract::facets::{
    CommandSessionControlContext, HostedWorkTarget, PlanProjection, RelayProjection, RemoteLink,
    RemoteOpenOutcome, RemoteStartInfo, ResumeImportReceipt, ResumeSource, SessionTitleReceipt,
    TitleReport, TodoProjection,
};
use std::cell::RefCell;
use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct FakeControl {
    pub(crate) blocked: bool,
    pub(crate) relay: Option<RelayProjection>,
    pub(crate) resume: Option<Result<ResumeSource, String>>,
    pub(crate) import: Option<Result<ResumeImportReceipt, String>>,
    pub(crate) sanitized_title: Option<String>,
    pub(crate) rename: Option<Result<SessionTitleReceipt, String>>,
    pub(crate) title_report: Option<TitleReport>,
    pub(crate) set_title: Option<Result<(), String>>,
    pub(crate) clear_title: Option<Result<(), String>>,
    pub(crate) remote_status: Option<String>,
    pub(crate) remote_link: Option<Option<RemoteLink>>,
    pub(crate) browser_open: Option<RemoteOpenOutcome>,
    pub(crate) start_info: Option<RemoteStartInfo>,
    pub(crate) stop_refusal: Option<Option<String>>,
    pub(crate) hosted: Option<Option<HostedWorkTarget>>,
    pub(crate) blockers: Vec<String>,
    pub(crate) calls: RefCell<Vec<String>>,
}

pub(crate) fn message(result: &super::CommandResult) -> &str {
    result
        .message
        .as_deref()
        .map(|message| message.strip_prefix("Error: ").unwrap_or(message))
        .unwrap_or("")
}

impl FakeControl {
    fn call(&self, name: &str, arg: Option<&str>) {
        let mut calls = self.calls.borrow_mut();
        match arg {
            Some(arg) => calls.push(format!("{name}({arg})")),
            None => calls.push(name.to_string()),
        }
    }
}

impl CommandSessionControlContext for FakeControl {
    fn transition_blocked(&self) -> bool {
        self.call("transition_blocked", None);
        self.blocked
    }
    fn transition_blockers(&self) -> Vec<String> {
        self.call("transition_blockers", None);
        self.blockers.clone()
    }
    fn relay_projection(&self) -> RelayProjection {
        self.call("relay_projection", None);
        self.relay.clone().expect("unexpected relay_projection()")
    }
    fn open_resume_picker(&mut self) {
        self.call("open_resume_picker", None);
    }
    fn resolve_resume_source(&mut self, raw: &str) -> Result<ResumeSource, String> {
        self.call("resolve_resume_source", Some(raw));
        match self.resume.clone() {
            Some(result) => result,
            None => Ok(ResumeSource::NotFound {
                raw: raw.to_string(),
                error: "missing".to_string(),
            }),
        }
    }
    fn import_session_file(&mut self, path: PathBuf) -> Result<ResumeImportReceipt, String> {
        self.call("import_session_file", Some(&path.display().to_string()));
        match self.import.clone() {
            Some(result) => result,
            None => Err(format!(
                "unexpected import_session_file({}) on empty fake",
                path.display()
            )),
        }
    }
    fn sanitize_session_title(&self, raw: &str) -> String {
        self.call("sanitize_session_title", Some(raw));
        self.sanitized_title
            .clone()
            .unwrap_or_else(|| raw.to_string())
    }
    fn rename_session(&mut self, title: &str) -> Result<SessionTitleReceipt, String> {
        self.call("rename_session", Some(title));
        match self.rename.clone() {
            Some(result) => result,
            None => Err(format!("unexpected rename_session({title}) on empty fake")),
        }
    }
    fn title_report(&self) -> TitleReport {
        self.call("title_report", None);
        self.title_report
            .clone()
            .expect("unexpected title_report()")
    }
    fn set_window_title(&mut self, title: String) -> Result<(), String> {
        self.call("set_window_title", Some(&title));
        match self.set_title.clone() {
            Some(result) => result,
            None => Err("unexpected set_window_title on empty fake".to_string()),
        }
    }
    fn clear_window_title(&mut self) -> Result<(), String> {
        self.call("clear_window_title", None);
        match self.clear_title.clone() {
            Some(result) => result,
            None => Err("unexpected clear_window_title on empty fake".to_string()),
        }
    }
    fn remote_status(&self) -> String {
        self.call("remote_status", None);
        self.remote_status
            .clone()
            .expect("unexpected remote_status()")
    }
    fn remote_link(&self) -> Option<RemoteLink> {
        self.call("remote_link", None);
        self.remote_link.clone().expect("unexpected remote_link()")
    }
    fn remote_browser_open(&self) -> RemoteOpenOutcome {
        self.call("remote_browser_open", None);
        self.browser_open
            .clone()
            .expect("unexpected browser_open()")
    }
    fn remote_start_info(&self) -> RemoteStartInfo {
        self.call("remote_start_info", None);
        self.start_info.clone().expect("unexpected start_info()")
    }
    fn remote_stop_refusal(&self) -> Option<String> {
        self.call("remote_stop_refusal", None);
        self.stop_refusal
            .clone()
            .expect("unexpected stop_refusal()")
    }
    fn resolve_hosted_work_target(&self) -> Option<HostedWorkTarget> {
        self.call("resolve_hosted_work_target", None);
        self.hosted.clone().expect("unexpected hosted()")
    }
}

pub(crate) fn relay_projection_fixture() -> RelayProjection {
    RelayProjection {
        compact_template: "# Session relay".to_string(),
        handoff_path: ".codewhale/handoff.md".to_string(),
        workspace: "/work".to_string(),
        mode: "operate".to_string(),
        model: "model-x".to_string(),
        goal_objective: Some("objective-y".to_string()),
        goal_token_budget: Some(900),
        todos: TodoProjection::Absent,
        plan: PlanProjection::Absent,
    }
}
