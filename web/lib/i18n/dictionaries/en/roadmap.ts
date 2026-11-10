import type { RoadmapDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/roadmap/page.tsx`. Copy
 * moved verbatim from its `isZh` ternaries. The tracks themselves are content
 * and stay in the page as `tracksEn` / `tracksZh`.
 */
export const roadmap: RoadmapDict = {
  metaTitle: "Roadmap · Codewhale",
  metaDescription:
    "Seven long-term goals, what each release is for, and where your help changes the outcome.",
  eyebrow: "Project roadmap",
  title: "Where Codewhale is going",
  introduction:
    "Codewhale improves through the people who use it and say what happened. This page shows the long-term goals, the releases ahead and what is still open. Nothing here is a promise or a date.",
  sectionTitle: "Shipped, underway and ruled out",
  browseIssues: "Browse open issues",
  trackCount: "{count} items",
  trackCountOne: "{count} item",
  contributeTitle: "Keep roadmap decisions in the open.",
  contributeBody:
    "Use issues for bugs and well-scoped feature requests, Discussions for ideas that need shaping, and pull requests for concrete changes with tests or documentation. Verification across languages, platforms, and providers helps maintainers judge priority.",
  issuesDetail: "Report a problem or propose scoped work.",
  discussionsDetail: "Explore an early idea before implementation.",
  pullsDetail: "Review existing work or send a focused change.",
};
