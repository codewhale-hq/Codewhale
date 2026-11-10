#!/usr/bin/env python3
import fnmatch
import glob
import json
import os
import re
import shlex
import sys

FREEZE_VARIABLE = "CODEWHALE_RELEASE_FREEZE"
FROZEN_GLOBS = (".github/workflows/release*.yml", ".github/workflows/auto-tag.yml")

SHELL_TOOLS = {"bash", "Bash", "exec_shell"}
FILE_TOOLS = {
    "write",
    "edit",
    "write_file",
    "edit_file",
    "fim_edit",
    "File",
    "file",
    "apply_patch",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
}
FILE_READ_ACTIONS = {"read", "list", "search_name", "search_content"}

KEYWORDS = {"if", "then", "do", "else", "elif", "while", "until", "!", "{", "}"}
WRAPPERS = {"time", "sudo", "doas", "env", "command", "builtin", "exec", "nice", "nohup", "xargs"}
SHELLS = {"sh", "bash", "zsh", "dash", "ksh"}
READ_ONLY_PROGRAMS = {
    "cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "ag", "wc",
    "diff", "cmp", "ls", "stat", "file", "shasum", "sha256sum", "md5", "md5sum", "jq",
    "yq", "bat", "actionlint", "yamllint", "echo", "printf", "test", "[", "realpath",
    "basename", "dirname", "sed", "awk", "gawk", "perl",
}
IN_PLACE_PROGRAMS = {"sed", "awk", "gawk", "perl"}
READ_ONLY_GIT = {
    "diff", "log", "show", "status", "blame", "annotate", "grep", "ls-files",
    "cat-file", "rev-parse", "shortlog",
}
GIT_VALUE_OPTIONS = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env", "--super-prefix"}
COMMIT_VALUE_OPTIONS = {
    "--message", "--file", "--reuse-message", "--reedit-message", "--fixup", "--squash",
    "--author", "--date", "--template", "--cleanup", "--trailer", "--pathspec-from-file",
}

TEMPLATE_SUFFIXES = (".example", ".sample", ".template")
KEY_SUFFIXES = (".pem", ".key", ".p8", ".p12", ".pfx", ".jks", ".keystore", ".ppk")
SSH_IDENTITIES = {"id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"}
BUILD_FOLDERS = ("target", "node_modules", ".mimosa")
FORBIDDEN_SAMPLES = (
    ".env", ".env.local", "prod.env", "id_rsa", "id_ed25519", "server.pem", "server.key",
    "AuthKey.p8", "cert.p12", "cert.pfx", "release.jks", "release.keystore", "putty.ppk",
)

BLANKET = (
    "Staging guard: this stages everything at once (the A, all or update flags, a dot, a "
    "star or the repo root) and can sweep in credentials, build output and unfinished work "
    "from other agents. Stage your own files by name."
)
COMMIT_ALL = (
    "Staging guard: commit with the a or all flag sweeps in every tracked change, including "
    "edits other agents have not finished. Stage your own changes by name, then commit "
    "without that flag."
)
CREDENTIALS = (
    "Staging guard: {name} holds credentials (.env style), which are never committed. "
    "Templates ending in .example, .sample or .template are fine. Keep the real one "
    "untracked and listed in .gitignore."
)
PRIVATE_KEY = (
    "Staging guard: {name} looks like a private key (pem, key, p8, p12, pfx, keystore or an "
    "ssh identity), and keys are never committed. If it is a deliberate test fixture, ask "
    "the person to stage it."
)
BUILD_OUTPUT = (
    "Staging guard: {name} holds build output, dependencies or tool state, which never "
    "belongs in git. Stage the source change only, and add that folder to .gitignore if it "
    "shows up as untracked."
)
FROZEN = (
    "Release freeze: {name} is a release workflow, frozen while CODEWHALE_RELEASE_FREEZE "
    "is set. Reading it is fine; ask the person before changing it. The freeze lifts when "
    "that variable is unset."
)

