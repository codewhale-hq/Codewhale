<!-- source: README.md sha256:5da2c4c9d5f9 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Otwartoźródłowy agent programistyczny w języku Rust do twojego terminala. Dowolny model, floty wieloagentowe, piaskownica systemu operacyjnego i weryfikowalne pokwitowania.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Strona](https://codewhale.net) · [Dokumentacja](../README.md) · [Lista zmian](../../CHANGELOG.md) · [Współtworzenie](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 w terminalu po rzeczywistym zadaniu na lokalnym Ollama (qwen3:4b): edytował ls.py, poczekał na zatwierdzenie przed uruchomieniem python3 -m unittest -v test_ls.py, uruchomił 2 przechodzące testy i zwrócił wynik.">

<sub>Rzeczywista sesja w opublikowanym Codewhale v0.10.4 (48669ad108fa) na lokalnym Ollama (qwen3:4b) w trybie Accept Edits.</sub>

</div>

## Instalacja

Powyższy skrypt instaluje zweryfikowane sumą kontrolną `codewhale` oraz krótki alias `codew` w `~/.local/bin` na macOS i Linuxie. Jeśli powłoka zgłasza `command not found`, dodaj `~/.local/bin` do `PATH` ([szczegóły](../INSTALL.md#put-it-on-your-path)). Aktualizacja w miejscu: `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Gotowe binaria są dostępne dla Linuxa, macOS i Windows (x64 oraz arm64). Scoop, Nix, Homebrew na Linuxie, Android/Termux (wersja poglądowa) oraz ręczne pobieranie opisano w [docs/INSTALL.md](../INSTALL.md); obrazy kontenerów w [docs/DOCKER.md](../DOCKER.md). Korzystaj z jednej metody instalacji na komputer, aby dwie binaria nie przesłaniały się w `PATH`.

## Szybki start

1. Zrób commit lub stash bieżącej pracy, a następnie uruchom `codewhale` w katalogu głównym repozytorium git.
2. Uruchom `/provider` (lub naciśnij `F3`), aby wybrać dostawcę, wkleić klucz API i wybrać model. Domyślnie używany jest DeepSeek; jeśli nie skonfigurowano klucza, a lokalny serwer Ollama odpowiada, Codewhale zaproponuje jego modele przy pierwszej wiadomości.
3. Podaj zadanie:

```text
Fix the failing tests and explain what changed.
```

Polecenia tylko do odczytu uruchamiają się od razu. Polecenia powłoki wprowadzające zmiany pytają o zgodę: `y` uruchamia raz, `a` zezwala na pasujące polecenia do końca sesji, a `n` odmawia. Użyj `/undo`, aby cofnąć ostatnią turę i jej zmiany w plikach, lub `/help`, aby wyświetlić polecenia i skróty klawiszowe.

## Możliwości

- **Ponad 50 wbudowanych dostawców i modele lokalne:** Anthropic, OpenAI, DeepSeek (domyślnie `deepseek-flash` / V4.1 Flash; [szczegóły](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, dowolny punkt końcowy zgodny z OpenAI oraz lokalne środowiska przez Ollama, vLLM lub SGLang. Przełączaj modele w trakcie sesji poleceniem `/model`, kieruj tury automatycznie przez `/model auto` (`/router`) i sprawdzaj opóźnienia oraz koszty przez `/turn inspect`. Zobacz [Dostawcy](../PROVIDERS.md).
- **Pokwitowania i cofanie:** `/receipts` (lub `codewhale receipts --last` po wyjściu) zapisuje zmienione pliki, polecenia, wywołania sieciowe/MCP, podagentów, zgody i błędy. `/undo` cofa ostatnią turę w jednym kroku; `/restore` przywraca obszar roboczy do wcześniejszej migawki (wymaga `/trust on` lub Full Access). Zobacz [Pokwitowania](../RECEIPTS.md).
- **Podagenci, floty i przepływy pracy:** realizuj wieloturowe cele przez `/goal`, deleguj zadania [podagentom](../SUBAGENTS.md), koordynuj [floty (Fleets)](../FLEET.md) o określonych rolach i modelach lub uruchamiaj [przepływy pracy](../WORKFLOW_AUTHORING.md) zapisane w repozytorium.
- **MCP, umiejętności, haki i wtyczki:** podłączaj [serwery MCP](../MCP.md), [umiejętności](../SKILLS.md), [wtyczki](../PLUGINS.md) i [haki](../HOOKS.md) cyklu życia. Eksperymentalny tryb kodu (`execute_tools`) grupuje wiele wywołań MCP/wtyczek z zachowaniem zwykłych zasad zatwierdzania. Opcjonalny [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) dla macOS pozostaje wyłączony, dopóki jawnie go nie przejrzysz i nie włączysz.
- **Bezpośrednia zgodność:** automatycznie czyta `AGENTS.md`. Włącz `CLAUDE.md`, `.cursorrules` lub `.github/copilot-instructions.md` przez `project_instruction_imports` w [config.example.toml](../../config.example.toml). Zaimportuj ustawienia Claude Code i serwery MCP poleceniem `/import-claude` albo instaluj wtyczki Claude Code (umiejętności, polecenia, profile agentów, MCP) oraz wtyczki DeepSeek Harness (DSH) przez `/plugin install` ([szczegóły](../CLAUDE_PLUGIN_COMPAT.md)).

## Punkty wejścia

| Polecenie | Przeznaczenie |
| --- | --- |
| `codewhale` | Interaktywna aplikacja terminalowa |
| `codewhale exec "…"` | Bezinterfejsowy uruchamiacz pojedynczych zadań dla skryptów i CI ([przewodnik](../INSTALL.md#headless-scripts-ci); `--auto` używa Full Access bez piaskownicy) |
| `codewhale web` | Lokalny klient webowy pod `127.0.0.1` ([przewodnik](../WEB.md)) |
| `codewhale review --pr N` | Doradczy przegląd PR na GitHubie; [konfiguracja bota GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) działa na twoim kluczu |
| `codewhale app-server --http` | Lokalne HTTP [Runtime API](../RUNTIME_API.md) dla wątków, zdarzeń, zatwierdzeń i diffów per tura |
| `codewhale serve --acp` | Agent Client Protocol przez stdio dla edytorów takich jak Zed |

Wszystkie punkty wejścia dzielą jeden silnik w języku Rust ([architektura](../ARCHITECTURE.md)). Utrzymywane przez społeczność [rozszerzenie dla VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) ([kod źródłowy](https://github.com/HengQuWorld/CodeWhale-VSCode)) łączy się z tym samym silnikiem. Aktualny status wydań, wersji desktopowej i chmury znajdziesz w [informacjach o najnowszym wydaniu](https://github.com/codewhale-hq/Codewhale/releases/latest) oraz w [planie rozwoju](../ROADMAP.md).

## Uprawnienia, piaskownica i prywatność

`Shift+Tab` przełącza tryby uprawnień: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` służy wyłącznie do autouzupełniania). **Accept Edits** (domyślny) stosuje edycje w repozytorium git i pyta przed poleceniami. **Ask** pyta przed każdą edycją i poleceniem. **Plan** działa tylko do odczytu. **Auto-Review** używa reguł i modelu recenzującego, aby zatwierdzać rutynowe wywołania i blokować ryzykowne. **Full Access** pomija pytania i wyłącza piaskownicę OS (reguły repozytorium i zasady zarządzane nadal obowiązują). Zobacz [Tryby](../MODES.md).

W trybach Ask, Accept Edits i Auto-Review polecenia działają w piaskownicy systemu operacyjnego tam, gdzie jest dostępna: Seatbelt na macOS i bubblewrap na Linuxie (Windows nie ma jeszcze piaskownicy OS). Polecenia w piaskownicy i sesje terminala nie mogą odczytywać ścieżek z poświadczeniami, takich jak `~/.ssh`, `~/.aws` czy pliki `.env`. Uruchom `/preview-request`, aby obejrzeć zanonimizowane żądanie wychodzące przed wysłaniem.

Klucze API dodane przez `/provider` zostają na twoim komputerze, a modele lokalne nie wymagają klucza. `/cost` i stopka śledzą koszt sesji, gdy cennik jest znany (nieznana cena jest pokazywana jako nieznana, nigdy jako darmowa).

Zbiorcza telemetria jest domyślnie włączona (wersja, system operacyjny, długość i wynik sesji, identyfikatory wbudowanych dostawców, liczniki funkcji/błędów; losowy identyfikator instalacji zmieniany co 90 dni; nigdy kod, prompty, rozmowy, nazwy modeli, ścieżki ani poświadczenia). Wyłącz ją poleceniem:

```bash
codewhale config set telemetry false
```

Ustaw `CODEWHALE_NO_UPDATE_CHECK=1`, aby pominąć sprawdzanie wydań na GitHubie przy starcie. Szczegóły: [kolejność autoryzacji](../AUTHORIZATION_ORDER.md), [piaskownica](../SANDBOX.md) i [telemetria](../TELEMETRY.md).

## Dokumentacja

Przewodniki podążają za gałęzią `main` i mogą opisywać zmiany nieobecne jeszcze w najnowszym wydaniu.

| Zacznij tutaj | Dokumentacja referencyjna |
| --- | --- |
| [Przewodnik użytkownika](../GUIDE.md) | [Konfiguracja](../CONFIGURATION.md) |
| [Instalacja](../INSTALL.md) | [Architektura](../ARCHITECTURE.md) |
| [Dostawcy i modele lokalne](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Tryby i uprawnienia](../MODES.md) | [Tworzenie wtyczek](../PLUGIN_AUTHORING.md) |
| [Skróty klawiszowe](../KEYBINDINGS.md) | [Pokwitowania](../RECEIPTS.md) |
| [Przegląd PR na GitHubie](../GITHUB_ACTION.md) | [Cała dokumentacja](../README.md) |

## Współtworzenie i licencja

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Ściana współtwórców Codewhale"></a>

[Ściana współtwórców](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) obejmuje kod, współtworzone commity, przeniesione PR-y i uznane zgłoszenia. Zgłoszenia błędów i propozycje funkcji kieruj do [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), pytania do [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), a zasady PR i układ projektu znajdziesz w [CONTRIBUTING.md](../../CONTRIBUTING.md) ([struktura projektu](../../CONTRIBUTING.md#project-structure)). Dołącz na [Discordzie](https://discord.gg/37gfS3ksug) lub dodaj `hunterbown` na WeChat, aby dołączyć do grupy Whale Brothers.

Codewhale zaczął jako `deepseek-tui` i zachowuje zgodność wsteczną z jego konfiguracją oraz sesjami. Projekt jest utrzymywany niezależnie i nie jest powiązany z DeepSeek ani żadnym innym dostawcą modeli.

[MIT](../../LICENSE). Informacje o komponentach zewnętrznych: [dołączone źródła](../../THIRD_PARTY_NOTICES.md) i [zaadaptowane projekty](../THIRD_PARTY_NOTICES.md).
