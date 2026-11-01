//! The one place that decides whether a delegated child may make a call that
//! **executes**, **mutates**, or **reaches the network**.
//!
//! Before this module the answer was spread across three hand-maintained name
//! lists ([`codewhale_agent_contract::role::RAW_SHELL_DENYLIST`] and its siblings) plus a
//! role posture that keyed on `ShellPolicy::Full`. That shape had a structural
//! hole: a name list can only deny the execution primitives someone remembered
//! to write down, and `shell = "full"` was being read as "may run arbitrary
//! code" by every tool whose approval requirement is `Required`. So a member
//! saved as read-only-with-checks (`write = false`, `shell = "full"` — the
//! `tester`/`verifier` preset, and any `custom` member shaped like it) lost
//! `Bash` and kept:
//!
//! - `tasks{action:"gate_run"}` — runs an operator-supplied command line;
//! - `automation{action:"run"}` / `{action:"create"}` — executes or schedules a
//!   stored automation, with its own cwd and prompt;
//! - `start_mcp_server` — spawns a process and opens a socket;
//! - every repository plugin tool, which is a shell command by definition;
//!
//! each of which mutates the workspace and reaches the network exactly as well
//! as the shell that was just removed, while the receipt said `write=false`.
//!
//! ## What is enforced
//!
//! The classification is derived, never listed: it comes from the tool's own
//! [`ToolCapability`] set and from `is_read_only_for` applied to the **actual
//! input**, after [`canonical_action_alias`] has resolved the family/action
//! pair. That is what makes it cover tools this file has never heard of —
//! plugins, runtime MCP servers, and anything registered later.
//!
//! | call classification | requires |
//! |---|---|
//! | read-only for this input | nothing |
//! | built-in verification (default or test-selection) | shell authority |
//! | `ExecutesCode` | write **and** shell authority |
//! | `WritesFiles` | write authority |
//! | `Network` | network authority |
//!
//! `ExecutesCode` requires *write* authority because an arbitrary program is an
//! arbitrary mutation primitive; requiring shell authority as well keeps the
//! existing posture rule from being weakened. Two carve-outs are deliberate and
//! are the "bounded positives" this module must not break:
//!
//! - **`agent`** declares `ExecutesCode` (it runs a child model loop), but
//!   delegation is governed by the depth budget and by the fact that the child
//!   inherits this same envelope. Denying it here would stop a read-only member
//!   from fanning out read-only work, which is a capability, not an escape.
//! - **Bounded verification** — `run_tests` / `run_verifiers` / `Run` — is the
//!   whole purpose of a read-only verifier, and the shipped `verifier` role is
//!   exactly `write = false, shell = "full"`. Classifying it by tool name would
//!   either take the role's job away or hand it a program launcher, so the
//!   bound is read off the concrete call by [`classify_verification`]:
//!   argument-free and pure test *selection* both cost shell authority (each
//!   forks a process, which `analyst`/`scout` were never granted), and
//!   anything that can name a program is held to the raw-shell bar. Every
//!   consumer of that contract — the catalog filter, the dispatch guard, and
//!   `reject_unbounded_verification` / `is_delegated_builtin_verification` in
//!   [`crate::tools::subagent`] — reads this one classifier rather than
//!   re-deriving it.

use serde_json::Value;

use crate::tools::spec::{ApprovalRequirement, ToolCapability, ToolSpec};
use codewhale_execpolicy::canonical_action::canonical_action_alias;

/// The execution authority a child actually holds, read off the runtime posture
/// rather than off a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionEnvelope {
    /// May mutate the workspace.
    pub write: bool,
    /// May be handed a model-visible network tool.
    pub network: bool,
    /// May run arbitrary commands (raw shell posture).
    pub shell: bool,
}

impl ExecutionEnvelope {
    /// The widest envelope: used by callers that impose no narrowing at all.
    ///
    /// Currently reached only from this module's tests — the production
    /// callers all derive an envelope from a real authority rather than
    /// starting from the widest one. Kept because it is the identity element
    /// [`Self::narrow`] is defined against, and removing it would leave that
    /// invariant untestable.
    pub const UNRESTRICTED: Self = Self {
        write: true,
        network: true,
        shell: true,
    };

