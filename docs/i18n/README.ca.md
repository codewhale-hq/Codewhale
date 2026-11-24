<!-- source: README.md sha256:7e0e7ebc680b -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agent de programació de codi obert en Rust per al teu terminal. Qualsevol model, flotes multiagent, aïllament a nivell de sistema operatiu i rebuts verificables.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Lloc web](https://codewhale.net) · [Documentació](../README.md) · [Registre de canvis](../../CHANGELOG.md) · [Contribuir](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · Català

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 en un terminal després d’una tasca real amb Ollama local (qwen3:4b): ha editat ls.py, ha esperat aprovació abans d’executar python3 -m unittest -v test_ls.py, ha executat 2 proves superades i n’ha informat el resultat.">

<sub>Sessió real en la versió publicada Codewhale v0.10.4 (48669ad108fa) amb Ollama local (qwen3:4b) en mode Accept Edits.</sub>

</div>

## Instal·lació

L’script d’instal·lació de dalt col·loca el binari verificat amb suma de comprovació `codewhale` i el seu àlies curt `codew` a `~/.local/bin` a macOS i Linux. Si la teva shell indica `command not found`, afegeix `~/.local/bin` al `PATH` ([detalls](../INSTALL.md#put-it-on-your-path)). Actualitza directament amb `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Es publiquen binaris precompilats per a Linux, macOS i Windows (x64 i arm64). Scoop, Nix, Homebrew a Linux, Android/Termux (vista prèvia) i les baixades manuals es troben a [docs/INSTALL.md](../INSTALL.md); les imatges de contenidor són a [docs/DOCKER.md](../DOCKER.md). Fes servir un sol mètode d’instal·lació per màquina perquè dos binaris no s’amaguin mútuament al `PATH`.

## Inici ràpid

1. Fes commit o `git stash` de la teva feina i executa `codewhale` des de l’arrel d’un repositori git.
2. Executa `/provider` (o prem `F3`) per triar un proveïdor, enganxar una clau API i seleccionar un model. DeepSeek és el valor predeterminat; si no hi ha cap clau configurada i respon un servidor Ollama local, Codewhale t’ofereix els seus models al primer missatge.
3. Dona-li una tasca:

```text
Fix the failing tests and explain what changed.
```

Les ordres de només lectura s’executen immediatament. Les ordres de shell que modifiquen l’estat demanen aprovació: `y` executa una vegada, `a` permet ordres coincidents durant la resta de la sessió i `n` denega. Executa `/undo` per revertir l’últim torn i les seves edicions de fitxers, o `/help` per veure ordres i dreceres de teclat.

## Capacitats

- **Més de 50 proveïdors integrats i models locals:** Anthropic, OpenAI, DeepSeek (`deepseek-flash` / V4.1 Flash per defecte; [detalls](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, qualsevol endpoint compatible amb OpenAI i entorns d’execució locals mitjançant Ollama, vLLM o SGLang. Canvia de model a mitja sessió amb `/model`, encamina per torn amb `/model auto` (`/router`) i inspecciona latència i cost amb `/turn inspect`. Consulta [Proveïdors](../PROVIDERS.md).
- **Rebuts i reversió:** `/receipts` (o `codewhale receipts --last` en sortir) registra fitxers canviats, ordres, crides web/MCP, subagents, aprovacions i errors. `/undo` reverteix l’últim torn en un sol pas; `/restore` torna l’espai de treball a una instantània anterior (requereix `/trust on` o Full Access). Consulta [Rebuts](../RECEIPTS.md).
- **Subagents, Fleets i fluxos de treball:** Mantén objectius de diversos torns amb `/goal`, delega feina a [subagents](../SUBAGENTS.md), coordina [Fleets](../FLEET.md) per rol i model o executa [fluxos de treball](../WORKFLOW_AUTHORING.md) versionats al repositori.
- **MCP, skills, hooks i plugins:** Connecta [servidors MCP](../MCP.md), [skills](../SKILLS.md), [plugins](../PLUGINS.md) i [hooks](../HOOKS.md) de cicle de vida. El mode de codi experimental (`execute_tools`) agrupa diverses crides MCP/plugin sota les teves regles d’aprovació habituals. El complement opcional de macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) resta desactivat fins que el revisis i l’activis explícitament.
- **Compatibilitat directa:** Llegeix `AGENTS.md` automàticament. Activa `CLAUDE.md`, `.cursorrules` o `.github/copilot-instructions.md` mitjançant `project_instruction_imports` a [config.example.toml](../../config.example.toml). Importa la configuració i els servidors MCP de Claude Code amb `/import-claude`, o instal·la plugins de Claude Code (skills, ordres, perfils d’agent, MCP) i plugins DeepSeek Harness (DSH) amb `/plugin install` ([detalls](../CLAUDE_PLUGIN_COMPAT.md)).

## Punts d’entrada

| Ordre | Propòsit |
| --- | --- |
| `codewhale` | Aplicació interactiva de terminal |
| `codewhale exec "…"` | Executor sense interfície d’una sola tasca per a scripts i CI ([guia](../INSTALL.md#headless-scripts-ci); `--auto` usa Full Access llevat que estigui en sandbox) |
| `codewhale web` | Client web local a `127.0.0.1` ([guia](../WEB.md)) |
| `codewhale review --pr N` | Revisió orientativa de PR a GitHub; [configuració de bot de GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) amb la teva clau |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md) HTTP local per a fils, esdeveniments, aprovacions i diffs per torn |
| `codewhale serve --acp` | Agent Client Protocol per stdio per a editors com Zed |

Tots els punts d’entrada comparteixen un únic motor en Rust ([arquitectura](../ARCHITECTURE.md)). Una [extensió de VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) mantinguda per la comunitat ([codi font](https://github.com/HengQuWorld/CodeWhale-VSCode)) es connecta al mateix motor. Consulta les [últimes notes de versió](https://github.com/codewhale-hq/Codewhale/releases/latest) i el [full de ruta](../ROADMAP.md) per a l’estat actual de versions, escriptori i núvol.

## Permisos, sandboxing i privadesa

`Shift+Tab` alterna els modes de permís: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` només completa). **Accept Edits** (per defecte) aplica edicions dins del repositori git i demana confirmació abans d’executar ordres. **Ask** demana confirmació en cada edició i ordre. **Plan** és de només lectura. **Auto-Review** utilitza regles i un model revisor per aprovar crides rutinàries i bloquejar les d’alt risc. **Full Access** omet les confirmacions i desactiva el sandbox del sistema operatiu (les regles del repositori i la política gestionada continuen vigents). Consulta [Modes](../MODES.md).

A Ask, Accept Edits i Auto-Review, les ordres s’executen dins d’un sandbox del sistema operatiu quan és disponible: Seatbelt a macOS i bubblewrap a Linux (Windows encara no té sandbox del SO). Les ordres en sandbox i les sessions de terminal no poden llegir rutes de credencials com `~/.ssh`, `~/.aws` o fitxers `.env`. Executa `/preview-request` per inspeccionar una petició sortint redactada abans d’enviar-la.

Les claus API afegides amb `/provider` es queden a la teva màquina, i els models locals no requereixen clau. `/cost` i el peu de pàgina segueixen la despesa de la sessió quan el preu és conegut (un preu desconegut es mostra com a desconegut, mai com a gratuït).

La telemetria agregada està activada per defecte (versió, SO, durada/resultat de la sessió, IDs de proveïdors integrats, IDs de models del catàleg públic integrat (els models personalitzats, privats o locals es compten com `other`), totals de testimonis, recomptes de crides/errors d'eines integrades i comptadors de funcions/errors; ID d'instal·lació aleatori que rota cada 90 dies; mai codi, prompts, converses, noms de models personalitzats/privats, noms de fitxers/repos/branques, credencials ni adreces IP). Desactiva-la amb:

```bash
codewhale config set telemetry false
```

En iniciar-se, Codewhale també comprova nous llançaments a GitHub (`CODEWHALE_NO_UPDATE_CHECK=1` per ometre) i actualitza el catàleg públic `models.dev` cada 24 hores (`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` o `[catalog] auto_refresh = false` per ometre; desactivat automàticament en CI i amb `DO_NOT_TRACK`). Detalls: [ordre d'autorització](../AUTHORIZATION_ORDER.md), [sandboxing](../SANDBOX.md) i [telemetria](../TELEMETRY.md).

## Documentació

Les guies segueixen `main` i poden descriure canvis que encara no són a l’última versió publicada.

| Comença aquí | Referència |
| --- | --- |
| [Guia d’usuari](../GUIDE.md) | [Configuració](../CONFIGURATION.md) |
| [Instal·lació](../INSTALL.md) | [Arquitectura](../ARCHITECTURE.md) |
| [Proveïdors i models locals](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modes i permisos](../MODES.md) | [Creació de plugins](../PLUGIN_AUTHORING.md) |
| [Dreceres de teclat](../KEYBINDINGS.md) | [Rebuts](../RECEIPTS.md) |
| [Revisió de pull requests a GitHub](../GITHUB_ACTION.md) | [Tota la documentació](../README.md) |

## Contribució i llicència

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Mur de col·laboradors de Codewhale"></a>

El [mur de col·laboradors](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) registra codi, commits en coautoria, PRs integrades i informes acreditats. Els informes d’errors i propostes de funcionalitats van a [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), les preguntes a [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), i les guies de PR i l’estructura del projecte són a [CONTRIBUTING.md](../../CONTRIBUTING.md) ([estructura del projecte](../../CONTRIBUTING.md#project-structure)). Uneix-te al nostre [Discord](https://discord.gg/37gfS3ksug) o afegeix `hunterbown` a WeChat per al grup Whale Brothers.

Codewhale va començar com a `deepseek-tui` i manté la retrocompatibilitat amb la seva configuració i les seves sessions. Es manté de manera independent i no està afiliat a DeepSeek ni a cap altre proveïdor de models.

[MIT](../../LICENSE). Avisos de tercers: [fonts incloses](../../THIRD_PARTY_NOTICES.md) i [dissenys adaptats](../THIRD_PARTY_NOTICES.md).