HEREDOC = re.compile(r"<<-?[ \t]*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1[^\n]*\n.*?\n[ \t]*\2[ \t]*(?=\n|$)", re.S)
PATCH_PATHS = re.compile(
    r"^(?:\+\+\+|---) (?:[ab]/)?(\S+)|^\*\*\* (?:Update|Add|Delete) File: (.+?)\s*$|^\*\*\* Move to: (.+?)\s*$",
    re.M,
)
ASSIGNMENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
COMMAND_FIELD = re.compile(r'"command"\s*:\s*"((?:[^"\\]|\\.)*)')


def display(name):
    cleaned = "".join(ch for ch in name if ch not in "'\"`").strip()
    return cleaned[:32] or "this"


def freeze_on():
    return os.environ.get(FREEZE_VARIABLE, "").strip().lower() not in ("", "0", "false", "no", "off")


def is_separator(token):
    return bool(token) and all(ch in "();|&\n" for ch in token)


def is_redirect(token):
    return bool(token) and set(token) <= set("<>&|") and ("<" in token or ">" in token)


def segments(text):
    text = HEREDOC.sub("\n", text)
    try:
        lexer = shlex.shlex(text, posix=True, punctuation_chars="();<>|&\n")
        lexer.whitespace = " \t\r"
        lexer.whitespace_split = True
        lexer.commenters = ""
        tokens = list(lexer)
    except ValueError:
        for piece in re.split(r"[;&|\n]+", text):
            yield [token.strip("'\"") for token in piece.split()], []
        return
    words, targets = [], []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token.startswith("#"):
            while index < len(tokens) and tokens[index] != "\n":
                index += 1
            continue
        if is_redirect(token):
            if index + 1 < len(tokens):
                operand = tokens[index + 1]
                duplicate = token.endswith("&") and (operand.isdigit() or operand == "-")
                if not duplicate and "<" not in token:
                    targets.append(operand)
            index += 2
            continue
        if is_separator(token):
            if words:
                yield words, targets
            words, targets = [], []
        else:
            words.append(token)
        index += 1
    if words:
        yield words, targets


def strip_leading(words):
    index, wrapped = 0, False
    while index < len(words):
        word = words[index]
        if word in KEYWORDS or word in WRAPPERS or ASSIGNMENT.match(word) or (wrapped and word.startswith("-")):
            wrapped = wrapped or word in WRAPPERS
            index += 1
            continue
        break
    return words[index:]


def shell_argument(words):
    for index, word in enumerate(words[1:-1], start=1):
        if word.startswith("-") and not word.startswith("--") and "c" in word[1:]:
            return words[index + 1]
    return None


def commands(text, depth=0):
    for words, targets in segments(text):
        words = strip_leading(words)
        if not words:
            if targets:
                yield [], targets
            continue
        program = os.path.basename(words[0])
        if depth < 3 and program in SHELLS:
            inner = shell_argument(words)
            if inner is not None:
                yield from commands(inner, depth + 1)
                continue
        if depth < 3 and program == "eval":
            yield from commands(" ".join(words[1:]), depth + 1)
            continue
        yield words, targets


def git_subcommand(words):
    index = 1
    while index < len(words):
        word = words[index]
        if word in GIT_VALUE_OPTIONS:
            index += 2
            continue
        if word.startswith("-"):
            index += 1
            continue
        return word, words[index + 1:]
    return None, []


def pathspec_body(spec):
    spec = spec.replace("\\", "/")
    if spec.startswith(":("):
        magic, _, rest = spec[2:].partition(")")
        if "exclude" in magic:
            return None
        return rest
    if spec.startswith(":"):
        rest = spec[1:]
        if rest[:1] in ("!", "^"):
            return None
        return rest.lstrip("/")
    return spec


def is_blanket(spec):
    body = pathspec_body(spec)
    if body is None:
        return False
    parts = [part for part in body.strip().split("/") if part]
    return all(part in (".", "..", "*") for part in parts)


