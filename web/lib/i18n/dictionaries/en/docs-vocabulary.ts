import type { DocsVocabularyDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/vocabulary/page.tsx`.
 * Copy moved verbatim from its `isZh` ternaries. The terms and their
 * definitions are content and stay in `lib/content/vocabulary.ts`.
 */
export const docsVocabulary: DocsVocabularyDict = {
  metaTitle: "Vocabulary · Codewhale Docs",
  metaDescription:
    "The exact product nouns: Fleet, Workflow, Lane, Runtime, Advisor, Plan, Work and Operate with the permission postures, plus requested→effective reasoning, routing source, and measurement principles.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Vocabulary",
  lead: "These definitions describe Codewhale's product vocabulary, with the controls of the published release first and source-only controls kept in a labelled section. The execution nouns match the public fact matrix in the repository.",
  executionHeading: "Execution nouns",
  controlHeading: "Modes and permissions",
  controlLead:
    "Tab with an empty composer cycles Plan → Work → Operate, and Shift+Tab cycles the permission posture Ask → Auto-Review → Full Access. You decide what it may do without asking.",
  sourceControlHeading: "Modes and permissions in source",
  routeHeading: "Route identity: provider · model · requested→effective reasoning · source",
  advisoryHeading: "Advisory role",
  measurementHeading: "Measurement principles",
  leaderboardNote:
    "Benchmark leaderboard: none on this site, and none will appear without route identity and a measurement harness attached.",
  sourceNote:
    "Source documents: docs/FLEET.md, docs/MODES.md at the published release tag, docs/public-surface-facts.json · Vocabulary copy lives in web/lib/content/vocabulary.ts; update docs-map.ts when changing.",
};
