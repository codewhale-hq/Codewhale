//! Expand a shell command line into the set of commands a shell would run.
//!
//! Deny rules are the one gate that holds under `AskForApproval::Never`, so
//! they cannot be matched against the raw command string: the string a user
//! types and the set of commands the shell executes are different things. A
//! command substitution runs its body (`` `rm -rf /` ``, `$(rm -rf /)`), a
//! quoted argument executes with the quotes removed (`rm -rf "/"`), and a
//! wrapper hands its payload straight back to a shell (`bash -c '…'`,
//! `eval '…'`, `sudo …`).
//!
//! Matching one string pattern per metacharacter loses that race by
//! construction — every new quoting or wrapping form is another bypass. This
//! module instead tokenizes the command the way a POSIX shell word-splits it
//! and returns *every* command line that would actually be executed, so deny
//! rules can be matched against each one.
//!
//! Deliberately conservative in the deny direction: when a construct is
//! ambiguous the expander emits extra candidate command lines rather than
//! fewer. Over-emitting only makes deny matching stricter — `denied_prefix_matches`
//! stays anchored at the first positional token, so an extra candidate that no
//! rule names is inert. Under-emitting is a bypass.
//!
//! What it does *not* do is evaluate anything: `$VAR` is left as literal text,
//! and single-quoted text is never treated as code (`echo '` + "`" + `rm -rf /`" +
//! "`" + `'` really does just print). Fidelity to shell semantics is the point in
//! both directions.
//!
//! Because nothing is evaluated, some command words cannot be known without
//! running the shell: a parameter or command substitution (`$v`, `${v}`,
//! `$(…)`), a glob or brace expansion (`r[m]`, `{rm,-f,x}`), ANSI-C escapes,
//! a command word that is not a plain literal (`=rm`, `/bin/(r)m`), a builtin
//! that rebinds names (`alias`, `hash x=…`, `setopt`), and a shell that reads
//! its script from a pipe, a here-string, or a process substitution.
//! [`Expansion::dynamic`] reports that case so the policy engine
//! can fail closed instead of matching deny rules against text the shell will
//! rewrite. [`Expansion::nested`] reports that some code runs inside another
//! command (a substitution, `eval`, a `-c` payload, `find -exec`), which is
//! never covered by an allow rule written for the outer command.
//!
//! Known limits: a script *file* (`bash ./x.sh`, `. ./env.sh`) is opaque
//! (a descriptor path such as `/dev/stdin` is not a file: it is dynamic), as
//! is any program that interprets its arguments as code (`python -c`, `ssh
//! host cmd`, `git -c alias.x=!cmd`). Aliases and functions defined in an
//! earlier call are not tracked. Arithmetic contexts (`(( ))`, `$(( ))`,
//! `let`, `[[ … -eq … ]]`) evaluate the *values* of variables they name, so a
//! value holding `a[$(cmd)]` runs `cmd` without it appearing in the text.
//! `cmd /c` and PowerShell `-Command` payloads are scanned with this POSIX
//! grammar, which finds their command words but not every cmd.exe or
//! PowerShell construct.

use std::collections::HashSet;

/// Maximum nesting depth followed through substitutions and `-c` payloads.
const MAX_DEPTH: usize = 8;

/// Upper bound on emitted command lines, so a pathological input cannot turn
/// one policy check into unbounded work.
const MAX_COMMANDS: usize = 256;

/// Upper bound on the parser states explored while locating the real command
/// word behind wrapper words. Running out marks the command unresolved.
const MAX_HEAD_STATES: usize = 512;

/// A word that prefixes another command rather than being the command: the
/// real invocation is what follows. Stripping it keeps `sudo rm -rf /`
/// matchable by an `rm -rf /` rule.
///
/// The option grammar matters: an option that takes a separate value
/// (`sudo -u root`, `timeout -s KILL`) and a required operand
/// (`chroot NEWROOT`, `timeout DURATION`) both sit between the wrapper and the
/// command it runs. An option missing from the table may or may not take a
/// value, so both readings are explored.
struct Wrapper {
    name: &'static str,
    /// Short options whose value is the rest of the word or the next word.
    short_values: &'static str,
    /// Short options known to take no value. Any other short option may or
    /// may not take one, so both readings are explored.
    short_switches: &'static str,
    /// Long options whose value may be the next word.
    long_values: &'static [&'static str],
    /// Long options known to take no value.
    long_switches: &'static [&'static str],
    /// Required operands before the command (`chroot NEWROOT`).
    operands: u8,
}

