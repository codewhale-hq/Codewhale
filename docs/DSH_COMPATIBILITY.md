# Published DeepSeek Harness packages in Codewhale

On the `0.10.5` source build (`6f093967ae3`, macOS arm64, default network
policy, no model provider calls):

- **Pinned 33-row Engine lifecycle (`dsh_acceptance.py run`):** **13 of 33**
  packages pass every local Engine lifecycle step (`admission.preview`,
  `install`, `trust`, `enable`, `restart_retained`, `revoke`, `uninstall`, and
  `security.no_key_leak`; provider and desktop slot steps remain `UNAVAILABLE`),
  **3 of 33** match their pinned `BLOCKED` admission refusals
  (`dsh-agent-squad`, `dsh-im`, `dsh-archive-manager`), and **17 of 33** stop
  cleanly at `registry.npmjs.org` network approval (**0** stop at the 2 MiB
  scan limit).
- **Direct extension-host cold-activate (all 36 surveyed tarballs, without
  `npm install`):** **15 of 36** activate cleanly in the extension host,
  registering **18 tools**, **9 commands**, and **2 hooks** (`dsh-image-gen` 6
  tools, `dsh-client-ui-task-board` 8 tools + 1 hook, `modsearch` 2 tools,
  `modlens` 1 tool, `dsh-code-review` 1 tool + 3 commands, `dsh-btw` 3 commands
  + 1 hook, `dsh-pet` 3 commands, plus 8 zero-registration plugins). Of the
  remaining 21: **14** fail only on missing third-party npm packages in the raw
  tarball (zero missing `@deepseek-ai/*` peers prior to npm staging), **6** hit
  explicit policy or runtime boundaries, and **1** (`dsh-web-ui`) has no server
  entry.
- **Prior `0.10.3` scripted-turn check:** `@michengai/dsh-code-review` 1.0.9
  ran its `dsh_code_review` tool end to end in one terminal turn against a
  scripted local model (starting its reviewer sub-agent and returning its
  report). Review quality with a real model was not tested.

This page lists the published DeepSeek Harness (DSH) packages we qualified,
unmodified, in Codewhale. The services and limits behind each row are in
[DSH plugin compatibility](DSH_PLUGIN_COMPAT.md).

## How this was tested

Built from source on 2026-11-24 (`0.10.5` wave-3 build `6f093967ae3`, macOS
arm64, Node runtime). Two checks ran with zero model-provider calls:

1. **Pinned 33-row Engine lifecycle:** `scripts/dsh-plugin-acceptance/dsh_acceptance.py`
   (`selftest` 15/15 passed; `pins` 33/33 SHA-256 verified) drove every pinned
   row through `/v1/apps/plugins` on a throwaway `CODEWHALE_HOME` with
   `--enable extension_host` and default network policy (no `--allow-provider`).
2. **Direct extension-host cold-activate:** each of the 36 SHA-256-pinned
   survey tarballs was extracted without `npm install` and activated directly
   against `crates/engine/extension-host/dist/codewhale-extension-host.mjs`
   with an isolated `data_dir`.

Earlier `0.10.3` npm-approved terminal checks and the scripted local-model turn
for `dsh-code-review` are marked where carried forward. Nothing here was run on
Windows, Linux, Bun, or live Desktop slot mounts.

## What the columns mean

**Status in the terminal** is what happens on this build with the default
network policy:

- **activates**: the extension host logs the plugin as active.
- **fails: …**: import, review, trust and enable succeed, but the plugin does
  not activate, and the host log names what it is missing.
- **stops at import review: …**: `codewhale plugins import dsh` refuses
  before anything is installed.

**Where it belongs** is our reading of the package and its failure:

- **terminal**: it has a headless part a terminal can host.
- **desktop or web only**: its main purpose is a web panel or client UI that
  a terminal cannot show. This means the package was designed for a desktop or
  web host. It does not mean it works in the Codewhale desktop app: none of
  these packages was verified there, and the plugin web server they need does
  not exist yet.
- **refused by policy**: it needs an authority Codewhale keeps in its core,
  and the policy is named.
- **needs a host service not built yet**: it needs a service or module the
  extension host does not provide, named in the row.

