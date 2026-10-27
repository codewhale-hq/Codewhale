use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Events that can trigger hook execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Triggered when a new session starts
    SessionStart,
    /// Triggered when a session ends (quit, Ctrl+C)
    SessionEnd,
    /// Triggered before a user message is sent to the LLM
    MessageSubmit,
    /// Triggered before a tool is executed
    ToolCallBefore,
    /// Triggered after a tool completes (success or failure)
    ToolCallAfter,
    /// Triggered when the user changes modes (Plan, Act, Operate)
    ModeChange,
    /// Triggered when an error occurs
    OnError,
    /// Triggered after a turn completes and post-turn state has been updated
    TurnEnd,
    /// Triggered when a sub-agent is spawned
    SubagentSpawn,
    /// Triggered when a sub-agent reaches a terminal state
    SubagentComplete,
    /// Triggered immediately before each `exec_shell` invocation. The hook's
    /// stdout is parsed as `KEY=VALUE\n` lines and merged on top of the
    /// shell command's environment — useful for ephemeral credentials,
    /// per-skill PATH adjustments, or short-lived tokens (#456). Hooks that
    /// fail or time out are logged but do *not* abort the shell call; they
    /// simply contribute no env vars.
    ShellEnv,
    /// Triggered when the session becomes idle after real work: a turn
    /// finished (or a wait ended) and no prompt, approval, or continuation
    /// is outstanding (#6004). Transient tool errors never fire this by
    /// themselves; it marks "agent done, waiting for the next instruction".
    SessionIdle,
    /// Triggered when a turn ends in a terminal failure (#6004). Transient
    /// tool failures that the agent absorbs never fire this; only a turn
    /// whose final status is failed does. Hook authors that want opencode's
    /// grace-period semantics should debounce inside the hook.
    SessionError,
    /// Triggered when the agent starts waiting on the person: an approval
    /// prompt opens, a `request_user_input` question is presented, or a goal
    /// continuation is parked between passes (#6004). The payload's `reason`
    /// field is `approval`, `user_input`, or `goal_continuation`.
    WaitingForUser,
    /// Triggered when idle or waiting transitions to active work (#6004).
    /// Startup and repeated observations of the same state stay silent.
    SessionBusy,
}

/// Every event name the runtime actually fires, in the order `/hooks events`
/// and `docs/HOOKS.md` list them. Tests assert this is exhaustive so a new
/// variant cannot ship without a documented firing point.
#[cfg(any(test, feature = "test-support"))]
pub const ALL_HOOK_EVENTS: [HookEvent; 15] = [
    HookEvent::SessionStart,
    HookEvent::SessionEnd,
    HookEvent::TurnEnd,
    HookEvent::MessageSubmit,
    HookEvent::ToolCallBefore,
    HookEvent::ToolCallAfter,
    HookEvent::ModeChange,
    HookEvent::OnError,
    HookEvent::SubagentSpawn,
    HookEvent::SubagentComplete,
    HookEvent::ShellEnv,
    HookEvent::SessionIdle,
    HookEvent::SessionError,
    HookEvent::WaitingForUser,
    HookEvent::SessionBusy,
];

/// How much a hook's result can change what Codewhale does next.
///
/// This is the steering allowlist. "Observer" is a statement about
/// Codewhale's control flow only — an observer hook is still an arbitrary
/// shell command and can have any external side effect it likes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookSteering {
    /// stdout/exit code can replace or block the submitted text.
    TransformsSubmittedText,
    /// stdout/exit code can allow, deny, ask, rewrite input, or add context.
    DecidesToolCall,
    /// stdout contributes `KEY=VALUE` pairs to one `exec_shell` invocation.
    ContributesShellEnv,
    /// stdout is ignored and the result cannot change Codewhale's behavior.
    Observer,
}

