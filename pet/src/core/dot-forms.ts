import { DOT_BODY } from './dot-body.js';
import { DOT_PIXEL_BODY } from './dot-pixel-body.js';
import type { OwnerActivityKind } from './pet-engine.js';

export type FormKind = Exclude<OwnerActivityKind, 'tool'> | 'waiting' | 'done';
export type FormStyle = 'smooth' | 'pixel';
export interface FormPoint {
  x: number;
  y: number;
  lit: number;
  fade: number;
}

const TAU = Math.PI * 2;
const DEG = Math.PI / 180;
const N = DOT_BODY.length;

const clamp = (v: number, lo = 0, hi = 1): number => (v < lo ? lo : v > hi ? hi : v);
const smoothstep = (e0: number, e1: number, x: number): number => {
  const t = clamp((x - e0) / (e1 - e0));
  return t * t * (3 - 2 * t);
};
const mix = (a: number, b: number, t: number): number => a + (b - a) * t;

export const FORM_HUE: Record<FormKind, number> = {
  executing: 208,
  editing: 170,
  reading: 206,
  searching: 210,
  testing: 168,
  browsing: 212,
  computer: 214,
  memory: 264,
  thinking: 44,
  responding: 172,
  delegating: 172,
  waiting: 38,
  done: 166,
};
export const FORM_SCALE = 1.0;
export const FORM_STILL_TIME = 1.25;

type Pt = readonly [number, number];
type Poly = readonly Pt[];