**Checked** says where the status comes from: **this build** means the
package was run again on the build described above; **0.10.3 survey** means
the status is from the 0.10.3 run, carried forward because the cause is in
code this change did not touch, with the changed wording of its message
predicted from the code.

## Packages

Counts on `0.10.5` (`6f093967ae3`, default network policy, no provider calls):
in the pinned 33-row Engine lifecycle, **13** pass full local lifecycle, **3**
match pinned `BLOCKED` admission refusals (`dsh-agent-squad`, `dsh-im`,
`dsh-archive-manager`), and **17** stop cleanly at `registry.npmjs.org` network
approval (**0** at the 2 MiB scan limit). In direct extension-host
cold-activate across all 36 tarballs (no `npm install`), **15** activate,
**14** fail only on missing third-party npm packages, **6** hit explicit
policy/runtime boundaries, and **1** (`dsh-web-ui`) has no server entry.

| Package | Version | Status in `0.10.5` (default network policy) | Where it belongs | Checked |
| --- | --- | --- | --- | --- |
| `@michengai/dsh-code-review` | 1.0.9 | passes local Engine lifecycle; cold-activates (1 tool `dsh_code_review`, 3 commands; `/review` renamed to avoid built-in collision; Chinese UI because no host language setting is exposed) | terminal | 0.10.5 build (+ 0.10.3 scripted tool turn) |
| `dsh-anchored-standard` | 1.2.1 | passes local Engine lifecycle; cold-activates and registers nothing (preset installer) | terminal | 0.10.5 build |
| `dsh-plugin-model-proxy` | 0.1.6 | stops at import review: npm network approval (`undici` missing offline). With npm approved (0.10.3): activates and registers nothing | terminal | 0.10.5 build + 0.10.3 npm approved |
| `@michengai/dsh-btw` | 1.3.7 | passes local Engine lifecycle; cold-activates registering 3 commands and 1 hook (`ctx.tools.guard` blocker cleared; `fork` sub-agent provider still not implemented) | terminal | 0.10.5 build |
| `@linxin666/dsh-liangshen` | 0.4.16 | passes local Engine lifecycle; cold-activates (`agentPresets` service now provided; registers no tools/commands) | terminal | 0.10.5 build |
| `@michengai/dsh-automation` | 1.0.22 | passes local Engine lifecycle; direct cold-activate fails on `@deepseek-ai/dsh-user-approval` (`@deepseek-ai/dsh-workspace` blocker cleared) | refused by policy: scheduled runs set approval policy to `never`, and approval authority stays with Codewhale core | 0.10.5 build |
| `@nanmicoder/dsh-agent-teams` | 0.1.29 | passes local Engine lifecycle; direct cold-activate fails: unsupported Harness subagent contract (cannot install retired-member guard) | refused by policy: patches the shared `subagents` service, which plugins may not do | 0.10.5 build |
| `dsh-context` | 0.83.0 | stops at import review: npm network approval (`zod` missing offline). Pure `deriveEventMessage` export is now provided in `0.10.5` | needs npm approval (`zod`) | 0.10.5 build |
| `dsh-whale-widget` | 0.4.2 | passes local Engine lifecycle; cold-activates (0 tools/commands; balance widget UI still requires desktop/web client slot) | desktop or web only | 0.10.5 build |
| `@michengai/dsh-codex-ui` | 2.0.21 | passes local Engine lifecycle; cold-activates (0 tools/commands; UI skin requires desktop/web client slot) | desktop or web only | 0.10.5 build |
| `@linxin666/dsh-client-ui-skill-explorer` | 0.4.16 | passes local Engine lifecycle; cold-activates (0 tools/commands; web panel requires desktop/web client slot) | desktop or web only | 0.10.5 build |
| `@linxin666/dsh-client-ui-git-graph` | 0.4.14 | passes local Engine lifecycle; direct cold-activate fails: requires `subprocess` (core-owned) | refused by policy: `subprocess` belongs to Codewhale core | 0.10.5 build |
| `@linxin666/dsh-client-ui-task-board` | 0.4.16 | passes local Engine lifecycle; cold-activates registering **8 tools** and **1 hook** (`ReasoningEffortId` and `typertGateway`/`workspaceRegistry` blockers cleared) | terminal + desktop/web panel | 0.10.5 build |
| `dsh-image-gen` | 0.11.1 | passes local Engine lifecycle; cold-activates registering **6 tools** (`attachments` blocker cleared) | terminal | 0.10.5 build |
| `dsh-cost-meter` | 1.9.62 | stops at import review: npm network approval (`acorn` missing offline) | needs npm approval | 0.10.5 build |
| `@anionex/dsh-vision-toolkit` | 0.1.49 | stops at import review: npm network approval (`tar` missing offline) | needs npm approval | 0.10.5 build |
| `@dfy-plugins/dsh-image-generation` | 0.1.4 | stops at import review: npm network approval (`@dfy-plugins/resource-core` missing offline) | needs npm approval | 0.10.5 build |
| `@goodandready/dsh-image-gen` | 0.12.27 | stops at import review: npm network approval (`sharp` missing offline) | needs npm approval | 0.10.5 build |
| `@michengai/dsh-agency-agents` | 1.2.27 | stops at import review: npm network approval (`zod` missing offline) | needs npm approval | 0.10.5 build |
| `@liustack/modsearch` | 5.10.12 | stops at import review: npm network approval; direct cold-activate passes registering **2 tools** | terminal (needs npm approval on import) | 0.10.5 build |
| `@liustack/modlens` | 3.27.3 | stops at import review: npm network approval; direct cold-activate passes registering **1 tool** | terminal (needs npm approval on import) | 0.10.5 build |
| `@michengai/dsh-im-connect` | 1.0.34 | stops at import review: npm network approval (`undici` missing offline) | needs npm approval | 0.10.5 build |
| `dsh-mnemon` | 0.5.43 | stops at import review: npm network approval (`dsh-mnemon-source-memory-spaces` missing offline) | needs npm approval | 0.10.5 build |
| `@deepseek-harness-tui/dsh-tui` | 0.19.0 | stops at import review: npm network approval (`emoji-regex` missing offline) | needs npm approval | 0.10.5 build |
| `@michengai/dsh-skills-manager` | 1.4.0 | stops at import review: npm network approval (`fflate` missing offline) | desktop or web only | 0.10.5 build |
| `dsh-pocket` | 2.12.0 | stops at import review: npm network approval; direct cold-activate passes (0 tools/commands) | desktop or web only | 0.10.5 build |
| `dshmarket` | 1.73.17 | stops at import review: npm network approval (`js-yaml` missing offline) | desktop or web only | 0.10.5 build |
| `dsh-better-sidebar` | 0.30.1 | stops at import review: npm network approval (`ws` missing offline) | desktop or web only | 0.10.5 build |
| `@linxin666/dsh-web-all` | 0.4.16 | direct cold-activate passes (0 tools/commands; not in 33-row `pins.json`) | desktop or web only | 0.10.5 build |
| `dsh-pet` | 0.4.7 | stops at import review: npm network approval (2 MiB scan-limit refusal cleared); direct cold-activate passes registering **3 commands** | desktop or web only | 0.10.5 build |
| `dsh-univer-office` | 0.4.0 | stops at import review: npm network approval (2 MiB scan-limit refusal cleared; `@puppeteer/browsers` missing offline) | desktop or web only | 0.10.5 build |
| `@changfenhuang/dsh-genui` | 0.16.0 | passes local Engine lifecycle (2 MiB scan-limit refusal cleared); direct cold-activate fails: system prompt sections are only available on a child agent's `agent/created` context | refused by policy: root-agent system prompt sections | 0.10.5 build |

Pinned `BLOCKED` admission refusals (all 3 verified matching exact pinned basis
in `0.10.5`): `dsh-agent-squad` (direct cold-activate also refused on root
`systemPrompt.section`), `dsh-im` (direct cold-activate passes with 0
registrations), and `dsh-archive-manager` (direct cold-activate throws
`TypeError`). Client-only without server entry: `DamonKoy-dsh-web-ui`
(`dsh-web-ui` 0.1.1).
