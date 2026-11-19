import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { resolve, sep } from "node:path";
import { createHash } from "node:crypto";

export function readDocTopics(webDir) {
  const path = resolve(webDir, "lib", "docs-map.ts");
  if (!existsSync(path)) throw new Error(`missing docs registry: ${path}`);
  const source = readFileSync(path, "utf8");
  const topics = [];
  const pattern = /\{\s*id:\s*"(\w[^"]*)",\s*slug:\s*"(\w[^"]*)",[\s\S]*?repoSource:\s*(\[[^\]]+\]|"[^"]+")/g;
  for (const match of source.matchAll(pattern)) {
    const paths = match[3].startsWith("[")
      ? match[3].match(/"([^"]+)"/g)?.map((value) => value.slice(1, -1)) ?? []
      : [match[3].slice(1, -1)];
    topics.push({ id: match[1], slug: match[2], repoSource: paths });
  }
  if (topics.length === 0) throw new Error("empty docs registry");
  return topics;
}

export function deriveAgentDocs(webDir, repoRoot) {
  return readDocTopics(webDir).map((topic) => ({
    id: topic.id, slug: topic.slug,
    sources: topic.repoSource.map((path) => {
      const absolute = resolve(repoRoot, path.split("#", 1)[0]);
      if (!absolute.startsWith(resolve(repoRoot) + sep)) throw new Error(`invalid doc source: ${path}`);
      const content = readFileSync(absolute, "utf8");
      return { path, sha256: createHash("sha256").update(content).digest("hex"), content };
    }),
  }));
}

export function writeAgentDocs(webDir, repoRoot) {
  writeFileSync(
    resolve(webDir, "data", "agent-docs.generated.json"),
    `${JSON.stringify(deriveAgentDocs(webDir, repoRoot), null, 2)}\n`,
  );
}
