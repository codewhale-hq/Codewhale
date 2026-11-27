import data from "../data/desktop-published-release.json";

export interface MacosAssetFact {
  name: string;
  platform: "macos";
  size: number;
  sha256: string;
  signed: true;
  notarized: true;
  url: string;
}

export interface AptSource {
  repository: string;
  keyUrl: string;
  suite: string;
  component: string;
  architecture: AptArchitecture;
  package: string;
  keyFingerprint: string;
}

export const APT_ARCHITECTURES = ["amd64", "arm64"] as const;
export type AptArchitecture = (typeof APT_ARCHITECTURES)[number];

export interface LinuxDebAssetFact {
  name: string;
  platform: "linux";
  kind: "deb";
  arch: "x64";
  size: number;
  sha256: string;
  url: string;
  apt: AptSource;
}

export interface LinuxAppImageAssetFact {
  name: string;
  platform: "linux";
  kind: "appimage";
  arch: "x64";
  size: number;
  sha256: string;
  url: string;
}

export type LinuxAssetFact = LinuxDebAssetFact | LinuxAppImageAssetFact;
export type DesktopAssetFact = MacosAssetFact | LinuxAssetFact;

export interface DesktopReleaseFact {
  tag: string;
  version: string;
  publishedAt: string;
  url: string;
  readAt: string;
  assets: DesktopAssetFact[];
}

export type DesktopArch = "arm64" | "x64";

export interface LinuxDownloads {
  version: string;
  apt: AptSource | null;
  appImage: { name: string; size: number; sha256: string; url: string } | null;
}

export interface DesktopDownload {
  version: string;
  arch: DesktopArch | null;
  name: string;
  size: number;
  sha256: string;
  url: string;
}

const DOWNLOAD_ORIGINS = ["https://github.com/codewhale-hq/", "https://downloads.codewhale.net/"];

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isDate(value: unknown): value is string {
  return typeof value === "string" && Number.isFinite(Date.parse(value));
}

function isDownloadUrl(value: unknown): value is string {
  return typeof value === "string" && DOWNLOAD_ORIGINS.some((origin) => value.startsWith(origin));
}

const SAFE_URL = /^https:\/\/[A-Za-z0-9.-]+(?::\d+)?(?:\/[A-Za-z0-9._~%+\-/]*)?$/;
const APT_TOKEN = /^[a-z0-9][a-z0-9+.-]*$/;
const KEY_FILE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const KEY_FINGERPRINT = /^[0-9A-F]{40}$/;
const REPOSITORY_ORIGIN = "https://downloads.codewhale.net/";

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && keys.every((key) => actual.includes(key));
}

function hasFileFacts(value: Record<string, unknown>): boolean {
  return (
    typeof value.name === "string" &&
    value.name.length > 0 &&
    typeof value.size === "number" &&
    Number.isSafeInteger(value.size) &&
    value.size > 0 &&
    typeof value.sha256 === "string" &&
    /^[0-9a-f]{64}$/.test(value.sha256) &&
    isDownloadUrl(value.url)
  );
}

export function aptKeyFileName(keyUrl: string): string | null {
  const name = keyUrl.slice(keyUrl.lastIndexOf("/") + 1);
  return KEY_FILE_NAME.test(name) ? name : null;
}

function isAptSource(value: unknown): value is AptSource {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["repository", "keyUrl", "suite", "component", "architecture", "package", "keyFingerprint"]) &&
    typeof value.repository === "string" &&
    value.repository.startsWith(REPOSITORY_ORIGIN) &&
    SAFE_URL.test(value.repository) &&
    typeof value.keyUrl === "string" &&
    SAFE_URL.test(value.keyUrl) &&
    aptKeyFileName(value.keyUrl) !== null &&
    typeof value.suite === "string" &&
    APT_TOKEN.test(value.suite) &&
    typeof value.component === "string" &&
    APT_TOKEN.test(value.component) &&
    typeof value.architecture === "string" &&
    (APT_ARCHITECTURES as readonly string[]).includes(value.architecture) &&
    typeof value.package === "string" &&
    APT_TOKEN.test(value.package) &&
    typeof value.keyFingerprint === "string" &&
    KEY_FINGERPRINT.test(value.keyFingerprint)
  );
}

function isMacosAsset(value: Record<string, unknown>): boolean {
  return value.platform === "macos" && hasFileFacts(value) && value.signed === true && value.notarized === true;
}

function isLinuxAsset(value: Record<string, unknown>): boolean {
  if (value.platform !== "linux" || value.arch !== "x64" || !hasFileFacts(value)) return false;
  const base = ["name", "platform", "kind", "arch", "size", "sha256", "url"];
  if (value.kind === "deb") return hasExactKeys(value, [...base, "apt"]) && isAptSource(value.apt);
  if (value.kind === "appimage") return hasExactKeys(value, base);
  return false;
}

function isDesktopAsset(value: unknown): value is DesktopAssetFact {
  return isRecord(value) && (isMacosAsset(value) || isLinuxAsset(value));
}

export function parseDesktopRelease(value: unknown): DesktopReleaseFact | null {
  if (!isRecord(value)) return null;
  if (
    typeof value.tag !== "string" ||
    !/^v\d+\.\d+\.\d+$/.test(value.tag) ||
    value.version !== value.tag.slice(1) ||
    !isDate(value.publishedAt) ||
    !isDate(value.readAt) ||
    !isDownloadUrl(value.url) ||
    !Array.isArray(value.assets) ||
    !value.assets.every(isDesktopAsset)
  ) {
    return null;
  }
  return value as unknown as DesktopReleaseFact;
}

export const DESKTOP_RELEASE: DesktopReleaseFact | null = parseDesktopRelease(data);

function assetArch(name: string): DesktopArch | null {
  if (/[-_.]arm64[-_.]/.test(name)) return "arm64";
  if (/[-_.]x64[-_.]/.test(name)) return "x64";
  return null;
}

export function desktopDownloads(release: DesktopReleaseFact | null = DESKTOP_RELEASE): DesktopDownload[] {
  if (!release) return [];
  const seen = new Set<DesktopArch | null>();
  const downloads: DesktopDownload[] = [];
  for (const asset of release.assets) {
    const arch = assetArch(asset.name);
    if (asset.platform !== "macos" || seen.has(arch)) continue;
    seen.add(arch);
    downloads.push({ version: release.version, arch, name: asset.name, size: asset.size, sha256: asset.sha256, url: asset.url });
  }
  return downloads.sort((a, b) => Number(a.arch === "x64") - Number(b.arch === "x64"));
}

export function desktopDownload(release: DesktopReleaseFact | null = DESKTOP_RELEASE): DesktopDownload | null {
  return desktopDownloads(release)[0] ?? null;
}

export function linuxDownloads(release: DesktopReleaseFact | null = DESKTOP_RELEASE): LinuxDownloads | null {
  if (!release) return null;
  let apt: AptSource | null = null;
  let appImage: LinuxDownloads["appImage"] = null;
  for (const asset of release.assets) {
    if (asset.platform !== "linux") continue;
    if (asset.kind === "deb") apt ??= asset.apt;
    else appImage ??= { name: asset.name, size: asset.size, sha256: asset.sha256, url: asset.url };
  }
  return apt || appImage ? { version: release.version, apt, appImage } : null;
}
