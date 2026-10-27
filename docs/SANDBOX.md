# Sandbox threat model

> 阅读简体中文版：[zh_hans/SANDBOX.md](zh_hans/SANDBOX.md)。

Codewhale can launch shell commands proposed by a model. Approval policy,
workspace-aware tools, and an operating-system command wrapper are separate
controls: an approval is not a sandbox, and selecting `workspace-write` does
not prove that the current platform has an OS wrapper available.

This document describes only behavior wired into the command execution path.
See [Authorization order](AUTHORIZATION_ORDER.md) for the policy layers that
run before execution reaches this boundary.

## Platform overview

| Mechanism | Platform | Selection | What Codewhale reports |
|---|---|---|---|
| Seatbelt (`sandbox-exec`) | macOS | Automatic when the runtime probe succeeds | `macos-seatbelt` |
| Bubblewrap (`/usr/bin/bwrap`) | Linux | Default when installed and a wrapped probe run succeeds; `prefer_bwrap = false` opts out | `linux-bwrap` |
| No OS wrapper | Linux without usable bwrap, or opted out | `prefer_bwrap = false`, or bwrap absent/unusable | `none` |
| No OS wrapper | Windows | Current implementation | `none` |
| OpenSandbox-compatible service | Any supported host | `sandbox_backend = "opensandbox"` | External execution path |

The repository also contains a future Windows helper contract. It is not
wired into child-command launch, so Codewhale does not advertise it as an
active sandbox. Source-only sandbox code is not evidence that a command was
restricted.

## macOS: Seatbelt

Codewhale probes `/usr/bin/sandbox-exec` by running a minimal profile. When the
probe succeeds and the selected `SandboxPolicy` requests a sandbox, the child
command is wrapped with a generated Seatbelt profile.

The profile can provide:

- broad filesystem reads;
- writes limited by the selected policy, including the workspace and specific
  runtime/cache paths needed by supported tools;
- network access only when the policy enables it. A `read-only` policy with
  network (the agent `read_only: true` shell mode) gets outbound IPv4 and the
  system DNS resolver only. IPv6 and the host's own addresses, loopback
  included, are denied, except the port of a local proxy named in
  `HTTP_PROXY`, `HTTPS_PROXY` or `ALL_PROXY`, and other local sockets stay
  denied.

If the probe fails or `sandbox-exec` is unavailable, Codewhale reports no OS
sandbox and launches the command without a Seatbelt wrapper. It does not set a
Seatbelt marker on that fallback.

## Linux: default bubblewrap

Linux command sandboxing is on by default whenever bubblewrap works. Set the
top-level configuration key to opt out:

```toml
prefer_bwrap = false
```

Codewhale selects bubblewrap only when `/usr/bin/bwrap` is a regular
executable file AND a wrapped probe run proves it can create its namespaces
on this host — the exec bit alone lies where user namespaces are restricted
(e.g. Ubuntu 24.04 with `kernel.apparmor_restrict_unprivileged_userns`),
where every wrapped command would fail rather than run unsandboxed. The
wrapper derives its mounts and network namespace from the resolved
`SandboxPolicy`:

```text
/usr/bin/bwrap \
  --unshare-all \
  [--share-net] \
  --ro-bind / / \
  --dev /dev \
  --proc /proc \
  --tmpfs /tmp \
  [--dev-bind <device-root> <device-root> ...] \
  --bind <writable-root> <writable-root> ... \
  --ro-bind <protected-descendant> <protected-descendant> ... \
  [--ro-bind <extra-ro-root> <extra-ro-root> ...] \
  --chdir <cwd> \
  -- <program> <args>
```

