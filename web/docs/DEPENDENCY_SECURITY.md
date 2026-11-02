# Website build dependency mitigation

The existing GHSA-vfj7-8cjw-p6xm mitigation covers `braces` 3.0.3
and its build-tool dependents.
We retain the upstream package name, version and licence, and the complete raw
audit report. This is a local mitigation, not an upstream fixed version or a
claim of zero upstream findings.

`npm ci` runs `scripts/patch-braces.mjs`. It verifies the reviewed original or
patched SHA-256 of all four affected source files before writing any, limits
the parser's structural stack and every AST walker to 64 levels, and refuses
an unknown version or changed source. The normal expansion, compilation,
escaping and length limits remain intact. No package is published or replaced.

`npm run audit:dependencies` verifies the installed patched hashes before
auditing the complete lock. Only the exact reviewed advisories described here
and dependencies whose entire advisory chain ends in them can be classified
as mitigated. An unreviewed nested copy, another advisory, an incomplete audit,
or an unapplied/changed patch fails the check. CI saves the raw report,
including the mitigated upstream findings. Installation with `--ignore-scripts`
does not apply the mitigations and cannot pass this gate.

After an upstream fix becomes available, upgrade and verify it, then remove
the installation patch, its audit classification and tests in the same slice.
Never broaden the admitted advisory list to get a passing check.

References: [upstream advisory](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm)
and [upstream issue and recommended depth guard](https://github.com/micromatch/braces/issues/70).

## Minimatch globparts amplification

Dependabot #244 reports [GHSA-g3hv-75wg-m249](https://github.com/advisories/GHSA-g3hv-75wg-m249)
against minimatch versions below 11.0.1. The upstream report identifies
`firstPhasePreProcess` at optimization level 2 or higher. The installed 8.0.7
and 10.2.5 copies contain that path; the six 3.1.5 copies do not contain it.
The broad registry advisory remains visible for all of these old versions.

Upstream's [fix](https://github.com/isaacs/minimatch/commit/77a0f5c4b80bdc641c3c930a1d9edf5ad2db57f8)
caps generated patterns at `braceExpandMax` (100,000 by default) and skips the
quadratic deduplication pass for more than 1,000 patterns. Version 11 also
[replaces RegExp entries with Matcher objects and removes existing APIs](https://github.com/isaacs/minimatch/blob/v11.0.1/changelog.md).
A direct 11.0.3 override made both installed glob 9 and glob 12 silently return
no files for a valid glob. That incompatible override is not retained.

`scripts/patch-minimatch.mjs`, run by `npm ci`, backports only those upstream
preprocessing bounds to the reviewed 8.0.7 and 10.2.5 CommonJS and ESM files.
It validates every locked installed copy, version and complete file hash
before writing any file, and refuses unknown versions, source changes or an
unapplied patch in check mode. All six 3.1.5 source files retain their original
reviewed hashes. Package names, versions, licences, ordinary matching APIs and
the complete raw audit findings remain unchanged. This is a compatible local
backport, not an upstream fixed-version or Dependabot-closure claim.

The dependency audit admits this exact advisory only after that installed
verification and only for the verified node paths. Different advisories or
unreviewed nested copies remain blocked. Remove the backport and its audit
classification together once compatible consumer upgrades can use a fixed
upstream minimatch release and their actual glob/TypeScript journeys pass.

## Glob traversal after cancellation

The same complete audit also reports [GHSA-3g3m-2fjr-6pw4](https://github.com/advisories/GHSA-3g3m-2fjr-6pw4)
in the installed glob 9.3.5 and 12.0.0. Its fixed upstream version is 14.0.2.
Before the compatible backport, a controlled abort rejected both `glob()` and
`globStream()` while each continued reading the fixture tree afterward.

`scripts/patch-glob.mjs` applies the [upstream walker fix](https://github.com/isaacs/node-glob/commit/4682e8c9babee1b385202ede36389634e12076d7)
to the reviewed CommonJS and ESM files of both installed versions. Four abort
branches return after their callback, and the corresponding `onResume` change
allows pending callbacks to drain. The version and full preimage/postimage
hashes of every locked copy are verified before any file is changed. Unknown
versions, changed bytes and unpatched installations fail closed. Package
versions, licences and consumer APIs are preserved; no major upgrade is forced.

The audit keeps the raw upstream findings and verifies these installed hashes
before admitting this exact advisory and its complete transitive chains. Its
finite graph walk checks every reachable concrete advisory leaf, requires at
least one verified leaf, and rejects unknown advisories and missing or empty
entries. A cycle without a concrete verified leaf stays blocked. Cyclic peer
dependencies with only verified leaves do not manufacture an unknown finding.
The recorded upstream finding count is never changed to zero by this local
classification. No advisory is dismissed or represented as registry-fixed.

## Bundled runtime copies

The lockfile inventory does not expose dependencies compiled into another
package. A separate inventory of the installed web npm graph found affected
preprocessing in four runtime bundles: Next 16.4.1's
`dist/compiled/@vercel/nft/index.js`, cf 1.0.0-beta.12's
`dist/autoconfig-PfnbLuEO.mjs` and `dist/d1-MvAQMvvo.mjs`, and wrangler
4.147.0's `wrangler-dist/cli.js`. The Next NFT bundle also contains the affected
glob walkers. Its original bytes match the
[tagged upstream bundle](https://github.com/vercel/next.js/blob/v16.4.1/packages/next/src/compiled/@vercel/nft/index.js).
All four originals match the official npm archives and locked integrity values.

`scripts/patch-bundled-globs.mjs` applies the same upstream preprocessing fix
to these four reviewed bundles and the walker fix to NFT, preserving private
variable and callback bindings. A closed table requires the exact package
versions, lock identities and complete preimage/postimage hashes. Every file is
validated before any write. Postinstall applies the backports and the dependency
audit always verifies all four bundles, even though npm's advisory report does
not list the embedded copies. Unknown versions, changed bytes and unapplied
patches fail closed. Package licences and notices remain unchanged.

The public `nodeFileTrace` fixture preserves the expected traced files and
warnings. That public API does not expose the bundled walkers' cancellation
control; no bundled runtime cancellation result is inferred from the separate
glob package probes. The reviewed source guards and bundle identity provide
that narrower evidence. CLI help/version checks cover loading these tool
entrypoints without deployment or account access.

The literal-symbol inventory covers 12 matching runtime JavaScript files in
this installed npm graph: four minimatch distributions, four glob walkers and
these four bundles. Source maps are counted separately and are not executed
copies; the scan cannot prove absence of arbitrarily renamed implementations.
Next's separate compiled `glob` is
[glob 7.1.7](https://github.com/vercel/next.js/blob/v16.4.1/packages/next/taskfile.js),
outside this glob advisory's affected range; its old matcher has no reported
preprocessing path. These scoped findings are not a claim that every bundled
dependency is free of vulnerabilities.
