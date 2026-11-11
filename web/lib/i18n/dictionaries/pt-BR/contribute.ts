import type { ContributeDict } from "../types";

/** Brazilian Portuguese dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Contribuir · Codewhale",
  metaDescription:
    "Qualquer feedback ajuda o Codewhale, com ou sem código. Veja para onde enviar um bug, uma ideia, uma tradução, um provedor ou uma correção.",
  kicker: "Vamos construir o Codewhale juntos",
  title: "Ajude a construir o Codewhale",
  lede: "Qualquer feedback ajuda, com ou sem código, e um bug, uma ideia ou uma tela que pareceu errada já bastam para começar.",
  fileIssue: "Abrir uma issue",
  browsePulls: "Ver as pull requests",
  fullGuide: "Guia do colaborador",
  pathsTitle: "Escolha por onde começar",
  workflowTitle: "Envie uma alteração que outras pessoas possam revisar",
  reviewTitle: "Mostre o que mudou e como você verificou",
  reviewScope:
    "Dê aos revisores o problema, o motivo da alteração, a evidência dos testes e o risco que sobra. Um escopo enxuto gera um feedback mais claro.",
  devTitle: "Compile e verifique sua alteração de código",
  devScope:
    "Para código Rust, compile a parte relevante e execute as verificações específicas dela. Siga o guia do colaborador para as verificações mais amplas que a sua alteração exigir.",
  mergedAccountsCount: {
    one: "{n} conta do GitHub teve um pull request integrado nos últimos 30 dias.",
    other: "{n} contas do GitHub tiveram pull requests integrados nos últimos 30 dias.",
  },
  mergedPeopleCount: {
    one: "{n} pessoa teve um pull request integrado nos últimos 30 dias.",
    other: "{n} pessoas tiveram pull requests integrados nos últimos 30 dias.",
  },
};
