#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SPEC = JSON.parse(readFileSync(join(HERE, 'source/spec.json'), 'utf8'));
const MARKDATA = JSON.parse(readFileSync(join(HERE, 'source/mark-parts.json'), 'utf8'));
const SOURCE_DIGEST = createHash('sha256').update(readFileSync(join(HERE, 'source/spec.json'))).update(readFileSync(join(HERE, 'source/mark-parts.json'))).update(readFileSync(join(HERE, 'source/mark-reversed.svg'))).digest('hex');
const STEPS = SPEC.steps;

const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const lerp = (a, b, t) => a + (b - a) * t;
const smooth = (a, b, v) => { const t = clamp((v - a) / (b - a), 0, 1); return t * t * (3 - 2 * t); };
const span = (a, b, n) => Array.from({ length: n + 1 }, (_, i) => a + ((b - a) * i) / n);
const fix = (n, d = 3) => +n.toFixed(d);
const endOf = (c) => (c[0] === 'C' ? [c[5], c[6]] : c[0] === 'M' ? [c[1], c[2]] : [0, 0]);
const pathString = (path) => path.map((c) => c[0] + c.slice(1).map((n) => fix(n)).join(' ')).join(' ');

export function parse(d) {
  const t = d.match(/[MCZ]|-?\d*\.?\d+(?:e-?\d+)?/g) ?? [], p = [];
  for (let i = 0; i < t.length;) { const op = t[i++], n = { M: 2, C: 6, Z: 0 }[op]; p.push([op, ...t.slice(i, i + n).map(Number)]); i += n; }
  return p;
}
function ellipse(x, y, rx, ry) {
  const k = 0.55228475;
  return [['M', x + rx, y], ['C', x + rx, y + ry * k, x + rx * k, y + ry, x, y + ry], ['C', x - rx * k, y + ry, x - rx, y + ry * k, x - rx, y], ['C', x - rx, y - ry * k, x - rx * k, y - ry, x, y - ry], ['C', x + rx * k, y - ry, x + rx, y - ry * k, x + rx, y], ['Z']];
}
function samplePoints(path) {
  const p = [];
  let last = [0, 0];
  for (const c of path) {
    if (c[0] === 'M') { last = [c[1], c[2]]; p.push(last); }
    else if (c[0] === 'C') {
      const a = last;
      for (let j = 1; j <= 10; j += 1) { const t = j / 10, q = 1 - t; p.push([q * q * q * a[0] + 3 * q * q * t * c[1] + 3 * q * t * t * c[3] + t * t * t * c[5], q * q * q * a[1] + 3 * q * q * t * c[2] + 3 * q * t * t * c[4] + t * t * t * c[6]]); }
      last = [c[5], c[6]];
    }
  }
  return p;
}
function measure(path) {
  let last = [0, 0], length = 0;
  const knots = [], samples = [];
  for (const c of path) {
    if (c[0] === 'M') { last = [c[1], c[2]]; samples.push({ l: 0, p: last, d: [1, 0] }); }
    else if (c[0] === 'C') {
      const a = last, l0 = length;
      let prev = a;
      for (let j = 1; j <= 30; j += 1) {
        const t = j / 30, q = 1 - t;
        const p = [q * q * q * a[0] + 3 * q * q * t * c[1] + 3 * q * t * t * c[3] + t * t * t * c[5], q * q * q * a[1] + 3 * q * q * t * c[2] + 3 * q * t * t * c[4] + t * t * t * c[6]];
        const d = [3 * q * q * (c[1] - a[0]) + 6 * q * t * (c[3] - c[1]) + 3 * t * t * (c[5] - c[3]), 3 * q * q * (c[2] - a[1]) + 6 * q * t * (c[4] - c[2]) + 3 * t * t * (c[6] - c[4])];
        length += Math.hypot(p[0] - prev[0], p[1] - prev[1]); samples.push({ l: length, p, d }); prev = p;
      }
      const seg = length - l0;
      knots.push({ l0, l1: length, h0: Math.hypot(c[1] - a[0], c[2] - a[1]) / (seg || 1), h1: Math.hypot(c[3] - c[5], c[4] - c[6]) / (seg || 1) });
      last = [c[5], c[6]];
    }
  }
  samples[0].d = samples[1].d;
  return { length, knots, samples };
}
function match(source, target) {
  const src = measure(source), dst = measure(target);
  const at = (u) => {
    const l = clamp(u, 0, 1) * dst.length;
    let i = 1;
    while (i < dst.samples.length - 1 && dst.samples[i].l < l) i += 1;
    const a = dst.samples[i - 1], b = dst.samples[i], t = (l - a.l) / (b.l - a.l || 1);
    const d = [lerp(a.d[0], b.d[0], t), lerp(a.d[1], b.d[1], t)], n = Math.hypot(...d) || 1;
    return { p: [lerp(a.p[0], b.p[0], t), lerp(a.p[1], b.p[1], t)], d: [d[0] / n, d[1] / n] };
  };
  const out = [['M', ...at(0).p]];
  for (const k of src.knots) {
    const a = at(k.l0 / src.length), b = at(k.l1 / src.length), sp = ((k.l1 - k.l0) / src.length) * dst.length, h0 = sp * Math.min(0.6, k.h0), h1 = sp * Math.min(0.6, k.h1);
    out.push(['C', a.p[0] + a.d[0] * h0, a.p[1] + a.d[1] * h0, b.p[0] - b.d[0] * h1, b.p[1] - b.d[1] * h1, ...b.p]);
  }
  out.push(['Z']);
  return out;
}
function signedArea(path) {
  const p = samplePoints(path);
  return p.reduce((sum, a, i) => { const b = p[(i + 1) % p.length]; return sum + a[0] * b[1] - b[0] * a[1]; }, 0) / 2;
}
function orientLike(source, path) {
  if (signedArea(source) * signedArea(path) >= 0) return path;
  const start = endOf(path[0]), segments = [];
  let at = start;
  for (const c of path) if (c[0] === 'C') { segments.push({ at, c }); at = [c[5], c[6]]; }
  if (Math.hypot(at[0] - start[0], at[1] - start[1]) > 0.01) segments.push({ at, c: ['C', ...at, ...start, ...start] });
  return [['M', ...start], ...segments.reverse().map(({ at: a, c }) => ['C', c[3], c[4], c[1], c[2], ...a]), ['Z']];
}
const fitPath = (source, target) => match(source, orientLike(source, target));
function pieces(source, stops, targets) {
  let begin = 0;
  const result = [];
  for (let i = 0; i < stops.length; i += 1) {
    const last = stops[i], section = begin === 0 ? source.slice(0, last + 1) : [['M', ...endOf(source[begin])], ...source.slice(begin + 1, last + 1)];
    const fitted = match(section, parse(targets[i]));
    result.push(...fitted.slice(i ? 1 : 0, -1));
    begin = last;
  }
  return [...result, ['Z']];
}

