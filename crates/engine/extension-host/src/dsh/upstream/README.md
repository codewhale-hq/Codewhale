# DeepSeek Harness composition sources

Pinned to `deepseek-ai/deepseek-harness` commit
`0d1f50007f9bca3f52b06e1c3074fa14d5fb0720`. `UPSTREAM.json` records each
original source hash, current source hash, and explicit adaptation.

Loader, include, and group retain the upstream composition and lifecycle
algorithms. The loader disables the optional native addon probe. Strict host
builds add type annotations and context casts, and two upstream configs extend
the host configuration. `../fiber-state.ts` carries the exact six values of
Cordis 4.0.4's erased const enum. These changed files are marked as adaptations.

Each package's complete MIT license is adjacent to its source and included in
`dist/LICENSES.txt`. The package loader runs only reviewed source closures.
Rust retains session identity, permission decisions, secrets, invocation
admission, and the Engine turn loop.
