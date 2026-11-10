import lights from "../vendor/whale-lights/whale-lights.json";

type Lights = {
  center: [number, number];
  beadRadius: number;
  mark: { scale: number; offset: [number, number] };
  spine: { head: number; sweep: number };
  drift: { spread: number; swim: number; swimPeriod: number; swimWaves: number; hover: number; hoverRate: [number, number]; bob: number; bobRatio: number };
  rest: { breath: [number, number]; wave: [number, number]; glow: [number, number]; crestWidth: number; crestWander: number; crestLift: number; stars: number };
  beads: [number, number][];
};

const L = lights as unknown as Lights;
const TAU = Math.PI * 2;
const [CX, CY] = L.center;
export const IGNITE = 0.14;
export const BEAD_RADIUS = L.beadRadius * L.mark.scale;

function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) { h ^= text.charCodeAt(i); h = Math.imul(h, 0x01000193); }
  return h >>> 0;
}

function prng(seed: number): () => number {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function spineOf([x, y]: [number, number]): number {
  const angle = (Math.atan2(y - CY, x - CX) * 180 / Math.PI + 360) % 360;
  return Math.max(0, Math.min(1, ((angle - L.spine.head + 360) % 360) / L.spine.sweep));
}

export const BEADS = L.beads.map((point) => ({
  home: [(point[0] - CX) * L.mark.scale + L.mark.offset[0], (point[1] - CY) * L.mark.scale + L.mark.offset[1]] as [number, number],
  point,
  spine: spineOf(point),
}));

const HOVER = BEADS.map((_, index) => {
  const random = prng(hash(`bead:${index}`));
  const [base, span] = L.drift.hoverRate;
  return [base + random() * span, base + random() * span, random() * TAU, random() * TAU] as const;
});

const HOME = (() => {
  const random = prng(hash("codewhale"));
  const stars = new Set<number>();
  for (let bin = 0; bin < L.rest.stars; bin += 1) {
    const low = bin / L.rest.stars;
    const high = (bin + 1) / L.rest.stars + (bin === L.rest.stars - 1 ? 1e-6 : 0);
    const choices = BEADS.flatMap((bead, index) => (bead.spine >= low && bead.spine < high ? [index] : []));
    const pick = choices[Math.floor(random() * choices.length)];
    if (pick !== undefined) stars.add(pick);
  }
  for (let skip = 0; skip < 9; skip += 1) random();
  return {
    stars,
    breath: L.rest.breath[0] + random() * L.rest.breath[1],
    wave: L.rest.wave[0] + random() * L.rest.wave[1],
    phase: random(),
  };
})();

export function isStar(index: number): boolean {
  return HOME.stars.has(index);
}

export function lightsAt(time: number) {
  const glow = L.rest.glow[0] + L.rest.glow[1] * Math.sin((TAU * time) / HOME.breath);
  const crest = ((time / HOME.wave + HOME.phase + L.rest.crestWander * Math.sin((TAU * time) / (HOME.wave * L.drift.bobRatio))) % 1) * 1.4 - 0.2;
  return { glow, crest };
}

export function beadShine(index: number, crest: number): number {
  return Math.max(0, 1 - Math.abs(BEADS[index]!.spine - crest) / L.rest.crestWidth) ** 2;
}

export function beadOffset(index: number, time: number, shine: number): [number, number] {
  const bead = BEADS[index]!;
  const [w1, w2, p1, p2] = HOVER[index]!;
  const { spread, swim, swimPeriod, swimWaves, hover, bob, bobRatio } = L.drift;
  const dx = bead.point[0] - CX, dy = bead.point[1] - CY, r = Math.hypot(dx, dy) || 1;
  const reach = spread * Math.sin((TAU * time) / HOME.breath);
  const wave = swim * Math.sin(TAU * (time / swimPeriod - bead.spine * swimWaves)) + shine * L.rest.crestLift;
  return [
    (dx * reach + (dx / r) * wave + hover * Math.sin(time * w1 + p1)) * L.mark.scale,
    (dy * reach + (dy / r) * wave + hover * Math.sin(time * w2 + p2) + bob * Math.sin((TAU * time) / (HOME.breath * bobRatio))) * L.mark.scale,
  ];
}