function walk(anchor, heading, length, kappa) {
  const pts = [anchor], theta = new Float64Array(STEPS + 1);
  theta[0] = heading;
  for (let i = 0; i < STEPS; i += 1) {
    theta[i + 1] = theta[i] + kappa[i] / STEPS;
    const mid = (theta[i] + theta[i + 1]) / 2, [x, y] = pts[i];
    pts.push([x + (Math.cos(mid) * length) / STEPS, y + (Math.sin(mid) * length) / STEPS]);
  }
  return { anchor, heading, length, kappa, pts, theta };
}
function place(sp, s, d) {
  const inside = clamp(s, 0, 1), f = inside * STEPS, i = Math.min(STEPS - 1, Math.floor(f)), t = f - i;
  const [ax, ay] = sp.pts[i], [bx, by] = sp.pts[i + 1];
  const theta = lerp(sp.theta[i], sp.theta[i + 1], t), tx = Math.cos(theta), ty = Math.sin(theta), over = (s - inside) * sp.length;
  return [lerp(ax, bx, t) + tx * over + ty * d, lerp(ay, by, t) + ty * over - tx * d];
}
function project(sp, [x, y]) {
  let best = Infinity, index = 0;
  sp.pts.forEach(([px, py], i) => { const q = (px - x) ** 2 + (py - y) ** 2; if (q < best) { best = q; index = i; } });
  const theta = sp.theta[index], tx = Math.cos(theta), ty = Math.sin(theta), [px, py] = sp.pts[index];
  return [index / STEPS + ((x - px) * tx + (y - py) * ty) / sp.length, (x - px) * ty - (y - py) * tx];
}
function traced(points) {
  const dense = [];
  for (let i = 0; i < points.length - 1; i += 1) {
    const p0 = points[Math.max(0, i - 1)], p1 = points[i], p2 = points[i + 1], p3 = points[Math.min(points.length - 1, i + 2)];
    for (let k = 0; k < 24; k += 1) {
      const t = k / 24, t2 = t * t, t3 = t2 * t;
      const axis = (a) => 0.5 * (2 * p1[a] + (p2[a] - p0[a]) * t + (2 * p0[a] - 5 * p1[a] + 4 * p2[a] - p3[a]) * t2 + (3 * p1[a] - p0[a] - 3 * p2[a] + p3[a]) * t3);
      dense.push([axis(0), axis(1)]);
    }
  }
  dense.push(points.at(-1));
  const lengths = [0];
  for (let i = 1; i < dense.length; i += 1) lengths.push(lengths[i - 1] + Math.hypot(dense[i][0] - dense[i - 1][0], dense[i][1] - dense[i - 1][1]));
  const length = lengths.at(-1);
  let j = 0;
  const nodes = span(0, length, STEPS).map((l) => {
    while (j < dense.length - 2 && lengths[j + 1] < l) j += 1;
    const t = (l - lengths[j]) / (lengths[j + 1] - lengths[j] || 1);
    return [lerp(dense[j][0], dense[j + 1][0], t), lerp(dense[j][1], dense[j + 1][1], t)];
  });
  const heading = nodes.slice(1).map(([x, y], i) => Math.atan2(y - nodes[i][1], x - nodes[i][0]));
  for (let i = 1; i < heading.length; i += 1) heading[i] = heading[i - 1] + Math.atan2(Math.sin(heading[i] - heading[i - 1]), Math.cos(heading[i] - heading[i - 1]));
  const kappa = Float64Array.from({ length: STEPS }, (_, i) => { const a = Math.max(0, i - 3), b = Math.min(STEPS - 1, i + 3); return ((heading[b] - heading[a]) / (b - a)) * STEPS; });
  return walk(nodes[0], heading[0], length, kappa);
}
function profile(points) {
  const xs = points.map((p) => p[0]), ys = points.map((p) => p[1]), n = xs.length;
  const h = xs.slice(1).map((x, i) => x - xs[i]), d = h.map((w, i) => (ys[i + 1] - ys[i]) / w);
  const m = xs.map((_, i) => {
    if (i === 0) return d[0];
    if (i === n - 1) return d[n - 2];
    if (d[i - 1] * d[i] <= 0) return 0;
    const w1 = 2 * h[i] + h[i - 1], w2 = h[i] + 2 * h[i - 1];
    return (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]);
  });
  return (s) => {
    if (s <= xs[0]) return ys[0];
    if (s >= xs[n - 1]) return ys[n - 1];
    let i = 0;
    while (s > xs[i + 1]) i += 1;
    const t = (s - xs[i]) / h[i], t2 = t * t, t3 = t2 * t;
    return (2 * t3 - 3 * t2 + 1) * ys[i] + (t3 - 2 * t2 + t) * h[i] * m[i] + (3 * t2 - 2 * t3) * ys[i + 1] + (t3 - t2) * h[i] * m[i + 1];
  };
}
function curve(pts) {
  const at = (i) => pts[clamp(i, 0, pts.length - 1)];
  let d = `M${fix(pts[0][0])} ${fix(pts[0][1])}`;
  for (let i = 0; i < pts.length - 1; i += 1) {
    const a = at(i - 1), b = at(i), c = at(i + 1), e = at(i + 2);
    d += ` C${fix(b[0] + (c[0] - a[0]) / 6)} ${fix(b[1] + (c[1] - a[1]) / 6)} ${fix(c[0] - (e[0] - b[0]) / 6)} ${fix(c[1] - (e[1] - b[1]) / 6)} ${fix(c[0])} ${fix(c[1])}`;
  }
  return d;
}

