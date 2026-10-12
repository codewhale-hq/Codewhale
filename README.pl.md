<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Open-source'owy agent programistyczny, który działa z dowolnym modelem.**

Codewhale czyta Twój projekt, edytuje pliki, uruchamia polecenia i sprawdza
własną pracę — w Twoim terminalu, z modelem hostowanym lub lokalnym, który
wybierzesz.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Strona](https://codewhale.net) · [Dokumentacja](docs/README.md) · [Dziennik zmian](CHANGELOG.md) · [Współtworzenie](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Sesja Codewhale w terminalu" width="760">

<sub>Prawdziwy zrzut terminala Codewhale ze skonfigurowanej lokalnej sesji demonstracyjnej.</sub>

</div>

## Instalacja

macOS i Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Instalator pobiera pliki binarne z weryfikacją sum kontrolnych do
`~/.local/bin`. Jeśli `codewhale` zgłasza potem "command not found", uruchom
jedną linię z PATH, którą wypisuje instalator, albo zajrzyj do
[Dodawanie do PATH](docs/INSTALL.md#put-it-on-your-path). Aktualizować można w
dowolnej chwili poleceniem `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo i inne sposoby</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew w Linuksie, Android/Termux, ręczne pobieranie z
weryfikacją sum kontrolnych i opcjonalne lustro CNB opisuje
[przewodnik instalacji](docs/INSTALL.md). Wybierz jeden sposób: kilka instalacji
na jednej maszynie zaczyna się kłócić o `PATH`.

</details>

## Szybki start

**Klucze API dostawców w jednym miejscu.** [Utwórz konto Codewhale](https://app.codewhale.net/register)
lub uruchom `codewhale login`, aby się zalogować. Zapisuj i aktualizuj klucze API na swoim koncie,
a następnie korzystaj z nich przez trasę modeli Codewhale na urządzeniach, na których się zalogowano.
[Skonfiguruj klucze konta](docs/CONFIGURATION.md#account-provider-keys).
Logowanie nie przesyła istniejących kluczy lokalnych; korzystanie lokalne jest nadal możliwe bez konta.

1. **Otwórz projekt.** Uruchom `codewhale` w folderze, z którym chcesz pracować.
2. **Połącz model.** Uruchom `/provider` (lub naciśnij `F3`), aby dodać klucz
   hostowanego dostawcy albo wybrać lokalne środowisko. Jeśli Ollama już działa
   z modelem czatu, Codewhale sam się na niego przełączy. Model zmienisz
   poleceniem `/model`.
3. **Daj mu konkretne zadanie.**

```text
Fix the failing tests and explain what changed.
```

To samo zadanie można uruchomić bez interfejsu, ze skryptu lub zadania CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Polecenia i skróty klawiszowe pokaże `/help`.

## Sposoby uruchamiania

Każdy klient korzysta z tego samego lokalnego Runtime Codewhale, więc sesje,
narzędzia i uprawnienia zachowują się wszędzie tak samo.

| Polecenie | Co robi |
| --- | --- |
| `codewhale` | Interaktywny interfejs terminalowy |
| `codewhale exec "…"` | Jedna tura bez interfejsu ze skryptu lub CI; `--auto` zezwala na narzędzia plików i powłoki, `--output-format stream-json` strumieniuje JSON |
| `codewhale web` | Wbudowany [lokalny klient przeglądarkowy](docs/WEB.md) pod `127.0.0.1` |
| `codewhale review --pr N` | Doradczy [przegląd pull requesta](docs/GITHUB_ACTION.md); publikowanie trzeba włączyć |
| Runtime API | [Lokalne HTTP API](docs/RUNTIME_API.md) dla wątków, zdarzeń i zatwierdzeń |

Natywna aplikacja desktopowa (GPUI) powstaje jako główny klient dla zalogowanych
użytkowników; jej dostępność opisuje
[strona produktu](https://codewhale.net/en/product). Utrzymywane przez
społeczność [rozszerzenie VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
łączy się z tym samym Runtime z panelu bocznego
([kod źródłowy](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Co potrafi

- **Dowolny model, bez uwiązania.** Ponad 40 wbudowanych tras do dostawców —
  Anthropic, DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI i inne
  — a do tego dowolny punkt końcowy zgodny z OpenAI oraz modele lokalne przez
  Ollama, vLLM lub SGLang. [Dostawcy](docs/PROVIDERS.md)
- **Kontrola zostaje po Twojej stronie.** Tryb Plan bada projekt, niczego nie
  zmieniając; Work i Operate wprowadzają zmiany. Postawy zatwierdzania
  decydują, kiedy wywołanie narzędzia wymaga Twojej zgody, `/undo` i `/restore`
  przywracają zmiany w obszarze roboczym, a `/receipts` wylicza każdy plik,
  polecenie i zatwierdzenie w sesji. [Tryby](docs/MODES.md) ·
  [Pokwitowania](docs/RECEIPTS.md)
- **Do długich zadań.** Ustaw trwały `/goal`, deleguj ograniczone zadania
  [subagentom](docs/SUBAGENTS.md), uruchamiaj nadzorowane
  [zespoły agentów](docs/FLEET.md) z kontrolą przed wydatkami albo opisuj je jako
  wersjonowane [przepływy pracy](docs/WORKFLOW_AUTHORING.md).
- **Rozszerzaj to, czego już używasz.** Podłączaj [serwery MCP](docs/MCP.md),
  instaluj [umiejętności](docs/SKILLS.md) i [wtyczki](docs/PLUGINS.md), uruchamiaj
  [hooki](docs/HOOKS.md) na zdarzeniach sesji i narzędzi oraz ładuj istniejące
  [wtyczki Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md).
- **Computer Use.** Dołączona wtyczka dodaje narzędzia do obserwowania i
  obsługi innych aplikacji. Przed użyciem sprawdź jej dostęp i włącz ją.
  [Przewodnik](crates/tui/plugins/computer-use/README.md)

## Tryby i uprawnienia

| | Wybierasz | Opcje |
| --- | --- | --- |
| **Tryb** — co robi agent | `Tab` lub `/mode` | Plan (badanie, bez zmian) · Work (edycja i uruchamianie) · Operate (prowadzi cel przez zaplanowane, weryfikowane kroki) |
| **Postawa** — kiedy pyta wcześniej | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access nadal przestrzega twardych granic polityki. Każdą opcję wyjaśnia
[przewodnik po trybach i uprawnieniach](docs/MODES.md).

## Bezpieczeństwo

Codewhale działa na Twoim komputerze z dostępem, który mu przyznasz. Postawy
zatwierdzania i reguły repozytorium ograniczają to, co agent może zrobić, a
polecenia działają wewnątrz piaskownicy systemowej tam, gdzie jest obsługiwana
(Seatbelt w macOS; bubblewrap w Linuksie, gdy jest zainstalowany i działa).
`/preview-request` pokazuje dokładne zapytanie z zredagowanymi sekretami, zanim
cokolwiek zostanie wysłane. Nieznane ceny modeli pozostają nieznane, zamiast być
podawane jako darmowe.

Zobacz [kolejność autoryzacji](docs/AUTHORIZATION_ORDER.md),
[piaskownicę](docs/SANDBOX.md) i [telemetrię](docs/TELEMETRY.md) — liczniki
użycia są domyślnie włączone, a `codewhale config set telemetry false` je
wyłącza.

## Dokumentacja

| Zacznij tutaj | Zgłęb temat |
| --- | --- |
| [Instalacja](docs/INSTALL.md) | [Konfiguracja](docs/CONFIGURATION.md) |
| [Dostawcy i modele lokalne](docs/PROVIDERS.md) | [Architektura](docs/ARCHITECTURE.md) |
| [Tryby i uprawnienia](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Skróty klawiszowe](docs/KEYBINDINGS.md) | [Tworzenie wtyczek](docs/PLUGIN_AUTHORING.md) |
| [Przegląd PR na GitHubie](docs/GITHUB_ACTION.md) | [Cała dokumentacja](docs/README.md) |

## Społeczność

Zgłoszenia błędów, pomysły i pull requesty są mile widziane — niezależnie od
tego, czy używasz Codewhale od miesięcy, czy próbujesz go po raz pierwszy. Jeśli
brakuje dostawcy albo jakiś przepływ jest niewygodny,
[otwórz issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) lub
[wyślij pull request](CONTRIBUTING.md). Pierwsze wkłady są mile widziane, a
autorzy zachowują uznanie za wprowadzoną pracę. Dobrym punktem wyjścia jest
[struktura repozytorium](CONTRIBUTING.md#project-structure).

Dołącz do [Discorda](https://discord.gg/37gfS3ksug) albo dodaj Huntera na
WeChacie (`hunterbown`) i poproś o przyjęcie do grupy Whale Brothers.

## Historia i licencja

Codewhale zaczynał jako `deepseek-tui` i nadal czyta konfigurację oraz sesje tego
projektu. Dziś jest niezależny od dostawców, utrzymywany samodzielnie i nie jest
powiązany z żadnym dostawcą modeli. Dziękujemy
[wszystkim współtwórcom](docs/CONTRIBUTORS.md) oraz społecznościom open source,
które pomogły mu urosnąć.

[MIT](LICENSE). Fragmenty zaadaptowane z innych projektów open source opisano w
[informacjach o komponentach zewnętrznych](docs/THIRD_PARTY_NOTICES.md).
