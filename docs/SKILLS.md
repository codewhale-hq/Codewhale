# Skills Manager

In the terminal, `/skills` opens **Extensions → Skills**. Enter on a skill opens the dedicated manager, also available directly through `/skills manage`, for existing install, update, remove, and trust operations. `/skill <name>` still activates a skill, and the explicit inspection, remote, sync, and suggestion subcommands remain available.


> 阅读简体中文版：[zh_hans/SKILLS.md](zh_hans/SKILLS.md)

Skills are reusable `SKILL.md` instruction packs. Codewhale discovers them from
several roots, but **only Codewhale-owned directories are writable**. The unified
`/skills manage` manager is the interactive surface for audit and mutation; slash
aliases share the same write path.

For Claude Code plugin boundaries, see [CLAUDE_PLUGIN_COMPAT.md](CLAUDE_PLUGIN_COMPAT.md).
For `skills_dir` and `[skills]` config keys, see [CONFIGURATION.md](CONFIGURATION.md).

## Architecture (four layers)

| Layer | Role |
| --- | --- |
| **Root catalog** | Single source of precedence and ownership (`SkillRootCatalog`). |
| **Audit** | Read-only, unmerged on-disk inventory (status, digest, actions). |
| **Mutation controller** | Only writer for install / import / update / remove / trust. |
| **Skills manager view** | TUI: emits events only; never writes files itself. |

Runtime discovery (`SkillRegistry`) still merges skills for the model. Audit
intentionally does **not** merge — it shows every on-disk copy so conflicts and
shadowing stay visible.

## Ownership and roots

**Writable (Codewhale-owned)**

| Scope | Path |
| --- | --- |
| Project | `<workspace>/.codewhale/skills/` |
| Global | `~/.codewhale/skills/` |

**Read-only compatible** (discover / import source only — never mutated in place)

Examples: `<workspace>/.agents/skills`, `./skills`, `.claude/skills`,
`.cursor/skills`, `.opencode/skills`, `~/.agents/skills`, `~/.claude/skills`,
and similar harness layouts.

**Audit-only (not runtime-active)**

- Flat `<workspace>/skills` is an audit candidate until `[skills] flat_workspace_root = true`; an explicit `skills_dir` can also select it.
- `.codex/skills` appears in **compatible** audit scans so operators can see it.
  It does **not** join the runtime discovery set.

Configured `skills_dir` that is not one of the owned Codewhale roots stays
read-only. Discovery and the manager can list it; mutations still target owned
project/global roots only.

Within each scope, the owned `.codewhale/skills` root wins name collisions.
Project order is `.codewhale`, `.agents`, `.claude`, `.opencode`, `.cursor`, then
an opted-in flat `skills` root. Global order is `.codewhale`, `.agents`, `.claude`,
then legacy `.deepseek`. Project roots precede global roots. Shadowing warnings
name both copies, so install and update select the owned copy in that scope.

Every root inside the workspace — owned, compatible, or a configured
`skills_dir` that resolves there — loads only once the workspace is trusted
(`/trust on --save`). Until then discovery names the skipped directories in its
warning, and the session's skills directory falls back to the global one.

## Slash commands

| Command | Behavior |
| --- | --- |
| `/skills` | Opens Extensions → Skills (owned-only scan, **no network**). |
| `/skills manage` | Opens the dedicated Skills Manager for audit and mutation. |
| `/skills <prefix>` | Text list filtered by name prefix. |
| `/skills inspect` | Text discovery mode, searched directories, and source paths. |
| `/skills --remote` | Explicit registry listing (network). |
| `/skills suggest <task>` | Rank up to three remote skills for a task, with matching evidence and an explicit install command (network; no install). |
| `/skills sync` | Explicit registry → local cache sync (network). |
| `/skill <name>` | Activate a skill for the next turn. |
| `/skill install [--project\|--global] <spec>` | Install via mutation controller. |
| `/skill update [--project\|--global] <name>` | Update a managed skill from its registry provenance. |
| `/skill uninstall [--project\|--global] <name>` | Remove a managed skill. |
| `/skill trust [--project\|--global] <name>` | Write digest-bound advisory trust. |

Notes:

