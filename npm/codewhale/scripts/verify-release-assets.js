const https = require("https");
const http = require("http");
const {
  allReleaseAssetNames,
  BUNDLE_ASSET_NAMES,
  BUNDLE_CHECKSUM_MANIFEST,
  checksummedReleaseAssetNames,
  checksumManifestUrl,
  CNB_BINARY_ASSET_NAMES,
  CNB_RELEASE_ASSET_NAMES,
  redactUrl,
  releaseAssetUrl,
  usesCnbMirror,
  validateReleaseUrl,
} = require("./artifacts");

const pkg = require("../package.json");

function resolveBinaryVersion() {
  const configuredVersion =
    process.env.CODEWHALE_VERSION ||
    process.env.DEEPSEEK_TUI_VERSION ||
    process.env.DEEPSEEK_VERSION ||
    pkg.codewhaleBinaryVersion || pkg.deepseekBinaryVersion ||
    pkg.version;
  return String(configuredVersion).trim();
}

function resolveRepo() {
  return (
    process.env.CODEWHALE_GITHUB_REPO ||
    process.env.DEEPSEEK_TUI_GITHUB_REPO ||
    process.env.DEEPSEEK_GITHUB_REPO ||
    "codewhale-hq/CodeWhale"
  );
}

function hasReleaseBaseOverride() {
  return Boolean(
    process.env.CODEWHALE_RELEASE_BASE_URL ||
      process.env.DEEPSEEK_TUI_RELEASE_BASE_URL ||
      process.env.DEEPSEEK_RELEASE_BASE_URL ||
      process.env.CODEWHALE_USE_CNB_MIRROR,
  );
}

function packageVersionMatchesBinaryVersion(version) {
  return String(pkg.version).trim() === version;
}

function assertPackageVersionMatchesBinaryVersion(version) {
  if (packageVersionMatchesBinaryVersion(version)) {
    return;
  }
  if (process.env.CODEWHALE_ALLOW_NPM_BINARY_MISMATCH === "1") {
    console.log(
      `npm package version ${pkg.version} points at binary release ${version} (allowed packaging-only mismatch).`,
    );
    return;
  }
  throw new Error(
    `npm package version ${pkg.version} does not match codewhaleBinaryVersion ${version}. ` +
      "Set CODEWHALE_ALLOW_NPM_BINARY_MISMATCH=1 only for an intentional packaging-only npm release.",
  );
}

// Every request is bounded: `idleMs` covers connecting and any stall (it is
// the socket timeout, which starts before the socket connects), `totalMs` is a
// hard ceiling for one request including a body that keeps trickling, and a
// body larger than `maxBodyBytes` is cut off. Redirects are limited to ten, so
// a chain is bounded too. Tests shrink these through the exported object.
const limits = {
  idleMs: 30_000,
  totalMs: 120_000,
  maxBodyBytes: 8 * 1024 * 1024,
};

// Reject first, then tear the request down: an error passed to `destroy()` can
// surface on an emitter nobody listens to once a response has started.
function armTotalTimeout(req, url, reject) {
  const safeUrl = redactUrl(url);
  const fail = (error) => {
    reject(error);
    req.destroy();
  };
  const timer = setTimeout(
    () => fail(new Error(`Request exceeded ${limits.totalMs} ms: ${safeUrl}`)),
    limits.totalMs,
  );
  timer.unref();
  const clear = () => clearTimeout(timer);
  req.on("close", clear);
  req.on("error", clear);
  req.on("timeout", () =>
    fail(new Error(`Request timed out after ${limits.idleMs} ms without data: ${safeUrl}`)),
  );
  return { clear, fail };
}

