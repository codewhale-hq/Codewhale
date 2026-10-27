# Third-party notices

Codewhale is MIT licensed (see [`LICENSE`](../LICENSE)). Portions of it are
adapted from other open-source projects. Their copyright and permission
notices are reproduced here, and each adapted file carries an attribution
header pointing back to its origin.

Rust crate dependencies are covered by `cargo deny` (see [`deny.toml`](../deny.toml))
and are not duplicated here; this file records source-level adaptations, where
the licence obligation travels with the code rather than with a package.

---

## pi-mono — credential resolution design

- **Project:** pi-mono (<https://github.com/earendil-works/pi-mono>)
- **Author:** Mario Zechner
- **Licence:** MIT
- **Used in:** `crates/runtime/src/credentials/` and
  `crates/tui/src/config/credential_resolve.rs`
- **What was taken:** the design of `packages/ai/src/auth/` — one type-tagged
  credential per provider, `modify` as the only serialized write path, a single
  stated precedence rule enforced in one place, an injectable auth context, and
  a resolution result that names its source. This is a design port into
  idiomatic synchronous Rust over Codewhale's existing stores, not a
  line-for-line copy; several doc comments are adapted closely.

```
MIT License

Copyright (c) 2025 Mario Zechner

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## Other source-level adaptations

These predate this file. Each already names its origin in a header comment on
the adapting file; they are listed here so the set is discoverable from one
place. Their licence text has not been reproduced below — that is a gap, not a
claim that none is required.

| Codewhale file | Adapted from |
| --- | --- |
| `crates/tui/src/tui/frame_rate_limiter.rs` | `codex-rs/tui/src/tui/frame_rate_limiter.rs`, [openai/codex](https://github.com/openai/codex) |
| `crates/tui/src/tui/display_refresh.rs` | Grok CLI's host display-refresh probe |
| `patches/unicode-width-0.2.2/` | vendored patch; upstream `LICENSE-MIT` retained in-tree |
| `patches/crossterm-0.29.0/` | vendored patch (surrogate key-up handling in `src/event/sys/windows/parse.rs`, the change proposed upstream as crossterm-rs/crossterm#1073); upstream MIT `LICENSE` retained in-tree |