- There is **no** `/skills audit` subcommand. Use the manager (and `c` to toggle
  compatible roots) or `/skills inspect` for discovery details.
- Bare `/skill install <spec>` (no scope flag) installs into the Codewhale
  **global** owned root.
- `/skills suggest` only reads the curated registry through the existing
  network policy. It never downloads, trusts, enables, or activates a skill;
  each result gives a separate `/skill install <name>` command for the user to
  choose.
- If the same name exists in both project and global owned roots, update /
  uninstall / trust require `--project` or `--global`.
- If a name exists only under a compatible external root, writes are refused;
  import it through `/skills manage` instead of editing harness directories.

## Skills Manager (TUI)

Direct manager path: type `/skills manage` and confirm. The surface is zero-network on
open (owned-only audit).

| Key | Action |
| --- | --- |
| `↑`/`↓` or `j`/`k` | Move selection |
| `Enter` | Primary available action / confirm pending prompt |
| `i` | Import (external → owned) |
| `u` | Update (managed + registry provenance) |
| `r` | Remove (managed; confirms first) |
| `t` | Trust (managed; digest-bound) |
| `s` | Toggle import target: project ↔ global |
| `c` | Toggle scan: owned-only ↔ compatible (still local disk only) |
| `Esc` | Cancel confirm, or close the manager |

The view never calls install helpers or touches the filesystem. It emits a
mutation request; the host runs the controller, shows a receipt, and rebuilds
the inventory.

## Bundled catalog tiers

Codewhale presents its shipped skills in two compact tiers so agentic workflows
are not buried under document and integration helpers:

- **Core agentic** — planning, implementation, debugging, review, verification,
  delegation, Fleet, release, and `best-of-n` comparison workflows.
- **Format & tooling** — document formats, data visualization, frontend and web
  testing, and skill/plugin/MCP authoring helpers.

Workspace, user, and compatible-harness skills stay labeled **custom**; Codewhale
does not guess their intent from their name. The shipped pack also does not
advertise capabilities the runtime lacks. In particular, image understanding
is available, but an image-generation skill is not bundled until a real
image-generation tool exists.

