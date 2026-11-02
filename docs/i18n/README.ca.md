<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Un agent de programació de codi obert per al teu terminal que funciona amb qualsevol model.**

Dona-li una tasca a Codewhale i llegirà el repositori, editarà fitxers i
executarà ordres fins que la tasca estigui feta o necessiti alguna resposta teva.
Funciona amb més de 40 proveïdors integrats, DeepSeek V4.1 Flash per defecte, i amb models
que s’executen a la teva pròpia màquina. `/receipts` llista els fitxers que ha
canviat, les ordres que ha executat i les crides web i MCP que ha fet.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Lloc web](https://codewhale.net) · [Documentació](../README.md) · [Registre de canvis](../../CHANGELOG.md) · [Contribuir](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Una sessió de terminal de Codewhale" width="760">

<sub>Una compilació de desenvolupament de la 0.10.2 amb un model de prova amb guió, en Full Access dins d’un espai de treball d’un sol ús. L’edició, tres proves i una revisió d’un subagent es van executar de debò.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — un 30% més d’ús de DeepSeek v4.1 Flash" width="760">
  </a>
</p>

[Consulta l’oferta de Codewhale × OrcaRouter](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale rep una comissió per recomanació d’OrcaRouter.

## Instal·lació

macOS i Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

L’instal·lador desa `codewhale` i el seu àlies curt `codew` a `~/.local/bin`,
després de comprovar-ne la suma de verificació. Si després el teu shell diu
`command not found`, executa la línia de PATH que mostra l’instal·lador
([detalls](../INSTALL.md#put-it-on-your-path)). `codewhale update`
l’actualitza. Aquestes ordres instal·len la darrera versió. Aquesta pàgina
segueix `main`, de manera que una funcionalitat marcada amb una versió arriba en
aquella versió; el [registre de canvis](../../CHANGELOG.md) llista què afegeix cada
versió.

<details>
<summary><b>Windows, npm, Cargo i altres maneres d’instal·lar-lo</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Hi ha binaris precompilats per a Linux, macOS i Windows en x64 i arm64. Nix,
Homebrew a Linux, Android/Termux (versió preliminar) i les descàrregues manuals
s’expliquen a la [guia d’instal·lació](../INSTALL.md), i els contenidors a
[Docker](../DOCKER.md). Fes servir un sol mètode d’instal·lació per màquina;
si no, acabaràs amb dues còpies al `PATH` i s’executarà la que hi aparegui
primer.

</details>

## Inici ràpid

1. Fes un commit o un stash de la teva feina i després executa `codewhale` des
   de l’arrel d’un repositori git. Per defecte, hi aplica les edicions de
   fitxers sense preguntar i et mostra el diff.
2. Executa `/provider` (o prem `F3`), tria un proveïdor, enganxa’n la clau API
   i escull un model. DeepSeek és el proveïdor per defecte. Si no tens cap
   clau, tria un entorn local. Si no hi ha cap proveïdor configurat i Ollama ja
   serveix un model de xat, Codewhale el fa servir automàticament; a partir de
   la 0.10.2 te l’ofereix quan envies el primer missatge.
3. Escriu una tasca:

```text
Fix the failing tests and explain what changed.
```

Les ordres de només lectura s’executen directament. Per a qualsevol altra ordre
de shell, Codewhale pregunta primer: `y` l’executa una vegada, `a` permet ordres
com aquesta durant la resta de la conversa, `n` la rebutja. `/undo` desfà la
teva última petició i els canvis que va fer als fitxers. `/help` llista les ordres i les tecles.

Per a scripts i CI, `codewhale exec` executa una sola tasca sense la interfície.
Per defecte, `--auto` fa servir Full Access: les eines s’executen sense preguntar,
les ordres de shell no tenen cap sandbox de Codewhale i el mode de confiança
permet que les eines de fitxers escriguin fora de l’espai de treball. Afegeix
`--sandbox workspace-write` per mantenir el sandbox de shell i les restriccions
de les eines de fitxers a l’espai de treball, amb el mode de confiança desactivat:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Modes i permisos

Una instal·lació nova comença en mode Work amb el permís Ask.

| | Es canvia amb | Opcions |
| --- | --- | --- |
| Mode | `Tab` (amb l’entrada buida) o `/mode` | **Plan** llegeix i cerca però no canvia res. **Work** edita fitxers i executa ordres. **Operate** té el mateix accés que Work, tracta la teva petició com un objectiu i continua fins que l’objectiu està verificat o necessita alguna resposta teva. |
| Permís | `Shift+Tab` | **Ask** aplica les edicions de fitxers al teu repositori sense preguntar i pregunta abans de les ordres de shell que no són de només lectura. **Auto-Review** deixa que unes regles i un model revisor aprovin les crides rutinàries i rebutja les d’alt risc; canvia a Ask per aprovar-les tu mateix. **Full Access** no pregunta mai i desactiva el sandbox de Codewhale; les regles del repositori i la política gestionada continuen aplicant-se. |

Consulta la [guia de modes i permisos](../MODES.md) per als detalls.

## Què fa

### Novetats de la 0.10.2

- `/pet on` converteix el teu terminal en una balena animada que mostra què fa
  l’agent, mentre el quadre de missatges continua actiu. Les seves 980
  partícules escriuen el pas actual en anglès, xinès o japonès.
- `/workbar terminal` mostra els shells que executa l’agent mentre s’executen.
  Escriu en un d’ells, o prem `Ctrl+N` per obrir un shell propi (macOS i
  Linux).
- `/undo` desfà tota la teva última petició d’un sol cop, inclosos tots els
  fitxers que va canviar, i `/diff` funciona en carpetes sense Git.
- Quan un pla està llest, Codewhale et pregunta si vols començar
  Work amb Ask o Auto-Review, o continuar planificant.
- `/color` dona a cada xat el seu propi color al quadre de missatges, desat amb
  la sessió.
- `web_search` pot fer servir la nova Search API de DeepSeek: defineix
  `[search] provider = "deepseek"` amb una clau de DeepSeek.
- Fora de Full Access, un `curl` o `wget` que pujaria una credencial no
  s’executa mai sense la teva aprovació, i es rebutja qualsevol ordre que amagui
  Unicode invisible. `write` ja no substitueix un fitxer que l’agent no ha
  llegit, ni un que ha canviat després que el llegís.

### Models

Hi ha més de 40 proveïdors integrats, entre els quals Anthropic, OpenAI,
DeepSeek, Google, Mistral, Moonshot, OpenRouter i xAI. Funciona qualsevol
endpoint compatible amb OpenAI, i també els models locals mitjançant Ollama,
vLLM o SGLang. El model per defecte és `deepseek-flash` de DeepSeek, que
DeepSeek va passar a DeepSeek-V4.1-Flash-1017 el 2026-10-17; la 0.10.1
l’executa sense cap actualització
([detalls](../PROVIDERS.md#deepseek-v41-flash)).

`/model` canvia de model entre torns, i `/model auto` en tria un per a cada torn
(`/router` configura com ho fa). `/turn inspect` mostra quin model ha fet servir
un torn, quant ha costat i quant ha trigat. `codewhale auth chatgpt` et permet
fer servir un pla de ChatGPT en lloc d’una clau API (experimental). Consulta
[Proveïdors](../PROVIDERS.md).

### Rebuts i desfer

`/receipts`, o `codewhale receipts --last` després de sortir, llista els fitxers
canviats, les ordres executades, les crides web i MCP, els agents iniciats, les
aprovacions i qui les va donar, i els errors. `/undo` desfà la teva última petició i els fitxers que va canviar, d’un sol cop i sense el mode de confiança. `/restore` torna tot l’espai de treball a una
instantània anterior; com que això sobreescriu les edicions posteriors,
necessita `/trust on` o Full Access. Consulta [Rebuts](../RECEIPTS.md).

Les execucions d’exec que ofereixen eines es desen en tots els formats de
sortida, de manera que `codewhale receipts --last` funciona després
d’`exec --auto`, també amb `--json`. Les execucions que només donen una resposta
en text o amb `--json` només es desen quan es reprèn una conversa o quan
comença la compactació de la conversa; fes servir `--output-format stream-json`
per desar-les sempre per als rebuts o per a `exec --continue`.

### Feines més llargues

`/goal` defineix un objectiu que es manté entre torns. Codewhale pot repartir la
feina entre [subagents](../SUBAGENTS.md), executar una [flota](../FLEET.md)
d’agents o seguir un [flux de treball](../WORKFLOW_AUTHORING.md) versionat al
repositori.

### Si véns d’un altre agent

Codewhale llegeix `AGENTS.md` pel seu compte. Els fitxers d’instruccions escrits
per a altres eines, com ara `CLAUDE.md`, `.cursorrules` o
`.github/copilot-instructions.md`, només es carreguen si ho actives amb
`project_instruction_imports = ["claude"]` (o `"cursor"`, `"cline"`,
`"windsurf"`, `"copilot"`, `"all"`) a la configuració; consulta
[config.example.toml](../../config.example.toml). Codewhale avisa quan en troba un que
no ha carregat.

`/import-claude` prepara una importació de la teva configuració i dels servidors
MCP de Claude Code perquè la revisis. `/plugin install` accepta plugins de
Claude Code que continguin skills, ordres, perfils d’agent i servidors MCP, i
rebutja els plugins que necessiten hooks o servidors LSP
([detalls](../CLAUDE_PLUGIN_COMPAT.md)).

### Extensions

Connecta [servidors MCP](../MCP.md), afegeix [skills](../SKILLS.md) i
[plugins](../PLUGINS.md), i executa [hooks](../HOOKS.md) en esdeveniments de
sessió i d’eines. Code mode (experimental, activat per defecte) permet que el
model escrigui un sol programa `execute_tools` que crida diverses eines de MCP i
de plugins; cada crida continua subjecta a les teves regles d’aprovació i
conserva el seu rebut. El plugin Computer Use instal·lat independentment (experimental, només
macOS) pot observar i operar altres aplicacions. Roman desactivat fins que en revises
l’accés i l’actives ([guia](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Maneres d’executar-lo

| Ordre | Què fa |
| --- | --- |
| `codewhale` | L’aplicació interactiva de terminal |
| `codewhale exec "…"` | Una tasca sense la interfície. Per si sol respon sense fer servir eines; `--auto` activa les eines amb Full Access per defecte, `--sandbox workspace-write` les restringeix, i `--output-format stream-json` emet els esdeveniments en streaming com a JSON |
| `codewhale web` | Una interfície web local a `127.0.0.1` ([guia](../WEB.md)) |
| `codewhale review --pr N` | Una revisió orientativa d’un pull request de GitHub. La [configuració del bot de GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) genera un workflow amb la versió fixada per a revisions automàtiques i `@codewhale review`, amb la teva pròpia clau de model i una identitat d’App opcional |
| `codewhale app-server --http` | La [Runtime API](../RUNTIME_API.md), una API HTTP local per a fils, esdeveniments, aprovacions i els canvis de fitxers de cada torn |
| `codewhale serve --acp` | L’Agent Client Protocol sobre stdio, per a editors com ara Zed |

L’aplicació de terminal, `exec`, `web` i la Runtime API comparteixen un mateix
motor ([arquitectura](../ARCHITECTURE.md)). Una
[extensió de VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
mantinguda per la comunitat s’hi connecta des de la barra lateral
([codi font](https://github.com/HengQuWorld/CodeWhale-VSCode)).
Hi ha en desenvolupament una aplicació d’escriptori nativa basada en la Runtime
API; encara no té cap descàrrega pública.

## Cost i comptes

Codewhale és gratuït i té llicència MIT. Amb la teva pròpia clau API pagues
directament al proveïdor; els models locals no necessiten clau. La barra
inferior i `/cost` mostren quant ha costat una sessió quan se’n coneix el preu,
i un preu desconegut es mostra com a desconegut, mai com a gratuït.

No necessites cap compte. Les claus que afegeixes amb `/provider` es queden a la
teva màquina.

## Seguretat i privadesa

En Ask i Auto-Review, les ordres s’executen dins d’un sandbox del sistema
operatiu allà on el sistema en té: Seatbelt a macOS, i bubblewrap a Linux quan
està instal·lat. Windows encara no té sandbox del sistema operatiu, i Full
Access desactiva el sandbox. Dins del sandbox, les ordres de shell no poden
llegir magatzems de credencials com ara `~/.ssh`, `~/.aws` i els fitxers `.env`;
a partir de la 0.10.2, la mateixa regla s’aplica a les sessions de terminal i a
més eines de fitxers. `/preview-request` mostra una previsualització de la
propera petició, amb les dades sensibles ocultades, sense enviar-la.

La telemetria està activada per defecte. Envia recomptes agregats (versió i
plataforma, durada i resultat de la sessió, quins proveïdors integrats has fet
servir, recomptes de funcionalitats i d’errors) amb un identificador
d’instal·lació aleatori que canvia cada 90 dies, i mai converses, codi, prompts,
noms de models, noms de fitxers o de repositoris, ni credencials. `codewhale config set telemetry false` la desactiva. En iniciar-se,
Codewhale també comprova a GitHub si hi ha una versió nova; defineix
`CODEWHALE_NO_UPDATE_CHECK=1` per ometre-ho. Més informació a
[ordre d’autorització](../AUTHORIZATION_ORDER.md),
[sandboxing](../SANDBOX.md) i [telemetria](../TELEMETRY.md).

## Documentació

| Comença aquí | Referència |
| --- | --- |
| [Instal·lació](../INSTALL.md) | [Configuració](../CONFIGURATION.md) |
| [Proveïdors i models locals](../PROVIDERS.md) | [Arquitectura](../ARCHITECTURE.md) |
| [Modes i permisos](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [Dreceres de teclat](../KEYBINDINGS.md) | [Creació de plugins](../PLUGIN_AUTHORING.md) |
| [Revisió de PR a GitHub](../GITHUB_ACTION.md) | [Tota la documentació](../README.md) |

## Comunitat

Si falta un proveïdor o un flux de treball és incòmode,
[obre un issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) o
[envia un pull request](../../CONTRIBUTING.md). Qui contribueix conserva el crèdit pel
treball que s’hi integra, i l’[estructura del repositori](../../CONTRIBUTING.md#project-structure)
mostra on és cada cosa.

Xateja al [Discord](https://discord.gg/37gfS3ksug), o afegeix el mantenidor,
Hunter, a WeChat (`hunterbown`) i demana entrar al grup Whale Brothers.

## Història i llicència

Codewhale va començar com a `deepseek-tui` i encara llegeix la configuració i
les sessions d’aquell projecte. Es manté de manera independent i no està afiliat
a DeepSeek ni a cap altre proveïdor de models. Gràcies a
[tothom qui hi ha contribuït](../CONTRIBUTORS.md).

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). El codi adaptat d’altres projectes de codi obert es recull als
[avisos de tercers](../THIRD_PARTY_NOTICES.md).
