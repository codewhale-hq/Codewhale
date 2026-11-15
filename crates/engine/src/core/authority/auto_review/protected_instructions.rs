use super::*;

fn is_protected_instruction_basename(name: &str) -> bool {
    name.eq_ignore_ascii_case("AGENTS.md") || name.eq_ignore_ascii_case("CLAUDE.md")
}

fn matches_loaded_instruction_path(
    candidate: &std::path::Path,
    loaded_paths: &[std::path::PathBuf],
    windows_mode: bool,
) -> bool {
    if loaded_paths.is_empty() {
        return false;
    }
    let case_insensitive = windows_mode || cfg!(target_os = "macos");
    let cand_variants = path_compare_variants(candidate, windows_mode);
    loaded_paths.iter().any(|loaded| {
        let loaded_variants = path_compare_variants(loaded, windows_mode);
        cand_variants.iter().any(|c| {
            loaded_variants
                .iter()
                .any(|l| same_compare_path(c, l, case_insensitive))
        })
    })
}

fn path_targets_protected_instruction(
    candidate: &std::path::Path,
    virtual_links: &[(String, VirtualLinkTarget)],
    scope: CriticalPathScope<'_>,
    loaded_paths: &[std::path::PathBuf],
) -> bool {
    let check_concrete = |path: &std::path::Path| -> bool {
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(is_protected_instruction_basename)
        {
            return true;
        }
        if matches_loaded_instruction_path(path, loaded_paths, scope.windows_mode) {
            return true;
        }
        if let Some(phys) = resolve_physical_preserving_tail(path) {
            if phys
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(is_protected_instruction_basename)
            {
                return true;
            }
            if matches_loaded_instruction_path(&phys, loaded_paths, scope.windows_mode) {
                return true;
            }
        }
        if let Ok(link_target) = std::fs::read_link(path) {
            let joined = if link_target.is_absolute() {
                link_target
            } else if let Some(parent) = path.parent() {
                parent.join(link_target)
            } else {
                link_target
            };
            if joined
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(is_protected_instruction_basename)
                || matches_loaded_instruction_path(&joined, loaded_paths, scope.windows_mode)
            {
                return true;
            }
        }
        false
    };

    if check_concrete(candidate) {
        return true;
    }
    if !virtual_links.is_empty()
        && let Some(ResolvedTarget::Concrete(remapped)) =
            resolve_via_virtual_links(&candidate.to_string_lossy(), virtual_links, scope)
        && check_concrete(&remapped)
    {
        return true;
    }
    false
}

fn raw_operand_targets_protected_instruction(
    raw: &str,
    cwd: &ShellCwd,
    virtual_links: &[(String, VirtualLinkTarget)],
    scope: CriticalPathScope<'_>,
    loaded_paths: &[std::path::PathBuf],
) -> bool {
    let trimmed = shell_token_trim(raw);
    if trimmed.is_empty() {
        return false;
    }
    if let Some(name) = trimmed
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        && is_protected_instruction_basename(name)
    {
        return true;
    }
    let cwd_path = match cwd {
        ShellCwd::Known(p) => Some(p.as_path()),
        ShellCwd::Unset | ShellCwd::Tainted => None,
    };
    if let Some(ResolvedTarget::Concrete(resolved)) =
        resolve_shell_path_target(trimmed, cwd_path, scope.home, scope.windows_mode)
    {
        return path_targets_protected_instruction(&resolved, virtual_links, scope, loaded_paths);
    }
    path_targets_protected_instruction(
        std::path::Path::new(trimmed),
        virtual_links,
        scope,
        loaded_paths,
    )
}

fn text_mentions_protected_instruction(text: &str, loaded_paths: &[std::path::PathBuf]) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("agents.md") || lower.contains("claude.md") {
        return true;
    }
    loaded_paths.iter().any(|path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .filter(|name| !name.is_empty())
            .is_some_and(|name| lower.contains(&name.to_ascii_lowercase()))
    })
}

