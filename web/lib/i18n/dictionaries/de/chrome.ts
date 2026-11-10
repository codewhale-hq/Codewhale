import type { ChromeDict } from "../types";

/**
 * German chrome dictionary.
 *
 * Native Neufassung in der aktuellen englischen Richtung — „any model, on
 * your machine", nicht die eingestellte „local-first"-Positionierung.
 * Register wie der TUI-Pack (crates/tui/locales/de.json): knapp, nominal,
 * Anrede per Du in der Community-Tradition deutscher Entwicklerwerkzeuge.
 *
 * Terminologie folgt dem TUI-Pack: Modi und Permission-Posturen bleiben
 * literal (Plan / Work / Operate, Ask / Auto-Review / Full Access),
 * „Berechtigungen" ist permissions, „Laufzeitbeleg" ist receipt; `Runtime`,
 * `fleet` und `TUI` bleiben Produktnamen.
 *
 * Die sekundären Nav-Labels paaren das deutsche Primary mit einem kurzen
 * englischen Gegenstück — das Han-Paar ist das Editorial der englischen
 * Ausgabe.
 */
export const chrome: ChromeDict = {
  navDocs: "Dokumentation",
  navStart: "Erste Schritte",
  navInstall: "Installieren",
  navFaq: "FAQ",
  navCommunity: "Community",
  navContribute: "Mitwirken",

  navFeatures: "Funktionen",
  navProduct: "Produkt",
  navModels: "Modelle",
  navPlugins: "Plugins",

  skipToContent: "Zum Hauptinhalt springen",

  navPrimaryAria: "Hauptnavigation",
  navHomeAria: "Codewhale-Startseite",

  installCta: "Installieren →",

  dateLocale: "de-DE",

  menuOpen: "Menü öffnen",
  menuClose: "Menü schließen",

  themeAuto: "auto",
  themeLight: "hell",
  themeDark: "dunkel",
  themeAria: "Design: {mode} (Klick zum Wechseln)",
  themeTitle: "Design · auto / hell / dunkel",

  footerTagline: "Nutze jedes Modell mit einem Agenten, den du lesen kannst.",
  footerProduct: "Produkt",
  footerProject: "Projekt",
  footerDocs: "Dokumentation",
  footerGuide: "Erste Schritte",
  footerInstall: "Installation",
  footerModels: "Modelle",
  footerRuntime: "Runtime",
  footerFaq: "FAQ",
  footerIssues: "Issues",
  footerContribute: "Mitwirken",
  footerRoadmap: "Roadmap",
  footerLicense: "MIT-Lizenz",
  footerTerms: "Nutzungsbedingungen",
  footerPrivacy: "Datenschutz",
  footerChangelog: "Änderungsprotokoll",
  footerCanonicalSource: "Kanonische Quelle: ",
  footerReleases: " · Releases: ",
  footerReleasesLink: "GitHub-Releases",
  footerSecurity: "Sicherheit",

  switcherLabel: "Sprache",
  switcherSwitchTo: "Zu {label} wechseln",
  partialBadge: "(teilweise)",
};
