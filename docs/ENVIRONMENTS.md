# Environment-specific caveats

> 阅读简体中文版：[zh_hans/ENVIRONMENTS.md](zh_hans/ENVIRONMENTS.md)。

Standard build/test/run commands live in `AGENTS.md` and `CONTRIBUTING.md`.
This file records only the non-obvious quirks of particular environments, so
they do not cost context on machines that will never hit them.

## Cursor Cloud VMs

- **System build dep:** the build needs `libdbus-1-dev` (pulled in by
  `crates/secrets` for the OS keyring). It is installed by the startup update
  script; if a `cargo build` fails with a `dbus`/`pkg-config` error, that dep is
  missing.
- **`rustup default` must be set:** some tests and runtime paths spawn shells in
  temp dirs *outside* this checkout (e.g. `run_verifiers_background_*`, sub-agent
  worktrees). Those spawned shells only see the repo's `rust-toolchain.toml`
  override while inside `/workspace`, so without a global default they fail with
  "rustup could not choose a version of rustc to run". The update script runs
  `rustup default stable` to fix this.
- **Known env-specific test failures at `/workspace` (not code bugs):** because
  the checkout sits directly under `/`, two `codewhale-tui` subagent tests fail
  here — `git_repo_root_reports_attempted_paths_when_no_repo_found` (cannot
  create a temp dir in the unwritable parent `/`) and
  `create_isolated_worktree_reports_friendly_error_when_no_repo_found` (walking
  up to `/` discovers `/workspace` itself as a repo). Both pass when the repo is
  checked out under a normal, writable parent.

## Running the agent without provider API keys

Point Codewhale at any local OpenAI-compatible endpoint via the keyless
`vllm`/`ollama`/`sglang` providers:

```sh
CODEWHALE_PROVIDER=vllm VLLM_BASE_URL=http://127.0.0.1:8000/v1 VLLM_MODEL=<id> \
  codewhale exec --auto "..."
```

`codewhale exec` (add `--auto` for tool use) is the non-interactive path to
exercise the full agent loop. `--auto` enables Full Access by default: no
Codewhale shell sandbox, with trust mode on. Add `--sandbox workspace-write`
to keep the shell sandbox and file-tool workspace restrictions, with trust
mode off.

## Keeping the host awake during a turn

While an interactive TUI turn is in flight, Codewhale holds the platform's
idle-sleep assertion, so an unattended machine does not idle into sleep
mid-turn and lose the work:

- macOS: `caffeinate -i`
- Linux: `systemd-inhibit --what=idle --why="Codewhale turn in flight" --mode=block cat`,
  where `cat` reads a pipe Codewhale holds for the turn

The assertion is released the moment the turn ends — on Linux by closing that
pipe, so `cat` exits and `systemd-inhibit` follows without leaving a process
behind — and it covers *idle* sleep only: an explicit `sleep` / `pmset sleepnow`, a closed lid, or a low battery
still suspends the machine. Headless hosts — `exec`, app-server, CI — never
hold it, so a shared runner's power policy is untouched. Windows is not
implemented: `SetThreadExecutionState` is thread-affine and needs a holder that
pins the thread, so the gap is deliberate rather than silent.

