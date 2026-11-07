<!-- Thanks for the PR! English or 中文 are both fine. Short is good. -->

## What and why

<!-- What does this change, and why? A few sentences is enough. -->

## Related issue (optional)

<!-- Closes #N if this finishes it, Refs #N if related. Skip it if there is none.
Please do not write a closing word inside a negation ("this does not close #N"):
GitHub closes the issue anyway. -->

## How I tested it

<!-- Commands you ran and what they showed. Test what you changed, for example
`scripts/dev-test.sh tui <filter>`. A few quick checks run on your PR; if one
fails for a reason unrelated to your change, a maintainer will deal with it.
For visible UI changes, add a screenshot or recording. -->

## Checklist

- [ ] One focused change, started from current `main` (no need to rebase when `main` moves)
- [ ] `cargo fmt --all` passes, and tests cover new or changed behavior
- [ ] If this adds or changes a user-facing feature, I updated its docs and its row in `docs/features.toml` (`cargo test -p codewhale-tui --test feature_registry` checks it)
