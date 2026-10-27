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
  lead: "Deja que Codewhale trabaje en tus aplicaciones mientras sigues trabajando. Dale permiso una vez en este Mac y detén el uso del ordenador cuando quieras.",
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
    { title: "Activa Computer Use", body: "En los ajustes de Connectors de Codewhale, instala o actualiza Computer Use y luego revísalo y actívalo. Pídele que trabaje en una aplicación. En el primer uso, elige Trust until revoked en el asistente nativo; esa confianza se mantiene entre aplicaciones, modelos y tareas." },
  ],
  controlsTitle: "Mantén el control",
  controlsBody: "El control compacto de Codewhale aparece junto a la aplicación en uso. Pause suspende el uso del ordenador; Stop computer use termina todas las sesiones actuales del ordenador. Elige Turn off en el asistente para retirar el acceso de Codewhale. Pide una vista previa en directo cuando quieras ver lo que hace.",
  updateTitle: "Actualizaciones cuando tú quieras",
  updateBody: "Elige Check for updates (buscar actualizaciones) en la aplicación. Antes de instalar una actualización, comprueba la descarga, la firma de Codewhale y la notarización de Apple, y conserva la versión anterior por si necesitas recuperarla.",
  help: "Configuración y solución de problemas",
  notes: "Notas de la versión",
  demo: "Ver la comprobación en segundo plano",
  source: "Código fuente y otras plataformas",
  platforms: "Esta descarga es para Mac. En Windows y Linux, por ahora, se usa el plugin desde el código fuente y la configuración del lado del host.",
};
