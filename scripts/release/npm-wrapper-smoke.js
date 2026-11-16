#!/usr/bin/env node

const crypto = require("crypto");
const fs = require("fs");
const fsp = require("fs/promises");
const https = require("https");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");

const repoRoot = path.resolve(__dirname, "..", "..");
const packageDir = path.join(repoRoot, "npm", "codewhale");
const prepareAssetsScript = path.join(
  repoRoot,
  "scripts",
  "release",
  "prepare-local-release-assets.js",
);
const CLEANUP_MAX_RETRIES = 8;
const CLEANUP_RETRY_DELAY_MS = 250;

function shellQuote(value) {
  return /\s/.test(value) ? JSON.stringify(value) : value;
}

function usesWindowsCommandShim(command) {
  return process.platform === "win32" && (command === "npm" || command === "npx");
}

function runCommand(command, args, options = {}) {
  const cwd = options.cwd || repoRoot;
  console.log(`$ ${[command, ...args].map(shellQuote).join(" ")}`);
  const child = spawn(command, args, {
    cwd,
    env: {
      ...process.env,
      ...(options.env || {}),
    },
    encoding: "utf8",
    shell: usesWindowsCommandShim(command),
    stdio: options.capture ? ["ignore", "pipe", "pipe"] : "inherit",
    windowsHide: true,
  });

  if (!options.capture) {
    return new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (status) => {
        if (status === 0) {
          resolve({ stdout: "", stderr: "" });
        } else {
          reject(new Error(`${command} exited with status ${status}`));
        }
      });
    });
  }

  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (status) => {
      if (status === 0) {
        resolve({ stdout, stderr });
        return;
      }
      process.stdout.write(stdout);
      process.stderr.write(stderr);
      reject(new Error(`${command} exited with status ${status}`));
    });
  });
}

function derLen(length) {
  if (length < 0x80) return Buffer.from([length]);
  const bytes = [];
  let n = length;
  while (n > 0) {
    bytes.unshift(n & 0xff);
    n >>= 8;
  }
  return Buffer.from([0x80 | bytes.length, ...bytes]);
}

function derTag(tag, content) {
  return Buffer.concat([Buffer.from([tag]), derLen(content.length), content]);
}

function derSeq(...items) {
  return derTag(0x30, Buffer.concat(items));
}

function derSet(...items) {
  return derTag(0x31, Buffer.concat(items));
}

function derOid(bytes) {
  return derTag(0x06, Buffer.from(bytes));
}

function derInt(bytes) {
  const buf = Buffer.from(bytes);
  return derTag(0x02, buf[0] & 0x80 ? Buffer.concat([Buffer.from([0]), buf]) : buf);
}

function derBitString(bytes) {
  return derTag(0x03, Buffer.concat([Buffer.from([0x00]), bytes]));
}

function derUtcTime(date) {
  const pad = (n) => String(n).padStart(2, "0");
  const s =
    String(date.getUTCFullYear()).slice(-2) +
    pad(date.getUTCMonth() + 1) +
    pad(date.getUTCDate()) +
    pad(date.getUTCHours()) +
    pad(date.getUTCMinutes()) +
    pad(date.getUTCSeconds()) +
    "Z";
  return derTag(0x17, Buffer.from(s, "ascii"));
}

async function createLoopbackCertificate(certDir) {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const spki = publicKey.export({ type: "spki", format: "der" });
  const sigAlg = derSeq(derOid([0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]));
  const name = derSeq(
    derSet(derSeq(derOid([0x55, 0x04, 0x03]), derTag(0x0c, Buffer.from("127.0.0.1", "utf8")))),
  );
  const now = new Date();
  const later = new Date(now.getTime() + 24 * 60 * 60 * 1000);
  const basicConstraints = derSeq(
    derOid([0x55, 0x1d, 0x13]),
    Buffer.from([0x01, 0x01, 0xff]),
    derTag(0x04, derSeq(Buffer.from([0x01, 0x01, 0xff]))),
  );
  const subjectAltName = derSeq(
    derOid([0x55, 0x1d, 0x11]),
    derTag(0x04, derSeq(derTag(0x87, Buffer.from([127, 0, 0, 1])))),
  );
  const tbs = derSeq(
    derTag(0xa0, derInt([0x02])),
    derInt(crypto.randomBytes(8)),
    sigAlg,
    name,
    derSeq(derUtcTime(now), derUtcTime(later)),
    name,
    spki,
    derTag(0xa3, derSeq(basicConstraints, subjectAltName)),
  );
  const sig = crypto.sign("sha256", tbs, privateKey);
  const certDer = derSeq(tbs, sigAlg, derBitString(sig));
  const b64 = certDer.toString("base64").match(/.{1,64}/g).join("\n");
  const certPem = `-----BEGIN CERTIFICATE-----\n${b64}\n-----END CERTIFICATE-----\n`;
  const keyPem = privateKey.export({ type: "pkcs8", format: "pem" });
  const caPath = path.join(certDir, "smoke-ca.pem");
  await fsp.writeFile(caPath, certPem, "utf8");
  return { cert: certPem, key: keyPem, caPath };
}

