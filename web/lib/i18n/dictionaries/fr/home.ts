import type { HomeDict } from "../types";

/** French home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale : agent de programmation open source",
  metaDescription: "Codewhale est un agent de programmation open source pour votre terminal. Lisez, modifiez et testez des projets avec vos modèles hébergés ou locaux.",
  heroTitle: "L’agent de programmation open source pour tous les modèles",
  heroIntro: "{brand} lit votre projet, modifie les fichiers et exécute vos tests depuis votre terminal. Connectez un modèle hébergé ou local et choisissez les actions qui nécessitent votre approbation.",
  getCodewhale: "Installer Codewhale",
  heroInstallAria: "Commande d'installation",
  exploreProduct: "Découvrir Codewhale",
  shotPreview: "Aperçu du terminal",
  screenshotAlt:
    "Codewhale v{version} : capture du terminal avec la conversation, le champ de message et les commandes de session.",
  latestRelease: "Dernière version {tag}",
  releaseUnavailable: "État des versions indisponible",
  proofContributors: "{count} contributeurs",
  currentSource: "Source",
  sourceCandidate: "Non publiée",
  publishedRelease: "publiée",
  gainHeading: "Créez et automatisez depuis votre terminal",
  gainLede:
    "Décrivez ce que vous voulez créer ou automatiser. Codewhale peut modifier des fichiers, exécuter des commandes et vérifier le résultat, avec des accès que vous contrôlez.",
  gain: [
    [
      "Créez des applications et des outils",
      "Créez une application, ajoutez une fonctionnalité ou écrivez un script. Codewhale peut travailler sur les fichiers du projet, exécuter le code et tester ce qu’il crée."
    ],
    [
      "Automatisez les tâches répétitives",
      "Exécutez des workflows depuis votre terminal, vos scripts ou votre CI. Pour les tâches plus importantes, déléguez une partie du travail à une Fleet d’agents utilisant différents modèles."
    ],
    [
      "Connectez vos outils habituels",
      "Ajoutez des outils via des plugins et des serveurs MCP, ou utilisez des API depuis vos propres scripts. Chaque service nécessite sa propre configuration et authentification."
    ]
  ],
  chapterModels: "Vos modèles",
  modelsHeading: "Utilisez les modèles de votre choix",
  modelsBody:
    "Connectez vos comptes fournisseurs, un point de terminaison compatible avec OpenAI, ou des modèles locaux et auto-hébergés. Choisissez un modèle pour la session et pour chaque agent d’une Fleet.",
  modelsFacts: [
    [
      "Vos comptes API",
      "Connectez des fournisseurs comme DeepSeek, OpenAI, Anthropic ou Google avec vos propres clés. Le modèle par défaut, deepseek-flash, utilise désormais DeepSeek-V4.1-Flash."
    ],
    [
      "Votre passerelle",
      "Utilisez un endpoint compatible avec OpenAI et choisissez les modèles qu’il propose."
    ],
    [
      "Votre inférence",
      "Exécutez des modèles locaux ou auto-hébergés avec Ollama, vLLM ou SGLang."
    ],
    [
      "Votre abonnement ChatGPT",
      "Connectez-vous avec codewhale auth chatgpt pour utiliser un abonnement ChatGPT à la place d’une clé API. Expérimental."
    ]
  ],
  modelsLink: "Parcourir les modèles et les fournisseurs",
  startHeading: "Premiers pas",
  startLede:
    "Installez Codewhale, connectez un modèle et ouvrez un dossier de projet. Ajoutez des plugins et d’autres agents selon vos besoins.",
  startGuideLink: "Suivre le guide de démarrage",
  startVocabularyLink: "Voir le vocabulaire du produit",
  chapterAvailability: "Où l’utiliser",
  availabilityHeading: "Ce qui est disponible et en développement",
  availabilityLede:
    "Le terminal et le client de navigateur local sont disponibles. L’application native de bureau et la nouvelle application web hébergée sont en développement.",
  availability: [
    [
      "Terminal et navigateur local",
      "Publié",
      "Installez-le sur Linux, macOS ou Windows, puis lancez codewhale, ou codewhale web pour le client de navigateur local. npm et Cargo fonctionnent aussi ; Android sous Termux est disponible en aperçu."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Disponible",
      "Un projet distinct, maintenu par la communauté : chat, fils et modifications de fichiers dans une barre latérale VS Code, sur le même Codewhale Runtime. Installez-la depuis le VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "Application web hébergée",
      "En développement",
      "En cours de reconstruction pour correspondre à l’application de bureau, et pas encore ouverte. Pour l’utiliser dans un navigateur dès aujourd’hui, lancez codewhale web sur votre machine."
    ],
    [
      "Bureau",
      "Build de développement",
      "Une application native sur le même moteur. Examinez et commitez les modifications, suivez le travail entre les projets et modifiez un message pour le renvoyer dans une nouvelle branche. Aucun téléchargement public n’est encore disponible."
    ]
  ],
  availabilityNote:
    "Le terminal, le navigateur local et la GUI ne nécessitent aucun compte Codewhale. Si vous utilisez votre propre clé fournisseur, celui-ci vous facture cet usage.",
  surfacesHeading: "Travaillez avec vos fichiers et vos outils",
  surfaces: [
    [
      "Fichiers et terminal",
      "Créez des fichiers, exécutez des commandes, analysez des données et testez vos réalisations. Vous définissez le dossier de travail et les permissions."
    ],
    [
      "Plugins et applications connectées",
      "Ajoutez des compétences et des outils via les plugins et MCP. Vérifiez et activez les connexions que l’agent peut utiliser."
    ],
    [
      "Navigateur et ordinateur · aperçu",
      "Utilisez les outils de navigation et le plugin Computer Use dans les applications et les sites, avec les accès que vous accordez."
    ],
    [
      "Des sessions à reprendre",
      "Conservez ensemble la conversation, les résultats des outils et l’historique. Reprenez la tâche dans le terminal ou son client de navigateur local."
    ],
    [
      "Équipes d’agents",
      "Répartissez un travail important entre des agents aux rôles et modèles différents avec Fleet, et suivez leur progression au même endroit."
    ]
  ],
  runtimeLink: "Découvrir les outils et intégrations",
  installBandHeading: "Installer sur macOS ou Linux",
  copy: "Copier",
  copied: "Copié ✓",
  binaries: "Binaires",
  chinaMirrors: "Miroirs en Chine",
  installGuideLink: "Lire le guide d’installation",
  communityHeading: "Contribuez à Codewhale",
  communityBody:
    "{count} personnes ont contribué à créer Codewhale. Signalez un bug, améliorez la documentation ou contribuez au code sur GitHub.",
  communityLinksAria: "Liens de la communauté",
  contribute: "Contribuer sur GitHub",
};