def forbidden(spec):
    body = pathspec_body(spec)
    if body is None:
        return None
    parts = [part for part in body.split("/") if part not in ("", ".")]
    if not parts:
        return None
    for folder in BUILD_FOLDERS:
        if folder in parts and "fixtures" not in parts:
            return BUILD_OUTPUT.format(name=folder)
    base = parts[-1]
    lower = base.lower()
    if any(ch in lower for ch in "*?["):
        samples = [sample for sample in FORBIDDEN_SAMPLES if fnmatch.fnmatch(sample.lower(), lower)]
        if not samples:
            return None
        lower = samples[0].lower()
    if lower.endswith(TEMPLATE_SUFFIXES):
        return None
    if lower == ".env" or lower.startswith(".env.") or lower.endswith(".env"):
        return CREDENTIALS.format(name=display(base))
    if lower in SSH_IDENTITIES or lower.endswith(KEY_SUFFIXES):
        return PRIVATE_KEY.format(name=display(base))
    return None


def add_verdict(args):
    specs, blanket, dry_run, literal = [], False, False, False
    for arg in args:
        if literal:
            specs.append(arg)
        elif arg == "--":
            literal = True
        elif arg in ("--all", "--no-ignore-removal", "--update"):
            blanket = True
        elif arg == "--dry-run":
            dry_run = True
        elif arg.startswith("--"):
            continue
        elif arg.startswith("-") and len(arg) > 1:
            letters = arg[1:]
            dry_run = dry_run or "n" in letters
            blanket = blanket or "A" in letters or "u" in letters
        else:
            specs.append(arg)
    if dry_run:
        return None
    if blanket or any(is_blanket(spec) for spec in specs):
        return BLANKET
    for spec in specs:
        reason = forbidden(spec)
        if reason:
            return reason
    return None


def commit_verdict(args):
    specs, literal, index = [], False, 0
    while index < len(args):
        arg = args[index]
        if literal:
            specs.append(arg)
        elif arg == "--":
            literal = True
        elif arg == "--all":
            return COMMIT_ALL
        elif arg.startswith("--"):
            if "=" not in arg and arg in COMMIT_VALUE_OPTIONS:
                index += 1
        elif arg.startswith("-") and len(arg) > 1:
            letters = arg[1:]
            for position, letter in enumerate(letters):
                if letter == "a":
                    return COMMIT_ALL
                if letter in "mFCct":
                    if position == len(letters) - 1:
                        index += 1
                    break
                if letter in "Su":
                    break
        else:
            specs.append(arg)
        index += 1
    if any(is_blanket(spec) for spec in specs):
        return BLANKET
    for spec in specs:
        reason = forbidden(spec)
        if reason:
            return reason
    return None


def relative(token, workspace):
    text = token.strip().replace("\\", "/")
    if not text or text.startswith("~"):
        return None
    if os.path.isabs(text):
        root = workspace.replace("\\", "/").rstrip("/") + "/"
        if not text.startswith(root):
            return None
        text = text[len(root):]
    while text.startswith("./"):
        text = text[2:]
    text = text.rstrip("/")
    return text if text and text != "." else None


def frozen_files(workspace):
    found = set()
    for pattern in FROZEN_GLOBS:
        for path in glob.glob(os.path.join(workspace, pattern)):
            found.add(os.path.relpath(path, workspace).replace("\\", "/"))
    return sorted(found)


def frozen_hit(token, workspace, frozen):
    rel = relative(token, workspace)
    if rel is None:
        return None
    globbed = any(ch in rel for ch in "*?[")
    for path in frozen:
        same_depth = path.count("/") == rel.count("/") or "**" in rel
        if rel == path or path.startswith(rel + "/") or (globbed and same_depth and fnmatch.fnmatch(path, rel)):
            return os.path.basename(path)
    for pattern in FROZEN_GLOBS:
        if fnmatch.fnmatch(rel, pattern):
            return os.path.basename(rel)
    return None


def writes_in_place(words):
    for word in words[1:]:
        if word.startswith("--in-place") or word == "inplace":
            return True
        if word.startswith("-") and not word.startswith("--") and "i" in word[1:]:
            return True
    return False


