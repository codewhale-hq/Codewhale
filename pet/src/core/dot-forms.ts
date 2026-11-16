import { DOT_BODY } from './dot-body.js';
import { DOT_PIXEL_BODY } from './dot-pixel-body.js';
import type { OwnerActivityKind } from './pet-engine.js';

export type FormKind = Exclude<OwnerActivityKind, 'tool'> | 'waiting' | 'done';
export type FormStyle = 'smooth' | 'pixel';
export interface FormPoint { x: number; y: number; lit: number; fade: number }
type Vec = number[];
type Rand = () => number;
interface Shape { area: number; at: (r: Rand) => Vec }
interface Part {
  shape: Shape; w: number; lit: number; dim?: number; at?: Vec;
  move?: (p: Vec, t: number) => Vec; glow?: (p: Vec, t: number) => number; fade?: (t: number) => number;
}

const TAU = Math.PI * 2;
const clamp = (v: number, a = 0, b = 1) => Math.max(a, Math.min(b, v));
const ease = (v: number) => { const u = clamp(v); return u * u * (3 - 2 * u); };
const mix = (a: number, b: number, t: number) => a + (b - a) * t;
const deg = (d: number) => d * Math.PI / 180;
function rng(seed: number): Rand {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6D2B79F5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const rect = (w: number, h: number): Shape => ({ area: w * h, at: r => [(r() - .5) * w, (r() - .5) * h] });
const disc = (rad: number): Shape => ({ area: Math.PI * rad * rad,
  at: r => { const a = r() * TAU, d = Math.sqrt(r()) * rad; return [Math.cos(a) * d, Math.sin(a) * d]; } });
const ring = (rad: number, th: number, a0 = 0, a1 = TAU): Shape => ({ area: rad * th * (a1 - a0),
  at: r => { const a = a0 + r() * (a1 - a0), d = rad + (r() - .5) * th; return [Math.cos(a) * d, Math.sin(a) * d]; } });
const oval = (rx: number, ry: number, th: number): Shape => ({ area: (rx + ry) * Math.PI * th,
  at: r => { const a = r() * TAU, k = 1 + (r() - .5) * th / Math.max(rx, ry); return [Math.cos(a) * rx * k, Math.sin(a) * ry * k]; } });
const ovalFill = (rx: number, ry: number): Shape => ({ area: Math.PI * rx * ry,
  at: r => { const a = r() * TAU, d = Math.sqrt(r()); return [Math.cos(a) * rx * d, Math.sin(a) * ry * d]; } });
const seg = (x1: number, y1: number, x2: number, y2: number, th: number): Shape => {
  const dx = x2 - x1, dy = y2 - y1, len = Math.hypot(dx, dy), nx = -dy / len, ny = dx / len;
  return { area: len * th, at: r => { const u = r(), v = (r() - .5) * th; return [x1 + dx * u + nx * v, y1 + dy * u + ny * v]; } };
};
const tri = (a: Vec, b: Vec, c: Vec): Shape => ({
  area: Math.abs((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])) / 2,
  at: r => {
    let u = r(), v = r();
    if (u + v > 1) { u = 1 - u; v = 1 - v; }
    return [a[0] + (b[0] - a[0]) * u + (c[0] - a[0]) * v, a[1] + (b[1] - a[1]) * u + (c[1] - a[1]) * v];
  },
});
const poly = (pts: Vec[], th: number) => pts.map((p, i) => seg(p[0], p[1], pts[(i + 1) % pts.length][0], pts[(i + 1) % pts.length][1], th));
const rot = (p: Vec, a: number, o: Vec = [0, 0]): Vec => {
  const c = Math.cos(a), s = Math.sin(a), x = p[0] - o[0], y = p[1] - o[1];
  return [o[0] + x * c - y * s, o[1] + x * s + y * c];
};
const P = (shape: Shape, opts: Partial<Part> = {}): Part => ({ shape, w: 1, lit: 0, ...opts });

