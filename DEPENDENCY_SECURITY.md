# Extension packager dependency acceptance

The npm audit of `extensions/vscode` reports GHSA-xcx6-jx8p-x3h5, a high
severity CWE-346 advisory in `@azure/msal-node` 6.0.1. It reaches the lockfile
through `@vscode/vsce` 4.0.0 → `@azure/identity` 4.13.3 → `@azure/msal-node`, all
of which are build-only tooling. There is no upstream-consistent fix: every
published `@vscode/vsce` and `@azure/identity` release still declares
`@azure/msal-node: ^6.0.0`, so only a semver-breaking `overrides` pin or a major
downgrade of the packager would clear the report, and either changes the
packaging tool's behaviour.

This is a reviewed acceptance of an unfixed advisory, not a mitigation. The
vulnerable package is a `devDependency`: `npm run package` invokes
`vsce package --no-dependencies`, so no dependency of it ships in the vsix and
the vulnerable authorization-code path is never executed when packaging. The
extension's published contents and runtime dependency set are unchanged.

`npm run audit:dependencies` audits the complete lockfile and keeps the raw
report. Only this exact advisory, in `@azure/msal-node`, reachable through that
one chain, can be classified as accepted. Any other advisory, a second
`@azure/msal-node` installation, a critical finding, an incomplete audit, or a
count mismatch fails the check. CI stores the raw report, including the accepted
findings.

After an upstream fix is available, upgrade and verify it, then remove this
acceptance and its classification in the same slice. Never broaden the admitted
advisory list to get a passing check.

References: [upstream advisory](https://github.com/advisories/GHSA-xcx6-jx8p-x3h5).