const shape = (id) => MARKDATA.shapes.find((s) => s.id === id);
const W = SPEC.whale, L = W.length;
const MARK = traced(SPEC.mark.spine);
const SWIM = walk(W.anchor, W.heading, L, Float64Array.from({ length: STEPS }, (_, i) => W.arch.bend * smooth(W.arch.from, W.arch.to, (i + 0.5) / STEPS)));
const dor = profile(W.dorsal), ven = profile(W.ventral);
const at = (s, v) => place(SWIM, s, v * L);
const belly = (a, b, n) => span(a, b, n).map((s) => at(s, ven(s)));
const back = (a, b, n) => span(a, b, n).map((s) => at(s, -dor(s)));
const lip = (dor(W.nose.at) + ven(W.nose.at)) / 2, chin = (ven(W.nose.at) - dor(W.nose.at)) / 2;
const snout = span(Math.PI, 0, 8).map((phi) => at(W.nose.at - W.nose.reach * Math.sin(phi) ** 0.8 * (phi > Math.PI / 2 ? W.nose.chin : 1), chin - lip * Math.cos(phi)));
const g = W.gape, th = W.throat, front = W.nose.at + 0.006;
const mouth = (s) => g.from + (g.to - g.from) * (s / g.at) ** g.bend;
const flank = (s) => g.to + (ven(s) - th.inset - g.to) * ((s - g.at) / (th.end - g.at)) ** th.drop;
const sliver = ([a, b, depth, width], side, from, to) => span(from, to, 40).map((t) => { const s = a + (b - a) * t; return at(s, ven(s) * depth + side * width * Math.sin(Math.PI * t) ** 0.9); });
const fin = W.fin, turn = (fin.angle * Math.PI) / 180;
const blade = (u, v) => at(fin.at + u * Math.cos(turn) - v * Math.sin(turn), fin.v + u * Math.sin(turn) + v * Math.cos(turn));
const D = W.dorsalFin;
const F = W.flukes, roll = Math.sin((F.roll * Math.PI) / 180), dip = Math.sin((F.pitch * Math.PI) / 180), run = Math.cos((F.pitch * Math.PI) / 180);
const fluke = ([u0, w0], side) => { const u = u0 * F.scale, w = w0 * F.scale, near = side < 0 ? 1 + F.near : 1 - F.near; return at(1 + u * run * near - F.inset, (-w * side * roll - u * dip) * near); };
const flukeTip = F.lead.at(-1), notch = [F.trail.at(-1)[0], 0];
const [eyeX, eyeY] = at(W.eye.at, W.eye.v);
const SEC = W.sections;

const GOALS = {
  body: pieces(shape('body').path, SEC.body, [
    curve(belly(1, W.blade[0], 60)),
    curve(belly(W.blade[0], W.blade[1], 10)),
    curve([...belly(W.blade[1], W.nose.at, 40), ...snout.slice(1)]),
    curve(back(W.nose.at, 1, 90)),
    curve([at(1, -dor(1)), at(1 + W.cap, 0), at(1, ven(1))]),
  ]),
  pouch: pieces(shape('pouch').path, SEC.pouch, [
    curve([at(front, mouth(front)), ...span(front, th.end, 60).map((s) => at(s, ven(s) - th.inset))]),
    curve([...span(th.end, g.at, 50).map((s) => at(s, flank(s))), ...span(g.at, front, 40).slice(1).map((s) => at(s, mouth(s)))]),
  ]),
  'pleat-1': pieces(shape('pleat-1').path, SEC['pleat-1'], [curve(sliver(W.pleats[0], -1, 0, 1)), curve(sliver(W.pleats[0], 1, 1, 0))]),
  'pleat-2': pieces(shape('pleat-2').path, SEC['pleat-2'], [curve(sliver(W.pleats[1], -1, 0, 1)), curve(sliver(W.pleats[1], 1, 1, 0))]),
  eye: fitPath(shape('eye').path, ellipse(eyeX, eyeY, W.eye.r * L, W.eye.r * L * 0.78)),
  flipper: pieces(shape('flipper').path, SEC.flipper, [
    curve([blade(0, -fin.width * 0.35), ...span(0.02, 1, 30).map((t) => blade(fin.length * t, -fin.width * Math.sin(Math.PI * t) ** 0.62 * (1 - 0.3 * t)))]),
    curve([...span(1, 0.02, 30).map((t) => blade(fin.length * t, fin.width * fin.lead * Math.sin(Math.PI * t) ** 0.5 * (1 - 0.2 * t))), blade(0, fin.width * 0.2), blade(0, -fin.width * 0.35)]),
  ]),
  flukes: pieces(shape('flukes').path, SEC.flukes, [
    curve(F.lead.map((p) => fluke(p, 1))),
    curve([flukeTip, ...F.trail.slice(0, -1), notch].map((p) => fluke(p, 1))),
    curve([notch, ...F.trail.slice(0, -1).reverse(), flukeTip].map((p) => fluke(p, -1))),
    curve([...F.lead].reverse().map((p) => fluke(p, -1))),
    curve([fluke(F.lead[0], -1), fluke(F.lead[0], 1)]),
  ]),
};
const DORSAL_PATH = parse(`${curve([
  ...back(D.at - D.base / 2, D.at + D.base / 2, 24).reverse(),
  at(D.at - D.base * 0.05, -dor(D.at) - D.height * 0.62),
  at(D.at + D.base / 2 + D.sweep, -dor(D.at) - D.height),
  at(D.at + D.base / 2 - 0.004, -dor(D.at + D.base / 2) - D.height * 0.1),
  at(D.at + D.base / 2, -dor(D.at + D.base / 2) + 0.002),
])} Z`);

