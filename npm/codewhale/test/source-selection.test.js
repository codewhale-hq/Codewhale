const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const {
  CHECKSUM_MANIFEST,
  cnbReleaseBaseUrl,
  explicitReleaseBase,
  firstPartyReleaseSources,
  githubReleaseBaseUrl,
  hasExplicitReleaseBase,
  shouldRaceFirstPartyMirrors,
} = require("../scripts/artifacts");
const { run, _internal } = require("../scripts/install");

const VERSION = "0.9.10";
const REPO = "codewhale-hq/CodeWhale";
const CODEWHALE_ASSET = "codewhale-linux-x64";
const CODEW_ASSET = "codew-linux-x64";
const REQUIRED_ASSETS = [CODEWHALE_ASSET, CODEW_ASSET];

function sha256(content) {
  return crypto.createHash("sha256").update(content).digest("hex");
}

function hasExactHostname(value, expectedHostname) {
  return new URL(value).hostname === expectedHostname;
}

function manifestFor(files) {
  return Object.entries(files)
    .map(([name, body]) => `${sha256(body)}  ${name}`)
    .join("\n");
}

function abortError() {
  const err = new Error("The operation was aborted");
  err.name = "AbortError";
  err.code = "ABORT_ERR";
  err.nonRetryable = true;
  return err;
}

function hangUntilAbort(signal) {
  return new Promise((_, reject) => {
    const fail = () => reject(abortError());
    if (!signal) {
      return;
    }
    if (signal.aborted) {
      fail();
      return;
    }
    signal.addEventListener("abort", fail, { once: true });
  });
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

async function makeTempDir(t) {
  const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "codewhale-source-"));
  t.after(() => fs.promises.rm(dir, { force: true, recursive: true }));
  return dir;
}

function linuxSelectOptions(overrides = {}) {
  return {
    version: VERSION,
    repo: REPO,
    requiredAssets: REQUIRED_ASSETS,
    context: "runtime",
    env: {},
    platform: "linux",
    arch: "x64",
    ...overrides,
  };
}

async function withoutForcedDownload(callback) {
  const previousCodewhale = process.env.CODEWHALE_FORCE_DOWNLOAD;
  const previousTui = process.env.DEEPSEEK_TUI_FORCE_DOWNLOAD;
  const previousLegacy = process.env.DEEPSEEK_FORCE_DOWNLOAD;
  delete process.env.CODEWHALE_FORCE_DOWNLOAD;
  delete process.env.DEEPSEEK_TUI_FORCE_DOWNLOAD;
  delete process.env.DEEPSEEK_FORCE_DOWNLOAD;
  try {
    return await callback();
  } finally {
    if (previousCodewhale === undefined) {
      delete process.env.CODEWHALE_FORCE_DOWNLOAD;
    } else {
      process.env.CODEWHALE_FORCE_DOWNLOAD = previousCodewhale;
    }
    if (previousTui === undefined) {
      delete process.env.DEEPSEEK_TUI_FORCE_DOWNLOAD;
    } else {
      process.env.DEEPSEEK_TUI_FORCE_DOWNLOAD = previousTui;
    }
    if (previousLegacy === undefined) {
      delete process.env.DEEPSEEK_FORCE_DOWNLOAD;
    } else {
      process.env.DEEPSEEK_FORCE_DOWNLOAD = previousLegacy;
    }
  }
}

