import type { ComputerUseDict } from "../types";

/**
 * Catalan dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them; the macOS settings and the Applications folder use
 * the names the Catalan system UI shows.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Previsualització · Codewhale",
  metaDescription: "Baixa i configura Codewhale Computer Use per a Mac. Control d’aplicacions en segon pla, configuració de permisos i controls humans de Pause i Stop.",
  title: "Computer Use",
  lead: "Deixa que el teu agent treballi a les apps del Mac mentre continues fent servir l’ordinador.",
  publisher: "De Codewhale",
  download: "Baixa per a Mac",
  downloadZip: "Arxiu ZIP (el fa servir l’actualitzador integrat)",
  requirements: "macOS 13.5 o posterior · Apple silicon i Intel",
  included: "Una sola baixada. No cal instal·lar Node ni cap compilador a part.",
  pendingTitle: "Baixada per a Mac en preparació",
  pendingBody: "L’instal·lador públic apareixerà aquí quan s’hagin completat la notarització d’Apple i les comprovacions de publicació.",
  unavailableTitle: "No s’ha pogut comprovar la disponibilitat de la baixada",
  unavailableBody: "Actualitza la pàgina per tornar-ho a provar o consulta les versions publicades a sota.",
  releases: "Versions publicades",
  receipt: "Detalls de verificació de la baixada",
  setup: "Configura el teu Mac",
  steps: [
    { title: "Instal·la l’aplicació", body: "Obre la imatge de disc i arrossega Codewhale Computer Use a «Aplicacions». Obre-la des d’«Aplicacions» i tria Computer Use a la icona de la balena de la barra de menús." },
    { title: "Revisa els permisos", body: "Fes servir els botons de configuració per obrir «Accessibilitat» i «Gravació de pantalla» a Configuració del Sistema. Tu decideixes quins permisos concedeixes." },
    { title: "Connecta el teu agent", body: "Tria a continuació la configuració de Codewhale per a la teva versió de la CLI. Els altres clients MCP compatibles poden fer servir la guia de configuració del repositori." },
  ],
  controlsTitle: "Mantén el control",
  controlsBody: "Després de concedir els permisos de macOS, el mode estàndard funciona sense demanar permís per a cada app. Les apps que bloquegis continuen fora de l’abast; Pause suspèn l’ús de l’ordinador i Stop computer use finalitza les sessions actuals. L’aprovació estricta és opcional. Demana una previsualització en directe quan vulguis veure què fa.",
  updateTitle: "Actualitzacions quan tu vulguis",
  updateBody: "Tria Check for updates (Cerca actualitzacions) a l’aplicació. Abans d’instal·lar una actualització, comprova la baixada, la signatura de Codewhale i la notarització d’Apple, i conserva l’aplicació anterior per si cal recuperar-la.",
  help: "Configuració i resolució de problemes",
  notes: "Notes de la versió",
  demo: "Mira la comprovació en segon pla",
  source: "Veure a GitHub",
  platforms: "L’ajudant del Mac és una beta i el connector del catàleg només admet Mac. Les versions publicades també inclouen una previsualització experimental sense signar per a Windows x64; Linux requereix una configuració experimental des del codi font o amb Docker. L’entrada a Windows i Linux utilitza l’escriptori compartit.",
  connectTitle: "Connecta Codewhale",
  connectVersion: "Codewhale 0.10.2 inclou un connector Computer Use antic. Instal·lar l’entrada del catàleg no l’actualitza. Connecta directament l’auxiliar de Mac instal·lat mitjançant MCP.",
  connectLimit: "La connexió MCP directa afegeix eines sense les habilitats del connector. No la facis servir alhora que el connector integrat antic. Si està activat, executa primer /plugin disable computer-use.",
  connectRuntime: "L’aplicació inclou Node; no cal instal·lar-lo per separat. Si està instal·lada en una altra ubicació, assigna a APP el camí real del paquet de l’aplicació.",
  connectReload: "En una sessió de terminal en execució, executa /mcp reload i demana:",
  connectPrompt: "Comprova els permisos de Computer Use amb request_access. No enviïs cap entrada.",
  connectExpected: "Comprova via: app, la versió de l’auxiliar i els permisos que faltin a la resposta. Si no es troba l’auxiliar, atura’t i segueix la indicació de configuració. Això només comprova la connexió i els permisos.",
  connectGuide: "Guia de configuració MCP",
  contributeTitle: "Ajuda a millorar Computer Use",
  contributeBody: "Millora la documentació o prova un flux de treball en una aplicació i comparteix resultats reproduïbles.",
  contributeGuide: "Guia de contribució",
  contributeIssue: "Informar d’un problema",
};