function track(from, to, home) {
  const out = [];
  from.forEach((command, index) => {
    const goal = to[index];
    for (let axis = 1; axis < command.length; axis += 2) {
      const t = [goal[axis], goal[axis + 1]], [st, dt] = project(SWIM, t), pt = place(SWIM, st, dt);
      const [sm, dm] = home ? home([st, dt]) : project(MARK, [command[axis], command[axis + 1]]);
      const m = home ? place(MARK, sm, dm) : [command[axis], command[axis + 1]], pm = place(MARK, sm, dm);
      out.push(sm, dm, m[0] - pm[0], m[1] - pm[1], st, dt, t[0] - pt[0], t[1] - pt[1]);
    }
  });
  return out;
}
const TRACKS = Object.fromEntries(Object.entries(GOALS).map(([id, goal]) => [id, { path: shape(id).path, coords: track(shape(id).path, goal) }]));
TRACKS.dorsal = { path: DORSAL_PATH, coords: track(DORSAL_PATH, DORSAL_PATH, ([s]) => [s, 0]) };
const BEAT = (() => {
  const weight = Float64Array.from({ length: STEPS }, (_, i) => smooth(W.beatSpan[0], W.beatSpan[1], (i + 0.5) / STEPS));
  const mean = weight.reduce((sum, v) => sum + v, 0) / STEPS;
  return weight.map((v) => v / mean);
})();

export function spineAt(amount, beat = 0) {
  const flex = ((beat * Math.PI) / 180) * Math.max(0, amount);
  return walk([lerp(MARK.anchor[0], SWIM.anchor[0], amount), lerp(MARK.anchor[1], SWIM.anchor[1], amount)], lerp(MARK.heading, SWIM.heading, amount), lerp(MARK.length, SWIM.length, amount), MARK.kappa.map((k, i) => lerp(k, SWIM.kappa[i], amount) + flex * BEAT[i]));
}
function morph({ path, coords }, sp, amount) {
  let k = 0;
  return path.map((command) => {
    if (command[0] === 'Z') return ['Z'];
    const values = [];
    for (let axis = 1; axis < command.length; axis += 2, k += 8) {
      const [x, y] = place(sp, lerp(coords[k], coords[k + 4], amount), lerp(coords[k + 1], coords[k + 5], amount));
      values.push(x + lerp(coords[k + 2], coords[k + 6], amount), y + lerp(coords[k + 3], coords[k + 7], amount));
    }
    return [command[0], ...values];
  });
}
export function frame(amount, beat = 0) {
  const sp = amount === 0 && beat === 0 ? null : spineAt(amount, beat);
  const parts = {};
  for (const s of MARKDATA.shapes) parts[s.id] = TRACKS[s.id] && sp ? morph(TRACKS[s.id], sp, amount) : s.path;
  parts.dorsal = morph(TRACKS.dorsal, sp ?? MARK, amount);
  return parts;
}
const boxOf = (paths) => {
  const xy = paths.flatMap((p) => p.flatMap((c) => c.slice(1)));
  const xs = xy.filter((_, i) => i % 2 === 0), ys = xy.filter((_, i) => i % 2 === 1);
  return [Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys)];
};
const SWIM_BOX = (() => { const [x0, y0, x1, y1] = boxOf([GOALS.body, GOALS.flukes, GOALS.flipper, DORSAL_PATH]); return [x0 - 6, y0 - 6, x1 - x0 + 12, y1 - y0 + 12]; })();

