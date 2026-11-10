import { readdirSync, readFileSync, statSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

const out = process.argv[2];
if (!out) {
  console.error("usage: link-report.mjs <output.json>");
  process.exit(2);
}

const roots = ["web/lib/content", "web/lib/i18n/dictionaries/en"];
const urlPattern = /https?:\/\/[^\s"'`<>)\\]+/g;
const found = new Map();

const walk = (dir) => {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path);
    else if (/\.(ts|tsx)$/.test(name) && !/\.(test|generated)\./.test(name)) {
      for (const match of readFileSync(path, "utf8").matchAll(urlPattern)) {
        const url = match[0].replace(/[.,;:]+$/, "");
        if (!found.has(url)) found.set(url, new Set());
        found.get(url).add(path);
      }
    }
  }
};
for (const root of roots) walk(root);

const urls = [...found.keys()].filter((url) => !/[{$]/.test(url)).slice(0, 400);
const results = [];
let next = 0;

const probe = async (url) => {
  for (const method of ["HEAD", "GET"]) {
    try {
      const res = await fetch(url, {
        method,
        redirect: "follow",
        signal: AbortSignal.timeout(12_000),
        headers: { "user-agent": "codewhale-web-maintainer-link-check" },
      });
      if (res.status < 400) return { ok: true, status: res.status, finalUrl: res.url };
      if (method === "GET" || res.status === 404 || res.status === 410) return { ok: false, status: res.status, finalUrl: res.url };
    } catch (error) {
      if (method === "GET") return { ok: false, status: 0, error: error instanceof Error ? error.name : "error" };
    }
  }
  return { ok: false, status: 0 };
};

await Promise.all(
  Array.from({ length: 8 }, async () => {
    while (next < urls.length) {
      const url = urls[next++];
      const result = await probe(url);
      const moved = result.ok && result.finalUrl && result.finalUrl.replace(/\/$/, "") !== url.replace(/\/$/, "");
      if (!result.ok || moved) results.push({ url, ...result, files: [...found.get(url)].sort() });
    }
  }),
);

results.sort((a, b) => a.url.localeCompare(b.url));
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, JSON.stringify({ checked: urls.length, flagged: results }, null, 2) + "\n");
console.log(`link-report: ${urls.length} checked, ${results.length} flagged`);
