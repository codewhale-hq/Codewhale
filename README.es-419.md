<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**El agente de código abierto que funciona con cualquier modelo.**

Codewhale lee tu proyecto, edita archivos, ejecuta comandos y comprueba su propio
trabajo — en tu terminal, con un modelo alojado o local que tú eliges.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Sitio web](https://codewhale.net) · [Documentación](docs/README.md) · [Registro de cambios](CHANGELOG.md) · [Contribuir](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Una sesión de terminal de Codewhale" width="760">

<sub>Captura real de la terminal de Codewhale en una sesión de demostración local configurada.</sub>

</div>

## Instalación

macOS y Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

El instalador descarga binarios con suma de verificación en `~/.local/bin`. Si
después `codewhale` responde "command not found", ejecuta la única línea de PATH
que muestra el instalador, o consulta [Agregarlo al PATH](docs/INSTALL.md#put-it-on-your-path).
Actualiza cuando quieras con `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo y otras vías</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew en Linux, Android/Termux, las descargas manuales con
verificación de suma de control y el espejo opcional de CNB se explican en la
[guía de instalación](docs/INSTALL.md). Elige una sola vía: varias instalaciones
en la misma máquina terminan disputándose el `PATH`.

</details>

## Inicio rápido

**Las claves API de tus proveedores, en un solo lugar.** [Crea una cuenta de Codewhale](https://app.codewhale.net/register)
o ejecuta `codewhale login` para iniciar sesión. Guarda y actualiza tus claves API en tu cuenta
y úsalas a través de la ruta de modelos de Codewhale en los dispositivos donde hayas iniciado sesión.
[Configura las claves de tu cuenta](docs/CONFIGURATION.md#account-provider-keys).
Iniciar sesión no sube las claves locales existentes; puedes seguir usando Codewhale localmente sin una cuenta.

1. **Abre tu proyecto.** Ejecuta `codewhale` en la carpeta en la que quieres trabajar.
2. **Conecta un modelo.** Ejecuta `/provider` (o presiona `F3`) para agregar una
   clave alojada o elegir un entorno local. Si Ollama ya se está ejecutando con
   un modelo de chat, Codewhale cambia a él por sí solo. Usa `/model` para cambiar de modelo.
3. **Dale una tarea concreta.**

```text
Fix the failing tests and explain what changed.
```

La misma tarea se puede ejecutar sin interfaz desde un script o un trabajo de CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Ejecuta `/help` para ver los comandos y los atajos de teclado.

## Formas de ejecutarlo

Todos los clientes usan el mismo Runtime local de Codewhale, así que las sesiones,
las herramientas y los permisos se comportan igual en todas partes.

| Comando | Qué hace |
| --- | --- |
| `codewhale` | La interfaz interactiva de terminal |
| `codewhale exec "…"` | Un turno sin interfaz desde un script o CI; `--auto` permite las herramientas de archivos y de shell, `--output-format stream-json` transmite JSON |
| `codewhale web` | El [cliente de navegador local](docs/WEB.md) incluido, en `127.0.0.1` |
| `codewhale review --pr N` | Una [revisión de pull request](docs/GITHUB_ACTION.md) de carácter orientativo; publicarla es opcional |
| Runtime API | Una [API HTTP local](docs/RUNTIME_API.md) para hilos, eventos y aprobaciones |

Se está desarrollando una aplicación de escritorio nativa (GPUI) como cliente del
producto con sesión iniciada; consulta la
[página del producto](https://codewhale.net/en/product) para ver su disponibilidad. La
[extensión de VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode),
mantenida por la comunidad, se conecta al mismo Runtime desde una barra lateral
([código fuente](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Qué hace

- **Cualquier modelo, sin dependencia de un proveedor.** Más de 40 rutas de
  proveedores integradas — Anthropic, DeepSeek, Google, Mistral, Moonshot,
  OpenAI, OpenRouter, xAI y más — además de cualquier endpoint compatible con
  OpenAI y modelos locales mediante Ollama, vLLM o SGLang. [Proveedores](docs/PROVIDERS.md)
- **Tú mantienes el control.** El modo Plan explora sin cambiar nada; Work y
  Operate hacen cambios. Las posturas de aprobación deciden cuándo una llamada a
  una herramienta necesita tu visto bueno, `/undo` y `/restore` recuperan los
  cambios del espacio de trabajo, y `/receipts` enumera cada archivo, comando y
  aprobación de una sesión. [Modos](docs/MODES.md) ·
  [Recibos](docs/RECEIPTS.md)
- **Pensado para trabajos largos.** Define un `/goal` duradero, delega trabajo
  acotado a [sub-agentes](docs/SUBAGENTS.md), ejecuta [equipos de agentes](docs/FLEET.md)
  supervisados con una verificación previa del gasto, o automatízalos como
  [flujos de trabajo](docs/WORKFLOW_AUTHORING.md) versionados en el repositorio.
- **Amplía lo que ya usas.** Conecta [servidores MCP](docs/MCP.md), instala
  [skills](docs/SKILLS.md) y [plugins](docs/PLUGINS.md), ejecuta
  [hooks](docs/HOOKS.md) en eventos de sesión y de herramientas, y carga los
  [plugins de Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) existentes.
- **Computer Use.** Un plugin incluido agrega herramientas para observar y
  operar otras aplicaciones. Revisa el acceso que solicita y habilítalo antes de usarlo.
  [Guía](crates/tui/plugins/computer-use/README.md)

## Modos y permisos

| | Se elige con | Opciones |
| --- | --- | --- |
| **Modo** — lo que hace el agente | `Tab` o `/mode` | Plan (explorar, sin cambios) · Work (editar y ejecutar) · Operate (llevar un objetivo adelante con pasos planificados y verificados) |
| **Postura** — cuándo pregunta antes | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access sigue respetando los límites estrictos de las políticas. La
[guía de modos y permisos](docs/MODES.md) explica cada opción.

## Seguridad

Codewhale se ejecuta en tu equipo con el acceso que le otorgues. Las posturas de
aprobación y las reglas del repositorio limitan lo que el agente puede hacer, y
los comandos se ejecutan dentro de un sandbox del sistema operativo donde es
compatible (Seatbelt en macOS; bubblewrap en Linux cuando está instalado y funciona).
`/preview-request` muestra la solicitud exacta, con los datos sensibles
redactados, antes de enviar nada. Los precios de modelos desconocidos siguen
siendo desconocidos en lugar de presentarse como gratuitos.

Consulta el [orden de autorización](docs/AUTHORIZATION_ORDER.md), el
[sandboxing](docs/SANDBOX.md) y la [telemetría](docs/TELEMETRY.md): los
contadores de uso están activados por defecto y `codewhale config set telemetry false`
los desactiva.

## Documentación

| Empieza aquí | Profundiza |
| --- | --- |
| [Instalación](docs/INSTALL.md) | [Configuración](docs/CONFIGURATION.md) |
| [Proveedores y modelos locales](docs/PROVIDERS.md) | [Arquitectura](docs/ARCHITECTURE.md) |
| [Modos y permisos](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Atajos de teclado](docs/KEYBINDINGS.md) | [Creación de plugins](docs/PLUGIN_AUTHORING.md) |
| [Revisión de PR en GitHub](docs/GITHUB_ACTION.md) | [Toda la documentación](docs/README.md) |

## Comunidad

Los reportes de errores, las ideas de funciones y los pull requests son
bienvenidos, ya uses Codewhale desde hace meses o lo pruebes por primera vez. Si
falta un proveedor o un flujo de trabajo resulta incómodo,
[abre un issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) o
[envía un pull request](CONTRIBUTING.md). Las primeras contribuciones son
bienvenidas, y quienes contribuyen conservan el crédito por el trabajo que se
integra. La [estructura del repositorio](CONTRIBUTING.md#project-structure) es un
buen punto de partida.

Únete a [Discord](https://discord.gg/37gfS3ksug), o agrega a Hunter en WeChat
(`hunterbown`) y pide entrar al grupo Whale Brothers.

## Historia y licencia

Codewhale comenzó como `deepseek-tui` y todavía lee la configuración y las
sesiones de ese proyecto. Ahora es neutral respecto de los proveedores, se
mantiene de forma independiente y no está afiliado a ningún proveedor de
modelos. Gracias a [cada colaborador](docs/CONTRIBUTORS.md) y a las comunidades
de código abierto que ayudaron a que crezca.

[MIT](LICENSE). Las partes adaptadas de otros proyectos de código abierto se
registran en los [avisos de terceros](docs/THIRD_PARTY_NOTICES.md).
