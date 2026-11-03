//! Semantic aliases for the model-facing action tools.
//!
//! `Bash`, `File`, `Git`, `Run`, `Web`, and `rlm` deliberately keep their
//! canonical names at the execution and audit boundaries.  Presentation and
//! policy consumers, however, still understand the older per-action names.
//! Resolve that semantic name in one place so live calls and saved legacy
//! transcripts receive identical downstream behavior without rewriting the
//! original call.
//!
//! This table is not documentation — it is the **action-policy seam**. A
//! permission check that denies `fetch_url` only reaches `Web{action:"fetch"}`
//! because the pair is listed here. A family that is missing from the table is
//! a family whose actions no deny list can see, which is why `rlm` was added:
//! `rlm{action:"open", url:...}` calls `FetchUrlTool` *inside the process*,
//! under its own name, and a name-keyed deny list never sees that call.

use serde_json::Value;

pub const CANONICAL_ACTION_ALIASES: &[(&str, &str, &str)] = &[
    ("bash", "run", "exec_shell"),
    ("bash", "wait", "exec_shell_wait"),
    ("bash", "interact", "exec_shell_interact"),
    ("bash", "cancel", "exec_shell_cancel"),
    ("Bash", "run", "exec_shell"),
    ("Bash", "wait", "exec_shell_wait"),
    ("Bash", "interact", "exec_shell_interact"),
    ("Bash", "cancel", "exec_shell_cancel"),
    ("File", "read", "read_file"),
    ("File", "list", "list_dir"),
    ("File", "search_name", "file_search"),
    ("File", "search_content", "grep_files"),
    ("File", "write", "write_file"),
    ("File", "edit", "edit_file"),
    ("File", "patch", "apply_patch"),
    ("Git", "status", "git_status"),
    ("Git", "diff", "git_diff"),
    ("Git", "log", "git_log"),
    ("Git", "show", "git_show"),
    ("Git", "blame", "git_blame"),
    ("Git", "commit_plan", "git_commit_plan"),
    ("Git", "fetch", "git_fetch"),
    ("Git", "merge_tree", "git_merge_tree"),
    ("Git", "review_scope", "git_review_scope"),
    ("Run", "tests", "run_tests"),
    ("Run", "verifiers", "run_verifiers"),
    ("Web", "search", "web_search"),
    ("Web", "fetch", "fetch_url"),
    ("Web", "wait", "wait_for_dev_server"),
    // The RLM session family. `open` reaches the network (it fetches a `url`
    // through `FetchUrlTool` in-process) and `eval` runs operator-supplied
    // Python against a live kernel — sockets and filesystem both. The other
    // three actions are bounded local metadata. Listing every pair is what lets
    // a deny list keep the local half and remove the reaching half, instead of
    // having to choose between the whole family and nothing.
    ("rlm", "session_objects", "rlm_session_objects"),
    ("rlm", "open", "rlm_open"),
    ("rlm", "eval", "rlm_eval"),
    ("rlm", "configure", "rlm_configure"),
    ("rlm", "close", "rlm_close"),
    // The durable-work families. These were absent for the same reason `rlm`
    // was: they are model-visible under one canonical name (`tasks`,
    // `automation`, `github`) and their per-action legacy names are registered
    // as *hidden* aliases. A deny list naming `task_gate_run` therefore never
    // saw `tasks{action:"gate_run"}`, and the action-enum pruner never saw the
    // family at all — so an operator ceiling could not express "durable task
    // bookkeeping, yes; running a gate command, no".
    //
    // `gate_run` runs an operator-supplied command, `automation.run` executes a
    // stored automation, and the mutating `automation.*` actions schedule agent
    // runs with their own cwd. All three are execution primitives spelled as
    // bookkeeping, which is exactly the shape
    // `execution_envelope` classifies from capabilities.
    ("tasks", "create", "task_create"),
    ("tasks", "list", "task_list"),
    ("tasks", "read", "task_read"),
    ("tasks", "cancel", "task_cancel"),
    ("tasks", "gate_run", "task_gate_run"),
    ("tasks", "pr_attempt_record", "pr_attempt_record"),
    ("tasks", "pr_attempt_list", "pr_attempt_list"),
    ("tasks", "pr_attempt_read", "pr_attempt_read"),
    ("tasks", "pr_attempt_preflight", "pr_attempt_preflight"),
    ("automation", "create", "automation_create"),
    ("automation", "list", "automation_list"),
    ("automation", "read", "automation_read"),
    ("automation", "update", "automation_update"),
    ("automation", "pause", "automation_pause"),
    ("automation", "resume", "automation_resume"),
    ("automation", "delete", "automation_delete"),
    ("automation", "run", "automation_run"),
    ("github", "issue_context", "github_issue_context"),
    ("github", "pr_context", "github_pr_context"),
    ("github", "comment", "github_comment"),
    ("github", "close_issue", "github_close_issue"),
    ("github", "close_pr", "github_close_pr"),
    ("github", "report_draft", "github_report_draft"),
    ("github", "report_read", "github_report_read"),
];

