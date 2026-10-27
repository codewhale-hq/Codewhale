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
  lead: "Lass Codewhale in deinen Apps arbeiten, während du weiterarbeitest. Erlaube den Zugriff einmal auf diesem Mac und stoppe die Computernutzung jederzeit.",
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
    { title: "Computer Use aktivieren", body: "Installiere oder aktualisiere Computer Use in den Connectors-Einstellungen von Codewhale, prüfe es und aktiviere es. Bitte Codewhale, in einer App zu arbeiten. Wähle beim ersten Einsatz im nativen Helfer Trust until revoked; dieses Vertrauen gilt über Apps, Modelle und Aufgaben hinweg." },
  ],
  controlsTitle: "Du behältst die Kontrolle",
  controlsBody: "Die kompakte Codewhale-Steuerung erscheint neben der verwendeten App. Pause unterbricht die Computernutzung; Stop computer use beendet alle laufenden Computersitzungen. Mit Turn off im Helfer entziehst du Codewhale den Zugriff. Bitte um eine Live-Vorschau, wenn du zusehen möchtest.",
  updateTitle: "Updates, wann du willst",
  updateBody: "Wähle in der App Check for updates. Vor der Installation eines Updates prüft die App den Download, die Codewhale-Signatur und die Beglaubigung durch Apple und behält die vorherige App zur Wiederherstellung.",
  help: "Einrichtung und Fehlerbehebung",
  notes: "Versionshinweise",
  demo: "Hintergrundprüfung ansehen",
  source: "Quellcode und weitere Plattformen",
  platforms: "Dieser Download ist für den Mac. Windows und Linux nutzen derzeit das Quellcode-Plugin und die Einrichtung auf dem Host.",
};
