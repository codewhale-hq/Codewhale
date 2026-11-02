# Release Checklist

Follow this sequence from the canonical `/Volumes/VIXinSSD/CW/codewhale`
checkout on `main`. Agent-owned preparation lands as verified slices on `main`;
it requires no release branch, worktree, nested clone or preparation PR.
Contributor PRs still land as PRs. Treat any unchecked required gate as a release
blocker.

For deeper context on the underlying tools (preflight scripts, npm smoke,
publish-crates), see [`RELEASE_RUNBOOK.md`](RELEASE_RUNBOOK.md).
For larger milestone releases, add any version-specific acceptance matrix to
the existing release packet before tagging; use it for provider routes, feature
gates, GUI/runtime smoke, remote-workbench decisions, and credit hygiene that the
generic checklist does not enumerate. Record its candidate SHA, owners, local
check results, hosted run URLs, artifact inventory, install/provider receipts,
known limitations and publication approval. These proofs are separate.

## 0. Reconcile the canonical checkout

- [ ] Inspect the branch, status, upstream and alternate checkouts before editing:
      ```bash
      git branch --show-current
      git status --short --branch
      git log -5 --oneline
      git worktree list
      git fetch origin main
      git log --left-right --oneline main...origin/main
      ```
- [ ] The canonical checkout is on `main`. Recover stranded work only after
      preserving dirty/untracked files and comparing commits with current
      `origin/main`. Keep a recoverable backup and unresolved inventory; do not
      reset away work, stash it indefinitely or create another development lane.
- [ ] Uncommitted work found in the checkout is yours (founder, 2026-10-22):
      read it, prove it, and commit it by explicit path. A file changed in the
      last 15 minutes or held by a live session is mid-edit: ask that writer
      to commit first. Check status before staging; never `git add -A`.
      Historical branches/checkouts are inventoried;
      deleting them requires explicit Hunter approval even if a helper calls
      their tips safe to delete.

## 1. CHANGELOG entry exists for the version

- [ ] `CHANGELOG.md` has a `## [X.Y.Z] - YYYY-MM-DD` heading at the top
- [ ] The entry credits every external contributor, harvested PR author,
      linked issue reporter, reproduction/log provider, reviewer, and
      verification helper whose work materially shaped this version. Get the
      commit list with:
      ```
      git log vPREV..HEAD --no-merges --format="%h %an <%ae> %s" \
        | grep -v '<your-email@…>'
      ```
      For each contributor, link both their display name and (when known)
      `@github-handle`. Then inspect linked issues and harvested PRs so
      reporters/helpers are not lost just because they did not author commits.
      Keep `docs/CONTRIBUTORS.md` and `web/lib/release-credits.ts` consistent.
- [ ] The entry uses the Keep a Changelog headers — `Added`, `Changed`,
      `Fixed`, `Security`, `Removed`, `Deprecated`. Add `Known issues` only
      if there is something material the user must work around.
- [ ] The entry mentions all referenced issue/PR numbers as `#NNNN` so the
      auto-linker on GitHub picks them up.
- [ ] Run `scripts/sync-changelog.sh` to regenerate `crates/tui/CHANGELOG.md`
      (the recent-releases slice embedded in the binary for `/change`). Do
      not edit that file by hand, and do not copy the full root changelog
      into it — older entries live in `docs/CHANGELOG_ARCHIVE.md`.
- [ ] Run `scripts/release/check-feature-release-notes.sh vPREV HEAD`. Every
      issue-linked `feat` commit must leave a receipt in `CHANGELOG.md` or the
      archive; the Version drift CI gate runs the same check with full history.

## 2. Version pins are in sync

