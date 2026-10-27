//! Approval fingerprint keys (§5.A).
//!
//! Instead of caching by tool name alone (which would let an approved
//! `exec_shell "cat foo"` silently pass `exec_shell "rm -rf /"`), the
//! approval flow uses a **call fingerprint** — a digest of the tool name
//! and the semantically‑relevant portion of its arguments.
//!
//! ## Two fingerprint shapes
//!
//! There are two key flavours, used for opposite sides of the decision:
//!
//! * [`build_approval_key`] — an **exact** digest of the full arguments.
//!   Used to scope *denials* so that denying one call (e.g. `rm -rf /tmp/x`)
//!   does not also suppress a later, different call to the same tool (#1617).
//!
//!   | Tool           | Exact key                                |
//!   |---------------|------------------------------------------|
//!   | file writes    | `file:<tool_name>:<hash of args>`        |
//!   | shell tools    | `shell:<tool_name>:<hash of args>`       |
//!   | `fetch_url`    | `net:<hostname>`                         |
//!   | everything else| `tool:<tool_name>:<hash of input>`       |
//!
//! * [`build_approval_grouping_key`] — a **lossy / arity-aware** digest.
//!   Used to scope *approvals* so that approving `cargo build` for the
//!   session also covers `cargo build --release` (the v0.8.37 behaviour).
//!
//!   | Tool           | Grouping key                             |
//!   |---------------|------------------------------------------|
//!   | `apply_patch`  | `patch:<hash of file paths>`             |
//!   | shell tools    | `shell:<command family>` for a simple, known command; `shell:cmd:<full normalized command>` otherwise |
//!   | shell interact / wait | `shell:<tool_name>:<hash of args>` |
//!   | `fetch_url`    | `net:<hostname>`                         |
//!   | Computer Use consent / `app_script` | `cu:<tool_name>:<hash of input>` |
//!   | other MCP tools| `mcp:<tool_name>` (the reviewed kind)    |
//!   | everything else| `tool:<tool_name>:<hash of input>`       |
//!
//! ## Computer Use calls that need a human (K1 / K2)
//!
//! [`computer_use_user_gate`] names the Computer Use calls whose approval must
//! come from a person: granting or revoking per-app consent (which includes the
//! shared-pointer `scope: "foreground"` decision) and `app_script`, an
//! unsandboxed osascript. Those calls are never covered by the MCP kind grant:
//! their session grant is the exact call, so allowing app X never allows app Y
//! and approving one script never approves a changed one. Engine preparation
//! also refuses them in any posture that cannot open a human approval card,
//! and refuses a `run_actions` batch that carries one as a step
//! ([`computer_use_batch_hidden_gate`]).
//!
//! Known limits of this stopgap: the calls are matched by MCP tool-name suffix
//! (`_consent`, `_consent_allow`, `_consent_revoke`, `_app_script`), so a
//! different MCP server exposing a tool with one of those names is gated the
//! same way (fail closed). Consent, script and computer registration/spawn
//! calls force an exact human card. Only its human decision can be attested
//! to the reviewed built-in plugin; grants and autonomous modes cannot mint it.
use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

use codewhale_execpolicy::command_safety::{canonical_prefix_is_leading, classify_command};

/// The fingerprint of a tool call — stable enough to match repeated
/// calls but specific enough to avoid privilege confusion.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApprovalKey(pub String);

/// Build the approval‑cache key for a tool call.
///
/// The key incorporates the tool name and a canonical digest of the
/// arguments so that denying one call suppresses exact retries, not later
/// invocations of the same tool with different parameters.
#[must_use]
pub fn build_approval_key(tool_name: &str, input: &serde_json::Value) -> ApprovalKey {
    let tool_name =
        codewhale_execpolicy::canonical_action::canonical_action_alias(tool_name, input);
    let fingerprint = match tool_name {
        "apply_patch" | "write_file" | "edit_file" | "fim_edit" => {
            format!("file:{tool_name}:{}", hash_json_value(input))
        }
        "exec_shell"
        | "task_shell_start"
        | "exec_shell_wait"
        | "exec_shell_interact"
        | "exec_wait"
        | "exec_interact" => {
            let mut execution_input = input.clone();
            if let Some(fields) = execution_input.as_object_mut() {
                fields.remove("description");
            }
            format!("shell:{tool_name}:{}", hash_json_value(&execution_input))
        }
        "fetch_url" | "web.fetch" | "web_fetch" => {
            let host = parse_host(input);
            format!("net:{host}")
        }
        _ => format!("tool:{tool_name}:{}", hash_json_value(input)),
    };
    ApprovalKey(fingerprint)
}

/// Build the **grouping** approval key for a tool call.
///
/// Unlike [`build_approval_key`], this collapses argument variants of the
/// same command family onto one key (the v0.8.37 behaviour) so that an
/// "approve for session" decision covers later invocations that differ only
/// by flags. Denials must keep using the exact [`build_approval_key`].
#[must_use]
pub fn build_approval_grouping_key(tool_name: &str, input: &serde_json::Value) -> ApprovalKey {
    let tool_name =
        codewhale_execpolicy::canonical_action::canonical_action_alias(tool_name, input);
    let fingerprint = match tool_name {
        "apply_patch" => {
            let paths_hash = hash_patch_paths(input);
            format!("patch:{paths_hash}")
        }
        "exec_shell" | "task_shell_start" => shell_command_grant_scope(input),
        // Interact and wait calls carry no command, only input for a live
        // session. Keying them on the (empty) command prefix gave every one
        // of them the same grant; a grant covers the exact call only.
        "exec_shell_wait" | "exec_shell_interact" | "exec_wait" | "exec_interact" => {
            format!("shell:{tool_name}:{}", hash_json_value(input))
        }
        "fetch_url" | "web.fetch" | "web_fetch" => {
            let host = parse_host(input);
            format!("net:{host}")
        }
        // MCP tools are reviewed as kinds: a trusted plugin bundle's MCP
        // tools were human-reviewed at trust time, so the session grant the
        // approval card offers (`2` — "approves for the session") is the
        // reviewed kind, `mcp:<tool>`. Hashing the full params here would
        // make every exact-argument variant its own family and silently
        // narrow the granted kind into a one-call grant (the regression the
        // plugin e2e acceptance catches). Shell keeps its command-family
        // key (R2); this arm never widens shell or file tools.
        //
        // Computer Use consent and `app_script` are the exception (K1/K2):
        // a kind grant there would let one approval cover every app or every
        // script, so their session grant is the exact call.
        name if computer_use_user_gate(name, input).is_some() => {
            format!("cu:{name}:{}", hash_json_value(input))
        }
        name if crate::mcp::McpPool::is_mcp_tool(name) => format!("mcp:{name}"),
        // E1: a session grant for web browsing covers the argument class the
        // person approved (search, open, …), not the one exact query.
        "web.run" => format!("web:{tool_name}:{}", web_run_action_class(input)),
        "web_search" => format!("web:{tool_name}"),
        _ => format!("tool:{tool_name}:{}", hash_json_value(input)),
    };
    ApprovalKey(fingerprint)
}