test("Linux x64 races first-party sources unless an explicit override is set", () => {
  assert.equal(shouldRaceFirstPartyMirrors({}, "linux", "x64"), true);
  // The CNB mirror publishes Linux assets only, and native HarmonyOS resolves to
  // its own asset family; it must not be handed a Linux x64 URL.
  assert.equal(shouldRaceFirstPartyMirrors({}, "openharmony", "x64"), false);
  assert.equal(shouldRaceFirstPartyMirrors({}, "linux", "arm64"), false);
  assert.equal(shouldRaceFirstPartyMirrors({}, "darwin", "arm64"), false);
  assert.equal(
    shouldRaceFirstPartyMirrors({ CODEWHALE_USE_CNB_MIRROR: "1" }, "linux", "x64"),
    false,
  );
  assert.equal(
    shouldRaceFirstPartyMirrors({ CODEWHALE_USE_CNB_MIRROR: "0" }, "linux", "x64"),
    true,
  );
  assert.equal(
    shouldRaceFirstPartyMirrors(
      { CODEWHALE_RELEASE_BASE_URL: "https://mirror.example/v0.9.10/" },
      "linux",
      "x64",
    ),
    false,
  );
  assert.equal(
    hasExplicitReleaseBase({ DEEPSEEK_TUI_RELEASE_BASE_URL: "https://legacy.example/" }),
    true,
  );
  assert.equal(
    hasExplicitReleaseBase({
      CODEWHALE_RELEASE_BASE_URL: "   ",
      DEEPSEEK_TUI_RELEASE_BASE_URL: "https://legacy.example/",
    }),
    true,
  );
  assert.equal(
    explicitReleaseBase({
      CODEWHALE_RELEASE_BASE_URL: "   ",
      DEEPSEEK_TUI_RELEASE_BASE_URL: "https://legacy.example/releases",
    }),
    "https://legacy.example/releases/",
  );
});

test("first-party source URLs stay pinned to the exact package version", () => {
  assert.deepEqual(firstPartyReleaseSources(VERSION, REPO), [
    {
      id: "github",
      label: "GitHub Releases",
      baseUrl: githubReleaseBaseUrl(VERSION, REPO),
    },
    {
      id: "cnb",
      label: "CNB first-party mirror",
      baseUrl: cnbReleaseBaseUrl(VERSION),
    },
  ]);
  assert.equal(
    githubReleaseBaseUrl(VERSION, REPO),
    `https://github.com/${REPO}/releases/download/v${VERSION}/`,
  );
  assert.equal(
    cnbReleaseBaseUrl(VERSION),
    `https://cnb.cool/codewhale.net/codewhale/-/releases/download/v${VERSION}/`,
  );
});

test("CNB wins when its checksum manifest validates first", async () => {
  const githubBody = Buffer.from("github-codewhale");
  const cnbBody = Buffer.from("cnb-codewhale");
  const fetched = [];
  let githubSignal;

  const source = await _internal.selectReleaseSource(
    linuxSelectOptions({
      fetchText: async (url, opts) => {
        fetched.push(url);
        if (hasExactHostname(url, "github.com")) {
          githubSignal = opts && opts.signal;
          return hangUntilAbort(opts && opts.signal);
        }
        return manifestFor({
          [CODEWHALE_ASSET]: cnbBody,
          [CODEW_ASSET]: Buffer.from("cnb-codew"),
        });
      },
    }),
  );

  assert.equal(source.id, "cnb");
  assert.match(source.label, /CNB first-party mirror/);
  assert.equal(source.baseUrl, cnbReleaseBaseUrl(VERSION));
  assert.equal(source.checksums.get(CODEWHALE_ASSET), sha256(cnbBody));
  assert.notEqual(source.checksums.get(CODEWHALE_ASSET), sha256(githubBody));
  assert.equal(githubSignal && githubSignal.aborted, true);
  assert.ok(fetched.some((url) => hasExactHostname(url, "cnb.cool")));
  assert.ok(fetched.some((url) => hasExactHostname(url, "github.com")));
  assert.ok(fetched.every((url) => url.endsWith(CHECKSUM_MANIFEST)));
});

test("GitHub wins when its checksum manifest validates first", async () => {
  const githubBody = Buffer.from("github-codewhale");
  let cnbSignal;

  const source = await _internal.selectReleaseSource(
    linuxSelectOptions({
      fetchText: async (url, opts) => {
        if (hasExactHostname(url, "cnb.cool")) {
          cnbSignal = opts && opts.signal;
          return hangUntilAbort(opts && opts.signal);
        }
        return manifestFor({
          [CODEWHALE_ASSET]: githubBody,
          [CODEW_ASSET]: Buffer.from("github-codew"),
        });
      },
    }),
  );

  assert.equal(source.id, "github");
  assert.equal(source.baseUrl, githubReleaseBaseUrl(VERSION, REPO));
  assert.equal(source.checksums.get(CODEWHALE_ASSET), sha256(githubBody));
  assert.equal(cnbSignal && cnbSignal.aborted, true);
});

