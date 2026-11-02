function assertSupportedNode() {
  const version = process.versions && process.versions.node ? process.versions.node : "unknown";
  const major = Number.parseInt(String(version).split(".")[0], 10);
  if (Number.isNaN(major) || major < 18) {
    process.stderr.write(
      "codewhale: Node.js 18 or newer is required for npm installation. " +
      `Current Node.js version is ${version}. ` +
      "Please upgrade Node.js and rerun `npm install -g codewhale`.\n",
    );
    process.exit(1);
  }
}

assertSupportedNode();

const fs = require("fs");
const https = require("https");
const http = require("http");
const net = require("net");
const tls = require("tls");
const crypto = require("crypto");
const { URL } = require("url");
const { mkdir, chmod, stat, rename, readFile, unlink, writeFile } = fs.promises;
const { createWriteStream } = fs;
const path = require("path");
const os = require("os");

const {
  CHECKSUM_MANIFEST,
  assertCnbMirrorSupportedPlatform,
  checksumManifestUrl,
  cnbReleaseBaseUrl,
  detectBinaryNames,
  explicitReleaseBase,
  firstPartyReleaseSources,
  githubReleaseBaseUrl,
  openHarmonyDirections,
  releaseAssetUrl,
  releaseAssetUrlFromBase,
  releaseBinaryDirectory,
  shouldRaceFirstPartyMirrors,
  usesCnbMirror,
} = require("./artifacts");
const { preflightGlibc, detectHostGlibc, detectBinaryRequiredGlibc, _internal: glibc } = require("./preflight-glibc");
const compiledHosts = require("./compiled-hosts");
const pkg = require("../package.json");

const DEFAULT_TIMEOUT_MS = 300_000; // 5 minutes per attempt
const DEFAULT_STALL_MS = 30_000; // abort if no bytes for 30s
const OPTIONAL_TIMEOUT_MS = 15_000; // fail fast during optional npm postinstall
const OPTIONAL_STALL_MS = 5_000; // avoid long hangs when install can recover on first run
const MANIFEST_TIMEOUT_MS = 15_000; // small checksum probes must not wait on a binary budget
const MANIFEST_STALL_MS = 5_000;
const MAX_ATTEMPTS = 5;
const OPTIONAL_MAX_ATTEMPTS = 1; // runtime keeps the full retry budget on first launch
const BASE_BACKOFF_MS = 1_000;

const RETRYABLE_NET_CODES = new Set([
  "ECONNRESET",
  "ECONNREFUSED",
  "EDOWNLOADTIMEOUT",
  "ETIMEDOUT",
  "EAI_AGAIN",
  "ENOTFOUND",
  "ENETUNREACH",
  "EHOSTUNREACH",
  "EPIPE",
  "ECONNABORTED",
]);

class NonRetryableError extends Error {
  constructor(message) {
    super(message);
    this.name = "NonRetryableError";
    this.nonRetryable = true;
  }
}

class HttpStatusError extends Error {
  constructor(status, url) {
    super(`Request failed with status ${status}: ${url}`);
    this.name = "HttpStatusError";
    this.status = status;
  }
}

class DownloadTimeoutError extends Error {
  constructor(message) {
    super(message);
    this.name = "DownloadTimeoutError";
    this.code = "EDOWNLOADTIMEOUT";
  }
}

function abortError(message) {
  const err = new Error(message || "The operation was aborted");
  err.name = "AbortError";
  err.code = "ABORT_ERR";
  err.nonRetryable = true;
  return err;
}

function isAbortError(err) {
  return Boolean(err) && (err.name === "AbortError" || err.code === "ABORT_ERR");
}

// Binary-version precedence must match run.js and verify-release-assets.js so
// install-time asset resolution agrees with runtime and release verification.
// `codewhaleBinaryVersion` lets a packaging-only npm release target a specific
// CodeWhale binary; legacy env vars and `deepseekBinaryVersion` stay supported
// for backward compatibility (#3769). `pkgObj`/`env` are injectable for tests.
function resolvePackageVersion(pkgObj = pkg, env = process.env) {
  const configuredVersion =
    env.CODEWHALE_VERSION ||
    env.DEEPSEEK_TUI_VERSION ||
    env.DEEPSEEK_VERSION ||
    pkgObj.codewhaleBinaryVersion ||
    pkgObj.deepseekBinaryVersion ||
    pkgObj.version;
  return String(configuredVersion).trim();
}

function resolveRepo(env = process.env) {
  return (
    env.CODEWHALE_GITHUB_REPO ||
    env.DEEPSEEK_TUI_GITHUB_REPO ||
    env.DEEPSEEK_GITHUB_REPO ||
    "codewhale-hq/CodeWhale"
  );
}

function isOptionalInstall(argv = process.argv.slice(2), env = process.env) {
  return (
    argv.includes("--optional") ||
    env.CODEWHALE_OPTIONAL_INSTALL === "1" ||
    env.DEEPSEEK_TUI_OPTIONAL_INSTALL === "1" ||
    env.DEEPSEEK_OPTIONAL_INSTALL === "1"
  );
}

function shouldForceDownload(env = process.env) {
  return (
    env.CODEWHALE_FORCE_DOWNLOAD === "1" ||
    env.DEEPSEEK_TUI_FORCE_DOWNLOAD === "1" ||
    env.DEEPSEEK_FORCE_DOWNLOAD === "1"
  );
}

function shouldDisableInstall(env = process.env) {
  return (
    env.CODEWHALE_DISABLE_INSTALL === "1" ||
    env.DEEPSEEK_TUI_DISABLE_INSTALL === "1" ||
    env.DEEPSEEK_DISABLE_INSTALL === "1"
  );
}

function isInstallContext(context) {
  return context === "install";
}

function isPnpmUserAgent(env = process.env) {
  return String(env.npm_config_user_agent || "").toLowerCase().includes("pnpm/");
}

function shouldSkipOptionalPostinstall(
  context,
  argv = process.argv.slice(2),
  env = process.env,
) {
  return isInstallContext(context) && isOptionalInstall(argv, env) && isPnpmUserAgent(env);
}

// Optional install only relaxes npm postinstall behavior. Runtime downloads
// keep the normal retry/timeout budget so first-run recovery stays resilient.
function defaultTimeoutMs(context = "runtime", env = process.env) {
  return isInstallContext(context) && isOptionalInstall(undefined, env)
    ? OPTIONAL_TIMEOUT_MS
    : DEFAULT_TIMEOUT_MS;
}

function defaultStallMs(context = "runtime", env = process.env) {
  return isInstallContext(context) && isOptionalInstall(undefined, env)
    ? OPTIONAL_STALL_MS
    : DEFAULT_STALL_MS;
}

function maxAttempts(context = "runtime", env = process.env) {
  return isInstallContext(context) && isOptionalInstall(undefined, env)
    ? OPTIONAL_MAX_ATTEMPTS
    : MAX_ATTEMPTS;
}

function binaryPaths() {
  const { codewhale, codew } = detectBinaryNames();
  const releaseDir = releaseBinaryDirectory();
  return {
    codewhale: {
      asset: codewhale,
      target: path.join(releaseDir, process.platform === "win32" ? "codewhale.exe" : "codewhale"),
    },
    codew: {
      asset: codew,
      target: path.join(releaseDir, process.platform === "win32" ? "codew.exe" : "codew"),
    },
  };
} // single binary — no tui asset (v0.9.5+)

