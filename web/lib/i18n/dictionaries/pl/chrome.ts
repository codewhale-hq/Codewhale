import type { ChromeDict } from "../types";

/**
 * Polish chrome dictionary.
 *
 * Natywny przekład w aktualnym kierunku angielskiego — „any model, on your
 * machine", bez wycofanego pozycjonowania „local-first". Rejestr bezpośredni
 * (ty), standard polskich narzędzi deweloperskich.
 *
 * Tryby i postawy uprawnień zostają dosłowne (Plan / Work / Operate, Ask /
 * Auto-Review / Full Access); `Runtime`, `fleet` i `TUI` to nazwy produktu;
 * „receipt" to potwierdzenie.
 *
 * Drugie etykiety nawigacji parują polską etykietę z krótkim angielskim
 * odpowiednikiem — para han to zabieg redakcyjny wydania angielskiego.
 */
export const chrome: ChromeDict = {
  navDocs: "Dokumentacja",
  navContribute: "Współtwórz",
  navSignIn: "Zaloguj się",
  navDesktop: "Pulpit",

  navFeatures: "Możliwości",
  navModels: "Modele",
  navPlugins: "Wtyczki",

  skipToContent: "Przejdź do treści głównej",

  navPrimaryAria: "Nawigacja główna",
  navHomeAria: "Strona główna Codewhale",

  installCta: "Instaluj →",

  dateLocale: "pl-PL",

  menuOpen: "Otwórz menu",
  menuClose: "Zamknij menu",
  translationMachineDraft: "To tłumaczenie jest roboczym tekstem maszynowym i nie zostało jeszcze sprawdzone przez osobę, dla której polski jest językiem ojczystym.",

  themeAuto: "auto",
  themeLight: "jasny",
  themeDark: "ciemny",
  themeAria: "Motyw: {mode} (kliknij, aby przełączyć)",
  themeTitle: "Motyw · auto / jasny / ciemny",

  footerProduct: "Produkt",
  footerProject: "Projekt",
  footerDocs: "Dokumentacja",
  footerGuide: "Pierwsze kroki",
  footerInstall: "Instalacja",
  footerModels: "Modele",
  footerFaq: "FAQ",
  footerContribute: "Współtwórz",
  footerRoadmap: "Plan rozwoju",
  footerLicense: "Licencja MIT",
  footerMerch: "Gadżety",
  footerTerms: "Warunki usługi",
  footerPrivacy: "Prywatność",
  footerChangelog: "Dziennik zmian",
  footerCanonicalSource: "Repozytorium źródłowe: ",
  footerReleases: " · Wydania: ",
  footerReleasesLink: "Wydania na GitHubie",
  footerSecurity: "Bezpieczeństwo",
  footerLegal: "Prawne",

  switcherLabel: "Język",
  switcherSwitchTo: "Przełącz na: {label}",
  partialBadge: "(częściowo)",

  stateReleased: "Wydano",
  stateAvailable: "Dostępne",
  stateBeta: "Beta",
  stateDev: "Wersja deweloperska",
  stateOpen: "Otwarte",
  stateInviteOnly: "Tylko na zaproszenie",
  stateNotOpen: "Jeszcze nie otwarte",
  stateNotPublic: "Niepubliczne",
  stateInSource: "W kodzie źródłowym, jeszcze niewydane",
  numbersAsOf: "Stan na {date}",
  downloadMacos: "Pobierz dla macOS",
  downloadVersion: "Wersja {version}",
  downloadSize: "Rozmiar {size}",
  downloadChecksum: "Suma kontrolna SHA-256",
  downloadSigned: "Podpisano i poświadczono notarialnie",
};
