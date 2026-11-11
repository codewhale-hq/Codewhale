import type { ContributeDict } from "../types";

/** Polish dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Współtwórz · Codewhale",
  metaDescription:
    "Każda opinia pomaga Codewhale, z kodem lub bez. Sprawdź, dokąd trafia błąd, pomysł, tłumaczenie, dostawca lub poprawka.",
  kicker: "Twórzmy Codewhale razem",
  title: "Pomóż tworzyć Codewhale",
  lede: "Każda opinia pomaga, z kodem lub bez. Błąd, pomysł albo ekran, który wyglądał źle, w zupełności wystarczą na początek.",
  fileIssue: "Otwórz zgłoszenie",
  browsePulls: "Przeglądaj pull requesty",
  fullGuide: "Przewodnik dla współtwórców",
  pathsTitle: "Wybierz, od czego zacząć",
  workflowTitle: "Wyślij zmianę, którą inni mogą przejrzeć",
  reviewTitle: "Pokaż, co się zmieniło i jak to sprawdzono",
  reviewScope:
    "Przekaż recenzentom problem, powód zmiany, wyniki testów i pozostałe ryzyko. Wąski zakres daje bardziej czytelne uwagi.",
  devTitle: "Zbuduj i sprawdź zmianę w kodzie",
  devScope:
    "Przy kodzie Rust zbuduj odpowiedni fragment i uruchom jego ukierunkowane testy. Dalsze kontrole, których wymaga Twoja zmiana, opisuje przewodnik dla współtwórców.",
  mergedAccountsCount: {
    one: "W ciągu ostatnich 30 dni scalono pull requesty od {n} konta GitHub.",
    few: "W ciągu ostatnich 30 dni scalono pull requesty od {n} kont GitHub.",
    many: "W ciągu ostatnich 30 dni scalono pull requesty od {n} kont GitHub.",
    other: "W ciągu ostatnich 30 dni scalono pull requesty od {n} konta GitHub.",
  },
  mergedPeopleCount: {
    one: "W ciągu ostatnich 30 dni scalono pull requesty od {n} osoby.",
    few: "W ciągu ostatnich 30 dni scalono pull requesty od {n} osób.",
    many: "W ciągu ostatnich 30 dni scalono pull requesty od {n} osób.",
    other: "W ciągu ostatnich 30 dni scalono pull requesty od {n} osoby.",
  },
};
