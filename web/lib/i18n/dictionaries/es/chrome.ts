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
  navContribute: "Contribuir",
  navSignIn: "Iniciar sesión",
  navDesktop: "Escritorio",

  navFeatures: "Capacidades",
  navModels: "Modelos",
  navPlugins: "Plugins",

  skipToContent: "Saltar al contenido principal",

  navPrimaryAria: "Navegación principal",
  navHomeAria: "Inicio de Codewhale",

  installCta: "Instalar →",

  dateLocale: "es-419",

  menuOpen: "Abrir menú",
  menuClose: "Cerrar menú",
  translationMachineDraft: "Esta traducción es un borrador automático que aún no ha revisado un hablante nativo.",

  themeAuto: "auto",
  themeLight: "claro",
  themeDark: "oscuro",
  themeAria: "Tema: {mode} (clic para alternar)",
  themeTitle: "Tema · auto / claro / oscuro",

  footerProduct: "Producto",
  footerProject: "Proyecto",
  footerDocs: "Documentación",
  footerGuide: "Primeros pasos",
  footerInstall: "Instalación",
  footerModels: "Modelos",
  footerFaq: "Preguntas frecuentes",
  footerContribute: "Contribuir",
  footerRoadmap: "Hoja de ruta",
  footerLicense: "Licencia MIT",
  footerMerch: "Merchandising",
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
  stateInSource: "En el código fuente, aún sin publicar",
  numbersAsOf: "A fecha de {date}",
  downloadMacos: "Descargar para macOS",
  downloadVersion: "Versión {version}",
  downloadSize: "Tamaño {size}",
  downloadChecksum: "Suma de verificación SHA-256",
  downloadSigned: "Firmado y notarizado",
};
