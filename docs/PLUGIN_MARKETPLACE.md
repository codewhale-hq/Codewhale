# First-party plugin marketplace

Codewhale includes an offline snapshot of the `codewhale` catalog in the same
marketplace store consumed by the terminal, Extensions, recommendations, and
Runtime API. It lists Computer Use, WhaleWiki, Whalesong, Cloudflare Docs, the
Codewhale skill bundle, and Codewhale for Chrome (Chromewhale). Browsing does
not fetch or execute anything.

```text
/plugin marketplace list
/plugin marketplace show codewhale
/plugin marketplace install codewhale whalewiki
/plugin show whalewiki
/plugin trust whalewiki
/plugin enable whalewiki
/plugin update whalewiki
```

Review the manifest and capabilities before trust and enablement. Installation
uses the existing size-limited, traversal-safe installer and starts disabled
and untrusted. An update with changed bytes requires review again. Official
catalog provenance grants no execution or network permission. Removing the
catalog persists the choice and leaves installed plugins untouched; a locally
added catalog named `codewhale` takes precedence over the bundled snapshot.

The bundle source uses a gzip tarball URL with `#path=plugins/whalewiki` (or
`#path=skills`). The fragment selects exactly one bundle inside the shared
repository archive. Only that subtree is installed. Empty paths, traversal,
ambiguous roots, links, oversized archives, and changed plugin identities are
rejected. The install receipt preserves the source, including its selector,
so `/plugin update` retains the same bundle selector and reviewed revision.

## Computer Use across upgrades

Computer Use is an ordinary independently released plugin. Install it from
Connectors or `/plugin marketplace install codewhale computer-use`, review its
capabilities, then enable it. Install the signed Mac helper from
[Computer Use setup](https://codewhale.net/computer-use) for native permission
setup, trust and Pause/Stop controls.

The catalog uses the stable `computer-use.tar.gz` asset on the canonical
repository's latest release. `/plugin update computer-use` downloads that source;
changed bytes require review again. Later CU releases need no Engine rebuild.
Older Engine versions embedded a CU copy. Their snapshots and receipts remain
intact, but the current Engine does not discover them. Install and review the
ordinary plugin once; a built-in receipt cannot authorize a new downloaded
bundle. Existing manually installed or pinned bundles keep their normal source
and review rules.

## Chromewhale

Chromewhale (listed as "Codewhale for Chrome") is in the catalog bundled with
Core from marketplace revision `ae3dd22` on, where its checks pass on macOS,
Linux and Windows. It is a developer preview: installing the plugin does not
install the browser side, so you load its bundled Chrome extension unpacked
yourself. It then reads and acts on the tab you are looking at, one granted
site at a time. Like every catalog entry, it installs disabled and untrusted
until you review it.

## Keeping the repositories current

| Content | Authoritative source | Copies to check |
| --- | --- | --- |
| Catalog, WhaleWiki, Whalesong, Cloudflare Docs | `codewhale-hq/codewhale-plugin-marketplace` | Core catalog snapshot |
| Bundled skills | Core active catalog and `crates/tui/assets/skills` | Marketplace `skills` and `skills/upstream.json` |
| Computer Use | `codewhale-hq/codewhale-cu-plugin` | Stable release archive; no runtime mirrors |

The `Marketplace connection` workflow validates the exact catalog revision on
catalog changes. Its weekly and manual runs compare the current public
marketplace, bundled snapshot, and skills. Drift fails
the check with a maintenance instruction; it does not rewrite user installs
or grant new permissions. It uses read-only repository access.

For each intentional update, review the upstream diff, synchronize the
source-owned copies, and run the marketplace checks:

```sh
# From codewhale-plugin-marketplace, with sibling source checkouts:
# After committing canonical skill changes, when intentionally updating skills:
npm run sync:skills
npm run check -- --core ../codewhale
npm test && npm run check:web
```

Commit the reviewed marketplace changes, then update Core from that committed
revision (the generator never copies an uncommitted marketplace document):

```sh
# From codewhale:
python3 scripts/sync-marketplace.py --marketplace ../codewhale-plugin-marketplace
python3 scripts/sync-marketplace.py --marketplace ../codewhale-plugin-marketplace --check
npm test && npm run check:web
```

Review the generated snapshot and rebuild Core. Its provenance records the
exact marketplace commit. Local bundle URLs pin that immutable revision; remote
entries retain their declared source. Updating catalog metadata requires a new
Core snapshot; updating a remote plugin at its stable release URL does not. An
existing pinned install does not silently follow `main`. Push the
reviewed marketplace revision before publishing a Core release that references it. Hosted CI must be green for the
actual published revisions; local checks do not prove a public URL works.

Computer Use source and checks belong to its canonical repository. Publish its
qualified `computer-use.tar.gz` release asset before publishing a catalog that
references that address. Do not copy its runtime or native helper into Core.

Skill wording changes also need behavioral evaluation before claiming better
outcomes. See [Skill evaluation](SKILL_EVALUATION.md).
