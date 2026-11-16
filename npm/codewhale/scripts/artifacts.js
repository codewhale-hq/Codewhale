const path = require("path");
const os = require("os");
const compiledHosts = require("./compiled-hosts");

const CHECKSUM_MANIFEST = "codewhale-artifacts-sha256.txt";
const CHECKSUM_SIGNATURE = `${CHECKSUM_MANIFEST}.sig`;
const BUNDLE_CHECKSUM_MANIFEST = "codewhale-bundles-sha256.txt";
const WINDOWS_INSTALLER_ASSET = "CodeWhaleSetup.exe";
const RELEASE_GITHUB_HOSTS = new Set([
  "github.com",
  "api.github.com",
  "objects.githubusercontent.com",
  "release-assets.githubusercontent.com",
  "github-releases.githubusercontent.com",
]);
const RELEASE_CNB_HOST = "cnb.cool";
const UPDATE_ALLOWED_HOSTS_ENV = "CODEWHALE_UPDATE_ALLOWED_HOSTS";
const transportOptions = {
  allowLoopbackHttp: false,
};

// Compatibility bridge introduced in v0.9.5: already-shipped v0.9.4 clients
// require these names before
// they will advertise or install a newer release. They contain byte-identical
// copies of the consolidated `codewhale` runtime; current installers do not
// expose them as a third command.
const LEGACY_TUI_BRIDGE_ASSET_NAMES = [
  "codewhale-tui-linux-x64",
  "codewhale-tui-linux-arm64",
  "codewhale-tui-android-arm64",
  // Published for symmetry with every other platform: the name exists so
  // release completeness stays one rule, not a case analysis. No shipped client
  // requests it; nothing else references it.
  "codewhale-tui-ohos-arm64",
  "codewhale-tui-macos-x64",
  "codewhale-tui-macos-arm64",
  "codewhale-tui-windows-x64.exe",
  "codewhale-tui-windows-arm64.exe",
];

const CNB_BINARY_ASSET_NAMES = [
  "codewhale-linux-x64",
  "codew-linux-x64",
  "codewhale-tui-linux-x64",
];
const CNB_RELEASE_ASSET_NAMES = [
  ...CNB_BINARY_ASSET_NAMES,
  CHECKSUM_MANIFEST,
];

const BUNDLE_ASSET_NAMES = [
  "codewhale-linux-x64.tar.gz",
  "codewhale-linux-arm64.tar.gz",
  "codewhale-android-arm64.tar.gz",
  "codewhale-ohos-arm64.tar.gz",
  "codewhale-macos-x64.tar.gz",
  "codewhale-macos-arm64.tar.gz",
  "codewhale-windows-x64.zip",
  "codewhale-windows-x64-portable.zip",
  "codewhale-windows-arm64.zip",
  "codewhale-windows-arm64-portable.zip",
];

const ASSET_MATRIX = {
  linux: {
    x64: ["codewhale-linux-x64", "codew-linux-x64"],
    arm64: ["codewhale-linux-arm64", "codew-linux-arm64"],
  },
  android: {
    arm64: ["codewhale-android-arm64", "codew-android-arm64"],
  },
  // Native HarmonyOS/OpenHarmony (aarch64-unknown-linux-ohos). These binaries
  // only run on a device when they are code-signed, so the release build
  // self-signs them; see docs/HarmonyOS.md.
  openharmony: {
    arm64: ["codewhale-ohos-arm64", "codew-ohos-arm64"],
  },
  darwin: {
    x64: ["codewhale-macos-x64", "codew-macos-x64"],
    arm64: ["codewhale-macos-arm64", "codew-macos-arm64"],
  },
  win32: {
    x64: ["codewhale-windows-x64.exe", "codew-windows-x64.exe", "codewhale.bat"],
    arm64: ["codewhale-windows-arm64.exe", "codew-windows-arm64.exe"],
  },
};

