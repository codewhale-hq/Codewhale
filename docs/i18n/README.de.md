<!-- source: README.md sha256:7e0e7ebc680b -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Open-Source-Coding-Agent in Rust für dein Terminal. Beliebige Modelle, Multi-Agent-Fleets, OS-Sandboxing und überprüfbare Belege.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Website](https://codewhale.net) · [Dokumentation](../README.md) · [Changelog](../../CHANGELOG.md) · [Mitwirken](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · Deutsch · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 im Terminal nach einer echten Aufgabe mit lokalem Ollama (qwen3:4b): hat ls.py bearbeitet, vor dem Ausführen von python3 -m unittest -v test_ls.py um Freigabe gebeten, 2 bestandene Tests ausgeführt und das Ergebnis zusammengefasst.">

<sub>Echte Sitzung mit der veröffentlichten Version Codewhale v0.10.4 (48669ad108fa) und lokalem Ollama (qwen3:4b) im Modus Accept Edits.</sub>

</div>

## Installation

Das Installationsskript oben legt das prüfsummengeprüfte Binary `codewhale` samt kurzem Alias `codew` unter macOS und Linux in `~/.local/bin` ab. Meldet deine Shell `command not found`, füge `~/.local/bin` zu deinem `PATH` hinzu ([Details](../INSTALL.md#put-it-on-your-path)). Aktualisiere direkt mit `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Vorkompilierte Binaries erscheinen für Linux, macOS und Windows (x64 und arm64). Scoop, Nix, Homebrew unter Linux, Android/Termux (Vorschau) und manuelle Downloads stehen in [docs/INSTALL.md](../INSTALL.md); Container-Images in [docs/DOCKER.md](../DOCKER.md). Nutze pro Rechner nur eine Installationsmethode, damit sich zwei Binaries im `PATH` nicht gegenseitig verdecken.

## Schnellstart

1. Committe oder stashe deine Arbeit und starte `codewhale` im Wurzelverzeichnis eines Git-Repositories.
2. Rufe `/provider` auf (oder drücke `F3`), wähle einen Anbieter, füge deinen API-Schlüssel ein und wähle ein Modell. DeepSeek ist die Voreinstellung; ist kein Schlüssel eingerichtet und ein lokaler Ollama-Server antwortet, bietet Codewhale beim ersten Prompt dessen Modelle an.
3. Gib eine Aufgabe ein:

```text
Fix the failing tests and explain what changed.
```

Nur-lesende Befehle laufen sofort. Verändernde Shell-Befehle fragen nach einer Freigabe: `y` erlaubt einmalig, `a` erlaubt passende Befehle für den Rest der Sitzung und `n` lehnt ab. Mit `/undo` machst du den letzten Schritt samt Dateiänderungen rückgängig; `/help` zeigt Befehle und Tastenkürzel.

## Funktionen

- **50+ integrierte Anbieter und lokale Modelle:** Anthropic, OpenAI, DeepSeek (`deepseek-flash` / V4.1 Flash als Standard; [Details](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, jeder OpenAI-kompatible Endpunkt sowie lokale Runtimes über Ollama, vLLM oder SGLang. Wechsle Modelle mitten in der Sitzung mit `/model`, route pro Antwort mit `/model auto` (`/router`) und prüfe Latenz und Kosten mit `/turn inspect`. Siehe [Anbieter](../PROVIDERS.md).
- **Belege und Rollback:** `/receipts` (oder `codewhale receipts --last` nach dem Beenden) protokolliert geänderte Dateien, Befehle, Web-/MCP-Aufrufe, Sub-Agents, Freigaben und Fehler. `/undo` macht den letzten Schritt auf einmal rückgängig; `/restore` setzt den Workspace auf einen früheren Snapshot zurück (erfordert `/trust on` oder Full Access). Siehe [Belege](../RECEIPTS.md).
- **Sub-Agents, Fleets und Workflows:** Verfolge mehrstufige Ziele mit `/goal`, delegiere Aufgaben an [Sub-Agents](../SUBAGENTS.md), koordiniere rollen- und modellspezifische [Fleets](../FLEET.md) oder führe im Repository versionierte [Workflows](../WORKFLOW_AUTHORING.md) aus.
- **MCP, Skills, Hooks und Plugins:** Binde [MCP-Server](../MCP.md), [Skills](../SKILLS.md), [Plugins](../PLUGINS.md) und Lifecycle-[Hooks](../HOOKS.md) ein. Der experimentelle Code-Modus (`execute_tools`) bündelt mehrere MCP-/Plugin-Aufrufe unter deinen normalen Freigaberegeln. Das optionale macOS-Plugin [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) bleibt deaktiviert, bis du es ausdrücklich geprüft und eingeschaltet hast.
- **Direkte Kompatibilität:** Liest `AGENTS.md` automatisch. Aktiviere `CLAUDE.md`, `.cursorrules` oder `.github/copilot-instructions.md` über `project_instruction_imports` in [config.example.toml](../../config.example.toml). Importiere Einstellungen und MCP-Server aus Claude Code mit `/import-claude` oder installiere Claude-Code-Plugins (Skills, Befehle, Agent-Profile, MCP) und DeepSeek-Harness-(DSH)-Plugins mit `/plugin install` ([Details](../CLAUDE_PLUGIN_COMPAT.md)).

## Einstiegspunkte

| Befehl | Zweck |
| --- | --- |
| `codewhale` | Interaktive Terminal-App |
| `codewhale exec "…"` | Headless-Einzelaufgaben-Runner für Skripte und CI ([Anleitung](../INSTALL.md#headless-scripts-ci); `--auto` nutzt Full Access, sofern nicht gesandboxt) |
| `codewhale web` | Lokaler Web-Client unter `127.0.0.1` ([Anleitung](../WEB.md)) |
| `codewhale review --pr N` | Hinweisführendes GitHub-PR-Review; [GitHub-Bot-Einrichtung](../GITHUB_ACTION.md#set-up-your-own-bot) läuft mit deinem eigenen Schlüssel |
| `codewhale app-server --http` | Lokale HTTP-[Runtime-API](../RUNTIME_API.md) für Threads, Events, Freigaben und Diffs pro Schritt |
| `codewhale serve --acp` | Agent Client Protocol über stdio für Editoren wie Zed |

Alle Einstiegspunkte teilen sich eine gemeinsame Rust-Engine ([Architektur](../ARCHITECTURE.md)). Eine von der Community gepflegte [VS-Code-Erweiterung](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) ([Quellcode](https://github.com/HengQuWorld/CodeWhale-VSCode)) verbindet sich mit derselben Engine. Den aktuellen Release- sowie Desktop-/Cloud-Status findest du in den [aktuellen Release Notes](https://github.com/codewhale-hq/Codewhale/releases/latest) und der [Roadmap](../ROADMAP.md).

## Berechtigungen, Sandboxing und Datenschutz

`Shift+Tab` schaltet die Berechtigungsmodi durch: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` vervollständigt nur). **Accept Edits** (Standard) wendet Änderungen im Git-Repository direkt an und fragt vor Befehlen nach. **Ask** fragt bei jeder Bearbeitung und jedem Befehl. **Plan** arbeitet rein lesend. **Auto-Review** nutzt Regeln und ein Review-Modell, um Routineaufrufe freizugeben und risikoreiche zu blockieren. **Full Access** überspringt Rückfragen und schaltet die OS-Sandbox aus (Repository-Regeln und verwaltete Richtlinien gelten weiter). Siehe [Modi](../MODES.md).

In Ask, Accept Edits und Auto-Review laufen Befehle nach Möglichkeit in einer OS-Sandbox: Seatbelt unter macOS und bubblewrap unter Linux (Windows bietet noch keine OS-Sandbox). Gesandboxte Befehle und Terminalsitzungen können keine Zugangsdaten-Pfade wie `~/.ssh`, `~/.aws` oder `.env`-Dateien lesen. Mit `/preview-request` prüfst du eine geschwärzte ausgehende Anfrage vor dem Senden.

Über `/provider` hinterlegte API-Schlüssel bleiben auf deinem Rechner; lokale Modelle brauchen keinen Schlüssel. `/cost` und die Fußzeile zeigen die Sitzungskosten, sofern Preise bekannt sind (unbekannte Preise werden als unbekannt ausgewiesen, niemals als kostenlos).

Aggregierte Telemetrie ist standardmäßig aktiv (Version, Betriebssystem, Sitzungsdauer/-ergebnis, eingebaute Anbieter-IDs, Modell-IDs aus dem mitgelieferten öffentlichen Katalog (eigene, private oder lokale Modelle werden als `other` gezählt), Token-Summen, Aufruf-/Fehlerzahlen eingebauter Werkzeuge und Funktions-/Fehlerzähler; zufällige, alle 90 Tage rotierende Installations-ID; niemals Code, Prompts, Gespräche, eigene/private Modellnamen, Datei-/Repo-/Branch-Namen, Zugangsdaten oder IP-Adressen). Du deaktivierst sie mit:

```bash
codewhale config set telemetry false
```

Beim Start prüft Codewhale außerdem GitHub auf neue Releases (`CODEWHALE_NO_UPDATE_CHECK=1` zum Überspringen) und aktualisiert alle 24 Stunden den öffentlichen `models.dev`-Modellkatalog (`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` oder `[catalog] auto_refresh = false` zum Überspringen; in CI und bei `DO_NOT_TRACK` automatisch unterdrückt). Details: [Autorisierungsreihenfolge](../AUTHORIZATION_ORDER.md), [Sandboxing](../SANDBOX.md) und [Telemetrie](../TELEMETRY.md).

## Dokumentation

Die Anleitungen folgen `main` und können Änderungen beschreiben, die noch nicht im letzten Release enthalten sind.

| Einstieg | Referenz |
| --- | --- |
| [Benutzerhandbuch](../GUIDE.md) | [Konfiguration](../CONFIGURATION.md) |
| [Installation](../INSTALL.md) | [Architektur](../ARCHITECTURE.md) |
| [Anbieter und lokale Modelle](../PROVIDERS.md) | [Runtime-API](../RUNTIME_API.md) |
| [Modi und Berechtigungen](../MODES.md) | [Plugin-Entwicklung](../PLUGIN_AUTHORING.md) |
| [Tastenkürzel](../KEYBINDINGS.md) | [Belege](../RECEIPTS.md) |
| [GitHub-Pull-Request-Review](../GITHUB_ACTION.md) | [Gesamte Dokumentation](../README.md) |

## Mitwirken und Lizenz

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale-Mitwirkenden-Wand"></a>

Die [Mitwirkenden-Übersicht](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) erfasst Code, Co-Author-Commits, übernommene PRs und genannte Fehlerberichte. Fehlerberichte und Funktionsvorschläge gehören in [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), Fragen in [Discussions](https://github.com/codewhale-hq/Codewhale/discussions); PR-Richtlinien und Aufbau stehen in [CONTRIBUTING.md](../../CONTRIBUTING.md) ([Projektstruktur](../../CONTRIBUTING.md#project-structure)). Tritt unserem [Discord](https://discord.gg/37gfS3ksug) bei oder füge `hunterbown` auf WeChat für die Whale-Brothers-Gruppe hinzu.

Codewhale begann als `deepseek-tui` und bleibt abwärtskompatibel zu dessen Konfiguration und Sitzungen. Das Projekt wird unabhängig gepflegt und steht in keiner Verbindung zu DeepSeek oder anderen Modellanbietern.

[MIT](../../LICENSE). Hinweise zu Drittanbietern: [übernommene Quellen](../../THIRD_PARTY_NOTICES.md) und [adaptierte Entwürfe](../THIRD_PARTY_NOTICES.md).
