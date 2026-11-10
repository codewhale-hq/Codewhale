import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export function runtimeRouteInventory(source) {
  const rows = new Map();
  const pattern = /\.route\(\s*"([^"\n]+)"\s*,/g;
  for (const match of source.matchAll(pattern)) {
    let depth = 1;
    let quoted = false;
    let escaped = false;
    let lineComment = false;
    let blockComment = false;
    let end = match.index + match[0].length;
    for (; end < source.length && depth; end++) {
      const char = source[end], next = source[end + 1];
      if (lineComment) { if (char === "\n") lineComment = false; continue; }
      if (blockComment) { if (char === "*" && next === "/") { blockComment = false; end++; } continue; }
      if (quoted) {
        if (escaped) escaped = false;
        else if (char === "\\") escaped = true;
        else if (char === '"') quoted = false;
        continue;
      }
      if (char === "/" && next === "/") { lineComment = true; end++; continue; }
      if (char === "/" && next === "*") { blockComment = true; end++; continue; }
      if (char === '"') quoted = true;
      else if (char === "(") depth++;
      else if (char === ")") depth--;
    }
    if (depth) throw new Error(`Unclosed route registration: ${match[1]}`);
    const body = source.slice(match.index + match[0].length, end);
    const methods = [...body.matchAll(/\b(get|post|put|patch|delete|options|head|any)\s*\(/g)].map((method) => method[1].toUpperCase());
    if (!methods.length) throw new Error(`Unknown HTTP method registration: ${match[1]}`);
    const registered = rows.get(match[1]) ?? new Set();
    for (const method of methods) registered.add(method);
    rows.set(match[1], registered);
  }
  return [...rows].sort(([a], [b]) => a.localeCompare(b)).map(([path, methods]) => ({ path, methods: [...methods].sort() }));
}

export function renderRuntimeRouteInventory(source) {
  const rows = runtimeRouteInventory(source);
  return `# Runtime HTTP route inventory\n\nThis table is generated from literal route registrations in\n\`crates/app-server/src/runtime_api.rs\`. It names the paths and methods that\nthe current source registers, including native-client and compatibility\nsurfaces. A registered path is not a grant of permission or a promise that\na feature is enabled in your installed version.\n\nThe [Runtime API contract](RUNTIME_API.md) explains request fields, events,\nauthorization and compatibility. This inventory is a navigation index, not\na replacement for those schemas; routes used by the native client can have\nclient-specific contracts. Provider calls, file mutations and connected-service\nactions remain subject to their documented authority boundaries.\n\nRun \`node web/scripts/check-runtime-routes.mjs --write\` after changing a\nregistration; \`npm run check:docs\` refuses a stale inventory. Dynamic nested\nrouters, fallback handlers and static-asset mounts are outside this literal\nregistration index.\n\n| HTTP method | Path |\n| --- | --- |\n${rows.map((row) => `| ${row.methods.join(" / ")} | \`${row.path}\` |`).join("\n")}\n\nThe source contains ${rows.length} distinct literal paths in this table.\n`;
}

const sourcePath = new URL("../../crates/app-server/src/runtime_api.rs", import.meta.url);
const documentPath = new URL("../../docs/RUNTIME_ROUTES.md", import.meta.url);
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const source = readFileSync(sourcePath, "utf8");
  const expected = renderRuntimeRouteInventory(source);
  if (process.argv.includes("--write")) writeFileSync(documentPath, expected);
  else if (readFileSync(documentPath, "utf8") !== expected) {
    console.error("[runtime-routes] FAIL: docs/RUNTIME_ROUTES.md differs from registered source paths; regenerate it.");
    process.exit(1);
  }
  console.log(`[runtime-routes] PASS: ${runtimeRouteInventory(source).length} literal HTTP paths match source.`);
}