// ────────────────────────────────────────────────────────────────────────────
// Logging / progress
// ────────────────────────────────────────────────────────────────────────────

function isQuietInstall(env = process.env) {
  if (
    env.CODEWHALE_QUIET_INSTALL === "1" ||
    env.DEEPSEEK_TUI_QUIET_INSTALL === "1"
  ) {
    return true;
  }
  const level = (env.npm_config_loglevel || "").toLowerCase();
  return level === "silent" || level === "error";
}

function logInfo(message) {
  if (isQuietInstall()) {
    return;
  }
  process.stderr.write(`codewhale: ${message}\n`);
}

function installFailureHint(error) {
  const message = error && error.message ? String(error.message) : "";
  const code = error && error.code ? String(error.code) : "";
  const releaseBase =
    process.env.CODEWHALE_RELEASE_BASE_URL ||
    process.env.DEEPSEEK_TUI_RELEASE_BASE_URL ||
    process.env.DEEPSEEK_RELEASE_BASE_URL;
  const networkMarkers = [
    "github.com",
    "ENOTFOUND",
    "EAI_AGAIN",
    "ETIMEDOUT",
    "ECONNRESET",
    "ENETUNREACH",
    "EHOSTUNREACH",
    "EDOWNLOADTIMEOUT",
  ];
  const looksLikeNetworkDownloadFailure = networkMarkers.some(
    (marker) => message.includes(marker) || code === marker,
  );
  if (!looksLikeNetworkDownloadFailure) {
    return "";
  }

  if (releaseBase) {
    return [
      "codewhale install hint:",
      `  CODEWHALE_RELEASE_BASE_URL resolves to ${releaseBase}`,
      "  Verify that this directory contains codewhale-artifacts-sha256.txt",
      "  plus the codewhale/codew binary assets for your platform (single binary).",
    ].join("\n");
  }

  return [
    "codewhale install hint:",
    "  The npm package downloads prebuilt binaries from GitHub Releases.",
    "  On Linux x64 it also probes the CNB first-party checksum manifest and uses",
    "  the first source whose HTTP response and manifest validate.",
    "  If both are unavailable, mirror the release assets and set:",
    "    CODEWHALE_RELEASE_BASE_URL=https://<mirror>/<release-asset-directory>/",
    "  or CODEWHALE_USE_CNB_MIRROR=1 on Linux x64.",
    "  The directory must contain codewhale-artifacts-sha256.txt and the platform binaries.",
    "  See https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md#npm-binary-download-times-out",
  ].join("\n");
}

function envInt(name, fallback, env = process.env) {
  const raw = env[name];
  if (!raw) {
    return fallback;
  }
  const parsed = Number.parseInt(String(raw).trim(), 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    return fallback;
  }
  return parsed;
}

function downloadTimeoutMs(context = "runtime", env = process.env) {
  return envInt(
    "CODEWHALE_DOWNLOAD_TIMEOUT_MS",
    envInt(
      "DEEPSEEK_TUI_DOWNLOAD_TIMEOUT_MS",
      envInt("DEEPSEEK_DOWNLOAD_TIMEOUT_MS", defaultTimeoutMs(context, env), env),
      env,
    ),
    env,
  );
}

function downloadStallMs(context = "runtime", env = process.env) {
  return envInt(
    "CODEWHALE_DOWNLOAD_STALL_MS",
    envInt(
      "DEEPSEEK_TUI_DOWNLOAD_STALL_MS",
      envInt("DEEPSEEK_DOWNLOAD_STALL_MS", defaultStallMs(context, env), env),
      env,
    ),
    env,
  );
}

function formatMb(bytes) {
  return (bytes / (1024 * 1024)).toFixed(0);
}

function createProgressReporter(assetName, totalBytes) {
  if (isQuietInstall()) {
    return { onChunk: () => {}, finish: () => {} };
  }
  const isTty = !!process.stderr.isTTY;
  const interactive = isTty;
  const tickBytes = interactive ? 1 * 1024 * 1024 : 5 * 1024 * 1024;
  const tickMs = 2_000;

  let received = 0;
  let lastBytesPrinted = 0;
  let lastTimePrinted = 0;
  let everPrinted = false;

  const render = (final) => {
    if (totalBytes && totalBytes > 0) {
      const pct = Math.min(100, Math.round((received / totalBytes) * 100));
      const line = `codewhale: downloading ${assetName}: ${formatMb(received)} / ${formatMb(totalBytes)} MB (${pct}%)`;
      if (interactive) {
        process.stderr.write(`${line}\r`);
      } else {
        process.stderr.write(`${line}\n`);
      }
    } else {
      const line = `codewhale: downloading ${assetName}: ${formatMb(received)} MB downloaded`;
      if (interactive) {
        process.stderr.write(`${line}\r`);
      } else {
        process.stderr.write(`${line}\n`);
      }
    }
    everPrinted = true;
    lastBytesPrinted = received;
    lastTimePrinted = Date.now();
  };

  return {
    onChunk(chunkLen) {
      received += chunkLen;
      const now = Date.now();
      if (
        received - lastBytesPrinted >= tickBytes ||
        (interactive && now - lastTimePrinted >= tickMs)
      ) {
        render(false);
      }
    },
    finish() {
      // Final line — always render once.
      render(true);
      if (interactive && everPrinted) {
        // Move past the carriage-return line and emit a "done" footer.
        process.stderr.write("\n");
      }
      process.stderr.write(`codewhale: ${assetName} ... done.\n`);
    },
  };
}

// ────────────────────────────────────────────────────────────────────────────
// Proxy support (HTTPS_PROXY / HTTP_PROXY / NO_PROXY) — pure Node, CONNECT
// tunnel + TLS upgrade for HTTPS targets.
// ────────────────────────────────────────────────────────────────────────────

function getProxyUrl(targetUrl) {
  const isHttps = targetUrl.protocol === "https:";
  const candidates = isHttps
    ? ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
    : ["HTTP_PROXY", "http_proxy"];
  for (const name of candidates) {
    const raw = process.env[name];
    if (raw && String(raw).trim() !== "") {
      return String(raw).trim();
    }
  }
  return null;
}

function shouldBypassProxy(host) {
  const raw = process.env.NO_PROXY || process.env.no_proxy;
  if (!raw) {
    return false;
  }
  const lower = String(host).toLowerCase();
  for (const part of String(raw).split(",")) {
    const entry = part.trim().toLowerCase();
    if (!entry) {
      continue;
    }
    if (entry === "*") {
      return true;
    }
    // Strip leading dot and any explicit port.
    const stripped = entry.replace(/^\./, "").replace(/:.*$/, "");
    if (!stripped) {
      continue;
    }
    if (lower === stripped || lower.endsWith(`.${stripped}`)) {
      return true;
    }
  }
  return false;
}

