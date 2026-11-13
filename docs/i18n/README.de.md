<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>Das Modell kann jedem gehören. Der Agent sollte dir gehören.</h1>

Codewhale ist ein quelloffener Coding-Agent für dein Terminal, geschrieben in Rust. Er arbeitet mit jedem Modell, kann eine Aufgabe auf mehrere Agenten verteilen, und du kannst jede Zeile davon lesen.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Das installiert die neueste Version unter macOS oder Linux und braucht kein Konto. [Windows, npm und Cargo](#installation) stehen weiter unten.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Website](https://codewhale.net) · [Dokumentation](../README.md) · [Changelog](../../CHANGELOG.md) · [Mitwirken](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Diese Übersetzung ist ein maschineller Entwurf und wurde noch nicht von Muttersprachlern geprüft.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale in einem Terminal nach einer Demo-Aufgabe. Es meldet, dass es export.py ein --json-Flag hinzugefügt hat, dass alle 3 lokalen Tests bestanden sind und dass ein delegiertes Review keine Probleme gefunden hat. Die Aufgabenliste darunter zeigt drei erledigte Schritte, und die Statuszeile zeigt Full Access und den lokalen Anbieter website-demo.">

<sub>Ein Entwicklungs-Build von Version 0.10.3, die noch nicht veröffentlicht ist, mit einem geskripteten Testmodell, in Full Access in einem Wegwerf-Arbeitsbereich. Die Änderung, drei Tests und ein Review durch einen Sub-Agenten liefen wirklich.</sub>

</div>

## Installation

Der Befehl oben legt das per Prüfsumme verifizierte `codewhale` und seinen Kurznamen `codew` in `~/.local/bin` ab. Wenn deine Shell danach `command not found` meldet, folge dem PATH-Hinweis, den der Installer ausgibt ([Details](../INSTALL.md#put-it-on-your-path)). `codewhale update` aktualisiert es.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Vorkompilierte Binärdateien gibt es für Linux, macOS und Windows auf x64 und arm64. Scoop, Nix, Homebrew unter Linux, Android und Termux (Vorschau) und manuelle Downloads stehen in der [Installationsanleitung](../INSTALL.md), Container unter [Docker](../DOCKER.md). Nutze pro Rechner nur eine Installationsart; sonst landen zwei Kopien auf deinem `PATH`, und die zuerst gefundene gewinnt.

## Schnellstart

1. Committe oder stashe deine Arbeit und starte dann `codewhale` im Wurzelverzeichnis eines Git-Repositorys. Standardmäßig übernimmt es Dateiänderungen im Repository ohne Rückfrage und fragt vor jedem Shell-Befehl, der nicht nur liest.
2. Führe `/provider` aus (oder drücke `F3`), wähle einen Anbieter, füge seinen API-Schlüssel ein und wähle ein Modell. DeepSeek ist der Standardanbieter. Beim ersten Start ohne Schlüssel fragt Codewhale nach einem Anbieter, sobald du deine erste Nachricht sendest, und bietet ein lokales Modell an, wenn ein lokaler Ollama-Server antwortet.
3. Gib eine Aufgabe ein:

```text
Fix the failing tests and explain what changed.
```

Befehle, die nur lesen, laufen sofort. Bei jedem anderen Shell-Befehl fragt Codewhale zuerst: `y` führt ihn einmal aus, `a` erlaubt ähnliche Befehle für den Rest des Gesprächs, und `n` lehnt ab. `/undo` nimmt deine letzte Anfrage samt ihrer Dateiänderungen zurück, und `/help` listet Befehle und Tasten auf.

Für Skripte und CI führt `codewhale exec` eine Aufgabe ohne Oberfläche aus. Lies [Läufe ohne Oberfläche](../INSTALL.md#headless-scripts-ci), bevor du ihm mit `--auto` Werkzeuge gibst, denn `--auto` nutzt Full Access, solange du keine Sandbox angibst.

## Was es tut

Gib Codewhale eine Aufgabe, und es liest das Repository, bearbeitet Dateien und führt Befehle aus, bis die Aufgabe erledigt ist oder es deine Eingabe braucht. Die [Hinweise zur neuesten Version](https://github.com/codewhale-hq/Codewhale/releases/latest) listen die neuesten Änderungen, und das [Änderungsprotokoll](../../CHANGELOG.md) enthält jede Version.

### Jedes Modell

Mehr als 40 Anbieter sind eingebaut, darunter Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter und xAI. Jeder OpenAI-kompatible Endpunkt funktioniert, ebenso lokale Modelle über Ollama, vLLM oder SGLang. Das Standardmodell ist DeepSeeks `deepseek-flash`, hinter dem jetzt DeepSeek V4.1 Flash steht ([Details](../PROVIDERS.md#deepseek-v41-flash)).

`/model` wechselt das Modell zwischen Runden, und `/model auto` wählt für jede Runde eines (`/router` legt fest, wie). `/turn inspect` zeigt, welches Modell eine Runde genutzt hat, was sie gekostet hat und wie lange sie dauerte. Siehe [Anbieter](../PROVIDERS.md).

### Belege und Rückgängig

`/receipts`, oder `codewhale receipts --last` nach dem Beenden, listet die Dateien, die eine Sitzung geändert hat, die ausgeführten Befehle, ihre Web- und MCP-Aufrufe, die gestarteten Agenten, jede Freigabe und wer sie erteilt hat, und was fehlgeschlagen ist. `/undo` nimmt deine letzte Anfrage und die von ihr geänderten Dateien in einem Schritt zurück. `/restore` setzt den ganzen Arbeitsbereich auf einen früheren Snapshot zurück; weil das spätere Änderungen überschreibt, braucht es `/trust on` oder Full Access. Siehe [Belege](../RECEIPTS.md).

### Mehrere Agenten an einer Aufgabe

`/goal` setzt ein Ziel, das über mehrere Runden bestehen bleibt. Codewhale kann Arbeit auf [Sub-Agenten](../SUBAGENTS.md) verteilen, eine [Fleet](../FLEET.md) von Agenten mit eigenen Rollen und Modellen betreiben oder einem im Repository eingecheckten [Workflow](../WORKFLOW_AUTHORING.md) folgen.

### Erweiterungen

Verbinde [MCP-Server](../MCP.md), füge [Skills](../SKILLS.md) und [Plugins](../PLUGINS.md) hinzu und führe [Hooks](../HOOKS.md) bei Sitzungs- und Werkzeugereignissen aus. Der Code-Modus (experimentell, standardmäßig an) lässt das Modell ein `execute_tools`-Programm schreiben, das mehrere MCP- und Plugin-Werkzeuge aufruft; jeder Aufruf folgt weiter deinen Freigaberegeln und behält seinen Beleg. Das in v0.10.2 enthaltene Computer-Use-Plugin (experimentell, nur macOS) kann andere Apps beobachten und bedienen; es bleibt aus, bis du seinen Zugriff geprüft und es aktiviert hast ([Anleitung](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). Kommt in 0.10.3: Das Plugin erscheint [separat](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Wege, es zu starten

| Befehl | Was er tut |
| --- | --- |
| `codewhale` | Die interaktive Terminal-App |
| `codewhale exec "…"` | Eine Aufgabe ohne Oberfläche, für Skripte und CI ([Läufe ohne Oberfläche](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Ein lokaler Web-Client auf `127.0.0.1` ([Anleitung](../WEB.md)) |
| `codewhale review --pr N` | Ein beratendes Review eines GitHub-Pull-Requests; die [Einrichtung des GitHub-Bots](../GITHUB_ACTION.md#set-up-your-own-bot) führt es mit deinem eigenen Modellschlüssel aus |
| `codewhale app-server --http` | Die [Runtime API](../RUNTIME_API.md), eine lokale HTTP-API für Threads, Ereignisse, Freigaben und die Dateiänderungen jeder Runde |
| `codewhale serve --acp` | Das Agent Client Protocol über stdio, für Editoren wie Zed |

Die Terminal-App, `exec`, `web` und die Runtime API teilen sich eine Engine ([Architektur](../ARCHITECTURE.md)). Eine von der Community gepflegte [VS-Code-Erweiterung](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) verbindet sich aus der Seitenleiste damit ([Quellcode](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Wie es sicher bleibt

Zwei Schalter legen fest, wie viel Codewhale ohne Rückfrage tun darf. Bei leerer Eingabe wechselt `Tab` den Modus: **Plan** liest und sucht, ändert aber nichts, **Work** bearbeitet Dateien und führt Befehle aus, und **Operate** hat dieselben Rechte wie Work und arbeitet weiter, bis das Ziel geprüft ist oder es dich braucht. `Shift+Tab` wechselt die Berechtigung. **Ask**, die Voreinstellung, übernimmt Dateiänderungen in deinem Repository ohne Rückfrage und fragt vor Shell-Befehlen, die nicht nur lesen. **Auto-Review** lässt Regeln und ein Review-Modell Routineaufrufe freigeben und lehnt riskante ab; wechsle zu Ask, um sie selbst freizugeben. **Full Access** fragt nie und schaltet die Sandbox von Codewhale ab, wobei Repository-Regeln und verwaltete Richtlinien weiter gelten. Die [Anleitung zu v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md) hat die Details.

Kommt in 0.10.3: `Shift+Tab` wird ein einziger Regler durch Ask, Accept Edits, Plan, Auto-Review und Full Access, `Tab` wechselt den Modus nicht mehr, und Operate entfällt. Ask fragt dann auch vor jeder Dateiänderung, und die neue Voreinstellung, Accept Edits, übernimmt Änderungen im Git-Repository und fragt weiterhin vor Befehlen. Die [aktuelle Anleitung](../MODES.md) beschreibt das.

In Ask und Auto-Review laufen Befehle in einer Betriebssystem-Sandbox, wo das System eine hat: Seatbelt unter macOS und bubblewrap unter Linux, wenn es installiert ist. Windows hat noch keine Betriebssystem-Sandbox, und Full Access schaltet die Sandbox ab. In der Sandbox können Shell-Befehle und Terminal-Sitzungen keine Zugangsdatenspeicher wie `~/.ssh`, `~/.aws` und `.env`-Dateien lesen. `/preview-request` zeigt eine geschwärzte Vorschau der nächsten Anfrage, ohne sie zu senden.

Du brauchst kein Konto, und Schlüssel, die du mit `/provider` hinzufügst, bleiben auf deinem Rechner. Mit deinem eigenen API-Schlüssel bezahlst du den Anbieter direkt, und lokale Modelle brauchen keinen Schlüssel. Die Fußzeile und `/cost` zeigen, was eine Sitzung gekostet hat, wenn der Preis bekannt ist; ein unbekannter Preis wird als unbekannt angezeigt, nie als kostenlos.

Telemetrie ist standardmäßig an. Sie sendet zusammengefasste Zahlen (Version und Plattform, Sitzungsdauer und Ergebnis, welche eingebauten Anbieter du genutzt hast, Funktions- und Fehlerzähler) mit einer zufälligen Installations-ID, die alle 90 Tage wechselt, und niemals Gespräche, Code, Prompts, Modellnamen, Datei- oder Repository-Namen oder Zugangsdaten. Dieser Befehl schaltet sie ab:

```bash
codewhale config set telemetry false
```

Beim Start prüft Codewhale außerdem auf GitHub, ob es eine neue Version gibt; setze `CODEWHALE_NO_UPDATE_CHECK=1`, um das zu überspringen. Mehr unter [Reihenfolge der Autorisierung](../AUTHORIZATION_ORDER.md), [Sandbox](../SANDBOX.md) und [Telemetrie](../TELEMETRY.md).

## Wohin es geht

Codewhale wird zu einer einzigen Umgebung über dein Terminal, eine Desktop-App, den Browser und Cloud-Computer hinweg, alles von einer Engine betrieben. Jeder Teil unten sagt, wo er heute steht.

| Teil | Stand | Heute |
| --- | --- | --- |
| Terminal-App | Veröffentlicht | Installiere sie unter macOS, Linux oder Windows und führe `codewhale` in einem beliebigen Repository aus. |
| Lokaler Browser-Client | Veröffentlicht | Führe `codewhale web` aus, um das Gespräch zu lesen, eine Aufgabe zu senden und Freigaben in deinem Browser zu beantworten; er gehört zum Terminal und braucht kein Konto. |
| Viele Agenten an einer Aufgabe | Veröffentlicht | Subagenten, Fleets und Workflows lassen mehrere Agenten eine Aufgabe teilen, jeder mit eigener Rolle. |
| Desktop-App | In Entwicklung, kein Download | Wir bauen eine Desktop-App auf derselben Engine, und sie hat noch keinen öffentlichen Download. |
| Cloud-Computer | Nur auf Einladung | Cloud-Computer gibt es nur auf Einladung, und wir bauen sie so, dass eine Sitzung auf einer Maschine in der Cloud weiterarbeiten kann. |
| Eine Sitzung über alle hinweg | Im Aufbau | Wir bauen eine Sitzung, die dich vom Terminal zum Desktop, zum Browser und in die Cloud begleitet und auf dem ganzen Weg deine bleibt. |

Die [Roadmap](../ROADMAP.md) sagt, was jede Version abdeckt und wo du helfen kannst.

## Von einem anderen Agenten kommend

Codewhale liest `AGENTS.md` von selbst. Anweisungsdateien für andere Werkzeuge wie `CLAUDE.md`, `.cursorrules` oder `.github/copilot-instructions.md` werden nur geladen, wenn du sie mit `project_instruction_imports = ["claude"]` (oder `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) in deiner Konfiguration einschaltest; siehe [config.example.toml](../../config.example.toml). Codewhale warnt, wenn es eine solche Datei findet und nicht geladen hat.

`/import-claude` bereitet einen Import deiner Claude-Code-Einstellungen und MCP-Server vor, den du prüfen kannst. `/plugin install` nimmt Claude-Code-Plugins mit Skills, Befehlen, Agentenprofilen und MCP-Servern an und lehnt Plugins ab, die Hooks oder LSP-Server brauchen ([Details](../CLAUDE_PLUGIN_COMPAT.md)).

## Dokumentation

Diese Anleitungen folgen dem Hauptzweig und können Änderungen beschreiben, die noch nicht im neuesten Release sind.

| Hier anfangen | Referenz |
| --- | --- |
| [Benutzerhandbuch](../GUIDE.md) | [Konfiguration](../CONFIGURATION.md) |
| [Installation](../INSTALL.md) | [Architektur](../ARCHITECTURE.md) |
| [Anbieter und lokale Modelle](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modi und Berechtigungen](../MODES.md) | [Plugins schreiben](../PLUGIN_AUTHORING.md) |
| [Tastenbelegung](../KEYBINDINGS.md) | [Belege](../RECEIPTS.md) |
| [Review von GitHub-Pull-Requests](../GITHUB_ACTION.md) | [Gesamte Dokumentation](../README.md) |

## Mitwirken

Codewhale wird offen von den Menschen gebaut, die es nutzen, in vielen Sprachen.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Die Codewhale-Wand der Mitwirkenden: ein Avatar für jede Person, deren Code, mitverfasste Commits, weitergeführte Pull-Requests oder gewürdigte Meldungen in Codewhale stecken, oben die Gesamtzahl."></a>

Die [Wand der Mitwirkenden](../CONTRIBUTORS.md) zählt alle, deren Code, mitverfasste Commits, weitergeführte Pull-Requests oder gewürdigte Meldungen in Codewhale stecken, auch Menschen, die GitHubs eigene Liste übersieht. `scripts/contributor-wall.py` erzeugt Bild und Liste neu aus der Git-Historie und GitHub.

Jedes Feedback ist willkommen: ein Fehler, eine Idee oder etwas, das falsch aussah oder sich falsch anfühlte. [Eröffne ein Issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) für all das; ein Issue ist auch der beste Ort, um eine neue Funktion zu beginnen. Pull-Requests sind willkommen für Fehlerbehebungen, Übersetzungen, Anbieter und geplante Arbeit. Fragen gehören in die [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). Die [Anleitung zum Mitwirken](../../CONTRIBUTING.md) zeigt, wohin jede Art von Hilfe gehört, wie deine Arbeit mit deinem Namen landet und [wo was liegt](../../CONTRIBUTING.md#project-structure).

Chatte auf [Discord](https://discord.gg/37gfS3ksug), oder füge den Maintainer Hunter auf WeChat (`hunterbown`) hinzu und frag nach der Gruppe Whale Brothers. Alle sind willkommen.

## Geschichte und Lizenz

Codewhale begann als `deepseek-tui` und liest weiterhin dessen Konfiguration und Sitzungen. Es wird unabhängig gepflegt und ist weder mit DeepSeek noch mit einem anderen Modellanbieter verbunden.

[MIT](../../LICENSE). Aus anderen Open-Source-Projekten übernommener Code steht in den Hinweisen zu Drittanbietern für [eingebundene Quellen](../../THIRD_PARTY_NOTICES.md) und [übernommene Entwürfe](../THIRD_PARTY_NOTICES.md).