- [ ] Use `./scripts/release/prepare-release.sh X.Y.Z` when version pins or
      generated sources need preparation. Reuse a valid preparation receipt
      when those sources are already current. The helper bumps the
      workspace version, every per-crate dependency pin, the npm wrapper
      (`version` + `codewhaleBinaryVersion`), Runtime SDK, VS Code extension
      and lock, remote-smoke default, public source-candidate facts, and README
      install-tag examples; it refreshes the Cargo/npm locks, regenerates
      `crates/tui/CHANGELOG.md` and `web/lib/facts.generated.ts`, and ends
      by running the version and OHOS gates. Write the CHANGELOG entry
      **before** running it. The helper is safe to rerun at the requested
      workspace version; it skips replacements but refreshes both generated
      files and reruns the gates.
- [ ] `npm/deepseek-tui/package.json` remains private/compatibility-only and
      is **not** bumped or published.
- [ ] `./scripts/release/check-versions.sh` reports
      `Version state OK: workspace=X.Y.Z, npm=X.Y.Z, npm-binary=X.Y.Z, lockfile in sync.`
- [ ] `./scripts/release/check-ohos-deps.sh` reports that the OpenHarmony
      Windows linker keeps the target/sysroot flags, the target enables the
      `rquickjs-sys` bindgen edge, and its graph does not pull the unsupported
      `nix` 0.28/0.29, `portable-pty`, `starlark`, `arboard`, or `keyring`
      crates.

## 3. Verify and land preparation slices

- [ ] Run the existing checks appropriate to each changed surface once; batch
      coherent Rust edits before compiling. During implementation, use the
      smallest local check or running-product observation that proves the
      change. After implementation and product review, freeze the candidate
      commit and run final validation once, including
      `npm test && npm run check:web` from the repository root. Hosted CI owns
      exhaustive workspace and platform coverage at that final step. Rerun
      only checks invalidated by a fix. Record actual results and failures in
      commit messages.
- [ ] Commit verified owned paths directly to `main` and push normally. Resolve
      push rejection with the other writer while preserving dirty work; never
      force-push. Reuse valid receipts instead of rebuilding merely to commit
      or push. Existing contributor PRs retain their review and merge process.
- [ ] The live milestone and PR queue no longer contain work intended for this
      version:
      ```
      gh issue list --repo codewhale-hq/CodeWhale --milestone "vX.Y.Z" --state open
      gh pr list --repo codewhale-hq/CodeWhale --state open --limit 100
      ```
- [ ] Any remaining same-theme work is explicitly retargeted to a later
      version or called out as a known issue. Do not freeze/tag while still
      planning to merge more same-version fixes.
- [ ] The release tag does not already point at an older source SHA, or the
      maintainer has deliberately chosen to publish exactly that older SHA:
      ```
      git ls-remote origin refs/heads/main refs/tags/vX.Y.Z
      gh release view vX.Y.Z --repo codewhale-hq/CodeWhale
      ./scripts/release/check-published.sh X.Y.Z
      ```
- [ ] If `vX.Y.Z` exists with no GitHub Release/packages and `main` has moved
      on, stop. Choose one of: publish the existing tag as-is, bump the later
      work to the next patch version, or explicitly approve deleting/recreating
      the unpublished tag. Publishing older source after canonical `main` has
      advanced needs a separately authorized recovery plan; do not silently
      move tags or switch the shared checkout. An absent release/unpublished
      packages are expected at this stage, not permission to alter a tag.
- [ ] Freeze the final source: all intended work is committed and pushed;
      clean local `HEAD`, `main` and freshly fetched `origin/main` agree.
      Record the full 40-character SHA/version and coordinate with shared
      writers to preserve that source through packaging and publication.
- [ ] `./scripts/release/check-versions.sh --require-dated-release` and
      `python3 scripts/check-contributor-credit.py` pass for the candidate.
- [ ] `./scripts/release/publish-crates.sh dry-run` passes for the frozen
      candidate. Cargo 1.90+ verifies every release tarball and its real size
      without uploading. CI/RC does not replace this check. Retain the receipt;
      do not repeat it for unchanged source just to commit or push. Publish mode
      necessarily verifies the packages again before uploading.

