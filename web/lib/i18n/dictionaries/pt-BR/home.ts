import type { HomeDict } from "../types";

/** Brazilian Portuguese home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale: agente de programação de código aberto",
  metaDescription: "Codewhale é um agente de programação de código aberto para seu terminal. Leia, edite e teste projetos com seus modelos hospedados ou locais.",
  heroTitle: "O agente de programação de código aberto para qualquer modelo",
  heroIntro: "{brand} lê seu projeto, edita arquivos e executa seus testes pelo terminal. Conecte um modelo hospedado ou local e escolha quais ações precisam da sua aprovação.",
  getCodewhale: "Instalar o Codewhale",
  heroInstallAria: "Comando de instalação",
  exploreProduct: "Explore o Codewhale",
  shotPreview: "Prévia do terminal",
  screenshotAlt:
    "Codewhale v{version}: captura do terminal com conversa, campo de mensagem e controles da sessão.",
  latestRelease: "Último lançamento {tag}",
  releaseUnavailable: "Status do lançamento indisponível",
  proofContributors: "{count} colaboradores",
  currentSource: "Código-fonte",
  sourceCandidate: "Não publicado",
  publishedRelease: "publicado",
  gainHeading: "Crie e automatize pelo seu terminal",
  gainLede:
    "Descreva o que você quer criar ou automatizar. Codewhale pode editar arquivos, executar comandos e verificar o resultado, com acesso controlado por você.",
  gain: [
    [
      "Crie aplicativos e ferramentas",
      "Crie um aplicativo, adicione uma funcionalidade ou escreva um script. Codewhale pode trabalhar com os arquivos do projeto, executar o código e testar o que cria."
    ],
    [
      "Automatize o trabalho repetitivo",
      "Execute fluxos de trabalho pelo terminal, scripts ou CI. Para tarefas maiores, delegue partes do trabalho a uma Fleet de agentes com modelos diferentes."
    ],
    [
      "Conecte as ferramentas que você usa",
      "Adicione ferramentas por meio de plugins e servidores MCP ou use APIs nos seus próprios scripts. Cada serviço precisa de configuração e autenticação próprias."
    ]
  ],
  chapterModels: "Seus modelos",
  modelsHeading: "Use os modelos que você escolher",
  modelsBody:
    "Conecte suas contas de provedores, um endpoint compatível com OpenAI ou modelos locais e hospedados por você. Escolha um modelo para a sessão e para cada agente de uma Fleet.",
  modelsFacts: [
    [
      "Suas contas de API",
      "Conecte provedores como DeepSeek, OpenAI, Anthropic ou Google com suas próprias chaves. O modelo padrão, deepseek-flash, agora executa DeepSeek-V4.1-Flash."
    ],
    [
      "Seu gateway",
      "Use um endpoint compatível com OpenAI e escolha os modelos que ele oferece."
    ],
    [
      "Sua inferência",
      "Execute modelos locais ou auto-hospedados com Ollama, vLLM ou SGLang."
    ],
    [
      "Seu plano do ChatGPT",
      "Entre com codewhale auth chatgpt para usar um plano do ChatGPT no lugar de uma chave de API. Experimental."
    ]
  ],
  modelsLink: "Ver modelos e provedores",
  startHeading: "Comece a usar",
  startLede:
    "Instale Codewhale, conecte um modelo e abra uma pasta de projeto. Você pode adicionar plugins e mais agentes conforme precisar.",
  startGuideLink: "Seguir o guia de primeiros passos",
  startVocabularyLink: "Ver o vocabulário do produto",
  chapterAvailability: "Onde funciona",
  availabilityHeading: "Disponível agora e em desenvolvimento",
  availabilityLede:
    "O terminal e o cliente de navegador local já estão disponíveis. O aplicativo desktop nativo e o novo aplicativo web hospedado estão em desenvolvimento.",
  availability: [
    [
      "Terminal e navegador local",
      "Lançado",
      "Instale no Linux, macOS ou Windows e depois execute codewhale, ou codewhale web para o cliente de navegador local. npm e Cargo também funcionam; Android no Termux é uma prévia."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Disponível",
      "Um projeto separado, mantido pela comunidade: chat, threads e alterações de arquivos em uma barra lateral do VS Code sobre o mesmo Codewhale Runtime. Instale pelo VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "Aplicativo web hospedado",
      "Em desenvolvimento",
      "Está sendo reconstruído para acompanhar o aplicativo desktop e ainda não está aberto. Para usá-lo no navegador hoje, execute codewhale web na sua máquina."
    ],
    [
      "Desktop",
      "Build de desenvolvimento",
      "Um aplicativo nativo com o mesmo mecanismo. Revise e faça commit de alterações, acompanhe o trabalho entre projetos e edite uma mensagem para reenviá-la como uma nova ramificação. Ainda não há download público."
    ]
  ],
  availabilityNote:
    "O terminal, o navegador local e a GUI não precisam de uma conta do Codewhale. Se você usar sua própria chave de provedor, esse provedor cobra pelo uso.",
  surfacesHeading: "Trabalhe com arquivos e ferramentas",
  surfaces: [
    [
      "Arquivos e terminal",
      "Crie arquivos, execute comandos, examine dados e teste o que constrói. Você define a pasta de trabalho e as permissões."
    ],
    [
      "Plugins e aplicativos conectados",
      "Adicione habilidades e ferramentas por plugins e MCP. Revise e habilite as conexões que deseja que o agente use."
    ],
    [
      "Navegador e computador · prévia",
      "Use ferramentas de navegador e o plugin Computer Use em aplicativos e sites, com o acesso que conceder."
    ],
    [
      "Sessões que você pode retomar",
      "Mantenha conversa, resultados das ferramentas e histórico juntos. Retome a tarefa no terminal ou no cliente de navegador local."
    ],
    [
      "Equipes de agentes",
      "Use Fleet para dividir um trabalho maior entre agentes com funções e modelos diferentes e acompanhar seu progresso em um só lugar."
    ]
  ],
  runtimeLink: "Explore ferramentas e integrações",
  installBandHeading: "Instale no macOS ou Linux",
  copy: "Copiar",
  copied: "Copiado ✓",
  binaries: "Binários",
  chinaMirrors: "Espelhos da China",
  installGuideLink: "Ler o guia de instalação",
  communityHeading: "Contribua com Codewhale",
  communityBody:
    "{count} pessoas ajudaram a construir o Codewhale. Relate um bug, melhore a documentação ou contribua com código no GitHub.",
  communityLinksAria: "Links da comunidade",
  contribute: "Contribua no GitHub",
};
