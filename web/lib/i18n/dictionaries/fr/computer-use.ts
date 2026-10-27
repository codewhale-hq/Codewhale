import type { ComputerUseDict } from "../types";

/**
 * French dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them. Vouvoiement, typographic apostrophe (’) and French
 * guillemets, in line with the other French dictionaries.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Aperçu · Codewhale",
  metaDescription: "Téléchargez et configurez Codewhale Computer Use pour Mac : contrôle des apps en arrière-plan, configuration des autorisations et commandes Pause et Stop à portée de main.",
  title: "Computer Use",
  lead: "Laissez Codewhale travailler dans vos apps pendant que vous continuez votre travail. Autorisez-le une fois sur ce Mac et arrêtez l’utilisation de l’ordinateur quand vous le souhaitez.",
  publisher: "Par Codewhale",
  download: "Télécharger pour Mac",
  downloadZip: "Archive ZIP (utilisée par la mise à jour intégrée)",
  requirements: "macOS 13.5 ou ultérieur · Apple silicon et Intel",
  included: "Une seule app à télécharger. Aucune installation séparée de Node ni de compilateur.",
  pendingTitle: "Téléchargement Mac en préparation",
  pendingBody: "Le programme d’installation public apparaîtra ici une fois la notarisation Apple et les vérifications de publication terminées.",
  unavailableTitle: "Impossible de vérifier la disponibilité du téléchargement",
  unavailableBody: "Actualisez cette page pour réessayer, ou consultez les versions publiées ci-dessous.",
  releases: "Versions publiées",
  receipt: "Détails de vérification du téléchargement",
  setup: "Configurer votre Mac",
  steps: [
    { title: "Installer l’app", body: "Ouvrez l’image disque et faites glisser Codewhale Computer Use dans « Applications ». Lancez-la depuis « Applications », puis choisissez Computer Use dans l’icône de baleine de votre barre des menus." },
    { title: "Vérifier les autorisations", body: "Les boutons de configuration ouvrent « Accessibilité » et « Enregistrement de l’écran » dans Réglages Système. C’est vous qui décidez des autorisations à accorder." },
    { title: "Activer Computer Use", body: "Dans les réglages Connectors de Codewhale, installez ou mettez à jour Computer Use, puis examinez-le et activez-le. Demandez-lui de travailler dans une app. À la première utilisation, choisissez Trust until revoked dans l’assistant natif ; cette confiance reste valable entre les apps, les modèles et les tâches." },
  ],
  controlsTitle: "Gardez la main",
  controlsBody: "La commande compacte Codewhale apparaît près de l’app utilisée. Pause suspend l’utilisation de l’ordinateur ; Stop computer use termine toutes les sessions en cours. Choisissez Turn off dans l’assistant pour retirer l’accès de Codewhale. Demandez un aperçu en direct lorsque vous souhaitez suivre les opérations.",
  updateTitle: "Des mises à jour quand vous le décidez",
  updateBody: "Choisissez Check for updates (rechercher les mises à jour) dans l’app. Avant d’installer une mise à jour, l’assistant vérifie le téléchargement, la signature Codewhale et la notarisation Apple, et conserve la version précédente pour pouvoir la restaurer.",
  help: "Configuration et dépannage",
  notes: "Notes de version",
  demo: "Voir la vérification en arrière-plan",
  source: "Code source et autres plateformes",
  platforms: "Ce téléchargement est destiné au Mac. Sous Windows et Linux, on utilise pour l’instant le plugin depuis les sources et la configuration côté hôte.",
};