static WRAPPERS: &[Wrapper] = &[
    Wrapper {
        name: "sudo",
        short_values: "ughpCDrtTRUac",
        short_switches: "AbBEeHiKklNnPSsVv",
        long_values: &[
            "user",
            "group",
            "host",
            "prompt",
            "close-from",
            "chdir",
            "role",
            "type",
            "command-timeout",
            "chroot",
            "other-user",
            "auth-type",
            "login-class",
        ],
        long_switches: &[
            "login",
            "preserve-env",
            "non-interactive",
            "background",
            "edit",
            "shell",
            "stdin",
            "reset-timestamp",
            "remove-timestamp",
            "validate",
            "list",
            "set-home",
            "bell",
            "askpass",
            "preserve-groups",
        ],
        operands: 0,
    },
    Wrapper {
        name: "doas",
        short_values: "uCa",
        short_switches: "Lns",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "env",
        short_values: "uCSPa",
        short_switches: "i0v",
        long_values: &["unset", "chdir", "argv0"],
        long_switches: &["ignore-environment", "null", "debug"],
        operands: 0,
    },
    Wrapper {
        name: "nohup",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "nice",
        short_values: "n",
        short_switches: "",
        long_values: &["adjustment"],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "ionice",
        short_values: "cnpPu",
        short_switches: "t",
        long_values: &["class", "classdata", "pid", "pgid", "uid"],
        long_switches: &["ignore"],
        operands: 0,
    },
    Wrapper {
        name: "time",
        short_values: "fo",
        short_switches: "aplqvh",
        long_values: &["format", "output"],
        long_switches: &["append", "portability", "verbose", "quiet"],
        operands: 0,
    },
    Wrapper {
        name: "timeout",
        short_values: "sk",
        short_switches: "fpv",
        long_values: &["signal", "kill-after"],
        long_switches: &["preserve-status", "foreground", "verbose"],
        operands: 1,
    },
    Wrapper {
        name: "stdbuf",
        short_values: "ioe",
        short_switches: "",
        long_values: &["input", "output", "error"],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "setsid",
        short_values: "",
        short_switches: "cfw",
        long_values: &[],
        long_switches: &["ctty", "fork", "wait"],
        operands: 0,
    },
    Wrapper {
        name: "command",
        short_values: "",
        short_switches: "pvV",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "builtin",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "exec",
        short_values: "a",
        short_switches: "cl",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "xargs",
        short_values: "adEILnPsJSR",
        short_switches: "0eiloprtx",
        long_values: &[
            "arg-file",
            "delimiter",
            "eof",
            "replace",
            "max-lines",
            "max-args",
            "max-procs",
            "max-chars",
            "process-slot-var",
        ],
        long_switches: &[
            "null",
            "no-run-if-empty",
            "interactive",
            "verbose",
            "exit",
            "open-tty",
            "show-limits",
        ],
        operands: 0,
    },
    Wrapper {
        name: "unbuffer",
        short_values: "",
        short_switches: "p",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "busybox",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "chroot",
        short_values: "ugG",
        short_switches: "",
        long_values: &["userspec", "groups"],
        long_switches: &["skip-chdir"],
        operands: 1,
    },
    Wrapper {
        name: "proot",
        short_values: "rbmwqkRSvi",
        short_switches: "0Vh",
        long_values: &[
            "rootfs",
            "bind",
            "mount",
            "cwd",
            "pwd",
            "qemu",
            "kernel-release",
            "verbose",
        ],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "caffeinate",
        short_values: "tw",
        short_switches: "dimsu",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "arch",
        short_values: "ed",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "sandbox-exec",
        short_values: "fnpD",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "noglob",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "nocorrect",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "-",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "nsenter",
        short_values: "tSG",
        short_switches: "amuinpUCTrwFZcy",
        long_values: &["target", "setuid", "setgid"],
        long_switches: &["all", "preserve-credentials", "no-fork"],
        operands: 0,
    },
    Wrapper {
        name: "unshare",
        short_values: "SGRw",
        short_switches: "mupinUCTrcfk",
        long_values: &["root", "wd", "setuid", "setgid", "propagation", "setgroups"],
        long_switches: &[
            "fork",
            "mount-proc",
            "map-root-user",
            "map-current-user",
            "kill-child",
        ],
        operands: 0,
    },
    Wrapper {
        name: "runuser",
        short_values: "ugGcswf",
        short_switches: "lmpP",
        long_values: &[
            "user",
            "group",
            "supp-group",
            "command",
            "shell",
            "whitelist-environment",
            "session-command",
        ],
        long_switches: &["login", "preserve-environment", "pty"],
        operands: 0,
    },
    Wrapper {
        name: "flock",
        short_values: "wEc",
        short_switches: "sxunoFh",
        long_values: &["timeout", "wait", "conflict-exit-code", "command"],
        long_switches: &[
            "shared",
            "exclusive",
            "unlock",
            "nonblock",
            "nb",
            "close",
            "no-fork",
            "verbose",
        ],
        operands: 1,
    },
    Wrapper {
        name: "watch",
        short_values: "nq",
        short_switches: "bcdeghprtwx",
        long_values: &["interval", "equexit"],
        long_switches: &[
            "beep",
            "color",
            "no-color",
            "differences",
            "errexit",
            "chgexit",
            "precise",
            "no-title",
            "no-wrap",
            "exec",
        ],
        operands: 0,
    },
    Wrapper {
        name: "wsl",
        short_values: "du",
        short_switches: "e",
        long_values: &["distribution", "user", "cd", "shell-type"],
        long_switches: &["exec"],
        operands: 0,
    }, // Privilege, sandbox, scheduling and tracing launchers: each runs its
    // operands as a command.
    Wrapper {
        name: "pkexec",
        short_values: "",
        short_switches: "",
        long_values: &["user"],
        long_switches: &["disable-internal-agent", "keep-cwd"],
        operands: 0,
    },
    Wrapper {
        name: "run0",
        short_values: "ugD",
        short_switches: "",
        long_values: &[
            "user",
            "group",
            "nice",
            "chdir",
            "setenv",
            "unit",
            "property",
            "description",
            "slice",
            "machine",
        ],
        long_switches: &["no-ask-password", "background", "pty", "pipe"],
        operands: 0,
    },
    Wrapper {
        name: "fakeroot",
        short_values: "lsib",
        short_switches: "uhv",
        long_values: &["lib", "faked"],
        long_switches: &["unknown-is-real"],
        operands: 0,
    },
    Wrapper {
        name: "taskset",
        short_values: "",
        short_switches: "acpV",
        long_values: &[],
        long_switches: &["all-tasks", "cpu-list", "pid"],
        operands: 1,
    },
    Wrapper {
        name: "chrt",
        short_values: "TPD",
        short_switches: "abdefimoprRv",
        long_values: &["sched-runtime", "sched-period", "sched-deadline"],
        long_switches: &[
            "all-tasks",
            "batch",
            "deadline",
            "fifo",
            "idle",
            "other",
            "pid",
            "rr",
            "reset-on-fork",
            "verbose",
            "max",
        ],
        operands: 1,
    },
    Wrapper {
        name: "prlimit",
        short_values: "po",
        short_switches: "",
        long_values: &["pid", "output"],
        long_switches: &["noheadings", "raw", "verbose"],
        operands: 0,
    },
    Wrapper {
        name: "strace",
        short_values: "abeIoOpPsSuEX",
        short_switches: "cCdDfFhikNqrtTvVwxyYzZ",
        long_values: &[
            "output",
            "expr",
            "trace",
            "signal",
            "status",
            "attach",
            "user",
            "env",
            "string-limit",
            "trace-path",
        ],
        long_switches: &[
            "follow-forks",
            "output-separately",
            "summary-only",
            "summary",
            "no-abbrev",
            "verbose",
        ],
        operands: 0,
    },
    Wrapper {
        name: "ltrace",
        short_values: "aAeDFlnopsuxwX",
        short_switches: "bcCfhiLrStTV",
        long_values: &["output", "library", "indent", "align"],
        long_switches: &["demangle", "help", "version"],
        operands: 0,
    },
    Wrapper {
        name: "systemd-run",
        short_values: "puEHM",
        short_switches: "rtqGdPS",
        long_values: &[
            "property",
            "unit",
            "description",
            "slice",
            "setenv",
            "uid",
            "gid",
            "nice",
            "working-directory",
            "host",
            "machine",
            "service-type",
            "on-active",
            "on-boot",
            "on-startup",
            "on-unit-active",
            "on-unit-inactive",
            "on-calendar",
            "timer-property",
            "path-property",
            "socket-property",
        ],
        long_switches: &[
            "user",
            "system",
            "scope",
            "pty",
            "pipe",
            "wait",
            "collect",
            "quiet",
            "no-ask-password",
            "remain-after-exit",
            "same-dir",
            "no-block",
            "send-sighup",
        ],
        operands: 0,
    },
    Wrapper {
        name: "numactl",
        short_values: "NmCipPw",
        short_switches: "laHsS",
        long_values: &[
            "cpunodebind",
            "membind",
            "physcpubind",
            "interleave",
            "preferred",
            "preferred-many",
            "weighted-interleave",
            "huge",
            "offset",
            "length",
            "mode",
            "strict",
            "shmmode",
            "shmid",
            "shm",
            "file",
        ],
        long_switches: &["localalloc", "all", "hardware", "show", "touch"],
        operands: 0,
    },
    Wrapper {
        name: "firejail",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[
            "noprofile",
            "quiet",
            "private",
            "private-dev",
            "private-tmp",
            "nonewprivs",
            "noroot",
            "seccomp",
            "x11",
            "appimage",
            "allusers",
        ],
        operands: 0,
    },
    Wrapper {
        name: "xvfb-run",
        short_values: "efnpsw",
        short_switches: "alh",
        long_values: &[
            "error-file",
            "auth-file",
            "server-num",
            "xauth-protocol",
            "server-args",
            "wait",
        ],
        long_switches: &["auto-servernum", "listen-tcp", "help"],
        operands: 0,
    },
    Wrapper {
        name: "dbus-launch",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[
            "sh-syntax",
            "csh-syntax",
            "auto-syntax",
            "binary-syntax",
            "close-stderr",
            "exit-with-session",
            "exit-with-x11",
            "version",
        ],
        operands: 0,
    },
    Wrapper {
        name: "dbus-run-session",
        short_values: "",
        short_switches: "",
        long_values: &["config-file", "dbus-daemon"],
        long_switches: &["session", "version", "help"],
        operands: 0,
    },
    Wrapper {
        name: "sg",
        short_values: "",
        short_switches: "c",
        long_values: &[],
        long_switches: &[],
        operands: 1,
    },
    Wrapper {
        name: "proxychains",
        short_values: "f",
        short_switches: "q",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "proxychains4",
        short_values: "f",
        short_switches: "q",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "torsocks",
        short_values: "uapP",
        short_switches: "idqh",
        long_values: &["user", "pass", "address", "port"],
        long_switches: &["isolate", "debug", "quiet", "shell", "help", "version"],
        operands: 0,
    },
    Wrapper {
        name: "tsocks",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "eatmydata",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "cpulimit",
        short_values: "lpe",
        short_switches: "mzikvh",
        long_values: &["limit", "pid", "exe", "cpu"],
        long_switches: &[
            "monitor-forks",
            "lazy",
            "include-children",
            "kill",
            "verbose",
            "help",
        ],
        operands: 0,
    },
    Wrapper {
        name: "setpriv",
        short_values: "",
        short_switches: "",
        long_values: &[
            "reuid",
            "regid",
            "groups",
            "inh-caps",
            "ambient-caps",
            "bounding-set",
            "securebits",
            "pdeathsig",
            "selinux-label",
            "apparmor-profile",
            "landlock-access",
            "landlock-rule",
        ],
        long_switches: &[
            "clear-groups",
            "keep-groups",
            "init-groups",
            "nnp",
            "no-new-privs",
            "reset-env",
            "dump",
            "list-caps",
        ],
        operands: 0,
    },
    Wrapper {
        name: "trickle",
        short_values: "udwtlnL",
        short_switches: "svh",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    // GNU coreutils as installed on macOS (`brew install coreutils`).
    Wrapper {
        name: "gtimeout",
        short_values: "sk",
        short_switches: "fpv",
        long_values: &["signal", "kill-after"],
        long_switches: &["preserve-status", "foreground", "verbose"],
        operands: 1,
    },
    Wrapper {
        name: "gnice",
        short_values: "n",
        short_switches: "",
        long_values: &["adjustment"],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "gnohup",
        short_values: "",
        short_switches: "",
        long_values: &[],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "gstdbuf",
        short_values: "ioe",
        short_switches: "",
        long_values: &["input", "output", "error"],
        long_switches: &[],
        operands: 0,
    },
    Wrapper {
        name: "gchroot",
        short_values: "ugG",
        short_switches: "",
        long_values: &["userspec", "groups"],
        long_switches: &["skip-chdir"],
        operands: 1,
    },
];

/// Shell reserved words that can lead a simple command without being it:
/// `if rm x; then …`, `while rm x; do …`, `! rm x`.
const RESERVED_PREFIXES: &[&str] = &[
    "if", "then", "elif", "else", "do", "while", "until", "!", "coproc",
];

const REBINDING_BUILTINS: &[&str] = &[
    "alias", "enable", "setopt", "unsetopt", "emulate", "shopt", "autoload", "zmodload",
];

/// Shells whose `-c` argument is a command line to be parsed, not an operand.
const SHELL_NAMES: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "ksh93", "mksh", "ash", "fish", "csh", "tcsh", "rbash",
    "yash",
];

/// The commands a shell line would run, plus what could not be resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expansion {
    /// Every command line found, as [`expanded_commands`] returns them.
    pub commands: Vec<String>,
    pub words: Vec<Vec<String>>,
    /// A command word (or a shell's script) is only known at run time, so no
    /// deny rule can be matched against it reliably.
    pub dynamic: bool,
    /// Some code runs nested inside another command: a command or process
    /// substitution, `eval`, a shell `-c` payload or stdin script, or a
    /// `find -exec` payload.
    pub nested: bool,
    /// An argument's value is resolved only at execution time. A prefix grant
    /// cannot check whether that value introduces a write/execute option.
    /// Unlike `dynamic`, this does not make the command head unknowable.
    pub arguments_dynamic: bool,
    /// Unquoted control operators or grouping. Recorded before command
    /// deduplication: `echo x; echo x` still contains a command list.
    pub control: bool,
    /// Unquoted redirection syntax, including descriptor duplication and
    /// heredocs. Quoted operator characters and heredoc body data are excluded.
    pub redirects: bool,
}

/// Returns every command line the shell would execute for `command`.
///
/// Results contain word-split commands, substitutions and wrapper payloads.
/// Literal data, including quoted heredoc bodies, is not treated as code.
/// Windows scans retain native path separators as well as POSIX candidates.
pub fn expanded_commands(command: &str) -> Vec<String> {
    expand_command(command).commands
}

/// [`expanded_commands`] plus the flags a policy needs to fail closed.
pub fn expand_command(command: &str) -> Expansion {
    expand_for_platform(command, cfg!(windows))
}

#[cfg(test)]
fn expanded_commands_for_platform(command: &str, windows: bool) -> Vec<String> {
    expand_for_platform(command, windows).commands
}