def shell_freeze_verdict(words, targets, workspace, frozen):
    for target in targets:
        hit = frozen_hit(target, workspace, frozen)
        if hit:
            return FROZEN.format(name=display(hit))
    hits = [hit for hit in (frozen_hit(word, workspace, frozen) for word in words[1:]) if hit]
    if not hits:
        return None
    program = os.path.basename(words[0])
    if program in IN_PLACE_PROGRAMS and writes_in_place(words):
        return FROZEN.format(name=display(hits[0]))
    if program in READ_ONLY_PROGRAMS:
        return None
    if program == "git" and git_subcommand(words)[0] in READ_ONLY_GIT:
        return None
    return FROZEN.format(name=display(hits[0]))


def shell_verdict(text, workspace, freeze):
    frozen = frozen_files(workspace) if freeze else []
    for words, targets in commands(text):
        if freeze:
            reason = shell_freeze_verdict(words, targets, workspace, frozen)
            if reason:
                return reason
        if not words or os.path.basename(words[0]) != "git":
            continue
        subcommand, args = git_subcommand(words)
        if subcommand in ("add", "stage"):
            reason = add_verdict(args)
        elif subcommand == "commit":
            reason = commit_verdict(args)
        else:
            reason = None
        if reason:
            return reason
    return None


def file_targets(args):
    paths = []
    for key in ("path", "file_path", "filePath", "notebook_path"):
        value = args.get(key)
        if isinstance(value, str):
            paths.append(value)
    for key in ("replace", "changes", "edits"):
        items = args.get(key)
        if isinstance(items, list):
            paths.extend(item["path"] for item in items if isinstance(item, dict) and isinstance(item.get("path"), str))
    patch = args.get("patch") or args.get("input")
    if isinstance(patch, str):
        for groups in PATCH_PATHS.findall(patch):
            paths.extend(group for group in groups if group)
    return paths


def file_freeze_verdict(args, raw, workspace):
    frozen = frozen_files(workspace)
    if not isinstance(args, dict):
        for path in frozen:
            if path in raw:
                return FROZEN.format(name=display(os.path.basename(path)))
        return None
    if str(args.get("action", "")).lower() in FILE_READ_ACTIONS:
        return None
    for target in file_targets(args):
        hit = frozen_hit(target, workspace, frozen)
        if hit:
            return FROZEN.format(name=display(hit))
    return None


def shell_text(args, raw):
    if isinstance(args, dict) and isinstance(args.get("command"), str):
        return args["command"]
    match = COMMAND_FIELD.search(raw)
    if match:
        fragment = match.group(1)
        while fragment:
            try:
                return json.loads('"' + fragment + '"')
            except ValueError:
                fragment = fragment[:-1]
    return raw


def verdict(tool, args, raw, workspace):
    freeze = freeze_on()
    if tool in SHELL_TOOLS:
        return shell_verdict(shell_text(args, raw), workspace, freeze)
    if freeze and tool in FILE_TOOLS:
        return file_freeze_verdict(args, raw, workspace)
    return None


def parse(raw):
    try:
        return json.loads(raw)
    except ValueError:
        return None


def main():
    if "DEEPSEEK_TOOL_NAME" in os.environ:
        raw = os.environ.get("DEEPSEEK_TOOL_ARGS", "")
        workspace = os.environ.get("DEEPSEEK_WORKSPACE") or os.getcwd()
        reason = verdict(os.environ["DEEPSEEK_TOOL_NAME"], parse(raw), raw, workspace)
        if reason:
            print(json.dumps({"decision": "deny", "reason": reason}))
        return 0
    payload = json.load(sys.stdin)
    tool_input = payload.get("tool_input")
    raw = json.dumps(tool_input)
    workspace = os.environ.get("CLAUDE_PROJECT_DIR") or payload.get("cwd") or os.getcwd()
    reason = verdict(payload.get("tool_name", ""), tool_input, raw, workspace)
    if reason:
        print(reason, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
