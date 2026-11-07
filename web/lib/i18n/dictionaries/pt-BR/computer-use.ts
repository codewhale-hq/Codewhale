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
  lead: "Deixe seu agente trabalhar nos apps do Mac enquanto você continua usando o computador.",
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
    { title: "Conecte seu agente", body: "No Codewhale, instale o Computer Use com /plugin marketplace install codewhale computer-use, depois revise e ative o plugin. O assistente é instalado separadamente. Outros hosts MCP compatíveis se conectam a ele seguindo o guia de configuração." },
  ],
  controlsTitle: "Mantenha o controle",
  controlsBody: "Após conceder as permissões do macOS, o modo padrão funciona sem pedir autorização para cada app. Os apps que você bloquear continuam inacessíveis; Pause suspende o uso do computador e Stop computer use encerra as sessões atuais. A aprovação estrita é opcional. Peça uma prévia ao vivo quando quiser acompanhar o trabalho.",
  updateTitle: "Atualizações quando você quiser",
  updateBody: "Escolha Check for updates (buscar atualizações) no app. Antes de instalar uma atualização, ele verifica o download, a assinatura do Codewhale e a notarização da Apple, e mantém a versão anterior do app para recuperação.",
  help: "Configuração e solução de problemas",
  notes: "Notas de lançamento",
  demo: "Veja a verificação em segundo plano",
  source: "Código-fonte e outras plataformas",
  platforms: "O assistente para Mac está em beta e o plugin do catálogo oferece suporte apenas a Mac. As versões publicadas também incluem uma prévia experimental sem assinatura para Windows x64; o Linux exige uma configuração experimental pelo código-fonte ou com Docker. A entrada no Windows e no Linux usa a área de trabalho compartilhada.",
};
