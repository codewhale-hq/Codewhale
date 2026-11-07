import type { ComputerUseDict } from "../types";

/**
 * German dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names (Codewhale,
 * Computer Use), the menu items Pause, Stop and Check for updates, and the
 * macOS setting names stay as the app and macOS show them. Register follows
 * the other German dictionaries: knapp, Anrede per Du.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Vorschau · Codewhale",
  metaDescription: "Codewhale Computer Use für den Mac herunterladen und einrichten. App-Steuerung im Hintergrund, Berechtigungen einrichten und Pause und Stop in deiner Hand.",
  title: "Computer Use",
  lead: "Lass deinen Agenten in Mac-Apps arbeiten, während du deinen Computer weiter nutzt.",
  publisher: "Von Codewhale",
  download: "Für den Mac herunterladen",
  downloadZip: "ZIP-Archiv (wird vom Updater in der App verwendet)",
  requirements: "macOS 13.5 oder neuer · Apple silicon und Intel",
  included: "Ein einziger App-Download. Keine separate Node-Installation, kein Compiler nötig.",
  pendingTitle: "Mac-Download in Vorbereitung",
  pendingBody: "Der öffentliche Installer erscheint hier, sobald die Beglaubigung durch Apple und die Release-Prüfungen abgeschlossen sind.",
  unavailableTitle: "Verfügbarkeit des Downloads konnte nicht geprüft werden",
  unavailableBody: "Lade die Seite neu, um es erneut zu versuchen, oder sieh dir unten die veröffentlichten Releases an.",
  releases: "Veröffentlichte Releases",
  receipt: "Prüfdetails zum Download",
  setup: "Deinen Mac einrichten",
  steps: [
    { title: "App installieren", body: "Öffne das Disk-Image und ziehe Codewhale Computer Use in den Ordner „Programme“. Öffne die App aus „Programme“ und wähle dann Computer Use über das Wal-Symbol in deiner Menüleiste." },
    { title: "Berechtigungen prüfen", body: "Über die Einrichtungsschaltflächen öffnest du „Bedienungshilfen“ und „Bildschirmaufnahme“ in den Systemeinstellungen. Welche Berechtigungen du erteilst, entscheidest du." },
    { title: "Deinen Agenten verbinden", body: "Installiere Computer Use in Codewhale mit /plugin marketplace install codewhale computer-use, prüfe das Plugin und aktiviere es. Der Helfer wird separat installiert. Andere kompatible MCP-Hosts verbinden sich über die Einrichtungsanleitung mit ihm." },
  ],
  controlsTitle: "Du behältst die Kontrolle",
  controlsBody: "Nach Erteilung der macOS-Berechtigungen arbeitet der Standardmodus ohne Rückfragen für jede App. Von dir gesperrte Apps bleiben gesperrt; Pause unterbricht die Computernutzung und Stop computer use beendet laufende Sitzungen. Strenge Genehmigungen sind optional. Bitte um eine Live-Vorschau, wenn du zusehen möchtest.",
  updateTitle: "Updates, wann du willst",
  updateBody: "Wähle in der App Check for updates. Vor der Installation eines Updates prüft die App den Download, die Codewhale-Signatur und die Beglaubigung durch Apple und behält die vorherige App zur Wiederherstellung.",
  help: "Einrichtung und Fehlerbehebung",
  notes: "Versionshinweise",
  demo: "Hintergrundprüfung ansehen",
  source: "Quellcode und weitere Plattformen",
  platforms: "Der Mac-Helfer ist eine Beta; das Marketplace-Plugin unterstützt nur Mac. Veröffentlichte Versionen enthalten auch eine unsignierte experimentelle Vorschau für Windows x64; Linux erfordert eine experimentelle Einrichtung aus dem Quellcode oder mit Docker. Eingaben unter Windows und Linux nutzen den gemeinsamen Desktop.",
};
