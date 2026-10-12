//! Model-facing command syntax follows the same dispatcher as execution.

use crate::shell_dispatcher::{ShellKind, global_dispatcher};
use std::sync::OnceLock;

const POWERSHELL_GUIDANCE: &str = "Use PowerShell syntax. Bash is a legacy tool name, not a Bash interpreter. \
    For JSON use Invoke-RestMethod; for text use Invoke-WebRequest -UseBasicParsing \
    on Windows PowerShell to avoid dependency on the Internet Explorer engine. \
    Do not assume head, sed, awk, or other Unix utilities are installed. \
    Use PowerShell cmdlets or verified available programs; parse JSON and select needed fields \
    instead of appending head. Bash heredocs are not PowerShell syntax. Use PowerShell \
    5.1-compatible syntax (no && or ||) unless the detected executable is pwsh. Example: \
    $text = 'sample'; $text.Substring(0, [Math]::Min(3, $text.Length)).";

const BASH_GUIDANCE: &str = "Use Bash syntax: pipelines, redirections, $(command), \
    and && / || are supported. Quote paths and variable expansions, such as \"$path\"; \
    use single quotes for literal text. For literal multiline input, use a quoted heredoc \
    delimiter (<<'EOF') with its closing delimiter on a separate line. Use only installed \
    programs; do not assume GNU-specific flags on macOS/BSD. Example: printf '%s\\n' 'sample'.";

const SH_GUIDANCE: &str = "Use POSIX sh syntax: pipelines, redirections, $(command), \
    and && / || are supported. Quote paths and variable expansions, such as \"$path\"; \
    use single quotes for literal text. Do not use Bash-only arrays, [[ ... ]], \
    process substitution, or here-strings. Use only installed programs and portable \
    utility options. Example: printf '%s\\n' 'sample'.";

const ZSH_GUIDANCE: &str = "Use zsh syntax. Quote paths, literal wildcard patterns, \
    and variable expansions; unmatched unquoted globs can fail before a command runs. \
    A bare word starting with = undergoes =command PATH expansion (e.g. echo === fails); \
    quote such arguments, e.g. echo '==='. Do not assume Bash array indexing or word \
    splitting rules. Use only installed programs; do not assume GNU-specific flags on macOS/BSD.";

const CMD_GUIDANCE: &str = "Use cmd.exe syntax: %NAME% expands environment variables; use double quotes \
    around paths containing spaces (single quotes are not quoting delimiters). \
    Use cmd built-ins or installed programs, not Bash or PowerShell syntax. \
    Do not assume Unix utilities are installed. Example: echo sample";

const FISH_GUIDANCE: &str = "Use fish syntax: set NAME value for variables, \
    and begin ... end for blocks. Bash assignment NAME=value and \
    heredocs are not portable fish syntax. Quote paths and use only \
    installed programs. Example: printf '%s\\n' 'sample'.";

const FALLBACK_GUIDANCE: &str = "Use the detected shell's syntax and only installed programs; \
    do not infer Bash syntax from the legacy tool name.";

pub(super) fn command_guidance(kind: &ShellKind) -> String {
    let syntax = match kind {
        // Match execution's PowerShell-family detection, including custom paths.
        _ if kind.is_powershell() => POWERSHELL_GUIDANCE,
        ShellKind::Cmd => CMD_GUIDANCE,
        ShellKind::Sh => SH_GUIDANCE,
        ShellKind::Bash => BASH_GUIDANCE,
        ShellKind::Custom { binary, .. } => {
            match std::path::Path::new(binary)
                .file_stem()
                .and_then(|name| name.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("bash") => BASH_GUIDANCE,
                Some("sh" | "dash" | "ash") => SH_GUIDANCE,
                Some("zsh") => ZSH_GUIDANCE,
                Some("fish") => FISH_GUIDANCE,
                _ => FALLBACK_GUIDANCE,
            }
        }
        _ => FALLBACK_GUIDANCE,
    };
    format!(
        "The command to execute. Actual execution shell: `{}`. {syntax}",
        kind.binary()
    )
}

