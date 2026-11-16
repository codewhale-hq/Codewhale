import { stableHash } from './model.js';
import { DOT_BODY } from './dot-body.js';
import { DOT_PIXEL_BODY } from './dot-pixel-body.js';
import { FORM_HUE, FORM_SCALE, FORM_STILL_TIME, formPoint, type FormKind } from './dot-forms.js';
import type { EngineOwnerProjection } from './pet-engine.js';

export type BodyStyle = 'smooth' | 'pixel';
export interface DotOptions { bodyStyle?: BodyStyle }
export interface DotField { points: number[][]; materials: number[][]; morph: number; bodyStyle: BodyStyle }
const clamp = (n: number, lo = 0, hi = 1) => Math.max(lo, Math.min(hi, n));
const ease = (n: number) => { const u = clamp(n); return u * u * (3 - 2 * u); };

function form(a: EngineOwnerProjection, timeMs: number): FormKind | null {
  if (a.freshness !== 'fresh' || !a.observed) return null;
  if (a.authoritativePresence === 'needs_you') return 'waiting';
  if (a.authoritativePresence === 'done') return timeMs - (a.observedAtMs ?? timeMs) < 2600 ? 'done' : null;
  if (a.authoritativePresence === 'idle' || !a.activityKind) return null;
  return a.activityKind === 'tool' ? 'executing' : a.activityKind;
}

export function dotFieldKey(a: EngineOwnerProjection): string {
  return [a.freshness, a.authoritativePresence, a.activityKind, a.actionId,
    a.parallelAgentCount, a.doneEffectId, a.activeSpans.length === 0 && !!a.failedToolAge].join(':');
}

function pigment(hue: number, light: number): number[] {
  const h = ((hue % 360) + 360) % 360 / 60, c = (1 - Math.abs(2 * light - 1)) * .62;
  const x = c * (1 - Math.abs(h % 2 - 1)), m = light - c / 2;
  const rgb = h < 1 ? [c, x, 0] : h < 2 ? [x, c, 0] : h < 3 ? [0, c, x]
    : h < 4 ? [0, x, c] : h < 5 ? [x, 0, c] : [c, 0, x];
  return rgb.map(v => Math.round((v + m) * 255));
}

export function projectDotField(activity: EngineOwnerProjection,
  timeMs: number, still = false, expressionAtMs = 0, options: DotOptions = {}): DotField {
  const kind = form(activity, timeMs), identity = activity.actionId || activity.activityKind || 'whale';
  const variant = (stableHash(identity) % 997) / 997;
  const t = still ? 0 : timeMs / 1000;
  const bodyStyle = options.bodyStyle ?? 'smooth';
  const body = bodyStyle === 'pixel' ? DOT_PIXEL_BODY : DOT_BODY;
  const fresh = activity.freshness === 'fresh' && activity.observed;
  const failed = fresh && !activity.activeSpans.length && !!activity.failedToolAge;
  const elapsed = Math.max(0, timeMs - expressionAtMs) / 1000;
  const phase = (elapsed + 1.7) % 8.4;
  const formTime = still ? FORM_STILL_TIME : kind === 'done' ? (timeMs - (activity.observedAtMs ?? timeMs)) / 1000 : elapsed;
  const morph = !kind ? 0 : still ? 1 : kind === 'done'
    ? 1 - ease((timeMs - (activity.observedAtMs ?? timeMs) - 800) / 900)
    : ease(phase / 1.7) * (1 - ease((phase - 5.4) / .9));
  const agents = activity.parallelAgentCount >= 3 ? 3 : 2;
  const hue = failed ? 16 : kind ? FORM_HUE[kind] : 188;
  const points: number[][] = [], materials: number[][] = [];
  for (let i = 0; i < body.length; i++) {
    const [hx, hy] = body[i];
    const tail = clamp((-hx - hy + .1) / .65);
    const homeX = hx * 1.9 + (still ? 0 : .008 * Math.sin(t * .75 + hy * 5));
    const homeY = hy * 1.9 + (still ? 0 : .009 * Math.sin(t * 1.1 + hx * 4)
      + tail * .012 * Math.sin(t * 1.55 + hx * 7));
    let light = .61 + .14 * clamp((hx + .45) / .9) + .035 * Math.cos(hy * 7);
    if (!still) light = clamp(light + .025 * Math.sin(hx * 10 - hy * 5 - t * 1.1), .5, .9);
    const base = bodyStyle === 'pixel' ? DOT_PIXEL_BODY[i].slice(2) : pigment(185 + clamp((hy - hx + .45) / .9) * 70, light);
    let x = homeX, y = homeY, color = fresh ? base : [145, 166, 178], alpha = fresh ? .88 : .42;
    if (kind && morph > 0) {
      const e = ease(morph * 1.25 - ((i % 97) / 97) * .25), target = formPoint(kind, bodyStyle, i, formTime, agents);
      x += (target.x * FORM_SCALE - homeX) * e;
      y += (target.y * FORM_SCALE - homeY) * e;
      const accent = pigment(hue + (i % 9) - 4 + variant * 6 - 3, clamp(.6 + target.lit * .4, .5, .92));
      color = base.map((v, k) => v + (accent[k] - v) * e);
      alpha += (clamp(.25 + .75 * target.fade) - alpha) * e;
    }
    points.push([clamp(x, -.94, .94), clamp(y, -.94, .94)]);
    materials.push([...color.map(Math.round), alpha]);
  }
  return { points, materials, morph, bodyStyle };
}

export function blendDotFields(from: DotField, to: DotField, progress: number): DotField {
  const mix = ease(progress);
  if (mix >= 1) return to;
  return {
    morph: from.morph + (to.morph - from.morph) * mix,
    bodyStyle: to.bodyStyle,
    points: to.points.map((p, i) => p.map((v, k) => from.points[i][k] + (v - from.points[i][k]) * mix)),
    materials: to.materials.map((p, i) => p.map((v, k) => from.materials[i][k] + (v - from.materials[i][k]) * mix)),
  };
}
