#!/usr/bin/env bash
# Tests for app-server-smoke.sh: drive it against a fake `codewhale` binary so
# the stdio-probe assertions, auth-list matrix parser, cheap-model mapping, and
# secret redaction are all exercised without a real build or any model tokens.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SMOKE="$SCRIPT_DIR/app-server-smoke.sh"
TESTS=0
FAILED=0

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAKE="$WORK/codewhale"
cat >"$FAKE" <<'FAKE_EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "app-server" ]]; then
    [[ "$HOME" == "$CODEWHALE_HOME" && -d "$HOME" && -z "${OPENAI_API_KEY:-}" ]] || exit 4
    if [[ -f "$(dirname "$0")/stall" ]]; then
        sleep 30
    fi
    cat >/dev/null   # drain the JSON-RPC requests
    printf '%s\n' \
        '{"id":0,"result":{"userAgent":"fixture","codexHome":"/tmp","platformFamily":"unix","platformOs":"macos"}}' \
        '{"id":1,"result":{"status":"ok","service":"deepseek-app-server","transport":"stdio"}}' \
        '{"id":2,"result":{"methods":["thread/request","prompt/run","prompt/request","thread/goal/set"]}}' \
        '{"id":3,"result":{"ok":true,"data":{"transport":"stdio+http"}}}' \
        '{"id":4,"result":{}}' \
        '{"id":5,"result":{}}' \
        '{"id":6,"result":{"ok":true}}'
    exit 0
fi
if [[ "${1:-}" == "auth" && "${2:-}" == "list" ]]; then
    printf '%s\n' \
        'provider     config store env  active' \
        'deepseek      yes     -      no   config' \
        'zai           no      -      yes  env' \
        'openrouter    no      -      no   missing' \
        'arcee         yes     -      no   config'
    exit 0
fi
# exec form: --provider <slug> --model <model> exec <prompt>
slug=""; model=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --provider) slug="$2"; shift 2 ;;
        --model) model="$2"; shift 2 ;;
        exec) shift; break ;;
        *) shift ;;
    esac
done
# Emit a fake secret on the reply line to prove redaction scrubs it.
printf 'pong [%s/%s] token=sk-DEADBEEFCAFE12345\n' "$slug" "$model"
exit 0
FAKE_EOF
chmod +x "$FAKE"

# run_smoke <expected_exit> -- <args...> : capture combined output, check exit.
LAST_OUT=""
run_smoke() {
    local expected="$1"; shift
    [[ "$1" == "--" ]] && shift
    local rc=0
    LAST_OUT="$(bash "$SMOKE" --bin "$FAKE" "$@" 2>&1)" || rc=$?
    TESTS=$((TESTS + 1))
    if [[ "$rc" != "$expected" ]]; then
        printf '\033[1;31mFAIL\033[0m exit %s (wanted %s) for: %s\n' "$rc" "$expected" "$*"
        printf '%s\n' "$LAST_OUT" | sed 's/^/    /'
        FAILED=$((FAILED + 1))
        return 1
    fi
    return 0
}

want()    { TESTS=$((TESTS + 1)); if printf '%s' "$LAST_OUT" | grep -qF "$1"; then :; else printf '\033[1;31mFAIL\033[0m output missing: %s\n' "$1"; FAILED=$((FAILED + 1)); fi; }
want_not(){ TESTS=$((TESTS + 1)); if printf '%s' "$LAST_OUT" | grep -qF "$1"; then printf '\033[1;31mFAIL\033[0m output should not contain: %s\n' "$1"; FAILED=$((FAILED + 1)); fi; }

# 1. Default: stdio probe only, passes, matrix skipped.
run_smoke 0 -- || true
want "healthz reports ok"
want "capabilities advertise thread/* methods"
want "app/capabilities reports transport"
want "matrix skipped"

# 2. Dry-run matrix: maps known providers, flags unmapped, skips 'missing'.
run_smoke 0 -- --matrix || true
want "deepseek -> deepseek-chat"
want "zai -> glm-4-flash"
want "arcee -> (UNMAPPED"
want_not "openrouter"

# 3. Real exec for a mapped provider passes; secret is redacted.
run_smoke 0 -- --matrix --real --provider deepseek || true
want "deepseek/deepseek-chat exec ok"
want "REDACTED"
want_not "sk-DEADBEEFCAFE12345"

# 4. Real exec across all configured providers fails on the unmapped one.
run_smoke 1 -- --matrix --real || true
want "arcee has no cheap-model mapping"

# 5. Override supplies a model for the otherwise-unmapped provider.
TESTS=$((TESTS + 1))
LAST_OUT="$(SMOKE_MODEL_ARCEE=arcee-cheap bash "$SMOKE" --bin "$FAKE" --matrix 2>&1)" || { FAILED=$((FAILED + 1)); printf 'FAIL override run errored\n'; }
want "arcee -> arcee-cheap"

# A non-answering binary must fail within a deadline instead of hanging release.
touch "$WORK/stall"
SMOKE_STDIO_TIMEOUT=1 run_smoke 1 -- || true
want "did not shut down within the probe deadline"
rm "$WORK/stall"

# Ambient provider credentials must never reach the stdio probe.
OPENAI_API_KEY=non-secret-test-sentinel run_smoke 0 -- || true
want "healthz reports ok"

echo ""
if [[ "$FAILED" -eq 0 ]]; then
    printf '\033[1;32mapp-server-smoke.test.sh: all %s checks passed\033[0m\n' "$TESTS"
else
    printf '\033[1;31mapp-server-smoke.test.sh: %s/%s checks failed\033[0m\n' "$FAILED" "$TESTS"
    exit 1
fi
