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
  navContribute: "Mitwirken",
  navSignIn: "Anmelden",
  navDesktop: "Desktop",

  navFeatures: "Funktionen",
  navModels: "Modelle",
  navPlugins: "Plugins",

  skipToContent: "Zum Hauptinhalt springen",

  navPrimaryAria: "Hauptnavigation",
  navHomeAria: "Codewhale-Startseite",

  installCta: "Installieren →",

  dateLocale: "de-DE",

  menuOpen: "Menü öffnen",
  menuClose: "Menü schließen",
  translationMachineDraft: "Diese Übersetzung ist ein maschineller Entwurf und wurde noch nicht von Muttersprachlern geprüft.",

  themeAuto: "auto",
  themeLight: "hell",
  themeDark: "dunkel",
  themeAria: "Design: {mode} (Klick zum Wechseln)",
  themeTitle: "Design · auto / hell / dunkel",

  footerProduct: "Produkt",
  footerProject: "Projekt",
  footerDocs: "Dokumentation",
  footerGuide: "Erste Schritte",
  footerInstall: "Installation",
  footerModels: "Modelle",
  footerFaq: "FAQ",
  footerContribute: "Mitwirken",
  footerRoadmap: "Roadmap",
  footerLicense: "MIT-Lizenz",
  footerMerch: "Merchandise",
  footerTerms: "Nutzungsbedingungen",
  footerPrivacy: "Datenschutz",
  footerChangelog: "Änderungsprotokoll",
  footerCanonicalSource: "Kanonische Quelle: ",
  footerReleases: " · Releases: ",
  footerReleasesLink: "GitHub-Releases",
  footerSecurity: "Sicherheit",
  footerLegal: "Rechtliches",

  switcherLabel: "Sprache",
  switcherSwitchTo: "Zu {label} wechseln",
  partialBadge: "(teilweise)",

  stateReleased: "Veröffentlicht",
  stateAvailable: "Verfügbar",
  stateBeta: "Beta",
  stateDev: "Entwicklungsversion",
  stateOpen: "Offen",
  stateInviteOnly: "Nur auf Einladung",
  stateNotOpen: "Noch nicht geöffnet",
  stateNotPublic: "Nicht öffentlich",
  stateInSource: "Im Quellcode, noch nicht veröffentlicht",
  numbersAsOf: "Stand: {date}",
  downloadMacos: "Für macOS herunterladen",
  downloadVersion: "Version {version}",
  downloadSize: "Größe {size}",
  downloadChecksum: "SHA-256-Prüfsumme",
  downloadSigned: "Signiert und notarisiert",
};