    /// Whether this envelope narrows anything, i.e. whether enforcement can
    /// ever refuse a call.
    #[must_use]
    pub const fn is_unrestricted(self) -> bool {
        self.write && self.network && self.shell
    }

    /// Intersect with another envelope. Used wherever a child envelope is
    /// derived from a parent one: every field takes the more restrictive side,
    /// so a descendant can never widen an ancestor.
    ///
    /// Exercised by this module's tests today; the grandchild-derivation path
    /// that consumes it in production lands with the ratification UI.
    #[must_use]
    pub const fn narrow(self, other: Self) -> Self {
        Self {
            write: self.write && other.write,
            network: self.network && other.network,
            shell: self.shell && other.shell,
        }
    }
}

/// Tool names whose execution is governed by a different, stricter mechanism
/// and which therefore must not be judged by capability alone.
///
/// Only `agent` qualifies, and the reason is specific: its `ExecutesCode`
/// capability describes running a child *model loop*, not a child *program*,
/// and that loop runs under a narrowed copy of this same envelope. See the
/// module docs.
fn is_delegation_tool(canonical: &str) -> bool {
    canonical == "agent"
}

/// Cargo/test-harness flags a bounded verification call may carry.
///
/// An allowlist, not a denylist, and that is the whole of its security value.
/// The flags that turn `cargo test` into an arbitrary-program launcher —
/// `--config` (which can set `target.runner`), `--manifest-path`,
/// `--target-dir`, `--target` — are dangerous precisely because nobody thinks
/// to write them down. A denylist would have to enumerate them; this list has
/// to enumerate the harmless ones, and an unknown flag is refused by default.
///
/// Everything here either selects *which* of the workspace's own tests run or
/// changes how their output is reported.
const BOUNDED_TEST_FLAGS: &[&str] = &[
    "--all",
    "--all-features",
    "--all-targets",
    "--benches",
    "--bin",
    "--bins",
    "--color",
    "--doc",
    "--example",
    "--examples",
    "--exact",
    "--features",
    "--ignored",
    "--include-ignored",
    "--jobs",
    "--lib",
    "--no-default-features",
    "--no-fail-fast",
    "--nocapture",
    "--package",
    "--quiet",
    "--release",
    "--show-output",
    "--skip",
    "--test",
    "--test-threads",
    "--tests",
    "--verbose",
    "--workspace",
    "-j",
    "-p",
    "-q",
];

/// How tightly one call to the built-in verification surface is bounded.
///
/// This is the typed policy the whole verification contract keys on. It exists
/// because "bounded" is not a property of the *tool* — `run_tests` is both the
/// verifier's entire job and, with the wrong `args`, a way to point cargo at
/// another manifest — so the question has to be asked of the concrete call and
/// answered in one place that catalog and dispatch both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationBound {
    /// No operator input at all: the workspace's own configured checks.
    Default,
    /// Operator-supplied arguments that only *select* among the workspace's own
    /// tests. No shell metacharacters, no separators, no unknown flags — see
    /// [`is_bounded_test_argv`].
    Filter,
    /// Names a program to run, or a flag that can redirect what runs. This is
    /// arbitrary code execution wearing a verification tool's name.
    Unbounded,
}

/// Classify a call to the built-in verification surface, or `None` if the call
/// is not one.
///
/// `run_verifiers{commands}` is *always* unbounded: every entry names a
/// `program`, so there is no bounded form of it to admit.
#[must_use]
pub fn classify_verification(canonical: &str, input: &Value) -> Option<VerificationBound> {
    match canonical {
        "run_tests" => Some(match input.get("args") {
            None | Some(Value::Null) => VerificationBound::Default,
            Some(Value::String(args)) if args.trim().is_empty() => VerificationBound::Default,
            Some(Value::String(args)) if is_bounded_test_argv(args) => VerificationBound::Filter,
            // A wrongly-typed value fails closed and lets the tool's own schema
            // error explain the shape.
            Some(_) => VerificationBound::Unbounded,
        }),
        "run_verifiers" => Some(match input.get("commands") {
            None | Some(Value::Null) => VerificationBound::Default,
            Some(Value::Array(commands)) if commands.is_empty() => VerificationBound::Default,
            Some(_) => VerificationBound::Unbounded,
        }),
        _ => None,
    }
}