fn expand_for_platform(command: &str, windows: bool) -> Expansion {
    let mut expander = Expander {
        out: Vec::new(),
        out_words: Vec::new(),
        seen: HashSet::new(),
        literal_backslashes: windows,
        dynamic: false,
        nested: false,
        arguments_dynamic: false,
        control: false,
        redirects: false,
    };
    // Native Windows shells preserve path separators. Also retain the POSIX
    // interpretation for Bash/WSL commands. Both passes use the same bounded,
    // heredoc-aware parser and only contribute deny targets, never grants.
    expander.expand(command, 0);
    if windows {
        expander.literal_backslashes = false;
        expander.expand(command, 0);
    }
    Expansion {
        commands: expander.out,
        words: expander.out_words,
        dynamic: expander.dynamic,
        nested: expander.nested,
        arguments_dynamic: expander.arguments_dynamic,
        control: expander.control,
        redirects: expander.redirects,
    }
}

struct Expander {
    out: Vec<String>,
    out_words: Vec<Vec<String>>,
    seen: HashSet<String>,
    literal_backslashes: bool,
    dynamic: bool,
    nested: bool,
    arguments_dynamic: bool,
    control: bool,
    redirects: bool,
}

/// The word being read, with what the parser learned about it.
#[derive(Default)]
struct WordBuf {
    text: String,
    started: bool,
    quoted: bool,
    redirect_operand: bool,
    /// The word's final text depends on an expansion the parser does not run.
    dynamic: bool,
    /// An unquoted `{` appeared, so brace expansion may rewrite the word.
    brace: bool,
    /// An unquoted `[` appeared, so the word may be a bracket glob.
    bracket: bool,
    odd: bool,
}

impl WordBuf {
    fn flush(&mut self, line: &mut Line) {
        if self.started || !self.text.is_empty() || self.dynamic {
            if self.redirect_operand {
                self.text.clear();
                self.redirect_operand = false;
            } else {
                // `[` alone is the test builtin and `{}` is a find
                // placeholder; only a closed bracket or a brace list expands.
                let dynamic = self.dynamic
                    || (self.bracket && self.text.len() > 1 && self.text.contains(']'))
                    || (self.brace && (self.text.contains(',') || self.text.contains("..")));
                line.words.push(std::mem::take(&mut self.text));
                line.dynamic.push(dynamic);
                line.odd.push(self.odd);
            }
            self.started = false;
        }
        self.quoted = false;
        self.dynamic = false;
        self.brace = false;
        self.bracket = false;
        self.odd = false;
    }
}

/// One simple command: its words, which of them are dynamic, and whether a
/// heredoc feeds its stdin.
#[derive(Default, Clone)]
struct Line {
    words: Vec<String>,
    dynamic: Vec<bool>,
    odd: Vec<bool>,
    heredoc: bool,
}

/// How a shell invocation receives its script.
enum ShellInput {
    /// `-c` / `--command`: the word at each offset may be the command line.
    Command(Vec<usize>),
    /// A script file operand at this offset.
    Script(usize),
    /// The script is read from stdin.
    Stdin,
}

