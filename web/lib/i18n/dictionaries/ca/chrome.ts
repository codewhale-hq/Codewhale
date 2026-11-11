import type { ChromeDict } from "../types";

/**
 * Catalan chrome dictionary.
 *
 * Reescriptura nativa en la direcció anglesa actual — «any model, on your
 * machine», no el posicionament «local-first» retirat. Registre amb tu,
 * com el pack TUI (crates/tui/locales/ca.json): «Executa `{command}` per
 * confirmar».
 *
 * Terminologia alineada amb el pack TUI: els modes i les postures de
 * permisos es mantenen literals (Plan / Work / Operate, Ask / Auto-Review /
 * Full Access), «permisos» és permissions, «resguard» és receipt; `Runtime`,
 * `fleet` i `TUI` resten noms de producte.
 *
 * Les etiquetes secundàries de navegació aparien l’etiqueta catalana amb
 * un equivalent anglès curt — la parella han és el recurs editorial propi
 * de l’edició anglesa.
 */
export const chrome: ChromeDict = {
  navDocs: "Documentació",
  navContribute: "Col·laborar",
  navSignIn: "Inicia la sessió",
  navDesktop: "Escriptori",

  navFeatures: "Capacitats",
  navModels: "Models",
  navPlugins: "Plugins",

  skipToContent: "Salta al contingut principal",

  navPrimaryAria: "Navegació principal",
  navHomeAria: "Inici de Codewhale",

  installCta: "Instal·la →",

  dateLocale: "ca-ES",

  menuOpen: "Obre el menú",
  menuClose: "Tanca el menú",
  translationMachineDraft: "Aquesta traducció és un esborrany automàtic que encara no ha revisat cap parlant nadiu.",

  themeAuto: "auto",
  themeLight: "clar",
  themeDark: "fosc",
  themeAria: "Tema: {mode} (fes clic per canviar)",
  themeTitle: "Tema · auto / clar / fosc",

  footerProduct: "Producte",
  footerProject: "Projecte",
  footerDocs: "Documentació",
  footerGuide: "Primers passos",
  footerInstall: "Instal·lació",
  footerModels: "Models",
  footerFaq: "PMF",
  footerContribute: "Col·laborar",
  footerRoadmap: "Full de ruta",
  footerLicense: "Llicència MIT",
  footerMerch: "Productes",
  footerTerms: "Termes del servei",
  footerPrivacy: "Privadesa",
  footerChangelog: "Registre de canvis",
  footerCanonicalSource: "Font canònica: ",
  footerReleases: " · Versions: ",
  footerReleasesLink: "Versions de GitHub",
  footerSecurity: "Seguretat",
  footerLegal: "Legal",

  switcherLabel: "Llengua",
  switcherSwitchTo: "Canvia a {label}",
  partialBadge: "(parcial)",

  stateReleased: "Publicat",
  stateAvailable: "Disponible",
  stateBeta: "Beta",
  stateDev: "Versió de desenvolupament",
  stateOpen: "Obert",
  stateInviteOnly: "Només per invitació",
  stateNotOpen: "Encara no obert",
  stateNotPublic: "No públic",
  stateInSource: "Al codi font, encara no publicat",
  numbersAsOf: "A data de {date}",
  downloadMacos: "Baixa per a macOS",
  downloadVersion: "Versió {version}",
  downloadSize: "Mida {size}",
  downloadChecksum: "Suma de verificació SHA-256",
  downloadSigned: "Signat i notaritzat",
};
