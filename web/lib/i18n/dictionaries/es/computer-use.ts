import type { ComputerUseDict } from "../types";

/**
 * Spanish dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Neutral Spanish (Latin America
 * and Spain), informal `tú`, matching the register of the other es
 * dictionaries. Product names (Codewhale, Computer Use), the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Vista previa · Codewhale",
  metaDescription: "Descarga y configura Codewhale Computer Use para Mac. Control de aplicaciones en segundo plano, configuración de permisos y controles Pause y Stop en tus manos.",
  title: "Computer Use",
  lead: "Deja que tu agente trabaje en apps de Mac mientras sigues usando el ordenador.",
  publisher: "Por Codewhale",
  download: "Descargar para Mac",
  downloadZip: "Archivo ZIP (lo usa el actualizador integrado en la aplicación)",
  requirements: "macOS 13.5 o posterior · Apple silicon e Intel",
  included: "Una sola descarga. No necesitas instalar Node ni un compilador por separado.",
  pendingTitle: "Descarga para Mac en preparación",
  pendingBody: "El instalador público aparecerá aquí cuando se completen la notarización de Apple y las comprobaciones de lanzamiento.",
  unavailableTitle: "No se pudo comprobar la disponibilidad de la descarga",
  unavailableBody: "Recarga esta página para volver a intentarlo o consulta los lanzamientos publicados más abajo.",
  releases: "Lanzamientos publicados",
  receipt: "Datos de verificación de la descarga",
  setup: "Configura tu Mac",
  steps: [
    { title: "Instala la aplicación", body: "Abre la imagen de disco y arrastra Codewhale Computer Use a «Aplicaciones». Ábrela desde «Aplicaciones» y luego elige Computer Use en el icono de la ballena de la barra de menús." },
    { title: "Revisa los permisos", body: "Usa los botones de configuración para abrir «Accesibilidad» y «Grabación de pantalla» en Ajustes del Sistema. Tú decides qué permisos conceder." },
    { title: "Conecta tu agente", body: "Elige a continuación la configuración de Codewhale para tu versión de la CLI. Otros clientes MCP compatibles pueden usar la guía de configuración del repositorio." },
  ],
  controlsTitle: "Mantén el control",
  controlsBody: "Tras conceder los permisos de macOS, el modo estándar funciona sin pedir permiso para cada app. Las apps que bloquees siguen fuera de su alcance; Pause suspende el uso del ordenador y Stop computer use termina las sesiones actuales. La aprobación estricta es opcional. Pide una vista previa en directo cuando quieras ver lo que hace.",
  updateTitle: "Actualizaciones cuando tú quieras",
  updateBody: "Elige Check for updates (buscar actualizaciones) en la aplicación. Antes de instalar una actualización, comprueba la descarga, la firma de Codewhale y la notarización de Apple, y conserva la versión anterior por si necesitas recuperarla.",
  help: "Configuración y solución de problemas",
  notes: "Notas de la versión",
  demo: "Ver la comprobación en segundo plano",
  source: "Ver en GitHub",
  platforms: "El asistente para Mac está en beta y el plugin del catálogo solo admite Mac. Las versiones publicadas también incluyen una vista previa experimental sin firmar para Windows x64; Linux requiere una configuración experimental desde el código fuente o con Docker. La entrada en Windows y Linux utiliza el escritorio compartido.",
  connectTitle: "Conecta Codewhale",
  connectVersion: "Codewhale 0.10.2 incluye un plugin Computer Use antiguo. Instalar la entrada del catálogo no lo actualiza. Conecta directamente el asistente de Mac instalado mediante MCP.",
  connectLimit: "La conexión MCP directa añade herramientas sin las habilidades del plugin. No la uses junto con el plugin integrado antiguo. Si está activado, ejecuta primero /plugin disable computer-use.",
  connectRuntime: "La aplicación incluye Node; no hace falta instalarlo por separado. Si está instalada en otra ubicación, asigna a APP la ruta real del paquete de la aplicación.",
  connectReload: "En una sesión de terminal en ejecución, ejecuta /mcp reload y pide:",
  connectPrompt: "Comprueba los permisos de Computer Use con request_access. No envíes ninguna entrada.",
  connectExpected: "Comprueba que la respuesta indique via: app, la versión del asistente y los permisos que falten. Si no se encuentra el asistente, detente y sigue la indicación de configuración. Esto solo comprueba la conexión y los permisos.",
  connectGuide: "Guía de configuración de MCP",
  contributeTitle: "Ayuda a mejorar Computer Use",
  contributeBody: "Mejora la documentación o prueba un flujo de trabajo en una aplicación y comparte resultados reproducibles.",
  contributeGuide: "Guía de contribución",
  contributeIssue: "Informar de un problema",
};