impl Expander {
    fn emit(&mut self, tokens: &[String]) {
        let joined = tokens
            .iter()
            .filter(|token| !token.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if joined.is_empty() || self.seen.contains(&joined) {
            return;
        }
        if self.out.len() >= MAX_COMMANDS {
            // A dropped command line was never checked: unresolved.
            self.dynamic = true;
            return;
        }
        self.seen.insert(joined.clone());
        self.out.push(joined);
        self.out_words.push(
            tokens
                .iter()
                .filter(|token| !token.is_empty())
                .cloned()
                .collect(),
        );
    }

    /// Word-split `input` into command lines and record each one, recursing
    /// into every nested command text found along the way.
    fn expand(&mut self, input: &str, depth: usize) {
        if depth > MAX_DEPTH || self.out.len() >= MAX_COMMANDS {
            // Unexamined code is unresolved code.
            self.dynamic = true;
            return;
        }
        if depth > 0 && !input.trim().is_empty() {
            self.nested = true;
        }
        // shlex does not implement ANSI-C/localized quoting or shell CR
        // semantics. Keep the conservative scan for those forms rather than
        // let an unrecognized heredoc delimiter hide following commands.
        if input.contains('\r') || input.contains("$'") || input.contains("$\"") {
            for segment in super::command_segments(input) {
                self.emit(&[segment]);
            }
        }
        let chars: Vec<char> = input.chars().collect();
        let n = chars.len();
        let mut i = 0usize;
        let mut commands: Vec<Line> = Vec::new();
        let mut line = Line::default();
        let mut word = WordBuf::default();
        let mut nested: Vec<String> = Vec::new();
        let mut heredocs = Vec::new();
        // Set when the text does not parse cleanly (an unterminated quote,
        // substitution or heredoc, a `case` inside a substitution): where
        // this parser ends a region may not be where the shell ends it.
        let mut uncertain = false;

        while i < n {
            let c = chars[i];
            match c {
                '#' if !word.started && word.text.is_empty() => {
                    while i < n && chars[i] != '\n' {
                        i += 1;
                    }
                }
                // A backslash outside quotes escapes exactly one character,
                // including an operator: `echo a\;b` is one word, not two
                // commands. A backslash-newline is a line continuation.
                '\\' if self.literal_backslashes => {
                    word.text.push('\\');
                    word.started = true;
                    i += 1;
                }
                '\\' => {
                    if i + 1 < n {
                        if chars[i + 1] != '\n' {
                            word.text.push(chars[i + 1]);
                            word.started = true;
                            word.quoted = true;
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                // Single quotes are fully literal: no substitution, no escapes.
                '\'' => {
                    word.started = true;
                    word.quoted = true;
                    i += 1;
                    while i < n && chars[i] != '\'' {
                        word.text.push(chars[i]);
                        i += 1;
                    }
                    uncertain |= i >= n;
                    i = (i + 1).min(n);
                }
                // Double quotes suppress word splitting but NOT substitution.
                '"' => {
                    word.started = true;
                    word.quoted = true;
                    i += 1;
                    while i < n && chars[i] != '"' {
                        match chars[i] {
                            '\\' if self.literal_backslashes => {
                                word.text.push('\\');
                                i += 1;
                            }
                            '\\' if i + 1 < n => {
                                word.text.push(chars[i + 1]);
                                i += 2;
                            }
                            '`' => {
                                word.dynamic = true;
                                let (inner, next, certain) = read_backtick(&chars, i);
                                uncertain |= !certain;
                                nested.push(inner);
                                i = next;
                            }
                            '$' if i + 1 < n && matches!(chars[i + 1], '(' | '{') => {
                                word.dynamic = true;
                                let (inner, next, certain) = read_substitution(&chars, i + 1);
                                uncertain |= !certain;
                                nested.push(inner);
                                i = next;
                            }
                            ch => {
                                if ch == '$' && starts_parameter(chars.get(i + 1)) {
                                    word.dynamic = true;
                                }
                                word.text.push(ch);
                                i += 1;
                            }
                        }
                    }
                    uncertain |= i >= n;
                    i = (i + 1).min(n);
                }
                '$' if i + 1 < n && chars[i + 1] == '\'' => {
                    word.started = true;
                    word.quoted = true;
                    word.dynamic = true;
                    i += 2;
                    while i < n && chars[i] != '\'' {
                        if chars[i] == '\\' && i + 1 < n {
                            i = push_ansi_c_escape(&chars, i, &mut word.text);
                        } else {
                            word.text.push(chars[i]);
                            i += 1;
                        }
                    }
                    uncertain |= i >= n;
                    i = (i + 1).min(n);
                }
                // Command substitution, both spellings. The body is a command
                // line in its own right; the substitution contributes no text
                // to the enclosing word (we do not evaluate output).
                '`' => {
                    word.started |= word.redirect_operand;
                    word.dynamic = true;
                    let (inner, next, certain) = read_backtick(&chars, i);
                    uncertain |= !certain;
                    nested.push(inner);
                    i = next;
                }
                // `$(…)`, and `${…}`: an expansion, not a command — but it can
                // *contain* one (`${x:-$(rm -rf /)}`), so the body is rescanned.
                // Process substitution `<(…)` / `>(…)` also runs its body.
                '$' | '<' | '>'
                    if i + 1 < n && (chars[i + 1] == '(' || (c == '$' && chars[i + 1] == '{')) =>
                {
                    word.started |= word.redirect_operand;
                    word.dynamic = true;
                    let (inner, next, certain) = read_substitution(&chars, i + 1);
                    uncertain |= !certain;
                    nested.push(inner);
                    i = next;
                }
                '<' if chars.get(i + 1) == Some(&'<') && chars.get(i + 2) != Some(&'<') => {
                    self.redirects = true;
                    if !word.quoted && is_redirect_descriptor(&word.text) {
                        word.text.clear();
                        word.started = false;
                    }
                    word.flush(&mut line);
                    line.heredoc = true;
                    i += 2;
                    let strip_tabs = chars.get(i) == Some(&'-');
                    if strip_tabs {
                        i += 1;
                    }
                    while i < n && matches!(chars[i], ' ' | '\t') {
                        i += 1;
                    }
                    let start = i;
                    let mut quote = None;
                    let mut literal = false;
                    while i < n {
                        let ch = chars[i];
                        if quote.is_none()
                            && matches!(ch, ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>')
                        {
                            break;
                        }
                        if ch == '\\' && quote != Some('\'') {
                            literal = true;
                            i = (i + 2).min(n);
                            continue;
                        }
                        if matches!(ch, '\'' | '"') {
                            literal = true;
                            if quote == Some(ch) {
                                quote = None;
                            } else if quote.is_none() {
                                quote = Some(ch);
                            }
                        }
                        i += 1;
                    }
                    let raw: String = chars[start..i].iter().collect();
                    if let Some(delimiter) = shlex::split(&raw)
                        .and_then(|mut words| (words.len() == 1).then(|| words.remove(0)))
                    {
                        heredocs.push((delimiter, literal, strip_tabs));
                    }
                }
                // Unquoted redirections are syntax, even without whitespace.
                // Keep the command words on both sides together, but omit the
                // descriptor and next operand. Parse that operand normally so
                // nested substitutions are still checked as commands.
                '<' | '>' | '&' if redirection_len(&chars[i..]) > 0 => {
                    self.redirects = true;
                    if !word.quoted && is_redirect_descriptor(&word.text) {
                        word.text.clear();
                        word.started = false;
                    }
                    word.flush(&mut line);
                    word.redirect_operand = true;
                    i += redirection_len(&chars[i..]);
                }
                ' ' | '\t' => {
                    word.flush(&mut line);
                    i += 1;
                }
                // A subshell boundary. `$(`, `<(` and `>(` were consumed by the
                // arms above, so a bare paren here is grouping: the body is a
                // command list of its own, not part of the surrounding word.
                '(' | ')' => {
                    self.control = true;
                    let glued = if c == '(' {
                        (word.started || !word.text.is_empty())
                            && chars.get(i + 1) != Some(&')')
                            && !is_array_assignment_prefix(&word.text)
                    } else {
                        !(i > 0 && chars[i - 1] == '(')
                            && chars.get(i + 1).is_some_and(|next| {
                                !next.is_whitespace()
                                    && !matches!(next, ';' | '|' | '&' | '(' | ')' | '<' | '>')
                            })
                    };
                    uncertain |= glued;
                    word.flush(&mut line);
                    end_command(&mut commands, &mut line);
                    word.redirect_operand = false;
                    if c == '(' && chars.get(i + 1) == Some(&'(') {
                        // `(( … ))` is arithmetic, where `<<` is a shift and
                        // not a heredoc. Its substitutions still run, and
                        // `((cmd) )` is two subshells, so the body is scanned
                        // as code on its own; a heredoc it appears to open
                        // cannot swallow the lines after it.
                        let (inner, next, certain) = read_substitution(&chars, i);
                        uncertain |= !certain;
                        nested.push(inner);
                        i = next;
                    } else {
                        i += 1;
                    }
                }
                // Control operators end the current command line. `&&`, `||`,
                // `;;`, `|&` and runs of newlines collapse into one break.
                '\n' | '\r' | ';' | '&' | '|' => {
                    self.control = true;
                    word.flush(&mut line);
                    end_command(&mut commands, &mut line);
                    word.redirect_operand = false;
                    i += 1;
                    if c == '\n' && !heredocs.is_empty() {
                        let shell_stdin = commands.iter().any(|line| {
                            command_heads(&line.words).heads.iter().any(|&head| {
                                let name = command_name(&line.words[head]);
                                SHELL_NAMES.contains(&name.as_str())
                                    || matches!(name.as_str(), "source" | ".")
                            })
                        });
                        for (delimiter, literal, strip_tabs) in heredocs.drain(..) {
                            let mut body = String::new();
                            // A body that runs to the end of the input may be
                            // a misread `<<`; what it swallowed is unexamined.
                            let mut terminated = false;
                            while i < n {
                                let start = i;
                                while i < n && chars[i] != '\n' {
                                    i += 1;
                                }
                                let mut text: String = chars[start..i].iter().collect();
                                if i < n {
                                    i += 1;
                                }
                                // An unquoted heredoc joins escaped newlines
                                // before checking its delimiter (E\ + OF can
                                // terminate EOF). Do not swallow later code.
                                while !literal
                                    && text.chars().rev().take_while(|c| *c == '\\').count() % 2
                                        == 1
                                    && i < n
                                {
                                    text.pop();
                                    let start = i;
                                    while i < n && chars[i] != '\n' {
                                        i += 1;
                                    }
                                    text.extend(chars[start..i].iter());
                                    if i < n {
                                        i += 1;
                                    }
                                }
                                let text = if strip_tabs {
                                    text.trim_start_matches('\t')
                                } else {
                                    &text
                                };
                                if text == delimiter {
                                    terminated = true;
                                    break;
                                }
                                body.push_str(text);
                                body.push('\n');
                            }
                            uncertain |= !terminated;
                            if shell_stdin {
                                nested.push(body);
                            } else if !literal {
                                let (bodies, certain) = heredoc_substitutions(&body);
                                uncertain |= !certain;
                                nested.extend(bodies);
                            }
                        }
                    }
                }
                _ => {
                    if c == '$' && starts_parameter(chars.get(i + 1)) {
                        word.dynamic = true;
                    }
                    match c {
                        '*' | '?' => word.dynamic = true,
                        '[' => word.bracket = true,
                        '{' => word.brace = true,
                        _ => {}
                    }
                    if !is_plain_word_char(c, !word.started && word.text.is_empty()) {
                        word.odd = true;
                    }
                    word.text.push(c);
                    word.started = true;
                    i += 1;
                }
            }
        }
        word.flush(&mut line);
        end_command(&mut commands, &mut line);
        self.dynamic |= uncertain;

        for line in &commands {
            self.record(line, depth);
        }
        for inner in nested {
            self.expand(&inner, depth + 1);
        }
    }

    /// Record one word-split command line, plus the invocation hiding inside it
    /// when the head is a wrapper, and flag what cannot be resolved.
    fn record(&mut self, line: &Line, depth: usize) {
        let tokens = &line.words;
        if tokens.is_empty() {
            return;
        }
        if depth > MAX_DEPTH {
            self.dynamic = true;
            return;
        }
        self.emit(tokens);

        let heads = command_heads(tokens);
        self.dynamic |= heads.truncated;
        for payload in &heads.split_payloads {
            // `env -S 'cmd args'` splits its value into a command line.
            self.expand(payload, depth + 1);
        }
        let mut dynamic = line.dynamic.clone();
        mark_replacement_operands(tokens, &heads.heads, &mut dynamic);
        for &head in &heads.heads {
            // `sudo rm -rf /` is an `rm -rf /`: emit the command the wrappers
            // run, with their options and operands removed.
            if head > 0 {
                self.emit(&tokens[head..]);
            }
            // Also keep a wrapper's options in front of the command it runs,
            // so a deny rule can still skip them as flags if the option table
            // misreads one. (`command -v NAME` runs nothing: no head follows.)
            if wrapper_index(&tokens[head]).is_some()
                && heads.heads.iter().any(|&inner| inner > head)
            {
                self.emit(&tokens[head + 1..]);
            }
            // A command word (or a wrapper word before it) that the shell
            // rewrites at run time cannot be matched against any rule.
            if tokens[..=head]
                .iter()
                .zip(&dynamic)
                .any(|(token, dynamic)| *dynamic && !is_env_assignment(token))
            {
                self.dynamic = true;
            }
            if line.odd[head] && !matches!(tokens[head].as_str(), "[" | "[[") {
                self.dynamic = true;
            }
            let name = command_name(&tokens[head]);
            let args_dynamic = dynamic[head + 1..].iter().any(|dynamic| *dynamic);
            self.arguments_dynamic |= args_dynamic;
            match name.as_str() {
                // `eval …` takes a *command line* as data. Parse it.
                "eval" | "iex" | "invoke-expression" => {
                    self.dynamic |= name != "eval" && head + 1 == tokens.len();
                    self.code_payload(&tokens[head + 1..].join(" "), args_dynamic, depth);
                }
                _ if REBINDING_BUILTINS.contains(&name.as_str()) => self.dynamic = true,
                "hash" => {
                    self.dynamic |= tokens[head + 1..].iter().any(|token| token != "-r");
                }
                "source" | "." => match tokens.get(head + 1) {
                    None if !line.heredoc => self.dynamic = true,
                    Some(_) if dynamic[head + 1] => self.dynamic = true,
                    Some(script) => self.dynamic |= script_read_at_run_time(script, line),
                    _ => {}
                },
                "find" => self.record_find_exec(line, head, depth),
                // `trap 'code' SIGNAL…` runs its first operand later in this
                // shell.
                "trap" => {
                    if let Some(at) =
                        (head + 1..tokens.len()).find(|&at| !tokens[at].starts_with('-'))
                    {
                        self.code_payload(&tokens[at], dynamic[at], depth);
                    }
                }
                // These hand the value of `-c` / `--command` to a shell.
                "su" | "runuser" | "flock" | "script" => {
                    for (payload, payload_dynamic) in
                        command_option_payloads(tokens, head, &dynamic)
                    {
                        self.code_payload(&payload, payload_dynamic, depth);
                    }
                }
                // `watch` and `sg GROUP [-c]` join their operands and hand
                // them to `sh -c`.
                "watch" | "sg" => {
                    for &inner in heads.heads.iter().filter(|&&inner| inner > head) {
                        let inner_dynamic = dynamic[inner..].iter().any(|dynamic| *dynamic);
                        self.code_payload(&tokens[inner..].join(" "), inner_dynamic, depth);
                    }
                }
                // `cmd /c …` parses the rest as a cmd.exe command line, which
                // expands `%VAR%` and `!VAR!` and strips `^` escapes itself.
                "cmd" => {
                    if let Some(at) = (head + 1..tokens.len()).find(|&at| {
                        matches!(tokens[at].to_ascii_lowercase().as_str(), "/c" | "/k" | "/r")
                    }) {
                        let payload = tokens[at + 1..].join(" ");
                        let payload_dynamic = dynamic[at + 1..].iter().any(|dynamic| *dynamic)
                            || payload.contains(['%', '!', '^']);
                        self.code_payload(&payload, payload_dynamic, depth);
                    }
                }
                "powershell" | "pwsh" => self.record_powershell(tokens, head, &dynamic, depth),
                _ if SHELL_NAMES.contains(&name.as_str()) => {
                    match shell_input(&tokens[head..]) {
                        ShellInput::Command(offsets) => {
                            for offset in offsets {
                                self.code_payload(
                                    &tokens[head + offset],
                                    dynamic[head + offset],
                                    depth,
                                );
                            }
                        }
                        ShellInput::Script(offset) => {
                            self.dynamic |= dynamic[head + offset]
                                || script_read_at_run_time(&tokens[head + offset], line);
                        }
                        // A heredoc body was already expanded as the script;
                        // any other stdin is only known at run time.
                        ShellInput::Stdin => self.dynamic |= !line.heredoc,
                    }
                }
                _ => {}
            }
        }
    }

    /// Code handed to another command as a string: parse it as a command line.
    fn code_payload(&mut self, payload: &str, dynamic: bool, depth: usize) {
        self.dynamic |= dynamic;
        self.expand(payload, depth + 1);
    }

    /// `powershell` / `pwsh`: `-Command` (or any prefix of it) takes the rest
    /// of the line as code, `-EncodedCommand` cannot be read statically, and
    /// a bare operand may start code too (Windows PowerShell's default).
    /// PowerShell syntax is only approximated by the POSIX parser, which is
    /// enough to find the command words a deny rule names.
    fn record_powershell(
        &mut self,
        tokens: &[String],
        head: usize,
        dynamic: &[bool],
        depth: usize,
    ) {
        let rest = |at: usize| {
            (
                tokens[at..].join(" "),
                dynamic[at..].iter().any(|dynamic| *dynamic),
            )
        };
        let mut positional_seen = false;
        for at in head + 1..tokens.len() {
            let Some(option) = tokens[at].strip_prefix(['-', '/']) else {
                if !positional_seen {
                    positional_seen = true;
                    let (payload, payload_dynamic) = rest(at);
                    self.code_payload(&payload, payload_dynamic, depth);
                }
                continue;
            };
            let option = option.to_ascii_lowercase();
            if option.is_empty() {
                continue;
            }
            if "command".starts_with(option.as_str()) {
                if at + 1 >= tokens.len() || tokens[at + 1] == "-" {
                    // The code arrives on stdin.
                    self.dynamic = true;
                } else {
                    let (payload, payload_dynamic) = rest(at + 1);
                    self.code_payload(&payload, payload_dynamic, depth);
                }
                return;
            }
            if option == "ec" || "encodedcommand".starts_with(option.as_str()) {
                self.dynamic = true;
                return;
            }
            if "file".starts_with(option.as_str()) {
                // A script file is opaque, as it is for every other shell.
                return;
            }
        }
    }

    /// `find … -exec CMD … ;` runs `CMD` for every match.
    fn record_find_exec(&mut self, line: &Line, head: usize, depth: usize) {
        let tokens = &line.words;
        let mut index = head + 1;
        while index < tokens.len() {
            if matches!(
                tokens[index].as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir"
            ) {
                let start = index + 1;
                let mut end = start;
                while end < tokens.len() && !matches!(tokens[end].as_str(), ";" | "+") {
                    end += 1;
                }
                if start < end {
                    self.nested = true;
                    // `{}` is replaced by each file name found, so a word
                    // holding it is only known at run time.
                    let payload = Line {
                        words: tokens[start..end].to_vec(),
                        dynamic: (start..end)
                            .map(|at| line.dynamic[at] || tokens[at].contains("{}"))
                            .collect(),
                        odd: line.odd[start..end].to_vec(),
                        heredoc: false,
                    };
                    self.record(&payload, depth + 1);
                }
                index = end;
            }
            index += 1;
        }
    }
}

/// Values of `-c CMD`, `-cCMD`, `--command CMD`, `--command=CMD` and
/// `--session-command` after `tokens[head]`, with whether each is dynamic:
/// the code `su`, `runuser`, `flock` and `script` hand to a shell.
fn command_option_payloads(
    tokens: &[String],
    head: usize,
    dynamic: &[bool],
) -> Vec<(String, bool)> {
    let mut payloads = Vec::new();
    for (at, token) in tokens.iter().enumerate().skip(head + 1) {
        let next = || {
            tokens
                .get(at + 1)
                .map(|value| (value.clone(), dynamic[at + 1]))
        };
        if let Some(long) = token.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            if matches!(name, "command" | "session-command") {
                payloads.extend(match inline {
                    Some(value) => Some((value.to_string(), dynamic[at])),
                    None => next(),
                });
            }
        } else if let Some(flags) = token.strip_prefix('-')
            && let Some(offset) = flags.find('c')
        {
            let value = &flags[offset + 1..];
            payloads.extend(if value.is_empty() {
                next()
            } else {
                Some((value.to_string(), dynamic[at]))
            });
        }
    }
    payloads
}

/// Mark the operands of an `xargs` that contain its replacement string
/// (`-I R`, `-J R`, `-i[R]`, `--replace[=R]`, `{}` by default) as known only
/// at run time: `xargs -I{} sh -c {}` runs whatever arrives on stdin.
fn mark_replacement_operands(tokens: &[String], heads: &[usize], dynamic: &mut [bool]) {
    let Some(xargs) = WRAPPERS.iter().find(|wrapper| wrapper.name == "xargs") else {
        return;
    };
    for &head in heads {
        if command_name(&tokens[head]) != "xargs" {
            continue;
        }
        let mut replacements: Vec<String> = Vec::new();
        let mut values = HashSet::new();
        let mut index = head + 1;
        while index < tokens.len() && tokens[index].starts_with('-') && tokens[index] != "--" {
            let token = tokens[index].as_str();
            if let Some(long) = token.strip_prefix("--") {
                match long.split_once('=') {
                    Some(("replace", value)) => replacements.push(value.to_string()),
                    None if long == "replace" => replacements.push("{}".to_string()),
                    None if xargs.long_values.contains(&long) => {
                        values.insert(index + 1);
                        index += 1;
                    }
                    _ => {}
                }
            } else {
                for (at, flag) in token[1..].char_indices() {
                    let rest = &token[1 + at + flag.len_utf8()..];
                    if flag == 'i' {
                        replacements.push(if rest.is_empty() { "{}" } else { rest }.to_string());
                        break;
                    }
                    if xargs.short_values.contains(flag) {
                        let value = if rest.is_empty() {
                            values.insert(index + 1);
                            index += 1;
                            tokens.get(index).cloned()
                        } else {
                            Some(rest.to_string())
                        };
                        if matches!(flag, 'I' | 'J') {
                            replacements.extend(value);
                        }
                        break;
                    }
                }
            }
            index += 1;
        }
        for (at, token) in tokens.iter().enumerate().skip(head + 1) {
            if !values.contains(&at)
                && !token.starts_with('-')
                && replacements.iter().any(|replacement| {
                    !replacement.is_empty() && token.contains(replacement.as_str())
                })
            {
                dynamic[at] = true;
            }
        }
    }
}

/// True unless the `$` is a literal one: it ends the word or precedes a
/// separator. Everything else (`$v`, `$1`, `$@`, `$=v`, `$~v`, `$^v`, `$"…"`,
/// and any flag character a shell may add) is read as an expansion.
fn starts_parameter(next: Option<&char>) -> bool {
    next.is_some_and(|c| !c.is_whitespace() && !matches!(c, ';' | '|' | '&' | ')' | '<' | '>'))
}

fn is_plain_word_char(c: char, leading: bool) -> bool {
    c.is_alphanumeric()
        || matches!(c, '_' | '.' | '/' | '+' | ':' | '@' | ',' | '-')
        || (leading && c == '~')
}

fn is_array_assignment_prefix(text: &str) -> bool {
    let name = text
        .strip_suffix("+=")
        .or_else(|| text.strip_suffix('='))
        .unwrap_or("");
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Unquoted heredocs expand substitutions, but quotes and ordinary lines are
/// data. Also reports whether every substitution was read with certainty.
fn heredoc_substitutions(body: &str) -> (Vec<String>, bool) {
    let chars: Vec<char> = body.chars().collect();
    let mut result = Vec::new();
    let mut all_certain = true;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if chars
                .get(i + 1)
                .is_some_and(|c| matches!(c, '$' | '`' | '\\' | '\n')) =>
            {
                i += 2
            }
            '`' => {
                let (inner, next, certain) = read_backtick(&chars, i);
                all_certain &= certain;
                result.push(inner);
                i = next;
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                let (inner, next, certain) = read_substitution(&chars, i + 1);
                all_certain &= certain;
                result.push(inner);
                i = next;
            }
            _ => i += 1,
        }
    }
    (result, all_certain)
}

fn redirection_len(chars: &[char]) -> usize {
    match chars {
        ['&', '>', '>', ..] | ['<', '<', '<' | '-', ..] => 3,
        ['&', '>', ..] | ['<', '<' | '>' | '&', ..] | ['>', '>' | '&' | '|', ..] => 2,
        ['<' | '>', ..] => 1,
        _ => 0,
    }
}

fn is_redirect_descriptor(word: &str) -> bool {
    if !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    // Bash also accepts an unquoted `{name}` in place of an IO number.
    word.strip_prefix('{')
        .and_then(|word| word.strip_suffix('}'))
        .is_some_and(|name| {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
                && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        })
}

/// Close the current simple command.
///
/// `{` and `}` stand alone as reserved words in `{ cmd; }` — they group a
/// command rather than being part of one, so they split the line
/// (`function f { rm x; }` runs `rm x`). Reserved words that lead a command
/// (`if`, `then`, `while`, `do`, `!`, …) are dropped so the word after them is
/// read as the command word. Every downstream consumer (wrapper detection,
/// emission) then looks at real command words only.
fn end_command(commands: &mut Vec<Line>, line: &mut Line) {
    let line = std::mem::take(line);
    let mut piece = Line {
        heredoc: line.heredoc,
        ..Line::default()
    };
    for ((word, dynamic), odd) in line.words.into_iter().zip(line.dynamic).zip(line.odd) {
        if matches!(word.as_str(), "{" | "}") {
            push_command(commands, &mut piece);
            continue;
        }
        if piece.words.is_empty() && RESERVED_PREFIXES.contains(&word.as_str()) {
            continue;
        }
        piece.words.push(word);
        piece.dynamic.push(dynamic);
        piece.odd.push(odd);
    }
    push_command(commands, &mut piece);
}

fn push_command(commands: &mut Vec<Line>, piece: &mut Line) {
    if !piece.words.is_empty() {
        let heredoc = piece.heredoc;
        commands.push(std::mem::take(piece));
        piece.heredoc = heredoc;
    }
}

/// Read a backtick substitution. `start` indexes the opening backtick; returns
/// the body, the index just past the closing backtick, and whether a closing
/// backtick was found.
fn read_backtick(chars: &[char], start: usize) -> (String, usize, bool) {
    let mut i = start + 1;
    let mut inner = String::new();
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                inner.push(chars[i]);
                inner.push(chars[i + 1]);
                i += 2;
            }
            '`' => return (inner, i + 1, true),
            c => {
                inner.push(c);
                i += 1;
            }
        }
    }
    (inner, i, false)
}

