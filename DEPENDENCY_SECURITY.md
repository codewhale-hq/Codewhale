# Extension packager dependency security

The VS Code extension's packaging toolchain uses `@vscode/vsce` →
`@azure/identity` → `@azure/msal-node`. MSAL Node versions before 7.1.0 are
affected by [GHSA-xcx6-jx8p-x3h5](https://github.com/AzureAD/microsoft-authentication-library-for-js/security/advisories/GHSA-xcx6-jx8p-x3h5).
The extension manifest overrides Azure Identity's MSAL Node dependency to
`^7.1.2`; its lockfile resolves 7.1.2. Keep the override until Azure Identity
requires a patched version itself.

`npm run audit:dependencies -- --report <path>` audits the complete lockfile
and keeps the raw report. Every reported vulnerability blocks the check.
Incomplete reports, count mismatches and critical findings also fail. There
is no advisory exception.

After changing this dependency, materialize the lockfile with `npm ci`, run
the extension's existing `npm test` checks and build a local package with
`npm run package`. Packaging uses `vsce package --no-dependencies`, which
excludes development dependencies from the VSIX. A local package build does
not establish that a publishing authentication flow works; verify that flow
only when publication is separately authorized.
