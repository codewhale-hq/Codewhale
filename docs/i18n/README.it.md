<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>Il modello può essere di chiunque. L’agente dovrebbe essere tuo.</h1>

Codewhale è un agente di programmazione open source per il tuo terminale, scritto in Rust. Funziona con qualsiasi modello, può dividere un compito tra più agenti e puoi leggerne ogni riga.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Questo installa l’ultima versione su macOS o Linux, e non richiede un account. [Windows, npm e Cargo](#installazione) sono più sotto.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Sito web](https://codewhale.net) · [Documentazione](../README.md) · [Registro delle modifiche](../../CHANGELOG.md) · [Contribuire](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Questa traduzione è una bozza automatica e non è ancora stata rivista da un madrelingua.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale in un terminale dopo un compito dimostrativo. Riferisce di aver aggiunto un’opzione --json a export.py, che tutti e 3 i test locali sono passati e che una revisione delegata non ha trovato problemi. L’elenco dei compiti sotto mostra tre passaggi completati, e la riga di stato mostra Full Access e il provider locale website-demo.">

<sub>Una build di sviluppo della 0.10.3, non ancora rilasciata, con un modello di test scriptato, in Full Access dentro un workspace usa e getta. La modifica, tre test e una revisione di un sub-agente sono stati eseguiti davvero.</sub>

</div>

## Installazione

Il comando qui sopra mette `codewhale`, verificato con checksum, e il suo alias breve `codew` in `~/.local/bin`. Se poi la shell dice `command not found`, segui la nota sul PATH che stampa l’installer ([dettagli](../INSTALL.md#put-it-on-your-path)). `codewhale update` lo aggiorna.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

I binari precompilati coprono Linux, macOS e Windows su x64 e arm64. Scoop, Nix, Homebrew su Linux, Android e Termux (anteprima) e i download manuali sono nella [guida all’installazione](../INSTALL.md), e i container in [Docker](../DOCKER.md). Usa un solo metodo di installazione per macchina; altrimenti due copie finiscono nel tuo `PATH` e vince la prima trovata.

## Avvio rapido

1. Fai commit o stash del tuo lavoro, poi esegui `codewhale` dalla radice di un repository git. Per impostazione predefinita applica le modifiche ai file del repository senza chiedere e chiede prima di ogni comando shell che non sia di sola lettura.
2. Esegui `/provider` (o premi `F3`), scegli un provider, incolla la sua chiave API e scegli un modello. DeepSeek è il provider predefinito. Al primo avvio senza chiave, Codewhale chiede un provider quando invii il primo messaggio, e propone un modello locale quando risponde un server Ollama locale.
3. Scrivi un compito:

```text
Fix the failing tests and explain what changed.
```

I comandi in sola lettura partono subito. Per qualsiasi altro comando di shell Codewhale chiede prima: `y` lo esegue una volta, `a` consente comandi simili per il resto della conversazione e `n` rifiuta. `/undo` annulla la tua ultima richiesta e le sue modifiche ai file, e `/help` elenca comandi e tasti.

Per script e CI, `codewhale exec` esegue un compito senza interfaccia. Leggi [esecuzioni senza interfaccia](../INSTALL.md#headless-scripts-ci) prima di dargli strumenti con `--auto`, perché `--auto` usa Full Access a meno che tu non aggiunga una sandbox.

## Cosa fa

Dai un compito a Codewhale: legge il repository, modifica file ed esegue comandi finché il compito è finito o gli serve una tua risposta. Le [note dell’ultima versione](https://github.com/codewhale-hq/Codewhale/releases/latest) elencano le novità più recenti, e il [registro delle modifiche](../../CHANGELOG.md) contiene ogni versione.

### Qualsiasi modello

Più di 40 provider sono integrati, tra cui Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter e xAI. Funziona qualsiasi endpoint compatibile con OpenAI, e anche i modelli locali tramite Ollama, vLLM o SGLang. Il modello predefinito è `deepseek-flash` di DeepSeek, che ora serve DeepSeek V4.1 Flash ([dettagli](../PROVIDERS.md#deepseek-v41-flash)).

`/model` cambia modello tra un turno e l’altro, e `/model auto` ne sceglie uno per ogni turno (`/router` stabilisce come). `/turn inspect` mostra quale modello ha usato un turno, quanto è costato e quanto è durato. Vedi [Provider](../PROVIDERS.md).

### Ricevute e annullamento

`/receipts`, oppure `codewhale receipts --last` dopo l’uscita, elenca i file che una sessione ha modificato, i comandi eseguiti, le chiamate web e MCP, gli agenti avviati, ogni approvazione e chi l’ha data, e cosa è fallito. `/undo` annulla la tua ultima richiesta e i file che ha modificato in un solo passo. `/restore` riporta l’intero workspace a uno snapshot precedente e, poiché sovrascrive le modifiche successive, richiede `/trust on` o Full Access. Vedi [Ricevute](../RECEIPTS.md).

### Più agenti su un compito

`/goal` imposta un obiettivo che prosegue tra i turni. Codewhale può dividere il lavoro tra [sub-agenti](../SUBAGENTS.md), eseguire una [Fleet](../FLEET.md) di agenti con ruoli e modelli propri, oppure seguire un [workflow](../WORKFLOW_AUTHORING.md) salvato nel repository.

### Estensioni

Collega [server MCP](../MCP.md), aggiungi [skill](../SKILLS.md) e [plugin](../PLUGINS.md), ed esegui [hook](../HOOKS.md) sugli eventi di sessione e degli strumenti. La modalità codice (sperimentale, attiva per impostazione predefinita) permette al modello di scrivere un programma `execute_tools` che chiama più strumenti MCP e di plugin; ogni chiamata rispetta comunque le tue regole di approvazione e conserva la sua ricevuta. Il plugin Computer Use incluso in v0.10.2 (sperimentale, solo macOS) può osservare e usare altre app, e resta spento finché non ne controlli l’accesso e lo attivi ([guida](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). In arrivo con 0.10.3: il plugin viene rilasciato [a parte](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Modi per avviarlo

| Comando | Cosa fa |
| --- | --- |
| `codewhale` | L’app interattiva da terminale |
| `codewhale exec "…"` | Un compito senza interfaccia, per script e CI ([esecuzioni senza interfaccia](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Un client web locale su `127.0.0.1` ([guida](../WEB.md)) |
| `codewhale review --pr N` | Una revisione consultiva di una pull request su GitHub; la [configurazione del bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) la esegue con la tua chiave del modello |
| `codewhale app-server --http` | La [Runtime API](../RUNTIME_API.md), un’API HTTP locale per thread, eventi, approvazioni e le modifiche ai file di ogni turno |
| `codewhale serve --acp` | L’Agent Client Protocol su stdio, per editor come Zed |

L’app da terminale, `exec`, `web` e la Runtime API condividono un solo motore ([architettura](../ARCHITECTURE.md)). Un’[estensione per VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) mantenuta dalla comunità si collega dalla barra laterale ([sorgente](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Come resta sicuro

Due interruttori dicono quanto Codewhale può fare da solo. Con l’input vuoto, `Tab` cambia modalità: **Plan** legge e cerca ma non cambia nulla, **Work** modifica file ed esegue comandi, e **Operate** ha lo stesso accesso di Work e continua finché l’obiettivo è verificato o ha bisogno di te. `Shift+Tab` cambia il permesso. **Ask**, il predefinito, applica le modifiche ai file del tuo repository senza chiedere e chiede prima dei comandi shell che non sono di sola lettura. **Auto-Review** lascia che regole e un modello revisore approvino le chiamate di routine e rifiuta quelle ad alto rischio; passa ad Ask per approvarle tu. **Full Access** non chiede mai e disattiva la sandbox di Codewhale, anche se le regole del repository e la policy gestita restano valide. La [guida di v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md) ha i dettagli.

In arrivo con 0.10.3: `Shift+Tab` diventa un’unica manopola che passa per Ask, Accept Edits, Plan, Auto-Review e Full Access, `Tab` non cambia più modalità e Operate viene ritirato. Ask chiederà anche prima di ogni modifica ai file, e il nuovo predefinito, Accept Edits, applicherà le modifiche dentro il repository git continuando a chiedere prima dei comandi. La [guida attuale](../MODES.md) lo descrive.

In Ask e Auto-Review i comandi girano in una sandbox del sistema operativo, dove il sistema ne ha una: Seatbelt su macOS e bubblewrap su Linux quando è installato. Windows non ha ancora una sandbox del sistema operativo, e Full Access disattiva la sandbox. Dentro la sandbox, i comandi di shell e le sessioni di terminale non possono leggere archivi di credenziali come `~/.ssh`, `~/.aws` e i file `.env`. `/preview-request` mostra un’anteprima oscurata della prossima richiesta senza inviarla.

Non ti serve un account, e le chiavi che aggiungi con `/provider` restano sulla tua macchina. Con la tua chiave API paghi direttamente il provider, e i modelli locali non richiedono chiavi. Il piè di pagina e `/cost` mostrano quanto è costata una sessione quando il prezzo è noto, e un prezzo sconosciuto appare come sconosciuto, mai come gratuito.

La telemetria è attiva per impostazione predefinita. Invia conteggi aggregati (versione e piattaforma, durata ed esito della sessione, quali provider integrati hai usato, conteggi di funzioni ed errori) con un ID di installazione casuale che cambia ogni 90 giorni, e mai conversazioni, codice, prompt, nomi di modelli, nomi di file o repository, né credenziali. Questo comando la disattiva:

```bash
codewhale config set telemetry false
```

All’avvio Codewhale controlla anche su GitHub se c’è una nuova versione; imposta `CODEWHALE_NO_UPDATE_CHECK=1` per saltare il controllo. Altro in [ordine di autorizzazione](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md) e [telemetria](../TELEMETRY.md).

## Dove sta andando

Codewhale sta diventando un unico ambiente che copre il tuo terminale, un’app desktop, il browser e i computer nel cloud, tutto guidato da un solo motore. Ogni parte qui sotto dice a che punto si trova oggi.

| Parte | Stato | Oggi |
| --- | --- | --- |
| App da terminale | Pubblicato | Installala su macOS, Linux o Windows ed esegui `codewhale` in qualsiasi repository. |
| Client browser locale | Pubblicato | Esegui `codewhale web` per leggere la conversazione, inviare un compito e rispondere alle approvazioni nel tuo browser; arriva con il terminale e non richiede un account. |
| Molti agenti su un compito | Pubblicato | Subagenti, Fleet e workflow permettono a più agenti di condividere un compito, ciascuno con il proprio ruolo. |
| App desktop | In sviluppo, nessun download | Stiamo costruendo un’app desktop sullo stesso motore, e non ha ancora un download pubblico. |
| Computer nel cloud | Solo su invito | I computer nel cloud sono solo su invito, e li stiamo costruendo perché una sessione possa continuare a lavorare su una macchina nel cloud. |
| Una sessione attraverso tutti | In costruzione | Stiamo costruendo una sessione che ti accompagna dal terminale al desktop, al browser e al cloud, e resta tua per tutto il percorso. |

La [roadmap](../ROADMAP.md) dice cosa copre ogni versione e dove puoi dare una mano.

## Se arrivi da un altro agente

Codewhale legge `AGENTS.md` da solo. I file di istruzioni scritti per altri strumenti, come `CLAUDE.md`, `.cursorrules` o `.github/copilot-instructions.md`, vengono caricati solo se li attivi con `project_instruction_imports = ["claude"]` (oppure `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) nella tua configurazione; vedi [config.example.toml](../../config.example.toml). Codewhale ti avvisa quando ne trova uno che non ha caricato.

`/import-claude` prepara un’importazione delle tue impostazioni di Claude Code e dei server MCP da farti rivedere. `/plugin install` accetta plugin di Claude Code che contengono skill, comandi, profili di agente e server MCP, e rifiuta i plugin che richiedono hook o server LSP ([dettagli](../CLAUDE_PLUGIN_COMPAT.md)).

## Documentazione

Queste guide seguono il ramo principale e possono descrivere modifiche non ancora presenti nell’ultima versione.

| Inizia da qui | Riferimento |
| --- | --- |
| [Guida per l’utente](../GUIDE.md) | [Configurazione](../CONFIGURATION.md) |
| [Installazione](../INSTALL.md) | [Architettura](../ARCHITECTURE.md) |
| [Provider e modelli locali](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modalità e permessi](../MODES.md) | [Scrivere plugin](../PLUGIN_AUTHORING.md) |
| [Scorciatoie da tastiera](../KEYBINDINGS.md) | [Ricevute](../RECEIPTS.md) |
| [Revisione delle pull request su GitHub](../GITHUB_ACTION.md) | [Tutta la documentazione](../README.md) |

## Contribuire

Codewhale è costruito alla luce del sole da chi lo usa, in molte lingue.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Il muro dei contributori di Codewhale: un avatar per ogni persona il cui codice, commit in coautoria, pull request riprese o segnalazioni accreditate sono in Codewhale, con il totale in alto."></a>

Il [muro dei contributori](../CONTRIBUTORS.md) conta tutte le persone il cui codice, commit in coautoria, pull request riprese o segnalazioni accreditate sono in Codewhale, comprese quelle che l’elenco dei contributori di GitHub non mostra. `scripts/contributor-wall.py` rigenera immagine ed elenco dalla cronologia git e da GitHub.

Ogni riscontro è benvenuto: un bug, un’idea o qualcosa che sembrava o dava una sensazione sbagliata. [Apri una issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) per ognuno di questi; una issue è anche il posto migliore per avviare una nuova funzione. Le pull request sono benvenute per correzioni, traduzioni, provider e lavoro pianificato. Le domande vanno nelle [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). La [guida per contribuire](../../CONTRIBUTING.md) mostra dove va ogni tipo di aiuto, come il tuo lavoro entra con il tuo nome e [dove si trova ogni cosa](../../CONTRIBUTING.md#project-structure).

Chatta su [Discord](https://discord.gg/37gfS3ksug), oppure aggiungi il maintainer, Hunter, su WeChat (`hunterbown`) e chiedi di entrare nel gruppo Whale Brothers. Tutti sono benvenuti.

## Storia e licenza

Codewhale è nato come `deepseek-tui` e legge ancora la configurazione e le sessioni di quel progetto. È mantenuto in modo indipendente e non è affiliato a DeepSeek né ad altri provider di modelli.

[MIT](../../LICENSE). Il codice adattato da altri progetti open source è elencato negli avvisi di terze parti per le [sorgenti incluse](../../THIRD_PARTY_NOTICES.md) e i [progetti adattati](../THIRD_PARTY_NOTICES.md).