test("one unavailable first-party source does not block the valid source", async () => {
  const githubBody = Buffer.from("github-only");
  const fetched = [];

  const source = await _internal.selectReleaseSource(
    linuxSelectOptions({
      fetchText: async (url) => {
        fetched.push(url);
        if (hasExactHostname(url, "cnb.cool")) {
          const err = new Error("Request failed with status 404: " + url);
          err.name = "HttpStatusError";
          err.status = 404;
          err.nonRetryable = true;
          throw err;
        }
        return manifestFor({
          [CODEWHALE_ASSET]: githubBody,
          [CODEW_ASSET]: Buffer.from("github-codew"),
        });
      },
    }),
  );

  assert.equal(source.id, "github");
  assert.equal(source.checksums.get(CODEWHALE_ASSET), sha256(githubBody));
  assert.equal(fetched.length, 2);
});

test("both invalid manifests fail closed", async () => {
  await assert.rejects(
    () =>
      _internal.selectReleaseSource(
        linuxSelectOptions({
          fetchText: async () => "not-a-checksum-manifest",
        }),
      ),
    (err) => {
      assert.match(String(err.message), /No usable first-party release source/);
      assert.match(String(err.message), /GitHub Releases/);
      assert.match(String(err.message), /CNB first-party mirror/);
      assert.equal(err.nonRetryable, true);
      return true;
    },
  );
});

test("aggregate source failure preserves retryable optional-install behavior", async () => {
  let failure;
  try {
    await _internal.selectReleaseSource(
      linuxSelectOptions({
        fetchText: async (url) => {
          const error = new Error(`getaddrinfo ENOTFOUND ${new URL(url).hostname}`);
          error.code = "ENOTFOUND";
          throw error;
        },
      }),
    );
  } catch (error) {
    failure = error;
  }

  assert.ok(failure);
  assert.equal(failure.retryable, true);
  assert.equal(failure.nonRetryable, undefined);
  assert.equal(
    _internal.shouldIgnoreInstallFailure(
      "install",
      failure,
      ["--optional"],
      {},
    ),
    true,
  );
});

test("explicit release base and CNB override take precedence over the race", async () => {
  const overrideBase = "https://mirror.example/v0.9.10/";
  const overrideBody = Buffer.from("override-binary");
  const fetched = [];

  const overrideSource = await _internal.selectReleaseSource(
    linuxSelectOptions({
      env: {
        CODEWHALE_RELEASE_BASE_URL: overrideBase,
        CODEWHALE_USE_CNB_MIRROR: "1",
      },
      fetchText: async (url) => {
        fetched.push(url);
        if (!url.startsWith(overrideBase)) {
          throw new Error(`unexpected fetch ${url}`);
        }
        return manifestFor({
          [CODEWHALE_ASSET]: overrideBody,
          [CODEW_ASSET]: Buffer.from("override-codew"),
        });
      },
    }),
  );

  assert.equal(overrideSource.id, "override");
  assert.equal(overrideSource.baseUrl, overrideBase);
  assert.deepEqual(fetched, [`${overrideBase}${CHECKSUM_MANIFEST}`]);

  fetched.length = 0;
  const cnbSource = await _internal.selectReleaseSource(
    linuxSelectOptions({
      env: { CODEWHALE_USE_CNB_MIRROR: "1" },
      fetchText: async (url) => {
        fetched.push(url);
        if (!url.startsWith(cnbReleaseBaseUrl(VERSION))) {
          throw new Error(`unexpected fetch ${url}`);
        }
        return manifestFor({
          [CODEWHALE_ASSET]: Buffer.from("cnb-forced"),
          [CODEW_ASSET]: Buffer.from("cnb-forced-codew"),
        });
      },
    }),
  );

  assert.equal(cnbSource.id, "cnb");
  assert.deepEqual(fetched, [`${cnbReleaseBaseUrl(VERSION)}${CHECKSUM_MANIFEST}`]);
});

