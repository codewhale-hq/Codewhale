<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agente di programmazione open source in Rust per il tuo terminale. Qualsiasi modello, flotte multi-agente, sandboxing a livello di sistema operativo e ricevute verificabili.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Sito web](https://codewhale.net) · [Documentazione](../README.md) · [Registro delle modifiche](../../CHANGELOG.md) · [Contribuire](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · Italiano · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 in un terminale dopo un’attività reale su Ollama locale (qwen3:4b): ha modificato ls.py, atteso approvazione prima di eseguire python3 -m unittest -v test_ls.py, eseguito 2 test superati e riportato il risultato.">

<sub>Sessione reale sulla versione pubblicata Codewhale v0.10.4 (48669ad108fa) con Ollama locale (qwen3:4b) in modalità Accept Edits.</sub>

</div>

## Installazione

Lo script di installazione qui sopra installa il binario verificato tramite checksum `codewhale` e il suo alias breve `codew` in `~/.local/bin` su macOS e Linux. Se la shell segnala `command not found`, aggiungi `~/.local/bin` al tuo `PATH` ([dettagli](../INSTALL.md#put-it-on-your-path)). Aggiorna sul posto con `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

I binari precompilati sono distribuiti per Linux, macOS e Windows (x64 e arm64). Scoop, Nix, Homebrew su Linux, Android/Termux (anteprima) e i download manuali si trovano in [docs/INSTALL.md](../INSTALL.md); le immagini container sono in [docs/DOCKER.md](../DOCKER.md). Usa un solo metodo di installazione per macchina così che due binari non si oscurino a vicenda nel `PATH`.

## Avvio rapido

1. Esegui commit o `git stash` del tuo lavoro, quindi avvia `codewhale` dalla radice di un repository git.
2. Esegui `/provider` (oppure premi `F3`) per scegliere un provider, incollare una chiave API e selezionare un modello. DeepSeek è l’impostazione predefinita; se non è configurata alcuna chiave e risponde un server Ollama locale, Codewhale propone i suoi modelli al primo messaggio.
3. Assegna un’attività:

```text
Fix the failing tests and explain what changed.
```

I comandi di sola lettura vengono eseguiti subito, tranne in Ask, che chiede prima di ogni comando. I comandi shell che modificano lo stato chiedono approvazione: `y` esegue una volta, `a` consente i comandi corrispondenti per il resto della sessione e `n` rifiuta. Esegui `/undo` per annullare l’ultimo turno e le relative modifiche ai file, oppure `/help` per comandi e scorciatoie da tastiera.

## Funzionalità

- **Oltre 50 provider integrati e modelli locali:** Anthropic, OpenAI, DeepSeek (`deepseek-flash` / V4.1 Flash per impostazione predefinita; [dettagli](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, qualsiasi endpoint compatibile con OpenAI e runtime locali tramite Ollama, vLLM o SGLang. Cambia modello a metà sessione con `/model`, instrada per singolo turno con `/model auto` (`/router`) e ispeziona latenza e costi con `/turn inspect`. Vedi [Provider](../PROVIDERS.md).
- **Ricevute e ripristino:** `/receipts` (oppure `codewhale receipts --last` dopo l’uscita) registra file modificati, comandi, chiamate web/MCP, sub-agent, approvazioni ed errori. `/undo` annulla l’ultimo turno in un solo passaggio; `/restore` riporta l’area di lavoro a uno snapshot precedente (richiede `/trust on` o Full Access). Vedi [Ricevute](../RECEIPTS.md).
- **Sub-agent, Fleet e workflow:** Porta avanti obiettivi su più turni con `/goal`, delega attività ai [sub-agent](../SUBAGENTS.md), coordina [Fleet](../FLEET.md) per ruolo e modello o esegui [workflow](../WORKFLOW_AUTHORING.md) versionati nel repository.
- **MCP, skill, hook e plugin:** Collega [server MCP](../MCP.md), [skill](../SKILLS.md), [plugin](../PLUGINS.md) e [hook](../HOOKS.md) del ciclo di vita. La modalità codice sperimentale (`execute_tools`) raggruppa più chiamate MCP/plugin sotto le normali regole di approvazione. Il plugin opzionale per macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) resta disattivato finché non viene esaminato e abilitato esplicitamente.
- **Compatibilità immediata:** Legge automaticamente `AGENTS.md`. Abilita `CLAUDE.md`, `.cursorrules` o `.github/copilot-instructions.md` tramite `project_instruction_imports` in [config.example.toml](../../config.example.toml). Importa impostazioni e server MCP di Claude Code con `/import-claude`, oppure installa plugin Claude Code (skill, comandi, profili agente, MCP) e plugin DeepSeek Harness (DSH) con `/plugin install` ([dettagli](../CLAUDE_PLUGIN_COMPAT.md)).

## Punti di ingresso

| Comando | Scopo |
| --- | --- |
| `codewhale` | Applicazione interattiva da terminale |
| `codewhale exec "…"` | Runner headless per singole attività in script e CI ([guida](../INSTALL.md#headless-scripts-ci); `--auto` usa Full Access se non in sandbox) |
| `codewhale web` | Client web locale su `127.0.0.1` ([guida](../WEB.md)) |
| `codewhale review --pr N` | Revisione consultiva di PR GitHub; [configurazione bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) con la tua chiave |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md) HTTP locale per thread, eventi, approvazioni e diff per turno |
| `codewhale serve --acp` | Agent Client Protocol su stdio per editor come Zed |

Tutti i punti di ingresso condividono lo stesso motore in Rust ([architettura](../ARCHITECTURE.md)). Un’[estensione VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) mantenuta dalla community ([sorgente](https://github.com/HengQuWorld/CodeWhale-VSCode)) si collega allo stesso motore. Consulta le [ultime note di rilascio](https://github.com/codewhale-hq/Codewhale/releases/latest) e la [roadmap](../ROADMAP.md) per lo stato attuale di release, desktop e cloud.

## Permessi, sandboxing e privacy

`Shift+Tab` scorre le modalità di permesso: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` completa soltanto). **Accept Edits** (predefinita) applica le modifiche dentro il repository git e chiede conferma prima dei comandi. **Ask** chiede conferma per ogni modifica e comando. **Plan** è di sola lettura. **Auto-Review** usa regole e un modello revisore per approvare le chiamate di routine e bloccare quelle ad alto rischio. **Full Access** salta le richieste di conferma e disattiva il sandbox del sistema operativo (le regole del repository e le policy gestite restano attive). Vedi [Modalità](../MODES.md).

In Ask, Accept Edits e Auto-Review, i comandi girano in un sandbox del sistema operativo dove disponibile: Seatbelt su macOS e bubblewrap su Linux (Windows non ha ancora un sandbox di sistema). I comandi in sandbox e le sessioni terminale non possono leggere percorsi di credenziali come `~/.ssh`, `~/.aws` o file `.env`. Esegui `/preview-request` per ispezionare una richiesta in uscita oscurata prima dell’invio.

Le chiavi API aggiunte con `/provider` restano sulla tua macchina e i modelli locali non richiedono chiavi. `/cost` e il piè di pagina tracciano la spesa della sessione quando il listino è noto (un prezzo sconosciuto viene mostrato come sconosciuto, mai come gratuito).

La telemetria aggregata è attiva per impostazione predefinita (versione, SO, durata/esito della sessione, ID dei provider integrati, ID dei modelli del catalogo pubblico integrato (i modelli personalizzati, privati o locali vengono registrati come `other`), totali dei token, conteggi di chiamate/errori degli strumenti integrati e contatori di funzionalità/errori; ID di installazione casuale ruotato ogni 90 giorni; mai codice, prompt, conversazioni, nomi di modelli personalizzati/privati, nomi di file/repo/branch, credenziali o indirizzi IP). Disattivala con:

```bash
codewhale config set telemetry false
```

All'avvio Codewhale controlla anche eventuali nuove release su GitHub (`CODEWHALE_NO_UPDATE_CHECK=1` per saltare) e aggiorna il catalogo pubblico `models.dev` ogni 24 ore (`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` o `[catalog] auto_refresh = false` per saltare; disattivato automaticamente in CI e con `DO_NOT_TRACK`). Dettagli: [ordine di autorizzazione](../AUTHORIZATION_ORDER.md), [sandboxing](../SANDBOX.md) e [telemetria](../TELEMETRY.md).

## Documentazione

Le guide seguono `main` e possono descrivere modifiche non ancora presenti nell’ultima release.

| Inizia qui | Riferimento |
| --- | --- |
| [Guida utente](../GUIDE.md) | [Configurazione](../CONFIGURATION.md) |
| [Installazione](../INSTALL.md) | [Architettura](../ARCHITECTURE.md) |
| [Provider e modelli locali](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modalità e permessi](../MODES.md) | [Creazione di plugin](../PLUGIN_AUTHORING.md) |
| [Scorciatoie da tastiera](../KEYBINDINGS.md) | [Ricevute](../RECEIPTS.md) |
| [Revisione pull request GitHub](../GITHUB_ACTION.md) | [Tutta la documentazione](../README.md) |

## Contribuire e licenza

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Muro dei contributori di Codewhale"></a>

Il [muro dei contributori](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) registra codice, commit in coautoria, PR integrate e segnalazioni accreditate. Segnalazioni di bug e proposte di funzionalità vanno in [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), le domande in [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), mentre le linee guida per le PR e la struttura del progetto sono in [CONTRIBUTING.md](../../CONTRIBUTING.md) ([struttura del progetto](../../CONTRIBUTING.md#project-structure)). Unisciti su [Discord](https://discord.gg/37gfS3ksug) o aggiungi `hunterbown` su WeChat per il gruppo Whale Brothers.

Codewhale è nato come `deepseek-tui` e rimane retrocompatibile con la sua configurazione e le sue sessioni. È mantenuto in modo indipendente e non è affiliato a DeepSeek né ad alcun altro provider di modelli.

[MIT](../../LICENSE). Note di terze parti: [sorgenti inclusi](../../THIRD_PARTY_NOTICES.md) e [design adattati](../THIRD_PARTY_NOTICES.md).
