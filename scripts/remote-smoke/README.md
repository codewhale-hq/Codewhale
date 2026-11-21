# Remote-workbench smoke lab (EXPERIMENTAL)

Status: experimental smoke-lab scripts for the US-first remote-workbench lane
(issue #1990). Not part of the supported install paths until the smoke passes
and this graduates into a documented setup.

This concretizes `docs/REMOTE_VM_US.md`: a cheap US VPS running the Codewhale
runtime on `127.0.0.1` plus the Telegram long-polling bridge, reusing the
provider-agnostic Ubuntu scripts under `scripts/tencent-lighthouse/` (audited:
nothing in them is Tencent-specific).

## Layout

- `setup-vm.sh` — provider-agnostic. Run on any fresh Ubuntu 24.04 VM:
  bootstrap + prebuilt release binaries (sha256-verified, no Rust
  build) + `gh` CLI + 4G swapfile + Telegram bridge services + secrets +
  validator + doctor.
- `digitalocean/provision.sh`, `digitalocean/teardown.sh` — active lane.
  Chosen over AWS Lightsail for auth simplicity: one API token vs IAM
  credential setup (#1990 allows "a clearly documented better alternative").
- `aws-lightsail/provision.sh`, `aws-lightsail/teardown.sh` — kept as the
  AWS alternative; same flow, needs `aws configure` first.
- `agent-session.sh` — sourceable helper for interactive/tmux agent sessions
  as the `codewhale` user.  Sources `/etc/codewhale/runtime.env` so the
  provider key is available outside of systemd.

Both provisioners print the API-reported monthly price and require a typed
`yes` before creating anything billable, and both teardowns end with a
leftover-billable-resources check.

## Who this lane is for (China note)

Telegram is blocked in mainland China and DigitalOcean has no China
datacenters (cross-border routes are slow; DO IP ranges are frequently
GFW-affected). Mainland-based users should prefer a regional host and chat
bridge approved by their organization. This lane is for users outside mainland
China.

## Security model

- Runtime API binds `127.0.0.1:7878` only; the only inbound port anywhere is
  SSH (cloud firewall + ufw, both default to caller-IP /32 where supported).
- Telegram uses outbound long polling — no webhook, no public ingress.
- Telegram chats are allowlisted (`TELEGRAM_CHAT_ALLOWLIST`); unlisted chats
  are refused. `TELEGRAM_ALLOW_UNLISTED=true` only for first pairing.
- Secrets travel as a chmod-600 file over scp, land in `/etc/codewhale/*.env`
  (0640 root:codewhale), and the transfer file is shredded. Never in argv,
  shell history, or logs.

## Run order — DigitalOcean (from the laptop)

```bash
# 0. once: create an API token (Web UI -> API -> Generate New Token, write
#    scope), then in a real terminal: doctl auth init   (paste token)

# 1. provision (asks before billing starts)
bash scripts/remote-smoke/digitalocean/provision.sh
# defaults: sfo3, s-1vcpu-2gb (~$12/mo), ubuntu-24-04-x64, ~/.ssh/id_ed25519.pub

# 2. secrets file (never commit; values from BotFather / provider console)
umask 077 && cat > /tmp/cw-secrets.env <<'EOF'
TELEGRAM_BOT_TOKEN=...
CODEWHALE_PROVIDER=deepseek
PROVIDER_KEY_NAME=DEEPSEEK_API_KEY
PROVIDER_KEY_VALUE=...
TELEGRAM_CHAT_ALLOWLIST=...   # optional; empty enables first-pairing mode
EOF

# 3. push secrets + installer, run it (DO Ubuntu images log in as root)
scp /tmp/cw-secrets.env scripts/remote-smoke/setup-vm.sh root@<IP>:/tmp/
rm /tmp/cw-secrets.env
ssh root@<IP> 'SECRETS_FILE=/tmp/cw-secrets.env bash /tmp/setup-vm.sh'

# 4. phone smoke per docs/REMOTE_VM_US.md "First Smoke Test"

# 5. teardown when done (stops billing)
bash scripts/remote-smoke/digitalocean/teardown.sh
```

For AWS Lightsail substitute step 0 with `aws configure`, step 1/5 with the
`aws-lightsail/` scripts, and ssh as `ubuntu@<IP>` with `sudo` in step 3.

## Cost

Billed hourly until destroyed. DO `s-1vcpu-2gb` ≈ $12/mo (~$0.018/h);
1 vCPU / 2 GB is enough because the VM downloads release binaries instead of
compiling Rust. A same-day smoke costs well under $1. Bigger options for a
longer-lived host: `s-2vcpu-2gb` (~$18/mo), `s-2vcpu-4gb` (~$24/mo, the
docs/REMOTE_VM_US.md default spec).

## Known sharp edges (from the 2026-06-09 audit)

- The Rust binary reads only `DEEPSEEK_RUNTIME_TOKEN`/`--auth-token` and
  `--port`; the `CODEWHALE_RUNTIME_*` names in `/etc/codewhale/runtime.env`
  work because the systemd unit expands them into flags. Don't start
  `codewhale serve` by hand and expect the env file to apply.
- `codewhale-runtime.service` hard-fails activation if
  `/home/codewhale/.codewhale` or `/home/codewhale/.deepseek` don't exist
  (`ReadWritePaths`); `setup-vm.sh` pre-creates them.
- Current installs place the byte-identical `codewhale` and `codew` commands
  side by side. The runtime is consolidated; no third `codewhale-tui` command
  is required.
- Exactly one bridge process per bot token — a second poller causes endless
  Telegram 409s. Stop any local bridge before starting the VM one.
- `/interrupt` is queued behind an active streaming turn (known limitation,
  documented in `docs/REMOTE_SETUP_DESIGN.md` hardening table).

## Autonomous agent loop (#3022)

Once the droplet is provisioned and `gh` is authenticated with a
fine-grained PAT (scoped to codewhale-hq/CodeWhale: Contents RW, Issues RW,
PRs RW, Metadata R), an agent can work the full pick→PR loop headless.

One-time git wiring after `gh auth login` so pushes use the PAT and
commits have a stable identity:

```bash
gh auth setup-git
git config --global user.name "whalebro-agent"
git config --global user.email "whalebro-agent@users.noreply.github.com"
```

```bash
# 1. Pick an issue a maintainer granted agent-ready
gh issue list --repo codewhale-hq/CodeWhale --label agent-ready --state open --json number,title,url

# 2. Claim it
gh issue edit <N> --add-label agent-in-progress --remove-label agent-ready

# 3. Isolate in a worktree
git -C /opt/whalebro/codewhale fetch origin
git -C /opt/whalebro/codewhale worktree add \
  /opt/whalebro/worktrees/issue-<N> -b agent/<N>-<slug> origin/main
cd /opt/whalebro/worktrees/issue-<N>

# 4. Execute (run inside a tmux session for SSH-disconnect safety)
. /opt/whalebro/codewhale/scripts/remote-smoke/agent-session.sh
gh issue view <N> --json body -q .body | \
  codewhale exec --auto --output-format stream-json "$(cat)"

# 5. Verify (run the issue's Verification block verbatim)
# 6. Deliver
gh pr create --repo codewhale-hq/CodeWhale --base main \
  --title "<title>" --body "Closes #<N>"

# 7. On blockage: swap label to needs-human + comment
gh issue edit <N> --add-label needs-human --remove-label agent-in-progress
```

Keep the same safety rules for any automated agent lane: PR-only delivery, no
force-push, secrets never in argv/history/logs, and one worktree per issue.
