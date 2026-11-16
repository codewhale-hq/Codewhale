// PetSim — the portable core of the Codewhale pet.
//
// This is a faithful, DOM-free port of the consort study's particle engine
// (grammar.js). The same code — same constants, same order of operations — is
// what the Rust core implements. Four rules keep the view
// identical on every surface:
//
//   1. The body is the same 980 points, loaded from whale-points.tsv — never
//      re-sampled from the image on each platform.
//   2. Per-particle jitter phases come from mulberry32(0xC0FFEE), not from the
//      platform's RNG.
//   3. All motion is a pure function of (sim clock, state): nothing accumulates
//      noise. Two runs fed the same tape produce the same frame.
//   4. Colour, hollowness and brightness are computed here, once — renderers
//      only place and stamp dots.
//
// Positions stay normalized in body space (roughly [-0.5, 0.5]²). A renderer
// maps them through layout() to its own medium.

export const PET_MAX_SECONDS = 100 * 365 * 86_400;

export interface PetState {
  activity: number;    // how much work 0..1
  coherence: number;   // converging school vs thrashing 0..1
  attention: number;   // interaction salience 0..1
  channel: string;     // Whalesong semantic category
  observed: number;    // instrumentation coverage 0..1
  roamX: number;       // tank position, -1..1
  roamY: number;
  flip: number;        // 1 faces right, -1 left, passing 0 = turning edge-on
  lit: number;         // sleep dimmer 0..1
}

export interface PetOpts { motion: boolean; sensitivity: number; podSlots?: readonly (readonly [number, number])[] }

export const REST_STATE: PetState = {
  activity: 0.35, coherence: 0.8, attention: 0, channel: 'reasoning',
  observed: 1, roamX: 0, roamY: 0, flip: 1, lit: 1,
};

const lerp = (a: number, b: number, t: number) => a + (b - a) * t;
const clamp = (v: number, a = 0, b = 1) => Math.min(b, Math.max(a, v));

// ---------------------------------------------------------------------------
// Whalesong's vocabulary, carried over unchanged: colour, register, and the
// sustained-vs-onset rule live in the same table the instrument uses.
export interface Channel { key: string; label: string; color: string; freq: number; sustained: boolean; arch: string; form: string }

export const ARCH_OF: Record<string, string> = {
  reasoning: 'gyre', memory: 'gyre',
  tool: 'strike', code: 'strike', filesystem: 'strike',
  network: 'cross', communication: 'cross', browser: 'cross',
  agent: 'pod', orchestration: 'pod',
  error: 'tear', human: 'address', other: 'drift',
};

export const CHANNELS: Channel[] = [
  { key: 'reasoning',     label: 'Model / reasoning',  color: '#73c9b5', freq: 130.81, sustained: true,  arch: 'gyre',    form: 'gyre · rolling' },
  { key: 'tool',          label: 'Tool calls',         color: '#74aadd', freq: 261.63, sustained: false, arch: 'strike',  form: 'strike · reaching' },
  { key: 'memory',        label: 'Memory / RAG',       color: '#b6a77f', freq: 195.99, sustained: true,  arch: 'gyre',    form: 'gyre · scanning' },
  { key: 'code',          label: 'Code execution',     color: '#9b9ed7', freq: 164.81, sustained: false, arch: 'strike',  form: 'strike · along the body' },
  { key: 'filesystem',    label: 'Filesystem',         color: '#92b9c9', freq: 440.00, sustained: false, arch: 'strike',  form: 'strike · fanning' },
  { key: 'network',       label: 'Network / API',      color: '#d3ac74', freq: 523.25, sustained: false, arch: 'cross',   form: 'crossing · one way' },
  { key: 'browser',       label: 'Browser / computer', color: '#9ea9df', freq: 349.23, sustained: false, arch: 'cross',   form: 'crossing · a sweep' },
  { key: 'communication', label: 'Agent messages',     color: '#83c5c9', freq: 293.66, sustained: false, arch: 'cross',   form: 'crossing · two ways' },
  { key: 'agent',         label: 'Subagent activity',  color: '#b09acb', freq: 220.00, sustained: true,  arch: 'pod',     form: 'pod · peers' },
  { key: 'orchestration', label: 'Orchestration',      color: '#6c8798', freq:  98.00, sustained: true,  arch: 'pod',     form: 'pod · hub' },
  { key: 'error',         label: 'Errors / exceptions',color: '#e79186', freq: 185.00, sustained: false, arch: 'tear',    form: 'torn · irregular' },
  { key: 'human',         label: 'Human interaction',  color: '#c2b787', freq: 391.99, sustained: false, arch: 'address', form: 'decision · junction' },
  { key: 'other',         label: 'Unclassified',       color: '#738492', freq: 146.83, sustained: false, arch: 'drift',   form: 'drifting · unformed' },
];
export const CHANNEL_INDEX: Record<string, number> = Object.fromEntries(CHANNELS.map((c, i) => [c.key, i]));

