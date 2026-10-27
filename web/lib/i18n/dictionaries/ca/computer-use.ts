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
  lead: "Deixa que Codewhale treballi a les teves aplicacions mentre continues treballant. Dona-li permís una vegada en aquest Mac i atura l’ús de l’ordinador quan vulguis.",
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
    { title: "Activa Computer Use", body: "A la configuració de Connectors de Codewhale, instal·la o actualitza Computer Use i, després, revisa’l i activa’l. Demana-li que treballi en una aplicació. En el primer ús, tria Trust until revoked a la finestra nativa de l’ajudant; aquesta confiança es manté entre aplicacions, models i tasques." },
  ],
  controlsTitle: "Mantén el control",
  controlsBody: "El control compacte de Codewhale apareix al costat de l’aplicació en ús. Pause suspèn l’ús de l’ordinador; Stop computer use finalitza totes les sessions actuals de l’ordinador. Tria Turn off a l’ajudant per retirar l’accés de Codewhale. Demana una previsualització en directe quan vulguis veure què fa.",
  updateTitle: "Actualitzacions quan tu vulguis",
  updateBody: "Tria Check for updates (Cerca actualitzacions) a l’aplicació. Abans d’instal·lar una actualització, comprova la baixada, la signatura de Codewhale i la notarització d’Apple, i conserva l’aplicació anterior per si cal recuperar-la.",
  help: "Configuració i resolució de problemes",
  notes: "Notes de la versió",
  demo: "Mira la comprovació en segon pla",
  source: "Codi font i altres plataformes",
  platforms: "Aquesta baixada és per a Mac. Windows i Linux fan servir de moment el connector des del codi font i la configuració a l’equip amfitrió.",
};
