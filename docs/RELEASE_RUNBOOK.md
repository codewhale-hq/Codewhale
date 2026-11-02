# Codewhale Release Runbook

This runbook is the source of truth for shipping Rust crates, GitHub release assets,
and the `codewhale` npm wrapper.

Use [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md) as the ordered release
sequence: reconcile the canonical checkout, prepare and land verified slices,
freeze one SHA, verify Cargo tarballs and exact-SHA hosted CI/RC, obtain Hunter's
publication approval, tag that SHA, then verify each public destination.
Agent-owned work stays in `/Volumes/VIXinSSD/CW/codewhale` on `main`; do not
create release branches, worktrees or nested clones. Contributor PRs still land
as PRs. Coordinate file ownership and preserve dirty work before any recovery.
Keep evidence in the existing release packet instead of maintaining a second
release queue here.

GitHub assets are uploaded into a draft and published only after every expected
asset's name, upload state, size and GitHub SHA-256 digest match the verified
local set. A failed upload leaves an unpublished draft. Reruns may reuse matching
draft assets; missing or mismatched digests stop publication for maintainer
review. Public assets are never replaced. Draft lookup checks every release page.
CNB release tags and GHCR version/latest tags follow canonical publication;
manual CNB tag recovery also requires a complete public GitHub asset inventory.

Current packaging note:
- `codewhale-tui` is the live runtime crate linked into the installed
  `codewhale`/`codew` commands; it is not a third installed command.
- `codewhale-app-server` is a supporting library crate. The shipped entrypoint
  is `codewhale app-server`; do not add or publish a standalone app-server binary.

## Canonical Publish Targets

- End-user crates:
  - `codewhale-tui`
  - `codewhale-cli`

The complete supporting-crate list and dependency order live in
[`scripts/release/crates.sh`](../scripts/release/crates.sh), validated against
the locked workspace graph by the publish helper. Do not maintain a second
handwritten list here.

## Version Coordination

- Rust crates inherit the shared workspace version from [Cargo.toml](../Cargo.toml).
- Internal path dependency versions should match the shared workspace version; stale older pins are release blockers once the workspace version moves.
- The npm wrapper version lives in [npm/codewhale/package.json](../npm/codewhale/package.json).
- `codewhaleBinaryVersion` controls which GitHub release binaries the npm wrapper downloads.
- Packaging-only npm releases are allowed:
  - bump the npm package version
  - leave `codewhaleBinaryVersion` pinned to the previously released Rust binaries
  - rerun `npm pack` smoke checks before `npm publish`

## Release Source Timing

Freeze the source before creating a public `vX.Y.Z` tag. A version bump is
preparation, not a release: all intended source must reach `main` before the
final candidate is chosen. Record the full SHA and coordinate a pause with
shared writers through packaging and publication. Clean local `HEAD`, `main`
and freshly fetched `origin/main` must agree. A later fix changes the candidate
and needs affected proofs replaced. Once created, the tag is the immutable
release anchor; later main commits cannot change its contents.

Before tagging, verify the live queue and existing anchors:

```bash
gh issue list --repo codewhale-hq/CodeWhale --milestone "vX.Y.Z" --state open
gh pr list --repo codewhale-hq/CodeWhale --state open --limit 100
git ls-remote origin refs/heads/main refs/tags/vX.Y.Z
gh release view vX.Y.Z --repo codewhale-hq/CodeWhale
./scripts/release/check-published.sh X.Y.Z
```

If a same-version tag already exists but there is no GitHub Release and nothing
is published, stop and choose deliberately:

- publish exactly the tagged SHA, leaving later commits for the next patch;
- bump the later work to the next patch version and tag that later SHA; or
- with explicit maintainer approval only, delete/recreate the unpublished tag
  after confirming no package, GitHub Release, mirror, or installer consumer has
  treated it as public.

Do not delete, move, or recreate a release tag implicitly during ordinary
preparation or milestone cleanup. Publishing an older tagged SHA after canonical
`main` has advanced needs a separately authorized recovery plan; do not switch
the shared checkout or create another lane to bypass the source guard.

## Preflight

For each preparation slice, run the existing checks appropriate to its changed
surface once, then record the actual result in the commit message and land owned
paths directly on `main`. Batch coherent Rust edits before compiling. Web changes
use `npm test && npm run check:web` from the repository root. Do not repeat a
passed check merely to commit/push or run exhaustive local workspace builds in
addition to the final hosted gates.

