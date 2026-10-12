import { PetWorld, type PetInteraction, type PetSegment } from './pet-world.js';
import { compilePetTelemetry, decodePetJSONL, PetLiveTape } from './pet-telemetry.js';
import { ARCH_OF, digest, layout, PetSim } from './pet-sim.js';
import { renderPetPCM, type PetVoice } from './pet-audio.js';
import { PetEngineTelemetry, type EngineOwnerProjection } from './pet-engine.js';
import { blendDotFields, dotFieldKey, projectDotField, textLanguage, type DotField, type DotOptions } from './dot-field.js';
import { DOT_LABELS, type GlyphLanguage } from './dot-glyphs.js';

/** Synchronous native boundary: JSON state and directly transferable PCM.
 * Native hosts share the actual world / score implementation, not a rewrite. */
export class PetNative {
  private world: PetWorld;
  private engine = new PetEngineTelemetry();
  private expressionActivity = this.engine.activity(0);
  private expressionTransition?: { at: number; from: DotField };
  private expressionOptions: Required<DotOptions> = { bodyStyle: 'smooth', textLanguage: 'en' };
  private engineTick = 0;
  private segment?: PetSegment;
  private liveTape = new PetLiveTape();
  private stillProjection?: { key: string; style: PetSim['frame'] };
  constructor(pointsJSON: string, tapeJSONL = '', interactionsJSON = '[]', live = false, expressionVersion: 1 | 2 = 2) {
    const points = JSON.parse(pointsJSON) as [number, number][];
    if (!Array.isArray(points) || points.length !== 980 || points.some(p => !Array.isArray(p) || p.length !== 2 || !p.every(n => Number.isFinite(n) && Math.abs(n) <= 1)))
      throw new Error('Invalid native whale body.');
    this.world = new PetWorld(points, live ? compilePetTelemetry([]) : decodePetJSONL(tapeJSONL), JSON.parse(interactionsJSON) as PetInteraction[], expressionVersion, true);
  }
  step(dt: number, motion: boolean): string { this.world.step(dt, { motion, sensitivity: 1 }); return this.snapshot(); }
  snapshot(): string { return JSON.stringify({ ...this.world.frame, voices: this.world.voices, digest: digest(this.world.sim) }); }
  private expression(): DotField {
    const { frame } = this.world;
    const target = projectDotField(this.expressionActivity, frame.timeMs, false, this.expressionTransition?.at ?? 0, this.expressionOptions);
    return this.expressionTransition
      ? blendDotFields(this.expressionTransition.from, target, (frame.timeMs - this.expressionTransition.at) / 560)
      : target;
  }
  private prepareExpression(activity: EngineOwnerProjection): void {
    if (dotFieldKey(activity) !== dotFieldKey(this.expressionActivity))
      this.expressionTransition = { at: this.world.frame.timeMs, from: this.expression() };
    this.expressionActivity = activity;
  }
  /** View-only projection. Display cadence and accessibility preferences never
   * advance the owner, consume randomness, or change its score/checkpoint. */
  presentation(options: DotOptions = {}, interfaceLanguage = 'en'): string {
    if (!['smooth', 'pixel'].includes(options.bodyStyle ?? 'smooth')
      || !['auto', 'en', 'zh-Hans', 'zh-Hant', 'ja'].includes(options.textLanguage ?? 'auto'))
      throw new Error('Invalid pet presentation preferences.');
    const next = { bodyStyle: options.bodyStyle ?? 'smooth', textLanguage: textLanguage(options.textLanguage, interfaceLanguage) };
    if (next.bodyStyle !== this.expressionOptions.bodyStyle || next.textLanguage !== this.expressionOptions.textLanguage) {
      const from = this.expression(); this.expressionOptions = next;
      this.expressionTransition = { at: this.world.frame.timeMs, from };
    }
    const { sim, frame } = this.world;
    const activity = this.engine.activity(frame.timeMs);
    const observed = activity.observed && activity.freshness === 'fresh';
    const state = { ...frame.state, observed: observed ? 1 : 0, roamX: 0, roamY: 0, flip: 1,
      lit: observed ? 1 : frame.behaviour === 'doze' ? .18 : 1 };
    const key = JSON.stringify([state, frame.pod]);
    if (this.stillProjection?.key !== key) {
      const still = new PetSim(sim.p.map(p => [p.hx, p.hy]), 0xC0FFEE, sim.expressionVersion);
      const peers = frame.pod.filter(p => p.present);
      still.step(1 / 30, state, { motion: false, sensitivity: 1,
        podSlots: peers.length >= 3 ? peers.map(p => [[0, 2, 4, 1, 3, 5][p.slot], p.phase] as const) : undefined });
      this.stillProjection = { key, style: still.frame };
    }
    const still = projectDotField(activity, frame.timeMs, true, 0, this.expressionOptions);
    const language = this.expressionOptions.textLanguage as GlyphLanguage;
    const label = activity.authoritativePresence === 'needs_you' ? 'waiting'
      : activity.authoritativePresence === 'done' ? 'done' : activity.activityKind;
    return JSON.stringify({ ...frame, digest: digest(sim), state, style: { ...sim.frame, hollow: !observed }, activity,
      ...this.expression(), displayLanguage: language,
      displayPhrase: observed && label ? DOT_LABELS[language][label] : '',
      still: { ...still, style: { ...this.stillProjection.style, hollow: !observed }, state } });
  }
  /** Losing a producer invalidates outstanding coverage, never the creature. */
  disconnectEngine(): void {
    this.engine = new PetEngineTelemetry(); this.world.voices = [];
    this.expressionActivity = this.engine.activity(this.world.frame.timeMs); this.expressionTransition = undefined;
  }
  interact(kind: PetInteraction['kind'], x: number, y: number): void { this.world.interact(kind, x, y); }
  interactions(): string { return JSON.stringify(this.world.interactions); }
  accept(packet: string): void { this.world.acceptTelemetry(JSON.parse(packet)); }
  acceptLiveTail(text: string): boolean {
    const packet = this.liveTape.readTail(text); if (!packet) return false;
    this.world.acceptTelemetry(packet); return true;
  }
  resetLiveInput(): number { this.liveTape.reset(); return this.resumeEngine(); }
  recording(withCheckpoint = false): string { return JSON.stringify(this.world.recording(withCheckpoint)); }
  needsSegment(): boolean { return this.world.needsSegment; }
  prepareSegment(): string { this.segment = this.world.prepareSegment(); return JSON.stringify(this.segment.recording); }
  commitSegment(): void { if (!this.segment) throw new Error('No pet segment was prepared.'); this.segment.commit(); this.segment = undefined; }
  checkpoint(): string { return JSON.stringify(this.world.checkpoint()); }
  recordingChunk(index: number, completed = false): string | null { return this.world.recordingChunk(index, completed); }
  restoreCheckpoint(text: string): void {
    if (text.length > 512 * 1024) throw new Error('Pet checkpoint exceeds its size limit.');
    this.restoreRecording(JSON.stringify({ ...this.world.recording(false), checkpoint: JSON.parse(text) }));
  }
  restoreRecording(text: string): void {
    if (text.length > 8 * 1024 * 1024) throw new Error('Native habitat exceeds 8 MiB.');
    const points = this.world.sim.p.map(p => [p.hx, p.hy] as [number, number]);
    this.world = PetWorld.fromRecording(points, JSON.parse(text));
    this.liveTape.reset();
    this.segment = undefined; this.engineTick = Math.round(this.world.frame.timeMs * 30 / 1000); this.engine = new PetEngineTelemetry();
    this.expressionActivity = this.engine.activity(this.world.frame.timeMs); this.expressionTransition = undefined;
  }
  /** Resume a live host at the first unrecorded bucket. Keep every accepted
   * interval, but never present its last observed frame as current evidence. */
  resumeEngine(): number {
    this.world.resumeObservation();
    this.engineTick = Math.round(this.world.frame.timeMs * 30 / 1000);
    this.engine = new PetEngineTelemetry();
    this.expressionActivity = this.engine.activity(this.world.frame.timeMs); this.expressionTransition = undefined;
    this.world.voices = [];
    return this.world.frame.timeMs;
  }
  /** A rejected observation leaves the reducer exactly as it was, like a batch. */
  observeEngine(metadataJSON: string, timeMs: number): void {
    const next = this.engine.clone(); next.observe(JSON.parse(metadataJSON), timeMs);
    this.prepareExpression(next.activity(this.world.frame.timeMs)); this.engine = next;
  }
  observeEngineBatch(metadataJSON: string, timeMs: number): void {
    const events: unknown = JSON.parse(metadataJSON);
    if (!Array.isArray(events) || events.length > 64) throw new Error('Invalid Engine batch.');
    const next = this.engine.clone();
    for (const event of events) next.observe(event, timeMs);
    this.prepareExpression(next.activity(this.world.frame.timeMs));
    this.engine = next;
  }
  advanceEngine(timeMs: number, motion: boolean, waiting: boolean): void {
    const target = Math.floor(timeMs * 30 / 1000 + 1e-8);
    if (!Number.isFinite(timeMs) || target < this.engineTick || target - this.engineTick > 300) throw new Error('Engine pet clock jump.');
    this.engine.confirmWaiting(timeMs, waiting);
    const voices: PetVoice[] = [];
    while (this.engineTick < target) {
      if ((this.engineTick + 1) % 12 === 0) this.world.acceptTelemetry(this.engine.bucket(Math.floor(this.engineTick / 12)));
      this.world.step(1 / 30, { motion, sensitivity: 1 });
      this.prepareExpression(this.engine.activity(this.world.frame.timeMs));
      voices.push(...this.world.voices);
      this.engineTick++;
    }
    this.world.voices = voices;
    this.prepareExpression(this.engine.activity(this.world.frame.timeMs));
  }
  terminal(width: number, height: number): string {
    if (![width, height].every(n => Number.isSafeInteger(n) && n >= 1 && n <= 512)) throw new Error('Invalid pet raster size.');
    const { sim, frame } = this.world, l = layout(width * 2, height * 4, frame.state);
    const cells = Array(width * height).fill(0), bits = [[1, 8], [2, 16], [4, 32], [64, 128]];
    sim.p.forEach((p, i) => {
      if (frame.state.observed < .92 && i % 2 === 1) return;
      const x = Math.round(l.ox + p.x * l.scale * l.flipX), y = Math.round(l.oy + p.y * l.scale);
      if (x >= 0 && x < width * 2 && y >= 0 && y < height * 4) cells[Math.floor(y / 4) * width + Math.floor(x / 2)] |= bits[y % 4][x % 2];
    });
    return JSON.stringify({ width, height, cells, timeMs: frame.timeMs, channel: frame.state.channel, arch: ARCH_OF[frame.state.channel],
      hollow: frame.state.observed < .92, dozing: frame.behaviour === 'doze', lit: frame.state.lit });
  }
  pcmChannels(voicesJSON: string, startSample: number, length: number, rate: number): [Float32Array, Float32Array] {
    const p = renderPetPCM(JSON.parse(voicesJSON) as PetVoice[], startSample, length, rate);
    return [p.left, p.right];
  }
  pcm(voicesJSON: string, startSample: number, length: number, rate: number): string {
    return JSON.stringify(this.pcmChannels(voicesJSON, startSample, length, rate).map(channel => Array.from(channel)));
  }
}
