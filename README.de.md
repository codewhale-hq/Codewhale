<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Der Open-Source-Coding-Agent, der mit jedem Modell arbeitet.**

Codewhale liest dein Projekt, bearbeitet Dateien, führt Befehle aus und prüft seine eigene
Arbeit — in deinem Terminal, mit einem gehosteten oder lokalen Modell deiner Wahl.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Website](https://codewhale.net) · [Dokumentation](docs/README.md) · [Changelog](CHANGELOG.md) · [Mitwirken](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Eine Codewhale-Terminalsitzung" width="760">

<sub>Echte Codewhale-Terminalaufnahme einer eingerichteten lokalen Demositzung.</sub>

</div>

## Installation

macOS und Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Das Installationsskript lädt Binärdateien mit geprüfter Prüfsumme nach `~/.local/bin`. Meldet
`codewhale` danach „command not found“, führe die eine PATH-Zeile aus, die das Skript ausgibt,
oder siehe [Zum PATH hinzufügen](docs/INSTALL.md#put-it-on-your-path).
Aktualisieren kannst du jederzeit mit `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo und weitere Wege</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew unter Linux, Android/Termux, manuelle Downloads mit Prüfsummenkontrolle
und der optionale CNB-Mirror sind in der
[Installationsanleitung](docs/INSTALL.md) beschrieben. Wähle einen Weg: Mehrere Installationen
auf einem Rechner streiten sich am Ende um den `PATH`.

</details>

## Schnellstart

**Die API-Schlüssel deiner Anbieter an einem Ort verwalten.** [Erstelle ein Codewhale-Konto](https://app.codewhale.net/register)
oder führe `codewhale login` aus, um dich anzumelden. Speichere und aktualisiere deine API-Schlüssel im Konto
und nutze sie über die Codewhale-Modellroute auf deinen angemeldeten Geräten.
[Kontoschlüssel einrichten](docs/CONFIGURATION.md#account-provider-keys).
Beim Anmelden werden vorhandene lokale Schlüssel nicht hochgeladen; die lokale Nutzung ist weiterhin ohne Konto möglich.

1. **Öffne dein Projekt.** Starte `codewhale` in dem Ordner, an dem du arbeiten willst.
2. **Verbinde ein Modell.** Starte `/provider` (oder drücke `F3`), um einen gehosteten Schlüssel
   hinzuzufügen oder eine lokale Runtime zu wählen. Läuft Ollama bereits mit einem Chat-Modell,
   wechselt Codewhale von selbst dorthin. Mit `/model` wechselst du das Modell.
3. **Gib ihm eine konkrete Aufgabe.**

```text
Fix the failing tests and explain what changed.
```

Dieselbe Aufgabe läuft ohne Oberfläche aus einem Skript oder CI-Job:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Mit `/help` siehst du Befehle und Tastenkürzel.

## Wege, es auszuführen

Jeder Client steuert dieselbe lokale Codewhale Runtime, daher verhalten sich Sitzungen, Tools
und Berechtigungen überall gleich.

| Befehl | Was er tut |
| --- | --- |
| `codewhale` | Die interaktive Terminaloberfläche |
| `codewhale exec "…"` | Ein Durchlauf ohne Oberfläche aus einem Skript oder der CI; `--auto` erlaubt Datei- und Shell-Werkzeuge, `--output-format stream-json` streamt JSON |
| `codewhale web` | Der mitgelieferte [lokale Browser-Client](docs/WEB.md) auf `127.0.0.1` |
| `codewhale review --pr N` | Ein beratendes [Pull-Request-Review](docs/GITHUB_ACTION.md); das Veröffentlichen ist optional |
| Runtime API | Eine [lokale HTTP-API](docs/RUNTIME_API.md) für Threads, Ereignisse und Freigaben |

Eine native Desktop-App (GPUI) wird als Produkt-Client für angemeldete Nutzer entwickelt;
Informationen zur Verfügbarkeit findest du auf der
[Produktseite](https://codewhale.net/en/product). Die von der Community gepflegte
[VS Code-Erweiterung](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
verbindet sich aus einer Seitenleiste mit derselben Runtime ([Quellcode](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Was es kann

- **Jedes Modell, ohne Lock-in.** Über 40 eingebaute Anbieter-Routen — Anthropic, DeepSeek,
  Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI und weitere — dazu jeder
  OpenAI-kompatible Endpunkt und lokale Modelle über Ollama, vLLM oder SGLang.
  [Anbieter](docs/PROVIDERS.md)
- **Du behältst die Kontrolle.** Der Plan-Modus erkundet, ohne etwas zu ändern; Work und Operate
  nehmen Änderungen vor. Freigabe-Haltungen entscheiden, wann ein Tool-Aufruf deine Zustimmung
  braucht, `/undo` und `/restore` stellen Änderungen im Arbeitsbereich wieder her, und
  `/receipts` listet jede Datei, jeden Befehl und jede Freigabe einer Sitzung auf.
  [Modi](docs/MODES.md) · [Belege](docs/RECEIPTS.md)
- **Gebaut für lange Aufgaben.** Setze ein dauerhaftes `/goal`, delegiere abgegrenzte Arbeit an
  [Sub-Agenten](docs/SUBAGENTS.md), betreibe beaufsichtigte [Agententeams](docs/FLEET.md) mit
  einer Prüfung vor dem Ausgeben, oder skripte sie als eingecheckte
  [Workflows](docs/WORKFLOW_AUTHORING.md).
- **Erweitere, was du schon nutzt.** Verbinde [MCP-Server](docs/MCP.md), installiere
  [Skills](docs/SKILLS.md) und [Plugins](docs/PLUGINS.md), führe
  [Hooks](docs/HOOKS.md) bei Sitzungs- und Tool-Ereignissen aus und lade vorhandene
  [Claude-Code-Plugins](docs/CLAUDE_PLUGIN_COMPAT.md).
- **Computer Use.** Ein mitgeliefertes Plugin fügt Tools zum Beobachten und Bedienen anderer
  Anwendungen hinzu. Prüfe seine Zugriffsrechte und aktiviere es vor der Nutzung.
  [Anleitung](crates/tui/plugins/computer-use/README.md)

## Modi und Berechtigungen

| | Auswahl mit | Optionen |
| --- | --- | --- |
| **Modus** — was der Agent gerade tut | `Tab` oder `/mode` | Plan (erkunden, keine Änderungen) · Work (bearbeiten und ausführen) · Operate (ein Ziel über geplante, geprüfte Schritte verfolgen) |
| **Haltung** — wann er vorher fragt | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access respektiert weiterhin die harten Richtliniengrenzen. Die
[Anleitung zu Modi und Berechtigungen](docs/MODES.md) erklärt jede Option.

## Sicherheit

Codewhale läuft auf deinem Rechner mit den Zugriffsrechten, die du ihm gibst. Freigabe-Haltungen
und Repository-Regeln begrenzen, was der Agent tun darf, und Befehle laufen, wo unterstützt, in
einer Betriebssystem-Sandbox (Seatbelt unter macOS; bubblewrap unter Linux, wenn installiert und funktionsfähig).
`/preview-request` zeigt die exakte, geschwärzte Anfrage, bevor etwas gesendet wird.
Unbekannte Modellpreise bleiben unbekannt, statt als kostenlos ausgewiesen zu werden.

Siehe [Autorisierungsreihenfolge](docs/AUTHORIZATION_ORDER.md),
[Sandboxing](docs/SANDBOX.md) und [Telemetrie](docs/TELEMETRY.md) — Nutzungszähler sind
standardmäßig aktiv, und `codewhale config set telemetry false` schaltet sie ab.

## Dokumentation

| Zum Einstieg | Zum Vertiefen |
| --- | --- |
| [Installation](docs/INSTALL.md) | [Konfiguration](docs/CONFIGURATION.md) |
| [Anbieter und lokale Modelle](docs/PROVIDERS.md) | [Architektur](docs/ARCHITECTURE.md) |
| [Modi und Berechtigungen](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Tastenbelegung](docs/KEYBINDINGS.md) | [Plugins entwickeln](docs/PLUGIN_AUTHORING.md) |
| [GitHub-PR-Review](docs/GITHUB_ACTION.md) | [Gesamte Dokumentation](docs/README.md) |

## Community

Fehlerberichte, Funktionsideen und Pull Requests sind willkommen — ob du Codewhale seit Monaten
nutzt oder es zum ersten Mal ausprobierst. Fehlt ein Anbieter oder ist ein Workflow
umständlich, [eröffne ein Issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) oder
[sende einen Pull Request](CONTRIBUTING.md). Erste Beiträge sind willkommen, und wer beiträgt,
behält die Anerkennung für die übernommene Arbeit. Die
[Repository-Struktur](CONTRIBUTING.md#project-structure) ist ein guter Einstieg.

Tritt dem [Discord](https://discord.gg/37gfS3ksug) bei, oder füge Hunter auf WeChat
(`hunterbown`) hinzu und bitte darum, in die Gruppe Whale Brothers aufgenommen zu werden.

## Geschichte und Lizenz

Codewhale begann als `deepseek-tui` und liest weiterhin die Konfiguration und die Sitzungen
dieses Projekts. Es ist inzwischen anbieterneutral, wird unabhängig gepflegt und ist mit keinem
Modellanbieter verbunden. Danke an
[alle Mitwirkenden](docs/CONTRIBUTORS.md) und an die Open-Source-Communities, die zu seinem
Wachstum beigetragen haben.

[MIT](LICENSE). Aus anderen Open-Source-Projekten übernommene Teile sind in den
[Hinweisen zu Drittanbietern](docs/THIRD_PARTY_NOTICES.md) verzeichnet.
