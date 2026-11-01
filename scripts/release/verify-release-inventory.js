#!/usr/bin/env node
// The GitHub Release asset set, checked as a whole.
//
// release.yml uploads into a DRAFT release, then runs this with
//   --draft --asset-dir artifacts --publish
// which requires the draft to carry exactly the authoritative inventory
// (allReleaseAssetNames), every asset fully uploaded with the local byte
// size and SHA-256 digest, and only then flips draft -> published. A rerun
// can reuse draft assets only when their bytes match the verified local set.
// The whole set becomes public
// at once; an upload that dies halfway leaves a draft nobody can install from,
// not a public partial release.
//
// release-republish.yml runs it with --manifest: an older tag may predate the
// current inventory, so there the release's own checksum manifest is the
// inventory, and the release must carry exactly the manifest's assets, each
// with the SHA-256 digest the manifest lists, plus the manifest itself before
// any channel is derived from it.
//
// Set GH_BIN to choose the GitHub CLI binary (tests use a fake).

const { execFileSync } = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const { allReleaseAssetNames, CHECKSUM_MANIFEST, CHECKSUM_SIGNATURE } = require("../../npm/codewhale/scripts/artifacts");
const compiledHosts = require("../../npm/codewhale/scripts/compiled-hosts");
const { validateTarget } = require("./ensure-release-assets-absent");

function usage() {
  return [
    "Usage:",
    "  node scripts/release/verify-release-inventory.js [--draft] [--asset-dir DIR] [--publish] OWNER/REPO vX.Y.Z",
    "  node scripts/release/verify-release-inventory.js --manifest OWNER/REPO vX.Y.Z",
  ].join("\n");
}

function ghRunner(ghBin = process.env.GH_BIN || "gh", exec = execFileSync) {
  return (args) => {
    try {
      return exec(ghBin, args, {
        encoding: "utf8",
        maxBuffer: 20 * 1024 * 1024,
        stdio: ["ignore", "pipe", "pipe"],
      });
    } catch (error) {
      const detail = String(error && error.stderr ? error.stderr : error && error.message).trim();
      throw new Error(`gh ${args.slice(0, 2).join(" ")} failed${detail ? `: ${detail}` : ""}`);
    }
  };
}

/** The draft (listed, since drafts are invisible on the tags endpoint) or the published release. */
function findRelease(repo, tag, { draft }, gh) {
  validateTarget(repo, tag);
  if (!draft) {
    const release = JSON.parse(gh(["api", `repos/${repo}/releases/tags/${encodeURIComponent(tag)}`]));
    if (!release || release.draft) throw new Error(`GitHub Release ${tag} is not published`);
    return release;
  }
  const pages = JSON.parse(gh(["api", `repos/${repo}/releases?per_page=100`, "--paginate", "--slurp"]));
  if (!Array.isArray(pages) || pages.some((page) => !Array.isArray(page))) {
    throw new Error("GitHub did not return a paginated release list");
  }
  const listed = pages.flat();
  const drafts = listed.filter((release) => release && release.draft === true && release.tag_name === tag);
  if (drafts.length !== 1) {
    throw new Error(
      `Expected exactly one draft release for ${tag}; found ${drafts.length}. ` +
        "A stale draft from an earlier failed run must be reviewed and deleted by a maintainer before rerunning.",
    );
  }
  return drafts[0];
}

/** Exact names, each fully uploaded, and matching local size and SHA-256. */
function assertInventory(release, tag, expectedNames, assetDir, manifestDigests = null) {
  if (!release || !Array.isArray(release.assets)) {
    throw new Error(`GitHub Release ${tag} did not provide an asset inventory`);
  }
  const byName = new Map();
  for (const asset of release.assets) {
    if (!asset || typeof asset.name !== "string" || byName.has(asset.name)) {
      throw new Error(`GitHub Release ${tag} has a malformed or duplicate asset entry`);
    }
    byName.set(asset.name, asset);
  }
  const expected = new Set(expectedNames);
  const missing = expectedNames.filter((name) => !byName.has(name));
  const unexpected = [...byName.keys()].filter((name) => !expected.has(name));
  if (missing.length > 0 || unexpected.length > 0) {
    throw new Error(
      `GitHub Release ${tag} does not carry the exact asset inventory` +
        `${missing.length > 0 ? `; missing: ${missing.join(", ")}` : ""}` +
        `${unexpected.length > 0 ? `; unexpected: ${unexpected.join(", ")}` : ""}`,
    );
  }
  for (const name of expectedNames) {
    const asset = byName.get(name);
    if (asset.state !== "uploaded" || !(asset.size > 0)) {
      throw new Error(`GitHub Release ${tag} asset ${name} is not fully uploaded (state ${asset.state}, ${asset.size} bytes)`);
    }
    if (manifestDigests && manifestDigests.has(name) && asset.digest !== manifestDigests.get(name)) {
      throw new Error(`GitHub Release ${tag} asset ${name} has a missing or mismatched SHA-256 digest against ${CHECKSUM_MANIFEST}; refusing a stale manifest`);
    }
    if (assetDir) {
      const local = fs.statSync(path.join(assetDir, name)).size;
      if (asset.size !== local) {
        throw new Error(`GitHub Release ${tag} asset ${name} has ${asset.size} bytes; the verified local asset has ${local}`);
      }
      const digest = `sha256:${crypto.hash("sha256", fs.readFileSync(path.join(assetDir, name)))}`;
      if (asset.digest !== digest) {
        throw new Error(`GitHub Release ${tag} asset ${name} has a missing or mismatched SHA-256 digest; refusing stale draft bytes`);
      }
    }
  }
  return expectedNames.length;
}

