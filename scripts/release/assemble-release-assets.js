#!/usr/bin/env node

const crypto = require("crypto");
const fs = require("fs/promises");
const path = require("path");

const {
  allReleaseAssetNames,
  BUNDLE_ASSET_NAMES,
  BUNDLE_CHECKSUM_MANIFEST,
  CHECKSUM_MANIFEST,
  CHECKSUM_SIGNATURE,
  checksummedReleaseAssetNames,
} = require("../../npm/codewhale/scripts/artifacts");

const compiledHosts = require("../../npm/codewhale/scripts/compiled-hosts");

async function readHostCatalog(directory) {
  const file = path.join(directory, compiledHosts.HOST_CATALOG);
  try {
    const metadata = await fs.lstat(file);
    if (!metadata.isFile() || metadata.isSymbolicLink()) throw new Error("compiled host catalog is not a regular file");
    return compiledHosts.parseCatalog(await fs.readFile(file, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
}

const WINDOWS_LAUNCHER = "codewhale.bat";

function usage() {
  return [
    "Usage:",
    "  node scripts/release/assemble-release-assets.js INPUT_DIR OUTPUT_DIR",
    "  node scripts/release/assemble-release-assets.js --verify ASSET_DIR",
  ].join("\n");
}

async function sha256(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(await fs.readFile(filePath));
  return hash.digest("hex");
}

function parseChecksumManifest(content, label) {
  const checksums = new Map();
  for (const line of content.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    const match = trimmed.match(/^([a-fA-F0-9]{64})\s+\*?(.+)$/);
    if (!match) {
      throw new Error(`${label} contains an invalid checksum row: ${trimmed}`);
    }
    const name = match[2];
    if (checksums.has(name)) {
      throw new Error(`${label} contains duplicate checksum rows for ${name}`);
    }
    checksums.set(name, match[1].toLowerCase());
  }
  return checksums;
}

function assertExactNames(actualNames, expectedNames, label) {
  const actual = new Set(actualNames);
  const expected = new Set(expectedNames);
  const missing = expectedNames.filter((name) => !actual.has(name));
  const unexpected = actualNames.filter((name) => !expected.has(name));
  if (missing.length > 0 || unexpected.length > 0 || actual.size !== actualNames.length) {
    throw new Error(
      `${label} does not match the authoritative inventory` +
        `${missing.length > 0 ? `; missing: ${missing.join(", ")}` : ""}` +
        `${unexpected.length > 0 ? `; unexpected: ${unexpected.join(", ")}` : ""}` +
        `${actual.size !== actualNames.length ? "; duplicate basenames are present" : ""}`,
    );
  }
}

async function assertManifest(directory, manifestName, expectedNames) {
  const manifestPath = path.join(directory, manifestName);
  const checksums = parseChecksumManifest(
    await fs.readFile(manifestPath, "utf8"),
    manifestName,
  );
  assertExactNames([...checksums.keys()], expectedNames, manifestName);
  for (const name of expectedNames) {
    const actual = await sha256(path.join(directory, name));
    if (checksums.get(name) !== actual) {
      throw new Error(`${manifestName} checksum mismatch for ${name}`);
    }
  }
}

async function verifyAssetDirectory(directory) {
  const entries = await fs.readdir(directory, { withFileTypes: true });
  const nonFiles = entries.filter((entry) => !entry.isFile());
  if (nonFiles.length > 0) {
    throw new Error(
      `Release asset directory must be flat; found: ${nonFiles.map((entry) => entry.name).join(", ")}`,
    );
  }

  const catalog = await readHostCatalog(directory);
  const expected = allReleaseAssetNames(catalog);
  const signature = entries.find((entry) => entry.name === CHECKSUM_SIGNATURE);
  if (signature && (await fs.stat(path.join(directory, CHECKSUM_SIGNATURE))).size > 1024) {
    throw new Error(`${CHECKSUM_SIGNATURE} is larger than a detached signature`);
  }
  assertExactNames(
    entries.map((entry) => entry.name).filter((name) => name !== CHECKSUM_SIGNATURE),
    expected,
    "Release asset directory",
  );
  await assertManifest(directory, CHECKSUM_MANIFEST, checksummedReleaseAssetNames(catalog));
  await assertManifest(directory, BUNDLE_CHECKSUM_MANIFEST, BUNDLE_ASSET_NAMES);
  if (catalog) compiledHosts.verifyDirectory(directory, catalog);
  console.log(`Verified ${expected.length} release assets in ${directory}`);
}

function windowsLauncherContents() {
  return [
    "@echo off",
    "where wt >nul 2>nul",
    "set NO_ANIMATIONS=1",
    'if "%ERRORLEVEL%"=="0" (',
    '    wt --title Codewhale cmd /k "%~dp0codewhale-windows-x64.exe"',
    ") else (",
    '    "%~dp0codewhale-windows-x64.exe"',
    ")",
    "",
  ].join("\r\n");
}

function intermediateArtifactPath(inputDirectory, name, catalog) {
  if (compiledHosts.assets(catalog).includes(name)) return path.join(inputDirectory, "codewhale-compiled-hosts", name);
  if (name === BUNDLE_CHECKSUM_MANIFEST || BUNDLE_ASSET_NAMES.includes(name)) {
    return path.join(inputDirectory, "codewhale-bundles", name);
  }
  return path.join(inputDirectory, name, name);
}

async function assemble(inputDirectory, outputDirectory) {
  const catalog = await readHostCatalog(path.join(inputDirectory, "codewhale-compiled-hosts"));
  const expected = allReleaseAssetNames(catalog);
  const generated = new Set([WINDOWS_LAUNCHER, CHECKSUM_MANIFEST]);
  const copiedNames = expected.filter((name) => !generated.has(name));
  const sources = new Map();
  for (const name of copiedNames) {
    const source = intermediateArtifactPath(inputDirectory, name, catalog);
    let sourceStat;
    try {
      sourceStat = await fs.lstat(source);
    } catch (error) {
      if (error && error.code === "ENOENT") {
        throw new Error(`Downloaded release artifacts are missing ${name} at ${source}`);
      }
      throw error;
    }
    if (!sourceStat.isFile()) {
      throw new Error(`Downloaded release artifact must be a regular file: ${source}`);
    }
    sources.set(name, source);
  }

  await fs.mkdir(outputDirectory, { recursive: true });
  const existing = await fs.readdir(outputDirectory);
  if (existing.length > 0) {
    throw new Error(`Output directory must be empty: ${outputDirectory}`);
  }

  for (const name of copiedNames) {
    await fs.copyFile(sources.get(name), path.join(outputDirectory, name));
  }
  await fs.writeFile(
    path.join(outputDirectory, WINDOWS_LAUNCHER),
    windowsLauncherContents(),
    "utf8",
  );

  const checksumRows = [];
  for (const name of [...checksummedReleaseAssetNames(catalog)].sort()) {
    checksumRows.push(`${await sha256(path.join(outputDirectory, name))}  ${name}`);
  }
  await fs.writeFile(
    path.join(outputDirectory, CHECKSUM_MANIFEST),
    `${checksumRows.join("\n")}\n`,
    "utf8",
  );

  await verifyAssetDirectory(outputDirectory);
}

async function main() {
  if (process.argv[2] === "--verify") {
    if (process.argv.length !== 4) {
      throw new Error(usage());
    }
    await verifyAssetDirectory(path.resolve(process.argv[3]));
    return;
  }
  if (process.argv.length !== 4) {
    throw new Error(usage());
  }
  await assemble(path.resolve(process.argv[2]), path.resolve(process.argv[3]));
}

if (require.main === module) {
  main().catch((error) => {
    console.error(`Release asset assembly failed: ${error.message}`);
    process.exit(1);
  });
}

module.exports = {
  assemble,
  parseChecksumManifest,
  verifyAssetDirectory,
  windowsLauncherContents,
};
