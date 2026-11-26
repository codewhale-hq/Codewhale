<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Агент для програмування на Rust із відкритим вихідним кодом для вашого термінала. Будь-яка модель, мультиагентні флоти, пісочниця ОС і перевірювані квитанції.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Сайт](https://codewhale.net) · [Документація](../README.md) · [Журнал змін](../../CHANGELOG.md) · [Участь](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 у терміналі після реального завдання на локальній Ollama (qwen3:4b): відредагував ls.py, запросив підтвердження перед запуском python3 -m unittest -v test_ls.py, виконав 2 успішні тести та повідомив результат.">

<sub>Реальний сеанс на опублікованому Codewhale v0.10.4 (48669ad108fa) із локальною Ollama (qwen3:4b) у режимі Accept Edits.</sub>

</div>

## Встановлення

Скрипт встановлення вище розміщує перевірені за контрольною сумою `codewhale` і короткий псевдонім `codew` у `~/.local/bin` на macOS та Linux. Якщо оболонка повідомляє `command not found`, додайте `~/.local/bin` до `PATH` ([детальніше](../INSTALL.md#put-it-on-your-path)). Оновлення на місці: `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Готові бінарні файли постачаються для Linux, macOS і Windows (x64 та arm64). Scoop, Nix, Homebrew для Linux, Android/Termux (попередній перегляд) і ручне завантаження описані в [docs/INSTALL.md](../INSTALL.md); образи контейнерів — у [docs/DOCKER.md](../DOCKER.md). Використовуйте один спосіб встановлення на машину, щоб два бінарні файли не перекривали один одного в `PATH`.

## Швидкий старт

1. Закомітьте або відкладіть (stash) поточну роботу, потім запустіть `codewhale` із кореня git-репозиторію.
2. Виконайте `/provider` (або натисніть `F3`), щоб обрати провайдера, вставити API-ключ і вибрати модель. За замовчуванням використовується DeepSeek; якщо ключ не налаштовано, а локальний сервер Ollama відповідає, Codewhale запропонує його моделі під час першого повідомлення.
3. Дайте завдання:

```text
Fix the failing tests and explain what changed.
```

Команди лише для читання виконуються одразу, крім режиму Ask, який питає перед кожною командою. Команди оболонки, що вносять зміни, запитують підтвердження: `y` — виконати один раз, `a` — дозволити подібні команди до кінця сеансу, `n` — відхилити. Виконайте `/undo`, щоб скасувати останній хід і його правки файлів, або `/help` для команд і комбінацій клавіш.

## Можливості

- **Понад 50 вбудованих провайдерів і локальні моделі:** Anthropic, OpenAI, DeepSeek (`deepseek-flash` / V4.1 Flash за замовчуванням; [детальніше](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, будь-яка OpenAI-сумісна кінцева точка та локальні середовища через Ollama, vLLM або SGLang. Перемикайте моделі під час сеансу командою `/model`, маршрутизуйте кожен хід через `/model auto` (`/router`) і перевіряйте затримку та вартість через `/turn inspect`. Див. [Провайдери](../PROVIDERS.md).
- **Квитанції та відкат:** `/receipts` (або `codewhale receipts --last` після виходу) записує змінені файли, команди, веб-/MCP-виклики, субагентів, підтвердження та збої. `/undo` скасовує останній хід в один крок; `/restore` повертає робочу область до попереднього знімка (потрібно `/trust on` або Full Access). Див. [Квитанції](../RECEIPTS.md).
- **Субагенти, флоти та робочі процеси:** ведіть багатокрокові цілі через `/goal`, делегуйте роботу [субагентам](../SUBAGENTS.md), координуйте [флоти (Fleets)](../FLEET.md) із різними ролями й моделями або запускайте [робочі процеси](../WORKFLOW_AUTHORING.md) з репозиторію.
- **MCP, навички, хуки та плагіни:** підключайте [MCP-сервери](../MCP.md), [навички](../SKILLS.md), [плагіни](../PLUGINS.md) і [хуки](../HOOKS.md) життєвого циклу. Експериментальний кодовий режим (`execute_tools`) об’єднує кілька викликів MCP/плагінів за звичайними правилами підтвердження. Додатковий [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) для macOS залишається вимкненим, доки ви явно не перевірите й не увімкнете його.
- **Пряма сумісність:** автоматично читає `AGENTS.md`. Підключіть `CLAUDE.md`, `.cursorrules` або `.github/copilot-instructions.md` через `project_instruction_imports` у [config.example.toml](../../config.example.toml). Імпортуйте налаштування Claude Code та MCP-сервери командою `/import-claude` або встановлюйте плагіни Claude Code (навички, команди, профілі агентів, MCP) і плагіни DeepSeek Harness (DSH) через `/plugin install` ([детальніше](../CLAUDE_PLUGIN_COMPAT.md)).

## Точки входу

| Команда | Призначення |
| --- | --- |
| `codewhale` | Інтерактивний термінальний застосунок |
| `codewhale exec "…"` | Автономний запуск одного завдання для скриптів і CI ([посібник](../INSTALL.md#headless-scripts-ci); `--auto` використовує Full Access без пісочниці) |
| `codewhale web` | Локальний вебклієнт на `127.0.0.1` ([посібник](../WEB.md)) |
| `codewhale review --pr N` | Дорадча перевірка PR на GitHub; [налаштування GitHub-бота](../GITHUB_ACTION.md#set-up-your-own-bot) працює на вашому ключі |
| `codewhale app-server --http` | Локальний HTTP [Runtime API](../RUNTIME_API.md) для потоків, подій, підтверджень і покрокових дифіів |
| `codewhale serve --acp` | Agent Client Protocol поверх stdio для редакторів на кшталт Zed |

Усі точки входу використовують один рушій на Rust ([архітектура](../ARCHITECTURE.md)). До того самого рушія підключається підтримуване спільнотою [розширення для VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) ([вихідний код](https://github.com/HengQuWorld/CodeWhale-VSCode)). Поточний статус випусків, десктопу й хмари див. у [примітках до останнього випуску](https://github.com/codewhale-hq/Codewhale/releases/latest) та [дорожній карті](../ROADMAP.md).

## Дозволи, пісочниця та приватність

`Shift+Tab` циклічно перемикає режими дозволів: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` лише доповнює введення). **Accept Edits** (за замовчуванням) застосовує правки всередині git-репозиторію та запитує дозвіл перед командами. **Ask** запитує перед кожною правкою й командою. **Plan** працює лише на читання. **Auto-Review** використовує правила та модель-рецензента, щоб схвалювати рутинні виклики й блокувати ризиковані. **Full Access** пропускає запити та вимикає пісочницю ОС (правила репозиторію та керовані політики залишаються чинними). Див. [Режими](../MODES.md).

У режимах Ask, Accept Edits та Auto-Review команди виконуються в пісочниці ОС, де вона доступна: Seatbelt на macOS і bubblewrap на Linux (у Windows пісочниці ОС поки немає). Команди в пісочниці та термінальні сеанси не можуть читати шляхи з обліковими даними, як-от `~/.ssh`, `~/.aws` чи файли `.env`. Виконайте `/preview-request`, щоб переглянути відредагований вихідний запит перед надсиланням.

API-ключі, додані через `/provider`, залишаються на вашому комп’ютері, а локальним моделям ключ не потрібен. `/cost` і рядок стану відстежують витрати за сеанс, коли тарифи відомі (невідома ціна позначається як невідома, а не безкоштовна).

Агрегована телеметрія увімкнена за замовчуванням (версія, ОС, тривалість і результат сеансу, ідентифікатори вбудованих провайдерів, ідентифікатори моделей із вбудованого публічного каталогу (користувацькі, приватні та локальні моделі записуються як `other`), сумарні токени, лічильники викликів і помилок вбудованих інструментів, лічильники функцій і помилок; випадковий ідентифікатор встановлення, що оновлюється кожні 90 днів; ніколи код, промпти, діалоги, користувацькі/приватні назви моделей, назви файлів/репозиторіїв/гілок, облікові дані чи IP-адреси). Вимкнути її можна командою:

```bash
codewhale config set telemetry false
```

Під час запуску Codewhale також перевіряє нові релізи на GitHub (`CODEWHALE_NO_UPDATE_CHECK=1`, щоб пропустити) та кожні 24 години оновлює публічний каталог моделей `models.dev` (`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` або `[catalog] auto_refresh = false`, щоб вимкнути; автоматично вимикається в CI та за `DO_NOT_TRACK`). Детальніше: [порядок авторизації](../AUTHORIZATION_ORDER.md), [пісочниця](../SANDBOX.md) та [телеметрія](../TELEMETRY.md).

## Документація

Посібники слідують за гілкою `main` і можуть описувати зміни, яких ще немає в останньому випуску.

| Початок роботи | Довідник |
| --- | --- |
| [Посібник користувача](../GUIDE.md) | [Конфігурація](../CONFIGURATION.md) |
| [Встановлення](../INSTALL.md) | [Архітектура](../ARCHITECTURE.md) |
| [Провайдери та локальні моделі](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Режими та дозволи](../MODES.md) | [Створення плагінів](../PLUGIN_AUTHORING.md) |
| [Комбінації клавіш](../KEYBINDINGS.md) | [Квитанції](../RECEIPTS.md) |
| [Перевірка PR на GitHub](../GITHUB_ACTION.md) | [Уся документація](../README.md) |

## Участь і ліцензія

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Стіна учасників Codewhale"></a>

[Стіна учасників](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) враховує код, спільні коміти, перенесені PR і відзначені звіти. Звіти про помилки й пропозиції функцій надсилайте в [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), запитання — у [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), а правила для PR і структуру проєкту див. у [CONTRIBUTING.md](../../CONTRIBUTING.md) ([структура проєкту](../../CONTRIBUTING.md#project-structure)). Приєднуйтеся в [Discord](https://discord.gg/37gfS3ksug) або додайте `hunterbown` у WeChat для групи Whale Brothers.

Codewhale починався як `deepseek-tui` і зберігає зворотну сумісність із його конфігурацією та сеансами. Проєкт підтримується незалежно й не пов’язаний із DeepSeek або будь-яким іншим провайдером моделей.

[MIT](../../LICENSE). Повідомлення про сторонній код: [вбудовані вихідні коди](../../THIRD_PARTY_NOTICES.md) та [адаптовані рішення](../THIRD_PARTY_NOTICES.md).
