<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**L’agente di programmazione open source che funziona con qualsiasi modello.**

Codewhale legge il tuo progetto, modifica i file, esegue comandi e controlla il proprio
lavoro — nel tuo terminale, con un modello ospitato o locale a tua scelta.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Sito web](https://codewhale.net) · [Documentazione](docs/README.md) · [Changelog](CHANGELOG.md) · [Contribuire](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Una sessione di terminale di Codewhale" width="760">

<sub>Acquisizione reale del terminale Codewhale da una sessione demo locale configurata.</sub>

</div>

## Installazione

macOS e Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Il programma di installazione scarica in `~/.local/bin` binari con checksum verificato. Se
`codewhale` risponde poi «command not found», esegui l’unica riga PATH che l’installer
stampa, oppure consulta [Aggiungere al PATH](docs/INSTALL.md#put-it-on-your-path).
Aggiorna in qualsiasi momento con `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo e altre vie</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew su Linux, Android/Termux, download manuali con verifica del checksum
e il mirror CNB opzionale sono descritti nella
[guida all’installazione](docs/INSTALL.md). Scegli una sola via: più installazioni
sulla stessa macchina finiscono per contendersi il `PATH`.

</details>

## Avvio rapido

**Le chiavi API dei tuoi provider, in un unico posto.** [Crea un account Codewhale](https://app.codewhale.net/register)
oppure esegui `codewhale login` per accedere. Salva e aggiorna le chiavi API nel tuo account,
poi usale tramite l’instradamento dei modelli di Codewhale sui dispositivi su cui hai effettuato l’accesso.
[Configura le chiavi dell’account](docs/CONFIGURATION.md#account-provider-keys).
L’accesso non carica le chiavi locali esistenti; puoi continuare a usare Codewhale in locale senza un account.

1. **Apri il tuo progetto.** Esegui `codewhale` nella cartella su cui vuoi lavorare.
2. **Collega un modello.** Esegui `/provider` (o premi `F3`) per aggiungere una chiave
   ospitata o scegliere un runtime locale. Se Ollama è già in esecuzione con un modello di
   chat, Codewhale passa a quello da solo. Usa `/model` per cambiare modello.
3. **Dagli un compito concreto.**

```text
Fix the failing tests and explain what changed.
```

Lo stesso compito gira senza interfaccia da uno script o da un job di CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Esegui `/help` per i comandi e le scorciatoie da tastiera.

## Modi di eseguirlo

Ogni client pilota lo stesso Runtime Codewhale locale, quindi sessioni, strumenti e
permessi si comportano allo stesso modo ovunque.

| Comando | Cosa fa |
| --- | --- |
| `codewhale` | L’interfaccia interattiva nel terminale |
| `codewhale exec "…"` | Un turno senza interfaccia da uno script o dalla CI; `--auto` consente gli strumenti per file e shell, `--output-format stream-json` trasmette JSON in streaming |
| `codewhale web` | Il [client browser locale](docs/WEB.md) incluso, su `127.0.0.1` |
| `codewhale review --pr N` | Una [revisione di pull request](docs/GITHUB_ACTION.md) a scopo consultivo; la pubblicazione è facoltativa |
| Runtime API | Una [API HTTP locale](docs/RUNTIME_API.md) per thread, eventi e approvazioni |

Un’app desktop nativa (GPUI) è in sviluppo come client del prodotto per gli utenti con
account; consulta la
[pagina del prodotto](https://codewhale.net/en/product) per la disponibilità. L’[estensione VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
mantenuta dalla comunità si connette allo stesso Runtime da una barra laterale ([sorgente](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Cosa fa

- **Qualsiasi modello, nessun vincolo.** Oltre 40 percorsi di provider integrati — Anthropic,
  DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI e altri — più
  qualsiasi endpoint compatibile con OpenAI e modelli locali tramite Ollama, vLLM o SGLang.
  [Provider](docs/PROVIDERS.md)
- **Resti tu al controllo.** La modalità Plan esplora senza modificare nulla; Work
  e Operate apportano modifiche. Le posture di approvazione decidono quando una chiamata a uno
  strumento richiede il tuo consenso, `/undo` e `/restore` ripristinano le modifiche
  all’area di lavoro, e `/receipts` elenca ogni file, comando e approvazione di una sessione.
  [Modalità](docs/MODES.md) · [Ricevute](docs/RECEIPTS.md)
- **Pensato per i lavori lunghi.** Imposta un `/goal` duraturo, delega lavoro circoscritto a
  [sub-agenti](docs/SUBAGENTS.md), avvia [team di agenti](docs/FLEET.md) supervisionati
  con un controllo prima della spesa, oppure scrivili come
  [workflow](docs/WORKFLOW_AUTHORING.md) versionati nel repository.
- **Estendi ciò che già usi.** Collega [server MCP](docs/MCP.md), installa
  [skill](docs/SKILLS.md) e [plugin](docs/PLUGINS.md), esegui
  [hook](docs/HOOKS.md) sugli eventi di sessione e di strumento e carica i
  [plugin di Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) esistenti.
- **Computer Use.** Un plugin incluso aggiunge strumenti per osservare e usare altre
  applicazioni. Controlla i suoi accessi e abilitalo prima dell’uso.
  [Guida](crates/tui/plugins/computer-use/README.md)

## Modalità e permessi

| | Si sceglie con | Opzioni |
| --- | --- | --- |
| **Modalità** — cosa sta facendo l’agente | `Tab` o `/mode` | Plan (esplorare, nessuna modifica) · Work (modificare ed eseguire) · Operate (portare avanti un obiettivo con passaggi pianificati e verificati) |
| **Postura** — quando chiede prima | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access rispetta comunque i limiti rigidi delle policy. La
[guida a modalità e permessi](docs/MODES.md) spiega ogni opzione.

## Sicurezza

Codewhale gira sulla tua macchina con gli accessi che gli concedi. Le posture di approvazione
e le regole del repository limitano ciò che l’agente può fare, e i comandi vengono eseguiti in
una sandbox del sistema operativo dove supportata (Seatbelt su macOS; bubblewrap su Linux quando è
installato e funzionante). `/preview-request` mostra la richiesta esatta, con i dati oscurati, prima che
venga inviato qualcosa. I prezzi dei modelli sconosciuti restano sconosciuti invece di essere
indicati come gratuiti.

Consulta l’[ordine di autorizzazione](docs/AUTHORIZATION_ORDER.md),
la [sandbox](docs/SANDBOX.md) e la [telemetria](docs/TELEMETRY.md) — i contatori
di utilizzo sono attivi per impostazione predefinita e `codewhale config set telemetry false` li disattiva.

## Documentazione

| Per iniziare | Per approfondire |
| --- | --- |
| [Installazione](docs/INSTALL.md) | [Configurazione](docs/CONFIGURATION.md) |
| [Provider e modelli locali](docs/PROVIDERS.md) | [Architettura](docs/ARCHITECTURE.md) |
| [Modalità e permessi](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Scorciatoie da tastiera](docs/KEYBINDINGS.md) | [Creare plugin](docs/PLUGIN_AUTHORING.md) |
| [Revisione PR su GitHub](docs/GITHUB_ACTION.md) | [Tutta la documentazione](docs/README.md) |

## Comunità

Segnalazioni di bug, idee per nuove funzioni e pull request sono benvenute, che tu usi
Codewhale da mesi o lo stia provando per la prima volta. Se manca un provider o un workflow è
scomodo, [apri una issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) o
[invia una pull request](CONTRIBUTING.md). I primi contributi sono benvenuti, e
chi contribuisce mantiene il riconoscimento per il lavoro integrato. La
[struttura del repository](CONTRIBUTING.md#project-structure) è un buon punto di partenza.

Unisciti al [Discord](https://discord.gg/37gfS3ksug), oppure aggiungi Hunter su WeChat
(`hunterbown`) e chiedi di entrare nel gruppo Whale Brothers.

## Storia e licenza

Codewhale è nato come `deepseek-tui` e legge ancora la configurazione e le sessioni di quel
progetto. Ora è indipendente da qualsiasi provider, mantenuto in modo autonomo e non è
affiliato ad alcun fornitore di modelli. Grazie a
[tutti i contributori](docs/CONTRIBUTORS.md) e alle comunità open source che
hanno aiutato la sua crescita.

[MIT](LICENSE). Le parti adattate da altri progetti open source sono riportate negli
[avvisi di terze parti](docs/THIRD_PARTY_NOTICES.md).
