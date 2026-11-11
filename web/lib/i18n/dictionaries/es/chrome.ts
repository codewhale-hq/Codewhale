import type { ChromeDict } from "../types";

/**
 * Spanish chrome dictionary — native rewrite in neutral (pan-Hispanic)
 * Spanish, informal `tú`, matching the register of the es-419 TUI pack and
 * mirroring the current English direction (bring-your-own-model, runs on
 * your machine; the old positioning is gone).
 *
 * Terminology is aligned with crates/tui/locales/es-419.json so the website
 * and the terminal name the same things the same way: modes stay literal
 * (Plan / Work / Operate), permission postures stay literal (Ask /
 * Auto-Review / Full Access) under "postura de permisos", `Runtime`,
 * `fleet`, and `Workflow` stay literal product nouns, and "receipt" is
 * "recibo".
 */
export const chrome: ChromeDict = {
  navDocs: "Documentación",
  navStart: "Empezar",
  navInstall: "Instalar",
  navFaq: "Preguntas",
  navCommunity: "Comunidad",
  navContribute: "Contribuir",

  navFeatures: "Capacidades",
  navProduct: "Producto",
  navModels: "Modelos",
  navPlugins: "Plugins",

  skipToContent: "Saltar al contenido principal",

  navPrimaryAria: "Navegación principal",
  navHomeAria: "Inicio de Codewhale",

  installCta: "Instalar →",

  dateLocale: "es-419",

  menuOpen: "Abrir menú",
  menuClose: "Cerrar menú",

  themeAuto: "auto",
  themeLight: "claro",
  themeDark: "oscuro",
  themeAria: "Tema: {mode} (clic para alternar)",
  themeTitle: "Tema · auto / claro / oscuro",

  footerTagline: "Usa cualquier modelo con un agente que puedes leer.",
  footerProduct: "Producto",
  footerProject: "Proyecto",
  footerDocs: "Documentación",
  footerGuide: "Primeros pasos",
  footerInstall: "Instalación",
  footerApps: "Escritorio y web",
  footerModels: "Modelos",
  footerRuntime: "Runtime",
  footerFaq: "Preguntas frecuentes",
  footerIssues: "Incidencias",
  footerContribute: "Contribuir",
  footerRoadmap: "Hoja de ruta",
  footerLicense: "Licencia MIT",
  footerTerms: "Términos del servicio",
  footerPrivacy: "Privacidad",
  footerChangelog: "Registro de cambios",
  footerCanonicalSource: "Fuente canónica: ",
  footerReleases: " · Lanzamientos: ",
  footerReleasesLink: "Lanzamientos en GitHub",
  footerSecurity: "Seguridad",
  footerLegal: "Legal",

  switcherLabel: "Idioma",
  switcherSwitchTo: "Cambiar a {label}",
  partialBadge: "(parcial)",

  stateReleased: "Publicado",
  stateAvailable: "Disponible",
  stateBeta: "Beta",
  stateDev: "Compilación de desarrollo",
  stateOpen: "Abierto",
  stateInviteOnly: "Solo por invitación",
  stateNotOpen: "Aún no abierto",
  stateNotPublic: "No público",
  numbersAsOf: "A fecha de {date}",
  downloadMacos: "Descargar para macOS",
  downloadVersion: "Versión {version}",
  downloadSize: "Tamaño {size}",
  downloadChecksum: "Suma de verificación SHA-256",
  downloadSigned: "Firmado y notarizado",
};