impl HookEvent {
    /// Get string representation for environment variable
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
            HookEvent::MessageSubmit => "message_submit",
            HookEvent::ToolCallBefore => "tool_call_before",
            HookEvent::ToolCallAfter => "tool_call_after",
            HookEvent::ModeChange => "mode_change",
            HookEvent::OnError => "on_error",
            HookEvent::TurnEnd => "turn_end",
            HookEvent::SubagentSpawn => "subagent_spawn",
            HookEvent::SubagentComplete => "subagent_complete",
            HookEvent::ShellEnv => "shell_env",
            HookEvent::SessionIdle => "session_idle",
            HookEvent::SessionError => "session_error",
            HookEvent::WaitingForUser => "waiting_for_user",
            HookEvent::SessionBusy => "session_busy",
        }
    }

    /// The steering contract for this event, as implemented.
    #[must_use]
    pub fn steering(self) -> HookSteering {
        match self {
            HookEvent::MessageSubmit => HookSteering::TransformsSubmittedText,
            HookEvent::ToolCallBefore => HookSteering::DecidesToolCall,
            HookEvent::ShellEnv => HookSteering::ContributesShellEnv,
            HookEvent::SessionStart
            | HookEvent::SessionEnd
            | HookEvent::ToolCallAfter
            | HookEvent::ModeChange
            | HookEvent::OnError
            | HookEvent::TurnEnd
            | HookEvent::SubagentSpawn
            | HookEvent::SubagentComplete
            | HookEvent::SessionIdle
            | HookEvent::SessionError
            | HookEvent::SessionBusy
            | HookEvent::WaitingForUser => HookSteering::Observer,
        }
    }

    /// Whether a hook result for this event can change Codewhale's own
    /// behavior. Never read this as "side-effect free" — see [`HookSteering`].
    #[must_use]
    pub fn can_steer(self) -> bool {
        !matches!(self.steering(), HookSteering::Observer)
    }

    /// Whether this event's context carries a tool name/arguments, so
    /// `tool_name` / `tool_category` conditions can ever match.
    ///
    /// `on_error` is included because the tool-failure path in
    /// `tui/tool_routing.rs` fires it with the tool name, call id, and result
    /// attached. An `on_error` firing that has no tool behind it (a transport
    /// or capacity error) simply does not match a tool predicate — it is
    /// skipped at dispatch, not rejected at load.
    #[must_use]
    pub fn provides_tool_identity(self) -> bool {
        matches!(
            self,
            HookEvent::ToolCallBefore
                | HookEvent::ToolCallAfter
                | HookEvent::ShellEnv
                | HookEvent::OnError
        )
    }

    /// Whether this event's context can carry a real process exit code, so
    /// `exit_code` conditions can ever match. `tool_call_after` observes every
    /// completed tool and `on_error` observes the failing ones; in both cases
    /// the code is only present when the tool actually reported one
    /// (`exec_shell` and friends).
    #[must_use]
    pub fn provides_exit_code(self) -> bool {
        matches!(self, HookEvent::ToolCallAfter | HookEvent::OnError)
    }

    /// Whether this event's context carries a mode label, so `mode`
    /// conditions can ever match. `shell_env` fires inside the `exec_shell`
    /// tool with a deliberately narrow context and has no mode.
    #[must_use]
    pub fn provides_mode(self) -> bool {
        !matches!(self, HookEvent::ShellEnv)
    }

    /// Whether `background = true` is honored as actual scheduling for this
    /// event. Events whose result is part of the contract are always run in
    /// the foreground, so declaring them background is a config error rather
    /// than a scheduling choice.
    #[must_use]
    pub fn honors_background(self) -> bool {
        !matches!(self, HookEvent::ShellEnv)
    }
}

/// Condition for when a hook should run
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum HookCondition {
    /// Always run this hook
    #[default]
    Always,
    /// Only run for specific tool names
    ToolName {
        /// Tool name to match (e.g., "`exec_shell`", "`write_file`")
        name: String,
    },
    /// Only run for specific tool categories
    ToolCategory {
        /// Category: "safe", "`file_write`", "shell"
        category: String,
    },
    /// Only run in specific modes
    Mode {
        /// Mode: "plan", "agent", "yolo"
        mode: String,
    },
    /// Only run when exit code matches (for `ToolCallAfter` / `OnError`)
    ExitCode {
        /// Exit code to match.
        ///
        /// `i64`, not `i32`: a Windows crash code such as `3221225477`
        /// (`0xC0000005`, access violation) is a real code a shell tool
        /// reports, and narrowing it would silently turn the predicate into
        /// one that can never match.
        code: i64,
    },
    /// Combine multiple conditions with AND
    All { conditions: Vec<HookCondition> },
    /// Combine multiple conditions with OR
    Any { conditions: Vec<HookCondition> },
}

#[derive(Debug, Clone)]
pub struct NativeShellHook {
    pub owner: crate::OwnerRef,
    pub scope: Option<crate::EntryRef>,
    pub handle: u64,
    pub dialect: String,
    pub point: String,
    pub matcher: Option<String>,
}

/// A single hook definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    #[serde(skip)]
    pub native_shell: Option<NativeShellHook>,
    /// The event that triggers this hook
    pub event: HookEvent,

    /// Shell command to execute (platform shell: `sh -c` on Unix, `cmd /C` on Windows)
    pub command: String,

    /// Optional condition for when this hook should run
    #[serde(default)]
    pub condition: Option<HookCondition>,

    /// Timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Run in background (don't wait for completion)
    #[serde(default)]
    pub background: bool,

    /// Continue if this hook fails (default: true)
    #[serde(default = "default_continue_on_error")]
    pub continue_on_error: bool,

    /// Optional name for logging/debugging
    #[serde(default)]
    pub name: Option<String>,

    /// Content- and generation-bound authority for a plugin-contributed hook.
    /// Never accepted from TOML; only the reviewed staged adapter may attach
    /// it after parsing immutable bytes.
    #[serde(skip)]
    pub plugin_authority: Option<crate::PluginAuthority>,

    /// Exact-byte project approval attached by the loader; never read from TOML.
    #[serde(skip)]
    pub project_authority: Option<crate::ProjectHookAuthority>,
}

