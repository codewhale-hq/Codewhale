<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>Model może należeć do kogokolwiek. Agent powinien być twój.</h1>

Codewhale to otwartoźródłowy agent programistyczny do twojego terminala, napisany w Rust. Działa z dowolnym modelem, potrafi podzielić jedno zadanie między kilku agentów, a każdą jego linię możesz przeczytać.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

To polecenie instaluje najnowsze wydanie w systemie macOS lub Linux i nie wymaga konta. [Windows, npm i Cargo](#instalacja) są niżej.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Strona](https://codewhale.net) · [Dokumentacja](../README.md) · [Lista zmian](../../CHANGELOG.md) · [Współtworzenie](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>To tłumaczenie jest roboczym tekstem maszynowym i nie zostało jeszcze sprawdzone przez osobę, dla której polski jest językiem ojczystym.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale w terminalu po zadaniu demonstracyjnym. Informuje, że dodał flagę --json do export.py, że wszystkie 3 lokalne testy przeszły i że delegowany przegląd nie znalazł problemów. Lista zadań poniżej pokazuje trzy ukończone kroki, a wiersz stanu pokazuje Full Access i lokalnego dostawcę website-demo.">

<sub>Kompilacja deweloperska wersji 0.10.3, jeszcze niewydanej, ze skryptowanym modelem testowym, w trybie Full Access w jednorazowym obszarze roboczym. Edycja, trzy testy i przegląd przez podagenta zostały naprawdę wykonane.</sub>

</div>

## Instalacja

Powyższe polecenie umieszcza sprawdzony sumą kontrolną `codewhale` i jego krótki alias `codew` w `~/.local/bin`. Jeśli potem powłoka zgłosi `command not found`, wykonaj wskazówkę o PATH, którą wypisuje instalator ([szczegóły](../INSTALL.md#put-it-on-your-path)). `codewhale update` go aktualizuje.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Gotowe pliki binarne są dostępne dla systemów Linux, macOS i Windows na x64 i arm64. Scoop, Nix, Homebrew w systemie Linux, Android i Termux (wersja zapoznawcza) oraz ręczne pobieranie opisuje [przewodnik instalacji](../INSTALL.md), a kontenery — [Docker](../DOCKER.md). Używaj jednej metody instalacji na komputerze; w przeciwnym razie w `PATH` znajdą się dwie kopie i wygra ta znaleziona jako pierwsza.

## Szybki start

1. Zatwierdź lub odłóż do stash swoją pracę, a potem uruchom `codewhale` w katalogu głównym repozytorium git. Domyślnie stosuje zmiany plików w repozytorium bez pytania i pyta przed każdym poleceniem powłoki, które nie tylko czyta.
2. Uruchom `/provider` (lub naciśnij `F3`), wybierz dostawcę, wklej jego klucz API i wybierz model. Domyślnym dostawcą jest DeepSeek. Przy pierwszym uruchomieniu bez klucza Codewhale pyta o dostawcę, gdy wyślesz pierwszą wiadomość, i proponuje model lokalny, gdy odpowie lokalny serwer Ollama.
3. Wpisz zadanie:

```text
Fix the failing tests and explain what changed.
```

Polecenia tylko do odczytu uruchamiają się od razu. Przy każdym innym poleceniu powłoki Codewhale najpierw pyta: `y` uruchamia je raz, `a` zezwala na podobne polecenia do końca rozmowy, a `n` odmawia. `/undo` cofa twoją ostatnią prośbę i jej zmiany w plikach, a `/help` wyświetla polecenia i klawisze.

W skryptach i CI `codewhale exec` wykonuje jedno zadanie bez interfejsu. Przeczytaj o [uruchomieniach bez interfejsu](../INSTALL.md#headless-scripts-ci), zanim dasz mu narzędzia przez `--auto`, ponieważ `--auto` używa Full Access, jeśli nie dodasz piaskownicy.

## Co robi

Daj Codewhale zadanie, a przeczyta repozytorium, zmieni pliki i uruchomi polecenia, aż zadanie będzie gotowe albo będzie potrzebować twojej odpowiedzi. [Informacje o najnowszym wydaniu](https://github.com/codewhale-hq/Codewhale/releases/latest) wymieniają najnowsze zmiany, a [dziennik zmian](../../CHANGELOG.md) obejmuje każdą wersję.

### Dowolny model

Wbudowano ponad 40 dostawców, w tym Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter i xAI. Działa każdy punkt końcowy zgodny z OpenAI, a także modele lokalne przez Ollama, vLLM lub SGLang. Domyślnym modelem jest `deepseek-flash` od DeepSeek, za którym stoi teraz DeepSeek V4.1 Flash ([szczegóły](../PROVIDERS.md#deepseek-v41-flash)).

`/model` zmienia model między turami, a `/model auto` wybiera model na każdą turę (sposób ustawia `/router`). `/turn inspect` pokazuje, jakiego modelu użyła tura, ile kosztowała i jak długo trwała. Zobacz [Dostawcy](../PROVIDERS.md).

### Potwierdzenia i cofanie

`/receipts` lub `codewhale receipts --last` po wyjściu wymienia pliki zmienione w sesji, uruchomione polecenia, wywołania sieciowe i MCP, uruchomionych agentów, każde zatwierdzenie i osobę, która je wydała, oraz to, co się nie udało. `/undo` w jednym kroku cofa twoją ostatnią prośbę i zmienione przez nią pliki. `/restore` przywraca cały obszar roboczy do wcześniejszej migawki, a ponieważ nadpisuje późniejsze zmiany, wymaga `/trust on` lub Full Access. Zobacz [Potwierdzenia](../RECEIPTS.md).

### Kilku agentów przy jednym zadaniu

`/goal` ustawia cel, który trwa przez kolejne tury. Codewhale może podzielić pracę między [podagentów](../SUBAGENTS.md), uruchomić [Fleet](../FLEET.md) agentów z własnymi rolami i modelami albo wykonać [przepływ pracy](../WORKFLOW_AUTHORING.md) zapisany w repozytorium.

### Rozszerzenia

Podłączaj [serwery MCP](../MCP.md), dodawaj [umiejętności](../SKILLS.md) i [wtyczki](../PLUGINS.md) oraz uruchamiaj [hooki](../HOOKS.md) przy zdarzeniach sesji i narzędzi. Tryb kodu (eksperymentalny, domyślnie włączony) pozwala modelowi napisać jeden program `execute_tools`, który wywołuje kilka narzędzi MCP i wtyczek; każde wywołanie nadal podlega twoim regułom zatwierdzania i zachowuje potwierdzenie. Wtyczka Computer Use dołączona do v0.10.2 (eksperymentalna, tylko macOS) może obserwować inne aplikacje i nimi sterować; pozostaje wyłączona, dopóki nie sprawdzisz jej dostępu i jej nie włączysz ([przewodnik](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). Nadchodzi w 0.10.3: wtyczka będzie wydawana [osobno](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Sposoby uruchamiania

| Polecenie | Co robi |
| --- | --- |
| `codewhale` | Interaktywna aplikacja terminalowa |
| `codewhale exec "…"` | Jedno zadanie bez interfejsu, dla skryptów i CI ([uruchomienia bez interfejsu](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Lokalny klient webowy na `127.0.0.1` ([przewodnik](../WEB.md)) |
| `codewhale review --pr N` | Doradczy przegląd pull requesta na GitHubie; [konfiguracja bota GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) uruchamia go z twoim własnym kluczem modelu |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md), lokalne API HTTP dla wątków, zdarzeń, zatwierdzeń i zmian plików w każdej turze |
| `codewhale serve --acp` | Agent Client Protocol przez stdio, dla edytorów takich jak Zed |

Aplikacja terminalowa, `exec`, `web` i Runtime API korzystają z jednego silnika ([architektura](../ARCHITECTURE.md)). Utrzymywane przez społeczność [rozszerzenie VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) łączy się z nim z paska bocznego ([kod źródłowy](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Jak dba o bezpieczeństwo

Dwa przełączniki określają, ile Codewhale może zrobić sam. Gdy pole wpisywania jest puste, `Tab` zmienia tryb: **Plan** czyta i szuka, ale niczego nie zmienia, **Work** edytuje pliki i uruchamia polecenia, a **Operate** ma te same uprawnienia co Work i działa, dopóki cel nie zostanie zweryfikowany albo nie będzie potrzebować ciebie. `Shift+Tab` zmienia uprawnienie. **Ask**, domyślne, stosuje zmiany plików w twoim repozytorium bez pytania i pyta przed poleceniami powłoki, które nie tylko czytają. **Auto-Review** pozwala regułom i modelowi recenzującemu zatwierdzać rutynowe wywołania i odrzuca te wysokiego ryzyka; przełącz na Ask, aby zatwierdzić je samodzielnie. **Full Access** nigdy nie pyta i wyłącza piaskownicę Codewhale, choć reguły repozytorium i zarządzane zasady nadal obowiązują. Szczegóły są w [przewodniku dla v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md).

Nadchodzi w 0.10.3: `Shift+Tab` stanie się jednym pokrętłem przez Ask, Accept Edits, Plan, Auto-Review i Full Access, `Tab` przestanie zmieniać tryb, a Operate zostanie wycofany. Ask będzie pytać także przed każdą zmianą pliku, a nowe ustawienie domyślne, Accept Edits, będzie stosować zmiany wewnątrz repozytorium git i nadal pytać przed poleceniami. Opisuje to [aktualny przewodnik](../MODES.md).

W Ask i Auto-Review polecenia działają w piaskownicy systemu operacyjnego, jeśli system ją ma: Seatbelt w macOS i bubblewrap w systemie Linux, gdy jest zainstalowany. Windows nie ma jeszcze piaskownicy systemowej, a Full Access wyłącza piaskownicę. W piaskownicy polecenia powłoki i sesje terminala nie mogą czytać magazynów poświadczeń, takich jak `~/.ssh`, `~/.aws` i pliki `.env`. `/preview-request` pokazuje następne żądanie z ukrytymi danymi, nie wysyłając go.

Nie potrzebujesz konta, a klucze dodane przez `/provider` zostają na twoim komputerze. Z własnym kluczem API płacisz dostawcy bezpośrednio, a modele lokalne nie potrzebują klucza. Stopka i `/cost` pokazują koszt sesji, gdy cena jest znana; nieznana cena jest pokazywana jako nieznana, nigdy jako darmowa.

Telemetria jest domyślnie włączona. Wysyła zbiorcze liczniki (wersja i platforma, długość i wynik sesji, użyci wbudowani dostawcy, liczniki funkcji i błędów) z losowym identyfikatorem instalacji zmienianym co 90 dni, i nigdy nie wysyła rozmów, kodu, promptów, nazw modeli, nazw plików ani repozytoriów, ani poświadczeń. To polecenie ją wyłącza:

```bash
codewhale config set telemetry false
```

Przy starcie Codewhale sprawdza też na GitHubie, czy jest nowe wydanie; ustaw `CODEWHALE_NO_UPDATE_CHECK=1`, aby to pominąć. Więcej w [kolejności autoryzacji](../AUTHORIZATION_ORDER.md), [piaskownicy](../SANDBOX.md) i [telemetrii](../TELEMETRY.md).

## Dokąd zmierza

Codewhale staje się jednym środowiskiem obejmującym twój terminal, aplikację na komputer, przeglądarkę i komputery w chmurze, napędzanym jednym silnikiem. Każda część poniżej mówi, na jakim etapie jest dziś.

| Część | Stan | Dziś |
| --- | --- | --- |
| Aplikacja terminalowa | Wydane | Zainstaluj ją w systemie macOS, Linux lub Windows i uruchom `codewhale` w dowolnym repozytorium. |
| Lokalny klient w przeglądarce | Wydane | Uruchom `codewhale web`, aby czytać rozmowę, wysyłać zadanie i odpowiadać na prośby o zatwierdzenie w przeglądarce; jest dołączony do terminala i nie wymaga konta. |
| Wielu agentów przy jednym zadaniu | Wydane | Podagenci, Fleety i przepływy pracy pozwalają kilku agentom dzielić jedno zadanie, każdemu z własną rolą. |
| Aplikacja na komputer | W rozwoju, bez pobierania | Budujemy aplikację na komputer na tym samym silniku i nie ma ona jeszcze publicznego pobierania. |
| Komputery w chmurze | Tylko na zaproszenie | Komputery w chmurze są dostępne tylko na zaproszenie, a budujemy je tak, by sesja mogła dalej pracować na maszynie w chmurze. |
| Jedna sesja przez wszystkie | W budowie | Budujemy jedną sesję, która idzie z tobą od terminala do aplikacji na komputer, przeglądarki i chmury i przez całą drogę pozostaje twoja. |

[Plan rozwoju](../ROADMAP.md) mówi, co obejmuje każde wydanie i gdzie możesz pomóc.

## Jeśli przechodzisz z innego agenta

Codewhale sam czyta `AGENTS.md`. Pliki instrukcji pisane dla innych narzędzi, takie jak `CLAUDE.md`, `.cursorrules` czy `.github/copilot-instructions.md`, są wczytywane tylko wtedy, gdy włączysz je przez `project_instruction_imports = ["claude"]` (lub `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) w konfiguracji; zobacz [config.example.toml](../../config.example.toml). Codewhale ostrzega, gdy znajdzie taki plik i go nie wczyta.

`/import-claude` przygotowuje import twoich ustawień Claude Code i serwerów MCP do przejrzenia. `/plugin install` przyjmuje wtyczki Claude Code zawierające umiejętności, polecenia, profile agentów i serwery MCP, a odrzuca wtyczki, które potrzebują hooków lub serwerów LSP ([szczegóły](../CLAUDE_PLUGIN_COMPAT.md)).

## Dokumentacja

Te przewodniki odpowiadają gałęzi głównej i mogą opisywać zmiany, których nie ma jeszcze w najnowszym wydaniu.

| Zacznij tutaj | Dokumentacja techniczna |
| --- | --- |
| [Przewodnik użytkownika](../GUIDE.md) | [Konfiguracja](../CONFIGURATION.md) |
| [Instalacja](../INSTALL.md) | [Architektura](../ARCHITECTURE.md) |
| [Dostawcy i modele lokalne](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Tryby i uprawnienia](../MODES.md) | [Pisanie wtyczek](../PLUGIN_AUTHORING.md) |
| [Skróty klawiszowe](../KEYBINDINGS.md) | [Potwierdzenia](../RECEIPTS.md) |
| [Przegląd pull requestów na GitHubie](../GITHUB_ACTION.md) | [Cała dokumentacja](../README.md) |

## Współtworzenie

Codewhale budują otwarcie ludzie, którzy z niego korzystają, w wielu językach.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Ściana współtwórców Codewhale: awatar każdej osoby, której kod, współautorskie commity, kontynuowane pull requesty lub uznane zgłoszenia są w Codewhale, a na górze łączna liczba."></a>

[Ściana współtwórców](../CONTRIBUTORS.md) liczy wszystkich, których kod, współautorskie commity, kontynuowane pull requesty lub uznane zgłoszenia są w Codewhale, także osoby pomijane przez własną listę współtwórców GitHuba. `scripts/contributor-wall.py` odtwarza obraz i listę z historii git i danych z GitHuba.

Każda opinia jest mile widziana: błąd, pomysł albo coś, co wyglądało lub działało źle. [Otwórz issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) w każdej z tych spraw; issue to też najlepsze miejsce, by zacząć nową funkcję. Pull requesty są mile widziane przy poprawkach, tłumaczeniach, dostawcach i zaplanowanej pracy. Pytania zadawaj w [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). [Przewodnik dla współtwórców](../../CONTRIBUTING.md) pokazuje, dokąd trafia każdy rodzaj pomocy, jak twoja praca wchodzi do projektu pod twoim nazwiskiem i [gdzie co leży](../../CONTRIBUTING.md#project-structure).

Rozmawiaj na [Discordzie](https://discord.gg/37gfS3ksug) albo dodaj opiekuna projektu, Huntera, na WeChacie (`hunterbown`) i poproś o dołączenie do grupy Whale Brothers. Każdy jest mile widziany.

## Historia i licencja

Codewhale zaczynał jako `deepseek-tui` i nadal czyta konfigurację i sesje tamtego projektu. Jest utrzymywany niezależnie i nie jest powiązany z DeepSeek ani z żadnym innym dostawcą modeli.

[MIT](../../LICENSE). Kod zaadaptowany z innych projektów open source jest wymieniony w informacjach o stronach trzecich dla [dołączonych źródeł](../../THIRD_PARTY_NOTICES.md) i [zaadaptowanych rozwiązań](../THIRD_PARTY_NOTICES.md).