/** The SHA-256 digest the release's own checksum manifest lists for each asset, by name. */
function manifestInventory(repo, tag, gh) {
  const text = gh(["release", "download", tag, "--repo", repo, "--pattern", CHECKSUM_MANIFEST, "--output", "-"]);
  const digests = new Map();
  for (const line of String(text).split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const match = trimmed.match(/^([a-fA-F0-9]{64})\s+\*?(.+)$/);
    if (!match) throw new Error(`${CHECKSUM_MANIFEST} contains an invalid row: ${trimmed}`);
    if (digests.has(match[2])) throw new Error(`${CHECKSUM_MANIFEST} lists ${match[2]} twice`);
    digests.set(match[2], `sha256:${match[1].toLowerCase()}`);
  }
  if (digests.size === 0) throw new Error(`${CHECKSUM_MANIFEST} for ${tag} lists no assets`);
  return digests;
}

function run(argv, gh = ghRunner(), log = console.log) {
  const flags = { draft: false, publish: false, manifest: false, assetDir: null };
  const positional = [];
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--draft") flags.draft = true;
    else if (arg === "--publish") flags.publish = true;
    else if (arg === "--manifest") flags.manifest = true;
    else if (arg === "--asset-dir") flags.assetDir = argv[++i];
    else positional.push(arg);
  }
  if (positional.length !== 2 || (flags.publish && (!flags.draft || !flags.assetDir)) || (flags.manifest && (flags.draft || flags.assetDir))) {
    throw new Error(usage());
  }
  const [repo, tag] = positional;
  const release = findRelease(repo, tag, { draft: flags.draft }, gh);
  let catalog;
  if (!flags.manifest && release.assets.some((asset) => asset.name === compiledHosts.HOST_CATALOG)) {
    if (!flags.assetDir) throw new Error("compiled-host release verification requires --asset-dir; verify exact local qualification/catalog bytes before publication");
    catalog = compiledHosts.parseCatalog(fs.readFileSync(path.join(flags.assetDir, compiledHosts.HOST_CATALOG), "utf8"), tag.replace(/^v/, ""));
    compiledHosts.verifyDirectory(flags.assetDir, catalog);
  }
  const manifestDigests = flags.manifest ? manifestInventory(repo, tag, gh) : null;
  const expected = manifestDigests ? [...manifestDigests.keys(), CHECKSUM_MANIFEST] : allReleaseAssetNames(catalog);
  if (release.assets.some((asset) => asset.name === CHECKSUM_SIGNATURE)) expected.push(CHECKSUM_SIGNATURE);
  const count = assertInventory(release, tag, expected, flags.assetDir, manifestDigests);
  log(`Verified ${count} assets on the ${flags.draft ? "draft" : "published"} release ${tag}`);
  if (!flags.publish) return;
  gh(["api", "-X", "PATCH", `repos/${repo}/releases/${release.id}`, "-F", "draft=false"]);
  const published = findRelease(repo, tag, { draft: false }, gh);
  if (published.id !== release.id) {
    throw new Error(`Published release for ${tag} is ${published.id}, not the verified draft ${release.id}`);
  }
  assertInventory(published, tag, expected, flags.assetDir);
  log(`Published ${tag} with its ${count} verified assets`);
}

if (require.main === module) {
  try {
    run(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}

module.exports = { assertInventory, findRelease, manifestInventory, run };