fn default_timeout() -> u64 {
    30
}

fn default_continue_on_error() -> bool {
    true
}

impl Hook {
    /// Create a new hook with minimal configuration
    pub fn new(event: HookEvent, command: &str) -> Self {
        Self {
            event,
            command: command.to_string(),
            condition: None,
            timeout_secs: 30,
            background: false,
            continue_on_error: true,
            name: None,
            plugin_authority: None,
            project_authority: None,
            native_shell: None,
        }
    }

    /// Builder: set condition
    pub fn with_condition(mut self, condition: HookCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Builder: set timeout
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Builder: run in background
    pub fn background(mut self) -> Self {
        self.background = true;
        self
    }

    /// Builder: set name
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }
}

/// A configured hook that can never behave the way it is written.
///
/// Reported by [`HooksConfig::validate`] and, for rejections, surfaced in
/// `/hooks list` so a broken hook is visible instead of silently inert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfigProblem {
    /// Hook `name`, or `None` for an unnamed entry.
    pub name: Option<String>,
    /// The event the hook is registered for, or `None` when the problem is
    /// with a setting in the `[hooks]` table itself rather than with one
    /// entry — `default_timeout_secs` governs every hook, so pinning its
    /// rejection on an arbitrary hook would misreport the blast radius.
    pub event: Option<HookEvent>,
    /// What is wrong, in one line, with no paths or payload content.
    pub detail: String,
    /// `true` when the hook is dropped at load and will never run.
    pub rejected: bool,
}

/// Configuration for hooks (loaded from config.toml)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksConfig {
    /// List of hooks to execute
    #[serde(default)]
    pub hooks: Vec<Hook>,

    /// Global enable/disable for all hooks
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Global timeout override. When set this **replaces** every hook's own
    /// `timeout_secs` rather than only filling in for hooks that omit one —
    /// see `HookExecutor::execute_sync_inner`. Documented as-implemented in
    /// `docs/HOOKS.md`; leave unset for per-hook timeouts.
    #[serde(default)]
    pub default_timeout_secs: Option<u64>,

    /// Working directory for hook execution (default: workspace)
    #[serde(default)]
    pub working_dir: Option<PathBuf>,

    /// Problems found by [`HooksConfig::validate`] at load time. Never read
    /// from or written to `config.toml`; populated by
    /// the host loader so `/hooks` can show a rejected
    /// hook instead of leaving it silently inert.
    #[serde(skip)]
    pub problems: Vec<HookConfigProblem>,
}

fn default_enabled() -> bool {
    true
}

impl HooksConfig {
    /// Report every configured hook that cannot behave as written.
    ///
    /// A condition that references context the event never carries can never
    /// match, so a hook wearing one is inert — the dangerous version of that
    /// is a `deny` gate the operator believes is armed. Those are reported as
    /// `rejected` and dropped by the host loader rather than left
    /// to fail silently at dispatch time. Problems that only affect how a
    /// hook is scheduled are reported as warnings and the hook still runs.
    #[must_use]
    pub fn validate(&self) -> Vec<HookConfigProblem> {
        self.validate_settings()
            .into_iter()
            .chain(self.validate_indexed().into_iter().map(|(_, p)| p))
            .collect()
    }

    /// Problems with the `[hooks]` table itself, independent of any entry.
    ///
    /// `default_timeout_secs = 0` is the one that matters: it *replaces* every
    /// hook's own `timeout_secs`, so the per-hook `timeout_secs = 0` rejection
    /// does nothing to stop one line from killing every hook in the config
    /// before it can produce output — including a `tool_call_before` gate,
    /// which then fails closed on every tool call.
    pub fn validate_settings(&self) -> Vec<HookConfigProblem> {
        let mut problems = Vec::new();
        if self.default_timeout_secs == Some(0) {
            problems.push(HookConfigProblem {
                name: None,
                event: None,
                detail: "`default_timeout_secs = 0` would expire every hook immediately; \
                         the override is ignored and per-hook `timeout_secs` applies"
                    .to_string(),
                rejected: true,
            });
        }
        problems
    }