async function serveDirectory(root, certDir) {
  const tls = await createLoopbackCertificate(certDir);
  const server = https.createServer({ key: tls.key, cert: tls.cert }, async (request, response) => {
    try {
      const requestUrl = new URL(request.url || "/", "https://127.0.0.1");
      const decodedPath = decodeURIComponent(requestUrl.pathname);
      const filePath = path.resolve(root, `.${decodedPath}`);
      const relative = path.relative(root, filePath);
      if (relative.startsWith("..") || path.isAbsolute(relative)) {
        response.writeHead(403);
        response.end("forbidden");
        return;
      }

      const fileStat = await fsp.stat(filePath);
      if (!fileStat.isFile()) {
        response.writeHead(404);
        response.end("not found");
        return;
      }

      response.writeHead(200, {
        "Content-Length": fileStat.size,
        "Content-Type": "application/octet-stream",
      });
      fs.createReadStream(filePath).pipe(response);
    } catch (error) {
      response.writeHead(error && error.code === "ENOENT" ? 404 : 500);
      response.end(error && error.message ? error.message : "not found");
    }
  });

  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolve({
        baseUrl: `https://127.0.0.1:${address.port}/`,
        caPath: tls.caPath,
        server,
      });
    });
  });
}

function parsePackJson(stdout) {
  const trimmed = stdout.trim();
  if (!trimmed) {
    throw new Error("npm pack did not return package metadata");
  }
  const parsed = JSON.parse(trimmed);
  const first = Array.isArray(parsed) ? parsed[0] : parsed;
  if (!first || !first.filename) {
    throw new Error(`npm pack metadata did not include a filename: ${trimmed}`);
  }
  return first.filename;
}

async function removeSmokeWorkspace(tempRoot, remove = fsp.rm) {
  await remove(tempRoot, {
    force: true,
    recursive: true,
    // Windows can report ENOTEMPTY briefly while completed binary downloads
    // are still being released by the filesystem. fs.rm retries ENOTEMPTY
    // (and the other documented transient removal errors) with linear
    // backoff; the finite cap still surfaces a persistent cleanup failure.
    maxRetries: CLEANUP_MAX_RETRIES,
    retryDelay: CLEANUP_RETRY_DELAY_MS,
  });
}

async function main() {
  const tempRoot = await fsp.mkdtemp(path.join(os.tmpdir(), "codewhale-npm-smoke-"));
  const suppliedAssetsDir = String(
    process.env.CODEWHALE_SMOKE_ASSETS_DIR || "",
  ).trim();
  const releaseAssetsDir = suppliedAssetsDir
    ? path.resolve(suppliedAssetsDir)
    : path.join(tempRoot, "release-assets");
  const packDir = path.join(tempRoot, "pack");
  const installDir = path.join(tempRoot, "install");
  let keepTemp =
    process.env.CODEWHALE_KEEP_SMOKE_DIR === "1" ||
    process.env.DEEPSEEK_TUI_KEEP_SMOKE_DIR === "1";
  let server;

  try {
    await fsp.mkdir(packDir, { recursive: true });
    await fsp.mkdir(installDir, { recursive: true });

    if (suppliedAssetsDir) {
      const assetDirectoryStat = await fsp.stat(releaseAssetsDir);
      if (!assetDirectoryStat.isDirectory()) {
        throw new Error(`CODEWHALE_SMOKE_ASSETS_DIR is not a directory: ${releaseAssetsDir}`);
      }
      console.log(`Using preassembled release assets from ${releaseAssetsDir}`);
    } else {
      const prepareArgs = [prepareAssetsScript, releaseAssetsDir];
      const cargoTargetDir = String(process.env.CARGO_TARGET_DIR || "").trim();
      if (cargoTargetDir) {
        prepareArgs.push(
          path.join(path.resolve(repoRoot, cargoTargetDir), "release"),
        );
      }
      await runCommand(process.execPath, prepareArgs);
    }
    const served = await serveDirectory(releaseAssetsDir, tempRoot);
    server = served.server;

    const env = {
      CODEWHALE_FORCE_DOWNLOAD: "1",
      CODEWHALE_RELEASE_BASE_URL: served.baseUrl,
      NODE_EXTRA_CA_CERTS: served.caPath,
    };
    const pack = await runCommand(
      "npm",
      ["pack", "--json", "--pack-destination", packDir],
      {
        capture: true,
        cwd: packageDir,
        env,
      },
    );
    const tarball = path.join(packDir, parsePackJson(pack.stdout));

    await runCommand("npm", ["init", "-y"], { cwd: installDir });
    await runCommand("npm", ["install", tarball], { cwd: installDir, env });
    await runCommand("npx", ["--no-install", "codewhale", "doctor", "--help"], {
      cwd: installDir,
      env,
    });
    await runCommand("npx", ["--no-install", "codew", "--version"], {
      cwd: installDir,
      env,
    });

    console.log(`npm wrapper smoke passed with local assets from ${served.baseUrl}`);
  } catch (error) {
    keepTemp = true;
    console.error(`npm wrapper smoke failed: ${error.message}`);
    console.error(`Smoke workspace retained at ${tempRoot}`);
    process.exitCode = 1;
  } finally {
    if (server) {
      await new Promise((resolve) => server.close(resolve));
    }
    if (!keepTemp) {
      await removeSmokeWorkspace(tempRoot);
    }
  }
}

if (require.main === module) {
  main();
}

module.exports = {
  CLEANUP_MAX_RETRIES,
  CLEANUP_RETRY_DELAY_MS,
  removeSmokeWorkspace,
  serveDirectory,
};