Before freezing the candidate, use the lightweight preparation checks:

```bash
./scripts/release/check-versions.sh --require-dated-release
./scripts/release/check-ohos-deps.sh
./scripts/release/check-feature-release-notes.sh vPREV HEAD
python3 scripts/check-contributor-credit.py
```

`check-versions.sh` also runs in CI on every push/PR (the `versions` job in
`.github/workflows/ci.yml`), so drift between `Cargo.toml`, the per-crate
manifests, the npm wrapper and Runtime SDK, the VS Code extension and lock,
generated web facts, and `Cargo.lock` is caught before release time rather than
at it.

Ordinary main-push GitHub CI can delegate heavy Linux checks to CNB and omit
some smoke coverage. A successful pointer job is not a Linux test receipt.
The final manual full-CI dispatch forces the real Linux, macOS and Windows
gates; keep its exact-SHA evidence rather than substituting an older CNB run.

`publish-crates.sh` requires Cargo 1.90 or newer for multi-package verification;
this release-tool requirement is separate from the workspace's Rust 1.89 MSRV.
Use a release toolchain meeting that requirement.

Run the package preflight once for the clean frozen candidate and retain its
SHA/result:

```bash
./scripts/release/publish-crates.sh dry-run
```

Both modes validate publication order against the locked workspace graph, then
run one `cargo publish --dry-run --locked --registry crates-io` covering all
release crates listed in `scripts/release/crates.sh`. Cargo resolves unpublished
workspace dependencies through a
temporary local registry, builds every unpacked tarball, and checks publication
metadata and real tarball sizes before any upload. Dry-run mode can permit
source edits, but the final release receipt must describe the frozen source.
It stops without uploading. CI/RC does not verify Cargo publication tarballs
and cannot replace this gate. Publish mode necessarily verifies them again.

Cargo treats every crate it serves from that temporary registry as immutable by
name and version, so a later run at the same version can verify an earlier
run's code. It skips extracting a crate under `$CARGO_HOME/registry/src/` once
`.cargo-ok` exists, and it keeps compiled units of those crates in the build
directory because they track no source files. At 0.10.2 one dry run failed with
`E0432 unresolved import anyhow` in codewhale-core from an older extraction;
after that extraction was moved aside, the next run passed while linking
codewhale-state and codewhale-config built from older sources. Before
packaging, both modes therefore refuse to start when they find a release
crate's `<crate>-<version>` directory under any `$CARGO_HOME/registry/src/*`
other than crates.io's `index.crates.io-*`, or a dep-info file under the build
directory's `*/deps/` that names such a directory. The refusal lists what it
found and deletes nothing; removing shared cache directories still needs
explicit cleanup approval. It prints, with the paths filled in, the kind of
rerun that produced the 0.10.2 receipt at 14683c9983 on 2026-10-16: a new,
empty `CARGO_HOME` and build directory.

```bash
mode=dry-run
cache="${CODEWHALE_CACHE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/codewhale}"
lock="${CODEWHALE_BUILD_LOCK_FILE:-${cache}/build.lock}"
root="${cache}/release-verify-$(git rev-parse --short=10 HEAD)-${mode}-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "${cache}" && mkdir "${root}" &&
  CARGO_HOME="${root}/cargo-home" CARGO_TARGET_DIR="${root}/target" \
  python3 scripts/build-lock.py "${lock}" -- ./scripts/release/publish-crates.sh "${mode}"
```

That run was a cold build of about 19 GB and 962 compile steps, so check
`df -h` first. `cache` and `lock` use the same fallbacks as
`scripts/dev-cache.sh`. `CARGO_TARGET_DIR` moves the build directory with
plain Cargo and with `scripts/dev-cargo.sh`, which honors an inherited target
directory, and `scripts/build-lock.py` holds the machine build lock whichever
`cargo` is on `PATH`. That lock serializes only with `scripts/dev-cargo.sh`
and other `build-lock.py` users; a `cargo` wrapper with its own build slots
does not take it. The 0.10.2 receipt run set `CODEWHALE_CACHE_ROOT`
instead, which only `scripts/dev-cargo.sh` reads, and kept its lock inside the
new root. `mkdir` without `-p` refuses a root that already exists, and the
`&&` chain stops the run there. Every run leaves state that the next run at
the same version refuses, so the root name carries the SHA, the mode and a
UTC timestamp:
publish mode (step 5 of the Rust crates release) and each partial-publish
rerun get their own root. Pass the crates.io token to publish mode as
`CARGO_REGISTRY_TOKEN` for that one command, as step 5 shows; Cargo then
writes no `credentials.toml` under the root. If you run `cargo login` with
that `CARGO_HOME` instead, run `CARGO_HOME="${root}/cargo-home" cargo logout`
once the publish finishes, so no plaintext token copy stays in the cache tree.
The CNB `work/v*` release preflight in `.cnb.yml` calls this script in a fresh
container, where the check finds nothing. Known limits: the check also refuses
compiled units recorded against a different `CARGO_HOME`, which Cargo would
rebuild rather than reuse, and it does not scan `<target-triple>/`
subdirectories of the build directory, which host-only package verification
does not use.

