#!/usr/bin/env node
/**
 * Website/docs General Translation pipeline.
 *
 * Runtime authority stays web/lib/i18n/dictionaries (one logical path).
 * This script exports those dictionaries to web/gt-catalog/[locale].json
 * so the MIT `gt` CLI can translate updated English copy, then imports
 * reviewed JSON back into the same dictionary files.
 *
 * Not for the TUI. Not for model completions. Not a /translate replacement.
 * Never call `gt generate` here — that scanner is framework-only and would
 * look for <T> JSX this site does not use.
 */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(webRoot, "..");
const configPath = path.join(webRoot, "gt.config.json");
const catalogDir = path.join(webRoot, "gt-catalog");
const dictDir = path.join(webRoot, "lib", "i18n", "dictionaries");
const pinnedCli = "2.17.2";
const tuiLocaleDir = path.join(repoRoot, "crates", "tui", "locales");
const promptPath = path.join(repoRoot, "crates", "tui", "src", "prompts", "text.rs");

const STEMS = {
  features: { exportName: "features", typeName: "FeaturesDict" },
  "docs-skills": { exportName: "docsSkills", typeName: "DocsPageDict" },
  "docs-claude-code": { exportName: "docsClaudeCode", typeName: "DocsPageDict" },
  chrome: { exportName: "chrome", typeName: "ChromeDict" },
  home: { exportName: "home", typeName: "HomeDict" },
  "docs-guide": { exportName: "docsGuide", typeName: "DocsGuideDict" },
  "docs-shell": { exportName: "docsShell", typeName: "DocsShellDict" },
  "docs-hooks": { exportName: "docsHooks", typeName: "DocsHooksDict" },
  "docs-troubleshooting": { exportName: "docsTroubleshooting", typeName: "DocsTroubleshootingDict" },
  "docs-configuration": { exportName: "docsConfiguration", typeName: "DocsConfigurationDict" },
  "docs-constitution": { exportName: "docsConstitution", typeName: "DocsConstitutionDict" },
  "docs-fleet": { exportName: "docsFleet", typeName: "DocsFleetDict" },
  "docs-mcp": { exportName: "docsMcp", typeName: "DocsMcpDict" },
  "docs-modes": { exportName: "docsModes", typeName: "DocsModesDict" },
  "docs-runtime-api": { exportName: "docsRuntimeApi", typeName: "DocsRuntimeApiDict" },
  "docs-sandbox": { exportName: "docsSandbox", typeName: "DocsSandboxDict" },
  "docs-subagents": { exportName: "docsSubagents", typeName: "DocsSubagentsDict" },
  "docs-web": { exportName: "docsWeb", typeName: "DocsWebDict" },
  "docs-auth": { exportName: "docsAuth", typeName: "DocsAuthDict" },
  "docs-trust": { exportName: "docsTrust", typeName: "DocsTrustDict" },
  states: { exportName: "states", typeName: "StatesDict" },
  changelog: { exportName: "changelog", typeName: "ChangelogDict" },
  "computer-use": { exportName: "computerUse", typeName: "ComputerUseDict" },
};

const REQUIRED_STEMS = ["chrome", "home"];

function fail(message) {
  throw new Error(message);
}

function assertExactKeys(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    fail(`${label} keys must be exactly: ${wanted.join(", ")} (found: ${actual.join(", ")})`);
  }
}

function stableStringify(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

function deepEqual(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

async function readConfig() {
  const config = JSON.parse(await readFile(configPath, "utf8"));
  assertExactKeys(config, ["defaultLocale", "locales", "files"], "gt.config.json");
  assertExactKeys(config.files, ["json"], "gt.config.json files");
  assertExactKeys(config.files.json, ["include"], "gt.config.json files.json");
  if (config.defaultLocale !== "en") fail("defaultLocale must remain en");
  if (
    !Array.isArray(config.locales) ||
    config.locales.length === 0 ||
    !config.locales.every((locale) => typeof locale === "string" && locale.length > 0) ||
    new Set(config.locales).size !== config.locales.length
  ) {
    fail("locales must be a non-empty, duplicate-free string array");
  }
  if (config.locales.includes("en")) fail("locales must not include the default locale");
  const include = config.files.json.include;
  if (!Array.isArray(include) || include.length !== 1 || include[0] !== "gt-catalog/[locale].json") {
    fail("the JSON source must remain web/gt-catalog/[locale].json");
  }
  for (const locale of config.locales) {
    if (locale.includes("..") || locale.includes("/") || locale.includes("\\")) {
      fail(`locale ${locale} is not a safe catalog name`);
    }
  }
  return config;
}

async function assertWebsiteOnlyAsync() {
  const source = await readFile(configPath, "utf8");
  if (source.includes("crates/tui") || source.includes("prompts/text.rs")) {
    fail("gt.config.json must not mention TUI paths");
  }
  if (!source.includes("gt-catalog/[locale].json")) {
    fail("gt.config.json must target the website catalog only");
  }
}

async function loadDictionaryModule(locale, stem) {
  const spec = STEMS[stem];
  if (!spec) fail(`unknown dictionary stem ${stem}`);
  const filePath = path.join(dictDir, locale, `${stem}.ts`);
  if (!existsSync(filePath)) return null;
  const ts = await import("typescript");
  const source = await readFile(filePath, "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
    },
    fileName: filePath,
  });
  const mod = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
  const value = mod[spec.exportName];
  if (value == null) fail(`missing export ${spec.exportName} in ${filePath}`);
  return value;
}

