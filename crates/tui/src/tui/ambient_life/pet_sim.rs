//! pet_sim.rs — the Codewhale pet core, Rust port.
//!
//! A faithful, dependency-free port of PetSim.ts (which ports grammar.js's
//! consort engine). Same 980-point body from whale-points.tsv, same
//! mulberry32(0xC0FFEE) jitter, same gait field and spring integration, same
//! colour/hollow/brightness encoding. If a tape produces the same digest here
//! as in TypeScript, every surface drew the same whale.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(usize)]
pub enum ChannelId {
    Reasoning,
    Tool,
    Memory,
    Code,
    Filesystem,
    Network,
    Browser,
    Communication,
    Agent,
    Orchestration,
    Error,
    Human,
    #[default]
    Other,
}

impl ChannelId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
            Self::Tool => "tool",
            Self::Memory => "memory",
            Self::Code => "code",
            Self::Filesystem => "filesystem",
            Self::Network => "network",
            Self::Browser => "browser",
            Self::Communication => "communication",
            Self::Agent => "agent",
            Self::Orchestration => "orchestration",
            Self::Error => "error",
            Self::Human => "human",
            Self::Other => "other",
        }
    }
}
impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(usize)]
pub enum Archetype {
    Gyre,
    Strike,
    Cross,
    Pod,
    Tear,
    Address,
    #[default]
    Drift,
}

impl Archetype {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gyre => "gyre",
            Self::Strike => "strike",
            Self::Cross => "cross",
            Self::Pod => "pod",
            Self::Tear => "tear",
            Self::Address => "address",
            Self::Drift => "drift",
        }
    }
}
impl std::fmt::Display for Archetype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ChannelId {
    pub fn from_key(key: &str) -> Option<Self> {
        CHANNELS
            .iter()
            .find(|c| c.key.as_str() == key)
            .map(|c| c.key)
    }
}

pub const N_CHANNELS: usize = 13;

#[derive(Clone, Copy, Default)]
pub struct PetState {
    pub activity: f64,
    pub coherence: f64,
    pub attention: f64,
    pub channel: ChannelId,
    pub observed: f64,
    pub roam_x: f64,
    pub roam_y: f64,
    pub flip: f64,
    pub lit: f64,
}

