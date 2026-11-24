<!-- source: README.md sha256:5da2c4c9d5f9 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agente de programação open source em Rust para o seu terminal. Qualquer modelo, frotas multiagente, sandbox no sistema operacional e recibos verificáveis.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Site](https://codewhale.net) · [Documentação](../README.md) · [Registro de alterações](../../CHANGELOG.md) · [Contribuir](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · Português · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 em um terminal após uma tarefa real com Ollama local (qwen3:4b): editou ls.py, aguardou aprovação antes de executar python3 -m unittest -v test_ls.py, executou 2 testes com sucesso e relatou o resultado.">

<sub>Sessão real na versão publicada Codewhale v0.10.4 (48669ad108fa) executando Ollama local (qwen3:4b) no modo Accept Edits.</sub>

</div>

## Instalação

O script de instalação acima coloca o binário verificado por checksum `codewhale` e seu alias curto `codew` em `~/.local/bin` no macOS e no Linux. Se o shell informar `command not found`, adicione `~/.local/bin` ao seu `PATH` ([detalhes](../INSTALL.md#put-it-on-your-path)). Atualize no local com `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Binários pré-compilados são distribuídos para Linux, macOS e Windows (x64 e arm64). Scoop, Nix, Homebrew no Linux, Android/Termux (prévia) e downloads manuais estão em [docs/INSTALL.md](../INSTALL.md); imagens de contêiner estão em [docs/DOCKER.md](../DOCKER.md). Use apenas um método de instalação por máquina para que dois binários não se sobreponham no `PATH`.

## Início rápido

1. Faça commit ou `git stash` do seu trabalho e execute `codewhale` na raiz de um repositório git.
2. Execute `/provider` (ou pressione `F3`) para escolher um provedor, colar uma chave de API e selecionar um modelo. DeepSeek é o padrão; se nenhuma chave estiver configurada e um servidor Ollama local responder, o Codewhale oferece seus modelos na primeira mensagem.
3. Envie uma tarefa:

```text
Fix the failing tests and explain what changed.
```

Comandos somente leitura são executados imediatamente. Comandos de shell que alteram o estado pedem aprovação: `y` executa uma vez, `a` permite comandos correspondentes pelo restante da sessão e `n` recusa. Execute `/undo` para reverter o último turno e suas edições de arquivo, ou `/help` para comandos e atalhos de teclado.

## Recursos

- **Mais de 50 provedores integrados e modelos locais:** Anthropic, OpenAI, DeepSeek (`deepseek-flash` / V4.1 Flash por padrão; [detalhes](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, qualquer endpoint compatível com OpenAI e runtimes locais via Ollama, vLLM ou SGLang. Troque de modelo durante a sessão com `/model`, roteie por turno com `/model auto` (`/router`) e inspecione latência e custo com `/turn inspect`. Veja [Provedores](../PROVIDERS.md).
- **Recibos e reversão:** `/receipts` (ou `codewhale receipts --last` ao sair) registra arquivos alterados, comandos, chamadas web/MCP, subagentes, aprovações e falhas. `/undo` desfaz o último turno em uma etapa; `/restore` retorna o workspace a um snapshot anterior (requer `/trust on` ou Full Access). Veja [Recibos](../RECEIPTS.md).
- **Subagentes, Fleets e workflows:** Conduza objetivos em vários turnos com `/goal`, delegue trabalho para [subagentes](../SUBAGENTS.md), coordene [Fleets](../FLEET.md) por função e modelo ou execute [workflows](../WORKFLOW_AUTHORING.md) versionados no repositório.
- **MCP, skills, hooks e plugins:** Conecte [servidores MCP](../MCP.md), [skills](../SKILLS.md), [plugins](../PLUGINS.md) e [hooks](../HOOKS.md) de ciclo de vida. O modo de código experimental (`execute_tools`) agrupa várias chamadas MCP/plugin sob suas regras normais de aprovação. O complemento opcional para macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) permanece desativado até ser revisado e habilitado explicitamente.
- **Compatibilidade imediata:** Lê `AGENTS.md` automaticamente. Habilite `CLAUDE.md`, `.cursorrules` ou `.github/copilot-instructions.md` via `project_instruction_imports` em [config.example.toml](../../config.example.toml). Importe configurações e servidores MCP do Claude Code com `/import-claude`, ou instale plugins do Claude Code (skills, comandos, perfis de agente, MCP) e plugins DeepSeek Harness (DSH) com `/plugin install` ([detalhes](../CLAUDE_PLUGIN_COMPAT.md)).

## Pontos de entrada

| Comando | Finalidade |
| --- | --- |
| `codewhale` | Aplicativo interativo de terminal |
| `codewhale exec "…"` | Executor headless de tarefa única para scripts e CI ([guia](../INSTALL.md#headless-scripts-ci); `--auto` usa Full Access salvo em sandbox) |
| `codewhale web` | Cliente web local em `127.0.0.1` ([guia](../WEB.md)) |
| `codewhale review --pr N` | Revisão consultiva de PR no GitHub; [configuração de bot do GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) com sua própria chave |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md) HTTP local para threads, eventos, aprovações e diffs por turno |
| `codewhale serve --acp` | Agent Client Protocol via stdio para editores como Zed |

Todos os pontos de entrada compartilham o mesmo motor em Rust ([arquitetura](../ARCHITECTURE.md)). Uma [extensão para VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) mantida pela comunidade ([código-fonte](https://github.com/HengQuWorld/CodeWhale-VSCode)) conecta-se ao mesmo motor. Consulte as [notas da versão mais recente](https://github.com/codewhale-hq/Codewhale/releases/latest) e o [roadmap](../ROADMAP.md) para ver o status atual de lançamentos, desktop e nuvem.

## Permissões, sandboxing e privacidade

`Shift+Tab` alterna os modos de permissão: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` apenas completa). **Accept Edits** (padrão) aplica edições dentro do repositório git e pergunta antes de executar comandos. **Ask** solicita confirmação em cada edição e comando. **Plan** é somente leitura. **Auto-Review** usa regras e um modelo revisor para aprovar chamadas rotineiras e bloquear as de alto risco. **Full Access** pula confirmações e desativa o sandbox do sistema operacional (regras do repositório e políticas gerenciadas continuam valendo). Veja [Modos](../MODES.md).

Em Ask, Accept Edits e Auto-Review, os comandos rodam em um sandbox do sistema operacional quando disponível: Seatbelt no macOS e bubblewrap no Linux (o Windows ainda não possui sandbox do SO). Comandos em sandbox e sessões de terminal não podem ler caminhos de credenciais como `~/.ssh`, `~/.aws` ou arquivos `.env`. Execute `/preview-request` para inspecionar uma requisição redigida antes do envio.

Chaves de API adicionadas com `/provider` ficam na sua máquina, e modelos locais não exigem chave. `/cost` e o rodapé acompanham o gasto da sessão quando o preço é conhecido (um preço desconhecido aparece como desconhecido, nunca como gratuito).

A telemetria agregada vem ativada por padrão (versão, SO, duração/resultado da sessão, IDs de provedores integrados, contagens de recursos/erros; ID de instalação aleatório rotacionado a cada 90 dias; nunca código, prompts, conversas, nomes de modelos, caminhos ou credenciais). Desative com:

```bash
codewhale config set telemetry false
```

Defina `CODEWHALE_NO_UPDATE_CHECK=1` para pular a verificação inicial de lançamentos no GitHub. Detalhes: [ordem de autorização](../AUTHORIZATION_ORDER.md), [sandboxing](../SANDBOX.md) e [telemetria](../TELEMETRY.md).

## Documentação

Os guias acompanham `main` e podem descrever mudanças ainda não incluídas na última versão publicada.

| Comece por aqui | Referência |
| --- | --- |
| [Guia do usuário](../GUIDE.md) | [Configuração](../CONFIGURATION.md) |
| [Instalação](../INSTALL.md) | [Arquitetura](../ARCHITECTURE.md) |
| [Provedores e modelos locais](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modos e permissões](../MODES.md) | [Criação de plugins](../PLUGIN_AUTHORING.md) |
| [Atalhos de teclado](../KEYBINDINGS.md) | [Recibos](../RECEIPTS.md) |
| [Revisão de pull requests no GitHub](../GITHUB_ACTION.md) | [Toda a documentação](../README.md) |

## Contribuição e licença

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Mural de colaboradores do Codewhale"></a>

O [mural de colaboradores](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) registra código, commits em coautoria, PRs incorporados e relatos creditados. Relatos de bugs e sugestões de recursos vão em [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), dúvidas em [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), e as diretrizes de PR e estrutura estão em [CONTRIBUTING.md](../../CONTRIBUTING.md) ([estrutura do projeto](../../CONTRIBUTING.md#project-structure)). Participe no [Discord](https://discord.gg/37gfS3ksug) ou adicione `hunterbown` no WeChat para o grupo Whale Brothers.

O Codewhale começou como `deepseek-tui` e mantém compatibilidade retroativa com suas configurações e sessões. É mantido de forma independente e não tem afiliação com a DeepSeek nem com qualquer outro provedor de modelos.

[MIT](../../LICENSE). Avisos de terceiros: [fontes incluídas](../../THIRD_PARTY_NOTICES.md) e [designs adaptados](../THIRD_PARTY_NOTICES.md).
