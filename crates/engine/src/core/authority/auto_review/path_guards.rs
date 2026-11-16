use std::path::Path;

use serde_json::Value;

use crate::tools::spec::normalize_path;

use super::protected_instructions::{dequote_shell_word, extract_shell_output_redirects};
use super::{
    CriticalPathScope, ResolvedTarget, ShellCwd, StmtSep, ToolCategory, VirtualLinkTarget,
    advance_shell_cwd, extract_command_substitutions, extract_nested_shell_script,
    file_write_target_paths, get_tool_category_for_call, is_env_assignment, normalize_command_name,
    record_ln_virtual_link, resolve_shell_path_target, resolve_via_virtual_links,
    shell_command_param, shell_token_trim, shell_tool_initial_cwd, skip_shell_wrappers,
    split_simple_shell_words, split_top_level_statements,
};

const MAX_SHELL_DEPTH: usize = 4;

fn is_safe_pseudo_device_str(raw: &str) -> bool {
    let trimmed = shell_token_trim(raw).trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.replace('\\', "/").to_ascii_lowercase();
    let check = lower.strip_prefix("//./").unwrap_or(&lower);
    matches!(
        check,
        "/dev/null"
            | "/dev/zero"
            | "/dev/stdin"
            | "/dev/stdout"
            | "/dev/stderr"
            | "/dev/random"
            | "/dev/urandom"
            | "/dev/tty"
            | "nul"
            | "con"
            | "prn"
            | "aux"
    ) || check
        .strip_prefix("/dev/fd/")
        .is_some_and(|rest| !rest.is_empty())
}

fn is_safe_pseudo_device_path(path: &Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    is_safe_pseudo_device_str(&text)
}

fn is_url_like(raw: &str) -> bool {
    let lower = shell_token_trim(raw).to_ascii_lowercase();
    if lower.starts_with("file://") {
        return false;
    }
    [
        "http://", "https://", "ftp://", "ftps://", "ssh://", "git://", "mailto:", "ws://",
        "wss://", "data:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn expand_raw_token(raw: &str) -> String {
    let trimmed = shell_token_trim(raw).trim();
    if let Some(rest) = trimmed.strip_prefix("file://") {
        rest.to_string()
    } else {
        trimmed.to_string()
    }
}

fn has_windows_drive_prefix(raw: &str) -> Option<(char, &str)> {
    let bytes = raw.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        Some(((bytes[0] as char).to_ascii_lowercase(), &raw[2..]))
    } else {
        None
    }
}

