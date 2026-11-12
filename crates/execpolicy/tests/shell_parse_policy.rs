//! Deny rules hold against commands whose word the shell resolves at run
//! time, and allow rules only cover the command as written.

use codewhale_execpolicy::{
    AskForApproval, ExecApprovalRequirement, ExecPolicyContext, ExecPolicyEngine, PermissionAction,
    Ruleset, ToolAskRule,
    bash_arity::BashArityDict,
    command_safety::{is_agent_readonly_shell_command, is_parallel_readonly_command},
    toml_rules::{ExecPolicyConfig, RuleDecision},
};

fn context(command: &str, approval: AskForApproval) -> ExecPolicyContext<'_> {
    ExecPolicyContext {
        command,
        cwd: "/workspace",
        tool: Some("exec_shell"),
        path: None,
        ask_for_approval: approval,
        sandbox_mode: None,
    }
}

fn deny_rm_engines() -> [ExecPolicyEngine; 2] {
    [
        ExecPolicyEngine::new(vec![], vec!["rm".to_string()]),
        ExecPolicyEngine::with_rulesets(vec![Ruleset::user(vec![], vec![]).with_ask_rules(vec![
            ToolAskRule {
                action: PermissionAction::Deny,
                ..ToolAskRule::exec_shell("rm")
            },
        ])]),
    ]
}

/// Spellings whose command word is only known at run time, or which run a
/// command behind a reserved word or a wrapper's operands.
const HIDDEN_RM: &[&str] = &[
    "v=rm; $v -f f",
    "v=rm; \"$v\" -f f",
    "v=rm; ${v} -f f",
    "sudo $v f",
    "bash -c '$v f'",
    "eval \"$v f\"",
    "$(echo rm) -f f",
    "`echo rm` f",
    "rm${IFS}x",
    "x=r; ${x}m f",
    "IFS=,; c=rm,x; $c",
    "/bin/r[m] -f f",
    "{rm,-f,f}",
    "$'\\x72m' f",
    "printf rm | sh",
    "echo rm x | bash",
    "sh <<< 'rm x'",
    "source <(echo rm x)",
    "find . -exec rm {} +",
    "find . -execdir rm {} \\;",
    "if true; then rm x; fi",
    "while rm x; do :; done",
    "until rm x; do :; done",
    "! rm x",
    "function f { rm x; }",
    "chroot /newroot rm -rf /",
    "chroot /newroot sh -c 'rm -rf /'",
    "sudo --user root bash -c 'rm -rf /'",
    "sudo --user root rm -rf /",
    "timeout -s KILL 5 rm x",
    "env -S'rm x'",
    // Wrapper options missing from the option tables, read both ways.
    "env -P /usr/bin rm -rf /",
    "ls | xargs -J % rm -rf %",
    "proot -R /x rm -rf /",
    "chroot -u root / rm -rf /",
    "doas -a style rm x",
    "sudo -a type rm x",
    // Substitution bodies read past quotes, and a `case` inside one.
    "echo $(case x in x) rm -rf /;; esac)",
    "echo $(echo \")\"; rm -rf /)",
    "echo 'unterminated $(rm x)",
    // `<<` inside arithmetic is a shift, not a heredoc.
    "((x = 1 << 2))\nrm -rf /",
    "let x=1<<2\nrm x",
    // Replacement strings are only known at run time.
    "echo 'rm -rf /' | xargs -I{} sh -c {}",
    "echo rm | xargs -I CMD CMD -rf /",
    "find . -exec sh -c {} \\;",
    // More wrappers, shells and code-as-string commands.
    "bash.exe -c 'rm -rf /'",
    "caffeinate -i rm -rf /",
    "arch -arm64 rm -rf /",
    "noglob rm -rf /",
    "nsenter -t 1 -m rm x",
    "unshare -r rm x",
    "sandbox-exec -n no-network rm x",
    "runuser -u root -- rm x",
    "trap 'rm -rf /' EXIT",
    "su -c 'rm -rf /' root",
    "flock /tmp/lock -c 'rm x'",
    "script -qc 'rm x' /dev/null",
    "watch 'ls; rm -rf /'",
    "cmd /c rm x",
    "pwsh -NoProfile -Command rm x",
    "powershell -enc cgBtACAAeAA=",
    "wsl -e rm x",
    // Options may follow `-c`; the command string is the first operand.
    "bash -c -e 'rm -rf /'",
    "sh -c -- 'rm -rf /'",
    "bash -c -o pipefail 'rm x'",
    // A script operand that names stdin reads the pipe or here-string.
    "echo 'rm x' | bash /dev/stdin",
    "bash /dev/stdin <<< 'rm x'",
    ". /dev/stdin <<< 'rm x'",
    "sh /proc/self/fd/0 <<< 'rm x'",
    // Launchers that run their operands as a command.
    "pkexec rm x",
    "pkexec --user root rm x",
    "run0 -u root rm x",
    "fakeroot rm -rf /",
    "taskset -c 0 rm x",
    "taskset 0x3 rm x",
    "strace -f -o /tmp/t rm x",
    "ltrace rm x",
    "chrt 1 rm -rf /",
    "chrt -r 10 rm x",
    "prlimit --nofile=10 rm -rf /",
    "systemd-run --user --scope rm x",
    "numactl -N 0 rm x",
    "firejail --noprofile rm x",
    "xvfb-run -a rm x",
    "dbus-launch --exit-with-session rm x",
    "proxychains -q rm x",
    "eatmydata rm x",
    "cpulimit -l 50 rm x",
    "sg wheel -c 'rm x'",
    "sg wheel 'rm x'",
    "gtimeout 5 rm x",
    "gnice -n 5 rm x",
    "alias qq='rm -rf'; eval qq /",
    "hash -p /bin/rm r; r x",
    "hash r=/bin/rm; r x",
    "=rm x",
    "parallel rm ::: x",
    "parallel 'rm {}' ::: x",
    "parallel -j 2 -- rm ::: x",
    "echo 'rm x' | parallel",
    "parallel ::: 'rm x'",
    "parallel -j 2 ::: ls 'rm x'",
    "parallel :::: cmds.txt ::: 'rm x'",
    "chpst -u nobody rm x",
    "screen -dmS s rm x",
    "at now <<< 'rm x'",
    "echo 'rm x' | batch",
    "at now <<EOF\nrm x\nEOF",
    "echo 'rm x' | at -q b now",
    "echo 'rm x' | at -qb now",
    "at -f script.sh now",
];