/// Read a `( … )` or `{ … }` region. `open_at` indexes the opening delimiter;
/// returns the body, the index just past the matching close, and whether the
/// region was read with certainty.
///
/// Quoted text, escapes, backticks and nested `$(`/`${` are skipped the way
/// the shell skips them, so `$(echo ")"; rm x)` closes at the last paren. A
/// `case` inside a `$( … )` is flagged uncertain rather than parsed: its
/// `pattern)` closes no paren, so the shell may end the region elsewhere. An
/// unterminated region is uncertain too.
fn read_substitution(chars: &[char], open_at: usize) -> (String, usize, bool) {
    let n = chars.len();
    let (open, close) = if chars[open_at] == '{' {
        ('{', '}')
    } else {
        ('(', ')')
    };
    let start = open_at + 1;
    let mut depth = 1usize;
    let mut certain = true;
    let mut i = start;
    while i < n {
        let c = chars[i];
        match c {
            '\\' => i += 2,
            '$' if chars.get(i + 1) == Some(&'\'') => {
                i += 2;
                while i < n && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < n {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                certain &= i < n;
                i += 1;
            }
            '\'' => {
                i += 1;
                while i < n && chars[i] != '\'' {
                    i += 1;
                }
                certain &= i < n;
                i += 1;
            }
            '"' => {
                i += 1;
                while i < n && chars[i] != '"' {
                    match chars[i] {
                        '\\' => i += 2,
                        '`' => {
                            let (_, next, inner_certain) = read_backtick(chars, i);
                            certain &= inner_certain;
                            i = next;
                        }
                        '$' if matches!(chars.get(i + 1), Some('(' | '{')) => {
                            let (_, next, inner_certain) = read_substitution(chars, i + 1);
                            certain &= inner_certain;
                            i = next;
                        }
                        _ => i += 1,
                    }
                }
                certain &= i < n;
                i += 1;
            }
            '`' => {
                let (_, next, inner_certain) = read_backtick(chars, i);
                certain &= inner_certain;
                i = next;
            }
            '$' if matches!(chars.get(i + 1), Some('(' | '{')) => {
                let (_, next, inner_certain) = read_substitution(chars, i + 1);
                certain &= inner_certain;
                i = next;
            }
            _ if c == close => {
                depth -= 1;
                if depth == 0 {
                    return (chars[start..i].iter().collect(), i + 1, certain);
                }
                i += 1;
            }
            _ => {
                if c == open {
                    depth += 1;
                } else if open == '('
                    && c == 'c'
                    && chars[i..].starts_with(&['c', 'a', 's', 'e'])
                    && chars.get(i + 4).is_none_or(|next| next.is_whitespace())
                    && (i == start
                        || matches!(
                            chars[i - 1],
                            ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | '!'
                        ))
                {
                    certain = false;
                }
                i += 1;
            }
        }
    }
    (chars[start..n.max(start)].iter().collect(), n, false)
}

fn push_ansi_c_escape(chars: &[char], backslash_idx: usize, out: &mut String) -> usize {
    let next = chars[backslash_idx + 1];
    match next {
        'a' => {
            out.push('\x07');
            backslash_idx + 2
        }
        'b' => {
            out.push('\x08');
            backslash_idx + 2
        }
        'e' | 'E' => {
            out.push('\x1b');
            backslash_idx + 2
        }
        'f' => {
            out.push('\x0c');
            backslash_idx + 2
        }
        'n' => {
            out.push('\n');
            backslash_idx + 2
        }
        'r' => {
            out.push('\r');
            backslash_idx + 2
        }
        't' => {
            out.push('\t');
            backslash_idx + 2
        }
        'v' => {
            out.push('\x0b');
            backslash_idx + 2
        }
        '\\' | '\'' | '"' | '?' => {
            out.push(next);
            backslash_idx + 2
        }
        '0'..='7' => {
            let mut val = 0u32;
            let mut end = backslash_idx + 1;
            while end < chars.len() && end < backslash_idx + 4 && ('0'..='7').contains(&chars[end])
            {
                val = (val << 3) | (chars[end] as u32 - '0' as u32);
                end += 1;
            }
            out.push(char::from((val & 0xff) as u8));
            end
        }
        'x' => {
            let mut val = 0u32;
            let mut end = backslash_idx + 2;
            while end < chars.len() && end < backslash_idx + 4 {
                if let Some(digit) = chars[end].to_digit(16) {
                    val = (val << 4) | digit;
                    end += 1;
                } else {
                    break;
                }
            }
            if end > backslash_idx + 2 {
                out.push(char::from((val & 0xff) as u8));
                end
            } else {
                out.push('\\');
                out.push('x');
                backslash_idx + 2
            }
        }
        'u' | 'U' => {
            let max_digits = if next == 'u' { 4 } else { 8 };
            let mut val = 0u32;
            let mut end = backslash_idx + 2;
            while end < chars.len() && end < backslash_idx + 2 + max_digits {
                if let Some(digit) = chars[end].to_digit(16) {
                    val = (val << 4) | digit;
                    end += 1;
                } else {
                    break;
                }
            }
            if end > backslash_idx + 2 {
                if let Some(ch) = char::from_u32(val) {
                    out.push(ch);
                } else {
                    out.extend(&chars[backslash_idx..end]);
                }
                end
            } else {
                out.push('\\');
                out.push(next);
                backslash_idx + 2
            }
        }
        'c' if backslash_idx + 2 < chars.len()
            && chars[backslash_idx + 2].is_ascii()
            && chars[backslash_idx + 2] != '\'' =>
        {
            out.push(char::from((chars[backslash_idx + 2] as u8) & 0x1f));
            backslash_idx + 3
        }
        _ => {
            out.push('\\');
            out.push(next);
            backslash_idx + 2
        }
    }
}

pub(crate) fn split_ansi_c_shell_words(segment: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = segment.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    while i < n {
        let ch = chars[i];
        if ch.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut current));
                started = false;
            }
            i += 1;
            continue;
        }
        if ch == '#' && !started {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        started = true;
        match ch {
            '$' if i + 1 < n && chars[i + 1] == '\'' => {
                i += 2;
                while i < n && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < n {
                        i = push_ansi_c_escape(&chars, i, &mut current);
                    } else {
                        current.push(chars[i]);
                        i += 1;
                    }
                }
                if i >= n {
                    return None;
                }
                i += 1;
            }
            '\'' => {
                i += 1;
                while i < n && chars[i] != '\'' {
                    current.push(chars[i]);
                    i += 1;
                }
                if i >= n {
                    return None;
                }
                i += 1;
            }
            '"' => {
                i += 1;
                while i < n && chars[i] != '"' {
                    if chars[i] == '\\' {
                        if i + 1 >= n {
                            return None;
                        }
                        let next = chars[i + 1];
                        if matches!(next, '"' | '\\' | '$' | '`') {
                            current.push(next);
                            i += 2;
                        } else if next == '\n' {
                            i += 2;
                        } else {
                            current.push('\\');
                            current.push(next);
                            i += 2;
                        }
                    } else {
                        current.push(chars[i]);
                        i += 1;
                    }
                }
                if i >= n {
                    return None;
                }
                i += 1;
            }
            '\\' => {
                if i + 1 >= n {
                    return None;
                }
                if chars[i + 1] != '\n' {
                    current.push(chars[i + 1]);
                }
                i += 2;
            }
            _ => {
                current.push(ch);
                i += 1;
            }
        }
    }
    if started {
        words.push(current);
    }
    Some(words)
}

