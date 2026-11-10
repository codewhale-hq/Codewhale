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
  navStart: "Començar",
  navInstall: "Instal·lar",
  navFaq: "PMF",
  navCommunity: "Comunitat",
  navContribute: "Col·laborar",

  navFeatures: "Capacitats",
  navProduct: "Producte",
  navModels: "Models",
  navPlugins: "Plugins",

  skipToContent: "Salta al contingut principal",

  navPrimaryAria: "Navegació principal",
  navHomeAria: "Inici de Codewhale",

  installCta: "Instal·la →",

  dateLocale: "ca-ES",

  menuOpen: "Obre el menú",
  menuClose: "Tanca el menú",

  themeAuto: "auto",
  themeLight: "clar",
  themeDark: "fosc",
  themeAria: "Tema: {mode} (fes clic per canviar)",
  themeTitle: "Tema · auto / clar / fosc",

  footerTagline:
    "L’agent de programació de codi obert per al teu terminal i els models que triïs.",
  footerProduct: "Producte",
  footerProject: "Projecte",
  footerDocs: "Documentació",
  footerGuide: "Primers passos",
  footerInstall: "Instal·lació",
  footerModels: "Models",
  footerRuntime: "Runtime",
  footerFaq: "PMF",
  footerIssues: "Issues",
  footerContribute: "Col·laborar",
  footerRoadmap: "Full de ruta",
  footerLicense: "Llicència MIT",
  footerTerms: "Termes del servei",
  footerPrivacy: "Privadesa",
  footerChangelog: "Registre de canvis",
  footerCanonicalSource: "Font canònica: ",
  footerReleases: " · Versions: ",
  footerReleasesLink: "Versions de GitHub",
  footerSecurity: "Seguretat",

  switcherLabel: "Llengua",
  switcherSwitchTo: "Canvia a {label}",
  partialBadge: "(parcial)",
};