Repository-maintenance and release-operator helpers (the `gh-*` skills and
`codew-release-qa-sweep` under [`skills/`](skills/README.md)) are **not** part
of the end-user starter pack and are never auto-installed; a catalog-matrix
test pins that boundary. Shipping them as an optional bundle is plugin-delivery
work tracked separately in
[#4836](https://github.com/codewhale-hq/CodeWhale/issues/4836).

### Invocation and alias metadata

Bundled and user skills may declare two runtime-routing fields in frontmatter:

| Field | Meaning |
| --- | --- |
| `invocation: model+user` | The default; the skill appears in the model's compact catalogue and can be loaded by the model or user. |
| `invocation: explicit-only` | The skill remains loadable by an explicit name, but is omitted from the model catalogue so opt-in instructions do not become ambient context. |
| `aliases-for: name, other-name` | Additional lookup names for the same canonical skill. Aliases are not separate catalogue entries and do not duplicate prompt content. |

`disable-model-invocation: true` makes a skill explicit-only. `user-invocable:
false` hides it from user menus and refuses explicit activation while preserving
model selection. Setting both disables both paths. Boolean spellings `true/false`,
`yes/no`, `on/off`, and `1/0` are accepted; invalid policy booleans fail closed.
The model's catalog, list/query, and `load_skill` enforce model eligibility; the
user's slash and command palettes enforce user eligibility. `argument-hint` is
shown beside user-facing descriptions. `when_to_use` joins the routing description
as `Use when:`.

The runtime and installer share one frontmatter validator. Runtime accepts missing
descriptions and heading-only Markdown with warnings; installation requires a
frontmatter block, a nonempty description, and a path-safe name. A name/directory
mismatch warns without renaming the file. Nested `metadata` stays nested; flow
and block lists share the same interpretation. `license`, `compatibility`,
`metadata`, localized descriptions, and `x-*` extension keys are accepted silently.
Unknown keys warn once. `allowed-tools` / `disallowed-tools`, `model`, `context`,
and `agent` warn because they grant no tool, approval, provider, or fork authority.
The existing workspace-trust and reviewed-plugin byte/hash gates still apply.

Missing or unknown invocation values retain the historical `model+user`
behavior. Canonical names win over aliases when a collision exists. Loading a
skill reports its canonical invocation and aliases so receipts remain
inspectable.

### Non-ASCII names and saved activation

ASCII names keep their existing command spelling. A name containing non-ASCII
characters gets a stable ASCII ID: a shortened old slug plus 32 hexadecimal
SHA-256 digits, at most 64 characters per skill-name segment. The hash uses
trimmed UTF-8 with ASCII case folding; it does not transliterate or merge Unicode
normalization forms. Unqualified raw names and those IDs select the same body.
Package directories stay in place. Qualified lookup requires the declared canonical
namespace, with ASCII case folding and no punctuation folding: `Team.Plugin:技能`
cannot select a skill in `team-plugin`.

Previously disabled lossy names such as `skill` or `pdf` continue to suppress
every corresponding renamed skill. Enabling one exact catalog ID enables only
that identity, including a literal ASCII skill named `skill`; it does not enable
its formerly colliding siblings. Toggle requests use the exact ID returned by
`GET /v1/skills`. Plugin bundle trust remains a separate gate.

Activation still uses one `skills_state.toml` file and its `disabled` array.
Reserved `!codewhale-skill-state:1:*` entries preserve legacy veto history and
exact enable choices through older writers, using the same lock and atomic
write. Listing/discovery do not rewrite the file. Unknown versions or malformed
reserved entries are errors and are left untouched; existing recovery behavior
keeps native skills available but hides reviewed plugin skills when policy
cannot be read.

This is **not simultaneous-version activation compatibility**. v0.10.0 readers
cannot enforce new per-identity disables, and their lossy or no-op toggles cannot
express every new choice. Upgrade every runtime sharing the state directory
before relying on consistent controls. Retaining marker strings through an old
write does not give that old binary the new identity semantics.

### Starter-pack parity decisions

The v0.9.2 parity audit in [#4698](https://github.com/codewhale-hq/CodeWhale/issues/4698)
compared the five `xai-grok-memory` / `xai-grok-shell` reference skills with
the actual Codewhale bundle. This is a decision matrix, not a request to copy
reference text or advertise unsupported tools:

| Reference skill | Codewhale decision | Runtime grounding |
| --- | --- | --- |
| `check-work` | Canonical alias/compatibility mapping to `verify` | `verify` is the shipped evidence-collection workflow. |
| `code-review` | Canonical alias/compatibility mapping to `review` | `review` is the shipped read-only correctness workflow. |
| `create-skill` | Canonical alias/compatibility mapping to `skill-creator` | `skill-creator` is the shipped authoring workflow. |
| `help` | Bounded `invocation: explicit-only` router, not an ambient manual | Routes to `/help`, `/skills`, `/config`, `doctor`, and the installed `docs/` tree; it embeds no manual text. |
| `imagine` | Intentionally out of scope | Codewhale has no image-generation/edit tool, so the starter pack must not advertise one. |

Notes on the two non-alias decisions:

- **`help`** ships as a bundled skill but is `explicit-only`, so
  it never appears in the model catalogue and costs zero ambient prompt budget.
  Its body is a routing card — which surface owns which fact — and explicitly
  forbids pasting a command list or settings table into context. A checked
  invariant keeps it under 80 lines and requires it to name the `/help`,
  `/skills`, `/config`, and `doctor` surfaces.
- **`imagine`** stays out. The shipped runtime exposes image *understanding*,
  not image generation or edit, so no bundled skill may advertise it. The
  catalog matrix asserts that `imagine`, `image`, and `image-gen` are absent
  from the bundle and resolve to nothing.

No reference skill body is copied by this compatibility slice. The explicit
aliases and invocation metadata are bounded routing facts; the full skill body
still enters context only through `load_skill`.

### Catalog fixture matrix (provider-free)

[`crates/tui/assets/skills-catalog-matrix.json`](../crates/tui/assets/skills-catalog-matrix.json)
is an **authored** expectation table covering every bundled skill: canonical
name, tier, invocation, aliases, whether it renders as an ambient catalogue
entry, and which of its aliases are shadowed by another canonical name. The
tests in `crates/skills/src/catalog_matrix.rs` assert a bijection between
that fixture and `BUNDLED_SKILLS`, so the shipped pack cannot change without an
explicit fixture update.

What those tests do and do not claim:

- They validate **deterministic registry / catalog / resolver behavior**:
  install, parse, eligibility, explicit load, non-activation, alias resolution,
  explicit-only exclusion, collision precedence, and prompt budget.
- They validate **nothing about semantic LLM routing**. Whether a model chooses
  `debug` for a stack trace is a live-provider question; see
  [LIVE_SMOKE.md](LIVE_SMOKE.md).

Collision and prompt-budget invariants asserted today:

| Invariant | Meaning |
| --- | --- |
| Canonical wins | A canonical bundled name always beats another skill's alias (`docx` → `docx`, never `documents`). |
| Single alias owner | No two bundled skills may claim the same alias. |
| No duplicate entries | Each canonical name renders at most one catalogue line; aliases render zero. |
| Budget headroom | The shipped pack alone renders under the window-scaled skills budget (25 600 chars at the default 128k window; 2 400-char floor) with **no** "additional skills omitted" line, so user skills are never silently displaced. |
| No context poisoning | Descriptions stay single-line and respect `[skills] listing_max_description_chars` (default 400) before entering the prompt. |

### Locale-aware routing metadata

`description_<tag>` frontmatter is supported (exact tag, then primary subtag,
then the canonical description — with Traditional Chinese excluded from the
Simplified `zh` fallback). **No bundled skill ships a localized routing
description**, and none is fabricated. The shipped contract is therefore an
explicit, tested fallback:

- For every skill in the bundle × every locale in `Locale::shipped()` — all 15
  of `en`, `ja`, `zh-Hans`, `zh-Hant`, `pt-BR`, `es-419`, `vi`, `ko`, `ca`,
  `de`, `fr`, `id`, `hi`, `ru`, `uk` (`crates/localization/src/lib.rs:70-88`) —
  `description_for_locale` returns the canonical English description.
- The rendered catalogue block is byte-identical across all shipped locales.
- Exact-tag match, primary-subtag fallback (`pt-BR` → `description_pt`), and
  English fallback are covered against a synthetic authored fixture, so the
  resolution paths stay tested even while the bundle itself is English-only.

If a bundled skill later ships localized routing metadata, the parity test
fails until source-backed coverage is added for it — the fallback contract
cannot silently absorb a translation.

## Audit statuses

Each audited row carries precedence and relationship flags:

| Status | Meaning |
| --- | --- |
| **Active** | Highest-precedence copy for that canonical name in the scan. |
| **Shadowed** | Same name exists at a higher-precedence root. |
| **Duplicate** | Same canonical name and same package digest as another copy. |
| **Conflict** | Same canonical name, different package digest. |

External skills with no owned peer (and a valid digest) are **import
candidates**. Externals that conflict with or exactly duplicate an owned copy
can still offer Import — duplicate → already present; conflict → confirm replace
in the selected import scope.

## Provenance and markers

Managed installs write schema **v2** metadata under the skill directory:

**`.installed-from` (v2)** — written last on successful install/import:

```json
{
  "schema_version": 2,
  "spec": "github:owner/repo",
  "url": "https://…",
  "source_checksum": "…",
  "content_digest": "…",
  "installed_name": "my-skill",
  "registry_version": null
}
```

- `content_digest` is a bounded package tree hash (not SKILL.md alone).
- Display of URLs strips userinfo, query, and fragment.
- Imports use a local `import:…` provenance and **cannot** be updated from a
  registry; re-import or remove them instead.
- Legacy v1 markers are recognized as managed with
  `LegacyMetadataUnknown` integrity until refreshed.

**`.trusted` (v2)** — advisory, digest-bound:

```json
{
  "schema_version": 2,
  "content_digest": "…"
}
```

Trust records review intent. It does **not** sandbox the skill or auto-approve
tools. Content updates clear trust so a stale marker cannot outlive the bytes.

Manual skills (owned root, no managed marker) are visible but not
update/remove/trust through the managed actions.

## Package digest and safety

Audit and mutation share a bounded package digest:

- Regular files only; symlinks that escape the skill root or cycle → fail closed.
- Caps on total size, file count, and depth.
- Mutations re-check an expected digest before write (TOCTOU).
- Import/replace keeps a `.bak` until digest + marker finalize succeed; failure
  restores the previous owned package.

## Readiness

The audit model has a readiness field and optional provider hook for a future
readiness cache ([#4407](https://github.com/codewhale-hq/CodeWhale/issues/4407)).
Today, when no cache is wired, readiness is always **`Unknown`**. The manager
does not run readiness probes and does not block mutations on readiness.

## Config knobs

Parents, fresh and nested children, and `/context` use the same ambient-index renderer. `[skills] listing_max_description_chars` controls the per-entry ceiling (1–40,000; default 400). `listing_budget_fraction` controls the share of the resolved route window (greater than 0 and at most 1; default 0.05), estimated at four characters per token. The total index retains a 2,400-character floor and 40,000-character ceiling. Explicit profile values override inherited values independently. Changing a route recomputes its budget; it does not silently change the description setting.

The renderer favors `Use when:` triggers, shortens descriptions before falling back to names, and reports shortened/omitted entries. Descriptions exceeding the configured cap without a trigger also produce a load warning. `/skills inspect` shows a localized note with the same effective cap, and `/context` repeats the loss count. `load_skill name="list"` and exact-name loading retain full text. These limits govern ambient routing metadata, not skill bodies or external MCP resource descriptions.

```toml
# Optional override for discovery preference (not automatically a write target
# unless it is the Codewhale project/global owned path).
skills_dir = "/path/to/skills"

[skills]
# When true, runtime discovery skips cross-tool roots (.claude, .agents, …).
# Owned Codewhale roots and an explicit skills_dir override still apply.
scan_codewhale_only = false
listing_max_description_chars = 400
listing_budget_fraction = 0.05

# Optional registry / install size overrides used by --remote, sync, and install.
# registry_url = "https://…"
# max_install_size_bytes = 5242880
```

See [CONFIGURATION.md](CONFIGURATION.md) for the full config surface.


### Bundled skills

Use `[skills.bundled]` independently of the discovery directory:

```toml
[skills.bundled]
install = "auto"
selected = []
disabled = ["debug"]
explicit_only = ["handoff"]
```

`auto` preserves the generation-marker and deletion-as-opt-out behavior. Setting top-level `skills_dir` disables automatic bundle installation in this mode. `all` installs enabled missing names even with a current marker or a discovery override, writing only to the owned global root (`$CODEWHALE_HOME/skills`, normally `~/.codewhale/skills`). `none` writes no files and leaves the marker untouched. Discovery continues in every mode.

An empty `selected` list enables the whole bundle; a nonempty list limits installation and discovery. `disabled` wins and ignores the bundled copy before name resolution, so a compatible root can supply the same name. `explicit_only` removes an enabled bundled copy from the model index and model `load_skill`, while retaining explicit `/skill <name>` invocation. Canonical names and their compatibility aliases share policy: `documents` maps to `docx`, `presentations` to `pptx`, and `spreadsheets` to `xlsx`.

These lists apply only to recognized shipped copies in the owned global root. Edited files, managed installs with `.installed-from`, project copies and compatible-root copies keep their own behavior. Bundle settings never delete skill files, including retired pack entries. `all` preserves edited and managed bodies. A marker that is a directory or symlink is refused without deleting its contents. Profile fields merge independently; an explicit empty list clears the inherited list.

`codewhale doctor` and its JSON output report mode, marker generation, installed and missing names, disabled and explicit-only counts, shadowing, unknown names and ignored explicit-only entries. `/skills inspect` adds each bundled path and its selection reason. Unknown config names produce a doctor warning; they do not name a write target.

## Operator checklist

1. Prefer `/skills manage` for day-to-day management; keep `--remote` / `sync` explicit.
2. Never hand-edit `.claude` / `.agents` / `.cursor` trees to “install” for
   Codewhale — import into `.codewhale/skills` instead.
3. Treat `.trusted` as advisory documentation of review, not a security boundary.
4. After registry updates that change content, re-trust if you still want the
   advisory marker.
5. Dual project+global copies of the same name need an explicit scope flag on
   CLI mutations.
