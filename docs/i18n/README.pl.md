<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agent programistyczny open source do terminala, który działa z dowolnym modelem.**

Dajesz Codewhale zadanie, a on czyta repozytorium, edytuje pliki i uruchamia
polecenia, dopóki zadanie nie zostanie wykonane albo nie będzie potrzebował
informacji od Ciebie. Działa z ponad 40 wbudowanymi dostawcami (domyślnie z
DeepSeek V4.1 Flash) oraz z modelami uruchomionymi na Twoim własnym komputerze.
`/receipts` wyświetla listę plików, które zmienił, poleceń, które uruchomił,
oraz wywołań internetowych i MCP, które wykonał.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Strona](https://codewhale.net) · [Dokumentacja](../README.md) · [Dziennik zmian](../../CHANGELOG.md) · [Współtworzenie](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Sesja Codewhale w terminalu" width="760">

<sub>Wersja rozwojowa 0.10.2 ze skryptowanym modelem testowym, z uprawnieniem Full Access, w jednorazowym obszarze roboczym. Edycja, trzy testy i przegląd wykonany przez subagenta odbyły się naprawdę.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — o 30% więcej korzystania z DeepSeek v4.1 Flash" width="760">
  </a>
</p>

[Zobacz ofertę Codewhale × OrcaRouter](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale otrzymuje prowizję za polecenia od OrcaRouter.

## Instalacja

macOS i Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Instalator umieszcza w `~/.local/bin` plik `codewhale` i jego krótki alias
`codew`, po weryfikacji sum kontrolnych. Jeśli powłoka zgłosi potem
`command not found`, uruchom linię z PATH, którą wypisuje instalator
([szczegóły](../INSTALL.md#put-it-on-your-path)). Do aktualizacji służy
`codewhale update`. Te polecenia instalują najnowsze wydanie. Ta strona opisuje
gałąź `main`, więc funkcja oznaczona wersją pojawi się w tym wydaniu;
[dziennik zmian](../../CHANGELOG.md) pokazuje, co wnosi każde wydanie.

<details>
<summary><b>Windows, npm, Cargo i inne sposoby instalacji</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Gotowe pliki binarne są dostępne dla Linuksa, macOS i Windows na x64 i arm64.
Nix, Homebrew w Linuksie, Android/Termux (wersja zapoznawcza) i ręczne
pobieranie opisuje [przewodnik instalacji](../INSTALL.md), a kontenery
[Docker](../DOCKER.md). Na jednej maszynie używaj jednej metody instalacji;
inaczej w `PATH` znajdą się dwie kopie i zadziała ta, która jest pierwsza.

</details>

## Szybki start

1. Zrób commit lub stash swoich zmian, a następnie uruchom `codewhale` w
   katalogu głównym repozytorium git. Domyślnie wprowadza tam zmiany w plikach
   bez pytania i pokazuje Ci diff.
2. Uruchom `/provider` (lub naciśnij `F3`), wybierz dostawcę, wklej jego klucz
   API i wybierz model. Domyślnym dostawcą jest DeepSeek. Jeśli nie masz
   klucza, wybierz zamiast tego lokalne środowisko. Jeśli żaden dostawca nie
   jest skonfigurowany, a Ollama już udostępnia model czatu, Codewhale
   automatycznie go użyje; od 0.10.2 proponuje go, gdy wyślesz pierwszą
   wiadomość.
3. Wpisz zadanie:

```text
Fix the failing tests and explain what changed.
```

Polecenia tylko do odczytu uruchamiają się od razu. Przed każdym innym
poleceniem powłoki Codewhale najpierw pyta: `y` uruchamia je jednorazowo, `a`
zezwala na podobne polecenia do końca rozmowy, `n` odmawia. `/undo` cofa Twoje
ostatnie zlecenie i zmiany, które wprowadziło w plikach. `/help` wyświetla polecenia i skróty klawiszowe.

W skryptach i CI `codewhale exec` wykonuje jedno zadanie bez interfejsu.
Domyślnie `--auto` używa Full Access: narzędzia działają bez pytania, polecenia
powłoki działają bez piaskownicy Codewhale, a tryb zaufania pozwala narzędziom
plikowym zapisywać pliki poza obszarem roboczym. Dodaj
`--sandbox workspace-write`, aby zachować piaskownicę powłoki i ograniczyć
narzędzia plikowe do obszaru roboczego, z wyłączonym trybem zaufania:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Tryby i uprawnienia

Świeża instalacja startuje w trybie Work z uprawnieniem Ask.

| | Przełączanie | Opcje |
| --- | --- | --- |
| Tryb | `Tab` (gdy pole wprowadzania jest puste) lub `/mode` | **Plan** czyta i przeszukuje, ale niczego nie zmienia. **Work** edytuje pliki i uruchamia polecenia. **Operate** ma ten sam dostęp co Work, traktuje Twoje zlecenie jako cel i działa dalej, dopóki cel nie zostanie zweryfikowany albo nie będzie potrzebował informacji od Ciebie. |
| Uprawnienie | `Shift+Tab` | **Ask** wprowadza zmiany w plikach Twojego repozytorium bez pytania i pyta przed poleceniami powłoki, które nie są tylko do odczytu. **Auto-Review** pozwala regułom i modelowi recenzującemu zatwierdzać rutynowe wywołania, a wywołania wysokiego ryzyka odrzuca; aby zatwierdzać je samodzielnie, przełącz na Ask. **Full Access** nigdy nie pyta i wyłącza piaskownicę Codewhale; reguły repozytorium i polityka zarządzana nadal obowiązują. |

Szczegóły opisuje [przewodnik po trybach i uprawnieniach](../MODES.md).

## Co potrafi

### Nowości w 0.10.2

- `/pet on` zamienia terminal w animowanego wieloryba, który pokazuje, czym
  zajmuje się agent, a pole wiadomości pozostaje przy tym aktywne. Jego 980
  cząsteczek układa się w nazwę bieżącego kroku po angielsku, chińsku lub
  japońsku.
- `/workbar terminal` pokazuje powłoki uruchomione przez agenta na bieżąco, w
  trakcie ich działania. Możesz pisać w każdej z nich albo nacisnąć `Ctrl+N`,
  aby otworzyć własną powłokę (macOS i Linux).
- `/undo` cofa całe Twoje ostatnie zlecenie w jednym kroku, łącznie ze
  wszystkimi zmienionymi przez nie plikami, a `/diff` działa w folderach bez
  Gita.
- Gdy plan jest gotowy, Codewhale pyta, czy rozpocząć Work z Ask lub
  Auto-Review, czy dalej planować.
- `/color` nadaje każdemu czatowi własny kolor pola wiadomości, zapisywany
  razem z sesją.
- `web_search` może korzystać z nowego Search API od DeepSeek: ustaw
  `[search] provider = "deepseek"` i podaj klucz DeepSeek.
- Poza Full Access polecenie `curl` lub `wget`, które wysłałoby dane
  uwierzytelniające, nigdy nie uruchomi się bez Twojej zgody, a polecenie
  ukrywające niewidoczne znaki Unicode zostaje odrzucone. `write` nie zastępuje
  już pliku, którego agent nie przeczytał, ani pliku, który zmienił się po
  przeczytaniu.

### Modele

Wbudowanych jest ponad 40 dostawców, w tym Anthropic, OpenAI, DeepSeek,
Google, Mistral, Moonshot, OpenRouter i xAI. Działa każdy punkt końcowy zgodny
z OpenAI, a także modele lokalne przez Ollama, vLLM lub SGLang. Domyślnym
modelem jest `deepseek-flash` od DeepSeek, który DeepSeek 2026-10-17 przełączył
na DeepSeek-V4.1-Flash-1017; 0.10.1 obsługuje go bez aktualizacji
([szczegóły](../PROVIDERS.md#deepseek-v41-flash)).

`/model` przełącza model między turami, a `/model auto` wybiera model dla
każdej tury (sposób wyboru ustawia `/router`). `/turn inspect` pokazuje, jakiego
modelu użyła tura, ile kosztowała i jak długo trwała. `codewhale auth chatgpt`
pozwala korzystać z subskrypcji ChatGPT zamiast klucza API (eksperymentalnie).
Zobacz [Dostawcy](../PROVIDERS.md).

### Pokwitowania i cofanie

`/receipts` (albo `codewhale receipts --last` po wyjściu z programu) wyświetla
listę zmienionych plików, uruchomionych poleceń, wywołań internetowych i MCP,
uruchomionych agentów, zatwierdzeń wraz z tym, kto ich udzielił, oraz
niepowodzeń. `/undo` cofa Twoje ostatnie zlecenie i zmienione przez nie pliki w jednym kroku, bez trybu zaufania. `/restore`
przywraca cały obszar roboczy do wcześniejszej migawki; ponieważ nadpisuje to
późniejsze zmiany, wymaga `/trust on` lub Full Access. Zobacz
[Pokwitowania](../RECEIPTS.md).

Uruchomienia exec z dostępnymi narzędziami są zapisywane w każdym formacie
wyjściowym, więc `codewhale receipts --last` działa po `exec --auto`, także
z `--json`. Uruchomienia bez narzędzi z wyjściem tekstowym lub `--json` są
zapisywane tylko przy wznawianiu lub gdy zaczyna się kompakcja rozmowy; użyj
`--output-format stream-json`, aby zawsze je zapisywać na potrzeby pokwitowań
lub `exec --continue`.

### Dłuższa praca

`/goal` ustawia cel, który obowiązuje przez kolejne tury. Codewhale może
podzielić pracę między [subagentów](../SUBAGENTS.md), uruchomić
[flotę](../FLEET.md) agentów albo wykonać
[przepływ pracy](../WORKFLOW_AUTHORING.md) zapisany w repozytorium.

### Przejście z innego agenta

Codewhale sam czyta `AGENTS.md`. Pliki instrukcji napisane dla innych narzędzi,
takie jak `CLAUDE.md`, `.cursorrules` czy `.github/copilot-instructions.md`, są
wczytywane tylko wtedy, gdy włączysz to w konfiguracji ustawieniem
`project_instruction_imports = ["claude"]` (lub `"cursor"`, `"cline"`,
`"windsurf"`, `"copilot"`, `"all"`); zobacz
[config.example.toml](../../config.example.toml). Codewhale ostrzega, gdy znajdzie
taki plik, którego nie wczytał.

`/import-claude` przygotowuje import Twoich ustawień Claude Code i serwerów MCP
do przejrzenia. `/plugin install` przyjmuje wtyczki Claude Code zawierające
umiejętności, polecenia, profile agentów i serwery MCP, a odrzuca wtyczki
wymagające hooków lub serwerów LSP
([szczegóły](../CLAUDE_PLUGIN_COMPAT.md)).

### Rozszerzenia

Podłączaj [serwery MCP](../MCP.md), dodawaj [umiejętności](../SKILLS.md) i
[wtyczki](../PLUGINS.md) oraz uruchamiaj [hooki](../HOOKS.md) na
zdarzeniach sesji i narzędzi. Code mode (eksperymentalny, domyślnie włączony)
pozwala modelowi napisać jeden program `execute_tools`, który wywołuje kilka
narzędzi MCP i wtyczek; każde wywołanie nadal podlega Twoim regułom
zatwierdzania i ma własne pokwitowanie. Osobno instalowana wtyczka Computer Use
(eksperymentalna, tylko macOS) potrafi obserwować i obsługiwać inne aplikacje.
Pozostaje wyłączona, dopóki nie sprawdzisz jej dostępu i jej nie włączysz
([przewodnik](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Sposoby uruchamiania

| Polecenie | Co robi |
| --- | --- |
| `codewhale` | Interaktywna aplikacja terminalowa |
| `codewhale exec "…"` | Jedno zadanie bez interfejsu. Bez dodatkowych flag odpowiada bez użycia narzędzi; `--auto` domyślnie włącza narzędzia z Full Access, `--sandbox workspace-write` je ogranicza, a `--output-format stream-json` strumieniuje zdarzenia jako JSON |
| `codewhale web` | Lokalny interfejs przeglądarkowy pod `127.0.0.1` ([przewodnik](../WEB.md)) |
| `codewhale review --pr N` | Doradczy przegląd pull requesta na GitHubie. [Konfiguracja bota GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) generuje przypięty do wersji workflow do automatycznych przeglądów i `@codewhale review`, z użyciem własnego klucza modelu i opcjonalnej tożsamości App |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md), lokalne HTTP API dla wątków, zdarzeń, zatwierdzeń i zmian w plikach z każdej tury |
| `codewhale serve --acp` | Agent Client Protocol przez stdio, dla edytorów takich jak Zed |

Aplikacja terminalowa, `exec`, `web` i Runtime API korzystają z jednego silnika
([architektura](../ARCHITECTURE.md)). Utrzymywane przez społeczność
[rozszerzenie VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
łączy się z nim z panelu bocznego ([kod źródłowy](https://github.com/HengQuWorld/CodeWhale-VSCode)).
Natywna aplikacja desktopowa oparta na Runtime API jest w trakcie tworzenia;
nie ma jeszcze publicznej wersji do pobrania.

## Koszty i konta

Codewhale jest darmowy i udostępniany na licencji MIT. Z własnym kluczem API
płacisz bezpośrednio dostawcy; modele lokalne nie wymagają klucza. Stopka i
`/cost` pokazują koszt sesji, gdy cena jest znana; nieznana cena jest
pokazywana jako nieznana, nigdy jako darmowa.

Konto nie jest wymagane. Klucze dodane przez `/provider` pozostają na Twoim
komputerze.

## Bezpieczeństwo i prywatność

Przy uprawnieniach Ask i Auto-Review polecenia działają w piaskownicy
systemowej, jeśli system ją ma: Seatbelt w macOS i bubblewrap w Linuksie, gdy
jest zainstalowany. Windows nie ma jeszcze piaskownicy systemowej, a Full
Access wyłącza piaskownicę. W piaskownicy polecenia powłoki nie mogą odczytywać
magazynów danych uwierzytelniających, takich jak `~/.ssh`, `~/.aws` i pliki
`.env`; od 0.10.2 ta sama reguła obejmuje sesje terminala i więcej narzędzi
plikowych. `/preview-request` pokazuje zredagowany podgląd następnego
zapytania, nie wysyłając go.

Telemetria jest domyślnie włączona. Wysyła zagregowane liczniki (wersja i
platforma, długość i wynik sesji, używani wbudowani dostawcy, liczniki użycia
funkcji i błędów) wraz z losowym identyfikatorem instalacji, który zmienia się
co 90 dni; nigdy nie wysyła rozmów, kodu, promptów, nazw modeli, nazw plików
ani repozytoriów, ani danych uwierzytelniających.
`codewhale config set telemetry false` ją wyłącza. Przy uruchomieniu Codewhale
sprawdza też na GitHubie, czy jest nowe wydanie; ustaw
`CODEWHALE_NO_UPDATE_CHECK=1`, aby to pominąć. Więcej o
[kolejności autoryzacji](../AUTHORIZATION_ORDER.md),
[piaskownicy](../SANDBOX.md) i [telemetrii](../TELEMETRY.md).

## Dokumentacja

| Zacznij tutaj | Materiały referencyjne |
| --- | --- |
| [Instalacja](../INSTALL.md) | [Konfiguracja](../CONFIGURATION.md) |
| [Dostawcy i modele lokalne](../PROVIDERS.md) | [Architektura](../ARCHITECTURE.md) |
| [Tryby i uprawnienia](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [Skróty klawiszowe](../KEYBINDINGS.md) | [Tworzenie wtyczek](../PLUGIN_AUTHORING.md) |
| [Przegląd PR na GitHubie](../GITHUB_ACTION.md) | [Cała dokumentacja](../README.md) |

## Społeczność

Jeśli brakuje dostawcy albo jakiś przepływ pracy jest niewygodny,
[otwórz issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) lub
[wyślij pull request](../../CONTRIBUTING.md). Autorzy zachowują uznanie za pracę,
która zostanie włączona, a [struktura repozytorium](../../CONTRIBUTING.md#project-structure)
pokazuje, gdzie co się znajduje.

Rozmawiaj na [Discordzie](https://discord.gg/37gfS3ksug) albo dodaj opiekuna
projektu, Huntera, na WeChacie (`hunterbown`) i poproś o dołączenie do grupy
Whale Brothers.

## Historia i licencja

Codewhale zaczynał jako `deepseek-tui` i nadal czyta konfigurację oraz sesje
tego projektu. Jest utrzymywany niezależnie i nie jest powiązany z DeepSeek ani
z żadnym innym dostawcą modeli. Dziękujemy
[wszystkim, którzy wnieśli swój wkład](../CONTRIBUTORS.md).

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). Kod zaadaptowany z innych projektów open source wymieniono w
[informacjach o komponentach zewnętrznych](../THIRD_PARTY_NOTICES.md).
