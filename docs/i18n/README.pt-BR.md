<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>O modelo pode ser de qualquer pessoa. O agente deve ser seu.</h1>

Codewhale é um agente de programação de código aberto para o seu terminal, escrito em Rust. Ele funciona com qualquer modelo, pode dividir uma tarefa entre vários agentes, e você pode ler cada linha do código.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Isso instala a versão mais recente no macOS ou no Linux, e não exige conta. [Windows, npm e Cargo](#instalação) estão mais abaixo.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Site](https://codewhale.net) · [Documentação](../README.md) · [Registro de alterações](../../CHANGELOG.md) · [Contribuir](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Esta tradução é um rascunho automático que ainda não foi revisado por um falante nativo.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale em um terminal depois de uma tarefa de demonstração. Ele informa que adicionou uma opção --json ao export.py, que os 3 testes locais passaram e que uma revisão delegada não encontrou problemas. A lista de tarefas abaixo mostra três etapas concluídas, e a linha de status mostra Full Access e o provedor local website-demo.">

<sub>Uma build de desenvolvimento da 0.10.3, que ainda não foi lançada, com um modelo de teste roteirizado, em Full Access dentro de um workspace descartável. A edição, três testes e uma revisão por um subagente rodaram de verdade.</sub>

</div>

## Instalação

O comando acima coloca o `codewhale`, verificado por checksum, e seu apelido curto `codew` em `~/.local/bin`. Se depois o shell disser `command not found`, siga a nota sobre o PATH que o instalador imprime ([detalhes](../INSTALL.md#put-it-on-your-path)). `codewhale update` faz a atualização.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Há binários pré-compilados para Linux, macOS e Windows em x64 e arm64. Scoop, Nix, Homebrew no Linux, Android e Termux (prévia) e downloads manuais estão no [guia de instalação](../INSTALL.md), e contêineres em [Docker](../DOCKER.md). Use um único método de instalação por máquina; senão duas cópias ficam no seu `PATH` e vale a que aparecer primeiro.

## Início rápido

1. Faça commit ou stash do seu trabalho e rode `codewhale` na raiz de um repositório git. Por padrão, ele aplica edições de arquivos no repositório sem perguntar e pergunta antes de cada comando de shell que não seja somente leitura.
2. Rode `/provider` (ou pressione `F3`), escolha um provedor, cole a chave de API e escolha um modelo. DeepSeek é o provedor padrão. Na primeira execução sem chave, o Codewhale pede um provedor quando você envia a primeira mensagem, e oferece um modelo local quando um servidor Ollama local responde.
3. Digite uma tarefa:

```text
Fix the failing tests and explain what changed.
```

Comandos somente leitura rodam na hora. Para qualquer outro comando de shell, o Codewhale pergunta antes: `y` roda uma vez, `a` permite comandos parecidos pelo resto da conversa, e `n` recusa. `/undo` desfaz seu último pedido e as alterações de arquivos dele, e `/help` lista comandos e teclas.

Para scripts e CI, `codewhale exec` roda uma tarefa sem a interface. Leia [execuções sem interface](../INSTALL.md#headless-scripts-ci) antes de dar ferramentas a ele com `--auto`, porque `--auto` usa Full Access a menos que você adicione um sandbox.

## O que ele faz

Dê uma tarefa ao Codewhale e ele lê o repositório, edita arquivos e roda comandos até a tarefa terminar ou precisar da sua resposta. As [notas da versão mais recente](https://github.com/codewhale-hq/Codewhale/releases/latest) listam as mudanças mais novas, e o [changelog](../../CHANGELOG.md) traz todas as versões.

### Qualquer modelo

Mais de 40 provedores vêm integrados, entre eles Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter e xAI. Qualquer endpoint compatível com a OpenAI funciona, assim como modelos locais via Ollama, vLLM ou SGLang. O modelo padrão é o `deepseek-flash` da DeepSeek, que agora serve o DeepSeek V4.1 Flash ([detalhes](../PROVIDERS.md#deepseek-v41-flash)).

`/model` troca de modelo entre turnos, e `/model auto` escolhe um para cada turno (`/router` define como). `/turn inspect` mostra qual modelo um turno usou, quanto custou e quanto tempo levou. Veja [Provedores](../PROVIDERS.md).

### Recibos e desfazer

`/receipts`, ou `codewhale receipts --last` depois de sair, lista os arquivos que uma sessão alterou, os comandos que rodou, as chamadas web e MCP, os agentes que iniciou, cada aprovação e quem a deu, e o que falhou. `/undo` desfaz seu último pedido e os arquivos que ele alterou em um passo. `/restore` volta todo o workspace para um snapshot anterior e, como isso sobrescreve edições posteriores, exige `/trust on` ou Full Access. Veja [Recibos](../RECEIPTS.md).

### Vários agentes em uma tarefa

`/goal` define um objetivo que continua entre turnos. O Codewhale pode dividir o trabalho entre [subagentes](../SUBAGENTS.md), rodar uma [Fleet](../FLEET.md) de agentes com papéis e modelos próprios, ou seguir um [fluxo de trabalho](../WORKFLOW_AUTHORING.md) salvo no repositório.

### Extensões

Conecte [servidores MCP](../MCP.md), adicione [skills](../SKILLS.md) e [plugins](../PLUGINS.md), e rode [hooks](../HOOKS.md) em eventos de sessão e de ferramentas. O modo código (experimental, ativado por padrão) permite que o modelo escreva um programa `execute_tools` que chama várias ferramentas MCP e de plugins; cada chamada continua seguindo suas regras de aprovação e mantém seu recibo. O plugin Computer Use incluído na v0.10.2 (experimental, só macOS) pode observar e operar outros apps, e fica desligado até você revisar o acesso dele e ativá-lo ([guia](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). Chegando na 0.10.3: o plugin passa a ser lançado [separadamente](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Formas de rodar

| Comando | O que faz |
| --- | --- |
| `codewhale` | O app interativo de terminal |
| `codewhale exec "…"` | Uma tarefa sem a interface, para scripts e CI ([execuções sem interface](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Um cliente web local em `127.0.0.1` ([guia](../WEB.md)) |
| `codewhale review --pr N` | Uma revisão consultiva de um pull request do GitHub; a [configuração do bot do GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) a roda com a sua própria chave de modelo |
| `codewhale app-server --http` | A [Runtime API](../RUNTIME_API.md), uma API HTTP local para threads, eventos, aprovações e as alterações de arquivos de cada turno |
| `codewhale serve --acp` | O Agent Client Protocol via stdio, para editores como o Zed |

O app de terminal, `exec`, `web` e a Runtime API compartilham um motor ([arquitetura](../ARCHITECTURE.md)). Uma [extensão para VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) mantida pela comunidade se conecta a ele pela barra lateral ([código-fonte](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Como ele se mantém seguro

Duas chaves dizem quanto o Codewhale pode fazer sozinho. Com a entrada vazia, `Tab` troca o modo: **Plan** lê e pesquisa mas não muda nada, **Work** edita arquivos e roda comandos, e **Operate** tem o mesmo acesso que Work e continua até o objetivo ser verificado ou precisar de você. `Shift+Tab` troca a permissão. **Ask**, o padrão, aplica edições de arquivos no seu repositório sem perguntar e pergunta antes de comandos de shell que não são somente leitura. **Auto-Review** deixa regras e um modelo revisor aprovarem chamadas rotineiras e recusa as de alto risco; mude para Ask para aprová-las você mesmo. **Full Access** nunca pergunta e desliga o sandbox do Codewhale, embora as regras do repositório e a política gerenciada continuem valendo. O [guia da v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md) tem os detalhes.

Chegando na 0.10.3: `Shift+Tab` vira um único seletor que passa por Ask, Accept Edits, Plan, Auto-Review e Full Access, `Tab` deixa de trocar o modo e Operate é aposentado. Ask passará a perguntar também antes de cada edição de arquivo, e o novo padrão, Accept Edits, aplicará edições dentro do repositório git e continuará perguntando antes de comandos. O [guia atual](../MODES.md) descreve isso.

Em Ask e Auto-Review, os comandos rodam em um sandbox do sistema operacional quando o sistema tem um: Seatbelt no macOS, e bubblewrap no Linux quando está instalado. O Windows ainda não tem sandbox do sistema, e Full Access desliga o sandbox. Dentro do sandbox, comandos de shell e sessões de terminal não conseguem ler armazenamentos de credenciais como `~/.ssh`, `~/.aws` e arquivos `.env`. `/preview-request` mostra uma prévia com dados ocultos da próxima requisição sem enviá-la.

Você não precisa de conta, e as chaves adicionadas com `/provider` ficam na sua máquina. Com a sua própria chave de API, você paga o provedor diretamente, e modelos locais não precisam de chave. O rodapé e `/cost` mostram quanto uma sessão custou quando o preço é conhecido, e um preço desconhecido aparece como desconhecido, nunca como grátis.

A telemetria vem ativada por padrão. Ela envia contagens agregadas (versão e plataforma, duração e resultado da sessão, quais provedores integrados você usou, contagens de recursos e de erros) com um ID de instalação aleatório que muda a cada 90 dias, e nunca conversas, código, prompts, nomes de modelos, nomes de arquivos ou repositórios, nem credenciais. Este comando a desativa:

```bash
codewhale config set telemetry false
```

Ao iniciar, o Codewhale também verifica no GitHub se há uma versão nova; defina `CODEWHALE_NO_UPDATE_CHECK=1` para pular isso. Mais em [ordem de autorização](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md) e [telemetria](../TELEMETRY.md).

## Para onde ele vai

O Codewhale está se tornando um único ambiente que abrange seu terminal, um aplicativo de desktop, o navegador e computadores na nuvem, tudo movido por um só motor. Cada parte abaixo diz em que ponto está hoje.

| Parte | Estado | Hoje |
| --- | --- | --- |
| Aplicativo de terminal | Lançado | Instale no macOS, Linux ou Windows e execute `codewhale` em qualquer repositório. |
| Cliente de navegador local | Lançado | Execute `codewhale web` para ler a conversa, enviar uma tarefa e responder a aprovações no navegador; ele vem com o terminal e não exige conta. |
| Vários agentes em uma tarefa | Lançado | Subagentes, Fleets e fluxos de trabalho permitem que vários agentes dividam uma tarefa, cada um com seu papel. |
| Aplicativo de desktop | Em desenvolvimento, sem download | Estamos construindo um aplicativo de desktop sobre o mesmo motor, e ele ainda não tem download público. |
| Computadores na nuvem | Somente por convite | Os computadores na nuvem são somente por convite, e estamos construindo esses computadores para que uma sessão continue trabalhando em uma máquina na nuvem. |
| Uma sessão por todos eles | Em construção | Estamos construindo uma sessão que acompanha você do terminal ao desktop, ao navegador e à nuvem, e continua sua durante todo o caminho. |

O [roteiro](../ROADMAP.md) diz o que cada versão cobre e onde você pode ajudar.

## Vindo de outro agente

O Codewhale lê o `AGENTS.md` por conta própria. Arquivos de instruções escritos para outras ferramentas, como `CLAUDE.md`, `.cursorrules` ou `.github/copilot-instructions.md`, só são carregados quando você ativa com `project_instruction_imports = ["claude"]` (ou `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) na sua configuração; veja [config.example.toml](../../config.example.toml). O Codewhale avisa quando encontra um que não carregou.

`/import-claude` prepara uma importação das suas configurações do Claude Code e dos servidores MCP para você revisar. `/plugin install` aceita plugins do Claude Code que contêm skills, comandos, perfis de agente e servidores MCP, e recusa plugins que precisam de hooks ou servidores LSP ([detalhes](../CLAUDE_PLUGIN_COMPAT.md)).

## Documentação

Estes guias acompanham o branch principal e podem descrever mudanças que ainda não estão na versão mais recente.

| Comece aqui | Referência |
| --- | --- |
| [Guia do usuário](../GUIDE.md) | [Configuração](../CONFIGURATION.md) |
| [Instalação](../INSTALL.md) | [Arquitetura](../ARCHITECTURE.md) |
| [Provedores e modelos locais](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modos e permissões](../MODES.md) | [Criação de plugins](../PLUGIN_AUTHORING.md) |
| [Atalhos de teclado](../KEYBINDINGS.md) | [Recibos](../RECEIPTS.md) |
| [Revisão de pull requests no GitHub](../GITHUB_ACTION.md) | [Toda a documentação](../README.md) |

## Contribuir

O Codewhale é construído abertamente por quem o usa, em muitos idiomas.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="O mural de contribuidores do Codewhale: um avatar para cada pessoa cujo código, commits em coautoria, pull requests continuados ou relatos creditados estão no Codewhale, com o total no topo."></a>

O [mural de contribuidores](../CONTRIBUTORS.md) conta todas as pessoas cujo código, commits em coautoria, pull requests continuados ou relatos creditados estão no Codewhale, inclusive quem a própria lista de contribuidores do GitHub deixa de fora. `scripts/contributor-wall.py` gera de novo a imagem e a lista a partir do histórico do git e do GitHub.

Todo retorno é bem-vindo: um bug, uma ideia ou algo que parecia ou soava errado. [Abra uma issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) para qualquer um deles; uma issue também é o melhor lugar para começar um recurso novo. Pull requests são bem-vindos para correções, traduções, provedores e trabalho planejado. Perguntas vão para o [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). O [guia de contribuição](../../CONTRIBUTING.md) mostra para onde vai cada tipo de ajuda, como o seu trabalho entra com o seu nome, e [onde fica cada coisa](../../CONTRIBUTING.md#project-structure).

Converse no [Discord](https://discord.gg/37gfS3ksug), ou adicione o mantenedor, Hunter, no WeChat (`hunterbown`) e peça para entrar no grupo Whale Brothers. Todos são bem-vindos.

## História e licença

O Codewhale começou como `deepseek-tui` e ainda lê a configuração e as sessões daquele projeto. Ele é mantido de forma independente e não é afiliado à DeepSeek nem a nenhum outro provedor de modelos.

[MIT](../../LICENSE). O código adaptado de outros projetos de código aberto está listado nos avisos de terceiros para [fontes incluídas](../../THIRD_PARTY_NOTICES.md) e [designs adaptados](../THIRD_PARTY_NOTICES.md).