function detectBinaryNames() {
  const platform = os.platform();
  const arch = os.arch();
  const defaults = ASSET_MATRIX[platform];
  if (!defaults) {
    const supported = Object.keys(ASSET_MATRIX).map(p => `'${p}'`).join(', ');
    throw new Error(
      `Unsupported platform: ${platform}. Supported platforms: ${supported}.\n\n` +
      unsupportedBuildHint(),
    );
  }
  const pair = defaults[arch];
  if (!pair) {
    const supported = Object.keys(defaults).map(a => `'${a}'`).join(', ');
    const hint = platform === "linux" && arch === "riscv64" ? unsupportedRiscvHint() : unsupportedBuildHint();
    throw new Error(
      `Unsupported architecture: ${arch} on platform ${platform}. ` +
      `Supported architectures: ${supported}.\n\n` +
      hint,
    );
  }
  return {
    platform,
    arch,
    codewhale: pair[0],
    codew: pair[1],
  };
}

function unsupportedBuildHint() {
  return [
    "No prebuilt binary is available for this platform/architecture combo.",
    "You can still run codewhale by building from source with Cargo (single binary):",
    "",
    "  # Requires Rust 1.89+ (https://rustup.rs)",
    "  cargo install codewhale-cli --locked   # provides `codewhale`",
    "",
    "Or build from a checkout:",
    "",
    "  git clone https://github.com/codewhale-hq/CodeWhale.git",
    "  cd CodeWhale",
    "  cargo install --path crates/cli --locked   # single binary",
    "",
    "See https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md",
    "for cross-compilation, mirror, Linux ARM64, FreeBSD, and winget specifics.",
  ].join("\n");
}

function openHarmonyDirections() {
  return [
    "On a HarmonyOS PC, install Codewhale inside the Linux subsystem (融合开发引擎, openEuler):",
    "",
    "  curl -fsSL https://codewhale.net/install.sh | sh",
    "",
    "Or build the native aarch64-unknown-linux-ohos binary from source by following",
    "\"Cross-Compiling To OpenHarmony\" in docs/HarmonyOS.md of codewhale-hq/CodeWhale.",
  ].join("\n");
}

function unsupportedRiscvHint() {
  return [
    "Linux riscv64 prebuilt binaries are temporarily unavailable.",
    "CodeWhale currently depends on rquickjs-sys, which does not ship",
    "riscv64gc-unknown-linux-gnu bindings in the locked dependency set.",
    "",
    "Track the release notes and docs/INSTALL.md for the next RISC-V support update.",
  ].join("\n");
}

function executableName(base, platform) {
  return platform === "win32" ? `${base}.exe` : base;
}

function ensureTrailingSlash(baseUrl) {
  const trimmed = String(baseUrl).trim();
  return trimmed.endsWith("/") ? trimmed : `${trimmed}/`;
}

function githubReleaseBaseUrl(version, repo = "codewhale-hq/CodeWhale") {
  return `https://github.com/${repo}/releases/download/v${version}/`;
}

function cnbReleaseBaseUrl(version) {
  return `https://cnb.cool/codewhale.net/codewhale/-/releases/download/v${version}/`;
}

function releaseAssetUrlFromBase(baseName, baseUrl) {
  return new URL(baseName, ensureTrailingSlash(baseUrl)).toString();
}

function explicitReleaseBase(env = process.env) {
  const candidates = [
    env.CODEWHALE_RELEASE_BASE_URL,
    env.DEEPSEEK_TUI_RELEASE_BASE_URL,
    env.DEEPSEEK_RELEASE_BASE_URL,
  ];
  for (const candidate of candidates) {
    const override = String(candidate || "").trim();
    if (override) {
      return ensureTrailingSlash(override);
    }
  }
  return "";
}

function hasExplicitReleaseBase(env = process.env) {
  return Boolean(explicitReleaseBase(env));
}

