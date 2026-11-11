import type { ContributeDict } from "../types";

/** Spanish dictionary for `app/[locale]/contribute/page.tsx`. */
export const contribute: ContributeDict = {
  metaTitle: "Contribuir · Codewhale",
  metaDescription:
    "Cualquier comentario ayuda a Codewhale, con o sin código. Descubre adónde enviar un error, una idea, una traducción, un proveedor o una corrección.",
  kicker: "Construyamos Codewhale juntos",
  title: "Ayuda a construir Codewhale",
  lede: "Cualquier comentario ayuda, con o sin código, y basta con un error, una idea o una pantalla que se veía mal para empezar.",
  fileIssue: "Abrir una incidencia",
  browsePulls: "Ver las pull requests",
  fullGuide: "Guía para colaboradores",
  pathsTitle: "Elige por dónde empezar",
  workflowTitle: "Envía un cambio que otros puedan revisar",
  reviewTitle: "Muestra qué cambió y cómo lo comprobaste",
  reviewScope:
    "Cuéntales a los revisores cuál es el problema, por qué haces el cambio, qué pruebas lo respaldan y qué riesgo queda. Un alcance acotado recibe comentarios más claros.",
  devTitle: "Compila y comprueba tu cambio de código",
  devScope:
    "Si es código Rust, compila la parte correspondiente y ejecuta sus comprobaciones específicas. Sigue la guía para colaboradores en las comprobaciones más amplias que necesite tu cambio.",
  mergedAccountsCount: {
    one: "{n} cuenta de GitHub tuvo un pull request fusionado en los últimos 30 días.",
    other: "{n} cuentas de GitHub tuvieron pull requests fusionados en los últimos 30 días.",
  },
  mergedPeopleCount: {
    one: "{n} persona tuvo un pull request fusionado en los últimos 30 días.",
    other: "{n} personas tuvieron pull requests fusionados en los últimos 30 días.",
  },
};
