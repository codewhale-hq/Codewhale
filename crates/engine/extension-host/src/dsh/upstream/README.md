# DeepSeek Harness composition sources

Pinned to `deepseek-ai/deepseek-harness` commit
`66491b05e3f05ad35bcdd4f6cd7ed26041d9e3db`. `UPSTREAM.json` records each
original source hash, current source hash, and explicit adaptation.

Loader, include, and group retain the upstream composition and lifecycle
algorithms. The loader disables the optional native addon probe. Strict host
builds add type annotations and context casts, and two upstream configs extend
the host configuration. `../fiber-state.ts` carries the exact six values of
Cordis 4.1.0's erased const enum. These changed files are marked as adaptations.

Each package's complete MIT license is adjacent to its source and included in
`dist/LICENSES.txt`. The package loader runs only reviewed source closures.
Rust retains session identity, permission decisions, secrets, invocation
admission, and the Engine turn loop.

The pin is tag `dsh-v0.2.2-rc.1`. Loader 1.1.0, include 1.1.0 and group
1.1.0 share Cordis 4.1.0. Include now carries upstream `journal.ts` and
`patch.ts`; the host imports only the pure patch application and YAML schema.
Tree mutations report through `commit(change)`, which reviewed compositions
implement as a no-op, so a reviewed composition is never rewritten. Bare
specifiers keep the native import fallback rather than upstream's
`.cordis/resolve.mjs` helper, and reviewed composition imports remain confined
to the Rust-approved module closure. Volatile updates retain the same reviewed
Native owner; they do not mint Engine grants. The separately recorded preset
mount audit retains its original commit.
