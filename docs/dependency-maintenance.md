# Dependency maintenance

Reviewed against the 0.9.13 Core dependency graph on September 8, 2026.
This records maintenance decisions, not a security clearance or release test.

The terminal Markdown renderer uses Syntect's embedded syntax and theme dumps
with the existing fancy-regex backend. It does not load external syntax YAML,
theme plists, or emit Syntect HTML. Selecting those features explicitly removes
the unused `yaml-rust`, `plist`, and `quick-xml` dependency path. The embedded
YAML language grammar remains available for highlighting YAML code blocks.

`derivative`, `fxhash`, `paste`, and `ttf-parser` are absent from the Core lockfile.
Their obsolete advisory exceptions are removed so a future reintroduction is
visible to the advisory checks. No `cargo-audit` advisory is ignored.

The remaining `cargo-deny` maintenance exception is
[RUSTSEC-2025-0141](https://rustsec.org/advisories/RUSTSEC-2025-0141.html)
for `bincode` 1.3.3. Syntect 5.3.0 requires it to deserialize its compiled-in
syntax and theme assets and for its parser's internal lazy-context encoding.
Codewhale does not pass arbitrary external dump files to these loaders. The
maintenance warning remains visible in `cargo-audit`; the lack of an identified
exploit in this advisory does not guarantee safety.

Revisit the exception when upgrading or replacing Syntect, or before adding
external dump, grammar, or theme loading. Remove it when the dependency leaves
the graph. A replacement must preserve embedded language/theme coverage,
multiline highlighting and error recovery, and pass the existing Markdown
renderer tests with a resolver-verified lockfile. Adding ignores for new
advisories requires a separate assessment.

The Apps desktop lockfile and its platform dependencies are a separate graph;
this Core cleanup does not resolve their maintenance or GLib qualification work.

Automatic Git review and workspace polling require the runtime configuration
override interface introduced in [Git 2.31](https://github.com/git/git/blob/master/Documentation/RelNotes/2.31.0.adoc). The shared read authority probes the
resolved absolute executable once per process and pins subsequent commands to
that executable even if PATH changes. It refuses automatic reads if the executable
cannot honor those overrides; explicit user Git writes retain their existing behavior.
The porcelain-v1 display fallback does not waive this safety requirement.
The absolute path fixes executable search selection; it does not authenticate an
executable replaced at that path by the same local user.

Local fixtures cover native modern Git and an emulated executable that ignores
runtime overrides. They do not qualify a real older-Git installation, partial
clones, or all platform-specific Git behavior.

## VS Code extension packager

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
