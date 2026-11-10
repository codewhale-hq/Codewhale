# Codewhale website positioning

Reviewed November 6, 2026 Pacific. Current homepage and product copy follow
this brief. Earlier competitive observations remain in Git history.

## What the live competitors actually present

| Site | What the page does | Lesson for Codewhale |
| --- | --- | --- |
| [OpenCode](https://opencode.ai/) | Names the category, shows desktop/terminal/mobile/SDK views, then models, integrations and contributor proof. | Show usable surfaces and the next action. Model choice is expected here. Do not offer our unfinished clients as downloads. |
| [Pi](https://pi.dev/) | Leads with ownership and customization; demonstrates extensions, session history and workflows; packages connect use with contribution. | Show what people can adapt and share. A Markdown skill is an inviting first contribution. |
| [Oh My Pi](https://omp.sh/) | Leads with built-in IDE tools, a copyable installer, provider marks and selectable feature demonstrations. | Sell a working capability, then show it. Avoid an undifferentiated feature list. |
| [Aider](https://aider.chat/) | Establishes terminal pair programming, then shows the product, codebase/git/testing capabilities and installation. | Name the job and make trying it straightforward. |
| [Cursor](https://cursor.com/) | Leads with building software and puts interface demonstrations beside feature claims. | Evidence should carry more weight than adjectives. |
| [Claude Code](https://claude.com/product/claude-code) | Opens with recognizable tasks and steering/review across supported surfaces. | Use concrete jobs; distinguish shipping capability from our roadmap. |

These are presentation observations, not an independent evaluation of
competitor quality, adoption or completeness.

## The supported position

**Use any model through an agent you can read.** (founder, 2026-11-10.) The model is the
part nobody can inspect. Codewhale is the open-source agent between the reader
and that model: it decides what is sent, which tools run and what is allowed,
and its code can be read, or Codewhale can be asked to read its own code and
explain it. An international community improves it with every release. The
earlier headline, "An open-source agent for your computer", is retired.
Codewhale combines a model-neutral local agent, coordinated model
teams, reusable Workflows, connected tools and an independently released
Computer Use plugin, with visible permissions and recorded execution.
Do not claim competitors lack these capabilities.

| Capability | Value to the reader | Owning evidence and limit |
| --- | --- | --- |
| Operate | Keep a larger objective moving through planning, delegation and verification. | v0.10.2 docs/MODES.md and crates/tui/src/runtime_handoff.rs; current crates/engine/src/runtime_handoff.rs. Same authority as Work. No hosted or always-on promise. |
| Fleet | Choose different models for investigation, implementation and review, then save the roles. | v0.10.2 docs/FLEET.md. Model pins are explicit; configuration alone proves no quality or cost gain. |
| Workflows | Connect steps, dependencies, parallel branches, review gates and results into a repeatable process. | Released Workflow tool/CLI and workflows/issue_audit.workflow.js. Some saved Fleet fields remain unsupported; link the guide. |
| Skills and plugins | Teach a checklist or report format, then share it without changing Rust. | v0.10.2 docs/SKILLS.md and docs/PLUGIN_AUTHORING.md. Native ESM mods and DSH have separate experimental/candidate limits. |
| Request preview and receipts | Inspect the next primary request and review recorded work and approvals. | v0.10.2 docs/PREVIEW_REQUEST.md, docs/RECEIPTS.md, docs/GUIDE.md. Coverage has limits; undo does not reverse external effects. |
| Computer Use | Work in another Mac app while keeping your cursor and foreground app; use accessible controls or a separate browser profile. | [Published v0.16.1](https://github.com/codewhale-hq/codewhale-cu-plugin/releases/tag/v0.16.1), README and release checklist. Mac beta; Windows unsigned experimental preview; Linux experimental. Helper plus compatible MCP host connection. |
| Terminal and local browser | Use files, commands, approvals and session history on your machine. | Released v0.10.2 docs/WEB.md, docs/GUIDE.md and feature registry. Native desktop and hosted web remain separate development surfaces. |

## The page's sequence

1. Promise, brief category explanation, install command and contribution path.
2. Real terminal gallery: conversation, workbar, delegated reviewer and models.
   These captures are a local development demo with a deterministic provider,
   not live-provider performance evidence.
3. Show how one task grows into Operate, a Fleet and a reusable Workflow.
4. Give Computer Use a dedicated section with practical background behavior
   and accurate setup, rather than hiding it in a generic tools row.
5. Explain model choice and current availability. Keep preview/source labels.
6. Invite bugs, docs, translations, skills, code and testing on another setup.

## Marketing habits to apply

- Start with a recurring job and show the feature that helps.
  [April Dunford's method](https://www.aprildunford.com/post/a-quickstart-guide-to-positioning)
  connects alternatives, differentiated capabilities and value for a specific audience.
- Make the category and next step clear. [Nielsen Norman Group](https://www.nngroup.com/articles/homepage-design-principles/)
  recommends an understandable proposition and scannable content.
- Make contribution part of use. [GitHub's community guide](https://opensource.guide/building-community/)
  recommends clear, low-friction entry points. Its [contribution guide](https://opensource.guide/how-to-contribute/)
  includes useful noncode work.
- Publish verified examples: a parallel issue review, a reusable report
  Workflow, or background Mac app work. State version and environment. Do
  not substitute a synthetic animation for measured evidence.
- Measure install completion, first useful task and first useful contribution
  before claiming better conversion. This change establishes no conversion result.

COPY_STYLE.md owns the voice. The existing Codewhale Moda kit supplies the
restored ocean identity; shared type, spacing and motion remain in tokens.
Do not invent customer logos, performance comparisons, spending guarantees or
continuous-operation claims.
