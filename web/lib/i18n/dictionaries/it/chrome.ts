import type { ChromeDict } from "../types";

/**
 * Italian chrome dictionary.
 *
 * Riscrittura nativa nella direzione inglese attuale — «any model, on your
 * machine», non il ritirato posizionamento «local-first». Registro con il
 * tu, lo standard dei tool per sviluppatori in italiano.
 *
 * I modi e le posture di permesso restano letterali (Plan / Work / Operate,
 * Ask / Auto-Review / Full Access); `Runtime`, `fleet` e `TUI` restano nomi
 * di prodotto; «ricevuta» è receipt.
 *
 * Le etichette secondarie di navigazione abbinano l'etichetta italiana a
 * una breve controparte inglese — la coppia Han è il dispositivo editoriale
 * dell'edizione inglese.
 */
export const chrome: ChromeDict = {
  navDocs: "Documentazione",
  navStart: "Inizia",
  navInstall: "Installa",
  navFaq: "FAQ",
  navCommunity: "Comunità",
  navContribute: "Contribuisci",

  navFeatures: "Capacità",
  navProduct: "Prodotto",
  navModels: "Modelli",
  navPlugins: "Plugin",

  skipToContent: "Vai al contenuto principale",

  navPrimaryAria: "Navigazione principale",
  navHomeAria: "Home di Codewhale",

  installCta: "Installa →",

  dateLocale: "it-IT",

  menuOpen: "Apri il menu",
  menuClose: "Chiudi il menu",

  themeAuto: "auto",
  themeLight: "chiaro",
  themeDark: "scuro",
  themeAria: "Tema: {mode} (clic per cambiare)",
  themeTitle: "Tema · auto / chiaro / scuro",

  footerTagline: "Usa qualsiasi modello con un agente che puoi leggere.",
  footerProduct: "Prodotto",
  footerProject: "Progetto",
  footerDocs: "Documentazione",
  footerGuide: "Primi passi",
  footerInstall: "Installazione",
  footerApps: "Desktop e web",
  footerModels: "Modelli",
  footerRuntime: "Runtime",
  footerFaq: "FAQ",
  footerIssues: "Issues",
  footerContribute: "Contribuisci",
  footerRoadmap: "Roadmap",
  footerLicense: "Licenza MIT",
  footerTerms: "Termini di servizio",
  footerPrivacy: "Privacy",
  footerChangelog: "Registro delle modifiche",
  footerCanonicalSource: "Sorgente canonico: ",
  footerReleases: " · Release: ",
  footerReleasesLink: "Release di GitHub",
  footerSecurity: "Sicurezza",
  footerLegal: "Note legali",

  switcherLabel: "Lingua",
  switcherSwitchTo: "Passa a {label}",
  partialBadge: "(parziale)",

  stateReleased: "Rilasciato",
  stateAvailable: "Disponibile",
  stateBeta: "Beta",
  stateDev: "Build di sviluppo",
  stateOpen: "Aperto",
  stateInviteOnly: "Solo su invito",
  stateNotOpen: "Non ancora aperto",
  stateNotPublic: "Non pubblico",
  numbersAsOf: "Al {date}",
  downloadMacos: "Scarica per macOS",
  downloadVersion: "Versione {version}",
  downloadSize: "Dimensione {size}",
  downloadChecksum: "Checksum SHA-256",
  downloadSigned: "Firmato e notarizzato",
};
