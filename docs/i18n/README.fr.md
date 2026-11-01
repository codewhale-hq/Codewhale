<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Un agent de code open source pour votre terminal qui fonctionne avec n’importe quel modèle.**

Donnez une tâche à Codewhale : il lit le dépôt, modifie des fichiers et exécute
des commandes jusqu’à ce que la tâche soit terminée ou qu’il ait besoin d’une
réponse de votre part. Il fonctionne avec plus de 40 fournisseurs intégrés,
DeepSeek V4.1 Flash par défaut, ainsi qu’avec des modèles qui tournent sur votre
propre machine. `/receipts` liste les fichiers qu’il a modifiés, les commandes
qu’il a exécutées et les appels web et MCP qu’il a effectués.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Site web](https://codewhale.net) · [Documentation](../README.md) · [Journal des modifications](../../CHANGELOG.md) · [Contribuer](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Une session de terminal Codewhale" width="760">

<sub>Une version de développement de la 0.10.2 avec un modèle de test scripté, en Full Access dans un espace de travail jetable. La modification, trois tests et une revue par un sous-agent ont réellement été exécutés.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — 30 % d’utilisation de DeepSeek v4.1 Flash en plus" width="760">
  </a>
</p>

[Voir l’offre Codewhale × OrcaRouter](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale reçoit une commission de recommandation d’OrcaRouter.

## Installation

macOS et Linux :

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

L’installeur place `codewhale` et son alias court `codew` dans `~/.local/bin`,
après vérification de leur somme de contrôle. Si votre shell répond ensuite
`command not found`, exécutez la ligne PATH qu’affiche l’installeur
([détails](../INSTALL.md#put-it-on-your-path)). `codewhale update` le met à
jour. Ces commandes installent la dernière version publiée. Cette page suit
`main` : une fonctionnalité marquée d’un numéro de version arrive avec cette
version, et le [journal des modifications](../../CHANGELOG.md) liste ce qu’apporte
chaque version.

<details>
<summary><b>Windows, npm, Cargo et autres méthodes d’installation</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Des binaires précompilés sont fournis pour Linux, macOS et Windows, en x64 et
arm64. Nix, Homebrew sous Linux, Android/Termux (préversion) et les
téléchargements manuels sont décrits dans le
[guide d’installation](../INSTALL.md), et les conteneurs dans
[Docker](../DOCKER.md). Tenez-vous-en à une seule méthode d’installation par
machine ; sinon, deux copies se retrouvent dans le `PATH` et c’est la première
trouvée qui l’emporte.

</details>

## Démarrage rapide

1. Commitez ou mettez de côté (stash) votre travail, puis lancez `codewhale` à
   la racine d’un dépôt git. Par défaut, il y applique les modifications de
   fichiers sans demander et vous montre le diff.
2. Lancez `/provider` (ou appuyez sur `F3`), choisissez un fournisseur, collez
   sa clé API et choisissez un modèle. DeepSeek est le fournisseur par défaut.
   Sans clé, choisissez plutôt un runtime local. Si aucun fournisseur n’est
   configuré et qu’Ollama sert déjà un modèle de chat, Codewhale l’utilise
   automatiquement ; à partir de la 0.10.2, il vous le propose lorsque vous
   envoyez votre premier message.
3. Saisissez une tâche :

```text
Fix the failing tests and explain what changed.
```

Les commandes en lecture seule s’exécutent immédiatement. Pour toute autre
commande shell, Codewhale demande d’abord : `y` l’exécute une fois, `a` autorise
les commandes similaires pour le reste de la conversation, `n` refuse. `/undo`
annule votre dernière demande et ses modifications de fichiers. `/help` liste les commandes et les touches.

Pour les scripts et la CI, `codewhale exec` exécute une tâche sans l’interface.
Par défaut, `--auto` utilise Full Access : les outils s’exécutent sans demander,
les commandes shell n’ont pas de bac à sable Codewhale et le mode de confiance
permet aux outils de fichiers d’écrire en dehors de l’espace de travail.
Ajoutez `--sandbox workspace-write` pour conserver le bac à sable du shell et
les restrictions des outils de fichiers à l’espace de travail, avec le mode de
confiance désactivé :

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Modes et permissions

Une nouvelle installation démarre en mode Work avec la permission Ask.

| | Changer avec | Options |
| --- | --- | --- |
| Mode | `Tab` (quand la saisie est vide) ou `/mode` | **Plan** lit et effectue des recherches, mais ne modifie rien. **Work** modifie des fichiers et exécute des commandes. **Operate** a les mêmes accès que Work, traite votre demande comme un objectif et continue jusqu’à ce que l’objectif soit vérifié ou qu’il ait besoin d’une réponse de votre part. |
| Permission | `Shift+Tab` | **Ask** applique sans demander les modifications de fichiers dans votre dépôt et demande avant les commandes shell qui ne sont pas en lecture seule. **Auto-Review** laisse des règles et un modèle de revue approuver les appels courants et refuse ceux à haut risque ; passez en Ask pour les approuver vous-même. **Full Access** ne demande jamais et désactive le bac à sable de Codewhale ; les règles du dépôt et la politique gérée s’appliquent toujours. |

Le [guide des modes et des permissions](../MODES.md) donne les détails.

## Ce qu’il fait

### Nouveautés de la 0.10.2

- `/pet on` transforme votre terminal en une baleine animée qui montre ce que
  fait l’agent, tandis que la zone de message reste utilisable. Ses
  980 particules écrivent l’étape en cours en anglais, en chinois ou en
  japonais.
- `/workbar terminal` affiche les shells que l’agent exécute, pendant leur
  exécution. Tapez dans l’un d’eux, ou appuyez sur `Ctrl+N` pour ouvrir votre
  propre shell (macOS et Linux).
- `/undo` annule toute votre dernière demande en une seule étape, y compris
  chaque fichier qu’elle a modifié, et `/diff` fonctionne dans les dossiers sans
  Git.
- Quand un plan est prêt, Codewhale demande s’il faut lancer Work avec Ask ou
  Auto-Review, ou continuer à planifier.
- `/color` donne à chaque conversation sa propre couleur de zone de saisie,
  enregistrée avec la session.
- `web_search` peut utiliser la nouvelle Search API de DeepSeek : définissez
  `[search] provider = "deepseek"` avec une clé DeepSeek.
- En dehors de Full Access, un `curl` ou un `wget` qui enverrait des
  informations d’identification ne s’exécute jamais sans votre approbation, et
  une commande qui dissimule de l’Unicode invisible est refusée. `write` ne
  remplace plus un fichier que l’agent n’a pas lu, ni un fichier modifié après
  qu’il l’a lu.

### Modèles

Plus de 40 fournisseurs sont intégrés, dont Anthropic, OpenAI, DeepSeek,
Google, Mistral, Moonshot, OpenRouter et xAI. Tout point d’accès compatible
OpenAI fonctionne, tout comme les modèles locaux via Ollama, vLLM ou SGLang.
Le modèle par défaut est `deepseek-flash` de DeepSeek, que DeepSeek a fait
passer à DeepSeek-V4.1-Flash-1017 le 2026-10-17 ; la 0.10.1 l’exécute sans
mise à jour ([détails](../PROVIDERS.md#deepseek-v41-flash)).

`/model` change de modèle entre deux tours, et `/model auto` en choisit un pour
chaque tour (`/router` permet de régler comment). `/turn inspect` montre quel
modèle un tour a utilisé, ce qu’il a coûté et combien de temps il a pris.
`codewhale auth chatgpt` vous permet d’utiliser un abonnement ChatGPT au lieu
d’une clé API (expérimental). Voir [Fournisseurs](../PROVIDERS.md).

### Reçus et annulation

`/receipts`, ou `codewhale receipts --last` après avoir quitté, liste les
fichiers modifiés, les commandes exécutées, les appels web et MCP, les agents
lancés, les approbations et qui les a données, ainsi que les échecs. `/undo` annule votre dernière demande et les fichiers qu’elle a modifiés, en une seule étape et sans mode de confiance. `/restore`
ramène tout l’espace de travail à un instantané antérieur ; comme cela écrase
les modifications ultérieures, il faut `/trust on` ou Full Access. Voir
[Reçus](../RECEIPTS.md).

Les exécutions d’exec qui proposent des outils sont enregistrées dans tous les
formats de sortie ; `codewhale receipts --last` fonctionne donc après
`exec --auto`, y compris avec `--json`. Les exécutions qui se limitent à une
réponse en texte ou avec `--json` ne sont enregistrées que lors d’une reprise
ou lorsque la compaction de la conversation commence ; utilisez
`--output-format stream-json` pour toujours les enregistrer pour les reçus ou
pour `exec --continue`.

### Travail de longue durée

`/goal` fixe un objectif qui se maintient d’un tour à l’autre. Codewhale peut
répartir le travail entre des [sous-agents](../SUBAGENTS.md), faire tourner
une [flotte](../FLEET.md) d’agents ou suivre un
[workflow](../WORKFLOW_AUTHORING.md) versionné dans le dépôt.

### Venir d’un autre agent

Codewhale lit `AGENTS.md` de lui-même. Les fichiers d’instructions écrits pour
d’autres outils, comme `CLAUDE.md`, `.cursorrules` ou
`.github/copilot-instructions.md`, ne sont chargés que si vous l’activez
explicitement avec `project_instruction_imports = ["claude"]` (ou `"cursor"`,
`"cline"`, `"windsurf"`, `"copilot"`, `"all"`) dans votre configuration ; voir
[config.example.toml](../../config.example.toml). Codewhale vous avertit lorsqu’il en
trouve un qu’il n’a pas chargé.

`/import-claude` prépare une importation de vos paramètres Claude Code et de vos
serveurs MCP, pour que vous la vérifiiez. `/plugin install` accepte les plugins
Claude Code contenant des skills, des commandes, des profils d’agent et des
serveurs MCP, et refuse ceux qui ont besoin de hooks ou de serveurs LSP
([détails](../CLAUDE_PLUGIN_COMPAT.md)).

### Extensions

Connectez des [serveurs MCP](../MCP.md), ajoutez des [skills](../SKILLS.md)
et des [plugins](../PLUGINS.md), et exécutez des [hooks](../HOOKS.md) sur
les événements de session et d’outil. Le mode code (expérimental, activé par
défaut) permet au modèle d’écrire un seul programme `execute_tools` qui appelle
plusieurs outils MCP et de plugins ; chaque appel reste soumis à vos règles
d’approbation et conserve son reçu. Le plugin Computer Use installé séparément
(expérimental, macOS uniquement) peut observer et utiliser d’autres
applications. Il reste désactivé tant que vous n’avez pas examiné ses accès et
ne l’avez pas activé ([guide](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Façons de l’exécuter

| Commande | Ce qu’elle fait |
| --- | --- |
| `codewhale` | L’application interactive dans le terminal |
| `codewhale exec "…"` | Une tâche sans l’interface. Sans option, il répond sans utiliser d’outils ; `--auto` active les outils avec Full Access par défaut, `--sandbox workspace-write` les restreint, et `--output-format stream-json` diffuse les événements en JSON |
| `codewhale web` | Une interface web locale sur `127.0.0.1` ([guide](../WEB.md)) |
| `codewhale review --pr N` | Une revue consultative d’une pull request GitHub. La [configuration du bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) génère un workflow épinglé pour les revues automatiques et `@codewhale review`, avec votre propre clé de modèle et une identité App facultative |
| `codewhale app-server --http` | La [Runtime API](../RUNTIME_API.md), une API HTTP locale pour les fils, les événements, les approbations et les modifications de fichiers de chaque tour |
| `codewhale serve --acp` | L’Agent Client Protocol sur stdio, pour des éditeurs comme Zed |

L’application de terminal, `exec`, `web` et la Runtime API partagent un seul
moteur ([architecture](../ARCHITECTURE.md)). Une
[extension VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
maintenue par la communauté s’y connecte depuis la barre latérale
([source](https://github.com/HengQuWorld/CodeWhale-VSCode)). Une application de
bureau native reposant sur la Runtime API est en cours de développement ; elle
n’est pas encore disponible en téléchargement public.

## Coût et comptes

Codewhale est gratuit et sous licence MIT. Avec votre propre clé API, vous payez
directement le fournisseur ; les modèles locaux ne nécessitent aucune clé. Le
pied de page et `/cost` indiquent ce qu’une session a coûté lorsque le prix est
connu ; un prix inconnu est affiché comme inconnu, jamais comme gratuit.

Aucun compte n’est nécessaire. Les clés que vous ajoutez avec `/provider`
restent sur votre machine.

## Sécurité et confidentialité

En Ask et en Auto-Review, les commandes s’exécutent dans un bac à sable du
système d’exploitation lorsque le système en propose un : Seatbelt sur macOS, et
bubblewrap sous Linux lorsqu’il est installé. Windows n’a pas encore de bac à
sable du système, et Full Access désactive le bac à sable. Dans le bac à sable,
les commandes shell ne peuvent pas lire les emplacements qui stockent des
informations d’identification, comme `~/.ssh`, `~/.aws` et les fichiers `.env` ;
à partir de la 0.10.2, la même règle couvre les sessions de terminal et
davantage d’outils de fichiers. `/preview-request` affiche un aperçu expurgé de
la prochaine requête sans l’envoyer.

La télémétrie est activée par défaut. Elle envoie des compteurs agrégés (version
et plateforme, durée et résultat des sessions, fournisseurs intégrés que vous
avez utilisés, compteurs de fonctionnalités et d’erreurs) avec un identifiant
d’installation aléatoire renouvelé tous les 90 jours, et jamais de
conversations, de code, de prompts, de noms de modèles, de noms de fichiers ou
de dépôts, ni d’informations d’identification.
`codewhale config set telemetry false` la désactive. Au démarrage, Codewhale
vérifie aussi sur GitHub si une nouvelle version est disponible ; définissez
`CODEWHALE_NO_UPDATE_CHECK=1` pour l’éviter. Pour en savoir plus :
[ordre d’autorisation](../AUTHORIZATION_ORDER.md),
[bac à sable](../SANDBOX.md) et [télémétrie](../TELEMETRY.md).

## Documentation

| Pour commencer | Référence |
| --- | --- |
| [Installation](../INSTALL.md) | [Configuration](../CONFIGURATION.md) |
| [Fournisseurs et modèles locaux](../PROVIDERS.md) | [Architecture](../ARCHITECTURE.md) |
| [Modes et permissions](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [Raccourcis clavier](../KEYBINDINGS.md) | [Création de plugins](../PLUGIN_AUTHORING.md) |
| [Revue de PR GitHub](../GITHUB_ACTION.md) | [Toute la documentation](../README.md) |

## Communauté

S’il manque un fournisseur ou si un workflow est peu pratique,
[ouvrez une issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) ou
[envoyez une pull request](../../CONTRIBUTING.md). Les personnes qui contribuent
restent créditées pour le travail intégré, et
l’[organisation du dépôt](../../CONTRIBUTING.md#project-structure) montre où se trouve
chaque chose.

Discutez sur [Discord](https://discord.gg/37gfS3ksug), ou ajoutez le mainteneur,
Hunter, sur WeChat (`hunterbown`) et demandez à rejoindre le groupe Whale
Brothers.

## Historique et licence

Codewhale a commencé sous le nom de `deepseek-tui` et lit toujours la
configuration et les sessions de ce projet. Il est maintenu de manière
indépendante et n’est affilié ni à DeepSeek ni à aucun autre fournisseur de
modèles. Merci à [toutes les personnes qui ont contribué](../CONTRIBUTORS.md).

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). Le code adapté d’autres projets open source est répertorié dans
les [mentions tierces](../THIRD_PARTY_NOTICES.md).