/// Exact and grouping keys for one call, as the engine puts them on an
/// approval request. A tool with an [`approval_scope`] (extension tools) is
/// keyed `<scope>:<tool_name>:<hash of input>` for both, so its grants are
/// bound to the reviewed plugin build and never widened to a family; every
/// other tool keeps [`build_approval_key`] / [`build_approval_grouping_key`].
/// The registry's captured owning agent prefixes both keys with `agent:<id>:`,
/// so its grants and denials never cover a parent or sibling's call.
///
/// [`approval_scope`]: crate::tools::spec::ToolSpec::approval_scope
#[must_use]
pub fn approval_keys_for_call(
    registry: Option<&crate::tools::ToolRegistry>,
    tool_name: &str,
    input: &serde_json::Value,
) -> (ApprovalKey, ApprovalKey) {
    let scope = registry
        .and_then(|registry| registry.get(tool_name))
        .and_then(|tool| tool.approval_scope());
    let keys = match scope {
        Some(scope) => {
            let key = ApprovalKey(format!("{scope}:{tool_name}:{}", hash_json_value(input)));
            (key.clone(), key)
        }
        None => (
            build_approval_key(tool_name, input),
            build_approval_grouping_key(tool_name, input),
        ),
    };
    if let Some(owner) = registry.and_then(|registry| registry.context().owner_agent_id.as_deref())
    {
        let scoped = |key: ApprovalKey| ApprovalKey(format!("agent:{owner}:{}", key.0));
        (scoped(keys.0), scoped(keys.1))
    } else {
        keys
    }
}

/// [`approval_keys_for_call`] for a call an extension made through `core/call`
/// (`scope` is the extension plugin build's [`approval_scope`]): both keys are
/// prefixed `extcall:<scope>:`, so a session grant or a denial recorded for the
/// model's call of a tool never matches the extension's call of it, and the
/// reverse, and neither crosses to another plugin build.
///
/// [`approval_scope`]: crate::tools::spec::ToolSpec::approval_scope
#[must_use]
pub fn extension_origin_approval_keys(
    scope: &str,
    registry: Option<&crate::tools::ToolRegistry>,
    tool_name: &str,
    input: &serde_json::Value,
) -> (ApprovalKey, ApprovalKey) {
    let (exact, grouping) = approval_keys_for_call(registry, tool_name, input);
    let scoped = |key: ApprovalKey| ApprovalKey(format!("extcall:{scope}:{}", key.0));
    (scoped(exact), scoped(grouping))
}

/// The sorted `web.run` action kinds present in `input`, e.g. `open+search_query`.
fn web_run_action_class(input: &Value) -> String {
    const ACTIONS: [&str; 6] = [
        "click",
        "find",
        "image_query",
        "open",
        "screenshot",
        "search_query",
    ];
    let present: Vec<String> = ACTIONS
        .into_iter()
        .filter(|action| input.get(*action).is_some_and(|value| !value.is_null()))
        .map(|action| {
            if action == "open" {
                format!("open({})", web_run_open_targets(input))
            } else {
                action.to_string()
            }
        })
        .collect();
    if present.is_empty() {
        "none".to_string()
    } else {
        present.join("+")
    }
}

/// The sorted target set of a `web.run` `open`: the host of each raw URL, or
/// `ref` for a result reference. `open` fetches any raw URL it is given, and a
/// URL can carry local data out in its path or query, so an "open" grant
/// covers the hosts the person approved — as `fetch_url` grants do — never
/// every host.
fn web_run_open_targets(input: &Value) -> String {
    let mut targets: Vec<String> = input
        .get("open")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| {
            let ref_id = item.get("ref_id").and_then(Value::as_str).unwrap_or("");
            if ref_id.starts_with("http://") || ref_id.starts_with("https://") {
                reqwest::Url::parse(ref_id)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                    .unwrap_or_else(|| format!("url:{}", hash_json_value(item)))
            } else {
                "ref".to_string()
            }
        })
        .collect();
    targets.sort_unstable();
    targets.dedup();
    targets.join(",")
}

/// A Computer Use call whose approval must come from a person (K1 / K2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComputerUseUserGate {
    /// A consent ledger write that widens what the model may do: `allow`, or
    /// `revoke` (which can clear a persisted deny).
    Consent {
        action: &'static str,
        app: Option<String>,
        bundle_id: Option<String>,
        scope: &'static str,
        remember: bool,
        /// An `allow` carrying a plugin `confirm` token: the person is
        /// confirming an irreversible action (pay, buy, send, transfer,
        /// delete) the plugin paused on, not consenting to an app.
        confirm: bool,
    },
    /// `app_script`: arbitrary AppleScript/JXA through osascript.
    AppScript {
        language: &'static str,
        script_sha256: String,
        first_line: String,
        /// Non-empty lines in the script, so the card can say how much is
        /// not shown by `first_line`.
        line_count: usize,
    },
    /// Registering or spawning a computer the plugin will then drive (and,
    /// for ssh, push an agent to).
    Computer {
        action: &'static str,
        transport: Option<String>,
        /// `user@host:port` for ssh, the target or image otherwise.
        destination: Option<String>,
    },
}

