import type { ContributeDict } from "../types";

export const contribute: ContributeDict = {
  metaTitle: "Contribute · Codewhale",
  metaDescription:
    "All feedback helps Codewhale, with or without code. Find where a bug, an idea, a translation, a provider or a fix should go.",
  kicker: "Build Codewhale together",
  title: "Help build Codewhale",
  lede: "All feedback helps, with or without code, and a bug, an idea or a screen that looked wrong is enough to start.",
  fileIssue: "Open an issue",
  browsePulls: "Browse pull requests",
  fullGuide: "Contributor guide",
  pathsTitle: "Choose where to start",
  workflowTitle: "Send a change others can review",
  reviewTitle: "Show what changed and how you checked",
  reviewScope:
    "Give reviewers the problem, the reason for the change, test evidence, and remaining risk. A tight scope gets clearer feedback.",
  devTitle: "Build and check your code change",
  devScope:
    "For Rust code, build the relevant code and run its focused checks. Follow the contributor guide for any broader checks your change needs.",
  mergedAccountsCount: {
    one: "{n} GitHub account had a pull request merged in the last 30 days.",
    other: "{n} GitHub accounts had pull requests merged in the last 30 days.",
  },
  mergedPeopleCount: {
    one: "{n} person had a pull request merged in the last 30 days.",
    other: "{n} people had pull requests merged in the last 30 days.",
  },
};