function flatten(path) {
  const pts = [];
  let last = [0, 0];
  for (const c of path) {
    if (c[0] === 'M') { last = [c[1], c[2]]; pts.push(last); }
    else if (c[0] === 'C') {
      const a = last;
      for (let j = 1; j <= 10; j += 1) { const t = j / 10, q = 1 - t; pts.push([q * q * q * a[0] + 3 * q * q * t * c[1] + 3 * q * t * t * c[3] + t * t * t * c[5], q * q * q * a[1] + 3 * q * q * t * c[2] + 3 * q * t * t * c[4] + t * t * t * c[6]]); }
      last = [c[5], c[6]];
    }
  }
  return pts;
}
function inside(poly, x, y) {
  let hit = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i, i += 1) { const [xi, yi] = poly[i], [xj, yj] = poly[j]; if ((yi > y) !== (yj > y) && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) hit = !hit; }
  return hit;
}
function silhouette(parts, form) {
  const ids = form === 'mark' ? ['body', 'flukes'] : ['body', 'flukes', 'dorsal', 'flipper'];
  return { body: ids.map((id) => flatten(parts[id])), throat: flatten(parts.pouch), pleats: ['pleat-1', 'pleat-2'].map((id) => flatten(parts[id])), eye: flatten(parts.eye) };
}
function classify(P, x, y) {
  if (inside(P.eye, x, y)) return 'eye';
  if (P.pleats.some((p) => inside(p, x, y))) return 'pleat';
  if (inside(P.throat, x, y)) return 'throat';
  if (P.body.some((p) => inside(p, x, y))) return 'body';
  return null;
}
function lattice(P, { cols, pitch, box, coverage = SPEC.drones.coverage }) {
  const bodyPts = P.body.flat();
  const [x0, y0, x1, y1] = box ?? [Math.min(...bodyPts.map((p) => p[0])), Math.min(...bodyPts.map((p) => p[1])), Math.max(...bodyPts.map((p) => p[0])), Math.max(...bodyPts.map((p) => p[1]))];
  const p = pitch ?? (x1 - x0) / cols, nx = Math.ceil((x1 - x0) / p), ny = Math.ceil((y1 - y0) / p);
  const ox = x0 + ((x1 - x0) - nx * p) / 2, oy = y0 + ((y1 - y0) - ny * p) / 2, sub = [1 / 6, 3 / 6, 5 / 6], dots = [];
  for (let j = 0; j < ny; j += 1) for (let i = 0; i < nx; i += 1) {
    const counts = {};
    let filled = 0;
    for (const u of sub) for (const v of sub) { const c = classify(P, ox + (i + u) * p, oy + (j + v) * p); if (c) { counts[c] = (counts[c] ?? 0) + 1; filled += 1; } }
    if (filled / 9 < coverage) continue;
    let cls = Object.entries(counts).sort((a, b) => b[1] - a[1])[0][0];
    if (cls === 'pleat' && counts.pleat < 3) cls = 'throat';
    if (cls === 'eye') cls = 'body';
    dots.push({ i, j, x: ox + (i + 0.5) * p, y: oy + (j + 0.5) * p, cls });
  }
  const eyeBox = boxOf([[['M', ...P.eye[0]], ...P.eye.map((q) => ['M', ...q])]]), ex = (eyeBox[0] + eyeBox[2]) / 2, ey = (eyeBox[1] + eyeBox[3]) / 2;
  let best = null, dist = Infinity;
  for (const dot of dots) { const q = (dot.x - ex) ** 2 + (dot.y - ey) ** 2; if (q < dist) { dist = q; best = dot; } }
  if (best && Math.sqrt(dist) < p * 1.5) best.cls = 'eye';
  return { pitch: p, nx, ny, ox, oy, dots };
}

const MARK_PARTS = frame(0), WHALE_PARTS = frame(1);
const LETTER = { body: 'b', throat: 't', pleat: 'p', eye: 'e' };
const rowsOf = (Lt) => {
  const grid = Array.from({ length: Lt.ny }, () => Array.from({ length: Lt.nx }, () => '.'));
  for (const d of Lt.dots) grid[d.j][d.i] = LETTER[d.cls];
  return grid.map((r) => r.join(''));
};
const order = (Lt) => Lt.dots.filter((d) => d.cls !== 'eye').sort((a, b) => a.j - b.j || a.i - b.i);
const pack = (Lt) => ({ cols: Lt.nx, pitch: fix(Lt.pitch, 4), origin: [fix(Lt.ox, 3), fix(Lt.oy, 3)], count: order(Lt).length, rows: rowsOf(Lt) });
const DR = SPEC.drones;
const MARK_SIL = silhouette(MARK_PARTS, 'mark'), WHALE_SIL = silhouette(WHALE_PARTS, 'whale');
const LAUNCH = DR.launch.densities.map((cols) => lattice(MARK_SIL, { cols }));
const SIDEBAR = DR.sidebar.densities.map((cols) => lattice(MARK_SIL, { cols }));
const WHALES = DR.whale.densities.map((cols) => lattice(WHALE_SIL, { cols }));

function signFormation(rows, base) {
  const live = order(base);
  const xs = live.map((d) => d.x), ys = live.map((d) => d.y);
  const x0 = Math.min(...xs), x1 = Math.max(...xs), y0 = Math.min(...ys), y1 = Math.max(...ys);
  const side = Math.min(x1 - x0, y1 - y0) * 0.86, cx = (x0 + x1) / 2, cy = (y0 + y1) / 2, n = rows.length;
  const cells = [];
  for (let j = 0; j < base.ny; j += 1) for (let i = 0; i < base.nx; i += 1) {
    const x = base.ox + (i + 0.5) * base.pitch, y = base.oy + (j + 0.5) * base.pitch;
    const gx = Math.floor(((x - (cx - side / 2)) / side) * n), gy = Math.floor(((y - (cy - side / 2)) / side) * n);
    if (gx >= 0 && gy >= 0 && gx < n && gy < n && rows[gy][gx] === '#') cells.push([i, j, x, y]);
  }
  const taken = new Set(), from = new Array(live.length).fill(-1);
  const byDistance = cells.map((c, k) => k).sort((a, b) => Math.hypot(cells[a][2] - cx, cells[a][3] - cy) - Math.hypot(cells[b][2] - cx, cells[b][3] - cy));
  for (const k of byDistance) {
    let best = -1, d = Infinity;
    live.forEach((dot, index) => { if (taken.has(index)) return; const q = (dot.x - cells[k][2]) ** 2 + (dot.y - cells[k][3]) ** 2; if (q < d) { d = q; best = index; } });
    if (best >= 0) { taken.add(best); from[best] = k; }
  }
  return { cells: cells.map(([i, j]) => [i, j]), from };
}