## 4. Exact-SHA full CI and release candidate

- [ ] Dispatch full CI and the build-only release candidate against frozen
      `main` with the same independent SHA guard:
      ```bash
      candidate_sha="$(git rev-parse HEAD)"
      gh workflow run ci.yml --ref main -f expected_sha="${candidate_sha}"
      gh workflow run release-candidate.yml --ref main -f expected_sha="${candidate_sha}"
      ```
- [ ] Both runs resolve to that SHA and all required jobs execute and succeed.
      A green workflow badge alone is insufficient: ordinary main-push CI can
      delegate Linux work to CNB and omit some smoke coverage. Manual full CI
      forces the release gates on Linux, macOS and Windows.
- [ ] The candidate's Parity job succeeds, and its artifact jobs report all eight
      targets and the complete 38-file asset inventory, including Android
      arm64, Windows arm64, native HarmonyOS arm64, `codew`, the NSIS installer,
      archives, checksum manifests, and eight compatibility-only
      `codewhale-tui-*` release filenames that are not installed commands. The
      packed npm wrapper installs against those assets and runs its delegated
      entrypoints. Keep run URLs and
      required evidence before the Actions artifacts expire; these are not a
      public release.
- [ ] Windows static import receipts cover x64/ARM64 CLI, shim and compatibility
      assets, any optional host, and the NSIS setup plus its actual embedded PE
      plugins. Verify the selected architecture profile and its conservative
      limits in [ENVIRONMENTS.md](ENVIRONMENTS.md#windows-support-and-release-qualification).
      Preserve the generated candidate Uninstall.exe import/SHA receipt. Static
      import/profile PASS does not replace final release startup/install on the
      promised oldest x64 OS, native ARM64 package acceptance, or packaged/native
      app owner-crash recovery. Record or explicitly resolve these remaining
      runtime gates before tagging.
- [ ] Complete the version-specific acceptance packet. Cross-building Android
      does not prove a real Termux/device session. Local install, actual provider
      calls and customer acceptance need separate receipts; provider spend still
      requires authorization. Required unproven acceptance blocks release;
      disclose an explicitly accepted limitation as such.
- [ ] If source changes or `main` moves before tagging, deliberately choose the
      new candidate and replace affected proofs. Tag only a SHA with its exact
      CI/RC evidence. Do not duplicate hosted exhaustive gates locally.

## 5. Publication approval

- [ ] Hunter explicitly approves the exact version/SHA and publication targets:
      tag, GitHub Release, npm, GHCR, CNB release tag and legacy Homebrew
      automation, plus Cargo or other downstream publication where intended.
      Source readiness and build-only CI/RC do not authorize a tag, registry
      write, deployment or public launch.

## 6. Tag and publish the approved source

- [ ] Fetch `origin/main` again and verify that clean canonical `main`,
      `origin/main` and the approved SHA still agree.
- [ ] The release source is reachable from `main`:
      `./scripts/release/ensure-release-on-main.sh HEAD`
- [ ] Create `vX.Y.Z` from the final `main` SHA using the **Create release tag**
      workflow, or create and push a signed local tag:
      `git tag -s vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`
      Tagging can start public automation immediately. If no run appears,
      first exclude a queued/active tag-triggered run, then dispatch only the
      existing tag: `gh workflow run release.yml --ref vX.Y.Z -f version=X.Y.Z`.
      Do not dispatch `release.yml` from `main` or start duplicate runs.
- [ ] The Release workflow succeeds: it repeats parity/build gates, verifies
      the draft asset inventory before GitHub publication, then runs dependent
      npm, GHCR, CNB and Homebrew publication. It requires an exact-SHA green
      RC receipt and does not publish Cargo crates.
- [ ] The public GitHub Release assets are proven to match the tag commit
      before publishing Cargo or npm:
      ```
      git fetch origin tag vX.Y.Z
      ./scripts/release/verify-release-assets.sh X.Y.Z
      ./scripts/release/require-release-tag-checkout.sh X.Y.Z
      ```
      This checks the local tag, remote tag, successful Release workflow SHA,
      full binary/archive/installer asset set, and both checksum manifests. If
      it fails, stop publication and follow the runbook's recovery gates.
      Publish from the same clean canonical checkout on `main` when `HEAD`
      matches the immutable remote tag. Do not create a detached worktree,
      switch release lanes or force-fetch a conflicting tag. If `main` advanced,
      coordinate with Hunter for a new candidate or separately authorized
      recovery plan.
- [ ] Publish Cargo manually within the approved scope with the
      [runbook's step 5 block](RELEASE_RUNBOOK.md#rust-crates-release), which
      runs `./scripts/release/publish-crates.sh publish` from a new, empty
      `CARGO_HOME` and build directory with the token in `CARGO_REGISTRY_TOKEN`. It
      verifies every package, skips already-published versions and waits for
      each new dependency to appear. npm normally uses Trusted Publishing;
      manual recovery keeps the same clean-source and public-asset guards.

## 7. Verify public results

- [ ] The live GitHub Release body has its own `## Contributors` or
      `## Credits` section; do not rely on "see CHANGELOG" alone. Verify with:
      ```
      gh release view vX.Y.Z --repo codewhale-hq/CodeWhale --json body \
        --jq '.body | test("## (Contributors|Credits)")'
      ```
- [ ] `npm view codewhale@X.Y.Z version codewhaleBinaryVersion --json`
      reports the new version on the npm registry.
- [ ] `npm view deepseek-tui deprecated` is non-empty. The legacy npm package
      is deprecated and must not receive an `X.Y.Z` publish.
- [ ] Distribution channels are canonical-first: the website install page
      (codewhale.net/install) shows Codewhale-native commands first (`npm install -g
      codewhale`, `curl .../install.sh | sh`); Homebrew is labeled as legacy
      compatibility; the shell installer uses codewhale-native names as documented
      in `docs/REBRAND.md#homebrew`.
- [ ] Every release crate has the new version on crates.io; there is no
      automated Cargo publish job. Partial publication is incomplete.
- [ ] `ghcr.io/codewhale-hq/codewhale:vX.Y.Z` and `:latest` are updated.
- [ ] The CNB release tag and approved downstream results are verified
      separately; a successful GitHub Release does not prove them.
- [ ] The final registry verification passes:
      ```
      ./scripts/release/check-published.sh X.Y.Z
      ```
- [ ] Published install/upgrade smoke has its own receipt; candidate installs
      do not prove public delivery. Deployment/customer acceptance remain
      separate gates where required.

## 8. Handoff and tracking

- [ ] Edit the GitHub release notes to expand any CVE-style or attack
      details within the approved publication scope.
- [ ] Re-run the GitHub Release body check after any release-workflow rerun;
      workflows can overwrite notes and accidentally remove contributor credit.
- [ ] Note any deferred items in the next release's tracking issue.
- [ ] Close any issues that this release fixed.
- [ ] Update existing tracking with evidence and follow the current contributor
      communication rules in [AGENTS.md](../AGENTS.md). Handle the existing
      `sync-release-record` workflow's
      metadata proposal after publication: it currently opens a bot PR, not a
      required preparation branch. A source record does not prove deployment.
- [ ] Report canonical path, branch, SHA, upstream state and dirty/stranded
      work. Historical branch/checkout removal needs explicit cleanup approval
      and does not block a release whose source is already reconciled.

---

If a step fails, **fix the underlying cause** rather than skipping it. Pre-commit
hooks, signing, and CI are all here to catch real problems. Do not skip hooks or
signing, force-push shared refs, replace public assets or move release tags to
make the checklist look green.