/// The final path component, so `/usr/bin/sudo` reads as `sudo`.
fn basename(token: &str) -> &str {
    token
        .rsplit(['/', '\\'])
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(token)
}

fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            let name = name.strip_suffix('+').unwrap_or(name);
            !name.is_empty()
                && !name.starts_with('-')
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        }
        None => false,
    }
}

/// True for a bare scalar operand that may belong to a wrapper word rather
/// than start a command — `timeout 5`, `nice -n 10`, `timeout 1.5s`.
fn is_scalar_operand(token: &str) -> bool {
    let body = token.trim_end_matches(['s', 'm', 'h', 'd']);
    !body.is_empty() && body.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

/// The command a word names, for comparing against a table of names: the
/// lowercased basename with any `.exe` suffix removed, so `/usr/bin/sudo`,
/// `bash.exe` and `C:\Git\bin\bash.EXE` read as `sudo` and `bash`.
fn command_name(token: &str) -> String {
    let name = basename(token).to_ascii_lowercase();
    match name.strip_suffix(".exe") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => name,
    }
}

/// Index into [`WRAPPERS`] when `token` names a wrapper word.
fn wrapper_index(token: &str) -> Option<usize> {
    let name = command_name(token);
    WRAPPERS.iter().position(|wrapper| wrapper.name == name)
}

/// What [`command_heads`] found.
struct Heads {
    /// Every index that may be a command word, ascending.
    heads: Vec<usize>,
    /// `env -S` values, which are command lines.
    split_payloads: Vec<String>,
    /// The walk ran out of states before every reading was explored.
    truncated: bool,
}