impl PetState {
    pub fn rest() -> Self {
        PetState {
            activity: 0.35,
            coherence: 0.8,
            attention: 0.0,
            channel: ChannelId::Reasoning,
            observed: 1.0,
            roam_x: 0.0,
            roam_y: 0.0,
            flip: 1.0,
            lit: 1.0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Channel {
    pub key: ChannelId,
    pub label: &'static str,
    pub rgb: [u8; 3],
    pub freq: f64,
    pub sustained: bool,
    pub arch: Archetype,
    pub form: &'static str,
}

pub const CHANNELS: [Channel; N_CHANNELS] = [
    Channel {
        key: ChannelId::Reasoning,
        label: "Model / reasoning",
        rgb: [0x73, 0xc9, 0xb5],
        freq: 130.81,
        sustained: true,
        arch: Archetype::Gyre,
        form: "gyre · rolling",
    },
    Channel {
        key: ChannelId::Tool,
        label: "Tool calls",
        rgb: [0x74, 0xaa, 0xdd],
        freq: 261.63,
        sustained: false,
        arch: Archetype::Strike,
        form: "strike · reaching",
    },
    Channel {
        key: ChannelId::Memory,
        label: "Memory / RAG",
        rgb: [0xb6, 0xa7, 0x7f],
        freq: 195.99,
        sustained: true,
        arch: Archetype::Gyre,
        form: "gyre · scanning",
    },
    Channel {
        key: ChannelId::Code,
        label: "Code execution",
        rgb: [0x9b, 0x9e, 0xd7],
        freq: 164.81,
        sustained: false,
        arch: Archetype::Strike,
        form: "strike · along the body",
    },
    Channel {
        key: ChannelId::Filesystem,
        label: "Filesystem",
        rgb: [0x92, 0xb9, 0xc9],
        freq: 440.00,
        sustained: false,
        arch: Archetype::Strike,
        form: "strike · fanning",
    },
    Channel {
        key: ChannelId::Network,
        label: "Network / API",
        rgb: [0xd3, 0xac, 0x74],
        freq: 523.25,
        sustained: false,
        arch: Archetype::Cross,
        form: "crossing · one way",
    },
    Channel {
        key: ChannelId::Browser,
        label: "Browser / computer",
        rgb: [0x9e, 0xa9, 0xdf],
        freq: 349.23,
        sustained: false,
        arch: Archetype::Cross,
        form: "crossing · a sweep",
    },
    Channel {
        key: ChannelId::Communication,
        label: "Agent messages",
        rgb: [0x83, 0xc5, 0xc9],
        freq: 293.66,
        sustained: false,
        arch: Archetype::Cross,
        form: "crossing · two ways",
    },
    Channel {
        key: ChannelId::Agent,
        label: "Subagent activity",
        rgb: [0xb0, 0x9a, 0xcb],
        freq: 220.00,
        sustained: true,
        arch: Archetype::Pod,
        form: "pod · peers",
    },
    Channel {
        key: ChannelId::Orchestration,
        label: "Orchestration",
        rgb: [0x6c, 0x87, 0x98],
        freq: 98.00,
        sustained: true,
        arch: Archetype::Pod,
        form: "pod · hub",
    },
    Channel {
        key: ChannelId::Error,
        label: "Errors / exceptions",
        rgb: [0xe7, 0x91, 0x86],
        freq: 185.00,
        sustained: false,
        arch: Archetype::Tear,
        form: "torn · irregular",
    },
    Channel {
        key: ChannelId::Human,
        label: "Human interaction",
        rgb: [0xc2, 0xb7, 0x87],
        freq: 391.99,
        sustained: false,
        arch: Archetype::Address,
        form: "decision · junction",
    },
    Channel {
        key: ChannelId::Other,
        label: "Unclassified",
        rgb: [0x73, 0x84, 0x92],
        freq: 146.83,
        sustained: false,
        arch: Archetype::Drift,
        form: "drifting · unformed",
    },
];

const UNKNOWN_RGB: [f64; 3] = [0x73 as f64, 0x84 as f64, 0x92 as f64];
const REST_RGB: [f64; 3] = [122.0, 214.0, 240.0];

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
fn clamp(v: f64, a: f64, b: f64) -> f64 {
    v.min(b).max(a)
}

/// mulberry32 — the same 32-bit sequence as every other port.
pub struct Mulberry32 {
    a: u32,
}
impl Mulberry32 {
    pub fn new(seed: u32) -> Self {
        Mulberry32 { a: seed }
    }
    pub fn next_f64(&mut self) -> f64 {
        self.a = self.a.wrapping_add(0x6D2B79F5);
        let mut t = self.a;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4294967296.0
    }
}

#[derive(Clone, Copy, Default)]
pub struct Particle {
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub s: f64,
    pub jx: f64,
    pub jy: f64,
    pub pod: u32,
    pub hx: f64,
    pub hy: f64,
    pub ang: f64,
    pub rad: f64,
    pub tail: f64,
    pub tx: f64,
    pub ty: f64,
}

#[derive(Clone, Copy, Default)]
pub struct Frame {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub alpha: f64,
    pub hollow: bool,
    pub channel: ChannelId,
    pub arch: Archetype,
    pub work: f64,
}

/// Version 2 fields preserve particle identity and consume no random draws.
fn field_target(q: &Particle, t: f64, act: f64, att: f64, key: ChannelId) -> Option<(f64, f64)> {
    let u = q.s * 2.0 - 1.0;
    let lane = f64::from(q.pod) - 2.5;
    let a = q.s * std::f64::consts::PI * 2.0;
    let flow = t * (0.35 + act * 0.65);
    Some(match key {
        ChannelId::Reasoning => {
            let ring = 0.34 + 0.105 * (a * 3.0 + flow + lane * 0.18).cos();
            (
                ring * (a * 2.0 + flow * 0.3).cos(),
                ring * (a * 2.0 + flow * 0.3).sin() * 0.7 + 0.10 * (a * 3.0 + flow).sin(),
            )
        }
        ChannelId::Memory => (
            0.46 * (a + lane * 0.1 + flow * 0.25).cos(),
            lane * 0.082 + 0.052 * (a * 2.0 + flow).sin(),
        ),
        ChannelId::Code => (
            u * 0.57,
            lane * 0.066
                + 0.12
                    * (u * 7.0 + flow * 2.0 + f64::from(q.pod) * std::f64::consts::PI / 3.0).sin(),
        ),
        ChannelId::Filesystem => {
            let branch = ((u + 0.3) / 1.3).max(0.0);
            (
                u * 0.56,
                lane * 0.13 * branch + 0.025 * (u * 8.0 - flow).sin(),
            )
        }
        ChannelId::Tool => {
            let reach = 0.14 + (u + 1.0) * 0.20 + 0.04 * (flow * 3.0 - u * 4.0).sin();
            (
                (f64::from(q.pod) * std::f64::consts::PI / 3.0).cos() * reach,
                (f64::from(q.pod) * std::f64::consts::PI / 3.0).sin() * reach * 0.8 + q.hy * 0.06,
            )
        }
        ChannelId::Browser => (
            u * 0.56,
            lane * 0.083 + 0.035 * (u * 5.0 - flow * 2.0).sin(),
        ),
        ChannelId::Network | ChannelId::Communication => {
            let direction = if key == ChannelId::Communication && q.pod % 2 == 1 {
                -1.0
            } else {
                1.0
            };
            let phase = a + flow * direction;
            (
                0.54 * phase.cos(),
                phase.sin() * (0.12 + f64::from(q.pod) * 0.035) + lane * 0.024,
            )
        }
        ChannelId::Human => {
            let gap = if u < 0.0 { -0.075 } else { 0.075 };
            (
                u * 0.47 + gap,
                lane * 0.10 * u.abs() + 0.012 * (flow + a).sin() * (1.0 - att),
            )
        }
        _ => return None,
    })
}

/// Version 1 is retained for saved recordings.
/// Ported line-for-line from PetSim.ts gaitTarget().
// Keep the numeric port signature aligned with the TypeScript reference.
#[allow(clippy::too_many_arguments)]
fn gait_target(
    q: &Particle,
    t: f64,
    act: f64,
    coh: f64,
    att: f64,
    key: ChannelId,
    work: f64,
    legacy_expression: bool,
) -> (f64, f64) {
    let omega = lerp(4.6, 5.2 + act * 2.8, work);
    let breath = 1.0 + (t * 1.85).sin() * lerp(0.048, 0.018, work);
    let flex = (q.ang * 2.05 + t * omega).sin()
        * lerp(0.042, 0.016 + act * 0.028, work)
        * (0.18 + 0.82 * q.tail);
    let mut px = (q.ang + flex).cos() * q.rad * breath;
    let mut py = (q.ang + flex).sin() * q.rad * breath;
    px += (t * 0.33).sin() * lerp(0.030, 0.014, work);
    py += (t * 0.21).cos() * lerp(0.018, 0.010, work);
    if work < 0.02 {
        return (px, py);
    }

    let arch = CHANNELS[key as usize].arch;
    let mut gx = px;
    let mut gy = py;
    match arch {
        Archetype::Gyre => {
            if key == ChannelId::Memory {
                let pulse =
                    1.0 + (t * (2.4 + act * 1.6) - q.rad * 11.0).sin() * (0.15 + act * 0.10);
                gx *= pulse;
                gy *= pulse;
            } else {
                let roll = (t * (1.05 + act * 0.35)).sin() * (0.48 + act * 0.32);
                let (c, sn) = (roll.cos(), roll.sin());
                gx = px * c - py * sn * 0.88;
                gy = px * sn * 0.88 + py * c;
            }
        }
        Archetype::Strike => {
            if key == ChannelId::Tool {
                let rate = 2.7 + act * 2.1;
                let lunge = (t * rate).sin().max(0.0).powi(2);
                gx += lunge * 0.11;
                if q.s > 0.60 {
                    let reach = (t * rate + q.pod as f64 * 0.92).sin().max(0.0).powi(4)
                        * (0.30 + act * 0.24);
                    gx += q.ang.cos() * reach;
                    gy += q.ang.sin() * reach;
                }
            } else if key == ChannelId::Code {
                let rate = 3.2 + act * 1.8;
                let wave = (t * rate - q.tail * 7.5).sin();
                let bump = 0.11 + act * 0.08;
                gx += q.ang.cos() * wave * bump;
                gy += q.ang.sin() * wave * bump * 1.2;
                gx += wave.max(0.0) * 0.07;
            } else {
                let rate = 2.15 + act * 1.5;
                let side = (q.pod % 2) as f64 * 2.0 - 1.0;
                let w = (t * rate + q.pod as f64 * 0.72).sin().max(0.0).powi(2);
                gx += w * 0.055;
                gy += side * w * (0.17 + act * 0.13);
            }
        }
        Archetype::Cross => {
            if key == ChannelId::Browser {
                let band = ((t * (0.55 + act * 0.35)) % 1.0) * 1.28 - 0.64;
                let in_band = (1.0 - (q.hy - band).abs() / 0.08).max(0.0);
                gx += in_band * (0.24 + act * 0.10);
                gy += in_band * 0.02;
            } else {
                let two = key == ChannelId::Communication;
                let courier = q.s < if two { 0.44 } else { 0.32 };
                if courier {
                    let dir = if two {
                        if q.s < 0.22 { 1.0 } else { -1.0 }
                    } else {
                        1.0
                    };
                    let u = (t * (0.38 + act * 0.36) + q.s * 5.2) % 1.0;
                    let going = if u < 0.5 { u * 2.0 } else { 2.0 - u * 2.0 };
                    let e = going * going * (3.0 - 2.0 * going);
                    gx = lerp(q.hx, dir * 0.80, e);
                    gy =
                        q.hy * (1.0 - e * 0.38) + (going * std::f64::consts::PI).sin() * 0.11 * dir;
                }
            }
        }
        Archetype::Pod => {
            let n = 6u32;
            let k = q.pod % n;
            let hub = key == ChannelId::Orchestration && k == 0;
            let spread = 0.30 + act * 0.11;
            let orbit = t * (0.55 + act * 0.28);
            if hub {
                gx = px * 0.70;
                gy = py * 0.70;
            } else {
                let slots = if key == ChannelId::Orchestration {
                    n - 1
                } else {
                    n
                };
                let a = (if key == ChannelId::Orchestration {
                    k as f64 - 1.0
                } else {
                    k as f64
                }) * (std::f64::consts::PI * 2.0 / slots as f64)
                    + orbit;
                let sc = 0.34;
                gx = q.hx * sc + a.cos() * spread * 1.28;
                gy = q.hy * sc + a.sin() * spread * 0.80;
            }
        }
        Archetype::Tear => {
            let side = if q.hx + q.hy < 0.0 { -1.0 } else { 1.0 };
            gx += side * (0.24 + (1.0 - coh) * 0.16);
            gy += side * 0.15;
            gx += (t * 11.4 + q.s * 40.0).sin() * (0.045 + act * 0.05);
            gy += (t * 9.2 + q.s * 31.0).cos() * (0.040 + act * 0.045);
        }
        Archetype::Address => {
            let face = 0.90 + att * 0.08;
            let th: f64 = 0.70;
            let z = (q.s - 0.5) * 0.42;
            let mut ax = q.hx * th.cos() + z * th.sin();
            let mut ay = q.hy;
            let disc = 0.48 * face;
            ax = lerp(ax, q.ang.cos() * (0.36_f64).min(q.rad + 0.06) * 0.95, disc);
            ay = lerp(ay, q.ang.sin() * (0.36_f64).min(q.rad + 0.06) * 1.08, disc);
            let grow = 1.20 + (t * 1.65).sin() * 0.055;
            gx = ax * grow;
            gy = ay * grow;
        }
        _ => {
            let mill = 0.13 + (1.0 - coh) * 0.10;
            gx = q.hx * 0.52 + (t * 0.72 + q.jx).sin() * mill;
            gy = q.hy * 0.52 + (t * 0.54 + q.jy).cos() * mill;
        }
    }
    if !legacy_expression && let Some((x, y)) = field_target(q, t, act, att, key) {
        gx = x;
        gy = y;
    }
    (lerp(px, gx, work), lerp(py, gy, work))
}

fn still_t(key: ChannelId) -> f64 {
    match key {
        ChannelId::Reasoning => 1.15,
        ChannelId::Memory => 0.42,
        ChannelId::Tool => 0.30,
        ChannelId::Code => 0.18,
        ChannelId::Filesystem => 0.48,
        ChannelId::Network => 0.72,
        ChannelId::Browser => 0.95,
        ChannelId::Communication => 0.58,
        ChannelId::Agent => 1.25,
        ChannelId::Orchestration => 0.85,
        ChannelId::Error => 0.35,
        ChannelId::Human => 0.05,
        ChannelId::Other => 0.90,
    }
}

pub struct PetSim {
    pub p: Vec<Particle>,
    legacy_expression: bool,
    phase: f64,
    clock: f64,
    tear: f64,
    prev: ChannelId,
    col: [f64; 3],
    pub frame: Frame,
}

impl PetSim {
    /// Select the authored v1 field for historical conformance tapes.
    pub fn legacy_whale() -> Self {
        let mut sim = Self::whale();
        sim.legacy_expression = true;
        sim
    }

    /// Authored body embedded once; all Rust surfaces use these same points.
    pub fn whale() -> Self {
        let points: Vec<(f64, f64)> = include_str!("whale-points.tsv")
            .lines()
            .map(|line| {
                let (x, y) = line.split_once('\t').expect("baked whale point");
                (x.parse().expect("baked x"), y.parse().expect("baked y"))
            })
            .collect();
        Self::new(&points, 0xC0FFEE)
    }

    // These rounded constants are part of the cross-language tape contract.
    #[allow(clippy::approx_constant)]
    pub fn new(points: &[(f64, f64)], seed: u32) -> Self {
        let mut rng = Mulberry32::new(seed);
        let p = points
            .iter()
            .enumerate()
            .map(|(i, &(hx, hy))| {
                let mut q = Particle {
                    x: hx,
                    y: hy,
                    vx: 0.0,
                    vy: 0.0,
                    s: rng.next_f64(),
                    jx: rng.next_f64() * 6.283,
                    jy: rng.next_f64() * 6.283,
                    pod: (i % 6) as u32,
                    hx,
                    hy,
                    ..Default::default()
                };
                q.tx = hx;
                q.ty = hy;
                q.ang = hy.atan2(hx);
                q.rad = hx.hypot(hy);
                q.tail = clamp(((-hx - hy) * 0.5 + 0.22) / 0.62, 0.0, 1.0);
                q
            })
            .collect();
        let cur = ChannelId::Reasoning;
        PetSim {
            p,
            legacy_expression: false,
            phase: 0.0,
            clock: 0.0,
            tear: 0.0,
            prev: cur,
            col: REST_RGB,
            frame: Frame {
                r: REST_RGB[0],
                g: REST_RGB[1],
                b: REST_RGB[2],
                alpha: 0.3,
                hollow: false,
                channel: ChannelId::Reasoning,
                arch: Archetype::Gyre,
                work: 0.0,
            },
        }
    }

    /// Advance the sim by dt seconds under `state`. Identical math to PetSim.ts.
    pub fn step(&mut self, dt: f64, state: &PetState, motion: bool, sensitivity: f64) {
        let s = |v: f64| lerp(0.5, v, sensitivity);
        let act = s(state.activity);
        let coh = s(state.coherence);
        let att = s(state.attention);
        let seen = s(state.observed);
        let mot = if motion { 1.0 } else { 0.0 };
        self.phase += dt * (0.18 + act * 0.55) * mot;
        if motion {
            self.clock += dt;
        }

        let shown = state.channel;
        let ch = CHANNELS[shown as usize];

        let work = clamp((act - 0.16) / 0.18, 0.0, 1.0);
        let wander = lerp(0.32, 1.0, (1.0 - coh).powf(1.15));

        if shown != self.prev {
            if shown == ChannelId::Error {
                self.tear = 1.0;
            }
            self.prev = shown;
        }
        self.tear = if motion {
            (self.tear - dt * 1.6).max(0.0)
        } else {
            0.0
        };

        let split = (1.0 - coh).powf(1.6) * 0.16 + self.tear * 0.10;
        let blur = (1.0 - coh).powf(1.45) * 0.22 + self.tear * 0.18;
        let pull = if motion { 2.2 + coh * 5.2 } else { 18.0 };
        let t_gait = if motion { self.clock } else { still_t(ch.key) };

        for q in self.p.iter_mut() {
            if motion {
                q.jx += dt * (0.40 + act * 1.1);
                q.jy += dt * (0.34 + act * 0.9);
            }
            let (gx, gy) = gait_target(
                q,
                t_gait,
                act,
                coh,
                att,
                ch.key,
                work,
                self.legacy_expression,
            );
            let pod_ang = q.pod as f64 * 1.047 + self.phase * 0.22;
            let tx = gx + (q.jx + q.s * 9.0).sin() * blur * wander + pod_ang.cos() * split;
            let ty = gy + (q.jy + q.s * 7.0).cos() * blur * wander + pod_ang.sin() * split * 0.55;
            q.tx = tx;
            q.ty = ty;
            if !motion {
                q.x = tx;
                q.y = ty;
                q.vx = 0.0;
                q.vy = 0.0;
                continue;
            }
            q.vx += (tx - q.x) * pull * dt;
            q.vy += (ty - q.y) * pull * dt;
            q.vx *= 0.90;
            q.vy *= 0.90;
            let speed = if motion { 2.6 } else { 8.0 };
            q.x += q.vx * dt * speed;
            q.y += q.vy * dt * speed;
        }

        let want = if work > 0.35 {
            [
                CHANNELS[shown as usize].rgb[0] as f64,
                CHANNELS[shown as usize].rgb[1] as f64,
                CHANNELS[shown as usize].rgb[2] as f64,
            ]
        } else {
            REST_RGB
        };
        let k = if motion { (dt * 2.6).min(1.0) } else { 1.0 };
        for c in 0..3 {
            self.col[c] += (lerp(UNKNOWN_RGB[c], want[c], seen) - self.col[c]) * k;
        }
        let lit = clamp(state.lit, 0.0, 1.0);
        let alpha = (0.22 + act * 0.10)
            * lerp(0.50, 1.0, coh)
            * lerp(0.55, 1.0, seen)
            * lerp(0.35, 1.0, lit);
        self.frame = Frame {
            r: self.col[0],
            g: self.col[1],
            b: self.col[2],
            alpha: (alpha * 1.85).min(0.92),
            hollow: seen < 0.92,
            channel: ch.key,
            arch: ch.arch,
            work,
        };
    }
}

/// Body-space → renderer-space, same as PetSim.ts layout().
pub struct Layout {
    pub scale: f64,
    pub flip_x: f64,
    pub ox: f64,
    pub oy: f64,
    pub dot: f64,
}
pub fn layout(w: f64, h: f64, state: &PetState) -> Layout {
    let att = state.attention;
    let scale = (w * 0.52).min(h * 0.92) * (1.0 + att * 0.07);
    Layout {
        scale,
        flip_x: state.flip,
        ox: w / 2.0 + state.roam_x * w * 0.30,
        oy: h / 2.0 + state.roam_y * h * 0.30 + h * att * 0.05,
        dot: (1.6_f64).max(w.min(h) * 0.0092) * (1.0 + att * 0.18),
    }
}

/// Conformance digest: quantize the field onto a 64×32 grid over
/// [-0.66, 0.66]², then FNV-1a the counts plus the frame encoding.
pub fn digest(sim: &PetSim) -> String {
    const W: usize = 64;
    const H: usize = 32;
    let mut grid = [0u8; W * H];
    for q in &sim.p {
        let cx = ((q.x + 0.66) / 1.32 * W as f64).floor() as i32;
        let cy = ((q.y + 0.66) / 1.32 * H as f64).floor() as i32;
        if cx >= 0 && cx < W as i32 && cy >= 0 && cy < H as i32 {
            let i = cy as usize * W + cx as usize;
            grid[i] = grid[i].saturating_add(1);
        }
    }
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |b: u64| {
        h ^= b & 0xff;
        h = h.wrapping_mul(0x100000001b3);
    };
    for &v in &grid {
        mix(v as u64);
    }
    mix(sim.frame.r.round() as u64);
    mix(sim.frame.g.round() as u64);
    mix(sim.frame.b.round() as u64);
    mix((sim.frame.alpha * 255.0).round() as u64);
    mix(if sim.frame.hollow { 1 } else { 0 });
    format!("{:016x}", h)
}

// ---------------------------------------------------------------------------
// Braille raster — the terminal's renderer medium. Each cell is a 2×4 dot
// matrix (Unicode U+2800 + bits). Hollow frames stamp odd particles only:
// the body is still shown, visibly not asserted — the ASCII hollow.
pub const BRAILLE_BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Rasterize into a W×H grid of braille cells. Returns packed braille bits.
pub fn braille(sim: &PetSim, cells_w: usize, cells_h: usize, state: &PetState) -> Vec<u8> {
    let lay = layout(cells_w as f64 * 2.0, cells_h as f64 * 4.0, state);
    let mut grid = vec![0u8; cells_w * cells_h];
    for (i, q) in sim.p.iter().enumerate() {
        if sim.frame.hollow && i % 2 == 1 {
            continue;
        }
        let dx = lay.ox + q.x * lay.scale * lay.flip_x;
        let dy = lay.oy + q.y * lay.scale;
        let dx = dx.round() as i32;
        let dy = dy.round() as i32;
        if dx < 0 || dy < 0 {
            continue;
        }
        let (dcx, dcy) = (dx as usize / 2, dy as usize / 4);
        if dcx >= cells_w || dcy >= cells_h {
            continue;
        }
        grid[dcy * cells_w + dcx] |= BRAILLE_BITS[(dy as usize) % 4][(dx as usize) % 2];
    }
    grid
}

/// Text form of a braille grid — for snapshots and conformance output.
pub fn braille_text(grid: &[u8], cells_w: usize, cells_h: usize) -> String {
    let mut out = String::new();
    for y in 0..cells_h {
        for x in 0..cells_w {
            let b = grid[y * cells_w + x];
            out.push(if b == 0 {
                ' '
            } else {
                char::from_u32(0x2800 + b as u32).unwrap_or(' ')
            });
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[test]
fn constant_input_is_fully_still_in_every_channel() {
    for channel in CHANNELS {
        let mut sim = PetSim::whale();
        let state = PetState {
            channel: channel.key,
            activity: 0.8,
            coherence: 0.4,
            ..PetState::rest()
        };
        sim.step(1.0 / 30.0, &state, false, 1.0);
        let before = digest(&sim);
        for _ in 0..180 {
            sim.step(1.0 / 30.0, &state, false, 1.0);
        }
        assert_eq!(before, digest(&sim), "{}", channel.key);
    }
}
