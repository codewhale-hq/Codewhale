<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Um agente de programação de código aberto para o seu terminal que funciona com qualquer modelo.**

Dê uma tarefa ao Codewhale e ele lê o repositório, edita arquivos e executa
comandos até concluir a tarefa ou precisar de uma resposta sua. Ele funciona com
mais de 40 provedores integrados, com DeepSeek V4.1 Flash como padrão, e com modelos
rodando na sua própria máquina. `/receipts` lista os arquivos que ele alterou,
os comandos que executou e as chamadas web e MCP que fez.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Site](https://codewhale.net) · [Documentação](../README.md) · [Changelog](../../CHANGELOG.md) · [Contribuindo](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Uma sessão de terminal do Codewhale" width="760">

<sub>Um build de desenvolvimento da 0.10.2 com um modelo de teste roteirizado, em Acesso total dentro de um workspace descartável. A edição, três testes e uma revisão de sub-agente rodaram de verdade.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — 30% a mais de uso do DeepSeek v4.1 Flash" width="760">
  </a>
</p>

[Ver a oferta Codewhale × OrcaRouter](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). A Codewhale recebe uma comissão por indicação da OrcaRouter.

## Instalação

macOS e Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

O instalador coloca `codewhale` e o alias curto `codew`, com checksum
verificado, em `~/.local/bin`. Se depois o shell responder `command not found`,
execute a linha de PATH que o instalador exibe
([detalhes](../INSTALL.md#put-it-on-your-path)). `codewhale update` faz a
atualização. Esses comandos instalam a versão mais recente. Esta página
acompanha a branch `main`, então um recurso marcado com uma versão chega nessa
versão; o [changelog](../../CHANGELOG.md) lista o que cada versão traz.

<details>
<summary><b>Windows, npm, Cargo e outras formas de instalar</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Há binários pré-compilados para Linux, macOS e Windows em x64 e arm64. Nix,
Homebrew no Linux, Android/Termux (prévia) e downloads manuais estão no
[guia de instalação](../INSTALL.md), e contêineres em
[Docker](../DOCKER.md). Use um único método de instalação por máquina; caso
contrário, duas cópias acabam no `PATH` e vale a que vier primeiro.

</details>

## Início rápido

1. Faça commit ou stash do seu trabalho e execute `codewhale` na raiz de um
   repositório git. Por padrão, ele aplica as edições de arquivos ali sem
   perguntar e mostra o diff.
2. Execute `/provider` (ou pressione `F3`), escolha um provedor, cole a chave
   de API dele e escolha um modelo. DeepSeek é o provedor padrão. Sem uma
   chave, escolha um runtime local. Se nenhum provedor estiver configurado e o
   Ollama já estiver servindo um modelo de chat, o Codewhale passa a usá-lo
   automaticamente; a partir da 0.10.2, ele o oferece quando você envia sua
   primeira mensagem.
3. Digite uma tarefa:

```text
Fix the failing tests and explain what changed.
```

Comandos somente leitura rodam na hora. Para qualquer outro comando de shell, o
Codewhale pergunta antes: `y` executa uma vez, `a` permite comandos parecidos
pelo resto da conversa, `n` recusa. `/undo` desfaz seu último pedido e as
alterações de arquivos que ele fez.
`/help` lista os comandos e as teclas.

Para scripts e CI, `codewhale exec` executa uma tarefa sem a interface. Por
padrão, `--auto` usa Acesso total: as ferramentas rodam sem perguntar, os
comandos de shell não têm o sandbox do Codewhale e o modo de confiança permite
que as ferramentas de arquivos escrevam fora do workspace. Adicione
`--sandbox workspace-write` para manter o sandbox do shell e as restrições das
ferramentas de arquivos ao workspace, com o modo de confiança desligado:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Modos e permissões

Uma instalação nova começa no modo Work com a permissão Perguntar.

| | Alterne com | Opções |
| --- | --- | --- |
| Modo | `Tab` (com o campo de entrada vazio) ou `/mode` | **Plan** lê e pesquisa, mas não altera nada. **Work** edita arquivos e executa comandos. **Operate** tem o mesmo acesso que o Work, trata seu pedido como um objetivo e continua até o objetivo ser verificado ou precisar de uma resposta sua. |
| Permissão | `Shift+Tab` | **Perguntar** (Ask) aplica edições de arquivos no seu repositório sem perguntar e pergunta antes de comandos de shell que não sejam somente leitura. **Revisão automática** (Auto-Review) deixa regras e um modelo revisor aprovarem chamadas rotineiras e recusa as de alto risco; mude para Perguntar para aprová-las você mesmo. **Acesso total** (Full Access) nunca pergunta e desliga o sandbox do Codewhale; as regras do repositório e a política gerenciada continuam valendo. |

Os detalhes estão no [guia de modos e permissões](../MODES.md).

## O que ele faz

### Novidades na 0.10.2

- `/pet on` transforma o seu terminal em uma baleia animada que mostra o que o
  agente está fazendo, enquanto a caixa de mensagem continua ativa. Suas 980
  partículas escrevem a etapa atual em inglês, chinês ou japonês.
- `/workbar terminal` mostra os shells que o agente está executando enquanto
  eles rodam. Digite em um deles ou pressione `Ctrl+N` para abrir um shell só
  seu (macOS e Linux).
- `/undo` desfaz todo o seu último pedido de uma vez, incluindo cada arquivo que
  ele alterou, e `/diff` funciona em pastas sem Git.
- Quando um plano está pronto, o Codewhale pergunta se você quer começar o
  Work com Perguntar ou Revisão automática, ou continuar planejando.
- `/color` dá a cada chat sua própria cor na caixa de mensagem, salva com a
  sessão.
- `web_search` pode usar a nova Search API da DeepSeek: defina
  `[search] provider = "deepseek"` com uma chave da DeepSeek.
- Fora do Acesso total, um `curl` ou `wget` que enviaria uma credencial nunca
  roda sem a sua aprovação, e um comando que esconde Unicode invisível é
  recusado. `write` não substitui mais um arquivo que o agente não leu, nem um
  que mudou depois que ele o leu.

### Modelos

Mais de 40 provedores vêm integrados, incluindo Anthropic, OpenAI, DeepSeek,
Google, Mistral, Moonshot, OpenRouter e xAI. Qualquer endpoint compatível com
OpenAI funciona, assim como modelos locais via Ollama, vLLM ou SGLang. O padrão
é o `deepseek-flash` da DeepSeek, que a DeepSeek passou para o
DeepSeek-V4.1-Flash-1017 em 2026-10-17; a 0.10.1 o executa sem precisar de
atualização ([detalhes](../PROVIDERS.md#deepseek-v41-flash)).

`/model` troca de modelo entre turnos, e `/model auto` escolhe um para cada
turno (`/router` configura como). `/turn inspect` mostra qual modelo um turno
usou, quanto custou e quanto tempo levou. `codewhale auth chatgpt` permite usar
um plano do ChatGPT em vez de uma chave de API (experimental). Veja
[Provedores](../PROVIDERS.md).

### Recibos e desfazer

`/receipts`, ou `codewhale receipts --last` depois que você sai, lista os
arquivos alterados, os comandos executados, as chamadas web e MCP, os agentes
iniciados, as aprovações e quem as deu, e as falhas. `/undo` desfaz seu último pedido e os arquivos que ele alterou, em uma única etapa e sem o modo de confiança. `/restore` volta o workspace inteiro para um snapshot
anterior; como isso sobrescreve edições posteriores, exige `/trust on` ou Acesso
total. Veja [Recibos](../RECEIPTS.md).

As execuções de exec que oferecem ferramentas são salvas em todos os formatos
de saída, então `codewhale receipts --last` funciona depois de `exec --auto`,
inclusive com `--json`. As execuções que só dão uma resposta em texto ou com
`--json` são salvas apenas ao retomar uma conversa ou quando a compactação da
conversa começa; use `--output-format stream-json` para sempre salvá-las para
os recibos ou para `exec --continue`.

### Trabalhos mais longos

`/goal` define um objetivo que se mantém entre turnos. O Codewhale pode dividir
o trabalho entre [sub-agentes](../SUBAGENTS.md), executar uma
[frota](../FLEET.md) de agentes ou seguir um
[workflow](../WORKFLOW_AUTHORING.md) versionado no repositório.

### Vindo de outro agente

O Codewhale lê o `AGENTS.md` por conta própria. Arquivos de instruções escritos
para outras ferramentas, como `CLAUDE.md`, `.cursorrules` ou
`.github/copilot-instructions.md`, só são carregados quando você opta por eles
com `project_instruction_imports = ["claude"]` (ou `"cursor"`, `"cline"`,
`"windsurf"`, `"copilot"`, `"all"`) na sua configuração; veja
[config.example.toml](../../config.example.toml). O Codewhale avisa quando encontra
um que não carregou.

`/import-claude` prepara uma importação das suas configurações e servidores MCP
do Claude Code para você revisar. `/plugin install` aceita plugins do Claude
Code que contêm skills, comandos, perfis de agente e servidores MCP, e recusa
plugins que precisam de hooks ou servidores LSP
([detalhes](../CLAUDE_PLUGIN_COMPAT.md)).

### Extensões

Conecte [servidores MCP](../MCP.md), adicione [skills](../SKILLS.md) e
[plugins](../PLUGINS.md) e execute [hooks](../HOOKS.md) em eventos de
sessão e de ferramentas. O Code mode (experimental, ativado por padrão) permite
que o modelo escreva um único programa `execute_tools` que chama várias
ferramentas de MCP e de plugins; cada chamada continua sujeita às suas regras de
aprovação e mantém o seu recibo. O plugin Computer Use instalado separadamente (experimental,
só no macOS) pode observar e operar outros aplicativos. Ele fica desligado até você
revisar o acesso dele e ativá-lo
([guia](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Formas de executar

| Comando | O que faz |
| --- | --- |
| `codewhale` | O aplicativo interativo de terminal |
| `codewhale exec "…"` | Uma tarefa sem a interface. Sozinho, ele responde sem usar ferramentas; `--auto` habilita ferramentas com Acesso total por padrão, `--sandbox workspace-write` as restringe, e `--output-format stream-json` transmite os eventos como JSON |
| `codewhale web` | Uma interface web local em `127.0.0.1` ([guia](../WEB.md)) |
| `codewhale review --pr N` | Uma revisão consultiva de um pull request do GitHub. A [configuração do bot do GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) gera um workflow com versão fixada para revisões automáticas e `@codewhale review`, usando sua própria chave de modelo e uma identidade de App opcional |
| `codewhale app-server --http` | A [Runtime API](../RUNTIME_API.md), uma API HTTP local para threads, eventos, aprovações e as alterações de arquivos de cada turno |
| `codewhale serve --acp` | O Agent Client Protocol via stdio, para editores como o Zed |

O aplicativo de terminal, o `exec`, o `web` e a Runtime API compartilham um
único engine ([arquitetura](../ARCHITECTURE.md)). Uma
[extensão do VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
mantida pela comunidade se conecta a ele pela barra lateral
([código-fonte](https://github.com/HengQuWorld/CodeWhale-VSCode)).
Um aplicativo desktop nativo baseado na Runtime API está em desenvolvimento;
ainda não há download público.

## Custo e contas

O Codewhale é gratuito e tem licença MIT. Com sua própria chave de API, você
paga diretamente ao provedor; modelos locais não precisam de chave. O rodapé e
`/cost` mostram quanto uma sessão custou quando o preço é conhecido, e um preço
desconhecido aparece como desconhecido, nunca como gratuito.

Você não precisa de uma conta. As chaves que você adiciona com `/provider`
ficam na sua máquina.

## Segurança e privacidade

Em Perguntar e Revisão automática, os comandos rodam em um sandbox do sistema
operacional quando o sistema tem um: Seatbelt no macOS e bubblewrap no Linux,
quando está instalado. O Windows ainda não tem sandbox do sistema operacional,
e o Acesso total desliga o sandbox. Dentro do sandbox, comandos de shell não
conseguem ler armazenamentos de credenciais como `~/.ssh`, `~/.aws` e arquivos
`.env`; a partir da 0.10.2, a mesma regra vale para sessões de terminal e mais
ferramentas de arquivos. `/preview-request` mostra uma prévia da próxima
requisição, com dados sensíveis ocultados, sem enviá-la.

A telemetria vem ativada por padrão. Ela envia contagens agregadas (versão e
plataforma, duração e resultado da sessão, quais provedores integrados você
usou, contagens de recursos e de erros) com um ID de instalação aleatório que
muda a cada 90 dias, e nunca envia conversas, código, prompts, nomes de modelos,
nomes de arquivos ou de repositórios, nem credenciais. `codewhale config set telemetry false` a desativa. Na
inicialização, o Codewhale também consulta o GitHub em busca de uma nova
versão; defina `CODEWHALE_NO_UPDATE_CHECK=1` para pular essa verificação. Mais
em [ordem de autorização](../AUTHORIZATION_ORDER.md),
[sandboxing](../SANDBOX.md) e [telemetria](../TELEMETRY.md).

## Documentação

| Comece aqui | Referência |
| --- | --- |
| [Instalação](../INSTALL.md) | [Configuração](../CONFIGURATION.md) |
| [Provedores e modelos locais](../PROVIDERS.md) | [Arquitetura](../ARCHITECTURE.md) |
| [Modos e permissões](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [Atalhos de teclado](../KEYBINDINGS.md) | [Criação de plugins](../PLUGIN_AUTHORING.md) |
| [Revisão de PR no GitHub](../GITHUB_ACTION.md) | [Toda a documentação](../README.md) |

## Comunidade

Se faltar um provedor ou algum fluxo de trabalho for incômodo,
[abra uma issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) ou
[envie um pull request](../../CONTRIBUTING.md). Quem contribui mantém o crédito pelo
trabalho incorporado, e a
[estrutura do repositório](../../CONTRIBUTING.md#project-structure) mostra onde fica
cada coisa.

Converse no [Discord](https://discord.gg/37gfS3ksug), ou adicione o mantenedor,
Hunter, no WeChat (`hunterbown`) e peça para entrar no grupo Whale Brothers.

## Histórico e licença

O Codewhale começou como `deepseek-tui` e ainda lê a configuração e as sessões
desse projeto. Ele é mantido de forma independente e não é afiliado à DeepSeek
nem a nenhum outro provedor de modelos. Agradecemos a
[todos que contribuíram](../CONTRIBUTORS.md).

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). O código adaptado de outros projetos de código aberto está
listado nos [avisos de terceiros](../THIRD_PARTY_NOTICES.md).