/// Literal spellings that were already denied and must stay denied.
const LITERAL_RM: &[&str] = &[
    "rm -f f",
    "\\rm x",
    "r''m x",
    "/bin/rm x",
    "(rm x)",
    "{ rm x; }",
    "case a in a) rm x;; esac",
    "f(){ rm -f x; }; f",
    "xargs rm",
    "command -p rm x",
];

/// Commands whose parse runs into a budget: past the budget, nothing was
/// checked, so the command is unresolved.
fn budget_commands() -> Vec<String> {
    vec![
        format!("sudo {}rm -rf /", "-H ".repeat(600)),
        format!("env {}rm -rf /", "-i ".repeat(600)),
        (1..=256)
            .map(|index| format!("true {index}"))
            .chain(["rm -rf /".to_string()])
            .collect::<Vec<_>>()
            .join("; "),
    ]
}

fn denied(engine: &ExecPolicyEngine, command: &str) -> bool {
    let decision = engine
        .check(context(command, AskForApproval::Never))
        .expect("policy check");
    !decision.allow
        && matches!(
            decision.requirement,
            ExecApprovalRequirement::Forbidden { .. }
        )
}

#[test]
fn deny_rules_hold_against_runtime_resolved_and_reserved_word_spellings() {
    let budget = budget_commands();
    let mut missed = Vec::new();
    for engine in deny_rm_engines() {
        for command in HIDDEN_RM
            .iter()
            .chain(LITERAL_RM)
            .copied()
            .chain(budget.iter().map(String::as_str))
        {
            if !denied(&engine, command) {
                missed.push(command.chars().take(80).collect::<String>());
            }
        }
    }
    assert!(missed.is_empty(), "not denied: {missed:#?}");
}

#[test]
fn unresolved_words_prompt_only_where_a_person_always_sees_the_prompt() {
    let engine = ExecPolicyEngine::new(vec![], vec!["rm".to_string()]);
    let requirement = |approval| {
        engine
            .check(context("v=rm; $v x", approval))
            .expect("policy check")
            .requirement
    };
    assert!(matches!(
        requirement(AskForApproval::OnRequest),
        ExecApprovalRequirement::NeedsApproval { .. }
    ));
    assert!(matches!(
        requirement(AskForApproval::UnlessTrusted),
        ExecApprovalRequirement::NeedsApproval { .. }
    ));
    // `OnFailure` is also the posture of sessions that approve on their own.
    for approval in [AskForApproval::OnFailure, AskForApproval::Never] {
        assert!(matches!(
            requirement(approval),
            ExecApprovalRequirement::Forbidden { .. }
        ));
    }
}

