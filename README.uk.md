<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Агент для програмування з відкритим кодом, що працює з будь-якою моделлю.**

Codewhale читає ваш проєкт, редагує файли, виконує команди та перевіряє власну
роботу — у вашому терміналі, з хмарною чи локальною моделлю на ваш вибір.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Вебсайт](https://codewhale.net) · [Документація](docs/README.md) · [Журнал змін](CHANGELOG.md) · [Участь у проєкті](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Сесія Codewhale в терміналі" width="760">

<sub>Справжній знімок терміналу Codewhale з налаштованого локального демонстраційного сеансу.</sub>

</div>

## Встановлення

macOS і Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Інсталятор завантажує бінарні файли з перевіркою контрольних сум у
`~/.local/bin`. Якщо після цього `codewhale` виводить "command not found",
виконайте рядок для PATH, який друкує інсталятор, або див.
[Додавання до PATH](docs/INSTALL.md#put-it-on-your-path). Оновлюватися можна в
будь-який момент командою `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo та інші способи</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew у Linux, Android/Termux, ручне завантаження з перевіркою
контрольних сум і необов’язкове дзеркало CNB описано в
[посібнику зі встановлення](docs/INSTALL.md). Оберіть один спосіб: кілька
встановлень на одній машині починають конфліктувати через `PATH`.

</details>

## Швидкий старт

**Ключі API ваших провайдерів — в одному місці.** [Створіть обліковий запис Codewhale](https://app.codewhale.net/register)
або виконайте `codewhale login`, щоб увійти. Зберігайте й оновлюйте ключі API в обліковому записі,
а потім використовуйте їх через маршрут моделей Codewhale на пристроях, де ви ввійшли.
[Налаштування ключів облікового запису](docs/CONFIGURATION.md#account-provider-keys).
Вхід не завантажує наявні локальні ключі до облікового запису; локальна робота доступна й без нього.

1. **Відкрийте проєкт.** Запустіть `codewhale` у теці, з якою хочете працювати.
2. **Підключіть модель.** Виконайте `/provider` (або натисніть `F3`), щоб додати
   ключ хмарного провайдера чи вибрати локальний рантайм. Якщо Ollama вже
   запущено з чат-моделлю, Codewhale сам перемкнеться на неї. Змінити модель
   можна командою `/model`.
3. **Дайте конкретне завдання.**

```text
Fix the failing tests and explain what changed.
```

Те саме завдання можна виконати без інтерфейсу — зі скрипту чи завдання CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Команди й комбінації клавіш показує `/help`.

## Способи запуску

Кожен клієнт працює з одним і тим самим локальним Runtime Codewhale, тож сесії,
інструменти й дозволи всюди поводяться однаково.

| Команда | Що робить |
| --- | --- |
| `codewhale` | Інтерактивний термінальний інтерфейс |
| `codewhale exec "…"` | Один хід без інтерфейсу зі скрипту чи CI; `--auto` дозволяє інструменти роботи з файлами та оболонкою, `--output-format stream-json` виводить JSON потоком |
| `codewhale web` | Вбудований [локальний браузерний клієнт](docs/WEB.md) на `127.0.0.1` |
| `codewhale review --pr N` | Рекомендаційний [огляд pull request](docs/GITHUB_ACTION.md); публікація вмикається окремо |
| Runtime API | [Локальний HTTP API](docs/RUNTIME_API.md) для тредів, подій і підтверджень |

Нативний настільний застосунок (GPUI) створюють як основний клієнт для
користувачів, що ввійшли в обліковий запис; про його доступність див. на
[сторінці продукту](https://codewhale.net/en/product). Підтримуване спільнотою
[розширення для VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
підключається до того самого Runtime з бічної панелі
([вихідний код](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Що він уміє

- **Будь-яка модель, без прив’язки.** Понад 40 вбудованих маршрутів до
  провайдерів — Anthropic, DeepSeek, Google, Mistral, Moonshot, OpenAI,
  OpenRouter, xAI та інші — плюс будь-яка OpenAI-сумісна кінцева точка й локальні
  моделі через Ollama, vLLM або SGLang. [Провайдери](docs/PROVIDERS.md)
- **Контроль залишається за вами.** Режим Plan досліджує проєкт, нічого не
  змінюючи; Work і Operate вносять зміни. Режими підтвердження визначають, коли
  виклик інструмента потребує вашої згоди, `/undo` і `/restore` повертають зміни
  в робочому каталозі, а `/receipts` перелічує кожен файл, команду й
  підтвердження в сесії. [Режими](docs/MODES.md) ·
  [Квитанції](docs/RECEIPTS.md)
- **Для довгих завдань.** Задайте довготривалу `/goal`, доручайте обмежені
  частини роботи [субагентам](docs/SUBAGENTS.md), запускайте контрольовані
  [команди агентів](docs/FLEET.md) з перевіркою перед витратами або описуйте їх
  як версійовані [робочі процеси](docs/WORKFLOW_AUTHORING.md).
- **Розширюйте те, чим уже користуєтеся.** Підключайте [MCP-сервери](docs/MCP.md),
  встановлюйте [навички](docs/SKILLS.md) і [плагіни](docs/PLUGINS.md), запускайте
  [гуки](docs/HOOKS.md) на події сесії та інструментів і завантажуйте наявні
  [плагіни Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md).
- **Computer Use.** Плагін, що входить до комплекту, додає інструменти для
  спостереження за іншими застосунками та керування ними. Перед використанням
  перевірте його доступ і ввімкніть його.
  [Посібник](crates/tui/plugins/computer-use/README.md)

## Режими й дозволи

| | Вибір | Варіанти |
| --- | --- | --- |
| **Режим** — чим зайнятий агент | `Tab` або `/mode` | Plan (дослідження, без змін) · Work (редагування й запуск) · Operate (веде мету через заплановані й перевірювані кроки) |
| **Підтвердження** — коли він питає заздалегідь | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access, як і раніше, дотримується жорстких меж політики. Кожен варіант
пояснює [посібник з режимів і дозволів](docs/MODES.md).

## Безпека

Codewhale працює на вашій машині з тими правами доступу, які ви йому надали.
Режими підтвердження та правила репозиторію обмежують дії агента, а команди
виконуються всередині пісочниці ОС там, де вона підтримується (Seatbelt у
macOS; bubblewrap у Linux, якщо встановлений і працює). `/preview-request` показує точний
запит із вилученими секретами до того, як щось буде надіслано. Невідомі ціни
моделей лишаються невідомими, а не видаються за безкоштовні.

Див. [порядок авторизації](docs/AUTHORIZATION_ORDER.md),
[пісочницю](docs/SANDBOX.md) і [телеметрію](docs/TELEMETRY.md) — лічильники
використання ввімкнені за замовчуванням, а `codewhale config set telemetry false`
їх вимикає.

## Документація

| З чого почати | Докладніше |
| --- | --- |
| [Встановлення](docs/INSTALL.md) | [Конфігурація](docs/CONFIGURATION.md) |
| [Провайдери та локальні моделі](docs/PROVIDERS.md) | [Архітектура](docs/ARCHITECTURE.md) |
| [Режими й дозволи](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Комбінації клавіш](docs/KEYBINDINGS.md) | [Створення плагінів](docs/PLUGIN_AUTHORING.md) |
| [Огляд PR у GitHub](docs/GITHUB_ACTION.md) | [Уся документація](docs/README.md) |

## Спільнота

Звіти про помилки, ідеї та pull request вітаються — користуєтеся ви Codewhale
вже місяцями чи пробуєте вперше. Якщо бракує провайдера або якийсь сценарій
незручний,
[відкрийте issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) або
[надішліть pull request](CONTRIBUTING.md). Перші внески теж вітаються, а автори
зберігають визнання за прийняту роботу. Добра відправна точка —
[структура репозиторію](CONTRIBUTING.md#project-structure).

Приєднуйтеся до [Discord](https://discord.gg/37gfS3ksug) або додайте Гантера у
WeChat (`hunterbown`) і попросіть вступити до групи Whale Brothers.

## Історія та ліцензія

Codewhale починався як `deepseek-tui` і досі читає конфігурацію та сесії того
проєкту. Тепер він не прив’язаний до провайдерів, розвивається незалежно й не
пов’язаний з жодним постачальником моделей. Дякуємо
[всім учасникам](docs/CONTRIBUTORS.md) та спільнотам відкритого коду, які
допомогли йому зрости.

[MIT](LICENSE). Фрагменти, адаптовані з інших проєктів із відкритим кодом,
перелічено в [повідомленнях про сторонні компоненти](docs/THIRD_PARTY_NOTICES.md).