function requestRelease(url, options = {}, redirects = 0) {
  const method = options.method || "GET";
  const readBody = Boolean(options.readBody);
  const maxRedirects = options.maxRedirects == null ? 10 : options.maxRedirects;
  const safeUrl = redactUrl(url);
  if (redirects > maxRedirects) {
    return Promise.reject(new Error(`Too many redirects while requesting ${safeUrl}`));
  }
  let parsedUrl;
  try {
    parsedUrl = validateReleaseUrl(url);
  } catch (err) {
    return Promise.reject(err);
  }
  const client = parsedUrl.protocol === "https:" ? https : http;
  const headers = {
    ...(options.headers || {}),
    "User-Agent": "codewhale-npm-release-check",
  };
  if (options.attachGitHubToken) {
    const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;
    if (token && parsedUrl.origin === "https://api.github.com") {
      headers.Authorization = `Bearer ${token}`;
    }
  }
  return new Promise((resolve, reject) => {
    const onResponse = (res) => {
      const status = res.statusCode || 0;
      const location = res.headers && res.headers.location;
      if (status >= 300 && status < 400 && location) {
        res.resume();
        clear();
        let next;
        try {
          next = new URL(location, url).toString();
        } catch (err) {
          reject(new Error(`Invalid redirect location from ${safeUrl}: ${err.message}`));
          return;
        }
        resolve(requestRelease(next, options, redirects + 1));
        return;
      }
      if (!readBody) {
        res.resume();
        clear();
        resolve({ status, url: safeUrl, body: "" });
        return;
      }
      const chunks = [];
      let size = 0;
      res.setEncoding("utf8");
      res.on("error", reject);
      res.on("data", (chunk) => {
        size += Buffer.byteLength(chunk);
        if (size > limits.maxBodyBytes) {
          guard.fail(new Error(`Response exceeds ${limits.maxBodyBytes} bytes: ${safeUrl}`));
          return;
        }
        chunks.push(chunk);
      });
      res.on("end", () => resolve({ status, url: safeUrl, body: chunks.join("") }));
    };
    const req =
      method === "GET"
        ? client.get(url, { headers, timeout: limits.idleMs }, onResponse)
        : client.request(url, { method, headers, timeout: limits.idleMs }, onResponse);
    const guard = armTotalTimeout(req, url, reject);
    const clear = guard.clear;
    req.on("error", reject);
    if (method !== "GET") {
      req.end();
    }
  });
}

async function requestStatus(url, method = "HEAD", redirects = 0) {
  const result = await requestRelease(url, { method, readBody: false }, redirects);
  return result.status;
}

async function verifyAsset(url, label) {
  let status = await requestStatus(url, "HEAD");
  if (status === 403 || status === 405) {
    status = await requestStatus(url, "GET");
  }
  if (status < 200 || status >= 400) {
    throw new Error(`${label} returned HTTP ${status} (${redactUrl(url)})`);
  }
}

async function downloadText(url, redirects = 0) {
  const result = await requestRelease(url, { method: "GET", readBody: true }, redirects);
  if (result.status !== 200) {
    throw new Error(`Request failed with status ${result.status}: ${result.url}`);
  }
  return result.body;
}

async function downloadJson(url, redirects = 0) {
  const result = await requestRelease(
    url,
    {
      method: "GET",
      readBody: true,
      attachGitHubToken: true,
      headers: {
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
      },
    },
    redirects,
  );
  let parsed;
  try {
    parsed = result.body ? JSON.parse(result.body) : {};
  } catch (error) {
    throw new Error(`Invalid JSON from ${result.url}: ${error.message}`);
  }
  if (result.status < 200 || result.status >= 300) {
    const message = parsed.message ? `: ${parsed.message}` : "";
    throw new Error(`GitHub API request failed with status ${result.status}${message} (${result.url})`);
  }
  return parsed;
}

function githubApiUrl(repo, path) {
  return `https://api.github.com/repos/${repo}${path}`;
}

async function githubApi(repo, path) {
  return downloadJson(githubApiUrl(repo, path));
}

