import { readFileSync } from "node:fs";

const [rulesPath, lane] = process.argv.slice(2);
if (!rulesPath || !lane) {
  console.error("usage: check-allowlist.mjs <rules-file> <lane>  (git diff --name-status on stdin)");
  process.exit(2);
}

const toRegExp = (glob) =>
  new RegExp(
    "^" +
      glob
        .replace(/[.+^${}()|[\]\\]/g, "\\$&")
        .replace(/\*\*\//g, "\u0000")
        .replace(/\*\*/g, "\u0001")
        .replace(/\*/g, "[^/]*")
        .replace(/\u0000/g, "(?:.*/)?")
        .replace(/\u0001/g, ".*") +
      "$",
  );

const allow = [];
const deny = [];
for (const raw of readFileSync(rulesPath, "utf8").split("\n")) {
  const line = raw.trim();
  if (!line) continue;
  if (line.startsWith("!")) {
    deny.push(toRegExp(line.slice(1)));
    continue;
  }
  const split = line.indexOf(": ");
  if (split < 1) continue;
  if (line.slice(0, split) === lane) allow.push(toRegExp(line.slice(split + 2)));
}

if (allow.length === 0) {
  console.error(`::error::no allowlist entries for lane "${lane}"`);
  process.exit(1);
}

const changes = readFileSync(0, "utf8").split("\n").filter(Boolean);
const bad = [];
for (const entry of changes) {
  const [status, path, ...rest] = entry.split("\t");
  const unsafe = !path || rest.length > 0 || path.startsWith("/") || path.split("/").includes("..") || path.includes("\\");
  if (status !== "M" || unsafe || deny.some((re) => re.test(path)) || !allow.some((re) => re.test(path))) {
    bad.push(entry.replace(/\t/g, " "));
  }
}

if (bad.length > 0) {
  console.error(`::error::lane "${lane}" changed paths outside its allowlist or by something other than an in-place edit:`);
  for (const entry of bad) console.error(`  ${entry}`);
  process.exit(1);
}
console.log(`lane "${lane}": ${changes.length} changed file(s), all inside the allowlist.`);
