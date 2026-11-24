<!-- source: README.md sha256:5da2c4c9d5f9 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Агент для программирования на Rust с открытым исходным кодом для вашего терминала. Любая модель, мультиагентные флоты, песочница ОС и проверяемые квитанции.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Сайт](https://codewhale.net) · [Документация](../README.md) · [Журнал изменений](../../CHANGELOG.md) · [Участие](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 в терминале после реальной задачи на локальной Ollama (qwen3:4b): отредактировал ls.py, запросил подтверждение перед запуском python3 -m unittest -v test_ls.py, выполнил 2 успешных теста и сообщил результат.">

<sub>Реальная сессия на опубликованном Codewhale v0.10.4 (48669ad108fa) с локальной Ollama (qwen3:4b) в режиме Accept Edits.</sub>

</div>

## Установка

Скрипт установки выше помещает проверенные по контрольной сумме `codewhale` и его короткий псевдоним `codew` в `~/.local/bin` на macOS и Linux. Если оболочка сообщает `command not found`, добавьте `~/.local/bin` в `PATH` ([подробнее](../INSTALL.md#put-it-on-your-path)). Обновление на месте: `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Готовые бинарные файлы выпускаются для Linux, macOS и Windows (x64 и arm64). Scoop, Nix, Homebrew для Linux, Android/Termux (превью) и ручная загрузка описаны в [docs/INSTALL.md](../INSTALL.md); образы контейнеров — в [docs/DOCKER.md](../DOCKER.md). Используйте один способ установки на машину, чтобы два бинарных файла не перекрывали друг друга в `PATH`.

## Быстрый старт

1. Закоммитьте или отложите (stash) текущие изменения, затем запустите `codewhale` из корня git-репозитория.
2. Выполните `/provider` (или нажмите `F3`), чтобы выбрать провайдера, вставить API-ключ и выбрать модель. По умолчанию используется DeepSeek; если ключ не настроен, а локальный сервер Ollama отвечает, Codewhale предложит его модели при первом сообщении.
3. Дайте задачу:

```text
Fix the failing tests and explain what changed.
```

Команды только для чтения выполняются сразу. Команды оболочки, вносящие изменения, запрашивают подтверждение: `y` — выполнить один раз, `a` — разрешить похожие команды до конца сеанса, `n` — отклонить. Выполните `/undo`, чтобы откатить последний ход и его правки файлов, или `/help` для списка команд и горячих клавиш.

## Возможности

- **Более 50 встроенных провайдеров и локальные модели:** Anthropic, OpenAI, DeepSeek (`deepseek-flash` / V4.1 Flash по умолчанию; [подробнее](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, любая OpenAI-совместимая точка доступа и локальные среды через Ollama, vLLM или SGLang. Переключайте модели прямо в сеансе командой `/model`, маршрутизируйте каждый ход через `/model auto` (`/router`) и проверяйте задержку и стоимость через `/turn inspect`. См. [Провайдеры](../PROVIDERS.md).
- **Квитанции и откат:** `/receipts` (или `codewhale receipts --last` после выхода) фиксирует изменённые файлы, команды, веб-/MCP-вызовы, субагентов, подтверждения и ошибки. `/undo` откатывает последний ход одним шагом; `/restore` возвращает рабочую область к более раннему снимку (требуется `/trust on` или Full Access). См. [Квитанции](../RECEIPTS.md).
- **Субагенты, флоты и рабочие процессы:** ведите многоходовые цели через `/goal`, делегируйте задачи [субагентам](../SUBAGENTS.md), координируйте [флоты (Fleets)](../FLEET.md) с разными ролями и моделями или запускайте хранящиеся в репозитории [рабочие процессы](../WORKFLOW_AUTHORING.md).
- **MCP, навыки, хуки и плагины:** подключайте [MCP-серверы](../MCP.md), [навыки](../SKILLS.md), [плагины](../PLUGINS.md) и [хуки](../HOOKS.md) жизненного цикла. Экспериментальный кодовый режим (`execute_tools`) объединяет несколько вызовов MCP/плагинов в рамках обычных правил подтверждения. Дополнительный [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) для macOS остаётся выключенным, пока вы явно не проверите и не включите его.
- **Прямая совместимость:** автоматически читает `AGENTS.md`. Подключите `CLAUDE.md`, `.cursorrules` или `.github/copilot-instructions.md` через `project_instruction_imports` в [config.example.toml](../../config.example.toml). Импортируйте настройки Claude Code и MCP-серверы командой `/import-claude` или устанавливайте плагины Claude Code (навыки, команды, профили агентов, MCP) и плагины DeepSeek Harness (DSH) через `/plugin install` ([подробнее](../CLAUDE_PLUGIN_COMPAT.md)).

## Точки входа

| Команда | Назначение |
| --- | --- |
| `codewhale` | Интерактивное терминальное приложение |
| `codewhale exec "…"` | Автономный запуск одной задачи для скриптов и CI ([руководство](../INSTALL.md#headless-scripts-ci); `--auto` использует Full Access без песочницы) |
| `codewhale web` | Локальный веб-клиент на `127.0.0.1` ([руководство](../WEB.md)) |
| `codewhale review --pr N` | Рекомендательная проверка PR на GitHub; [настройка GitHub-бота](../GITHUB_ACTION.md#set-up-your-own-bot) работает на вашем ключе |
| `codewhale app-server --http` | Локальный HTTP [Runtime API](../RUNTIME_API.md) для потоков, событий, подтверждений и пошаговых диффов |
| `codewhale serve --acp` | Agent Client Protocol поверх stdio для редакторов вроде Zed |

Все точки входа используют один движок на Rust ([архитектура](../ARCHITECTURE.md)). К тому же движку подключается поддерживаемое сообществом [расширение для VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) ([исходный код](https://github.com/HengQuWorld/CodeWhale-VSCode)). Актуальное состояние релизов, десктопа и облака см. в [примечаниях к последнему выпуску](https://github.com/codewhale-hq/Codewhale/releases/latest) и [дорожной карте](../ROADMAP.md).

## Права доступа, песочница и приватность

`Shift+Tab` переключает режимы разрешений по кругу: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` отвечает только за автодополнение). **Accept Edits** (по умолчанию) применяет правки внутри git-репозитория и спрашивает разрешение перед командами. **Ask** спрашивает перед каждой правкой и командой. **Plan** работает только на чтение. **Auto-Review** использует правила и модель-рецензента, чтобы одобрять рутинные вызовы и блокировать рискованные. **Full Access** пропускает запросы и отключает песочницу ОС (правила репозитория и управляемые политики продолжают действовать). См. [Режимы](../MODES.md).

В режимах Ask, Accept Edits и Auto-Review команды выполняются в песочнице ОС, где она доступна: Seatbelt на macOS и bubblewrap на Linux (для Windows песочницы ОС пока нет). Команды в песочнице и терминальные сеансы не могут читать пути с учётными данными, такие как `~/.ssh`, `~/.aws` или файлы `.env`. Выполните `/preview-request`, чтобы просмотреть отредактированный исходящий запрос перед отправкой.

API-ключи, добавленные через `/provider`, остаются на вашем компьютере, а локальным моделям ключ не нужен. `/cost` и строка состояния отслеживают расходы за сеанс, когда тариф известен (неизвестная цена отображается как неизвестная, а не как бесплатная).

Агрегированная телеметрия включена по умолчанию (версия, ОС, длительность и исход сеанса, идентификаторы встроенных провайдеров, счётчики функций и ошибок; случайный идентификатор установки со сменой каждые 90 дней; никогда не отправляются код, запросы, переписки, названия моделей, пути или ключи). Отключить её можно так:

```bash
codewhale config set telemetry false
```

Установите `CODEWHALE_NO_UPDATE_CHECK=1`, чтобы пропустить проверку обновлений GitHub при запуске. Подробнее: [порядок авторизации](../AUTHORIZATION_ORDER.md), [песочница](../SANDBOX.md) и [телеметрия](../TELEMETRY.md).

## Документация

Руководства отслеживают ветку `main` и могут описывать изменения, ещё не вошедшие в последний релиз.

| С чего начать | Справочник |
| --- | --- |
| [Руководство пользователя](../GUIDE.md) | [Конфигурация](../CONFIGURATION.md) |
| [Установка](../INSTALL.md) | [Архитектура](../ARCHITECTURE.md) |
| [Провайдеры и локальные модели](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Режимы и разрешения](../MODES.md) | [Создание плагинов](../PLUGIN_AUTHORING.md) |
| [Горячие клавиши](../KEYBINDINGS.md) | [Квитанции](../RECEIPTS.md) |
| [Проверка PR на GitHub](../GITHUB_ACTION.md) | [Вся документация](../README.md) |

## Участие и лицензия

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Стена участников Codewhale"></a>

[Стена участников](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) учитывает код, совместные коммиты, перенесённые PR и отмеченные отчёты. Сообщения об ошибках и предложения функций направляйте в [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), вопросы — в [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), а правила для PR и структуру репозитория смотрите в [CONTRIBUTING.md](../../CONTRIBUTING.md) ([структура проекта](../../CONTRIBUTING.md#project-structure)). Присоединяйтесь в [Discord](https://discord.gg/37gfS3ksug) или добавьте `hunterbown` в WeChat для группы Whale Brothers.

Codewhale начинался как `deepseek-tui` и сохраняет обратную совместимость с его конфигурацией и сеансами. Проект поддерживается независимо и не связан с DeepSeek или каким-либо другим провайдером моделей.

[MIT](../../LICENSE). Уведомления о стороннем коде: [включённые исходники](../../THIRD_PARTY_NOTICES.md) и [адаптированные решения](../THIRD_PARTY_NOTICES.md).
