# CNB Cool mirror

> 阅读简体中文版：[zh_hans/CNB_MIRROR.md](zh_hans/CNB_MIRROR.md)。

`cnb.cool/codewhale.net/codewhale` is a one-way mirror of this
GitHub repository for users on networks where GitHub is slow or blocked
(primarily mainland China). The mirror receives every push to `main` and
each `v*` release tag after its complete GitHub Release is published. The
GitCode mirror (`gitcode.com/HunterBown/Codewhale`) receives the same `main`
and release tags from [`Sync to GitCode`](../.github/workflows/sync-gitcode.yml);
it carries source only, with no binary releases.

## Source and downloads are separate checks (2026-10-06 audit)

CNB `main` matched canonical GitHub `main` during this audit. GitHub, npm and
npmmirror's latest published version was **0.10.0**; the two registry wrapper
tarballs had identical bytes and verified integrity. CNB's latest binary
release remained **0.9.9**. Its `v0.10.0` source tag exists, but that tag build
failed the release-note check before compilation and upload. The CNB 0.10.0
checksum manifest returned 404; do not recommend forcing the CNB-only installer
for that version. 0.10.1 remains a Main source candidate until publication.

Check source identity, then the actual versioned release, assets and manifest.
A successful GitHub mirror workflow or a source tag is not binary availability.
Re-pushing an existing tag does not create a fresh tag-push build. Maintainers
must recover the build/publication separately and verify the result, preserving
published tags. Refresh this dated status after publication.

## Provenance

**GitHub is the sole canonical source.** All releases, tags, and source code
originate at `github.com/codewhale-hq/CodeWhale`. The CNB mirror is a read-only
replica maintained by the `Sync to CNB` workflow — it exists solely to serve
users behind GFW-blocked or slow GitHub connections.

