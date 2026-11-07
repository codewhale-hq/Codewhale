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
  lead: "Laissez votre agent travailler dans les apps Mac pendant que vous continuez à utiliser votre ordinateur.",
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
    { title: "Connecter votre agent", body: "Dans Codewhale, installez Computer Use avec /plugin marketplace install codewhale computer-use, puis examinez et activez le plugin. L’assistant s’installe séparément. Les autres hôtes MCP compatibles s’y connectent en suivant le guide de configuration." },
  ],
  controlsTitle: "Gardez la main",
  controlsBody: "Après l’octroi des autorisations macOS, le mode standard fonctionne sans demander une autorisation pour chaque app. Les apps que vous bloquez restent inaccessibles ; Pause suspend l’utilisation de l’ordinateur et Stop computer use termine les sessions en cours. L’approbation stricte est facultative. Demandez un aperçu en direct pour suivre les opérations.",
  updateTitle: "Des mises à jour quand vous le décidez",
  updateBody: "Choisissez Check for updates (rechercher les mises à jour) dans l’app. Avant d’installer une mise à jour, l’assistant vérifie le téléchargement, la signature Codewhale et la notarisation Apple, et conserve la version précédente pour pouvoir la restaurer.",
  help: "Configuration et dépannage",
  notes: "Notes de version",
  demo: "Voir la vérification en arrière-plan",
  source: "Code source et autres plateformes",
  platforms: "L’assistant Mac est en bêta et le plugin du catalogue ne prend en charge que Mac. Les versions publiées comprennent aussi un aperçu expérimental non signé pour Windows x64 ; Linux nécessite une configuration expérimentale depuis les sources ou avec Docker. Les entrées sous Windows et Linux utilisent le bureau partagé.",
};
