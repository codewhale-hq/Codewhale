<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**L’agent de codi obert que funciona amb qualsevol model.**

Codewhale llegeix el teu projecte, edita fitxers, executa ordres i comprova la seva
pròpia feina — al teu terminal, amb un model allotjat o local que tu tries.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Lloc web](https://codewhale.net) · [Documentació](docs/README.md) · [Registre de canvis](CHANGELOG.md) · [Contribuir](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Una sessió de terminal de Codewhale" width="760">

<sub>Captura real del terminal de Codewhale en una sessió de demostració local configurada.</sub>

</div>

## Instal·lació

macOS i Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

L’instal·lador descarrega binaris amb suma de verificació a `~/.local/bin`. Si
després `codewhale` respon "command not found", executa l’única línia de PATH que
mostra l’instal·lador, o consulta [Afegir-lo al PATH](docs/INSTALL.md#put-it-on-your-path).
Actualitza quan vulguis amb `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo i altres vies</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew a Linux, Android/Termux, les descàrregues manuals amb
verificació de la suma de control i el mirall opcional de CNB s’expliquen a la
[guia d’instal·lació](docs/INSTALL.md). Tria una sola via: diverses
instal·lacions a la mateixa màquina acaben disputant-se el `PATH`.

</details>

## Inici ràpid

**Les claus API dels teus proveïdors, en un sol lloc.** [Crea un compte de Codewhale](https://app.codewhale.net/register)
o executa `codewhale login` per iniciar la sessió. Desa i actualitza les claus API al teu compte
i fes-les servir a través de la ruta de models de Codewhale als dispositius on hagis iniciat la sessió.
[Configura les claus del compte](docs/CONFIGURATION.md#account-provider-keys).
Iniciar la sessió no puja les claus locals existents; pots continuar fent-ne un ús local sense compte.

1. **Obre el teu projecte.** Executa `codewhale` a la carpeta on vols treballar.
2. **Connecta un model.** Executa `/provider` (o prem `F3`) per afegir una clau
   allotjada o triar un entorn local. Si Ollama ja s’està executant amb un model
   de xat, Codewhale hi canvia per si sol. Fes servir `/model` per canviar de model.
3. **Dona-li una tasca concreta.**

```text
Fix the failing tests and explain what changed.
```

La mateixa tasca es pot executar sense interfície des d’un script o una tasca de CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Executa `/help` per veure les ordres i les dreceres de teclat.

## Maneres d’executar-lo

Tots els clients fan servir el mateix Runtime local de Codewhale, de manera que
les sessions, les eines i els permisos es comporten igual a tot arreu.

| Ordre | Què fa |
| --- | --- |
| `codewhale` | La interfície interactiva de terminal |
| `codewhale exec "…"` | Un torn sense interfície des d’un script o CI; `--auto` permet les eines de fitxers i de shell, `--output-format stream-json` emet JSON en streaming |
| `codewhale web` | El [client de navegador local](docs/WEB.md) inclòs, a `127.0.0.1` |
| `codewhale review --pr N` | Una [revisió de pull request](docs/GITHUB_ACTION.md) merament orientativa; publicar-la és opcional |
| Runtime API | Una [API HTTP local](docs/RUNTIME_API.md) per a fils, esdeveniments i aprovacions |

S’està desenvolupant una aplicació d’escriptori nativa (GPUI) com a client del
producte amb sessió iniciada; consulta la
[pàgina del producte](https://codewhale.net/en/product) per veure’n la disponibilitat. L’[extensió de VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode),
mantinguda per la comunitat, es connecta al mateix Runtime des d’una barra lateral
([codi font](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Què fa

- **Qualsevol model, sense dependència d’un proveïdor.** Més de 40 rutes de
  proveïdors integrades — Anthropic, DeepSeek, Google, Mistral, Moonshot,
  OpenAI, OpenRouter, xAI i més — a més de qualsevol endpoint compatible amb
  OpenAI i models locals mitjançant Ollama, vLLM o SGLang. [Proveïdors](docs/PROVIDERS.md)
- **Tu mantens el control.** El mode Plan explora sense canviar res; Work i
  Operate fan canvis. Les postures d’aprovació decideixen quan una crida a una
  eina necessita el teu vistiplau, `/undo` i `/restore` recuperen els canvis de
  l’espai de treball, i `/receipts` enumera cada fitxer, ordre i aprovació d’una
  sessió. [Modes](docs/MODES.md) · [Rebuts](docs/RECEIPTS.md)
- **Pensat per a feines llargues.** Defineix un `/goal` durador, delega feina
  acotada a [subagents](docs/SUBAGENTS.md), executa [equips d’agents](docs/FLEET.md)
  supervisats amb una comprovació prèvia de la despesa, o automatitza’ls com a
  [fluxos de treball](docs/WORKFLOW_AUTHORING.md) versionats al repositori.
- **Amplia el que ja fas servir.** Connecta [servidors MCP](docs/MCP.md),
  instal·la [skills](docs/SKILLS.md) i [plugins](docs/PLUGINS.md), executa
  [hooks](docs/HOOKS.md) en esdeveniments de sessió i d’eines, i carrega els
  [plugins de Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) existents.
- **Computer Use.** Un plugin inclòs afegeix eines per observar i operar altres
  aplicacions. Revisa l’accés que sol·licita i habilita’l abans de fer-lo servir.
  [Guia](crates/tui/plugins/computer-use/README.md)

## Modes i permisos

| | Es tria amb | Opcions |
| --- | --- | --- |
| **Mode** — el que fa l’agent | `Tab` o `/mode` | Plan (explorar, sense canvis) · Work (editar i executar) · Operate (portar un objectiu endavant amb passos planificats i verificats) |
| **Postura** — quan pregunta abans | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access continua respectant els límits estrictes de les polítiques. La
[guia de modes i permisos](docs/MODES.md) explica cada opció.

## Seguretat

Codewhale s’executa al teu equip amb l’accés que li concedeixis. Les postures
d’aprovació i les regles del repositori limiten el que l’agent pot fer, i les
ordres s’executen dins d’un sandbox del sistema operatiu on és compatible
(Seatbelt a macOS; bubblewrap a Linux quan està instal·lat i funciona). `/preview-request` mostra la
sol·licitud exacta, amb les dades sensibles ocultades, abans d’enviar res. Els
preus de models desconeguts continuen sent desconeguts en lloc de presentar-se
com a gratuïts.

Consulta l’[ordre d’autorització](docs/AUTHORIZATION_ORDER.md), el
[sandboxing](docs/SANDBOX.md) i la [telemetria](docs/TELEMETRY.md): els
comptadors d’ús estan activats per defecte i `codewhale config set telemetry false`
els desactiva.

## Documentació

| Comença aquí | Aprofundeix | 
| --- | --- |
| [Instal·lació](docs/INSTALL.md) | [Configuració](docs/CONFIGURATION.md) |
| [Proveïdors i models locals](docs/PROVIDERS.md) | [Arquitectura](docs/ARCHITECTURE.md) |
| [Modes i permisos](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Dreceres de teclat](docs/KEYBINDINGS.md) | [Creació de plugins](docs/PLUGIN_AUTHORING.md) |
| [Revisió de PR a GitHub](docs/GITHUB_ACTION.md) | [Tota la documentació](docs/README.md) |

## Comunitat

Els informes d’errors, les idees de funcionalitats i els pull requests són
benvinguts, tant si fa mesos que fas servir Codewhale com si el proves per
primera vegada. Si falta un proveïdor o un flux de treball resulta incòmode,
[obre un issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) o
[envia un pull request](CONTRIBUTING.md). Les primeres contribucions són
benvingudes, i qui contribueix conserva el crèdit pel treball que s’hi integra.
L’[estructura del repositori](CONTRIBUTING.md#project-structure) és un bon punt
de partida.

Uneix-te al [Discord](https://discord.gg/37gfS3ksug), o afegeix Hunter a WeChat
(`hunterbown`) i demana entrar al grup Whale Brothers.

## Història i llicència

Codewhale va començar com a `deepseek-tui` i encara llegeix la configuració i les
sessions d’aquest projecte. Ara és neutral respecte dels proveïdors, es manté de
manera independent i no està afiliat a cap proveïdor de models. Gràcies a
[tots els col·laboradors](docs/CONTRIBUTORS.md) i a les comunitats de codi obert
que han ajudat que creixi.

[MIT](LICENSE). Les parts adaptades d’altres projectes de codi obert es recullen
als [avisos de tercers](docs/THIRD_PARTY_NOTICES.md).
