<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**L’agent de code open source qui fonctionne avec n’importe quel modèle.**

Codewhale lit votre projet, modifie des fichiers, exécute des commandes et vérifie son propre
travail — dans votre terminal, avec un modèle hébergé ou local de votre choix.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Site web](https://codewhale.net) · [Documentation](docs/README.md) · [Journal des modifications](CHANGELOG.md) · [Contribuer](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Une session de terminal Codewhale" width="760">

<sub>Capture réelle du terminal Codewhale dans une session de démonstration locale configurée.</sub>

</div>

## Installation

macOS et Linux :

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

L’installeur télécharge des binaires dont la somme de contrôle est vérifiée dans `~/.local/bin`. Si
`codewhale` répond ensuite « command not found », exécutez l’unique ligne PATH que l’installeur
affiche, ou consultez [Ajouter au PATH](docs/INSTALL.md#put-it-on-your-path).
Mettez à jour à tout moment avec `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo et autres méthodes</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew sous Linux, Android/Termux, les téléchargements manuels avec vérification
des sommes de contrôle et le miroir CNB facultatif sont décrits dans le
[guide d’installation](docs/INSTALL.md). Choisissez une seule méthode : plusieurs installations
sur une même machine finissent par se disputer le `PATH`.

</details>

## Démarrage rapide

**Les clés API de vos fournisseurs, au même endroit.** [Créez un compte Codewhale](https://app.codewhale.net/register)
ou lancez `codewhale login` pour vous connecter. Enregistrez et mettez à jour vos clés API dans votre compte,
puis utilisez-les via le routage des modèles Codewhale sur vos appareils connectés à ce compte.
[Configurer les clés du compte](docs/CONFIGURATION.md#account-provider-keys).
La connexion ne transfère pas les clés locales existantes ; l’utilisation locale reste possible sans compte.

1. **Ouvrez votre projet.** Lancez `codewhale` dans le dossier sur lequel vous voulez travailler.
2. **Connectez un modèle.** Lancez `/provider` (ou appuyez sur `F3`) pour ajouter une clé
   hébergée ou choisir un runtime local. Si Ollama est déjà en cours d’exécution avec un modèle
   de chat, Codewhale bascule dessus automatiquement. Utilisez `/model` pour changer de modèle.
3. **Donnez-lui une tâche concrète.**

```text
Fix the failing tests and explain what changed.
```

La même tâche s’exécute sans interface depuis un script ou un job CI :

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Lancez `/help` pour les commandes et les raccourcis clavier.

## Façons de l’exécuter

Chaque client pilote le même Runtime Codewhale local ; les sessions, les outils et les
permissions se comportent donc de la même façon partout.

| Commande | Ce qu’elle fait |
| --- | --- |
| `codewhale` | L’interface interactive dans le terminal |
| `codewhale exec "…"` | Un tour sans interface depuis un script ou la CI ; `--auto` autorise les outils de fichiers et de shell, `--output-format stream-json` diffuse du JSON en flux |
| `codewhale web` | Le [client de navigateur local](docs/WEB.md) fourni, sur `127.0.0.1` |
| `codewhale review --pr N` | Une [revue de pull request](docs/GITHUB_ACTION.md) à titre consultatif ; la publication est facultative |
| Runtime API | Une [API HTTP locale](docs/RUNTIME_API.md) pour les fils, les événements et les approbations |

Une application de bureau native (GPUI) est en cours de développement comme client du produit
pour les utilisateurs connectés ; consultez la
[page produit](https://codewhale.net/en/product) pour sa disponibilité. L’[extension VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
maintenue par la communauté se connecte au même Runtime depuis une barre latérale ([source](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Ce qu’il fait

- **N’importe quel modèle, sans enfermement.** Plus de 40 routes de fournisseurs intégrées —
  Anthropic, DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI et d’autres — ainsi que
  tout point d’accès compatible OpenAI et des modèles locaux via Ollama, vLLM ou SGLang.
  [Fournisseurs](docs/PROVIDERS.md)
- **Vous gardez le contrôle.** Le mode Plan explore sans rien modifier ; Work et Operate
  apportent des modifications. Les postures d’approbation décident quand un appel d’outil
  nécessite votre accord, `/undo` et `/restore` récupèrent les modifications de l’espace de
  travail, et `/receipts` liste chaque fichier, commande et approbation d’une session.
  [Modes](docs/MODES.md) · [Reçus](docs/RECEIPTS.md)
- **Conçu pour les longues tâches.** Fixez un `/goal` durable, déléguez un travail délimité à des
  [sous-agents](docs/SUBAGENTS.md), lancez des [équipes d’agents](docs/FLEET.md) supervisées avec
  une vérification avant dépense, ou scriptez-les sous forme de
  [workflows](docs/WORKFLOW_AUTHORING.md) versionnés.
- **Prolongez ce que vous utilisez déjà.** Connectez des [serveurs MCP](docs/MCP.md), installez
  des [skills](docs/SKILLS.md) et des [plugins](docs/PLUGINS.md), exécutez des
  [hooks](docs/HOOKS.md) sur les événements de session et d’outil, et chargez les
  [plugins Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) existants.
- **Computer Use.** Un plugin inclus ajoute des outils pour observer et utiliser d’autres
  applications. Examinez ses accès et activez-le avant de l’utiliser.
  [Guide](crates/tui/plugins/computer-use/README.md)

## Modes et permissions

| | Choisir avec | Options |
| --- | --- | --- |
| **Mode** — ce que fait l’agent | `Tab` ou `/mode` | Plan (explorer, sans modification) · Work (modifier et exécuter) · Operate (mener un objectif par des étapes planifiées et vérifiées) |
| **Posture** — quand il demande d’abord | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access respecte toujours les limites impératives des politiques. Le
[guide des modes et des permissions](docs/MODES.md) explique chaque option.

## Sécurité

Codewhale s’exécute sur votre machine avec les accès que vous lui accordez. Les postures
d’approbation et les règles du dépôt limitent ce que l’agent peut faire, et les commandes
s’exécutent dans un bac à sable du système d’exploitation lorsqu’il est pris en charge (Seatbelt
sur macOS ; bubblewrap sous Linux lorsqu’il est installé et fonctionnel). `/preview-request` affiche la requête exacte,
expurgée, avant tout envoi. Le prix d’un modèle inconnu reste inconnu au lieu d’être présenté
comme gratuit.

Consultez l’[ordre d’autorisation](docs/AUTHORIZATION_ORDER.md),
le [bac à sable](docs/SANDBOX.md) et la [télémétrie](docs/TELEMETRY.md) — les compteurs
d’utilisation sont activés par défaut et `codewhale config set telemetry false` les désactive.

## Documentation

| Pour commencer | Pour aller plus loin |
| --- | --- |
| [Installation](docs/INSTALL.md) | [Configuration](docs/CONFIGURATION.md) |
| [Fournisseurs et modèles locaux](docs/PROVIDERS.md) | [Architecture](docs/ARCHITECTURE.md) |
| [Modes et permissions](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Raccourcis clavier](docs/KEYBINDINGS.md) | [Création de plugins](docs/PLUGIN_AUTHORING.md) |
| [Revue de PR GitHub](docs/GITHUB_ACTION.md) | [Toute la documentation](docs/README.md) |

## Communauté

Les signalements de bugs, les idées de fonctionnalités et les pull requests sont les bienvenus,
que vous utilisiez Codewhale depuis des mois ou que vous l’essayiez pour la première fois.
S’il manque un fournisseur ou si un workflow est peu pratique,
[ouvrez une issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) ou
[envoyez une pull request](CONTRIBUTING.md). Les premières contributions sont les bienvenues, et
les personnes qui contribuent restent créditées pour le travail intégré.
L’[organisation du dépôt](CONTRIBUTING.md#project-structure) est un bon point de départ.

Rejoignez le [Discord](https://discord.gg/37gfS3ksug), ou ajoutez Hunter sur WeChat
(`hunterbown`) et demandez à rejoindre le groupe Whale Brothers.

## Historique et licence

Codewhale a commencé sous le nom de `deepseek-tui` et lit toujours la configuration et les
sessions de ce projet. Il est désormais indépendant de tout fournisseur, maintenu de manière
autonome, et n’est affilié à aucun fournisseur de modèles. Merci à
[toutes les personnes qui contribuent](docs/CONTRIBUTORS.md) et aux communautés open source qui
l’ont aidé à grandir.

[MIT](LICENSE). Les parties adaptées d’autres projets open source sont indiquées dans les
[mentions tierces](docs/THIRD_PARTY_NOTICES.md).
