#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, writeFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = path.join(webRoot, "data", "desktop-media.json");
const THEMES = ["light", "dark"];
const FAKE = /scripted|website-demo|simulator|fixture|loopback/i;

const args = process.argv.slice(2);
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const outDir = path.join(webRoot, manifest.outDir);
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const fileName = (shot, theme, width, format) => `${shot.id}-${theme}-${width}.${format}`;

function expectedFiles() {
  return manifest.shots.flatMap((shot) =>
    THEMES.flatMap((theme) => manifest.widths.flatMap((width) => manifest.formats.map((format) => fileName(shot, theme, width, format)))),
  );
}

function check() {
  const errors = [];
  if (manifest.capture.simulator !== false) errors.push("capture.simulator must be false");
  for (const key of ["model", "provider", "engine", "app"]) {
    if (FAKE.test(String(manifest.capture[key] ?? ""))) errors.push(`capture.${key} names a simulated or scripted source`);
  }
  const wanted = new Set(expectedFiles());
  for (const name of wanted) {
    const record = manifest.outputs[name];
    const file = path.join(outDir, name);
    if (!record) errors.push(`${name}: not recorded; run with --source`);
    else if (!existsSync(file)) errors.push(`${name}: missing from ${manifest.outDir}`);
    else {
      const bytes = readFileSync(file);
      if (sha(bytes) !== record.sha256) errors.push(`${name}: bytes differ from the recorded SHA-256`);
      if (bytes.length > manifest.budgetBytes) errors.push(`${name}: ${bytes.length} bytes is over the ${manifest.budgetBytes} budget`);
    }
  }
  for (const name of Object.keys(manifest.outputs)) if (!wanted.has(name)) errors.push(`${name}: recorded but no shot produces it`);
  if (existsSync(outDir)) {
    for (const name of readdirSync(outDir)) if (!wanted.has(name)) errors.push(`${name}: stray file in ${manifest.outDir}`);
  }
  if (errors.length) {
    console.error(`desktop media check failed:\n- ${errors.join("\n- ")}`);
    process.exit(1);
  }
  console.log(`desktop media is current (${wanted.size} files)`);
}

async function build(source) {
  const { default: sharp } = await import("sharp");
  mkdirSync(outDir, { recursive: true });
  const outputs = {};
  for (const shot of manifest.shots) {
    for (const theme of THEMES) {
      const input = path.join(source, `${shot.source}-${theme}.png`);
      if (!existsSync(input)) throw new Error(`missing source ${input}`);
      for (const width of manifest.widths) {
        for (const format of manifest.formats) {
          let pipeline = sharp(input);
          if (shot.crop) pipeline = pipeline.extract(shot.crop);
          pipeline = pipeline.resize({ width, withoutEnlargement: true });
          pipeline = format === "avif" ? pipeline.avif({ quality: 58, effort: 6 }) : pipeline.webp({ quality: 80, effort: 6 });
          const { data, info } = await pipeline.toBuffer({ resolveWithObject: true });
          const name = fileName(shot, theme, width, format);
          writeFileSync(path.join(outDir, name), data);
          outputs[name] = { width: info.width, height: info.height, bytes: data.length, sha256: sha(data) };
        }
      }
    }
  }
  manifest.outputs = outputs;
  writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  check();
}

if (args.includes("--source")) await build(path.resolve(args[args.indexOf("--source") + 1]));
else if (args.includes("--check") || args.length === 0) check();
else {
  console.error("usage: build-desktop-media.mjs --source <dir> | --check");
  process.exit(2);
}