fn raw_segments(raw: &str) -> Vec<String> {
    raw.split(['/', '\\'])
        .filter(|seg| !seg.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn token_is_protected(
    raw: &str,
    cwd: Option<&Path>,
    virtual_links: &[(String, VirtualLinkTarget)],
    scope: CriticalPathScope<'_>,
) -> bool {
    let expanded = expand_raw_token(raw);
    if expanded.is_empty() || is_safe_pseudo_device_str(&expanded) || is_url_like(&expanded) {
        return false;
    }
    if crate::core::authority::is_auto_review_protected_segments(&raw_segments(&expanded)) {
        return true;
    }
    let resolved = resolve_via_virtual_links(&expanded, virtual_links, scope)
        .or_else(|| resolve_shell_path_target(&expanded, cwd, scope.home, scope.windows_mode));
    match resolved {
        Some(ResolvedTarget::Concrete(path)) => {
            crate::core::authority::auto_review_path_is_protected(
                &path,
                scope.workspace,
                scope.home,
            )
        }
        Some(ResolvedTarget::HomeRoot) => scope.home.is_some_and(|h| {
            crate::core::authority::auto_review_path_is_protected(h, scope.workspace, scope.home)
        }),
        Some(ResolvedTarget::Unresolved) | None => {
            let candidate = crate::core::authority::expand_home_prefix(&expanded, scope.home);
            crate::core::authority::auto_review_path_is_protected(
                &candidate,
                scope.workspace,
                scope.home,
            )
        }
    }
}

fn token_points_outside_workspace(
    raw: &str,
    cwd: Option<&Path>,
    virtual_links: &[(String, VirtualLinkTarget)],
    scope: CriticalPathScope<'_>,
    canon_ws: &Path,
    lex_ws: &Path,
) -> bool {
    let expanded = expand_raw_token(raw);
    if expanded.is_empty()
        || expanded == "-"
        || is_safe_pseudo_device_str(&expanded)
        || is_url_like(&expanded)
    {
        return false;
    }
    if expanded.starts_with("\\\\") {
        return true;
    }
    if let Some((drive, _)) = has_windows_drive_prefix(&expanded) {
        let ws_drive = has_windows_drive_prefix(&lex_ws.to_string_lossy()).map(|(d, _)| d);
        if ws_drive != Some(drive) {
            return true;
        }
    }
    let resolved = resolve_via_virtual_links(&expanded, virtual_links, scope)
        .or_else(|| resolve_shell_path_target(&expanded, cwd, scope.home, scope.windows_mode));
    match resolved {
        Some(ResolvedTarget::HomeRoot) => {
            let Some(home) = scope.home else {
                return true;
            };
            let Some(home_canon) = crate::core::authority::resolve_deepest_existing(home) else {
                return true;
            };
            !home_canon.starts_with(canon_ws)
        }
        Some(ResolvedTarget::Unresolved) => {
            let upper = expanded.to_ascii_uppercase();
            expanded.starts_with('~')
                || expanded.starts_with("$HOME")
                || expanded.starts_with("${HOME}")
                || upper.starts_with("%USERPROFILE%")
                || upper.starts_with("%APPDATA%")
                || expanded.starts_with('/')
                || expanded.starts_with("..")
                || expanded.contains("/../")
        }
        Some(ResolvedTarget::Concrete(path)) => {
            if is_safe_pseudo_device_path(&path) {
                return false;
            }
            let lex = normalize_path(&path);
            if !lex.starts_with(lex_ws) {
                return true;
            }
            match crate::core::authority::resolve_deepest_existing(&path) {
                Some(real) => {
                    if is_safe_pseudo_device_path(&real) {
                        return false;
                    }
                    !real.starts_with(canon_ws)
                }
                None => true,
            }
        }
        None => false,
    }
}

fn consume_redirect_word(chars: &[char], i: &mut usize, windows_mode: bool) -> String {
    let start = *i;
    let mut sub_single = false;
    let mut sub_double = false;
    while *i < chars.len() {
        let c = chars[*i];
        if sub_single {
            if c == '\'' {
                sub_single = false;
            }
            *i += 1;
            continue;
        }
        if sub_double {
            if ((!windows_mode && c == '\\') || (windows_mode && c == '`')) && *i + 1 < chars.len()
            {
                *i += 2;
                continue;
            }
            if c == '"' {
                sub_double = false;
            }
            *i += 1;
            continue;
        }
        if ((!windows_mode && c == '\\') || (windows_mode && c == '`')) && *i + 1 < chars.len() {
            *i += 2;
            continue;
        }
        if c == '\'' {
            sub_single = true;
            *i += 1;
            continue;
        }
        if c == '"' {
            sub_double = true;
            *i += 1;
            continue;
        }
        if c.is_ascii_whitespace() || matches!(c, ';' | '&' | '|' | '>' | '<' | '(' | ')') {
            break;
        }
        *i += 1;
    }
    chars[start..*i].iter().collect()
}

pub(super) fn extract_shell_input_redirects(stmt: &str, windows_mode: bool) -> Vec<String> {
    let chars: Vec<char> = stmt.chars().collect();
    let mut targets = Vec::new();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    while i < chars.len() {
        let ch = chars[i];
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if ((!windows_mode && ch == '\\') || (windows_mode && ch == '`')) && i + 1 < chars.len()
            {
                i += 2;
                continue;
            }
            if ch == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if ((!windows_mode && ch == '\\') || (windows_mode && ch == '`')) && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if ch == '\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if ch == '"' {
            in_double = true;
            i += 1;
            continue;
        }
        if ch == '<' {
            i += 1;
            if i < chars.len() && chars[i] == '<' {
                let mut count = 1usize;
                while i < chars.len() && chars[i] == '<' {
                    count += 1;
                    i += 1;
                }
                if count == 2 && i < chars.len() && chars[i] == '-' {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_whitespace() {
                    i += 1;
                }
                let _ = consume_redirect_word(&chars, &mut i, windows_mode);
                continue;
            }
            if i < chars.len() && chars[i] == '>' {
                i += 1;
            }
            while i < chars.len() && chars[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < chars.len() && chars[i] == '&' {
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '-') {
                    i += 1;
                }
                continue;
            }
            let raw = consume_redirect_word(&chars, &mut i, windows_mode);
            if !raw.is_empty() {
                let word = dequote_shell_word(&raw, windows_mode);
                if !word.is_empty() {
                    targets.push(word);
                }
            }
            continue;
        }
        i += 1;
    }
    targets
}

fn walk_shell_statements<F>(
    command: &str,
    mut cwd: ShellCwd,
    mut virtual_links: Vec<(String, VirtualLinkTarget)>,
    scope: CriticalPathScope<'_>,
    depth: usize,
    visit: &mut F,
) -> bool
where
    F: FnMut(&str, &[String], &ShellCwd, &[(String, VirtualLinkTarget)]) -> bool,
{
    if depth > MAX_SHELL_DEPTH {
        return false;
    }
    let Some(stmts) = split_top_level_statements(command, scope.windows_mode) else {
        return visit(command, &[], &cwd, &virtual_links);
    };
    for stmt in stmts {
        for subst in extract_command_substitutions(stmt.raw, scope.windows_mode) {
            if walk_shell_statements(
                subst,
                cwd.clone(),
                virtual_links.clone(),
                scope,
                depth + 1,
                visit,
            ) {
                return true;
            }
        }
        if let Some(inner) = stmt.subshell_inner {
            if walk_shell_statements(
                inner,
                cwd.clone(),
                virtual_links.clone(),
                scope,
                depth + 1,
                visit,
            ) {
                return true;
            }
            continue;
        }
        let Some(words) = split_simple_shell_words(stmt.raw, scope.windows_mode) else {
            if visit(stmt.raw, &[], &cwd, &virtual_links) {
                return true;
            }
            continue;
        };
        if visit(stmt.raw, &words, &cwd, &virtual_links) {
            return true;
        }
        if let Some(nested) = extract_nested_shell_script(&words)
            && walk_shell_statements(
                &nested,
                cwd.clone(),
                virtual_links.clone(),
                scope,
                depth + 1,
                visit,
            )
        {
            return true;
        }
        if stmt.sep_before != StmtSep::Pipe
            && stmt.sep_after != StmtSep::Pipe
            && let Some(idx) = skip_shell_wrappers(&words)
        {
            let cmd = normalize_command_name(&words[idx]);
            let args = &words[idx + 1..];
            match cmd.as_str() {
                "cd" | "pushd" => {
                    cwd = advance_shell_cwd(&cwd, args, true, &virtual_links, scope);
                }
                "popd" => cwd = ShellCwd::Tainted,
                "ln" => record_ln_virtual_link(args, &cwd, &mut virtual_links, scope),
                _ => {}
            }
        }
    }
    false
}

fn cwd_path(cwd: &ShellCwd) -> Option<&Path> {
    match cwd {
        ShellCwd::Known(path) => Some(path.as_path()),
        ShellCwd::Unset | ShellCwd::Tainted => None,
    }
}

fn extract_quoted_script_strings(script: &str) -> Vec<String> {
    let chars: Vec<char> = script.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let quote = chars[i];
        if quote == '\'' || quote == '"' {
            let mut buf = String::new();
            i += 1;
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' && i + 1 < chars.len() {
                    buf.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if ch == quote {
                    i += 1;
                    break;
                }
                buf.push(ch);
                i += 1;
            }
            if !buf.is_empty() {
                out.push(buf);
            }
        } else {
            i += 1;
        }
    }
    out
}