const IND_BEATS = Array.from({ length: DR.indicator.frames }, (_, k) => fix(DR.indicator.amplitude * Math.sin((2 * Math.PI * k) / DR.indicator.frames), 4));
const IND_PARTS = IND_BEATS.map((b) => frame(1, b));
const IND_BOX = (() => {
  const [x0, y0, x1, y1] = boxOf(IND_PARTS.flatMap((p) => ['body', 'flukes', 'dorsal', 'flipper'].map((id) => p[id])));
  const pad = (x1 - x0) / 40;
  return [x0 - pad, y0 - pad * 4, x1 + pad, y1 + pad];
})();
const INDICATOR = DR.indicator.densities.map((cols) => {
  const pitch = (IND_BOX[2] - IND_BOX[0]) / cols;
  const frames = IND_PARTS.map((parts) => lattice(silhouette(parts, 'whale'), { pitch, box: IND_BOX }));
  const at = [IND_BOX[2] - pitch * 4, IND_BOX[1] + pitch * 1.6];
  const lantern = [[0, 0], [1, 0], [-1, 0], [0.5, 0.87], [-0.5, 0.87], [0.5, -0.87], [-0.5, -0.87]].slice(0, DR.indicator.lanternDrones).map(([u, v]) => [fix(at[0] + u * pitch * 0.9, 3), fix(at[1] + v * pitch * 0.9, 3)]);
  return { cols, pitch: fix(pitch, 4), origin: [fix(frames[0].ox, 3), fix(frames[0].oy, 3)], frames: frames.map(rowsOf), counts: frames.map((f) => order(f).length), lantern };
});

function brailleRows(Lt) {
  const bit = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]], on = new Set(Lt.dots.filter((d) => d.cls !== 'eye').map((d) => `${d.i},${d.j}`)), rows = [];
  for (let cy = 0; cy < Lt.ny; cy += 4) {
    let line = '';
    for (let cx = 0; cx < Lt.nx; cx += 2) { let code = 0; for (let r = 0; r < 4; r += 1) for (let c = 0; c < 2; c += 1) if (on.has(`${cx + c},${cy + r}`)) code |= bit[r][c]; line += String.fromCodePoint(0x2800 + code); }
    rows.push(line);
  }
  return rows;
}
const TERM_LAUNCH = lattice(MARK_SIL, { cols: DR.terminalLaunch.cols });
const termIndBox = boxOf(DR.terminalIndicator.beats.map((b) => frame(1, b)).flatMap((p) => ['body', 'flukes'].map((id) => p[id])));
const TERM_IND = DR.terminalIndicator.beats.map((b) => lattice(silhouette(frame(1, b), 'whale'), { cols: DR.terminalIndicator.cols, box: termIndBox, coverage: DR.terminalIndicator.coverage }));

const head = (kind) => ({ schema: 1, kind, source: `codewhale-design/whale/source sha256 ${SOURCE_DIGEST}`, note: 'Generated by codewhale-design/whale/generate.mjs. Do not edit.' });
const r6 = (a) => Array.from(a, (v) => fix(v, 6));
const OUTPUTS = {};
OUTPUTS['out/whale-geometry.json'] = JSON.stringify({
  ...head('geometry'), steps: STEPS,
  mark: { anchor: r6(MARK.anchor), heading: fix(MARK.heading, 9), length: fix(MARK.length, 6), kappa: r6(MARK.kappa), box: SPEC.mark.box },
  swim: { anchor: SWIM.anchor, heading: SWIM.heading, length: SWIM.length, kappa: r6(SWIM.kappa), box: SWIM_BOX.map((v) => fix(v, 3)) },
  beat: r6(BEAT),
  parts: Object.fromEntries(Object.entries(TRACKS).map(([id, t]) => [id, { commands: t.path.map((c) => c[0]).join(''), mark: id === 'dorsal' ? null : t.path.map((c) => c.slice(1).map((v) => fix(v, 6))), coords: r6(t.coords) }])),
});
OUTPUTS['out/whale-drones.json'] = JSON.stringify({
  ...head('drones'),
  encoding: 'Each density lists rows of lattice cells: "." empty, "b" body, "t" throat, "p" pleat, "e" the eye (no drone). A cell at column i, row j sits at origin + (i + 0.5, j + 0.5) * pitch, in mark units. Drones are numbered in row-major order, skipping "." and "e". Pick the densest entry whose drones stay at least minPitchPx device pixels apart.',
  radius: SPEC.drones.radius, minPitchPx: DR.minPitchPx,
  launch: LAUNCH.map(pack),
  sidebar: SIDEBAR.map((Lt) => ({ ...pack(Lt), signs: Object.fromEntries(['waiting', 'blocked', 'complete'].map((name) => [name, signFormation(DR.signs[name], Lt)])) })),
  whale: { box: SWIM_BOX.map((v) => fix(v, 3)), densities: WHALES.map(pack) },
  indicator: { box: IND_BOX.map((v) => fix(v, 3)), period: 'loop_breathe_ms', beats: IND_BEATS, densities: INDICATOR },
});
OUTPUTS['out/whale-braille.json'] = JSON.stringify({
  ...head('braille'),
  launch: { cells: [Math.ceil(TERM_LAUNCH.nx / 2), Math.ceil(TERM_LAUNCH.ny / 4)], rows: brailleRows(TERM_LAUNCH) },
  indicator: { beats: DR.terminalIndicator.beats, cells: [Math.ceil(TERM_IND[0].nx / 2), Math.ceil(TERM_IND[0].ny / 4)], frames: TERM_IND.map(brailleRows) },
}, null, 1);

