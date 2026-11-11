import type { ContributeDict } from "../types";

/** Catalan dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Col·laborar · Codewhale",
  metaDescription:
    "Tot el feedback ajuda Codewhale, amb codi o sense. Descobreix on ha d’anar un error, una idea, una traducció, un proveïdor o una correcció.",
  kicker: "Construïm Codewhale junts",
  title: "Ajuda a construir Codewhale",
  lede: "Tot el feedback ajuda, amb codi o sense, i un error, una idea o una pantalla que no es veia bé ja és un bon punt de partida.",
  fileIssue: "Obre un issue",
  browsePulls: "Consulta els pull requests",
  fullGuide: "Guia per a col·laboradors",
  pathsTitle: "Tria per on començar",
  workflowTitle: "Envia un canvi que altres puguin revisar",
  reviewTitle: "Mostra què ha canviat i com ho has comprovat",
  reviewScope:
    "Explica a qui revisa el problema, el motiu del canvi, les proves i el risc que queda. Un abast reduït aporta comentaris més clars.",
  devTitle: "Compila i comprova el teu canvi de codi",
  devScope:
    "Si és codi Rust, compila la part afectada i executa-hi les comprovacions específiques. Per a altres comprovacions que el teu canvi necessiti, segueix la guia per a col·laboradors.",
  mergedAccountsCount: {
    one: "{n} compte de GitHub ha tingut un pull request fusionat en els darrers 30 dies.",
    other: "{n} comptes de GitHub han tingut pull requests fusionats en els darrers 30 dies.",
  },
  mergedPeopleCount: {
    one: "{n} persona ha tingut un pull request fusionat en els darrers 30 dies.",
    other: "{n} persones han tingut pull requests fusionats en els darrers 30 dies.",
  },
};