/// Whether a `run_tests` argv is a pure test *selection*.
///
/// Every token must be either an allowlisted flag (optionally `flag=value`) or
/// a bare filter, and every value must survive [`is_bounded_argv_value`], whose
/// character set contains no separator, no glob, and no shell metacharacter. So
/// `-p tui exact_fleet` passes, and `--manifest-path ../evil/Cargo.toml`,
/// `--config target.runner="sh -c ..."`, `$(id)`, `a; rm -rf .` and `../..` do
/// not.
#[must_use]
fn is_bounded_test_argv(args: &str) -> bool {
    args.split_whitespace().all(|arg| {
        // The cargo/harness separator carries nothing itself.
        if arg == "--" {
            return true;
        }
        if arg.starts_with('-') {
            let (flag, value) = arg.split_once('=').unwrap_or((arg, ""));
            return BOUNDED_TEST_FLAGS.contains(&flag) && is_bounded_argv_value(value);
        }
        is_bounded_argv_value(arg)
    })
}

/// The character set a bounded argv token may draw from.
///
/// Deliberately expressed as what is *allowed*: alphanumerics plus the four
/// characters a Rust test path needs (`_`, `-`, `.`, `:`). No `/`, `\`, `~`,
/// `*`, `$`, backtick, quote, or redirection — so a path, a glob, a traversal,
/// and every shell expansion are all excluded by construction rather than by a
/// list of things to fear.
#[must_use]
fn is_bounded_argv_value(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
}

/// How one concrete call is classified, for both enforcement and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallClass {
    /// Read-only for this input.
    Bounded,
    /// Built-in verification in its default, argument-free form, or narrowed to
    /// a test *selection*. Needs shell authority — either one starts a process —
    /// but not write authority, because running the workspace's own checks is
    /// what a read-only verifier is for.
    ///
    /// The argument-free form is deliberately **not** free. It carries no
    /// operator argument, so nothing about *what* runs is in doubt — but it
    /// still forks cargo and every configured verifier command, and a member
    /// saved as `analyst` (`shell = "none"`) or `scout` was given no authority
    /// to start a process at all. Treating "bounded" as "costless" is what let a
    /// shell-less posture launch the test suite while its ceiling said it could
    /// not run anything. The typed `verifier`/`tester` preset keeps this
    /// capability because its ceiling grants `shell = "full"`; that is the whole
    /// difference between the two roles.
    VerificationFilter,
    /// Built-in verification carrying an operator command line. Held to the
    /// same bar as raw shell, with its own wording.
    UnboundedVerification,
    /// Bounded remote-ref fetch (`Git{action: fetch}`): fixed argv against a
    /// configured remote, updating only remote-tracking refs. Needs shell
    /// authority (it forks git) and network (it reaches the remote) — but
    /// not write authority, because fetching what is under review is what a
    /// read-only verifier is for. Shape errors (unknown remote, malformed
    /// refspec) surface from the tool, not the envelope.
    BoundedFetch,
    /// Runs a program or a child process.
    Executes,
    /// Mutates the filesystem.
    Mutates,
    /// Reaches the network.
    Reaches,
}