/// Classify an MCP tool call as a Computer Use call that needs a human
/// decision. See the module docs for the matching rule and its limits.
#[must_use]
pub(crate) fn computer_use_user_gate(
    tool_name: &str,
    input: &Value,
) -> Option<ComputerUseUserGate> {
    if !tool_name.starts_with("mcp_") {
        return None;
    }
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let computer_action = if tool_name.ends_with("_computer_register") {
        Some("register")
    } else if tool_name.ends_with("_computer_spawn") {
        Some("spawn")
    } else if tool_name.ends_with("_computer") {
        match input.get("action").and_then(Value::as_str) {
            Some("register") => Some("register"),
            Some("spawn") => Some("spawn"),
            _ => None,
        }
    } else {
        None
    };
    if let Some(action) = computer_action {
        let host = text("host");
        let destination = match host {
            Some(host) => {
                let user = text("user")
                    .map(|user| format!("{user}@"))
                    .unwrap_or_default();
                let port = input
                    .get("port")
                    .and_then(Value::as_u64)
                    .map(|port| format!(":{port}"))
                    .unwrap_or_default();
                Some(format!("{user}{host}{port}"))
            }
            None => text("target").or_else(|| text("image")),
        };
        return Some(ComputerUseUserGate::Computer {
            action,
            transport: text("transport"),
            destination,
        });
    }
    let action = if tool_name.ends_with("_consent_allow") {
        "allow"
    } else if tool_name.ends_with("_consent_revoke") {
        "revoke"
    } else if tool_name.ends_with("_consent") {
        match input.get("action").and_then(Value::as_str) {
            Some("allow") => "allow",
            Some("revoke") => "revoke",
            // `status` reads; `deny` only narrows what the model may do.
            _ => return None,
        }
    } else if tool_name.ends_with("_app_script") {
        let script = input.get("script").and_then(Value::as_str).unwrap_or("");
        let language = match input.get("language").and_then(Value::as_str) {
            Some("javascript") => "JXA",
            _ => "AppleScript",
        };
        let digest = Sha256::digest(script.as_bytes());
        let mut script_sha256 = String::with_capacity(64);
        for byte in digest {
            write!(&mut script_sha256, "{byte:02x}").expect("writing to String cannot fail");
        }
        let first_line = script
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        let line_count = script
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        return Some(ComputerUseUserGate::AppScript {
            language,
            script_sha256,
            first_line,
            line_count,
        });
    } else {
        return None;
    };
    let pid = input
        .get("pid")
        .and_then(Value::as_i64)
        .map(|pid| format!("pid:{pid}"));
    Some(ComputerUseUserGate::Consent {
        action,
        app: text("app").or_else(|| text("name")).or(pid),
        bundle_id: text("bundle_id"),
        scope: if input.get("scope").and_then(Value::as_str) == Some("foreground") {
            "foreground"
        } else {
            "app"
        },
        remember: input.get("remember").and_then(Value::as_bool) == Some(true),
        confirm: action == "allow" && text("confirm").is_some(),
    })
}

/// Whether this call exists only to carry a person's own answer: Computer Use
/// consent, a consent revoke, or an app script. A posture may supply an
/// allowance, but it can never supply an answer.
#[must_use]
pub(crate) fn computer_use_human_decision_tool(tool_name: &str) -> bool {
    tool_name.starts_with("mcp_")
        && [
            "_consent",
            "_consent_allow",
            "_consent_revoke",
            "_app_script",
        ]
        .iter()
        .any(|suffix| tool_name.ends_with(suffix))
}

/// The inner tool of the first `run_actions` step that would need a human
/// decision (K1 / K2). Computer Use `run_actions` accepts any plugin tool name
/// as a step — including the unlisted `consent_allow` / `consent_revoke` and
/// `app_script` — so a batch would otherwise carry a consent grant or a
/// script past the per-call card. Engine preparation refuses such a batch.
#[must_use]
pub(crate) fn computer_use_batch_hidden_gate(tool_name: &str, input: &Value) -> Option<String> {
    if !tool_name.starts_with("mcp_") || !tool_name.ends_with("_run_actions") {
        return None;
    }
    input
        .get("steps")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|step| {
            let inner = step.get("tool").and_then(Value::as_str)?;
            let arguments = step.get("arguments").cloned().unwrap_or(Value::Null);
            computer_use_user_gate(&format!("mcp_{inner}"), &arguments).map(|_| inner.to_string())
        })
}

/// The session-grant scope for a shell command.
///
/// A simple command whose family is in the arity dictionary keeps the
/// family grant, so approving `git status` also covers `git status -s`
/// without covering `git push`. Everything else fails closed to the full
/// normalized command:
///
/// * compound commands (`;`, `&&`, `|`, redirects, substitutions, `$VAR`):
///   a grant for `cd` must not extend to whatever is chained after it;
/// * wrappers and interpreters (`bash -c`, `env`, `sudo`, `xargs`,
///   `python -c`, `docker run`, …): the first word says nothing about what
///   runs;
/// * commands the dictionary does not know, which used to collapse to their
///   first word, so one approval covered every use of that program.
fn shell_command_grant_scope(input: &serde_json::Value) -> String {
    let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    if tokens.is_empty() {
        return "shell:<empty>".to_string();
    }
    if !shell_command_is_compound(cmd)
        && !shell_command_is_wrapper(&tokens)
        && !shell_command_has_code_option(&tokens)
    {
        let family = classify_command(&tokens);
        // Options wedged before a subcommand (`git -c k=v status`), a chain,
        // or code that runs nested or resolves only at run time also keep
        // the grant to this exact command (#6675).
        let expansion = codewhale_execpolicy::shell_expand::expand_command(cmd);
        if command_family_is_known(&family)
            && !family_arguments_are_config(&family)
            && !family_arguments_are_code(&family)
            && !expansion.dynamic
            && !expansion.nested
            && expansion.commands.len() == 1
            && canonical_prefix_is_leading(&tokens, &family)
        {
            return format!("shell:{family}");
        }
    }
    format!("shell:cmd:{}", normalize_shell_command(cmd))
}

/// Whether any option could change what a known command runs, reads or
/// writes. The family ignores flags, so a family grant is kept only when
/// every option is on [`INERT_OPTIONS`]; anything else keys the grant on the
/// full command.
///
/// An allow-list, not a deny-list: options take values in too many spellings
/// (`-C../x`, `-f/tmp/x`, `--output out.patch`, `-x "cmd"`, `-exec ./x`,
/// `-O<cmd>`) for a list of dangerous ones to be complete.
fn shell_command_has_code_option(tokens: &[&str]) -> bool {
    tokens.iter().skip(1).any(|token| {
        token.contains('=') || (token.starts_with('-') && !INERT_OPTIONS.contains(token))
    })
}

