#!/bin/sh
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cargo build --locked -p codewhale-cli --bin codewhale
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/codewhale"
WORK=$(mktemp -d)
mkdir -p "$WORK/home" "$WORK/project"
TOKEN=e2e-fixture-token
PORT=17979
export CODEWHALE_HOME="$WORK/home"
export CODEWHALE_ACP_FIXTURE_AGENT="$ROOT/crates/engine/tests/fixtures/acp-agent.mjs"
(cd "$WORK/project" && exec "$BIN" app-server --http --host 127.0.0.1 --port "$PORT" --auth-token "$TOKEN") >"$WORK/server.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT
api() { curl -fsS -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' "$@"; }
i=0
until api "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; do
  i=$((i+1)); [ $i -lt 120 ] || { cat "$WORK/server.log"; exit 1; }; sleep 0.5
done
echo "== GET /v1/agents/external (ids, runnable)"
api "http://127.0.0.1:$PORT/v1/agents/external" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{for(const a of JSON.parse(s).agents)console.log(a.id,a.state,"runnable="+a.runnable)})'
echo "== POST /v1/threads (executor fixture)"
THREAD=$(api -X POST "http://127.0.0.1:$PORT/v1/threads" -d "{\"workspace\":\"$WORK/project\",\"permission_posture\":\"ask\",\"executor\":{\"kind\":\"acp\",\"agent_id\":\"fixture\"}}")
echo "$THREAD"
TID=$(printf '%s' "$THREAD" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).id))')
echo "== POST turn"
api -X POST "http://127.0.0.1:$PORT/v1/threads/$TID/turns" -d '{"prompt":"hello from the e2e script"}'
echo
APPROVAL=""
i=0
while [ -z "$APPROVAL" ]; do
  i=$((i+1)); [ $i -lt 120 ] || { echo "no approval"; cat "$WORK/server.log"; exit 1; }
  sleep 0.25
  APPROVAL=$(api "http://127.0.0.1:$PORT/v1/threads/$TID" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const p=JSON.parse(s).pending_approvals||[];console.log(p.length?p[0].id:"")})')
done
echo "pending approval $APPROVAL; file exists before approval: $([ -e "$WORK/project/fixture.txt" ] && echo yes || echo no)"
[ ! -e "$WORK/project/fixture.txt" ]
echo "== POST /v1/approvals/$APPROVAL allow"
api -X POST "http://127.0.0.1:$PORT/v1/approvals/$APPROVAL" -d '{"decision":"allow"}'
echo
i=0
STATUS=in_progress
while [ "$STATUS" = in_progress ]; do
  i=$((i+1)); [ $i -lt 120 ] || { echo "turn never settled"; exit 1; }
  sleep 0.25
  STATUS=$(api "http://127.0.0.1:$PORT/v1/threads/$TID" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const t=JSON.parse(s).turns;console.log(t[t.length-1].status)})')
done
echo "turn status: $STATUS; file exists after approval: $([ -e "$WORK/project/fixture.txt" ] && echo yes || echo no)"
[ "$STATUS" = completed ]
[ -e "$WORK/project/fixture.txt" ]
echo "== thread detail (executor, items)"
api "http://127.0.0.1:$PORT/v1/threads/$TID" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const d=JSON.parse(s);console.log(JSON.stringify(d.thread.executor));for(const i of d.items)console.log(i.kind,i.status,JSON.stringify((i.detail||"").slice(0,60)))})'
echo "== event names (SSE replay)"
timeout 3 curl -sS -N -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT/v1/threads/$TID/events?since_seq=0" | grep '^event:' | sort | uniq -c || true
echo "== env reaching the agent has no CODEWHALE_ACP_FIXTURE_AGENT or token"
if grep -q -e CODEWHALE_ACP_FIXTURE_AGENT -e "$TOKEN" "$WORK/project/fixture-env.json"; then
  echo "fixture received a private runtime value"
  exit 1
fi
echo "PASS: external session, durable approval, write and credential isolation"