function parseProxy(proxyStr) {
  // Accept "http://user:pass@host:port" and bare "host:port".
  const normalized = /^[a-z][a-z0-9+\-.]*:\/\//i.test(proxyStr)
    ? proxyStr
    : `http://${proxyStr}`;
  const u = new URL(normalized);
  const port = u.port
    ? Number.parseInt(u.port, 10)
    : u.protocol === "https:"
      ? 443
      : 80;
  let auth = null;
  if (u.username) {
    const user = decodeURIComponent(u.username);
    const pass = u.password ? decodeURIComponent(u.password) : "";
    auth = Buffer.from(`${user}:${pass}`).toString("base64");
  }
  return {
    protocol: u.protocol,
    host: u.hostname,
    port,
    auth,
    raw: proxyStr,
  };
}

function connectThroughProxy(proxy, targetHost, targetPort, timeoutMs) {
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host: proxy.host, port: proxy.port });
    let settled = false;
    const fail = (err) => {
      if (settled) return;
      settled = true;
      try {
        socket.destroy();
      } catch {
        // ignore
      }
      reject(err);
    };

    const timer = timeoutMs > 0
      ? setTimeout(() => fail(new DownloadTimeoutError(
          `proxy CONNECT to ${proxy.host}:${proxy.port} timed out after ${timeoutMs} ms`,
        )), timeoutMs)
      : null;

    socket.once("error", (err) => {
      if (timer) clearTimeout(timer);
      // Surface proxy host so the user can fix it.
      const wrapped = new Error(
        `proxy connection failed (${proxy.host}:${proxy.port}): ${err.message}`,
      );
      wrapped.code = err.code;
      fail(wrapped);
    });

    socket.once("connect", () => {
      const lines = [
        `CONNECT ${targetHost}:${targetPort} HTTP/1.1`,
        `Host: ${targetHost}:${targetPort}`,
        "User-Agent: codewhale-installer",
        "Proxy-Connection: keep-alive",
      ];
      if (proxy.auth) {
        lines.push(`Proxy-Authorization: Basic ${proxy.auth}`);
      }
      const req = `${lines.join("\r\n")}\r\n\r\n`;

      let buf = Buffer.alloc(0);
      const onData = (chunk) => {
        buf = Buffer.concat([buf, chunk]);
        const idx = buf.indexOf("\r\n\r\n");
        if (idx === -1) {
          if (buf.length > 16 * 1024) {
            socket.removeListener("data", onData);
            fail(new Error(
              `proxy ${proxy.host}:${proxy.port} returned an oversized response header`,
            ));
          }
          return;
        }
        socket.removeListener("data", onData);
        const head = buf.slice(0, idx).toString("utf8");
        const firstLine = head.split(/\r?\n/, 1)[0] || "";
        const m = firstLine.match(/^HTTP\/\d\.\d\s+(\d{3})/);
        if (!m) {
          fail(new Error(`proxy ${proxy.host}:${proxy.port} returned invalid CONNECT reply: ${firstLine}`));
          return;
        }
        const code = Number.parseInt(m[1], 10);
        if (code !== 200) {
          fail(new Error(
            `proxy ${proxy.host}:${proxy.port} refused CONNECT to ${targetHost}:${targetPort}: HTTP ${code}`,
          ));
          return;
        }
        if (timer) clearTimeout(timer);
        if (settled) return;
        settled = true;
        // Any bytes past the header belong to the tunneled stream — but in
        // practice CONNECT 200 has no body; if it did, we'd lose those bytes
        // here. Keep it simple: trust well-behaved proxies.
        resolve(socket);
      };
      socket.on("data", onData);
      socket.write(req, "utf8");
    });
  });
}

// ────────────────────────────────────────────────────────────────────────────
// HTTP request with timeout, stall detection, and proxy support.
// ────────────────────────────────────────────────────────────────────────────