test("locked source downloads each required binary once from the winner", async (t) => {
  const dir = await makeTempDir(t);
  const cnbWhale = Buffer.from("cnb-codewhale-bytes");
  const cnbCodew = Buffer.from("cnb-codew-bytes");
  const githubWhale = Buffer.from("github-codewhale-bytes");
  const githubCodew = Buffer.from("github-codew-bytes");
  const binaryDownloads = [];
  const manifestFetches = [];

  const source = await _internal.selectReleaseSource(
    linuxSelectOptions({
      fetchText: async (url, opts) => {
        manifestFetches.push(url);
        if (hasExactHostname(url, "github.com")) {
          return hangUntilAbort(opts && opts.signal);
        }
        return manifestFor({
          [CODEWHALE_ASSET]: cnbWhale,
          [CODEW_ASSET]: cnbCodew,
        });
      },
    }),
  );

  assert.equal(source.id, "cnb");

  const fakeDownload = async (url, destination) => {
    binaryDownloads.push(url);
    const body = url.endsWith(CODEW_ASSET) ? cnbCodew : cnbWhale;
    await fs.promises.mkdir(path.dirname(destination), { recursive: true });
    await fs.promises.writeFile(destination, body);
  };

  await withoutForcedDownload(async () => {
    await _internal.ensureBinary(
      path.join(dir, "codewhale"),
      CODEWHALE_ASSET,
      VERSION,
      REPO,
      async () => source.checksums,
      {
        baseUrl: source.baseUrl,
        sourceId: source.id,
        sourceLabel: source.label,
        download: fakeDownload,
      },
    );
    await _internal.ensureBinary(
      path.join(dir, "codew"),
      CODEW_ASSET,
      VERSION,
      REPO,
      async () => source.checksums,
      {
        baseUrl: source.baseUrl,
        sourceId: source.id,
        sourceLabel: source.label,
        download: fakeDownload,
      },
    );
  });

  assert.deepEqual(binaryDownloads, [
    `${cnbReleaseBaseUrl(VERSION)}${CODEWHALE_ASSET}`,
    `${cnbReleaseBaseUrl(VERSION)}${CODEW_ASSET}`,
  ]);
  assert.ok(manifestFetches.every((url) => url.endsWith(CHECKSUM_MANIFEST)));
  assert.ok(!binaryDownloads.some((url) => hasExactHostname(url, "github.com")));
  assert.equal(await fs.promises.readFile(path.join(dir, "codewhale"), "utf8"), cnbWhale.toString());
  assert.equal(
    await fs.promises.readFile(path.join(dir, "codewhale.source"), "utf8"),
    [
      `source=${source.id}`,
      `label=${source.label}`,
      `base=${source.baseUrl}`,
      `version=${VERSION}`,
      "",
    ].join("\n"),
  );
});

