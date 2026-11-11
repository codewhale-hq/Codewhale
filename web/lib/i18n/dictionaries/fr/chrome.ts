import type { ChromeDict } from "../types";

/**
 * French chrome dictionary.
 *
 * Native rewrite mirroring the current English direction — « any model, on
 * your machine », pas l'ancien positionnement « local-first ». Registre en
 * vouvoiement, aligné sur le pack TUI (crates/tui/locales/fr.json) :
 * « Exécutez `{command}` pour confirmer ».
 *
 * Terminologie alignée sur le pack TUI : les modes et les postures de
 * permissions restent littéraux (Plan / Work / Operate, Ask / Auto-Review /
 * Full Access), « receipt » est « reçu », « runtime », `fleet` et `TUI`
 * restent des noms produits. L'apostrophe est typographique (’).
 *
 * Les libellés secondaires de navigation associent le libellé français à un
 * court équivalent anglais — le couple han (文档 / 指引 / …) est le procédé
 * éditorial propre à l'édition anglaise.
 */
export const chrome: ChromeDict = {
  navDocs: "Documentation",
  navContribute: "Contribuer",
  navSignIn: "Se connecter",
  navDesktop: "Bureau",

  navFeatures: "Capacités",
  navModels: "Modèles",
  navPlugins: "Plugins",

  skipToContent: "Aller au contenu principal",

  navPrimaryAria: "Navigation principale",
  navHomeAria: "Accueil de Codewhale",

  installCta: "Installer →",

  dateLocale: "fr-FR",

  menuOpen: "Ouvrir le menu",
  menuClose: "Fermer le menu",
  translationMachineDraft: "Cette traduction est un brouillon automatique qui n'a pas encore été relu par un locuteur natif.",

  themeAuto: "auto",
  themeLight: "clair",
  themeDark: "sombre",
  themeAria: "Thème : {mode} (cliquer pour changer)",
  themeTitle: "Thème · auto / clair / sombre",

  footerProduct: "Produit",
  footerProject: "Projet",
  footerDocs: "Documentation",
  footerGuide: "Premiers pas",
  footerInstall: "Installation",
  footerModels: "Modèles",
  footerFaq: "FAQ",
  footerContribute: "Contribuer",
  footerRoadmap: "Feuille de route",
  footerLicense: "Licence MIT",
  footerMerch: "Boutique",
  footerTerms: "Conditions d’utilisation",
  footerPrivacy: "Confidentialité",
  footerChangelog: "Journal des modifications",
  footerCanonicalSource: "Source canonique : ",
  footerReleases: " · Versions : ",
  footerReleasesLink: "Versions GitHub",
  footerSecurity: "Sécurité",
  footerLegal: "Mentions légales",

  switcherLabel: "Langue",
  switcherSwitchTo: "Passer en {label}",
  partialBadge: "(partiel)",

  stateReleased: "Publié",
  stateAvailable: "Disponible",
  stateBeta: "Bêta",
  stateDev: "Version de développement",
  stateOpen: "Ouvert",
  stateInviteOnly: "Sur invitation uniquement",
  stateNotOpen: "Pas encore ouvert",
  stateNotPublic: "Non public",
  stateInSource: "Dans le code source, pas encore publié",
  numbersAsOf: "Au {date}",
  downloadMacos: "Télécharger pour macOS",
  downloadVersion: "Version {version}",
  downloadSize: "Taille {size}",
  downloadChecksum: "Somme de contrôle SHA-256",
  downloadSigned: "Signé et notarisé",
};