async function loadCatalogFromDictionaries(locale) {
  const catalog = {};
  for (const stem of Object.keys(STEMS)) {
    const value = await loadDictionaryModule(locale, stem);
    if (value == null) {
      if (REQUIRED_STEMS.includes(stem) && locale === "en") {
        fail(`English is missing required dictionary ${stem}.ts`);
      }
      continue;
    }
    catalog[stem] = value;
  }
  return catalog;
}

async function readCatalogFile(locale) {
  const filePath = path.join(catalogDir, `${locale}.json`);
  const source = await readFile(filePath, "utf8");
  let parsed;
  try {
    parsed = JSON.parse(source);
  } catch (error) {
    fail(`${filePath} is not valid JSON: ${error.message}`);
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    fail(`${filePath} must be a JSON object of dictionary stems`);
  }
  for (const stem of Object.keys(parsed)) {
    if (!STEMS[stem]) fail(`${filePath} contains unknown stem ${stem}`);
  }
  for (const stem of REQUIRED_STEMS) {
    if (!parsed[stem]) fail(`${filePath} is missing required stem ${stem}`);
  }
  return parsed;
}

async function writeCatalogFile(locale, catalog) {
  await mkdir(catalogDir, { recursive: true });
  const filePath = path.join(catalogDir, `${locale}.json`);
  await writeFile(filePath, stableStringify(catalog));
  return filePath;
}

function emitTsValue(value, indent) {
  const pad = " ".repeat(indent);
  if (typeof value === "string") return JSON.stringify(value);
  if (Array.isArray(value)) {
    if (value.every((row) => Array.isArray(row) && row.every((cell) => typeof cell === "string"))) {
      const rows = value.map((row) => `${pad}  [${row.map((cell) => JSON.stringify(cell)).join(", ")}]`);
      return `[\n${rows.join(",\n")},\n${pad}]`;
    }
    fail("catalog arrays must be [string, string] rows");
  }
  if (value && typeof value === "object") {
    const keys = Object.keys(value);
    const lines = keys.map((key) => `${pad}  ${key}: ${emitTsValue(value[key], indent + 2)},`);
    return `{\n${lines.join("\n")}\n${pad}}`;
  }
  fail(`unsupported catalog value type ${typeof value}`);
}

async function writeDictionaryFile(locale, stem, value) {
  const spec = STEMS[stem];
  const filePath = path.join(dictDir, locale, `${stem}.ts`);
  await mkdir(path.dirname(filePath), { recursive: true });
  const body = `import type { ${spec.typeName} } from "../types";

/**
 * Website dictionary for locale \`${locale}\`.
 * Runtime authority is this file (web/lib/i18n/dictionaries).
 * Source/sink for General Translation is web/gt-catalog/${locale}.json.
 */
export const ${spec.exportName}: ${spec.typeName} = ${emitTsValue(value, 0)};
`;
  await writeFile(filePath, body);
  return filePath;
}