const BUILD: Record<Exclude<FormKind, 'delegating'>, () => Part[]> = {
  executing: () => {
    const turn = (t: number) => { const u = (t * .7) % 1; return deg(-45 + (u < .72 ? mix(-26, 26, ease(u / .72)) : mix(26, -26, ease((u - .72) / .28)))); };
    const g = (p: Vec, t: number) => { const q = rot(p, turn(t)); return [q[0] + .3, q[1] - .3]; };
    return [
      P(ring(.2, .1, deg(38), deg(322)), { move: g }), P(rect(.74, .13), { at: [-.55, 0], move: g, w: 1.1 }),
      P(ring(.11, .075), { at: [-.98, 0], move: g }),
      P(disc(.085), { move: g, lit: .5, w: 1.6 }),
    ];
  },
  editing: () => {
    const tip = (t: number) => { const u = (t * .32) % 1; return [mix(-.75, .2, u), .36 + .035 * Math.sin(u * 46)]; };
    const pen = (p: Vec, t: number) => { const q = rot(p, deg(-52)), o = tip(t); return [q[0] + o[0], q[1] + o[1]]; };
    return [
      P(tri([0, 0], [.13, -.055], [.13, .055]), { move: pen, lit: .5, w: 1.4 }), P(rect(.6, .11), { at: [.43, 0], move: pen }),
      P(rect(.09, .11), { at: [.79, 0], move: pen, lit: .3 }),
      P(seg(-.75, .36, .2, .36, .03), { w: 1.6, move: (p, t) => {
        const o = tip(t), u = clamp((p[0] + .75) / .95);
        return p[0] > o[0] ? [o[0], o[1]] : [p[0], .36 + .035 * Math.sin(u * 46) + (p[1] - .36)];
      } }),
    ];
  },
  reading: () => {
    const leaf = (p: Vec, t: number) => { const k = Math.cos(((t * .28) % 1) * Math.PI); return [p[0] * k, p[1] - .03 * (1 - Math.abs(k))]; };
    const lines = (cx: number) => [0, 1, 2, 3, 4].map(k => P(seg(cx - .2, -.22 + k * .11, cx + .2, -.22 + k * .11, .028),
      { w: 1.3, glow: (p, t) => Math.exp(-Math.pow((p[1] + .22 - ((t * .5) % 1) * .5) * 14, 2)) }));
    return [
      ...poly([[-.6, -.36], [0, -.3], [0, .38], [-.6, .32]], .03).map(s => P(s)),
      ...poly([[.6, -.36], [0, -.3], [0, .38], [.6, .32]], .03).map(s => P(s)),
      ...lines(-.3), ...lines(.3),
      ...poly([[.56, -.33], [.02, -.28], [.02, .35], [.56, .3]], .022).map(s => P(s, { move: leaf, lit: .35 })),
    ];
  },
  searching: () => {
    const at = (t: number) => [.34 * Math.sin(t * .9), .22 * Math.sin(t * 1.37 + 1)];
    const lens = (p: Vec, t: number) => { const o = at(t); return [p[0] + o[0], p[1] + o[1]]; };
    return [
      ...[0, 1, 2, 3, 4, 5].map(k => P(seg(-.62, -.4 + k * .16, k % 2 ? .38 : .62, -.4 + k * .16, .03), { w: .9, dim: .45,
        glow: (p, t) => { const o = at(t); return Math.exp(-((p[0] - o[0]) ** 2 + (p[1] - o[1]) ** 2) * 30); } })),
      P(ring(.23, .06), { move: lens, lit: .4, w: 2 }), P(seg(.17, .17, .42, .42, .08), { move: lens, w: 1.6 }),
    ];
  },
  testing: () => [
    P(seg(-.1, -.56, -.1, -.2, .035)), P(seg(.1, -.56, .1, -.2, .035)), P(seg(-.16, -.57, .16, -.57, .04)),
    P(seg(-.1, -.2, -.5, .42, .035)), P(seg(.1, -.2, .5, .42, .035)), P(seg(-.5, .44, .5, .44, .04)),
    P(tri([-.26, .08], [.26, .08], [.46, .4]), { lit: .15, w: 1.1 }), P(tri([-.26, .08], [.46, .4], [-.46, .4]), { lit: .15, w: 1.1 }),
    ...[0, 1, 2, 3, 4].map(k => P(disc(.035 + (k % 2) * .015), { w: 2.4, lit: .6, move: (p, t) => {
      const u = (t * (.34 + k * .05) + k * .23) % 1;
      return [p[0] + (-.2 + k * .1) * (1 - u * .6) + .03 * Math.sin(u * 9 + k), p[1] + mix(.3, -.5, u)];
    } })),
  ],
  browsing: () => [
    P(ring(.56, .035), { w: 1.2 }), P(seg(-.56, 0, .56, 0, .028)), P(seg(-.48, -.28, .48, -.28, .026)), P(seg(-.48, .28, .48, .28, .026)),
    ...[0, 1, 2].map(k => P(oval(.56, .56, .03), { w: 1.1, lit: .25, move: (p, t) => [p[0] * Math.cos(t * .7 + k * TAU / 6), p[1]] })),
  ],
  computer: () => {
    const path = (t: number) => {
      const u = (t * .36) % 1, k = ease(clamp(u / .55));
      return [mix(-.5, .18, k), mix(-.42, .1, k) + (u > .55 && u < .66 ? .02 : 0)];
    };
    const ptr = (p: Vec, t: number) => { const o = path(t); return [p[0] + o[0], p[1] + o[1]]; };
    return [
      ...poly([[-.12, -.02], [.56, -.02], [.56, .26], [-.12, .26]], .035).map(s => P(s)),
      P(seg(.02, .12, .42, .12, .035), { dim: .6 }),
      P(tri([0, 0], [0, .34], [.24, .24]), { move: ptr, lit: .5, w: 1.6 }), P(seg(.09, .22, .17, .42, .06), { move: ptr, lit: .5 }),
      P(ring(.1, .03), { w: 1.4, lit: .6,
        move: (p, t) => { const u = clamp((((t * .36) % 1) - .55) / .4), k = 1 + u * 3.2; return [.18 + p[0] * k, .1 + p[1] * k]; },
        fade: t => { const u = (((t * .36) % 1) - .55) / .4; return u < 0 ? 0 : 1 - clamp(u); } }),
    ];
  },
  memory: () => [0, 1, 2].flatMap(k => {
    const bob = (p: Vec, t: number) => [p[0], p[1] - .36 + k * .3 + .05 * Math.sin(t * 1.3 - k * 1.1)];
    const lip: Shape = { area: Math.PI * .5 * .05,
      at: r => { const a = r() * Math.PI, d = 1 + (r() - .5) * .1; return [Math.cos(a) * .5 * d, .07 + Math.sin(a) * .11 * d]; } };
    return [P(oval(.5, .11, .05), { move: bob, lit: k === 0 ? .4 : .15, w: 1.2 }), P(lip, { move: bob, w: 1.2 }),
      ...(k === 0 ? [P(ovalFill(.42, .07), { move: bob, lit: .35, w: .35 })] : [])];
  }),
  thinking: () => [
    P(ring(.3, .045, deg(140), deg(400)), { at: [0, -.14], w: 1.3 }), P(seg(-.2, .1, -.11, .2, .04)), P(seg(.2, .1, .11, .2, .04)),
    P(rect(.24, .05), { at: [0, .25] }), P(rect(.2, .05), { at: [0, .33] }), P(rect(.12, .04), { at: [0, .4] }),
    ...poly([[-.1, .16], [-.05, -.1], [0, .02], [.05, -.1], [.1, .16]], .028).slice(0, 4)
      .map(s => P(s, { lit: .5, glow: (_p, t) => .5 + .5 * Math.sin(t * 3) })),
    ...[0, 1, 2, 3, 4, 5, 6].map(k => {
      const a = deg(150 + k * 40);
      return P(seg(Math.cos(a) * .42, -.14 + Math.sin(a) * .42, Math.cos(a) * .56, -.14 + Math.sin(a) * .56, .035), { w: 1.4, lit: .5,
        move: (p, t) => { const s = 1 + .1 * Math.sin(t * 3 - k * .5); return [p[0] * s, (p[1] + .14) * s - .14]; },
        fade: t => .45 + .55 * (.5 + .5 * Math.sin(t * 3 - k * .5)) });
    }),
  ],
  responding: () => [
    ...poly([[-.6, -.42], [.6, -.42], [.6, .22], [-.6, .22]], .04).map(s => P(s)), P(tri([-.34, .22], [-.1, .22], [-.4, .5]), { w: .8 }),
    ...[0, 1, 2].map(k => P(disc(.075), { w: 2.2, lit: .55,
      move: (p, t) => [p[0] - .26 + k * .26, p[1] - .1 - .09 * Math.max(0, Math.sin(t * 3.4 - k * .9))] })),
  ],
  waiting: () => {
    const swing = (t: number) => deg(15) * Math.sin(t * 5.2) * (.55 + .45 * Math.sin(t * .9));
    const g = (p: Vec, t: number) => rot(p, swing(t), [0, -.42]);
    const dome: Shape = { area: Math.PI * .38 * .46 / 2,
      at: r => { const a = Math.PI + r() * Math.PI, d = Math.sqrt(r()); return [Math.cos(a) * .38 * d, .12 + Math.sin(a) * .46 * d]; } };
    return [
      P(dome, { move: g }), P(rect(.94, .08), { at: [0, .16], move: g }), P(disc(.055), { at: [0, -.38], move: g }),
      P(disc(.075), { at: [0, .29], lit: .5, w: 1.6, move: (p, t) => rot(p, swing(t - .09) * 1.5, [0, -.42]) }),
      ...[-1, 1].flatMap(sd => [0, 1].map(k => P(ring(.62 + k * .13, .03, deg(sd > 0 ? -28 : 152), deg(sd > 0 ? 28 : 208)),
        { at: [0, -.05], w: 1.3, lit: .4, fade: t => .25 + .75 * Math.max(0, Math.sin(t * 5.2 - k * 1.2)) }))),
    ];
  },
  done: () => {
    const pop = (p: Vec, t: number) => { const s = 1 + .08 * Math.exp(-((t % 3) * 3)) * Math.cos(t * 9); return [p[0] * s, p[1] * s]; };
    return [P(seg(-.36, .02, -.1, .28, .12), { move: pop, lit: .45, w: 1.3 }), P(seg(-.1, .28, .42, -.3, .12), { move: pop, lit: .45, w: 1.3 }),
      P(ring(.64, .04), { w: .9, dim: .75 })];
  },
};