If a dry-run is interrupted (Ctrl-C, or a shell that gets killed), inspect the
half-packed `package/` directory in Cargo's build directory before retrying and
coordinate with active builds. For a run from the blocks above that is
`${root}/target/package` with any `cargo`. Otherwise plain Cargo uses
`target/package` and `scripts/dev-cargo.sh` moves it under
`${CODEWHALE_CACHE_ROOT}/build/`; `cargo metadata --format-version 1
--no-deps` reports it as `build_directory`.
Cargo can resume against it and every unpacked tarball may then fail with
`error: Current directory is invalid:
No such file or directory (os error 2)` — which looks like a compiler or
workspace defect and is neither (seen 2026-09-21; a clean re-run passed).
Keep recovery material and obtain explicit cleanup approval before deleting
shared build/package directories; do not reset the source to repair packaging.
Publish mode requires the approved release checkout and assets, then skips
versions already on crates.io and uploads the remaining crates in dependency
order. Resuming still verifies the complete source release; it never weakens
the artifact gate merely because an earlier crate was already uploaded.
Registry-side acceptance and credentials are still checked during real upload;
a successful preflight cannot guarantee that every later upload will succeed.

The final full-CI and RC workflows own npm wrapper build/install smoke. When a
wrapper change needs focused local verification, reuse a valid matching runtime
or build the single runtime once and run the cross-platform smoke harness.
This packs the npm wrapper, installs it into a
clean temporary project, serves local release assets over HTTP, and checks both
published commands against that runtime: `codewhale doctor --help` and
`codew --version`.

```bash
cargo build --release --locked -p codewhale-cli --bin codewhale
node scripts/release/npm-wrapper-smoke.js
```

Set `DEEPSEEK_TUI_KEEP_SMOKE_DIR=1` to keep the temporary pack/install
directory for inspection.

## Exact-head GitHub proof before publication

Two manual workflows provide exact-head evidence without crossing the public
release boundary. Run them after the intended source is frozen on canonical
`main`, and pass the full commit SHA as an independent
guard against that ref moving between inspection and dispatch:

```bash
git fetch origin main
candidate_sha="$(git rev-parse HEAD)"

gh workflow run ci.yml --ref main \
  -f expected_sha="${candidate_sha}"
gh workflow run release-candidate.yml --ref main \
  -f expected_sha="${candidate_sha}"
```

The manual `ci.yml` path verifies that the dispatch resolved to
`expected_sha`, disables light-change shortcuts, and forces the heavy Rust,
workflow, mobile, Actions, Linux, macOS, Windows, npm-wrapper, and documentation
gates. A mismatch fails before those gates start; it never silently tests a
different head.

Verify beforehand that clean local `HEAD`, `main` and `origin/main` agree.
Record both run URLs, resolved SHAs and required job results; inspect executed
gates rather than the workflow badge. If `main` changes before tagging, choose
the new candidate deliberately and replace affected evidence. Do not run these
exhaustive gates locally as well.

