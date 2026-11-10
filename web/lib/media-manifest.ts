/**
 * media-manifest.ts — the real-session media surface for codewhale.net.
 *
 * Every real-session asset the site may show is declared here, with its
 * poster, captions, transcript, GIF fallback, and budgets. The manifest is
 * the contract: `web/components/session-media.tsx` renders whatever is
 * declared, and `web/lib/media-manifest.test.ts` enforces the rules below.
 *
 * THE HONESTY CONTRACT:
 *   - A `pending` entry declares intent only. It has NO asset fields, and no
 *     files may exist for it under `web/public/media/`. The component renders
 *     a visible "recording pending release candidate" state — never a mock,
 *     staged, or recycled clip. (Release issue #4906: the dogfood recording
 *     happens after the v0.9.2 candidate is stable.)
 *   - A `published` entry is complete or it does not ship: poster, video,
 *     per-locale captions (WebVTT), a transcript, and a GIF fallback are all
 *     required. Tests verify presence and byte budgets, inspect PNG poster
 *     dimensions, and compare declared video metadata with MEDIA_BUDGETS.
 *     Actual video duration/dimensions remain a recording-checklist gate.
 *   - Reduced motion is structural, not a media query patch: the video never
 *     autoplays (`preload="none"`, user-initiated only), the poster is the
 *     static default, and the GIF fallback link is always visible.
 *
 * The exact post-dogfood recording procedure lives in
 * docs/releases/v0.9.2-media-plan.md. Flip an entry to `published` only by
 * following that checklist.
 */

import type { LocalizedText } from "./content/vocabulary";

/**
 * The homepage terminal. The site renders it as live text from the PTY cell
 * capture (`capture`, via scripts/render-terminal-capture.mjs and
 * components/terminal-capture.tsx); `src` is the same frame rasterized by
 * that script for the README, which cannot run the site's renderer. Captured
 * build identity is independent of the current source/release. This local
 * website-demo session exercises real Engine tools; it does not qualify an
 * inference provider. sourceCommit is the candidate's base; exact binary and
 * dirty-source hashes are recorded in terminal-captures/manifest.json.
 */
export const TERMINAL_SCREENSHOT = {
  capture: "web/lib/terminal-captures/website-home-100x24.json",
  src: "/codewhale-tui-0102-demo.png",
  width: 1872,
  height: 956,
  version: "0.10.3",
  sourceCommit: "e5279420647e15896150728ecf9a1f9fa80aadd1",
  sha256: "89e30c3e2f31c0c189d8162bdc253f8e36b4bbf4ffc1f56271f88f8a48eff8cb",
} as const;

/** Published-asset budgets; see the module contract for what tests inspect. */
export const MEDIA_BUDGETS = {
  poster: { width: 1280, height: 720, maxBytes: 500_000 },
  video: { width: 1280, height: 720, maxBytes: 10_000_000, maxDurationSeconds: 120 },
  gifFallback: { maxBytes: 6_000_000 },
  /** WebVTT caption tracks must exist per shipped locale and be non-empty. */
  captionLocales: ["en", "zh"],
} as const;

/**
 * The reduced-motion policy identifier, referenced by the component and
 * asserted by the test: static poster, no autoplay, optional GIF link.
 */
export const REDUCED_MOTION_POLICY = "static-poster-no-autoplay" as const;

/** Directory under web/public/ that holds published session media. */
export const MEDIA_PUBLIC_DIR = "media";

export type MediaStatus = "pending" | "published";

export interface MediaPoster {
  /** Path relative to web/public/ (e.g. "media/first-fleet-session.png"). */
  src: string;
  width: number;
  height: number;
  alt: LocalizedText;
}

export interface MediaVideo {
  src: string;
  /** Measured duration of the shipped file, seconds. */
  durationSeconds: number;
  width: number;
  height: number;
}

export interface MediaCaptionsTrack {
  /** Path relative to web/public/ (WebVTT). */
  src: string;
  srclang: string;
  label: string;
}

export interface MediaAsset {
  /** Stable identifier; also the file stem for every asset file. */
  id: string;
  title: LocalizedText;
  description: LocalizedText;
  status: MediaStatus;
  /** Shown in place of any imagery while status is "pending". */
  pendingLabel: LocalizedText;
  /** Published-only fields; absent while pending (enforced by the test). */
  poster?: MediaPoster;
  video?: MediaVideo;
  captions?: MediaCaptionsTrack[];
  gifFallback?: { src: string };
  /** Repo-relative transcript document (e.g. "docs/evidence/..."). */
  transcript?: string;
}

export const MEDIA_ASSETS: MediaAsset[] = [
  {
    id: "first-fleet-session",
    title: {
      en: "A real Codewhale session, end to end",
      zh: "一次真实的 Codewhale 端到端会话",
    },
    description: {
      en: "Install, a first session with no key, connecting a provider, and one fleet workflow on a local model. To be recorded from a release build.",
      zh: "安装、无密钥的首次会话、接入提供商，以及在本地模型上跑一次 fleet workflow。将从发布版本录制。",
    },
    status: "pending",
    pendingLabel: {
      en: "Recording pending release candidate",
      zh: "待发布候选版录制",
    },
  },
];

export function getMediaAsset(id: string): MediaAsset | undefined {
  return MEDIA_ASSETS.find((asset) => asset.id === id);
}