export function validatePetState(value: unknown): asserts value is PetState {
  const s = value as PetState;
  if (!s || typeof s !== 'object' || !Object.hasOwn(CHANNEL_INDEX, s.channel)
    || ![s.activity, s.coherence, s.attention, s.observed, s.lit].every(n => Number.isFinite(n) && n >= 0 && n <= 1)
    || ![s.roamX, s.roamY, s.flip].every(n => Number.isFinite(n) && Math.abs(n) <= 1))
    throw new Error('Invalid pet state.');
}

const hex2rgb = (h: string) => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)];
const RGB = CHANNELS.map(c => hex2rgb(c.color));
const UNKNOWN_RGB = hex2rgb('#738492');
const REST_RGB = [122, 214, 240];

// mulberry32 — a 32-bit seeded PRNG tiny enough to port by hand correctly.
export function mulberry32(seed: number) {
  let a = seed >>> 0;
  return Object.assign(() => {
    a = (a + 0x6D2B79F5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }, { state: () => a, restore: (state: number) => {
    if (!Number.isSafeInteger(state) || state < 0 || state > 0xffffffff) throw new Error('Invalid pet random stream.');
    a = state;
  } });
}

export interface Particle {
  x: number; y: number; vx: number; vy: number;
  s: number; jx: number; jy: number; pod: number;
  hx: number; hy: number; ang: number; rad: number; tail: number;
  tx: number; ty: number;
}

export interface Frame {
  r: number; g: number; b: number;   // particle colour, 0..255
  alpha: number;                     // uniform per-dot alpha
  hollow: boolean;                   // coverage gap: rings, not filled dots
  channel: string;                   // active category
  arch: string;                      // active archetype
  work: number;                      // rest↔work blend actually applied
}

export interface PetSimCheckpoint {
  version: 1;
  expressionVersion?: 1 | 2;
  body: number[][];
  particles: number[][];
  phase: number; clock: number; tear: number; previous: number; current: number;
  color: number[]; frame: Frame;
}

/** Work reorganizes the same particles; no new random draws or invented facts.
 * These are expressive fields, not diagrams of unobserved network/file topology. */
function fieldTarget(q: Particle, t: number, act: number, att: number, key: string): [number, number] | undefined {
  const u = q.s * 2 - 1, lane = q.pod - 2.5, a = q.s * Math.PI * 2;
  const flow = t * (.35 + act * .65);
  if (key === 'reasoning') {
    const ring = .34 + .105 * Math.cos(a * 3 + flow + lane * .18);
    return [ring * Math.cos(a * 2 + flow * .3), ring * Math.sin(a * 2 + flow * .3) * .7 + .10 * Math.sin(a * 3 + flow)];
  }
  if (key === 'memory') return [.46 * Math.cos(a + lane * .1 + flow * .25), lane * .082 + .052 * Math.sin(a * 2 + flow)];
  if (key === 'code') return [u * .57, lane * .066 + .12 * Math.sin(u * 7 + flow * 2 + q.pod * Math.PI / 3)];
  if (key === 'filesystem') {
    const branch = Math.max(0, (u + .3) / 1.3);
    return [u * .56, lane * .13 * branch + .025 * Math.sin(u * 8 - flow)];
  }
  if (key === 'tool') {
    const reach = .14 + (u + 1) * .20 + .04 * Math.sin(flow * 3 - u * 4);
    return [Math.cos(q.pod * Math.PI / 3) * reach, Math.sin(q.pod * Math.PI / 3) * reach * .8 + q.hy * .06];
  }
  if (key === 'browser') return [u * .56, lane * .083 + .035 * Math.sin(u * 5 - flow * 2)];
  if (key === 'network' || key === 'communication') {
    const direction = key === 'communication' && q.pod % 2 === 1 ? -1 : 1;
    const phase = a + flow * direction;
    return [.54 * Math.cos(phase), Math.sin(phase) * (.12 + q.pod * .035) + lane * .024];
  }
  if (key === 'human') {
    const gap = u < 0 ? -.075 : .075;
    return [u * .47 + gap, lane * .10 * Math.abs(u) + .012 * Math.sin(flow + a) * (1 - att)];
  }
  return undefined;
}

// Version 1 retains the original authored gait for existing recordings.
function gaitTarget(q: Particle, t: number, act: number, coh: number, att: number, key: string, work: number, podSlots?: PetOpts['podSlots'], expressionVersion = 1): [number, number] {
  const { hx, hy, ang, rad, tail, s, pod, jx, jy } = q;
  const omega = lerp(4.6, 5.2 + act * 2.8, work);
  const breath = 1 + Math.sin(t * 1.85) * lerp(0.048, 0.018, work);
  const flex = Math.sin(ang * 2.05 + t * omega) * lerp(0.042, 0.016 + act * 0.028, work) * (0.18 + 0.82 * tail);
  let px = Math.cos(ang + flex) * rad * breath;
  let py = Math.sin(ang + flex) * rad * breath;
  px += Math.sin(t * 0.33) * lerp(0.030, 0.014, work);
  py += Math.cos(t * 0.21) * lerp(0.018, 0.010, work);
  if (work < 0.02) return [px, py];

  const arch = ARCH_OF[key] || 'drift';
  let gx = px, gy = py;
  if (arch === 'gyre') {
    if (key === 'memory') {
      const pulse = 1 + Math.sin(t * (2.4 + act * 1.6) - rad * 11) * (0.15 + act * 0.10);
      gx *= pulse; gy *= pulse;
    } else {
      const roll = Math.sin(t * (1.05 + act * 0.35)) * (0.48 + act * 0.32);
      const c = Math.cos(roll), sn = Math.sin(roll);
      gx = px * c - py * sn * 0.88;
      gy = px * sn * 0.88 + py * c;
    }
  } else if (arch === 'strike') {
    if (key === 'tool') {
      const rate = 2.7 + act * 2.1;
      const lunge = Math.pow(Math.max(0, Math.sin(t * rate)), 2);
      gx += lunge * 0.11;
      if (s > 0.60) {
        const reach = Math.pow(Math.max(0, Math.sin(t * rate + pod * 0.92)), 4) * (0.30 + act * 0.24);
        gx += Math.cos(ang) * reach;
        gy += Math.sin(ang) * reach;
      }
    } else if (key === 'code') {
      const rate = 3.2 + act * 1.8;
      const wave = Math.sin(t * rate - tail * 7.5);
      const bump = 0.11 + act * 0.08;
      gx += Math.cos(ang) * wave * bump;
      gy += Math.sin(ang) * wave * bump * 1.2;
      gx += Math.max(0, wave) * 0.07;
    } else {
      const rate = 2.15 + act * 1.5;
      const side = (pod % 2) * 2 - 1;
      const w = Math.pow(Math.max(0, Math.sin(t * rate + pod * 0.72)), 2);
      gx += w * 0.055;
      gy += side * w * (0.17 + act * 0.13);
    }
  } else if (arch === 'cross') {
    if (key === 'browser') {
      const band = ((t * (0.55 + act * 0.35)) % 1) * 1.28 - 0.64;
      const inBand = Math.max(0, 1 - Math.abs(hy - band) / 0.08);
      gx += inBand * (0.24 + act * 0.10);
      gy += inBand * 0.02;
    } else {
      const two = key === 'communication';
      const courier = s < (two ? 0.44 : 0.32);
      if (courier) {
        const dir = two ? (s < 0.22 ? 1 : -1) : 1;
        const u = (t * (0.38 + act * 0.36) + s * 5.2) % 1;
        const going = u < 0.5 ? u * 2 : 2 - u * 2;
        const e = going * going * (3 - 2 * going);
        gx = lerp(hx, dir * 0.80, e);
        gy = hy * (1 - e * 0.38) + Math.sin(going * Math.PI) * 0.11 * dir;
      }
    }
  } else if (arch === 'pod') {
    const n = 6, member = podSlots?.length ? podSlots[pod % podSlots.length] : undefined;
    const k = member ? member[0] : pod % n;
    const hub = key === 'orchestration' && k === 0;
    const spread = 0.30 + act * 0.11;
    const orbit = t * (0.55 + act * 0.28);
    if (hub) { gx = px * 0.70; gy = py * 0.70; }
    else {
      const slots = key === 'orchestration' ? n - 1 : n;
      const a = (key === 'orchestration' ? k - 1 : k) * (Math.PI * 2 / slots) + orbit + (member ? member[1] * .04 : 0);
      const sc = 0.34;
      gx = hx * sc + Math.cos(a) * spread * 1.28;
      gy = hy * sc + Math.sin(a) * spread * 0.80;
    }
  } else if (arch === 'tear') {
    const side = hx + hy < 0 ? -1 : 1;
    gx += side * (0.24 + (1 - coh) * 0.16);
    gy += side * 0.15;
    gx += Math.sin(t * 11.4 + s * 40) * (0.045 + act * 0.05);
    gy += Math.cos(t * 9.2 + s * 31) * (0.040 + act * 0.045);
  } else if (arch === 'address') {
    const face = 0.90 + att * 0.08;
    const th = 0.70;
    const z = (s - 0.5) * 0.42;
    let ax = hx * Math.cos(th) + z * Math.sin(th);
    let ay = hy;
    const disc = 0.48 * face;
    ax = lerp(ax, Math.cos(ang) * Math.min(0.36, rad + 0.06) * 0.95, disc);
    ay = lerp(ay, Math.sin(ang) * Math.min(0.36, rad + 0.06) * 1.08, disc);
    const grow = 1.20 + Math.sin(t * 1.65) * 0.055;
    gx = ax * grow; gy = ay * grow;
  } else {
    const mill = 0.13 + (1 - coh) * 0.10;
    gx = hx * 0.52 + Math.sin(t * 0.72 + jx) * mill;
    gy = hy * 0.52 + Math.cos(t * 0.54 + jy) * mill;
  }
  if (expressionVersion === 2) {
    const field = fieldTarget(q, t, act, att, key);
    if (field) [gx, gy] = field;
  }
  return [lerp(px, gx, work), lerp(py, gy, work)];
}

// Fixed reduced-motion clock per channel, so ticks still point along the gait.
const STILL_T: Record<string, number> = {
  reasoning: 1.15, memory: 0.42, tool: 0.30, code: 0.18, filesystem: 0.48,
  network: 0.72, browser: 0.95, communication: 0.58, agent: 1.25,
  orchestration: 0.85, error: 0.35, human: 0.05, other: 0.90,
};

export class PetSim {
  readonly p: Particle[];
  private phase = 0;
  private clock = 0;
  private tear = 0;
  private prev: number;
  private col = [...REST_RGB];
  private cur: number;
  frame: Frame = { r: REST_RGB[0], g: REST_RGB[1], b: REST_RGB[2], alpha: 0.3, hollow: false, channel: 'reasoning', arch: 'gyre', work: 0 };

  constructor(points: [number, number][], seed = 0xC0FFEE, readonly expressionVersion: 1 | 2 = 2) {
    if (expressionVersion !== 1 && expressionVersion !== 2) throw new Error('Unsupported pet expression version.');
    const rand = mulberry32(seed);
    this.p = points.map(([hx, hy], i) => {
      const q: Particle = {
        x: hx, y: hy, vx: 0, vy: 0,
        s: rand(), jx: rand() * 6.283, jy: rand() * 6.283, pod: i % 6,
        hx, hy, ang: 0, rad: 0, tail: 0, tx: hx, ty: hy,
      };
      q.ang = Math.atan2(hy, hx);
      q.rad = Math.hypot(hx, hy);
      q.tail = clamp(((-hx - hy) * 0.5 + 0.22) / 0.62);
      return q;
    });
    this.cur = this.prev = CHANNEL_INDEX['reasoning'];
  }

  checkpoint(): PetSimCheckpoint {
    return { version: 1, expressionVersion: this.expressionVersion, body: this.p.map(p => [p.hx, p.hy, p.s]),
      particles: this.p.map(p => [p.x, p.y, p.vx, p.vy, p.jx, p.jy, p.tx, p.ty]),
      phase: this.phase, clock: this.clock, tear: this.tear, previous: this.prev, current: this.cur,
      color: [...this.col], frame: { ...this.frame } };
  }

  /** Restore into a newly constructed sim. Authored body and seeded particle
   * identity must match exactly; a checkpoint cannot replace the whale. */
  restore(value: unknown): void {
    const c = value as PetSimCheckpoint;
    const inRange = (n: number, low: number, high: number) => Number.isFinite(n) && n >= low && n <= high;
    if (!c || c.version !== 1 || c.expressionVersion !== undefined && ![1, 2].includes(c.expressionVersion) || (c.expressionVersion ?? 1) !== this.expressionVersion || !Array.isArray(c.body) || c.body.length !== this.p.length
      || c.body.some((v, i) => !Array.isArray(v) || v.length !== 3 || v[0] !== this.p[i].hx || v[1] !== this.p[i].hy || v[2] !== this.p[i].s)
      || !Array.isArray(c.particles) || c.particles.length !== this.p.length
      || c.particles.some(v => !Array.isArray(v) || v.length !== 8 || v.some((n, i) => !inRange(n, i === 4 || i === 5 ? 0 : -8, i === 4 || i === 5 ? 2 * PET_MAX_SECONDS : 8)))
      || !inRange(c.phase, 0, PET_MAX_SECONDS) || !inRange(c.clock, 0, PET_MAX_SECONDS) || !inRange(c.tear, 0, 1)
      || ![c.previous, c.current].every(n => Number.isInteger(n) && n >= 0 && n < CHANNELS.length)
      || !Array.isArray(c.color) || c.color.length !== 3 || c.color.some(n => !inRange(n, 0, 255))
      || !c.frame || ![c.frame.r, c.frame.g, c.frame.b].every(n => inRange(n, 0, 255))
      || !inRange(c.frame.alpha, 0, 1) || !inRange(c.frame.work, 0, 1) || typeof c.frame.hollow !== 'boolean'
      || c.frame.channel !== CHANNELS[c.current].key || c.frame.arch !== CHANNELS[c.current].arch)
      throw new Error('Invalid pet particle checkpoint.');
    this.phase = c.phase; this.clock = c.clock; this.tear = c.tear; this.prev = c.previous; this.cur = c.current;
    this.col = [...c.color]; this.frame = { ...c.frame };
    this.p.forEach((p, i) => { [p.x, p.y, p.vx, p.vy, p.jx, p.jy, p.tx, p.ty] = c.particles[i]; });
  }

  /** Advance the sim by dt seconds under `state`. Identical math on every port. */
  step(dt: number, state: PetState, opts: PetOpts): void {
    const S = (v: number) => lerp(0.5, v, opts.sensitivity);
    const act = S(state.activity), coh = S(state.coherence), att = S(state.attention);
    const seen = S(state.observed === undefined ? 1 : state.observed);
    const motion = opts.motion ? 1 : 0;
    this.phase += dt * (0.18 + act * 0.55) * motion;
    this.clock += dt * (opts.motion ? 1 : 0);

    if (CHANNEL_INDEX[state.channel] !== undefined) this.cur = CHANNEL_INDEX[state.channel];
    const shown = this.cur;
    const ch = CHANNELS[shown];

    const work = clamp((act - 0.16) / 0.18);
    const wander = lerp(0.32, 1, Math.pow(1 - coh, 1.15));

    if (shown !== this.prev) { if (shown === CHANNEL_INDEX['error']) this.tear = 1; this.prev = shown; }
    this.tear = opts.motion ? Math.max(0, this.tear - dt * 1.6) : 0;

    const split = Math.pow(1 - coh, 1.6) * 0.16 + this.tear * 0.10;
    const blur = Math.pow(1 - coh, 1.45) * 0.22 + this.tear * 0.18;
    const pull = opts.motion ? (2.2 + coh * 5.2) : 18;
    const tGait = opts.motion ? this.clock : (STILL_T[ch.key] ?? 0.4);

    for (const q of this.p) {
      if (opts.motion) {
        q.jx += dt * (0.40 + act * 1.1);
        q.jy += dt * (0.34 + act * 0.9);
      }
      const [gx, gy] = gaitTarget(q, tGait, act, coh, att, ch.key, work, opts.podSlots, this.expressionVersion);
      const podAng = q.pod * 1.047 + this.phase * 0.22;
      const tx = gx + Math.sin(q.jx + q.s * 9) * blur * wander + Math.cos(podAng) * split;
      const ty = gy + Math.cos(q.jy + q.s * 7) * blur * wander + Math.sin(podAng) * split * 0.55;
      q.tx = tx; q.ty = ty;
      if (!opts.motion) { q.x = tx; q.y = ty; q.vx = 0; q.vy = 0; continue; }
      q.vx += (tx - q.x) * pull * dt; q.vy += (ty - q.y) * pull * dt;
      q.vx *= 0.90; q.vy *= 0.90;
      q.x += q.vx * dt * (opts.motion ? 2.6 : 8); q.y += q.vy * dt * (opts.motion ? 2.6 : 8);
    }

    // ---- visual encoding: the parts of "the same view" that are not motion
    const want = work > 0.35 ? RGB[shown] : REST_RGB;
    const k = opts.motion ? Math.min(1, dt * 2.6) : 1;
    for (let c = 0; c < 3; c++) this.col[c] += (lerp(UNKNOWN_RGB[c], want[c], seen) - this.col[c]) * k;
    const lit = clamp(state.lit);
    const alpha = (0.22 + act * 0.10) * lerp(0.50, 1, coh) * lerp(0.55, 1, seen) * lerp(0.35, 1, lit);
    this.frame = {
      r: this.col[0], g: this.col[1], b: this.col[2],
      alpha: Math.min(0.92, alpha * 1.85),
      hollow: seen < 0.92,
      channel: ch.key, arch: ch.arch, work,
    };
  }
}

/** Body-space → renderer-space. Renderers place each dot at (lx,ly) in pixels/cells. */
export function layout(w: number, h: number, state: PetState) {
  const att = state.attention;
  const scale = Math.min(w * 0.52, h * 0.92) * (1 + att * 0.07);
  return {
    scale,
    flipX: state.flip,
    ox: w / 2 + state.roamX * w * 0.30,
    oy: h / 2 + state.roamY * h * 0.30 + h * att * 0.05,
    dot: Math.max(1.6, Math.min(w, h) * 0.0092) * (1 + att * 0.18),
  };
}

// ---------------------------------------------------------------------------
// Conformance. A tape is a list of [dt, state] rows; run it and digest the
// quantized field every `every` frames. 64×32 cells over [-0.66, 0.66]².
// Two implementations that produce the same digest lines drew the same whale.
export function digest(sim: PetSim): string {
  const W = 64, H = 32;
  const grid = new Uint8Array(W * H);
  for (const q of sim.p) {
    const cx = Math.floor((q.x + 0.66) / 1.32 * W);
    const cy = Math.floor((q.y + 0.66) / 1.32 * H);
    if (cx >= 0 && cx < W && cy >= 0 && cy < H) grid[cy * W + cx] = Math.min(255, grid[cy * W + cx] + 1);
  }
  // FNV-1a 64 over the grid plus the frame encoding (rgb, hollow, alpha byte)
  // Two unsigned halves also work in embedded engines without BigInt.
  // FNV's prime is (256 << 32) + 435; these products stay below 2^42,
  // so every intermediate integer is exactly representable by a JS number.
  let hi = 0xcbf29ce4, lo = 0x84222325;
  const mix = (b: number) => {
    lo = (lo ^ (b & 0xff)) >>> 0;
    const product = lo * 435;
    hi = (hi * 435 + lo * 256 + Math.floor(product / 4294967296)) >>> 0;
    lo = product >>> 0;
  };
  for (const v of grid) mix(v);
  mix(Math.round(sim.frame.r)); mix(Math.round(sim.frame.g)); mix(Math.round(sim.frame.b));
  mix(Math.round(sim.frame.alpha * 255)); mix(sim.frame.hollow ? 1 : 0);
  return hi.toString(16).padStart(8, '0') + lo.toString(16).padStart(8, '0');
}

/** Shared tape runner. `rows` are parsed tape.tsv lines. */
export function runTape(sim: PetSim, rows: string[], opts: PetOpts): string[] {
  const out: string[] = [];
  let f = 0;
  for (const row of rows) {
    const c = row.split('\t');
    if (c.length < 10 || c[0] === 'dt') continue;
    const st: PetState = {
      activity: +c[1], coherence: +c[2], attention: +c[3], channel: c[4],
      observed: +c[5], roamX: +c[6], roamY: +c[7], flip: +c[8], lit: +c[9],
    };
    sim.step(+c[0], st, opts);
    if (f++ % 30 === 0) out.push(`f${String(f - 1).padStart(4, '0')} ${digest(sim)} ${st.channel}`);
  }
  out.push(`final ${digest(sim)}`);
  return out;
}
