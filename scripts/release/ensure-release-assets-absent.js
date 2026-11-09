#!/usr/bin/env node

const { execFileSync } = require("node:child_process");

function usage() {
  return "Usage: node scripts/release/ensure-release-assets-absent.js OWNER/REPO vX.Y.Z";
}

function validateTarget(repo, tag) {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo)) {
    throw new Error(`Invalid GitHub repository: ${repo}`);
  }
  if (!/^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$/.test(tag)) {
    throw new Error(`Invalid release tag: ${tag}`);
  }
}

function isNotFoundError(error) {
  return (
    error &&
    error.status !== 0 &&
    /\bHTTP 404\b/.test(String(error.stderr || ""))
  );
}

function fetchRelease(repo, tag, ghBin = process.env.GH_BIN || "gh", exec = execFileSync) {
  validateTarget(repo, tag);
  const endpoint = `repos/${repo}/releases/tags/${encodeURIComponent(tag)}`;
  let output;
  try {
    output = exec(ghBin, ["api", endpoint], {
      encoding: "utf8",
      maxBuffer: 10 * 1024 * 1024,
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (error) {
    if (isNotFoundError(error)) {
      return null;
    }
    const detail = String(error && error.stderr ? error.stderr : "").trim();
    throw new Error(
      `Could not inspect GitHub Release ${tag}${detail ? `: ${detail}` : ""}`,
    );
  }

  try {
    return JSON.parse(output);
  } catch (error) {
    throw new Error(`GitHub Release ${tag} returned invalid JSON: ${error.message}`);
  }
}

function assertReleaseAssetsAbsent(release, tag) {
  if (release === null) {
    return;
  }
  if (!release || !Array.isArray(release.assets)) {
    throw new Error(`GitHub Release ${tag} did not provide an asset inventory`);
  }
  if (release.assets.length === 0) {
    return;
  }

  const names = release.assets.map((asset) => asset && asset.name).filter(Boolean);
  const inventory = names.length > 0 ? names.join(", ") : `${release.assets.length} unnamed asset(s)`;
  throw new Error(
    `Refusing to replace existing assets for ${tag}: ${inventory}. ` +
      "A normal Release workflow rerun must never delete or overwrite public bytes.",
  );
}

function main() {
  if (process.argv.length !== 4) {
    throw new Error(usage());
  }
  const repo = process.argv[2];
  const tag = process.argv[3];
  const release = fetchRelease(repo, tag);
  assertReleaseAssetsAbsent(release, tag);
  console.log(
    release === null
      ? `No existing GitHub Release assets found for ${tag}.`
      : `Existing GitHub Release ${tag} has no assets; first upload may proceed.`,
  );
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error(`Release immutability check failed: ${error.message}`);
    process.exit(1);
  }
}

module.exports = {
  assertReleaseAssetsAbsent,
  fetchRelease,
  isNotFoundError,
  validateTarget,
};
