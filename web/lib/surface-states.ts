import { DESKTOP_RELEASE, desktopDownload, type DesktopReleaseFact } from "./desktop-release";
import { FOUNDER_SWITCH_VALUES, type FounderSwitchValues } from "./founder-switches";
import { getChrome } from "./i18n/dictionaries";
import type { ChromeDict } from "./i18n/dictionaries/types";

export const SURFACE_STATES = ["released", "available", "beta", "dev", "open", "invite_only", "not_open", "not_public", "in_source"] as const;
export type SurfaceState = (typeof SURFACE_STATES)[number];

export const SURFACE_IDS = [
  "terminal",
  "local-browser",
  "vscode",
  "desktop",
  "hosted-web",
  "cloud-computers",
  "accounts",
  "phone",
  "computer-use",
] as const;
export type SurfaceId = (typeof SURFACE_IDS)[number];

export type SurfaceTone = "ready" | "attention" | "idle";

export const STATE_CHROME_KEY = {
  released: "stateReleased",
  available: "stateAvailable",
  beta: "stateBeta",
  dev: "stateDev",
  open: "stateOpen",
  invite_only: "stateInviteOnly",
  not_open: "stateNotOpen",
  not_public: "stateNotPublic",
  in_source: "stateInSource",
} as const satisfies Record<SurfaceState, keyof ChromeDict>;

export const STATE_TONE: Record<SurfaceState, SurfaceTone> = {
  released: "ready",
  available: "ready",
  open: "ready",
  beta: "attention",
  dev: "attention",
  invite_only: "idle",
  not_open: "idle",
  not_public: "idle",
  in_source: "idle",
};

const COMMITTED_STATE: Record<Exclude<SurfaceId, "desktop">, SurfaceState> = {
  terminal: "released",
  "local-browser": "released",
  vscode: "available",
  "hosted-web": "not_open",
  "cloud-computers": "invite_only",
  accounts: "open",
  phone: "not_public",
  "computer-use": "beta",
};

export interface SurfaceContext {
  desktopRelease?: DesktopReleaseFact | null;
  switches?: FounderSwitchValues;
}

export function surfaceState(id: SurfaceId, context: SurfaceContext = {}): SurfaceState {
  if (id === "desktop") {
    return desktopDownload(context.desktopRelease === undefined ? DESKTOP_RELEASE : context.desktopRelease) ? "released" : "dev";
  }
  return COMMITTED_STATE[id];
}

export function stateLabel(state: SurfaceState, locale: string): string {
  return getChrome(locale)[STATE_CHROME_KEY[state]];
}

export interface SurfaceCapabilities {
  link: boolean;
  download: boolean;
  requestAccess: boolean;
}

export function surfaceCapabilities(id: SurfaceId, context: SurfaceContext = {}): SurfaceCapabilities {
  const state = surfaceState(id, context);
  const switches = context.switches ?? FOUNDER_SWITCH_VALUES;
  return {
    link: state !== "not_open" && state !== "not_public",
    download: id === "desktop" && state === "released",
    requestAccess: id === "cloud-computers" && state === "invite_only" && switches.cloudInvitationsOpen,
  };
}
