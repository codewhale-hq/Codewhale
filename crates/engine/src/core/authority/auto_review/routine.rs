use crate::tools::spec::normalize_path;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Arity {
    Bool,
    Word,
    Num,
    Path,
}

type Flags = &'static [(&'static str, Arity)];

#[derive(Clone, Copy)]
enum Pos {
    None,
    Operands,
    Targets(&'static [&'static str]),
}

struct Spec {
    flags: Flags,
    pos: Pos,
    harness: Option<&'static Spec>,
    lints: bool,
}

const LIBTEST: Spec = Spec {
    flags: &[
        ("--nocapture", Arity::Bool),
        ("--show-output", Arity::Bool),
        ("--ignored", Arity::Bool),
        ("--include-ignored", Arity::Bool),
        ("--exact", Arity::Bool),
        ("--quiet", Arity::Bool),
        ("-q", Arity::Bool),
        ("--list", Arity::Bool),
        ("--test-threads", Arity::Num),
        ("--skip", Arity::Word),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const CLIPPY_LINTS: Spec = Spec {
    flags: &[("--no-deps", Arity::Bool)],
    pos: Pos::None,
    harness: None,
    lints: true,
};

const NODE_HARNESS: Spec = Spec {
    flags: &[
        ("--ci", Arity::Bool),
        ("--coverage", Arity::Bool),
        ("--silent", Arity::Bool),
        ("--verbose", Arity::Bool),
        ("--bail", Arity::Bool),
        ("--runInBand", Arity::Bool),
        ("--watch", Arity::Word),
        ("--watchAll", Arity::Word),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const NEXTEST_FILTERS: Spec = Spec {
    flags: &[],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const CARGO_FLAGS: Flags = &[
    ("--release", Arity::Bool),
    ("-r", Arity::Bool),
    ("--workspace", Arity::Bool),
    ("--all", Arity::Bool),
    ("--all-targets", Arity::Bool),
    ("--all-features", Arity::Bool),
    ("--no-default-features", Arity::Bool),
    ("--lib", Arity::Bool),
    ("--bins", Arity::Bool),
    ("--tests", Arity::Bool),
    ("--benches", Arity::Bool),
    ("--examples", Arity::Bool),
    ("--doc", Arity::Bool),
    ("--no-run", Arity::Bool),
    ("--locked", Arity::Bool),
    ("--frozen", Arity::Bool),
    ("--offline", Arity::Bool),
    ("--quiet", Arity::Bool),
    ("-q", Arity::Bool),
    ("--verbose", Arity::Bool),
    ("-v", Arity::Bool),
    ("--no-fail-fast", Arity::Bool),
    ("--keep-going", Arity::Bool),
    ("-p", Arity::Word),
    ("--package", Arity::Word),
    ("--exclude", Arity::Word),
    ("--bin", Arity::Word),
    ("--test", Arity::Word),
    ("--example", Arity::Word),
    ("--bench", Arity::Word),
    ("--features", Arity::Word),
    ("-F", Arity::Word),
    ("--target", Arity::Word),
    ("--profile", Arity::Word),
    ("--color", Arity::Word),
    ("--message-format", Arity::Word),
    ("--jobs", Arity::Num),
    ("-j", Arity::Num),
    ("--manifest-path", Arity::Path),
];

const CARGO_TEST: Spec = Spec {
    flags: CARGO_FLAGS,
    pos: Pos::Operands,
    harness: Some(&LIBTEST),
    lints: false,
};

const CARGO_BUILD: Spec = Spec {
    flags: CARGO_FLAGS,
    pos: Pos::None,
    harness: None,
    lints: false,
};

const CARGO_CLIPPY: Spec = Spec {
    flags: CARGO_FLAGS,
    pos: Pos::None,
    harness: Some(&CLIPPY_LINTS),
    lints: false,
};

const NEXTEST: Spec = Spec {
    flags: &[
        ("--release", Arity::Bool),
        ("--workspace", Arity::Bool),
        ("--all-targets", Arity::Bool),
        ("--lib", Arity::Bool),
        ("--bins", Arity::Bool),
        ("--tests", Arity::Bool),
        ("--locked", Arity::Bool),
        ("--frozen", Arity::Bool),
        ("--offline", Arity::Bool),
        ("--no-fail-fast", Arity::Bool),
        ("--no-run", Arity::Bool),
        ("--no-default-features", Arity::Bool),
        ("--all-features", Arity::Bool),
        ("-p", Arity::Word),
        ("--package", Arity::Word),
        ("--features", Arity::Word),
        ("-F", Arity::Word),
        ("--profile", Arity::Word),
        ("--run-ignored", Arity::Word),
        ("--status-level", Arity::Word),
        ("--final-status-level", Arity::Word),
        ("--color", Arity::Word),
        ("--jobs", Arity::Num),
        ("-j", Arity::Num),
        ("--test-threads", Arity::Num),
    ],
    pos: Pos::Operands,
    harness: Some(&NEXTEST_FILTERS),
    lints: false,
};

const PYTEST: Spec = Spec {
    flags: &[
        ("-v", Arity::Bool),
        ("-vv", Arity::Bool),
        ("-vvv", Arity::Bool),
        ("-q", Arity::Bool),
        ("-qq", Arity::Bool),
        ("-x", Arity::Bool),
        ("-s", Arity::Bool),
        ("-l", Arity::Bool),
        ("--lf", Arity::Bool),
        ("--ff", Arity::Bool),
        ("--nf", Arity::Bool),
        ("--sw", Arity::Bool),
        ("--co", Arity::Bool),
        ("--collect-only", Arity::Bool),
        ("--disable-warnings", Arity::Bool),
        ("--no-header", Arity::Bool),
        ("--no-summary", Arity::Bool),
        ("--strict-markers", Arity::Bool),
        ("--showlocals", Arity::Bool),
        ("-ra", Arity::Bool),
        ("-rA", Arity::Bool),
        ("-rf", Arity::Bool),
        ("-rs", Arity::Bool),
        ("-rx", Arity::Bool),
        ("-rE", Arity::Bool),
        ("--tb", Arity::Word),
        ("--color", Arity::Word),
        ("--capture", Arity::Word),
        ("-k", Arity::Word),
        ("-m", Arity::Word),
        ("--deselect", Arity::Word),
        ("-n", Arity::Word),
        ("--maxfail", Arity::Num),
        ("--durations", Arity::Num),
        ("--ignore", Arity::Path),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const UNITTEST: Spec = Spec {
    flags: &[
        ("-v", Arity::Bool),
        ("-q", Arity::Bool),
        ("-f", Arity::Bool),
        ("-b", Arity::Bool),
        ("-c", Arity::Bool),
        ("-l", Arity::Bool),
        ("--verbose", Arity::Bool),
        ("--quiet", Arity::Bool),
        ("--failfast", Arity::Bool),
        ("--buffer", Arity::Bool),
        ("--catch", Arity::Bool),
        ("--locals", Arity::Bool),
        ("-k", Arity::Word),
        ("-p", Arity::Word),
        ("-s", Arity::Path),
        ("-t", Arity::Path),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const NODE_PM: Spec = Spec {
    flags: &[
        ("--silent", Arity::Bool),
        ("--if-present", Arity::Bool),
        ("--ignore-scripts", Arity::Bool),
        ("--recursive", Arity::Bool),
        ("-r", Arity::Bool),
    ],
    pos: Pos::None,
    harness: Some(&NODE_HARNESS),
    lints: false,
};

const BUN_TEST: Spec = Spec {
    flags: &[
        ("--bail", Arity::Bool),
        ("--coverage", Arity::Bool),
        ("--timeout", Arity::Num),
        ("--rerun-each", Arity::Num),
        ("-t", Arity::Word),
        ("--test-name-pattern", Arity::Word),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const GO_TEST: Spec = Spec {
    flags: &[
        ("-v", Arity::Bool),
        ("-race", Arity::Bool),
        ("-short", Arity::Bool),
        ("-cover", Arity::Bool),
        ("-failfast", Arity::Bool),
        ("-json", Arity::Bool),
        ("-benchmem", Arity::Bool),
        ("-run", Arity::Word),
        ("-skip", Arity::Word),
        ("-tags", Arity::Word),
        ("-timeout", Arity::Word),
        ("-bench", Arity::Word),
        ("-benchtime", Arity::Word),
        ("-shuffle", Arity::Word),
        ("-vet", Arity::Word),
        ("-count", Arity::Num),
        ("-parallel", Arity::Num),
        ("-p", Arity::Num),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const GO_BUILD: Spec = Spec {
    flags: &[
        ("-v", Arity::Bool),
        ("-race", Arity::Bool),
        ("-trimpath", Arity::Bool),
        ("-tags", Arity::Word),
        ("-p", Arity::Num),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const GO_VET: Spec = Spec {
    flags: &[("-v", Arity::Bool), ("-tags", Arity::Word)],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const MAKE: Spec = Spec {
    flags: &[
        ("-k", Arity::Bool),
        ("-s", Arity::Bool),
        ("-n", Arity::Bool),
        ("--keep-going", Arity::Bool),
        ("--silent", Arity::Bool),
        ("--quiet", Arity::Bool),
        ("--dry-run", Arity::Bool),
        ("-j", Arity::Num),
        ("--jobs", Arity::Num),
    ],
    pos: Pos::Targets(&["test", "check"]),
    harness: None,
    lints: false,
};

const MVN: Spec = Spec {
    flags: &[
        ("-q", Arity::Bool),
        ("-B", Arity::Bool),
        ("-o", Arity::Bool),
        ("-e", Arity::Bool),
        ("--quiet", Arity::Bool),
        ("--batch-mode", Arity::Bool),
        ("--offline", Arity::Bool),
        ("--errors", Arity::Bool),
        ("--no-transfer-progress", Arity::Bool),
        ("-Dtest", Arity::Word),
    ],
    pos: Pos::Targets(&["test"]),
    harness: None,
    lints: false,
};

const GRADLE: Spec = Spec {
    flags: &[
        ("-q", Arity::Bool),
        ("-i", Arity::Bool),
        ("--quiet", Arity::Bool),
        ("--info", Arity::Bool),
        ("--warn", Arity::Bool),
        ("--offline", Arity::Bool),
        ("--no-daemon", Arity::Bool),
        ("--stacktrace", Arity::Bool),
        ("--rerun-tasks", Arity::Bool),
        ("--continue", Arity::Bool),
        ("--parallel", Arity::Bool),
        ("--console", Arity::Word),
        ("--tests", Arity::Word),
        ("--max-workers", Arity::Num),
    ],
    pos: Pos::Targets(&["test"]),
    harness: None,
    lints: false,
};

const DOTNET: Spec = Spec {
    flags: &[
        ("--no-build", Arity::Bool),
        ("--no-restore", Arity::Bool),
        ("--nologo", Arity::Bool),
        ("-c", Arity::Word),
        ("--configuration", Arity::Word),
        ("-v", Arity::Word),
        ("--verbosity", Arity::Word),
        ("-f", Arity::Word),
        ("--framework", Arity::Word),
        ("--filter", Arity::Word),
    ],
    pos: Pos::Operands,
    harness: None,
    lints: false,
};

const SWIFT: Spec = Spec {
    flags: &[
        ("--parallel", Arity::Bool),
        ("--skip-build", Arity::Bool),
        ("--enable-code-coverage", Arity::Bool),
        ("--verbose", Arity::Bool),
        ("-v", Arity::Bool),
        ("--filter", Arity::Word),
        ("--skip", Arity::Word),
        ("-c", Arity::Word),
        ("--configuration", Arity::Word),
    ],
    pos: Pos::None,
    harness: None,
    lints: false,
};

const COMMAND_KEYS: &[&str] = &[
    "action",
    "command",
    "cmd",
    "cwd",
    "working_dir",
    "timeout_ms",
    "timeout",
    "timeout_secs",
    "background",
    "tty",
    "combined_output",
    "description",
];

fn runner_for<'a>(tokens: &'a [&'a str]) -> Option<(&'static Spec, &'a [&'a str], &'a str)> {
    let rest = |n: usize| &tokens[n..];
    let head = *tokens.first()?;
    let spec = match tokens {
        ["python" | "python3", "-m", "unittest", ..] => return Some((&UNITTEST, rest(3), head)),
        ["python" | "python3", "-m", "pytest", ..] => return Some((&PYTEST, rest(3), head)),
        ["pytest", ..] => return Some((&PYTEST, rest(1), head)),
        ["npm" | "pnpm" | "yarn", "test", ..] => (&NODE_PM, 2),
        ["bun", "test", ..] => (&BUN_TEST, 2),
        ["go", "test", ..] => (&GO_TEST, 2),
        ["go", "build", ..] => (&GO_BUILD, 2),
        ["go", "vet", ..] => (&GO_VET, 2),
        ["cargo", "test", ..] => (&CARGO_TEST, 2),
        ["cargo", "check" | "build", ..] => (&CARGO_BUILD, 2),
        ["cargo", "clippy", ..] => (&CARGO_CLIPPY, 2),
        ["cargo", "nextest", "run", ..] => (&NEXTEST, 3),
        ["make", ..] => (&MAKE, 1),
        ["mvn", ..] => (&MVN, 1),
        ["gradle", ..] => (&GRADLE, 1),
        ["dotnet", "test", ..] => (&DOTNET, 2),
        ["swift", "test", ..] => (&SWIFT, 2),
        _ => return None,
    };
    Some((spec.0, rest(spec.1), head))
}

fn word_ok(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '_' | '.' | ':' | ',' | '/' | '=' | '+' | '@' | '%' | '-'
                )
        })
}

fn is_number(text: &str) -> bool {
    (1..=5).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn lookup(flags: Flags, name: &str) -> Option<Arity> {
    flags
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, arity)| *arity)
}

struct Places<'a> {
    workspace: Option<&'a Path>,
    base: Option<PathBuf>,
}

impl Places<'_> {
    fn inside(&self, raw: &str) -> bool {
        let (Some(workspace), Some(base)) = (self.workspace, self.base.as_deref()) else {
            return false;
        };
        path_inside_workspace(workspace, base, raw)
    }
}

fn path_inside_workspace(workspace: &Path, base: &Path, raw: &str) -> bool {
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let lexical = normalize_path(&joined);
    if !lexical.starts_with(normalize_path(workspace)) {
        return false;
    }
    let Ok(root) = workspace.canonicalize() else {
        return false;
    };
    let mut probe = lexical.as_path();
    loop {
        if let Ok(real) = probe.canonicalize() {
            return real.starts_with(&root);
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return false,
        }
    }
}

fn operand_ok(token: &str, places: &Places<'_>) -> bool {
    word_ok(token) && !token.starts_with('-') && !token.contains('=') && places.inside(token)
}

fn value_ok(arity: Arity, value: &str, places: &Places<'_>) -> bool {
    if !word_ok(value) || value.starts_with('-') {
        return false;
    }
    match arity {
        Arity::Bool => false,
        Arity::Word => true,
        Arity::Num => is_number(value),
        Arity::Path => places.inside(value),
    }
}

fn args_ok(spec: &Spec, args: &[&str], places: &Places<'_>) -> bool {
    let mut targets = 0usize;
    let mut index = 0usize;
    while index < args.len() {
        let token = args[index];
        index += 1;
        if token == "--" {
            return match spec.harness {
                Some(harness) => {
                    targets_ok(spec, targets) && args_ok(harness, &args[index..], places)
                }
                None => false,
            };
        }
        if spec.lints
            && token.len() >= 2
            && matches!(token.get(..2), Some("-D" | "-W" | "-A" | "-F"))
        {
            let value = if token.len() == 2 {
                let Some(next) = args.get(index) else {
                    return false;
                };
                index += 1;
                next
            } else {
                &token[2..]
            };
            if !word_ok(value) || value.contains('=') || value.starts_with('-') {
                return false;
            }
            continue;
        }
        if token.len() > 1 && token.starts_with('-') {
            let (name, inline) = match token.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (token, None),
            };
            let Some(arity) = lookup(spec.flags, name) else {
                if !token.starts_with("--")
                    && token.len() > 2
                    && let Some(short) = token.get(..2)
                    && matches!(lookup(spec.flags, short), Some(Arity::Num))
                    && is_number(&token[2..])
                {
                    continue;
                }
                return false;
            };
            match arity {
                Arity::Bool => {
                    if inline.is_some() {
                        return false;
                    }
                }
                _ => {
                    let value = match inline {
                        Some(value) => value,
                        None => {
                            let Some(next) = args.get(index) else {
                                return false;
                            };
                            index += 1;
                            next
                        }
                    };
                    if !value_ok(arity, value, places) {
                        return false;
                    }
                }
            }
            continue;
        }
        match spec.pos {
            Pos::None => return false,
            Pos::Operands => {
                if !operand_ok(token, places) {
                    return false;
                }
            }
            Pos::Targets(allowed) => {
                if !allowed.contains(&token) {
                    return false;
                }
                targets += 1;
            }
        }
    }
    targets_ok(spec, targets)
}

fn targets_ok(spec: &Spec, targets: usize) -> bool {
    match spec.pos {
        Pos::Targets(_) => targets == 1,
        _ => true,
    }
}

fn redirect_token(token: &str) -> bool {
    matches!(token, "2>&1" | "2>/dev/null" | ">/dev/null")
}

fn trailing_filter_ok(text: &str) -> bool {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    match tokens.as_slice() {
        ["head" | "tail", "-n", count] => is_number(count),
        ["grep", literal] => word_ok(literal) && !literal.starts_with('-'),
        _ => false,
    }
}

fn command_text(params: &Value) -> Option<&str> {
    params
        .get("command")
        .or_else(|| params.get("cmd"))
        .and_then(Value::as_str)
}

pub(super) fn shell_params_are_routine_runner(params: &Value, workspace: Option<&Path>) -> bool {
    let Some(object) = params.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !COMMAND_KEYS.contains(&key.as_str()))
    {
        return false;
    }
    if object
        .get("action")
        .is_some_and(|action| action.as_str() != Some("run"))
    {
        return false;
    }
    let Some(command) = command_text(params) else {
        return false;
    };
    if command.len() > 1024
        || !command.chars().all(|ch| {
            ch == ' '
                || ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '_' | '.' | ':' | ',' | '/' | '=' | '+' | '@' | '%' | '-' | '|' | '>' | '&'
                )
        })
    {
        return false;
    }

    let mut parts = command.split('|');
    let runner_text = parts.next().unwrap_or_default();
    if let Some(filter) = parts.next()
        && (parts.next().is_some() || !trailing_filter_ok(filter))
    {
        return false;
    }
    let mut tokens = runner_text.split_whitespace().collect::<Vec<_>>();
    let mut redirects = 0;
    while tokens.last().is_some_and(|token| redirect_token(token)) {
        tokens.pop();
        redirects += 1;
    }
    if redirects > 2 {
        return false;
    }

    let Some((spec, args, head)) = runner_for(&tokens) else {
        return false;
    };

    let expansion = codewhale_execpolicy::shell_expand::expand_command(command);
    if expansion.dynamic
        || expansion.nested
        || expansion.arguments_dynamic
        || expansion.commands.is_empty()
        || !expansion.commands.iter().all(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|first| first == head || matches!(first, "head" | "tail" | "grep"))
        })
    {
        return false;
    }

    let mut places = Places {
        workspace,
        base: workspace.map(Path::to_path_buf),
    };
    let directory = ["cwd", "working_dir"]
        .into_iter()
        .find_map(|key| object.get(key).filter(|value| !value.is_null()));
    if let Some(directory) = directory {
        let Some(raw) = directory.as_str() else {
            return false;
        };
        let Some(workspace) = workspace else {
            return false;
        };
        if raw.is_empty()
            || raw.contains(['\0', '~', '$', '*', '?', '[', ']', '{', '}'])
            || !path_inside_workspace(workspace, workspace, raw)
        {
            return false;
        }
        let path = Path::new(raw);
        places.base = Some(if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        });
    }

    args_ok(spec, args, &places)
}

fn git_discards(argv: &[&str]) -> bool {
    let Some(at) = argv
        .iter()
        .skip(1)
        .position(|word| matches!(*word, "checkout" | "restore" | "switch"))
        .map(|at| at + 1)
    else {
        return false;
    };
    if at != 1 {
        return true;
    }
    let rest = &argv[2..];
    let plain = |word: &str| word_ok(word) && !word.starts_with('-');
    match argv[1] {
        "restore" => true,
        "checkout" => {
            !matches!(rest, [flag @ ("-b" | "-B"), name] | [flag @ ("-b" | "-B"), name, _]
            if plain(name) && (rest.len() == 2 || plain(rest[2])) && !flag.is_empty())
        }
        _ => rest.iter().any(|word| {
            matches!(*word, "--force" | "--discard-changes" | "--force-create")
                || (word.starts_with('-')
                    && !word.starts_with("--")
                    && word.chars().skip(1).any(|ch| matches!(ch, 'f' | 'C')))
        }),
    }
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn normalized_command_word(token: &str) -> String {
    let normalized = token.trim_matches(['"', '\'']).replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or(&normalized);
    base.strip_suffix(".exe")
        .unwrap_or(base)
        .to_ascii_lowercase()
}

fn strip_env_prefix(tokens: &[String]) -> &[String] {
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if normalized_command_word(token) == "env" {
            index += 1;
            while index < tokens.len()
                && (tokens[index].starts_with('-') || is_env_assignment(&tokens[index]))
            {
                index += 1;
            }
            continue;
        }
        if is_env_assignment(token) {
            index += 1;
            continue;
        }
        break;
    }
    &tokens[index..]
}

fn argv_readings(segment: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    if let Some(words) = shlex::split(segment) {
        out.push(words);
    } else {
        out.push(
            segment
                .split_whitespace()
                .map(|word| word.replace(['"', '\''], ""))
                .filter(|word| !word.is_empty())
                .collect(),
        );
    }
    let expansion = codewhale_execpolicy::shell_expand::expand_command(segment);
    out.extend(expansion.words);
    out
}

pub(super) fn command_discards_work(command: &str) -> bool {
    let expansion = codewhale_execpolicy::shell_expand::expand_command(command);
    if expansion.words.iter().any(|words| {
        words
            .split(|word| matches!(word.as_str(), ";" | "&" | "&&" | "|" | "||"))
            .any(|segment| {
                let slice = strip_env_prefix(segment);
                let refs = slice.iter().map(String::as_str).collect::<Vec<_>>();
                refs.first()
                    .is_some_and(|first| normalized_command_word(first) == "git")
                    && git_discards(&refs)
            })
    }) {
        return true;
    }
    let mut lines = expansion
        .commands
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    lines.push(command);
    lines.into_iter().any(|line| {
        line.split(['\n', ';', '&', '|']).any(|segment| {
            argv_readings(segment).into_iter().any(|words| {
                let slice = strip_env_prefix(&words);
                let refs = slice.iter().map(String::as_str).collect::<Vec<_>>();
                refs.first()
                    .is_some_and(|first| normalized_command_word(first) == "git")
                    && git_discards(&refs)
            })
        })
    })
}

pub(super) fn segment_is_runner_class(segment: &str) -> bool {
    argv_readings(segment).into_iter().any(|words| {
        let slice = strip_env_prefix(&words);
        let Some(head) = slice.first() else {
            return false;
        };
        match normalized_command_word(head).as_str() {
            "cargo" => slice.get(1).is_some_and(|sub| {
                matches!(
                    sub.trim_matches(['"', '\'']),
                    "test" | "check" | "build" | "clippy" | "nextest"
                )
            }),
            "make" => true,
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::{AutoReviewAction, AutoReviewContext, AutoReviewPolicy, RunOrigin};
    use codewhale_execpolicy::ApprovalMode;
    use serde_json::{Value, json};

    fn decide(params: &Value, workspace: &std::path::Path, mode: ApprovalMode) -> AutoReviewAction {
        let ctx = AutoReviewContext::from_tool_call(
            "exec_shell",
            params,
            RunOrigin::Interactive,
            mode,
            true,
            Some(workspace),
        );
        AutoReviewPolicy::default().evaluate(&ctx).action
    }

    fn routine(command: &str, workspace: &std::path::Path) -> bool {
        super::shell_params_are_routine_runner(&json!({ "command": command }), Some(workspace))
    }

    fn auto_allows(params: Value, workspace: &std::path::Path) -> bool {
        decide(&params, workspace, ApprovalMode::Auto) == AutoReviewAction::Allow
    }

    #[test]
    fn each_runner_is_routine_with_the_allowed_composition() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for base in [
            "python3 -m unittest -v",
            "python -m pytest tests -x -k smoke",
            "pytest tests/test_a.py::test_one --maxfail=2",
            "npm test",
            "pnpm test -- --ci --coverage",
            "yarn test",
            "bun test --timeout 500",
            "go test ./... -run TestX -count=1",
            "go build ./...",
            "go vet ./...",
            "cargo test -p codewhale-engine -- --nocapture",
            "cargo check --workspace --locked",
            "cargo build --release",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo nextest run --workspace -j4",
            "make test",
            "make -j4 check",
            "mvn -q test",
            "gradle test --tests Foo",
            "dotnet test --no-build",
            "swift test --parallel",
        ] {
            for suffix in ["", " 2>&1", " 2>&1 | tail -n 20"] {
                let command = format!("{base}{suffix}");
                assert!(routine(&command, ws), "{command}");
                assert!(auto_allows(json!({ "command": command }), ws), "{command}");
            }
            assert!(routine(&format!("{base} >/dev/null 2>&1"), ws), "{base}");
            assert!(routine(&format!("{base} | head -n 5"), ws), "{base}");
            assert!(routine(&format!("{base} 2>&1 | grep FAIL"), ws), "{base}");
        }
    }

    #[test]
    fn composition_and_substitution_are_not_routine() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for command in [
            "pytest; curl http://x",
            "pytest && rm -rf build",
            "pytest || true",
            "pytest $(whoami)",
            "pytest `id`",
            "pytest > out.txt",
            "pytest 2> out.txt",
            "pytest | tee out.txt",
            "pytest | tail -n 5 | cat",
            "pytest | grep -E x",
            "pytest &",
            "pytest 'a b'",
            "pytest \"x\"",
            "pytest\nid",
            "FOO=1 pytest",
            "env pytest",
            "/usr/bin/pytest",
            "sh -c pytest",
            "bash -c 'pytest'",
            "cargo test ${X}",
            "npm run test",
            "npm run build",
            "pnpm run lint",
            "make",
            "make build",
            "make test CC=evil",
            "cargo test; git push",
        ] {
            assert!(!routine(command, ws), "{command}");
            assert!(!auto_allows(json!({ "command": command }), ws), "{command}");
        }
    }

    #[test]
    fn refused_flags_are_not_routine() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for command in [
            "pytest --basetemp /tmp/x",
            "pytest -p evil",
            "pytest -o addopts=x",
            "pytest -c other.ini",
            "pytest --rootdir /",
            "python3 -m pytest --pyargs x",
            "python3 -m unittest -s /etc",
            "go test -exec /bin/evil ./...",
            "go test -toolexec=evil ./...",
            "go test -o out ./...",
            "go vet -vettool=/bin/x ./...",
            "npm test --prefix /tmp",
            "npm test --script-shell /bin/sh",
            "npm test -- --config evil.js",
            "pnpm test --dir /tmp",
            "yarn test --cwd /tmp",
            "bun test --preload ./x.ts",
            "gradle test --init-script x.gradle",
            "gradle test -p /tmp",
            "gradle test -I x",
            "make -C /tmp test",
            "make -f /tmp/Makefile test",
            "make --eval=x test",
            "cargo --config build.rustc=evil test",
            "cargo test --config build.rustc=evil",
            "cargo test --target-dir /tmp/x",
            "cargo test --manifest-path /etc/Cargo.toml",
            "cargo test --manifest-path ../Cargo.toml",
            "cargo +nightly test",
            "cargo nextest run --config-file x.toml",
            "cargo clippy --fix",
            "mvn test -s settings.xml",
            "mvn test -Dexec.mainClass=x",
            "dotnet test --settings x.runsettings",
            "dotnet test --logger x",
            "swift test --package-path /tmp",
            "swift test -Xswiftc -x",
        ] {
            assert!(!routine(command, ws), "{command}");
        }
    }

    #[test]
    fn paths_outside_the_workspace_are_not_routine() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::create_dir(ws.join("sub")).unwrap();
        assert!(routine("pytest sub", ws));
        for command in [
            "pytest /etc",
            "pytest ../other",
            "pytest sub/../../other",
            "go test ../...",
            "cargo test --manifest-path /tmp/Cargo.toml",
        ] {
            assert!(!routine(command, ws), "{command}");
        }
        let outside = tempfile::tempdir().unwrap();
        let params = json!({ "command": "pytest", "cwd": outside.path().to_str().unwrap() });
        assert!(!super::shell_params_are_routine_runner(&params, Some(ws)));
        assert!(!super::shell_params_are_routine_runner(
            &json!({ "command": "pytest", "cwd": ".." }),
            Some(ws)
        ));
        assert!(super::shell_params_are_routine_runner(
            &json!({ "command": "pytest", "cwd": "sub" }),
            Some(ws)
        ));
        for extra in [
            json!({ "command": "pytest", "env": { "PYTEST_ADDOPTS": "-p evil" } }),
            json!({ "command": "pytest", "stdin": "x" }),
            json!({ "command": "pytest", "sandbox_permissions": "workspace-write" }),
            json!({ "command": "pytest", "action": "interact" }),
        ] {
            assert!(
                !super::shell_params_are_routine_runner(&extra, Some(ws)),
                "{extra}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_workspace_is_not_routine() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(!routine("pytest link", dir.path()));
        assert!(!routine("pytest link/new", dir.path()));
    }

    #[test]
    fn discard_forms_are_never_routine_in_auto_review() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for command in [
            "git restore .",
            "git restore src/a.rs",
            "git restore --staged x",
            "git checkout -- .",
            "git checkout -- src/a.rs",
            "git checkout HEAD src/a.rs",
            "git checkout -f",
            "git checkout main",
            "git checkout src/a.rs",
            "git switch -f main",
            "git switch --discard-changes main",
            "git switch --force main",
            "git -C . checkout -f",
            "git 'restore' .",
            "/usr/bin/git restore .",
            "git status && git restore .",
        ] {
            assert!(!auto_allows(json!({ "command": command }), ws), "{command}");
        }
        for command in [
            "git switch main",
            "git switch -c feature-x",
            "git checkout -b feature-x",
            "git checkout -B feature-x",
        ] {
            assert!(auto_allows(json!({ "command": command }), ws), "{command}");
        }
    }

    #[test]
    fn ask_and_accept_edits_are_unchanged_for_the_same_commands() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for command in [
            "pytest",
            "cargo test --config x",
            "git restore .",
            "python3 -m unittest 2>&1",
        ] {
            for mode in [ApprovalMode::Suggest, ApprovalMode::Never] {
                let params = json!({ "command": command });
                let ctx = AutoReviewContext::from_tool_call(
                    "exec_shell",
                    &params,
                    RunOrigin::Interactive,
                    mode,
                    true,
                    Some(ws),
                );
                assert_ne!(
                    AutoReviewPolicy::default().evaluate(&ctx).reason,
                    "Auto-Review allows a proven read/build/test shell command",
                    "{command}"
                );
            }
        }
    }

    #[test]
    fn unproven_shell_hold_names_the_routine_list() {
        let dir = tempfile::tempdir().unwrap();
        let params = json!({ "command": "python3 -m unittest && curl x.test" });
        let ctx = AutoReviewContext::from_tool_call(
            "exec_shell",
            &params,
            RunOrigin::Interactive,
            ApprovalMode::Auto,
            true,
            Some(dir.path()),
        );
        let decision = AutoReviewPolicy::default().evaluate(&ctx);
        assert_eq!(decision.action, AutoReviewAction::AskUser);
        assert!(
            decision.reason.contains("routine list"),
            "{}",
            decision.reason
        );
    }

    #[test]
    fn disguised_runner_or_discard_forms_do_not_bypass_review() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for command in [
            r#"cargo "test" --manifest-path /etc/Cargo.toml"#,
            r#""cargo" test --config build.rustc=evil"#,
            r#"cargo 'build' --target-dir /tmp/x"#,
            r#"'make' -f /tmp/evil.mk test"#,
            r#""/usr/bin/make" -f /tmp/evil.mk"#,
            r#""/usr/bin/cargo" test --target-dir /tmp/x"#,
            "FOO=1 make -f /tmp/evil.mk",
            "env cargo test --target-dir /tmp/x",
            r#""git" "restore" ."#,
            "FOO=1 git restore .",
            "env git checkout -- .",
        ] {
            assert!(!auto_allows(json!({ "command": command }), ws), "{command}");
        }
    }
}