fn extract_inline_script(cmd: &str, args: &[String]) -> Option<String> {
    let is_script_runner = matches!(
        cmd,
        "python"
            | "python2"
            | "python3"
            | "py"
            | "node"
            | "nodejs"
            | "deno"
            | "bun"
            | "ruby"
            | "perl"
            | "php"
            | "lua"
            | "osascript"
    );
    if !is_script_runner {
        return None;
    }
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if matches!(arg.as_str(), "-c" | "-e") {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("-c").or_else(|| arg.strip_prefix("-e"))
            && !rest.is_empty()
        {
            return Some(rest.to_string());
        }
    }
    None
}

fn script_looks_mutating(script: &str) -> bool {
    let low = script.to_ascii_lowercase();
    [
        "open(", "write", "unlink", "remove", "rename", "replace", "symlink", "chmod", "chown",
        "rmdir", "mkdir", "rmtree", "copy", "shutil", "fs.", "fs/", "append", "truncate",
    ]
    .iter()
    .any(|kw| low.contains(kw))
}

fn is_script_runner_with_read_intent(cmd: &str, script: &str) -> bool {
    let _ = cmd;
    let low = script.to_ascii_lowercase();
    [
        "open(",
        "read",
        "readlines",
        "path(",
        "fs.",
        "fs/",
        "listdir",
        "scandir",
        "walk",
        "glob",
        "stat",
        "exists",
        "cat ",
    ]
    .iter()
    .any(|kw| low.contains(kw))
}

