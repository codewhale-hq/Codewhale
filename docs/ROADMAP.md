# Roadmap

Codewhale is the open-source agent between you and any model. You can read its
code, or ask it to read its own code and explain what it does. This page
says where the project is going and where help counts most. It has two parts,
the long-term goals that change slowly and the releases that change every few
weeks.

Nothing here is a promise or a date. A milestone is a plan, and a release
exists only when its tag and downloads do. The
[changelog](../CHANGELOG.md) is the record of what shipped.

## How the project improves

Codewhale is what it is because people used it on machines, providers, shells
and languages the maintainers do not have, and then said what happened. Each bug
report, each screenshot of something that looked wrong, each confused first run,
each translation and each provider fix moved the project. We want that loop to
keep running, release after release.

So all feedback is welcome, including "this felt off" with no proposed fix.
You do not need to write code to shape Codewhale. See
[how to contribute](../CONTRIBUTING.md) for where each kind of help goes.

## Long-term goals

Each goal names the open questions, because that is where a new idea or a
report is most likely to change the outcome.

### 1. One engine behind every way to use it

One Rust engine runs the turn, the tools, permissions, sessions and receipts.
The terminal app, `codewhale exec`, the local web UI, the
[Runtime API](RUNTIME_API.md), editors over the Agent Client Protocol and the
desktop app in development are all clients of it. A fix in the engine reaches
every client, and no client grows a second agent loop.

What is still open: the largest crates still need to be split
([#5316](https://github.com/codewhale-hq/CodeWhale/issues/5316),
[#5586](https://github.com/codewhale-hq/CodeWhale/issues/5586)), and the desktop
app has no public download yet
([#4986](https://github.com/codewhale-hq/CodeWhale/issues/4986)).

### 2. Every model is a peer

More than 40 providers are built in, along with any OpenAI-compatible endpoint
and local runtimes. No provider is ranked above another. A provider that
behaves differently is described by a profile, not by special cases scattered
through the code.

What is still open: Responses API behavior needs to move into provider profiles
([#5092](https://github.com/codewhale-hq/CodeWhale/issues/5092),
[#5093](https://github.com/codewhale-hq/CodeWhale/issues/5093)), and the
community uses many providers and models that we have not tested. The
[provider list](PROVIDERS.md) is a standing project, so additions and
corrections are welcome at any time.

### 3. You stay in control, and you can see what happened

Codewhale asks before it does something you did not allow, runs commands in a
sandbox where the system has one, and keeps a receipt of every file changed,
command run and call made. A cost it does not know is shown as unknown, never
as free.

What is still open: Windows has no OS sandbox yet, approval prompts could be
clearer, and we want to hear about every case where Codewhale refused something
reasonable or allowed something it should have asked about. Those reports are
among the most useful we get.

### 4. Many agents working side by side

Sub-agents, fleets and workflows let several agents share a job. Other coding
agents can run as Codewhale agents too, so you can keep the tools you already
use and see their work in one place.

What is still open: long-running work needs to survive restarts and
cancellation cleanly, a finished goal needs independent verification
([#6013](https://github.com/codewhale-hq/CodeWhale/issues/6013)), and context
needs to stay useful in very long sessions
([#7206](https://github.com/codewhale-hq/CodeWhale/issues/7206),
[#6426](https://github.com/codewhale-hq/CodeWhale/issues/6426)).

### 5. Bring what you already have

Codewhale reads `AGENTS.md`, imports Claude Code settings and plugins, speaks
MCP, and runs reviewed
[DeepSeek Harness plugins](DSH_PLUGIN_COMPAT.md). Switching should not mean
starting over.

What is still open: DeepSeek Harness plugins are not yet fully compatible on
macOS, Linux and Windows, and we want to hear about each plugin, skill or config
format that does not import the way you expected.

### 6. For everyone, in their language, on their machine

The README ships in 19 languages, and the terminal and website are localized.
Codewhale runs on macOS, Linux and Windows, with preview or community support
for Termux, HarmonyOS and more. Accessibility is part of the work, not an
extra.

What is still open: translation is a standing project
([localization matrix](LOCALIZATION.md),
[Chinese localization group](https://github.com/codewhale-hq/CodeWhale/issues/6804)),
and so are input methods
([#2323](https://github.com/codewhale-hq/CodeWhale/issues/2323)), screen
readers ([accessibility](ACCESSIBILITY.md)) and new platforms such as FreeBSD
([#1097](https://github.com/codewhale-hq/CodeWhale/issues/1097)).

### 7. A project that helps build itself

Codewhale is used to work on Codewhale. A well-written issue is something an
agent can pick up and finish, and a person reviews the result. The better the
tool gets, the faster it can improve, and the more a clear report is worth.

What is still open: memory that carries across sessions
([#6050](https://github.com/codewhale-hq/CodeWhale/issues/6050)) and skills
that Codewhale learns with your approval
([#5860](https://github.com/codewhale-hq/CodeWhale/issues/5860)) are both at
the design stage.

## Releases

The latest release is on the
[releases page](https://github.com/codewhale-hq/CodeWhale/releases). `main`
holds the next one while it is being built.

| Milestone | What it is for |
| --- | --- |
| [0.10.3](https://github.com/codewhale-hq/CodeWhale/milestone/73) | This release is in progress; follow [#7284](https://github.com/codewhale-hq/Codewhale/issues/7284) for what is left and how to help. It fixes undo, sign-in, model selection and install problems reported against 0.10.2, and it adds DeepSeek Harness plugin compatibility and ACP in both directions: other agents running inside Codewhale, and editors driving Codewhale. Codex App Server v2 compatibility moved out of this release. The changelog lists its [known limits](../CHANGELOG.md). |
| [0.10.4](https://github.com/codewhale-hq/CodeWhale/milestone/75) | This release improves reliability through process cleanup, resource limits, fail-closed error handling and dead-code removal. |
| [0.10.5](https://github.com/codewhale-hq/CodeWhale/milestone/76) | This release improves usability with running-command detail, usage and tool diagnostics, and file previews. |
| [0.11](https://github.com/codewhale-hq/CodeWhale/milestone/72) | This release reworks the architecture with smaller crates, provider profiles, one command contract and cache-stable context folding. |
| [1.0](https://github.com/codewhale-hq/CodeWhale/milestone/66) | This longer-term release brings the desktop app to every platform and adds pluggable memory and approved skill learning. |
| [Backlog](https://github.com/codewhale-hq/CodeWhale/milestone/69) | These are ideas we kept without a release, and nothing here blocks one. |

Scope moves between milestones as we learn. If something you need is in a later
milestone or missing, say so on its issue or open one, because that is how
priorities get corrected.

## Where to start

- The [help wanted](https://github.com/codewhale-hq/CodeWhale/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22)
  label marks scoped issues that a contributor can pick up, and you can comment
  on one to claim it.
- You can add a 👍 to any [open issue](https://github.com/codewhale-hq/CodeWhale/issues)
  you care about, and we read that as a real signal.
- [Ideas](https://github.com/codewhale-hq/CodeWhale/discussions/categories/ideas)
  in Discussions is the place for something you want to talk through before it
  becomes an issue.
- You can chat on [Discord](https://discord.gg/37gfS3ksug), or add `hunterbown`
  on WeChat and ask to join the Whale Brothers group. Everyone is welcome.
