# DeepSeek Harness composition sources

Pinned to `deepseek-ai/deepseek-harness` commit
`b830ec950faba0a46dda564be3e2d854afed94f1`. `UPSTREAM.json` records each
original source hash, current source hash, and explicit adaptation.

Loader, include, and group retain the upstream composition and lifecycle
algorithms. The loader disables the optional native addon probe. Strict host
builds add type annotations and context casts, and two upstream configs extend
the host configuration. `../fiber-state.ts` carries the exact six values of
Cordis 4.0.5's erased const enum. These changed files are marked as adaptations.

Each package's complete MIT license is adjacent to its source and included in
`dist/LICENSES.txt`. The package loader runs only reviewed source closures.
Rust retains session identity, permission decisions, secrets, invocation
admission, and the Engine turn loop.

The pin is tag `dsh-v0.2.1-rc.5`. Loader 1.0.6, include 1.0.10 and group
1.0.5 share Cordis 4.0.5. Reviewed composition imports remain confined to the
Rust-approved module closure, and composition writes remain no-ops. Volatile
updates retain the same reviewed Native owner; they do not mint Engine grants.
The separately recorded preset mount audit retains its original commit.