fn find_inner_command_slice(cmd: &str, args: &[String]) -> Option<Vec<String>> {
    match cmd {
        "xargs" | "parallel" => {
            let mut i = 0usize;
            while i < args.len() {
                let a = args[i].as_str();
                if a == "--" {
                    i += 1;
                    break;
                }
                if a.starts_with('-') {
                    let takes_val = matches!(
                        a,
                        "-I" | "-L" | "-n" | "-P" | "-s" | "-d" | "-E" | "-a" | "-J"
                    );
                    i += if takes_val { 2 } else { 1 };
                } else {
                    break;
                }
            }
            if i < args.len() {
                Some(args[i..].to_vec())
            } else {
                None
            }
        }
        "find" => {
            let mut roots = Vec::new();
            let mut i = 0usize;
            while i < args.len() {
                let a = args[i].as_str();
                if a.starts_with('-') || a == "!" || a == "(" {
                    break;
                }
                roots.push(args[i].clone());
                i += 1;
            }
            while i < args.len() {
                if matches!(args[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                    let start = i + 1;
                    let mut end = start;
                    while end < args.len() && !matches!(args[end].as_str(), ";" | "+") {
                        end += 1;
                    }
                    if start < end {
                        let mut expanded = Vec::new();
                        for token in &args[start..end] {
                            if token == "{}" && !roots.is_empty() {
                                expanded.extend(roots.iter().cloned());
                            } else {
                                expanded.push(token.clone());
                            }
                        }
                        return Some(expanded);
                    }
                }
                i += 1;
            }
            None
        }
        _ => None,
    }
}

fn check_mutating_words_protected(
    words: &[String],
    cwd: &ShellCwd,
    virtual_links: &[(String, VirtualLinkTarget)],
    scope: CriticalPathScope<'_>,
) -> bool {
    let Some(idx) = skip_shell_wrappers(words) else {
        return false;
    };
    let cmd = normalize_command_name(&words[idx]);
    let args = &words[idx + 1..];
    let cwd_opt = cwd_path(cwd);

    if let Some(inner) = find_inner_command_slice(&cmd, args)
        && check_mutating_words_protected(&inner, cwd, virtual_links, scope)
    {
        return true;
    }

    if let Some(script) = extract_inline_script(&cmd, args)
        && script_looks_mutating(&script)
    {
        for candidate in extract_quoted_script_strings(&script) {
            if token_is_protected(&candidate, cwd_opt, virtual_links, scope) {
                return true;
            }
        }
    }

    let check_tok = |tok: &str| token_is_protected(tok, cwd_opt, virtual_links, scope);

    if cmd == "dd" {
        return args
            .iter()
            .any(|arg| arg.strip_prefix("of=").is_some_and(check_tok));
    }

    if cmd == "git" {
        if let Some((subcmd, sub_args)) = super::git_subcommand_and_args(words) {
            match subcmd {
                "config" => {
                    if sub_args.iter().any(|a| a == "--global") {
                        return true;
                    }
                    let mut i = 0usize;
                    while i < sub_args.len() {
                        let a = sub_args[i].as_str();
                        if matches!(a, "-f" | "--file") {
                            if sub_args.get(i + 1).is_some_and(|p| check_tok(p)) {
                                return true;
                            }
                            i += 2;
                            continue;
                        }
                        if let Some(p) = a.strip_prefix("--file=")
                            && check_tok(p)
                        {
                            return true;
                        }
                        i += 1;
                    }
                    return false;
                }
                "mv" | "rm" | "checkout" | "restore" | "clean" | "reset" | "apply" | "am"
                | "clone" => {
                    return sub_args
                        .iter()
                        .filter(|a| *a != "--" && !a.starts_with('-'))
                        .any(|a| check_tok(a));
                }
                _ => return false,
            }
        }
        return false;
    }

    if cmd == "find" {
        let mut roots = Vec::new();
        let mut i = 0usize;
        while i < args.len() {
            let a = args[i].as_str();
            if a.starts_with('-') || a == "!" || a == "(" {
                break;
            }
            roots.push(args[i].as_str());
            i += 1;
        }
        let has_delete = args[i..].iter().any(|a| a == "-delete");
        if has_delete && roots.iter().any(|r| check_tok(r)) {
            return true;
        }
        let mut j = i;
        while j < args.len() {
            if matches!(args[j].as_str(), "-fprint" | "-fprintf" | "-fls")
                && args.get(j + 1).is_some_and(|p| check_tok(p))
            {
                return true;
            }
            j += 1;
        }
        return false;
    }

    let is_in_place_sed_perl_ruby = matches!(cmd.as_str(), "sed" | "perl" | "ruby")
        && args.iter().any(|a| {
            a == "--in-place"
                || a.starts_with("--in-place=")
                || (a.starts_with('-') && a.contains('i') && !a.starts_with("--"))
        });
    let is_in_place_awk = matches!(cmd.as_str(), "awk" | "gawk" | "mawk" | "nawk")
        && (args
            .iter()
            .any(|a| a == "inplace" || a == "--include=inplace")
            || args.windows(2).any(|w| w[0] == "-i" && w[1] == "inplace"));
    let is_extract_archive = match cmd.as_str() {
        "tar" | "gtar" | "bsdtar" => args.iter().any(|a| {
            a == "--extract"
                || a == "--get"
                || (!a.starts_with("--")
                    && (a.starts_with('-') || args.first().is_some_and(|f| f == a))
                    && a.contains('x'))
        }),
        "unzip" | "7z" | "7za" | "7zz" | "cpio" | "pax" | "unrar" | "unar" | "expand-archive" => {
            true
        }
        _ => false,
    };

    if is_extract_archive {
        if cwd_opt.is_some_and(|c| {
            crate::core::authority::auto_review_path_is_protected(c, scope.workspace, scope.home)
        }) {
            return true;
        }
        let mut i = 0usize;
        while i < args.len() {
            let a = args[i].as_str();
            if matches!(a, "-C" | "--directory" | "-d" | "-destinationpath") {
                if args.get(i + 1).is_some_and(|p| check_tok(p)) {
                    return true;
                }
                i += 2;
                continue;
            }
            if let Some(p) = a
                .strip_prefix("--directory=")
                .or_else(|| a.strip_prefix("-C"))
                .or_else(|| a.strip_prefix("-d"))
                .or_else(|| a.strip_prefix("-o"))
                .filter(|p| !p.is_empty())
                && check_tok(p)
            {
                return true;
            }
            if a != "--" && !a.starts_with('-') && check_tok(a) {
                return true;
            }
            i += 1;
        }
        return false;
    }

    let is_mutator = matches!(
        cmd.as_str(),
        "tee"
            | "rm"
            | "rmdir"
            | "unlink"
            | "touch"
            | "mkdir"
            | "mkfifo"
            | "mknod"
            | "chmod"
            | "chown"
            | "chgrp"
            | "ln"
            | "cp"
            | "mv"
            | "install"
            | "truncate"
            | "shred"
            | "setfacl"
            | "xattr"
            | "chflags"
            | "chattr"
            | "mktemp"
            | "sponge"
            | "patch"
            | "ed"
            | "ex"
            | "copy"
            | "xcopy"
            | "robocopy"
            | "move"
            | "del"
            | "erase"
            | "rd"
            | "ren"
            | "rename"
            | "mklink"
            | "icacls"
            | "attrib"
            | "takeown"
            | "new-item"
            | "ni"
            | "set-content"
            | "sc"
            | "add-content"
            | "ac"
            | "out-file"
            | "copy-item"
            | "cpi"
            | "move-item"
            | "mi"
            | "remove-item"
            | "ri"
            | "clear-content"
            | "clc"
            | "rename-item"
            | "rni"
    ) || is_in_place_sed_perl_ruby
        || is_in_place_awk;

    if !is_mutator {
        return false;
    }

    let mut after_double_dash = false;
    let mut skipped_inline_program = false;
    let saw_script_flag = args.iter().any(|a| {
        matches!(a.as_str(), "-e" | "-f" | "--expression" | "--file")
            || a.starts_with("--expression=")
            || a.starts_with("--file=")
    });
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        if !after_double_dash && a == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if !after_double_dash && a.starts_with('-') && a.len() > 1 {
            if matches!(
                a,
                "-t" | "--target-directory"
                    | "-d"
                    | "--directory"
                    | "-o"
                    | "--output"
                    | "-r"
                    | "-f"
                    | "--file"
                    | "-filepath"
                    | "-path"
                    | "-literalpath"
            ) {
                if args.get(i + 1).is_some_and(|p| check_tok(p)) {
                    return true;
                }
                i += 2;
                continue;
            }
            if matches!(
                a,
                "-e" | "--expression" | "-s" | "-m" | "-n" | "-g" | "-p" | "-i"
            ) {
                i += 1;
                continue;
            }
            if let Some(p) = a
                .strip_prefix("--target-directory=")
                .or_else(|| a.strip_prefix("--directory="))
                .or_else(|| a.strip_prefix("--output="))
                .or_else(|| a.strip_prefix("--file="))
                .or_else(|| a.strip_prefix("-t"))
                .filter(|p| !p.is_empty())
                && check_tok(p)
            {
                return true;
            }
            i += 1;
            continue;
        }
        if (is_in_place_sed_perl_ruby || is_in_place_awk)
            && !saw_script_flag
            && !skipped_inline_program
        {
            skipped_inline_program = true;
            i += 1;
            continue;
        }
        if cmd == "patch" {
            if check_tok(a) {
                return true;
            }
        } else if matches!(cmd.as_str(), "chmod" | "chown" | "chgrp") {
            if (a.contains('/') || a.contains('\\') || check_tok(a)) && check_tok(a) {
                return true;
            }
        } else if check_tok(a) {
            return true;
        }
        i += 1;
    }
    false
}