fn text_mentions_instruction_mutator_keyword(text: &str, windows_mode: bool) -> bool {
    text_mentions_delete_keyword(text, windows_mode)
        || [
            "mv", "cp", "ln", "tee", "touch", "truncate", "install", "chmod", "chown", "patch",
            "sed", "perl", "ruby", "python", "node", "git ",
        ]
        .iter()
        .any(|kw| text.to_ascii_lowercase().contains(kw))
}

pub(super) fn dequote_shell_word(raw: &str, windows_mode: bool) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    while i < chars.len() {
        let ch = chars[i];
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                out.push(ch);
            }
            i += 1;
            continue;
        }
        if in_double {
            if ch == '"' {
                in_double = false;
                i += 1;
                continue;
            }
            if ((!windows_mode && ch == '\\') || (windows_mode && ch == '`'))
                && let Some(&next) = chars.get(i + 1)
            {
                out.push(next);
                i += 2;
                continue;
            }
            out.push(ch);
            i += 1;
            continue;
        }
        if ((!windows_mode && ch == '\\') || (windows_mode && ch == '`'))
            && let Some(&next) = chars.get(i + 1)
        {
            out.push(next);
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
        out.push(ch);
        i += 1;
    }
    out
}

pub(super) fn extract_shell_output_redirects(stmt: &str, windows_mode: bool) -> Vec<String> {
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
        let is_rw_redirect = ch == '<' && i + 1 < chars.len() && chars[i + 1] == '>';
        if ch == '>' || is_rw_redirect {
            i += if is_rw_redirect { 2 } else { 1 };
            if !is_rw_redirect && i < chars.len() && (chars[i] == '>' || chars[i] == '|') {
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
            let start = i;
            let mut sub_single = false;
            let mut sub_double = false;
            while i < chars.len() {
                let c = chars[i];
                if sub_single {
                    if c == '\'' {
                        sub_single = false;
                    }
                    i += 1;
                    continue;
                }
                if sub_double {
                    if ((!windows_mode && c == '\\') || (windows_mode && c == '`'))
                        && i + 1 < chars.len()
                    {
                        i += 2;
                        continue;
                    }
                    if c == '"' {
                        sub_double = false;
                    }
                    i += 1;
                    continue;
                }
                if ((!windows_mode && c == '\\') || (windows_mode && c == '`'))
                    && i + 1 < chars.len()
                {
                    i += 2;
                    continue;
                }
                if c == '\'' {
                    sub_single = true;
                    i += 1;
                    continue;
                }
                if c == '"' {
                    sub_double = true;
                    i += 1;
                    continue;
                }
                if c.is_ascii_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | ')') {
                    break;
                }
                i += 1;
            }
            let raw_target: String = chars[start..i].iter().collect();
            let target = dequote_shell_word(&raw_target, windows_mode);
            if !target.is_empty() && target != "/dev/null" {
                targets.push(target);
            }
            continue;
        }
        i += 1;
    }
    targets
}