/// Classify one call from the tool's real capabilities plus this input.
///
/// `ExecutesCode` outranks the others because it subsumes them: a call that can
/// run a program can write and can reach out, whatever else it declares.
#[must_use]
pub fn classify_call(name: &str, input: &Value, spec: &dyn ToolSpec) -> CallClass {
    let canonical = canonical_action_alias(name, input);
    if is_delegation_tool(canonical) {
        return CallClass::Bounded;
    }
    // The verification surface answers for itself, before the generic rules, so
    // an unbounded form can never be swallowed by a read-only claim and a
    // bounded one can never be judged on the tool's name alone.
    match classify_verification(canonical, input) {
        // Default and Filter land on the same class: both start a process, and
        // the only thing that separates them is whether an operator argument
        // narrowed *which* tests run. Neither is available to a posture with no
        // shell authority.
        Some(VerificationBound::Default | VerificationBound::Filter) => {
            return CallClass::VerificationFilter;
        }
        Some(VerificationBound::Unbounded) => return CallClass::UnboundedVerification,
        None => {}
    }
    // The bounded fetch answers for itself next, for the same reason: a
    // `git fetch <remote>` updates only remote-tracking refs through fixed
    // argv, so it needs a process (shell) and a remote (network) — but not
    // workspace write authority. Classed before the generic rules so a
    // malformed fetch can never be swallowed by the family's read-only
    // default and a bounded one is never judged as opaque execution.
    if canonical == "git_fetch" {
        return CallClass::BoundedFetch;
    }
    if spec.is_read_only_for(input) {
        return CallClass::Bounded;
    }
    let capabilities = spec.capabilities();
    if capabilities.contains(&ToolCapability::ExecutesCode) {
        CallClass::Executes
    } else if capabilities.contains(&ToolCapability::WritesFiles) {
        CallClass::Mutates
    } else if capabilities.contains(&ToolCapability::Network) {
        CallClass::Reaches
    } else {
        // Fail closed on an under-declared tool. A call that is not read-only
        // for this input and names no positive capability, yet still asks for
        // approval, is a tool describing its *consequence* without describing
        // its *mechanism* — `AutomationTool` was exactly this shape and its
        // `run` action executes a stored automation. Treating the approval
        // requirement as the floor means a tool has to be positively read-only
        // to escape the envelope, rather than merely quiet about itself.
        //
        // The two approval levels map to different classes on purpose.
        // `Required` is the level shell and code execution sit at, so it earns
        // `Executes`. `Suggest` is the file-mutation level, and mapping it to
        // `Executes` would additionally demand *shell* authority from a
        // write-capable child that has none — an over-block with no security
        // value, since the write requirement is the one that bites.
        match spec.approval_requirement_for(input) {
            ApprovalRequirement::Required => CallClass::Executes,
            ApprovalRequirement::Suggest => CallClass::Mutates,
            ApprovalRequirement::Auto => CallClass::Bounded,
        }
    }
}

/// Refuse a call that falls outside `envelope`.
///
/// Returns the operator-facing refusal text, which names the posture rather
/// than the tool, so the refusal reads as a contract instead of a malfunction.
///
/// `proven_read_only` (#5426/#5438) carries bounded read-only shell evidence
/// — the exact `agent_readonly_bash_input` predicate `BashTool::execute`
/// enforces under `ShellPolicy::ReadOnly` — so the call classifies as
/// [`CallClass::Bounded`]: the same class `classify_call` assigns when the
/// spec itself reports the input read-only. Two invariants: the admission
/// can never outrun the execute-time refusal (same predicate both sides),
/// and the parent's parallel auto-approve classifier is untouched —
/// `spec.is_read_only_for` still answers the deliberately tighter
/// `is_parallel_readonly_command` for every other consumer.
pub fn enforce_execution_envelope(
    name: &str,
    input: &Value,
    spec: &dyn ToolSpec,
    envelope: ExecutionEnvelope,
    proven_read_only: bool,
) -> Result<(), String> {
    if envelope.is_unrestricted() {
        return Ok(());
    }
    if proven_read_only {
        // Classified Bounded: no capability the call can exercise escapes
        // the envelope. Network-reaching shape is still rejected separately
        // by the child's network gate, and the execute path re-verifies the
        // same predicate before running anything.
        return Ok(());
    }
    match classify_call(name, input, spec) {
        CallClass::Bounded => Ok(()),
        CallClass::VerificationFilter => {
            if envelope.shell {
                Ok(())
            } else {
                Err(format!(
                    "[execution_envelope.verification.shell_denied] Tool {name} starts a test or verifier process, and this agent has no shell \
                     authority under its clamped permission ceiling. That holds for the default, \
                     argument-free form too: running the workspace's own checks still forks a \
                     process, which a `shell = \"none\"` posture was never granted. Use a member \
                     whose saved ceiling grants `shell = \"full\"` (the `verifier`/`tester` \
                     preset), or report findings without running the checks yourself."
                ))
            }
        }
        CallClass::UnboundedVerification => {
            if envelope.write && envelope.shell {
                Ok(())
            } else {
                Err(format!(
                    "[execution_envelope.verification.unbounded] Tool {name} was called with operator-supplied commands or arguments that can \
                     name a program or redirect what runs, which is arbitrary execution however \
                     it is spelled. This agent runs read-only under its clamped permission \
                     ceiling. The default verification gates, and test-selection arguments \
                     (filters, `-p`, `--lib`, `--exact`), remain available to a member whose \
                     ceiling grants shell authority."
                ))
            }
        }
        CallClass::BoundedFetch => {
            if !envelope.shell {
                return Err(format!(
                    "[execution_envelope.fetch.shell_denied] Tool {name} fetches remote refs, which starts a git process and updates remote-tracking refs, and this agent has no shell \
                     authority under its clamped permission ceiling. Use a member whose saved \
                     ceiling grants `shell = \"full\"` (the `verifier`/`tester` preset), or report \
                     findings without fetching the remote yourself."
                ));
            }
            if !envelope.network {
                return Err(format!(
                    "[execution_envelope.fetch.network_denied] Tool {name} fetches remote refs, which reaches the network, and this agent runs with no network \
                     capability under its clamped permission ceiling."
                ));
            }
            Ok(())
        }
        CallClass::Executes => {
            if !envelope.write {
                return Err(format!(
                    "[execution_envelope.executes.write_denied] Tool {name} runs a program or a child process, which mutates the workspace \
                     just as directly as a file write. This agent runs read-only under its \
                     clamped permission ceiling, so arbitrary execution is refused however it is \
                     spelled — shell, verification gate, automation, plugin, or MCP server. The \
                     built-in verification gates (Run/run_tests/run_verifiers in their default \
                     form) are still available."
                ));
            }
            if !envelope.shell {
                return Err(format!(
                    "[execution_envelope.executes.shell_denied] Tool {name} runs a program or a child process, and this agent has no shell \
                     authority under its clamped permission ceiling. Use a member whose saved \
                     ceiling grants `shell = \"full\"`."
                ));
            }
            Ok(())
        }
        CallClass::Mutates => {
            if envelope.write {
                Ok(())
            } else {
                Err(format!(
                    "[execution_envelope.mutates.write_denied] Tool {name} mutates state and this agent runs read-only under its clamped \
                     permission ceiling."
                ))
            }
        }
        CallClass::Reaches => {
            if envelope.network {
                Ok(())
            } else {
                Err(format!(
                    "[execution_envelope.network.denied] Tool {name} reaches the network and this agent runs with no network \
                     capability (`network_tool = false`) under its clamped permission ceiling."
                ))
            }
        }
    }
}