test("run locks both binaries to the first valid manifest source", async (t) => {
  const dir = await makeTempDir(t);
  const cnbWhale = Buffer.from("run-cnb-codewhale");
  const cnbCodew = Buffer.from("run-cnb-codew");
  const bothStarted = deferred();
  const releaseCnbManifest = deferred();
  const binaryDownloads = [];
  let manifestStarts = 0;
  let githubSignal;

  const sources = [
    {
      id: "github",
      label: "GitHub Releases",
      baseUrl: "https://github.invalid/releases/download/v0.9.10/",
    },
    {
      id: "cnb",
      label: "CNB first-party mirror",
      baseUrl: "https://cnb.invalid/releases/download/v0.9.10/",
    },
  ];
  const paths = {
    codewhale: {
      asset: CODEWHALE_ASSET,
      target: path.join(dir, "codewhale"),
    },
    codew: {
      asset: CODEW_ASSET,
      target: path.join(dir, "codew"),
    },
  };

  const install = run({
    context: "runtime",
    env: {
      CODEWHALE_FORCE_DOWNLOAD: "1",
      CODEWHALE_QUIET_INSTALL: "1",
      CODEWHALE_VERSION: VERSION,
    },
    platform: "linux",
    arch: "x64",
    paths,
    releaseDir: dir,
    sources,
    fetchText: (url, options) => {
      manifestStarts += 1;
      if (manifestStarts === sources.length) {
        bothStarted.resolve();
      }
      if (url.startsWith(sources[0].baseUrl)) {
        githubSignal = options.signal;
        return hangUntilAbort(options.signal);
      }
      return releaseCnbManifest.promise;
    },
    download: async (url, destination) => {
      binaryDownloads.push(url);
      const body = url.endsWith(CODEW_ASSET) ? cnbCodew : cnbWhale;
      await fs.promises.writeFile(destination, body);
    },
  });

  await bothStarted.promise;
  releaseCnbManifest.resolve(
    manifestFor({
      [CODEWHALE_ASSET]: cnbWhale,
      [CODEW_ASSET]: cnbCodew,
    }),
  );
  await install;

  assert.equal(githubSignal.aborted, true);
  assert.deepEqual(binaryDownloads.sort(), [
    `${sources[1].baseUrl}${CODEW_ASSET}`,
    `${sources[1].baseUrl}${CODEWHALE_ASSET}`,
  ]);
  assert.equal(
    await fs.promises.readFile(`${paths.codewhale.target}.source`, "utf8"),
    [
      "source=cnb",
      "label=CNB first-party mirror",
      `base=${sources[1].baseUrl}`,
      `version=${VERSION}`,
      "",
    ].join("\n"),
  );
  assert.equal(
    await fs.promises.readFile(`${paths.codew.target}.source`, "utf8"),
    await fs.promises.readFile(`${paths.codewhale.target}.source`, "utf8"),
  );
});

test("run fails before binary download when neither manifest validates", async (t) => {
  const dir = await makeTempDir(t);
  const binaryDownloads = [];

  await assert.rejects(
    () =>
      run({
        context: "runtime",
        env: {
          CODEWHALE_FORCE_DOWNLOAD: "1",
          CODEWHALE_QUIET_INSTALL: "1",
          CODEWHALE_VERSION: VERSION,
        },
        platform: "linux",
        arch: "x64",
        releaseDir: dir,
        paths: {
          codewhale: {
            asset: CODEWHALE_ASSET,
            target: path.join(dir, "codewhale"),
          },
          codew: {
            asset: CODEW_ASSET,
            target: path.join(dir, "codew"),
          },
        },
        fetchText: async () => "invalid manifest",
        download: async (url) => {
          binaryDownloads.push(url);
        },
      }),
    /No usable first-party release source/,
  );

  assert.deepEqual(binaryDownloads, []);
  assert.equal(
    await fs.promises.access(path.join(dir, "codewhale")).then(() => true, () => false),
    false,
  );
  assert.equal(
    await fs.promises.access(path.join(dir, "codew")).then(() => true, () => false),
    false,
  );
});

test("run sends manifest and binaries only to an explicit release base", async (t) => {
  const dir = await makeTempDir(t);
  const baseUrl = "https://mirror.invalid/releases/download/v0.9.10/";
  const codewhaleBody = Buffer.from("override-codewhale");
  const codewBody = Buffer.from("override-codew");
  const manifestFetches = [];
  const binaryDownloads = [];

  await run({
    context: "runtime",
    env: {
      CODEWHALE_FORCE_DOWNLOAD: "1",
      CODEWHALE_QUIET_INSTALL: "1",
      CODEWHALE_RELEASE_BASE_URL: baseUrl,
      CODEWHALE_USE_CNB_MIRROR: "1",
      CODEWHALE_VERSION: VERSION,
    },
    platform: "linux",
    arch: "x64",
    releaseDir: dir,
    paths: {
      codewhale: {
        asset: CODEWHALE_ASSET,
        target: path.join(dir, "codewhale"),
      },
      codew: {
        asset: CODEW_ASSET,
        target: path.join(dir, "codew"),
      },
    },
    fetchText: async (url) => {
      manifestFetches.push(url);
      return manifestFor({
        [CODEWHALE_ASSET]: codewhaleBody,
        [CODEW_ASSET]: codewBody,
      });
    },
    download: async (url, destination) => {
      binaryDownloads.push(url);
      await fs.promises.writeFile(
        destination,
        url.endsWith(CODEW_ASSET) ? codewBody : codewhaleBody,
      );
    },
  });

  assert.deepEqual(manifestFetches, [`${baseUrl}${CHECKSUM_MANIFEST}`]);
  assert.deepEqual(binaryDownloads.sort(), [
    `${baseUrl}${CODEW_ASSET}`,
    `${baseUrl}${CODEWHALE_ASSET}`,
  ]);
});

