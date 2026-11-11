import type { ContributeDict } from "../types";

/** German dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Mitwirken · Codewhale",
  metaDescription:
    "Jedes Feedback hilft Codewhale, mit oder ohne Code. Finde heraus, wohin ein Bug, eine Idee, eine Übersetzung, ein Anbieter oder ein Fix gehört.",
  kicker: "Codewhale gemeinsam bauen",
  title: "Hilf mit, Codewhale zu bauen",
  lede: "Jedes Feedback hilft, mit oder ohne Code. Ein Bug, eine Idee oder ein Bildschirm, der falsch aussah, reicht als Anfang.",
  fileIssue: "Issue öffnen",
  browsePulls: "Pull Requests ansehen",
  fullGuide: "Leitfaden für Mitwirkende",
  pathsTitle: "Such dir einen Einstieg aus",
  workflowTitle: "Eine Änderung senden, die andere prüfen können",
  reviewTitle: "Zeige, was sich geändert hat und wie du es geprüft hast",
  reviewScope:
    "Gib Reviewern das Problem, den Grund für die Änderung, Testnachweise und verbleibende Risiken mit. Ein enger Umfang führt zu klarerem Feedback.",
  devTitle: "Deine Codeänderung bauen und prüfen",
  devScope:
    "Baue bei Rust-Code den betroffenen Code und führe seine gezielten Prüfungen aus. Weitere Prüfungen, die deine Änderung braucht, findest du im Leitfaden für Mitwirkende.",
  mergedAccountsCount: {
    one: "{n} GitHub-Konto hatte in den letzten 30 Tagen einen zusammengeführten Pull Request.",
    other: "{n} GitHub-Konten hatten in den letzten 30 Tagen zusammengeführte Pull Requests.",
  },
  mergedPeopleCount: {
    one: "{n} Person hatte in den letzten 30 Tagen einen zusammengeführten Pull Request.",
    other: "{n} Personen hatten in den letzten 30 Tagen zusammengeführte Pull Requests.",
  },
};
