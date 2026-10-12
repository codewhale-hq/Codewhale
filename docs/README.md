# Codewhale documentation

Start with the [user guide](GUIDE.md). The website renders a subset of these
pages at [codewhale.net/docs](https://codewhale.net/en/docs); the Markdown here
is the source. Translations live beside their English page (`*.id.md`) or in
[`id/`](id) and [`zh_hans/`](zh_hans).

## Get started

- [Installing Codewhale](INSTALL.md), including PATH help and shell completions
- [User guide](GUIDE.md): first run, sessions, commands, everyday workflows
- [Keybindings](KEYBINDINGS.md)
- [Pet mode](PET_MODE.md): a glanceable whale view per terminal, with replies and agents on demand
- [Modes and permission postures](MODES.md)
- [Configuration](CONFIGURATION.md)
- [Providers and local models](PROVIDERS.md) and the [Model Lab roadmap](MODEL_LAB.md)
- Platform notes: [Docker](DOCKER.md), [Termux / Android](TERMUX.md),
  [HarmonyOS](HarmonyOS.md), [environment caveats](ENVIRONMENTS.md),
  [classroom and lab installs](CLASSROOM_INSTALL.md)

## Using Codewhale

- [Local browser client](WEB.md) (`codewhale web`)
- [Agent fleet](FLEET.md), [sub-agents](SUBAGENTS.md), and the
  [fleet and workflow tutorial](FLEET_WORKFLOW_TUTORIAL.md)
- [Workflow authoring](WORKFLOW_AUTHORING.md),
  [automatic workflows](AUTOMATIC_WORKFLOWS.md), and
  [experimental workflow search](WORKFLOW_EXPERIMENTAL_SEARCH.md)
- [Skills](SKILLS.md) and [evaluating skill changes](SKILL_EVALUATION.md)
- [User memory](MEMORY.md)
- [`read_media`](READ_MEDIA.md) and [`/preview-request`](PREVIEW_REQUEST.md)

## Extending Codewhale

- [MCP servers](MCP.md)
- [Hooks](HOOKS.md)
- [Installing plugins](PLUGINS.md), [writing a plugin](PLUGIN_AUTHORING.md),
  [plugin bundles](PLUGIN_BUNDLES.md), [the first-party marketplace](PLUGIN_MARKETPLACE.md),
  and [Claude plugin compatibility](CLAUDE_PLUGIN_COMPAT.md)
- [Writing an extension tool](EXTENSIONS.md) (experimental `extension_host`)
- [LSP: PHP and custom language servers](LSP_PHP_CUSTOM.md)
- [Runtime API and integration contract](RUNTIME_API.md)
- [GitHub App setup](GITHUB_APP.md)
- [DeepSeek Harness integration](INTEGRATIONS_DSH.md)

## Safety and trust

- [Authorization order](AUTHORIZATION_ORDER.md)
- [Sandbox threat model](SANDBOX.md)
- [Workroom security model](WORKROOM_SECURITY.md)
- [Runtime receipts](RECEIPTS.md)
- [Signed cloud facts](CLOUD_FACTS.md)
- [Telemetry](TELEMETRY.md)
- [Accessibility](ACCESSIBILITY.md)
- Security reports: see [`.github/SECURITY.md`](../.github/SECURITY.md)

## Architecture

- [Product](PRODUCT.md) and [architecture overview](ARCHITECTURE.md)
- [Agent runtime](AGENT_RUNTIME.md) and [Codewhale Agent](CODEWHALE_AGENT.md)
- [Command and control-plane contract](COMMAND_CONTROL_PLANE.md)
- [Tool surface](TOOL_SURFACE.md)
- [Prompt-cache stability](CACHE.md)
- [Workroom architecture](WORKROOM_ARCHITECTURE.md)
- Design notes: [`architecture/`](architecture), [`design/`](design),
  [`decisions/`](decisions), and [`rfcs/`](rfcs)

## Contributing and operating

- [Contribution guide](../CONTRIBUTING.md) and [agent ethos](AGENT_ETHOS.md)
- [Voice and terminal charter](VOICE.md), [motion contract](MOTION_CONTRACT.md),
  and [settings picker framework](SETTINGS_PICKER_FRAMEWORK.md)
- [Issue triage](ISSUE_TRIAGE.md)
- [Build and test performance](BUILD_PERFORMANCE.md)
- [Live smoke runs](LIVE_SMOKE.md)
- [Localization matrix](LOCALIZATION.md)
- [Dependency maintenance](dependency-maintenance.md)
- [Release checklist](RELEASE_CHECKLIST.md) and [release runbook](RELEASE_RUNBOOK.md)
- [Operations runbook](OPERATIONS_RUNBOOK.md)
- [Catalog refresh](CATALOG_REFRESH.md) and [CNB mirror](CNB_MIRROR.md)
- Agent skills for contributors: [`skills/`](skills)

## History and reference

- [Feature registry](features.toml): every user feature with its status, first
  release, docs page and owning code, checked against the code by a test
- [Contributors](CONTRIBUTORS.md)
- [Changelog archive](CHANGELOG_ARCHIVE.md) and the
  [lifecycle outbox changelog](changelog-lifecycle-outbox.md)
- [Rebrand: DeepSeek TUI to Codewhale](REBRAND.md) and
  [legacy `.deepseek/` paths](LEGACY_PATHS.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)
- Historical plans: [TUI modularization](TUI_MODULARIZATION.md),
  [post-0.9.1 seams](POST_0_9_1_SEAMS.md),
  [runtime simplification](RUNTIME_SIMPLIFICATION_DESIGN.md),
  [tool lifecycle (v0.8.53)](TOOL_LIFECYCLE.md)