pub(super) fn runtime_command_guidance() -> &'static str {
    // A conformance replay of a recorded host names that host's shell, not
    // whichever shell first initialized this process-wide cache (#6698).
    #[cfg(all(test, unix))]
    if let Some(binary) = crate::prompts::recorded_shell() {
        let kind = ShellKind::Custom {
            binary,
            flag: "-c".to_string(),
        };
        return Box::leak(command_guidance(&kind).into_boxed_str());
    }
    static GUIDANCE: OnceLock<String> = OnceLock::new();
    GUIDANCE.get_or_init(|| command_guidance(global_dispatcher().kind()))
}

fn render_description(guidance: &str) -> String {
    format!(
        "{guidance} Execute in the workspace. Action \"run\" (default) executes a command; \
         \"wait\" blocks for a background task until completion or timeout; \"interact\" sends stdin to a background task; \
         \"cancel\" kills a background task. Pass wait=false for a nonblocking task snapshot. Foreground mode is for bounded commands; \
         use background=true for work expected to take >5 seconds."
    )
}

pub(super) fn description() -> &'static str {
    #[cfg(all(test, unix))]
    if crate::prompts::recorded_shell().is_some() {
        return Box::leak(render_description(runtime_command_guidance()).into_boxed_str());
    }
    static DESCRIPTION: OnceLock<String> = OnceLock::new();
    DESCRIPTION.get_or_init(|| render_description(runtime_command_guidance()))
}

// Interpreter syntax lives on the command parameter. Repeating it in the
// tool description adds the same bytes to every active request.
pub(super) fn foreground_description() -> &'static str {
    "Execute a shell command in the workspace and return stdout and stderr. Output keeps the last 2000 lines or 50KB. An optional timeout is the seconds to wait in the foreground (default 30 seconds); a command still running then is never killed for time: it moves to the background by itself and you get its output so far, how long ago it last printed, its build phase, and a task_id, so long builds and tests need no special timeout. Find task_shell_wait with tool_search to read more or wait for it. In Ask, after a sandbox denial, retry the exact command once with sandbox_permissions (the narrowest wider mode that suffices) and a one-sentence justification; the approval prompt asks the user."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_guidance_preserves_unix_shell_contracts() {
        for (binary, expected) in [
            ("/bin/bash", BASH_GUIDANCE),
            ("bash", BASH_GUIDANCE),
            ("/usr/local/bin/bash", BASH_GUIDANCE),
            ("/bin/sh", SH_GUIDANCE),
            ("/bin/dash", SH_GUIDANCE),
            ("/bin/ash", SH_GUIDANCE),
            ("/bin/zsh", ZSH_GUIDANCE),
        ] {
            let text = command_guidance(&ShellKind::Custom {
                binary: binary.into(),
                flag: "-lc".into(),
            });
            assert!(text.contains(expected), "missing guidance for {binary}");
            assert!(!text.contains("Use PowerShell syntax"));
        }
        assert!(command_guidance(&ShellKind::Bash).contains(BASH_GUIDANCE));
        assert!(command_guidance(&ShellKind::Sh).contains(SH_GUIDANCE));
    }

    #[test]
    fn shell_guidance_matches_each_interpreter() {
        for kind in [
            ShellKind::Pwsh,
            ShellKind::WindowsPowerShell,
            ShellKind::Cmd,
            ShellKind::Sh,
            ShellKind::Bash,
            ShellKind::Custom {
                binary: "/bin/zsh".into(),
                flag: "-lc".into(),
            },
            ShellKind::Custom {
                binary: "/opt/pwsh".into(),
                flag: "-c".into(),
            },
            ShellKind::Custom {
                binary: "/bin/fish".into(),
                flag: "-c".into(),
            },
        ] {
            let text = command_guidance(&kind);
            assert!(text.contains(kind.binary()));
            assert_eq!(text.contains("Use PowerShell syntax"), kind.is_powershell());
            assert_eq!(
                text.contains("=command PATH expansion"),
                kind.binary() == "/bin/zsh"
            );
            assert!(!text.contains("user's login shell"));
        }
    }
}
