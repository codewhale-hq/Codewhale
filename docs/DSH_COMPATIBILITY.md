# Published DeepSeek Harness packages in Codewhale

Of the 32 published DeepSeek Harness packages we tried unmodified, 2
activate in the Codewhale terminal with the default network policy.
`@michengai/dsh-code-review` 1.0.9 registers its `dsh_code_review` tool and
three commands, and its tool ran end to end in one terminal turn against a
scripted local model: the plugin was called, started its reviewer sub-agent
and returned its report. Review quality with a real model was not tested.
`dsh-anchored-standard` 1.2.1 activates and registers nothing. A third,
`dsh-plugin-model-proxy` 0.1.6, activates once `registry.npmjs.org` is allowed
and registers nothing. The other 29 do not activate; each now reports the
service, module or export it is missing, or stops at import review and says
why.

This page lists every published DeepSeek Harness (DSH) package we tried,
unmodified, in the terminal. The services and limits behind each row are in
[DSH plugin compatibility](DSH_PLUGIN_COMPAT.md).

## How this was tested

Built from source on 2026-11-22 (codewhale 0.10.3 dev build of this change,
macOS arm64, Node 26.10). Each package got its own throwaway Codewhale home
with `[features] extension_host = true`, telemetry and update checks off, and
the provider pointed at a closed local port, so no model provider was called.
For each package: `codewhale plugins import dsh <dir>`, `plugins import dsh
approve <dir> <hash>`, `plugins trust <name> --review-token <token> --yes`,
`plugins enable <name>`, then the terminal UI was started once and the
extension host's line for the plugin was read from the log. Packages marked
"npm approved" were run again with `registry.npmjs.org` in `allow` under
`[network]`, which fetches their declared npm dependencies. For
`dsh-code-review`, the terminal UI was then pointed at a local scripted
OpenAI-compatible endpoint that asks for one `dsh_code_review` call and
answers the reviewer sub-agent with fixed text; both approval cards (the
plugin tool and the sub-agent start) were approved by hand. The packages are
the SHA-256-pinned tarballs of the 0.10.3 survey. Nothing here was run on
Windows, Linux, Bun or the desktop app.

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

Counts, by status with the default network policy: 2 activate, 10 fail at
activation, 20 stop at import review (17 at npm network approval, 3 at the
scan limit). By where they belong: 3 terminal, 10 desktop or web only, 4
refused by policy, 15 need a host service not built yet.