function isCnbSupportedTarget(
  platform = os.platform(),
  arch = os.arch(),
) {
  return platform === "linux" && arch === "x64";
}

function releaseBaseUrl(version, repo = "codewhale-hq/CodeWhale") {
  // CODEWHALE_RELEASE_BASE_URL is the canonical override.
  // DEEPSEEK_TUI_RELEASE_BASE_URL / DEEPSEEK_RELEASE_BASE_URL are legacy aliases.
  const override = explicitReleaseBase();
  if (override) {
    return override;
  }
  // When CODEWHALE_USE_CNB_MIRROR is set, use the CNB (China-friendly)
  // mirror that already builds and publishes binary release assets.
  if (usesCnbMirror()) {
    assertCnbMirrorSupportedPlatform();
    return cnbReleaseBaseUrl(version);
  }
  return githubReleaseBaseUrl(version, repo);
}

function usesCnbMirror(env = process.env) {
  return !hasExplicitReleaseBase(env) && env.CODEWHALE_USE_CNB_MIRROR === "1";
}

function shouldRaceFirstPartyMirrors(
  env = process.env,
  platform = os.platform(),
  arch = os.arch(),
) {
  return (
    isCnbSupportedTarget(platform, arch) &&
    !hasExplicitReleaseBase(env) &&
    env.CODEWHALE_USE_CNB_MIRROR !== "1"
  );
}

function firstPartyReleaseSources(version, repo = "codewhale-hq/CodeWhale") {
  return [
    {
      id: "github",
      label: "GitHub Releases",
      baseUrl: githubReleaseBaseUrl(version, repo),
    },
    {
      id: "cnb",
      label: "CNB first-party mirror",
      baseUrl: cnbReleaseBaseUrl(version),
    },
  ];
}

function assertCnbMirrorSupportedPlatform(
  platform = os.platform(),
  arch = os.arch(),
) {
  if (isCnbSupportedTarget(platform, arch)) {
    return;
  }
  throw new Error(
    "CODEWHALE_USE_CNB_MIRROR=1 currently supports only Linux x64; " +
      `detected ${platform} ${arch}. Native HarmonyOS has its own asset and ` +
      "no CNB mirror. " +
      "Use the GitHub Release or set CODEWHALE_RELEASE_BASE_URL to a " +
      "complete mirror for this platform.",
  );
}

function releaseAssetUrl(baseName, version, repo = "codewhale-hq/CodeWhale") {
  return releaseAssetUrlFromBase(baseName, releaseBaseUrl(version, repo));
}

function checksumManifestUrl(version, repo = "codewhale-hq/CodeWhale") {
  return releaseAssetUrl(CHECKSUM_MANIFEST, version, repo);
}

function redactUrl(rawUrl) {
  const input = String(rawUrl || "");
  try {
    const parsed = new URL(input);
    if (parsed.username || parsed.password) {
      parsed.username = "";
      parsed.password = "";
      return parsed.toString();
    }
    return input;
  } catch {
    return input.replace(/^([a-z][a-z0-9+\-.]*:\/\/)[^/@\s]+@/i, "$1");
  }
}

function extraAllowedReleaseHosts(env = process.env) {
  const extra = new Set();
  const override = explicitReleaseBase(env);
  if (override) {
    try {
      const parsed = new URL(override);
      if (parsed.hostname) {
        extra.add(parsed.hostname.toLowerCase());
      }
    } catch {
    }
  }
  const configured = env && env[UPDATE_ALLOWED_HOSTS_ENV];
  if (configured) {
    for (const part of String(configured).split(",")) {
      const host = part.trim().toLowerCase();
      if (host) {
        extra.add(host);
      }
    }
  }
  return extra;
}

function isAllowedReleaseHost(hostname, env = process.env) {
  const host = String(hostname || "").toLowerCase();
  if (!host) {
    return false;
  }
  if (
    RELEASE_GITHUB_HOSTS.has(host) ||
    host === RELEASE_CNB_HOST ||
    host.endsWith(`.${RELEASE_CNB_HOST}`)
  ) {
    return true;
  }
  return extraAllowedReleaseHosts(env).has(host);
}

