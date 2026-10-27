import type { ComputerUseDict } from "../types";

/**
 * Brazilian Portuguese dictionary for `app/[locale]/computer-use/page.tsx`
 * and the Computer Use section of the install page. Product names, the menu
 * items Pause, Stop and Check for updates, and the macOS setting names stay
 * as the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Prévia · Codewhale",
  metaDescription: "Baixe e configure o Codewhale Computer Use para Mac. Controle de apps em segundo plano, configuração de permissões e controles humanos de Pause e Stop.",
  title: "Computer Use",
  lead: "Deixe o Codewhale trabalhar nos seus apps enquanto você continua trabalhando. Autorize uma vez neste Mac e interrompa o uso do computador quando quiser.",
  publisher: "Por Codewhale",
  download: "Baixar para Mac",
  downloadZip: "Arquivo ZIP (usado pelo atualizador interno do app)",
  requirements: "macOS 13.5 ou posterior · Apple silicon e Intel",
  included: "Um único download do app. Não é preciso instalar o Node nem um compilador à parte.",
  pendingTitle: "Download para Mac em preparação",
  pendingBody: "O instalador público aparecerá aqui assim que a notarização da Apple e as verificações de lançamento forem concluídas.",
  unavailableTitle: "Não foi possível verificar a disponibilidade do download",
  unavailableBody: "Atualize esta página para tentar de novo ou confira os lançamentos publicados abaixo.",
  releases: "Lançamentos publicados",
  receipt: "Detalhes de verificação do download",
  setup: "Configure seu Mac",
  steps: [
    { title: "Instale o app", body: "Abra a imagem de disco e arraste o Codewhale Computer Use para a pasta Aplicativos. Abra-o a partir de Aplicativos e escolha Computer Use no ícone da baleia na barra de menus." },
    { title: "Revise as permissões", body: "Use os botões de configuração para abrir Acessibilidade e Gravação de Tela nos Ajustes do Sistema. Você decide quais permissões conceder." },
    { title: "Ative o Computer Use", body: "Nas configurações de Connectors do Codewhale, instale ou atualize o Computer Use, depois revise e ative o plugin. Peça para ele trabalhar em um app. No primeiro uso, escolha Trust until revoked no assistente nativo; essa confiança continua válida entre apps, modelos e tarefas." },
  ],
  controlsTitle: "Mantenha o controle",
  controlsBody: "O controle compacto do Codewhale aparece ao lado do app em uso. Pause suspende o uso do computador; Stop computer use encerra todas as sessões atuais do computador. Escolha Turn off no assistente para remover o acesso do Codewhale. Peça uma prévia ao vivo quando quiser acompanhar o trabalho.",
  updateTitle: "Atualizações quando você quiser",
  updateBody: "Escolha Check for updates (buscar atualizações) no app. Antes de instalar uma atualização, ele verifica o download, a assinatura do Codewhale e a notarização da Apple, e mantém a versão anterior do app para recuperação.",
  help: "Configuração e solução de problemas",
  notes: "Notas de lançamento",
  demo: "Veja a verificação em segundo plano",
  source: "Código-fonte e outras plataformas",
  platforms: "Este download é para Mac. No Windows e no Linux, por enquanto, usa-se o plugin a partir do código-fonte com configuração no lado do host.",
};
