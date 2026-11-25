import data from "../data/desktop-published-release.json";

export interface DesktopAssetFact {
  name: string;
  platform: "macos";
  size: number;
  sha256: string;
  signed: true;
  notarized: true;
  url: string;
}

export interface DesktopReleaseFact {
  tag: string;
  version: string;
  publishedAt: string;
  url: string;
  readAt: string;
  assets: DesktopAssetFact[];
}

export type DesktopArch = "arm64" | "x64";

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

function isDesktopAsset(value: unknown): value is DesktopAssetFact {
  return (
    isRecord(value) &&
    typeof value.name === "string" &&
    value.name.length > 0 &&
    value.platform === "macos" &&
    typeof value.size === "number" &&
    Number.isSafeInteger(value.size) &&
    value.size > 0 &&
    typeof value.sha256 === "string" &&
    /^[0-9a-f]{64}$/.test(value.sha256) &&
    value.signed === true &&
    value.notarized === true &&
    isDownloadUrl(value.url)
  );
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