const P = SPEC.palette;
const grad = (id, [a, b], box) => `<linearGradient id="${id}" gradientUnits="userSpaceOnUse" x1="${fix(box[0])}" y1="${fix(box[1])}" x2="${fix(box[0] + box[2])}" y2="${fix(box[1] + box[3])}"><stop stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient>`;
function coloredSvg(parts, box, pal, ids) {
  const fill = 'url(#body)', path = (id, f, extra = '') => `<path id="${id}" d="${pathString(parts[id])}" fill="${f}"${extra}/>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${box.map((v) => fix(v)).join(' ')}"><defs>${grad('body', pal.body, box)}</defs>${ids.back.map((id) => path(id, fill)).join('')}${path('pouch', pal.throat)}${['pleat-1', 'pleat-2'].map((id) => path(id, pal.pleat, ' opacity=".78"')).join('')}${ids.front.map((id) => path(id, fill)).join('')}${path('eye', pal.eye)}</svg>\n`;
}
function dronesSvg(Lt, box, pal) {
  const b = [Lt.ox, Lt.oy, Lt.ox + Lt.nx * Lt.pitch, Lt.oy + Lt.ny * Lt.pitch], body = pal.dronesBody ?? pal.body, r = fix(SPEC.drones.radius * Lt.pitch, 3);
  const color = (d) => {
    if (d.cls === 'throat') return pal.dronesThroat;
    if (d.cls === 'pleat') return pal.pleat;
    const t = clamp(((d.x - b[0]) / (b[2] - b[0]) + (d.y - b[1]) / (b[3] - b[1])) / 2, 0, 1);
    return '#' + [0, 1, 2].map((k) => { const a = parseInt(body[0].slice(1 + 2 * k, 3 + 2 * k), 16), z = parseInt(body[1].slice(1 + 2 * k, 3 + 2 * k), 16); return Math.round(a + (z - a) * t).toString(16).padStart(2, '0'); }).join('');
  };
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${box.map((v) => fix(v)).join(' ')}">${Lt.dots.filter((d) => d.cls !== 'eye').map((d) => `<circle cx="${fix(d.x)}" cy="${fix(d.y)}" r="${r}" fill="${color(d)}"/>`).join('')}</svg>\n`;
}
const markReversed = readFileSync(join(HERE, 'source/mark-reversed.svg'), 'utf8').match(/ d="([^"]+)"/)[1];
function squircle(cx, cy, a, n = 5, steps = 240) {
  const pts = [];
  for (let k = 0; k < steps; k += 1) { const t = (k / steps) * Math.PI * 2, c = Math.cos(t), s = Math.sin(t); pts.push([cx + a * Math.sign(c) * Math.abs(c) ** (2 / n), cy + a * Math.sign(s) * Math.abs(s) ** (2 / n)]); }
  return 'M' + pts.map((q) => q.map((v) => v.toFixed(2)).join(' ')).join(' L') + ' Z';
}
function iconSvg(look) {
  const I = SPEC.icon, tile = look === 'light' ? I.tile : I[look].tile, mark = look === 'light' ? I.mark : I[look].mark, sq = squircle(512, 512, 412);
  const sheen = look === 'tinted' ? '' : `<path d="${sq}" fill="url(#sheen)"/>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024"><defs><linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1"><stop stop-color="#fff" stop-opacity="${I.sheen}"/><stop offset=".5" stop-color="#fff" stop-opacity="0"/></linearGradient><filter id="drop" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur in="SourceAlpha" stdDeviation="14"/><feOffset dy="12"/><feComponentTransfer><feFuncA type="linear" slope=".3"/></feComponentTransfer><feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter></defs><g filter="url(#drop)"><path d="${sq}" fill="${tile}"/>${sheen}</g><path transform="translate(512 518) scale(${I.markScale}) translate(-256 -256.5)" d="${markReversed}" fill="${mark}" fill-rule="evenodd"/></svg>\n`;
}
const whaleIds = { back: ['flukes', 'dorsal', 'body'], front: ['flipper'] };
const markBox = SPEC.mark.box;
for (const theme of ['light', 'dark']) {
  const sfx = theme === 'light' ? '' : '-dark';
  OUTPUTS[`svg/mark-parts${sfx}.svg`] = coloredSvg(MARK_PARTS, markBox, P[theme], { back: ['flukes', 'body'], front: [] });
  OUTPUTS[`svg/whale-b${sfx}.svg`] = coloredSvg(WHALE_PARTS, SWIM_BOX, P[theme], whaleIds);
  OUTPUTS[`svg/drones-launch${sfx}.svg`] = dronesSvg(LAUNCH.at(-3), markBox, P[theme]);
  OUTPUTS[`svg/drones-sidebar${sfx}.svg`] = dronesSvg(SIDEBAR.at(-1), markBox, P[theme]);
  OUTPUTS[`svg/drones-whale${sfx}.svg`] = dronesSvg(WHALES.at(-1), SWIM_BOX, P[theme]);
  const ib = [IND_BOX[0], IND_BOX[1], IND_BOX[2] - IND_BOX[0], IND_BOX[3] - IND_BOX[1]], top = INDICATOR.at(-1);
  const indLattice = IND_PARTS.map((parts) => lattice(silhouette(parts, 'whale'), { pitch: top.pitch, box: IND_BOX }));
  indLattice.forEach((Lt, k) => { OUTPUTS[`svg/fluke-${k}${sfx}.svg`] = dronesSvg(Lt, ib, P[theme]); });
  OUTPUTS[`svg/fluke-waiting${sfx}.svg`] = dronesSvg(indLattice[0], ib, P[theme]).replace('</svg>', `${top.lantern.map(([x, y]) => `<circle cx="${x}" cy="${y}" r="${fix(SPEC.drones.radius * top.pitch, 3)}" fill="${P[theme].lantern}"/>`).join('')}</svg>`);
}
const POSES = [['uncurl-0', 0, 0], ['uncurl-25', 0.25, 0], ['uncurl-50', 0.5, 0], ['uncurl-75', 0.75, 0], ['uncurl-100', 1, 0], ['stroke-up', 1, SPEC.poses.stroke], ['stroke-down', 1, -SPEC.poses.stroke]];
const POSE_PARTS = POSES.map(([, amount, beat]) => frame(amount, beat));
const POSE_BOX = (() => { const [x0, y0, x1, y1] = boxOf(POSE_PARTS.flatMap((p) => ['body', 'flukes', 'dorsal', 'flipper'].map((id) => p[id]))); return [x0 - 6, y0 - 6, x1 - x0 + 12, y1 - y0 + 12]; })();
const shut = (eye) => { const [, y0, , y1] = boxOf([eye]), cy = (y0 + y1) / 2; return eye.map((c) => [c[0], ...c.slice(1).map((v, i) => (i % 2 ? cy + (v - cy) * SPEC.poses.blink : v))]); };
function signSvg(Lt, sign, box, pal, ink) {
  const r = fix(SPEC.drones.radius * Lt.pitch, 3), rest = (pal.dronesBody ?? pal.body)[1];
  const dot = (x, y, fill, extra = '') => `<circle cx="${fix(x)}" cy="${fix(y)}" r="${r}" fill="${fill}"${extra}/>`;
  const lit = new Set(sign.cells.map(([i, j]) => `${i},${j}`));
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${box.map((v) => fix(v)).join(' ')}">${Lt.dots.filter((d) => d.cls !== 'eye' && !lit.has(`${d.i},${d.j}`)).map((d) => dot(d.x, d.y, rest, ` opacity="${SPEC.poses.signRest}"`)).join('')}${sign.cells.map(([i, j]) => dot(Lt.ox + (i + 0.5) * Lt.pitch, Lt.oy + (j + 0.5) * Lt.pitch, ink)).join('')}</svg>\n`;
}
for (const theme of ['light', 'dark']) {
  const sfx = theme === 'light' ? '' : '-dark';
  POSES.forEach(([name, amount], k) => { OUTPUTS[`svg/pose-${name}${sfx}.svg`] = coloredSvg(POSE_PARTS[k], POSE_BOX, P[theme], amount === 0 ? { back: ['flukes', 'body'], front: [] } : whaleIds); });
  OUTPUTS[`svg/pose-blink${sfx}.svg`] = coloredSvg({ ...WHALE_PARTS, eye: shut(WHALE_PARTS.eye) }, POSE_BOX, P[theme], whaleIds);
  const Lt = SIDEBAR.at(-1);
  for (const [name, ink] of [['waiting', P[theme].lantern], ['blocked', P[theme].blocked], ['complete', (P[theme].dronesBody ?? P[theme].body)[0]]]) OUTPUTS[`svg/sign-${name}${sfx}.svg`] = signSvg(Lt, signFormation(DR.signs[name], Lt), markBox, P[theme], ink);
}
for (const look of ['light', 'dark', 'tinted']) OUTPUTS[`svg/icon-${look}.svg`] = iconSvg(look);