export const FORM_HUE: Record<FormKind, number> = {
  executing: 198, editing: 172, reading: 186, searching: 205, testing: 150, browsing: 212, computer: 222,
  memory: 268, thinking: 48, responding: 190, delegating: 200, waiting: 38, done: 160,
};
export const FORM_SCALE = 1.22;
export const FORM_STILL_TIME = 1.3;

interface Slot { part: Part; local: Vec }
const BODIES: Record<FormStyle, readonly (readonly number[])[]> = { smooth: DOT_BODY, pixel: DOT_PIXEL_BODY };
const N = DOT_BODY.length;
const rankBy = (points: readonly (readonly number[])[]) =>
  points.map((p, i) => [p[0] + p[1] * .35, i]).sort((a, b) => a[0] - b[0] || a[1] - b[1]).map(v => v[1]);

function compile(key: Exclude<FormKind, 'delegating'>, homeRank: number[]): Slot[] {
  const parts = BUILD[key](), random = rng(key.length * 7919 + key.charCodeAt(0) * 131);
  const total = parts.reduce((s, p) => s + p.shape.area * p.w, 0);
  const slots: Slot[] = [];
  for (const part of parts) {
    const n = Math.max(3, Math.round(N * part.shape.area * part.w / total));
    for (let i = 0; i < n; i++) {
      const q = part.shape.at(random);
      slots.push({ part, local: [q[0] + (part.at?.[0] ?? 0), q[1] + (part.at?.[1] ?? 0)] });
    }
  }
  while (slots.length > N) slots.splice(Math.floor(random() * slots.length), 1);
  while (slots.length < N) slots.push({ ...slots[Math.floor(random() * slots.length)] });
  const rest = slots.map(s => (s.part.move ? s.part.move(s.local, 0) : s.local));
  const order = slots.map((_, i) => i).sort((a, b) =>
    (rest[a][0] + rest[a][1] * .35) - (rest[b][0] + rest[b][1] * .35) || a - b);
  const bySlot: Slot[] = new Array(N);
  order.forEach((slot, rank) => { bySlot[homeRank[rank]] = slots[slot]; });
  return bySlot;
}

