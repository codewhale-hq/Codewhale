import type { ContributeDict } from "../types";

/** Italian dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Contribuisci · Codewhale",
  metaDescription:
    "Ogni feedback aiuta Codewhale, con o senza codice. Scopri dove vanno inviati un bug, un’idea, una traduzione, un fornitore o una correzione.",
  kicker: "Costruiamo Codewhale insieme",
  title: "Aiuta a costruire Codewhale",
  lede: "Ogni feedback aiuta, con o senza codice: un bug, un’idea o una schermata che sembrava sbagliata bastano per iniziare.",
  fileIssue: "Apri una issue",
  browsePulls: "Sfoglia le pull request",
  fullGuide: "Guida per i contributori",
  pathsTitle: "Scegli da dove iniziare",
  workflowTitle: "Invia una modifica che altri possano rivedere",
  reviewTitle: "Mostra cosa è cambiato e come l’hai verificato",
  reviewScope:
    "Fornisci a chi rivede il problema, il motivo della modifica, le prove dei test e il rischio residuo. Un ambito ristretto porta a un feedback più chiaro.",
  devTitle: "Compila e verifica la tua modifica al codice",
  devScope:
    "Per il codice Rust, compila la parte interessata ed esegui i controlli mirati. Segui la guida per i contributori per gli altri controlli che la tua modifica richiede.",
  mergedAccountsCount: {
    one: "{n} account GitHub ha avuto un pull request unito negli ultimi 30 giorni.",
    other: "{n} account GitHub hanno avuto pull request uniti negli ultimi 30 giorni.",
  },
  mergedPeopleCount: {
    one: "{n} persona ha avuto un pull request unito negli ultimi 30 giorni.",
    other: "{n} persone hanno avuto pull request uniti negli ultimi 30 giorni.",
  },
};
