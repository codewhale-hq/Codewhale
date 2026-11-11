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

export interface DesktopDownload {
  version: string;
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

export function desktopDownload(release: DesktopReleaseFact | null = DESKTOP_RELEASE): DesktopDownload | null {
  const asset = release?.assets.find((candidate) => candidate.platform === "macos");
  if (!release || !asset) return null;
  return { version: release.version, name: asset.name, size: asset.size, sha256: asset.sha256, url: asset.url };
}