async function check() {
  const config = await readConfig();
  await assertWebsiteOnlyAsync();
  if (existsSync(tuiLocaleDir)) {
    const tuiPacks = (await readdir(tuiLocaleDir)).filter((name) => name.endsWith(".json"));
    if (tuiPacks.length === 0) fail("TUI locale directory exists but is empty — refusing to proceed");
  }
  if (existsSync(promptPath)) {
    const digest = createHash("sha256").update(await readFile(promptPath)).digest("hex");
    if (digest.length !== 64) fail("could not fingerprint prompts/text.rs");
  }

  const locales = [config.defaultLocale, ...config.locales];
  for (const locale of locales) {
    const fromDict = await loadCatalogFromDictionaries(locale);
    const fromFile = await readCatalogFile(locale);
    if (!deepEqual(fromDict, fromFile)) {
      fail(
        `gt-catalog/${locale}.json is out of sync with web/lib/i18n/dictionaries/${locale}/ — run npm run i18n:gt -- export`,
      );
    }
    for (const stem of Object.keys(fromFile)) {
      if (REQUIRED_STEMS.includes(stem)) continue;
      const enValue = (await loadCatalogFromDictionaries("en"))[stem];
      if (enValue && !deepEqual(Object.keys(fromFile[stem]).sort(), Object.keys(enValue).sort())) {
        fail(`gt-catalog/${locale}.json stem ${stem} does not have English key parity`);
      }
    }
  }
  console.log(
    `Website GT catalog OK — ${config.locales.join(", ")} (local JSON; TUI packs untouched; no API)`,
  );
}

async function exportCatalogs() {
  const config = await readConfig();
  const locales = [config.defaultLocale, ...config.locales];
  const written = [];
  for (const locale of locales) {
    const catalog = await loadCatalogFromDictionaries(locale);
    written.push(await writeCatalogFile(locale, catalog));
  }
  console.log(`Exported ${written.length} website catalogs:\n${written.map((file) => `  ${path.relative(webRoot, file)}`).join("\n")}`);
}

async function importCatalogs() {
  const config = await readConfig();
  const written = [];
  for (const locale of config.locales) {
    const catalog = await readCatalogFile(locale);
    for (const [stem, value] of Object.entries(catalog)) {
      written.push(await writeDictionaryFile(locale, stem, value));
    }
  }
  console.log(
    `Imported ${written.length} website dictionaries from gt-catalog (TUI packs not written):\n${written
      .map((file) => `  ${path.relative(webRoot, file)}`)
      .join("\n")}`,
  );
}

async function validatePinnedCli() {
  const packagePath = path.join(webRoot, "node_modules", "gt", "package.json");
  let packageJson;
  try {
    packageJson = JSON.parse(await readFile(packagePath, "utf8"));
  } catch {
    fail("gt is not installed locally; run npm ci in web/ (this wrapper never downloads packages)");
  }
  if (packageJson.version !== pinnedCli) {
    fail(`expected the locked gt ${pinnedCli} package (found ${packageJson.version})`);
  }
  const bin = packageJson.bin;
  const binPath = typeof bin === "string" ? bin : bin.gt;
  if (!binPath) fail("gt package.json is missing a bin");
  return path.join(path.dirname(packagePath), binPath);
}

function translateEnvironment() {
  const key = process.env.GT_API_KEY?.trim();
  const project = process.env.GT_PROJECT_ID?.trim();
  if (!key || !project) {
    fail(
      "gt translate is fail-closed. Set BYOK env GT_API_KEY and GT_PROJECT_ID (never commit them). Local export/import/check do not need a key.",
    );
  }
  const environment = { ...process.env };
  environment.NO_COLOR = "1";
  environment.GT_API_KEY = key;
  environment.GT_PROJECT_ID = project;
  return environment;
}

async function translate() {
  await readConfig();
  await assertWebsiteOnlyAsync();
  const environment = translateEnvironment();
  const cli = await validatePinnedCli();
  const result = spawnSync(
    process.execPath,
    [cli, "--skip-version-check", "translate", "--config", "gt.config.json"],
    {
      cwd: webRoot,
      env: environment,
      stdio: "inherit",
      windowsHide: true,
    },
  );
  if (result.error) fail(result.error.message);
  if (result.status !== 0) fail(`gt translate exited with status ${result.status ?? "unknown"}`);
  console.log("gt translate finished — review gt-catalog/*.json then run npm run i18n:gt -- import");
}

function usage() {
  console.log(`Usage: node scripts/gt-site.mjs <check|export|import|translate>

  check      catalogs match dictionaries; website-only schema
  export     dictionaries → gt-catalog/[locale].json (no API)
  import     gt-catalog → website dictionary TS only (no TUI writes)
  translate  fail-closed without GT_API_KEY + GT_PROJECT_ID

Never wraps inference. Never edits crates/engine/src/prompts/text.rs.`);
}

const command = process.argv[2] ?? "check";

try {
  if (command === "check") await check();
  else if (command === "export") await exportCatalogs();
  else if (command === "import") await importCatalogs();
  else if (command === "translate") await translate();
  else if (command === "-h" || command === "--help") usage();
  else {
    usage();
    fail(`unknown command ${command}`);
  }
} catch (error) {
  console.error(`[gt-site] FAIL — ${error.message}`);
  process.exitCode = 1;
}