/// Value-free options that only change how much a command prints or which of
/// its own outputs it builds, never what it runs or where it writes.
const INERT_OPTIONS: &[&str] = &[
    "-h",
    "--help",
    "-V",
    "--version",
    "-v",
    "-vv",
    "--verbose",
    "-q",
    "--quiet",
    "-s",
    "--short",
    "--porcelain",
    "--oneline",
    "--stat",
    "--name-only",
    "--name-status",
    "--cached",
    "--staged",
    "--no-color",
    "--release",
    "-p",
    "--package",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--lib",
    "--bins",
    "--tests",
    "--locked",
    "--frozen",
    "--offline",
    "--no-fail-fast",
    "--dry-run",
];

/// Families whose positional arguments beyond the family are themselves what
/// runs or gets installed (`go run <file>`, `git bisect run <cmd>`,
/// `make <target>`, `npm install <pkg>`), so one grant would cover any of
/// them.
fn family_arguments_are_code(family: &str) -> bool {
    const FAMILIES: &[&str] = &[
        "make",
        "go run",
        "go install",
        "go get",
        "go generate",
        "deno run",
        "bun run",
        "cargo run",
        "cargo install",
        "cargo add",
        "git bisect",
        "git submodule",
        "npm install",
        "yarn add",
        "pnpm add",
        "bun add",
        "pip install",
        "pip3 install",
        "docker compose run",
        "docker compose exec",
        "docker container run",
        "docker container exec",
    ];
    FAMILIES.contains(&family)
}

/// Families whose arguments are settings, so one grant would cover every
/// setting (`git config user.name x` covering `git config core.fsmonitor`).
fn family_arguments_are_config(family: &str) -> bool {
    matches!(family.split(' ').nth(1), Some("config" | "set" | "remote"))
}

/// Whether the dictionary recognised `family`, rather than falling back to
/// the bare first word.
fn command_family_is_known(family: &str) -> bool {
    use codewhale_execpolicy::command_safety::COMMAND_ARITY;
    COMMAND_ARITY
        .iter()
        .any(|(key, _)| family == *key || family.starts_with(&format!("{key} ")))
}

/// Any shell syntax that chains, redirects, substitutes, or expands.
fn shell_command_is_compound(cmd: &str) -> bool {
    cmd.contains(|c: char| {
        matches!(
            c,
            ';' | '&' | '|' | '<' | '>' | '`' | '$' | '(' | ')' | '{' | '}' | '\n' | '\r'
        )
    })
}

/// Commands whose first word runs something else the grant cannot see.
fn shell_command_is_wrapper(tokens: &[&str]) -> bool {
    const WRAPPERS: &[&str] = &[
        "bash",
        "sh",
        "zsh",
        "dash",
        "ksh",
        "fish",
        "csh",
        "tcsh",
        "env",
        "sudo",
        "doas",
        "su",
        "xargs",
        "nohup",
        "time",
        "timeout",
        "nice",
        "ionice",
        "exec",
        "eval",
        "command",
        "builtin",
        "stdbuf",
        "script",
        "watch",
        "parallel",
        "chroot",
        "nsenter",
        "unshare",
        "setsid",
        "caffeinate",
        "strace",
        "ltrace",
        "gdb",
        "lldb",
        "osascript",
        "pwsh",
        "powershell",
        "cmd",
        "npx",
        "pnpx",
        "bunx",
        "uvx",
        "node",
        "perl",
        "ruby",
        "php",
        "python",
        "python2",
        "python3",
        "find",
        "ssh",
    ];
    const RUNNERS: &[&str] = &[
        "docker run",
        "docker exec",
        "kubectl exec",
        "npm exec",
        "pnpm exec",
        "pnpm dlx",
        "yarn dlx",
        "uv run",
        "poetry run",
    ];
    let first = tokens[0];
    // `FOO=1 cmd`: an environment assignment can change what `cmd` does.
    if first.contains('=') {
        return true;
    }
    let program = first.rsplit('/').next().unwrap_or(first);
    if WRAPPERS.contains(&program) {
        return true;
    }
    let lead = tokens
        .iter()
        .take(2)
        .map(|token| token.rsplit('/').next().unwrap_or(token))
        .collect::<Vec<_>>()
        .join(" ");
    RUNNERS.contains(&lead.as_str())
}