fn collect_apply_patch_fallback_paths(patch_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in patch_text.lines() {
        let trimmed = line.trim();
        for prefix in [
            "*** Update File: ",
            "*** Add File: ",
            "*** Delete File: ",
            "*** Move to: ",
            "+++ b/",
            "--- a/",
            "+++ ",
            "--- ",
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let p = rest.trim();
                if !p.is_empty() && p != "/dev/null" {
                    out.push(p.to_string());
                }
                break;
            }
        }
    }
    out
}

fn file_call_protected_targets(tool_name: &str, params: &Value) -> Option<Vec<String>> {
    let canonical =
        codewhale_execpolicy::canonical_action::canonical_action_alias(tool_name, params);
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let is_mutating_file = matches!(
        canonical,
        "write_file" | "edit_file" | "apply_patch" | "delete_file" | "move_file" | "rename_file"
    ) || matches!(
        get_tool_category_for_call(tool_name, params),
        ToolCategory::FileWrite
    ) || (tool_name.eq_ignore_ascii_case("File")
        && action.as_deref().is_some_and(|a| {
            matches!(
                a,
                "write" | "edit" | "patch" | "delete" | "remove" | "rename" | "move"
            )
        }));
    if !is_mutating_file {
        return None;
    }

    let mut targets = file_write_target_paths(tool_name, params).unwrap_or_default();
    let aliased = codewhale_tools::param_alias::with_canonical_path_argument(params);
    for key in [
        "path",
        "file_path",
        "filePath",
        "target_path",
        "destination",
        "dest",
        "new_path",
        "to",
        "from",
    ] {
        if let Some(s) = aliased.get(key).and_then(Value::as_str).map(str::trim)
            && !s.is_empty()
            && !targets.iter().any(|t| t == s)
        {
            targets.push(s.to_string());
        }
    }
    for array_key in ["changes", "replace", "edits"] {
        if let Some(items) = aliased.get(array_key).and_then(Value::as_array) {
            for item in items {
                let aliased_item = codewhale_tools::param_alias::with_canonical_path_argument(item);
                if let Some(s) = aliased_item
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    && !s.is_empty()
                    && !targets.iter().any(|t| t == s)
                {
                    targets.push(s.to_string());
                }
            }
        }
    }
    if canonical == "apply_patch"
        && let Some(patch_text) = aliased.get("patch").and_then(Value::as_str)
    {
        for p in collect_apply_patch_fallback_paths(patch_text) {
            if !targets.contains(&p) {
                targets.push(p);
            }
        }
    }
    Some(targets)
}

pub(super) fn protected_path_write_with_home(
    tool_name: &str,
    params: &Value,
    workspace: Option<&Path>,
    home: Option<&Path>,
    windows_mode: bool,
) -> bool {
    if let Some(targets) = file_call_protected_targets(tool_name, params)
        && targets.iter().any(|raw| {
            let expanded = crate::core::authority::expand_home_prefix(raw, home);
            crate::core::authority::auto_review_path_is_protected(&expanded, workspace, home)
                || crate::core::authority::is_auto_review_protected_segments(&raw_segments(raw))
        })
    {
        return true;
    }

    let category = get_tool_category_for_call(tool_name, params);
    let Some(command) = shell_command_param(params) else {
        return false;
    };
    if !matches!(category, ToolCategory::Shell) && command.is_empty() {
        return false;
    }
    let scope = CriticalPathScope {
        workspace,
        home,
        windows_mode,
        dotglob: false,
    };
    let initial_cwd = shell_tool_initial_cwd(tool_name, params, workspace, home, windows_mode);
    walk_shell_statements(
        command,
        initial_cwd,
        Vec::new(),
        scope,
        0,
        &mut |stmt_raw, words, cwd, virtual_links| {
            let cwd_opt = cwd_path(cwd);
            if extract_shell_output_redirects(stmt_raw, windows_mode)
                .iter()
                .any(|target| token_is_protected(target, cwd_opt, virtual_links, scope))
            {
                return true;
            }
            if !words.is_empty() && check_mutating_words_protected(words, cwd, virtual_links, scope)
            {
                return true;
            }
            false
        },
    )
}