const check = process.argv.includes('--check');
let drift = 0;
for (const [rel, text] of Object.entries(OUTPUTS)) {
  const file = join(HERE, rel);
  if (check) { if (!existsSync(file) || readFileSync(file, 'utf8') !== text) { drift += 1; console.error(`stale: ${rel}`); } continue; }
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, text);
}
if (check) { if (drift) { console.error(`${drift} generated file(s) are stale; run node whale/generate.mjs`); process.exit(1); } console.log(`whale: ${Object.keys(OUTPUTS).length} generated files match the source`); process.exit(0); }
if (process.argv.includes('--icons')) {
  const out = join(HERE, 'out/icon'), set = join(out, 'AppIcon.iconset');
  rmSync(out, { recursive: true, force: true });
  mkdirSync(set, { recursive: true });
  const png = (look, size, file) => execFileSync('rsvg-convert', ['-w', String(size), '-h', String(size), join(HERE, `svg/icon-${look}.svg`), '-o', file]);
  for (const [name, size] of [['16x16', 16], ['16x16@2x', 32], ['32x32', 32], ['32x32@2x', 64], ['128x128', 128], ['128x128@2x', 256], ['256x256', 256], ['256x256@2x', 512], ['512x512', 512], ['512x512@2x', 1024]]) png('light', size, join(set, `icon_${name}.png`));
  execFileSync('iconutil', ['-c', 'icns', set, '-o', join(out, 'codewhale.icns')]);
  png('light', 1024, join(out, 'codewhale-1024.png'));
  for (const look of ['dark', 'tinted']) png(look, 1024, join(out, `codewhale-${look}-1024.png`));
  const sizes = [16, 24, 32, 48, 64, 128, 256];
  for (const s of sizes) png('light', s, join(out, `ico-${s}.png`));
  execFileSync('magick', [...sizes.map((s) => join(out, `ico-${s}.png`)), join(out, 'codewhale.ico')]);
  for (const s of sizes) rmSync(join(out, `ico-${s}.png`));
  rmSync(set, { recursive: true, force: true });
}
console.log(`whale: wrote ${Object.keys(OUTPUTS).length} files${process.argv.includes('--icons') ? ' and out/icon' : ''}; launch ${LAUNCH.map((x) => order(x).length).join('/')}, sidebar ${SIDEBAR.map((x) => order(x).length).join('/')}, indicator ${INDICATOR.map((x) => x.counts[0]).join('/')}`);
