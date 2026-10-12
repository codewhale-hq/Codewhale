<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Агент для программирования с открытым исходным кодом, работающий с любой моделью.**

Codewhale читает ваш проект, редактирует файлы, выполняет команды и проверяет
собственную работу — в вашем терминале, с облачной или локальной моделью на ваш
выбор.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Сайт](https://codewhale.net) · [Документация](docs/README.md) · [Список изменений](CHANGELOG.md) · [Участие в проекте](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Сессия Codewhale в терминале" width="760">

<sub>Настоящий снимок терминала Codewhale из настроенного локального демонстрационного сеанса.</sub>

</div>

## Установка

macOS и Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Установщик скачивает бинарные файлы с проверкой контрольных сумм в
`~/.local/bin`. Если после этого `codewhale` выдаёт "command not found",
выполните строку для PATH, которую выводит установщик, или см.
[Добавление в PATH](docs/INSTALL.md#put-it-on-your-path). Обновляться можно в
любой момент командой `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo и другие способы</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew в Linux, Android/Termux, ручная загрузка с проверкой
контрольных сумм и необязательное зеркало CNB описаны в
[руководстве по установке](docs/INSTALL.md). Выберите один способ: несколько
установок на одной машине начинают конфликтовать из-за `PATH`.

</details>

## Быстрый старт

**Ключи API ваших провайдеров — в одном месте.** [Создайте аккаунт Codewhale](https://app.codewhale.net/register)
или выполните `codewhale login`, чтобы войти. Сохраняйте и обновляйте ключи API в аккаунте,
затем используйте их через маршрут моделей Codewhale на устройствах, где вы вошли в аккаунт.
[Настройка ключей аккаунта](docs/CONFIGURATION.md#account-provider-keys).
Вход не загружает существующие локальные ключи в аккаунт; локальная работа доступна и без аккаунта.

1. **Откройте проект.** Запустите `codewhale` в папке, с которой хотите
   работать.
2. **Подключите модель.** Выполните `/provider` (или нажмите `F3`), чтобы
   добавить ключ облачного провайдера или выбрать локальный рантайм. Если Ollama
   уже запущена с чат-моделью, Codewhale переключится на неё сам. Сменить модель
   можно командой `/model`.
3. **Дайте конкретную задачу.**

```text
Fix the failing tests and explain what changed.
```

Ту же задачу можно выполнить без интерфейса — из скрипта или задания CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Команды и сочетания клавиш показывает `/help`.

## Способы запуска

Каждый клиент работает с одним и тем же локальным Runtime Codewhale, поэтому
сессии, инструменты и разрешения везде ведут себя одинаково.

| Команда | Что делает |
| --- | --- |
| `codewhale` | Интерактивный терминальный интерфейс |
| `codewhale exec "…"` | Один ход без интерфейса из скрипта или CI; `--auto` разрешает инструменты работы с файлами и оболочкой, `--output-format stream-json` выводит JSON потоком |
| `codewhale web` | Встроенный [локальный браузерный клиент](docs/WEB.md) на `127.0.0.1` |
| `codewhale review --pr N` | Рекомендательный [обзор pull request](docs/GITHUB_ACTION.md); публикация включается отдельно |
| Runtime API | [Локальный HTTP API](docs/RUNTIME_API.md) для тредов, событий и подтверждений |

Нативное настольное приложение (GPUI) создаётся как основной клиент для
вошедших в аккаунт пользователей; о его доступности см. на
[странице продукта](https://codewhale.net/en/product). Поддерживаемое
сообществом [расширение для VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
подключается к тому же Runtime из боковой панели
([исходный код](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Что он умеет

- **Любая модель, без привязки.** Более 40 встроенных маршрутов к провайдерам —
  Anthropic, DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI и
  другие — плюс любая OpenAI-совместимая конечная точка и локальные модели через
  Ollama, vLLM или SGLang. [Провайдеры](docs/PROVIDERS.md)
- **Контроль остаётся за вами.** Режим Plan исследует проект, ничего не меняя;
  Work и Operate вносят изменения. Режимы подтверждения определяют, когда вызов
  инструмента требует вашего согласия, `/undo` и `/restore` возвращают изменения
  в рабочем каталоге, а `/receipts` перечисляет каждый файл, команду и
  подтверждение в сессии. [Режимы](docs/MODES.md) ·
  [Квитанции](docs/RECEIPTS.md)
- **Для долгих задач.** Задайте долговременную `/goal`, поручайте ограниченные
  части работы [субагентам](docs/SUBAGENTS.md), запускайте контролируемые
  [команды агентов](docs/FLEET.md) с проверкой перед расходами или описывайте их
  в виде версионируемых [рабочих процессов](docs/WORKFLOW_AUTHORING.md).
- **Расширяйте то, чем уже пользуетесь.** Подключайте [MCP-серверы](docs/MCP.md),
  устанавливайте [навыки](docs/SKILLS.md) и [плагины](docs/PLUGINS.md), запускайте
  [хуки](docs/HOOKS.md) на события сессии и инструментов и загружайте
  существующие [плагины Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md).
- **Computer Use.** Входящий в комплект плагин добавляет инструменты для
  наблюдения за другими приложениями и управления ими. Перед использованием
  проверьте его доступ и включите его.
  [Руководство](crates/tui/plugins/computer-use/README.md)

## Режимы и разрешения

| | Выбор | Варианты |
| --- | --- | --- |
| **Режим** — чем занят агент | `Tab` или `/mode` | Plan (исследование, без изменений) · Work (правка и запуск) · Operate (ведёт цель через запланированные и проверяемые шаги) |
| **Подтверждение** — когда он спрашивает заранее | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access по-прежнему соблюдает жёсткие границы политики. Каждый вариант
объясняет [руководство по режимам и разрешениям](docs/MODES.md).

## Безопасность

Codewhale работает на вашей машине с теми правами доступа, которые вы ему
дали. Режимы подтверждения и правила репозитория ограничивают действия агента,
а команды выполняются внутри песочницы ОС там, где она поддерживается (Seatbelt
в macOS; bubblewrap в Linux, если установлен и работает). `/preview-request` показывает
точный запрос с вычеркнутыми секретами до того, как что-либо будет отправлено.
Неизвестные цены моделей остаются неизвестными, а не выдаются за бесплатные.

См. [порядок авторизации](docs/AUTHORIZATION_ORDER.md),
[песочницу](docs/SANDBOX.md) и [телеметрию](docs/TELEMETRY.md) — счётчики
использования включены по умолчанию, а `codewhale config set telemetry false`
их отключает.

## Документация

| С чего начать | Подробнее |
| --- | --- |
| [Установка](docs/INSTALL.md) | [Конфигурация](docs/CONFIGURATION.md) |
| [Провайдеры и локальные модели](docs/PROVIDERS.md) | [Архитектура](docs/ARCHITECTURE.md) |
| [Режимы и разрешения](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Сочетания клавиш](docs/KEYBINDINGS.md) | [Создание плагинов](docs/PLUGIN_AUTHORING.md) |
| [Обзор PR в GitHub](docs/GITHUB_ACTION.md) | [Вся документация](docs/README.md) |

## Сообщество

Отчёты об ошибках, идеи и pull request приветствуются — пользуетесь ли вы
Codewhale уже месяцы или пробуете его впервые. Если не хватает провайдера или
какой-то сценарий неудобен,
[откройте issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) или
[отправьте pull request](CONTRIBUTING.md). Первые вклады тоже приветствуются, а
авторы сохраняют признание за принятую работу. Хорошая отправная точка —
[структура репозитория](CONTRIBUTING.md#project-structure).

Присоединяйтесь к [Discord](https://discord.gg/37gfS3ksug) или добавьте Хантера
в WeChat (`hunterbown`) и попросите вступить в группу Whale Brothers.

## История и лицензия

Codewhale начинался как `deepseek-tui` и по-прежнему читает конфигурацию и
сессии того проекта. Теперь он не привязан к провайдерам, развивается независимо
и не связан ни с одним поставщиком моделей. Благодарим
[всех участников](docs/CONTRIBUTORS.md) и сообщества открытого кода, которые
помогли ему вырасти.

[MIT](LICENSE). Фрагменты, адаптированные из других проектов с открытым
исходным кодом, перечислены в [уведомлениях о сторонних компонентах](docs/THIRD_PARTY_NOTICES.md).
