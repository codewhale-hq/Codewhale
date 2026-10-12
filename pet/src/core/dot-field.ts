import { stableHash } from './model.js';
import { DOT_GLYPHS, type GlyphLanguage } from './dot-glyphs.js';
import { DOT_BODY } from './dot-body.js';
import { DOT_PIXEL_BODY } from './dot-pixel-body.js';
import type { EngineOwnerProjection, OwnerActivityKind } from './pet-engine.js';

export type BodyStyle = 'smooth' | 'pixel';
export type TextLanguage = GlyphLanguage | 'auto';
export interface DotOptions { bodyStyle?: BodyStyle; textLanguage?: TextLanguage }
export interface DotField { points: number[][]; materials: number[][]; morph: number; bodyStyle: BodyStyle }
type Verb = OwnerActivityKind | 'waiting' | 'done';
const TAU = Math.PI * 2;
const clamp = (n: number, lo = 0, hi = 1) => Math.max(lo, Math.min(hi, n));
const ease = (n: number) => { const u = clamp(n); return u * u * (3 - 2 * u); };
const GLYPH_POINTS = Object.fromEntries(Object.entries(DOT_GLYPHS).map(([language, glyphs]) => [language,
  Object.fromEntries(Object.entries(glyphs).map(([key, rows]) => [key,
    rows.flatMap((row, y) => [...row].flatMap((pixel, x) => pixel === '#' ? [[(x - 23.5) / 24, (y - 23.5) / 24]] : [])),
  ])),
])) as Record<GlyphLanguage, Record<Verb, number[][]>>;
const HUES: Record<Verb, number> = {
  reading: 185, editing: 190, searching: 194, testing: 201, executing: 198,
  browsing: 195, computer: 198, memory: 180, tool: 195, thinking: 185,
  responding: 188, delegating: 192, waiting: 38, done: 180,
};