test("checksum mismatch against the locked source fails closed", async (t) => {
  const dir = await makeTempDir(t);
  const expected = Buffer.from("expected-cnb-bytes");
  const actual = Buffer.from("tampered-cnb-bytes");
  const downloads = [];
  const source = {
    id: "cnb",
    label: "CNB first-party mirror",
    baseUrl: cnbReleaseBaseUrl(VERSION),
    checksums: new Map([[CODEWHALE_ASSET, sha256(expected)]]),
  };

  await assert.rejects(
    () =>
      _internal.ensureBinary(
        path.join(dir, "codewhale"),
        CODEWHALE_ASSET,
        VERSION,
        REPO,
        async () => source.checksums,
        {
          baseUrl: source.baseUrl,
          sourceId: source.id,
          sourceLabel: source.label,
          download: async (url, destination) => {
            downloads.push(url);
            await fs.promises.mkdir(path.dirname(destination), { recursive: true });
            await fs.promises.writeFile(destination, actual);
          },
        },
      ),
    /Checksum mismatch for codewhale-linux-x64 from CNB first-party mirror/,
  );
  assert.deepEqual(downloads, [`${cnbReleaseBaseUrl(VERSION)}${CODEWHALE_ASSET}`]);
  assert.equal(await fs.promises.access(path.join(dir, "codewhale")).then(() => true, () => false), false);
});

test("source selection is visible in progress output", async () => {
  const previousWrite = process.stderr.write;
  const previousQuiet = process.env.DEEPSEEK_TUI_QUIET_INSTALL;
  let stderr = "";
  process.stderr.write = (chunk) => {
    stderr += String(chunk);
    return true;
  };
  delete process.env.DEEPSEEK_TUI_QUIET_INSTALL;

  try {
    await _internal.selectReleaseSource(
      linuxSelectOptions({
        fetchText: async (url, opts) => {
          if (hasExactHostname(url, "github.com")) {
            return hangUntilAbort(opts && opts.signal);
          }
          return manifestFor({
            [CODEWHALE_ASSET]: Buffer.from("cnb"),
            [CODEW_ASSET]: Buffer.from("cnb-codew"),
          });
        },
      }),
    );
    assert.match(stderr, /probing GitHub Releases and CNB first-party mirror/);
    assert.match(stderr, /selected CNB first-party mirror/);
  } finally {
    process.stderr.write = previousWrite;
    if (previousQuiet === undefined) {
      delete process.env.DEEPSEEK_TUI_QUIET_INSTALL;
    } else {
      process.env.DEEPSEEK_TUI_QUIET_INSTALL = previousQuiet;
    }
  }
});

test("aborting a losing manifest probe closes a body already in progress", async (t) => {
  const responseStarted = deferred();
  const responseClosed = deferred();
  const server = http.createServer((_req, res) => {
    res.on("close", () => responseClosed.resolve());
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.write(`${"a".repeat(64)}  ${CODEWHALE_ASSET}\n`);
    responseStarted.resolve();
  });
  await new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", resolve);
    server.once("error", reject);
  });
  t.after(
    () =>
      new Promise((resolve) => {
        server.close(() => resolve());
      }),
  );

  const address = server.address();
  const controller = new AbortController();
  const fetching = _internal.downloadText(
    `http://127.0.0.1:${address.port}/${CHECKSUM_MANIFEST}`,
    {
      signal: controller.signal,
      stallMs: 10_000,
      totalTimeoutMs: 10_000,
    },
  );
  await responseStarted.promise;
  controller.abort();

  await assert.rejects(fetching, (error) => {
    assert.equal(error.name, "AbortError");
    assert.equal(error.code, "ABORT_ERR");
    return true;
  });
  await responseClosed.promise;
});

