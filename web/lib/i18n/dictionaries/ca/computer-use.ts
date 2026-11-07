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
    { title: "Connecta el teu agent", body: "A Codewhale, instal·la Computer Use amb /plugin marketplace install codewhale computer-use i, després, revisa i activa el connector. L’ajudant s’instal·la per separat. Altres amfitrions MCP compatibles s’hi connecten seguint la guia de configuració." },
  ],
  controlsTitle: "Mantén el control",
  controlsBody: "Després de concedir els permisos de macOS, el mode estàndard funciona sense demanar permís per a cada app. Les apps que bloquegis continuen fora de l’abast; Pause suspèn l’ús de l’ordinador i Stop computer use finalitza les sessions actuals. L’aprovació estricta és opcional. Demana una previsualització en directe quan vulguis veure què fa.",
  updateTitle: "Actualitzacions quan tu vulguis",
  updateBody: "Tria Check for updates (Cerca actualitzacions) a l’aplicació. Abans d’instal·lar una actualització, comprova la baixada, la signatura de Codewhale i la notarització d’Apple, i conserva l’aplicació anterior per si cal recuperar-la.",
  help: "Configuració i resolució de problemes",
  notes: "Notes de la versió",
  demo: "Mira la comprovació en segon pla",
  source: "Codi font i altres plataformes",
  platforms: "L’ajudant del Mac és una beta i el connector del catàleg només admet Mac. Les versions publicades també inclouen una previsualització experimental sense signar per a Windows x64; Linux requereix una configuració experimental des del codi font o amb Docker. L’entrada a Windows i Linux utilitza l’escriptori compartit.",
};
