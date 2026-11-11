import type { ConstitutionDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/constitution/page.tsx` and
 * its `components/thinking-trace.tsx`. Copy moved verbatim from their `isZh`
 * ternaries and `{ en, zh }` label. The three layers and the traced scenes
 * are content and stay with the page and the component.
 */
export const constitution: ConstitutionDict = {
  metaTitle: "Three layers of rules · Codewhale",
  metaDescription:
    "Codewhale's nested constitution: bundled base rules, your standing rules (/constitution), and your repo's rules (.codewhale/constitution.json). Rank is enforced in code and survives a model swap.",
  kicker: "The thesis",
  title: "Three layers of rules",
  titleAside: "三层法",
  titleAsideLang: "zh",
  lede: "As a project ages, instructions pile up and conflict: the original spec, a refactor that contradicts it, stale memory, a previous agent's handoff, your current request, fresh test output that doesn't match what the handoff claimed. A flat system prompt makes the model resolve that by guess. Codewhale uses a nested constitution with a defined rank. The order is enforced in code, tests assert it can't drift, and it stays intact when you swap models.",
  since: "Since v0.9.0",
  sinceBody:
    "Constitution-first setup — first launch walks language, model, posture, and your constitution; /setup any time. The model can draft it. You ratify it.",
  rankTitle: "The rank, most-static first",
  rankScope:
    "Below the three layers rank project instructions (AGENTS.md), then memory and handoffs. Your current request and live tool evidence still control the active turn — the model may be given many layers, but it is instructed never to report a fact the tools did not return.",
  boundaryTitle: "What stays enforced in code",
  boundaryBody:
    "Approval, sandbox, network, and trust controls are enforced in code — constitution text never overrides them.",
  traceTitle: "Observable in the reasoning",
  traceScope:
    "The rank shows up in the model's reasoning: it cites the articles as it decides. These panes are illustrations of that kind of reasoning, paraphrased for this page, not a transcript of one session.",
  illustration: "Illustration",
  install: "Install",
  configuration: "Configuration",
};