| Package | Version | Status in the terminal | Where it belongs | Checked |
| --- | --- | --- | --- | --- |
| `@michengai/dsh-code-review` | 1.0.9 | activates (tool `dsh_code_review`; commands `/plugin-dsh-code-review-review`, `/review-cancel`, `/review-status`); its text is Chinese because Codewhale exposes no host language setting, and `/review` is renamed because a built-in command has that name | terminal | this build, including one tool call in a turn |
| `dsh-anchored-standard` | 1.2.1 | activates; registers no tools or commands (a preset installer); whether its preset changes anything in Codewhale was not checked | terminal | this build |
| `dsh-plugin-model-proxy` | 0.1.6 | stops at import review: npm network approval. With npm approved: activates and registers nothing; whether its proxy routing affects Codewhale's model requests was not tested | terminal | this build (npm approved) |
| `@michengai/dsh-btw` | 1.3.7 | fails: `ctx.tools.guard is not a function` | needs a host service not built yet: `tools.guard`, the `fork` sub-agent provider, and a session on command invocations | this build |
| `@linxin666/dsh-liangshen` | 0.4.16 | fails: waits on `agentPresets` (the DSH 0.2.2 agent-preset registry is not adopted) | needs a host service not built yet: the agent-preset registry | this build |
| `@michengai/dsh-automation` | 1.0.22 | fails: composition row `dsh-automation` failed activation: requires `@deepseek-ai/dsh-workspace` | refused by policy: its scheduled runs set the approval policy to `never`, and approval authority stays with the Codewhale core | this build |
| `@nanmicoder/dsh-agent-teams` | 0.1.29 | fails: composition row `agent-teams` failed activation: requires `@deepseek-ai/dsh-subagent` | refused by policy: it patches the shared `subagents` service, which plugins may not do; it also needs the DSH sub-agent message delivery runtime | this build |
| `dsh-context` | 0.83.0 | stops at import review: npm network approval (`zod`). With npm approved: fails: composition row `dsh-context` failed activation: `@deepseek-ai/dsh-session` does not provide an export named `deriveEventMessage` | needs a host service not built yet: `sessionProjections`, after the missing `dsh-session` export | this build (default and npm approved) |
| `dsh-whale-widget` | 0.4.2 | fails: waits on `webServer`, `connection` (DSH Web UI services; Codewhale has no plugin web server) | desktop or web only: a balance widget injected into the DSH web page | this build |
| `@michengai/dsh-codex-ui` | 2.0.21 | fails: composition row `codex-ui-session-title` requires `sessionTitle`; reading its source, its other rows also need `webServer` and `profileContext` | desktop or web only: a UI skin for the DSH desktop and web client | this build |
| `@linxin666/dsh-client-ui-skill-explorer` | 0.4.16 | fails: composition row `ui-skill-explorer` requires `webServer` | desktop or web only: its server half only serves its web panel | this build |
| `@linxin666/dsh-client-ui-git-graph` | 0.4.14 | fails: composition row `ui-git-graph` requires `webServer`, `subprocess`, `workspaceRegistry`; `subprocess` belongs to the Codewhale core | refused by policy: `subprocess` is core-owned (and its web panel needs a plugin web server) | this build |
| `@linxin666/dsh-client-ui-task-board` | 0.4.16 | fails: composition row `ui-task-board` failed activation: `@deepseek-ai/dsh-llm` does not provide an export named `ReasoningEffortId` | desktop or web only: a web panel; behind the export it waits on `typertGateway`, `workspaceRegistry`, `webServer` | this build |
| `dsh-image-gen` | 0.11.1 | fails: composition row `image-gen` requires `attachments` | needs a host service not built yet: `attachments` (built, not switched on), a root `systemPrompt.context`, and its nested `connection`/`webServer` waits | this build |
| `dsh-cost-meter` | 1.9.62 | stops at import review: npm network approval. With npm approved: fails: composition row `cost-meter` failed activation: requires `@deepseek-ai/dsh-credentials` | needs a host service not built yet: the `@deepseek-ai/dsh-credentials` module | this build (default and npm approved) |
| `@anionex/dsh-vision-toolkit` | 0.1.49 | stops at import review: npm network approval. With npm approved (0.10.3): fails on `@deepseek-ai/dsh-credentials` | needs a host service not built yet: the `@deepseek-ai/dsh-credentials` module | 0.10.3 survey |
| `@dfy-plugins/dsh-image-generation` | 0.1.4 | stops at import review: npm network approval. With npm approved (0.10.3): fails on `@deepseek-ai/dsh-credentials` | needs a host service not built yet: the `@deepseek-ai/dsh-credentials` module | 0.10.3 survey |
| `@goodandready/dsh-image-gen` | 0.12.27 | stops at import review: npm network approval. With npm approved (0.10.3): fails on `@deepseek-ai/dsh-credentials` | needs a host service not built yet: the `@deepseek-ai/dsh-credentials` module | 0.10.3 survey |
| `@michengai/dsh-agency-agents` | 1.2.27 | stops at import review: npm network approval. With npm approved (0.10.3): fails on `@deepseek-ai/dsh-typert-protocol` | needs a host service not built yet: the `@deepseek-ai/dsh-typert-protocol` module | 0.10.3 survey |
| `@liustack/modsearch` | 5.10.12 | stops at import review: npm network approval. With npm approved (0.10.3): a row waits on `web`, `webServer` and `settings` | needs a host service not built yet: `web` | 0.10.3 survey |
| `@liustack/modlens` | 3.27.3 | stops at import review: npm network approval. With npm approved (0.10.3): a row waits on `attachments`, `llm`, `webServer` | needs a host service not built yet: `attachments` (built, not switched on) | 0.10.3 survey |
| `@michengai/dsh-im-connect` | 1.0.34 | stops at import review: npm network approval. With npm approved (0.10.3): a row waits on `webServer`, `agentPresets`, `agentDefaultModel` and others | needs a host service not built yet: `webServer`, the agent-preset registry, `agentDefaultModel` | 0.10.3 survey |
| `dsh-mnemon` | 0.5.43 | stops at import review: npm network approval. With npm approved (0.10.3): its row `dsh-mnemon/bundle` is absent from the reviewed module closure | needs a host service not built yet: child-scoped `tools.guard`; the closure error may be an import converter gap and was not investigated | 0.10.3 survey |
| `@deepseek-harness-tui/dsh-tui` | 0.19.0 | stops at import review: npm network approval. With npm approved (0.10.3): refused peers `@deepseek-ai/dsh-atomic-write` and `@deepseek-ai/dsh-llm-pi-ai`, a missing `KNOWN_SESSION_EVENT_TYPES` export, and `dshHomePath is not defined` | needs a host service not built yet: those modules and exports | 0.10.3 survey |
| `@michengai/dsh-skills-manager` | 1.4.0 | stops at import review: npm network approval. With npm approved (0.10.3): a row waits on `connection` | desktop or web only: a skills management page | 0.10.3 survey |
| `dsh-pocket` | 2.12.0 | stops at import review: npm network approval. With npm approved (0.10.3): a row waits on `connection`, `webServer` | desktop or web only: serves the DSH web client to a phone | 0.10.3 survey |
| `dshmarket` | 1.73.17 | stops at import review: npm network approval. With npm approved (0.10.3): waits on `webServer`, `loader` | desktop or web only: a visual plugin market | 0.10.3 survey |
| `dsh-better-sidebar` | 0.30.1 | stops at import review: npm network approval. With npm approved (0.10.3): fails on `@deepseek-ai/dsh-settings` | desktop or web only: a sidebar for the DSH client | 0.10.3 survey |
| `@linxin666/dsh-web-all` | 0.4.16 | stops at import review: npm network approval. With npm approved (0.10.3): refused peers `dsh-settings`, `dsh-credentials`, `dsh-subagent`, a missing `ReasoningEffortId` export, and `agentPresets` | desktop or web only: a bundle of DSH Web UI plugins | 0.10.3 survey |
| `dsh-pet` | 0.4.7 | stops at import review: `lib/app.js` exceeds the 2,097,152-byte scan limit; behind it, npm approval and refused peers `@deepseek-ai/dsh-home-paths`, `@deepseek-ai/dsh-credentials` (read, not run) | desktop or web only: an animated desktop pet | 0.10.3 survey |
| `dsh-univer-office` | 0.4.0 | stops at import review: `lib/index.js` exceeds the 2,097,152-byte scan limit; behind it, npm approval and the refused peer `@deepseek-ai/dsh-skill` (read, not run) | needs a host service not built yet: the `@deepseek-ai/dsh-skill` module (its viewer is a web window) | 0.10.3 survey |
| `@changfenhuang/dsh-genui` | 0.16.0 | stops at import review: `lib/assets/mermaid.js` exceeds the 2,097,152-byte scan limit; with that file stubbed (0.10.3), activation is refused: system prompt sections are only available on a child agent | refused by policy: root-agent system prompt sections | 0.10.3 survey |

Not tried: `dsh-agent-squad`, `dsh-im` and `dsh-archive-manager`, which the
acceptance pins mark as refused, and `DamonKoy-dsh-web-ui`.