test("HTTP fixture never starts the losing source's full binary download", async (t) => {
  const dir = await makeTempDir(t);
  const cnbWhale = Buffer.from("fixture-cnb-whale");
  const cnbCodew = Buffer.from("fixture-cnb-codew");
  const githubWhale = Buffer.from("fixture-github-whale");
  const githubCodew = Buffer.from("fixture-github-codew");
  const hits = { githubManifest: 0, githubBinary: 0, cnbManifest: 0, cnbBinary: 0 };

  const github = await listenFixture({
    manifest: manifestFor({
      [CODEWHALE_ASSET]: githubWhale,
      [CODEW_ASSET]: githubCodew,
    }),
    files: {
      [CODEWHALE_ASSET]: githubWhale,
      [CODEW_ASSET]: githubCodew,
    },
    onManifest: () => {
      hits.githubManifest += 1;
    },
    onBinary: () => {
      hits.githubBinary += 1;
    },
  });
  const cnb = await listenFixture({
    manifest: manifestFor({
      [CODEWHALE_ASSET]: cnbWhale,
      [CODEW_ASSET]: cnbCodew,
    }),
    files: {
      [CODEWHALE_ASSET]: cnbWhale,
      [CODEW_ASSET]: cnbCodew,
    },
    onManifest: () => {
      hits.cnbManifest += 1;
    },
    onBinary: () => {
      hits.cnbBinary += 1;
    },
  });
  t.after(() => github.close());
  t.after(() => cnb.close());

  const source = await _internal.selectReleaseSource(
    linuxSelectOptions({
      sources: [
        {
          id: "github",
          label: "GitHub Releases",
          baseUrl: github.baseUrl,
        },
        {
          id: "cnb",
          label: "CNB first-party mirror",
          baseUrl: cnb.baseUrl,
        },
      ],
      fetchText: (url, opts) =>
        url.startsWith(github.baseUrl)
          ? hangUntilAbort(opts && opts.signal)
          : _internal.downloadText(url, opts),
    }),
  );

  assert.equal(source.id, "cnb");
  await withoutForcedDownload(() =>
    _internal.ensureBinary(
      path.join(dir, "codewhale"),
      CODEWHALE_ASSET,
      VERSION,
      REPO,
      async () => source.checksums,
      { baseUrl: source.baseUrl, sourceId: source.id, sourceLabel: source.label },
    ),
  );

  assert.equal(hits.githubBinary, 0);
  assert.equal(hits.cnbBinary, 1);
  assert.equal(hits.cnbManifest, 1);
  assert.equal(await fs.promises.readFile(path.join(dir, "codewhale"), "utf8"), cnbWhale.toString());
});

function listenFixture(spec) {
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const urlPath = String(req.url || "").split("?")[0];
      const name = path.posix.basename(urlPath);
      if (name === CHECKSUM_MANIFEST) {
        if (spec.onManifest) spec.onManifest();
        res.writeHead(200, { "Content-Type": "text/plain" });
        res.end(spec.manifest);
        return;
      }
      if (spec.files[name]) {
        if (spec.onBinary) spec.onBinary();
        res.writeHead(200, { "Content-Type": "application/octet-stream" });
        res.end(spec.files[name]);
        return;
      }
      res.writeHead(404);
      res.end("missing");
    });
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolve({
        baseUrl: `http://127.0.0.1:${address.port}/releases/download/v${VERSION}/`,
        close: () =>
          new Promise((done) => {
            server.close(() => done());
          }),
      });
    });
    server.once("error", reject);
  });
}