/// Collapse insignificant whitespace. Quoted text keeps its exact spacing:
/// `echo "a  b"` and `echo "a b"` are different commands.
fn normalize_shell_command(cmd: &str) -> String {
    if cmd.contains(['"', '\'', '\\']) {
        cmd.trim().to_string()
    } else {
        cmd.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// Hash the sorted set of file paths referenced by a patch input.
///
/// The paths come from [`preflight_apply_patch`] — the same resolver the
/// executor, the permission path (`core/engine.rs`) and auto-review already
/// use — rather than from a second, weaker parser. That matters because this
/// string *is* the scope of an "approve for the session" grant: two patches
/// share a grant exactly when they share this key.
///
/// The previous implementation read only `+++ b/` headers and the
/// `replace`/`changes` array, so it saw no paths at all for the documented
/// `apply_patch{path, patch}` override, for `--no-prefix` diffs, or for
/// delete-only diffs — and collapsed all of them to one shared constant.
/// Approving any one of those pre-approved every later one, to any file
/// (#6247).
///
/// An input the resolver cannot parse gets a digest of the input itself, not
/// a shared constant: an unparseable patch is its own family and matches
/// nothing but a byte-identical repeat.
fn hash_patch_paths(input: &serde_json::Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let Ok(preflight) = codewhale_tools::patch_preflight::preflight_apply_patch(input) else {
        return format!("unparsed_{}", hash_json_value(input));
    };

    let mut paths: Vec<&str> = preflight.touched_files.iter().map(String::as_str).collect();

    paths.sort_unstable();
    paths.dedup();

    if paths.is_empty() {
        // The resolver parsed the input but found no target. Fail closed for
        // the same reason as the error arm above: a shared key here is a
        // shared grant.
        return format!("no_target_{}", hash_json_value(input));
    }

    let mut hasher = DefaultHasher::new();
    for path in &paths {
        path.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

/// Parse the host portion from a URL input.
fn parse_host(input: &serde_json::Value) -> String {
    let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");

    if let Ok(parsed) = reqwest::Url::parse(url) {
        parsed.host_str().unwrap_or(url).to_string()
    } else {
        url.to_string()
    }
}

fn hash_json_value(value: &Value) -> String {
    let mut canonical = String::new();
    push_canonical_json(value, &mut canonical);

    let digest = Sha256::digest(canonical.as_bytes());
    let mut short = String::with_capacity(16);
    for byte in &digest[..8] {
        write!(&mut short, "{byte:02x}").expect("writing to String cannot fail");
    }
    short
}

/// Maximum nesting depth the canonical serializer descends. Aligned with
/// serde_json's own parse limit so parsed input never truncates; anything
/// deeper emits a fixed marker, keeping keys deterministic.
const MAX_CANONICAL_JSON_DEPTH: usize = 128;

fn push_canonical_json(value: &Value, out: &mut String) {
    push_canonical_json_at(value, out, 0)
}

fn push_canonical_json_at(value: &Value, out: &mut String, depth: usize) {
    if depth > MAX_CANONICAL_JSON_DEPTH {
        out.push_str("maxdepth");
        return;
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => {
            out.push_str("bool:");
            out.push_str(if *value { "true" } else { "false" });
        }
        Value::Number(value) => {
            out.push_str("number:");
            // Avoid allocating via value.to_string().
            if let Some(n) = value.as_f64() {
                let _ = write!(out, "{n}");
            } else if let Some(n) = value.as_i64() {
                let _ = write!(out, "{n}");
            } else if let Some(n) = value.as_u64() {
                let _ = write!(out, "{n}");
            } else {
                out.push_str(&value.to_string());
            }
        }
        Value::String(value) => {
            out.push_str("string:");
            // Emit JSON-encoded string without an intermediate allocation.
            out.push('"');
            for ch in value.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c.is_control() => {
                        let _ = write!(out, "\\u{:04x}", c as u32);
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_canonical_json_at(item, out, depth + 1);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);

            out.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let encoded_key =
                    serde_json::to_string(key).expect("serializing an object key cannot fail");
                out.push_str(&encoded_key);
                out.push(':');
                push_canonical_json_at(value, out, depth + 1);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn different_commands_different_keys() {
        let key_a = build_approval_key("exec_shell", &json!({"command": "ls"}));
        let key_b = build_approval_key("exec_shell", &json!({"command": "rm -rf /tmp"}));
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn same_command_same_key() {
        let key_a = build_approval_key("exec_shell", &json!({"command": "cargo build --release"}));
        let key_b = build_approval_key("exec_shell", &json!({"command": "cargo build --release"}));
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn pathological_nesting_yields_a_stable_key() {
        let mut value = Value::String("leaf".to_string());
        for _ in 0..150 {
            let mut map = serde_json::Map::new();
            map.insert("t".to_string(), value);
            value = Value::Object(map);
        }
        let key_a = build_approval_key("exec_shell", &value);
        let key_b = build_approval_key("exec_shell", &value);
        assert_eq!(key_a, key_b, "truncated keys must stay deterministic");
    }

    #[test]
    fn shell_keys_include_full_command_arguments() {
        let key_a = build_approval_key("exec_shell", &json!({"command": "cargo build"}));
        let key_b = build_approval_key("exec_shell", &json!({"command": "cargo build --release"}));
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn grouping_key_collapses_shell_flag_variants() {
        let key_a = build_approval_grouping_key("exec_shell", &json!({"command": "cargo build"}));
        let key_b =
            build_approval_grouping_key("exec_shell", &json!({"command": "cargo build --release"}));
        assert_eq!(
            key_a, key_b,
            "approving a command family must cover later flag variants"
        );
    }

    #[test]
    fn shell_grants_fail_closed_on_compound_wrapper_and_unknown_commands() {
        let key = |cmd: &str| build_approval_grouping_key("exec_shell", &json!({"command": cmd}));
        // Compound: a grant for `cd` never covers what is chained after it.
        assert_ne!(key("cd src"), key("cd src && make clean"));
        assert_ne!(key("cargo build"), key("cargo build; make install"));
        assert_ne!(key("cargo build"), key("cargo build > out.log"));
        assert_ne!(key("git status"), key("git status $(pwd)"));
        // Wrappers and interpreters are keyed by the whole command.
        assert_ne!(key("bash build.sh"), key("bash -c 'make clean'"));
        assert_ne!(key("python3 script.py"), key("python3 -c 'import os'"));
        assert_ne!(key("env FOO=1 make"), key("env FOO=1 make clean"));
        assert_ne!(key("FOO=1 make"), key("FOO=1 make install"));
        assert_ne!(
            key("docker run alpine ls"),
            key("docker run alpine touch x")
        );
        assert_ne!(key("/usr/bin/sudo ls"), key("/usr/bin/sudo make install"));
        // Unknown commands no longer collapse to their first word.
        assert_ne!(key("rm tmp/x"), key("rm -r build"));
        assert_ne!(key("cat README.md"), key("cat notes/other.txt"));
        // The full-command key still matches an exact repeat, modulo
        // insignificant whitespace, but not a quoted-spacing change.
        assert_eq!(key("cd src && make"), key("  cd src   &&  make "));
        assert_ne!(key("echo \"a  b\""), key("echo \"a b\""));
        assert!(key("rm -r build").0.starts_with("shell:cmd:"));
        // Options and config arguments that can run code key the full
        // command, though the family ignores flags.
        assert_ne!(key("git status"), key("git -ccore.fsmonitor=./hook status"));
        assert_ne!(key("git status"), key("git -C ../other status"));
        assert_ne!(
            key("cargo build"),
            key("cargo build --config build.rustc-wrapper=./x")
        );
        assert_ne!(key("git diff"), key("git diff --output=out.patch"));
        assert_ne!(key("make"), key("make -f /tmp/x"));
        assert_ne!(
            key("git config user.name x"),
            key("git config core.fsmonitor ./x.sh")
        );
        // A known, simple command keeps its family grant.
        assert_eq!(key("git status"), key("git status --porcelain"));
        assert_ne!(key("git status"), key("git push"));
    }

    #[test]
    fn shell_family_grants_survive_only_inert_options() {
        let key = |cmd: &str| build_approval_grouping_key("exec_shell", &json!({"command": cmd}));
        // Options with an attached or separate value, and single-dash long
        // options, key the full command.
        for (granted, other) in [
            ("git status", "git -C../other status"),
            ("make", "make -f/tmp/x"),
            ("git diff", "git diff --output out.patch"),
            ("git rebase HEAD~1", "git rebase -x \"touch x\" HEAD~1"),
            ("go test ./...", "go test -exec ./x ./..."),
            ("go build ./...", "go build -toolexec ./x ./..."),
            ("git clone ./repo", "git clone -u \"cmd\" ./repo"),
            ("git grep pat", "git grep -Ocmd pat"),
            ("git log", "git log --git-dir x"),
            ("cargo test", "cargo test -- --nocapture"),
        ] {
            assert_ne!(key(granted), key(other), "{other}");
            assert!(key(other).0.starts_with("shell:cmd:"), "{other}");
        }
        // Every option once listed as able to run code still keys the full
        // command.
        for option in [
            "-c",
            "-C",
            "--config",
            "--exec",
            "--exec-path",
            "--script-shell",
            "-f",
            "--file",
            "--makefile",
            "-e",
            "--eval",
            "--require",
            "--upload-pack",
            "--receive-pack",
            "--manifest-path",
        ] {
            assert!(!INERT_OPTIONS.contains(&option), "{option}");
        }
        // Families whose arguments are the code that runs or is installed
        // key the full command.
        for (granted, other) in [
            ("go run ./cmd/tool", "go run /tmp/other.go"),
            ("deno run main.ts", "deno run https://host/x.ts"),
            ("cargo run", "cargo run --bin other"),
            ("git bisect start", "git bisect run ./x"),
            ("git submodule update", "git submodule foreach ./x"),
            ("make build", "make clean"),
            ("npm install", "npm install left-pad"),
            ("pip install requests", "pip install other"),
            ("cargo install ripgrep", "cargo install other"),
        ] {
            assert_ne!(key(granted), key(other), "{other}");
        }
        // Inert options keep the family grant.
        assert_eq!(key("cargo build"), key("cargo build --release --locked"));
        assert_eq!(key("git status"), key("git status -s"));
        assert_eq!(key("git diff"), key("git diff --stat --cached"));
    }

    #[test]
    fn shell_interact_grants_are_per_exact_call() {
        for tool in [
            "exec_shell_interact",
            "exec_interact",
            "exec_shell_wait",
            "exec_wait",
        ] {
            let a = build_approval_grouping_key(tool, &json!({"task_id": "t1", "input": "y\n"}));
            let b = build_approval_grouping_key(tool, &json!({"task_id": "t1", "input": "n\n"}));
            let c = build_approval_grouping_key(tool, &json!({"task_id": "t2", "input": "y\n"}));
            assert_ne!(a, b, "{tool}: different input must not share a grant");
            assert_ne!(a, c, "{tool}: a different session must not share a grant");
            assert_eq!(
                a,
                build_approval_grouping_key(tool, &json!({"task_id": "t1", "input": "y\n"}))
            );
        }
    }

    #[test]
    fn grouping_key_grants_mcp_tools_as_reviewed_kinds() {
        // A session grant for a reviewed plugin MCP tool is the kind
        // (`mcp:<tool>`), not the exact arguments: the plugin e2e acceptance
        // approves the echo kind once and later variants of the same reviewed
        // tool must not re-prompt. R2's shell command-family scoping is
        // untouched — this is the MCP arm only.
        let key_a = build_approval_grouping_key(
            "mcp_plugin-4-demo-local_echo",
            &json!({"text": "acceptance", "hang": false}),
        );
        let key_b = build_approval_grouping_key(
            "mcp_plugin-4-demo-local_echo",
            &json!({"text": "acceptance", "hang": true}),
        );
        assert_eq!(
            key_a, key_b,
            "a reviewed MCP kind grant covers argument variants of that tool"
        );
        let key_c = build_approval_grouping_key("mcp_plugin-4-demo-local_kick", &json!({"x": 1}));
        assert_ne!(key_a, key_c, "a different MCP tool is a different kind");
        // The exact-call key stays per-arguments so denials still suppress
        // only exact retries.
        let exact_a = build_approval_key(
            "mcp_plugin-4-demo-local_echo",
            &json!({"text": "acceptance", "hang": false}),
        );
        let exact_b = build_approval_key(
            "mcp_plugin-4-demo-local_echo",
            &json!({"text": "acceptance", "hang": true}),
        );
        assert_ne!(exact_a, exact_b, "denial keys remain argument-exact");
    }

    /// K1: one session grant for Computer Use consent must not cover a
    /// consent request for a different app, a different scope, or a
    /// persisted (`remember`) variant — the MCP kind grant is not used here.
    #[test]
    fn computer_use_consent_grants_are_per_exact_call_not_per_kind() {
        let tool = "mcp_plugin-12-computer-use-computer_consent";
        let safari = build_approval_grouping_key(
            tool,
            &json!({"action": "allow", "app": "Safari", "bundle_id": "com.apple.Safari"}),
        );
        let terminal = build_approval_grouping_key(
            tool,
            &json!({"action": "allow", "app": "Terminal", "bundle_id": "com.apple.Terminal"}),
        );
        let foreground =
            build_approval_grouping_key(tool, &json!({"action": "allow", "scope": "foreground"}));
        let persisted = build_approval_grouping_key(
            tool,
            &json!({"action": "allow", "app": "Safari", "bundle_id": "com.apple.Safari", "remember": true}),
        );
        assert_ne!(safari, terminal, "allowing app X must never allow app Y");
        assert_ne!(safari, foreground);
        assert_ne!(safari, persisted);
        assert!(safari.0.starts_with("cu:"), "{safari:?}");
        for name in [
            "mcp_codewhale-cu_consent_allow",
            "mcp_codewhale-cu_consent_revoke",
        ] {
            let a = build_approval_grouping_key(name, &json!({"app": "Safari"}));
            let b = build_approval_grouping_key(name, &json!({"app": "Terminal"}));
            assert_ne!(a, b, "{name}");
        }
        // Reads and self-narrowing decisions keep the ordinary kind grant.
        assert_eq!(
            build_approval_grouping_key(tool, &json!({"action": "status"})).0,
            format!("mcp:{tool}")
        );
        assert!(
            computer_use_user_gate(tool, &json!({"action": "deny", "app": "Safari"})).is_none()
        );
        assert!(computer_use_user_gate("mcp_codewhale-cu_consent_status", &json!({})).is_none());
        assert!(computer_use_user_gate("consent_allow", &json!({"app": "Safari"})).is_none());
    }

    /// K2: an `app_script` session grant is the exact script; a changed
    /// script is a new approval.
    #[test]
    fn app_script_grants_are_per_exact_script() {
        let tool = "mcp_plugin-12-computer-use-computer_app_script";
        let a = build_approval_grouping_key(
            tool,
            &json!({"script": "tell application \"Finder\" to get name of front window"}),
        );
        let same = build_approval_grouping_key(
            tool,
            &json!({"script": "tell application \"Finder\" to get name of front window"}),
        );
        let changed =
            build_approval_grouping_key(tool, &json!({"script": "do shell script \"id\""}));
        assert_eq!(a, same);
        assert_ne!(a, changed, "a changed script must prompt again");
        let Some(ComputerUseUserGate::AppScript {
            language,
            script_sha256,
            first_line,
            line_count,
        }) = computer_use_user_gate(
            tool,
            &json!({"script": "\n  ObjC.import('Foundation')\nrest", "language": "javascript"}),
        )
        else {
            panic!("app_script must be gated");
        };
        assert_eq!(language, "JXA");
        assert_eq!(script_sha256.len(), 64);
        assert_eq!(first_line, "ObjC.import('Foundation')");
        assert_eq!(line_count, 2);
    }

    #[test]
    fn grouping_key_still_separates_distinct_commands() {
        let key_a = build_approval_grouping_key("exec_shell", &json!({"command": "git status"}));
        let key_b = build_approval_grouping_key("exec_shell", &json!({"command": "git push"}));
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn grouping_key_does_not_cover_interposed_options_chains_or_nested_code() {
        let key =
            |command: &str| build_approval_grouping_key("exec_shell", &json!({"command": command}));
        let granted = key("git status");
        assert_eq!(granted, key("git status -s"));
        for command in [
            "git --git-dir=/tmp/e/.git status",
            "git --exec-path=/x status",
            "git status $(touch p)",
            "git status && rm x",
        ] {
            assert_ne!(granted, key(command), "{command}");
        }
        // Such a command still matches an identical repeat, and only that.
        assert_eq!(key("git status && ls"), key("git status && ls"));
        assert_ne!(key("git status && ls"), key("git status && pwd"));
    }

    /// #6247. The `path` override is the documented way to patch without
    /// diff headers (`apply_patch.rs` tells the model "Ensure the patch
    /// includes ---/+++ headers or provide `path`"), and a bare hunk has no
    /// `+++` line at all. Before the fix both of these produced the constant
    /// `patch:no_files`, so one session grant covered every later one.
    #[test]
    fn grouping_key_scopes_a_path_override_to_its_own_file() {
        let hunk = "@@ -1 +1 @@\n-old\n+new\n";
        let benign = build_approval_grouping_key(
            "apply_patch",
            &json!({"path": ".env.example", "patch": hunk}),
        );
        let sensitive = build_approval_grouping_key(
            "apply_patch",
            &json!({"path": ".codewhale/settings.json", "patch": hunk}),
        );
        assert_ne!(
            benign, sensitive,
            "approving a patch to one file must never cover a patch to another"
        );
        assert!(
            !format!("{benign:?}").contains("no_files"),
            "a resolvable target must never collapse to the shared constant"
        );
    }

    /// The executor's `normalize_diff_path` accepts a prefix-less header, so
    /// the fingerprint must too — otherwise a `--no-prefix` diff is a second
    /// route to the shared key.
    #[test]
    fn grouping_key_reads_prefix_less_diff_headers() {
        let prefixed = build_approval_grouping_key(
            "apply_patch",
            &json!({"patch": "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -1 +1 @@\n-a\n+b\n"}),
        );
        let bare = build_approval_grouping_key(
            "apply_patch",
            &json!({"patch": "--- src/auth.rs\n+++ src/auth.rs\n@@ -1 +1 @@\n-a\n+b\n"}),
        );
        assert_eq!(
            prefixed, bare,
            "the same target written two legal ways is one approval family"
        );
        let other = build_approval_grouping_key(
            "apply_patch",
            &json!({"patch": "--- src/billing.rs\n+++ src/billing.rs\n@@ -1 +1 @@\n-a\n+b\n"}),
        );
        assert_ne!(bare, other, "different targets are different families");
    }

    /// Fail closed: an input the resolver cannot parse is its own family, not
    /// a member of a shared one. Two different unparseable inputs must not
    /// share a grant.
    #[test]
    fn grouping_key_fails_closed_on_an_unresolvable_patch() {
        let a = build_approval_grouping_key("apply_patch", &json!({"patch": "not a diff at all"}));
        let b = build_approval_grouping_key("apply_patch", &json!({"patch": "also not a diff"}));
        assert_ne!(a, b, "unparseable inputs must not share an approval family");
        for key in [&a, &b] {
            let rendered = format!("{key:?}");
            assert!(
                !rendered.contains("no_files"),
                "the shared constant must not survive anywhere: {rendered}"
            );
        }
    }

    #[test]
    fn grouping_key_collapses_patch_body_for_same_path() {
        let key_a = build_approval_grouping_key(
            "apply_patch",
            &json!({"replace": [{"path": "a.rs", "content": "x"}]}),
        );
        let key_b = build_approval_grouping_key(
            "apply_patch",
            &json!({"replace": [{"path": "a.rs", "content": "y"}]}),
        );
        assert_eq!(
            key_a, key_b,
            "approving a patch family must cover later edits to the same path"
        );
    }

    #[test]
    fn grouping_key_treats_replace_and_legacy_changes_as_the_same_path_set() {
        let canonical = build_approval_grouping_key(
            "apply_patch",
            &json!({"replace": [{"path": "a.rs", "content": "new"}]}),
        );
        let legacy = build_approval_grouping_key(
            "apply_patch",
            &json!({"changes": [{"path": "a.rs", "content": "new"}]}),
        );

        assert_eq!(canonical, legacy);
    }

    #[test]
    fn denial_key_stays_exact_while_grouping_key_collapses() {
        let exact_a = build_approval_key("exec_shell", &json!({"command": "cargo build"}));
        let exact_b =
            build_approval_key("exec_shell", &json!({"command": "cargo build --release"}));
        assert_ne!(exact_a, exact_b, "denials must remain exact-call scoped");

        let group_a = build_approval_grouping_key("exec_shell", &json!({"command": "cargo build"}));
        let group_b =
            build_approval_grouping_key("exec_shell", &json!({"command": "cargo build --release"}));
        assert_eq!(group_a, group_b, "approvals must group by command family");
    }

    #[test]
    fn patch_keys_differ_by_path() {
        let key_a = build_approval_key(
            "apply_patch",
            &json!({"replace": [{"path": "a.rs", "content": "x"}]}),
        );
        let key_b = build_approval_key(
            "apply_patch",
            &json!({"replace": [{"path": "b.rs", "content": "x"}]}),
        );
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn patch_keys_differ_by_body_for_same_path() {
        let key_a = build_approval_key(
            "apply_patch",
            &json!({"replace": [{"path": "a.rs", "content": "x"}]}),
        );
        let key_b = build_approval_key(
            "apply_patch",
            &json!({"replace": [{"path": "a.rs", "content": "y"}]}),
        );
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn net_keys_differ_by_host() {
        let key_a = build_approval_key("fetch_url", &json!({"url": "https://example.com"}));
        let key_b = build_approval_key("fetch_url", &json!({"url": "https://other.org"}));
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn generic_tool_keys_include_arguments() {
        let key_a = build_approval_key("read_file", &json!({"path": "a.txt"}));
        let key_b = build_approval_key("read_file", &json!({"path": "b.txt"}));
        assert_ne!(key_a, key_b);
        assert!(key_a.0.starts_with("tool:read_file:"));
    }

    #[test]
    fn generic_tool_same_arguments_reuse_key() {
        let input = json!({"path": "a.txt"});
        let key_a = build_approval_key("edit_file", &input);
        let key_b = build_approval_key("edit_file", &input);
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn input_hash_is_stable_across_object_key_order() {
        let key_a = build_approval_key("write_file", &json!({"path": "a.txt", "content": "x"}));
        let key_b = build_approval_key("write_file", &json!({"content": "x", "path": "a.txt"}));
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn lowercase_primitives_share_legacy_approval_keys() {
        let shell = json!({"command": "cargo test"});
        assert_eq!(
            build_approval_key("bash", &shell),
            build_approval_key("exec_shell", &shell)
        );
        let write = json!({"path": "a.txt", "content": "x"});
        assert_eq!(
            build_approval_key("write", &write),
            build_approval_key("write_file", &write)
        );
        let edit = json!({
            "path": "a.txt",
            "edits": [{"oldText": "x", "newText": "y"}]
        });
        assert_eq!(
            build_approval_key("edit", &edit),
            build_approval_key("edit_file", &edit)
        );
    }

    #[test]
    fn canonical_json_omits_trailing_commas() {
        let mut canonical = String::new();
        push_canonical_json(&json!({"b": [true, false], "a": {"x": 1}}), &mut canonical);

        assert_eq!(
            canonical,
            r#"{"a":{"x":number:1},"b":[bool:true,bool:false]}"#
        );
        assert!(!canonical.contains(",]"));
        assert!(!canonical.contains(",}"));
    }

    #[test]
    fn web_run_session_grant_covers_its_argument_class_only() {
        let search = |q: &str| json!({"search_query": [{"q": q}]});
        assert_eq!(
            build_approval_grouping_key("web.run", &search("espresso")),
            build_approval_grouping_key("web.run", &search("grinders")),
            "approving one search covers later searches"
        );
        assert_ne!(
            build_approval_grouping_key("web.run", &search("espresso")),
            build_approval_grouping_key(
                "web.run",
                &json!({"open": [{"ref_id": "https://x.test"}]})
            ),
            "a search grant never covers opening a page"
        );
        let open = |url: &str| json!({"open": [{"ref_id": url}]});
        assert_eq!(
            build_approval_grouping_key("web.run", &open("https://docs.rs/a")),
            build_approval_grouping_key("web.run", &open("https://DOCS.rs/b?x=1")),
            "an open grant covers later pages on the approved host"
        );
        assert_ne!(
            build_approval_grouping_key("web.run", &open("https://docs.rs/a")),
            build_approval_grouping_key("web.run", &open("https://evil.test/?q=secret")),
            "an open grant never covers another host"
        );
        assert_ne!(
            build_approval_grouping_key("web.run", &open("turn0search0")),
            build_approval_grouping_key("web.run", &open("https://evil.test/")),
            "a result-reference open grant never covers a raw URL"
        );
        assert_ne!(
            build_approval_key("web.run", &search("espresso")),
            build_approval_key("web.run", &search("grinders")),
            "denials stay exact-call scoped"
        );
    }
    #[test]
    fn computer_register_and_spawn_need_a_human_card() {
        for (tool, input) in [
            (
                "mcp_codewhale-cu_computer_register",
                json!({"computer": "box", "transport": "ssh", "host": "box.example", "user": "me", "port": 2222}),
            ),
            (
                "mcp_codewhale-cu_computer",
                json!({"action": "register", "id": "box", "transport": "ssh", "host": "box.example"}),
            ),
            (
                "mcp_codewhale-cu_computer_spawn",
                json!({"image": "desktop"}),
            ),
            (
                "mcp_codewhale-cu_computer",
                json!({"action": "spawn", "id": "d"}),
            ),
        ] {
            assert!(
                matches!(
                    computer_use_user_gate(tool, &input),
                    Some(ComputerUseUserGate::Computer { .. })
                ),
                "{tool} {input}"
            );
        }
        assert_eq!(
            computer_use_user_gate(
                "mcp_codewhale-cu_computer_register",
                &json!({"transport": "ssh", "host": "box.example", "user": "me", "port": 2222}),
            ),
            Some(ComputerUseUserGate::Computer {
                action: "register",
                transport: Some("ssh".to_string()),
                destination: Some("me@box.example:2222".to_string()),
            })
        );
        for (tool, input) in [
            ("mcp_codewhale-cu_computer", json!({"action": "list"})),
            (
                "mcp_codewhale-cu_computer",
                json!({"action": "switch", "id": "box"}),
            ),
            (
                "mcp_codewhale-cu_computer_switch",
                json!({"computer": "box"}),
            ),
        ] {
            assert_eq!(computer_use_user_gate(tool, &input), None, "{tool}");
        }
        assert_eq!(
            computer_use_batch_hidden_gate(
                "mcp_codewhale-cu_run_actions",
                &json!({"steps": [{"tool": "codewhale-cu_computer_register", "arguments": {"host": "x"}}]}),
            )
            .as_deref(),
            Some("codewhale-cu_computer_register")
        );
    }
}
