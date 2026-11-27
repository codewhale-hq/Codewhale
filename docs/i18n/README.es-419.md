<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agente de programación de código abierto en Rust para tu terminal. Cualquier modelo, flotas multiagente, aislamiento a nivel de sistema operativo y recibos verificables.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Sitio web](https://codewhale.net) · [Documentación](../README.md) · [Registro de cambios](../../CHANGELOG.md) · [Contribuir](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · Español · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 en una terminal después de una tarea real con Ollama local (qwen3:4b): editó ls.py, esperó aprobación antes de ejecutar python3 -m unittest -v test_ls.py, ejecutó 2 pruebas exitosas e informó el resultado.">

<sub>Sesión real en la versión publicada Codewhale v0.10.4 (48669ad108fa) ejecutando Ollama local (qwen3:4b) en modo Accept Edits.</sub>

</div>

## Instalación

El script de instalación de arriba coloca el binario verificado por suma de comprobación `codewhale` y su alias corto `codew` en `~/.local/bin` en macOS y Linux. Si tu shell responde `command not found`, agrega `~/.local/bin` a tu `PATH` ([detalles](../INSTALL.md#put-it-on-your-path)). Actualiza en el sitio con `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Los binarios precompilados se publican para Linux, macOS y Windows (x64 y arm64). Scoop, Nix, Homebrew en Linux, Android/Termux (vista previa) y descargas manuales están en [docs/INSTALL.md](../INSTALL.md); las imágenes de contenedor están en [docs/DOCKER.md](../DOCKER.md). Usa un solo método de instalación por equipo para que dos binarios no se oculten entre sí en el `PATH`.

## Inicio rápido

1. Haz commit o guarda con `git stash` tu trabajo y ejecuta `codewhale` desde la raíz de un repositorio git.
2. Ejecuta `/provider` (o presiona `F3`) para elegir un proveedor, pegar una clave API y seleccionar un modelo. DeepSeek es el valor predeterminado; si no hay una clave configurada y responde un servidor Ollama local, Codewhale ofrece sus modelos en tu primer mensaje.
3. Dale una tarea:

```text
Fix the failing tests and explain what changed.
```

Los comandos de solo lectura se ejecutan de inmediato, salvo en Ask, que pregunta antes de cada comando. Los comandos de shell que modifican el entorno piden aprobación: `y` ejecuta una vez, `a` permite comandos coincidentes durante el resto de la sesión y `n` deniega. Ejecuta `/undo` para revertir el último turno y sus ediciones de archivos, o `/help` para ver comandos y atajos de teclado.

## Capacidades

- **Más de 50 proveedores integrados y modelos locales:** Anthropic, OpenAI, DeepSeek (`deepseek-flash` / V4.1 Flash por defecto; [detalles](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, cualquier endpoint compatible con OpenAI y tiempos de ejecución locales mediante Ollama, vLLM o SGLang. Cambia de modelo a mitad de sesión con `/model`, enruta por turno con `/model auto` (`/router`) e inspecciona latencia y costo con `/turn inspect`. Consulta [Proveedores](../PROVIDERS.md).
- **Recibos y reversión:** `/receipts` (o `codewhale receipts --last` al salir) registra archivos modificados, comandos, llamadas web/MCP, subagentes, aprobaciones y fallos. `/undo` revierte el último turno en un solo paso; `/restore` devuelve el espacio de trabajo a una instantánea anterior (requiere `/trust on` o Full Access). Consulta [Recibos](../RECEIPTS.md).
- **Subagentes, Fleets y flujos de trabajo:** Mantén objetivos de varios turnos con `/goal`, delega trabajo a [subagentes](../SUBAGENTS.md), coordina [Fleets](../FLEET.md) por rol y modelo o ejecuta [flujos de trabajo](../WORKFLOW_AUTHORING.md) versionados en el repositorio.
- **MCP, skills, hooks y plugins:** Conecta [servidores MCP](../MCP.md), [skills](../SKILLS.md), [plugins](../PLUGINS.md) y [hooks](../HOOKS.md) de ciclo de vida. El modo de código experimental (`execute_tools`) agrupa varias llamadas MCP o de plugins bajo tus reglas normales de aprobación. El complemento opcional de macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) permanece desactivado hasta que lo revises y actives explícitamente.
- **Compatibilidad directa:** Lee `AGENTS.md` automáticamente. Activa `CLAUDE.md`, `.cursorrules` o `.github/copilot-instructions.md` con `project_instruction_imports` en [config.example.toml](../../config.example.toml). Importa la configuración y los servidores MCP de Claude Code con `/import-claude`, o instala plugins de Claude Code (skills, comandos, perfiles de agente, MCP) y plugins de DeepSeek Harness (DSH) con `/plugin install` ([detalles](../CLAUDE_PLUGIN_COMPAT.md)).

## Puntos de entrada

| Comando | Propósito |
| --- | --- |
| `codewhale` | Aplicación interactiva de terminal |
| `codewhale exec "…"` | Ejecutor sin interfaz para scripts y CI ([guía](../INSTALL.md#headless-scripts-ci); `--auto` usa Full Access salvo que esté en sandbox) |
| `codewhale web` | Cliente web local en `127.0.0.1` ([guía](../WEB.md)) |
| `codewhale review --pr N` | Revisión orientativa de PR en GitHub; [configuración de bot de GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) con tu propia clave |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md) HTTP local para hilos, eventos, aprobaciones y diffs por turno |
| `codewhale serve --acp` | Agent Client Protocol sobre stdio para editores como Zed |

Todos los puntos de entrada comparten un único motor en Rust ([arquitectura](../ARCHITECTURE.md)). Una [extensión de VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) mantenida por la comunidad ([código fuente](https://github.com/HengQuWorld/CodeWhale-VSCode)) se conecta al mismo motor. Consulta las [últimas notas de versión](https://github.com/codewhale-hq/Codewhale/releases/latest) y la [hoja de ruta](../ROADMAP.md) para ver el estado actual de lanzamientos, escritorio y nube.

## Permisos, sandboxing y privacidad

`Shift+Tab` cambia el modo de permisos: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` solo autocompleta). **Accept Edits** (predeterminado) aplica ediciones dentro del repositorio git y pregunta antes de ejecutar comandos. **Ask** pide confirmación en cada edición y comando. **Plan** es de solo lectura. **Auto-Review** usa reglas y un modelo revisor para aprobar llamadas rutinarias y bloquear las de alto riesgo. **Full Access** omite confirmaciones y desactiva el sandbox del sistema operativo (las reglas del repositorio y la política administrada siguen vigentes). Consulta [Modos](../MODES.md).

En Ask, Accept Edits y Auto-Review, los comandos se ejecutan dentro de un sandbox del sistema operativo cuando está disponible: Seatbelt en macOS y bubblewrap en Linux (Windows aún no cuenta con sandbox del SO). Los comandos en sandbox y las sesiones de terminal no pueden leer rutas de credenciales como `~/.ssh`, `~/.aws` o archivos `.env`. Ejecuta `/preview-request` para inspeccionar una solicitud saliente redactada antes de enviarla.

Las claves API agregadas con `/provider` permanecen en tu máquina, y los modelos locales no requieren clave. `/cost` y el pie de página muestran el gasto de la sesión cuando el precio es conocido (un precio desconocido se muestra como desconocido, nunca como gratuito).

La telemetría agregada está activada por defecto (versión, SO, duración y resultado de la sesión, IDs de proveedores integrados, IDs de modelos del catálogo público integrado (los modelos personalizados, privados o locales se registran como `other`), totales de tokens, conteos de llamadas/errores de herramientas integradas y contadores de funciones/errores; ID de instalación aleatorio que rota cada 90 días; nunca código, prompts, conversaciones, nombres de modelos personalizados/privados, nombres de archivos/repos/ramas, credenciales ni direcciones IP). Desactívala con:

```bash
codewhale config set telemetry false
```

Al iniciar, Codewhale también comprueba nuevas versiones en GitHub (`CODEWHALE_NO_UPDATE_CHECK=1` para omitir) y actualiza el catálogo público de modelos `models.dev` cada 24 horas (`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` o `[catalog] auto_refresh = false` para omitir; desactivado automáticamente en CI y con `DO_NOT_TRACK`). Detalles: [orden de autorización](../AUTHORIZATION_ORDER.md), [sandboxing](../SANDBOX.md) y [telemetría](../TELEMETRY.md).

## Documentación

Las guías siguen `main` y pueden describir cambios que aún no están en la última versión publicada.

| Para empezar | Referencia |
| --- | --- |
| [Guía de usuario](../GUIDE.md) | [Configuración](../CONFIGURATION.md) |
| [Instalación](../INSTALL.md) | [Arquitectura](../ARCHITECTURE.md) |
| [Proveedores y modelos locales](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modos y permisos](../MODES.md) | [Creación de plugins](../PLUGIN_AUTHORING.md) |
| [Atajos de teclado](../KEYBINDINGS.md) | [Recibos](../RECEIPTS.md) |
| [Revisión de pull requests en GitHub](../GITHUB_ACTION.md) | [Toda la documentación](../README.md) |

## Contribuciones y licencia

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Muro de colaboradores de Codewhale"></a>

El [muro de colaboradores](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) registra código, commits en coautoría, PRs integrados y reportes acreditados. Los reportes de errores y propuestas de funciones van en [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), las preguntas en [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), y las pautas de PR y organización del repositorio están en [CONTRIBUTING.md](../../CONTRIBUTING.md) ([estructura del proyecto](../../CONTRIBUTING.md#project-structure)). Únete en [Discord](https://discord.gg/37gfS3ksug) o agrega a `hunterbown` en WeChat para el grupo Whale Brothers.

Codewhale comenzó como `deepseek-tui` y sigue siendo retrocompatible con su configuración y sus sesiones. Se mantiene de forma independiente y no está afiliado con DeepSeek ni con ningún otro proveedor de modelos.

[MIT](../../LICENSE). Avisos de terceros: [fuentes incluidas](../../THIRD_PARTY_NOTICES.md) y [diseños adaptados](../THIRD_PARTY_NOTICES.md).