`release-candidate.yml` also fails unless the selected ref resolves to the
exact requested SHA. It runs the same parity gate as the public release
(`release-parity.yml`: fmt, check, clippy, workspace nextest, doctests,
protocol and state parity), and `release.yml` refuses to start unless a green
release-candidate run with a green Parity job exists for the exact tag SHA
(`scripts/release/require-rc-receipt.sh`). Tag the SHA the final full CI and RC
validated. A missing receipt stops publication; do not move the tag to conceal
it or use a different SHA's green run.
It invokes the same reusable artifact workflow as the
public release, building all eight targets (including Android arm64, native
Windows arm64 and native HarmonyOS arm64), staging `codewhale` and `codew`
(single binary), building the NSIS installer and ten platform archives, and
validating the authoritative
38-file inventory from `npm/codewhale/scripts/artifacts.js` (30 current
artifacts and manifests plus eight compatibility-only `codewhale-tui-*`
filenames containing the same compiled `codewhale` bytes for v0.9.4 update
clients). The HarmonyOS leg is the one target whose bytes are post-processed:
the binary is self-signed before staging, because a device refuses to execute
an unsigned ELF. It then installs
the packed npm wrapper against those assembled local assets and exercises its
delegated entrypoints. The resulting `codewhale-release-assets` bundle is a
short-lived GitHub Actions artifact only.
Retain required asset/evidence copies before artifact expiry; record cross-build,
install and real-device/provider results separately in the existing packet.