function pointInPoly(x: number, y: number, poly: Poly): boolean {
  let inside = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const xi = poly[i]![0];
    const yi = poly[i]![1];
    const xj = poly[j]![0];
    const yj = poly[j]![1];
    if ((yi > y) !== (yj > y) && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

function polySdf(x: number, y: number, poly: Poly): number {
  let best = Infinity;
  for (let i = 0; i < poly.length - 1; i++) {
    const ax = poly[i]![0];
    const ay = poly[i]![1];
    const bx = poly[i + 1]![0];
    const by = poly[i + 1]![1];
    const dx = bx - ax;
    const dy = by - ay;
    const len2 = dx * dx + dy * dy;
    const t = len2 > 1e-9 ? clamp(((x - ax) * dx + (y - ay) * dy) / len2) : 0;
    const px = ax + dx * t;
    const py = ay + dy * t;
    const d2 = (x - px) * (x - px) + (y - py) * (y - py);
    if (d2 < best) best = d2;
  }
  const d = Math.sqrt(best);
  return pointInPoly(x, y, poly) ? -d : d;
}

const sdCircle = (x: number, y: number, cx: number, cy: number, r: number): number => Math.hypot(x - cx, y - cy) - r;
const sdRing = (x: number, y: number, cx: number, cy: number, r: number, th: number): number => Math.abs(Math.hypot(x - cx, y - cy) - r) - th;

const sdEllipse = (x: number, y: number, cx: number, cy: number, rx: number, ry: number, rotRad = 0): number => {
  const dx = x - cx;
  const dy = y - cy;
  const c = Math.cos(rotRad);
  const s = Math.sin(rotRad);
  const u = dx * c + dy * s;
  const v = -dx * s + dy * c;
  return (Math.hypot(u / rx, v / ry) - 1) * Math.min(rx, ry);
};

const sdBox = (x: number, y: number, cx: number, cy: number, hw: number, hh: number, r = 0, rotRad = 0): number => {
  const dx = x - cx;
  const dy = y - cy;
  const c = Math.cos(rotRad);
  const s = Math.sin(rotRad);
  const qx = Math.abs(dx * c + dy * s) - (hw - r);
  const qy = Math.abs(-dx * s + dy * c) - (hh - r);
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
};

const sdSegment = (x: number, y: number, ax: number, ay: number, bx: number, by: number, r: number): number => {
  const pax = x - ax;
  const pay = y - ay;
  const bax = bx - ax;
  const bay = by - ay;
  const h = clamp((pax * bax + pay * bay) / (bax * bax + bay * bay || 1));
  return Math.hypot(pax - bax * h, pay - bay * h) - r;
};

const sdArc = (x: number, y: number, cx: number, cy: number, r: number, halfTh: number, midDeg: number, spanDeg: number): number => {
  const dx = x - cx;
  const dy = y - cy;
  const mid = midDeg * DEG;
  const halfSpan = spanDeg * 0.5 * DEG;
  let da = Math.atan2(dy, dx) - mid;
  while (da > Math.PI) da -= TAU;
  while (da < -Math.PI) da += TAU;
  if (Math.abs(da) <= halfSpan) return Math.abs(Math.hypot(dx, dy) - r) - halfTh;
  const a1 = mid - halfSpan;
  const a2 = mid + halfSpan;
  return Math.min(
    Math.hypot(dx - r * Math.cos(a1), dy - r * Math.sin(a1)),
    Math.hypot(dx - r * Math.cos(a2), dy - r * Math.sin(a2)),
  ) - halfTh;
};

const opUnion = (...ds: number[]): number => Math.min(...ds);
const opSmoothUnion = (d1: number, d2: number, k: number): number => {
  const h = clamp(0.5 + (0.5 * (d2 - d1)) / k);
  return mix(d2, d1, h) - k * h * (1 - h);
};
const opSub = (d1: number, d2: number): number => Math.max(d1, -d2);

interface Sample {
  d: number;
  zone: number;
  part: number;
}

function sdMiniWhale(x: number, y: number, cx: number, cy: number, scale: number): Sample {
  const lx = (x - cx) / scale;
  const ly = (y - cy) / scale;
  const head = sdEllipse(lx, ly, 0.25, 0.01, 0.38, 0.22, 0.05);
  const back = sdEllipse(lx, ly, -0.07, 0.0, 0.35, 0.165, -0.10);
  const dorsal = polySdf(lx, ly, [[-0.16, -0.14], [-0.03, -0.14], [-0.18, -0.29], [-0.16, -0.14]]);
  const peduncle = sdSegment(lx, ly, -0.20, -0.01, -0.49, -0.04, 0.076);
  const flukeUp = sdEllipse(lx, ly, -0.60, -0.19, 0.175, 0.062, -0.74);
  const flukeDn = sdEllipse(lx, ly, -0.59, 0.11, 0.165, 0.058, 0.66);
  const pectoral = sdEllipse(lx, ly, 0.10, 0.20, 0.15, 0.052, 0.50);
  const body = opSmoothUnion(opSmoothUnion(head, back, 0.09), peduncle, 0.07);
  const full = opUnion(body, dorsal, flukeUp, flukeDn, pectoral);
  const eye = sdCircle(lx, ly, 0.37, -0.02, 0.056);
  return {
    d: opSub(full, eye) * scale,
    zone: ly > 0.03 && lx > -0.05 ? 1 : 0,
    part: lx < -0.35 ? 1 : 0,
  };
}

type CompileKey = Exclude<FormKind, 'delegating'> | 'delegating2' | 'delegating3';

function evalSdf(kind: CompileKey, x: number, y: number): Sample {
  switch (kind) {
    case 'thinking': {
      const dome = sdCircle(x, y, 0, -0.14, 0.39);
      const neck = sdBox(x, y, 0, 0.18, 0.21, 0.13, 0.04);
      const cavity = opSmoothUnion(sdCircle(x, y, 0, -0.15, 0.255), sdBox(x, y, 0, 0.10, 0.105, 0.10, 0.03), 0.05);
      const shell = opSub(opSmoothUnion(dome, neck, 0.08), cavity);
      const filament = opUnion(
        sdSegment(x, y, 0, 0.09, 0, -0.13, 0.044),
        sdSegment(x, y, 0, -0.13, -0.09, -0.22, 0.038),
        sdSegment(x, y, 0, -0.13, 0.09, -0.22, 0.038),
      );
      const base1 = sdBox(x, y, 0, 0.39, 0.19, 0.044, 0.03);
      const base2 = sdBox(x, y, 0, 0.52, 0.13, 0.042, 0.03);
      let rays = Infinity;
      let rayIdx = 0;
      const angles = [-155, -118, -90, -62, -25];
      for (let i = 0; i < angles.length; i++) {
        const a = angles[i]! * DEG;
        const dr = sdSegment(x, y, Math.cos(a) * 0.51, -0.14 + Math.sin(a) * 0.51, Math.cos(a) * 0.67, -0.14 + Math.sin(a) * 0.67, 0.040);
        if (dr < rays) {
          rays = dr;
          rayIdx = i + 2;
        }
      }
      const d = opUnion(shell, filament, base1, base2, rays);
      if (filament <= shell && filament <= rays) return { d, zone: 2, part: 1 };
      if (rays <= shell && rays <= base1 && rays <= base2) return { d, zone: 2, part: rayIdx };
      return { d, zone: y > 0.3 ? 1 : 0, part: 0 };
    }
    case 'reading': {
      const gapX = 0.052;
      const left = polySdf(x, y, [[-0.78, -0.44], [-gapX, -0.34], [-gapX, 0.52], [-0.78, 0.42], [-0.78, -0.44]]);
      const right = polySdf(x, y, [[gapX, -0.34], [0.78, -0.44], [0.78, 0.42], [gapX, 0.52], [gapX, -0.34]]);
      let cuts = Infinity;
      for (const ry of [-0.20, 0.03, 0.26]) {
        cuts = Math.min(cuts, sdSegment(x, y, -0.63, ry - 0.04, -0.19, ry + 0.02, 0.036), sdSegment(x, y, 0.19, ry + 0.02, 0.63, ry - 0.04, 0.036));
      }
      const pages = opSub(opUnion(left, right), cuts);
      const cover = opUnion(sdSegment(x, y, -0.76, 0.54, 0, 0.62, 0.026), sdSegment(x, y, 0, 0.62, 0.76, 0.54, 0.026));
      return { d: opUnion(pages, cover), zone: Math.abs(x) < 0.18 ? 1 : 0, part: x > 0 ? 1 : 0 };
    }
    case 'searching': {
      const lcx = -0.10;
      const lcy = -0.10;
      const lr = 0.38;
      const rim = sdRing(x, y, lcx, lcy, lr, 0.072);
      const handle = sdSegment(x, y, lcx + lr * 0.73, lcy + lr * 0.73, 0.62, 0.62, 0.082);
      const glint = sdArc(x, y, lcx, lcy, lr * 0.62, 0.028, -132, 68);
      const glass = opUnion(rim, handle, glint);
      let lines = Infinity;
      for (const [x0, ly, x1] of [[-0.72, -0.58, -0.34], [-0.72, -0.34, -0.56], [-0.72, 0.16, -0.52], [-0.72, 0.40, -0.18]] as const) {
        lines = Math.min(lines, sdSegment(x, y, x0, ly, x1, ly, 0.026));
      }
      const cleanLines = opSub(lines, sdCircle(x, y, lcx, lcy, lr + 0.11));
      const d = opUnion(glass, cleanLines);
      if (glass <= cleanLines) return { d, zone: glint < rim ? 2 : 1, part: 1 };
      return { d, zone: 0, part: 0 };
    }
    case 'editing': {
      const ang = -45 * DEG;
      const dx = x - 0.04;
      const dy = y + 0.06;
      const u = dx * Math.cos(ang) + dy * Math.sin(ang);
      const v = -dx * Math.sin(ang) + dy * Math.cos(ang);
      const barrel = Math.max(Math.abs(u - 0.06) - 0.36, Math.abs(v) - 0.135);
      const eraser = sdBox(u, v, 0.52, 0, 0.085, 0.135, 0.04);
      const nib = polySdf(u, v, [[-0.34, -0.135], [-0.34, 0.135], [-0.63, 0], [-0.34, -0.135]]);
      const gaps = Math.min(sdBox(u, v, -0.32, 0, 0.026, 0.16), sdBox(u, v, 0.43, 0, 0.026, 0.16));
      const pencil = opSub(opUnion(barrel, eraser, nib), gaps);
      const line = sdSegment(x, y, -0.44, 0.58, 0.62, 0.58, 0.040);
      const d = opUnion(pencil, line);
      if (line < pencil) return { d, zone: 2, part: 2 };
      return { d, zone: u < -0.34 ? 2 : u > 0.43 ? 1 : 0, part: 1 };
    }
    case 'executing': {
      const jx = 0.25;
      const jy = -0.25;
      const jaw = opSub(sdCircle(x, y, jx, jy, 0.30), sdBox(x, y, jx + 0.16, jy - 0.16, 0.24, 0.135, 0.02, -45 * DEG));
      const shaft = sdSegment(x, y, 0.06, -0.06, -0.38, 0.38, 0.108);
      const tail = opSub(sdCircle(x, y, -0.44, 0.44, 0.19), sdCircle(x, y, -0.44, 0.44, 0.085));
      const wrench = opSub(opUnion(opSmoothUnion(jaw, shaft, 0.05), tail), sdCircle(x, y, jx + 0.03, jy - 0.03, 0.128));
      const nut = sdCircle(x, y, jx + 0.03, jy - 0.03, 0.066);
      const d = opUnion(wrench, nut);
      if (nut < wrench) return { d, zone: 2, part: 2 };
      return { d, zone: jaw < shaft ? 1 : 0, part: 1 };
    }
    case 'testing': {
      const lip = sdSegment(x, y, -0.18, -0.60, 0.18, -0.60, 0.046);
      const neckOuter = sdBox(x, y, 0, -0.40, 0.135, 0.20, 0.02);
      const coneOuter = polySdf(x, y, [[-0.135, -0.22], [0.135, -0.22], [0.58, 0.58], [-0.58, 0.58], [-0.135, -0.22]]);
      const outer = opSmoothUnion(opUnion(lip, neckOuter, coneOuter), sdBox(x, y, 0, 0.56, 0.56, 0.06, 0.05), 0.04);
      const inner = opUnion(sdBox(x, y, 0, -0.38, 0.074, 0.24), polySdf(x, y, [[-0.074, -0.20], [0.074, -0.20], [0.27, 0.16], [-0.27, 0.16], [-0.074, -0.20]]));
      const cuts = opUnion(sdCircle(x, y, -0.15, 0.39, 0.062), sdCircle(x, y, 0.16, 0.45, 0.070), sdCircle(x, y, 0.02, 0.28, 0.048));
      const flask = opSub(opSub(outer, inner), cuts);
      const b1 = sdCircle(x, y, -0.02, -0.75, 0.048);
      const b2 = sdCircle(x, y, 0.09, -0.44, 0.038);
      const d = opUnion(flask, b1, b2);
      if (b1 < flask || b2 < flask) return { d, zone: 2, part: 2 };
      const liq = y > 0.16 && y < 0.54 && Math.abs(x) < 0.48;
      return { d, zone: liq ? 1 : 0, part: liq ? 1 : 0 };
    }
    case 'browsing': {
      const R = 0.64;
      const outer = sdRing(x, y, 0, 0, R, 0.056);
      const clip = sdCircle(x, y, 0, 0, R + 0.02);
      const eq = Math.max(sdSegment(x, y, -R, 0, R, 0, 0.038), clip);
      const lat1 = Math.max(sdSegment(x, y, -R * 0.86, -0.32, R * 0.86, -0.32, 0.032), clip);
      const lat2 = Math.max(sdSegment(x, y, -R * 0.86, 0.32, R * 0.86, 0.32, 0.032), clip);
      const mer = opSub(sdEllipse(x, y, 0, 0, 0.33, R, 0), sdEllipse(x, y, 0, 0, 0.25, R - 0.06, 0));
      const d = opUnion(outer, eq, lat1, lat2, mer);
      const isMer = mer <= outer && mer <= eq && mer <= lat1 && mer <= lat2;
      return { d, zone: isMer ? 2 : 1, part: isMer ? (x >= 0 ? 1 : 2) : 0 };
    }
    case 'computer': {
      const arrow = polySdf(x, y, [[-0.24, -0.48], [-0.24, 0.34], [-0.04, 0.16], [0.10, 0.48], [0.23, 0.42], [0.09, 0.10], [0.34, 0.10], [-0.24, -0.48]]);
      const ripple = sdArc(x, y, -0.24, -0.48, 0.22, 0.034, -125, 105);
      const bar = sdBox(x, y, 0.38, 0.56, 0.26, 0.066, 0.034);
      const d = opUnion(arrow, ripple, bar);
      if (ripple < arrow && ripple < bar) return { d, zone: 2, part: 2 };
      if (bar < arrow) return { d, zone: 0, part: 0 };
      return { d, zone: 1, part: 1 };
    }
    case 'memory': {
      const dia = (cy: number) => polySdf(x, y, [[0, cy - 0.24], [0.64, cy], [0, cy + 0.24], [-0.64, cy], [0, cy - 0.24]]);
      const top = dia(-0.36);
      const mid = opSub(dia(0.04), dia(-0.11) - 0.058);
      const bot = opSub(dia(0.44), dia(0.29) - 0.058);
      const d = opUnion(top, mid, bot);
      if (top <= mid && top <= bot) return { d, zone: 2, part: 1 };
      if (mid <= bot) return { d, zone: 1, part: 2 };
      return { d, zone: 0, part: 3 };
    }
    case 'responding': {
      const bubble = opUnion(sdEllipse(x, y, 0, -0.06, 0.66, 0.46), polySdf(x, y, [[-0.36, 0.24], [-0.48, 0.62], [-0.08, 0.34], [-0.36, 0.24]]));
      let holes = Infinity;
      let cores = Infinity;
      let idx = 0;
      const xs = [-0.28, 0, 0.28];
      for (let i = 0; i < 3; i++) {
        holes = Math.min(holes, sdCircle(x, y, xs[i]!, -0.06, 0.115));
        const dc = sdCircle(x, y, xs[i]!, -0.06, 0.056);
        if (dc < cores) {
          cores = dc;
          idx = i + 1;
        }
      }
      const shell = opSub(bubble, holes);
      const d = opUnion(shell, cores);
      if (cores < shell) return { d, zone: 2, part: idx };
      return { d, zone: y < -0.2 ? 1 : 0, part: 0 };
    }
    case 'delegating2': {
      const w1 = sdMiniWhale(x, y, -0.10, -0.30, 0.72);
      const w2 = sdMiniWhale(x, y, 0.16, 0.30, 0.76);
      if (w1.d <= w2.d) return { d: w1.d, zone: w1.zone, part: 1 + w1.part };
      return { d: w2.d, zone: w2.zone === 1 ? 2 : 1, part: 3 + w2.part };
    }
    case 'delegating3': {
      const w1 = sdMiniWhale(x, y, -0.10, -0.46, 0.62);
      const w2 = sdMiniWhale(x, y, 0.22, 0.01, 0.70);
      const w3 = sdMiniWhale(x, y, -0.06, 0.48, 0.60);
      if (w1.d <= w2.d && w1.d <= w3.d) return { d: w1.d, zone: w1.zone, part: 1 + w1.part };
      if (w2.d <= w3.d) return { d: w2.d, zone: w2.zone === 1 ? 2 : 1, part: 3 + w2.part };
      return { d: w3.d, zone: w3.zone, part: 5 + w3.part };
    }
    case 'waiting': {
      const crown = sdCircle(x, y, 0, -0.58, 0.072);
      const dome = sdCircle(x, y, 0, -0.16, 0.34);
      const skirt = polySdf(x, y, [[-0.34, -0.16], [0.34, -0.16], [0.50, 0.32], [-0.50, 0.32], [-0.34, -0.16]]);
      const lip = sdBox(x, y, 0, 0.33, 0.56, 0.062, 0.04);
      const bell = opUnion(crown, opSmoothUnion(opSub(dome, sdBox(x, y, 0, 0.6, 1, 0.28)), skirt, 0.06), lip);
      const clapper = sdCircle(x, y, 0, 0.54, 0.098);
      const arcs = opUnion(sdArc(x, y, 0, -0.04, 0.66, 0.036, 180, 50), sdArc(x, y, 0, -0.04, 0.66, 0.036, 0, 50));
      const d = opUnion(bell, clapper, arcs);
      if (clapper < bell && clapper < arcs) return { d, zone: 2, part: 2 };
      if (arcs < bell) return { d, zone: 2, part: 3 };
      return { d, zone: Math.abs(y - 0.33) < 0.09 ? 2 : 1, part: 1 };
    }
    case 'done': {
      const s1 = sdSegment(x, y, -0.50, 0.04, -0.12, 0.42, 0.118);
      const s2 = sdSegment(x, y, -0.12, 0.42, 0.54, -0.34, 0.118);
      return { d: Math.min(s1, s2), zone: s2 < s1 ? 2 : 1, part: 1 };
    }
  }
}

interface CompiledSlot {
  x: number;
  y: number;
  zone: number;
  part: number;
  rim: number;
  u: number;
}

function sampleHexLattice(kind: CompileKey, pitch: number): Array<CompiledSlot & { priority: number }> {
  const inset = 0.008;
  const G = 88;
  const step = 1.84 / G;
  const vals = new Float32Array((G + 1) * (G + 1));
  for (let j = 0; j <= G; j++) {
    const y = -0.92 + j * step;
    for (let i = 0; i <= G; i++) {
      vals[j * (G + 1) + i] = evalSdf(kind, -0.92 + i * step, y).d + inset;
    }
  }
  const raw: [number, number][] = [];
  const interp = (x1: number, y1: number, v1: number, x2: number, y2: number, v2: number): [number, number] => {
    const t = clamp(v1 / (v1 - v2 || 1e-9));
    return [x1 + (x2 - x1) * t, y1 + (y2 - y1) * t];
  };
  for (let j = 0; j < G; j++) {
    const y0 = -0.92 + j * step;
    const y1 = y0 + step;
    for (let i = 0; i < G; i++) {
      const x0 = -0.92 + i * step;
      const x1 = x0 + step;
      const v00 = vals[j * (G + 1) + i]!;
      const v10 = vals[j * (G + 1) + i + 1]!;
      const v01 = vals[(j + 1) * (G + 1) + i]!;
      const v11 = vals[(j + 1) * (G + 1) + i + 1]!;
      const mask = (v00 < 0 ? 1 : 0) | (v10 < 0 ? 2 : 0) | (v11 < 0 ? 4 : 0) | (v01 < 0 ? 8 : 0);
      if (mask === 0 || mask === 15) continue;
      const edges: [number, number][] = [];
      if ((v00 < 0) !== (v10 < 0)) edges.push(interp(x0, y0, v00, x1, y0, v10));
      if ((v10 < 0) !== (v11 < 0)) edges.push(interp(x1, y0, v10, x1, y1, v11));
      if ((v11 < 0) !== (v01 < 0)) edges.push(interp(x1, y1, v11, x0, y1, v01));
      if ((v01 < 0) !== (v00 < 0)) edges.push(interp(x0, y1, v01, x0, y0, v00));
      if (edges.length >= 2) raw.push([(edges[0]![0] + edges[1]![0]) * 0.5, (edges[0]![1] + edges[1]![1]) * 0.5]);
    }
  }
  raw.sort((a, b) => Math.atan2(a[1], a[0]) - Math.atan2(b[1], b[0]));
  const rims: Array<CompiledSlot & { priority: number }> = [];
  const minR2 = (pitch * 0.88) ** 2;
  for (const [px, py] of raw) {
    let ok = true;
    for (let k = 0; k < rims.length; k++) {
      const dx = px - rims[k]!.x;
      const dy = py - rims[k]!.y;
      if (dx * dx + dy * dy < minR2) {
        ok = false;
        break;
      }
    }
    if (!ok) continue;
    const s = evalSdf(kind, px, py);
    rims.push({ x: px, y: py, zone: s.zone, part: s.part, rim: 1, u: 0, priority: 2 });
  }
  const out = [...rims];
  const minI2 = (pitch * 0.74) ** 2;
  const dy = pitch * 0.8660254;
  let row = 0;
  for (let y = -0.90; y <= 0.90; y += dy, row++) {
    const xOff = (row & 1) ? pitch * 0.5 : 0;
    for (let x = -0.90 + xOff; x <= 0.90; x += pitch) {
      const s = evalSdf(kind, x, y);
      if (s.d > -inset) continue;
      let near = false;
      for (let k = 0; k < rims.length; k++) {
        const dx = x - rims[k]!.x;
        const ddy = y - rims[k]!.y;
        if (dx * dx + ddy * ddy < minI2) {
          near = true;
          break;
        }
      }
      if (near) continue;
      const rim = Math.exp(-Math.max(0, -s.d - inset) / 0.085);
      out.push({ x, y, zone: s.zone, part: s.part, rim, u: 0, priority: 1 + rim * 0.5 });
    }
  }
  return out;
}

const BODIES: Record<FormStyle, readonly (readonly number[])[]> = { smooth: DOT_BODY, pixel: DOT_PIXEL_BODY };
const HOME_RANKS: Record<FormStyle, number[]> = {
  smooth: DOT_BODY.map((p, i) => [p[0] * 0.78 + p[1] * 0.62, i] as const).sort((a, b) => a[0] - b[0] || a[1] - b[1]).map((v) => v[1]),
  pixel: DOT_PIXEL_BODY.map((p, i) => [p[0] * 0.78 + p[1] * 0.62, i] as const).sort((a, b) => a[0] - b[0] || a[1] - b[1]).map((v) => v[1]),
};

function compileKey(kind: CompileKey): { pitch: number; slots: CompiledSlot[] } {
  let lo = 0.015;
  let hi = 0.085;
  let best: Array<CompiledSlot & { priority: number }> = [];
  let bestPitch = (lo + hi) * 0.5;
  for (let iter = 0; iter < 12; iter++) {
    const mid = (lo + hi) * 0.5;
    const pts = sampleHexLattice(kind, mid);
    if (!best.length || Math.abs(pts.length - N) < Math.abs(best.length - N)) {
      best = pts;
      bestPitch = mid;
    }
    if (pts.length > N) lo = mid;
    else hi = mid;
  }
  if (best.length > N) {
    const rims = best.filter((p) => p.priority >= 2);
    const ints = best.filter((p) => p.priority < 2);
    if (rims.length >= N) {
      const st = rims.length / N;
      best = Array.from({ length: N }, (_, i) => rims[Math.min(rims.length - 1, Math.floor(i * st))]!);
    } else {
      const need = N - rims.length;
      const st = ints.length / need;
      best = [...rims, ...Array.from({ length: need }, (_, i) => ints[Math.min(ints.length - 1, Math.floor(i * st))]!)];
    }
  } else if (best.length > 0 && best.length < N) {
    const baseLen = best.length;
    for (let i = baseLen; i < N; i++) best.push({ ...best[(i * 73) % baseLen]! });
  }
  best.sort((a, b) => (a.x * 0.78 + a.y * 0.62) - (b.x * 0.78 + b.y * 0.62));
  return {
    pitch: bestPitch,
    slots: best.map((p, idx) => ({ x: p.x, y: p.y, zone: p.zone, part: p.part, rim: p.rim, u: idx / (N - 1) })),
  };
}

const CACHE = new Map<string, { pitch: number; slots: CompiledSlot[] }>();

export function getFormLayout(kind: FormKind, style: FormStyle, agents = 3): { pitch: number; slots: CompiledSlot[] } {
  const key: CompileKey = kind === 'delegating' ? (agents >= 3 ? 'delegating3' : 'delegating2') : kind;
  const cacheKey = `${style}:${key}`;
  const hit = CACHE.get(cacheKey);
  if (hit) return hit;
  const raw = compileKey(key);
  const rank = HOME_RANKS[style];
  const byHome = new Array<CompiledSlot>(N);
  for (let r = 0; r < N; r++) {
    byHome[rank[r]!] = raw.slots[r]!;
  }
  void BODIES;
  const layout = { pitch: raw.pitch, slots: byHome };
  CACHE.set(cacheKey, layout);
  return layout;
}

export function formPoint(kind: FormKind, style: FormStyle, i: number, t: number, agents = 3): FormPoint {
  const s = getFormLayout(kind, style, agents).slots[i]!;
  const { x: px, y: py, zone, part, rim, u } = s;

  switch (kind) {
    case 'thinking': {
      if (part === 1) return { x: px, y: py + 0.004 * Math.sin(t * 4.2), lit: 0.92, fade: 0.95 };
      if (part >= 2) {
        const idx = part - 2;
        const w = 0.5 + 0.5 * Math.sin(t * 4.2 - idx * 0.65);
        const dx = px;
        const dy = py + 0.14;
        const len = Math.hypot(dx, dy) || 1;
        return { x: px + (dx / len) * 0.032 * w, y: py + (dy / len) * 0.032 * w, lit: 0.88, fade: 0.55 + 0.45 * w };
      }
      return { x: px, y: py + 0.004 * Math.sin(t * 2.1), lit: 0.35 + rim * 0.35, fade: 0.78 + rim * 0.22 };
    }
    case 'reading': {
      const scanY = -0.32 + ((t * 0.42) % 1) * 0.76;
      const beam = Math.exp(-(((py - scanY) * 7.5) ** 2));
      const leaf = (part === 1 ? -0.016 : 0.016) * Math.sin(t * 2.2);
      return { x: px, y: py + leaf * Math.abs(px), lit: 0.28 + rim * 0.32 + beam * 0.45, fade: 0.74 + rim * 0.24 };
    }
    case 'searching': {
      const ox = 0.10 * Math.sin(t * 1.6);
      const oy = 0.07 * Math.sin(t * 2.3 + 0.8);
      if (part === 1) return { x: px + ox, y: py + oy, lit: zone === 2 ? 0.92 : 0.45 + rim * 0.35, fade: 0.90 };
      const glow = Math.exp(-(((Math.hypot(px - (-0.10 + ox), py - (-0.10 + oy)) - 0.38) * 4.5) ** 2));
      return { x: px, y: py, lit: mix(0.22, 0.82, glow), fade: mix(0.55, 0.92, glow) };
    }
    case 'editing': {
      const cyc = (t * 0.45) % 1;
      if (part === 1) {
        return { x: px + (cyc - 0.5) * 0.18, y: py + 0.014 * Math.sin(t * 9), lit: zone === 2 ? 0.92 : 0.38 + rim * 0.36, fade: 0.86 + rim * 0.14 };
      }
      const normX = clamp((px + 0.44) / 1.06);
      const drawn = smoothstep(normX - 0.08, normX + 0.02, 0.35 + cyc * 0.85);
      return { x: px, y: py + 0.004 * Math.sin(t * 3), lit: 0.82, fade: 0.45 + 0.55 * drawn };
    }
    case 'executing': {
      if (part === 2) return { x: px, y: py + 0.004 * Math.sin(t * 3.1), lit: 0.94, fade: 0.98 };
      const cyc = (t * 0.62) % 1;
      const ang = (cyc < 0.68 ? smoothstep(0, 0.68, cyc) * 20 - 10 : mix(10, -10, smoothstep(0.68, 1, cyc))) * DEG;
      const dx = px - 0.28;
      const dy = py + 0.28;
      return {
        x: 0.28 + dx * Math.cos(ang) - dy * Math.sin(ang),
        y: -0.28 + dx * Math.sin(ang) + dy * Math.cos(ang),
        lit: zone === 1 ? 0.65 : 0.35 + rim * 0.35,
        fade: 0.80 + rim * 0.20,
      };
    }
    case 'testing': {
      if (part === 2) {
        const rise = (t * 0.65 + u * 2.1) % 1;
        return { x: (px - 0.02) * 0.6 + 0.03 * Math.sin(rise * 8 + u * 5), y: mix(0.24, -0.78, rise), lit: 0.94, fade: 0.95 * (1 - smoothstep(0.82, 1, rise)) };
      }
      const wave = part === 1 ? 0.018 * Math.sin(px * 7 + t * 3.4) : 0.004 * Math.sin(t * 2.2);
      return { x: px, y: py + wave, lit: part === 1 ? 0.68 + rim * 0.22 : 0.36 + rim * 0.38, fade: 0.80 + rim * 0.20 };
    }
    case 'browsing': {
      if (part === 1 || part === 2) {
        const R = Math.sqrt(Math.max(0.01, 0.64 * 0.64 - py * py));
        const lon = (part === 1 ? 0.50 : -0.50) + 0.20 * Math.sin(t * 1.45);
        return { x: R * Math.sin(lon), y: py, lit: 0.65, fade: 0.88 };
      }
      return { x: px, y: py + 0.004 * Math.sin(t * 1.8), lit: 0.45 + rim * 0.35, fade: 0.82 + rim * 0.18 };
    }
    case 'computer': {
      const cyc = (t * 0.55) % 1;
      const press = cyc > 0.46 && cyc < 0.62 ? 0.03 : 0;
      if (part === 2) {
        const rp = clamp((cyc - 0.46) / 0.44);
        const sc = 0.88 + rp * 0.58;
        return { x: -0.24 + (px + 0.24) * sc, y: -0.48 + (py + 0.48) * sc, lit: 0.92, fade: cyc >= 0.46 ? (1 - rp * 0.75) * 0.95 : 0.30 };
      }
      const glide = part === 1 ? 0.036 * Math.sin(cyc * TAU) : 0.004 * Math.sin(t * 2);
      return { x: px + glide + press * 0.5, y: py + glide * 0.6 + press, lit: 0.52 + rim * 0.35, fade: 0.84 + rim * 0.16 };
    }
    case 'memory': {
      const layer = Math.max(0, part - 1);
      const w = 0.5 + 0.5 * Math.sin(t * 2.4 - layer * 1.05);
      return { x: px, y: py + 0.022 * Math.sin(t * 2.4 - layer * 0.9), lit: 0.35 + w * 0.42 + rim * 0.20, fade: 0.72 + w * 0.26 };
    }
    case 'responding': {
      if (part >= 1 && part <= 3) {
        const k = part - 1;
        return { x: px, y: py - 0.08 * Math.max(0, Math.sin(t * 4.0 - k * 0.95)), lit: 0.94, fade: 0.98 };
      }
      return { x: px, y: py + 0.015 * Math.sin(t * 1.8), lit: 0.35 + rim * 0.38, fade: 0.76 + rim * 0.22 };
    }
    case 'delegating': {
      const wIdx = Math.floor((part - 1) / 2);
      const fluke = (part - 1) % 2 === 1;
      return {
        x: px + 0.032 * Math.sin(t * 1.7 + wIdx * 1.9),
        y: py + 0.022 * Math.cos(t * 2.1 + wIdx * 1.6) + (fluke ? 0.036 * Math.sin(t * 5.2 + wIdx * 1.8) : 0),
        lit: zone >= 1 ? 0.86 : 0.40 + rim * 0.38,
        fade: 0.82 + rim * 0.18,
      };
    }
    case 'waiting': {
      if (part === 3) {
        const sd = px < 0 ? -1 : 1;
        const beat = 0.45 + 0.55 * Math.max(0, Math.sin(t * 5.2 * sd));
        return { x: px + sd * 0.026 * beat, y: py, lit: 0.92, fade: 0.55 + 0.45 * beat };
      }
      const ang = (part === 2 ? 14 : 10) * DEG * Math.sin(t * 5.2 - (part === 2 ? 0.45 : 0)) * (0.65 + 0.35 * Math.sin(t * 1.3));
      const dy = py + 0.58;
      return {
        x: px * Math.cos(ang) - dy * Math.sin(ang),
        y: -0.58 + px * Math.sin(ang) + dy * Math.cos(ang),
        lit: part === 2 ? 0.95 : 0.46 + rim * 0.38,
        fade: 0.84 + rim * 0.16,
      };
    }
    case 'done': {
      const pop = 1 + 0.06 * Math.exp(-t * 2.2) * Math.cos(t * 11) + 0.008 * Math.sin(t * 5.5);
      return { x: px * pop, y: py * pop, lit: 0.56 + rim * 0.36, fade: 0.88 + rim * 0.12 };
    }
  }
}