function httpRequest(rawUrl, opts = {}) {
  const context =
    opts.context === undefined || opts.context === null ? "runtime" : opts.context;
  const totalTimeoutMs =
    opts.totalTimeoutMs === undefined || opts.totalTimeoutMs === null
      ? downloadTimeoutMs(context)
      : opts.totalTimeoutMs;
  const stallMs =
    opts.stallMs === undefined || opts.stallMs === null
      ? downloadStallMs(context)
      : opts.stallMs;

  return new Promise((resolve, reject) => {
    let url;
    try {
      url = new URL(rawUrl);
    } catch (err) {
      reject(new NonRetryableError(`Invalid URL: ${rawUrl} (${err.message})`));
      return;
    }
    if (url.protocol !== "https:" && url.protocol !== "http:") {
      reject(new NonRetryableError(`Unsupported protocol: ${url.protocol}`));
      return;
    }

    const proxyStr = !shouldBypassProxy(url.hostname) ? getProxyUrl(url) : null;
    const isHttps = url.protocol === "https:";
    const port = url.port
      ? Number.parseInt(url.port, 10)
      : isHttps
        ? 443
        : 80;

    let totalTimer = null;
    let stallTimer = null;
    let settled = false;
    let handedOff = false;
    let req = null;
    let res = null;
    const signal = opts.signal;
    let onAbort = null;

    const cleanup = () => {
      if (totalTimer) {
        clearTimeout(totalTimer);
        totalTimer = null;
      }
      if (stallTimer) {
        clearTimeout(stallTimer);
        stallTimer = null;
      }
      if (signal && onAbort) {
        signal.removeEventListener("abort", onAbort);
        onAbort = null;
      }
    };

    const fail = (err) => {
      if (settled) {
        // The stall, total, and abort budgets outlive the handoff: once the
        // caller owns the body, end it with the error so the caller's own
        // `error` handler rejects instead of waiting on a silent socket.
        if (handedOff) {
          cleanup();
          try {
            if (res && !res.destroyed) res.destroy(err);
          } catch {
            // ignore
          }
        }
        return;
      }
      settled = true;
      cleanup();
      try {
        if (req && !req.destroyed) req.destroy();
      } catch {
        // ignore
      }
      try {
        if (res && !res.destroyed) res.destroy();
      } catch {
        // ignore
      }
      reject(err);
    };

    // Hand the live response stream to the caller. The timers stay armed
    // until the body ends or closes (see `fail`).
    const handOff = (response) => {
      settled = true;
      handedOff = true;
      response.on("close", () => cleanup());
      resolve({ redirect: null, response });
    };

    if (signal) {
      if (signal.aborted) {
        reject(abortError());
        return;
      }
      onAbort = function () {
        fail(abortError());
      };
      signal.addEventListener("abort", onAbort);
    }

    if (totalTimeoutMs > 0) {
      totalTimer = setTimeout(() => {
        fail(new DownloadTimeoutError(
          `download exceeded total timeout of ${totalTimeoutMs} ms ` +
          `(set CODEWHALE_DOWNLOAD_TIMEOUT_MS to raise it; current stall budget is ${stallMs} ms)`,
        ));
      }, totalTimeoutMs);
    }

    const armStallTimer = () => {
      if (stallMs <= 0) return;
      if (stallTimer) clearTimeout(stallTimer);
      stallTimer = setTimeout(() => {
        // A body the caller has paused (a slow disk pushing back through
        // `pipe`) is not a stalled network. The total budget still bounds it.
        if (handedOff && res && res.readableFlowing === false) {
          armStallTimer();
          return;
        }
        fail(new DownloadTimeoutError(
          `download stalled — no bytes received for ${stallMs} ms ` +
          `(set CODEWHALE_DOWNLOAD_STALL_MS to raise it; total budget is ${totalTimeoutMs} ms)`,
        ));
      }, stallMs);
    };

    const launch = (socket) => {
      const reqOptions = {
        method: "GET",
        host: url.hostname,
        port,
        path: `${url.pathname}${url.search || ""}`,
        headers: {
          Host: url.host,
          "User-Agent": "codewhale-installer",
          Accept: "*/*",
          Connection: "close",
        },
      };
      if (socket) {
        reqOptions.createConnection = () => socket;
        if (isHttps) {
          // Wrap raw TCP socket from CONNECT in TLS.
          const tlsSocket = tls.connect({
            socket,
            servername: url.hostname,
            ALPNProtocols: ["http/1.1"],
          });
          tlsSocket.once("error", (err) => fail(err));
          reqOptions.createConnection = () => tlsSocket;
        }
      }
      const client = isHttps ? https : http;
      try {
        req = client.request(reqOptions, (response) => {
          res = response;
          response.pause();
          armStallTimer();
          response.on("data", () => {
            armStallTimer();
          });
          response.on("end", () => {
            cleanup();
          });
          response.on("error", (err) => fail(err));

          const status = response.statusCode || 0;
          if (status >= 300 && status < 400 && response.headers.location) {
            cleanup();
            settled = true;
            response.resume();
            resolve({ redirect: response.headers.location, response: null });
            return;
          }
          if (status < 200 || status >= 300) {
            const err = new HttpStatusError(status, rawUrl);
            // 4xx: non-retryable; 5xx: retryable.
            if (status >= 400 && status < 500) {
              err.nonRetryable = true;
            }
            fail(err);
            return;
          }
          if (settled) return;
          handOff(response);
        });
        req.once("error", (err) => fail(err));
        req.once("socket", (s) => {
          // Belt-and-suspenders: surface socket-level errors quickly.
          s.once("error", (err) => fail(err));
        });
        req.end();
      } catch (err) {
        fail(err);
      }
    };

    if (proxyStr) {
      let proxy;
      try {
        proxy = parseProxy(proxyStr);
      } catch (err) {
        fail(new NonRetryableError(
          `Invalid proxy URL "${proxyStr}": ${err.message}`,
        ));
        return;
      }
      if (!isHttps) {
        // Plain HTTP through proxy — send absolute URI, no CONNECT.
        const client = http;
        try {
          req = client.request(
            {
              host: proxy.host,
              port: proxy.port,
              method: "GET",
              path: rawUrl,
              headers: {
                Host: url.host,
                "User-Agent": "codewhale-installer",
                Accept: "*/*",
                Connection: "close",
                ...(proxy.auth ? { "Proxy-Authorization": `Basic ${proxy.auth}` } : {}),
              },
            },
            (response) => {
              res = response;
              response.pause();
              armStallTimer();
              response.on("data", () => armStallTimer());
              response.on("end", () => cleanup());
              response.on("error", (err) => fail(err));
              const status = response.statusCode || 0;
              if (status >= 300 && status < 400 && response.headers.location) {
                cleanup();
                settled = true;
                response.resume();
                resolve({ redirect: response.headers.location, response: null });
                return;
              }
              if (status < 200 || status >= 300) {
                const err = new HttpStatusError(status, rawUrl);
                if (status >= 400 && status < 500) err.nonRetryable = true;
                fail(err);
                return;
              }
              if (settled) return;
              handOff(response);
            },
          );
          req.once("error", (err) => fail(err));
          req.end();
        } catch (err) {
          fail(err);
        }
        return;
      }

      // HTTPS through proxy: CONNECT tunnel + TLS upgrade.
      connectThroughProxy(proxy, url.hostname, port, Math.max(stallMs, 5_000))
        .then((tcpSocket) => {
          if (settled) {
            try { tcpSocket.destroy(); } catch { /* ignore */ }
            return;
          }
          const tlsSocket = tls.connect({
            socket: tcpSocket,
            servername: url.hostname,
            ALPNProtocols: ["http/1.1"],
          });
          tlsSocket.once("error", (err) => fail(err));
          tlsSocket.once("secureConnect", () => {
            if (settled) {
              try { tlsSocket.destroy(); } catch { /* ignore */ }
              return;
            }
            const reqOptions = {
              method: "GET",
              createConnection: () => tlsSocket,
              path: `${url.pathname}${url.search || ""}`,
              headers: {
                Host: url.host,
                "User-Agent": "codewhale-installer",
                Accept: "*/*",
                Connection: "close",
              },
            };
            try {
              req = https.request(reqOptions, (response) => {
                res = response;
                response.pause();
                armStallTimer();
                response.on("data", () => armStallTimer());
                response.on("end", () => cleanup());
                response.on("error", (err) => fail(err));
                const status = response.statusCode || 0;
                if (status >= 300 && status < 400 && response.headers.location) {
                  cleanup();
                  settled = true;
                  response.resume();
                  resolve({ redirect: response.headers.location, response: null });
                  return;
                }
                if (status < 200 || status >= 300) {
                  const err = new HttpStatusError(status, rawUrl);
                  if (status >= 400 && status < 500) err.nonRetryable = true;
                  fail(err);
                  return;
                }
                if (settled) return;
                handOff(response);
              });
              req.once("error", (err) => fail(err));
              req.end();
            } catch (err) {
              fail(err);
            }
          });
        })
        .catch((err) => fail(err));
      return;
    }

    // No proxy — direct connection.
    launch(null);
  });
}

// ────────────────────────────────────────────────────────────────────────────
// Retry wrapper
// ────────────────────────────────────────────────────────────────────────────

function isRetryable(err) {
  if (!err) return false;
  if (isAbortError(err)) return false;
  if (err.nonRetryable) return false;
  if (err.retryable === true) return true;
  if (err instanceof NonRetryableError) return false;
  if (err instanceof DownloadTimeoutError) return true;
  // withRetry() rethrows a plain Error while preserving name/status, so wrapped
  // HTTP 5xx failures still classify as retryable during optional postinstall.
  if (
    (err instanceof HttpStatusError || err.name === "HttpStatusError") &&
    typeof err.status === "number"
  ) {
    return err.status >= 500;
  }
  if (err.code && RETRYABLE_NET_CODES.has(err.code)) return true;
  // Network-flavored messages we may see without a code.
  const msg = String(err.message || "").toLowerCase();
  if (msg.includes("network") && msg.includes("unreachable")) return true;
  if (msg.includes("socket hang up")) return true;
  if (msg.includes("aborted")) return true;
  return false;
}

