<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>El modelo puede ser de cualquiera. El agente debería ser tuyo.</h1>

Codewhale es un agente de programación de código abierto para tu terminal, escrito en Rust. Funciona con cualquier modelo, puede repartir una tarea entre varios agentes y puedes leer cada línea de su código.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Esto instala la versión más reciente en macOS o Linux, y no necesita cuenta. [Windows, npm y Cargo](#instalación) están más abajo.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Sitio web](https://codewhale.net) · [Documentación](../README.md) · [Registro de cambios](../../CHANGELOG.md) · [Contribuir](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Esta traducción es un borrador automático que aún no ha revisado un hablante nativo.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale en una terminal después de una tarea de demostración. Informa que agregó una opción --json a export.py, que pasaron las 3 pruebas locales y que una revisión delegada no encontró problemas. La lista de tareas de abajo muestra tres pasos completados, y la línea de estado muestra Full Access y el proveedor local website-demo.">

<sub>Una compilación de desarrollo de 0.10.3, que aún no se ha publicado, con un modelo de prueba guionizado, en Full Access dentro de un espacio de trabajo desechable. La edición, tres pruebas y una revisión de un subagente se ejecutaron de verdad.</sub>

</div>

## Instalación

El comando de arriba coloca `codewhale`, verificado con suma de comprobación, y su alias corto `codew` en `~/.local/bin`. Si después tu shell dice `command not found`, sigue la nota sobre el PATH que imprime el instalador ([detalles](../INSTALL.md#put-it-on-your-path)). `codewhale update` lo actualiza.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Hay binarios precompilados para Linux, macOS y Windows en x64 y arm64. Scoop, Nix, Homebrew en Linux, Android y Termux (vista previa) y las descargas manuales están en la [guía de instalación](../INSTALL.md), y los contenedores en [Docker](../DOCKER.md). Usa un solo método de instalación por máquina; si no, quedan dos copias en tu `PATH` y gana la que aparezca primero.

## Inicio rápido

1. Haz commit o stash de tu trabajo y ejecuta `codewhale` desde la raíz de un repositorio git. De forma predeterminada aplica los cambios de archivos del repositorio sin preguntar y pregunta antes de cada comando de shell que no sea de solo lectura.
2. Ejecuta `/provider` (o presiona `F3`), elige un proveedor, pega su clave de API y elige un modelo. DeepSeek es el proveedor predeterminado. En el primer inicio sin clave, Codewhale te pide un proveedor cuando envías tu primer mensaje, y ofrece un modelo local cuando responde un servidor Ollama local.
3. Escribe una tarea:

```text
Fix the failing tests and explain what changed.
```

Los comandos de solo lectura se ejecutan de inmediato. Para cualquier otro comando de shell, Codewhale pregunta primero: `y` lo ejecuta una vez, `a` permite comandos parecidos durante el resto de la conversación y `n` lo rechaza. `/undo` deshace tu última petición y sus cambios de archivos, y `/help` muestra los comandos y las teclas.

Para scripts y CI, `codewhale exec` ejecuta una tarea sin la interfaz. Lee [ejecuciones sin interfaz](../INSTALL.md#headless-scripts-ci) antes de darle herramientas con `--auto`, porque `--auto` usa Full Access a menos que agregues un sandbox.

## Qué hace

Dale una tarea a Codewhale y leerá el repositorio, editará archivos y ejecutará comandos hasta terminar la tarea o necesitar tu respuesta. Las [notas de la última versión](https://github.com/codewhale-hq/Codewhale/releases/latest) listan los cambios más recientes, y el [registro de cambios](../../CHANGELOG.md) tiene todas las versiones.

### Cualquier modelo

Trae más de 40 proveedores integrados, entre ellos Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter y xAI. Funciona con cualquier endpoint compatible con OpenAI, y también con modelos locales mediante Ollama, vLLM o SGLang. El modelo predeterminado es `deepseek-flash` de DeepSeek, que ahora sirve DeepSeek V4.1 Flash ([detalles](../PROVIDERS.md#deepseek-v41-flash)).

`/model` cambia de modelo entre turnos, y `/model auto` elige uno para cada turno (`/router` define cómo). `/turn inspect` muestra qué modelo usó un turno, cuánto costó y cuánto tardó. Consulta [Proveedores](../PROVIDERS.md).

### Recibos y deshacer

`/receipts`, o `codewhale receipts --last` después de salir, lista los archivos que cambió una sesión, los comandos que ejecutó, sus llamadas web y MCP, los agentes que inició, cada aprobación y quién la dio, y lo que falló. `/undo` deshace tu última petición y los archivos que cambió en un solo paso. `/restore` regresa todo el espacio de trabajo a una instantánea anterior, y como eso sobrescribe ediciones posteriores, necesita `/trust on` o Full Access. Consulta [Recibos](../RECEIPTS.md).

### Varios agentes en una tarea

`/goal` fija un objetivo que se mantiene entre turnos. Codewhale puede repartir el trabajo entre [subagentes](../SUBAGENTS.md), ejecutar una [Fleet](../FLEET.md) de agentes con sus propios roles y modelos, o seguir un [flujo de trabajo](../WORKFLOW_AUTHORING.md) guardado en el repositorio.

### Extensiones

Conecta [servidores MCP](../MCP.md), agrega [skills](../SKILLS.md) y [plugins](../PLUGINS.md), y ejecuta [hooks](../HOOKS.md) en eventos de sesión y de herramientas. El modo código (experimental, activado por defecto) permite que el modelo escriba un programa `execute_tools` que llama a varias herramientas MCP y de plugins; cada llamada sigue respetando tus reglas de aprobación y conserva su recibo. El plugin Computer Use incluido en v0.10.2 (experimental, solo macOS) puede observar y operar otras apps, y permanece apagado hasta que revises su acceso y lo actives ([guía](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). Próximamente en 0.10.3: el plugin se publica [por separado](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Formas de ejecutarlo

| Comando | Qué hace |
| --- | --- |
| `codewhale` | La aplicación interactiva de terminal |
| `codewhale exec "…"` | Una tarea sin la interfaz, para scripts y CI ([ejecuciones sin interfaz](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Un cliente web local en `127.0.0.1` ([guía](../WEB.md)) |
| `codewhale review --pr N` | Una revisión orientativa de un pull request de GitHub; la [configuración del bot de GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) la ejecuta con tu propia clave de modelo |
| `codewhale app-server --http` | La [Runtime API](../RUNTIME_API.md), una API HTTP local para hilos, eventos, aprobaciones y los cambios de archivos de cada turno |
| `codewhale serve --acp` | El Agent Client Protocol sobre stdio, para editores como Zed |

La aplicación de terminal, `exec`, `web` y la Runtime API comparten un solo motor ([arquitectura](../ARCHITECTURE.md)). Una [extensión de VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) mantenida por la comunidad se conecta desde la barra lateral ([código fuente](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Cómo se mantiene seguro

Dos interruptores dicen cuánto puede hacer Codewhale por su cuenta. Con la entrada vacía, `Tab` cambia el modo: **Plan** lee y busca pero no cambia nada, **Work** edita archivos y ejecuta comandos, y **Operate** tiene el mismo acceso que Work y sigue hasta verificar el objetivo o necesitarte. `Shift+Tab` cambia el permiso. **Ask**, el predeterminado, aplica cambios de archivos en tu repositorio sin preguntar y pregunta antes de los comandos de shell que no son de solo lectura. **Auto-Review** deja que reglas y un modelo revisor aprueben las llamadas rutinarias y rechaza las de alto riesgo; cambia a Ask para aprobarlas tú. **Full Access** nunca pregunta y desactiva el sandbox de Codewhale, aunque las reglas del repositorio y la política administrada siguen aplicando. La [guía de v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md) tiene los detalles.

Próximamente en 0.10.3: `Shift+Tab` se convierte en un solo selector que pasa por Ask, Accept Edits, Plan, Auto-Review y Full Access, `Tab` deja de cambiar el modo y Operate se retira. Ask preguntará también antes de cada cambio de archivo, y el nuevo predeterminado, Accept Edits, aplicará los cambios dentro del repositorio git y seguirá preguntando antes de los comandos. La [guía actual](../MODES.md) lo describe.

En Ask y Auto-Review, los comandos se ejecutan en un sandbox del sistema operativo cuando el sistema tiene uno: Seatbelt en macOS, y bubblewrap en Linux cuando está instalado. Windows aún no tiene sandbox del sistema operativo, y Full Access apaga el sandbox. Dentro del sandbox, los comandos de shell y las sesiones de terminal no pueden leer almacenes de credenciales como `~/.ssh`, `~/.aws` y los archivos `.env`. `/preview-request` muestra una vista previa censurada de la próxima petición sin enviarla.

No necesitas cuenta, y las claves que agregas con `/provider` se quedan en tu máquina. Con tu propia clave de API le pagas directamente al proveedor, y los modelos locales no necesitan clave. El pie de página y `/cost` muestran lo que ha costado una sesión cuando se conoce el precio, y un precio desconocido se muestra como desconocido, nunca como gratis.

La telemetría está activada por defecto. Envía conteos agregados (versión y plataforma, duración y resultado de la sesión, qué proveedores integrados usaste, conteos de funciones y de errores) con un ID de instalación aleatorio que cambia cada 90 días, y nunca conversaciones, código, prompts, nombres de modelos, nombres de archivos o repositorios, ni credenciales. Este comando la desactiva:

```bash
codewhale config set telemetry false
```

Al iniciar, Codewhale también busca en GitHub una versión nueva; define `CODEWHALE_NO_UPDATE_CHECK=1` para omitirlo. Más en [orden de autorización](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md) y [telemetría](../TELEMETRY.md).

## Hacia dónde va

Codewhale se está convirtiendo en un único entorno que abarca tu terminal, una aplicación de escritorio, el navegador y computadoras en la nube, todo gobernado por un solo motor. Cada parte de abajo indica en qué punto está hoy.

| Parte | Estado | Hoy |
| --- | --- | --- |
| Aplicación de terminal | Publicado | Instálala en macOS, Linux o Windows y ejecuta `codewhale` en cualquier repositorio. |
| Cliente de navegador local | Publicado | Ejecuta `codewhale web` para leer la conversación, enviar una tarea y responder a las aprobaciones en tu navegador; viene con la terminal y no necesita cuenta. |
| Varios agentes en una tarea | Publicado | Los subagentes, las Fleets y los flujos de trabajo permiten que varios agentes compartan una tarea, cada uno con su propio rol. |
| Aplicación de escritorio | En desarrollo, sin descarga | Estamos construyendo una aplicación de escritorio sobre el mismo motor, y todavía no tiene descarga pública. |
| Computadoras en la nube | Solo por invitación | Las computadoras en la nube son solo por invitación, y las estamos construyendo para que una sesión pueda seguir trabajando en una máquina en la nube. |
| Una sesión a través de todos | En construcción | Estamos construyendo una sesión que te acompaña desde la terminal hasta el escritorio, el navegador y la nube, y sigue siendo tuya en todo el camino. |

La [hoja de ruta](../ROADMAP.md) dice qué cubre cada versión y dónde puedes ayudar.

## Si vienes de otro agente

Codewhale lee `AGENTS.md` por su cuenta. Los archivos de instrucciones escritos para otras herramientas, como `CLAUDE.md`, `.cursorrules` o `.github/copilot-instructions.md`, solo se cargan si los activas con `project_instruction_imports = ["claude"]` (o `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) en tu configuración; consulta [config.example.toml](../../config.example.toml). Codewhale te avisa cuando encuentra uno que no cargó.

`/import-claude` prepara una importación de tu configuración de Claude Code y tus servidores MCP para que la revises. `/plugin install` acepta plugins de Claude Code que contienen skills, comandos, perfiles de agente y servidores MCP, y rechaza los que necesitan hooks o servidores LSP ([detalles](../CLAUDE_PLUGIN_COMPAT.md)).

## Documentación

Estas guías siguen la rama principal y pueden describir cambios que todavía no están en la última versión.

| Empieza aquí | Referencia |
| --- | --- |
| [Guía del usuario](../GUIDE.md) | [Configuración](../CONFIGURATION.md) |
| [Instalación](../INSTALL.md) | [Arquitectura](../ARCHITECTURE.md) |
| [Proveedores y modelos locales](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modos y permisos](../MODES.md) | [Crear plugins](../PLUGIN_AUTHORING.md) |
| [Atajos de teclado](../KEYBINDINGS.md) | [Recibos](../RECEIPTS.md) |
| [Revisión de pull requests en GitHub](../GITHUB_ACTION.md) | [Toda la documentación](../README.md) |

## Contribuir

Codewhale se construye en abierto por las personas que lo usan, en muchos idiomas.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="El muro de contribuidores de Codewhale: un avatar por cada persona cuyo código, commits en coautoría, pull requests retomados o reportes acreditados están en Codewhale, con el total arriba."></a>

El [muro de contribuidores](../CONTRIBUTORS.md) cuenta a todas las personas cuyo código, commits en coautoría, pull requests retomados o reportes acreditados están en Codewhale, incluidas las que la propia lista de GitHub no muestra. `scripts/contributor-wall.py` regenera la imagen y la lista a partir del historial de git y de GitHub.

Toda opinión es bienvenida: un error, una idea o algo que se veía o se sentía mal. [Abre un issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) para cualquiera de ellas; un issue también es el mejor lugar para empezar una función nueva. Los pull requests son bienvenidos para correcciones, traducciones, proveedores y trabajo planificado. Las preguntas van a [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). La [guía de contribución](../../CONTRIBUTING.md) muestra a dónde va cada tipo de ayuda, cómo entra tu trabajo con tu nombre y [dónde está cada cosa](../../CONTRIBUTING.md#project-structure).

Conversa en [Discord](https://discord.gg/37gfS3ksug), o agrega al mantenedor, Hunter, en WeChat (`hunterbown`) y pide unirte al grupo Whale Brothers. Todos son bienvenidos.

## Historia y licencia

Codewhale empezó como `deepseek-tui` y todavía lee la configuración y las sesiones de ese proyecto. Se mantiene de forma independiente y no está afiliado a DeepSeek ni a ningún otro proveedor de modelos.

[MIT](../../LICENSE). El código adaptado de otros proyectos de código abierto aparece en los avisos de terceros para [fuentes incluidas](../../THIRD_PARTY_NOTICES.md) y [diseños adaptados](../THIRD_PARTY_NOTICES.md).
