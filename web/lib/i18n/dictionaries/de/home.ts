import type { HomeDict } from "../types";

/** German home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale · Nutze jedes Modell mit einem Agenten, den du lesen kannst",
  metaDescription: "Codewhale ist der Open-Source-Agent zwischen dir und jedem Modell. Lies seinen Code oder lass ihn sich selbst erklären. Eine internationale Community verbessert ihn mit jeder Version.",
  heroTitle: "Nutze jedes Modell mit einem Agenten, den du lesen kannst",
  heroIntro: "{brand} ist der Open-Source-Agent zwischen dir und dem Modell. Lies seinen Code oder lass ihn seinen eigenen Code lesen und erklären, was er tut. Eine internationale Community verbessert ihn mit jeder Version.",
  getCodewhale: "Codewhale installieren",
  heroInstallAria: "Installationsbefehl",
  exploreProduct: "Codewhale entdecken",
  shotPreview: "Terminal-Vorschau",
  screenshotAlt:
    "Codewhale v{version}: eine Terminalaufnahme mit Gespräch, Nachrichteneingabe und Sitzungssteuerung.",
  latestRelease: "Aktuellstes Release {tag}",
  releaseUnavailable: "Release-Status nicht verfügbar",
  proofContributors: "{count} Mitwirkende",
  currentSource: "Quelle",
  sourceCandidate: "Unveröffentlicht",
  publishedRelease: "veröffentlicht",
  gainHeading: "Entwickle und automatisiere in deinem Terminal",
  gainLede:
    "Beschreibe, was du erstellen oder automatisieren möchtest. Codewhale kann Dateien bearbeiten, Befehle ausführen und das Ergebnis prüfen. Du bestimmst, worauf es zugreifen darf.",
  gain: [
    [
      "Apps und Werkzeuge bauen",
      "Erstelle eine App, füge eine Funktion hinzu oder schreibe ein Skript. Codewhale kann mit den Projektdateien arbeiten, den Code ausführen und testen, was es erstellt."
    ],
    [
      "Wiederkehrende Arbeit automatisieren",
      "Führe Workflows über dein Terminal, Skripte oder CI aus. Bei größeren Aufgaben kannst du Teile der Arbeit an eine Fleet von Agenten mit unterschiedlichen Modellen delegieren."
    ],
    [
      "Deine Werkzeuge verbinden",
      "Füge Tools über Plugins und MCP-Server hinzu oder nutze APIs in deinen eigenen Skripten. Jeder Dienst benötigt eine eigene Einrichtung und Authentifizierung."
    ]
  ],
  chapterModels: "Deine Modelle",
  modelsHeading: "Nutze die Modelle deiner Wahl",
  modelsBody:
    "Verbinde deine Anbieterkonten, einen OpenAI-kompatiblen Endpunkt oder lokale und selbst gehostete Modelle. Wähle ein Modell für die Sitzung und für jeden Agenten in einer Fleet.",
  modelsFacts: [
    [
      "Deine API-Konten",
      "Verbinde Anbieter wie DeepSeek, OpenAI, Anthropic oder Google mit deinen eigenen Schlüsseln. Das Standardmodell deepseek-flash verwendet jetzt DeepSeek-V4.1-Flash."
    ],
    [
      "Dein Gateway",
      "Nutze einen OpenAI-kompatiblen Endpoint und wähle dessen Modelle."
    ],
    [
      "Deine Inferenz",
      "Betreibe lokale oder selbst gehostete Modelle mit Ollama, vLLM oder SGLang."
    ],
    [
      "Dein ChatGPT-Abo",
      "Melde dich mit codewhale auth chatgpt an, um ein ChatGPT-Abo anstelle eines API-Schlüssels zu nutzen. Experimentell."
    ]
  ],
  modelsLink: "Modelle und Anbieter durchsuchen",
  startHeading: "Erste Schritte",
  startLede:
    "Installiere Codewhale, verbinde ein Modell und öffne einen Projektordner. Plugins und weitere Agenten kannst du nach Bedarf hinzufügen.",
  startGuideLink: "Dem Leitfaden für die ersten Schritte folgen",
  startVocabularyLink: "Produktvokabular ansehen",
  chapterAvailability: "Wo es läuft",
  availabilityHeading: "Jetzt verfügbar und in Entwicklung",
  availabilityLede:
    "Terminal und lokaler Browser-Client sind verfügbar. Eine native Desktop-App und eine neu aufgebaute gehostete Web-App sind in Entwicklung.",
  availability: [
    [
      "Terminal und lokaler Browser",
      "Veröffentlicht",
      "Installiere Codewhale unter Linux, macOS oder Windows und führe dann codewhale aus, oder codewhale web für den lokalen Browser-Client. npm und Cargo funktionieren ebenfalls; Android unter Termux ist eine Vorschau."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Verfügbar",
      "Ein separates, von der Community gepflegtes Projekt: Chat, Threads und Dateiänderungen in einer VS Code-Seitenleiste über dieselbe Codewhale Runtime. Installiere sie über den VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "Gehostete Web-App",
      "In Entwicklung",
      "Wird neu aufgebaut, damit sie zur Desktop-App passt, und ist noch nicht geöffnet. Für eine Browseransicht kannst du schon jetzt codewhale web auf deinem Rechner ausführen."
    ],
    [
      "Desktop-App",
      "Entwicklungsbuild",
      "Eine native App mit derselben Engine. Prüfe und committe Änderungen, verfolge Arbeit über Projekte hinweg und bearbeite eine Nachricht, um sie als neuen Zweig erneut zu senden. Einen öffentlichen Download gibt es noch nicht."
    ]
  ],
  availabilityNote:
    "Terminal, lokaler Browser und GUI brauchen kein Codewhale-Konto. Wenn du deinen eigenen Anbieterschlüssel verwendest, rechnet der Anbieter diese Nutzung ab.",
  surfacesHeading: "Arbeite mit Dateien und Tools",
  surfaces: [
    [
      "Dateien und Terminal",
      "Erstelle Dateien, führe Befehle aus, untersuche Daten und teste deine Ergebnisse. Arbeitsordner und Berechtigungen bestimmst du."
    ],
    [
      "Plugins und verbundene Apps",
      "Ergänze Skills und Werkzeuge über Plugins und MCP. Prüfe und aktiviere die Verbindungen, die dein Agent nutzen darf."
    ],
    [
      "Browser und Computer · Vorschau",
      "Nutze Browser-Werkzeuge und das Computer-Use-Plugin für Apps und Websites mit dem Zugriff, den du erlaubst."
    ],
    [
      "Sitzungen zum Fortsetzen",
      "Behalte Gespräch, Werkzeugergebnisse und Arbeitsverlauf zusammen. Setze die Aufgabe im Terminal oder lokalen Browser-Client fort."
    ],
    [
      "Agententeams",
      "Verteile größere Aufgaben mit Fleet auf Agenten mit verschiedenen Rollen und Modellen. Verfolge ihren Fortschritt an einem Ort."
    ]
  ],
  runtimeLink: "Tools und Integrationen entdecken",
  installBandHeading: "Installation unter macOS oder Linux",
  copy: "Kopieren",
  copied: "Kopiert ✓",
  binaries: "Binärdateien",
  chinaMirrors: "China-Mirrors",
  installGuideLink: "Installationsleitfaden lesen",
  contribute: "Auf GitHub mitwirken",
};
