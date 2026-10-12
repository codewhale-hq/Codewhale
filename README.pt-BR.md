<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**O agente de código aberto que funciona com qualquer modelo.**

O Codewhale lê seu projeto, edita arquivos, executa comandos e verifica o próprio
trabalho — no seu terminal, com um modelo hospedado ou local à sua escolha.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Site](https://codewhale.net) · [Documentação](docs/README.md) · [Changelog](CHANGELOG.md) · [Contribuindo](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Uma sessão de terminal do Codewhale" width="760">

<sub>Captura real do terminal Codewhale em uma sessão de demonstração local configurada.</sub>

</div>

## Instalação

macOS e Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

O instalador baixa binários com checksum verificado para `~/.local/bin`. Se
depois `codewhale` responder "command not found", execute a única linha de PATH
que o instalador exibe, ou veja [Colocar no PATH](docs/INSTALL.md#put-it-on-your-path).
Atualize quando quiser com `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo e outras formas</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew no Linux, Android/Termux, downloads manuais com
verificação de checksum e o espelho opcional do CNB estão descritos no
[guia de instalação](docs/INSTALL.md). Escolha uma única forma: várias
instalações na mesma máquina acabam disputando o `PATH`.

</details>

## Início rápido

**As chaves API dos seus provedores, em um só lugar.** [Crie uma conta Codewhale](https://app.codewhale.net/register)
ou execute `codewhale login` para entrar. Salve e atualize suas chaves API na conta
e use-as pela rota de modelos do Codewhale nos dispositivos em que você estiver conectado.
[Configure as chaves da conta](docs/CONFIGURATION.md#account-provider-keys).
Entrar na conta não envia as chaves locais existentes; o uso local continua disponível sem uma conta.

1. **Abra seu projeto.** Execute `codewhale` na pasta em que quer trabalhar.
2. **Conecte um modelo.** Execute `/provider` (ou pressione `F3`) para adicionar
   uma chave hospedada ou escolher um ambiente local. Se o Ollama já estiver em
   execução com um modelo de chat, o Codewhale passa a usá-lo sozinho. Use
   `/model` para trocar de modelo.
3. **Dê a ele uma tarefa concreta.**

```text
Fix the failing tests and explain what changed.
```

A mesma tarefa pode rodar sem interface, a partir de um script ou job de CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Execute `/help` para ver os comandos e atalhos de teclado.

## Formas de executar

Todos os clientes usam o mesmo Runtime local do Codewhale, então sessões,
ferramentas e permissões se comportam da mesma forma em qualquer lugar.

| Comando | O que faz |
| --- | --- |
| `codewhale` | A interface interativa de terminal |
| `codewhale exec "…"` | Um turno sem interface, a partir de um script ou CI; `--auto` permite as ferramentas de arquivos e de shell, `--output-format stream-json` transmite JSON em streaming |
| `codewhale web` | O [cliente de navegador local](docs/WEB.md) incluído, em `127.0.0.1` |
| `codewhale review --pr N` | Uma [revisão de pull request](docs/GITHUB_ACTION.md) apenas consultiva; publicá-la é opcional |
| Runtime API | Uma [API HTTP local](docs/RUNTIME_API.md) para threads, eventos e aprovações |

Um aplicativo desktop nativo (GPUI) está sendo desenvolvido como cliente do
produto com login; veja a
[página do produto](https://codewhale.net/en/product) para saber a disponibilidade. A
[extensão do VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode),
mantida pela comunidade, conecta-se ao mesmo Runtime a partir de uma barra
lateral ([código-fonte](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## O que ele faz

- **Qualquer modelo, sem dependência de fornecedor.** Mais de 40 rotas de
  provedores integradas — Anthropic, DeepSeek, Google, Mistral, Moonshot,
  OpenAI, OpenRouter, xAI e outros — além de qualquer endpoint compatível com
  OpenAI e modelos locais via Ollama, vLLM ou SGLang. [Provedores](docs/PROVIDERS.md)
- **Você mantém o controle.** O modo Plan explora sem alterar nada; Work e
  Operate fazem alterações. As posturas de aprovação definem quando uma chamada
  de ferramenta precisa do seu OK, `/undo` e `/restore` recuperam alterações do
  workspace, e `/receipts` lista cada arquivo, comando e aprovação de uma
  sessão. [Modos](docs/MODES.md) · [Recibos](docs/RECEIPTS.md)
- **Feito para trabalhos longos.** Defina um `/goal` durável, delegue trabalho
  delimitado a [sub-agentes](docs/SUBAGENTS.md), execute [equipes de agentes](docs/FLEET.md)
  supervisionadas com uma checagem prévia de gastos, ou automatize-as como
  [workflows](docs/WORKFLOW_AUTHORING.md) versionados no repositório.
- **Amplie o que você já usa.** Conecte [servidores MCP](docs/MCP.md), instale
  [skills](docs/SKILLS.md) e [plugins](docs/PLUGINS.md), execute
  [hooks](docs/HOOKS.md) em eventos de sessão e de ferramentas, e carregue os
  [plugins do Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) existentes.
- **Computer Use.** Um plugin incluído adiciona ferramentas para observar e
  operar outros aplicativos. Revise o acesso que ele solicita e ative-o antes de usar.
  [Guia](crates/tui/plugins/computer-use/README.md)

## Modos e permissões

| | Escolha com | Opções |
| --- | --- | --- |
| **Modo** — o que o agente está fazendo | `Tab` ou `/mode` | Plan (explorar, sem alterações) · Work (editar e executar) · Operate (conduzir um objetivo com etapas planejadas e verificadas) |
| **Postura** — quando ele pergunta antes | `Shift+Tab` | Ask · Auto-Review · Full Access |

O Full Access continua respeitando os limites rígidos de política. O
[guia de modos e permissões](docs/MODES.md) explica cada opção.

## Segurança

O Codewhale roda na sua máquina com o acesso que você conceder. As posturas de
aprovação e as regras do repositório limitam o que o agente pode fazer, e os
comandos rodam dentro de um sandbox do sistema operacional onde há suporte
(Seatbelt no macOS; bubblewrap no Linux quando instalado e funcionando).
`/preview-request` mostra a requisição exata, com dados sensíveis ocultados,
antes de qualquer envio. Preços de modelos desconhecidos continuam
desconhecidos, em vez de serem apresentados como gratuitos.

Veja a [ordem de autorização](docs/AUTHORIZATION_ORDER.md), o
[sandboxing](docs/SANDBOX.md) e a [telemetria](docs/TELEMETRY.md): as
contagens de uso ficam ativadas por padrão e `codewhale config set telemetry false`
as desativa.

## Documentação

| Comece aqui | Aprofunde-se |
| --- | --- |
| [Instalação](docs/INSTALL.md) | [Configuração](docs/CONFIGURATION.md) |
| [Provedores e modelos locais](docs/PROVIDERS.md) | [Arquitetura](docs/ARCHITECTURE.md) |
| [Modos e permissões](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Atalhos de teclado](docs/KEYBINDINGS.md) | [Criação de plugins](docs/PLUGIN_AUTHORING.md) |
| [Revisão de PR no GitHub](docs/GITHUB_ACTION.md) | [Toda a documentação](docs/README.md) |

## Comunidade

Relatos de bugs, ideias de recursos e pull requests são bem-vindos, quer você
use o Codewhale há meses ou esteja testando pela primeira vez. Se faltar um
provedor ou algum fluxo de trabalho for incômodo,
[abra uma issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) ou
[envie um pull request](CONTRIBUTING.md). As primeiras contribuições são
bem-vindas, e quem contribui mantém o crédito pelo trabalho incorporado. A
[estrutura do repositório](CONTRIBUTING.md#project-structure) é um bom ponto de
partida.

Entre no [Discord](https://discord.gg/37gfS3ksug), ou adicione o Hunter no WeChat
(`hunterbown`) e peça para entrar no grupo Whale Brothers.

## Histórico e licença

O Codewhale começou como `deepseek-tui` e ainda lê a configuração e as sessões
desse projeto. Agora ele é neutro quanto a provedores, mantido de forma
independente e não é afiliado a nenhum provedor de modelos. Agradecemos a
[todos os colaboradores](docs/CONTRIBUTORS.md) e às comunidades de código aberto
que ajudaram o projeto a crescer.

[MIT](LICENSE). Trechos adaptados de outros projetos de código aberto estão
registrados nos [avisos de terceiros](docs/THIRD_PARTY_NOTICES.md).