/// Every index in `tokens` that may be a command word, walking leading
/// environment assignments and wrapper words together with the options and
/// operands those wrappers take. Wrapper words are themselves command words
/// and are included. Also returns `env -S` payloads, which are command lines.
///
/// Where the grammar is ambiguous (an option missing from the table that may
/// take a value, a bare number after a wrapper) both readings are kept,
/// because an extra candidate only makes deny matching stricter while a
/// missing one is a bypass.
fn command_heads(tokens: &[String]) -> Heads {
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum State {
        /// At a command word position.
        Command,
        /// Inside the arguments of `WRAPPERS[wrapper]`.
        Wrapper {
            wrapper: usize,
            operands: u8,
            options_done: bool,
        },
    }
    let mut heads = Vec::new();
    let mut payloads = Vec::new();
    let mut truncated = false;
    let mut seen = HashSet::new();
    let mut stack = vec![(0usize, State::Command)];
    while let Some((index, state)) = stack.pop() {
        if index >= tokens.len() || seen.contains(&(index, state)) {
            continue;
        }
        if seen.len() >= MAX_HEAD_STATES {
            truncated = true;
            break;
        }
        seen.insert((index, state));
        let token = tokens[index].as_str();
        match state {
            State::Command => {
                if is_env_assignment(token) {
                    stack.push((index + 1, State::Command));
                    continue;
                }
                if !heads.contains(&index) {
                    heads.push(index);
                }
                if let Some(position) = wrapper_index(token) {
                    stack.push((
                        index + 1,
                        State::Wrapper {
                            wrapper: position,
                            operands: WRAPPERS[position].operands,
                            options_done: false,
                        },
                    ));
                }
            }
            State::Wrapper {
                wrapper,
                operands,
                options_done,
            } => {
                let spec = &WRAPPERS[wrapper];
                let next = |operands, options_done| State::Wrapper {
                    wrapper,
                    operands,
                    options_done,
                };
                if !options_done && token == "--" {
                    stack.push((index + 1, next(operands, true)));
                } else if !options_done && token.len() > 1 && token.starts_with('-') {
                    let same = next(operands, false);
                    if let Some(long) = token.strip_prefix("--") {
                        let (name, inline) = match long.split_once('=') {
                            Some((name, value)) => (name, Some(value)),
                            None => (long, None),
                        };
                        if spec.name == "env" && name == "split-string" {
                            match inline {
                                Some(value) => payloads.push(value.to_string()),
                                None => payloads.extend(tokens.get(index + 1).cloned()),
                            }
                        }
                        if inline.is_some() || spec.long_switches.contains(&name) {
                            stack.push((index + 1, same));
                        } else if spec.long_values.contains(&name) {
                            stack.push((index + 2, same));
                        } else {
                            // Unknown long option: it may or may not take
                            // the next word as its value.
                            stack.push((index + 1, same));
                            stack.push((index + 2, same));
                        }
                    } else {
                        let flags = &token[1..];
                        // `command -v NAME` / `-V` only looks NAME up.
                        if spec.name == "command" && flags.contains(['v', 'V']) {
                            continue;
                        }
                        if spec.name == "env"
                            && let Some(at) = flags.find('S')
                        {
                            let value = &flags[at + 1..];
                            if value.is_empty() {
                                payloads.extend(tokens.get(index + 1).cloned());
                            } else {
                                payloads.push(value.to_string());
                            }
                        }
                        // Walk the cluster: a switch continues it, a value
                        // option takes the rest of the word or the next word,
                        // and an option missing from the table is read both
                        // ways (`env -P DIR cmd`, `xargs -J % cmd %`).
                        let mut step = Some(1);
                        for (at, flag) in flags.char_indices() {
                            if spec.short_values.contains(flag) {
                                if at + flag.len_utf8() == flags.len() {
                                    step = Some(2);
                                }
                                break;
                            }
                            if !flag.is_ascii_digit() && !spec.short_switches.contains(flag) {
                                step = None;
                                break;
                            }
                        }
                        match step {
                            Some(step) => stack.push((index + step, same)),
                            None => {
                                stack.push((index + 1, same));
                                stack.push((index + 2, same));
                            }
                        }
                    }
                } else if operands > 0 {
                    stack.push((index + 1, next(operands - 1, options_done)));
                } else {
                    if is_scalar_operand(token) {
                        stack.push((index + 1, next(0, options_done)));
                    }
                    stack.push((index, State::Command));
                }
            }
        }
    }
    heads.sort_unstable();
    Heads {
        heads,
        split_payloads: payloads,
        truncated,
    }
}

/// A script operand that names a file descriptor rather than a file:
/// `/dev/stdin`, `/dev/fd/N` or `/proc/<pid>/fd/N`. Its text arrives through
/// a pipe, a here-string or a redirect, so it is only known at run time —
/// unless it is stdin and a heredoc body (already expanded as the script)
/// feeds it.
fn script_read_at_run_time(script: &str, line: &Line) -> bool {
    let mut parts: Vec<&str> = Vec::new();
    for part in script.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    if !script.starts_with('/') {
        return false;
    }
    let descriptor = match parts.as_slice() {
        ["dev", "stdin"] => "0",
        ["dev", "fd", fd] => fd,
        ["proc", process, "fd", fd]
            if *process == "self"
                || *process == "thread-self"
                || process.chars().all(|ch| ch.is_ascii_digit()) =>
        {
            fd
        }
        _ => return false,
    };
    descriptor != "0" || !line.heredoc
}

