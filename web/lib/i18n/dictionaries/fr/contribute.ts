import type { ContributeDict } from "../types";

/** French dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Contribuer · Codewhale",
  metaDescription:
    "Tout retour aide Codewhale, avec ou sans code. Trouvez où envoyer un bug, une idée, une traduction, un fournisseur ou un correctif.",
  kicker: "Construisons Codewhale ensemble",
  title: "Aidez à construire Codewhale",
  lede: "Tout retour aide, avec ou sans code, et un bug, une idée ou un écran qui semblait faux suffit pour commencer.",
  fileIssue: "Ouvrir un ticket",
  browsePulls: "Parcourir les pull requests",
  fullGuide: "Guide du contributeur",
  pathsTitle: "Choisissez par où commencer",
  workflowTitle: "Envoyez un changement que d’autres peuvent relire",
  reviewTitle: "Montrez ce qui a changé et comment vous l’avez vérifié",
  reviewScope:
    "Donnez aux relecteurs le problème, la raison du changement, les preuves de test et le risque restant. Un périmètre resserré attire des retours plus clairs.",
  devTitle: "Compilez et vérifiez votre changement de code",
  devScope:
    "Pour du code Rust, compilez la partie concernée et lancez ses vérifications ciblées. Suivez le guide du contributeur pour les vérifications plus larges dont votre changement a besoin.",
  mergedAccountsCount: {
    one: "{n} compte GitHub a eu un pull request fusionné au cours des 30 derniers jours.",
    other: "{n} comptes GitHub ont eu des pull requests fusionnés au cours des 30 derniers jours.",
  },
  mergedPeopleCount: {
    one: "{n} personne a eu un pull request fusionné au cours des 30 derniers jours.",
    other: "{n} personnes ont eu des pull requests fusionnés au cours des 30 derniers jours.",
  },
};