fn protected_instruction_in_invocations(
    invocations: &[Vec<String>],
    cwd: &ShellCwd,
    virtual_links: &[(String, VirtualLinkTarget)],
    scope: CriticalPathScope<'_>,
    loaded_paths: &[std::path::PathBuf],
) -> bool {
    let check_operand = |operand: &str, active_cwd: &ShellCwd| -> bool {
        raw_operand_targets_protected_instruction(
            operand,
            active_cwd,
            virtual_links,
            scope,
            loaded_paths,
        )
    };
    fn collect_non_flags(args: &[String]) -> Vec<&str> {
        let mut out = Vec::new();
        let mut after_dd = false;
        for arg in args {
            if !after_dd && arg == "--" {
                after_dd = true;
                continue;
            }
            if !after_dd && arg.starts_with('-') && arg.len() > 1 {
                continue;
            }
            out.push(arg.as_str());
        }
        out
    }
    for argv in invocations {
        let Some(cmd_idx) = skip_shell_wrappers(argv) else {
            continue;
        };
        let raw_cmd = &argv[cmd_idx];
        let args = &argv[cmd_idx + 1..];
        let cmd = normalize_command_name(raw_cmd);
        match cmd.as_str() {
            "rm" | "rmdir" | "unlink" | "mv" | "touch" | "truncate" | "tee" | "chmod" | "chown"
            | "chgrp" | "shred" | "srm" | "chattr" => {
                let mut target_dir: Option<&str> = None;
                let mut i = 0usize;
                let mut after_dd = false;
                let mut positionals: Vec<&str> = Vec::new();
                while i < args.len() {
                    let arg = args[i].as_str();
                    if !after_dd && arg == "--" {
                        after_dd = true;
                        i += 1;
                        continue;
                    }
                    if !after_dd && (arg == "-t" || arg == "--target-directory") {
                        if let Some(dir) = args.get(i + 1) {
                            target_dir = Some(dir.as_str());
                            i += 2;
                            continue;
                        }
                    } else if !after_dd && let Some(dir) = arg.strip_prefix("--target-directory=") {
                        target_dir = Some(dir);
                        i += 1;
                        continue;
                    } else if !after_dd && arg.starts_with('-') && arg.len() > 1 {
                        i += 1;
                        continue;
                    }
                    positionals.push(arg);
                    i += 1;
                }
                for operand in &positionals {
                    if check_operand(operand, cwd) {
                        return true;
                    }
                }
                if cmd == "mv"
                    && let Some(dir) = target_dir
                {
                    if check_operand(dir, cwd) {
                        return true;
                    }
                    for src in &positionals {
                        if let Some(name) = src
                            .trim_end_matches(['/', '\\'])
                            .rsplit(['/', '\\'])
                            .next()
                            .filter(|n| !n.is_empty())
                        {
                            let joined = format!("{}/{name}", dir.trim_end_matches(['/', '\\']));
                            if check_operand(&joined, cwd) {
                                return true;
                            }
                        }
                    }
                }
            }
            "cp" | "ln" | "install" => {
                let mut target_dir: Option<&str> = None;
                let mut i = 0usize;
                let mut after_dd = false;
                let mut positionals: Vec<&str> = Vec::new();
                while i < args.len() {
                    let arg = args[i].as_str();
                    if !after_dd && arg == "--" {
                        after_dd = true;
                        i += 1;
                        continue;
                    }
                    if !after_dd && (arg == "-t" || arg == "--target-directory") {
                        if let Some(dir) = args.get(i + 1) {
                            target_dir = Some(dir.as_str());
                            i += 2;
                            continue;
                        }
                    } else if !after_dd && let Some(dir) = arg.strip_prefix("--target-directory=") {
                        target_dir = Some(dir);
                        i += 1;
                        continue;
                    } else if !after_dd && arg.starts_with('-') && arg.len() > 1 {
                        i += 1;
                        continue;
                    }
                    positionals.push(arg);
                    i += 1;
                }
                if let Some(dir) = target_dir {
                    if check_operand(dir, cwd) {
                        return true;
                    }
                    for src in &positionals {
                        if let Some(name) = src
                            .trim_end_matches(['/', '\\'])
                            .rsplit(['/', '\\'])
                            .next()
                            .filter(|n| !n.is_empty())
                        {
                            let joined = format!("{}/{name}", dir.trim_end_matches(['/', '\\']));
                            if check_operand(&joined, cwd) {
                                return true;
                            }
                        }
                    }
                } else if positionals.len() >= 2 {
                    let dst = positionals[positionals.len() - 1];
                    if check_operand(dst, cwd) {
                        return true;
                    }
                    for src in &positionals[..positionals.len() - 1] {
                        if let Some(name) = src
                            .trim_end_matches(['/', '\\'])
                            .rsplit(['/', '\\'])
                            .next()
                            .filter(|n| !n.is_empty())
                        {
                            let joined = format!("{}/{name}", dst.trim_end_matches(['/', '\\']));
                            if check_operand(&joined, cwd) {
                                return true;
                            }
                        }
                    }
                }
            }
            "dd" => {
                for arg in args {
                    if let Some(dst) = arg.strip_prefix("of=")
                        && check_operand(dst, cwd)
                    {
                        return true;
                    }
                }
            }
            "patch" => {
                for operand in collect_non_flags(args) {
                    if check_operand(operand, cwd) {
                        return true;
                    }
                }
            }
            "sed" | "perl" | "ruby" | "awk" | "gawk" => {
                let in_place = args.iter().any(|arg| {
                    arg == "-i"
                        || arg.starts_with("-i")
                        || arg == "--in-place"
                        || arg.starts_with("--in-place=")
                });
                if in_place {
                    for operand in collect_non_flags(args) {
                        if check_operand(operand, cwd) {
                            return true;
                        }
                    }
                }
            }
            "python" | "python3" | "node" | "nodejs" | "php" | "bun" | "deno" => {
                let inline = args
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "-c" | "-e" | "--eval"));
                if inline
                    && args
                        .iter()
                        .any(|arg| text_mentions_protected_instruction(arg, loaded_paths))
                {
                    return true;
                }
            }
            "git" => {
                let mut git_cwd = cwd.clone();
                let mut i = 0usize;
                let mut subcmd: Option<&str> = None;
                while i < args.len() {
                    let arg = args[i].as_str();
                    if arg == "-C" {
                        if let Some(dir) = args.get(i + 1) {
                            git_cwd = advance_shell_cwd(
                                &git_cwd,
                                std::slice::from_ref(dir),
                                false,
                                virtual_links,
                                scope,
                            );
                            i += 2;
                            continue;
                        }
                    } else if let Some(dir) = arg.strip_prefix("-C")
                        && !dir.is_empty()
                    {
                        git_cwd = advance_shell_cwd(
                            &git_cwd,
                            &[dir.to_string()],
                            false,
                            virtual_links,
                            scope,
                        );
                        i += 1;
                        continue;
                    } else if matches!(arg, "-c" | "--git-dir" | "--work-tree") {
                        i += 2;
                        continue;
                    } else if arg.starts_with('-') {
                        i += 1;
                        continue;
                    }
                    subcmd = Some(arg);
                    i += 1;
                    break;
                }
                if let Some(sub) = subcmd
                    && matches!(
                        sub,
                        "mv" | "rm" | "checkout" | "restore" | "apply" | "clean" | "reset"
                    )
                {
                    let mut branch_create = false;
                    let mut after_dd = false;
                    for operand in &args[i..] {
                        if operand == "--" {
                            after_dd = true;
                            continue;
                        }
                        if !after_dd && matches!(operand.as_str(), "-b" | "-B") {
                            branch_create = true;
                            continue;
                        }
                        if !after_dd && operand.starts_with('-') {
                            continue;
                        }
                        if sub == "checkout" && branch_create && !after_dd {
                            branch_create = false;
                            continue;
                        }
                        if check_operand(operand, &git_cwd) {
                            return true;
                        }
                    }
                }
            }
            "find" => {
                let mut has_mutator = false;
                let mut saw_expr = false;
                let mut starts: Vec<&str> = Vec::new();
                let mut names_or_paths: Vec<&str> = Vec::new();
                let mut j = 0usize;
                while j < args.len() {
                    let arg = args[j].as_str();
                    if arg == "-delete" {
                        has_mutator = true;
                    } else if matches!(arg, "-exec" | "-execdir" | "-ok" | "-okdir") {
                        has_mutator = true;
                        for exec_arg in &args[j + 1..] {
                            if exec_arg == ";" || exec_arg == "+" {
                                break;
                            }
                            if check_operand(exec_arg, cwd) {
                                return true;
                            }
                        }
                    } else if matches!(
                        arg,
                        "-name" | "-iname" | "-path" | "-ipath" | "-wholename" | "-iwholename"
                    ) {
                        saw_expr = true;
                        if let Some(pat) = args.get(j + 1) {
                            names_or_paths.push(pat.as_str());
                            j += 2;
                            continue;
                        }
                    } else if !saw_expr && !arg.starts_with('-') && arg != "(" && arg != "!" {
                        starts.push(arg);
                    } else {
                        saw_expr = true;
                    }
                    j += 1;
                }
                if has_mutator
                    && (starts.iter().any(|s| check_operand(s, cwd))
                        || names_or_paths.iter().any(|p| check_operand(p, cwd)))
                {
                    return true;
                }
            }
            "xargs" | "parallel" => {
                if args.iter().any(|arg| {
                    matches!(
                        normalize_command_name(arg).as_str(),
                        "rm" | "rmdir" | "unlink" | "mv" | "cp" | "ln" | "tee" | "chmod"
                    )
                }) && args.iter().any(|arg| {
                    text_mentions_protected_instruction(arg, loaded_paths)
                        || check_operand(arg, cwd)
                }) {
                    return true;
                }
            }
            "rd" | "remove-item" | "ri" | "del" | "erase" | "move" | "move-item" | "mi" | "ren"
            | "rename-item" | "rni" | "copy" | "copy-item" | "cpi" | "set-content" | "sc"
            | "add-content" | "ac" | "out-file" | "new-item" | "ni" | "clear-content" | "clc"
            | "icacls" | "attrib"
                if scope.windows_mode =>
            {
                for operand in collect_non_flags(args) {
                    if check_operand(operand, cwd) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn eval_protected_instruction_shell_text(
    command: &str,
    mut cwd: ShellCwd,
    scope: CriticalPathScope<'_>,
    depth: usize,
    mut dir_stack: Vec<ShellCwd>,
    mut virtual_links: Vec<(String, VirtualLinkTarget)>,
    loaded_paths: &[std::path::PathBuf],
) -> bool {
    if depth > 12 {
        return text_mentions_protected_instruction(command, loaded_paths)
            && (command.contains('>')
                || text_mentions_instruction_mutator_keyword(command, scope.windows_mode));
    }
    let Some(stmts) = split_top_level_statements(command, scope.windows_mode) else {
        return text_mentions_protected_instruction(command, loaded_paths)
            && (command.contains('>')
                || text_mentions_instruction_mutator_keyword(command, scope.windows_mode));
    };
    let mut cond_depth: usize = 0;
    let mut group_pipe_stack: Vec<bool> = Vec::new();
    let mut tainted_after_list = false;

    for stmt in stmts {
        if let Some(inner) = stmt.subshell_inner {
            if eval_protected_instruction_shell_text(
                inner,
                cwd.clone(),
                scope,
                depth + 1,
                dir_stack.clone(),
                virtual_links.clone(),
                loaded_paths,
            ) {
                return true;
            }
            if stmt.sep_after == StmtSep::Seq && tainted_after_list {
                cwd = ShellCwd::Tainted;
                dir_stack.clear();
                tainted_after_list = false;
            }
            continue;
        }

        for sub_cmd in extract_command_substitutions(stmt.raw, scope.windows_mode) {
            if eval_protected_instruction_shell_text(
                sub_cmd,
                cwd.clone(),
                scope,
                depth + 1,
                dir_stack.clone(),
                virtual_links.clone(),
                loaded_paths,
            ) {
                return true;
            }
        }

        for target in extract_shell_output_redirects(stmt.raw, scope.windows_mode) {
            if raw_operand_targets_protected_instruction(
                &target,
                &cwd,
                &virtual_links,
                scope,
                loaded_paths,
            ) {
                return true;
            }
        }

        let mut handled_nested_shell = false;
        if let Some(words) = split_simple_shell_words(stmt.raw, scope.windows_mode) {
            let mut idx = 0usize;
            while idx < words.len() {
                let w = words[idx].as_str();
                if matches!(w, "if" | "elif" | "while" | "until") {
                    cond_depth += 1;
                    idx += 1;
                } else if matches!(w, "then" | "do") {
                    cond_depth = cond_depth.saturating_sub(1);
                    idx += 1;
                } else if w == "{" || w == "(" {
                    group_pipe_stack.push(
                        stmt.sep_after == StmtSep::Pipe
                            || stmt.sep_after == StmtSep::Async
                            || group_pipe_stack.last().copied().unwrap_or(false),
                    );
                    idx += 1;
                } else if w == "}" || w == ")" {
                    group_pipe_stack.pop();
                    idx += 1;
                } else {
                    break;
                }
            }
            let effective = &words[idx..];
            if let Some(cmd_idx) = skip_shell_wrappers(effective) {
                let raw_cmd = &effective[cmd_idx];
                let args = &effective[cmd_idx + 1..];
                let verb = normalize_command_name(raw_cmd);
                let in_subshell_stage = stmt.sep_before == StmtSep::Pipe
                    || stmt.sep_after == StmtSep::Pipe
                    || stmt.sep_after == StmtSep::Async
                    || group_pipe_stack.last().copied().unwrap_or(false);
                match verb.as_str() {
                    "cd" | "pushd" | "popd" => {
                        if !in_subshell_stage {
                            if cond_depth > 0 {
                                cwd = ShellCwd::Tainted;
                                dir_stack.clear();
                            } else {
                                let next_cwd = match verb.as_str() {
                                    "cd" => {
                                        advance_shell_cwd(&cwd, args, true, &virtual_links, scope)
                                    }
                                    "pushd" => {
                                        if args.is_empty() {
                                            if let Some(top) = dir_stack.pop() {
                                                dir_stack.push(cwd.clone());
                                                top
                                            } else {
                                                ShellCwd::Tainted
                                            }
                                        } else {
                                            dir_stack.push(cwd.clone());
                                            advance_shell_cwd(
                                                &cwd,
                                                args,
                                                false,
                                                &virtual_links,
                                                scope,
                                            )
                                        }
                                    }
                                    "popd" => dir_stack.pop().unwrap_or(ShellCwd::Tainted),
                                    _ => cwd.clone(),
                                };
                                if stmt.sep_after == StmtSep::Or {
                                    tainted_after_list = true;
                                } else if matches!(stmt.sep_before, StmtSep::Or | StmtSep::And) {
                                    cwd = next_cwd;
                                    tainted_after_list = true;
                                } else {
                                    cwd = next_cwd;
                                }
                            }
                        }
                    }
                    "ln" => {
                        let ln_invocations = vec![effective[cmd_idx..].to_vec()];
                        if protected_instruction_in_invocations(
                            &ln_invocations,
                            &cwd,
                            &virtual_links,
                            scope,
                            loaded_paths,
                        ) {
                            return true;
                        }
                        if !in_subshell_stage && cond_depth == 0 {
                            record_ln_virtual_link(args, &cwd, &mut virtual_links, scope);
                        }
                    }
                    _ => {
                        if let Some(nested_script) =
                            extract_nested_shell_script(&effective[cmd_idx..])
                        {
                            handled_nested_shell = true;
                            if eval_protected_instruction_shell_text(
                                &nested_script,
                                cwd.clone(),
                                scope,
                                depth + 1,
                                dir_stack.clone(),
                                virtual_links.clone(),
                                loaded_paths,
                            ) {
                                return true;
                            }
                        }
                    }
                }
            }
        }

        if !handled_nested_shell {
            use codewhale_execpolicy::command_safety::{
                command_invocations, destroyer_floor_invocations,
            };
            if let Some(invocations) = command_invocations(stmt.raw) {
                if protected_instruction_in_invocations(
                    &invocations,
                    &cwd,
                    &virtual_links,
                    scope,
                    loaded_paths,
                ) {
                    return true;
                }
            } else if text_mentions_protected_instruction(stmt.raw, loaded_paths)
                && (stmt.raw.contains('>')
                    || text_mentions_instruction_mutator_keyword(stmt.raw, scope.windows_mode))
            {
                return true;
            }
            if let Some(floor_invocations) = destroyer_floor_invocations(stmt.raw)
                && protected_instruction_in_invocations(
                    &floor_invocations,
                    &cwd,
                    &virtual_links,
                    scope,
                    loaded_paths,
                )
            {
                return true;
            }
        }

        if stmt.sep_after == StmtSep::Seq && tainted_after_list {
            cwd = ShellCwd::Tainted;
            dir_stack.clear();
            tainted_after_list = false;
        }
    }
    false
}

pub(super) fn protected_instruction_write_for_call(
    tool_name: &str,
    params: &Value,
    workspace: Option<&std::path::Path>,
    loaded_instruction_paths: &[std::path::PathBuf],
) -> bool {
    protected_instruction_write_with_home(
        tool_name,
        params,
        workspace,
        codewhale_paths::user_home().as_deref(),
        loaded_instruction_paths,
        cfg!(windows),
    )
}

pub(super) fn protected_instruction_write_with_home(
    tool_name: &str,
    params: &Value,
    workspace: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
    loaded_instruction_paths: &[std::path::PathBuf],
    windows_mode: bool,
) -> bool {
    let ws_cwd = workspace
        .map(std::path::Path::to_path_buf)
        .map_or(ShellCwd::Unset, ShellCwd::Known);
    let file_scope = CriticalPathScope {
        workspace,
        home,
        windows_mode,
        dotglob: !windows_mode,
    };

    if let Some(targets) = file_write_target_paths(tool_name, params)
        && targets.iter().any(|target| {
            raw_operand_targets_protected_instruction(
                target,
                &ws_cwd,
                &[],
                file_scope,
                loaded_instruction_paths,
            )
        })
    {
        return true;
    }

    let canonical =
        codewhale_execpolicy::canonical_action::canonical_action_alias(tool_name, params);
    if canonical == "apply_patch"
        && let Some(patch_text) = params.get("patch").and_then(Value::as_str)
    {
        for line in patch_text.lines() {
            let trimmed = line.trim();
            for prefix in [
                "*** Add File: ",
                "*** Update File: ",
                "*** Delete File: ",
                "*** Move to: ",
            ] {
                if let Some(target) = trimmed.strip_prefix(prefix)
                    && raw_operand_targets_protected_instruction(
                        target.trim(),
                        &ws_cwd,
                        &[],
                        file_scope,
                        loaded_instruction_paths,
                    )
                {
                    return true;
                }
            }
        }
    }

    if file_write_delete_paths(tool_name, params, workspace)
        .iter()
        .any(|target| {
            raw_operand_targets_protected_instruction(
                target,
                &ws_cwd,
                &[],
                file_scope,
                loaded_instruction_paths,
            )
        })
    {
        return true;
    }
    let lower_name = canonical.to_ascii_lowercase();
    if matches!(
        lower_name.as_str(),
        "write_file" | "edit_file" | "apply_patch" | "delete_file" | "move_file" | "rename_file"
    ) || lower_name.starts_with("write_")
        || lower_name.starts_with("edit_")
        || lower_name.starts_with("patch_")
        || lower_name.starts_with("delete_")
        || lower_name.starts_with("move_")
        || lower_name.starts_with("rename_")
    {
        for key in [
            "path",
            "file_path",
            "filePath",
            "target_path",
            "targetPath",
            "destination",
            "dest",
            "source",
            "from",
            "to",
        ] {
            if let Some(raw) = params.get(key).and_then(Value::as_str)
                && raw_operand_targets_protected_instruction(
                    raw,
                    &ws_cwd,
                    &[],
                    file_scope,
                    loaded_instruction_paths,
                )
            {
                return true;
            }
        }
    }

    let shell_cwd = shell_tool_initial_cwd(tool_name, params, workspace, home, windows_mode);
    let command_text = shell_command_param(params);
    for (input, text) in execution_texts(tool_name, params) {
        match input {
            ExecutionInput::Command | ExecutionInput::Unjoinable => {
                let scope = scope_for_command(&text, workspace, home, windows_mode);
                if eval_protected_instruction_shell_text(
                    &text,
                    shell_cwd.clone(),
                    scope,
                    0,
                    Vec::new(),
                    Vec::new(),
                    loaded_instruction_paths,
                ) {
                    return true;
                }
            }
            ExecutionInput::Stdin => {
                if command_invokes_shell(command_text, windows_mode)
                    || split_top_level_statements(&text, windows_mode).is_none()
                {
                    let scope = scope_for_command(&text, workspace, home, windows_mode);
                    if eval_protected_instruction_shell_text(
                        &text,
                        shell_cwd.clone(),
                        scope,
                        0,
                        Vec::new(),
                        Vec::new(),
                        loaded_instruction_paths,
                    ) {
                        return true;
                    }
                }
            }
        }
    }
    false
}