Every CNB release includes `codewhale-artifacts-sha256.txt` — a SHA256 manifest
of the CNB-built Linux x64 binaries, generated from the same source commit that
is tagged on GitHub. (CNB builds from source, so these checksums cover the
CNB-built artifacts, not GitHub's release assets.) Verify a downloaded binary
against it:

```bash
# Verify a downloaded CNB binary against the CNB manifest
sha256sum -c codewhale-artifacts-sha256.txt --ignore-missing
```

## How it works

The mirror is maintained by the [`Sync to CNB`](../.github/workflows/sync-cnb.yml)
GitHub Actions workflow:

- **Trigger:** `push` to `main`, the release workflow after canonical publication,
  the `release: published` event for a release published outside it, a
  schedule at :17 and :47 past each hour, or `workflow_dispatch` for manual
  recovery. The schedule re-mirrors `main` (catching pushes that started no
  workflow) and pushes the latest GitHub Release tag when CNB lacks it, after
  the same release-inventory check; it fails when that tag differs on CNB or
  its CNB checksum manifest is unreachable. It never creates a tag or
  release: it only copies ones already published on GitHub. A tag push alone does not mirror a release tag. Manual tag recovery
  also verifies the complete public GitHub asset inventory and immutable source.
- **Auth:** HTTPS basic auth as user `cnb` with the `CNB_GIT_TOKEN`
  repository secret as the password. Forks never inherit the secret, so the
  `sync` job runs only when the repository owner is `codewhale-hq`; on a fork
  it is skipped instead of failing. In this repository a missing or misspelled
  secret still fails the push step.
- **Scope:** only the ref that triggered the run is pushed. Tag pushes
  push exactly that tag; branch syncs push `main`. Other branches and
  dependabot refs are intentionally *not* mirrored.
- **Concurrency:** `cnb-sync-${{ github.ref }}` serializes each ref separately.
  Distinct release tags cannot replace one another in a global pending slot;
  only a newer push of the same branch supersedes its pending branch sync.
- **Retry:** each push is retried up to three times with linear
  backoff (5s, 10s) before the workflow gives up.

CNB pipeline configuration is also source-controlled in GitHub at
[`/.cnb.yml`](../.cnb.yml). This is deliberate: the sync workflow force-mirrors
GitHub refs to CNB, so pipeline files created only on the CNB side will be
overwritten. Submit `.cnb.yml` changes through GitHub PRs and let the one-way
mirror carry them to CNB.

## CNB tag releases

When CNB receives a `v*` tag, the root `.cnb.yml` tag pipeline builds Linux x64
release assets from source and publishes a CNB release with:

- `codewhale-linux-x64`
- `codew-linux-x64`
- `codewhale-tui-linux-x64` (compatibility-only release filename; not a third
  installed command)
- `codewhale-artifacts-sha256.txt`

This gives users who can reach CNB but not GitHub a CNB-native release path.
GitHub remains the canonical macOS/Windows release matrix; the CNB tag pipeline
is the China-friendly Linux x64 fallback.
The GitHub release workflow calls the mirror only after publishing its complete,
verified asset set. CNB cannot publish a version whose canonical release failed.
An existing CNB tag must match; recovery never force-updates a release tag.

## CNB Linux CI and release preflight

First-party `fix/*` and `rebrand/*` branches are mirrored to CNB so the heavy
Linux Rust gates run on Tencent-hosted runners instead of GitHub Actions:

- `./scripts/release/check-versions.sh`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test --workspace --all-features --locked`
- `cargo build --release --locked -p codewhale-cli --bin codewhale`
- `node scripts/release/npm-wrapper-smoke.js`

Release branches matching `work/v*` also run
`./scripts/release/publish-crates.sh dry-run`. GitHub Actions keeps the cheap
drift/fmt statuses plus the macOS and Windows jobs that CNB cannot replace.

## Verifying the mirror after a release

After `release.yml` completes for a `vX.Y.Z` tag, the CNB mirror
should have both the new commit on `main` and the new tag:

```bash
# Quick check: does the new tag exist on CNB?
git ls-remote https://cnb.cool/codewhale.net/codewhale.git \
    refs/tags/vX.Y.Z

# Quick check: is CNB's main at the same commit as origin/main?
gh_main=$(git ls-remote https://github.com/codewhale-hq/CodeWhale.git refs/heads/main | awk '{print $1}')
cnb_main=$(git ls-remote https://cnb.cool/codewhale.net/codewhale.git refs/heads/main | awk '{print $1}')
test "$gh_main" = "$cnb_main" && echo "in sync" || echo "DIVERGED: gh=$gh_main cnb=$cnb_main"
```

Or check the workflow run directly:

```bash
gh run list --workflow=sync-cnb.yml --repo codewhale-hq/CodeWhale --limit 5
```

If the most recent run for the release tag is `success`, the mirror
caught it. If it's `failure`, fix or re-run the mirror workflow before
directing users to the mirrored tag.

## Manual fallback

Manual mirror repair is maintainer-only. Do not put PATs in remote URLs or
publish force-push recipes in contributor-facing docs. Use the configured
GitHub Actions secret and the workflow dispatch path whenever possible.

### Re-trigger the workflow manually

If the workflow is healthy but happened to fail on the release run
(e.g. a transient CNB outage that's since cleared), retrigger it
without pushing anything:

```bash
# Prefer rerunning the existing failed tag run when one exists.
gh run rerun <failed-tag-run-id> --repo codewhale-hq/CodeWhale

# If no tag run exists, dispatch from the exact existing release tag.
gh workflow run sync-cnb.yml --repo codewhale-hq/CodeWhale --ref vX.Y.Z
```

Do not omit `--ref` when repairing a tag: a default-branch dispatch syncs
`main`, not `refs/tags/vX.Y.Z`. Afterward, prove the tag and its Linux x64
release assets exist before directing users to CNB.

## Rotating `CNB_GIT_TOKEN`

If the workflow starts failing with auth errors and the token has
expired:

1. Log in to `cnb.cool` and generate a new personal access token
   with `repo` (push) scope.
2. Update the `CNB_GIT_TOKEN` repository secret:
   ```bash
   gh secret set CNB_GIT_TOKEN --repo codewhale-hq/CodeWhale
   ```
3. Re-trigger the workflow on a recent commit:
   ```bash
   gh workflow run sync-cnb.yml --repo codewhale-hq/CodeWhale
   ```
4. Confirm the run succeeds via `gh run list --workflow=sync-cnb.yml`.

## Binary release assets and `codewhale update`

CNB now builds Linux x64 assets for `v*` tags from the source-controlled
`.cnb.yml` pipeline. GitHub remains the canonical macOS/Windows release matrix.

### Automatic source selection (Linux x64)

On Linux x64, `codewhale update` picks its asset source before it downloads
anything large. Once the target tag is known, it requests
`codewhale-artifacts-sha256.txt` for that exact tag from GitHub Releases and
from the CNB release **at the same time**, and takes the first source that
answers with a manifest listing `codewhale-linux-x64`. The straggler's answer is
discarded.

Three properties this relies on:

- **The manifest is the probe.** It is a few hundred bytes, so a blocked or slow
  source loses in about the time its connection takes to fail — the user never
  waits out a stalled multi-megabyte asset download, and no timeout is doing the
  choosing.
- **Manifest and binary come from the same source.** CNB builds its own
  artifacts from the tagged source (musl-static, not GitHub's glibc build), so
  the two manifests describe different bytes and are not interchangeable. The
  winning source supplies both, and a checksum mismatch fails the update rather
  than falling back to the loser.
- **Selection never changes which release is installed.** The tag still comes
  from GitHub's stable-release or beta-release lookup, so `--beta` keeps its
  meaning; only where the bytes for that tag are fetched from is decided by the
  probe.

`codewhale update` and `codewhale update --check` both print the result as a
`Release source:` line, and the post-install summary repeats it, so the source a
given binary came from is recoverable after the fact.

Every other target keeps a single canonical source: CNB publishes Linux x64 and
nothing else, so macOS, Windows, Android, and Linux arm64 do not race CNB;
Linux riscv64 remains explicitly unsupported. All supported self-update paths
are nevertheless checksum-required: the chosen source must publish a valid
`codewhale-artifacts-sha256.txt` entry for the exact platform binary, or
`codewhale update` stops before downloading that binary. There is no
unverified-install fallback.

Setting `CODEWHALE_RELEASE_BASE_URL` (or a legacy alias) or
`CODEWHALE_USE_CNB_MIRROR` turns selection off entirely — an explicitly named
source is used as named, including its own checksum manifest, with
`CODEWHALE_RELEASE_BASE_URL` outranking `CODEWHALE_USE_CNB_MIRROR`.

### Manual paths

Users behind GitHub-blocking networks can also select a source explicitly:

- **`cargo install`** from the CNB mirror:
  ```bash
  cargo install --git https://cnb.cool/codewhale.net/codewhale --tag vX.Y.Z codewhale-cli --locked
  ```
  The current `codewhale` binary runs the TUI in-process. Cargo users who want
  the optional short command can add a `codew` symlink beside it; a separate
  `codewhale-tui` install is not required.
  Linux build-time dependencies (`build-essential`, `pkg-config`,
  `libdbus-1-dev` on Debian/Ubuntu) are required — see
  [INSTALL.md](INSTALL.md#5-cargo-and-building-from-source).

- **CNB release assets** for Linux x64, when the matching CNB tag pipeline has
  completed successfully. Download `codewhale-linux-x64`, `codew-linux-x64`,
  and `codewhale-artifacts-sha256.txt` from the CNB release for `vX.Y.Z`, then
  verify the binaries against the manifest. The published
  `codewhale-tui-linux-x64` file is a legacy-client bridge and is not required
  by current installs. On Linux x64 and OpenHarmony x64 the npm wrapper probes
  that CNB checksum manifest concurrently with GitHub Releases for the exact
  package version and locks onto the first source whose HTTP response and
  manifest validate — it does not wait for a slow GitHub binary download. Set
  `CODEWHALE_USE_CNB_MIRROR=1` to force CNB only, or
  `CODEWHALE_RELEASE_BASE_URL` to skip the race. Other platforms must use
  GitHub or a complete `CODEWHALE_RELEASE_BASE_URL` mirror.

- **`CODEWHALE_RELEASE_BASE_URL`** environment variable, if a CDN mirror of
  release assets exists. The npm wrapper installer and `codewhale update` read
  this variable to redirect binary downloads. For `codewhale update`, also set
  `CODEWHALE_VERSION=X.Y.Z` so the updater can label the mirrored
  release without contacting GitHub. The directory pointed to must contain
  `codewhale-artifacts-sha256.txt` and the platform binaries; format matches
  a GitHub Release asset directory. The earlier `DEEPSEEK_TUI_*` names remain
  accepted as compatibility aliases.

## Clone from CNB

For a stable install, clone `main` or a release tag from:

```bash
https://cnb.cool/codewhale.net/codewhale.git
```

The mirror receives `main` and release tags. GitHub
is the fallback when the CNB workflow or credentials are unhealthy.

The bilingual self-hosting guide is
[`scripts/tencent-lighthouse/README.md`](../scripts/tencent-lighthouse/README.md).
CNB deploy-button examples live in
[`deploy/tencent-lighthouse/cnb/`](../deploy/tencent-lighthouse/cnb/README.md). They are
not active until copied into `.cnb.yml` and `.cnb/tag_deploy.yml`, because live
deploy jobs require a Lighthouse deploy key, target host, and explicit CNB
quota/billing policy.