function isLoopbackHost(hostname) {
  const host = String(hostname || "").toLowerCase();
  return host === "127.0.0.1" || host === "localhost" || host === "::1" || host === "[::1]";
}

function validateReleaseUrl(rawUrl, options = {}) {
  const env = options.env || process.env;
  const allowLoopbackHttp =
    options.allowLoopbackHttp === undefined
      ? transportOptions.allowLoopbackHttp
      : Boolean(options.allowLoopbackHttp);
  const safeUrl = redactUrl(rawUrl);
  let parsed;
  try {
    parsed = new URL(rawUrl);
  } catch (err) {
    throw new Error(`Invalid release URL: ${safeUrl} (${err.message})`);
  }
  if (allowLoopbackHttp && parsed.protocol === "http:" && isLoopbackHost(parsed.hostname)) {
    return parsed;
  }
  if (parsed.protocol !== "https:") {
    throw new Error(`Release download requires HTTPS, not ${parsed.protocol}: ${safeUrl}`);
  }
  if (!isAllowedReleaseHost(parsed.hostname, env)) {
    throw new Error(
      `Release host "${parsed.hostname}" is not an allowed release host. ` +
        "A private mirror's host is allowed when it is the CODEWHALE_RELEASE_BASE_URL HTTPS base URL; " +
        `any other host can be added with ${UPDATE_ALLOWED_HOSTS_ENV}=host1,host2`,
    );
  }
  return parsed;
}

function releaseBinaryDirectory() {
  return path.join(__dirname, "..", "bin", "downloads");
}

function allAssetNames() {
  const names = [];
  for (const platformAssets of Object.values(ASSET_MATRIX)) {
    for (const assets of Object.values(platformAssets)) {
      names.push(...assets);
    }
  }
  return Array.from(new Set(names));
}

function allReleaseAssetNames(catalog) {
  return [
    ...allAssetNames(),
    ...LEGACY_TUI_BRIDGE_ASSET_NAMES,
    ...BUNDLE_ASSET_NAMES,
    WINDOWS_INSTALLER_ASSET,
    BUNDLE_CHECKSUM_MANIFEST,
    CHECKSUM_MANIFEST,
    ...compiledHosts.assets(catalog),
  ];
}

function checksummedReleaseAssetNames(catalog) {
  return allReleaseAssetNames(catalog).filter((name) => name !== CHECKSUM_MANIFEST);
}

module.exports = {
  allAssetNames,
  allReleaseAssetNames,
  assertCnbMirrorSupportedPlatform,
  BUNDLE_ASSET_NAMES,
  BUNDLE_CHECKSUM_MANIFEST,
  CHECKSUM_MANIFEST,
  CHECKSUM_SIGNATURE,
  checksummedReleaseAssetNames,
  CNB_BINARY_ASSET_NAMES,
  CNB_RELEASE_ASSET_NAMES,
  checksumManifestUrl,
  cnbReleaseBaseUrl,
  detectBinaryNames,
  executableName,
  explicitReleaseBase,
  firstPartyReleaseSources,
  githubReleaseBaseUrl,
  hasExplicitReleaseBase,
  isAllowedReleaseHost,
  isCnbSupportedTarget,
  LEGACY_TUI_BRIDGE_ASSET_NAMES,
  openHarmonyDirections,
  redactUrl,
  releaseAssetUrl,
  releaseAssetUrlFromBase,
  releaseBaseUrl,
  releaseBinaryDirectory,
  shouldRaceFirstPartyMirrors,
  transportOptions,
  UPDATE_ALLOWED_HOSTS_ENV,
  usesCnbMirror,
  validateReleaseUrl,
  WINDOWS_INSTALLER_ASSET,
};