const COMPILED = Object.fromEntries((['smooth', 'pixel'] as const).map(style => {
  const homeRank = rankBy(BODIES[style]);
  return [style, Object.fromEntries((Object.keys(BUILD) as Exclude<FormKind, 'delegating'>[]).map(key => [key, compile(key, homeRank)]))];
})) as Record<FormStyle, Record<Exclude<FormKind, 'delegating'>, Slot[]>>;

const POD = [
  [[-.42, .26, .6], [.06, -.32, .72], [.46, .28, .56]],
  [[-.4, .1, .66], [.4, -.1, .66]],
];

export function formPoint(kind: FormKind, style: FormStyle, i: number, t: number, agents = 3): FormPoint {
  if (kind === 'delegating') {
    const pod = agents >= 3 ? POD[0] : POD[1], w = i % pod.length, [ox, oy, s] = pod[w], [hx, hy] = BODIES[style][i];
    return { x: ox + hx * s + .05 * Math.sin(t * .8 + w * 2), y: oy + hy * s + .035 * Math.sin(t * 1.5 + w * 1.7 + hx * 5),
      lit: .12 + .1 * w, fade: 1 };
  }
  const slot = COMPILED[style][kind][i], part = slot.part, p = part.move ? part.move(slot.local, t) : slot.local;
  const glow = part.glow ? part.glow(p, t) : 0;
  return { x: p[0], y: p[1], lit: part.lit + glow * .45, fade: (part.fade ? part.fade(t) : 1) * (part.dim ?? 1) };
}