Windows import evidence uses independent profiles for the native x64
compatibility target, observed ARM64 environment and x86 NSIS/WOW64 contracts;
see [ENVIRONMENTS.md](ENVIRONMENTS.md#windows-support-and-release-qualification).
Keep every staged Windows executable and the actual embedded NSIS PE plugin
dumps, hashes and failures. The generated Uninstall.exe needs its own scoped
installation receipt. Static import qualification does not prove final release
startup on Windows 10 1607, installed runtime dependencies or native ARM64
package/desktop behavior. Preserve these runtime receipts separately before
claiming the release is ready.

This candidate workflow does not create a tag or GitHub Release, publish a
crate or npm package, push a container, update Homebrew, deploy anything, or
write repository contents. Its green result is evidence, not publication
authorization. The stop line remains explicit Hunter approval: do not create
the `vX.Y.Z` tag, dispatch `release.yml`, or run any registry publication step
until that approval is given for the exact version/SHA and intended destinations.
Tag creation can immediately start GitHub, npm, GHCR, CNB and Homebrew
publication; include those destinations in the approval. Cargo publication and
any deployment/downstream launch also require their applicable authorization.

The Android target is cross-built and included in the checksum/bundle gates,
but GitHub's Linux runner cannot execute the Android binary as a real Termux
user. Keep the real-device limitation in the release packet unless separate
device evidence exists.

To exercise `npm run release:check` locally as well, regenerate the local asset
directory with a full asset matrix fixture before starting the server:

```bash
DEEPSEEK_TUI_PREPARE_ALL_ASSETS=1 node scripts/release/prepare-local-release-assets.js
cd npm/codewhale
DEEPSEEK_TUI_VERSION=X.Y.Z DEEPSEEK_TUI_RELEASE_BASE_URL=http://127.0.0.1:8123/ npm run release:check
```

Set `DEEPSEEK_TUI_VERSION` to the npm package version you are verifying for that local run.

CNB can supply a separate Linux tarball/install receipt. The final manual
full-CI run supplies hosted Linux, macOS and Windows wrapper smoke. Neither
proves installation from a subsequently published registry or a real provider
turn; retain those separate acceptance receipts where required.

After publishing, prove the release is visible in both registries:

```bash
./scripts/release/check-published.sh X.Y.Z
```

Do not mark a Rust release complete until that command sees `codewhale@X.Y.Z`
on npm and every `codewhale-*` crate at `X.Y.Z` on crates.io. For a rare
npm packaging-only release, run with `--allow-npm-binary-mismatch` and keep the
release notes explicit that no new Rust binary version shipped.

## Historical Branch and Checkout Inventory

No new agent release/integration branch is part of this process. For historical
branches, the existing helper can produce a read-only report against `main`:

```bash
./scripts/release/branch-hygiene.sh --release-branch main
```

The default mode is a dry run. It reports the current checkout branch, main ref,
local and remote release tips, safe local or remote branch deletes, branches
kept for contributor work, and branches that still need a human decision. A
safe-delete label is not cleanup authority: removal of any branch or checkout
needs explicit Hunter approval for the named targets. Preserve unique commits,
dirty/untracked work and contributor credit before an approved cleanup. Cleanup
does not block a release once its canonical source is reconciled.

Use `--remote upstream` when you are working from a fork and the canonical
release refs live on the upstream remote instead of `origin`.

Report canonical path, branch, SHA, upstream state and unresolved work at
handoff. Keep backups as recovery artifacts, not alternate development lanes.

## Rust Crates Release

Crate publishing to crates.io is **manual** — there is no automated
`crates-publish` GitHub workflow. Operators run the helpers in
`scripts/release/` from a developer workstation, passing a crates.io publish
token to the publish command as `CARGO_REGISTRY_TOKEN` (step 5).

Release preparation lands directly on canonical `main` in verified slices;
contributor PRs retain their normal review/merge process. The source must be on
`main` before any approved `vX.Y.Z` tag is pushed. The tag release workflow runs
`scripts/release/ensure-release-on-main.sh` for tag pushes and manual dispatches,
and fails branch-only release sources before assets are published.

1. Write the CHANGELOG entry, then run
   `./scripts/release/prepare-release.sh X.Y.Z` — it bumps every
   version-bearing file (workspace + crate pins + npm wrapper + Runtime SDK +
   VS Code extension/lock + remote-smoke default + public source-candidate
   facts + README install tags), refreshes the Cargo/npm locks and generated
   files, and runs
   the version and OHOS gates. It is safe to rerun after the workspace already
   equals `X.Y.Z`: the second run skips replacements, refreshes the packaged
   changelog and web facts, and reruns both gates.
2. Freeze clean canonical `main` at the intended SHA, run
   `./scripts/release/publish-crates.sh dry-run`, and complete the exact-SHA
   manual full-CI/RC and version-specific acceptance gates above. Retain the
   receipts rather than repeating unchanged local checks.
3. Obtain Hunter's explicit publication approval, then create `vX.Y.Z`
   from `main` with the manual **Create release tag** workflow or with a signed
   local tag push from a developer machine.
   - If `RELEASE_TAG_PAT` is configured, the tag push starts `release.yml`.
   - If no Release run appears and the tag already exists, first confirm that
     no tag-triggered run is queued or active, then dispatch the exact tag:
     `gh workflow run release.yml --ref vX.Y.Z -f version=X.Y.Z`.
   - Never dispatch from `main`, and do not start a duplicate while the
     tag-triggered run is merely delayed. The workflow serializes runs for the
     same tag. It also refuses to start release work when that tag already owns
     any GitHub Release asset, rechecks immediately before upload, and disables
     the release action's overwrite behavior. A normal rerun must never replace
     public bytes.
4. Wait for the GitHub Release workflow and all public assets to finish, then
   fetch the release tag and run the public asset gate. Do not publish any
   Cargo or npm package until it passes:

   ```bash
   git fetch origin tag vX.Y.Z
   ./scripts/release/verify-release-assets.sh X.Y.Z
   ```

5. Publish approved Rust crates from the same clean canonical checkout on
   `main`, with `HEAD` still matching the immutable remote release tag:

   ```bash
   ./scripts/release/require-release-tag-checkout.sh X.Y.Z && {
     mode=publish
     cache="${CODEWHALE_CACHE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/codewhale}"
     lock="${CODEWHALE_BUILD_LOCK_FILE:-${cache}/build.lock}"
     root="${cache}/release-verify-$(git rev-parse --short=10 HEAD)-${mode}-$(date -u +%Y%m%dT%H%M%SZ)"
     token=; while [ -z "${token}" ]; do printf 'crates.io token: ' >&2; read -rs token || break; echo >&2; done
     [ -n "${token}" ] && mkdir -p "${cache}" && mkdir "${root}" &&
       CARGO_REGISTRY_TOKEN="${token}" CARGO_HOME="${root}/cargo-home" \
       CARGO_TARGET_DIR="${root}/target" \
       python3 scripts/build-lock.py "${lock}" -- ./scripts/release/publish-crates.sh "${mode}"
     unset token
   }
   ```

   Both Cargo and npm publication fail closed unless `HEAD`, the clean local
   checkout, and the remote `vX.Y.Z` tag still agree. The authoritative crate
   dependency order lives in `scripts/release/crates.sh`; do not maintain a
   second handwritten order in this runbook. The helper waits for each new
   version to appear on crates.io before moving to dependents and safely skips
   versions that are already public on a rerun. The block runs it from a new,
   empty `CARGO_HOME` and build directory of its own, as the package preflight
   above describes; the dry run's state makes it refuse otherwise. The braces
   make the shell read the whole block before `read` runs, so a pasted or
   piped block never takes its own next line as the token, and the prompt
   repeats on an empty line, so a stray blank line from a paste is not
   taken as the token. `read -rs` keeps the token out of shell history,
   end of input stops the block, and the environment variable keeps the
   token out of `credentials.toml`. The tag check is chained with `&&`, so a
   failed check asks for no token.
   If shared `main` advances or the checkout becomes dirty, stop and coordinate
   a new candidate or separately authorized recovery with Hunter. Do not switch
   branches, create another checkout, force-fetch a conflicting tag or bypass
   the guard.

The publish helper is idempotent for reruns: already-published crate versions are skipped.

## GitHub Release Assets

`.github/workflows/release.yml` builds and stages these artifacts:

- one `codewhale-*` runtime binary for Linux x64/arm64, Android arm64, macOS
  x64/arm64, and Windows x64/arm64
- byte-identical `codew-*` command assets copied from that runtime
- byte-identical `codewhale-tui-*` compatibility filenames so installed v0.9.4
  clients can discover and complete the one-runtime upgrade; current installers
  never expose those filenames as a third command
- `codewhale.bat` for the Windows npm/GitHub x64 launcher, and the same
  filename inside Windows zip archives and the NSIS install (those copies
  launch `codewhale.exe` and prefer Windows Terminal)
- platform `.tar.gz` / `.zip` archives and `CodeWhaleSetup.exe`

The release job also uploads `codewhale-artifacts-sha256.txt` and
`codewhale-bundles-sha256.txt`. The npm installer and release verification
script depend on those manifests. The authoritative release asset list lives in
`npm/codewhale/scripts/artifacts.js`.

Before any Cargo or npm publish, prove that the public GitHub Release assets
belong to the tag commit you are publishing:

```bash
./scripts/release/verify-release-assets.sh X.Y.Z
```

That gate compares the local and remote `vX.Y.Z` tag SHAs, confirms a
successful `Release` workflow run used that SHA, then runs the npm wrapper's
release check against the public GitHub asset URLs. The npm check fails if the
release is missing a required binary, archive, installer, or manifest; either
manifest omits a required row; or the assets predate the matching release
workflow run. If the command fails, rerun or repair `release.yml`; do not
publish Cargo or npm against stale assets.

## AUR / Omarchy Package

`codewhale-bin` is a downstream package of the same Linux release, not a new
Codewhale semantic version. After the public asset gate above passes, render
its AUR metadata from the verified release directory:

```bash
./packaging/aur/render.sh /path/to/release-assets /tmp/codewhale-bin
```

The renderer reads the workspace version and extracts the x64/arm64 archive
hashes only after both release checksum manifests agree with the actual files.
It emits no `SKIP` checksums or source-controlled per-release values. Follow
[`packaging/aur/README.md`](../packaging/aur/README.md) for the clean Arch build,
`.SRCINFO` comparison, and package-content checks.

The GitHub release workflows only verify that the AUR metadata can be rendered
from their candidate assets. They do not publish to AUR. AUR publication is a
separate, explicitly authorized maintainer action after the matching tag and
assets are public.

## npm Wrapper Release

`release.yml` publishes `codewhale` through npm Trusted Publishing after the
exact-SHA GitHub Release job succeeds. The job has only `contents: read` and
`id-token: write`; it does not use `NPM_TOKEN`, `NODE_AUTH_TOKEN`, or a
long-lived bypass-2FA credential.

Before the first automated publish, configure the `codewhale` package's npm
Trusted Publisher with these exact values:

- organization or user: `codewhale-hq`
- repository: `Codewhale`
- workflow filename: `release.yml`
- environment: leave blank (the workflow does not claim a GitHub environment)

That npm-side binding is an external release gate. If it is missing or differs
in case, repository, workflow filename, or environment, the publish job must
fail; do not add a token fallback to make it pass.

### Steps

1. Set the npm package version in [npm/codewhale/package.json](../npm/codewhale/package.json) to match the workspace `Cargo.toml`. CI's version-drift guard will catch mismatches before tag.
2. Set `codewhaleBinaryVersion` to the GitHub release tag that should supply binaries.
3. Follow the single candidate/approval/tag sequence above. `release.yml` builds
   the binary matrix and publishes the GitHub Release after its artifact and
   container-build gates. CNB release-tag sync and GHCR uploads depend on that
   GitHub publication; include these destinations in publication approval.
4. **Wait for the GitHub Release to finalize** with the full binary and archive
   matrix, Windows installer, and both checksum manifests. The dependent `npm`
   job checks the remote tag again, runs the public asset freshness gate and
   package tests, then publishes with OIDC. The package's `prepublishOnly` hook
   repeats the clean exact-tag and public-asset checks immediately before the
   registry write.
5. Confirm the `npm` job succeeded, then prove the published package and binary
   version are visible:

```bash
npm view codewhale@X.Y.Z version codewhaleBinaryVersion --json
./scripts/release/check-published.sh X.Y.Z
```

For a rare packaging-only npm release where the npm package version intentionally
points at older Rust binaries, add `--allow-npm-binary-mismatch` and keep the
release notes explicit that no new binary version shipped. That exception is a
separate manual release path: the normal trusted-publishing job deliberately
does not set `CODEWHALE_ALLOW_NPM_BINARY_MISMATCH`.

Do not publish `npm/deepseek-tui`; it is deprecated compatibility metadata only.

### Manual recovery

If GitHub OIDC is unavailable after the GitHub Release and public-asset gate are
green and manual npm publication is approved, use the same clean canonical
checkout on `main` with `HEAD` matching the immutable tag. Authenticate
interactively with npm's normal WebAuthn/2FA flow; never create a long-lived
bypass-2FA token:

```bash
./scripts/release/require-release-tag-checkout.sh X.Y.Z
./scripts/release/verify-release-assets.sh X.Y.Z
npm login
npm whoami
cd npm/codewhale
npm publish --access public
```

The same `prepublishOnly` gates rerun on every attempt. An OIDC or login failure
is not permission to edit the tagged package, move the tag, or skip asset
verification.

## CNB Cool mirror

The `Sync to CNB` workflow mirrors `main` source pushes (and existing supported
contributor branch patterns) to `cnb.cool/codewhale.net/codewhale` for source
access and Linux CI. These workflow patterns do not require an agent branch.
Release-tag sync is called by `release.yml` after canonical GitHub publication;
an ordinary GitHub tag push is not an independent CNB publication receipt.
After approved release publication, **verify the mirror caught the exact tag**
before declaring that destination shipped:

```bash
git ls-remote https://cnb.cool/codewhale.net/codewhale.git refs/tags/vX.Y.Z
```

If the workflow failed for the release tag, use the exact-tag rerun or
`workflow_dispatch --ref vX.Y.Z` recovery documented in
[docs/CNB_MIRROR.md](CNB_MIRROR.md#manual-fallback).

## Recovery and Rollback

### Re-tagging an UNPUBLISHED release (pulled before npm/crates)

If `vX.Y.Z` was tagged and a GitHub Release was created, but **no package was
published** (npm still on the prior version, `check-published.sh` shows the
crates unpublished), the tag is still recoverable — a bug found after tagging
can be fixed and the same version recut. Confirm first that nothing consumed it:
`npm view codewhale version` and crates.io both show the PRIOR version, no
Homebrew/Winget/mirror points at it, and the GitHub Release download counts are
only the release pipeline's own verification passes. Then, only with explicit
Hunter approval naming the release/tag to remove and recreate, use this
exceptional recovery sequence:

```bash
# 1. land the verified fix on canonical main, then freeze its new candidate
# 2. delete the premature Release + tag
gh release delete vX.Y.Z --repo codewhale-hq/CodeWhale --yes --cleanup-tag
git push origin :refs/tags/vX.Y.Z    # belt-and-suspenders
git tag -d vX.Y.Z                    # local
# 3. validate the fixed HEAD: full CI and RC (Parity included), same exact SHA
candidate_sha="$(git rev-parse HEAD)"
gh workflow run ci.yml --repo codewhale-hq/Codewhale --ref main \
  -f expected_sha="${candidate_sha}"
gh workflow run release-candidate.yml --repo codewhale-hq/CodeWhale --ref main \
  -f expected_sha="${candidate_sha}"
# 4. after both receipts and approval, recut at that same HEAD
gh workflow run auto-tag.yml --repo codewhale-hq/CodeWhale --ref main
# 5. release.yml rebuilds assets; rebuild + reinstall locally from the new tag
```

This is the sanctioned path from "do not delete/move/recreate a release tag
implicitly": it is explicit, approved, and only for a tag no registry consumer
has treated as public. Never do it once a crate or the npm wrapper is published
for that version — bump to the next patch instead.

### External publish gates (not code defects)

- **crates.io:** publishing needs a valid crates.io publish token, passed to
  the publish command as `CARGO_REGISTRY_TOKEN` (Rust crates release, step 5).
  Verify it with an authenticated, read-only *client* call rather
  than the `/api/v1/me` endpoint: crates.io answers `/api/v1/me` with
  `403 {"errors":[{"detail":"this action can only be performed on the
  crates.io website"}]}` even for a good token (checked 2026-09-21 with a
  token that `cargo owner` accepts), so a 403 there proves nothing about the
  credential. A good token prints the owner, e.g. `Hmbown (Hunter Bown)`:

  ```bash
  {
    token=; while [ -z "${token}" ]; do printf 'crates.io token: ' >&2; read -rs token || break; echo >&2; done
    [ -n "${token}" ] && CARGO_REGISTRY_TOKEN="${token}" cargo owner --list codewhale-tui
    unset token
  }
  ```

  A 403 or `401` from this call, or `cargo publish` refusing credentials,
  means the token is missing/expired. Create a new token, check it with this
  call, then rerun the step 5 block. It makes another new `CARGO_HOME` and
  build directory, which the rerun needs because the failed run's own state makes
  the guard refuse otherwise, and it keeps the token out of that root.
- **npm:** the OIDC job publishes only if the npmjs.com Trusted Publisher for
  `codewhale-hq` / `Codewhale` / workflow `release.yml` / blank environment is
  configured. Missing config → the `npm` job fails `E404 No match found`. Fix
  the binding (or use the manual WebAuthn recovery above), then re-run the
  failed `npm` job: `gh run rerun <release-run-id> --failed`.

- User-facing rollback:
  - npm: `npm install -g codewhale@X.Y.Z`
  - Cargo: `cargo install codewhale-cli --version X.Y.Z --locked --force`;
    add an optional `codew` alias as documented in
    [docs/INSTALL.md](INSTALL.md#7-build-from-source)
  - manual assets: download binaries or the platform archive plus the matching
    `codewhale-artifacts-sha256.txt` or `codewhale-bundles-sha256.txt`
    manifest from `https://github.com/codewhale-hq/CodeWhale/releases/tag/vX.Y.Z`
  - workspace files: use `/restore list [N]` and `/restore <N>` for side-git
    snapshots; this does not change the installed binary version or rewrite
    conversation history
  - keep [docs/INSTALL.md](INSTALL.md#roll-back-to-a-previous-release) in sync
    with these commands
- Crates publish partially:
  - rerun the Rust crates release step 5 block; its timestamped root gives
    each rerun another new, empty `CARGO_HOME` and build directory, and the failed
    run's own state makes `publish-crates.sh publish` refuse otherwise
  - already-published crate versions will be skipped
- GitHub assets missing or checksum manifest incomplete:
  - fix `.github/workflows/release.yml`, but do not rerun it over an existing
    asset set and do not delete assets merely to make the guard pass
  - if any asset may have been public or consumed, cut a new patch version
  - only after explicit maintainer approval and proof that no downstream
    publication or consumer treated the failed asset set as public may a
    deliberately scoped recovery remove the failed release before an exact-tag
    rerun; record that exception in the release packet
- npm packaging-only problem:
  - bump only the npm package version
  - keep `codewhaleBinaryVersion` on the last known-good Rust release
  - repack and republish the wrapper
- A bad npm publish cannot be overwritten:
  - publish a new npm version with corrected metadata or install logic
- CNB mirror failed for the release tag:
  - check the run via `gh run list --workflow=sync-cnb.yml`
  - rerun the failed tag run, or dispatch
    `gh workflow run sync-cnb.yml --ref vX.Y.Z`; never omit the tag ref
  - follow the proof steps in
    [docs/CNB_MIRROR.md](CNB_MIRROR.md#manual-fallback)
- Workflow runner failure or hung release job:
  - every release-lane job carries an explicit `timeout-minutes` to contain
    unattended runs, but timeouts are containment rather than immediate recovery
  - if a workflow job sits `in_progress` with 404 logs (or produces no useful
    log output for 20 minutes), cancel the run and rerun failed jobs / dispatch
    an exact-ref rerun rather than waiting out the full job timeout
  - check the last-useful-log timestamp before cancelling to distinguish an
    infrastructure failure (runner dropped / HTTP 404 on log stream) from a
    legitimate long build step (e.g. Windows artifact compilation)