fn unquoted_redirect_op_end(chars: &[char], i: usize, word_start: bool) -> Option<usize> {
    let n = chars.len();
    let op = if chars[i] == '<' || chars[i] == '>' {
        i
    } else if word_start && chars[i] == '&' && i + 1 < n && chars[i + 1] == '>' {
        i + 1
    } else if word_start && chars[i].is_ascii_digit() {
        let mut d = i;
        while d < n && chars[d].is_ascii_digit() {
            d += 1;
        }
        if d < n && (chars[d] == '<' || chars[d] == '>') {
            d
        } else {
            return None;
        }
    } else {
        return None;
    };
    let end = if chars[op] == '<' {
        if op + 2 < n && chars[op + 1] == '<' && (chars[op + 2] == '<' || chars[op + 2] == '-') {
            op + 3
        } else if op + 1 < n && matches!(chars[op + 1], '<' | '>' | '&') {
            op + 2
        } else {
            op + 1
        }
    } else if op + 1 < n && matches!(chars[op + 1], '>' | '|' | '&') {
        op + 2
    } else {
        op + 1
    };
    if end == op + 1 && end < n && chars[end] == '(' {
        return None;
    }
    Some(end)
}

fn strip_unquoted_shell_redirects(stmt: &str, windows_mode: bool) -> String {
    let chars: Vec<char> = stmt.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(stmt.len());
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    while i < n {
        let ch = chars[i];
        if in_single {
            out.push(ch);
            if ch == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if !windows_mode && ch == '\\' && i + 1 < n {
                out.push(ch);
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            out.push(ch);
            if ch == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if in_backtick {
            if !windows_mode && ch == '\\' && i + 1 < n {
                out.push(ch);
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            out.push(ch);
            if ch == '`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }
        if !windows_mode && ch == '$' && i + 1 < n && chars[i + 1] == '\'' {
            out.push(ch);
            out.push('\'');
            i += 2;
            while i < n && chars[i] != '\'' {
                if chars[i] == '\\' && i + 1 < n {
                    out.push(chars[i]);
                    out.push(chars[i + 1]);
                    i += 2;
                } else {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            if i < n {
                out.push('\'');
                i += 1;
            }
            continue;
        }
        if !windows_mode && ch == '\\' && i + 1 < n {
            out.push(ch);
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if ch == '\'' {
            in_single = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '"' {
            in_double = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '`' {
            in_backtick = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '#' && (i == 0 || chars[i - 1].is_whitespace()) {
            while i < n && chars[i] != '\n' {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        let word_start = i == 0
            || chars[i - 1].is_whitespace()
            || matches!(chars[i - 1], ';' | '&' | '|' | '(' | ')' | '{' | '}');
        if let Some(mut j) = unquoted_redirect_op_end(&chars, i, word_start) {
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            let _ = consume_redirect_word(&chars, &mut j, windows_mode);
            if !out.ends_with(char::is_whitespace) {
                out.push(' ');
            }
            i = j;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn words_without_unquoted_redirects<'a>(
    stmt_raw: &str,
    words: &'a [String],
    windows_mode: bool,
) -> std::borrow::Cow<'a, [String]> {
    if words.is_empty() || !stmt_raw.contains(['<', '>']) {
        return std::borrow::Cow::Borrowed(words);
    }
    let stripped = strip_unquoted_shell_redirects(stmt_raw, windows_mode);
    match split_simple_shell_words(&stripped, windows_mode) {
        Some(clean) => std::borrow::Cow::Owned(clean),
        None => std::borrow::Cow::Borrowed(words),
    }
}

fn check_read_words_outside_workspace(
    words: &[String],
    cwd: &ShellCwd,
    virtual_links: &[(String, VirtualLinkTarget)],
    scope: CriticalPathScope<'_>,
    canon_ws: &Path,
    lex_ws: &Path,
) -> bool {
    let Some(idx) = skip_shell_wrappers(words) else {
        return false;
    };
    let cmd = normalize_command_name(&words[idx]);
    let args = &words[idx + 1..];
    let cwd_opt = cwd_path(cwd);

    if let Some(inner) = find_inner_command_slice(&cmd, args)
        && check_read_words_outside_workspace(&inner, cwd, virtual_links, scope, canon_ws, lex_ws)
    {
        return true;
    }

    let check_tok = |tok: &str| {
        token_points_outside_workspace(tok, cwd_opt, virtual_links, scope, canon_ws, lex_ws)
    };

    if matches!(
        cmd.as_str(),
        "echo"
            | "printf"
            | "true"
            | "false"
            | "pwd"
            | "sleep"
            | "exit"
            | "return"
            | "export"
            | "unset"
            | "local"
            | "readonly"
            | "alias"
            | "cd"
            | "pushd"
            | "popd"
            | "mkdir"
            | "rmdir"
            | "rm"
            | "unlink"
            | "touch"
            | "chmod"
            | "chown"
            | "chgrp"
            | "mkfifo"
            | "mknod"
            | "truncate"
            | "shred"
    ) {
        return false;
    }

    let cwd_is_outside = match cwd {
        ShellCwd::Known(c) => {
            let lex = normalize_path(c);
            let real = crate::core::authority::resolve_deepest_existing(c);
            !lex.starts_with(lex_ws) || !real.is_some_and(|r| r.starts_with(canon_ws))
        }
        ShellCwd::Unset => true,
        ShellCwd::Tainted => false,
    };

    if matches!(
        cmd.as_str(),
        "ls" | "dir"
            | "ll"
            | "la"
            | "tree"
            | "find"
            | "fd"
            | "du"
            | "stat"
            | "gci"
            | "get-childitem"
    ) && cwd_is_outside
        && args.iter().all(|a| a.starts_with('-'))
    {
        return true;
    }

    if let Some(script) = extract_inline_script(&cmd, args)
        && is_script_runner_with_read_intent(&cmd, &script)
    {
        for candidate in extract_quoted_script_strings(&script) {
            if check_tok(&candidate) {
                return true;
            }
        }
    }

    if cmd == "dd" {
        return args
            .iter()
            .any(|arg| arg.strip_prefix("if=").is_some_and(check_tok));
    }

    if cmd == "git" {
        let mut i = 0usize;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "--" {
                break;
            }
            if matches!(a, "-C" | "--git-dir" | "--work-tree") {
                if args.get(i + 1).is_some_and(|p| check_tok(p)) {
                    return true;
                }
                i += 2;
                continue;
            }
            if let Some(p) = a
                .strip_prefix("--git-dir=")
                .or_else(|| a.strip_prefix("--work-tree="))
                .or_else(|| a.strip_prefix("-C"))
                .filter(|p| !p.is_empty())
                && check_tok(p)
            {
                return true;
            }
            if !a.starts_with('-') {
                break;
            }
            i += 1;
        }
        if let Some((_, sub_args)) = super::git_subcommand_and_args(words) {
            return sub_args
                .iter()
                .filter(|a| *a != "--" && !a.starts_with('-'))
                .any(|a| check_tok(a));
        }
        return false;
    }

    if cmd == "find" {
        for a in args {
            let s = a.as_str();
            if s.starts_with('-') || s == "!" || s == "(" {
                break;
            }
            if check_tok(s) {
                return true;
            }
        }
        return false;
    }

    if matches!(cmd.as_str(), "curl" | "wget") {
        let mut i = 0usize;
        while i < args.len() {
            let a = args[i].as_str();
            if matches!(
                a,
                "-d" | "--data"
                    | "--data-raw"
                    | "--data-binary"
                    | "-F"
                    | "--form"
                    | "-T"
                    | "--upload-file"
                    | "-K"
                    | "--config"
            ) {
                if let Some(val) = args.get(i + 1) {
                    let rhs = val.strip_prefix('@').or_else(|| {
                        if matches!(a, "-F" | "--form") {
                            val.split_once("=@").map(|(_, p)| p)
                        } else {
                            None
                        }
                    });
                    if let Some(path) = rhs {
                        if check_tok(path) {
                            return true;
                        }
                    } else if matches!(a, "-T" | "--upload-file" | "-K" | "--config")
                        && check_tok(val)
                    {
                        return true;
                    }
                }
                i += 2;
                continue;
            }
            if let Some(val) = a
                .strip_prefix("--data=")
                .or_else(|| a.strip_prefix("--data-binary="))
                .or_else(|| a.strip_prefix("--upload-file="))
                .or_else(|| a.strip_prefix("--config="))
            {
                let path = val.strip_prefix('@').unwrap_or(val);
                if check_tok(path) {
                    return true;
                }
            }
            i += 1;
        }
        return false;
    }

    let is_pattern_tool = matches!(
        cmd.as_str(),
        "grep" | "rg" | "ag" | "ack" | "fgrep" | "egrep" | "sed" | "awk" | "gawk" | "mawk" | "nawk"
    );
    let saw_pattern_flag = args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-e" | "-f" | "--regexp" | "--file" | "--expression"
        ) || a.starts_with("--regexp=")
            || a.starts_with("--file=")
            || a.starts_with("--expression=")
    });
    let mut skipped_pattern = false;
    let mut after_double_dash = false;
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        if !after_double_dash && a == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if !after_double_dash && a.starts_with('-') && a.len() > 1 {
            if matches!(
                a,
                "-f" | "--file"
                    | "--files-from"
                    | "--from-file"
                    | "-C"
                    | "--directory"
                    | "--config"
                    | "--input"
                    | "-i"
                    | "-path"
                    | "-literalpath"
            ) {
                if args.get(i + 1).is_some_and(|p| check_tok(p)) {
                    return true;
                }
                i += 2;
                continue;
            }
            if let Some((flag, rhs)) = a.split_once('=') {
                if !matches!(
                    flag,
                    "-o" | "--output" | "--out" | "--exclude" | "--exclude-dir" | "--ignore"
                ) && check_tok(rhs)
                {
                    return true;
                }
            } else if let Some(rhs) = a.strip_prefix("-f").or_else(|| a.strip_prefix("-C"))
                && !rhs.is_empty()
                && check_tok(rhs)
            {
                return true;
            }
            if matches!(a, "-e" | "--regexp" | "--expression") {
                i += 2;
                continue;
            }
            if matches!(cmd.as_str(), "head" | "tail")
                && matches!(a, "-n" | "-c" | "--lines" | "--bytes")
            {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if matches!(cmd.as_str(), "awk" | "gawk" | "mawk" | "nawk") && is_env_assignment(a) {
            i += 1;
            continue;
        }
        if is_pattern_tool && !saw_pattern_flag && !skipped_pattern {
            skipped_pattern = true;
            i += 1;
            continue;
        }
        if check_tok(a) {
            return true;
        }
        i += 1;
    }
    false
}

pub(super) fn command_reads_credential_file_with_home(
    tool_name: &str,
    params: &Value,
    workspace: Option<&Path>,
    home: Option<&Path>,
    windows_mode: bool,
) -> bool {
    let category = get_tool_category_for_call(tool_name, params);
    let Some(command) = shell_command_param(params) else {
        return false;
    };
    if !matches!(category, ToolCategory::Shell) && command.is_empty() {
        return false;
    }
    let scope = CriticalPathScope {
        workspace,
        home,
        windows_mode,
        dotglob: false,
    };
    let initial_cwd = shell_tool_initial_cwd(tool_name, params, workspace, home, windows_mode);
    walk_shell_statements(
        command,
        initial_cwd,
        Vec::new(),
        scope,
        0,
        &mut |stmt_raw, words, cwd, virtual_links| {
            let cwd_opt = cwd_path(cwd);
            let check_target = |raw: &str| {
                let expanded = expand_raw_token(raw);
                if super::names_credential_file(&expanded) {
                    return true;
                }
                let resolved = resolve_via_virtual_links(&expanded, virtual_links, scope)
                    .or_else(|| resolve_shell_path_target(&expanded, cwd_opt, home, windows_mode));
                match resolved {
                    Some(ResolvedTarget::Concrete(p)) => {
                        super::names_credential_file(&p.to_string_lossy())
                    }
                    _ => false,
                }
            };
            if extract_shell_input_redirects(stmt_raw, windows_mode)
                .iter()
                .any(|target| check_target(target))
            {
                return true;
            }
            let clean_words = words_without_unquoted_redirects(stmt_raw, words, windows_mode);
            let Some(idx) = skip_shell_wrappers(&clean_words) else {
                return false;
            };
            let cmd = normalize_command_name(&clean_words[idx]);
            if !matches!(
                cmd.as_str(),
                "cat"
                    | "head"
                    | "tail"
                    | "less"
                    | "more"
                    | "bat"
                    | "nl"
                    | "tac"
                    | "od"
                    | "hexdump"
                    | "xxd"
                    | "strings"
                    | "base64"
                    | "sed"
                    | "awk"
                    | "gawk"
                    | "grep"
                    | "rg"
                    | "ag"
                    | "jq"
                    | "yq"
                    | "cp"
                    | "scp"
                    | "rsync"
                    | "pbcopy"
                    | "xclip"
                    | "xsel"
                    | "wl-copy"
                    | "clip"
                    | "type"
                    | "get-content"
                    | "gc"
                    | "source"
                    | "."
            ) {
                return false;
            }
            clean_words[idx + 1..].iter().any(|arg| {
                if arg.starts_with('-') && !arg.contains('=') {
                    return false;
                }
                let raw = arg.split_once('=').map_or(arg.as_str(), |(_, rhs)| rhs);
                check_target(raw)
            })
        },
    )
}

pub(super) fn outside_workspace_shell_read_with_home(
    tool_name: &str,
    params: &Value,
    workspace: Option<&Path>,
    home: Option<&Path>,
    windows_mode: bool,
) -> bool {
    let Some(ws) = workspace else {
        return false;
    };
    let category = get_tool_category_for_call(tool_name, params);
    let Some(command) = shell_command_param(params) else {
        return false;
    };
    if !matches!(category, ToolCategory::Shell) && command.is_empty() {
        return false;
    }
    let lex_ws = normalize_path(ws);
    let canon_ws =
        crate::core::authority::resolve_deepest_existing(ws).unwrap_or_else(|| lex_ws.clone());
    let scope = CriticalPathScope {
        workspace: Some(ws),
        home,
        windows_mode,
        dotglob: false,
    };
    let initial_cwd = shell_tool_initial_cwd(tool_name, params, Some(ws), home, windows_mode);
    walk_shell_statements(
        command,
        initial_cwd,
        Vec::new(),
        scope,
        0,
        &mut |stmt_raw, words, cwd, virtual_links| {
            let cwd_opt = cwd_path(cwd);
            if extract_shell_input_redirects(stmt_raw, windows_mode)
                .iter()
                .any(|target| {
                    token_points_outside_workspace(
                        target,
                        cwd_opt,
                        virtual_links,
                        scope,
                        &canon_ws,
                        &lex_ws,
                    )
                })
            {
                return true;
            }
            let clean_words = words_without_unquoted_redirects(stmt_raw, words, windows_mode);
            !clean_words.is_empty()
                && check_read_words_outside_workspace(
                    &clean_words,
                    cwd,
                    virtual_links,
                    scope,
                    &canon_ws,
                    &lex_ws,
                )
        },
    )
}
