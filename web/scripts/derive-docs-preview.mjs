#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { createCssVariablesTheme, createHighlighter } from "shiki";
import { buildInstallGuide } from "./install-guide-lib.mjs";
import { runtimeRouteInventory } from "./check-runtime-routes.mjs";

const docsDir = new URL("../../docs/", import.meta.url);
const routeSource = new URL("../../crates/app-server/src/runtime_api.rs", import.meta.url);
const target = new URL("../lib/docs-preview.generated.ts", import.meta.url);

const PAGES = [
  { slug: "install", source: "INSTALL.md" },
  { slug: "quickstart", source: "GUIDE.md" },
  { slug: "modes", source: "MODES.md" },
];
const LANGUAGES = [
  { key: "en", lang: "en", dir: "", tableLabel: "Table" },
  { key: "zh", lang: "zh-Hans", dir: "zh_hans/", tableLabel: "表格" },
];
const FENCES = { bash: "bash", sh: "bash", shell: "bash", powershell: "powershell", toml: "toml", json: "json", gitignore: "ini" };
const SITE_PATHS = new Map(PAGES.flatMap((page) => LANGUAGES.map((language) => [`${language.dir}${page.source}`, `/docs-preview/${page.slug}`])));

const theme = createCssVariablesTheme({ name: "codewhale", variablePrefix: "--tk-", fontStyle: false });
const highlighter = await createHighlighter({ themes: [theme], langs: [...new Set(Object.values(FENCES))] });

const escapeHtml = (text) => text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");

function highlight(code, fence) {
  const lang = FENCES[fence];
  if (!lang) return null;
  return highlighter.codeToTokensBase(code, { lang, theme: "codewhale" }).map((line) => line.map((token) => {
    const role = token.color?.match(/^var\(--tk-token-([a-z-]+)\)$/)?.[1];
    return role ? `<span class="tk-${role}">${escapeHtml(token.content)}</span>` : escapeHtml(token.content);
  }).join("")).join("\n");
}

function renderPage(page, language) {
  const sourcePath = `${language.dir}${page.source}`;
  const url = new URL(sourcePath, docsDir);
  if (!existsSync(url)) return null;
  const source = readFileSync(url, "utf8");
  const title = source.match(/^# (.+)$/m)?.[1];
  if (!title) throw new Error(`[docs-preview] ${sourcePath} has no title heading`);
  const body = source
    .replace(/^# .+\n+/, "")
    .replace(/^> [^\n]*\]\((?:zh_hans\/|\.\.\/)[\w.]+\)[^\n]*\n+/, "");
  const guide = buildInstallGuide(body, { sourcePath, tableLabel: language.tableLabel, resolveLink: (path) => SITE_PATHS.get(path) });
  return {
    title,
    lang: language.lang,
    sourcePath: `docs/${sourcePath}`,
    sourceHash: createHash("sha256").update(source).digest("hex"),
    headings: guide.headings.filter((heading) => heading.depth <= 3),
    chunks: guide.chunks.map((chunk) => chunk.kind === "code" ? { ...chunk, html: highlight(chunk.text, chunk.lang) } : chunk),
    searchSections: [{ id: "", title, text: guide.preamble }, ...guide.searchSections],
    markdown: `# ${title}\n\n${body}`,
  };
}

function routeGroups() {
  const groups = new Map();
  for (const row of runtimeRouteInventory(readFileSync(routeSource, "utf8"))) {
    const segments = row.path.split("/").filter(Boolean);
    const name = segments[0] === "v1" ? segments[1] : segments[0] === "__codewhale" ? "bootstrap" : segments[0] ?? "root";
    groups.set(name, [...(groups.get(name) ?? []), row]);
  }
  return [...groups].sort(([a], [b]) => a.localeCompare(b)).map(([name, routes]) => ({ name, routes }));
}

const pages = Object.fromEntries(PAGES.map((page) => [
  page.slug,
  Object.fromEntries(LANGUAGES.map((language) => [language.key, renderPage(page, language)]).filter(([, rendered]) => rendered)),
]));
const routes = { sourcePath: "crates/app-server/src/runtime_api.rs", sourceHash: createHash("sha256").update(readFileSync(routeSource)).digest("hex"), groups: routeGroups() };

writeFileSync(target, `// Generated from docs/*.md and crates/app-server/src/runtime_api.rs by scripts/derive-docs-preview.mjs. Do not edit.\n\nexport const DOCS_PREVIEW_PAGES = ${JSON.stringify(pages, null, 1)};\n\nexport const DOCS_PREVIEW_ROUTES = ${JSON.stringify(routes, null, 1)};\n`);
console.log(`[docs-preview] rendered ${PAGES.length} Markdown pages in ${LANGUAGES.length} languages and ${routes.groups.reduce((sum, group) => sum + group.routes.length, 0)} runtime routes`);