    /// [`Self::validate`], but each problem is paired with the index of the
    /// entry that produced it.
    ///
    /// The index is the hook's identity for rejection purposes. Keying on
    /// `(name, event)` instead would make one invalid unnamed `session_start`
    /// entry delete *every* unnamed `session_start` entry, and one invalid
    /// `gate` delete every other hook also called `gate` — innocent hooks
    /// dropped because they share a label with a broken one.
    pub fn validate_indexed(&self) -> Vec<(usize, HookConfigProblem)> {
        let mut problems = Vec::new();
        for (index, hook) in self.hooks.iter().enumerate() {
            let mut condition_rejections = Vec::new();
            collect_condition_problems(hook.event, hook.condition.as_ref(), &mut |detail| {
                condition_rejections.push(detail);
            });

            let mut push = |detail: String, rejected: bool| {
                problems.push((
                    index,
                    HookConfigProblem {
                        name: hook.name.clone(),
                        event: Some(hook.event),
                        detail,
                        rejected,
                    },
                ));
            };
            // A condition that can never match makes the hook inert, so it is
            // dropped; the rest only affect how the hook is scheduled, so the
            // hook still runs and the problem is a warning.
            for detail in condition_rejections {
                push(detail, true);
            }

            if hook.background && !hook.event.honors_background() {
                push(
                    format!(
                        "`background = true` is not honored for `{}`; its stdout is the \
                         contract, so it always runs in the foreground",
                        hook.event.as_str()
                    ),
                    false,
                );
            } else if hook.background && hook.event.can_steer() {
                push(
                    format!(
                        "`background = true` makes this `{}` hook observer-only — it is \
                         submitted and never awaited, so it cannot steer the turn",
                        hook.event.as_str()
                    ),
                    false,
                );
            }

            if hook.timeout_secs == 0 {
                push(
                    "`timeout_secs = 0` expires immediately; the command is killed \
                     before it can produce output"
                        .to_string(),
                    true,
                );
            }

            if hook.command.trim().is_empty() {
                push("`command` is empty".to_string(), true);
            }
        }
        problems
    }

    /// Get hooks for a specific event
    pub fn hooks_for_event(&self, event: HookEvent) -> Vec<&Hook> {
        if !self.enabled {
            return Vec::new();
        }
        self.hooks.iter().filter(|h| h.event == event).collect()
    }

    /// The timeout the runtime will actually apply to `hook`.
    ///
    /// `[hooks].default_timeout_secs` *replaces* the per-hook value when set.
    /// This is the single owner of that rule: the executor enforces it and
    /// `/hooks list` displays it, so the listing cannot advertise a per-hook
    /// number the runtime will not use.
    #[must_use]
    pub fn effective_timeout_secs(&self, hook: &Hook) -> u64 {
        // `filter`, not `unwrap_or`: `apply_validation` already strips a zero
        // override at load, but a `HooksConfig` can also be built in code, and
        // a zero here means "kill every hook before it speaks".
        self.default_timeout_secs
            .filter(|secs| *secs > 0)
            .unwrap_or(hook.timeout_secs)
    }

    /// `true` when `[hooks].default_timeout_secs` is overriding per-hook
    /// timeouts, so surfaces can name the provenance of the number they show.
    #[must_use]
    pub fn timeout_is_overridden(&self) -> bool {
        // An ignored zero override is not provenance: `/hooks list` must not
        // credit a number to a setting the runtime refused to apply.
        self.default_timeout_secs.is_some_and(|secs| secs > 0)
    }
}

/// Walk a condition tree and report every predicate the event can never
/// satisfy. `all` / `any` are walked so a nested unsupported predicate is
/// caught rather than hidden behind a combinator.
fn collect_condition_problems(
    event: HookEvent,
    condition: Option<&HookCondition>,
    reject: &mut impl FnMut(String),
) {
    let Some(condition) = condition else {
        return;
    };
    match condition {
        HookCondition::Always => {}
        HookCondition::ToolName { .. } | HookCondition::ToolCategory { .. } => {
            if !event.provides_tool_identity() {
                reject(format!(
                    "`{}` never carries a tool name, so this tool condition can never match",
                    event.as_str()
                ));
            }
        }
        HookCondition::Mode { .. } => {
            if !event.provides_mode() {
                reject(format!(
                    "`{}` runs with a narrow context that has no mode, so a `mode` \
                     condition can never match; scope it with `tool_name` or \
                     `tool_category` instead",
                    event.as_str()
                ));
            }
        }
        HookCondition::ExitCode { .. } => {
            if !event.provides_exit_code() {
                reject(format!(
                    "`{}` has no completed process to read an exit code from; \
                     `exit_code` conditions are only supported on `tool_call_after` \
                     and `on_error`",
                    event.as_str()
                ));
            }
        }
        HookCondition::All { conditions } | HookCondition::Any { conditions } => {
            for nested in conditions {
                // Reborrow rather than pass `reject` itself: `&mut F` also
                // implements `FnMut`, so passing it directly would recurse in
                // the type parameter and never finish monomorphizing.
                collect_condition_problems(event, Some(nested), &mut *reject);
            }
        }
    }
}
