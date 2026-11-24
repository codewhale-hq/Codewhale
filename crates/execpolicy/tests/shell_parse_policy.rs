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
    "find . -delete",
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
    "v=rm; $=v -f f",
    "v=rm; $~v -f f",
    "v=rm; $^v -f f",
    "$'rm' -f f",
    "/bin/(r)m -f f",
    "/bin/r(m) -f f",
    "/usr/bin/rm(N) -f f",
    "setopt equals; =rm -f f",
    "=rm -f f",
    "rm# x",
    "hash x=/bin/rm; x -f f",
    "hash -p /bin/rm x; x -f f",
    "alias x=rm\nx -f f",
    "shopt -s expand_aliases\nalias x=rm\nx -f f",
    "emulate -L sh; rm x",
    "iex 'rm f'",
    "Invoke-Expression 'rm f'",
    "iex ('r'+'m f')",
    "'rm f' | iex",
    "genv rm x",
    "genv -i rm x",
    "genv -S'rm x'",
    "echo x | gxargs rm",
    "echo 'rm -rf /' | gxargs -I{} sh -c {}",
    "gtime -p rm x",
    "gsetsid -f rm x",
    "toybox rm x",
    "script -q /dev/null rm x",
    "script -t 0 -q /dev/null rm x",
    "parallel rm ::: x",
    "parallel ::: 'rm x'",
    "bwrap --ro-bind / / rm x",
    "bwrap --ro-bind / / -- rm x",
    "setarch x86_64 rm x",
    "setarch x86_64 -R rm x",
    "linux64 rm x",
    "faketime '2020-01-01' rm x",
    "faketime -f '-1d' rm x",
    "rlwrap -a rm x",
    "chronic rm x",
    "ifne rm x",
    "runcon system_u:system_r:unconfined_t:s0 rm x",
    "runcon -t unconfined_t rm x",
    "chpst -u nobody rm x",
    "softlimit -m 1024 rm x",
    "valgrind --tool=none rm x",
    "perf stat rm x",
    "perf stat -r 2 -e cycles rm x",
    "perf record -g rm x",
    "watchexec -w src rm x",
    "watchexec -w src 'ls; rm x'",
    "entr rm x",
    "entr -s 'ls; rm x' /_",
    "systemd-inhibit --why=test rm x",
    "cgexec -g cpu:test rm x",
    "daemonize /bin/rm x",
    "start-stop-daemon -S -x /bin/rm -- x",
    "start-stop-daemon --start --exec=/bin/rm -- -rf /",
    "nsjail --config /tmp/c -- rm x",
    "torify rm x",
    "launchctl asuser 501 rm x",
    "launchctl bsexec 123 rm x",
    "/lib64/ld-linux-x86-64.so.2 /bin/rm x",
    "/lib/ld-linux-aarch64.so.1 --library-path /tmp /bin/rm x",
    "ld.so /bin/rm x",
    "npm exec -- rm x",
    "opam exec -- rm x",
    "op run -- rm x",
    "kubectl exec pod -- rm x",
    "kubectl exec pod -- sh -c 'rm x'",
    "uv run rm x",
    "uv run --with pkg rm x",
    "poetry run rm x",
    "pipx run rm x",
    "bundle exec rm x",
    "nix develop -c rm x",
    "nix develop .#dev -c bash -c 'rm x'",
    "nix develop -c$(echo rm) x",
    "nix develop -c$(echo rm)",
    "nix develop --command=$(echo rm) x",
    "nix develop --command$(echo =rm) x",
    "nix develop --command='rm x'",
    "nix develop -crm x",
    "nix shell -c$(echo rm) x",
    "nix shell --command=$(echo rm) x",
    "nix shell --command='rm x'",
    "nix-shell -p busybox --run 'rm x'",
    "nix-shell --run$(echo =rm) x",
    "su -c$(echo rm) x",
    "npx -c$(echo rm) x",
    "npx -c$(echo rm)",
    "start-stop-daemon -S -x$(echo /bin/rm) -- x",
    "start-stop-daemon -S --exec=$(echo /bin/rm) -- x",
    "launchctl submit -p$(echo /bin/rm) -- x",
    "cmd /c$(echo rm) x",
    "pwsh -c$(echo rm) x",
    "find . -exec$(echo rm) x \\;",
    "sh -c$(echo rm) x",
    "nu --commands$(echo =rm) x",
    "docker run --entrypoint=$(echo /bin/rm) alpine x",
    "docker run --entrypoint$(echo =/bin/rm) alpine x",
    "docker run --rm alpine rm x",
    "docker run --entrypoint /bin/rm alpine -rf /",
    "docker exec ctr rm x",
    "npx -c 'rm x'",
    "ssh host rm x",
    "ssh -p 2222 user@host 'ls; rm x'",
    "tmux new-session -d 'rm x'",
    "tmux new-session -d -s s rm x",
    "screen -dm rm x",
    "screen -dmS s rm x",
    "elvish -c 'rm x'",
    "nu -c 'rm x'",
    "nu --commands 'rm x'",
    "xonsh -c 'rm x'",
    "oksh -c 'rm x'",
    "posh -c 'rm x'",
    "osh -c 'rm x'",
    "echo 'rm x' | elvish",
    "nu <<< 'rm x'",
    "alias qq='rm -rf'; eval qq /",
    "hash -p /bin/rm r; r x",
    "hash -p/bin/rm r; r x",
    "hash -lp /bin/rm r; r x",
    "hash r=/bin/rm; r x",
    "=rm x",
    "parallel 'rm {}' ::: x",
    "parallel -j 2 -- rm ::: x",
    "echo 'rm x' | parallel",
    "parallel -j 2 ::: ls 'rm x'",
    "parallel :::: cmds.txt ::: 'rm x'",
    "parallel :::: cmds.txt",
    "parallel sh -c ::: 'rm x'",
    "parallel eval ::: 'rm x'",
    "parallel 'sh -c {}' ::: 'rm x'",
    "parallel bash -c {} ::: 'rm x'",
    "at now <<< 'rm x'",
    "echo 'rm x' | batch",
    "at now <<EOF\nrm x\nEOF",
    "echo 'rm x' | at -q b now",
    "echo 'rm x' | at -qb now",
    "at -f script.sh now",
    "at -f script.sh -l",
    "at -l -f script.sh",
    "awk 'BEGIN{system(\"rm x\")}'",
    "awk 'BEGIN{\"rm x\" | getline}'",
    "awk 'BEGIN{\"rm x\" |& getline}'",
    "awk 'BEGIN{print \"x\" | \"rm y\"}'",
    "awk 'BEGIN{print \"x\" |& \"rm y\"}'",
    "awk 'BEGIN{c=\"rm x\"; system(c)}'",
    "awk 'BEGIN{system(\"r\" \"m\" \" x\")}'",
    r#"awk 'BEGIN{system("\162\155 x")}'"#,
    "awk '{x = a / b; system(\"rm x\"); y = c / d}'",
    "gawk -e 'BEGIN{system(\"rm x\")}'",
    "gawk --source='BEGIN{system(\"rm x\")}'",
    "awk $PROG file",
    "sed 's/a/rm x/e'",
    "sed 's#a#rm x#ge'",
    "sed 's|a|rm x|2ep'",
    "sed 's/.*/rm x/e'",
    "sed 's/a/rm x/ew /tmp/out'",
    "sed 's/^//e'",
    "sed 'e rm x'",
    "sed '1e rm x'",
    "sed '/foo/e rm x'",
    "sed 'e'",
    "sed '1{e}'",
    "sed 's/foo/bar/g; e rm x'",
    "sed -e 's/a/b/' -e 's/c/rm x/e'",
    "sed --expression='e rm x'",
    "sed -i '' 's/a/rm x/e' file.txt",
    "sed -i .bak 's/a/rm x/e' file.txt",
    "sed $PROG file",
    "echo 'rm x' | ssh host",
    "echo 'rm x' |& ssh host",
    "echo 'rm x' |\nssh host",
    "ssh host < script",
    "ssh host 0< script",
    "ssh host <<< 'rm x'",
    "ssh host 0<<< 'rm x'",
    "ssh host <<EOF\nrm x\nEOF",
    "ssh host sh <<EOF\nrm x\nEOF",
    "ssh host 'ssh host2' <<EOF\nrm x\nEOF",
    "ssh host ls <<EOF\n$(rm x)\nEOF",
    "echo 'rm x' | find . -exec ssh host \\;",
    "echo 'rm x' | { a; ssh host; }",
    "echo 'rm x' | { { a; }; ssh host; }",
    "echo 'rm x' | (a; ssh host)",
    "{ a; ssh host; } <<< 'rm x'",
    "(a; ssh host) < script",
    "numactl --strict rm",
    "numactl --strict -N 0 rm",
    "su $(echo -c) 'rm x'",
    "su $FLAG 'rm x'",
    "su --$(echo command) 'rm x'",
    "nix $(echo develop) -c rm x",
    "nix develop $(echo -c) rm x",
    "nix develop --$(echo command) rm x",
    "docker $(echo run) --entrypoint rm alpine",
    "docker run $(echo --entrypoint) rm alpine",
    "docker run $(echo --entrypoint=rm) alpine",
    "docker run --entrypoint $(echo rm) alpine",
    "docker run --entrypoint=$(echo rm) alpine",
    "start-stop-daemon --start $(echo --exec) rm -- x",
    "start-stop-daemon --start $FLAG rm -- x",
    "start-stop-daemon --start --$(echo exec) /bin/rm -- x",
    "start-stop-daemon -K $(echo --start) --exec /bin/rm -- x",
    "nix-shell $(echo --run) 'rm x'",
    "npx $(echo -c) 'rm x'",
    "npm $(echo exec) -c 'rm x'",
    "npm exec $(echo -c) 'rm x'",
    "uv $(echo run) rm x",
    "tmux $(echo new-session) -d 'rm x'",
    "launchctl $(echo submit) -p /bin/rm",
    "launchctl submit $(echo -p) /bin/rm",
    "ssh $(echo -p) 22 host rm x",
    "screen -r $(echo -m) rm x",
    "find . $(echo -exec) rm {} +",
    "cmd $(echo /c) rm x",
    "bash --$(echo command) 'rm x'",
    "env -S $(echo 'rm x')",
    "env --$(echo 'split-string=rm x')",
    "tmux display-popup 'rm x'",
    "tmux popup -E 'rm x'",
    "tmux display-popup -EE -d /tmp -t s -w 80% -h 50% -x C -y C -T title -s fg=red -S fg=blue -b single -e FOO=1 'rm x'",
    "tmux display-popup -E rm x",
    "tmux confirm-before 'rm x'",
    "tmux confirm -b -c y -p 'sure?' -t s 'rm x'",
    "tmux confirm-before -p 'delete?' \"run-shell 'rm x'\"",
    "tmux confirm-before \"display-popup -E 'rm x'\"",
    "tmux confirm-before \"list-sessions ; run-shell 'rm x'\"",
    "tmux command-prompt -I 'rm x'",
    "tmux command-prompt -I'rm x'",
    "tmux command-prompt -bI 'rm x'",
    "tmux command-prompt -I \"run-shell 'rm x'\"",
    "tmux command-prompt -I 'echo hi,rm x'",
    "tmux command-prompt \"run-shell 'rm x'\"",
    "tmux command-prompt -p 'cmd:' \"run-shell 'rm %%'\"",
    "tmux command-prompt -I 'rm x' \"run-shell '%%'\"",
    "tmux command-prompt -I 'foo,rm x' \"run-shell '%2'\"",
    "tmux command-prompt -p 'cmd:' \"run-shell '%%'\"",
    "tmux command-prompt -p 'cmd:' '%%'",
    "tmux bind-key k run-shell 'rm x'",
    "tmux bind k run 'rm x'",
    "tmux bind-key -n F1 run-shell 'rm x'",
    "tmux bind-key -r -T root -N 'note' k display-popup -E 'rm x'",
    "tmux bind-key k \"run-shell 'rm x'\"",
    "tmux bind-key k confirm-before \"run-shell 'rm x'\"",
    "tmux bind-key k display-message hi \\; run-shell 'rm x'",
    "tmux bind-key k source-file /tmp/evil.conf",
    "tmux bind-key k $CMD",
    "tmux bind-key k '$CMD'",
    "tmux source-file f",
    "tmux source ~/.tmux.conf",
    "tmux source-file -q -t %0 f",
    "tmux source-file -n -F '#(rm x)'",
    "tmux source-file -t -n f",
    "tmux source-file -- -n",
    "tmux run-shell 'rm x'",
    "tmux run -b -d 1 -c /tmp 'rm x'",
    "tmux run-shell -C \"display-popup 'rm x'\"",
    "tmux send-keys -t s 'rm x' Enter",
    "tmux if-shell 'true' 'rm x'",
    "tmux if-shell 'true' \"run-shell 'rm x'\"",
    "tmux pipe-pane -o 'rm x'",
    "tmux list-sessions \\; display-popup 'rm x'",
    "oc rsh pod rm x",
    "oc rsh pod -- rm x",
    "oc -n prod rsh -c app pod rm x",
    "oc exec pod -- rm x",
    "oc exec pod rm x",
    "oc exec -n prod pod -c app -- sh -c 'rm x'",
    "oc exec -f pod.yaml rm x",
    "oc debug node/n -- chroot /host rm x",
    "oc debug --image=alpine -- rm x",
    "oc run tmp --image=alpine -- rm x",
    "oc exec pod -- $v x",
    "vagrant ssh -c 'rm x'",
    "vagrant ssh --command 'rm x'",
    "vagrant ssh --command='rm x'",
    "vagrant ssh default -c 'rm x'",
    "vagrant ssh -- rm x",
    "vagrant ssh default -- -t 'ls; rm x'",
    "vagrant ssh -c \"$CMD\"",
    "vagrant winrm -c 'rm x'",
    "op run --env-file .env -- rm x",
    "op run --env-file .env rm x",
    "op run -- $v x",
    "op plugin run rm x",
    "opam exec rm x",
    "opam exec --switch default rm x",
    "opam exec -- $v x",
    "helm template . --post-renderer rm",
    "helm upgrade rel . --post-renderer=/bin/rm",
    "helm install rel . --post-renderer sh --post-renderer-args -c --post-renderer-args 'rm x'",
    "helm template . --post-renderer \"$RENDERER\"",
    "helm template . $(echo --post-renderer) rm",
    "helm template . --$(echo post-renderer) rm",
    "helm plugin install ./evil",
    "helm plugin $(echo install) ./evil",
    "oc $(echo exec) pod -- rm x",
    "vagrant ssh $(echo -c) 'rm x'",
    "op $(echo run) -- rm x",
    "op plugin $(echo run) rm x",
    "opam $(echo exec) rm x",
    // ssh -o ProxyCommand / LocalCommand (including under -N and normalized whitespace).
    "ssh -o 'ProxyCommand=rm x' host",
    "ssh -N -o 'ProxyCommand=rm x' host",
    "ssh -oProxyCommand='rm x' host",
    "ssh -o 'ProxyCommand = rm x' host",
    "ssh -o 'ProxyCommand\trm x' host",
    "ssh -o 'PermitLocalCommand=yes' -o 'LocalCommand=rm x' host",
    "ssh -N -o 'PermitLocalCommand = yes' -o 'LocalCommand=rm x' host",
    "ssh -o ' PermitLocalCommand=yes' -o 'LocalCommand=rm x' host",
    "ssh -o 'PermitLocalCommand\t=\tyes' -o 'LocalCommand=rm x' host",
    "ssh -o \"$OPT\" host",
    "echo 'rm x' | mosh host",
    "echo 'rm x' | dbclient host",
    // screen -X / -Q (including combined with -r/-R/-d).
    "screen -X screen rm x",
    "screen -X exec rm x",
    "screen -X eval 'rm x'",
    "screen -X stuff 'rm x\n'",
    "screen -X at 0 rm x",
    "screen -r s -X stuff 'rm x\n'",
    "screen -r 1 -X exec rm x",
    "screen -dr -X exec rm x",
    "screen -R s -X screen rm",
    "screen -r s -Q stuff 'rm x\n'",
    // kubectl exec / run / debug.
    "kubectl exec pod rm x",
    "kubectl exec pod -- rm x",
    "kubectl exec -c ctr pod rm x",
    "kubectl exec -n ns pod rm x",
    "kubectl run tmp --image=alpine -- rm x",
    // docker/podman container/compose groups, create, buildx --call, --health-cmd.
    "docker container run alpine rm x",
    "docker container exec ctr rm x",
    "podman container run alpine rm x",
    "podman container exec ctr rm x",
    "docker create alpine rm x",
    "docker compose exec svc rm x",
    "docker compose run svc rm x",
    "docker compose -f x.yml exec svc rm x",
    "docker compose -f=x exec svc rm x",
    "docker compose --ansi never exec svc rm x",
    "docker compose -p proj exec svc rm x",
    "docker --context prod compose exec svc rm x",
    "docker buildx bake --call 'system(\"rm x\")'",
    "docker run --health-cmd 'rm x' alpine",
    "docker run --health-cmd='rm x' alpine",
    // parallel --compress-program / --decompress-program & bwrap --args.
    "parallel --compress-program 'rm x' ::: y",
    "parallel --compress-program='rm x' ::: y",
    "parallel --compress-program=rm echo ::: y",
    "parallel --compress-program='rm x' echo ::: y",
    "parallel --decompress-program='rm x' echo ::: y",
    "bwrap --args 3",
    "bwrap --dev-bind / / --args=4",
    // Shell -c aliases, gawk -i/-l, and pwsh - / piped stdin.
    "oil -c 'rm x'",
    "ysh -c 'rm x'",
    "pdksh -c 'rm x'",
    "loksh -c 'rm x'",
    "rksh -c 'rm x'",
    "ion -c 'rm x'",
    "ngs -c 'rm x'",
    "gawk -i /dev/stdin '{print}' f",
    "gawk --include=/dev/stdin '{print}' f",
    "gawk -l \"$EXT\" '{print}' f",
    "gawk --load=\"$EXT\" '{print}' f",
    "pwsh -",
    "powershell -",
    "pwsh <<EOF\nrm x\nEOF",
    "echo 'rm x' | pwsh",
    "echo 'rm x' | powershell",
    "pwsh -File -",
    "pwsh -File /dev/stdin",
    // Quoted dynamic words, INERT_DASH_DASH_HEADS, and separated nix -c shell-string.
    "find . \"$X\" rm {} +",
    "docker run \"$X\" alpine",
    "npm exec -- \"$X\"",
    "true -- $v",
    "sed -- \"$X\"",
    "awk -- \"$X\"",
    "nix develop -c 'echo a; rm -rf /'",
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
            "arr=(a b)",
            "arr+=(c)",
            "f() { ls; }",
            "f(){ ls; }",
            "g++ x.cc",
            "set -euo pipefail",
            "hash -r",
            "grep foo$ file",
            "~/bin/tool --flag=a%b",
            "./a.out x=1",
            "FOO=bar ls",
            "git rm old.txt",
            "git rm -f old.txt",
            "grep rm notes.txt",
            "grep -rn rm src/",
            "docker rm my-container",
            "docker run --rm alpine ls",
            "docker run --rm alpine git rm old.txt",
            "uv add rm",
            "uv run git rm old.txt",
            "poetry add rm",
            "perf list rm",
            "ssh host git rm old.txt",
            "ssh -N host",
            "runcon -t unconfined_t git rm old.txt",
            "script -q /dev/null git rm old.txt",
            "start-stop-daemon -K -x /bin/rm",
            "start-stop-daemon -S -x /bin/echo -- hi",
            "docker run --entrypoint /bin/echo alpine hi",
            "nix develop -c echo hi",
            "nix develop --command echo hi",
            "nix develop --command=echo hi",
            "nix develop -c grep -c rm notes.txt",
            "nix shell -c echo hi",
            "screen -r rm",
            "/lib64/ld-linux-x86-64.so.2 --list /bin/rm",
            "grep \"--\" rm notes.txt",
            "grep -- \"$PATTERN\" notes.txt",
            "cargo test -p codewhale-engine -- --nocapture",
            "parallel echo ::: a b",
            "at -l",
            "at -q a -l",
            "at -r 3",
            "at -d 3",
            "at -c 3",
            "batch -l",
            "awk '{print}'",
            "awk '{print $1}'",
            "awk -F: '{print $1}' /etc/passwd",
            "awk '/foo|bar/ {print}' file.txt",
            "awk '$1 == \"a\" || $2 == \"b\" {print}' file.txt",
            "awk '{print $1 / 2}' file.txt",
            "awk '{print \"system(\\\"rm x\\\")\"}'",
            "sed -n '/x/p'",
            "sed 's/foo/bar/g'",
            "sed -e 's/echo/echoed/g'",
            "sed 's/e/x/g'",
            "sed 's/a/b/w readme.txt'",
            "sed 'y/abcde/ABCDE/'",
            "sed ':end; s/a/b/; t end'",
            "sed -i 's/foo/bar/' file.txt",
            "sed -i '' 's/foo/bar/g' file.txt",
            "x || ssh host",
            "echo x | y || ssh host",
            "echo x | y && ssh host",
            "echo x | y; ssh host",
            "ssh host 3< f",
            "ssh host {fd}< f",
            "ssh host 3<<< 'rm x'",
            "ssh host 3<&0",
            "ssh host 3<<EOF\nrm x\nEOF",
            "ssh host ls <<EOF\nrm x\nEOF",
            "ssh host <<EOF\nls\nEOF",
            "ssh -N host < f",
            "echo x | ssh -N host",
            "find . -exec ssh host \\;",
            "echo x | { a; b; }; ssh host",
            "echo x | (a; b); ssh host",
            "{ a; ssh host; } 3< f",
            "numactl --strict ls",
            "numactl --strict echo hello",
            "echo $(whoami)",
            "echo $(date)",
            "git commit -m $(cat msg)",
            "ls $DIR",
            "docker ps",
            "docker ps --filter \"$F\"",
            "docker run --rm alpine echo $(whoami)",
            "docker run --rm -e \"$FOO\" alpine ls",
            "docker run --rm --env=\"$FOO\" alpine ls",
            "docker run --entrypoint '' alpine ls",
            "nix --version",
            "nix build $FLAKE",
            "npm test $FILTER",
            "runuser -u root -- echo $(date)",
            "flock /tmp/lock echo $(date)",
            "script -q /dev/null echo $(date)",
            "bash ./script.sh $ARG",
            "find $DIR -name \"$PAT\"",
            "screen -r \"$SESSION\"",
            "tmux list-sessions",
            "tmux ls",
            "tmux display-message \"hi\"",
            "tmux attach",
            "tmux attach-session -t work",
            "tmux -V",
            "tmux display-popup -C",
            "tmux display-popup -E ls",
            "tmux display-popup -T 'rm x' ls",
            "tmux confirm-before -p 'rm?' kill-pane",
            "tmux confirm-before -b -c y 'display-message hi'",
            "tmux command-prompt -p 'Rename:' \"rename-window '%%'\"",
            "tmux command-prompt -p 'rm?' \"rename-window '%%'\"",
            "tmux command-prompt -p -I \"rename-window '%%'\"",
            "tmux command-prompt -I 'mywin' \"rename-window '%%'\"",
            "tmux bind-key -n k list-sessions",
            "tmux bind-key -N 'rm shortcut' k display-message hi",
            "tmux bind-key -r -N 'note' k",
            "tmux source-file -n ~/.tmux.conf",
            "tmux source-file -n -t %0 ~/.tmux.conf",
            "tmux if-shell -F '#{==:#{session_name},work}' 'display-message ok'",
            "tmux pipe-pane",
            "tmux send-keys -t s 'ls' Enter",
            "oc get pods",
            "oc rsh pod",
            "oc exec pod -- ls",
            "oc exec --namespace=\"$NS\" pod -- ls",
            "vagrant status",
            "vagrant ssh",
            "vagrant ssh -c 'ls'",
            "op item list",
            "op run --env-file .env -- ls",
            "op run --env-file=\"$ENV_FILE\" -- ls",
            "opam list",
            "opam exec -- ls",
            "helm list",
            "helm status my-release",
            "helm template .",
            "helm template . --namespace=\"$NS\"",
            "helm template . --post-renderer kustomize",
            "helm plugin list",
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

#[test]
fn subcommand_deny_rules_hold_against_glob_group_spellings() {
    let engines = [
        ExecPolicyEngine::new(vec![], vec!["git push".to_string()]),
        ExecPolicyEngine::with_rulesets(vec![Ruleset::user(vec![], vec![]).with_ask_rules(vec![
            ToolAskRule {
                action: PermissionAction::Deny,
                ..ToolAskRule::exec_shell("git push")
            },
        ])]),
    ];
    for engine in &engines {
        for command in [
            "git push",
            "/usr/bin/git(N) push",
            "/usr/bin/(g)it push",
            "g=git; $=g push",
        ] {
            assert!(denied(engine, command), "{command} must be denied");
        }
        for command in ["git status", "git log --oneline -3"] {
            let decision = engine
                .check(context(command, AskForApproval::Never))
                .expect("policy check");
            assert!(decision.allow, "{command:?} was denied: {decision:?}");
        }
    }
}