async function resolveTagCommitSha(repo, tag) {
  const ref = await githubApi(repo, `/git/ref/tags/${encodeURIComponent(tag)}`);
  if (!ref.object || !ref.object.sha || !ref.object.type) {
    throw new Error(`GitHub tag ref ${tag} did not include an object SHA`);
  }
  if (ref.object.type === "commit") {
    return ref.object.sha;
  }
  if (ref.object.type !== "tag") {
    throw new Error(`GitHub tag ref ${tag} points at ${ref.object.type}, not a commit or annotated tag`);
  }
  const tagObject = await githubApi(repo, `/git/tags/${ref.object.sha}`);
  if (!tagObject.object || tagObject.object.type !== "commit" || !tagObject.object.sha) {
    throw new Error(`Annotated tag ${tag} did not peel to a commit SHA`);
  }
  return tagObject.object.sha;
}

async function findReleaseWorkflowRun(repo, tag, tagSha, api = githubApi) {
  const runs = await api(repo, "/actions/workflows/release.yml/runs?per_page=100");
  const candidates = (runs.workflow_runs || [])
    .filter((run) => run.head_sha === tagSha)
    .filter((run) => run.event === "push" || run.event === "workflow_dispatch")
    .sort((a, b) => String(b.updated_at).localeCompare(String(a.updated_at)));

  const orderedCandidates = [
    ...candidates.filter((run) => run.head_branch === tag),
    ...candidates.filter((run) => run.head_branch !== tag),
  ];
  for (const candidate of orderedCandidates) {
    const runId = candidate.database_id || candidate.id;
    if (!runId) {
      continue;
    }
    const jobs = await api(repo, `/actions/runs/${runId}/jobs?per_page=100`);
    const releaseJob = (jobs.jobs || []).find(
      (job) => job.name === "release" && job.conclusion === "success",
    );
    if (releaseJob) {
      // #5429: pin asset freshness to the successful release job's own
      // started_at, not the run-level run_started_at. A job-level rerun
      // (`gh run rerun --failed`) bumps run_started_at past the asset upload
      // timestamps even though this release job produced those assets.
      return { ...candidate, release_job_started_at: releaseJob.started_at || null };
    }
  }

  if (orderedCandidates.length === 0) {
    throw new Error(
      `No release.yml workflow run found for ${tag} at ${tagSha}. ` +
        "Rerun the Release workflow before publishing npm, or increase the verifier's last-100-runs search window.",
    );
  }
  throw new Error(
    `No successful asset-publishing job found in release.yml workflow runs for ${tag} at ${tagSha}. ` +
      "Repair the Release workflow before publishing npm.",
  );
}

function parseGitHubTime(value, label) {
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) {
    throw new Error(`GitHub ${label} timestamp is invalid: ${value}`);
  }
  return timestamp;
}

function assertReleaseAssetsFresh(release, expectedAssets, run) {
  const assetsByName = new Map((release.assets || []).map((asset) => [asset.name, asset]));
  const missing = expectedAssets.filter((asset) => !assetsByName.has(asset));
  if (missing.length > 0) {
    throw new Error(`GitHub Release is missing required release asset(s): ${missing.join(", ")}`);
  }

  // #5429: compare against the successful release job's started_at when the
  // run record carries it; fall back to the run-level timestamp only when a
  // job baseline is unavailable.
  const baseline = run.release_job_started_at || run.run_started_at || run.created_at;
  const baselineLabel = run.release_job_started_at ? "release job start" : "workflow run start";
  const freshnessBaseline = parseGitHubTime(baseline, baselineLabel);
  const stale = [];
  for (const expected of expectedAssets) {
    const asset = assetsByName.get(expected);
    if (asset.state && asset.state !== "uploaded") {
      stale.push(`${expected} has state ${asset.state}`);
      continue;
    }
    const updatedAt = parseGitHubTime(asset.updated_at || asset.created_at, `${expected} update`);
    if (updatedAt < freshnessBaseline) {
      stale.push(`${expected} updated at ${asset.updated_at || asset.created_at}`);
    }
  }

  if (stale.length > 0) {
    throw new Error(
      `GitHub Release asset set is stale for workflow run ${run.database_id || run.id}: ${stale.join("; ")}`,
    );
  }
}