#[test]
fn ordinary_commands_stay_allowed_next_to_a_deny_rule() {
    for engine in deny_rm_engines() {
        for command in [
            "ls *.rs",
            "echo $HOME",
            "[ -f x ] && ls",
            "find . -name '*.rs'",
            "if true; then ls; fi",
            "rmdir x",
            "sudo -u root ls",
            "command -v rm",
            "command -pV rm",
            "sudo -E ls $f",
            "nice -5 ls",
            "timeout -v 5 ls",
            "xargs -I{} echo {}",
            "find . -exec grep -l x {} +",
            "cat <<EOF\nhello\nEOF\nls",
            "for ((i = 0; i < 3; i++)); do echo $i; done",
            "echo $(echo \")\")",
            "watch -n 5 ls",
            "alias ll='ls -la'",
            "hash -r",
            "parallel echo ::: a b",
            "at -l",
            "at -q a -l",
            "at -r 3",
            "at -d 3",
            "at -c 3",
            "batch -l",
        ] {
            let decision = engine
                .check(context(command, AskForApproval::Never))
                .expect("policy check");
            assert!(decision.allow, "{command:?} was denied: {decision:?}");
        }
    }
    // Without any deny rule, a runtime-resolved word is left to the mode.
    let open = ExecPolicyEngine::new(vec![], vec![]);
    for command in ["v=ls; $v", "echo $HOME", "ls *.rs"] {
        let decision = open
            .check(context(command, AskForApproval::Never))
            .expect("policy check");
        assert!(decision.allow, "{command:?} was denied: {decision:?}");
    }
}

#[test]
fn trusted_prefix_does_not_cover_interposed_options_or_nested_code() {
    let engine = ExecPolicyEngine::new(vec!["git status".to_string(), "ls".to_string()], vec![]);
    let trusted = |command: &str| {
        matches!(
            engine
                .check(context(command, AskForApproval::UnlessTrusted))
                .expect("policy check")
                .requirement,
            ExecApprovalRequirement::Skip { .. }
        )
    };
    assert!(trusted("git status"));
    assert!(trusted("git status -s --porcelain"));
    assert!(trusted("ls -la"));
    for command in [
        "git -ccore.fsmonitor=x status",
        "git -c core.fsmonitor=x status",
        "git --exec-path=/x status",
        "git -C /elsewhere status",
        "ls $(touch x)",
        "ls `touch x`",
        "$L -la",
    ] {
        assert!(!trusted(command), "{command:?} was auto-approved");
    }

    let dict = BashArityDict::new();
    assert!(!dict.allow_rule_matches("git status", "git --exec-path=/x status"));
    assert!(dict.allow_rule_matches("python -m pytest", "python -m pytest -x"));
    assert!(!dict.allow_rule_matches("python -m pytest", "python -m pip install x"));
}

#[test]
fn file_rules_fail_closed_on_runtime_resolved_words() {
    let config = ExecPolicyConfig::parse(
        r#"
[rules.shell]
allow = ["git status", "ls"]
deny = ["rm", "rm *"]
"#,
    )
    .expect("parse rules");
    let budget = budget_commands();
    let missed: Vec<&str> = HIDDEN_RM
        .iter()
        .copied()
        .chain(budget.iter().map(String::as_str))
        .filter(|command| !matches!(config.evaluate(command), RuleDecision::Deny(_)))
        .collect();
    assert!(missed.is_empty(), "not denied: {missed:#?}");
    assert_eq!(config.evaluate("git status -s"), RuleDecision::Allow);
    for command in ["git -ccore.fsmonitor=x status", "ls $(touch x)"] {
        assert!(
            matches!(config.evaluate(command), RuleDecision::AskUser(_)),
            "{command:?} was auto-approved"
        );
    }
}

#[test]
fn agent_read_only_rejects_a_glob_that_can_expand_to_an_option() {
    for command in ["rg foo *", "ls *", "git log ''*", "cat *.md"] {
        assert!(
            !is_agent_readonly_shell_command(command),
            "{command:?} was classified read-only"
        );
    }
    for command in [
        "ls src/*",
        "rg foo ./*",
        "find . -name '*.rs'",
        "cat README.md",
    ] {
        assert!(
            is_agent_readonly_shell_command(command),
            "{command:?} was rejected"
        );
    }
}

#[test]
fn parallel_read_only_rejects_parentheses() {
    assert!(is_parallel_readonly_command("cat README.md"));
    for command in [
        "cat .(e:'touch pwned':)",
        "ls foo(e:'id':)",
        "rg needle .(+cmd)",
        "cat (id)",
        "gh pr view 1(e:'id':)",
    ] {
        assert!(
            !is_parallel_readonly_command(command),
            "{command:?} was classified read-only"
        );
    }
}

#[test]
fn typed_deny_rule_skips_global_options_before_the_subcommand() {
    let engine = ExecPolicyEngine::with_rulesets(vec![
        Ruleset::user(vec![], vec![]).with_ask_rules(vec![ToolAskRule {
            action: PermissionAction::Deny,
            workspace: Some("/workspace".to_string()),
            ..ToolAskRule::exec_shell("git push")
        }]),
    ]);
    for command in [
        "git push",
        "git -C . push",
        "git -c a=b push origin main",
        "git --no-pager push",
    ] {
        assert!(denied(&engine, command), "{command} must be denied");
    }
    assert!(!denied(&engine, "git -C . status"));
    // The rule stays scoped to its workspace.
    let elsewhere = ExecPolicyContext {
        cwd: "/other",
        ..context("git -C . push", AskForApproval::Never)
    };
    assert!(engine.check(elsewhere).expect("policy check").allow);
}
