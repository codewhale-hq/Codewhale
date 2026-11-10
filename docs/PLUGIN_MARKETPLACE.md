# First-party plugin marketplace

Codewhale includes an offline snapshot of the `codewhale` catalog in the same
marketplace store consumed by the terminal, Extensions, recommendations, and
Runtime API. Browse the [public first-party marketplace](https://github.com/codewhale-hq/codewhale-plugin-marketplace)
for current entries and their prerequisites. Its contents can be newer than
the snapshot in your installed CLI. Browsing a catalog does not fetch or
execute a plugin.

```text
/plugin marketplace list
/plugin marketplace show codewhale
/plugin marketplace install codewhale whalewiki
/plugin show whalewiki
/plugin trust whalewiki
```

The last command prints a review. Run its exact confirmation command containing
both full hashes, then `/plugin enable whalewiki`. Review the manifest and
capabilities before trust and enablement. Installation
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

## Catalog freshness and pinned installs

The published v0.10.2 CLI contains the 19-entry catalog from marketplace revision
`2e3a6fba453c9b34d5b1444ab5bd6edc7b119189`. Current source and the public
marketplace include later additions, such as Office and Studio; that does not
make them part of the v0.10.2 snapshot. Use `/plugin marketplace show codewhale`
to inspect the catalog your session actually has.

The bundled catalog is a build-time snapshot. It does not poll the public
repository. To use a separately reviewed catalog revision, obtain a local copy
of the public marketplace, inspect its changes, then add its local document:

```text
/plugin marketplace add codewhale /absolute/path/codewhale-plugin-marketplace/marketplace.json
/plugin marketplace show codewhale
```

Re-adding the same document refreshes its saved contents. If `codewhale` already
names a different local source, remove that catalog first with
`/plugin marketplace remove codewhale`, then add the intended document. Catalog
removal does not remove installed plugins or change their trust.

Repository entries in the bundled snapshot pin an immutable marketplace commit.
Remote entries keep their declared URL and any archive checksum. The current
public Computer Use entry pins a versioned `computer-use.tar.gz` asset and its
SHA-256; it does not follow GitHub's latest-release address. `/plugin update`
re-reads the installed bundle's recorded source, including its pin. It does not
select a new source from a refreshed catalog.

### Changing an installed pin

For a user-scope bundle installed by Codewhale, retain its original source and
any local changes before replacing it: uninstall deletes the installed copy.
After refreshing and reviewing the catalog, this example selects its current
WhaleWiki entry:

```text
/plugin show whalewiki
/plugin disable whalewiki
/plugin uninstall whalewiki
/plugin marketplace install codewhale whalewiki
/plugin trust whalewiki
```

Run the exact hash-bound confirmation printed by the new review, then
`/plugin enable whalewiki`. Do not apply this sequence to built-in or workspace
bundles: update and uninstall accept only user-scope installed bundles. A local
catalog's `path:` source records that local directory, so keep it available for
future `/plugin update` calls. A changed source still needs review and does not
silently inherit an earlier receipt.

## Computer Use across versions

For the current helper with published CLI v0.10.2, follow the [direct MCP setup](https://github.com/codewhale-hq/codewhale-cu-plugin#codewhale-cli-setup), which uses the app’s bundled runtime. Disable an enabled older built-in before using that connection. Direct MCP supplies tools separately from plugin commands and skills.

The published v0.10.2 CLI embeds Computer Use 0.12.1. To inspect that older bundle, start with
`/plugin show computer-use`, review with `/plugin trust computer-use`, run the
printed hash-bound confirmation, then `/plugin enable computer-use`. A catalog
install of the same name reports that the built-in already exists; it does not
replace it with a newer release. Direct installation of a second bundle with
that name is refused. Built-in bundles cannot be updated or uninstalled through
the user-plugin commands.

The current Engine source candidate no longer creates or discovers the embedded
copy. On that layout, install `computer-use` from the catalog as an ordinary
user plugin, then review, trust and enable it. Its old snapshots and receipts
remain intact; a built-in receipt cannot authorize the downloaded bundle.
This source transition does not establish a published CLI migration path.

Computer Use has its own [published releases](https://github.com/codewhale-hq/codewhale-cu-plugin/releases)
and [contribution guide](https://github.com/codewhale-hq/codewhale-cu-plugin/blob/main/CONTRIBUTING.md).
Install the native Mac helper separately using the
[setup guide](https://codewhale.net/computer-use) for system permissions and
Pause/Stop controls. A helper release, a plugin bundle and an Engine release
have separate versions; their presence alone does not qualify a combination.

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
| Computer Use | `codewhale-hq/codewhale-cu-plugin` | Versioned release archive and checksum in the catalog; no current runtime mirror |

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
entries retain their declared source. Updating the bundled catalog requires a
new Core snapshot; users may explicitly add a reviewed local catalog without
rebuilding Core. Neither change retargets an existing pinned install. Push the
reviewed marketplace revision before publishing a Core release that references it. Hosted CI must be green for the
actual published revisions; local checks do not prove a public URL works.

Computer Use source and checks belong to its canonical repository. Publish its
qualified `computer-use.tar.gz` release asset before publishing a catalog that
references that address. Do not copy its runtime or native helper into Core.

Skill wording changes also need behavioral evaluation before claiming better
outcomes. See [Skill evaluation](SKILL_EVALUATION.md).
