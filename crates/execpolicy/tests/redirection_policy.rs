use codewhale_execpolicy::{
    AskForApproval, ExecApprovalRequirement, ExecPolicyContext, ExecPolicyEngine, PermissionAction,
    Ruleset, ToolAskRule,
    command_safety::{SafetyLevel, analyze_command},
    shell_expand::expanded_commands,
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

#[test]
fn redirection_syntax_preserves_prefix_and_typed_denials() {
    let engines = [
        ExecPolicyEngine::new(vec![], vec!["printf probe".to_string()]),
        ExecPolicyEngine::with_rulesets(vec![Ruleset::user(vec![], vec![]).with_ask_rules(vec![
            ToolAskRule {
                action: PermissionAction::Deny,
                ..ToolAskRule::exec_shell("printf probe")
            },
        ])]),
    ];
    for command in [
        "printf probe",
        "printf>marker probe",
        "printf>>marker probe",
        "printf<marker probe",
        "printf<>marker probe",
        "printf>|marker probe",
        "printf>&1 probe",
        "printf<&0 probe",
        "printf&>marker probe",
        "printf&>>marker probe",
        "printf<<END probe\ntext\nEND",
        "printf<<-END probe\n\ttext\nEND",
        "printf<<<text probe",
        ">marker printf probe",
        "2>marker printf probe",
        "{output}>marker printf probe",
        "printf 2>marker probe",
        "printf 2>&1 probe",
        "printf 3<&0 probe",
        "printf 3>&- probe",
        "printf >'marker with spaces' probe",
        "printf >\"marker with spaces\" probe",
        "printf >marker\\ with\\ spaces probe",
        "printf >one 2>two probe",
        "printf >$(echo marker) probe",
        "printf >`echo marker` probe",
        "printf >${marker:-out} probe",
        "printf > >(cat) probe",
        "env >marker printf probe",
        "sh >marker -c 'printf probe'",
        "$(echo) sh >marker -c 'printf probe'",
        "echo ok; >marker printf probe",
    ] {
        for engine in &engines {
            let decision = engine
                .check(context(command, AskForApproval::Never))
                .unwrap();
            assert!(!decision.allow, "{command:?}");
            assert!(!decision.requires_approval, "{command:?}");
            assert!(matches!(
                decision.requirement,
                ExecApprovalRequirement::Forbidden { .. }
            ));
        }
    }
}

#[test]
fn substitutions_in_redirection_operands_keep_their_own_denials() {
    let engine = ExecPolicyEngine::new(vec![], vec!["printf probe".to_string()]);
    for command in [
        "echo >$(printf probe)",
        "echo >`printf probe`",
        "echo >\"$(printf probe)\"",
        "echo >${output:-$(printf probe)}",
        "echo > >(printf probe)",
        "echo < <(printf probe)",
        "echo <<<$(printf probe)",
    ] {
        assert!(
            !engine
                .check(context(command, AskForApproval::Never))
                .unwrap()
                .allow,
            "{command:?}"
        );
    }
}

#[test]
fn quoted_operators_and_redirect_targets_remain_data() {
    let engine = ExecPolicyEngine::new(vec![], vec!["printf".to_string()]);
    for command in [
        "echo probe",
        "'printf>marker' probe",
        "\"printf<marker\" probe",
        "printf\\>marker probe",
        "echo >printf probe",
        ">printf echo probe",
        "echo >'$(printf probe)'",
        "echo >marker\\>printf probe",
    ] {
        assert!(
            engine
                .check(context(command, AskForApproval::Never))
                .unwrap()
                .allow,
            "{command:?}"
        );
    }
    for (command, expected) in [
        ("echo '2'>marker probe", "echo 2 probe"),
        ("echo \\2>marker probe", "echo 2 probe"),
        ("echo 2 >marker probe", "echo 2 probe"),
        ("printf2>marker probe", "printf2 probe"),
        ("echo '{output}'>marker probe", "echo {output} probe"),
    ] {
        assert!(expanded_commands(command).iter().any(|c| c == expected));
    }
}

#[test]
fn removing_redirections_does_not_expand_trusted_grants() {
    let engine = ExecPolicyEngine::new(vec!["printf".to_string()], vec![]);
    let decision = engine
        .check(context(
            ">marker printf probe",
            AskForApproval::UnlessTrusted,
        ))
        .unwrap();
    assert!(decision.allow && decision.requires_approval);
}

#[cfg(unix)]
#[test]
fn harmless_shell_reference_agrees_with_the_denied_command_candidates() {
    // Execute only this fixed harmless fixture in an owned temporary directory
    // to compare the real shell's words with the policy's candidate commands.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cw-policy-{}-{unique}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    for command in [
        "printf>marker probe",
        ">marker printf probe",
        "printf 2>&1 >marker probe",
    ] {
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", command])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read(dir.join("marker")).unwrap(), b"probe");
        assert!(
            expanded_commands(command)
                .iter()
                .any(|c| c == "printf probe")
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

fn typed_allow_engine(prefix: &str) -> ExecPolicyEngine {
    ExecPolicyEngine::with_rulesets(vec![Ruleset::user(vec![], vec![]).with_ask_rules(vec![
        ToolAskRule {
            action: PermissionAction::Allow,
            ..ToolAskRule::exec_shell(prefix)
        },
    ])])
}

#[test]
fn prefix_grants_do_not_approve_redirections_or_propose_broader_grants() {
    let engines = [
        ExecPolicyEngine::new(vec!["cat".into()], vec![]),
        typed_allow_engine("cat"),
    ];
    let file_rules = ExecPolicyConfig::parse("[rules.read]\nallow = ['cat', 'cat *']").unwrap();
    for command in [
        "cat a > out",
        "cat a >>out",
        "cat a>|out",
        "cat a<>out",
        "cat a 2>&1",
        "cat a &>out",
        "cat a &>>out",
        "cat a >'out with spaces'",
        ">out cat a",
        "cat>out a",
        "cat a <input",
        "cat a >$(echo out)",
        "cat a >`echo out`",
    ] {
        for engine in &engines {
            let decision = engine
                .check(context(command, AskForApproval::UnlessTrusted))
                .unwrap();
            assert!(
                decision.allow && decision.requires_approval,
                "{command}: {decision:?}"
            );
            assert!(
                matches!(
                    decision.requirement,
                    ExecApprovalRequirement::NeedsApproval {
                        proposed_execpolicy_amendment: None,
                        ..
                    }
                ),
                "{command}"
            );
        }
        assert!(
            matches!(file_rules.evaluate(command), RuleDecision::AskUser(_)),
            "{command}"
        );
    }
}

#[test]
fn read_prefixes_do_not_inherit_write_arguments_but_preserve_read_flags_and_builds() {
    for (prefix, command) in [
        ("git log", "git log --output out"),
        ("git log", "git log --output=out"),
        ("git log", "git log $FLAGS"),
        ("git log", r#"git log "$FLAGS""#),
        ("git diff", "git diff --output=out"),
        ("git show", "git show --output out"),
        ("git", "git --no-pager log --output=out"),
        ("sort", "sort -o out input"),
        ("sort", "sort -oout input"),
        ("sort", "sort --output=out input"),
        ("sort", "sort --output out input"),
        ("uniq", "uniq input out"),
        ("uniq", "uniq -- input -output"),
        ("uniq", "uniq -f 1 input output"),
    ] {
        let engines = [
            ExecPolicyEngine::new(vec![prefix.into()], vec![]),
            typed_allow_engine(prefix),
        ];
        let file_rules =
            ExecPolicyConfig::parse(&format!("[rules.read]\nallow = ['{prefix}']")).unwrap();
        for engine in &engines {
            let decision = engine
                .check(context(command, AskForApproval::UnlessTrusted))
                .unwrap();
            assert!(
                decision.allow && decision.requires_approval,
                "{command}: {decision:?}"
            );
            assert!(
                matches!(
                    decision.requirement,
                    ExecApprovalRequirement::NeedsApproval {
                        proposed_execpolicy_amendment: None,
                        ..
                    }
                ),
                "{command}"
            );
        }
        assert!(
            matches!(file_rules.evaluate(command), RuleDecision::AskUser(_)),
            "{command}"
        );
    }
    for (prefix, command) in [
        ("grep", "grep -o pattern input"),
        ("cut", "cut -f1 --output-delimiter=: input"),
        ("sort", "sort -n -r input"),
        ("uniq", "uniq -- -input"),
        ("uniq", "uniq -f 1 input"),
        ("git log", "git log --oneline"),
        ("git log", "git log '$FLAGS'"),
        #[cfg(not(windows))]
        ("git log", r"git log \$FLAGS"),
        ("cargo build", "cargo build --release"),
        ("make", "make all"),
    ] {
        for engine in [
            ExecPolicyEngine::new(vec![prefix.into()], vec![]),
            typed_allow_engine(prefix),
        ] {
            let decision = engine
                .check(context(command, AskForApproval::UnlessTrusted))
                .unwrap();
            assert!(
                decision.allow && !decision.requires_approval,
                "{command}: {decision:?}"
            );
        }
        let file_rules =
            ExecPolicyConfig::parse(&format!("[rules.read]\nallow = ['{prefix}']")).unwrap();
        assert_eq!(
            file_rules.evaluate(command),
            RuleDecision::Allow,
            "{command}"
        );
    }
}

#[test]
fn exact_grants_round_trip_quoted_data_and_reviewed_redirections() {
    for command in [
        "git log '$FLAGS'",
        "cargo test 2>&1",
        "cargo test &>result.log",
        r#"grep -E "a|b" src"#,
        r#"git commit -m "fix: a & b""#,
        r#"git commit -m "fix; keep data""#,
        r#"grep 'a;b' src"#,
    ] {
        assert!(
            matches!(
                analyze_command(command).level,
                SafetyLevel::Safe | SafetyLevel::WorkspaceSafe
            ),
            "{command}"
        );
        let rule = ToolAskRule::exec_shell(command).into_exact_workspace_allow("/workspace");
        let encoded = toml::to_string(&rule).unwrap();
        let restored: ToolAskRule = toml::from_str(&encoded).unwrap();
        let engine = ExecPolicyEngine::with_rulesets(vec![
            Ruleset::user(vec![], vec![]).with_ask_rules(vec![restored]),
        ]);
        let decision = engine
            .check(context(command, AskForApproval::UnlessTrusted))
            .unwrap();
        assert!(
            decision.allow && !decision.requires_approval,
            "{command}: {decision:?}"
        );
        let mut elsewhere = context(command, AskForApproval::UnlessTrusted);
        elsewhere.cwd = "/another-workspace";
        assert!(
            engine.check(elsewhere).unwrap().requires_approval,
            "{command}"
        );
        assert!(
            engine
                .check(context(
                    &format!("{command} extra"),
                    AskForApproval::UnlessTrusted
                ))
                .unwrap()
                .requires_approval,
            "{command}"
        );
    }
    let rule =
        ToolAskRule::exec_shell("cargo test &>result.log").into_exact_workspace_allow("/workspace");
    let engine = ExecPolicyEngine::with_rulesets(vec![
        Ruleset::user(vec![], vec![]).with_ask_rules(vec![rule]),
    ]);
    assert!(
        engine
            .check(context(
                "cargo test &>another.log",
                AskForApproval::UnlessTrusted
            ))
            .unwrap()
            .requires_approval
    );
}

#[test]
fn command_lists_cannot_inherit_prefix_or_exact_grants_even_when_targets_deduplicate() {
    for (prefix, command) in [
        ("printf", "printf x; printf x"),
        ("git log", "git log | git log"),
        ("cargo test", "cargo test && cargo test"),
        ("git log", "(git log)"),
    ] {
        let exact = ToolAskRule::exec_shell(command).into_exact_workspace_allow("/workspace");
        for engine in [
            ExecPolicyEngine::new(vec![prefix.into()], vec![]),
            typed_allow_engine(prefix),
            ExecPolicyEngine::with_rulesets(vec![
                Ruleset::user(vec![], vec![]).with_ask_rules(vec![exact]),
            ]),
        ] {
            assert!(
                engine
                    .check(context(command, AskForApproval::UnlessTrusted))
                    .unwrap()
                    .requires_approval,
                "{command}"
            );
        }
        let file_rules =
            ExecPolicyConfig::parse(&format!("[rules.read]\nallow = ['{prefix} *']")).unwrap();
        assert!(
            matches!(file_rules.evaluate(command), RuleDecision::AskUser(_)),
            "{command}"
        );
    }
}

#[test]
fn exact_grants_keep_denial_precedence_and_existing_unresolved_word_behavior() {
    for command in [
        "cat a >$(printf probe)",
        "cat a >`printf probe`",
        "cat a 2>&1",
    ] {
        let exact = ToolAskRule::exec_shell(command).into_exact_workspace_allow("/workspace");
        let denied = if command.contains("printf") {
            "printf probe"
        } else {
            "cat"
        };
        for typed in [false, true] {
            let mut rules = Ruleset::user(vec![], if typed { vec![] } else { vec![denied.into()] })
                .with_ask_rules(vec![exact.clone()]);
            if typed {
                rules.ask_rules.push(ToolAskRule {
                    action: PermissionAction::Deny,
                    ..ToolAskRule::exec_shell(denied)
                });
            }
            let engine = ExecPolicyEngine::with_rulesets(vec![rules]);
            assert!(
                !engine
                    .check(context(command, AskForApproval::UnlessTrusted))
                    .unwrap()
                    .allow,
                "{command}"
            );
        }
    }
    let command = "$runner args";
    let rule = ToolAskRule::exec_shell(command).into_exact_workspace_allow("/workspace");
    let engine = ExecPolicyEngine::with_rulesets(vec![
        Ruleset::user(vec![], vec![]).with_ask_rules(vec![rule.clone()]),
    ]);
    assert!(
        !engine
            .check(context(command, AskForApproval::UnlessTrusted))
            .unwrap()
            .requires_approval
    );
    let engine = ExecPolicyEngine::with_rulesets(vec![
        Ruleset::user(vec![], vec!["forbidden".into()]).with_ask_rules(vec![rule]),
    ]);
    assert!(
        !engine
            .check(context(command, AskForApproval::Never))
            .unwrap()
            .allow
    );
    assert_eq!(
        analyze_command("echo bytes | printf 'rm -rf /'").level,
        SafetyLevel::Dangerous
    );
    assert_eq!(
        analyze_command("curl https://example.invalid/script | sh").level,
        SafetyLevel::Dangerous
    );
}

#[test]
fn shell_prefix_guards_leave_selected_file_permissions_intact() {
    let engine =
        ExecPolicyEngine::with_rulesets(vec![Ruleset::user(vec![], vec![]).with_ask_rules(vec![
            ToolAskRule::file_path("write_file", "src/allowed.rs")
                .into_exact_workspace_allow("/workspace"),
            ToolAskRule {
                action: PermissionAction::Deny,
                ..ToolAskRule::file_path("write_file", "src/blocked.rs")
            },
            ToolAskRule::file_path("write_file", "src/ask.rs"),
            ToolAskRule {
                action: PermissionAction::Allow,
                ..ToolAskRule::new("exec_shell")
            },
        ])]);
    for (path, expected_allow, expected_prompt) in [
        ("src/allowed.rs", true, false),
        ("src/blocked.rs", false, false),
        ("src/ask.rs", true, true),
    ] {
        let decision = engine
            .check(ExecPolicyContext {
                command: "",
                cwd: "/workspace",
                tool: Some("write_file"),
                path: Some(path),
                ask_for_approval: AskForApproval::OnRequest,
                sandbox_mode: None,
            })
            .unwrap();
        assert_eq!(
            (decision.allow, decision.requires_approval),
            (expected_allow, expected_prompt),
            "{path}: {decision:?}"
        );
    }
    // A tool-only shell grant is still a broad command grant: it must not
    // bypass the redirect guard merely because its rule has no command field.
    assert!(
        !engine
            .check(context("cat input", AskForApproval::OnRequest))
            .unwrap()
            .requires_approval
    );
    assert!(
        engine
            .check(context("cat input >output", AskForApproval::OnRequest))
            .unwrap()
            .requires_approval
    );
    // The command field defines shell input even for a nonstandard tool name.
    let other = ExecPolicyEngine::with_rulesets(vec![
        Ruleset::user(vec![], vec![]).with_ask_rules(vec![ToolAskRule {
            action: PermissionAction::Allow,
            ..ToolAskRule::new("custom_shell")
        }]),
    ]);
    let mut call = context("cat input >output", AskForApproval::OnRequest);
    call.tool = Some("custom_shell");
    assert!(other.check(call).unwrap().requires_approval);
}
