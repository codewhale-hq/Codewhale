import type { DocsVocabularyDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/vocabulary/page.tsx`.
 * Copy moved verbatim from its `isZh` ternaries. The terms and their
 * definitions are content and stay in `lib/content/vocabulary.ts`.
 */
export const docsVocabulary: DocsVocabularyDict = {
  metaTitle: "Vocabulary · Codewhale Docs",
  metaDescription:
    "The exact product nouns: Fleet, Workflow, Lane, Runtime, Advisor, Plan / Work and permission postures in the 0.10.3 source candidate, plus requested→effective reasoning, routing source, and measurement principles.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Vocabulary",
  lead: "These definitions describe Codewhale's product vocabulary, with control names scoped to the source candidate below. The execution nouns match the public fact matrix in the repository.",
  executionHeading: "Execution nouns",
  controlHeading: "Modes and permissions in the 0.10.3 source candidate",
  controlLead:
    "These controls describe the unpublished 0.10.3 source candidate; the published v0.10.2 release has different controls. Shift+Tab steps Ask → Accept Edits → Plan → Auto-Review → Full Access, while Tab completes text.",
  routeHeading: "Route identity: provider · model · requested→effective reasoning · source",
  advisoryHeading: "Advisory role",
  measurementHeading: "Measurement principles",
  leaderboardNote:
    "Benchmark leaderboard: none on this site, and none will appear without route identity and a measurement harness attached.",
  sourceNote:
    "Source documents: docs/FLEET.md, docs/MODES.md, docs/public-surface-facts.json · Vocabulary copy lives in web/lib/content/vocabulary.ts; update docs-map.ts when changing.",
};
