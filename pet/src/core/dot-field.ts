import { stableHash } from './model.js';
import { DOT_BODY } from './dot-body.js';
import { DOT_PIXEL_BODY } from './dot-pixel-body.js';
import { FORM_HUE, FORM_SCALE, FORM_STILL_TIME, formPoint, type FormKind } from './dot-forms.js';
import type { EngineOwnerProjection } from './pet-engine.js';

export type BodyStyle = 'smooth' | 'pixel';
export interface DotOptions {
  bodyStyle?: BodyStyle;
}
export interface DotField {
  points: number[][];
  materials: number[][];
  morph: number;
  bodyStyle: BodyStyle;
}
const clamp = (n: number, lo = 0, hi = 1) => Math.max(lo, Math.min(hi, n));
const ease = (n: number) => {
  const u = clamp(n);
  return u * u * (3 - 2 * u);
};

function form(a: EngineOwnerProjection, timeMs: number): FormKind | null {
  if (a.freshness !== 'fresh' || !a.observed) return null;
  if (a.authoritativePresence === 'needs_you') return 'waiting';
  if (a.authoritativePresence === 'done') return timeMs - (a.observedAtMs ?? timeMs) < 2600 ? 'done' : null;
  if (a.authoritativePresence === 'idle' || !a.activityKind) return null;
  return a.activityKind === 'tool' ? 'executing' : a.activityKind;
}

export function dotFieldKey(a: EngineOwnerProjection): string {
  return [
    a.freshness,
    a.authoritativePresence,
    a.activityKind,
    a.actionId,
    a.parallelAgentCount,
    a.doneEffectId,
    a.activeSpans.length === 0 && !!a.failedToolAge,
  ].join(':');
}

function pigment(hue: number, light: number, satScale = 0.72): number[] {
  const h = (((hue % 360) + 360) % 360) / 60;
  const c = (1 - Math.abs(2 * light - 1)) * satScale;
  const x = c * (1 - Math.abs((h % 2) - 1));
  const m = light - c / 2;
  const rgb =
    h < 1 ? [c, x, 0] : h < 2 ? [x, c, 0] : h < 3 ? [0, c, x] : h < 4 ? [0, x, c] : h < 5 ? [x, 0, c] : [c, 0, x];
  return rgb.map((v) => Math.round(clamp(v + m, 0, 1) * 255));
}

export function projectDotField(
  activity: EngineOwnerProjection,
  timeMs: number,
  still = false,
  expressionAtMs = 0,
  options: DotOptions = {},
): DotField {
  const kind = form(activity, timeMs);
  const identity = activity.actionId || activity.activityKind || 'whale';
  const variant = (stableHash(identity) % 997) / 997;
  const t = still ? 0 : timeMs / 1000;
  const bodyStyle = options.bodyStyle ?? 'smooth';
  const body = bodyStyle === 'pixel' ? DOT_PIXEL_BODY : DOT_BODY;
  const fresh = activity.freshness === 'fresh' && activity.observed;
  const failed = fresh && !activity.activeSpans.length && !!activity.failedToolAge;
  const elapsed = Math.max(0, timeMs - expressionAtMs) / 1000;
  const phase = (elapsed + 1.7) % 8.4;
  const formTime = still ? FORM_STILL_TIME : kind === 'done' ? (timeMs - (activity.observedAtMs ?? timeMs)) / 1000 : elapsed;
  const morph = !kind
    ? 0
    : still
      ? 1
      : kind === 'done'
        ? 1 - ease((timeMs - (activity.observedAtMs ?? timeMs) - 800) / 900)
        : ease(phase / 1.7) * (1 - ease((phase - 5.4) / 0.9));
  const agents = activity.parallelAgentCount >= 3 ? 3 : 2;
  const hue = failed ? 8 : kind ? FORM_HUE[kind] : 206;
  const points: number[][] = [];
  const materials: number[][] = [];

  for (let i = 0; i < body.length; i++) {
    const [hx, hy] = body[i]!;
    const tail = clamp((-hx - hy + 0.1) / 0.65);
    const homeX = hx * 1.9 + (still ? 0 : 0.008 * Math.sin(t * 0.75 + hy * 5));
    const homeY =
      hy * 1.9 +
      (still ? 0 : 0.009 * Math.sin(t * 1.1 + hx * 4) + tail * 0.012 * Math.sin(t * 1.55 + hx * 7));

    const inThroat = hy > 0.06 && hx > -0.18;
    let light = inThroat
      ? 0.80 + 0.08 * clamp((hx + hy) / 0.7)
      : 0.58 + 0.16 * clamp((hx + 0.45) / 0.9) + 0.03 * Math.cos(hy * 7);
    if (!still) light = clamp(light + 0.022 * Math.sin(hx * 9 - hy * 5 - t * 1.1), 0.52, 0.92);
    const baseHue = inThroat ? 196 : 208 + clamp((-hy - hx + 0.35) / 0.9) * 14;
    const base = bodyStyle === 'pixel' ? DOT_PIXEL_BODY[i]!.slice(2) : pigment(baseHue, light, inThroat ? 0.82 : 0.74);

    let x = homeX;
    let y = homeY;
    let color = fresh ? base : [126, 178, 224];
    let alpha = fresh ? (inThroat ? 0.94 : 0.86) : (inThroat ? 0.72 : 0.56);

    if (kind && morph > 0) {
      const e = ease(morph * 1.25 - ((i % 97) / 97) * 0.25);
      const target = formPoint(kind, bodyStyle, i, formTime, agents);
      x += (target.x * FORM_SCALE - homeX) * e;
      y += (target.y * FORM_SCALE - homeY) * e;
      const accent = pigment(hue + (variant * 4 - 2), clamp(0.56 + target.lit * 0.38, 0.50, 0.93), 0.78);
      color = base.map((v, k) => v + (accent[k]! - v) * e);
      alpha += (clamp(0.42 + 0.56 * target.fade) - alpha) * e;
    }

    points.push([clamp(x, -0.94, 0.94), clamp(y, -0.94, 0.94)]);
    materials.push([...color.map(Math.round), alpha]);
  }
  return { points, materials, morph, bodyStyle };
}

export function blendDotFields(from: DotField, to: DotField, progress: number): DotField {
  const m = ease(progress);
  if (m >= 1) return to;
  return {
    morph: from.morph + (to.morph - from.morph) * m,
    bodyStyle: to.bodyStyle,
    points: to.points.map((p, i) => p.map((v, k) => from.points[i]![k]! + (v - from.points[i]![k]!) * m)),
    materials: to.materials.map((p, i) => p.map((v, k) => from.materials[i]![k]! + (v - from.materials[i]![k]!) * m)),
  };
}