async function verifyGitHubReleaseFreshness(repo, version, expectedAssets) {
  const tag = `v${version}`;
  const tagSha = await resolveTagCommitSha(repo, tag);
  const release = await githubApi(repo, `/releases/tags/${encodeURIComponent(tag)}`);
  const run = await findReleaseWorkflowRun(repo, tag, tagSha);
  assertReleaseAssetsFresh(release, expectedAssets, run);
  console.log(
    `GitHub release asset freshness OK: ${expectedAssets.length} release assets for ${tag} were produced by run ${run.database_id || run.id} at ${tagSha.slice(0, 12)}.`,
  );
}

function parseChecksumManifest(text) {
  const checksums = new Map();
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    const match = trimmed.match(/^([a-fA-F0-9]{64})\s+\*?(.+)$/);
    if (!match) {
      throw new Error(`Invalid checksum manifest line: ${trimmed}`);
    }
    checksums.set(match[2], match[1].toLowerCase());
  }
  return checksums;
}

function assertChecksumManifestIncludes(checksums, expectedAssets, label) {
  const missing = expectedAssets.filter((asset) => !checksums.has(asset));
  if (missing.length > 0) {
    throw new Error(`${label} is missing ${missing.join(", ")}`);
  }
}

const compiledHosts = require("./compiled-hosts");

async function run() {
  const version = resolveBinaryVersion();
  const repo = resolveRepo();
  const cnbMirror = usesCnbMirror();
  const checksums = parseChecksumManifest(await downloadText(checksumManifestUrl(version, repo)));
  let catalog;
  if (checksums.has(compiledHosts.HOST_CATALOG)) {
    const text = await downloadText(releaseAssetUrl(compiledHosts.HOST_CATALOG, version, repo));
    compiledHosts.verifyBytes(Buffer.from(text), checksums.get(compiledHosts.HOST_CATALOG), compiledHosts.HOST_CATALOG);
    catalog = compiledHosts.parseCatalog(text, version);
  }
  if (compiledHosts.requested() && !catalog) throw new Error("compiled host requested but this source has no qualified catalog");
  const assets = cnbMirror ? [...CNB_RELEASE_ASSET_NAMES, ...compiledHosts.assets(catalog)] : allReleaseAssetNames(catalog);

  assertPackageVersionMatchesBinaryVersion(version);

  console.log(`Verifying ${assets.length} release assets for ${repo}@v${version}...`);
  if (hasReleaseBaseOverride()) {
    console.log("Skipping GitHub workflow freshness check because a release asset mirror/base URL override is set.");
  } else {
    await verifyGitHubReleaseFreshness(repo, version, assets);
  }
  for (const asset of assets) {
    const url = releaseAssetUrl(asset, version, repo);
    await verifyAsset(url, asset);
    console.log(`  ok ${asset}`);
  }
  assertChecksumManifestIncludes(
    checksums,
    cnbMirror ? [...CNB_BINARY_ASSET_NAMES, ...compiledHosts.assets(catalog)] : checksummedReleaseAssetNames(catalog),
    "Canonical checksum manifest",
  );
  if (!cnbMirror) {
    const bundleChecksums = parseChecksumManifest(
      await downloadText(releaseAssetUrl(BUNDLE_CHECKSUM_MANIFEST, version, repo)),
    );
    assertChecksumManifestIncludes(
      bundleChecksums,
      BUNDLE_ASSET_NAMES,
      "Bundle checksum manifest",
    );
  }
  console.log("Release assets verified.");
}

if (require.main === module) {
  run().catch((error) => {
    console.error("Release asset verification failed:", error.message);
    process.exit(1);
  });
}

module.exports = {
  downloadJson,
  downloadText,
  limits,
  requestStatus,
  assertChecksumManifestIncludes,
  assertPackageVersionMatchesBinaryVersion,
  assertReleaseAssetsFresh,
  findReleaseWorkflowRun,
  hasReleaseBaseOverride,
  parseChecksumManifest,
};