pub fn tool_call_denied(rules: Option<&[String]>, name: &str, input: &Value) -> bool {
    let action = canonical_action_alias(name, input);
    codewhale_execpolicy::tool_rules::tool_denied(rules, name)
        || codewhale_execpolicy::tool_rules::tool_denied(rules, action)
        || (action == "rlm_open"
            && input
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|url| !url.trim().is_empty())
            && codewhale_execpolicy::tool_rules::tool_denied(rules, "fetch_url"))
        || (matches!(
            classify_verification(action, input),
            Some(VerificationBound::Unbounded)
        ) && rules.is_some_and(|rules| {
            codewhale_execpolicy::tool_rules::tool_matches_any_rule(rules, "Bash")
        }))
}

/// Repeat the command ceiling at native dispatch and direct delegation sinks.
/// The existing child evidence exception is limited to the canonical lowercase
/// tool, a child-owned context, and the same strict read-only grammar enforced
/// by Bash itself. It never admits a session, stdin, or background command.
pub fn enforce_tool_denial(
    context: &crate::tools::spec::ToolContext,
    name: &str,
    input: &Value,
) -> Result<(), crate::tools::spec::ToolError> {
    let bounded_child_read = name == "bash"
        && context.owner_agent_id.is_some()
        && context.shell_policy == codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly
        && crate::tools::shell::agent_readonly_bash_input(input);
    if !bounded_child_read && tool_call_denied(Some(&context.disallowed_tools), name, input) {
        return Err(crate::tools::spec::ToolError::permission_denied(format!(
            "Tool '{name}' or its execution dependency is in the disallowed-tools list"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const READ_ONLY: ExecutionEnvelope = ExecutionEnvelope {
        write: false,
        network: false,
        shell: true,
    };

    /// A stand-in for any tool the registry may hold, including ones this file
    /// has never heard of. The point of the guard is that it needs nothing but
    /// the trait.
    struct FakeTool {
        name: &'static str,
        capabilities: Vec<ToolCapability>,
        read_only_action: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl ToolSpec for FakeTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "fake"
        }
        fn input_schema(&self) -> Value {
            json!({})
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            self.capabilities.clone()
        }
        fn is_read_only_for(&self, input: &Value) -> bool {
            match self.read_only_action {
                Some(action) => input.get("action").and_then(Value::as_str) == Some(action),
                None => false,
            }
        }
        async fn execute(
            &self,
            _input: Value,
            _context: &crate::tools::spec::ToolContext,
        ) -> Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError> {
            unreachable!("classification never executes")
        }
    }

    fn executes(name: &'static str, read_only_action: Option<&'static str>) -> FakeTool {
        FakeTool {
            name,
            capabilities: vec![ToolCapability::ExecutesCode],
            read_only_action,
        }
    }

    /// The blocker this module exists for: a read-only member that kept
    /// `shell = "full"` so it could run checks must not regain arbitrary
    /// execution through a tool the raw-shell name list never mentioned.
    #[test]
    fn execution_primitives_spelled_as_bookkeeping_are_refused_read_only() {
        for (name, input) in [
            (
                "tasks",
                json!({"action": "gate_run", "command": "rm -rf src"}),
            ),
            ("automation", json!({"action": "run", "id": "a1"})),
            (
                "automation",
                json!({"action": "create", "name": "x", "prompt": "exfiltrate"}),
            ),
            ("start_mcp_server", json!({"command": "node", "args": []})),
            ("plugin_deploy", json!({})),
        ] {
            let spec = executes(name, Some("list"));
            let error = enforce_execution_envelope(name, &input, &spec, READ_ONLY, false)
                .expect_err("read-only member must not execute programs");
            assert!(error.contains("read-only"), "{name}: {error}");
        }
    }

    /// The `analyst`/`scout` posture: tools, but no shell authority at all.
    const NO_SHELL: ExecutionEnvelope = ExecutionEnvelope {
        write: false,
        network: false,
        shell: false,
    };

    /// A member whose ceiling grants no shell must not start a process, and the
    /// argument-free verification gates are no exception: running the
    /// workspace's own checks still forks cargo and every configured verifier.
    /// "Bounded" bounds *what* runs, not *whether* something runs.
    #[test]
    fn a_shell_less_posture_cannot_start_a_verification_process() {
        let verifier = executes("run_verifiers", None);
        for input in [json!({}), json!({"commands": []})] {
            let error =
                enforce_execution_envelope("run_verifiers", &input, &verifier, NO_SHELL, false)
                    .expect_err("an analyst was granted no authority to start a process");
            assert!(error.contains("shell authority"), "{error}");
        }

        let tests = executes("run_tests", None);
        for input in [json!({}), json!({"args": "  "}), json!({"args": "-p tui"})] {
            enforce_execution_envelope("run_tests", &input, &tests, NO_SHELL, false)
                .expect_err("the default test gate still forks a process");
        }

        // The unbounded form was already refused and stays refused, with its own
        // wording — the two failures must not collapse into one.
        let error = enforce_execution_envelope(
            "run_verifiers",
            &json!({"commands": [{"program": "bash", "args": ["-lc", "id"]}]}),
            &verifier,
            NO_SHELL,
            false,
        )
        .expect_err("operator command lines are refused first");
        assert!(error.contains("arbitrary execution"), "{error}");
    }

    /// Bounded fetch costs shell plus network, never workspace write: the
    /// shipped `verifier`/`tester` ceiling (`write = false, shell = "full"`,
    /// network) keeps it, while a shell-less or network-less posture loses
    /// it with a naming-its-cause refusal (#6298).
    #[test]
    fn bounded_fetch_costs_shell_plus_network_never_write() {
        let fetch = executes("Git", None);
        let input = json!({"action": "fetch", "remote": "origin"});
        assert_eq!(
            classify_call("Git", &input, &fetch),
            CallClass::BoundedFetch
        );

        let verifier = ExecutionEnvelope {
            write: false,
            network: true,
            shell: true,
        };
        enforce_execution_envelope("Git", &input, &fetch, verifier, false)
            .expect("write=false shell=full with network keeps fetch");

        let error = enforce_execution_envelope("Git", &input, &fetch, NO_SHELL, false)
            .expect_err("a shell-less posture cannot fetch");
        assert!(error.contains("shell authority"), "{error}");

        let no_network = ExecutionEnvelope {
            write: false,
            network: false,
            shell: true,
        };
        let error = enforce_execution_envelope("Git", &input, &fetch, no_network, false)
            .expect_err("a network-less posture cannot fetch");
        assert!(error.contains("network"), "{error}");
    }

    /// `cwd` scopes *where* the workspace's own checks run; it cannot name a
    /// program or redirect what runs, so the Default/Filter bound — and the
    /// shell authority it costs — is unchanged by it.
    #[test]
    fn run_cwd_does_not_change_the_verification_bound() {
        assert_eq!(
            classify_verification("run_tests", &json!({"cwd": "crates/tui"})),
            Some(VerificationBound::Default)
        );
        assert_eq!(
            classify_verification("run_tests", &json!({"args": "-p tui", "cwd": "crates/tui"})),
            Some(VerificationBound::Filter)
        );
        assert_eq!(
            classify_verification("run_verifiers", &json!({"cwd": "crates/tui"})),
            Some(VerificationBound::Default)
        );
    }

    /// The other side of the same rule: the shipped `verifier`/`tester` preset
    /// is `write = false, shell = "full"`, and that is exactly the ceiling that
    /// keeps the verification surface. The typed role, not the tool name, is
    /// what separates it from `analyst`.
    #[test]
    fn a_verifier_ceiling_keeps_the_verification_surface() {
        let tests = executes("run_tests", None);
        for input in [json!({}), json!({"args": "-p tui exact_fleet"})] {
            enforce_execution_envelope("run_tests", &input, &tests, READ_ONLY, false)
                .expect("a verifier ceiling grants shell authority, which is its whole job");
        }
    }

    /// Catalog and dispatch read the same classifier, so a call the dispatch
    /// guard would refuse is never advertised. Asserting on `classify_call`
    /// directly is what pins that they cannot drift apart.
    #[test]
    fn the_default_and_filter_forms_classify_identically() {
        let tests = executes("run_tests", None);
        assert_eq!(
            classify_call("run_tests", &json!({}), &tests),
            CallClass::VerificationFilter
        );
        assert_eq!(
            classify_call("run_tests", &json!({"args": "-p tui"}), &tests),
            CallClass::VerificationFilter
        );
        assert_eq!(
            classify_call(
                "run_tests",
                &json!({"args": "--manifest-path ../x"}),
                &tests
            ),
            CallClass::UnboundedVerification
        );
    }

    /// …and the bounded positives it must not break.
    #[test]
    fn bounded_read_only_and_verification_calls_survive() {
        let tasks = executes("tasks", Some("list"));
        enforce_execution_envelope(
            "tasks",
            &json!({"action": "list"}),
            &tasks,
            READ_ONLY,
            false,
        )
        .expect("durable task bookkeeping is read-only");

        let verifier = executes("run_verifiers", None);
        for input in [json!({}), json!({"commands": []})] {
            enforce_execution_envelope("run_verifiers", &input, &verifier, READ_ONLY, false)
                .expect("the default verification gate is what a verifier is for");
        }
        let tests = executes("run_tests", None);
        for input in [json!({}), json!({"args": "  "})] {
            enforce_execution_envelope("run_tests", &input, &tests, READ_ONLY, false)
                .expect("the default test gate is bounded");
        }

        // Delegation stays available: a read-only member may still fan out
        // read-only children, which inherit this same envelope.
        let agent = executes("agent", None);
        enforce_execution_envelope(
            "agent",
            &json!({"prompt": "read"}),
            &agent,
            READ_ONLY,
            false,
        )
        .expect("delegation is governed by depth, not by write authority");
    }

    /// The shipped `verifier` role is `write = false, shell = "full"`, and its
    /// documented job is running the suite. A test *selection* must survive, or
    /// the envelope has taken a shipped role's purpose away.
    #[test]
    fn a_read_only_shell_capable_role_keeps_test_selection_arguments() {
        let tests = executes("run_tests", None);
        for args in [
            "-p codewhale-tui",
            "--lib fleet::exact",
            "exact_fleet_workflow --exact",
            "--package tui --test-threads=1 --nocapture",
            "--workspace --all-features -- --skip slow_case",
        ] {
            enforce_execution_envelope(
                "run_tests",
                &json!({"args": args}),
                &tests,
                READ_ONLY,
                false,
            )
            .unwrap_or_else(|error| panic!("`{args}` selects tests and must run: {error}"));
        }
    }

    /// …and *every* form is refused for the stricter read-only roles, which
    /// hold no shell authority at all.
    ///
    /// Both the selection form and the argument-free default are refused,
    /// because both fork a process: running the workspace's own configured
    /// checks is still starting a program, and a `shell = "none"` posture was
    /// never granted that. Admitting the default form would make "no shell" a
    /// label rather than a ceiling.
    #[test]
    fn a_shell_less_read_only_role_cannot_start_a_verification_process_at_all() {
        const NO_SHELL: ExecutionEnvelope = ExecutionEnvelope {
            write: false,
            network: false,
            shell: false,
        };
        let tests = executes("run_tests", None);
        for input in [json!({"args": "-p tui"}), json!({})] {
            let error = enforce_execution_envelope("run_tests", &input, &tests, NO_SHELL, false)
                .expect_err("a planner/scout/consultant has no shell authority");
            assert!(error.contains("shell"), "{input}: {error}");
        }
    }

    /// The escape hatch stays shut: a selection is a selection, and anything
    /// that can name a program or redirect what runs is raw shell.
    #[test]
    fn operator_command_lines_are_refused_however_they_are_spelled() {
        let tests = executes("run_tests", None);
        for args in [
            "--manifest-path ../evil/Cargo.toml",
            "--config target.runner=sh",
            "--target-dir /tmp/out",
            "$(id)",
            "a; rm -rf .",
            "--lib | tee /tmp/x",
            "../../etc/passwd",
            "tests/*",
            "--features tui/evil",
            "`whoami`",
        ] {
            let error = enforce_execution_envelope(
                "run_tests",
                &json!({"args": args}),
                &tests,
                READ_ONLY,
                false,
            )
            .expect_err("`{args}` is not a test selection");
            assert!(error.contains("read-only"), "{args}: {error}");
        }

        let verifier = executes("run_verifiers", None);
        for input in [
            json!({"commands": [{"program": "bash", "args": ["-lc", "rm -rf src"]}]}),
            // Wrongly-typed values fail closed rather than reading as absent.
            json!({"commands": "bash -lc whoami"}),
        ] {
            assert!(
                enforce_execution_envelope("run_verifiers", &input, &verifier, READ_ONLY, false)
                    .is_err(),
                "run_verifiers names programs and must be refused: {input}"
            );
        }
    }

    #[test]
    fn write_and_network_capabilities_are_gated_independently() {
        let writer = FakeTool {
            name: "pandoc_convert",
            capabilities: vec![ToolCapability::WritesFiles],
            read_only_action: None,
        };
        assert!(
            enforce_execution_envelope("pandoc_convert", &json!({}), &writer, READ_ONLY, false)
                .is_err()
        );

        let reacher = FakeTool {
            name: "mcp__remote__query",
            capabilities: vec![ToolCapability::Network],
            read_only_action: None,
        };
        assert!(
            enforce_execution_envelope(
                "mcp__remote__query",
                &json!({}),
                &reacher,
                READ_ONLY,
                false
            )
            .is_err()
        );
        assert!(
            enforce_execution_envelope(
                "mcp__remote__query",
                &json!({}),
                &reacher,
                ExecutionEnvelope {
                    network: true,
                    ..READ_ONLY
                },
                false
            )
            .is_ok()
        );
    }

    /// An unrestricted envelope must be a true no-op, so nothing here can
    /// change behavior for an ordinary write-capable child.
    #[test]
    fn an_unrestricted_envelope_refuses_nothing() {
        let spec = executes("tasks", None);
        enforce_execution_envelope(
            "tasks",
            &json!({"action": "gate_run", "command": "cargo test"}),
            &spec,
            ExecutionEnvelope::UNRESTRICTED,
            false,
        )
        .expect("a write-capable, shell-capable child keeps its gates");
    }

    #[test]
    fn narrowing_never_widens() {
        let parent = ExecutionEnvelope {
            write: false,
            network: true,
            shell: true,
        };
        let child = ExecutionEnvelope {
            write: true,
            network: false,
            shell: true,
        };
        let narrowed = parent.narrow(child);
        assert!(!narrowed.write, "a child cannot regain the parent's writes");
        assert!(!narrowed.network, "a child cannot regain its own denial");
        assert!(narrowed.shell);
    }
}
