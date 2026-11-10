#!/usr/bin/env bash
# app-server runtime API release smoke.
#
# Two independent checks, both safe to run before every release:
#
#   1. app-server stdio probe (always): drives `codewhale app-server --stdio`
#      with JSON-RPC health/capabilities requests and asserts the control
#      surface answers. Spends no model tokens, makes no network calls, and runs
#      against a throwaway config so it never reads the maintainer's real keys.
#
#   2. provider/model matrix (opt-in, --matrix): discovers configured providers
#      from `codewhale auth list`, maps each to a cheap sentinel model, and
#      either prints the plan (default, dry-run) or runs a tiny `exec` prompt per
#      provider (--real). `auth list` reports presence flags only; secrets are
#      never printed and exec output is passed through a redactor.
#
# Usage:
#   scripts/release/app-server-smoke.sh                       # stdio probe only
#   scripts/release/app-server-smoke.sh --matrix              # + print provider matrix (dry-run)
#   scripts/release/app-server-smoke.sh --matrix --real       # + exec a sentinel per provider
#   scripts/release/app-server-smoke.sh --matrix --provider deepseek --provider zai
#   scripts/release/app-server-smoke.sh --bin ./target/release/codewhale
#
# Binary resolution order: --bin <path>, $CODEWHALE_BIN, ./target/release/codewhale, PATH.
# Per-provider cheap-model override: SMOKE_MODEL_<SLUG> with slug upper-cased and
#   '-' replaced by '_', e.g. SMOKE_MODEL_XIAOMI_MIMO=mimo-7b.

set -euo pipefail

PASS=0
FAIL=0
BIN="${CODEWHALE_BIN:-}"
DO_MATRIX=0
DRY_RUN=1
SENTINEL="${SMOKE_SENTINEL:-Reply with exactly the single word: pong}"
EXEC_TIMEOUT="${SMOKE_EXEC_TIMEOUT:-60}"
declare -a ONLY_PROVIDERS=()

# Best-effort CHEAP models per provider slot. These are intentionally overridable
# (SMOKE_MODEL_<SLUG>) because there is no committed route-effective model
# inventory yet (#3205); unmapped configured providers fail loudly in --real mode
# instead of guessing. Keep this list conservative: only add a provider here when
# its mapped id is a genuinely cheap/small model. Providers without a verified
# cheap default (e.g. arcee, openrouter, xiaomi-mimo, openai-codex) are left
# unmapped on purpose and must be given a model per run via SMOKE_MODEL_<SLUG>.
default_model_for() {
    case "$1" in
        deepseek) echo "deepseek-chat" ;;
        zai) echo "glm-4-flash" ;;
        moonshot) echo "moonshot-v1-8k" ;;
        openai) echo "gpt-4o-mini" ;;
        *) echo "" ;;
    esac
}

log()  { printf '\033[1;34m>>> %s\033[0m\n' "$*"; }
pass() { printf '\033[1;32m  \xe2\x9c\x93 %s\033[0m\n' "$*"; PASS=$((PASS + 1)); }
fail() { printf '\033[1;31m  \xe2\x9c\x97 %s\033[0m\n' "$*"; FAIL=$((FAIL + 1)); }
note() { printf '    %s\n' "$*"; }

usage() { sed -n '2,33p' "$0"; }

# Mask anything that looks like a credential. Defense in depth: the CLI does not
# print secrets, but exec output is untrusted text.
redact() {
    sed -E \
        -e 's/(sk-[A-Za-z0-9]{2})[A-Za-z0-9_-]+/\1…REDACTED/g' \
        -e 's/(Bearer +)[A-Za-z0-9._-]+/\1REDACTED/g' \
        -e 's/(([Aa][Pp][Ii][_-]?[Kk][Ee][Yy]|[Tt][Oo][Kk][Ee][Nn]|[Ss][Ee][Cc][Rr][Ee][Tt])["'"'"' :=]+)[^"'"'"' ,}]+/\1REDACTED/g'
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --matrix) DO_MATRIX=1 ;;
            --real) DRY_RUN=0 ;;
            --dry-run) DRY_RUN=1 ;;
            --provider) shift; ONLY_PROVIDERS+=("${1:?--provider needs a value}") ;;
            --bin) shift; BIN="${1:?--bin needs a path}" ;;
            -h|--help) usage; exit 0 ;;
            *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
        esac
        shift
    done
}

resolve_bin() {
    if [[ -n "$BIN" ]]; then
        [[ -x "$BIN" ]] || { echo "codewhale binary not executable: $BIN" >&2; exit 2; }
        return
    fi
    if [[ -x "./target/release/codewhale" ]]; then
        BIN="./target/release/codewhale"
    elif command -v codewhale >/dev/null 2>&1; then
        BIN="$(command -v codewhale)"
    else
        echo "could not find a codewhale binary." >&2
        echo "  build one: cargo build -p codewhale-cli --release" >&2
        echo "  or pass:   --bin <path> / CODEWHALE_BIN=<path>" >&2
        exit 2
    fi
}

# ── Check 1: app-server stdio control surface ────────────────────────────────