If a turn is suspended anyway, the engine notices on wake — wall-clock elapsed
diverging from monotonic elapsed by more than the suspend threshold — reports
`System sleep detected; connection lost — retrying request`, and re-issues the
request instead of failing the turn (#2990).

## Linux sandboxed terminals

Sandboxed PTYs require bubblewrap and util-linux `setsid` at `/usr/bin/setsid`
or `/bin/setsid`. Codewhale assigns the controlling terminal inside bubblewrap's
PID namespace so Ctrl-C reaches the foreground program without killing its
wrapper. A program that handles SIGINT can continue accepting input. Both
`exec_shell` PTY jobs and persistent terminal sessions use this path. Missing
`setsid` fails the PTY launch; it does not remove the sandbox. Non-PTY commands
do not require this helper. Namespace isolation, read-only mounts and
`--die-with-parent` cleanup remain in force.

## Windows support and release qualification

Windows compatibility depends on the runtime architecture and terminal. The
native x64 CLI/TUI compatibility target is Windows 10 1607 (build 14393) or
later in the legacy Windows console. A release's actual executable and installer
must still pass startup and installation on that target; an import-table pass
alone does not prove it. Required Visual C++ runtime DLLs must also be present.

| Surface | Compatibility target or platform requirement | Qualification boundary |
| --- | --- | --- |
| Native x64 CLI/TUI | Windows 10 x64 1607, build 14393 | Final release bytes need an actual oldest-OS startup receipt |
| ConPTY API | Windows 10 1809, build 17763; Windows Server 2019 | This is Microsoft's API minimum; Codewhale's terminal-session routes remain Unix-only and return 501 on Windows |
| Current Windows Terminal | Windows 10 2004, build 19041, or later | This terminal has a newer prerequisite than the ConPTY API; use the legacy console for the x64 compatibility target |
| Native ARM64 CLI/TUI | Windows 11 ARM64 is the target | Exports observed on build 26300 (debug candidate) and build 26200 (release build, hosted probe); no minimum Windows 11 build or Windows 10 ARM64 support is inferred from those runs |
| NSIS installer | x86 setup/plugins under WOW64, installing x64 runtime bytes | Native ARM64 installation uses its archive/npm assets; generated Uninstall.exe and actual install behavior need separate receipts |

[Microsoft's ConPTY requirements](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole)
and [current Terminal requirements](https://github.com/microsoft/terminal/blob/main/README.md)
are separate. Windows 11 supports x64 emulation on ARM64; an emulated x64 run
does not qualify native x64 execution or Windows 10 1607.

The release import guard uses separate per-machine profiles in
[`windows-import-floor.json`](../scripts/release/windows-import-floor.json).
Unknown import pairs, unlisted ordinals and delay-loaded dependencies stop the
gate. Its PASS covers the reviewed static import contract; it does not certify
all runtime features, the installed dependency closure or a minimum OS build.
The Native extension host still requires its runtime capability APIs and
isolation probes; where DeriveCapabilitySidsFromName is absent it reports
Unsupported rather than weakening its sandbox. This feature limit is distinct
from CLI startup compatibility.

When a candidate's windows-arm64 import check stops on pairs the arm64
profile has not seen, requalify it with
[`windows-arm64-export-probe.yml`](../.github/workflows/windows-arm64-export-probe.yml).
Dispatch it with the candidate run's `run_id` and `source_sha`. On a
GitHub-hosted Windows 11 ARM64 runner it loads each imported DLL from System32
in a native ARM64 process, resolves every pair with GetProcAddress, and
uploads a probe JSON with module paths and hashes. Add only probe-resolved
pairs to the profile, record the run, runner OS build, binary SHA256 and counts
in its qualification, and rerun the candidate. The probe resolves exports
statically and does not launch the executables.

### Recorded 0.10.2 candidate evidence

The 2026-10-13 qualification snapshot is source
[`cf7199b52e`](https://github.com/codewhale-hq/Codewhale/commit/cf7199b52ecb9147e4670ae7874924ffc30a6a67).
A default-feature debug ARM64 build on Windows 11 build 26300 produced executable
SHA256 `ca51f91562a0569ee43855715856e1cfee74e8a834bd6206183a3175373fdb2d`.
All 453 imported symbols resolved in a native ARM64 process using system-only
DLL lookup. The ordinary-user candidate checks passed 89 classifier tests and
one owner-crash recovery test, plus scoped editor wheel, Pinyin and undo checks.
This is neither a dist-profile release artifact nor installer acceptance, and
it establishes no oldest supported Windows 11 build.

The x64 profile comes from @LmeSzinc's
[#6953 export investigation](https://github.com/codewhale-hq/Codewhale/issues/6953)
on build 14393.447: all 411 v0.10.0 imports and 453 of 454 v0.10.1 imports resolved.
The missing DeriveCapabilitySidsFromName pair is excluded; the conservative
allowed union has 456 pairs. The x86 NSIS profile has 189 named Microsoft API
contracts predating 1607 and one explicitly identified ordinal: COMCTL32 #17 is
InitCommonControls in the x86 Windows SDK 10.0.26100.0 import library and stock
SysWOW64 DLL. Its [documented minimum is Vista](https://learn.microsoft.com/en-us/windows/win32/api/commctrl/nf-commctrl-initcommoncontrols).
The manifest retains official source URLs, image hashes, counts and the ordinal
identity; these qualify static contracts without claiming a 1607 installer run.

On 2026-10-16, probe run 38890634267 (Windows 11 ARM64 build 26200.9457)
resolved all 445 imports of the release-build windows-arm64 executables from
candidate run 38867608774, and the 7 pairs the 2026-10-13 profile had not seen
were added. Release candidate 38899652987 on the frozen 0.10.2 source
`14683c9983` then passed the import check on every Windows target: 445 imports
on ARM64, 453 on x64 including the installer payload, and 163 for the x86
setup.

At this snapshot, startup/installation of the final release bytes on an actual
1607 machine and the candidate's generated uninstaller were still unqualified.
The final frozen source, release import dumps, archive/native/npm installation,
packaged/native-app owner recovery and oldest-OS receipts belong in the release
acceptance packet. See [the release checklist](RELEASE_CHECKLIST.md).

## Windows PowerShell execution policy

The shell tool runs PowerShell with `-ExecutionPolicy Bypass`. That sets only
the policy of the child process it launches: nothing is persisted, no
administrator rights are needed, and your own PowerShell windows keep their
policy. Without it, a machine whose local policy is `Restricted` (the Windows
client default) or `AllSigned` refuses the temporary `.ps1` script Codewhale
writes for multiline commands (#6745).

A policy set by Group Policy (the `MachinePolicy` or `UserPolicy` rows of
`Get-ExecutionPolicy -List`) outranks the process scope. On such a machine,
multiline commands are still refused and PowerShell's refusal is returned as
the command's error; Codewhale does not work around an administrator-enforced
policy. Single-line commands run through `-Command`, which the execution
policy does not govern. Scripts that a command itself calls run under the same
process scope; the shell tool's approval and sandbox settings, not the
execution policy, decide what may run.

To let the machine or user policy apply instead, set
`CODEWHALE_POWERSHELL_EXECUTION_POLICY=inherit` before starting Codewhale: the
shell tool then omits `-ExecutionPolicy` entirely, so a `Restricted` or
`AllSigned` policy refuses multiline commands that need the temporary `.ps1`
script. Unset, `bypass`, or any other value keeps the default `Bypass`.

## Consolidated runtime commands

The current `codewhale` binary runs the TUI in-process. Release installers copy
the same bytes to the optional `codew` short command; no sibling
`codewhale-tui` executable is required. `DEEPSEEK_TUI_BIN` remains a legacy
replay/migration setting, not a current install requirement.

## Environment-aware Fleet placement (preview)

A Fleet task can declare where it is able to run. The manager and scheduler
use the same placement decision when choosing a worker. Requirements
are hard constraints; `prefer` ranks only eligible environments. A busy or
ineligible worker does not prevent later queued tasks from using another eligible
worker. `max_workers` limits concurrency across the whole roster, rather than
restricting selection to the first entries.

The runtime accepts `workspace.placement` on a task and `environment` on a
configured worker. The same task contract flows into the existing subagent
launch manifest, and the selected environment becomes recorded task context.
This uses the existing Engine, Fleet ledger, permission checks and executor.
An explicit environment kind describes the operator's environment independently
of its transport: an Engine inside a cloud VM can run a `local` subprocess in
an environment declared `cloud`. Explicit environment IDs cannot name different hosts
within one document. Use explicit IDs when a task must stay on a specific host.

| Task field | Meaning |
| --- | --- |
| `environments` | Allowed environment IDs; empty permits any configured candidate |
| `kinds` | Allowed `local`, `remote` or `cloud` kinds |
| `capabilities` | Every named capability must be declared by the environment |
| `data` | Every named data handle must already be declared available there |
| `resources.cpu_cores`, `memory_mib`, `gpus` | Minimum declared capacity; missing capacity fails the requirement |
| `persistent` | Require persistent or temporary storage; unknown fails either explicit requirement |
| `max_cost_per_hour_microusd` | Upper bound on a declared hourly compute rate; unknown fails the requirement |
| `prefer` | Ordered preferred environment IDs, after all requirements pass |

These declarations select among configured execution targets. They grant no
tools, network access, secrets, filesystem access, cloud allocation or spend.
They are operator declarations, not live hardware measurements, resource
reservations, OS quotas or billing quotes. Data handles do not copy files or
credentials. A remote working directory must already contain the intended data.
Hourly rate matching does not replace task budgets or the platform's spend gate.

For CLI invocation, use `codewhale fleet run tasks.json --require-placement`
(and add `--check` to validate without creating a run). Placement is always
enforced by supporting Engines; the flag makes older binaries refuse the
invocation instead of silently ignoring new fields. API clients must check
`GET /v1/targets` for `placement.version == 1` before relying on these semantics.
Do not send placement-constrained specs to an older Engine without that gate.

Specs without placement constraints retain their existing worker order among
supported available hosts. If no configured environment can satisfy a new run,
creation fails before recording the run or launching a worker. `fleet run --check`
also avoids creating the ledger.
The selected worker is rechecked when its launch manifest is prepared. Busy
eligible workers leave work queued. Placement never silently relaxes a hard
requirement or provisions a different machine.

### Preview and receipts

`POST /v1/fleet/placement` accepts the same JSON document as `fleet run` and
returns `execution: "preview"`, `grants_execution: false`, and one decision per
task. Preview requests are limited to 256 KiB and 64 tasks. Each decision
contains the selected worker/environment, the selection reason and typed
refusals for every rejected candidate. It has no live load,
connectivity or permission probe and creates nothing. An omitted worker list
previews this computer with unknown resources.

Each initial lease publishes a checksummed `environment_placement` JSON receipt
through the existing Fleet artifact stream. It includes the run, attempt,
candidates, requirements represented by the persisted task spec, and the
selected worker. Restart attempts retain that environment and recheck the
selected worker before dispatch; recovery does not silently move the task.
The evidence is marked `operator_declared`. The Runtime's existing worker,
artifact and event endpoints expose the receipt.

### Tailscale and WhaleNet

`GET /v1/tailnet` reads the already-connected local Tailscale client. It returns
node IDs, names, OS, addresses and online status without enrolling devices or
altering tailnet policy. The request has a five-second deadline and bounded
output. Missing or disconnected Tailscale returns `tailnet_unavailable`.
`GET /v1/targets` advertises both the inventory and placement preview routes.
The routes use the Runtime API's existing authentication boundary.

An operator-authored Fleet worker can use this host declaration:

```json
{
  "id": "builder-slot",
  "name": "Private builder",
  "host": {
    "kind": "tailscale",
    "node_id": "NODE_ID_FROM_INVENTORY",
    "user": "worker",
    "working_directory": "/srv/example",
    "codewhale_binary": "/usr/local/bin/codewhale"
  },
  "environment": {
    "id": "builder",
    "kind": "cloud",
    "resources": { "cpu_cores": 8, "memory_mib": 32768, "gpus": 0 },
    "capabilities": ["rust"],
    "data": ["repo:example"],
    "persistent": true,
    "cost_per_hour_microusd": 2000000
  }
}
```

The corresponding task can require:

```json
{
  "id": "review",
  "name": "Review the source",
  "instructions": "Inspect the repository and return concrete findings. Do not change files.",
  "worker": { "role": "reviewer" },
  "workspace": {
    "placement": {
      "kinds": ["local", "cloud"],
      "capabilities": ["rust"],
      "data": ["repo:example"],
      "resources": { "memory_mib": 8192 },
      "max_cost_per_hour_microusd": 3000000,
      "prefer": ["builder"]
    }
  },
  "budget": { "max_steps": 20, "max_seconds": 600 }
}
```

The rate and hardware above are example declarations, not an offer. Put workers
and tasks in the document's `workers` and `tasks` arrays. A local candidate
declaring only 4096 MiB would be refused for this task even if listed first.

Before each dispatch batch, Codewhale refreshes inventory on the blocking pool,
so a slow Tailscale client does not block the async runtime. At launch it resolves
the exact node ID again from that snapshot and refuses snapshots older than 30
seconds. It requires an online
peer advertising Tailscale SSH host keys, pins those keys in a private temporary
known-hosts file, and uses OpenSSH through `tailscale nc`. It disables SSH
configuration aliases and alternative host-key trust sources for that transport.
An absent node, missing keys or stale name never falls back to another address
or ordinary public SSH. The destination still enforces its tailnet and SSH
rules. A checksummed node-identity receipt records the selected node, address,
OS user and check time before SSH starts; it explicitly does not claim that
authentication or the task succeeded. The normal worker result supplies that
later evidence. In-place Tailscale host restarts are refused; a fresh Fleet
attempt performs a new identity check.

This works with an enrolled Tailscale-compatible network, including WhaleNet's
existing network foundation when its node advertises Tailscale SSH. Existing
SSH remote-process cleanup limitations still apply: a lost local connection
does not prove the remote process stopped. Remote tasks should remain bounded.

This preview does not enroll an agent as a separate node, implement the app's
per-machine consent flow, allocate Boat computers, sync workspaces, or enable
remote custom workers in the managed Fleet API. Those authority and lifecycle
paths remain owned by their existing runtime/platform surfaces. It is the
placement and connectivity foundation for that integration. ShannonNet stays
separate and deferred.

Transport references: [Tailscale SSH](https://tailscale.com/docs/features/tailscale-ssh),
[Tailscale CLI](https://tailscale.com/docs/reference/tailscale-cli), and the
[upstream SSH wrapper](https://github.com/tailscale/tailscale/blob/main/cmd/tailscale/cli/ssh.go).