function backoffDelay(attempt) {
  // attempt is 1-indexed; first retry waits ~1s.
  const base = BASE_BACKOFF_MS * 2 ** (attempt - 1);
  const jitter = (Math.random() * 0.4 - 0.2) * base; // ±20%
  return Math.max(0, Math.round(base + jitter));
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function sleepWithSignal(ms, signal) {
  if (!signal) {
    return sleep(ms);
  }
  if (signal.aborted) {
    return Promise.reject(abortError());
  }
  return new Promise((resolve, reject) => {
    let timer = null;
    const cleanup = () => {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      signal.removeEventListener("abort", onAbort);
    };
    const onAbort = () => {
      cleanup();
      reject(abortError());
    };
    timer = setTimeout(() => {
      cleanup();
      resolve();
    }, ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

async function withRetry(label, fn, context, signal) {
  const resolvedContext =
    context === undefined || context === null ? "runtime" : context;
  let lastErr;
  const attemptLimit = maxAttempts(resolvedContext);
  for (let attempt = 1; attempt <= attemptLimit; attempt++) {
    if (signal && signal.aborted) {
      throw abortError();
    }
    try {
      return await fn(attempt);
    } catch (err) {
      lastErr = err;
      if (isAbortError(err) || !isRetryable(err) || attempt === attemptLimit) {
        break;
      }
      const wait = backoffDelay(attempt);
      logInfo(
        `${label} failed (attempt ${attempt}/${attemptLimit}): ${err.message}; retrying in ${wait} ms`,
      );
      if (attempt === 1) {
        const hint = installFailureHint(err);
        if (hint) {
          process.stderr.write(`${hint}\n`);
        }
      }
      await sleepWithSignal(wait, signal);
      if (signal && signal.aborted) {
        throw abortError();
      }
    }
  }
  const msg = lastErr && lastErr.message ? lastErr.message : String(lastErr);
  const wrapped = new Error(
    `${label} failed after ${attemptLimit} attempt(s): ${msg}`,
  );
  // Preserve retry classification metadata because the install entrypoint uses
  // the wrapped error to decide whether optional postinstall may ignore it.
  if (lastErr && lastErr.code) {
    wrapped.code = lastErr.code;
  }
  if (lastErr && lastErr.name) {
    wrapped.name = lastErr.name;
  }
  if (lastErr && typeof lastErr.status === "number") {
    wrapped.status = lastErr.status;
  }
  if (lastErr && lastErr.nonRetryable) {
    wrapped.nonRetryable = true;
  }
  if (lastErr && lastErr.stack) {
    wrapped.cause = lastErr;
  }
  throw wrapped;
}

// ────────────────────────────────────────────────────────────────────────────
// Public download primitives (now retry + progress aware)
// ────────────────────────────────────────────────────────────────────────────

async function followRedirects(url, opts = {}) {
  const maxRedirects = 10;
  let current = url;
  for (let hop = 0; hop < maxRedirects; hop++) {
    const result = await httpRequest(current, opts);
    if (result.redirect) {
      try {
        current = new URL(result.redirect, current).toString();
      } catch {
        current = result.redirect;
      }
      continue;
    }
    return result;
  }
  throw new NonRetryableError(`too many redirects starting at ${url}`);
}

function streamToFile(response, destination, progress, signal) {
  return new Promise((resolve, reject) => {
    const sink = createWriteStream(destination);
    let done = false;
    const onAbort = () => {
      try {
        response.destroy();
      } catch {
        // ignore
      }
      finish(abortError());
    };
    const finish = (err) => {
      if (done) return;
      done = true;
      if (signal) {
        signal.removeEventListener("abort", onAbort);
      }
      if (err) {
        sink.destroy();
        reject(err);
      } else {
        resolve();
      }
    };
    response.on("data", (chunk) => {
      if (progress) progress.onChunk(chunk.length);
    });
    response.on("error", (err) => finish(err));
    sink.on("error", (err) => finish(err));
    sink.on("finish", () => finish(null));
    if (signal) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener("abort", onAbort, { once: true });
    }
    response.pipe(sink);
  });
}

async function download(url, destination, options = {}) {
  await mkdir(path.dirname(destination), { recursive: true });
  const assetName = options.assetName || path.basename(destination);
  const context =
    options.context === undefined || options.context === null ? "runtime" : options.context;
  const attemptLimit = maxAttempts(context);
  await withRetry(`download ${assetName}`, async (attempt) => {
    const result = await followRedirects(url, {
      context,
      totalTimeoutMs: downloadTimeoutMs(context),
      stallMs: downloadStallMs(context),
      signal: options.signal,
    });
    const response = result.response;
    const lenHeader = response.headers["content-length"];
    const total = lenHeader ? Number.parseInt(lenHeader, 10) : 0;
    const progress = createProgressReporter(assetName, Number.isFinite(total) ? total : 0);
    if (attempt > 1) {
      logInfo(`retry attempt ${attempt}/${attemptLimit} for ${assetName}`);
    }
    try {
      await streamToFile(response, destination, progress, options.signal);
    } catch (err) {
      // Ensure we don't leave a partial file confusing future attempts.
      try {
        await unlink(destination);
      } catch {
        // ignore
      }
      throw err;
    }
    progress.finish();
  }, context, options.signal);
}

async function downloadText(url, options = {}) {
  const context =
    options.context === undefined || options.context === null ? "runtime" : options.context;
  const totalTimeoutMs =
    options.totalTimeoutMs === undefined || options.totalTimeoutMs === null
      ? downloadTimeoutMs(context)
      : options.totalTimeoutMs;
  const stallMs =
    options.stallMs === undefined || options.stallMs === null
      ? downloadStallMs(context)
      : options.stallMs;
  return withRetry(`fetch ${url}`, async () => {
    const result = await followRedirects(url, {
      context,
      totalTimeoutMs,
      stallMs,
      signal: options.signal,
    });
    const response = result.response;
    response.setEncoding("utf8");
    // NOTE: do NOT use `for await (const chunk of response)` here.
    // `httpRequest` attaches a `data` listener on the response to re-arm
    // the stall timer, which puts the stream in flowing mode. The async
    // iterator expects paused mode and will silently miss every chunk —
    // this manifested as an empty checksum manifest in the npm wrapper
    // smoke test ("Checksum manifest is missing <asset>"). Subscribing
    // to `data` events directly stacks alongside the stall listener and
    // both fire per chunk, so we collect the body correctly without
    // disturbing the stall detection.
    return new Promise((resolve, reject) => {
      const chunks = [];
      let settled = false;
      const signal = options.signal;
      const cleanup = () => {
        if (signal) {
          signal.removeEventListener("abort", onAbort);
        }
      };
      const finish = (error, value) => {
        if (settled) return;
        settled = true;
        cleanup();
        if (error) {
          reject(error);
        } else {
          resolve(value);
        }
      };
      const onAbort = () => {
        try {
          response.destroy();
        } catch {
          // ignore
        }
        finish(abortError());
      };
      response.on("data", (chunk) => {
        chunks.push(chunk);
      });
      response.on("end", () => {
        finish(null, chunks.join(""));
      });
      response.on("error", (error) => finish(error));
      if (signal) {
        if (signal.aborted) {
          onAbort();
          return;
        }
        signal.addEventListener("abort", onAbort, { once: true });
      }
      response.resume();
    });
  }, context, options.signal);
}

async function readLocalVersion(file) {
  return readFile(file, "utf8").catch(() => "");
}

async function fileExists(file) {
  try {
    const result = await stat(file);
    return result.isFile();
  } catch {
    return false;
  }
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
      throw new NonRetryableError(`Invalid checksum manifest line: ${trimmed}`);
    }
    checksums.set(match[2], match[1].toLowerCase());
  }
  return checksums;
}

async function sha256File(filePath) {
  const content = await readFile(filePath);
  return crypto.createHash("sha256").update(content).digest("hex");
}

async function verifyChecksum(filePath, assetName, checksums, sourceLabel) {
  const expected = checksums.get(assetName);
  if (!expected) {
    const from = sourceLabel ? ` from ${sourceLabel}` : "";
    throw new NonRetryableError(`Checksum manifest is missing ${assetName}${from}`);
  }
  const actual = await sha256File(filePath);
  if (actual !== expected) {
    // Bytes are corrupted; another fetch is unlikely to help without a fix
    // upstream. Mark non-retryable. Never mix a locked source's bytes with
    // another source's manifest.
    const from = sourceLabel ? ` from ${sourceLabel}` : "";
    throw new NonRetryableError(
      `Checksum mismatch for ${assetName}${from}: expected ${expected}, got ${actual}`,
    );
  }
}

async function checksumMatches(filePath, assetName, checksums) {
  const expected = checksums.get(assetName);
  if (!expected) {
    throw new NonRetryableError(`Checksum manifest is missing ${assetName}`);
  }
  const actual = await sha256File(filePath);
  return actual === expected;
}

function formatSourceReceipt(source, version) {
  return [
    `source=${source.id}`,
    `label=${source.label}`,
    `base=${source.baseUrl}`,
    `version=${version}`,
    "",
  ].join("\n");
}

async function writeSourceReceipt(targetPath, source, version) {
  await writeFile(`${targetPath}.source`, formatSourceReceipt(source, version), "utf8");
}

function assertManifestHasAssets(checksums, requiredAssets, label) {
  const missing = [];
  for (let i = 0; i < requiredAssets.length; i += 1) {
    const asset = requiredAssets[i];
    if (!checksums.has(asset)) {
      missing.push(asset);
    }
  }
  if (missing.length > 0) {
    throw new NonRetryableError(
      `${label} checksum manifest is missing ${missing.join(", ")}`,
    );
  }
}

async function fetchChecksumManifest(url, options) {
  const fetchText = options.fetchText || downloadText;
  const text = await fetchText(url, {
    context: options.context,
    signal: options.signal,
    totalTimeoutMs:
      options.totalTimeoutMs === undefined || options.totalTimeoutMs === null
        ? MANIFEST_TIMEOUT_MS
        : options.totalTimeoutMs,
    stallMs:
      options.stallMs === undefined || options.stallMs === null
        ? MANIFEST_STALL_MS
        : options.stallMs,
  });
  return parseChecksumManifest(text);
}

async function loadSourceManifest(source, options) {
  const url = releaseAssetUrlFromBase(CHECKSUM_MANIFEST, source.baseUrl);
  const checksums = await fetchChecksumManifest(url, options);
  assertManifestHasAssets(checksums, options.requiredAssets || [], source.label);
  return {
    id: source.id,
    label: source.label,
    baseUrl: source.baseUrl,
    checksums,
  };
}

function aggregateSourceErrors(options, failures) {
  const parts = [];
  let allNonRetryable = failures.length > 0;
  let anyRetryable = false;
  for (let i = 0; i < failures.length; i += 1) {
    const failure = failures[i];
    const message =
      failure.error && failure.error.message
        ? failure.error.message
        : String(failure.error);
    parts.push(`${failure.source.label}: ${message}`);
    if (
      !(
        failure.error &&
        (failure.error.nonRetryable || failure.error instanceof NonRetryableError)
      )
    ) {
      allNonRetryable = false;
    }
    if (isRetryable(failure.error)) {
      anyRetryable = true;
    }
  }
  const err = new Error(
    `No usable first-party release source for v${options.version}. ${parts.join("; ")}`,
  );
  if (allNonRetryable) {
    err.nonRetryable = true;
  } else if (anyRetryable) {
    err.retryable = true;
  }
  return err;
}

async function raceFirstPartyManifests(sources, options) {
  logInfo(
    `probing ${sources.map((source) => source.label).join(" and ")} checksum manifests`,
  );
  const controllers = sources.map(() => new AbortController());

  return new Promise((resolve, reject) => {
    let remaining = sources.length;
    const failures = [];
    let settled = false;

    const finishSuccess = (index, selected) => {
      if (settled) {
        return;
      }
      settled = true;
      for (let i = 0; i < controllers.length; i += 1) {
        if (i !== index) {
          try {
            controllers[i].abort();
          } catch {
            // ignore
          }
        }
      }
      logInfo(`selected ${selected.label} for v${options.version}`);
      resolve(selected);
    };

    const finishFailure = (source, error) => {
      if (settled) {
        return;
      }
      if (isAbortError(error)) {
        remaining -= 1;
        if (remaining === 0) {
          settled = true;
          reject(aggregateSourceErrors(options, failures));
        }
        return;
      }
      failures.push({ source, error });
      remaining -= 1;
      if (remaining === 0) {
        settled = true;
        reject(aggregateSourceErrors(options, failures));
      }
    };

    for (let i = 0; i < sources.length; i += 1) {
      const source = sources[i];
      loadSourceManifest(source, {
        context: options.context,
        fetchText: options.fetchText,
        requiredAssets: options.requiredAssets,
        signal: controllers[i].signal,
      }).then(
        (selected) => finishSuccess(i, selected),
        (error) => finishFailure(source, error),
      );
    }
  });
}

async function selectReleaseSource(options) {
  const version = options.version;
  const repo = options.repo || "codewhale-hq/CodeWhale";
  const env = options.env || process.env;
  const platform =
    options.platform === undefined || options.platform === null
      ? os.platform()
      : options.platform;
  const arch =
    options.arch === undefined || options.arch === null ? os.arch() : options.arch;
  const requiredAssets = options.requiredAssets || [];
  const context =
    options.context === undefined || options.context === null
      ? "runtime"
      : options.context;
  const fetchText = options.fetchText;
  const override = explicitReleaseBase(env);
  if (override) {
    logInfo(`using explicit release base for v${version}`);
    return loadSourceManifest(
      {
        id: "override",
        label: "explicit release base",
        baseUrl: override,
      },
      {
        context,
        fetchText,
        requiredAssets,
      },
    );
  }
  if (usesCnbMirror(env)) {
    assertCnbMirrorSupportedPlatform(platform, arch);
    logInfo(`using CNB first-party mirror for v${version}`);
    return loadSourceManifest(
      {
        id: "cnb",
        label: "CNB first-party mirror",
        baseUrl: cnbReleaseBaseUrl(version),
      },
      {
        context,
        fetchText,
        requiredAssets,
      },
    );
  }
  if (shouldRaceFirstPartyMirrors(env, platform, arch)) {
    const sources = options.sources || firstPartyReleaseSources(version, repo);
    return raceFirstPartyManifests(sources, {
      version,
      context,
      fetchText,
      requiredAssets,
    });
  }
  logInfo(`using GitHub Releases for v${version}`);
  return loadSourceManifest(
    {
      id: "github",
      label: "GitHub Releases",
      baseUrl: githubReleaseBaseUrl(version, repo),
    },
    {
      context,
      fetchText,
      requiredAssets,
    },
  );
}

async function loadChecksums(version, repo, options = {}) {
  return parseChecksumManifest(await downloadText(checksumManifestUrl(version, repo), options));
}

function existingBinaryCandidates(targetPath, assetName) {
  const candidates = [targetPath];
  const assetPath = path.join(path.dirname(targetPath), assetName);
  if (assetPath !== targetPath) {
    candidates.push(assetPath);
  }
  return candidates;
}

async function adoptExistingBinaryIfValid(targetPath, assetName, version, getChecksums, marker) {
  const candidates = [];
  for (const candidate of existingBinaryCandidates(targetPath, assetName)) {
    if (await fileExists(candidate)) {
      candidates.push(candidate);
    }
  }
  if (candidates.length === 0) {
    return false;
  }

  const checksums = await getChecksums();
  for (const candidate of candidates) {
    if (!(await checksumMatches(candidate, assetName, checksums))) {
      continue;
    }
    preflightGlibc(candidate);
    if (candidate !== targetPath) {
      await rename(candidate, targetPath);
    }
    if (process.platform !== "win32") {
      await chmod(targetPath, 0o755);
    }
    await writeFile(marker, String(version), "utf8");
    return true;
  }
  return false;
}

async function resolveLockedSource(options) {
  let sourceId = options.sourceId;
  let sourceLabel = options.sourceLabel;
  let baseUrl = options.baseUrl;
  if (options.getSource) {
    const source = await options.getSource();
    sourceId = source.id;
    sourceLabel = source.label;
    baseUrl = source.baseUrl;
  }
  return { sourceId, sourceLabel, baseUrl };
}

async function ensureBinary(targetPath, assetName, version, repo, getChecksums, options = {}) {
  const marker = `${targetPath}.version`;
  const env = options.env || process.env;
  const downloadIfNeeded = shouldForceDownload(env);
  if (!downloadIfNeeded) {
    const existing = await fileExists(targetPath);
    if (existing) {
      const markerVersion = await readLocalVersion(marker);
      if (markerVersion === String(version)) {
        return targetPath;
      }
    }
    if (await adoptExistingBinaryIfValid(targetPath, assetName, version, getChecksums, marker)) {
      const locked = await resolveLockedSource(options);
      if (locked.sourceId) {
        await writeSourceReceipt(targetPath, {
          id: locked.sourceId,
          label: locked.sourceLabel || locked.sourceId,
          baseUrl: locked.baseUrl || "",
        }, version);
      }
      return targetPath;
    }
  }
  const checksums = await getChecksums();
  const locked = await resolveLockedSource(options);
  const url = locked.baseUrl
    ? releaseAssetUrlFromBase(assetName, locked.baseUrl)
    : releaseAssetUrl(assetName, version, repo);
  const destination = `${targetPath}.${process.pid}.${Date.now()}.download`;
  const downloadFn = options.download || download;
  const progressName = locked.sourceLabel
    ? `${assetName} from ${locked.sourceLabel}`
    : assetName;
  await downloadFn(url, destination, { assetName: progressName, context: options.context });
  try {
    await verifyChecksum(destination, assetName, checksums, locked.sourceLabel);
    preflightGlibc(destination);
  } catch (error) {
    await unlink(destination).catch(() => {});
    throw error;
  }
  if (process.platform !== "win32") {
    await chmod(destination, 0o755);
  }
  await rename(destination, targetPath);
  await writeFile(marker, String(version), "utf8");
  if (locked.sourceId) {
    await writeSourceReceipt(targetPath, {
      id: locked.sourceId,
      label: locked.sourceLabel || locked.sourceId,
      baseUrl: locked.baseUrl || "",
    }, version);
  }
  return targetPath;
}

// Optional install may only downgrade retryable download failures to warnings.
// Unsupported platforms, checksum mismatches, glibc compatibility errors, and
// malformed release metadata must still fail with actionable diagnostics.
function shouldIgnoreInstallFailure(
  context,
  error,
  argv = process.argv.slice(2),
  env = process.env,
) {
  return isInstallContext(context) && isOptionalInstall(argv, env) && isRetryable(error);
}

function preflightCompiledHost(file, host) {
  if (!host.target.startsWith("linux-")) return;
  const required = detectBinaryRequiredGlibc(file);
  const available = detectHostGlibc();
  if (host.libc === "glibc" && (!available || (required && glibc.compareVersion(available, required) < 0))) {
    throw new NonRetryableError("compiled Bun image requires a matching GNU libc; the Codewhale CLI remains static musl. Use Node on a musl-only installation.");
  }
  if (host.libc === "musl" && required) throw new NonRetryableError("qualified musl host unexpectedly contains GNU libc dependencies");
}

async function prepareCompiledHost({ version, releaseDir, source, context, options }) {
  const catalogUrl = releaseAssetUrlFromBase(compiledHosts.HOST_CATALOG, source.baseUrl);
  const fetchText = options.fetchText || downloadText;
  const text = await fetchText(catalogUrl, { context });
  compiledHosts.verifyBytes(Buffer.from(text), source.checksums.get(compiledHosts.HOST_CATALOG), compiledHosts.HOST_CATALOG);
  const catalog = compiledHosts.parseCatalog(text, version);
  const host = compiledHosts.selectedHost(catalog, options.platform, options.arch);
  const suffix = host.target.startsWith("windows-") ? ".exe" : "";
  const payloads = [
    { asset: host.asset, name: compiledHosts.HOST_NAME + suffix, hash: host.sha256, executable: true },
    { asset: host.notices_asset, name: compiledHosts.HOST_NAME + ".LICENSES.txt", hash: host.notices_sha256 },
    { asset: host.source_asset, name: compiledHosts.HOST_NAME + ".relink-source.tar.gz", hash: host.source_sha256 },
  ];
  const stageDir = await fs.promises.mkdtemp(path.join(releaseDir, ".compiled-host-"));
  const snapshots = [];
  const published = [];
  let retainBackups = false;
  const existingHash = async (file) => {
    try {
      const metadata = await fs.promises.lstat(file);
      if (!metadata.isFile() || metadata.isSymbolicLink()) throw new NonRetryableError(`refusing nonregular compiled host destination ${file}`);
      return compiledHosts.sha256(await readFile(file));
    } catch (error) { if (error.code === "ENOENT") return undefined; throw error; }
  };
  const cleanup = async () => { if (!retainBackups) await fs.promises.rm(stageDir, { recursive: true, force: true }); };
  const rollback = async (original) => {
    const failures = [];
    for (const snapshot of published.slice().reverse()) {
      try {
        if (await existingHash(snapshot.target) !== snapshot.newHash) throw new Error(`destination changed after publication: ${snapshot.target}`);
        if (snapshot.hash) await rename(path.join(stageDir, snapshot.name + ".previous"), snapshot.target);
        else await unlink(snapshot.target);
      } catch (error) { failures.push(error.message); }
    }
    if (failures.length) {
      retainBackups = true;
      throw new NonRetryableError(`${original.message}; rollback incomplete: ${failures.join("; ")}. Recovery files retained at ${stageDir}`);
    }
    throw original;
  };
  try {
    for (const payload of payloads) {
      if (source.checksums.get(payload.asset) !== payload.hash) throw new NonRetryableError(`catalog and checksum manifest disagree for ${payload.asset}`);
      const stage = path.join(stageDir, payload.name);
      await (options.download || download)(releaseAssetUrlFromBase(payload.asset, source.baseUrl), stage, { context, assetName: payload.asset });
      compiledHosts.verifyBytes(await readFile(stage), payload.hash, payload.asset);
      if (payload.executable) preflightCompiledHost(stage, host);
      if (process.platform !== "win32") await chmod(stage, payload.executable ? 0o755 : 0o644);
    }
    const receiptName = compiledHosts.HOST_NAME + ".release.json";
    await writeFile(path.join(stageDir, receiptName), text, { mode: 0o644 });
    const oldReceipt = path.join(releaseDir, receiptName);
    const oldReceiptHash = await existingHash(oldReceipt);
    let oldPayloadHashes;
    if (oldReceiptHash) {
      const previous = compiledHosts.selectedHost(compiledHosts.parseCatalog(await readFile(oldReceipt)), options.platform, options.arch);
      oldPayloadHashes = [previous.sha256, previous.notices_sha256, previous.source_sha256];
    }
    const names = [...payloads.map((payload) => payload.name), receiptName];
    for (const [index, name] of names.entries()) {
      const target = path.join(releaseDir, name);
      const hash = await existingHash(target);
      if (index < payloads.length && hash && (!oldPayloadHashes || hash !== oldPayloadHashes[index])) throw new NonRetryableError(`refusing unclaimed or modified compiled host destination ${target}`);
      if (hash) {
        const backup = path.join(stageDir, name + ".previous");
        await fs.promises.copyFile(target, backup);
        if (compiledHosts.sha256(await readFile(backup)) !== hash) throw new NonRetryableError(`compiled host changed while backing up ${target}`);
      }
      snapshots.push({ name, target, hash, newHash: compiledHosts.sha256(await readFile(path.join(stageDir, name))) });
    }
  } catch (error) { await cleanup(); throw error; }
  return {
    cleanup,
    async publish() {
      try {
        // Validate every destination before publication, then each immediately
        // before replacement. Only bytes proven by the prior receipt are owned.
        for (const snapshot of snapshots) if (await existingHash(snapshot.target) !== snapshot.hash) throw new NonRetryableError(`compiled host destination changed during installation: ${snapshot.target}`);
        for (const snapshot of snapshots) {
          if (await existingHash(snapshot.target) !== snapshot.hash) throw new NonRetryableError(`compiled host destination changed during installation: ${snapshot.target}`);
          const staged = path.join(stageDir, snapshot.name);
          if (snapshot.hash === undefined) {
            await fs.promises.link(staged, snapshot.target); // fresh install never clobbers a raced-in file
            published.push(snapshot);
            await unlink(staged);
          } else {
            await rename(staged, snapshot.target);
            published.push(snapshot);
          }
        }
      } catch (error) { await rollback(error); }
    },
  };
}

async function installCompiledHost(options) {
  const prepared = await prepareCompiledHost(options);
  try { await prepared.publish(); } finally { await prepared.cleanup(); }
}

async function run(options = {}) {
  const context =
    options.context === undefined || options.context === null ? "runtime" : options.context;
  const env = options.env || process.env;
  if (shouldDisableInstall(env)) {
    return;
  }
  if (shouldSkipOptionalPostinstall(context, process.argv.slice(2), env)) {
    logInfo(
      "pnpm optional postinstall detected; skipping install-time download. The binary will be checked on first run.",
    );
    return;
  }
  const version = resolvePackageVersion(pkg, env);
  const repo = resolveRepo(env);
  const paths = options.paths || binaryPaths();
  if (
    isInstallContext(context) &&
    (options.platform || os.platform()) === "openharmony" &&
    !isQuietInstall(env)
  ) {
    process.stderr.write(
      `codewhale: note: installing ${paths.codewhale.asset}, the native HarmonyOS/OpenHarmony build.\n` +
        `It is code-signed, but a native HarmonyOS terminal (HiShell) is not a Linux shell:\n` +
        `there is no git and no package manager, and the debugging shell (hdc shell) cannot\n` +
        `execute it at all. details: docs/HarmonyOS.md\n` +
        `${openHarmonyDirections()}\n`,
    );
  }
  const releaseDir = options.releaseDir || releaseBinaryDirectory();
  await mkdir(releaseDir, { recursive: true });

  let sourcePromise;
  const getSource = () => {
    if (!sourcePromise) {
      sourcePromise = selectReleaseSource({
        version,
        repo,
        requiredAssets: [paths.codewhale.asset, paths.codew.asset, ...(compiledHosts.requested(env) ? [compiledHosts.HOST_CATALOG] : [])],
        context,
        env,
        platform: options.platform,
        arch: options.arch,
        sources: options.sources,
        fetchText: options.fetchText,
      });
    }
    return sourcePromise;
  };
  const getChecksums = () => getSource().then((source) => source.checksums);

  // Fully validate and stage an explicitly requested companion before any CLI
  // replacement. Missing qualification cannot leave an updated CLI behind.
  const preparedHost = compiledHosts.requested(env)
    ? await prepareCompiledHost({ version, releaseDir, source: await getSource(), context, options })
    : undefined;
  try {
    await Promise.all([
    ensureBinary(paths.codewhale.target, paths.codewhale.asset, version, repo, getChecksums, {
      context,
      getSource,
      download: options.download,
      env,
    }),
    ensureBinary(paths.codew.target, paths.codew.asset, version, repo, getChecksums, {
      context,
      getSource,
      download: options.download,
      env,
    }),
  ]); // single binary
    if (preparedHost) await preparedHost.publish();
  } finally { if (preparedHost) await preparedHost.cleanup(); }
}

async function getBinaryPath(name) {
  await run({ context: "runtime" });
  const paths = binaryPaths();
  if (name === "codewhale") {
    return paths.codewhale.target;
  }
  if (name === "codew") {
    return paths.codew.target;
  }
  if (name === "codewhale-tui") {
    // v0.9.5 single-binary: codewhale-tui is now an alias to codewhale for backwards compat
    return paths.codewhale.target;
  }
  throw new Error(`Unknown binary: ${name}`);
}

module.exports = {
  getBinaryPath,
  installFailureHint,
  run,
  _internal: {
    resolvePackageVersion,
    resolveRepo,
    isOptionalInstall,
    shouldForceDownload,
    shouldDisableInstall,
    isQuietInstall,
    adoptExistingBinaryIfValid,
    shouldIgnoreInstallFailure,
    shouldSkipOptionalPostinstall,
    httpRequest,
    defaultTimeoutMs,
    defaultStallMs,
    downloadTimeoutMs,
    downloadStallMs,
    binaryPaths,
    installCompiledHost,
    prepareCompiledHost,
    preflightCompiledHost,
    ensureBinary,
    maxAttempts,
    withRetry,
    selectReleaseSource,
    downloadText,
    download,
    parseChecksumManifest,
    MANIFEST_TIMEOUT_MS,
    MANIFEST_STALL_MS,
  },
};

if (require.main === module) {
  run({ context: "install" }).catch((error) => {
    console.error("codewhale install failed:", error.message);
    const hint = installFailureHint(error);
    if (hint) {
      console.error(hint);
    }
    if (shouldIgnoreInstallFailure("install", error)) {
      console.error(
        "Optional install enabled; continuing without a usable binary. The download will be retried on first run.",
      );
      process.exit(0);
    }
    process.exit(1);
  });
}