/// The conservative action label policy uses when the model omits `action`.
///
/// This is a *policy* fallback only. Execution rejects an actionless call in
/// every family (see `required_action`); approval and parallel-safety
/// predicates cannot return an error, so they still need a label, and it must
/// be the family's least dangerous action.
///
/// `None` means the family never had even a policy default — `rlm`'s contract:
/// `RlmTool::resolve_action` errors rather than guessing.
/// Policy still resolves an *explicit* action for such a family — see
/// [`canonical_action_alias`].
#[must_use]
pub fn action_family_default(tool_name: &str) -> Option<Option<&'static str>> {
    match tool_name {
        "bash" | "Bash" => Some(Some("run")),
        "File" => Some(Some("read")),
        "Git" => Some(Some("status")),
        "Run" => Some(Some("tests")),
        "Web" => Some(Some("search")),
        // Families whose wrappers reject an actionless call rather than
        // guessing. Policy still resolves an *explicit* action for them.
        "rlm" | "tasks" | "automation" | "github" => Some(None),
        _ => None,
    }
}

/// Whether `tool_name` is a model-facing action family whose `action` enum
/// policy may prune.
///
/// Derived from [`action_family_default`] rather than spelled out at each call
/// site: a hard-coded family list that falls behind the alias table is a family
/// whose actions stay visible after policy removed them.
#[must_use]
pub fn is_action_family(tool_name: &str) -> bool {
    action_family_default(tool_name).is_some()
}

/// Resolve a canonical action tool to the legacy name for that exact action.
///
/// A missing action falls back to the family's conservative default so the
/// *policy* label is never absent; execution itself refuses the call (see
/// `required_action`). Unknown actions stay canonical so policy remains
/// conservative and the eventual tool error is attributed to the call the
/// model actually made.
///
/// A family with no default (`rlm`) still resolves an **explicit** action. The
/// earlier shape returned the family name for any such call, which meant
/// `rlm{action:"eval"}` never resolved to `rlm_eval` and therefore never met a
/// deny list entry naming it.
#[must_use]
pub fn canonical_action_alias<'a>(tool_name: &'a str, input: &Value) -> &'a str {
    // The new model-facing file primitives deliberately reuse the old
    // semantic policy names. This keeps permissions.toml, repo law, resource
    // envelopes, approval caches, audit aggregation, and saved policy state
    // compatible across the presentation change.
    match tool_name {
        "read" => return "read_file",
        "write" => return "write_file",
        "edit" => return "edit_file",
        _ => {}
    }
    let Some(default_action) = action_family_default(tool_name) else {
        return tool_name;
    };
    let Some(action) = input
        .get("action")
        .and_then(Value::as_str)
        .or(default_action)
    else {
        return tool_name;
    };

    CANONICAL_ACTION_ALIASES
        .iter()
        .find_map(|(family, candidate_action, alias)| {
            (*family == tool_name && *candidate_action == action).then_some(*alias)
        })
        .unwrap_or(tool_name)
}