function verb(a: EngineOwnerProjection, timeMs: number): Verb | null {
  if (a.freshness !== 'fresh' || !a.observed) return null;
  if (a.authoritativePresence === 'needs_you') return 'waiting';
  if (a.authoritativePresence === 'done') return timeMs - (a.observedAtMs ?? timeMs) < 2600 ? 'done' : null;
  if (a.authoritativePresence === 'idle') return null;
  return a.activityKind;
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

export function textLanguage(value = 'auto', interfaceLanguage = 'en'): GlyphLanguage {
  const tag = (value === 'auto' ? interfaceLanguage : value).toLowerCase();
  return tag.startsWith('ja') ? 'ja' : /^(zh-hant|zh-tw|zh-hk|zh-mo)/.test(tag) ? 'zh-Hant'
    : tag.startsWith('zh') ? 'zh-Hans' : 'en';
}

export function projectDotField(activity: EngineOwnerProjection,
  timeMs: number, still = false, expressionAtMs = 0, options: DotOptions = {}): DotField {
  const kind = verb(activity, timeMs), identity = activity.actionId || activity.activityKind || 'whale';
  const seed = stableHash(identity), variant = (seed % 997) / 997;
  const t = still ? 0 : timeMs / 1000, cycle = still ? .43 : (t / (2.8 + variant * .6)) % 1;
  const sweep = -.86 + cycle * 1.72, scanRow = Math.floor(cycle * 5) % 5;
  const bodyStyle = options.bodyStyle ?? 'smooth', language = textLanguage(options.textLanguage);
  const body = bodyStyle === 'pixel' ? DOT_PIXEL_BODY : DOT_BODY;
  const glyph = kind ? GLYPH_POINTS[language][kind] : null;
  const fresh = activity.freshness === 'fresh' && activity.observed;
  const failed = fresh && !activity.activeSpans.length && !!activity.failedToolAge;
  const phase = ((Math.max(0, timeMs - expressionAtMs) / 1000 + 1.7) % 8.4);
  const morph = !kind ? 0 : still ? 1 : kind === 'done'
    ? 1 - ease((timeMs - (activity.observedAtMs ?? timeMs) - 800) / 900)
    : ease(phase / 1.7) * (1 - ease((phase - 5.4) / .9));
  const points: number[][] = [], materials: number[][] = [];
  const lens = [Math.sin(cycle * TAU) * .62, -.62 + scanRow * .31];
  for (let i = 0; i < body.length; i++) {
    const [hx, hy] = body[i];
    const tail = clamp((-hx - hy + .1) / .65);
    const homeX = hx * 1.9 + (still ? 0 : .008 * Math.sin(t * .75 + hy * 5));
    const homeY = hy * 1.9 + (still ? 0 : .009 * Math.sin(t * 1.1 + hx * 4)
      + tail * .012 * Math.sin(t * 1.55 + hx * 7));
    const bodyLight = .61 + .14 * clamp((hx + .45) / .9) + .035 * Math.cos(hy * 7);
    let x = homeX, y = homeY, light = bodyLight, alpha = fresh ? .88 : .42;
    if (glyph && kind) {
      [x, y] = glyph[Math.floor(i * glyph.length / body.length)];
      x += ((i * 17) % 7 - 3) * .0016;
      y += ((i * 11) % 7 - 3) * .0016;
      let beam = Math.exp(-Math.pow((y - sweep) * 10, 2));
      switch (kind) {
        case 'reading':
          beam = Math.exp(-Math.pow((y + .64 - scanRow * .32) * 18, 2))
            * (.35 + .65 * Math.exp(-Math.pow((x - sweep) * 4, 2))); break;
        case 'searching': case 'computer':
          beam = Math.exp(-((x - lens[0]) ** 2 + (y - lens[1]) ** 2) * 24); break;
        case 'editing': case 'responding': case 'testing':
          beam = Math.exp(-Math.pow((x - sweep) * 12, 2)); break;
        case 'executing': case 'tool':
          beam = (1 + Math.sin(x * 8 - y * 3 - cycle * TAU)) / 2; break;
        case 'memory':
          beam = Math.exp(-Math.pow((Math.hypot(x, y) - (1 - cycle)) * 12, 2)); break;
        case 'thinking':
          beam = (1 + Math.cos(Math.atan2(y, x) - cycle * TAU)) / 2; break;
        case 'delegating':
          beam = (1 + Math.sin(y * Math.max(1, Math.min(6, activity.parallelAgentCount)) * 5 - cycle * TAU)) / 2; break;
        case 'waiting':
          beam = .2 + .2 * (1 + Math.sin(t * 1.5)) / 2; break;
        case 'done':
          beam = still ? .35 : clamp(1 - (timeMs - (activity.observedAtMs ?? timeMs)) / 1700); break;
      }
      light = .66 + beam * .18;
      alpha = .84 + beam * .15;
      if (!still) {
        const px = x, py = y;
        x += .009 * Math.sin(py * 12 - t * 1.3) + .003 * Math.sin(px * 24 + py * 8 + t * 2.1);
        y += .007 * Math.sin(px * 13 + t * 1.05) + .003 * Math.cos(py * 26 - px * 6 - t * 1.7);
      }
      x = homeX + (x - homeX) * morph; y = homeY + (y - homeY) * morph;
      light = bodyLight + (light - bodyLight) * morph;
      alpha = .88 + (alpha - .88) * morph;
    }
    if (!still) light = clamp(light + .025 * Math.sin(hx * 10 - hy * 5 - t * 1.1), .5, .9);
    points.push([clamp(x, -.94, .94), clamp(y, -.94, .94)]);
    const hue = morph > 0 ? (failed ? 16 : kind ? HUES[kind] : 188) : 188;
    const base = bodyStyle === 'pixel' ? DOT_PIXEL_BODY[i].slice(2)
      : pigment(185 + clamp((hy - hx + .45) / .9) * 70, light);
    const accent = pigment(hue + variant * 6 - 3, light);
    const color = !fresh ? [145, 166, 178] : base.map((v, k) => Math.round(v + (accent[k] - v) * morph));
    materials.push([...color, alpha]);
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
