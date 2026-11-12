<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>Le modèle peut être à n’importe qui. L’agent devrait être à vous.</h1>

Codewhale est un agent de code open source pour votre terminal, écrit en Rust. Il fonctionne avec n’importe quel modèle, peut répartir une tâche entre plusieurs agents, et chaque ligne de son code est à lire par vous.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Cette commande installe la dernière version sur macOS ou Linux, et elle ne demande aucun compte. [Windows, npm et Cargo](#installation) sont plus bas.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Site web](https://codewhale.net) · [Documentation](../README.md) · [Journal des modifications](../../CHANGELOG.md) · [Contribuer](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Cette traduction est un brouillon automatique qui n'a pas encore été relu par un locuteur natif.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale dans un terminal après une tâche de démonstration. Il indique avoir ajouté une option --json à export.py, que les 3 tests locaux passent et qu’une revue déléguée n’a trouvé aucun problème. La liste de tâches en dessous montre trois étapes terminées, et la ligne d’état affiche Full Access et le fournisseur local website-demo.">

<sub>Une version de développement de la 0.10.3, qui n’est pas encore publiée, avec un modèle de test scripté, en Full Access dans un espace de travail jetable. La modification, trois tests et une revue par un sous-agent ont réellement tourné.</sub>

</div>

## Installation

La commande ci-dessus place `codewhale`, vérifié par somme de contrôle, et son alias court `codew` dans `~/.local/bin`. Si votre shell affiche ensuite `command not found`, suivez la note sur le PATH qu’affiche l’installateur ([détails](../INSTALL.md#put-it-on-your-path)). `codewhale update` le met à jour.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Des binaires précompilés existent pour Linux, macOS et Windows sur x64 et arm64. Scoop, Nix, Homebrew sous Linux, Android et Termux (aperçu) et les téléchargements manuels sont dans le [guide d’installation](../INSTALL.md), et les conteneurs dans [Docker](../DOCKER.md). Utilisez une seule méthode d’installation par machine ; sinon deux copies se retrouvent dans votre `PATH` et la première trouvée l’emporte.

## Démarrage rapide

1. Validez ou mettez de côté votre travail, puis lancez `codewhale` à la racine d’un dépôt git. Par défaut, il applique les modifications de fichiers du dépôt sans demander et demande avant chaque commande shell qui ne fait pas que lire.
2. Lancez `/provider` (ou appuyez sur `F3`), choisissez un fournisseur, collez sa clé d’API et choisissez un modèle. DeepSeek est le fournisseur par défaut. Au premier lancement sans clé, Codewhale demande un fournisseur quand vous envoyez votre premier message, et propose un modèle local quand un serveur Ollama local répond.
3. Tapez une tâche :

```text
Fix the failing tests and explain what changed.
```

Les commandes en lecture seule s’exécutent tout de suite. Pour toute autre commande shell, Codewhale demande d’abord : `y` l’exécute une fois, `a` autorise les commandes semblables pour le reste de la conversation, et `n` refuse. `/undo` annule votre dernière demande et ses modifications de fichiers, et `/help` liste les commandes et les touches.

Pour les scripts et la CI, `codewhale exec` exécute une tâche sans interface. Lisez [exécutions sans interface](../INSTALL.md#headless-scripts-ci) avant de lui donner des outils avec `--auto`, car `--auto` utilise Full Access tant que vous n’ajoutez pas de bac à sable.

## Ce qu’il fait

Donnez une tâche à Codewhale : il lit le dépôt, modifie des fichiers et exécute des commandes jusqu’à ce que la tâche soit terminée ou qu’il ait besoin de vous. Les [notes de la dernière version](https://github.com/codewhale-hq/Codewhale/releases/latest) listent les changements les plus récents, et le [journal des modifications](../../CHANGELOG.md) couvre chaque version.

### N’importe quel modèle

Plus de 40 fournisseurs sont intégrés, dont Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter et xAI. Tout point d’accès compatible OpenAI fonctionne, tout comme les modèles locaux via Ollama, vLLM ou SGLang. Le modèle par défaut est `deepseek-flash` de DeepSeek, qui sert désormais DeepSeek V4.1 Flash ([détails](../PROVIDERS.md#deepseek-v41-flash)).

`/model` change de modèle entre deux tours, et `/model auto` en choisit un à chaque tour (`/router` règle comment). `/turn inspect` montre quel modèle un tour a utilisé, ce qu’il a coûté et combien de temps il a pris. Voir [Fournisseurs](../PROVIDERS.md).

### Reçus et annulation

`/receipts`, ou `codewhale receipts --last` après avoir quitté, liste les fichiers modifiés par une session, les commandes lancées, ses appels web et MCP, les agents démarrés, chaque approbation et qui l’a donnée, et ce qui a échoué. `/undo` annule votre dernière demande et les fichiers qu’elle a modifiés en une seule étape. `/restore` ramène tout l’espace de travail à un instantané antérieur ; comme cela écrase les modifications suivantes, il faut `/trust on` ou Full Access. Voir [Reçus](../RECEIPTS.md).

### Plusieurs agents sur une tâche

`/goal` fixe un objectif qui se poursuit d’un tour à l’autre. Codewhale peut répartir le travail entre des [sous-agents](../SUBAGENTS.md), faire tourner une [Fleet](../FLEET.md) d’agents avec leurs propres rôles et modèles, ou suivre un [workflow](../WORKFLOW_AUTHORING.md) versionné dans le dépôt.

### Extensions

Connectez des [serveurs MCP](../MCP.md), ajoutez des [skills](../SKILLS.md) et des [plugins](../PLUGINS.md), et lancez des [hooks](../HOOKS.md) sur les événements de session et d’outil. Le mode code (expérimental, activé par défaut) permet au modèle d’écrire un programme `execute_tools` qui appelle plusieurs outils MCP et de plugins ; chaque appel respecte toujours vos règles d’approbation et garde son reçu. Le plugin Computer Use fourni avec v0.10.2 (expérimental, macOS uniquement) peut observer et piloter d’autres applications ; il reste désactivé tant que vous n’avez pas vérifié ses accès et ne l’avez pas activé ([guide](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). À venir dans 0.10.3 : le plugin est publié [séparément](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Façons de le lancer

| Commande | Ce qu’elle fait |
| --- | --- |
| `codewhale` | L’application de terminal interactive |
| `codewhale exec "…"` | Une tâche sans interface, pour les scripts et la CI ([exécutions sans interface](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Un client web local sur `127.0.0.1` ([guide](../WEB.md)) |
| `codewhale review --pr N` | Une revue consultative d’une pull request GitHub ; la [configuration du bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) la lance avec votre propre clé de modèle |
| `codewhale app-server --http` | La [Runtime API](../RUNTIME_API.md), une API HTTP locale pour les fils, les événements, les approbations et les modifications de fichiers de chaque tour |
| `codewhale serve --acp` | L’Agent Client Protocol sur stdio, pour des éditeurs comme Zed |

L’application de terminal, `exec`, `web` et la Runtime API partagent un même moteur ([architecture](../ARCHITECTURE.md)). Une [extension VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) maintenue par la communauté s’y connecte depuis la barre latérale ([source](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Comment il reste sûr

Deux réglages disent ce que Codewhale peut faire sans demander. Quand la saisie est vide, `Tab` change de mode : **Plan** lit et cherche mais ne change rien, **Work** modifie des fichiers et exécute des commandes, et **Operate** a les mêmes accès que Work et continue jusqu’à ce que l’objectif soit vérifié ou qu’il ait besoin de vous. `Shift+Tab` change la permission. **Ask**, par défaut, applique les modifications de fichiers de votre dépôt sans demander et demande avant les commandes shell qui ne font pas que lire. **Auto-Review** laisse des règles et un modèle relecteur approuver les appels courants et refuse ceux à haut risque ; passez en Ask pour les approuver vous-même. **Full Access** ne demande jamais et désactive le bac à sable de Codewhale, mais les règles du dépôt et la politique gérée s’appliquent toujours. Le [guide de v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md) donne les détails.

À venir dans 0.10.3 : `Shift+Tab` devient un seul sélecteur qui passe par Ask, Accept Edits, Plan, Auto-Review et Full Access, `Tab` ne change plus de mode et Operate est retiré. Ask demandera aussi avant chaque modification de fichier, et le nouveau réglage par défaut, Accept Edits, appliquera les modifications dans le dépôt git tout en demandant avant les commandes. Le [guide actuel](../MODES.md) le décrit.

En Ask et en Auto-Review, les commandes tournent dans un bac à sable du système quand il en existe un : Seatbelt sur macOS, et bubblewrap sur Linux quand il est installé. Windows n’a pas encore de bac à sable système, et Full Access désactive le bac à sable. Dans le bac à sable, les commandes shell et les sessions de terminal ne peuvent pas lire les magasins d’identifiants comme `~/.ssh`, `~/.aws` et les fichiers `.env`. `/preview-request` affiche un aperçu expurgé de la prochaine requête sans l’envoyer.

Vous n’avez besoin d’aucun compte, et les clés ajoutées avec `/provider` restent sur votre machine. Avec votre propre clé d’API, vous payez directement le fournisseur, et les modèles locaux n’ont pas besoin de clé. Le pied de page et `/cost` indiquent ce qu’une session a coûté quand le prix est connu ; un prix inconnu est affiché comme inconnu, jamais comme gratuit.

La télémétrie est activée par défaut. Elle envoie des compteurs agrégés (version et plateforme, durée et issue des sessions, fournisseurs intégrés utilisés, compteurs de fonctionnalités et d’erreurs) avec un identifiant d’installation aléatoire renouvelé tous les 90 jours, et jamais de conversations, de code, de prompts, de noms de modèles, de noms de fichiers ou de dépôts, ni d’identifiants. Cette commande la désactive :

```bash
codewhale config set telemetry false
```

Au démarrage, Codewhale vérifie aussi sur GitHub s’il existe une nouvelle version ; définissez `CODEWHALE_NO_UPDATE_CHECK=1` pour l’éviter. Plus d’informations dans [l’ordre d’autorisation](../AUTHORIZATION_ORDER.md), [le bac à sable](../SANDBOX.md) et [la télémétrie](../TELEMETRY.md).

## Où il va

Codewhale devient un seul environnement qui couvre votre terminal, une application de bureau, le navigateur et des ordinateurs dans le cloud, le tout animé par un seul moteur. Chaque partie ci-dessous indique où elle en est aujourd’hui.

| Partie | État | Aujourd’hui |
| --- | --- | --- |
| Application de terminal | Publié | Installez-la sur macOS, Linux ou Windows et lancez `codewhale` dans n’importe quel dépôt. |
| Client de navigateur local | Publié | Lancez `codewhale web` pour lire la conversation, envoyer une tâche et répondre aux demandes d’approbation dans votre navigateur ; il est fourni avec le terminal et ne demande aucun compte. |
| Plusieurs agents sur une tâche | Publié | Les sous-agents, les Fleets et les workflows permettent à plusieurs agents de partager une tâche, chacun avec son propre rôle. |
| Application de bureau | En développement, sans téléchargement | Nous construisons une application de bureau sur le même moteur, et elle n’a pas encore de téléchargement public. |
| Ordinateurs dans le cloud | Sur invitation | Les ordinateurs dans le cloud sont sur invitation, et nous les construisons pour qu’une session puisse continuer à travailler sur une machine dans le cloud. |
| Une session à travers tout | En construction | Nous construisons une session qui vous suit du terminal au bureau, au navigateur et au cloud, et reste à vous tout au long du chemin. |

La [feuille de route](../ROADMAP.md) indique ce que couvre chaque version et où vous pouvez aider.

## Venir d’un autre agent

Codewhale lit `AGENTS.md` de lui-même. Les fichiers d’instructions écrits pour d’autres outils, comme `CLAUDE.md`, `.cursorrules` ou `.github/copilot-instructions.md`, ne sont chargés que si vous les activez avec `project_instruction_imports = ["claude"]` (ou `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) dans votre configuration ; voir [config.example.toml](../../config.example.toml). Codewhale vous avertit quand il en trouve un qu’il n’a pas chargé.

`/import-claude` prépare l’import de vos réglages Claude Code et de vos serveurs MCP pour que vous les examiniez. `/plugin install` accepte les plugins Claude Code qui contiennent des skills, des commandes, des profils d’agent et des serveurs MCP, et refuse ceux qui ont besoin de hooks ou de serveurs LSP ([détails](../CLAUDE_PLUGIN_COMPAT.md)).

## Documentation

Ces guides suivent la branche principale et peuvent décrire des changements qui ne sont pas encore dans la dernière version.

| Pour commencer | Référence |
| --- | --- |
| [Guide de l’utilisateur](../GUIDE.md) | [Configuration](../CONFIGURATION.md) |
| [Installation](../INSTALL.md) | [Architecture](../ARCHITECTURE.md) |
| [Fournisseurs et modèles locaux](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modes et permissions](../MODES.md) | [Écrire un plugin](../PLUGIN_AUTHORING.md) |
| [Raccourcis clavier](../KEYBINDINGS.md) | [Reçus](../RECEIPTS.md) |
| [Revue de pull requests GitHub](../GITHUB_ACTION.md) | [Toute la documentation](../README.md) |

## Contribuer

Codewhale est construit au grand jour par les personnes qui l’utilisent, dans de nombreuses langues.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Le mur des contributeurs de Codewhale : un avatar pour chaque personne dont le code, les commits coécrits, les pull requests reprises ou les signalements crédités font partie de Codewhale, avec le total en haut."></a>

Le [mur des contributeurs](../CONTRIBUTORS.md) compte toutes les personnes dont le code, les commits coécrits, les pull requests reprises ou les signalements crédités font partie de Codewhale, y compris celles que la liste de contributeurs de GitHub oublie. `scripts/contributor-wall.py` régénère l’image et la liste à partir de l’historique git et de GitHub.

Tous les retours sont bienvenus : un bug, une idée, ou quelque chose qui avait l’air ou semblait faux. [Ouvrez une issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) pour chacun d’eux ; une issue est aussi le meilleur endroit pour lancer une nouvelle fonctionnalité. Les pull requests sont bienvenues pour les corrections, les traductions, les fournisseurs et le travail prévu. Les questions vont dans les [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). Le [guide de contribution](../../CONTRIBUTING.md) montre où va chaque type d’aide, comment votre travail est intégré à votre nom, et [où se trouve chaque chose](../../CONTRIBUTING.md#project-structure).

Discutez sur [Discord](https://discord.gg/37gfS3ksug), ou ajoutez le mainteneur, Hunter, sur WeChat (`hunterbown`) et demandez à rejoindre le groupe Whale Brothers. Tout le monde est bienvenu.

## Historique et licence

Codewhale a commencé sous le nom `deepseek-tui` et lit toujours la configuration et les sessions de ce projet. Il est maintenu de façon indépendante et n’est affilié ni à DeepSeek ni à aucun autre fournisseur de modèles.

[MIT](../../LICENSE). Le code adapté d’autres projets open source est listé dans les mentions tierces pour les [sources intégrées](../../THIRD_PARTY_NOTICES.md) et les [conceptions adaptées](../THIRD_PARTY_NOTICES.md).
