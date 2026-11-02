<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Ein Open-Source-Coding-Agent für dein Terminal, der mit jedem Modell arbeitet.**

Gib Codewhale eine Aufgabe, und es liest das Repository, bearbeitet Dateien und
führt Befehle aus, bis die Aufgabe erledigt ist oder es eine Eingabe von dir
braucht. Es arbeitet mit mehr als 40 eingebauten Anbietern, standardmäßig mit
DeepSeek V4.1 Flash, und mit Modellen, die auf deinem eigenen Rechner laufen.
`/receipts` listet die Dateien auf, die es geändert hat, die Befehle, die es
ausgeführt hat, und die Web- und MCP-Aufrufe, die es gemacht hat.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Website](https://codewhale.net) · [Dokumentation](../README.md) · [Changelog](../../CHANGELOG.md) · [Mitwirken](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Eine Codewhale-Terminalsitzung" width="760">

<sub>Ein Entwicklungs-Build von 0.10.2 mit einem geskripteten Testmodell, in Full Access in einem Wegwerf-Arbeitsbereich. Die Änderung, drei Tests und ein Review durch einen Sub-Agenten liefen wirklich.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — 30 % mehr Nutzung von DeepSeek v4.1 Flash" width="760">
  </a>
</p>

[Angebot von Codewhale × OrcaRouter ansehen](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale erhält eine Empfehlungsprovision von OrcaRouter.

## Installation

macOS und Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Das Installationsskript legt `codewhale` und den kurzen Alias `codew` nach
Prüfsummenkontrolle in `~/.local/bin` ab. Meldet deine Shell danach
`command not found`, führe die PATH-Zeile aus, die das Skript ausgibt
([Details](../INSTALL.md#put-it-on-your-path)).
`codewhale update` aktualisiert es. Diese Befehle installieren das neueste
Release. Diese Seite folgt `main`; eine mit einer Version markierte Funktion
kommt also mit diesem Release, und der [Changelog](../../CHANGELOG.md) listet, was
jedes Release hinzufügt.

<details>
<summary><b>Windows, npm, Cargo und weitere Installationswege</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Vorkompilierte Binärdateien gibt es für Linux, macOS und Windows auf x64 und
arm64. Nix, Homebrew unter Linux, Android/Termux (Vorschau) und manuelle
Downloads beschreibt die [Installationsanleitung](../INSTALL.md), Container
die Seite [Docker](../DOCKER.md). Bleib pro Rechner bei einer
Installationsmethode; sonst landen zwei Kopien im `PATH`, und es gewinnt die,
die zuerst kommt.

</details>

## Schnellstart

1. Committe oder stashe deine Arbeit und starte dann `codewhale` im
   Wurzelverzeichnis eines Git-Repositorys. Standardmäßig wendet es dort
   Dateiänderungen an, ohne zu fragen, und zeigt dir den Diff.
2. Führe `/provider` aus (oder drücke `F3`), wähle einen Anbieter, füge seinen
   API-Schlüssel ein und wähle ein Modell. DeepSeek ist der Standardanbieter.
   Ohne Schlüssel wählst du stattdessen eine lokale Runtime. Ist kein Anbieter
   konfiguriert und stellt Ollama bereits ein Chat-Modell bereit, verwendet
   Codewhale es automatisch; ab 0.10.2 bietet es dir das Modell an, sobald du
   deine erste Nachricht sendest.
3. Gib eine Aufgabe ein:

```text
Fix the failing tests and explain what changed.
```

Befehle, die nur lesen, laufen sofort. Bei jedem anderen Shell-Befehl fragt
Codewhale vorher: `y` führt ihn einmal aus, `a` erlaubt ähnliche Befehle für den
Rest der Unterhaltung, `n` lehnt ab. `/undo` macht deine letzte Anfrage samt
ihren Dateiänderungen rückgängig.
`/help` listet Befehle und Tasten auf.

Für Skripte und CI führt `codewhale exec` eine Aufgabe ohne Oberfläche aus.
Standardmäßig nutzt `--auto` Full Access: Tools laufen ohne Nachfrage,
Shell-Befehle haben keine Codewhale-Sandbox, und der Vertrauensmodus erlaubt
Datei-Tools, außerhalb des Arbeitsbereichs zu schreiben. Füge
`--sandbox workspace-write` hinzu, um die Shell-Sandbox und die Beschränkung
der Datei-Tools auf den Arbeitsbereich beizubehalten, bei ausgeschaltetem
Vertrauensmodus:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Modi und Berechtigungen

Eine neue Installation startet im Work-Modus mit der Berechtigung Ask.

| | Wechseln mit | Optionen |
| --- | --- | --- |
| Modus | `Tab` (bei leerer Eingabe) oder `/mode` | **Plan** liest und durchsucht, ändert aber nichts. **Work** bearbeitet Dateien und führt Befehle aus. **Operate** hat dieselben Zugriffsrechte wie Work, behandelt deine Anfrage als Ziel und arbeitet weiter, bis das Ziel überprüft ist oder es eine Eingabe von dir braucht. |
| Berechtigung | `Shift+Tab` | **Ask** wendet Dateiänderungen in deinem Repository an, ohne zu fragen, und fragt vor Shell-Befehlen, die nicht nur lesen. **Auto-Review** lässt Regeln und ein Prüfmodell Routineaufrufe freigeben und lehnt Aufrufe mit hohem Risiko ab; wechsle zu Ask, um diese selbst freizugeben. **Full Access** fragt nie und schaltet die Sandbox von Codewhale ab; Repository-Regeln und verwaltete Richtlinien gelten weiterhin. |

Details stehen in der [Anleitung zu Modi und Berechtigungen](../MODES.md).

## Was es kann

### Neu in 0.10.2

- `/pet on` verwandelt dein Terminal in einen animierten Wal, der zeigt, was der
  Agent gerade tut, während das Nachrichtenfeld weiter benutzbar bleibt. Seine
  980 Partikel schreiben den aktuellen Schritt auf Englisch, Chinesisch oder
  Japanisch.
- `/workbar terminal` zeigt die Shells, die der Agent ausführt, während sie
  laufen. Tippe in eine davon, oder drücke `Ctrl+N` für eine eigene Shell
  (macOS und Linux).
- `/undo` macht deine gesamte letzte Anfrage in einem Schritt rückgängig,
  einschließlich jeder Datei, die sie geändert hat, und `/diff` funktioniert in
  Ordnern ohne Git.
- Wenn ein Plan bereit ist, fragt Codewhale, ob es Work mit Ask oder
  Auto-Review starten oder weiterplanen soll.
- `/color` gibt jedem Chat eine eigene Farbe für das Nachrichtenfeld, die mit
  der Sitzung gespeichert wird.
- `web_search` kann die neue Search API von DeepSeek verwenden: setze
  `[search] provider = "deepseek"` mit einem DeepSeek-Schlüssel.
- Außerhalb von Full Access läuft ein `curl` oder `wget`, das Zugangsdaten
  hochladen würde, nie ohne deine Freigabe, und ein Befehl, der unsichtbares
  Unicode versteckt, wird abgelehnt. `write` ersetzt keine Datei mehr, die der
  Agent nicht gelesen hat oder die sich geändert hat, nachdem er sie gelesen
  hat.

### Modelle

Mehr als 40 Anbieter sind eingebaut, darunter Anthropic, OpenAI, DeepSeek,
Google, Mistral, Moonshot, OpenRouter und xAI. Jeder OpenAI-kompatible Endpunkt
funktioniert, ebenso lokale Modelle über Ollama, vLLM oder SGLang. Standard ist
`deepseek-flash` von DeepSeek, das DeepSeek am 2026-10-17 auf
DeepSeek-V4.1-Flash-1017 umgestellt hat; 0.10.1 nutzt es ohne Update
([Details](../PROVIDERS.md#deepseek-v41-flash)).

`/model` wechselt das Modell zwischen den Turns, und `/model auto` wählt für
jeden Turn eines aus (`/router` legt fest, wie). `/turn inspect` zeigt, welches
Modell ein Turn verwendet hat, was er gekostet hat und wie lange er gedauert
hat. Mit `codewhale auth chatgpt` kannst du statt eines API-Schlüssels ein
ChatGPT-Abo verwenden (experimentell). Siehe [Anbieter](../PROVIDERS.md).

### Belege und Rückgängigmachen

`/receipts`, oder nach dem Beenden `codewhale receipts --last`, listet geänderte
Dateien, ausgeführte Befehle, Web- und MCP-Aufrufe, gestartete Agenten,
Freigaben und wer sie erteilt hat sowie Fehlschläge auf. `/undo` macht deine letzte Anfrage und ihre Dateiänderungen in einem Schritt rückgängig, ohne Vertrauensmodus. `/restore` setzt den gesamten
Arbeitsbereich auf einen früheren Snapshot zurück; weil dabei spätere
Änderungen überschrieben werden, braucht es `/trust on` oder Full Access. Siehe
[Belege](../RECEIPTS.md).

Exec-Läufe, die Tools anbieten, werden in jedem Ausgabeformat gespeichert,
sodass `codewhale receipts --last` nach `exec --auto` funktioniert, auch mit
`--json`. Läufe, die nur antworten, werden im Textformat und mit `--json` nur
beim Fortsetzen oder beim Beginn der Gesprächskompaktierung gespeichert;
verwende `--output-format stream-json`, um sie immer für Belege oder
`exec --continue` zu speichern.

### Längere Arbeit

`/goal` setzt ein Ziel, das über mehrere Turns bestehen bleibt. Codewhale kann
Arbeit auf [Sub-Agenten](../SUBAGENTS.md) verteilen, eine
[Flotte](../FLEET.md) von Agenten betreiben oder einem im Repository
eingecheckten [Workflow](../WORKFLOW_AUTHORING.md) folgen.

### Umstieg von einem anderen Agenten

Codewhale liest `AGENTS.md` von selbst. Anweisungsdateien für andere Tools wie
`CLAUDE.md`, `.cursorrules` oder `.github/copilot-instructions.md` werden nur
geladen, wenn du sie mit `project_instruction_imports = ["claude"]` (oder
`"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) in deiner
Konfiguration aktivierst; siehe [config.example.toml](../../config.example.toml).
Codewhale warnt, wenn es eine findet, die es nicht geladen hat.

`/import-claude` bereitet einen Import deiner Claude-Code-Einstellungen und
MCP-Server vor, den du prüfen kannst. `/plugin install` nimmt Claude-Code-Plugins
mit Skills, Befehlen, Agentenprofilen und MCP-Servern an und lehnt Plugins ab,
die Hooks oder LSP-Server brauchen
([Details](../CLAUDE_PLUGIN_COMPAT.md)).

### Erweiterungen

Verbinde [MCP-Server](../MCP.md), füge [Skills](../SKILLS.md) und
[Plugins](../PLUGINS.md) hinzu und führe [Hooks](../HOOKS.md) bei Sitzungs-
und Tool-Ereignissen aus. Der Code-Modus (experimentell, standardmäßig
eingeschaltet) lässt das Modell ein einziges `execute_tools`-Programm schreiben,
das mehrere MCP- und Plugin-Tools aufruft; jeder Aufruf unterliegt weiterhin
deinen Freigaberegeln und behält seinen Beleg. Das unabhängig installierte Plugin
Computer Use (experimentell, nur macOS) kann andere Apps beobachten und
bedienen. Es bleibt ausgeschaltet,
bis du seine Zugriffsrechte geprüft und es aktiviert hast
([Anleitung](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Wege, es auszuführen

| Befehl | Was er tut |
| --- | --- |
| `codewhale` | Die interaktive Terminal-App |
| `codewhale exec "…"` | Eine Aufgabe ohne Oberfläche. Ohne weitere Optionen antwortet es, ohne Tools zu verwenden; `--auto` aktiviert Tools standardmäßig mit Full Access, `--sandbox workspace-write` schränkt sie ein, und `--output-format stream-json` streamt Ereignisse als JSON |
| `codewhale web` | Eine lokale Web-Oberfläche auf `127.0.0.1` ([Anleitung](../WEB.md)) |
| `codewhale review --pr N` | Ein beratendes Review eines GitHub-Pull-Requests. Die [GitHub-Bot-Einrichtung](../GITHUB_ACTION.md#set-up-your-own-bot) erzeugt einen gepinnten Workflow für automatische Reviews und `@codewhale review`, mit deinem eigenen Modellschlüssel und optionaler App-Identität |
| `codewhale app-server --http` | Die [Runtime API](../RUNTIME_API.md), eine lokale HTTP-API für Threads, Ereignisse, Freigaben und die Dateiänderungen jedes Turns |
| `codewhale serve --acp` | Das Agent Client Protocol über stdio, für Editoren wie Zed |

Die Terminal-App, `exec`, `web` und die Runtime API teilen sich eine Engine
([Architektur](../ARCHITECTURE.md)). Eine von der Community gepflegte
[VS-Code-Erweiterung](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
verbindet sich aus der Seitenleiste damit ([Quellcode](https://github.com/HengQuWorld/CodeWhale-VSCode)).
Eine native Desktop-App auf Basis der Runtime API ist in Entwicklung; einen
öffentlichen Download gibt es noch nicht.

## Kosten und Konten

Codewhale ist kostenlos und unter MIT lizenziert. Mit deinem eigenen
API-Schlüssel bezahlst du den Anbieter direkt; lokale Modelle brauchen keinen
Schlüssel. Die Fußzeile und `/cost` zeigen, was eine Sitzung gekostet hat, wenn
der Preis bekannt ist; ein unbekannter Preis wird als unbekannt angezeigt, nie
als kostenlos.

Du brauchst kein Konto. Schlüssel, die du mit `/provider` hinzufügst, bleiben
auf deinem Rechner.

## Sicherheit und Datenschutz

In Ask und Auto-Review laufen Befehle in einer Betriebssystem-Sandbox, sofern
das System eine hat: Seatbelt unter macOS und bubblewrap unter Linux, wenn es
installiert ist. Windows hat noch keine Betriebssystem-Sandbox, und Full Access
schaltet die Sandbox ab. In der Sandbox können Shell-Befehle keine Speicherorte
für Zugangsdaten wie `~/.ssh`, `~/.aws` und `.env`-Dateien lesen; ab 0.10.2
gilt dieselbe Regel auch für Terminalsitzungen und weitere Datei-Tools.
`/preview-request` zeigt eine geschwärzte Vorschau der nächsten Anfrage, ohne
sie zu senden.

Telemetrie ist standardmäßig eingeschaltet. Sie sendet aggregierte Zählwerte
(Version und Plattform, Sitzungsdauer und -ergebnis, welche eingebauten
Anbieter du verwendet hast, Funktions- und Fehlerzähler) mit einer zufälligen
Installations-ID, die alle 90 Tage wechselt, und niemals Unterhaltungen, Code,
Prompts, Modellnamen, Datei- oder Repository-Namen oder Zugangsdaten. `codewhale config set telemetry false` schaltet sie ab. Beim Start
prüft Codewhale außerdem auf GitHub, ob es ein neues Release gibt; setze
`CODEWHALE_NO_UPDATE_CHECK=1`, um das zu überspringen. Mehr dazu unter
[Autorisierungsreihenfolge](../AUTHORIZATION_ORDER.md),
[Sandboxing](../SANDBOX.md) und [Telemetrie](../TELEMETRY.md).

## Dokumentation

| Zum Einstieg | Referenz |
| --- | --- |
| [Installation](../INSTALL.md) | [Konfiguration](../CONFIGURATION.md) |
| [Anbieter und lokale Modelle](../PROVIDERS.md) | [Architektur](../ARCHITECTURE.md) |
| [Modi und Berechtigungen](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [Tastenbelegung](../KEYBINDINGS.md) | [Plugins entwickeln](../PLUGIN_AUTHORING.md) |
| [GitHub-PR-Review](../GITHUB_ACTION.md) | [Gesamte Dokumentation](../README.md) |

## Community

Fehlt ein Anbieter oder ist ein Workflow umständlich,
[eröffne ein Issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) oder
[sende einen Pull Request](../../CONTRIBUTING.md). Wer beiträgt, behält die
Anerkennung für die Arbeit, die übernommen wird, und die
[Repository-Struktur](../../CONTRIBUTING.md#project-structure) zeigt, wo was liegt.

Chatte auf [Discord](https://discord.gg/37gfS3ksug), oder füge den Maintainer
Hunter auf WeChat (`hunterbown`) hinzu und bitte darum, in die Gruppe
Whale Brothers aufgenommen zu werden.

## Geschichte und Lizenz

Codewhale begann als `deepseek-tui` und liest weiterhin die Konfiguration und
die Sitzungen dieses Projekts. Es wird unabhängig gepflegt und ist weder mit
DeepSeek noch mit einem anderen Modellanbieter verbunden. Danke an
[alle, die beigetragen haben](../CONTRIBUTORS.md).

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). Code, der aus anderen Open-Source-Projekten übernommen wurde,
ist in den [Hinweisen zu Drittanbietern](../THIRD_PARTY_NOTICES.md)
aufgeführt.
