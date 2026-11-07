/**
 * The v2 whale performance, loaded in the browser only. The vendored files
 * (vendor/whale-character-v2, see its PROVENANCE.md) attach `WhaleActing`
 * and `WhaleHabitat` to window in dependency order — the same executable
 * reference the desktop app and its web client draw, so the site never
 * keeps a second motion implementation.
 */
import type { WhaleStateKey } from "./content/whale-states";

export type WhalePresence = "Offline" | "Idle" | "Listening" | "Thinking" | "Working" | "NeedsYou" | "Done";

export type WhaleActivity = { kind: string; observed: boolean; parallel?: number };
export type WhaleContext = { freshness?: "Live" | "Stale"; turnId?: string; status?: "completed" };

export type WhaleDirector = {
  f: number;
  reduced: boolean;
  acting: string;
  set(presence: WhalePresence, activity?: WhaleActivity | null, context?: WhaleContext): void;
  setReduced(value: boolean): void;
  step(dt: number): void;
};

type View = { theme: "paper" | "charcoal"; size: number; dpr: number; lod: number; pose?: Record<string, number> };

type RigShape = { id: string; role: string; opacity: number; path: unknown };

export type WhaleRig = {
  build(direction: "mark" | "cruise" | "open", pose?: Record<string, number>): { shapes: RigShape[] };
  holesFor(id: string, shapes: RigShape[], mono?: boolean): unknown[];
  pathString(path: unknown): string;
};

export type WhaleReference = {
  rig: WhaleRig;
  acting: {
    Director: new (options?: { seed?: number; reduced?: boolean }) => WhaleDirector;
    render(ctx: CanvasRenderingContext2D, director: WhaleDirector, view: View): void;
  };
  habitat: {
    pose(director: WhaleDirector): Record<string, number>;
    observe(x: number, y: number): void;
    leave(): void;
  };
};

let reference: Promise<WhaleReference> | null = null;

export function loadWhaleReference(): Promise<WhaleReference> {
  if (typeof window === "undefined") return Promise.reject(new Error("The whale renderer needs a browser."));
  if (!reference) reference = (async () => {
    // @ts-expect-error The vendor reference has no TypeScript declarations.
    await import("../vendor/whale-character-v2/mark-data.js");
    // @ts-expect-error The vendor reference has no TypeScript declarations.
    await import("../vendor/whale-character-v2/rig.js");
    // @ts-expect-error The vendor reference has no TypeScript declarations.
    await import("../vendor/whale-character-v2/props.js");
    // @ts-expect-error The vendor reference has no TypeScript declarations.
    await import("../vendor/whale-character-v2/acting.js");
    // @ts-expect-error The vendor reference has no TypeScript declarations.
    await import("../vendor/whale-character-v2/habitat.js");
    const globals = window as unknown as { WhaleActing?: WhaleReference["acting"]; WhaleHabitat?: WhaleReference["habitat"]; WhaleRig?: WhaleRig };
    if (!globals.WhaleActing?.Director || !globals.WhaleHabitat?.pose || !globals.WhaleRig?.build) throw new Error("The whale motion reference did not load.");
    return { rig: globals.WhaleRig, acting: globals.WhaleActing, habitat: globals.WhaleHabitat };
  })();
  return reference;
}

/**
 * What the homepage whale is acting out. The terminal gallery sets it when a
 * reader picks a view, so the character mirrors the screen beside it. This is
 * illustration, not telemetry: no session is being observed.
 */
export type WhaleCue = {
  presence: WhalePresence;
  kind?: string;
  parallel?: number;
  /** The word shown under the whale (a WHALE_STATE_TEXT key). */
  label: WhaleStateKey;
  /** A reader picked this (a terminal view); the autoplay yields to it. */
  chosen?: boolean;
};

const listeners = new Set<(cue: WhaleCue) => void>();
let current: WhaleCue = { presence: "Idle", label: "rest" };

export function cueWhale(cue: WhaleCue): void {
  current = cue;
  listeners.forEach((listener) => listener(cue));
}

export function onWhaleCue(listener: (cue: WhaleCue) => void): () => void {
  listeners.add(listener);
  listener(current);
  return () => { listeners.delete(listener); };
}

/**
 * The session the hero whale acts out on its own: a request arrives, it
 * plans, reads, edits, runs the tests, finishes, and rests. Seconds per step.
 */
export const WHALE_PERFORMANCE: { cue: WhaleCue; seconds: number }[] = [
  { cue: { presence: "Idle", label: "rest" }, seconds: 2.5 },
  { cue: { presence: "Listening", label: "listen" }, seconds: 2.8 },
  { cue: { presence: "Thinking", label: "think" }, seconds: 2.6 },
  { cue: { presence: "Working", kind: "reading", label: "read" }, seconds: 3.2 },
  { cue: { presence: "Working", kind: "editing", label: "write" }, seconds: 3.4 },
  { cue: { presence: "Working", kind: "testing", label: "run" }, seconds: 3.2 },
  { cue: { presence: "Done", label: "done" }, seconds: 3.6 },
];