The sandbox always gets a private `/dev` (fresh device nodes, so `>/dev/null`
works), a private `/proc`, and a tmpfs `/tmp` (#5410). Two optional top-level
config keys extend the mounts: `bwrap_ro_roots` (extra host paths bind-mounted
read-only, applied last so they can narrow a policy-writable path) and
`bwrap_dev_roots` (host character/block device nodes bind-mounted read-write;
directories are never honored). Missing paths are skipped silently.

That gives the child a read-only root view. For `workspace-write`, every safe,
existing policy root is mounted read-write: the working directory, configured
additional roots, `/tmp` and `TMPDIR` unless excluded, and verified Git
worktree metadata roots. Existing `.codewhale` and `.deepseek` descendants are
remounted read-only after their writable parent. Missing paths, non-directory
paths, and `/` are not promoted to writable mounts.

For `read-only`, there are no writable binds, so the working directory remains
inside the read-only root view. `--unshare-all` isolates the network namespace
by default. Codewhale adds `--share-net` only when the policy's
`network_access` is true. The agent `read_only: true` shell mode turns it on
only in Full Access (see [Subagents](SUBAGENTS.md)); it then shares the host
network namespace, including loopback and abstract unix sockets, like a
networked `workspace-write` shell. Bubblewrap does not filter either.
`danger-full-access` and `external-sandbox` bypass the local wrapper entirely.

If the user opts out, or `/usr/bin/bwrap` is missing, non-executable, or
cannot actually confine a child, Codewhale reports `none` and launches the
command without a Linux OS wrapper. There is no marker-only fallback to a
different Linux sandbox.

Install bubblewrap separately to get enforcement on Linux:

- Ubuntu/Debian: `apt install bubblewrap`
- Fedora: `dnf install bubblewrap`
- Arch: `pacman -S bubblewrap`

Codewhale does not vendor bubblewrap.

## Windows: no advertised OS sandbox

The Windows command path currently reports no OS sandbox. The source tree has
a future helper contract for Job Object process-tree cleanup, but it is not
wired into selection and must not be described as any of the following:

- read-only filesystem or workspace-write enforcement;
- network blocking;
- registry isolation;
- restricted-token or AppContainer isolation.

Windows host permissions and approval policy still apply, but they are not a
Codewhale OS command sandbox.

## Linux process hardening is not a command sandbox

At startup on Linux, Codewhale best-effort applies `PR_SET_DUMPABLE=0`,
`PR_SET_NO_NEW_PRIVS=1`, and `RLIMIT_CORE=0` to its own process. Each failure is
logged and startup continues. These controls reduce process-inspection,
privilege-escalation, and core-dump risk; they do not create filesystem or
network isolation for a child command and are not listed as a sandbox backend.

The one exception is the startup posture itself: when the startup sandbox mode
resolves to `danger-full-access` (via `CODEWHALE_SANDBOX_MODE` or the config
file's `sandbox_mode` key), `PR_SET_NO_NEW_PRIVS` is skipped so that
`sudo`/`su`/setuid helpers keep working from the agent shell (#5723) — "full
access" means it. Every narrower posture keeps the flag as defense-in-depth,
and `CODEWHALE_NO_NEW_PRIVS` overrides the posture in both directions
(#5413): a falsey value always skips the flag, a truthy value always sets it.
The flag is irreversible for the process tree, so the decision can only be
made at launch; per-call sandbox escalation inside a session cannot lift it.

## External OpenSandbox execution

When `sandbox_backend = "opensandbox"` is configured, shell execution is sent
to the configured OpenSandbox-compatible HTTP endpoint instead of starting a
local child. Codewhale validates the request/response contract, but isolation
guarantees belong to the configured service and its operator.

```toml
sandbox_backend = "opensandbox"
sandbox_url = "http://localhost:8080"
sandbox_api_key = "YOUR_API_KEY"
```

`sandbox_backend = "none"` (or omitting the key) keeps local execution.
Unsupported backend settings refuse shell execution; they never silently select
local execution. Choose a supported backend or explicitly select `none`.

## Policies and fallbacks

The local `sandbox_mode` values are:

```toml
sandbox_mode = "workspace-write" # read-only | workspace-write | danger-full-access | external-sandbox
```

- `read-only` and `workspace-write` are enforced by Seatbelt or bubblewrap only
  when that wrapper is selected and available.
- The configured value can only narrow the sandbox the permission mode
  picks: Full Access (`yolo`) runs without the wrapper, Ask and Auto-Review
  use `workspace-write`, and Plan uses `read-only`. A configured
  `danger-full-access` or `external-sandbox` therefore takes effect only in
  Full Access; it never loosens Ask or Auto-Review.
- `danger-full-access` deliberately bypasses the local OS wrapper. On Linux it
  also skips the `PR_SET_NO_NEW_PRIVS` process-hardening flag at startup so
  `sudo`/setuid workflows keep running (#5723); see the process-hardening
  section above.
- `external-sandbox` declares that execution is already externally isolated
  and bypasses a second local wrapper.
- When no wrapper is selected, the shell command runs without Codewhale OS
  isolation. Approval rules and workspace-aware native file tools remain
  separate controls.

Canonical environment overrides exist for `sandbox_mode` and the external
backend:

- `CODEWHALE_SANDBOX_MODE`
- `CODEWHALE_SANDBOX_BACKEND`
- `CODEWHALE_SANDBOX_URL`
- `CODEWHALE_SANDBOX_API_KEY`

`CODEWHALE_PREFER_BWRAP` (legacy alias `DEEPSEEK_PREFER_BWRAP`) overrides the
preference explicitly; the top-level `prefer_bwrap` config key is the durable
setting.

## Diagnostics and failure attribution

`codewhale setup --status`, `codewhale doctor`, `codewhale doctor --json`, and
the `diagnostics` tool report the locally available wrapper after applying the
resolved bubblewrap preference. An individual command can still bypass that
wrapper when its policy does not request sandboxing. On Linux, merely finding
a sandbox-related syscall or source module does not make `sandbox_available`
true.

Denial attribution is intentionally conservative:

- Seatbelt uses its wrapper-specific denial patterns.
- Bubblewrap setup errors must be prefixed by `bwrap:`; a read-only-filesystem
  error from the bwrap filesystem view can also identify the boundary.
- A child command's generic `Permission denied` or `Operation not permitted`
  is not, by itself, proof that Codewhale's sandbox blocked it.
- Unsandboxed command failures are never labeled sandbox denials.

## Limitations

- Availability is checked before launch; the selected wrapper can still fail
  because of host policy, container restrictions, or a race after the probe.
- Bubblewrap ignores a configured writable root if it is missing, is not a
  directory, or canonicalizes to `/`; a path can also disappear between policy
  resolution and wrapper launch.
- Seatbelt profiles are generated at runtime and must be tested against the
  commands they are expected to support.
- No current local wrapper is advertised on Windows.
- An external sandbox backend is only as strong as its configured service.
- No sandbox protects against kernel vulnerabilities or all resource-exhaustion
  and side-channel attacks.

## Implementation references

- `crates/sandbox/src/lib.rs` — truthful selection and public capability markers
- `crates/sandbox/src/seatbelt.rs` — macOS wrapper and availability probe
- `crates/sandbox/src/bwrap.rs` — Linux wrapper and functional availability probe
- `crates/sandbox/src/process_hardening.rs` — Linux parent-process hardening
- `crates/sandbox/src/backend.rs` — external backend selection
- `crates/tui/src/tools/diagnostics.rs` — machine-readable diagnostics