/// How the shell invocation `tokens` (with `tokens[0]` the shell) gets its
/// script.
///
/// Combined short flags count (`bash -lc '…'`). The scan deliberately does
/// NOT stop at the first non-flag operand: an earlier version did, and
/// `bash -o vi -c 'payload'` walked straight past the deny expander because
/// `vi` (the argument of `-o`) ended the scan before `-c` was seen
/// (2026-08-04 review). Continuing the scan can over-read a `-c` that is
/// really an argument to a script (`bash script.sh -c x`), but this
/// expander's contract is explicit that over-emitting targets is safe and
/// under-emitting is a bypass.
///
/// `-c` only switches the shell into command mode: options may still follow
/// it, and the command string is the first *operand* after option parsing
/// (`bash -c -e 'cmd'`, `sh -c -- 'cmd'`, `bash -c -o pipefail 'cmd'` all run
/// `cmd`). Shells that read `-c`'s value as the very next word (fish) run
/// that word instead, so both candidates are reported when they differ.
fn shell_input(tokens: &[String]) -> ShellInput {
    let mut script = None;
    let mut from_stdin = false;
    let mut options_done = false;
    let mut command = Vec::new();
    let mut index = 1usize;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if !options_done && token == "--" {
            options_done = true;
        } else if !options_done && token == "-" {
            // `bash -` reads the script from stdin; after `-c` it only ends
            // the options.
            from_stdin |= command.is_empty();
            options_done = true;
        } else if let Some(long) = token.strip_prefix("--").filter(|_| !options_done) {
            if long.eq_ignore_ascii_case("command") {
                return match tokens.get(index + 1) {
                    Some(_) => ShellInput::Command(vec![index + 1]),
                    None => ShellInput::Stdin,
                };
            }
            if matches!(long, "rcfile" | "init-file") {
                index += 1;
            }
        } else if !options_done
            && token.len() > 1
            && (token.starts_with('-') || token.starts_with('+'))
        {
            let flags = &token[1..];
            if token.starts_with('-') && flags.contains('c') && command.is_empty() {
                command.push(index + 1);
            }
            from_stdin |= token.starts_with('-') && flags.contains('s');
            if flags.contains(['o', 'O']) {
                index += 1;
            }
        } else if !command.is_empty() {
            if command[0] != index {
                command.push(index);
            }
            return ShellInput::Command(command);
        } else if script.is_none() {
            script = Some(index);
        }
        index += 1;
    }
    command.retain(|&at| at < tokens.len());
    match script {
        _ if !command.is_empty() => ShellInput::Command(command),
        Some(index) if !from_stdin => ShellInput::Script(index),
        _ => ShellInput::Stdin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(command: &str) -> Vec<String> {
        // Exercise the POSIX grammar consistently on every test host.
        expanded_commands_for_platform(command, false)
    }

    #[test]
    fn native_windows_command_words_are_plain_literals() {
        for command in [
            r"C:\Windows\System32\where.exe git",
            r"C:\Git\bin\bash.exe -lc ls",
            r".\build.bat --release",
            r"\\server\share\tool.exe /q",
        ] {
            assert!(
                !expand_for_platform(command, true).dynamic,
                "{command} was unresolved"
            );
        }
        assert!(expand_for_platform(r"%COMSPEC% /c dir", true).dynamic);
    }

    #[test]
    fn windows_scan_retains_native_paths_and_posix_deny_candidates() {
        for (command, expected) in [
            (
                r"C:\Windows\System32\cat.exe ~/.ssh/id_rsa",
                r"C:\Windows\System32\cat.exe ~/.ssh/id_rsa",
            ),
            (r"del /f c:\users\x\file", r"del /f c:\users\x\file"),
            (
                r"echo safe & xcopy /e /y c:\src d:\dst",
                r"xcopy /e /y c:\src d:\dst",
            ),
            (
                r#""C:\Program Files\cat.exe" "c:\path with spaces\file""#,
                r"C:\Program Files\cat.exe c:\path with spaces\file",
            ),
            (r"del relative\file", r"del relative\file"),
            (
                r"\\server\share\cat.exe file",
                r"\\server\share\cat.exe file",
            ),
            (r"bash -c 'rm -rf \/'", "rm -rf /"),
        ] {
            let targets = expanded_commands_for_platform(command, true);
            assert!(
                targets.iter().any(|target| target == expected),
                "missing {expected:?} from {targets:?}"
            );
            assert!(targets.len() <= MAX_COMMANDS);
        }
        let targets =
            expanded_commands_for_platform("cat <<'EOF'\ndel c:\\users\\x\\file\nEOF", true);
        assert!(
            !targets.iter().any(|target| target.starts_with("del ")),
            "literal heredoc data must stay inert: {targets:?}"
        );
    }

    fn contains(command: &str, expected: &str) -> bool {
        expand(command).iter().any(|target| target == expected)
    }

    #[test]
    fn backtick_body_is_a_command() {
        assert!(contains("`rm -rf /`", "rm -rf /"));
        assert!(contains("echo `rm -rf /`", "rm -rf /"));
        assert!(contains("echo `rm -rf /`", "echo"));
    }

    #[test]
    fn dollar_paren_body_is_a_command() {
        assert!(contains("echo $(rm -rf /)", "rm -rf /"));
        assert!(contains("x=$(rm -rf /)", "rm -rf /"));
        assert!(contains("echo \"$(rm -rf /)\"", "rm -rf /"));
    }

    #[test]
    fn nested_substitution_is_followed() {
        assert!(contains("echo $(echo `rm -rf /`)", "rm -rf /"));
    }

    #[test]
    fn quotes_are_removed_from_operands() {
        assert!(contains("rm -rf \"/\"", "rm -rf /"));
        assert!(contains("rm -rf '/'", "rm -rf /"));
        assert!(contains("\"rm\" -rf /", "rm -rf /"));
        assert!(contains("rm -r\"f\" /", "rm -rf /"));
    }

    #[test]
    fn single_quoted_text_is_not_a_command() {
        // A literal backtick inside single quotes is printed, not executed.
        let targets = expand("echo '`rm -rf /`'");
        assert!(
            !targets.iter().any(|t| t == "rm -rf /"),
            "single-quoted text must not become a command: {targets:?}"
        );
    }

    #[test]
    fn escaped_operators_do_not_split() {
        let targets = expand("echo a\\;b");
        assert_eq!(targets, vec!["echo a;b".to_string()]);
        assert!(targets.contains(&"echo a;b".to_string()), "{targets:?}");
    }

    #[test]
    fn control_operators_split_commands() {
        for command in [
            "ls && rm -rf /",
            "ls || rm -rf /",
            "ls ; rm -rf /",
            "ls | rm -rf /",
            "ls & rm -rf /",
            "ls\nrm -rf /",
        ] {
            assert!(contains(command, "rm -rf /"), "{command}");
        }
    }

    #[test]
    fn wrappers_and_payloads_are_unwrapped() {
        for command in [
            "sudo rm -rf /",
            "env rm -rf /",
            "timeout 5 rm -rf /",
            "nohup rm -rf /",
            "xargs rm -rf /",
            "/usr/bin/sudo rm -rf /",
            "eval 'rm -rf /'",
            "bash -c 'rm -rf /'",
            "sh -lc \"rm -rf /\"",
            "sudo -u root bash -c 'rm -rf /'",
            // 2026-08-04: `-o vi` used to end the flag scan before `-c` was
            // seen, so the payload skipped deny expansion entirely.
            "bash -o vi -c 'rm -rf /'",
            "zsh --norcs -c 'rm -rf /'",
        ] {
            assert!(
                contains(command, "rm -rf /"),
                "{command}: {:?}",
                expand(command)
            );
        }
    }

    #[test]
    fn options_after_dash_c_do_not_hide_the_command_string() {
        // The command string is the first operand once options end, so an
        // option between `-c` and it does not change what runs.
        for command in [
            "bash -c -e 'rm -rf /'",
            "bash -c -l 'rm -rf /'",
            "sh -c -x 'rm -rf /'",
            "bash -c -- 'rm -rf /'",
            "sh -c - 'rm -rf /'",
            "bash -c +e 'rm -rf /'",
            "bash -lc -e 'rm -rf /'",
            "bash -c -o pipefail 'rm -rf /'",
            "bash -c -O extglob -e 'rm -rf /'",
            "bash script.sh -c 'rm -rf /'",
        ] {
            assert!(
                contains(command, "rm -rf /"),
                "{command}: {:?}",
                expand(command)
            );
        }
        // Arguments after the command string are positional parameters.
        assert!(!contains("bash -c 'echo $0' 'rm -rf /'", "rm -rf /"));
    }

    #[test]
    fn script_operand_naming_a_descriptor_is_read_at_run_time() {
        for command in [
            "echo 'rm -rf /' | bash /dev/stdin",
            "bash /dev/stdin <<< 'rm -rf /'",
            "sh /dev/fd/0 <<< 'rm -rf /'",
            "sh /proc/self/fd/0 <<< 'rm -rf /'",
            "bash //dev/./stdin <<< 'rm -rf /'",
            "bash /dev/fd/3 3< script",
            ". /dev/stdin <<< 'rm -rf /'",
            "source /proc/self/fd/0 <<< 'rm -rf /'",
        ] {
            assert!(expand_command(command).dynamic, "{command}");
        }
        // A heredoc on stdin was expanded as the script itself.
        let heredoc = expand_for_platform("bash /dev/stdin <<EOF\nrm -rf /\nEOF", false);
        assert!(!heredoc.dynamic);
        assert!(heredoc.commands.iter().any(|target| target == "rm -rf /"));
        // An ordinary script file is still opaque, not unresolved.
        assert!(!expand_command("bash ./dev/stdin").dynamic);
        assert!(!expand_command(". ./env.sh").dynamic);
    }

    #[test]
    fn wrapper_head_scan_stops_at_a_real_command() {
        // `echo` prints its arguments; nothing here is executed as a shell.
        let targets = expand("echo bash -c 'rm -rf /'");
        assert!(
            !targets.iter().any(|t| t == "rm -rf /"),
            "arguments of a printing command must not be parsed as code: {targets:?}"
        );
    }

    #[test]
    fn process_and_parameter_substitution_bodies_are_commands() {
        assert!(contains("diff <(rm -rf /) b", "rm -rf /"));
        assert!(contains("echo ${x:-$(rm -rf /)}", "rm -rf /"));
    }

    #[test]
    fn expansion_is_bounded() {
        let deep = "$(".repeat(64) + "rm -rf /" + &")".repeat(64);
        let targets = expand(&deep);
        assert!(targets.len() <= MAX_COMMANDS);
    }

    #[test]
    fn grouping_is_a_command_boundary() {
        assert!(contains("(rm -rf /)", "rm -rf /"));
        assert!(contains("{ rm -rf /; }", "rm -rf /"));
        assert!(contains("(cd /tmp && rm -rf /)", "rm -rf /"));
        // Escaped and quoted parens are operands, not grouping.
        assert!(contains(
            "find . \\( -name a \\) -print",
            "find . ( -name a ) -print"
        ));
    }

    #[test]
    fn syntax_metadata_is_independent_of_candidates_and_literal_data() {
        for command in [
            "printf x; printf x",
            "git log | git log",
            "cargo test && cargo test",
            "(git log)",
        ] {
            let expansion = expand_for_platform(command, false);
            assert!(expansion.control, "{command}: {expansion:?}");
        }
        assert_eq!(
            expand_for_platform("printf x; printf x", false)
                .commands
                .len(),
            1
        );
        assert!(expand_for_platform("sudo git log", false).commands.len() > 1);
        assert!(!expand_for_platform("sudo git log", false).control);
        for command in [
            r#"grep -E "a|b" src"#,
            r#"git commit -m "fix: a & b; c""#,
            r"echo a\;b\&c\|d\>e",
            "echo '> < ; | &'",
            "echo ok # ; | > ignored",
        ] {
            let expansion = expand_for_platform(command, false);
            assert!(
                !expansion.control && !expansion.redirects,
                "{command}: {expansion:?}"
            );
        }
        for command in [
            "cargo test 2>&1",
            "cargo test &>result.log",
            "cat a<>out",
            "cat a>|out",
            "cat 3<&0",
            "cat<<<literal",
        ] {
            for windows in [false, true] {
                let expansion = expand_for_platform(command, windows);
                assert!(
                    expansion.redirects && !expansion.control,
                    "{command}: {expansion:?}"
                );
            }
        }
        for command in ["git log $FLAGS", r#"git log "$FLAGS""#] {
            let expansion = expand_for_platform(command, false);
            assert!(
                expansion.arguments_dynamic && !expansion.dynamic,
                "{command}: {expansion:?}"
            );
        }
        assert!(!expand_for_platform("git log '$FLAGS'", false).arguments_dynamic);
        assert!(!expand_for_platform(r"git log \$FLAGS", false).arguments_dynamic);
        let heredoc = expand_for_platform("cat <<'EOF'\n; | > $(not-code)\nEOF", false);
        assert!(heredoc.redirects);
        assert!(!heredoc.nested);
        assert_eq!(heredoc.commands, vec!["cat"]);
    }

    #[test]
    fn plain_command_expands_to_itself() {
        assert_eq!(expand("git status -s"), vec!["git status -s".to_string()]);
    }

    #[test]
    fn ansi_c_escapes_decode_control_and_newline_characters() {
        for (command, expected_head) in [
            (r#"sh -c $'echo x\nwipefs -a /dev/sda'"#, "wipefs"),
            (r#"sh -c $'echo x\nmkfs.ext4 /dev/sda1'"#, "mkfs.ext4"),
            (r#"sh -c $'echo x\rwipefs -a /dev/sda'"#, "wipefs"),
            (r#"sh -c $'echo x\x0amkfs.ext4 /dev/sda1'"#, "mkfs.ext4"),
            (r#"sh -c $'echo x\012shred /dev/sda'"#, "shred"),
            (r#"sh -c $'echo x\u000add if=/dev/zero of=/dev/sda'"#, "dd"),
            (r#"sh -c $'echo x\cJwipefs -a /dev/sda'"#, "wipefs"),
            (r#"$(sh -c $'echo \'x\'\nwipefs -a /dev/sda')"#, "wipefs"),
        ] {
            let expansion = expand_for_platform(command, false);
            assert!(
                expansion
                    .words
                    .iter()
                    .any(|words| words.first().map(String::as_str) == Some(expected_head)),
                "{command}: {:?}",
                expansion.words
            );
        }
        assert!(
            expand_for_platform(r#"sh -c $'echo\ta\tb'"#, false)
                .words
                .iter()
                .any(|words| words == &["echo", "a", "b"])
        );
        assert!(expand_for_platform("$'rm' -f f", false).dynamic);
        assert!(expand_for_platform("$'\\x72m' f", false).dynamic);
    }
}