stdio_probe() {
    log "=== app-server stdio probe (no model tokens) ==="
    local out
    # Keep the canonical home short for macOS Unix sockets, and resolve /tmp's
    # symlink before the credential store's no-follow directory walk.
    if ! out="$(python3 - "$BIN" "${SMOKE_STDIO_TIMEOUT:-20}" <<'PY'
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="cw-", dir="/tmp") as temporary:
    root = Path(temporary).resolve()
    state_root = root / "state"
    state_root.mkdir(mode=0o700)
    config = root / "config.toml"
    config.write_text("telemetry = false\n")
    env = {key: os.environ[key] for key in ("PATH", "USER", "SYSTEMROOT") if key in os.environ}
    env.update(CODEWHALE_HOME=str(state_root), XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"), TMPDIR=str(root), CODEWHALE_NO_UPDATE_CHECK="1")
    requests = json.dumps({"id": 0, "method": "initialize", "params": {"clientInfo": {"name": "codewhale-smoke", "version": "1"}}}) + "\n"
    requests += json.dumps({"method": "initialized"}) + "\n"
    requests += "".join(json.dumps({"id": i, "method": method}) + "\n"
        for i, method in enumerate(("healthz", "capabilities", "app/capabilities",
            "prompt/capabilities", "thread/capabilities", "shutdown"), 1))
    process = subprocess.Popen([binary, "app-server", "--stdio", "--config", str(config)],
        cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, start_new_session=True)
    try:
        stdout, _ = process.communicate(requests.encode(), timeout=float(sys.argv[2]))
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        print("app-server stdio did not shut down within the probe deadline", file=sys.stderr)
        sys.exit(1)
    if process.returncode:
        print(f"app-server stdio exited with status {process.returncode}", file=sys.stderr)
        sys.exit(1)
    try:
        replies = [json.loads(line) for line in stdout.splitlines()]
        if {reply.get("id") for reply in replies if "result" in reply} != set(range(0, 7)):
            raise ValueError("missing successful response")
    except (ValueError, AttributeError):
        print("app-server stdio did not return seven successful JSON-RPC responses", file=sys.stderr)
        sys.exit(1)
    sys.stdout.buffer.write(stdout)
PY
)"; then
        fail "app-server stdio responses and graceful shutdown"
        return
    fi

    if [[ -z "$out" ]]; then
        fail "app-server --stdio produced no output"
        return
    fi

    probe_assert "$out" '"status":"ok"'        "healthz reports ok"
    probe_assert "$out" '"thread/request"'     "capabilities advertise thread/* methods"
    probe_assert "$out" '"prompt/run"'         "capabilities advertise prompt/run"
    probe_assert "$out" '"transport":"stdio+http"' "app/capabilities reports transport"
    probe_assert "$out" '"prompt/request"'     "prompt/capabilities lists prompt/request"
    probe_assert "$out" '"thread/goal/set"'    "thread/capabilities lists goal methods"
}

probe_assert() {
    local haystack="$1" needle="$2" desc="$3"
    if printf '%s' "$haystack" | grep -qF "$needle"; then
        pass "$desc"
    else
        fail "$desc (missing: $needle)"
    fi
}

# ── Check 2: provider/model matrix ───────────────────────────────────────────

# Echo configured provider slugs (active != missing) from `codewhale auth list`.
configured_providers() {
    "$BIN" auth list 2>/dev/null \
        | awk 'NR > 1 && NF >= 2 && $NF != "missing" { print $1 }'
}

model_for() {
    local slug="$1" var
    var="SMOKE_MODEL_$(printf '%s' "$slug" | tr '[:lower:]-' '[:upper:]_')"
    if [[ -n "${!var:-}" ]]; then
        printf '%s' "${!var}"
    else
        default_model_for "$slug"
    fi
}

want_provider() {
    [[ ${#ONLY_PROVIDERS[@]} -eq 0 ]] && return 0
    local p
    for p in "${ONLY_PROVIDERS[@]}"; do
        [[ "$p" == "$1" ]] && return 0
    done
    return 1
}

run_matrix() {
    if [[ $DRY_RUN -eq 1 ]]; then
        log "=== provider/model matrix (dry-run) ==="
    else
        log "=== provider/model matrix (real exec) ==="
    fi

    local -a providers=()
    local p
    while IFS= read -r p; do
        [[ -z "$p" ]] && continue
        want_provider "$p" && providers+=("$p")
    done < <(configured_providers)

    if [[ ${#providers[@]} -eq 0 ]]; then
        note "no configured providers discovered (auth list); nothing to test"
        return
    fi

    local slug model
    for slug in "${providers[@]}"; do
        model="$(model_for "$slug")"
        if [[ -z "$model" ]]; then
            if [[ $DRY_RUN -eq 1 ]]; then
                note "$slug -> (UNMAPPED; set SMOKE_MODEL_$(printf '%s' "$slug" | tr '[:lower:]-' '[:upper:]_'))"
            else
                fail "$slug has no cheap-model mapping (set SMOKE_MODEL_$(printf '%s' "$slug" | tr '[:lower:]-' '[:upper:]_')=<model>)"
            fi
            continue
        fi

        if [[ $DRY_RUN -eq 1 ]]; then
            note "$slug -> $model    [$BIN --provider $slug --model $model exec \"<sentinel>\"]"
            continue
        fi

        local out rc=0
        if command -v timeout >/dev/null 2>&1; then
            out="$(timeout "$EXEC_TIMEOUT" "$BIN" --provider "$slug" --model "$model" exec "$SENTINEL" 2>&1)" || rc=$?
        else
            out="$("$BIN" --provider "$slug" --model "$model" exec "$SENTINEL" 2>&1)" || rc=$?
        fi
        if [[ $rc -eq 0 ]]; then
            pass "$slug/$model exec ok"
            note "$(printf '%s' "$out" | redact | tail -n 1)"
        else
            fail "$slug/$model exec failed (rc=$rc)"
            note "$(printf '%s' "$out" | redact | tail -n 3)"
        fi
    done
}

main() {
    parse_args "$@"
    resolve_bin
    log "Using binary: $BIN"

    stdio_probe
    if [[ $DO_MATRIX -eq 1 ]]; then
        run_matrix
    else
        note "(provider/model matrix skipped; pass --matrix to enable)"
    fi

    echo ""
    log "Results: $PASS passed, $FAIL failed"
    [[ $FAIL -eq 0 ]]
}

main "$@"
